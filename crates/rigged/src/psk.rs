//! Unreal's ActorX formats, as UE Viewer (umodel), CUE4Parse and the
//! Blender add-ons write them: `.psk` meshes (`.pskx` adds extra UV sets,
//! vertex colours, normals and morph targets) and `.psa` animation sets.
//! Both are a run of chunks, each a 32-byte header (a 20-byte name, flags,
//! the size of one record and the record count) and its records.
//!
//! Conventions, as the Blender importers apply them: rotations are stored
//! the way Unreal's row-vector maths uses them, so every bone's quaternion
//! but the root's is the conjugate of the rotation it means; texture V runs
//! upwards; a wedge's 16-bit point index may carry junk in its high half
//! when the mesh has at most 65,536 points; and morph offsets have Y
//! negated relative to the points.  Unreal space is left-handed (X forward,
//! Y right, Z up, centimetres), so `Basis::unreal` mirrors it.
//!
//! Materials are known only by name; a picture of that name (or with the
//! usual `_D`, `_BaseColor`, `_Diffuse` endings) beside the mesh, or in a
//! `Textures` folder, becomes the base colour.  Animations in a `.psa`
//! beside the mesh with the same name, or whose bones match, play as clips.

use crate::basis::Basis;
use crate::read::{Cursor, Result, fixed_name, latin1, truncated};
use crate::skeleton::{Bone, Budget, Rig, sampled_clip, strongest_four, unique_names};
use crate::textures::{Textures, read_limited, siblings};
use slerp_scene::glam::{Quat, Vec2, Vec3, Vec4};
use slerp_scene::{Limits, LoadError, Material, Mesh, Node, Primitive, Scene, TexRef, Transform};
use std::path::Path;

/// The `.psa` files read beside a mesh, and their total size.
const MAX_PSA_FILES: usize = 16;
const MAX_PSA_BYTES: u64 = 512 << 20;

/*┌─────────────────╮
| Chunks          |
└─────────────────╯*/
//region Chunks

struct Chunk<'a> {
    name: String,
    size: usize,
    count: usize,
    data: &'a [u8],
}

/// Splits a file into its chunks, checking that each fits.
fn chunks(bytes: &[u8]) -> Result<Vec<Chunk<'_>>> {
    let mut c = Cursor::new(bytes);
    let mut out = Vec::new();
    while !c.is_empty() {
        let name = fixed_name(c.bytes(20, "a chunk header")?, latin1);
        let _flags = c.i32("a chunk header")?;
        let size = c.i32("a chunk header")?;
        let count = c.i32("a chunk header")?;
        let (size, count) = match (usize::try_from(size), usize::try_from(count)) {
            (Ok(s), Ok(n)) => (s, n),
            _ => return Err(LoadError::invalid(format!("chunk {name} has a negative size"))),
        };
        let total = size.checked_mul(count).ok_or_else(|| truncated(&name))?;
        let data = c.bytes(total, &format!("chunk {name}"))?;
        out.push(Chunk { name, size, count, data });
        if out.len() > 4096 {
            return Err(LoadError::invalid("too many chunks"));
        }
    }
    Ok(out)
}

impl<'a> Chunk<'a> {
    /// The records, each at least `min` bytes (later versions append
    /// fields, which are skipped).
    fn records(&self, min: usize) -> Result<impl Iterator<Item = Cursor<'a>> + '_> {
        if self.count > 0 && self.size < min {
            return Err(LoadError::invalid(format!(
                "chunk {} has {}-byte records, expected {min}",
                self.name, self.size
            )));
        }
        Ok((0..self.count).map(move |i| Cursor::new(&self.data[i * self.size..(i + 1) * self.size])))
    }
}

/// A bone record, shared by meshes (`REFSKELT`) and animations
/// (`BONENAMES`): 120 bytes.
struct RawBone {
    name: String,
    parent: i32,
    rotation: Quat,
    position: Vec3,
}

fn read_bones(chunk: &Chunk) -> Result<Vec<RawBone>> {
    let mut out = Vec::with_capacity(chunk.count.min(crate::skeleton::MAX_BONES));
    for mut r in chunk.records(120)? {
        let name = fixed_name(r.bytes(64, "a bone")?, latin1);
        let _flags = r.i32("a bone")?;
        let _children = r.i32("a bone")?;
        let parent = r.i32("a bone")?;
        let rotation = r.quat("a bone")?;
        let position = r.vec3("a bone")?;
        out.push(RawBone { name, parent, rotation, position });
    }
    Ok(out)
}

/// A bone's pose in Slerp's coordinates from its stored rotation and
/// position.  Only bone 0 (the root) stores its rotation as meant.
fn pose(index: usize, rotation: Quat, position: Vec3, basis: &Basis) -> Transform {
    let q = if rotation.length_squared() > 1e-12 { rotation.normalize() } else { Quat::IDENTITY };
    let q = if index == 0 { q } else { q.conjugate() };
    Transform { translation: basis.point(position), rotation: basis.rotation(q), scale: Vec3::ONE }
}

fn rig_of(bones: &[RawBone], basis: &Basis) -> Result<Rig> {
    Rig::from_local(
        bones
            .iter()
            .enumerate()
            .map(|(i, b)| Bone {
                name: b.name.clone(),
                // Bone 0 names itself (or -1) as its parent.
                parent: usize::try_from(b.parent).ok().filter(|&p| p != i),
                local: pose(i, b.rotation, b.position, basis),
            })
            .collect(),
    )
}

//endregion

/*┌─────────────────╮
| Meshes          |
└─────────────────╯*/
//region Meshes

struct Wedge {
    point: u32,
    uv: Vec2,
}

pub fn load_psk(path: &Path, bytes: &[u8], limits: &Limits) -> Result<Scene> {
    let basis = Basis::unreal();
    let mut budget = Budget::new(bytes.len() as u64);
    let chunks = chunks(bytes)?;
    if chunks.first().is_none_or(|c| c.name != "ACTRHEAD") {
        return Err(LoadError::NotMine);
    }
    let find = |prefix: &str| chunks.iter().find(|c| c.name.starts_with(prefix));

    // 1. Points, wedges (a point with texture coordinates) and faces.
    let mut points = Vec::new();
    if let Some(c) = find("PNTS") {
        budget.take(c.count, 12, "points")?;
        for mut r in c.records(12)? {
            points.push(r.vec3("a point")?);
        }
    }
    let wide_points = points.len() > 65_536;
    let mut wedges = Vec::new();
    if let Some(c) = find("VTXW") {
        budget.take(c.count, 16, "wedges")?;
        for mut r in c.records(16)? {
            let raw = r.u32("a wedge")?;
            let point = if wide_points { raw } else { raw & 0xFFFF };
            let uv = r.vec2("a wedge")?;
            wedges.push(Wedge { point, uv });
        }
    }
    let mut faces: Vec<([u32; 3], u8)> = Vec::new();
    for c in chunks.iter().filter(|c| c.name.starts_with("FACE")) {
        let wide = c.name == "FACE3200";
        budget.take(c.count, 16, "faces")?;
        for mut r in c.records(if wide { 18 } else { 12 })? {
            let w = if wide {
                [r.u32("a face")?, r.u32("a face")?, r.u32("a face")?]
            } else {
                [r.u16("a face")? as u32, r.u16("a face")? as u32, r.u16("a face")? as u32]
            };
            faces.push((w, r.u8("a face")?));
        }
    }
    if points.is_empty() || wedges.is_empty() {
        return Err(LoadError::invalid("PSK: no points or wedges"));
    }

    // 2. The skeleton and each point's weights.
    let raw_bones = find("REFSKEL").map(read_bones).transpose()?.unwrap_or_default();
    let rig = rig_of(&raw_bones, &basis)?;
    let mut influences: Vec<Vec<(u32, f32)>> = Vec::new();
    if let Some(c) = find("RAWW").filter(|_| !rig.is_empty()) {
        budget.take(points.len(), 32, "weights")?;
        influences = vec![Vec::new(); points.len()];
        for mut r in c.records(12)? {
            let w = r.f32("a weight")?;
            let (point, bone) = (r.i32("a weight")?, r.i32("a weight")?);
            let (Ok(point), Ok(bone)) = (usize::try_from(point), u32::try_from(bone)) else { continue };
            if let Some(list) = influences.get_mut(point).filter(|l| (bone as usize) < rig.len() && l.len() < 64) {
                list.push((bone, w));
            }
        }
    }

    // 3. Extras of `.pskx`: more UV sets and colours per wedge, normals
    //    per point, morph targets.
    let mut extra_uvs: Vec<Vec<Vec2>> = Vec::new();
    for c in chunks.iter().filter(|c| c.name.starts_with("EXTRAUV")) {
        budget.take(c.count, 8, "texture coordinates")?;
        extra_uvs.push(c.records(8)?.map(|mut r| r.vec2("a UV").unwrap_or_default()).collect());
    }
    // Vertex colours are counted, not drawn: Unreal materials mostly use
    // them as masks (blending layers, wind, damage), which drawn as colour
    // paint characters pink and green.
    let colors = find("VERTEXCOLOR").map_or(0, |c| c.count);
    let normals: Vec<Vec3> = match find("VTXNORMS") {
        Some(c) => {
            budget.take(c.count, 12, "normals")?;
            c.records(12)?.map(|mut r| r.vec3("a normal").unwrap_or_default()).collect()
        }
        None => Vec::new(),
    };

    // 4. One vertex per wedge, in Slerp's coordinates.
    let mut all = Primitive::default();
    budget.take(wedges.len(), 96, "vertices")?;
    let mut list = Vec::new();
    for (i, w) in wedges.iter().enumerate() {
        let p = points.get(w.point as usize).copied().unwrap_or(Vec3::ZERO);
        all.positions.push(basis.point(p));
        all.uv0.push(w.uv);
        if let Some(uvs) = extra_uvs.first() {
            all.uv1.push(uvs.get(i).copied().unwrap_or_default());
        }
        if normals.len() == points.len() {
            all.normals.push(basis.dir(normals[(w.point as usize).min(normals.len() - 1)]).normalize_or(Vec3::Y));
        }
        if !influences.is_empty() {
            list.clear();
            list.extend_from_slice(influences.get(w.point as usize).map_or(&[][..], |l| l.as_slice()));
            let (j, wt) = strongest_four(&mut list, 0);
            all.joints.push(j);
            all.weights.push(wt);
        }
    }
    if normals.len() != points.len() && !normals.is_empty() {
        all.normals.clear();
    }

    // 5. A primitive per material, faces in file order (the mirror to
    //    right-handed space makes Unreal's clockwise faces
    //    counter-clockwise).
    let materials = match find("MATT") {
        Some(c) => c.records(88)?.map(|mut r| fixed_name(r.bytes(64, "a material").unwrap_or(&[]), latin1)).collect(),
        None => Vec::new(),
    };
    let material_count = materials.len().max(1);
    let mut by_material: Vec<Vec<u32>> = vec![Vec::new(); material_count];
    let flip = basis.flip_winding(false);
    for &(w, m) in &faces {
        if w.iter().any(|&i| i as usize >= wedges.len()) {
            continue;
        }
        let slot = (m as usize).min(material_count - 1);
        by_material[slot].extend(if flip { [w[0], w[2], w[1]] } else { w });
    }
    let triangles: usize = by_material.iter().map(|i| i.len() / 3).sum();
    if triangles as u64 > limits.max_triangles {
        return Err(LoadError::TooLarge(format!("{triangles} triangles")));
    }

    // 6. Morph targets, as sparse point offsets carried to the wedges that
    //    use each point.
    let morphs = read_morphs(&chunks, &basis, &mut budget)?;
    let mut wedges_of_point: Vec<Vec<u32>> = Vec::new();
    if !morphs.is_empty() {
        budget.take(wedges.len(), 8, "morph targets")?;
        wedges_of_point = vec![Vec::new(); points.len()];
        for (i, w) in wedges.iter().enumerate() {
            if let Some(list) = wedges_of_point.get_mut(w.point as usize) {
                list.push(i as u32);
            }
        }
    }

    let mut scene = Scene::default();
    let base = rig.add_nodes(&mut scene);
    let skinned = !rig.is_empty() && !all.joints.is_empty();
    if skinned {
        scene.skins.push(rig.skin(base, "skeleton"));
    }
    let mut textures = Textures::new(path, limits);
    let mut mesh = Mesh {
        name: path.file_stem().map_or("mesh".into(), |s| s.to_string_lossy().into_owned()),
        primitives: Vec::new(),
        target_names: morphs.iter().map(|m| m.0.clone()).collect(),
    };
    for (mi, indices) in by_material.iter().enumerate() {
        if indices.is_empty() {
            continue;
        }
        let (mut prim, new_of_old) = crate::mesh::gather(&all, indices);
        for (_, offsets) in &morphs {
            budget.take(offsets.len(), 16, "morph targets")?;
            let wedge_offsets: Vec<(u32, Vec3)> = offsets
                .iter()
                .flat_map(|&(point, d)| wedges_of_point.get(point as usize).into_iter().flatten().map(move |&w| (w, d)))
                .collect();
            let target = crate::mesh::sparse_target(&wedge_offsets, &new_of_old, prim.positions.len());
            if !target.positions.is_empty() {
                budget.take(target.positions.len(), 12, "morph targets")?;
            }
            prim.targets.push(target);
        }
        prim.ensure_normals();
        prim.material = Some(scene.materials.len());
        let name = materials.get(mi).cloned().unwrap_or_else(|| format!("material {mi}"));
        scene.materials.push(material(&name, &mut textures));
        mesh.primitives.push(prim);
    }
    scene.meshes.push(mesh);
    let mut node = Node::new(scene.meshes[0].name.clone());
    node.mesh = Some(0);
    node.skin = skinned.then_some(0);
    scene.nodes.push(node);
    scene.roots.push(scene.nodes.len() - 1);
    scene.textures = std::mem::take(&mut textures.list);
    scene.info.warnings.append(&mut textures.warnings);

    let pskx = extra_uvs.len() + colors + normals.len() + morphs.len() > 0;
    if colors > 0 {
        scene.info.details.push(("Vertex colours".into(), "not drawn (Unreal uses them as material masks)".into()));
    }
    scene.info.format = if pskx { "Unreal ActorX mesh (PSKX)" } else { "Unreal ActorX mesh (PSK)" }.into();
    scene.info.details.push(("Points".into(), points.len().to_string()));
    if !extra_uvs.is_empty() {
        scene.info.details.push(("UV sets".into(), (1 + extra_uvs.len()).to_string()));
    }
    if extra_uvs.len() > 1 {
        scene.info.warnings.push(format!("{} extra UV sets; only the first is kept", extra_uvs.len()));
    }
    if !morphs.is_empty() {
        scene.info.details.push(("Morph targets".into(), morphs.len().to_string()));
    }
    crate::describe_rig(&mut scene, &rig);
    if limits.animations && limits.neighbours && skinned {
        add_neighbour_psa(path, &rig, base, &mut scene, &mut budget);
    }
    Ok(scene)
}

/// A morph target's name and its point offsets.
type Morph = (String, Vec<(u32, Vec3)>);

/// Morph targets: names, then each one's point offsets in turn.
fn read_morphs(chunks: &[Chunk], basis: &Basis, budget: &mut Budget) -> Result<Vec<Morph>> {
    let (Some(info), Some(data)) =
        (chunks.iter().find(|c| c.name == "MRPHINFO"), chunks.iter().find(|c| c.name == "MRPHDATA"))
    else {
        return Ok(Vec::new());
    };
    budget.take(data.count, 16, "morph targets")?;
    let mut deltas = data.records(28)?.map(|mut r| {
        let d = r.vec3("a morph").unwrap_or_default();
        let _tangent = r.vec3("a morph");
        let point = r.i32("a morph").unwrap_or(-1);
        (u32::try_from(point).unwrap_or(u32::MAX), basis.point(Vec3::new(d.x, -d.y, d.z)))
    });
    let mut out = Vec::new();
    for mut r in info.records(68)? {
        let name = fixed_name(r.bytes(64, "a morph")?, latin1);
        let n = usize::try_from(r.i32("a morph")?).unwrap_or(0).min(data.count);
        out.push((name, deltas.by_ref().take(n).collect()));
    }
    Ok(out)
}

/// A material for an Unreal material name, with a picture of that name
/// when one is beside the mesh.
fn material(name: &str, textures: &mut Textures) -> Material {
    let mut m = Material { name: name.to_string(), base_color: Vec4::ONE, roughness: 0.7, ..Default::default() };
    if textures.dir().is_none() || name.is_empty() {
        return m;
    }
    // UE Viewer names pictures after textures, not materials; the usual
    // names for a material `M_Body` are `T_Body_D`, `Body_D`,
    // `M_Body_BaseColor` and the like.
    let stem = ["M_", "MI_", "Mat_", "MAT_"].iter().find_map(|p| name.strip_prefix(p)).unwrap_or(name);
    let mut names = Vec::new();
    for n in [name.to_string(), stem.to_string(), format!("T_{stem}")] {
        for suffix in ["", "_D", "_BaseColor", "_Diffuse", "_Albedo", "_BC", "_C"] {
            names.push(format!("{n}{suffix}"));
        }
    }
    let colour = names.iter().find_map(|n| textures.locate(&format!("{n}.png"), true));
    match colour {
        Some(p) => m.base_color_texture = textures.load_path(&p).map(TexRef::new),
        None => textures.warn(format!("no picture found for material {name}")),
    }
    let normal = ["_N", "_Normal", "_NRM"].iter().find_map(|s| {
        [name, stem, &format!("T_{stem}")].iter().find_map(|n| textures.locate(&format!("{n}{s}.png"), true))
    });
    if let Some(p) = normal {
        m.normal_texture = textures.load_path(&p).map(TexRef::new);
    }
    m
}

//endregion

/*┌─────────────────╮
| Animations      |
└─────────────────╯*/
//region Animations

struct Sequence {
    name: String,
    fps: f32,
    first: usize,
    frames: usize,
}

struct Psa<'a> {
    bones: Vec<RawBone>,
    sequences: Vec<Sequence>,
    keys: Option<Chunk<'a>>,
    scales: Option<Chunk<'a>>,
}

fn parse_psa(bytes: &[u8]) -> Result<Psa<'_>> {
    let mut chunks = chunks(bytes)?;
    if chunks.first().is_none_or(|c| c.name != "ANIMHEAD") {
        return Err(LoadError::NotMine);
    }
    let bones = chunks.iter().find(|c| c.name == "BONENAMES").map(read_bones).transpose()?.unwrap_or_default();
    let mut sequences = Vec::new();
    if let Some(c) = chunks.iter().find(|c| c.name == "ANIMINFO") {
        for mut r in c.records(168)? {
            let name = fixed_name(r.bytes(64, "a sequence")?, latin1);
            r.skip(64 + 4 * 6, "a sequence")?;
            let fps = r.f32("a sequence")?;
            let _start_bone = r.i32("a sequence")?;
            let first = r.i32("a sequence")?.max(0) as usize;
            let frames = r.i32("a sequence")?.max(0) as usize;
            sequences.push(Sequence { name, fps, first, frames });
        }
    }
    // CUE4Parse once wrote each sequence's frame count as its first frame;
    // sequences are stored in order, so their starts can be rebuilt.
    if sequences.first().is_some_and(|s| s.first == s.frames && s.frames > 0) {
        let mut start = 0;
        for s in &mut sequences {
            s.first = start;
            start += s.frames;
        }
    }
    let take = |name: &str, chunks: &mut Vec<Chunk<'_>>| chunks.iter().position(|c| c.name == name);
    let keys = take("ANIMKEYS", &mut chunks).map(|i| chunks.remove(i));
    let scales = take("SCALEKEYS", &mut chunks).map(|i| chunks.remove(i)).filter(|c| c.count > 0);
    Ok(Psa { bones, sequences, keys, scales })
}

impl Psa<'_> {
    /// The local poses of one sequence's frames, bone after bone, frame
    /// after frame.
    fn poses(&self, seq: &Sequence, basis: &Basis, budget: &mut Budget) -> Result<Vec<Transform>> {
        let bones = self.bones.len();
        let keys = self.keys.as_ref().ok_or_else(|| LoadError::invalid("PSA: no keys"))?;
        if keys.size < 32 {
            return Err(LoadError::invalid("PSA: keys are too small"));
        }
        let start = seq.first.checked_mul(bones).ok_or_else(|| truncated("the keys"))?;
        let count = seq.frames.checked_mul(bones).ok_or_else(|| truncated("the keys"))?;
        if start.checked_add(count).is_none_or(|end| end > keys.count) {
            return Err(LoadError::invalid(format!("PSA: sequence {} runs past the keys", seq.name)));
        }
        budget.take(count, 40, "poses")?;
        let mut out = Vec::with_capacity(count);
        for k in start..start + count {
            let mut r = Cursor::new(&keys.data[k * keys.size..(k + 1) * keys.size]);
            let position = r.vec3("a key")?;
            let rotation = r.quat("a key")?;
            let mut t = pose(k % bones.max(1), rotation, position, basis);
            if let Some(s) = &self.scales
                && s.size >= 16
                && k < s.count
            {
                let mut r = Cursor::new(&s.data[k * s.size..(k + 1) * s.size]);
                t.scale = basis.scale(r.vec3("a scale key")?);
            }
            out.push(t);
        }
        Ok(out)
    }
}

/// An animation set opened on its own: its skeleton in the first frame's
/// pose (a PSA carries no rest pose), and its sequences as clips.
pub fn load_psa(_path: &Path, bytes: &[u8], limits: &Limits) -> Result<Scene> {
    let basis = Basis::unreal();
    let mut budget = Budget::new(bytes.len() as u64);
    let psa = parse_psa(bytes)?;
    if psa.bones.is_empty() {
        return Err(LoadError::invalid("PSA: no bones"));
    }
    let rig = rig_of(&psa.bones, &basis)?;
    let mut scene = Scene::default();
    let base = rig.add_nodes(&mut scene);
    if limits.animations {
        let rest = rig.rest();
        for seq in &psa.sequences {
            match psa.poses(seq, &basis, &mut budget) {
                Ok(poses) => scene
                    .animations
                    .push(sampled_clip(&seq.name, seq.fps, rig.len(), &poses, &rest, |b| Some(base + b))),
                Err(e @ LoadError::TooLarge(_)) => return Err(e),
                Err(e) => scene.info.warnings.push(e.to_string()),
            }
        }
        unique_names(&mut scene.animations);
    }
    scene.info.format = "Unreal ActorX animation (PSA)".into();
    scene.info.details.push(("Sequences".into(), psa.sequences.len().to_string()));
    crate::describe_rig(&mut scene, &rig);
    Ok(scene)
}

/// Adds the sequences of the `.psa` files beside a mesh whose bones are the
/// mesh's: the one with the mesh's name first, then the others.
fn add_neighbour_psa(path: &Path, rig: &Rig, base: usize, scene: &mut Scene, budget: &mut Budget) {
    let Some(dir) = path.parent() else { return };
    let stem = path.file_stem().map(|s| s.to_string_lossy().to_lowercase()).unwrap_or_default();
    let mut files = siblings(dir, &["psa"], MAX_PSA_FILES);
    files.sort_by_key(|f| f.file_stem().map(|s| s.to_string_lossy().to_lowercase()) != Some(stem.clone()));
    let basis = Basis::unreal();
    let mut read = 0u64;
    let mut clips = Vec::new();
    for file in files {
        let Some(bytes) = read_limited(&file, MAX_PSA_BYTES.saturating_sub(read)) else { continue };
        read += bytes.len() as u64;
        let Ok(psa) = parse_psa(&bytes) else {
            scene.info.warnings.push(format!("{} couldn't be read", file.display()));
            continue;
        };
        let map: Vec<Option<usize>> =
            psa.bones.iter().map(|b| rig.bones.iter().position(|r| r.name.eq_ignore_ascii_case(&b.name))).collect();
        let matched = map.iter().filter(|m| m.is_some()).count();
        if psa.bones.is_empty() || matched * 10 < psa.bones.len() * 6 {
            continue;
        }
        let rest: Vec<Transform> = map.iter().map(|m| m.map_or(Transform::IDENTITY, |b| rig.bones[b].local)).collect();
        for seq in &psa.sequences {
            match psa.poses(seq, &basis, budget) {
                Ok(poses) => clips.push(sampled_clip(&seq.name, seq.fps, psa.bones.len(), &poses, &rest, |b| {
                    map[b].map(|r| base + r)
                })),
                Err(LoadError::TooLarge(_)) => {
                    scene.info.warnings.push("more animations than memory allows; some were left out".into());
                    break;
                }
                Err(e) => scene.info.warnings.push(format!("{}: {e}", file.display())),
            }
        }
    }
    unique_names(&mut clips);
    scene.animations.extend(clips);
}

//endregion

#[cfg(test)]
pub mod tests {
    use super::*;

    /// Writes ActorX chunks for tests.
    pub struct Writer(pub Vec<u8>);

    impl Writer {
        pub fn chunk(&mut self, name: &str, size: usize, records: &[Vec<u8>]) {
            let mut n = [0u8; 20];
            n[..name.len()].copy_from_slice(name.as_bytes());
            self.0.extend_from_slice(&n);
            self.0.extend_from_slice(&1999801i32.to_le_bytes());
            self.0.extend_from_slice(&(size as i32).to_le_bytes());
            self.0.extend_from_slice(&(records.len() as i32).to_le_bytes());
            for r in records {
                assert_eq!(r.len(), size, "{name}");
                self.0.extend_from_slice(r);
            }
        }
    }

    pub fn floats(v: &[f32]) -> Vec<u8> {
        v.iter().flat_map(|f| f.to_le_bytes()).collect()
    }

    pub fn bone(name: &str, parent: i32, rotation: Quat, position: Vec3) -> Vec<u8> {
        let mut b = vec![0u8; 64];
        b[..name.len()].copy_from_slice(name.as_bytes());
        b.extend_from_slice(&0i32.to_le_bytes());
        b.extend_from_slice(&0i32.to_le_bytes());
        b.extend_from_slice(&parent.to_le_bytes());
        b.extend(floats(&rotation.to_array()));
        b.extend(floats(&position.to_array()));
        b.extend(floats(&[1.0, 1.0, 1.0, 1.0]));
        b
    }

    /// A triangle on two bones: the root at the origin and a child 100 cm
    /// up (Unreal's Z), turned a quarter turn about Z.
    pub fn triangle_psk() -> Vec<u8> {
        let mut w = Writer(Vec::new());
        w.chunk("ACTRHEAD", 0, &[]);
        let pts = [[0.0, 0.0, 0.0], [100.0, 0.0, 0.0], [0.0, 100.0, 0.0]];
        w.chunk("PNTS0000", 12, &pts.iter().map(|p| floats(p)).collect::<Vec<_>>());
        let wedge = |p: u32, u: f32, v: f32| {
            let mut b = (p | 0xABCD_0000).to_le_bytes().to_vec();
            b.extend(floats(&[u, v]));
            b.extend_from_slice(&[0, 0, 0, 0]);
            b
        };
        w.chunk("VTXW0000", 16, &[wedge(0, 0.0, 0.0), wedge(1, 1.0, 0.0), wedge(2, 0.0, 1.0)]);
        let mut face = Vec::new();
        for i in [0u16, 1, 2] {
            face.extend_from_slice(&i.to_le_bytes());
        }
        face.extend_from_slice(&[0, 0]);
        face.extend_from_slice(&1u32.to_le_bytes());
        w.chunk("FACE0000", 12, &[face]);
        let mut mat = vec![0u8; 88];
        mat[..4].copy_from_slice(b"Skin");
        w.chunk("MATT0000", 88, &[mat]);
        let turn = Quat::from_rotation_z(std::f32::consts::FRAC_PI_2);
        w.chunk(
            "REFSKELT",
            120,
            &[
                bone("root", 0, Quat::IDENTITY, Vec3::ZERO),
                bone("top", 0, turn.conjugate(), Vec3::new(0.0, 0.0, 100.0)),
            ],
        );
        let weight = |w: f32, p: i32, b: i32| {
            let mut v = floats(&[w]);
            v.extend_from_slice(&p.to_le_bytes());
            v.extend_from_slice(&b.to_le_bytes());
            v
        };
        w.chunk("RAWWEIGHTS", 12, &[weight(1.0, 0, 0), weight(1.0, 1, 0), weight(0.5, 2, 0), weight(0.5, 2, 1)]);
        w.0
    }

    #[test]
    fn triangle_mesh() {
        let limits = Limits { neighbours: false, ..Limits::viewer() };
        let scene = load_psk(Path::new("t.psk"), &triangle_psk(), &limits).unwrap();
        let p = &scene.meshes[0].primitives[0];
        // Unreal's X stays X, its Y (right) becomes +Z, centimetres become
        // metres; the junk in the wedges' high bits is ignored.
        assert_eq!(p.positions[1], Vec3::new(1.0, 0.0, 0.0));
        assert_eq!(p.positions[2], Vec3::new(0.0, 0.0, 1.0));
        assert_eq!(p.indices, vec![0, 1, 2]);
        assert_eq!(p.joints[2][..2], [0, 1]);
        // The child is 1 m up and turned a quarter turn about up, which a
        // mirror reverses.
        let top = &scene.nodes[1];
        assert!((top.transform.translation - Vec3::new(0.0, 1.0, 0.0)).length() < 1e-6);
        let expected = Quat::from_rotation_y(-std::f32::consts::FRAC_PI_2);
        assert!(top.transform.rotation.angle_between(expected) < 1e-5, "{:?}", top.transform.rotation);
        assert_eq!(scene.materials[0].name, "Skin");
    }

    #[test]
    fn animation_sets() {
        let mut w = Writer(Vec::new());
        w.chunk("ANIMHEAD", 0, &[]);
        w.chunk("BONENAMES", 120, &[bone("root", -1, Quat::IDENTITY, Vec3::ZERO)]);
        let seq = |name: &str, first: i32, frames: i32| {
            let mut b = vec![0u8; 168];
            b[..name.len()].copy_from_slice(name.as_bytes());
            b[152..156].copy_from_slice(&10.0f32.to_le_bytes());
            b[160..164].copy_from_slice(&first.to_le_bytes());
            b[164..168].copy_from_slice(&frames.to_le_bytes());
            b
        };
        // Written by an old CUE4Parse: first frame = frame count.
        w.chunk("ANIMINFO", 168, &[seq("walk", 2, 2), seq("run", 2, 1)]);
        let key = |x: f32| {
            let mut b = floats(&[x, 0.0, 0.0]);
            b.extend(floats(&[0.0, 0.0, 0.0, 1.0, 0.1]));
            b
        };
        w.chunk("ANIMKEYS", 32, &[key(0.0), key(100.0), key(300.0)]);
        let scene = load_psa(Path::new("a.psa"), &w.0, &Limits::viewer()).unwrap();
        assert_eq!(scene.animations.len(), 2);
        let walk = &scene.animations[0];
        assert_eq!((walk.name.as_str(), walk.duration), ("walk", 0.1));
        assert_eq!(walk.channels[0].values, vec![0.0, 0.0, 0.0, 1.0, 0.0, 0.0]);
        let run = &scene.animations[1];
        assert_eq!(run.channels[0].values, vec![3.0, 0.0, 0.0]);
    }

    #[test]
    fn damaged_chunks() {
        let good = triangle_psk();
        for len in 0..good.len() {
            let _ = load_psk(Path::new("t.psk"), &good[..len], &Limits::viewer());
        }
        // A record count that overflows, and a negative size.
        let mut w = Writer(Vec::new());
        w.chunk("ACTRHEAD", 0, &[]);
        let mut bad = w.0.clone();
        bad.extend_from_slice(b"PNTS0000\0\0\0\0\0\0\0\0\0\0\0\0");
        bad.extend_from_slice(&0i32.to_le_bytes());
        bad.extend_from_slice(&i32::MAX.to_le_bytes());
        bad.extend_from_slice(&i32::MAX.to_le_bytes());
        assert!(load_psk(Path::new("t.psk"), &bad, &Limits::viewer()).is_err());
        let mut neg = w.0;
        neg.extend_from_slice(b"PNTS0000\0\0\0\0\0\0\0\0\0\0\0\0");
        neg.extend_from_slice(&0i32.to_le_bytes());
        neg.extend_from_slice(&(-12i32).to_le_bytes());
        neg.extend_from_slice(&1i32.to_le_bytes());
        assert!(load_psk(Path::new("t.psk"), &neg, &Limits::viewer()).is_err());
    }
}
