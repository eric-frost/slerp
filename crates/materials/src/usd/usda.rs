//! USD's text format (`#usda 1.0`): prims, properties and the metadata
//! Slerp uses, read straight into a `Layer`.  Unknown metadata and
//! values are parsed and dropped, so new USD features don't break reading.

use super::stage::ns_path;
use super::{Layer, Reference, Samples, Specifier, TimeSampled, Value};
use slerp_scene::LoadError;
use std::sync::Arc;

/// Prims may nest this deep.
const MAX_DEPTH: u32 = 400;

/*┌─────────────────╮
| Lexer           |
└─────────────────╯*/
//region Lexer

#[derive(Clone, Debug, PartialEq)]
enum Tok<'a> {
    Ident(&'a str),
    Num(&'a str),
    Str(String),
    Asset(String),
    Path(String),
    Punct(u8),
    Eof,
}

struct Lexer<'a> {
    s: &'a [u8],
    pos: usize,
}

fn err(msg: impl Into<String>) -> LoadError {
    LoadError::invalid(format!("USD text: {}", msg.into()))
}

impl<'a> Lexer<'a> {
    fn skip_space(&mut self) {
        let s = self.s;
        loop {
            while self.pos < s.len() && s[self.pos].is_ascii_whitespace() {
                self.pos += 1;
            }
            match (s.get(self.pos), s.get(self.pos + 1)) {
                (Some(b'#'), _) | (Some(b'/'), Some(b'/')) => {
                    while self.pos < s.len() && s[self.pos] != b'\n' {
                        self.pos += 1;
                    }
                }
                (Some(b'/'), Some(b'*')) => {
                    self.pos += 2;
                    while self.pos < s.len() && !s[self.pos..].starts_with(b"*/") {
                        self.pos += 1;
                    }
                    self.pos = (self.pos + 2).min(s.len());
                }
                _ => return,
            }
        }
    }

    fn next(&mut self) -> Result<Tok<'a>, LoadError> {
        self.skip_space();
        let s = self.s;
        let Some(&c) = s.get(self.pos) else { return Ok(Tok::Eof) };
        let next = s.get(self.pos + 1).copied().unwrap_or(0);
        match c {
            b'"' | b'\'' => self.string(c),
            b'@' => self.asset(),
            b'<' => {
                let start = self.pos + 1;
                let end = s[start..].iter().position(|&b| b == b'>').ok_or_else(|| err("unterminated path"))? + start;
                self.pos = end + 1;
                Ok(Tok::Path(String::from_utf8_lossy(&s[start..end]).into_owned()))
            }
            b'0'..=b'9' => Ok(Tok::Num(self.number())),
            b'-' | b'+' | b'.' if next.is_ascii_digit() || (next == b'.' && c != b'.') => Ok(Tok::Num(self.number())),
            b'-' if s[self.pos + 1..].starts_with(b"inf") => {
                let start = self.pos;
                self.pos += 4;
                Ok(Tok::Num(std::str::from_utf8(&s[start..self.pos]).unwrap_or("0")))
            }
            b'(' | b')' | b'[' | b']' | b'{' | b'}' | b'=' | b',' | b':' | b'.' | b';' | b'&' => {
                self.pos += 1;
                Ok(Tok::Punct(c))
            }
            _ if c.is_ascii_alphabetic() || c == b'_' => {
                let start = self.pos;
                while self.pos < s.len() && (s[self.pos].is_ascii_alphanumeric() || matches!(s[self.pos], b'_' | b':'))
                {
                    self.pos += 1;
                }
                Ok(Tok::Ident(std::str::from_utf8(&s[start..self.pos]).unwrap_or("")))
            }
            _ => Err(err(format!("unexpected byte {c:#x}"))),
        }
    }

    fn number(&mut self) -> &'a str {
        let start = self.pos;
        self.pos += 1;
        while self.pos < self.s.len() && matches!(self.s[self.pos], b'0'..=b'9' | b'.' | b'e' | b'E' | b'-' | b'+') {
            self.pos += 1;
        }
        std::str::from_utf8(&self.s[start..self.pos]).unwrap_or("0")
    }

    fn string(&mut self, quote: u8) -> Result<Tok<'a>, LoadError> {
        let s = self.s;
        let triple = s.get(self.pos + 1) == Some(&quote) && s.get(self.pos + 2) == Some(&quote);
        self.pos += if triple { 3 } else { 1 };
        let mut out = Vec::new();
        loop {
            let Some(&c) = s.get(self.pos) else { return Err(err("unterminated string")) };
            if c == quote && (!triple || (s.get(self.pos + 1) == Some(&quote) && s.get(self.pos + 2) == Some(&quote))) {
                self.pos += if triple { 3 } else { 1 };
                return Ok(Tok::Str(String::from_utf8_lossy(&out).into_owned()));
            }
            if c == b'\n' && !triple {
                return Err(err("newline in string"));
            }
            self.pos += 1;
            if c == b'\\' {
                let Some(&e) = s.get(self.pos) else { return Err(err("unterminated string")) };
                self.pos += 1;
                out.push(match e {
                    b'n' => b'\n',
                    b't' => b'\t',
                    b'r' => b'\r',
                    b'0' => 0,
                    other => other,
                });
            } else {
                out.push(c);
            }
        }
    }

    fn asset(&mut self) -> Result<Tok<'a>, LoadError> {
        let s = self.s;
        if s[self.pos..].starts_with(b"@@@") {
            self.pos += 3;
            let start = self.pos;
            loop {
                if self.pos >= s.len() {
                    return Err(err("unterminated asset path"));
                }
                if s[self.pos..].starts_with(b"@@@") && s[self.pos - 1] != b'\\' {
                    let text = String::from_utf8_lossy(&s[start..self.pos]).replace("\\@@@", "@@@");
                    self.pos += 3;
                    return Ok(Tok::Asset(text));
                }
                self.pos += 1;
            }
        }
        let start = self.pos + 1;
        let end = s[start..].iter().position(|&b| b == b'@').ok_or_else(|| err("unterminated asset path"))? + start;
        self.pos = end + 1;
        Ok(Tok::Asset(String::from_utf8_lossy(&s[start..end]).into_owned()))
    }
}

//endregion

/*┌─────────────────╮
| Parser          |
└─────────────────╯*/
//region Parser

/// What kind of value a property's type calls for.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Hint {
    Int,
    Float,
    Str,
}

fn hint(type_name: &str) -> Hint {
    let base = type_name.trim_end_matches("[]");
    match base {
        "int" | "uint" | "int64" | "uint64" | "uchar" | "bool" | "int2" | "int3" | "int4" => Hint::Int,
        "token" | "string" | "asset" => Hint::Str,
        _ => Hint::Float,
    }
}

struct Parser<'a> {
    lex: Lexer<'a>,
    peeked: Option<Tok<'a>>,
    /// Keep every time sample of the properties `keeps_samples` names.
    samples: bool,
}

impl<'a> Parser<'a> {
    fn peek(&mut self) -> Result<&Tok<'a>, LoadError> {
        if self.peeked.is_none() {
            self.peeked = Some(self.lex.next()?);
        }
        Ok(self.peeked.as_ref().unwrap_or(&Tok::Eof))
    }

    fn next(&mut self) -> Result<Tok<'a>, LoadError> {
        match self.peeked.take() {
            Some(t) => Ok(t),
            None => self.lex.next(),
        }
    }

    fn is(&mut self, p: u8) -> Result<bool, LoadError> {
        Ok(self.peek()? == &Tok::Punct(p))
    }

    fn eat(&mut self, p: u8) -> Result<bool, LoadError> {
        let yes = self.is(p)?;
        if yes {
            self.next()?;
        }
        Ok(yes)
    }

    fn expect(&mut self, p: u8) -> Result<(), LoadError> {
        match self.next()? {
            Tok::Punct(c) if c == p => Ok(()),
            t => Err(err(format!("expected '{}', found {t:?}", p as char))),
        }
    }

    fn ident(&mut self) -> Result<&'a str, LoadError> {
        match self.next()? {
            Tok::Ident(s) => Ok(s),
            t => Err(err(format!("expected a name, found {t:?}"))),
        }
    }

    fn string(&mut self) -> Result<String, LoadError> {
        match self.next()? {
            Tok::Str(s) => Ok(s),
            t => Err(err(format!("expected a string, found {t:?}"))),
        }
    }

    /// Skips one value of any shape.
    fn skip_value(&mut self) -> Result<(), LoadError> {
        let mut depth = 0usize;
        loop {
            match self.next()? {
                Tok::Punct(b'(' | b'[' | b'{') => depth += 1,
                Tok::Punct(b')' | b']' | b'}') => {
                    depth = depth.checked_sub(1).ok_or_else(|| err("unbalanced brackets"))?;
                }
                Tok::Eof => return Err(err("unexpected end")),
                _ => {}
            }
            if depth == 0 {
                // A reference may carry a layer offset in parentheses.
                return Ok(());
            }
        }
    }

    fn value(&mut self, hint: Hint, depth: u32) -> Result<Value, LoadError> {
        if depth > 64 {
            return Err(err("values nested too deeply"));
        }
        match self.next()? {
            Tok::Num(s) => Ok(number(s, hint)),
            Tok::Str(s) => Ok(Value::Str(s)),
            Tok::Asset(s) => Ok(Value::Asset(s)),
            Tok::Path(s) => Ok(Value::Path(s)),
            Tok::Ident(id) => Ok(match id {
                "true" => Value::Bool(true),
                "false" => Value::Bool(false),
                "None" => Value::None,
                "inf" | "nan" => Value::Float(id.parse().unwrap_or(0.0)),
                _ => Value::Str(id.to_string()),
            }),
            Tok::Punct(b'(') => {
                let mut nums = Vec::new();
                self.tuple_into(&mut nums, 0)?;
                Ok(Value::Tuple(nums))
            }
            Tok::Punct(b'[') => self.list(hint, depth),
            Tok::Punct(b'{') => self.dict(depth),
            t => Err(err(format!("unexpected {t:?} where a value belongs"))),
        }
    }

    /// Reads a parenthesised tuple (after its `(`), flattening nested
    /// tuples such as matrix rows.
    fn tuple_into<T: FromF64>(&mut self, out: &mut Vec<T>, depth: u32) -> Result<(), LoadError> {
        if depth > 4 {
            return Err(err("tuple nested too deeply"));
        }
        loop {
            match self.next()? {
                Tok::Punct(b')') => return Ok(()),
                Tok::Punct(b',') => {}
                Tok::Punct(b'(') => self.tuple_into(out, depth + 1)?,
                Tok::Num(s) => out.push(T::from_f64(s.parse::<f64>().unwrap_or(0.0))),
                Tok::Ident(id) => out.push(T::from_f64(id.parse::<f64>().unwrap_or(0.0))),
                Tok::Str(_) | Tok::Asset(_) | Tok::Path(_) => {}
                t => return Err(err(format!("unexpected {t:?} in tuple"))),
            }
        }
    }

    /// Reads a list after its `[`.
    fn list(&mut self, hint: Hint, depth: u32) -> Result<Value, LoadError> {
        let first = self.peek()?.clone();
        match first {
            Tok::Punct(b']') => {
                self.next()?;
                Ok(match hint {
                    Hint::Int => Value::Ints(Vec::new()),
                    Hint::Str => Value::Strs(Vec::new()),
                    Hint::Float => Value::Floats(Vec::new(), 1),
                })
            }
            Tok::Punct(b'(') => {
                let mut flat: Vec<f32> = Vec::new();
                let mut width = 0;
                loop {
                    match self.next()? {
                        Tok::Punct(b']') => break,
                        Tok::Punct(b',') => {}
                        Tok::Punct(b'(') => {
                            let before = flat.len();
                            self.tuple_into(&mut flat, 0)?;
                            if width == 0 {
                                width = flat.len() - before;
                            }
                        }
                        t => return Err(err(format!("unexpected {t:?} in a list of tuples"))),
                    }
                }
                let width = width.max(1);
                flat.truncate(flat.len() / width * width);
                Ok(Value::Floats(flat, width))
            }
            Tok::Num(_) | Tok::Ident("inf" | "nan") => {
                if hint == Hint::Int {
                    let mut out = Vec::new();
                    loop {
                        match self.next()? {
                            Tok::Punct(b']') => return Ok(Value::Ints(out)),
                            Tok::Punct(b',') => {}
                            Tok::Num(s) => out.push(s.parse::<i64>().map(|v| v as i32).unwrap_or(-1)),
                            Tok::Ident(b) => out.push((b == "true") as i32),
                            t => return Err(err(format!("unexpected {t:?} in a list of numbers"))),
                        }
                    }
                }
                let mut out = Vec::new();
                loop {
                    match self.next()? {
                        Tok::Punct(b']') => return Ok(Value::Floats(out, 1)),
                        Tok::Punct(b',') => {}
                        Tok::Num(s) | Tok::Ident(s) => out.push(s.parse::<f32>().unwrap_or(0.0)),
                        t => return Err(err(format!("unexpected {t:?} in a list of numbers"))),
                    }
                }
            }
            Tok::Ident("true" | "false") if hint == Hint::Int => {
                let mut out = Vec::new();
                loop {
                    match self.next()? {
                        Tok::Punct(b']') => return Ok(Value::Ints(out)),
                        Tok::Punct(b',') => {}
                        Tok::Ident(b) => out.push((b == "true") as i32),
                        t => return Err(err(format!("unexpected {t:?} in a list of booleans"))),
                    }
                }
            }
            Tok::Path(_) => {
                let mut out = Vec::new();
                loop {
                    match self.next()? {
                        Tok::Punct(b']') => return Ok(Value::Paths(out)),
                        Tok::Punct(b',') => {}
                        Tok::Path(p) => out.push(p),
                        t => return Err(err(format!("unexpected {t:?} in a list of paths"))),
                    }
                }
            }
            _ => {
                let mut out = Vec::new();
                loop {
                    if self.eat(b']')? {
                        return Ok(Value::Strs(out));
                    }
                    if self.eat(b',')? {
                        continue;
                    }
                    match self.value(Hint::Str, depth + 1)? {
                        Value::Str(s) | Value::Asset(s) | Value::Path(s) => out.push(s),
                        _ => {}
                    }
                    // A sublayer or reference may carry a layer offset.
                    if self.is(b'(')? {
                        self.skip_value()?;
                    }
                }
            }
        }
    }

    /// Reads a dictionary after its `{`: `type name = value` entries.
    fn dict(&mut self, depth: u32) -> Result<Value, LoadError> {
        let mut out = Vec::new();
        loop {
            if self.eat(b'}')? {
                return Ok(Value::Dict(out));
            }
            if self.eat(b';')? || self.eat(b',')? {
                continue;
            }
            let ty = self.ident()?;
            if self.eat(b'[')? {
                self.expect(b']')?;
            }
            let key = match self.next()? {
                Tok::Ident(s) => s.to_string(),
                Tok::Str(s) => s,
                t => return Err(err(format!("unexpected {t:?} as a dictionary key"))),
            };
            self.expect(b'=')?;
            let v = if ty == "dictionary" {
                self.expect(b'{')?;
                self.dict(depth + 1)?
            } else {
                self.value(hint(ty), depth + 1)?
            };
            out.push((key, v));
        }
    }

    /// References and payloads: a single item, a list, or `None`.
    fn refs(&mut self) -> Result<Vec<Reference>, LoadError> {
        let mut out = Vec::new();
        let list = self.eat(b'[')?;
        loop {
            if list && self.eat(b']')? {
                return Ok(out);
            }
            if list && self.eat(b',')? {
                continue;
            }
            match self.next()? {
                Tok::Asset(asset) => {
                    let prim = match self.peek()? {
                        Tok::Path(_) => match self.next()? {
                            Tok::Path(p) => p,
                            _ => String::new(),
                        },
                        _ => String::new(),
                    };
                    out.push(Reference { asset, prim });
                }
                Tok::Path(prim) => out.push(Reference { asset: String::new(), prim }),
                Tok::Ident("None") => {}
                t => return Err(err(format!("unexpected {t:?} in references"))),
            }
            if self.is(b'(')? {
                self.skip_value()?;
            }
            if !list {
                return Ok(out);
            }
        }
    }

    fn paths(&mut self) -> Result<Vec<String>, LoadError> {
        Ok(match self.value(Hint::Str, 0)? {
            Value::Path(p) => vec![p],
            Value::Paths(p) => p,
            _ => Vec::new(),
        })
    }
}

trait FromF64 {
    fn from_f64(v: f64) -> Self;
}

impl FromF64 for f32 {
    fn from_f64(v: f64) -> Self {
        v as f32
    }
}

impl FromF64 for f64 {
    fn from_f64(v: f64) -> Self {
        v
    }
}

/// Numbers per element of an array type: `float2[]` 2, `matrix4d[]` 16.
fn type_width(type_name: &str) -> usize {
    let base = type_name.trim_end_matches("[]");
    if base.starts_with("matrix") {
        let n = base[6..7.min(base.len())].parse::<usize>().unwrap_or(4);
        return n * n;
    }
    if base.starts_with("quat") {
        return 4;
    }
    base.chars().rfind(char::is_ascii_digit).and_then(|c| c.to_digit(10)).map_or(1, |d| d as usize)
}

/// Fixes up a value read for a property of `type_name`: an empty array
/// still has its element width, and half-precision types (`half3`,
/// `quath`, `texCoord2h`) keep only what a half holds, as USD stores them.
fn fix_width(v: &mut Value, type_name: &str) {
    let base = type_name.trim_end_matches("[]");
    let half = base.starts_with("half") || (base.ends_with('h') && base.contains(|c: char| c.is_ascii_digit()));
    let half = half || base == "quath";
    match v {
        Value::Floats(f, w) => {
            if f.is_empty() {
                *w = type_width(type_name);
            }
            if half {
                f.iter_mut().for_each(|x| *x = round_half(*x));
            }
        }
        Value::Tuple(t) if half => t.iter_mut().for_each(|x| *x = round_half(*x as f32) as f64),
        Value::Float(x) if half => *x = round_half(*x as f32) as f64,
        _ => {}
    }
}

/// The nearest half-precision number (ties to even), as a float.
fn round_half(x: f32) -> f32 {
    let a = x.abs();
    if !x.is_finite() || a == 0.0 {
        return x;
    }
    if a >= 65520.0 {
        return f32::INFINITY.copysign(x);
    }
    // Halves have 10 fraction bits; below 2^-14 they are evenly spaced.
    let step = 2f32.powi(a.log2().floor().max(-14.0) as i32 - 10);
    (x / step).round_ties_even() * step
}

fn number(s: &str, hint: Hint) -> Value {
    if hint == Hint::Int
        && let Ok(i) = s.parse::<i64>()
    {
        return Value::Int(i);
    }
    Value::Float(s.parse().unwrap_or(0.0))
}

//endregion

/*┌─────────────────╮
| Layer           |
└─────────────────╯*/
//region Layer

pub(crate) fn parse(bytes: &[u8], samples: bool) -> Result<Layer, LoadError> {
    let header_end = bytes.iter().position(|&b| b == b'\n').unwrap_or(bytes.len()).min(64);
    let version = String::from_utf8_lossy(&bytes[..header_end]).trim_start_matches("#usda").trim().to_string();
    let mut layer = Layer { format: format!("USD text {version}"), ..Default::default() };
    layer.prims.insert("/".into(), Default::default());
    let mut p = Parser { lex: Lexer { s: bytes, pos: 0 }, peeked: None, samples };
    if p.eat(b'(')? {
        p.metadata(&mut layer, Target::Layer)?;
    }
    loop {
        match p.peek()? {
            Tok::Eof => return Ok(layer),
            Tok::Ident("def" | "over" | "class") => p.prim(&mut layer, "/", 0)?,
            t => return Err(err(format!("unexpected {t:?} at the top level"))),
        }
    }
}

/// What a metadata block belongs to.
#[derive(Clone)]
enum Target {
    Layer,
    Prim(String),
    Prop(String, String),
}

/// A target path written relative to the prim that owns the property, such
/// as `<Animation>` or `<../Mesh.points>`, made absolute.  `prim` is a spec
/// path, which may hold variant selections; targets point into namespace.
fn absolute(prim: &str, target: &str) -> String {
    if target.is_empty() || target.starts_with('/') {
        return target.to_string();
    }
    let ns = ns_path(prim);
    let mut parts: Vec<&str> = ns.split('/').filter(|p| !p.is_empty()).collect();
    let mut rest = target;
    loop {
        if let Some(r) = rest.strip_prefix("../") {
            parts.pop();
            rest = r;
        } else if rest == ".." {
            parts.pop();
            rest = "";
        } else if let Some(r) = rest.strip_prefix("./") {
            rest = r;
        } else if rest == "." {
            rest = "";
        } else {
            break;
        }
    }
    let base = format!("/{}", parts.join("/"));
    if rest.is_empty() {
        base
    } else if rest.starts_with('.') {
        format!("{base}{rest}")
    } else if base == "/" {
        format!("/{rest}")
    } else {
        format!("{base}/{rest}")
    }
}

/// A child's spec path: variant children follow their `{set=sel}` directly.
pub(crate) fn child_path(parent: &str, name: &str) -> String {
    if parent == "/" {
        format!("/{name}")
    } else if parent.ends_with('}') {
        format!("{parent}{name}")
    } else {
        format!("{parent}/{name}")
    }
}

impl Parser<'_> {
    fn prim(&mut self, layer: &mut Layer, parent: &str, depth: u32) -> Result<(), LoadError> {
        if depth > MAX_DEPTH {
            return Err(err("prims nested too deeply"));
        }
        let specifier = match self.ident()? {
            "def" => Specifier::Def,
            "class" => Specifier::Class,
            _ => Specifier::Over,
        };
        let type_name = match self.peek()? {
            Tok::Ident(t) => {
                let t = t.to_string();
                self.next()?;
                t
            }
            _ => String::new(),
        };
        let name = self.string()?;
        if name.is_empty() || name.contains(['/', '.', '{', '}']) {
            return Err(err(format!("bad prim name {name:?}")));
        }
        let path = child_path(parent, &name);
        let siblings = &mut layer.prim_mut(parent).children;
        if !siblings.contains(&name) {
            siblings.push(name);
        }
        let spec = layer.prim_mut(&path);
        spec.specifier = specifier;
        spec.type_name = type_name;
        if self.eat(b'(')? {
            self.metadata(layer, Target::Prim(path.clone()))?;
        }
        self.expect(b'{')?;
        self.body(layer, &path, depth)
    }

    fn body(&mut self, layer: &mut Layer, path: &str, depth: u32) -> Result<(), LoadError> {
        loop {
            match self.peek()? {
                Tok::Punct(b'}') => {
                    self.next()?;
                    return Ok(());
                }
                Tok::Punct(b';') => {
                    self.next()?;
                }
                Tok::Ident("def" | "over" | "class") => self.prim(layer, path, depth + 1)?,
                Tok::Ident("variantSet") => {
                    self.next()?;
                    let set = self.string()?;
                    self.expect(b'=')?;
                    self.expect(b'{')?;
                    let sets = &mut layer.prim_mut(path).variant_sets;
                    if !sets.contains(&set) {
                        sets.push(set.clone());
                    }
                    while !self.eat(b'}')? {
                        let variant = self.string()?;
                        let vpath = format!("{path}{{{set}={variant}}}");
                        layer.prim_mut(&vpath);
                        if self.eat(b'(')? {
                            self.metadata(layer, Target::Prim(vpath.clone()))?;
                        }
                        self.expect(b'{')?;
                        self.body(layer, &vpath, depth + 1)?;
                    }
                }
                Tok::Ident("reorder") => {
                    self.next()?;
                    self.ident()?;
                    self.expect(b'=')?;
                    self.skip_value()?;
                }
                Tok::Eof => return Err(err("unexpected end inside a prim")),
                _ => self.property(layer, path)?,
            }
        }
    }

    fn property(&mut self, layer: &mut Layer, prim: &str) -> Result<(), LoadError> {
        let mut word = self.ident()?;
        let delete = word == "delete";
        if matches!(word, "prepend" | "append" | "add" | "delete") {
            word = self.ident()?;
        }
        if word == "custom" {
            word = self.ident()?;
        }
        if matches!(word, "uniform" | "varying" | "config") {
            word = self.ident()?;
        }
        if word == "rel" {
            let name = self.ident()?.to_string();
            let targets = if self.eat(b'=')? { self.paths()? } else { Vec::new() };
            let targets: Vec<String> = targets.iter().map(|t| absolute(prim, t)).collect();
            if self.eat(b'(')? {
                self.metadata(layer, Target::Prop(prim.to_string(), name.clone()))?;
            }
            if !delete {
                let prop = layer.prim_mut(prim).prop_mut(&name);
                if !targets.is_empty() {
                    prop.targets = targets;
                }
            }
            return Ok(());
        }

        let mut type_name = word.to_string();
        if self.eat(b'[')? {
            self.expect(b']')?;
            type_name.push_str("[]");
        }
        let name = self.ident()?.to_string();
        let suffix = if self.eat(b'.')? { self.ident()? } else { "" };
        let h = hint(&type_name);
        let mut value = None;
        let mut sample = None;
        let mut samples = None;
        let mut targets = Vec::new();
        if self.eat(b'=')? {
            match suffix {
                "connect" => targets = self.paths()?.iter().map(|t| absolute(prim, t)).collect(),
                "timeSamples" => {
                    let all = self.samples && super::keeps_samples(&name);
                    (sample, samples) = self.time_samples(h, all, &type_name)?;
                }
                "spline" => self.skip_value()?,
                _ => value = Some(self.value(h, 0)?),
            }
        }
        if self.eat(b'(')? {
            self.metadata(layer, Target::Prop(prim.to_string(), name.clone()))?;
        }
        if delete {
            return Ok(());
        }
        if let Some(v) = &mut value {
            fix_width(v, &type_name);
        }
        let prop = layer.prim_mut(prim).prop_mut(&name);
        prop.type_name = type_name;
        if let Some(v) = value {
            prop.default = Some(Arc::new(v));
        }
        if let Some(v) = sample {
            prop.sample = Some(v);
        }
        if samples.is_some() {
            prop.samples = samples;
        }
        if !targets.is_empty() {
            prop.targets = targets;
        }
        Ok(())
    }

    /// A `{ time: value, ... }` block: the value at the earliest time, and
    /// with `all` every sample in time order.  Without `all`, samples later
    /// than the earliest seen so far are skipped without being built.
    /// Blocked samples (`None`) are left out.
    fn time_samples(&mut self, h: Hint, all: bool, type_name: &str) -> Result<TimeSampled, LoadError> {
        self.expect(b'{')?;
        let mut pairs: Vec<(f64, Value)> = Vec::new();
        loop {
            if self.eat(b'}')? {
                break;
            }
            if self.eat(b',')? {
                continue;
            }
            let time = match self.next()? {
                Tok::Num(s) | Tok::Ident(s) => s.parse::<f64>().unwrap_or(f64::NAN),
                t => return Err(err(format!("unexpected {t:?} as a sample time"))),
            };
            self.expect(b':')?;
            let wanted = all || pairs.first().is_none_or(|(t, _)| time < *t);
            if !wanted || !time.is_finite() {
                self.skip_value()?;
                continue;
            }
            let mut v = self.value(h, 0)?;
            if v == Value::None {
                continue;
            }
            fix_width(&mut v, type_name);
            if all {
                pairs.push((time, v));
            } else {
                pairs = vec![(time, v)];
            }
        }
        // Later duplicates of a time win, as in USD's own reader.
        pairs.reverse();
        pairs.sort_by(|a, b| a.0.total_cmp(&b.0));
        pairs.dedup_by(|a, b| a.0 == b.0);
        let (times, values): (Vec<f64>, Vec<Arc<Value>>) = pairs.into_iter().map(|(t, v)| (t, Arc::new(v))).unzip();
        let first = values.first().cloned();
        Ok((first, if all { Some(Arc::new(Samples { times, values })) } else { None }))
    }

    /// Reads a metadata block after its `(`.
    fn metadata(&mut self, layer: &mut Layer, target: Target) -> Result<(), LoadError> {
        loop {
            let key = match self.next()? {
                Tok::Punct(b')') => return Ok(()),
                Tok::Punct(b';') | Tok::Str(_) => continue,
                Tok::Ident(k) => k,
                t => return Err(err(format!("unexpected {t:?} in metadata"))),
            };
            let (op, key) = match key {
                "prepend" | "append" | "add" | "delete" | "reorder" => (key, self.ident()?),
                _ => ("", key),
            };
            self.expect(b'=')?;
            let delete = op == "delete" || op == "reorder";
            match (&target, key) {
                (Target::Prim(path), "references" | "payload") => {
                    let refs = self.refs()?;
                    if !delete {
                        let spec = layer.prim_mut(path);
                        if key == "payload" { spec.payloads.extend(refs) } else { spec.references.extend(refs) }
                    }
                }
                (Target::Prim(path), "inherits" | "specializes") => {
                    let paths = self.paths()?;
                    if !delete {
                        layer.prim_mut(path).inherits.extend(paths);
                    }
                }
                (Target::Prim(path), "variants") => {
                    if let Value::Dict(d) = self.value(Hint::Str, 0)? {
                        let spec = layer.prim_mut(path);
                        for (k, v) in d {
                            if let Value::Str(sel) = v {
                                spec.variant_selection.push((k, sel));
                            }
                        }
                    }
                }
                (Target::Prim(path), "variantSets" | "apiSchemas") => {
                    let names = match self.value(Hint::Str, 0)? {
                        Value::Str(s) => vec![s],
                        Value::Strs(s) => s,
                        _ => Vec::new(),
                    };
                    if !delete {
                        let spec = layer.prim_mut(path);
                        let list = if key == "variantSets" { &mut spec.variant_sets } else { &mut spec.api_schemas };
                        for n in names {
                            if !list.contains(&n) {
                                list.push(n);
                            }
                        }
                    }
                }
                (Target::Prim(path), "active") => {
                    let v = self.value(Hint::Int, 0)?;
                    layer.prim_mut(path).active = Some(v != Value::Bool(false) && v != Value::Int(0));
                }
                (Target::Prim(path), "assetInfo") => {
                    let v = self.value(Hint::Str, 0)?;
                    layer.prim_mut(path).asset_info = Some(v);
                }
                (Target::Prim(path), "inactiveIds") => {
                    let v = self.value(Hint::Int, 0)?;
                    if !delete {
                        let ids = match v {
                            Value::Ints(i) => i.into_iter().map(i64::from).collect(),
                            Value::Int(i) => vec![i],
                            _ => Vec::new(),
                        };
                        layer.prim_mut(path).inactive_ids = Some(ids);
                    }
                }
                (Target::Layer, "subLayers") => {
                    if let Value::Strs(s) = self.value(Hint::Str, 0)? {
                        layer.sublayers = s;
                    }
                }
                (Target::Layer, "defaultPrim" | "upAxis") => {
                    let v = self.value(Hint::Str, 0)?;
                    let s = v.as_str().map(str::to_string);
                    if key == "upAxis" { layer.up_axis = s } else { layer.default_prim = s }
                }
                (Target::Layer, "metersPerUnit") => layer.meters_per_unit = self.value(Hint::Float, 0)?.as_f64(),
                (Target::Layer, "timeCodesPerSecond" | "framesPerSecond" | "startTimeCode" | "startFrame") => {
                    let v = self.value(Hint::Float, 0)?.as_f64();
                    match key {
                        "timeCodesPerSecond" => layer.time_codes_per_second = v,
                        "framesPerSecond" => layer.frames_per_second = v,
                        // The newer name wins over the older one.
                        "startTimeCode" => layer.start_time = v,
                        _ => layer.start_time = layer.start_time.or(v),
                    }
                }
                (Target::Prop(prim, name), "bindMaterialAs") => {
                    let v = self.value(Hint::Str, 0)?;
                    layer.prim_mut(prim).prop_mut(name).stronger = Some(v.as_str() == Some("strongerThanDescendants"));
                }
                (Target::Prop(prim, name), "weight") => {
                    let v = self.value(Hint::Float, 0)?;
                    layer.prim_mut(prim).prop_mut(name).weight = v.as_f64();
                }
                (Target::Prop(prim, name), "interpolation" | "colorSpace" | "elementSize") => {
                    let v = self.value(Hint::Int, 0)?;
                    let prop = layer.prim_mut(prim).prop_mut(name);
                    match key {
                        "interpolation" => prop.interpolation = v.as_str().map(str::to_string),
                        "colorSpace" => prop.color_space = v.as_str().map(str::to_string),
                        _ => prop.element_size = v.as_f64().map(|f| f as i64),
                    }
                }
                _ => {
                    self.value(Hint::Str, 0)?;
                    if self.is(b'(')? {
                        self.skip_value()?;
                    }
                }
            }
        }
    }
}

//endregion
