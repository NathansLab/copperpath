//! A multi-pin net whose largest pin is walled in still routes its other pins (#39).
//! Without stubs the tree starts at the pad with the most cells (the enclosed one).

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

fn pad(r: &str, x: f64, y: f64, s: f64) -> String {
    format!(
        "(footprint \"P\" (layer \"F.Cu\") (at {x} {y}) (property \"Reference\" \"{r}\") \
         (pad \"1\" smd rect (at 0 0) (size {s} {s}) (layers \"F.Cu\") (net \"N\")))"
    )
}

#[test]
fn isolated_start_pin_keeps_the_rest_of_the_net() {
    let dir = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("isolated_pin");
    std::fs::create_dir_all(&dir).unwrap();
    let pcb = dir.join("iso.kicad_pcb");
    let edge: String = [
        (0, 0, 30, 0),
        (30, 0, 30, 20),
        (30, 20, 0, 20),
        (0, 20, 0, 0),
    ]
    .iter()
    .map(|(a, b, c, d)| {
        format!(
            "(gr_line (start {a} {b}) (end {c} {d}) (stroke (width 0.1)) (layer \"Edge.Cuts\"))"
        )
    })
    .collect();
    let text = format!(
        "(kicad_pcb (version 20260206) (layers (0 \"F.Cu\" signal) (2 \"B.Cu\" signal) (25 \"Edge.Cuts\" user)) \
         {edge}{}{}{}{}{}{}{})",
        // the largest pad (the tree start) is enclosed on both layers
        pad("U1", 8.0, 10.0, 3.0),
        ring(5.0, 7.0, 11.0, 13.0, "F.Cu"),
        ring(5.0, 7.0, 11.0, 13.0, "B.Cu"),
        pad("R1", 20.0, 5.0, 1.0),
        pad("R2", 20.0, 15.0, 1.0),
        pad("R3", 26.0, 10.0, 1.0),
        "",
    );
    std::fs::write(&pcb, text).unwrap();
    let out = dir.join("iso_routed.kicad_pcb");
    let o = Command::new(env!("CARGO_BIN_EXE_copperpath"))
        .args(["route", "--threads", "2", "--no-stubs", "--json", "--pcb"])
        .arg(&pcb)
        .arg("--out")
        .arg(&out)
        .output()
        .expect("run copperpath");
    let v: serde_json::Value = serde_json::from_slice(&o.stdout)
        .unwrap_or_else(|_| panic!("{}", String::from_utf8_lossy(&o.stderr)));
    // only the walled-in pad stays unconnected: one component left, not three
    assert_eq!(
        v["failed"][0][1],
        1,
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
}
