//! Routing model: obstacle objects from the board, board configuration (optional JSON, see
//! `examples/config.json`),
//! routable nets, pins and copper connectivity.

use crate::board::*;
use crate::geom::*;
use crate::rules::{wild, Rules};
use serde_json::Value;
use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Kind {
    Pad {
        fp: usize,
        pad: usize,
    },
    Track,
    Via,
    Zone,
    /// rule area: forbids tracks and/or vias on its layers
    Keepout {
        tracks: bool,
        vias: bool,
    },
    /// courtyard of a via-keepout footprint (config via_keepout_refs)
    ViaKeepout,
}

#[derive(Debug, Clone)]
pub struct Obj {
    pub shape: Shape,
    pub layers: u8,
    pub net: usize,
    pub kind: Kind,
    pub locked: bool,
    /// drilled hole radius (THT pads, vias), 0 otherwise
    pub hole_r: f64,
    pub bbox: BBox,
}

/// Board configuration. Everything is optional; the defaults route every net with at least two
/// pins on every copper layer of the board. See `examples/config.json` for the JSON format.
#[derive(Debug, Clone)]
pub struct BoardCfg {
    /// net classes that are not routed (wildcards), e.g. power classes poured as planes
    pub skip_classes: Vec<String>,
    /// nets that are not routed (wildcards)
    pub skip_nets: Vec<String>,
    /// plane nets (exact names): never routed, their pours are handled elsewhere
    pub plane_nets: Vec<String>,
    /// footprints (reference wildcards) whose courtyard forbids vias
    pub via_keepout_refs: Vec<String>,
    /// allowed copper layers per net class (class wildcard, layer mask)
    pub class_layers: Vec<(String, u8)>,
    /// preferred direction per copper layer: 0 = horizontal, 1 = vertical (global router)
    pub layer_dir: [u8; NL],
    /// reference wildcard of mounting holes (their keepout rules apply)
    pub mh_pattern: String,
    /// locked pours (zone name wildcards) that yield like unlocked ones (KiCad refills them)
    pub yield_pours: Vec<String>,
}

impl Default for BoardCfg {
    fn default() -> Self {
        BoardCfg {
            skip_classes: Vec::new(),
            skip_nets: Vec::new(),
            plane_nets: Vec::new(),
            via_keepout_refs: Vec::new(),
            class_layers: Vec::new(),
            layer_dir: [0, 1, 0, 1, 0, 1],
            mh_pattern: "MH*".into(),
            yield_pours: Vec::new(),
        }
    }
}

/// Load a board configuration. The keys may sit at the top level of the JSON object, or per board
/// under `boards.<name>` (selected with `board`; without a name the first entry is used).
/// `layer_dir` and `mounting_hole_refs` may also be given at the top level next to `boards`.
pub fn load_cfg(path: &str, board: Option<&str>) -> Result<BoardCfg, String> {
    let t = std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?;
    let j: Value = serde_json::from_str(&t).map_err(|e| format!("{path}: {e}"))?;
    parse_cfg(&j, board)
}

/// Board configuration from an already parsed JSON value (see [`load_cfg`]).
pub fn parse_cfg(j: &Value, board: Option<&str>) -> Result<BoardCfg, String> {
    let mut c = BoardCfg::default();
    let b = match j.get("boards").and_then(|v| v.as_object()) {
        Some(boards) => match board {
            Some(name) => boards
                .get(name)
                .ok_or_else(|| format!("config: no board '{name}' under \"boards\""))?,
            None => boards
                .values()
                .next()
                .ok_or("config: \"boards\" is empty")?,
        },
        None => j,
    };
    for src in [j, b] {
        if let Some(ld) = src["layer_dir"].as_object() {
            for (k, v) in ld {
                if let Some(i) = layer_idx(k) {
                    c.layer_dir[i] = if v.as_str() == Some("V") { 1 } else { 0 };
                }
            }
        }
        if let Some(s) = src["mounting_hole_refs"].as_str() {
            c.mh_pattern = s.to_string();
        }
    }
    let strs = |v: &Value| -> Vec<String> {
        v.as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|s| s.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default()
    };
    c.skip_classes = strs(&b["skip_classes"]);
    c.skip_nets = strs(&b["skip_nets"]);
    c.yield_pours = strs(&b["yield_pours"]);
    c.via_keepout_refs = strs(&b["via_keepout_refs"]);
    if let Some(cl) = b["class_layers"].as_object() {
        for (k, v) in cl {
            c.class_layers.push((k.clone(), layer_mask(&strs(v))));
        }
    }
    if let Some(pl) = b["planes"].as_array() {
        for e in pl {
            if let Some(n) = e["net"].as_str().or(e.as_str()) {
                c.plane_nets.push(n.to_string());
            }
        }
    }
    Ok(c)
}

pub struct Model<'a> {
    pub b: &'a Board,
    pub r: &'a Rules,
    pub cfg: BoardCfg,
    pub objs: Vec<Obj>,
    /// routable nets (have >= 2 pins, not skipped, not plane nets)
    pub routable: Vec<bool>,
    pub net_layers: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CopperMode {
    /// all copper of the file (as routed)
    All,
    /// only locked tracks/vias/zones (unlocked router copper and router pours removed)
    LockedOnly,
}

impl<'a> Model<'a> {
    pub fn new(b: &'a Board, r: &'a Rules, cfg: BoardCfg, mode: CopperMode) -> Model<'a> {
        let mut objs = Vec::new();
        for (fi, f) in b.fps.iter().enumerate() {
            Self::push_fp(&mut objs, f, fi, &cfg);
        }
        for t in &b.tracks {
            if mode == CopperMode::LockedOnly && !t.locked {
                continue;
            }
            let s = Shape::Capsule {
                a: t.a,
                b: t.b,
                r: t.w / 2.0,
            };
            objs.push(Obj {
                bbox: s.bbox(),
                shape: s,
                layers: 1 << t.layer,
                net: t.net,
                kind: Kind::Track,
                locked: t.locked,
                hole_r: 0.0,
            });
        }
        for v in &b.vias {
            if mode == CopperMode::LockedOnly && !v.locked {
                continue;
            }
            let s = Shape::circle(v.at, v.d / 2.0);
            objs.push(Obj {
                bbox: s.bbox(),
                shape: s,
                layers: 0x3f,
                net: v.net,
                kind: Kind::Via,
                locked: v.locked,
                hole_r: v.drill / 2.0,
            });
        }
        for z in &b.zones {
            if z.outline.len() < 3 {
                continue;
            }
            let s = Shape::Poly {
                pts: z.outline.clone(),
                r: 0.0,
            };
            if z.keepout {
                if z.ko_tracks || z.ko_vias {
                    objs.push(Obj {
                        bbox: s.bbox(),
                        shape: s,
                        layers: z.layers,
                        net: 0,
                        kind: Kind::Keepout {
                            tracks: z.ko_tracks,
                            vias: z.ko_vias,
                        },
                        locked: true,
                        hole_r: 0.0,
                    });
                }
                continue;
            }
            if mode == CopperMode::LockedOnly && !z.locked {
                continue;
            }
            if !z.locked || cfg.yield_pours.iter().any(|p| wild(&z.name, p)) {
                continue; // router pours never block (they refill)
            }
            objs.push(Obj {
                bbox: s.bbox(),
                shape: s,
                layers: z.layers,
                net: z.net,
                kind: Kind::Zone,
                locked: true,
                hole_r: 0.0,
            });
        }
        for (s, lm) in &b.graphics {
            objs.push(Obj {
                bbox: s.bbox(),
                shape: s.clone(),
                layers: *lm,
                net: 0,
                kind: Kind::Zone,
                locked: true,
                hole_r: 0.0,
            });
        }
        let n = b.nets.len();
        let mut npins = vec![0usize; n];
        for f in &b.fps {
            for pd in &f.pads {
                if pd.net != 0 && pd.layers != 0 {
                    npins[pd.net] += 1;
                }
            }
        }
        let routable: Vec<bool> = (0..n)
            .map(|i| {
                if i == 0 || npins[i] < 2 {
                    return false;
                }
                let name = &b.nets[i];
                if name.starts_with("unconnected-") {
                    return false;
                }
                let cls = &r.class_of(i).name;
                if cfg.skip_classes.iter().any(|p| wild(cls, p))
                    || cfg.skip_nets.iter().any(|p| wild(name, p))
                {
                    return false;
                }
                !cfg.plane_nets.iter().any(|p| p == name)
            })
            .collect();
        let net_layers: Vec<u8> = (0..n)
            .map(|i| {
                let cls = &r.class_of(i).name;
                cfg.class_layers
                    .iter()
                    .find(|(p, _)| wild(cls, p))
                    .map(|(_, m)| *m)
                    .unwrap_or(0x3f)
                    & b.cu_mask
            })
            .collect();
        Model {
            b,
            r,
            cfg,
            objs,
            routable,
            net_layers,
        }
    }

    pub fn push_fp(objs: &mut Vec<Obj>, f: &Footprint, fi: usize, cfg: &BoardCfg) {
        for (pi, pd) in f.pads.iter().enumerate() {
            let hole_r = if pd.kind.contains("thru_hole") {
                pd.drill / 2.0
            } else {
                0.0
            };
            // outer copper layers (F.Cu, B.Cu)
            let outer: u8 = 1 | (1 << (NL - 1));
            if pd.inner_barrel_only && pd.layers & !outer != 0 {
                // unused inner layers keep only the plated barrel (KiCad removes the annulus there;
                // a track of the own net on such a layer would bring it back: accepted)
                let s = Shape::circle(f.pad_world(pd).0, pd.drill / 2.0);
                objs.push(Obj {
                    bbox: s.bbox(),
                    shape: s,
                    layers: pd.layers & !outer,
                    net: pd.net,
                    kind: Kind::Pad { fp: fi, pad: pi },
                    locked: f.locked,
                    hole_r,
                });
            }
            for s in f.pad_shapes(pd) {
                let layers = if pd.kind == "np_thru_hole" {
                    0x3f
                } else if pd.inner_barrel_only {
                    pd.layers & outer
                } else {
                    pd.layers
                };
                if layers == 0 {
                    continue;
                }
                objs.push(Obj {
                    bbox: s.bbox(),
                    shape: s,
                    layers,
                    net: pd.net,
                    kind: Kind::Pad { fp: fi, pad: pi },
                    locked: f.locked,
                    hole_r,
                });
            }
        }
        if cfg.via_keepout_refs.iter().any(|p| wild(&f.refd, p)) {
            let c = f.crtyd_world();
            if c.len() >= 3 {
                let s = Shape::Poly { pts: c, r: 0.0 };
                objs.push(Obj {
                    bbox: s.bbox(),
                    shape: s,
                    layers: 0x3f,
                    net: 0,
                    kind: Kind::ViaKeepout,
                    locked: true,
                    hole_r: 0.0,
                });
            }
        }
    }

    pub fn is_mh(&self, fi: usize) -> bool {
        wild(&self.b.fps[fi].refd, &self.cfg.mh_pattern)
    }

    /// required clearance between a new item of net `net` and an obstacle on layer l (copper rules only)
    #[inline]
    pub fn clr(&self, net: usize, o: &Obj, l: usize) -> f64 {
        match o.kind {
            Kind::Keepout { .. } | Kind::ViaKeepout => 0.0,
            _ => {
                if o.net == net && net != 0 {
                    return -1.0; // same net: not an obstacle
                }
                let c = self.r.clr(net, o.net, is_outer(l));
                if let Kind::Pad { fp, pad } = o.kind {
                    return c.max(self.b.fps[fp].pads[pad].local_clr);
                }
                c
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// pins and connectivity

#[derive(Debug, Clone)]
pub struct Pin {
    pub fp: usize,
    pub pad: usize,
    pub at: P,
    pub layers: u8,
    pub comp: usize, // connectivity component id within the net
}

/// pins of every net plus the copper components already formed by locked tracks/vias/zones
pub fn pins_by_net(m: &Model) -> HashMap<usize, Vec<Pin>> {
    let mut out: HashMap<usize, Vec<Pin>> = HashMap::new();
    for (fi, f) in m.b.fps.iter().enumerate() {
        for (pi, pd) in f.pads.iter().enumerate() {
            if pd.net == 0 || pd.layers == 0 || !m.routable[pd.net] {
                continue;
            }
            let (c, _) = f.pad_world(pd);
            out.entry(pd.net).or_default().push(Pin {
                fp: fi,
                pad: pi,
                at: c,
                layers: pd.layers,
                comp: 0,
            });
        }
    }
    // union-find over same-net objects that touch on a shared layer
    let mut by_net: HashMap<usize, Vec<usize>> = HashMap::new();
    for (i, o) in m.objs.iter().enumerate() {
        if o.net != 0
            && matches!(
                o.kind,
                Kind::Pad { .. } | Kind::Track | Kind::Via | Kind::Zone
            )
            && out.contains_key(&o.net)
        {
            by_net.entry(o.net).or_default().push(i);
        }
    }
    for (net, pins) in out.iter_mut() {
        let ids = by_net.get(net).cloned().unwrap_or_default();
        let k = ids.len();
        let mut par: Vec<usize> = (0..k).collect();
        fn find(par: &mut [usize], x: usize) -> usize {
            let mut x = x;
            while par[x] != x {
                par[x] = par[par[x]];
                x = par[x];
            }
            x
        }
        for a in 0..k {
            for c in a + 1..k {
                let (oa, oc) = (&m.objs[ids[a]], &m.objs[ids[c]]);
                if oa.layers & oc.layers == 0 || !oa.bbox.overlaps(&oc.bbox) {
                    continue;
                }
                // pads of one footprint pad number are connected anyway via kind match below
                if shape_dist(&oa.shape, &oc.shape) < 1e-3 {
                    let (ra, rc) = (find(&mut par, a), find(&mut par, c));
                    par[ra] = rc;
                }
            }
        }
        for pin in pins.iter_mut() {
            let idx = ids.iter().position(|&i| matches!(m.objs[i].kind, Kind::Pad { fp, pad } if fp == pin.fp && pad == pin.pad));
            pin.comp = idx.map(|j| find(&mut par, j)).unwrap_or(usize::MAX);
        }
    }
    out
}

/// two-pin connections: minimum spanning tree over the copper components of each net
pub fn mst_connections(pins: &[Pin]) -> Vec<(usize, usize)> {
    // group pins by component, connect components (Prim on component representatives using nearest pin pair)
    let mut comps: Vec<usize> = pins.iter().map(|p| p.comp).collect();
    comps.sort();
    comps.dedup();
    if comps.len() < 2 {
        return vec![];
    }
    let k = comps.len();
    let cid = |c: usize| comps.binary_search(&c).unwrap();
    let mut in_tree = vec![false; k];
    let mut best = vec![(f64::INFINITY, 0usize, 0usize); k];
    in_tree[cid(pins[0].comp)] = true;
    let mut res = Vec::new();
    let upd = |best: &mut Vec<(f64, usize, usize)>, in_tree: &Vec<bool>, src_comp: usize| {
        for (i, a) in pins.iter().enumerate() {
            if cid(a.comp) != src_comp {
                continue;
            }
            for (j, b) in pins.iter().enumerate() {
                let cb = cid(b.comp);
                if in_tree[cb] {
                    continue;
                }
                let d = (a.at - b.at).len();
                if d < best[cb].0 {
                    best[cb] = (d, i, j);
                }
            }
        }
    };
    upd(&mut best, &in_tree, cid(pins[0].comp));
    for _ in 1..k {
        let mut bi = usize::MAX;
        let mut bd = f64::INFINITY;
        for c in 0..k {
            if !in_tree[c] && best[c].0 < bd {
                bd = best[c].0;
                bi = c;
            }
        }
        if bi == usize::MAX {
            break;
        }
        in_tree[bi] = true;
        res.push((best[bi].1, best[bi].2));
        upd(&mut best, &in_tree, bi);
    }
    res
}

#[cfg(test)]
mod tests {
    use super::*;

    fn board(items: &str) -> Board {
        crate::board::parse_board(format!(
            "(kicad_pcb (version 20260206) (layers (0 \"F.Cu\" signal) (4 \"In1.Cu\" signal) \
             (6 \"In2.Cu\" signal) (2 \"B.Cu\" signal)) {items})"
        ))
        .unwrap()
    }

    fn objs(b: &Board, cfg: BoardCfg, mode: CopperMode) -> Vec<Obj> {
        let r = Rules::load("", "", &b.nets).unwrap();
        Model::new(b, &r, cfg, mode).objs
    }

    #[test]
    fn unlocked_copper_of_other_nets_is_an_obstacle() {
        let b = board("(segment (start 0 0) (end 5 0) (width 0.25) (layer \"F.Cu\") (net \"HV\"))");
        let track = |o: &Obj| matches!(o.kind, Kind::Track);
        assert_eq!(
            objs(&b, BoardCfg::default(), CopperMode::All)
                .iter()
                .filter(|o| track(o))
                .count(),
            1
        );
        assert_eq!(
            objs(&b, BoardCfg::default(), CopperMode::LockedOnly)
                .iter()
                .filter(|o| track(o))
                .count(),
            0
        );
    }

    #[test]
    fn yield_pours_releases_locked_pours() {
        let b = board(
            "(zone (net \"GND\") (name \"CELL1_GND\") (layer \"F.Cu\") (locked yes) \
             (polygon (pts (xy 0 0) (xy 5 0) (xy 5 5) (xy 0 5))))",
        );
        let zones = |c: BoardCfg| {
            objs(&b, c, CopperMode::All)
                .iter()
                .filter(|o| matches!(o.kind, Kind::Zone))
                .count()
        };
        assert_eq!(zones(BoardCfg::default()), 1);
        let cfg = BoardCfg {
            yield_pours: vec!["CELL*".into()],
            ..BoardCfg::default()
        };
        assert_eq!(zones(cfg), 0);
    }

    #[test]
    fn removed_unused_layers_leave_only_the_barrel_inside() {
        let b = board(
            "(footprint \"MH\" (layer \"F.Cu\") (at 10 10) (property \"Reference\" \"H1\") \
             (pad \"1\" thru_hole circle (at 0 0) (size 8.6 8.6) (drill 4.3) (layers \"*.Cu\") \
             (remove_unused_layers yes) (keep_end_layers yes) (net \"PE\")))",
        );
        let os = objs(&b, BoardCfg::default(), CopperMode::All);
        let inner: u8 = 0b011110;
        let barrel: Vec<&Obj> = os.iter().filter(|o| o.layers & inner != 0).collect();
        assert_eq!(barrel.len(), 1);
        assert!((barrel[0].bbox.x1 - barrel[0].bbox.x0 - 4.3).abs() < 1e-6);
        // the full 8.6 mm pad stays on F.Cu and B.Cu only
        assert!(os
            .iter()
            .any(|o| o.layers == 0b100001 && o.bbox.x1 - o.bbox.x0 > 8.5));
    }
}
