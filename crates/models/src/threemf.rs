//! 3MF, the 3D Manufacturing Format of 3D printing: a zip package (OPC) of
//! XML model parts, textures and a thumbnail.
//!
//! Covered: the core specification (objects with meshes or components,
//! build items with their transforms, units), the Materials and Properties
//! extension (base materials with their display properties, colour groups,
//! 2D textures with their coordinate groups, composite materials and
//! multi-property layers, per triangle and per corner), the Production
//! extension (items and components in other model parts, each part with its
//! own object ids) and the Beam Lattice extension, drawn as tubes with
//! balls at their joints.  Slices and volumetric data are ignored.
//!
//! Model parts are read as a stream rather than a document tree, since
//! slicers write models of hundreds of megabytes of XML.  3MF is Z-up, so the
//! scene is turned Y-up, and scaled to metres from the model's unit.

use crate::archive::{Zip, zip_find, zip_open, zip_read};
use crate::common::{TextureSet, check_triangles, parse_f32, srgb_color};
use crate::polymesh::{Attr, Binding, PolyMesh, Smoothing};
use glam::{Mat4, Vec2, Vec3, Vec4};
use quick_xml::events::{BytesStart, Event};
use slerp_scene::textures::DecodeOptions;
use slerp_scene::{AlphaMode, Filter, Limits, LoadError, Material, Mesh, Node, Scene, TexRef, Transform, Wrap};
use std::collections::HashMap;
use std::path::Path;

/// Component chains deeper than this are malformed or malicious.
const MAX_DEPTH: usize = 64;
/// Components can multiply; the scene stops growing past this many nodes.
const MAX_NODES: usize = 1 << 20;
/// The model parts one package may pull in.
const MAX_PARTS: usize = 256;
/// Sides of the tubes beams are drawn as.
const BEAM_SIDES: usize = 8;

/// True for a zip whose first entries name a 3MF model part, or any zip
/// named `.3mf`.
pub fn looks_like_3mf(head: &[u8], by_extension: bool) -> bool {
    head.starts_with(b"PK\x03\x04")
        && (by_extension
            || crate::common::find(head, b"3D/3dmodel.model").is_some()
            || crate::common::find(head, b"3dmanufacturing").is_some())
}

/*┌─────────────────╮
| Model parts     |
└─────────────────╯*/
//region Model parts

#[derive(Default)]
struct Base {
    name: String,
    color: Vec4,
    /// Display properties: a group id and this base's index in it.
    display: Option<(u32, usize)>,
}

#[derive(Default)]
struct TexGroup {
    texture: u32,
    coords: Vec<Vec2>,
}

#[derive(Default)]
struct Composite {
    bases: u32,
    indices: Vec<u32>,
    mixes: Vec<Vec<f32>>,
}

#[derive(Default)]
struct Multi {
    groups: Vec<u32>,
    entries: Vec<Vec<u32>>,
}

#[derive(Clone, Default)]
struct Texture2d {
    path: String,
    wrap_u: Option<Wrap>,
    wrap_v: Option<Wrap>,
    nearest: bool,
}

/// Physically based display properties for one base material.
#[derive(Clone, Copy, Default)]
struct Display {
    metallic: Option<f32>,
    roughness: Option<f32>,
    translucent: bool,
}

#[derive(Clone, Copy)]
struct Triangle {
    v: [u32; 3],
    pid: u32,
    p: [u32; 3],
}

#[derive(Clone, Copy)]
struct Beam {
    v: [u32; 2],
    r: [f32; 2],
}

#[derive(Default)]
struct MeshData {
    positions: Vec<Vec3>,
    triangles: Vec<Triangle>,
    beams: Vec<Beam>,
    beam_radius: f32,
    balls: bool,
}

struct Component {
    object: u32,
    transform: Mat4,
    path: Option<String>,
}

#[derive(Default)]
struct Object {
    name: String,
    kind: String,
    pid: u32,
    pindex: u32,
    mesh: Option<MeshData>,
    components: Vec<Component>,
}

/// One model part: its resources and, for the root part, the build.
#[derive(Default)]
struct Part {
    unit: f32,
    unit_name: &'static str,
    objects: HashMap<u32, Object>,
    bases: HashMap<u32, Vec<Base>>,
    colors: HashMap<u32, Vec<Vec4>>,
    tex_groups: HashMap<u32, TexGroup>,
    textures: HashMap<u32, Texture2d>,
    composites: HashMap<u32, Composite>,
    multis: HashMap<u32, Multi>,
    displays: HashMap<u32, Vec<Display>>,
    items: Vec<Component>,
    metadata: Vec<(String, String)>,
}

/// No property: the object's default applies, or none.
const NONE: u32 = u32::MAX;

fn attr<'a>(e: &'a BytesStart, name: &[u8]) -> Option<std::borrow::Cow<'a, [u8]>> {
    e.attributes().with_checks(false).flatten().find(|a| a.key.local_name().as_ref() == name).map(|a| a.value)
}

/// Calls `f` with each attribute's local name and raw value.
fn for_each_attr(e: &BytesStart, mut f: impl FnMut(&[u8], &[u8])) {
    for a in e.attributes().with_checks(false).flatten() {
        f(a.key.local_name().as_ref(), &a.value);
    }
}

/// A non-negative decimal integer, or `NONE`.
fn parse_u32(v: &[u8]) -> u32 {
    let v = crate::common::trim(v);
    if v.is_empty() || v.len() > 10 {
        return NONE;
    }
    let mut n: u64 = 0;
    for &c in v {
        if !c.is_ascii_digit() {
            return NONE;
        }
        n = n * 10 + (c - b'0') as u64;
    }
    u32::try_from(n).unwrap_or(NONE)
}

fn attr_u32(e: &BytesStart, name: &[u8]) -> Option<u32> {
    let v = attr(e, name)?;
    std::str::from_utf8(&v).ok()?.trim().parse().ok()
}

fn attr_f32(e: &BytesStart, name: &[u8]) -> Option<f32> {
    attr(e, name).and_then(|v| parse_f32(crate::common::trim(&v))).filter(|v| v.is_finite())
}

fn attr_string(e: &BytesStart, name: &[u8]) -> Option<String> {
    attr(e, name).map(|v| unescape(&v))
}

/// Replaces XML's predefined entities and character references.
fn unescape(v: &[u8]) -> String {
    let text = String::from_utf8_lossy(v);
    if !text.contains('&') {
        return text.into_owned();
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = &*text;
    while let Some(at) = rest.find('&') {
        out.push_str(&rest[..at]);
        rest = &rest[at..];
        let Some(end) = rest.find(';').filter(|&e| e <= 12) else {
            out.push('&');
            rest = &rest[1..];
            continue;
        };
        match entity(&rest[1..end]) {
            Some(c) => out.push(c),
            None => out.push_str(&rest[..=end]),
        }
        rest = &rest[end + 1..];
    }
    out.push_str(rest);
    out
}

/// The character an entity name (without `&` and `;`) stands for.
fn entity(name: &str) -> Option<char> {
    match name {
        "amp" => Some('&'),
        "lt" => Some('<'),
        "gt" => Some('>'),
        "quot" => Some('"'),
        "apos" => Some('\''),
        _ => {
            let n = name.strip_prefix('#')?;
            let code = match n.strip_prefix('x').or_else(|| n.strip_prefix('X')) {
                Some(hex) => u32::from_str_radix(hex, 16).ok()?,
                None => n.parse().ok()?,
            };
            char::from_u32(code)
        }
    }
}

fn numbers(v: &[u8]) -> Vec<f32> {
    crate::common::tokens(v).map(|t| parse_f32(t).unwrap_or(0.0)).collect()
}

/// A 3MF transform: twelve numbers, the first three rows of a matrix that
/// multiplies row vectors.
fn transform(e: &BytesStart) -> Mat4 {
    let Some(v) = attr(e, b"transform") else { return Mat4::IDENTITY };
    let m = numbers(&v);
    if m.len() < 12 || !m.iter().all(|x| x.is_finite()) {
        return Mat4::IDENTITY;
    }
    Mat4::from_cols(
        Vec4::new(m[0], m[1], m[2], 0.0),
        Vec4::new(m[3], m[4], m[5], 0.0),
        Vec4::new(m[6], m[7], m[8], 0.0),
        Vec4::new(m[9], m[10], m[11], 1.0),
    )
}

/// `#RRGGBB` or `#RRGGBBAA`, sRGB, as linear RGBA.
fn hex_color(v: &[u8]) -> Option<Vec4> {
    let v = crate::common::trim(v).strip_prefix(b"#")?;
    if v.len() != 6 && v.len() != 8 {
        return None;
    }
    let byte = |i: usize| -> Option<f32> {
        let s = std::str::from_utf8(v.get(i * 2..i * 2 + 2)?).ok()?;
        Some(u8::from_str_radix(s, 16).ok()? as f32 / 255.0)
    };
    let a = if v.len() == 8 { byte(3)? } else { 1.0 };
    Some(srgb_color(byte(0)?, byte(1)?, byte(2)?, a))
}

fn wrap(v: Option<std::borrow::Cow<[u8]>>) -> Option<Wrap> {
    match v.as_deref()? {
        b"mirror" => Some(Wrap::MirroredRepeat),
        b"clamp" | b"none" => Some(Wrap::ClampToEdge),
        _ => Some(Wrap::Repeat),
    }
}

/// Which group the elements being read belong to.
enum Open {
    None,
    Bases(u32),
    Colors(u32),
    TexCoords(u32),
    Composite(u32),
    Multi(u32),
    Display(u32),
}

/// Reads one model part.  Malformed XML ends the part where it breaks,
/// keeping what was read before, with a warning.
fn parse_part(xml: &[u8], limits: &Limits, warnings: &mut Vec<String>) -> Result<Part, LoadError> {
    let mut reader = quick_xml::Reader::from_reader(xml);
    reader.config_mut().check_end_names = false;
    let mut part = Part { unit: 0.001, unit_name: "millimetre", ..Default::default() };
    let mut object: Option<(u32, Object)> = None;
    let mut open = Open::None;
    let mut metadata: Option<(String, String)> = None;
    let mut in_build = false;
    let mut triangles = 0u64;
    let mut root_seen = false;
    loop {
        let event = match reader.read_event() {
            Ok(Event::Eof) => break,
            Ok(e) => e,
            Err(e) => {
                warnings.push(format!("3MF model XML: {e}"));
                break;
            }
        };
        let (e, start) = match &event {
            Event::Start(e) => (e, true),
            Event::Empty(e) => (e, false),
            Event::Text(t) => {
                if let Some((_, value)) = metadata.as_mut() {
                    value.push_str(&String::from_utf8_lossy(t.as_ref()));
                }
                continue;
            }
            Event::GeneralRef(r) => {
                if let Some((_, value)) = metadata.as_mut()
                    && let Some(c) = entity(&String::from_utf8_lossy(r.as_ref()))
                {
                    value.push(c);
                }
                continue;
            }
            Event::End(e) => {
                match e.local_name().as_ref() {
                    b"object" => {
                        if let Some((id, o)) = object.take() {
                            part.objects.insert(id, o);
                        }
                    }
                    b"basematerials"
                    | b"colorgroup"
                    | b"texture2dgroup"
                    | b"compositematerials"
                    | b"multiproperties"
                    | b"pbmetallicdisplayproperties"
                    | b"pbspeculardisplayproperties"
                    | b"translucentdisplayproperties" => open = Open::None,
                    b"build" => in_build = false,
                    b"metadata" => {
                        if let Some((name, value)) = metadata.take() {
                            part.metadata.push((name, value.trim().to_string()));
                        }
                    }
                    _ => {}
                }
                continue;
            }
            _ => continue,
        };
        let name = e.local_name();
        match name.as_ref() {
            b"model" => {
                root_seen = true;
                (part.unit, part.unit_name) = match attr(e, b"unit").as_deref() {
                    Some(b"micron") => (1e-6, "micron"),
                    Some(b"centimeter") => (0.01, "centimetre"),
                    Some(b"inch") => (0.0254, "inch"),
                    Some(b"foot") => (0.3048, "foot"),
                    Some(b"meter") => (1.0, "metre"),
                    _ => (0.001, "millimetre"),
                };
            }
            b"metadata" if start => metadata = attr_string(e, b"name").map(|n| (n, String::new())),
            b"object" => {
                let id = attr_u32(e, b"id").unwrap_or(NONE);
                let o = Object {
                    name: attr_string(e, b"name").unwrap_or_default(),
                    kind: attr_string(e, b"type").unwrap_or_default(),
                    pid: attr_u32(e, b"pid").unwrap_or(NONE),
                    pindex: attr_u32(e, b"pindex").unwrap_or(0),
                    ..Default::default()
                };
                if start {
                    object = Some((id, o));
                } else {
                    part.objects.insert(id, o);
                }
            }
            b"mesh" => {
                if let Some((_, o)) = object.as_mut() {
                    o.mesh = Some(MeshData::default());
                }
            }
            b"vertex" => {
                if let Some(m) = object.as_mut().and_then(|o| o.1.mesh.as_mut()) {
                    let mut v = Vec3::NAN;
                    for_each_attr(e, |name, value| {
                        let slot = match name {
                            b"x" => 0,
                            b"y" => 1,
                            b"z" => 2,
                            _ => return,
                        };
                        v[slot] = parse_f32(value).unwrap_or(f32::NAN);
                    });
                    m.positions.push(v);
                }
            }
            b"triangle" => {
                if let Some(m) = object.as_mut().and_then(|o| o.1.mesh.as_mut()) {
                    triangles += 1;
                    if triangles.is_multiple_of(65536) {
                        check_triangles(triangles, limits)?;
                    }
                    // v1 v2 v3 pid p1 p2 p3, read in one pass: this is the
                    // hot loop of large files.
                    let mut f = [NONE; 7];
                    for_each_attr(e, |name, value| {
                        let slot = match name {
                            b"v1" => 0,
                            b"v2" => 1,
                            b"v3" => 2,
                            b"pid" => 3,
                            b"p1" => 4,
                            b"p2" => 5,
                            b"p3" => 6,
                            _ => return,
                        };
                        f[slot] = parse_u32(value);
                    });
                    let p2 = if f[5] == NONE { f[4] } else { f[5] };
                    let p3 = if f[6] == NONE { f[4] } else { f[6] };
                    m.triangles.push(Triangle { v: [f[0], f[1], f[2]], pid: f[3], p: [f[4], p2, p3] });
                }
            }
            b"beamlattice" => {
                if let Some(m) = object.as_mut().and_then(|o| o.1.mesh.as_mut()) {
                    m.beam_radius = attr_f32(e, b"radius").unwrap_or(1.0);
                    m.balls = attr(e, b"cap").as_deref() != Some(b"butt");
                }
            }
            b"beam" => {
                if let Some(m) = object.as_mut().and_then(|o| o.1.mesh.as_mut()) {
                    let r1 = attr_f32(e, b"r1").unwrap_or(m.beam_radius);
                    let r2 = attr_f32(e, b"r2").unwrap_or(r1);
                    let v = [attr_u32(e, b"v1").unwrap_or(NONE), attr_u32(e, b"v2").unwrap_or(NONE)];
                    m.beams.push(Beam { v, r: [r1, r2] });
                }
            }
            b"component" => {
                if let Some((_, o)) = object.as_mut() {
                    o.components.push(Component {
                        object: attr_u32(e, b"objectid").unwrap_or(NONE),
                        transform: transform(e),
                        path: attr_string(e, b"path"),
                    });
                }
            }
            b"build" => in_build = start,
            b"item" if in_build || object.is_none() => part.items.push(Component {
                object: attr_u32(e, b"objectid").unwrap_or(NONE),
                transform: transform(e),
                path: attr_string(e, b"path"),
            }),
            b"basematerials" => {
                let id = attr_u32(e, b"id").unwrap_or(NONE);
                part.bases.entry(id).or_default();
                if start {
                    open = Open::Bases(id);
                }
                let display = attr_u32(e, b"displaypropertiesid");
                if let (Some(d), Some(list)) = (display, part.bases.get_mut(&id)) {
                    list.iter_mut().enumerate().for_each(|(i, b)| b.display = b.display.or(Some((d, i))));
                }
            }
            b"base" => {
                if let Open::Bases(id) = open {
                    let list = part.bases.entry(id).or_default();
                    let display = attr_u32(e, b"displaypropertiesid").map(|d| (d, list.len()));
                    list.push(Base {
                        name: attr_string(e, b"name").unwrap_or_default(),
                        color: attr(e, b"displaycolor").and_then(|v| hex_color(&v)).unwrap_or(Vec4::splat(0.8)),
                        display,
                    });
                }
            }
            b"colorgroup" => {
                let id = attr_u32(e, b"id").unwrap_or(NONE);
                part.colors.entry(id).or_default();
                if start {
                    open = Open::Colors(id);
                }
            }
            b"color" => {
                if let Open::Colors(id) = open {
                    let c = attr(e, b"color").and_then(|v| hex_color(&v)).unwrap_or(Vec4::ONE);
                    part.colors.entry(id).or_default().push(c);
                }
            }
            b"texture2d" => {
                let id = attr_u32(e, b"id").unwrap_or(NONE);
                part.textures.insert(
                    id,
                    Texture2d {
                        path: attr_string(e, b"path").unwrap_or_default(),
                        wrap_u: wrap(attr(e, b"tilestyleu")),
                        wrap_v: wrap(attr(e, b"tilestylev")),
                        nearest: attr(e, b"filter").as_deref() == Some(b"nearest"),
                    },
                );
            }
            b"texture2dgroup" => {
                let id = attr_u32(e, b"id").unwrap_or(NONE);
                let texture = attr_u32(e, b"texid").unwrap_or(NONE);
                part.tex_groups.insert(id, TexGroup { texture, coords: Vec::new() });
                if start {
                    open = Open::TexCoords(id);
                }
            }
            b"tex2coord" => {
                if let Open::TexCoords(id) = open
                    && let Some(g) = part.tex_groups.get_mut(&id)
                {
                    let u = attr_f32(e, b"u").unwrap_or(0.0);
                    let v = attr_f32(e, b"v").unwrap_or(0.0);
                    g.coords.push(Vec2::new(u, 1.0 - v));
                }
            }
            b"compositematerials" => {
                let id = attr_u32(e, b"id").unwrap_or(NONE);
                let indices = attr(e, b"matindices").map(|v| numbers(&v).iter().map(|&f| f as u32).collect());
                part.composites.insert(
                    id,
                    Composite {
                        bases: attr_u32(e, b"matid").unwrap_or(NONE),
                        indices: indices.unwrap_or_default(),
                        mixes: Vec::new(),
                    },
                );
                if start {
                    open = Open::Composite(id);
                }
            }
            b"composite" => {
                if let Open::Composite(id) = open
                    && let Some(c) = part.composites.get_mut(&id)
                {
                    c.mixes.push(attr(e, b"values").map(|v| numbers(&v)).unwrap_or_default());
                }
            }
            b"multiproperties" => {
                let id = attr_u32(e, b"id").unwrap_or(NONE);
                let groups = attr(e, b"pids").map(|v| numbers(&v).iter().map(|&f| f as u32).collect());
                part.multis.insert(id, Multi { groups: groups.unwrap_or_default(), entries: Vec::new() });
                if start {
                    open = Open::Multi(id);
                }
            }
            b"multi" => {
                if let Open::Multi(id) = open
                    && let Some(m) = part.multis.get_mut(&id)
                {
                    let ix = attr(e, b"pindices").map(|v| numbers(&v).iter().map(|&f| f as u32).collect());
                    m.entries.push(ix.unwrap_or_default());
                }
            }
            b"pbmetallicdisplayproperties" | b"pbspeculardisplayproperties" | b"translucentdisplayproperties" => {
                let id = attr_u32(e, b"id").unwrap_or(NONE);
                part.displays.entry(id).or_default();
                if start {
                    open = Open::Display(id);
                }
            }
            b"pbmetallic" | b"pbspecular" | b"translucent" => {
                if let Open::Display(id) = open {
                    let d = match name.as_ref() {
                        b"pbmetallic" => Display {
                            metallic: attr_f32(e, b"metallicness"),
                            roughness: attr_f32(e, b"roughness"),
                            translucent: false,
                        },
                        b"pbspecular" => Display {
                            metallic: Some(0.0),
                            roughness: attr_f32(e, b"glossiness").map(|g| 1.0 - g),
                            translucent: false,
                        },
                        _ => Display { metallic: Some(0.0), roughness: attr_f32(e, b"roughness"), translucent: true },
                    };
                    part.displays.entry(id).or_default().push(d);
                }
            }
            _ => {}
        }
    }
    if let Some((id, o)) = object.take() {
        part.objects.insert(id, o);
    }
    if !root_seen {
        return Err(LoadError::invalid("3MF model part has no model element"));
    }
    check_triangles(triangles, limits)?;
    Ok(part)
}

//endregion

/*┌─────────────────╮
| Properties      |
└─────────────────╯*/
//region Properties

/// What one corner looks like once its property group is resolved.
#[derive(Clone, Copy)]
struct Look {
    /// Linear RGBA, multiplied into the material.
    color: Vec4,
    uv: Option<Vec2>,
    /// Texture layers bottom to top, `NONE` after the last.  Layers share
    /// the first one's coordinates.
    textures: [u32; LAYERS],
    base: Option<(u32, u32)>,
    /// The colour comes from a colour or composite group, so it varies per
    /// corner and goes into vertex colours.
    painted: bool,
}

/// Texture layers one multi-property may stack.
const LAYERS: usize = 4;

impl Look {
    const PLAIN: Look = Look { color: Vec4::ONE, uv: None, textures: [NONE; LAYERS], base: None, painted: false };
}

impl Part {
    /// Resolves property `index` of group `group`; `depth` stops multi
    /// properties that name each other.
    fn look(&self, group: u32, index: u32, depth: usize) -> Option<Look> {
        let i = index as usize;
        if let Some(bases) = self.bases.get(&group) {
            let b = bases.get(i)?;
            return Some(Look { color: b.color, base: Some((group, index)), ..Look::PLAIN });
        }
        if let Some(colors) = self.colors.get(&group) {
            return Some(Look { color: *colors.get(i)?, painted: true, ..Look::PLAIN });
        }
        if let Some(g) = self.tex_groups.get(&group) {
            let mut textures = [NONE; LAYERS];
            textures[0] = g.texture;
            return Some(Look { uv: Some(*g.coords.get(i)?), textures, ..Look::PLAIN });
        }
        if let Some(c) = self.composites.get(&group) {
            let bases = self.bases.get(&c.bases)?;
            let mix = c.mixes.get(i)?;
            let mut color = Vec4::ZERO;
            let mut total = 0.0;
            for (k, &w) in mix.iter().enumerate() {
                let Some(b) = c.indices.get(k).and_then(|&j| bases.get(j as usize)) else { continue };
                color += b.color * w;
                total += w;
            }
            let color = if total > 0.0 { color / total } else { Vec4::ONE };
            return Some(Look { color, painted: true, ..Look::PLAIN });
        }
        if let Some(m) = self.multis.get(&group) {
            if depth > 2 {
                return None;
            }
            let entry = m.entries.get(i)?;
            let mut look = Look::PLAIN;
            for (k, &g) in m.groups.iter().enumerate() {
                let Some(layer) = self.look(g, entry.get(k).copied().unwrap_or(0), depth + 1) else { continue };
                look.color *= layer.color;
                look.painted |= layer.painted;
                look.base = look.base.or(layer.base);
                // Texture layers stack when they map the same way, which
                // lets them be composed into one picture.
                for &t in layer.textures.iter().take_while(|&&t| t != NONE) {
                    let Some(free) = look.textures.iter().position(|&x| x == NONE) else { break };
                    if free == 0 {
                        look.uv = layer.uv;
                    } else if layer.uv != look.uv {
                        continue;
                    }
                    look.textures[free] = t;
                }
            }
            return Some(look);
        }
        None
    }
}

/// A material per distinct (texture, base material, painted) combination.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct MaterialKey {
    part: usize,
    textures: [u32; LAYERS],
    base: Option<(u32, u32)>,
    painted: bool,
    /// A single colour under the textures, as `f32` bits.
    tint: Option<[u32; 4]>,
}

//endregion

/*┌─────────────────╮
| Scene           |
└─────────────────╯*/
//region Scene

struct Builder<'a> {
    zip: Zip,
    limits: &'a Limits,
    /// Parsed parts by path; index 0 is the root part.
    parts: Vec<(String, Part)>,
    textures: TextureSet<'a>,
    texture_ids: HashMap<(usize, u32), Option<usize>>,
    materials: HashMap<MaterialKey, usize>,
    meshes: HashMap<(usize, u32), Option<usize>>,
    scene: Scene,
    warnings: Vec<String>,
}

impl Builder<'_> {
    /// The part a path names, read on first use.
    fn part(&mut self, path: &str) -> Option<usize> {
        let key = path.trim_start_matches('/').to_ascii_lowercase();
        if let Some(i) = self.parts.iter().position(|(p, _)| *p == key) {
            return Some(i);
        }
        if self.parts.len() >= MAX_PARTS {
            return None;
        }
        let Some(entry) = zip_find(&self.zip, &key).cloned() else {
            self.warnings.push(format!("3MF part not found: {path}"));
            return None;
        };
        let parsed = zip_read(&self.zip, &entry, self.limits.max_file_bytes)
            .and_then(|xml| parse_part(&xml, self.limits, &mut self.warnings));
        match parsed {
            Ok(part) => {
                self.parts.push((key, part));
                Some(self.parts.len() - 1)
            }
            Err(e) => {
                self.warnings.push(format!("3MF part {path}: {e}"));
                None
            }
        }
    }

    fn texture(&mut self, part: usize, id: u32) -> Option<(usize, Texture2d)> {
        let info = self.parts[part].1.textures.get(&id).cloned()?;
        if let Some(&t) = self.texture_ids.get(&(part, id)) {
            return t.map(|t| (t, info));
        }
        let loaded = match zip_find(&self.zip, &info.path).cloned() {
            Some(entry) => match zip_read(&self.zip, &entry, self.limits.max_file_bytes) {
                Ok(bytes) => self.textures.embedded(&info.path, &bytes),
                Err(e) => {
                    self.warnings.push(format!("texture {}: {e}", info.path));
                    None
                }
            },
            None => {
                self.warnings.push(format!("texture not found in package: {}", info.path));
                None
            }
        };
        self.texture_ids.insert((part, id), loaded);
        loaded.map(|t| (t, info))
    }

    fn material(&mut self, key: MaterialKey) -> usize {
        if let Some(&m) = self.materials.get(&key) {
            return m;
        }
        let mut m = Material { double_sided: true, ..Material::default() };
        if let Some((g, i)) = key.base {
            let part = &self.parts[key.part].1;
            if let Some(b) = part.bases.get(&g).and_then(|l| l.get(i as usize)) {
                m.name = b.name.clone();
                m.base_color = b.color;
                let display = b.display.and_then(|(d, k)| part.displays.get(&d)?.get(k).copied());
                if let Some(d) = display {
                    m.metallic = d.metallic.unwrap_or(0.0).clamp(0.0, 1.0);
                    m.roughness = d.roughness.unwrap_or(0.5).clamp(0.0, 1.0);
                    if d.translucent {
                        m.base_color.w = m.base_color.w.min(0.5);
                    }
                }
            }
        }
        if key.painted {
            m.base_color = Vec4::new(1.0, 1.0, 1.0, m.base_color.w);
        }
        let layers: Vec<(usize, Texture2d)> =
            key.textures.iter().take_while(|&&t| t != NONE).filter_map(|&id| self.texture(key.part, id)).collect();
        if let Some((first, info)) = layers.first().cloned() {
            // Texture alpha lets lower layers show through, not the object:
            // layers over a base colour or over each other are composed
            // into one opaque picture.
            let tint = key.tint.map(|c| Vec4::from_array(c.map(f32::from_bits)));
            let base = key.base.map(|_| m.base_color).or(tint);
            let texture = if base.is_some() || layers.len() > 1 {
                let ids: Vec<usize> = layers.iter().map(|l| l.0).collect();
                compose(&mut self.textures, &ids, base).unwrap_or(first)
            } else {
                first
            };
            let mut tex = TexRef::new(texture);
            tex.sampler.wrap_u = info.wrap_u.unwrap_or(Wrap::Repeat);
            tex.sampler.wrap_v = info.wrap_v.unwrap_or(Wrap::Repeat);
            if info.nearest {
                tex.sampler.mag = Filter::Nearest;
                tex.sampler.min = Filter::Nearest;
            }
            m.base_color_texture = Some(tex);
            m.base_color = Vec4::new(1.0, 1.0, 1.0, m.base_color.w);
            if m.name.is_empty() {
                m.name = info.path.rsplit('/').next().unwrap_or_default().to_string();
            }
        }
        let texture_alpha =
            m.base_color_texture.and_then(|t| self.textures.get(t.texture)).is_some_and(|t| t.has_alpha);
        if m.base_color.w < 0.999 || texture_alpha {
            m.alpha_mode = AlphaMode::Blend;
        }
        self.scene.materials.push(m);
        let index = self.scene.materials.len() - 1;
        self.materials.insert(key, index);
        index
    }

    /// The mesh of object `id` in `part`, built on first use.
    fn mesh(&mut self, part: usize, id: u32) -> Option<usize> {
        if let Some(&m) = self.meshes.get(&(part, id)) {
            return m;
        }
        let built = self.build_mesh(part, id);
        self.meshes.insert((part, id), built);
        built
    }

    fn build_mesh(&mut self, part: usize, id: u32) -> Option<usize> {
        let p = &self.parts[part].1;
        let o = p.objects.get(&id)?;
        let data = o.mesh.as_ref()?;
        let name = o.name.clone();
        // 1. Each corner's look.
        let looks: Vec<[Look; 3]> = data
            .triangles
            .iter()
            .map(|t| {
                let (group, p1) = if t.pid != NONE { (t.pid, t.p[0]) } else { (o.pid, o.pindex) };
                let corners: [u32; 3] = if t.pid != NONE && t.p[0] != NONE { t.p } else { [p1; 3] };
                let first = if group == NONE { None } else { p.look(group, corners[0], 0) };
                // Base materials can't vary across a triangle.
                match first {
                    Some(l) if p.bases.contains_key(&group) => [l; 3],
                    Some(l) => {
                        [l, p.look(group, corners[1], 0).unwrap_or(l), p.look(group, corners[2], 0).unwrap_or(l)]
                    }
                    None => [Look::PLAIN; 3],
                }
            })
            .collect();

        // 2. Textures over one plain colour (a colour group with a single
        //    entry under a multi-property, say) are composed onto that
        //    colour like textures over a base material, rather than tinting
        //    them through vertex colours.
        let mut tints: HashMap<[u32; LAYERS], Option<Vec4>> = HashMap::new();
        for l in looks.iter().flatten().filter(|l| l.painted && l.textures[0] != NONE) {
            let tint = tints.entry(l.textures).or_insert(Some(l.color));
            if *tint != Some(l.color) {
                *tint = None;
            }
        }

        // 3. A material slot per triangle, and the corners' attributes.
        let mut pm =
            PolyMesh { positions: data.positions.clone(), smoothing: Smoothing::default(), ..Default::default() };
        let mut keys: Vec<MaterialKey> = Vec::new();
        let mut uvs = Vec::new();
        let mut colors = Vec::new();
        let (mut any_uv, mut any_paint) = (false, false);
        for (t, looks) in data.triangles.iter().zip(&looks) {
            let l = looks[0];
            let tint =
                if l.painted && l.textures[0] != NONE { tints.get(&l.textures).copied().flatten() } else { None };
            let key = MaterialKey {
                part,
                textures: l.textures,
                base: l.base,
                painted: l.painted && tint.is_none(),
                tint: tint.map(|c| c.to_array().map(f32::to_bits)),
            };
            let slot = match keys.iter().position(|k| *k == key) {
                Some(s) => s,
                None => {
                    keys.push(key);
                    keys.len() - 1
                }
            };
            pm.add_face(&t.v);
            pm.face_slots.push(slot as u32);
            for l in looks {
                any_uv |= l.uv.is_some();
                any_paint |= key.painted;
                uvs.push(l.uv.unwrap_or(Vec2::ZERO));
                colors.push(if key.painted { l.color } else { Vec4::ONE });
            }
        }
        if any_uv {
            pm.uvs = Some(Attr::new(uvs, Binding::Corner));
        }
        if any_paint {
            pm.colors = Some(Attr::new(colors, Binding::Corner));
        }
        let mut prims = pm.build(&mut self.warnings);

        // 4. Beams as tubes, in the object's base material.
        if !data.beams.is_empty() {
            let base = (o.pid != NONE).then_some((o.pid, o.pindex));
            let key = MaterialKey { part, textures: [NONE; LAYERS], base, painted: false, tint: None };
            let slot = match keys.iter().position(|k| *k == key) {
                Some(s) => s,
                None => {
                    keys.push(key);
                    keys.len() - 1
                }
            };
            let lattice = beam_lattice(data);
            if !lattice.indices.is_empty() {
                prims.push((slot as u32, lattice));
            }
        }
        if prims.is_empty() {
            return None;
        }
        let primitives = prims
            .into_iter()
            .map(|(slot, mut prim)| {
                prim.material = keys.get(slot as usize).map(|&k| self.material(k));
                prim
            })
            .collect();
        self.scene.meshes.push(Mesh { name, primitives, target_names: Vec::new() });
        Some(self.scene.meshes.len() - 1)
    }

    /// Adds a node for object `id` of `part` and its components.
    fn node(&mut self, part: usize, id: u32, transform: Mat4, depth: usize) -> Option<usize> {
        if depth > MAX_DEPTH || self.scene.nodes.len() >= MAX_NODES {
            return None;
        }
        let (name, kind, components) = {
            let o = self.parts[part].1.objects.get(&id)?;
            let comps: Vec<(u32, Mat4, Option<String>)> =
                o.components.iter().map(|c| (c.object, c.transform, c.path.clone())).collect();
            (o.name.clone(), o.kind.clone(), comps)
        };
        let mesh = self.mesh(part, id);
        let mut node = Node::new(if name.is_empty() { format!("object {id}") } else { name });
        node.transform = Transform::from_matrix(transform);
        node.mesh = mesh;
        node.visible = !matches!(kind.as_str(), "support" | "solidsupport");
        self.scene.nodes.push(node);
        let index = self.scene.nodes.len() - 1;
        for (object, m, path) in components {
            let target = match path {
                Some(p) => self.part(&p),
                None => Some(part),
            };
            if let Some(t) = target
                && let Some(child) = self.node(t, object, m, depth + 1)
            {
                self.scene.nodes[index].children.push(child);
            }
        }
        Some(index)
    }
}

/// Composes texture layers, bottom first, each mixed over the ones below by
/// its alpha, onto `base` when there is one (which makes the result opaque
/// apart from the base's own alpha).  Layers are sampled at the first
/// layer's size.
fn compose(textures: &mut TextureSet, layers: &[usize], base: Option<Vec4>) -> Option<usize> {
    let (w, h, first) = crate::common::rgba8(textures.get(*layers.first()?)?)?;
    let (w, h) = (w as usize, h as usize);
    let mut out: Vec<f32> = match base {
        Some(c) => {
            let encode = |v: f32| crate::common::linear_to_srgb(v);
            let px = [encode(c.x), encode(c.y), encode(c.z), c.w.clamp(0.0, 1.0)];
            px.iter().copied().cycle().take(w * h * 4).collect()
        }
        None => vec![0.0; w * h * 4],
    };
    let mut first = Some(first.into_owned());
    for &layer in layers {
        let (lw, lh, px) = match first.take() {
            Some(px) => (w as u32, h as u32, std::borrow::Cow::Owned(px)),
            None => crate::common::rgba8(textures.get(layer)?)?,
        };
        let (lw, lh) = (lw as usize, lh as usize);
        for y in 0..h {
            let sy = y * lh / h;
            for x in 0..w {
                let sx = x * lw / w;
                let s = &px[(sy * lw + sx) * 4..(sy * lw + sx) * 4 + 4];
                let d = &mut out[(y * w + x) * 4..(y * w + x) * 4 + 4];
                let a = s[3] as f32 / 255.0;
                for c in 0..3 {
                    d[c] = d[c] * (1.0 - a) + s[c] as f32 / 255.0 * a;
                }
                if base.is_none() {
                    d[3] = d[3] + a * (1.0 - d[3]);
                }
            }
        }
    }
    let bytes = out.iter().map(|&v| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8).collect();
    Some(textures.push(slerp_scene::Texture::from_rgba8(w as u32, h as u32, bytes, slerp_scene::ColorSpace::Srgb)))
}

/// Beams as tubes of `BEAM_SIDES` sides, with balls where they meet when
/// the lattice caps them with spheres.
fn beam_lattice(data: &MeshData) -> slerp_scene::Primitive {
    let mut p = slerp_scene::Primitive::default();
    let mut joint_radius: HashMap<u32, f32> = HashMap::new();
    for b in &data.beams {
        let (Some(&a), Some(&c)) = (data.positions.get(b.v[0] as usize), data.positions.get(b.v[1] as usize)) else {
            continue;
        };
        let (ra, rc) = (b.r[0].abs(), b.r[1].abs());
        if !a.is_finite() || !c.is_finite() || !ra.is_finite() || !rc.is_finite() {
            continue;
        }
        let axis = (c - a).normalize_or_zero();
        if axis == Vec3::ZERO {
            continue;
        }
        let side = axis.any_orthonormal_vector();
        let up = axis.cross(side);
        let base = p.positions.len() as u32;
        for k in 0..BEAM_SIDES {
            let angle = k as f32 / BEAM_SIDES as f32 * std::f32::consts::TAU;
            let n = side * angle.cos() + up * angle.sin();
            p.positions.push(a + n * ra);
            p.positions.push(c + n * rc);
            p.normals.push(n);
            p.normals.push(n);
        }
        for k in 0..BEAM_SIDES as u32 {
            let (i, j) = (base + 2 * k, base + 2 * ((k + 1) % BEAM_SIDES as u32));
            p.indices.extend([i, j, i + 1, i + 1, j, j + 1]);
        }
        if data.balls {
            for (v, r) in [(b.v[0], ra), (b.v[1], rc)] {
                let e = joint_radius.entry(v).or_insert(0.0);
                *e = e.max(r);
            }
        }
    }
    // Balls: octahedra subdivided once, enough at thumbnail size.
    let ball = unit_ball();
    let mut joints: Vec<(u32, f32)> = joint_radius.into_iter().collect();
    joints.sort_unstable_by_key(|j| j.0);
    for (v, r) in joints {
        let centre = data.positions[v as usize];
        let base = p.positions.len() as u32;
        for &n in &ball.0 {
            p.positions.push(centre + n * r);
            p.normals.push(n);
        }
        p.indices.extend(ball.1.iter().map(|&i| base + i));
    }
    p
}

/// A unit sphere approximated by an octahedron subdivided once.
fn unit_ball() -> (Vec<Vec3>, Vec<u32>) {
    let mut points = vec![Vec3::X, Vec3::NEG_X, Vec3::Y, Vec3::NEG_Y, Vec3::Z, Vec3::NEG_Z];
    let faces = [[0, 2, 4], [2, 1, 4], [1, 3, 4], [3, 0, 4], [2, 0, 5], [1, 2, 5], [3, 1, 5], [0, 3, 5]];
    let mut indices = Vec::new();
    let mid = |a: usize, b: usize, points: &mut Vec<Vec3>| -> u32 {
        points.push((points[a] + points[b]).normalize());
        (points.len() - 1) as u32
    };
    for [a, b, c] in faces {
        let ab = mid(a, b, &mut points);
        let bc = mid(b, c, &mut points);
        let ca = mid(c, a, &mut points);
        let (a, b, c) = (a as u32, b as u32, c as u32);
        indices.extend([a, ab, ca, ab, b, bc, ca, bc, c, ab, bc, ca]);
    }
    (points, indices)
}

/// The root model part's path, from the package relationships.
fn root_part(zip: &Zip, limits: &Limits) -> Option<String> {
    package_relationship(zip, limits, b"/3dmodel")
}

/// The target of the package relationship whose type ends with `suffix`.
fn package_relationship(zip: &Zip, limits: &Limits, suffix: &[u8]) -> Option<String> {
    let rels = zip_find(zip, "_rels/.rels")?;
    let xml = zip_read(zip, rels, limits.max_file_bytes.min(1 << 20)).ok()?;
    let mut reader = quick_xml::Reader::from_reader(xml.as_slice());
    loop {
        match reader.read_event() {
            Ok(Event::Empty(e) | Event::Start(e)) if e.local_name().as_ref() == b"Relationship" => {
                let kind = attr(&e, b"Type").unwrap_or_default();
                if kind.ends_with(suffix) {
                    return attr_string(&e, b"Target");
                }
            }
            Ok(Event::Eof) | Err(_) => return None,
            _ => {}
        }
    }
}

/// The largest picture read as a saved thumbnail.
const MAX_PICTURE_BYTES: u64 = 16 << 20;

/// The picture a 3MF package carries of its model, read without the model:
/// the one its thumbnail relationship names (`Metadata/thumbnail.png` from
/// most programs), else the first plate's picture that Bambu Studio,
/// OrcaSlicer and PrusaSlicer save in their projects (`Metadata/plate_1.png`).
/// Their `pick_` and `top_` pictures are false-colour aids for the slicer,
/// not pictures of the model, and are never taken.  Only the zip's
/// directory and the picture itself are read, whatever the package's size.
pub fn saved_picture(path: &Path, limits: &Limits) -> Result<Option<slerp_scene::Texture>, LoadError> {
    let zip = Zip::open(path).map_err(|e| LoadError::invalid(e.to_string()))?;
    let named = package_relationship(&zip, limits, b"/metadata/thumbnail");
    let candidates = named.iter().map(String::as_str).chain(["Metadata/plate_1.png", "Metadata/thumbnail.png"]);
    for name in candidates {
        let Some(entry) = zip_find(&zip, name) else { continue };
        let Ok(bytes) = zip_read(&zip, entry, MAX_PICTURE_BYTES) else { continue };
        let opts = limits.max_texture_size.map_or_else(DecodeOptions::full, DecodeOptions::preview);
        if let Ok(mut t) = slerp_scene::textures::decode(&bytes, &opts) {
            t.source.container = "3MF".into();
            t.source.format = format!("the package's picture {}", entry.path);
            return Ok(Some(t));
        }
    }
    Ok(None)
}

pub fn load(path: &Path, bytes: &[u8], limits: &Limits) -> Result<Scene, LoadError> {
    // 1. The package and its root model part.
    let zip = zip_open(bytes)?;
    let root = root_part(&zip, limits)
        .or_else(|| {
            let e = zip.entries().iter().find(|e| e.path.to_ascii_lowercase().ends_with(".model"));
            e.map(|e| e.path.clone())
        })
        .ok_or_else(|| LoadError::invalid("3MF package has no model part"))?;
    let mut b = Builder {
        zip,
        limits,
        parts: Vec::new(),
        textures: TextureSet::new(path, limits),
        texture_ids: HashMap::new(),
        materials: HashMap::new(),
        meshes: HashMap::new(),
        scene: Scene::default(),
        warnings: Vec::new(),
    };
    let key = root.trim_start_matches('/').to_ascii_lowercase();
    let entry =
        zip_find(&b.zip, &key).cloned().ok_or_else(|| LoadError::invalid(format!("3MF part {root} missing")))?;
    let xml = zip_read(&b.zip, &entry, limits.max_file_bytes)?;
    let part = parse_part(&xml, limits, &mut b.warnings)?;
    drop(xml);
    let unit = part.unit;
    b.parts.push((key, part));

    // 2. Build items, or every object when the build is empty.
    let items: Vec<(u32, Mat4, Option<String>)> =
        b.parts[0].1.items.iter().map(|i| (i.object, i.transform, i.path.clone())).collect();
    if items.is_empty() {
        let mut ids: Vec<u32> = b.parts[0].1.objects.keys().copied().collect();
        ids.sort_unstable();
        for id in ids {
            b.node(0, id, Mat4::IDENTITY, 0);
        }
    }
    for (object, m, path) in items {
        let part = match path {
            Some(p) => b.part(&p),
            None => Some(0),
        };
        if let Some(p) = part {
            b.node(p, object, m, 0);
        }
    }
    b.scene.compute_roots();
    if b.scene.meshes.is_empty() {
        return Err(LoadError::invalid("3MF package holds no meshes"));
    }

    // 3. Units and up axis, and what the file says about itself.
    b.scene.wrap_roots(Transform {
        rotation: glam::Quat::from_rotation_x(-std::f32::consts::FRAC_PI_2),
        scale: Vec3::splat(unit),
        ..Transform::IDENTITY
    });
    let mut scene = b.scene;
    scene.info.format = "3MF".into();
    scene.info.details.push(("Unit".into(), b.parts[0].1.unit_name.into()));
    for (name, value) in &b.parts[0].1.metadata {
        match name.as_str() {
            "Application" => scene.info.generator = Some(value.clone()),
            "Title" | "Designer" | "Description" | "LicenseTerms" if !value.is_empty() => {
                scene.info.details.push((name.clone(), value.clone()))
            }
            _ => {}
        }
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
    use crate::archive::tests::stored_zip;

    const RELS: &[u8] = br#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Target="/3D/3dmodel.model" Id="rel0" Type="http://schemas.microsoft.com/3dmanufacturing/2013/01/3dmodel"/></Relationships>"#;

    const MODEL: &str = r##"<?xml version="1.0" encoding="UTF-8"?>
<model unit="centimeter" xmlns="http://schemas.microsoft.com/3dmanufacturing/core/2015/02" xmlns:m="http://schemas.microsoft.com/3dmanufacturing/material/2015/02">
  <metadata name="Title">Two &amp; one</metadata>
  <resources>
    <basematerials id="1"><base name="Red" displaycolor="#FF0000"/><base name="Glass" displaycolor="#FFFFFF80"/></basematerials>
    <m:colorgroup id="2"><m:color color="#00FF00FF"/><m:color color="#0000FFFF"/></m:colorgroup>
    <object id="3" type="model" pid="1" pindex="0" name="Tri">
      <mesh>
        <vertices><vertex x="0" y="0" z="0"/><vertex x="1" y="0" z="0"/><vertex x="0" y="1" z="0"/><vertex x="0" y="0" z="1"/></vertices>
        <triangles>
          <triangle v1="0" v2="2" v3="1"/>
          <triangle v1="0" v2="1" v3="3" pid="2" p1="0" p2="1" p3="0"/>
          <triangle v1="0" v2="3" v3="2" pid="1" p1="1"/>
          <triangle v1="1" v2="2" v3="3" pid="9" p1="5"/>
        </triangles>
      </mesh>
    </object>
    <object id="4" type="model"><components><component objectid="3" transform="1 0 0 0 1 0 0 0 1 10 0 0"/></components></object>
  </resources>
  <build>
    <item objectid="3"/>
    <item objectid="4" transform="2 0 0 0 2 0 0 0 2 0 0 0"/>
  </build>
</model>"##;

    fn package() -> Vec<u8> {
        stored_zip(&[("_rels/.rels", RELS), ("3D/3dmodel.model", MODEL.as_bytes())])
    }

    #[test]
    fn loads_items_components_materials_and_units() {
        let bytes = package();
        assert!(looks_like_3mf(&bytes, false));
        let s = load(Path::new("/nonexistent/t.3mf"), &bytes, &Limits::viewer()).unwrap();
        assert_eq!(s.info.format, "3MF");
        assert!(s.info.details.iter().any(|d| d.1 == "Two & one"), "{:?}", s.info.details);
        // One mesh, instanced by an item and by a component.
        assert_eq!(s.meshes.len(), 1);
        assert_eq!(s.nodes.iter().filter(|n| n.mesh.is_some()).count(), 2);
        assert_eq!(s.triangle_count(), 4);
        let names: Vec<&str> = s.materials.iter().map(|m| m.name.as_str()).collect();
        assert!(names.contains(&"Red") && names.contains(&"Glass"), "{names:?}");
        let glass = s.materials.iter().find(|m| m.name == "Glass").unwrap();
        assert_eq!(glass.alpha_mode, AlphaMode::Blend);
        // The painted triangle has green and blue corners.
        assert!(s.meshes[0].primitives.iter().any(|p| p.colors.contains(&Vec4::new(0.0, 0.0, 1.0, 1.0))));
        // Centimetres, Z-up: the component, moved 10 cm along X and scaled
        // by 2 with its item, reaches 22 cm along X and 2 cm up.
        let pose = slerp_scene::Pose::rest(&s);
        let b = s.bounds(&pose);
        assert!((b.max.x - 0.22).abs() < 1e-5 && (b.max.y - 0.02).abs() < 1e-5, "{b:?}");
        assert!((b.min.z + 0.02).abs() < 1e-5, "{b:?}");
    }

    #[test]
    fn survives_truncation_and_garbage() {
        let bytes = package();
        for cut in (0..bytes.len()).step_by(7) {
            let _ = load(Path::new("t.3mf"), &bytes[..cut], &Limits::viewer());
        }
        for cut in (0..MODEL.len()).step_by(11) {
            let z = stored_zip(&[("3D/3dmodel.model", &MODEL.as_bytes()[..cut])]);
            let _ = load(Path::new("t.3mf"), &z, &Limits::viewer());
        }
        let looped = MODEL.replace(
            "<object id=\"4\" type=\"model\"><components>",
            "<object id=\"4\" type=\"model\"><components><component objectid=\"4\"/>",
        );
        let z = stored_zip(&[("3D/3dmodel.model", looped.as_bytes())]);
        assert!(load(Path::new("t.3mf"), &z, &Limits::viewer()).is_ok());
        let limits = Limits { max_triangles: 2, ..Limits::viewer() };
        assert!(matches!(load(Path::new("t.3mf"), &package(), &limits), Err(LoadError::TooLarge(_))));
    }
}
