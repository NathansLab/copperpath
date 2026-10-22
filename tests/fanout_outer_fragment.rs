//! `fanout --plane REGEX` where the pattern also matches an outer pour of the same net: a pad
//! inside an isolated F.Cu pour fragment (no via) is still an open island; only the fragment
//! that reaches the inner plane through a via counts as connected.

use std::process::Command;

fn smd(r: &str, x: f64, y: f64) -> String {
    format!(
        "(footprint \"P\" (layer \"F.Cu\") (at {x} {y}) (property \"Reference\" \"{r}\") \
         (pad \"1\" smd rect (at 0 0) (size 1 1) (layers \"F.Cu\") (net \"GND\")))"
    )
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

#[test]
fn isolated_outer_fragment_is_open() {
    let dir = std::env::temp_dir().join("cp_fanout_fragment");
    std::fs::create_dir_all(&dir).unwrap();
    let pcb = dir.join("frag.kicad_pcb");
    let text = format!(
        "(kicad_pcb (version 20260206) (layers (0 \"F.Cu\" signal) (4 \"In1.Cu\" signal) \
         (6 \"In2.Cu\" signal) (2 \"B.Cu\" signal) (25 \"Edge.Cuts\" user)) \
         (gr_rect (start 0 0) (end 30 12) (stroke (width 0.1)) (layer \"Edge.Cuts\")) \
         {}{} \
         (via (at 8 8) (size 0.6) (drill 0.3) (layers \"F.Cu\" \"B.Cu\") (net \"GND\")) \
         (zone (net \"GND\") (layers \"In1.Cu\") (name \"P_GND_In1\") \
           (polygon (pts (xy 0.5 0.5) (xy 29.5 0.5) (xy 29.5 11.5) (xy 0.5 11.5))) \
           (filled_polygon (layer \"In1.Cu\") (pts (xy 1 1) (xy 29 1) (xy 29 11) (xy 1 11)))) \
         (zone (net \"GND\") (layers \"F.Cu\") (name \"P_GND_F\") \
           (polygon (pts (xy 0.5 0.5) (xy 29.5 0.5) (xy 29.5 11.5) (xy 0.5 11.5))) \
           (filled_polygon (layer \"F.Cu\") (pts (xy 2 2) (xy 10 2) (xy 10 10) (xy 2 10))) \
           (filled_polygon (layer \"F.Cu\") (pts (xy 20 2) (xy 28 2) (xy 28 10) (xy 20 10)))))",
        smd("R1", 5.0, 5.0),
        smd("R2", 24.0, 6.0),
    );
    std::fs::write(&pcb, text).unwrap();
    let o = run(&["opens", "--pcb", pcb.to_str().unwrap(), "--nets", "GND"]);
    assert_eq!(o["opens"], 1, "{o}");
    let out = dir.join("frag_out.kicad_pcb");
    let v = run(&[
        "fanout",
        "--pcb",
        pcb.to_str().unwrap(),
        "--net",
        "GND",
        "--plane",
        "P_GND_.*",
        "--out",
        out.to_str().unwrap(),
    ]);
    assert_eq!(v["open_islands"], 1, "{v}");
    assert_eq!(v["islands"][0]["pads"][0], "R2.1", "{v}");
    assert_eq!(v["connected"], 1, "{v}");
}
