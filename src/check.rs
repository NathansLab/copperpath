//! `check`: routability report = via-site maps per class, region analysis (schematic sheets), global
//! congestion (groute), hotspots, JSON + PNG per layer.

use crate::board::*;
use crate::geom::*;
use crate::img::*;
use crate::model::*;
use crate::sites::*;
use serde_json::{json, Value};
use std::collections::BTreeMap;

/// functional regions: footprints grouped by sheet name (e.g. "Channel A"), bbox of their courtyards
pub fn sheet_regions(b: &Board) -> BTreeMap<String, (BBox, Vec<usize>)> {
    let mut r: BTreeMap<String, (BBox, Vec<usize>)> = BTreeMap::new();
    for (i, f) in b.fps.iter().enumerate() {
        if f.sheetname.is_empty() {
            continue;
        }
        let e = r
            .entry(f.sheetname.clone())
            .or_insert((BBox::empty(), vec![]));
        for q in f.crtyd_world() {
            e.0.add(q);
        }
        if f.crtyd.is_empty() {
            e.0.add(f.at);
        }
        e.1.push(i);
    }
    r
}

/// free via-site analysis inside a bbox: count, area, y-bands (rows with at least one site)
pub fn region_sites(gs: &GridSpec, map: &[u8], bb: &BBox) -> Value {
    let Some((ix0, iy0, ix1, iy1)) = gs.range(bb) else {
        return json!({});
    };
    let mut free = 0usize;
    let mut rows: Vec<(f64, usize)> = Vec::new();
    let mut blockers: BTreeMap<String, usize> = BTreeMap::new();
    for iy in iy0..=iy1 {
        let mut c = 0;
        for ix in ix0..=ix1 {
            let v = map[gs.idx(ix, iy)];
            if v == R_FREE {
                c += 1;
            } else {
                let l = (v >> 4) as usize;
                let lname = if (v & 15) == K_EDGE || (v & 15) == K_HOLE {
                    ""
                } else {
                    CU[l.min(5)]
                };
                *blockers
                    .entry(format!("{}@{}", kind_name(v), lname))
                    .or_default() += 1;
            }
        }
        free += c;
        rows.push((gs.center(0, iy).y, c));
    }
    // contiguous y bands with sites
    let mut bands = Vec::new();
    let mut cur: Option<(f64, f64, usize)> = None;
    for (y, c) in &rows {
        if *c > 0 {
            cur = Some(match cur {
                Some((a, _, n)) => (a, *y, n + c),
                None => (*y, *y, *c),
            });
        } else if let Some(b) = cur.take() {
            bands.push(b);
        }
    }
    if let Some(b) = cur {
        bands.push(b);
    }
    let mut top: Vec<(String, usize)> = blockers.into_iter().collect();
    top.sort_by_key(|e| std::cmp::Reverse(e.1));
    top.truncate(8);
    let half = gs.res / 2.0;
    json!({
        "free_sites": free,
        "free_area_mm2": free as f64 * gs.res * gs.res,
        "cells": (ix1 - ix0 + 1) * (iy1 - iy0 + 1),
        "y_bands": bands.iter().map(|(a, b, n)| json!({"y0": round2(a - half), "y1": round2(b + half), "sites": n})).collect::<Vec<_>>(),
        "top_blockers": top.iter().map(|(k, n)| json!({"what": k, "cells": n})).collect::<Vec<_>>(),
    })
}

pub fn round2(x: f64) -> f64 {
    (x * 100.0).round() / 100.0
}

/// stable colour per net class name (hash -> hue); the Default class is drawn in a neutral blue
pub fn class_color(name: &str) -> [u8; 3] {
    if name == "Default" {
        return [110, 140, 200];
    }
    let h = name
        .bytes()
        .fold(2166136261u32, |h, c| (h ^ c as u32).wrapping_mul(16777619));
    let hue = (h % 360) as f64;
    let (s, v) = (0.75, 0.85);
    let c = v * s;
    let x = c * (1.0 - ((hue / 60.0) % 2.0 - 1.0).abs());
    let (r, g, b) = match (hue / 60.0) as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = v - c;
    [
        ((r + m) * 255.0) as u8,
        ((g + m) * 255.0) as u8,
        ((b + m) * 255.0) as u8,
    ]
}

/// layer image: obstacles, optional site map (green), optional heat overlay
pub fn layer_image(
    m: &Model,
    gs: GridSpec,
    l: Option<usize>,
    sites: Option<&[u8]>,
    heatmap: Option<&[f64]>,
) -> Canvas {
    let mut cv = Canvas::new(gs, [18, 18, 22]);
    if let Some(outl) = m.b.edge.first() {
        let mut cells = Vec::new();
        gs.fill_poly(outl, &mut |x, y| cells.push((x, y)));
        for (x, y) in cells {
            cv.set(x, y, [34, 36, 40]);
        }
    }
    for o in &m.objs {
        let on = match l {
            Some(l) => o.layers & (1 << l) != 0,
            None => true,
        };
        if !on {
            continue;
        }
        let col = match o.kind {
            Kind::Keepout { .. } => [120, 0, 120],
            Kind::ViaKeepout => [90, 90, 90],
            _ => class_color(&m.r.class_of(o.net).name),
        };
        let a = match o.kind {
            Kind::Zone => 0.35,
            Kind::ViaKeepout => 0.25,
            _ => 0.85,
        };
        if matches!(o.kind, Kind::ViaKeepout) {
            if let Shape::Poly { pts, .. } = &o.shape {
                cv.outline(pts, [160, 160, 160]);
            }
            continue;
        }
        cv.shape(&o.shape, col, a);
    }
    if let Some(s) = sites {
        for (i, &v) in s.iter().enumerate().take(gs.n()) {
            if v == R_FREE {
                let (x, y) = (i % gs.nx, i / gs.nx);
                cv.blend(x, y, [60, 255, 90], 0.55);
            }
        }
    }
    if let Some(h) = heatmap {
        for (i, &v) in h.iter().enumerate().take(gs.n()) {
            if v > 0.0 {
                let (x, y) = (i % gs.nx, i / gs.nx);
                cv.blend(x, y, heat(v), 0.6);
            }
        }
    }
    // footprint courtyards
    for f in &m.b.fps {
        let c = f.crtyd_world();
        if c.len() >= 3 {
            let on_side = match l {
                Some(0) => !f.back,
                Some(5) => f.back,
                _ => true,
            };
            if on_side {
                cv.outline(&c, [200, 200, 200]);
            }
        }
    }
    cv
}
