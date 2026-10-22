//! Rule model from the single source of truth: net classes (.kicad_pro) + custom rules (.kicad_dru).
//! Semantics mirror KiCad's DRC engine as far as a net-level model allows:
//! - net class: first matching netclass pattern (wildcard or anchored regex), else Default;
//! - '==' against a string literal is a wildcard match for NetName/NetClass; hasNetclass() is exact;
//! - the last matching rule wins; without a match the larger of the two net-class clearances;
//! - (layer outer)/(layer inner) restrict a rule; creepage counts as clearance on outer layers.
//!
//! Item-specific conditions are evaluated only when the item supplies the attribute; at net level
//! a rule that needs an unknown item attribute is skipped.

use regex::Regex;
use serde_json::Value;
use std::collections::HashMap;

#[derive(Debug, Clone)]
pub struct NetClass {
    pub name: String,
    pub clearance: f64,
    pub track_width: f64,
    pub via_d: f64,
    pub via_drill: f64,
}

#[derive(Debug, Clone)]
enum Expr {
    Or(Box<Expr>, Box<Expr>),
    And(Box<Expr>, Box<Expr>),
    Not(Box<Expr>),
    Cmp {
        side: u8,
        attr: String,
        eq: bool,
        rhs: Rhs,
    },
    Func {
        side: u8,
        name: String,
        arg: String,
    },
    True,
}

#[derive(Debug, Clone)]
enum Rhs {
    Str(String),
    Attr(u8, String),
}

#[derive(Debug, Clone)]
pub struct Rule {
    pub name: String,
    pub layer: Option<String>,
    pub cons: HashMap<String, HashMap<String, f64>>,
    pub cons_flag: HashMap<String, String>,
    expr: Expr,
    pub item_specific: bool,
}

/// attributes of one item for rule evaluation
#[derive(Debug, Clone, Default)]
pub struct Item<'a> {
    pub net: &'a str,
    pub class: &'a str,
    pub typ: Option<&'a str>,    // "Track", "Via", "Pad", "Zone"
    pub parent: Option<&'a str>, // footprint reference
    pub pad_num: Option<&'a str>,
    pub pad_type: Option<&'a str>,
    /// references of the footprints whose courtyard the item intersects (None = unknown:
    /// `intersectsCourtyard` then leaves the rule undecided)
    pub courtyards: Option<&'a [String]>,
}

pub fn wild(v: &str, pat: &str) -> bool {
    // fnmatch-style: * ? [..] ; KiCad wildcard has * and ? only
    fn rec(v: &[u8], p: &[u8]) -> bool {
        if p.is_empty() {
            return v.is_empty();
        }
        match p[0] {
            b'*' => (0..=v.len()).any(|k| rec(&v[k..], &p[1..])),
            b'?' => !v.is_empty() && rec(&v[1..], &p[1..]),
            c => !v.is_empty() && v[0] == c && rec(&v[1..], &p[1..]),
        }
    }
    rec(v.as_bytes(), pat.as_bytes())
}

// ---------------------------------------------------------------------------------------------
// condition parser
struct Tok<'a> {
    s: &'a [u8],
    i: usize,
}

impl<'a> Tok<'a> {
    fn ws(&mut self) {
        while self.i < self.s.len() && self.s[self.i].is_ascii_whitespace() {
            self.i += 1;
        }
    }
    fn eat(&mut self, t: &str) -> bool {
        self.ws();
        if self.s[self.i..].starts_with(t.as_bytes()) {
            self.i += t.len();
            true
        } else {
            false
        }
    }
    fn ident(&mut self) -> String {
        self.ws();
        let st = self.i;
        while self.i < self.s.len()
            && (self.s[self.i].is_ascii_alphanumeric() || self.s[self.i] == b'_')
        {
            self.i += 1;
        }
        String::from_utf8_lossy(&self.s[st..self.i]).into()
    }
    fn string(&mut self) -> Result<String, String> {
        self.ws();
        if self.i >= self.s.len() || self.s[self.i] != b'\'' {
            return Err(format!("expected string at {}", self.i));
        }
        self.i += 1;
        let st = self.i;
        while self.i < self.s.len() && self.s[self.i] != b'\'' {
            self.i += 1;
        }
        let r = String::from_utf8_lossy(&self.s[st..self.i]).into();
        self.i += 1;
        Ok(r)
    }
    fn or(&mut self) -> Result<Expr, String> {
        let mut l = self.and()?;
        while self.eat("||") {
            let r = self.and()?;
            l = Expr::Or(Box::new(l), Box::new(r));
        }
        Ok(l)
    }
    fn and(&mut self) -> Result<Expr, String> {
        let mut l = self.unary()?;
        while self.eat("&&") {
            let r = self.unary()?;
            l = Expr::And(Box::new(l), Box::new(r));
        }
        Ok(l)
    }
    fn unary(&mut self) -> Result<Expr, String> {
        self.ws();
        if self.s[self.i..].starts_with(b"!") && !self.s[self.i..].starts_with(b"!=") {
            self.i += 1;
            return Ok(Expr::Not(Box::new(self.unary()?)));
        }
        if self.eat("(") {
            let e = self.or()?;
            if !self.eat(")") {
                return Err(format!("expected ) at {}", self.i));
            }
            return Ok(e);
        }
        let id = self.ident();
        if id == "true" {
            return Ok(Expr::True);
        }
        let side = match id.as_str() {
            "A" => 0u8,
            "B" => 1u8,
            _ => return Err(format!("unexpected '{id}' at {}", self.i)),
        };
        if !self.eat(".") {
            return Err("expected .".into());
        }
        let attr = self.ident();
        if self.eat("(") {
            let arg = self.string()?;
            self.eat(")");
            return Ok(Expr::Func {
                side,
                name: attr,
                arg,
            });
        }
        let eq = if self.eat("==") {
            true
        } else if self.eat("!=") {
            false
        } else {
            return Err(format!("expected comparison after {attr}"));
        };
        self.ws();
        let rhs = if self.i < self.s.len() && self.s[self.i] == b'\'' {
            Rhs::Str(self.string()?)
        } else {
            let id2 = self.ident();
            let s2 = if id2 == "A" { 0 } else { 1 };
            self.eat(".");
            Rhs::Attr(s2, self.ident())
        };
        Ok(Expr::Cmp {
            side,
            attr,
            eq,
            rhs,
        })
    }
}

const ITEM_ATTRS: [&str; 9] = [
    "Type",
    "Parent",
    "memberOfFootprint",
    "Pad_Type",
    "Pad_Number",
    "intersectsCourtyard",
    "Layer",
    "Area",
    "Courtyard",
];

fn get<'a>(it: &Item<'a>, attr: &str) -> Option<&'a str> {
    match attr {
        "NetName" => Some(it.net),
        "NetClass" => Some(it.class),
        "Type" => it.typ,
        "Parent" => it.parent,
        "Pad_Number" => it.pad_num,
        "Pad_Type" => it.pad_type,
        _ => None,
    }
}

/// three-valued evaluation: None = unknown (item attribute missing)
fn eval(e: &Expr, ab: [&Item; 2]) -> Option<bool> {
    match e {
        Expr::True => Some(true),
        Expr::Not(x) => eval(x, ab).map(|v| !v),
        Expr::And(l, r) => match (eval(l, ab), eval(r, ab)) {
            (Some(false), _) | (_, Some(false)) => Some(false),
            (Some(true), Some(true)) => Some(true),
            _ => None,
        },
        Expr::Or(l, r) => match (eval(l, ab), eval(r, ab)) {
            (Some(true), _) | (_, Some(true)) => Some(true),
            (Some(false), Some(false)) => Some(false),
            _ => None,
        },
        Expr::Func { side, name, arg } => {
            let it = ab[*side as usize];
            match name.as_str() {
                "hasNetclass" => Some(it.class == arg),
                "memberOfFootprint" => it.parent.map(|p| wild(p, arg)),
                // the side (front/back) of the courtyard is not distinguished
                "intersectsCourtyard" | "intersectsFrontCourtyard" | "intersectsBackCourtyard" => {
                    it.courtyards.map(|c| c.iter().any(|r| wild(r, arg)))
                }
                _ => None, // insideArea etc.: position dependent
            }
        }
        Expr::Cmp {
            side,
            attr,
            eq,
            rhs,
        } => {
            let v = get(ab[*side as usize], attr)?;
            let r = match rhs {
                Rhs::Str(s) => {
                    if attr == "NetName" || attr == "NetClass" {
                        wild(v, s)
                    } else {
                        v == s
                    }
                }
                Rhs::Attr(s2, a2) => v == get(ab[*s2 as usize], a2)?,
            };
            Some(r == *eq)
        }
    }
}

pub fn parse_dru(text: &str) -> Result<Vec<Rule>, String> {
    let mut rules = Vec::new();
    let re_name = Regex::new(r#"^"([^"]*)"|^(\S+)"#).unwrap();
    let re_layer = Regex::new(r"\(layer (\w[\w.]*)\)").unwrap();
    let re_cons =
        Regex::new(r"\(constraint (\w+)((?: \((?:min|opt|max) [\d.]+mm\))*)(?: (\w+))?\)").unwrap();
    let re_val = Regex::new(r"\((min|opt|max) ([\d.]+)mm\)").unwrap();
    let re_cond = Regex::new(r#"\(condition "(.*)"\)"#).unwrap();
    // strip comments
    let clean: String = text
        .lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .collect::<Vec<_>>()
        .join("\n");
    for blk in clean.split("\n(rule ").skip(1) {
        let nm = re_name.captures(blk).ok_or("rule name")?;
        let name = nm.get(1).or(nm.get(2)).unwrap().as_str().to_string();
        let layer = re_layer.captures(blk).map(|c| c[1].to_string());
        let mut cons = HashMap::new();
        let mut cons_flag = HashMap::new();
        for c in re_cons.captures_iter(blk) {
            let vals: HashMap<String, f64> = re_val
                .captures_iter(&c[2])
                .map(|v| (v[1].to_string(), v[2].parse().unwrap()))
                .collect();
            if vals.is_empty() {
                cons_flag.insert(
                    c[1].to_string(),
                    c.get(3).map(|m| m.as_str().to_string()).unwrap_or_default(),
                );
            } else {
                cons.insert(c[1].to_string(), vals);
            }
        }
        let cond = re_cond
            .captures(blk)
            .map(|c| c[1].to_string())
            .unwrap_or_else(|| "true".into());
        let mut t = Tok {
            s: cond.as_bytes(),
            i: 0,
        };
        let expr = t.or().map_err(|e| format!("rule '{name}': {e}"))?;
        // `A.Parent != B.Parent` alone is net-level conservative (different parts): keep the rule in the matrix
        let cond_n = cond
            .replace("A.Parent != B.Parent", "")
            .replace("A.Parent!=B.Parent", "");
        let item_specific = ITEM_ATTRS.iter().any(|a| cond_n.contains(a));
        rules.push(Rule {
            name,
            layer,
            cons,
            cons_flag,
            expr,
            item_specific,
        });
    }
    Ok(rules)
}

pub struct Rules {
    pub classes: Vec<NetClass>,
    pub class_idx: HashMap<String, usize>,
    pub min_clearance: f64,
    /// board setup minimum track width (hard floor, also for neck-down)
    pub min_track: f64,
    /// board setup via minimums: diameter, annular width, drill
    pub min_via: (f64, f64, f64),
    pub min_edge: f64,
    pub min_hole_to_hole: f64,
    pub min_hole_clearance: f64,
    pub dru: Vec<Rule>,
    /// net id -> class index
    pub net_class: Vec<usize>,
    pub nets: Vec<String>,
    /// clearance matrix [kind][a*n+b], kind 0 = outer (incl. creepage), 1 = inner
    pub mat: [Vec<f32>; 2],
}

fn layer_applies(r: &Rule, outer: bool) -> bool {
    match r.layer.as_deref() {
        None => true,
        Some("outer") => outer,
        Some("inner") => !outer,
        Some(l) => (l == "F.Cu" || l == "B.Cu") == outer,
    }
}

impl Rules {
    pub fn load(pro: &str, dru: &str, nets: &[String]) -> Result<Rules, String> {
        // empty `pro`: no project file, a single Default class with KiCad's default values
        let pj: Value = if pro.is_empty() {
            serde_json::json!({"net_settings": {"classes": [{"name": "Default"}]}})
        } else {
            serde_json::from_str(&std::fs::read_to_string(pro).map_err(|e| format!("{pro}: {e}"))?)
                .map_err(|e| e.to_string())?
        };
        let ns = &pj["net_settings"];
        let mut classes = Vec::new();
        for c in ns["classes"].as_array().ok_or("no classes")? {
            classes.push(NetClass {
                name: c["name"].as_str().unwrap_or("").into(),
                clearance: c["clearance"].as_f64().unwrap_or(0.2),
                track_width: c["track_width"].as_f64().unwrap_or(0.25),
                via_d: c["via_diameter"].as_f64().unwrap_or(0.6),
                via_drill: c["via_drill"].as_f64().unwrap_or(0.3),
            });
        }
        let class_idx: HashMap<String, usize> = classes
            .iter()
            .enumerate()
            .map(|(i, c)| (c.name.clone(), i))
            .collect();
        let def = *class_idx.get("Default").unwrap_or(&0);
        // pattern assignment: first match (wildcard or anchored regex)
        let mut pats: Vec<(Option<Regex>, String, usize)> = Vec::new();
        if let Some(a) = ns["netclass_patterns"].as_array() {
            for e in a {
                let pat = e["pattern"].as_str().unwrap_or("").to_string();
                let cls = *class_idx
                    .get(e["netclass"].as_str().unwrap_or(""))
                    .unwrap_or(&def);
                let re = Regex::new(&format!("^(?:{pat})$")).ok();
                pats.push((re, pat, cls));
            }
        }
        let explicit: HashMap<String, usize> = ns["netclass_assignments"]
            .as_object()
            .map(|o| {
                o.iter()
                    .filter_map(|(k, v)| {
                        let c = match v {
                            Value::String(s) => s.clone(),
                            Value::Array(a) => a.first()?.as_str()?.to_string(),
                            _ => return None,
                        };
                        Some((k.clone(), *class_idx.get(&c)?))
                    })
                    .collect()
            })
            .unwrap_or_default();
        let net_class: Vec<usize> = nets
            .iter()
            .map(|n| {
                if n.is_empty() {
                    return def;
                }
                if let Some(c) = explicit.get(n) {
                    return *c;
                }
                for (re, pat, c) in &pats {
                    if wild(n, pat) || re.as_ref().map(|r| r.is_match(n)).unwrap_or(false) {
                        return *c;
                    }
                }
                def
            })
            .collect();
        let rs = &pj["board"]["design_settings"]["rules"];
        let dru_rules = if dru.is_empty() {
            Vec::new()
        } else {
            parse_dru(&std::fs::read_to_string(dru).map_err(|e| format!("{dru}: {e}"))?)?
        };
        let mut r = Rules {
            classes,
            class_idx,
            // KiCad's default minimum clearance is 0: the net classes decide
            min_clearance: rs["min_clearance"].as_f64().unwrap_or(0.0),
            min_track: rs["min_track_width"].as_f64().unwrap_or(0.2),
            // KiCad's board setup defaults when the project has no value (kicad-cli 10 reports
            // 0.5 mm via diameter, 0.1 mm annular width, 0.3 mm hole)
            min_via: (
                rs["min_via_diameter"].as_f64().unwrap_or(0.5),
                rs["min_via_annular_width"].as_f64().unwrap_or(0.1),
                rs["min_through_hole_diameter"].as_f64().unwrap_or(0.3),
            ),
            min_edge: rs["min_copper_edge_clearance"].as_f64().unwrap_or(0.5),
            min_hole_to_hole: rs["min_hole_to_hole"].as_f64().unwrap_or(0.25),
            min_hole_clearance: rs["min_hole_clearance"].as_f64().unwrap_or(0.25),
            dru: dru_rules,
            net_class,
            nets: nets.to_vec(),
            mat: [Vec::new(), Vec::new()],
        };
        r.build_matrix();
        Ok(r)
    }

    pub fn class_of(&self, net: usize) -> &NetClass {
        &self.classes[self.net_class[net]]
    }
    pub fn item(&self, net: usize) -> Item<'_> {
        // parent = net name: items of different nets count as different parts (net-level conservative)
        Item {
            net: &self.nets[net],
            class: &self.classes[self.net_class[net]].name,
            parent: Some(&self.nets[net]),
            ..Default::default()
        }
    }

    fn matches(&self, r: &Rule, a: &Item, b: &Item) -> Option<bool> {
        match (eval(&r.expr, [a, b]), eval(&r.expr, [b, a])) {
            (Some(true), _) | (_, Some(true)) => Some(true),
            (Some(false), Some(false)) => Some(false),
            _ => None,
        }
    }

    /// copper clearance between two items (mm) on an outer or inner layer
    pub fn clearance_items(&self, a: &Item, b: &Item, outer: bool, item_rules: bool) -> f64 {
        let ca = self
            .class_idx
            .get(a.class)
            .map(|&i| self.classes[i].clearance)
            .unwrap_or(0.2);
        let cb = self
            .class_idx
            .get(b.class)
            .map(|&i| self.classes[i].clearance)
            .unwrap_or(0.2);
        let mut v = ca.max(cb).max(self.min_clearance);
        let mut cr: f64 = 0.0;
        for r in &self.dru {
            if (r.item_specific && !item_rules) || !layer_applies(r, outer) {
                continue;
            }
            let has_c = r.cons.get("clearance").and_then(|c| c.get("min"));
            let has_cr = r.cons.get("creepage").and_then(|c| c.get("min"));
            if has_c.is_none() && has_cr.is_none() {
                continue;
            }
            if self.matches(r, a, b) != Some(true) {
                continue;
            }
            if let Some(c) = has_c {
                v = *c;
            }
            if let Some(c) = has_cr {
                cr = *c;
            }
        }
        if outer {
            v = v.max(cr);
        }
        v
    }

    fn build_matrix(&mut self) {
        use rayon::prelude::*;
        let n = self.nets.len();
        for (k, outer) in [(0usize, true), (1usize, false)] {
            let m: Vec<f32> = (0..n * n)
                .into_par_iter()
                .map(|ij| {
                    let (i, j) = (ij / n, ij % n);
                    if i == j && i != 0 {
                        return 0.0;
                    }
                    if j < i {
                        return -1.0; // filled below
                    }
                    self.clearance_items(&self.item(i), &self.item(j), outer, false) as f32
                })
                .collect();
            let mut m = m;
            for i in 0..n {
                for j in 0..i {
                    m[i * n + j] = m[j * n + i];
                }
            }
            self.mat[k] = m;
        }
    }

    /// net-level clearance (mm) between nets a and b on a layer kind
    #[inline]
    pub fn clr(&self, a: usize, b: usize, outer: bool) -> f64 {
        let n = self.nets.len();
        self.mat[if outer { 0 } else { 1 }][a * n + b] as f64
    }

    pub fn edge_clearance(&self, net: usize) -> f64 {
        let a = self.item(net);
        let e = Item::default();
        let mut v = self.min_edge;
        for r in &self.dru {
            if let Some(c) = r.cons.get("edge_clearance").and_then(|c| c.get("min")) {
                if !r.item_specific && eval(&r.expr, [&a, &e]) == Some(true) {
                    v = *c;
                }
            }
        }
        v
    }

    /// (min, opt) track width: DRU track_width rule (Type == 'Track') if any, else the class width
    pub fn track_width(&self, net: usize) -> (f64, f64) {
        self.track_width_at(net, None)
    }

    /// [`Rules::track_width`] for a track inside the courtyards of the footprints `courtyards`
    /// (rules with `intersectsCourtyard` apply; None = position unknown, they are skipped)
    pub fn track_width_at(&self, net: usize, courtyards: Option<&[String]>) -> (f64, f64) {
        let mut a = self.item(net);
        a.typ = Some("Track");
        a.courtyards = courtyards;
        let e = Item::default();
        let c = self.class_of(net);
        // neck-down floor: 0.2 mm unless a DRU min says otherwise, never below the board minimum
        let (mut mn, mut opt) = (self.min_track.max(0.2), c.track_width);
        for r in &self.dru {
            if let Some(tw) = r.cons.get("track_width") {
                if eval(&r.expr, [&a, &e]) == Some(true) {
                    if let Some(m) = tw.get("min") {
                        mn = m.max(self.min_track);
                    }
                    opt = tw.get("opt").copied().unwrap_or(opt.max(mn));
                }
            }
        }
        (mn, opt.max(mn))
    }

    /// via diameter and drill of the net class, raised to the board setup minimums
    pub fn via(&self, net: usize) -> (f64, f64) {
        let c = self.class_of(net);
        let (md, ma, mh) = self.min_via;
        let drill = c.via_drill.max(mh);
        (c.via_d.max(md).max(drill + 2.0 * ma), drill)
    }

    pub fn max_length(&self, net: usize) -> Option<f64> {
        let a = self.item(net);
        let e = Item::default();
        for r in self.dru.iter().rev() {
            if let Some(l) = r.cons.get("length") {
                if eval(&r.expr, [&a, &e]) == Some(true) {
                    return l.get("max").copied();
                }
            }
        }
        None
    }

    /// physical/hole keepout around a footprint item (e.g. mounting holes) for a net (0 if no rule)
    pub fn hole_keepout(&self, parent: &str, net: usize) -> f64 {
        let fp_item = Item {
            net: "",
            class: "Default",
            parent: Some(parent),
            typ: Some("Pad"),
            ..Default::default()
        };
        let b = self.item(net);
        let mut v: f64 = 0.0;
        for r in &self.dru {
            for k in ["physical_clearance", "hole_clearance"] {
                if let Some(c) = r.cons.get(k).and_then(|c| c.get("min")) {
                    if self.matches(r, &fp_item, &b) == Some(true) {
                        v = v.max(*c);
                    }
                }
            }
        }
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DEMO: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/examples/demo/demo");

    #[test]
    fn without_project_file_default_class_and_board_min_width() {
        let nets: Vec<String> = ["", "A"].iter().map(|s| s.to_string()).collect();
        let r = Rules::load("", "", &nets).unwrap();
        assert_eq!(r.classes.len(), 1);
        assert_eq!(r.track_width(1), (0.2, 0.25));
        // board setup minimum is a hard floor for the neck-down width
        let mut r = r;
        r.min_track = 0.3;
        assert_eq!(r.track_width(1), (0.3, 0.3));
        // class via 0.6 / 0.3; a 0.2 mm annular minimum makes it 0.7 / 0.3
        assert_eq!(r.via(1), (0.6, 0.3));
        r.min_via = (0.0, 0.2, 0.0);
        assert!((r.via(1).0 - 0.7).abs() < 1e-9);
    }

    fn demo_rules() -> Rules {
        let nets: Vec<String> = ["", "GND", "+5V", "VIN", "SDA", "+3V3", "OUT_A"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        Rules::load(
            &format!("{DEMO}.kicad_pro"),
            &format!("{DEMO}.kicad_dru"),
            &nets,
        )
        .unwrap()
    }

    #[test]
    fn wildcards() {
        assert!(wild("GND", "GND"));
        assert!(wild("/ch1/OUT", "*/OUT"));
        assert!(wild("ADC2", "ADC?"));
        assert!(!wild("ADC12", "ADC?"));
        assert!(wild("anything", "*"));
    }

    #[test]
    fn netclass_patterns_from_project() {
        let r = demo_rules();
        let cls = |n: usize| r.class_of(n).name.clone();
        assert_eq!(cls(1), "Power"); // GND
        assert_eq!(cls(2), "Power"); // +5V
        assert_eq!(cls(3), "Power"); // VIN
        assert_eq!(cls(4), "Default"); // SDA
        assert_eq!(cls(5), "Default"); // +3V3: no pattern
        assert_eq!(r.class_of(1).track_width, 0.4);
        assert_eq!(r.via(1), (0.8, 0.4));
        assert_eq!(r.via(4), (0.6, 0.3));
    }

    #[test]
    fn dru_clearance_matrix_with_layer_restriction() {
        let r = demo_rules();
        // Power vs other: 0.3 mm on outer layers (DRU rule), class clearance 0.2 mm inside
        assert!((r.clr(1, 4, true) - 0.3).abs() < 1e-6);
        assert!((r.clr(1, 4, false) - 0.2).abs() < 1e-6);
        // Power vs Power: the rule does not match
        assert!((r.clr(1, 2, true) - 0.2).abs() < 1e-6);
        // Default vs Default
        assert!((r.clr(4, 6, true) - 0.2).abs() < 1e-6);
        // same net is never an obstacle
        assert_eq!(r.clr(4, 4, true), 0.0);
    }

    #[test]
    fn dru_track_width_and_hole_keepout() {
        let r = demo_rules();
        assert_eq!(r.track_width(1), (0.25, 0.4));
        assert_eq!(r.track_width(4), (0.2, 0.25));
        assert_eq!(r.hole_keepout("MH1", 4), 0.5);
        assert_eq!(r.hole_keepout("U1", 4), 0.0);
    }

    #[test]
    fn parse_dru_conditions_constraints_and_comments() {
        let txt = r#"(version 1)
# a comment (rule "ignored")
(rule "hv"
  (layer outer)
  (condition "A.NetClass == 'HV*' && B.NetClass != 'HV*'")
  (constraint clearance (min 1.5mm))
  (constraint creepage (min 2.5mm)))
(rule len
  (condition "A.NetName == '/clk*'")
  (constraint length (max 25mm)))
(rule "pad_only"
  (condition "A.Type == 'Pad' && A.memberOfFootprint('J*')")
  (constraint clearance (min 0.4mm)))
(rule "flag"
  (condition "A.hasNetclass('Power')")
  (constraint zone_connection solid))
"#;
        let rs = parse_dru(txt).unwrap();
        assert_eq!(rs.len(), 4);
        assert_eq!(rs[0].name, "hv");
        assert_eq!(rs[0].layer.as_deref(), Some("outer"));
        assert_eq!(rs[0].cons["clearance"]["min"], 1.5);
        assert_eq!(rs[0].cons["creepage"]["min"], 2.5);
        assert!(!rs[0].item_specific);
        assert_eq!(rs[1].name, "len");
        assert_eq!(rs[1].cons["length"]["max"], 25.0);
        assert!(rs[2].item_specific);
        assert_eq!(rs[3].cons_flag["zone_connection"], "solid");
        // conditions evaluate against items
        let hv = Item {
            net: "/HV+",
            class: "HV_DC",
            ..Default::default()
        };
        let lv = Item {
            net: "/x",
            class: "Default",
            ..Default::default()
        };
        assert_eq!(eval(&rs[0].expr, [&hv, &lv]), Some(true));
        assert_eq!(eval(&rs[0].expr, [&lv, &hv]), Some(false));
        // item attribute unknown at net level -> undecided
        assert_eq!(eval(&rs[2].expr, [&lv, &hv]), None);
    }

    #[test]
    fn courtyard_neck_rule_applies_inside_the_courtyard() {
        let dru = "(version 1)\n(rule \"neck\" (constraint track_width (min 0.15mm) (opt 0.15mm)) \
                   (condition \"A.Type == 'Track' && A.intersectsCourtyard('U*')\"))\n";
        let dir = std::env::temp_dir().join("copperpath_neck_rule");
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("n.kicad_dru");
        std::fs::write(&p, dru).unwrap();
        let mut r = Rules::load("", p.to_str().unwrap(), &["".into(), "GND".into()]).unwrap();
        r.min_track = 0.1;
        let (_, w) = r.track_width(1);
        assert!(w > 0.15, "outside: class width {w}");
        let u = ["U3".to_string()];
        assert_eq!(r.track_width_at(1, Some(&u)), (0.15, 0.15));
        let c = ["C1".to_string()];
        assert_eq!(r.track_width_at(1, Some(&c)).1, w);
    }

    #[test]
    fn bad_condition_is_reported() {
        let e = parse_dru("(version 1)\n(rule \"x\"\n (condition \"A.NetClass ~ 'a'\")\n (constraint clearance (min 1mm)))\n");
        assert!(e.is_err());
    }
}
