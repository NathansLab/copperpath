//! Why is a connection open? For two pads of a net, a raster of the legal track-centre area
//! (DRU/net-class clearance to every foreign object, rule areas, mask apertures, board edge)
//! over a window around them, all allowed layers, through vias where a via fits. Checks in order:
//! a corridor at the net's minimum width (then the failure is congestion or the search budget),
//! the same without unlocked tracks/vias of other nets (then routed copper blocks it: rip-up
//! candidates), else the widest track that still gets through (bisection) and the bottleneck
//! where the required width does not fit, with the nearest blockers there.

#![allow(clippy::needless_range_loop)]

use crate::board::*;
use crate::geom::*;
use crate::model::{self, Blocker, Kind, Model, Pin};
use crate::render;
use serde::Serialize;
use std::collections::VecDeque;

#[derive(Debug, Clone, Serialize)]
pub struct Explain {
    pub net: String,
    pub from: String,
    pub to: String,
    /// "congestion", "routed-copper", "too-narrow", "enclosed"
    pub kind: &'static str,
    /// one line for humans and agents
    pub reason: String,
    /// widest track that gets through the fixed obstacles (mm), None if none
    pub corridor_mm: Option<f64>,
    /// minimum track width of the net (mm)
    pub need_mm: f64,
    /// bottleneck (or the frontier point closest to the target)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub at: Option<[f64; 2]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub layer: Option<String>,
    pub blockers: Vec<Blocker>,
    /// nets of unlocked copper that blocks the corridor (rip-up candidates)
    pub rip_nets: Vec<String>,
    /// region for an image
    pub roi: [f64; 4],
    #[serde(skip_serializing_if = "Option::is_none")]
    pub img: Option<String>,
}

struct Win<'a> {
    m: &'a Model<'a>,
    net: usize,
    gs: GridSpec,
    layers: Vec<usize>,
    inside: Vec<bool>,
}

impl Win<'_> {
    /// legal centre cells per layer for half width hw (index into `layers`), and via cells
    fn free(&self, hw: f64, via_r: f64, unlocked: bool) -> (Vec<Vec<bool>>, Vec<bool>) {
        let (m, gs, net) = (self.m, &self.gs, self.net);
        let gb = gs.bbox().inflate(3.0);
        let n = gs.n();
        let mut fr: Vec<Vec<bool>> = self.layers.iter().map(|_| self.inside.clone()).collect();
        let mut via = self.inside.clone();
        let ec = m.r.edge_clearance(net);
        for (a, e) in &m.b.edge_segs {
            let s = Shape::Capsule {
                a: *a,
                b: *e,
                r: 0.0,
            };
            for f in fr.iter_mut() {
                gs.stamp(&s, ec + hw, |x, y| f[gs.idx(x, y)] = false);
            }
            gs.stamp(&s, ec + via_r, |x, y| via[gs.idx(x, y)] = false);
        }
        for o in &m.objs {
            if !o.bbox.overlaps(&gb) {
                continue;
            }
            if !unlocked && !o.locked && matches!(o.kind, Kind::Track | Kind::Via) {
                continue;
            }
            for (k, &l) in self.layers.iter().enumerate() {
                if o.layers & (1 << l) == 0 {
                    continue;
                }
                let c = m.clr(net, o, l);
                if c < 0.0 {
                    continue;
                }
                let (tr, vi) = match o.kind {
                    Kind::Keepout { tracks, vias } => (tracks, vias),
                    Kind::ViaKeepout => (false, true),
                    Kind::Zone if !o.locked => (false, false),
                    _ => (true, true),
                };
                if tr {
                    let f = &mut fr[k];
                    gs.stamp(&o.shape, c + hw, |x, y| f[gs.idx(x, y)] = false);
                }
                if vi {
                    let hc = if o.hole_r > 0.0 {
                        m.r.min_hole_to_hole
                    } else {
                        0.0
                    };
                    gs.stamp(&o.shape, (c + via_r).max(hc + via_r), |x, y| {
                        via[gs.idx(x, y)] = false
                    });
                }
            }
        }
        // a via needs room on every layer it passes
        let _ = n;
        (fr, via)
    }

    /// BFS from the cells of pad a to the cells of pad b; returns the path (layer index, cell)
    /// or the reached set
    fn connect(
        &self,
        fr: &[Vec<bool>],
        via: &[bool],
        src: &[(usize, usize)],
        dst: &[(usize, usize)],
    ) -> Result<Vec<(usize, usize)>, Vec<Vec<bool>>> {
        let (nx, ny) = (self.gs.nx, self.gs.ny);
        let n = self.gs.n();
        let nl = self.layers.len();
        let mut par = vec![u32::MAX; nl * n];
        let mut goal = vec![false; nl * n];
        for &(k, i) in dst {
            goal[k * n + i] = true;
        }
        let mut q = VecDeque::new();
        for &(k, i) in src {
            if par[k * n + i] == u32::MAX {
                par[k * n + i] = (k * n + i) as u32;
                q.push_back(k * n + i);
            }
        }
        while let Some(s) = q.pop_front() {
            if goal[s] {
                let mut path = vec![(s / n, s % n)];
                let mut c = s;
                while par[c] as usize != c {
                    c = par[c] as usize;
                    path.push((c / n, c % n));
                }
                path.reverse();
                return Ok(path);
            }
            let (k, i) = (s / n, s % n);
            let (x, y) = ((i % nx) as i64, (i / nx) as i64);
            for dy in -1i64..=1 {
                for dx in -1i64..=1 {
                    if dx == 0 && dy == 0 {
                        continue;
                    }
                    let (xx, yy) = (x + dx, y + dy);
                    if xx < 0 || yy < 0 || xx >= nx as i64 || yy >= ny as i64 {
                        continue;
                    }
                    let j = yy as usize * nx + xx as usize;
                    let t = k * n + j;
                    if (fr[k][j] || goal[t]) && par[t] == u32::MAX {
                        par[t] = s as u32;
                        q.push_back(t);
                    }
                }
            }
            if via[i] {
                for k2 in 0..nl {
                    let t = k2 * n + i;
                    if k2 != k && par[t] == u32::MAX && (fr[k2][i] || goal[t]) {
                        par[t] = s as u32;
                        q.push_back(t);
                    }
                }
            }
        }
        Err((0..nl)
            .map(|k| (0..n).map(|i| par[k * n + i] != u32::MAX).collect())
            .collect())
    }

    fn pad_cells(&self, p: &Pin) -> Vec<(usize, usize)> {
        let f = &self.m.b.fps[p.fp];
        let pd = &f.pads[p.pad];
        let mut v = Vec::new();
        for (k, &l) in self.layers.iter().enumerate() {
            if pd.layers & (1 << l) == 0 {
                continue;
            }
            for s in f.pad_shapes(pd) {
                self.gs
                    .stamp(&s, 0.0, |x, y| v.push((k, self.gs.idx(x, y))));
            }
        }
        if v.is_empty() {
            if let Some((x, y)) = self.gs.cell_of(p.at) {
                for k in 0..self.layers.len() {
                    v.push((k, self.gs.idx(x, y)));
                }
            }
        }
        v
    }
}

pub fn describe(b: &Blocker) -> String {
    let what = match (&b.fp, b.kind) {
        (Some(fp), _) => format!("pad {fp} ({})", b.net),
        (None, "keepout") => "rule area".to_string(),
        (None, k) => format!("{k} {}", b.net),
    };
    format!(
        "{}{what} on {} ({:.2} mm away, needs {:.2})",
        if b.locked && b.kind != "keepout" && b.kind != "pad" {
            "locked "
        } else {
            ""
        },
        b.layer,
        b.dist_mm,
        b.need_mm
    )
}

fn pin_name(m: &Model, p: &Pin) -> String {
    let f = &m.b.fps[p.fp];
    format!("{}.{}", f.refd, f.pads[p.pad].num)
}

/// explain why pins a and b of `net` are not connected
pub fn explain(m: &Model, net: usize, a: &Pin, b: &Pin, margin: f64) -> Explain {
    let mut wb = BBox::empty();
    wb.add(a.at);
    wb.add(b.at);
    let mut wb = wb.inflate(margin);
    if m.b.bbox.valid() {
        let bb = m.b.bbox.inflate(0.5);
        wb = BBox {
            x0: wb.x0.max(bb.x0),
            y0: wb.y0.max(bb.y0),
            x1: wb.x1.min(bb.x1),
            y1: wb.y1.min(bb.y1),
        };
    }
    let area = (wb.x1 - wb.x0) * (wb.y1 - wb.y0);
    let res = if area / (0.05 * 0.05) < 1.5e6 {
        0.05
    } else {
        (area / 1.5e6).sqrt().max(0.05)
    };
    let gs = GridSpec::new(wb, res);
    let mut inside = vec![false; gs.n()];
    for i in 0..gs.n() {
        inside[i] = m.b.inside(gs.center(i % gs.nx, i / gs.nx));
    }
    let lm = m.net_layers[net] & m.b.cu_mask;
    let layers: Vec<usize> = (0..NL).filter(|l| lm & (1 << l) != 0).collect();
    let w = Win {
        m,
        net,
        gs,
        layers,
        inside,
    };
    let need = m.r.track_width(net).0;
    let hw = need / 2.0;
    let via_r = m.r.via(net).0 / 2.0;
    let (sa, sb) = (w.pad_cells(a), w.pad_cells(b));
    // image region: the window, at least half as high as wide (and vice versa)
    let (cw, ch) = (wb.x1 - wb.x0, wb.y1 - wb.y0);
    let (gx, gy) = (
        ((ch * 0.5 - cw) / 2.0).max(0.0),
        ((cw * 0.5 - ch) / 2.0).max(0.0),
    );
    let roi =
        [wb.x0 - gx, wb.y0 - gy, wb.x1 + gx, wb.y1 + gy].map(|v| (v * 1000.0).round() / 1000.0);
    let mut ex = Explain {
        net: m.b.nets[net].clone(),
        from: pin_name(m, a),
        to: pin_name(m, b),
        kind: "congestion",
        reason: String::new(),
        corridor_mm: None,
        need_mm: need,
        at: None,
        layer: None,
        blockers: Vec::new(),
        rip_nets: Vec::new(),
        roi,
        img: None,
    };
    let r4 = |v: f64| (v * 1e4).round() / 1e4;
    let (fr, via) = w.free(hw, via_r, true);
    if w.connect(&fr, &via, &sa, &sb).is_ok() {
        ex.corridor_mm = Some(need);
        ex.reason = format!(
            "a legal corridor for {need:.2} mm exists: congestion or search budget (try --iters, --max-expand, --reroute-blockers, or a region route)"
        );
        return ex;
    }
    let (frl, vial) = w.free(hw, via_r, false);
    if let Ok(path) = w.connect(&frl, &vial, &sa, &sb) {
        // unlocked copper on the corridor found without it
        ex.kind = "routed-copper";
        ex.corridor_mm = Some(need);
        let mut nets = Vec::new();
        for &(k, i) in &path {
            if !fr[k][i] && frl[k][i] {
                let q = w.gs.center(i % w.gs.nx, i / w.gs.nx);
                let l = w.layers[k];
                for bl in model::nearest_blockers(m, net, &Shape::circle(q, hw), 1 << l, 0.5, 3) {
                    if !bl.locked && matches!(bl.kind, "track" | "via") && !nets.contains(&bl.net) {
                        if ex.at.is_none() {
                            ex.at = Some([r4(q.x), r4(q.y)]);
                            ex.layer = Some(CU[l].to_string());
                        }
                        nets.push(bl.net.clone());
                        ex.blockers.push(bl);
                    }
                }
            }
        }
        ex.rip_nets = nets;
        ex.reason = format!(
            "blocked by routed copper of {}: a {need:.2} mm corridor exists without it (rip up and reroute)",
            if ex.rip_nets.is_empty() {
                "other nets".to_string()
            } else {
                ex.rip_nets.join(", ")
            }
        );
        return ex;
    }
    // fixed obstacles: widest half width that connects
    let (mut lo, mut hi) = (-1.0f64, hw);
    let mut best: Option<Vec<(usize, usize)>> = None;
    {
        let (f0, v0) = w.free(0.0, via_r, false);
        match w.connect(&f0, &v0, &sa, &sb) {
            Ok(p) => {
                lo = 0.0;
                best = Some(p);
            }
            Err(reached) => {
                // enclosed: frontier point of the reached area closest to the target
                ex.kind = "enclosed";
                let mut bestd = f64::INFINITY;
                for (k, rs) in reached.iter().enumerate() {
                    for (i, &r) in rs.iter().enumerate() {
                        if r {
                            let q = w.gs.center(i % w.gs.nx, i / w.gs.nx);
                            let d = (q - b.at).len();
                            if d < bestd {
                                bestd = d;
                                ex.at = Some([r4(q.x), r4(q.y)]);
                                ex.layer = Some(CU[w.layers[k]].to_string());
                            }
                        }
                    }
                }
            }
        }
    }
    if lo >= 0.0 {
        for _ in 0..6 {
            let mid = (lo + hi) / 2.0;
            let (f, v) = w.free(mid, via_r, false);
            match w.connect(&f, &v, &sa, &sb) {
                Ok(p) => {
                    lo = mid;
                    best = Some(p);
                }
                Err(_) => hi = mid,
            }
        }
        ex.kind = "too-narrow";
        ex.corridor_mm = Some(r4(2.0 * lo));
        // bottleneck: first cell of the widest path where the needed width does not fit
        if let Some(path) = &best {
            for &(k, i) in path {
                if !frl[k][i] {
                    let q = w.gs.center(i % w.gs.nx, i / w.gs.nx);
                    ex.at = Some([r4(q.x), r4(q.y)]);
                    ex.layer = Some(CU[w.layers[k]].to_string());
                    break;
                }
            }
        }
    }
    if let (Some(at), Some(l)) = (ex.at, &ex.layer) {
        let l = layer_idx(l).unwrap_or(0);
        ex.blockers =
            model::nearest_blockers(m, net, &Shape::circle(p(at[0], at[1]), hw), 1 << l, 1.5, 3);
    }
    let bl = ex
        .blockers
        .iter()
        .map(describe)
        .collect::<Vec<_>>()
        .join("; ");
    let at = ex
        .at
        .map(|q| {
            format!(
                " at ({:.2}, {:.2}) {}",
                q[0],
                q[1],
                ex.layer.as_deref().unwrap_or("")
            )
        })
        .unwrap_or_default();
    ex.reason = match ex.corridor_mm {
        Some(c) => format!(
            "widest free corridor fits a {c:.2} mm track < {need:.2} mm needed{at}: blocked by {bl}"
        ),
        None => format!(
            "no legal path from {} to {}{at}: enclosed by {bl}",
            ex.from, ex.to
        ),
    };
    ex
}

/// open connections of the nets selected by `sel` (minimum spanning tree over the copper
/// islands), optionally only those touching pad `pad` ("REF.PIN"), explained; at most `max`
pub fn explain_opens(
    m: &Model,
    sel: &(dyn Fn(usize) -> bool + Sync),
    pad: Option<&str>,
    max: usize,
) -> Vec<Explain> {
    let (pins, _) = model::pins_and_copper_of(m, sel);
    let rats = render::ratsnest(m, sel);
    let mut out = Vec::new();
    for r in rats {
        if out.len() >= max {
            break;
        }
        if pad.is_some_and(|p| r.pads[0] != p && r.pads[1] != p) {
            continue;
        }
        let Some(&net) = m.b.net_id.get(&r.net) else {
            continue;
        };
        let Some(ps) = pins.get(&net) else { continue };
        let find = |name: &str| ps.iter().find(|q| pin_name(m, q) == name).cloned();
        if let (Some(a), Some(b)) = (find(&r.pads[0]), find(&r.pads[1])) {
            out.push(explain(m, net, &a, &b, 3.0));
        }
    }
    out
}

/// a small ROI image of an explanation: the net highlighted, ratsnest, labels, rule areas,
/// the bottleneck (red) and the blockers (yellow)
pub fn image(m: &Model, ex: &Explain, path: &str, px: usize) -> Result<(), String> {
    let mut drc = Vec::new();
    if let Some(at) = ex.at {
        drc.push(render::Marker {
            at: p(at[0], at[1]),
            to: None,
            kind: ex.kind.to_string(),
            error: true,
        });
    }
    for bl in &ex.blockers {
        drc.push(render::Marker {
            at: p(bl.at[0], bl.at[1]),
            to: None,
            kind: bl.kind.to_string(),
            error: false,
        });
    }
    let roi = BBox {
        x0: ex.roi[0],
        y0: ex.roi[1],
        x1: ex.roi[2],
        y1: ex.roi[3],
    };
    let title = format!("{} {} -> {}: {}", ex.net, ex.from, ex.to, ex.kind);
    image_roi(
        m,
        m.b.net_id.get(&ex.net).copied(),
        roi,
        drc,
        &title,
        path,
        px,
    )
}

/// markers (yellow) at the blockers, for an image
pub fn blocker_markers(bl: &[Blocker]) -> Vec<render::Marker> {
    bl.iter()
        .map(|b| render::Marker {
            at: p(b.at[0], b.at[1]),
            to: None,
            kind: b.kind.to_string(),
            error: false,
        })
        .collect()
}

/// ROI image with one net highlighted (ratsnest of that net, labels, fills, rule areas, halos)
/// and markers
pub fn image_roi(
    m: &Model,
    net: Option<usize>,
    roi: BBox,
    drc: Vec<render::Marker>,
    title: &str,
    path: &str,
    px: usize,
) -> Result<(), String> {
    let o = render::Opts {
        roi: Some(roi),
        layers: 0x3f,
        nets: None,
        hl_net: net,
        fills: true,
        labels: true,
        courtyards: false,
        opens: true,
        halo_min: 1.0,
        drc,
        px,
        per_layer: false,
        title: title.to_string(),
    };
    let r = render::render(m, &o)?;
    if let Some(d) = std::path::Path::new(path).parent() {
        let _ = std::fs::create_dir_all(d);
    }
    std::fs::write(path, &r.png).map_err(|e| format!("{path}: {e}"))
}

/// file name for an explanation image
pub fn img_name(dir: &str, k: usize, ex: &Explain) -> String {
    let clean = |s: &str| s.replace(|c: char| !c.is_ascii_alphanumeric() && c != '.', "_");
    format!(
        "{}/{:02}_{}_{}_{}.png",
        dir.trim_end_matches(['/', '\\']),
        k + 1,
        clean(&ex.net),
        clean(&ex.from),
        clean(&ex.to)
    )
}
