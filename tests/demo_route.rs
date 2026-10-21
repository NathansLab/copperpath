//! Routes the demo board with the release binary and checks the result:
//! 0 opens, only 45-degree multiples, no track copper inside a foreign pad, straight pad exits.

use placeroute::board;
use placeroute::geom::{shape_dist, Shape};
use std::process::Command;

const DEMO: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/examples/demo/demo.kicad_pcb");

fn octi(dx: f64, dy: f64) -> bool {
    let (ax, ay) = (dx.abs(), dy.abs());
    let tol = 1e-4;
    ax <= tol || ay <= tol || (ax - ay).abs() <= tol
}

#[test]
fn demo_routes_completely_and_cleanly() {
    let out_dir = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("demo_route");
    let out = out_dir.join("demo_routed.kicad_pcb");
    let o = Command::new(env!("CARGO_BIN_EXE_placeroute"))
        .args(["route", "--pcb", DEMO, "--threads", "4", "--json", "--out"])
        .arg(&out)
        .output()
        .expect("run placeroute");
    assert!(
        o.status.success(),
        "placeroute failed: {}",
        String::from_utf8_lossy(&o.stderr)
    );
    let v: serde_json::Value =
        serde_json::from_slice(&o.stdout).expect("--json prints one JSON object");
    assert_eq!(v["ok"], true);
    assert_eq!(v["opens"], 0, "opens: {}", v["open_nets"]);
    assert_eq!(v["failed"].as_array().map(|a| a.len()), Some(0));
    assert_eq!(v["aes"]["exact_fail"], 0);
    assert!(v["connections"].as_u64().unwrap() >= 40);
    let short: Vec<String> = v["aes"]["short_stubs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s.as_str().unwrap().to_string())
        .collect();

    let b = board::load(out.to_str().unwrap()).unwrap();
    assert!(!b.tracks.is_empty());
    // 4-layer board: nothing on In3/In4
    assert!(
        b.tracks.iter().all(|t| b.cu_mask & (1 << t.layer) != 0),
        "track on a layer the board does not have"
    );

    let mut exits = 0;
    for t in &b.tracks {
        let d = t.b - t.a;
        assert!(
            octi(d.x, d.y),
            "segment {:?} -> {:?} is not a 45-degree multiple",
            t.a,
            t.b
        );
        let cap = Shape::Capsule {
            a: t.a,
            b: t.b,
            r: t.w / 2.0,
        };
        for f in &b.fps {
            for pd in &f.pads {
                if pd.layers & (1 << t.layer) == 0 {
                    continue;
                }
                let (c, ang) = f.pad_world(pd);
                for s in f.pad_shapes(pd) {
                    if pd.net != t.net {
                        // never inside or touching a foreign pad (min clearance of the demo: 0.15 mm)
                        assert!(
                            shape_dist(&cap, &s) > 0.15 - 1e-3,
                            "track of {} too close to {}.{}",
                            b.nets[t.net],
                            f.refd,
                            pd.num
                        );
                        continue;
                    }
                    // own pad: a track end inside the pad must be the pad centre, and the segment
                    // leaves straight along a pad axis for at least half the pad extent + 0.12 mm
                    for (e, other) in [(t.a, t.b), (t.b, t.a)] {
                        if s.dist(e) > 1e-6 {
                            continue;
                        }
                        assert!(
                            (e - c).len() < 2e-3,
                            "track end {:?} inside {}.{} but not at its centre {:?}",
                            e,
                            f.refd,
                            pd.num,
                            c
                        );
                        let u = (other - e) * (1.0 / (other - e).len());
                        let ax = placeroute::geom::p(1.0, 0.0).rot(ang);
                        let along_x = u.cross(ax).abs() < 1e-3;
                        let along_y = u.dot(ax).abs() < 1e-3;
                        assert!(
                            along_x || along_y,
                            "exit of {}.{} is not along a pad axis",
                            f.refd,
                            pd.num
                        );
                        let half = if along_x {
                            pd.size.0 / 2.0
                        } else {
                            pd.size.1 / 2.0
                        };
                        let name = format!("{}.{}", f.refd, pd.num);
                        let min = if short.contains(&name) {
                            half + 0.12
                        } else {
                            half + 0.5
                        };
                        assert!(
                            (other - e).len() >= min - 1e-3,
                            "exit stub of {name} is {:.3} mm, needs {:.3}",
                            (other - e).len(),
                            min
                        );
                        exits += 1;
                    }
                }
            }
        }
    }
    assert!(exits >= 70, "only {exits} pad exits found");
}

#[test]
fn errors_are_reported_with_exit_codes() {
    let bin = env!("CARGO_BIN_EXE_placeroute");
    let o = Command::new(bin)
        .args(["route", "--pcb", "does_not_exist.kicad_pcb", "--json"])
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(1));
    let v: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(v["ok"], false);
    assert!(v["error"].as_str().unwrap().contains("not found"));
    let o = Command::new(bin).args(["bogus"]).output().unwrap();
    assert_eq!(o.status.code(), Some(2));
    let o = Command::new(bin).args(["--version"]).output().unwrap();
    assert!(String::from_utf8_lossy(&o.stdout).starts_with("placeroute "));
}
