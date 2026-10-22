//! Saved zone fills count as connected copper: `opens` reports the real islands of a plane net
//! (pads per island), and the router joins the islands, starting on the fill if needed.

use std::process::Command;

fn pad(r: &str, x: f64, y: f64) -> String {
    format!(
        "(footprint \"P\" (layer \"F.Cu\") (at {x} {y}) (property \"Reference\" \"{r}\") \
         (pad \"1\" smd rect (at 0 0) (size 1 1) (layers \"F.Cu\") (net \"GND\")))"
    )
}

fn board(dir: &std::path::Path) -> std::path::PathBuf {
    std::fs::create_dir_all(dir).unwrap();
    let pcb = dir.join("fill.kicad_pcb");
    let text = format!(
        "(kicad_pcb (version 20260206) (layers (0 \"F.Cu\" signal) (2 \"B.Cu\" signal) (25 \"Edge.Cuts\" user)) \
         (gr_rect (start 0 0) (end 40 20) (stroke (width 0.1)) (layer \"Edge.Cuts\")) \
         {}{}{}{}{} \
         (segment (start 30 5) (end 30 15) (width 0.25) (layer \"F.Cu\") (net \"GND\")) \
         (zone (net \"GND\") (layers \"F.Cu\") (name \"pour\") \
           (polygon (pts (xy 1 1) (xy 20 1) (xy 20 19) (xy 1 19))) \
           (filled_polygon (layer \"F.Cu\") (pts (xy 2 2) (xy 12 2) (xy 12 18) (xy 2 18)))))",
        pad("R1", 5.0, 5.0),
        pad("R2", 5.0, 15.0),
        pad("R3", 30.0, 5.0),
        pad("R4", 30.0, 15.0),
        // inside the zone outline but outside its fill: its own island
        pad("R5", 16.0, 10.0),
    );
    std::fs::write(&pcb, text).unwrap();
    pcb
}

fn run(args: &[&str], pcb: &std::path::Path) -> serde_json::Value {
    let o = Command::new(env!("CARGO_BIN_EXE_copperpath"))
        .args(args)
        .args(["--threads", "2", "--json", "--pcb"])
        .arg(pcb)
        .output()
        .expect("run copperpath");
    serde_json::from_slice(&o.stdout)
        .unwrap_or_else(|_| panic!("{}", String::from_utf8_lossy(&o.stderr)))
}

#[test]
fn opens_reports_fill_islands() {
    let pcb = board(&std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("fill_opens"));
    let v = run(&["opens"], &pcb);
    assert_eq!(v["opens"], 2, "{v}");
    let isl = &v["per_net"][0]["islands"];
    assert_eq!(isl[0], serde_json::json!(["R1.1", "R2.1"]), "{v}");
    assert_eq!(isl[1], serde_json::json!(["R3.1", "R4.1"]), "{v}");
    assert_eq!(isl[2], serde_json::json!(["R5.1"]), "{v}");
}

#[test]
fn router_joins_fill_islands() {
    let pcb = board(&std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("fill_route"));
    let out = pcb.with_file_name("fill_routed.kicad_pcb");
    let v = run(&["route", "--out", out.to_str().unwrap()], &pcb);
    assert_eq!(v["connections"], 2, "{v}");
    assert_eq!(v["opens"], 0, "{v}");
}
