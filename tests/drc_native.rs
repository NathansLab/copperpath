//! Native copper DRC (`drc`) with KiCad's rule names, and thermal reliefs in the native zone
//! filler (#86): a pad whose spokes are all blocked stays unconnected, like in KiCad.

use std::process::Command;

fn fp(r: &str, x: f64, y: f64, net: &str) -> String {
    format!(
        "(footprint \"P\" (layer \"F.Cu\") (at {x} {y}) (property \"Reference\" \"{r}\") \
         (pad \"1\" smd rect (at 0 0) (size 1 1) (layers \"F.Cu\") (net \"{net}\")))"
    )
}

fn seg(a: (f64, f64), b: (f64, f64), net: &str) -> String {
    format!(
        "(segment (start {} {}) (end {} {}) (width 0.25) (layer \"F.Cu\") (net \"{net}\"))",
        a.0, a.1, b.0, b.1
    )
}

fn write(name: &str, body: &str) -> std::path::PathBuf {
    let d = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("drc_native");
    std::fs::create_dir_all(&d).unwrap();
    let pcb = d.join(format!("{name}.kicad_pcb"));
    std::fs::write(
        &pcb,
        format!(
            "(kicad_pcb (version 20260206) (layers (0 \"F.Cu\" signal) (2 \"B.Cu\" signal) (25 \"Edge.Cuts\" user)) \
             (gr_rect (start 0 0) (end 40 20) (stroke (width 0.1)) (layer \"Edge.Cuts\")) {body})"
        ),
    )
    .unwrap();
    pcb
}

fn run(args: &[&str], pcb: &std::path::Path) -> serde_json::Value {
    let o = Command::new(env!("CARGO_BIN_EXE_copperpath"))
        .args(args)
        .args(["--threads", "2", "--json", "--pcb"])
        .arg(pcb)
        .output()
        .expect("run copperpath");
    serde_json::from_slice(&o.stdout)
        .unwrap_or_else(|_| panic!("{}", String::from_utf8_lossy(&o.stderr)))
}

#[test]
fn copper_violations_by_kicad_type() {
    let body = [
        fp("R1", 5.0, 5.0, "A"),
        fp("R2", 5.0, 10.0, "B"),
        // A runs over the pad of B
        seg((5.0, 5.0), (5.0, 10.0), "A"),
        // centrelines cross
        seg((10.0, 2.0), (10.0, 8.0), "A"),
        seg((8.0, 5.0), (12.0, 5.0), "B"),
        // 0.05 mm apart, default clearance 0.2 mm
        seg((15.0, 2.0), (15.0, 8.0), "A"),
        seg((15.3, 2.0), (15.3, 8.0), "B"),
    ]
    .concat();
    let v = run(&["drc"], &write("violations", &body));
    // as kicad-cli 10: one collision per net pair (the short wins over the crossing)
    assert_eq!(v["by_type"]["shorting_items"], 1, "{v}");
    assert_eq!(v["by_type"]["clearance"], 1, "{v}");
    assert_eq!(v["violations"], 2, "{v}");
}

fn thermal_board(name: &str, connect: &str) -> std::path::PathBuf {
    thermal_board_open(name, connect, false)
}

/// `open_east`: no stub east of G1, so one spoke reaches the fill (KiCad: starved_thermal)
fn thermal_board_open(name: &str, connect: &str, open_east: bool) -> std::path::PathBuf {
    // G1's four spokes (axis directions) end on foreign stubs; the corners stay free, so only a
    // solid connection reaches it
    let stub = |x: f64, y: f64, vertical: bool| {
        let (dx, dy) = if vertical { (0.0, 0.4) } else { (0.4, 0.0) };
        seg((x - dx, y - dy), (x + dx, y + dy), "X")
    };
    let body = [
        fp("G1", 10.0, 10.0, "GND"),
        fp("G2", 30.0, 10.0, "GND"),
        if open_east {
            String::new()
        } else {
            stub(11.2, 10.0, true)
        },
        stub(8.8, 10.0, true),
        stub(10.0, 11.2, false),
        stub(10.0, 8.8, false),
        format!(
            "(zone (net \"GND\") (layers \"F.Cu\") (name \"pour\") (connect_pads {connect}(clearance 0.2)) \
             (min_thickness 0.2) (fill (thermal_gap 0.5) (thermal_bridge_width 0.3)) \
             (polygon (pts (xy 1 1) (xy 39 1) (xy 39 19) (xy 1 19))))"
        ),
    ]
    .concat();
    write(name, &body)
}

#[test]
fn blocked_thermal_spokes_leave_the_pad_unconnected() {
    let v = run(&["drc", "--refill-zones"], &thermal_board("thermal", ""));
    assert_eq!(v["unconnected"], 1, "{v}");
    let v = run(&["drc", "--refill-zones"], &thermal_board("solid", "yes "));
    assert_eq!(v["unconnected"], 0, "{v}");
}

#[test]
fn one_spoke_is_a_starved_thermal() {
    // as kicad-cli 10: connected through one spoke, below the minimum of 2
    let v = run(
        &["drc", "--refill-zones"],
        &thermal_board_open("starved", "", true),
    );
    assert_eq!(v["unconnected"], 0, "{v}");
    assert_eq!(v["by_type"]["starved_thermal"], 1, "{v}");
}

#[test]
fn routed_demo_is_clean() {
    let demo = concat!(env!("CARGO_MANIFEST_DIR"), "/examples/demo/demo.kicad_pcb");
    let d = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("drc_native");
    std::fs::create_dir_all(&d).unwrap();
    let out = d.join("demo_routed.kicad_pcb");
    let st = Command::new(env!("CARGO_BIN_EXE_copperpath"))
        .args(["route", "--threads", "2", "--pcb", demo, "--out"])
        .arg(&out)
        .output()
        .unwrap();
    assert!(
        st.status.success(),
        "{}",
        String::from_utf8_lossy(&st.stderr)
    );
    for f in ["kicad_pro", "kicad_dru"] {
        let src = demo.replace("kicad_pcb", f);
        std::fs::copy(&src, out.with_extension(f)).unwrap();
    }
    let v = run(&["drc"], &out);
    assert_eq!(v["violations"], 0, "{v}");
    assert_eq!(v["unconnected"], 0, "{v}");
}

#[test]
fn class_clearance_below_the_old_default_is_legal() {
    // KiCad's default minimum clearance is 0: a 0.1 mm net class allows 0.15 mm gaps
    let body = [
        seg((20.0, 5.0), (25.0, 5.0), "A"),
        seg((20.0, 5.4), (25.0, 5.4), "B"),
    ]
    .concat();
    let pcb = write("fine_class", &body);
    std::fs::write(
        pcb.with_extension("kicad_pro"),
        r#"{"net_settings": {"classes": [{"name": "Default", "clearance": 0.1, "track_width": 0.2,
            "via_diameter": 0.6, "via_drill": 0.3}]}}"#,
    )
    .unwrap();
    let v = run(&["drc"], &pcb);
    assert_eq!(v["violations"], 0, "{v}");
}

#[test]
fn thermals_adds_a_stub_to_a_starved_pad() {
    // the one-spoke pad gets a stub track into the fill: no starved thermal, nothing new
    let pcb = thermal_board_open("starved_fix", "", true);
    let out = pcb.with_file_name("starved_fix_out.kicad_pcb");
    let o = Command::new(env!("CARGO_BIN_EXE_copperpath"))
        .args(["thermals", "--json", "--pcb"])
        .arg(&pcb)
        .arg("--out")
        .arg(&out)
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let v: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(v["starved"], 1, "{v}");
    assert_eq!(v["stubs"], 1, "{v}");
    let v = run(&["drc", "--refill-zones"], &out);
    assert_eq!(v["violations"], 0, "{v}");
    assert_eq!(v["unconnected"], 0, "{v}");
}
