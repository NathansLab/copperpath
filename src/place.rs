//! Placement: zones file (written by the AI coordinator), schematic-grouped initial placement and a
//! parallel simulated-annealing optimizer with hard rules and a weighted cost.
//!
//! Hard rules (a move may never increase a part's violation count): pad-to-copper clearance from the
//! DRU net-pair matrix (outer layers incl. creepage), mounting-hole keepouts, courtyard overlaps on
//! the same side, pad edge clearance, courtyard inside the board and inside its zone, allowed sides
//! and rotations, `near` max distances, locked parts and parts with locked copper never move.
//! Cost: HPWL, ratsnest crossings, free via sites of the target class in the evaluation region
//! (incremental count map), loop-net HPWL, max-length nets (DRU length rule), straight pad exits,
//! preferred rotation, near distance.

use crate::board::*;
use crate::geom::*;
use crate::model::*;
use crate::rules::wild;
use crate::sch::SchPos;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

// ------------------------------------------------------------------------------------------------
// zones file

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct Near {
    pub r#ref: String,
    #[serde(default)]
    pub pin: Option<String>,
    pub max: f64,
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct ZoneSpec {
    pub refs: Vec<String>,
    #[serde(default)]
    pub rect: Option<[f64; 4]>,
    #[serde(default)]
    pub poly: Option<Vec<[f64; 2]>>,
    /// "F", "B" or "FB" (default: the part's current side)
    #[serde(default)]
    pub side: Option<String>,
    /// rotation step in degrees (45 default, 0 = keep rotation)
    #[serde(default)]
    pub rot_step: Option<f64>,
    #[serde(default)]
    pub rot_pref: Vec<f64>,
    #[serde(default)]
    pub near: Option<Near>,
    #[serde(default)]
    pub why: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct Weights {
    pub hpwl: f64,
    pub cross: f64,
    pub via: f64,
    pub cong: f64,
    pub loop_: f64,
    pub len: f64,
    /// pad exit angle: ratsnest lines that leave a pad far off its axes
    pub exit: f64,
    pub pref: f64,
    pub near: f64,
    /// penalty for rotations that are not a multiple of 90 deg (aesthetics: flush, aligned groups)
    pub diag: f64,
}

impl Default for Weights {
    fn default() -> Self {
        Weights {
            hpwl: 1.0,
            cross: 3.0,
            via: 0.02,
            cong: 0.0,
            loop_: 2.0,
            len: 20.0,
            exit: 0.5,
            pref: 2.0,
            near: 0.5,
            diag: 5.0,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct ZonesFile {
    #[serde(default)]
    pub movable: Vec<String>,
    #[serde(default)]
    pub fixed: Vec<String>,
    #[serde(default)]
    pub zones: Vec<ZoneSpec>,
    /// region where free via sites of `via_class` are counted (x0, y0, x1, y1)
    #[serde(default)]
    pub eval_region: Option<[f64; 4]>,
    #[serde(default)]
    pub via_class: Option<String>,
    /// "keep" (current poses) or "schematic" (grouped initial placement from the schematic)
    #[serde(default)]
    pub init: Option<String>,
    #[serde(default)]
    pub weights: Option<Weights>,
    #[serde(default)]
    pub loop_nets: Vec<String>,
    #[serde(default)]
    pub moves: Option<usize>,
    #[serde(default)]
    pub seed: Option<u64>,
    /// parts allowed to move although locked copper touches them (that copper is dropped on export)
    #[serde(default)]
    pub free_locked_copper: bool,
}

pub fn ref_match(f: &Footprint, pat: &str) -> bool {
    if let Some(s) = pat.strip_prefix("sheet:") {
        return wild(&f.sheetname, s);
    }
    wild(&f.refd, pat)
}

// ------------------------------------------------------------------------------------------------
// rng

#[derive(Clone)]
pub struct Rng(u64);
impl Rng {
    pub fn new(s: u64) -> Rng {
        Rng(s.wrapping_mul(0x9E3779B97F4A7C15) | 1)
    }
    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545F4914F6CDD1D)
    }
    pub fn f(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }
    pub fn gauss(&mut self) -> f64 {
        let (u, v) = (self.f().max(1e-12), self.f());
        (-2.0 * u.ln()).sqrt() * (2.0 * std::f64::consts::PI * v).cos()
    }
    pub fn below(&mut self, n: usize) -> usize {
        (self.next_u64() % n as u64) as usize
    }
}

// ------------------------------------------------------------------------------------------------
// problem

#[derive(Clone)]
pub struct PartSpec {
    pub fi: usize,
    pub region: Option<Vec<P>>,
    pub sides: (bool, bool), // F allowed, B allowed
    pub rot_step: f64,
    pub rot_pref: Vec<f64>,
    pub near: Vec<(P, f64)>,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct Pose {
    pub at: P,
    pub rot: f64,
    pub back: bool,
}

/// geometry of one placed part
#[derive(Clone)]
pub struct Placed {
    pub pads: Vec<(Shape, u8, usize, f64, usize)>, // shape, layers, net, hole radius, pad index
    pub crtyd: Vec<P>,
    pub cbb: BBox,
    pub pbb: BBox,
    pub pins: Vec<(usize, P, P)>, // net, centre, exit direction
}

pub struct Problem<'a> {
    pub m: &'a Model<'a>,
    pub parts: Vec<PartSpec>,
    pub idx_of_fp: HashMap<usize, usize>,
    pub fixed: Vec<Obj>,
    pub fixed_grid: Bucket,
    pub fixed_crtyd: Vec<(Vec<P>, BBox, bool)>, // courtyards of fixed parts with side
    pub w: Weights,
    pub nets: Vec<usize>, // nets touched by movable parts
    pub net_pins_fixed: HashMap<usize, Vec<P>>,
    pub net_w: HashMap<usize, f64>,
    pub net_maxlen: HashMap<usize, f64>,
    pub eval: Option<(GridSpec, usize, f64, f64)>, // grid, rep net, via r, drill r
    pub base_count: Vec<u16>,
    pub maxclr: f64,
    /// the board has a closed Edge.Cuts outline (courtyards must lie inside it)
    pub has_outline: bool,
    pub fp_keepouts: Vec<(Vec<P>, u8)>, // rule areas forbidding footprints
}

/// uniform bucket grid over object bounding boxes
pub struct Bucket {
    pub x0: f64,
    pub y0: f64,
    pub s: f64,
    pub nx: usize,
    pub ny: usize,
    pub cells: Vec<Vec<u32>>,
}

impl Bucket {
    pub fn new(bb: BBox, s: f64, items: &[BBox]) -> Bucket {
        let nx = ((bb.x1 - bb.x0) / s).ceil() as usize + 1;
        let ny = ((bb.y1 - bb.y0) / s).ceil() as usize + 1;
        let mut b = Bucket {
            x0: bb.x0,
            y0: bb.y0,
            s,
            nx,
            ny,
            cells: vec![Vec::new(); nx * ny],
        };
        for (i, ib) in items.iter().enumerate() {
            b.for_cells(ib, |c| b_push(c, i));
            fn b_push(_c: usize, _i: usize) {}
            let (x0, y0, x1, y1) = b.span(ib);
            for y in y0..=y1 {
                for x in x0..=x1 {
                    b.cells[y * nx + x].push(i as u32);
                }
            }
        }
        b
    }
    fn span(&self, ib: &BBox) -> (usize, usize, usize, usize) {
        let c =
            |v: f64, o: f64, n: usize| (((v - o) / self.s).floor().max(0.0) as usize).min(n - 1);
        (
            c(ib.x0, self.x0, self.nx),
            c(ib.y0, self.y0, self.ny),
            c(ib.x1, self.x0, self.nx),
            c(ib.y1, self.y0, self.ny),
        )
    }
    fn for_cells<F: FnMut(usize)>(&self, ib: &BBox, mut f: F) {
        let (x0, y0, x1, y1) = self.span(ib);
        for y in y0..=y1 {
            for x in x0..=x1 {
                f(y * self.nx + x);
            }
        }
    }
    pub fn query(&self, ib: &BBox, out: &mut Vec<u32>) {
        out.clear();
        let (x0, y0, x1, y1) = self.span(ib);
        for y in y0..=y1 {
            for x in x0..=x1 {
                out.extend_from_slice(&self.cells[y * self.nx + x]);
            }
        }
        out.sort_unstable();
        out.dedup();
    }
}

pub fn place_part(f: &Footprint, pose: &Pose) -> Placed {
    let g = f.moved(pose.at, pose.rot, pose.back);
    let mut pads = Vec::new();
    let mut pbb = BBox::empty();
    let mut pins = Vec::new();
    for (pi, pd) in g.pads.iter().enumerate() {
        let layers = if pd.kind == "np_thru_hole" {
            0x3f
        } else {
            pd.layers
        };
        if layers == 0 {
            continue;
        }
        let hole_r = if pd.kind.contains("thru_hole") {
            pd.drill / 2.0
        } else {
            0.0
        };
        for s in g.pad_shapes(pd) {
            let b = s.bbox();
            pbb.add(p(b.x0, b.y0));
            pbb.add(p(b.x1, b.y1));
            pads.push((s, layers, pd.net, hole_r, pi));
        }
        if pd.net != 0 {
            let (c, ang) = g.pad_world(pd);
            // exit direction: along the pad's long axis, away from the footprint centre
            let ax = if pd.size.0 >= pd.size.1 {
                p(1.0, 0.0).rot(ang)
            } else {
                p(0.0, 1.0).rot(ang)
            };
            let out = c - g.at;
            let mut d = if out.dot(ax) >= 0.0 { ax } else { ax * -1.0 };
            if out.len() < 1e-6 {
                d = p(0.0, 0.0);
            }
            pins.push((pd.net, c, d));
        }
    }
    let crtyd = g.crtyd_world();
    let mut cbb = BBox::empty();
    for q in &crtyd {
        cbb.add(*q);
    }
    if !cbb.valid() {
        cbb = pbb;
    }
    Placed {
        pads,
        crtyd,
        cbb,
        pbb,
        pins,
    }
}

pub fn poly_overlap(a: &[P], b: &[P]) -> bool {
    if a.len() < 3 || b.len() < 3 {
        return false;
    }
    let (na, nb) = (a.len(), b.len());
    for i in 0..na {
        for j in 0..nb {
            // touching / collinear edges count as overlap (KiCad courtyards_overlap does too)
            if seg_seg_dist(a[i], a[(i + 1) % na], b[j], b[(j + 1) % nb]) < 0.01 {
                return true;
            }
        }
    }
    point_in_poly(a[0], b) || point_in_poly(b[0], a)
}

fn poly_inside(inner: &[P], outer: &[P]) -> bool {
    inner.iter().all(|q| point_in_poly(*q, outer))
}

impl<'a> Problem<'a> {
    pub fn new(m: &'a Model<'a>, zf: &ZonesFile) -> Result<Problem<'a>, String> {
        let b = m.b;
        // locked copper attached to a part -> not movable (unless free_locked_copper)
        let mut has_locked_copper: HashSet<usize> = HashSet::new();
        if !zf.free_locked_copper {
            for o in &m.objs {
                if !matches!(o.kind, Kind::Track | Kind::Via) {
                    continue;
                }
                for (fi, f) in b.fps.iter().enumerate() {
                    for pd in &f.pads {
                        if pd.net != o.net || pd.net == 0 {
                            continue;
                        }
                        for s in f.pad_shapes(pd) {
                            if s.bbox().overlaps(&o.bbox)
                                && pd.layers & o.layers != 0
                                && shape_dist(&s, &o.shape) < 1e-3
                            {
                                has_locked_copper.insert(fi);
                            }
                        }
                    }
                }
            }
        }
        let mut parts = Vec::new();
        let mut idx_of_fp = HashMap::new();
        for (fi, f) in b.fps.iter().enumerate() {
            if !zf.movable.iter().any(|pt| ref_match(f, pt))
                || zf.fixed.iter().any(|pt| ref_match(f, pt))
            {
                continue;
            }
            if f.locked || has_locked_copper.contains(&fi) {
                eprintln!(
                    "[place] {} stays: {}",
                    f.refd,
                    if f.locked {
                        "locked"
                    } else {
                        "locked copper attached"
                    }
                );
                continue;
            }
            let mut spec = PartSpec {
                fi,
                region: None,
                sides: (!f.back, f.back),
                rot_step: 45.0,
                rot_pref: vec![],
                near: vec![],
            };
            if f.refd.starts_with('J') || f.refd.starts_with('X') {
                spec.rot_step = 0.0; // connector facing is fixed unless a zone says otherwise
            }
            let mut got_shape = false;
            for z in &zf.zones {
                if !z.refs.iter().any(|pt| ref_match(f, pt)) {
                    continue;
                }
                if !got_shape && (z.rect.is_some() || z.poly.is_some()) {
                    spec.region = Some(match (&z.rect, &z.poly) {
                        (Some(r), _) => {
                            vec![p(r[0], r[1]), p(r[2], r[1]), p(r[2], r[3]), p(r[0], r[3])]
                        }
                        (_, Some(pl)) => pl.iter().map(|q| p(q[0], q[1])).collect(),
                        _ => unreachable!(),
                    });
                    got_shape = true;
                }
                if let Some(s) = &z.side {
                    spec.sides = (s.contains('F'), s.contains('B'));
                }
                if let Some(rs) = z.rot_step {
                    spec.rot_step = rs;
                }
                if !z.rot_pref.is_empty() {
                    spec.rot_pref = z.rot_pref.clone();
                }
                if let Some(n) = &z.near {
                    if let Some(t) = b.fp(&n.r#ref) {
                        let tp = match &n.pin {
                            Some(pn) => t
                                .pads
                                .iter()
                                .find(|pd| &pd.num == pn)
                                .map(|pd| t.pad_world(pd).0)
                                .unwrap_or(t.at),
                            None => t.at,
                        };
                        spec.near.push((tp, n.max));
                    }
                }
            }
            idx_of_fp.insert(fi, parts.len());
            parts.push(spec);
        }
        if parts.is_empty() {
            return Err("no movable parts".into());
        }
        // fixed objects: everything except pads/via-keepouts of movable parts
        let fixed: Vec<Obj> = m
            .objs
            .iter()
            .filter(|o| match o.kind {
                Kind::Pad { fp, .. } => !idx_of_fp.contains_key(&fp),
                // mask apertures move with their part; they only matter to the router
                Kind::Mask { fp, .. } => !idx_of_fp.contains_key(&fp),
                _ => true,
            })
            .filter(|o| {
                // via keepout courtyards of movable parts are recomputed
                if let Kind::ViaKeepout = o.kind {
                    !parts.iter().any(|ps| {
                        let c = b.fps[ps.fi].crtyd_world();
                        c.len() >= 3
                            && (c[0] - o.shape.bbox().center()).len() < 1e3
                            && o.bbox
                                == Shape::Poly {
                                    pts: c.clone(),
                                    r: 0.0,
                                }
                                .bbox()
                    })
                } else {
                    true
                }
            })
            .cloned()
            .collect();
        let bbs: Vec<BBox> = fixed.iter().map(|o| o.bbox).collect();
        let fixed_grid = Bucket::new(b.bbox.inflate(10.0), 2.0, &bbs);
        let fixed_crtyd = b
            .fps
            .iter()
            .enumerate()
            .filter(|(fi, _)| !idx_of_fp.contains_key(fi))
            .filter_map(|(_, f)| {
                let c = f.crtyd_world();
                if c.len() < 3 {
                    return None;
                }
                let mut bb = BBox::empty();
                for q in &c {
                    bb.add(*q);
                }
                Some((c, bb, f.back))
            })
            .collect();
        // nets
        let mut nets: Vec<usize> = Vec::new();
        let mut npins = vec![0usize; b.nets.len()];
        for f in &b.fps {
            for pd in &f.pads {
                npins[pd.net] += 1;
            }
        }
        for ps in &parts {
            for pd in &b.fps[ps.fi].pads {
                if pd.net != 0
                    && npins[pd.net] >= 2
                    && npins[pd.net] <= 40
                    && !b.nets[pd.net].starts_with("unconnected-")
                {
                    nets.push(pd.net);
                }
            }
        }
        nets.sort();
        nets.dedup();
        let mut net_pins_fixed: HashMap<usize, Vec<P>> = HashMap::new();
        for (fi, f) in b.fps.iter().enumerate() {
            if idx_of_fp.contains_key(&fi) {
                continue;
            }
            for pd in &f.pads {
                if nets.binary_search(&pd.net).is_ok() {
                    net_pins_fixed
                        .entry(pd.net)
                        .or_default()
                        .push(f.pad_world(pd).0);
                }
            }
        }
        let w = zf.weights.clone().unwrap_or_default();
        // loop nets (e.g. gate-drive or sense loops) get the extra `loop_` HPWL weight
        let loop_pats: Vec<String> = zf.loop_nets.clone();
        let mut net_w = HashMap::new();
        let mut net_maxlen = HashMap::new();
        for &n in &nets {
            let name = &b.nets[n];
            let mut wt = w.hpwl;
            if loop_pats.iter().any(|pt| wild(name, pt)) {
                wt += w.loop_;
            }
            net_w.insert(n, wt);
            if let Some(ml) = m.r.max_length(n) {
                net_maxlen.insert(n, ml);
            }
        }
        // evaluation region for via sites
        let eval = match (&zf.eval_region, &zf.via_class) {
            (Some(r), Some(cn)) => {
                let ci = *m.r.class_idx.get(cn).ok_or(format!("unknown class {cn}"))?;
                let rep = crate::sites::class_reps(m)
                    .into_iter()
                    .find(|(c, _)| *c == ci)
                    .map(|(_, n)| n)
                    .ok_or("class has no routable net")?;
                let (vd, vdr) = m.r.via(rep);
                Some((
                    GridSpec::new(
                        BBox {
                            x0: r[0],
                            y0: r[1],
                            x1: r[2],
                            y1: r[3],
                        },
                        0.25,
                    ),
                    rep,
                    vd / 2.0,
                    vdr / 2.0,
                ))
            }
            _ => None,
        };
        let mut maxclr: f64 = 0.0;
        for v in &m.r.mat[0] {
            maxclr = maxclr.max(*v as f64);
        }
        let fp_keepouts = b
            .zones
            .iter()
            .filter(|z| z.keepout && z.ko_footprints && z.outline.len() >= 3)
            .map(|z| (z.outline.clone(), z.layers))
            .collect();
        let mut pb = Problem {
            m,
            parts,
            idx_of_fp,
            fixed,
            fixed_grid,
            fixed_crtyd,
            w,
            nets,
            net_pins_fixed,
            net_w,
            net_maxlen,
            eval,
            base_count: vec![],
            maxclr,
            has_outline: !b.edge.is_empty(),
            fp_keepouts,
        };
        pb.base_count = pb.fixed_via_count();
        Ok(pb)
    }

    // --- via-site count map in the evaluation region ------------------------------------------
    fn via_stamp_d(&self, net: usize, layers: u8, is_pad: bool, mh: Option<&str>) -> f64 {
        let Some((_, rep, vr, _)) = &self.eval else {
            return -1.0;
        };
        let mut d: f64 = -1.0;
        for l in 0..NL {
            if layers & (1 << l) == 0 {
                continue;
            }
            let mut c = if net == *rep && net != 0 {
                -1.0
            } else {
                self.m.r.clr(*rep, net, is_outer(l))
            };
            if is_pad {
                c = c.max(0.2);
            }
            if c >= 0.0 {
                d = d.max(c + vr);
            }
        }
        if let Some(r) = mh {
            d = d.max(self.m.r.hole_keepout(r, *rep) + vr);
        }
        d
    }

    fn fixed_via_count(&self) -> Vec<u16> {
        let Some((gs, rep, vr, dr)) = &self.eval else {
            return vec![];
        };
        let mut cnt = vec![0u16; gs.n()];
        let vm = crate::sites::via_map(self.m, gs, *rep, *vr, *dr, &self.fixed);
        for (i, v) in vm.iter().enumerate() {
            if *v != 0 {
                cnt[i] = 1;
            }
        }
        cnt
    }

    /// stamp (+1 / -1) a placed part into the count map, returns change of free cells
    fn stamp_part(&self, fi: usize, pl: &Placed, cnt: &mut [u16], sign: i32) -> i64 {
        let Some((gs, _, vr, dr)) = &self.eval else {
            return 0;
        };
        let bbr = gs.bbox_f();
        let f = &self.m.b.fps[fi];
        let mh = if self.m.is_mh(fi) {
            Some(f.refd.as_str())
        } else {
            None
        };
        let mut dfree = 0i64;
        let mut apply = |i: usize, cnt: &mut [u16]| {
            if sign > 0 {
                if cnt[i] == 0 {
                    dfree -= 1;
                }
                cnt[i] += 1;
            } else {
                cnt[i] -= 1;
                if cnt[i] == 0 {
                    dfree += 1;
                }
            }
        };
        for (s, layers, net, hole_r, _) in &pl.pads {
            let d = self.via_stamp_d(*net, *layers, true, mh);
            if d < 0.0 || !s.bbox().inflate(d).overlaps(&bbr) {
                continue;
            }
            let mut cells = Vec::new();
            gs.stamp(s, d, |x, y| cells.push(gs.idx(x, y)));
            if *hole_r > 0.0 {
                let c = s.bbox().center();
                gs.stamp(
                    &Shape::circle(c, 0.0),
                    self.m.r.min_hole_to_hole + hole_r + dr,
                    |x, y| cells.push(gs.idx(x, y)),
                );
            }
            cells.sort_unstable();
            cells.dedup();
            for i in cells {
                apply(i, cnt);
            }
        }
        if self
            .m
            .cfg
            .via_keepout_refs
            .iter()
            .any(|pt| wild(&f.refd, pt))
            && pl.crtyd.len() >= 3
        {
            let mut cells = Vec::new();
            gs.stamp(
                &Shape::Poly {
                    pts: pl.crtyd.clone(),
                    r: 0.0,
                },
                *vr,
                |x, y| cells.push(gs.idx(x, y)),
            );
            cells.sort_unstable();
            cells.dedup();
            for i in cells {
                apply(i, cnt);
            }
        }
        dfree
    }

    // --- hard rules ---------------------------------------------------------------------------
    /// number of rule violations of part k at its placement against fixed items and other parts
    pub fn violations(
        &self,
        k: usize,
        pl: &Placed,
        all: &[Placed],
        poses: &[Pose],
        q: &mut Vec<u32>,
    ) -> usize {
        let spec = &self.parts[k];
        let f = &self.m.b.fps[spec.fi];
        let mut v = 0usize;
        let back = poses[k].back;
        if (back && !spec.sides.1) || (!back && !spec.sides.0) {
            v += 10;
        }
        // zone
        if let Some(rg) = &spec.region {
            if !poly_inside(&pl.crtyd, rg)
                && (pl.crtyd.len() >= 3 || !point_in_poly(poses[k].at, rg))
            {
                v += 1;
            }
        }
        // board outline
        if self.has_outline && !pl.crtyd.iter().all(|q| self.m.b.inside(*q)) {
            v += 1;
        }
        // near
        for (tp, mx) in &spec.near {
            if (poses[k].at - *tp).len() > *mx {
                v += 1;
            }
        }
        // footprint keepouts
        for (ko, layers) in &self.fp_keepouts {
            let side_bit = if back { 1 << 5 } else { 1 };
            if layers & side_bit != 0 && poly_overlap(&pl.crtyd, ko) {
                v += 1;
            }
        }
        // courtyards
        let side_courtyard = |bk: bool| bk == back;
        for (c, bb, bk) in &self.fixed_crtyd {
            if side_courtyard(*bk) && bb.overlaps(&pl.cbb) && poly_overlap(&pl.crtyd, c) {
                v += 1;
            }
        }
        for (j, o) in all.iter().enumerate() {
            if j != k
                && poses[j].back == back
                && o.cbb.overlaps(&pl.cbb)
                && poly_overlap(&pl.crtyd, &o.crtyd)
            {
                v += 1;
            }
        }
        // copper clearance against fixed objects
        let mh_self = self.m.is_mh(spec.fi);
        for (s, layers, net, _, _) in &pl.pads {
            let sb = s.bbox();
            self.fixed_grid.query(&sb.inflate(self.maxclr), q);
            for &oi in q.iter() {
                let o = &self.fixed[oi as usize];
                if matches!(o.kind, Kind::ViaKeepout) || matches!(o.kind, Kind::Keepout { .. }) {
                    continue;
                }
                let common = layers & o.layers;
                if common == 0 {
                    continue;
                }
                let mut c: f64 = -1.0;
                for l in 0..NL {
                    if common & (1 << l) != 0 {
                        c = c.max(self.m.clr(*net, o, l));
                    }
                }
                if let Kind::Pad { fp, .. } = o.kind {
                    if self.m.is_mh(fp) {
                        c = c.max(self.m.r.hole_keepout(&self.m.b.fps[fp].refd, *net));
                    }
                }
                if mh_self {
                    c = c.max(self.m.r.hole_keepout(&f.refd, o.net));
                }
                if matches!(o.kind, Kind::Via) {
                    c = c.max(0.2); // via-to-pad gap: no via in or touching a pad (any net, also the pad's own)
                }
                if c < 0.0 || !sb.inflate(c).overlaps(&o.bbox) {
                    continue;
                }
                if shape_dist(s, &o.shape) < c - 1e-4 {
                    v += 1;
                }
            }
            // edge clearance
            let ec = self.m.r.edge_clearance(*net);
            for (a, e) in &self.m.b.edge_segs {
                if dist_seg(sb.center(), *a, *e) < ec + (sb.x1 - sb.x0).max(sb.y1 - sb.y0)
                    && shape_dist(
                        s,
                        &Shape::Capsule {
                            a: *a,
                            b: *e,
                            r: 0.0,
                        },
                    ) < ec - 1e-4
                {
                    v += 1;
                }
            }
        }
        // copper clearance against other movable parts
        for (j, o) in all.iter().enumerate() {
            if j == k || !o.pbb.inflate(self.maxclr).overlaps(&pl.pbb) {
                continue;
            }
            for (s, la, na, _, _) in &pl.pads {
                let sb = s.bbox();
                for (t, lb, nb, _, _) in &o.pads {
                    let common = la & lb;
                    if common == 0 || (na == nb && *na != 0) {
                        continue;
                    }
                    let mut c: f64 = 0.0;
                    for l in 0..NL {
                        if common & (1 << l) != 0 {
                            c = c.max(self.m.r.clr(*na, *nb, is_outer(l)));
                        }
                    }
                    if !sb.inflate(c).overlaps(&t.bbox()) {
                        continue;
                    }
                    if shape_dist(s, t) < c - 1e-4 {
                        v += 1;
                    }
                }
            }
        }
        v
    }

    // --- cost ---------------------------------------------------------------------------------
    fn net_pins(&self, n: usize, all: &[Placed]) -> Vec<(P, P)> {
        let mut v: Vec<(P, P)> = self
            .net_pins_fixed
            .get(&n)
            .map(|x| x.iter().map(|q| (*q, p(0.0, 0.0))).collect())
            .unwrap_or_default();
        for pl in all {
            for (nn, c, d) in &pl.pins {
                if *nn == n {
                    v.push((*c, *d));
                }
            }
        }
        v
    }

    /// MST edges of a net (indices into pins)
    fn mst(pins: &[(P, P)]) -> Vec<(usize, usize)> {
        let k = pins.len();
        if k < 2 {
            return vec![];
        }
        let mut in_t = vec![false; k];
        let mut best = vec![(f64::INFINITY, 0usize); k];
        in_t[0] = true;
        for j in 1..k {
            best[j] = ((pins[0].0 - pins[j].0).len(), 0);
        }
        let mut e = Vec::new();
        for _ in 1..k {
            let mut bi = 0;
            let mut bd = f64::INFINITY;
            for j in 0..k {
                if !in_t[j] && best[j].0 < bd {
                    bd = best[j].0;
                    bi = j;
                }
            }
            in_t[bi] = true;
            e.push((best[bi].1, bi));
            for j in 0..k {
                if !in_t[j] {
                    let d = (pins[bi].0 - pins[j].0).len();
                    if d < best[j].0 {
                        best[j] = (d, bi);
                    }
                }
            }
        }
        e
    }

    /// per-net cost terms: (hpwl part, length penalty, exit-angle penalty) and ratsnest segments
    fn net_cost(&self, n: usize, all: &[Placed]) -> (f64, Vec<(P, P)>) {
        let pins = self.net_pins(n, all);
        if pins.len() < 2 {
            return (0.0, vec![]);
        }
        let mut bb = BBox::empty();
        for (c, _) in &pins {
            bb.add(*c);
        }
        let hpwl = (bb.x1 - bb.x0) + (bb.y1 - bb.y0);
        let mut cost = self.net_w[&n] * hpwl;
        let e = Self::mst(&pins);
        let mut len = 0.0;
        let mut exit = 0.0;
        let mut segs = Vec::new();
        for (a, b) in &e {
            let (pa, pb) = (pins[*a], pins[*b]);
            let d = pb.0 - pa.0;
            let l = d.len();
            len += l;
            segs.push((pa.0, pb.0));
            if l > 1e-6 {
                let u = d * (1.0 / l);
                for (dir, uu) in [(pa.1, u), (pb.1, u * -1.0)] {
                    if dir.len() < 0.5 {
                        continue;
                    }
                    let c = dir.dot(uu);
                    let ang = c.clamp(-1.0, 1.0).acos().to_degrees();
                    exit += if c < 0.0 {
                        1.0
                    } else {
                        ((ang - 22.5) / 67.5).clamp(0.0, 1.0)
                    };
                }
            }
        }
        if let Some(ml) = self.net_maxlen.get(&n) {
            let est = len * 1.15;
            if est > *ml {
                cost += self.w.len * (est - ml);
            }
        }
        cost += self.w.exit * exit;
        (cost, segs)
    }

    fn crossings(segs: &[Vec<(P, P)>], k: usize) -> usize {
        // crossings of net k's segments with all others
        let mut c = 0;
        for (a, b) in &segs[k] {
            for (j, s) in segs.iter().enumerate() {
                if j == k {
                    continue;
                }
                for (c2, d2) in s {
                    if segs_intersect(*a, *b, *c2, *d2) {
                        c += 1;
                    }
                }
            }
        }
        c
    }

    fn part_soft(&self, k: usize, pose: &Pose) -> f64 {
        let spec = &self.parts[k];
        let mut c = 0.0;
        if !spec.rot_pref.is_empty()
            && !spec.rot_pref.iter().any(|r| {
                ((r - pose.rot).rem_euclid(360.0)).min((pose.rot - r).rem_euclid(360.0)) < 1.0
            })
        {
            c += self.w.pref;
        }
        for (tp, _) in &spec.near {
            c += self.w.near * (pose.at - *tp).len();
        }
        let r = pose.rot.rem_euclid(90.0);
        if r > 1.0 && r < 89.0 {
            c += self.w.diag;
        }
        c
    }
}

impl GridSpec {
    pub fn bbox_f(&self) -> BBox {
        BBox {
            x0: self.x0,
            y0: self.y0,
            x1: self.x0 + self.nx as f64 * self.res,
            y1: self.y0 + self.ny as f64 * self.res,
        }
    }
}

// ------------------------------------------------------------------------------------------------
// state + annealing

#[derive(Clone)]
pub struct State {
    pub poses: Vec<Pose>,
    pub placed: Vec<Placed>,
    pub viol: Vec<usize>,
    pub cnt: Vec<u16>,
    pub free: i64,
    pub net_c: HashMap<usize, f64>,
    pub segs: Vec<Vec<(P, P)>>, // per net index (self.nets order)
    pub cross: usize,
    pub soft: Vec<f64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Terms {
    pub total: f64,
    pub net: f64,
    pub cross: usize,
    pub via_sites: i64,
    pub soft: f64,
    pub violations: usize,
}

impl<'a> Problem<'a> {
    pub fn init_state(&self, poses: Vec<Pose>) -> State {
        let placed: Vec<Placed> = self
            .parts
            .iter()
            .zip(&poses)
            .map(|(s, ps)| place_part(&self.m.b.fps[s.fi], ps))
            .collect();
        let mut q = Vec::new();
        let viol: Vec<usize> = (0..self.parts.len())
            .map(|k| self.violations(k, &placed[k], &placed, &poses, &mut q))
            .collect();
        let mut cnt = self.base_count.clone();
        let mut free = cnt.iter().filter(|c| **c == 0).count() as i64;
        if self.eval.is_some() {
            for (k, pl) in placed.iter().enumerate() {
                free += self.stamp_part(self.parts[k].fi, pl, &mut cnt, 1);
            }
        }
        let mut net_c = HashMap::new();
        let mut segs = Vec::new();
        for &n in &self.nets {
            let (c, s) = self.net_cost(n, &placed);
            net_c.insert(n, c);
            segs.push(s);
        }
        let mut cross = 0;
        for k in 0..segs.len() {
            cross += Self::crossings(&segs, k);
        }
        cross /= 2;
        let soft = (0..self.parts.len())
            .map(|k| self.part_soft(k, &poses[k]))
            .collect();
        State {
            poses,
            placed,
            viol,
            cnt,
            free,
            net_c,
            segs,
            cross,
            soft,
        }
    }

    pub fn terms(&self, s: &State) -> Terms {
        let net: f64 = s.net_c.values().sum();
        let soft: f64 = s.soft.iter().sum();
        let total = net + self.w.cross * s.cross as f64 - self.w.via * s.free as f64 + soft;
        Terms {
            total,
            net,
            cross: s.cross,
            via_sites: s.free,
            soft,
            violations: s.viol.iter().sum(),
        }
    }

    fn rand_pose(&self, k: usize, cur: &Pose, rng: &mut Rng, sigma: f64) -> Pose {
        let spec = &self.parts[k];
        let mut np = *cur;
        let r = rng.f();
        if r < 0.15 && spec.rot_step > 0.0 {
            let steps = [1.0, -1.0, 2.0, -2.0, 4.0];
            let st = steps[rng.below(steps.len())];
            let mut stp = spec.rot_step;
            if stp < 45.0 {
                stp = 45.0;
            }
            np.rot = ((cur.rot + st * stp) % 360.0 + 360.0) % 360.0;
            if np.rot > 180.0 {
                np.rot -= 360.0;
            }
        } else if r < 0.2 && spec.sides.0 && spec.sides.1 {
            np.back = !cur.back;
        } else {
            np.at = p(
                cur.at.x + rng.gauss() * sigma,
                cur.at.y + rng.gauss() * sigma,
            );
            np.at = p(
                (np.at.x / 0.05).round() * 0.05,
                (np.at.y / 0.05).round() * 0.05,
            );
        }
        np
    }

    /// apply a move of part k to state; returns delta cost (state mutated); undo via returned closure data
    fn try_move(&self, s: &mut State, k: usize, np: Pose, q: &mut Vec<u32>) -> Option<(f64, Undo)> {
        let fi = self.parts[k].fi;
        let newpl = place_part(&self.m.b.fps[fi], &np);
        // hard rules: the part's violations may not increase, nor may any other part's
        let oldpose = s.poses[k];
        let oldpl = std::mem::replace(&mut s.placed[k], newpl);
        s.poses[k] = np;
        let nv = self.violations(k, &s.placed[k], &s.placed, &s.poses, q);
        if nv > s.viol[k] {
            s.placed[k] = oldpl;
            s.poses[k] = oldpose;
            return None;
        }
        // neighbours affected (overlap/clearance with this part): recompute their violation counts
        let mut nb_viol = Vec::new();
        let near_bb = s.placed[k].cbb.inflate(self.maxclr);
        let old_bb = oldpl.cbb.inflate(self.maxclr);
        for j in 0..self.parts.len() {
            if j == k {
                continue;
            }
            if s.placed[j].cbb.overlaps(&near_bb) || s.placed[j].cbb.overlaps(&old_bb) {
                let v = self.violations(j, &s.placed[j], &s.placed, &s.poses, q);
                if v > s.viol[j] {
                    s.placed[k] = oldpl;
                    s.poses[k] = oldpose;
                    return None;
                }
                nb_viol.push((j, s.viol[j], v));
            }
        }
        let old_cost = self.state_cost_part(s, k, &oldpl);
        // via map update
        let mut dfree = 0;
        if self.eval.is_some() {
            dfree += self.stamp_part(fi, &oldpl, &mut s.cnt, -1);
            dfree += self.stamp_part(fi, &s.placed[k], &mut s.cnt, 1);
        }
        s.free += dfree;
        // nets
        let mut old_nets = Vec::new();
        let mut cross_old = 0;
        let mut cross_new = 0;
        let pnets: Vec<usize> = {
            let mut v: Vec<usize> = s.placed[k]
                .pins
                .iter()
                .map(|x| x.0)
                .filter(|n| self.net_w.contains_key(n))
                .collect();
            v.sort();
            v.dedup();
            v
        };
        let idx: Vec<usize> = pnets
            .iter()
            .map(|n| self.nets.binary_search(n).unwrap())
            .collect();
        for &ni in &idx {
            cross_old += Self::crossings(&s.segs, ni);
        }
        for (&n, &ni) in pnets.iter().zip(&idx) {
            let (c, sg) = self.net_cost(n, &s.placed);
            old_nets.push((n, ni, s.net_c[&n], std::mem::replace(&mut s.segs[ni], sg)));
            s.net_c.insert(n, c);
        }
        for &ni in &idx {
            cross_new += Self::crossings(&s.segs, ni);
        }
        // crossings between two moved nets are counted twice in both sums -> consistent delta
        let oldcross = s.cross;
        s.cross = (s.cross as i64 + cross_new as i64 - cross_old as i64).max(0) as usize;
        let oldsoft = s.soft[k];
        s.soft[k] = self.part_soft(k, &np);
        for (j, _, v) in &nb_viol {
            s.viol[*j] = *v;
        }
        let oldviol = s.viol[k];
        s.viol[k] = nv;
        let new_cost = self.state_cost_part(s, k, &s.placed[k]);
        let _ = (old_cost, new_cost);
        let dnet: f64 = old_nets.iter().map(|(n, _, oc, _)| s.net_c[n] - oc).sum();
        let delta = dnet + self.w.cross * (s.cross as f64 - oldcross as f64)
            - self.w.via * dfree as f64
            + (s.soft[k] - oldsoft)
            + 1000.0 * (nv as f64 - oldviol as f64);
        Some((
            delta,
            Undo {
                k,
                oldpose,
                oldpl,
                old_nets,
                oldcross,
                oldsoft,
                oldviol,
                nb_viol,
                dfree,
            },
        ))
    }

    fn state_cost_part(&self, _s: &State, _k: usize, _pl: &Placed) -> f64 {
        0.0
    }

    fn undo(&self, s: &mut State, u: Undo) {
        let k = u.k;
        let fi = self.parts[k].fi;
        if self.eval.is_some() {
            self.stamp_part(fi, &s.placed[k], &mut s.cnt, -1);
            self.stamp_part(fi, &u.oldpl, &mut s.cnt, 1);
        }
        s.free -= u.dfree;
        s.placed[k] = u.oldpl;
        s.poses[k] = u.oldpose;
        for (n, ni, oc, sg) in u.old_nets {
            s.net_c.insert(n, oc);
            s.segs[ni] = sg;
        }
        s.cross = u.oldcross;
        s.soft[k] = u.oldsoft;
        s.viol[k] = u.oldviol;
        for (j, ov, _) in u.nb_viol {
            s.viol[j] = ov;
        }
    }

    pub fn anneal(
        &self,
        start: &State,
        moves: usize,
        seed: u64,
        t0_scale: f64,
    ) -> (State, Vec<(usize, f64)>) {
        let mut rng = Rng::new(seed);
        let mut s = start.clone();
        let mut q = Vec::new();
        let n = self.parts.len();
        // initial temperature from random move deltas
        let mut ds = Vec::new();
        for _ in 0..200 {
            let k = rng.below(n);
            let np = self.rand_pose(k, &s.poses[k], &mut rng, 1.0);
            if let Some((d, u)) = self.try_move(&mut s, k, np, &mut q) {
                ds.push(d.abs());
                self.undo(&mut s, u);
            }
        }
        ds.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let t0 = if ds.is_empty() {
            1.0
        } else {
            ds[ds.len() / 2].max(1e-3) * t0_scale
        };
        let t1 = t0 * 1e-3;
        let mut best = s.clone();
        let mut best_c = self.terms(&s).total;
        let mut cur_c = best_c;
        let mut trace = Vec::new();
        let mut acc = 0usize;
        for it in 0..moves {
            let fr = it as f64 / moves as f64;
            let t = t0 * (t1 / t0).powf(fr);
            let sigma = 3.0 * (1.0 - fr) + 0.1;
            let k = rng.below(n);
            let np = self.rand_pose(k, &s.poses[k], &mut rng, sigma);
            let Some((d, u)) = self.try_move(&mut s, k, np, &mut q) else {
                continue;
            };
            if d <= 0.0 || rng.f() < (-d / t).exp() {
                cur_c += d;
                acc += 1;
                if cur_c < best_c - 1e-9 {
                    best_c = cur_c;
                    best = s.clone();
                }
            } else {
                self.undo(&mut s, u);
            }
            if it % (moves / 20).max(1) == 0 {
                trace.push((it, cur_c));
            }
        }
        let _ = acc;
        self.repair(&mut best, &mut rng, &mut q);
        (best, trace)
    }
}

impl<'a> Problem<'a> {
    /// greedy legalisation: random moves of violating parts, accepted only if they lower the
    /// total violation count (cost is ignored)
    pub fn repair(&self, s: &mut State, rng: &mut Rng, q: &mut Vec<u32>) {
        for _ in 0..20000 {
            let bad: Vec<usize> = (0..self.parts.len()).filter(|k| s.viol[*k] > 0).collect();
            if bad.is_empty() {
                return;
            }
            let k = bad[rng.below(bad.len())];
            let sigma = 0.3 + 5.0 * rng.f();
            let np = self.rand_pose(k, &s.poses[k], rng, sigma);
            let before: usize = s.viol.iter().sum();
            if let Some((_, u)) = self.try_move(s, k, np, q) {
                let after: usize = s.viol.iter().sum();
                if after >= before {
                    self.undo(s, u);
                }
            }
        }
    }
}

/// saved cost terms of one net: (net, index, cost, ratsnest segments)
type NetSave = (usize, usize, f64, Vec<(P, P)>);

pub struct Undo {
    k: usize,
    oldpose: Pose,
    oldpl: Placed,
    old_nets: Vec<NetSave>,
    oldcross: usize,
    oldsoft: f64,
    oldviol: usize,
    nb_viol: Vec<(usize, usize, usize)>,
    dfree: i64,
}

// ------------------------------------------------------------------------------------------------
// schematic-grouped initial placement

/// poses from the schematic: each group (sheet name) keeps its schematic arrangement, scaled so the
/// courtyards fit, centred on the group's zone (or its current centroid), then legalised by pushing
/// overlapping courtyards apart.
pub fn schematic_init(pb: &Problem, sch: &HashMap<String, SchPos>) -> Vec<Pose> {
    let b = pb.m.b;
    let mut poses: Vec<Pose> = pb
        .parts
        .iter()
        .map(|s| {
            let f = &b.fps[s.fi];
            Pose {
                at: f.at,
                rot: f.rot,
                back: f.back,
            }
        })
        .collect();
    let mut groups: HashMap<String, Vec<usize>> = HashMap::new();
    for (k, s) in pb.parts.iter().enumerate() {
        groups
            .entry(b.fps[s.fi].sheetname.clone())
            .or_default()
            .push(k);
    }
    for (_, ks) in groups {
        let pts: Vec<(usize, P)> = ks
            .iter()
            .filter_map(|&k| sch.get(&b.fps[pb.parts[k].fi].refd).map(|sp| (k, sp.at)))
            .collect();
        if pts.len() < 2 {
            continue;
        }
        let mut sb = BBox::empty();
        for (_, q) in &pts {
            sb.add(*q);
        }
        // target box: zone of the first part, else the current bbox of the group
        let mut tb = BBox::empty();
        if let Some(rg) = &pb.parts[pts[0].0].region {
            for q in rg {
                tb.add(*q);
            }
        } else {
            for &(k, _) in &pts {
                tb.add(b.fps[pb.parts[k].fi].at);
            }
        }
        let area: f64 = pts
            .iter()
            .map(|(k, _)| {
                let c = &pb.parts[*k];
                let f = &b.fps[c.fi];
                let mut bb = BBox::empty();
                for q in f.crtyd_world() {
                    bb.add(q);
                }
                if bb.valid() {
                    (bb.x1 - bb.x0) * (bb.y1 - bb.y0)
                } else {
                    1.0
                }
            })
            .sum();
        let sw = (sb.x1 - sb.x0).max(1.0);
        let sh = (sb.y1 - sb.y0).max(1.0);
        let fit = ((tb.x1 - tb.x0) / sw).min((tb.y1 - tb.y0) / sh) * 0.9;
        let dens = (area * 2.5 / (sw * sh)).sqrt();
        let k_s = fit.min(dens.max(fit * 0.5));
        let c_s = sb.center();
        let c_t = tb.center();
        for (k, q) in &pts {
            let f = &b.fps[pb.parts[*k].fi];
            let sp = &sch[&f.refd];
            poses[*k].at = c_t + (*q - c_s) * k_s;
            // keep current rotation modulo the schematic orientation hint (2-pin parts follow the symbol)
            if f.pads.len() == 2 && pb.parts[*k].rot_step > 0.0 {
                poses[*k].rot = -sp.rot;
            }
        }
        // legalise: push apart overlapping courtyards (bbox based), 200 iterations
        for _ in 0..200 {
            let mut moved = false;
            let bbs: Vec<BBox> = pts
                .iter()
                .map(|(k, _)| {
                    place_part(&b.fps[pb.parts[*k].fi], &poses[*k])
                        .cbb
                        .inflate(0.1)
                })
                .collect();
            for i in 0..pts.len() {
                for j in i + 1..pts.len() {
                    let (a, c) = (&bbs[i], &bbs[j]);
                    if !a.overlaps(c) || poses[pts[i].0].back != poses[pts[j].0].back {
                        continue;
                    }
                    let ox = (a.x1.min(c.x1) - a.x0.max(c.x0)).max(0.0);
                    let oy = (a.y1.min(c.y1) - a.y0.max(c.y0)).max(0.0);
                    let d = c.center() - a.center();
                    let (dx, dy) = if ox < oy {
                        ((ox / 2.0 + 0.05) * d.x.signum(), 0.0)
                    } else {
                        (0.0, (oy / 2.0 + 0.05) * d.y.signum())
                    };
                    let (dx, dy) = if dx == 0.0 && dy == 0.0 {
                        (0.1, 0.0)
                    } else {
                        (dx, dy)
                    };
                    poses[pts[i].0].at = poses[pts[i].0].at - p(dx, dy);
                    poses[pts[j].0].at = poses[pts[j].0].at + p(dx, dy);
                    moved = true;
                }
            }
            if !moved {
                break;
            }
        }
    }
    poses
}

/// run annealing from `init` on all threads (one start per thread), return the best state
/// start i begins from inits[i % inits.len()] (e.g. current poses and schematic-grouped poses)
pub fn optimize(
    pb: &Problem,
    inits: &[State],
    moves: usize,
    seed: u64,
    starts: usize,
    t0_scale: f64,
) -> (State, Vec<Vec<(usize, f64)>>) {
    let res: Vec<(State, Vec<(usize, f64)>)> = (0..starts)
        .into_par_iter()
        .map(|i| {
            pb.anneal(
                &inits[i % inits.len()],
                moves,
                seed + 7919 * i as u64,
                t0_scale,
            )
        })
        .collect();
    let mut best: Option<State> = None;
    let mut traces = Vec::new();
    for (s, t) in res {
        traces.push(t);
        let better = match &best {
            None => true,
            Some(b) => {
                let (tb, ts) = (pb.terms(b), pb.terms(&s));
                (ts.violations, ts.total) < (tb.violations, tb.total)
            }
        };
        if better {
            best = Some(s);
        }
    }
    (best.unwrap(), traces)
}
