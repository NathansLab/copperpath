//! Rough electrical figures per net from its copper, for comparing routes and checking routing
//! intents (`low_r`, `low_l`): DC resistance and partial inductance.
//!
//! - Track resistance: R = rho * l / (w * t), copper rho = 1.72e-8 Ohm m (20 degC), t from
//!   `--cu-um` (default 35 um). Segments of a net add up, so for a multi-pin net this is the
//!   resistance of all its copper in series: an upper bound for any pin pair.
//! - Via resistance: barrel of plating `plating` (25 um) around the drill, length = board
//!   thickness (1.6 mm).
//! - Inductance: partial self-inductance of a flat conductor of the net's total track length at
//!   its narrowest width (Grover: L = 0.2 l (ln(2l / (w + t)) + 0.5 + 0.2235 (w + t) / l) nH, mm),
//!   plus 0.2 h (ln(4h / d) + 1) nH per via. No return path or mutual coupling: use it to rank
//!   routes, not as a field solution.

use crate::board::Board;
use crate::geom::{shape_dist, Shape, P};
use crate::model::{Kind, Model};
use serde::Serialize;

/// copper resistivity, Ohm mm
const RHO: f64 = 1.72e-5;

#[derive(Debug, Clone, Copy)]
pub struct Stack {
    /// track copper thickness, mm
    pub cu: f64,
    /// via barrel plating, mm
    pub plating: f64,
    /// board thickness, mm
    pub thickness: f64,
}

impl Default for Stack {
    fn default() -> Self {
        Stack {
            cu: 0.035,
            plating: 0.025,
            thickness: 1.6,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct NetElec {
    pub net: String,
    pub length_mm: f64,
    pub segments: usize,
    pub vias: usize,
    /// narrowest and widest track, mm
    pub width_min_mm: f64,
    pub width_max_mm: f64,
    pub r_mohm: f64,
    pub l_nh: f64,
    /// track length running closer than `near` mm (edge to edge) to another net's copper on the
    /// same layer, and the smallest such gap (mm); filled by [`coupling`]
    pub coupled_mm: f64,
    pub gap_min_mm: Option<f64>,
    /// microstrip impedance (Ohm) of the narrowest track on an outer layer (see
    /// [`outer_dielectric`]); None without outer-layer tracks
    pub z0_ohm: Option<f64>,
}

/// the dielectric under the outer copper layer: (height h, epsilon_r, copper thickness t), mm.
/// From the board stackup when it has one, else FR4 (epsilon_r 4.5, 35 um copper) with 1.51 mm
/// (two layers) or 0.2 mm (prepreg of a multilayer board) to the next copper.
pub fn outer_dielectric(b: &Board) -> (f64, f64, f64) {
    let top = b.stackup.iter().position(|l| l.typ == "copper");
    if let Some(i) = top {
        let t = b.stackup[i].thickness.max(0.005);
        if let Some(d) = b.stackup[i + 1..]
            .iter()
            .take_while(|l| l.typ != "copper")
            .find(|l| l.thickness > 0.0)
        {
            let h: f64 = b.stackup[i + 1..]
                .iter()
                .take_while(|l| l.typ != "copper")
                .map(|l| l.thickness)
                .sum();
            return (h, d.epsilon_r.unwrap_or(4.5), t);
        }
    }
    let h = if b.cu_mask.count_ones() > 2 {
        0.2
    } else {
        1.51
    };
    (h, 4.5, 0.035)
}

/// characteristic impedance (Ohm) of a surface microstrip of width w over a plane at height h,
/// copper thickness t (mm), dielectric epsilon_r (Hammerstad-Jensen with the thickness
/// correction of the width, dw = t/pi (1 + ln(2h/t)); within a few percent of the usual
/// calculators)
pub fn z_microstrip(w: f64, h: f64, t: f64, er: f64) -> f64 {
    let w = w + t / std::f64::consts::PI * (1.0 + (2.0 * h / t).ln());
    let u = w / h;
    let a = if u < 1.0 {
        0.04 * (1.0 - u).powi(2)
    } else {
        0.0
    };
    let eeff = (er + 1.0) / 2.0 + (er - 1.0) / 2.0 * ((1.0 + 12.0 / u).powf(-0.5) + a);
    if u <= 1.0 {
        60.0 / eeff.sqrt() * (8.0 / u + u / 4.0).ln()
    } else {
        120.0 * std::f64::consts::PI / (eeff.sqrt() * (u + 1.393 + 0.667 * (u + 1.444).ln()))
    }
}

/// track width (mm) for impedance `z` on the outer layers of board `b`; None outside 0.02..10 mm
pub fn width_for_z(b: &Board, z: f64) -> Option<f64> {
    let (h, er, t) = outer_dielectric(b);
    let (mut lo, mut hi) = (0.02f64, 10.0f64);
    if z_microstrip(lo, h, t, er) < z || z_microstrip(hi, h, t, er) > z {
        return None;
    }
    // impedance falls with width
    for _ in 0..60 {
        let m = 0.5 * (lo + hi);
        if z_microstrip(m, h, t, er) > z {
            lo = m;
        } else {
            hi = m;
        }
    }
    Some(0.5 * (lo + hi))
}

/// partial self-inductance of a flat conductor (Grover), nH; l, w, t in mm
pub fn l_flat(l: f64, w: f64, t: f64) -> f64 {
    if l <= 0.0 {
        return 0.0;
    }
    let wt = w + t;
    0.2 * l * ((2.0 * l / wt).ln() + 0.5 + 0.2235 * wt / l).max(0.0)
}

/// via inductance, nH; h (barrel length) and d (drill) in mm
pub fn l_via(h: f64, d: f64) -> f64 {
    0.2 * h * ((4.0 * h / d.max(0.01)).ln() + 1.0)
}

fn r3(v: f64) -> f64 {
    (v * 1000.0).round() / 1000.0
}

/// figures of every net with tracks or vias (`sel`: net filter), sorted by net name
pub fn nets(b: &Board, st: &Stack, sel: &dyn Fn(usize) -> bool) -> Vec<NetElec> {
    let n = b.nets.len();
    let mut len = vec![0.0f64; n];
    let mut r = vec![0.0f64; n];
    let mut segs = vec![0usize; n];
    let mut vias = vec![0usize; n];
    let mut wmin = vec![f64::INFINITY; n];
    let mut wmax = vec![0.0f64; n];
    let mut lv = vec![0.0f64; n];
    let mut wout = vec![f64::INFINITY; n];
    for t in &b.tracks {
        if crate::board::is_outer(t.layer) {
            wout[t.net] = wout[t.net].min(t.w);
        }
        let l = (t.b - t.a).len();
        len[t.net] += l;
        r[t.net] += RHO * l / (t.w.max(1e-6) * st.cu);
        segs[t.net] += 1;
        wmin[t.net] = wmin[t.net].min(t.w);
        wmax[t.net] = wmax[t.net].max(t.w);
    }
    for v in &b.vias {
        vias[v.net] += 1;
        let d = v.drill.max(0.05);
        let area = std::f64::consts::PI * (d + st.plating) * st.plating;
        r[v.net] += RHO * st.thickness / area;
        lv[v.net] += l_via(st.thickness, d);
    }
    let mut out: Vec<NetElec> = (1..n)
        .filter(|&i| (segs[i] > 0 || vias[i] > 0) && sel(i))
        .map(|i| {
            let w = if wmin[i].is_finite() { wmin[i] } else { 0.0 };
            NetElec {
                net: b.nets[i].clone(),
                length_mm: r3(len[i]),
                segments: segs[i],
                vias: vias[i],
                width_min_mm: r3(w),
                width_max_mm: r3(wmax[i]),
                r_mohm: r3(r[i] * 1000.0),
                l_nh: r3(l_flat(len[i], w.max(0.01), st.cu) + lv[i]),
                coupled_mm: 0.0,
                gap_min_mm: None,
                z0_ohm: wout[i].is_finite().then(|| {
                    let (h, er, t) = outer_dielectric(b);
                    (z_microstrip(wout[i], h, t, er) * 10.0).round() / 10.0
                }),
            }
        })
        .collect();
    out.sort_by(|a, b| a.net.cmp(&b.net));
    out
}

/// coupled length and smallest gap of each net's tracks to other nets' copper (pads, tracks,
/// vias) on the same layer: tracks are sampled every 0.1 mm; a sample counts when the gap at it
/// is below `near` mm. Fills `coupled_mm` / `gap_min_mm` of `rows` (matched by net name).
pub fn coupling(m: &Model, near: f64, rows: &mut [NetElec]) {
    use std::collections::HashMap;
    let cu: Vec<&crate::model::Obj> = m
        .objs
        .iter()
        .filter(|o| matches!(o.kind, Kind::Pad { .. } | Kind::Track | Kind::Via))
        .collect();
    let cs = 1.0f64;
    let mut grid: HashMap<(i32, i32), Vec<usize>> = HashMap::new();
    for (k, o) in cu.iter().enumerate() {
        let b = o.bbox.inflate(near);
        for x in (b.x0 / cs).floor() as i32..=(b.x1 / cs).floor() as i32 {
            for y in (b.y0 / cs).floor() as i32..=(b.y1 / cs).floor() as i32 {
                grid.entry((x, y)).or_default().push(k);
            }
        }
    }
    let idx: HashMap<String, usize> = rows
        .iter()
        .enumerate()
        .map(|(i, r)| (r.net.clone(), i))
        .collect();
    for t in &m.b.tracks {
        let Some(&ri) = idx.get(&m.b.nets[t.net]) else {
            continue;
        };
        let l = (t.b - t.a).len();
        let n = ((l / 0.1).ceil() as usize).max(1);
        let step = l / n as f64;
        for s in 0..=n {
            let f = s as f64 / n as f64;
            let q = P {
                x: t.a.x + (t.b.x - t.a.x) * f,
                y: t.a.y + (t.b.y - t.a.y) * f,
            };
            let probe = Shape::circle(q, t.w / 2.0);
            let mut gap = f64::INFINITY;
            if let Some(v) = grid.get(&((q.x / cs).floor() as i32, (q.y / cs).floor() as i32)) {
                for &k in v {
                    let o = cu[k];
                    if o.net == t.net || o.layers & (1 << t.layer) == 0 {
                        continue;
                    }
                    gap = gap.min(shape_dist(&probe, &o.shape));
                }
            }
            if gap < near {
                let r = &mut rows[ri];
                if s < n {
                    r.coupled_mm += step;
                }
                r.gap_min_mm = Some(r.gap_min_mm.map_or(gap, |g| g.min(gap)));
            }
        }
    }
    for r in rows.iter_mut() {
        r.coupled_mm = r3(r.coupled_mm);
        r.gap_min_mm = r.gap_min_mm.map(|g| r3(g.max(0.0)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn microstrip_against_reference_values() {
        // 50 Ohm on 0.2 mm prepreg (epsilon_r 4.4, 35 um): calculators give about 0.33-0.36 mm
        let z = z_microstrip(0.34, 0.2, 0.035, 4.4);
        assert!((z - 50.0).abs() < 3.0, "{z}");
        // 1.6 mm two-layer FR4: 50 Ohm needs about 2.9-3.1 mm
        let z = z_microstrip(3.0, 1.51, 0.035, 4.5);
        assert!((z - 50.0).abs() < 3.0, "{z}");
        // wider is lower
        assert!(z_microstrip(0.2, 0.2, 0.035, 4.4) > z_microstrip(0.4, 0.2, 0.035, 4.4));
    }

    #[test]
    fn textbook_values() {
        // 10 mm of 0.25 mm track in 35 um copper: about 20 mOhm and about 9.5 nH
        let r = RHO * 10.0 / (0.25 * 0.035) * 1000.0;
        assert!((r - 19.7).abs() < 0.1, "{r}");
        let l = l_flat(10.0, 0.25, 0.035);
        assert!((l - 9.5).abs() < 0.2, "{l}");
        // a 0.3 mm via through 1.6 mm: about 1.3 nH
        let lv = l_via(1.6, 0.3);
        assert!((lv - 1.3).abs() < 0.05, "{lv}");
    }
}
