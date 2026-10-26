//! `tune`: accordion meanders bring a group of nets to the length of the longest (length
//! matching), DRC-clean.

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
fn group_is_matched_to_the_longest_net() {
    let d = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("tune");
    std::fs::create_dir_all(&d).unwrap();
    let routed = d.join("routed.kicad_pcb");
    let tuned = d.join("tuned.kicad_pcb");
    let (routed, tuned) = (routed.to_str().unwrap(), tuned.to_str().unwrap());
    let pro = DEMO.replace("kicad_pcb", "kicad_pro");
    let dru = DEMO.replace("kicad_pcb", "kicad_dru");
    assert_eq!(cp(&["route", "--pcb", DEMO, "--out", routed])["opens"], 0);
    let pat = "PWM_.*|ADC[12]";
    let before = cp(&["netinfo", "--pcb", routed, "--pro", &pro, "--match", pat]);
    assert!(
        before["groups"][0]["skew_mm"].as_f64().unwrap() > 1.0,
        "{before}"
    );
    let r = cp(&[
        "tune", "--pcb", routed, "--pro", &pro, "--dru", &dru, "--match", pat, "--out", tuned,
    ]);
    assert!(
        r["nets"]
            .as_array()
            .unwrap()
            .iter()
            .all(|n| n["short_mm"] == 0.0),
        "{r}"
    );
    let after = cp(&["netinfo", "--pcb", tuned, "--pro", &pro, "--match", pat]);
    assert!(
        after["groups"][0]["skew_mm"].as_f64().unwrap() < 0.06,
        "{after}"
    );
    let drc = cp(&["drc", "--pcb", tuned, "--pro", &pro, "--dru", &dru]);
    assert_eq!(drc["violations"], 0, "{drc}");
    assert_eq!(drc["unconnected"], 0, "{drc}");
}
