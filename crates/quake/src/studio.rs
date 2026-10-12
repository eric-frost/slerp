//! Source engine studio models (`.mdl` with magic "IDST", versions 44 to
//! 49: Half-Life 2 and its episodes, Portal, Team Fortress 2, Counter-Strike:
//! Source and Global Offensive, Left 4 Dead, Portal 2, Garry's Mod).
//!
//! A model is three files: the `.mdl` holds the skeleton, body parts,
//! materials, skins, flexes and sequences; the `.vvd` holds the vertices
//! (with bone weights) in the reference pose; the `.vtx` (`.dx90.vtx` and
//! its siblings) holds each mesh's triangles per level of detail, of which
//! the full-detail one is shown.  Materials are `.vmt` files with `.vtf`
//! textures, found in the game's `materials/` folders and VPK packs above
//! the model.
//!
//! The first model of each body part is shown and the others are hidden
//! nodes the viewer can switch on.  The skin families (alternative sets of
//! materials) are the file's parts, so the viewer can pick one.  Flex
//! descriptors become morph targets and sequences skeletal clips, both in
//! the viewer only.  Vertices are skinned to up to three bones; the
//! skeleton keeps Source's units and Z-up axes under a root node that
//! converts both, as for Half-Life models.
//!
//! Not read: attachments, hitboxes, eyeball projection (eyes show their
//! base texture or grey), flex controllers and rules (each flex descriptor
//! is its own slider), animations in included models, and IK and root
//! motion.

mod anim;
mod materials;
mod mdl;
mod vtx;
mod vvd;

use crate::read::{Bytes, Result};
use crate::{find_in, find_relative, read_neighbour, y_up_root};
use slerp_scene::glam::{Mat4, Vec3, Vec4};
use slerp_scene::{Info, Limits, LoadError, Mesh, MorphTarget, Node, Primitive, Scene, Skin, Transform};
use std::collections::HashMap;
use std::path::Path;

/// The `.vtx` files tried, best first: studiomdl writes the Direct3D 9
/// one for every model and the others for older hardware.
const VTX_SUFFIXES: &[&str] = &["dx90.vtx", "dx80.vtx", "sw.vtx", "vtx", "xbox.vtx"];

/// Whether the start of a file is a Source model: "IDST" and a version from
/// 44 to 49, or a later one this loader refuses with a message.  Half-Life's
/// version 10 isn't.
pub(crate) fn sniff(head: &[u8]) -> bool {
    head.starts_with(b"IDST")
        && head.get(4..8).is_some_and(|v| {
            let v = i32::from_le_bytes([v[0], v[1], v[2], v[3]]);
            (44..=59).contains(&v)
        })
}

/// The file's parts: its skin families, when it has more than one.
pub(crate) fn parts(bytes: &[u8]) -> Vec<String> {
    match mdl::header(Bytes::new(bytes)) {
        Ok(h) if h.skin_families > 1 => (1..=h.skin_families).map(|i| format!("Skin {i}")).collect(),
        _ => Vec::new(),
    }
}

/// The skin family a part name stands for.
pub(crate) fn family_of(part: &str) -> Option<usize> {
    part.strip_prefix("Skin ")?.parse::<usize>().ok()?.checked_sub(1)
}

/// Loads the model wearing skin family `family`.
pub(crate) fn load(path: &Path, data: &[u8], limits: &Limits, family: usize) -> Result<Scene> {
    let b = Bytes::new(data);
    let h = mdl::header(b)?;
    let mut info = Info { format: format!("Source engine model (MDL version {})", h.version), ..Default::default() };
    if h.bodyparts.0 == 0 {
        return Err(LoadError::invalid("the model has no body parts (an animation-only model)"));
    }

    // 1. The vertices and triangles beside it.
    if !limits.neighbours {
        return Err(LoadError::invalid("the model's geometry is in its .vvd and .vtx files, which weren't read"));
    }
    let dir = path.parent().ok_or_else(|| LoadError::invalid("the model has no folder"))?;
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
    let vvd_bytes = find_in(dir, &format!("{stem}.vvd"))
        .and_then(|p| read_neighbour(&p))
        .ok_or_else(|| LoadError::invalid(format!("{stem}.vvd, which holds the model's vertices, wasn't found")))?;
    let vtx_file = VTX_SUFFIXES.iter().find_map(|s| find_in(dir, &format!("{stem}.{s}")));
    let vtx_bytes = vtx_file.as_deref().and_then(read_neighbour).ok_or_else(|| {
        LoadError::invalid(format!("{stem}.dx90.vtx, which holds the model's triangles, wasn't found"))
    })?;
    let vvd = vvd::read(&vvd_bytes)?;
    let vtx = vtx::read(&vtx_bytes, h.version >= 49)?;
    let checksum = b.i32(8)?;
    if vvd.checksum != checksum || vtx.checksum != checksum {
        info.warnings.push("the .vvd or .vtx file belongs to another build of this model".into());
    }

    // 2. The skeleton, in Source's space under a root that converts it.
    let bones = mdl::bones(b, &h)?;
    let mut scene =
        Scene { name: path.file_stem().and_then(|s| s.to_str()).unwrap_or("model").to_string(), ..Default::default() };
    let mut world = vec![Mat4::IDENTITY; bones.len()];
    for (i, bone) in bones.iter().enumerate() {
        let local = Mat4::from_rotation_translation(bone.quat, bone.pos);
        world[i] = bone.parent.map_or(local, |p| world[p] * local);
    }
    let mut root = Node::new("root");
    root.transform = y_up_root();
    scene.nodes.push(root);
    let bone_base = scene.nodes.len();
    for bone in &bones {
        let mut n = Node::new(bone.name.clone());
        n.transform = Transform { translation: bone.pos, rotation: bone.quat, scale: Vec3::ONE };
        scene.nodes.push(n);
    }
    for (i, bone) in bones.iter().enumerate() {
        let parent = bone.parent.map_or(0, |p| bone_base + p);
        scene.nodes[parent].children.push(bone_base + i);
    }
    let convert = y_up_root().matrix();
    scene.skins.push(Skin {
        name: "skeleton".into(),
        joints: (0..bones.len()).map(|i| bone_base + i).collect(),
        inverse_bind: world.iter().map(|w| (convert * *w).inverse()).collect(),
        skeleton: Some(0),
    });

    // 3. Materials for the chosen skin family.
    let names = mdl::textures(b, &h)?;
    let folders = mdl::material_folders(b, &h)?;
    let (found, lib) = materials::find(path, &names, &folders, limits, limits.animations);
    let table = mdl::skin_table(b, &h)?;
    let row = table.get(family.min(table.len().saturating_sub(1)));
    let mut made: HashMap<usize, usize> = HashMap::new();
    let material_of = |scene: &mut Scene, made: &mut HashMap<usize, usize>, skin_ref: usize| -> Option<usize> {
        let tex = row.and_then(|r| r.get(skin_ref)).copied().unwrap_or(skin_ref);
        let f = found.get(tex)?;
        Some(*made.entry(tex).or_insert_with(|| materials::add(scene, f)))
    };

    // 4. Body parts: each model is a mesh of one primitive per material
    //    mesh, with the triangles of its full-detail level.
    let flexes = limits.animations;
    let parts = mdl::bodyparts(b, &h, flexes)?;
    let flex_names = if flexes { mdl::flex_names(b, &h)? } else { Vec::new() };
    let mut triangles = 0u64;
    let mut missing_triangles = false;
    for (p, part) in parts.iter().enumerate() {
        for (m, model) in part.models.iter().enumerate() {
            if model.meshes.is_empty() {
                continue;
            }
            let tris = vtx.parts.get(p).and_then(|ms| ms.get(m));
            // The flex descriptors any of the model's meshes moves, which
            // become the mesh's morph targets.
            let mut targets: Vec<usize> = model
                .meshes
                .iter()
                .flat_map(|k| &k.flexes)
                .flat_map(|f| std::iter::once(f.desc).chain(f.pair))
                .collect();
            targets.sort_unstable();
            targets.dedup();
            let mut mesh = Mesh {
                name: model.name.clone(),
                target_names: targets.iter().map(|&t| flex_names.get(t).cloned().unwrap_or_default()).collect(),
                ..Default::default()
            };
            for (k, mm) in model.meshes.iter().enumerate() {
                let Some(list) = tris.and_then(|t| t.get(k)) else {
                    missing_triangles = true;
                    continue;
                };
                let base = model.vertex_start.saturating_add(mm.vertex_offset);
                let mut prim =
                    Primitive { material: material_of(&mut scene, &mut made, mm.material), ..Default::default() };
                let mut map: HashMap<u32, u32> = HashMap::new();
                let mut corner = |local: u32, prim: &mut Primitive| -> Result<u32> {
                    if let Some(&i) = map.get(&local) {
                        return Ok(i);
                    }
                    if local as usize >= mm.vertex_count {
                        return Err(LoadError::invalid("a triangle uses a vertex past its mesh's"));
                    }
                    let v = vvd
                        .vertices
                        .get(base + local as usize)
                        .ok_or_else(|| LoadError::invalid("a triangle uses a vertex the .vvd file doesn't have"))?;
                    let i = prim.positions.len() as u32;
                    prim.positions.push(convert.transform_point3(v.position));
                    prim.normals.push(convert.transform_vector3(v.normal).normalize_or(Vec3::Y));
                    prim.uv0.push(v.uv);
                    let mut joints = [0u16; 4];
                    let mut weights = Vec4::ZERO;
                    for j in 0..v.count.max(1) as usize {
                        let bone = v.bones[j] as usize;
                        joints[j] = if bone < bones.len() { bone as u16 } else { 0 };
                        weights[j] = if v.count == 0 { 1.0 } else { v.weights[j].max(0.0) };
                    }
                    let sum = weights.x + weights.y + weights.z;
                    prim.joints.push(joints);
                    prim.weights.push(if sum > 0.0 { weights / sum } else { Vec4::X });
                    map.insert(local, i);
                    Ok(i)
                };
                for t in list.chunks_exact(3) {
                    // Source's front faces wind clockwise.
                    let (a, c, d) = (corner(t[0], &mut prim)?, corner(t[1], &mut prim)?, corner(t[2], &mut prim)?);
                    prim.indices.extend_from_slice(&[a, d, c]);
                }
                triangles += (prim.indices.len() / 3) as u64;
                if triangles > limits.max_triangles {
                    return Err(LoadError::TooLarge(format!("{triangles} triangles")));
                }
                if prim.indices.is_empty() {
                    continue;
                }
                if !targets.is_empty() {
                    prim.targets = morph_targets(&targets, &mm.flexes, &map, prim.positions.len(), convert);
                }
                mesh.primitives.push(prim);
            }
            if mesh.primitives.is_empty() {
                continue;
            }
            let mut node =
                Node::new(if model.name.is_empty() { format!("{} {}", part.name, m + 1) } else { model.name.clone() });
            node.visible = m == 0;
            node.skin = Some(0);
            node.weights = vec![0.0; mesh.target_names.len()];
            scene.meshes.push(mesh);
            node.mesh = Some(scene.meshes.len() - 1);
            scene.nodes.push(node);
        }
    }
    if scene.meshes.is_empty() {
        return Err(LoadError::invalid("the model's .vtx file has no triangles for it"));
    }
    if missing_triangles {
        info.warnings.push("the .vtx file lacks some of the model's meshes".into());
    }
    scene.compute_roots();

    // 5. Sequences as clips, from this file and its `.ani` file.
    if limits.animations && h.seqs.0 > 0 {
        let ani = if h.anim_blocks.0 > 0 { find_ani(path, b, &h) } else { None };
        if h.anim_blocks.0 > 0 && ani.is_none() {
            info.warnings.push("some animations are in the model's .ani file, which wasn't found".into());
        }
        let sources = anim::Sources { mdl: b, header: &h, ani: ani.as_deref().map(Bytes::new) };
        scene.animations = anim::clips(&sources, &bones, bone_base, &mut info.warnings);
    }

    // 6. Facts for the inspector.
    let mut used: Vec<usize> = made.keys().copied().collect();
    used.sort_unstable();
    let missing: Vec<&str> =
        used.iter().filter_map(|&t| found.get(t)).filter(|f| f.base.is_none()).map(|f| f.name.as_str()).collect();
    if !missing.is_empty() && !lib.is_empty() {
        let shown = missing.iter().take(8).copied().collect::<Vec<_>>().join(", ");
        let more = if missing.len() > 8 { format!(" and {} more", missing.len() - 8) } else { String::new() };
        info.warnings.push(format!("materials or textures not found: {shown}{more}"));
    } else if lib.is_empty() && !names.is_empty() {
        info.warnings.push("no materials folder or VPK pack was found above the model".into());
    }
    let includes = mdl::includes(b, &h).unwrap_or_default();
    if !includes.is_empty() {
        info.warnings.push(format!("animations in included models aren't loaded: {}", includes.join(", ")));
    }
    let d = &mut info.details;
    if !h.name.is_empty() {
        d.push(("Model name".into(), h.name.clone()));
    }
    d.push(("Bones".into(), bones.len().to_string()));
    let groups: Vec<String> = parts
        .iter()
        .map(|p| if p.models.len() > 1 { format!("{} ({} choices)", p.name, p.models.len()) } else { p.name.clone() })
        .collect();
    d.push(("Body parts".into(), groups.join(", ")));
    if table.len() > 1 {
        d.push(("Skin families".into(), format!("{} (showing {})", table.len(), family.min(table.len() - 1) + 1)));
    }
    d.push((
        "Materials".into(),
        found.iter().map(|f| f.path.as_deref().unwrap_or(&f.name)).collect::<Vec<_>>().join(", "),
    ));
    if !folders.is_empty() {
        d.push(("Material folders".into(), folders.join(", ")));
    }
    if !lib.is_empty() {
        d.push(("Searched".into(), lib.describe()));
    }
    d.push(("Sequences".into(), h.seqs.0.to_string()));
    if h.flexdescs.0 > 0 {
        d.push(("Flexes".into(), h.flexdescs.0.to_string()));
    }
    let surface = mdl::string_at(b, h.surface_prop);
    if !surface.is_empty() {
        d.push(("Surface".into(), surface));
    }
    if h.mass > 0.0 {
        d.push(("Mass".into(), format!("{} kg", h.mass)));
    }
    scene.info = info;
    Ok(scene)
}

/// Each target's position and normal offsets for one primitive, from the
/// mesh's flexes.  A left and right pair splits each vertex's move between
/// its two descriptors by the vertex's side.
fn morph_targets(
    targets: &[usize],
    flexes: &[mdl::Flex],
    map: &HashMap<u32, u32>,
    count: usize,
    convert: Mat4,
) -> Vec<MorphTarget> {
    let mut out: Vec<MorphTarget> = targets
        .iter()
        .map(|_| MorphTarget { positions: vec![Vec3::ZERO; count], normals: vec![Vec3::ZERO; count] })
        .collect();
    let slot = |desc: usize| targets.binary_search(&desc).ok();
    for flex in flexes {
        let left = slot(flex.desc);
        let right = flex.pair.and_then(slot);
        for v in &flex.verts {
            let Some(&i) = map.get(&(v.index as u32)) else { continue };
            let (dp, dn) =
                (convert.transform_vector3(v.delta), convert.transform_vector3(v.ndelta).normalize_or_zero());
            let shares = match right {
                Some(r) => [(left, 1.0 - v.side), (Some(r), v.side)],
                None => [(left, 1.0), (None, 0.0)],
            };
            for (t, w) in shares {
                if let Some(t) = t {
                    out[t].positions[i as usize] += dp * w;
                    out[t].normals[i as usize] += dn * v.ndelta.length() * w;
                }
            }
        }
    }
    out
}

/// The model's `.ani` file: beside it with the same name, or where the
/// model says, below any folder above it.
fn find_ani(path: &Path, b: Bytes, h: &mdl::Header) -> Option<Vec<u8>> {
    let dir = path.parent()?;
    let stem = path.file_stem()?.to_str()?;
    if let Some(p) = find_in(dir, &format!("{stem}.ani")) {
        return read_neighbour(&p);
    }
    let stored = mdl::string_at(b, h.anim_block_name);
    if stored.is_empty() {
        return None;
    }
    let p = dir.ancestors().take(8).find_map(|a| find_relative(a, &stored))?;
    read_neighbour(&p)
}

#[cfg(test)]
mod tests;
