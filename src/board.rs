//! Board model parsed from a .kicad_pcb (KiCad 10 text format, nets referenced by name).
//! Footprint geometry is kept in the footprint frame so the placer can move/rotate/flip parts.

use crate::geom::*;
use crate::sexpr::{self, List};
use std::collections::HashMap;

pub const CU: [&str; 6] = ["F.Cu", "In1.Cu", "In2.Cu", "In3.Cu", "In4.Cu", "B.Cu"];
pub const NL: usize = 6;

pub fn layer_idx(name: &str) -> Option<usize> {
    CU.iter().position(|l| *l == name)
}
pub fn is_outer(l: usize) -> bool {
    l == 0 || l == NL - 1
}

/// layer mask from a KiCad layer list ("F.Cu", "*.Cu", "F&B.Cu", ...)
pub fn layer_mask(names: &[String]) -> u8 {
    let mut m = 0u8;
    for n in names {
        match n.as_str() {
            "*.Cu" => m |= 0x3f,
            "F&B.Cu" => m |= 1 | (1 << 5),
            s => {
                if let Some(i) = layer_idx(s) {
                    m |= 1 << i
                }
            }
        }
    }
    m
}

#[derive(Debug, Clone)]
pub struct Pad {
    pub num: String,
    pub net: usize,
    pub kind: String, // smd, thru_hole, np_thru_hole, connect
    /// shapes in the pad frame (centre 0,0, pad orientation applied later)
    pub shapes: Vec<Shape>,
    /// position in the footprint frame (as stored), and pad angle relative to the footprint
    pub pos: P,
    pub rel_rot: f64,
    pub size: (f64, f64),
    pub layers: u8,
    pub drill: f64,
    /// local pad clearance override (0 = none)
    pub local_clr: f64,
}

#[derive(Debug, Clone)]
pub struct Footprint {
    pub refd: String,
    pub value: String,
    pub lib: String,
    pub at: P,
    pub rot: f64,
    pub back: bool,
    pub locked: bool,
    pub attr: Vec<String>,
    pub sheetname: String,
    pub sheetfile: String,
    pub path: String,
    pub board: String,
    pub pads: Vec<Pad>,
    /// courtyard polygon in the footprint frame (as stored, i.e. already mirrored for B)
    pub crtyd: Vec<P>,
    pub span: (usize, usize),
}

impl Footprint {
    pub fn is_smd(&self) -> bool {
        self.attr.iter().any(|a| a == "smd")
    }
    pub fn is_tht(&self) -> bool {
        self.attr.iter().any(|a| a == "through_hole")
    }
    pub fn pad_world(&self, pd: &Pad) -> (P, f64) {
        (self.at + pd.pos.rot(self.rot), pd.rel_rot + self.rot)
    }
    /// pad copper shapes in world coordinates
    pub fn pad_shapes(&self, pd: &Pad) -> Vec<Shape> {
        let (c, a) = self.pad_world(pd);
        pd.shapes.iter().map(|s| s.xform(c, a)).collect()
    }
    pub fn crtyd_world(&self) -> Vec<P> {
        self.crtyd
            .iter()
            .map(|q| self.at + q.rot(self.rot))
            .collect()
    }
    /// same footprint placed at a new pose (side flip mirrors the local geometry and swaps copper layers)
    pub fn moved(&self, at: P, rot: f64, back: bool) -> Footprint {
        let mut f = self.clone();
        f.at = at;
        f.rot = rot;
        if back != self.back {
            f.back = back;
            f.crtyd = f.crtyd.iter().rev().map(|q| p(q.x, -q.y)).collect();
            for pd in &mut f.pads {
                pd.pos = p(pd.pos.x, -pd.pos.y);
                pd.rel_rot = -pd.rel_rot;
                pd.shapes = pd.shapes.iter().map(|s| s.mirror_y()).collect();
                let mut m = 0u8;
                for l in 0..NL {
                    if pd.layers & (1 << l) != 0 {
                        m |= 1 << (NL - 1 - l);
                    }
                }
                pd.layers = m;
            }
        }
        f
    }
}

#[derive(Debug, Clone)]
pub struct Track {
    pub a: P,
    pub b: P,
    pub w: f64,
    pub layer: usize,
    pub net: usize,
    pub locked: bool,
    pub uuid: String,
}

#[derive(Debug, Clone)]
pub struct Via {
    pub at: P,
    pub d: f64,
    pub drill: f64,
    pub net: usize,
    pub locked: bool,
    pub uuid: String,
}

#[derive(Debug, Clone)]
pub struct Zone {
    pub net: usize,
    pub name: String,
    pub layers: u8,
    pub outline: Vec<P>,
    pub locked: bool,
    pub keepout: bool,
    pub ko_tracks: bool,
    pub ko_vias: bool,
    pub ko_pour: bool,
    pub ko_footprints: bool,
    pub priority: i64,
    /// owning footprint (rule areas inside footprints)
    pub fp: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct Board {
    pub text: String,
    pub nets: Vec<String>,
    pub net_id: HashMap<String, usize>,
    pub fps: Vec<Footprint>,
    pub tracks: Vec<Track>,
    pub vias: Vec<Via>,
    pub zones: Vec<Zone>,
    pub edge: Vec<Vec<P>>, // closed outline polygons
    pub edge_segs: Vec<(P, P)>,
    pub bbox: BBox,
    /// copper layers present in the board's layer stack (bit i = CU[i]); 2-, 4- and 6-layer boards
    pub cu_mask: u8,
}

impl Board {
    pub fn net(&mut self, name: &str) -> usize {
        if let Some(&i) = self.net_id.get(name) {
            return i;
        }
        let i = self.nets.len();
        self.nets.push(name.to_string());
        self.net_id.insert(name.to_string(), i);
        i
    }
    pub fn fp(&self, r: &str) -> Option<&Footprint> {
        self.fps.iter().find(|f| f.refd == r)
    }
    pub fn fp_idx(&self, r: &str) -> Option<usize> {
        self.fps.iter().position(|f| f.refd == r)
    }
}

fn pts_of(l: &List) -> Vec<P> {
    l.child("pts")
        .map(|pts| {
            pts.children("xy")
                .filter_map(|xy| Some(p(xy.argf(0)?, xy.argf(1)?)))
                .collect()
        })
        .unwrap_or_default()
}

fn parse_pad(l: &List, b: &mut Board, fp_rot: f64) -> Pad {
    let num = l.arg(0).unwrap_or("").to_string();
    let kind = l.arg(1).unwrap_or("").to_string();
    let shape = l.arg(2).unwrap_or("").to_string();
    let (x, y, a) = l.xy("at").unwrap_or((0.0, 0.0, 0.0));
    let (w, h, _) = l.xy("size").unwrap_or((0.0, 0.0, 0.0));
    let net = l.val("net").map(|n| b.net(n)).unwrap_or(0);
    let layers = layer_mask(&l.atoms("layers"));
    let drill = l
        .child("drill")
        .and_then(|d| {
            d.items
                .iter()
                .skip(1)
                .filter_map(|n| n.atom())
                .find_map(|s| s.parse::<f64>().ok())
        })
        .unwrap_or(0.0);
    let local_clr = l
        .val("clearance")
        .and_then(|v| v.parse::<f64>().ok())
        .unwrap_or(0.0);
    let o = p(0.0, 0.0);
    let mut shapes = Vec::new();
    match shape.as_str() {
        "circle" => shapes.push(Shape::circle(o, w / 2.0)),
        "oval" => shapes.push(oval_shape(o, w, h, 0.0)),
        "roundrect" => {
            let rr = l.valf("roundrect_rratio").unwrap_or(0.25) * w.min(h);
            shapes.push(rect_shape(o, w, h, 0.0, rr))
        }
        "custom" => {
            let anchor = l
                .child("options")
                .and_then(|op| op.val("anchor"))
                .unwrap_or("rect");
            if anchor == "circle" {
                shapes.push(Shape::circle(o, w / 2.0));
            } else {
                shapes.push(rect_shape(o, w, h, 0.0, 0.0));
            }
            if let Some(pr) = l.child("primitives") {
                for g in pr.lists() {
                    let gw = g.valf("width").unwrap_or(0.0) / 2.0;
                    match g.head() {
                        "gr_poly" => shapes.push(Shape::Poly {
                            pts: pts_of(g),
                            r: gw,
                        }),
                        "gr_line" => {
                            if let (Some(s), Some(e)) = (g.xy("start"), g.xy("end")) {
                                shapes.push(Shape::Capsule {
                                    a: p(s.0, s.1),
                                    b: p(e.0, e.1),
                                    r: gw.max(0.0),
                                })
                            }
                        }
                        "gr_circle" => {
                            if let (Some(c), Some(e)) = (g.xy("center"), g.xy("end")) {
                                let r = ((e.0 - c.0).powi(2) + (e.1 - c.1).powi(2)).sqrt();
                                shapes.push(Shape::circle(p(c.0, c.1), r + gw))
                            }
                        }
                        "gr_rect" => {
                            if let (Some(s), Some(e)) = (g.xy("start"), g.xy("end")) {
                                shapes.push(Shape::Poly {
                                    pts: vec![p(s.0, s.1), p(e.0, s.1), p(e.0, e.1), p(s.0, e.1)],
                                    r: gw,
                                })
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
        "trapezoid" => shapes.push(rect_shape(o, w, h, 0.0, 0.0)),
        _ => shapes.push(rect_shape(o, w, h, 0.0, 0.0)),
    }
    if kind == "np_thru_hole" && shapes.is_empty() {
        shapes.push(Shape::circle(o, drill / 2.0));
    }
    Pad {
        num,
        net,
        kind,
        shapes,
        pos: p(x, y),
        rel_rot: a - fp_rot,
        size: (w, h),
        layers,
        drill,
        local_clr,
    }
}

fn parse_zone(z: &List, b: &mut Board, fp: Option<&str>, fp_at: Option<(P, f64)>) -> Zone {
    let net = z.val("net").map(|n| b.net(n)).unwrap_or(0);
    let mut ls = z.atoms("layers");
    if ls.is_empty() {
        ls = z.atoms("layer");
    }
    let mut outline: Vec<P> = z.child("polygon").map(pts_of).unwrap_or_default();
    if let Some((at, r)) = fp_at {
        // footprint zones are stored in board coordinates in KiCad >= 6; keep as is
        let _ = (at, r);
    }
    if outline.is_empty() {
        outline = Vec::new();
    }
    let ko = z.child("keepout");
    let na = |k: &str| {
        ko.and_then(|c| c.val(k))
            .map(|v| v == "not_allowed")
            .unwrap_or(false)
    };
    Zone {
        net,
        name: z.val("name").unwrap_or("").to_string(),
        layers: layer_mask(&ls),
        outline,
        locked: z.flag("locked"),
        keepout: ko.is_some(),
        ko_tracks: na("tracks"),
        ko_vias: na("vias"),
        ko_pour: na("copperpour"),
        ko_footprints: na("footprints"),
        priority: z.valf("priority").unwrap_or(0.0) as i64,
        fp: fp.map(|s| s.to_string()),
    }
}

/// courtyard outline from fp_rect / fp_poly / fp_line / fp_circle on *.CrtYd (footprint frame)
fn courtyard(f: &List) -> Vec<P> {
    let mut polys: Vec<Vec<P>> = Vec::new();
    let mut lines: Vec<(P, P)> = Vec::new();
    for g in f.lists() {
        let lay = g.val("layer").unwrap_or("");
        if !lay.ends_with("CrtYd") {
            continue;
        }
        match g.head() {
            "fp_rect" => {
                if let (Some(s), Some(e)) = (g.xy("start"), g.xy("end")) {
                    polys.push(vec![p(s.0, s.1), p(e.0, s.1), p(e.0, e.1), p(s.0, e.1)]);
                }
            }
            "fp_poly" => polys.push(pts_of(g)),
            "fp_line" => {
                if let (Some(s), Some(e)) = (g.xy("start"), g.xy("end")) {
                    lines.push((p(s.0, s.1), p(e.0, e.1)));
                }
            }
            "fp_arc" => {
                if let (Some(s), Some(m), Some(e)) = (g.xy("start"), g.xy("mid"), g.xy("end")) {
                    lines.push((p(s.0, s.1), p(m.0, m.1)));
                    lines.push((p(m.0, m.1), p(e.0, e.1)));
                }
            }
            "fp_circle" => {
                if let (Some(c), Some(e)) = (g.xy("center"), g.xy("end")) {
                    let r = ((e.0 - c.0).powi(2) + (e.1 - c.1).powi(2)).sqrt();
                    polys.push(
                        (0..16)
                            .map(|k| {
                                let t = k as f64 * std::f64::consts::PI / 8.0;
                                p(c.0 + r * t.cos(), c.1 + r * t.sin())
                            })
                            .collect(),
                    );
                }
            }
            _ => {}
        }
    }
    if polys.len() == 1 && lines.is_empty() {
        return polys.pop().unwrap();
    }
    if !lines.is_empty() && polys.is_empty() {
        if let Some(ch) = chain(&lines) {
            return ch;
        }
    }
    let mut all: Vec<P> = polys.into_iter().flatten().collect();
    for (a, b) in lines {
        all.push(a);
        all.push(b);
    }
    hull(all)
}

/// chain line segments into one closed polygon if they form exactly one loop
fn chain(lines: &[(P, P)]) -> Option<Vec<P>> {
    let close = |a: P, b: P| (a - b).len() < 1e-4;
    let mut used = vec![false; lines.len()];
    let mut poly = vec![lines[0].0, lines[0].1];
    used[0] = true;
    loop {
        let last = *poly.last().unwrap();
        let mut found = false;
        for (i, (a, b)) in lines.iter().enumerate() {
            if used[i] {
                continue;
            }
            if close(*a, last) {
                poly.push(*b);
            } else if close(*b, last) {
                poly.push(*a);
            } else {
                continue;
            }
            used[i] = true;
            found = true;
            break;
        }
        if !found {
            break;
        }
    }
    if used.iter().all(|u| *u) && close(poly[0], *poly.last().unwrap()) {
        poly.pop();
        Some(poly)
    } else {
        None
    }
}

pub fn parse_board(text: String) -> Result<Board, String> {
    let root = sexpr::parse(&text)?;
    let mut b = Board::default();
    b.net(""); // net 0 = no net
    for it in root.lists() {
        match it.head() {
            "layers" => {
                // (layers (0 "F.Cu" signal) (4 "In1.Cu" signal) ... (2 "B.Cu" signal) ...)
                for l in it.lists() {
                    if let Some(i) = l.arg(0).and_then(layer_idx) {
                        b.cu_mask |= 1 << i;
                    }
                }
            }
            "footprint" => {
                let (x, y, r) = it.xy("at").unwrap_or((0.0, 0.0, 0.0));
                let mut refd = String::new();
                let mut value = String::new();
                let mut board = String::new();
                for pr in it.children("property") {
                    match pr.arg(0) {
                        Some("Reference") => refd = pr.arg(1).unwrap_or("").to_string(),
                        Some("Value") => value = pr.arg(1).unwrap_or("").to_string(),
                        Some("Board") => board = pr.arg(1).unwrap_or("").to_string(),
                        _ => {}
                    }
                }
                let pads: Vec<Pad> = it
                    .children("pad")
                    .map(|pd| parse_pad(pd, &mut b, r))
                    .collect();
                let fpz: Vec<Zone> = it
                    .children("zone")
                    .map(|z| parse_zone(z, &mut b, Some(&refd), Some((p(x, y), r))))
                    .collect();
                b.zones.extend(fpz);
                b.fps.push(Footprint {
                    refd,
                    value,
                    lib: it.arg(0).unwrap_or("").to_string(),
                    at: p(x, y),
                    rot: r,
                    back: it.val("layer") == Some("B.Cu"),
                    locked: it.flag("locked"),
                    attr: it.atoms("attr"),
                    sheetname: it.val("sheetname").unwrap_or("").to_string(),
                    sheetfile: it.val("sheetfile").unwrap_or("").to_string(),
                    path: it.val("path").unwrap_or("").to_string(),
                    board,
                    pads,
                    crtyd: courtyard(it),
                    span: it.span,
                });
            }
            "segment" | "arc" => {
                let (Some(s), Some(e)) = (it.xy("start"), it.xy("end")) else {
                    continue;
                };
                let Some(layer) = it.val("layer").and_then(layer_idx) else {
                    continue;
                };
                let net = it.val("net").map(|n| b.net(n)).unwrap_or(0);
                let w = it.valf("width").unwrap_or(0.2);
                let locked = it.flag("locked");
                let uuid = it.val("uuid").unwrap_or("").to_string();
                if it.head() == "arc" {
                    if let Some(m) = it.xy("mid") {
                        b.tracks.push(Track {
                            a: p(s.0, s.1),
                            b: p(m.0, m.1),
                            w,
                            layer,
                            net,
                            locked,
                            uuid: uuid.clone(),
                        });
                        b.tracks.push(Track {
                            a: p(m.0, m.1),
                            b: p(e.0, e.1),
                            w,
                            layer,
                            net,
                            locked,
                            uuid,
                        });
                        continue;
                    }
                }
                b.tracks.push(Track {
                    a: p(s.0, s.1),
                    b: p(e.0, e.1),
                    w,
                    layer,
                    net,
                    locked,
                    uuid,
                });
            }
            "via" => {
                let Some(a) = it.xy("at") else { continue };
                let net = it.val("net").map(|n| b.net(n)).unwrap_or(0);
                b.vias.push(Via {
                    at: p(a.0, a.1),
                    d: it.valf("size").unwrap_or(0.6),
                    drill: it.valf("drill").unwrap_or(0.3),
                    net,
                    locked: it.flag("locked"),
                    uuid: it.val("uuid").unwrap_or("").to_string(),
                });
            }
            "zone" => {
                let z = parse_zone(it, &mut b, None, None);
                b.zones.push(z);
            }
            "gr_line" | "gr_rect" | "gr_poly" | "gr_arc" | "gr_circle" => {
                if it.val("layer") != Some("Edge.Cuts") {
                    continue;
                }
                match it.head() {
                    "gr_rect" => {
                        let (s, e) = (it.xy("start").unwrap(), it.xy("end").unwrap());
                        b.edge
                            .push(vec![p(s.0, s.1), p(e.0, s.1), p(e.0, e.1), p(s.0, e.1)]);
                    }
                    "gr_poly" => b.edge.push(pts_of(it)),
                    "gr_line" => {
                        let (s, e) = (it.xy("start").unwrap(), it.xy("end").unwrap());
                        b.edge_segs.push((p(s.0, s.1), p(e.0, e.1)));
                    }
                    "gr_arc" => {
                        let (s, m, e) = (
                            it.xy("start").unwrap(),
                            it.xy("mid").unwrap(),
                            it.xy("end").unwrap(),
                        );
                        b.edge_segs.push((p(s.0, s.1), p(m.0, m.1)));
                        b.edge_segs.push((p(m.0, m.1), p(e.0, e.1)));
                    }
                    "gr_circle" => {
                        let (c, e) = (it.xy("center").unwrap(), it.xy("end").unwrap());
                        let r = ((e.0 - c.0).powi(2) + (e.1 - c.1).powi(2)).sqrt();
                        b.edge.push(
                            (0..32)
                                .map(|k| {
                                    let t = k as f64 * std::f64::consts::PI / 16.0;
                                    p(c.0 + r * t.cos(), c.1 + r * t.sin())
                                })
                                .collect(),
                        );
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }
    if !b.edge_segs.is_empty() {
        if let Some(ch) = chain(&b.edge_segs) {
            b.edge.push(ch);
        }
    }
    let mut bb = BBox::empty();
    for poly in &b.edge {
        for q in poly {
            bb.add(*q);
        }
    }
    for (a, c) in &b.edge_segs {
        bb.add(*a);
        bb.add(*c);
    }
    b.bbox = bb;
    if b.cu_mask == 0 {
        b.cu_mask = 0x3f; // no layer table: assume all six copper layers
    }
    // keep edge segments of polygons for clearance stamping
    for poly in b.edge.clone() {
        let n = poly.len();
        for i in 0..n {
            b.edge_segs.push((poly[i], poly[(i + 1) % n]));
        }
    }
    b.text = text;
    Ok(b)
}

pub fn load(path: &str) -> Result<Board, String> {
    let t = std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?;
    parse_board(t)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn demo_board_layers_pads_and_rule_area() {
        let b = load(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/examples/demo/demo.kicad_pcb"
        ))
        .unwrap();
        // 4-layer stack: F.Cu, In1.Cu, In2.Cu, B.Cu
        assert_eq!(b.cu_mask, 0b100111);
        assert_eq!(b.fps.len(), 25);
        assert!(b.nets.len() >= 21);
        let u1 = b.fp("U1").unwrap();
        assert_eq!(u1.pads.len(), 14);
        let p1 = u1.pads.iter().find(|p| p.num == "1").unwrap();
        assert_eq!(b.nets[p1.net], "+3V3");
        assert_eq!(p1.layers, 1); // SMD pad on F.Cu
        let ra: Vec<&Zone> = b.zones.iter().filter(|z| z.keepout).collect();
        assert_eq!(ra.len(), 1);
        assert!(ra[0].ko_tracks && ra[0].ko_vias && !ra[0].ko_footprints);
        assert_eq!(ra[0].layers, 0b100111);
        assert!(b.bbox.x1 - b.bbox.x0 > 69.9 && b.bbox.y1 - b.bbox.y0 > 49.9);
    }

    #[test]
    fn layer_masks() {
        let m = layer_mask(&["F.Cu".into(), "In2.Cu".into()]);
        assert_eq!(m, 0b101);
        assert_eq!(layer_mask(&["*.Cu".into()]), 0x3f);
        assert_eq!(layer_mask(&["F&B.Cu".into()]), 0b100001);
    }
}
