//! `nudge`: local placement optimisation by real routing.
//!
//! A few movable parts get a discrete move space (dx/dy range on a grid, allowed rotations,
//! optional keep-in polygon; the side stays). Every candidate placement is checked with the
//! placement hard rules of [`crate::place`] (pad clearance from the DRU matrix incl. creepage on
//! the outer layers, hole keepouts, via-to-pad gap, courtyard overlap, board edge, footprint
//! keepouts, keep-in) and then routed for real: the unlocked copper of the affected nets inside
//! the region is ripped once, and the detailed router routes those nets in the region with the
//! moved parts. Score: (opens, length + via and move penalties); the best candidate wins.
//!
//! Speed: the board is cropped once to the region plus the router's job margin (all copper of
//! the routed nets is kept, so their connectivity is exact; far copper of other nets cannot
//! matter), each worker thread keeps one copy of the cropped board and only swaps the moved
//! footprints, so a candidate costs one small model build plus one windowed route.

use crate::board::*;
use crate::droute::{self, DParams, RNet};
use crate::geom::*;
use crate::model::*;
use crate::place;
use crate::rules::{wild, Rules};
use rayon::prelude::*;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::time::Instant;

/// one movable part of the move spec (JSON)
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct PartSpec {
    #[serde(rename = "ref")]
    pub refd: String,
    /// allowed x offset range (mm), default [-range, range]
    pub dx: Option<[f64; 2]>,
    pub dy: Option<[f64; 2]>,
    /// grid step of the offsets (mm)
    pub step: Option<f64>,
    /// allowed absolute rotations (degrees); overrides `drot`
    pub rot: Option<Vec<f64>>,
    /// allowed rotation deltas from the current rotation (degrees)
    pub drot: Option<Vec<f64>>,
    /// keep-in polygon: the courtyard (or the origin without one) must stay inside
    pub keepin: Option<Vec<[f64; 2]>>,
}

/// move spec (JSON file or built from the command line)
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Spec {
    pub parts: Vec<PartSpec>,
    /// nets to route (wildcards); default: the nets of the movable parts plus the nets open in
    /// the region
    pub nets: Option<Vec<String>>,
    /// existing copper that may be ripped: "region" (unlocked copper of the routed nets inside
    /// the region), "attached" (only the part of it connected to the movable parts), "none"
    pub rip: String,
    /// region (x0, y0, x1, y1); default: the move envelope of the parts plus `margin`
    pub region: Option<[f64; 4]>,
    pub margin: f64,
    /// defaults for parts without their own values
    pub range: f64,
    pub step: f64,
    pub drot: Vec<f64>,
    /// cost weights: mm of track per via, per mm moved, per rotated part
    pub via_w: f64,
    pub move_w: f64,
    pub rot_w: f64,
    /// listed parts may move although they are locked
    pub move_locked: bool,
}

impl Default for Spec {
    fn default() -> Self {
        Spec {
            parts: Vec::new(),
            nets: None,
            rip: "region".into(),
            region: None,
            margin: 2.0,
            range: 1.0,
            step: 0.05,
            drot: vec![0.0],
            via_w: 2.0,
            move_w: 2.0,
            rot_w: 0.5,
            move_locked: false,
        }
    }
}

/// search options
#[derive(Clone)]
pub struct Opts {
    pub params: DParams,
    /// wall-clock budget of the search (s)
    pub time_limit: f64,
    /// stop after this many routed candidates
    pub max_cands: usize,
    /// sample points per axis of the coarse scan
    pub coarse: usize,
    /// route every candidate with the exact cleanup (slower; default: only the winner)
    pub clean_search: bool,
}

impl Default for Opts {
    fn default() -> Self {
        Opts {
            params: DParams::default(),
            time_limit: 60.0,
            max_cands: 20000,
            coarse: 9,
            clean_search: false,
        }
    }
}

/// a movable part with its discrete move space
struct Part {
    refd: String,
    /// index into the cropped board's footprints
    fi: usize,
    at0: P,
    rot0: f64,
    step: f64,
    ix: (i32, i32),
    iy: (i32, i32),
    rots: Vec<f64>,
    keepin: Option<Vec<P>>,
    /// rule areas owned by the footprint (indices into the cropped board's zones) and their
    /// outlines at the start pose
    zones: Vec<(usize, Vec<P>)>,
}

/// a candidate: per part (x steps, y steps, rotation index)
type Cand = Vec<(i32, i32, usize)>;

/// result of routing one candidate
#[derive(Clone)]
pub struct Eval {
    pub opens: usize,
    pub length: f64,
    pub vias: usize,
    pub moved: f64,
    pub rotated: usize,
    pub cost: f64,
    pub routed: HashMap<usize, RNet>,
    pub failed: Vec<(usize, usize)>,
}

impl Eval {
    fn better(&self, o: &Eval) -> bool {
        (self.opens, self.cost) < (o.opens, o.cost)
    }
}

/// the outcome of a nudge search
pub struct Outcome {
    pub region: BBox,
    pub nets: Vec<usize>,
    pub ripped: Vec<String>,
    pub opens_before: usize,
    /// per net open connections in the region before
    pub open_before: HashMap<usize, usize>,
    /// best candidate (None: no feasible candidate routed)
    pub best: Option<Eval>,
    /// new pose per part: (ref, old at, old rot, new at, new rot)
    pub moves: Vec<(String, P, f64, P, f64)>,
    pub evaluated: usize,
    pub infeasible: usize,
    pub search_s: f64,
    pub route_ms: f64,
    pub stopped: Option<&'static str>,
    pub log: Vec<Value>,
}

impl Outcome {
    /// the best candidate is an improvement over the board as it is
    pub fn improves(&self) -> bool {
        self.best
            .as_ref()
            .is_some_and(|b| b.opens < self.opens_before)
    }
}

fn offsets(r: [f64; 2], step: f64) -> (i32, i32) {
    let lo = (r[0] / step - 1e-9).ceil() as i32;
    let hi = (r[1] / step + 1e-9).floor() as i32;
    (lo.min(0), hi.max(0))
}

/// `n` evenly spread integers of [lo, hi] (always including 0)
fn spread(lo: i32, hi: i32, n: usize) -> Vec<i32> {
    let mut v: Vec<i32> = (0..n.max(2))
        .map(|k| lo + ((hi - lo) as f64 * k as f64 / (n.max(2) - 1) as f64).round() as i32)
        .collect();
    v.push(0);
    v.sort_unstable();
    v.dedup();
    v
}

/// the board reduced to what routing inside `win` can see: every footprint, track, via and
/// zone of the routed nets (exact connectivity), other items only near the window; ripped
/// items are left out
fn crop(b: &Board, win: &BBox, nets: &HashSet<usize>, ripped: &HashSet<String>) -> Board {
    let near = |bb: &BBox| bb.overlaps(win);
    let mut c = Board {
        text: String::new(),
        nets: b.nets.clone(),
        net_id: b.net_id.clone(),
        edge: b.edge.clone(),
        edge_segs: b.edge_segs.clone(),
        bbox: b.bbox,
        cu_mask: b.cu_mask,
        mask_margin: b.mask_margin,
        ..Default::default()
    };
    let mut kept_fp: HashSet<String> = HashSet::new();
    for f in &b.fps {
        let mut bb = BBox::empty();
        for q in f.crtyd_world() {
            bb.add(q);
        }
        for pd in &f.pads {
            for s in f.pad_shapes(pd) {
                let sb = s.bbox();
                bb.add(p(sb.x0, sb.y0));
                bb.add(p(sb.x1, sb.y1));
            }
        }
        if near(&bb) || f.pads.iter().any(|pd| nets.contains(&pd.net)) {
            kept_fp.insert(f.refd.clone());
            c.fps.push(f.clone());
        }
    }
    for t in &b.tracks {
        if ripped.contains(&t.uuid) && !t.uuid.is_empty() {
            continue;
        }
        let s = Shape::Capsule {
            a: t.a,
            b: t.b,
            r: t.w / 2.0,
        };
        if nets.contains(&t.net) || near(&s.bbox()) {
            c.tracks.push(t.clone());
        }
    }
    for v in &b.vias {
        if ripped.contains(&v.uuid) && !v.uuid.is_empty() {
            continue;
        }
        if nets.contains(&v.net) || near(&BBox::empty_at(v.at).inflate(v.d)) {
            c.vias.push(v.clone());
        }
    }
    for z in &b.zones {
        let mut bb = BBox::empty();
        for q in &z.outline {
            bb.add(*q);
        }
        let own = z.fp.as_ref().is_some_and(|r| kept_fp.contains(r));
        if !z.keepout && z.net != 0 && nets.contains(&z.net) {
            c.zones.push(z.clone());
        } else if near(&bb) || own {
            let mut z = z.clone();
            if !z.keepout && !nets.contains(&z.net) {
                // fills of other nets only matter for their own connectivity
                z.fills.clear();
            }
            c.zones.push(z);
        }
    }
    c.graphics = b
        .graphics
        .iter()
        .filter(|(s, _)| near(&s.bbox()))
        .cloned()
        .collect();
    c.mask = b
        .mask
        .iter()
        .filter(|(s, _)| near(&s.bbox()))
        .cloned()
        .collect();
    c
}

/// unlocked tracks and vias of `nets` inside `region` (uuids); with `attached` only those
/// connected (inside the region) to the pads of the footprints `fps`
fn rip_set(
    b: &Board,
    region: &BBox,
    nets: &HashSet<usize>,
    attached: Option<&[usize]>,
) -> Vec<String> {
    let inside = |q: P| region.contains(q);
    // items whose every piece lies inside (arcs are two tracks with one uuid)
    let mut out_uuid: HashSet<&str> = HashSet::new();
    for t in &b.tracks {
        if !(inside(t.a) && inside(t.b)) {
            out_uuid.insert(&t.uuid);
        }
    }
    let cand_t: Vec<usize> = (0..b.tracks.len())
        .filter(|&i| {
            let t = &b.tracks[i];
            !t.locked
                && !t.uuid.is_empty()
                && nets.contains(&t.net)
                && !out_uuid.contains(t.uuid.as_str())
        })
        .collect();
    let cand_v: Vec<usize> = (0..b.vias.len())
        .filter(|&i| {
            let v = &b.vias[i];
            !v.locked && !v.uuid.is_empty() && nets.contains(&v.net) && inside(v.at)
        })
        .collect();
    let Some(fps) = attached else {
        let mut u: Vec<String> = cand_t
            .iter()
            .map(|&i| b.tracks[i].uuid.clone())
            .chain(cand_v.iter().map(|&i| b.vias[i].uuid.clone()))
            .collect();
        u.sort();
        u.dedup();
        return u;
    };
    // flood fill from the movable parts' pads over touching candidate copper of the same net
    let mut items: Vec<(Shape, u8, usize, String)> = Vec::new();
    for &i in &cand_t {
        let t = &b.tracks[i];
        items.push((
            Shape::Capsule {
                a: t.a,
                b: t.b,
                r: t.w / 2.0,
            },
            1 << t.layer,
            t.net,
            t.uuid.clone(),
        ));
    }
    for &i in &cand_v {
        let v = &b.vias[i];
        items.push((Shape::circle(v.at, v.d / 2.0), 0x3f, v.net, v.uuid.clone()));
    }
    let mut front: Vec<(Shape, u8, usize)> = Vec::new();
    for &fi in fps {
        let f = &b.fps[fi];
        for pd in &f.pads {
            for s in f.pad_shapes(pd) {
                front.push((s, pd.layers, pd.net));
            }
        }
    }
    let mut taken = vec![false; items.len()];
    while let Some((s, l, n)) = front.pop() {
        let sb = s.bbox().inflate(1e-3);
        for (k, it) in items.iter().enumerate() {
            if taken[k] || it.2 != n || it.1 & l == 0 || !it.0.bbox().overlaps(&sb) {
                continue;
            }
            if shape_dist(&it.0, &s) < 1e-3 {
                taken[k] = true;
                front.push((it.0.clone(), it.1, it.2));
            }
        }
    }
    let mut u: Vec<String> = (0..items.len())
        .filter(|&k| taken[k])
        .map(|k| items[k].3.clone())
        .collect();
    u.sort();
    u.dedup();
    u
}

/// open connections (components - 1) per net among the pins inside `region`
fn opens_in(m: &Model, region: &BBox, nets: &HashSet<usize>) -> HashMap<usize, usize> {
    let (pins, _) = pins_and_copper_of(m, &|n| nets.contains(&n));
    let mut out = HashMap::new();
    for (n, ps) in pins {
        let mut c: Vec<usize> = ps
            .iter()
            .filter(|p| region.contains(p.at))
            .map(|p| p.comp)
            .collect();
        c.sort_unstable();
        c.dedup();
        if c.len() > 1 {
            out.insert(n, c.len() - 1);
        }
    }
    out
}

/// footprint `f0` (at its start pose) moved to (at, rot), and its rule areas with it
fn place_into(w: &mut Board, part: &Part, f0: &Footprint, at: P, rot: f64) {
    w.fps[part.fi] = f0.moved(at, rot, f0.back);
    for (zi, pts0) in &part.zones {
        w.zones[*zi].outline = pts0
            .iter()
            .map(|q| at + (*q - part.at0).rot(-part.rot0).rot(rot))
            .collect();
    }
}

/// Searches the move space of the parts in `spec` on board `b` and returns the best placement
/// with its routed copper. Nothing is written.
pub fn run(b: &Board, r: &Rules, cfg: &BoardCfg, spec: &Spec, o: &Opts) -> Result<Outcome, String> {
    let t0 = Instant::now();
    let deadline = t0 + std::time::Duration::from_secs_f64(o.time_limit.max(0.1));
    if spec.parts.is_empty() {
        return Err("no movable parts given (--parts or a spec file with \"parts\")".into());
    }
    // ---- parts and their move space (indices into the full board for now)
    let mut fis = Vec::new();
    let mut seen = HashSet::new();
    for ps in &spec.parts {
        let fi = b
            .fp_idx(&ps.refd)
            .ok_or_else(|| format!("footprint {} not found", ps.refd))?;
        if !seen.insert(fi) {
            return Err(format!("footprint {} listed twice", ps.refd));
        }
        if b.fps[fi].locked && !spec.move_locked {
            return Err(format!(
                "footprint {} is locked (--move-locked moves it anyway)",
                ps.refd
            ));
        }
        fis.push(fi);
    }
    // ---- region: the move envelope of every part plus the margin
    let region = match spec.region {
        Some(rg) => BBox {
            x0: rg[0],
            y0: rg[1],
            x1: rg[2],
            y1: rg[3],
        },
        None => {
            let mut bb = BBox::empty();
            for (ps, &fi) in spec.parts.iter().zip(&fis) {
                let f = &b.fps[fi];
                let rg = spec.range;
                let (dx, dy) = (ps.dx.unwrap_or([-rg, rg]), ps.dy.unwrap_or([-rg, rg]));
                // radius of the footprint about its origin (any rotation)
                let mut rad: f64 = 0.0;
                for q in &f.crtyd {
                    rad = rad.max(q.len());
                }
                for pd in &f.pads {
                    rad = rad.max(pd.pos.len() + 0.5 * pd.size.0.hypot(pd.size.1));
                }
                bb.add(p(f.at.x + dx[0] - rad, f.at.y + dy[0] - rad));
                bb.add(p(f.at.x + dx[1] + rad, f.at.y + dy[1] + rad));
            }
            bb.inflate(spec.margin)
        }
    };
    // ---- nets: given, or the parts' nets plus the nets open in the region
    let m0 = Model::new(b, r, cfg.clone(), CopperMode::All);
    let routable: HashSet<usize> = (0..b.nets.len()).filter(|&n| m0.routable[n]).collect();
    let open0 = opens_in(&m0, &region, &routable);
    let mut nets: HashSet<usize> = match &spec.nets {
        Some(pats) => (0..b.nets.len())
            .filter(|&n| routable.contains(&n) && pats.iter().any(|pt| wild(&b.nets[n], pt)))
            .collect(),
        None => {
            let mut s: HashSet<usize> = open0.keys().copied().collect();
            for &fi in &fis {
                for pd in &b.fps[fi].pads {
                    if routable.contains(&pd.net) {
                        s.insert(pd.net);
                    }
                }
            }
            s
        }
    };
    nets.retain(|n| routable.contains(n));
    let mut net_list: Vec<usize> = nets.iter().copied().collect();
    net_list.sort_unstable();
    let open_before: HashMap<usize, usize> = open0
        .into_iter()
        .filter(|(n, _)| nets.contains(n))
        .collect();
    let opens_before: usize = open_before.values().sum();
    if opens_before == 0 {
        // nothing to gain: no candidate can have fewer opens
        return Ok(Outcome {
            region,
            nets: net_list,
            ripped: Vec::new(),
            opens_before,
            open_before,
            best: None,
            moves: fis
                .iter()
                .map(|&fi| {
                    let f = &b.fps[fi];
                    (f.refd.clone(), f.at, f.rot, f.at, f.rot)
                })
                .collect(),
            evaluated: 0,
            infeasible: 0,
            search_s: t0.elapsed().as_secs_f64(),
            route_ms: 0.0,
            stopped: None,
            log: vec![
                json!({"phase": "start", "note": "no open connection of the routed nets in the region"}),
            ],
        });
    }
    // ---- copper that may be ripped
    let ripped = match spec.rip.as_str() {
        "none" => Vec::new(),
        "region" => rip_set(b, &region, &nets, None),
        "attached" => rip_set(b, &region, &nets, Some(&fis)),
        x => return Err(format!("rip: unknown mode '{x}' (region, attached, none)")),
    };
    let ripped_set: HashSet<String> = ripped.iter().cloned().collect();
    // ---- cropped board: region + router job margin + the largest clearance
    let mut maxd: f64 = 0.0;
    for v in &r.mat[0] {
        maxd = maxd.max(*v as f64);
    }
    let win = region.inflate(o.params.job_margin + maxd + 3.0);
    let mut cb = crop(b, &win, &nets, &ripped_set);
    for f in cb.fps.iter_mut() {
        if spec.parts.iter().any(|ps| ps.refd == f.refd) {
            f.locked = false; // --move-locked (the placement checker skips locked parts)
        }
    }
    let cb = cb;
    let mut parts = Vec::new();
    for (ps, &fi) in spec.parts.iter().zip(&fis) {
        let f = &b.fps[fi];
        let step = ps.step.unwrap_or(spec.step).max(1e-3);
        let rg = spec.range;
        let mut rots: Vec<f64> = match (&ps.rot, &ps.drot) {
            (Some(a), _) => a.clone(),
            (None, Some(d)) => d.iter().map(|d| f.rot + d).collect(),
            (None, None) => spec.drot.iter().map(|d| f.rot + d).collect(),
        };
        let norm = |a: f64| crate::movefp::norm_angle(a);
        rots = rots.into_iter().map(norm).collect();
        let mut rr: Vec<f64> = Vec::new();
        for a in rots {
            if !rr.iter().any(|x| (norm(x - a)).abs() < 1e-6) {
                rr.push(a);
            }
        }
        // the current rotation first: the start candidate
        if let Some(k) = rr.iter().position(|x| norm(x - f.rot).abs() < 1e-6) {
            rr.swap(0, k);
        }
        parts.push(Part {
            refd: f.refd.clone(),
            fi: cb
                .fp_idx(&f.refd)
                .expect("cropped board keeps the movable parts"),
            at0: f.at,
            rot0: f.rot,
            step,
            ix: offsets(ps.dx.unwrap_or([-rg, rg]), step),
            iy: offsets(ps.dy.unwrap_or([-rg, rg]), step),
            rots: rr,
            keepin: ps
                .keepin
                .as_ref()
                .map(|v| v.iter().map(|q| p(q[0], q[1])).collect()),
            zones: cb
                .zones
                .iter()
                .enumerate()
                .filter(|(_, z)| z.fp.as_deref() == Some(f.refd.as_str()))
                .map(|(i, z)| (i, z.outline.clone()))
                .collect(),
        });
    }
    // ---- hard placement rules (placement checker of `place`) on the ripped, cropped board
    let mc = Model::new(&cb, r, cfg.clone(), CopperMode::All);
    let zf = place::ZonesFile {
        movable: parts.iter().map(|pt| pt.refd.clone()).collect(),
        zones: parts
            .iter()
            .filter_map(|pt| {
                pt.keepin.as_ref().map(|k| place::ZoneSpec {
                    refs: vec![pt.refd.clone()],
                    poly: Some(k.iter().map(|q| [q.x, q.y]).collect()),
                    ..Default::default()
                })
            })
            .collect(),
        free_locked_copper: true,
        ..Default::default()
    };
    let pb = place::Problem::new(&mc, &zf)?;
    let pidx: Vec<usize> = parts.iter().map(|pt| pb.idx_of_fp[&pt.fi]).collect();
    let pose_of = |pt: &Part, c: (i32, i32, usize)| -> (P, f64) {
        (
            pt.at0 + p(c.0 as f64 * pt.step, c.1 as f64 * pt.step),
            pt.rots[c.2],
        )
    };
    let placed_all = |c: &Cand| -> (Vec<place::Placed>, Vec<place::Pose>) {
        let mut poses: Vec<place::Pose> = pb
            .parts
            .iter()
            .map(|s| {
                let f = &cb.fps[s.fi];
                place::Pose {
                    at: f.at,
                    rot: f.rot,
                    back: f.back,
                }
            })
            .collect();
        for (k, pt) in parts.iter().enumerate() {
            let (at, rot) = pose_of(pt, c[k]);
            poses[pidx[k]].at = at;
            poses[pidx[k]].rot = rot;
        }
        let pl = pb
            .parts
            .iter()
            .zip(&poses)
            .map(|(s, ps)| place::place_part(&cb.fps[s.fi], ps))
            .collect();
        (pl, poses)
    };
    let viol = |c: &Cand| -> Vec<usize> {
        let (pl, poses) = placed_all(c);
        let mut q = Vec::new();
        (0..pb.parts.len())
            .map(|k| pb.violations(k, &pl[k], &pl, &poses, &mut q))
            .collect()
    };
    let start: Cand = parts.iter().map(|_| (0, 0, 0)).collect();
    let v0 = viol(&start);
    let feasible = |c: &Cand| viol(c).iter().zip(&v0).all(|(a, b)| a <= b);
    // ---- maps of everything that stays, shared by all candidates
    let tf = Instant::now();
    let was_quiet = droute::QUIET.swap(true, std::sync::atomic::Ordering::Relaxed);
    let fixed = droute::Router::fixed_maps(
        &mc,
        region,
        o.params.clone(),
        Some(&nets),
        parts.iter().map(|pt| pt.fi).collect(),
    );
    let fixed_s = tf.elapsed().as_secs_f64();
    // ---- routing of one candidate on a worker's copy of the cropped board
    // thread time (us) of model builds, router setups (maps) and searches
    let prof: [std::sync::atomic::AtomicU64; 3] = Default::default();
    let mut prm = o.params.clone();
    prm.deadline = Some(deadline);
    // the search routes without the exact cleanup (much faster); the winner is routed again with it
    let mut fast = prm.clone();
    fast.clean = fast.clean && o.clean_search;
    let eval = |w: &mut Board, c: &Cand, prm: &DParams| -> Eval {
        let mut moved = 0.0;
        let mut rotated = 0;
        for (k, pt) in parts.iter().enumerate() {
            let (at, rot) = pose_of(pt, c[k]);
            place_into(w, pt, &cb.fps[pt.fi], at, rot);
            moved += (at - pt.at0).len();
            if crate::movefp::norm_angle(rot - pt.rot0).abs() > 1e-6 {
                rotated += 1;
            }
        }
        let t = Instant::now();
        let m = Model::new(w, r, cfg.clone(), CopperMode::All);
        let t1 = Instant::now();
        let rt = droute::Router::new_with(&m, region, prm.clone(), Some(&nets), &fixed);
        let t2 = Instant::now();
        let res = rt.run();
        let us = |d: std::time::Duration| d.as_micros() as u64;
        use std::sync::atomic::Ordering::Relaxed;
        prof[0].fetch_add(us(t1 - t), Relaxed);
        prof[1].fetch_add(us(t2 - t1), Relaxed);
        prof[2].fetch_add(us(t2.elapsed()), Relaxed);
        let opens: usize = res.failed.iter().map(|f| f.1).sum();
        let length: f64 = res
            .routed
            .values()
            .flat_map(|x| x.segs.iter())
            .map(|s| (s.b - s.a).len())
            .sum();
        let vias: usize = res.routed.values().map(|x| x.vias.len()).sum();
        Eval {
            opens,
            length,
            vias,
            moved,
            rotated,
            cost: length
                + spec.via_w * vias as f64
                + spec.move_w * moved
                + spec.rot_w * rotated as f64,
            routed: res.routed,
            failed: res.failed,
        }
    };
    // ---- search: coarse scan per part, then pattern refinement, rounds over all parts
    let stop =
        || droute::STOP.load(std::sync::atomic::Ordering::Relaxed) || Instant::now() > deadline;
    let mut cache: HashMap<Cand, Option<Eval>> = HashMap::new();
    let mut evaluated = 0usize;
    let mut infeasible = 0usize;
    let mut route_s = 0.0f64;
    let mut log: Vec<Value> = Vec::new();
    // evaluates the new candidates of `cs` (parallel), returns the best of all of `cs`
    let mut batch = |cs: Vec<Cand>,
                     cache: &mut HashMap<Cand, Option<Eval>>,
                     evaluated: &mut usize,
                     infeasible: &mut usize|
     -> Option<(Cand, Eval)> {
        let mut new: Vec<Cand> = cs
            .iter()
            .filter(|c| !cache.contains_key(*c))
            .cloned()
            .collect();
        new.sort();
        new.dedup();
        let ok: Vec<bool> = new.par_iter().map(&feasible).collect();
        let mut todo = Vec::new();
        for (c, f) in new.into_iter().zip(ok) {
            if f {
                todo.push(c);
            } else {
                *infeasible += 1;
                cache.insert(c, None);
            }
        }
        let room = o.max_cands.saturating_sub(*evaluated);
        todo.truncate(room);
        if !todo.is_empty() && !stop() {
            let t = Instant::now();
            let res: Vec<Option<Eval>> = todo
                .par_iter()
                .map_init(
                    || cb.clone(),
                    |w, c| {
                        if stop() {
                            None
                        } else {
                            Some(eval(w, c, &fast))
                        }
                    },
                )
                .collect();
            route_s += t.elapsed().as_secs_f64();
            for (c, e) in todo.into_iter().zip(res) {
                if e.is_some() {
                    *evaluated += 1;
                    cache.insert(c, e);
                }
            }
        }
        let mut best: Option<(Cand, Eval)> = None;
        for c in &cs {
            if let Some(Some(e)) = cache.get(c) {
                if best
                    .as_ref()
                    .is_none_or(|(bc, be)| e.better(be) || (!be.better(e) && c < bc))
                {
                    best = Some((c.clone(), e.clone()));
                }
            }
        }
        best
    };
    let mut cur = start.clone();
    let mut best: Option<Eval> = batch(
        vec![start.clone()],
        &mut cache,
        &mut evaluated,
        &mut infeasible,
    )
    .map(|x| x.1);
    if let Some(e) = &best {
        log.push(json!({"phase": "start", "opens": e.opens, "cost": round3(e.cost)}));
    }
    let take = |cand: Option<(Cand, Eval)>, cur: &mut Cand, best: &mut Option<Eval>| -> bool {
        if let Some((c, e)) = cand {
            if best.as_ref().is_none_or(|b| e.better(b)) {
                *cur = c;
                *best = Some(e);
                return true;
            }
        }
        false
    };
    for round in 0..8 {
        let mut improved = false;
        for k in 0..parts.len() {
            if stop() || evaluated >= o.max_cands {
                break;
            }
            let pt = &parts[k];
            let span = (pt.ix.1 - pt.ix.0).max(pt.iy.1 - pt.iy.0);
            let mut s = if round == 0 {
                // coarse scan of the whole move space of this part
                let mut cs = Vec::new();
                for &x in &spread(pt.ix.0, pt.ix.1, o.coarse) {
                    for &y in &spread(pt.iy.0, pt.iy.1, o.coarse) {
                        for ri in 0..pt.rots.len() {
                            let mut c = cur.clone();
                            c[k] = (x, y, ri);
                            cs.push(c);
                        }
                    }
                }
                let b = batch(cs, &mut cache, &mut evaluated, &mut infeasible);
                if take(b, &mut cur, &mut best) {
                    improved = true;
                }
                (span / (o.coarse.max(2) as i32 - 1)).max(1)
            } else {
                2
            };
            // pattern search around the current best, step halving down to one grid step
            while s >= 1 && !stop() && evaluated < o.max_cands {
                let (x, y, ri) = cur[k];
                let mut cs = Vec::new();
                for (dx, dy) in [
                    (1, 0),
                    (-1, 0),
                    (0, 1),
                    (0, -1),
                    (1, 1),
                    (1, -1),
                    (-1, 1),
                    (-1, -1),
                ] {
                    let (nx, ny) = (x + dx * s, y + dy * s);
                    if nx < pt.ix.0 || nx > pt.ix.1 || ny < pt.iy.0 || ny > pt.iy.1 {
                        continue;
                    }
                    let mut c = cur.clone();
                    c[k] = (nx, ny, ri);
                    cs.push(c);
                }
                for r2 in 0..pt.rots.len() {
                    let mut c = cur.clone();
                    c[k] = (x, y, r2);
                    cs.push(c);
                }
                let b = batch(cs, &mut cache, &mut evaluated, &mut infeasible);
                if take(b, &mut cur, &mut best) {
                    improved = true;
                } else {
                    s /= 2;
                }
            }
        }
        if let Some(e) = &best {
            log.push(json!({"phase": format!("round {round}"), "opens": e.opens, "cost": round3(e.cost), "evaluated": evaluated}));
        }
        if !improved || stop() || evaluated >= o.max_cands {
            break;
        }
    }
    // the winner with the exact cleanup (kept unless it loses connections)
    if let Some(b) = &best {
        if prm.clean && !fast.clean && !droute::STOP.load(std::sync::atomic::Ordering::Relaxed) {
            let mut full = prm.clone();
            full.deadline = None;
            let fin = eval(&mut cb.clone(), &cur, &full);
            log.push(json!({"phase": "final cleanup", "opens": fin.opens, "cost": round3(fin.cost), "kept": fin.opens <= b.opens}));
            if fin.opens <= b.opens {
                best = Some(fin);
            }
        }
    }
    droute::QUIET.store(was_quiet, std::sync::atomic::Ordering::Relaxed);
    let pf: Vec<f64> = prof
        .iter()
        .map(|x| {
            x.load(std::sync::atomic::Ordering::Relaxed) as f64 * 1e-3 / evaluated.max(1) as f64
        })
        .collect();
    log.push(json!({"phase": "profile", "fixed_maps_s": round3(fixed_s), "thread_ms_per_candidate": {"model": round3(pf[0]), "router_setup": round3(pf[1]), "route": round3(pf[2])}}));
    let stopped = if droute::STOP.load(std::sync::atomic::Ordering::Relaxed) {
        Some("signal")
    } else if Instant::now() > deadline {
        Some("time-limit")
    } else if evaluated >= o.max_cands {
        Some("max-candidates")
    } else {
        None
    };
    let moves = parts
        .iter()
        .zip(&cur)
        .map(|(pt, c)| {
            let (at, rot) = pose_of(pt, *c);
            (pt.refd.clone(), pt.at0, pt.rot0, at, rot)
        })
        .collect();
    Ok(Outcome {
        region,
        nets: net_list,
        ripped,
        opens_before,
        open_before,
        best,
        moves,
        evaluated,
        infeasible,
        search_s: t0.elapsed().as_secs_f64(),
        route_ms: if evaluated > 0 {
            1e3 * route_s / evaluated as f64
        } else {
            0.0
        },
        stopped,
        log,
    })
}

fn round3(v: f64) -> f64 {
    (v * 1000.0).round() / 1000.0
}

/// `--auto`: small 2-pad SMD parts (not locked) whose pads lie within `dist` mm of a pin of an
/// open connection, nearest first, at most `max` parts
pub fn auto_parts(
    b: &Board,
    r: &Rules,
    cfg: &BoardCfg,
    region: Option<BBox>,
    dist: f64,
    max: usize,
) -> Vec<String> {
    let m = Model::new(b, r, cfg.clone(), CopperMode::All);
    let routable: HashSet<usize> = (0..b.nets.len()).filter(|&n| m.routable[n]).collect();
    let (pins, _) = pins_and_copper_of(&m, &|n| routable.contains(&n));
    // pins of nets with more than one component (in the region)
    let mut open_pins: Vec<P> = Vec::new();
    for ps in pins.values() {
        let ps: Vec<&Pin> = ps
            .iter()
            .filter(|p| region.is_none_or(|rg| rg.contains(p.at)))
            .collect();
        let mut c: Vec<usize> = ps.iter().map(|p| p.comp).collect();
        c.sort_unstable();
        c.dedup();
        if c.len() > 1 {
            open_pins.extend(ps.iter().map(|p| p.at));
        }
    }
    let mut cand: Vec<(f64, String)> = Vec::new();
    for f in &b.fps {
        let cu: Vec<&Pad> = f
            .pads
            .iter()
            .filter(|pd| pd.kind != "np_thru_hole" && pd.layers != 0)
            .collect();
        if f.locked || cu.len() != 2 || !cu.iter().all(|pd| pd.kind == "smd") {
            continue;
        }
        let d = cu
            .iter()
            .map(|pd| f.pad_world(pd).0)
            .flat_map(|c| open_pins.iter().map(move |q| (*q - c).len()))
            .fold(f64::INFINITY, f64::min);
        if d <= dist {
            cand.push((d, f.refd.clone()));
        }
    }
    cand.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
    cand.into_iter().take(max).map(|x| x.1).collect()
}

/// the board text with the moved footprints, without the ripped items and with the new copper
pub fn apply(b: &Board, out: &Outcome) -> Result<String, String> {
    let mv: Vec<crate::movefp::FpMove> = out
        .moves
        .iter()
        .filter(|m| (m.3 - m.1).len() > 1e-9 || crate::movefp::norm_angle(m.4 - m.2).abs() > 1e-9)
        .map(|m| crate::movefp::FpMove {
            refd: m.0.clone(),
            at: m.3,
            rot: m.4,
        })
        .collect();
    let text = crate::movefp::move_footprints(&b.text, &mv)?;
    let (text, _) = crate::reroute::remove_items(&text, &out.ripped);
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn example_spec_parses() {
        let t = include_str!("../examples/nudge.json");
        let s: Spec = serde_json::from_str(t).unwrap();
        assert_eq!(s.parts.len(), 2);
        assert_eq!(
            s.parts[0].drot.as_deref(),
            Some(&[0.0, 90.0, 180.0, 270.0][..])
        );
        assert_eq!(s.parts[1].keepin.as_ref().map(|k| k.len()), Some(4));
        assert_eq!(s.rip, "region");
    }

    #[test]
    fn grid_offsets_and_spread() {
        assert_eq!(offsets([-1.0, 1.0], 0.05), (-20, 20));
        assert_eq!(offsets([0.3, 1.0], 0.1), (0, 10)); // the start pose is always included
        assert_eq!(spread(-20, 20, 5), vec![-20, -10, 0, 10, 20]);
        assert_eq!(spread(-1, 1, 9), vec![-1, 0, 1]);
    }
}
