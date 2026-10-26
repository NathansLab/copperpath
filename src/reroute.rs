//! Blocker search for `route --reroute-blockers`: for a net that the router could not complete,
//! a coarse multi-layer grid search in which unlocked tracks and vias of other nets are passable
//! at a high cost (locked copper, pads, rule areas and the edge stay walls). The unlocked
//! copper on the cheapest corridor is what blocks the net; the caller rips it and reroutes the
//! target net together with the ripped nets.

use crate::board::NL;
use crate::geom::*;
use crate::model::*;
use crate::sites::{track_map, via_map, R_FREE};
use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap};

pub struct BlockerParams {
    pub res: f64,
    /// window around the net's pins (mm)
    pub margin_mm: f64,
    /// clearance margin as in the router (mm)
    pub margin: f64,
    /// cost factor of a cell that only ripping makes free
    pub soft: u32,
    /// via cost in cells
    pub via_cells: u32,
    /// at most this many blocking nets
    pub max_nets: usize,
}

impl Default for BlockerParams {
    fn default() -> Self {
        BlockerParams {
            res: 0.1,
            margin_mm: 6.0,
            margin: 0.02,
            soft: 25,
            via_cells: 15,
            max_nets: 3,
        }
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct Blocking {
    /// blocking nets in corridor order
    pub nets: Vec<usize>,
    /// uuids of the blocking unlocked tracks/vias of those nets
    pub uuids: Vec<String>,
    /// corridor length (mm) and how many of its cells are blocked
    pub length_mm: f64,
    pub blocked_cells: usize,
}

/// is this board track/via rippable for `net`: unlocked copper of another net that is not a
/// plane net (its fanout tracks and vias stay)
fn soft_net(m: &Model, locked: bool, n: usize, net: usize) -> bool {
    !locked && n != 0 && n != net && !m.cfg.plane_nets.contains(&m.b.nets[n])
}

/// cheapest corridor between two copper components of `net` inside `region` with unlocked
/// foreign copper passable; None when no corridor exists even then (locked copper, pads, edge)
/// or the net has fewer than two components
pub fn find_blockers(m: &Model, net: usize, region: BBox, p: &BlockerParams) -> Option<Blocking> {
    let (pins, copper) = pins_and_copper_of(m, &|n| n == net);
    let pins: Vec<Pin> = pins
        .get(&net)?
        .iter()
        .filter(|pn| region.contains(pn.at))
        .cloned()
        .collect();
    let mut comps: Vec<usize> = pins.iter().map(|pn| pn.comp).collect();
    comps.sort();
    comps.dedup();
    if comps.len() < 2 {
        return None;
    }
    // the component with the most pins and the component nearest to it: one corridor, a small
    // window (a whole-board net would need a whole-board grid)
    let start = *comps
        .iter()
        .max_by_key(|c| pins.iter().filter(|pn| pn.comp == **c).count())
        .unwrap();
    let other = pins
        .iter()
        .filter(|a| a.comp != start)
        .map(|a| {
            let d = pins
                .iter()
                .filter(|b| b.comp == start)
                .map(|b| (a.at - b.at).len())
                .fold(f64::INFINITY, f64::min);
            (d, a.comp)
        })
        .min_by(|x, y| x.0.total_cmp(&y.0))
        .map(|(_, c)| c)?;
    let pins: Vec<Pin> = pins
        .into_iter()
        .filter(|pn| pn.comp == start || pn.comp == other)
        .collect();
    let comps = [start.min(other), start.max(other)];
    let mut bb = BBox::empty();
    for pn in &pins {
        bb.add(pn.at);
    }
    let bb = bb.inflate(p.margin_mm);
    let win = BBox {
        x0: bb.x0.max(region.x0),
        y0: bb.y0.max(region.y0),
        x1: bb.x1.min(region.x1),
        y1: bb.y1.min(region.y1),
    };
    let g = GridSpec::new(win, p.res);
    let n = g.n();
    let wb = g.bbox().inflate(5.0);
    let objs_all: Vec<Obj> = m
        .objs
        .iter()
        .filter(|o| o.bbox.overlaps(&wb))
        .cloned()
        .collect();
    let is_soft =
        |o: &Obj| matches!(o.kind, Kind::Track | Kind::Via) && soft_net(m, o.locked, o.net, net);
    let objs_fixed: Vec<Obj> = objs_all.iter().filter(|o| !is_soft(o)).cloned().collect();
    // neck-down width: the corridor search should not miss what the router could do
    let (w, _) = m.r.track_width(net);
    let (vd, vdr) = m.r.via(net);
    let (hw, vr, dr) = (w / 2.0 + p.margin, vd / 2.0 + p.margin, vdr / 2.0);
    let layers: Vec<usize> = (0..NL)
        .filter(|l| m.net_layers[net] & m.b.cu_mask & (1 << l) != 0)
        .collect();
    let mut full = vec![Vec::new(); NL];
    let mut fixed = vec![Vec::new(); NL];
    for &l in &layers {
        full[l] = track_map(m, &g, net, hw, l, &objs_all);
        fixed[l] = track_map(m, &g, net, hw, l, &objs_fixed);
    }
    let vfull = via_map(m, &g, net, vr, dr, &objs_all);
    let vfixed = via_map(m, &g, net, vr, dr, &objs_fixed);
    // component cells: pads and existing tracks/vias of the net
    let mut comp_of: HashMap<(usize, usize), usize> = HashMap::new();
    for pn in &pins {
        let f = &m.b.fps[pn.fp];
        let pd = &f.pads[pn.pad];
        for s in f.pad_shapes(pd) {
            for &l in &layers {
                if pd.layers & (1 << l) != 0 {
                    g.stamp(&s, 0.0, |x, y| {
                        comp_of.insert((l, g.idx(x, y)), pn.comp);
                    });
                }
            }
        }
        if let Some((x, y)) = g.cell_of(pn.at) {
            for &l in &layers {
                if pd.layers & (1 << l) != 0 {
                    comp_of.insert((l, g.idx(x, y)), pn.comp);
                }
            }
        }
    }
    let no = m.objs.len();
    for &(ci, c) in copper.get(&net).into_iter().flatten() {
        if ci >= no || !comps.contains(&c) {
            continue; // fills: leave out (large); pads and tracks/vias suffice for a corridor
        }
        let o = &m.objs[ci];
        for &l in &layers {
            if o.layers & (1 << l) != 0 {
                g.stamp(&o.shape, 0.0, |x, y| {
                    comp_of.insert((l, g.idx(x, y)), c);
                });
            }
        }
    }
    let nl = NL;
    let node = |l: usize, i: usize| l * n + i;
    let mut dist = vec![u32::MAX; n * nl];
    let mut prev = vec![u32::MAX; n * nl];
    let mut pq = BinaryHeap::new();
    for (&(l, i), &c) in &comp_of {
        if c == start {
            dist[node(l, i)] = 0;
            pq.push(Reverse((0u32, node(l, i) as u32)));
        }
    }
    let cost = |l: usize, i: usize| -> Option<u32> {
        if full[l][i] == R_FREE {
            Some(1)
        } else if fixed[l][i] == R_FREE {
            Some(p.soft)
        } else {
            None
        }
    };
    let mut hit = None;
    while let Some(Reverse((d, k))) = pq.pop() {
        let k = k as usize;
        if d > dist[k] {
            continue;
        }
        let (l, i) = (k / n, k % n);
        if comp_of.get(&(l, i)).is_some_and(|c| *c != start) {
            hit = Some(k);
            break;
        }
        let (x, y) = ((i % g.nx) as i64, (i / g.nx) as i64);
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
            let (a, b) = (x + dx, y + dy);
            if a < 0 || b < 0 || a >= g.nx as i64 || b >= g.ny as i64 {
                continue;
            }
            let j = b as usize * g.nx + a as usize;
            let target = comp_of.get(&(l, j)).is_some_and(|c| *c != start);
            let Some(c) = cost(l, j).or(target.then_some(1)) else {
                continue;
            };
            let step = if dx != 0 && dy != 0 { 14 } else { 10 } * c;
            let nd = d + step;
            if nd < dist[node(l, j)] {
                dist[node(l, j)] = nd;
                prev[node(l, j)] = k as u32;
                pq.push(Reverse((nd, node(l, j) as u32)));
            }
        }
        // through via to every other layer
        let vc = if vfull[i] == R_FREE {
            Some(p.via_cells * 10)
        } else if vfixed[i] == R_FREE {
            Some(p.via_cells * 10 * p.soft)
        } else {
            None
        };
        if let Some(vc) = vc {
            for &l2 in &layers {
                if l2 == l {
                    continue;
                }
                let nd = d + vc;
                if nd < dist[node(l2, i)] {
                    dist[node(l2, i)] = nd;
                    prev[node(l2, i)] = k as u32;
                    pq.push(Reverse((nd, node(l2, i) as u32)));
                }
            }
        }
    }
    let mut k = hit?;
    let mut path = vec![k];
    while prev[k] != u32::MAX {
        k = prev[k] as usize;
        path.push(k);
    }
    path.reverse();
    // blocked cells (track) and via steps on the corridor -> unlocked foreign copper there
    let mut probes: Vec<(Option<usize>, P, f64)> = Vec::new(); // (layer or None = via), point, radius
    let mut len = 0.0;
    for w in path.windows(2) {
        let ((l0, i0), (l1, i1)) = ((w[0] / n, w[0] % n), (w[1] / n, w[1] % n));
        let c1 = g.center(i1 % g.nx, i1 / g.nx);
        if l0 != l1 {
            if vfull[i1] != R_FREE {
                probes.push((None, c1, vr));
            }
            continue;
        }
        len += (c1 - g.center(i0 % g.nx, i0 / g.nx)).len();
        if full[l1][i1] != R_FREE && !comp_of.contains_key(&(l1, i1)) {
            probes.push((Some(l1), c1, hw));
        }
    }
    let mut nets: Vec<usize> = Vec::new();
    let mut by_net: HashMap<usize, Vec<String>> = HashMap::new();
    let tol = p.res * 0.75;
    let mut add = |n: usize, uuid: &str| {
        if !nets.contains(&n) {
            nets.push(n);
        }
        let v = by_net.entry(n).or_default();
        if !uuid.is_empty() && !v.iter().any(|u| u == uuid) {
            v.push(uuid.to_string());
        }
    };
    for (lay, q, r) in &probes {
        for t in &m.b.tracks {
            if !soft_net(m, t.locked, t.net, net) || lay.is_some_and(|l| l != t.layer) {
                continue;
            }
            let c = m.r.clr(net, t.net, crate::board::is_outer(t.layer));
            if dist_seg(*q, t.a, t.b) <= c + r + t.w / 2.0 + tol {
                add(t.net, &t.uuid);
            }
        }
        for v in &m.b.vias {
            if !soft_net(m, v.locked, v.net, net) {
                continue;
            }
            let c = layers
                .iter()
                .filter(|l| lay.is_none_or(|x| x == **l))
                .map(|l| m.r.clr(net, v.net, crate::board::is_outer(*l)))
                .fold(0.0f64, f64::max);
            let hole = if lay.is_none() {
                v.drill / 2.0 + dr + m.r.min_hole_to_hole
            } else {
                0.0
            };
            if (v.at - *q).len() <= (c + r + v.d / 2.0).max(hole) + tol {
                add(v.net, &v.uuid);
            }
        }
    }
    nets.truncate(p.max_nets);
    // a ripped net loses all its unlocked copper, not only the pieces on the corridor: it is
    // routed again as a whole, and leftover pieces would dangle
    let uuids = nets
        .iter()
        .flat_map(|&n| {
            let mut v = by_net.get(&n).cloned().unwrap_or_default();
            let rest =
                m.b.tracks
                    .iter()
                    .filter(|t| t.net == n && !t.locked)
                    .map(|t| &t.uuid)
                    .chain(
                        m.b.vias
                            .iter()
                            .filter(|x| x.net == n && !x.locked)
                            .map(|x| &x.uuid),
                    );
            for u in rest {
                if !u.is_empty() && !v.contains(u) {
                    v.push(u.clone());
                }
            }
            v
        })
        .collect();
    Some(Blocking {
        nets,
        uuids,
        length_mm: (len * 100.0).round() / 100.0,
        blocked_cells: probes.len(),
    })
}

/// the board text without the items (segment, arc, via) with these uuids
pub fn remove_items(text: &str, uuids: &[String]) -> (String, usize) {
    let mut cut: Vec<(usize, usize)> = Vec::new();
    let b = text.as_bytes();
    for u in uuids {
        let pat = format!("(uuid \"{u}\")");
        let Some(at) = text.find(&pat) else { continue };
        // the enclosing item: walk back to its opening paren at depth -1
        let mut depth = 0i32;
        let mut s = at;
        loop {
            if s == 0 {
                break;
            }
            s -= 1;
            match b[s] {
                b')' => depth += 1,
                b'(' => {
                    if depth == 0 {
                        break;
                    }
                    depth -= 1;
                }
                _ => {}
            }
        }
        let head: String = text[s + 1..].chars().take(8).collect();
        if !(head.starts_with("segment") || head.starts_with("via") || head.starts_with("arc")) {
            continue;
        }
        let mut d = 0i32;
        let mut e = s;
        while e < b.len() {
            match b[e] {
                b'(' => d += 1,
                b')' => {
                    d -= 1;
                    if d == 0 {
                        break;
                    }
                }
                _ => {}
            }
            e += 1;
        }
        cut.push((s, e + 1));
    }
    cut.sort();
    cut.dedup();
    let mut out = String::with_capacity(text.len());
    let mut last = 0;
    for (s, e) in &cut {
        if *s < last {
            continue;
        }
        out.push_str(&text[last..*s]);
        last = *e;
    }
    out.push_str(&text[last..]);
    (out, cut.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn removes_items_by_uuid() {
        let t = "(kicad_pcb (segment (start 0 0) (end 1 0) (uuid \"a\")) (via (at 1 1) (uuid \"b\")) (segment (start 2 2) (end 3 3) (uuid \"c\")))";
        let (o, k) = remove_items(t, &["a".into(), "b".into(), "x".into()]);
        assert_eq!(k, 2);
        assert!(!o.contains("\"a\"") && !o.contains("\"b\"") && o.contains("\"c\""));
    }
}
