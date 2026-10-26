//! Second connection for starved thermal reliefs: a pad whose native fill keeps only one spoke
//! (`starved_thermal` in kicad-cli) gets a short straight track from its centre into the zone
//! fill of its net. kicad-cli counts a track that leaves the pad as a spoke, so the pad then
//! meets the minimum of two without changing the zone's connection style.
//!
//! Spokes are counted as kicad-cli 10 does: per fill polygon, the crossings of its outline with
//! the pad outline grown by half the relief gap; spokes into an isolated island (a polygon that
//! touches no other item of the net) only count when the pad has no other spoke or track; a
//! track counts when it starts in the pad and ends in the zone's fill. Stubs end in connected
//! fill.

use crate::board::Zone;
use crate::drc::MIN_SPOKES;
use crate::droute::{RNet, RSeg};
use crate::geom::{shape_dist, BBox, PolyIndex, Shape, P};
use crate::model::{Kind, Model};
use std::collections::HashMap;

/// a candidate stub keeps this margin (mm) beyond every clearance (raster and rounding of the fill)
const MARGIN: f64 = 0.01;

/// fill polygons of one zone on one layer, with the polygons that connect to at least two items
/// of the net (pads, vias, tracks); the others are isolated islands
pub struct LayerFill {
    polys: Vec<PolyIndex>,
    pub connected: Vec<bool>,
}

/// how far (mm) from an item's copper a fill polygon still counts as connected to it: the fill
/// raster (spokes overlap the pad; a neighbour behind its relief gap does not count)
const REACH: f64 = 0.08;

fn touches(pi: &PolyIndex, sh: &Shape, d: f64, buf: &mut Vec<u32>) -> bool {
    let sb = sh.bbox().inflate(d);
    if !pi.bbox.overlaps(&sb) {
        return false;
    }
    pi.edges_near(&sb, buf);
    let n = pi.pts.len();
    buf.iter().any(|&e| {
        let (a, b) = (pi.pts[e as usize], pi.pts[(e as usize + 1) % n]);
        shape_dist(sh, &Shape::Capsule { a, b, r: 0.0 }) <= d
    }) || pi.contains(sh.bbox().center())
}

impl LayerFill {
    pub fn new(m: &Model, z: &Zone, l: usize) -> LayerFill {
        let polys: Vec<PolyIndex> = z
            .fills
            .iter()
            .filter(|(fl, _)| *fl == l)
            .map(|(_, p)| PolyIndex::new(p.clone()))
            .collect();
        let mut hits = vec![0usize; polys.len()];
        let mut buf = Vec::new();
        for o in &m.objs {
            if o.net != z.net || o.layers & (1 << l) == 0 {
                continue;
            }
            if !matches!(o.kind, Kind::Pad { .. } | Kind::Via | Kind::Track) {
                continue;
            }
            for (i, p) in polys.iter().enumerate() {
                if hits[i] < 2 && touches(p, &o.shape, REACH, &mut buf) {
                    hits[i] += 1;
                }
            }
        }
        LayerFill {
            polys,
            connected: hits.iter().map(|&h| h >= 2).collect(),
        }
    }

    fn in_any(&self, q: P) -> bool {
        self.polys.iter().any(|p| p.contains(q))
    }

    fn in_connected(&self, q: P) -> bool {
        (0..self.polys.len()).any(|i| self.connected[i] && self.polys[i].contains(q))
    }

    /// spokes of a pad into connected fill and into islands: runs of samples on the pad outline
    /// grown by `mid` (half the relief gap) that lie inside one polygon
    fn spokes(&self, c: P, shapes: &[Shape], mid: f64) -> (usize, usize) {
        let ring = ring(c, shapes, mid);
        let mut bb = BBox::empty();
        ring.iter().for_each(|&q| bb.add(q));
        let (mut main, mut island) = (0, 0);
        for i in 0..self.polys.len() {
            if !self.polys[i].bbox.overlaps(&bb) {
                continue;
            }
            let inside: Vec<bool> = ring.iter().map(|&q| self.polys[i].contains(q)).collect();
            let n = inside.len();
            let runs = (0..n)
                .filter(|&k| inside[k] && !inside[(k + n - 1) % n])
                .count();
            if self.connected[i] {
                main += runs;
            } else {
                island += runs;
            }
        }
        (main, island)
    }
}

/// points on the outline of the pad grown by `mid`: one per 0.5 degrees around the centre
fn ring(c: P, shapes: &[Shape], mid: f64) -> Vec<P> {
    let dist = |q: P| {
        shapes
            .iter()
            .map(|s| s.dist(q))
            .fold(f64::INFINITY, f64::min)
    };
    (0..720)
        .map(|k| {
            let d = P { x: 1.0, y: 0.0 }.rot(k as f64 * 0.5);
            let (mut lo, mut hi) = (0.0, 0.05);
            while dist(c + d * hi) < mid && hi < 50.0 {
                lo = hi;
                hi *= 2.0;
            }
            for _ in 0..30 {
                let t = 0.5 * (lo + hi);
                if dist(c + d * t) < mid {
                    lo = t;
                } else {
                    hi = t;
                }
            }
            c + d * hi
        })
        .collect()
}

/// tracks of the net on layer `l` that start in the pad (grown by `mid`) and end in the zone's
/// fill: kicad-cli counts each as a spoke
fn tracks_in(m: &Model, net: usize, l: usize, shapes: &[Shape], mid: f64, lf: &LayerFill) -> usize {
    let inpad = |q: P| shapes.iter().any(|s| s.dist(q) <= mid);
    m.objs
        .iter()
        .filter(|o| matches!(o.kind, Kind::Track) && o.net == net && o.layers & (1 << l) != 0)
        .filter(|o| match o.shape {
            Shape::Capsule { a, b, .. } => {
                (inpad(a) && !inpad(b) && lf.in_any(b)) || (inpad(b) && !inpad(a) && lf.in_any(a))
            }
            _ => false,
        })
        .count()
}

/// kicad-cli's resolved spoke count from spokes into connected fill, into islands, and tracks
fn resolved(main: usize, island: usize, tracks: usize) -> usize {
    if main + tracks == 0 {
        island
    } else {
        main + tracks
    }
}

/// a thermal pad of the native fill: (spokes into connected fill, into islands, tracks); None
/// when the fill does not reach it at all (unconnected, not starved)
fn pad_spokes(
    m: &Model,
    z: &Zone,
    lf: &LayerFill,
    l: usize,
    fi: usize,
    pad: usize,
) -> Option<(usize, usize, usize)> {
    let f = &m.b.fps[fi];
    let pd = &f.pads[pad];
    let (c, _) = f.pad_world(pd);
    let shapes = f.pad_shapes(pd);
    let mid = pd.thermal_gap.unwrap_or(z.thermal_gap) / 2.0;
    let (main, island) = lf.spokes(c, &shapes, mid);
    if main + island == 0 {
        return None;
    }
    Some((main, island, tracks_in(m, z.net, l, &shapes, mid, lf)))
}

/// stub tracks per net for the thermal pads of the native fill (`Zone::spokes`) that resolve to
/// fewer than [`MIN_SPOKES`] spokes; pads where no direction is legal are left as they are
pub fn stubs(m: &Model) -> HashMap<usize, RNet> {
    let mut out: HashMap<usize, RNet> = HashMap::new();
    let min = MIN_SPOKES as usize;
    for z in &m.b.zones {
        let mut fills: HashMap<usize, LayerFill> = HashMap::new();
        for &(l, fi, pad, _) in &z.spokes {
            let lf = fills.entry(l).or_insert_with(|| LayerFill::new(m, z, l));
            let Some((main, island, tracks)) = pad_spokes(m, z, lf, l, fi, pad) else {
                continue;
            };
            // fewest stubs that resolve the pad (a track makes island spokes stop counting)
            let need = (0..=min)
                .find(|k| resolved(main, island, tracks + k) >= min)
                .unwrap_or(min);
            if need == 0 {
                continue;
            }
            let mut made: Vec<RSeg> = Vec::new();
            for s in stub_dirs(m, z, lf, l, fi, pad) {
                // a second stub at least 60 degrees away from the first
                let ok = made.iter().all(|t| {
                    let (u, v) = (t.b - t.a, s.b - s.a);
                    u.dot(v) / (u.len() * v.len()) < 0.5
                });
                if ok {
                    made.push(s);
                }
                if made.len() == need {
                    break;
                }
            }
            out.entry(z.net).or_default().segs.extend(made);
        }
    }
    out
}

/// starved thermal pads of zone `z` (kicad-cli rules): (layer, footprint, pad, resolved spokes,
/// island spokes ignored)
pub fn starved(m: &Model, z: &Zone) -> Vec<(usize, usize, usize, usize, usize)> {
    let mut fills: HashMap<usize, LayerFill> = HashMap::new();
    let mut out = Vec::new();
    for &(l, fi, pad, _) in &z.spokes {
        let lf = fills.entry(l).or_insert_with(|| LayerFill::new(m, z, l));
        if let Some((main, island, tracks)) = pad_spokes(m, z, lf, l, fi, pad) {
            let r = resolved(main, island, tracks);
            if r < MIN_SPOKES as usize {
                out.push((l, fi, pad, r, if main + tracks == 0 { 0 } else { island }));
            }
        }
    }
    out
}

/// legal stubs from the pad centre into connected fill, spoke directions first, then diagonals
fn stub_dirs(m: &Model, z: &Zone, lf: &LayerFill, l: usize, fi: usize, pad: usize) -> Vec<RSeg> {
    let f = &m.b.fps[fi];
    let pd = &f.pads[pad];
    let (c, rot) = f.pad_world(pd);
    let shapes = f.pad_shapes(pd);
    let gap = pd.thermal_gap.unwrap_or(z.thermal_gap);
    let (wmin, _) = m.r.track_width(z.net);
    let w = pd.thermal_bridge.unwrap_or(z.thermal_bridge).max(wmin);
    let base = pd.spoke_angle + rot;
    let dirs = (0..4)
        .map(|k| base + 90.0 * k as f64)
        .chain((0..4).map(|k| base + 45.0 + 90.0 * k as f64));
    // (stub, ends in connected fill)
    let mut out: Vec<(RSeg, bool)> = Vec::new();
    for deg in dirs {
        let d = P { x: 1.0, y: 0.0 }.rot(deg);
        // leave the pad, cross the relief gap, end a track width inside the fill; for connected
        // fill look up to 3 mm further (across an island to the fill behind it)
        let mut t = 0.0;
        while t < 20.0 && shapes.iter().any(|s| s.dist(c + d * t) <= 0.0) {
            t += 0.01;
        }
        let k0 = t + gap + w + MARGIN;
        let ends = |k: f64, f: &dyn Fn(P) -> bool| f(c + d * k) && f(c + d * (k - w / 2.0));
        let mut k = k0;
        let mut found = None;
        while k < k0 + 3.0 {
            if ends(k, &|q| lf.in_connected(q)) {
                found = Some((k, true));
                break;
            }
            k += 0.05;
        }
        if found.is_none() && ends(k0, &|q| lf.in_any(q)) {
            found = Some((k0, false));
        }
        if let Some((k, conn)) = found {
            let e = c + d * k;
            let sh = Shape::Capsule {
                a: c,
                b: e,
                r: w / 2.0,
            };
            if legal(m, z.net, l, &sh) {
                out.push((RSeg { a: c, b: e, l, w }, conn));
            }
        }
    }
    // connected fill first
    out.sort_by_key(|x| !x.1);
    out.into_iter().map(|x| x.0).collect()
}

/// clearances of a new track `sh` of net `net` on layer `l`: foreign copper (holes with the hole
/// clearance), rule areas without tracks, the board edge
fn legal(m: &Model, net: usize, l: usize, sh: &Shape) -> bool {
    let bb = sh.bbox();
    for o in &m.objs {
        let on_layer = o.layers & (1 << l) != 0;
        match o.kind {
            Kind::Keepout { tracks: true, .. } if on_layer => {
                if o.bbox.overlaps(&bb) && shape_dist(&o.shape, sh) <= 0.0 {
                    return false;
                }
            }
            Kind::Pad { .. } | Kind::Track | Kind::Via if o.net != net || net == 0 => {
                let reach = m.r.min_hole_clearance.max(2.0) + MARGIN;
                if !o.bbox.inflate(reach).overlaps(&bb) {
                    continue;
                }
                if on_layer && shape_dist(&o.shape, sh) < m.clr(net, o, l).max(0.0) + MARGIN {
                    return false;
                }
                if o.hole_r > 0.0 {
                    let hole = match o.shape {
                        Shape::Capsule { a, b, .. } => Shape::Capsule { a, b, r: o.hole_r },
                        _ => Shape::circle(o.bbox.center(), o.hole_r),
                    };
                    if shape_dist(&hole, sh) < m.r.min_hole_clearance + MARGIN {
                        return false;
                    }
                }
            }
            _ => {}
        }
    }
    let ec = m.r.edge_clearance(net);
    if let Shape::Capsule { a, b, r } = *sh {
        for &(p, q) in &m.b.edge_segs {
            if crate::geom::seg_seg_dist(a, b, p, q) - r < ec + MARGIN {
                return false;
            }
        }
    }
    true
}
