//! Inter-Quake Export (`.iqe`), the text form of IQM: joints with their
//! base poses, meshes of vertices (`vp`, `vt`, `vn`, `vc`, `vb`) and faces
//! (`fa` counted from the file's first vertex, `fm` from the mesh's), and
//! animations of frames, each a pose per joint.  Poses come as a
//! translation with a quaternion (`pq`, whose missing w is negative as in
//! MD5), a rotation matrix (`pm`) or Euler angles in radians (`pa`).
//! Coordinates and winding are IQM's.

use crate::iqm::{OUTWARD, basis};
use crate::read::{Result, float, int, lines, words};
use crate::skeleton::{Bone, Budget, Rig, sampled_clip, strongest_four, unique_names};
use crate::textures::Textures;
use slerp_scene::glam::{Mat3, Quat, Vec2, Vec3, Vec4};
use slerp_scene::{Limits, LoadError, Material, Mesh, Node, Primitive, Scene, TexRef, Transform};
use std::path::Path;

pub fn looks_like_iqe(text: &str) -> bool {
    text.starts_with("# Inter-Quake Export")
}

/// A pose command's transform, in the file's coordinates.
fn pose(w: &[&str]) -> Option<Transform> {
    let v: Vec<f32> = w[1..].iter().map(|s| float(s)).collect::<Option<Vec<f32>>>()?;
    let get = |i: usize, default: f32| v.get(i).copied().unwrap_or(default);
    let translation = Vec3::new(get(0, 0.0), get(1, 0.0), get(2, 0.0));
    let (rotation, scale_at) = match w[0] {
        "pq" => {
            let xyz = Vec3::new(get(3, 0.0), get(4, 0.0), get(5, 0.0));
            let w = v.get(6).copied().unwrap_or_else(|| -(1.0 - xyz.length_squared()).max(0.0).sqrt());
            (Quat::from_xyzw(xyz.x, xyz.y, xyz.z, w), 7)
        }
        "pm" => {
            let row = |i: usize| Vec3::new(get(i, 0.0), get(i + 1, 0.0), get(i + 2, 0.0));
            let m = Mat3::from_cols(row(3), row(6), row(9));
            let (_, r, _) = slerp_scene::glam::Mat4::from_mat3(m).to_scale_rotation_translation();
            (r, 12)
        }
        "pa" => (crate::smd::euler(Vec3::new(get(3, 0.0), get(4, 0.0), get(5, 0.0))), 6),
        _ => return None,
    };
    let rotation =
        if rotation.is_finite() && rotation.length_squared() > 1e-12 { rotation.normalize() } else { Quat::IDENTITY };
    let scale = Vec3::new(get(scale_at, 1.0), get(scale_at + 1, 1.0), get(scale_at + 2, 1.0));
    Some(Transform { translation, rotation, scale })
}

struct IqeMesh {
    name: String,
    material: String,
    first_vertex: usize,
    faces: Vec<u32>,
}

#[derive(Default)]
struct IqeAnim {
    name: String,
    fps: f32,
    frames: Vec<Vec<Transform>>,
}

pub fn load(path: &Path, bytes: &[u8], limits: &Limits) -> Result<Scene> {
    let text = crate::read::text(bytes);
    if !looks_like_iqe(&text) {
        return Err(LoadError::NotMine);
    }
    let basis = basis();
    let mut budget = Budget::new(bytes.len() as u64);
    let mut joints: Vec<(String, Option<usize>)> = Vec::new();
    let mut base_poses: Vec<Transform> = Vec::new();
    let mut anims: Vec<IqeAnim> = Vec::new();
    let mut meshes: Vec<IqeMesh> = Vec::new();
    let mut positions: Vec<Vec3> = Vec::new();
    let mut uvs: Vec<Vec2> = Vec::new();
    let mut normals: Vec<Vec3> = Vec::new();
    let mut colors: Vec<Vec4> = Vec::new();
    let mut blends: Vec<Vec<(u32, f32)>> = Vec::new();
    let mut any_faces = false;

    // 1. Read every command.
    for line in lines(&text) {
        let w = words(line);
        let Some(&cmd) = w.first() else { continue };
        if cmd.starts_with('#') {
            continue;
        }
        let nums = || w[1..].iter().filter_map(|s| float(s)).collect::<Vec<f32>>();
        match cmd {
            "joint" => {
                if joints.len() >= crate::skeleton::MAX_BONES {
                    return Err(LoadError::TooLarge("too many joints".into()));
                }
                let parent = w.get(2).and_then(|s| int(s)).and_then(|p| usize::try_from(p).ok());
                joints.push((w.get(1).unwrap_or(&"").to_string(), parent));
            }
            "pq" | "pm" | "pa" => {
                let p = pose(&w).ok_or_else(|| LoadError::invalid(format!("IQE: bad pose: {line}")))?;
                budget.take(1, 40, "poses")?;
                match anims.last_mut() {
                    Some(a) => match a.frames.last_mut() {
                        Some(f) => f.push(p),
                        None => a.frames.push(vec![p]),
                    },
                    None => base_poses.push(p),
                }
            }
            "mesh" => meshes.push(IqeMesh {
                name: w.get(1).unwrap_or(&"").to_string(),
                material: String::new(),
                first_vertex: positions.len(),
                faces: Vec::new(),
            }),
            "material" => {
                if let Some(m) = meshes.last_mut() {
                    m.material = w.get(1).unwrap_or(&"").to_string();
                }
            }
            "vp" => {
                budget.take(1, 96, "vertices")?;
                let v = nums();
                positions.push(Vec3::new(
                    v.first().copied().unwrap_or(0.0),
                    v.get(1).copied().unwrap_or(0.0),
                    v.get(2).copied().unwrap_or(0.0),
                ));
            }
            "vt" => {
                let v = nums();
                uvs.push(Vec2::new(v.first().copied().unwrap_or(0.0), v.get(1).copied().unwrap_or(0.0)));
            }
            "vn" => {
                let v = nums();
                normals.push(Vec3::new(
                    v.first().copied().unwrap_or(0.0),
                    v.get(1).copied().unwrap_or(0.0),
                    v.get(2).copied().unwrap_or(0.0),
                ));
            }
            "vc" => {
                let v = nums();
                let c = |i: usize, d: f32| v.get(i).copied().unwrap_or(d);
                colors.push(Vec4::new(c(0, 0.0), c(1, 0.0), c(2, 0.0), c(3, 1.0)));
            }
            "vb" => {
                let v = nums();
                blends.push(v.chunks_exact(2).take(16).map(|p| (p[0].max(0.0) as u32, p[1])).collect());
            }
            "fa" | "fm" => {
                any_faces = true;
                let first = if cmd == "fm" { meshes.last().map_or(0, |m| m.first_vertex) } else { 0 };
                let corners: Vec<u32> = w[1..]
                    .iter()
                    .filter_map(|s| int(s))
                    .filter_map(|i| {
                        let abs = if i < 0 { positions.len() as i64 + i } else { first as i64 + i };
                        u32::try_from(abs).ok()
                    })
                    .collect();
                budget.take(corners.len().saturating_sub(2), 12, "triangles")?;
                if meshes.is_empty() {
                    meshes.push(IqeMesh {
                        name: String::new(),
                        material: String::new(),
                        first_vertex: 0,
                        faces: Vec::new(),
                    });
                }
                let faces = &mut meshes.last_mut().unwrap().faces;
                for k in 1..corners.len().saturating_sub(1) {
                    faces.extend([corners[0], corners[k], corners[k + 1]]);
                }
            }
            "animation" => {
                anims.push(IqeAnim { name: w.get(1).unwrap_or(&"").to_string(), fps: 0.0, frames: Vec::new() })
            }
            "framerate" => {
                if let (Some(a), Some(f)) = (anims.last_mut(), w.get(1).and_then(|s| float(s))) {
                    a.fps = f;
                }
            }
            "frame" => {
                if let Some(a) = anims.last_mut() {
                    a.frames.push(Vec::new());
                }
            }
            _ => {}
        }
    }
    // Meshes without faces are triangle lists of their own vertices.
    if !any_faces {
        let ends: Vec<usize> = meshes.iter().skip(1).map(|m| m.first_vertex).chain([positions.len()]).collect();
        for (m, end) in meshes.iter_mut().zip(ends) {
            m.faces = (m.first_vertex as u32..end as u32).collect();
            m.faces.truncate(m.faces.len() / 3 * 3);
        }
    }

    // 2. The skeleton from the base poses.
    let bones: Vec<Bone> = joints
        .iter()
        .enumerate()
        .map(|(i, (name, parent))| Bone {
            name: name.clone(),
            parent: *parent,
            local: basis.transform(base_poses.get(i).copied().unwrap_or(Transform::IDENTITY)),
        })
        .collect();
    let rig = Rig::from_local(bones)?;
    let mut scene = Scene::default();
    let base = rig.add_nodes(&mut scene);
    let skinned = !rig.is_empty() && blends.len() == positions.len();
    if skinned {
        scene.skins.push(rig.skin(base, "skeleton"));
    }

    // 3. One vertex list, then a primitive per mesh.
    let n = positions.len();
    let mut all = Primitive { positions: positions.iter().map(|&p| basis.point(p)).collect(), ..Default::default() };
    if uvs.len() == n {
        all.uv0 = uvs;
    }
    if normals.len() == n {
        all.normals = normals.iter().map(|&v| basis.dir(v).normalize_or(Vec3::Y)).collect();
    }
    if colors.len() == n {
        all.colors = colors;
    }
    if skinned {
        for mut list in blends {
            list.retain(|&(j, _)| (j as usize) < rig.len());
            let (j, w) = strongest_four(&mut list, 0);
            all.joints.push(j);
            all.weights.push(w);
        }
    }
    let flip = basis.flip_winding(OUTWARD);
    let mut textures = Textures::new(path, limits);
    for (mi, m) in meshes.iter_mut().enumerate() {
        if flip {
            crate::mesh::flip(&mut m.faces);
        }
        let (mut prim, _) = crate::mesh::gather(&all, &m.faces);
        if prim.indices.is_empty() {
            continue;
        }
        prim.ensure_normals();
        prim.material = Some(scene.materials.len());
        let mut material =
            Material { name: m.material.clone(), base_color: Vec4::ONE, roughness: 0.7, ..Default::default() };
        if textures.dir().is_some() && !m.material.is_empty() {
            match textures.locate(&m.material, true) {
                Some(p) => material.base_color_texture = textures.load_path(&p).map(TexRef::new),
                None => textures.warn(format!("texture not found: {}", m.material)),
            }
        }
        scene.materials.push(material);
        let name = if m.name.is_empty() { format!("mesh {mi}") } else { m.name.clone() };
        scene.meshes.push(Mesh { name: name.clone(), primitives: vec![prim], target_names: Vec::new() });
        let mut node = Node::new(name);
        node.mesh = Some(scene.meshes.len() - 1);
        node.skin = skinned.then_some(0);
        scene.nodes.push(node);
        scene.roots.push(scene.nodes.len() - 1);
    }
    scene.textures = std::mem::take(&mut textures.list);
    scene.info.warnings.append(&mut textures.warnings);

    // 4. Animations: each frame a pose per joint.
    if limits.animations {
        let rest = rig.rest();
        let count = rig.len();
        for a in &anims {
            let mut poses = Vec::with_capacity(a.frames.len() * count);
            for f in &a.frames {
                for (j, &r) in rest.iter().enumerate() {
                    poses.push(f.get(j).map_or(r, |&p| basis.transform(p)));
                }
            }
            let fps = if a.fps > 0.0 { a.fps } else { 24.0 };
            scene.animations.push(sampled_clip(&a.name, fps, count, &poses, &rest, |b| Some(base + b)));
        }
        unique_names(&mut scene.animations);
    }
    scene.info.format = "Inter-Quake Export (IQE)".into();
    crate::describe_rig(&mut scene, &rig);
    Ok(scene)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub const TRIANGLE: &str = "# Inter-Quake Export
joint \"root\" -1
pq 0 0 0 0 0 0 1
joint \"arm\" 0
pq 0 0 1 0 0 0
mesh \"tri\"
material \"skin.png\"
vp 0 0 0
vt 0 0
vb 0 1
vp 1 0 0
vt 1 0
vb 1 1
vp 0 0 1
vt 0 1
vb 0 0.5 1 0.5
fm 0 1 2
animation \"wave\"
framerate 10
frame
pq 0 0 0 0 0 0 1
pq 0 0 1 0 0 0 1
frame
pq 0 0 0 0 0 0 1
pq 1 0 1 0 0 0 1
";

    #[test]
    fn triangle_with_animation() {
        assert!(looks_like_iqe(TRIANGLE));
        let limits = Limits { neighbours: false, ..Limits::viewer() };
        let scene = load(Path::new("t.iqe"), TRIANGLE.as_bytes(), &limits).unwrap();
        let p = &scene.meshes[0].primitives[0];
        assert_eq!(p.positions.len(), 3);
        assert!((p.positions[2] - Vec3::new(0.0, 1.0, 0.0)).length() < 1e-6);
        assert_eq!(p.joints[1][0], 1);
        // `pq` without w makes the identity with a negative w.
        assert!(scene.nodes[1].transform.rotation.angle_between(Quat::IDENTITY) < 1e-6);
        let clip = &scene.animations[0];
        assert_eq!(clip.name, "wave");
        assert_eq!(clip.channels.len(), 1);
        assert_eq!(clip.channels[0].values[3..6], [1.0, 1.0, 0.0]);
    }

    #[test]
    fn pose_forms_agree() {
        let pq = pose(&["pq", "1", "2", "3", "0", "0", "0.70710677", "0.70710677"]).unwrap();
        let pa = pose(&["pa", "1", "2", "3", "0", "0", "1.5707964"]).unwrap();
        let pm = pose(&["pm", "1", "2", "3", "0", "1", "0", "-1", "0", "0", "0", "0", "1"]).unwrap();
        for p in [pa, pm] {
            assert_eq!(p.translation, pq.translation);
            assert!(p.rotation.angle_between(pq.rotation) < 1e-5, "{:?} {:?}", p.rotation, pq.rotation);
        }
    }
}
