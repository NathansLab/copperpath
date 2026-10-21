//! Detailed router for a region (e.g. one functional block): grid maze router (0.05 mm, 6 layers, 45°
//! moves, through vias) with rip-up and reroute.
//!
//! Legality is exact at cell centres: a track centre may sit in a cell when its half width plus the
//! DRU clearance (net pair, layer kind) to every foreign object is kept (`sites::track_map`), a via
//! when `sites::via_map` allows it (all layers, via-to-pad gap, hole-to-hole, rule areas, via-keepout
//! courtyards). Tracks use the DRU opt width; within `neck` mm of the net's own pads the DRU min
//! width is allowed (neck-down for fine-pitch pins). Fixed maps are computed once per net (in
//! parallel). Copper routed for other nets is stamped per net as owner maps: a hard obstacle in the
//! normal search, a cost in the rip-up search that finds the blocking nets.

use crate::board::*;
use crate::clean::{self, Checker, End, FItem, Run};
use crate::geom::*;
use crate::model::*;
use crate::sites::{track_map, via_map, R_FREE};
use rayon::prelude::*;
use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap, HashSet};

#[derive(Clone, Debug)]
pub struct DParams {
    pub res: f64,
    pub margin: f64,
    pub neck: f64,
    pub via_cost: f32,
    pub bend_cost: f32,
    pub wrong_dir: f32,
    pub narrow_cost: f32,
    pub window: f64,
    pub yield_same_class: bool,
    pub pres0: f32,
    pub pres_mul: f32,
    pub hist_inc: f32,
    pub nego_iters: usize,
    /// nets routed in parallel against the same snapshot even if their windows overlap
    /// (1 = only non-overlapping nets in parallel: result identical to serial routing)
    pub par_nets: usize,
    /// iterations without a new minimum of conflict cells before the negotiation stops
    pub stall_iters: usize,
    /// experimental: parallel connection searches inside a multi-pin net (0 = number of threads,
    /// 1 = serial; parallel was slower and not shorter on the public benchmark boards)
    pub par_conn: usize,
    /// per-net window: bbox of the net's pins + this margin (mm); maps and searches stay inside
    pub job_margin: f64,
    /// A* heuristic weight (1 = optimal, larger = greedier and faster)
    pub hweight: f32,
    /// node expansions per search before it gives up
    pub max_expand: u64,
    /// pad exit stubs (straight-exit rule): straight from the pad centre along a pad axis to half extent + stub_ext
    pub stubs: bool,
    pub stub_ext: f64,
    /// extra cost (cells) for a stub along the short axis of an elongated pad
    pub stub_side_cost: f32,
    /// cost per cell of routing through the net's own pads (pins with stubs)
    pub own_pad_cost: f32,
    /// extra cost (cells) of a short stub (straight-exit exception)
    pub short_stub_cost: f32,
    /// preferred exit stub length beyond the pad edge where there is room (falls back to stub_ext)
    pub stub_ext_long: f64,
    /// a 90 degree turn costs bend_cost * bend90 (45 degrees: bend_cost); turns > 90 are forbidden
    pub bend90: f32,
    /// exact post-route cleanup (stub alignment, octilinear pull-tight)
    pub clean: bool,
    /// cleanup may slide stub ends / switch stub axes / slide junctions (off in the fallback pass)
    pub slide: bool,
}

impl Default for DParams {
    fn default() -> Self {
        DParams {
            res: 0.05,
            margin: 0.005,
            neck: 1.5,
            via_cost: 30.0,
            bend_cost: 4.0,
            wrong_dir: 0.6,
            narrow_cost: 0.3,
            window: 4.0,
            yield_same_class: true,
            pres0: 2.0,
            pres_mul: 1.6,
            hist_inc: 3.0,
            nego_iters: 25,
            par_nets: 1,
            stall_iters: 3,
            par_conn: 1,
            job_margin: 6.0,
            hweight: 1.02,
            max_expand: 4_000_000,
            stubs: true,
            stub_ext: 0.6,
            stub_side_cost: 20.0,
            own_pad_cost: 40.0,
            short_stub_cost: 30.0,
            stub_ext_long: 0.6,
            slide: true,
            bend90: 1.4,
            clean: true,
        }
    }
}

/// a tiny T-junction leg of a routed net: (leg segment, neighbour segment, whether the
/// neighbour's `a` end is the moved one, new end point of the neighbour on the parent segment).
/// The leg is shorter than its width, one end lies inside another segment of the net (not at
/// its ends), the other end is shared with exactly one
/// neighbour (no via, no pin there), and the neighbour's line meets the parent within 0.2 mm.
fn tiny_leg(rn: &RNet, pins: &[crate::model::Pin]) -> Option<(usize, usize, bool, P)> {
    let segs = &rn.segs;
    let same = |p: P, q: P| (p - q).len() < 1e-6;
    for (i, s) in segs.iter().enumerate() {
        let len = (s.b - s.a).len();
        if len >= s.w || len < 1e-9 {
            continue;
        }
        for (j, q) in [(s.a, s.b), (s.b, s.a)] {
            let Some(par) = segs
                .iter()
                .enumerate()
                .find(|(pi, p)| {
                    *pi != i
                        && p.l == s.l
                        && dist_seg(j, p.a, p.b) < 1e-6
                        && !same(j, p.a)
                        && !same(j, p.b)
                })
                .map(|(_, p)| p)
            else {
                continue;
            };
            if rn.vias.iter().any(|v| same(v.at, q))
                || pins.iter().any(|pn| (pn.at - q).len() < 1e-3)
            {
                continue;
            }
            let nbrs: Vec<usize> = segs
                .iter()
                .enumerate()
                .filter(|(ni, n)| *ni != i && n.l == s.l && (same(n.a, q) || same(n.b, q)))
                .map(|(ni, _)| ni)
                .collect();
            if nbrs.len() != 1 {
                continue;
            }
            let n = &segs[nbrs[0]];
            let far = if same(n.a, q) { n.b } else { n.a };
            let Some(x) = crate::clean::intersect(far, q - far, par.a, par.b - par.a) else {
                continue;
            };
            if dist_seg(x, par.a, par.b) > 1e-6
                || (x - q).len() > 0.2
                || (x - far).dot(q - far) <= 0.0
                || !crate::clean::octi(x - far)
            {
                continue;
            }
            return Some((i, nbrs[0], same(n.a, q), x));
        }
    }
    None
}

#[derive(Clone, Debug)]
pub struct RSeg {
    pub a: P,
    pub b: P,
    pub l: usize,
    pub w: f64,
}

#[derive(Clone, Debug)]
pub struct RVia {
    pub at: P,
    pub d: f64,
    pub drill: f64,
}

#[derive(Clone, Debug, Default)]
pub struct RNet {
    pub segs: Vec<RSeg>,
    pub vias: Vec<RVia>,
}

/// result of routing one net: copper, routes made, stub ends (pin, stub, short), unreached components
/// a found connection: path cells (layer, cell) and the component it reached
type Found = (Vec<(usize, usize)>, usize);
type NetRouteResult = (RNet, usize, Vec<(usize, usize, bool)>, Vec<usize>);

pub struct NetJob {
    pub net: usize,
    pub pins: Vec<Pin>, // pins inside the region
    pub hw: f64,        // opt half width
    pub hn: f64,        // min half width (neck-down)
    pub vr: f64,
    pub dr: f64,
}

struct NetMaps {
    g: GridSpec, // window of this net (cells aligned with the router grid)
    ox: usize,   // window origin in router cells
    oy: usize,
    t: Vec<Vec<u8>>,  // wide track, per layer, 0 = free
    tn: Vec<Vec<u8>>, // narrow track
    v: Vec<u8>,
    neck: Vec<bool>,
    /// layer bits of the net's own pads (pins that have exit stubs) per window cell
    own: Vec<u8>,
}

/// pad exit stub: pad centre c -> e along the pad axis u, e projected from the grid cell g
#[derive(Clone, Debug)]
pub struct Stub {
    pub pin: usize,
    pub l: usize,
    pub d: u8,
    pub u: P,
    pub c: P,
    pub e: P,
    pub g: usize,
    pub wide: bool,
    pub cost: f32,
    pub min_len: f64,
    pub cells: Vec<usize>,
    /// shorter than the exit length (the full stub is blocked, e.g. 0.84 mm gap between header
    /// pads): an straight-exit exception, used only when it beats the detour
    pub short: bool,
    /// exit along the short axis of an elongated pad (only kept when no long-axis stub is legal)
    pub side: bool,
}

impl NetMaps {
    /// window index of a router cell index
    #[inline]
    fn li(&self, nxg: usize, i: usize) -> Option<usize> {
        let (x, y) = (i % nxg, i / nxg);
        if x < self.ox || y < self.oy || x >= self.ox + self.g.nx || y >= self.oy + self.g.ny {
            return None;
        }
        Some((y - self.oy) * self.g.nx + (x - self.ox))
    }
}

type Nego<'b> = Option<(f32, &'b [Vec<f32>])>;

/// owner maps in the window of the net they were built for
struct Owners {
    t: Vec<Vec<u16>>,
    tn: Vec<Vec<u16>>,
    v: Vec<u16>,
}

pub struct DResult {
    pub routed: HashMap<usize, RNet>,
    pub failed: Vec<(usize, usize)>, // (net, unconnected components left)
    /// why the last search of each failed net gave up (same order as `failed`)
    pub why: Vec<&'static str>,
    pub routes: usize,
    pub rips: usize,
    pub conns: usize,
    pub expanded: u64,
    pub stats: AesStats,
}

#[derive(Default, Debug, Clone)]
pub struct AesStats {
    pub pins: usize,
    pub pins_no_stub: Vec<String>,
    pub stubs_used: usize,
    pub short_stubs: Vec<String>,
    pub fallback_ends: usize,
    pub aligned: usize,
    pub align_failed: usize,
    pub pulls: usize,
    pub mitered: usize,
    pub miter_failed: usize,
    pub shifted: usize,
    pub jogs: usize,
    pub kept90: [usize; 5],
    pub exact_fail: usize,
}

const DIRS: [(i32, i32); 8] = [
    (1, 0),
    (1, 1),
    (0, 1),
    (-1, 1),
    (-1, 0),
    (-1, -1),
    (0, -1),
    (1, -1),
];

pub struct Router<'a> {
    pub m: &'a Model<'a>,
    pub gs: GridSpec,
    pub p: DParams,
    pub objs: Vec<Obj>,
    pub jobs: Vec<NetJob>,
    maps: Vec<NetMaps>,
    stubs: Vec<Vec<Stub>>,
    maxd: f64,
    expanded: std::sync::atomic::AtomicU64,
    /// per job: reason of the last failed search (0 none, 1 node cap, 2 job window, 3 no path)
    why: Vec<std::sync::atomic::AtomicU8>,
}

const OFF: [(f64, f64); 4] = [(1.0, 0.0), (0.0, 1.0), (-1.0, 0.0), (0.0, -1.0)];

#[inline]
fn tdir(a: u8, b: u8) -> u8 {
    let d = (a as i32 - b as i32).rem_euclid(8) as u8;
    d.min(8 - d)
}

impl<'a> Router<'a> {
    pub fn new(
        m: &'a Model<'a>,
        region: BBox,
        p: DParams,
        only_nets: Option<&HashSet<usize>>,
    ) -> Router<'a> {
        let gs = GridSpec::new(region, p.res);
        let gb = gs.bbox();
        let mut maxd: f64 = 0.0;
        for v in &m.r.mat[0] {
            maxd = maxd.max(*v as f64);
        }
        let objs: Vec<Obj> = m
            .objs
            .iter()
            .filter(|o| o.bbox.overlaps(&gb.inflate(maxd + 2.0)))
            .cloned()
            .collect();
        // connections: pins inside the region, nets with >= 2 copper components there
        let pins = pins_by_net(m);
        let mut jobs = Vec::new();
        let mut keys: Vec<&usize> = pins.keys().collect();
        keys.sort();
        for &net in keys {
            if let Some(f) = only_nets {
                if !f.contains(&net) {
                    continue;
                }
            }
            let ps: Vec<Pin> = pins[&net]
                .iter()
                .filter(|pn| region.contains(pn.at))
                .cloned()
                .collect();
            let mut comps: Vec<usize> = ps.iter().map(|pn| pn.comp).collect();
            comps.sort();
            comps.dedup();
            if comps.len() < 2 {
                continue;
            }
            let (wmin, w) = m.r.track_width(net);
            let (vd, vdr) = m.r.via(net);
            jobs.push(NetJob {
                net,
                pins: ps,
                hw: w / 2.0,
                hn: wmin.min(w) / 2.0,
                vr: vd / 2.0,
                dr: vdr / 2.0,
            });
        }
        let t = std::time::Instant::now();
        let jm = p.job_margin;
        let ck = Checker::new(m, &objs, maxd);
        let ms: Vec<(NetMaps, Vec<Stub>)> = jobs
            .par_iter()
            .map(|j| {
                let mut bb = BBox::empty();
                for pn in &j.pins {
                    bb.add(pn.at);
                }
                let bb = bb.inflate(jm);
                let ox = (((bb.x0 - gs.x0) / gs.res).floor().max(0.0) as usize).min(gs.nx - 1);
                let oy = (((bb.y0 - gs.y0) / gs.res).floor().max(0.0) as usize).min(gs.ny - 1);
                let ex = (((bb.x1 - gs.x0) / gs.res).ceil().max(0.0) as usize).clamp(ox + 1, gs.nx);
                let ey = (((bb.y1 - gs.y0) / gs.res).ceil().max(0.0) as usize).clamp(oy + 1, gs.ny);
                let g = GridSpec {
                    x0: gs.x0 + ox as f64 * gs.res,
                    y0: gs.y0 + oy as f64 * gs.res,
                    res: gs.res,
                    nx: ex - ox,
                    ny: ey - oy,
                };
                let wb = g.bbox().inflate(maxd + 2.0);
                // locked pours of the net's own class (e.g. a guard pour of the same class) refill
                // around this net's copper with their own clearance -> not an obstacle
                let cls = m.r.net_class[j.net];
                let objs: Vec<Obj> = objs
                    .iter()
                    .filter(|o| {
                        o.bbox.overlaps(&wb)
                            && !(p.yield_same_class
                                && matches!(o.kind, Kind::Zone)
                                && o.net != 0 // net-less copper never refills
                                && m.r.net_class[o.net] == cls)
                    })
                    .cloned()
                    .collect();
                let objs = &objs;
                let t: Vec<Vec<u8>> = (0..NL)
                    .map(|l| track_map(m, &g, j.net, j.hw + p.margin, l, objs))
                    .collect();
                let tn: Vec<Vec<u8>> = (0..NL)
                    .map(|l| track_map(m, &g, j.net, j.hn + p.margin, l, objs))
                    .collect();
                let v = via_map(m, &g, j.net, j.vr + p.margin, j.dr, objs);
                let mut neck = vec![false; g.n()];
                for pn in &j.pins {
                    let f = &m.b.fps[pn.fp];
                    for s in f.pad_shapes(&f.pads[pn.pad]) {
                        g.stamp(&s, p.neck, |x, y| neck[g.idx(x, y)] = true);
                    }
                }
                let mut mp = NetMaps {
                    g,
                    ox,
                    oy,
                    t,
                    tn,
                    v,
                    neck,
                    own: vec![0u8; g.n()],
                };
                let stubs = if p.stubs {
                    make_stubs(m, &ck, &gs, j, &mp, &p)
                } else {
                    Vec::new()
                };
                for (pi, pn) in j.pins.iter().enumerate() {
                    if !stubs.iter().any(|s| s.pin == pi) {
                        continue;
                    }
                    let f = &m.b.fps[pn.fp];
                    let pd = &f.pads[pn.pad];
                    for s in f.pad_shapes(pd) {
                        let g = mp.g;
                        let own = &mut mp.own;
                        g.stamp(&s, 0.0, |x, y| own[g.idx(x, y)] |= pd.layers);
                    }
                }
                (mp, stubs)
            })
            .collect();
        let (maps, stubs): (Vec<NetMaps>, Vec<Vec<Stub>>) = ms.into_iter().unzip();
        eprintln!(
            "[droute] grid {}x{} @ {} mm, {} objects, {} nets to route, window cells {:.1} M, fixed maps in {:.2} s",
            gs.nx,
            gs.ny,
            p.res,
            objs.len(),
            jobs.len(),
            maps.iter().map(|m| m.g.n()).sum::<usize>() as f64 / 1e6,
            t.elapsed().as_secs_f64()
        );
        if std::env::var("DROUTE_DEBUG").is_ok() {
            for (j, mp) in jobs.iter().zip(&maps) {
                let fv = mp.v.iter().filter(|v| **v == R_FREE).count();
                let ft: Vec<usize> = (0..NL)
                    .map(|l| mp.t[l].iter().filter(|v| **v == R_FREE).count())
                    .collect();
                let mut hist = [0usize; 16];
                for v in &mp.v {
                    hist[(*v & 15) as usize] += 1;
                }
                eprintln!("[dbg] {} hw {:.2} hn {:.2} vr {:.2}: via sites {} (blockers {:?}), free track cells per layer {:?}", m.b.nets[j.net], j.hw, j.hn, j.vr, fv, hist, ft);
            }
        }
        let nj_why = jobs.len();
        Router {
            m,
            gs,
            p,
            objs,
            jobs,
            maps,
            stubs,
            maxd,
            expanded: Default::default(),
            why: (0..nj_why).map(|_| Default::default()).collect(),
        }
    }

    fn stamp_owner(g: &GridSpec, map: &mut [u16], last: &mut [u16], sh: &Shape, d: f64, id: u16) {
        g.stamp(sh, d, |x, y| {
            let i = g.idx(x, y);
            if last[i] != id {
                last[i] = id;
                map[i] += 1;
            }
        });
    }

    /// occupancy maps: number of other nets whose routed copper makes a cell illegal for job k
    fn owner_maps(&self, k: usize, routed: &[RNet]) -> Owners {
        let g = &self.maps[k].g;
        let n = g.n();
        let wb = g.bbox().inflate(3.0);
        let job = &self.jobs[k];
        let mut o = Owners {
            t: vec![vec![0u16; n]; NL],
            tn: vec![vec![0u16; n]; NL],
            v: vec![0u16; n],
        };
        let mut lt = vec![vec![0u16; n]; NL];
        let mut ltn = vec![vec![0u16; n]; NL];
        let mut lv = vec![0u16; n];
        let mg = self.p.margin;
        for (j, rn) in routed.iter().enumerate() {
            if j == k {
                continue;
            }
            let oj = &self.jobs[j];
            let id = (j + 1) as u16;
            for s in &rn.segs {
                let sh = Shape::Capsule {
                    a: s.a,
                    b: s.b,
                    r: s.w / 2.0,
                };
                if !sh.bbox().overlaps(&wb) {
                    continue;
                }
                let c = self.m.r.clr(job.net, oj.net, is_outer(s.l)).max(0.0);
                Self::stamp_owner(g, &mut o.t[s.l], &mut lt[s.l], &sh, c + job.hw + mg, id);
                Self::stamp_owner(g, &mut o.tn[s.l], &mut ltn[s.l], &sh, c + job.hn + mg, id);
                // a new via also keeps the hole clearance from this track
                let cv = (c + job.vr).max(job.dr + self.m.r.min_hole_clearance);
                Self::stamp_owner(g, &mut o.v, &mut lv, &sh, cv + mg, id);
            }
            for v in &rn.vias {
                let sh = Shape::circle(v.at, v.d / 2.0);
                if !sh.bbox().overlaps(&wb) {
                    continue;
                }
                let mut cm: f64 = 0.0;
                // tracks keep the hole clearance from the via's drill (can exceed the copper clearance)
                let ch = v.drill / 2.0 + self.m.r.min_hole_clearance - v.d / 2.0;
                for l in 0..NL {
                    let c = self.m.r.clr(job.net, oj.net, is_outer(l)).max(0.0).max(ch);
                    cm = cm.max(c);
                    Self::stamp_owner(g, &mut o.t[l], &mut lt[l], &sh, c + job.hw + mg, id);
                    Self::stamp_owner(g, &mut o.tn[l], &mut ltn[l], &sh, c + job.hn + mg, id);
                }
                let hh = self.m.r.min_hole_to_hole + v.drill / 2.0 + job.dr;
                Self::stamp_owner(
                    g,
                    &mut o.v,
                    &mut lv,
                    &Shape::circle(v.at, 0.0),
                    (cm + v.d / 2.0 + job.vr + mg).max(hh),
                    id,
                );
            }
        }
        o
    }

    /// cells covered by the pad copper of a pin (cell centres inside the pad) plus its centre cell
    fn pin_cells(&self, pin: &Pin) -> Vec<(usize, usize)> {
        let f = &self.m.b.fps[pin.fp];
        let pd = &f.pads[pin.pad];
        let mut out = Vec::new();
        for s in f.pad_shapes(pd) {
            for l in 0..NL {
                if pd.layers & (1 << l) != 0 {
                    self.gs
                        .stamp(&s, 0.0, |x, y| out.push((l, self.gs.idx(x, y))));
                }
            }
        }
        if let Some((x, y)) = self.gs.cell_of(pin.at) {
            for l in 0..NL {
                if pd.layers & (1 << l) != 0 {
                    out.push((l, self.gs.idx(x, y)));
                }
            }
        }
        out.sort_unstable();
        out.dedup();
        out
    }

    #[inline]
    fn wi(&self, k: usize, i: usize) -> Option<usize> {
        self.maps[k].li(self.gs.nx, i)
    }
    #[inline]
    fn wide_ok(&self, k: usize, o: &Owners, l: usize, i: usize) -> bool {
        let Some(w) = self.wi(k, i) else { return false };
        self.maps[k].t[l][w] == R_FREE && o.t[l][w] == 0
    }
    #[inline]
    fn narrow_ok(&self, k: usize, o: &Owners, l: usize, i: usize) -> bool {
        let Some(w) = self.wi(k, i) else { return false };
        let mp = &self.maps[k];
        mp.neck[w] && mp.tn[l][w] == R_FREE && o.tn[l][w] == 0
    }
    /// fixed-only legality (rip-up search ignores routed copper)
    #[inline]
    fn fixed_ok(&self, k: usize, l: usize, i: usize) -> bool {
        let Some(w) = self.wi(k, i) else { return false };
        let mp = &self.maps[k];
        mp.t[l][w] == R_FREE || (mp.neck[w] && mp.tn[l][w] == R_FREE)
    }
    /// conflict of a routed cell (window of job k) against owner maps
    #[inline]
    fn owned(&self, k: usize, o: &Owners, l: usize, i: usize, wide: bool) -> bool {
        let Some(w) = self.wi(k, i) else { return false };
        if l >= NL {
            return o.v[w] > 0;
        }
        if wide {
            o.t[l][w] > 0
        } else {
            o.tn[l][w] > 0
        }
    }

    /// negotiated congestion on the detailed grid (PathFinder): every net is routed with foreign
    /// copper as a growing cost (present factor x occupancy + history); nets with conflicts are
    /// rerouted until none are left. Remaining conflicts: the net with the most conflict cells is
    /// ripped and rerouted with routed copper as a hard obstacle.
    /// up to `m` open components closest to the tree (centre of the component to the nearest
    /// sampled tree cell, Manhattan)
    fn nearest_comps(
        &self,
        k: usize,
        tree: &[(usize, usize)],
        open: &[usize],
        comp_cells: &[Vec<(usize, usize)>],
        m: usize,
    ) -> Vec<usize> {
        let _ = k;
        let nx = self.gs.nx as i64;
        let step = (tree.len() / 2000).max(1);
        let mut d: Vec<(i64, usize)> = open
            .iter()
            .map(|&c| {
                let cc = &comp_cells[c];
                let (sx, sy) = cc.iter().fold((0i64, 0i64), |a, x| {
                    (a.0 + x.1 as i64 % nx, a.1 + x.1 as i64 / nx)
                });
                let (cx, cy) = (sx / cc.len() as i64, sy / cc.len() as i64);
                let dm = tree
                    .iter()
                    .step_by(step)
                    .map(|t| (t.1 as i64 % nx - cx).abs() + (t.1 as i64 / nx - cy).abs())
                    .min()
                    .unwrap_or(0);
                (dm, c)
            })
            .collect();
        d.sort();
        d.into_iter().take(m).map(|x| x.1).collect()
    }

    /// list scheduling of `need` into waves: a net goes one wave after the latest earlier net whose
    /// window (plus the owner-map reach) overlaps its own
    fn waves(&self, need: &[usize]) -> Vec<Vec<usize>> {
        let bb: Vec<BBox> = need
            .iter()
            .map(|&k| self.maps[k].g.bbox().inflate(3.0 + self.p.margin + 0.5))
            .collect();
        let mut wv = vec![0usize; need.len()];
        let mut out: Vec<Vec<usize>> = Vec::new();
        let relax = self.p.par_nets.max(1);
        if relax > 1 {
            // relaxed: blocks of `relax` nets in order, each block against the same snapshot
            // (stale view of each other; the negotiation removes the resulting conflicts)
            return need.chunks(relax).map(|c| c.to_vec()).collect();
        }
        for i in 0..need.len() {
            let w = (0..i)
                .filter(|&j| bb[j].overlaps(&self.maps[need[i]].g.bbox()))
                .map(|j| wv[j] + 1)
                .max()
                .unwrap_or(0);
            wv[i] = w;
            if out.len() <= w {
                out.resize(w + 1, Vec::new());
            }
            out[w].push(need[i]);
        }
        out
    }

    pub fn run(&self) -> DResult {
        let nj = self.jobs.len();
        let n = self.gs.n();
        let mut routed: Vec<RNet> = vec![RNet::default(); nj];
        let mut cells: Vec<Vec<(usize, usize, bool)>> = vec![Vec::new(); nj];
        let mut vias: Vec<Vec<usize>> = vec![Vec::new(); nj];
        let mut left = vec![0usize; nj];
        let mut hist: Vec<Vec<f32>> = vec![vec![0.0; n]; NL];
        let hp = |j: &NetJob| {
            let mut b = BBox::empty();
            for pn in &j.pins {
                b.add(pn.at);
            }
            (b.x1 - b.x0) + (b.y1 - b.y0)
        };
        let mut order: Vec<usize> = (0..nj).collect();
        order.sort_by(|a, b| hp(&self.jobs[*a]).partial_cmp(&hp(&self.jobs[*b])).unwrap());
        let mut need: Vec<usize> = order.clone();
        let mut pres = self.p.pres0;
        let mut routes = 0;
        let mut it = 0;
        let dbg = std::env::var("DROUTE_DEBUG").is_ok();
        let mut best_hist: Vec<usize> = Vec::new();
        let ck = Checker::new(self.m, &self.objs, self.maxd);
        let mut nst: Vec<AesStats> = vec![AesStats::default(); nj];
        loop {
            let t_it = std::time::Instant::now();
            // nets whose windows cannot see each other's copper route in parallel; a net waits for
            // every earlier net (in `need` order) it can see, so the result equals the serial order
            let waves = self.waves(&need);
            for wave in &waves {
                let hist_ref = &hist;
                let routed_ref = &routed;
                let rs: Vec<(usize, NetRouteResult, AesStats, f64)> = wave
                    .par_iter()
                    .map(|&k| {
                        let t_net = std::time::Instant::now();
                        let o = self.owner_maps(k, routed_ref);
                        let fr = self.foreign(k, routed_ref);
                        let mut st = AesStats::default();
                        let r = self.route_net(k, &o, Some((pres, hist_ref)), &ck, &fr, &mut st);
                        (k, r, st, t_net.elapsed().as_secs_f64())
                    })
                    .collect();
                for (k, r, st, dt) in rs {
                    if dt > 2.0 {
                        let mp = &self.maps[k];
                        eprintln!(
                            "[droute]   slow net {}: {:.1} s, {} pins, window {}x{}, wave of {}, left {}",
                            self.m.b.nets[self.jobs[k].net],
                            dt,
                            self.jobs[k].pins.len(),
                            mp.g.nx,
                            mp.g.ny,
                            wave.len(),
                            r.1
                        );
                    }
                    nst[k] = st;
                    routed[k] = r.0;
                    left[k] = r.1;
                    cells[k] = r.2;
                    vias[k] = r.3;
                    routes += 1;
                }
            }
            if dbg {
                eprintln!(
                    "[dbg] {} nets in {} waves (largest {})",
                    need.len(),
                    waves.len(),
                    waves.iter().map(|w| w.len()).max().unwrap_or(0)
                );
            }
            // conflicts of every net against all others
            let conf: Vec<Vec<(usize, usize)>> = (0..nj)
                .into_par_iter()
                .map(|k| {
                    let o = self.owner_maps(k, &routed);
                    let mut c: Vec<(usize, usize)> = cells[k]
                        .iter()
                        .filter(|(l, i, w)| self.owned(k, &o, *l, *i, *w))
                        .map(|(l, i, _)| (*l, *i))
                        .collect();
                    for &i in &vias[k] {
                        if self.owned(k, &o, NL, i, false) {
                            c.push((NL, i));
                        }
                    }
                    c
                })
                .collect();
            let total: usize = conf.iter().map(|c| c.len()).sum();
            eprintln!(
                "[droute] iter {it}: {} nets in {:.1} s, conflict cells {total}, nets in conflict {}, unrouted comps {}",
                need.len(),
                t_it.elapsed().as_secs_f64(),
                conf.iter().filter(|c| !c.is_empty()).count(),
                left.iter().sum::<usize>()
            );
            if dbg {
                eprintln!("[dbg] nego iter {it}: pres {pres:.2}, {} nets rerouted, conflict cells {total}, nets in conflict {}", need.len(), conf.iter().filter(|c| !c.is_empty()).count());
            }
            best_hist.push(total);
            // no new best for `stall_iters` iterations: stop negotiating, rip and reroute hard
            let sw = self.p.stall_iters.max(1);
            let stalled = best_hist.len() > sw
                && best_hist[best_hist.len() - sw..].iter().min().unwrap()
                    >= best_hist[..best_hist.len() - sw].iter().min().unwrap();
            if total == 0 || it + 1 >= self.p.nego_iters || stalled {
                if total > 0 && dbg {
                    for (k, ck) in conf.iter().enumerate() {
                        if ck.is_empty() {
                            continue;
                        }
                        let pts: Vec<String> = ck
                            .iter()
                            .step_by((ck.len() / 4).max(1))
                            .take(4)
                            .map(|(l, i)| {
                                let c = self.gs.center(i % self.gs.nx, i / self.gs.nx);
                                format!(
                                    "{}@({:.2},{:.2})",
                                    if *l < NL { CU[*l] } else { "via" },
                                    c.x,
                                    c.y
                                )
                            })
                            .collect();
                        eprintln!(
                            "[dbg]   conflicts {} x{}: {:?}",
                            self.m.b.nets[self.jobs[k].net],
                            ck.len(),
                            pts
                        );
                    }
                }
                if total > 0 {
                    // resolve: rip the most conflicted nets until clean, reroute them hard
                    let mut ripped: Vec<usize> = Vec::new();
                    let mut conf = conf;
                    while let Some(k) = (0..nj)
                        .filter(|k| !conf[*k].is_empty())
                        .max_by_key(|k| conf[*k].len())
                    {
                        routed[k] = RNet::default();
                        cells[k].clear();
                        vias[k].clear();
                        ripped.push(k);
                        conf = (0..nj)
                            .into_par_iter()
                            .map(|j| {
                                if cells[j].is_empty() && vias[j].is_empty() {
                                    return vec![];
                                }
                                let o = self.owner_maps(j, &routed);
                                let mut c: Vec<(usize, usize)> = cells[j]
                                    .iter()
                                    .filter(|(l, i, w)| self.owned(j, &o, *l, *i, *w))
                                    .map(|(l, i, _)| (*l, *i))
                                    .collect();
                                for &i in &vias[j] {
                                    if self.owned(j, &o, NL, i, false) {
                                        c.push((NL, i));
                                    }
                                }
                                c
                            })
                            .collect();
                    }
                    for &k in &ripped {
                        let o = self.owner_maps(k, &routed);
                        let fr = self.foreign(k, &routed);
                        nst[k] = AesStats::default();
                        let r = self.route_net(k, &o, None, &ck, &fr, &mut nst[k]);
                        routed[k] = r.0;
                        left[k] = r.1;
                        cells[k] = r.2;
                        vias[k] = r.3;
                        routes += 1;
                    }
                    if dbg {
                        eprintln!(
                            "[dbg] ripped and rerouted hard: {:?}",
                            ripped
                                .iter()
                                .map(|k| (self.m.b.nets[self.jobs[*k].net].as_str(), left[*k]))
                                .collect::<Vec<_>>()
                        );
                    }
                }
                break;
            }
            for c in &conf {
                for &(l, i) in c {
                    if l < NL {
                        hist[l][i] += self.p.hist_inc;
                    }
                }
            }
            need = order
                .iter()
                .cloned()
                .filter(|k| !conf[*k].is_empty())
                .collect();
            pres *= self.p.pres_mul;
            it += 1;
        }
        let failed: Vec<(usize, usize)> = (0..nj)
            .filter(|k| left[*k] > 0)
            .map(|k| (self.jobs[k].net, left[k]))
            .collect();
        let why = (0..nj)
            .filter(|k| left[*k] > 0)
            .map(
                |k| match self.why[k].load(std::sync::atomic::Ordering::Relaxed) {
                    1 => "node cap hit (raise --max-expand)",
                    2 => "job window exhausted (raise --job-margin)",
                    3 => "no path (target unreachable)",
                    _ => "no legal start or target cell",
                },
            )
            .collect();
        let conns = self
            .jobs
            .iter()
            .map(|j| {
                let mut c: Vec<usize> = j.pins.iter().map(|p| p.comp).collect();
                c.sort();
                c.dedup();
                c.len() - 1
            })
            .sum();
        let mut stats = AesStats::default();
        for x in &nst {
            stats.pins += x.pins;
            stats.pins_no_stub.extend(x.pins_no_stub.iter().cloned());
            stats.stubs_used += x.stubs_used;
            stats.short_stubs.extend(x.short_stubs.iter().cloned());
            stats.fallback_ends += x.fallback_ends;
            stats.aligned += x.aligned;
            stats.align_failed += x.align_failed;
            stats.pulls += x.pulls;
            stats.mitered += x.mitered;
            stats.miter_failed += x.miter_failed;
            stats.shifted += x.shifted;
            stats.jogs += x.jogs;
            for q in 0..5 {
                stats.kept90[q] += x.kept90[q];
            }
        }
        // final octilinear polish against the final copper of all other nets (the cleanup ran
        // against the copper of the negotiation round): a segment only slightly off an octilinear
        // direction (grid rounding at off-grid pad centres) gets a tiny perpendicular jog
        if self.p.clean {
            for k in 0..nj {
                let fr = self.foreign(k, &routed);
                let job = &self.jobs[k];
                let mut i = 0;
                while i < routed[k].segs.len() {
                    let sg = routed[k].segs[i].clone();
                    let at_pin = |q: P| job.pins.iter().any(|pn| (pn.at - q).len() < 1e-3);
                    let hw = sg.w / 2.0 + self.p.margin;
                    let ok = |a: P, b: P| {
                        ck.seg_ok_fixed(job.net, a, b, hw, sg.l)
                            && ck.seg_ok_foreign(job.net, a, b, hw, sg.l, &fr)
                    };
                    if let Some((e_a, e_b)) = clean::octi_jog(sg.a, sg.b) {
                        // keep the straight part at a pad centre (the exit), jog at the other end
                        let order = if at_pin(sg.b) && !at_pin(sg.a) {
                            [e_b, e_a]
                        } else {
                            [e_a, e_b]
                        };
                        if let Some(e) = order.into_iter().find(|e| ok(sg.a, *e) && ok(*e, sg.b)) {
                            routed[k].segs[i] = RSeg {
                                a: sg.a,
                                b: e,
                                ..sg.clone()
                            };
                            routed[k].segs.insert(
                                i + 1,
                                RSeg {
                                    a: e,
                                    b: sg.b,
                                    ..sg
                                },
                            );
                            stats.jogs += 1;
                            i += 1;
                        }
                    }
                    i += 1;
                }
            }
        }
        // tiny junction legs: a segment shorter than its width that hangs off the middle of another
        // segment of the net (T-junction) is dropped and its neighbour extended onto the parent
        if self.p.clean {
            for k in 0..nj {
                let fr = self.foreign(k, &routed);
                let job = &self.jobs[k];
                while let Some((i, n, a_end, x)) = tiny_leg(&routed[k], &job.pins) {
                    let sg = routed[k].segs[n].clone();
                    let hw = sg.w / 2.0 + self.p.margin;
                    let (na, nb) = if a_end { (x, sg.b) } else { (sg.a, x) };
                    if !(ck.seg_ok_fixed(job.net, na, nb, hw, sg.l)
                        && ck.seg_ok_foreign(job.net, na, nb, hw, sg.l, &fr))
                    {
                        break;
                    }
                    routed[k].segs[n] = RSeg { a: na, b: nb, ..sg };
                    routed[k].segs.remove(i);
                    stats.jogs += 1;
                }
            }
        }
        // exact verification of all routed copper (fixed objects + other nets)
        let fails: Vec<String> = (0..nj)
            .into_par_iter()
            .flat_map(|k| {
                let fr = self.foreign(k, &routed);
                let job = &self.jobs[k];
                let mut out = Vec::new();
                for sg in &routed[k].segs {
                    let hw = sg.w / 2.0;
                    if !ck.seg_ok_fixed(job.net, sg.a, sg.b, hw, sg.l)
                        || !ck.seg_ok_foreign(job.net, sg.a, sg.b, hw, sg.l, &fr)
                    {
                        out.push(format!(
                            "{} {} ({:.3},{:.3})-({:.3},{:.3}) w {}",
                            self.m.b.nets[job.net], CU[sg.l], sg.a.x, sg.a.y, sg.b.x, sg.b.y, sg.w
                        ));
                    }
                }
                out
            })
            .collect();
        stats.exact_fail = fails.len();
        if dbg {
            for f in &fails {
                eprintln!("[dbg] exact check fails: {f}");
            }
        }
        DResult {
            stats,
            routed: (0..nj)
                .map(|k| (self.jobs[k].net, routed[k].clone()))
                .collect(),
            failed,
            why,
            routes,
            rips: it,
            conns,
            expanded: self.expanded.load(std::sync::atomic::Ordering::Relaxed),
        }
    }

    /// routed copper of the other nets near job k's window (exact shapes for the cleanup checks)
    fn foreign(&self, k: usize, routed: &[RNet]) -> Vec<FItem> {
        let wb = self.maps[k].g.bbox().inflate(3.0);
        let mut out = Vec::new();
        for (j, rn) in routed.iter().enumerate() {
            if j == k {
                continue;
            }
            let net = self.jobs[j].net;
            for s in &rn.segs {
                let sh = Shape::Capsule {
                    a: s.a,
                    b: s.b,
                    r: s.w / 2.0,
                };
                let bb = sh.bbox();
                if bb.overlaps(&wb) {
                    out.push(FItem {
                        sh,
                        bb,
                        layers: 1 << s.l,
                        net,
                        drill_r: 0.0,
                    });
                }
            }
            for v in &rn.vias {
                let sh = Shape::circle(v.at, v.d / 2.0);
                let bb = sh.bbox();
                if bb.overlaps(&wb) {
                    out.push(FItem {
                        sh,
                        bb,
                        layers: 0x3f,
                        net,
                        drill_r: v.drill / 2.0,
                    });
                }
            }
        }
        out
    }

    /// grow a tree over the pin components of job k; returns copper, unconnected components left,
    /// the cells of the copper (for conflict detection) and via cells.
    /// Pins are reached through their exit stubs (straight from the pad centre along a pad axis);
    /// pins without any legal stub fall back to their pad cells (counted as straight-exit exceptions).
    fn route_net(
        &self,
        k: usize,
        o: &Owners,
        nego: Nego,
        ck: &Checker,
        foreign: &[FItem],
        st: &mut AesStats,
    ) -> NetRouteResult {
        let job = &self.jobs[k];
        let mp = &self.maps[k];
        let n = mp.g.n();
        let soft = nego.is_some();
        let mut comp_ids: Vec<usize> = job.pins.iter().map(|p| p.comp).collect();
        comp_ids.sort();
        comp_ids.dedup();
        let nc = comp_ids.len();
        let stubs = &self.stubs[k];
        // usable stubs and their start cost
        let mut s_cost = vec![f32::INFINITY; stubs.len()];
        for (si, s) in stubs.iter().enumerate() {
            let mut c = s.cost;
            let mut ok = true;
            for &ci in &s.cells {
                let Some(w) = self.wi(k, ci) else { continue };
                let own = if s.wide { o.t[s.l][w] } else { o.tn[s.l][w] };
                match nego {
                    Some((pres, hist)) => c += pres * own as f32 + hist[s.l][ci],
                    None => {
                        if own > 0 {
                            ok = false;
                        }
                    }
                }
            }
            if !soft && !self.wide_ok(k, o, s.l, s.g) && !self.narrow_ok(k, o, s.l, s.g) {
                ok = false;
            }
            if ok {
                s_cost[si] = c;
            }
        }
        let mut comp_cells: Vec<Vec<(usize, usize)>> = vec![Vec::new(); nc];
        let mut pad_of: HashMap<(usize, usize), P> = HashMap::new();
        let mut node_stub: HashMap<(usize, usize), usize> = HashMap::new();
        let ok = |l: usize, i: usize| {
            if soft {
                self.fixed_ok(k, l, i)
            } else {
                self.wide_ok(k, o, l, i) || self.narrow_ok(k, o, l, i)
            }
        };
        st.pins += job.pins.len();
        for (pi, pn) in job.pins.iter().enumerate() {
            let ci = comp_ids.binary_search(&pn.comp).unwrap();
            let mut any = false;
            for (si, s) in stubs.iter().enumerate() {
                if s.pin == pi && s_cost[si].is_finite() {
                    any = true;
                    if let std::collections::hash_map::Entry::Vacant(e) =
                        node_stub.entry((s.l, s.g))
                    {
                        e.insert(si);
                        comp_cells[ci].push((s.l, s.g));
                    }
                }
            }
            if !any {
                if !stubs.iter().any(|s| s.pin == pi) {
                    st.pins_no_stub.push(format!(
                        "{}.{}",
                        self.m.b.fps[pn.fp].refd, self.m.b.fps[pn.fp].pads[pn.pad].num
                    ));
                }
                for c in self.pin_cells(pn) {
                    if ok(c.0, c.1) {
                        comp_cells[ci].push(c);
                        pad_of.insert(c, pn.at);
                    }
                }
            }
        }
        let info = |c: &(usize, usize), used: &[Option<P>]| -> (f32, u8) {
            match node_stub.get(c) {
                Some(&si) => (
                    if used[si].is_some() { 0.0 } else { s_cost[si] },
                    stubs[si].d,
                ),
                None => (0.0, 8),
            }
        };
        let mut used: Vec<Option<P>> = vec![None; stubs.len()];
        let mut tmark = vec![0u16; n * NL];
        let mut tree_mark = vec![false; n * NL];
        let mut tinfo: HashMap<usize, (f32, u8)> = HashMap::new();
        let start = (0..nc).max_by_key(|c| comp_cells[*c].len()).unwrap_or(0);
        let mut in_tree = vec![false; nc];
        in_tree[start] = true;
        let mut tree: Vec<(usize, usize)> = comp_cells[start].clone();
        let mut src_info: HashMap<(usize, usize), (f32, u8)> = HashMap::new();
        for c in &tree {
            tree_mark[c.0 * n + self.wi(k, c.1).unwrap()] = true;
            src_info.insert(*c, info(c, &used));
        }
        for (c, ccs) in comp_cells.iter().enumerate().take(nc) {
            if c != start {
                for cc in ccs {
                    let node = cc.0 * n + self.wi(k, cc.1).unwrap();
                    tmark[node] = (c + 1) as u16;
                    if let Some(&si) = node_stub.get(cc) {
                        tinfo.insert(node, (s_cost[si], (stubs[si].d + 4) % 8));
                    }
                }
            }
        }
        let mut rn = RNet::default();
        let mut cells = Vec::new();
        let mut vias = Vec::new();
        let mut left = nc - 1;
        let par = if self.p.par_conn == 0 {
            rayon::current_num_threads()
        } else {
            self.p.par_conn
        };
        let mut dead = vec![false; nc];
        let mut found: Vec<Found> = Vec::new();
        loop {
            let open: Vec<usize> = (0..nc)
                .filter(|c| !in_tree[*c] && !dead[*c] && !comp_cells[*c].is_empty())
                .collect();
            if open.is_empty() && found.is_empty() {
                break;
            }
            if found.is_empty() {
                if par > 1 && open.len() >= 3 {
                    // several searches from the same tree in parallel, each guided to one of the
                    // components nearest to the tree; accepted in order below (a search may end
                    // at another component than its guide, duplicates are dropped)
                    let near = self.nearest_comps(k, &tree, &open, &comp_cells, par);
                    let rs: Vec<Option<Found>> = near
                        .par_iter()
                        .map(|&c| {
                            self.astar(
                                k,
                                &tree,
                                &src_info,
                                &tmark,
                                &tinfo,
                                &tree_mark,
                                &[c],
                                &comp_cells,
                                o,
                                nego,
                            )
                        })
                        .collect();
                    // keep a result only if it stays clear of the results kept before it (else the
                    // two would likely share a corridor: redundant copper); dropped ones re-search
                    let nxg = self.gs.nx;
                    let gap = (2.0 / self.gs.res) as usize;
                    let mut kept: Vec<(usize, usize, usize, usize)> = Vec::new();
                    for (r, &c) in rs.into_iter().zip(&near) {
                        match r {
                            Some(f) => {
                                let (mut x0, mut y0, mut x1, mut y1) =
                                    (usize::MAX, usize::MAX, 0, 0);
                                for &(_, i) in &f.0 {
                                    let (x, y) = (i % nxg, i / nxg);
                                    (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x), y1.max(y));
                                }
                                let b = (
                                    x0.saturating_sub(gap),
                                    y0.saturating_sub(gap),
                                    x1 + gap,
                                    y1 + gap,
                                );
                                if kept
                                    .iter()
                                    .all(|q| b.2 < q.0 || q.2 < b.0 || b.3 < q.1 || q.3 < b.1)
                                {
                                    kept.push(b);
                                    found.push(f);
                                }
                            }
                            None => dead[c] = true,
                        }
                    }
                    found.reverse(); // pop() takes them in guide order
                    if found.is_empty() {
                        continue;
                    }
                } else {
                    let Some(f) = self.astar(
                        k,
                        &tree,
                        &src_info,
                        &tmark,
                        &tinfo,
                        &tree_mark,
                        &open,
                        &comp_cells,
                        o,
                        nego,
                    ) else {
                        break;
                    };
                    found.push(f);
                }
            }
            let (path, ci) = found.pop().unwrap();
            if in_tree[ci] {
                continue; // reached a component an earlier search of the batch already joined
            }
            in_tree[ci] = true;
            left -= 1;
            for cc in &comp_cells[ci] {
                let node = cc.0 * n + self.wi(k, cc.1).unwrap();
                tmark[node] = 0;
                tree_mark[node] = true;
                tree.push(*cc);
            }
            let added = self.emit(
                k, o, nego, &path, &pad_of, &node_stub, &mut used, &mut rn, &mut cells, &mut vias,
                ck, foreign, st,
            );
            for cc in &comp_cells[ci] {
                src_info.insert(*cc, info(cc, &used));
            }
            for c in [path[0], path[path.len() - 1]] {
                if node_stub.contains_key(&c) {
                    src_info.insert(c, info(&c, &used));
                }
            }
            for (l, i) in added {
                let Some(w) = self.wi(k, i) else { continue };
                if !tree_mark[l * n + w] {
                    tree_mark[l * n + w] = true;
                    tree.push((l, i));
                }
            }
        }
        if left > 0 && std::env::var("DROUTE_DEBUG").is_ok() {
            for c in 0..nc {
                if !in_tree[c] {
                    let names: Vec<String> = job
                        .pins
                        .iter()
                        .filter(|pn| comp_ids.binary_search(&pn.comp).unwrap() == c)
                        .map(|pn| {
                            format!(
                                "{}.{}({} cells)",
                                self.m.b.fps[pn.fp].refd,
                                self.m.b.fps[pn.fp].pads[pn.pad].num,
                                comp_cells[c].len()
                            )
                        })
                        .collect();
                    eprintln!(
                        "[dbg]   {} unreachable ({}): {:?}",
                        self.m.b.nets[job.net],
                        if soft { "negotiated" } else { "hard" },
                        names
                    );
                }
            }
        }
        (rn, left, cells, vias)
    }

    /// width choice of a cell: opt width unless only the neck-down width is free
    #[inline]
    fn wide_choice(&self, k: usize, o: &Owners, nego: Nego, l: usize, i: usize) -> bool {
        if nego.is_none() {
            return self.wide_ok(k, o, l, i);
        }
        let mp = &self.maps[k];
        let Some(w) = self.wi(k, i) else { return false };
        if mp.t[l][w] != R_FREE {
            return false;
        }
        if o.t[l][w] == 0 {
            return true;
        }
        !(mp.neck[w] && mp.tn[l][w] == R_FREE && o.tn[l][w] == 0)
    }

    /// multi-source multi-target A* inside a window (bbox of sources and targets + `window` mm);
    /// retried on the whole grid when the window search fails
    #[allow(clippy::too_many_arguments)]
    fn astar(
        &self,
        k: usize,
        src: &[(usize, usize)],
        src_info: &HashMap<(usize, usize), (f32, u8)>,
        tmark: &[u16],
        tinfo: &HashMap<usize, (f32, u8)>,
        tree_mark: &[bool],
        open: &[usize],
        comp_cells: &[Vec<(usize, usize)>],
        o: &Owners,
        nego: Nego,
    ) -> Option<(Vec<(usize, usize)>, usize)> {
        let nxg = self.gs.nx;
        let mut bbs: Vec<(i32, i32, i32, i32)> = Vec::new();
        let mut all = (i32::MAX, i32::MAX, i32::MIN, i32::MIN);
        let grow = |b: &mut (i32, i32, i32, i32), i: usize| {
            let (x, y) = ((i % nxg) as i32, (i / nxg) as i32);
            b.0 = b.0.min(x);
            b.1 = b.1.min(y);
            b.2 = b.2.max(x);
            b.3 = b.3.max(y);
        };
        for c in open {
            let mut b = (i32::MAX, i32::MAX, i32::MIN, i32::MIN);
            for &(_, i) in &comp_cells[*c] {
                grow(&mut b, i);
                grow(&mut all, i);
            }
            bbs.push(b);
        }
        for &(_, i) in src {
            grow(&mut all, i);
        }
        let wc = (self.p.window / self.p.res).ceil() as i32;
        let win = (all.0 - wc, all.1 - wc, all.2 + wc, all.3 + wc);
        if let Some(r) = self.astar_win(
            k, src, src_info, tmark, tinfo, tree_mark, &bbs, o, nego, win,
        ) {
            return Some(r);
        }
        let mp = &self.maps[k];
        let full = (
            mp.ox as i32,
            mp.oy as i32,
            (mp.ox + mp.g.nx) as i32 - 1,
            (mp.oy + mp.g.ny) as i32 - 1,
        );
        if win.0 <= full.0 && win.1 <= full.1 && win.2 >= full.2 && win.3 >= full.3 {
            return None;
        }
        self.astar_win(
            k, src, src_info, tmark, tinfo, tree_mark, &bbs, o, nego, full,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn astar_win(
        &self,
        k: usize,
        src: &[(usize, usize)],
        src_info: &HashMap<(usize, usize), (f32, u8)>,
        tmark: &[u16],
        tinfo: &HashMap<usize, (f32, u8)>,
        tree_mark: &[bool],
        bbs: &[(i32, i32, i32, i32)],
        o: &Owners,
        nego: Nego,
        win: (i32, i32, i32, i32),
    ) -> Option<(Vec<(usize, usize)>, usize)> {
        let mp = &self.maps[k];
        let nxg = self.gs.nx as i32;
        let n = mp.g.n();
        let x0 = win.0.max(mp.ox as i32);
        let y0 = win.1.max(mp.oy as i32);
        let x1 = win.2.min((mp.ox + mp.g.nx) as i32 - 1);
        let y1 = win.3.min((mp.oy + mp.g.ny) as i32 - 1);
        if x1 < x0 || y1 < y0 {
            return None;
        }
        let wx = (x1 - x0 + 1) as usize;
        let wy = (y1 - y0 + 1) as usize;
        let wn = wx * wy;
        let loc = |l: usize, i: usize| -> usize {
            let (x, y) = ((i as i32) % nxg, (i as i32) / nxg);
            l * wn + ((y - y0) as usize) * wx + (x - x0) as usize
        };
        let glob = |ln: usize| -> (usize, usize) {
            let (l, r) = (ln / wn, ln % wn);
            let (x, y) = ((r % wx) as i32 + x0, (r / wx) as i32 + y0);
            (l, (y * nxg + x) as usize)
        };
        let total = wn * NL;
        let mut g = vec![f32::INFINITY; total];
        let mut par = vec![u32::MAX; total];
        let mut indir = vec![8u8; total];
        // key of the newest heap entry per node: older entries are stale and skipped (lazy deletion)
        let mut fkey = vec![u32::MAX; total];
        // heuristic per (x, y), shared by all layers, computed on first use
        let hc = std::cell::RefCell::new(vec![f32::NAN; wn]);
        let h = |i: usize| -> f32 {
            let (x, y) = ((i as i32) % nxg, (i as i32) / nxg);
            let ci = ((y - y0) as usize) * wx + (x - x0) as usize;
            let mut hc = hc.borrow_mut();
            if !hc[ci].is_nan() {
                return hc[ci];
            }
            let mut best = i32::MAX as f32;
            for b in bbs {
                let dx = (b.0 - x).max(0).max(x - b.2) as f32;
                let dy = (b.1 - y).max(0).max(y - b.3) as f32;
                best = best.min(dx.max(dy) + (std::f32::consts::SQRT_2 - 1.0) * dx.min(dy));
            }
            hc[ci] = best;
            best
        };
        let inside = |i: usize| {
            let (x, y) = ((i as i32) % nxg, (i as i32) / nxg);
            x >= x0 && x <= x1 && y >= y0 && y <= y1
        };
        let mut heap: BinaryHeap<Reverse<(u32, u32)>> = BinaryHeap::new();
        let key = |f: f32| (f * 64.0) as u32;
        for &(l, i) in src {
            if !inside(i) {
                continue;
            }
            let (c0, d0) = src_info.get(&(l, i)).cloned().unwrap_or((0.0, 8));
            let ln = loc(l, i);
            if c0 < g[ln] {
                g[ln] = c0;
                indir[ln] = d0;
                fkey[ln] = key(c0 + self.p.hweight * h(i));
                heap.push(Reverse((fkey[ln], ln as u32)));
            }
        }
        let dirpref = &self.m.cfg.layer_dir;
        let opc = self.p.own_pad_cost;
        let cell_ok = |l: usize, i: usize| -> (bool, f32) {
            let Some(w) = mp.li(self.gs.nx, i) else {
                return (false, 0.0);
            };
            let own = if mp.own[w] & (1 << l) != 0 { opc } else { 0.0 };
            match nego {
                Some((pres, hist)) => {
                    let fw = mp.t[l][w] == R_FREE;
                    let fnr = mp.neck[w] && mp.tn[l][w] == R_FREE;
                    if !fw && !fnr {
                        return (false, 0.0);
                    }
                    let cw = if fw {
                        pres * o.t[l][w] as f32
                    } else {
                        f32::INFINITY
                    };
                    let cn = if fnr {
                        self.p.narrow_cost + pres * o.tn[l][w] as f32
                    } else {
                        f32::INFINITY
                    };
                    (true, cw.min(cn) + hist[l][i] + own)
                }
                None => {
                    if self.wide_ok(k, o, l, i) {
                        (true, own)
                    } else if self.narrow_ok(k, o, l, i) {
                        (true, self.p.narrow_cost + own)
                    } else {
                        (false, 0.0)
                    }
                }
            }
        };
        let bend = |t: u8| -> f32 {
            match t {
                0 => 0.0,
                1 => self.p.bend_cost,
                _ => self.p.bend_cost * self.p.bend90,
            }
        };
        let mut expanded = 0u64;
        let mut found = None;
        let hw = self.p.hweight;
        // node cap: the configured value, at least half the nodes of the job window
        let cap = self.p.max_expand.max((n * NL / 2) as u64);
        let (jx0, jy0) = (mp.ox as i32, mp.oy as i32);
        let (jx1, jy1) = (jx0 + mp.g.nx as i32 - 1, jy0 + mp.g.ny as i32 - 1);
        let clipped = mp.g.nx < self.gs.nx || mp.g.ny < self.gs.ny;
        let mut edge = false;
        while let Some(Reverse((fk, ln))) = heap.pop() {
            let ln = ln as usize;
            if fk != fkey[ln] {
                continue;
            }
            fkey[ln] = u32::MAX - 1; // expanded; a later improvement pushes it again
            let (l, i) = glob(ln);
            let node = l * n + mp.li(self.gs.nx, i).unwrap();
            expanded += 1;
            if expanded > cap {
                break;
            }
            let tm = tmark[node];
            if tm != 0 && !tree_mark[node] {
                found = Some((ln, tm as usize - 1));
                break;
            }
            let gc = g[ln];
            let x = (i as i32) % nxg;
            let y = (i as i32) / nxg;
            edge |= x == jx0 || x == jx1 || y == jy0 || y == jy1;
            let din = indir[ln];
            for (d, (dx, dy)) in DIRS.iter().enumerate() {
                let (xx, yy) = (x + dx, y + dy);
                if xx < x0 || yy < y0 || xx > x1 || yy > y1 {
                    continue;
                }
                let t = if din < 8 { tdir(din, d as u8) } else { 0 };
                if t > 2 {
                    continue; // no acute turns
                }
                let j = (yy * nxg + xx) as usize;
                let (okj, extra) = cell_ok(l, j);
                if !okj {
                    continue;
                }
                let mut c = if *dx != 0 && *dy != 0 {
                    std::f32::consts::SQRT_2
                } else {
                    1.0
                };
                if !is_outer(l) {
                    let wrong = if dirpref[l] == 0 { *dx == 0 } else { *dy == 0 };
                    if wrong {
                        c *= 1.0 + self.p.wrong_dir;
                    } else if *dx != 0 && *dy != 0 {
                        c *= 1.0 + 0.5 * self.p.wrong_dir;
                    }
                }
                c += bend(t) + extra;
                let nj = l * n + mp.li(self.gs.nx, j).unwrap();
                if tmark[nj] != 0 && !tree_mark[nj] {
                    if let Some(&(tc, td)) = tinfo.get(&nj) {
                        let t2 = tdir(d as u8, td);
                        if t2 > 2 {
                            continue;
                        }
                        c += tc + bend(t2);
                    }
                }
                let nn = l * wn + ((yy - y0) as usize) * wx + (xx - x0) as usize;
                let ng = gc + c;
                if ng < g[nn] {
                    g[nn] = ng;
                    par[nn] = ln as u32;
                    indir[nn] = d as u8;
                    fkey[nn] = key(ng + hw * h(j));
                    heap.push(Reverse((fkey[nn], nn as u32)));
                }
            }
            // through via: legal for this net's via, landing cell must be legal on the new layer
            let wv = mp.li(self.gs.nx, i).unwrap();
            let via_ok = mp.v[wv] == R_FREE && (nego.is_some() || o.v[wv] == 0);
            if via_ok {
                let pen = match nego {
                    Some((pres, _)) => pres * o.v[wv] as f32,
                    None => 0.0,
                };
                for l2 in 0..NL {
                    if l2 == l {
                        continue;
                    }
                    let (okj, extra) = cell_ok(l2, i);
                    if !okj {
                        continue;
                    }
                    let nn = l2 * wn + (ln % wn);
                    let nj = l2 * n + wv;
                    let mut tc = 0.0;
                    if tmark[nj] != 0 && !tree_mark[nj] {
                        if let Some(&(c2, _)) = tinfo.get(&nj) {
                            tc = c2;
                        }
                    }
                    let ng = gc + self.p.via_cost + pen + extra + tc;
                    if ng < g[nn] {
                        g[nn] = ng;
                        par[nn] = ln as u32;
                        indir[nn] = 8;
                        fkey[nn] = key(ng + hw * h(i));
                        heap.push(Reverse((fkey[nn], nn as u32)));
                    }
                }
            }
        }
        self.expanded
            .fetch_add(expanded, std::sync::atomic::Ordering::Relaxed);
        if found.is_none() {
            let why = if expanded > cap {
                1
            } else if edge && clipped {
                2
            } else {
                3
            };
            self.why[k].store(why, std::sync::atomic::Ordering::Relaxed);
        }
        let (ln, c) = found?;
        let mut path = vec![glob(ln)];
        let mut cur = ln;
        while par[cur] != u32::MAX {
            cur = par[cur] as usize;
            path.push(glob(cur));
        }
        path.reverse();
        Some((path, c))
    }

    /// convert a cell path into runs per layer (split at vias), attach exit stubs / junctions /
    /// fallback pad stubs at the ends, clean each run exactly (stub alignment, octilinear
    /// pull-tight), and append segments and vias. Returns the cells of the new copper that later
    /// branches may start from (stub segments excluded up to their end).
    #[allow(clippy::too_many_arguments)]
    fn emit(
        &self,
        k: usize,
        o: &Owners,
        nego: Nego,
        path: &[(usize, usize)],
        pad_of: &HashMap<(usize, usize), P>,
        node_stub: &HashMap<(usize, usize), usize>,
        used: &mut [Option<P>],
        rn: &mut RNet,
        cells: &mut Vec<(usize, usize, bool)>,
        vias: &mut Vec<usize>,
        ck: &Checker,
        foreign: &[FItem],
        st: &mut AesStats,
    ) -> Vec<(usize, usize)> {
        let mut added = Vec::new();
        if path.is_empty() {
            return added;
        }
        let job = &self.jobs[k];
        let mp = &self.maps[k];
        let stubs = &self.stubs[k];
        let c = |i: usize| self.gs.center(i % self.gs.nx, i / self.gs.nx);
        // fallback pins: drop leading/trailing cells inside the same pad
        let mut s = 0;
        while s + 1 < path.len()
            && pad_of.contains_key(&path[s + 1])
            && pad_of.get(&path[s + 1]) == pad_of.get(&path[s])
            && path[s + 1].0 == path[s].0
        {
            s += 1;
        }
        let mut e = path.len() - 1;
        while e > s
            && pad_of.contains_key(&path[e - 1])
            && pad_of.get(&path[e - 1]) == pad_of.get(&path[e])
            && path[e - 1].0 == path[e].0
        {
            e -= 1;
        }
        let path = &path[s..=e];
        let wide = |l: usize, i: usize| self.wide_choice(k, o, nego, l, i);
        let (ww, wn) = (job.hw * 2.0, job.hn * 2.0);
        // layer runs
        let mut runs: Vec<Run> = Vec::new();
        let mut via_at: Vec<(P, usize)> = Vec::new();
        let mut a = 0;
        while a < path.len() {
            let mut b = a;
            while b + 1 < path.len() && path[b + 1].0 == path[a].0 {
                b += 1;
            }
            let l = path[a].0;
            let pts: Vec<P> = path[a..=b].iter().map(|q| c(q.1)).collect();
            let w: Vec<f64> = path[a..=b]
                .windows(2)
                .map(|q| {
                    if wide(l, q[0].1) && wide(l, q[1].1) {
                        ww
                    } else {
                        wn
                    }
                })
                .collect();
            runs.push(Run {
                l,
                pts,
                w,
                head: End::Fixed,
                tail: End::Fixed,
                alts: [Vec::new(), Vec::new()],
            });
            if b + 1 < path.len() {
                via_at.push((c(path[b].1), path[b].1));
            }
            a = b + 1;
        }
        // old-style stub to the pad centre for fallback pins (only where the centre is legal)
        let pstub = |pc: P, l: usize, i: usize| -> Option<bool> {
            let (x, y) = self.gs.cell_of(pc)?;
            let ci = self.gs.idx(x, y);
            let w = wide(l, i);
            let wc = self.wi(k, ci)?;
            if w && self.wide_ok(k, o, l, ci) {
                Some(true)
            } else if mp.tn[l][wc] == R_FREE && o.tn[l][wc] == 0 {
                Some(false)
            } else {
                None
            }
        };
        // ends: head of the first run, tail of the last run
        let mut used_head: Option<usize> = None;
        let mut used_tail: Option<usize> = None;
        for end in 0..2 {
            let node = if end == 0 {
                path[0]
            } else {
                path[path.len() - 1]
            };
            let ri = if end == 0 { 0 } else { runs.len() - 1 };
            let mut r = if end == 0 {
                runs[ri].clone()
            } else {
                runs[ri].reversed()
            };
            if end == 1 && path.len() == 1 {
                break;
            }
            if let Some(&si) = node_stub.get(&node) {
                let sb = &stubs[si];
                if let Some(x) = used[si] {
                    r.pts[0] = x;
                    r.head = End::Fixed;
                } else {
                    r.pts.insert(0, sb.c);
                    r.w.insert(0, if sb.wide { ww } else { wn });
                    r.head = End::Stub(sb.u, sb.min_len);
                    // the pad's other unused stubs on this layer: the cleanup may switch the exit
                    r.alts[0] = stubs
                        .iter()
                        .enumerate()
                        .filter(|(i2, s2)| {
                            s2.pin == sb.pin && s2.l == sb.l && (*i2 == si || used[*i2].is_none())
                        })
                        .map(|(i2, s2)| clean::Alt {
                            u: s2.u,
                            ml: s2.min_len,
                            hard: if s2.short {
                                s2.min_len
                            } else {
                                s2.min_len - 0.38
                            },
                            short: s2.short,
                            id: i2,
                        })
                        .collect();
                    if end == 0 {
                        used_head = Some(si);
                    } else {
                        used_tail = Some(si);
                    }
                }
                st.stubs_used += 1;
            } else if let Some(pc) = pad_of.get(&node) {
                st.fallback_ends += 1;
                if let Some(wd) = pstub(*pc, node.0, node.1) {
                    r.pts.insert(0, *pc);
                    let w0 = if wd && wide(node.0, node.1) { ww } else { wn };
                    r.w.insert(0, w0);
                    if !wd && !r.w.is_empty() && r.w.len() > 1 {
                        r.w[1] = wn;
                    }
                }
                r.head = End::Fixed;
            } else {
                // junction on this net's copper
                let q = r.pts[0];
                let par = rn
                    .segs
                    .iter()
                    .filter(|s| s.l == r.l)
                    .map(|s| (dist_seg(q, s.a, s.b), s))
                    .filter(|(d, s)| *d < s.w / 2.0)
                    .min_by(|x, y| x.0.partial_cmp(&y.0).unwrap());
                r.head = match par {
                    Some((_, s)) if (s.b - s.a).len() > 1e-9 => End::Junction(s.a, s.b),
                    _ => End::Fixed,
                };
            }
            runs[ri] = if end == 0 { r } else { r.reversed() };
        }
        // exact cleanup per run
        let neck = |q: P| -> bool {
            match self
                .gs
                .cell_of(q)
                .and_then(|(x, y)| self.wi(k, self.gs.idx(x, y)))
            {
                Some(w) => mp.neck[w],
                None => false,
            }
        };
        let own_pads: Vec<(Shape, u8)> = job
            .pins
            .iter()
            .flat_map(|pn| {
                let f = &self.m.b.fps[pn.fp];
                let pd = &f.pads[pn.pad];
                f.pad_shapes(pd).into_iter().map(move |s| (s, pd.layers))
            })
            .collect();
        let cx = clean::Ctx {
            ck,
            net: job.net,
            hw_opt: job.hw,
            margin: self.p.margin,
            foreign,
            own_pads: &own_pads,
            neck: &neck,
            slide: self.p.slide,
        };
        let mut cs = clean::CleanStats::default();
        let nr = runs.len();
        // via slide: a stub run that goes straight to a via slightly off its axis moves the via
        // onto the axis (exactly checked: via rules, own vias hole-to-hole, both runs)
        if self.p.clean {
            let hh = self.m.r.min_hole_to_hole + job.dr * 2.0;
            let vr = job.vr + self.p.margin;
            let via_ok = |q: P| {
                ck.via_ok_fixed(job.net, q, vr, job.dr)
                    && ck.via_ok_foreign(job.net, q, vr, job.dr, foreign)
                    && rn.vias.iter().all(|v| (v.at - q).len() > hh)
            };
            for k2 in 0..via_at.len() {
                for r in runs.iter_mut() {
                    r.merge();
                }
                let (left, right) = runs.split_at_mut(k2 + 1);
                let (ra, rb) = (&mut left[k2], &mut right[0]);
                if let Some(x) = clean::straight_stub_target(ra) {
                    if clean::via_slide(ra, rb, x, &cx, &via_ok) {
                        via_at[k2].0 = x;
                        cs.aligned += 1;
                    }
                } else {
                    let mut rbr = rb.reversed();
                    if let Some(x) = clean::straight_stub_target(&rbr) {
                        let mut rar = ra.reversed();
                        if clean::via_slide(&mut rbr, &mut rar, x, &cx, &via_ok) {
                            *rb = rbr.reversed();
                            *ra = rar.reversed();
                            via_at[k2].0 = x;
                            cs.aligned += 1;
                        }
                    }
                }
            }
        }
        for r in runs.iter_mut() {
            if r.pts.len() < 2 {
                continue;
            }
            if self.p.clean {
                clean::clean_run(r, &cx, &mut cs);
            } else {
                r.merge();
            }
        }
        // the stub each stub end finally uses (the cleanup may have switched axes or shortened it)
        let pin_name = |pi: usize| {
            let pn = &job.pins[pi];
            format!(
                "{}.{}",
                self.m.b.fps[pn.fp].refd, self.m.b.fps[pn.fp].pads[pn.pad].num
            )
        };
        for (e, ri, ue) in [
            (0usize, 0usize, &mut used_head),
            (1, nr - 1, &mut used_tail),
        ] {
            let Some(si) = *ue else { continue };
            let r = &runs[ri];
            if std::env::var("PR_DEBUG").is_ok() {
                eprintln!(
                    "[dbg] end {} pin {} si {} pts {:?} alts {:?}",
                    e,
                    pin_name(stubs[si].pin),
                    si,
                    r.pts,
                    r.alts[e]
                );
            }
            if r.pts.len() < 2 {
                continue;
            }
            if let Some(a) = clean::used_alt(r, e) {
                let n = r.pts.len();
                let len = if e == 0 {
                    (r.pts[1] - r.pts[0]).len()
                } else {
                    (r.pts[n - 2] - r.pts[n - 1]).len()
                };
                if a.short || len < a.ml - 1e-6 {
                    st.short_stubs.push(pin_name(stubs[a.id].pin));
                }
                *ue = Some(a.id);
            } else if stubs[si].short {
                st.short_stubs.push(pin_name(stubs[si].pin));
            }
        }
        st.aligned += cs.aligned;
        st.align_failed += cs.align_failed;
        st.pulls += cs.pulls;
        st.mitered += cs.mitered;
        st.miter_failed += cs.miter_failed;
        st.shifted += cs.shifted;
        st.jogs += cs.jogs;
        for q in 0..5 {
            st.kept90[q] += cs.kept90[q];
        }
        // stub ends now in use: a point on the stub axis at the stub length (still on the copper)
        let stub_end = |r: &Run| -> P {
            let (c0, p1) = (r.pts[0], r.pts[1]);
            if let End::Stub(..) = r.head {
                if let Some(a) = clean::used_alt(r, 0) {
                    let len = (p1 - c0).len();
                    return c0 + a.u * len.min(a.ml + 0.1);
                }
            }
            p1
        };
        if let Some(si) = used_head {
            if runs[0].pts.len() >= 2 {
                used[si] = Some(stub_end(&runs[0]));
            }
        }
        if let Some(si) = used_tail {
            let r = runs[nr - 1].reversed();
            if r.pts.len() >= 2 {
                used[si] = Some(stub_end(&r));
            }
        }
        // emit segments, rasterise for conflicts and tree sources
        let res = self.gs.res;
        for r in &runs {
            for (sk, sg) in r.pts.windows(2).enumerate() {
                let (a, b) = (sg[0], sg[1]);
                if (b - a).len() < 1e-9 {
                    continue;
                }
                let w = r.w[sk];
                rn.segs.push(RSeg { a, b, l: r.l, w });
                let is_stub_seg = |q: P| -> bool {
                    (sk == 0
                        && matches!(r.head, End::Stub(..))
                        && (q - r.pts[0]).len() < (b - a).len() - 0.06)
                        || (sk + 2 == r.pts.len()
                            && matches!(r.tail, End::Stub(..))
                            && (q - r.pts[r.pts.len() - 1]).len() < (b - a).len() - 0.06)
                };
                let ns = ((b - a).len() / (res * 0.25)).ceil() as usize + 1;
                let mut last = usize::MAX;
                for s2 in 0..=ns {
                    let q = a + (b - a) * (s2 as f64 / ns as f64);
                    let Some((x, y)) = self.gs.cell_of(q) else {
                        continue;
                    };
                    let ci = self.gs.idx(x, y);
                    if ci != last {
                        cells.push((r.l, ci, w > wn + 1e-9 || (ww - wn).abs() < 1e-9));
                        last = ci;
                    }
                    if is_stub_seg(q) {
                        continue;
                    }
                    for dy in -1i64..=1 {
                        for dx in -1i64..=1 {
                            let (xx, yy) = (x as i64 + dx, y as i64 + dy);
                            if xx < 0
                                || yy < 0
                                || xx >= self.gs.nx as i64
                                || yy >= self.gs.ny as i64
                            {
                                continue;
                            }
                            let cc = self.gs.center(xx as usize, yy as usize);
                            if dist_seg(cc, a, b) <= 0.03 {
                                added.push((r.l, self.gs.idx(xx as usize, yy as usize)));
                            }
                        }
                    }
                }
            }
        }
        cells.sort_unstable();
        cells.dedup();
        added.sort_unstable();
        added.dedup();
        // vias at layer changes; one via per spot (a second via closer than hole-to-hole is
        // replaced by short links on both layers to the existing one)
        let hh = self.m.r.min_hole_to_hole + job.dr * 2.0;
        for (k2, (a, ci)) in via_at.iter().enumerate() {
            let (la, lb) = (runs[k2].l, runs[k2 + 1].l);
            vias.push(*ci);
            if let Some(ex) = rn.vias.iter().map(|v| v.at).find(|v| (*v - *a).len() < hh) {
                if (ex - *a).len() > 1e-6 {
                    for l in [la, lb] {
                        rn.segs.push(RSeg {
                            a: ex,
                            b: *a,
                            l,
                            w: job.hn * 2.0,
                        });
                    }
                }
            } else {
                rn.vias.push(RVia {
                    at: *a,
                    d: job.vr * 2.0,
                    drill: job.dr * 2.0,
                });
            }
        }
        added
    }
}

/// exit stubs of every pin of a job: for each copper layer of the pad and each of the 4 pad axes,
/// a straight segment from the pad centre to half extent + stub_ext, ending on the grid cell nearest
/// to that point (projected back onto the axis). Kept when exactly legal for the opt width (else the
/// neck-down width), not crossing other pads of the net, and its end cell is legal in the maps.
fn make_stubs(
    m: &Model,
    ck: &Checker,
    gs: &GridSpec,
    j: &NetJob,
    mp: &NetMaps,
    p: &DParams,
) -> Vec<Stub> {
    let mut out = Vec::new();
    let own: Vec<(usize, Vec<Shape>, u8)> = j
        .pins
        .iter()
        .enumerate()
        .map(|(pi, pn)| {
            let f = &m.b.fps[pn.fp];
            (pi, f.pad_shapes(&f.pads[pn.pad]), f.pads[pn.pad].layers)
        })
        .collect();
    for (pi, pn) in j.pins.iter().enumerate() {
        let f = &m.b.fps[pn.fp];
        let pd = &f.pads[pn.pad];
        let (c, a) = f.pad_world(pd);
        let am = a.rem_euclid(45.0);
        if am > 1e-6 && am < 45.0 - 1e-6 {
            continue; // pad axes off the router's 8 directions
        }
        let (sw, sh) = pd.size;
        let elong = sw.max(sh) >= 1.2 * sw.min(sh);
        let st0 = out.len();
        for (q, off) in OFF.iter().enumerate() {
            let u = clean::unit(crate::geom::p(off.0, off.1).rot(a));
            let half = if q % 2 == 0 { sw / 2.0 } else { sh / 2.0 };
            let short_axis = elong && half < sw.max(sh) / 2.0 - 1e-9;
            let Some(d) = DIRS.iter().position(|(dx, dy)| {
                clean::unit(crate::geom::p(*dx as f64, *dy as f64)).dot(u) > 0.999
            }) else {
                continue;
            };
            for l in 0..NL {
                if pd.layers & m.net_layers[j.net] & (1 << l) == 0 {
                    continue;
                }
                // full-length stub first; shorter ones (exceptions) only when it is blocked
                // long stubs where there is room (the corner after it can then be mitered while the
                // diagonal still starts >= 0.5 mm outside the pad), minimum straight-exit length in dense arrays
                let exts: Vec<(f64, bool)> = [p.stub_ext_long, 0.75, p.stub_ext]
                    .into_iter()
                    .filter(|e| *e >= p.stub_ext - 1e-9)
                    .map(|e| (e, false))
                    .chain([(0.4, true), (0.3, true), (0.2, true), (0.12, true)])
                    .collect();
                for (ext, short) in exts {
                    let e0 = c + u * (half + ext);
                    let Some((gx, gy)) = gs.cell_of(e0) else {
                        continue;
                    };
                    let gi = gs.idx(gx, gy);
                    let Some(w) = mp.li(gs.nx, gi) else { continue };
                    let gc = gs.center(gx, gy);
                    let e = c + u * (gc - c).dot(u);
                    let min_len = if short {
                        ((e - c).len() - 0.03).max(half + 0.05)
                    } else {
                        half + 0.5
                    };
                    if (e - c).len() < min_len || (e - c).len() < half + 0.05 {
                        continue;
                    }
                    let cl = Shape::Capsule { a: c, b: e, r: 0.0 };
                    let mut wide = None;
                    for (wd, hw) in [(true, j.hw), (false, j.hn)] {
                        let cell_ok = if wd {
                            mp.t[l][w] == R_FREE
                        } else {
                            mp.neck[w] && mp.tn[l][w] == R_FREE
                        };
                        if !cell_ok {
                            continue;
                        }
                        if own.iter().any(|(pj, shs, ls)| {
                            *pj != pi
                                && ls & (1 << l) != 0
                                && shs.iter().any(|s| shape_dist(&cl, s) < hw)
                        }) {
                            continue;
                        }
                        if ck.seg_ok_fixed(j.net, c, e, hw + p.margin, l) {
                            wide = Some(wd);
                            break;
                        }
                    }
                    let Some(wd) = wide else { continue };
                    let mut cells = Vec::new();
                    let n = ((e - c).len() / (gs.res * 0.5)).ceil() as usize + 1;
                    for s in 0..=n {
                        if let Some((x, y)) = gs.cell_of(c + (e - c) * (s as f64 / n as f64)) {
                            cells.push(gs.idx(x, y));
                        }
                    }
                    cells.dedup();
                    let cost = if short_axis { p.stub_side_cost } else { 0.0 }
                        + if short { p.short_stub_cost } else { 0.0 };
                    out.push(Stub {
                        pin: pi,
                        l,
                        d: d as u8,
                        u,
                        c,
                        e,
                        g: gi,
                        wide: wd,
                        cost,
                        min_len,
                        cells,
                        short,
                        side: short_axis,
                    });
                    break;
                }
            }
        }
        // elongated pad: the exit follows the long axis whenever one is legal on that layer
        if elong {
            let keep: Vec<bool> = (st0..out.len())
                .map(|i| !out[i].side || !out[st0..].iter().any(|o| o.l == out[i].l && !o.side))
                .collect();
            let mut it = keep.into_iter();
            let tail: Vec<Stub> = out.drain(st0..).filter(|_| it.next().unwrap()).collect();
            out.extend(tail);
        }
    }
    out
}

impl<'a> Router<'a> {
    /// per-layer images for one net: cells legal for its track (green: opt width, yellow: neck-down
    /// width), all routed copper (white), its pins (magenta)
    pub fn debug_images(&self, net: usize, res: &DResult, dir: &str) {
        let Some(k) = self.jobs.iter().position(|j| j.net == net) else {
            return;
        };
        let mp = &self.maps[k];
        for (l, lname) in CU.iter().enumerate() {
            let mut cv = crate::check::layer_image(self.m, self.gs, Some(l), None, None);
            for i in 0..self.gs.n() {
                let (x, y) = (i % self.gs.nx, i / self.gs.nx);
                let Some(w) = mp.li(self.gs.nx, i) else {
                    continue;
                };
                if mp.t[l][w] == R_FREE {
                    cv.blend(x, y, [60, 255, 90], 0.45);
                } else if mp.neck[w] && mp.tn[l][w] == R_FREE {
                    cv.blend(x, y, [255, 230, 60], 0.6);
                }
            }
            for rn in res.routed.values() {
                for sg in rn.segs.iter().filter(|sg| sg.l == l) {
                    cv.shape(
                        &Shape::Capsule {
                            a: sg.a,
                            b: sg.b,
                            r: sg.w / 2.0,
                        },
                        [240, 240, 240],
                        0.9,
                    );
                }
                for v in &rn.vias {
                    cv.shape(&Shape::circle(v.at, v.d / 2.0), [255, 255, 255], 1.0);
                }
            }
            for pn in &self.jobs[k].pins {
                let f = &self.m.b.fps[pn.fp];
                for sh in f.pad_shapes(&f.pads[pn.pad]) {
                    cv.shape(&sh, [255, 0, 255], 0.7);
                }
            }
            cv.save(&format!("{dir}/dbg_{lname}.png"), 2).ok();
            if l == 0 {
                for pn in &self.jobs[k].pins {
                    for dy in [-1.5, -1.0, -0.5, 0.0, 0.5, 1.0] {
                        if let Some((x, y)) = self.gs.cell_of(pn.at + p(0.0, dy)) {
                            let Some(i) = mp.li(self.gs.nx, self.gs.idx(x, y)) else {
                                continue;
                            };
                            eprintln!(
                                "[dbg] {}.{} dy {:+.1}: wide {} narrow {} neck {}",
                                self.m.b.fps[pn.fp].refd,
                                self.m.b.fps[pn.fp].pads[pn.pad].num,
                                dy,
                                crate::sites::kind_name(mp.t[0][i]),
                                crate::sites::kind_name(mp.tn[0][i]),
                                mp.neck[i]
                            );
                        }
                    }
                }
            }
        }
    }
}

// ------------------------------------------------------------------------------------------------
// KiCad output

fn uuid(seed: &mut u64) -> String {
    let mut nx = || {
        *seed ^= *seed << 13;
        *seed ^= *seed >> 7;
        *seed ^= *seed << 17;
        *seed
    };
    let (a, b) = (nx(), nx());
    format!(
        "{:08x}-{:04x}-4{:03x}-{:04x}-{:012x}",
        (a >> 32) as u32,
        (a >> 16) as u16,
        (a & 0xfff) as u16,
        ((b >> 48) as u16 & 0x3fff) | 0x8000,
        b & 0xffff_ffff_ffff
    )
}

/// append routed copper to the board text (unlocked, so it can be ripped by later runs)
pub fn write_pcb(text: &str, nets: &[String], res: &DResult, out: &str) -> std::io::Result<()> {
    let mut seed = 0x1234_5678_9abc_def1u64;
    let mut add = String::new();
    let mut keys: Vec<&usize> = res.routed.keys().collect();
    keys.sort();
    for &net in keys {
        let rn = &res.routed[&net];
        let name = crate::sexpr::q(&nets[net]);
        for s in &rn.segs {
            add += &format!(
                "\t(segment\n\t\t(start {:.6} {:.6})\n\t\t(end {:.6} {:.6})\n\t\t(width {})\n\t\t(layer \"{}\")\n\t\t(net {})\n\t\t(uuid \"{}\")\n\t)\n",
                s.a.x, s.a.y, s.b.x, s.b.y, s.w, CU[s.l], name, uuid(&mut seed)
            );
        }
        for v in &rn.vias {
            add += &format!(
                "\t(via\n\t\t(at {:.6} {:.6})\n\t\t(size {})\n\t\t(drill {})\n\t\t(layers \"F.Cu\" \"B.Cu\")\n\t\t(net {})\n\t\t(uuid \"{}\")\n\t)\n",
                v.at.x, v.at.y, v.d, v.drill, name, uuid(&mut seed)
            );
        }
    }
    let end = text.rfind(')').unwrap();
    let mut o = String::with_capacity(text.len() + add.len());
    o.push_str(&text[..end]);
    o.push_str(&add);
    o.push_str(&text[end..]);
    std::fs::write(out, o)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seg(a: P, b: P) -> RSeg {
        RSeg {
            a,
            b,
            l: 0,
            w: 0.25,
        }
    }

    #[test]
    fn tiny_leg_found_and_resolved() {
        // parent along x, a 0.07 mm diagonal leg off its middle, then a vertical neighbour
        let rn = RNet {
            segs: vec![
                seg(p(0.0, 0.0), p(10.0, 0.0)),
                seg(p(5.0, 0.0), p(5.05, 0.05)),
                seg(p(5.05, 0.05), p(5.05, 3.0)),
            ],
            vias: vec![],
        };
        let (leg, nb, a_end, x) = tiny_leg(&rn, &[]).expect("leg");
        assert_eq!((leg, nb, a_end), (1, 2, true));
        assert!((x - p(5.05, 0.0)).len() < 1e-9);
    }

    #[test]
    fn tiny_leg_kept_at_via_or_long_leg() {
        let mut rn = RNet {
            segs: vec![
                seg(p(0.0, 0.0), p(10.0, 0.0)),
                seg(p(5.0, 0.0), p(5.05, 0.05)),
                seg(p(5.05, 0.05), p(5.05, 3.0)),
            ],
            vias: vec![],
        };
        rn.vias.push(RVia {
            at: p(5.05, 0.05),
            d: 0.6,
            drill: 0.3,
        });
        assert!(tiny_leg(&rn, &[]).is_none());
        // a leg longer than the track width is a real segment
        let rn = RNet {
            segs: vec![
                seg(p(0.0, 0.0), p(10.0, 0.0)),
                seg(p(5.0, 0.0), p(5.3, 0.3)),
                seg(p(5.3, 0.3), p(5.3, 3.0)),
            ],
            vias: vec![],
        };
        assert!(tiny_leg(&rn, &[]).is_none());
    }
}
