//! copperpath command line: check, place, route, opens, hints, pads, netclass (see README.md).

use copperpath::{
    board, check, droute, fanout, geom, groute, model, place, reroute, rules, sch, sites, stitch,
};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::time::Instant;

/// version + git hash (set by build.rs)
const VERSION: &str = concat!(
    env!("CARGO_PKG_VERSION"),
    " (",
    env!("COPPERPATH_GIT_HASH"),
    ")"
);

const USAGE: &str = "usage: copperpath <command> --pcb BOARD.kicad_pcb [options]

commands:
  check     routability report (via sites, global-route congestion, hotspots)
  place     simulated-annealing placement inside zones (--zones zones.json)
  route     detailed router for a region (--region x0,y0,x1,y1 --out OUT.kicad_pcb)
  opens     unconnected connections per net, counted from the copper
  hints     near-collinear pad pairs that force jogs between straight pad exits
  pads      pad world positions, sizes and nets (tab separated)
  netclass  net -> net class table (tab separated)
  stitch    stitching vias for pour islands of a plane net (--net --zones --plane --out)
  fanout    track + via from every open island of a plane net to its plane (--net --plane --out)

common options:
  --cfg config.json   board configuration (optional, see examples/config.json)
  --board NAME        entry under \"boards\" in the config
  --pro / --dru       project / custom rules file (default: next to the board)
  --outline N         use only closed Edge.Cuts outline N (1-based, file order) of a panel
  --threads N         worker threads (default: all cores)
  --json              print one JSON result object to stdout (logs stay on stderr)
  --version           print version and git hash

see README.md for the options of each command";

/// set together with `droute::STOP` by --cancel-file / --cancel-on-stdin-eof (reported as
/// `stopped: "cancel"` instead of "signal")
static CANCELLED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn cancel() {
    CANCELLED.store(true, std::sync::atomic::Ordering::Relaxed);
    droute::STOP.store(true, std::sync::atomic::Ordering::Relaxed);
}

/// cancel paths that work on every platform (a GUI on Windows can only kill the process
/// otherwise): the cancel file appears (polled once per second), or stdin reaches EOF (the
/// parent closed it or died)
fn watch_cancel(file: Option<&str>, stdin_eof: bool) {
    if let Some(f) = file {
        let f = std::path::PathBuf::from(f);
        std::thread::spawn(move || loop {
            if f.exists() {
                eprintln!("[route] cancel file {} found", f.display());
                cancel();
                return;
            }
            std::thread::sleep(std::time::Duration::from_secs(1));
        });
    }
    if stdin_eof {
        std::thread::spawn(|| {
            use std::io::Read;
            let mut buf = [0u8; 256];
            let mut inp = std::io::stdin();
            loop {
                match inp.read(&mut buf) {
                    Ok(0) => break,
                    Err(e) if e.kind() != std::io::ErrorKind::Interrupted => break,
                    _ => {}
                }
            }
            eprintln!("[route] stdin closed");
            cancel();
        });
    }
}

/// command result: JSON summary, or an error message (exit code 1)
type CmdResult = Result<Value, String>;

pub struct Args {
    pub cmd: String,
    pub kv: HashMap<String, String>,
}

impl Args {
    pub fn parse() -> Args {
        let a: Vec<String> = std::env::args().skip(1).collect();
        let cmd = a.first().cloned().unwrap_or_default();
        let mut kv = HashMap::new();
        let mut i = 1;
        while i < a.len() {
            if let Some(k) = a[i].strip_prefix("--") {
                if i + 1 < a.len() && !a[i + 1].starts_with("--") {
                    kv.insert(k.to_string(), a[i + 1].clone());
                    i += 2;
                } else {
                    kv.insert(k.to_string(), "1".into());
                    i += 1;
                }
            } else {
                i += 1;
            }
        }
        Args { cmd, kv }
    }
    pub fn get(&self, k: &str) -> Option<&str> {
        self.kv.get(k).map(|s| s.as_str())
    }
    pub fn s(&self, k: &str, d: &str) -> String {
        self.get(k).unwrap_or(d).to_string()
    }
    pub fn f(&self, k: &str, d: f64) -> f64 {
        self.get(k).and_then(|v| v.parse().ok()).unwrap_or(d)
    }
    pub fn u(&self, k: &str, d: usize) -> usize {
        self.get(k).and_then(|v| v.parse().ok()).unwrap_or(d)
    }
    /// machine-readable mode: stdout carries only the final JSON object
    pub fn json(&self) -> bool {
        self.get("json").is_some()
    }
}

pub struct Ctx {
    pub board: board::Board,
    pub rules: rules::Rules,
    pub cfg: model::BoardCfg,
    pub pcb: String,
    pub pro: String,
    pub dru: String,
}

pub fn load_ctx(a: &Args) -> Result<Ctx, String> {
    let pcb = a
        .get("pcb")
        .ok_or("missing --pcb BOARD.kicad_pcb")?
        .to_string();
    if !std::path::Path::new(&pcb).is_file() {
        return Err(format!("{pcb}: board file not found"));
    }
    let base = pcb.trim_end_matches(".kicad_pcb").to_string();
    let pro = a.s("pro", &format!("{base}.kicad_pro"));
    let dru = a.s("dru", &format!("{base}.kicad_dru"));
    let t0 = Instant::now();
    let mut board = board::load(&pcb)?;
    // panels: several closed outlines; each pin is judged against the outline containing it,
    // --outline N restricts the board to one of them
    let outer = board.outer_outlines().len();
    if let Some(n) = a.get("outline") {
        let n: usize = n
            .parse()
            .map_err(|_| format!("--outline {n}: expected a number from 1 to {outer}"))?;
        board.select_outline(n)?;
        eprintln!("[load] restricted to board outline {n} of {outer}");
    } else if outer > 1 {
        eprintln!("[load] {outer} board outlines (panel): each item is checked against the outline it lies in; --outline N selects one");
    }
    // without a project file KiCad uses its default net class; do the same
    let pro = if std::path::Path::new(&pro).is_file() {
        pro
    } else {
        eprintln!("[load] warning: {pro} not found, using KiCad's default net class");
        String::new()
    };
    let rules = rules::Rules::load(
        &pro,
        if std::path::Path::new(&dru).exists() {
            &dru
        } else {
            ""
        },
        &board.nets,
    )?;
    // board configuration: optional JSON (see examples/config.json), built-in defaults otherwise
    let mut cfg = match a.get("cfg") {
        Some(path) => model::load_cfg(path, a.get("board"))?,
        None => model::BoardCfg::default(),
    };
    if let Some(y) = a.get("yield-pours") {
        cfg.yield_pours
            .extend(y.split(',').filter(|s| !s.is_empty()).map(String::from));
    }
    eprintln!(
        "[load] {} footprints, {} tracks, {} vias, {} zones, {} copper graphics, {} nets, {} DRU rules in {:.2} s",
        board.fps.len(),
        board.tracks.len(),
        board.vias.len(),
        board.zones.len(),
        board.graphics.len(),
        board.nets.len(),
        rules.dru.len(),
        t0.elapsed().as_secs_f64()
    );
    Ok(Ctx {
        board,
        rules,
        cfg,
        pcb,
        pro,
        dru,
    })
}

fn main() {
    let a = Args::parse();
    if matches!(a.cmd.as_str(), "--version" | "-V" | "version") || a.get("version").is_some() {
        println!("copperpath {VERSION}");
        return;
    }
    if matches!(a.cmd.as_str(), "--help" | "-h" | "help") {
        println!("{USAGE}");
        return;
    }
    let threads = a.u(
        "threads",
        std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4),
    );
    rayon::ThreadPoolBuilder::new()
        .num_threads(threads)
        .build_global()
        .ok();
    let t0 = Instant::now();
    let res: CmdResult = match a.cmd.as_str() {
        "check" => cmd_check(&a),
        "place" => cmd_place(&a),
        "route" => cmd_route(&a),
        "opens" => cmd_opens(&a),
        "pads" => cmd_pads(&a),
        "hints" => cmd_hints(&a),
        "netclass" => cmd_netclass(&a),
        "stitch" => cmd_stitch(&a),
        "fanout" => cmd_fanout(&a),
        "" => Err(format!("no command given\n{USAGE}")),
        c => Err(format!("unknown command '{c}'\n{USAGE}")),
    };
    let secs = t0.elapsed().as_secs_f64();
    match res {
        Ok(mut v) => {
            eprintln!("[done] {} in {:.2} s ({} threads)", a.cmd, secs, threads);
            if a.json() {
                if !v.is_object() {
                    v = json!({ "result": v });
                }
                v["ok"] = json!(true);
                v["command"] = json!(a.cmd);
                v["version"] = json!(VERSION);
                v["threads"] = json!(threads);
                v["total_s"] = json!(check::round2(secs));
                if v.get("warnings").is_none() {
                    v["warnings"] = json!([]);
                }
                println!("{}", serde_json::to_string_pretty(&v).unwrap_or_default());
            }
        }
        Err(e) => {
            eprintln!("error: {e}");
            if a.json() {
                println!(
                    "{}",
                    json!({"ok": false, "command": a.cmd, "version": VERSION, "error": e})
                );
            }
            std::process::exit(if a.cmd.is_empty() || !KNOWN.contains(&a.cmd.as_str()) {
                2
            } else {
                1
            });
        }
    }
}

const KNOWN: [&str; 9] = [
    "check", "place", "route", "opens", "pads", "hints", "netclass", "stitch", "fanout",
];

fn write_file(path: &str, data: &str) -> Result<(), String> {
    if let Some(d) = std::path::Path::new(path).parent() {
        if !d.as_os_str().is_empty() {
            std::fs::create_dir_all(d).map_err(|e| format!("{}: {e}", d.display()))?;
        }
    }
    std::fs::write(path, data).map_err(|e| format!("{path}: {e}"))
}

/// pads --pcb B: pad world positions, world bounding-box size and net
fn cmd_pads(a: &Args) -> CmdResult {
    let c = load_ctx(a)?;
    let mut rows = Vec::new();
    for f in &c.board.fps {
        for pd in &f.pads {
            let (q, ang) = f.pad_world(pd);
            let mut bb = geom::BBox::empty();
            for s in f.pad_shapes(pd) {
                let b2 = s.bbox();
                bb.add(geom::p(b2.x0, b2.y0));
                bb.add(geom::p(b2.x1, b2.y1));
            }
            let net = &c.board.nets[pd.net];
            if !a.json() {
                println!(
                    "{}\t{}\t{:.4}\t{:.4}\t{:.2}\t{:.4}\t{:.4}\t{}",
                    f.refd,
                    pd.num,
                    q.x,
                    q.y,
                    ang.rem_euclid(360.0),
                    bb.x1 - bb.x0,
                    bb.y1 - bb.y0,
                    net
                );
            }
            rows.push(json!({"ref": f.refd, "pad": pd.num, "x": geom_r(q.x), "y": geom_r(q.y), "rot": geom_r(ang.rem_euclid(360.0)),
                "w": geom_r(bb.x1 - bb.x0), "h": geom_r(bb.y1 - bb.y0), "net": net}));
        }
    }
    Ok(json!({ "pads": rows }))
}

/// netclass --pcb B: net -> class table
fn cmd_netclass(a: &Args) -> CmdResult {
    let c = load_ctx(a)?;
    let mut map = serde_json::Map::new();
    for (i, n) in c.board.nets.iter().enumerate() {
        let cls = &c.rules.class_of(i).name;
        if !a.json() {
            println!("{}\t{}", n, cls);
        }
        if !n.is_empty() {
            map.insert(n.clone(), json!(cls));
        }
    }
    Ok(json!({ "net_class": map }))
}

fn cmd_check(a: &Args) -> CmdResult {
    let c = load_ctx(a)?;
    let mode = if a.s("copper", "locked") == "all" {
        model::CopperMode::All
    } else {
        model::CopperMode::LockedOnly
    };
    let m = model::Model::new(&c.board, &c.rules, c.cfg.clone(), mode);
    let out = a.s("out", "pr_check");
    std::fs::create_dir_all(&out).ok();
    let res = a.f("res", 0.25);
    let gs = geom::GridSpec::new(c.board.bbox, res);
    let t = Instant::now();
    let reps = sites::class_reps(&m);
    let sm = sites::build(&m, gs, &reps);
    eprintln!(
        "[sites] {} classes on {}x{} grid in {:.2} s",
        sm.cls.len(),
        gs.nx,
        gs.ny,
        t.elapsed().as_secs_f64()
    );
    // region report
    let regions = check::sheet_regions(&c.board);
    let mut rep_regions = serde_json::Map::new();
    for (name, (bb, idx)) in &regions {
        // class of the region: most common routable class among its nets
        let mut cnt: HashMap<usize, usize> = HashMap::new();
        for &fi in idx {
            for pd in &c.board.fps[fi].pads {
                if m.routable[pd.net] {
                    *cnt.entry(c.rules.net_class[pd.net]).or_default() += 1;
                }
            }
        }
        let Some((&cls, _)) = cnt.iter().max_by_key(|(_, n)| **n) else {
            continue;
        };
        let Some(cs) = sm.for_class(cls) else {
            continue;
        };
        let mut v = check::region_sites(&gs, &cs.via, bb);
        v["class"] = json!(c.rules.classes[cls].name);
        v["bbox"] = json!([bb.x0, bb.y0, bb.x1, bb.y1].map(check::round2));
        rep_regions.insert(name.clone(), v);
    }
    // what-if ablation: via sites per region with one blocker category removed (tells the AI what to change)
    let mut whatif = serde_json::Map::new();
    if a.get("no-whatif").is_none() {
        type Filter = fn(&model::Obj) -> Option<model::Obj>;
        let cats: [(&str, Filter); 5] = [
            ("no_inner_locked_zones", |o| {
                if matches!(o.kind, model::Kind::Zone) {
                    let mut o2 = o.clone();
                    o2.layers &= 0b100001;
                    if o2.layers == 0 {
                        None
                    } else {
                        Some(o2)
                    }
                } else {
                    Some(o.clone())
                }
            }),
            ("no_via_keepout_courtyards", |o| {
                if matches!(o.kind, model::Kind::ViaKeepout) {
                    None
                } else {
                    Some(o.clone())
                }
            }),
            ("no_locked_tracks_vias", |o| {
                if matches!(o.kind, model::Kind::Track | model::Kind::Via) {
                    None
                } else {
                    Some(o.clone())
                }
            }),
            ("no_rule_areas", |o| {
                if matches!(o.kind, model::Kind::Keepout { .. }) {
                    None
                } else {
                    Some(o.clone())
                }
            }),
            ("no_lanes_no_courtyards", |o| {
                if matches!(o.kind, model::Kind::ViaKeepout) {
                    return None;
                }
                if matches!(o.kind, model::Kind::Zone) {
                    let mut o2 = o.clone();
                    o2.layers &= 0b100001;
                    if o2.layers == 0 {
                        None
                    } else {
                        Some(o2)
                    }
                } else {
                    Some(o.clone())
                }
            }),
        ];
        use rayon::prelude::*;
        let gate_regions: Vec<(&String, &geom::BBox, usize)> = regions
            .iter()
            .filter_map(|(n, (bb, _))| {
                let cls = rep_regions.get(n)?.get("class")?.as_str()?;
                Some((n, bb, *c.rules.class_idx.get(cls)?))
            })
            .collect();
        let rows: Vec<(String, serde_json::Value)> = cats
            .par_iter()
            .map(|(cname, f)| {
                let objs: Vec<model::Obj> = m.objs.iter().filter_map(f).collect();
                let mut per = serde_json::Map::new();
                for (n, bb, ci) in &gate_regions {
                    let Some(cs) = sm.for_class(*ci) else {
                        continue;
                    };
                    let vm = sites::via_map(&m, &gs, cs.rep_net, cs.via_r, cs.drill_r, &objs);
                    let v = check::region_sites(&gs, &vm, bb);
                    per.insert((*n).clone(), json!(v["free_sites"]));
                }
                (cname.to_string(), serde_json::Value::Object(per))
            })
            .collect();
        for (k, v) in rows {
            whatif.insert(k, v);
        }
    }
    let report = json!({
        "whatif_via_sites": whatif,
        "pcb": c.pcb,
        "grid_mm": res,
        "copper": a.s("copper", "locked"),
        "classes": sm.cls.iter().map(|cs| {
            let free = cs.via.iter().filter(|v| **v == sites::R_FREE).count();
            json!({"class": c.rules.classes[cs.class].name, "rep_net": c.board.nets[cs.rep_net], "via_d": cs.via_r*2.0, "track_w": cs.track_hw*2.0,
                   "via_sites": free, "via_site_area_mm2": free as f64 * res * res})
        }).collect::<Vec<_>>(),
        "regions": rep_regions,
    });
    write_file(
        &format!("{out}/report.json"),
        &serde_json::to_string_pretty(&report).unwrap_or_default(),
    )?;
    let mut summary = json!({"out_dir": out, "classes": report["classes"], "regions": report["regions"].as_object().map(|o| o.len()).unwrap_or(0)});
    // images: via sites for selected classes
    let t = Instant::now();
    // --img-classes A,B (default: every class with routable nets)
    let want = a.get("img-classes");
    for cs in &sm.cls {
        let name = &c.rules.classes[cs.class].name;
        if want.is_some_and(|w| !w.split(',').any(|w| w == name)) {
            continue;
        }
        let cv = check::layer_image(&m, gs, None, Some(&cs.via), None);
        cv.save(&format!("{out}/via_sites_{name}.png"), 2)
            .map_err(|e| format!("png: {e}"))?;
    }
    // high-resolution crops: --crop x0,y0,x1,y1 [--crop-class Default] [--img-res 0.05]
    if let Some(cr) = a.get("crop") {
        let v: Vec<f64> = cr.split(',').filter_map(|x| x.parse().ok()).collect();
        let bb = geom::BBox {
            x0: v[0],
            y0: v[1],
            x1: v[2],
            y1: v[3],
        };
        let cgs = geom::GridSpec::new(bb, a.f("img-res", 0.05));
        let cname = a.s("crop-class", "Default");
        if let Some(&ci) = c.rules.class_idx.get(&cname) {
            if let Some(cs) = sm.for_class(ci) {
                let vm = sites::via_map(&m, &cgs, cs.rep_net, cs.via_r, cs.drill_r, &m.objs);
                check::layer_image(&m, cgs, None, Some(&vm), None)
                    .save(&format!("{out}/crop_via_{cname}.png"), 1)
                    .map_err(|e| format!("png: {e}"))?;
                for l in 0..board::NL {
                    let tm = sites::track_map(&m, &cgs, cs.rep_net, cs.track_hw, l, &m.objs);
                    check::layer_image(&m, cgs, Some(l), Some(&tm), None)
                        .save(&format!("{out}/crop_track_{cname}_{}.png", board::CU[l]), 1)
                        .map_err(|e| format!("png: {e}"))?;
                }
            }
        }
    }
    eprintln!("[img] {:.2} s", t.elapsed().as_secs_f64());
    // global routing / congestion
    if a.get("no-groute").is_none() {
        let t = Instant::now();
        let pins = model::pins_by_net(&m);
        let gp = groute::GParams {
            tile: a.f("tile", 1.0),
            iters: a.u("iters", 12),
            ..Default::default()
        };
        let gr = groute::route(&m, &sm, &pins, &gp);
        let util = groute::tile_util(&gr);
        let hs = groute::hotspots(&m, &gr);
        eprintln!(
            "[groute] {} connections, {} failed, {} hotspots in {:.2} s",
            gr.conns.len(),
            gr.failed.len(),
            hs.len(),
            t.elapsed().as_secs_f64()
        );
        let tgs = geom::GridSpec {
            x0: gr.x0,
            y0: gr.y0,
            res: gr.tile,
            nx: gr.tx,
            ny: gr.ty,
        };
        // per layer heat images on the fine grid
        for (l, ul) in util.iter().enumerate().take(board::NL) {
            let mut hm = vec![0f64; gs.n()];
            for iy in 0..gs.ny {
                for ix in 0..gs.nx {
                    if let Some((tx, ty)) = tgs.cell_of(gs.center(ix, iy)) {
                        hm[gs.idx(ix, iy)] = ul[ty * gr.tx + tx];
                    }
                }
            }
            let mut cv = check::layer_image(&m, gs, Some(l), None, Some(&hm));
            for h in &hs {
                let b = h.bbox;
                if h.id <= 25 && h.layers.iter().any(|x| x == board::CU[l]) {
                    cv.outline(
                        &[
                            geom::p(b[0], b[1]),
                            geom::p(b[2], b[1]),
                            geom::p(b[2], b[3]),
                            geom::p(b[0], b[3]),
                        ],
                        [255, 255, 255],
                    );
                    cv.text(
                        geom::p(b[0], b[1] - 2.0),
                        &format!("{}", h.id),
                        [255, 255, 255],
                        2,
                    );
                }
            }
            cv.save(&format!("{out}/congestion_{}.png", board::CU[l]), 2)
                .map_err(|e| format!("png: {e}"))?;
        }
        let failed: Vec<serde_json::Value> = gr.failed.iter().map(|&ci| {
            let cn = &gr.conns[ci];
            json!({"net": c.board.nets[cn.net], "from": [cn.pa.x, cn.pa.y], "to": [cn.pb.x, cn.pb.y]})
        }).collect();
        let layer_sum: Vec<serde_json::Value> = (0..board::NL).map(|l| {
            let ovf_h: f32 = gr.h_used[l].iter().zip(&gr.h_cap[l]).map(|(u, c)| (u - c).max(0.0)).sum();
            let ovf_v: f32 = gr.v_used[l].iter().zip(&gr.v_cap[l]).map(|(u, c)| (u - c).max(0.0)).sum();
            let used: f32 = gr.h_used[l].iter().sum::<f32>() + gr.v_used[l].iter().sum::<f32>();
            json!({"layer": board::CU[l], "overflow_mm": check::round2((ovf_h + ovf_v) as f64), "used_mm_pitch": check::round2(used as f64)})
        }).collect();
        let vias: f32 = gr.via_used.iter().sum();
        let g = json!({
            "tile_mm": gr.tile, "connections": gr.conns.len(), "failed": failed, "overflow_per_iter": gr.overflow_hist,
            "layers": layer_sum, "vias": vias, "hotspots": hs,
        });
        write_file(
            &format!("{out}/groute.json"),
            &serde_json::to_string_pretty(&g).unwrap_or_default(),
        )?;
        // compact per-tile utilisation (percent, u8) for the AI / viewer
        let cong: Vec<Vec<u8>> = util
            .iter()
            .map(|u| {
                u.iter()
                    .map(|v| (v * 100.0).round().min(255.0) as u8)
                    .collect()
            })
            .collect();
        write_file(&format!("{out}/congestion.json"), &json!({"tx": gr.tx, "ty": gr.ty, "tile": gr.tile, "x0": gr.x0, "y0": gr.y0, "layers": board::CU, "util_pct": cong}).to_string())?;
        summary["groute"] = json!({"failed": g["failed"].as_array().map(|v| v.len()), "layers": g["layers"], "vias": vias, "hotspots_top": hs.iter().take(8).map(|h| json!({"id": h.id, "bbox": h.bbox, "ovf": h.overflow_mm, "vias_over": h.vias_over, "layers": h.layers, "nets": h.nets.iter().take(4).collect::<Vec<_>>()})).collect::<Vec<_>>()});
    }
    Ok(summary)
}

/// place --pcb B --zones z.json [--sch DIR] [--moves N] [--starts K] [--out DIR]
/// writes place.json (ref, x, y, rot, side, old, why) for apply_place.py and terms before/after
fn cmd_place(a: &Args) -> CmdResult {
    let c = load_ctx(a)?;
    let zpath = a
        .get("zones")
        .ok_or("missing --zones zones.json (see examples/zones.json)")?;
    let ztext = std::fs::read_to_string(zpath).map_err(|e| format!("{zpath}: {e}"))?;
    let zf: place::ZonesFile = serde_json::from_str(&ztext).map_err(|e| format!("{zpath}: {e}"))?;
    let m = model::Model::new(
        &c.board,
        &c.rules,
        c.cfg.clone(),
        model::CopperMode::LockedOnly,
    );
    let t = Instant::now();
    let pb = place::Problem::new(&m, &zf)?;
    eprintln!(
        "[place] {} movable parts, {} nets, {} fixed objects, setup {:.2} s",
        pb.parts.len(),
        pb.nets.len(),
        pb.fixed.len(),
        t.elapsed().as_secs_f64()
    );
    let cur: Vec<place::Pose> = pb
        .parts
        .iter()
        .map(|s| {
            let f = &c.board.fps[s.fi];
            place::Pose {
                at: f.at,
                rot: f.rot,
                back: f.back,
            }
        })
        .collect();
    let s0 = pb.init_state(cur.clone());
    let t0 = pb.terms(&s0);
    eprintln!("[place] start: {:?}", t0);
    eprintln!(
        "[place] via sites with all movable parts removed (upper bound): {}",
        pb.base_count.iter().filter(|c| **c == 0).count()
    );
    let mut nc: Vec<(f64, usize)> = s0.net_c.iter().map(|(n, c)| (*c, *n)).collect();
    nc.sort_by(|a, b| b.0.total_cmp(&a.0));
    for (cst, n) in nc.iter().take(6) {
        eprintln!(
            "[place]   net cost {:8.1}  {}  maxlen {:?}",
            cst,
            c.board.nets[*n],
            pb.net_maxlen.get(n)
        );
    }
    let init_mode = a.s("init", zf.init.as_deref().unwrap_or("keep"));
    let mut inits = Vec::new();
    if init_mode == "keep" || init_mode == "both" {
        inits.push(s0.clone());
    }
    if init_mode == "schematic" || init_mode == "both" {
        let pdir = std::path::Path::new(&c.pcb)
            .parent()
            .map(|d| d.to_string_lossy().to_string())
            .unwrap_or_default();
        let dir = a.s("sch", if pdir.is_empty() { "." } else { &pdir });
        let sch = sch::load_dir(&dir);
        let s = pb.init_state(place::schematic_init(&pb, &sch));
        eprintln!(
            "[place] schematic init ({} symbols): {:?}",
            sch.len(),
            pb.terms(&s)
        );
        inits.push(s);
    }
    let moves = a.u("moves", zf.moves.unwrap_or(20000));
    let starts = a.u("starts", rayon::current_num_threads());
    let t = Instant::now();
    let (best, traces) = place::optimize(
        &pb,
        &inits,
        moves,
        zf.seed.unwrap_or(1),
        starts,
        a.f("t0", 1.0),
    );
    let t1 = pb.terms(&best);
    let dt = t.elapsed().as_secs_f64();
    eprintln!(
        "[place] best: {:?}  ({} starts x {} moves in {:.2} s, {:.0} moves/s)",
        t1,
        starts,
        moves,
        dt,
        (starts * moves) as f64 / dt
    );
    // only accept if violations did not increase vs the current board
    let out = a.s("out", "pr_place");
    std::fs::create_dir_all(&out).ok();
    let mut rows = Vec::new();
    for (k, s) in pb.parts.iter().enumerate() {
        let f = &c.board.fps[s.fi];
        let ps = best.poses[k];
        if (ps.at - f.at).len() < 1e-6 && (ps.rot - f.rot).abs() < 1e-6 && ps.back == f.back {
            continue;
        }
        let why = zf
            .zones
            .iter()
            .find(|z| z.refs.iter().any(|pt| place::ref_match(f, pt)))
            .map(|z| z.why.clone())
            .unwrap_or_default();
        rows.push(json!({"ref": f.refd, "x": geom_r(ps.at.x), "y": geom_r(ps.at.y), "rot": geom_r(ps.rot), "side": if ps.back {"B"} else {"F"},
            "old": [geom_r(f.at.x), geom_r(f.at.y), geom_r(f.rot), if f.back {"B"} else {"F"}], "why": format!("copperpath SA: {why}")}));
    }
    write_file(
        &format!("{out}/place.json"),
        &serde_json::to_string_pretty(&rows).unwrap_or_default(),
    )?;
    write_file(&format!("{out}/place_report.json"), &serde_json::to_string_pretty(&json!({"start": t0, "best": t1, "moved": rows.len(),
        "parts": pb.parts.len(), "moves": moves, "starts": starts, "seconds": dt, "traces": traces})).unwrap_or_default())?;
    eprintln!("[place] {} parts moved -> {out}/place.json", rows.len());
    Ok(
        json!({"out_dir": out, "parts": pb.parts.len(), "moved": rows.len(), "start": t0, "best": t1, "moves": moves, "starts": starts, "search_s": check::round2(dt)}),
    )
}

fn geom_r(v: f64) -> f64 {
    (v * 10000.0).round() / 10000.0
}

fn parse_box(s: &str) -> Result<geom::BBox, String> {
    let v: Vec<f64> = s
        .split(',')
        .map(|x| x.trim().parse::<f64>())
        .collect::<Result<_, _>>()
        .map_err(|_| format!("bad box '{s}': expected x0,y0,x1,y1 in mm"))?;
    if v.len() != 4 || v[2] <= v[0] || v[3] <= v[1] {
        return Err(format!(
            "bad box '{s}': expected x0,y0,x1,y1 with x1 > x0 and y1 > y0"
        ));
    }
    Ok(geom::BBox {
        x0: v[0],
        y0: v[1],
        x1: v[2],
        y1: v[3],
    })
}

/// nets matching --nets patterns (comma separated, wildcards), None = all
fn net_filter(a: &Args, nets: &[String]) -> Option<std::collections::HashSet<usize>> {
    a.get("nets").map(|pats| {
        let pats: Vec<&str> = pats.split(',').collect();
        (0..nets.len())
            .filter(|i| pats.iter().any(|pt| rules::wild(&nets[*i], pt)))
            .collect()
    })
}

/// hints --pcb B [--region x0,y0,x1,y1] [--nets pats] [--tol 1.0]: placement hints for clean routing.
/// Per net a Euclidean MST of its pads (the likely connections); an edge whose pads are almost but not
/// exactly in one row or column (perpendicular offset 0.02 .. tol mm, run >= 0.5 mm) forces a jog or
/// staircase between two straight exit stubs -> align the pads before routing.
fn cmd_hints(a: &Args) -> CmdResult {
    let c = load_ctx(a)?;
    let region = a.get("region").map(parse_box).transpose()?;
    let only = net_filter(a, &c.board.nets);
    let tol: f64 = a.s("tol", "1.0").parse().unwrap_or(1.0);
    let mut by_net: std::collections::BTreeMap<usize, Vec<(String, geom::P)>> = Default::default();
    for f in &c.board.fps {
        for pd in &f.pads {
            if pd.net == 0 || only.as_ref().is_some_and(|o| !o.contains(&pd.net)) {
                continue;
            }
            let (q, _) = f.pad_world(pd);
            if region.is_some_and(|r| q.x < r.x0 || q.x > r.x1 || q.y < r.y0 || q.y > r.y1) {
                continue;
            }
            by_net
                .entry(pd.net)
                .or_default()
                .push((format!("{}.{}", f.refd, pd.num), q));
        }
    }
    let mut hints = Vec::new();
    if !a.json() {
        println!("net\tfrom\tto\taxis\toffset_mm\trun_mm");
    }
    for (net, pins) in &by_net {
        let n = pins.len();
        if !(2..=40).contains(&n) {
            continue;
        }
        // Prim MST
        let mut inq = vec![false; n];
        let mut best = vec![(f64::INFINITY, 0usize); n];
        best[0].0 = 0.0;
        for _ in 0..n {
            let Some(u) = (0..n)
                .filter(|i| !inq[*i])
                .min_by(|x, y| best[*x].0.total_cmp(&best[*y].0))
            else {
                break;
            };
            inq[u] = true;
            if u != 0 {
                let v = best[u].1;
                let d = pins[u].1 - pins[v].1;
                let (ax, ay) = (d.x.abs(), d.y.abs());
                let (off, run, axis) = if ax < ay {
                    (ax, ay, "x")
                } else {
                    (ay, ax, "y")
                };
                if off > 0.02 && off < tol && run >= 0.5 {
                    if !a.json() {
                        println!(
                            "{}\t{}\t{}\t{}\t{:.3}\t{:.3}",
                            c.board.nets[*net], pins[v].0, pins[u].0, axis, off, run
                        );
                    }
                    hints.push(json!({"net": c.board.nets[*net], "from": pins[v].0, "to": pins[u].0, "axis": axis, "offset_mm": geom_r(off), "run_mm": geom_r(run)}));
                }
            }
            for w in 0..n {
                if !inq[w] {
                    let d = (pins[w].1 - pins[u].1).len();
                    if d < best[w].0 {
                        best[w] = (d, u);
                    }
                }
            }
        }
    }
    eprintln!(
        "[hints] {} near-collinear pad pairs (offset 0.02..{} mm): align them in placement",
        hints.len(),
        tol
    );
    Ok(json!({"tol_mm": tol, "count": hints.len(), "hints": hints}))
}

/// route, then (with --reroute-blockers) rip the unlocked copper of other nets that blocks each
/// net left open and route it again together with the ripped nets; a try that leaves any of
/// these nets worse than before is rolled back
fn cmd_route(a: &Args) -> CmdResult {
    let mut summary = route_once(a)?;
    if a.get("reroute-blockers").is_none() || summary["opens"].as_u64().unwrap_or(0) == 0 {
        return Ok(summary);
    }
    let out = summary["out"].as_str().unwrap_or_default().to_string();
    let c0 = load_ctx(a)?;
    let region = match a.get("region") {
        Some(s) => parse_box(s)?,
        None => c0.board.bbox,
    };
    let max_nets = a.u("max-ripped", 3);
    let max_targets = a.u("max-targets", 10);
    // rules of the input board for every later step (the copies have other base names)
    let mut base_kv = a.kv.clone();
    for k in ["reroute-blockers", "drc", "json", "dbg-net"] {
        base_kv.remove(k);
    }
    if !c0.pro.is_empty() {
        base_kv.insert("pro".into(), c0.pro.clone());
    }
    if std::path::Path::new(&c0.dru).is_file() {
        base_kv.insert("dru".into(), c0.dru.clone());
    }
    let load_out = |path: &str| -> Result<Ctx, String> {
        let mut kv = base_kv.clone();
        kv.insert("pcb".into(), path.to_string());
        load_ctx(&Args {
            cmd: "route".into(),
            kv,
        })
    };
    let per_net = |c: &Ctx| -> HashMap<String, usize> {
        let all: std::collections::HashSet<usize> = (1..c.board.nets.len()).collect();
        count_opens(c, Some(region), Some(&all))
            .1
            .into_iter()
            .map(|(n, k, _)| (n, k))
            .collect()
    };
    // only the nets the user asked to route are targets
    let user_filt: Option<std::collections::HashSet<String>> = net_filter(a, &c0.board.nets)
        .map(|f| f.iter().map(|i| c0.board.nets[*i].clone()).collect());
    let mut log = Vec::new();
    let mut tried: std::collections::HashSet<String> = Default::default();
    for _ in 0..max_targets {
        let c = load_out(&out)?;
        let before = per_net(&c);
        let mut targets: Vec<(&String, &usize)> = before
            .iter()
            .filter(|(n, _)| {
                !tried.contains(*n) && user_filt.as_ref().is_none_or(|f| f.contains(*n))
            })
            .collect();
        targets.sort();
        let Some((target, t_before)) = targets.first().map(|(n, k)| ((*n).clone(), **k)) else {
            break;
        };
        tried.insert(target.clone());
        let tnet = c.board.net_id[&target];
        let m = model::Model::new(&c.board, &c.rules, c.cfg.clone(), model::CopperMode::All);
        let bp = reroute::BlockerParams {
            max_nets,
            margin_mm: a.f("job-margin", 6.0),
            ..Default::default()
        };
        let Some(bl) = reroute::find_blockers(&m, tnet, region, &bp) else {
            eprintln!("[reroute] {target}: no corridor even through unlocked copper");
            log.push(json!({"net": target, "accepted": false, "reason": "no corridor through unlocked copper (locked copper, pads or edge block it)"}));
            continue;
        };
        if bl.nets.is_empty() || bl.uuids.is_empty() {
            eprintln!(
                "[reroute] {target}: corridor of {} mm has no rippable blocker",
                bl.length_mm
            );
            log.push(json!({"net": target, "accepted": false, "reason": "the cheapest corridor is not blocked by unlocked copper of other nets"}));
            continue;
        }
        let ripped: Vec<String> = bl.nets.iter().map(|n| c.board.nets[*n].clone()).collect();
        eprintln!(
            "[reroute] {target}: ripping {} items of {:?} on a {} mm corridor",
            bl.uuids.len(),
            ripped,
            bl.length_mm
        );
        let (text, cut) = reroute::remove_items(&c.board.text, &bl.uuids);
        let rip = out.replace(".kicad_pcb", "_rip.kicad_pcb");
        let tryo = out.replace(".kicad_pcb", "_try.kicad_pcb");
        write_file(&rip, &text)?;
        let mut kv = base_kv.clone();
        kv.insert("pcb".into(), rip.clone());
        kv.insert("out".into(), tryo.clone());
        // exact names as patterns (wildcards in net names are taken literally enough here)
        let mut nets = vec![target.clone()];
        nets.extend(ripped.iter().cloned());
        kv.insert("nets".into(), nets.join(","));
        let res = route_once(&Args {
            cmd: "route".into(),
            kv,
        });
        let after = match &res {
            Ok(_) => per_net(&load_out(&tryo)?),
            Err(_) => before.clone(),
        };
        let worse: Vec<&String> = nets
            .iter()
            .filter(|n| after.get(*n).copied().unwrap_or(0) > before.get(*n).copied().unwrap_or(0))
            .collect();
        let t_after = after.get(&target).copied().unwrap_or(0);
        let total = |h: &HashMap<String, usize>| h.values().sum::<usize>();
        let ok =
            res.is_ok() && worse.is_empty() && t_after < t_before && total(&after) < total(&before);
        let cleanup = |p: &str| {
            for ext in [".kicad_pcb", "_route.json", ".kicad_pro", ".kicad_dru"] {
                let _ = std::fs::remove_file(p.replace(".kicad_pcb", ext));
            }
        };
        if ok {
            std::fs::rename(&tryo, &out).map_err(|e| format!("{out}: {e}"))?;
        }
        cleanup(&tryo);
        cleanup(&rip);
        eprintln!(
            "[reroute] {target}: {} (opens of the net {t_before} -> {t_after}, board {} -> {})",
            if ok { "accepted" } else { "rolled back" },
            total(&before),
            total(&after)
        );
        log.push(json!({
            "net": target, "ripped_nets": ripped, "ripped_items": cut,
            "corridor_mm": bl.length_mm, "accepted": ok,
            "opens_before": t_before, "opens_after": t_after,
            "board_opens_before": total(&before), "board_opens_after": total(&after),
            "worse": worse,
            "reason": if ok { "".to_string() } else if let Err(e) = &res { e.clone() } else if !worse.is_empty() { "a ripped net could not be restored".into() } else { "the target is still open".into() },
        }));
    }
    // final state of the written board
    let c = load_out(&out)?;
    let filt = net_filter(a, &c.board.nets);
    let (opens, open_nets) = count_opens(&c, Some(region), filt.as_ref());
    summary["opens"] = json!(opens);
    summary["open_nets"] = json!(open_nets
        .iter()
        .map(|(n, k, _)| json!({"net": n, "opens": k}))
        .collect::<Vec<_>>());
    summary["reroute"] = json!(log);
    if a.get("drc").is_some() {
        summary["drc"] = drc_delta(a, &c0.pcb, &out)?;
    }
    write_file(
        &out.replace(".kicad_pcb", "_route.json"),
        &serde_json::to_string_pretty(&summary).unwrap_or_default(),
    )?;
    eprintln!("[reroute] {opens} opens -> {out}");
    Ok(summary)
}

/// route --pcb B [--region x0,y0,x1,y1] [--nets pats] [--res 0.05] [--drc] --out OUT.kicad_pcb
fn route_once(a: &Args) -> CmdResult {
    let c = load_ctx(a)?;
    let region = match a.get("region") {
        Some(s) => parse_box(s)?,
        None => {
            // whole board: the outline's bounding box
            let b = c.board.bbox;
            if b.x1 <= b.x0 || b.y1 <= b.y0 {
                return Err("the board has no Edge.Cuts outline: give --region x0,y0,x1,y1".into());
            }
            b
        }
    };
    let out = a.s("out", "routed.kicad_pcb");
    if !out.ends_with(".kicad_pcb") {
        return Err(format!(
            "--out {out}: the output file must end in .kicad_pcb"
        ));
    }
    if std::path::Path::new(&out).canonicalize().ok()
        == std::path::Path::new(&c.pcb).canonicalize().ok()
    {
        return Err("--out must differ from --pcb (the input board is never overwritten)".into());
    }
    // all existing copper stays in the output, so all of it is an obstacle for other nets (and
    // connectivity for its own net); --ignore-unlocked restores the old "locked only" view
    let mode = if a.get("ignore-unlocked").is_some() {
        model::CopperMode::LockedOnly
    } else {
        model::CopperMode::All
    };
    let m = model::Model::new(&c.board, &c.rules, c.cfg.clone(), mode);
    if !c.cfg.class_layers.is_empty() {
        for cl in &c.rules.classes {
            if let Some(mk) = model::class_layer_mask(&c.cfg.class_layers, &cl.name) {
                let ls: Vec<&str> = (0..6)
                    .filter(|l| mk & c.board.cu_mask & (1 << l) != 0)
                    .map(|l| board::CU[l])
                    .collect();
                eprintln!("[route] class {} layers {}", cl.name, ls.join(","));
            }
        }
    }
    let mut p = droute::DParams::default();
    p.res = a.f("res", p.res);
    p.via_cost = a.f("via-cost", p.via_cost as f64) as f32;
    p.hweight = a.f("hweight", p.hweight as f64) as f32;
    p.max_expand = a.f("max-expand", p.max_expand as f64) as u64;
    p.job_margin = a.f("job-margin", p.job_margin);
    p.nego_iters = a.f("iters", p.nego_iters as f64) as usize;
    p.par_nets = a.u("par-nets", p.par_nets);
    let t_start = Instant::now();
    let limit = a.f("time-limit", 0.0);
    if limit > 0.0 {
        p.deadline = Some(t_start + std::time::Duration::from_secs_f64(limit));
    }
    droute::install_stop_handler();
    watch_cancel(a.get("cancel-file"), a.get("cancel-on-stdin-eof").is_some());
    p.stall_iters = a.u("stall-iters", p.stall_iters);
    p.par_conn = a.u("par-conn", p.par_conn);
    p.bend_cost = a.f("bend-cost", p.bend_cost as f64) as f32;
    p.bend90 = a.f("bend90", p.bend90 as f64) as f32;
    p.stub_ext = a.f("stub-ext", p.stub_ext);
    p.stub_ext_long = a.f("stub-ext-long", p.stub_ext_long);
    p.own_pad_cost = a.f("own-pad-cost", p.own_pad_cost as f64) as f32;
    p.stub_side_cost = a.f("stub-side-cost", p.stub_side_cost as f64) as f32;
    p.stubs = a.get("no-stubs").is_none();
    p.clean = a.get("no-clean").is_none();
    let filt = net_filter(a, &c.board.nets);
    let t = Instant::now();
    let mut r = droute::Router::new(&m, region, p.clone(), filt.as_ref());
    let mut res = r.run();
    // fallback ladder when the cleanup's geometry costs completeness: no sliding, then no cleanup
    // (completeness first; the level used is reported)
    let mut level = "full";
    let halted = || {
        droute::STOP.load(std::sync::atomic::Ordering::Relaxed)
            || p.deadline.is_some_and(|d| Instant::now() > d)
    };
    let stopped = if CANCELLED.load(std::sync::atomic::Ordering::Relaxed) {
        Some("cancel")
    } else if droute::STOP.load(std::sync::atomic::Ordering::Relaxed) {
        Some("signal")
    } else if halted() {
        Some("time-limit")
    } else {
        None
    };
    if !res.failed.is_empty() && p.clean && !halted() {
        // both fallback levels run at the same time (a route mostly uses one core); the choice
        // is the same as trying them one after the other
        let run_level = |slide: bool, clean: bool| {
            let mut p2 = p.clone();
            p2.slide = slide;
            p2.clean = clean;
            let r2 = droute::Router::new(&m, region, p2, filt.as_ref());
            let res2 = r2.run();
            (r2, res2)
        };
        let (a, b) = rayon::join(|| run_level(false, true), || run_level(false, false));
        for (lv, (r2, res2)) in [("no-slide", a), ("no-clean", b)] {
            if res.failed.is_empty() || halted() {
                break;
            }
            eprintln!(
                "[route] fallback {}: {} failed nets (was {})",
                lv,
                res2.failed.len(),
                res.failed.len()
            );
            if res2.failed.len() < res.failed.len() {
                r = r2;
                res = res2;
                level = lv;
            }
        }
    }
    if let Some(why) = stopped {
        eprintln!("[route] stopped early ({why}): writing the best result so far");
    }
    eprintln!("[route] cleanup level {}", level);
    let dt = t.elapsed().as_secs_f64();
    let nseg: usize = res.routed.values().map(|x| x.segs.len()).sum();
    let nvia: usize = res.routed.values().map(|x| x.vias.len()).sum();
    let len: f64 = res
        .routed
        .values()
        .flat_map(|x| x.segs.iter())
        .map(|s| (s.b - s.a).len())
        .sum();
    eprintln!("[route] {} nets, {} connections: {} segments ({:.1} mm), {} vias, {} routes, {} rips, {:.1} M nodes expanded, search {:.2} s",
        r.jobs.len(), res.conns, nseg, len, nvia, res.routes, res.rips, res.expanded as f64 * 1e-6, dt);
    let s = &res.stats;
    eprintln!("[aes] pins {} without legal stub {} {:?}; stub ends {}, short stubs {:?}, fallback pad ends {}; stub alignments {} (failed {}), pull-tights {}, mitered {} (kept 90: {} = clearance/L-stubs/shift-blocked/Z-U-short/short-legs {:?}, stub-end shifts {}), exact-check fails {}",
        s.pins, s.pins_no_stub.len(), s.pins_no_stub, s.stubs_used, s.short_stubs, s.fallback_ends, s.aligned, s.align_failed, s.pulls, s.mitered, s.miter_failed, s.kept90, s.shifted, s.exact_fail);
    let mut warnings: Vec<String> = Vec::new();
    for ((n, left), why) in res.failed.iter().zip(&res.why) {
        eprintln!(
            "[route]   FAILED {} ({} components left): {}",
            c.board.nets[*n], left, why
        );
        warnings.push(format!(
            "net {} not completed ({} components left): {}",
            c.board.nets[*n], left, why
        ));
    }
    if level != "full" {
        warnings.push(format!(
            "cleanup fallback level '{level}' was needed for completeness"
        ));
    }
    if !s.pins_no_stub.is_empty() {
        warnings.push(format!(
            "{} pins without a legal straight exit stub",
            s.pins_no_stub.len()
        ));
    }
    if s.exact_fail > 0 {
        warnings.push(format!(
            "{} exact-check failures in the cleanup",
            s.exact_fail
        ));
    }
    if let Some(dn) = a.get("dbg-net") {
        if let Some(n) = c.board.net_id.get(dn) {
            let d = std::path::Path::new(&out)
                .parent()
                .map(|d| d.to_string_lossy().to_string())
                .unwrap_or_default();
            r.debug_images(*n, &res, if d.is_empty() { "." } else { &d });
        }
    }
    write_file(&out, "")?; // creates the output directory
    droute::write_pcb(&c.board.text, &c.board.nets, &res, &out)
        .map_err(|e| format!("{out}: {e}"))?;
    for (src, ext) in [(&c.pro, "kicad_pro"), (&c.dru, "kicad_dru")] {
        if std::path::Path::new(src).is_file() {
            std::fs::copy(src, out.replace(".kicad_pcb", &format!(".{ext}")))
                .map_err(|e| format!("{src}: {e}"))?;
        }
    }
    // opens counted on the written board (all copper, routed region and nets only)
    let mut a2 = Args {
        cmd: a.cmd.clone(),
        kv: a.kv.clone(),
    };
    a2.kv.insert("pcb".into(), out.clone());
    a2.kv.remove("pro");
    a2.kv.remove("dru");
    let c2 = load_ctx(&a2)?;
    let (opens, open_nets) = count_opens(&c2, Some(region), filt.as_ref());
    let drc = if a.get("drc").is_some() {
        Some(drc_delta(a, &c.pcb, &out)?)
    } else {
        None
    };
    if let Some(d) = &drc {
        let (n, ne) = (
            d["new_violations"].as_u64().unwrap_or(0),
            d["new_errors"].as_u64().unwrap_or(0),
        );
        eprintln!(
            "[drc] kicad-cli: {} new violations ({} errors), unconnected {} -> {}",
            n, ne, d["unconnected_before"], d["unconnected_after"]
        );
        if n > 0 {
            warnings.push(format!("kicad-cli DRC: {n} new violations ({ne} errors)"));
        }
    }
    let summary = json!({
        "out": out, "nets": r.jobs.len(), "connections": res.conns, "segments": nseg, "vias": nvia, "length_mm": check::round2(len),
        "routes": res.routes, "rips": res.rips, "opens": opens,
        "open_nets": open_nets.iter().map(|(n, k, _)| json!({"net": n, "opens": k})).collect::<Vec<_>>(),
        "aes": {"cleanup_level": level, "pins": s.pins, "pins_no_stub": s.pins_no_stub, "stub_ends": s.stubs_used, "short_stubs": s.short_stubs, "fallback_ends": s.fallback_ends, "aligned": s.aligned, "align_failed": s.align_failed, "pulls": s.pulls, "mitered": s.mitered, "miter_kept_90": s.miter_failed, "stub_end_shifts": s.shifted, "axis_jogs": s.jogs, "kept90_by_cause": {"clearance": s.kept90[0], "L_between_stubs": s.kept90[1], "shift_blocked": s.kept90[2], "Z_U_too_short": s.kept90[3], "short_legs": s.kept90[4]}, "exact_fail": s.exact_fail},
        "drc": drc, "search_s": check::round2(dt),
        "failed": res.failed.iter().map(|(n, l)| json!([c.board.nets[*n], l])).collect::<Vec<_>>(),
        "warnings": warnings,
        "stopped": stopped,
        "hotspots": res.hotspots.iter().take(20).map(|h| json!({"net": c.board.nets[h.net], "cells": h.cells, "at": [check::round2(h.x), check::round2(h.y)], "layer": h.layer})).collect::<Vec<_>>(),
    });
    write_file(
        &out.replace(".kicad_pcb", "_route.json"),
        &serde_json::to_string_pretty(&summary).unwrap_or_default(),
    )?;
    eprintln!("[route] {opens} opens -> {out}");
    Ok(summary)
}

/// kicad-cli DRC on the input and the routed board; violations only the routed board has.
/// kicad-cli: --kicad-cli PATH, else $KICAD_CLI, else `kicad-cli` on the PATH.
fn drc_delta(a: &Args, before: &str, after: &str) -> Result<Value, String> {
    let cli = a
        .get("kicad-cli")
        .map(String::from)
        .or_else(|| std::env::var("KICAD_CLI").ok())
        .unwrap_or_else(|| "kicad-cli".into());
    let run = |pcb: &str, rep: &str| -> Result<Value, String> {
        let st = std::process::Command::new(&cli)
            .args([
                "pcb",
                "drc",
                "--format",
                "json",
                "--severity-all",
                "-o",
                rep,
                pcb,
            ])
            .output()
            .map_err(|e| {
                format!("--drc: cannot run '{cli}': {e} (set --kicad-cli or $KICAD_CLI)")
            })?;
        let txt = std::fs::read_to_string(rep).map_err(|_| {
            format!(
                "--drc: kicad-cli wrote no report for {pcb}: {}",
                String::from_utf8_lossy(&st.stderr).trim()
            )
        })?;
        serde_json::from_str(&txt).map_err(|e| format!("{rep}: {e}"))
    };
    let base = after.replace(".kicad_pcb", "_drc_before.json");
    let new = after.replace(".kicad_pcb", "_drc_after.json");
    let (b, n) = (run(before, &base)?, run(after, &new)?);
    let keys = |d: &Value| -> HashMap<String, usize> {
        let mut m = HashMap::new();
        for v in d["violations"].as_array().into_iter().flatten() {
            let mut items: Vec<String> = v["items"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|i| i["description"].as_str().unwrap_or("").to_string())
                .collect();
            items.sort();
            *m.entry(format!(
                "{} {} | {}",
                v["severity"].as_str().unwrap_or(""),
                v["type"].as_str().unwrap_or(""),
                items.join(" / ")
            ))
            .or_insert(0) += 1;
        }
        m
    };
    let (kb, kn) = (keys(&b), keys(&n));
    let mut added: Vec<String> = Vec::new();
    for (k, cnt) in &kn {
        for _ in kb.get(k).copied().unwrap_or(0)..*cnt {
            added.push(k.clone());
        }
    }
    added.sort();
    let unc = |d: &Value| {
        d["unconnected_items"]
            .as_array()
            .map(|v| v.len())
            .unwrap_or(0)
    };
    Ok(json!({
        "violations_before": kb.values().sum::<usize>(), "violations_after": kn.values().sum::<usize>(),
        "new_violations": added.len(), "new_errors": added.iter().filter(|k| k.starts_with("error ")).count(),
        "new": added.iter().take(50).collect::<Vec<_>>(),
        "unconnected_before": unc(&b), "unconnected_after": unc(&n), "reports": [base, new],
    }))
}

/// output path for a command that writes a board copy: must end in .kicad_pcb and differ from
/// the input
fn out_path(a: &Args, c: &Ctx, default: &str) -> Result<String, String> {
    let out = a.s("out", default);
    if !out.ends_with(".kicad_pcb") {
        return Err(format!(
            "--out {out}: the output file must end in .kicad_pcb"
        ));
    }
    if std::path::Path::new(&out).canonicalize().ok()
        == std::path::Path::new(&c.pcb).canonicalize().ok()
    {
        return Err("--out must differ from --pcb (the input board is never overwritten)".into());
    }
    Ok(out)
}

/// regex that must match a whole name
fn full_regex(a: &Args, k: &str) -> Result<regex::Regex, String> {
    let pat = a.get(k).ok_or(format!("missing --{k} REGEX"))?;
    regex::Regex::new(&format!("^(?:{pat})$")).map_err(|e| format!("--{k}: {e}"))
}

/// --via D,DRILL or the net class via
fn via_arg(a: &Args, c: &Ctx, net: usize) -> Result<(f64, f64), String> {
    match a.get("via") {
        Some(v) => {
            let mut it = v.split(',').map(|x| x.trim().parse::<f64>());
            match (it.next(), it.next()) {
                (Some(Ok(d)), Some(Ok(h))) => Ok((d, h)),
                _ => Err(format!("--via {v}: expected DIAMETER,DRILL in mm")),
            }
        }
        None => Ok(c.rules.via(net)),
    }
}

/// the board text new copper is appended to: --base or the input
fn base_text(a: &Args, c: &Ctx) -> Result<String, String> {
    match a.get("base") {
        Some(b) => std::fs::read_to_string(b).map_err(|e| format!("--base {b}: {e}")),
        None => Ok(c.board.text.clone()),
    }
}

/// fanout --pcb FILLED --net GND --plane REGEX [--reach 6] [--via D,DRILL] [--width W]
/// [--margin 0.05] [--min-clr 0] [--islands opens.json] [--base B] --out OUT.kicad_pcb
fn cmd_fanout(a: &Args) -> CmdResult {
    let c = load_ctx(a)?;
    let out = out_path(a, &c, "fanout.kicad_pcb")?;
    let net_name = a.s("net", "GND");
    let net = *c
        .board
        .net_id
        .get(&net_name)
        .ok_or(format!("--net {net_name}: no such net"))?;
    let (via_d, drill) = via_arg(a, &c, net)?;
    // --islands: `opens --json` output; only islands with one of its pads of this net
    let mut only = std::collections::HashSet::new();
    if let Some(f) = a.get("islands") {
        let v: Value = serde_json::from_str(
            &std::fs::read_to_string(f).map_err(|e| format!("--islands {f}: {e}"))?,
        )
        .map_err(|e| format!("--islands {f}: {e}"))?;
        for pn in v["per_net"].as_array().into_iter().flatten() {
            if pn["net"] == json!(net_name) {
                for isl in pn["islands"].as_array().into_iter().flatten() {
                    for pad in isl.as_array().into_iter().flatten() {
                        if let Some(s) = pad.as_str() {
                            only.insert(s.to_string());
                        }
                    }
                }
            }
        }
        if only.is_empty() {
            return Err(format!("--islands {f}: no islands of net {net_name}"));
        }
    }
    let p = fanout::FanoutParams {
        net,
        plane: full_regex(a, "plane")?,
        reach: a.f("reach", 6.0),
        via_d,
        drill,
        width: a.get("width").and_then(|w| w.parse().ok()),
        margin: a.f("margin", 0.05),
        min_clr: a.f("min-clr", 0.0),
        only,
        res: a.f("res", 0.05),
    };
    let m = model::Model::new(&c.board, &c.rules, c.cfg.clone(), model::CopperMode::All);
    let t = Instant::now();
    let r = fanout::fanout(&m, &p)?;
    let ok = r.islands.iter().filter(|i| i.pad.is_some()).count();
    eprintln!(
        "[fanout] {net_name}: {} open islands, {ok} connected, {} failed in {:.2} s",
        r.islands.len(),
        r.islands.len() - ok,
        t.elapsed().as_secs_f64()
    );
    for i in &r.islands {
        match &i.pad {
            Some(pd) => eprintln!(
                "[fanout]   {pd}: {:.2} mm on {} to via {:?}",
                i.length_mm,
                i.layer.as_deref().unwrap_or(""),
                i.via.unwrap_or_default()
            ),
            None => eprintln!(
                "[fanout]   failed {:?}: {:?}; nearest blocker {}",
                i.pads,
                i.why,
                i.blockers
                    .first()
                    .map(|b| format!(
                        "{} {} {} on {} at {:.2} mm (needs {:.2})",
                        b.net,
                        b.kind,
                        b.fp.as_deref().unwrap_or(""),
                        b.layer,
                        b.dist_mm,
                        b.need_mm
                    ))
                    .unwrap_or_else(|| "-".into())
            ),
        }
    }
    let mut routed = HashMap::new();
    routed.insert(net, r.copper);
    droute::write_copper(&base_text(a, &c)?, &c.board.nets, &routed, &out)
        .map_err(|e| format!("{out}: {e}"))?;
    eprintln!("[fanout] -> {out}");
    Ok(json!({
        "out": out, "net": net_name, "via": [via_d, drill],
        "open_islands": r.islands.len(), "connected": ok,
        "islands": r.islands,
    }))
}

/// stitch --pcb FILLED --net GND --zones REGEX --plane REGEX [--grid G] [--via D,DRILL]
/// [--margin 0.05] [--min-clr 0] [--min-area 0.3] [--base BASE.kicad_pcb] --out OUT.kicad_pcb
fn cmd_stitch(a: &Args) -> CmdResult {
    let c = load_ctx(a)?;
    let out = out_path(a, &c, "stitched.kicad_pcb")?;
    let net_name = a.s("net", "GND");
    let net = *c
        .board
        .net_id
        .get(&net_name)
        .ok_or(format!("--net {net_name}: no such net"))?;
    let (via_d, drill) = via_arg(a, &c, net)?;
    let p = stitch::StitchParams {
        net,
        zones: full_regex(a, "zones")?,
        plane: full_regex(a, "plane")?,
        grid: a.f("grid", 0.0),
        via_d,
        drill,
        margin: a.f("margin", 0.05),
        min_clr: a.f("min-clr", 0.0),
        min_area: a.f("min-area", 0.3),
        step: a.f("step", 0.25),
    };
    let nfill: usize = c
        .board
        .zones
        .iter()
        .filter(|z| z.net == net && p.zones.is_match(&z.name))
        .map(|z| z.fills.len())
        .sum();
    if nfill == 0 {
        return Err(format!(
            "no saved fill in the zones matching --zones for net {net_name}: refill a copy first (kicad-cli pcb drc --refill-zones --save-board)"
        ));
    }
    let m = model::Model::new(&c.board, &c.rules, c.cfg.clone(), model::CopperMode::All);
    let t = Instant::now();
    let r = stitch::stitch(&m, &p);
    eprintln!(
        "[stitch] {net_name}: {} islands, {} without via, {} fixed, {} new vias, {} failed in {:.2} s; rejected {:?}",
        r.islands,
        r.lone,
        r.fixed,
        r.vias.len(),
        r.failed.len(),
        t.elapsed().as_secs_f64(),
        r.why
    );
    for (l, q, ar, _) in r.failed.iter().take(12) {
        eprintln!("[stitch]   no spot: {l} ({:.2}, {:.2}) {ar} mm2", q.x, q.y);
    }
    // vias go into a copy of the input, or of --base (e.g. the same board without saved fills)
    let text = base_text(a, &c)?;
    let mut routed = HashMap::new();
    routed.insert(
        net,
        droute::RNet {
            segs: vec![],
            vias: r
                .vias
                .iter()
                .map(|q| droute::RVia {
                    at: *q,
                    d: via_d,
                    drill,
                })
                .collect(),
        },
    );
    droute::write_copper(&text, &c.board.nets, &routed, &out).map_err(|e| format!("{out}: {e}"))?;
    eprintln!("[stitch] {} vias -> {out}", r.vias.len());
    Ok(json!({
        "out": out, "net": net_name, "via": [via_d, drill],
        "islands": r.islands, "lone": r.lone, "fixed": r.fixed,
        "vias": r.vias.iter().map(|q| [geom_r(q.x), geom_r(q.y)]).collect::<Vec<_>>(),
        "failed": r.failed.iter().map(|(l, q, ar, bl)| json!({"layer": l, "at": [geom_r(q.x), geom_r(q.y)], "area_mm2": ar, "blockers": bl})).collect::<Vec<_>>(),
        "rejected": r.why,
    }))
}

/// opens --pcb B [--region box] [--nets pats]: unconnected connections counted from copper (all copper)
fn cmd_opens(a: &Args) -> CmdResult {
    let c = load_ctx(a)?;
    let (total, per) = count_opens(
        &c,
        a.get("region").map(parse_box).transpose()?,
        net_filter(a, &c.board.nets).as_ref(),
    );
    if !a.json() {
        for (n, k, _) in &per {
            println!("{n}\t{k}");
        }
        println!("OPENS\t{total}");
    }
    // islands: the pads of each copper component (largest first); --max-islands caps the list
    let cap = a.u("max-islands", 50);
    Ok(json!({"opens": total, "per_net": per
        .iter()
        .map(|(n, k, isl)| json!({"net": n, "opens": k, "islands": isl.iter().take(cap).collect::<Vec<_>>()}))
        .collect::<Vec<_>>()}))
}

/// unconnected connections (copper components - 1) per routable net (plus the nets named in
/// the filter, e.g. plane nets), and the islands (pad names per component, largest first)
type OpenNets = Vec<(String, usize, Vec<Vec<String>>)>;

fn count_opens(
    c: &Ctx,
    region: Option<geom::BBox>,
    filt: Option<&std::collections::HashSet<usize>>,
) -> (usize, OpenNets) {
    let m = model::Model::new(&c.board, &c.rules, c.cfg.clone(), model::CopperMode::All);
    let sel = |n: usize| m.routable[n] || filt.is_some_and(|f| f.contains(&n));
    let (pins, _) = model::pins_and_copper_of(&m, &sel);
    let mut total = 0;
    let mut per = Vec::new();
    let mut keys: Vec<&usize> = pins.keys().collect();
    keys.sort();
    for &n in keys {
        if filt.is_some_and(|f| !f.contains(&n)) {
            continue;
        }
        let mut isl: std::collections::BTreeMap<usize, Vec<String>> = Default::default();
        for p in pins[&n]
            .iter()
            .filter(|p| region.map(|r| r.contains(p.at)).unwrap_or(true))
        {
            let f = &c.board.fps[p.fp];
            isl.entry(p.comp)
                .or_default()
                .push(format!("{}.{}", f.refd, f.pads[p.pad].num));
        }
        if isl.len() > 1 {
            total += isl.len() - 1;
            let mut v: Vec<Vec<String>> = isl.into_values().collect();
            for i in v.iter_mut() {
                i.sort();
                i.dedup();
            }
            v.sort_by_key(|i| std::cmp::Reverse(i.len()));
            per.push((c.board.nets[n].clone(), v.len() - 1, v));
        }
    }
    (total, per)
}
