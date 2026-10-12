//! 3D Studio `.3ds` files (3D Studio for DOS and 3ds Max's legacy export):
//! a tree of chunks, each a 16-bit id and a 32-bit length.
//!
//! The editor section holds materials and named objects.  A triangle
//! object's vertices are stored in world space, together with the object's
//! local frame (`MESH_MATRIX`), per-face smoothing groups and the faces each
//! material covers.  The keyframer section, when present, holds the object
//! hierarchy, each node's pivot and its position, rotation and scale tracks.
//! As in lib3ds, a node's mesh is the world-space mesh taken back through
//! the object's frame and pivot, and the node's transform comes from its
//! tracks at the first frame; the tracks also become an animation clip.
//! Without a keyframer every object is a root node in world space.
//!
//! 3DS is Z-up with no stated unit, so the scene is rotated to Y-up and left
//! at 1 unit = 1 m.  Colours are taken as display (sRGB) values: 3D Studio
//! predates linear workflows.

use crate::common::{Reader, TextureSet, check_triangles, flipped_uv_transform, looks_like_normal_map, srgb_color};
use crate::polymesh::{Attr, Binding, PolyMesh, Smoothing};
use glam::{Mat3, Mat4, Quat, Vec2, Vec3, Vec4};
use slerp_scene::{
    AlphaMode, Animation, Channel, Interpolation, Limits, LoadError, Material, Mesh, Node, Property, Scene, TexRef,
    Transform, Wrap,
};
use std::collections::HashMap;
use std::path::Path;

const MAIN: u16 = 0x4D4D;
const VERSION: u16 = 0x0002;
const EDITOR: u16 = 0x3D3D;
const KEYFRAMER: u16 = 0xB000;

/// Keyframer tracks are capped at this many keys.
const MAX_KEYS: usize = 100_000;
/// 3ds Max's default frame rate; the file doesn't store one.
const FPS: f32 = 30.0;

/// True when `head` starts with the main chunk.  Without the `.3ds`
/// extension the first child must be the version chunk too, since a 0x4D4D
/// start alone is common ("MM" begins big-endian TIFFs).
pub fn looks_like_3ds(head: &[u8], by_extension: bool) -> bool {
    if head.len() < 16 || head[0..2] != MAIN.to_le_bytes() {
        return false;
    }
    let len = u32::from_le_bytes([head[2], head[3], head[4], head[5]]);
    if len < 6 {
        return false;
    }
    let first = u16::from_le_bytes([head[6], head[7]]);
    let first_len = u32::from_le_bytes([head[8], head[9], head[10], head[11]]);
    by_extension || (first == VERSION && first_len == 10) || (first == EDITOR && first_len as u64 + 6 <= len as u64)
}

/*┌─────────────────╮
| Chunks          |
└─────────────────╯*/
//region Chunks

/// One chunk's id and body.  A chunk whose length runs past its parent is
/// cut at the parent's end, so truncated files keep what they hold.
#[derive(Clone, Copy)]
struct Chunk<'a> {
    id: u16,
    body: &'a [u8],
}

fn chunks(data: &[u8]) -> impl Iterator<Item = Chunk<'_>> {
    let mut at = 0usize;
    std::iter::from_fn(move || {
        let head = data.get(at..at.checked_add(6)?)?;
        let id = u16::from_le_bytes([head[0], head[1]]);
        let len = u32::from_le_bytes([head[2], head[3], head[4], head[5]]) as usize;
        if len < 6 {
            return None;
        }
        let end = at.saturating_add(len).min(data.len());
        let body = &data[at + 6..end];
        at = end;
        Some(Chunk { id, body })
    })
}

/// A zero-terminated name, read as Latin-1, and the bytes after it.
fn cstring(b: &[u8]) -> (String, &[u8]) {
    let end = b.iter().position(|&c| c == 0).unwrap_or(b.len());
    let name: String = b[..end].iter().map(|&c| c as char).collect();
    (name, b.get(end + 1..).unwrap_or_default())
}

/// A colour chunk's value: 8-bit or float, preferring the gamma-corrected
/// variant as lib3ds does.
fn color(body: &[u8]) -> Option<Vec3> {
    let mut plain = None;
    let mut corrected = None;
    for c in chunks(body) {
        let mut r = Reader::new(c.body);
        let v = match c.id {
            0x0010 | 0x0013 => r.f32().zip(r.f32()).zip(r.f32()).map(|((a, b), c)| Vec3::new(a, b, c)),
            0x0011 | 0x0012 => r.bytes(3).map(|b| Vec3::new(b[0] as f32, b[1] as f32, b[2] as f32) / 255.0),
            _ => None,
        };
        match c.id {
            0x0012 | 0x0013 => corrected = corrected.or(v),
            _ => plain = plain.or(v),
        }
    }
    corrected.or(plain).filter(|v| v.is_finite())
}

/// A percentage chunk as 0-1: an integer percent or a float fraction.
fn percent(body: &[u8]) -> Option<f32> {
    chunks(body)
        .find_map(|c| {
            let mut r = Reader::new(c.body);
            match c.id {
                0x0030 => r.u16().map(|v| v as i16 as f32 / 100.0),
                0x0031 => r.f32(),
                _ => None,
            }
        })
        .filter(|v| v.is_finite())
}

//endregion

/*┌─────────────────╮
| Editor data     |
└─────────────────╯*/
//region Editor data

#[derive(Default)]
struct Map {
    file: String,
    strength: f32,
    tiling: u16,
    scale: Vec2,
    offset: Vec2,
    rotation: f32,
}

fn read_map(body: &[u8]) -> Map {
    let mut m = Map { strength: 1.0, scale: Vec2::ONE, ..Default::default() };
    for c in chunks(body) {
        let mut r = Reader::new(c.body);
        match c.id {
            0xA300 => m.file = cstring(c.body).0,
            0xA351 => m.tiling = r.u16().unwrap_or(0),
            0xA354 => m.scale.x = r.f32().filter(|v| v.is_finite() && *v != 0.0).unwrap_or(1.0),
            0xA356 => m.scale.y = r.f32().filter(|v| v.is_finite() && *v != 0.0).unwrap_or(1.0),
            0xA358 => m.offset.x = r.f32().filter(|v| v.is_finite()).unwrap_or(0.0),
            0xA35A => m.offset.y = r.f32().filter(|v| v.is_finite()).unwrap_or(0.0),
            0xA35C => m.rotation = r.f32().filter(|v| v.is_finite()).unwrap_or(0.0),
            _ => {}
        }
    }
    if let Some(p) = percent(body) {
        m.strength = p;
    }
    m
}

#[derive(Default)]
struct MaterialData {
    name: String,
    diffuse: Option<Vec3>,
    shininess: Option<f32>,
    shine_strength: Option<f32>,
    transparency: f32,
    self_illum: f32,
    two_sided: bool,
    texture: Option<Map>,
    opacity: Option<Map>,
    bump: Option<Map>,
}

fn read_material(body: &[u8]) -> MaterialData {
    let mut m = MaterialData::default();
    for c in chunks(body) {
        match c.id {
            0xA000 => m.name = cstring(c.body).0,
            0xA020 => m.diffuse = color(c.body),
            0xA040 => m.shininess = percent(c.body),
            0xA041 => m.shine_strength = percent(c.body),
            0xA050 => m.transparency = percent(c.body).unwrap_or(0.0).clamp(0.0, 1.0),
            0xA084 => m.self_illum = percent(c.body).unwrap_or(0.0).clamp(0.0, 1.0),
            0xA081 => m.two_sided = true,
            0xA200 => m.texture = Some(read_map(c.body)),
            0xA210 => m.opacity = Some(read_map(c.body)),
            0xA230 => m.bump = Some(read_map(c.body)),
            _ => {}
        }
    }
    m
}

/// A triangle object as the editor stores it.
#[derive(Default)]
struct ObjectData {
    name: String,
    hidden: bool,
    positions: Vec<Vec3>,
    uvs: Vec<Vec2>,
    faces: Vec<[u16; 3]>,
    smoothing: Option<Vec<u32>>,
    /// Material names and the faces they cover.
    groups: Vec<(String, Vec<u16>)>,
    /// The object's local frame, when the file gives one.
    matrix: Option<Mat4>,
}

fn read_trimesh(body: &[u8], obj: &mut ObjectData) {
    for c in chunks(body) {
        let mut r = Reader::new(c.body);
        match c.id {
            0x4110 => {
                let n = (r.u16().unwrap_or(0) as usize).min(r.remaining() / 12);
                obj.positions = (0..n)
                    .map(|_| Vec3::new(r.f32().unwrap_or(0.0), r.f32().unwrap_or(0.0), r.f32().unwrap_or(0.0)))
                    .collect();
            }
            0x4140 => {
                let n = (r.u16().unwrap_or(0) as usize).min(r.remaining() / 8);
                obj.uvs = (0..n).map(|_| Vec2::new(r.f32().unwrap_or(0.0), 1.0 - r.f32().unwrap_or(0.0))).collect();
            }
            0x4160 => {
                let v: Vec<f32> = (0..12).map_while(|_| r.f32()).collect();
                if v.len() == 12 && v.iter().all(|x| x.is_finite()) {
                    obj.matrix = Some(Mat4::from_cols(
                        Vec4::new(v[0], v[1], v[2], 0.0),
                        Vec4::new(v[3], v[4], v[5], 0.0),
                        Vec4::new(v[6], v[7], v[8], 0.0),
                        Vec4::new(v[9], v[10], v[11], 1.0),
                    ));
                }
            }
            0x4120 => {
                let n = (r.u16().unwrap_or(0) as usize).min(r.remaining() / 8);
                obj.faces = (0..n)
                    .map(|_| {
                        let f = [r.u16().unwrap_or(0), r.u16().unwrap_or(0), r.u16().unwrap_or(0)];
                        r.u16();
                        f
                    })
                    .collect();
                for sub in chunks(&c.body[r.pos.min(c.body.len())..]) {
                    let mut s = Reader::new(sub.body);
                    match sub.id {
                        0x4130 => {
                            let (name, rest) = cstring(sub.body);
                            let mut s = Reader::new(rest);
                            let count = (s.u16().unwrap_or(0) as usize).min(s.remaining() / 2);
                            let faces = (0..count).filter_map(|_| s.u16()).collect();
                            obj.groups.push((name, faces));
                        }
                        0x4150 => {
                            let count = n.min(s.remaining() / 4);
                            obj.smoothing = Some((0..count).filter_map(|_| s.u32()).collect());
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
}

//endregion

/*┌─────────────────╮
| Keyframer       |
└─────────────────╯*/
//region Keyframer

#[derive(Default)]
struct KfNode {
    id: u16,
    name: String,
    instance: String,
    parent: Option<u16>,
    pivot: Vec3,
    is_object: bool,
    positions: Vec<(f32, Vec3)>,
    rotations: Vec<(f32, Quat)>,
    scales: Vec<(f32, Vec3)>,
}

/// Reads a track's keys, skipping each key's spline parameters.
fn track<T>(body: &[u8], value: impl Fn(&mut Reader) -> Option<T>) -> Vec<(f32, T)> {
    let mut r = Reader::new(body);
    if r.bytes(10).is_none() {
        return Vec::new();
    }
    let count = (r.u32().unwrap_or(0) as usize).min(MAX_KEYS).min(r.remaining() / 10);
    let mut keys = Vec::with_capacity(count);
    for _ in 0..count {
        let (Some(frame), Some(flags)) = (r.u32(), r.u16()) else { break };
        for bit in 0..5 {
            if flags & (1 << bit) != 0 {
                r.f32();
            }
        }
        let Some(v) = value(&mut r) else { break };
        keys.push((frame as i32 as f32, v));
    }
    keys
}

fn vec3(r: &mut Reader) -> Option<Vec3> {
    let v = Vec3::new(r.f32()?, r.f32()?, r.f32()?);
    v.is_finite().then_some(v)
}

fn read_node(body: &[u8], default_id: u16) -> KfNode {
    let mut n = KfNode { id: default_id, ..Default::default() };
    for c in chunks(body) {
        let mut r = Reader::new(c.body);
        match c.id {
            0xB030 => n.id = r.u16().unwrap_or(default_id),
            0xB010 => {
                let (name, rest) = cstring(c.body);
                n.name = name;
                let mut r = Reader::new(rest);
                r.u16();
                r.u16();
                n.parent = r.u16().filter(|&p| p != 0xFFFF);
            }
            0xB011 => n.instance = cstring(c.body).0,
            0xB013 => n.pivot = vec3(&mut r).unwrap_or(Vec3::ZERO),
            0xB020 => n.positions = track(c.body, vec3),
            0xB022 => n.scales = track(c.body, vec3),
            0xB021 => {
                // Each key turns by an angle about an axis, relative to the
                // key before, with lib3ds's sign.
                let keys = track(c.body, |r| {
                    let angle = r.f32()?;
                    let axis = vec3(r)?;
                    angle.is_finite().then_some((angle, axis))
                });
                let mut q = Quat::IDENTITY;
                n.rotations = keys
                    .into_iter()
                    .enumerate()
                    .map(|(i, (t, (angle, axis)))| {
                        let delta = axis.try_normalize().map_or(Quat::IDENTITY, |a| Quat::from_axis_angle(a, -angle));
                        q = if i == 0 { delta } else { (q * delta).normalize() };
                        (t, q)
                    })
                    .collect();
            }
            _ => {}
        }
    }
    n
}

/// The node's transform at its first keys.
fn rest_transform(n: &KfNode) -> Transform {
    Transform {
        translation: n.positions.first().map_or(Vec3::ZERO, |k| k.1),
        rotation: n.rotations.first().map_or(Quat::IDENTITY, |k| k.1),
        scale: n.scales.first().map_or(Vec3::ONE, |k| k.1),
    }
}

//endregion

/*┌─────────────────╮
| Scene           |
└─────────────────╯*/
//region Scene

struct Builder<'a> {
    materials: Vec<MaterialData>,
    by_name: HashMap<String, usize>,
    /// Scene material per (3DS material, double-sided).
    made: HashMap<usize, usize>,
    textures: TextureSet<'a>,
    scene: Scene,
}

impl Builder<'_> {
    fn material(&mut self, name: &str) -> Option<usize> {
        let index = *self.by_name.get(name)?;
        if let Some(&m) = self.made.get(&index) {
            return Some(m);
        }
        let d = &self.materials[index];
        let mut m = Material { name: d.name.clone(), double_sided: d.two_sided, ..Material::default() };
        let diffuse = d.diffuse.unwrap_or(Vec3::splat(0.8));
        let alpha = 1.0 - d.transparency;
        m.base_color = srgb_color(diffuse.x, diffuse.y, diffuse.z, alpha);
        if let Some(map) = d.texture.as_ref().filter(|m| !m.file.is_empty()) {
            let file = map.file.clone();
            let (tiling, scale, offset, rotation) = (map.tiling, map.scale, map.offset, map.rotation);
            if let Some(t) = self.textures.file(&file) {
                let mut tex = TexRef::new(t);
                let wrap = if tiling & 0x10 != 0 {
                    Wrap::ClampToEdge
                } else if tiling & 0x2 != 0 {
                    Wrap::MirroredRepeat
                } else {
                    Wrap::Repeat
                };
                tex.sampler.wrap_u = wrap;
                tex.sampler.wrap_v = wrap;
                let uv =
                    Mat3::from_translation(offset) * Mat3::from_angle(rotation.to_radians()) * Mat3::from_scale(scale);
                tex.transform = flipped_uv_transform(uv);
                m.base_color = Vec4::new(1.0, 1.0, 1.0, alpha);
                m.base_color_texture = Some(tex);
            }
        }
        let d = &self.materials[index];
        if let (Some(t), Some(file)) = (m.base_color_texture, d.opacity.as_ref().map(|o| o.file.clone()))
            && let Some(mask) = self.textures.file(&file)
            && let Some(merged) = self.textures.merge_alpha(t.texture, mask)
        {
            m.base_color_texture = Some(TexRef { texture: merged, ..t });
            m.alpha_mode = AlphaMode::Blend;
        }
        let d = &self.materials[index];
        if let Some(file) = d.bump.as_ref().map(|b| b.file.clone()).filter(|f| !f.is_empty())
            && let Some(t) = self.textures.file_quiet(&file)
            && self.textures.get(t).is_some_and(looks_like_normal_map)
        {
            m.normal_texture = Some(TexRef::new(t));
        }
        let d = &self.materials[index];
        if alpha < 0.999 {
            m.alpha_mode = AlphaMode::Blend;
        }
        // Shininess is Phong glossiness as a fraction; strength scales the
        // highlight.
        m.roughness = (1.0 - 0.8 * d.shininess.unwrap_or(0.1)).clamp(0.15, 1.0);
        m.specular = d.shine_strength.map_or(0.5, |s| s.clamp(0.0, 1.0));
        m.emissive = m.base_color.truncate() * d.self_illum;
        self.scene.materials.push(m);
        let at = self.scene.materials.len() - 1;
        self.made.insert(index, at);
        Some(at)
    }

    /// Builds an object's mesh with its vertices moved by `to_local`.
    fn mesh(&mut self, obj: &ObjectData, to_local: Mat4, warnings: &mut Vec<String>) -> Option<usize> {
        let mut pm = PolyMesh {
            positions: obj.positions.iter().map(|&p| to_local.transform_point3(p)).collect(),
            ..Default::default()
        };
        let mirrored = to_local.determinant() < 0.0;
        for f in &obj.faces {
            let f = f.map(u32::from);
            pm.add_face(&if mirrored { [f[0], f[2], f[1]] } else { f });
        }
        if !obj.uvs.is_empty() {
            pm.uvs = Some(Attr::new(obj.uvs.clone(), Binding::Vertex));
        }
        pm.smoothing = match &obj.smoothing {
            Some(groups) => Smoothing::Groups(groups.clone()),
            None => Smoothing::default(),
        };
        // Slot per face: the material group that names it last, or none.
        let mut slots = vec![u32::MAX; obj.faces.len()];
        let mut materials = Vec::new();
        for (name, faces) in &obj.groups {
            let slot = materials.len() as u32;
            materials.push(self.material(name));
            for &f in faces {
                if let Some(s) = slots.get_mut(f as usize) {
                    *s = slot;
                }
            }
        }
        pm.face_slots = slots;
        let prims = pm.build(warnings);
        if prims.is_empty() {
            return None;
        }
        let primitives = prims
            .into_iter()
            .map(|(slot, mut p)| {
                p.material = materials.get(slot as usize).copied().flatten();
                p
            })
            .collect();
        self.scene.meshes.push(Mesh { name: obj.name.clone(), primitives, target_names: Vec::new() });
        Some(self.scene.meshes.len() - 1)
    }
}

pub fn load(path: &Path, bytes: &[u8], limits: &Limits) -> Result<Scene, LoadError> {
    // 1. The main chunk and its sections.
    let main = chunks(bytes).next().filter(|c| c.id == MAIN).ok_or(LoadError::NotMine)?;
    let mut version = None;
    let mut objects: Vec<ObjectData> = Vec::new();
    let mut materials: Vec<MaterialData> = Vec::new();
    let mut nodes: Vec<KfNode> = Vec::new();
    let mut frames = None;
    for c in chunks(main.body) {
        match c.id {
            VERSION => version = Reader::new(c.body).u32(),
            EDITOR => {
                for e in chunks(c.body) {
                    match e.id {
                        0xAFFF => materials.push(read_material(e.body)),
                        0x4000 => {
                            let (name, rest) = cstring(e.body);
                            let mut obj = ObjectData { name, ..Default::default() };
                            for sub in chunks(rest) {
                                match sub.id {
                                    0x4010 => obj.hidden = true,
                                    0x4100 => read_trimesh(sub.body, &mut obj),
                                    _ => {}
                                }
                            }
                            if !obj.faces.is_empty() {
                                objects.push(obj);
                            }
                        }
                        _ => {}
                    }
                }
            }
            KEYFRAMER => {
                for k in chunks(c.body) {
                    match k.id {
                        0xB008 => {
                            let mut r = Reader::new(k.body);
                            frames = r.u32().zip(r.u32());
                        }
                        0xB001..=0xB007 => {
                            let mut n = read_node(k.body, nodes.len().min(0xFFFE) as u16);
                            n.is_object = k.id == 0xB002;
                            nodes.push(n);
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
    if objects.is_empty() {
        return Err(LoadError::invalid("3DS file holds no triangle objects"));
    }
    let triangles: u64 = objects.iter().map(|o| o.faces.len() as u64).sum();
    check_triangles(triangles, limits)?;

    let mut b = Builder {
        by_name: materials.iter().enumerate().map(|(i, m)| (m.name.clone(), i)).collect(),
        materials,
        made: HashMap::new(),
        textures: TextureSet::new(path, limits),
        scene: Scene::default(),
    };
    b.scene.info.format = match version {
        Some(v) => format!("3D Studio (version {v})"),
        None => "3D Studio".into(),
    };
    let mut warnings = Vec::new();

    // 2. Nodes from the keyframer, each placing its object's mesh through
    //    the object's frame and the node's pivot.
    let by_name: HashMap<&str, usize> = objects.iter().enumerate().map(|(i, o)| (o.name.as_str(), i)).collect();
    let mut placed = vec![false; objects.len()];
    let mut node_of: HashMap<u16, usize> = HashMap::new();
    let mut meshes: HashMap<(usize, [u32; 3]), Option<usize>> = HashMap::new();
    for n in &nodes {
        let dummy = n.name == "$$$DUMMY";
        let name = if dummy || (!n.instance.is_empty() && n.is_object) { n.instance.clone() } else { n.name.clone() };
        let mut node = Node::new(if name.is_empty() { n.name.clone() } else { name });
        node.transform = rest_transform(n);
        if n.is_object
            && !dummy
            && let Some(&oi) = by_name.get(n.name.as_str())
        {
            let obj = &objects[oi];
            let frame = obj.matrix.filter(|m| m.determinant().abs() > 1e-12).unwrap_or(Mat4::IDENTITY);
            let key = (oi, n.pivot.to_array().map(f32::to_bits));
            let mesh = match meshes.get(&key) {
                Some(&m) => m,
                None => {
                    let to_local = Mat4::from_translation(-n.pivot) * frame.inverse();
                    let m = b.mesh(obj, to_local, &mut warnings);
                    meshes.insert(key, m);
                    m
                }
            };
            node.mesh = mesh;
            node.visible = !obj.hidden;
            placed[oi] = true;
        }
        b.scene.nodes.push(node);
        node_of.entry(n.id).or_insert(b.scene.nodes.len() - 1);
    }
    let parents: Vec<Option<usize>> = nodes.iter().map(|n| n.parent.and_then(|p| node_of.get(&p).copied())).collect();
    for (child, parent) in parents.iter().enumerate() {
        if let Some(p) = *parent
            && !is_ancestor(&parents, child, p)
        {
            b.scene.nodes[p].children.push(child);
        }
    }

    // 3. Objects no node shows sit at the root in world space.
    for (oi, obj) in objects.iter().enumerate() {
        if placed[oi] {
            continue;
        }
        if let Some(m) = b.mesh(obj, Mat4::IDENTITY, &mut warnings) {
            let mut node = Node::new(obj.name.clone());
            node.mesh = Some(m);
            node.visible = !obj.hidden;
            b.scene.nodes.push(node);
        }
    }
    b.scene.compute_roots();
    if b.scene.meshes.is_empty() {
        return Err(LoadError::invalid("3DS objects hold no usable faces"));
    }

    // 4. The keyframer's tracks as one clip.
    if limits.animations && nodes.iter().any(|n| n.positions.len() > 1 || n.rotations.len() > 1 || n.scales.len() > 1) {
        let start = frames.map_or(0.0, |f| f.0 as f32);
        let mut anim = Animation { name: "Keyframer".into(), ..Default::default() };
        let time = |frame: f32| ((frame - start) / FPS).max(0.0);
        for (i, n) in nodes.iter().enumerate() {
            let mut add = |property, keys: Vec<(f32, Vec<f32>)>| {
                if keys.len() > 1 {
                    anim.channels.push(Channel {
                        node: i,
                        property,
                        interpolation: Interpolation::Linear,
                        times: keys.iter().map(|k| time(k.0)).collect(),
                        values: keys.into_iter().flat_map(|k| k.1).collect(),
                    });
                }
            };
            add(Property::Translation, n.positions.iter().map(|k| (k.0, k.1.to_array().to_vec())).collect());
            add(Property::Rotation, n.rotations.iter().map(|k| (k.0, k.1.to_array().to_vec())).collect());
            add(Property::Scale, n.scales.iter().map(|k| (k.0, k.1.to_array().to_vec())).collect());
        }
        // Times must rise; files with keys out of order lose those channels.
        anim.channels.retain(|c| c.times.windows(2).all(|w| w[1] > w[0]));
        anim.duration = anim.channels.iter().filter_map(|c| c.times.last().copied()).fold(0.0, f32::max);
        if !anim.channels.is_empty() {
            b.scene.animations.push(anim);
        }
    }

    b.scene.convert_z_up();
    b.scene.info.details.push(("Units".into(), "unknown (1 unit = 1 m)".into()));
    if !nodes.is_empty() {
        b.scene.info.details.push(("Keyframer nodes".into(), nodes.len().to_string()));
    }
    let mut scene = b.scene;
    scene.info.warnings = warnings;
    scene.info.warnings.extend(b.textures.warnings);
    scene.textures = b.textures.textures;
    Ok(scene)
}

/// True when `node` is `of` or one of its ancestors, which would make a
/// cycle if `node` became a child of `of`.
fn is_ancestor(parents: &[Option<usize>], node: usize, of: usize) -> bool {
    let mut at = Some(of);
    for _ in 0..=parents.len() {
        match at {
            Some(a) if a == node => return true,
            Some(a) => at = parents[a],
            None => return false,
        }
    }
    true
}

//endregion

#[cfg(test)]
mod tests {
    use super::*;

    fn chunk(id: u16, body: &[u8]) -> Vec<u8> {
        let mut out = id.to_le_bytes().to_vec();
        out.extend_from_slice(&(body.len() as u32 + 6).to_le_bytes());
        out.extend_from_slice(body);
        out
    }

    fn floats(v: &[f32]) -> Vec<u8> {
        v.iter().flat_map(|f| f.to_le_bytes()).collect()
    }

    /// A unit quad in the XY plane at height z = 5, red, with a keyframer
    /// node that has a pivot.
    pub(super) fn quad() -> Vec<u8> {
        let mut points = 4u16.to_le_bytes().to_vec();
        points.extend(floats(&[0., 0., 5., 1., 0., 5., 1., 1., 5., 0., 1., 5.]));
        let mut faces = 2u16.to_le_bytes().to_vec();
        for f in [[0u16, 1, 2, 7], [0, 2, 3, 7]] {
            faces.extend(f.iter().flat_map(|v| v.to_le_bytes()));
        }
        let mut group = b"Red\0".to_vec();
        group.extend([2u8, 0, 0, 0, 1, 0]);
        faces.extend(chunk(0x4130, &group));
        faces.extend(chunk(0x4150, &[1, 0, 0, 0, 1, 0, 0, 0]));
        let mut matrix = floats(&[1., 0., 0., 0., 1., 0., 0., 0., 1., 0., 0., 5.]);
        matrix = chunk(0x4160, &matrix);
        let mut mesh = chunk(0x4110, &points);
        mesh.extend(matrix);
        mesh.extend(chunk(0x4120, &faces));
        let mut object = b"Quad\0".to_vec();
        object.extend(chunk(0x4100, &mesh));
        let mut material = chunk(0xA000, b"Red\0");
        material.extend(chunk(0xA020, &chunk(0x0011, &[255, 0, 0])));
        let mut editor = chunk(0xAFFF, &material);
        editor.extend(chunk(0x4000, &object));

        // Keyframer: the node sits at (0, 0, 5) with its pivot at the
        // quad's centre, and moves along X over 30 frames.
        let mut hdr = b"Quad\0".to_vec();
        hdr.extend([0, 0, 0, 0, 0xFF, 0xFF]);
        let mut pos = vec![0u8; 10];
        pos.extend(2u32.to_le_bytes());
        for (frame, x) in [(0u32, 0.5f32), (30, 1.5)] {
            pos.extend(frame.to_le_bytes());
            pos.extend([0, 0]);
            pos.extend(floats(&[x, 0.5, 5.0]));
        }
        let mut node = chunk(0xB030, &0u16.to_le_bytes());
        node.extend(chunk(0xB010, &hdr));
        node.extend(chunk(0xB013, &floats(&[0.5, 0.5, 0.0])));
        node.extend(chunk(0xB020, &pos));
        let kf = chunk(0xB002, &node);

        let mut main = chunk(VERSION, &3u32.to_le_bytes());
        main.extend(chunk(EDITOR, &editor));
        main.extend(chunk(KEYFRAMER, &kf));
        chunk(MAIN, &main)
    }

    #[test]
    fn loads_quad_with_keyframer_pivot() {
        let bytes = quad();
        assert!(looks_like_3ds(&bytes, false));
        let s = load(Path::new("/nonexistent/q.3ds"), &bytes, &Limits::viewer()).unwrap();
        assert_eq!(s.triangle_count(), 2);
        assert_eq!(s.materials[0].name, "Red");
        assert_eq!(s.materials[0].base_color, Vec4::new(1.0, 0.0, 0.0, 1.0));
        // At frame 0 the quad is where the editor put it: 5 units up
        // (along Z, so +Y after conversion).
        let pose = slerp_scene::Pose::rest(&s);
        let bounds = s.bounds(&pose);
        assert!((bounds.min - Vec3::new(0.0, 5.0, -1.0)).length() < 1e-4, "{bounds:?}");
        assert!((bounds.max - Vec3::new(1.0, 5.0, 0.0)).length() < 1e-4, "{bounds:?}");
        // The clip moves it one unit along X over a second.
        let anim = &s.animations[0];
        assert!((anim.duration - 1.0).abs() < 1e-5);
        let mut pose = slerp_scene::Pose::rest(&s);
        pose.sample(&s, 0, 1.0);
        pose.update_world(&s);
        assert!((s.bounds(&pose).min.x - 1.0).abs() < 1e-4);
    }

    #[test]
    fn survives_truncation_and_garbage() {
        let bytes = quad();
        for cut in 0..bytes.len() {
            let _ = load(Path::new("q.3ds"), &bytes[..cut], &Limits::viewer());
        }
        for i in 0..bytes.len() {
            let mut b = bytes.clone();
            b[i] = 0xFF;
            let _ = load(Path::new("q.3ds"), &b, &Limits::viewer());
        }
        let limits = Limits { max_triangles: 1, ..Limits::viewer() };
        assert!(matches!(load(Path::new("q.3ds"), &bytes, &limits), Err(LoadError::TooLarge(_))));
    }
}
