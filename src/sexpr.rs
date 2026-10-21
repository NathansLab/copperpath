//! Minimal, fast S-expression reader for KiCad files (.kicad_pcb, .kicad_sch, .kicad_dru-like).
//! Keeps byte spans of every list so writers can patch the original text in place.

#[derive(Debug, Clone)]
pub enum Node {
    Atom(String),
    List(List),
}

#[derive(Debug, Clone)]
pub struct List {
    pub items: Vec<Node>,
    /// byte range [start, end) of the list in the source text, including the parentheses
    pub span: (usize, usize),
}

impl Node {
    pub fn as_list(&self) -> Option<&List> {
        match self {
            Node::List(l) => Some(l),
            _ => None,
        }
    }
    pub fn atom(&self) -> Option<&str> {
        match self {
            Node::Atom(s) => Some(s),
            _ => None,
        }
    }
}

impl List {
    /// head symbol, e.g. "footprint" for (footprint ...)
    pub fn head(&self) -> &str {
        self.items.first().and_then(|n| n.atom()).unwrap_or("")
    }
    pub fn lists(&self) -> impl Iterator<Item = &List> {
        self.items.iter().filter_map(|n| n.as_list())
    }
    pub fn children<'a>(&'a self, head: &'a str) -> impl Iterator<Item = &'a List> + 'a {
        self.lists().filter(move |l| l.head() == head)
    }
    pub fn child(&self, head: &str) -> Option<&List> {
        self.lists().find(|l| l.head() == head)
    }
    /// i-th atom after the head (0-based)
    pub fn arg(&self, i: usize) -> Option<&str> {
        self.items.get(i + 1).and_then(|n| n.atom())
    }
    pub fn argf(&self, i: usize) -> Option<f64> {
        self.arg(i).and_then(|s| s.parse().ok())
    }
    /// value of (head value) child
    pub fn val(&self, head: &str) -> Option<&str> {
        self.child(head).and_then(|c| c.arg(0))
    }
    pub fn valf(&self, head: &str) -> Option<f64> {
        self.child(head).and_then(|c| c.argf(0))
    }
    /// (head x y [z]) -> (x, y, z or 0)
    pub fn xy(&self, head: &str) -> Option<(f64, f64, f64)> {
        let c = self.child(head)?;
        Some((c.argf(0)?, c.argf(1)?, c.argf(2).unwrap_or(0.0)))
    }
    /// atoms of (head a b c ...)
    pub fn atoms(&self, head: &str) -> Vec<String> {
        self.child(head)
            .map(|c| {
                c.items[1..]
                    .iter()
                    .filter_map(|n| n.atom().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default()
    }
    /// (head yes) / bare `head` atom flag
    pub fn flag(&self, head: &str) -> bool {
        if let Some(c) = self.child(head) {
            return c.arg(0).map(|v| v != "no").unwrap_or(true);
        }
        self.items.iter().any(|n| n.atom() == Some(head))
    }
}

pub fn parse(text: &str) -> Result<List, String> {
    let b = text.as_bytes();
    let mut i = 0usize;
    let mut stack: Vec<List> = Vec::new();
    let mut root: Option<List> = None;
    while i < b.len() {
        let c = b[i];
        match c {
            b'(' => {
                stack.push(List {
                    items: Vec::new(),
                    span: (i, 0),
                });
                i += 1;
            }
            b')' => {
                let mut l = stack
                    .pop()
                    .ok_or_else(|| format!("unbalanced ')' at {i}"))?;
                l.span.1 = i + 1;
                i += 1;
                match stack.last_mut() {
                    Some(p) => p.items.push(Node::List(l)),
                    None => {
                        root = Some(l);
                        break;
                    }
                }
            }
            b' ' | b'\t' | b'\n' | b'\r' => i += 1,
            b'"' => {
                i += 1;
                let mut s = String::new();
                let mut st = i;
                while i < b.len() && b[i] != b'"' {
                    if b[i] == b'\\' && i + 1 < b.len() {
                        s.push_str(&text[st..i]);
                        let e = b[i + 1];
                        s.push(match e {
                            b'n' => '\n',
                            b't' => '\t',
                            _ => e as char,
                        });
                        i += 2;
                        st = i;
                        continue;
                    }
                    i += 1;
                }
                s.push_str(&text[st..i]);
                i += 1;
                if let Some(p) = stack.last_mut() {
                    p.items.push(Node::Atom(s));
                }
            }
            _ => {
                let st = i;
                while i < b.len() && !matches!(b[i], b' ' | b'\t' | b'\n' | b'\r' | b'(' | b')') {
                    i += 1;
                }
                if let Some(p) = stack.last_mut() {
                    p.items.push(Node::Atom(text[st..i].to_string()));
                }
            }
        }
    }
    root.ok_or_else(|| "no root list".into())
}

/// quote a string for KiCad output
pub fn q(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_nested_lists_atoms_and_spans() {
        let src = r#"(kicad_pcb (version 20260206) (net "GND") (at 1.5 -2 90) (locked yes))"#;
        let root = parse(src).unwrap();
        assert_eq!(root.head(), "kicad_pcb");
        assert_eq!(root.val("version"), Some("20260206"));
        assert_eq!(root.val("net"), Some("GND"));
        assert_eq!(root.xy("at"), Some((1.5, -2.0, 90.0)));
        assert!(root.flag("locked"));
        assert!(!root.flag("hide"));
        let at = root.child("at").unwrap();
        assert_eq!(&src[at.span.0..at.span.1], "(at 1.5 -2 90)");
        assert_eq!(root.span, (0, src.len()));
    }

    #[test]
    fn quoted_strings_keep_spaces_parens_and_escapes() {
        let root = parse(r#"(a "x (y) z" "q\"uote" "tab\there" (b) c)"#).unwrap();
        assert_eq!(root.arg(0), Some("x (y) z"));
        assert_eq!(root.arg(1), Some("q\"uote"));
        assert_eq!(root.arg(2), Some("tab\there"));
        assert_eq!(root.lists().count(), 1);
        assert_eq!(root.atoms("b"), Vec::<String>::new());
        assert_eq!(q("q\"uote"), r#""q\"uote""#);
    }

    #[test]
    fn flag_without_value_and_with_no() {
        let root = parse("(pad (locked) (hide no) remove_unused_layers)").unwrap();
        assert!(root.flag("locked"));
        assert!(!root.flag("hide"));
        assert!(root.flag("remove_unused_layers"));
    }

    #[test]
    fn unbalanced_input_is_an_error() {
        assert!(parse("(a (b)").is_err());
        assert!(parse("a)").is_err());
    }
}
