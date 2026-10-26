//! `status`: one-call board overview for agents (stack, classes, zones, opens, DRC, next steps).

use std::process::Command;

#[test]
fn status_of_the_demo_suggests_the_next_commands() {
    let o = Command::new(env!("CARGO_BIN_EXE_copperpath"))
        .args(["status", "--json", "--pcb"])
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/examples/demo/demo.kicad_pcb"
        ))
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let v: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(v["layers"].as_array().unwrap().len(), 4, "{v}");
    assert_eq!(v["opens"], 50, "{v}");
    assert!(
        v["classes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["name"] == "Power"),
        "{v}"
    );
    let next: Vec<&str> = v["next"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|n| n.as_str())
        .collect();
    // free inner layer and a power-like net without a zone: autoroute with a plane
    assert!(
        next.iter()
            .any(|n| n.contains("autoroute") && n.contains("--planes In1.Cu=GND")),
        "{v}"
    );
}
