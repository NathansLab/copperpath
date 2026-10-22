//! `autoroute`: demo board with a GND plane on In1 in one command: zone created, GND pads fanned
//! out to it, signals routed, every connection closed (GND through the native plane fill),
//! stage timing reported, budget honoured.

use std::process::Command;

#[test]
fn autoroute_demo_with_gnd_plane() {
    let dir = std::env::temp_dir().join("cp_autoroute_test");
    std::fs::create_dir_all(&dir).unwrap();
    let out = dir.join("demo_auto.kicad_pcb");
    let o = Command::new(env!("CARGO_BIN_EXE_copperpath"))
        .args([
            "autoroute",
            "--pcb",
            "examples/demo/demo.kicad_pcb",
            "--planes",
            "In1.Cu=GND",
            "--time",
            "120",
            "--threads",
            "2",
            "--json",
            "--out",
        ])
        .arg(&out)
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let v: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(v["opens_before"], 50, "{v}");
    assert_eq!(v["opens"], 0, "{v}");
    assert_eq!(v["within_budget"], true);
    let names: Vec<&str> = v["stages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["stage"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["planes", "fanout", "route", "gate"], "{v}");
    let text = std::fs::read_to_string(&out).unwrap();
    assert!(text.contains("cp_plane_GND_In1"));
    assert!(v["plane_nets"].as_array().unwrap().contains(&"GND".into()));
}
