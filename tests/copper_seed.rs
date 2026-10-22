//! A connection may start at any copper of a component, not only at its pads: a pad boxed in
//! on its layer (no room or no permission for a new via) still joins through its own via.

use std::process::Command;

fn ring(x0: f64, y0: f64, x1: f64, y1: f64, layer: &str) -> String {
    let c = [(x0, y0), (x1, y0), (x1, y1), (x0, y1), (x0, y0)];
    c.windows(2)
        .map(|w| {
            format!(
                "(segment (start {} {}) (end {} {}) (width 0.3) (locked yes) (layer \"{layer}\") (net \"X\"))",
                w[0].0, w[0].1, w[1].0, w[1].1
            )
        })
        .collect()
}

fn pad(r: &str, x: f64, y: f64) -> String {
    format!(
        "(footprint \"P\" (layer \"F.Cu\") (at {x} {y}) (property \"Reference\" \"{r}\") \
         (pad \"1\" smd rect (at 0 0) (size 1 1) (layers \"F.Cu\") (net \"N\")))"
    )
}

#[test]
fn boxed_in_pad_joins_through_its_own_via() {
    let dir = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("copper_seed");
    std::fs::create_dir_all(&dir).unwrap();
    let pcb = dir.join("seed.kicad_pcb");
    let text = format!(
        "(kicad_pcb (version 20260206) (layers (0 \"F.Cu\" signal) (2 \"B.Cu\" signal) (25 \"Edge.Cuts\" user)) \
         (gr_rect (start 0 0) (end 30 20) (stroke (width 0.1)) (layer \"Edge.Cuts\")) \
         {}{}{} \
         (segment (start 7 10) (end 9 10) (width 0.25) (layer \"F.Cu\") (net \"N\")) \
         (via (at 9 10) (size 0.6) (drill 0.3) (layers \"F.Cu\" \"B.Cu\") (net \"N\"))          (zone (net 0) (layers \"F.Cu\" \"B.Cu\") (keepout (tracks allowed) (vias not_allowed)          (pads allowed) (copperpour allowed) (footprints allowed))          (polygon (pts (xy 4 7) (xy 12 7) (xy 12 13) (xy 4 13)))))",
        pad("R1", 7.0, 10.0),
        ring(5.0, 8.0, 11.0, 12.0, "F.Cu"),
        pad("R2", 22.0, 10.0),
    );
    std::fs::write(&pcb, text).unwrap();
    let out = dir.join("seed_routed.kicad_pcb");
    let o = Command::new(env!("CARGO_BIN_EXE_copperpath"))
        .args(["route", "--threads", "2", "--json", "--nets", "N", "--pcb"])
        .arg(&pcb)
        .arg("--out")
        .arg(&out)
        .output()
        .expect("run copperpath");
    let v: serde_json::Value = serde_json::from_slice(&o.stdout)
        .unwrap_or_else(|_| panic!("{}", String::from_utf8_lossy(&o.stderr)));
    assert_eq!(v["opens"], 0, "{v}\n{}", String::from_utf8_lossy(&o.stderr));
}
