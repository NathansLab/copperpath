//! Planes: native zone filler (no kicad-cli), plane report (islands, unconnected pads, necks,
//! fragmentation, stitching coverage), plane-net detection and zone outline generation
//! (`plane create`, `plane adjust`: grow into free area, cut back from HV copper, merge).
//!
//! Everything works on a raster of the zone's layer (default 0.1 mm cells). The filler follows
//! KiCad's recipe: the area where a disk of half the minimum width fits (zone outline shrunk,
//! foreign copper grown by clearance + half width, board edge, pour keep-outs, other-net zones
//! of higher priority) is grown again by half the minimum width; islands without copper of the
//! zone's net are isolated (KiCad removes them). Pads of the own net connect solidly (thermal
//! spokes are not modelled).

#![allow(clippy::needless_range_loop, clippy::too_many_arguments)]

use crate::board::*;
use crate::geom::*;
use crate::model::{self, Kind, Model};
use crate::rules::Rules;
use rayon::prelude::*;
use serde::Serialize;

// ---------------------------------------------------------------------------------------------
// raster helpers

/// squared Euclidean distance transform (in cells^2): distance of every cell to the nearest
/// feature cell (Felzenszwalb & Huttenlocher, separable, exact)
pub fn edt(feat: &[bool], nx: usize, ny: usize) -> Vec<f32> {
    const INF: f32 = 1e20;
    let mut d: Vec<f32> = feat.iter().map(|&f| if f { 0.0 } else { INF }).collect();
    let n = nx.max(ny);
    let mut f = vec![0f32; n];
    let mut out = vec![0f32; n];
    let mut v = vec![0usize; n];
    let mut z = vec![0f32; n + 1];
    let mut dt1 = |f: &[f32], k: usize, out: &mut [f32]| {
        let mut j = 0usize;
        v[0] = 0;
        z[0] = -INF;
        z[1] = INF;
        for q in 1..k {
            loop {
                let vj = v[j];
                let s = ((f[q] + (q * q) as f32) - (f[vj] + (vj * vj) as f32))
                    / (2.0 * q as f32 - 2.0 * vj as f32);
                if s <= z[j] && j > 0 {
                    j -= 1;
                    continue;
                }
                if s <= z[j] {
                    // j == 0
                    v[0] = q;
                    z[0] = -INF;
                    z[1] = INF;
                    break;
                }
                j += 1;
                v[j] = q;
                z[j] = s;
                z[j + 1] = INF;
                break;
            }
        }
        j = 0;
        for (q, o) in out.iter_mut().enumerate().take(k) {
            while z[j + 1] < q as f32 {
                j += 1;
            }
            let dq = q as f32 - v[j] as f32;
            *o = dq * dq + f[v[j]];
        }
    };
    for x in 0..nx {
        for y in 0..ny {
            f[y] = d[y * nx + x];
        }
        dt1(&f[..ny], ny, &mut out[..ny]);
        for y in 0..ny {
            d[y * nx + x] = out[y];
        }
    }
    for y in 0..ny {
        f[..nx].copy_from_slice(&d[y * nx..(y + 1) * nx]);
        dt1(&f[..nx], nx, &mut out[..nx]);
        d[y * nx..(y + 1) * nx].copy_from_slice(&out[..nx]);
    }
    d
}

/// connected components: label (1-based) per cell of `m`, 0 outside; 8- or 4-connected
pub fn label(m: &[bool], nx: usize, ny: usize, eight: bool) -> (Vec<u32>, u32) {
    let mut lab = vec![0u32; m.len()];
    let mut n = 0u32;
    let mut stack = Vec::new();
    for s in 0..m.len() {
        if !m[s] || lab[s] != 0 {
            continue;
        }
        n += 1;
        lab[s] = n;
        stack.push(s);
        while let Some(i) = stack.pop() {
            let (x, y) = ((i % nx) as i64, (i / nx) as i64);
            for dy in -1i64..=1 {
                for dx in -1i64..=1 {
                    if (dx == 0 && dy == 0) || (!eight && dx != 0 && dy != 0) {
                        continue;
                    }
                    let (xx, yy) = (x + dx, y + dy);
                    if xx < 0 || yy < 0 || xx >= nx as i64 || yy >= ny as i64 {
                        continue;
                    }
                    let j = yy as usize * nx + xx as usize;
                    if m[j] && lab[j] == 0 {
                        lab[j] = n;
                        stack.push(j);
                    }
                }
            }
        }
    }
    (lab, n)
}

const DIRS: [(i64, i64); 8] = [
    (1, 0),
    (1, 1),
    (0, 1),
    (-1, 1),
    (-1, 0),
    (-1, -1),
    (0, -1),
    (1, -1),
];

/// Moore-neighbour boundary trace of an 8-connected cell set, clockwise (y down), starting at
/// its first cell in raster order. Returns the boundary cells.
fn trace(member: &dyn Fn(i64, i64) -> bool, start: (i64, i64)) -> Vec<(i64, i64)> {
    let mut out = vec![start];
    let mut cur = start;
    let mut d = 0usize; // as if we arrived moving east
    let mut first: Option<usize> = None;
    let cap = 4_000_000;
    loop {
        let s = if d.is_multiple_of(2) {
            (d + 6) % 8
        } else {
            (d + 5) % 8
        };
        let mut nd = None;
        for k in 0..8 {
            let dd = (s + k) % 8;
            if member(cur.0 + DIRS[dd].0, cur.1 + DIRS[dd].1) {
                nd = Some(dd);
                break;
            }
        }
        let Some(nd) = nd else { return out }; // single cell
        if cur == start {
            match first {
                None => first = Some(nd),
                Some(f) if f == nd => break,
                _ => {}
            }
        }
        cur = (cur.0 + DIRS[nd].0, cur.1 + DIRS[nd].1);
        d = nd;
        if cur == start {
            continue;
        }
        out.push(cur);
        if out.len() > cap {
            break;
        }
    }
    out
}

/// Douglas-Peucker simplification of a closed ring
fn simplify(pts: &[P], tol: f64) -> Vec<P> {
    let n = pts.len();
    if n <= 4 {
        return pts.to_vec();
    }
    fn dp(pts: &[P], tol: f64, keep: &mut [bool], a: usize, b: usize) {
        if b <= a + 1 {
            return;
        }
        let (mut dm, mut im) = (0.0, a);
        for i in a + 1..b {
            let d = dist_seg(pts[i], pts[a], pts[b]);
            if d > dm {
                dm = d;
                im = i;
            }
        }
        if dm > tol {
            keep[im] = true;
            dp(pts, tol, keep, a, im);
            dp(pts, tol, keep, im, b);
        }
    }
    // split at the point farthest from the first
    let far = (0..n)
        .max_by(|&i, &j| (pts[i] - pts[0]).len().total_cmp(&(pts[j] - pts[0]).len()))
        .unwrap_or(n / 2);
    let mut ring: Vec<P> = pts.to_vec();
    ring.push(pts[0]);
    let mut keep = vec![false; n + 1];
    keep[0] = true;
    keep[far] = true;
    keep[n] = true;
    dp(&ring, tol, &mut keep, 0, far);
    dp(&ring, tol, &mut keep, far, n);
    (0..n).filter(|&i| keep[i]).map(|i| ring[i]).collect()
}

fn signed_area(pts: &[P]) -> f64 {
    let n = pts.len();
    (0..n).map(|i| pts[i].cross(pts[(i + 1) % n])).sum::<f64>() / 2.0
}

/// join holes into the outer ring with zero-width cuts (KiCad's "fractured" fill polygons)
fn fracture(outer: Vec<P>, mut holes: Vec<Vec<P>>) -> Vec<P> {
    let mut poly = outer;
    let sa = signed_area(&poly);
    holes.retain(|h| h.len() >= 3);
    for h in holes.iter_mut() {
        if signed_area(h).signum() == sa.signum() {
            h.reverse();
        }
    }
    // topmost vertex of each hole, processed from the top
    let mut order: Vec<(usize, usize)> = holes
        .iter()
        .enumerate()
        .map(|(k, h)| {
            let i = (0..h.len())
                .min_by(|&a, &b| h[a].y.total_cmp(&h[b].y).then(h[a].x.total_cmp(&h[b].x)))
                .unwrap();
            (k, i)
        })
        .collect();
    order.sort_by(|a, b| holes[a.0][a.1].y.total_cmp(&holes[b.0][b.1].y));
    for (k, i) in order {
        let h = &holes[k];
        let top = h[i];
        // nearest edge of the polygon straight above
        let mut best: Option<(usize, f64)> = None;
        let n = poly.len();
        for e in 0..n {
            let (a, b) = (poly[e], poly[(e + 1) % n]);
            if (a.x - b.x).abs() < 1e-12 {
                continue;
            }
            if top.x < a.x.min(b.x) || top.x > a.x.max(b.x) {
                continue;
            }
            let y = a.y + (top.x - a.x) * (b.y - a.y) / (b.x - a.x);
            if y < top.y - 1e-9 && best.is_none_or(|(_, by)| y > by) {
                best = Some((e, y));
            }
        }
        let Some((e, y)) = best else { continue };
        let q = p(top.x, y);
        let mut np = Vec::with_capacity(poly.len() + h.len() + 3);
        np.extend_from_slice(&poly[..=e]);
        np.push(q);
        for j in 0..=h.len() {
            np.push(h[(i + j) % h.len()]);
        }
        np.push(q);
        np.extend_from_slice(&poly[e + 1..]);
        poly = np;
    }
    poly
}

/// polygons (fractured, one per 8-connected component) of a cell mask, through the centres of
/// the boundary cells, simplified with tolerance `tol` (mm)
pub fn polygons(m: &[bool], gs: &GridSpec, tol: f64, min_cells: usize) -> Vec<Vec<P>> {
    rings(m, gs, tol, min_cells)
        .into_iter()
        .map(|(o, h)| fracture(o, h))
        .collect()
}

/// outer ring and hole rings of every 8-connected component of a cell mask
pub fn rings(m: &[bool], gs: &GridSpec, tol: f64, min_cells: usize) -> Vec<(Vec<P>, Vec<Vec<P>>)> {
    let (nx, ny) = (gs.nx, gs.ny);
    let (fg, nfg) = label(m, nx, ny, true);
    // background components (4-connected); those not touching the border are holes
    let bgm: Vec<bool> = m.iter().map(|v| !v).collect();
    let (bg, nbg) = label(&bgm, nx, ny, false);
    let mut border = vec![false; nbg as usize + 1];
    for x in 0..nx {
        border[bg[x] as usize] = true;
        border[bg[(ny - 1) * nx + x] as usize] = true;
    }
    for y in 0..ny {
        border[bg[y * nx] as usize] = true;
        border[bg[y * nx + nx - 1] as usize] = true;
    }
    // first cell (raster order) and size of every component
    let mut first_fg = vec![usize::MAX; nfg as usize + 1];
    let mut size = vec![0usize; nfg as usize + 1];
    let mut first_bg = vec![usize::MAX; nbg as usize + 1];
    let mut hole_of = vec![0u32; nbg as usize + 1];
    for i in 0..m.len() {
        let l = fg[i] as usize;
        if l > 0 {
            size[l] += 1;
            if first_fg[l] == usize::MAX {
                first_fg[l] = i;
            }
        } else {
            let b = bg[i] as usize;
            if first_bg[b] == usize::MAX {
                first_bg[b] = i;
                // enclosing component: the cell above the first cell of a hole is foreground
                if !border[b] && i >= nx {
                    hole_of[b] = fg[i - nx];
                }
            }
        }
    }
    let to_p = |c: &(i64, i64)| gs.center(c.0 as usize, c.1 as usize);
    let mut holes: Vec<Vec<Vec<P>>> = vec![Vec::new(); nfg as usize + 1];
    for b in 1..=nbg as usize {
        if border[b] || hole_of[b] == 0 || first_bg[b] == usize::MAX {
            continue;
        }
        let s = first_bg[b];
        let mem = |x: i64, y: i64| {
            x >= 0
                && y >= 0
                && (x as usize) < nx
                && (y as usize) < ny
                && bg[y as usize * nx + x as usize] == b as u32
        };
        let ring: Vec<P> = trace(&mem, ((s % nx) as i64, (s / nx) as i64))
            .iter()
            .map(to_p)
            .collect();
        if ring.len() >= 3 {
            holes[hole_of[b] as usize].push(simplify(&ring, tol));
        }
    }
    let mut out = Vec::new();
    for l in 1..=nfg as usize {
        if size[l] < min_cells.max(1) {
            continue;
        }
        let s = first_fg[l];
        let mem = |x: i64, y: i64| {
            x >= 0
                && y >= 0
                && (x as usize) < nx
                && (y as usize) < ny
                && fg[y as usize * nx + x as usize] == l as u32
        };
        let ring: Vec<P> = trace(&mem, ((s % nx) as i64, (s / nx) as i64))
            .iter()
            .map(to_p)
            .collect();
        if ring.len() < 3 {
            continue;
        }
        let outer = simplify(&ring, tol);
        out.push((outer, std::mem::take(&mut holes[l])));
    }
    out
}

/// cells (centres) inside the board outlines (even-odd)
fn board_mask(b: &Board, gs: &GridSpec) -> Vec<bool> {
    let mut m = vec![b.edge.is_empty(); gs.n()];
    for ol in &b.edge {
        gs.fill_poly(ol, &mut |x, y| {
            let i = gs.idx(x, y);
            m[i] = !m[i];
        });
    }
    m
}

// ---------------------------------------------------------------------------------------------
// native fill

/// one island of a native fill
#[derive(Debug, Clone, Serialize)]
pub struct Island {
    pub area_mm2: f64,
    /// touches copper of the zone's net (pad, via, track) on this layer; KiCad removes the others
    pub connected: bool,
    /// via or plated hole of the net in the island
    pub has_via: bool,
    pub bbox: [f64; 4],
    /// a point inside the island
    pub at: [f64; 2],
}

/// a narrow neck between copper of the net inside one island
#[derive(Debug, Clone, Serialize)]
pub struct Neck {
    pub layer: String,
    pub width_mm: f64,
    pub at: [f64; 2],
}

/// native fill of one zone on one layer
pub struct ZoneFill {
    pub zone: usize,
    pub layer: usize,
    pub gs: GridSpec,
    pub fill: Vec<bool>,
    /// island label per cell (0 = no fill)
    pub lab: Vec<u32>,
    pub islands: Vec<Island>,
    /// cells of the zone outline inside the board
    pub outline_cells: usize,
    /// cells covered by copper of the own net (terminals)
    pub own: Vec<bool>,
    /// cells of vias / plated holes of the own net
    pub vias: Vec<bool>,
}

#[derive(Debug, Clone)]
pub struct FillParams {
    pub res: f64,
    /// minimum copper width (KiCad zone min_thickness)
    pub min_width: f64,
}

impl Default for FillParams {
    fn default() -> Self {
        FillParams {
            res: 0.1,
            min_width: 0.25,
        }
    }
}

fn r3(v: f64) -> f64 {
    (v * 1000.0).round() / 1000.0
}

/// an own pad with a thermal relief: centre, angle (deg), half size, gap, spoke width, round
type Thermal = (P, f64, (f64, f64), f64, f64, bool);

/// native fill of zone `zi` on copper layer `l`
pub fn fill_zone(m: &Model, zi: usize, l: usize, fp: &FillParams) -> ZoneFill {
    let b = m.b;
    let z = &b.zones[zi];
    let net = z.net;
    let bit = 1u8 << l;
    let res = fp.res;
    let mut bb = BBox::empty();
    for q in &z.outline {
        bb.add(*q);
    }
    let bb = bb.inflate(2.0 * res);
    let gs = GridSpec::new(bb, res);
    let n = gs.n();
    // the zone's own minimum width (KiCad min_thickness), the global one for zones without it
    let rt = if z.min_thickness > 0.0 {
        z.min_thickness
    } else {
        fp.min_width
    } / 2.0;
    // inside the outline and the board
    let mut outl = vec![false; n];
    gs.fill_poly(&z.outline, &mut |x, y| outl[gs.idx(x, y)] = true);
    let inb = board_mask(b, &gs);
    let outline_cells = (0..n).filter(|&i| outl[i] && inb[i]).count();
    // centre-allowed: outline shrunk by rt
    let outside: Vec<bool> = (0..n).map(|i| !(outl[i] && inb[i])).collect();
    let dout = edt(&outside, gs.nx, gs.ny);
    let rc = rt / res;
    let mut ok: Vec<bool> = (0..n).map(|i| dout[i] >= (rc * rc) as f32).collect();
    // `bad`: cells no copper of the zone may cover (foreign copper plus its clearance); thermal
    // spokes are checked against it
    let mut bad = vec![false; n];
    let block = |s: &Shape, d: f64, ok: &mut Vec<bool>| {
        gs.stamp(s, d, |x, y| ok[gs.idx(x, y)] = false);
    };
    let block2 = |s: &Shape, d: f64, ok: &mut Vec<bool>, bad: &mut Vec<bool>| {
        gs.stamp(s, d + rt, |x, y| ok[gs.idx(x, y)] = false);
        gs.stamp(s, d, |x, y| bad[gs.idx(x, y)] = true);
    };
    // board edge
    let ec = m.r.edge_clearance(net) + rt;
    for (a, e) in &b.edge_segs {
        block2(
            &Shape::Capsule {
                a: *a,
                b: *e,
                r: 0.0,
            },
            ec - rt,
            &mut ok,
            &mut bad,
        );
    }
    for ol in &b.edge {
        for i in 0..ol.len() {
            block2(
                &Shape::Capsule {
                    a: ol[i],
                    b: ol[(i + 1) % ol.len()],
                    r: 0.0,
                },
                ec - rt,
                &mut ok,
                &mut bad,
            );
        }
    }
    // foreign copper
    let gb = gs.bbox();
    let mut own = vec![false; n];
    let mut vias = vec![false; n];
    // own pads with a thermal relief: (centre, angle, half size, gap, spoke width)
    let mut thermals: Vec<Thermal> = Vec::new();
    for o in &m.objs {
        if o.layers & bit == 0 || !o.bbox.inflate(2.0).overlaps(&gb) {
            continue;
        }
        match o.kind {
            Kind::Pad { .. } | Kind::Track | Kind::Via => {
                if o.net == net && net != 0 {
                    gs.stamp(&o.shape, 0.0, |x, y| own[gs.idx(x, y)] = true);
                    if o.hole_r > 0.0 {
                        gs.stamp(&o.shape, 0.0, |x, y| vias[gs.idx(x, y)] = true);
                    }
                    // pads connect as the pad / footprint / zone says (vias and tracks solid)
                    if let Kind::Pad { fp: fi, pad } = o.kind {
                        let f = &b.fps[fi];
                        let pd = &f.pads[pad];
                        let mode = match pd.zone_connect.unwrap_or(z.connect) {
                            3 if pd.drill > 0.0 => 1,
                            3 => 2,
                            k => k,
                        };
                        if mode == 0 {
                            let c = m.r.clr(net, 0, is_outer(l)).max(z.clearance);
                            block(&o.shape, c + rt, &mut ok);
                        } else if mode == 1 {
                            let gap = pd.thermal_gap.unwrap_or(z.thermal_gap);
                            let w = pd.thermal_bridge.unwrap_or(z.thermal_bridge);
                            block(&o.shape, gap + rt, &mut ok);
                            let (c, a) = f.pad_world(pd);
                            let round = (pd.size.0 - pd.size.1).abs() < 1e-6
                                && matches!(o.shape, Shape::Capsule { .. });
                            thermals.push((
                                c,
                                a,
                                (pd.size.0 / 2.0, pd.size.1 / 2.0),
                                gap,
                                w.max(2.0 * rt),
                                round,
                            ));
                        }
                    }
                } else {
                    let c = m.clr(net, o, l).max(0.0).max(z.clearance);
                    // a hole needs its hole clearance too (NPTH without copper)
                    let c = if o.hole_r > 0.0 {
                        c.max(m.r.min_hole_clearance)
                    } else {
                        c
                    };
                    block2(&o.shape, c, &mut ok, &mut bad);
                }
            }
            _ => {}
        }
    }
    // copper graphics (net-less)
    for (s, gl) in &b.graphics {
        if gl & bit != 0 && s.bbox().inflate(2.0).overlaps(&gb) {
            block2(
                s,
                m.r.clr(net, 0, is_outer(l)).max(z.clearance),
                &mut ok,
                &mut bad,
            );
        }
    }
    // other zones: pour keep-outs, zones of other nets with a higher priority
    for (zj, o) in b.zones.iter().enumerate() {
        if zj == zi || o.layers & bit == 0 || o.outline.len() < 3 {
            continue;
        }
        let s = Shape::Poly {
            pts: o.outline.clone(),
            r: 0.0,
        };
        if o.keepout {
            if o.ko_pour {
                block(&s, rt, &mut ok);
            }
        } else if o.net != net && o.priority > z.priority {
            // the outline stands in for that zone's fill; spokes are not checked against it
            // (its fill keeps clear of this zone's pads, e.g. a pad inside its outline)
            block(&s, m.r.clr(net, o.net, is_outer(l)) + rt, &mut ok);
        }
    }
    // grow back by rt
    let dok = edt(&ok, gs.nx, gs.ny);
    let r2 = (rc * rc) as f32 + 1e-3;
    let mut fill: Vec<bool> = (0..n).map(|i| dok[i] <= r2 && inb[i]).collect();
    // thermal spokes (KiCad: four, along the pad axes, 45 degrees for round pads): a spoke is
    // kept when it is clear of foreign copper and its end reaches the fill
    let mut spokes: Vec<Shape> = Vec::new();
    for &(c, a, (hx, hy), gap, w, round) in &thermals {
        for k in 0..4 {
            let ang = a.to_radians()
                + if round { 45f64.to_radians() } else { 0.0 }
                + k as f64 * std::f64::consts::FRAC_PI_2;
            let (dx, dy) = (ang.cos(), ang.sin());
            // pad half extent along the spoke (pad frame axes)
            let ext = if round || k % 2 == 0 { hx } else { hy };
            let e = P {
                x: c.x + dx * (ext + gap + rt + res),
                y: c.y + dy * (ext + gap + rt + res),
            };
            let sp = Shape::Capsule {
                a: c,
                b: e,
                r: w / 2.0,
            };
            let (mut clear, mut reach) = (true, false);
            let tip = Shape::circle(e, w / 2.0);
            gs.stamp(&sp, 0.0, |x, y| clear &= !bad[gs.idx(x, y)]);
            gs.stamp(&tip, 0.0, |x, y| reach |= fill[gs.idx(x, y)]);
            if clear && reach {
                spokes.push(sp);
            }
        }
    }
    for sp in &spokes {
        gs.stamp(sp, 0.0, |x, y| {
            let i = gs.idx(x, y);
            fill[i] = inb[i];
        });
    }
    let (lab, k) = label(&fill, gs.nx, gs.ny, true);
    let mut isl: Vec<Island> = (0..k)
        .map(|_| Island {
            area_mm2: 0.0,
            connected: false,
            has_via: false,
            bbox: [
                f64::INFINITY,
                f64::INFINITY,
                f64::NEG_INFINITY,
                f64::NEG_INFINITY,
            ],
            at: [0.0, 0.0],
        })
        .collect();
    let a1 = res * res;
    for i in 0..n {
        let li = lab[i] as usize;
        if li == 0 {
            continue;
        }
        let it = &mut isl[li - 1];
        if it.area_mm2 == 0.0 {
            let c = gs.center(i % gs.nx, i / gs.nx);
            it.at = [r3(c.x), r3(c.y)];
        }
        it.area_mm2 += a1;
        it.connected |= own[i];
        it.has_via |= vias[i];
        let c = gs.center(i % gs.nx, i / gs.nx);
        it.bbox[0] = it.bbox[0].min(c.x);
        it.bbox[1] = it.bbox[1].min(c.y);
        it.bbox[2] = it.bbox[2].max(c.x);
        it.bbox[3] = it.bbox[3].max(c.y);
    }
    for it in isl.iter_mut() {
        it.area_mm2 = r3(it.area_mm2);
        it.bbox = it.bbox.map(r3);
    }
    ZoneFill {
        zone: zi,
        layer: l,
        gs,
        fill,
        lab,
        islands: isl,
        outline_cells,
        own,
        vias,
    }
}

impl ZoneFill {
    /// fill polygons of the connected islands (KiCad removes isolated ones)
    pub fn polygons(&self, keep_isolated: bool) -> Vec<Vec<P>> {
        let m: Vec<bool> = (0..self.fill.len())
            .map(|i| {
                let l = self.lab[i] as usize;
                l > 0 && (keep_isolated || self.islands[l - 1].connected)
            })
            .collect();
        polygons(&m, &self.gs, self.gs.res * 0.5, 2)
    }

    /// narrowest necks between copper of the net inside an island: cells are added from the
    /// widest (largest distance to the fill edge) down; when two parts that both hold copper of
    /// the net meet, the width there is a neck the current depends on
    pub fn necks(&self) -> Vec<Neck> {
        let (nx, ny) = (self.gs.nx, self.gs.ny);
        let n = self.fill.len();
        let nonfill: Vec<bool> = self.fill.iter().map(|f| !f).collect();
        let d = edt(&nonfill, nx, ny);
        let mut cells: Vec<usize> = (0..n).filter(|&i| self.fill[i]).collect();
        // copper of the net is joined first (it conducts by itself), then the fill from the
        // widest cells down
        let key = |i: usize| if self.own[i] { f32::INFINITY } else { d[i] };
        cells.sort_by(|&a, &b| key(b).total_cmp(&key(a)));
        let mut parent: Vec<u32> = (0..n as u32).collect();
        let mut term = self.own.clone();
        let mut act = vec![false; n];
        fn find(p: &mut [u32], mut i: usize) -> usize {
            while p[i] as usize != i {
                p[i] = p[p[i] as usize];
                i = p[i] as usize;
            }
            i
        }
        let mut out = Vec::new();
        for &i in &cells {
            act[i] = true;
            let (x, y) = ((i % nx) as i64, (i / nx) as i64);
            for (dx, dy) in DIRS {
                let (xx, yy) = (x + dx, y + dy);
                if xx < 0 || yy < 0 || xx >= nx as i64 || yy >= ny as i64 {
                    continue;
                }
                let j = yy as usize * nx + xx as usize;
                if !act[j] {
                    continue;
                }
                let (ri, rj) = (find(&mut parent, i), find(&mut parent, j));
                if ri == rj {
                    continue;
                }
                if term[ri] && term[rj] && !self.own[i] {
                    let w = (2.0 * (d[i] as f64).sqrt() - 1.0).max(1.0) * self.gs.res;
                    let c = self.gs.center(i % nx, i / nx);
                    out.push(Neck {
                        layer: CU[self.layer].to_string(),
                        width_mm: r3(w),
                        at: [r3(c.x), r3(c.y)],
                    });
                }
                parent[rj] = ri as u32;
                term[ri] |= term[rj];
            }
        }
        out.sort_by(|a, b| a.width_mm.total_cmp(&b.width_mm));
        out
    }

    /// fraction of the fill within `r` mm of a via / plated hole of the net
    pub fn stitch_coverage(&self, r: f64) -> f64 {
        let d = edt(&self.vias, self.gs.nx, self.gs.ny);
        let rc = (r / self.gs.res).powi(2) as f32;
        let (mut tot, mut cov) = (0usize, 0usize);
        for i in 0..self.fill.len() {
            if self.fill[i] {
                tot += 1;
                cov += (d[i] <= rc) as usize;
            }
        }
        if tot == 0 {
            0.0
        } else {
            cov as f64 / tot as f64
        }
    }
}

/// copper zones selected by `sel` (zone index), with their layers
fn zone_layers(b: &Board, sel: &dyn Fn(usize) -> bool) -> Vec<(usize, usize)> {
    let mut v = Vec::new();
    for (zi, z) in b.zones.iter().enumerate() {
        if z.keepout || z.net == 0 || z.outline.len() < 3 || !sel(zi) {
            continue;
        }
        for l in 0..NL {
            if z.layers & b.cu_mask & (1 << l) != 0 {
                v.push((zi, l));
            }
        }
    }
    v
}

/// fill the selected zones natively (all layers, in parallel)
pub fn fill_zones(m: &Model, sel: &dyn Fn(usize) -> bool, fp: &FillParams) -> Vec<ZoneFill> {
    zone_layers(m.b, sel)
        .par_iter()
        .map(|&(zi, l)| fill_zone(m, zi, l, fp))
        .collect()
}

/// replace the saved fills (`Zone::fills`) of the selected zones by native fills; returns the
/// number of fill polygons. Commands that read fills (`stitch`, `fanout`, `opens`, `route`)
/// then work without kicad-cli. Islands without copper of the net are kept (a new plane has no
/// via yet: `fanout` lands its vias on it; they hold no pads, so connectivity is unchanged).
pub fn fill_board(
    b: &mut Board,
    r: &Rules,
    cfg: &model::BoardCfg,
    sel: &dyn Fn(usize) -> bool,
    fp: &FillParams,
) -> usize {
    let polys: Vec<(usize, usize, Vec<Vec<P>>)> = {
        let m = Model::new(b, r, cfg.clone(), model::CopperMode::All);
        fill_zones(&m, sel, fp)
            .par_iter()
            .map(|f| (f.zone, f.layer, f.polygons(true)))
            .collect()
    };
    let mut cleared = std::collections::HashSet::new();
    let mut k = 0;
    for (zi, l, ps) in polys {
        if cleared.insert(zi) {
            b.zones[zi].fills.clear();
        }
        for pl in ps {
            b.zones[zi].fills.push((l, pl));
            k += 1;
        }
    }
    k
}

// ---------------------------------------------------------------------------------------------
// detection and report

/// nets that look like power nets by name
pub fn power_like(name: &str) -> bool {
    let u = name.rsplit('/').next().unwrap_or(name).to_ascii_uppercase();
    u.contains("GND")
        || u.starts_with('+')
        || u.starts_with('-')
        || u.starts_with("VCC")
        || u.starts_with("VDD")
        || u.starts_with("VSS")
        || u.starts_with("VBAT")
        || u.starts_with("VIN")
        || u.starts_with("PGND")
        || u.starts_with("AGND")
}

#[derive(Debug, Clone, Serialize)]
pub struct PlaneNet {
    pub net: String,
    pub class: String,
    pub pads: usize,
    /// zones of the net: name, layers, priority, saved fill
    pub zones: Vec<serde_json::Value>,
    /// has a zone on an inner layer (a plane) or on any layer
    pub plane: bool,
    pub pour: bool,
    /// power-like name without any zone
    pub candidate: bool,
}

/// plane and power nets of the board: nets with zones (`plane` = on an inner layer) and
/// power-like nets with many pads but no zone (`candidate`)
pub fn detect(b: &Board, r: &Rules, min_pads: usize) -> Vec<PlaneNet> {
    let mut pads = vec![0usize; b.nets.len()];
    for f in &b.fps {
        for pd in &f.pads {
            pads[pd.net] += 1;
        }
    }
    let mut out = Vec::new();
    for n in 1..b.nets.len() {
        let zs: Vec<&Zone> = b
            .zones
            .iter()
            .filter(|z| z.net == n && !z.keepout)
            .collect();
        let plane = zs.iter().any(|z| z.layers & 0x1e != 0);
        let cand = zs.is_empty() && power_like(&b.nets[n]) && pads[n] >= min_pads;
        if zs.is_empty() && !cand {
            continue;
        }
        out.push(PlaneNet {
            net: b.nets[n].clone(),
            class: r.class_of(n).name.clone(),
            pads: pads[n],
            zones: zs
                .iter()
                .map(|z| {
                    serde_json::json!({
                        "name": z.name,
                        "layers": (0..NL).filter(|l| z.layers & (1 << l) != 0).map(|l| CU[l]).collect::<Vec<_>>(),
                        "priority": z.priority, "locked": z.locked,
                        "filled": !z.fills.is_empty(),
                        "area_mm2": r3(poly_area(&z.outline)),
                    })
                })
                .collect(),
            plane,
            pour: !zs.is_empty(),
            candidate: cand,
        });
    }
    out.sort_by_key(|p| std::cmp::Reverse(p.pads));
    out
}

/// per zone and layer
#[derive(Debug, Clone, Serialize)]
pub struct LayerReport {
    pub zone: String,
    pub layer: String,
    pub outline_mm2: f64,
    pub fill_mm2: f64,
    /// share of the outline (inside the board) that is not filled
    pub lost_pct: f64,
    pub islands: usize,
    pub isolated_islands: usize,
    pub islands_without_via: usize,
    /// share of the fill in the largest island
    pub largest_pct: f64,
    pub stitch_coverage_pct: f64,
    pub narrowest_neck_mm: Option<f64>,
    pub island_list: Vec<Island>,
}

#[derive(Debug, Clone, Serialize)]
pub struct NetReport {
    pub net: String,
    pub layers: Vec<LayerReport>,
    /// copper components of the net with the native fills (pads per component, largest first)
    pub components: usize,
    pub unconnected_pads: Vec<String>,
    pub necks: Vec<Neck>,
}

pub struct ReportParams {
    pub fill: FillParams,
    pub stitch_radius: f64,
    pub necks: usize,
    pub max_islands: usize,
}

/// plane report of net `net`: native fill of all its zones, islands, connectivity of its pads
/// through all copper (tracks, vias, fills), necks and stitching coverage. Also returns the
/// board with the native fills (for `render`).
pub fn report(
    b: &Board,
    r: &Rules,
    cfg: &model::BoardCfg,
    net: usize,
    rp: &ReportParams,
) -> (NetReport, Board) {
    let m = Model::new(b, r, cfg.clone(), model::CopperMode::All);
    let sel = |zi: usize| b.zones[zi].net == net;
    let fills = fill_zones(&m, &sel, &rp.fill);
    let a1 = rp.fill.res * rp.fill.res;
    let per: Vec<(LayerReport, Vec<Neck>, Vec<Vec<P>>)> = fills
        .par_iter()
        .map(|f| {
            let fa: f64 = f.islands.iter().map(|i| i.area_mm2).sum();
            let oa = f.outline_cells as f64 * a1;
            let big = f.islands.iter().map(|i| i.area_mm2).fold(0.0, f64::max);
            let necks = f.necks();
            let mut il = f.islands.clone();
            il.sort_by(|a, b| b.area_mm2.total_cmp(&a.area_mm2));
            il.truncate(rp.max_islands);
            (
                LayerReport {
                    zone: b.zones[f.zone].name.clone(),
                    layer: CU[f.layer].to_string(),
                    outline_mm2: r3(oa),
                    fill_mm2: r3(fa),
                    lost_pct: if oa > 0.0 {
                        r3(100.0 * (1.0 - fa / oa).max(0.0))
                    } else {
                        0.0
                    },
                    islands: f.islands.len(),
                    isolated_islands: f.islands.iter().filter(|i| !i.connected).count(),
                    islands_without_via: f
                        .islands
                        .iter()
                        .filter(|i| i.connected && !i.has_via)
                        .count(),
                    largest_pct: if fa > 0.0 { r3(100.0 * big / fa) } else { 0.0 },
                    stitch_coverage_pct: r3(100.0 * f.stitch_coverage(rp.stitch_radius)),
                    narrowest_neck_mm: necks.first().map(|n| n.width_mm),
                    island_list: il,
                },
                necks,
                f.polygons(false),
            )
        })
        .collect();
    drop(m);
    // board with native fills for connectivity
    let mut nb = b.clone();
    for z in nb.zones.iter_mut().filter(|z| z.net == net) {
        z.fills.clear();
    }
    for (f, (_, _, ps)) in fills.iter().zip(&per) {
        for pl in ps {
            nb.zones[f.zone].fills.push((f.layer, pl.clone()));
        }
    }
    let (components, unconnected) = {
        let m2 = Model::new(&nb, r, cfg.clone(), model::CopperMode::All);
        let (pins, _) = model::pins_and_copper_of(&m2, &|n| n == net);
        let ps = pins.get(&net).cloned().unwrap_or_default();
        let mut cnt: std::collections::HashMap<usize, usize> = Default::default();
        for p in &ps {
            *cnt.entry(p.comp).or_default() += 1;
        }
        let main = cnt.iter().max_by_key(|(_, k)| **k).map(|(c, _)| *c);
        let mut un: Vec<String> = ps
            .iter()
            .filter(|p| Some(p.comp) != main)
            .map(|p| {
                let f = &nb.fps[p.fp];
                format!("{}.{}", f.refd, f.pads[p.pad].num)
            })
            .collect();
        un.sort();
        un.dedup();
        (cnt.len(), un)
    };
    let mut necks: Vec<Neck> = per.iter().flat_map(|(_, n, _)| n.clone()).collect();
    necks.sort_by(|a, b| a.width_mm.total_cmp(&b.width_mm));
    necks.truncate(rp.necks);
    (
        NetReport {
            net: b.nets[net].clone(),
            layers: per.into_iter().map(|(l, _, _)| l).collect(),
            components,
            unconnected_pads: unconnected,
            necks,
        },
        nb,
    )
}

// ---------------------------------------------------------------------------------------------
// zone outlines: create, grow, cut back from HV copper, merge

pub struct OutlineParams {
    pub res: f64,
    /// grow the zone by at most this distance into free area (mm); None = no growth,
    /// Some(INFINITY) = the whole free area connected to the zone
    pub grow: Option<f64>,
    /// nets whose copper the outline keeps `creepage` away from (all layers' copper on this
    /// layer; on outer layers also their pads)
    pub hv: Vec<usize>,
    pub creepage: f64,
    /// keep this far from zones of other nets on the layer (on top of the rule clearance)
    pub margin: f64,
    /// parts of the outline narrower than this are dropped (mm)
    pub min_neck: f64,
    /// outlines smaller than this are dropped (mm^2)
    pub min_area: f64,
}

/// new outline(s) for a zone of net `net` on layer `l`: the start region `seed` (polygon),
/// grown into the free area of the layer, minus HV copper grown by the creepage distance.
/// Free area: inside the board (edge clearance), outside pour keep-outs and outside the zones of
/// other nets on the layer (rule clearance + margin). Returns outer outlines (no holes) and the
/// holes (HV islands inside the outline) as separate polygons.
pub fn outline(
    m: &Model,
    net: usize,
    lm: u8,
    seeds: &[Vec<P>],
    skip_zones: &[usize],
    op: &OutlineParams,
) -> (Vec<Vec<P>>, Vec<Vec<P>>) {
    let b = m.b;
    let bit = lm;
    let res = op.res;
    let clr = |o: usize| m.r.clr(net, o, true).max(m.r.clr(net, o, false));
    let bb = if b.bbox.valid() {
        b.bbox.inflate(2.0 * res)
    } else {
        let mut x = BBox::empty();
        seeds.iter().flatten().for_each(|q| x.add(*q));
        x.inflate(2.0)
    };
    let gs = GridSpec::new(bb, res);
    let n = gs.n();
    let inb = board_mask(b, &gs);
    let mut free = inb.clone();
    // the board edge clearance is left to the filler (the outline may run along the edge)
    {
        let mut blk = |s: &Shape, d: f64| gs.stamp(s, d, |x, y| free[gs.idx(x, y)] = false);
        for (zj, o) in b.zones.iter().enumerate() {
            if skip_zones.contains(&zj) || o.layers & bit == 0 || o.outline.len() < 3 {
                continue;
            }
            let s = Shape::Poly {
                pts: o.outline.clone(),
                r: 0.0,
            };
            if o.keepout {
                if o.ko_pour {
                    blk(&s, 0.0);
                }
            } else if o.net != net {
                blk(&s, clr(o.net) + op.margin);
            }
        }
    }
    // HV cut
    let mut hv = vec![false; n];
    if !op.hv.is_empty() && op.creepage > 0.0 {
        for o in &m.objs {
            if o.layers & bit == 0 || !op.hv.contains(&o.net) {
                continue;
            }
            if matches!(
                o.kind,
                Kind::Pad { .. } | Kind::Track | Kind::Via | Kind::Zone
            ) {
                gs.stamp(&o.shape, op.creepage, |x, y| hv[gs.idx(x, y)] = true);
            }
        }
    }
    let mut seedm = vec![false; n];
    for seed in seeds {
        gs.fill_poly(seed, &mut |x, y| seedm[gs.idx(x, y)] = true);
    }
    let ok: Vec<bool> = (0..n).map(|i| free[i] && !hv[i]).collect();
    let region: Vec<bool> = match op.grow {
        None => (0..n).map(|i| seedm[i] && ok[i]).collect(),
        Some(g) => {
            // free cells connected to the seed within distance g of it
            let ds = edt(&seedm, gs.nx, gs.ny);
            let gc = if g.is_finite() {
                ((g / res).powi(2)) as f32
            } else {
                f32::INFINITY
            };
            let cand: Vec<bool> = (0..n).map(|i| ok[i] && ds[i] <= gc).collect();
            let (lab, _) = label(&cand, gs.nx, gs.ny, true);
            let mut keep = std::collections::HashSet::new();
            for i in 0..n {
                if seedm[i] && lab[i] > 0 {
                    keep.insert(lab[i]);
                }
            }
            (0..n).map(|i| keep.contains(&lab[i])).collect()
        }
    };
    // drop parts narrower than min_neck (opening), then shrink by one cell so the traced
    // outline (through cell centres, simplified) stays inside
    let rn = (op.min_neck / 2.0 / res).powi(2) as f32;
    let nreg: Vec<bool> = region.iter().map(|v| !v).collect();
    let dr = edt(&nreg, gs.nx, gs.ny);
    let er: Vec<bool> = (0..n).map(|i| dr[i] > rn).collect();
    let de = edt(&er, gs.nx, gs.ny);
    let opened: Vec<bool> = (0..n).map(|i| region[i] && de[i] <= rn + 1e-3).collect();
    let nop: Vec<bool> = opened.iter().map(|v| !v).collect();
    let dr = edt(&nop, gs.nx, gs.ny);
    let core: Vec<bool> = (0..n).map(|i| dr[i] > 2.0).collect();
    // the outer ring is written as the zone outline, holes become pour keep-outs
    let mut outers = Vec::new();
    let mut holes = Vec::new();
    // only holes made by the HV cut need keep-outs (the filler avoids everything else anyway)
    let (hvl, nhv) = label(&hv, gs.nx, gs.ny, true);
    let mut hv_pts = vec![None; nhv as usize + 1];
    for i in 0..n {
        let k = hvl[i] as usize;
        if k > 0 && hv_pts[k].is_none() {
            hv_pts[k] = Some(gs.center(i % gs.nx, i / gs.nx));
        }
    }
    for (o, h) in rings(&core, &gs, res * 0.7, 4) {
        if poly_area(&o) < op.min_area {
            continue;
        }
        outers.push(o);
        holes.extend(
            h.into_iter()
                .filter(|r| hv_pts.iter().flatten().any(|q| point_in_poly(*q, r))),
        );
    }
    (outers, holes)
}

// ---------------------------------------------------------------------------------------------
// writing zones into the board text

/// spans of the top-level `(zone ...)` blocks in a board text
pub fn zone_spans(text: &str) -> Vec<(usize, usize)> {
    top_spans(text, "zone")
}

/// spans of the direct children `(head ...)` of the top-level list
fn top_spans(text: &str, head: &str) -> Vec<(usize, usize)> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut i = 0;
    let mut in_str = false;
    let mut start = 0;
    while i < bytes.len() {
        let c = bytes[i];
        if in_str {
            if c == b'\\' {
                i += 1;
            } else if c == b'"' {
                in_str = false;
            }
        } else if c == b'"' {
            in_str = true;
        } else if c == b'(' {
            depth += 1;
            if depth == 2 && text[i + 1..].starts_with(head) {
                let after = bytes.get(i + 1 + head.len()).copied().unwrap_or(b' ');
                if after.is_ascii_whitespace() || after == b')' {
                    start = i;
                }
            }
        } else if c == b')' {
            if depth == 2 && start > 0 {
                out.push((start, i + 1));
                start = 0;
            }
            depth -= 1;
        }
        i += 1;
    }
    out
}

fn uuid_from(seed: &mut u64) -> String {
    let mut nx = || {
        *seed ^= *seed << 13;
        *seed ^= *seed >> 7;
        *seed ^= *seed << 17;
        *seed
    };
    let (a, b) = (nx(), nx());
    format!(
        "{:08x}-{:04x}-4{:03x}-8{:03x}-{:012x}",
        a >> 32,
        (a >> 16) & 0xffff,
        a & 0xfff,
        b >> 52,
        b & 0xffff_ffff_ffff
    )
}

/// `(pts ...)` block at indentation depth 3
fn pts_only(pts: &[P]) -> String {
    let mut s = String::from("\t\t\t(pts\n\t\t\t\t");
    for (k, q) in pts.iter().enumerate() {
        if k > 0 && k % 6 == 0 {
            s += "\n\t\t\t\t";
        } else if k > 0 {
            s += " ";
        }
        s += &format!("(xy {:.4} {:.4})", q.x, q.y);
    }
    s += "\n\t\t\t)\n";
    s
}

fn pts_text(pts: &[P]) -> String {
    format!("\t\t(polygon\n{}\t\t)\n", pts_only(pts))
}

/// the board text with the outline of zone `name` replaced by `pts` (its saved fills dropped);
/// `extra` further outlines become copies of the zone (same settings, new uuid, name suffix)
pub fn set_zone_outline(
    text: &str,
    name: &str,
    pts: &[P],
    extra: &[Vec<P>],
    seed: &mut u64,
) -> Option<String> {
    for (s, e) in zone_spans(text) {
        let blk = &text[s..e];
        let nm = crate::sexpr::parse(blk)
            .ok()
            .and_then(|l| l.val("name").map(String::from))
            .unwrap_or_default();
        if nm != name {
            continue;
        }
        let with = |pts: &[P]| -> String {
            let mut out = String::new();
            let mut last = 0;
            let mut spans: Vec<(usize, usize, bool)> = top_spans(blk, "polygon")
                .into_iter()
                .map(|(a, b)| (a, b, true))
                .chain(
                    top_spans(blk, "filled_polygon")
                        .into_iter()
                        .map(|(a, b)| (a, b, false)),
                )
                .collect();
            spans.sort();
            let mut done = false;
            for (a, b, poly) in spans {
                out.push_str(blk[last..a].trim_end_matches(['\t', ' ']));
                if poly && !done {
                    out.push_str(pts_text(pts).trim_start_matches('\n'));
                    done = true;
                }
                last = b;
                if blk[last..].starts_with('\n') {
                    last += 1;
                }
            }
            out.push_str(&blk[last..]);
            out
        };
        let mut add = String::new();
        for (k, x) in extra.iter().enumerate() {
            let mut t = with(x);
            if let Some(u0) = t.find("(uuid \"") {
                let u1 = t[u0 + 7..].find('"').map(|i| u0 + 7 + i).unwrap_or(u0 + 7);
                t.replace_range(u0 + 7..u1, &uuid_from(seed));
            }
            t = t.replacen(
                &format!("(name {})", crate::sexpr::q(name)),
                &format!("(name {})", crate::sexpr::q(&format!("{name}_{}", k + 2))),
                1,
            );
            add += "\n\t";
            add += &t;
        }
        let mut out = String::with_capacity(text.len());
        out.push_str(&text[..s]);
        out.push_str(&with(pts));
        out.push_str(&add);
        out.push_str(&text[e..]);
        return Some(out);
    }
    None
}

/// KiCad text of a copper zone (unfilled; KiCad fills it)
pub fn zone_text(
    net: &str,
    layer: &str,
    name: &str,
    priority: i64,
    min_width: f64,
    clearance: f64,
    pts: &[P],
    seed: &mut u64,
) -> String {
    format!(
        "\t(zone\n\t\t(net {})\n\t\t(layer \"{layer}\")\n\t\t(uuid \"{}\")\n\t\t(name {})\n\t\t(hatch edge 0.5)\n\t\t(priority {priority})\n\t\t(connect_pads\n\t\t\t(clearance {clearance})\n\t\t)\n\t\t(min_thickness {min_width})\n\t\t(filled_areas_thickness no)\n\t\t(fill\n\t\t\t(thermal_gap 0.5)\n\t\t\t(thermal_bridge_width 0.5)\n\t\t)\n{}\t)\n",
        crate::sexpr::q(net),
        uuid_from(seed),
        crate::sexpr::q(name),
        pts_text(pts)
    )
}

/// KiCad text of a pour keep-out rule area on one layer (tracks and vias allowed)
pub fn keepout_text(layer: &str, name: &str, pts: &[P], seed: &mut u64) -> String {
    format!(
        "\t(zone\n\t\t(layer \"{layer}\")\n\t\t(uuid \"{}\")\n\t\t(name {})\n\t\t(hatch edge 0.5)\n\t\t(connect_pads\n\t\t\t(clearance 0)\n\t\t)\n\t\t(min_thickness 0.25)\n\t\t(filled_areas_thickness no)\n\t\t(keepout\n\t\t\t(tracks allowed)\n\t\t\t(vias allowed)\n\t\t\t(pads allowed)\n\t\t\t(copperpour not_allowed)\n\t\t\t(footprints allowed)\n\t\t)\n\t\t(fill\n\t\t\t(thermal_gap 0.5)\n\t\t\t(thermal_bridge_width 0.5)\n\t\t)\n{}\t)\n",
        uuid_from(seed),
        crate::sexpr::q(name),
        pts_text(pts)
    )
}

pub fn seed_of(text: &str) -> u64 {
    let mut seed = 0x9e37_79b9_7f4a_7c15u64;
    for b in text.bytes().step_by(89).chain(text.len().to_le_bytes()) {
        seed = (seed ^ b as u64).wrapping_mul(0x100_0000_01b3);
    }
    seed | 1
}

/// remove the zones with the given names (exact) from the text, then append `add` before the
/// final parenthesis
pub fn replace_zones(text: &str, remove: &[String], add: &str) -> String {
    let mut out = String::with_capacity(text.len() + add.len());
    let mut last = 0;
    for (s, e) in zone_spans(text) {
        let blk = &text[s..e];
        let name = crate::sexpr::parse(blk)
            .ok()
            .and_then(|l| l.val("name").map(String::from))
            .unwrap_or_default();
        if !name.is_empty() && remove.contains(&name) {
            out.push_str(&text[last..s]);
            // drop the indentation before the block and the newline after it
            while out.ends_with('\t') || out.ends_with(' ') {
                out.pop();
            }
            last = e;
            if text[last..].starts_with('\n') {
                last += 1;
            } else if text[last..].starts_with("\r\n") {
                last += 2;
            }
        }
    }
    out.push_str(&text[last..]);
    let end = out.rfind(')').unwrap_or(out.len());
    out.insert_str(end, add);
    out
}

/// replace the saved fills of the zones in the text: every `filled_polygon` child of a zone
/// is removed and the native fill polygons of `fills` (zone name -> (layer, polygon)) are added
pub fn write_fills(text: &str, b: &Board) -> String {
    let mut out = String::with_capacity(text.len());
    let mut last = 0;
    let spans = zone_spans(text);
    // zones in file order (top level only: the board parser keeps footprint zones apart)
    let top: Vec<&Zone> = b.zones.iter().filter(|z| z.fp.is_none()).collect();
    for (k, (s, e)) in spans.iter().enumerate() {
        let Some(z) = top.get(k) else { break };
        if z.keepout || z.net == 0 {
            continue;
        }
        out.push_str(&text[last..*s]);
        let blk = &text[*s..*e];
        // strip old fills
        let mut body = String::new();
        let mut l2 = 0;
        for (fs, fe) in top_spans(blk, "filled_polygon") {
            body.push_str(blk[l2..fs].trim_end_matches(['\t', ' ']));
            l2 = fe;
            if blk[l2..].starts_with('\n') {
                l2 += 1;
            }
        }
        body.push_str(&blk[l2..]);
        let end = body.rfind(')').unwrap_or(body.len());
        let mut fills = String::new();
        for (l, pts) in &z.fills {
            fills += &format!(
                "\t\t(filled_polygon\n\t\t\t(layer \"{}\")\n{}\t\t)\n",
                CU[*l],
                pts_only(pts)
            );
        }
        body.insert_str(end, &fills);
        out.push_str(&body);
        last = *e;
    }
    out.push_str(&text[last..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edt_matches_brute_force() {
        let (nx, ny) = (13, 9);
        let mut f = vec![false; nx * ny];
        for &(x, y) in &[(1, 1), (10, 7), (6, 3)] {
            f[y * nx + x] = true;
        }
        let d = edt(&f, nx, ny);
        for y in 0..ny {
            for x in 0..nx {
                let bf = [(1, 1), (10, 7), (6, 3)]
                    .iter()
                    .map(|&(a, b): &(i64, i64)| (x as i64 - a).pow(2) + (y as i64 - b).pow(2))
                    .min()
                    .unwrap();
                assert_eq!(d[y * nx + x], bf as f32, "{x},{y}");
            }
        }
    }

    #[test]
    fn ring_with_hole_is_fractured_and_unfractured() {
        let gs = GridSpec::new(
            BBox {
                x0: 0.0,
                y0: 0.0,
                x1: 2.0,
                y1: 2.0,
            },
            0.1,
        );
        let mut m = vec![false; gs.n()];
        for y in 2..18 {
            for x in 2..18 {
                if !(7..13).contains(&x) || !(7..13).contains(&y) {
                    m[gs.idx(x, y)] = true;
                }
            }
        }
        let ps = polygons(&m, &gs, 0.05, 1);
        assert_eq!(ps.len(), 1);
        let pl = &ps[0];
        // a point in the hole is outside, a point in the ring inside (even-odd)
        assert!(!point_in_poly(p(1.0, 1.0), pl));
        assert!(point_in_poly(p(0.4, 1.0), pl));
        let r = rings(&m, &gs, 0.05, 1);
        let (o, h) = (&r[0].0, &r[0].1);
        assert_eq!(h.len(), 1);
        assert!(point_in_poly(p(1.0, 1.0), o));
        assert!(point_in_poly(p(1.0, 1.0), &h[0]));
    }

    #[test]
    fn trace_single_cell_and_bar() {
        let mem = |x: i64, y: i64| (0..5).contains(&x) && y == 0;
        let t = trace(&mem, (0, 0));
        assert_eq!(t.first(), Some(&(0, 0)));
        assert!(t.contains(&(4, 0)));
        let one = |x: i64, y: i64| x == 3 && y == 3;
        assert_eq!(trace(&one, (3, 3)), vec![(3, 3)]);
    }

    #[test]
    fn top_level_zone_spans() {
        let t = "(kicad_pcb (net 1 \"a(\") (zone (name \"z\") (polygon (pts))) (footprint (zone)) (zone))";
        let s = zone_spans(t);
        assert_eq!(s.len(), 2);
        assert!(t[s[0].0..s[0].1].starts_with("(zone (name"));
    }
}
