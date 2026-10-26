//! Routing intents per net (`--prio`): `low_r` widens the track (lower resistance), `few_vias`
//! and `low_l` avoid vias; `netinfo` reports the estimated resistance and inductance.

use std::process::Command;

const DEMO: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/examples/demo/demo.kicad_pcb");

fn cp(args: &[&str]) -> serde_json::Value {
    let o = Command::new(env!("CARGO_BIN_EXE_copperpath"))
        .args(args)
        .args(["--threads", "2", "--json"])
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    serde_json::from_slice(&o.stdout).unwrap()
}

fn net<'a>(v: &'a serde_json::Value, n: &str) -> &'a serde_json::Value {
    v["nets"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["net"] == n)
        .unwrap()
}

#[test]
fn intents_change_width_and_vias() {
    let d = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("net_prio");
    std::fs::create_dir_all(&d).unwrap();
    let pro = DEMO.replace("kicad_pcb", "kicad_pro");
    let dru = DEMO.replace("kicad_pcb", "kicad_dru");
    let mut info = Vec::new();
    for (name, prio) in [("base", ""), ("prio", "+5V=low_r,ADC1=few_vias,CLK=low_l")] {
        let out = d.join(format!("{name}.kicad_pcb"));
        let out = out.to_str().unwrap();
        let mut args = vec!["route", "--pcb", DEMO, "--out", out];
        if !prio.is_empty() {
            args.extend(["--prio", prio]);
        }
        let r = cp(&args);
        assert_eq!(r["opens"], 0, "{r}");
        let drc = cp(&["drc", "--pcb", out, "--pro", &pro, "--dru", &dru]);
        assert_eq!(drc["violations"], 0, "{name}: {drc}");
        info.push(cp(&["netinfo", "--pcb", out, "--pro", &pro]));
    }
    let (b, p) = (&info[0], &info[1]);
    let f = |v: &serde_json::Value, n: &str, k: &str| net(v, n)[k].as_f64().unwrap();
    assert!(f(p, "+5V", "width_min_mm") > f(b, "+5V", "width_min_mm"));
    assert!(f(p, "+5V", "r_mohm") < 0.7 * f(b, "+5V", "r_mohm"));
    assert!(f(p, "ADC1", "vias") <= f(b, "ADC1", "vias"));
    assert!(f(p, "CLK", "l_nh") <= f(b, "CLK", "l_nh") * 1.05);
}

#[test]
fn bad_profile_is_refused() {
    let o = Command::new(env!("CARGO_BIN_EXE_copperpath"))
        .args(["route", "--pcb", DEMO, "--prio", "CLK=fast", "--out"])
        .arg(std::env::temp_dir().join("never.kicad_pcb"))
        .output()
        .unwrap();
    assert!(!o.status.success());
    assert!(String::from_utf8_lossy(&o.stderr).contains("unknown profile"));
}

/// intents per net class, from the project file's `copperpath` block (next to the net classes)
#[test]
fn class_intent_from_the_project_file() {
    let d = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("net_prio_class");
    std::fs::create_dir_all(&d).unwrap();
    let pcb = d.join("demo.kicad_pcb");
    std::fs::copy(DEMO, &pcb).unwrap();
    std::fs::copy(
        DEMO.replace("kicad_pcb", "kicad_dru"),
        d.join("demo.kicad_dru"),
    )
    .unwrap();
    let mut pro: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(DEMO.replace("kicad_pcb", "kicad_pro")).unwrap(),
    )
    .unwrap();
    pro["copperpath"] = serde_json::json!({"net_prio": {"class:Power": "low_r"}});
    std::fs::write(d.join("demo.kicad_pro"), pro.to_string()).unwrap();
    let out = d.join("routed.kicad_pcb");
    let out = out.to_str().unwrap();
    let r = cp(&["route", "--pcb", pcb.to_str().unwrap(), "--out", out]);
    assert_eq!(r["opens"], 0, "{r}");
    let pro_path = d.join("demo.kicad_pro");
    let info = cp(&["netinfo", "--pcb", out, "--pro", pro_path.to_str().unwrap()]);
    // Power class (0.4 mm) doubled; a Default net stays at 0.25 mm
    assert!(
        net(&info, "+5V")["width_max_mm"].as_f64().unwrap() >= 0.79,
        "{info}"
    );
    assert!(
        net(&info, "CLK")["width_max_mm"].as_f64().unwrap() <= 0.26,
        "{info}"
    );
}

/// `quiet` keeps a net away from other nets' copper: much less length within 0.5 mm of it
#[test]
fn quiet_reduces_coupled_length() {
    let d = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("net_prio_quiet");
    std::fs::create_dir_all(&d).unwrap();
    let pro = DEMO.replace("kicad_pcb", "kicad_pro");
    let mut coupled = Vec::new();
    for (name, prio) in [("base", ""), ("quiet", "CLK=quiet")] {
        let out = d.join(format!("{name}.kicad_pcb"));
        let out = out.to_str().unwrap();
        let mut args = vec!["route", "--pcb", DEMO, "--out", out];
        if !prio.is_empty() {
            args.extend(["--prio", prio]);
        }
        assert_eq!(cp(&args)["opens"], 0);
        let info = cp(&["netinfo", "--pcb", out, "--pro", &pro, "--nets", "CLK"]);
        coupled.push(net(&info, "CLK")["coupled_mm"].as_f64().unwrap());
    }
    assert!(coupled[1] < 0.5 * coupled[0], "{coupled:?}");
}

/// `z:50`: the track width comes from the stackup (demo: 4 layers, default 0.2 mm prepreg),
/// `netinfo` reports the microstrip impedance; DRC-clean
#[test]
fn impedance_intent_sets_the_width() {
    let d = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("net_prio_z");
    std::fs::create_dir_all(&d).unwrap();
    let out = d.join("z.kicad_pcb");
    let out = out.to_str().unwrap();
    let pro = DEMO.replace("kicad_pcb", "kicad_pro");
    let dru = DEMO.replace("kicad_pcb", "kicad_dru");
    let r = cp(&["route", "--pcb", DEMO, "--prio", "CLK=z:50", "--out", out]);
    assert_eq!(r["opens"], 0, "{r}");
    let drc = cp(&["drc", "--pcb", out, "--pro", &pro, "--dru", &dru]);
    assert_eq!(drc["violations"], 0, "{drc}");
    let info = cp(&["netinfo", "--pcb", out, "--pro", &pro]);
    let z = net(&info, "CLK")["z0_ohm"].as_f64().unwrap();
    assert!((z - 50.0).abs() < 1.0, "{z}");
    // length skew of a group of nets
    let g = cp(&["netinfo", "--pcb", out, "--pro", &pro, "--match", "PWM_.*"]);
    let g = &g["groups"][0];
    assert_eq!(g["nets"], 2, "{g}");
    let skew = g["max_mm"].as_f64().unwrap() - g["min_mm"].as_f64().unwrap();
    assert!((g["skew_mm"].as_f64().unwrap() - skew).abs() < 0.01, "{g}");
    let o = Command::new(env!("CARGO_BIN_EXE_copperpath"))
        .args(["route", "--pcb", DEMO, "--prio", "CLK=z:500", "--out", out])
        .output()
        .unwrap();
    assert!(String::from_utf8_lossy(&o.stderr).contains("out of range"));
}

/// `outer`: the net's tracks stay on F.Cu/B.Cu (on the demo ADC1 otherwise uses In1.Cu)
#[test]
fn outer_intent_keeps_tracks_off_inner_layers() {
    let d = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("net_prio_outer");
    std::fs::create_dir_all(&d).unwrap();
    let out = d.join("outer.kicad_pcb");
    let out = out.to_str().unwrap();
    let r = cp(&["route", "--pcb", DEMO, "--prio", "ADC1=outer", "--out", out]);
    assert_eq!(r["opens"], 0, "{r}");
    let text = std::fs::read_to_string(out).unwrap();
    let flat: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    for l in ["In1.Cu", "In2.Cu"] {
        assert!(
            !flat.contains(&format!("(layer \"{l}\") (net \"ADC1\")")),
            "ADC1 on {l}"
        );
    }
    assert!(flat.contains("(net \"ADC1\")"));
}

/// `pair`: the negative net of a differential pair follows the positive one at the pair gap
/// (coupled over most of its length; without the intent the two part at an obstacle)
#[test]
fn pair_intent_keeps_the_pair_together() {
    let d = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("net_prio_pair");
    std::fs::create_dir_all(&d).unwrap();
    let fp = |r: &str, x: f64, y: f64, pads: &[(&str, f64, f64, &str)]| {
        let p: String = pads
            .iter()
            .map(|(n, dx, dy, net)| {
                format!(
                    "(pad \"{n}\" smd rect (at {dx} {dy}) (size 0.6 0.6) (layers \"F.Cu\") (net \"{net}\"))"
                )
            })
            .collect();
        format!(
            "(footprint \"P\" (layer \"F.Cu\") (at {x} {y}) (property \"Reference\" \"{r}\") {p})"
        )
    };
    let body = [
        fp(
            "J1",
            5.0,
            10.0,
            &[("1", 0.0, 0.0, "USB_D+"), ("2", 0.0, 0.9, "USB_D-")],
        ),
        fp(
            "U1",
            35.0,
            14.0,
            &[("1", 0.0, 0.0, "USB_D+"), ("2", 0.0, 0.9, "USB_D-")],
        ),
        fp(
            "X1",
            20.0,
            11.3,
            &[("1", 0.0, 0.0, "A"), ("2", 0.0, 4.0, "A")],
        ),
        fp(
            "X2",
            12.0,
            8.0,
            &[("1", 0.0, 0.0, "B"), ("2", 0.0, 6.0, "B")],
        ),
    ]
    .concat();
    let pcb = d.join("pair.kicad_pcb");
    std::fs::write(
        &pcb,
        format!(
            "(kicad_pcb (version 20260206) (layers (0 \"F.Cu\" signal) (2 \"B.Cu\" signal) (25 \"Edge.Cuts\" user)) \
             (gr_rect (start 0 0) (end 40 25) (stroke (width 0.1)) (layer \"Edge.Cuts\")) {body})"
        ),
    )
    .unwrap();
    let pcb = pcb.to_str().unwrap();
    let mut coupled = Vec::new();
    for (name, prio) in [("free", ""), ("paired", "USB_D*=pair")] {
        let out = d.join(format!("out_{name}.kicad_pcb"));
        let out = out.to_str().unwrap();
        let mut args = vec!["route", "--pcb", pcb, "--out", out];
        if !prio.is_empty() {
            args.extend(["--prio", prio]);
        }
        assert_eq!(cp(&args)["opens"], 0);
        assert_eq!(cp(&["drc", "--pcb", out])["violations"], 0, "{name}");
        let info = cp(&["netinfo", "--pcb", out, "--nets", "USB_D*", "--near", "0.4"]);
        coupled.push(net(&info, "USB_D-")["coupled_mm"].as_f64().unwrap());
    }
    assert!(
        coupled[1] > 20.0 && coupled[1] > 4.0 * coupled[0],
        "{coupled:?}"
    );
}
