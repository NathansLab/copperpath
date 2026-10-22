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
