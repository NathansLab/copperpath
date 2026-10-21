//! Selection-aware routing (#22): "route the rest" after manual work keeps every existing
//! track and via (same UUID, same geometry), removes nothing and hands out no UUID twice;
//! routing selected nets only adds copper of those nets.

use copperpath::{board, delta};
use std::process::Command;

const DEMO: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/examples/demo/demo.kicad_pcb");

fn dir() -> std::path::PathBuf {
    let d = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("selection");
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// route `pcb` with extra args; returns the parsed delta
fn route(pcb: &std::path::Path, out: &std::path::Path, extra: &[&str]) -> serde_json::Value {
    let dj = out.with_extension("json");
    let o = Command::new(env!("CARGO_BIN_EXE_copperpath"))
        .args(["route", "--threads", "2", "--json", "--pcb"])
        .arg(pcb)
        .arg("--out")
        .arg(out)
        .arg("--delta-json")
        .arg(&dj)
        .args(extra)
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    serde_json::from_str(&std::fs::read_to_string(&dj).unwrap()).unwrap()
}

/// uuid -> (geometry in nm, net) of every track and via
fn copper(b: &board::Board) -> std::collections::BTreeMap<String, (Vec<i64>, String)> {
    let nm = delta::nm;
    let mut m = std::collections::BTreeMap::new();
    for t in &b.tracks {
        let g = vec![nm(t.a.x), nm(t.a.y), nm(t.b.x), nm(t.b.y), nm(t.w), t.layer as i64];
        m.insert(t.uuid.clone(), (g, b.nets[t.net].clone()));
    }
    for v in &b.vias {
        let g = vec![nm(v.at.x), nm(v.at.y), nm(v.d), nm(v.drill)];
        m.insert(v.uuid.clone(), (g, b.nets[v.net].clone()));
    }
    assert_eq!(m.len(), b.tracks.len() + b.vias.len(), "duplicate UUIDs");
    m
}

fn nets_of(dv: &serde_json::Value) -> std::collections::BTreeSet<String> {
    dv["add"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["net"].as_str().unwrap().to_string())
        .collect()
}

#[test]
fn route_the_rest_keeps_manual_copper_and_selected_nets_stay_selected() {
    let d = dir();
    // "manual work": two nets routed first (unlocked copper, as drawn by hand)
    let manual = d.join("manual.kicad_pcb");
    let dv = route(DEMO.as_ref(), &manual, &["--nets", "SDA,SCL"]);
    let want: std::collections::BTreeSet<String> = ["SDA", "SCL"].map(String::from).into();
    assert_eq!(nets_of(&dv), want, "selected nets: only their copper is added");
    assert_eq!(dv["remove"].as_array().map(|r| r.len()), Some(0));
    let before = copper(&board::load(manual.to_str().unwrap()).unwrap());
    assert!(before.len() > 2 && before.keys().all(|u| !u.is_empty()));

    // route the rest: nothing removed, every manual item unchanged, the manual nets get nothing
    let rest = d.join("rest.kicad_pcb");
    let dv = route(&manual, &rest, &[]);
    assert_eq!(dv["remove"].as_array().map(|r| r.len()), Some(0));
    let added = nets_of(&dv);
    assert!(!added.is_empty() && added.is_disjoint(&want), "{added:?}");
    let after = copper(&board::load(rest.to_str().unwrap()).unwrap());
    for (uuid, item) in &before {
        assert_eq!(after.get(uuid), Some(item), "manual item {uuid} changed or vanished");
    }
}
