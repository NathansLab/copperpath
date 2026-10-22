//! `route --reroute-blockers`: a signal pad boxed in between two parallel tracks of other nets
//! (no via allowed between them) connects after the blocking unlocked track is ripped and its
//! net rerouted; locked tracks are never ripped.

use std::process::Command;

fn smd(r: &str, x: f64, y: f64, net: &str) -> String {
    format!(
        "(footprint \"P\" (layer \"F.Cu\") (at {x} {y}) (property \"Reference\" \"{r}\") \
         (pad \"1\" smd rect (at 0 0) (size 1 1) (layers \"F.Cu\") (net \"{net}\")))"
    )
}

fn board(dir: &std::path::Path, locked: bool) -> std::path::PathBuf {
    std::fs::create_dir_all(dir).unwrap();
    let pcb = dir.join("box.kicad_pcb");
    let lk = if locked { "(locked yes) " } else { "" };
    let text = format!(
        "(kicad_pcb (version 20260206) (layers (0 \"F.Cu\" signal) (2 \"B.Cu\" signal) (25 \"Edge.Cuts\" user)) \
         (gr_rect (start 0 0) (end 40 20) (stroke (width 0.1)) (layer \"Edge.Cuts\")) \
         {}{}{}{}{}{} \
         (segment (start 1.5 9) (end 38.5 9) (width 0.25) {lk}(layer \"F.Cu\") (net \"A\") (uuid \"aaaa0000-0000-4000-8000-000000000001\")) \
         (segment (start 1.5 11) (end 38.5 11) (width 0.25) {lk}(layer \"F.Cu\") (net \"B\") (uuid \"bbbb0000-0000-4000-8000-000000000001\")) \
         (zone (net 0) (layers \"F.Cu\" \"B.Cu\") (keepout (tracks allowed) (vias not_allowed) \
           (pads allowed) (copperpour allowed) (footprints allowed)) \
           (polygon (pts (xy 0 9.3) (xy 40 9.3) (xy 40 10.7) (xy 0 10.7)))))",
        smd("RA1", 1.0, 9.0, "A"),
        smd("RA2", 39.0, 9.0, "A"),
        smd("RB1", 1.0, 11.0, "B"),
        smd("RB2", 39.0, 11.0, "B"),
        smd("T1", 10.0, 10.0, "T"),
        smd("T2", 10.0, 16.0, "T"),
    );
    std::fs::write(&pcb, text).unwrap();
    pcb
}

fn route(pcb: &std::path::Path) -> serde_json::Value {
    let out = pcb.with_file_name("box_routed.kicad_pcb");
    let o = Command::new(env!("CARGO_BIN_EXE_copperpath"))
        .args([
            "route",
            "--threads",
            "2",
            "--json",
            "--reroute-blockers",
            "--pcb",
        ])
        .arg(pcb)
        .arg("--out")
        .arg(&out)
        .output()
        .expect("run copperpath");
    serde_json::from_slice(&o.stdout)
        .unwrap_or_else(|_| panic!("{}", String::from_utf8_lossy(&o.stderr)))
}

#[test]
fn blocking_track_is_ripped_and_rerouted() {
    let pcb = board(
        &std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("reroute_unlocked"),
        false,
    );
    let v = route(&pcb);
    assert_eq!(v["opens"], 0, "{v}");
    let r = &v["reroute"][0];
    assert_eq!(r["net"], "T", "{v}");
    assert_eq!(r["accepted"], true, "{v}");
    assert_eq!(r["ripped_nets"], serde_json::json!(["B"]), "{v}");
    let text = std::fs::read_to_string(pcb.with_file_name("box_routed.kicad_pcb")).unwrap();
    assert!(!text.contains("bbbb0000"), "the blocking track is gone");
    assert!(text.contains("aaaa0000"), "the other track stays");
}

#[test]
fn locked_tracks_stay() {
    let pcb = board(
        &std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("reroute_locked"),
        true,
    );
    let v = route(&pcb);
    assert_eq!(v["opens"], 1, "{v}");
    assert_eq!(v["reroute"][0]["accepted"], false, "{v}");
    let text = std::fs::read_to_string(pcb.with_file_name("box_routed.kicad_pcb")).unwrap();
    assert!(text.contains("aaaa0000") && text.contains("bbbb0000"));
}
