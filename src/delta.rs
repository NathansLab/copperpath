//! Change sets for the KiCad IPC API (`route --delta-json`): the items a run removes from the
//! input board (by UUID) and the items it adds, in integer nanometres (KiCad's internal unit).
//!
//! Schema version 1:
//! `{"v":1,"units":"nm","remove":["<uuid>"],"add":[item]}` with items
//! `{"type":"track","net","layer","width","start":[x,y],"end":[x,y]}`,
//! `{"type":"arc",...,"mid":[x,y],...}` and
//! `{"type":"via","net","at":[x,y],"size","drill","layers":["F.Cu","B.Cu"],"via_type":"through"}`.

use crate::board::CU;
use crate::droute::DResult;
use crate::sexpr;
use serde_json::{json, Value};
use std::collections::HashSet;

/// millimetres -> integer nanometres
pub fn nm(mm: f64) -> i64 {
    (mm * 1e6).round() as i64
}

fn mm(v: &Value) -> Result<f64, String> {
    v.as_i64()
        .map(|n| n as f64 * 1e-6)
        .ok_or_else(|| format!("expected an integer (nm), got {v}"))
}

fn xy(v: &Value) -> Result<(f64, f64), String> {
    match v.as_array().map(|a| a.as_slice()) {
        Some([x, y]) => Ok((mm(x)?, mm(y)?)),
        _ => Err(format!("expected [x, y], got {v}")),
    }
}

/// the change set of a route run: `remove` lists UUIDs of input items that were ripped or
/// replaced, `add` the routed copper in the same order as the board `write_pcb` writes
pub fn route_delta(nets: &[String], res: &DResult, remove: &[String]) -> Value {
    let mut add = Vec::new();
    let mut keys: Vec<&usize> = res.routed.keys().collect();
    keys.sort();
    for &net in keys {
        let rn = &res.routed[&net];
        for s in &rn.segs {
            add.push(
                json!({"type": "track", "net": nets[net], "layer": CU[s.l], "width": nm(s.w),
                "start": [nm(s.a.x), nm(s.a.y)], "end": [nm(s.b.x), nm(s.b.y)]}),
            );
        }
        for v in &rn.vias {
            add.push(json!({"type": "via", "net": nets[net], "at": [nm(v.at.x), nm(v.at.y)],
                "size": nm(v.d), "drill": nm(v.drill), "layers": ["F.Cu", "B.Cu"], "via_type": "through"}));
        }
    }
    json!({"v": 1, "units": "nm", "remove": remove, "add": add})
}

/// apply a change set to board text: drop the top-level segments, arcs and vias whose UUID is in
/// `remove`, append the `add` items (what the IPC plugin does through a commit)
pub fn apply(text: &str, delta: &Value) -> Result<String, String> {
    if delta["v"] != 1 || delta["units"] != "nm" {
        return Err("unsupported delta: expected v 1 and units nm".into());
    }
    let remove: HashSet<&str> = delta["remove"]
        .as_array()
        .map(|a| a.iter().filter_map(|u| u.as_str()).collect())
        .unwrap_or_default();
    let root = sexpr::parse(text)?;
    let mut cut: Vec<(usize, usize)> = root
        .lists()
        .filter(|it| matches!(it.head(), "segment" | "arc" | "via"))
        .filter(|it| it.val("uuid").is_some_and(|u| remove.contains(u)))
        .map(|it| it.span)
        .collect();
    cut.sort();
    let mut o = String::with_capacity(text.len());
    let mut at = 0;
    for (s, e) in cut {
        o.push_str(&text[at..s]);
        at = e;
    }
    let close = text[at..]
        .rfind(')')
        .ok_or("board text without a closing ')'")?;
    let end = at + close;
    o.push_str(&text[at..end]);
    let add = delta["add"].as_array().map(Vec::as_slice).unwrap_or(&[]);
    for it in add {
        let net = sexpr::q(it["net"].as_str().unwrap_or(""));
        match it["type"].as_str() {
            Some(t @ ("track" | "arc")) => {
                let (s, e) = (xy(&it["start"])?, xy(&it["end"])?);
                let mid = if t == "arc" {
                    let m = xy(&it["mid"])?;
                    format!("\t\t(mid {:.6} {:.6})\n", m.0, m.1)
                } else {
                    String::new()
                };
                o += &format!(
                    "\t({}\n\t\t(start {:.6} {:.6})\n{mid}\t\t(end {:.6} {:.6})\n\t\t(width {:.6})\n\t\t(layer \"{}\")\n\t\t(net {})\n\t)\n",
                    if t == "arc" { "arc" } else { "segment" },
                    s.0, s.1, e.0, e.1, mm(&it["width"])?, it["layer"].as_str().unwrap_or("F.Cu"), net
                );
            }
            Some("via") => {
                let a = xy(&it["at"])?;
                let layers: Vec<String> = it["layers"]
                    .as_array()
                    .map(|l| l.iter().filter_map(|s| s.as_str()).map(sexpr::q).collect())
                    .unwrap_or_default();
                o += &format!(
                    "\t(via\n\t\t(at {:.6} {:.6})\n\t\t(size {:.6})\n\t\t(drill {:.6})\n\t\t(layers {})\n\t\t(net {})\n\t)\n",
                    a.0, a.1, mm(&it["size"])?, mm(&it["drill"])?, layers.join(" "), net
                );
            }
            t => return Err(format!("unknown item type {t:?}")),
        }
    }
    o.push_str(&text[end..]);
    Ok(o)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::board::parse_board;

    const MINI: &str = "(kicad_pcb (version 20260206) (layers (0 \"F.Cu\" signal) (2 \"B.Cu\" signal))\n\
        (segment (start 0 0) (end 1 0) (width 0.2) (layer \"F.Cu\") (net \"A\") (uuid \"keep\"))\n\
        (segment (start 0 1) (end 1 1) (width 0.2) (layer \"F.Cu\") (net \"A\") (uuid \"gone\"))\n\
        (via (at 2 2) (size 0.6) (drill 0.3) (layers \"F.Cu\" \"B.Cu\") (net \"A\") (uuid \"gone2\")))";

    #[test]
    fn apply_removes_by_uuid_and_adds_in_nm() {
        let d = json!({"v": 1, "units": "nm", "remove": ["gone", "gone2"], "add": [
            {"type": "track", "net": "B", "layer": "B.Cu", "width": 150000, "start": [1, 2], "end": [3000000, 2]},
            {"type": "arc", "net": "B", "layer": "F.Cu", "width": 150000, "start": [0, 0], "mid": [1000000, 1000000], "end": [2000000, 0]},
            {"type": "via", "net": "B", "at": [5000000, 5000000], "size": 600000, "drill": 300000, "layers": ["F.Cu", "B.Cu"], "via_type": "through"}]});
        let b = parse_board(apply(MINI, &d).unwrap()).unwrap();
        // kept segment + track + arc (two chords)
        assert_eq!(b.tracks.len(), 4);
        assert_eq!(b.tracks[0].uuid, "keep");
        let t = &b.tracks[1];
        assert_eq!((nm(t.a.x), nm(t.a.y), nm(t.b.x)), (1, 2, 3000000));
        assert_eq!((b.nets[t.net].as_str(), t.layer, nm(t.w)), ("B", 5, 150000));
        assert_eq!(b.vias.len(), 1);
        assert_eq!((nm(b.vias[0].at.x), nm(b.vias[0].d)), (5000000, 600000));
    }

    #[test]
    fn apply_rejects_unknown_versions() {
        assert!(apply(MINI, &json!({"v": 2, "units": "nm"})).is_err());
    }
}
