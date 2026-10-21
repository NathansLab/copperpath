//! Stitching vias for plane nets: one via in every filled pour island that has no via or plated
//! hole of its net yet, optionally a regular grid of vias per island. Reads the saved zone fills
//! (`filled_polygon`); a via is legal when it lies on a plane fill of the same net (so it really
//! connects), keeps the DRU/net-class clearance to every other net on every layer (outer layers
//! incl. creepage, last matching rule wins), 0.2 mm to every pad, the hole rules, the board edge
//! and stays out of no-via rule areas. Unlocked zones of other nets are ignored (they refill
//! around the via); locked zones are copper.

use crate::clean::Checker;
use crate::geom::*;
use crate::model::*;
use regex::Regex;
use std::collections::BTreeMap;

pub struct StitchParams {
    pub net: usize,
    /// zones whose fill islands get vias (zone name, full match)
    pub zones: Regex,
    /// zones of the same net a via must land on (inner planes, zone name, full match)
    pub plane: Regex,
    /// grid pitch (mm) of extra vias per island; 0 = only one via per island without one
    pub grid: f64,
    pub via_d: f64,
    pub drill: f64,
    /// extra clearance (mm) on top of every rule
    pub margin: f64,
    /// floor (mm) on the clearance to other nets (0 = rules only)
    pub min_clr: f64,
    /// islands below this area (mm^2) are skipped
    pub min_area: f64,
    /// candidate raster (mm)
    pub step: f64,
}

#[derive(Debug, Default, serde::Serialize)]
pub struct StitchResult {
    pub islands: usize,
    /// islands without a via or plated hole of the net
    pub lone: usize,
    /// lone islands that got a via
    pub fixed: usize,
    pub vias: Vec<P>,
    /// lone islands without a legal spot: (layer, point inside, area mm^2)
    pub failed: Vec<(String, P, f64)>,
    /// rejected candidates by cause
    pub why: BTreeMap<String, usize>,
}

struct Island<'a> {
    layer: usize,
    poly: &'a PolyIndex,
    area: f64,
}

pub fn stitch(m: &Model, p: &StitchParams) -> StitchResult {
    let net = p.net;
    let vr = p.via_d / 2.0;
    let dr = p.drill / 2.0;
    let mut res = StitchResult::default();
    // fills of the island zones and of the planes (same net); a fill object does not know its
    // zone, so walk the zones in the order Model::new built `fills`
    let mut islands: Vec<Island> = Vec::new();
    let mut planes: Vec<&PolyIndex> = Vec::new();
    let mut plane_outlines: Vec<PolyIndex> = Vec::new();
    let mut fi = 0;
    for z in m.b.zones.iter().filter(|z| !z.keepout && z.net != 0) {
        let n = z.fills.len();
        if z.net == net {
            if p.plane.is_match(&z.name) {
                if n == 0 && z.outline.len() >= 3 {
                    plane_outlines.push(PolyIndex::new(z.outline.clone()));
                }
                planes.extend((fi..fi + n).map(|k| &m.fill_idx[k]));
            }
            if p.zones.is_match(&z.name) {
                for k in fi..fi + n {
                    let poly = &m.fill_idx[k];
                    let area = poly.area();
                    if area >= p.min_area {
                        islands.push(Island {
                            layer: z.fills[k - fi].0,
                            poly,
                            area,
                        });
                    }
                }
            }
        }
        fi += n;
    }
    islands.sort_by(|a, b| b.area.total_cmp(&a.area));
    res.islands = islands.len();
    // existing vias and plated holes of the net
    let mut own: Vec<P> = m
        .objs
        .iter()
        .filter(|o| {
            o.net == net && o.hole_r > 0.0 && matches!(o.kind, Kind::Via | Kind::Pad { .. })
        })
        .map(|o| o.bbox.center())
        .collect();
    let maxd = m.r.mat[0]
        .iter()
        .chain(&m.r.mat[1])
        .fold(0.0f64, |a, v| a.max(*v as f64));
    let mut ck = Checker::new(m, &m.objs, maxd);
    // locked zones of other nets are copper here (also of the own class)
    ck.yield_same_class = false;
    ck.min_clr = p.min_clr;
    let gap = p.via_d + m.r.min_hole_to_hole.max(0.25);
    let mut whys: BTreeMap<String, usize> = BTreeMap::new();
    let on_plane = |q: P| -> bool {
        planes
            .iter()
            .any(|pl| pl.contains(q) && pl.boundary_dist(q, vr) >= vr)
            || plane_outlines
                .iter()
                .any(|pl| pl.contains(q) && pl.boundary_dist(q, 0.5 + vr) >= 0.5 + vr)
    };
    let mut new: Vec<P> = Vec::new();
    let legal = |q: P, new: &[P], whys: &mut BTreeMap<String, usize>| -> bool {
        let k = if !on_plane(q) {
            "plane"
        } else if new.iter().any(|n| (*n - q).len() < gap) {
            "new_via"
        } else {
            match ck.via_block_fixed(net, q, vr + p.margin, dr) {
                Some(k) => k,
                None => return true,
            }
        };
        *whys.entry(k.to_string()).or_insert(0) += 1;
        false
    };
    let core = |isl: &Island, q: P| isl.poly.contains(q) && isl.poly.boundary_dist(q, 0.05) >= 0.05;
    for isl in &islands {
        let bb = isl.poly.bbox;
        let has = own.iter().chain(new.iter()).any(|c| {
            bb.inflate(vr).contains(*c) && isl.poly.dist(&Shape::circle(*c, 0.0), vr) <= vr
        });
        if !has {
            res.lone += 1;
            // raster inside the island, closest to the island's centroid first
            let ctr = centroid(&isl.poly.pts);
            let (nx, ny) = (
                ((bb.x1 - bb.x0) / p.step) as usize + 1,
                ((bb.y1 - bb.y0) / p.step) as usize + 1,
            );
            let mut cand: Vec<P> = (0..nx * ny)
                .map(|k| P {
                    x: bb.x0 + (k % nx) as f64 * p.step,
                    y: bb.y0 + (k / nx) as f64 * p.step,
                })
                .filter(|q| core(isl, *q))
                .collect();
            cand.sort_by(|a, b| (*a - ctr).len().total_cmp(&(*b - ctr).len()));
            match cand.into_iter().find(|q| legal(*q, &new, &mut whys)) {
                Some(q) => {
                    new.push(q);
                    own.push(q);
                    res.fixed += 1;
                }
                None => res.failed.push((
                    crate::board::CU[isl.layer].to_string(),
                    inner_point(isl, ctr),
                    (isl.area * 100.0).round() / 100.0,
                )),
            }
        }
        if p.grid > 0.0 {
            let g = p.grid;
            let mut offs: Vec<P> = (-4i32..=4)
                .flat_map(|a| {
                    (-4i32..=4).map(move |c| P {
                        x: a as f64 * 0.25,
                        y: c as f64 * 0.25,
                    })
                })
                .collect();
            offs.sort_by(|a, b| a.len().total_cmp(&b.len()));
            for i in (bb.x0 / g).floor() as i64..=(bb.x1 / g).floor() as i64 {
                for j in (bb.y0 / g).floor() as i64..=(bb.y1 / g).floor() as i64 {
                    let gp = P {
                        x: i as f64 * g,
                        y: j as f64 * g,
                    };
                    if !core(isl, gp) || own.iter().any(|o| (*o - gp).len() < g / 2.0) {
                        continue;
                    }
                    // nearest legal spot within 1 mm of the grid point
                    if let Some(q) = offs
                        .iter()
                        .map(|d| gp + *d)
                        .find(|q| core(isl, *q) && legal(*q, &new, &mut whys))
                    {
                        new.push(q);
                        own.push(q);
                    }
                }
            }
        }
    }
    res.vias = new;
    res.why = whys;
    res
}

fn centroid(pts: &[P]) -> P {
    let n = pts.len().max(1) as f64;
    pts.iter().fold(P { x: 0.0, y: 0.0 }, |s, q| s + *q) * (1.0 / n)
}

/// a point inside the island near `c` (for the report)
fn inner_point(isl: &Island, c: P) -> P {
    if isl.poly.contains(c) {
        return c;
    }
    let bb = isl.poly.bbox;
    let mut best = bb.center();
    let mut bd = f64::INFINITY;
    for k in 0..400 {
        let q = P {
            x: bb.x0 + (k % 20) as f64 / 19.0 * (bb.x1 - bb.x0),
            y: bb.y0 + (k / 20) as f64 / 19.0 * (bb.y1 - bb.y0),
        };
        if isl.poly.contains(q) && (q - c).len() < bd {
            bd = (q - c).len();
            best = q;
        }
    }
    best
}
