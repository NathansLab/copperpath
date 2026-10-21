//! --time-limit and SIGTERM end the run early and still write the best result so far (#8).

use std::process::Command;

const DEMO: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/examples/demo/demo.kicad_pcb");

fn out(name: &str) -> std::path::PathBuf {
    let dir = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("stop_early");
    std::fs::create_dir_all(&dir).unwrap();
    let o = dir.join(name);
    let _ = std::fs::remove_file(&o);
    o
}

#[test]
fn time_limit_writes_a_result() {
    let o = out("tl.kicad_pcb");
    let r = Command::new(env!("CARGO_BIN_EXE_copperpath"))
        .args([
            "route",
            "--pcb",
            DEMO,
            "--threads",
            "2",
            "--time-limit",
            "0.3",
            "--json",
            "--out",
        ])
        .arg(&o)
        .output()
        .unwrap();
    assert!(r.status.success(), "{}", String::from_utf8_lossy(&r.stderr));
    let v: serde_json::Value = serde_json::from_slice(&r.stdout).unwrap();
    assert_eq!(v["stopped"], "time-limit");
    assert!(v["hotspots"].is_array());
    assert!(o.exists());
}

#[cfg(unix)]
#[test]
fn sigterm_writes_a_result() {
    let o = out("term.kicad_pcb");
    let child = Command::new(env!("CARGO_BIN_EXE_copperpath"))
        .args(["route", "--pcb", DEMO, "--threads", "1", "--json", "--out"])
        .arg(&o)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(700));
    Command::new("kill")
        .args(["-TERM", &child.id().to_string()])
        .status()
        .unwrap();
    let r = child.wait_with_output().unwrap();
    assert!(r.status.success(), "{}", String::from_utf8_lossy(&r.stderr));
    let v: serde_json::Value = serde_json::from_slice(&r.stdout).unwrap();
    // a fast machine may finish before the signal; then nothing was stopped
    assert!(v["stopped"] == "signal" || v["stopped"].is_null());
    assert!(o.exists());
}
