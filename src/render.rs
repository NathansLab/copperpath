//! `render`: PNG images of a board region for AI agents and humans. Native rasteriser (no
//! Python): all copper layers colour-coded and semi-transparent, zone fills, pads with
//! `REF.PIN` / net labels (collision-free, only inside the region), vias, courtyards, rule areas
//! and HV clearance halos shaded, ratsnest lines of open connections, DRC markers from a
//! `kicad-cli` JSON report, a millimetre grid with coordinates and a scale bar. `--per-layer`
//! draws one small image per copper layer side by side (small multiples).

use crate::board::*;
use crate::font;
use crate::geom::*;
use crate::model;
use rayon::prelude::*;
use serde_json::{json, Value};

/// KiCad-like layer colours: F.Cu, In1..In4, B.Cu
pub const LAYER_RGB: [[u8; 3]; NL] = [
    [220, 60, 60],
    [110, 200, 110],
    [230, 150, 50],
    [80, 200, 210],
    [225, 105, 160],
    [80, 130, 230],
];
const BG_OUT: [u8; 3] = [14, 14, 18];
const BG_IN: [u8; 3] = [30, 32, 40];

/// a DRC marker from a kicad-cli JSON report (`violations` and `unconnected_items`)
#[derive(Debug, Clone)]
pub struct Marker {
    pub at: P,
    /// second item (unconnected items: the other end)
    pub to: Option<P>,
    pub kind: String,
    pub error: bool,
}

/// parse a `kicad-cli pcb drc --format json` report (positions in mm)
pub fn parse_drc(text: &str) -> Result<Vec<Marker>, String> {
    let v: Value = serde_json::from_str(text).map_err(|e| format!("DRC report: {e}"))?;
    let mut out = Vec::new();
    let pos =
        |it: &Value| -> Option<P> { Some(p(it["pos"]["x"].as_f64()?, it["pos"]["y"].as_f64()?)) };
    for key in ["violations", "unconnected_items", "schematic_parity"] {
        for viol in v[key].as_array().into_iter().flatten() {
            let items: Vec<P> = viol["items"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(pos)
                .collect();
            let Some(&at) = items.first() else { continue };
            out.push(Marker {
                at,
                to: if key == "unconnected_items" {
                    items.get(1).copied()
                } else {
                    None
                },
                kind: viol["type"].as_str().unwrap_or(key).to_string(),
                error: viol["severity"].as_str() != Some("warning"),
            });
        }
    }
    Ok(out)
}

pub struct Opts {
    /// region in board mm (None: the board with a 1 mm margin)
    pub roi: Option<BBox>,
    /// copper layers to draw (mask, `CU` order)
    pub layers: u8,
    /// only these nets are drawn in colour; all others are grey (None: all)
    pub nets: Option<regex::Regex>,
    pub hl_net: Option<usize>,
    pub fills: bool,
    pub labels: bool,
    pub courtyards: bool,
    pub opens: bool,
    /// HV halos: nets whose outer-layer clearance to another net in the region is at least
    /// this (mm) get a shaded band of that width (0 = off)
    pub halo_min: f64,
    pub drc: Vec<Marker>,
    /// image width in pixels
    pub px: usize,
    pub per_layer: bool,
    pub title: String,
}

/// pixel mapping of one tile: world (x, y) -> ((x - x0) * s, (y - y0) * s)
#[derive(Clone, Copy)]
struct View {
    x0: f64,
    y0: f64,
    s: f64,
    w: usize,
    h: usize,
}

impl View {
    fn px(&self, q: P) -> (f64, f64) {
        ((q.x - self.x0) * self.s, (q.y - self.y0) * self.s)
    }
    fn bbox(&self) -> BBox {
        BBox {
            x0: self.x0,
            y0: self.y0,
            x1: self.x0 + self.w as f64 / self.s,
            y1: self.y0 + self.h as f64 / self.s,
        }
    }
}

/// coverage buffer (0..255) of one tile
struct Cov {
    v: Vec<u8>,
    w: usize,
    h: usize,
}

impl Cov {
    fn new(w: usize, h: usize) -> Cov {
        Cov {
            v: vec![0; w * h],
            w,
            h,
        }
    }
    #[inline]
    fn put(&mut self, x: usize, y: usize, c: u8) {
        let o = &mut self.v[y * self.w + x];
        if c > *o {
            *o = c;
        }
    }
    /// anti-aliased capsule (pixel units)
    fn capsule(&mut self, a: (f64, f64), b: (f64, f64), r: f64) {
        let r = r.max(0.6);
        let x0 = (a.0.min(b.0) - r - 1.0).floor().max(0.0) as usize;
        let y0 = (a.1.min(b.1) - r - 1.0).floor().max(0.0) as usize;
        let x1 = ((a.0.max(b.0) + r + 1.0).ceil().max(0.0) as usize).min(self.w);
        let y1 = ((a.1.max(b.1) + r + 1.0).ceil().max(0.0) as usize).min(self.h);
        let (dx, dy) = (b.0 - a.0, b.1 - a.1);
        let l2 = dx * dx + dy * dy;
        for y in y0..y1 {
            let cy = y as f64 + 0.5;
            for x in x0..x1 {
                let cx = x as f64 + 0.5;
                let t = if l2 < 1e-12 {
                    0.0
                } else {
                    (((cx - a.0) * dx + (cy - a.1) * dy) / l2).clamp(0.0, 1.0)
                };
                let (ex, ey) = (cx - a.0 - dx * t, cy - a.1 - dy * t);
                let d = (ex * ex + ey * ey).sqrt();
                let c = (r - d + 0.5).clamp(0.0, 1.0);
                if c > 0.0 {
                    self.put(x, y, (c * 255.0) as u8);
                }
            }
        }
    }
    /// even-odd polygon fill at pixel centres (pixel units)
    fn poly(&mut self, pts: &[(f64, f64)]) {
        scan_poly(pts, self.w, self.h, |i| {
            self.v[i] = 255;
        });
    }
    fn shape(&mut self, v: &View, s: &Shape, grow: f64) {
        match s {
            Shape::Capsule { a, b, r } => self.capsule(v.px(*a), v.px(*b), (r + grow) * v.s),
            Shape::Poly { pts, r } => {
                let pp: Vec<(f64, f64)> = pts.iter().map(|q| v.px(*q)).collect();
                if pp.len() >= 3 {
                    self.poly(&pp);
                }
                let rr = (r + grow) * v.s;
                if rr > 0.05 || pp.len() < 3 {
                    for i in 0..pp.len() {
                        self.capsule(pp[i], pp[(i + 1) % pp.len()], rr);
                    }
                }
            }
        }
    }
}

/// call f(pixel index) for every pixel centre inside the polygon (even-odd)
fn scan_poly<F: FnMut(usize)>(pts: &[(f64, f64)], w: usize, h: usize, mut f: F) {
    let n = pts.len();
    if n < 3 {
        return;
    }
    let (mut ymin, mut ymax) = (f64::INFINITY, f64::NEG_INFINITY);
    for q in pts {
        ymin = ymin.min(q.1);
        ymax = ymax.max(q.1);
    }
    let y0 = (ymin - 0.5).ceil().max(0.0) as usize;
    let y1 = ((ymax - 0.5).floor().min(h as f64 - 1.0)).max(-1.0);
    if y1 < 0.0 {
        return;
    }
    let mut xs: Vec<f64> = Vec::new();
    for y in y0..=(y1 as usize) {
        let cy = y as f64 + 0.5;
        xs.clear();
        for i in 0..n {
            let (a, b) = (pts[i], pts[(i + 1) % n]);
            if (a.1 > cy) != (b.1 > cy) {
                xs.push(a.0 + (cy - a.1) * (b.0 - a.0) / (b.1 - a.1));
            }
        }
        xs.sort_by(|u, v| u.total_cmp(v));
        for k in (0..xs.len() / 2).map(|k| 2 * k) {
            let i0 = (xs[k] - 0.5).ceil().max(0.0) as i64;
            let i1 = ((xs[k + 1] - 0.5).floor() as i64).min(w as i64 - 1);
            for x in i0..=i1 {
                f(y * w + x as usize);
            }
        }
    }
}

/// RGB tile
struct Img {
    px: Vec<[u8; 3]>,
    w: usize,
    h: usize,
}

impl Img {
    #[inline]
    fn blend(&mut self, x: i64, y: i64, c: [u8; 3], a: f32) {
        if x < 0 || y < 0 || x >= self.w as i64 || y >= self.h as i64 {
            return;
        }
        let o = &mut self.px[y as usize * self.w + x as usize];
        for k in 0..3 {
            o[k] = (o[k] as f32 * (1.0 - a) + c[k] as f32 * a) as u8;
        }
    }
    fn paint(&mut self, cov: &Cov, c: [u8; 3], a: f32) {
        for (o, &v) in self.px.iter_mut().zip(&cov.v) {
            if v > 0 {
                let aa = a * v as f32 / 255.0;
                for k in 0..3 {
                    o[k] = (o[k] as f32 * (1.0 - aa) + c[k] as f32 * aa) as u8;
                }
            }
        }
    }
    /// thin line (pixel units), clipped
    fn line(&mut self, a: (f64, f64), b: (f64, f64), c: [u8; 3], al: f32, dash: usize) {
        let Some((a, b)) = clip(a, b, self.w as f64, self.h as f64) else {
            return;
        };
        let n = ((b.0 - a.0).abs().max((b.1 - a.1).abs()).ceil() as usize).max(1);
        for k in 0..=n {
            if dash > 0 && (k / dash) % 2 == 1 {
                continue;
            }
            let t = k as f64 / n as f64;
            self.blend(
                (a.0 + (b.0 - a.0) * t) as i64,
                (a.1 + (b.1 - a.1) * t) as i64,
                c,
                al,
            );
        }
    }
    fn rect(&mut self, x: i64, y: i64, w: usize, h: usize, c: [u8; 3], a: f32) {
        for yy in y..y + h as i64 {
            for xx in x..x + w as i64 {
                self.blend(xx, yy, c, a);
            }
        }
    }
    fn text(&mut self, t: &str, x: i64, y: i64, s: usize, c: [u8; 3]) {
        font::draw(t, x, y, s, |xx, yy| self.blend(xx, yy, c, 1.0));
    }
    /// text on a dark box
    fn label(&mut self, t: &str, x: i64, y: i64, s: usize, c: [u8; 3]) {
        let (tw, th) = font::text_size(t, s);
        self.rect(x - 1, y - 1, tw + 2, th + 2, [0, 0, 0], 0.6);
        self.text(t, x, y, s, c);
    }
}

/// Liang-Barsky clip of a segment to [0, w) x [0, h)
fn clip(a: (f64, f64), b: (f64, f64), w: f64, h: f64) -> Option<((f64, f64), (f64, f64))> {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let (mut t0, mut t1) = (0.0f64, 1.0f64);
    for (pp, q) in [
        (-dx, a.0),
        (dx, w - 1.0 - a.0),
        (-dy, a.1),
        (dy, h - 1.0 - a.1),
    ] {
        if pp.abs() < 1e-12 {
            if q < 0.0 {
                return None;
            }
        } else {
            let r = q / pp;
            if pp < 0.0 {
                t0 = t0.max(r);
            } else {
                t1 = t1.min(r);
            }
        }
    }
    (t0 <= t1).then_some({
        (
            (a.0 + dx * t0, a.1 + dy * t0),
            (a.0 + dx * t1, a.1 + dy * t1),
        )
    })
}

fn lighten(c: [u8; 3], k: f32) -> [u8; 3] {
    [0, 1, 2].map(|i| (c[i] as f32 + (255.0 - c[i] as f32) * k) as u8)
}

/// short display name of a net: the last hierarchy component of long names
pub fn short_net(n: &str) -> String {
    let s = if n.chars().count() > 16 {
        n.rsplit('/').next().unwrap_or(n)
    } else {
        n
    };
    if s.chars().count() > 18 {
        format!("{}~", s.chars().take(17).collect::<String>())
    } else {
        s.to_string()
    }
}

/// a "nice" grid step (1, 2, 5 x 10^k mm) for about `n` lines over `span` mm
fn nice_step(span: f64, n: f64) -> f64 {
    let raw = (span / n).max(1e-3);
    let e = 10f64.powf(raw.log10().floor());
    for k in [1.0, 2.0, 5.0, 10.0] {
        if k * e >= raw {
            return k * e;
        }
    }
    10.0 * e
}

fn fmt_mm(v: f64, step: f64) -> String {
    let dec = if step >= 1.0 {
        0
    } else if step >= 0.1 {
        1
    } else {
        2
    };
    format!("{v:.dec$}")
}

/// an open connection drawn as a ratsnest line
#[derive(Debug, Clone, serde::Serialize)]
pub struct Rat {
    pub net: String,
    pub a: [f64; 2],
    pub b: [f64; 2],
    pub pads: [String; 2],
    pub len_mm: f64,
}

/// ratsnest of the open connections: per net, a minimum spanning tree over its copper
/// components (closest pad pair between two components)
pub fn ratsnest(m: &model::Model, sel: &(dyn Fn(usize) -> bool + Sync)) -> Vec<Rat> {
    let (pins, _) = model::pins_and_copper_of(m, sel);
    let mut keys: Vec<&usize> = pins.keys().collect();
    keys.sort();
    let mut out = Vec::new();
    for &n in keys {
        let ps = &pins[&n];
        let mut comps: Vec<usize> = ps.iter().map(|q| q.comp).collect();
        comps.sort();
        comps.dedup();
        if comps.len() < 2 {
            continue;
        }
        // Prim over components, edge = closest pin pair
        let k = comps.len();
        let ci = |c: usize| comps.binary_search(&c).unwrap();
        let groups: Vec<Vec<usize>> = (0..k)
            .map(|g| (0..ps.len()).filter(|&i| ci(ps[i].comp) == g).collect())
            .collect();
        let mut intree = vec![false; k];
        intree[0] = true;
        // best[g] = (dist, pin in tree, pin in g)
        let mut best: Vec<(f64, usize, usize)> = vec![(f64::INFINITY, 0, 0); k];
        let relax = |g0: usize, best: &mut Vec<(f64, usize, usize)>, intree: &[bool]| {
            for g in 0..k {
                if intree[g] {
                    continue;
                }
                for &i in &groups[g0] {
                    for &j in &groups[g] {
                        let d = (ps[i].at - ps[j].at).len();
                        if d < best[g].0 {
                            best[g] = (d, i, j);
                        }
                    }
                }
            }
        };
        relax(0, &mut best, &intree);
        for _ in 1..k {
            let Some(g) = (0..k)
                .filter(|&g| !intree[g])
                .min_by(|&a, &b| best[a].0.total_cmp(&best[b].0))
            else {
                break;
            };
            intree[g] = true;
            let (d, i, j) = best[g];
            let name = |q: &model::Pin| {
                let f = &m.b.fps[q.fp];
                format!("{}.{}", f.refd, f.pads[q.pad].num)
            };
            out.push(Rat {
                net: m.b.nets[n].clone(),
                a: [ps[i].at.x, ps[i].at.y],
                b: [ps[j].at.x, ps[j].at.y],
                pads: [name(&ps[i]), name(&ps[j])],
                len_mm: (d * 1000.0).round() / 1000.0,
            });
            relax(g, &mut best, &intree);
        }
    }
    out
}

struct Ctx<'a> {
    b: &'a Board,
    o: &'a Opts,
    /// net is drawn in colour
    on: Vec<bool>,
    /// HV halo width per net (0 = none)
    halo: Vec<f64>,
    rats: &'a [Rat],
}

/// render one tile: layers `lm` into a w x h image of view v
fn tile(c: &Ctx, v: View, lm: u8, labels: bool) -> (Img, usize) {
    let (b, o) = (c.b, c.o);
    let vb = v.bbox();
    let mut img = Img {
        px: vec![BG_OUT; v.w * v.h],
        w: v.w,
        h: v.h,
    };
    // board area (even-odd over all outlines)
    let mut inside = vec![false; v.w * v.h];
    for ol in &b.edge {
        let pp: Vec<(f64, f64)> = ol.iter().map(|q| v.px(*q)).collect();
        scan_poly(&pp, v.w, v.h, |i| inside[i] = !inside[i]);
    }
    if b.edge.is_empty() {
        inside.iter_mut().for_each(|x| *x = true);
    }
    for (px, &ins) in img.px.iter_mut().zip(&inside) {
        if ins {
            *px = BG_IN;
        }
    }
    let layers: Vec<usize> = (0..NL)
        .rev()
        .filter(|l| lm & b.cu_mask & (1 << l) != 0)
        .collect();
    let lit = |net: usize| c.on.get(net).copied().unwrap_or(true);
    // per layer: fills, coloured copper, grey copper (nets filtered out)
    let per: Vec<(usize, Cov, Cov, Cov)> = layers
        .par_iter()
        .map(|&l| {
            let bit = 1u8 << l;
            let mut fill = Cov::new(v.w, v.h);
            let mut cu = Cov::new(v.w, v.h);
            let mut grey = Cov::new(v.w, v.h);
            if o.fills {
                for z in b.zones.iter().filter(|z| !z.keepout && z.net != 0) {
                    for (zl, pts) in &z.fills {
                        if *zl == l {
                            let mut bb = BBox::empty();
                            pts.iter().for_each(|q| bb.add(*q));
                            if bb.overlaps(&vb) {
                                let pp: Vec<(f64, f64)> = pts.iter().map(|q| v.px(*q)).collect();
                                if lit(z.net) {
                                    fill.poly(&pp);
                                } else {
                                    grey.poly(&pp);
                                }
                            }
                        }
                    }
                }
            }
            for t in b.tracks.iter().filter(|t| t.layer == l) {
                let s = Shape::Capsule {
                    a: t.a,
                    b: t.b,
                    r: t.w / 2.0,
                };
                if !s.bbox().overlaps(&vb) {
                    continue;
                }
                if lit(t.net) {
                    cu.shape(&v, &s, 0.0)
                } else {
                    grey.shape(&v, &s, 0.0)
                }
            }
            for (s, gl) in &b.graphics {
                if gl & bit != 0 && s.bbox().overlaps(&vb) {
                    cu.shape(&v, s, 0.0);
                }
            }
            for f in &b.fps {
                for pd in &f.pads {
                    if pd.layers & bit == 0 || pd.kind == "np_thru_hole" {
                        continue;
                    }
                    let barrel = pd.inner_barrel_only && !is_outer(l);
                    let shapes = if barrel {
                        vec![Shape::circle(f.pad_world(pd).0, pd.drill / 2.0)]
                    } else {
                        f.pad_shapes(pd)
                    };
                    for s in shapes {
                        if !s.bbox().overlaps(&vb) {
                            continue;
                        }
                        if lit(pd.net) {
                            cu.shape(&v, &s, 0.0)
                        } else {
                            grey.shape(&v, &s, 0.0)
                        }
                    }
                }
            }
            (l, fill, cu, grey)
        })
        .collect();
    // HV halos on the outer layers
    let mut halo = Cov::new(v.w, v.h);
    if c.halo.iter().any(|h| *h > 0.0) && lm & 0x21 != 0 {
        let outer = lm & b.cu_mask & 0x21;
        for f in &b.fps {
            for pd in &f.pads {
                let h = c.halo.get(pd.net).copied().unwrap_or(0.0);
                if h > 0.0 && pd.layers & outer != 0 {
                    for s in f.pad_shapes(pd) {
                        if s.bbox().inflate(h).overlaps(&vb) {
                            halo.shape(&v, &s, h);
                        }
                    }
                }
            }
        }
        for t in &b.tracks {
            let h = c.halo.get(t.net).copied().unwrap_or(0.0);
            if h > 0.0 && outer & (1 << t.layer) != 0 {
                let s = Shape::Capsule {
                    a: t.a,
                    b: t.b,
                    r: t.w / 2.0,
                };
                if s.bbox().inflate(h).overlaps(&vb) {
                    halo.shape(&v, &s, h);
                }
            }
        }
    }
    for (l, fill, _, grey) in &per {
        img.paint(fill, LAYER_RGB[*l], 0.28);
        img.paint(grey, [120, 120, 120], 0.22);
    }
    // halo as a diagonal hatch
    for y in 0..v.h {
        for x in 0..v.w {
            let a = halo.v[y * v.w + x];
            if a > 0 && (x + y) % 6 < 2 {
                img.blend(x as i64, y as i64, [200, 120, 255], 0.55);
            }
        }
    }
    for (l, _, cu, _) in &per {
        let col = match o.hl_net {
            Some(_) => lighten(LAYER_RGB[*l], 0.25),
            None => LAYER_RGB[*l],
        };
        img.paint(cu, col, 0.62);
    }
    // holes, vias
    for f in &b.fps {
        for pd in &f.pads {
            if pd.drill > 0.0 && pd.kind.contains("thru_hole") {
                let q = v.px(f.pad_world(pd).0);
                let r = pd.drill / 2.0 * v.s;
                disk(&mut img, q, r, [10, 10, 12], 0.95);
            }
        }
    }
    for via in &b.vias {
        let s = Shape::circle(via.at, via.d / 2.0);
        if !s.bbox().overlaps(&vb) {
            continue;
        }
        let q = v.px(via.at);
        let (ring, a) = if lit(via.net) {
            ([215, 215, 200], 0.9)
        } else {
            ([110, 110, 110], 0.6)
        };
        disk(&mut img, q, via.d / 2.0 * v.s, ring, a);
        disk(&mut img, q, via.drill / 2.0 * v.s, [20, 20, 24], 0.95);
    }
    // rule areas: orange hatch + outline
    for z in b.zones.iter().filter(|z| z.keepout && z.layers & lm != 0) {
        let pp: Vec<(f64, f64)> = z.outline.iter().map(|q| v.px(*q)).collect();
        let w = v.w;
        let mut cells = Vec::new();
        scan_poly(&pp, v.w, v.h, |i| cells.push(i));
        for i in cells {
            let (x, y) = (i % w, i / w);
            if (x + v.h - y) % 10 < 2 {
                img.blend(x as i64, y as i64, [255, 150, 40], 0.5);
            }
        }
        for i in 0..pp.len() {
            img.line(pp[i], pp[(i + 1) % pp.len()], [255, 150, 40], 0.9, 4);
        }
    }
    if o.courtyards {
        for f in &b.fps {
            let cy = f.crtyd_world();
            if cy.len() >= 2 {
                let pp: Vec<(f64, f64)> = cy.iter().map(|q| v.px(*q)).collect();
                for i in 0..pp.len() {
                    img.line(pp[i], pp[(i + 1) % pp.len()], [230, 80, 230], 0.6, 0);
                }
            }
        }
    }
    // board outline
    for (a, e) in &b.edge_segs {
        img.line(v.px(*a), v.px(*e), [230, 220, 90], 0.9, 0);
    }
    for ol in &b.edge {
        for i in 0..ol.len() {
            img.line(
                v.px(ol[i]),
                v.px(ol[(i + 1) % ol.len()]),
                [230, 220, 90],
                0.9,
                0,
            );
        }
    }
    // grid
    let span = (vb.x1 - vb.x0).max(vb.y1 - vb.y0);
    let step = nice_step(span, 8.0);
    let gx0 = (vb.x0 / step).ceil() as i64;
    let gx1 = (vb.x1 / step).floor() as i64;
    let gy0 = (vb.y0 / step).ceil() as i64;
    let gy1 = (vb.y1 / step).floor() as i64;
    for k in gx0..=gx1 {
        let x = (k as f64 * step - v.x0) * v.s;
        img.line((x, 0.0), (x, v.h as f64), [255, 255, 255], 0.10, 0);
    }
    for k in gy0..=gy1 {
        let y = (k as f64 * step - v.y0) * v.s;
        img.line((0.0, y), (v.w as f64, y), [255, 255, 255], 0.10, 0);
    }
    // ratsnest
    for rt in c.rats {
        if !lit(c.b.net_id.get(&rt.net).copied().unwrap_or(0)) {
            continue;
        }
        let (a, e) = (v.px(p(rt.a[0], rt.a[1])), v.px(p(rt.b[0], rt.b[1])));
        img.line(a, e, [255, 255, 255], 0.85, 0);
        disk(&mut img, a, 2.0, [255, 255, 255], 0.9);
        disk(&mut img, e, 2.0, [255, 255, 255], 0.9);
    }
    // DRC markers
    for mk in &o.drc {
        let q = v.px(mk.at);
        let col = if mk.error {
            [255, 40, 40]
        } else {
            [255, 200, 0]
        };
        if let Some(t) = mk.to {
            img.line(q, v.px(t), col, 0.9, 3);
        }
        for d in -5i64..=5 {
            img.blend(q.0 as i64 + d, q.1 as i64 + d, col, 1.0);
            img.blend(q.0 as i64 + d, q.1 as i64 - d, col, 1.0);
            img.blend(q.0 as i64 + d + 1, q.1 as i64 + d, col, 1.0);
            img.blend(q.0 as i64 + d + 1, q.1 as i64 - d, col, 1.0);
        }
        ring(&mut img, q, 8.0, col);
    }
    let mut nlab = 0;
    let mut occ = vec![false; v.w * v.h];
    if labels {
        nlab = draw_labels(c, &v, lm, &mut img, &mut occ);
    }
    // coordinate labels along the top and left edges, scale bar bottom left
    for k in gx0..=gx1 {
        let x = (k as f64 * step - v.x0) * v.s;
        img.label(
            &fmt_mm(k as f64 * step, step),
            x as i64 + 2,
            2,
            1,
            [190, 190, 190],
        );
    }
    for k in gy0..=gy1 {
        let y = (k as f64 * step - v.y0) * v.s;
        img.label(
            &fmt_mm(k as f64 * step, step),
            2,
            y as i64 + 2,
            1,
            [190, 190, 190],
        );
    }
    let bar = (step * v.s) as i64;
    let (bx, by) = (8i64, v.h as i64 - 14);
    img.rect(bx - 3, by - 12, bar as usize + 6, 22, [0, 0, 0], 0.6);
    img.rect(bx, by, bar as usize, 3, [255, 255, 255], 1.0);
    img.text(
        &format!("{} mm", fmt_mm(step, step)),
        bx,
        by - 10,
        1,
        [255, 255, 255],
    );
    (img, nlab)
}

fn disk(img: &mut Img, c: (f64, f64), r: f64, col: [u8; 3], a: f32) {
    let r = r.max(0.8);
    for y in (c.1 - r - 1.0) as i64..=(c.1 + r + 1.0) as i64 {
        for x in (c.0 - r - 1.0) as i64..=(c.0 + r + 1.0) as i64 {
            let d = ((x as f64 + 0.5 - c.0).powi(2) + (y as f64 + 0.5 - c.1).powi(2)).sqrt();
            let k = (r - d + 0.5).clamp(0.0, 1.0) as f32;
            if k > 0.0 {
                img.blend(x, y, col, a * k);
            }
        }
    }
}

fn ring(img: &mut Img, c: (f64, f64), r: f64, col: [u8; 3]) {
    let n = (r * 8.0) as usize;
    for k in 0..n {
        let t = k as f64 / n as f64 * std::f64::consts::TAU;
        img.blend(
            (c.0 + r * t.cos()) as i64,
            (c.1 + r * t.sin()) as i64,
            col,
            1.0,
        );
    }
}

/// place a label of lines at centre (cx, cy) if the box is free; mark it occupied
fn try_label(
    img: &mut Img,
    occ: &mut [bool],
    lines: &[(&str, [u8; 3])],
    cx: f64,
    cy: f64,
    s: usize,
) -> bool {
    let tw = lines
        .iter()
        .map(|(t, _)| font::text_size(t, s).0)
        .max()
        .unwrap_or(0);
    let lh = 8 * s;
    let th = lh * lines.len();
    let x0 = (cx - tw as f64 / 2.0).round() as i64 - 1;
    let y0 = (cy - th as f64 / 2.0).round() as i64 - 1;
    let (w, h) = (tw as i64 + 2, th as i64 + 1);
    if x0 < 0 || y0 < 0 || x0 + w >= img.w as i64 || y0 + h >= img.h as i64 {
        return false;
    }
    for y in y0..y0 + h {
        for x in x0..x0 + w {
            if occ[y as usize * img.w + x as usize] {
                return false;
            }
        }
    }
    for y in (y0 - 1).max(0)..(y0 + h + 1).min(img.h as i64) {
        for x in (x0 - 1).max(0)..(x0 + w + 1).min(img.w as i64) {
            occ[y as usize * img.w + x as usize] = true;
        }
    }
    img.rect(x0, y0, w as usize, h as usize, [0, 0, 0], 0.55);
    for (k, (t, col)) in lines.iter().enumerate() {
        let (lw, _) = font::text_size(t, s);
        let x = (cx - lw as f64 / 2.0).round() as i64;
        img.text(t, x, y0 + 1 + (k * lh) as i64, s, *col);
    }
    true
}

fn draw_labels(c: &Ctx, v: &View, lm: u8, img: &mut Img, occ: &mut [bool]) -> usize {
    let b = c.b;
    let vb = v.bbox();
    let mut n = 0;
    // pads, largest first
    let mut pads: Vec<(f64, usize, usize, P)> = Vec::new();
    for (fi, f) in b.fps.iter().enumerate() {
        for (pi, pd) in f.pads.iter().enumerate() {
            if pd.layers & lm == 0 && pd.kind != "np_thru_hole" {
                continue;
            }
            let (q, _) = f.pad_world(pd);
            if vb.contains(q) {
                pads.push((pd.size.0 * pd.size.1, fi, pi, q));
            }
        }
    }
    pads.sort_by(|a, b| b.0.total_cmp(&a.0));
    for (_, fi, pi, q) in pads {
        let f = &b.fps[fi];
        let pd = &f.pads[pi];
        let (cx, cy) = v.px(q);
        let id = if pd.num.is_empty() {
            f.refd.clone()
        } else {
            format!("{}.{}", f.refd, pd.num)
        };
        let net = short_net(&b.nets[pd.net]);
        let lit = c.on.get(pd.net).copied().unwrap_or(true);
        let tc = if lit {
            [255, 255, 255]
        } else {
            [150, 150, 150]
        };
        let nc = if lit {
            [255, 230, 120]
        } else {
            [140, 140, 110]
        };
        // scale 2 when the label fits into the pad's box
        let (bw, bh) = {
            let mut bb = BBox::empty();
            for sh in f.pad_shapes(pd) {
                let x = sh.bbox();
                bb.add(p(x.x0, x.y0));
                bb.add(p(x.x1, x.y1));
            }
            ((bb.x1 - bb.x0) * v.s, (bb.y1 - bb.y0) * v.s)
        };
        let tw2 = font::text_size(&id, 2).0.max(font::text_size(&net, 2).0) as f64;
        let s = if tw2 <= bw * 1.15 && 32.0 <= bh * 1.3 {
            2
        } else {
            1
        };
        let mut lines: Vec<(&str, [u8; 3])> = vec![(&id, tc)];
        if pd.net != 0 {
            lines.push((&net, nc));
        }
        // try on the pad, then just beside it
        let off = (pd.size.0.max(pd.size.1) / 2.0) * v.s + 9.0;
        let ok = try_label(img, occ, &lines, cx, cy, s)
            || try_label(img, occ, &lines, cx, cy - off, 1)
            || try_label(img, occ, &lines, cx, cy + off, 1)
            || try_label(img, occ, &lines[..1], cx, cy, 1);
        n += ok as usize;
    }
    // net names on long track segments
    for t in &b.tracks {
        if lm & (1 << t.layer) == 0 || t.net == 0 {
            continue;
        }
        let (a, e) = (v.px(t.a), v.px(t.b));
        let len = ((e.0 - a.0).powi(2) + (e.1 - a.1).powi(2)).sqrt();
        let name = short_net(&b.nets[t.net]);
        let (tw, _) = font::text_size(&name, 1);
        if len < tw as f64 + 30.0 {
            continue;
        }
        let m = ((a.0 + e.0) / 2.0, (a.1 + e.1) / 2.0);
        if m.0 < 0.0 || m.1 < 0.0 || m.0 >= v.w as f64 || m.1 >= v.h as f64 {
            continue;
        }
        let col = lighten(LAYER_RGB[t.layer], 0.5);
        n += try_label(img, occ, &[(&name, col)], m.0, m.1, 1) as usize;
    }
    n
}

/// result of [`render`]
pub struct Out {
    pub w: usize,
    pub h: usize,
    pub roi: BBox,
    pub scale: f64,
    pub labels: usize,
    pub rats: Vec<Rat>,
    pub png: Vec<u8>,
}

/// render the board into a PNG (bytes in `Out::png`)
pub fn render(m: &model::Model, o: &Opts) -> Result<Out, String> {
    let b = m.b;
    let roi = match o.roi {
        Some(r) => r,
        None => {
            let bb = if b.bbox.valid() {
                b.bbox
            } else {
                BBox::empty()
            };
            bb.inflate(1.0)
        }
    };
    if !(roi.x1 > roi.x0 && roi.y1 > roi.y0) {
        return Err("empty region".into());
    }
    let n = b.nets.len();
    let on: Vec<bool> = (0..n)
        .map(|i| match (o.hl_net, &o.nets) {
            (Some(h), _) => i == h,
            (None, Some(re)) => re.is_match(&b.nets[i]),
            _ => true,
        })
        .collect();
    // nets with copper in the region
    let mut present = vec![false; n];
    for f in &b.fps {
        for pd in &f.pads {
            if roi.contains(f.pad_world(pd).0) {
                present[pd.net] = true;
            }
        }
    }
    for t in &b.tracks {
        if roi.contains(t.a) || roi.contains(t.b) {
            present[t.net] = true;
        }
    }
    present[0] = false;
    let mut halo = vec![0.0; n];
    if o.halo_min > 0.0 {
        let ids: Vec<usize> = (0..n).filter(|&i| present[i]).collect();
        for &a in &ids {
            let mx = ids
                .iter()
                .filter(|&&bb| bb != a)
                .map(|&bb| m.r.clr(a, bb, true))
                .fold(0.0, f64::max);
            if mx >= o.halo_min {
                halo[a] = mx;
            }
        }
    }
    let rats: Vec<Rat> = if o.opens {
        let sel = |i: usize| i != 0 && o.hl_net.is_none_or(|h| h == i);
        ratsnest(m, &sel)
            .into_iter()
            .filter(|r| {
                let (a, e) = (p(r.a[0], r.a[1]), p(r.b[0], r.b[1]));
                roi.contains(a) || roi.contains(e) || {
                    let mut bb = BBox::empty();
                    bb.add(a);
                    bb.add(e);
                    bb.overlaps(&roi)
                }
            })
            .collect()
    } else {
        Vec::new()
    };
    let c = Ctx {
        b,
        o,
        on,
        halo,
        rats: &rats,
    };
    let hdr = 16usize;
    let wmm = roi.x1 - roi.x0;
    let hmm = roi.y1 - roi.y0;
    let layers: Vec<usize> = (0..NL)
        .filter(|l| o.layers & b.cu_mask & (1 << l) != 0)
        .collect();
    let (img, s, labels) = if o.per_layer && layers.len() > 1 {
        let k = layers.len();
        let cols = (k as f64).sqrt().ceil() as usize;
        let rows = k.div_ceil(cols);
        let gap = 6usize;
        let tw = (o.px.saturating_sub((cols - 1) * gap) / cols).max(50);
        let s = tw as f64 / wmm;
        let th = (hmm * s).round().max(20.0) as usize;
        let (w, h) = (
            o.px.max(tw * cols + gap * (cols - 1)),
            hdr + rows * (th + hdr + gap),
        );
        let mut img = Img {
            px: vec![[0, 0, 0]; w * h],
            w,
            h,
        };
        let tiles: Vec<(usize, Img, usize)> = layers
            .par_iter()
            .enumerate()
            .map(|(i, &l)| {
                let v = View {
                    x0: roi.x0,
                    y0: roi.y0,
                    s,
                    w: tw,
                    h: th,
                };
                let (t, nl) = tile(&c, v, 1 << l, o.labels);
                (i, t, nl)
            })
            .collect();
        let mut nl = 0;
        for (i, t, k) in tiles {
            nl += k;
            let (cx, cy) = ((i % cols) * (tw + gap), hdr + (i / cols) * (th + hdr + gap));
            img.text(
                CU[layers[i]],
                cx as i64 + 2,
                cy as i64 + 4,
                1,
                LAYER_RGB[layers[i]],
            );
            for y in 0..th {
                let dst = (cy + hdr + y) * w + cx;
                img.px[dst..dst + tw].copy_from_slice(&t.px[y * tw..(y + 1) * tw]);
            }
        }
        (img, s, nl)
    } else {
        let s = o.px as f64 / wmm;
        let th = (hmm * s).round().max(20.0) as usize;
        let v = View {
            x0: roi.x0,
            y0: roi.y0,
            s,
            w: o.px,
            h: th,
        };
        let lm = layers.iter().fold(0u8, |a, l| a | (1 << l));
        let (t, nl) = tile(&c, v, lm, o.labels);
        let (w, h) = (o.px, th + hdr);
        let mut img = Img {
            px: vec![[0, 0, 0]; w * h],
            w,
            h,
        };
        img.px[hdr * w..].copy_from_slice(&t.px);
        // legend
        let mut x = w as i64 - 4;
        for &l in layers.iter().rev() {
            let (tw, _) = font::text_size(CU[l], 1);
            x -= tw as i64 + 16;
            img.rect(x, 4, 8, 8, LAYER_RGB[l], 1.0);
            img.text(CU[l], x + 11, 4, 1, [220, 220, 220]);
        }
        (img, s, nl)
    };
    let mut img = img;
    let title = format!(
        "{}  x {:.2}..{:.2}  y {:.2}..{:.2} mm  {:.0} px/mm",
        o.title, roi.x0, roi.x1, roi.y0, roi.y1, s
    );
    img.text(&title, 4, 4, 1, [235, 235, 235]);
    // PNG
    let mut png = Vec::new();
    {
        let mut e = png::Encoder::new(&mut png, img.w as u32, img.h as u32);
        e.set_color(png::ColorType::Rgb);
        e.set_depth(png::BitDepth::Eight);
        e.set_compression(png::Compression::Fast);
        let mut wr = e.write_header().map_err(|e| e.to_string())?;
        let data: Vec<u8> = img.px.iter().flat_map(|c| c.iter().copied()).collect();
        wr.write_image_data(&data).map_err(|e| e.to_string())?;
    }
    Ok(Out {
        w: img.w,
        h: img.h,
        roi,
        scale: s,
        labels,
        rats,
        png,
    })
}

/// layer mask from a list like "F,In1,B" or "F.Cu,In2.Cu" ("all" or empty = every layer)
pub fn parse_layers(s: &str) -> Result<u8, String> {
    if s.is_empty() || s == "all" {
        return Ok(0x3f);
    }
    let mut m = 0u8;
    for t in s.split(',').map(str::trim).filter(|t| !t.is_empty()) {
        let name = if t.ends_with(".Cu") {
            t.to_string()
        } else {
            format!("{t}.Cu")
        };
        let l = layer_idx(&name).ok_or(format!("--layers: unknown copper layer '{t}'"))?;
        m |= 1 << l;
    }
    Ok(m)
}

/// JSON summary of a render
pub fn summary(o: &Out, out: &str) -> Value {
    let r2 = |x: f64| (x * 1000.0).round() / 1000.0;
    json!({
        "out": out, "width": o.w, "height": o.h,
        "roi": [r2(o.roi.x0), r2(o.roi.y0), r2(o.roi.x1), r2(o.roi.y1)],
        "px_per_mm": r2(o.scale), "labels": o.labels,
        "opens_in_roi": o.rats.len(),
        "ratsnest": o.rats.iter().take(200).collect::<Vec<_>>(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clip_keeps_inside_and_cuts_outside() {
        assert!(clip((-10.0, -10.0), (-5.0, -1.0), 100.0, 100.0).is_none());
        let (a, b) = clip((-10.0, 50.0), (200.0, 50.0), 100.0, 100.0).unwrap();
        assert!(a.0.abs() < 1e-9 && (b.0 - 99.0).abs() < 1e-9);
    }

    #[test]
    fn nice_steps() {
        assert_eq!(nice_step(10.0, 8.0), 2.0);
        assert_eq!(nice_step(80.0, 8.0), 10.0);
        assert!((nice_step(3.0, 8.0) - 0.5).abs() < 1e-12);
    }

    #[test]
    fn layers_parse() {
        assert_eq!(parse_layers("F,B").unwrap(), 1 | 32);
        assert_eq!(parse_layers("In1.Cu").unwrap(), 2);
        assert!(parse_layers("X").is_err());
    }
}
