//! #53: a pin whose own pad already sits closer to a high-voltage pad of the same package than
//! the creepage rule still gets a stub: copper inside the own pad does not count.

use std::process::Command;

#[test]
fn lv_pin_next_to_hv_row_routes() {
    let dir = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("own_pad_creepage");
    std::fs::create_dir_all(&dir).unwrap();
    let pcb = dir.join("iso.kicad_pcb");
    // SOIC-16W-like rows 9.75 mm apart, pads 1.65 mm long: 8.1 mm edge to edge < 9.7 mm creepage:
    // every cell of the LV pad is illegal, only a stub from inside the pad leaves it
    let u1 = "(footprint \"U\" (layer \"F.Cu\") (at 20 15) (property \"Reference\" \"U1\") \
        (pad \"1\" smd rect (at -4.875 0) (size 1.65 0.6) (layers \"F.Cu\") (net \"HV\")) \
        (pad \"9\" smd rect (at 4.875 0) (size 1.65 0.6) (layers \"F.Cu\") (net \"LV1\")))";
    let c1 = "(footprint \"C\" (layer \"F.Cu\") (at 28 15) (property \"Reference\" \"C1\") \
        (pad \"1\" smd rect (at 0 0) (size 0.8 0.9) (layers \"F.Cu\") (net \"LV1\")))";
    std::fs::write(
        &pcb,
        format!(
            "(kicad_pcb (version 20260206) (layers (0 \"F.Cu\" signal) (2 \"B.Cu\" signal) (25 \"Edge.Cuts\" user)) \
             (gr_rect (start 0 0) (end 40 30) (stroke (width 0.1)) (layer \"Edge.Cuts\")) {u1}{c1})"
        ),
    )
    .unwrap();
    std::fs::write(
        dir.join("iso.kicad_dru"),
        "(version 1)\n\
         (rule \"hv-lv\" (layer outer) (constraint creepage (min 9.7mm)) \
         (condition \"A.NetName == 'HV' && B.NetName == 'LV1'\"))\n",
    )
    .unwrap();
    let out = dir.join("iso_routed.kicad_pcb");
    let o = Command::new(env!("CARGO_BIN_EXE_copperpath"))
        .args([
            "route",
            "--threads",
            "2",
            "--json",
            "--nets",
            "LV1",
            "--pcb",
        ])
        .arg(&pcb)
        .arg("--out")
        .arg(&out)
        .output()
        .expect("run copperpath");
    let v: serde_json::Value = serde_json::from_slice(&o.stdout)
        .unwrap_or_else(|_| panic!("{}", String::from_utf8_lossy(&o.stderr)));
    assert_eq!(v["opens"], 0, "{v}");
    assert_eq!(v["aes"]["exact_fail"], 0, "{v}");
    // every new track point lies in the LV pad or beyond it, away from the HV row
    let text = std::fs::read_to_string(&out).unwrap();
    let tail = &text[text.find("(segment").unwrap()..];
    for cap in tail
        .split("(start ")
        .skip(1)
        .chain(tail.split("(end ").skip(1))
    {
        let x: f64 = cap.split_whitespace().next().unwrap().parse().unwrap();
        assert!(
            x >= 24.05 - 1e-6,
            "track inside the creepage gap at x = {x}"
        );
    }
}
