//! Inter-Quake Model (`.iqm`, version 2), the binary model format of
//! Sauerbraten, Tesseract and other id-style engines: meshes over shared
//! vertex arrays, joints with their bind pose relative to their parents,
//! and animations as frames of packed 16-bit pose channels, each scaled
//! and offset per joint.  Coordinates are Blender's (right-handed, Z up,
//! models facing -Y); a unit is a metre in the exporters' defaults.
//! Version 1 files are refused with a message.

use crate::basis::Basis;
use crate::read::{Cursor, Result, truncated};
use crate::skeleton::{Bone, Budget, Rig, sampled_clip, strongest_four, unique_names};
use crate::textures::Textures;
use slerp_scene::glam::{Mat3, Quat, Vec2, Vec3, Vec4};
use slerp_scene::{Limits, LoadError, Material, Mesh, Node, Primitive, Scene, TexRef, Transform};
use std::path::Path;

/// Blender's axes: Z up, models facing -Y.
pub fn basis() -> Basis {
    Basis::new(Mat3::from_cols(Vec3::X, Vec3::NEG_Z, Vec3::Y), 1.0)
}

/// Whether triangles in file order face outwards in the file's own
/// coordinates: the format's front faces are clockwise.
pub const OUTWARD: bool = false;

#[derive(Default)]
struct Header {
    version: u32,
    text: (usize, usize),
    meshes: (usize, usize),
    vertex_arrays: (usize, usize),
    vertexes: usize,
    triangles: (usize, usize),
    joints: (usize, usize),
    poses: (usize, usize),
    anims: (usize, usize),
    frames: usize,
    frame_channels: usize,
    ofs_frames: usize,
}

fn header(bytes: &[u8]) -> Result<Header> {
    let mut c = Cursor::new(bytes);
    if c.bytes(16, "the header")? != b"INTERQUAKEMODEL\0" {
        return Err(LoadError::NotMine);
    }
    let mut u = || -> Result<usize> { Ok(c.u32("the header")? as usize) };
    let version = u()? as u32;
    if version != 2 {
        return Err(LoadError::Unsupported(format!("IQM version {version} (only version 2 is supported)")));
    }
    let _filesize = u()?;
    let _flags = u()?;
    let text = (u()?, u()?);
    let meshes = (u()?, u()?);
    let vertex_arrays_count = u()?;
    let vertexes = u()?;
    let vertex_arrays = (vertex_arrays_count, u()?);
    let triangles = (u()?, u()?);
    let _adjacency = u()?;
    let joints = (u()?, u()?);
    let poses = (u()?, u()?);
    let anims = (u()?, u()?);
    let frames = u()?;
    let frame_channels = u()?;
    let ofs_frames = u()?;
    Ok(Header {
        version,
        text,
        meshes,
        vertex_arrays,
        vertexes,
        triangles,
        joints,
        poses,
        anims,
        frames,
        frame_channels,
        ofs_frames,
    })
}

/// The records of a table at `offset`, `count` of `size` bytes, checked
/// to lie inside the file.
fn table<'a>(bytes: &'a [u8], (count, offset): (usize, usize), size: usize, what: &str) -> Result<Cursor<'a>> {
    let c = Cursor::at(bytes, offset, what)?;
    c.count(count as i64, size, what)?;
    Ok(c)
}

/// A string from the text block, at `offset`.
fn text(bytes: &[u8], h: &Header, offset: u32) -> String {
    let (len, start) = h.text;
    let block = bytes.get(start..start.saturating_add(len).min(bytes.len())).unwrap_or(&[]);
    let s = block.get(offset as usize..).unwrap_or(&[]);
    let end = s.iter().position(|&b| b == 0).unwrap_or(s.len());
    String::from_utf8_lossy(&s[..end]).into_owned()
}

/// One vertex array: what it holds, its component format and count, and
/// where.
struct Array {
    kind: u32,
    format: u32,
    size: usize,
    offset: usize,
}

impl Array {
    fn component_bytes(&self) -> Option<usize> {
        Some(match self.format {
            0 | 1 => 1,
            2 | 3 | 6 => 2,
            4 | 5 | 7 => 4,
            8 => 8,
            _ => return None,
        })
    }

    /// Vertex `v`'s components as floats, integers normalised to 0..1 or
    /// -1..1 when `normalised`.
    fn read(&self, bytes: &[u8], v: usize, normalised: bool, out: &mut [f32]) {
        let Some(cb) = self.component_bytes() else { return };
        let start = self.offset + v * self.size * cb;
        for (i, slot) in out.iter_mut().enumerate().take(self.size) {
            let at = start + i * cb;
            let Some(b) = bytes.get(at..at + cb) else { return };
            let n = |x: f32, max: f32| if normalised { x / max } else { x };
            *slot = match self.format {
                0 => n(b[0] as i8 as f32, 127.0).max(-1.0),
                1 => n(b[0] as f32, 255.0),
                2 => n(i16::from_le_bytes([b[0], b[1]]) as f32, 32767.0).max(-1.0),
                3 => n(u16::from_le_bytes([b[0], b[1]]) as f32, 65535.0),
                4 => i32::from_le_bytes([b[0], b[1], b[2], b[3]]) as f32,
                5 => u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as f32,
                6 => half(u16::from_le_bytes([b[0], b[1]])),
                7 => f32::from_le_bytes([b[0], b[1], b[2], b[3]]),
                _ => f64::from_le_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]) as f32,
            };
            if !slot.is_finite() {
                *slot = 0.0;
            }
        }
    }
}

/// An IEEE half-precision float.
fn half(h: u16) -> f32 {
    let sign = if h & 0x8000 != 0 { -1.0 } else { 1.0 };
    let exp = ((h >> 10) & 0x1f) as i32;
    let frac = (h & 0x3ff) as f32;
    sign * match exp {
        0 => frac * 2f32.powi(-24),
        31 => 0.0,
        e => (1.0 + frac / 1024.0) * 2f32.powi(e - 15),
    }
}

pub fn load(path: &Path, bytes: &[u8], limits: &Limits) -> Result<Scene> {
    let h = header(bytes)?;
    let basis = basis();
    let mut budget = Budget::new(bytes.len() as u64);

    // 1. Joints: bind pose relative to the parent.
    let mut c = table(bytes, h.joints, 48, "joints")?;
    let mut bones = Vec::with_capacity(h.joints.0.min(crate::skeleton::MAX_BONES));
    for _ in 0..h.joints.0 {
        let name = text(bytes, &h, c.u32("a joint")?);
        let parent = usize::try_from(c.i32("a joint")?).ok();
        let t = c.vec3("a joint")?;
        let r = c.quat("a joint")?;
        let s = c.vec3("a joint")?;
        let r = if r.length_squared() > 1e-12 { r.normalize() } else { Quat::IDENTITY };
        let local = basis.transform(Transform { translation: t, rotation: r, scale: s });
        bones.push(Bone { name, parent, local });
    }
    let rig = Rig::from_local(bones)?;
    let mut scene = Scene::default();
    let base = rig.add_nodes(&mut scene);
    let skinned = !rig.is_empty();
    if skinned {
        scene.skins.push(rig.skin(base, "skeleton"));
    }

    // 2. Vertex arrays, read into one vertex list.
    let n = h.vertexes;
    let mut arrays = Vec::new();
    let mut c = table(bytes, h.vertex_arrays, 20, "vertex arrays")?;
    for _ in 0..h.vertex_arrays.0.min(64) {
        let kind = c.u32("a vertex array")?;
        let _flags = c.u32("a vertex array")?;
        let format = c.u32("a vertex array")?;
        let size = (c.u32("a vertex array")? as usize).min(16);
        let offset = c.u32("a vertex array")? as usize;
        let array = Array { kind, format, size, offset };
        let Some(cb) = array.component_bytes() else { continue };
        let end = n.checked_mul(size * cb).and_then(|b| b.checked_add(offset));
        if end.is_none_or(|e| e > bytes.len()) {
            return Err(truncated("a vertex array"));
        }
        arrays.push(array);
    }
    budget.take(n, 96, "vertices")?;
    let array = |kind: u32| arrays.iter().find(|a| a.kind == kind);
    let mut all = Primitive::default();
    if let Some(a) = array(0) {
        for v in 0..n {
            let mut v4 = [0.0; 4];
            a.read(bytes, v, false, &mut v4);
            all.positions.push(basis.point(Vec3::new(v4[0], v4[1], v4[2])));
        }
    } else {
        return Err(LoadError::invalid("IQM: no positions"));
    }
    if let Some(a) = array(1) {
        for v in 0..n {
            let mut v4 = [0.0; 4];
            a.read(bytes, v, true, &mut v4);
            all.uv0.push(Vec2::new(v4[0], v4[1]));
        }
    }
    if let Some(a) = array(2) {
        for v in 0..n {
            let mut v4 = [0.0; 4];
            a.read(bytes, v, true, &mut v4);
            all.normals.push(basis.dir(Vec3::new(v4[0], v4[1], v4[2])).normalize_or(Vec3::Y));
        }
    }
    if let Some(a) = array(6) {
        for v in 0..n {
            let mut v4 = [1.0; 4];
            a.read(bytes, v, true, &mut v4);
            let lin = |c: f32| if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) };
            all.colors.push(Vec4::new(lin(v4[0]), lin(v4[1]), lin(v4[2]), v4[3]));
        }
    }
    if let (Some(ji), Some(jw)) = (array(4), array(5))
        && skinned
    {
        let mut list = Vec::with_capacity(4);
        for v in 0..n {
            let mut v4 = [0.0; 4];
            let mut w = [0.0; 4];
            ji.read(bytes, v, false, &mut v4);
            jw.read(bytes, v, true, &mut w);
            list.clear();
            for k in 0..4 {
                let j = v4[k] as u32;
                if (j as usize) < rig.len() {
                    list.push((j, w[k]));
                }
            }
            let (joints, weights) = strongest_four(&mut list, 0);
            all.joints.push(joints);
            all.weights.push(weights);
        }
    }

    // 3. Triangles and meshes: a primitive per mesh.
    let mut c = table(bytes, h.triangles, 12, "triangles")?;
    budget.take(h.triangles.0, 12, "triangles")?;
    let mut tris = Vec::with_capacity(h.triangles.0);
    for _ in 0..h.triangles.0 {
        tris.push([c.u32("a triangle")?, c.u32("a triangle")?, c.u32("a triangle")?]);
    }
    let flip = basis.flip_winding(OUTWARD);
    let mut textures = Textures::new(path, limits);
    let mut c = table(bytes, h.meshes, 24, "meshes")?;
    for _ in 0..h.meshes.0.min(65_536) {
        let name = text(bytes, &h, c.u32("a mesh")?);
        let material_name = text(bytes, &h, c.u32("a mesh")?);
        let _first_vertex = c.u32("a mesh")?;
        let _vertex_count = c.u32("a mesh")?;
        let first = c.u32("a mesh")? as usize;
        let count = c.u32("a mesh")? as usize;
        let range = tris.get(first..first.saturating_add(count).min(tris.len())).unwrap_or(&[]);
        let indices: Vec<u32> = range.iter().flat_map(|t| if flip { [t[0], t[2], t[1]] } else { *t }).collect();
        let (mut prim, _) = crate::mesh::gather(&all, &indices);
        if prim.indices.is_empty() {
            continue;
        }
        prim.ensure_normals();
        prim.material = Some(scene.materials.len());
        scene.materials.push(material(&material_name, &mut textures));
        scene.meshes.push(Mesh { name: name.clone(), primitives: vec![prim], target_names: Vec::new() });
        let mut node = Node::new(if name.is_empty() { format!("mesh {}", scene.meshes.len()) } else { name });
        node.mesh = Some(scene.meshes.len() - 1);
        node.skin = skinned.then_some(0);
        scene.nodes.push(node);
        scene.roots.push(scene.nodes.len() - 1);
    }
    scene.textures = std::mem::take(&mut textures.list);
    scene.info.warnings.append(&mut textures.warnings);

    // 4. Animations.
    if limits.animations && skinned && h.anims.0 > 0 {
        scene.animations = animations(bytes, &h, &rig, base, &basis, &mut budget)?;
    }
    scene.info.format = format!("Inter-Quake Model (IQM {})", h.version);
    scene.info.details.push(("Meshes".into(), scene.meshes.len().to_string()));
    crate::describe_rig(&mut scene, &rig);
    Ok(scene)
}

fn animations(
    bytes: &[u8],
    h: &Header,
    rig: &Rig,
    base: usize,
    basis: &Basis,
    budget: &mut Budget,
) -> Result<Vec<slerp_scene::Animation>> {
    // 1. Poses: per joint, which channels animate and their scales.
    let mut c = table(bytes, h.poses, 88, "poses")?;
    let mut poses = Vec::with_capacity(h.poses.0.min(rig.len()));
    for _ in 0..h.poses.0.min(rig.len()) {
        let _parent = c.i32("a pose")?;
        let mask = c.u32("a pose")?;
        let mut offset = [0f32; 10];
        let mut scale = [0f32; 10];
        for o in &mut offset {
            *o = c.f32("a pose")?;
        }
        for s in &mut scale {
            *s = c.f32("a pose")?;
        }
        poses.push((mask, offset, scale));
    }
    // 2. Frames: the animated channels of every pose, as 16-bit values.
    let frame_bytes = h.frame_channels.checked_mul(2).ok_or_else(|| truncated("frames"))?;
    let frames = table(bytes, (h.frames, h.ofs_frames), frame_bytes, "frames")?;
    budget.take(h.frames.saturating_mul(poses.len()), 40, "poses")?;
    let mut locals = Vec::with_capacity(h.frames * poses.len());
    let mut fc = frames;
    for _ in 0..h.frames {
        let mut used = 0;
        for &(mask, offset, scale) in &poses {
            let mut v = offset;
            for (i, slot) in v.iter_mut().enumerate() {
                if mask & (1 << i) != 0 {
                    if used < h.frame_channels {
                        *slot += fc.u16("a frame")? as f32 * scale[i];
                    }
                    used += 1;
                }
            }
            let q = Quat::from_xyzw(v[3], v[4], v[5], v[6]);
            let q = if q.is_finite() && q.length_squared() > 1e-12 { q.normalize() } else { Quat::IDENTITY };
            let t =
                Transform { translation: Vec3::new(v[0], v[1], v[2]), rotation: q, scale: Vec3::new(v[7], v[8], v[9]) };
            locals.push(basis.transform(t));
        }
        // Channels the poses don't account for are skipped.
        if used < h.frame_channels {
            fc.skip((h.frame_channels - used) * 2, "a frame")?;
        }
    }
    // 3. Clips over ranges of frames.
    let mut c = table(bytes, h.anims, 20, "animations")?;
    let rest = rig.rest();
    let mut clips = Vec::new();
    for _ in 0..h.anims.0.min(4096) {
        let name = text(bytes, h, c.u32("an animation")?);
        let first = c.u32("an animation")? as usize;
        let count = c.u32("an animation")? as usize;
        let fps = c.f32("an animation")?;
        let _flags = c.u32("an animation")?;
        let bones = poses.len();
        let range = locals.get(first * bones..first.saturating_add(count).min(h.frames) * bones).unwrap_or(&[]);
        if range.is_empty() {
            continue;
        }
        let fps = if fps > 0.0 { fps } else { 24.0 };
        clips.push(sampled_clip(&name, fps, bones, range, &rest, |b| (b < rig.len()).then_some(base + b)));
    }
    unique_names(&mut clips);
    Ok(clips)
}

/// A material named in the file, usually a picture's file name.
fn material(name: &str, textures: &mut Textures) -> Material {
    let mut m = Material { name: name.to_string(), base_color: Vec4::ONE, roughness: 0.7, ..Default::default() };
    if textures.dir().is_some() && !name.is_empty() {
        match textures.locate(name, true) {
            Some(p) => m.base_color_texture = textures.load_path(&p).map(TexRef::new),
            None => textures.warn(format!("texture not found: {name}")),
        }
    }
    m
}

#[cfg(test)]
pub mod tests {
    use super::*;

    const HEADER: usize = 124;

    /// A one-triangle IQM with two joints and a two-frame animation that
    /// moves the second joint along X.
    pub fn triangle_iqm() -> Vec<u8> {
        let mut text = b"\0tri\0skin.png\0root\0arm\0wave\0".to_vec();
        while !text.len().is_multiple_of(4) {
            text.push(0);
        }
        let f = |v: &[f32]| v.iter().flat_map(|x| x.to_le_bytes()).collect::<Vec<u8>>();
        let u = |v: &[u32]| v.iter().flat_map(|x| x.to_le_bytes()).collect::<Vec<u8>>();
        let mut body = Vec::new();
        let at = |part: Vec<u8>, body: &mut Vec<u8>| {
            let ofs = HEADER + body.len();
            body.extend(part);
            ofs as u32
        };
        let ofs_text = at(text.clone(), &mut body);
        let ofs_pos = at(f(&[0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0]), &mut body);
        let ofs_bi = at(vec![0, 0, 0, 0, 1, 0, 0, 0, 1, 0, 0, 0], &mut body);
        let ofs_bw = at(vec![255, 0, 0, 0, 255, 0, 0, 0, 128, 127, 0, 0], &mut body);
        let ofs_va = at(u(&[0, 0, 7, 3, ofs_pos, 4, 0, 1, 4, ofs_bi, 5, 0, 1, 4, ofs_bw]), &mut body);
        let ofs_tri = at(u(&[0, 1, 2]), &mut body);
        let ofs_mesh = at(u(&[1, 5, 0, 3, 0, 1]), &mut body);
        let mut joints = u(&[14, u32::MAX]);
        joints.extend(f(&[0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 1.0, 1.0, 1.0]));
        joints.extend(u(&[19, 0]));
        joints.extend(f(&[0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 1.0, 1.0, 1.0]));
        let ofs_joints = at(joints, &mut body);
        let mut poses = u(&[u32::MAX, 0]);
        poses.extend(f(&[0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 1.0, 1.0, 1.0]));
        poses.extend(f(&[0.0; 10]));
        poses.extend(u(&[0, 1]));
        poses.extend(f(&[0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 1.0, 1.0, 1.0]));
        poses.extend(f(&[0.5, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0]));
        let ofs_poses = at(poses, &mut body);
        let ofs_anims = at([u(&[23, 0, 2]), f(&[10.0]), u(&[0])].concat(), &mut body);
        let ofs_frames = at([0u16, 2].iter().flat_map(|v| v.to_le_bytes()).collect(), &mut body);
        let mut out = b"INTERQUAKEMODEL\0".to_vec();
        let total = (HEADER + body.len()) as u32;
        out.extend(u(&[2, total, 0, text.len() as u32, ofs_text, 1, ofs_mesh, 3, 3, ofs_va, 1, ofs_tri, 0]));
        out.extend(u(&[2, ofs_joints, 2, ofs_poses, 1, ofs_anims, 2, 1, ofs_frames, 0, 0, 0, 0, 0]));
        assert_eq!(out.len(), HEADER);
        out.extend(body);
        out
    }

    #[test]
    fn triangle_with_animation() {
        let limits = Limits { neighbours: false, ..Limits::viewer() };
        let scene = load(Path::new("t.iqm"), &triangle_iqm(), &limits).unwrap();
        let p = &scene.meshes[0].primitives[0];
        assert_eq!(p.positions.len(), 3);
        // Blender's Z up is Slerp's Y up.
        assert!((p.positions[2] - Vec3::new(0.0, 1.0, 0.0)).length() < 1e-6);
        assert_eq!(p.joints[1][0], 1);
        assert!((p.weights[2].x - 128.0 / 255.0).abs() < 1e-3);
        let clip = &scene.animations[0];
        assert_eq!(clip.name, "wave");
        assert!((clip.duration - 0.1).abs() < 1e-6);
        let ch = &clip.channels[0];
        assert_eq!(ch.node, 1);
        // Frame 1 moves the arm by 2 * 0.5 along X; its rest is 1 up.
        assert_eq!(ch.values[3..6], [1.0, 1.0, 0.0]);
        assert_eq!(scene.materials[0].name, "skin.png");
    }

    #[test]
    fn half_floats() {
        assert_eq!(half(0x3c00), 1.0);
        assert_eq!(half(0xc000), -2.0);
        assert_eq!(half(0), 0.0);
    }
}
