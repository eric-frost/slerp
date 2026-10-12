//! Quake and Half-Life maps: BSP version 29, BSP2 and 2PSB (Quake with
//! 32-bit indices), and Half-Life's version 30.
//!
//! A map becomes a `SceneKind::Level`: one node per drawn model (the world
//! and each brush entity), triangulated faces grouped by texture, a
//! lightmap atlas read through the second UV set, the player's start as the
//! spawn point and point entities as markers.  Textures come from the BSP
//! itself or, for Half-Life, from the WAD3 files its worldspawn names.

use crate::entities::{self, Entity};
use crate::miptex::{self, Palette};
use crate::pixels::QUAKE_PALETTE;
use crate::read::{Bytes, Result, cap};
use crate::wad::{self, Found};
use crate::{dir_to_y_up, find_in, read_neighbour, to_y_up};
use slerp_scene::glam::{Vec2, Vec3, Vec4};
use slerp_scene::textures::{ColorSpace, Texture};
use slerp_scene::{
    AlphaMode, Effect, Filter, Info, Limits, LoadError, Marker, Material, Mesh, Primitive, Sampler, Scene, SceneKind,
    Spawn, TexRef, Transform, Wrap,
};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;

/*┌─────────────────╮
| File layout     |
└─────────────────╯*/
//region File layout

const LUMP_COUNT: usize = 15;
const HEADER_BYTES: usize = 4 + LUMP_COUNT * 8;

const ENTITIES: usize = 0;
const PLANES: usize = 1;
const TEXTURES: usize = 2;
const VERTEXES: usize = 3;
const TEXINFO: usize = 6;
const FACES: usize = 7;
const LIGHTING: usize = 8;
const EDGES: usize = 12;
const SURFEDGES: usize = 13;
const MODELS: usize = 14;

/// Lightmap blocks wider or taller than this are treated as unlit; Quake
/// engines refuse far smaller ones, so only corrupt files reach it.
const MAX_LIGHTMAP_SIDE: u32 = 2048;

/// The largest lightmap atlas page.  Pages stay whole at any texture limit,
/// since shrinking one would blend neighbouring faces' light together.
const MAX_PAGE: u32 = 4096;

/// Texture names of brushes that only steer the compiler or the game and
/// are never drawn.
const TOOL_TEXTURES: &[&str] = &["origin", "aaatrigger", "null", "hint", "hintskip", "nodraw", "bevel"];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Version {
    Quake,
    Bsp2,
    Bsp2Rmq,
    HalfLife,
}

impl Version {
    fn name(self) -> &'static str {
        match self {
            Version::Quake => "Quake BSP",
            Version::Bsp2 => "Quake BSP2",
            Version::Bsp2Rmq => "Quake BSP2 (2PSB)",
            Version::HalfLife => "Half-Life BSP",
        }
    }

    /// BSP2 and 2PSB widen faces and edges to 32-bit fields.
    fn wide(self) -> bool {
        matches!(self, Version::Bsp2 | Version::Bsp2Rmq)
    }

    /// Height of the player's eyes above the spawn entity's origin: Quake's
    /// view offset is 22 units, Half-Life's 28.
    fn eye_height(self) -> f32 {
        if self == Version::HalfLife { 28.0 } else { 22.0 }
    }
}

/// Whether the first bytes of a version 29 or 30 file look like a BSP lump
/// table, since those versions are plain numbers rather than magic text.
pub fn plausible_header(head: &[u8]) -> bool {
    if head.len() < HEADER_BYTES {
        return true;
    }
    let b = Bytes::new(head);
    (0..LUMP_COUNT).all(|i| {
        let (Ok(ofs), Ok(len)) = (b.i32(4 + 8 * i), b.i32(8 + 8 * i)) else { return false };
        ofs >= 0 && len >= 0 && (len == 0 || ofs as usize >= HEADER_BYTES)
    })
}

#[derive(Clone, Copy, Default)]
struct Lump {
    ofs: usize,
    len: usize,
}

struct TexInfo {
    s: Vec4,
    t: Vec4,
    miptex: i32,
}

struct Face {
    plane: usize,
    back: bool,
    first_edge: usize,
    edge_count: usize,
    texinfo: usize,
    styles: [u8; 4],
    light_ofs: i32,
}

struct Model {
    first_face: usize,
    face_count: usize,
}

//endregion

/*┌─────────────────╮
| Loading         |
└─────────────────╯*/
//region Loading

/// Everything parsed from the file before meshes are built.
struct Map<'a> {
    version: Version,
    bytes: Bytes<'a>,
    lumps: [Lump; LUMP_COUNT],
    bspx: HashMap<String, &'a [u8]>,
    planes: Vec<Vec3>,
    vertices: Vec<Vec3>,
    edges: Vec<[u32; 2]>,
    surfedges: Vec<i32>,
    texinfo: Vec<TexInfo>,
    faces: Vec<Face>,
    models: Vec<Model>,
    entities: Vec<Entity>,
}

pub fn load(path: &Path, data: &[u8], limits: &Limits) -> Result<Scene> {
    let map = parse(data)?;
    let mut warnings = Vec::new();
    let textures = load_textures(&map, path, limits, &mut warnings)?;
    let light = LightData::pick(&map, path, limits);
    build(&map, path, textures, light, limits, warnings)
}

fn parse(data: &[u8]) -> Result<Map<'_>> {
    let b = Bytes::new(data);

    // 1. Version and lump table.  Every lump must lie inside the file.
    let version = match b.slice(0, 4)? {
        b"BSP2" => Version::Bsp2,
        b"2PSB" => Version::Bsp2Rmq,
        [29, 0, 0, 0] => Version::Quake,
        [30, 0, 0, 0] => Version::HalfLife,
        _ => return Err(LoadError::NotMine),
    };
    let mut lumps = [Lump::default(); LUMP_COUNT];
    for (i, l) in lumps.iter_mut().enumerate() {
        *l = Lump { ofs: b.index(4 + 8 * i)?, len: b.index(8 + 8 * i)? };
        b.slice(l.ofs, l.len)?;
    }
    // Blue Shift swapped the entity and plane lumps; tell them apart by the
    // entity text's opening brace.
    if version == Version::HalfLife {
        let starts_brace =
            |l: Lump| b.slice(l.ofs, l.len.min(64)).is_ok_and(|s| s.trim_ascii_start().starts_with(b"{"));
        if !starts_brace(lumps[ENTITIES]) && starts_brace(lumps[PLANES]) {
            lumps.swap(ENTITIES, PLANES);
        }
    }
    let lump = |i: usize| -> Result<Bytes> { b.sub(lumps[i].ofs, lumps[i].len) };

    // 2. Arrays.  Records are fixed-size, so counts come from lump lengths
    //    and can't exceed the file.
    let planes = records(lump(PLANES)?, 20, |r, at| r.vec3(at))?;
    let vertices = records(lump(VERTEXES)?, 12, |r, at| r.vec3(at))?;
    let edges = if version.wide() {
        records(lump(EDGES)?, 8, |r, at| Ok([r.u32(at)?, r.u32(at + 4)?]))?
    } else {
        records(lump(EDGES)?, 4, |r, at| Ok([r.u16(at)? as u32, r.u16(at + 2)? as u32]))?
    };
    let surfedges = records(lump(SURFEDGES)?, 4, |r, at| r.i32(at))?;
    let texinfo = records(lump(TEXINFO)?, 40, |r, at| {
        let v = |k: usize| r.f32(at + 4 * k);
        Ok(TexInfo {
            s: Vec4::new(v(0)?, v(1)?, v(2)?, v(3)?),
            t: Vec4::new(v(4)?, v(5)?, v(6)?, v(7)?),
            miptex: r.i32(at + 32)?,
        })
    })?;
    let faces = if version.wide() {
        records(lump(FACES)?, 28, |r, at| {
            Ok(Face {
                plane: r.u32(at)? as usize,
                back: r.i32(at + 4)? != 0,
                first_edge: r.u32(at + 8)? as usize,
                edge_count: r.u32(at + 12)? as usize,
                texinfo: r.u32(at + 16)? as usize,
                styles: [r.u8(at + 20)?, r.u8(at + 21)?, r.u8(at + 22)?, r.u8(at + 23)?],
                light_ofs: r.i32(at + 24)?,
            })
        })?
    } else {
        records(lump(FACES)?, 20, |r, at| {
            Ok(Face {
                plane: r.u16(at)? as usize,
                back: r.i16(at + 2)? != 0,
                first_edge: r.u32(at + 4)? as usize,
                edge_count: r.u16(at + 8)? as usize,
                texinfo: r.u16(at + 10)? as usize,
                styles: [r.u8(at + 12)?, r.u8(at + 13)?, r.u8(at + 14)?, r.u8(at + 15)?],
                light_ofs: r.i32(at + 16)?,
            })
        })?
    };
    let models = records(lump(MODELS)?, 64, |r, at| {
        Ok(Model { first_face: r.u32(at + 56)? as usize, face_count: r.u32(at + 60)? as usize })
    })?;
    if models.is_empty() {
        return Err(LoadError::invalid("the map has no models"));
    }
    let entities = entities::parse(lump(ENTITIES)?.data);

    // 3. BSPX extension lumps follow the last standard lump, 4-byte aligned:
    //    "BSPX", a count, then 32-byte entries of a 24-byte name, offset and
    //    length.
    let mut bspx = HashMap::new();
    let end = lumps.iter().map(|l| l.ofs + l.len).max().unwrap_or(0);
    let at = end.div_ceil(4) * 4;
    if b.slice(at, 4).is_ok_and(|m| m == b"BSPX")
        && let Ok(n) = b.u32(at + 4)
    {
        for i in 0..(n as usize).min(256) {
            let e = at + 8 + 32 * i;
            let (Ok(name), Ok(ofs), Ok(len)) = (b.name(e, 24), b.u32(e + 24), b.u32(e + 28)) else { break };
            if let Ok(s) = b.slice(ofs as usize, len as usize) {
                bspx.insert(name, s);
            }
        }
    }

    Ok(Map { version, bytes: b, lumps, bspx, planes, vertices, edges, surfedges, texinfo, faces, models, entities })
}

/// Reads every `stride`-byte record of a lump; a trailing partial record is
/// ignored.
fn records<T>(b: Bytes, stride: usize, read: impl Fn(&Bytes, usize) -> Result<T>) -> Result<Vec<T>> {
    let count = b.len() / stride;
    let mut out = Vec::with_capacity(count);
    for i in 0..count {
        out.push(read(&b, i * stride)?);
    }
    Ok(out)
}

//endregion

/*┌─────────────────╮
| Textures        |
└─────────────────╯*/
//region Textures

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum Surface {
    Normal,
    Sky,
    Warp,
    Tool,
}

/// One entry of the map's texture list, with its decoded pictures.
struct Slot {
    name: String,
    width: u32,
    height: u32,
    surface: Surface,
    base: Option<Arc<Texture>>,
    emissive: Option<Arc<Texture>>,
    masked: bool,
}

fn classify(name: &str, version: Version) -> Surface {
    let n = name.to_ascii_lowercase();
    if TOOL_TEXTURES.contains(&n.as_str()) || ["clip", "trigger", "skip"].iter().any(|p| n.starts_with(p)) {
        Surface::Tool
    } else if n.starts_with("sky") {
        Surface::Sky
    } else if n.starts_with('*') || (version == Version::HalfLife && n.starts_with('!')) {
        Surface::Warp
    } else {
        Surface::Normal
    }
}

fn load_textures(map: &Map, path: &Path, limits: &Limits, warnings: &mut Vec<String>) -> Result<Vec<Slot>> {
    let lump = map.bytes.sub(map.lumps[TEXTURES].ofs, map.lumps[TEXTURES].len)?;
    let palette = if map.version == Version::HalfLife { Palette::Embedded } else { Palette::Global(QUAKE_PALETTE) };
    let container = map.version.name();
    let max = limits.max_texture_size;
    let mut slots = Vec::new();
    if lump.len() >= 4 {
        let count = cap(lump.index(0)?, lump.len() / 4, "textures")?;
        for i in 0..count {
            let ofs = lump.i32(4 + 4 * i)?;
            let header = usize::try_from(ofs).ok().and_then(|o| miptex::Header::read(lump, o).ok().map(|h| (o, h)));
            let Some((ofs, h)) = header else {
                slots.push(Slot {
                    name: String::new(),
                    width: 64,
                    height: 64,
                    surface: Surface::Normal,
                    base: None,
                    emissive: None,
                    masked: false,
                });
                continue;
            };
            let mut slot = Slot {
                surface: classify(&h.name, map.version),
                name: h.name.clone(),
                width: h.width.max(1),
                height: h.height.max(1),
                base: None,
                emissive: None,
                masked: false,
            };
            if !h.is_external() && slot.surface != Surface::Tool {
                match miptex::decode(lump, ofs, palette, max, container) {
                    Ok(d) => {
                        slot.base = Some(Arc::new(d.base));
                        slot.emissive = d.emissive.map(Arc::new);
                        slot.masked = d.masked;
                    }
                    Err(e) => warnings.push(format!("texture {}: {e}", h.name)),
                }
            }
            slots.push(slot);
        }
    }

    // Textures whose pixels live in WADs named by the worldspawn.
    let missing: HashSet<String> = slots
        .iter()
        .filter(|s| s.base.is_none() && s.surface != Surface::Tool && !s.name.is_empty())
        .map(|s| s.name.to_ascii_lowercase())
        .collect();
    if !missing.is_empty() {
        let list = map.entities.first().and_then(|w| w.get("wad")).unwrap_or("");
        let found =
            if list.is_empty() { HashMap::new() } else { wad::lookup(list, path, &missing, limits, warnings).found };
        for s in &mut slots {
            if s.base.is_none()
                && let Some(d) = found.get(&s.name.to_ascii_lowercase())
            {
                s.base = Some(d.base.clone());
                s.emissive = d.emissive.clone();
                s.masked = d.masked;
            }
        }
        let still: Vec<&str> = slots
            .iter()
            .filter(|s| s.base.is_none() && missing.contains(&s.name.to_ascii_lowercase()))
            .map(|s| s.name.as_str())
            .collect();
        if !still.is_empty() {
            let mut names = still.clone();
            names.sort_unstable();
            names.dedup();
            let shown = names.iter().take(8).copied().collect::<Vec<_>>().join(", ");
            let more = if names.len() > 8 { format!(" and {} more", names.len() - 8) } else { String::new() };
            warnings.push(format!("{} textures not found: {shown}{more}", names.len()));
        }
    }
    Ok(slots)
}

/// Decodes the textures in `names` (lowercase) that a compiled map
/// carries in its texture lump, reading nothing else.  A `.map`'s own BSP
/// beside it holds every texture the map used, even when the WADs it names
/// are gone from the mapper's computer.
pub(crate) fn embedded_textures(data: &[u8], names: &HashSet<String>, max: Option<u32>) -> HashMap<String, Found> {
    let mut out = HashMap::new();
    let b = Bytes::new(data);
    let palette = match b.slice(0, 4) {
        Ok(b"BSP2" | b"2PSB" | [29, 0, 0, 0]) => Palette::Global(QUAKE_PALETTE),
        Ok([30, 0, 0, 0]) => Palette::Embedded,
        _ => return out,
    };
    let (Ok(ofs), Ok(len)) = (b.index(4 + 8 * TEXTURES), b.index(8 + 8 * TEXTURES)) else { return out };
    let Ok(lump) = b.sub(ofs, len) else { return out };
    let Ok(count) = lump.index(0).and_then(|n| cap(n, lump.len() / 4, "textures")) else { return out };
    for i in 0..count {
        let Some(at) = lump.i32(4 + 4 * i).ok().and_then(|o| usize::try_from(o).ok()) else { continue };
        let Ok(h) = miptex::Header::read(lump, at) else { continue };
        let key = h.name.to_ascii_lowercase();
        if h.is_external() || !names.contains(&key) || out.contains_key(&key) {
            continue;
        }
        if let Ok(d) = miptex::decode(lump, at, palette, max, "Quake BSP") {
            out.insert(key, Found::from(d));
        }
    }
    out
}

//endregion

/*┌─────────────────╮
| Lightmaps       |
└─────────────────╯*/
//region Lightmaps

/// Where a map's light samples come from, in order of preference: HDR
/// samples in a BSPX lump, coloured light from a `.lit` file or BSPX lump,
/// then the map's own grey (Quake) or RGB (Half-Life) lump.
enum LightData<'a> {
    None,
    /// One byte per sample; face offsets count samples.
    Grey(&'a [u8]),
    /// Three bytes per sample; face offsets count samples (as for `.lit`
    /// files, which mirror the grey lump) or bytes (Half-Life).
    Rgb {
        data: std::borrow::Cow<'a, [u8]>,
        offsets_in_bytes: bool,
    },
    /// Packed E5BGR9 floats, four bytes per sample; offsets count samples.
    Hdr(&'a [u8]),
}

impl<'a> LightData<'a> {
    fn pick(map: &Map<'a>, path: &Path, limits: &Limits) -> LightData<'a> {
        let lighting = map.bytes.slice(map.lumps[LIGHTING].ofs, map.lumps[LIGHTING].len).unwrap_or(&[]);
        if let Some(hdr) = map.bspx.get("LIGHTING_E5BGR9").filter(|d| !d.is_empty()) {
            return LightData::Hdr(hdr);
        }
        if map.version == Version::HalfLife {
            return if lighting.is_empty() {
                LightData::None
            } else {
                LightData::Rgb { data: lighting.into(), offsets_in_bytes: true }
            };
        }
        if limits.neighbours
            && let Some(lit) = lit_file(path, lighting.len())
        {
            return LightData::Rgb { data: lit.into(), offsets_in_bytes: false };
        }
        if let Some(rgb) = map.bspx.get("RGBLIGHTING").filter(|d| !d.is_empty()) {
            return LightData::Rgb { data: (*rgb).into(), offsets_in_bytes: false };
        }
        if lighting.is_empty() { LightData::None } else { LightData::Grey(lighting) }
    }

    fn is_hdr(&self) -> bool {
        matches!(self, LightData::Hdr(_))
    }

    fn describe(&self) -> &'static str {
        match self {
            LightData::None => "none",
            LightData::Grey(_) => "grey",
            LightData::Rgb { .. } => "coloured",
            LightData::Hdr(_) => "HDR",
        }
    }

    /// Converts a face's light offset to a sample index.
    fn sample_index(&self, ofs: i32) -> Option<usize> {
        let ofs = usize::try_from(ofs).ok()?;
        match self {
            LightData::Rgb { offsets_in_bytes: true, .. } => Some(ofs / 3),
            LightData::None => None,
            _ => Some(ofs),
        }
    }

    /// Sample `i` as linear values where 1.0 is the brightest 8-bit sample.
    /// ericw-tools writes HDR samples with Quake's doubling already applied
    /// (1.0 where the grey lightmap holds 128), so they are halved to share
    /// the 8-bit scale and `lightmap_scale`.
    fn sample(&self, i: usize) -> Option<[f32; 3]> {
        match self {
            LightData::None => None,
            LightData::Grey(d) => d.get(i).map(|&v| [v as f32 / 255.0; 3]),
            LightData::Rgb { data, .. } => {
                let s = data.get(i * 3..i * 3 + 3)?;
                Some([s[0] as f32 / 255.0, s[1] as f32 / 255.0, s[2] as f32 / 255.0])
            }
            LightData::Hdr(d) => {
                let s = d.get(i * 4..i * 4 + 4)?;
                let packed = u32::from_le_bytes([s[0], s[1], s[2], s[3]]);
                let scale = 2f32.powi((packed >> 27) as i32 - 25);
                Some([
                    (packed & 511) as f32 * scale,
                    ((packed >> 9) & 511) as f32 * scale,
                    ((packed >> 18) & 511) as f32 * scale,
                ])
            }
        }
    }
}

/// Reads a `.lit` file next to the map: "QLIT", version 1, then three
/// bytes for every byte of the map's grey lightmaps.
fn lit_file(path: &Path, grey_len: usize) -> Option<Vec<u8>> {
    let stem = path.file_stem()?.to_str()?;
    let file = find_in(path.parent()?, &format!("{stem}.lit"))?;
    let mut bytes = read_neighbour(&file)?;
    if bytes.len() != 8 + grey_len * 3 || &bytes[..4] != b"QLIT" || bytes[4..8] != 1u32.to_le_bytes() {
        return None;
    }
    bytes.drain(..8);
    Some(bytes)
}

/// Where one face's lightmap lives and how its vertices find it.
struct FaceLight {
    width: u32,
    height: u32,
    /// First sample of the face's first style; styles follow one another.
    sample: usize,
    /// The style slots to add together.
    slots: Vec<usize>,
    /// World position (Quake space) to luxel coordinates, where luxel `i`'s
    /// centre is at `i`.
    s: Vec4,
    t: Vec4,
    /// Page and top-left corner of the block, inside its one-luxel gutter.
    page: usize,
    x: u32,
    y: u32,
}

/// Light styles that start switched off: lights flagged START_OFF that the
/// light tool gave a switchable style (32 and up).
fn styles_off(entities: &[Entity]) -> HashSet<u8> {
    entities
        .iter()
        .filter(|e| e.classname().starts_with("light"))
        .filter(|e| e.float("spawnflags").is_some_and(|f| f as i64 & 1 != 0))
        .filter_map(|e| e.float("style").map(|s| s as i64))
        .filter(|s| (32..255).contains(s))
        .map(|s| s as u8)
        .collect()
}

/// Works out the lightmap block of face `index`, or `None` when it has
/// none (sky, liquids, unlit faces).
fn face_light(map: &Map, light: &LightData, index: usize, positions: &[Vec3], off: &HashSet<u8>) -> Option<FaceLight> {
    let face = &map.faces[index];
    let tex = map.texinfo.get(face.texinfo)?;

    // 1. Placement: from DECOUPLED_LM when the light tool wrote one, else
    //    Quake's rule of one luxel per 16 texels (or LMSHIFT's scale).
    let (width, height, ofs, s, t) = if let Some(d) = map.bspx.get("DECOUPLED_LM") {
        let r = Bytes::new(d);
        let at = index * 40;
        let row = |k: usize| -> Option<Vec4> {
            let f = |j: usize| r.f32(at + 8 + 4 * (k * 4 + j)).ok();
            Some(Vec4::new(f(0)?, f(1)?, f(2)?, f(3)?))
        };
        (r.u16(at).ok()? as u32, r.u16(at + 2).ok()? as u32, r.i32(at + 4).ok()?, row(0)?, row(1)?)
    } else {
        let shift = map.bspx.get("LMSHIFT").and_then(|d| d.get(index)).copied().unwrap_or(4).min(10);
        let scale = (1u32 << shift) as f32;
        let (mut lo, mut hi) = (Vec2::splat(f32::INFINITY), Vec2::splat(f32::NEG_INFINITY));
        for &p in positions {
            // Quake computes texture coordinates in double precision; so do
            // we, so block sizes match the light tool's.
            let st = Vec2::new(dot64(p, tex.s) as f32, dot64(p, tex.t) as f32);
            lo = lo.min(st);
            hi = hi.max(st);
        }
        let (blo, bhi) = ((lo / scale).floor(), (hi / scale).ceil());
        let size = bhi - blo + Vec2::ONE;
        if !(size.x.is_finite() && size.y.is_finite()) || size.x < 1.0 || size.y < 1.0 {
            return None;
        }
        let s = Vec4::new(tex.s.x / scale, tex.s.y / scale, tex.s.z / scale, tex.s.w / scale - blo.x);
        let t = Vec4::new(tex.t.x / scale, tex.t.y / scale, tex.t.z / scale, tex.t.w / scale - blo.y);
        (size.x as u32, size.y as u32, face.light_ofs, s, t)
    };
    if width == 0 || height == 0 || width > MAX_LIGHTMAP_SIDE || height > MAX_LIGHTMAP_SIDE {
        return None;
    }

    // 2. Samples: every style slot in use, except styles switched off at
    //    the start, all at normal brightness.
    let sample = light.sample_index(ofs)?;
    let slots: Vec<usize> =
        (0..4).take_while(|&k| face.styles[k] != 255).filter(|&k| !off.contains(&face.styles[k])).collect();
    let block = width as usize * height as usize;
    let used = (0..4).take_while(|&k| face.styles[k] != 255).count();
    light.sample(sample + used.max(1) * block - 1)?;
    Some(FaceLight { width, height, sample, slots, s, t, page: 0, x: 0, y: 0 })
}

fn dot64(p: Vec3, v: Vec4) -> f64 {
    p.x as f64 * v.x as f64 + p.y as f64 * v.y as f64 + p.z as f64 * v.z as f64 + v.w as f64
}

/// Packs lightmap blocks (with a one-luxel gutter each) into square pages
/// by rows, tallest first, and returns each page's size.
fn pack(blocks: &mut [&mut FaceLight]) -> Vec<(u32, u32)> {
    let area: u64 = blocks.iter().map(|b| (b.width as u64 + 2) * (b.height as u64 + 2)).sum();
    let largest = blocks.iter().map(|b| b.width.max(b.height) + 2).max().unwrap_or(1);
    let mut side = 64u32;
    while (side as u64 * side as u64) < area + area / 8 && side < MAX_PAGE {
        side *= 2;
    }
    while side < largest && side < MAX_PAGE {
        side *= 2;
    }
    blocks.sort_by(|a, b| b.height.cmp(&a.height).then(b.width.cmp(&a.width)));
    let mut pages = vec![(side, 0u32)];
    let (mut x, mut y, mut row) = (0u32, 0u32, 0u32);
    for b in blocks.iter_mut() {
        let (w, h) = (b.width + 2, b.height + 2);
        if x + w > side {
            x = 0;
            y += row;
            row = 0;
        }
        if y + h > side {
            pages.push((side, 0));
            x = 0;
            y = 0;
            row = 0;
        }
        b.page = pages.len() - 1;
        b.x = x + 1;
        b.y = y + 1;
        x += w;
        row = row.max(h);
        let page = pages.last_mut().expect("pages is never empty");
        page.1 = page.1.max(y + h);
    }
    // Trim each page to the rows it uses, keeping heights a multiple of 4.
    pages.iter().map(|&(w, h)| (w, h.max(4).div_ceil(4) * 4)).collect()
}

/// One atlas page's texels: bytes for 8-bit light, floats for HDR.
enum Page {
    Ldr(Vec<u8>),
    Hdr(Vec<f32>),
}

impl Page {
    fn set(&mut self, texel: usize, rgb: [f32; 3]) {
        match self {
            Page::Ldr(d) => {
                let byte = |v: f32| (v * 255.0).round().clamp(0.0, 255.0) as u8;
                d[texel * 4..texel * 4 + 4].copy_from_slice(&[byte(rgb[0]), byte(rgb[1]), byte(rgb[2]), 255]);
            }
            Page::Hdr(d) => d[texel * 4..texel * 4 + 4].copy_from_slice(&[rgb[0], rgb[1], rgb[2], 1.0]),
        }
    }
}

/// Fills the atlas pages with every face's summed light, replicating edge
/// luxels into the gutter so filtering never reaches a neighbour.  Texels
/// no face covers stay black.
fn fill_pages<'f>(light: &LightData, faces: impl Iterator<Item = &'f FaceLight>, pages: &[(u32, u32)]) -> Vec<Texture> {
    let mut data: Vec<Page> = pages
        .iter()
        .map(|&(w, h)| {
            let n = w as usize * h as usize * 4;
            if light.is_hdr() {
                let mut d = vec![0.0; n];
                d.iter_mut().skip(3).step_by(4).for_each(|a| *a = 1.0);
                Page::Hdr(d)
            } else {
                let mut d = vec![0; n];
                d.iter_mut().skip(3).step_by(4).for_each(|a| *a = 255);
                Page::Ldr(d)
            }
        })
        .collect();
    for f in faces {
        let (w, h) = (f.width as i64, f.height as i64);
        let (pw, _) = pages[f.page];
        let page = &mut data[f.page];
        for j in -1..=h {
            for i in -1..=w {
                let (si, sj) = (i.clamp(0, w - 1) as usize, j.clamp(0, h - 1) as usize);
                let mut sum = [0.0f32; 3];
                for &slot in &f.slots {
                    let at = f.sample + slot * (w * h) as usize + sj * w as usize + si;
                    if let Some(s) = light.sample(at) {
                        sum = [sum[0] + s[0], sum[1] + s[1], sum[2] + s[2]];
                    }
                }
                page.set((f.y as i64 + j) as usize * pw as usize + (f.x as i64 + i) as usize, sum);
            }
        }
    }
    data.into_iter()
        .zip(pages)
        .map(|(d, &(w, h))| {
            let mut tex = match d {
                Page::Hdr(d) => Texture::from_rgba32f(w, h, d),
                Page::Ldr(d) => Texture::from_rgba8(w, h, d, ColorSpace::Linear),
            };
            tex.source.container = "lightmap atlas".into();
            tex.source.format = format!("{} light", light.describe());
            tex
        })
        .collect()
}

//endregion

/*┌─────────────────╮
| Building        |
└─────────────────╯*/
//region Building

/// A model that is drawn: the world, or a brush entity's model.
struct Drawn {
    model: usize,
    /// The entity's origin, in Quake space.
    origin: Vec3,
    /// Half-Life's entity render mode and amount.
    render: (u8, u8),
    class: String,
}

/// What makes two faces share a material.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct MatKey {
    slot: Option<usize>,
    page: Option<usize>,
    /// Half-Life's entity render mode and amount.
    render: (u8, u8),
}

fn build(
    map: &Map,
    path: &Path,
    slots: Vec<Slot>,
    light: LightData,
    limits: &Limits,
    mut warnings: Vec<String>,
) -> Result<Scene> {
    let version = map.version;
    let mut scene = Scene { kind: SceneKind::Level, ..Default::default() };

    // 1. Which models are drawn and where: the world, and each brush
    //    entity's "*N" model at its origin.  Triggers are never drawn, nor
    //    are Half-Life entities rendered fully transparent.
    let mut drawn = vec![Drawn { model: 0, origin: Vec3::ZERO, render: (0, 255), class: "worldspawn".into() }];
    for e in &map.entities {
        let Some(m) = e.get("model").and_then(|m| m.strip_prefix('*')).and_then(|n| n.parse::<usize>().ok()) else {
            continue;
        };
        let class = e.classname();
        if m == 0 || m >= map.models.len() || class.starts_with("trigger_") || drawn.iter().any(|d| d.model == m) {
            continue;
        }
        let mode = e.float("rendermode").unwrap_or(0.0).clamp(0.0, 5.0) as u8;
        let amount = e.float("renderamt").unwrap_or(0.0).clamp(0.0, 255.0) as u8;
        let render = if version == Version::HalfLife && mode != 0 && mode != 4 { (mode, amount) } else { (mode, 255) };
        if render.0 != 0 && render.0 != 4 && render.1 == 0 {
            continue;
        }
        drawn.push(Drawn {
            model: m,
            origin: e.vec3("origin").unwrap_or(Vec3::ZERO),
            render,
            class: class.to_string(),
        });
    }

    // 2. Faces to draw, with their polygons in Quake space.
    let face_polygon = |fi: usize| -> Option<Vec<Vec3>> {
        let f = map.faces.get(fi)?;
        if f.edge_count < 3 || f.edge_count > 4096 {
            return None;
        }
        let mut out = Vec::with_capacity(f.edge_count);
        for k in 0..f.edge_count {
            let se = *map.surfedges.get(f.first_edge.checked_add(k)?)?;
            let v =
                if se >= 0 { map.edges.get(se as usize)?[0] } else { map.edges.get(se.unsigned_abs() as usize)?[1] };
            out.push(*map.vertices.get(v as usize)?);
        }
        Some(out)
    };
    let slot_of = |fi: usize| -> Option<usize> {
        let ti = map.texinfo.get(map.faces[fi].texinfo)?;
        usize::try_from(ti.miptex).ok().filter(|&s| s < slots.len())
    };
    let mut faces_by_model: Vec<Vec<(usize, Vec<Vec3>)>> = Vec::new();
    let mut triangles = 0u64;
    let mut bad_faces = 0;
    for d in &drawn {
        let model = &map.models[d.model];
        let mut list = Vec::new();
        let end = model.first_face.saturating_add(model.face_count).min(map.faces.len());
        for fi in model.first_face.min(end)..end {
            if slot_of(fi).is_some_and(|s| slots[s].surface == Surface::Tool) {
                continue;
            }
            match face_polygon(fi) {
                Some(poly) => {
                    triangles += poly.len() as u64 - 2;
                    list.push((fi, poly));
                }
                None => bad_faces += 1,
            }
        }
        faces_by_model.push(list);
    }
    if triangles > limits.max_triangles {
        return Err(LoadError::TooLarge(format!("{triangles} triangles")));
    }
    if bad_faces > 0 {
        warnings.push(format!("{bad_faces} malformed faces skipped"));
    }

    // 3. Lightmaps for every lit face of a normal surface, packed into
    //    atlas pages.
    let off = styles_off(&map.entities);
    let lit_surface = |fi: usize| slot_of(fi).is_none_or(|s| slots[s].surface == Surface::Normal);
    let mut lights: BTreeMap<usize, FaceLight> = BTreeMap::new();
    if !matches!(light, LightData::None) {
        for list in &faces_by_model {
            for (fi, poly) in list {
                if lit_surface(*fi)
                    && let Some(fl) = face_light(map, &light, *fi, poly, &off)
                {
                    lights.insert(*fi, fl);
                }
            }
        }
    }
    // Faces of a lit map that have no light of their own are black, as in
    // Quake; they all share one black block.
    let mut dark = FaceLight {
        width: 1,
        height: 1,
        sample: 0,
        slots: Vec::new(),
        s: Vec4::ZERO,
        t: Vec4::ZERO,
        page: 0,
        x: 0,
        y: 0,
    };
    let has_light = !matches!(light, LightData::None);
    let mut pages = Vec::new();
    if has_light {
        let mut blocks: Vec<&mut FaceLight> = lights.values_mut().collect();
        blocks.push(&mut dark);
        pages = pack(&mut blocks);
    }
    // The dark block has no samples, so it stays black.
    let lightmap_textures = if has_light { fill_pages(&light, lights.values(), &pages) } else { Vec::new() };
    let page_base = scene.textures.len();
    scene.textures.extend(lightmap_textures.into_iter().map(Arc::new));

    // 4. Textures into the scene, once each.
    let mut tex_index: Vec<(Option<usize>, Option<usize>)> = Vec::with_capacity(slots.len());
    for s in &slots {
        let mut add = |t: &Option<Arc<Texture>>| {
            t.as_ref().map(|t| {
                scene.textures.push(t.clone());
                scene.textures.len() - 1
            })
        };
        let base = add(&s.base);
        let emissive = add(&s.emissive);
        tex_index.push((base, emissive));
    }

    // 5. Meshes: one per drawn model, one primitive per material.
    let mut materials: HashMap<MatKey, usize> = HashMap::new();
    let lightmap_scale = if version == Version::HalfLife { 1.0 } else { 2.0 };
    let mut face_count = 0;
    for (di, d) in drawn.iter().enumerate() {
        let mut prims: HashMap<usize, Primitive> = HashMap::new();
        for (fi, poly) in &faces_by_model[di] {
            let face = &map.faces[*fi];
            let slot = slot_of(*fi);
            let surface = slot.map_or(Surface::Normal, |s| slots[s].surface);
            let fl = if surface == Surface::Normal && has_light { Some(lights.get(fi).unwrap_or(&dark)) } else { None };
            let key = MatKey { slot, page: fl.map(|f| f.page), render: d.render };
            let mat = *materials.entry(key).or_insert_with(|| {
                let s = slot.map(|s| &slots[s]);
                let (base, emissive) = slot.map_or((None, None), |s| tex_index[s]);
                let mut mat = Material {
                    name: s.map_or_else(String::new, |s| s.name.clone()),
                    roughness: 1.0,
                    specular: 0.0,
                    base_color: if base.is_some() { Vec4::ONE } else { Vec4::new(0.5, 0.5, 0.5, 1.0) },
                    base_color_texture: base.map(TexRef::new),
                    ..Default::default()
                };
                if let Some(e) = emissive {
                    mat.emissive = Vec3::ONE;
                    mat.emissive_texture = Some(TexRef::new(e));
                }
                if s.is_some_and(|s| s.masked) {
                    mat.alpha_mode = AlphaMode::Mask(0.5);
                }
                match surface {
                    Surface::Sky => {
                        mat.unlit = true;
                        // Half-Life draws a skybox instead; its "sky"
                        // texture is a plain placeholder.
                        if version != Version::HalfLife {
                            mat.effect = Effect::Sky;
                        }
                    }
                    Surface::Warp => {
                        mat.unlit = true;
                        mat.effect = Effect::Warp;
                    }
                    _ => {}
                }
                if let Some(f) = fl {
                    mat.lightmap_texture = Some(TexRef {
                        texture: page_base + f.page,
                        uv_set: 1,
                        transform: None,
                        sampler: Sampler {
                            wrap_u: Wrap::ClampToEdge,
                            wrap_v: Wrap::ClampToEdge,
                            mag: Filter::Linear,
                            min: Filter::Linear,
                            mipmaps: false,
                        },
                    });
                    mat.lightmap_scale = lightmap_scale;
                }
                if d.render.0 != 0 && d.render.0 != 4 {
                    mat.alpha_mode = AlphaMode::Blend;
                    mat.base_color.w = d.render.1 as f32 / 255.0;
                }
                scene.materials.push(mat);
                scene.materials.len() - 1
            });
            let prim = prims.entry(mat).or_insert_with(|| Primitive { material: Some(mat), ..Default::default() });

            // The face's normal, and the winding that faces along it, since
            // compilers disagree on which way polygons wind.
            let mut normal = map.planes.get(face.plane).copied().unwrap_or(Vec3::Z);
            if face.back {
                normal = -normal;
            }
            let newell = poly.iter().zip(poly.iter().cycle().skip(1)).fold(Vec3::ZERO, |n, (a, b)| {
                n + Vec3::new((a.y - b.y) * (a.z + b.z), (a.z - b.z) * (a.x + b.x), (a.x - b.x) * (a.y + b.y))
            });
            let forward = newell.dot(normal) >= 0.0;
            let tex = map.texinfo.get(face.texinfo);
            let (tw, th) = slot.map_or((64.0, 64.0), |s| (slots[s].width as f32, slots[s].height as f32));
            let first = prim.positions.len() as u32;
            for &p in poly {
                prim.positions.push(to_y_up(p));
                prim.normals.push(dir_to_y_up(normal));
                prim.uv0
                    .push(tex.map_or(Vec2::ZERO, |t| Vec2::new(dot64(p, t.s) as f32 / tw, dot64(p, t.t) as f32 / th)));
                if let Some(f) = fl {
                    let (pw, ph) = pages[f.page];
                    // The dark block's zero projection puts every vertex on its one luxel.
                    let luxel = Vec2::new(dot64(p, f.s) as f32, dot64(p, f.t) as f32);
                    prim.uv1.push(Vec2::new(
                        (f.x as f32 + luxel.x + 0.5) / pw as f32,
                        (f.y as f32 + luxel.y + 0.5) / ph as f32,
                    ));
                }
            }
            for k in 1..poly.len() as u32 - 1 {
                if forward {
                    prim.indices.extend_from_slice(&[first, first + k, first + k + 1]);
                } else {
                    prim.indices.extend_from_slice(&[first, first + k + 1, first + k]);
                }
            }
            face_count += 1;
        }
        if prims.is_empty() {
            continue;
        }
        let mut primitives: Vec<Primitive> = prims.into_values().collect();
        primitives.sort_by_key(|p| p.material);
        let name = if d.model == 0 { "world".to_string() } else { format!("*{} {}", d.model, d.class) };
        let node = scene.add_mesh_node(Mesh { name, primitives, target_names: Vec::new() });
        scene.nodes[node].transform = Transform { translation: to_y_up(d.origin), ..Transform::IDENTITY };
    }
    if scene.nodes.is_empty() {
        return Err(LoadError::invalid("the map has no drawable faces"));
    }

    // 6. Entities: the spawn point and markers.
    scene.spawn = spawn(&map.entities, version);
    scene.markers = markers(&map.entities);

    // 7. Facts for the inspector.
    let world = map.entities.first();
    let title = world
        .and_then(|w| w.get("message"))
        .map(|m| m.replace("\\n", " ").trim().to_string())
        .filter(|m| !m.is_empty());
    scene.name =
        title.clone().unwrap_or_else(|| path.file_stem().and_then(|s| s.to_str()).unwrap_or("map").to_string());
    let mut details = Vec::new();
    if let Some(t) = title {
        details.push(("Map".into(), t));
    }
    let loaded = slots.iter().filter(|s| s.base.is_some()).count();
    details.push(("Textures".into(), format!("{} ({loaded} with pixels)", slots.len())));
    details.push(("Entities".into(), map.entities.len().to_string()));
    details.push(("Brush models".into(), format!("{} drawn of {}", drawn.len(), map.models.len())));
    details.push(("Faces".into(), face_count.to_string()));
    if has_light {
        let sizes: Vec<String> = pages.iter().map(|(w, h)| format!("{w}x{h}")).collect();
        details.push(("Lightmap".into(), format!("{}, {}", light.describe(), sizes.join(" + "))));
    }
    if !map.bspx.is_empty() {
        let mut names: Vec<&str> = map.bspx.keys().map(String::as_str).collect();
        names.sort_unstable();
        details.push(("BSPX lumps".into(), names.join(", ")));
    }
    scene.info = Info { format: version.name().into(), warnings, details, ..Info::default() };
    Ok(scene)
}

/// The player's start at eye height, facing the entity's angle.
fn spawn(entities: &[Entity], version: Version) -> Option<Spawn> {
    let classes = ["info_player_start", "info_player_deathmatch", "info_player_coop", "info_player_start2"];
    spawn_at(entities, &classes, version.eye_height())
}

/// The first entity of the first class in `classes`, as a spawn point
/// `eye` units above its origin.  Quake's angle is degrees around +Z with
/// 0 facing +X, which in Y-up space is a turn of (angle - 90) degrees from
/// -Z; Half-Life and Source give the yaw as the second of `angles`.
pub(crate) fn spawn_at(entities: &[Entity], classes: &[&str], eye: f32) -> Option<Spawn> {
    let e = classes.iter().find_map(|c| entities.iter().find(|e| e.classname().eq_ignore_ascii_case(c)))?;
    let origin = e.vec3("origin")?;
    let angle = e.float("angle").or_else(|| e.vec3("angles").map(|a| a.y)).unwrap_or(0.0);
    Some(Spawn { position: to_y_up(origin + Vec3::new(0.0, 0.0, eye)), yaw: (angle - 90.0).to_radians() })
}

/// Point entities as markers, coloured by kind: lights in their own colour,
/// player starts green, monsters red, items and weapons yellow, the rest
/// grey.  Brush entities are drawn as geometry instead.
pub(crate) fn markers(entities: &[Entity]) -> Vec<Marker> {
    entities
        .iter()
        .skip(1)
        .filter(|e| !e.get("model").is_some_and(|m| m.starts_with('*')))
        .filter_map(|e| marker(e, e.classname().to_string()))
        .collect()
}

/// A point entity's marker, labelled `label`, or `None` without an origin.
pub(crate) fn marker(e: &Entity, label: String) -> Option<Marker> {
    let origin = e.vec3("origin")?;
    let class = e.classname();
    let color = if class.starts_with("light") {
        light_color(e)
    } else if class.starts_with("info_player") {
        Vec4::new(0.2, 1.0, 0.3, 1.0)
    } else if class.starts_with("monster_") || class.starts_with("npc_") {
        Vec4::new(1.0, 0.25, 0.2, 1.0)
    } else if class.starts_with("item_") || class.starts_with("weapon_") || class.starts_with("ammo_") {
        Vec4::new(1.0, 0.85, 0.2, 1.0)
    } else {
        Vec4::new(0.7, 0.7, 0.75, 1.0)
    };
    Some(Marker { label, position: to_y_up(origin), color })
}

/// A light entity's colour: Quake's `_color` (0-1 or 0-255) or Half-Life's
/// `_light` ("r g b brightness"), else warm white.
pub(crate) fn light_color(e: &Entity) -> Vec4 {
    let c = e.vec3("_color").or_else(|| e.vec3("color")).or_else(|| e.vec3("_light"));
    match c {
        Some(c) if c.max_element() > 0.0 => {
            let c = if c.max_element() > 1.0 { c / 255.0 } else { c };
            (c / c.max_element().max(1e-6)).clamp(Vec3::ZERO, Vec3::ONE).extend(1.0)
        }
        _ => Vec4::new(1.0, 0.95, 0.8, 1.0),
    }
}

//endregion

#[cfg(test)]
pub(crate) mod tests;
