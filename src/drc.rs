//! Native copper DRC: the copper rules of KiCad's DRC (`kicad-cli pcb drc`) on the model, in
//! milliseconds instead of seconds. Reports violations with KiCad's type names so reports can be
//! compared:
//!
//! - `shorting_items`: copper of two nets overlaps (`tracks_crossing` when two track centrelines
//!   cross);
//! - `clearance`: copper of two nets closer than the rule (net classes, DRU, pad-local);
//! - `hole_clearance`: a drilled hole closer to copper of another net than `min_hole_clearance`;
//! - `hole_to_hole`: two drilled holes closer than `min_hole_to_hole` (edge to edge);
//! - `copper_edge_clearance`: copper closer to Edge.Cuts than the edge clearance;
//! - `track_width`: a track narrower than the rule minimum;
//! - `via_diameter`, `annular_width` (vias), `drill_out_of_range` (holes) against the board
//!   setup minimums.
//!
//! Like KiCad, a violation needs `actual < required - epsilon` (epsilon 0.0005 mm), items of one
//! net never collide, net-less pads collide with every net, and each item pair is reported once
//! (the first layer they share). Collisions (short, crossing) are reported once per net pair and
//! only between nets that have pads (checked against kicad-cli 10 on synthetic boards). Zone fills are not checked against other copper (KiCad's filler
//! keeps their clearance by construction); unconnected items come from the connectivity pass
//! (`opens`). Not covered: silk, courtyard, mask, footprint library and annular-ring checks.

use crate::board::{is_outer, CU};
use crate::geom::{shape_dist, BBox, Shape, P};
use crate::model::{Kind, Model, Obj};
use std::collections::HashMap;

/// KiCad's default DRC epsilon (advanced config `DRCEpsilon`), mm
pub const EPS: f64 = 0.0005;

#[derive(Debug, Clone)]
pub struct Violation {
    pub typ: &'static str,
    /// required and actual value (mm)
    pub need: f64,
    pub actual: f64,
    pub layer: usize,
    pub at: P,
    /// descriptions of the one or two items
    pub items: Vec<String>,
}

/// copper items that take part: pads, tracks, vias (objs indices)
fn copper(m: &Model) -> Vec<usize> {
    (0..m.objs.len())
        .filter(|&i| matches!(m.objs[i].kind, Kind::Pad { .. } | Kind::Track | Kind::Via))
        .collect()
}

pub fn describe(m: &Model, o: &Obj) -> String {
    let net = |n: usize| {
        if n == 0 {
            "<no net>".to_string()
        } else {
            m.b.nets[n].clone()
        }
    };
    match o.kind {
        Kind::Pad { fp, pad } => format!(
            "Pad {} [{}] of {}",
            m.b.fps[fp].pads[pad].num,
            net(o.net),
            m.b.fps[fp].refd
        ),
        Kind::Via => format!("Via [{}]", net(o.net)),
        _ => format!("Track [{}]", net(o.net)),
    }
}

/// nearest point pair is not needed: a representative location (midpoint of the two bbox
/// centres, clamped to the overlap)
fn at(a: &BBox, b: &BBox) -> P {
    let c = |bb: &BBox| P {
        x: (bb.x0 + bb.x1) / 2.0,
        y: (bb.y0 + bb.y1) / 2.0,
    };
    let (p, q) = (c(a), c(b));
    P {
        x: (p.x + q.x) / 2.0,
        y: (p.y + q.y) / 2.0,
    }
}

fn seg_cross(a: P, b: P, c: P, d: P) -> bool {
    let o = |p: P, q: P, r: P| (q.x - p.x) * (r.y - p.y) - (q.y - p.y) * (r.x - p.x);
    let (d1, d2, d3, d4) = (o(c, d, a), o(c, d, b), o(a, b, c), o(a, b, d));
    ((d1 > 0.0 && d2 < 0.0) || (d1 < 0.0 && d2 > 0.0))
        && ((d3 > 0.0 && d4 < 0.0) || (d3 < 0.0 && d4 > 0.0))
}

/// uniform grid over item bboxes (inflated by `pad`), candidate pairs i < j
fn pairs(bbs: &[BBox], pad: f64, mut f: impl FnMut(usize, usize)) {
    let cs = 1.0f64.max(pad * 2.0);
    let mut grid: HashMap<(i32, i32), Vec<u32>> = HashMap::new();
    for (k, b) in bbs.iter().enumerate() {
        let b = b.inflate(pad);
        for x in (b.x0 / cs).floor() as i32..=(b.x1 / cs).floor() as i32 {
            for y in (b.y0 / cs).floor() as i32..=(b.y1 / cs).floor() as i32 {
                grid.entry((x, y)).or_default().push(k as u32);
            }
        }
    }
    let mut seen = std::collections::HashSet::new();
    for v in grid.values() {
        for (ii, &i) in v.iter().enumerate() {
            for &j in &v[ii + 1..] {
                let (i, j) = (i.min(j) as usize, i.max(j) as usize);
                if bbs[i].inflate(pad).overlaps(&bbs[j].inflate(pad)) && seen.insert((i, j)) {
                    f(i, j);
                }
            }
        }
    }
}

pub fn check(m: &Model) -> Vec<Violation> {
    let r = m.r;
    let ids = copper(m);
    let objs: Vec<&Obj> = ids.iter().map(|&i| &m.objs[i]).collect();
    let bbs: Vec<BBox> = objs.iter().map(|o| o.bbox).collect();
    let maxc = r
        .mat
        .iter()
        .flat_map(|v| v.iter())
        .fold(r.min_clearance, |a, &b| a.max(b as f64))
        .max(r.min_hole_clearance)
        .max(r.min_hole_to_hole)
        + 0.1;
    // KiCad reports a collision only between nets that have pads (net-less pads count), and
    // one per net pair (a short, else a crossing)
    let mut padded = vec![false; m.b.nets.len().max(1)];
    padded[0] = true;
    for o in &objs {
        if matches!(o.kind, Kind::Pad { .. }) {
            padded[o.net] = true;
        }
    }
    let mut out = Vec::new();
    let mut hits: HashMap<(usize, usize), Violation> = HashMap::new();
    pairs(&bbs, maxc / 2.0, |i, j| {
        let (a, b) = (objs[i], objs[j]);
        if a.net == b.net && a.net != 0 {
            return;
        }
        // copper clearance on the first shared layer
        let shared = a.layers & b.layers & m.b.cu_mask;
        if shared != 0 {
            let l = shared.trailing_zeros() as usize;
            let d = shape_dist(&a.shape, &b.shape);
            let mut need = r.clr(a.net, b.net, is_outer(l));
            for o in [a, b] {
                if let Kind::Pad { fp, pad } = o.kind {
                    need = need.max(m.b.fps[fp].pads[pad].local_clr);
                }
            }
            let typ = if d <= 0.0 {
                match (&a.kind, &b.kind, &a.shape, &b.shape) {
                    (
                        Kind::Track,
                        Kind::Track,
                        Shape::Capsule { a: p, b: q, .. },
                        Shape::Capsule { a: s, b: t, .. },
                    ) if seg_cross(*p, *q, *s, *t) => Some("tracks_crossing"),
                    _ => Some("shorting_items"),
                }
            } else if d < need - EPS {
                Some("clearance")
            } else {
                None
            };
            if let Some(typ) = typ {
                let v = Violation {
                    typ,
                    need,
                    actual: d.max(0.0),
                    layer: l,
                    at: at(&a.bbox, &b.bbox),
                    items: vec![describe(m, a), describe(m, b)],
                };
                if typ == "clearance" {
                    out.push(v);
                } else if padded[a.net] && padded[b.net] {
                    let key = (a.net.min(b.net), a.net.max(b.net));
                    let keep = hits
                        .get(&key)
                        .is_none_or(|h| h.typ == "tracks_crossing" && typ == "shorting_items");
                    if keep {
                        hits.insert(key, v);
                    }
                }
                return;
            }
        }
        // holes: hole to foreign copper, hole to hole
        let hole = |o: &Obj| match o.shape {
            Shape::Capsule { a, b, .. } if o.hole_r > 0.0 => {
                Some(Shape::Capsule { a, b, r: o.hole_r })
            }
            _ => None,
        };
        let (ha, hb) = (hole(a), hole(b));
        if let (Some(x), Some(y)) = (&ha, &hb) {
            let d = shape_dist(x, y);
            if r.min_hole_to_hole > 0.0 && d < r.min_hole_to_hole - EPS {
                out.push(Violation {
                    typ: "hole_to_hole",
                    need: r.min_hole_to_hole,
                    actual: d.max(0.0),
                    layer: 0,
                    at: at(&a.bbox, &b.bbox),
                    items: vec![describe(m, a), describe(m, b)],
                });
                return;
            }
        }
        if r.min_hole_clearance > 0.0 {
            for (h, o, ho) in [(&ha, b, a), (&hb, a, b)] {
                let Some(h) = h else { continue };
                if o.layers & m.b.cu_mask == 0 {
                    continue;
                }
                let d = shape_dist(h, &o.shape);
                if d < r.min_hole_clearance - EPS {
                    out.push(Violation {
                        typ: "hole_clearance",
                        need: r.min_hole_clearance,
                        actual: d.max(0.0),
                        layer: (o.layers & m.b.cu_mask).trailing_zeros() as usize,
                        at: at(&ho.bbox, &o.bbox),
                        items: vec![describe(m, ho), describe(m, o)],
                    });
                    return;
                }
            }
        }
    });
    let mut hits: Vec<_> = hits.into_iter().collect();
    hits.sort_by_key(|(k, _)| *k);
    out.extend(hits.into_iter().map(|(_, v)| v));
    // copper to board edge, track width
    for o in &objs {
        let need = r.edge_clearance(o.net);
        if need > 0.0 && !m.b.edge_segs.is_empty() {
            let bb = o.bbox.inflate(need);
            let mut d = f64::INFINITY;
            for &(p, q) in &m.b.edge_segs {
                let sb = BBox {
                    x0: p.x.min(q.x),
                    y0: p.y.min(q.y),
                    x1: p.x.max(q.x),
                    y1: p.y.max(q.y),
                };
                if sb.overlaps(&bb) {
                    d = d.min(shape_dist(&Shape::Capsule { a: p, b: q, r: 0.0 }, &o.shape));
                }
            }
            if d < need - EPS {
                out.push(Violation {
                    typ: "copper_edge_clearance",
                    need,
                    actual: d.max(0.0),
                    layer: (o.layers & m.b.cu_mask).trailing_zeros() as usize,
                    at: at(&o.bbox, &o.bbox),
                    items: vec![describe(m, o)],
                });
            }
        }
        // via size and holes against the board setup minimums
        let (md, ma, mh) = r.min_via;
        let hole_d = 2.0 * o.hole_r;
        let size = |v: &mut Vec<Violation>, typ: &'static str, need: f64, actual: f64| {
            if actual < need - EPS {
                v.push(Violation {
                    typ,
                    need,
                    actual,
                    layer: (o.layers & m.b.cu_mask).trailing_zeros() as usize,
                    at: at(&o.bbox, &o.bbox),
                    items: vec![describe(m, o)],
                });
            }
        };
        if let (Kind::Via, Shape::Capsule { r: vr, .. }) = (&o.kind, &o.shape) {
            size(&mut out, "via_diameter", md, 2.0 * vr);
            size(&mut out, "annular_width", ma, vr - o.hole_r);
        }
        if hole_d > 0.0 {
            size(&mut out, "drill_out_of_range", mh, hole_d);
        }
        if let (Kind::Track, Shape::Capsule { r: hw, .. }) = (&o.kind, &o.shape) {
            let (min, _) = r.track_width(o.net);
            let min = min.max(r.min_track);
            if 2.0 * hw < min - EPS {
                out.push(Violation {
                    typ: "track_width",
                    need: min,
                    actual: 2.0 * hw,
                    layer: o.layers.trailing_zeros() as usize,
                    at: at(&o.bbox, &o.bbox),
                    items: vec![describe(m, o)],
                });
            }
        }
    }
    out
}

/// KiCad-style JSON report (`kicad-cli pcb drc --format json` layout, positions in mm)
pub fn report(v: &[Violation], unconnected: &[(String, Vec<String>)]) -> serde_json::Value {
    use serde_json::json;
    let item = |d: &str, at: P| json!({"description": d, "pos": {"x": at.x, "y": at.y}});
    json!({
        "source": "copperpath drc",
        "units": "mm",
        "violations": v.iter().map(|x| json!({
            "type": x.typ, "severity": "error",
            "description": format!("{} (rule {:.4} mm; actual {:.4} mm) on {}", x.typ, x.need, x.actual, CU[x.layer]),
            "items": x.items.iter().map(|d| item(d, x.at)).collect::<Vec<_>>(),
        })).collect::<Vec<_>>(),
        "unconnected_items": unconnected.iter().map(|(net, isl)| json!({
            "type": "unconnected_items", "severity": "error",
            "description": format!("Missing connection in net {net}"),
            "items": isl.iter().map(|d| json!({"description": d})).collect::<Vec<_>>(),
        })).collect::<Vec<_>>(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom::dist_seg;

    #[test]
    fn crossing_segments() {
        let p = |x, y| P { x, y };
        assert!(seg_cross(p(0., 0.), p(2., 2.), p(0., 2.), p(2., 0.)));
        assert!(!seg_cross(p(0., 0.), p(1., 0.), p(0., 1.), p(1., 1.)));
        assert!(dist_seg(p(0., 1.), p(-1., 0.), p(1., 0.)) == 1.0);
    }
}
