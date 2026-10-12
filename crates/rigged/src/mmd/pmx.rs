//! PMX 2.0 and 2.1 (PMX Editor's format): a header naming the text
//! encoding (UTF-16LE or UTF-8) and the byte width of each kind of index,
//! then vertices, faces, texture paths, materials, bones, morphs, display
//! frames, rigid bodies, joints and (2.1) soft bodies.  Reading stops after
//! the morphs, since physics isn't run.

use super::{MmdBone, MmdMaterial, Model, Morph, MorphKind, sjis};
use crate::read::{Cursor, Result};
use crate::skeleton::Budget;
use slerp_scene::LoadError;

struct Header {
    utf8: bool,
    extra_uvs: usize,
    vertex_index: u8,
    texture_index: u8,
    material_index: u8,
    bone_index: u8,
    morph_index: u8,
    rigid_index: u8,
}

/// Reads a length-prefixed string in the file's encoding.
fn text(c: &mut Cursor, h: &Header) -> Result<String> {
    let len = c.i32("a name")?;
    let len = c.count(len.into(), 1, "text bytes")?;
    let b = c.bytes(len, "a name")?;
    Ok(if h.utf8 {
        String::from_utf8_lossy(b).into_owned()
    } else {
        let units: Vec<u16> = b.chunks_exact(2).map(|p| u16::from_le_bytes([p[0], p[1]])).collect();
        String::from_utf16_lossy(&units)
    })
}

/// A signed index of `size` bytes (bones, textures, materials, morphs);
/// -1 means none.
fn index(c: &mut Cursor, size: u8) -> Result<i32> {
    Ok(match size {
        1 => c.i8("an index")? as i32,
        2 => c.i16("an index")? as i32,
        _ => c.i32("an index")?,
    })
}

/// A vertex index, unsigned for 1 and 2 bytes.
fn vertex_index(c: &mut Cursor, size: u8) -> Result<u32> {
    Ok(match size {
        1 => c.u8("a vertex index")? as u32,
        2 => c.u16("a vertex index")? as u32,
        _ => c.i32("a vertex index")?.max(0) as u32,
    })
}

fn optional(i: i32) -> Option<usize> {
    usize::try_from(i).ok()
}

pub(super) fn parse(bytes: &[u8], budget: &mut Budget) -> Result<Model> {
    let mut c = Cursor::new(bytes);
    if c.bytes(4, "the header")? != b"PMX " {
        return Err(LoadError::NotMine);
    }
    let version = c.f32("the header")?;
    if !(2.0..2.2).contains(&version) {
        return Err(LoadError::Unsupported(format!("PMX version {version}")));
    }
    let globals = c.u8("the header")? as usize;
    let g = c.bytes(globals, "the header")?;
    if g.len() < 8 {
        return Err(LoadError::invalid("PMX: the header is too short"));
    }
    let width = |b: u8, what: &str| -> Result<u8> {
        match b {
            1 | 2 | 4 => Ok(b),
            _ => Err(LoadError::invalid(format!("PMX: {what} indices are {b} bytes wide"))),
        }
    };
    let h = Header {
        utf8: g[0] == 1,
        extra_uvs: (g[1] as usize).min(4),
        vertex_index: width(g[2], "vertex")?,
        texture_index: width(g[3], "texture")?,
        material_index: width(g[4], "material")?,
        bone_index: width(g[5], "bone")?,
        morph_index: width(g[6], "morph")?,
        rigid_index: width(g[7], "rigid body")?,
    };
    let mut m = Model { format: format!("MikuMikuDance PMX {version:.1}"), ..Default::default() };
    m.name = text(&mut c, &h)?;
    m.english_name = text(&mut c, &h)?;
    m.comment = text(&mut c, &h)?;
    let _english_comment = text(&mut c, &h)?;

    // 1. Vertices.
    let count = c.i32("the vertex count")?;
    let count = c.count(count.into(), 33, "vertices")?;
    budget.take(count, 96, "vertices")?;
    m.positions.reserve(count);
    for _ in 0..count {
        m.positions.push(c.vec3("a vertex")?);
        m.normals.push(c.vec3("a vertex")?);
        m.uvs.push(c.vec2("a vertex")?);
        c.skip(16 * h.extra_uvs, "a vertex")?;
        let b = h.bone_index;
        let (bones, weights) = match c.u8("a vertex")? {
            0 => ([index(&mut c, b)?, -1, -1, -1], [1.0, 0.0, 0.0, 0.0]),
            1 => {
                let (b0, b1) = (index(&mut c, b)?, index(&mut c, b)?);
                let w = c.f32("a vertex")?;
                ([b0, b1, -1, -1], [w, 1.0 - w, 0.0, 0.0])
            }
            kind @ (2 | 4) => {
                let bones = [index(&mut c, b)?, index(&mut c, b)?, index(&mut c, b)?, index(&mut c, b)?];
                let weights = [c.f32("a vertex")?, c.f32("a vertex")?, c.f32("a vertex")?, c.f32("a vertex")?];
                if kind == 4 {
                    m.qdef += 1;
                }
                (bones, weights)
            }
            3 => {
                let (b0, b1) = (index(&mut c, b)?, index(&mut c, b)?);
                let w = c.f32("a vertex")?;
                // The SDEF centre and its two reference points.
                c.skip(36, "a vertex")?;
                m.sdef += 1;
                ([b0, b1, -1, -1], [w, 1.0 - w, 0.0, 0.0])
            }
            other => return Err(LoadError::invalid(format!("PMX: unknown skinning type {other}"))),
        };
        m.bones.push(bones);
        m.weights.push(weights);
        let _edge = c.f32("a vertex")?;
    }

    // 2. Faces.
    let count = c.i32("the face count")?;
    let count = c.count(count.into(), h.vertex_index as usize, "face indices")?;
    budget.take(count, 4, "faces")?;
    m.indices.reserve(count);
    for _ in 0..count {
        m.indices.push(vertex_index(&mut c, h.vertex_index)?);
    }

    // 3. Texture paths.
    let count = c.i32("the texture count")?;
    let count = c.count(count.into(), 4, "textures")?;
    for _ in 0..count {
        m.textures.push(text(&mut c, &h)?);
    }

    // 4. Materials.
    let count = c.i32("the material count")?;
    let count = c.count(count.into(), 60, "materials")?;
    for _ in 0..count {
        let name = text(&mut c, &h)?;
        let _english = text(&mut c, &h)?;
        let diffuse = c.vec4("a material")?;
        let specular = c.vec3("a material")?;
        let shininess = c.f32("a material")?;
        let _ambient = c.vec3("a material")?;
        let flags = c.u8("a material")?;
        let _edge_colour = c.vec4("a material")?;
        let _edge_size = c.f32("a material")?;
        let texture = optional(index(&mut c, h.texture_index)?);
        let sphere_texture = optional(index(&mut c, h.texture_index)?);
        let sphere_mode = c.u8("a material")?;
        // The toon ramp: a texture, or one of MMD's ten shared ones.
        match c.u8("a material")? {
            0 => {
                index(&mut c, h.texture_index)?;
            }
            _ => {
                c.u8("a material")?;
            }
        }
        let _memo = text(&mut c, &h)?;
        let index_count = usize::try_from(c.i32("a material")?).unwrap_or(0);
        m.materials.push(MmdMaterial {
            name,
            diffuse,
            specular,
            shininess,
            flags,
            texture,
            sphere: sphere_texture.filter(|_| sphere_mode != 0).map(|t| (t, sphere_mode)),
            index_count,
        });
    }

    // 5. Bones.
    let count = c.i32("the bone count")?;
    let count = c.count(count.into(), 20, "bones")?;
    if count > crate::skeleton::MAX_BONES {
        return Err(LoadError::TooLarge(format!("{count} bones")));
    }
    for _ in 0..count {
        let name = text(&mut c, &h)?;
        let _english = text(&mut c, &h)?;
        let position = c.vec3("a bone")?;
        let parent = optional(index(&mut c, h.bone_index)?);
        let _layer = c.i32("a bone")?;
        let flags = c.u16("a bone")?;
        // The tail: a bone or an offset.
        if flags & 0x0001 != 0 {
            index(&mut c, h.bone_index)?;
        } else {
            c.skip(12, "a bone")?;
        }
        // Rotation or translation inherited from another bone.
        if flags & 0x0300 != 0 {
            index(&mut c, h.bone_index)?;
            c.f32("a bone")?;
        }
        if flags & 0x0400 != 0 {
            c.skip(12, "a bone")?;
        }
        if flags & 0x0800 != 0 {
            c.skip(24, "a bone")?;
        }
        if flags & 0x2000 != 0 {
            c.i32("a bone")?;
        }
        if flags & 0x0020 != 0 {
            index(&mut c, h.bone_index)?;
            c.i32("a bone")?;
            c.f32("a bone")?;
            let links = c.i32("a bone")?;
            let links = c.count(links.into(), h.bone_index as usize + 1, "IK links")?;
            for _ in 0..links {
                index(&mut c, h.bone_index)?;
                if c.u8("an IK link")? != 0 {
                    c.skip(24, "an IK link")?;
                }
            }
        }
        m.skeleton.push(MmdBone { sjis: sjis(&name), name, parent, position });
    }

    // 6. Morphs.  A file that ends here (or is cut short in them) keeps the
    //    morphs read so far, since the model itself is whole.
    if let Err(e @ LoadError::TooLarge(_)) = read_morphs(&mut c, &h, &mut m, budget) {
        return Err(e);
    }
    Ok(m)
}

fn read_morphs(c: &mut Cursor, h: &Header, m: &mut Model, budget: &mut Budget) -> Result<()> {
    let count = c.i32("the morph count")?;
    let count = c.count(count.into(), 12, "morphs")?;
    for _ in 0..count {
        let name = text(c, h)?;
        let _english = text(c, h)?;
        let _panel = c.u8("a morph")?;
        let kind = c.u8("a morph")?;
        let offsets = c.i32("a morph")?;
        let record = match kind {
            0 | 9 => h.morph_index as usize + 4,
            1 => h.vertex_index as usize + 12,
            2 => h.bone_index as usize + 28,
            3..=7 => h.vertex_index as usize + 16,
            8 => h.material_index as usize + 113,
            10 => h.rigid_index as usize + 25,
            other => return Err(LoadError::invalid(format!("PMX: unknown morph type {other}"))),
        };
        let offsets = c.count(offsets.into(), record, "morph offsets")?;
        budget.take(offsets, 16, "morphs")?;
        let kind = match kind {
            0 => {
                let mut members = Vec::with_capacity(offsets);
                for _ in 0..offsets {
                    let morph = index(c, h.morph_index)?;
                    let weight = c.f32("a group morph")?;
                    if let Some(i) = optional(morph) {
                        members.push((i, weight));
                    }
                }
                MorphKind::Group(members)
            }
            1 => {
                let mut list = Vec::with_capacity(offsets);
                for _ in 0..offsets {
                    let v = vertex_index(c, h.vertex_index)?;
                    list.push((v, c.vec3("a vertex morph")?));
                }
                MorphKind::Vertex(list)
            }
            _ => {
                // Bone, UV, material, flip and impulse morphs, skipped by
                // their record size.
                c.skip(offsets * record, "a morph")?;
                MorphKind::Other(match kind {
                    2 => "bone",
                    3..=7 => "UV",
                    8 => "material",
                    9 => "flip",
                    _ => "impulse",
                })
            }
        };
        m.morphs.push(Morph { sjis: sjis(&name), name, kind });
    }
    Ok(())
}
