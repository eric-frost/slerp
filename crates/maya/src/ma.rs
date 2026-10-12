//! Maya ASCII: a MEL script of a handful of commands, read tolerantly
//! without running anything.
//!
//! - `createNode type -n name -p parent` adds a node and makes it the one
//!   the following `setAttr`s set.
//! - `setAttr [flags] ".attr" [-type t] values` sets one value or a piece of
//!   an array; values can be numbers, booleans, strings, component lists and
//!   meshes' faces ("polyFaces").
//! - `select -ne name` picks an existing (usually default) node to set.
//! - `connectAttr "a.out" "b.in"`, `parent [-add] child parent`.
//! - `currentUnit`, `requires maya`, `fileInfo`, and `file -r` (references,
//!   noted but not loaded).
//!
//! Other commands are skipped, as are flags not listed here.  A statement
//! ends at a `;` outside quotes.

use crate::graph::{Face, Graph, Loop, Value, canon, parse_component};

/// Whether a file starts like Maya ASCII.
pub fn sniff(head: &[u8]) -> bool {
    let head = head.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(head);
    head.starts_with(b"//Maya ASCII")
}

/// A token of MEL text.
#[derive(Clone, Debug, PartialEq)]
enum Tok<'a> {
    Word(&'a str),
    Str(String),
}

struct Lexer<'a> {
    s: &'a str,
    b: &'a [u8],
    at: usize,
}

impl<'a> Lexer<'a> {
    fn new(s: &'a str) -> Self {
        Lexer { s, b: s.as_bytes(), at: 0 }
    }

    /// The next statement's tokens, or `None` at the end of the file.
    /// Parentheses and `+` between strings join them; braces and commas
    /// are dropped.
    fn statement(&mut self, out: &mut Vec<Tok<'a>>) -> bool {
        out.clear();
        let mut joining = false;
        loop {
            // 1. Whitespace and comments.
            while self.at < self.b.len() {
                let c = self.b[self.at];
                if c.is_ascii_whitespace() {
                    self.at += 1;
                } else if c == b'/' && self.b.get(self.at + 1) == Some(&b'/') {
                    while self.at < self.b.len() && self.b[self.at] != b'\n' {
                        self.at += 1;
                    }
                } else if c == b'/' && self.b.get(self.at + 1) == Some(&b'*') {
                    self.at += 2;
                    while self.at + 1 < self.b.len() && !(self.b[self.at] == b'*' && self.b[self.at + 1] == b'/') {
                        self.at += 1;
                    }
                    self.at += 2;
                } else {
                    break;
                }
            }
            if self.at >= self.b.len() {
                return !out.is_empty();
            }
            // 2. One token.
            let c = self.b[self.at];
            match c {
                b';' => {
                    self.at += 1;
                    return true;
                }
                b'"' => {
                    let s = self.string();
                    match (joining, out.last_mut()) {
                        (true, Some(Tok::Str(prev))) => prev.push_str(&s),
                        _ => out.push(Tok::Str(s)),
                    }
                    joining = false;
                }
                b'(' | b')' | b'{' | b'}' | b',' => self.at += 1,
                b'+' if self.b.get(self.at + 1).is_none_or(|c| c.is_ascii_whitespace() || *c == b'"') => {
                    self.at += 1;
                    joining = true;
                }
                _ => {
                    let start = self.at;
                    while self.at < self.b.len() {
                        let c = self.b[self.at];
                        if c.is_ascii_whitespace() || matches!(c, b';' | b'"' | b'(' | b')' | b'{' | b'}' | b',') {
                            break;
                        }
                        self.at += 1;
                    }
                    // The bytes between ASCII delimiters are whole UTF-8
                    // sequences, so this slice is always valid.
                    out.push(Tok::Word(&self.s[start..self.at]));
                    joining = false;
                }
            }
        }
    }

    /// A quoted string with MEL's escapes, starting at the opening quote.
    fn string(&mut self) -> String {
        self.at += 1;
        let mut out = Vec::new();
        while self.at < self.b.len() {
            let c = self.b[self.at];
            self.at += 1;
            match c {
                b'"' => break,
                b'\\' if self.at < self.b.len() => {
                    let e = self.b[self.at];
                    self.at += 1;
                    out.push(match e {
                        b'n' => b'\n',
                        b't' => b'\t',
                        b'r' => b'\r',
                        other => other,
                    });
                }
                _ => out.push(c),
            }
        }
        String::from_utf8_lossy(&out).into_owned()
    }
}

/// A MEL number or boolean.
fn number(w: &str) -> Option<f64> {
    match w {
        "yes" | "on" | "true" => Some(1.0),
        "no" | "off" | "false" => Some(0.0),
        _ => w.parse().ok(),
    }
}

fn is_flag(w: &str) -> bool {
    w.starts_with('-') && w.len() > 1 && !w.as_bytes()[1].is_ascii_digit() && w.as_bytes()[1] != b'.'
}

/// Reads a whole Maya ASCII file into a graph.
pub fn parse(text: &str) -> Graph {
    let mut g = Graph::new();
    let mut lexer = Lexer::new(text);
    let mut toks: Vec<Tok> = Vec::new();
    let mut current: Option<usize> = None;
    let mut statements = 0usize;
    while lexer.statement(&mut toks) {
        statements += 1;
        let Some(Tok::Word(cmd)) = toks.first() else { continue };
        let args = &toks[1..];
        match *cmd {
            "createNode" => current = create_node(&mut g, args),
            "setAttr" => set_attr(&mut g, current, args),
            "select" => {
                if let Some(name) = args.iter().rev().find_map(|t| match t {
                    Tok::Str(s) => Some(s.as_str()),
                    Tok::Word(w) if !is_flag(w) => Some(w),
                    _ => None,
                }) {
                    current = Some(g.find_or_add(name));
                }
            }
            "connectAttr" => {
                let plugs: Vec<&str> = strings(args);
                if let [src, dst, ..] = plugs[..] {
                    g.connect(src, dst);
                }
            }
            "parent" => parent(&mut g, args),
            "currentUnit" => current_unit(&mut g, args),
            "requires" => {
                if let [Tok::Word("maya"), Tok::Str(v), ..] = args {
                    g.version = v.clone();
                }
            }
            "fileInfo" => {
                if let [Tok::Str(k), Tok::Str(v), ..] = args {
                    g.file_info.push((k.clone(), v.clone()));
                }
            }
            "file" => {
                let reference =
                    args.iter().any(|t| matches!(t, Tok::Word(w) if matches!(*w, "-r" | "-rdi" | "-reference")));
                if reference
                    && let Some(path) = strings(args).last()
                    && !g.references.iter().any(|r| r == path)
                {
                    g.references.push(path.to_string());
                }
            }
            "upAxis" => {
                if args.iter().any(|t| matches!(t, Tok::Str(s) if s == "z") || *t == Tok::Word("z")) {
                    g.z_up = true;
                }
            }
            _ => {}
        }
    }
    if statements == 0 {
        g.warnings.push("no MEL statements found".into());
    }
    g
}

fn strings<'b>(args: &'b [Tok]) -> Vec<&'b str> {
    args.iter()
        .filter_map(|t| match t {
            Tok::Str(s) => Some(s.as_str()),
            _ => None,
        })
        .collect()
}

/// The value of flag `names` in `args`, a word or string.
fn flag_value<'b>(args: &'b [Tok], names: &[&str]) -> Option<&'b str> {
    let i = args.iter().position(|t| matches!(t, Tok::Word(w) if names.contains(w)))?;
    match args.get(i + 1)? {
        Tok::Str(s) => Some(s),
        Tok::Word(w) => Some(w),
    }
}

fn create_node(g: &mut Graph, args: &[Tok]) -> Option<usize> {
    let Some(Tok::Word(kind)) = args.first() else { return None };
    let name = flag_value(args, &["-n", "-name"]).unwrap_or(kind).to_string();
    let parent = flag_value(args, &["-p", "-parent"]).map(|p| g.find_or_add(p));
    Some(g.add(kind, &name, parent))
}

fn parent(g: &mut Graph, args: &[Tok]) {
    let names: Vec<&str> = strings(args);
    let add = args.iter().any(|t| matches!(t, Tok::Word(w) if matches!(*w, "-add" | "-a" | "-addObject")));
    let world = args.iter().any(|t| matches!(t, Tok::Word(w) if matches!(*w, "-w" | "-world")));
    match names[..] {
        [child] if world => {
            if let Some(c) = g.find(child) {
                g.nodes[c].parents.clear();
            }
        }
        [.., child, parent] => {
            let (Some(c), Some(p)) = (g.find(child), g.find(parent)) else { return };
            if c == p {
                return;
            }
            if add {
                if !g.nodes[c].parents.contains(&p) {
                    g.nodes[c].parents.push(p);
                }
            } else {
                g.nodes[c].parents = vec![p];
            }
        }
        _ => {}
    }
}

fn current_unit(g: &mut Graph, args: &[Tok]) {
    if let Some(l) = flag_value(args, &["-l", "-linear"]) {
        g.metres = match l {
            "mm" | "millimeter" => 0.001,
            "cm" | "centimeter" => 0.01,
            "m" | "meter" => 1.0,
            "km" | "kilometer" => 1000.0,
            "in" | "inch" => 0.0254,
            "ft" | "foot" => 0.3048,
            "yd" | "yard" => 0.9144,
            "mi" | "mile" => 1609.344,
            _ => g.metres,
        };
    }
    if let Some(a) = flag_value(args, &["-a", "-angle"]) {
        g.degrees = !matches!(a, "rad" | "radian");
    }
}

/// Flags of `setAttr` that take a value.
const SETATTR_VALUE_FLAGS: &[&str] = &[
    "-s",
    "-size",
    "-type",
    "-k",
    "-keyable",
    "-l",
    "-lock",
    "-cb",
    "-channelBox",
    "-ch",
    "-capacityHint",
    "-ca",
    "-caching",
];

fn set_attr(g: &mut Graph, current: Option<usize>, args: &[Tok]) {
    // 1. Flags, the attribute and the type.
    let mut i = 0;
    let mut attr: Option<&str> = None;
    let mut kind: Option<&str> = None;
    let mut values: Vec<&Tok> = Vec::new();
    while i < args.len() {
        match &args[i] {
            Tok::Word(w) if is_flag(w) => {
                if SETATTR_VALUE_FLAGS.contains(w) {
                    if matches!(*w, "-type") {
                        kind = match args.get(i + 1) {
                            Some(Tok::Str(s)) => Some(s.as_str()),
                            Some(Tok::Word(w)) => Some(w),
                            None => None,
                        };
                    }
                    i += 1;
                }
            }
            Tok::Str(s) if attr.is_none() => attr = Some(s),
            t => values.push(t),
        }
        i += 1;
    }
    let Some(attr) = attr else { return };
    // 2. The node: the current one for ".attr", or the one named.
    let (node, path) = if attr.starts_with('.') {
        let Some(n) = current else { return };
        (n, attr)
    } else {
        let start = attr.rfind('|').map_or(0, |i| i + 1);
        let Some(dot) = attr[start..].find('.').map(|d| d + start) else { return };
        (g.find_or_add(&attr[..dot]), &attr[dot..])
    };
    if values.is_empty() {
        return;
    }
    // Pieces of a long face list after the first leave the type out.
    if kind.is_none() && matches!(values.first(), Some(Tok::Word("f" | "h" | "mu" | "mc" | "fc"))) {
        kind = Some("polyFaces");
    }
    let value = match kind {
        Some("string") => match values.first() {
            Some(Tok::Str(s)) => Value::Text(s.clone()),
            _ => return,
        },
        Some("stringArray") => Value::Texts(
            values
                .iter()
                .filter_map(|t| match t {
                    Tok::Str(s) => Some(s.clone()),
                    _ => None,
                })
                .collect(),
        ),
        Some("componentList") => Value::Components(
            values.iter().filter_map(|t| if let Tok::Str(s) = t { parse_component(s) } else { None }).collect(),
        ),
        Some("polyFaces") => Value::Faces(poly_faces(&values)),
        Some(t) if t.ends_with("Array") => {
            // A count, then the values.
            let nums: Vec<f64> =
                values.iter().filter_map(|t| if let Tok::Word(w) = t { number(w) } else { None }).collect();
            Value::Nums(nums.get(1..).unwrap_or(&[]).to_vec())
        }
        Some(t)
            if t.starts_with("double")
                || t.starts_with("float")
                || t.starts_with("long")
                || t.starts_with("short")
                || t == "matrix"
                || t == "bool" =>
        {
            Value::Nums(values.iter().filter_map(|t| if let Tok::Word(w) = t { number(w) } else { None }).collect())
        }
        Some(_) => return,
        None => {
            let nums: Vec<f64> =
                values.iter().filter_map(|t| if let Tok::Word(w) = t { number(w) } else { None }).collect();
            if nums.is_empty() {
                match values.first() {
                    Some(Tok::Str(s)) => Value::Text(s.clone()),
                    _ => return,
                }
            } else {
                Value::Nums(nums)
            }
        }
    };
    let (key, range) = canon(path);
    g.nodes[node].attrs.push(crate::graph::Attr { key, range, value });
}

/// Maya ASCII's "polyFaces": `f n e...` starts a face, `h n e...` adds a
/// hole to it, `mu set n uv...` gives UVs to the last loop, `mc set n c...`
/// (and the older `fc n c...`) its colours.  Reading stops at anything
/// else, keeping the faces so far.
fn poly_faces(values: &[&Tok]) -> Vec<Face> {
    let words: Vec<&str> = values.iter().filter_map(|t| if let Tok::Word(w) = t { Some(*w) } else { None }).collect();
    let mut faces: Vec<Face> = Vec::new();
    let mut i = 0;
    let ints = |from: usize, n: usize| -> Option<Vec<i32>> {
        let slice = words.get(from..from.checked_add(n)?)?;
        slice.iter().map(|w| w.parse::<i32>().ok()).collect()
    };
    while i < words.len() {
        let tag = words[i];
        let read = |at: usize| words.get(at).and_then(|w| w.parse::<usize>().ok());
        match tag {
            "f" | "h" => {
                let Some(n) = read(i + 1) else { break };
                let Some(edges) = ints(i + 2, n) else { break };
                let lp = Loop { edges, ..Default::default() };
                if tag == "f" || faces.is_empty() {
                    faces.push(Face { loops: vec![lp] });
                } else if let Some(f) = faces.last_mut() {
                    f.loops.push(lp);
                }
                i += 2 + n;
            }
            "mu" | "mc" => {
                let (Some(set), Some(n)) = (read(i + 1), read(i + 2)) else { break };
                let Some(ids) = ints(i + 3, n) else { break };
                if let Some(lp) = faces.last_mut().and_then(|f| f.loops.last_mut()) {
                    if tag == "mu" {
                        lp.uvs.push((set as u32, ids));
                    } else {
                        lp.colors.push((set as u32, ids));
                    }
                }
                i += 3 + n;
            }
            "fc" => {
                let Some(n) = read(i + 1) else { break };
                let Some(ids) = ints(i + 2, n) else { break };
                if let Some(lp) = faces.last_mut().and_then(|f| f.loops.last_mut()) {
                    lp.colors.push((0, ids));
                }
                i += 2 + n;
            }
            _ => break,
        }
    }
    faces
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn statements_split_on_semicolons_outside_strings() {
        let mut l = Lexer::new("setAttr \".b\" -type \"string\" (\"a;\" + \"b\");\nconnectAttr \"x.o\" \"y.i\";");
        let mut t = Vec::new();
        assert!(l.statement(&mut t));
        assert_eq!(
            t,
            vec![
                Tok::Word("setAttr"),
                Tok::Str(".b".into()),
                Tok::Word("-type"),
                Tok::Str("string".into()),
                Tok::Str("a;b".into())
            ]
        );
        assert!(l.statement(&mut t));
        assert_eq!(t.len(), 3);
        assert!(!l.statement(&mut t));
    }

    #[test]
    fn faces_with_holes_and_uvs() {
        let words = [
            "f", "4", "0", "1", "2", "-4", "mu", "0", "4", "0", "1", "2", "3", "h", "3", "5", "6", "7", "mu", "0", "3",
            "4", "5", "6", "f", "3", "8", "9", "10",
        ];
        let toks: Vec<Tok> = words.iter().map(|w| Tok::Word(w)).collect();
        let refs: Vec<&Tok> = toks.iter().collect();
        let faces = poly_faces(&refs);
        assert_eq!(faces.len(), 2);
        assert_eq!(faces[0].loops.len(), 2);
        assert_eq!(faces[0].loops[0].edges, vec![0, 1, 2, -4]);
        assert_eq!(faces[0].loops[1].uvs, vec![(0, vec![4, 5, 6])]);
    }
}
