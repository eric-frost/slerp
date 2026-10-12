//! DirectX `.x` files, in their text, binary and MSZIP-compressed forms.
//!
//! An `.x` file is a sequence of templates (skipped) and data objects.
//! Covered: the `Frame` hierarchy with `FrameTransformMatrix`, `Mesh` with
//! `MeshNormals`, `MeshTextureCoords`, `MeshVertexColors`,
//! `MeshMaterialList` and `Material` (inline or referenced by name, with
//! `TextureFilename`), `SkinWeights`, and `AnimationSet`s, each of which
//! becomes a clip, with rotation, scale, position and matrix keys timed by
//! `AnimTicksPerSecond`.
//!
//! The text and binary forms are read through one token stream, so a single
//! parser serves both.  DirectX is left-handed: positions, frames, skins and
//! keys are mirrored along Z and faces reversed, which gives the same
//! picture in Slerp's right-handed coordinates.  Units are whatever the
//! exporter used; they're left at 1 unit = 1 m.

use crate::archive::mszip;
use crate::common::{TextureSet, check_triangles, parse_f32, srgb_color};
use crate::polymesh::{Attr, Binding, PolyMesh, Smoothing};
use glam::{Mat4, Quat, Vec2, Vec3, Vec4};
use slerp_scene::{
    AlphaMode, Animation, Channel, Interpolation, Limits, LoadError, Material, Mesh, Node, Property, Scene, Skin,
    TexRef, Transform,
};
use std::collections::HashMap;
use std::path::Path;

/// Objects nested deeper than this are malformed or malicious.
const MAX_DEPTH: usize = 128;
/// D3DX's tick rate when a file doesn't give one.
const DEFAULT_TICKS: f32 = 4800.0;
/// The frame rate assumed for keys that are frame numbers.
const FRAME_RATE: f32 = 30.0;

/// True for the `xof ` signature with a known encoding.
pub fn looks_like_x(head: &[u8]) -> bool {
    head.len() >= 16 && head.starts_with(b"xof ") && matches!(&head[8..12], b"txt " | b"bin " | b"tzip" | b"bzip")
}

/*┌─────────────────╮
| Tokens          |
└─────────────────╯*/
//region Tokens

#[derive(Clone, Debug, PartialEq)]
enum Tok {
    Name(String),
    Str(String),
    Num(f64),
    Open,
    Close,
    /// Commas, semicolons and the brackets of template declarations.
    Sep,
    Guid,
    End,
}

/// The text encoding: names, numbers, quoted strings, braces, separators
/// and `<GUID>`s, with `//` and `#` comments.
struct TextLexer<'a> {
    b: &'a [u8],
    at: usize,
}

impl TextLexer<'_> {
    fn next(&mut self) -> Tok {
        let b = self.b;
        loop {
            while self.at < b.len() && b[self.at].is_ascii_whitespace() {
                self.at += 1;
            }
            let Some(&c) = b.get(self.at) else { return Tok::End };
            if c == b'#' || (c == b'/' && b.get(self.at + 1) == Some(&b'/')) {
                while self.at < b.len() && b[self.at] != b'\n' {
                    self.at += 1;
                }
                continue;
            }
            self.at += 1;
            return match c {
                b'{' => Tok::Open,
                b'}' => Tok::Close,
                b',' | b';' | b'[' | b']' | b'(' | b')' => Tok::Sep,
                b'<' => {
                    while self.at < b.len() && b[self.at] != b'>' {
                        self.at += 1;
                    }
                    self.at += 1;
                    Tok::Guid
                }
                b'"' => {
                    let start = self.at;
                    while self.at < b.len() && b[self.at] != b'"' {
                        self.at += 1;
                    }
                    let s = String::from_utf8_lossy(&b[start..self.at.min(b.len())]).into_owned();
                    self.at += 1;
                    Tok::Str(s)
                }
                _ => {
                    let start = self.at - 1;
                    while self.at < b.len()
                        && !b[self.at].is_ascii_whitespace()
                        && !b",;{}<>\"[]()".contains(&b[self.at])
                    {
                        self.at += 1;
                    }
                    let word = &b[start..self.at];
                    if c.is_ascii_digit() || matches!(c, b'-' | b'+' | b'.') {
                        // MSVC writes NaNs and infinities as "-1.#IND00".
                        Tok::Num(parse_f32(word).filter(|v| v.is_finite()).unwrap_or(0.0) as f64)
                    } else {
                        Tok::Name(String::from_utf8_lossy(word).into_owned())
                    }
                }
            };
        }
    }
}

/// The binary encoding: 16-bit token codes, with names, strings and number
/// lists inline.  Number lists are handed out one number at a time.
struct BinLexer<'a> {
    b: &'a [u8],
    at: usize,
    doubles: bool,
    /// Numbers left in the current list, and whether they are floats.
    pending: (u32, bool),
}

impl BinLexer<'_> {
    fn u16(&mut self) -> Option<u16> {
        let v = self.b.get(self.at..self.at + 2)?;
        self.at += 2;
        Some(u16::from_le_bytes([v[0], v[1]]))
    }

    fn u32(&mut self) -> Option<u32> {
        let v = self.b.get(self.at..self.at + 4)?;
        self.at += 4;
        Some(u32::from_le_bytes([v[0], v[1], v[2], v[3]]))
    }

    fn text(&mut self) -> Option<String> {
        let len = self.u32()? as usize;
        let v = self.b.get(self.at..self.at.checked_add(len)?)?;
        self.at += len;
        Some(String::from_utf8_lossy(v).into_owned())
    }

    fn next(&mut self) -> Tok {
        if self.pending.0 > 0 {
            self.pending.0 -= 1;
            let v = if !self.pending.1 {
                self.u32().map(|v| v as f64)
            } else if self.doubles {
                self.b.get(self.at..self.at + 8).map(|v| {
                    self.at += 8;
                    f64::from_le_bytes(v.try_into().unwrap_or_default())
                })
            } else {
                self.b.get(self.at..self.at + 4).map(|v| {
                    self.at += 4;
                    f32::from_le_bytes(v.try_into().unwrap_or_default()) as f64
                })
            };
            return match v {
                Some(v) if v.is_finite() => Tok::Num(v),
                Some(_) => Tok::Num(0.0),
                None => Tok::End,
            };
        }
        let Some(code) = self.u16() else { return Tok::End };
        match code {
            1 => self.text().map_or(Tok::End, Tok::Name),
            2 => self.text().map_or(Tok::End, Tok::Str),
            3 => self.u32().map_or(Tok::End, |v| Tok::Num(v as f64)),
            5 => {
                self.at += 16;
                Tok::Guid
            }
            6 | 7 => match self.u32() {
                Some(n) => {
                    self.pending = (n, code == 7);
                    self.next()
                }
                None => Tok::End,
            },
            0x0a => Tok::Open,
            0x0b => Tok::Close,
            0x0c..=0x14 => Tok::Sep,
            0x1f => Tok::Name("template".into()),
            0x28..=0x34 => Tok::Name(String::new()),
            _ => Tok::End,
        }
    }
}

enum Lexer<'a> {
    Text(TextLexer<'a>),
    Binary(BinLexer<'a>),
}

//endregion

/*┌─────────────────╮
| Parsing         |
└─────────────────╯*/
//region Parsing

#[derive(Default)]
struct XMaterial {
    name: String,
    color: Vec4,
    power: f32,
    specular: Vec3,
    emissive: Vec3,
    texture: Option<String>,
    normal_map: Option<String>,
}

enum MatRef {
    Inline(usize),
    Named(String),
}

#[derive(Default)]
struct XSkin {
    bone: String,
    indices: Vec<u32>,
    weights: Vec<f32>,
    offset: Mat4,
}

#[derive(Default)]
struct XMesh {
    name: String,
    positions: Vec<Vec3>,
    face_sizes: Vec<u32>,
    corners: Vec<u32>,
    normals: Vec<Vec3>,
    normal_sizes: Vec<u32>,
    normal_corners: Vec<u32>,
    uvs: Vec<Vec2>,
    colors: Vec<(u32, Vec4)>,
    face_materials: Vec<u32>,
    materials: Vec<MatRef>,
    skins: Vec<XSkin>,
}

#[derive(Default)]
struct XFrame {
    name: String,
    matrix: Mat4,
    children: Vec<usize>,
    meshes: Vec<usize>,
}

/// One key list of one animated frame.  `kind` is 0 rotation, 1 scale,
/// 2 position, 3 or 4 matrix.
struct XKeys {
    kind: u32,
    times: Vec<f32>,
    values: Vec<Vec<f32>>,
}

#[derive(Default)]
struct XAnimSet {
    name: String,
    tracks: Vec<(String, Vec<XKeys>)>,
}

struct Parser<'a> {
    lexer: Lexer<'a>,
    peeked: Option<Tok>,
    frames: Vec<XFrame>,
    roots: Vec<usize>,
    meshes: Vec<XMesh>,
    loose_meshes: Vec<usize>,
    materials: Vec<XMaterial>,
    named_materials: HashMap<String, usize>,
    sets: Vec<XAnimSet>,
    ticks: Option<f32>,
    triangles: u64,
    limits: &'a Limits,
}

impl Parser<'_> {
    fn next(&mut self) -> Tok {
        if let Some(t) = self.peeked.take() {
            return t;
        }
        match &mut self.lexer {
            Lexer::Text(l) => l.next(),
            Lexer::Binary(l) => l.next(),
        }
    }

    fn peek(&mut self) -> &Tok {
        if self.peeked.is_none() {
            let t = self.next();
            self.peeked = Some(t);
        }
        self.peeked.as_ref().unwrap_or(&Tok::End)
    }

    /// The next number, skipping separators.  Anything else is left unread.
    fn num(&mut self) -> Option<f64> {
        loop {
            match self.peek() {
                Tok::Sep => {
                    self.next();
                }
                Tok::Num(v) => {
                    let v = *v;
                    self.next();
                    return Some(v);
                }
                _ => return None,
            }
        }
    }

    fn float(&mut self) -> Option<f32> {
        self.num().map(|v| v as f32)
    }

    fn int(&mut self) -> Option<u32> {
        self.num().map(|v| v.max(0.0).min(u32::MAX as f64) as u32)
    }

    fn floats<const N: usize>(&mut self) -> Option<[f32; N]> {
        let mut out = [0.0; N];
        for v in &mut out {
            *v = self.float()?;
        }
        Some(out)
    }

    fn string(&mut self) -> Option<String> {
        loop {
            match self.next() {
                Tok::Sep => continue,
                Tok::Str(s) | Tok::Name(s) => return Some(s),
                other => {
                    self.peeked = Some(other);
                    return None;
                }
            }
        }
    }

    /// Skips to the brace that closes the object whose `{` was just read.
    fn skip_object(&mut self) {
        let mut depth = 1usize;
        loop {
            match self.next() {
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

    /// Reads an object's optional name and opening brace.  `None` when no
    /// brace follows, which leaves the stream where it was.
    fn header(&mut self) -> Option<String> {
        let mut name = String::new();
        if let Tok::Name(n) = self.peek() {
            name = n.clone();
            self.next();
        }
        if *self.peek() != Tok::Open {
            return None;
        }
        self.next();
        if *self.peek() == Tok::Guid {
            self.next();
        }
        Some(name)
    }

    /// Reads a `{ Name }` reference after its `{`.
    fn reference(&mut self) -> Option<String> {
        let mut name = None;
        loop {
            match self.next() {
                Tok::Name(n) | Tok::Str(n) => name = name.or(Some(n)),
                Tok::Close | Tok::End => return name,
                Tok::Open => {
                    self.skip_object();
                }
                _ => {}
            }
        }
    }

    /// Reads data objects until the enclosing object closes, calling
    /// `child` for each named one; unknown objects and references are
    /// skipped.
    fn body(&mut self, depth: usize, mut child: impl FnMut(&mut Self, &str, String, usize)) {
        loop {
            match self.next() {
                Tok::Close | Tok::End => return,
                Tok::Open => {
                    self.skip_object();
                }
                Tok::Name(kind) => {
                    let Some(name) = self.header() else { continue };
                    if depth >= MAX_DEPTH {
                        self.skip_object();
                    } else {
                        child(self, &kind, name, depth + 1);
                    }
                }
                _ => {}
            }
        }
    }

    /// Top-level objects.
    fn file(&mut self) {
        loop {
            match self.next() {
                Tok::End => return,
                Tok::Name(kind) => {
                    let Some(name) = self.header() else { continue };
                    match kind.as_str() {
                        "Frame" => {
                            let f = self.frame(name, 1);
                            self.roots.push(f);
                        }
                        "Mesh" => {
                            let m = self.mesh(name, 1);
                            self.loose_meshes.push(m);
                        }
                        "Material" => {
                            let m = self.material(name.clone(), 1);
                            self.named_materials.insert(name, m);
                        }
                        "AnimationSet" => self.animation_set(name, 1),
                        "AnimTicksPerSecond" => {
                            self.ticks = self.float().filter(|t| *t > 0.0);
                            self.skip_object();
                        }
                        _ => self.skip_object(),
                    }
                }
                Tok::Open => self.skip_object(),
                _ => {}
            }
        }
    }

    fn frame(&mut self, name: String, depth: usize) -> usize {
        let index = self.frames.len();
        self.frames.push(XFrame { name, matrix: Mat4::IDENTITY, ..Default::default() });
        self.body(depth, |p, kind, name, depth| match kind {
            "FrameTransformMatrix" => {
                if let Some(m) = p.floats::<16>() {
                    let m = Mat4::from_cols_array(&m);
                    if m.is_finite() {
                        p.frames[index].matrix = m;
                    }
                }
                p.skip_object();
            }
            "Frame" => {
                let f = p.frame(name, depth);
                p.frames[index].children.push(f);
            }
            "Mesh" => {
                let m = p.mesh(name, depth);
                p.frames[index].meshes.push(m);
            }
            _ => p.skip_object(),
        });
        index
    }

    /// Reads `count` polygons as a corner count and that many indices each.
    fn faces(&mut self, sizes: &mut Vec<u32>, corners: &mut Vec<u32>, triangles: bool) -> Option<()> {
        let count = self.int()?;
        for _ in 0..count {
            let n = self.int()?;
            if n > 1 << 16 {
                return None;
            }
            sizes.push(n);
            for _ in 0..n {
                corners.push(self.int()?);
            }
            if triangles {
                self.triangles += n.saturating_sub(2) as u64;
            }
        }
        Some(())
    }

    fn mesh(&mut self, name: String, depth: usize) -> usize {
        let mut m = XMesh { name, ..Default::default() };
        // 1. Positions and faces, then the mesh's own objects.
        let ok = (|| {
            let count = self.int()?;
            for _ in 0..count {
                let [x, y, z] = self.floats::<3>()?;
                m.positions.push(Vec3::new(x, y, z));
            }
            self.faces(&mut m.face_sizes, &mut m.corners, true)
        })();
        let index = self.meshes.len();
        if ok.is_none() || self.triangles > self.limits.max_triangles {
            self.skip_object();
            self.meshes.push(m);
            return index;
        }
        self.meshes.push(m);
        self.body(depth, |p, kind, name, depth| {
            match kind {
                "MeshNormals" => {
                    let mut normals = Vec::new();
                    let (mut sizes, mut corners) = (Vec::new(), Vec::new());
                    let _ = (|| {
                        let count = p.int()?;
                        for _ in 0..count {
                            let [x, y, z] = p.floats::<3>()?;
                            normals.push(Vec3::new(x, y, z));
                        }
                        p.faces(&mut sizes, &mut corners, false)
                    })();
                    let m = &mut p.meshes[index];
                    (m.normals, m.normal_sizes, m.normal_corners) = (normals, sizes, corners);
                }
                "MeshTextureCoords" => {
                    let mut uvs = Vec::new();
                    if let Some(count) = p.int() {
                        for _ in 0..count {
                            let Some([u, v]) = p.floats::<2>() else { break };
                            uvs.push(Vec2::new(u, v));
                        }
                    }
                    p.meshes[index].uvs = uvs;
                }
                "MeshVertexColors" => {
                    let mut colors = Vec::new();
                    if let Some(count) = p.int() {
                        for _ in 0..count {
                            let (Some(i), Some([r, g, b, a])) = (p.int(), p.floats::<4>()) else { break };
                            colors.push((i, srgb_color(r, g, b, a)));
                        }
                    }
                    p.meshes[index].colors = colors;
                }
                "MeshMaterialList" => {
                    let mut faces = Vec::new();
                    let _ = (|| {
                        let _materials = p.int()?;
                        let count = p.int()?;
                        for _ in 0..count {
                            faces.push(p.int()?);
                        }
                        Some(())
                    })();
                    p.meshes[index].face_materials = faces;
                    // Materials follow, inline or as references.
                    loop {
                        match p.next() {
                            Tok::Close | Tok::End => break,
                            Tok::Open => {
                                if let Some(name) = p.reference() {
                                    p.meshes[index].materials.push(MatRef::Named(name));
                                }
                            }
                            Tok::Name(kind) => {
                                let Some(name) = p.header() else { continue };
                                if kind == "Material" && depth < MAX_DEPTH {
                                    let mat = p.material(name, depth + 1);
                                    p.meshes[index].materials.push(MatRef::Inline(mat));
                                } else {
                                    p.skip_object();
                                }
                            }
                            _ => {}
                        }
                    }
                    return;
                }
                "SkinWeights" => {
                    let mut skin = XSkin::default();
                    let _ = (|| {
                        skin.bone = p.string()?;
                        let count = p.int()?;
                        for _ in 0..count {
                            skin.indices.push(p.int()?);
                        }
                        for _ in 0..count {
                            skin.weights.push(p.float()?);
                        }
                        let m = Mat4::from_cols_array(&p.floats::<16>()?);
                        skin.offset = if m.is_finite() { m } else { Mat4::IDENTITY };
                        Some(())
                    })();
                    if !skin.bone.is_empty() {
                        p.meshes[index].skins.push(skin);
                    }
                }
                _ => {}
            }
            let _ = name;
            p.skip_object();
        });
        index
    }

    fn material(&mut self, name: String, depth: usize) -> usize {
        let mut m = XMaterial { name, color: Vec4::new(0.8, 0.8, 0.8, 1.0), ..Default::default() };
        let _ = (|| {
            let [r, g, b, a] = self.floats::<4>()?;
            m.color = srgb_color(r, g, b, a);
            m.power = self.float()?;
            let s = self.floats::<3>()?;
            m.specular = Vec3::from(s);
            let e = self.floats::<3>()?;
            m.emissive = srgb_color(e[0], e[1], e[2], 1.0).truncate();
            Some(())
        })();
        let index = self.materials.len();
        self.materials.push(m);
        self.body(depth, |p, kind, _, _| {
            let lower = kind.to_ascii_lowercase();
            if lower == "texturefilename" || lower == "normalmapfilename" || lower == "bumpmapfilename" {
                let file = p.string().filter(|f| !f.trim().is_empty());
                let m = &mut p.materials[index];
                if lower == "texturefilename" {
                    m.texture = m.texture.take().or(file);
                } else {
                    m.normal_map = m.normal_map.take().or(file);
                }
            }
            p.skip_object();
        });
        index
    }

    fn animation_set(&mut self, name: String, depth: usize) {
        let mut set = XAnimSet { name, tracks: Vec::new() };
        self.body(depth, |p, kind, _, depth| {
            if kind != "Animation" {
                p.skip_object();
                return;
            }
            let mut target = None;
            let mut keys = Vec::new();
            loop {
                match p.next() {
                    Tok::Close | Tok::End => break,
                    Tok::Open => target = target.or(p.reference()),
                    Tok::Name(kind) => {
                        let Some(_) = p.header() else {
                            // A bare frame name, as some exporters write it.
                            target = target.or(Some(kind));
                            continue;
                        };
                        if kind == "AnimationKey" && depth < MAX_DEPTH {
                            keys.extend(p.animation_key());
                        }
                        p.skip_object();
                    }
                    _ => {}
                }
            }
            if let Some(t) = target {
                set.tracks.push((t, keys));
            }
        });
        self.sets.push(set);
    }

    /// Reads an `AnimationKey`'s content, leaving its closing brace.
    fn animation_key(&mut self) -> Option<XKeys> {
        let kind = self.int()?;
        let count = self.int()?;
        let mut keys = XKeys { kind, times: Vec::new(), values: Vec::new() };
        for _ in 0..count {
            let time = self.float()?;
            let n = self.int()?.min(16);
            let mut v = Vec::with_capacity(n as usize);
            for _ in 0..n {
                v.push(self.float()?);
            }
            keys.times.push(time);
            keys.values.push(v);
        }
        Some(keys)
    }
}

//endregion

/*┌─────────────────╮
| Scene           |
└─────────────────╯*/
//region Scene

/// Mirrors along Z: left-handed DirectX to right-handed.
const FLIP: Vec3 = Vec3::new(1.0, 1.0, -1.0);

fn flip_matrix(m: Mat4) -> Mat4 {
    let s = Mat4::from_scale(FLIP);
    s * m * s
}

fn flip_quat(q: Quat) -> Quat {
    Quat::from_xyzw(-q.x, -q.y, q.z, q.w)
}

struct Builder<'a, 'p> {
    p: &'p Parser<'a>,
    textures: TextureSet<'a>,
    scene: Scene,
    materials: HashMap<usize, usize>,
    node_of_frame: Vec<Option<usize>>,
    warnings: Vec<String>,
    pending_skins: Vec<(usize, usize)>,
}

impl Builder<'_, '_> {
    fn material(&mut self, index: usize) -> usize {
        if let Some(&m) = self.materials.get(&index) {
            return m;
        }
        let x = &self.p.materials[index];
        let mut m = Material { name: x.name.clone(), base_color: x.color, emissive: x.emissive, ..Material::default() };
        // Phong power to roughness, as for OBJ's Ns.
        m.roughness = (2.0 / (x.power.max(0.0) + 2.0)).sqrt().clamp(0.05, 1.0);
        m.specular = x.specular.max_element().clamp(0.0, 1.0);
        if let Some(file) = &x.texture
            && let Some(t) = self.textures.file(file)
        {
            m.base_color_texture = Some(TexRef::new(t));
            m.base_color = Vec4::new(1.0, 1.0, 1.0, x.color.w);
            if self.textures.get(t).is_some_and(|t| t.has_alpha) {
                m.alpha_mode = AlphaMode::Mask(0.5);
            }
        }
        if let Some(file) = &x.normal_map
            && let Some(t) = self.textures.file(file)
        {
            m.normal_texture = Some(TexRef::new(t));
        }
        if x.color.w < 0.999 {
            m.alpha_mode = AlphaMode::Blend;
        }
        self.scene.materials.push(m);
        let at = self.scene.materials.len() - 1;
        self.materials.insert(index, at);
        at
    }

    /// Builds mesh `index`; returns the scene mesh and whether it is skinned.
    fn mesh(&mut self, index: usize) -> Option<usize> {
        let x = &self.p.meshes[index];
        let mut pm = PolyMesh {
            positions: x.positions.iter().map(|&p| p * FLIP).collect(),
            face_sizes: x.face_sizes.clone(),
            corners: x.corners.clone(),
            smoothing: Smoothing::default(),
            ..Default::default()
        };
        // Normals have faces of their own, matching the mesh's.
        if !x.normals.is_empty() && x.normal_sizes == x.face_sizes {
            pm.normals = Some(Attr::new(
                x.normals.iter().map(|&n| n * FLIP).collect(),
                Binding::CornerIndexed(x.normal_corners.clone()),
            ));
        }
        if !x.uvs.is_empty() {
            pm.uvs = Some(Attr::new(x.uvs.clone(), Binding::Vertex));
        }
        if !x.colors.is_empty() {
            let mut colors = vec![Vec4::ONE; x.positions.len()];
            for &(i, c) in &x.colors {
                if let Some(slot) = colors.get_mut(i as usize) {
                    *slot = c;
                }
            }
            pm.colors = Some(Attr::new(colors, Binding::Vertex));
        }
        // A short material list repeats its last entry, as D3DX does.
        if !x.face_materials.is_empty() {
            let last = *x.face_materials.last().unwrap_or(&0);
            pm.face_slots = (0..x.face_sizes.len()).map(|f| x.face_materials.get(f).copied().unwrap_or(last)).collect();
        }
        let joints = self.skin_weights(x);
        if let Some(j) = &joints {
            pm.skin = Some(j.clone());
        }
        pm.flip_winding();
        let prims = pm.build(&mut self.warnings);
        if prims.is_empty() {
            return None;
        }
        let slots: Vec<Option<usize>> = x
            .materials
            .iter()
            .map(|r| match r {
                MatRef::Inline(i) => Some(*i),
                MatRef::Named(n) => self.p.named_materials.get(n).copied(),
            })
            .collect();
        let primitives = prims
            .into_iter()
            .map(|(slot, mut prim)| {
                prim.material = slots.get(slot as usize).copied().flatten().map(|m| self.material(m));
                prim
            })
            .collect();
        self.scene.meshes.push(Mesh { name: x.name.clone(), primitives, target_names: Vec::new() });
        Some(self.scene.meshes.len() - 1)
    }

    /// Up to four strongest bone influences per vertex, as joints indexing
    /// the mesh's skin (bone order of its `SkinWeights`).
    fn skin_weights(&self, x: &XMesh) -> Option<(Vec<[u16; 4]>, Vec<Vec4>)> {
        if x.skins.is_empty() || x.skins.len() > u16::MAX as usize {
            return None;
        }
        let mut per: Vec<Vec<(u16, f32)>> = vec![Vec::new(); x.positions.len()];
        for (j, s) in x.skins.iter().enumerate() {
            for (&v, &w) in s.indices.iter().zip(&s.weights) {
                if let Some(list) = per.get_mut(v as usize)
                    && w.is_finite()
                    && w > 0.0
                {
                    list.push((j as u16, w));
                }
            }
        }
        let mut joints = Vec::with_capacity(per.len());
        let mut weights = Vec::with_capacity(per.len());
        for mut list in per {
            list.sort_by(|a, b| b.1.total_cmp(&a.1));
            list.truncate(4);
            let total: f32 = list.iter().map(|l| l.1).sum();
            let mut j = [0u16; 4];
            let mut w = [0f32; 4];
            for (k, &(joint, weight)) in list.iter().enumerate() {
                j[k] = joint;
                w[k] = if total > 0.0 { weight / total } else { 0.0 };
            }
            if total <= 0.0 {
                w[0] = 1.0;
            }
            joints.push(j);
            weights.push(Vec4::from(w));
        }
        Some((joints, weights))
    }

    fn frame(&mut self, f: usize, depth: usize, seen: &mut [bool]) -> Option<usize> {
        if depth > MAX_DEPTH || seen.get(f).copied().unwrap_or(true) {
            return None;
        }
        seen[f] = true;
        let x = &self.p.frames[f];
        let mut node = Node::new(x.name.clone());
        node.transform = Transform::from_matrix(flip_matrix(x.matrix));
        self.scene.nodes.push(node);
        let index = self.scene.nodes.len() - 1;
        self.node_of_frame[f] = Some(index);
        let meshes = x.meshes.clone();
        let children = x.children.clone();
        let mut on_frame = false;
        for &m in &meshes {
            let Some(mesh) = self.mesh(m) else { continue };
            // A skinned mesh follows its bones and ignores its frame, as in
            // Direct3D, so it gets a root node of its own: the renderer takes
            // the winding from the mesh node's matrix, which a mirroring
            // frame (a common way to turn Z-up into Y-up) would flip.
            let skinned = !self.p.meshes[m].skins.is_empty();
            let target = if !skinned && !on_frame {
                on_frame = true;
                index
            } else {
                let mut n = Node::new(self.p.meshes[m].name.clone());
                n.mesh = Some(mesh);
                self.scene.nodes.push(n);
                let child = self.scene.nodes.len() - 1;
                if !skinned {
                    self.scene.nodes[index].children.push(child);
                }
                child
            };
            self.scene.nodes[target].mesh = Some(mesh);
            if skinned {
                self.pending_skins.push((target, m));
            }
        }
        for c in children {
            if let Some(child) = self.frame(c, depth + 1, seen) {
                self.scene.nodes[index].children.push(child);
            }
        }
        Some(index)
    }

    /// Skins name their bones by frame name, so they are resolved once every
    /// frame has a node.
    fn skins(&mut self) {
        let by_name: HashMap<&str, usize> = self
            .p
            .frames
            .iter()
            .enumerate()
            .filter_map(|(f, x)| Some((x.name.as_str(), self.node_of_frame[f]?)))
            .collect();
        for (node, m) in std::mem::take(&mut self.pending_skins) {
            let x = &self.p.meshes[m];
            let mut skin = Skin { name: x.name.clone(), ..Default::default() };
            let mut missing = 0;
            for s in &x.skins {
                match by_name.get(s.bone.as_str()) {
                    Some(&j) => {
                        skin.joints.push(j);
                        skin.inverse_bind.push(flip_matrix(s.offset));
                    }
                    None => {
                        // Keep the joint slot so vertex joint indices stay
                        // valid; it follows the mesh's own node.
                        missing += 1;
                        skin.joints.push(node);
                        skin.inverse_bind.push(Mat4::IDENTITY);
                    }
                }
            }
            if missing > 0 {
                self.warnings.push(format!("{}: {missing} skin bones not found", x.name));
            }
            self.scene.skins.push(skin);
            self.scene.nodes[node].skin = Some(self.scene.skins.len() - 1);
        }
    }

    /// Ticks per second: the file's, or D3DX's 4800, unless the keys are
    /// so few ticks apart that they must be frame numbers (a whole clip in
    /// under a tenth of a second), which are taken at 30 per second.
    fn ticks_per_second(&self) -> (f32, bool) {
        if let Some(t) = self.p.ticks {
            return (t, false);
        }
        let last = self.p.sets.iter().flat_map(|s| &s.tracks).flat_map(|t| &t.1).flat_map(|k| k.times.last());
        let last = last.copied().fold(0.0f32, f32::max);
        if last > 0.0 && last < DEFAULT_TICKS / 10.0 { (FRAME_RATE, true) } else { (DEFAULT_TICKS, false) }
    }

    fn animations(&mut self) {
        let (tps, guessed) = self.ticks_per_second();
        if guessed {
            self.scene.info.details.push(("Ticks per second".into(), format!("{tps} (keys look like frame numbers)")));
        }
        let by_name: HashMap<&str, usize> = self
            .p
            .frames
            .iter()
            .enumerate()
            .filter_map(|(f, x)| Some((x.name.as_str(), self.node_of_frame[f]?)))
            .collect();
        for set in &self.p.sets {
            let mut anim = Animation {
                name: if set.name.is_empty() { "Animation".into() } else { set.name.clone() },
                ..Default::default()
            };
            for (target, key_lists) in &set.tracks {
                let Some(&node) = by_name.get(target.as_str()) else { continue };
                for keys in key_lists {
                    let times: Vec<f32> = keys.times.iter().map(|t| t / tps).collect();
                    let mut push = |property, values: Vec<f32>| {
                        anim.channels.push(Channel {
                            node,
                            property,
                            interpolation: Interpolation::Linear,
                            times: times.clone(),
                            values,
                        });
                    };
                    let get = |v: &Vec<f32>, i: usize| v.get(i).copied().unwrap_or(0.0);
                    match keys.kind {
                        0 => push(
                            Property::Rotation,
                            keys.values
                                .iter()
                                .flat_map(|v| {
                                    // Stored as w, x, y, z, and turning the other
                                    // way from Direct3D's row vectors: conjugated.
                                    let q = Quat::from_xyzw(-get(v, 1), -get(v, 2), -get(v, 3), get(v, 0));
                                    let q = if q.length_squared() > 1e-12 && q.is_finite() {
                                        q.normalize()
                                    } else {
                                        Quat::IDENTITY
                                    };
                                    flip_quat(q).to_array()
                                })
                                .collect(),
                        ),
                        1 => push(
                            Property::Scale,
                            keys.values.iter().flat_map(|v| [get(v, 0), get(v, 1), get(v, 2)]).collect(),
                        ),
                        2 => push(
                            Property::Translation,
                            keys.values
                                .iter()
                                .flat_map(|v| (Vec3::new(get(v, 0), get(v, 1), get(v, 2)) * FLIP).to_array())
                                .collect(),
                        ),
                        3 | 4 => {
                            let parts: Vec<Transform> = keys
                                .values
                                .iter()
                                .map(|v| {
                                    let m: [f32; 16] = std::array::from_fn(|i| get(v, i));
                                    let m = flip_matrix(Mat4::from_cols_array(&m));
                                    if m.is_finite() { Transform::from_matrix(m) } else { Transform::IDENTITY }
                                })
                                .collect();
                            push(Property::Translation, parts.iter().flat_map(|t| t.translation.to_array()).collect());
                            push(Property::Rotation, parts.iter().flat_map(|t| t.rotation.to_array()).collect());
                            push(Property::Scale, parts.iter().flat_map(|t| t.scale.to_array()).collect());
                        }
                        _ => {}
                    }
                }
            }
            // Keys must rise in time and every value must be there.
            anim.channels.retain(|c| {
                let width = if c.property == Property::Rotation { 4 } else { 3 };
                !c.times.is_empty()
                    && c.values.len() == c.times.len() * width
                    && c.times.windows(2).all(|w| w[1] > w[0])
                    && c.times.iter().all(|t| t.is_finite())
            });
            let start = anim.channels.iter().filter_map(|c| c.times.first().copied()).fold(f32::INFINITY, f32::min);
            if start.is_finite() && start != 0.0 {
                for c in &mut anim.channels {
                    c.times.iter_mut().for_each(|t| *t -= start);
                }
            }
            anim.duration = anim.channels.iter().filter_map(|c| c.times.last().copied()).fold(0.0, f32::max);
            if !anim.channels.is_empty() {
                self.scene.animations.push(anim);
            }
        }
    }
}

pub fn load(path: &Path, bytes: &[u8], limits: &Limits) -> Result<Scene, LoadError> {
    // 1. Header and encoding.
    if !looks_like_x(bytes) {
        return Err(LoadError::NotMine);
    }
    let encoding = &bytes[8..12];
    let doubles = &bytes[12..16] == b"0064";
    let inflated;
    let body: &[u8] = match encoding {
        b"tzip" | b"bzip" => {
            inflated = mszip(&bytes[16..], limits.max_file_bytes)?;
            &inflated
        }
        _ => &bytes[16..],
    };
    let lexer = match encoding {
        b"txt " | b"tzip" => Lexer::Text(TextLexer { b: body, at: 0 }),
        _ => Lexer::Binary(BinLexer { b: body, at: 0, doubles, pending: (0, false) }),
    };

    // 2. Every object in the file.
    let mut p = Parser {
        lexer,
        peeked: None,
        frames: Vec::new(),
        roots: Vec::new(),
        meshes: Vec::new(),
        loose_meshes: Vec::new(),
        materials: Vec::new(),
        named_materials: HashMap::new(),
        sets: Vec::new(),
        ticks: None,
        triangles: 0,
        limits,
    };
    p.file();
    check_triangles(p.triangles, limits)?;

    // 3. Nodes, meshes, skins and clips.
    let mut b = Builder {
        p: &p,
        textures: TextureSet::new(path, limits),
        scene: Scene::default(),
        materials: HashMap::new(),
        node_of_frame: vec![None; p.frames.len()],
        warnings: Vec::new(),
        pending_skins: Vec::new(),
    };
    let mut seen = vec![false; p.frames.len()];
    for &r in &p.roots {
        b.frame(r, 0, &mut seen);
    }
    for &m in &p.loose_meshes {
        if let Some(mesh) = b.mesh(m) {
            let mut node = Node::new(p.meshes[m].name.clone());
            node.mesh = Some(mesh);
            b.scene.nodes.push(node);
        }
    }
    b.skins();
    if limits.animations {
        b.animations();
    }
    b.scene.compute_roots();
    if b.scene.meshes.is_empty() {
        return Err(LoadError::invalid("DirectX file holds no meshes"));
    }
    let mut scene = b.scene;
    // Major and minor apart: the bytes may be anything, and slicing the
    // decoded text could cut a replacement character in two.
    let (major, minor) = (String::from_utf8_lossy(&bytes[4..6]), String::from_utf8_lossy(&bytes[6..8]));
    let encoding = match encoding {
        b"txt " => "text",
        b"bin " => "binary",
        b"tzip" => "compressed text",
        _ => "compressed binary",
    };
    scene.info.format = format!("DirectX {major}.{minor} ({encoding})");
    if let Some(t) = p.ticks {
        scene.info.details.push(("Ticks per second".into(), t.to_string()));
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

    const QUAD: &str = r#"xof 0303txt 0032
template Frame { <3d82ab46-62da-11cf-ab39-0020af71e433> [...] }
AnimTicksPerSecond { 10; }
Material Red { 1.0;0.0;0.0;1.0;; 20.0; 1.0;1.0;1.0;; 0.0;0.0;0.0;; TextureFilename { "missing.png"; } }
Frame Root {
  FrameTransformMatrix { 1,0,0,0, 0,1,0,0, 0,0,1,0, 0,0,5,1;; }
  Frame Bone { FrameTransformMatrix { 1,0,0,0, 0,1,0,0, 0,0,1,0, 0,0,0,1;; } }
  Mesh Quad {
    4; 0;0;0;, 1;0;0;, 1;1;0;, 0;1;0;;
    1; 4;0,3,2,1;;
    MeshNormals { 1; 0;0;-1;; 1; 4;0,0,0,0;; }
    MeshTextureCoords { 4; 0;1;, 1;1;, 1;0;, 0;0;; }
    MeshMaterialList { 1; 1; 0;; { Red } }
    SkinWeights { "Bone"; 4; 0,1,2,3; 1,1,1,1; 1,0,0,0, 0,1,0,0, 0,0,1,0, 0,0,0,1;; }
  }
}
AnimationSet Wave {
  Animation { { Bone } AnimationKey { 2; 2; 0;3;0,0,0;;, 10;3;1,0,0;;; } }
  Animation { { Root } AnimationKey { 0; 2; 0;4;1,0,0,0;;, 10;4;0.7071,0,0.7071,0;;; } }
}
"#;

    /// Found by fuzzing: a version that isn't text split a replacement
    /// character when it was cut into major and minor.
    #[test]
    fn version_bytes_that_are_not_text() {
        let mut bytes = QUAD.as_bytes().to_vec();
        bytes[5] = 0xff;
        let s = load(Path::new("/nonexistent/q.x"), &bytes, &Limits::viewer()).unwrap();
        assert!(s.info.format.starts_with("DirectX "));
    }

    #[test]
    fn loads_text_quad_with_skin_and_animation() {
        let s = load(Path::new("/nonexistent/q.x"), QUAD.as_bytes(), &Limits::viewer()).unwrap();
        assert_eq!(s.triangle_count(), 2);
        assert_eq!(s.materials[0].name, "Red");
        assert_eq!(s.materials[0].base_color, Vec4::new(1.0, 0.0, 0.0, 1.0));
        assert_eq!(s.skins.len(), 1);
        // The skinned mesh hangs from no frame: its bones place it.
        let mesh_node = s.nodes.iter().position(|n| n.mesh.is_some()).unwrap();
        assert!(s.roots.contains(&mesh_node));
        let p = &s.meshes[0].primitives[0];
        assert_eq!(p.joints.len(), p.positions.len());
        // Left-handed to right-handed: Z mirrors, and the normal facing -Z
        // faces +Z, still towards the side the face winds counter-clockwise
        // for.
        assert_eq!(p.normals[0], Vec3::Z);
        let [a, b, c] = [0, 1, 2].map(|k| p.positions[p.indices[k] as usize]);
        assert!((b - a).cross(c - a).z > 0.0);
        let root = s.nodes.iter().position(|n| n.name == "Root").unwrap();
        assert_eq!(s.nodes[root].transform.translation, Vec3::new(0.0, 0.0, -5.0));
        // One clip, one second long at 10 ticks per second.
        assert_eq!(s.animations.len(), 1);
        assert!((s.animations[0].duration - 1.0).abs() < 1e-6);
        assert_eq!(s.animations[0].channels.len(), 2);
    }

    /// The text file in binary form, written by hand: names, integer and
    /// float lists.
    #[test]
    fn loads_binary() {
        let mut b = b"xof 0303bin 0032".to_vec();
        let name = |b: &mut Vec<u8>, s: &str| {
            b.extend(1u16.to_le_bytes());
            b.extend((s.len() as u32).to_le_bytes());
            b.extend(s.as_bytes());
        };
        name(&mut b, "Mesh");
        b.extend(0x0au16.to_le_bytes());
        b.extend(6u16.to_le_bytes());
        b.extend(1u32.to_le_bytes());
        b.extend(3u32.to_le_bytes());
        b.extend(7u16.to_le_bytes());
        b.extend(9u32.to_le_bytes());
        for f in [0f32, 0., 0., 1., 0., 0., 0., 1., 0.] {
            b.extend(f.to_le_bytes());
        }
        b.extend(6u16.to_le_bytes());
        b.extend(5u32.to_le_bytes());
        for i in [1u32, 3, 0, 1, 2] {
            b.extend(i.to_le_bytes());
        }
        b.extend(0x0bu16.to_le_bytes());
        let s = load(Path::new("b.x"), &b, &Limits::viewer()).unwrap();
        assert_eq!(s.triangle_count(), 1);
        assert_eq!(s.info.format, "DirectX 03.03 (binary)");
        for cut in 16..b.len() {
            let _ = load(Path::new("b.x"), &b[..cut], &Limits::viewer());
        }
    }

    #[test]
    fn survives_truncation_and_garbage() {
        let bytes = QUAD.as_bytes();
        for cut in 0..bytes.len() {
            let _ = load(Path::new("q.x"), &bytes[..cut], &Limits::viewer());
        }
        let broken = QUAD.replace("4; 0;0;0;", "4294967295; 0;0;0;").replace("4;0,3,2,1", "4;0,1,99,-3");
        let _ = load(Path::new("q.x"), broken.as_bytes(), &Limits::viewer());
        let deep = format!("xof 0303txt 0032\n{}", "Frame a {".repeat(10_000));
        let _ = load(Path::new("q.x"), deep.as_bytes(), &Limits::viewer());
        let limits = Limits { max_triangles: 1, ..Limits::viewer() };
        assert!(matches!(load(Path::new("q.x"), bytes, &limits), Err(LoadError::TooLarge(_))));
    }
}
