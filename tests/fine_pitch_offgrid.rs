//! Fine-pitch pins off the routing grid (GitHub issue #1): a synthetic QFN-48 (0.4 mm pitch,
//! 0.8 x 0.2 mm pads, default class: 0.2 mm clearance, 0.2 mm neck-down) whose pad axes sit
//! 0.0164 mm off the 0.05 mm cell centres, one track per pin to four pin headers.
//! A 0.2 mm track between the pads fits only exactly on the pad axis, so every pin needs an
//! exact pad-axis stub; all of them must get one and the board must route completely.

use copperpath::board;
use copperpath::geom::{shape_dist, Shape};
use std::fmt::Write as _;
use std::process::Command;

/// synthetic 2-layer board: QFN-48 at (cx, cy), headers 1x12 (1.0 mm pitch) 6 mm outside each side
fn qfn48_board(cx: f64, cy: f64) -> String {
    let mut s = String::from(
        "(kicad_pcb\n\t(version 20260206)\n\t(generator \"copperpath-test\")\n\t(layers\n\t\t(0 \"F.Cu\" signal)\n\t\t(2 \"B.Cu\" signal)\n\t\t(25 \"Edge.Cuts\" user)\n\t)\n",
    );
    s += "\t(gr_rect\n\t\t(start 0 0)\n\t\t(end 30 30)\n\t\t(stroke (width 0.1) (type solid))\n\t\t(fill no)\n\t\t(layer \"Edge.Cuts\")\n\t)\n";
    let pad = |s: &mut String, num: usize, x: f64, y: f64, a: f64, w: f64, h: f64, net: &str| {
        let _ = write!(
            s,
            "\t\t(pad \"{num}\" smd roundrect\n\t\t\t(at {x:.4} {y:.4} {a})\n\t\t\t(size {w} {h})\n\t\t\t(layers \"F.Cu\" \"F.Mask\" \"F.Paste\")\n\t\t\t(roundrect_rratio 0.25)\n\t\t\t(net \"{net}\")\n\t\t)\n"
        );
    };
    // QFN-48: 12 pads per side, counter-clockwise from the top of the left row
    s += &format!("\t(footprint \"Package_DFN_QFN:QFN-48_6x6mm_P0.4mm\"\n\t\t(layer \"F.Cu\")\n\t\t(at {cx:.4} {cy:.4})\n\t\t(property \"Reference\" \"U1\"\n\t\t\t(at 0 -4 0)\n\t\t\t(layer \"F.SilkS\")\n\t\t)\n");
    for side in 0..4 {
        for i in 0..12 {
            let t = -2.2 + 0.4 * i as f64;
            let (x, y, a) = match side {
                0 => (-2.9, t, 0.0),
                1 => (t, 2.9, 90.0),
                2 => (2.9, -t, 0.0),
                _ => (-t, -2.9, 90.0),
            };
            let n = side * 12 + i + 1;
            pad(&mut s, n, x, y, a, 0.8, 0.2, &format!("P{n}"));
        }
    }
    s += "\t)\n";
    // headers: pin k of header `side` carries the net of QFN pin side * 12 + k
    for side in 0..4 {
        let (hx, hy) = match side {
            0 => (cx - 9.0, cy),
            1 => (cx, cy + 9.0),
            2 => (cx + 9.0, cy),
            _ => (cx, cy - 9.0),
        };
        let _ = write!(
            s,
            "\t(footprint \"Connector:PinHeader_1x12_P1.0mm\"\n\t\t(layer \"F.Cu\")\n\t\t(at {hx:.4} {hy:.4})\n\t\t(property \"Reference\" \"J{}\"\n\t\t\t(at 0 -7 0)\n\t\t\t(layer \"F.SilkS\")\n\t\t)\n",
            side + 1
        );
        for i in 0..12 {
            let t = -5.5 + i as f64;
            let (x, y) = match side {
                0 => (0.0, t),
                1 => (t, 0.0),
                2 => (0.0, -t),
                _ => (-t, 0.0),
            };
            let n = side * 12 + i + 1;
            let _ = write!(
                s,
                "\t\t(pad \"{}\" thru_hole circle\n\t\t\t(at {x:.4} {y:.4})\n\t\t\t(size 0.7 0.7)\n\t\t\t(drill 0.4)\n\t\t\t(layers \"*.Cu\" \"*.Mask\")\n\t\t\t(net \"P{n}\")\n\t\t)\n",
                i + 1
            );
        }
        s += "\t)\n";
    }
    s += ")\n";
    s
}

fn route(name: &str, cx: f64, cy: f64) -> (serde_json::Value, board::Board) {
    let dir = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("fine_pitch_offgrid");
    std::fs::create_dir_all(&dir).unwrap();
    let pcb = dir.join(format!("{name}.kicad_pcb"));
    std::fs::write(&pcb, qfn48_board(cx, cy)).unwrap();
    let out = dir.join(format!("{name}_routed.kicad_pcb"));
    let o = Command::new(env!("CARGO_BIN_EXE_copperpath"))
        .args(["route", "--threads", "4", "--json", "--pcb"])
        .arg(&pcb)
        .arg("--out")
        .arg(&out)
        .output()
        .expect("run copperpath");
    std::fs::write(dir.join(format!("{name}.log")), &o.stderr).unwrap();
    assert!(
        o.status.success(),
        "copperpath failed: {}",
        String::from_utf8_lossy(&o.stderr)
    );
    let v: serde_json::Value =
        serde_json::from_slice(&o.stdout).expect("--json prints one JSON object");
    (v, board::load(out.to_str().unwrap()).unwrap())
}

fn check(name: &str, cx: f64, cy: f64) {
    let (v, b) = route(name, cx, cy);
    assert_eq!(v["ok"], true);
    assert_eq!(v["aes"]["pins_no_stub"], serde_json::json!([]), "{name}");
    assert_eq!(v["opens"], 0, "{name}: opens: {}", v["open_nets"]);
    assert_eq!(v["connections"], 48);
    // QFN pins are left straight along the long pad axis to beyond the pad edge (exit stubs; a few
    // fallback pad ends in the crowded package corners are tolerated), and no track comes closer
    // than the 0.2 mm clearance to a foreign pad on its layer
    let u1 = b.fps.iter().find(|f| f.refd == "U1").unwrap();
    let mut straight = Vec::new();
    for pd in &u1.pads {
        let (c, ang) = u1.pad_world(pd);
        let ax = copperpath::geom::p(1.0, 0.0).rot(ang);
        let exit = b.tracks.iter().find_map(|t| {
            let o = if (t.a - c).len() < 1e-3 {
                t.b
            } else if (t.b - c).len() < 1e-3 {
                t.a
            } else {
                return None;
            };
            Some(o - c)
        });
        let d = exit.unwrap_or_else(|| panic!("{name}: no track starts at U1.{}", pd.num));
        // either end of the pad (no exposed pad: inward exits to a via are fine)
        if d.cross(ax).abs() < 1e-6 && d.len() > 0.4 {
            straight.push(pd.num.clone());
        }
    }
    eprintln!("{name}: {} straight pad-axis exits", straight.len());
    assert!(
        straight.len() >= 40,
        "{name}: only {} of 48 QFN pins have a straight pad-axis exit: {:?}",
        straight.len(),
        straight
    );
    for t in &b.tracks {
        let cap = Shape::Capsule {
            a: t.a,
            b: t.b,
            r: t.w / 2.0,
        };
        for f in &b.fps {
            for pd in f
                .pads
                .iter()
                .filter(|pd| pd.net != t.net && pd.layers & (1 << t.layer) != 0)
            {
                for s in f.pad_shapes(pd) {
                    let d = shape_dist(&cap, &s);
                    assert!(
                        d > 0.2 - 1e-6,
                        "{name}: track of {} {:?}-{:?} {:.6} mm from {}.{}",
                        b.nets[t.net],
                        t.a,
                        t.b,
                        d,
                        f.refd,
                        pd.num
                    );
                }
            }
        }
    }
}

#[test]
fn qfn48_off_grid_gets_exact_stubs() {
    // grid origin = board corner (0, 0); cell centres at 0.025 + k * 0.05 mm
    let c = 15.025 + 0.0164;
    check("qfn48_offgrid", c, c);
}

#[test]
fn qfn48_on_grid_reference() {
    check("qfn48_ongrid", 15.025, 15.025);
}
