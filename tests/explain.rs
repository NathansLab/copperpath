//! `explain`: why a connection is open. A wall of SIG copper on both layers between the two pads
//! of net A: fully closed and locked -> "enclosed"; locked with a 0.5 mm gap -> "too-narrow" with
//! the widest track and the blocking wall; closed but unlocked -> "routed-copper" with SIG as
//! rip-up candidate. Without a wall the corridor exists ("congestion").

use std::process::Command;

fn smd(r: &str, x: f64, y: f64) -> String {
    format!(
        "(footprint \"P\" (layer \"F.Cu\") (at {x} {y}) (property \"Reference\" \"{r}\") \
         (pad \"1\" smd rect (at 0 0) (size 1 1) (layers \"F.Cu\") (net \"A\")))"
    )
}

fn board(dir: &std::path::Path, name: &str, wall: &str) -> std::path::PathBuf {
    std::fs::create_dir_all(dir).unwrap();
    let pcb = dir.join(format!("{name}.kicad_pcb"));
    let seg = |y0: f64, y1: f64, l: &str, lock: bool| {
        format!(
            "(segment (start 20 {y0}) (end 20 {y1}) (width 0.2) (layer \"{l}\") (net \"SIG\"){})",
            if lock { " (locked yes)" } else { "" }
        )
    };
    let mut w = String::new();
    for l in ["F.Cu", "B.Cu"] {
        match wall {
            "closed" => w += &seg(0.0, 30.0, l, true),
            "unlocked" => w += &seg(0.0, 30.0, l, false),
            "gap" => {
                w += &seg(0.0, 14.65, l, true);
                w += &seg(15.35, 30.0, l, true);
            }
            _ => {}
        }
    }
    let text = format!(
        "(kicad_pcb (version 20260206) (layers (0 \"F.Cu\" signal) (2 \"B.Cu\" signal) \
         (25 \"Edge.Cuts\" user)) \
         (gr_rect (start 0 0) (end 40 30) (stroke (width 0.1)) (layer \"Edge.Cuts\")) {}{}{w})",
        smd("A1", 10.0, 15.0),
        smd("A2", 30.0, 15.0),
    );
    std::fs::write(&pcb, text).unwrap();
    pcb
}

fn explain(pcb: &std::path::Path, extra: &[&str]) -> serde_json::Value {
    let o = Command::new(env!("CARGO_BIN_EXE_copperpath"))
        .args(["explain", "--net", "A", "--threads", "2", "--json", "--pcb"])
        .arg(pcb)
        .args(extra)
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let v: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(v["open"], 1, "{v}");
    v["explain"][0].clone()
}

#[test]
fn explain_kinds() {
    let dir = std::env::temp_dir().join("cp_explain_test");
    let e = explain(&board(&dir, "free", ""), &[]);
    assert_eq!(e["kind"], "congestion", "{e}");

    let e = explain(&board(&dir, "closed", "closed"), &[]);
    assert_eq!(e["kind"], "enclosed", "{e}");
    assert_eq!(e["blockers"][0]["net"], "SIG", "{e}");

    let img = dir.join("img");
    let e = explain(
        &board(&dir, "gap", "gap"),
        &["--explain-img", img.to_str().unwrap()],
    );
    assert_eq!(e["kind"], "too-narrow", "{e}");
    // gap 0.5 mm between the walls, 0.2 mm clearance on each side: about 0.1 mm of track
    let c = e["corridor_mm"].as_f64().unwrap();
    assert!(c > 0.03 && c < 0.2, "{e}");
    let at = e["at"].as_array().unwrap();
    assert!((at[0].as_f64().unwrap() - 20.0).abs() < 0.6, "{e}");
    assert!(e["reason"].as_str().unwrap().contains("SIG"), "{e}");
    assert!(std::path::Path::new(e["img"].as_str().unwrap()).is_file());
    assert!(e["next"].as_str().unwrap().contains("nudge"), "{e}");

    // route --explain: the open connection comes with its reason
    let pcb = board(&dir, "gap2", "gap");
    let out = dir.join("gap2_routed.kicad_pcb");
    let o = Command::new(env!("CARGO_BIN_EXE_copperpath"))
        .args(["route", "--threads", "2", "--json", "--explain", "--pcb"])
        .arg(&pcb)
        .arg("--out")
        .arg(&out)
        .output()
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(v["opens"], 1, "{v}");
    assert_eq!(v["explain"][0]["kind"], "too-narrow", "{v}");

    let e = explain(&board(&dir, "unlocked", "unlocked"), &[]);
    assert_eq!(e["kind"], "routed-copper", "{e}");
    assert_eq!(e["rip_nets"][0], "SIG", "{e}");
    // the command that fixes it
    let next = e["next"].as_str().unwrap();
    assert!(next.contains("--nets 'A' --reroute-blockers"), "{e}");
}
