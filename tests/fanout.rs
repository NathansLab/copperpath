//! `fanout`: every open island of a plane net gets a track + via onto the plane fill; inside the
//! pad's courtyard a DRU neck-down rule (`intersectsCourtyard`) sets the width.

use std::process::Command;

fn smd(r: &str, x: f64, y: f64, s: f64, net: &str) -> String {
    format!(
        "(footprint \"P\" (layer \"F.Cu\") (at {x} {y}) (property \"Reference\" \"{r}\") \
         (pad \"1\" smd rect (at 0 0) (size {s} {s}) (layers \"F.Cu\") (net \"{net}\")))"
    )
}

fn board(dir: &std::path::Path) -> std::path::PathBuf {
    std::fs::create_dir_all(dir).unwrap();
    let pcb = dir.join("fan.kicad_pcb");
    // U1: GND pad boxed in by other-net pads above, below and left; the 0.8 mm gap to the right
    // only passes the 0.15 mm neck-down width (0.4 mm needs 0.9 mm with clearance and margin)
    let u1 = "(footprint \"U\" (layer \"F.Cu\") (at 10 10) (property \"Reference\" \"U1\") \
        (fp_rect (start -2 -2) (end 2.5 2) (layer \"F.CrtYd\")) \
        (pad \"1\" smd rect (at 0 0) (size 0.6 0.6) (layers \"F.Cu\") (net \"GND\")) \
        (pad \"2\" smd rect (at 0 -0.9) (size 1 1) (layers \"F.Cu\") (net \"SIG\")) \
        (pad \"3\" smd rect (at 0 0.9) (size 1 1) (layers \"F.Cu\") (net \"SIG\")) \
        (pad \"4\" smd rect (at -1 0) (size 0.6 2.8) (layers \"F.Cu\") (net \"SIG\")))";
    // R4: walled in by a SIG ring, no room for a via inside: fails and reports the ring
    let c = [
        (19.0, 4.0),
        (21.0, 4.0),
        (21.0, 6.0),
        (19.0, 6.0),
        (19.0, 4.0),
    ];
    let ring: String = c
        .windows(2)
        .map(|w| {
            format!(
                "(segment (start {} {}) (end {} {}) (width 0.3) (layer \"F.Cu\") (net \"SIG\"))",
                w[0].0, w[0].1, w[1].0, w[1].1
            )
        })
        .collect();
    let text = format!(
        "(kicad_pcb (version 20260206) (layers (0 \"F.Cu\" signal) (4 \"In1.Cu\" signal) \
         (6 \"In2.Cu\" signal) (2 \"B.Cu\" signal) (25 \"Edge.Cuts\" user)) \
         (gr_rect (start 0 0) (end 40 20) (stroke (width 0.1)) (layer \"Edge.Cuts\")) \
         {u1}{}{}{}{ring} \
         (segment (start 30 15) (end 32 15) (width 0.4) (layer \"F.Cu\") (net \"GND\")) \
         (via (at 32 15) (size 0.6) (drill 0.3) (layers \"F.Cu\" \"B.Cu\") (net \"GND\")) \
         (zone (net \"GND\") (layers \"In1.Cu\") (name \"PLANE\") \
           (polygon (pts (xy 1 1) (xy 39 1) (xy 39 19) (xy 1 19))) \
           (filled_polygon (layer \"In1.Cu\") (pts (xy 1 1) (xy 39 1) (xy 39 19) (xy 1 19)))))",
        smd("R2", 30.0, 8.0, 1.0, "GND"),
        smd("R3", 30.0, 15.0, 1.0, "GND"),
        smd("R4", 20.0, 5.0, 1.0, "GND"),
    );
    std::fs::write(&pcb, text).unwrap();
    std::fs::write(
        dir.join("fan.kicad_dru"),
        "(version 1)\n\
         (rule \"gnd\" (constraint track_width (min 0.15mm) (opt 0.4mm)) (condition \"A.NetName == 'GND'\"))\n\
         (rule \"neck\" (constraint track_width (min 0.15mm) (opt 0.15mm)) \
         (condition \"A.NetName == 'GND' && A.intersectsCourtyard('U1')\"))\n",
    )
    .unwrap();
    pcb
}

#[test]
fn open_islands_get_track_and_via() {
    let pcb = board(&std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("fanout"));
    let out = pcb.with_file_name("fan_out.kicad_pcb");
    let o = Command::new(env!("CARGO_BIN_EXE_copperpath"))
        .args([
            "fanout", "--json", "--net", "GND", "--plane", "PLANE", "--pcb",
        ])
        .arg(&pcb)
        .arg("--out")
        .arg(&out)
        .output()
        .expect("run copperpath");
    let v: serde_json::Value = serde_json::from_slice(&o.stdout)
        .unwrap_or_else(|_| panic!("{}", String::from_utf8_lossy(&o.stderr)));
    // R3 reaches the plane through its via: not open
    assert_eq!(v["open_islands"], 3, "{v}");
    assert_eq!(v["connected"], 2, "{v}");
    let failed: Vec<&serde_json::Value> = v["islands"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|i| i["pad"].is_null())
        .collect();
    assert_eq!(failed[0]["pads"][0], "R4.1", "{v}");
    assert_eq!(failed[0]["blockers"][0]["net"], "SIG", "{v}");
    assert_eq!(failed[0]["blockers"][0]["kind"], "track", "{v}");
    let text = std::fs::read_to_string(&out).unwrap();
    // neck-down inside U1's courtyard: the rule says 0.15, the board minimum (0.2) floors it
    assert!(
        text.contains("(width 0.2)\n"),
        "neck-down inside U1's courtyard"
    );
    assert!(text.contains("(width 0.4)\n"), "net width outside");
    // the fanned-out islands are connected, only R4 stays open
    let o = Command::new(env!("CARGO_BIN_EXE_copperpath"))
        .args(["opens", "--json", "--nets", "GND", "--pcb"])
        .arg(&out)
        .args(["--dru", pcb.with_extension("kicad_dru").to_str().unwrap()])
        .output()
        .expect("run copperpath");
    let v: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(v["opens"], 1, "{v}");
}
