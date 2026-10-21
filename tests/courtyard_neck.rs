//! #2: a DRU `track_width` rule scoped by `intersectsCourtyard` gives the router its neck-down
//! width inside that courtyard (power net with a 0.5 mm class width on a 0.5 mm pitch part).

use std::process::Command;

#[test]
fn courtyard_rule_sets_the_neck_width() {
    let dir = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("courtyard_neck");
    std::fs::create_dir_all(&dir).unwrap();
    let pcb = dir.join("neck.kicad_pcb");
    let pads: String = (0..5)
        .map(|k| {
            let net = if k == 2 {
                "PWR".to_string()
            } else {
                format!("S{k}")
            };
            format!(
                "(pad \"{}\" smd rect (at {} 0) (size 0.35 1) (layers \"F.Cu\") (net \"{net}\"))",
                k + 1,
                -1.0 + 0.5 * k as f64
            )
        })
        .collect();
    let text = format!(
        "(kicad_pcb (version 20260206) (layers (0 \"F.Cu\" signal) (2 \"B.Cu\" signal) (25 \"Edge.Cuts\" user)) \
         (gr_rect (start 0 0) (end 40 30) (stroke (width 0.1)) (layer \"Edge.Cuts\")) \
         (footprint \"U\" (layer \"F.Cu\") (at 20 15) (property \"Reference\" \"U1\") \
           (fp_rect (start -1.5 -1.2) (end 1.5 1.2) (layer \"F.CrtYd\")) {pads}) \
         (footprint \"C\" (layer \"F.Cu\") (at 20 22) (property \"Reference\" \"C1\") \
           (pad \"1\" smd rect (at 0 0) (size 1 1) (layers \"F.Cu\") (net \"PWR\"))))"
    );
    std::fs::write(&pcb, text).unwrap();
    std::fs::write(
        dir.join("neck.kicad_dru"),
        "(version 1)\n\
         (rule \"power\" (constraint track_width (min 0.5mm) (opt 0.5mm)) (condition \"A.NetName == 'PWR'\"))\n\
         (rule \"fine pitch neck-down inside courtyard\" (constraint track_width (min 0.2mm) (opt 0.3mm)) \
         (condition \"A.Type == 'Track' && A.intersectsCourtyard('U1')\"))\n",
    )
    .unwrap();
    let out = dir.join("neck_routed.kicad_pcb");
    let o = Command::new(env!("CARGO_BIN_EXE_copperpath"))
        .args([
            "route",
            "--threads",
            "2",
            "--json",
            "--nets",
            "PWR",
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
    assert_eq!(v["aes"]["pins_no_stub"], serde_json::json!([]), "{v}");
    let text = std::fs::read_to_string(&out).unwrap();
    assert!(
        text.contains("(width 0.2)\n"),
        "neck-down inside the courtyard"
    );
    assert!(text.contains("(width 0.5)\n"), "class width outside");
}
