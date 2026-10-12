//! MikuMikuDance models (PMX 2.0 and 2.1, PMD) and motions (VMD).
//!
//! Both model formats are read into [`Model`], which becomes a scene the
//! same way: a skeleton whose bones all rest unrotated (MMD poses bones by
//! rotating them away from the model's own axes), one mesh with a
//! primitive per material, and vertex morphs as morph targets.  MMD space
//! is left-handed, Y up, models facing -Z, about 8 cm to a unit
//! (`Basis::mmd`).
//!
//! What is approximated, since Slerp draws physically based materials:
//! MMD's diffuse colour and texture become the base colour; specular power
//! a roughness; toon ramps, sphere maps (multiplied or added environment
//! pictures) and edge outlines are left out.  SDEF vertices (spherical
//! blending) are blended linearly between their two bones like BDEF2, and
//! QDEF (dual quaternion) ones like BDEF4.  Group morphs are expanded into
//! the vertex morphs they combine; bone, UV, material, flip and impulse
//! morphs are counted but not shown.  Physics and IK are not run: motions
//! play the keys they store, so legs driven by IK targets stay straight.

mod pmd;
mod pmx;
pub(crate) mod vmd;

pub use vmd::load_vmd;

use crate::basis::Basis;
use crate::read::Result;
use crate::skeleton::{Bone, Budget, Rig, strongest_four};
use crate::textures::Textures;
use slerp_scene::glam::{Vec2, Vec3, Vec4};
use slerp_scene::{AlphaMode, Limits, LoadError, Material, Mesh, Node, Primitive, Scene, TexRef, Transform};
use std::path::Path;

/// A model as both PMX and PMD describe it, in MMD's own coordinates.
#[derive(Default)]
struct Model {
    format: String,
    name: String,
    english_name: String,
    comment: String,
    positions: Vec<Vec3>,
    normals: Vec<Vec3>,
    uvs: Vec<Vec2>,
    /// Up to four bones and weights per vertex; -1 for none.
    bones: Vec<[i32; 4]>,
    weights: Vec<[f32; 4]>,
    indices: Vec<u32>,
    textures: Vec<String>,
    materials: Vec<MmdMaterial>,
    skeleton: Vec<MmdBone>,
    morphs: Vec<Morph>,
    /// How many vertices used SDEF and QDEF skinning.
    sdef: usize,
    qdef: usize,
}

struct MmdMaterial {
    name: String,
    diffuse: Vec4,
    specular: Vec3,
    shininess: f32,
    /// Drawing flags: 0x01 double-sided, 0x10 outline.
    flags: u8,
    /// Index into `Model::textures`.
    texture: Option<usize>,
    /// A sphere map: its texture and mode (1 multiply, 2 add, 3 sub-texture).
    sphere: Option<(usize, u8)>,
    /// How many indices the material draws, after the ones before it.
    index_count: usize,
}

struct MmdBone {
    /// The name as stored (PMX's Unicode text, PMD's Shift-JIS decoded).
    name: String,
    /// The name's Shift-JIS bytes, which VMD motions match bones by.
    sjis: Vec<u8>,
    parent: Option<usize>,
    position: Vec3,
}

struct Morph {
    name: String,
    sjis: Vec<u8>,
    kind: MorphKind,
}

enum MorphKind {
    /// Offsets of vertices.
    Vertex(Vec<(u32, Vec3)>),
    /// Other morphs and how strongly each applies.
    Group(Vec<(usize, f32)>),
    /// Bone, UV, material, flip and impulse morphs, which aren't shown.
    Other(&'static str),
}

/// A name's Shift-JIS bytes, as VMD and PMD store it.
fn sjis(name: &str) -> Vec<u8> {
    let (bytes, _, _) = encoding_rs::SHIFT_JIS.encode(name);
    bytes.into_owned()
}

pub fn load_pmx(path: &Path, bytes: &[u8], limits: &Limits) -> Result<Scene> {
    let mut budget = Budget::new(bytes.len() as u64);
    let model = pmx::parse(bytes, &mut budget)?;
    build(model, path, limits, &mut budget)
}

pub fn load_pmd(path: &Path, bytes: &[u8], limits: &Limits) -> Result<Scene> {
    let mut budget = Budget::new(bytes.len() as u64);
    let model = pmd::parse(bytes, &mut budget)?;
    build(model, path, limits, &mut budget)
}

/*┌─────────────────╮
| Scene           |
└─────────────────╯*/
//region Scene

/// Where a model's skeleton and mesh are in its scene, for motions.
pub(crate) struct Layout {
    /// Node of bone 0.
    base: usize,
    /// The node holding the mesh and its morph targets.
    mesh_node: usize,
    /// Each target's name's Shift-JIS bytes.
    target_names: Vec<Vec<u8>>,
}

fn build(model: Model, path: &Path, limits: &Limits, budget: &mut Budget) -> Result<Scene> {
    let basis = Basis::mmd();
    let n = model.positions.len();

    // 1. Skeleton: bones at their model-space positions, unrotated.
    let bones: Vec<Bone> = model
        .skeleton
        .iter()
        .map(|b| Bone {
            name: b.name.clone(),
            parent: b.parent,
            local: Transform { translation: basis.point(b.position), ..Transform::IDENTITY },
        })
        .collect();
    let rig = Rig::from_world(bones)?;
    let mut scene = Scene::default();
    let base = rig.add_nodes(&mut scene);
    let skinned = !rig.is_empty();
    if skinned {
        scene.skins.push(rig.skin(base, "skeleton"));
    }

    // 2. Every vertex once, converted.
    budget.take(n, 96, "vertices")?;
    let mut all = Primitive {
        positions: model.positions.iter().map(|&p| basis.point(p)).collect(),
        normals: model.normals.iter().map(|&v| basis.dir(v).normalize_or(Vec3::Y)).collect(),
        uv0: model.uvs.clone(),
        ..Default::default()
    };
    if skinned {
        let mut list = Vec::with_capacity(4);
        for (b, w) in model.bones.iter().zip(&model.weights) {
            list.clear();
            for k in 0..4 {
                if let Ok(j) = u32::try_from(b[k])
                    && (j as usize) < rig.len()
                {
                    list.push((j, w[k]));
                }
            }
            let (j, w) = strongest_four(&mut list, 0);
            all.joints.push(j);
            all.weights.push(w);
        }
    }

    // 3. Morph targets: vertex morphs, and group morphs expanded into the
    //    vertex morphs they hold.
    let mut targets: Vec<(&Morph, Vec<(u32, Vec3)>)> = Vec::new();
    let mut unshown: Vec<&'static str> = Vec::new();
    for m in &model.morphs {
        match &m.kind {
            MorphKind::Vertex(offsets) => {
                targets.push((m, offsets.iter().map(|&(v, d)| (v, basis.point(d))).collect()));
            }
            MorphKind::Group(members) => {
                let mut offsets = Vec::new();
                for &(member, weight) in members {
                    if let Some(Morph { kind: MorphKind::Vertex(o), .. }) = model.morphs.get(member) {
                        budget.take(o.len(), 16, "morph targets")?;
                        offsets.extend(o.iter().map(|&(v, d)| (v, basis.point(d) * weight)));
                    }
                }
                if offsets.is_empty() {
                    unshown.push("group");
                } else {
                    targets.push((m, offsets));
                }
            }
            MorphKind::Other(kind) => unshown.push(kind),
        }
    }

    // 4. A primitive per material, over the index ranges in order.  MMD
    //    draws clockwise faces in its left-handed space, which the mirror
    //    turns around.
    let flip = basis.flip_winding(true);
    let mut textures = Textures::new(path, limits);
    let mut mesh = Mesh {
        name: if model.name.is_empty() { "model".into() } else { model.name.clone() },
        primitives: Vec::new(),
        target_names: targets.iter().map(|t| t.0.name.clone()).collect(),
    };
    let mut start = 0usize;
    for m in &model.materials {
        let end = start.saturating_add(m.index_count).min(model.indices.len());
        let mut indices = model.indices[start.min(end)..end].to_vec();
        start = end;
        if flip {
            crate::mesh::flip(&mut indices);
        }
        let (mut prim, new_of_old) = crate::mesh::gather(&all, &indices);
        if prim.indices.is_empty() {
            continue;
        }
        for (_, offsets) in &targets {
            let t = crate::mesh::sparse_target(offsets, &new_of_old, prim.positions.len());
            budget.take(t.positions.len(), 12, "morph targets")?;
            prim.targets.push(t);
        }
        prim.ensure_normals();
        prim.material = Some(scene.materials.len());
        let mut picture = |i: usize| model.textures.get(i).and_then(|name| textures.load(name, true));
        scene.materials.push(material(m, &mut picture));
        mesh.primitives.push(prim);
    }
    if mesh.primitives.is_empty() && n > 0 {
        // No materials: one primitive of every triangle.
        let mut indices = model.indices.clone();
        if flip {
            crate::mesh::flip(&mut indices);
        }
        let (mut prim, _) = crate::mesh::gather(&all, &indices);
        prim.ensure_normals();
        mesh.primitives.push(prim);
    }
    let triangles: usize = mesh.primitives.iter().map(|p| p.triangle_count()).sum();
    if triangles as u64 > limits.max_triangles {
        return Err(LoadError::TooLarge(format!("{triangles} triangles")));
    }
    let target_names = targets.iter().map(|t| t.0.sjis.clone()).collect();
    scene.meshes.push(mesh);
    let mut node = Node::new(scene.meshes[0].name.clone());
    node.mesh = Some(0);
    node.skin = skinned.then_some(0);
    scene.nodes.push(node);
    let mesh_node = scene.nodes.len() - 1;
    scene.roots.push(mesh_node);
    scene.textures = std::mem::take(&mut textures.list);
    scene.info.warnings.append(&mut textures.warnings);
    // MMD blends every texture's alpha, which hair and eyelashes rely on.
    for m in &mut scene.materials {
        if m.base_color_texture.is_some_and(|t| scene.textures.get(t.texture).is_some_and(|t| t.has_alpha)) {
            m.alpha_mode = AlphaMode::Blend;
        }
    }

    // 5. Facts for the inspector.
    let info = &mut scene.info;
    info.format = model.format.clone();
    if !model.english_name.is_empty() && model.english_name != model.name {
        info.details.push(("English name".into(), model.english_name.clone()));
    }
    if let Some(line) = model.comment.lines().map(str::trim).find(|l| !l.is_empty()) {
        info.details.push(("Comment".into(), line.chars().take(200).collect()));
    }
    info.details.push(("Morphs".into(), targets.len().to_string()));
    if !unshown.is_empty() {
        let mut kinds: Vec<&str> = unshown.clone();
        kinds.sort_unstable();
        kinds.dedup();
        info.details.push(("Morphs not shown".into(), format!("{} ({})", unshown.len(), kinds.join(", "))));
    }
    if model.sdef > 0 {
        info.warnings.push(format!("{} SDEF vertices are blended linearly (as BDEF2)", model.sdef));
    }
    if model.qdef > 0 {
        info.warnings.push(format!("{} QDEF vertices are blended linearly (as BDEF4)", model.qdef));
    }
    if model.materials.iter().any(|m| m.flags & 0x10 != 0) {
        info.details.push(("Outlines".into(), "not drawn".into()));
    }
    crate::describe_rig(&mut scene, &rig);

    // 6. Motions beside the model.
    if limits.animations && limits.neighbours && skinned {
        let layout = Layout { base, mesh_node, target_names };
        let bone_names: Vec<&[u8]> = model.skeleton.iter().map(|b| b.sjis.as_slice()).collect();
        vmd::add_neighbours(path, &rig, &bone_names, &layout, &mut scene, budget);
    }
    Ok(scene)
}

/// MMD's colours are display values; Slerp's material colours are linear.
fn linear(c: f32) -> f32 {
    let c = c.clamp(0.0, 1.0);
    if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
}

fn material(m: &MmdMaterial, picture: &mut dyn FnMut(usize) -> Option<usize>) -> Material {
    let d = m.diffuse;
    let mut out = Material {
        name: m.name.clone(),
        base_color: Vec4::new(linear(d.x), linear(d.y), linear(d.z), d.w.clamp(0.0, 1.0)),
        double_sided: m.flags & 0x01 != 0,
        ..Default::default()
    };
    // Specular power as Blinn-Phong's exponent, turned into roughness.
    let power = m.shininess.max(0.0);
    out.roughness = (2.0 / (power + 2.0)).sqrt().sqrt().clamp(0.3, 0.9);
    out.specular = (m.specular.max_element() * 2.0).clamp(0.2, 1.0);
    let texture = m.texture.and_then(&mut *picture);
    if let Some(t) = texture {
        out.base_color_texture = Some(TexRef::new(t));
    }
    // An added sphere map is a fake reflection: a shinier surface stands in.
    if let Some((_, 2)) = m.sphere {
        out.roughness = out.roughness.min(0.4);
    }
    if out.base_color.w < 0.999 {
        out.alpha_mode = AlphaMode::Blend;
    }
    out
}

//endregion

#[cfg(test)]
pub(crate) mod tests;
