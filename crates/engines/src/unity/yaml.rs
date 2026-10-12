//! The subset of YAML that Unity writes: documents introduced by
//! `--- !u!<class> &<fileID>` (optionally `stripped`), block mappings and
//! sequences by indentation (with sequence items at their key's
//! indentation, as Unity puts them), flow mappings and sequences that may
//! wrap over lines, plain, single- and double-quoted scalars, and long
//! plain scalars continued on more-indented lines.
//!
//! Documents are split first and parsed only when asked for, because a
//! scene's bulk is settings and scripts' data that Slerp never reads.

use slerp_scene::LoadError;

/// Deeper nesting than this is an attack, not a scene.
const MAX_DEPTH: usize = 64;

/// A parsed value.  Scalars stay text; callers convert what they read.
#[derive(Clone, Debug, PartialEq, Default)]
pub(crate) enum Yaml {
    #[default]
    Null,
    Str(String),
    Map(Vec<(String, Yaml)>),
    Seq(Vec<Yaml>),
}

static NULL: Yaml = Yaml::Null;

impl Yaml {
    /// The value under `key` of a mapping, or Null.
    pub fn get(&self, key: &str) -> &Yaml {
        match self {
            Yaml::Map(pairs) => pairs.iter().find(|(k, _)| k == key).map_or(&NULL, |(_, v)| v),
            _ => &NULL,
        }
    }

    /// Follows a path of keys separated by dots.
    pub fn at(&self, path: &str) -> &Yaml {
        path.split('.').fold(self, |v, k| v.get(k))
    }

    pub fn str(&self) -> Option<&str> {
        match self {
            Yaml::Str(s) => Some(s),
            _ => None,
        }
    }

    pub fn f32(&self) -> Option<f32> {
        self.str()?.trim().parse::<f32>().ok().filter(|v| v.is_finite())
    }

    pub fn i64(&self) -> Option<i64> {
        let s = self.str()?.trim();
        s.parse::<i64>().ok().or_else(|| s.parse::<f64>().ok().filter(|f| f.fract() == 0.0).map(|f| f as i64))
    }

    pub fn seq(&self) -> &[Yaml] {
        match self {
            Yaml::Seq(items) => items,
            _ => &[],
        }
    }

    pub fn map(&self) -> &[(String, Yaml)] {
        match self {
            Yaml::Map(pairs) => pairs,
            _ => &[],
        }
    }

    pub fn is_null(&self) -> bool {
        matches!(self, Yaml::Null)
    }
}

/// One `--- !u!` document, not yet parsed.
pub(crate) struct RawDoc<'a> {
    pub class: i64,
    pub file_id: i64,
    pub stripped: bool,
    /// The type name line (`GameObject:`) and the indented body after it.
    pub text: &'a str,
}

/// Splits a Unity YAML file into its documents.
pub(crate) fn split(text: &str) -> Vec<RawDoc<'_>> {
    let mut docs = Vec::new();
    let mut header: Option<(i64, i64, bool, usize)> = None;
    let mut pos = 0usize;
    for line in text.split_inclusive('\n') {
        let start = pos;
        pos += line.len();
        let Some(rest) = line.strip_prefix("--- ") else { continue };
        if let Some((class, id, stripped, body)) = header.take() {
            docs.push(RawDoc { class, file_id: id, stripped, text: &text[body..start] });
        }
        // `!u!1 &12345` or `!u!4 &-678 stripped`.
        let mut parts = rest.split_whitespace();
        let class = parts.next().and_then(|t| t.strip_prefix("!u!")).and_then(|c| c.parse().ok()).unwrap_or(-1);
        let id = parts.next().and_then(|t| t.strip_prefix('&')).and_then(|c| c.parse().ok()).unwrap_or(0);
        let stripped = parts.any(|p| p == "stripped");
        header = Some((class, id, stripped, pos));
    }
    if let Some((class, id, stripped, body)) = header {
        docs.push(RawDoc { class, file_id: id, stripped, text: &text[body.min(text.len())..] });
    }
    docs
}

/// Parses a document's text: `TypeName:` and its body.  Returns the body.
pub(crate) fn parse_doc(text: &str) -> Result<Yaml, LoadError> {
    let root = parse(text)?;
    match root {
        Yaml::Map(mut pairs) if pairs.len() == 1 => Ok(pairs.pop().map(|(_, v)| v).unwrap_or_default()),
        other => Ok(other),
    }
}

/// Parses a YAML text whose top level is a mapping (a document body, or a
/// `.meta` file).
pub(crate) fn parse(text: &str) -> Result<Yaml, LoadError> {
    let lines: Vec<Line> = text
        .lines()
        .filter_map(|l| {
            let content = l.trim_start_matches(' ');
            let content = content.trim_end();
            if content.is_empty() || content.starts_with('#') || content.starts_with('%') || content.starts_with("---")
            {
                return None;
            }
            Some(Line { indent: l.len() - l.trim_start_matches(' ').len(), text: content })
        })
        .collect();
    let mut p = Parser { lines, at: 0 };
    let v = p.node(0, 0)?;
    Ok(v)
}

#[derive(Clone, Copy)]
struct Line<'a> {
    indent: usize,
    text: &'a str,
}

struct Parser<'a> {
    lines: Vec<Line<'a>>,
    at: usize,
}

fn is_seq_item(t: &str) -> bool {
    t == "-" || t.starts_with("- ")
}

/// Splits `key: value` (or `key:` alone) at the colon that ends the key.
fn split_key(t: &str) -> Option<(String, &str)> {
    if let Some(rest) = t.strip_prefix('"').or_else(|| t.strip_prefix('\'')) {
        let quote = t.as_bytes()[0] as char;
        let end = rest.find(quote)?;
        let after = rest[end + 1..].trim_start();
        let value = after.strip_prefix(':')?;
        return Some((rest[..end].to_string(), value.trim_start()));
    }
    // The first ": " or a trailing ":", outside brackets.
    let bytes = t.as_bytes();
    for (i, &c) in bytes.iter().enumerate() {
        if c == b'{' || c == b'[' {
            return None;
        }
        if c == b':' && (i + 1 == bytes.len() || bytes[i + 1] == b' ') {
            return Some((t[..i].trim_end().to_string(), t[i + 1..].trim_start()));
        }
    }
    None
}

impl<'a> Parser<'a> {
    fn peek(&self) -> Option<Line<'a>> {
        self.lines.get(self.at).copied()
    }

    /// A block node whose lines are indented at least `min`.
    fn node(&mut self, min: usize, depth: usize) -> Result<Yaml, LoadError> {
        if depth > MAX_DEPTH {
            return Err(LoadError::invalid("YAML nested too deeply"));
        }
        let Some(line) = self.peek() else { return Ok(Yaml::Null) };
        if line.indent < min {
            return Ok(Yaml::Null);
        }
        if is_seq_item(line.text) {
            self.seq(line.indent, depth)
        } else if split_key(line.text).is_some() {
            self.map(line.indent, depth)
        } else {
            // A lone scalar or flow value.
            self.at += 1;
            self.inline(line.text, line.indent, depth)
        }
    }

    fn map(&mut self, indent: usize, depth: usize) -> Result<Yaml, LoadError> {
        let mut pairs = Vec::new();
        while let Some(line) = self.peek() {
            if line.indent != indent || is_seq_item(line.text) {
                if line.indent > indent {
                    // Stray deeper text: skip it rather than fail.
                    self.at += 1;
                    continue;
                }
                break;
            }
            let Some((key, rest)) = split_key(line.text) else {
                self.at += 1;
                continue;
            };
            self.at += 1;
            let value = if rest.is_empty() {
                match self.peek() {
                    Some(next) if next.indent > indent => self.node(indent + 1, depth + 1)?,
                    // Unity puts a mapping's sequence at the key's own
                    // indentation.
                    Some(next) if next.indent == indent && is_seq_item(next.text) => self.seq(indent, depth + 1)?,
                    _ => Yaml::Null,
                }
            } else {
                self.inline(rest, indent, depth + 1)?
            };
            pairs.push((key, value));
        }
        Ok(Yaml::Map(pairs))
    }

    fn seq(&mut self, indent: usize, depth: usize) -> Result<Yaml, LoadError> {
        let mut items = Vec::new();
        while let Some(line) = self.peek() {
            if line.indent != indent || !is_seq_item(line.text) {
                break;
            }
            let rest = line.text[1..].trim_start();
            if rest.is_empty() {
                self.at += 1;
                items.push(self.node(indent + 1, depth + 1)?);
            } else if !rest.starts_with(['{', '[', '"', '\'']) && split_key(rest).is_some() {
                // `- key: value` starts a mapping whose keys line up two
                // columns in.
                let inner = indent + (line.text.len() - rest.len());
                self.lines[self.at] = Line { indent: inner, text: rest };
                items.push(self.map(inner, depth + 1)?);
            } else {
                self.at += 1;
                items.push(self.inline(rest, indent, depth + 1)?);
            }
        }
        Ok(Yaml::Seq(items))
    }

    /// A value written after its key on the same line, gathering the lines
    /// it continues on.
    fn inline(&mut self, first: &str, indent: usize, depth: usize) -> Result<Yaml, LoadError> {
        let opens_flow = first.starts_with('{') || first.starts_with('[');
        let quoted = first.starts_with('"') || first.starts_with('\'');
        let mut text = first.to_string();
        if opens_flow {
            while !balanced(&text) {
                let Some(next) = self.peek() else { break };
                self.at += 1;
                text.push(' ');
                text.push_str(next.text);
            }
            let mut f = Flow { s: text.as_bytes(), pos: 0 };
            return f.value(depth);
        }
        if quoted {
            while !closed_quote(&text) {
                let Some(next) = self.peek() else { break };
                self.at += 1;
                text.push(' ');
                text.push_str(next.text);
            }
            let mut f = Flow { s: text.as_bytes(), pos: 0 };
            return f.value(depth);
        }
        if text == "|" || text == ">" || text.starts_with("|-") || text.starts_with(">-") {
            let mut out = Vec::new();
            while let Some(next) = self.peek().filter(|l| l.indent > indent) {
                self.at += 1;
                out.push(next.text);
            }
            return Ok(Yaml::Str(out.join(if text.starts_with('|') { "\n" } else { " " })));
        }
        // A plain scalar, folded over more-indented lines.
        while let Some(next) = self.peek().filter(|l| l.indent > indent) {
            self.at += 1;
            text.push(' ');
            text.push_str(next.text);
        }
        Ok(Yaml::Str(text))
    }
}

/// True when every bracket opened outside quotes is closed.
fn balanced(t: &str) -> bool {
    let mut level = 0i32;
    let mut quote: Option<u8> = None;
    for &c in t.as_bytes() {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => {}
            None => match c {
                b'"' | b'\'' => quote = Some(c),
                b'{' | b'[' => level += 1,
                b'}' | b']' => level -= 1,
                _ => {}
            },
        }
    }
    level <= 0
}

fn closed_quote(t: &str) -> bool {
    let b = t.as_bytes();
    let q = b[0];
    let mut i = 1;
    while i < b.len() {
        if q == b'"' && b[i] == b'\\' {
            i += 2;
            continue;
        }
        if b[i] == q {
            if q == b'\'' && b.get(i + 1) == Some(&b'\'') {
                i += 2;
                continue;
            }
            return true;
        }
        i += 1;
    }
    false
}

/// Flow-style values: `{a: b, c: [d, e]}`, quoted strings, plain scalars.
struct Flow<'a> {
    s: &'a [u8],
    pos: usize,
}

impl Flow<'_> {
    fn ws(&mut self) {
        while self.s.get(self.pos).is_some_and(|c| c.is_ascii_whitespace()) {
            self.pos += 1;
        }
    }

    fn value(&mut self, depth: usize) -> Result<Yaml, LoadError> {
        if depth > MAX_DEPTH {
            return Err(LoadError::invalid("YAML nested too deeply"));
        }
        self.ws();
        match self.s.get(self.pos) {
            Some(b'{') => {
                self.pos += 1;
                let mut pairs = Vec::new();
                loop {
                    self.ws();
                    match self.s.get(self.pos) {
                        Some(b'}') => {
                            self.pos += 1;
                            return Ok(Yaml::Map(pairs));
                        }
                        Some(b',') => self.pos += 1,
                        None => return Ok(Yaml::Map(pairs)),
                        _ => {
                            let key = match self.value(depth + 1)? {
                                Yaml::Str(s) => s,
                                _ => String::new(),
                            };
                            self.ws();
                            let value = if self.s.get(self.pos) == Some(&b':') {
                                self.pos += 1;
                                self.value(depth + 1)?
                            } else {
                                Yaml::Null
                            };
                            pairs.push((key, value));
                        }
                    }
                }
            }
            Some(b'[') => {
                self.pos += 1;
                let mut items = Vec::new();
                loop {
                    self.ws();
                    match self.s.get(self.pos) {
                        Some(b']') => {
                            self.pos += 1;
                            return Ok(Yaml::Seq(items));
                        }
                        Some(b',') => self.pos += 1,
                        None => return Ok(Yaml::Seq(items)),
                        _ => items.push(self.value(depth + 1)?),
                    }
                }
            }
            Some(&q @ (b'"' | b'\'')) => {
                self.pos += 1;
                let mut out = Vec::new();
                while let Some(&c) = self.s.get(self.pos) {
                    self.pos += 1;
                    if c == q {
                        if q == b'\'' && self.s.get(self.pos) == Some(&b'\'') {
                            self.pos += 1;
                            out.push(b'\'');
                            continue;
                        }
                        break;
                    }
                    if q == b'"' && c == b'\\' {
                        let Some(&e) = self.s.get(self.pos) else { break };
                        self.pos += 1;
                        match e {
                            b'n' => out.push(b'\n'),
                            b't' => out.push(b'\t'),
                            b'u' => {
                                let hex = self.s.get(self.pos..self.pos + 4).unwrap_or(&[]);
                                self.pos += hex.len();
                                let ch = std::str::from_utf8(hex)
                                    .ok()
                                    .and_then(|h| u32::from_str_radix(h, 16).ok())
                                    .and_then(char::from_u32)
                                    .unwrap_or('\u{FFFD}');
                                out.extend_from_slice(ch.encode_utf8(&mut [0; 4]).as_bytes());
                            }
                            other => out.push(other),
                        }
                        continue;
                    }
                    out.push(c);
                }
                Ok(Yaml::Str(String::from_utf8_lossy(&out).into_owned()))
            }
            None => Ok(Yaml::Null),
            _ => {
                // A plain scalar inside a flow ends at , : } or ]; a colon
                // only ends it when a space or the end follows.
                let start = self.pos;
                while let Some(&c) = self.s.get(self.pos) {
                    let colon_end = c == b':' && self.s.get(self.pos + 1).is_none_or(|n| n.is_ascii_whitespace());
                    if matches!(c, b',' | b'}' | b']') || colon_end {
                        break;
                    }
                    self.pos += 1;
                }
                // A stray bracket where a value belongs (`{a: ]}`) is
                // skipped, so the enclosing loop always moves on.
                if self.pos == start {
                    self.pos += 1;
                    return Ok(Yaml::Null);
                }
                let text = String::from_utf8_lossy(&self.s[start..self.pos]).trim().to_string();
                Ok(if text.is_empty() { Yaml::Null } else { Yaml::Str(text) })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "%YAML 1.1\n%TAG !u! tag:unity3d.com,2011:\n--- !u!1 &100\nGameObject:\n  m_Name: Cube Thing\n  m_Component:\n  - component: {fileID: 4}\n  - component: {fileID: 33}\n  m_IsActive: 1\n--- !u!4 &4 stripped\nTransform:\n  m_LocalRotation: {x: 0, y: 0.7071068, z: 0, w: 0.7071068}\n  m_Children: []\n  m_Father: {fileID: 0}\n--- !u!1001 &5\nPrefabInstance:\n  m_Modification:\n    m_Modifications:\n    - target: {fileID: 224194346362733190, guid: 3ce107b4a79bc4eef83afde434932a68,\n        type: 3}\n      propertyPath: m_LocalPosition.x\n      value: 18\n      objectReference: {fileID: 0}\n    - target: {fileID: 1, guid: ab, type: 3}\n      propertyPath: m_Name\n      value: 'It''s a\n        long name'\n      objectReference: {fileID: 0}\n  m_SourcePrefab: {fileID: 100100000, guid: cfa81c, type: 3}\n";

    #[test]
    fn reads_unity_documents() {
        let docs = split(SAMPLE);
        assert_eq!(docs.len(), 3);
        assert_eq!((docs[0].class, docs[0].file_id, docs[1].stripped), (1, 100, true));
        let go = parse_doc(docs[0].text).unwrap();
        assert_eq!(go.get("m_Name").str(), Some("Cube Thing"));
        assert_eq!(go.get("m_Component").seq().len(), 2);
        assert_eq!(go.get("m_Component").seq()[1].at("component.fileID").i64(), Some(33));
        let t = parse_doc(docs[1].text).unwrap();
        assert_eq!(t.at("m_LocalRotation.y").str(), Some("0.7071068"));
        let p = parse_doc(docs[2].text).unwrap();
        let mods = p.at("m_Modification.m_Modifications").seq();
        assert_eq!(mods.len(), 2);
        assert_eq!(mods[0].at("target.type").str(), Some("3"));
        assert_eq!(mods[0].at("target.fileID").i64(), Some(224194346362733190));
        assert_eq!(mods[1].get("value").str(), Some("It's a long name"));
        assert_eq!(p.at("m_SourcePrefab.guid").str(), Some("cfa81c"));
    }

    #[test]
    fn broken_yaml_never_panics() {
        for cut in 0..SAMPLE.len() {
            if SAMPLE.is_char_boundary(cut) {
                for d in split(&SAMPLE[..cut]) {
                    let _ = parse_doc(d.text);
                }
            }
        }
        // Stray closing brackets inside flows once looped for ever.
        for stray in ["a: {x: ]}", "a: [1, }, 2]", "a: {: :}", "a: [:]"] {
            let _ = parse(stray);
        }
        let deep = format!("a: {}", "[".repeat(10_000));
        assert!(parse(&deep).is_err());
        let deep_block: String = (0..200).map(|i| format!("{}k{i}:\n", " ".repeat(i))).collect();
        let _ = parse(&deep_block);
    }
}
