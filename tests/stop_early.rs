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

/// run `route` on the demo, call `act` once negotiation is under way, then wait (at most 60 s)
fn run_and_cancel(
    o: &std::path::Path,
    extra: &[&str],
    act: impl FnOnce(&mut std::process::Child),
) -> serde_json::Value {
    let mut child = Command::new(env!("CARGO_BIN_EXE_copperpath"))
        .args(["route", "--pcb", DEMO, "--threads", "1", "--json"])
        .args(extra)
        .arg("--out")
        .arg(o)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(700));
    act(&mut child);
    let t = std::time::Instant::now();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || tx.send(child.wait_with_output().unwrap()));
    let r = rx
        .recv_timeout(std::time::Duration::from_secs(60))
        .expect("router did not stop after the cancel request");
    assert!(r.status.success(), "{}", String::from_utf8_lossy(&r.stderr));
    assert!(o.exists());
    let v: serde_json::Value = serde_json::from_slice(&r.stdout).unwrap();
    eprintln!(
        "exited {:.1} s after the cancel request, stopped: {}",
        t.elapsed().as_secs_f64(),
        v["stopped"]
    );
    v
}

#[test]
fn cancel_file_writes_a_result() {
    let o = out("cancel.kicad_pcb");
    let f = o.with_extension("cancel");
    let _ = std::fs::remove_file(&f);
    let v = run_and_cancel(&o, &["--cancel-file", f.to_str().unwrap()], |_| {
        std::fs::write(&f, "").unwrap()
    });
    // a fast machine may finish before the cancel file is seen; then nothing was stopped
    assert!(v["stopped"] == "cancel" || v["stopped"].is_null());
}

#[test]
fn stdin_eof_writes_a_result() {
    let o = out("eof.kicad_pcb");
    let v = run_and_cancel(&o, &["--cancel-on-stdin-eof"], |c| drop(c.stdin.take()));
    assert!(v["stopped"] == "cancel" || v["stopped"].is_null());
}

#[test]
fn stdin_eof_ignored_without_flag() {
    let o = out("no_eof.kicad_pcb");
    let v = run_and_cancel(&o, &["--time-limit", "1.5"], |c| drop(c.stdin.take()));
    assert_ne!(v["stopped"], "cancel");
}
