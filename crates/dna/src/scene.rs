//! Turning DNA data into Slerp's scene: a joint per DNA joint, skinned
//! meshes at one level of detail, blend shapes as morph targets, and
//! materials from the export's maps or plain clay.
//!
//! Every node, whatever the level of detail, is created in the same order
//! (joints first, then meshes), so animation clips added to one level's
//! scene play unchanged on another's.

use crate::extract::{DnaData, MeshData};
use crate::maps::Maps;
use glam::{EulerRot, Mat4, Quat, Vec2, Vec3, Vec4};
use rayon::prelude::*;
use slerp_scene::{
    AlphaMode, Info, LoadError, Material, Mesh, MorphTarget, Node, Primitive, Scene, Skin, TexRef, Transform,
};
use std::collections::HashMap;
use std::sync::Arc;

/*┌─────────────────╮
| Units           |
└─────────────────╯*/
//region Units

/// How the file measures, after openriglogic turned its axes into
/// Slerp's (see `extract`): lengths in centimetres or metres, angles in
/// degrees or radians, and the order Euler angles apply in.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Units {
    /// Multiplies a file length into metres.
    pub metres: f32,
    /// Multiplies a file angle into radians.
    pub radians: f32,
    /// openriglogic's rotation sequence: 0 is xyz (X first, as Maya does).
    pub sequence: u32,
}

impl Units {
    pub fn of(d: &DnaData) -> Units {
        let c = |i: usize| d.conventions.get(i).copied().unwrap_or(0);
        Units {
            metres: if c(0) == 1 { 1.0 } else { 0.01 },
            radians: if c(1) == 1 { 1.0 } else { std::f32::consts::PI / 180.0 },
            sequence: c(5),
        }
    }

    /// The rotation of Euler angles in file units, applied in the file's
    /// order: for xyz, about X first, so the matrix is Rz Ry Rx.
    pub fn rotation(&self, r: Vec3) -> Quat {
        let (x, y, z) = (r.x * self.radians, r.y * self.radians, r.z * self.radians);
        let q = match self.sequence {
            1 => Quat::from_euler(EulerRot::YZX, y, z, x),
            2 => Quat::from_euler(EulerRot::ZXY, z, x, y),
            3 => Quat::from_euler(EulerRot::XZY, x, z, y),
            4 => Quat::from_euler(EulerRot::YXZ, y, x, z),
            5 => Quat::from_euler(EulerRot::XYZ, x, y, z),
            _ => Quat::from_euler(EulerRot::ZYX, z, y, x),
        };
        if q.is_finite() { q.normalize() } else { Quat::IDENTITY }
    }
}

//endregion

/*┌─────────────────╮
| Options         |
└─────────────────╯*/
//region Options

/// What a built scene holds.
#[derive(Clone)]
pub(crate) struct Build<'a> {
    pub lod: u16,
    /// Blend shapes as morph targets.
    pub blend_shapes: bool,
    /// Keep the meshes that need Unreal's own shaders (saliva, the inner
    /// eye corners, the eye's wet film and lid edge), hidden.
    pub helpers: bool,
    /// Textures found beside the file; clay and painted eyes without.
    pub maps: Option<&'a Maps>,
    pub max_triangles: u64,
    /// The file opened was the head (perhaps with the body joined), so the
    /// scene is framed on the face.
    pub opened_head: bool,
}

/// A built character and how its nodes relate to the DNA files.
pub(crate) struct Built {
    pub scene: Scene,
    /// The node of each head (or lone) DNA joint.
    pub head_joints: Vec<usize>,
    /// The node of each body DNA joint, when a body was loaded with a head.
    pub body_joints: Vec<usize>,
    /// For each blend shape channel of the head, the morph targets it
    /// drives as (node, target).
    pub channel_targets: Vec<Vec<(usize, usize)>>,
    /// Where each mesh's vertices come from in the DNA, for refitting
    /// their skin weights (see `fit`).
    pub sources: Vec<MeshSource>,
}

/// A scene mesh's origin: which file and DNA mesh, and the DNA position
/// of every vertex of each of its primitives.
pub(crate) struct MeshSource {
    pub mesh: usize,
    /// From the main file (the head), not the body.
    pub main: bool,
    pub dna_mesh: u16,
    pub positions: Vec<Vec<u32>>,
}

//endregion

/*┌─────────────────╮
| Building        |
└─────────────────╯*/
//region Building

/// Builds a scene from a DNA file and, optionally, the body that goes with
/// a head.  `main` is the file opened; `body` joins it under the same
/// skeleton, its joints merged with the head's by name.
pub(crate) fn build(main: &DnaData, body: Option<&DnaData>, opts: &Build) -> Result<Built, LoadError> {
    let mut scene = Scene::default();
    let mut warnings = Vec::new();

    // 1. Skeleton: the body's joints first when there is one, then the main
    //    file's, reusing body joints of the same name.
    let mut by_name: HashMap<String, usize> = HashMap::new();
    let body_joints = match body {
        Some(b) => add_joints(&mut scene, b, &mut by_name),
        None => Vec::new(),
    };
    let head_joints = add_joints(&mut scene, main, &mut by_name);
    scene.compute_roots();
    let rest = slerp_scene::Pose::rest(&scene);
    if body.is_some() {
        // The head's neck and shoulders, now the body's joints, should sit
        // where the head's own skeleton puts them.
        let own = world_matrices(main);
        let apart = head_joints
            .iter()
            .zip(&own)
            .filter(|&(&n, m)| (rest.world[n].w_axis - m.w_axis).truncate().length() > 1e-3)
            .count();
        if apart > 0 {
            warnings
                .push(format!("{apart} joints of the head sit more than 1 mm from the body's joints of the same name"));
        }
    }

    // 2. Meshes at the chosen level.
    let mut channel_targets = vec![Vec::new(); main.channels.len()];
    let mut sources = Vec::new();
    let mut triangles = 0u64;
    let mut parts: Vec<(&DnaData, &[usize])> = vec![(main, &head_joints)];
    if let Some(b) = body {
        parts.push((b, &body_joints));
    }
    for (dna, joint_nodes) in parts {
        let units = Units::of(dna);
        let lod = (opts.lod as usize).min(dna.lod_meshes.len().saturating_sub(1));
        let Some(list) = dna.lod_meshes.get(lod) else { continue };
        let inverse_bind: Vec<Mat4> = joint_nodes.iter().map(|&n| rest.world[n].inverse()).collect();
        let skin = scene.skins.len();
        scene.skins.push(Skin {
            name: dna.name.clone(),
            joints: joint_nodes.to_vec(),
            inverse_bind,
            skeleton: joint_nodes.first().copied(),
        });
        // The meshes to build, then built side by side (a head's blend
        // shapes make this the longest step), then added in order.
        let mut wanted = Vec::new();
        for &m in list {
            let Some((_, data)) = dna.meshes.iter().find(|(i, _)| *i == m) else { continue };
            let name = dna.mesh_names.get(m as usize).cloned().unwrap_or_default();
            let base = base_name(&name);
            let helper = HELPERS.contains(&base.as_str());
            if helper && !opts.helpers {
                continue;
            }
            // Eyelashes are cards cut out by their texture; drawn plain they
            // would be dark slabs over the eyes.
            if base == "eyelashes" && opts.maps.is_none_or(|m| m.get("eyelashes").is_none()) {
                continue;
            }
            wanted.push((m, data, name, base, helper));
        }
        let blend = opts.blend_shapes && std::ptr::eq(dna, main);
        let built: Vec<Result<BuiltMesh, String>> = wanted
            .par_iter()
            .map(|(_, data, _, base, _)| mesh(data, joint_nodes.len(), &units, blend, base == "teeth"))
            .collect();
        for ((m, _, name, base, helper), built) in wanted.into_iter().zip(built) {
            let built = match built {
                Ok(b) => b,
                Err(e) => {
                    warnings.push(format!("{name}: {e}"));
                    continue;
                }
            };
            triangles += built.triangles;
            if triangles > opts.max_triangles {
                return Err(LoadError::TooLarge(format!("more than {} triangles", opts.max_triangles)));
            }
            let material = material(&mut scene, &base, opts.maps);
            let mut mesh = Mesh { name: name.clone(), primitives: built.primitives, target_names: Vec::new() };
            for p in &mut mesh.primitives {
                p.material = Some(material);
            }
            mesh.target_names =
                built.channels.iter().map(|&c| dna.channels.get(c as usize).cloned().unwrap_or_default()).collect();
            let node = scene.add_mesh_node(mesh);
            sources.push(MeshSource {
                mesh: scene.meshes.len() - 1,
                main: std::ptr::eq(dna, main),
                dna_mesh: m,
                positions: built.sources,
            });
            scene.nodes[node].skin = Some(skin);
            scene.nodes[node].visible = !helper;
            for (t, &c) in built.channels.iter().enumerate() {
                if let Some(slots) = channel_targets.get_mut(c as usize) {
                    slots.push((node, t));
                }
            }
        }
    }
    if scene.meshes.is_empty() {
        return Err(LoadError::invalid("no meshes at this level of detail"));
    }

    // 3. Facts for the inspector.
    let format = match (opts.opened_head && is_head(main), body.is_some()) {
        (true, true) => HEAD_WITH_BODY_FORMAT,
        (true, false) => HEAD_FORMAT,
        (false, true) => "MetaHuman DNA (body and head)",
        (false, false) => "MetaHuman DNA (body)",
    };
    // The level shown, as the file numbers it: a file read at one level
    // only knows that one, but its meshes' names still say which it is.
    let level = scene.meshes.first().and_then(|m| lod_of(&m.name)).unwrap_or(opts.lod);
    let levels = if main.lod_count > 1 { format!("{level} of 0 to {}", main.lod_count - 1) } else { level.to_string() };
    scene.info = Info {
        format: format.to_string(),
        generator: None,
        warnings,
        details: vec![
            ("Level of detail".into(), levels),
            ("Joints".into(), by_name.len().to_string()),
            ("Rig controls".into(), (main.controls.len() + body.map_or(0, |b| b.controls.len())).to_string()),
            ("Blend shape channels".into(), main.channels.len().to_string()),
            ("Animated maps".into(), main.animated_maps.to_string()),
        ],
        neighbours: Vec::new(),
    };
    Ok(Built { scene, head_joints, body_joints, channel_targets, sources })
}

/// The formats a head opened alone or with its body reports, which also
/// mark the scenes `portrait` frames.  A body opened with its head is
/// framed whole instead.
pub(crate) const HEAD_FORMAT: &str = "MetaHuman DNA (head)";
pub(crate) const HEAD_WITH_BODY_FORMAT: &str = "MetaHuman DNA (head and body)";

/// Whether a DNA file is a head: it has face controls or a head mesh.
pub(crate) fn is_head(d: &DnaData) -> bool {
    !d.channels.is_empty() || d.mesh_names.iter().any(|n| base_name(n) == "head")
}

/// Meshes that only make sense with Unreal's shaders.
const HELPERS: [&str; 4] = ["saliva", "cartilage", "eyeshell", "eyeEdge"];

/// The level in a mesh name such as "head_lod3_mesh".
fn lod_of(name: &str) -> Option<u16> {
    let rest = &name[name.find("_lod")? + 4..];
    rest[..rest.find(|c: char| !c.is_ascii_digit()).unwrap_or(rest.len())].parse().ok()
}

/// "head_lod0_mesh" without its level suffix: "head".
pub(crate) fn base_name(name: &str) -> String {
    match name.find("_lod") {
        Some(at) => name[..at].to_string(),
        None => name.strip_suffix("_mesh").unwrap_or(name).to_string(),
    }
}

/// Each joint's local transform in its DNA, in metres.
fn joint_transform(d: &DnaData, units: &Units, i: usize) -> Transform {
    let (t, r) = match d.neutral_joints.get(i * 6..i * 6 + 6) {
        Some(v) => (Vec3::new(v[0], v[1], v[2]), Vec3::new(v[3], v[4], v[5])),
        None => (Vec3::ZERO, Vec3::ZERO),
    };
    Transform {
        translation: if t.is_finite() { t * units.metres } else { Vec3::ZERO },
        rotation: units.rotation(r),
        scale: Vec3::ONE,
    }
}

/// Each joint's parent in `d`, or `None` for roots.  A parent that comes
/// after its child, which openriglogic's files never have, counts as none,
/// so a damaged file can't make a cycle.
fn joint_parent(d: &DnaData, i: usize) -> Option<usize> {
    let p = *d.joint_parents.get(i)? as usize;
    (p < i).then_some(p)
}

/// The rest pose's world matrix of every joint of `d` by its own skeleton.
fn world_matrices(d: &DnaData) -> Vec<Mat4> {
    let units = Units::of(d);
    let mut out: Vec<Mat4> = Vec::with_capacity(d.joint_names.len());
    for i in 0..d.joint_names.len().min(d.joint_parents.len()) {
        let local = joint_transform(d, &units, i).matrix();
        out.push(match joint_parent(d, i) {
            Some(p) => out[p] * local,
            None => local,
        });
    }
    out
}

/// Adds a node per joint of `d` and returns their indices.  Joints whose
/// name is already in `by_name` (a head's neck and shoulders, shared with
/// the body) reuse that node.
fn add_joints(scene: &mut Scene, d: &DnaData, by_name: &mut HashMap<String, usize>) -> Vec<usize> {
    let units = Units::of(d);
    let count = d.joint_names.len().min(d.joint_parents.len());
    let mut nodes = Vec::with_capacity(count);
    for i in 0..count {
        let name = &d.joint_names[i];
        if let Some(&existing) = by_name.get(name) {
            nodes.push(existing);
            continue;
        }
        let mut node = Node::new(name.clone());
        node.transform = joint_transform(d, &units, i);
        scene.nodes.push(node);
        let index = scene.nodes.len() - 1;
        if let Some(p) = joint_parent(d, i).and_then(|p| nodes.get(p).copied()) {
            scene.nodes[p].children.push(index);
        }
        by_name.insert(name.clone(), index);
        nodes.push(index);
    }
    nodes
}

//endregion

/*┌─────────────────╮
| Meshes          |
└─────────────────╯*/
//region Meshes

/// Triangles per primitive when a mesh with blend shapes is split by
/// region: a face shape moves a few regions, and the renderer stores morph
/// deltas only for the primitives a shape moves.
const REGION_TRIANGLES: usize = 1000;

/// Blend shape offsets shorter than this (metres) are dropped.  A
/// MetaHuman shape fades out over thousands of vertices moving tens of
/// micrometres, far below what can be seen, and keeping them would make
/// every shape touch every region.
const MIN_DELTA: f32 = 3e-5;

struct BuiltMesh {
    primitives: Vec<Primitive>,
    /// The DNA position of each vertex, per primitive.
    sources: Vec<Vec<u32>>,
    /// The blend shape channel of each morph target, in target order.
    channels: Vec<u16>,
    triangles: u64,
}

/// One DNA mesh as primitives: a vertex per vertex layout (position, UV and
/// normal), triangles fanned from the faces, the four strongest skin
/// influences, and blend shape targets.
fn mesh(m: &MeshData, joints: usize, units: &Units, blend: bool, mouth: bool) -> Result<BuiltMesh, String> {
    let positions = m.positions.len() / 3;
    let uvs = m.uvs.len() / 2;
    let normals = m.normals.len() / 3;

    // 1. Vertices from layouts.  A layout pointing past the data makes the
    //    whole mesh invalid; a missing UV or normal is filled in.
    let layouts = m.layouts.len() / 3;
    let mut prim = Primitive::default();
    let mut layout_pos = Vec::with_capacity(layouts);
    for l in m.layouts.chunks_exact(3) {
        let (p, t, n) = (l[0] as usize, l[1] as usize, l[2] as usize);
        if p >= positions {
            return Err("a vertex points past the positions".into());
        }
        let pos = Vec3::from_slice(&m.positions[p * 3..p * 3 + 3]) * units.metres;
        prim.positions.push(if pos.is_finite() { pos } else { Vec3::ZERO });
        let normal = if n < normals { Vec3::from_slice(&m.normals[n * 3..n * 3 + 3]) } else { Vec3::ZERO };
        prim.normals.push(normal.try_normalize().unwrap_or(Vec3::Y));
        // DNA texture coordinates start at the bottom, glTF's at the top.
        let uv = if t < uvs { Vec2::new(m.uvs[t * 2], 1.0 - m.uvs[t * 2 + 1]) } else { Vec2::ZERO };
        prim.uv0.push(if uv.is_finite() { uv } else { Vec2::ZERO });
        layout_pos.push(p);
    }

    // 2. Triangles, fanned from each polygon.
    let mut i = 0usize;
    while i < m.faces.len() {
        let corners = m.faces[i] as usize;
        let end = i.saturating_add(1).saturating_add(corners);
        if end > m.faces.len() {
            break;
        }
        let face = &m.faces[i + 1..end];
        if corners >= 3 && face.iter().all(|&c| (c as usize) < layouts) {
            for k in 1..corners - 1 {
                prim.indices.extend_from_slice(&[face[0], face[k], face[k + 1]]);
            }
        }
        i = end;
    }
    let triangles = (prim.indices.len() / 3) as u64;
    if triangles == 0 {
        return Err("no faces".into());
    }

    // 2b. The mouth (teeth, gums and tongue) darkens toward the throat,
    //     where little light gets in, as chalk's bake makes it: full
    //     brightness at the front teeth, a sixth at the back of the tongue.
    //     Unreal's mouth shader does the same with its own occlusion.
    if mouth {
        let mut z: Vec<f32> = prim.positions.iter().map(|p| p.z).collect();
        z.sort_by(f32::total_cmp);
        let (back, front) = (z[z.len() * 3 / 100], z[z.len() * 97 / 100]);
        if front > back {
            prim.colors = prim
                .positions
                .iter()
                .map(|p| {
                    let k = 0.16 + 0.84 * ((p.z - back) / (front - back)).clamp(0.0, 1.0).powf(1.5);
                    Vec4::new(k, k, k, 1.0)
                })
                .collect();
        }
    }

    // 3. Skin weights: the four strongest of each position's influences,
    //    rescaled to sum to one.  Unreal blends up to twelve on a head;
    //    the strongest four carry nearly all of the motion.
    let skin = top_four(m, positions, joints);
    if let Some(skin) = &skin {
        prim.joints = layout_pos.iter().map(|&p| skin[p].0).collect();
        prim.weights = layout_pos.iter().map(|&p| skin[p].1).collect();
    }

    // 4. Blend shapes, then regions so each shape's deltas are stored only
    //    where it moves something.
    let mut channels = Vec::new();
    if !blend || m.targets.is_empty() {
        let sources = vec![layout_pos.iter().map(|&p| p as u32).collect()];
        return Ok(BuiltMesh { primitives: vec![prim], sources, channels, triangles });
    }
    // 4a. Offsets per DNA position, dropping the imperceptible ones.
    let rest: Vec<Vec3> = (0..positions)
        .map(|p| {
            let v = Vec3::from_slice(&m.positions[p * 3..p * 3 + 3]) * units.metres;
            if v.is_finite() { v } else { Vec3::ZERO }
        })
        .collect();
    let moves: Vec<Vec<(u32, Vec3)>> = m
        .targets
        .par_iter()
        .map(|t| {
            t.vertices
                .iter()
                .enumerate()
                .filter_map(|(k, &v)| {
                    let d = Vec3::from_slice(t.deltas.get(k * 3..k * 3 + 3)?) * units.metres;
                    ((v as usize) < positions && d.is_finite() && d.length_squared() >= MIN_DELTA * MIN_DELTA)
                        .then_some((v, d))
                })
                .collect()
        })
        .collect();
    channels.extend(m.targets.iter().map(|t| t.channel));

    // 4b. How each shape turns the surface: the normals the moved surface
    //     would have, less the rest normals, as Unreal stores with its
    //     morph targets.  Without them a shape that stretches the skin keeps
    //     the rest pose's shading.
    let tri_pos: Vec<[u32; 3]> = prim
        .indices
        .chunks_exact(3)
        .map(|t| [layout_pos[t[0] as usize] as u32, layout_pos[t[1] as usize] as u32, layout_pos[t[2] as usize] as u32])
        .collect();
    let bends = normal_deltas(&rest, &tri_pos, &moves);

    // 4c. Both onto the vertices (layouts) of each position.
    let mut layouts_of: Vec<Vec<u32>> = vec![Vec::new(); positions];
    for (l, &p) in layout_pos.iter().enumerate() {
        layouts_of[p].push(l as u32);
    }
    let sparse: Vec<Vec<(u32, Vec3, Vec3)>> = moves
        .par_iter()
        .zip(&bends)
        .map(|(mv, bend)| {
            let mut per: Vec<(u32, Vec3, Vec3)> = mv.iter().map(|&(p, d)| (p, d, Vec3::ZERO)).collect();
            per.extend(bend.iter().map(|&(p, n)| (p, Vec3::ZERO, n)));
            per.sort_unstable_by_key(|e| e.0);
            per.dedup_by(|later, kept| {
                let same = later.0 == kept.0;
                if same {
                    kept.1 += later.1;
                    kept.2 += later.2;
                }
                same
            });
            per.into_iter().flat_map(|(p, d, n)| layouts_of[p as usize].iter().map(move |&l| (l, d, n))).collect()
        })
        .collect();
    let (primitives, regions) = split_regions(&prim, &sparse);
    let sources = regions.iter().map(|r| r.iter().map(|&v| layout_pos[v] as u32).collect()).collect();
    Ok(BuiltMesh { primitives, sources, channels, triangles })
}

/// Normal changes shorter than this are dropped (about a tenth of a
/// degree), like `MIN_DELTA` for positions.
const MIN_BEND: f32 = 2e-3;

/// For each shape's (position, offset) list, the (position, normal change)
/// list: area-weighted vertex normals of the shape's triangles minus those
/// at rest, for every position next to one it moves.
fn normal_deltas(rest: &[Vec3], tris: &[[u32; 3]], moves: &[Vec<(u32, Vec3)>]) -> Vec<Vec<(u32, Vec3)>> {
    let n = rest.len();
    let face = |t: &[u32; 3], at: &dyn Fn(u32) -> Vec3| (at(t[1]) - at(t[0])).cross(at(t[2]) - at(t[0]));
    let rest_faces: Vec<Vec3> = tris.iter().map(|t| face(t, &|p| rest[p as usize])).collect();
    let mut rest_normals = vec![Vec3::ZERO; n];
    for (t, f) in tris.iter().zip(&rest_faces) {
        for &p in t {
            rest_normals[p as usize] += *f;
        }
    }
    let rest_normals: Vec<Vec3> = rest_normals.iter().map(|v| v.normalize_or_zero()).collect();
    // Triangles around each position.
    let mut start = vec![0u32; n + 1];
    for t in tris {
        for &p in t {
            start[p as usize + 1] += 1;
        }
    }
    for i in 0..n {
        start[i + 1] += start[i];
    }
    let mut fill = start.clone();
    let mut around = vec![0u32; start[n] as usize];
    for (ti, t) in tris.iter().enumerate() {
        for &p in t {
            around[fill[p as usize] as usize] = ti as u32;
            fill[p as usize] += 1;
        }
    }
    let tris_of = |p: u32| &around[start[p as usize] as usize..start[p as usize + 1] as usize];
    // Per thread, every position's offset in the shape at hand (zero for
    // the ones it leaves), cleared after each shape.
    moves
        .par_iter()
        .map_init(
            || (vec![Vec3::ZERO; n], vec![false; n]),
            |(offset, moved), mv| {
                for &(p, d) in mv {
                    offset[p as usize] = d;
                    moved[p as usize] = true;
                }
                let at = |p: u32| rest[p as usize] + offset[p as usize];
                let mut touched: Vec<u32> =
                    mv.iter().flat_map(|&(p, _)| tris_of(p).iter().flat_map(|&t| tris[t as usize])).collect();
                touched.sort_unstable();
                touched.dedup();
                let out = touched
                    .into_iter()
                    .filter_map(|q| {
                        let sum = tris_of(q).iter().fold(Vec3::ZERO, |acc, &t| {
                            let tri = &tris[t as usize];
                            acc + if tri.iter().any(|&p| moved[p as usize]) {
                                face(tri, &at)
                            } else {
                                rest_faces[t as usize]
                            }
                        });
                        let d = sum.normalize_or_zero() - rest_normals[q as usize];
                        (d.is_finite() && d.length_squared() >= MIN_BEND * MIN_BEND).then_some((q, d))
                    })
                    .collect();
                for &(p, _) in mv {
                    offset[p as usize] = Vec3::ZERO;
                    moved[p as usize] = false;
                }
                out
            },
        )
        .collect()
}

/// Each position's four strongest influences, or `None` when the mesh has
/// no skin weights.
fn top_four(m: &MeshData, positions: usize, joints: usize) -> Option<Vec<([u16; 4], Vec4)>> {
    if m.skin_counts.is_empty() {
        return None;
    }
    let mut out = vec![([0u16; 4], Vec4::new(1.0, 0.0, 0.0, 0.0)); positions];
    let mut at = 0usize;
    for (p, &count) in m.skin_counts.iter().enumerate().take(positions) {
        let end = at.saturating_add(count as usize);
        if end > m.skin_joints.len() || end > m.skin_weights.len() {
            break;
        }
        let mut list: Vec<(u16, f32)> = m.skin_joints[at..end]
            .iter()
            .zip(&m.skin_weights[at..end])
            .filter(|&(&j, &w)| (j as usize) < joints && w.is_finite() && w > 0.0)
            .map(|(&j, &w)| (j, w))
            .collect();
        at = end;
        list.sort_by(|a, b| b.1.total_cmp(&a.1));
        list.truncate(4);
        let total: f32 = list.iter().map(|x| x.1).sum();
        if total <= 0.0 {
            continue;
        }
        let mut j = [0u16; 4];
        let mut w = [0f32; 4];
        for (k, (jj, ww)) in list.into_iter().enumerate() {
            j[k] = jj;
            w[k] = ww / total;
        }
        out[p] = (j, Vec4::from(w));
    }
    Some(out)
}

/// Splits a primitive into regions of about `REGION_TRIANGLES` triangles,
/// cutting space in half along its longest side until the pieces are small
/// enough, and gives each piece the morph targets that move it.  `sparse`
/// holds each target's (vertex, position offset, normal offset).  Also
/// returns each piece's vertices as indices into `prim`'s.
fn split_regions(prim: &Primitive, sparse: &[Vec<(u32, Vec3, Vec3)>]) -> (Vec<Primitive>, Vec<Vec<usize>>) {
    // 1. Triangles by region.
    let tris: Vec<[u32; 3]> = prim.indices.chunks_exact(3).map(|t| [t[0], t[1], t[2]]).collect();
    let centre = |t: &[u32; 3]| {
        (prim.positions[t[0] as usize] + prim.positions[t[1] as usize] + prim.positions[t[2] as usize]) / 3.0
    };
    let mut regions: Vec<Vec<u32>> = Vec::new();
    let mut stack = vec![(0..tris.len() as u32).collect::<Vec<u32>>()];
    while let Some(mut list) = stack.pop() {
        if list.len() <= REGION_TRIANGLES {
            regions.push(list);
            continue;
        }
        let mut lo = Vec3::splat(f32::INFINITY);
        let mut hi = Vec3::splat(f32::NEG_INFINITY);
        for &t in &list {
            let c = centre(&tris[t as usize]);
            lo = lo.min(c);
            hi = hi.max(c);
        }
        let size = hi - lo;
        let axis = if size.x >= size.y && size.x >= size.z {
            0
        } else if size.y >= size.z {
            1
        } else {
            2
        };
        let mid = list.len() / 2;
        list.select_nth_unstable_by(mid, |&a, &b| {
            centre(&tris[a as usize])[axis].total_cmp(&centre(&tris[b as usize])[axis])
        });
        let upper = list.split_off(mid);
        stack.push(list);
        stack.push(upper);
    }

    // 2. A primitive per region with its own vertices.  A vertex on a
    //    region's border belongs to each region using it.
    let n = prim.positions.len();
    let mut local = vec![u32::MAX; n];
    let mut owners: Vec<Vec<(u32, u32)>> = vec![Vec::new(); n];
    let mut out = Vec::with_capacity(regions.len());
    let mut vertices = Vec::with_capacity(regions.len());
    for (r, region) in regions.iter().enumerate() {
        let mut p = Primitive::default();
        let mut global = Vec::new();
        for &t in region {
            for &v in &tris[t as usize] {
                let slot = &mut local[v as usize];
                if *slot == u32::MAX {
                    *slot = global.len() as u32;
                    owners[v as usize].push((r as u32, *slot));
                    global.push(v as usize);
                }
                p.indices.push(*slot);
            }
        }
        p.positions = global.iter().map(|&v| prim.positions[v]).collect();
        p.normals = global.iter().map(|&v| prim.normals[v]).collect();
        p.uv0 = global.iter().map(|&v| prim.uv0[v]).collect();
        if prim.colors.len() == n {
            p.colors = global.iter().map(|&v| prim.colors[v]).collect();
        }
        if prim.is_skinned() {
            p.joints = global.iter().map(|&v| prim.joints[v]).collect();
            p.weights = global.iter().map(|&v| prim.weights[v]).collect();
        }
        for &v in &global {
            local[v] = u32::MAX;
        }
        out.push(p);
        vertices.push(global);
    }

    // 3. Each target's offsets, spread over the regions they fall in; a
    //    region a target doesn't touch gets an empty one.
    let sizes: Vec<usize> = out.iter().map(|p| p.positions.len()).collect();
    let per_target: Vec<Vec<MorphTarget>> = sparse
        .par_iter()
        .map(|deltas| {
            let mut targets = vec![MorphTarget::default(); sizes.len()];
            for &(v, d, nrm) in deltas {
                for &(r, l) in &owners[v as usize] {
                    let t = &mut targets[r as usize];
                    let size = sizes[r as usize];
                    if d != Vec3::ZERO {
                        if t.positions.is_empty() {
                            t.positions = vec![Vec3::ZERO; size];
                        }
                        t.positions[l as usize] = d;
                    }
                    if nrm != Vec3::ZERO {
                        if t.normals.is_empty() {
                            t.normals = vec![Vec3::ZERO; size];
                        }
                        t.normals[l as usize] = nrm;
                    }
                }
            }
            targets
        })
        .collect();
    for p in &mut out {
        p.targets = Vec::with_capacity(per_target.len());
    }
    for targets in per_target {
        for (p, t) in out.iter_mut().zip(targets) {
            p.targets.push(t);
        }
    }
    (out, vertices)
}

//endregion

/*┌─────────────────╮
| Materials       |
└─────────────────╯*/
//region Materials

/// The material for mesh `base`, from the maps when they have it, else
/// clay: the neutral look of a sculpt, which suits a face without its
/// paint better than flat grey.
fn material(scene: &mut Scene, base: &str, maps: Option<&Maps>) -> usize {
    let name = base.to_string();
    if let Some(i) = scene.materials.iter().position(|m| m.name == name) {
        return i;
    }
    let mut m = Material { name, roughness: 0.6, ..Default::default() };
    let kind = match base {
        "eyeLeft" | "eyeRight" => "eyes",
        other => other,
    };
    let texture = |scene: &mut Scene, t: &Arc<slerp_scene::Texture>| {
        if let Some(i) = scene.textures.iter().position(|x| Arc::ptr_eq(x, t)) {
            return TexRef::new(i);
        }
        scene.textures.push(t.clone());
        TexRef::new(scene.textures.len() - 1)
    };
    match kind {
        "head" | "body" => m.base_color = Vec4::new(0.56, 0.42, 0.35, 1.0),
        "teeth" => {
            m.base_color = Vec4::new(0.78, 0.74, 0.66, 1.0);
            m.roughness = 0.35;
        }
        "eyes" => {
            m.roughness = 0.1;
            m.base_color = Vec4::ONE;
            let painted = crate::maps::painted_eye();
            m.base_color_texture = Some(texture(scene, &painted));
        }
        "eyelashes" => {
            m.base_color = Vec4::new(0.03, 0.022, 0.018, 1.0);
            m.alpha_mode = AlphaMode::Mask(0.3);
            m.double_sided = true;
        }
        _ => m.base_color = Vec4::new(0.8, 0.72, 0.68, 1.0),
    }
    if let Some(set) = maps.and_then(|maps| maps.get(kind)) {
        if let Some(c) = &set.color {
            m.base_color_texture = Some(texture(scene, c));
            // Lash maps that are only a white mask take their colour from
            // the material, as Unreal's does.
            if !(kind == "eyelashes" && set.lash_tint) {
                m.base_color = Vec4::ONE;
            }
        }
        if let Some(n) = &set.normal {
            m.normal_texture = Some(texture(scene, n));
        }
        if let Some(r) = &set.roughness {
            m.metallic_roughness_texture = Some(texture(scene, r));
            m.roughness = 1.0;
            m.metallic = 1.0;
            // DCC Export's map keeps Unreal's specular mask in red, low in
            // nostrils, creases and under the jaw.  Read as occlusion it
            // dims the light from the surroundings there, which plain PBR
            // would otherwise reflect as a wet sheen (chalk's skin shader
            // uses the same mask so).
            if set.specular_mask {
                m.occlusion_texture = m.metallic_roughness_texture;
            }
        }
    }
    scene.materials.push(m);
    scene.materials.len() - 1
}

//endregion
