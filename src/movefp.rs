//! Footprint move primitive for the board writer: translate and rotate a footprint in the
//! `.kicad_pcb` text, keeping everything else byte for byte.
//!
//! KiCad stores the children of a footprint in its own frame: pads, graphics and fields keep
//! their local positions when the footprint moves, but the angles of pads and fields
//! (`property`, `fp_text`) are absolute and turn with the footprint. Zones (rule areas) inside a
//! footprint are stored in board coordinates and are transformed point by point. The rotation
//! is about the footprint origin (its `at`).

use crate::geom::*;
use crate::sexpr::{self, List, Node};

/// a number as KiCad writes it: up to 6 decimals, no trailing zeros
pub fn num(v: f64) -> String {
    let v = if v.abs() < 5e-7 { 0.0 } else { v };
    let s = format!("{v:.6}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s == "-0" {
        "0".into()
    } else {
        s.to_string()
    }
}

/// angle in (-180, 180]
pub fn norm_angle(a: f64) -> f64 {
    let mut a = a.rem_euclid(360.0);
    if a > 180.0 + 1e-9 {
        a -= 360.0;
    }
    if (a - a.round()).abs() < 1e-9 {
        a = a.round();
    }
    a
}

/// `(at x y [angle] rest..)` with a new position and angle (an angle of 0 is left out, as
/// KiCad does; trailing atoms such as `unlocked` are kept)
fn at_list(l: &List, x: f64, y: f64, angle: f64) -> String {
    let mut rest: Vec<String> = Vec::new();
    let has_angle = l.argf(2).is_some();
    for n in l.items.iter().skip(if has_angle { 4 } else { 3 }) {
        match n {
            Node::Atom(a) => rest.push(a.clone()),
            Node::List(_) => {}
        }
    }
    let a = norm_angle(angle);
    let mut s = format!("(at {} {}", num(x), num(y));
    if a != 0.0 {
        s += &format!(" {}", num(a));
    }
    for r in rest {
        s += " ";
        s += &r;
    }
    s.push(')');
    s
}

/// the text edits (byte range of the footprint text, replacement) that move the footprint
/// whose list is `fp` (spans relative to `off`) from (at0, rot0) to (at1, rot1)
fn edits(
    fp: &List,
    off: usize,
    at0: P,
    rot0: f64,
    at1: P,
    rot1: f64,
) -> Vec<(usize, usize, String)> {
    let drot = rot1 - rot0;
    let mut ed = Vec::new();
    for c in fp.lists() {
        match c.head() {
            "at" => ed.push((
                c.span.0 + off,
                c.span.1 + off,
                at_list(c, at1.x, at1.y, rot1),
            )),
            "pad" | "property" | "fp_text" => {
                if drot.abs() < 1e-12 {
                    continue;
                }
                if let Some(a) = c.child("at") {
                    let (x, y) = (a.argf(0).unwrap_or(0.0), a.argf(1).unwrap_or(0.0));
                    let ang = a.argf(2).unwrap_or(0.0) + drot;
                    ed.push((a.span.0 + off, a.span.1 + off, at_list(a, x, y, ang)));
                }
            }
            "zone" => {
                // board coordinates: every (xy x y) of the outline and the saved fill
                let mut stack: Vec<&List> = vec![c];
                while let Some(l) = stack.pop() {
                    for ch in l.lists() {
                        if ch.head() == "xy" {
                            if let (Some(x), Some(y)) = (ch.argf(0), ch.argf(1)) {
                                let q = at1 + (p(x, y) - at0).rot(-rot0).rot(rot1);
                                ed.push((
                                    ch.span.0 + off,
                                    ch.span.1 + off,
                                    format!("(xy {} {})", num(q.x), num(q.y)),
                                ));
                            }
                        } else {
                            stack.push(ch);
                        }
                    }
                }
            }
            _ => {}
        }
    }
    ed
}

/// apply non-overlapping edits (start, end, replacement) to `text`
pub fn apply_edits(text: &str, mut ed: Vec<(usize, usize, String)>) -> String {
    ed.sort_by_key(|e| e.0);
    let mut out = String::with_capacity(text.len() + 64 * ed.len());
    let mut last = 0;
    for (s, e, r) in ed {
        if s < last {
            continue;
        }
        out.push_str(&text[last..s]);
        out.push_str(&r);
        last = e;
    }
    out.push_str(&text[last..]);
    out
}

/// one footprint move: reference, new position and new rotation (degrees)
#[derive(Debug, Clone)]
pub struct FpMove {
    pub refd: String,
    pub at: P,
    pub rot: f64,
}

/// the board text with the footprints moved (translation and rotation about the footprint
/// origin; the side stays). Unknown references are an error.
pub fn move_footprints(text: &str, moves: &[FpMove]) -> Result<String, String> {
    let root = sexpr::parse(text)?;
    let mut ed = Vec::new();
    for mv in moves {
        let fp = root
            .children("footprint")
            .find(|f| {
                f.children("property")
                    .any(|pr| pr.arg(0) == Some("Reference") && pr.arg(1) == Some(&mv.refd))
                    || f.children("fp_text").any(|t| {
                        t.arg(0) == Some("reference") && t.arg(1) == Some(mv.refd.as_str())
                    })
            })
            .ok_or_else(|| format!("footprint {} not found", mv.refd))?;
        let (x, y, r) = fp.xy("at").unwrap_or((0.0, 0.0, 0.0));
        ed.extend(edits(fp, 0, p(x, y), r, mv.at, mv.rot));
    }
    Ok(apply_edits(text, ed))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::board::parse_board;

    const B: &str = "(kicad_pcb (version 20260206) (layers (0 \"F.Cu\" signal) (25 \"Edge.Cuts\" user))\n\
        (footprint \"R_0603\" (layer \"F.Cu\") (at 10 20)\n\
          (property \"Reference\" \"R1\" (at 0 -1.4 0) (layer \"F.SilkS\"))\n\
          (fp_line (start -1 -0.5) (end 1 -0.5) (layer \"F.CrtYd\"))\n\
          (pad \"1\" smd rect (at -0.8 0) (size 0.8 0.9) (layers \"F.Cu\") (net \"A\"))\n\
          (pad \"2\" smd rect (at 0.8 0 0) (size 0.8 0.9) (layers \"F.Cu\") (net \"B\"))\n\
          (zone (net 0) (layers \"F.Cu\") (keepout (tracks not_allowed) (vias not_allowed) (pads allowed) (copperpour allowed) (footprints allowed))\n\
            (polygon (pts (xy 9.5 19.5) (xy 10.5 19.5) (xy 10.5 20.5)))))\n\
        (footprint \"X\" (layer \"F.Cu\") (at 1 1) (property \"Reference\" \"R2\" (at 0 0)))\n)";

    #[test]
    fn number_format() {
        assert_eq!(num(1.0), "1");
        assert_eq!(num(-0.0000001), "0");
        assert_eq!(num(12.3456789), "12.345679");
        assert_eq!(num(-2.5), "-2.5");
        assert_eq!(norm_angle(270.0), -90.0);
        assert_eq!(norm_angle(-180.0), 180.0);
    }

    #[test]
    fn translate_and_rotate_footprint() {
        let t = move_footprints(
            B,
            &[FpMove {
                refd: "R1".into(),
                at: p(10.6, 20.0),
                rot: 90.0,
            }],
        )
        .unwrap();
        assert!(t.contains("(at 10.6 20 90)"), "{t}");
        assert!(t.contains("(pad \"1\" smd rect (at -0.8 0 90)"), "{t}");
        assert!(t.contains("(pad \"2\" smd rect (at 0.8 0 90)"), "{t}");
        assert!(
            t.contains("(property \"Reference\" \"R1\" (at 0 -1.4 90)"),
            "{t}"
        );
        // local graphics stay, the other footprint is untouched
        assert!(t.contains("(fp_line (start -1 -0.5) (end 1 -0.5)"));
        assert!(t.contains("(at 1 1) (property \"Reference\" \"R2\" (at 0 0))"));
        // the parsed pads match the moved footprint geometry
        let b0 = parse_board(B.to_string()).unwrap();
        let b1 = parse_board(t).unwrap();
        let (f0, f1) = (b0.fp("R1").unwrap(), b1.fp("R1").unwrap());
        let g = f0.moved(p(10.6, 20.0), 90.0, false);
        for (pa, pb) in g.pads.iter().zip(&f1.pads) {
            let (ca, aa) = g.pad_world(pa);
            let (cb, ab) = f1.pad_world(pb);
            assert!((ca - cb).len() < 1e-6 && (aa - ab).abs() < 1e-6);
        }
        // footprint rule area: board coordinates, moved with the footprint
        let z0 = &b0.zones[0].outline;
        let z1 = &b1.zones[0].outline;
        for (a, b) in z0.iter().zip(z1) {
            let want = p(10.6, 20.0) + (*a - p(10.0, 20.0)).rot(90.0);
            assert!((want - *b).len() < 1e-5, "{want:?} {b:?}");
        }
    }

    #[test]
    fn rotating_back_drops_the_angle() {
        let t = move_footprints(
            B,
            &[FpMove {
                refd: "R1".into(),
                at: p(10.0, 20.0),
                rot: 90.0,
            }],
        )
        .unwrap();
        let t2 = move_footprints(
            &t,
            &[FpMove {
                refd: "R1".into(),
                at: p(10.0, 20.0),
                rot: 0.0,
            }],
        )
        .unwrap();
        assert!(t2.contains("(footprint \"R_0603\" (layer \"F.Cu\") (at 10 20)"));
        assert!(t2.contains("(pad \"2\" smd rect (at 0.8 0)"));
        assert!(move_footprints(
            B,
            &[FpMove {
                refd: "R9".into(),
                at: p(0.0, 0.0),
                rot: 0.0
            }]
        )
        .is_err());
    }
}
