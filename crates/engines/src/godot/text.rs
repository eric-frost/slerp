//! Godot's text resource format (`.tscn`, `.escn`, `.tres`): sections in
//! square brackets, each followed by `key = value` lines, values written in
//! Godot's variant syntax.  Godot 2, 3 and 4 spellings are all read (`Transform`
//! and `Transform3D`, `PoolVector3Array` and `PackedVector3Array`), and the
//! large arrays that embedded meshes hold are parsed in tight loops rather
//! than token by token.

use super::variant::{ExtRes, NodeEntry, ResFile, SubRes, Value};
use slerp_scene::LoadError;

/// Deeper nesting than this is an attack, not a scene.
const MAX_DEPTH: u32 = 64;
const MAX_SECTIONS: usize = 2_000_000;

/// The `type` attribute of a `[gd_resource ...]` header line, for sniffing.
pub(crate) fn header_type(line: &[u8]) -> Option<String> {
    let mut p = Parser { s: line, pos: 0 };
    p.skip_ws();
    if !p.eat(b'[') {
        return None;
    }
    let (_, attrs) = p.header().ok()?;
    attrs.into_iter().find(|(k, _)| k == "type").and_then(|(_, v)| v.str().map(str::to_string))
}

/// Parses a whole text resource or scene.
pub(crate) fn parse(bytes: &[u8]) -> Result<ResFile, LoadError> {
    let s = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
    let mut p = Parser { s, pos: 0 };
    let mut file = ResFile::default();
    // What the current section's `key = value` lines belong to.
    enum Target {
        Nothing,
        Sub(usize),
        Node(usize),
        Main,
    }
    let mut target = Target::Nothing;
    let mut sections = 0usize;
    let mut first = true;
    loop {
        p.skip_ws();
        let Some(c) = p.peek() else { break };
        if c == b'[' {
            p.pos += 1;
            sections += 1;
            if sections > MAX_SECTIONS {
                return Err(LoadError::TooLarge("too many sections".into()));
            }
            let (tag, attrs) = p.header()?;
            let attr = |k: &str| attrs.iter().find(|(n, _)| n == k).map(|(_, v)| v);
            let text = |k: &str| attr(k).and_then(|v| v.str()).map(str::to_string);
            let id = |k: &str| {
                attr(k).and_then(|v| match v {
                    Value::Str(s) => Some(s.clone()),
                    Value::Int(i) => Some(i.to_string()),
                    _ => None,
                })
            };
            if first {
                // 1. The header says what the file is and which Godot wrote
                //    it: format 1 and 2 are Godot 2 and 3, 3 and later 4.
                first = false;
                match tag.as_str() {
                    "gd_scene" => file.kind = "PackedScene".into(),
                    "gd_resource" => file.kind = text("type").unwrap_or_default(),
                    _ => return Err(LoadError::NotMine),
                }
                file.godot3 = attr("format").and_then(Value::int).is_some_and(|f| f < 3);
                target = Target::Nothing;
                continue;
            }
            target = match tag.as_str() {
                "ext_resource" => {
                    file.ext.push(ExtRes {
                        id: id("id").unwrap_or_default(),
                        kind: text("type").unwrap_or_default(),
                        path: text("path").unwrap_or_default(),
                    });
                    Target::Nothing
                }
                "sub_resource" => {
                    file.subs.push(SubRes {
                        id: id("id").unwrap_or_default(),
                        kind: text("type").unwrap_or_default(),
                        props: Vec::new(),
                    });
                    Target::Sub(file.subs.len() - 1)
                }
                "node" => {
                    file.nodes.push(NodeEntry {
                        name: text("name").unwrap_or_default(),
                        class: text("type"),
                        parent: text("parent"),
                        instance: attr("instance").cloned(),
                        props: Vec::new(),
                    });
                    Target::Node(file.nodes.len() - 1)
                }
                "resource" => Target::Main,
                // Connections and editable paths change nothing visible.
                _ => Target::Nothing,
            };
            continue;
        }
        if first {
            return Err(LoadError::NotMine);
        }
        // 2. A property line.
        let key = p.key()?;
        let value = p.value(0)?;
        match target {
            Target::Sub(i) => file.subs[i].props.push((key, value)),
            Target::Node(i) => file.nodes[i].props.push((key, value)),
            Target::Main => file.main.push((key, value)),
            Target::Nothing => {}
        }
    }
    if first {
        return Err(LoadError::NotMine);
    }
    Ok(file)
}

struct Parser<'a> {
    s: &'a [u8],
    pos: usize,
}

fn invalid(msg: impl Into<String>) -> LoadError {
    LoadError::invalid(msg)
}

impl Parser<'_> {
    fn peek(&self) -> Option<u8> {
        self.s.get(self.pos).copied()
    }

    fn eat(&mut self, c: u8) -> bool {
        if self.peek() == Some(c) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    /// Skips whitespace and `;` comments.
    fn skip_ws(&mut self) {
        while let Some(c) = self.peek() {
            if c.is_ascii_whitespace() {
                self.pos += 1;
            } else if c == b';' {
                while self.peek().is_some_and(|c| c != b'\n') {
                    self.pos += 1;
                }
            } else {
                break;
            }
        }
    }

    fn expect(&mut self, c: u8) -> Result<(), LoadError> {
        self.skip_ws();
        if self.eat(c) { Ok(()) } else { Err(invalid(format!("expected '{}' at byte {}", c as char, self.pos))) }
    }

    /// A section header after its `[`: the tag and `key=value` pairs.
    fn header(&mut self) -> Result<(String, Vec<(String, Value)>), LoadError> {
        self.skip_ws();
        let tag = self.ident().ok_or_else(|| invalid("bad section header"))?;
        let mut attrs = Vec::new();
        loop {
            self.skip_ws();
            if self.eat(b']') {
                return Ok((tag, attrs));
            }
            let key = self.ident().ok_or_else(|| invalid(format!("bad attribute in [{tag}] header")))?;
            self.expect(b'=')?;
            attrs.push((key, self.value(0)?));
        }
    }

    /// A property name: everything up to `=` on its line, or a quoted
    /// string.
    fn key(&mut self) -> Result<String, LoadError> {
        if self.peek() == Some(b'"') {
            let k = self.string()?;
            self.expect(b'=')?;
            return Ok(k);
        }
        let start = self.pos;
        while let Some(c) = self.peek() {
            match c {
                b'=' => {
                    let key = String::from_utf8_lossy(&self.s[start..self.pos]).trim().to_string();
                    self.pos += 1;
                    return Ok(key);
                }
                b'\n' => break,
                _ => self.pos += 1,
            }
        }
        Err(invalid(format!("expected a property at byte {start}")))
    }

    fn ident(&mut self) -> Option<String> {
        let start = self.pos;
        while self.peek().is_some_and(|c| c.is_ascii_alphanumeric() || c == b'_') {
            self.pos += 1;
        }
        (self.pos > start && !self.s[start].is_ascii_digit())
            .then(|| String::from_utf8_lossy(&self.s[start..self.pos]).into_owned())
    }

    fn string(&mut self) -> Result<String, LoadError> {
        if !self.eat(b'"') {
            return Err(invalid("expected a string"));
        }
        let mut out: Vec<u8> = Vec::new();
        loop {
            // Copy runs without escapes in one go.
            let start = self.pos;
            while self.peek().is_some_and(|c| c != b'"' && c != b'\\') {
                self.pos += 1;
            }
            out.extend_from_slice(&self.s[start..self.pos]);
            match self.peek() {
                Some(b'"') => {
                    self.pos += 1;
                    return Ok(String::from_utf8_lossy(&out).into_owned());
                }
                Some(b'\\') => {
                    self.pos += 1;
                    let e = self.peek().ok_or_else(|| invalid("unterminated string"))?;
                    self.pos += 1;
                    match e {
                        b'n' => out.push(b'\n'),
                        b't' => out.push(b'\t'),
                        b'r' => out.push(b'\r'),
                        b'u' | b'U' => {
                            let len = if e == b'u' { 4 } else { 6 };
                            let hex = self.s.get(self.pos..self.pos + len).ok_or_else(|| invalid("bad escape"))?;
                            self.pos += len;
                            let code = std::str::from_utf8(hex).ok().and_then(|h| u32::from_str_radix(h, 16).ok());
                            let ch = code.and_then(char::from_u32).unwrap_or('\u{FFFD}');
                            out.extend_from_slice(ch.encode_utf8(&mut [0; 4]).as_bytes());
                        }
                        other => out.push(other),
                    }
                }
                _ => return Err(invalid("unterminated string")),
            }
        }
    }

    /// The text of a number at the cursor, sign and exponent included.
    fn number_text(&mut self) -> &[u8] {
        let start = self.pos;
        if matches!(self.peek(), Some(b'-' | b'+')) {
            self.pos += 1;
        }
        if self.s[self.pos..].starts_with(b"inf") {
            self.pos += 3;
            return &self.s[start..self.pos];
        }
        while let Some(c) = self.peek() {
            let exponent_sign = matches!(c, b'-' | b'+') && matches!(self.s.get(self.pos - 1), Some(b'e' | b'E'));
            if c.is_ascii_digit() || matches!(c, b'.' | b'e' | b'E') || exponent_sign {
                self.pos += 1;
            } else {
                break;
            }
        }
        &self.s[start..self.pos]
    }

    fn number(&mut self) -> Result<Value, LoadError> {
        let text = self.number_text();
        let t = std::str::from_utf8(text).unwrap_or("");
        if let Ok(i) = t.parse::<i64>() {
            return Ok(Value::Int(i));
        }
        match t {
            "inf" | "+inf" => return Ok(Value::Float(f64::INFINITY)),
            "-inf" => return Ok(Value::Float(f64::NEG_INFINITY)),
            _ => {}
        }
        t.parse::<f64>().map(Value::Float).map_err(|_| invalid(format!("bad number {t:?}")))
    }

    fn value(&mut self, depth: u32) -> Result<Value, LoadError> {
        if depth > MAX_DEPTH {
            return Err(invalid("values nested too deeply"));
        }
        self.skip_ws();
        let c = self.peek().ok_or_else(|| invalid("value expected at end of file"))?;
        match c {
            b'"' => Ok(Value::Str(self.string()?)),
            b'&' | b'^' if self.s.get(self.pos + 1) == Some(&b'"') => {
                self.pos += 1;
                Ok(Value::Str(self.string()?))
            }
            b'[' => {
                self.pos += 1;
                Ok(Value::Array(self.list(b']', depth)?))
            }
            b'{' => {
                self.pos += 1;
                self.dict(depth)
            }
            b'-' | b'+' | b'.' | b'0'..=b'9' => self.number(),
            _ if c.is_ascii_alphabetic() || c == b'_' => {
                let id = self.ident().ok_or_else(|| invalid("bad identifier"))?;
                self.skip_ws();
                match self.peek() {
                    Some(b'(') => {
                        self.pos += 1;
                        self.construct(&id, depth)
                    }
                    // A typed array or dictionary: Array[Type]([...]).
                    Some(b'[') if id == "Array" || id == "Dictionary" => {
                        self.skip_group(b'[', b']')?;
                        self.expect(b'(')?;
                        let v = self.value(depth + 1)?;
                        self.expect(b')')?;
                        Ok(v)
                    }
                    _ => Ok(match id.as_str() {
                        "true" => Value::Bool(true),
                        "false" => Value::Bool(false),
                        "null" | "nil" => Value::Nil,
                        "inf" => Value::Float(f64::INFINITY),
                        "inf_neg" => Value::Float(f64::NEG_INFINITY),
                        "nan" => Value::Float(f64::NAN),
                        _ => Value::Other(id),
                    }),
                }
            }
            _ => Err(invalid(format!("unexpected byte {c:#x} at {}", self.pos))),
        }
    }

    /// Skips a bracketed group whose opening bracket is at the cursor.
    fn skip_group(&mut self, open: u8, close: u8) -> Result<(), LoadError> {
        let mut level = 0u32;
        while let Some(c) = self.peek() {
            if c == b'"' {
                self.string()?;
                continue;
            }
            self.pos += 1;
            if c == open {
                level += 1;
            } else if c == close {
                level -= 1;
                if level == 0 {
                    return Ok(());
                }
            }
            if level > MAX_DEPTH {
                return Err(invalid("brackets nested too deeply"));
            }
        }
        Err(invalid("unclosed bracket"))
    }

    fn list(&mut self, close: u8, depth: u32) -> Result<Vec<Value>, LoadError> {
        let mut out = Vec::new();
        loop {
            self.skip_ws();
            if self.eat(close) {
                return Ok(out);
            }
            out.push(self.value(depth + 1)?);
            self.skip_ws();
            if self.eat(b',') {
                continue;
            }
            if self.eat(close) {
                return Ok(out);
            }
            return Err(invalid(format!("expected ',' or '{}' at byte {}", close as char, self.pos)));
        }
    }

    fn dict(&mut self, depth: u32) -> Result<Value, LoadError> {
        let mut pairs = Vec::new();
        loop {
            self.skip_ws();
            if self.eat(b'}') {
                return Ok(Value::Dict(pairs));
            }
            let k = self.value(depth + 1)?;
            self.expect(b':')?;
            let v = self.value(depth + 1)?;
            pairs.push((k, v));
            self.skip_ws();
            if self.eat(b',') {
                continue;
            }
            if self.eat(b'}') {
                return Ok(Value::Dict(pairs));
            }
            return Err(invalid(format!("expected ',' or '}}' at byte {}", self.pos)));
        }
    }

    /// Numbers up to the closing parenthesis, parsed without tokens: the
    /// arrays of an embedded mesh run to millions of them.
    fn numbers(&mut self, mut push: impl FnMut(f64)) -> Result<(), LoadError> {
        loop {
            while self.peek().is_some_and(|c| c.is_ascii_whitespace() || c == b',') {
                self.pos += 1;
            }
            match self.peek() {
                Some(b')') => {
                    self.pos += 1;
                    return Ok(());
                }
                Some(c) if c.is_ascii_digit() || matches!(c, b'-' | b'+' | b'.' | b'i' | b'n') => {
                    let start = self.pos;
                    let text = if c == b'n' || c == b'i' {
                        // nan, inf, inf_neg
                        while self.peek().is_some_and(|c| c.is_ascii_alphabetic() || c == b'_') {
                            self.pos += 1;
                        }
                        &self.s[start..self.pos]
                    } else {
                        self.number_text()
                    };
                    let t = std::str::from_utf8(text).unwrap_or("");
                    let v = match t {
                        "nan" => f64::NAN,
                        "inf" | "+inf" => f64::INFINITY,
                        "inf_neg" | "-inf" => f64::NEG_INFINITY,
                        _ => t.parse::<f64>().map_err(|_| invalid(format!("bad number {t:?}")))?,
                    };
                    push(v);
                }
                _ => return Err(invalid(format!("expected a number at byte {}", self.pos))),
            }
        }
    }

    /// A constructor after its `(`.
    fn construct(&mut self, name: &str, depth: u32) -> Result<Value, LoadError> {
        let packed_floats = match name {
            "PackedFloat32Array" | "PackedFloat64Array" | "PackedRealArray" | "PoolRealArray" | "FloatArray"
            | "RealArray" => Some("PackedFloat32Array"),
            "PackedVector2Array" | "PoolVector2Array" | "Vector2Array" => Some("PackedVector2Array"),
            "PackedVector3Array" | "PoolVector3Array" | "Vector3Array" => Some("PackedVector3Array"),
            "PackedVector4Array" => Some("PackedVector4Array"),
            "PackedColorArray" | "PoolColorArray" | "ColorArray" => Some("PackedColorArray"),
            _ => None,
        };
        if let Some(kind) = packed_floats {
            let mut out = Vec::new();
            self.numbers(|v| out.push(v as f32))?;
            return Ok(Value::Floats(kind.into(), out));
        }
        match name {
            "PackedByteArray" | "PoolByteArray" | "ByteArray" | "RawArray" => {
                self.skip_ws();
                if self.peek() == Some(b'"') {
                    let encoded = self.string()?;
                    self.expect(b')')?;
                    return base64(encoded.as_bytes()).map(Value::Bytes).ok_or_else(|| invalid("bad base64"));
                }
                let mut out = Vec::new();
                self.numbers(|v| out.push(v as u8))?;
                Ok(Value::Bytes(out))
            }
            "PackedInt32Array" | "PackedInt64Array" | "PackedIntArray" | "PoolIntArray" | "IntArray" => {
                let mut out = Vec::new();
                self.numbers(|v| out.push(v as i64))?;
                Ok(Value::Ints(out))
            }
            "PackedStringArray" | "PoolStringArray" | "StringArray" => {
                let items = self.list(b')', depth)?;
                Ok(Value::Strings(items.into_iter().filter_map(|v| v.str().map(str::to_string)).collect()))
            }
            "ExtResource" | "SubResource" => {
                let args = self.list(b')', depth)?;
                let id = match args.first() {
                    Some(Value::Str(s)) => s.clone(),
                    Some(Value::Int(i)) => i.to_string(),
                    _ => return Err(invalid(format!("bad {name}"))),
                };
                Ok(if name == "ExtResource" { Value::Ext(id) } else { Value::Sub(id) })
            }
            "NodePath" | "StringName" => {
                let args = self.list(b')', depth)?;
                Ok(args.into_iter().next().unwrap_or(Value::Str(String::new())))
            }
            _ => {
                let canonical = match name {
                    "Quat" => "Quaternion",
                    "Rect3" => "AABB",
                    "Matrix3" => "Basis",
                    "Transform" => "Transform3D",
                    "Matrix32" => "Transform2D",
                    other => other,
                };
                let math = matches!(
                    canonical,
                    "Vector2"
                        | "Vector2i"
                        | "Rect2"
                        | "Rect2i"
                        | "Vector3"
                        | "Vector3i"
                        | "Transform2D"
                        | "Vector4"
                        | "Vector4i"
                        | "Plane"
                        | "Quaternion"
                        | "AABB"
                        | "Basis"
                        | "Transform3D"
                        | "Projection"
                        | "Color"
                );
                if math {
                    let mut out = Vec::new();
                    self.numbers(|v| out.push(v))?;
                    return Ok(Value::Math(canonical.into(), out));
                }
                // Objects, Callables and the like: skipped whole.
                self.pos -= 1;
                self.skip_group(b'(', b')')?;
                Ok(Value::Other(name.into()))
            }
        }
    }
}

/// Decodes standard base64, as Godot writes large byte arrays.
pub(crate) fn base64(input: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(input.len() / 4 * 3);
    let mut acc = 0u32;
    let mut bits = 0u32;
    for &c in input {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            b'=' => break,
            c if c.is_ascii_whitespace() => continue,
            _ => return None,
        } as u32;
        acc = (acc << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_godot4_and_godot3_syntax() {
        let text = br#"[gd_scene load_steps=3 format=3 uid="uid://abc"]

[ext_resource type="PackedScene" uid="uid://x" path="res://a.tscn" id="1_ab"]

[sub_resource type="BoxMesh" id="BoxMesh_1"]
size = Vector3(2, 1e+01, -0.5)

[node name="Root" type="Node3D"]
; a comment
transform = Transform3D(1, 0, 0, 0, 1, 0, 0, 0, 1, 1.5, inf, -inf)
metadata/_edit_group_ = true
"odd key" = &"name"

[node name="Box" type="MeshInstance3D" parent="."]
mesh = SubResource("BoxMesh_1")
data = {
"cells": PackedInt32Array(1, -2, 65536)
}
arr = Array[StringName]([&"a", &"b"])
bytes = PackedByteArray("AAEC/w==")
obj = Object(Node,"script":null)

[node name="Inst" parent="." instance=ExtResource("1_ab")]
"#;
        let f = parse(text).unwrap();
        assert_eq!(f.kind, "PackedScene");
        assert!(!f.godot3);
        assert_eq!(f.ext[0].path, "res://a.tscn");
        assert_eq!(f.subs[0].props[0].1.vec3().unwrap().y, 10.0);
        assert_eq!(f.nodes.len(), 3);
        assert_eq!(f.nodes[0].props.len(), 3);
        assert_eq!(f.nodes[1].props[1].1.get("cells").unwrap().ints().unwrap(), &[1, -2, 65536]);
        assert_eq!(f.nodes[1].props[2].1, Value::Array(vec![Value::Str("a".into()), Value::Str("b".into())]));
        assert_eq!(f.nodes[1].props[3].1.bytes().unwrap(), &[0, 1, 2, 255]);
        assert_eq!(f.nodes[2].instance, Some(Value::Ext("1_ab".into())));
        assert_eq!(f.nodes[2].parent.as_deref(), Some("."));

        let old = b"[gd_scene load_steps=2 format=2]\n[ext_resource path=\"res://m.mesh\" type=\"ArrayMesh\" id=1]\n\
            [node name=\"S\" type=\"Spatial\"]\ntransform = Transform( 1, 0, 0, 0, 1, 0, 0, 0, 1, 0, 2, 0 )\n\
            [node name=\"M\" type=\"MeshInstance\" parent=\".\"]\nmesh = ExtResource( 1 )\n";
        let f = parse(old).unwrap();
        assert!(f.godot3);
        assert_eq!(f.nodes[1].props[0].1, Value::Ext("1".into()));
        assert_eq!(f.nodes[0].props[0].1.transform().unwrap().w_axis.y, 2.0);
    }

    #[test]
    fn broken_text_fails_cleanly() {
        let good = br#"[gd_scene format=3]
[node name="R" type="Node3D"]
transform = Transform3D(1, 0, 0, 0, 1, 0, 0, 0, 1, 0, 0, 0)
data = {"a": [1, 2, {"b": PackedVector3Array(1, 2, 3)}]}
"#;
        for cut in 0..good.len() {
            let _ = parse(&good[..cut]);
        }
        let mut deep = b"[gd_scene format=3]\n[node name=\"R\" type=\"Node3D\"]\nx = ".to_vec();
        deep.extend(std::iter::repeat_n(b'[', 10_000));
        assert!(parse(&deep).is_err());
        assert!(matches!(parse(b"hello"), Err(LoadError::NotMine)));
        assert_eq!(
            header_type(br#"[gd_resource type="ArrayMesh" load_steps=2 format=3]"#).as_deref(),
            Some("ArrayMesh")
        );
    }
}
