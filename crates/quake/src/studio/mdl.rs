//! The `.mdl` file of a Source model: its header and the records this
//! loader uses (bones, textures and their search paths, the skin table, body
//! parts with their models and meshes, flex descriptors and flexes,
//! animation and sequence descriptions).
//!
//! Every record holds offsets relative to itself or to the file, so each is
//! read through `Bytes`, which fails instead of reading past the end, and
//! every count is capped before anything is allocated for it.  The layouts
//! are those of `studio.h` in Valve's SDKs, which kept the record sizes
//! from version 44 (Half-Life 2) to 49 (Portal 2, CS:GO) by turning unused
//! fields into new ones.

use crate::read::{Bytes, Result, cap};
use slerp_scene::LoadError;
use slerp_scene::glam::{Quat, Vec3};

/// Versions read: Half-Life 2 (44) to Portal 2 and CS:GO (49).
pub const VERSIONS: std::ops::RangeInclusive<i32> = 44..=49;

pub const HEADER_BYTES: usize = 408;
const BONE_BYTES: usize = 216;
const TEXTURE_BYTES: usize = 64;
const BODYPART_BYTES: usize = 16;
const MODEL_BYTES: usize = 148;
const MESH_BYTES: usize = 116;
const FLEX_BYTES: usize = 60;
pub const ANIMDESC_BYTES: usize = 100;
const SEQDESC_BYTES: usize = 212;

pub const MAX_BONES: usize = 512;
const MAX_TEXTURES: usize = 1024;
const MAX_BODYPARTS: usize = 256;
const MAX_MODELS: usize = 256;
const MAX_MESHES: usize = 1024;
const MAX_FLEXES: usize = 4096;
pub const MAX_ANIMS: usize = 8192;
const MAX_NAME: usize = 260;

/// The header flag that says flex deltas are fixed-point numbers scaled by
/// the header's `flVertAnimFixedPointScale`, not half-precision floats.
const FLAG_VERT_ANIM_FIXED_POINT: i32 = 0x0020_0000;

/*┌─────────────────╮
| Header          |
└─────────────────╯*/
//region Header

/// The counts and offsets of the header that this loader uses.
pub struct Header {
    pub version: i32,
    pub name: String,
    pub bones: (usize, usize),
    pub anims: (usize, usize),
    pub seqs: (usize, usize),
    pub textures: (usize, usize),
    pub cdtextures: (usize, usize),
    pub skin_refs: usize,
    pub skin_families: usize,
    pub skin_index: usize,
    pub bodyparts: (usize, usize),
    pub flexdescs: (usize, usize),
    pub includes: (usize, usize),
    pub anim_block_name: usize,
    pub anim_blocks: (usize, usize),
    pub surface_prop: usize,
    pub mass: f32,
    /// The scale of fixed-point flex deltas, or `None` when they are
    /// half-precision floats.
    pub fixed_point_flex: Option<f32>,
}

/// Reads the header, which must be a Source model of a version this loader
/// reads.  Later versions (Titanfall's 52 and 53, Apex's) are refused with
/// a message rather than misread.
pub fn header(b: Bytes) -> Result<Header> {
    if !b.starts_with(b"IDST") {
        return Err(LoadError::NotMine);
    }
    let version = b.i32(4)?;
    if version > *VERSIONS.end() {
        return Err(LoadError::Unsupported(format!(
            "Source model version {version} (Titanfall, Apex Legends and later games) isn't supported; versions 44 to 49 are"
        )));
    }
    if !VERSIONS.contains(&version) {
        return Err(LoadError::Unsupported(format!("studio model version {version}")));
    }
    b.slice(0, HEADER_BYTES)?;
    let pair = |at: usize, max: usize, what: &str| -> Result<(usize, usize)> {
        let count = cap(b.index(at)?, max, what)?;
        let offset = if count == 0 { 0 } else { b.index(at + 4)? };
        Ok((count, offset))
    };
    let flags = b.i32(152)?;
    let scale = b.f32(392)?;
    Ok(Header {
        version,
        name: b.name(12, 64)?,
        bones: pair(156, MAX_BONES, "bones")?,
        anims: pair(180, MAX_ANIMS, "animations")?,
        seqs: pair(188, MAX_ANIMS, "sequences")?,
        textures: pair(204, MAX_TEXTURES, "textures")?,
        cdtextures: pair(212, MAX_TEXTURES, "material folders")?,
        skin_refs: cap(b.index(220)?, MAX_TEXTURES, "skin references")?,
        skin_families: cap(b.index(224)?, MAX_TEXTURES, "skin families")?,
        skin_index: b.index(228)?,
        bodyparts: pair(232, MAX_BODYPARTS, "body parts")?,
        flexdescs: pair(260, MAX_FLEXES, "flex descriptors")?,
        includes: pair(336, MAX_TEXTURES, "included models")?,
        anim_block_name: b.index(348)?,
        anim_blocks: pair(352, 1 << 16, "animation blocks")?,
        surface_prop: b.index(308)?,
        mass: b.f32(328)?,
        fixed_point_flex: (flags & FLAG_VERT_ANIM_FIXED_POINT != 0 && scale != 0.0).then_some(scale),
    })
}

/// A NUL-terminated string at `at`, at most `MAX_NAME` bytes long; an
/// offset of 0 or past the end gives an empty name.
pub fn string_at(b: Bytes, at: usize) -> String {
    if at == 0 || at >= b.len() {
        return String::new();
    }
    let end = (at + MAX_NAME).min(b.len());
    crate::read::cstr(&b.data[at..end])
}

/// A string whose offset is stored at `field`, relative to `base`.
fn relative_string(b: Bytes, base: usize, field: usize) -> Result<String> {
    let rel = b.i32(field)?;
    if rel == 0 {
        return Ok(String::new());
    }
    Ok(base.checked_add_signed(rel as isize).map(|at| string_at(b, at)).unwrap_or_default())
}

//endregion

/*┌─────────────────╮
| Bones           |
└─────────────────╯*/
//region Bones

pub struct Bone {
    pub name: String,
    pub parent: Option<usize>,
    pub pos: Vec3,
    pub quat: Quat,
    /// The default rotation as Euler angles, which animated rotations are
    /// offsets from.
    pub rot: Vec3,
    pub pos_scale: Vec3,
    pub rot_scale: Vec3,
}

pub fn bones(b: Bytes, h: &Header) -> Result<Vec<Bone>> {
    let mut out = Vec::with_capacity(h.bones.0);
    for i in 0..h.bones.0 {
        let at = h.bones.1 + i * BONE_BYTES;
        b.slice(at, BONE_BYTES)?;
        let parent = b.i32(at + 4)?;
        // Parents come before their children; anything else would loop.
        let parent = usize::try_from(parent).ok().filter(|&p| p < i);
        let q = Quat::from_xyzw(b.f32(at + 44)?, b.f32(at + 48)?, b.f32(at + 52)?, b.f32(at + 56)?);
        let quat = if q.length_squared() > 1e-8 { q.normalize() } else { Quat::IDENTITY };
        out.push(Bone {
            name: relative_string(b, at, at)?,
            parent,
            pos: b.vec3(at + 32)?,
            quat,
            rot: b.vec3(at + 60)?,
            pos_scale: b.vec3(at + 72)?,
            rot_scale: b.vec3(at + 84)?,
        });
    }
    if out.is_empty() {
        return Err(LoadError::invalid("the model has no bones"));
    }
    Ok(out)
}

/// Source's Euler angles to a quaternion: rotations about X, then Y, then
/// Z (`AngleQuaternion` with a `RadianEuler`).
pub fn euler(a: Vec3) -> Quat {
    Quat::from_rotation_z(a.z) * Quat::from_rotation_y(a.y) * Quat::from_rotation_x(a.x)
}

//endregion

/*┌─────────────────╮
| Materials       |
└─────────────────╯*/
//region Materials

/// The texture (material) names, in the order meshes and the skin table
/// refer to them.
pub fn textures(b: Bytes, h: &Header) -> Result<Vec<String>> {
    let mut out = Vec::with_capacity(h.textures.0);
    for i in 0..h.textures.0 {
        let at = h.textures.1 + i * TEXTURE_BYTES;
        b.slice(at, TEXTURE_BYTES)?;
        out.push(relative_string(b, at, at)?);
    }
    Ok(out)
}

/// The folders below `materials/` searched for each texture name (Valve's
/// `$cdmaterials`), in order.
pub fn material_folders(b: Bytes, h: &Header) -> Result<Vec<String>> {
    let mut out = Vec::with_capacity(h.cdtextures.0);
    for i in 0..h.cdtextures.0 {
        let at = b.index(h.cdtextures.1 + 4 * i)?;
        out.push(string_at(b, at));
    }
    Ok(out)
}

/// The skin table: for each family, the texture each skin reference
/// stands for.  A model without one uses the references as texture
/// numbers.
pub fn skin_table(b: Bytes, h: &Header) -> Result<Vec<Vec<usize>>> {
    let mut families = Vec::with_capacity(h.skin_families);
    if h.skin_refs > 0 {
        b.array(h.skin_index, h.skin_families.saturating_mul(h.skin_refs), 2)?;
    }
    for f in 0..h.skin_families {
        let mut row = Vec::with_capacity(h.skin_refs);
        for r in 0..h.skin_refs {
            row.push(b.i16(h.skin_index + 2 * (f * h.skin_refs + r))?.max(0) as usize);
        }
        families.push(row);
    }
    Ok(families)
}

/// The models this one includes (shared animations, mostly), as stored.
pub fn includes(b: Bytes, h: &Header) -> Result<Vec<String>> {
    let mut out = Vec::new();
    for i in 0..h.includes.0 {
        let at = h.includes.1 + 8 * i;
        out.push(relative_string(b, at, at + 4)?);
    }
    Ok(out)
}

//endregion

/*┌─────────────────╮
| Body parts      |
└─────────────────╯*/
//region Body parts

pub struct BodyPart {
    pub name: String,
    pub models: Vec<Model>,
}

pub struct Model {
    pub name: String,
    /// The model's first vertex in the `.vvd` file's vertices.
    pub vertex_start: usize,
    pub meshes: Vec<Mesh>,
}

pub struct Mesh {
    /// The skin reference: a texture through the skin table.
    pub material: usize,
    /// The mesh's first vertex among its model's.
    pub vertex_offset: usize,
    pub vertex_count: usize,
    pub flexes: Vec<Flex>,
}

/// Morph data of one mesh for one flex descriptor (a facial shape).
pub struct Flex {
    pub desc: usize,
    /// The right-hand descriptor of a left and right pair, or `None`.
    pub pair: Option<usize>,
    pub verts: Vec<VertAnim>,
}

/// One vertex's move: `index` among the mesh's vertices, position and
/// normal offsets in the model's space, and for a left and right pair how
/// far the vertex belongs to the right side, 0 to 1.
pub struct VertAnim {
    pub index: usize,
    pub delta: Vec3,
    pub ndelta: Vec3,
    pub side: f32,
}

/// Every body part with its models and meshes; flexes too when
/// `with_flexes`.
pub fn bodyparts(b: Bytes, h: &Header, with_flexes: bool) -> Result<Vec<BodyPart>> {
    let mut parts = Vec::with_capacity(h.bodyparts.0);
    let mut flex_verts = 0usize;
    for p in 0..h.bodyparts.0 {
        let at = h.bodyparts.1 + p * BODYPART_BYTES;
        b.slice(at, BODYPART_BYTES)?;
        let name = relative_string(b, at, at)?;
        let count = cap(b.index(at + 4)?, MAX_MODELS, "models in a body part")?;
        let base = at.checked_add_signed(b.i32(at + 12)? as isize).ok_or_else(|| LoadError::invalid("model offset"))?;
        let mut models = Vec::with_capacity(count);
        for m in 0..count {
            let mat = base + m * MODEL_BYTES;
            b.slice(mat, MODEL_BYTES)?;
            let mesh_count = cap(b.index(mat + 72)?, MAX_MESHES, "meshes")?;
            let mesh_base =
                mat.checked_add_signed(b.i32(mat + 76)? as isize).ok_or_else(|| LoadError::invalid("mesh offset"))?;
            let vertex_bytes = b.index(mat + 84)?;
            let mut meshes = Vec::with_capacity(mesh_count);
            for k in 0..mesh_count {
                let mesh = mesh_base + k * MESH_BYTES;
                b.slice(mesh, MESH_BYTES)?;
                let flexes =
                    if with_flexes { read_flexes(b, h, mesh, &mut flex_verts).unwrap_or_default() } else { Vec::new() };
                meshes.push(Mesh {
                    material: b.index(mesh)?,
                    vertex_count: b.index(mesh + 8)?,
                    vertex_offset: b.index(mesh + 12)?,
                    flexes,
                });
            }
            models.push(Model { name: b.name(mat, 64)?, vertex_start: vertex_bytes / 48, meshes });
        }
        parts.push(BodyPart { name, models });
    }
    Ok(parts)
}

/// Morph vertices kept for a whole model, beyond which flexes are dropped.
const MAX_FLEX_VERTS: usize = 4 << 20;

fn read_flexes(b: Bytes, h: &Header, mesh: usize, total: &mut usize) -> Result<Vec<Flex>> {
    let count = cap(b.index(mesh + 16)?, MAX_FLEXES, "flexes")?;
    if count == 0 {
        return Ok(Vec::new());
    }
    let base = mesh.checked_add_signed(b.i32(mesh + 20)? as isize).ok_or_else(|| LoadError::invalid("flex offset"))?;
    let mut out = Vec::with_capacity(count);
    for i in 0..count {
        let at = base + i * FLEX_BYTES;
        b.slice(at, FLEX_BYTES)?;
        let desc = b.index(at)?;
        let verts = b.index(at + 20)?;
        let vert_at =
            at.checked_add_signed(b.i32(at + 24)? as isize).ok_or_else(|| LoadError::invalid("flex vertex offset"))?;
        let pair = b.index(at + 28)?;
        // Wrinkle flexes carry one more value per vertex.
        let stride = if b.u8(at + 32)? == 1 { 18 } else { 16 };
        let data = Bytes::new(b.array(vert_at, verts, stride)?);
        *total = total.saturating_add(verts);
        if *total > MAX_FLEX_VERTS || desc >= h.flexdescs.0 {
            continue;
        }
        let half3 = |at: usize| -> Result<Vec3> {
            let v = |o: usize| -> Result<f32> {
                let raw = data.i16(at + o)?;
                Ok(match h.fixed_point_flex {
                    Some(scale) => raw as f32 * scale,
                    None => finite(f16(raw as u16)),
                })
            };
            Ok(Vec3::new(v(0)?, v(2)?, v(4)?))
        };
        let mut list = Vec::with_capacity(verts);
        for v in 0..verts {
            let o = v * stride;
            list.push(VertAnim {
                index: data.u16(o)? as usize,
                side: data.u8(o + 3)? as f32 / 255.0,
                delta: half3(o + 4)?,
                ndelta: half3(o + 10)?,
            });
        }
        out.push(Flex { desc, pair: (pair > 0 && pair < h.flexdescs.0).then_some(pair), verts: list });
    }
    Ok(out)
}

/// The flex descriptors' names (FACS shapes such as `AU12L`, or a modeller's
/// own names).
pub fn flex_names(b: Bytes, h: &Header) -> Result<Vec<String>> {
    let mut out = Vec::with_capacity(h.flexdescs.0);
    for i in 0..h.flexdescs.0 {
        let at = h.flexdescs.1 + 4 * i;
        out.push(relative_string(b, at, at)?);
    }
    Ok(out)
}

fn finite(v: f32) -> f32 {
    if v.is_finite() { v } else { 0.0 }
}

/// An IEEE half-precision float.
pub fn f16(h: u16) -> f32 {
    let sign = if h & 0x8000 != 0 { -1.0 } else { 1.0 };
    let exp = ((h >> 10) & 0x1f) as i32;
    let frac = (h & 0x3ff) as f32;
    match exp {
        0 => sign * frac * 2f32.powi(-24),
        31 => {
            if frac == 0.0 {
                sign * f32::INFINITY
            } else {
                f32::NAN
            }
        }
        _ => sign * (1.0 + frac / 1024.0) * 2f32.powi(exp - 15),
    }
}

//endregion

/*┌─────────────────╮
| Sequences       |
└─────────────────╯*/
//region Sequences

/// What an animation description says about where its frames are.
pub struct AnimDesc {
    pub name: String,
    pub fps: f32,
    pub flags: i32,
    pub frames: usize,
    /// Offset of the description itself, which data offsets count from.
    pub at: usize,
    /// 0 for data in this file, else a block of the `.ani` file.
    pub block: usize,
    pub index: usize,
    /// Frames per section, 0 when the animation isn't split into sections.
    pub section_frames: usize,
    pub section_index: usize,
}

pub const ANIM_DELTA: i32 = 0x0004;
pub const ANIM_ALLZEROS: i32 = 0x0020;
/// CS:GO's frame-based animation encoding, which this loader doesn't read.
pub const ANIM_FRAMEANIM: i32 = 0x0040;

pub fn animdesc(b: Bytes, h: &Header, i: usize) -> Result<AnimDesc> {
    if i >= h.anims.0 {
        return Err(LoadError::invalid("animation index out of range"));
    }
    let at = h.anims.1 + i * ANIMDESC_BYTES;
    b.slice(at, ANIMDESC_BYTES)?;
    let fps = b.f32(at + 8)?;
    Ok(AnimDesc {
        name: relative_string(b, at, at + 4)?,
        fps: if fps > 0.0 && fps < 1000.0 { fps } else { 30.0 },
        flags: b.i32(at + 12)?,
        frames: b.index(at + 16)?,
        at,
        block: b.index(at + 52)?,
        index: b.index(at + 56)?,
        section_frames: b.index(at + 84)?,
        section_index: b.index(at + 80)?,
    })
}

/// A sequence's name and the animation that plays it: the first of its
/// blend grid.
pub struct Sequence {
    pub name: String,
    pub anim: usize,
}

pub fn sequences(b: Bytes, h: &Header) -> Result<Vec<Sequence>> {
    let mut out = Vec::with_capacity(h.seqs.0);
    for i in 0..h.seqs.0 {
        let at = h.seqs.1 + i * SEQDESC_BYTES;
        b.slice(at, SEQDESC_BYTES)?;
        let grid = at
            .checked_add_signed(b.i32(at + 60)? as isize)
            .ok_or_else(|| LoadError::invalid("sequence blend offset"))?;
        let anim = b.i16(grid)?;
        let Ok(anim) = usize::try_from(anim) else { continue };
        out.push(Sequence { name: relative_string(b, at, at + 4)?, anim });
    }
    Ok(out)
}

//endregion

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn half_floats() {
        assert_eq!(f16(0x3c00), 1.0);
        assert_eq!(f16(0xc000), -2.0);
        assert_eq!(f16(0x3555), 0.333_251_95);
        assert_eq!(f16(0x0001), 2f32.powi(-24));
        assert!(f16(0x7c00).is_infinite() && f16(0x7e00).is_nan());
    }

    #[test]
    fn euler_matches_source() {
        // AngleQuaternion's own formula, for angles (x, y, z).
        let (x, y, z) = (0.3f32, -0.7f32, 1.1f32);
        let (sr, cr) = (x * 0.5).sin_cos();
        let (sp, cp) = (y * 0.5).sin_cos();
        let (sy, cy) = (z * 0.5).sin_cos();
        let q = Quat::from_xyzw(
            sr * cp * cy - cr * sp * sy,
            cr * sp * cy + sr * cp * sy,
            cr * cp * sy - sr * sp * cy,
            cr * cp * cy + sr * sp * sy,
        );
        assert!(euler(Vec3::new(x, y, z)).dot(q).abs() > 0.99999);
    }
}
