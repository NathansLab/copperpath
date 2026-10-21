//! `stitch`: one via per filled pour island without a via of its net, on the plane fill, clear
//! of other nets and out of no-via rule areas; `--grid` adds a via grid per island.

use std::process::Command;

fn rect(x0: f64, y0: f64, x1: f64, y1: f64) -> String {
    format!("(pts (xy {x0} {y0}) (xy {x1} {y0}) (xy {x1} {y1}) (xy {x0} {y1}))")
}

fn board(dir: &std::path::Path) -> std::path::PathBuf {
    std::fs::create_dir_all(dir).unwrap();
    let pcb = dir.join("stitch.kicad_pcb");
    let islands: String = [
        (2.0, 2.0, 8.0, 8.0),    // A: has a via already
        (10.0, 2.0, 16.0, 8.0),  // B: gets one
        (20.0, 2.0, 22.0, 4.0),  // C: covered by another net on B.Cu
        (26.0, 2.0, 30.0, 6.0),  // D: inside a no-via rule area
        (2.0, 10.0, 38.0, 18.0), // E: large, grid vias
    ]
    .iter()
    .map(|(a, b, c, d)| format!("(filled_polygon (layer \"F.Cu\") {})", rect(*a, *b, *c, *d)))
    .collect();
    let text = format!(
        "(kicad_pcb (version 20260206) (layers (0 \"F.Cu\" signal) (2 \"B.Cu\" signal) (25 \"Edge.Cuts\" user)) \
         (gr_rect (start 0 0) (end 40 20) (stroke (width 0.1)) (layer \"Edge.Cuts\")) \
         (via (at 5 5) (size 0.6) (drill 0.3) (layers \"F.Cu\" \"B.Cu\") (net \"GND\")) \
         (segment (start 19 3) (end 23 3) (width 3) (layer \"B.Cu\") (net \"SIG\")) \
         (zone (net 0) (layers \"F.Cu\" \"B.Cu\") (keepout (tracks allowed) (vias not_allowed) \
           (pads allowed) (copperpour allowed) (footprints allowed)) (polygon {})) \
         (zone (net \"GND\") (layers \"F.Cu\") (name \"TOP_GND\") (polygon {}) {islands}) \
         (zone (net \"GND\") (layers \"B.Cu\") (name \"PLANE\") (polygon {}) \
           (filled_polygon (layer \"B.Cu\") {})))",
        rect(25.0, 1.0, 31.0, 7.0),
        rect(1.0, 1.0, 39.0, 19.0),
        rect(1.0, 1.0, 39.0, 19.0),
        rect(1.0, 1.0, 39.0, 19.0),
    );
    std::fs::write(&pcb, text).unwrap();
    pcb
}

fn stitch(pcb: &std::path::Path, extra: &[&str]) -> serde_json::Value {
    let out = pcb.with_file_name("stitched.kicad_pcb");
    let o = Command::new(env!("CARGO_BIN_EXE_copperpath"))
        .args([
            "stitch", "--json", "--net", "GND", "--zones", "TOP_.*", "--plane", "PLANE", "--pcb",
        ])
        .arg(pcb)
        .arg("--out")
        .arg(&out)
        .args(extra)
        .output()
        .expect("run copperpath");
    serde_json::from_slice(&o.stdout)
        .unwrap_or_else(|_| panic!("{}", String::from_utf8_lossy(&o.stderr)))
}

fn vias(v: &serde_json::Value) -> Vec<(f64, f64)> {
    v["vias"]
        .as_array()
        .unwrap()
        .iter()
        .map(|q| (q[0].as_f64().unwrap(), q[1].as_f64().unwrap()))
        .collect()
}

#[test]
fn one_via_per_lone_island() {
    let pcb = board(&std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("stitch_lone"));
    let v = stitch(&pcb, &[]);
    assert_eq!(v["ok"], true, "{v}");
    assert_eq!(v["islands"], 5, "{v}");
    assert_eq!(v["lone"], 4, "{v}");
    assert_eq!(v["fixed"], 2, "{v}");
    assert_eq!(v["failed"].as_array().unwrap().len(), 2, "{v}");
    // what blocks them: the other net on B.Cu, and the no-via rule area
    let first = |i: usize| v["failed"][i]["blockers"][0].clone();
    let kinds: Vec<String> = (0..2)
        .map(|i| format!("{} {}", first(i)["kind"], first(i)["net"]))
        .collect();
    assert!(kinds.iter().any(|k| k == "\"track\" \"SIG\""), "{v}");
    assert!(kinds.iter().any(|k| k.starts_with("\"keepout\"")), "{v}");
    let vs = vias(&v);
    assert_eq!(vs.len(), 2, "{v}");
    let inside = |q: &(f64, f64), x0: f64, y0: f64, x1: f64, y1: f64| {
        q.0 > x0 && q.0 < x1 && q.1 > y0 && q.1 < y1
    };
    assert!(vs.iter().any(|q| inside(q, 10.0, 2.0, 16.0, 8.0)), "{v}");
    assert!(vs.iter().any(|q| inside(q, 2.0, 10.0, 38.0, 18.0)), "{v}");
    // the vias are written to the copy
    let out = std::fs::read_to_string(pcb.with_file_name("stitched.kicad_pcb")).unwrap();
    assert_eq!(out.matches("(drill").count(), 3);
}

#[test]
fn grid_vias_keep_their_distance() {
    let pcb = board(&std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("stitch_grid"));
    let v = stitch(&pcb, &["--grid", "4"]);
    let vs = vias(&v);
    // island E (36 x 8 mm) on a 4 mm grid: points at y = 12 and 16, x = 4..36
    assert!(vs.len() >= 12, "{v}");
    for (i, a) in vs.iter().enumerate() {
        for b in &vs[i + 1..] {
            assert!(
                ((a.0 - b.0).powi(2) + (a.1 - b.1).powi(2)).sqrt() >= 0.85,
                "{v}"
            );
        }
        assert!(
            !(a.0 > 20.0 && a.0 < 22.0 && a.1 < 4.6),
            "via on the other net: {v}"
        );
        assert!(
            !(a.0 > 25.0 && a.0 < 31.0 && a.1 < 7.0),
            "via in the rule area: {v}"
        );
    }
}
