//! `autoroute`: demo board with a GND plane on In1 in one command: zone created, GND pads fanned
//! out to it, signals routed, every connection closed (GND through the native plane fill),
//! stage timing reported, budget honoured.

use std::process::Command;

#[test]
fn autoroute_demo_with_gnd_plane() {
    let dir = std::env::temp_dir().join("cp_autoroute_test");
    std::fs::create_dir_all(&dir).unwrap();
    let out = dir.join("demo_auto.kicad_pcb");
    let o = Command::new(env!("CARGO_BIN_EXE_copperpath"))
        .args([
            "autoroute",
            "--pcb",
            "examples/demo/demo.kicad_pcb",
            "--planes",
            "In1.Cu=GND",
            "--time",
            "120",
            "--threads",
            "2",
            "--json",
            "--out",
        ])
        .arg(&out)
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let v: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(v["opens_before"], 50, "{v}");
    assert_eq!(v["opens"], 0, "{v}");
    assert_eq!(v["within_budget"], true);
    let names: Vec<&str> = v["stages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["stage"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["planes", "fanout", "route", "gate"], "{v}");
    let text = std::fs::read_to_string(&out).unwrap();
    assert!(text.contains("cp_plane_GND_In1"));
    // the project and rules files travel with the output (later commands use its net classes)
    assert!(out.with_extension("kicad_pro").is_file());
    assert!(out.with_extension("kicad_dru").is_file());
    assert!(v["plane_nets"].as_array().unwrap().contains(&"GND".into()));
}

/// two-layer board with an outer GND pour: GND is routed after the signals, on the filled pour,
/// and only for what the pour leaves apart (a GND pad outside the zone)
#[test]
fn autoroute_two_layer_pour_net() {
    let dir = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("autoroute_pour");
    std::fs::create_dir_all(&dir).unwrap();
    let pad = |r: &str, x: f64, y: f64, net: &str| {
        format!(
            "(footprint \"P\" (layer \"F.Cu\") (at {x} {y}) (property \"Reference\" \"{r}\") \
             (pad \"1\" smd rect (at 0 0) (size 1 1) (layers \"F.Cu\") (net \"{net}\")))"
        )
    };
    let pcb = dir.join("pour.kicad_pcb");
    std::fs::write(
        &pcb,
        format!(
            "(kicad_pcb (version 20260206) (layers (0 \"F.Cu\" signal) (2 \"B.Cu\" signal) (25 \"Edge.Cuts\" user)) \
             (gr_rect (start 0 0) (end 40 20) (stroke (width 0.1)) (layer \"Edge.Cuts\")) {}{}{}{}{} \
             (zone (net \"GND\") (layers \"F.Cu\") (name \"pour\") (connect_pads (clearance 0.3)) \
               (min_thickness 0.25) (fill (thermal_gap 0.5) (thermal_bridge_width 0.5)) \
               (polygon (pts (xy 1 1) (xy 20 1) (xy 20 19) (xy 1 19)))))",
            pad("G1", 5.0, 5.0, "GND"),
            pad("G2", 5.0, 15.0, "GND"),
            // outside the pour: needs a track
            pad("G3", 32.0, 10.0, "GND"),
            pad("A1", 10.0, 10.0, "A"),
            pad("A2", 30.0, 15.0, "A"),
        ),
    )
    .unwrap();
    let out = dir.join("pour_routed.kicad_pcb");
    let o = Command::new(env!("CARGO_BIN_EXE_copperpath"))
        .args([
            "autoroute",
            "--time",
            "60",
            "--threads",
            "2",
            "--json",
            "--pcb",
        ])
        .arg(&pcb)
        .arg("--out")
        .arg(&out)
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let v: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(v["opens"], 0, "{v}");
    let stages: Vec<&str> = v["stages"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|s| s["stage"].as_str())
        .collect();
    assert!(stages.contains(&"pours"), "{v}");
}

/// unnamed inner zone (boards from KiCad 5 have no zone names): the plane is still found by its
/// key NET@LAYER and the GND pads are fanned out to it
#[test]
fn autoroute_unnamed_inner_plane() {
    let dir = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("autoroute_unnamed");
    std::fs::create_dir_all(&dir).unwrap();
    let pad = |r: &str, x: f64, y: f64, net: &str| {
        format!(
            "(footprint \"P\" (layer \"F.Cu\") (at {x} {y}) (property \"Reference\" \"{r}\") \
             (pad \"1\" smd rect (at 0 0) (size 1 1) (layers \"F.Cu\") (net \"{net}\")))"
        )
    };
    let pcb = dir.join("unnamed.kicad_pcb");
    std::fs::write(
        &pcb,
        format!(
            "(kicad_pcb (version 20260206) (layers (0 \"F.Cu\" signal) (4 \"In1.Cu\" signal) \
             (6 \"In2.Cu\" signal) (2 \"B.Cu\" signal) (25 \"Edge.Cuts\" user)) \
             (gr_rect (start 0 0) (end 40 20) (stroke (width 0.1)) (layer \"Edge.Cuts\")) {}{}{}{} \
             (zone (net \"GND\") (layers \"In1.Cu\") (connect_pads (clearance 0.3)) \
               (min_thickness 0.25) (fill (thermal_gap 0.5) (thermal_bridge_width 0.5)) \
               (polygon (pts (xy 1 1) (xy 39 1) (xy 39 19) (xy 1 19)))))",
            pad("G1", 5.0, 5.0, "GND"),
            pad("G2", 35.0, 15.0, "GND"),
            pad("A1", 10.0, 10.0, "A"),
            pad("A2", 30.0, 10.0, "A"),
        ),
    )
    .unwrap();
    let out = dir.join("unnamed_routed.kicad_pcb");
    let o = Command::new(env!("CARGO_BIN_EXE_copperpath"))
        .args([
            "autoroute",
            "--time",
            "60",
            "--threads",
            "2",
            "--json",
            "--pcb",
        ])
        .arg(&pcb)
        .arg("--out")
        .arg(&out)
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let v: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(v["opens"], 0, "{v}");
}
