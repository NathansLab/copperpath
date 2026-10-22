//! `render`: PNG of the demo board (a region, per-layer sheet) with labels, ratsnest of the open
//! connections and DRC markers; JSON summary.

use std::process::Command;

fn run(args: &[&str]) -> serde_json::Value {
    let o = Command::new(env!("CARGO_BIN_EXE_copperpath"))
        .args(args)
        .args(["--threads", "2", "--json"])
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    serde_json::from_slice(&o.stdout).unwrap()
}

fn png_size(path: &std::path::Path) -> (u32, u32) {
    let b = std::fs::read(path).unwrap();
    assert_eq!(&b[1..4], b"PNG");
    let be = |i: usize| u32::from_be_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]]);
    (be(16), be(20))
}

#[test]
fn render_demo_region_and_sheet() {
    let dir = std::env::temp_dir().join("cp_render_test");
    std::fs::create_dir_all(&dir).unwrap();
    let drc = dir.join("drc.json");
    std::fs::write(
        &drc,
        r#"{"coordinate_units":"mm","violations":[{"type":"clearance","severity":"error",
        "description":"x","items":[{"pos":{"x":120.0,"y":118.0}}]}],
        "unconnected_items":[{"type":"unconnected_items","severity":"error",
        "items":[{"pos":{"x":115.0,"y":110.0}},{"pos":{"x":125.0,"y":112.0}}]}]}"#,
    )
    .unwrap();
    let png = dir.join("roi.png");
    let v = run(&[
        "render",
        "--pcb",
        "examples/demo/demo.kicad_pcb",
        "--roi",
        "110,105,135,125",
        "--labels",
        "--opens",
        "--courtyards",
        "--fills",
        "--drc",
        drc.to_str().unwrap(),
        "--px",
        "800",
        "--out",
        png.to_str().unwrap(),
    ]);
    assert_eq!(v["ok"], true);
    assert_eq!(v["width"], 800);
    let (w, h) = png_size(&png);
    assert_eq!((w, h), (800, v["height"].as_u64().unwrap() as u32));
    // 25 x 20 mm at 32 px/mm plus the header strip
    assert!((h as i64 - (640 + 16)).abs() <= 1, "{h}");
    // the unrouted demo has open connections in this region, and pads get labels
    assert!(v["opens_in_roi"].as_u64().unwrap() > 5, "{v}");
    assert!(v["labels"].as_u64().unwrap() > 10, "{v}");
    let r0 = &v["ratsnest"][0];
    assert!(r0["net"].is_string() && r0["pads"][1].is_string());

    let sheet = dir.join("sheet.png");
    let v = run(&[
        "render",
        "--pcb",
        "examples/demo/demo.kicad_pcb",
        "--per-layer",
        "--layers",
        "F,In1,In2,B",
        "--px",
        "1000",
        "--out",
        sheet.to_str().unwrap(),
    ]);
    let (w, _) = png_size(&sheet);
    assert_eq!(w, 1000);
    assert_eq!(v["opens_in_roi"], 0); // ratsnest only with --opens
}

#[test]
fn render_rejects_bad_layer() {
    let o = Command::new(env!("CARGO_BIN_EXE_copperpath"))
        .args([
            "render",
            "--pcb",
            "examples/demo/demo.kicad_pcb",
            "--layers",
            "X9",
            "--out",
            "x.png",
            "--json",
        ])
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(1));
}
