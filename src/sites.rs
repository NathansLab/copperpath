//! Via-site and track-site maps per net class on a regular grid (default 0.25 mm).
//! A cell is a via site for class C if a through via of C's size centred there keeps every rule
//! (copper clearance per layer, via-to-pad gap 0.2 mm, hole-to-hole, edge, rule areas, via-keepout
//! courtyards, mounting-hole keepouts). Blocked cells record the blocker (kind | layer << 4).

use crate::board::*;
use crate::geom::*;
use crate::model::*;
use rayon::prelude::*;

pub const R_FREE: u8 = 0;
pub const K_PAD: u8 = 1;
pub const K_TRACK: u8 = 2;
pub const K_VIA: u8 = 3;
pub const K_ZONE: u8 = 4;
pub const K_KEEPOUT: u8 = 5;
pub const K_CRTYD: u8 = 6;
pub const K_EDGE: u8 = 7;
pub const K_HOLE: u8 = 8;
pub const K_MH: u8 = 9;
pub const K_MASK: u8 = 10;

pub fn kind_name(k: u8) -> &'static str {
    match k & 15 {
        K_PAD => "pad",
        K_TRACK => "track",
        K_VIA => "via",
        K_ZONE => "zone",
        K_KEEPOUT => "rule_area",
        K_CRTYD => "via_keepout_courtyard",
        K_EDGE => "edge",
        K_HOLE => "hole_to_hole",
        K_MH => "mounting_hole",
        K_MASK => "solder_mask_aperture",
        _ => "free",
    }
}

pub struct ClassSites {
    pub class: usize,
    pub rep_net: usize,
    pub via_r: f64,
    pub drill_r: f64,
    pub track_hw: f64,
    /// via blocker per cell (0 = free site)
    pub via: Vec<u8>,
    /// track blocker per layer per cell
    pub track: Vec<Vec<u8>>,
}

pub struct SiteMaps {
    pub gs: GridSpec,
    pub cls: Vec<ClassSites>,
}

impl SiteMaps {
    pub fn for_class(&self, c: usize) -> Option<&ClassSites> {
        self.cls.iter().find(|s| s.class == c)
    }
}

fn obj_code(o: &Obj, m: &Model) -> u8 {
    match o.kind {
        Kind::Pad { fp, .. } => {
            if m.is_mh(fp) {
                K_MH
            } else {
                K_PAD
            }
        }
        Kind::Track => K_TRACK,
        Kind::Via => K_VIA,
        Kind::Zone => K_ZONE,
        Kind::Keepout { .. } => K_KEEPOUT,
        Kind::ViaKeepout => K_CRTYD,
        Kind::Mask { .. } => K_MASK,
    }
}

/// via blocker map for net `net` with via radius vr and drill radius dr
pub fn via_map(m: &Model, gs: &GridSpec, net: usize, vr: f64, dr: f64, objs: &[Obj]) -> Vec<u8> {
    let mut map = vec![R_FREE; gs.n()];
    via_stamp(m, gs, net, vr, dr, objs, &mut map);
    edge_block(m, gs, net, vr, &mut map);
    map
}

/// stamps the via blockers of `objs` into the free cells of `map` (no board edge)
pub fn via_stamp(
    m: &Model,
    gs: &GridSpec,
    net: usize,
    vr: f64,
    dr: f64,
    objs: &[Obj],
    map: &mut [u8],
) {
    let via_pad_gap = 0.2; // via-to-pad gap: no via in or touching a pad (any net)
    for o in objs {
        let code = obj_code(o, m);
        // copper clearance on every layer the via crosses (through via) -> worst layer
        let mut d: f64 = -1.0;
        let mut lay = 0usize;
        match o.kind {
            Kind::Keepout { vias, .. } => {
                if vias {
                    d = vr; // via copper may not intersect the rule area
                }
            }
            Kind::ViaKeepout => d = vr,
            _ => {
                for l in 0..NL {
                    if o.layers & (1 << l) == 0 {
                        continue;
                    }
                    let mut c = m.clr(net, o, l);
                    if matches!(o.kind, Kind::Pad { .. }) {
                        c = c.max(via_pad_gap);
                    }
                    if c < 0.0 {
                        continue;
                    }
                    if c + vr > d {
                        d = c + vr;
                        lay = l;
                    }
                }
                if let Kind::Pad { fp, .. } = o.kind {
                    if m.is_mh(fp) {
                        let k = m.r.hole_keepout(&m.b.fps[fp].refd, net);
                        if k + vr > d {
                            d = k + vr;
                        }
                    }
                }
            }
        }
        if d >= 0.0 {
            let v = code | ((lay as u8) << 4);
            gs.stamp(&o.shape, d, |x, y| {
                let i = gs.idx(x, y);
                if map[i] == R_FREE {
                    map[i] = v;
                }
            });
        }
        // hole to hole
        if o.hole_r > 0.0 {
            let c = m.r.min_hole_to_hole + o.hole_r + dr;
            let ctr = match &o.shape {
                Shape::Capsule { a, b, .. } => (*a + *b) * 0.5,
                Shape::Poly { pts, .. } => {
                    pts.iter().fold(p(0.0, 0.0), |s, q| s + *q) * (1.0 / pts.len() as f64)
                }
            };
            gs.stamp(&Shape::circle(ctr, 0.0), c, |x, y| {
                let i = gs.idx(x, y);
                if map[i] == R_FREE {
                    map[i] = K_HOLE;
                }
            });
        }
    }
}

/// track blocker map on layer l for net `net` with half width hw
pub fn track_map(m: &Model, gs: &GridSpec, net: usize, hw: f64, l: usize, objs: &[Obj]) -> Vec<u8> {
    let mut map = vec![R_FREE; gs.n()];
    track_stamp(m, gs, net, hw, l, objs, &mut map);
    if m.net_layers[net] & (1 << l) == 0 {
        for v in map.iter_mut() {
            if *v == R_FREE {
                *v = K_KEEPOUT | ((l as u8) << 4);
            }
        }
    }
    edge_block(m, gs, net, hw, &mut map);
    map
}

/// stamps the track blockers of `objs` on layer l into the free cells of `map` (no board edge,
/// no layer restriction of the net)
pub fn track_stamp(
    m: &Model,
    gs: &GridSpec,
    net: usize,
    hw: f64,
    l: usize,
    objs: &[Obj],
    map: &mut [u8],
) {
    for o in objs {
        if o.layers & (1 << l) == 0 {
            continue;
        }
        let d = match o.kind {
            Kind::Keepout { tracks, .. } => {
                if tracks {
                    hw
                } else {
                    -1.0
                }
            }
            Kind::ViaKeepout => -1.0,
            _ => {
                let c = m.clr(net, o, l);
                if c < 0.0 {
                    -1.0
                } else {
                    let mut c = c;
                    if let Kind::Pad { fp, .. } = o.kind {
                        if m.is_mh(fp) {
                            c = c.max(m.r.hole_keepout(&m.b.fps[fp].refd, net));
                        }
                    }
                    c + hw
                }
            }
        };
        if d < 0.0 {
            continue;
        }
        let v = obj_code(o, m) | ((l as u8) << 4);
        gs.stamp(&o.shape, d, |x, y| {
            let i = gs.idx(x, y);
            if map[i] == R_FREE {
                map[i] = v;
            }
        });
        // board hole clearance to the drill of foreign / unconnected holes (NPTH pads of mounting holes)
        if o.hole_r > 0.0 && (o.net != net || net == 0) {
            let ctr = match &o.shape {
                Shape::Capsule { a, b, .. } => (*a + *b) * 0.5,
                Shape::Poly { pts, .. } => {
                    pts.iter().fold(p(0.0, 0.0), |s, q| s + *q) * (1.0 / pts.len() as f64)
                }
            };
            gs.stamp(
                &Shape::circle(ctr, 0.0),
                o.hole_r + m.r.min_hole_clearance + hw,
                |x, y| {
                    let i = gs.idx(x, y);
                    if map[i] == R_FREE {
                        map[i] = K_HOLE | ((l as u8) << 4);
                    }
                },
            );
        }
    }
}

fn edge_block(m: &Model, gs: &GridSpec, net: usize, r: f64, map: &mut [u8]) {
    let e = m.r.edge_clearance(net) + r;
    for (a, b) in &m.b.edge_segs {
        gs.stamp(
            &Shape::Capsule {
                a: *a,
                b: *b,
                r: 0.0,
            },
            e,
            |x, y| {
                let i = gs.idx(x, y);
                if map[i] == R_FREE {
                    map[i] = K_EDGE;
                }
            },
        );
    }
    // outside the outline
    if !m.b.edge.is_empty() {
        for iy in 0..gs.ny {
            for ix in 0..gs.nx {
                let i = gs.idx(ix, iy);
                if map[i] == R_FREE && !m.b.inside(gs.center(ix, iy)) {
                    map[i] = K_EDGE;
                }
            }
        }
    }
}

/// representative routable net per class (the class's net with most pins)
pub fn class_reps(m: &Model) -> Vec<(usize, usize)> {
    let mut cnt = vec![0usize; m.b.nets.len()];
    for f in &m.b.fps {
        for pd in &f.pads {
            cnt[pd.net] += 1;
        }
    }
    let mut best: std::collections::BTreeMap<usize, (usize, usize)> = Default::default();
    for (n, &cn) in cnt.iter().enumerate().take(m.b.nets.len()).skip(1) {
        if !m.routable[n] {
            continue;
        }
        let c = m.r.net_class[n];
        let e = best.entry(c).or_insert((n, 0));
        if cn > e.1 {
            *e = (n, cn);
        }
    }
    best.into_iter().map(|(c, (n, _))| (c, n)).collect()
}

pub fn build(m: &Model, gs: GridSpec, reps: &[(usize, usize)]) -> SiteMaps {
    let objs = &m.objs;
    let cls: Vec<ClassSites> = reps
        .par_iter()
        .map(|&(c, net)| {
            let (vd, vdr) = m.r.via(net);
            let (_, tw) = m.r.track_width(net);
            let via = via_map(m, &gs, net, vd / 2.0, vdr / 2.0, objs);
            let track: Vec<Vec<u8>> = (0..NL)
                .into_par_iter()
                .map(|l| track_map(m, &gs, net, tw / 2.0, l, objs))
                .collect();
            ClassSites {
                class: c,
                rep_net: net,
                via_r: vd / 2.0,
                drill_r: vdr / 2.0,
                track_hw: tw / 2.0,
                via,
                track,
            }
        })
        .collect();
    SiteMaps { gs, cls }
}
