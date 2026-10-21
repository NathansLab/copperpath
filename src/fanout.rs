//! Fan-out of open plane-net islands: for every copper island of the net that does not touch a
//! plane fill, a short track from one of its SMD pads to the nearest legal via spot within a
//! reach, the via landing on a plane fill of the same net. Grid search (0.05 mm, 8-connected)
//! on the pad's outer layer with the router's legality maps (DRU/net-class clearance incl.
//! outer-layer creepage, rule areas, mask apertures, edge); vias as in `stitch`. Inside the pad's
//! own courtyard a DRU neck-down rule (`track_width` with `intersectsCourtyard`) sets the width.

use crate::board::CU;
use crate::clean::Checker;
use crate::droute::{RNet, RSeg, RVia};
use crate::geom::*;
use crate::model::*;
use crate::sites::{track_map, R_FREE};
use regex::Regex;
use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashSet};

pub struct FanoutParams {
    pub net: usize,
    /// plane zones of the net the vias land on (zone name, full match)
    pub plane: Regex,
    /// longest track (mm)
    pub reach: f64,
    pub via_d: f64,
    pub drill: f64,
    /// track width outside the courtyard (None: net class / DRU width)
    pub width: Option<f64>,
    /// extra clearance (mm) on top of every rule
    pub margin: f64,
    /// floor (mm) on the via clearance to other nets
    pub min_clr: f64,
    /// only islands containing one of these pads ("REF.NUM"); empty = all open islands
    pub only: HashSet<String>,
    pub res: f64,
}

#[derive(Debug, Default, serde::Serialize)]
pub struct FanoutIsland {
    /// pads of the island
    pub pads: Vec<String>,
    /// pad that got the fan-out (None: failed)
    pub pad: Option<String>,
    pub layer: Option<String>,
    pub length_mm: f64,
    pub via: Option<[f64; 2]>,
    /// why no pad worked (per pad, first few)
    pub why: Vec<String>,
    /// failed islands: the other-net copper closest to violating its clearance around the
    /// island's pads (what to move)
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub blockers: Vec<Blocker>,
}

#[derive(Debug, Default)]
pub struct FanoutResult {
    pub islands: Vec<FanoutIsland>,
    pub copper: RNet,
}

pub fn fanout(m: &Model, p: &FanoutParams) -> Result<FanoutResult, String> {
    let net = p.net;
    let (pins, copper) = pins_and_copper_of(m, &|n| n == net);
    let pins = pins.get(&net).cloned().unwrap_or_default();
    // plane fills of the net and the components that touch one
    let no = m.objs.len();
    let plane_fill: Vec<usize> = (0..m.fills.len())
        .filter(|&k| m.fills[k].net == net && p.plane.is_match(&m.b.zones[m.fill_zone[k]].name))
        .collect();
    if plane_fill.is_empty() {
        return Err(format!(
            "no saved fill in a --plane zone of net {}: refill a copy first (kicad-cli pcb drc --refill-zones --save-board)",
            m.b.nets[net]
        ));
    }
    let on_plane_comp: HashSet<usize> = copper
        .get(&net)
        .map(|v| {
            v.iter()
                .filter(|(i, _)| *i >= no && plane_fill.contains(&(*i - no)))
                .map(|(_, c)| *c)
                .collect()
        })
        .unwrap_or_default();
    let mut comps: Vec<usize> = pins.iter().map(|pn| pn.comp).collect();
    comps.sort();
    comps.dedup();
    let name = |pn: &Pin| {
        let f = &m.b.fps[pn.fp];
        format!("{}.{}", f.refd, f.pads[pn.pad].num)
    };
    let maxd = m.r.mat[0]
        .iter()
        .chain(&m.r.mat[1])
        .fold(0.0f64, |a, v| a.max(*v as f64));
    let mut ck = Checker::new(m, &m.objs, maxd);
    ck.yield_same_class = false;
    ck.min_clr = p.min_clr;
    let (vr, dr) = (p.via_d / 2.0, p.drill / 2.0);
    let gap = p.via_d + m.r.min_hole_to_hole.max(0.25);
    let on_plane = |q: P| {
        plane_fill.iter().any(|&k| {
            let pl = &m.fill_idx[k];
            pl.contains(q) && pl.boundary_dist(q, vr) >= vr
        })
    };
    let w_net = p.width.unwrap_or(m.r.track_width(net).1);
    let mut res = FanoutResult::default();
    let mut new_vias: Vec<P> = Vec::new();
    for &c in &comps {
        if on_plane_comp.contains(&c) {
            continue;
        }
        let isl: Vec<&Pin> = pins.iter().filter(|pn| pn.comp == c).collect();
        let mut fi = FanoutIsland {
            pads: isl.iter().map(|pn| name(pn)).collect(),
            ..Default::default()
        };
        fi.pads.sort();
        if !p.only.is_empty() && !fi.pads.iter().any(|x| p.only.contains(x)) {
            continue;
        }
        for pn in &isl {
            let f = &m.b.fps[pn.fp];
            let pd = &f.pads[pn.pad];
            let outer = pd.layers & (1 | (1 << (crate::board::NL - 1)));
            if pd.kind.contains("thru_hole") || outer == 0 {
                if fi.why.len() < 6 {
                    fi.why.push(format!("{}: not an SMD pad", name(pn)));
                }
                continue;
            }
            let l = if outer & 1 != 0 {
                0
            } else {
                crate::board::NL - 1
            };
            match pad_fanout(
                m,
                p,
                &ck,
                pn,
                l,
                w_net,
                &|q: P| on_plane(q) && !new_vias.iter().any(|n| (*n - q).len() < gap),
                vr,
                dr,
            ) {
                Ok((segs, via, len)) => {
                    fi.pad = Some(name(pn));
                    fi.layer = Some(CU[l].to_string());
                    fi.length_mm = (len * 100.0).round() / 100.0;
                    let r4 = |v: f64| (v * 1e4).round() / 1e4;
                    fi.via = Some([r4(via.x), r4(via.y)]);
                    new_vias.push(via);
                    res.copper.segs.extend(segs);
                    res.copper.vias.push(RVia {
                        at: via,
                        d: p.via_d,
                        drill: p.drill,
                    });
                    break;
                }
                Err(e) => {
                    if fi.why.len() < 6 {
                        fi.why.push(format!("{}: {e}", name(pn)));
                    }
                }
            }
        }
        if fi.pad.is_none() {
            // nearest blockers around the island's pads on their layers (3 per island)
            let mut bl: Vec<Blocker> = Vec::new();
            for pn in &isl {
                let f = &m.b.fps[pn.fp];
                let pd = &f.pads[pn.pad];
                for sh in f.pad_shapes(pd) {
                    bl.extend(nearest_blockers(m, net, &sh, pd.layers, p.reach, 3));
                }
            }
            bl.sort_by(|a, b| (a.dist_mm - a.need_mm).total_cmp(&(b.dist_mm - b.need_mm)));
            bl.dedup_by(|a, b| a.at == b.at && a.net == b.net);
            bl.truncate(3);
            fi.blockers = bl;
        }
        res.islands.push(fi);
    }
    Ok(res)
}

/// grid search from the pad to the nearest legal via spot; (segments, via, track length)
#[allow(clippy::too_many_arguments)]
fn pad_fanout(
    m: &Model,
    p: &FanoutParams,
    ck: &Checker,
    pn: &Pin,
    l: usize,
    w_net: f64,
    via_extra: &dyn Fn(P) -> bool,
    vr: f64,
    dr: f64,
) -> Result<(Vec<RSeg>, P, f64), String> {
    let net = p.net;
    let f = &m.b.fps[pn.fp];
    let pd = &f.pads[pn.pad];
    // neck-down inside the own courtyard if a DRU rule says so
    let refs = [f.refd.clone()];
    let w_in = m.r.track_width_at(net, Some(&refs)).1.min(w_net);
    let crt = f.crtyd_world();
    let span = p.reach + 1.0;
    let res = p.res;
    let n = (2.0 * span / res).ceil() as usize + 1;
    let g = GridSpec {
        x0: pn.at.x - span - res / 2.0,
        y0: pn.at.y - span - res / 2.0,
        res,
        nx: n,
        ny: n,
    };
    let wb = g.bbox().inflate(ck_reach(m) + 1.0);
    let objs: Vec<Obj> = m
        .objs
        .iter()
        .filter(|o| o.bbox.overlaps(&wb))
        .cloned()
        .collect();
    let wide = track_map(m, &g, net, w_net / 2.0 + p.margin, l, &objs);
    let narrow = if w_in < w_net - 1e-9 && crt.len() >= 3 {
        let t = track_map(m, &g, net, w_in / 2.0 + p.margin, l, &objs);
        let mut inside = vec![false; g.n()];
        g.fill_poly(&crt, &mut |x, y| inside[g.idx(x, y)] = true);
        Some((t, inside))
    } else {
        None
    };
    // 0 = blocked, 1 = narrow only, 2 = wide
    let free = |i: usize| -> u8 {
        if wide[i] == R_FREE {
            2
        } else if narrow
            .as_ref()
            .is_some_and(|(t, ins)| ins[i] && t[i] == R_FREE)
        {
            1
        } else {
            0
        }
    };
    // start: free cells inside the own pad
    let mut own = vec![false; g.n()];
    for s in f.pad_shapes(pd) {
        g.stamp(&s, 0.0, |x, y| own[g.idx(x, y)] = true);
    }
    let starts: Vec<usize> = (0..g.n()).filter(|&i| own[i] && free(i) > 0).collect();
    if starts.is_empty() {
        return Err("no legal track cell in the pad (clearance/creepage)".into());
    }
    // Dijkstra in path length (integer units of res/1000)
    let unit = |diag: bool| if diag { 1414u32 } else { 1000u32 };
    let maxc = (p.reach / res * 1000.0) as u32;
    let mut dist = vec![u32::MAX; g.n()];
    let mut prev = vec![u32::MAX; g.n()];
    let mut pq = BinaryHeap::new();
    for &s in &starts {
        dist[s] = 0;
        pq.push(Reverse((0u32, s)));
    }
    let mut target = None;
    let mut tested = 0usize;
    while let Some(Reverse((d, i))) = pq.pop() {
        if d > dist[i] {
            continue;
        }
        if d > maxc {
            break;
        }
        let (x, y) = (i % n, i / n);
        let q = g.center(x, y);
        // via spots on a 0.25 mm lattice, never in the own pad
        if !own[i] && x % 5 == 0 && y % 5 == 0 {
            tested += 1;
            if via_extra(q) && ck.via_block_fixed(net, q, vr + p.margin, dr).is_none() {
                target = Some(i);
                break;
            }
        }
        for (dx, dy) in [
            (1i64, 0i64),
            (-1, 0),
            (0, 1),
            (0, -1),
            (1, 1),
            (1, -1),
            (-1, 1),
            (-1, -1),
        ] {
            let (a, b) = (x as i64 + dx, y as i64 + dy);
            if a < 0 || b < 0 || a >= n as i64 || b >= n as i64 {
                continue;
            }
            let j = b as usize * n + a as usize;
            if free(j) == 0 {
                continue;
            }
            let nd = d + unit(dx != 0 && dy != 0);
            if nd < dist[j] {
                dist[j] = nd;
                prev[j] = i as u32;
                pq.push(Reverse((nd, j)));
            }
        }
    }
    let Some(t) = target else {
        return Err(format!(
            "no legal via spot within {} mm ({tested} spots tested)",
            p.reach
        ));
    };
    let mut cells = vec![t];
    while prev[*cells.last().unwrap()] != u32::MAX {
        let c = prev[*cells.last().unwrap()] as usize;
        cells.push(c);
    }
    cells.reverse();
    // start at the pad centre when that cell is legal for the track, else at the start cell
    let c0 = g.cell_of(pn.at).map(|(x, y)| g.idx(x, y));
    let mut pts: Vec<(P, u8)> = Vec::new();
    if let Some(c0) = c0.filter(|&c| free(c) > 0 && c != cells[0]) {
        pts.push((pn.at, free(c0)));
    }
    for &c in &cells {
        pts.push((g.center(c % n, c / n), free(c)));
    }
    if pts.len() == 1 {
        // via right at the pad edge cell: a tiny stub is still needed for the connection
        pts.insert(0, (pn.at, free(cells[0])));
    }
    // merge collinear runs of the same width class
    let mut simp: Vec<(P, u8)> = vec![pts[0]];
    for k in 1..pts.len() {
        if k + 1 < pts.len() {
            let (a, _) = *simp.last().unwrap();
            let (b, wb) = pts[k];
            let (c, wc) = pts[k + 1];
            if ((b - a).cross(c - b)).abs() < 1e-9 && wb == wc {
                continue;
            }
        }
        simp.push(pts[k]);
    }
    let mut segs = Vec::new();
    let mut len = 0.0;
    for w in simp.windows(2) {
        let ((a, wa), (b, wc)) = (w[0], w[1]);
        if (b - a).len() < 1e-9 {
            continue;
        }
        let wd = if wa.min(wc) == 2 { w_net } else { w_in };
        if !ck.seg_ok_fixed(net, a, b, wd / 2.0 + p.margin * 0.5, l) {
            return Err("exact check of the track failed".into());
        }
        len += (b - a).len();
        segs.push(RSeg { a, b, l, w: wd });
    }
    let via = g.center(t % n, t / n);
    Ok((segs, via, len))
}

fn ck_reach(m: &Model) -> f64 {
    m.r.mat[0]
        .iter()
        .chain(&m.r.mat[1])
        .fold(0.0f64, |a, v| a.max(*v as f64))
}
