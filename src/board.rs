//! Board model parsed from a .kicad_pcb (KiCad 6+ text format; numbered or named net references).
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

/// solder mask layers ("F.Mask", "*.Mask", ...) as the mask of the copper layer they expose
/// (F.Mask -> F.Cu, B.Mask -> B.Cu)
pub fn mask_layers(names: &[String]) -> u8 {
    let mut m = 0u8;
    for n in names {
        match n.as_str() {
            "*.Mask" | "F&B.Mask" => m |= 1 | (1 << (NL - 1)),
            "F.Mask" => m |= 1,
            "B.Mask" => m |= 1 << (NL - 1),
            _ => {}
        }
    }
    m
}

/// swap the copper layer bits of a mask front to back (side flip)
fn flip_layers(lm: u8) -> u8 {
    let mut m = 0u8;
    for l in 0..NL {
        if lm & (1 << l) != 0 {
            m |= 1 << (NL - 1 - l);
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
    /// plated hole with `remove_unused_layers`: the pad shape only on the outer layers, only the
    /// barrel on the inner layers
    pub inner_barrel_only: bool,
    /// outer copper layers whose solder mask is opened over the pad (bit 0 = F.Mask, bit 5 = B.Mask)
    pub mask: u8,
    /// local solder mask expansion (pad, else footprint); None = board setup value
    pub mask_margin: Option<f64>,
    /// zone connection override (`zone_connect`: 0 none, 1 thermal, 2 solid, 3 THT thermal),
    /// thermal gap and spoke width overrides (pad, else footprint)
    pub zone_connect: Option<u8>,
    pub thermal_gap: Option<f64>,
    pub thermal_bridge: Option<f64>,
    /// thermal spoke angle in degrees (KiCad: 45 for circle pads, 90 otherwise)
    pub spoke_angle: f64,
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
    /// the separate courtyard polygons when there are several (footprint frame; `crtyd` is
    /// then their hull); empty for a single outline
    pub crtyd_parts: Vec<Vec<P>>,
    /// solder mask graphics (fp_* on F.Mask / B.Mask) in the footprint frame, as (shape, mask of
    /// the copper layer they expose)
    pub mask: Vec<(Shape, u8)>,
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
    /// solder mask graphics in world coordinates
    pub fn mask_world(&self) -> Vec<(Shape, u8)> {
        self.mask
            .iter()
            .map(|(s, lm)| (s.xform(self.at, self.rot), *lm))
            .collect()
    }
    /// every courtyard polygon in world coordinates (the separate ones, else the single outline)
    pub fn crtyd_parts_world(&self) -> Vec<Vec<P>> {
        if self.crtyd_parts.is_empty() {
            let c = self.crtyd_world();
            return if c.len() >= 3 { vec![c] } else { vec![] };
        }
        self.crtyd_parts
            .iter()
            .map(|pl| pl.iter().map(|q| self.at + q.rot(self.rot)).collect())
            .collect()
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
            f.crtyd_parts = f
                .crtyd_parts
                .iter()
                .map(|pl| pl.iter().rev().map(|q| p(q.x, -q.y)).collect())
                .collect();
            for pd in &mut f.pads {
                pd.pos = p(pd.pos.x, -pd.pos.y);
                pd.rel_rot = -pd.rel_rot;
                pd.shapes = pd.shapes.iter().map(|s| s.mirror_y()).collect();
                pd.layers = flip_layers(pd.layers);
                pd.mask = flip_layers(pd.mask);
            }
            f.mask = f
                .mask
                .iter()
                .map(|(s, lm)| (s.mirror_y(), flip_layers(*lm)))
                .collect();
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
    /// pad connection: 0 none, 1 thermal relief (KiCad default), 2 solid, 3 thermal for
    /// through-hole pads only (SMD solid)
    pub connect: u8,
    /// zone clearance (`connect_pads (clearance X)`), mm
    pub clearance: f64,
    /// minimum copper width of the fill, mm
    pub min_thickness: f64,
    /// thermal relief gap and spoke width, mm
    pub thermal_gap: f64,
    pub thermal_bridge: f64,
    /// owning footprint (rule areas inside footprints)
    pub fp: Option<String>,
    /// saved zone fill (`filled_polygon`): (copper layer index, polygon); empty if never filled
    pub fills: Vec<(usize, Vec<P>)>,
    /// spokes per thermal-relief pad of a native fill: (layer, footprint, pad, spokes kept);
    /// empty for saved fills
    pub spokes: Vec<(usize, usize, usize, u8)>,
}

impl Zone {
    /// name for zone selection (`--zones` / `--plane` regexes): the zone name, or for an unnamed
    /// zone `NET@LAYER[+LAYER...]` (e.g. `GND@In1.Cu`; boards from KiCad 5 have no zone names)
    pub fn key(&self, nets: &[String]) -> String {
        if !self.name.is_empty() {
            return self.name.clone();
        }
        let ls: Vec<&str> = (0..CU.len())
            .filter(|l| self.layers & (1 << l) != 0)
            .map(|l| CU[l])
            .collect();
        format!(
            "{}@{}",
            nets.get(self.net).map_or("", |s| s.as_str()),
            ls.join("+")
        )
    }
}

#[derive(Debug, Clone, Default)]
pub struct Board {
    pub text: String,
    pub nets: Vec<String>,
    pub net_id: HashMap<String, usize>,
    pub net_codes: HashMap<String, String>,
    pub fps: Vec<Footprint>,
    pub tracks: Vec<Track>,
    pub vias: Vec<Via>,
    pub zones: Vec<Zone>,
    pub edge: Vec<Vec<P>>, // closed outline polygons
    pub edge_segs: Vec<(P, P)>,
    /// copper graphics and texts without a net (board and footprint), as (shape, layer mask)
    pub graphics: Vec<(Shape, u8)>,
    pub bbox: BBox,
    /// copper layers present in the board's layer stack (bit i = CU[i]); 2-, 4- and 6-layer boards
    pub cu_mask: u8,
    /// board-level solder mask graphics (gr_* on F.Mask / B.Mask), as (shape, exposed copper layer)
    pub mask: Vec<(Shape, u8)>,
    /// board setup solder mask expansion of pads (`pad_to_mask_clearance`)
    pub mask_margin: f64,
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
    fn net_reference(&mut self, token: &str) -> usize {
        let name = self
            .net_codes
            .get(token)
            .cloned()
            .unwrap_or_else(|| token.to_owned());
        self.net(&name)
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

fn parse_pad(l: &List, b: &mut Board, fp_rot: f64, fp_margin: Option<f64>) -> Pad {
    let num = l.arg(0).unwrap_or("").to_string();
    let kind = l.arg(1).unwrap_or("").to_string();
    let shape = l.arg(2).unwrap_or("").to_string();
    let (x, y, a) = l.xy("at").unwrap_or((0.0, 0.0, 0.0));
    let (w, h, _) = l.xy("size").unwrap_or((0.0, 0.0, 0.0));
    let net = l.val("net").map(|n| b.net_reference(n)).unwrap_or(0);
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
    // (drill d (offset x y)): the copper shape is shifted against the hole (pad frame)
    if let Some(off) = l.child("drill").and_then(|d| d.xy("offset")) {
        if off.0 != 0.0 || off.1 != 0.0 {
            shapes = shapes
                .iter()
                .map(|s| s.xform(p(off.0, off.1), 0.0))
                .collect();
        }
    }
    let inner_barrel_only = kind == "thru_hole" && l.flag("remove_unused_layers");
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
        inner_barrel_only,
        mask: mask_layers(&l.atoms("layers")),
        mask_margin: l.valf("solder_mask_margin").or(fp_margin),
        zone_connect: l.valf("zone_connect").map(|v| v as u8),
        thermal_gap: l.valf("thermal_gap"),
        thermal_bridge: l.valf("thermal_bridge_width").or(l.valf("thermal_width")),
        spoke_angle: l
            .valf("thermal_bridge_angle")
            .unwrap_or(if shape == "circle" { 45.0 } else { 90.0 }),
    }
}

/// outline points of an arc given by start / mid / end (or the legacy centre / start / angle form)
fn arc_pts(g: &List) -> Option<Vec<P>> {
    let (s, e) = (g.xy("start")?, g.xy("end")?);
    if let Some(m) = g.xy("mid") {
        return Some(vec![p(s.0, s.1), p(m.0, m.1), p(e.0, e.1)]);
    }
    // legacy form (KiCad 5): start = centre, end = arc start, angle in degrees
    let ang = g.valf("angle")?;
    let (r0, a0) = ((e.0 - s.0).hypot(e.1 - s.1), (e.1 - s.1).atan2(e.0 - s.0));
    let n = ((ang.abs() / 15.0).ceil() as usize).max(2);
    Some(
        (0..=n)
            .map(|k| {
                let t = a0 + ang.to_radians() * k as f64 / n as f64;
                p(s.0 + r0 * t.cos(), s.1 + r0 * t.sin())
            })
            .collect(),
    )
}

/// polyline as capsules of half width r
fn chain_caps(pts: &[P], r: f64, closed: bool, out: &mut Vec<Shape>) {
    let n = pts.len();
    let m = if closed && n > 2 {
        n
    } else {
        n.saturating_sub(1)
    };
    for i in 0..m {
        out.push(Shape::Capsule {
            a: pts[i],
            b: pts[(i + 1) % n],
            r,
        });
    }
}

/// shapes (in the item's own frame) of a graphic item: gr_*/fp_* line, arc, rect, poly, circle,
/// text (approximate bounding box). Unfilled closed shapes become their outline.
fn graphic_shapes(g: &List) -> Vec<Shape> {
    let kind = g.head().split_once('_').map(|(_, k)| k).unwrap_or("");
    let r = g
        .valf("width")
        .or_else(|| g.child("stroke").and_then(|st| st.valf("width")))
        .unwrap_or(0.0)
        .max(0.0)
        / 2.0;
    let filled = match g.val("fill") {
        Some(v) => v != "no" && v != "none",
        None => kind == "poly" && g.child("stroke").is_none(), // KiCad 5 polygons are filled
    };
    let mut out = Vec::new();
    match kind {
        "line" => {
            if let (Some(a), Some(b)) = (g.xy("start"), g.xy("end")) {
                chain_caps(&[p(a.0, a.1), p(b.0, b.1)], r, false, &mut out);
            }
        }
        "arc" => {
            if let Some(pts) = arc_pts(g) {
                chain_caps(&pts, r, false, &mut out);
            }
        }
        "rect" | "poly" => {
            let pts = if kind == "rect" {
                match (g.xy("start"), g.xy("end")) {
                    (Some(a), Some(b)) => vec![p(a.0, a.1), p(b.0, a.1), p(b.0, b.1), p(a.0, b.1)],
                    _ => Vec::new(),
                }
            } else {
                pts_of(g)
            };
            if filled && pts.len() > 2 {
                out.push(Shape::Poly { pts, r });
            } else {
                chain_caps(&pts, r, true, &mut out);
            }
        }
        "circle" => {
            if let (Some(c), Some(e)) = (g.xy("center"), g.xy("end")) {
                let (c, rad) = (p(c.0, c.1), (e.0 - c.0).hypot(e.1 - c.1));
                if filled {
                    out.push(Shape::circle(c, rad + r));
                } else {
                    let pts: Vec<P> = (0..32)
                        .map(|k| {
                            let t = k as f64 * std::f64::consts::PI / 16.0;
                            p(c.x + rad * t.cos(), c.y + rad * t.sin())
                        })
                        .collect();
                    chain_caps(&pts, r, true, &mut out);
                }
            }
        }
        "text" => {
            // bounding box estimate (generous: 1.1 x glyph width per character), centred unless justified
            let txt = if g.head() == "fp_text" {
                g.arg(1)
            } else {
                g.arg(0)
            }
            .unwrap_or("");
            let Some((x, y, a)) = g.xy("at") else {
                return out;
            };
            let font = g.child("effects").and_then(|e| e.child("font"));
            let (h, w) = font
                .and_then(|f| f.xy("size"))
                .map(|s| (s.0, s.1))
                .unwrap_or((1.0, 1.0));
            let t = font.and_then(|f| f.valf("thickness")).unwrap_or(0.15);
            let lines: Vec<&str> = txt.split('\n').collect(); // the parser already unescaped \n
            let len = lines.iter().map(|l| l.chars().count()).max().unwrap_or(0) as f64;
            let (bw, bh) = (len * w * 1.1 + t, lines.len() as f64 * h * 1.6 + t);
            let just = g
                .child("effects")
                .map(|e| e.atoms("justify"))
                .unwrap_or_default();
            let x0 = if just.iter().any(|j| j == "left") {
                0.0
            } else if just.iter().any(|j| j == "right") {
                -bw
            } else {
                -bw / 2.0
            };
            let pts = [
                (x0, -bh / 2.0),
                (x0 + bw, -bh / 2.0),
                (x0 + bw, bh / 2.0),
                (x0, bh / 2.0),
            ]
            .iter()
            .map(|&(dx, dy)| p(x, y) + p(dx, dy).rot(a))
            .collect();
            out.push(Shape::Poly { pts, r: 0.0 });
        }
        _ => {}
    }
    out
}

/// exposed copper layer of a solder mask graphic item (0 if it is not on a mask layer)
fn graphic_mask_layers(g: &List) -> u8 {
    g.val("layer")
        .map(|l| mask_layers(&[l.to_string()]))
        .unwrap_or(0)
}

/// copper layer mask of a graphic item (0 if it is not on copper)
fn graphic_layers(g: &List) -> u8 {
    g.val("layer")
        .and_then(layer_idx)
        .map(|l| 1u8 << l)
        .unwrap_or(0)
}

fn parse_zone(z: &List, b: &mut Board, fp: Option<&str>, fp_at: Option<(P, f64)>) -> Zone {
    let net = z.val("net").map(|n| b.net_reference(n)).unwrap_or(0);
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
        connect: match z.child("connect_pads").and_then(|c| c.arg(0)) {
            Some("no") => 0,
            Some("yes") => 2,
            Some("thru_hole_only") => 3,
            _ => 1,
        },
        clearance: z
            .child("connect_pads")
            .and_then(|c| c.valf("clearance"))
            .unwrap_or(0.0),
        min_thickness: z.valf("min_thickness").unwrap_or(0.25),
        thermal_gap: z
            .child("fill")
            .and_then(|f| f.valf("thermal_gap"))
            .unwrap_or(0.5),
        thermal_bridge: z
            .child("fill")
            .and_then(|f| f.valf("thermal_bridge_width"))
            .unwrap_or(0.5),
        fp: fp.map(|s| s.to_string()),
        spokes: Vec::new(),
        fills: z
            .children("filled_polygon")
            .filter_map(|fp| {
                let l = fp.val("layer").and_then(layer_idx)?;
                let pts = pts_of(fp);
                (pts.len() >= 3).then_some((l, pts))
            })
            .collect(),
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
        let mut lp = chain(&lines);
        // exactly one loop that uses every line
        if lp.len() == 1 && lp[0].len() == lines.len() {
            return lp.pop().unwrap();
        }
    }
    let mut all: Vec<P> = polys.into_iter().flatten().collect();
    for (a, b) in lines {
        all.push(a);
        all.push(b);
    }
    hull(all)
}

/// the separate closed courtyard polygons of a footprint when it has more than one (rects,
/// polys, circles and closed line loops); empty otherwise
fn courtyard_parts(f: &List) -> Vec<Vec<P>> {
    let mut polys: Vec<Vec<P>> = Vec::new();
    let mut lines: Vec<(P, P)> = Vec::new();
    for g in f.lists() {
        if !g.val("layer").unwrap_or("").ends_with("CrtYd") {
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
    polys.retain(|pl| pl.len() >= 3);
    polys.extend(chain(&lines).into_iter().filter(|pl| pl.len() >= 3));
    if polys.len() < 2 {
        return Vec::new();
    }
    polys
}

/// chain line segments into closed loops (several board outlines in one file, cut-outs);
/// segments that close no loop are left out
fn chain(lines: &[(P, P)]) -> Vec<Vec<P>> {
    let close = |a: P, b: P| (a - b).len() < 1e-4;
    let mut used = vec![false; lines.len()];
    let mut loops = Vec::new();
    for s0 in 0..lines.len() {
        if used[s0] {
            continue;
        }
        let mut idx = vec![s0];
        used[s0] = true;
        let mut poly = vec![lines[s0].0, lines[s0].1];
        loop {
            if poly.len() > 2 && close(poly[0], *poly.last().unwrap()) {
                break;
            }
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
                idx.push(i);
                found = true;
                break;
            }
            if !found {
                break;
            }
        }
        if poly.len() > 3 && close(poly[0], *poly.last().unwrap()) {
            poly.pop();
            loops.push(poly);
        } else {
            // open chain: release its segments (they stay plain edge segments)
            for i in idx.into_iter().skip(1) {
                used[i] = false;
            }
        }
    }
    loops
}

impl Board {
    /// inside the board: inside an odd number of closed Edge.Cuts outlines (several outlines
    /// side by side = a panel, an outline inside another = a cut-out); no outline = everywhere
    pub fn inside(&self, q: P) -> bool {
        self.edge.is_empty() || self.edge.iter().filter(|o| point_in_poly(q, o)).count() % 2 == 1
    }
    /// indices of the outer board outlines (closed outlines not inside another one), in file order
    pub fn outer_outlines(&self) -> Vec<usize> {
        (0..self.edge.len())
            .filter(|&i| {
                let q = self.edge[i][0];
                !(0..self.edge.len()).any(|j| j != i && point_in_poly(q, &self.edge[j]))
            })
            .collect()
    }
    /// keep only outer outline `n` (1-based, see `outer_outlines`) and the cut-outs inside it;
    /// edge segments and the board box shrink to it (`--outline N`)
    pub fn select_outline(&mut self, n: usize) -> Result<(), String> {
        let outer = self.outer_outlines();
        let Some(&oi) = n.checked_sub(1).and_then(|k| outer.get(k)) else {
            return Err(format!(
                "--outline {n}: the board has {} closed Edge.Cuts outline(s)",
                outer.len()
            ));
        };
        let o = self.edge[oi].clone();
        let mut bb = BBox::empty();
        for q in &o {
            bb.add(*q);
        }
        let near = bb.inflate(1e-3);
        self.edge = self
            .edge
            .iter()
            .enumerate()
            .filter(|(i, pl)| *i == oi || point_in_poly(pl[0], &o))
            .map(|(_, pl)| pl.clone())
            .collect();
        self.edge_segs
            .retain(|(a, b)| near.contains(*a) && near.contains(*b));
        self.bbox = bb;
        Ok(())
    }
}

pub fn parse_board(text: String) -> Result<Board, String> {
    let root = sexpr::parse(&text)?;
    // KiCad 5 and older (modules, numbered nets, legacy arcs) is not read; refuse instead of
    // silently routing an empty board
    if let Some(v) = root.valf("version") {
        if v < 20211014.0 {
            return Err(format!(
                "board file format {v} is from KiCad 5 or older; open and save it in KiCad 6+ first"
            ));
        }
    }
    let format = sexpr::NetFormat::from_root(&root)?;
    let mut b = Board {
        net_codes: format.codes,
        ..Board::default()
    };
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
            "setup" => {
                b.mask_margin = it.valf("pad_to_mask_clearance").unwrap_or(0.0);
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
                    .map(|pd| {
                        let mut q = parse_pad(pd, &mut b, r, it.valf("solder_mask_margin"));
                        q.zone_connect =
                            q.zone_connect.or(it.valf("zone_connect").map(|v| v as u8));
                        q.thermal_gap = q.thermal_gap.or(it.valf("thermal_gap"));
                        q.thermal_bridge = q.thermal_bridge.or(it.valf("thermal_width"));
                        q
                    })
                    .collect();
                let fpz: Vec<Zone> = it
                    .children("zone")
                    .map(|z| parse_zone(z, &mut b, Some(&refd), Some((p(x, y), r))))
                    .collect();
                b.zones.extend(fpz);
                let mut mask = Vec::new();
                for g in it.lists() {
                    if !(g.head().starts_with("fp_") || g.head() == "property") {
                        continue;
                    }
                    let (at, lay) = (p(x, y), g.val("layer").unwrap_or(""));
                    if lay == "Edge.Cuts" {
                        // cut-outs drawn inside a footprint (frame: footprint position and rotation)
                        for sh in graphic_shapes(g) {
                            if let Shape::Capsule { a, b: e, .. } = sh.xform(at, r) {
                                b.edge_segs.push((a, e));
                            }
                        }
                        continue;
                    }
                    let mm = graphic_mask_layers(g);
                    if mm != 0 && !matches!(g.head(), "fp_text" | "property") {
                        // mask openings (e.g. over a solder jumper), kept in the footprint frame
                        for sh in graphic_shapes(g) {
                            mask.push((sh, mm));
                        }
                        continue;
                    }
                    let lm = graphic_layers(g);
                    if lm == 0 || g.head() == "property" || g.flag("hide") {
                        continue; // properties (reference, value) on copper are rare; skip
                    }
                    for sh in graphic_shapes(g) {
                        let sh = match (g.head(), g.xy("at")) {
                            // fp_text: local position, but its angle is already absolute
                            ("fp_text", Some((tx, ty, _))) => {
                                let c = p(tx, ty);
                                sh.xform(at + c.rot(r) - c, 0.0)
                            }
                            _ => sh.xform(at, r),
                        };
                        b.graphics.push((sh, lm));
                    }
                }
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
                    crtyd_parts: courtyard_parts(it),
                    mask,
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
                let net = it.val("net").map(|n| b.net_reference(n)).unwrap_or(0);
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
                let net = it.val("net").map(|n| b.net_reference(n)).unwrap_or(0);
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
            "gr_text" => {
                let lm = graphic_layers(it);
                if lm != 0 {
                    b.graphics
                        .extend(graphic_shapes(it).into_iter().map(|sh| (sh, lm)));
                }
            }
            "gr_line" | "gr_rect" | "gr_poly" | "gr_arc" | "gr_circle" => {
                if it.val("layer") != Some("Edge.Cuts") {
                    let mm = graphic_mask_layers(it);
                    if mm != 0 {
                        for sh in graphic_shapes(it) {
                            b.mask.push((sh, mm));
                        }
                        continue;
                    }
                    let lm = graphic_layers(it);
                    if lm != 0 {
                        b.graphics
                            .extend(graphic_shapes(it).into_iter().map(|sh| (sh, lm)));
                    }
                    continue;
                }
                match it.head() {
                    "gr_rect" => {
                        let (Some(s), Some(e)) = (it.xy("start"), it.xy("end")) else {
                            continue;
                        };
                        b.edge
                            .push(vec![p(s.0, s.1), p(e.0, s.1), p(e.0, e.1), p(s.0, e.1)]);
                    }
                    "gr_poly" => b.edge.push(pts_of(it)),
                    "gr_line" => {
                        let (Some(s), Some(e)) = (it.xy("start"), it.xy("end")) else {
                            continue;
                        };
                        b.edge_segs.push((p(s.0, s.1), p(e.0, e.1)));
                    }
                    "gr_arc" => {
                        if let Some(pts) = arc_pts(it) {
                            for w in pts.windows(2) {
                                b.edge_segs.push((w[0], w[1]));
                            }
                        }
                    }
                    "gr_circle" => {
                        let (Some(c), Some(e)) = (it.xy("center"), it.xy("end")) else {
                            continue;
                        };
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
        let loops = chain(&b.edge_segs);
        b.edge.extend(loops);
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

    /// minimal KiCad 10 board around the given items
    fn mini(items: &str) -> String {
        format!(
            "(kicad_pcb (version 20260206) (layers (0 \"F.Cu\" signal) (2 \"B.Cu\" signal) (25 \"Edge.Cuts\" user)) {items})"
        )
    }

    #[test]
    fn rejects_kicad5_boards() {
        let e = parse_board("(kicad_pcb (version 20171130) (module R1))".into()).unwrap_err();
        assert!(e.contains("KiCad 5"), "{e}");
    }

    #[test]
    fn legacy_edge_arc_is_read() {
        // centre (10, 10), start (12, 10), 90 degrees -> ends near (10, 12)
        let b = parse_board(mini(
            "(gr_arc (start 10 10) (end 12 10) (angle 90) (layer \"Edge.Cuts\") (width 0.1))",
        ))
        .unwrap();
        assert!(b.edge_segs.len() >= 2);
        let last = b.edge_segs.last().unwrap().1;
        assert!(
            (last.x - 10.0).abs() < 1e-6 && (last.y - 12.0).abs() < 1e-6,
            "{last:?}"
        );
    }

    #[test]
    fn copper_graphics_text_and_footprint_cutouts() {
        let b = parse_board(mini(concat!(
            "(gr_text \"HELLO\" (at 50 20 0) (layer \"F.Cu\") (effects (font (size 1 1) (thickness 0.2))))",
            "(gr_line (start 0 0) (end 5 0) (stroke (width 0.3)) (layer \"B.Cu\"))",
            "(gr_text \"silk\" (at 0 0 0) (layer \"F.SilkS\"))",
            "(footprint \"X\" (layer \"F.Cu\") (at 30 30 90)",
            "  (fp_poly (pts (xy 0 0) (xy 1 0) (xy 1 1)) (stroke (width 0)) (fill yes) (layer \"F.Cu\"))",
            "  (fp_line (start 2 0) (end 4 0) (stroke (width 0.05)) (layer \"Edge.Cuts\")))",
        )))
        .unwrap();
        // text box, B.Cu line, footprint polygon; silkscreen text ignored
        assert_eq!(b.graphics.len(), 3);
        let (txt, lm) = &b.graphics[0];
        assert_eq!(*lm, 1);
        assert_eq!(txt.dist(p(50.0, 20.0)), 0.0);
        assert!(txt.dist(p(50.0 + 3.5, 20.0)) > 0.0); // 5 glyphs x 1.1 mm / 2 + thickness
        assert_eq!(b.graphics[1].1, 1 << (NL - 1));
        // footprint frame: (2, 0) rotated by 90 degrees -> (30, 28)
        let (a, _) = b.edge_segs[0];
        assert!(
            (a.x - 30.0).abs() < 1e-6 && (a.y - 28.0).abs() < 1e-6,
            "{a:?}"
        );
    }

    #[test]
    fn multi_line_copper_text_is_tall() {
        let b = parse_board(mini(
            "(gr_text \"AB\\nCD\" (at 10 10 0) (layer \"F.Cu\") (effects (font (size 1 1) (thickness 0.2))))",
        ))
        .unwrap();
        let (txt, _) = &b.graphics[0];
        // two lines: 2 x 1.6 mm + thickness high -> 1.5 mm below the anchor is still text
        assert_eq!(txt.dist(p(10.0, 11.5)), 0.0);
        assert!(txt.dist(p(10.0 + 1.5, 10.0)) > 0.0); // 2 glyphs wide, not 5
    }

    #[test]
    fn drill_offset_shifts_the_pad_copper() {
        let b = parse_board(mini(
            "(footprint \"J\" (layer \"F.Cu\") (at 10 10) (pad \"1\" thru_hole oval (at 0 0) \
             (size 1.5 2) (drill 1 (offset 0 0.5)) (layers \"*.Cu\") (net \"A\")))",
        ))
        .unwrap();
        let f = &b.fps[0];
        let s = &f.pad_shapes(&f.pads[0])[0];
        // copper spans y 9.5..11.5 (centre 10.5), the hole stays at 10
        assert!(s.dist(p(10.0, 11.4)) == 0.0 && s.dist(p(10.0, 9.4)) > 0.0);
    }

    #[test]
    fn layer_masks() {
        let m = layer_mask(&["F.Cu".into(), "In2.Cu".into()]);
        assert_eq!(m, 0b101);
        assert_eq!(layer_mask(&["*.Cu".into()]), 0x3f);
        assert_eq!(layer_mask(&["F&B.Cu".into()]), 0b100001);
    }
}
