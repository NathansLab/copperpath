//! Coarse global router: PathFinder negotiated congestion on a tile grid (default 1 mm) x 6 layers.
//! Capacity per tile edge and layer comes from the fine track-site maps of the net's class (free
//! boundary length / track pitch); layer changes need a via site of the class inside the tile.
//! Each iteration routes all two-pin connections in parallel against the previous iteration's
//! costs (Jacobi PathFinder), then updates present/history congestion.

use crate::board::*;
use crate::geom::*;
use crate::model::*;
use crate::sites::*;
use rayon::prelude::*;
use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap};

pub struct GParams {
    pub tile: f64,
    pub iters: usize,
    pub via_cost: f64,
    pub dir_cost: f64,
    pub margin_tiles: i64,
}

impl Default for GParams {
    fn default() -> Self {
        GParams {
            tile: 1.0,
            iters: 12,
            via_cost: 3.0,
            dir_cost: 2.0,
            margin_tiles: 6,
        }
    }
}

#[derive(Clone)]
pub struct Conn {
    pub net: usize,
    pub a: (usize, usize, u8), // tile x, y, layer mask
    pub b: (usize, usize, u8),
    pub pa: P,
    pub pb: P,
}

pub struct GResult {
    pub tx: usize,
    pub ty: usize,
    pub tile: f64,
    pub x0: f64,
    pub y0: f64,
    /// [layer][edge] used mm and capacity mm for horizontal (x -> x+1) and vertical (y -> y+1) edges
    pub h_used: Vec<Vec<f32>>,
    pub v_used: Vec<Vec<f32>>,
    pub h_cap: Vec<Vec<f32>>,
    pub v_cap: Vec<Vec<f32>>,
    pub via_used: Vec<f32>,
    pub via_cap: Vec<f32>,
    pub paths: Vec<Vec<(usize, usize, usize)>>,
    pub conns: Vec<Conn>,
    pub failed: Vec<usize>,
    pub overflow_hist: Vec<f64>,
    /// nets per edge for hotspot attribution: (layer, is_h, edge) -> nets
    pub edge_nets: HashMap<(usize, bool, usize), Vec<usize>>,
}

#[derive(Copy, Clone, PartialEq)]
struct St {
    f: f64,
    g: f64,
    n: u32,
}
impl Eq for St {}
impl Ord for St {
    fn cmp(&self, o: &Self) -> Ordering {
        o.f.partial_cmp(&self.f).unwrap_or(Ordering::Equal)
    }
}
impl PartialOrd for St {
    fn partial_cmp(&self, o: &Self) -> Option<Ordering> {
        Some(self.cmp(o))
    }
}

struct ClassCap {
    h: Vec<Vec<f32>>, // free length mm per layer per h-edge
    v: Vec<Vec<f32>>,
    vias: Vec<f32>, // via-site area (mm^2) per tile
    pitch: f32,
}

pub fn route(m: &Model, sm: &SiteMaps, pins: &HashMap<usize, Vec<Pin>>, gp: &GParams) -> GResult {
    let gs = sm.gs;
    let k = (gp.tile / gs.res).round().max(1.0) as usize; // fine cells per tile
    let tile = k as f64 * gs.res;
    let tx = gs.nx / k;
    let ty = gs.ny / k;
    let nt = tx * ty;
    let hid = |x: usize, y: usize| y * (tx - 1) + x;
    let vid = |x: usize, y: usize| y * tx + x;
    let nh = (tx - 1) * ty;
    let nv = tx * (ty - 1);
    // capacities per class from fine maps
    let caps: HashMap<usize, ClassCap> = sm
        .cls
        .par_iter()
        .map(|cs| {
            let mut h = vec![vec![0f32; nh]; NL];
            let mut v = vec![vec![0f32; nv]; NL];
            for l in 0..NL {
                let tm = &cs.track[l];
                for y in 0..ty {
                    for x in 0..tx - 1 {
                        let fx = (x + 1) * k;
                        let mut c = 0;
                        for r in y * k..(y + 1) * k {
                            if tm[gs.idx(fx - 1, r)] == R_FREE && tm[gs.idx(fx, r)] == R_FREE {
                                c += 1;
                            }
                        }
                        h[l][hid(x, y)] = c as f32 * gs.res as f32;
                    }
                }
                for y in 0..ty - 1 {
                    for x in 0..tx {
                        let fy = (y + 1) * k;
                        let mut c = 0;
                        for q in x * k..(x + 1) * k {
                            if tm[gs.idx(q, fy - 1)] == R_FREE && tm[gs.idx(q, fy)] == R_FREE {
                                c += 1;
                            }
                        }
                        v[l][vid(x, y)] = c as f32 * gs.res as f32;
                    }
                }
            }
            let mut vias = vec![0f32; nt];
            for y in 0..ty {
                for x in 0..tx {
                    let mut c = 0;
                    for r in y * k..(y + 1) * k {
                        for q in x * k..(x + 1) * k {
                            if cs.via[gs.idx(q, r)] == R_FREE {
                                c += 1;
                            }
                        }
                    }
                    vias[y * tx + x] = c as f32 * (gs.res * gs.res) as f32; // via-site area mm^2
                }
            }
            let pitch = (cs.track_hw * 2.0 + m.r.classes[cs.class].clearance) as f32;
            (cs.class, ClassCap { h, v, vias, pitch })
        })
        .collect();
    // shared capacity = most permissive class
    let mut h_cap = vec![vec![0f32; nh]; NL];
    let mut v_cap = vec![vec![0f32; nv]; NL];
    let mut via_cap = vec![0f32; nt];
    for cc in caps.values() {
        for l in 0..NL {
            for (cap, &f) in h_cap[l].iter_mut().zip(&cc.h[l]).take(nh) {
                if f > 0.0 {
                    *cap = cap.max(f + cc.pitch);
                }
            }
            for (cap, &f) in v_cap[l].iter_mut().zip(&cc.v[l]).take(nv) {
                if f > 0.0 {
                    *cap = cap.max(f + cc.pitch);
                }
            }
        }
        for (cap, &f) in via_cap.iter_mut().zip(&cc.vias).take(nt) {
            *cap = cap.max(f);
        }
    }
    // connections
    let tile_of = |q: P| -> (usize, usize) {
        let x = (((q.x - gs.x0) / tile).floor().max(0.0) as usize).min(tx - 1);
        let y = (((q.y - gs.y0) / tile).floor().max(0.0) as usize).min(ty - 1);
        (x, y)
    };
    let mut conns = Vec::new();
    let mut nets: Vec<&usize> = pins.keys().collect();
    nets.sort();
    for &net in nets {
        let ps = &pins[&net];
        for (i, j) in mst_connections(ps) {
            let (a, b) = (&ps[i], &ps[j]);
            let ta = tile_of(a.at);
            let tb = tile_of(b.at);
            let la = a.layers & m.net_layers[net]
                | if a.layers & m.net_layers[net] == 0 {
                    a.layers
                } else {
                    0
                };
            let lb = b.layers & m.net_layers[net]
                | if b.layers & m.net_layers[net] == 0 {
                    b.layers
                } else {
                    0
                };
            conns.push(Conn {
                net,
                a: (ta.0, ta.1, la),
                b: (tb.0, tb.1, lb),
                pa: a.at,
                pb: b.at,
            });
        }
    }
    let pitch: Vec<f32> = (0..m.b.nets.len())
        .map(|n| {
            let (_, w) = m.r.track_width(n);
            (w + m.r.class_of(n).clearance) as f32
        })
        .collect();
    let mut h_used = vec![vec![0f32; nh]; NL];
    let mut v_used = vec![vec![0f32; nv]; NL];
    let mut via_used = vec![0f32; nt];
    let mut h_hist = vec![vec![0f32; nh]; NL];
    let mut v_hist = vec![vec![0f32; nv]; NL];
    let mut via_hist = vec![0f32; nt];
    let mut paths: Vec<Vec<(usize, usize, usize)>> = vec![Vec::new(); conns.len()];
    let mut pres = 0.5f32;
    let mut overflow_hist = Vec::new();
    let mut failed = Vec::new();
    let layer_dir = m.cfg.layer_dir;
    for it in 0..gp.iters {
        let snap = (&h_used, &v_used, &via_used, &h_hist, &v_hist, &via_hist);
        let new_paths: Vec<Option<Vec<(usize, usize, usize)>>> = conns
            .par_iter()
            .enumerate()
            .map(|(ci, c)| {
                let cc = caps.get(&m.r.net_class[c.net])?;
                let own = &paths[ci];
                astar(
                    c,
                    cc,
                    snap,
                    own,
                    pitch[c.net],
                    pres,
                    tx,
                    ty,
                    gp,
                    &layer_dir,
                    m.net_layers[c.net],
                )
            })
            .collect();
        failed.clear();
        for (ci, np) in new_paths.into_iter().enumerate() {
            match np {
                Some(pth) => paths[ci] = pth,
                None => {
                    failed.push(ci);
                    paths[ci].clear();
                }
            }
        }
        // recompute usage
        for l in 0..NL {
            h_used[l].iter_mut().for_each(|v| *v = 0.0);
            v_used[l].iter_mut().for_each(|v| *v = 0.0);
        }
        via_used.iter_mut().for_each(|v| *v = 0.0);
        for (ci, pth) in paths.iter().enumerate() {
            let pw = pitch[conns[ci].net];
            for w in pth.windows(2) {
                let (a, b) = (w[0], w[1]);
                if a.2 != b.2 {
                    via_used[a.1 * tx + a.0] += 1.0;
                } else if a.1 == b.1 {
                    h_used[a.2][hid(a.0.min(b.0), a.1)] += pw;
                } else {
                    v_used[a.2][vid(a.0, a.1.min(b.1))] += pw;
                }
            }
        }
        // overflow + history
        let mut ovf = 0.0f64;
        for l in 0..NL {
            for i in 0..nh {
                let o = (h_used[l][i] - h_cap[l][i]).max(0.0);
                if o > 0.0 {
                    h_hist[l][i] += 0.3 * o;
                    ovf += o as f64;
                }
            }
            for i in 0..nv {
                let o = (v_used[l][i] - v_cap[l][i]).max(0.0);
                if o > 0.0 {
                    v_hist[l][i] += 0.3 * o;
                    ovf += o as f64;
                }
            }
        }
        for i in 0..nt {
            let o = (via_used[i] - via_cap[i]).max(0.0);
            if o > 0.0 {
                via_hist[i] += 0.3 * o;
                ovf += o as f64;
            }
        }
        overflow_hist.push(ovf);
        eprintln!(
            "[groute] iter {it}: overflow {ovf:.1} mm, failed {}",
            failed.len()
        );
        if ovf < 1e-6 && failed.is_empty() {
            break;
        }
        pres *= 1.6;
    }
    let mut edge_nets: HashMap<(usize, bool, usize), Vec<usize>> = HashMap::new();
    for (ci, pth) in paths.iter().enumerate() {
        for w in pth.windows(2) {
            let (a, b) = (w[0], w[1]);
            if a.2 != b.2 {
                continue;
            }
            let key = if a.1 == b.1 {
                (a.2, true, hid(a.0.min(b.0), a.1))
            } else {
                (a.2, false, vid(a.0, a.1.min(b.1)))
            };
            edge_nets.entry(key).or_default().push(conns[ci].net);
        }
    }
    GResult {
        tx,
        ty,
        tile,
        x0: gs.x0,
        y0: gs.y0,
        h_used,
        v_used,
        h_cap,
        v_cap,
        via_used,
        via_cap,
        paths,
        conns,
        failed,
        overflow_hist,
        edge_nets,
    }
}

type Snap<'a> = (
    &'a Vec<Vec<f32>>,
    &'a Vec<Vec<f32>>,
    &'a Vec<f32>,
    &'a Vec<Vec<f32>>,
    &'a Vec<Vec<f32>>,
    &'a Vec<f32>,
);

#[allow(clippy::too_many_arguments)]
fn astar(
    c: &Conn,
    cc: &ClassCap,
    s: Snap,
    own: &[(usize, usize, usize)],
    pw: f32,
    pres: f32,
    tx: usize,
    ty: usize,
    gp: &GParams,
    layer_dir: &[u8; NL],
    net_layers: u8,
) -> Option<Vec<(usize, usize, usize)>> {
    let (h_used, v_used, via_used, h_hist, v_hist, via_hist) = s;
    let hid = |x: usize, y: usize| y * (tx - 1) + x;
    let vid = |x: usize, y: usize| y * tx + x;
    // own previous path usage is subtracted (rip-up)
    let mut own_h: HashMap<(usize, usize), f32> = HashMap::new();
    let mut own_v: HashMap<(usize, usize), f32> = HashMap::new();
    let mut own_via: HashMap<usize, f32> = HashMap::new();
    for w in own.windows(2) {
        let (a, b) = (w[0], w[1]);
        if a.2 != b.2 {
            *own_via.entry(a.1 * tx + a.0).or_default() += 1.0;
        } else if a.1 == b.1 {
            *own_h.entry((a.2, hid(a.0.min(b.0), a.1))).or_default() += pw;
        } else {
            *own_v.entry((a.2, vid(a.0, a.1.min(b.1)))).or_default() += pw;
        }
    }
    for mg in [gp.margin_tiles, gp.margin_tiles * 3, i64::MAX / 4] {
        let bx0 = (c.a.0.min(c.b.0) as i64 - mg).max(0) as usize;
        let by0 = (c.a.1.min(c.b.1) as i64 - mg).max(0) as usize;
        let bx1 = ((c.a.0.max(c.b.0) as i64).saturating_add(mg)).min(tx as i64 - 1) as usize;
        let by1 = ((c.a.1.max(c.b.1) as i64).saturating_add(mg)).min(ty as i64 - 1) as usize;
        let w = bx1 - bx0 + 1;
        let h = by1 - by0 + 1;
        let nn = w * h * NL;
        let id = |x: usize, y: usize, l: usize| ((l * h + (y - by0)) * w + (x - bx0)) as u32;
        let dec = |n: u32| {
            let n = n as usize;
            let x = n % w + bx0;
            let y = (n / w) % h + by0;
            let l = n / (w * h);
            (x, y, l)
        };
        let mut g = vec![f64::INFINITY; nn];
        let mut from = vec![u32::MAX; nn];
        let mut heap = BinaryHeap::new();
        let hcost = |x: usize, y: usize| {
            ((x as f64 - c.b.0 as f64).abs() + (y as f64 - c.b.1 as f64).abs()) * gp.tile
        };
        for l in 0..NL {
            if c.a.2 & (1 << l) != 0 {
                let n = id(c.a.0, c.a.1, l);
                g[n as usize] = 0.0;
                heap.push(St {
                    f: hcost(c.a.0, c.a.1),
                    g: 0.0,
                    n,
                });
            }
        }
        let near_pin = |x: usize, y: usize| {
            (x.abs_diff(c.a.0) <= 1 && y.abs_diff(c.a.1) <= 1)
                || (x.abs_diff(c.b.0) <= 1 && y.abs_diff(c.b.1) <= 1)
        };
        let edge_cost = |l: usize, is_h: bool, e: usize, x: usize, y: usize| -> Option<f64> {
            let (free, used, hist, own) = if is_h {
                (
                    cc.h[l][e],
                    h_used[l][e],
                    h_hist[l][e],
                    own_h.get(&(l, e)).copied().unwrap_or(0.0),
                )
            } else {
                (
                    cc.v[l][e],
                    v_used[l][e],
                    v_hist[l][e],
                    own_v.get(&(l, e)).copied().unwrap_or(0.0),
                )
            };
            let mut pen = 1.0f64;
            // free = length of the boundary where a centreline of this class fits (clearance included);
            // a band of length L carries about L / pitch + 1 tracks -> capacity L + pitch (mm of pitch)
            if free <= 0.0 {
                if !near_pin(x, y) {
                    return None;
                }
                pen = 3.0; // pin escape through own pad field
            }
            let cap = free + pw;
            let over = (used - own + pw - cap).max(0.0);
            let dir = if (layer_dir[l] == 0) == is_h {
                1.0
            } else {
                gp.dir_cost
            };
            Some(gp.tile * dir * pen * (1.0 + hist as f64) * (1.0 + (pres * over / pw) as f64))
        };
        let mut found = None;
        while let Some(St { g: gc, n, .. }) = heap.pop() {
            if gc > g[n as usize] {
                continue;
            }
            let (x, y, l) = dec(n);
            if x == c.b.0 && y == c.b.1 && c.b.2 & (1 << l) != 0 {
                found = Some(n);
                break;
            }
            let mut relax =
                |nx: usize, ny: usize, nl: usize, cost: f64, heap: &mut BinaryHeap<St>| {
                    let nn = id(nx, ny, nl);
                    let ng = gc + cost;
                    if ng < g[nn as usize] {
                        g[nn as usize] = ng;
                        from[nn as usize] = n;
                        heap.push(St {
                            f: ng + hcost(nx, ny),
                            g: ng,
                            n: nn,
                        });
                    }
                };
            if net_layers & (1 << l) != 0 || (x == c.a.0 && y == c.a.1) {
                if x > bx0 {
                    if let Some(cst) = edge_cost(l, true, hid(x - 1, y), x, y) {
                        relax(x - 1, y, l, cst, &mut heap);
                    }
                }
                if x < bx1 {
                    if let Some(cst) = edge_cost(l, true, hid(x, y), x, y) {
                        relax(x + 1, y, l, cst, &mut heap);
                    }
                }
                if y > by0 {
                    if let Some(cst) = edge_cost(l, false, vid(x, y - 1), x, y) {
                        relax(x, y - 1, l, cst, &mut heap);
                    }
                }
                if y < by1 {
                    if let Some(cst) = edge_cost(l, false, vid(x, y), x, y) {
                        relax(x, y + 1, l, cst, &mut heap);
                    }
                }
            }
            // via (through): change to any allowed layer in this tile
            let t = y * tx + x;
            let sites = cc.vias[t];
            if sites > 0.0 || near_pin(x, y) {
                let used = via_used[t] - own_via.get(&t).copied().unwrap_or(0.0);
                let cap = sites.max(0.5); // ~1 via per mm^2 of via-site area
                let over = (used + 1.0 - cap).max(0.0);
                let pen = if sites > 0.0 { 1.0 } else { 4.0 };
                let cst = gp.via_cost
                    * gp.tile
                    * pen
                    * (1.0 + via_hist[t] as f64)
                    * (1.0 + (pres * over) as f64);
                for nl in 0..NL {
                    if nl != l
                        && (net_layers & (1 << nl) != 0
                            || (x == c.b.0 && y == c.b.1 && c.b.2 & (1 << nl) != 0))
                    {
                        relax(x, y, nl, cst, &mut heap);
                    }
                }
            }
        }
        if let Some(mut n) = found {
            let mut pth = vec![dec(n)];
            while from[n as usize] != u32::MAX {
                n = from[n as usize];
                pth.push(dec(n));
            }
            pth.reverse();
            return Some(pth);
        }
        if bx0 == 0 && by0 == 0 && bx1 == tx - 1 && by1 == ty - 1 {
            break;
        }
    }
    None
}

/// per tile utilisation per layer: max over its edges of used/cap (cap 0 and used > 0 -> 2.0)
pub fn tile_util(r: &GResult) -> Vec<Vec<f64>> {
    let (tx, ty) = (r.tx, r.ty);
    let hid = |x: usize, y: usize| y * (tx - 1) + x;
    let vid = |x: usize, y: usize| y * tx + x;
    let mut out = vec![vec![0f64; tx * ty]; NL];
    let u = |used: f32, cap: f32| -> f64 {
        if used <= 0.0 {
            0.0
        } else if cap <= 0.0 {
            2.0
        } else {
            (used / cap) as f64
        }
    };
    for (l, ol) in out.iter_mut().enumerate().take(NL) {
        for y in 0..ty {
            for x in 0..tx {
                let mut m: f64 = 0.0;
                if x + 1 < tx {
                    m = m.max(u(r.h_used[l][hid(x, y)], r.h_cap[l][hid(x, y)]));
                }
                if x > 0 {
                    m = m.max(u(r.h_used[l][hid(x - 1, y)], r.h_cap[l][hid(x - 1, y)]));
                }
                if y + 1 < ty {
                    m = m.max(u(r.v_used[l][vid(x, y)], r.v_cap[l][vid(x, y)]));
                }
                if y > 0 {
                    m = m.max(u(r.v_used[l][vid(x, y - 1)], r.v_cap[l][vid(x, y - 1)]));
                }
                ol[y * tx + x] = m;
            }
        }
    }
    out
}

#[derive(serde::Serialize)]
pub struct Hotspot {
    pub id: usize,
    pub bbox: [f64; 4],
    pub overflow_mm: f64,
    pub tiles: usize,
    pub layers: Vec<String>,
    pub vias_over: f64,
    pub nets: Vec<(String, usize)>,
    pub parts: Vec<String>,
}

/// connected clusters of overflowing tiles (any layer or via overflow), largest first
pub fn hotspots(m: &Model, r: &GResult) -> Vec<Hotspot> {
    let (tx, ty) = (r.tx, r.ty);
    let hid = |x: usize, y: usize| y * (tx - 1) + x;
    let vid = |x: usize, y: usize| y * tx + x;
    let mut ovf = vec![0f64; tx * ty];
    let mut lay = vec![0u8; tx * ty];
    let mut viao = vec![0f64; tx * ty];
    for l in 0..NL {
        for y in 0..ty {
            for x in 0..tx - 1 {
                let o = (r.h_used[l][hid(x, y)] - r.h_cap[l][hid(x, y)]).max(0.0) as f64;
                if o > 0.0 {
                    ovf[y * tx + x] += o;
                    lay[y * tx + x] |= 1 << l;
                }
            }
        }
        for y in 0..ty - 1 {
            for x in 0..tx {
                let o = (r.v_used[l][vid(x, y)] - r.v_cap[l][vid(x, y)]).max(0.0) as f64;
                if o > 0.0 {
                    ovf[y * tx + x] += o;
                    lay[y * tx + x] |= 1 << l;
                }
            }
        }
    }
    for (t, vo) in viao.iter_mut().enumerate().take(tx * ty) {
        let cap = r.via_cap[t].max(0.5);
        let o = (r.via_used[t] - cap).max(0.0) as f64;
        if o > 0.0 && r.via_used[t] > 0.0 {
            *vo = o;
        }
    }
    let mut seen = vec![false; tx * ty];
    let mut out = Vec::new();
    for s in 0..tx * ty {
        if seen[s] || (ovf[s] <= 0.0 && viao[s] <= 0.0) {
            continue;
        }
        let mut st = vec![s];
        seen[s] = true;
        let mut cells = Vec::new();
        while let Some(t) = st.pop() {
            cells.push(t);
            let (x, y) = (t % tx, t / tx);
            for (dx, dy) in (-2i64..=2).flat_map(|a| (-2i64..=2).map(move |b| (a, b))) {
                let (nx, ny) = (x as i64 + dx, y as i64 + dy);
                if nx < 0 || ny < 0 || nx >= tx as i64 || ny >= ty as i64 {
                    continue;
                }
                let u = ny as usize * tx + nx as usize;
                if !seen[u] && (ovf[u] > 0.0 || viao[u] > 0.0) {
                    seen[u] = true;
                    st.push(u);
                }
            }
        }
        let mut bb = BBox::empty();
        let mut o = 0.0;
        let mut vo = 0.0;
        let mut lm = 0u8;
        let mut nets: HashMap<usize, usize> = HashMap::new();
        for &t in &cells {
            let (x, y) = (t % tx, t / tx);
            bb.add(p(r.x0 + x as f64 * r.tile, r.y0 + y as f64 * r.tile));
            bb.add(p(
                r.x0 + (x + 1) as f64 * r.tile,
                r.y0 + (y + 1) as f64 * r.tile,
            ));
            o += ovf[t];
            vo += viao[t];
            lm |= lay[t];
            for l in 0..NL {
                if x + 1 < tx {
                    if let Some(ns) = r.edge_nets.get(&(l, true, hid(x, y))) {
                        for n in ns {
                            *nets.entry(*n).or_default() += 1;
                        }
                    }
                }
                if y + 1 < ty {
                    if let Some(ns) = r.edge_nets.get(&(l, false, vid(x, y))) {
                        for n in ns {
                            *nets.entry(*n).or_default() += 1;
                        }
                    }
                }
            }
        }
        let mut nv: Vec<(String, usize)> = nets
            .into_iter()
            .map(|(n, c)| (m.b.nets[n].clone(), c))
            .collect();
        nv.sort_by_key(|e| std::cmp::Reverse(e.1));
        nv.truncate(10);
        let mut parts: Vec<String> =
            m.b.fps
                .iter()
                .filter(|f| bb.contains(f.at))
                .map(|f| f.refd.clone())
                .collect();
        parts.truncate(20);
        out.push(Hotspot {
            id: 0,
            bbox: [bb.x0, bb.y0, bb.x1, bb.y1],
            overflow_mm: (o * 100.0).round() / 100.0,
            tiles: cells.len(),
            layers: (0..NL)
                .filter(|l| lm & (1 << l) != 0)
                .map(|l| CU[l].to_string())
                .collect(),
            vias_over: vo,
            nets: nv,
            parts,
        });
    }
    out.sort_by(|a, b| {
        (b.overflow_mm + b.vias_over)
            .partial_cmp(&(a.overflow_mm + a.vias_over))
            .unwrap()
    });
    for (i, h) in out.iter_mut().enumerate() {
        h.id = i + 1;
    }
    out
}
