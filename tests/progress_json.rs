//! `route --progress-json` (#18): NDJSON events on stderr (`phase`, `iter`, last `done`);
//! without the flag no event is written and the routed board is the same.

use std::process::Command;

const DEMO: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/examples/demo/demo.kicad_pcb");

fn dir() -> std::path::PathBuf {
    let d = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("progress_json");
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// route the demo board to `out`; returns stderr
fn route(out: &std::path::Path, extra: &[&str]) -> String {
    let o = Command::new(env!("CARGO_BIN_EXE_copperpath"))
        .args(["route", "--pcb", DEMO, "--threads", "2", "--out"])
        .arg(out)
        .args(extra)
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&o.stderr).to_string();
    assert!(o.status.success(), "{err}");
    err
}

#[test]
fn progress_events_parse_and_end_with_done() {
    let d = dir();
    let out = d.join("with.kicad_pcb");
    let err = route(&out, &["--progress-json"]);
    let evs: Vec<serde_json::Value> = err
        .lines()
        .filter(|l| l.starts_with('{'))
        .map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("{l}: {e}")))
        .collect();
    assert!(evs.iter().all(|e| e["t"].is_number()), "{evs:?}");
    let phases: Vec<&str> = evs
        .iter()
        .filter(|e| e["ev"] == "phase")
        .map(|e| e["phase"].as_str().unwrap())
        .collect();
    assert_eq!(phases.first(), Some(&"load"), "{phases:?}");
    for p in ["droute", "clean", "write", "check"] {
        assert!(phases.contains(&p), "{p} missing in {phases:?}");
    }
    let iters: Vec<&serde_json::Value> = evs.iter().filter(|e| e["ev"] == "iter").collect();
    assert!(!iters.is_empty());
    for it in &iters {
        for k in [
            "iter",
            "conflict_cells",
            "nets_conflicted",
            "nets_left",
            "nets_total",
            "iters_max",
        ] {
            assert!(it[k].is_u64(), "{k} in {it}");
        }
        assert!(it["eta_s"].is_number() || it["eta_s"].is_null(), "{it}");
        assert!(it["nets_left"].as_u64() <= it["nets_total"].as_u64());
    }
    // t never runs backwards
    let ts: Vec<f64> = evs.iter().map(|e| e["t"].as_f64().unwrap()).collect();
    assert!(ts.windows(2).all(|w| w[0] <= w[1]), "{ts:?}");
    let done = evs.last().unwrap();
    assert_eq!(done["ev"], "done", "{done}");
    assert_eq!(done["v"], 1);
    assert_eq!(done["ok"], true);
    assert_eq!(
        done["routed"].as_u64().unwrap() + done["failed"].as_u64().unwrap(),
        iters[0]["nets_total"].as_u64().unwrap()
    );
    assert_eq!(done["out"], out.to_string_lossy().as_ref());

    // without the flag: no events, and the same board
    let out2 = d.join("without.kicad_pcb");
    let err2 = route(&out2, &[]);
    assert!(!err2.lines().any(|l| l.starts_with('{')), "{err2}");
    assert_eq!(
        std::fs::read(&out).unwrap(),
        std::fs::read(&out2).unwrap(),
        "--progress-json changed the routed board"
    );
}

#[test]
fn progress_done_reports_errors() {
    let o = Command::new(env!("CARGO_BIN_EXE_copperpath"))
        .args(["route", "--progress-json", "--pcb", "missing.kicad_pcb"])
        .output()
        .unwrap();
    assert!(!o.status.success());
    let err = String::from_utf8_lossy(&o.stderr);
    let last = err.lines().filter(|l| l.starts_with('{')).last().unwrap();
    let done: serde_json::Value = serde_json::from_str(last).unwrap();
    assert_eq!(done["ev"], "done");
    assert_eq!(done["ok"], false);
    assert!(done["error"].as_str().unwrap().contains("not found"));
}
