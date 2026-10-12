//! Half-Life's studio models (MDL, magic "IDST", version 10): a bone
//! hierarchy, body parts with alternative submodels, meshes stored as
//! triangle strips and fans, 8-bit textures with their own palettes, and
//! sequences of run-length-compressed bone animation.
//!
//! Textures may live in a companion `<name>T.mdl` and sequences in
//! `<name>01.mdl` and up; both are read when `Limits::neighbours` allows.
//! A texture-only file (a `T.mdl` opened by itself) becomes a gallery.
//!
//! Each vertex follows exactly one bone, so meshes become skinned
//! primitives with one joint per vertex.  The skeleton keeps Half-Life's
//! units and Z-up axes under a root node that converts both.

use crate::read::{Bytes, Result, cap};
use crate::{find_in, pixels, read_neighbour, y_up_root};
use slerp_scene::glam::{Mat4, Quat, Vec2, Vec3, Vec4};
use slerp_scene::{
    AlphaMode, Animation, Asset, Channel, Info, Interpolation, Limits, LoadError, Material, Mesh, NamedImage, Node,
    Primitive, Property, Scene, Skin, TexRef, Texture, Transform,
};
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

const HEADER_BYTES: usize = 244;
const MAX_BONES: usize = 1024;
const MAX_SEQUENCES: usize = 4096;
const MAX_TEXTURES: usize = 1024;
const MAX_BODYPARTS: usize = 256;
const MAX_MESHES: usize = 4096;
const MAX_VERTICES: usize = 1 << 20;
/// Frames beyond this in one sequence are not decoded.
const MAX_SEQUENCE_FRAMES: usize = 1 << 16;

const TEX_FULLBRIGHT: i32 = 4;
const TEX_ADDITIVE: i32 = 0x20;
const TEX_MASKED: i32 = 0x40;

/*┌─────────────────╮
| Header          |
└─────────────────╯*/
//region Header

/// The counts and offsets of a studio header that this loader uses.
struct Header {
    bones: (usize, usize),
    sequences: (usize, usize),
    groups: (usize, usize),
    textures: (usize, usize),
    skin_refs: usize,
    skin_families: usize,
    skin_index: usize,
    bodyparts: (usize, usize),
}

impl Header {
    fn read(b: Bytes) -> Result<Header> {
        if !b.starts_with(b"IDST") || b.i32(4)? != 10 {
            return Err(LoadError::NotMine);
        }
        b.slice(0, HEADER_BYTES)?;
        let pair = |at: usize, max: usize, what: &str| -> Result<(usize, usize)> {
            Ok((cap(b.index(at)?, max, what)?, b.index(at + 4)?))
        };
        Ok(Header {
            bones: pair(140, MAX_BONES, "bones")?,
            sequences: pair(164, MAX_SEQUENCES, "sequences")?,
            groups: pair(172, MAX_SEQUENCES, "sequence groups")?,
            textures: pair(180, MAX_TEXTURES, "textures")?,
            skin_refs: cap(b.index(192)?, MAX_TEXTURES, "skin references")?,
            skin_families: cap(b.index(196)?, MAX_TEXTURES, "skin families")?,
            skin_index: b.index(200)?,
            bodyparts: pair(204, MAX_BODYPARTS, "body parts")?,
        })
    }
}

//endregion

/*┌─────────────────╮
| Loading         |
└─────────────────╯*/
//region Loading

pub fn load(path: &Path, data: &[u8], limits: &Limits) -> Result<Asset> {
    let b = Bytes::new(data);
    let h = Header::read(b)?;
    let mut info = Info { format: "Half-Life MDL".into(), ..Default::default() };

    // 1. Textures, from this file or its `T.mdl` companion.
    let companion =
        if h.textures.0 == 0 && h.bodyparts.0 > 0 && limits.neighbours { companion_file(path, "T") } else { None };
    let (textures, skin_table) = match &companion {
        Some(t) => {
            let tb = Bytes::new(t);
            match Header::read(tb).and_then(|th| read_textures(tb, &th, limits)) {
                Ok(v) => v,
                Err(e) => {
                    info.warnings.push(format!("texture file: {e}"));
                    (Vec::new(), Vec::new())
                }
            }
        }
        None => read_textures(b, &h, limits)?,
    };
    if h.textures.0 == 0 && companion.is_none() && h.bodyparts.0 > 0 {
        info.warnings.push(if limits.neighbours {
            "texture file (T.mdl) not found".into()
        } else {
            "textures are in a T.mdl file, which wasn't read".into()
        });
    }
    if h.bodyparts.0 == 0 {
        // A texture file opened by itself.
        if textures.is_empty() {
            return Err(LoadError::invalid("the model has neither body parts nor textures"));
        }
        return Ok(Asset::Gallery(
            textures.into_iter().map(|t| NamedImage { name: t.name, texture: t.texture }).collect(),
        ));
    }

    // 2. Bones and their rest pose: positions and Euler angles from each
    //    bone's default values.
    let mut scene =
        Scene { name: path.file_stem().and_then(|s| s.to_str()).unwrap_or("model").to_string(), ..Default::default() };
    let bones = read_bones(b, &h)?;
    let mut world = vec![Mat4::IDENTITY; bones.len()];
    for (i, bone) in bones.iter().enumerate() {
        let local = Mat4::from_rotation_translation(bone.rest_rotation(), bone.rest_position());
        world[i] = match bone.parent {
            Some(p) => world[p] * local,
            None => local,
        };
    }
    let root_node = {
        let mut n = Node::new("root");
        n.transform = y_up_root();
        scene.nodes.push(n);
        0
    };
    let bone_base = scene.nodes.len();
    for bone in &bones {
        let mut n = Node::new(bone.name.clone());
        n.transform = Transform { translation: bone.rest_position(), rotation: bone.rest_rotation(), scale: Vec3::ONE };
        scene.nodes.push(n);
    }
    for (i, bone) in bones.iter().enumerate() {
        let parent = bone.parent.map_or(root_node, |p| bone_base + p);
        scene.nodes[parent].children.push(bone_base + i);
    }
    let convert = y_up_root().matrix();
    scene.skins.push(Skin {
        name: "skeleton".into(),
        joints: (0..bones.len()).map(|i| bone_base + i).collect(),
        inverse_bind: world.iter().map(|w| (convert * *w).inverse()).collect(),
        skeleton: Some(root_node),
    });

    // 3. Materials: one per texture, for skin family 0.
    let mut tex_slots: Vec<Option<usize>> = Vec::with_capacity(textures.len());
    for t in &textures {
        scene.textures.push(t.texture.clone());
        tex_slots.push(Some(scene.textures.len() - 1));
        let mut m = Material {
            name: t.name.clone(),
            base_color: Vec4::ONE,
            base_color_texture: Some(TexRef::new(scene.textures.len() - 1)),
            roughness: 0.8,
            specular: 0.3,
            ..Default::default()
        };
        if t.flags & TEX_MASKED != 0 {
            m.alpha_mode = AlphaMode::Mask(0.5);
        }
        if t.flags & TEX_ADDITIVE != 0 {
            m.alpha_mode = AlphaMode::Blend;
            m.unlit = true;
        }
        if t.flags & TEX_FULLBRIGHT != 0 {
            m.unlit = true;
        }
        scene.materials.push(m);
    }
    let material_of = |skin_ref: usize| -> Option<usize> {
        let tex = skin_table.get(skin_ref).copied().unwrap_or(skin_ref);
        (tex < scene.materials.len()).then_some(tex)
    };

    // 4. Body parts: submodel 0 of each is shown; the others are hidden
    //    nodes the viewer can switch to.
    let mut triangles = 0u64;
    for part in 0..h.bodyparts.0 {
        let at = h.bodyparts.1 + part * 76;
        let part_name = b.name(at, 64)?;
        let models = cap(b.index(at + 64)?, MAX_BODYPARTS, "submodels")?;
        let model_index = b.index(at + 72)?;
        for m in 0..models {
            let mat = b.sub(model_index + m * 112, 112)?;
            let (mesh, tris) = read_submodel(b, mat, &world, &material_of)?;
            triangles += tris;
            if triangles > limits.max_triangles {
                return Err(LoadError::TooLarge(format!("{triangles} triangles")));
            }
            let name = mat.name(0, 64)?;
            let mut node = Node::new(if name.is_empty() { format!("{part_name} {m}") } else { name });
            node.visible = m == 0;
            node.skin = Some(0);
            scene.meshes.push(mesh);
            node.mesh = Some(scene.meshes.len() - 1);
            scene.nodes.push(node);
        }
    }
    scene.compute_roots();
    normalise_uvs(&mut scene);

    // 5. Sequences as skeletal clips.
    let mut groups: HashMap<usize, Option<Vec<u8>>> = HashMap::new();
    if limits.animations {
        for s in 0..h.sequences.0 {
            match read_sequence(b, &h, s, &bones, bone_base, path, limits, &mut groups) {
                Ok(Some(mut clip)) => {
                    let base = clip.name.clone();
                    let mut n = 2;
                    while scene.animations.iter().any(|a| a.name == clip.name) {
                        clip.name = format!("{base} {n}");
                        n += 1;
                    }
                    scene.animations.push(clip);
                }
                Ok(None) => {}
                Err(e) => info.warnings.push(format!("sequence {s}: {e}")),
            }
        }
        if groups.values().any(Option::is_none) {
            info.warnings.push("some sequences are in sequence group files that weren't found".into());
        }
    }

    info.details.push(("Bones".into(), bones.len().to_string()));
    info.details.push(("Sequences".into(), h.sequences.0.to_string()));
    info.details.push(("Body parts".into(), h.bodyparts.0.to_string()));
    info.details.push(("Textures".into(), textures.iter().map(|t| t.name.as_str()).collect::<Vec<_>>().join(", ")));
    if h.skin_families > 1 {
        info.details.push(("Skin families".into(), h.skin_families.to_string()));
    }
    scene.info = info;
    Ok(Asset::Scene(scene))
}

/// Reads `<model><suffix>.mdl` next to the model, such as the `T.mdl`
/// texture file or the `01.mdl` sequence group.
fn companion_file(path: &Path, suffix: &str) -> Option<Vec<u8>> {
    let stem = path.file_stem()?.to_str()?;
    let file = find_in(path.parent()?, &format!("{stem}{suffix}.mdl"))?;
    read_neighbour(&file)
}

//endregion

/*┌─────────────────╮
| Parts           |
└─────────────────╯*/
//region Parts

struct HlTexture {
    name: String,
    flags: i32,
    texture: Arc<Texture>,
}

/// Decodes every texture through its own palette, and returns them with
/// skin family 0's table from skin reference to texture.
fn read_textures(b: Bytes, h: &Header, limits: &Limits) -> Result<(Vec<HlTexture>, Vec<usize>)> {
    let mut out = Vec::with_capacity(h.textures.0);
    for i in 0..h.textures.0 {
        let at = h.textures.1 + i * 80;
        let name = b.name(at, 64)?;
        let flags = b.i32(at + 64)?;
        let (w, hh) = (b.u32(at + 68)?, b.u32(at + 72)?);
        let n = pixels::check_size(w, hh)?;
        let data = b.index(at + 76)?;
        let indices = b.slice(data, n)?;
        let palette = b.slice(data + n, 768)?;
        let masked = flags & TEX_MASKED != 0;
        let mut rgba = pixels::expand(indices, w, hh, palette, masked.then_some(255));
        if flags & TEX_ADDITIVE != 0 {
            // Additive textures add their colour, so black adds nothing:
            // brightness stands in for opacity.
            for p in rgba.chunks_exact_mut(4) {
                p[3] = p[0].max(p[1]).max(p[2]);
            }
        }
        let texture =
            pixels::texture(w, hh, rgba, limits.max_texture_size, "Half-Life MDL", &format!("{name} ({w}x{hh} 8-bit)"));
        out.push(HlTexture { name, flags, texture: Arc::new(texture) });
    }
    let mut table = Vec::with_capacity(h.skin_refs);
    if h.skin_families > 0 {
        for r in 0..h.skin_refs {
            table.push(b.i16(h.skin_index + 2 * r)?.max(0) as usize);
        }
    }
    Ok((out, table))
}

struct Bone {
    name: String,
    parent: Option<usize>,
    /// Default position and Euler angles (radians), then their scales for
    /// animation values.
    value: [f32; 6],
    scale: [f32; 6],
}

impl Bone {
    fn rest_position(&self) -> Vec3 {
        Vec3::new(self.value[0], self.value[1], self.value[2])
    }

    fn rest_rotation(&self) -> Quat {
        angles(self.value[3], self.value[4], self.value[5])
    }
}

/// Half-Life's Euler angles: rotate about X, then Y, then Z.
fn angles(x: f32, y: f32, z: f32) -> Quat {
    Quat::from_rotation_z(z) * Quat::from_rotation_y(y) * Quat::from_rotation_x(x)
}

fn read_bones(b: Bytes, h: &Header) -> Result<Vec<Bone>> {
    let mut out = Vec::with_capacity(h.bones.0);
    for i in 0..h.bones.0 {
        let at = h.bones.1 + i * 112;
        let parent = b.i32(at + 32)?;
        let mut value = [0.0; 6];
        let mut scale = [0.0; 6];
        for k in 0..6 {
            value[k] = b.f32(at + 64 + 4 * k)?;
            scale[k] = b.f32(at + 88 + 4 * k)?;
        }
        // Parents come before children; anything else would loop.
        let parent = usize::try_from(parent).ok().filter(|&p| p < i);
        out.push(Bone { name: b.name(at, 32)?, parent, value, scale });
    }
    if out.is_empty() {
        return Err(LoadError::invalid("the model has no bones"));
    }
    Ok(out)
}

/// Builds one submodel's mesh: its vertices moved from bone space to the
/// model's rest pose and then to Y-up metres, one primitive per mesh.
fn read_submodel(
    b: Bytes,
    m: Bytes,
    world: &[Mat4],
    material_of: &dyn Fn(usize) -> Option<usize>,
) -> Result<(Mesh, u64)> {
    let mesh_count = cap(m.index(72)?, MAX_MESHES, "meshes")?;
    let mesh_index = m.index(76)?;
    let vert_count = cap(m.index(80)?, MAX_VERTICES, "vertices")?;
    let vert_bones = b.array(m.index(84)?, vert_count, 1)?;
    let verts = Bytes::new(b.array(m.index(88)?, vert_count, 12)?);
    let norm_count = cap(m.index(92)?, MAX_VERTICES, "normals")?;
    let norm_bones = b.array(m.index(96)?, norm_count, 1)?;
    let norms = Bytes::new(b.array(m.index(100)?, norm_count, 12)?);
    let convert = y_up_root().matrix();
    let bone_of = |i: u8| -> usize { if (i as usize) < world.len() { i as usize } else { 0 } };

    let mut out = Mesh { name: m.name(0, 64)?, ..Default::default() };
    let mut triangles = 0u64;
    for k in 0..mesh_count {
        let mh = b.sub(mesh_index + k * 20, 20)?;
        let skin_ref = mh.index(8)?;
        let mut prim = Primitive { material: material_of(skin_ref), ..Default::default() };
        let mut map: HashMap<(i16, i16, i16, i16), u32> = HashMap::new();
        let mut at = mh.index(4)?;

        // Triangle commands: a count (negative for a fan, positive for a
        // strip, zero to end) and that many (vertex, normal, s, t).
        loop {
            let n = b.i16(at)?;
            at += 2;
            if n == 0 {
                break;
            }
            let fan = n < 0;
            let count = n.unsigned_abs() as usize;
            let mut ids = Vec::with_capacity(count);
            for _ in 0..count {
                let key = (b.i16(at)?, b.i16(at + 2)?, b.i16(at + 4)?, b.i16(at + 6)?);
                at += 8;
                let (vi, ni) = (key.0 as usize, key.1 as usize);
                if key.0 < 0 || vi >= vert_count || key.1 < 0 || ni >= norm_count {
                    return Err(LoadError::invalid("a triangle command indexes past the vertices"));
                }
                let next = prim.positions.len() as u32;
                let id = *map.entry(key).or_insert(next);
                if id == next {
                    let bone = bone_of(vert_bones[vi]);
                    let nbone = bone_of(norm_bones[ni]);
                    let p = world[bone].transform_point3(verts.vec3(vi * 12)?);
                    let nrm = world[nbone].transform_vector3(norms.vec3(ni * 12)?);
                    prim.positions.push(convert.transform_point3(p));
                    prim.normals.push(convert.transform_vector3(nrm).normalize_or(Vec3::Y));
                    prim.uv0.push(Vec2::new(key.2 as f32, key.3 as f32));
                    prim.joints.push([bone as u16, 0, 0, 0]);
                    prim.weights.push(Vec4::X);
                }
                ids.push(id);
            }
            // Half-Life's front faces wind clockwise, so each triangle is
            // emitted reversed.
            for i in 2..count {
                let (a, bb, c) = if fan {
                    (ids[0], ids[i - 1], ids[i])
                } else if i % 2 == 0 {
                    (ids[i - 2], ids[i - 1], ids[i])
                } else {
                    (ids[i - 1], ids[i - 2], ids[i])
                };
                prim.indices.extend_from_slice(&[a, c, bb]);
                triangles += 1;
            }
        }
        if prim.indices.is_empty() {
            continue;
        }
        out.primitives.push(prim);
    }
    Ok((out, triangles))
}

/// Divides texel coordinates by each primitive's texture size once the
/// textures are known.
fn normalise_uvs(scene: &mut Scene) {
    for mesh in &mut scene.meshes {
        for prim in &mut mesh.primitives {
            let size = prim
                .material
                .and_then(|m| scene.materials[m].base_color_texture)
                .map(|t| {
                    let tex = &scene.textures[t.texture];
                    Vec2::new(tex.source.width.max(1) as f32, tex.source.height.max(1) as f32)
                })
                .unwrap_or(Vec2::ONE);
            for uv in &mut prim.uv0 {
                *uv /= size;
            }
        }
    }
}

//endregion

/*┌─────────────────╮
| Sequences       |
└─────────────────╯*/
//region Sequences

/// Decodes sequence `s` into a clip: a translation and a rotation channel
/// per bone, one key per frame.  Only the first blend is used.
#[allow(clippy::too_many_arguments)]
fn read_sequence(
    b: Bytes,
    h: &Header,
    s: usize,
    bones: &[Bone],
    bone_base: usize,
    path: &Path,
    limits: &Limits,
    groups: &mut HashMap<usize, Option<Vec<u8>>>,
) -> Result<Option<Animation>> {
    let at = h.sequences.1 + s * 176;
    let name = b.name(at, 32)?;
    let fps = b.f32(at + 32)?;
    let fps = if fps > 0.0 { fps } else { 30.0 };
    let frames = b.index(at + 56)?.min(MAX_SEQUENCE_FRAMES);
    let anim_index = b.index(at + 124)?;
    let group = b.index(at + 156)?;
    if frames == 0 {
        return Ok(None);
    }

    // 1. The bytes holding the animation: this file, or a group file.
    let group_bytes;
    let data = if group == 0 {
        b
    } else {
        if group >= h.groups.0 {
            return Err(LoadError::invalid("sequence group out of range"));
        }
        let entry = groups
            .entry(group)
            .or_insert_with(|| if limits.neighbours { companion_file(path, &format!("{group:02}")) } else { None });
        match entry {
            Some(bytes) => {
                group_bytes = bytes.clone();
                Bytes::new(&group_bytes)
            }
            None => return Ok(None),
        }
    };

    // 2. Per bone, the six channel offsets, then per frame the decoded
    //    values.
    let anims = Bytes::new(data.array(anim_index, bones.len(), 12)?);
    let times: Vec<f32> = (0..frames).map(|f| f as f32 / fps).collect();
    let mut channels = Vec::with_capacity(bones.len() * 2);
    for (bi, bone) in bones.iter().enumerate() {
        let base = anim_index + bi * 12;
        let mut translation = Vec::with_capacity(frames * 3);
        let mut rotation = Vec::with_capacity(frames * 4);
        let mut offsets = [0usize; 6];
        for (k, o) in offsets.iter_mut().enumerate() {
            *o = anims.u16(bi * 12 + 2 * k)? as usize;
        }
        for f in 0..frames {
            let mut v = bone.value;
            for k in 0..6 {
                if offsets[k] != 0 {
                    v[k] += anim_value(data, base + offsets[k], f)? as f32 * bone.scale[k];
                }
            }
            translation.extend_from_slice(&[v[0], v[1], v[2]]);
            let q = angles(v[3], v[4], v[5]);
            rotation.extend_from_slice(&[q.x, q.y, q.z, q.w]);
        }
        let node = bone_base + bi;
        channels.push(Channel {
            node,
            property: Property::Translation,
            interpolation: Interpolation::Linear,
            times: times.clone(),
            values: translation,
        });
        channels.push(Channel {
            node,
            property: Property::Rotation,
            interpolation: Interpolation::Linear,
            times: times.clone(),
            values: rotation,
        });
    }
    Ok(Some(Animation { name, duration: *times.last().unwrap_or(&0.0), channels }))
}

/// One channel's value at `frame`.  Values are stored as runs: a header of
/// (valid, total) bytes says the next `valid` 16-bit values are distinct
/// and the last of them repeats until `total` frames are covered.
fn anim_value(b: Bytes, at: usize, frame: usize) -> Result<i16> {
    let mut at = at;
    let mut k = frame;
    for _ in 0..=MAX_SEQUENCE_FRAMES {
        let (valid, total) = (b.u8(at)? as usize, b.u8(at + 1)? as usize);
        if total == 0 {
            return Err(LoadError::invalid("an animation run covers no frames"));
        }
        if total > k {
            let i = if valid > k { k + 1 } else { valid };
            return b.i16(at + 2 * i);
        }
        k -= total;
        at += 2 * (valid + 1);
    }
    Err(LoadError::invalid("animation runs never reach the frame"))
}

//endregion

#[cfg(test)]
mod tests;
