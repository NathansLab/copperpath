//! A panel file with several closed Edge.Cuts outlines: every pin is judged against the
//! outline that contains it (#68); `--outline N` restricts the board to one outline.

use std::process::Command;

fn rect_lines(x0: f64, y0: f64, x1: f64, y1: f64) -> String {
    let c = [(x0, y0), (x1, y0), (x1, y1), (x0, y1), (x0, y0)];
    c.windows(2)
        .map(|w| {
            format!(
                "(gr_line (start {} {}) (end {} {}) (stroke (width 0.1)) (layer \"Edge.Cuts\"))",
                w[0].0, w[0].1, w[1].0, w[1].1
            )
        })
        .collect()
}

fn pad(r: &str, x: f64, y: f64, net: &str) -> String {
    format!(
        "(footprint \"P\" (layer \"F.Cu\") (at {x} {y}) (property \"Reference\" \"{r}\") \
         (pad \"1\" smd rect (at 0 0) (size 1 1) (layers \"F.Cu\") (net \"{net}\")))"
    )
}

fn panel(dir: &std::path::Path, lines: bool) -> std::path::PathBuf {
    std::fs::create_dir_all(dir).unwrap();
    let pcb = dir.join("panel.kicad_pcb");
    // board 1: 0..20 x 0..15 (rect or lines), board 2: 22..42 (2 mm gap), board 3 with a
    // cut-out in the middle that the net must route around
    let edge = if lines {
        rect_lines(0.0, 0.0, 20.0, 15.0) + &rect_lines(22.0, 0.0, 42.0, 15.0)
    } else {
        "(gr_rect (start 0 0) (end 20 15) (stroke (width 0.1)) (layer \"Edge.Cuts\")) \
         (gr_rect (start 22 0) (end 42 15) (stroke (width 0.1)) (layer \"Edge.Cuts\"))"
            .to_string()
    } + &rect_lines(44.0, 0.0, 64.0, 15.0)
        + &rect_lines(52.0, 5.0, 56.0, 10.0);
    let text = format!(
        "(kicad_pcb (version 20260206) (layers (0 \"F.Cu\" signal) (2 \"B.Cu\" signal) (25 \"Edge.Cuts\" user)) \
         {edge}{}{}{}{}{}{})",
        pad("R1", 3.0, 7.5, "A"),
        pad("R2", 17.0, 7.5, "A"),
        pad("R3", 25.0, 7.5, "B"),
        pad("R4", 39.0, 7.5, "B"),
        pad("R5", 47.0, 7.5, "C"),
        pad("R6", 61.0, 7.5, "C"),
    );
    std::fs::write(&pcb, text).unwrap();
    pcb
}

fn route(pcb: &std::path::Path, extra: &[&str]) -> serde_json::Value {
    let out = pcb.with_file_name("panel_routed.kicad_pcb");
    let o = Command::new(env!("CARGO_BIN_EXE_copperpath"))
        .args(["route", "--threads", "2", "--json", "--pcb"])
        .arg(pcb)
        .arg("--out")
        .arg(&out)
        .args(extra)
        .output()
        .expect("run copperpath");
    serde_json::from_slice(&o.stdout)
        .unwrap_or_else(|_| panic!("{}", String::from_utf8_lossy(&o.stderr)))
}

#[test]
fn each_pin_uses_the_outline_it_lies_in() {
    for lines in [false, true] {
        let dir = std::path::Path::new(env!("CARGO_TARGET_TMPDIR"))
            .join(format!("panel_outlines_{lines}"));
        let v = route(&panel(&dir, lines), &[]);
        assert_eq!(v["ok"], true, "{v}");
        assert_eq!(v["opens"], 0, "lines={lines}: {v}");
    }
}

#[test]
fn outline_option_selects_one_board() {
    let dir = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("panel_outline_n");
    let pcb = panel(&dir, true);
    let v = route(&pcb, &["--outline", "2"]);
    assert_eq!(v["ok"], true, "{v}");
    assert_eq!(v["nets"], 1, "only net B lies inside outline 2: {v}");
    let v = route(&pcb, &["--outline", "4"]);
    assert_eq!(v["ok"], false, "{v}");
}
