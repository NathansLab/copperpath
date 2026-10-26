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
use crate::geom::{self, *};
use crate::model::*;
use crate::sites::{track_map, track_stamp, via_map, via_stamp, R_FREE};
use rayon::prelude::*;
use std::collections::{HashMap, HashSet};

/// silences the router's progress lines (grid, iterations, hotspots) while set, e.g. while
/// `nudge` routes thousands of small candidate windows
pub static QUIET: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

macro_rules! dlog {
    ($($t:tt)*) => {
        if !QUIET.load(std::sync::atomic::Ordering::Relaxed) {
            eprintln!($($t)*)
        }
    };
}

#[derive(Clone, Debug)]
pub struct DParams {
    pub res: f64,
    /// added to every required clearance of the maps and checks (mm); the default -EXACT_TOL lets
    /// copper touch the clearance exactly
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
    /// stop negotiating in time to finish by this instant (--time-limit); best result is written
    pub deadline: Option<std::time::Instant>,
    /// iterations without a new minimum of conflict cells before the negotiation stops
    pub stall_iters: usize,
    /// a new minimum counts only if it is this fraction below the earlier one (0 = any gain)
    pub stall_gain: f32,
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
    /// --progress-json: `iter` and phase events of this run (off for the fallback runs)
    pub progress: bool,
    /// also connect copper components (tracks, vias) that reach into the region while their
    /// pins lie outside it, e.g. stubs left by ripping copper inside the region (`nudge`)
    pub copper_comps: bool,
}

impl Default for DParams {
    fn default() -> Self {
        DParams {
            res: 0.05,
            margin: -EXACT_TOL,
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
            deadline: None,
            stall_iters: 3,
            stall_gain: 0.0,
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
            progress: false,
            bend90: 1.4,
            clean: true,
            copper_comps: false,
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

/// merge collinear overlapping segments of one net (same layer); returns how many were removed.
/// A segment ending on a pad centre (`centres`) is kept unless the other one ends there too (the
/// pad exit stays a track end at the pad centre).
fn merge_collinear(segs: &mut Vec<RSeg>, centres: &[P]) -> usize {
    const TOL: f64 = 1e-4;
    let on_line = |q: P, s: &RSeg| {
        let d = s.b - s.a;
        let l = d.len();
        l > 1e-9 && ((q.x - s.a.x) * d.y - (q.y - s.a.y) * d.x).abs() / l <= TOL
    };
    // position of q along s in [0, 1] (with tolerance)
    let t = |q: P, s: &RSeg| {
        let d = s.b - s.a;
        ((q.x - s.a.x) * d.x + (q.y - s.a.y) * d.y) / (d.x * d.x + d.y * d.y)
    };
    let mut removed = 0;
    'outer: loop {
        for i in 0..segs.len() {
            for j in 0..segs.len() {
                if i == j || segs[i].l != segs[j].l {
                    continue;
                }
                let (a, b) = (segs[i].clone(), segs[j].clone());
                if (a.b - a.a).len() < 1e-9 || !on_line(a.a, &b) || !on_line(a.b, &b) {
                    continue;
                }
                let at_centre = |q: P| centres.iter().any(|c| (*c - q).len() < 2e-3);
                let ends_b = |q: P| (q - b.a).len() < 2e-3 || (q - b.b).len() < 2e-3;
                if [a.a, a.b].iter().any(|&q| at_centre(q) && !ends_b(q)) {
                    continue;
                }
                let lb = (b.b - b.a).len();
                let eps = TOL / lb.max(1e-9);
                let (ta, tb) = (t(a.a, &b), t(a.b, &b));
                let inside = |x: f64| x >= -eps && x <= 1.0 + eps;
                let strictly = |x: f64| x > eps && x < 1.0 - eps;
                // a covered by b (not just meeting it at one end point)
                let covered = inside(ta)
                    && inside(tb)
                    && (strictly(ta) || strictly(tb) || (ta - tb).abs() > 1.0 - 2.0 * eps);
                if covered && a.w <= b.w + 1e-9 {
                    segs.remove(i);
                    removed += 1;
                    continue 'outer;
                }
                // partial overlap of equal width: b grows to the union (segments that only meet
                // end to end stay as they are)
                if (a.w - b.w).abs() < 1e-9 && (strictly(ta) || strictly(tb)) {
                    let lo = ta.min(tb).min(0.0);
                    let hi = ta.max(tb).max(1.0);
                    let at = |x: f64| P {
                        x: b.a.x + (b.b.x - b.a.x) * x,
                        y: b.a.y + (b.b.y - b.a.y) * x,
                    };
                    segs[j].a = at(lo);
                    segs[j].b = at(hi);
                    segs.remove(i);
                    removed += 1;
                    continue 'outer;
                }
            }
        }
        break;
    }
    removed
}

/// per-thread A* buffers, kept between searches; only the nodes a search wrote are reset
#[derive(Default)]
struct Scratch {
    g: Vec<f32>,
    par: Vec<u32>,
    indir: Vec<u8>,
    fkey: Vec<u32>,
    hc: Vec<f32>,
    touched: Vec<u32>,
    htouched: Vec<u32>,
}

impl Scratch {
    /// at least `total` nodes and `wn` heuristic cells, all at their initial values
    fn reset(&mut self, total: usize, wn: usize) {
        for &i in &self.touched {
            let i = i as usize;
            self.g[i] = f32::INFINITY;
            self.par[i] = u32::MAX;
            self.indir[i] = 8;
            self.fkey[i] = u32::MAX;
        }
        self.touched.clear();
        for &i in &self.htouched {
            self.hc[i as usize] = f32::NAN;
        }
        self.htouched.clear();
        if self.g.len() < total {
            self.g.resize(total, f32::INFINITY);
            self.par.resize(total, u32::MAX);
            self.indir.resize(total, 8);
            self.fkey.resize(total, u32::MAX);
        }
        if self.hc.len() < wn {
            self.hc.resize(wn, f32::NAN);
        }
    }
}

thread_local! {
    static SCRATCH: std::cell::RefCell<Scratch> = std::cell::RefCell::new(Scratch::default());
}

pub struct NetJob {
    pub net: usize,
    pub pins: Vec<Pin>, // pins inside the region
    pub hw: f64,        // opt half width
    pub hn: f64,        // min half width (neck-down)
    /// footprints whose courtyard allows a narrower neck-down width (DRU `track_width` rule with
    /// `intersectsCourtyard`): the neck-down then applies inside these courtyards only
    pub neck_fps: Vec<usize>,
    pub vr: f64,
    pub dr: f64,
    /// routing intent (cost multipliers, routed first)
    pub prio: crate::model::NetPrio,
    /// existing tracks, vias and zone fills of the pin components: a connection may start or
    /// end on them, not only at the pads
    pub copper: Vec<Seed>,
    /// the components to connect (sorted): those of the pins, plus copper-only ones with
    /// [`DParams::copper_comps`]
    pub comps: Vec<usize>,
}

/// existing copper of a pin component that can start or end a connection
#[derive(Debug, Clone)]
pub struct Seed {
    pub shape: Shape,
    pub layers: u8,
    /// component id ([`Pin::comp`])
    pub comp: usize,
    /// via centre (a via end is pulled to it)
    pub via_at: Option<P>,
    /// saved zone fill: seeds only inside the job window (it does not widen the window), the
    /// interior sampled every 4th cell plus the rim
    pub fill: bool,
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
    /// layers with at least one free cell (wide or narrow): the only layers the net's copper,
    /// and so its owner maps, can use
    lmask: u8,
}

/// extra cost per cell inside the `iso` spacing of another net's copper
const ISO_COST: f32 = 1.5;

/// job windows above this many nodes (cells x layers) get a reachability flood before the search
const FLOOD_MIN_NODES: usize = 4_000_000;

/// square dilation of a mask by r cells (Chebyshev distance), two separable passes with prefix
/// sums: O(cells) for any r
fn dilate(m: &[bool], nx: usize, ny: usize, r: usize) -> Vec<bool> {
    let run = |get: &dyn Fn(usize) -> bool, n: usize, out: &mut dyn FnMut(usize, bool)| {
        let mut pre = vec![0u32; n + 1];
        for i in 0..n {
            pre[i + 1] = pre[i] + get(i) as u32;
        }
        for i in 0..n {
            let (a, b) = (i.saturating_sub(r), (i + r + 1).min(n));
            out(i, pre[b] > pre[a]);
        }
    };
    let mut h = vec![false; m.len()];
    for y in 0..ny {
        run(&|x| m[y * nx + x], nx, &mut |x, v| h[y * nx + x] = v);
    }
    let mut out = vec![false; m.len()];
    for x in 0..nx {
        run(&|y| h[y * nx + x], ny, &mut |y, v| out[y * nx + x] = v);
    }
    out
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
    /// negotiated cell cost per (layer, map cell), INFINITY = blocked; built on the first
    /// negotiated search (an Owners serves one route_net call, so one negotiation state)
    cost: std::sync::OnceLock<Vec<f32>>,
    /// routing intent `iso`: map cells (layer-major) closer than the extra spacing to fixed or
    /// routed copper of other nets; built on the first search that needs it
    iso: std::sync::OnceLock<Vec<bool>>,
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
    /// conflict clusters of the last negotiation iteration (largest first)
    pub hotspots: Vec<Hotspot>,
}

/// a net's conflicting cells in the last negotiation iteration and where they are
#[derive(Debug, Clone)]
pub struct Hotspot {
    pub net: usize,
    pub cells: usize,
    pub x: f64,
    pub y: f64,
    pub layer: &'static str,
}

/// set by SIGTERM / SIGINT / Ctrl+C / Ctrl+Break or a cancel request (--cancel-file,
/// --cancel-on-stdin-eof): the router stops negotiating and writes the best result so far
pub static STOP: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// route SIGTERM and SIGINT (Unix) or Ctrl+C and Ctrl+Break (Windows console) to `STOP`
pub fn install_stop_handler() {
    #[cfg(windows)]
    {
        extern "system" fn on_ctrl(kind: u32) -> i32 {
            // CTRL_C_EVENT = 0, CTRL_BREAK_EVENT = 1; close/logoff/shutdown keep the default
            // (Windows ends the process as soon as the handler returns)
            if kind <= 1 {
                STOP.store(true, std::sync::atomic::Ordering::Relaxed);
                1
            } else {
                0
            }
        }
        #[link(name = "kernel32")]
        extern "system" {
            fn SetConsoleCtrlHandler(
                handler: Option<extern "system" fn(u32) -> i32>,
                add: i32,
            ) -> i32;
        }
        unsafe {
            SetConsoleCtrlHandler(Some(on_ctrl), 1);
        }
    }
    #[cfg(unix)]
    {
        extern "C" fn on_signal(_: i32) {
            STOP.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        extern "C" {
            fn signal(sig: i32, handler: extern "C" fn(i32)) -> usize;
        }
        // SIGINT = 2 and SIGTERM = 15 on Linux and macOS
        unsafe {
            signal(2, on_signal);
            signal(15, on_signal);
        }
    }
}

#[derive(Default, Debug, Clone)]
pub struct AesStats {
    /// collinear overlapping segments merged before writing
    pub merged: usize,
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
    /// nets whose copper failed the exact check and was not written
    pub exact_dropped: usize,
}

/// min-heap of u64 with four children per node: same pop order as a binary heap, fewer levels
/// and cache misses per pop (the A* queue)
#[derive(Default)]
struct QHeap {
    v: Vec<u64>,
}

impl QHeap {
    #[inline]
    fn push(&mut self, e: u64) {
        let mut i = self.v.len();
        self.v.push(e);
        while i > 0 {
            let p = (i - 1) / 4;
            if self.v[p] <= e {
                break;
            }
            self.v[i] = self.v[p];
            i = p;
        }
        self.v[i] = e;
    }
    #[inline]
    fn pop(&mut self) -> Option<u64> {
        let last = self.v.pop()?;
        let n = self.v.len();
        if n == 0 {
            return Some(last);
        }
        let top = self.v[0];
        let mut i = 0;
        loop {
            let c = 4 * i + 1;
            if c >= n {
                break;
            }
            let mut m = c;
            let mut mv = self.v[c];
            for cc in c + 1..(c + 4).min(n) {
                let x = self.v[cc];
                if x < mv {
                    m = cc;
                    mv = x;
                }
            }
            if mv >= last {
                break;
            }
            self.v[i] = mv;
            i = m;
        }
        self.v[i] = last;
        Some(top)
    }
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
/// float guard (mm) of the clearance checks: copper that touches the required clearance exactly is
/// legal (KiCad flags only distances below it). Fine-pitch rows need this: a 0.2 mm track between
/// 0.2 mm pads at 0.4 mm pitch (0.2 mm clearance) fits only exactly. Output is written in 1 nm steps.
pub const EXACT_TOL: f64 = 1e-6;

/// routing grid over `region` whose cell centres are shifted (less than one cell per axis) onto the
/// pad axes of the tightest pin rows: an exit stub and the parallel tracks after it then run on cell
/// centres. A pad votes with its coordinate across its long axis (both for square pads), weighted
/// by 1 / pitch^2 to the nearest pad of its footprint; the default phase (region corner) stays
/// unless another phase gets strictly more weight.
fn phased_grid(m: &Model, region: BBox, res: f64, jobs: &[NetJob]) -> GridSpec {
    let q = 1e-4; // phase bin (mm)
    let nb = (res / q).round() as i64;
    let bin = |phase: f64| ((phase / q).round() as i64).rem_euclid(nb);
    let mut votes: [HashMap<i64, (f64, f64)>; 2] = [HashMap::new(), HashMap::new()];
    for pn in jobs.iter().flat_map(|j| &j.pins) {
        let f = &m.b.fps[pn.fp];
        let pd = &f.pads[pn.pad];
        let (c, a) = f.pad_world(pd);
        let am = a.rem_euclid(90.0);
        if am > 1e-6 && am < 90.0 - 1e-6 {
            continue;
        }
        let pitch = f
            .pads
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != pn.pad)
            .map(|(_, o)| (f.pad_world(o).0 - c).len())
            .filter(|d| *d > 1e-6)
            .fold(f64::INFINITY, f64::min);
        if !pitch.is_finite() {
            continue;
        }
        let w = 1.0 / pitch.max(res).powi(2);
        let (sw, sh) = pd.size;
        let (wx, wy) = if (a.rem_euclid(180.0) - 90.0).abs() < 1.0 {
            (sh, sw)
        } else {
            (sw, sh)
        };
        // [x matters, y matters]: across the long axis of an elongated pad, both for square ones
        let lat = [wx < 1.2 * wy, wy < 1.2 * wx];
        for (ax, coord) in [c.x, c.y].into_iter().enumerate() {
            if lat[ax] {
                let ph = (coord - 0.5 * res).rem_euclid(res);
                votes[ax].entry(bin(ph)).or_insert((0.0, ph)).0 += w;
            }
        }
    }
    let score = |v: &HashMap<i64, (f64, f64)>, b: i64| -> f64 {
        (-1..=1)
            .filter_map(|d| v.get(&(b + d).rem_euclid(nb)))
            .map(|e| e.0)
            .sum()
    };
    let mut o = [region.x0, region.y0];
    for (ax, v) in votes.iter().enumerate() {
        let base = score(v, bin(o[ax].rem_euclid(res)));
        let best = v
            .iter()
            .map(|(b, e)| (score(v, *b), *b, e.1))
            .max_by(|x, y| x.0.total_cmp(&y.0).then(y.1.cmp(&x.1)));
        if let Some((s, _, ph)) = best {
            if s > base + 1e-9 {
                o[ax] -= (o[ax] - ph).rem_euclid(res);
            }
        }
    }
    GridSpec::new(
        BBox {
            x0: o[0],
            y0: o[1],
            x1: region.x1,
            y1: region.y1,
        },
        res,
    )
}

#[inline]
fn tdir(a: u8, b: u8) -> u8 {
    let d = (a as i32 - b as i32).rem_euclid(8) as u8;
    d.min(8 - d)
}

/// blocker maps of the objects that stay while a few footprints move (`nudge`): one window per
/// net (the region plus the job margin) on one grid, so a candidate placement only stamps the
/// pads and mask apertures of the moving footprints into copies of these maps
pub struct FixedMaps {
    pub gs: GridSpec,
    /// footprints (indices into the model's board) whose objects are stamped per candidate
    pub moving: HashSet<usize>,
    nets: HashMap<usize, FixedNet>,
}

struct FixedNet {
    ox: usize,
    oy: usize,
    g: GridSpec,
    t: Vec<Vec<u8>>,
    tn: Vec<Vec<u8>>,
    v: Vec<u8>,
}

fn is_moving(o: &Obj, mv: &HashSet<usize>) -> bool {
    match o.kind {
        Kind::Pad { fp, .. } | Kind::Mask { fp, .. } => mv.contains(&fp),
        _ => false,
    }
}

impl<'a> Router<'a> {
    pub fn new(
        m: &'a Model<'a>,
        region: BBox,
        p: DParams,
        only_nets: Option<&HashSet<usize>>,
    ) -> Router<'a> {
        Self::build(m, region, p, only_nets, None, None)
    }

    /// a router that reuses `fixed` (built by [`Router::fixed_maps`] on the same board with the
    /// moving footprints elsewhere): only the moving footprints' objects are stamped anew
    pub fn new_with(
        m: &'a Model<'a>,
        region: BBox,
        p: DParams,
        only_nets: Option<&HashSet<usize>>,
        fixed: &FixedMaps,
    ) -> Router<'a> {
        Self::build(m, region, p, only_nets, Some(fixed), None)
    }

    /// the maps of everything but the `moving` footprints' pads and mask apertures, per net of
    /// the region (see [`FixedMaps`])
    pub fn fixed_maps(
        m: &Model,
        region: BBox,
        p: DParams,
        only_nets: Option<&HashSet<usize>>,
        moving: HashSet<usize>,
    ) -> FixedMaps {
        let r = Router::build(m, region, p, only_nets, None, Some(&moving));
        let nets = r
            .jobs
            .iter()
            .zip(r.maps)
            .map(|(j, mp)| {
                (
                    j.net,
                    FixedNet {
                        ox: mp.ox,
                        oy: mp.oy,
                        g: mp.g,
                        t: mp.t,
                        tn: mp.tn,
                        v: mp.v,
                    },
                )
            })
            .collect();
        FixedMaps {
            gs: r.gs,
            moving,
            nets,
        }
    }

    fn build(
        m: &'a Model<'a>,
        region: BBox,
        p: DParams,
        only_nets: Option<&HashSet<usize>>,
        fixed: Option<&FixedMaps>,
        exclude: Option<&HashSet<usize>>,
    ) -> Router<'a> {
        // connections: pins inside the region, nets with >= 2 copper components there
        let (pins, copper) = pins_and_copper(m);
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
            if p.copper_comps {
                for &(oi, c) in copper.get(&net).into_iter().flatten() {
                    let o = m.cu(oi);
                    if matches!(o.kind, Kind::Track | Kind::Via) && o.bbox.overlaps(&region) {
                        comps.push(c);
                    }
                }
            }
            comps.sort();
            comps.dedup();
            if comps.len() < 2 {
                continue;
            }
            let (wmin, w) = m.r.track_width(net);
            let (vd, vdr) = m.r.via(net);
            let cu = copper
                .get(&net)
                .map(|v| {
                    v.iter()
                        .filter(|(oi, c)| {
                            comps.binary_search(c).is_ok() && m.cu(*oi).bbox.overlaps(&region)
                        })
                        .map(|&(oi, c)| {
                            let o = m.cu(oi);
                            let via_at = match (o.kind, &o.shape) {
                                (Kind::Via, Shape::Capsule { a, .. }) => Some(*a),
                                _ => None,
                            };
                            Seed {
                                shape: o.shape.clone(),
                                layers: o.layers,
                                comp: c,
                                via_at,
                                fill: matches!(o.kind, Kind::Zone),
                            }
                        })
                        .collect()
                })
                .unwrap_or_default();
            // courtyard neck-down rules of the pins' footprints (issue #2)
            let mut hn = wmin.min(w) / 2.0;
            let mut fps: Vec<usize> = ps.iter().map(|pn| pn.fp).collect();
            fps.sort_unstable();
            fps.dedup();
            let cw: Vec<(usize, f64)> = fps
                .into_iter()
                .filter(|&fi| m.b.fps[fi].crtyd.len() >= 3)
                .map(|fi| {
                    let refs = [m.b.fps[fi].refd.clone()];
                    (fi, m.r.track_width_at(net, Some(&refs)).0)
                })
                .filter(|(_, mn)| *mn < hn * 2.0 - 1e-9)
                .collect();
            let neck_fps: Vec<usize> = cw.iter().map(|(fi, _)| *fi).collect();
            if let Some(mn) = cw.iter().map(|(_, mn)| *mn).reduce(f64::max) {
                hn = mn / 2.0;
            }
            // routing intent: a wider track (capped at 5 mm), never narrower than the class
            let prio = m.cfg.prio(&m.b.nets[net], &m.r.class_of(net).name);
            let w = (w * prio.width).clamp(w.min(wmin), w.max(5.0));
            // controlled impedance: the width from the stackup replaces the class width (still
            // within the board minimum and 5 mm)
            let w = match (prio.z > 0.0).then(|| crate::elec::width_for_z(m.b, prio.z)) {
                Some(Some(zw)) => zw.clamp(wmin, 5.0),
                _ => w,
            };
            jobs.push(NetJob {
                net,
                pins: ps,
                hw: w / 2.0,
                hn,
                neck_fps,
                vr: vd / 2.0,
                dr: vdr / 2.0,
                prio,
                copper: cu,
                comps: comps.clone(),
            });
        }
        let gs = match fixed {
            Some(f) => f.gs,
            None => phased_grid(m, region, p.res, &jobs),
        };
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
        let t = std::time::Instant::now();
        let jm = p.job_margin;
        let ck = Checker::new(m, &objs, maxd);
        let ms: Vec<(NetMaps, Vec<Stub>)> = jobs
            .par_iter()
            .map(|j| {
                let base = fixed.and_then(|f| f.nets.get(&j.net));
                let mut bb = BBox::empty();
                for pn in &j.pins {
                    bb.add(pn.at);
                }
                if exclude.is_some() {
                    // fixed maps: one window for every placement of the moving parts
                    bb = region;
                }
                // existing copper of the components (clipped to the region) can be a start too
                for sd in j.copper.iter().filter(|s| !s.fill) {
                    let cb = sd.shape.bbox();
                    for q in [P { x: cb.x0, y: cb.y0 }, P { x: cb.x1, y: cb.y1 }] {
                        bb.add(geom::p(
                            q.x.clamp(region.x0, region.x1),
                            q.y.clamp(region.y0, region.y1),
                        ));
                    }
                }
                let bb = bb.inflate(jm);
                let ox = (((bb.x0 - gs.x0) / gs.res).floor().max(0.0) as usize).min(gs.nx - 1);
                let oy = (((bb.y0 - gs.y0) / gs.res).floor().max(0.0) as usize).min(gs.ny - 1);
                let ex = (((bb.x1 - gs.x0) / gs.res).ceil().max(0.0) as usize).clamp(ox + 1, gs.nx);
                let ey = (((bb.y1 - gs.y0) / gs.res).ceil().max(0.0) as usize).clamp(oy + 1, gs.ny);
                let mut g = GridSpec {
                    x0: gs.x0 + ox as f64 * gs.res,
                    y0: gs.y0 + oy as f64 * gs.res,
                    res: gs.res,
                    nx: ex - ox,
                    ny: ey - oy,
                };
                let (mut ox, mut oy) = (ox, oy);
                if let Some(nb) = base {
                    (ox, oy, g) = (nb.ox, nb.oy, nb.g);
                }
                let wb = g.bbox().inflate(maxd + 2.0);
                // locked pours of the net's own class (e.g. a guard pour of the same class) refill
                // around this net's copper with their own clearance -> not an obstacle
                let cls = m.r.net_class[j.net];
                let objs: Vec<Obj> = objs
                    .iter()
                    .filter(|o| {
                        o.bbox.overlaps(&wb)
                            && !exclude.is_some_and(|mv| is_moving(o, mv))
                            && !(p.yield_same_class
                                && matches!(o.kind, Kind::Zone)
                                && o.net != 0 // net-less copper never refills
                                && m.r.net_class[o.net] == cls)
                    })
                    .cloned()
                    .collect();
                let objs = &objs;
                let (t, tn, v) = match (base, fixed) {
                    (Some(nb), Some(f)) => {
                        // stamp only the moving footprints into copies of the fixed maps
                        let dynv: Vec<Obj> = objs
                            .iter()
                            .filter(|o| is_moving(o, &f.moving))
                            .cloned()
                            .collect();
                        let (mut t, mut tn, mut v) = (nb.t.clone(), nb.tn.clone(), nb.v.clone());
                        for l in 0..NL {
                            track_stamp(m, &g, j.net, j.hw + p.margin, l, &dynv, &mut t[l]);
                            track_stamp(m, &g, j.net, j.hn + p.margin, l, &dynv, &mut tn[l]);
                        }
                        via_stamp(m, &g, j.net, j.vr + p.margin, j.dr, &dynv, &mut v);
                        (t, tn, v)
                    }
                    _ => (
                        (0..NL)
                            .map(|l| track_map(m, &g, j.net, j.hw + p.margin, l, objs))
                            .collect(),
                        (0..NL)
                            .map(|l| track_map(m, &g, j.net, j.hn + p.margin, l, objs))
                            .collect(),
                        via_map(m, &g, j.net, j.vr + p.margin, j.dr, objs),
                    ),
                };
                let mut neck = vec![false; g.n()];
                // courtyard neck-down rule: narrow width inside those courtyards only
                for &fi in &j.neck_fps {
                    let c = m.b.fps[fi].crtyd_world();
                    g.fill_poly(&c, &mut |x, y| neck[g.idx(x, y)] = true);
                }
                for pn in j.pins.iter().filter(|_| j.neck_fps.is_empty()) {
                    let f = &m.b.fps[pn.fp];
                    for s in f.pad_shapes(&f.pads[pn.pad]) {
                        g.stamp(&s, p.neck, |x, y| neck[g.idx(x, y)] = true);
                    }
                }
                let lmask = (0..NL)
                    .filter(|&l| t[l].contains(&R_FREE) || tn[l].contains(&R_FREE))
                    .fold(0u8, |m, l| m | 1 << l);
                let mut mp = NetMaps {
                    g,
                    ox,
                    oy,
                    t,
                    tn,
                    v,
                    neck,
                    own: vec![0u8; g.n()],
                    lmask,
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
        dlog!(
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
        // layers the net cannot use get no owner map (never read: their own map is blocked)
        let lm = self.maps[k].lmask;
        let per_layer = || -> Vec<Vec<u16>> {
            (0..NL)
                .map(|l| vec![0u16; if lm & (1 << l) != 0 { n } else { 0 }])
                .collect()
        };
        let mut o = Owners {
            t: per_layer(),
            tn: per_layer(),
            v: vec![0u16; n],
            cost: std::sync::OnceLock::new(),
            iso: std::sync::OnceLock::new(),
        };
        let mut lt = per_layer();
        let mut ltn = per_layer();
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
                if lm & (1 << s.l) != 0 {
                    Self::stamp_owner(g, &mut o.t[s.l], &mut lt[s.l], &sh, c + job.hw + mg, id);
                    Self::stamp_owner(g, &mut o.tn[s.l], &mut ltn[s.l], &sh, c + job.hn + mg, id);
                }
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
                    if lm & (1 << l) == 0 {
                        continue;
                    }
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
        // layers without an owner map (the net cannot use them) have no conflicts
        let m = if wide { &o.t[l] } else { &o.tn[l] };
        m.get(w).is_some_and(|&c| c > 0)
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

    /// stop requested by a signal or the deadline has passed
    fn halt(&self) -> bool {
        STOP.load(std::sync::atomic::Ordering::Relaxed)
            || self
                .p
                .deadline
                .is_some_and(|d| std::time::Instant::now() > d)
    }

    /// cells (layer, cell; layer NL = via) where each job's copper overlaps another net's
    fn conflicts(
        &self,
        routed: &[RNet],
        cells: &[Vec<(usize, usize, bool)>],
        vias: &[Vec<usize>],
    ) -> Vec<Vec<(usize, usize)>> {
        (0..self.jobs.len())
            .into_par_iter()
            .map(|k| {
                if cells[k].is_empty() && vias[k].is_empty() {
                    return vec![];
                }
                let o = self.owner_maps(k, routed);
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
            .collect()
    }

    /// one hotspot per conflicting net: its conflict cell count and the median conflict cell
    fn hotspots(&self, conf: &[Vec<(usize, usize)>]) -> Vec<Hotspot> {
        let mut out: Vec<Hotspot> = conf
            .iter()
            .enumerate()
            .filter(|(_, c)| !c.is_empty())
            .map(|(k, c)| {
                let mut v = c.clone();
                v.sort_by_key(|(_, i)| (i % self.gs.nx, i / self.gs.nx));
                let (l, i) = v[v.len() / 2];
                let p = self.gs.center(i % self.gs.nx, i / self.gs.nx);
                Hotspot {
                    net: self.jobs[k].net,
                    cells: c.len(),
                    x: p.x,
                    y: p.y,
                    layer: if l < NL { CU[l] } else { "via" },
                }
            })
            .collect();
        out.sort_by_key(|h| std::cmp::Reverse(h.cells));
        out
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
        // nets with the `first` intent lead, then the shortest nets
        order.sort_by(|a, b| {
            let (ja, jb) = (&self.jobs[*a], &self.jobs[*b]);
            (!ja.prio.first, hp(ja))
                .partial_cmp(&(!jb.prio.first, hp(jb)))
                .unwrap()
        });
        let mut need: Vec<usize> = order.clone();
        let mut pres = self.p.pres0;
        let mut routes = 0;
        let mut it = 0;
        let dbg = std::env::var("DROUTE_DEBUG").is_ok();
        let mut best_hist: Vec<usize> = Vec::new();
        let ck = Checker::new(self.m, &self.objs, self.maxd);
        let mut nst: Vec<AesStats> = vec![AesStats::default(); nj];
        // best iteration so far: (conflict cells, routed, cells, vias, left)
        type Snap = (
            usize,
            Vec<RNet>,
            Vec<Vec<(usize, usize, bool)>>,
            Vec<Vec<usize>>,
            Vec<usize>,
        );
        let mut best: Option<Snap> = None;
        let t_run = std::time::Instant::now();
        let hotspots: Vec<Hotspot>;
        loop {
            let t_it = std::time::Instant::now();
            // nets whose windows cannot see each other's copper route in parallel; a net waits for
            // every earlier net (in `need` order) it can see, so the result equals the serial order
            let waves = self.waves(&need);
            let mut aborted = false;
            for wave in &waves {
                if self.halt() {
                    aborted = true;
                    break;
                }
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
                        dlog!(
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
            let conf = self.conflicts(&routed, &cells, &vias);
            let total: usize = conf.iter().map(|c| c.len()).sum();
            dlog!(
                "[droute] iter {it}: {} nets in {:.1} s, conflict cells {total}, nets in conflict {}, unrouted comps {}",
                need.len(),
                t_it.elapsed().as_secs_f64(),
                conf.iter().filter(|c| !c.is_empty()).count(),
                left.iter().sum::<usize>()
            );
            if self.p.progress {
                let now = std::time::Instant::now();
                let mean = (now - t_run).as_secs_f64() / (it + 1) as f64;
                let iters_left = self.p.nego_iters.saturating_sub(it + 1);
                crate::progress::emit(serde_json::json!({
                    "ev": "iter",
                    "iter": it,
                    "conflict_cells": total,
                    "nets_conflicted": conf.iter().filter(|c| !c.is_empty()).count(),
                    "nets_left": (0..nj).filter(|k| !conf[*k].is_empty() || left[*k] > 0).count(),
                    "nets_total": nj,
                    "iters_max": self.p.nego_iters,
                    "eta_s": if total == 0 { Some(0.0) } else { crate::progress::eta(mean, iters_left, self.p.deadline, now) },
                }));
            }
            if dbg {
                eprintln!("[dbg] nego iter {it}: pres {pres:.2}, {} nets rerouted, conflict cells {total}, nets in conflict {}", need.len(), conf.iter().filter(|c| !c.is_empty()).count());
            }
            best_hist.push(total);
            if !aborted && best.as_ref().is_none_or(|b| total < b.0) {
                best = Some((
                    total,
                    routed.clone(),
                    cells.clone(),
                    vias.clone(),
                    left.clone(),
                ));
            }
            // stop before an iteration that would end after the deadline (estimate: the last one)
            let out_of_time = aborted
                || STOP.load(std::sync::atomic::Ordering::Relaxed)
                || self
                    .p
                    .deadline
                    .is_some_and(|d| std::time::Instant::now() + t_it.elapsed() > d);
            if out_of_time && total > 0 {
                if let Some(b) = best.take() {
                    if b.0 < total {
                        (routed, cells, vias, left) = (b.1, b.2, b.3, b.4);
                        dlog!(
                            "[droute] stopped: back to the best iteration ({} conflict cells)",
                            b.0
                        );
                    }
                }
            }
            let conf = if out_of_time && total > 0 {
                self.conflicts(&routed, &cells, &vias)
            } else {
                conf
            };
            let total: usize = conf.iter().map(|c| c.len()).sum();
            // no new best for `stall_iters` iterations: stop negotiating, rip and reroute hard
            let sw = self.p.stall_iters.max(1);
            let stalled = best_hist.len() > sw
                && (*best_hist[best_hist.len() - sw..].iter().min().unwrap() as f32)
                    >= (1.0 - self.p.stall_gain)
                        * *best_hist[..best_hist.len() - sw].iter().min().unwrap() as f32;
            if total == 0 || it + 1 >= self.p.nego_iters || stalled || out_of_time {
                hotspots = self.hotspots(&conf);
                for h in hotspots.iter().take(10) {
                    dlog!(
                        "[droute] hotspot {}: {} conflict cells around ({:.2}, {:.2}) {}",
                        self.m.b.nets[h.net],
                        h.cells,
                        h.x,
                        h.y,
                        h.layer
                    );
                }
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
                    if self.p.progress {
                        crate::progress::phase("resolve");
                    }
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
                        conf = self.conflicts(&routed, &cells, &vias);
                    }
                    // in waves: nets whose windows cannot see each other reroute in parallel,
                    // with the same result as one after the other in `ripped` order
                    for wave in self.waves(&ripped) {
                        if STOP.load(std::sync::atomic::Ordering::Relaxed) {
                            // signal: no time for hard reroutes, the ripped nets stay unrouted
                            for &k in &wave {
                                left[k] = self.jobs[k].pins.len().saturating_sub(1).max(1);
                            }
                            continue;
                        }
                        let routed_ref = &routed;
                        let rs: Vec<(usize, NetRouteResult, AesStats)> = wave
                            .par_iter()
                            .map(|&k| {
                                let o = self.owner_maps(k, routed_ref);
                                let fr = self.foreign(k, routed_ref);
                                let mut st = AesStats::default();
                                let r = self.route_net(k, &o, None, &ck, &fr, &mut st);
                                (k, r, st)
                            })
                            .collect();
                        for (k, r, st) in rs {
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
        let why: Vec<&'static str> = (0..nj)
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
        let conns = self.jobs.iter().map(|j| j.comps.len() - 1).sum();
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
        if self.p.progress {
            crate::progress::phase("clean");
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
        // collinear segments on top of each other (pad exit stubs over a run of the same net)
        // become one: a covered segment is dropped, an overlapping one of the same width merged;
        // an end lying on the middle of another track would dangle in KiCad
        for (k, rn) in routed.iter_mut().enumerate() {
            let centres: Vec<P> = self.jobs[k]
                .pins
                .iter()
                .map(|pn| {
                    let f = &self.m.b.fps[pn.fp];
                    f.pad_world(&f.pads[pn.pad]).0
                })
                .collect();
            stats.merged += merge_collinear(&mut rn.segs, &centres);
        }
        // exact verification of all routed copper (fixed objects + other nets)
        let fails: Vec<(usize, usize, String)> = (0..nj)
            .into_par_iter()
            .flat_map(|k| {
                let fr = self.foreign(k, &routed);
                let job = &self.jobs[k];
                let mut out = Vec::new();
                for (si, sg) in routed[k].segs.iter().enumerate() {
                    let hw = sg.w / 2.0 - EXACT_TOL;
                    if !ck.seg_ok_fixed(job.net, sg.a, sg.b, hw, sg.l)
                        || !ck.seg_ok_foreign(job.net, sg.a, sg.b, hw, sg.l, &fr)
                    {
                        out.push((
                            k,
                            si,
                            format!(
                                "{} {} ({:.3},{:.3})-({:.3},{:.3}) w {}",
                                self.m.b.nets[job.net],
                                CU[sg.l],
                                sg.a.x,
                                sg.a.y,
                                sg.b.x,
                                sg.b.y,
                                sg.w
                            ),
                        ));
                    }
                }
                out
            })
            .collect();
        stats.exact_fail = fails.len();
        if dbg {
            for (_, _, f) in &fails {
                eprintln!("[dbg] exact check fails: {f}");
            }
        }
        // never write copper that fails the exact check: the failing segments are removed, then
        // whatever dangles from them; the rest of the net stays (its other connections hold)
        let mut failed = failed;
        let mut why = why;
        let mut bad: Vec<usize> = fails.iter().map(|(k, _, _)| *k).collect();
        bad.sort();
        bad.dedup();
        for &k in &bad {
            let segs: Vec<usize> = fails
                .iter()
                .filter(|(kk, _, _)| *kk == k)
                .map(|(_, si, _)| *si)
                .collect();
            let before = routed[k].segs.len();
            self.prune_failed(k, &mut routed[k], &segs);
            eprintln!(
                "[route] {}: {} segment(s) fail the exact clearance check; removed with what dangled ({} of {} segments kept, connection left open)",
                self.m.b.nets[self.jobs[k].net],
                segs.len(),
                routed[k].segs.len(),
                before
            );
            if !failed.iter().any(|(n, _)| *n == self.jobs[k].net) {
                failed.push((self.jobs[k].net, 1));
                why.push("exact-check");
            }
        }
        stats.exact_dropped = bad.len();
        // T-junctions become end points (what is written; the delta lists the same segments)
        for rn in routed.iter_mut() {
            rn.segs = split_tees(&rn.segs);
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
            hotspots,
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
        let comp_ids: Vec<usize> = job.comps.clone();
        let nc = comp_ids.len();
        let stubs = &self.stubs[k];
        // usable stubs and their start cost
        let mut s_cost = vec![f32::INFINITY; stubs.len()];
        for (si, s) in stubs.iter().enumerate() {
            let mut c = s.cost;
            let mut ok = true;
            for &ci in &s.cells {
                let Some(w) = self.wi(k, ci) else { continue };
                let m = if s.wide { &o.t[s.l] } else { &o.tn[s.l] };
                let own = m.get(w).copied().unwrap_or(0);
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
        // existing tracks and vias of each component: start/target cells too (a pad boxed in by
        // other copper still joins through its own via or a dangling track end)
        for sd in &job.copper {
            let Ok(ci) = comp_ids.binary_search(&sd.comp) else {
                continue;
            };
            for l in 0..NL {
                if sd.layers & (1 << l) == 0 {
                    continue;
                }
                let mut add = |i: usize| {
                    if self.wi(k, i).is_some() && ok(l, i) && !node_stub.contains_key(&(l, i)) {
                        comp_cells[ci].push((l, i));
                        // a via end is pulled to the via centre (like a pad end)
                        if let Some(c) = sd.via_at {
                            pad_of.entry((l, i)).or_insert(c);
                        }
                    }
                };
                match (&sd.shape, sd.fill) {
                    (Shape::Poly { pts, .. }, true) => {
                        // fill: window cells only; rim + every 4th interior cell
                        let g = mp.g;
                        let gi = |x: usize, y: usize| (mp.oy + y) * self.gs.nx + mp.ox + x;
                        g.fill_poly(pts, &mut |x, y| {
                            if x % 4 == 0 && y % 4 == 0 {
                                add(gi(x, y));
                            }
                        });
                        let n = pts.len();
                        for e in 0..n {
                            let rim = Shape::Capsule {
                                a: pts[e],
                                b: pts[(e + 1) % n],
                                r: 0.0,
                            };
                            g.stamp(&rim, g.res * 0.75, |x, y| add(gi(x, y)));
                        }
                    }
                    _ => self.gs.stamp(&sd.shape, 0.0, |x, y| add(self.gs.idx(x, y))),
                }
            }
        }
        for cc in comp_cells.iter_mut() {
            cc.sort_unstable();
            cc.dedup();
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
        // grow the tree from the largest component; if nothing can be joined to it (an isolated
        // start pin), retry from the next components so the rest of the net is still routed
        let mut starts: Vec<usize> = (0..nc).filter(|c| !comp_cells[*c].is_empty()).collect();
        // ties: the last component first, as max_by_key picked it before
        starts.sort_by_key(|c| std::cmp::Reverse((comp_cells[*c].len(), *c)));
        starts.truncate(3);
        if starts.is_empty() {
            starts.push(0);
        }
        // large windows: components the search cannot reach are known before it expands the
        // whole window (2-pin nets across a board can take minutes to fail otherwise)
        let grp: Option<Vec<u16>> = (n * NL > FLOOD_MIN_NODES && nc > 1)
            .then(|| self.comp_groups(k, &comp_cells, o, nego.is_none()));
        let mut best: Option<NetRouteResult> = None;
        let mut best_in_tree = vec![false; nc];
        for &start in &starts {
            let mut used: Vec<Option<P>> = vec![None; stubs.len()];
            let mut tmark = vec![0u16; n * NL];
            let mut tree_mark = vec![false; n * NL];
            let mut tinfo: HashMap<usize, (f32, u8)> = HashMap::new();
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
            if let Some(g) = &grp {
                for c in 0..nc {
                    if g[c] != g[start] && !comp_cells[c].is_empty() {
                        dead[c] = true;
                        self.why[k].store(3, std::sync::atomic::Ordering::Relaxed);
                    }
                }
            }
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
                                        (x0, y0, x1, y1) =
                                            (x0.min(x), y0.min(y), x1.max(x), y1.max(y));
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
                    k, o, nego, &path, &pad_of, &node_stub, &mut used, &mut rn, &mut cells,
                    &mut vias, ck, foreign, st,
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
            if best.as_ref().is_none_or(|b| left < b.1) {
                best = Some((rn, left, cells, vias));
                best_in_tree = in_tree;
            }
            if left + 1 < nc || left == 0 {
                break; // joined something (or done): keep this tree
            }
        }
        let (rn, left, cells, vias) = best.unwrap_or_else(|| {
            (
                RNet::default(),
                nc.saturating_sub(1),
                Vec::new(),
                Vec::new(),
            )
        });
        let in_tree = best_in_tree;
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
    /// reachability groups of a net's components in its job window: a flood over the cells the
    /// search may enter (fixed obstacles; with `hard` also other nets' copper), 8 neighbours and
    /// vias, without costs or turn limits, so it reaches at least what the search can. Components
    /// in different groups cannot be joined; the search would only find that out after
    /// expanding the whole window.
    fn comp_groups(
        &self,
        k: usize,
        comp_cells: &[Vec<(usize, usize)>],
        o: &Owners,
        hard: bool,
    ) -> Vec<u16> {
        let mp = &self.maps[k];
        let (nx, ny) = (mp.g.nx, mp.g.ny);
        let n = nx * ny;
        let free = |l: usize, w: usize| -> bool {
            let ow = |m: &Vec<u16>| !hard || m.get(w).is_none_or(|&c| c == 0);
            (mp.t[l][w] == R_FREE && ow(&o.t[l]))
                || (mp.neck[w] && mp.tn[l][w] == R_FREE && ow(&o.tn[l]))
        };
        let via = |w: usize| mp.v[w] == R_FREE && (!hard || o.v[w] == 0);
        let mut grp = vec![u16::MAX; comp_cells.len()];
        let mut next = 0u16;
        let mut seen = vec![0u8; n];
        for c in 0..comp_cells.len() {
            if grp[c] != u16::MAX || comp_cells[c].is_empty() {
                continue;
            }
            seen.iter_mut().for_each(|v| *v = 0);
            let mut stack: Vec<(usize, usize)> = Vec::new();
            for &(l, i) in &comp_cells[c] {
                if let Some(w) = self.wi(k, i) {
                    if l < NL && seen[w] & (1 << l) == 0 {
                        seen[w] |= 1 << l;
                        stack.push((l, w));
                    }
                }
            }
            while let Some((l, w)) = stack.pop() {
                let (x, y) = ((w % nx) as i64, (w / nx) as i64);
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
                    let (xx, yy) = (x + dx, y + dy);
                    if xx < 0 || yy < 0 || xx >= nx as i64 || yy >= ny as i64 {
                        continue;
                    }
                    let wj = yy as usize * nx + xx as usize;
                    if seen[wj] & (1 << l) == 0 && free(l, wj) {
                        seen[wj] |= 1 << l;
                        stack.push((l, wj));
                    }
                }
                if via(w) {
                    for l2 in 0..NL {
                        if seen[w] & (1 << l2) == 0 && free(l2, w) {
                            seen[w] |= 1 << l2;
                            stack.push((l2, w));
                        }
                    }
                }
            }
            for d in 0..comp_cells.len() {
                if grp[d] == u16::MAX
                    && (d == c
                        || comp_cells[d].iter().any(|&(l, i)| {
                            l < NL && self.wi(k, i).is_some_and(|w| seen[w] & (1 << l) != 0)
                        }))
                {
                    grp[d] = next;
                }
            }
            next = next.saturating_add(1);
        }
        grp
    }

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

    /// remove the segments `bad` of a routed net, then every segment with an end that touches no
    /// other segment, via, pad of the net or existing copper of its pins, and every via with fewer
    /// than two attachments (no dangling copper is written)
    fn prune_failed(&self, k: usize, rn: &mut RNet, bad: &[usize]) {
        let job = &self.jobs[k];
        let mut keep: Vec<bool> = (0..rn.segs.len()).map(|i| !bad.contains(&i)).collect();
        let mut vkeep = vec![true; rn.vias.len()];
        let pads: Vec<(u8, Shape)> = job
            .pins
            .iter()
            .flat_map(|pn| {
                let f = &self.m.b.fps[pn.fp];
                f.pad_shapes(&f.pads[pn.pad])
                    .into_iter()
                    .map(move |sh| (pn.layers, sh))
            })
            .collect();
        let fixed = |q: P, l: usize| {
            pads.iter()
                .any(|(ls, sh)| ls & (1 << l) != 0 && sh.dist(q) <= 1e-6)
                || job
                    .copper
                    .iter()
                    .any(|sd| sd.layers & (1 << l) != 0 && sd.shape.dist(q) <= 1e-6)
        };
        loop {
            let mut changed = false;
            for i in 0..rn.segs.len() {
                if !keep[i] {
                    continue;
                }
                let s = &rn.segs[i];
                let anchored =
                    |q: P| {
                        fixed(q, s.l)
                            || rn.segs.iter().enumerate().any(|(j, t)| {
                                j != i
                                    && keep[j]
                                    && t.l == s.l
                                    && dist_seg(q, t.a, t.b) <= t.w.max(s.w) / 2.0 + 1e-6
                            })
                            || rn.vias.iter().enumerate().any(|(v, via)| {
                                vkeep[v] && (q - via.at).len() <= via.d / 2.0 + 1e-6
                            })
                    };
                if !(anchored(s.a) && anchored(s.b)) {
                    keep[i] = false;
                    changed = true;
                }
            }
            for (v, via) in rn.vias.iter().enumerate() {
                if !vkeep[v] {
                    continue;
                }
                let segs = rn
                    .segs
                    .iter()
                    .enumerate()
                    .filter(|(j, t)| keep[*j] && dist_seg(via.at, t.a, t.b) <= via.d / 2.0 + 1e-6)
                    .count();
                let fx = (0..NL).filter(|&l| fixed(via.at, l)).count().min(1);
                if segs + fx < 2 {
                    vkeep[v] = false;
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
        let mut i = 0;
        rn.segs.retain(|_| {
            i += 1;
            keep[i - 1]
        });
        let mut v = 0;
        rn.vias.retain(|_| {
            v += 1;
            vkeep[v - 1]
        });
    }

    /// [`Self::astar_win_in`] with this thread's search buffers (reused across searches, so a
    /// search costs the nodes it touches, not the size of its window)
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
        let mut sc = SCRATCH.with(|c| c.take());
        let r = self.astar_win_in(
            k, src, src_info, tmark, tinfo, tree_mark, bbs, o, nego, win, &mut sc,
        );
        SCRATCH.with(|c| c.replace(sc));
        r
    }

    #[allow(clippy::too_many_arguments)]
    fn astar_win_in(
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
        sc: &mut Scratch,
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
        sc.reset(total, wn);
        let Scratch {
            g,
            par,
            indir,
            fkey,
            hc,
            touched,
            htouched,
        } = sc;
        // g, par, indir: cost, parent and incoming direction per node; fkey: key of the newest
        // heap entry per node (older entries are stale and skipped: lazy deletion); `touched`
        // lists the nodes written, which the next search resets
        // heuristic per (x, y), shared by all layers, computed on first use
        let hc = std::cell::RefCell::new((hc, htouched));
        let h = |x: i32, y: i32| -> f32 {
            let ci = ((y - y0) as usize) * wx + (x - x0) as usize;
            let mut b = hc.borrow_mut();
            let (hc, ht) = &mut *b;
            if !hc[ci].is_nan() {
                return hc[ci];
            }
            ht.push(ci as u32);
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
        // heap entry: key in the high half, window node in the low half (ties: lower node first)
        let mut heap = QHeap::default();
        let ent = |fk: u32, ln: usize| ((fk as u64) << 32) | ln as u64;
        let key = |f: f32| (f * 64.0) as u32;
        for &(l, i) in src {
            if !inside(i) {
                continue;
            }
            let (c0, d0) = src_info.get(&(l, i)).cloned().unwrap_or((0.0, 8));
            let ln = loc(l, i);
            if c0 < g[ln] {
                if g[ln] == f32::INFINITY {
                    touched.push(ln as u32);
                }
                g[ln] = c0;
                indir[ln] = d0;
                fkey[ln] = key(c0 + self.p.hweight * h((i as i32) % nxg, (i as i32) / nxg));
                heap.push(ent(fkey[ln], ln));
            }
        }
        let dirpref = &self.m.cfg.layer_dir;
        let opc = self.p.own_pad_cost;
        // the search window lies inside the net's map: w = map index of global cell i
        // negotiated cost of every cell of the net's map, computed once per route_net call: one
        // load per neighbour instead of seven
        let mn = mp.g.n();
        // routing intent iso: cells within the extra spacing of blocked or other nets' cells
        let piso = self.jobs[k].prio.iso;
        let isom: &[bool] = if piso > 0.0 {
            o.iso.get_or_init(|| {
                let r = (piso / self.p.res).round().max(1.0) as usize;
                let (nx, ny) = (mp.g.nx, mp.g.ny);
                let mut out = vec![false; NL * mn];
                for l in 0..NL {
                    let occ: Vec<bool> = (0..mn)
                        .map(|w| mp.t[l][w] != R_FREE || o.t[l][w] > 0)
                        .collect();
                    let near = dilate(&occ, nx, ny, r);
                    for w in 0..mn {
                        out[l * mn + w] = near[w] && !occ[w];
                    }
                }
                out
            })
        } else {
            &[]
        };
        let iso_at = |l: usize, w: usize| -> f32 {
            if !isom.is_empty() && isom[l * mn + w] {
                ISO_COST
            } else {
                0.0
            }
        };
        let cc: &[f32] = match nego {
            Some((pres, hist)) => o.cost.get_or_init(|| {
                let mut cc = vec![f32::INFINITY; NL * mn];
                let mnx = mp.g.nx;
                for l in 0..NL {
                    for my in 0..mp.g.ny {
                        let row = (my + mp.oy) * self.gs.nx + mp.ox;
                        for mx in 0..mnx {
                            let w = my * mnx + mx;
                            let fw = mp.t[l][w] == R_FREE;
                            let fnr = mp.neck[w] && mp.tn[l][w] == R_FREE;
                            if !fw && !fnr {
                                continue;
                            }
                            let own = if mp.own[w] & (1 << l) != 0 { opc } else { 0.0 };
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
                            cc[l * mn + w] = cw.min(cn) + hist[l][row + mx] + own + iso_at(l, w);
                        }
                    }
                }
                cc
            }),
            None => &[],
        };
        let cell_ok = |l: usize, w: usize| -> (bool, f32) {
            match nego {
                Some(_) => {
                    let c = cc[l * mn + w];
                    (c != f32::INFINITY, c)
                }
                None => {
                    let own = if mp.own[w] & (1 << l) != 0 { opc } else { 0.0 };
                    let own = own + iso_at(l, w);
                    if mp.t[l][w] == R_FREE && o.t[l][w] == 0 {
                        (true, own)
                    } else if mp.neck[w] && mp.tn[l][w] == R_FREE && o.tn[l][w] == 0 {
                        (true, self.p.narrow_cost + own)
                    } else {
                        (false, 0.0)
                    }
                }
            }
        };
        // routing intent of this net scales the bend, direction and via costs
        let pr = self.jobs[k].prio;
        let (bend_cost, wrong_dir, via_cost) = (
            self.p.bend_cost * pr.bend,
            self.p.wrong_dir * pr.dir,
            self.p.via_cost * pr.via,
        );
        let bend = |t: u8| -> f32 {
            match t {
                0 => 0.0,
                1 => bend_cost,
                _ => bend_cost * self.p.bend90,
            }
        };
        let mut expanded = 0u64;
        let mut found = None;
        let hw = self.p.hweight;
        // node cap: the configured value, at least half the nodes of the job window but at most
        // three times the configured value (a board-long net that needs more is boxed in; failing
        // after tens of millions of nodes cost up to a minute per search)
        let cap = self
            .p
            .max_expand
            .max(((n * NL / 2) as u64).min(3 * self.p.max_expand));
        let (jx0, jy0) = (mp.ox as i32, mp.oy as i32);
        let (jx1, jy1) = (jx0 + mp.g.nx as i32 - 1, jy0 + mp.g.ny as i32 - 1);
        let clipped = mp.g.nx < self.gs.nx || mp.g.ny < self.gs.ny;
        let mut edge = false;
        let (mox, moy, mnx) = (mp.ox as i32, mp.oy as i32, mp.g.nx as i32);
        while let Some(e) = heap.pop() {
            let (fk, ln) = ((e >> 32) as u32, (e & 0xffff_ffff) as usize);
            if fk != fkey[ln] {
                continue;
            }
            fkey[ln] = u32::MAX - 1; // expanded; a later improvement pushes it again
            let l = ln / wn;
            let r = ln - l * wn;
            let ry = r / wx;
            let (x, y) = ((r - ry * wx) as i32 + x0, ry as i32 + y0);
            let wv = ((y - moy) * mnx + (x - mox)) as usize;
            let node = l * n + wv;
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
                let wj = ((yy - moy) * mnx + (xx - mox)) as usize;
                let (okj, extra) = cell_ok(l, wj);
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
                        c *= 1.0 + wrong_dir;
                    } else if *dx != 0 && *dy != 0 {
                        c *= 1.0 + 0.5 * wrong_dir;
                    }
                }
                c += bend(t) + extra;
                let nj = l * n + wj;
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
                    if g[nn] == f32::INFINITY {
                        touched.push(nn as u32);
                    }
                    g[nn] = ng;
                    par[nn] = ln as u32;
                    indir[nn] = d as u8;
                    fkey[nn] = key(ng + hw * h(xx, yy));
                    heap.push(ent(fkey[nn], nn));
                }
            }
            // through via: legal for this net's via, landing cell must be legal on the new layer
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
                    let (okj, extra) = cell_ok(l2, wv);
                    if !okj {
                        continue;
                    }
                    let nn = l2 * wn + r;
                    let nj = l2 * n + wv;
                    let mut tc = 0.0;
                    if tmark[nj] != 0 && !tree_mark[nj] {
                        if let Some(&(c2, _)) = tinfo.get(&nj) {
                            tc = c2;
                        }
                    }
                    let ng = gc + via_cost + pen + extra + tc;
                    if ng < g[nn] {
                        if g[nn] == f32::INFINITY {
                            touched.push(nn as u32);
                        }
                        g[nn] = ng;
                        par[nn] = ln as u32;
                        indir[nn] = 8;
                        fkey[nn] = key(ng + hw * h(x, y));
                        heap.push(ent(fkey[nn], nn));
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
    write_copper(text, nets, &res.routed, out)
}

/// append new copper per net (unlocked) to the board text and write it to `out`
pub fn write_copper(
    text: &str,
    nets: &[String],
    routed: &HashMap<usize, RNet>,
    out: &str,
) -> std::io::Result<()> {
    std::fs::write(
        out,
        copper_text(text, nets, routed).map_err(std::io::Error::other)?,
    )
}

/// the board text with the routed copper appended (what `write_pcb` writes)
pub fn pcb_text(text: &str, nets: &[String], res: &DResult) -> Result<String, String> {
    copper_text(text, nets, &res.routed)
}

/// a cut of [`split_tees`]: position along the segment, cut point, optional connector (end, width)
type Cut = (f64, P, Option<(P, f64)>);

/// segments with every T-junction made an end point: a segment whose interior holds an end of
/// another segment on its layer, more than half a width from its ends, is split there
/// (kicad-cli counts a track end in the middle of another track as dangling)
pub fn split_tees(segs: &[RSeg]) -> Vec<RSeg> {
    const TOL: f64 = 1e-4;
    // exact duplicates (either direction) are written once
    let mut uniq: Vec<RSeg> = Vec::with_capacity(segs.len());
    for s in segs {
        let dup = uniq.iter().any(|u| {
            u.l == s.l
                && ((u.a - s.a).len() <= TOL && (u.b - s.b).len() <= TOL
                    || (u.a - s.b).len() <= TOL && (u.b - s.a).len() <= TOL)
        });
        if !dup {
            uniq.push(s.clone());
        }
    }
    let segs = &uniq;
    let ends: Vec<(P, usize, f64)> = segs
        .iter()
        .flat_map(|s| [(s.a, s.l, s.w), (s.b, s.l, s.w)])
        .collect();
    let mut out = Vec::with_capacity(segs.len());
    for s in segs {
        let d = s.b - s.a;
        let len = d.len();
        // interior points of this segment that are ends of others, ordered along it: (position
        // along the segment, cut point, the other end when it is off the centre line by up to a
        // quarter width, joined to the cut by a short connector, and its width)
        let off = (s.w / 4.0).max(TOL);
        let mut cut: Vec<Cut> = ends
            .iter()
            .filter(|(q, l, _)| *l == s.l && len > 2.0 * TOL && dist_seg(*q, s.a, s.b) <= off)
            .map(|&(q, _, w)| {
                let t = (q - s.a).dot(d) / len;
                let on = s.a + d * (t / len);
                if (q - on).len() <= TOL {
                    (t, q, None)
                } else {
                    (t, on, Some((q, w.min(s.w))))
                }
            })
            // a junction within half a width of an end already meets that end (no slivers)
            .filter(|&(t, _, _)| t > (s.w / 2.0).max(TOL) && t < len - (s.w / 2.0).max(TOL))
            .collect();
        cut.sort_by(|x, y| x.0.total_cmp(&y.0));
        cut.dedup_by(|x, y| (x.0 - y.0).abs() <= TOL);
        let mut a = s.a;
        for (_, q, conn) in cut {
            out.push(RSeg {
                a,
                b: q,
                l: s.l,
                w: s.w,
            });
            if let Some((e, w)) = conn {
                out.push(RSeg {
                    a: q,
                    b: e,
                    l: s.l,
                    w,
                });
            }
            a = q;
        }
        out.push(RSeg {
            a,
            b: s.b,
            l: s.l,
            w: s.w,
        });
    }
    out
}

/// the board text with new copper per net (unlocked) appended
pub fn copper_text(
    text: &str,
    nets: &[String],
    routed: &HashMap<usize, RNet>,
) -> Result<String, String> {
    let format = crate::sexpr::NetFormat::from_root(&crate::sexpr::parse(text)?)?;
    // seed from the board text: copper added to an output of an earlier run gets new uuids
    let mut seed = 0x1234_5678_9abc_def1u64;
    for b in text.bytes().step_by(97).chain(text.len().to_le_bytes()) {
        seed = (seed ^ b as u64).wrapping_mul(0x100_0000_01b3);
    }
    seed |= 1;
    let mut add = String::new();
    let mut keys: Vec<&usize> = routed.keys().collect();
    keys.sort();
    for &net in keys {
        let segs = split_tees(&routed[&net].segs);
        let rn = &routed[&net];
        let name = format.expression(&nets[net])?;
        for s in &segs {
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
    Ok(o)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn t_junction_becomes_an_end_point() {
        let seg = |a: (f64, f64), b: (f64, f64)| RSeg {
            a: P { x: a.0, y: a.1 },
            b: P { x: b.0, y: b.1 },
            l: 0,
            w: 0.2,
        };
        // a branch ends on the middle of a straight run: the run is split there
        let out = split_tees(&[seg((0.0, 0.0), (4.0, 0.0)), seg((1.5, 0.0), (1.5, 2.0))]);
        assert_eq!(out.len(), 3);
        assert!(out.iter().any(|s| s.a.x == 0.0 && s.b.x == 1.5));
        assert!(out
            .iter()
            .any(|s| s.a.x == 1.5 && s.b.x == 4.0 && s.b.y == 0.0));
        // ends meeting at end points are left alone
        let out = split_tees(&[seg((0.0, 0.0), (1.0, 0.0)), seg((1.0, 0.0), (1.0, 1.0))]);
        assert_eq!(out.len(), 2);
        // an end slightly off the centre line: split at its projection plus a connector
        let out = split_tees(&[seg((0.0, 0.0), (4.0, 0.0)), seg((2.0, 0.03), (2.0, 2.0))]);
        assert_eq!(out.len(), 4);
        assert!(out
            .iter()
            .any(|s| s.a.x == 2.0 && s.a.y == 0.0 && s.b.y == 0.03));
        // exact duplicates are written once
        let out = split_tees(&[seg((0.0, 0.0), (1.0, 0.0)), seg((1.0, 0.0), (0.0, 0.0))]);
        assert_eq!(out.len(), 1);
    }

    #[test]
    fn collinear_overlaps_merge() {
        let s = |ax, ay, bx, by| RSeg {
            a: p(ax, ay),
            b: p(bx, by),
            l: 0,
            w: 0.2,
        };
        // a stub lying on a run: dropped
        let mut v = vec![s(0.0, 0.0, 10.0, 0.0), s(3.0, 0.0, 4.0, 0.0)];
        assert_eq!(merge_collinear(&mut v, &[]), 1);
        assert_eq!(v.len(), 1);
        // partial overlap: one segment over the union
        let mut v = vec![s(0.0, 0.0, 5.0, 0.0), s(4.0, 0.0, 8.0, 0.0)];
        assert_eq!(merge_collinear(&mut v, &[]), 1);
        assert!((v[0].b - v[0].a).len() > 7.99);
        // end to end: unchanged
        let mut v = vec![s(0.0, 0.0, 5.0, 0.0), s(5.0, 0.0, 8.0, 0.0)];
        assert_eq!(merge_collinear(&mut v, &[]), 0);
        // a pad exit ending on a pad centre stays
        let mut v = vec![s(0.0, 0.0, 10.0, 0.0), s(3.0, 0.0, 4.0, 0.0)];
        assert_eq!(merge_collinear(&mut v, &[p(3.0, 0.0)]), 0);
    }

    #[test]
    fn qheap_pops_in_ascending_order() {
        let mut h = QHeap::default();
        let mut want = Vec::new();
        let mut x = 12345u64;
        for _ in 0..1000 {
            x = x
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            let e = x >> 40; // includes duplicates
            h.push(e);
            want.push(e);
            if e.is_multiple_of(3) {
                want.sort_unstable();
                assert_eq!(h.pop(), Some(want.remove(0)));
            }
        }
        want.sort_unstable();
        let got: Vec<u64> = std::iter::from_fn(|| h.pop()).collect();
        assert_eq!(got, want);
    }

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
