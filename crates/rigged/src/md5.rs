//! Doom 3's MD5 models (`.md5mesh`) and animations (`.md5anim`), both
//! text.  A mesh stores its joints in model space and each vertex as
//! weighted offsets from joints, so the bind-pose position is the weights'
//! sum; an animation stores a base frame and, per frame, the components
//! that change.  Quaternions store x, y and z, with w = -sqrt(1 - x² - y² -
//! z²), as id's code computes it.
//!
//! Materials are named by shader paths (`models/monsters/imp/imp`); the
//! picture is looked for under that path from the model's folder and each
//! folder above it (a game's `base/` holds both `models/md5/` and the
//! pictures), then by the path's last part beside the model, with the
//! `_d`, `_local` and `_s` pictures Doom 3 uses for colour, normals and
//! specular.  Animations beside the mesh whose joints match play as clips.

use crate::basis::Basis;
use crate::read::{Result, float, int};
use crate::skeleton::{Bone, Budget, Rig, sampled_clip, strongest_four, unique_names};
use crate::textures::{Textures, read_limited, siblings};
use slerp_scene::glam::{Quat, Vec2, Vec3};
use slerp_scene::{Limits, LoadError, Material, Mesh, Node, Primitive, Scene, TexRef, Transform};
use std::path::Path;

/// Files read as animations beside a mesh, and their total size.
const MAX_ANIM_FILES: usize = 64;
const MAX_ANIM_BYTES: u64 = 512 << 20;

/// Whether text is an MD5 file: `Some(true)` for an animation,
/// `Some(false)` for a mesh.
pub fn kind(text: &str) -> Option<bool> {
    let mut tokens = Tokens::new(text);
    if tokens.next()? != "MD5Version" {
        return None;
    }
    int(tokens.next()?)?;
    for t in tokens.take(64) {
        match t {
            "numFrames" | "frameRate" | "hierarchy" | "baseframe" => return Some(true),
            "numMeshes" | "joints" | "mesh" => return Some(false),
            _ => {}
        }
    }
    None
}

/*┌─────────────────╮
| Tokens          |
└─────────────────╯*/
//region Tokens

/// MD5's tokens: words, quoted strings (without the quotes), and the
/// brackets `( ) { }` on their own, with `//` comments skipped.
struct Tokens<'a> {
    s: &'a str,
    pos: usize,
}

impl<'a> Tokens<'a> {
    fn new(s: &'a str) -> Self {
        Tokens { s, pos: 0 }
    }

    fn float(&mut self, what: &str) -> Result<f32> {
        self.next().and_then(float).ok_or_else(|| bad(what))
    }

    fn int(&mut self, what: &str) -> Result<i64> {
        self.next().and_then(int).ok_or_else(|| bad(what))
    }

    fn expect(&mut self, token: &str) -> Result<()> {
        match self.next() {
            Some(t) if t == token => Ok(()),
            _ => Err(bad(&format!("\"{token}\""))),
        }
    }

    /// `( x y z )`.
    fn vec3(&mut self, what: &str) -> Result<Vec3> {
        self.expect("(")?;
        let v = Vec3::new(self.float(what)?, self.float(what)?, self.float(what)?);
        self.expect(")")?;
        Ok(v)
    }

    fn vec2(&mut self, what: &str) -> Result<Vec2> {
        self.expect("(")?;
        let v = Vec2::new(self.float(what)?, self.float(what)?);
        self.expect(")")?;
        Ok(v)
    }
}

impl<'a> Iterator for Tokens<'a> {
    type Item = &'a str;

    fn next(&mut self) -> Option<&'a str> {
        let b = self.s.as_bytes();
        loop {
            while self.pos < b.len() && b[self.pos].is_ascii_whitespace() {
                self.pos += 1;
            }
            if b[self.pos..].starts_with(b"//") {
                while self.pos < b.len() && b[self.pos] != b'\n' {
                    self.pos += 1;
                }
                continue;
            }
            break;
        }
        let start = self.pos;
        match *b.get(start)? {
            b'(' | b')' | b'{' | b'}' => {
                self.pos += 1;
                Some(&self.s[start..start + 1])
            }
            b'"' => {
                let end = self.s[start + 1..].find(['"', '\n']).map_or(b.len(), |e| start + 1 + e);
                self.pos = (end + 1).min(b.len());
                Some(&self.s[start + 1..end])
            }
            _ => {
                let mut end = start;
                while end < b.len() && !b[end].is_ascii_whitespace() && !b"(){}\"".contains(&b[end]) {
                    end += 1;
                }
                self.pos = end;
                // Stops on a multi-byte character boundary only at ASCII
                // bytes, so the slice is valid.
                Some(&self.s[start..end])
            }
        }
    }
}

fn bad(what: &str) -> LoadError {
    LoadError::invalid(format!("MD5: expected {what}"))
}

/// A declared count, bounded by what the text could hold (`per` bytes per
/// item at least) and by the budget.
fn count(n: i64, text: &str, per: usize, size: usize, budget: &mut Budget, what: &str) -> Result<usize> {
    let n = usize::try_from(n).map_err(|_| LoadError::invalid(format!("MD5: negative {what} count")))?;
    if n > text.len() / per + 1 {
        return Err(LoadError::invalid(format!("MD5: {n} {what} can't fit in the file")));
    }
    budget.take(n, size, what)?;
    Ok(n)
}

/// id's quaternion from its stored x, y and z.
fn quat_from_xyz(v: Vec3) -> Quat {
    let t = 1.0 - v.length_squared();
    let w = if t > 0.0 { -t.sqrt() } else { 0.0 };
    let q = Quat::from_xyzw(v.x, v.y, v.z, w);
    if q.length_squared() > 1e-12 { q.normalize() } else { Quat::IDENTITY }
}

//endregion

/*┌─────────────────╮
| Meshes          |
└─────────────────╯*/
//region Meshes

struct Joint {
    name: String,
    parent: i64,
    pos: Vec3,
    orient: Quat,
}

struct Md5Mesh {
    shader: String,
    /// Texture coordinates, first weight, weight count.
    verts: Vec<(Vec2, usize, usize)>,
    tris: Vec<[u32; 3]>,
    /// Joint, bias, offset in the joint's space.
    weights: Vec<(usize, f32, Vec3)>,
}

fn parse_mesh(text: &str, budget: &mut Budget) -> Result<(Vec<Joint>, Vec<Md5Mesh>)> {
    let mut t = Tokens::new(text);
    let mut joints = Vec::new();
    let mut meshes = Vec::new();
    while let Some(word) = t.next() {
        match word {
            "MD5Version" => {
                let v = t.int("a version")?;
                if v != 10 {
                    return Err(LoadError::Unsupported(format!("MD5 version {v} (only 10 is supported)")));
                }
            }
            "numJoints" => {
                let n = t.int("a joint count")?;
                joints.reserve(count(n, text, 16, 64, budget, "joints")?);
            }
            "joints" => {
                t.expect("{")?;
                loop {
                    let name = match t.next() {
                        Some("}") => break,
                        Some(n) => n.to_string(),
                        None => return Err(bad("\"}\"")),
                    };
                    let parent = t.int("a parent")?;
                    let pos = t.vec3("a joint position")?;
                    let orient = quat_from_xyz(t.vec3("a joint orientation")?);
                    if joints.len() >= crate::skeleton::MAX_BONES {
                        return Err(LoadError::TooLarge("too many joints".into()));
                    }
                    joints.push(Joint { name, parent, pos, orient });
                }
            }
            "mesh" => meshes.push(parse_one_mesh(&mut t, text, budget)?),
            _ => {}
        }
    }
    Ok((joints, meshes))
}

fn parse_one_mesh(t: &mut Tokens, text: &str, budget: &mut Budget) -> Result<Md5Mesh> {
    t.expect("{")?;
    let mut m = Md5Mesh { shader: String::new(), verts: Vec::new(), tris: Vec::new(), weights: Vec::new() };
    loop {
        match t.next().ok_or_else(|| bad("\"}\""))? {
            "}" => break,
            "shader" => m.shader = t.next().unwrap_or("").to_string(),
            "numverts" => {
                let n = count(t.int("a vertex count")?, text, 12, 64, budget, "vertices")?;
                m.verts = vec![(Vec2::ZERO, 0, 0); n];
            }
            "vert" => {
                let i = t.int("a vertex index")?;
                let uv = t.vec2("texture coordinates")?;
                let (start, n) = (t.int("a weight index")?, t.int("a weight count")?);
                let slot =
                    usize::try_from(i).ok().and_then(|i| m.verts.get_mut(i)).ok_or_else(|| bad("a vertex index"))?;
                *slot = (uv, start.max(0) as usize, n.clamp(0, 1 << 20) as usize);
            }
            "numtris" => {
                let n = count(t.int("a triangle count")?, text, 10, 12, budget, "triangles")?;
                m.tris = vec![[0; 3]; n];
            }
            "tri" => {
                let i = t.int("a triangle index")?;
                let mut tri = [0u32; 3];
                for c in &mut tri {
                    *c = u32::try_from(t.int("a vertex index")?).map_err(|_| bad("a vertex index"))?;
                }
                let slot =
                    usize::try_from(i).ok().and_then(|i| m.tris.get_mut(i)).ok_or_else(|| bad("a triangle index"))?;
                *slot = tri;
            }
            "numweights" => {
                let n = count(t.int("a weight count")?, text, 14, 20, budget, "weights")?;
                m.weights = vec![(usize::MAX, 0.0, Vec3::ZERO); n];
            }
            "weight" => {
                let i = t.int("a weight index")?;
                let joint = t.int("a joint index")?;
                let bias = t.float("a weight bias")?;
                let pos = t.vec3("a weight position")?;
                let slot =
                    usize::try_from(i).ok().and_then(|i| m.weights.get_mut(i)).ok_or_else(|| bad("a weight index"))?;
                *slot = (usize::try_from(joint).unwrap_or(usize::MAX), bias, pos);
            }
            _ => {}
        }
    }
    Ok(m)
}

pub fn load_mesh(path: &Path, bytes: &[u8], limits: &Limits) -> Result<Scene> {
    let text = crate::read::text(bytes);
    let mut budget = Budget::new(bytes.len() as u64);
    let (joints, meshes) = parse_mesh(&text, &mut budget)?;
    if meshes.is_empty() {
        return Err(LoadError::invalid("MD5: no meshes"));
    }
    let basis = Basis::z_up_x_forward_inches();

    // 1. The skeleton, from model-space joints.
    let bones: Vec<Bone> = joints
        .iter()
        .map(|j| Bone {
            name: j.name.clone(),
            parent: usize::try_from(j.parent).ok(),
            local: Transform { translation: basis.point(j.pos), rotation: basis.rotation(j.orient), scale: Vec3::ONE },
        })
        .collect();
    let rig = Rig::from_world(bones)?;
    let mut scene = Scene::default();
    let base = rig.add_nodes(&mut scene);
    let skinned = !rig.is_empty();
    if skinned {
        scene.skins.push(rig.skin(base, "skeleton"));
    }

    // 2. Meshes: each vertex the sum of its weights, in the bind pose.
    let mut textures = Textures::new(path, limits);
    let mut triangles = 0u64;
    for (mi, m) in meshes.iter().enumerate() {
        let mut prim = Primitive::default();
        budget.take(m.verts.len(), 64, "vertices")?;
        let mut influences = Vec::new();
        for &(uv, start, n) in &m.verts {
            let mut p = Vec3::ZERO;
            influences.clear();
            for &(joint, bias, offset) in
                m.weights.get(start..start.saturating_add(n).min(m.weights.len())).unwrap_or(&[])
            {
                let Some(j) = joints.get(joint) else { continue };
                p += (j.pos + j.orient * offset) * bias;
                influences.push((joint as u32, bias));
            }
            prim.positions.push(basis.point(p));
            prim.uv0.push(uv);
            if skinned {
                let (j, w) = strongest_four(&mut influences, 0);
                prim.joints.push(j);
                prim.weights.push(w);
            }
        }
        let flip = basis.flip_winding(false);
        let vertex_count = m.verts.len() as u32;
        for tri in &m.tris {
            if tri.iter().any(|&v| v >= vertex_count) {
                continue;
            }
            prim.indices.extend(if flip { [tri[0], tri[2], tri[1]] } else { *tri });
        }
        triangles += prim.indices.len() as u64 / 3;
        if triangles > limits.max_triangles {
            return Err(LoadError::TooLarge(format!("{triangles} triangles")));
        }
        prim.ensure_normals();
        prim.material = Some(scene.materials.len());
        scene.materials.push(material(&m.shader, &mut textures));
        let name = m.shader.rsplit('/').next().filter(|s| !s.is_empty()).map_or(format!("mesh {mi}"), str::to_string);
        scene.meshes.push(Mesh { name: name.clone(), primitives: vec![prim], target_names: Vec::new() });
        let mut node = Node::new(name);
        node.mesh = Some(scene.meshes.len() - 1);
        node.skin = skinned.then_some(0);
        scene.nodes.push(node);
        scene.roots.push(scene.nodes.len() - 1);
    }
    scene.textures = std::mem::take(&mut textures.list);
    scene.info.warnings.append(&mut textures.warnings);
    scene.info.format = "Doom 3 MD5 mesh".into();
    scene.info.details.push(("Meshes".into(), meshes.len().to_string()));
    crate::describe_rig(&mut scene, &rig);

    // 3. Animations beside the mesh, when wanted.
    if limits.animations && limits.neighbours && skinned {
        add_neighbour_anims(path, &rig, base, &mut scene, &mut budget);
    }
    Ok(scene)
}

/// A material for a shader path, with its pictures when they can be found.
fn material(shader: &str, textures: &mut Textures) -> Material {
    let mut m = Material {
        name: shader.to_string(),
        base_color: slerp_scene::glam::Vec4::ONE,
        roughness: 0.7,
        ..Default::default()
    };
    let find = |textures: &mut Textures, suffixes: &[&str]| -> Option<usize> {
        let dir = textures.dir()?.to_path_buf();
        let leaf = shader.rsplit('/').next().unwrap_or(shader);
        for suffix in suffixes {
            for e in crate::textures::EXTENSIONS {
                // Under the shader's path from this folder or one above it.
                let mut up = Some(dir.as_path());
                for _ in 0..6 {
                    let Some(at) = up else { break };
                    if let Some(p) = textures.find(at, &format!("{shader}{suffix}.{e}")) {
                        return textures.load_path(&p);
                    }
                    up = at.parent();
                }
                // The last part beside the model, or in its usual folders.
                if let Some(p) = textures.locate(&format!("{leaf}{suffix}.{e}"), false) {
                    return textures.load_path(&p);
                }
            }
        }
        None
    };
    if shader.is_empty() {
        return m;
    }
    // Exporters other than id's write a picture's file name as the shader.
    let found = if textures.dir().is_some() { textures.locate(shader, true) } else { None };
    let named = found.and_then(|p| textures.load_path(&p));
    m.base_color_texture = named.or_else(|| find(textures, &["", "_d"])).map(TexRef::new);
    if m.base_color_texture.is_none() && textures.dir().is_some() {
        textures.warn(format!("texture not found: {shader}"));
    }
    m.normal_texture = find(textures, &["_local", "_n"]).map(TexRef::new);
    if find(textures, &["_s"]).is_some() {
        // A specular map would need its own channel; its presence means a
        // shinier surface.
        m.roughness = 0.5;
    }
    m
}

//endregion

/*┌─────────────────╮
| Animations      |
└─────────────────╯*/
//region Animations

struct Anim {
    fps: f32,
    /// Name, parent, flags, first component.
    hierarchy: Vec<(String, i64, u32, usize)>,
    /// Base position and orientation of each joint.
    base: Vec<(Vec3, Quat)>,
    /// Every frame's components, frame after frame.
    frames: Vec<Vec<f32>>,
}

fn parse_anim(text: &str, budget: &mut Budget) -> Result<Anim> {
    let mut t = Tokens::new(text);
    let mut a = Anim { fps: 24.0, hierarchy: Vec::new(), base: Vec::new(), frames: Vec::new() };
    let mut components = 0usize;
    while let Some(word) = t.next() {
        match word {
            "MD5Version" => {
                let v = t.int("a version")?;
                if v != 10 {
                    return Err(LoadError::Unsupported(format!("MD5 version {v} (only 10 is supported)")));
                }
            }
            "frameRate" => a.fps = t.float("a frame rate")?,
            "numFrames" => {
                let n = count(t.int("a frame count")?, text, 4, 24, budget, "frames")?;
                a.frames.reserve(n);
            }
            "numAnimatedComponents" => {
                components = count(t.int("a component count")?, text, 2, 4, budget, "components")?
            }
            "hierarchy" => {
                t.expect("{")?;
                loop {
                    let name = match t.next() {
                        Some("}") => break,
                        Some(n) => n.to_string(),
                        None => return Err(bad("\"}\"")),
                    };
                    let parent = t.int("a parent")?;
                    let flags = t.int("flags")?.clamp(0, 63) as u32;
                    let start = t.int("a component index")?.max(0) as usize;
                    if a.hierarchy.len() >= crate::skeleton::MAX_BONES {
                        return Err(LoadError::TooLarge("too many joints".into()));
                    }
                    a.hierarchy.push((name, parent, flags, start));
                }
            }
            "bounds" => {
                t.expect("{")?;
                while t.next().ok_or_else(|| bad("\"}\""))? != "}" {}
            }
            "baseframe" => {
                t.expect("{")?;
                loop {
                    match t.next() {
                        Some("}") => break,
                        Some("(") => {
                            let p = Vec3::new(t.float("a position")?, t.float("a position")?, t.float("a position")?);
                            t.expect(")")?;
                            let q = t.vec3("an orientation")?;
                            budget.take(1, 32, "base frame")?;
                            a.base.push((p, Quat::from_xyzw(q.x, q.y, q.z, 0.0)));
                        }
                        _ => return Err(bad("a base frame")),
                    }
                }
            }
            "frame" => {
                t.int("a frame number")?;
                t.expect("{")?;
                budget.take(components, 4, "frames")?;
                let mut values = Vec::with_capacity(components);
                loop {
                    match t.next() {
                        Some("}") => break,
                        Some(v) => values.push(float(v).ok_or_else(|| bad("a frame value"))?),
                        None => return Err(bad("\"}\"")),
                    }
                    if values.len() > components.max(1) * 4 + 1024 {
                        return Err(bad("a frame of the declared size"));
                    }
                }
                a.frames.push(values);
            }
            _ => {}
        }
    }
    if a.hierarchy.is_empty() {
        return Err(LoadError::invalid("MD5 animation: no joints"));
    }
    Ok(a)
}

impl Anim {
    /// Every frame's local transforms (joint after joint, frame after
    /// frame) in Slerp's coordinates.
    fn poses(&self, basis: &Basis) -> Vec<Transform> {
        let joints = self.hierarchy.len();
        let mut out = Vec::with_capacity(self.frames.len() * joints);
        for frame in &self.frames {
            for (j, &(_, _, flags, start)) in self.hierarchy.iter().enumerate() {
                let (mut p, q) = self.base.get(j).copied().unwrap_or((Vec3::ZERO, Quat::IDENTITY));
                let mut q = Vec3::new(q.x, q.y, q.z);
                let mut at = start;
                let mut take = |bit: u32, slot: &mut f32| {
                    if flags & bit != 0 {
                        if let Some(&v) = frame.get(at) {
                            *slot = v;
                        }
                        at += 1;
                    }
                };
                take(1, &mut p.x);
                take(2, &mut p.y);
                take(4, &mut p.z);
                take(8, &mut q.x);
                take(16, &mut q.y);
                take(32, &mut q.z);
                out.push(Transform {
                    translation: basis.point(p),
                    rotation: basis.rotation(quat_from_xyz(q)),
                    scale: Vec3::ONE,
                });
            }
        }
        out
    }
}

/// An animation file opened on its own: its skeleton in the base frame,
/// and its one clip.
pub fn load_anim(path: &Path, bytes: &[u8], limits: &Limits) -> Result<Scene> {
    let text = crate::read::text(bytes);
    let mut budget = Budget::new(bytes.len() as u64);
    let anim = parse_anim(&text, &mut budget)?;
    let basis = Basis::z_up_x_forward_inches();
    let rest_poses = Anim { frames: vec![Vec::new()], ..clone_shape(&anim) }.poses(&basis);
    let bones: Vec<Bone> = anim
        .hierarchy
        .iter()
        .zip(&rest_poses)
        .map(|((name, parent, _, _), &local)| Bone { name: name.clone(), parent: usize::try_from(*parent).ok(), local })
        .collect();
    let rig = Rig::from_local(bones)?;
    let mut scene = Scene::default();
    let base = rig.add_nodes(&mut scene);
    if limits.animations {
        budget.take(anim.frames.len().saturating_mul(rig.len()), 40, "poses")?;
        let poses = anim.poses(&basis);
        let name = path.file_stem().map_or("animation".into(), |s| s.to_string_lossy().into_owned());
        scene.animations.push(sampled_clip(&name, anim.fps, rig.len(), &poses, &rig.rest(), |b| Some(base + b)));
    }
    scene.info.format = "Doom 3 MD5 animation".into();
    scene.info.details.push(("Frames".into(), anim.frames.len().to_string()));
    scene.info.details.push(("Frame rate".into(), anim.fps.to_string()));
    crate::describe_rig(&mut scene, &rig);
    Ok(scene)
}

/// The animation's joints and base frame without its frames.
fn clone_shape(a: &Anim) -> Anim {
    Anim { fps: a.fps, hierarchy: a.hierarchy.clone(), base: a.base.clone(), frames: Vec::new() }
}

/// Adds the clips of the `.md5anim` files beside a mesh whose joints are
/// the mesh's, by name.
fn add_neighbour_anims(path: &Path, rig: &Rig, base: usize, scene: &mut Scene, budget: &mut Budget) {
    let Some(dir) = path.parent() else { return };
    let basis = Basis::z_up_x_forward_inches();
    let mut read = 0u64;
    let mut clips = Vec::new();
    for file in siblings(dir, &["md5anim"], MAX_ANIM_FILES) {
        let Some(bytes) = read_limited(&file, MAX_ANIM_BYTES.saturating_sub(read)) else { continue };
        read += bytes.len() as u64;
        let text = crate::read::text(&bytes);
        let mut own = Budget::new(bytes.len() as u64);
        let Ok(anim) = parse_anim(&text, &mut own) else {
            scene.info.warnings.push(format!("{} couldn't be read", file.display()));
            continue;
        };
        // Joints matched by name; an animation for another skeleton is
        // left out.
        let map: Vec<Option<usize>> = anim.hierarchy.iter().map(|(n, ..)| rig.find(n)).collect();
        let matched = map.iter().filter(|m| m.is_some()).count();
        if matched * 10 < anim.hierarchy.len() * 6 {
            continue;
        }
        if budget.take(anim.frames.len().saturating_mul(anim.hierarchy.len()), 40, "poses").is_err() {
            scene.info.warnings.push("more animations than memory allows; some were left out".into());
            break;
        }
        let poses = anim.poses(&basis);
        let rest: Vec<Transform> = map.iter().map(|m| m.map_or(Transform::IDENTITY, |b| rig.bones[b].local)).collect();
        let name = file.file_stem().map_or("animation".into(), |s| s.to_string_lossy().into_owned());
        clips.push(sampled_clip(&name, anim.fps, anim.hierarchy.len(), &poses, &rest, |j| map[j].map(|b| base + b)));
    }
    unique_names(&mut clips);
    scene.animations.extend(clips);
}

//endregion

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub const CUBE_MESH: &str = r#"MD5Version 10
commandline "test"

numJoints 2
numMeshes 1

joints {
	"root"	-1 ( 0 0 0 ) ( 0 0 0 )		//
	"arm"	0 ( 0 0 10 ) ( 0 0 0 )		// root
}

mesh {
	shader "models/test/skin"
	numverts 3
	vert 0 ( 0 0 ) 0 1
	vert 1 ( 1 0 ) 1 1
	vert 2 ( 0 1 ) 2 2
	numtris 1
	tri 0 0 1 2
	numweights 4
	weight 0 0 1 ( 0 0 0 )
	weight 1 0 1 ( 0 10 0 )
	weight 2 1 0.5 ( 0 0 0 )
	weight 3 0 0.5 ( 10 0 10 )
}
"#;

    #[test]
    fn sniffs_meshes_and_anims() {
        assert_eq!(kind(CUBE_MESH), Some(false));
        assert_eq!(kind("MD5Version 10\ncommandline \"\"\nnumFrames 2\n"), Some(true));
        assert_eq!(kind("MD5Version ten"), None);
        assert_eq!(kind("hello"), None);
    }

    #[test]
    fn identity_orientation_has_w() {
        // (0, 0, 0) stored means w = -1, the identity rotation.
        let q = quat_from_xyz(Vec3::ZERO);
        assert!(q.angle_between(Quat::IDENTITY) < 1e-6);
    }

    #[test]
    fn mesh_positions_are_weight_sums() {
        let limits = Limits { neighbours: false, ..Limits::viewer() };
        let scene = load_mesh(Path::new("t.md5mesh"), CUBE_MESH.as_bytes(), &limits).unwrap();
        let p = &scene.meshes[0].primitives[0];
        let b = Basis::z_up_x_forward_inches();
        assert!((p.positions[1] - b.point(Vec3::new(0.0, 10.0, 0.0))).length() < 1e-6);
        // Half of (0, 0, 10) from the arm, half of (10, 0, 10) from the root.
        assert!((p.positions[2] - b.point(Vec3::new(5.0, 0.0, 10.0))).length() < 1e-6);
        assert_eq!(p.joints[2][..2], [0, 1]);
        assert_eq!(scene.skins[0].joints.len(), 2);
        assert_eq!(scene.nodes[1].name, "arm");
        // The arm sits 10 inches above the root.
        assert!((scene.nodes[1].transform.translation - Vec3::new(0.0, 0.254, 0.0)).length() < 1e-6);
    }

    #[test]
    fn animations_override_components() {
        let text = r#"MD5Version 10
commandline ""
numFrames 2
numJoints 2
frameRate 10
numAnimatedComponents 1
hierarchy {
	"root"	-1 0 0
	"arm"	0 4 0	// tz
}
bounds {
	( 0 0 0 ) ( 1 1 1 )
	( 0 0 0 ) ( 1 1 1 )
}
baseframe {
	( 0 0 0 ) ( 0 0 0 )
	( 0 0 10 ) ( 0 0 0 )
}
frame 0 {
	10
}
frame 1 {
	20
}
"#;
        let scene = load_anim(Path::new("walk.md5anim"), text.as_bytes(), &Limits::viewer()).unwrap();
        let clip = &scene.animations[0];
        assert_eq!(clip.name, "walk");
        assert!((clip.duration - 0.1).abs() < 1e-6);
        assert_eq!(clip.channels.len(), 1);
        let ch = &clip.channels[0];
        assert_eq!(ch.node, 1);
        assert!((ch.values[4] - 0.508).abs() < 1e-5, "{:?}", ch.values);
    }
}
