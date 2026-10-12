//! Valve's SMD, the text format Half-Life and Source models are compiled
//! from: a node list (the bones), skeleton frames (each bone's position and
//! Euler angles relative to its parent, frame by frame), and, in reference
//! files, triangles whose corners carry a material, a model-space position,
//! normal, texture coordinates and bone weights.  The same file type holds
//! a model's meshes (`triangles`) or one animation (frames only).
//!
//! Opening a reference mesh also offers the animation SMDs in its folder
//! as clips, when their bones are the mesh's by name; studiomdl compiles
//! them at 30 frames a second unless a `.qc` says otherwise, so that is the
//! rate used.  Source space is right-handed, Z up, models facing +X, in
//! inches.  Flex animation (`vertexanimation`) is not read.
//!
//! [`parse`] and [`euler`] are public so the Source MDL loader can share
//! the format's conventions.

use crate::basis::Basis;
use crate::read::{Result, float, int, lines, words};
use crate::skeleton::{Bone, Budget, Rig, sampled_clip, strongest_four, unique_names};
use crate::textures::{Textures, read_limited, siblings};
use slerp_scene::glam::{Quat, Vec2, Vec3, Vec4};
use slerp_scene::{Limits, LoadError, Material, Mesh, Node, Primitive, Scene, TexRef, Transform};
use std::collections::HashMap;
use std::path::Path;

/// studiomdl's frame rate when a `.qc` names none.
pub const FPS: f32 = 30.0;

/// Animation SMDs read beside a reference mesh, and their total size.
const MAX_ANIM_FILES: usize = 64;
const MAX_ANIM_BYTES: u64 = 512 << 20;

/*┌─────────────────╮
| Parsing         |
└─────────────────╯*/
//region Parsing

/// A parsed SMD, in the file's own coordinates.
#[derive(Debug, Default)]
pub struct Smd {
    pub nodes: Vec<SmdNode>,
    /// Skeleton frames in the order of their `time` numbers: each node's
    /// position and Euler angles (radians) relative to its parent.  A node
    /// a frame leaves out keeps its pose from the frame before.
    pub frames: Vec<Vec<(Vec3, Vec3)>>,
    /// Each frame's `time` number.
    pub times: Vec<i64>,
    pub triangles: Vec<SmdTriangle>,
    /// Weights of all corners: (node index, weight); corners index ranges
    /// of this.
    pub links: Vec<(usize, f32)>,
    /// Whether the file has flex animation, which isn't read.
    pub has_vertex_animation: bool,
}

#[derive(Debug, Clone)]
pub struct SmdNode {
    pub name: String,
    /// Index into `Smd::nodes`.
    pub parent: Option<usize>,
}

#[derive(Debug, Clone)]
pub struct SmdTriangle {
    pub material: String,
    pub corners: [SmdCorner; 3],
}

#[derive(Debug, Clone, Copy)]
pub struct SmdCorner {
    /// The node that takes whatever weight the links leave.
    pub parent: Option<usize>,
    pub position: Vec3,
    pub normal: Vec3,
    pub uv: Vec2,
    /// A range of `Smd::links`.
    pub links: (u32, u32),
}

/// Whether text is an SMD: `version 1`, then `nodes`, after any blank lines
/// and comments.
pub fn looks_like_smd(text: &str) -> bool {
    let mut significant = lines(text).map(str::trim).filter(|l| !l.is_empty() && !l.starts_with("//"));
    significant.next().is_some_and(|l| words(l) == ["version", "1"]) && significant.next() == Some("nodes")
}

/// Source's Euler angles: a roll about X, then a pitch about Y, then a yaw
/// about Z (`AngleQuaternion` in the SDK).
pub fn euler(angles: Vec3) -> Quat {
    Quat::from_rotation_z(angles.z) * Quat::from_rotation_y(angles.y) * Quat::from_rotation_x(angles.x)
}

/// Parses an SMD.  Counts are bounded by `budget`, so a file can't claim
/// more than its size justifies.
pub fn parse(text: &str, budget: &mut Budget) -> Result<Smd> {
    #[derive(PartialEq)]
    enum Section {
        None,
        Nodes,
        Skeleton,
        Triangles,
        Skip,
    }
    let mut smd = Smd::default();
    let mut section = Section::None;
    let mut ids: HashMap<i64, usize> = HashMap::new();
    let mut parents: Vec<i64> = Vec::new();
    let mut frame: Option<Vec<(Vec3, Vec3)>> = None;
    let mut by_time: HashMap<i64, usize> = HashMap::new();
    let mut corners: Vec<SmdCorner> = Vec::new();
    let mut material = String::new();
    for line in lines(text) {
        let w = words(line);
        let Some(&first) = w.first() else { continue };
        if first == "end" {
            if section == Section::Nodes {
                // Parents by id, now that every id is known.
                for (i, &p) in parents.iter().enumerate() {
                    smd.nodes[i].parent = ids.get(&p).copied().filter(|&p| p != i);
                }
            }
            if let Some(f) = frame.take() {
                smd.frames.push(f);
            }
            section = Section::None;
            continue;
        }
        match section {
            Section::None => {
                section = match first {
                    "nodes" => Section::Nodes,
                    "skeleton" => Section::Skeleton,
                    "triangles" => Section::Triangles,
                    "version" => Section::None,
                    "vertexanimation" => {
                        smd.has_vertex_animation = true;
                        Section::Skip
                    }
                    _ => Section::Skip,
                };
            }
            Section::Nodes => {
                let (Some(id), Some(name), Some(parent)) =
                    (w.first().and_then(|s| int(s)), w.get(1), w.get(2).and_then(|s| int(s)))
                else {
                    continue;
                };
                if smd.nodes.len() >= crate::skeleton::MAX_BONES {
                    return Err(LoadError::TooLarge("too many SMD nodes".into()));
                }
                ids.insert(id, smd.nodes.len());
                parents.push(parent);
                smd.nodes.push(SmdNode { name: name.to_string(), parent: None });
            }
            Section::Skeleton => {
                if first == "time" {
                    if let Some(f) = frame.take() {
                        smd.frames.push(f);
                    }
                    let time = w.get(1).and_then(|s| int(s)).unwrap_or(smd.times.last().map_or(0, |t| t + 1));
                    budget.take(smd.nodes.len(), 24, "skeleton frames")?;
                    // A frame starts from the one before, so bones it leaves
                    // out hold their pose; a repeated time replaces its frame.
                    let start =
                        smd.frames.last().cloned().unwrap_or_else(|| vec![(Vec3::ZERO, Vec3::ZERO); smd.nodes.len()]);
                    if let Some(&at) = by_time.get(&time) {
                        frame = Some(std::mem::take(&mut smd.frames[at]));
                        smd.frames.remove(at);
                        smd.times.remove(at);
                        by_time.retain(|_, v| *v != at);
                        for v in by_time.values_mut() {
                            if *v > at {
                                *v -= 1;
                            }
                        }
                    } else {
                        frame = Some(start);
                    }
                    by_time.insert(time, smd.frames.len());
                    smd.times.push(time);
                    continue;
                }
                let (Some(f), Some(&node)) =
                    (frame.as_mut(), w.first().and_then(|s| int(s)).and_then(|id| ids.get(&id)))
                else {
                    continue;
                };
                let v: Vec<f32> = w[1..].iter().filter_map(|s| float(s)).collect();
                if v.len() >= 6 && node < f.len() {
                    f[node] = (Vec3::new(v[0], v[1], v[2]), Vec3::new(v[3], v[4], v[5]));
                }
            }
            Section::Triangles => {
                if corners.is_empty() && !first.chars().next().is_some_and(|c| c.is_ascii_digit() || c == '-') {
                    material = line.trim().to_string();
                    continue;
                }
                if corners.is_empty() && w.len() < 9 {
                    // A material name that starts with a digit.
                    material = line.trim().to_string();
                    continue;
                }
                let v: Vec<f32> = w.iter().filter_map(|s| float(s)).collect();
                if v.len() < 9 {
                    return Err(LoadError::invalid(format!("SMD: a triangle corner has {} numbers", v.len())));
                }
                let start = smd.links.len() as u32;
                let declared = v.get(9).map_or(0, |&n| n.max(0.0) as usize);
                let count = declared.min(v.len().saturating_sub(10) / 2);
                for k in 0..count {
                    if let Some(&node) = ids.get(&(v[10 + 2 * k] as i64)) {
                        smd.links.push((node, v[11 + 2 * k]));
                    }
                }
                corners.push(SmdCorner {
                    parent: ids.get(&(v[0] as i64)).copied(),
                    position: Vec3::new(v[1], v[2], v[3]),
                    normal: Vec3::new(v[4], v[5], v[6]),
                    uv: Vec2::new(v[7], v[8]),
                    links: (start, smd.links.len() as u32),
                });
                if corners.len() == 3 {
                    budget.take(1, 160, "triangles")?;
                    smd.triangles.push(SmdTriangle {
                        material: material.clone(),
                        corners: [corners[0], corners[1], corners[2]],
                    });
                    corners.clear();
                }
            }
            Section::Skip => {}
        }
    }
    if let Some(f) = frame.take() {
        smd.frames.push(f);
    }
    if smd.nodes.is_empty() {
        return Err(LoadError::invalid("SMD: no nodes"));
    }
    // Frames in time order, each sized to the nodes.
    let mut order: Vec<usize> = (0..smd.frames.len().min(smd.times.len())).collect();
    order.sort_by_key(|&i| smd.times[i]);
    let frames = std::mem::take(&mut smd.frames);
    let times = std::mem::take(&mut smd.times);
    for i in order {
        let mut f = frames[i].clone();
        f.resize(smd.nodes.len(), (Vec3::ZERO, Vec3::ZERO));
        smd.frames.push(f);
        smd.times.push(times[i]);
    }
    Ok(smd)
}

//endregion

/*┌─────────────────╮
| Scenes          |
└─────────────────╯*/
//region Scenes

impl Smd {
    /// Frame `f`'s local transforms, converted.
    fn poses(&self, f: usize, basis: &Basis) -> Vec<Transform> {
        self.frames[f]
            .iter()
            .map(|&(p, a)| Transform {
                translation: basis.point(p),
                rotation: basis.rotation(euler(a)),
                scale: Vec3::ONE,
            })
            .collect()
    }

    /// The skeleton in its first frame (the bind pose of a reference file).
    fn rig(&self, basis: &Basis) -> Result<Rig> {
        let rest =
            if self.frames.is_empty() { vec![Transform::IDENTITY; self.nodes.len()] } else { self.poses(0, basis) };
        Rig::from_local(
            self.nodes
                .iter()
                .zip(rest)
                .map(|(n, local)| Bone { name: n.name.clone(), parent: n.parent, local })
                .collect(),
        )
    }

    /// Every frame's poses, frame after frame, and the frames' times in
    /// seconds.
    fn clip_data(&self, basis: &Basis, budget: &mut Budget) -> Result<(Vec<Transform>, Vec<f32>)> {
        budget.take(self.frames.len().saturating_mul(self.nodes.len()), 40, "poses")?;
        let poses = (0..self.frames.len()).flat_map(|f| self.poses(f, basis)).collect();
        let first = self.times.first().copied().unwrap_or(0);
        let times = self.times.iter().map(|&t| (t - first) as f32 / FPS).collect();
        Ok((poses, times))
    }
}

pub fn load(path: &Path, bytes: &[u8], limits: &Limits) -> Result<Scene> {
    let text = crate::read::text(bytes);
    let mut budget = Budget::new(bytes.len() as u64);
    let smd = parse(&text, &mut budget)?;
    let basis = Basis::z_up_x_forward_inches();
    let rig = smd.rig(&basis)?;
    let mut scene = Scene::default();
    let base = rig.add_nodes(&mut scene);
    let reference = !smd.triangles.is_empty();
    let name = path.file_stem().map_or("animation".into(), |s| s.to_string_lossy().into_owned());

    // 1. Meshes: corners with equal values share a vertex; one primitive
    //    per material.
    if reference {
        scene.skins.push(rig.skin(base, "skeleton"));
        let mut textures = Textures::new(path, limits);
        let mut by_material: Vec<(String, Primitive, HashMap<[u32; 12], u32>)> = Vec::new();
        let mut influences = Vec::new();
        let flip = basis.flip_winding(true);
        budget.take(smd.triangles.len(), 3 * 96, "vertices")?;
        for tri in &smd.triangles {
            let slot = match by_material.iter().position(|m| m.0 == tri.material) {
                Some(s) => s,
                None => {
                    by_material.push((tri.material.clone(), Primitive::default(), HashMap::new()));
                    by_material.len() - 1
                }
            };
            let (_, prim, seen) = &mut by_material[slot];
            let mut corner_index = [0u32; 3];
            for (k, c) in tri.corners.iter().enumerate() {
                let links = smd.links.get(c.links.0 as usize..c.links.1 as usize).unwrap_or(&[]);
                let key = corner_key(c, links);
                corner_index[k] = *seen.entry(key).or_insert_with(|| {
                    prim.positions.push(basis.point(c.position));
                    prim.normals.push(basis.dir(c.normal).normalize_or(Vec3::Y));
                    prim.uv0.push(Vec2::new(c.uv.x, 1.0 - c.uv.y));
                    influences.clear();
                    influences.extend(links.iter().map(|&(n, w)| (n as u32, w)));
                    // Whatever the links leave goes to the corner's node.
                    let total: f32 = links.iter().map(|l| l.1.max(0.0)).sum();
                    if let Some(p) = c.parent
                        && total < 0.999
                    {
                        influences.push((p as u32, 1.0 - total.max(0.0)));
                    }
                    let (j, w) = strongest_four(&mut influences, c.parent.unwrap_or(0) as u32);
                    prim.joints.push(j);
                    prim.weights.push(w);
                    prim.positions.len() as u32 - 1
                });
            }
            prim.indices.extend(if flip { [corner_index[0], corner_index[2], corner_index[1]] } else { corner_index });
        }
        let mut mesh = Mesh { name: name.clone(), primitives: Vec::new(), target_names: Vec::new() };
        for (material_name, mut prim, _) in by_material {
            if prim.normals.iter().all(|n| *n == Vec3::Y) {
                prim.normals.clear();
                prim.ensure_normals();
            }
            prim.material = Some(scene.materials.len());
            scene.materials.push(self::material(&material_name, &mut textures));
            mesh.primitives.push(prim);
        }
        scene.meshes.push(mesh);
        let mut node = Node::new(name.clone());
        node.mesh = Some(0);
        node.skin = Some(0);
        scene.nodes.push(node);
        scene.roots.push(scene.nodes.len() - 1);
        scene.textures = std::mem::take(&mut textures.list);
        scene.info.warnings.append(&mut textures.warnings);
        scene.info.details.push(("Triangles in file".into(), smd.triangles.len().to_string()));
    }

    // 2. Clips: the file's own frames, when it animates, and the animation
    //    SMDs beside a reference mesh.
    if limits.animations {
        if !smd.frames.is_empty() && (smd.frames.len() > 1 || !reference) {
            let (poses, times) = smd.clip_data(&basis, &mut budget)?;
            scene.animations.push(clip_at(&name, &times, rig.len(), &poses, &rig.rest(), |b| Some(base + b)));
        }
        if reference && limits.neighbours {
            add_neighbour_anims(path, &rig, base, &mut scene, &mut budget);
        }
    }
    scene.info.format = if reference { "Source SMD (reference mesh)" } else { "Source SMD (animation)" }.into();
    scene.info.details.push(("Frames".into(), smd.frames.len().to_string()));
    if smd.has_vertex_animation {
        scene.info.warnings.push("flex animation (vertexanimation) isn't shown".into());
    }
    crate::describe_rig(&mut scene, &rig);
    Ok(scene)
}

/// A clip at explicit key times (an SMD's `time` numbers may skip).
fn clip_at(
    name: &str,
    times: &[f32],
    bones: usize,
    poses: &[Transform],
    rest: &[Transform],
    node: impl Fn(usize) -> Option<usize>,
) -> slerp_scene::Animation {
    let mut clip = sampled_clip(name, FPS, bones, poses, rest, node);
    for ch in &mut clip.channels {
        for t in &mut ch.times {
            let frame = (*t * FPS).round() as usize;
            *t = times.get(frame).copied().unwrap_or(*t);
        }
    }
    clip.duration = times.last().copied().unwrap_or(0.0).max(0.0);
    clip
}

/// The bits of everything that makes a corner a distinct vertex.
fn corner_key(c: &SmdCorner, links: &[(usize, f32)]) -> [u32; 12] {
    let f = |v: f32| v.to_bits();
    let mut hash = 0u64;
    for &(n, w) in links {
        hash = hash.wrapping_mul(0x100000001b3).wrapping_add(n as u64) ^ w.to_bits() as u64;
    }
    [
        f(c.position.x),
        f(c.position.y),
        f(c.position.z),
        f(c.normal.x),
        f(c.normal.y),
        f(c.normal.z),
        f(c.uv.x),
        f(c.uv.y),
        c.parent.map_or(u32::MAX, |p| p as u32),
        hash as u32,
        (hash >> 32) as u32,
        links.len() as u32,
    ]
}

/// A material for an SMD material name: usually a picture's file name
/// (GoldSrc) or a material path whose picture may sit beside the model.
fn material(name: &str, textures: &mut Textures) -> Material {
    let mut m = Material { name: name.to_string(), base_color: Vec4::ONE, roughness: 0.7, ..Default::default() };
    let found = if textures.dir().is_some() { textures.locate(name, true) } else { None };
    if let Some(t) = found.and_then(|p| textures.load_path(&p)) {
        m.base_color_texture = Some(TexRef::new(t));
    } else if textures.dir().is_some() && !name.is_empty() {
        textures.warn(format!("texture not found: {name}"));
    }
    m
}

/// Adds the animation SMDs beside a reference mesh whose bones are the
/// mesh's, by name.  Files with triangles (other meshes, collision
/// models, levels of detail) are left out.
fn add_neighbour_anims(path: &Path, rig: &Rig, base: usize, scene: &mut Scene, budget: &mut Budget) {
    let Some(dir) = path.parent() else { return };
    let basis = Basis::z_up_x_forward_inches();
    let mut read = 0u64;
    let mut clips = Vec::new();
    for file in siblings(dir, &["smd"], MAX_ANIM_FILES * 4) {
        if file == path || clips.len() >= MAX_ANIM_FILES {
            continue;
        }
        let Some(bytes) = read_limited(&file, MAX_ANIM_BYTES.saturating_sub(read)) else { continue };
        read += bytes.len() as u64;
        let text = crate::read::text(&bytes);
        if !looks_like_smd(&text) {
            continue;
        }
        let mut own = Budget::new(bytes.len() as u64);
        let Ok(anim) = parse(&text, &mut own) else {
            scene.info.warnings.push(format!("{} couldn't be read", file.display()));
            continue;
        };
        if !anim.triangles.is_empty() || anim.frames.is_empty() {
            continue;
        }
        let map: Vec<Option<usize>> = anim.nodes.iter().map(|n| rig.find(&n.name)).collect();
        if map.iter().filter(|m| m.is_some()).count() * 10 < anim.nodes.len() * 6 {
            continue;
        }
        let Ok((poses, times)) = anim.clip_data(&basis, budget) else {
            scene.info.warnings.push("more animations than memory allows; some were left out".into());
            break;
        };
        let rest: Vec<Transform> = map.iter().map(|m| m.map_or(Transform::IDENTITY, |b| rig.bones[b].local)).collect();
        let name = file.file_stem().map_or("animation".into(), |s| s.to_string_lossy().into_owned());
        clips.push(clip_at(&name, &times, anim.nodes.len(), &poses, &rest, |n| map[n].map(|b| base + b)));
    }
    unique_names(&mut clips);
    scene.animations.extend(clips);
}

//endregion

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub const TRIANGLE: &str = "version 1
nodes
  0 \"root\" -1
  1 \"arm\" 0
end
skeleton
time 0
  0 0 0 0 0 0 0
  1 0 0 10 0 0 1.5707963
end
triangles
skin.bmp
  0 0 0 0 0 0 1 0 0
  0 10 0 0 0 0 1 1 0 1 1 1
  1 0 10 0 0 0 1 0 1 2 0 0.25 1 0.25
end
";

    #[test]
    fn sniffing() {
        assert!(looks_like_smd(TRIANGLE));
        assert!(looks_like_smd("// exported\n\nversion 1\r\nnodes\r\n"));
        assert!(!looks_like_smd("version 1\nskeleton\n"));
        assert!(!looks_like_smd("SEGA MEGA DRIVE"));
    }

    #[test]
    fn reference_mesh() {
        let limits = Limits { neighbours: false, ..Limits::viewer() };
        let scene = load(Path::new("ref.smd"), TRIANGLE.as_bytes(), &limits).unwrap();
        let p = &scene.meshes[0].primitives[0];
        assert_eq!(p.positions.len(), 3);
        // Source's +X (forward) is Slerp's +Z, its +Y (left) Slerp's +X;
        // inches become metres.
        assert!((p.positions[1] - Vec3::new(0.0, 0.0, 0.254)).length() < 1e-6);
        assert!((p.positions[2] - Vec3::new(0.254, 0.0, 0.0)).length() < 1e-6);
        // Corner 1: all on the arm.  Corner 2: a quarter each on the root
        // and the arm, the half left over on its own node (the arm).
        assert_eq!((p.joints[1][0], p.weights[1].x), (1, 1.0));
        assert_eq!(p.joints[2][..2], [1, 0]);
        assert!((p.weights[2].x - 0.75).abs() < 1e-6 && (p.weights[2].y - 0.25).abs() < 1e-6);
        // The arm is 10 inches up and turned a quarter turn about up.
        let arm = &scene.nodes[1].transform;
        assert!((arm.translation - Vec3::new(0.0, 0.254, 0.0)).length() < 1e-6);
        assert!(arm.rotation.angle_between(Quat::from_rotation_y(std::f32::consts::FRAC_PI_2)) < 1e-5);
        // Texture V runs downwards in Slerp.
        assert_eq!(p.uv0[2], Vec2::new(0.0, 0.0));
        assert!(scene.animations.is_empty());
    }

    #[test]
    fn animation_frames_hold_missing_bones() {
        let text = "version 1\nnodes\n0 \"root\" -1\n1 \"arm\" 0\nend\nskeleton\ntime 0\n0 0 0 0 0 0 0\n1 0 0 10 0 0 0\ntime 2\n1 0 0 20 0 0 0\ntime 1\n0 5 0 0 0 0 0\nend\n";
        let mut budget = Budget::new(text.len() as u64);
        let smd = parse(text, &mut budget).unwrap();
        assert_eq!(smd.times, vec![0, 1, 2]);
        // Time 1 came after time 2 in the file and starts from time 2's
        // pose; time 2 started from time 0's.
        assert_eq!(smd.frames[1][1].0, Vec3::new(0.0, 0.0, 20.0));
        assert_eq!(smd.frames[2][0].0, Vec3::ZERO);
        let scene = load(Path::new("walk.smd"), text.as_bytes(), &Limits::viewer()).unwrap();
        let clip = &scene.animations[0];
        assert_eq!(clip.name, "walk");
        assert!((clip.duration - 2.0 / 30.0).abs() < 1e-6);
    }

    #[test]
    fn animations_in_the_folder_become_clips() {
        let dir = std::env::temp_dir().join(format!("slerp-rigged-smd-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("model_ref.smd"), TRIANGLE).unwrap();
        // An animation of the same bones, one of other bones, and a
        // collision mesh (it has triangles), which aren't clips.
        let walk = "version 1\nnodes\n0 \"root\" -1\n1 \"arm\" 0\nend\nskeleton\ntime 0\n0 0 0 0 0 0 0\n1 0 0 10 0 0 0\ntime 1\n0 0 0 0 0 0 0\n1 0 0 12 0 0 0\nend\n";
        std::fs::write(dir.join("walk.smd"), walk).unwrap();
        std::fs::write(dir.join("other.smd"), walk.replace("\"arm\"", "\"tail\"").replace("\"root\"", "\"hips\""))
            .unwrap();
        std::fs::write(dir.join("model_phys.smd"), TRIANGLE).unwrap();
        let path = dir.join("model_ref.smd");
        let ((scene, _), neighbours) = slerp_scene::collect_neighbours(|| {
            (load(&path, &std::fs::read(&path).unwrap(), &Limits::viewer()).unwrap(), ())
        });
        std::fs::remove_dir_all(&dir).unwrap();
        let names: Vec<&str> = scene.animations.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(names, ["walk"]);
        assert!(neighbours.iter().any(|p| p.ends_with("walk.smd")));
        // The arm moves from 10 to 12 inches up over one frame at 30 per second.
        let ch = &scene.animations[0].channels[0];
        assert_eq!(ch.node, 1);
        assert!((ch.times[1] - 1.0 / 30.0).abs() < 1e-6);
        assert!((ch.values[4] - 12.0 * 0.0254).abs() < 1e-6);
    }

    #[test]
    fn euler_order() {
        // Yaw a quarter turn: +X goes to +Y.
        let q = euler(Vec3::new(0.0, 0.0, std::f32::consts::FRAC_PI_2));
        assert!((q * Vec3::X - Vec3::Y).length() < 1e-6);
        // Roll then yaw: +Y rolls to +Z, which yaw leaves alone.
        let q = euler(Vec3::new(std::f32::consts::FRAC_PI_2, 0.0, std::f32::consts::FRAC_PI_2));
        assert!((q * Vec3::Y - Vec3::Z).length() < 1e-6);
    }
}
