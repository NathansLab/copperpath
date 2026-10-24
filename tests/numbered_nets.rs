//! KiCad 6-9 use numbered net tables; preserve that format when adding copper.
use copperpath::{board, delta, sexpr};
use std::process::Command;
fn fixture() -> String {
    r#"(kicad_pcb (version 20241229)
      (layers (0 "F.Cu" signal) (31 "B.Cu" signal) (44 "Edge.Cuts" user))
      (footprint "Test" (layer "F.Cu") (at 4 5)
        (property "Reference" "J1")
        (pad "1" smd rect (at 0 0) (size 1 1) (layers "F.Cu" "F.Mask") (net 17 "SIGNAL_A")))
      (footprint "Test" (layer "F.Cu") (at 12 5)
        (property "Reference" "J2")
        (pad "1" smd rect (at 0 0) (size 1 1) (layers "F.Cu" "F.Mask") (net 17 "SIGNAL_A")))
      (net 0 "") (net 17 "SIGNAL_A")
      (gr_rect (start 0 0) (end 16 10) (layer "Edge.Cuts") (stroke (width 0.05) (type default))))"#
        .into()
}
#[test]
fn resolves_numbered_pads_tracks_vias_and_zones() {
    let text = fixture();
    let mut b = board::parse_board(text.clone()).unwrap();
    assert_eq!(b.nets, vec!["", "SIGNAL_A"]);
    assert_eq!(b.fp("J1").unwrap().pads[0].net, b.net_id["SIGNAL_A"]);
    let root = sexpr::parse(&text).unwrap();
    let format = sexpr::NetFormat::from_root(&root).unwrap();
    assert_eq!(format.expression("SIGNAL_A").unwrap(), "17");
    assert!(format.expression("MISSING").is_err());
    let delta = serde_json::json!({"add":[
       {"type":"track","net":"SIGNAL_A","start":[4000000,5000000],"end":[12000000,5000000],"width":250000,"layer":"F.Cu"},
       {"type":"via","net":"SIGNAL_A","at":[8000000,5000000],"diameter":600000,"drill":300000,"layers":["F.Cu","B.Cu"]}
    ]});
    // Track serialization independently exercises the common net formatter.
    let track_delta = serde_json::json!({"v":1,"units":"nm","add":[delta["add"][0].clone()]});
    let applied = delta::apply(&text, &track_delta).unwrap();
    assert!(applied.contains("(net 17)"));
    assert!(!applied.contains("(net \"17\")"));
    b = board::parse_board(applied).unwrap();
    assert_eq!(b.nets[b.tracks[0].net], "SIGNAL_A");
    let extra="(segment (start 4 5) (end 12 5) (width 0.25) (layer \"F.Cu\") (net 17)) (via (at 8 5) (size 0.6) (drill 0.3) (layers \"F.Cu\" \"B.Cu\") (net 17)) (zone (net 17) (layer \"F.Cu\") (polygon (pts (xy 1 1) (xy 3 1) (xy 3 3))))";
    let with_extra = format!("{} {extra})", &text[..text.rfind(')').unwrap()]);
    let b = board::parse_board(with_extra).unwrap();
    for net in [b.tracks[0].net, b.vias[0].net, b.zones[0].net] {
        assert_eq!(b.nets[net], "SIGNAL_A")
    }
}
#[test]
fn refuses_undefined_and_inconsistent_codes() {
    for text in [
        fixture().replacen("(net 17 \"SIGNAL_A\")", "(net 42 \"SIGNAL_A\")", 1),
        fixture().replacen("(net 17 \"SIGNAL_A\")", "(net 17 \"WRONG\")", 1),
    ] {
        assert!(board::parse_board(text).is_err());
    }
}
#[test]
fn routes_and_reloads_numbered_board() {
    let dir = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("numbered_nets");
    std::fs::create_dir_all(&dir).unwrap();
    let input = dir.join("input.kicad_pcb");
    let out = dir.join("routed.kicad_pcb");
    std::fs::write(&input, fixture()).unwrap();
    let o = Command::new(env!("CARGO_BIN_EXE_copperpath"))
        .args(["route", "--pcb"])
        .arg(&input)
        .args(["--out"])
        .arg(&out)
        .args(["--threads", "2", "--json"])
        .output()
        .unwrap();
    assert!(
        o.status.success(),
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    );
    let v: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(v["opens"], 0);
    let text = std::fs::read_to_string(&out).unwrap();
    assert!(text.contains("(net 17)"));
    let b = board::load(out.to_str().unwrap()).unwrap();
    assert!(!b.tracks.is_empty());
    assert!(b.tracks.iter().all(|t| b.nets[t.net] == "SIGNAL_A"));
}
#[test]
fn named_numeric_string_is_not_a_code_without_a_table() {
    let text = fixture()
        .replace("(net 17 \"SIGNAL_A\")", "(net \"17\")")
        .replace("(net 0 \"\")", "");
    let b = board::parse_board(text.clone()).unwrap();
    assert_eq!(b.nets, vec!["", "17"]);
    let format = sexpr::NetFormat::from_root(&sexpr::parse(&text).unwrap()).unwrap();
    assert_eq!(format.expression("17").unwrap(), "\"17\"");
}
