//! Schematic positions per reference from all .kicad_sch files of a project directory.
//! Reused sheets (e.g. several identical channels) carry one (instances (project (path ... (reference R)))) entry per
//! instance, so every instance gets the same relative arrangement.

use crate::geom::*;
use crate::sexpr;
use std::collections::HashMap;

#[derive(Debug, Clone)]
pub struct SchPos {
    pub file: String,
    pub at: P,
    pub rot: f64,
}

pub fn load_dir(dir: &str) -> HashMap<String, SchPos> {
    let mut out = HashMap::new();
    let Ok(rd) = std::fs::read_dir(dir) else {
        return out;
    };
    for e in rd.flatten() {
        let path = e.path();
        if path.extension().and_then(|s| s.to_str()) != Some("kicad_sch") {
            continue;
        }
        let Ok(t) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(root) = sexpr::parse(&t) else { continue };
        let file = path.file_name().unwrap().to_string_lossy().to_string();
        for sym in root.children("symbol") {
            let Some((x, y, r)) = sym.xy("at") else {
                continue;
            };
            let mut refs = Vec::new();
            if let Some(inst) = sym.child("instances") {
                for pr in inst.children("project") {
                    for pa in pr.children("path") {
                        if let Some(rf) = pa.val("reference") {
                            refs.push(rf.to_string());
                        }
                    }
                }
            }
            if refs.is_empty() {
                for pr in sym.children("property") {
                    if pr.arg(0) == Some("Reference") {
                        refs.push(pr.arg(1).unwrap_or("").to_string());
                    }
                }
            }
            for rf in refs {
                if rf.starts_with('#') {
                    continue;
                }
                out.insert(
                    rf,
                    SchPos {
                        file: file.clone(),
                        at: p(x, y),
                        rot: r,
                    },
                );
            }
        }
    }
    out
}
