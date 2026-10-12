//! VRML 2.0 (VRML97) and X3D, in X3D's classic (VRML-style) and XML
//! encodings, gzipped or not.
//!
//! Both encodings are read into the same generic node graph: nodes with a
//! type name and named fields whose values are numbers, strings, booleans or
//! other nodes.  `DEF`/`USE` share nodes, `PROTO` instances are expanded by
//! copying the prototype's body with its `IS` fields filled in, and
//! `Inline` files are read when `Limits::neighbours` allows.  The scene
//! builder then walks the graph:
//!
//! - Grouping: Transform, Group, StaticGroup, Anchor, Billboard, Collision,
//!   Switch, LOD, Inline, and the H-Anim and CAD grouping nodes.
//! - Geometry: IndexedFaceSet (convex and concave faces, crease angles,
//!   colours and normals per vertex or per face, texture coordinates, `ccw`
//!   and `solid`), IndexedLineSet, PointSet, ElevationGrid, Extrusion, Box,
//!   Sphere, Cylinder, Cone, and X3D's triangle, fan, strip and quad sets.
//! - Appearance: Material, X3D's PhysicalMaterial, UnlitMaterial and
//!   TwoSidedMaterial, ImageTexture, PixelTexture and TextureTransform.
//! - Animation: TimeSensors routed through Position-, Orientation- and
//!   ScaleInterpolators to transforms become clips.
//!
//! VRML 1.0 is a different language and is refused with a clear error.
//! Both formats are Y-up in metres, as Slerp is.  Colours are display
//! (sRGB) values, as 1990s browsers drew them.

use crate::archive::{gunzip, is_gzip};
use crate::common::{TextureSet, check_triangles, flipped_uv_transform, parse_f32, parse_i64, srgb_color};
use crate::polymesh::{Attr, Binding, PolyMesh, Smoothing};
use glam::{Mat3, Mat4, Quat, Vec2, Vec3, Vec4};
use slerp_scene::{
    AlphaMode, Animation, Channel, ColorSpace, Interpolation, Limits, LoadError, Material, Mesh, Node, Primitive,
    Property, Scene, TexRef, Texture, Topology, Transform, Wrap,
};
use std::collections::HashMap;
use std::path::Path;

/// Nodes nested deeper than this are malformed or malicious.
const MAX_DEPTH: usize = 200;
/// Nodes one load may create, counting prototype copies.
const MAX_NODES: usize = 2_000_000;
/// Inline files one load may read.
const MAX_INLINES: usize = 256;
/// Segments around generated spheres, cylinders and cones.
const SEGMENTS: usize = 32;

/// Which language a text file is in, from its header line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dialect {
    Vrml2,
    Vrml1,
    X3dClassic,
    X3dXml,
}

/// Recognises VRML and X3D by their header lines, and the XML encoding by
/// its root element.
pub fn dialect(head: &[u8]) -> Option<Dialect> {
    let head = crate::common::utf8_text(head);
    let t = crate::common::trim(&head);
    if t.starts_with(b"#VRML V2.0") || t.starts_with(b"#VRML V2") {
        Some(Dialect::Vrml2)
    } else if t.starts_with(b"#VRML V1.0") {
        Some(Dialect::Vrml1)
    } else if t.starts_with(b"#X3D V") {
        Some(Dialect::X3dClassic)
    } else if t.starts_with(b"<") && crate::common::find(t, b"<X3D").is_some() {
        Some(Dialect::X3dXml)
    } else {
        None
    }
}

/*┌─────────────────╮
| Node graph      |
└─────────────────╯*/
//region Node graph

#[derive(Clone, Debug)]
enum Value {
    Nums(Vec<f64>),
    Strs(Vec<String>),
    Bools(Vec<bool>),
    Nodes(Vec<usize>),
    Null,
    /// A prototype field reference, filled in when the prototype is used.
    Is(String),
}

#[derive(Clone, Debug, Default)]
struct VNode {
    kind: String,
    /// The `DEF` name, when it has one.
    name: Option<String>,
    fields: Vec<(String, Value)>,
}

#[derive(Clone, Debug)]
struct Proto {
    defaults: Vec<(String, Value)>,
    /// The body's first node, which gives the prototype its kind.
    body: Option<usize>,
}

#[derive(Clone, Debug)]
struct Route {
    from: String,
    from_field: String,
    to: String,
    to_field: String,
}

/// One parsed file: its node graph, root nodes, names and routes.
#[derive(Default)]
struct Doc {
    nodes: Vec<VNode>,
    roots: Vec<usize>,
    defs: HashMap<String, usize>,
    protos: HashMap<String, Proto>,
    routes: Vec<Route>,
    /// The folder of an inlined file relative to the main file, prefixed
    /// to the names it refers to.
    dir: String,
    warnings: Vec<String>,
}

impl Doc {
    fn push(&mut self, node: VNode) -> Option<usize> {
        if self.nodes.len() >= MAX_NODES {
            return None;
        }
        self.nodes.push(node);
        Some(self.nodes.len() - 1)
    }

    fn field(&self, node: usize, name: &str) -> Option<&Value> {
        self.nodes.get(node)?.fields.iter().rev().find(|(n, _)| n == name).map(|(_, v)| v)
    }

    fn nums(&self, node: usize, name: &str) -> &[f64] {
        match self.field(node, name) {
            Some(Value::Nums(v)) => v,
            _ => &[],
        }
    }

    fn num(&self, node: usize, name: &str, default: f32) -> f32 {
        self.nums(node, name).first().map(|&v| v as f32).filter(|v| v.is_finite()).unwrap_or(default)
    }

    fn vec3(&self, node: usize, name: &str, default: Vec3) -> Vec3 {
        match self.nums(node, name) {
            [x, y, z, ..] => Vec3::new(*x as f32, *y as f32, *z as f32),
            _ => default,
        }
    }

    fn boolean(&self, node: usize, name: &str, default: bool) -> bool {
        match self.field(node, name) {
            Some(Value::Bools(b)) => b.first().copied().unwrap_or(default),
            _ => default,
        }
    }

    fn strings(&self, node: usize, name: &str) -> &[String] {
        match self.field(node, name) {
            Some(Value::Strs(v)) => v,
            _ => &[],
        }
    }

    fn nodes(&self, node: usize, name: &str) -> &[usize] {
        match self.field(node, name) {
            Some(Value::Nodes(v)) => v,
            _ => &[],
        }
    }

    fn child(&self, node: usize, name: &str) -> Option<usize> {
        self.nodes(node, name).first().copied().filter(|&c| c < self.nodes.len())
    }

    fn kind(&self, node: usize) -> &str {
        self.nodes.get(node).map_or("", |n| n.kind.as_str())
    }

    /// Points from a coordinate-like node's `field` (Coordinate.point,
    /// Normal.vector, Color.color), three numbers each.
    fn vec3s(&self, node: Option<usize>, field: &str) -> Vec<Vec3> {
        let Some(n) = node else { return Vec::new() };
        self.nums(n, field).chunks_exact(3).map(|c| Vec3::new(c[0] as f32, c[1] as f32, c[2] as f32)).collect()
    }

    /// Copies prototype `name`'s body for an instance with `fields`.
    fn instantiate(&mut self, name: &str, fields: &[(String, Value)], depth: usize) -> Option<usize> {
        let proto = self.protos.get(name)?.clone();
        let body = proto.body?;
        let mut copies = HashMap::new();
        self.copy(body, fields, &proto.defaults, &mut copies, depth)
    }

    fn copy(
        &mut self,
        node: usize,
        fields: &[(String, Value)],
        defaults: &[(String, Value)],
        copies: &mut HashMap<usize, usize>,
        depth: usize,
    ) -> Option<usize> {
        if let Some(&c) = copies.get(&node) {
            return Some(c);
        }
        if depth > MAX_DEPTH || self.nodes.len() >= MAX_NODES {
            return None;
        }
        let source = self.nodes.get(node)?.clone();
        let mut out = VNode { kind: source.kind, name: source.name, fields: Vec::with_capacity(source.fields.len()) };
        for (field, value) in source.fields {
            let value = match value {
                Value::Is(of) => fields
                    .iter()
                    .rev()
                    .chain(defaults.iter().rev())
                    .find(|(n, _)| *n == of)
                    .map_or(Value::Null, |(_, v)| v.clone()),
                Value::Nodes(list) => Value::Nodes(
                    list.into_iter().filter_map(|c| self.copy(c, fields, defaults, copies, depth + 1)).collect(),
                ),
                v => v,
            };
            out.fields.push((field, value));
        }
        let index = self.push(out)?;
        copies.insert(node, index);
        Some(index)
    }
}

//endregion

/*┌─────────────────╮
| Classic text    |
└─────────────────╯*/
//region Classic text

#[derive(Clone, Debug, PartialEq)]
enum Tok<'a> {
    Open,
    Close,
    LBracket,
    RBracket,
    Str(String),
    Num(f64),
    Id(&'a [u8]),
    End,
}

struct Lexer<'a> {
    b: &'a [u8],
    at: usize,
    ahead: std::collections::VecDeque<Tok<'a>>,
}

impl<'a> Lexer<'a> {
    fn lex(&mut self) -> Tok<'a> {
        let b = self.b;
        loop {
            while self.at < b.len() && (b[self.at].is_ascii_whitespace() || b[self.at] == b',') {
                self.at += 1;
            }
            let Some(&c) = b.get(self.at) else { return Tok::End };
            if c == b'#' {
                while self.at < b.len() && b[self.at] != b'\n' && b[self.at] != b'\r' {
                    self.at += 1;
                }
                continue;
            }
            self.at += 1;
            return match c {
                b'{' => Tok::Open,
                b'}' => Tok::Close,
                b'[' => Tok::LBracket,
                b']' => Tok::RBracket,
                b'"' => {
                    let mut s = Vec::new();
                    while let Some(&c) = b.get(self.at) {
                        self.at += 1;
                        match c {
                            b'"' => break,
                            b'\\' => {
                                if let Some(&e) = b.get(self.at) {
                                    s.push(e);
                                    self.at += 1;
                                }
                            }
                            _ => s.push(c),
                        }
                    }
                    Tok::Str(String::from_utf8_lossy(&s).into_owned())
                }
                _ => {
                    let start = self.at - 1;
                    while self.at < b.len() && !b[self.at].is_ascii_whitespace() && !b",{}[]\"#".contains(&b[self.at]) {
                        self.at += 1;
                    }
                    let word = &b[start..self.at];
                    if c.is_ascii_digit() || matches!(c, b'-' | b'+' | b'.') {
                        Tok::Num(number(word))
                    } else {
                        Tok::Id(word)
                    }
                }
            };
        }
    }

    fn peek(&mut self, k: usize) -> &Tok<'a> {
        while self.ahead.len() <= k {
            let t = self.lex();
            self.ahead.push_back(t);
        }
        &self.ahead[k]
    }

    fn next(&mut self) -> Tok<'a> {
        self.ahead.pop_front().unwrap_or_else(|| self.lex())
    }
}

/// A number token: integers exactly (indices can pass 2^24), hexadecimal
/// pixels, decimals through the fast parser.  Malformed numbers are 0.
fn number(word: &[u8]) -> f64 {
    if let Some(hex) = word.strip_prefix(b"0x").or_else(|| word.strip_prefix(b"0X")) {
        return std::str::from_utf8(hex).ok().and_then(|h| i64::from_str_radix(h, 16).ok()).unwrap_or(0) as f64;
    }
    if let Some(i) = parse_i64(word) {
        return i as f64;
    }
    parse_f32(word).filter(|v| v.is_finite()).unwrap_or(0.0) as f64
}

const INTERFACE: [&[u8]; 8] = [
    b"field",
    b"exposedField",
    b"eventIn",
    b"eventOut",
    b"initializeOnly",
    b"inputOutput",
    b"inputOnly",
    b"outputOnly",
];

struct Classic<'a, 'd> {
    lex: Lexer<'a>,
    doc: &'d mut Doc,
}

impl Classic<'_, '_> {
    fn id_text(word: &[u8]) -> String {
        String::from_utf8_lossy(word).into_owned()
    }

    /// Reads statements until the end of input or a closing `}`/`]`.
    fn statements(&mut self, depth: usize, out: &mut Vec<usize>) {
        loop {
            match self.lex.peek(0).clone() {
                Tok::End | Tok::Close | Tok::RBracket => return,
                Tok::Id(_) => {
                    if !self.statement(depth, out) {
                        self.lex.next();
                    }
                }
                _ => {
                    self.lex.next();
                }
            }
        }
    }

    /// One statement starting at an identifier.  Returns false, having read
    /// nothing, when the next token isn't an identifier.
    fn statement(&mut self, depth: usize, out: &mut Vec<usize>) -> bool {
        let Tok::Id(word) = self.lex.peek(0).clone() else { return false };
        match word {
            b"ROUTE" => self.route(),
            b"PROTO" => self.proto(depth),
            b"EXTERNPROTO" => self.externproto(),
            b"PROFILE" | b"COMPONENT" => {
                self.lex.next();
                self.lex.next();
            }
            b"META" => {
                self.lex.next();
                self.lex.next();
                self.lex.next();
            }
            b"UNIT" => {
                for _ in 0..4 {
                    self.lex.next();
                }
            }
            b"IMPORT" | b"EXPORT" => {
                self.lex.next();
                self.lex.next();
                if self.lex.peek(0) == &Tok::Id(b"AS") {
                    self.lex.next();
                    self.lex.next();
                }
            }
            _ => {
                if let Some(n) = self.node(depth) {
                    out.push(n);
                }
            }
        }
        true
    }

    fn route(&mut self) {
        self.lex.next();
        let from = self.lex.next();
        let to_kw = self.lex.next();
        let to = self.lex.next();
        if let (Tok::Id(from), Tok::Id(b"TO"), Tok::Id(to)) = (from, to_kw, to) {
            let split = |w: &[u8]| {
                let s = Self::id_text(w);
                s.rsplit_once('.').map(|(a, b)| (a.to_string(), b.to_string()))
            };
            if let (Some((from, from_field)), Some((to, to_field))) = (split(from), split(to)) {
                self.doc.routes.push(Route { from, from_field, to, to_field });
            }
        }
    }

    /// `[ kind type name [default] ... ]`; defaults are kept when `values`.
    fn interface(&mut self, depth: usize) -> Vec<(String, Value)> {
        let mut out = Vec::new();
        if self.lex.peek(0) != &Tok::LBracket {
            return out;
        }
        self.lex.next();
        loop {
            match self.lex.next() {
                Tok::RBracket | Tok::End => return out,
                Tok::Id(kind) if INTERFACE.contains(&kind) => {
                    let (_ty, name) = (self.lex.next(), self.lex.next());
                    let Tok::Id(name) = name else { continue };
                    let with_value = matches!(kind, b"field" | b"exposedField" | b"initializeOnly" | b"inputOutput");
                    if with_value && !matches!(self.lex.peek(0), Tok::Id(k) if INTERFACE.contains(k)) {
                        let v = self.value(depth);
                        out.push((Self::id_text(name), v));
                    }
                }
                _ => {}
            }
        }
    }

    fn proto(&mut self, depth: usize) {
        self.lex.next();
        let Tok::Id(name) = self.lex.next() else { return };
        let name = Self::id_text(name);
        let defaults = self.interface(depth);
        if self.lex.peek(0) != &Tok::Open {
            return;
        }
        self.lex.next();
        // The body has its own names.
        let outer = std::mem::take(&mut self.doc.defs);
        let mut body = Vec::new();
        self.statements(depth + 1, &mut body);
        if self.lex.peek(0) == &Tok::Close {
            self.lex.next();
        }
        self.doc.defs = outer;
        self.doc.protos.insert(name, Proto { defaults, body: body.first().copied() });
    }

    fn externproto(&mut self) {
        self.lex.next();
        let Tok::Id(name) = self.lex.next() else { return };
        let name = Self::id_text(name);
        let _ = self.interface(0);
        let _ = self.value(0);
        self.doc.warnings.push(format!("EXTERNPROTO {name} not loaded"));
    }

    fn node(&mut self, depth: usize) -> Option<usize> {
        let Tok::Id(word) = self.lex.next() else { return None };
        match word {
            b"DEF" => {
                let Tok::Id(name) = self.lex.next() else { return None };
                let name = Self::id_text(name);
                let n = self.node(depth)?;
                self.doc.nodes[n].name = Some(name.clone());
                self.doc.defs.insert(name, n);
                Some(n)
            }
            b"USE" => {
                let Tok::Id(name) = self.lex.next() else { return None };
                self.doc.defs.get(&*String::from_utf8_lossy(name)).copied()
            }
            b"NULL" => None,
            kind => {
                if self.lex.peek(0) != &Tok::Open {
                    return None;
                }
                self.lex.next();
                let kind = Self::id_text(kind);
                if depth > MAX_DEPTH {
                    self.skip_body();
                    return None;
                }
                let mut node = VNode { kind: kind.clone(), name: None, fields: Vec::new() };
                self.fields(&mut node, depth);
                if self.doc.protos.contains_key(&kind) {
                    return self.doc.instantiate(&kind, &node.fields, depth);
                }
                self.doc.push(node)
            }
        }
    }

    fn skip_body(&mut self) {
        let mut depth = 1usize;
        loop {
            match self.lex.next() {
                Tok::Open => depth += 1,
                Tok::Close => {
                    depth -= 1;
                    if depth == 0 {
                        return;
                    }
                }
                Tok::End => return,
                _ => {}
            }
        }
    }

    fn fields(&mut self, node: &mut VNode, depth: usize) {
        loop {
            match self.lex.peek(0).clone() {
                Tok::Close => {
                    self.lex.next();
                    return;
                }
                Tok::End => return,
                Tok::Id(b"ROUTE") => self.route(),
                Tok::Id(b"PROTO") => self.proto(depth),
                Tok::Id(b"EXTERNPROTO") => self.externproto(),
                Tok::Id(kind) if INTERFACE.contains(&kind) => {
                    // A Script's own fields: kind, type, name, then a value
                    // or an IS.
                    self.lex.next();
                    self.lex.next();
                    let Tok::Id(name) = self.lex.next() else { continue };
                    let with_value = matches!(kind, b"field" | b"exposedField" | b"initializeOnly" | b"inputOutput");
                    if with_value || self.lex.peek(0) == &Tok::Id(b"IS") {
                        let v = self.value(depth);
                        node.fields.push((Self::id_text(name), v));
                    }
                }
                Tok::Id(name) => {
                    self.lex.next();
                    let v = self.value(depth);
                    node.fields.push((Self::id_text(name), v));
                }
                _ => {
                    self.lex.next();
                }
            }
        }
    }

    /// A field value of any type, recognised by its first token.
    fn value(&mut self, depth: usize) -> Value {
        match self.lex.peek(0).clone() {
            Tok::Num(_) => {
                let mut nums = Vec::new();
                while let Tok::Num(v) = self.lex.peek(0) {
                    nums.push(*v);
                    self.lex.next();
                }
                Value::Nums(nums)
            }
            Tok::Str(s) => {
                self.lex.next();
                Value::Strs(vec![s])
            }
            Tok::LBracket => {
                self.lex.next();
                let (mut nums, mut strs, mut bools, mut nodes) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
                loop {
                    match self.lex.peek(0).clone() {
                        Tok::RBracket => {
                            self.lex.next();
                            break;
                        }
                        Tok::End | Tok::Close => break,
                        Tok::Num(v) => {
                            nums.push(v);
                            self.lex.next();
                        }
                        Tok::Str(s) => {
                            strs.push(s);
                            self.lex.next();
                        }
                        Tok::Id(b"TRUE") => {
                            bools.push(true);
                            self.lex.next();
                        }
                        Tok::Id(b"FALSE") => {
                            bools.push(false);
                            self.lex.next();
                        }
                        Tok::Id(_) => {
                            let mut found = Vec::new();
                            if self.statement(depth + 1, &mut found) {
                                nodes.extend(found);
                            } else {
                                self.lex.next();
                            }
                        }
                        _ => {
                            self.lex.next();
                        }
                    }
                }
                if !nodes.is_empty() {
                    Value::Nodes(nodes)
                } else if !strs.is_empty() {
                    Value::Strs(strs)
                } else if !bools.is_empty() {
                    Value::Bools(bools)
                } else {
                    Value::Nums(nums)
                }
            }
            Tok::Id(b"TRUE") => {
                self.lex.next();
                Value::Bools(vec![true])
            }
            Tok::Id(b"FALSE") => {
                self.lex.next();
                Value::Bools(vec![false])
            }
            Tok::Id(b"NULL") => {
                self.lex.next();
                Value::Null
            }
            Tok::Id(b"IS") => {
                self.lex.next();
                match self.lex.next() {
                    Tok::Id(name) => Value::Is(Self::id_text(name)),
                    _ => Value::Null,
                }
            }
            Tok::Id(b"DEF") | Tok::Id(b"USE") => Value::Nodes(self.node(depth + 1).into_iter().collect()),
            Tok::Id(_) if self.lex.peek(1) == &Tok::Open => Value::Nodes(self.node(depth + 1).into_iter().collect()),
            _ => Value::Null,
        }
    }
}

fn parse_classic(text: &[u8], doc: &mut Doc) {
    let mut c = Classic { lex: Lexer { b: text, at: 0, ahead: Default::default() }, doc };
    let mut roots = Vec::new();
    loop {
        c.statements(0, &mut roots);
        // A stray closing bracket at the top level: skip it and go on.
        if c.lex.next() == Tok::End {
            break;
        }
    }
    doc.roots = roots;
}

//endregion

/*┌─────────────────╮
| XML encoding    |
└─────────────────╯*/
//region XML encoding

/// The field a child element fills when it doesn't say (`containerField`).
fn default_container(kind: &str) -> &'static str {
    match kind {
        "Appearance" => "appearance",
        "Material" | "TwoSidedMaterial" | "PhysicalMaterial" | "UnlitMaterial" => "material",
        "ImageTexture" | "PixelTexture" | "MovieTexture" | "MultiTexture" => "texture",
        "TextureTransform" | "MultiTextureTransform" => "textureTransform",
        "Coordinate" | "CoordinateDouble" => "coord",
        "TextureCoordinate" | "MultiTextureCoordinate" | "TextureCoordinateGenerator" => "texCoord",
        "Color" | "ColorRGBA" => "color",
        "Normal" => "normal",
        "FontStyle" => "fontStyle",
        "IndexedFaceSet"
        | "IndexedLineSet"
        | "PointSet"
        | "LineSet"
        | "Box"
        | "Sphere"
        | "Cone"
        | "Cylinder"
        | "ElevationGrid"
        | "Extrusion"
        | "Text"
        | "IndexedTriangleSet"
        | "TriangleSet"
        | "IndexedTriangleFanSet"
        | "IndexedTriangleStripSet"
        | "TriangleFanSet"
        | "TriangleStripSet"
        | "IndexedQuadSet"
        | "QuadSet" => "geometry",
        _ => "children",
    }
}

/// Attribute fields that hold strings even when they look like numbers.
fn is_string_field(name: &str) -> bool {
    matches!(
        name,
        "url" | "string" | "family" | "justify" | "info" | "title" | "description" | "name" | "style" | "type"
    )
}

/// An attribute's value, typed by what it looks like.
fn attribute_value(field: &str, text: &str) -> Value {
    if is_string_field(field) {
        let t = text.trim();
        if t.starts_with('"') || t.starts_with('\'') {
            let quote = t.as_bytes()[0] as char;
            return Value::Strs(t.split(quote).skip(1).step_by(2).map(str::to_string).collect());
        }
        return Value::Strs(vec![t.to_string()]);
    }
    let words: Vec<&str> =
        text.split(|c: char| c.is_ascii_whitespace() || c == ',').filter(|w| !w.is_empty()).collect();
    if !words.is_empty() && words.iter().all(|w| w.eq_ignore_ascii_case("true") || w.eq_ignore_ascii_case("false")) {
        return Value::Bools(words.iter().map(|w| w.eq_ignore_ascii_case("true")).collect());
    }
    let first = words.first().map_or(b' ', |w| w.as_bytes()[0]);
    if words.is_empty() || first.is_ascii_digit() || matches!(first, b'-' | b'+' | b'.') {
        return Value::Nums(words.iter().map(|w| number(w.as_bytes())).collect());
    }
    Value::Strs(vec![text.to_string()])
}

fn parse_xml(text: &[u8], doc: &mut Doc) -> Result<(), LoadError> {
    let text = crate::common::utf8_text(text);
    let text = String::from_utf8_lossy(&text);
    let options = roxmltree::ParsingOptions { allow_dtd: true, ..Default::default() };
    let xml = roxmltree::Document::parse_with_options(&text, options)
        .map_err(|e| LoadError::invalid(format!("X3D XML: {e}")))?;
    let root = xml.root_element();
    if root.tag_name().name() != "X3D" {
        return Err(LoadError::NotMine);
    }
    let Some(scene) = root.children().find(|c| c.is_element() && c.tag_name().name() == "Scene") else {
        return Err(LoadError::invalid("X3D file has no Scene"));
    };
    let mut roots = Vec::new();
    for c in scene.children().filter(|c| c.is_element()) {
        if let Some(n) = xml_node(c, doc, 0) {
            roots.push(n);
        }
    }
    doc.roots = roots;
    Ok(())
}

fn xml_node(e: roxmltree::Node, doc: &mut Doc, depth: usize) -> Option<usize> {
    let kind = e.tag_name().name();
    if depth > MAX_DEPTH {
        return None;
    }
    match kind {
        "ROUTE" => {
            let a = |n: &str| e.attribute(n).unwrap_or_default().to_string();
            doc.routes.push(Route {
                from: a("fromNode"),
                from_field: a("fromField"),
                to: a("toNode"),
                to_field: a("toField"),
            });
            return None;
        }
        "ProtoDeclare" => {
            xml_proto(e, doc, depth);
            return None;
        }
        "ExternProtoDeclare" => {
            doc.warnings.push(format!("ExternProtoDeclare {} not loaded", e.attribute("name").unwrap_or("")));
            return None;
        }
        "IS" | "field" | "fieldValue" | "metadata" | "MetadataString" | "MetadataFloat" | "MetadataInteger"
        | "MetadataSet" | "MetadataDouble" | "MetadataBoolean" => return None,
        _ => {}
    }
    if let Some(name) = e.attribute("USE") {
        return doc.defs.get(name).copied();
    }
    let mut node = VNode { kind: kind.to_string(), name: None, fields: Vec::new() };
    if kind == "ProtoInstance" {
        node.kind = e.attribute("name").unwrap_or_default().to_string();
    }
    for a in e.attributes() {
        match a.name() {
            "DEF" | "USE" | "containerField" | "class" | "id" => {}
            "name" if kind == "ProtoInstance" => {}
            name => node.fields.push((name.to_string(), attribute_value(name, a.value()))),
        }
    }
    // Child elements, grouped into the fields they fill, in order.
    for c in e.children().filter(|c| c.is_element()) {
        match c.tag_name().name() {
            "IS" => {
                for connect in c.children().filter(|x| x.is_element() && x.tag_name().name() == "connect") {
                    if let (Some(f), Some(p)) = (connect.attribute("nodeField"), connect.attribute("protoField")) {
                        node.fields.push((f.to_string(), Value::Is(p.to_string())));
                    }
                }
            }
            "fieldValue" if kind == "ProtoInstance" => {
                let Some(name) = c.attribute("name") else { continue };
                let value = match c.attribute("value") {
                    Some(v) => attribute_value(name, v),
                    None => Value::Nodes(
                        c.children().filter(|x| x.is_element()).filter_map(|x| xml_node(x, doc, depth + 1)).collect(),
                    ),
                };
                node.fields.push((name.to_string(), value));
            }
            _ => {
                let Some(child) = xml_node(c, doc, depth + 1) else { continue };
                let field = c.attribute("containerField").unwrap_or_else(|| default_container(c.tag_name().name()));
                match node.fields.iter_mut().find(|(n, _)| n == field) {
                    Some((_, Value::Nodes(list))) => list.push(child),
                    _ => node.fields.push((field.to_string(), Value::Nodes(vec![child]))),
                }
            }
        }
    }
    let def = e.attribute("DEF").map(str::to_string);
    let index = if doc.protos.contains_key(&node.kind) {
        let kind = node.kind.clone();
        doc.instantiate(&kind, &node.fields, depth)?
    } else {
        doc.push(node)?
    };
    if let Some(name) = def {
        doc.nodes[index].name = Some(name.clone());
        doc.defs.insert(name, index);
    }
    Some(index)
}

fn xml_proto(e: roxmltree::Node, doc: &mut Doc, depth: usize) {
    let Some(name) = e.attribute("name") else { return };
    let element = |n: &str| e.children().find(|c| c.is_element() && c.tag_name().name() == n);
    let mut defaults = Vec::new();
    if let Some(interface) = element("ProtoInterface") {
        for f in interface.children().filter(|c| c.is_element() && c.tag_name().name() == "field") {
            let Some(field) = f.attribute("name") else { continue };
            let value = match f.attribute("value") {
                Some(v) => attribute_value(field, v),
                None => Value::Nodes(
                    f.children().filter(|x| x.is_element()).filter_map(|x| xml_node(x, doc, depth + 1)).collect(),
                ),
            };
            defaults.push((field.to_string(), value));
        }
    }
    let outer = std::mem::take(&mut doc.defs);
    let body = element("ProtoBody")
        .and_then(|b| b.children().filter(|c| c.is_element()).find_map(|c| xml_node(c, doc, depth + 1)));
    doc.defs = outer;
    doc.protos.insert(name.to_string(), Proto { defaults, body });
}

//endregion

/*┌─────────────────╮
| Geometry        |
└─────────────────╯*/
//region Geometry

/// What a geometry node turned into, and what its material needs to know.
struct Built {
    primitives: Vec<Primitive>,
    colored: bool,
    solid: bool,
    /// Lines and points are drawn unlit.
    unlit: bool,
}

/// Splits an index list at -1 into polygons (corner counts and corners),
/// recording each kept corner's position in the list.
fn polygons(index: &[f64]) -> (Vec<u32>, Vec<u32>, Vec<usize>) {
    let (mut sizes, mut corners, mut at) = (Vec::new(), Vec::new(), Vec::new());
    let mut n = 0u32;
    for (i, &v) in index.iter().enumerate() {
        if v < 0.0 {
            if n > 0 {
                sizes.push(n);
            }
            n = 0;
            continue;
        }
        corners.push(v.min(u32::MAX as f64) as u32);
        at.push(i);
        n += 1;
    }
    if n > 0 {
        sizes.push(n);
    }
    (sizes, corners, at)
}

/// An attribute bound per vertex or per face, through an optional index
/// list shaped like `coordIndex`.
fn binding(index: &[f64], per_vertex: bool, corner_at: &[usize], faces: usize) -> Binding {
    let to_u32 = |v: f64| if v < 0.0 { u32::MAX } else { v.min(u32::MAX as f64) as u32 };
    if per_vertex {
        if index.is_empty() {
            Binding::Vertex
        } else {
            Binding::CornerIndexed(corner_at.iter().map(|&i| index.get(i).copied().map_or(u32::MAX, to_u32)).collect())
        }
    } else if index.is_empty() {
        Binding::Face
    } else {
        let face_index: Vec<u32> = index.iter().copied().filter(|&v| v >= 0.0).map(to_u32).take(faces).collect();
        Binding::FaceIndexed(face_index)
    }
}

impl Doc {
    /// Colours of a Color or ColorRGBA node, linear RGBA.
    fn colors(&self, node: Option<usize>) -> Vec<Vec4> {
        let Some(n) = node else { return Vec::new() };
        let v = self.nums(n, "color");
        if self.kind(n) == "ColorRGBA" {
            v.chunks_exact(4).map(|c| srgb_color(c[0] as f32, c[1] as f32, c[2] as f32, c[3] as f32)).collect()
        } else {
            v.chunks_exact(3).map(|c| srgb_color(c[0] as f32, c[1] as f32, c[2] as f32, 1.0)).collect()
        }
    }

    fn tex_coords(&self, node: Option<usize>) -> Vec<Vec2> {
        let Some(n) = node else { return Vec::new() };
        let n = if self.kind(n) == "MultiTextureCoordinate" { self.child(n, "texCoord").unwrap_or(n) } else { n };
        self.nums(n, "point").chunks_exact(2).map(|c| Vec2::new(c[0] as f32, 1.0 - c[1] as f32)).collect()
    }

    fn indexed_face_set(&self, g: usize, textured: bool, warnings: &mut Vec<String>) -> Built {
        let positions = self.vec3s(self.child(g, "coord"), "point");
        let (sizes, corners, corner_at) = polygons(self.nums(g, "coordIndex"));
        let faces = sizes.len();
        let mut pm = PolyMesh { positions, face_sizes: sizes, corners, ..Default::default() };
        pm.smoothing = Smoothing::Crease(self.num(g, "creaseAngle", 0.0));
        let colors = self.colors(self.child(g, "color"));
        if !colors.is_empty() {
            let b = binding(self.nums(g, "colorIndex"), self.boolean(g, "colorPerVertex", true), &corner_at, faces);
            pm.colors = Some(Attr::new(colors, b));
        }
        let normals = self.vec3s(self.child(g, "normal"), "vector");
        if !normals.is_empty() {
            let b = binding(self.nums(g, "normalIndex"), self.boolean(g, "normalPerVertex", true), &corner_at, faces);
            pm.normals = Some(Attr::new(normals, b));
        }
        let uvs = self.tex_coords(self.child(g, "texCoord"));
        if !uvs.is_empty() {
            pm.uvs = Some(Attr::new(uvs, binding(self.nums(g, "texCoordIndex"), true, &corner_at, faces)));
        } else if textured {
            pm.uvs = Some(Attr::new(default_tex_coords(&pm.positions), Binding::Vertex));
        }
        self.finish_polymesh(g, pm, warnings)
    }

    fn finish_polymesh(&self, g: usize, mut pm: PolyMesh, warnings: &mut Vec<String>) -> Built {
        if !self.boolean(g, "ccw", true) {
            pm.flip_winding();
        }
        let colored = pm.colors.is_some();
        let primitives = pm.build(warnings).into_iter().map(|(_, p)| p).collect();
        Built { primitives, colored, solid: self.boolean(g, "solid", true), unlit: false }
    }

    /// X3D's triangle, fan, strip and quad sets, whose attributes follow
    /// the vertex numbering.
    fn triangle_sets(&self, g: usize, textured: bool, warnings: &mut Vec<String>) -> Built {
        let kind = self.kind(g);
        let positions = self.vec3s(self.child(g, "coord"), "point");
        let mut pm = PolyMesh { positions, smoothing: Smoothing::Crease(std::f32::consts::PI), ..Default::default() };
        let index: Vec<u32> = match kind {
            "TriangleSet" | "TriangleFanSet" | "TriangleStripSet" | "QuadSet" => {
                (0..pm.positions.len() as u32).collect()
            }
            _ => self.nums(g, "index").iter().map(|&v| if v < 0.0 { u32::MAX } else { v as u32 }).collect(),
        };
        let counts: Vec<usize> = match kind {
            "TriangleFanSet" => self.nums(g, "fanCount").iter().map(|&c| c.max(0.0) as usize).collect(),
            "TriangleStripSet" => self.nums(g, "stripCount").iter().map(|&c| c.max(0.0) as usize).collect(),
            _ => Vec::new(),
        };
        // Runs separated by -1 (indexed fans and strips) or by counts.
        let runs: Vec<&[u32]> = if counts.is_empty() {
            index.split(|&i| i == u32::MAX).filter(|r| !r.is_empty()).collect()
        } else {
            let mut out = Vec::new();
            let mut at = 0usize;
            for c in counts {
                let end = at.saturating_add(c).min(index.len());
                out.push(&index[at..end]);
                at = end;
            }
            out
        };
        for run in runs {
            match kind {
                "IndexedTriangleFanSet" | "TriangleFanSet" => {
                    for k in 1..run.len().saturating_sub(1) {
                        pm.add_face(&[run[0], run[k], run[k + 1]]);
                    }
                }
                "IndexedTriangleStripSet" | "TriangleStripSet" => {
                    for k in 0..run.len().saturating_sub(2) {
                        if k % 2 == 0 {
                            pm.add_face(&[run[k], run[k + 1], run[k + 2]]);
                        } else {
                            pm.add_face(&[run[k + 1], run[k], run[k + 2]]);
                        }
                    }
                }
                "IndexedQuadSet" | "QuadSet" => run.chunks_exact(4).for_each(|q| pm.add_face(q)),
                _ => run.chunks_exact(3).for_each(|t| pm.add_face(t)),
            }
        }
        let colors = self.colors(self.child(g, "color"));
        if !colors.is_empty() {
            let per_vertex = self.boolean(g, "colorPerVertex", true);
            pm.colors = Some(Attr::new(colors, if per_vertex { Binding::Vertex } else { Binding::Face }));
        }
        let normals = self.vec3s(self.child(g, "normal"), "vector");
        if !normals.is_empty() {
            let per_vertex = self.boolean(g, "normalPerVertex", true);
            pm.normals = Some(Attr::new(normals, if per_vertex { Binding::Vertex } else { Binding::Face }));
        }
        let uvs = self.tex_coords(self.child(g, "texCoord"));
        if !uvs.is_empty() {
            pm.uvs = Some(Attr::new(uvs, Binding::Vertex));
        } else if textured {
            pm.uvs = Some(Attr::new(default_tex_coords(&pm.positions), Binding::Vertex));
        }
        self.finish_polymesh(g, pm, warnings)
    }

    fn elevation_grid(&self, g: usize, textured: bool, warnings: &mut Vec<String>) -> Built {
        let nx = self.num(g, "xDimension", 0.0).max(0.0) as usize;
        let nz = self.num(g, "zDimension", 0.0).max(0.0) as usize;
        let (dx, dz) = (self.num(g, "xSpacing", 1.0), self.num(g, "zSpacing", 1.0));
        let heights = self.nums(g, "height");
        let mut pm = PolyMesh::default();
        if nx >= 2 && nz >= 2 && nx.saturating_mul(nz) <= heights.len() {
            for j in 0..nz {
                for i in 0..nx {
                    pm.positions.push(Vec3::new(i as f32 * dx, heights[j * nx + i] as f32, j as f32 * dz));
                }
            }
            for j in 0..nz - 1 {
                for i in 0..nx - 1 {
                    let a = (j * nx + i) as u32;
                    let (b, c, d) = (a + nx as u32, a + nx as u32 + 1, a + 1);
                    pm.add_face(&[a, b, c, d]);
                }
            }
        }
        pm.smoothing = Smoothing::Crease(self.num(g, "creaseAngle", 0.0));
        let colors = self.colors(self.child(g, "color"));
        if !colors.is_empty() {
            let per_vertex = self.boolean(g, "colorPerVertex", true);
            pm.colors = Some(Attr::new(colors, if per_vertex { Binding::Vertex } else { Binding::Face }));
        }
        let normals = self.vec3s(self.child(g, "normal"), "vector");
        if !normals.is_empty() {
            let per_vertex = self.boolean(g, "normalPerVertex", true);
            pm.normals = Some(Attr::new(normals, if per_vertex { Binding::Vertex } else { Binding::Face }));
        }
        let uvs = self.tex_coords(self.child(g, "texCoord"));
        if !uvs.is_empty() {
            pm.uvs = Some(Attr::new(uvs, Binding::Vertex));
        } else if textured && nx >= 2 && nz >= 2 {
            let uv = (0..nx * nz)
                .map(|k| Vec2::new((k % nx) as f32 / (nx - 1) as f32, 1.0 - (k / nx) as f32 / (nz - 1) as f32))
                .collect();
            pm.uvs = Some(Attr::new(uv, Binding::Vertex));
        }
        self.finish_polymesh(g, pm, warnings)
    }

    /// A cross-section swept along a spine, scaled and turned per spine
    /// point, as the VRML specification's spine-aligned cross-section
    /// planes say.
    fn extrusion(&self, g: usize, warnings: &mut Vec<String>) -> Built {
        let pairs = |name: &str, default: &[f64]| -> Vec<Vec2> {
            let v = match self.field(g, name) {
                Some(Value::Nums(v)) => v.as_slice(),
                _ => default,
            };
            v.chunks_exact(2).map(|c| Vec2::new(c[0] as f32, c[1] as f32)).collect()
        };
        let cross = pairs("crossSection", &[1.0, 1.0, 1.0, -1.0, -1.0, -1.0, -1.0, 1.0, 1.0, 1.0]);
        let spine: Vec<Vec3> = match self.field(g, "spine") {
            Some(Value::Nums(v)) => {
                v.chunks_exact(3).map(|c| Vec3::new(c[0] as f32, c[1] as f32, c[2] as f32)).collect()
            }
            _ => vec![Vec3::ZERO, Vec3::Y],
        };
        let scales = pairs("scale", &[1.0, 1.0]);
        let orientations: Vec<Quat> =
            self.nums(g, "orientation").chunks_exact(4).map(|c| axis_angle(&[c[0], c[1], c[2], c[3]])).collect();
        let mut pm = PolyMesh::default();
        let (ns, nc) = (spine.len(), cross.len());
        if ns < 2 || nc < 2 || ns.saturating_mul(nc) > 4_000_000 {
            return Built { primitives: Vec::new(), colored: false, solid: true, unlit: false };
        }
        // 1. A frame per spine point.
        let closed = spine[0].distance(spine[ns - 1]) < 1e-6;
        let mut last_z = Vec3::Z;
        for i in 0..ns {
            let prev = if i > 0 {
                spine[i - 1]
            } else if closed {
                spine[ns - 2]
            } else {
                spine[0]
            };
            let next = if i + 1 < ns {
                spine[i + 1]
            } else if closed {
                spine[1]
            } else {
                spine[ns - 1]
            };
            let y = (next - prev).try_normalize().unwrap_or(Vec3::Y);
            let z = if i > 0 && i + 1 < ns || closed {
                (next - spine[i]).cross(prev - spine[i]).try_normalize()
            } else {
                None
            };
            let mut z = z.unwrap_or(last_z);
            if z.dot(last_z) < 0.0 {
                z = -z;
            }
            let z = if z.cross(y).length_squared() < 1e-12 { y.any_orthonormal_vector() } else { z };
            last_z = z;
            let x = y.cross(z).normalize_or_zero();
            let z = x.cross(y).normalize_or_zero();
            let r = orientations.get(i).or(orientations.last()).copied().unwrap_or(Quat::IDENTITY);
            let s = scales.get(i).or(scales.last()).copied().unwrap_or(Vec2::ONE);
            let frame = Mat3::from_cols(x, y, z);
            for c in &cross {
                let local = r * Vec3::new(c.x * s.x, 0.0, c.y * s.y);
                pm.positions.push(spine[i] + frame * local);
            }
        }
        // 2. Sides, and caps from the cross-section's own polygon.
        for i in 0..ns - 1 {
            for k in 0..nc - 1 {
                let a = (i * nc + k) as u32;
                pm.add_face(&[a, a + 1, a + 1 + nc as u32, a + nc as u32]);
            }
        }
        let cap_len = if cross[0].distance(cross[nc - 1]) < 1e-6 { nc - 1 } else { nc };
        if self.boolean(g, "beginCap", true) && cap_len >= 3 {
            let cap: Vec<u32> = (0..cap_len as u32).collect();
            pm.add_face(&cap);
        }
        if self.boolean(g, "endCap", true) && cap_len >= 3 {
            let base = ((ns - 1) * nc) as u32;
            let cap: Vec<u32> = (0..cap_len as u32).rev().map(|k| base + k).collect();
            pm.add_face(&cap);
        }
        pm.smoothing = Smoothing::Crease(self.num(g, "creaseAngle", 0.0));
        let mut built = self.finish_polymesh(g, pm, warnings);
        // Which way the sides face depends on the cross-section's turn, so
        // extrusions are drawn from both sides.
        built.solid = false;
        built
    }

    fn lines_or_points(&self, g: usize) -> Built {
        let kind = self.kind(g);
        let positions = self.vec3s(self.child(g, "coord"), "point");
        let colors = self.colors(self.child(g, "color"));
        let mut p = Primitive {
            topology: if kind == "PointSet" { Topology::Points } else { Topology::Lines },
            ..Default::default()
        };
        let per_vertex = self.boolean(g, "colorPerVertex", true);
        let color_index = self.nums(g, "colorIndex");
        let push = |p: &mut Primitive, v: u32, line: usize, corner: usize| -> Option<u32> {
            let pos = *positions.get(v as usize)?;
            if !pos.is_finite() {
                return None;
            }
            p.positions.push(pos);
            if !colors.is_empty() {
                let ci = if per_vertex {
                    color_index.get(corner).map_or(v as usize, |&c| c.max(0.0) as usize)
                } else {
                    color_index.get(line).map_or(line, |&c| c.max(0.0) as usize)
                };
                p.colors.push(colors.get(ci).copied().unwrap_or(Vec4::ONE));
            }
            Some((p.positions.len() - 1) as u32)
        };
        match kind {
            "PointSet" => {
                for v in 0..positions.len() as u32 {
                    if let Some(i) = push(&mut p, v, 0, v as usize) {
                        p.indices.push(i);
                    }
                }
            }
            "LineSet" => {
                let mut at = 0u32;
                for (line, &count) in self.nums(g, "vertexCount").iter().enumerate() {
                    let count = count.max(0.0).min(positions.len() as f64) as u32;
                    let mut prev = None;
                    for v in at..at.saturating_add(count) {
                        let cur = push(&mut p, v, line, v as usize);
                        if let (Some(a), Some(b)) = (prev, cur) {
                            p.indices.extend([a, b]);
                        }
                        prev = cur;
                    }
                    at = at.saturating_add(count);
                }
            }
            _ => {
                let index = self.nums(g, "coordIndex");
                let (mut line, mut prev) = (0usize, None);
                for (corner, &v) in index.iter().enumerate() {
                    if v < 0.0 {
                        line += 1;
                        prev = None;
                        continue;
                    }
                    let cur = push(&mut p, v.min(u32::MAX as f64) as u32, line, corner);
                    if let (Some(a), Some(b)) = (prev, cur) {
                        p.indices.extend([a, b]);
                    }
                    prev = cur;
                }
            }
        }
        let colored = !p.colors.is_empty();
        let primitives = if p.indices.is_empty() { Vec::new() } else { vec![p] };
        Built { primitives, colored, solid: false, unlit: true }
    }

    fn geometry(&self, g: usize, textured: bool, warnings: &mut Vec<String>) -> Option<Built> {
        let prim = |p: Primitive, solid: bool| Built { primitives: vec![p], colored: false, solid, unlit: false };
        Some(match self.kind(g) {
            "IndexedFaceSet" => self.indexed_face_set(g, textured, warnings),
            "IndexedTriangleSet"
            | "TriangleSet"
            | "IndexedTriangleFanSet"
            | "IndexedTriangleStripSet"
            | "TriangleFanSet"
            | "TriangleStripSet"
            | "IndexedQuadSet"
            | "QuadSet" => self.triangle_sets(g, textured, warnings),
            "ElevationGrid" => self.elevation_grid(g, textured, warnings),
            "Extrusion" => self.extrusion(g, warnings),
            "IndexedLineSet" | "LineSet" | "PointSet" => self.lines_or_points(g),
            "Box" => prim(make_box(self.vec3(g, "size", Vec3::splat(2.0))), self.boolean(g, "solid", true)),
            "Sphere" => prim(make_sphere(self.num(g, "radius", 1.0)), self.boolean(g, "solid", true)),
            "Cylinder" => prim(
                make_cylinder(
                    self.num(g, "radius", 1.0),
                    self.num(g, "radius", 1.0),
                    self.num(g, "height", 2.0),
                    self.boolean(g, "side", true),
                    self.boolean(g, "bottom", true),
                    self.boolean(g, "top", true),
                ),
                self.boolean(g, "solid", true),
            ),
            "Cone" => prim(
                make_cylinder(
                    self.num(g, "bottomRadius", 1.0),
                    0.0,
                    self.num(g, "height", 2.0),
                    self.boolean(g, "side", true),
                    self.boolean(g, "bottom", true),
                    false,
                ),
                self.boolean(g, "solid", true),
            ),
            _ => return None,
        })
    }
}

/// VRML's default texture coordinates: S along the longest side of the
/// bounding box, T along the second longest, at the same scale.
fn default_tex_coords(positions: &[Vec3]) -> Vec<Vec2> {
    let mut b = slerp_scene::Aabb::EMPTY;
    positions.iter().filter(|p| p.is_finite()).for_each(|&p| b.add(p));
    if b.is_empty() {
        return vec![Vec2::ZERO; positions.len()];
    }
    let size = b.size();
    let mut axes = [0usize, 1, 2];
    axes.sort_by(|&a, &c| size[c].total_cmp(&size[a]));
    let (s, t) = (axes[0], axes[1]);
    let scale = if size[s] > 0.0 { 1.0 / size[s] } else { 1.0 };
    positions.iter().map(|p| Vec2::new((p[s] - b.min[s]) * scale, 1.0 - (p[t] - b.min[t]) * scale)).collect()
}

fn make_box(size: Vec3) -> Primitive {
    let h = size.abs() * 0.5;
    let mut p = Primitive::default();
    for (n, u, v) in [
        (Vec3::Z, Vec3::X, Vec3::Y),
        (Vec3::NEG_Z, Vec3::NEG_X, Vec3::Y),
        (Vec3::X, Vec3::NEG_Z, Vec3::Y),
        (Vec3::NEG_X, Vec3::Z, Vec3::Y),
        (Vec3::Y, Vec3::X, Vec3::NEG_Z),
        (Vec3::NEG_Y, Vec3::X, Vec3::Z),
    ] {
        let base = p.positions.len() as u32;
        for (s, t) in [(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0)] {
            let corner = n + u * (s * 2.0 - 1.0) + v * (t * 2.0 - 1.0);
            p.positions.push(corner * h);
            p.normals.push(n);
            p.uv0.push(Vec2::new(s, 1.0 - t));
        }
        p.indices.extend([base, base + 1, base + 2, base, base + 2, base + 3]);
    }
    p
}

fn make_sphere(radius: f32) -> Primitive {
    let mut p = Primitive::default();
    let rings = SEGMENTS / 2;
    for r in 0..=rings {
        let theta = r as f32 / rings as f32 * std::f32::consts::PI;
        for s in 0..=SEGMENTS {
            let u = s as f32 / SEGMENTS as f32;
            // VRML's seam is at -Z, with S increasing counter-clockwise
            // seen from above.
            let phi = u * std::f32::consts::TAU;
            let n = Vec3::new(-phi.sin() * theta.sin(), theta.cos(), -phi.cos() * theta.sin());
            p.positions.push(n * radius.abs());
            p.normals.push(n);
            p.uv0.push(Vec2::new(u, r as f32 / rings as f32));
        }
    }
    let w = SEGMENTS as u32 + 1;
    for r in 0..rings as u32 {
        for s in 0..SEGMENTS as u32 {
            let a = r * w + s;
            p.indices.extend([a, a + w, a + 1, a + 1, a + w, a + w + 1]);
        }
    }
    p
}

/// A cylinder, or a cone when the top radius is 0, centred on the origin.
fn make_cylinder(bottom: f32, top: f32, height: f32, side: bool, cap_bottom: bool, cap_top: bool) -> Primitive {
    let mut p = Primitive::default();
    let h = height.abs() * 0.5;
    let (rb, rt) = (bottom.abs(), top.abs());
    let slope = (rb - rt) / height.abs().max(1e-12);
    let dir = |s: usize| {
        let phi = s as f32 / SEGMENTS as f32 * std::f32::consts::TAU;
        Vec3::new(-phi.sin(), 0.0, -phi.cos())
    };
    if side {
        let base = p.positions.len() as u32;
        for s in 0..=SEGMENTS {
            let d = dir(s);
            let n = (d + Vec3::Y * slope).normalize();
            let u = s as f32 / SEGMENTS as f32;
            p.positions.push(d * rb - Vec3::Y * h);
            p.positions.push(d * rt + Vec3::Y * h);
            p.normals.extend([n, n]);
            p.uv0.extend([Vec2::new(u, 1.0), Vec2::new(u, 0.0)]);
        }
        for s in 0..SEGMENTS as u32 {
            let a = base + 2 * s;
            p.indices.extend([a, a + 2, a + 1, a + 1, a + 2, a + 3]);
        }
    }
    for (cap, r, y, n) in [(cap_bottom, rb, -h, Vec3::NEG_Y), (cap_top, rt, h, Vec3::Y)] {
        if !cap || r <= 0.0 {
            continue;
        }
        let base = p.positions.len() as u32;
        p.positions.push(Vec3::Y * y);
        p.normals.push(n);
        p.uv0.push(Vec2::splat(0.5));
        for s in 0..SEGMENTS {
            let d = dir(s);
            p.positions.push(d * r + Vec3::Y * y);
            p.normals.push(n);
            p.uv0.push(Vec2::new(0.5 + d.x * 0.5, 0.5 + d.z * 0.5 * n.y));
        }
        for s in 0..SEGMENTS as u32 {
            let (a, b) = (base + 1 + s, base + 1 + (s + 1) % SEGMENTS as u32);
            if n.y > 0.0 { p.indices.extend([base, a, b]) } else { p.indices.extend([base, b, a]) }
        }
    }
    p
}

//endregion

/*┌─────────────────╮
| Scene           |
└─────────────────╯*/
//region Scene

/// An axis-angle rotation from four numbers.
fn axis_angle(v: &[f64]) -> Quat {
    let axis = Vec3::new(v[0] as f32, v[1] as f32, v[2] as f32);
    match axis.try_normalize() {
        Some(a) if v[3].is_finite() => Quat::from_axis_angle(a, v[3] as f32),
        _ => Quat::IDENTITY,
    }
}

/// Grouping nodes whose children are drawn, and the fields they're in.
fn child_fields(kind: &str) -> &'static [&'static str] {
    match kind {
        "Transform" | "Group" | "StaticGroup" | "Anchor" | "Billboard" | "Collision" | "CADAssembly" | "CADLayer"
        | "CADPart" | "HAnimJoint" | "HAnimSegment" | "HAnimSite" | "LOD" | "Switch" => {
            &["children", "level", "choice"]
        }
        "CADFace" => &["shape"],
        "HAnimHumanoid" => &["skeleton", "skin", "children"],
        "Inline" | "Shape" | "Scene" => &[],
        _ => &["children"],
    }
}

/// The lower half of a VRML transform split in two, SR × S × −SR × −C, in
/// a node of its own.  Animated scale orientations aren't followed.
#[derive(Clone, Copy)]
struct Pivot {
    node: usize,
    centre: Vec3,
    orientation: Quat,
}

impl Pivot {
    /// The pivot node's transform for scale `s`.
    fn transform(&self, s: Vec3) -> Transform {
        let sr = self.orientation;
        let m = Mat4::from_quat(sr)
            * Mat4::from_scale(s)
            * Mat4::from_quat(sr.inverse())
            * Mat4::from_translation(-self.centre);
        if m.is_finite() { Transform::from_matrix(m) } else { Transform::IDENTITY }
    }
}

/// Builds the scene for one or more parsed documents (the main file and
/// its inlines).
struct Builder<'a> {
    docs: Vec<Doc>,
    limits: &'a Limits,
    textures: TextureSet<'a>,
    scene: Scene,
    meshes: HashMap<(usize, usize), Option<usize>>,
    materials: HashMap<(usize, Option<usize>, u8), usize>,
    /// The scene nodes each document node became, for routes.
    instances: HashMap<(usize, usize), Vec<usize>>,
    /// Transforms split in two (see `transform_parts`), by their outer node.
    pivots: HashMap<usize, Pivot>,
    triangles: u64,
    inlines: usize,
    warnings: Vec<String>,
}

impl Builder<'_> {
    fn material(&mut self, doc: usize, appearance: Option<usize>, built: &Built, textured_hint: bool) -> usize {
        let flags =
            built.colored as u8 | (!built.solid as u8) << 1 | (built.unlit as u8) << 2 | (textured_hint as u8) << 3;
        if let Some(&m) = self.materials.get(&(doc, appearance, flags)) {
            return m;
        }
        let d = &self.docs[doc];
        let mut m = Material { double_sided: !built.solid, ..Material::default() };
        let material = appearance.and_then(|a| d.child(a, "material"));
        if let Some(mat) = material {
            m.name = d.nodes[mat].name.clone().unwrap_or_default();
            let transparency = d.num(mat, "transparency", 0.0).clamp(0.0, 1.0);
            match d.kind(mat) {
                "PhysicalMaterial" => {
                    let c = d.vec3(mat, "baseColor", Vec3::ONE);
                    m.base_color = srgb_color(c.x, c.y, c.z, 1.0 - transparency);
                    m.metallic = d.num(mat, "metallic", 1.0).clamp(0.0, 1.0);
                    m.roughness = d.num(mat, "roughness", 1.0).clamp(0.0, 1.0);
                }
                "UnlitMaterial" => {
                    let c = d.vec3(mat, "emissiveColor", Vec3::ONE);
                    m.base_color = srgb_color(c.x, c.y, c.z, 1.0 - transparency);
                    m.unlit = true;
                }
                _ => {
                    let c = d.vec3(mat, "diffuseColor", Vec3::splat(0.8));
                    m.base_color = srgb_color(c.x, c.y, c.z, 1.0 - transparency);
                    let shininess = d.num(mat, "shininess", 0.2).clamp(0.0, 1.0);
                    m.roughness = (2.0 / (shininess * 128.0 + 2.0)).sqrt().clamp(0.05, 1.0);
                    m.specular = d.vec3(mat, "specularColor", Vec3::ZERO).max_element().clamp(0.0, 1.0);
                }
            }
            let e = d.vec3(mat, "emissiveColor", Vec3::ZERO);
            if d.kind(mat) != "UnlitMaterial" {
                m.emissive = srgb_color(e.x, e.y, e.z, 1.0).truncate();
            }
            if built.unlit {
                // Lines and points take their colour from emissiveColor.
                m.base_color = srgb_color(e.x, e.y, e.z, m.base_color.w);
            }
        }
        m.unlit |= built.unlit;
        if built.colored {
            m.base_color = Vec4::new(1.0, 1.0, 1.0, m.base_color.w);
        }
        // Textures replace the diffuse colour.
        let texture = appearance.and_then(|a| {
            let t = d.child(a, "texture").or_else(|| material.and_then(|mat| d.child(mat, "baseTexture")))?;
            Some(if d.kind(t) == "MultiTexture" { d.child(t, "texture").unwrap_or(t) } else { t })
        });
        let transform = appearance.and_then(|a| d.child(a, "textureTransform")).map(|t| {
            let c = d.nums(t, "center");
            let c = Vec2::new(*c.first().unwrap_or(&0.0) as f32, *c.get(1).unwrap_or(&0.0) as f32);
            let s = d.nums(t, "scale");
            let s = Vec2::new(*s.first().unwrap_or(&1.0) as f32, *s.get(1).unwrap_or(&1.0) as f32);
            let tr = d.nums(t, "translation");
            let tr = Vec2::new(*tr.first().unwrap_or(&0.0) as f32, *tr.get(1).unwrap_or(&0.0) as f32);
            let r = d.num(t, "rotation", 0.0);
            Mat3::from_translation(-c)
                * Mat3::from_scale(s)
                * Mat3::from_angle(r)
                * Mat3::from_translation(c)
                * Mat3::from_translation(tr)
        });
        if let Some(t) = texture
            && let Some(index) = self.texture(doc, t)
        {
            let d = &self.docs[doc];
            let mut tex = TexRef::new(index);
            if !d.boolean(t, "repeatS", true) {
                tex.sampler.wrap_u = Wrap::ClampToEdge;
            }
            if !d.boolean(t, "repeatT", true) {
                tex.sampler.wrap_v = Wrap::ClampToEdge;
            }
            tex.transform = transform.and_then(flipped_uv_transform);
            m.base_color_texture = Some(tex);
            m.base_color = Vec4::new(1.0, 1.0, 1.0, m.base_color.w);
            if self.textures.get(index).is_some_and(|t| t.has_alpha) {
                m.alpha_mode = AlphaMode::Blend;
            }
        }
        if m.base_color.w < 0.999 {
            m.alpha_mode = AlphaMode::Blend;
        }
        self.scene.materials.push(m);
        let index = self.scene.materials.len() - 1;
        self.materials.insert((doc, appearance, flags), index);
        index
    }

    fn texture(&mut self, doc: usize, t: usize) -> Option<usize> {
        let d = &self.docs[doc];
        match d.kind(t) {
            "ImageTexture" => {
                let urls: Vec<String> = d.strings(t, "url").to_vec();
                let dir = d.dir.clone();
                for url in &urls {
                    if url.contains("://") && !url.starts_with("file://") {
                        continue;
                    }
                    let name =
                        if dir.is_empty() || url.starts_with('/') { url.clone() } else { format!("{dir}/{url}") };
                    if let Some(i) = self.textures.file_quiet(&name) {
                        return Some(i);
                    }
                }
                if let Some(first) = urls.first() {
                    self.textures.missing(first);
                }
                None
            }
            "PixelTexture" => {
                let v = d.nums(t, "image");
                let (w, h, comps) = match v {
                    [w, h, c, ..] => (*w as usize, *h as usize, *c as usize),
                    _ => return None,
                };
                if w == 0
                    || h == 0
                    || !(1..=4).contains(&comps)
                    || w.checked_mul(h)? > v.len() - 3
                    || w > 16384
                    || h > 16384
                {
                    return None;
                }
                let mut px = vec![0u8; w * h * 4];
                for y in 0..h {
                    for x in 0..w {
                        let value = v[3 + y * w + x] as i64 as u32;
                        let bytes: [u8; 4] = match comps {
                            1 => [value as u8, value as u8, value as u8, 255],
                            2 => [(value >> 8) as u8, (value >> 8) as u8, (value >> 8) as u8, value as u8],
                            3 => [(value >> 16) as u8, (value >> 8) as u8, value as u8, 255],
                            _ => [(value >> 24) as u8, (value >> 16) as u8, (value >> 8) as u8, value as u8],
                        };
                        // The first pixel is the bottom-left one.
                        let at = ((h - 1 - y) * w + x) * 4;
                        px[at..at + 4].copy_from_slice(&bytes);
                    }
                }
                Some(self.textures.push(Texture::from_rgba8(w as u32, h as u32, px, ColorSpace::Srgb)))
            }
            _ => None,
        }
    }

    /// The mesh of a Shape node, built once per document node.
    fn shape_mesh(&mut self, doc: usize, shape: usize) -> Option<usize> {
        if let Some(&m) = self.meshes.get(&(doc, shape)) {
            return m;
        }
        let d = &self.docs[doc];
        let geometry = d.child(shape, "geometry");
        let appearance = d.child(shape, "appearance");
        let textured = appearance.is_some_and(|a| d.child(a, "texture").is_some());
        let built = geometry.and_then(|g| d.geometry(g, textured, &mut self.warnings));
        let result = match built {
            Some(built) if !built.primitives.is_empty() => {
                let count: u64 = built.primitives.iter().map(|p| p.triangle_count() as u64).sum();
                self.triangles += count;
                let material = self.material(doc, appearance, &built, textured);
                let name = geometry.and_then(|g| self.docs[doc].nodes[g].name.clone()).unwrap_or_default();
                let primitives = built
                    .primitives
                    .into_iter()
                    .map(|mut p| {
                        p.material = Some(material);
                        p
                    })
                    .collect();
                self.scene.meshes.push(Mesh { name, primitives, target_names: Vec::new() });
                Some(self.scene.meshes.len() - 1)
            }
            _ => None,
        };
        self.meshes.insert((doc, shape), result);
        result
    }

    /// Adds scene nodes for document node `n` under `parent`.
    fn visit(&mut self, doc: usize, n: usize, parent: Option<usize>, depth: usize) -> Result<(), LoadError> {
        if depth > MAX_DEPTH || self.scene.nodes.len() >= MAX_NODES || n >= self.docs[doc].nodes.len() {
            return Ok(());
        }
        check_triangles(self.triangles, self.limits)?;
        let d = &self.docs[doc];
        let kind = d.kind(n).to_string();
        let name = d.nodes[n].name.clone();
        let attach = |b: &mut Self, node: Node| -> usize {
            b.scene.nodes.push(node);
            let index = b.scene.nodes.len() - 1;
            if let Some(p) = parent {
                b.scene.nodes[p].children.push(index);
            }
            b.instances.entry((doc, n)).or_default().push(index);
            index
        };
        match kind.as_str() {
            "Shape" => {
                if let Some(mesh) = self.shape_mesh(doc, n) {
                    let mut node = Node::new(name.unwrap_or_else(|| "Shape".into()));
                    node.mesh = Some(mesh);
                    attach(self, node);
                }
            }
            "Inline" => {
                let node = Node::new(name.unwrap_or_else(|| "Inline".into()));
                let index = attach(self, node);
                if let Some(child_doc) = self.inline(doc, n) {
                    let roots = self.docs[child_doc].roots.clone();
                    for r in roots {
                        self.visit(child_doc, r, Some(index), depth + 1)?;
                    }
                }
            }
            _ => {
                let d = &self.docs[doc];
                let fields = child_fields(&kind);
                if fields.is_empty() && kind != "Scene" {
                    return Ok(());
                }
                let mut children: Vec<usize> = Vec::new();
                for f in fields {
                    children.extend_from_slice(d.nodes(n, f));
                }
                match kind.as_str() {
                    "Switch" => {
                        let which = d.num(n, "whichChoice", -1.0);
                        children = if which >= 0.0 {
                            children.get(which as usize).copied().into_iter().collect()
                        } else {
                            Vec::new()
                        };
                    }
                    "LOD" => children.truncate(1),
                    _ => {}
                }
                if children.is_empty() && !d.nodes[n].fields.iter().any(|(f, _)| f == "translation" || f == "rotation")
                {
                    return Ok(());
                }
                let label = name.unwrap_or_else(|| kind.clone());
                let mut node = Node::new(label.clone());
                let (outer, inner) = self.transform_parts(doc, n);
                node.transform = outer;
                let mut index = attach(self, node);
                // The centre and scale orientation sit in a node of their
                // own below, so animating the rotation and translation
                // fields replaces exactly what they replace in VRML.
                if let Some(pivot) = inner {
                    let mut child = Node::new(format!("{label} pivot"));
                    child.transform = pivot.transform(self.docs[doc].vec3(n, "scale", Vec3::ONE));
                    self.scene.nodes.push(child);
                    let p = self.scene.nodes.len() - 1;
                    self.scene.nodes[index].children.push(p);
                    self.pivots.insert(index, Pivot { node: p, ..pivot });
                    index = p;
                }
                for c in children {
                    self.visit(doc, c, Some(index), depth + 1)?;
                }
            }
        }
        Ok(())
    }

    /// VRML's transform, T × C × R × SR × S × −SR × −C, as a node
    /// transform: T × R × S when there is no centre or scale orientation,
    /// otherwise T × C × R with a pivot for the rest.
    fn transform_parts(&self, doc: usize, n: usize) -> (Transform, Option<Pivot>) {
        let d = &self.docs[doc];
        let t = d.vec3(n, "translation", Vec3::ZERO);
        let c = d.vec3(n, "center", Vec3::ZERO);
        let r = match d.nums(n, "rotation") {
            [x, y, z, a, ..] => axis_angle(&[*x, *y, *z, *a]),
            _ => Quat::IDENTITY,
        };
        let s = d.vec3(n, "scale", Vec3::ONE);
        let sr = match d.nums(n, "scaleOrientation") {
            [x, y, z, a, ..] => axis_angle(&[*x, *y, *z, *a]),
            _ => Quat::IDENTITY,
        };
        if !(t.is_finite() && c.is_finite() && s.is_finite()) {
            return (Transform::IDENTITY, None);
        }
        let plain_scale = sr.angle_between(Quat::IDENTITY) < 1e-6 || (s - Vec3::splat(s.x)).abs().max_element() < 1e-6;
        if c == Vec3::ZERO && plain_scale {
            return (Transform { translation: t, rotation: r, scale: s }, None);
        }
        let pivot = Pivot { node: usize::MAX, centre: c, orientation: sr };
        (Transform { translation: t + c, rotation: r, scale: Vec3::ONE }, Some(pivot))
    }

    /// Reads the file an Inline names, as a new document.
    fn inline(&mut self, doc: usize, n: usize) -> Option<usize> {
        if !self.limits.neighbours || self.inlines >= MAX_INLINES {
            return None;
        }
        let urls = self.docs[doc].strings(n, "url").to_vec();
        let dir = self.docs[doc].dir.clone();
        for url in urls {
            if url.contains("://") && !url.starts_with("file://") {
                continue;
            }
            let name = if dir.is_empty() || url.starts_with('/') { url.clone() } else { format!("{dir}/{url}") };
            let Some((_, bytes)) = self.textures.read_neighbour(&name) else { continue };
            self.inlines += 1;
            let mut child = Doc {
                dir: name.rsplit_once('/').map(|(d, _)| d.to_string()).unwrap_or_default(),
                ..Default::default()
            };
            match read_doc(&bytes, self.limits, &mut child) {
                Ok(()) => {
                    self.warnings.append(&mut child.warnings);
                    self.docs.push(child);
                    return Some(self.docs.len() - 1);
                }
                Err(e) => self.warnings.push(format!("Inline {name}: {e}")),
            }
        }
        self.warnings.push(format!("Inline not loaded: {}", self.docs[doc].strings(n, "url").join(" ")));
        None
    }

    /// Clips from TimeSensors routed to interpolators routed to transforms.
    fn animations(&mut self, doc: usize) {
        let d = &self.docs[doc];
        let node_named = |name: &str| d.defs.get(name).copied();
        let mut clips: Vec<(String, f32, Vec<Channel>)> = Vec::new();
        for r in &d.routes {
            if !r.from_field.ends_with("fraction_changed") || !r.to_field.ends_with("set_fraction") {
                continue;
            }
            let (Some(timer), Some(interp)) = (node_named(&r.from), node_named(&r.to)) else { continue };
            if d.kind(timer) != "TimeSensor" {
                continue;
            }
            let cycle = d.num(timer, "cycleInterval", 1.0).max(1e-3);
            let property = match d.kind(interp) {
                "PositionInterpolator" => Property::Translation,
                "OrientationInterpolator" => Property::Rotation,
                _ => continue,
            };
            let keys = d.nums(interp, "key");
            let values = d.nums(interp, "keyValue");
            let width = if property == Property::Rotation { 4 } else { 3 };
            let count = keys.len().min(values.len() / width);
            if count == 0 {
                continue;
            }
            let mut times: Vec<f32> = keys[..count].iter().map(|&k| (k as f32).clamp(0.0, 1.0) * cycle).collect();
            for i in 1..times.len() {
                if times[i] <= times[i - 1] {
                    times[i] = times[i - 1] + 1e-4;
                }
            }
            let flat: Vec<f32> = values[..count * width]
                .chunks_exact(width)
                .flat_map(|v| {
                    if width == 4 { axis_angle(v).to_array().to_vec() } else { v.iter().map(|&x| x as f32).collect() }
                })
                .collect();
            // Every transform the interpolator drives.
            for r2 in d.routes.iter().filter(|x| x.from == r.to && x.from_field.ends_with("value_changed")) {
                let Some(target) = node_named(&r2.to) else { continue };
                let wanted = match property {
                    Property::Translation => r2.to_field.ends_with("translation"),
                    _ => r2.to_field.ends_with("rotation"),
                };
                let to_scale = r2.to_field.ends_with("scale") && property == Property::Translation;
                if !wanted && !to_scale {
                    continue;
                }
                let property = if to_scale { Property::Scale } else { property };
                for &node in self.instances.get(&(doc, target)).map(Vec::as_slice).unwrap_or_default() {
                    let name = d.nodes[timer].name.clone().unwrap_or_else(|| "Animation".into());
                    let channel = |node, property, values| Channel {
                        node,
                        property,
                        interpolation: Interpolation::Linear,
                        times: times.clone(),
                        values,
                    };
                    let mut new = Vec::new();
                    match (property, self.pivots.get(&node)) {
                        // The outer node's translation includes the centre.
                        (Property::Translation, Some(p)) => {
                            let mut values = flat.clone();
                            values.chunks_exact_mut(3).for_each(|v| {
                                v[0] += p.centre.x;
                                v[1] += p.centre.y;
                                v[2] += p.centre.z;
                            });
                            new.push(channel(node, property, values));
                        }
                        // A split transform's scale lives in its pivot,
                        // whose whole transform follows from each key.
                        (Property::Scale, Some(p)) => {
                            let keys: Vec<Transform> =
                                flat.chunks_exact(3).map(|v| p.transform(Vec3::new(v[0], v[1], v[2]))).collect();
                            let parts = |f: fn(&Transform) -> Vec<f32>| keys.iter().flat_map(f).collect::<Vec<f32>>();
                            new.push(channel(
                                p.node,
                                Property::Translation,
                                parts(|k| k.translation.to_array().to_vec()),
                            ));
                            new.push(channel(p.node, Property::Rotation, parts(|k| k.rotation.to_array().to_vec())));
                            new.push(channel(p.node, Property::Scale, parts(|k| k.scale.to_array().to_vec())));
                        }
                        _ => new.push(channel(node, property, flat.clone())),
                    }
                    match clips.iter_mut().find(|c| c.0 == name) {
                        Some(c) => c.2.extend(new),
                        None => clips.push((name, cycle, new)),
                    }
                }
            }
        }
        for (name, duration, channels) in clips {
            self.scene.animations.push(Animation { name, duration, channels });
        }
    }
}

/// Parses VRML or X3D bytes (gzipped or not) into `doc`.
fn read_doc(bytes: &[u8], limits: &Limits, doc: &mut Doc) -> Result<(), LoadError> {
    let inflated;
    let bytes = if is_gzip(bytes) {
        inflated = gunzip(bytes, limits.max_file_bytes)?;
        &inflated[..]
    } else {
        bytes
    };
    match dialect(&bytes[..bytes.len().min(4096)]) {
        Some(Dialect::Vrml2) | Some(Dialect::X3dClassic) => {
            let text = crate::common::utf8_text(bytes);
            parse_classic(&text, doc);
            Ok(())
        }
        Some(Dialect::X3dXml) => parse_xml(bytes, doc),
        Some(Dialect::Vrml1) => Err(LoadError::Unsupported("VRML 1.0 (only VRML 2.0 and X3D are read)".into())),
        None => Err(LoadError::NotMine),
    }
}

pub fn load(path: &Path, bytes: &[u8], limits: &Limits) -> Result<Scene, LoadError> {
    // 1. Parse.
    let mut doc = Doc::default();
    let inflated;
    let raw = if is_gzip(bytes) {
        inflated = gunzip(bytes, limits.max_file_bytes)?;
        &inflated[..]
    } else {
        bytes
    };
    let found = dialect(&raw[..raw.len().min(4096)]).ok_or(LoadError::NotMine)?;
    read_doc(raw, limits, &mut doc)?;

    // 2. Build nodes and meshes from the roots.
    let mut b = Builder {
        docs: vec![doc],
        limits,
        textures: TextureSet::new(path, limits),
        scene: Scene::default(),
        meshes: HashMap::new(),
        materials: HashMap::new(),
        instances: HashMap::new(),
        pivots: HashMap::new(),
        triangles: 0,
        inlines: 0,
        warnings: Vec::new(),
    };
    b.warnings.append(&mut b.docs[0].warnings);
    let roots = b.docs[0].roots.clone();
    for r in roots {
        b.visit(0, r, None, 0)?;
    }
    check_triangles(b.triangles, limits)?;
    b.scene.compute_roots();
    if b.scene.meshes.is_empty() {
        return Err(LoadError::invalid("VRML/X3D scene holds no geometry Slerp draws"));
    }
    if limits.animations {
        for doc in 0..b.docs.len() {
            b.animations(doc);
        }
    }
    let mut scene = b.scene;
    scene.info.format = match found {
        Dialect::Vrml2 => "VRML 2.0".into(),
        Dialect::X3dClassic => "X3D (classic encoding)".into(),
        _ => "X3D (XML encoding)".into(),
    };
    if is_gzip(bytes) {
        scene.info.format.push_str(", gzipped");
    }
    scene.info.warnings = b.warnings;
    scene.info.warnings.extend(b.textures.warnings);
    scene.textures = b.textures.textures;
    Ok(scene)
}

//endregion

#[cfg(test)]
mod tests {
    use super::*;

    const WORLD: &str = r#"#VRML V2.0 utf8
# A red L-shaped face, a shared box, a prototype and an animation.
PROTO Brick [ field SFColor tint 0 0 1 exposedField SFVec3f where 0 0 0 ] {
  Transform { translation IS where children Shape { appearance Appearance { material Material { diffuseColor IS tint } } geometry Box { size 1 1 1 } } }
}
DEF Root Transform {
  translation 0 1 0
  children [
    Shape {
      appearance Appearance { material DEF Red Material { diffuseColor 1 0 0 } }
      geometry IndexedFaceSet {
        coord Coordinate { point [ 0 0 0, 2 0 0, 2 1 0, 1 1 0, 1 2 0, 0 2 0 ] }
        coordIndex [ 0 1 2 3 4 5 -1 ]
        color Color { color [ 1 1 0 ] }
        colorPerVertex FALSE
        convex FALSE
        solid FALSE
      }
    }
    DEF B Shape { geometry Box { size 2 2 2 } }
    Transform { translation 5 0 0 children USE B }
    Brick { tint 0 1 0 where 0 0 -5 }
  ]
}
DEF Timer TimeSensor { cycleInterval 2 loop TRUE }
DEF Mover PositionInterpolator { key [ 0 1 ] keyValue [ 0 1 0, 0 3 0 ] }
ROUTE Timer.fraction_changed TO Mover.set_fraction
ROUTE Mover.value_changed TO Root.set_translation
"#;

    #[test]
    fn loads_vrml_world() {
        assert_eq!(dialect(WORLD.as_bytes()), Some(Dialect::Vrml2));
        let s = load(Path::new("/nonexistent/w.wrl"), WORLD.as_bytes(), &Limits::viewer()).unwrap();
        assert_eq!(s.info.format, "VRML 2.0");
        // The L face is concave: four triangles.  Two boxes share a mesh;
        // the prototype adds a third box with its own material.
        let l = &s.meshes[0];
        assert_eq!(l.primitives[0].triangle_count(), 4);
        assert_eq!(s.meshes.len(), 3, "{:?}", s.meshes.iter().map(|m| &m.name).collect::<Vec<_>>());
        assert_eq!(s.nodes.iter().filter(|n| n.mesh == Some(1)).count(), 2);
        // Per-face colour replaces the material's diffuse colour.
        let red = &s.materials[l.primitives[0].material.unwrap()];
        assert_eq!(red.base_color, Vec4::ONE);
        assert!(red.double_sided);
        assert_eq!(l.primitives[0].colors[0], Vec4::new(1.0, 1.0, 0.0, 1.0));
        let green = s.materials.iter().any(|m| m.base_color == Vec4::new(0.0, 1.0, 0.0, 1.0));
        assert!(green, "the prototype's tint is applied");
        // The animation moves the root from y = 1 to y = 3 over 2 s.
        assert_eq!(s.animations.len(), 1);
        assert_eq!(s.animations[0].name, "Timer");
        assert!((s.animations[0].duration - 2.0).abs() < 1e-6);
    }

    #[test]
    fn loads_x3d_xml_and_classic() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE X3D PUBLIC "ISO//Web3D//DTD X3D 3.3//EN" "https://www.web3d.org/specifications/x3d-3.3.dtd">
<X3D profile="Interchange" version="3.3"><head><meta name="title" content="t"/></head><Scene>
  <Transform DEF="T" translation="1 0 0"><Shape><Appearance><Material diffuseColor="0 0 1"/></Appearance>
    <IndexedTriangleSet index="0 1 2"><Coordinate point="0 0 0, 1 0 0, 0 1 0"/></IndexedTriangleSet></Shape></Transform>
  <Transform translation="3 0 0"><Shape USE="missing"/><Transform USE="T"/></Transform>
</Scene></X3D>"#;
        assert_eq!(dialect(xml.as_bytes()), Some(Dialect::X3dXml));
        let s = load(Path::new("a.x3d"), xml.as_bytes(), &Limits::viewer()).unwrap();
        assert_eq!(s.triangle_count(), 1);
        assert_eq!(s.nodes.iter().filter(|n| n.mesh.is_some()).count(), 2);
        assert_eq!(s.materials[0].base_color, Vec4::new(0.0, 0.0, 1.0, 1.0));

        let classic = "#X3D V3.3 utf8\nPROFILE Interchange\nCOMPONENT Geometry3D:2\nMETA \"title\" \"t\"\nShape { geometry Sphere { radius 2 } }\n";
        let s = load(Path::new("a.x3dv"), classic.as_bytes(), &Limits::viewer()).unwrap();
        assert_eq!(s.info.format, "X3D (classic encoding)");
        let b = s.bounds(&slerp_scene::Pose::rest(&s));
        assert!((b.max.y - 2.0).abs() < 1e-4);

        let v1 = "#VRML V1.0 ascii\nSeparator { Cube {} }\n";
        assert!(matches!(load(Path::new("a.wrl"), v1.as_bytes(), &Limits::viewer()), Err(LoadError::Unsupported(_))));
    }

    #[test]
    fn inlines_neighbouring_files_when_allowed() {
        let dir = std::env::temp_dir().join(format!("slerp-vrml-inline-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("parts")).unwrap();
        let main = "#VRML V2.0 utf8\nTransform { translation 5 0 0 children Inline { url [ \"missing.wrl\" \"parts/box.wrl\" ] } }\nInline { url \"main.wrl\" }\n";
        std::fs::write(dir.join("main.wrl"), main).unwrap();
        // The inlined file inlines itself and a sibling by its own folder.
        let part = "#VRML V2.0 utf8\nShape { geometry Box {} }\nInline { url \"box.wrl\" }\n";
        std::fs::write(dir.join("parts/box.wrl"), part).unwrap();
        let s = load(&dir.join("main.wrl"), main.as_bytes(), &Limits::viewer()).unwrap();
        assert!(s.meshes.len() >= 1);
        assert!(s.nodes.len() < 2000, "recursion is bounded: {} nodes", s.nodes.len());
        let lonely = Limits { neighbours: false, ..Limits::viewer() };
        assert!(load(&dir.join("main.wrl"), main.as_bytes(), &lonely).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn reads_gzip_and_generated_shapes() {
        let text = "#VRML V2.0 utf8\nShape { geometry Cylinder {} } Shape { geometry Cone {} } Shape { geometry ElevationGrid { xDimension 2 zDimension 2 height [0 0 0 1] } } Shape { geometry Extrusion { } }";
        let deflated = miniz_oxide::deflate::compress_to_vec(text.as_bytes(), 6);
        let mut gz = vec![0x1f, 0x8b, 8, 0, 0, 0, 0, 0, 0, 3];
        gz.extend_from_slice(&deflated);
        gz.extend_from_slice(&[0; 8]);
        let s = load(Path::new("a.wrz"), &gz, &Limits::viewer()).unwrap();
        assert_eq!(s.meshes.len(), 4);
        assert!(s.info.format.ends_with("gzipped"));
    }

    #[test]
    fn survives_truncation_and_garbage() {
        let bytes = WORLD.as_bytes();
        for cut in 0..bytes.len() {
            let _ = load(Path::new("w.wrl"), &bytes[..cut], &Limits::viewer());
        }
        for bad in [
            "#VRML V2.0 utf8\nShape { geometry IndexedFaceSet { coord Coordinate { point [ 0 0 0 ] } coordIndex [ 0 99 -5 4294967295 ] } }",
            "#VRML V2.0 utf8\nPROTO P [ ] { P { } } P { }",
            "#VRML V2.0 utf8\n] } ] Group { children [ USE nothing DEF a Group { children USE a } ] }",
            "#VRML V2.0 utf8\nShape { geometry ElevationGrid { xDimension 100000 zDimension 100000 } }",
            "#VRML V2.0 utf8\nShape { appearance Appearance { texture PixelTexture { image 99999 99999 3 0xFF } } geometry Box {} }",
        ] {
            let _ = load(Path::new("w.wrl"), bad.as_bytes(), &Limits::viewer());
        }
        let deep = format!("#VRML V2.0 utf8\n{}", "Group { children [ ".repeat(5000));
        let _ = load(Path::new("w.wrl"), deep.as_bytes(), &Limits::viewer());
        let limits = Limits { max_triangles: 3, ..Limits::viewer() };
        assert!(matches!(load(Path::new("w.wrl"), bytes, &limits), Err(LoadError::TooLarge(_))));
    }
}
