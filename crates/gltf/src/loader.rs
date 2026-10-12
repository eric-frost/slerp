//! The load itself: picking the scene, deciding what to decode, decoding it
//! in parallel, and assembling the scene model.

use crate::accessor::Accessors;
use crate::data::{self, Neighbours, Origins};
use crate::json::{NodeJ, Root};
use crate::materials::{self, TexRefs};
use crate::vrm::Vrm;
use crate::{Ctx, anim, meshes};
use rayon::prelude::*;
use slerp_scene::glam::{Mat4, Quat, Vec3};
use slerp_scene::textures::DecodeOptions;
use slerp_scene::{Info, Limits, LoadError, Mesh, Node, Scene, Transform};
use std::path::Path;

/// Extensions whose meaning Slerp understands, at least as far as a
/// still picture or the viewer needs.  A required extension outside this
/// list gets a warning, since parts of the model may be missing.
const SUPPORTED: &[&str] = &[
    "EXT_mesh_gpu_instancing",
    "EXT_meshopt_compression",
    "EXT_texture_webp",
    "KHR_draco_mesh_compression",
    "KHR_lights_punctual",
    "KHR_materials_clearcoat",
    "KHR_materials_emissive_strength",
    "KHR_materials_ior",
    "KHR_materials_pbrSpecularGlossiness",
    "KHR_materials_specular",
    "KHR_materials_unlit",
    "KHR_mesh_quantization",
    "KHR_node_visibility",
    "KHR_meshopt_compression",
    "KHR_texture_basisu",
    "KHR_texture_transform",
    "MSFT_texture_dds",
];

/// More instances than this on one node are dropped, so a file can't make
/// the renderer draw millions of copies.
const MAX_INSTANCES: usize = 65_536;

pub fn load_scene(path: &Path, bytes: &[u8], limits: &Limits) -> Result<Scene, LoadError> {
    // 1. Container, JSON and format.
    let ctx = Ctx::new(bytes.len() as u64, limits.max_file_bytes.saturating_sub(bytes.len() as u64));
    let container = data::container(bytes, &ctx)?;
    let root: Root =
        serde_json::from_slice(container.json).map_err(|e| LoadError::invalid(format!("glTF JSON: {e}")))?;
    let version = root.asset.min_version.as_deref().unwrap_or(&root.asset.version);
    if root.asset.version.is_empty() {
        return Err(if container.binary { LoadError::invalid("no asset version") } else { LoadError::NotMine });
    }
    if !version.starts_with("2.") {
        return Err(LoadError::Unsupported(format!("glTF {version} (only 2.0 is supported)")));
    }
    let vrm = Vrm::parse(&root, &ctx);
    let mut info = describe(&root, container.binary, &vrm, &ctx);

    // 2. The nodes of the chosen scene, and the meshes, skins and clips
    // they use.  Triangles are counted from the JSON before decoding.
    let scene_index = root.scene.filter(|&s| s < root.scenes.len()).or((!root.scenes.is_empty()).then_some(0));
    let sel = select(&root, scene_index, &ctx);
    if root.scenes.len() > 1
        && let Some(s) = scene_index
    {
        let name = root.scenes[s].name.clone().unwrap_or_else(|| format!("scene {s}"));
        info.details.push(("Scenes".into(), format!("{}, showing {name}", root.scenes.len())));
    }
    let used_nodes = || sel.old_of_new.iter().filter_map(|&o| root.nodes.get(o));
    let mesh_ids = sorted(used_nodes().filter_map(|n| n.mesh).filter(|&m| m < root.meshes.len()));
    let mesh_map = index_map(root.meshes.len(), &mesh_ids);
    let triangles: u64 = mesh_ids.iter().map(|&m| meshes::triangle_estimate(&root, &root.meshes[m])).sum();
    if triangles > limits.max_triangles {
        return Err(LoadError::TooLarge(format!("{triangles} triangles")));
    }
    let skin_ids =
        sorted(used_nodes().filter(|n| n.mesh.is_some()).filter_map(|n| n.skin).filter(|&s| s < root.skins.len()));
    let skin_map = index_map(root.skins.len(), &skin_ids);
    let anim_ids: Vec<usize> = if limits.animations { (0..root.animations.len()).collect() } else { Vec::new() };

    // 3. Materials in use, with glTF's default material appended for
    // primitives that name none, and the textures they need.
    let prims = || mesh_ids.iter().flat_map(|&m| &root.meshes[m].primitives);
    let material_ids = sorted(prims().filter_map(|p| p.material).filter(|&m| m < root.materials.len()));
    let material_map = index_map(root.materials.len(), &material_ids);
    let needs_default = prims().any(|p| p.material.is_none_or(|m| m >= root.materials.len()));
    let default_material = needs_default.then_some(material_ids.len());
    let material_of = |m: Option<usize>| m.and_then(|m| material_map.get(m).copied().flatten()).or(default_material);
    let mut used_textures = vec![false; root.textures.len()];
    for &m in &material_ids {
        let extra = vrm.extra_textures(m);
        for t in materials::textures_of(&root.materials[m]).chain(extra) {
            if let Some(slot) = used_textures.get_mut(t) {
                *slot = true;
            }
        }
    }

    // 4. The accessors, buffer views and buffers all that reads.
    let mut need_acc = vec![false; root.accessors.len()];
    let mut mark = |a: usize| {
        if let Some(slot) = need_acc.get_mut(a) {
            *slot = true;
        }
    };
    prims().flat_map(meshes::accessors_of).for_each(&mut mark);
    skin_ids.iter().filter_map(|&s| root.skins[s].inverse_bind_matrices).for_each(&mut mark);
    anim_ids.iter().flat_map(|&a| anim::accessors_of(&root.animations[a], &sel.new_of_old)).for_each(&mut mark);
    used_nodes()
        .filter(|n| n.mesh.is_some())
        .filter_map(|n| n.extensions.instancing.as_ref())
        .flat_map(|i| i.attributes.values().copied())
        .for_each(&mut mark);
    let mut need_view = vec![false; root.buffer_views.len()];
    let mut mark_view = |v: usize| {
        if let Some(slot) = need_view.get_mut(v) {
            *slot = true;
        }
    };
    for (a, _) in root.accessors.iter().zip(&need_acc).filter(|(_, n)| **n) {
        a.buffer_view.into_iter().for_each(&mut mark_view);
        if let Some(s) = &a.sparse {
            mark_view(s.indices.buffer_view);
            mark_view(s.values.buffer_view);
        }
    }
    // Draco-compressed primitives keep their geometry in a view of their
    // own; their attribute accessors have none.
    prims().filter_map(|p| p.extensions.draco.as_ref()).for_each(|d| mark_view(d.buffer_view));
    for t in (0..root.textures.len()).filter(|&t| used_textures[t]) {
        for i in materials::image_candidates(&root, t) {
            root.images[i].buffer_view.into_iter().for_each(&mut mark_view);
        }
    }
    let neighbours = Neighbours { dir: path.parent().map(Path::to_path_buf), enabled: limits.neighbours };
    // Pictures in the binary chunk are kept as ranges of the file, which
    // needs the bytes to be the file on disk.
    let on_disk = std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.len() == bytes.len() as u64);
    let origins = Origins::new(&root, &container, path, on_disk, &neighbours, limits.originals);
    let need_buf = data::buffers_for_views(&root, &need_view);
    let buffers = data::load_buffers(&root, container.bin, &need_buf, &neighbours, &ctx);
    let views = data::resolve_views(&root, &buffers, &need_view, &ctx)?;
    let acc = Accessors { json: &root.accessors, views: &views, view_json: &root.buffer_views, ctx: &ctx };

    // 5. Images, meshes, skins and clips, all in parallel.
    let opts = match limits.max_texture_size {
        Some(size) => DecodeOptions::preview(size),
        None => DecodeOptions::full(),
    };
    let morphs = meshes::morph_counts(&root);
    let (decoded, (meshes, (skins, animations))) = rayon::join(
        || {
            materials::decode_images(
                &root,
                &used_textures,
                &views,
                &neighbours,
                &origins,
                &opts,
                limits.gpu_blocks,
                &ctx,
            )
        },
        || {
            rayon::join(
                || -> Result<Vec<Mesh>, LoadError> {
                    let load = |&m: &usize| meshes::load_mesh(m, &root.meshes[m], &acc, &ctx, &material_of);
                    mesh_ids.par_iter().map(load).collect()
                },
                || {
                    rayon::join(
                        || -> Result<Vec<_>, LoadError> {
                            let load = |&s: &usize| anim::load_skin(s, &root.skins[s], &acc, &ctx, &sel.new_of_old);
                            skin_ids.par_iter().map(load).collect()
                        },
                        || -> Result<Vec<_>, LoadError> {
                            let load = |&a: &usize| {
                                anim::load_animation(
                                    a,
                                    &root.animations[a],
                                    &acc,
                                    &ctx,
                                    &root,
                                    &sel.new_of_old,
                                    &morphs,
                                )
                            };
                            anim_ids.par_iter().map(load).collect()
                        },
                    )
                },
            )
        },
    );
    let (meshes, skins, animations) = (meshes?, skins?, animations?);

    // 6. Materials, now that it's known which textures loaded.
    let refs = TexRefs { root: &root, of_texture: decoded.of_texture };
    let mut scene_materials: Vec<_> = material_ids
        .iter()
        .map(|&m| {
            let mut material = materials::build_material(m, &root.materials[m], &refs);
            vrm.adjust_material(m, &mut material, &refs);
            material
        })
        .collect();
    if needs_default {
        scene_materials.push(materials::default_material());
    }

    // 7. Nodes.  Skins that failed to load leave their meshes unskinned.
    let mut skin_slots: Vec<Option<usize>> = Vec::with_capacity(skins.len());
    let mut next_slot = 0;
    for s in &skins {
        skin_slots.push(s.as_ref().map(|_| next_slot));
        next_slot += usize::from(s.is_some());
    }
    let skin_of = |s: usize| skin_map.get(s).copied().flatten().and_then(|s| skin_slots[s]);
    let mut scene = Scene {
        name: scene_name(&root, scene_index, path),
        meshes,
        materials: scene_materials,
        textures: decoded.textures,
        skins: skins.into_iter().flatten().collect(),
        animations: animations.into_iter().flatten().collect(),
        ..Default::default()
    };
    for (new, &old) in sel.old_of_new.iter().enumerate() {
        let j = &root.nodes[old];
        let mut node = Node::new(j.name.clone().unwrap_or_else(|| format!("node {old}")));
        node.transform = transform(j, &ctx);
        node.children = sel.children[new].clone();
        node.mesh = j.mesh.and_then(|m| mesh_map.get(m).copied().flatten());
        node.skin = node.mesh.and(j.skin).and_then(skin_of);
        if let Some(m) = j.mesh.and_then(|m| root.meshes.get(m)) {
            node.weights = j.weights.clone().or_else(|| m.weights.clone()).unwrap_or_default();
        }
        node.visible = j.extensions.visibility.as_ref().is_none_or(|v| v.visible);
        scene.nodes.push(node);
    }
    scene.roots = sel.roots.clone();
    expand_instances(&mut scene, &root, &sel.old_of_new, &acc, &ctx)?;
    hide_subtrees(&mut scene);
    sanitize_joints(&mut scene);

    // 8. VRM: the bone map, expressions, and turning 0.x models around.
    scene.humanoid = vrm.humanoid(&sel.new_of_old);
    scene.expressions = vrm.expressions(&root, &sel.new_of_old, &morphs);
    if vrm.faces_back() {
        scene.wrap_roots(Transform { rotation: Quat::from_rotation_y(std::f32::consts::PI), ..Transform::IDENTITY });
    }

    info.neighbours = ctx.take_files();
    info.warnings = ctx.into_warnings();
    scene.info = info;
    Ok(scene)
}

/*┌─────────────────╮
| Selection       |
└─────────────────╯*/
//region Selection

/// The nodes of one scene, renumbered in depth-first order so parents come
/// before children.  Malformed hierarchies are made into a forest: a node
/// reached a second time (through a cycle or a second parent) is dropped
/// there.
struct Selection {
    old_of_new: Vec<usize>,
    new_of_old: Vec<Option<usize>>,
    children: Vec<Vec<usize>>,
    roots: Vec<usize>,
}

fn select(root: &Root, scene: Option<usize>, ctx: &Ctx) -> Selection {
    let n = root.nodes.len();
    let mut sel =
        Selection { old_of_new: Vec::new(), new_of_old: vec![None; n], children: Vec::new(), roots: Vec::new() };
    let mut parent = vec![None; n];
    for (i, node) in root.nodes.iter().enumerate() {
        for &c in &node.children {
            if c < n && parent[c].is_none() && c != i {
                parent[c] = Some(i);
            }
        }
    }
    let starts: Vec<usize> = match scene {
        Some(s) => root.scenes[s].nodes.clone(),
        None => (0..n).filter(|&i| parent[i].is_none()).collect(),
    };
    let mut odd_hierarchy = false;
    let mut add_tree = |sel: &mut Selection, start: usize| {
        if start >= n || sel.new_of_old[start].is_some() {
            return;
        }
        let claim = |sel: &mut Selection, old: usize| {
            let new = sel.old_of_new.len();
            sel.old_of_new.push(old);
            sel.new_of_old[old] = Some(new);
            sel.children.push(Vec::new());
            new
        };
        let top = claim(sel, start);
        sel.roots.push(top);
        let mut stack = vec![start];
        while let Some(old) = stack.pop() {
            let new = sel.new_of_old[old].unwrap_or(0);
            for &c in &root.nodes[old].children {
                if c >= n || sel.new_of_old[c].is_some() {
                    odd_hierarchy = true;
                    continue;
                }
                let child = claim(sel, c);
                sel.children[new].push(child);
                stack.push(c);
            }
        }
    };
    for s in starts {
        add_tree(&mut sel, s);
    }
    // Joints outside the scene's trees: bring in the tree each one hangs
    // from, so skinned meshes still have their skeletons.  Each skin and
    // each node is looked at once, however the trees chain together.
    let mut skin_done = vec![false; root.skins.len()];
    let mut scanned = 0;
    while scanned < sel.old_of_new.len() {
        let old = sel.old_of_new[scanned];
        scanned += 1;
        let Some(s) = root.nodes[old].mesh.and(root.nodes[old].skin).filter(|&s| s < root.skins.len()) else {
            continue;
        };
        if std::mem::replace(&mut skin_done[s], true) {
            continue;
        }
        for &j in &root.skins[s].joints {
            if j >= n || sel.new_of_old[j].is_some() {
                continue;
            }
            // Every node climbed here is claimed by `add_tree`, so no node
            // is climbed twice.
            let mut top = j;
            for _ in 0..n {
                match parent[top] {
                    Some(p) if sel.new_of_old[p].is_none() => top = p,
                    _ => break,
                }
            }
            add_tree(&mut sel, top);
        }
    }
    if odd_hierarchy {
        ctx.warn("the node hierarchy has cycles, shared children or missing nodes");
    }
    sel
}

fn sorted(it: impl Iterator<Item = usize>) -> Vec<usize> {
    let mut v: Vec<usize> = it.collect();
    v.sort_unstable();
    v.dedup();
    v
}

/// Maps the file's indices to positions in `ids`.
fn index_map(len: usize, ids: &[usize]) -> Vec<Option<usize>> {
    let mut map = vec![None; len];
    for (new, &old) in ids.iter().enumerate() {
        map[old] = Some(new);
    }
    map
}

//endregion

/*┌─────────────────╮
| Nodes           |
└─────────────────╯*/
//region Nodes

fn transform(j: &NodeJ, ctx: &Ctx) -> Transform {
    let t = match j.matrix {
        Some(m) => Transform::from_matrix(Mat4::from_cols_array(&m)),
        None => Transform {
            translation: j.translation.map_or(Vec3::ZERO, Vec3::from_array),
            rotation: j.rotation.map_or(Quat::IDENTITY, |r| unit_quat(Quat::from_array(r))),
            scale: j.scale.map_or(Vec3::ONE, Vec3::from_array),
        },
    };
    if t.translation.is_finite() && t.rotation.is_finite() && t.scale.is_finite() {
        t
    } else {
        ctx.warn("a node's transform isn't finite and was reset");
        Transform::IDENTITY
    }
}

/// Normalizes a rotation, or gives the identity for a zero or broken one.
fn unit_quat(q: Quat) -> Quat {
    let len = q.length();
    if len.is_finite() && len > 1e-12 { q / len } else { Quat::IDENTITY }
}

/// `KHR_node_visibility` hides a node with everything under it, while
/// Slerp's flag hides one node's mesh: the flag goes down the tree.  Nodes
/// are numbered parents first (see `select`).
fn hide_subtrees(scene: &mut Scene) {
    for i in 0..scene.nodes.len() {
        if !scene.nodes[i].visible {
            for c in scene.nodes[i].children.clone() {
                if let Some(child) = scene.nodes.get_mut(c) {
                    child.visible = false;
                }
            }
        }
    }
}

/// `EXT_mesh_gpu_instancing`: each instance becomes a child node showing
/// the mesh.
fn expand_instances(
    scene: &mut Scene,
    root: &Root,
    old_of_new: &[usize],
    acc: &Accessors,
    ctx: &Ctx,
) -> Result<(), LoadError> {
    for (new, &old) in old_of_new.iter().enumerate() {
        let Some(inst) = root.nodes[old].extensions.instancing.as_ref() else { continue };
        let Some(mesh) = scene.nodes[new].mesh else { continue };
        let read = |name: &str, pad: [f32; 4]| -> Result<Option<Vec<[f32; 4]>>, LoadError> {
            match inst.attributes.get(name) {
                Some(&a) => match acc.read::<4>(a, pad) {
                    Ok(v) => Ok(Some(v)),
                    Err(e @ LoadError::TooLarge(_)) => Err(e),
                    Err(e) => {
                        ctx.warn(format!("instance {name}s couldn't be read: {e}"));
                        Ok(None)
                    }
                },
                None => Ok(None),
            }
        };
        let translations = read("TRANSLATION", [0.0; 4])?;
        let rotations = read("ROTATION", [0.0, 0.0, 0.0, 1.0])?;
        let scales = read("SCALE", [1.0; 4])?;
        let counts = [&translations, &rotations, &scales].map(|v| v.as_ref().map(Vec::len));
        let Some(count) = counts.iter().flatten().copied().min() else { continue };
        if counts.iter().flatten().any(|&c| c != count) {
            ctx.warn("instance attributes have different lengths");
        }
        if count > MAX_INSTANCES {
            ctx.warn(format!("only the first {MAX_INSTANCES} of {count} instances are shown"));
        }
        let get = |v: &Option<Vec<[f32; 4]>>, i: usize, default: [f32; 4]| v.as_ref().map_or(default, |v| v[i]);
        let name = scene.nodes[new].name.clone();
        for i in 0..count.min(MAX_INSTANCES) {
            let [tx, ty, tz, _] = get(&translations, i, [0.0; 4]);
            let [sx, sy, sz, _] = get(&scales, i, [1.0; 4]);
            let rotation = Quat::from_array(get(&rotations, i, [0.0, 0.0, 0.0, 1.0]));
            let mut node = Node::new(format!("{name} instance {i}"));
            node.transform = Transform {
                translation: Vec3::new(tx, ty, tz),
                rotation: unit_quat(rotation),
                scale: Vec3::new(sx, sy, sz),
            };
            node.mesh = Some(mesh);
            node.weights = scene.nodes[new].weights.clone();
            scene.nodes.push(node);
            let child = scene.nodes.len() - 1;
            scene.nodes[new].children.push(child);
        }
        scene.nodes[new].mesh = None;
    }
    Ok(())
}

/// Clears vertex influences that name a joint past the end of the skin,
/// so neither the renderer nor the CPU skinning reads outside it.  A mesh
/// shown by several skins is checked against the smallest.
fn sanitize_joints(scene: &mut Scene) {
    let mut limit: Vec<Option<usize>> = vec![None; scene.meshes.len()];
    for node in &scene.nodes {
        if let (Some(m), Some(s)) = (node.mesh, node.skin) {
            let joints = scene.skins[s].joints.len();
            limit[m] = Some(limit[m].map_or(joints, |l| l.min(joints)));
        }
    }
    for (mesh, limit) in scene.meshes.iter_mut().zip(limit) {
        let Some(limit) = limit else { continue };
        for p in &mut mesh.primitives {
            for (j, w) in p.joints.iter_mut().zip(&mut p.weights) {
                for k in 0..4 {
                    if j[k] as usize >= limit {
                        j[k] = 0;
                        w[k] = 0.0;
                    }
                }
            }
        }
    }
}

//endregion

/*┌─────────────────╮
| Info            |
└─────────────────╯*/
//region Info

fn describe(root: &Root, binary: bool, vrm: &Vrm, ctx: &Ctx) -> Info {
    let mut info = Info {
        format: match (vrm.format(), binary) {
            (Some(v), _) => v.to_string(),
            (None, true) => "glTF 2.0 (binary)".into(),
            (None, false) => "glTF 2.0 (JSON)".into(),
        },
        generator: root.asset.generator.clone(),
        ..Info::default()
    };
    let mut add = |name: &str, value: String| info.details.push((name.into(), value));
    if let Some(c) = &root.asset.copyright {
        add("Copyright", c.clone());
    }
    if !root.extensions_used.is_empty() {
        add("Extensions used", root.extensions_used.join(", "));
    }
    if !root.extensions_required.is_empty() {
        add("Extensions required", root.extensions_required.join(", "));
    }
    if let Some(lights) = &root.extensions.lights
        && !lights.lights.is_empty()
    {
        let mut kinds: Vec<(String, usize)> = Vec::new();
        for l in &lights.lights {
            match kinds.iter_mut().find(|(k, _)| *k == l.kind) {
                Some((_, n)) => *n += 1,
                None => kinds.push((l.kind.clone(), 1)),
            }
        }
        let list: Vec<String> = kinds.iter().map(|(k, n)| format!("{n} {k}")).collect();
        add("Lights", list.join(", "));
    }
    vrm.describe(&mut info);
    for ext in &root.extensions_required {
        if !SUPPORTED.contains(&ext.as_str()) {
            ctx.warn(format!("the file requires {ext}, which isn't supported; parts may be missing or wrong"));
        }
    }
    info
}

fn scene_name(root: &Root, scene: Option<usize>, path: &Path) -> String {
    scene
        .and_then(|s| root.scenes[s].name.clone())
        .filter(|n| !n.is_empty())
        .or_else(|| path.file_stem().map(|s| s.to_string_lossy().into_owned()))
        .unwrap_or_default()
}

//endregion
