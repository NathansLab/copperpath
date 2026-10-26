//! `ripup`: remove the copper of some nets, then `route --nets` puts them back (agent loop).

use std::process::Command;

const DEMO: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/examples/demo/demo.kicad_pcb");

fn cp(args: &[&str]) -> serde_json::Value {
    let o = Command::new(env!("CARGO_BIN_EXE_copperpath"))
        .args(args)
        .args(["--threads", "2", "--json"])
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    serde_json::from_slice(&o.stdout).unwrap()
}

#[test]
fn ripup_then_reroute() {
    let d = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("ripup");
    std::fs::create_dir_all(&d).unwrap();
    let (pro, dru) = (
        DEMO.replace("kicad_pcb", "kicad_pro"),
        DEMO.replace("kicad_pcb", "kicad_dru"),
    );
    let p = |n: &str| d.join(n).to_str().unwrap().to_string();
    let (routed, ripped, again) = (
        p("routed.kicad_pcb"),
        p("ripped.kicad_pcb"),
        p("again.kicad_pcb"),
    );
    assert_eq!(cp(&["route", "--pcb", DEMO, "--out", &routed])["opens"], 0);
    let r = cp(&[
        "ripup", "--pcb", &routed, "--pro", &pro, "--nets", "CLK", "--out", &ripped,
    ]);
    assert!(r["removed"].as_u64().unwrap() > 0, "{r}");
    let o = cp(&["opens", "--pcb", &ripped, "--pro", &pro]);
    assert!(o["opens"].as_u64().unwrap() > 0, "{o}");
    let v = cp(&[
        "route", "--pcb", &ripped, "--pro", &pro, "--dru", &dru, "--nets", "CLK", "--out", &again,
    ]);
    assert_eq!(v["opens"], 0, "{v}");
}
