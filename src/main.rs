//! copperpath command line: check, place, route, opens, hints, pads, netclass (see README.md).

use copperpath::{
    board, check, delta, drc, droute, elec, explain, fanout, geom, groute, model, nudge, place,
    plane, progress, render, reroute, rules, sch, sexpr, sites, stitch, thermal,
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
  route     detailed router for a region (--region x0,y0,x1,y1 --out OUT.kicad_pcb
            and/or --delta-json DELTA.json: added/removed items in nm for the KiCad IPC API)
  ripup     remove tracks/vias of nets (--nets PATS [--region] [--locked] --out OUT)
  status    board overview for agents: layers, classes, zones, opens, DRC, next commands
  netinfo   length, vias, width, est. resistance (mOhm) and inductance (nH) per net
  drc       native copper DRC (KiCad rule names, ms instead of s): [-o report.json]
  opens     unconnected connections per net, counted from the copper
  hints     near-collinear pad pairs that force jogs between straight pad exits
  pads      pad world positions, sizes and nets (tab separated)
  netclass  net -> net class table (tab separated)
  stitch    stitching vias for pour islands of a plane net (--net --zones --plane --out)
  fanout    track + via from every open island of a plane net to its plane (--net --plane --out)
  nudge     move small parts locally until their nets route (--parts R5,R7 | --spec | --auto --out)
  render    PNG of a board region: layers, pads with labels, opens, DRC markers (--roi --out)
  plane     planes: list | report | fill | create | adjust (native zone filler, no kicad-cli)
  autoroute one command: plane fanout, routing, reroute, stitching, quality gate (--out --time)
  explain   why a connection is open: blockers, corridor width, ROI image (--net [--pad])

common options:
  --pcb -             read the board from stdin (give --pro / --dru explicitly)
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
    // --pcb -: board text on stdin (from an editor plugin); --pro / --dru have no default then
    let stdin = pcb == "-";
    if !stdin && !std::path::Path::new(&pcb).is_file() {
        return Err(format!("{pcb}: board file not found"));
    }
    let base = pcb.trim_end_matches(".kicad_pcb").to_string();
    let (pro, dru) = if stdin {
        (a.s("pro", ""), a.s("dru", ""))
    } else {
        (
            a.s("pro", &format!("{base}.kicad_pro")),
            a.s("dru", &format!("{base}.kicad_dru")),
        )
    };
    let t0 = Instant::now();
    let mut board = if stdin {
        let mut t = String::new();
        std::io::Read::read_to_string(&mut std::io::stdin(), &mut t)
            .map_err(|e| format!("stdin: {e}"))?;
        board::parse_board(t)?
    } else {
        board::load(&pcb)?
    };
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
    // plane nets given on the command line (autoroute): not routed as signals
    if let Some(pn) = a.get("plane-nets") {
        cfg.plane_nets
            .extend(pn.split(',').filter(|s| !s.is_empty()).map(String::from));
    }
    if let Some(y) = a.get("yield-pours") {
        cfg.yield_pours
            .extend(y.split(',').filter(|s| !s.is_empty()).map(String::from));
    }
    // signal tracks only on these layers: --signal-layers F.Cu,B.Cu (vias still pass every layer)
    if let Some(sl) = a
        .get("signal-layers")
        .filter(|v| *v != "auto" && *v != "all")
    {
        let names: Vec<String> = sl.split(',').map(|x| x.trim().to_string()).collect();
        let m = board::layer_mask(&names);
        if m == 0 {
            return Err(format!("--signal-layers {sl}: no copper layer"));
        }
        cfg.signal_layers = m;
    }
    // routing intent per net or class: --prio 'VBUS=low_r,class:Power=low_r' before --cfg, then
    // the project's "copperpath": {"net_prio": ...} block next to its net classes
    if let Some(p) = a.get("prio") {
        cfg.add_prio(p)?;
    }
    if let Ok(t) = std::fs::read_to_string(&pro) {
        if let Ok(j) = serde_json::from_str::<Value>(&t) {
            cfg.add_prio_json(&j["copperpath"]["net_prio"])
                .map_err(|e| format!("{pro}: copperpath.{e}"))?;
        }
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
        "drc" => cmd_drc(&a),
        "netinfo" => cmd_netinfo(&a),
        "status" => cmd_status(&a),
        "ripup" => cmd_ripup(&a),
        "thermals" => cmd_thermals(&a),
        "pads" => cmd_pads(&a),
        "hints" => cmd_hints(&a),
        "netclass" => cmd_netclass(&a),
        "stitch" => cmd_stitch(&a),
        "fanout" => cmd_fanout(&a),
        "nudge" => cmd_nudge(&a),
        "render" => cmd_render(&a),
        "explain" => cmd_explain(&a),
        "plane" => cmd_plane(&a),
        "autoroute" => cmd_autoroute(&a),
        "" => Err(format!("no command given\n{USAGE}")),
        c => Err(format!("unknown command '{c}'\n{USAGE}")),
    };
    let res = res.and_then(|v| explain_post(&a, v));
    let secs = t0.elapsed().as_secs_f64();
    // --progress-json: the last event of the stream
    if progress::enabled() {
        progress::emit(match &res {
            Ok(v) => {
                let failed = v["failed"].as_array().map_or(0, |f| f.len());
                json!({"ev": "done", "v": progress::VERSION, "ok": true,
                    "routed": v["nets"].as_u64().unwrap_or(0) as usize - failed, "failed": failed,
                    "opens": v["opens"], "stopped": v["stopped"], "out": v["out"], "delta_json": v["delta_json"]})
            }
            Err(e) => json!({"ev": "done", "v": progress::VERSION, "ok": false, "error": e}),
        });
    }
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

const KNOWN: [&str; 14] = [
    "check",
    "place",
    "route",
    "opens",
    "pads",
    "hints",
    "netclass",
    "stitch",
    "fanout",
    "nudge",
    "render",
    "plane",
    "explain",
    "autoroute",
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
fn cmd_route_core(a: &Args) -> CmdResult {
    let t0 = Instant::now();
    let mut summary = route_once(a)?;
    if a.get("reroute-blockers").is_none() || summary["opens"].as_u64().unwrap_or(0) == 0 {
        return Ok(summary);
    }
    // the reroute shares the route's --time-limit: none of it once the route used it up
    let limit = a.f("time-limit", 0.0);
    let out_of_time = || limit > 0.0 && t0.elapsed().as_secs_f64() >= limit;
    if out_of_time() {
        eprintln!("[reroute] skipped: --time-limit used up by the route");
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
        if out_of_time() {
            break;
        }
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
    // --delta-json FILE (or - for stdout) instead of or together with --out
    let delta_out = a.get("delta-json");
    if delta_out == Some("-") && a.json() {
        return Err("--delta-json - and --json both use stdout: give the delta a file".into());
    }
    let write_board = a.get("out").is_some() || delta_out.is_none();
    let out = a.s("out", "routed.kicad_pcb");
    if write_board && !out.ends_with(".kicad_pcb") {
        return Err(format!(
            "--out {out}: the output file must end in .kicad_pcb"
        ));
    }
    if write_board
        && c.pcb != "-"
        && std::path::Path::new(&out).canonicalize().ok()
            == std::path::Path::new(&c.pcb).canonicalize().ok()
    {
        return Err("--out must differ from --pcb (the input board is never overwritten)".into());
    }
    if c.pcb == "-" && a.get("cancel-on-stdin-eof").is_some() {
        return Err("--pcb - reads stdin to the end: use --cancel-file to cancel".into());
    }
    if a.get("drc").is_some() && (!write_board || c.pcb == "-") {
        return Err("--drc needs board files: give --pcb FILE and --out".into());
    }
    // all existing copper stays in the output, so all of it is an obstacle for other nets (and
    // connectivity for its own net); --ignore-unlocked restores the old "locked only" view
    let mode = if a.get("ignore-unlocked").is_some() {
        model::CopperMode::LockedOnly
    } else {
        model::CopperMode::All
    };
    let m = model::Model::new(&c.board, &c.rules, c.cfg.clone(), mode);
    let filt_early = net_filter(a, &c.board.nets);
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
    p.res = grid_res(a, &m, filt_early.as_ref());
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
    p.stall_gain = a.f("stall-gain", p.stall_gain as f64) as f32;
    p.par_conn = a.u("par-conn", p.par_conn);
    p.bend_cost = a.f("bend-cost", p.bend_cost as f64) as f32;
    p.bend90 = a.f("bend90", p.bend90 as f64) as f32;
    p.stub_ext = a.f("stub-ext", p.stub_ext);
    p.stub_ext_long = a.f("stub-ext-long", p.stub_ext_long);
    p.own_pad_cost = a.f("own-pad-cost", p.own_pad_cost as f64) as f32;
    p.stub_side_cost = a.f("stub-side-cost", p.stub_side_cost as f64) as f32;
    p.stubs = a.get("no-stubs").is_none();
    p.clean = a.get("no-clean").is_none();
    p.progress = progress::enabled();
    let filt = net_filter(a, &c.board.nets);
    let t = Instant::now();
    progress::phase("droute");
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
    // only failures the cleanup can have caused rerun the route (#54): nets whose search failed
    // (no path, node cap, window exhausted) fail at every cleanup level
    let search_fail = |w: &str| {
        w.starts_with("no path") || w.starts_with("node cap") || w.starts_with("job window")
    };
    let cleanup_suspect = res.why.iter().any(|w| !search_fail(w));
    if !cleanup_suspect && !res.failed.is_empty() {
        eprintln!(
            "[route] {} failed nets, all search failures: no cleanup fallback",
            res.failed.len()
        );
    }
    // --no-fallback: no extra full routes; such nets are dropped and reported (an agent reroutes
    // them with --nets)
    let fallback = a.get("no-fallback").is_none();
    if !res.failed.is_empty() && cleanup_suspect && p.clean && fallback && !halted() {
        // both fallback levels run at the same time (a route mostly uses one core); the choice
        // is the same as trying them one after the other
        progress::phase("fallback");
        let run_level = |slide: bool, clean: bool| {
            let mut p2 = p.clone();
            p2.slide = slide;
            p2.clean = clean;
            // two runs at once: their iterations would interleave in one progress stream
            p2.progress = false;
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
    progress::phase("write");
    let text = droute::pcb_text(&c.board.text, &c.board.nets, &res)?;
    if write_board {
        write_file(&out, &text)?;
        for (src, ext) in [(&c.pro, "kicad_pro"), (&c.dru, "kicad_dru")] {
            if std::path::Path::new(src).is_file() {
                std::fs::copy(src, out.replace(".kicad_pcb", &format!(".{ext}")))
                    .map_err(|e| format!("{src}: {e}"))?;
            }
        }
    }
    if let Some(dp) = delta_out {
        // routed copper is only appended: nothing of the input board is removed (yet)
        let d = delta::route_delta(&c.board.nets, &res, &[]);
        let s = serde_json::to_string(&d).unwrap_or_default();
        if dp == "-" {
            println!("{s}");
        } else {
            write_file(dp, &s)?;
        }
        eprintln!(
            "[route] delta: {} items added, 0 removed -> {dp}",
            d["add"].as_array().map(|v| v.len()).unwrap_or(0)
        );
    }
    // opens counted on the routed board (all copper, routed region and nets only)
    progress::phase("check");
    let board2 = board::parse_board(text)?;
    let rules2 = rules::Rules::load(
        &c.pro,
        if std::path::Path::new(&c.dru).is_file() {
            &c.dru
        } else {
            ""
        },
        &board2.nets,
    )?;
    let c2 = Ctx {
        board: board2,
        rules: rules2,
        cfg: c.cfg.clone(),
        pcb: out.clone(),
        pro: c.pro.clone(),
        dru: c.dru.clone(),
    };
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
        "out": write_board.then_some(&out), "delta_json": delta_out, "nets": r.jobs.len(), "connections": res.conns, "segments": nseg, "vias": nvia, "length_mm": check::round2(len),
        "routes": res.routes, "rips": res.rips, "opens": opens,
        "open_nets": open_nets.iter().map(|(n, k, _)| json!({"net": n, "opens": k})).collect::<Vec<_>>(),
        "aes": {"cleanup_level": level, "pins": s.pins, "pins_no_stub": s.pins_no_stub, "stub_ends": s.stubs_used, "short_stubs": s.short_stubs, "fallback_ends": s.fallback_ends, "aligned": s.aligned, "align_failed": s.align_failed, "pulls": s.pulls, "mitered": s.mitered, "miter_kept_90": s.miter_failed, "stub_end_shifts": s.shifted, "axis_jogs": s.jogs, "kept90_by_cause": {"clearance": s.kept90[0], "L_between_stubs": s.kept90[1], "shift_blocked": s.kept90[2], "Z_U_too_short": s.kept90[3], "short_legs": s.kept90[4]}, "exact_fail": s.exact_fail, "exact_dropped": s.exact_dropped},
        "drc": drc, "search_s": check::round2(dt),
        "failed": res.failed.iter().map(|(n, l)| json!([c.board.nets[*n], l])).collect::<Vec<_>>(),
        "warnings": warnings,
        "stopped": stopped,
        "hotspots": res.hotspots.iter().take(20).map(|h| json!({"net": c.board.nets[h.net], "cells": h.cells, "at": [check::round2(h.x), check::round2(h.y)], "layer": h.layer})).collect::<Vec<_>>(),
    });
    if write_board {
        write_file(
            &out.replace(".kicad_pcb", "_route.json"),
            &serde_json::to_string_pretty(&summary).unwrap_or_default(),
        )?;
        eprintln!("[route] {opens} opens -> {out}");
    } else {
        eprintln!("[route] {opens} opens");
    }
    Ok(summary)
}

/// cache key of a kicad-cli DRC report: board, project and rules text plus the kicad-cli binary
/// (path, size, mtime), so an edited board or rule file or another KiCad version runs again
fn drc_key(cli: &str, pcb: &str) -> Option<u64> {
    let base = pcb.trim_end_matches(".kicad_pcb");
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    let mut eat = |b: &[u8]| {
        for &x in b.iter().chain(&b.len().to_le_bytes()) {
            h = (h ^ x as u64).wrapping_mul(0x100_0000_01b3);
        }
    };
    eat(&std::fs::read(pcb).ok()?);
    for ext in ["kicad_pro", "kicad_dru"] {
        eat(&std::fs::read(format!("{base}.{ext}")).unwrap_or_default());
    }
    let bin = which(cli).unwrap_or_else(|| cli.into());
    eat(bin.as_bytes());
    if let Ok(m) = std::fs::metadata(&bin) {
        eat(&m.len().to_le_bytes());
        let t = m
            .modified()
            .ok()?
            .duration_since(std::time::UNIX_EPOCH)
            .ok()?;
        eat(&t.as_nanos().to_le_bytes());
    }
    Some(h)
}

/// `cli` resolved on the PATH (a path with a separator is taken as is)
fn which(cli: &str) -> Option<String> {
    if cli.contains(std::path::MAIN_SEPARATOR) || cli.contains('/') {
        return Some(cli.into());
    }
    std::env::split_paths(&std::env::var_os("PATH")?)
        .flat_map(|d| [d.join(cli), d.join(format!("{cli}.exe"))])
        .find(|p| p.is_file())
        .map(|p| p.to_string_lossy().into_owned())
}

/// kicad-cli DRC on the input and the routed board; violations only the routed board has.
/// kicad-cli: --kicad-cli PATH, else $KICAD_CLI, else `kicad-cli` on the PATH.
/// The input's report is cached (in this process and in the temp dir, keyed by `drc_key`), so
/// the autoroute gate and repeated runs on one input check it once; both boards run in parallel.
fn drc_delta(a: &Args, before: &str, after: &str) -> Result<Value, String> {
    if a.get("drc") == Some("native") {
        return drc_delta_native(a, before, after);
    }
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
    static MEM: std::sync::Mutex<Option<HashMap<u64, Value>>> = std::sync::Mutex::new(None);
    let key = drc_key(&cli, before);
    let disk = key.map(|k| {
        std::env::temp_dir()
            .join("copperpath-drc")
            .join(format!("{k:016x}.json"))
    });
    let cached = key
        .and_then(|k| MEM.lock().ok()?.as_ref()?.get(&k).cloned())
        .or_else(|| {
            let t = std::fs::read_to_string(disk.as_ref()?).ok()?;
            serde_json::from_str::<Value>(&t).ok()
        });
    let (b, n) = std::thread::scope(|sc| {
        let hb = cached.is_none().then(|| sc.spawn(|| run(before, &base)));
        let n = run(after, &new);
        let b = match hb {
            Some(h) => h
                .join()
                .unwrap_or_else(|_| Err("--drc: thread panicked".into())),
            None => Ok(cached.clone().unwrap_or_default()),
        };
        (b, n)
    });
    let (b, n) = (b?, n?);
    if cached.is_some() {
        write_file(&base, &serde_json::to_string_pretty(&b).unwrap_or_default())?;
    }
    if let Some(k) = key {
        if let Ok(mut m) = MEM.lock() {
            m.get_or_insert_with(HashMap::new).insert(k, b.clone());
        }
        if let Some(d) = disk.as_ref().filter(|_| cached.is_none()) {
            let _ = std::fs::create_dir_all(d.parent().unwrap_or(std::path::Path::new(".")));
            let _ = std::fs::write(d, b.to_string());
        }
    }
    drc_compare(&b, &n, base, new)
}

/// violations only the routed report has (by severity, type and items), unconnected counts
fn drc_compare(b: &Value, n: &Value, base: String, new: String) -> Result<Value, String> {
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
    let (kb, kn) = (keys(b), keys(n));
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
        "unconnected_before": unc(b), "unconnected_after": unc(n), "reports": [base, new],
    }))
}

/// `--drc native`: the native copper DRC (`drc.rs`, saved or native fills) on both boards with
/// the input's rules; milliseconds instead of two kicad-cli runs
fn drc_delta_native(a: &Args, before: &str, after: &str) -> Result<Value, String> {
    let mut kv = a.kv.clone();
    kv.insert("pcb".into(), before.into());
    let c0 = load_ctx(&Args {
        cmd: "drc".into(),
        kv: kv.clone(),
    })?;
    let report = |pcb: &str| -> Result<Value, String> {
        let mut kv = kv.clone();
        kv.insert("pcb".into(), pcb.into());
        kv.insert("pro".into(), c0.pro.clone());
        kv.insert("dru".into(), c0.dru.clone());
        let c = load_ctx_filled(&Args {
            cmd: "drc".into(),
            kv,
        })?;
        Ok(native_report(&c).0)
    };
    let base = after.replace(".kicad_pcb", "_drc_before.json");
    let new = after.replace(".kicad_pcb", "_drc_after.json");
    let (b, n) = (report(before)?, report(after)?);
    write_file(&base, &serde_json::to_string_pretty(&b).unwrap_or_default())?;
    write_file(&new, &serde_json::to_string_pretty(&n).unwrap_or_default())?;
    let mut d = drc_compare(&b, &n, base, new)?;
    d["engine"] = json!("native");
    Ok(d)
}

/// native DRC report of a loaded board: (kicad-style report, violations, unconnected count)
fn native_report(c: &Ctx) -> (Value, Vec<drc::Violation>, usize) {
    let m = model::Model::new(&c.board, &c.rules, c.cfg.clone(), model::CopperMode::All);
    let v = drc::check(&m);
    let all: std::collections::HashSet<usize> = (1..c.board.nets.len()).collect();
    let (opens, per) = count_opens(c, None, Some(&all));
    let mut unc = Vec::new();
    for (n, k, isl) in &per {
        for i in 0..*k {
            unc.push((n.clone(), isl.get(i + 1).cloned().unwrap_or_default()));
        }
    }
    (drc::report(&v, &unc), v, opens)
}

/// grid resolution of the detailed router: `--res MM` (default 0.05), or `--res auto`: 0.1 mm when
/// every net to route has at least 0.2 mm clearance and track width and no footprint with such
/// nets has a pad pitch under 0.5 mm (4x fewer cells; same result on the coarse PCBench boards),
/// else 0.05 mm
fn grid_res(a: &Args, m: &model::Model, filt: Option<&std::collections::HashSet<usize>>) -> f64 {
    match a.get("res") {
        None => return 0.05,
        Some(v) if v != "auto" => return v.parse().unwrap_or(0.05),
        _ => {}
    }
    let r = m.r;
    let nets: Vec<usize> = (1..m.b.nets.len())
        .filter(|&n| m.routable[n] && filt.is_none_or(|f| f.contains(&n)))
        .collect();
    let mut clr = f64::INFINITY;
    let mut width = f64::INFINITY;
    for &i in &nets {
        width = width.min(r.track_width(i).0);
        for j in 0..m.b.nets.len() {
            if j != i {
                clr = clr.min(r.clr(i, j, true)).min(r.clr(i, j, false));
            }
        }
    }
    let mut pitch = f64::INFINITY;
    for f in &m.b.fps {
        if !f.pads.iter().any(|p| nets.binary_search(&p.net).is_ok()) {
            continue;
        }
        let c: Vec<geom::P> = f.pads.iter().map(|p| f.pad_world(p).0).collect();
        for i in 0..c.len() {
            for j in i + 1..c.len() {
                let d = (c[i] - c[j]).len();
                if d > 1e-6 {
                    pitch = pitch.min(d);
                }
            }
        }
    }
    let res = if clr >= 0.2 - 1e-9 && width >= 0.2 - 1e-9 && pitch >= 0.5 - 1e-9 {
        0.1
    } else {
        0.05
    };
    eprintln!(
        "[route] grid {res} mm (auto: min clearance {:.3}, width {:.3}, pad pitch {:.3} mm)",
        clr, width, pitch
    );
    res
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
    let mut c = load_ctx_filled(a)?;
    let out = out_path(a, &c, "fanout.kicad_pcb")?;
    let net_name = a.s("net", "GND");
    let net = *c
        .board
        .net_id
        .get(&net_name)
        .ok_or(format!("--net {net_name}: no such net"))?;
    // --plane-only: other fills of the net (outer pours) do not join pads; every pad reaches the
    // plane itself, so later tracks that cut the pour do not strand it
    if a.get("plane-only").is_some() {
        let plane = full_regex(a, "plane")?;
        let nets = c.board.nets.clone();
        for z in c.board.zones.iter_mut() {
            if z.net == net && !plane.is_match(&z.key(&nets)) {
                z.fills.clear();
            }
        }
    }
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
    let c = load_ctx_filled(a)?;
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
        .filter(|z| z.net == net && p.zones.is_match(&z.key(&c.board.nets)))
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
    let c = load_ctx_filled(a)?;
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

/// remove the tracks and vias of the nets matching `--nets` (wildcards), unlocked ones only unless
/// `--locked`, optionally only those touching `--region`; writes `--out`
fn cmd_ripup(a: &Args) -> CmdResult {
    let c = load_ctx(a)?;
    let out = out_path(a, &c, "ripped.kicad_pcb")?;
    let filt = net_filter(a, &c.board.nets).ok_or("ripup: --nets PATTERNS is required")?;
    let region = a.get("region").map(parse_box).transpose()?;
    let locked = a.get("locked").is_some();
    let b = &c.board;
    let mut uuids: Vec<String> = Vec::new();
    let mut per: std::collections::BTreeMap<String, (usize, usize)> = Default::default();
    for t in &b.tracks {
        let inside = region.is_none_or(|r| r.contains(t.a) || r.contains(t.b));
        if filt.contains(&t.net) && (locked || !t.locked) && inside && !t.uuid.is_empty() {
            uuids.push(t.uuid.clone());
            per.entry(b.nets[t.net].clone()).or_default().0 += 1;
        }
    }
    for v in &b.vias {
        let inside = region.is_none_or(|r| r.contains(v.at));
        if filt.contains(&v.net) && (locked || !v.locked) && inside && !v.uuid.is_empty() {
            uuids.push(v.uuid.clone());
            per.entry(b.nets[v.net].clone()).or_default().1 += 1;
        }
    }
    let (text, n) = reroute::remove_items(&b.text, &uuids);
    write_file(&out, &text)?;
    eprintln!("[ripup] {n} items of {} nets removed -> {out}", per.len());
    Ok(json!({
        "out": out, "removed": n,
        "nets": per.iter().map(|(k, (t, v))| json!({"net": k, "tracks": t, "vias": v})).collect::<Vec<_>>(),
    }))
}

/// second connection for starved thermal reliefs: refills the zones natively, then adds a short
/// track from each pad with a single spoke into its net's fill (see `thermal.rs`); writes `--out`
fn cmd_thermals(a: &Args) -> CmdResult {
    let mut kv = a.kv.clone();
    kv.insert("refill-zones".into(), "1".into());
    kv.entry("fill-res".into()).or_insert_with(|| "0.05".into());
    let c = load_ctx_filled(&Args {
        cmd: a.cmd.clone(),
        kv,
    })?;
    let out = out_path(a, &c, "thermals.kicad_pcb")?;
    let m = model::Model::new(&c.board, &c.rules, c.cfg.clone(), model::CopperMode::All);
    let starved: usize = c
        .board
        .zones
        .iter()
        .map(|z| thermal::starved(&m, z).len())
        .sum();
    let add = thermal::stubs(&m);
    let n: usize = add.values().map(|r| r.segs.len()).sum();
    let text = droute::copper_text(&c.board.text, &c.board.nets, &add)?;
    write_file(&out, &text)?;
    for (src, ext) in [(&c.pro, "kicad_pro"), (&c.dru, "kicad_dru")] {
        let dst = out.replace(".kicad_pcb", &format!(".{ext}"));
        // never onto itself (--out next to the input: copying would truncate it)
        if std::path::Path::new(src).is_file() && *src != dst {
            std::fs::copy(src, &dst).map_err(|e| format!("{src}: {e}"))?;
        }
    }
    eprintln!("[thermals] {n} stub(s) for {starved} starved thermal pad(s) -> {out}");
    Ok(json!({"out": out, "starved": starved, "stubs": n}))
}

/// one-call board overview for agents (milliseconds): stack, net classes, zones (with their
/// selection key), plane nets and candidates, opens per net, native DRC by type, and the next
/// commands to run. `--refill-zones` fills the zones natively first (else saved fills count).
fn cmd_status(a: &Args) -> CmdResult {
    let c = load_ctx_filled(a)?;
    let t = Instant::now();
    let b = &c.board;
    let layers: Vec<&str> = (0..board::CU.len())
        .filter(|l| b.cu_mask & (1 << l) != 0)
        .map(|l| board::CU[l])
        .collect();
    let (_, v, opens) = native_report(&c);
    let mut by: std::collections::BTreeMap<&str, usize> = Default::default();
    for x in &v {
        *by.entry(x.typ).or_default() += 1;
    }
    let all: std::collections::HashSet<usize> = (1..b.nets.len()).collect();
    let (_, per) = count_opens(&c, None, Some(&all));
    let planes = plane::detect(b, &c.rules, a.u("min-pads", 4));
    let zones: Vec<Value> = b
        .zones
        .iter()
        .filter(|z| !z.keepout && z.net != 0)
        .map(|z| {
            json!({"key": z.key(&b.nets), "net": b.nets[z.net], "priority": z.priority,
                "filled": !z.fills.is_empty(), "locked": z.locked})
        })
        .collect();
    let classes: Vec<Value> = c
        .rules
        .classes
        .iter()
        .map(|k| {
            let n = (1..b.nets.len())
                .filter(|&i| c.rules.class_of(i).name == k.name)
                .count();
            json!({"name": k.name, "nets": n, "clearance_mm": k.clearance,
                "track_mm": k.track_width, "via_mm": [k.via_d, k.via_drill]})
        })
        .collect();
    // next commands
    let pcb = &c.pcb;
    let mut next: Vec<String> = Vec::new();
    let unfilled = b
        .zones
        .iter()
        .filter(|z| !z.keepout && z.net != 0 && z.fills.is_empty())
        .count();
    if unfilled > 0 && a.get("refill-zones").is_none() && a.get("native-fill").is_none() {
        next.push(format!(
            "{unfilled} zone(s) have no saved fill, so their pads count as open: add --refill-zones"
        ));
    }
    let inner_free: Vec<&str> = layers
        .iter()
        .copied()
        .filter(|l| l.starts_with("In"))
        .filter(|l| {
            !b.zones.iter().any(|z| {
                !z.keepout && board::layer_idx(l).is_some_and(|i| z.layers & (1 << i) != 0)
            })
        })
        .collect();
    let cand: Vec<&str> = planes
        .iter()
        .filter(|p| p.candidate)
        .map(|p| p.net.as_str())
        .collect();
    // routing intents from net names for routed nets without one (planes and pours excluded)
    let m = model::Model::new(&c.board, &c.rules, c.cfg.clone(), model::CopperMode::All);
    let pour: std::collections::HashSet<usize> = b.zones.iter().map(|z| z.net).collect();
    let suggested: Vec<String> = (1..b.nets.len())
        .filter(|&i| m.routable[i] && !pour.contains(&i))
        .filter(|&i| c.cfg.prio(&b.nets[i], &c.rules.class_of(i).name) == model::NetPrio::default())
        .filter_map(|i| model::suggest_intent(&b.nets[i]).map(|p| format!("{}={p}", b.nets[i])))
        .take(16)
        .collect();
    let suggested = suggested.join(",");
    if opens > 0 {
        let mut s =
            format!("copperpath autoroute --pcb {pcb} --out routed.kicad_pcb --drc native --json");
        if let (Some(l), Some(n)) = (inner_free.first(), cand.first()) {
            s += &format!(" --planes {l}={n}");
        }
        if !suggested.is_empty() {
            s += &format!(" --prio '{suggested}'");
        }
        next.push(s);
    }
    if let Some((n, _, _)) = per.iter().max_by_key(|(_, k, _)| *k) {
        next.push(format!("copperpath explain --pcb {pcb} --net '{n}' --json"));
    }
    if c.rules.classes.iter().any(|k| k.name != "Default") && c.cfg.net_prio.is_empty() {
        let k = &c
            .rules
            .classes
            .iter()
            .find(|k| k.name != "Default")
            .unwrap()
            .name;
        next.push(format!(
            "routing intent per class, e.g. --prio 'class:{k}=low_r' (or a copperpath block in the .kicad_pro)"
        ));
    }
    if !v.is_empty() {
        next.push(format!(
            "copperpath drc --pcb {pcb} --out drc.json   # where the violations are"
        ));
    }
    let dt = t.elapsed().as_secs_f64();
    if !a.json() {
        println!(
            "{}: {} layers ({}), {} footprints, {} nets, {:.1} x {:.1} mm",
            pcb,
            layers.len(),
            layers.join(","),
            b.fps.len(),
            b.nets.len() - 1,
            b.bbox.x1 - b.bbox.x0,
            b.bbox.y1 - b.bbox.y0
        );
        println!("opens {opens}, DRC {:?}", by);
        for n in &next {
            println!("next: {n}");
        }
    }
    Ok(json!({
        "layers": layers, "footprints": b.fps.len(), "nets": b.nets.len() - 1,
        "size_mm": [check::round2(b.bbox.x1 - b.bbox.x0), check::round2(b.bbox.y1 - b.bbox.y0)],
        "classes": classes, "zones": zones,
        "planes": planes.iter().filter(|p| p.plane || p.pour || p.candidate).collect::<Vec<_>>(),
        "opens": opens,
        "open_nets": per.iter().map(|(n, k, _)| json!({"net": n, "opens": k})).collect::<Vec<_>>(),
        "drc": by, "suggested_prio": suggested, "next": next, "s": check::round2(dt),
    }))
}

/// copper stack for the electrical estimates: --cu-um 35, --plating-um 25, --thickness 1.6 (mm)
fn stack(a: &Args) -> elec::Stack {
    let d = elec::Stack::default();
    elec::Stack {
        cu: a.f("cu-um", d.cu * 1000.0) / 1000.0,
        plating: a.f("plating-um", d.plating * 1000.0) / 1000.0,
        thickness: a.f("thickness", d.thickness),
    }
}

/// per-net length, vias, widths and estimated resistance / inductance of the board's copper
fn cmd_netinfo(a: &Args) -> CmdResult {
    let c = load_ctx(a)?;
    let filt = net_filter(a, &c.board.nets);
    let mut v = elec::nets(&c.board, &stack(a), &|n| {
        filt.as_ref().is_none_or(|f| f.contains(&n))
    });
    let m = model::Model::new(&c.board, &c.rules, c.cfg.clone(), model::CopperMode::All);
    elec::coupling(&m, a.f("near", 0.5), &mut v);
    // --match 'D[0-7];A[0-9]+': length skew per group of nets (full-name regexes, ';' between
    // groups): what a bus needs for length matching
    let mut groups = Vec::new();
    if let Some(spec) = a.get("match") {
        for pat in spec.split(';').map(str::trim).filter(|p| !p.is_empty()) {
            let re = regex::Regex::new(&format!("^(?:{pat})$"))
                .map_err(|e| format!("--match {pat}: {e}"))?;
            let g: Vec<&elec::NetElec> = v.iter().filter(|e| re.is_match(&e.net)).collect();
            let (Some(lo), Some(hi)) = (
                g.iter().min_by(|x, y| x.length_mm.total_cmp(&y.length_mm)),
                g.iter().max_by(|x, y| x.length_mm.total_cmp(&y.length_mm)),
            ) else {
                groups.push(json!({"match": pat, "nets": 0}));
                continue;
            };
            groups.push(json!({
                "match": pat, "nets": g.len(),
                "shortest": lo.net, "min_mm": lo.length_mm,
                "longest": hi.net, "max_mm": hi.length_mm,
                "skew_mm": check::round2(hi.length_mm - lo.length_mm),
            }));
        }
    }
    if !a.json() {
        println!("net\tlength_mm\tvias\twidth_mm\tR_mOhm\tL_nH\tcoupled_mm\tz0_ohm");
        for e in &v {
            println!(
                "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
                e.net,
                e.length_mm,
                e.vias,
                e.width_min_mm,
                e.r_mohm,
                e.l_nh,
                e.coupled_mm,
                e.z0_ohm.map_or("-".into(), |z| z.to_string())
            );
        }
        for g in &groups {
            println!("group {g}");
        }
    }
    Ok(json!({"nets": v, "groups": groups}))
}

/// native copper DRC with KiCad's rule names (see `drc.rs`); `-o` writes a KiCad-style JSON
/// report; unconnected items are the opens of every net with zones refilled natively
fn cmd_drc(a: &Args) -> CmdResult {
    let mut kv = a.kv.clone();
    // the native fill needs a finer raster than routing to match KiCad's polygon fill
    if a.get("refill-zones").is_some() && a.get("fill-res").is_none() {
        kv.insert("fill-res".into(), "0.05".into());
    }
    let c = load_ctx_filled(&Args {
        cmd: a.cmd.clone(),
        kv,
    })?;
    let t = Instant::now();
    let (rep, v, opens) = native_report(&c);
    let dt = t.elapsed().as_secs_f64();
    let mut by: std::collections::BTreeMap<&str, usize> = Default::default();
    for x in &v {
        *by.entry(x.typ).or_default() += 1;
    }
    if let Some(o) = a.get("out") {
        write_file(o, &serde_json::to_string_pretty(&rep).unwrap_or_default())?;
    }
    if !a.json() {
        for (k, n) in &by {
            println!("{k}\t{n}");
        }
        println!("unconnected_items\t{opens}");
    }
    eprintln!(
        "[drc] {} violations, {opens} unconnected in {dt:.3} s",
        v.len()
    );
    Ok(json!({"violations": v.len(), "by_type": by, "unconnected": opens, "s": check::round2(dt)}))
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
        // pads with the same number in one footprint are one terminal (KiCad: no missing
        // connection between them), so their components are merged
        let mut comp: HashMap<usize, usize> = HashMap::new();
        let mut first: HashMap<(usize, &str), usize> = HashMap::new();
        fn root(m: &mut HashMap<usize, usize>, mut c: usize) -> usize {
            while let Some(&p) = m.get(&c).filter(|&&p| p != c) {
                c = p;
            }
            c
        }
        for p in &pins[&n] {
            let key = (p.fp, c.board.fps[p.fp].pads[p.pad].num.as_str());
            let f = *first.entry(key).or_insert(p.comp);
            let (a, b) = (root(&mut comp, f), root(&mut comp, p.comp));
            if a != b {
                comp.insert(b, a);
            }
        }
        let mut isl: std::collections::BTreeMap<usize, Vec<String>> = Default::default();
        for p in pins[&n]
            .iter()
            .filter(|p| region.map(|r| r.contains(p.at)).unwrap_or(true))
        {
            let f = &c.board.fps[p.fp];
            isl.entry(root(&mut comp, p.comp))
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

/// comma separated numbers
fn num_list(s: &str, what: &str) -> Result<Vec<f64>, String> {
    s.split(',')
        .filter(|x| !x.trim().is_empty())
        .map(|x| x.trim().parse::<f64>())
        .collect::<Result<_, _>>()
        .map_err(|_| format!("--{what} {s}: expected comma separated numbers"))
}

/// nudge --pcb B (--parts R5,R7 [--range 1] [--step 0.05] [--rots 0,90] | --spec moves.json |
/// --auto [--auto-dist 3] [--auto-max 4]) [--nets pats] [--rip region|attached|none]
/// [--region x0,y0,x1,y1] [--margin 2] [--time-limit 60] [--max-cands N] [--dry-run] [--drc]
/// --out OUT.kicad_pcb
fn cmd_nudge(a: &Args) -> CmdResult {
    let c = load_ctx(a)?;
    let mut spec: nudge::Spec = match a.get("spec") {
        Some(f) => {
            let t = std::fs::read_to_string(f).map_err(|e| format!("{f}: {e}"))?;
            serde_json::from_str(&t).map_err(|e| format!("{f}: {e}"))?
        }
        None => nudge::Spec::default(),
    };
    if let Some(v) = a.get("range") {
        spec.range = v.parse().map_err(|_| format!("--range {v}: expected mm"))?;
    }
    if let Some(v) = a.get("step") {
        spec.step = v.parse().map_err(|_| format!("--step {v}: expected mm"))?;
    }
    if let Some(v) = a.get("rots") {
        spec.drot = num_list(v, "rots")?;
    }
    if let Some(v) = a.get("margin") {
        spec.margin = v
            .parse()
            .map_err(|_| format!("--margin {v}: expected mm"))?;
    }
    if let Some(v) = a.get("reach") {
        spec.reach = v.parse().map_err(|_| format!("--reach {v}: expected mm"))?;
    }
    if let Some(v) = a.get("rip-classes") {
        spec.rip_classes = v.split(',').map(String::from).collect();
    }
    if let Some(v) = a.get("exclude-nets") {
        spec.exclude_nets = v.split(',').map(String::from).collect();
    }
    if a.get("move-locked").is_some() {
        spec.move_locked = true;
    }
    if a.get("rip-locked").is_some() {
        spec.rip_locked = true;
    }
    if let Some(v) = a.get("push") {
        spec.push = v.parse().map_err(|_| format!("--push {v}: expected mm"))?;
    }
    if let Some(v) = a.get("push-range") {
        spec.push_range = v
            .parse()
            .map_err(|_| format!("--push-range {v}: expected mm"))?;
    }
    if let Some(v) = a.get("rip") {
        spec.rip = v.to_string();
    }
    if let Some(v) = a.get("nets") {
        spec.nets = Some(v.split(',').map(String::from).collect());
    }
    if let Some(v) = a.get("region") {
        let bb = parse_box(v)?;
        spec.region = Some([bb.x0, bb.y0, bb.x1, bb.y1]);
    }
    let mut parts: Vec<String> = a
        .get("parts")
        .map(|v| {
            v.split(',')
                .filter(|s| !s.is_empty())
                .map(String::from)
                .collect()
        })
        .unwrap_or_default();
    if a.get("auto").is_some() {
        let rg = spec.region.map(|r| geom::BBox {
            x0: r[0],
            y0: r[1],
            x1: r[2],
            y1: r[3],
        });
        let found = nudge::auto_parts(
            &c.board,
            &c.rules,
            &c.cfg,
            rg,
            a.f("auto-dist", 3.0),
            a.u("auto-max", 4),
        );
        eprintln!("[nudge] --auto picked {:?}", found);
        parts.extend(found);
    }
    for r in parts {
        if !spec.parts.iter().any(|p| p.refd == r) {
            spec.parts.push(nudge::PartSpec {
                refd: r,
                ..Default::default()
            });
        }
    }
    if spec.parts.is_empty() {
        return Err(if a.get("auto").is_some() {
            "--auto found no 2-pad SMD part near an open connection".into()
        } else {
            "no movable parts: give --parts R5,R7, --spec moves.json or --auto".into()
        });
    }
    let dry = a.get("dry-run").is_some();
    let out = if dry {
        String::new()
    } else {
        out_path(a, &c, "nudged.kicad_pcb")?
    };
    let mut o = nudge::Opts {
        time_limit: a.f("time-limit", 60.0),
        max_cands: a.u("max-cands", 20000),
        coarse: a.u("coarse", 9),
        clean_search: a.get("clean-search").is_some(),
        verify_fill: a.u("verify-fill", 8),
        fill_search: a.get("fill-search").is_some(),
        ..Default::default()
    };
    let p = &mut o.params;
    p.res = a.f("res", p.res);
    p.via_cost = a.f("via-cost", p.via_cost as f64) as f32;
    p.job_margin = a.f("job-margin", 3.0);
    p.max_expand = a.f("max-expand", 400_000.0) as u64;
    p.nego_iters = a.u("iters", 8);
    droute::install_stop_handler();
    eprintln!(
        "[nudge] {} parts: {}",
        spec.parts.len(),
        spec.parts
            .iter()
            .map(|p| p.refd.as_str())
            .collect::<Vec<_>>()
            .join(",")
    );
    let res = nudge::run(&c.board, &c.rules, &c.cfg, &spec, &o)?;
    let names =
        |v: &[usize]| -> Vec<String> { v.iter().map(|n| c.board.nets[*n].clone()).collect() };
    eprintln!(
        "[nudge] region {:.2},{:.2},{:.2},{:.2}, {} nets {:?}, {} ripped items, {} opens before",
        res.region.x0,
        res.region.y0,
        res.region.x1,
        res.region.y1,
        res.nets.len(),
        names(&res.nets),
        res.ripped.len(),
        res.opens_before
    );
    eprintln!(
        "[nudge] {} candidates routed ({} infeasible skipped), {:.1} ms per candidate (wall, {} threads), search {:.2} s{}",
        res.evaluated,
        res.infeasible,
        res.route_ms,
        rayon::current_num_threads(),
        res.search_s,
        res.stopped.map(|s| format!(", stopped: {s}")).unwrap_or_default()
    );
    let improved = res.improves();
    let moves: Vec<Value> = res
        .moves
        .iter()
        .map(|(r, a0, r0, a1, r1)| {
            json!({"ref": r, "from": [geom_r(a0.x), geom_r(a0.y), geom_r(*r0)], "to": [geom_r(a1.x), geom_r(a1.y), geom_r(*r1)],
                "dx": geom_r(a1.x - a0.x), "dy": geom_r(a1.y - a0.y), "drot": geom_r(copperpath::movefp::norm_angle(r1 - r0))})
        })
        .collect();
    for m in &moves {
        eprintln!("[nudge]   {}: {} -> {}", m["ref"], m["from"], m["to"]);
    }
    let best = res.best.as_ref();
    let failed: HashMap<usize, usize> = best
        .map(|b| b.failed.iter().copied().collect())
        .unwrap_or_default();
    // optional ROI images of what blocks (`render`)
    let img_dir = a.get("explain-img");
    let mut img_n = 0usize;
    let mut render_roi = |name: &str, roi: geom::BBox, hl: Option<usize>| -> Option<String> {
        let dir = img_dir?;
        img_n += 1;
        if img_n > 12 {
            return None;
        }
        let path = format!("{dir}/{name}.png");
        let o = render::Opts {
            roi: Some(roi),
            layers: render::parse_layers("all").ok()?,
            nets: None,
            hl_net: hl,
            fills: false,
            labels: true,
            courtyards: true,
            opens: true,
            halo_min: 1.0,
            drc: Vec::new(),
            px: 900,
            per_layer: false,
            title: name.to_string(),
        };
        let m = model::Model::new(&c.board, &c.rules, c.cfg.clone(), model::CopperMode::All);
        let r = render::render(&m, &o).ok()?;
        write_bytes(&path, &r.png).ok()?;
        Some(path)
    };
    let fails: HashMap<usize, &nudge::Fail> = best
        .map(|b| b.fails.iter().map(|f| (f.net, f)).collect())
        .unwrap_or_default();
    let per_net: Vec<Value> = res
        .nets
        .iter()
        .map(|n| {
            let mut v = json!({"net": c.board.nets[*n], "opens_before": res.open_before.get(n).copied().unwrap_or(0), "opens_after": failed.get(n).copied().unwrap_or(0)});
            if let Some(f) = fails.get(n) {
                v["reason"] = json!(format!("router: {}", f.why));
                v["components"] = json!(f
                    .comps
                    .iter()
                    .map(|cp| cp.iter().map(|(r, q)| json!({"pin": r, "at": [geom_r(q.x), geom_r(q.y)]})).collect::<Vec<_>>())
                    .collect::<Vec<_>>());
                let mut bb = geom::BBox::empty();
                for (_, q) in f.comps.iter().flatten() {
                    bb.add(*q);
                }
                if bb.valid() {
                    let name = format!("fail_{}", render::short_net(&c.board.nets[*n]).replace(['/', '\\', ' ', '~', '{', '}'], "_"));
                    if let Some(pth) = render_roi(&name, bb.inflate(2.0), Some(*n)) {
                        v["img"] = json!(pth);
                    }
                }
            }
            v
        })
        .collect();
    let reasons: Vec<Value> = res
        .reasons
        .iter()
        .enumerate()
        .map(|(i, (w, n, at))| {
            let mut v = json!({"reason": w, "candidates": n, "at": [geom_r(at.x), geom_r(at.y)]});
            if i < 4 {
                let roi = geom::BBox::empty_at(*at).inflate(3.0);
                if let Some(pth) = render_roi(&format!("blocked_{}", i + 1), roi, None) {
                    v["img"] = json!(pth);
                }
            }
            v
        })
        .collect();
    for rs in res.reasons.iter().take(5) {
        eprintln!("[nudge]   infeasible x{}: {}", rs.1, rs.0);
    }
    let unfilled = c
        .board
        .zones
        .iter()
        .filter(|z| !z.keepout && z.net != 0 && z.fills.is_empty())
        .count();
    let mut warnings: Vec<String> = Vec::new();
    if unfilled > 0 {
        warnings.push(format!("{unfilled} copper zones have no saved fill: pour connectivity is unknown, so the opens may be wrong (refill and save the board first, e.g. kicad-cli pcb drc --refill-zones --save-board)"));
        eprintln!("[nudge] warning: {}", warnings[0]);
    }
    let mut summary = json!({
        "improved": improved,
        "opens_before": res.opens_before,
        "pour_check": {"board_opens_before": res.true_before, "board_opens_after": best.and_then(|b| b.true_opens)},
        "score": best.map(|b| json!({"opens": b.opens, "length_mm": check::round2(b.length), "vias": b.vias, "moved_mm": geom_r(b.moved), "rotated": b.rotated, "cost": check::round2(b.cost)})),
        "moves": moves,
        "nets": per_net,
        "ripped_items": res.ripped.len(),
        "region": [geom_r(res.region.x0), geom_r(res.region.y0), geom_r(res.region.x1), geom_r(res.region.y1)],
        "candidates": res.evaluated, "infeasible": res.infeasible,
        "infeasible_reasons": reasons, "pushed": res.pushed, "warnings": warnings,
        "route_ms_per_candidate": check::round2(res.route_ms),
        "search_s": check::round2(res.search_s),
        "stopped": res.stopped,
        "log": res.log,
        "dry_run": dry,
        "written": false,
    });
    if !improved {
        eprintln!(
            "[nudge] no candidate beats the board as it is ({} opens): nothing written",
            res.opens_before
        );
        return Ok(summary);
    }
    if dry {
        return Ok(summary);
    }
    let b = best.expect("improved implies a best candidate");
    let text = nudge::apply(&c.board, &res)?;
    write_file(&out, "")?;
    droute::write_copper(&text, &c.board.nets, &b.routed, &out)
        .map_err(|e| format!("{out}: {e}"))?;
    for (src, ext) in [(&c.pro, "kicad_pro"), (&c.dru, "kicad_dru")] {
        if std::path::Path::new(src).is_file() {
            std::fs::copy(src, out.replace(".kicad_pcb", &format!(".{ext}")))
                .map_err(|e| format!("{src}: {e}"))?;
        }
    }
    // opens counted on the written board (region and routed nets)
    let mut kv = a.kv.clone();
    kv.insert("pcb".into(), out.clone());
    if !c.pro.is_empty() {
        kv.insert("pro".into(), c.pro.clone());
    }
    if std::path::Path::new(&c.dru).is_file() {
        kv.insert("dru".into(), c.dru.clone());
    }
    let c2 = load_ctx(&Args {
        cmd: "nudge".into(),
        kv,
    })?;
    let filt: std::collections::HashSet<usize> = res
        .nets
        .iter()
        .filter_map(|n| c2.board.net_id.get(&c.board.nets[*n]).copied())
        .collect();
    let (opens, open_nets) = count_opens(&c2, Some(res.region), Some(&filt));
    summary["written"] = json!(true);
    summary["out"] = json!(out);
    summary["opens"] = json!(opens);
    summary["open_nets"] = json!(open_nets
        .iter()
        .map(|(n, k, _)| json!({"net": n, "opens": k}))
        .collect::<Vec<_>>());
    // the routed nets on the whole board (saved fills as they are): no net may get worse
    let filt0: std::collections::HashSet<usize> = res.nets.iter().copied().collect();
    let whole = |cx: &Ctx, f: &std::collections::HashSet<usize>| -> HashMap<String, usize> {
        count_opens(cx, None, Some(f))
            .1
            .into_iter()
            .map(|(n, k, _)| (n, k))
            .collect()
    };
    let (wb, wa) = (whole(&c, &filt0), whole(&c2, &filt));
    let worse: Vec<Value> = wa
        .iter()
        .filter(|(n, k)| **k > wb.get(*n).copied().unwrap_or(0))
        .map(|(n, k)| json!({"net": n, "before": wb.get(n).copied().unwrap_or(0), "after": k}))
        .collect();
    summary["board_opens_before"] = json!(wb.values().sum::<usize>());
    summary["board_opens_after"] = json!(wa.values().sum::<usize>());
    if !worse.is_empty() {
        eprintln!("[nudge] warning: nets with more opens on the whole board: {worse:?}");
        if let Some(w) = summary["warnings"].as_array_mut() {
            w.push(json!(format!(
                "nets with more opens on the whole board: {}",
                worse.len()
            )));
        }
    }
    summary["worse_nets"] = json!(worse);
    if a.get("drc").is_some() {
        summary["drc"] = drc_delta(a, &c.pcb, &out)?;
    }
    write_file(
        &out.replace(".kicad_pcb", "_nudge.json"),
        &serde_json::to_string_pretty(&summary).unwrap_or_default(),
    )?;
    eprintln!("[nudge] {} -> {} opens -> {out}", res.opens_before, opens);
    Ok(summary)
}

/// render --pcb B [--roi x0,y0,x1,y1] [--layers F,In1,B] [--nets REGEX] [--hl-net N] [--fills]
/// [--labels] [--courtyards] [--opens] [--drc drc.json] [--halo-min 1.0] [--per-layer]
/// [--px 1200] --out roi.png
fn cmd_render(a: &Args) -> CmdResult {
    let c = load_ctx_filled(a)?;
    let out = a.s("out", "render.png");
    if !out.to_ascii_lowercase().ends_with(".png") {
        return Err(format!("--out {out}: the output file must end in .png"));
    }
    let hl_net = match a.get("hl-net") {
        Some(n) => Some(
            *c.board
                .net_id
                .get(n)
                .ok_or(format!("--hl-net {n}: no such net"))?,
        ),
        None => None,
    };
    let drc = match a.get("drc") {
        Some(f) => {
            render::parse_drc(&std::fs::read_to_string(f).map_err(|e| format!("--drc {f}: {e}"))?)?
        }
        None => Vec::new(),
    };
    let o = render::Opts {
        roi: a.get("roi").map(parse_box).transpose()?,
        layers: render::parse_layers(&a.s("layers", "all"))?,
        nets: a.get("nets").map(|_| full_regex(a, "nets")).transpose()?,
        hl_net,
        fills: a.get("fills").is_some(),
        labels: a.get("labels").is_some(),
        courtyards: a.get("courtyards").is_some(),
        opens: a.get("opens").is_some(),
        halo_min: a.f("halo-min", 1.0),
        drc,
        px: a.u("px", 1200).clamp(100, 8000),
        per_layer: a.get("per-layer").is_some(),
        title: std::path::Path::new(&c.pcb)
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default(),
    };
    let t = Instant::now();
    let m = model::Model::new(&c.board, &c.rules, c.cfg.clone(), model::CopperMode::All);
    let r = render::render(&m, &o)?;
    write_bytes(&out, &r.png)?;
    eprintln!(
        "[render] {}x{} px, {} labels, {} opens in the region in {:.2} s -> {out}",
        r.w,
        r.h,
        r.labels,
        r.rats.len(),
        t.elapsed().as_secs_f64()
    );
    Ok(render::summary(&r, &out))
}

fn write_bytes(path: &str, data: &[u8]) -> Result<(), String> {
    if let Some(d) = std::path::Path::new(path).parent() {
        if !d.as_os_str().is_empty() {
            std::fs::create_dir_all(d).map_err(|e| format!("{}: {e}", d.display()))?;
        }
    }
    std::fs::write(path, data).map_err(|e| format!("{path}: {e}"))
}

/// [`load_ctx`], then with `--native-fill` the copper zones are filled natively (replacing the
/// saved fills), so `stitch`, `fanout`, `opens` and `render` work without kicad-cli
fn load_ctx_filled(a: &Args) -> Result<Ctx, String> {
    let mut c = load_ctx(a)?;
    // --refill-zones: kicad-cli's name for the same thing
    if a.get("native-fill").or(a.get("refill-zones")).is_some() {
        let t = Instant::now();
        let fp = fill_params(a);
        let k = plane::fill_board(&mut c.board, &c.rules, &c.cfg, &|_| true, &fp);
        eprintln!(
            "[fill] native fill: {k} polygons at {} mm in {:.2} s",
            fp.res,
            t.elapsed().as_secs_f64()
        );
    }
    Ok(c)
}

fn fill_params(a: &Args) -> plane::FillParams {
    plane::FillParams {
        res: a.f("fill-res", 0.1).max(0.01),
        min_width: a.f("min-width", 0.25),
    }
}

fn net_arg(c: &Ctx, n: &str) -> Result<usize, String> {
    c.board
        .net_id
        .get(n)
        .copied()
        .ok_or(format!("--net {n}: no such net"))
}

fn nets_regex(c: &Ctx, pat: &str) -> Result<Vec<usize>, String> {
    let re = regex::Regex::new(&format!("^(?:{pat})$")).map_err(|e| format!("--hv: {e}"))?;
    Ok((1..c.board.nets.len())
        .filter(|&i| re.is_match(&c.board.nets[i]))
        .collect())
}

/// plane list | report | fill | create | adjust
fn cmd_plane(a: &Args) -> CmdResult {
    let sub = a
        .get("_sub")
        .map(String::from)
        .unwrap_or_else(|| std::env::args().nth(2).unwrap_or_default());
    match sub.as_str() {
        "list" => {
            let c = load_ctx(a)?;
            let v = plane::detect(&c.board, &c.rules, a.u("min-pads", 4));
            for p in &v {
                eprintln!(
                    "[plane] {:<16} {:<10} {:>4} pads  {} zone(s){}{}",
                    p.net,
                    p.class,
                    p.pads,
                    p.zones.len(),
                    if p.plane { "  plane" } else { "" },
                    if p.candidate {
                        "  candidate (power-like, no zone)"
                    } else {
                        ""
                    }
                );
            }
            Ok(json!({"nets": v}))
        }
        "report" => plane_report(a),
        "fill" => {
            let mut c = load_ctx(a)?;
            let out = out_path(a, &c, "filled.kicad_pcb")?;
            let nets = match a.get("net") {
                Some(n) => vec![net_arg(&c, n)?],
                None => vec![],
            };
            let b0 = c.board.clone();
            let t = Instant::now();
            let k = plane::fill_board(
                &mut c.board,
                &c.rules,
                &c.cfg,
                &|zi| nets.is_empty() || nets.contains(&b0.zones[zi].net),
                &fill_params(a),
            );
            write_file(&out, &plane::write_fills(&c.board.text, &c.board))?;
            eprintln!(
                "[plane] {k} fill polygons in {:.2} s -> {out}",
                t.elapsed().as_secs_f64()
            );
            Ok(json!({"out": out, "polygons": k}))
        }
        "create" | "adjust" => plane_outline(a, &sub),
        _ => Err(format!(
            "plane: expected a subcommand: list, report, fill, create, adjust (got '{sub}')"
        )),
    }
}

/// plane report [--net N] [--fill-res 0.1] [--min-width 0.25] [--stitch-radius 5] [--necks 5]
/// [--png out.png [--roi box] [--px 1200]]
fn plane_report(a: &Args) -> CmdResult {
    let c = load_ctx(a)?;
    let nets: Vec<usize> = match a.get("net") {
        Some(n) => vec![net_arg(&c, n)?],
        None => {
            let mut v: Vec<usize> = c
                .board
                .zones
                .iter()
                .filter(|z| !z.keepout && z.net != 0)
                .map(|z| z.net)
                .collect();
            v.sort();
            v.dedup();
            v
        }
    };
    if nets.is_empty() {
        return Err("no copper zones on the board (plane create makes one)".into());
    }
    let rp = plane::ReportParams {
        fill: fill_params(a),
        stitch_radius: a.f("stitch-radius", 5.0),
        necks: a.u("necks", 5),
        max_islands: a.u("max-islands", 20),
    };
    let t = Instant::now();
    let mut reps = Vec::new();
    let mut filled = c.board.clone();
    for &n in &nets {
        let (r, nb) = plane::report(&filled, &c.rules, &c.cfg, n, &rp);
        filled = nb;
        eprintln!(
            "[plane] {}: {} components, {} unconnected pads, narrowest neck {}",
            r.net,
            r.components,
            r.unconnected_pads.len(),
            r.necks
                .first()
                .map(|k| format!(
                    "{:.2} mm at ({:.2}, {:.2}) {}",
                    k.width_mm, k.at[0], k.at[1], k.layer
                ))
                .unwrap_or_else(|| "-".into())
        );
        for l in &r.layers {
            eprintln!(
                "[plane]   {} {}: fill {:.0}/{:.0} mm2 (lost {:.1} %), {} islands ({} isolated, {} without via), stitch coverage {:.0} %",
                l.zone, l.layer, l.fill_mm2, l.outline_mm2, l.lost_pct, l.islands,
                l.isolated_islands, l.islands_without_via, l.stitch_coverage_pct
            );
        }
        reps.push(r);
    }
    let mut res = json!({"nets": reps, "fill_s": check::round2(t.elapsed().as_secs_f64())});
    if let Some(png) = a.get("png") {
        let mut markers = Vec::new();
        for r in &reps {
            for k in &r.necks {
                markers.push(render::Marker {
                    at: geom::p(k.at[0], k.at[1]),
                    to: None,
                    kind: format!("neck {:.2}", k.width_mm),
                    error: false,
                });
            }
        }
        let hl = if nets.len() == 1 { Some(nets[0]) } else { None };
        let o = render::Opts {
            roi: a.get("roi").map(parse_box).transpose()?,
            layers: render::parse_layers(&a.s("layers", "all"))?,
            nets: None,
            hl_net: hl,
            fills: true,
            labels: a.get("labels").is_some(),
            courtyards: false,
            opens: true,
            halo_min: 0.0,
            drc: markers,
            px: a.u("px", 1200).clamp(100, 8000),
            per_layer: a.get("per-layer").is_some(),
            title: format!(
                "plane report {}",
                reps.iter()
                    .map(|r| r.net.as_str())
                    .collect::<Vec<_>>()
                    .join(",")
            ),
        };
        let m = model::Model::new(&filled, &c.rules, c.cfg.clone(), model::CopperMode::All);
        let r = render::render(&m, &o)?;
        write_bytes(png, &r.png)?;
        res["png"] = json!(png);
    }
    Ok(res)
}

/// plane create --net N --layer L [--region box] | plane adjust --zone NAME [--grow D|max]
/// [--merge]; both: [--hv REGEX --creepage D] [--margin 0] [--res 0.1] --out OUT.kicad_pcb
fn plane_outline(a: &Args, sub: &str) -> CmdResult {
    let c = load_ctx(a)?;
    let out = out_path(a, &c, "plane.kicad_pcb")?;
    let b = &c.board;
    let hv: Vec<usize> = match a.get("hv") {
        Some(p) => nets_regex(&c, p)?,
        None => Vec::new(),
    };
    let grow = match a.get("grow") {
        None => None,
        Some("max") | Some("1") => Some(f64::INFINITY),
        Some(g) => Some(
            g.parse::<f64>()
                .map_err(|_| format!("--grow {g}: expected mm or 'max'"))?,
        ),
    };
    let op = plane::OutlineParams {
        res: a.f("res", 0.1).max(0.02),
        grow,
        hv: hv.clone(),
        creepage: a.f("creepage", 0.0),
        margin: a.f("margin", 0.0),
        min_neck: a.f("min-neck", 1.0),
        min_area: a.f("min-area", 4.0),
    };
    if !hv.is_empty() && op.creepage <= 0.0 {
        return Err("--hv needs --creepage D (mm)".into());
    }
    let m = model::Model::new(b, &c.rules, c.cfg.clone(), model::CopperMode::All);
    let mut seed = plane::seed_of(&b.text);
    let mw = a.f("min-width", 0.25);
    let t = Instant::now();
    let (text, report) = if sub == "create" {
        let net_name = a.get("net").ok_or("missing --net NET")?;
        let net = net_arg(&c, net_name)?;
        let lname = a.get("layer").ok_or("missing --layer L (e.g. In1.Cu)")?;
        let lm = render::parse_layers(lname)?;
        if lm.count_ones() != 1 || lm & b.cu_mask == 0 {
            return Err(format!("--layer {lname}: one copper layer of the board"));
        }
        let l = lm.trailing_zeros() as usize;
        let region = match a.get("region") {
            Some(r) => parse_box(r)?,
            None => b.bbox,
        };
        let seed_poly = vec![
            geom::p(region.x0, region.y0),
            geom::p(region.x1, region.y0),
            geom::p(region.x1, region.y1),
            geom::p(region.x0, region.y1),
        ];
        let lshort = board::CU[l].trim_end_matches(".Cu");
        let name = a.s(
            "name",
            &format!(
                "cp_plane_{}_{lshort}",
                net_name.replace(|ch: char| !ch.is_ascii_alphanumeric(), "")
            ),
        );
        // a re-run replaces its own zone (and its keep-outs) instead of adding one
        let skip: Vec<usize> = (0..b.zones.len())
            .filter(|&i| {
                b.zones[i].name == name || b.zones[i].name.starts_with(&format!("{name}_hv"))
            })
            .collect();
        let (outers, holes) = plane::outline(&m, net, lm, &[seed_poly], &skip, &op);
        if outers.is_empty() {
            return Err(format!("no free area for {net_name} on {lname}"));
        }
        let mut add = String::new();
        let clr = c.rules.class_of(net).clearance;
        for (k, o) in outers.iter().enumerate() {
            let nm = if k == 0 {
                name.clone()
            } else {
                format!("{name}_{}", k + 1)
            };
            let expression =
                sexpr::NetFormat::from_root(&sexpr::parse(&b.text)?)?.expression(net_name)?;
            add += &plane::zone_text(
                &expression,
                board::CU[l],
                &nm,
                a.f("priority", 0.0) as i64,
                mw,
                clr,
                o,
                &mut seed,
            );
        }
        for (k, h) in holes.iter().enumerate() {
            add += &plane::keepout_text(board::CU[l], &format!("{name}_hv{}", k + 1), h, &mut seed);
        }
        let remove: Vec<String> = skip.iter().map(|&i| b.zones[i].name.clone()).collect();
        let area: f64 = outers.iter().map(|o| geom::poly_area(o)).sum();
        (
            plane::replace_zones(&b.text, &remove, &add),
            json!({"zone": name, "net": net_name, "layer": board::CU[l],
                   "outlines": outers.len(), "hv_keepouts": holes.len(),
                   "area_mm2": check::round2(area), "replaced": remove}),
        )
    } else {
        let pat = a
            .get("zone")
            .ok_or("missing --zone NAME (wildcards allowed)")?;
        let sel: Vec<usize> = (0..b.zones.len())
            .filter(|&i| {
                let z = &b.zones[i];
                !z.keepout && z.fp.is_none() && rules::wild(&z.name, pat) && !z.name.is_empty()
            })
            .collect();
        if sel.is_empty() {
            return Err(format!("--zone {pat}: no named copper zone matches"));
        }
        // groups: with --merge, all selected zones of one net and layer set are one region
        let mut groups: Vec<Vec<usize>> = Vec::new();
        for &zi in &sel {
            let z = &b.zones[zi];
            match groups.iter_mut().find(|g| {
                a.get("merge").is_some()
                    && b.zones[g[0]].net == z.net
                    && b.zones[g[0]].layers == z.layers
            }) {
                Some(g) => g.push(zi),
                None => groups.push(vec![zi]),
            }
        }
        let mut text = b.text.clone();
        let mut rep = Vec::new();
        for g in groups {
            let z0 = &b.zones[g[0]];
            let seeds: Vec<Vec<geom::P>> = g.iter().map(|&i| b.zones[i].outline.clone()).collect();
            let (outers, holes) =
                plane::outline(&m, z0.net, z0.layers & b.cu_mask, &seeds, &g, &op);
            if outers.is_empty() {
                rep.push(json!({"zone": z0.name, "error": "no free area left"}));
                continue;
            }
            let before: f64 = seeds.iter().map(|s| geom::poly_area(s)).sum();
            let after: f64 = outers.iter().map(|o| geom::poly_area(o)).sum();
            text = plane::set_zone_outline(&text, &z0.name, &outers[0], &outers[1..], &mut seed)
                .ok_or(format!("zone {} not found in the board text", z0.name))?;
            let merged: Vec<String> = g[1..].iter().map(|&i| b.zones[i].name.clone()).collect();
            let mut add = String::new();
            let l0 = (0..board::NL)
                .find(|l| z0.layers & (1 << l) != 0)
                .unwrap_or(0);
            for (k, h) in holes.iter().enumerate() {
                add += &plane::keepout_text(
                    board::CU[l0],
                    &format!("{}_hv{}", z0.name, k + 1),
                    h,
                    &mut seed,
                );
            }
            text = plane::replace_zones(&text, &merged, &add);
            rep.push(
                json!({"zone": z0.name, "merged": merged, "outlines": outers.len(),
                "hv_keepouts": holes.len(), "area_before_mm2": check::round2(before),
                "area_after_mm2": check::round2(after)}),
            );
        }
        (text, json!({"zones": rep}))
    };
    // the result must parse
    board::parse_board(text.clone())
        .map_err(|e| format!("internal: written board does not parse: {e}"))?;
    write_file(&out, &text)?;
    eprintln!(
        "[plane] {sub} in {:.2} s -> {out}",
        t.elapsed().as_secs_f64()
    );
    let mut r = report;
    r["out"] = json!(out);
    Ok(r)
}

/// route with an explanation of every connection left open (`--explain`, `--explain-img DIR`)
fn cmd_route(a: &Args) -> CmdResult {
    if a.get("progress-json").is_some() {
        progress::enable(Instant::now());
    }
    progress::phase("load");
    cmd_route_core(a)
}

/// the same arguments with some keys replaced
fn args_with(a: &Args, kv: &[(&str, &str)]) -> Args {
    let mut n = Args {
        cmd: a.cmd.clone(),
        kv: a.kv.clone(),
    };
    for (k, v) in kv {
        n.kv.insert(k.to_string(), v.to_string());
    }
    n
}

fn explain_wanted(a: &Args) -> bool {
    a.get("explain").is_some() || a.get("explain-img").is_some()
}

/// explanations (and images with --explain-img DIR) of the open connections of a board
fn explain_board(
    a: &Args,
    c: &Ctx,
    sel: &(dyn Fn(usize) -> bool + Sync),
    pad: Option<&str>,
) -> Result<Value, String> {
    let t = Instant::now();
    let m = model::Model::new(&c.board, &c.rules, c.cfg.clone(), model::CopperMode::All);
    let mut ex = explain::explain_opens(&m, sel, pad, a.u("explain-max", 20));
    if let Some(dir) = a.get("explain-img") {
        for (k, e) in ex.iter_mut().enumerate() {
            let path = explain::img_name(dir, k, e);
            explain::image(&m, e, &path, a.u("explain-px", 900))?;
            e.img = Some(path);
        }
    }
    for e in &ex {
        eprintln!("[explain] {} {} -> {}: {}", e.net, e.from, e.to, e.reason);
    }
    eprintln!(
        "[explain] {} open connections explained in {:.2} s",
        ex.len(),
        t.elapsed().as_secs_f64()
    );
    Ok(json!(ex))
}

/// explain --pcb B [--net N] [--pad REF.PIN] [--explain-img DIR] [--explain-max 20]
fn cmd_explain(a: &Args) -> CmdResult {
    let c = load_ctx_filled(a)?;
    let net = match a.get("net") {
        Some(n) => Some(net_arg(&c, n)?),
        None => None,
    };
    let pad = a.get("pad").map(String::from);
    if let (Some(p), None) = (&pad, net) {
        return Err(format!("--pad {p} needs --net"));
    }
    let ex = explain_board(
        a,
        &c,
        &|n| n != 0 && net.is_none_or(|k| k == n),
        pad.as_deref(),
    )?;
    Ok(json!({"open": ex.as_array().map(|v| v.len()), "explain": ex}))
}

/// `--explain` / `--explain-img DIR` for route, fanout, stitch and plane report: a reason per
/// failure (and a ROI image)
fn explain_post(a: &Args, mut v: Value) -> CmdResult {
    if !explain_wanted(a) {
        return Ok(v);
    }
    let sub = std::env::args().nth(2).unwrap_or_default();
    match a.cmd.as_str() {
        "route" | "autoroute" => {
            let Some(out) = v["out"].as_str().map(String::from) else {
                return Ok(v);
            };
            if v["opens"].as_u64().unwrap_or(0) == 0 {
                v["explain"] = json!([]);
                return Ok(v);
            }
            let c0 = load_ctx(a)?;
            let c = load_ctx(&args_with(
                a,
                &[
                    ("pcb", &out),
                    ("pro", &c0.pro),
                    ("dru", &c0.dru),
                    ("native-fill", "1"),
                ],
            ))?;
            let open: std::collections::HashSet<String> = v["open_nets"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|n| n["net"].as_str().map(String::from))
                .collect();
            let names = c.board.nets.clone();
            v["explain"] = explain_board(a, &c, &|n| open.contains(&names[n]), None)?;
        }
        "plane" if sub == "report" => {
            let mut c = load_ctx(a)?;
            plane::fill_board(&mut c.board, &c.rules, &c.cfg, &|_| true, &fill_params(a));
            let nets: std::collections::HashSet<usize> = v["nets"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|r| r["components"].as_u64().unwrap_or(1) > 1)
                .filter_map(|r| {
                    r["net"]
                        .as_str()
                        .and_then(|n| c.board.net_id.get(n).copied())
                })
                .collect();
            v["explain"] = explain_board(a, &c, &|n| nets.contains(&n), None)?;
        }
        "fanout" | "stitch" => {
            let c = load_ctx_filled(a)?;
            let m = model::Model::new(&c.board, &c.rules, c.cfg.clone(), model::CopperMode::All);
            let net = c.board.net_id.get(&a.s("net", "GND")).copied();
            let key = if a.cmd == "fanout" {
                "islands"
            } else {
                "failed"
            };
            let dir = a.get("explain-img").map(String::from);
            let mut k = 0;
            for it in v[key].as_array_mut().into_iter().flatten() {
                if a.cmd == "fanout" && !it["pad"].is_null() {
                    continue;
                }
                let bl: Vec<model::Blocker> = it["blockers"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(blocker_from)
                    .collect();
                let what = if a.cmd == "fanout" {
                    format!(
                        "no track + via onto the plane within --reach from {}",
                        it["pads"]
                            .as_array()
                            .map(|p| p
                                .iter()
                                .filter_map(|x| x.as_str())
                                .take(4)
                                .collect::<Vec<_>>()
                                .join(", "))
                            .unwrap_or_default()
                    )
                } else {
                    "no legal stitching via spot in this pour island".to_string()
                };
                let reason = if bl.is_empty() {
                    what
                } else {
                    format!(
                        "{what}: nearest blockers {}",
                        bl.iter()
                            .take(3)
                            .map(explain::describe)
                            .collect::<Vec<_>>()
                            .join("; ")
                    )
                };
                it["reason"] = json!(reason);
                eprintln!("[explain] {reason}");
                if let Some(dir) = &dir {
                    // ROI: the island's pads (fanout) or its point (stitch) and the blockers
                    let mut bb = geom::BBox::empty();
                    if let Some(at) = it["at"].as_array() {
                        bb.add(geom::p(
                            at[0].as_f64().unwrap_or(0.0),
                            at[1].as_f64().unwrap_or(0.0),
                        ));
                    }
                    for pn in it["pads"].as_array().into_iter().flatten() {
                        if let Some((r, num)) = pn.as_str().and_then(|s| s.rsplit_once('.')) {
                            if let Some(f) = c.board.fp(r) {
                                if let Some(pd) = f.pads.iter().find(|p| p.num == num) {
                                    bb.add(f.pad_world(pd).0);
                                }
                            }
                        }
                    }
                    for b in bl.iter().take(3) {
                        bb.add(geom::p(b.at[0], b.at[1]));
                    }
                    if !bb.valid() {
                        continue;
                    }
                    let roi = bb.inflate(2.0);
                    let path = format!(
                        "{}/{}_{:02}.png",
                        dir.trim_end_matches(['/', '\\']),
                        a.cmd,
                        k + 1
                    );
                    k += 1;
                    explain::image_roi(
                        &m,
                        net,
                        roi,
                        explain::blocker_markers(&bl),
                        &format!("{} failure {}", a.cmd, k),
                        &path,
                        a.u("explain-px", 900),
                    )?;
                    it["img"] = json!(path);
                }
            }
        }
        _ => {}
    }
    Ok(v)
}

/// a blocker back from its JSON form
fn blocker_from(v: &Value) -> Option<model::Blocker> {
    let kind = match v["kind"].as_str()? {
        "pad" => "pad",
        "track" => "track",
        "via" => "via",
        "zone" => "zone",
        "keepout" => "keepout",
        _ => "graphic",
    };
    Some(model::Blocker {
        net: v["net"].as_str().unwrap_or("").to_string(),
        kind,
        fp: v["fp"].as_str().map(String::from),
        layer: v["layer"].as_str().unwrap_or("").to_string(),
        dist_mm: v["dist_mm"].as_f64().unwrap_or(0.0),
        need_mm: v["need_mm"].as_f64().unwrap_or(0.0),
        locked: v["locked"].as_bool().unwrap_or(false),
        at: [v["at"][0].as_f64()?, v["at"][1].as_f64()?],
    })
}

/// a stage of `autoroute`
struct Stage {
    name: &'static str,
    out: String,
    secs: f64,
    opens: Option<usize>,
    ok: bool,
    note: String,
    detail: Value,
}

/// opens of every net (plane nets included, zones filled natively)
fn opens_all(a: &Args, pcb: &str, base: &Ctx) -> Result<(usize, Vec<Value>), String> {
    let mut kv = vec![("pcb", pcb), ("native-fill", "1")];
    if !base.pro.is_empty() {
        kv.push(("pro", &base.pro));
    }
    if std::path::Path::new(&base.dru).is_file() {
        kv.push(("dru", &base.dru));
    }
    let c = load_ctx_filled(&args_with(a, &kv))?;
    let all: std::collections::HashSet<usize> = (1..c.board.nets.len()).collect();
    let (total, per) = count_opens(&c, None, Some(&all));
    Ok((
        total,
        per.iter()
            .map(|(n, k, _)| json!({"net": n, "opens": k}))
            .collect(),
    ))
}

/// autoroute --pcb B --out OUT.kicad_pcb [--time 600] [--planes auto|In1.Cu=GND,In2.Cu=+3V3]
/// [--plane-nets A,B] [--no-fanout] [--no-stitch] [--stitch-grid G] [--drc] [--allow-drc]
/// [--explain] [--explain-img DIR]
fn cmd_autoroute(a: &Args) -> CmdResult {
    let t0 = Instant::now();
    let budget = a.f("time", 600.0);
    let c = load_ctx(a)?;
    let out = out_path(a, &c, "autorouted.kicad_pcb")?;
    let stem = out.trim_end_matches(".kicad_pcb").to_string();
    let work = format!("{stem}_stages");
    std::fs::create_dir_all(&work).map_err(|e| format!("{work}: {e}"))?;
    let mut base_kv: Vec<(String, String)> = Vec::new();
    if !c.pro.is_empty() {
        base_kv.push(("pro".into(), c.pro.clone()));
    }
    if std::path::Path::new(&c.dru).is_file() {
        base_kv.push(("dru".into(), c.dru.clone()));
    }
    let sub_args = |cmd: &str, kv: &[(&str, &str)]| -> Args {
        let mut n = Args {
            cmd: cmd.to_string(),
            kv: a.kv.clone(),
        };
        for k in [
            "out",
            "time",
            "json",
            "explain",
            "explain-img",
            "planes",
            "drc",
            "net",
        ] {
            n.kv.remove(k);
        }
        for (k, v) in &base_kv {
            n.kv.insert(k.clone(), v.clone());
        }
        for (k, v) in kv {
            n.kv.insert(k.to_string(), v.to_string());
        }
        n
    };
    let left = || budget - t0.elapsed().as_secs_f64();
    let mut stages: Vec<Stage> = Vec::new();
    let mut cur = c.pcb.clone();
    let (opens_in, _) = opens_all(a, &cur, &c)?;
    eprintln!("[autoroute] input: {opens_in} opens, budget {budget:.0} s");

    // 1. planes: zones from --planes L=NET (unless the net already has a zone on L)
    let mut plane_nets: Vec<String> = a
        .get("plane-nets")
        .map(|s| {
            s.split(',')
                .filter(|x| !x.is_empty())
                .map(String::from)
                .collect()
        })
        .unwrap_or_default();
    if let Some(spec) = a.get("planes").filter(|s| *s != "auto") {
        let t = Instant::now();
        let mut made = Vec::new();
        for item in spec.split(',').filter(|s| !s.is_empty()) {
            let (layer, nets) = item
                .split_once('=')
                .ok_or(format!("--planes {item}: expected LAYER=NET"))?;
            for net in nets.split('+') {
                let have = c.board.zones.iter().any(|z| {
                    !z.keepout
                        && c.board.nets[z.net] == net
                        && render::parse_layers(layer).is_ok_and(|m| z.layers & m != 0)
                });
                if have {
                    continue;
                }
                let o = format!("{work}/1_plane_{}.kicad_pcb", made.len() + 1);
                let r = plane_outline(
                    &sub_args(
                        "plane",
                        &[
                            ("pcb", &cur),
                            ("net", net),
                            ("layer", layer),
                            ("out", &o),
                            ("_sub", "create"),
                        ],
                    ),
                    "create",
                )?;
                made.push(r);
                cur = o;
            }
        }
        stages.push(Stage {
            name: "planes",
            out: cur.clone(),
            secs: t.elapsed().as_secs_f64(),
            opens: None,
            ok: true,
            note: format!("{} zone(s) created", made.len()),
            detail: json!(made),
        });
    }
    // plane nets: given, plus every net with a copper zone on an inner layer (its plane); a net
    // with outer pours only has no plane to fan out to, so it is routed like a signal
    {
        let b = board::load(&cur)?;
        let inner = 0b01_1110u8; // In1.Cu..In4.Cu in board::CU order
        for z in b
            .zones
            .iter()
            .filter(|z| !z.keepout && z.net != 0 && z.layers & inner != 0)
        {
            let n = &b.nets[z.net];
            if !plane_nets.contains(n) {
                plane_nets.push(n.clone());
            }
        }
    }
    // pour nets: zones on outer layers only (two-layer boards); routed after the signals, on the
    // filled pours, so tracks only join the islands the pours leave apart
    let pour_nets: Vec<String> = {
        let b = board::load(&cur)?;
        let mut v: Vec<String> = Vec::new();
        for z in b.zones.iter().filter(|z| !z.keepout && z.net != 0) {
            let n = &b.nets[z.net];
            if !plane_nets.contains(n) && !v.contains(n) && !n.contains(',') {
                v.push(n.clone());
            }
        }
        v
    };
    let pn = plane_nets.join(",");
    eprintln!("[autoroute] plane nets: {pn}");
    // inner layers that are planes (plane-net zones cover at least half the board): signal tracks
    // stay off them so the planes stay whole; vias still pass (the plane refills around them)
    let sig_layers: String = match a.s("signal-layers", "auto").as_str() {
        "all" => String::new(),
        "auto" => {
            let b = board::load(&cur)?;
            let area = |p: &[geom::P]| {
                let n = p.len();
                (0..n)
                    .map(|i| p[i].x * p[(i + 1) % n].y - p[(i + 1) % n].x * p[i].y)
                    .sum::<f64>()
                    .abs()
                    / 2.0
            };
            let board_area: f64 = if b.edge.is_empty() {
                (b.bbox.x1 - b.bbox.x0) * (b.bbox.y1 - b.bbox.y0)
            } else {
                b.outer_outlines().iter().map(|&i| area(&b.edge[i])).sum()
            };
            let mut planes = 0u8;
            for l in 1..5 {
                let cov: f64 = b
                    .zones
                    .iter()
                    .filter(|z| !z.keepout && z.layers & (1 << l) != 0)
                    .filter(|z| plane_nets.contains(&b.nets[z.net]))
                    .map(|z| area(&z.outline))
                    .sum();
                if board_area > 0.0 && cov >= 0.5 * board_area {
                    planes |= 1 << l;
                }
            }
            let keep: Vec<&str> = (0..board::CU.len())
                .filter(|l| b.cu_mask & (1 << l) != 0 && planes & (1 << l) == 0)
                .map(|l| board::CU[l])
                .collect();
            if planes == 0 || keep.is_empty() {
                String::new()
            } else {
                eprintln!(
                    "[autoroute] signal layers: {} (inner planes kept whole)",
                    keep.join(",")
                );
                keep.join(",")
            }
        }
        list => list.to_string(),
    };
    let skip_sig = plane_nets
        .iter()
        .chain(&pour_nets)
        .cloned()
        .collect::<Vec<_>>()
        .join(",");
    if !pour_nets.is_empty() {
        eprintln!(
            "[autoroute] pour nets (routed after the signals): {}",
            pour_nets.join(",")
        );
    }
    // zone name patterns per plane net: inner zones are the plane, outer ones the pours
    let zone_pats = |pcb: &str, net: &str| -> Result<(String, String), String> {
        let b = board::load(pcb)?;
        let esc = |z: &board::Zone| regex::escape(&z.key(&b.nets));
        let zs: Vec<&board::Zone> = b
            .zones
            .iter()
            .filter(|z| !z.keepout && b.nets[z.net] == net)
            .collect();
        let inner: Vec<String> = zs
            .iter()
            .filter(|z| z.layers & 0x1e != 0)
            .map(|z| esc(z))
            .collect();
        let outer: Vec<String> = zs
            .iter()
            .filter(|z| z.layers & 0x1e == 0)
            .map(|z| esc(z))
            .collect();
        let plane = if inner.is_empty() {
            outer.clone()
        } else {
            inner
        };
        Ok((plane.join("|"), outer.join("|")))
    };
    // 2. fanout of the plane nets
    if a.get("no-fanout").is_none() && !plane_nets.is_empty() {
        let t = Instant::now();
        let mut det = Vec::new();
        for net in &plane_nets {
            if left() < budget * 0.5 {
                break;
            }
            let (plane, _) = zone_pats(&cur, net)?;
            if plane.is_empty() {
                continue;
            }
            let o = format!("{work}/2_fanout_{}.kicad_pcb", det.len() + 1);
            match cmd_fanout(&sub_args(
                "fanout",
                &[
                    ("pcb", &cur),
                    ("net", net),
                    ("plane", &plane),
                    ("out", &o),
                    ("native-fill", "1"),
                    ("plane-only", "1"),
                ],
            )) {
                Ok(v) => {
                    det.push(json!({"net": net, "open_islands": v["open_islands"], "connected": v["connected"]}));
                    cur = o;
                }
                Err(e) => det.push(json!({"net": net, "error": e})),
            }
        }
        stages.push(Stage {
            name: "fanout",
            out: cur.clone(),
            secs: t.elapsed().as_secs_f64(),
            opens: None,
            ok: true,
            note: format!("{} plane net(s)", det.len()),
            detail: json!(det),
        });
    }
    // 3. signal routing (negotiated) + reroute of blockers, within the time left
    {
        let t = Instant::now();
        // reserve for the gate, plus a fifth of the budget for the pour nets' own stage
        let reserve = (budget * 0.15).clamp(1.0, 90.0)
            + if !pour_nets.is_empty() {
                budget * 0.2
            } else if !plane_nets.is_empty() {
                // plane pads the fanout could not join are routed after the signals
                budget * 0.1
            } else {
                0.0
            };
        // at least 40 % of the budget (at most 10 s): small budgets stay small
        let tl = (left() - reserve).max((budget * 0.4).min(10.0)).max(1.0);
        let o = format!("{work}/3_route.kicad_pcb");
        let tls = format!("{tl:.0}");
        let mut kv: Vec<(&str, &str)> = vec![("pcb", &cur), ("out", &o), ("time-limit", &tls)];
        if !skip_sig.is_empty() {
            kv.push(("plane-nets", &skip_sig));
        }
        if !sig_layers.is_empty() {
            kv.push(("signal-layers", &sig_layers));
        }
        if a.get("no-reroute").is_none() {
            kv.push(("reroute-blockers", "1"));
        }
        match cmd_route_core(&sub_args("route", &kv)) {
            Ok(v) => {
                stages.push(Stage {
                    name: "route",
                    out: o.clone(),
                    secs: t.elapsed().as_secs_f64(),
                    opens: v["opens"].as_u64().map(|x| x as usize),
                    ok: true,
                    note: format!(
                        "{} segments, {} vias, stopped: {}",
                        v["segments"], v["vias"], v["stopped"]
                    ),
                    detail: json!({"open_nets": v["open_nets"], "aes_cleanup": v["aes"]["cleanup_level"], "exact_fail": v["aes"]["exact_fail"]}),
                });
                cur = o;
            }
            Err(e) => stages.push(Stage {
                name: "route",
                out: cur.clone(),
                secs: t.elapsed().as_secs_f64(),
                opens: None,
                ok: false,
                note: e,
                detail: json!(null),
            }),
        }
    }
    // 3b. optional second pass (--second-pass) for the nets left open: wider search windows and a
    // larger node budget, in the time left (no gain on the stress board so far, see README)
    let open_sig: Vec<String> = stages
        .last()
        .filter(|s| s.name == "route")
        .and_then(|s| s.detail["open_nets"].as_array().cloned())
        .unwrap_or_default()
        .iter()
        .filter_map(|n| n["net"].as_str().map(String::from))
        .filter(|n| !n.contains([',', '*', '?']))
        .collect();
    if !open_sig.is_empty() && left() > budget * 0.25 && a.get("second-pass").is_some() {
        let t = Instant::now();
        let reserve = (budget * 0.1).clamp(5.0, 60.0);
        let tls = format!("{:.0}", (left() - reserve).max(10.0));
        let o = format!("{work}/3b_route.kicad_pcb");
        let nets = open_sig.join(",");
        let mut kv: Vec<(&str, &str)> = vec![
            ("pcb", &cur),
            ("out", &o),
            ("time-limit", &tls),
            ("nets", &nets),
            ("job-margin", "20"),
            ("max-expand", "20000000"),
            ("reroute-blockers", "1"),
        ];
        if !skip_sig.is_empty() {
            kv.push(("plane-nets", &skip_sig));
        }
        if !sig_layers.is_empty() {
            kv.push(("signal-layers", &sig_layers));
        }
        match cmd_route_core(&sub_args("route", &kv)) {
            Ok(v) => {
                let before = open_sig.len();
                stages.push(Stage {
                    name: "route2",
                    out: o.clone(),
                    secs: t.elapsed().as_secs_f64(),
                    opens: v["opens"].as_u64().map(|x| x as usize),
                    ok: true,
                    note: format!("{before} open nets retried, {} vias added", v["vias"]),
                    detail: json!({"open_nets": v["open_nets"]}),
                });
                cur = o;
            }
            Err(e) => stages.push(Stage {
                name: "route2",
                out: cur.clone(),
                secs: t.elapsed().as_secs_f64(),
                opens: None,
                ok: false,
                note: e,
                detail: json!(null),
            }),
        }
    }
    // 3c. pour nets: fill the zones natively, then route only what the fill leaves open (the
    // router starts on the fill), without ripping signals; new tracks can cut another pour, so
    // refill and repeat (up to 3 rounds) until the refilled board has no open pour net
    // runs even when the signals overran the budget: unconnected pour nets (GND) are worse than
    // a few seconds over it (each round then gets at least 15 % of the budget, at most 10 s)
    // plane nets that the plane fills and the fanout left open (pads the plane cannot reach,
    // e.g. dense through-hole pins) are routed here too, with tracks on the signal layers
    let mut late = pour_nets.clone();
    if !plane_nets.is_empty() {
        let (_, per) = opens_all(a, &cur, &c)?;
        for n in &per {
            if let Some(x) = n["net"].as_str() {
                if plane_nets.iter().any(|p| p == x) && !late.iter().any(|l| l == x) {
                    late.push(x.to_string());
                }
            }
        }
    }
    let pn_late = plane_nets
        .iter()
        .filter(|p| !late.contains(p))
        .cloned()
        .collect::<Vec<_>>()
        .join(",");
    if !late.is_empty() {
        let t = Instant::now();
        let mut rounds = Vec::new();
        for round in 1..=3 {
            let fo = format!("{work}/3c_fill_{round}.kicad_pcb");
            let o = format!("{work}/3c_pours_{round}.kicad_pcb");
            let tls = format!(
                "{:.0}",
                (left() - (budget * 0.05).clamp(2.0, 30.0))
                    .max((budget * 0.15).min(10.0))
                    .max(1.0)
            );
            let nets = late.join(",");
            let mut kv: Vec<(&str, &str)> = vec![
                ("pcb", &fo),
                ("out", &o),
                ("time-limit", &tls),
                ("nets", &nets),
            ];
            if !pn_late.is_empty() {
                kv.push(("plane-nets", &pn_late));
            }
            if !sig_layers.is_empty() {
                kv.push(("signal-layers", &sig_layers));
            }
            let r = cmd_plane(&sub_args(
                "plane",
                &[("pcb", &cur), ("out", &fo), ("_sub", "fill")],
            ))
            .and_then(|_| cmd_route_core(&sub_args("route", &kv)));
            match r {
                Ok(v) => {
                    cur = o;
                    let (left_open, per) = opens_all(a, &cur, &c)?;
                    let pour_open = per
                        .iter()
                        .filter(|n| {
                            n["net"]
                                .as_str()
                                .is_some_and(|x| late.iter().any(|p| p == x))
                        })
                        .count();
                    rounds.push(json!({"round": round, "segments": v["segments"], "vias": v["vias"], "opens": left_open, "open_pour_nets": pour_open}));
                    if pour_open == 0 || left() < 5.0 {
                        break;
                    }
                }
                Err(e) => {
                    rounds.push(json!({"round": round, "error": e}));
                    break;
                }
            }
        }
        let opens = rounds
            .last()
            .and_then(|r| r["opens"].as_u64())
            .map(|x| x as usize);
        stages.push(Stage {
            name: "pours",
            out: cur.clone(),
            secs: t.elapsed().as_secs_f64(),
            opens,
            ok: opens.is_some(),
            note: format!(
                "{} pour/plane net(s), {} round(s)",
                late.len(),
                rounds.len()
            ),
            detail: json!({"nets": late, "rounds": rounds}),
        });
    }
    // 3d. starved thermal reliefs (kicad-cli: fewer than 2 spokes): a short stub track from the
    // pad into its zone's fill counts as a spoke
    // (skipped once the budget is spent: on large boards its fine refill takes seconds)
    if a.get("no-thermals").is_none() && !c.board.zones.is_empty() && left() > 0.0 {
        let t = Instant::now();
        let o = format!("{work}/3d_thermals.kicad_pcb");
        match cmd_thermals(&sub_args("thermals", &[("pcb", &cur), ("out", &o)])) {
            // a board without starved pads gets no stage
            Ok(v) if v["starved"].as_u64() == Some(0) => {}
            Ok(v) => {
                let n = v["stubs"].as_u64().unwrap_or(0);
                if n > 0 {
                    cur = o.clone();
                }
                stages.push(Stage {
                    name: "thermals",
                    out: cur.clone(),
                    secs: t.elapsed().as_secs_f64(),
                    opens: None,
                    ok: true,
                    note: format!("{n} stub(s) for {} starved pad(s)", v["starved"]),
                    detail: v,
                });
            }
            Err(e) => stages.push(Stage {
                name: "thermals",
                out: cur.clone(),
                secs: t.elapsed().as_secs_f64(),
                opens: None,
                ok: false,
                note: e,
                detail: json!(null),
            }),
        }
    }
    // 4. stitching vias for outer pours of the plane nets
    if a.get("no-stitch").is_none() && left() > 5.0 {
        let t = Instant::now();
        let mut det = Vec::new();
        for net in &plane_nets {
            let (plane, outer) = zone_pats(&cur, net)?;
            if plane.is_empty() || outer.is_empty() || plane == outer {
                continue;
            }
            let o = format!("{work}/4_stitch_{}.kicad_pcb", det.len() + 1);
            let grid = a.s("stitch-grid", "0");
            match cmd_stitch(&sub_args(
                "stitch",
                &[
                    ("pcb", &cur),
                    ("net", net),
                    ("zones", &outer),
                    ("plane", &plane),
                    ("grid", &grid),
                    ("out", &o),
                    ("native-fill", "1"),
                ],
            )) {
                Ok(v) => {
                    det.push(json!({"net": net, "vias": v["vias"].as_array().map(|x| x.len()), "failed": v["failed"].as_array().map(|x| x.len())}));
                    cur = o;
                }
                Err(e) => det.push(json!({"net": net, "error": e})),
            }
        }
        if !det.is_empty() {
            stages.push(Stage {
                name: "stitch",
                out: cur.clone(),
                secs: t.elapsed().as_secs_f64(),
                opens: None,
                ok: true,
                note: format!("{} net(s)", det.len()),
                detail: json!(det),
            });
        }
    }
    // 5. quality gate: opens with native fills, KiCad DRC against the input (optional); the best
    // stage without new DRC errors is written
    let t = Instant::now();
    let (opens_out, open_nets) = opens_all(a, &cur, &c)?;
    let mut chosen = cur.clone();
    let mut gate = json!({"drc": "skipped (use --drc with kicad-cli)"});
    if a.get("drc").is_some() {
        let mut cands: Vec<String> = stages.iter().rev().map(|s| s.out.clone()).collect();
        cands.dedup();
        let mut tried = Vec::new();
        let mut found = None;
        for cand in cands {
            if cand == c.pcb {
                continue;
            }
            let d = drc_delta(a, &c.pcb, &cand)?;
            let ne = d["new_errors"].as_u64().unwrap_or(0);
            tried.push(json!({"board": cand, "new_errors": ne, "new": d["new"]}));
            if ne == 0 || a.get("allow-drc").is_some() {
                found = Some(cand);
                break;
            }
        }
        match found {
            Some(f) => chosen = f,
            None => {
                return Err(format!(
                    "quality gate: every stage adds KiCad DRC errors (see {work}): {}; nothing written (--allow-drc writes anyway)",
                    serde_json::to_string(&tried).unwrap_or_default()
                ));
            }
        }
        gate = json!({"drc": tried});
    }
    let (opens_final, open_final) = if chosen == cur {
        (opens_out, open_nets)
    } else {
        opens_all(a, &chosen, &c)?
    };
    std::fs::copy(&chosen, &out).map_err(|e| format!("{out}: {e}"))?;
    stages.push(Stage {
        name: "gate",
        out: chosen.clone(),
        secs: t.elapsed().as_secs_f64(),
        opens: Some(opens_final),
        ok: true,
        note: format!("written from {chosen}"),
        detail: gate,
    });
    let total = t0.elapsed().as_secs_f64();
    for s in &stages {
        eprintln!(
            "[autoroute] {:<7} {:>7.1} s  {}{}",
            s.name,
            s.secs,
            s.opens.map(|o| format!("{o} opens  ")).unwrap_or_default(),
            s.note
        );
    }
    eprintln!(
        "[autoroute] {opens_in} -> {opens_final} opens in {total:.1} s (budget {budget:.0} s) -> {out}"
    );
    Ok(json!({
        "out": out,
        "opens_before": opens_in,
        "opens": opens_final,
        "open_nets": open_final,
        "plane_nets": plane_nets,
        "budget_s": budget,
        "within_budget": total <= budget,
        "stages": stages.iter().map(|s| json!({"stage": s.name, "s": check::round2(s.secs), "ok": s.ok,
            "opens": s.opens, "note": s.note, "out": s.out, "detail": s.detail})).collect::<Vec<_>>(),
    }))
}
