//! `nudge --push`: joint moves. Pad T1 sits in a box of locked copper whose only opening is
//! blocked by resistor R1 (vias are forbidden). R1 frees the opening when it moves about 0.6 mm
//! down, but capacitor C1 sits right below it (courtyards 0.2 mm apart) and a 3-pad part U9
//! right above it. Without pushing there is no legal move; with `--push` nudge moves R1 and
//! pushes C1 out of the way in one candidate. Infeasible candidates report why.

use std::process::Command;

fn pad1(r: &str, x: f64, y: f64, net: &str) -> String {
    format!(
        "(footprint \"P\" (layer \"F.Cu\") (at {x} {y}) (property \"Reference\" \"{r}\" (at 0 -1 0) (layer \"F.SilkS\")) \
         (attr smd) (pad \"1\" smd rect (at 0 0) (size 1 1) (layers \"F.Cu\") (net \"{net}\")))"
    )
}

fn two_pad(r: &str, x: f64, y: f64) -> String {
    format!(
        "(footprint \"R_0603\" (layer \"F.Cu\") (at {x} {y}) \
           (property \"Reference\" \"{r}\" (at 0 -1.4 0) (layer \"F.SilkS\")) (attr smd) \
           (fp_rect (start -1.5 -0.75) (end 1.5 0.75) (stroke (width 0.05)) (layer \"F.CrtYd\")) \
           (pad \"1\" smd rect (at -0.8 0) (size 0.8 0.9) (layers \"F.Cu\")) \
           (pad \"2\" smd rect (at 0.8 0) (size 0.8 0.9) (layers \"F.Cu\")))"
    )
}

fn board(dir: &std::path::Path) -> std::path::PathBuf {
    std::fs::create_dir_all(dir).unwrap();
    let pcb = dir.join("push.kicad_pcb");
    // wall around T1 with an opening at x = 13, y 9.375 .. 10.625
    let wall = [
        (13.0, 9.375),
        (13.0, 7.0),
        (7.0, 7.0),
        (7.0, 14.0),
        (13.0, 14.0),
        (13.0, 10.625),
    ];
    let mut items = String::new();
    for (k, w) in wall.windows(2).enumerate() {
        items += &format!(
            "(segment (start {} {}) (end {} {}) (width 0.25) (locked yes) (layer \"F.Cu\") (net \"W\") (uuid \"00000000-0000-4000-8000-{:012}\"))",
            w[0].0, w[0].1, w[1].0, w[1].1, k + 1
        );
    }
    let u9 = "(footprint \"SOT\" (layer \"F.Cu\") (at 13 8.3) \
           (property \"Reference\" \"U9\" (at 0 -1.4 0) (layer \"F.SilkS\")) (attr smd) \
           (fp_rect (start -1.6 -0.75) (end 1.6 0.75) (stroke (width 0.05)) (layer \"F.CrtYd\")) \
           (pad \"1\" smd rect (at -1.1 0) (size 0.6 0.6) (layers \"F.Cu\")) \
           (pad \"2\" smd rect (at 1.1 0) (size 0.6 0.6) (layers \"F.Cu\")) \
           (pad \"3\" smd rect (at 1.1 -0.5) (size 0.6 0.4) (layers \"F.Cu\")))";
    let text = format!(
        "(kicad_pcb (version 20260206) (layers (0 \"F.Cu\" signal) (2 \"B.Cu\" signal) (25 \"Edge.Cuts\" user)) \
         (gr_rect (start 0 0) (end 30 20) (stroke (width 0.1)) (layer \"Edge.Cuts\")) \
         (zone (net 0) (layers \"F.Cu\" \"B.Cu\") (keepout (tracks allowed) (vias not_allowed) \
           (pads allowed) (copperpour allowed) (footprints allowed)) \
           (polygon (pts (xy 0 0) (xy 30 0) (xy 30 20) (xy 0 20)))) \
         {} {} {} {} {} {items})",
        two_pad("R1", 13.0, 10.0),
        two_pad("C1", 13.0, 11.7),
        u9,
        pad1("T1", 10.0, 10.0, "T"),
        pad1("T2", 17.0, 10.0, "T"),
    );
    std::fs::write(&pcb, text).unwrap();
    pcb
}

fn run(args: &[&str]) -> serde_json::Value {
    let o = Command::new(env!("CARGO_BIN_EXE_copperpath"))
        .args(args)
        .output()
        .expect("run copperpath");
    serde_json::from_slice(&o.stdout)
        .unwrap_or_else(|_| panic!("{}", String::from_utf8_lossy(&o.stderr)))
}

fn nudge(pcb: &std::path::Path, extra: &[&str]) -> serde_json::Value {
    let mut a = vec![
        "nudge",
        "--threads",
        "2",
        "--json",
        "--pcb",
        pcb.to_str().unwrap(),
        "--parts",
        "R1",
        "--range",
        "1",
        "--rots",
        "0",
        "--dry-run",
    ];
    a.extend_from_slice(extra);
    run(&a)
}

#[test]
fn without_push_no_legal_move_and_reasons() {
    let pcb = board(&std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("nudge_nopush"));
    let v = nudge(&pcb, &[]);
    assert_eq!(v["opens_before"], 1, "{v}");
    assert_eq!(v["improved"], false, "{v}");
    let rs = v["infeasible_reasons"].as_array().unwrap();
    assert!(
        rs.iter().any(|r| r["reason"]
            .as_str()
            .unwrap()
            .contains("R1: courtyard overlaps C1")),
        "{v}"
    );
    // the open net reports why the router gave up and where its pins are
    let t = v["nets"]
        .as_array()
        .unwrap()
        .iter()
        .find(|n| n["net"] == "T")
        .unwrap();
    assert!(t["reason"].as_str().unwrap().starts_with("router:"), "{v}");
    assert_eq!(t["components"].as_array().unwrap().len(), 2, "{v}");
}

#[test]
fn push_moves_the_neighbour_too() {
    let pcb = board(&std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("nudge_push"));
    let v = nudge(&pcb, &["--push", "2", "--push-range", "1.5"]);
    assert_eq!(v["improved"], true, "{v}");
    assert_eq!(v["score"]["opens"], 0, "{v}");
    let mv = v["moves"].as_array().unwrap();
    let r1 = mv.iter().find(|m| m["ref"] == "R1").unwrap();
    let c1 = mv
        .iter()
        .find(|m| m["ref"] == "C1")
        .expect("C1 is a push part");
    assert!(r1["dy"].as_f64().unwrap() > 0.3, "{v}");
    assert!(c1["dy"].as_f64().unwrap() > 0.2, "C1 pushed down: {v}");
    // U9 (3 pads) is never pushed
    assert!(mv.iter().all(|m| m["ref"] != "U9"), "{v}");
    assert!(v["pushed"].as_u64().unwrap() > 0, "{v}");
}
