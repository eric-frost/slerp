//! PMD, MikuMikuDance's original model format: fixed-size records with
//! Shift-JIS names.  Vertices carry two bones and the first one's weight
//! in percent; each material names its picture as `texture*sphere`;
//! morphs ("skins") are offsets of the vertices listed by the base morph.
//! An optional English section and the ten toon picture names follow the
//! display lists; physics comes last and isn't read.

use super::{MmdBone, MmdMaterial, Model, Morph, MorphKind};
use crate::read::{Cursor, Result, fixed_name, shift_jis};
use crate::skeleton::Budget;
use slerp_scene::LoadError;
use slerp_scene::glam::{Vec3, Vec4};

/// A fixed Shift-JIS name: decoded, and its bytes up to the first zero.
fn name(c: &mut Cursor, len: usize) -> Result<(String, Vec<u8>)> {
    let b = c.bytes(len, "a name")?;
    let end = b.iter().position(|&x| x == 0).unwrap_or(b.len());
    Ok((fixed_name(b, shift_jis), b[..end].to_vec()))
}

pub(super) fn parse(bytes: &[u8], budget: &mut Budget) -> Result<Model> {
    let mut c = Cursor::new(bytes);
    if c.bytes(3, "the header")? != b"Pmd" {
        return Err(LoadError::NotMine);
    }
    let version = c.f32("the header")?;
    let mut m = Model { format: format!("MikuMikuDance PMD {version:.1}"), ..Default::default() };
    m.name = name(&mut c, 20)?.0;
    m.comment = name(&mut c, 256)?.0;

    // 1. Vertices: 38 bytes each.
    let count = c.u32("the vertex count")?;
    let count = c.count(count.into(), 38, "vertices")?;
    budget.take(count, 96, "vertices")?;
    for _ in 0..count {
        m.positions.push(c.vec3("a vertex")?);
        m.normals.push(c.vec3("a vertex")?);
        m.uvs.push(c.vec2("a vertex")?);
        let (b0, b1) = (c.u16("a vertex")? as i32, c.u16("a vertex")? as i32);
        let w = c.u8("a vertex")?.min(100) as f32 / 100.0;
        let _edge = c.u8("a vertex")?;
        m.bones.push([b0, b1, -1, -1]);
        m.weights.push([w, 1.0 - w, 0.0, 0.0]);
    }

    // 2. Faces.
    let count = c.u32("the face count")?;
    let count = c.count(count.into(), 2, "face indices")?;
    budget.take(count, 4, "faces")?;
    for _ in 0..count {
        m.indices.push(c.u16("a face")? as u32);
    }

    // 3. Materials: 70 bytes each.  The picture names share one field.
    let count = c.u32("the material count")?;
    let count = c.count(count.into(), 70, "materials")?;
    for i in 0..count {
        let d = c.vec3("a material")?;
        let alpha = c.f32("a material")?;
        let shininess = c.f32("a material")?;
        let specular = c.vec3("a material")?;
        let _ambient = c.vec3("a material")?;
        let _toon = c.u8("a material")?;
        let edge = c.u8("a material")?;
        let index_count = c.u32("a material")? as usize;
        let (pictures, _) = name(&mut c, 20)?;
        let mut texture = None;
        let mut sphere = None;
        for part in pictures.split('*').map(str::trim).filter(|p| !p.is_empty()) {
            let lower = part.to_ascii_lowercase();
            m.textures.push(part.to_string());
            let t = m.textures.len() - 1;
            if lower.ends_with(".sph") {
                sphere = Some((t, 1));
            } else if lower.ends_with(".spa") {
                sphere = Some((t, 2));
            } else {
                texture = Some(t);
            }
        }
        m.materials.push(MmdMaterial {
            name: format!("material {i}"),
            diffuse: Vec4::new(d.x, d.y, d.z, alpha),
            specular,
            shininess,
            // PMD has no double-sided flag; MMD draws a material without
            // culling when it is translucent.
            flags: if alpha < 1.0 { 0x01 } else { 0 } | if edge != 0 { 0x10 } else { 0 },
            texture,
            sphere,
            index_count,
        });
    }

    // 4. Bones: 39 bytes each.
    let count = c.u16("the bone count")? as usize;
    let count = c.count(count as i64, 39, "bones")?;
    for _ in 0..count {
        let (name, sjis) = name(&mut c, 20)?;
        let parent = c.u16("a bone")?;
        let _tail = c.u16("a bone")?;
        let _kind = c.u8("a bone")?;
        let _ik = c.u16("a bone")?;
        let position = c.vec3("a bone")?;
        m.skeleton.push(MmdBone { name, sjis, parent: (parent != 0xFFFF).then_some(parent as usize), position });
    }

    // 5. IK chains, skipped.
    let count = c.u16("the IK count")? as usize;
    for _ in 0..count {
        c.skip(4, "an IK chain")?;
        let links = c.u8("an IK chain")? as usize;
        c.skip(6 + 2 * links, "an IK chain")?;
    }

    // 6. Morphs.  The base morph lists the vertices the others move, by
    //    position in its list.
    let count = c.u16("the morph count")? as usize;
    let count = c.count(count as i64, 25, "morphs")?;
    let mut base: Vec<u32> = Vec::new();
    for _ in 0..count {
        let (name, sjis) = name(&mut c, 20)?;
        let n = c.u32("a morph")?;
        let n = c.count(n.into(), 16, "morph offsets")?;
        let kind = c.u8("a morph")?;
        budget.take(n, 16, "morphs")?;
        let mut offsets: Vec<(u32, Vec3)> = Vec::with_capacity(n);
        for _ in 0..n {
            let index = c.u32("a morph")?;
            offsets.push((index, c.vec3("a morph")?));
        }
        if kind == 0 {
            base = offsets.iter().map(|o| o.0).collect();
            continue;
        }
        let offsets = offsets.into_iter().filter_map(|(i, d)| Some((*base.get(i as usize)?, d))).collect();
        m.morphs.push(Morph { name, sjis, kind: MorphKind::Vertex(offsets) });
    }

    // 7. The English section, if present, for the model's English name.
    //    Display lists come first.
    let english = (|| -> Result<String> {
        let lists = c.u8("display lists")? as usize;
        c.skip(2 * lists, "display lists")?;
        let names = c.u8("display lists")? as usize;
        c.skip(50 * names, "display lists")?;
        let entries = c.u32("display lists")? as usize;
        c.skip(entries.saturating_mul(3), "display lists")?;
        if c.u8("the English section")? == 0 {
            return Ok(String::new());
        }
        Ok(name(&mut c, 20)?.0)
    })();
    m.english_name = english.unwrap_or_default();
    Ok(m)
}
