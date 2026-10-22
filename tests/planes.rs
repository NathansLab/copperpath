//! Planes on a synthetic 4-layer board: `plane create` writes one GND zone on In1 (idempotent),
//! cuts it back from an HV net, `plane report` fills it natively and reports islands,
//! unconnected pads and the neck left by a signal track, `plane fill` writes fills that `opens`
//! reads, `plane adjust --grow` enlarges a zone.

use std::process::Command;

fn smd(r: &str, x: f64, y: f64, net: &str) -> String {
    format!(
        "(footprint \"P\" (layer \"F.Cu\") (at {x} {y}) (property \"Reference\" \"{r}\") \
         (pad \"1\" smd rect (at 0 0) (size 1 1) (layers \"F.Cu\") (net \"{net}\")))"
    )
}

/// 40 x 30 mm board; GND pads on F.Cu, each with a via; a SIG track on In1 at x = 20 from the
/// top edge down to y = `wall`
fn board(dir: &std::path::Path, name: &str, wall: f64) -> std::path::PathBuf {
    std::fs::create_dir_all(dir).unwrap();
    let pcb = dir.join(format!("{name}.kicad_pcb"));
    let mut items = String::new();
    for (k, (x, y)) in [(8.0, 10.0), (8.0, 20.0), (32.0, 10.0), (32.0, 20.0)]
        .iter()
        .enumerate()
    {
        items += &smd(&format!("R{}", k + 1), *x, *y, "GND");
        items += &format!(
            "(segment (start {x} {y}) (end {} {y}) (width 0.25) (layer \"F.Cu\") (net \"GND\")) \
             (via (at {} {y}) (size 0.6) (drill 0.3) (layers \"F.Cu\" \"B.Cu\") (net \"GND\")) ",
            x + 1.5,
            x + 1.5
        );
    }
    items += &smd("H1", 36.0, 4.0, "HV");
    items += &smd("H2", 36.0, 26.0, "HV");
    items += &format!(
        "(segment (start 20 0.5) (end 20 {wall}) (width 0.25) (layer \"In1.Cu\") (net \"SIG\"))"
    );
    let text = format!(
        "(kicad_pcb (version 20260206) (layers (0 \"F.Cu\" signal) (4 \"In1.Cu\" signal) \
         (6 \"In2.Cu\" signal) (2 \"B.Cu\" signal) (25 \"Edge.Cuts\" user)) \
         (gr_rect (start 0 0) (end 40 30) (stroke (width 0.1)) (layer \"Edge.Cuts\")) {items})"
    );
    std::fs::write(&pcb, text).unwrap();
    pcb
}

fn run(args: &[&str]) -> serde_json::Value {
    let o = Command::new(env!("CARGO_BIN_EXE_copperpath"))
        .args(args)
        .args(["--threads", "2", "--json"])
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    serde_json::from_slice(&o.stdout).unwrap()
}

fn s(p: &std::path::Path) -> &str {
    p.to_str().unwrap()
}

#[test]
fn plane_create_report_fill() {
    let dir = std::env::temp_dir().join("cp_planes_test");
    // the wall leaves a gap at the bottom: one island with a neck
    let pcb = board(&dir, "gap", 25.0);
    let out = dir.join("gap_plane.kicad_pcb");
    let v = run(&[
        "plane",
        "create",
        "--pcb",
        s(&pcb),
        "--net",
        "GND",
        "--layer",
        "In1.Cu",
        "--out",
        s(&out),
    ]);
    assert_eq!(v["zone"], "cp_plane_GND_In1");
    let text = std::fs::read_to_string(&out).unwrap();
    assert_eq!(text.matches("(zone").count(), 1);
    // re-running on the output replaces the zone instead of adding one
    let out2 = dir.join("gap_plane2.kicad_pcb");
    let v = run(&[
        "plane",
        "create",
        "--pcb",
        s(&out),
        "--net",
        "GND",
        "--layer",
        "In1.Cu",
        "--out",
        s(&out2),
    ]);
    assert_eq!(v["replaced"][0], "cp_plane_GND_In1");
    assert_eq!(
        std::fs::read_to_string(&out2)
            .unwrap()
            .matches("(zone")
            .count(),
        1
    );

    let r = run(&["plane", "report", "--pcb", s(&out), "--net", "GND"]);
    let n = &r["nets"][0];
    assert_eq!(n["components"], 1, "{n}");
    assert_eq!(n["unconnected_pads"].as_array().unwrap().len(), 0);
    let l = &n["layers"][0];
    assert_eq!(l["layer"], "In1.Cu");
    assert_eq!(l["islands"], 1);
    // gap: from the wall end (25 + 0.125 width/2 + 0.2 clearance) to the edge clearance (30 - 0.5)
    let neck = n["necks"][0]["width_mm"].as_f64().unwrap();
    assert!((neck - 4.175).abs() <= 0.15, "neck {neck}");

    // a full wall cuts the plane in two: two islands, the right pads are not connected
    let pcb = board(&dir, "cut", 29.9);
    let out = dir.join("cut_plane.kicad_pcb");
    run(&[
        "plane",
        "create",
        "--pcb",
        s(&pcb),
        "--net",
        "GND",
        "--layer",
        "In1.Cu",
        "--out",
        s(&out),
    ]);
    let r = run(&["plane", "report", "--pcb", s(&out), "--net", "GND"]);
    let n = &r["nets"][0];
    assert_eq!(n["layers"][0]["islands"], 2, "{n}");
    assert_eq!(n["components"], 2);
    assert_eq!(n["unconnected_pads"].as_array().unwrap().len(), 2);

    // native fill written to a copy: opens reads it like a KiCad fill
    let filled = dir.join("cut_filled.kicad_pcb");
    run(&["plane", "fill", "--pcb", s(&out), "--out", s(&filled)]);
    assert!(std::fs::read_to_string(&filled)
        .unwrap()
        .contains("(filled_polygon"));
    let o = run(&["opens", "--pcb", s(&filled), "--nets", "GND"]);
    assert_eq!(o["opens"], 1, "{o}");
    // the same without writing: --native-fill
    let o = run(&["opens", "--pcb", s(&out), "--nets", "GND", "--native-fill"]);
    assert_eq!(o["opens"], 1, "{o}");
}

#[test]
fn plane_hv_cut_and_grow() {
    let dir = std::env::temp_dir().join("cp_planes_test2");
    let pcb = board(&dir, "hv", 10.0);
    let plain = dir.join("hv_plain.kicad_pcb");
    let a0 = run(&[
        "plane",
        "create",
        "--pcb",
        s(&pcb),
        "--net",
        "GND",
        "--layer",
        "F.Cu",
        "--out",
        s(&plain),
    ])["area_mm2"]
        .as_f64()
        .unwrap();
    let cut = dir.join("hv_cut.kicad_pcb");
    let v = run(&[
        "plane",
        "create",
        "--pcb",
        s(&pcb),
        "--net",
        "GND",
        "--layer",
        "F.Cu",
        "--hv",
        "HV",
        "--creepage",
        "3",
        "--out",
        s(&cut),
    ]);
    // the HV pads sit 4 mm from the right edge: the cut notches the outline (no keep-out)
    let a1 = v["area_mm2"].as_f64().unwrap();
    assert!(a1 < a0 - 30.0, "{a0} {a1}");
    assert_eq!(v["outlines"], 1);

    // a small zone grows into the free area of its layer
    let small = dir.join("small.kicad_pcb");
    run(&[
        "plane",
        "create",
        "--pcb",
        s(&pcb),
        "--net",
        "GND",
        "--layer",
        "In2.Cu",
        "--region",
        "2,2,10,10",
        "--name",
        "gnd_small",
        "--out",
        s(&small),
    ]);
    let grown = dir.join("grown.kicad_pcb");
    let v = run(&[
        "plane",
        "adjust",
        "--pcb",
        s(&small),
        "--zone",
        "gnd_small",
        "--grow",
        "5",
        "--out",
        s(&grown),
    ]);
    let z = &v["zones"][0];
    let (b, a) = (
        z["area_before_mm2"].as_f64().unwrap(),
        z["area_after_mm2"].as_f64().unwrap(),
    );
    assert!(a > 2.5 * b, "{b} -> {a}");
}
