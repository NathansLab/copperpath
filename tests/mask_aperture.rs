//! Tracks of other nets stay out of solder mask apertures (#3): a mask polygon drawn around a
//! solder jumper and a pad mask expansion larger than the copper clearance. KiCad reports copper
//! of another net inside an aperture as `solder_mask_bridge`.

use copperpath::board::parse_board;
use copperpath::geom::{p, shape_dist, Shape};
use std::process::Command;

fn pad(r: &str, x: f64, y: f64, net: &str) -> String {
    format!(
        "(footprint \"P\" (layer \"F.Cu\") (at {x} {y}) (property \"Reference\" \"{r}\") \
         (pad \"1\" smd rect (at 0 0) (size 0.8 0.8) (layers \"F.Cu\" \"F.Mask\") (net \"{net}\")))"
    )
}

#[test]
fn tracks_avoid_mask_apertures_of_other_nets() {
    let dir = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("mask_aperture");
    std::fs::create_dir_all(&dir).unwrap();
    let pcb = dir.join("mask.kicad_pcb");
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
    // solder jumper: two 0.5 x 1 mm pads (nets A, B) inside a 2 x 2 mm F.Mask polygon
    let jumper = "(footprint \"JP\" (layer \"F.Cu\") (at 15 10) (property \"Reference\" \"JP1\") \
         (fp_poly (pts (xy -1 -1) (xy 1 -1) (xy 1 1) (xy -1 1)) (stroke (width 0)) (fill yes) \
         (layer \"F.Mask\")) \
         (pad \"1\" smd rect (at -0.4 0) (size 0.5 1) (layers \"F.Cu\") (net \"A\")) \
         (pad \"2\" smd rect (at 0.4 0) (size 0.5 1) (layers \"F.Cu\") (net \"B\")))";
    // test pad (net E) whose mask opening reaches 0.6 mm beyond the copper
    let wide = "(footprint \"TP\" (layer \"F.Cu\") (at 15 4) (property \"Reference\" \"TP1\") \
         (pad \"1\" smd rect (at 0 0) (size 0.8 0.8) (layers \"F.Cu\" \"F.Mask\") \
         (solder_mask_margin 0.6) (net \"E\")))";
    let text = format!(
        "(kicad_pcb (version 20260206) (layers (0 \"F.Cu\" signal) (25 \"Edge.Cuts\" user)) \
         (setup (pad_to_mask_clearance 0)) {edge}{jumper}{wide}{}{}{}{})",
        // straight paths of C and D run through the jumper and the test pad
        pad("R1", 5.0, 10.0, "C"),
        pad("R2", 25.0, 10.0, "C"),
        pad("R3", 5.0, 4.0, "D"),
        pad("R4", 25.0, 4.0, "D"),
    );
    std::fs::write(&pcb, text).unwrap();
    let out = dir.join("mask_routed.kicad_pcb");
    let o = Command::new(env!("CARGO_BIN_EXE_copperpath"))
        .args(["route", "--threads", "2", "--json", "--pcb"])
        .arg(&pcb)
        .arg("--out")
        .arg(&out)
        .output()
        .expect("run copperpath");
    let log = String::from_utf8_lossy(&o.stderr);
    let v: serde_json::Value = serde_json::from_slice(&o.stdout).expect("json output");
    assert_eq!(v["failed"].as_array().map(|a| a.len()), Some(0), "{log}");
    let b = parse_board(std::fs::read_to_string(&out).unwrap()).unwrap();
    let apertures = [
        Shape::Poly {
            pts: vec![p(14.0, 9.0), p(16.0, 9.0), p(16.0, 11.0), p(14.0, 11.0)],
            r: 0.0,
        },
        Shape::Poly {
            pts: vec![p(14.6, 3.6), p(15.4, 3.6), p(15.4, 4.4), p(14.6, 4.4)],
            r: 0.6,
        },
    ];
    let mut n = 0;
    for t in &b.tracks {
        let name = &b.nets[t.net];
        assert!(name == "C" || name == "D", "unexpected track net {name}");
        n += 1;
        let s = Shape::Capsule {
            a: t.a,
            b: t.b,
            r: t.w / 2.0,
        };
        for ap in &apertures {
            assert!(
                shape_dist(&s, ap) > 0.0,
                "net {name} track {:?}-{:?} enters a mask aperture",
                t.a,
                t.b
            );
        }
    }
    assert!(n >= 2, "{log}");
}
