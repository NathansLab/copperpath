//! `nudge`: pad T1 sits in a box of locked copper whose only opening is blocked by resistor
//! R1 (vias are forbidden on the whole board). Routing alone cannot connect T1; moving R1 by
//! about 0.6 mm (or turning it and moving it out of the opening) can, and nudge must find that.

use std::process::Command;

fn pad1(r: &str, x: f64, y: f64, net: &str) -> String {
    format!(
        "(footprint \"P\" (layer \"F.Cu\") (at {x} {y}) (property \"Reference\" \"{r}\" (at 0 -1 0) (layer \"F.SilkS\")) \
         (attr smd) (pad \"1\" smd rect (at 0 0) (size 1 1) (layers \"F.Cu\") (net \"{net}\")))"
    )
}

fn seg(a: (f64, f64), b: (f64, f64), net: &str, locked: bool, id: u32) -> String {
    format!(
        "(segment (start {} {}) (end {} {}) (width 0.25) {}(layer \"F.Cu\") (net \"{net}\") (uuid \"00000000-0000-4000-8000-{id:012}\"))",
        a.0,
        a.1,
        b.0,
        b.1,
        if locked { "(locked yes) " } else { "" }
    )
}

fn board(dir: &std::path::Path) -> std::path::PathBuf {
    std::fs::create_dir_all(dir).unwrap();
    let pcb = dir.join("nudge.kicad_pcb");
    // wall of net W around T1 with an opening at x = 13, y 9.375 .. 10.625
    let wall = [
        (13.0, 9.375),
        (13.0, 7.0),
        (7.0, 7.0),
        (7.0, 13.0),
        (13.0, 13.0),
        (13.0, 10.625),
    ];
    let mut items = String::new();
    for (k, w) in wall.windows(2).enumerate() {
        items += &seg(w[0], w[1], "W", true, k as u32 + 1);
    }
    // the existing (unlocked) routes of R1's nets
    items += &seg((13.8, 10.0), (13.8, 12.5), "A", false, 101);
    items += &seg((13.8, 12.5), (16.0, 12.5), "A", false, 102);
    items += &seg((12.2, 10.0), (12.2, 12.0), "B", false, 103);
    items += &seg((12.2, 12.0), (11.0, 12.0), "B", false, 104);
    let text = format!(
        "(kicad_pcb (version 20260206) (layers (0 \"F.Cu\" signal) (2 \"B.Cu\" signal) (25 \"Edge.Cuts\" user)) \
         (gr_rect (start 0 0) (end 30 20) (stroke (width 0.1)) (layer \"Edge.Cuts\")) \
         (zone (net 0) (layers \"F.Cu\" \"B.Cu\") (keepout (tracks allowed) (vias not_allowed) \
           (pads allowed) (copperpour allowed) (footprints allowed)) \
           (polygon (pts (xy 0 0) (xy 30 0) (xy 30 20) (xy 0 20)))) \
         (footprint \"R_0603\" (layer \"F.Cu\") (at 13 10) \
           (property \"Reference\" \"R1\" (at 0 -1.4 0) (layer \"F.SilkS\")) (attr smd) \
           (fp_rect (start -1.5 -0.75) (end 1.5 0.75) (stroke (width 0.05)) (layer \"F.CrtYd\")) \
           (pad \"1\" smd rect (at -0.8 0) (size 0.8 0.9) (layers \"F.Cu\") (net \"B\")) \
           (pad \"2\" smd rect (at 0.8 0) (size 0.8 0.9) (layers \"F.Cu\") (net \"A\"))) \
         {}{}{}{} {items})",
        pad1("T1", 10.0, 10.0, "T"),
        pad1("T2", 16.0, 7.0, "T"),
        pad1("PA", 16.0, 12.5, "A"),
        pad1("PB", 11.0, 12.0, "B"),
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

#[test]
fn route_alone_cannot_connect_the_boxed_pad() {
    let pcb = board(&std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("nudge_route"));
    let out = pcb.with_file_name("routed.kicad_pcb");
    let v = run(&[
        "route",
        "--threads",
        "2",
        "--json",
        "--pcb",
        pcb.to_str().unwrap(),
        "--out",
        out.to_str().unwrap(),
        "--nets",
        "T",
    ]);
    assert_eq!(v["opens"], 1, "{v}");
}

#[test]
fn nudge_moves_the_resistor_and_routes() {
    let pcb = board(&std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("nudge_move"));
    let out = pcb.with_file_name("nudged.kicad_pcb");
    let v = run(&[
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
        "--step",
        "0.05",
        "--rots",
        "0,90",
        "--out",
        out.to_str().unwrap(),
    ]);
    assert_eq!(v["improved"], true, "{v}");
    assert_eq!(v["opens_before"], 1, "{v}");
    assert_eq!(v["opens"], 0, "{v}");
    let m = &v["moves"][0];
    assert_eq!(m["ref"], "R1");
    let (dx, dy) = (m["dx"].as_f64().unwrap(), m["dy"].as_f64().unwrap());
    let moved = dx.hypot(dy);
    // the opening needs about 0.6 mm (or a turn plus a move out of the opening)
    assert!(moved > 0.5 || m["drot"] != 0.0, "{v}");
    let text = std::fs::read_to_string(&out).unwrap();
    assert!(text.contains("(locked yes)"), "the wall stays");
    // ripped unlocked routes of A and B are gone, new copper was appended
    assert!(!text.contains("8000-000000000101"), "old route of A ripped");
    // the written board has R1 at its new place
    let pads = Command::new(env!("CARGO_BIN_EXE_copperpath"))
        .args(["pads", "--pcb", out.to_str().unwrap()])
        .output()
        .unwrap();
    let pads = String::from_utf8_lossy(&pads.stdout);
    let r1: Vec<&str> = pads.lines().filter(|l| l.starts_with("R1\t")).collect();
    assert_eq!(r1.len(), 2, "{pads}");
}

#[test]
fn dry_run_and_auto_pick_the_resistor() {
    let pcb = board(&std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("nudge_auto"));
    let v = run(&[
        "nudge",
        "--threads",
        "2",
        "--json",
        "--pcb",
        pcb.to_str().unwrap(),
        "--auto",
        "--auto-dist",
        "4",
        "--range",
        "1",
        "--dry-run",
    ]);
    assert_eq!(v["improved"], true, "{v}");
    assert_eq!(v["written"], false, "{v}");
    assert_eq!(v["score"]["opens"], 0, "{v}");
    assert!(v["moves"]
        .as_array()
        .unwrap()
        .iter()
        .any(|m| m["ref"] == "R1"));
}
