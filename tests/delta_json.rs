//! `route --delta-json` (#20): applying the delta to the input board gives the `--out` board
//! (same tracks and vias within 1 nm, same opens); `--pcb -` reads the board from stdin.

use copperpath::{board, delta};
use std::io::Write;
use std::process::{Command, Stdio};

const DEMO: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/examples/demo/demo.kicad_pcb");
const PRO: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/examples/demo/demo.kicad_pro");
const DRU: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/examples/demo/demo.kicad_dru");

fn dir() -> std::path::PathBuf {
    let d = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("delta_json");
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// tracks and vias in integer nm, sorted
fn copper(b: &board::Board) -> (Vec<[i64; 6]>, Vec<[i64; 5]>) {
    let nm = delta::nm;
    let mut t: Vec<[i64; 6]> = b
        .tracks
        .iter()
        .map(|t| {
            [
                nm(t.a.x),
                nm(t.a.y),
                nm(t.b.x),
                nm(t.b.y),
                nm(t.w),
                (t.layer * 100000 + t.net) as i64,
            ]
        })
        .collect();
    let mut v: Vec<[i64; 5]> = b
        .vias
        .iter()
        .map(|v| [nm(v.at.x), nm(v.at.y), nm(v.d), nm(v.drill), v.net as i64])
        .collect();
    t.sort();
    v.sort();
    (t, v)
}

fn opens(pcb: &std::path::Path) -> serde_json::Value {
    let o = Command::new(env!("CARGO_BIN_EXE_copperpath"))
        .args(["opens", "--json", "--pro", PRO, "--dru", DRU, "--pcb"])
        .arg(pcb)
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let v: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    serde_json::json!({"opens": v["opens"], "per_net": v["per_net"]})
}

#[test]
fn delta_applied_to_the_input_equals_the_output_board() {
    let d = dir();
    let (out, dj) = (d.join("routed.kicad_pcb"), d.join("delta.json"));
    let o = Command::new(env!("CARGO_BIN_EXE_copperpath"))
        .args(["route", "--pcb", DEMO, "--threads", "4", "--json", "--out"])
        .arg(&out)
        .arg("--delta-json")
        .arg(&dj)
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let v: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(v["delta_json"], dj.to_str().unwrap());

    let dt = std::fs::read_to_string(&dj).unwrap();
    let dv: serde_json::Value = serde_json::from_str(&dt).unwrap();
    assert_eq!((dv["v"].as_i64(), dv["units"].as_str()), (Some(1), Some("nm")));
    let add = dv["add"].as_array().unwrap();
    assert_eq!(
        add.iter().filter(|i| i["type"] == "track").count() as u64,
        v["segments"].as_u64().unwrap()
    );
    assert_eq!(
        add.iter().filter(|i| i["type"] == "via").count() as u64,
        v["vias"].as_u64().unwrap()
    );

    // delta::apply refuses non-integer lengths
    let input = std::fs::read_to_string(DEMO).unwrap();
    let applied = d.join("applied.kicad_pcb");
    let txt = delta::apply(&input, &dv).unwrap();
    std::fs::write(&applied, txt).unwrap();
    let a = board::load(applied.to_str().unwrap()).unwrap();
    let b = board::load(out.to_str().unwrap()).unwrap();
    assert_eq!(a.nets, b.nets);
    let ((ta, va), (tb, vb)) = (copper(&a), copper(&b));
    assert_eq!((ta.len(), va.len()), (tb.len(), vb.len()));
    for (x, y) in ta.iter().zip(&tb) {
        assert!(x.iter().zip(y).all(|(p, q)| (p - q).abs() <= 1), "{x:?} != {y:?}");
    }
    for (x, y) in va.iter().zip(&vb) {
        assert!(x.iter().zip(y).all(|(p, q)| (p - q).abs() <= 1), "{x:?} != {y:?}");
    }
    assert_eq!(opens(&applied), opens(&out));
}

#[test]
fn board_from_stdin_and_delta_only() {
    let d = dir();
    let dj = d.join("stdin_delta.json");
    let mut ch = Command::new(env!("CARGO_BIN_EXE_copperpath"))
        .args([
            "route",
            "--pcb",
            "-",
            "--pro",
            PRO,
            "--dru",
            DRU,
            "--threads",
            "2",
            "--json",
            "--nets",
            "SDA,SCL",
            "--delta-json",
        ])
        .arg(&dj)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    ch.stdin
        .take()
        .unwrap()
        .write_all(&std::fs::read(DEMO).unwrap())
        .unwrap();
    let o = ch.wait_with_output().unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let v: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(v["out"], serde_json::Value::Null);
    assert_eq!(v["opens"], 0, "{}", v["open_nets"]);
    let dt = std::fs::read_to_string(&dj).unwrap();
    let dv: serde_json::Value = serde_json::from_str(&dt).unwrap();
    let add = dv["add"].as_array().unwrap();
    assert!(!add.is_empty());
    assert!(add.iter().all(|i| i["net"] == "SDA" || i["net"] == "SCL"));
    assert_eq!(dv["remove"].as_array().map(|r| r.len()), Some(0));
}
