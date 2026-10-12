//! FBX, binary and ASCII, versions 6 and 7, through the ufbx library.
//!
//! ufbx does the hard parts: the format's many versions, its transform
//! pivots and inheritance modes, geometry transforms (folded into the
//! vertices), conversion to metres (put on the root node, so skins and
//! animation stay in the file's own units below it), triangulation and
//! animation baking.  The up axis is turned to +Y here; see `up_rotation`.  This module maps its scene onto ours: one mesh per FBX
//! mesh with a primitive per material, every UV set the scene model holds
//! (two), skins with any number of joints, blend shapes as named morph
//! targets, and each animation stack baked to linear keyframes.
//!
//! Materials use the PBR maps when the file has a PBR shader (Stingray,
//! Arnold, 3ds Max Physical, glTF, Blender's Principled), and otherwise map
//! Lambert and Phong onto metallic-roughness.  Textures are decoded from
//! the file when embedded, otherwise looked up next to it.

use crate::common::{
    ChannelSource, FastMap, TextureSet, check_triangles, flipped_uv_transform, linear_color, srgb_color,
};
use glam::{Mat3, Mat4, Quat, Vec2, Vec3, Vec4};
use slerp_scene::{
    AlphaMode, Animation, Channel, Interpolation, Limits, LoadError, Material, Mesh, MorphTarget, Node, Primitive,
    Property, Sampler, Scene, Skin, TexRef, Topology, Transform, Wrap,
};
use std::path::Path;

pub const BINARY_MAGIC: &[u8] = b"Kaydara FBX Binary  \0";

/// ASCII FBX files start with a comment naming the format, or reach the
/// header extension within the first few hundred bytes.
pub fn looks_like_ascii(head: &[u8]) -> bool {
    crate::common::looks_like_text(head)
        && (crate::common::find(head, b"FBX").is_some() || crate::common::find(head, b"FBXHeaderExtension").is_some())
}

/// More morph target data than this (positions times targets) is skipped,
/// so a face rig with thousands of shapes can't exhaust memory.
const MAX_MORPH_VALUES: usize = 64 << 20;
/// At most this many of ufbx's warnings are passed on.
const MAX_WARNINGS: usize = 12;

/*┌─────────────────╮
| Conversions     |
└─────────────────╯*/
//region Conversions

fn v2(v: ufbx::Vec2) -> Vec2 {
    Vec2::new(v.x as f32, v.y as f32)
}

fn v3(v: ufbx::Vec3) -> Vec3 {
    Vec3::new(v.x as f32, v.y as f32, v.z as f32)
}

fn v4(v: ufbx::Vec4) -> Vec4 {
    Vec4::new(v.x as f32, v.y as f32, v.z as f32, v.w as f32)
}

fn quat(q: ufbx::Quat) -> Quat {
    let q = Quat::from_xyzw(q.x as f32, q.y as f32, q.z as f32, q.w as f32);
    if q.length_squared() > 0.0 && q.is_finite() { q.normalize() } else { Quat::IDENTITY }
}

fn mat4(m: &ufbx::Matrix) -> Mat4 {
    Mat4::from_cols(
        Vec4::new(m.m00 as f32, m.m10 as f32, m.m20 as f32, 0.0),
        Vec4::new(m.m01 as f32, m.m11 as f32, m.m21 as f32, 0.0),
        Vec4::new(m.m02 as f32, m.m12 as f32, m.m22 as f32, 0.0),
        Vec4::new(m.m03 as f32, m.m13 as f32, m.m23 as f32, 1.0),
    )
}

fn transform(t: &ufbx::Transform) -> Transform {
    let out = Transform { translation: v3(t.translation), rotation: quat(t.rotation), scale: v3(t.scale) };
    if out.translation.is_finite() && out.scale.is_finite() { out } else { Transform::IDENTITY }
}

/// The value a vertex attribute holds at face corner `ix`, or `None` when
/// the attribute or the index is missing.
fn attr<T: Copy>(values: &ufbx::List<T>, indices: &ufbx::List<u32>, ix: usize) -> Option<T> {
    values.get(*indices.get(ix)? as usize).copied()
}

//endregion

/*┌─────────────────╮
| Loading         |
└─────────────────╯*/
//region Loading

pub fn load(path: &Path, bytes: &[u8], limits: &Limits) -> Result<Scene, LoadError> {
    // 1. Parse.  Memory is capped for thumbnails; the viewer may use all.
    let mut opts = ufbx::LoadOpts {
        target_unit_meters: 1.0,
        space_conversion: ufbx::SpaceConversion::TransformRoot,
        geometry_transform_handling: ufbx::GeometryTransformHandling::ModifyGeometry,
        inherit_mode_handling: ufbx::InheritModeHandling::Compensate,
        generate_missing_normals: true,
        clean_skin_weights: true,
        ignore_animation: !limits.animations,
        load_external_files: false,
        use_blender_pbr_material: true,
        filename: path.to_str().unwrap_or_default().into(),
        ..Default::default()
    };
    if limits.max_file_bytes != u64::MAX {
        let cap = (bytes.len().saturating_mul(64)).clamp(256 << 20, 4 << 30);
        opts.temp_allocator.memory_limit = cap;
        opts.result_allocator.memory_limit = cap;
    }
    let fbx = ufbx::load_memory(bytes, opts)
        .map_err(|e| LoadError::invalid(format!("FBX: {} ({:?})", &*e.description, e.type_)))?;

    // 2. Refuse oversized scenes before building anything.
    let triangles: u64 = fbx.meshes.iter().map(|m| m.num_triangles as u64).sum();
    check_triangles(triangles, limits)?;

    let mut scene = Scene::default();
    describe(&fbx, &mut scene);
    let mut textures = TextureSet::new(path, limits);

    // 3. Nodes, one for one, so ufbx's typed ids are our node indices.
    for node in fbx.nodes.iter() {
        let mut n = Node::new(node.element.name.to_string());
        n.transform = transform(&node.local_transform);
        n.visible = node.visible;
        n.children =
            node.children.iter().map(|c| c.element.typed_id as usize).filter(|&c| c < fbx.nodes.count).collect();
        scene.nodes.push(n);
    }
    scene.roots = vec![fbx.root_node.element.typed_id as usize];
    let up = up_rotation(fbx.settings.axes.up);
    if up != Quat::IDENTITY {
        scene.wrap_roots(Transform { rotation: up, ..Transform::IDENTITY });
    }

    // 4. Materials and meshes.
    let mut materials: FastMap<u32, usize> = FastMap::default();
    for mesh in fbx.meshes.iter() {
        let instances: Vec<usize> = mesh
            .element
            .instances
            .iter()
            .map(|n| n.element.typed_id as usize)
            .filter(|&n| n < scene.nodes.len())
            .collect();
        if instances.is_empty() {
            continue;
        }
        let built = build_mesh(mesh, limits, &mut scene.info.warnings);
        if built.mesh.primitives.is_empty() {
            continue;
        }
        let mut out = built.mesh;
        for (prim, part_material) in out.primitives.iter_mut().zip(&built.part_materials) {
            prim.material = part_material.and_then(|mi| mesh.materials.get(mi)).map(|m| {
                *materials.entry(m.element.typed_id).or_insert_with(|| {
                    scene.materials.push(convert_material(m, &mut textures));
                    scene.materials.len() - 1
                })
            });
        }
        let mesh_index = scene.meshes.len();
        scene.meshes.push(out);
        let skin_index = built.skin.map(|skin| {
            scene.skins.push(skin);
            scene.skins.len() - 1
        });
        for &n in &instances {
            let node = &mut scene.nodes[n];
            if node.mesh.is_some() {
                // A node with several meshes: give the extra one a child.
                let mut child = Node::new(format!("{} mesh", node.name));
                child.mesh = Some(mesh_index);
                child.skin = skin_index;
                child.weights = built.weights.clone();
                scene.nodes.push(child);
                let ci = scene.nodes.len() - 1;
                scene.nodes[n].children.push(ci);
            } else {
                node.mesh = Some(mesh_index);
                node.skin = skin_index;
                node.weights = built.weights.clone();
            }
        }
    }
    if scene.meshes.is_empty() {
        // Skeletons with animation and no mesh (Mixamo's "without skin"
        // downloads, animation libraries) still serve the viewer as clips
        // to borrow, so they load as a scene with nothing to draw.
        if fbx.bones.is_empty() && fbx.anim_stacks.is_empty() {
            return Err(LoadError::invalid("FBX holds no meshes"));
        }
        scene.info.details.push(("Meshes".into(), "none: skeleton and animation only".into()));
    }

    // 5. Animation stacks, baked on several threads: an animation library
    //    holds dozens, and ufbx allows concurrent reads of a loaded scene.
    if limits.animations && !fbx.anim_stacks.is_empty() {
        let stacks = fbx.anim_stacks.count;
        let threads = std::thread::available_parallelism().map_or(1, |n| n.get()).clamp(1, 8).min(stacks);
        let (root, shared) = (&fbx, &scene);
        let mut baked: Vec<(usize, Result<Option<Animation>, String>)> = std::thread::scope(|s| {
            let workers: Vec<_> = (0..threads)
                .map(|t| {
                    s.spawn(move || {
                        (t..stacks)
                            .step_by(threads)
                            .map(|i| (i, bake(root, &root.anim_stacks[i], shared)))
                            .collect::<Vec<_>>()
                    })
                })
                .collect();
            workers
                .into_iter()
                .flat_map(|w| w.join().unwrap_or_else(|_| vec![(usize::MAX, Err("baking failed".into()))]))
                .collect()
        });
        baked.sort_by_key(|(i, _)| *i);
        for (i, result) in baked {
            let name = fbx.anim_stacks.get(i).map_or("?".to_string(), |s| s.element.name.to_string());
            match result {
                Ok(Some(anim)) => scene.animations.push(anim),
                Ok(None) => {}
                Err(e) => scene.info.warnings.push(format!("animation {name}: {e}")),
            }
        }
    }

    scene.info.warnings.extend(textures.warnings);
    scene.textures = textures.textures;
    Ok(scene)
}

/// The rotation that turns the file's up axis into +Y.
///
/// Only the up axis is used, not the front axis FBX also declares.  For the
/// usual declarations (Maya and Blender's Y-up with +Z front, 3ds Max's
/// Z-up with -Y front) the two agree.  They differ for unusual ones, such as
/// the Z-up, +Y-front files Blender writes with "Forward: -Y", where a full
/// conversion turns characters away from the camera: the Universal
/// Animation Library's FBX then faces the opposite way from its own glTF
/// export, which is how Unity, the FBX's intended reader, shows it.
fn up_rotation(up: ufbx::CoordinateAxis) -> Quat {
    use std::f32::consts::{FRAC_PI_2, PI};
    match up {
        ufbx::CoordinateAxis::PositiveZ => Quat::from_rotation_x(-FRAC_PI_2),
        ufbx::CoordinateAxis::NegativeZ => Quat::from_rotation_x(FRAC_PI_2),
        ufbx::CoordinateAxis::PositiveX => Quat::from_rotation_z(FRAC_PI_2),
        ufbx::CoordinateAxis::NegativeX => Quat::from_rotation_z(-FRAC_PI_2),
        ufbx::CoordinateAxis::NegativeY => Quat::from_rotation_x(PI),
        _ => Quat::IDENTITY,
    }
}

/// Format, generator and facts for the inspector.
fn describe(fbx: &ufbx::Scene, scene: &mut Scene) {
    let meta = &fbx.metadata;
    let kind = if meta.ascii { "ASCII" } else { "binary" };
    scene.info.format = format!("FBX {}.{} ({kind})", meta.version / 1000, meta.version % 1000 / 100);
    let creator = meta.creator.trim();
    let app = &meta.latest_application;
    scene.info.generator = if !creator.is_empty() {
        Some(creator.to_string())
    } else if !app.name.is_empty() {
        Some(format!("{} {}", &*app.name, &*app.version).trim().to_string())
    } else {
        None
    };
    let s = &fbx.settings;
    let up = match s.axes.up {
        ufbx::CoordinateAxis::PositiveX => "+X",
        ufbx::CoordinateAxis::NegativeX => "-X",
        ufbx::CoordinateAxis::NegativeY => "-Y",
        ufbx::CoordinateAxis::PositiveZ => "+Z",
        ufbx::CoordinateAxis::NegativeZ => "-Z",
        _ => "+Y",
    };
    scene.info.details.push(("Up axis".into(), up.into()));
    if s.original_unit_meters > 0.0 {
        scene.info.details.push(("Unit".into(), format!("{} m", s.original_unit_meters)));
    }
    if !fbx.anim_stacks.is_empty() {
        scene.info.details.push(("Animation stacks".into(), fbx.anim_stacks.count.to_string()));
    }
    for w in meta.warnings.iter().take(MAX_WARNINGS) {
        scene.info.warnings.push(format!("{} (x{})", &*w.description, w.count.max(1)));
    }
}

//endregion

/*┌─────────────────╮
| Meshes          |
└─────────────────╯*/
//region Meshes

struct BuiltMesh {
    mesh: Mesh,
    /// Index into the FBX mesh's material list, per primitive.
    part_materials: Vec<Option<usize>>,
    skin: Option<Skin>,
    /// Default morph weights.
    weights: Vec<f32>,
}

/// Up to four strongest joint weights per control point, normalised.
fn skin_weights(skin: &ufbx::SkinDeformer, vertices: usize) -> (Vec<[u16; 4]>, Vec<Vec4>) {
    let mut joints = vec![[0u16; 4]; vertices];
    let mut weights = vec![Vec4::ZERO; vertices];
    for (v, sv) in skin.vertices.iter().enumerate().take(vertices) {
        let begin = sv.weight_begin as usize;
        let Some(list) = skin.weights.get(begin..begin.saturating_add(sv.num_weights as usize)) else { continue };
        let mut best: [(u16, f32); 4] = [(0, 0.0); 4];
        for w in list {
            let (Ok(cluster), weight) = (u16::try_from(w.cluster_index), w.weight as f32) else { continue };
            // Also skips NaN weights.
            if weight.partial_cmp(&best[3].1) != Some(std::cmp::Ordering::Greater) {
                continue;
            }
            best[3] = (cluster, weight);
            best.sort_by(|a, b| b.1.total_cmp(&a.1));
        }
        let total: f32 = best.iter().map(|b| b.1).sum();
        if total > 0.0 {
            joints[v] = best.map(|b| b.0);
            weights[v] = Vec4::from_array(best.map(|b| b.1 / total));
        }
    }
    (joints, weights)
}

/// Position and normal offsets per control point for every blend channel,
/// with the channels' names and default weights.
struct Morphs {
    names: Vec<String>,
    defaults: Vec<f32>,
    positions: Vec<Vec<Vec3>>,
    normals: Vec<Vec<Vec3>>,
}

fn morphs(mesh: &ufbx::Mesh, vertices: usize, warnings: &mut Vec<String>) -> Morphs {
    let mut m = Morphs { names: Vec::new(), defaults: Vec::new(), positions: Vec::new(), normals: Vec::new() };
    for deformer in mesh.blend_deformers.iter() {
        for channel in deformer.channels.iter() {
            let shape = channel.target_shape.as_deref().or_else(|| channel.keyframes.last().map(|k| &*k.shape));
            let Some(shape) = shape else { continue };
            if (m.positions.len() + 1).saturating_mul(vertices) > MAX_MORPH_VALUES {
                warnings.push(format!("{}: too many morph targets; some skipped", &*mesh.element.name));
                return m;
            }
            let mut pos = vec![Vec3::ZERO; vertices];
            let mut nrm = if shape.normal_offsets.is_empty() { Vec::new() } else { vec![Vec3::ZERO; vertices] };
            for (i, &v) in shape.offset_vertices.iter().enumerate() {
                let v = v as usize;
                if v >= vertices {
                    continue;
                }
                if let Some(&d) = shape.position_offsets.get(i) {
                    pos[v] = v3(d);
                }
                if let (Some(&d), Some(slot)) = (shape.normal_offsets.get(i), nrm.get_mut(v)) {
                    *slot = v3(d);
                }
            }
            m.names.push(channel.element.name.to_string());
            m.defaults.push((channel.weight as f32).clamp(-10.0, 10.0));
            m.positions.push(pos);
            m.normals.push(nrm);
        }
    }
    m
}

fn build_mesh(mesh: &ufbx::Mesh, limits: &Limits, warnings: &mut Vec<String>) -> BuiltMesh {
    let vertices = mesh.num_vertices;
    let positions = &mesh.vertex_position;
    let normals = &mesh.vertex_normal;
    let uv0 = mesh.uv_sets.first().map(|s| &s.vertex_uv);
    let uv1 = mesh.uv_sets.get(1).map(|s| &s.vertex_uv);
    let tangents = mesh.uv_sets.first().map(|s| (&s.vertex_tangent, &s.vertex_bitangent));
    let colors = &mesh.vertex_color;
    if mesh.uv_sets.count > 2 {
        warnings.push(format!("{}: {} UV sets, only the first two kept", &*mesh.element.name, mesh.uv_sets.count));
    }

    // Skinning by control point; clusters become joints in order.
    let skin_def = mesh.skin_deformers.first().map(|s| &**s).filter(|s| !s.clusters.is_empty());
    let (joints_cp, weights_cp) = skin_def.map(|s| skin_weights(s, vertices)).unwrap_or_default();
    let skin = skin_def.map(|s| Skin {
        name: s.element.name.to_string(),
        joints: s.clusters.iter().map(|c| c.bone_node.as_ref().map_or(0, |b| b.element.typed_id as usize)).collect(),
        inverse_bind: s.clusters.iter().map(|c| mat4(&c.geometry_to_bone)).collect(),
        skeleton: None,
    });
    if skin_def.is_some_and(|s| s.clusters.count > u16::MAX as usize) {
        warnings.push(format!("{}: more than 65535 joints", &*mesh.element.name));
    }

    // Morph targets cost memory and thumbnails draw the rest pose, so they
    // are loaded for thumbnails only when the rest pose uses them.
    let any_default = mesh.blend_deformers.iter().flat_map(|d| d.channels.iter()).any(|c| c.weight != 0.0);
    let morph = if limits.animations || any_default { Some(morphs(mesh, vertices, warnings)) } else { None };

    let mut out = BuiltMesh {
        mesh: Mesh { name: mesh.element.name.to_string(), ..Mesh::default() },
        part_materials: Vec::new(),
        skin,
        weights: morph.as_ref().map(|m| m.defaults.clone()).unwrap_or_default(),
    };
    if let Some(m) = &morph {
        out.mesh.target_names = m.names.clone();
    }

    let mut tri = vec![0u32; mesh.max_face_triangles.saturating_mul(3).max(3)];
    for part in mesh.material_parts.iter() {
        let mut prim = Primitive { topology: Topology::Triangles, ..Default::default() };
        let mut source: Vec<u32> = Vec::new();
        let mut remap: FastMap<[u32; 6], u32> = FastMap::default();
        for &fi in part.face_indices.iter() {
            let Some(&face) = mesh.faces.get(fi as usize) else { continue };
            if face.num_indices < 3 {
                continue;
            }
            let needed = (face.num_indices as usize - 2) * 3;
            if tri.len() < needed {
                tri.resize(needed, 0);
            }
            let n = mesh.triangulate_face(&mut tri, face) as usize;
            for &ix in &tri[..(n * 3).min(tri.len())] {
                let ix = ix as usize;
                let idx = |l: &ufbx::List<u32>| l.get(ix).copied().unwrap_or(0);
                let key = [
                    idx(&mesh.vertex_indices),
                    idx(&normals.indices),
                    uv0.map_or(0, |u| idx(&u.indices)),
                    uv1.map_or(0, |u| idx(&u.indices)),
                    idx(&colors.indices),
                    tangents.map_or(0, |t| idx(&t.0.indices)),
                ];
                let next = prim.positions.len() as u32;
                let index = *remap.entry(key).or_insert(next);
                if index == next {
                    let cp = key[0];
                    prim.positions.push(attr(&positions.values, &positions.indices, ix).map_or(Vec3::ZERO, v3));
                    if normals.exists {
                        prim.normals.push(attr(&normals.values, &normals.indices, ix).map_or(Vec3::Y, v3));
                    }
                    if let Some(u) = uv0.filter(|u| u.exists) {
                        prim.uv0.push(attr(&u.values, &u.indices, ix).map_or(Vec2::ZERO, |v| {
                            let v = v2(v);
                            Vec2::new(v.x, 1.0 - v.y)
                        }));
                    }
                    if let Some(u) = uv1.filter(|u| u.exists) {
                        prim.uv1.push(attr(&u.values, &u.indices, ix).map_or(Vec2::ZERO, |v| {
                            let v = v2(v);
                            Vec2::new(v.x, 1.0 - v.y)
                        }));
                    }
                    if colors.exists {
                        let c = attr(&colors.values, &colors.indices, ix).map_or(Vec4::ONE, v4);
                        prim.colors.push(srgb_color(c.x, c.y, c.z, c.w));
                    }
                    if let Some((t, b)) = tangents.filter(|(t, b)| t.exists && b.exists && normals.exists) {
                        let tv = attr(&t.values, &t.indices, ix).map_or(Vec3::X, v3);
                        let bv = attr(&b.values, &b.indices, ix).map_or(Vec3::Y, v3);
                        let nv = *prim.normals.last().unwrap_or(&Vec3::Y);
                        // The bitangent is flipped with V, so the handedness
                        // flips too.
                        let w = if nv.cross(tv).dot(bv) < 0.0 { 1.0 } else { -1.0 };
                        prim.tangents.push(tv.extend(w));
                    }
                    if !joints_cp.is_empty() {
                        prim.joints.push(joints_cp.get(cp as usize).copied().unwrap_or_default());
                        prim.weights.push(weights_cp.get(cp as usize).copied().unwrap_or(Vec4::X));
                    }
                    source.push(cp);
                }
                prim.indices.push(index);
            }
        }
        if prim.indices.is_empty() {
            continue;
        }
        if let Some(m) = &morph {
            for (pos, nrm) in m.positions.iter().zip(&m.normals) {
                let gather = |d: &Vec<Vec3>| {
                    source.iter().map(|&cp| d.get(cp as usize).copied().unwrap_or(Vec3::ZERO)).collect()
                };
                prim.targets.push(MorphTarget {
                    positions: gather(pos),
                    normals: if nrm.is_empty() { Vec::new() } else { gather(nrm) },
                });
            }
        }
        out.part_materials.push((part.index as usize).into());
        out.mesh.primitives.push(prim);
    }
    out
}

//endregion

/*┌─────────────────╮
| Materials       |
└─────────────────╯*/
//region Materials

/// The file texture behind a material map: layered textures use their
/// first layer.
fn file_texture(t: &ufbx::Texture) -> &ufbx::Texture {
    if t.type_ == ufbx::TextureType::Layered
        && let Some(layer) = t.layers.first()
    {
        return &layer.texture;
    }
    t.file_textures.first().map(|f| &**f).unwrap_or(t)
}

/// Loads a texture: embedded bytes first, then the file it names.
fn load_texture(textures: &mut TextureSet, t: &ufbx::Texture) -> Option<TexRef> {
    let t = file_texture(t);
    let video_content = t.video.as_ref().map(|v| &v.content[..]).unwrap_or_default();
    let content: &[u8] = if !t.content.is_empty() { &t.content } else { video_content };
    let index = if !content.is_empty() {
        textures.embedded(&format!("{}:{}", t.file_index, &*t.filename), content)
    } else {
        let names = [&*t.relative_filename, &*t.filename, &*t.absolute_filename];
        let mut found = None;
        for name in names.iter().filter(|n| !n.is_empty()) {
            found = textures.file_quiet(name);
            if found.is_some() {
                break;
            }
        }
        if found.is_none() {
            textures.missing(names.iter().find(|n| !n.is_empty()).copied().unwrap_or(&*t.element.name));
        }
        found
    }?;
    let mut r = TexRef::new(index);
    if t.has_uv_transform {
        r.transform = flipped_uv_transform(uv_matrix(&t.uv_to_texture));
    }
    let wrap = |w: ufbx::WrapMode| if w == ufbx::WrapMode::Clamp { Wrap::ClampToEdge } else { Wrap::Repeat };
    r.sampler = Sampler { wrap_u: wrap(t.wrap_u), wrap_v: wrap(t.wrap_v), ..Sampler::default() };
    Some(r)
}

/// The 2D part of ufbx's UV matrix.
fn uv_matrix(m: &ufbx::Matrix) -> Mat3 {
    Mat3::from_cols(
        Vec3::new(m.m00 as f32, m.m10 as f32, 0.0),
        Vec3::new(m.m01 as f32, m.m11 as f32, 0.0),
        Vec3::new(m.m03 as f32, m.m13 as f32, 1.0),
    )
}

fn map_color(m: &ufbx::MaterialMap) -> Option<Vec3> {
    m.has_value.then(|| v3(ufbx::Vec3 { x: m.value_vec4.x, y: m.value_vec4.y, z: m.value_vec4.z }))
}

fn linear_color3(c: Vec3) -> Vec3 {
    linear_color(c.x, c.y, c.z, 1.0).truncate()
}

fn map_real(m: &ufbx::MaterialMap) -> Option<f32> {
    (m.has_value && m.value_vec4.x.is_finite()).then_some(m.value_vec4.x as f32)
}

fn map_texture(textures: &mut TextureSet, m: &ufbx::MaterialMap) -> Option<TexRef> {
    if m.feature_disabled || !m.texture_enabled {
        return None;
    }
    m.texture.as_ref().and_then(|t| load_texture(textures, t))
}

fn convert_material(m: &ufbx::Material, textures: &mut TextureSet) -> Material {
    let mut out = Material { name: m.element.name.to_string(), ..Material::default() };
    out.double_sided = m.features.double_sided.enabled;
    out.unlit = m.features.unlit.enabled;
    if m.features.pbr.enabled {
        pbr_material(m, textures, &mut out);
    } else {
        phong_material(m, textures, &mut out);
    }
    out
}

fn pbr_material(m: &ufbx::Material, textures: &mut TextureSet, out: &mut Material) {
    let p = &m.pbr;
    // A texture plugged into a slot replaces its colour, as in the tools
    // that write these shaders; the slot's weight still scales it.
    let opacity = map_real(&p.opacity).unwrap_or(1.0).clamp(0.0, 1.0);
    let alpha = if opacity < 0.01 { 1.0 } else { opacity };
    let weight = map_real(&p.base_factor).unwrap_or(1.0).max(0.0);
    out.base_color_texture = map_texture(textures, &p.base_color);
    out.base_color = match out.base_color_texture {
        Some(_) => Vec3::splat(weight).extend(alpha),
        None => (linear_color3(map_color(&p.base_color).unwrap_or(Vec3::splat(0.8))) * weight).extend(alpha),
    };
    let glossy = m.features.roughness_as_glossiness.enabled;
    let rough_value = map_real(&p.roughness).or(map_real(&p.glossiness).map(|g| 1.0 - g));
    out.roughness = rough_value
        .map_or(0.5, |r| if glossy && map_real(&p.roughness).is_some() { 1.0 - r } else { r })
        .clamp(0.0, 1.0);
    out.metallic = map_real(&p.metalness).unwrap_or(0.0).clamp(0.0, 1.0);
    let rough_tex = map_texture(textures, &p.roughness)
        .map(|r| (r, glossy))
        .or_else(|| map_texture(textures, &p.glossiness).map(|r| (r, true)));
    let metal_tex = map_texture(textures, &p.metalness);
    let rough_src = rough_tex.map(|(r, invert)| ChannelSource { texture: r.texture, channel: 0, invert });
    let metal_src = metal_tex.map(|r| ChannelSource { texture: r.texture, channel: 0, invert: false });
    if let Some(packed) = textures.pack_metal_rough(rough_src, metal_src) {
        let mut r = rough_tex.map(|t| t.0).or(metal_tex).unwrap_or(TexRef::new(packed));
        r.texture = packed;
        out.metallic_roughness_texture = Some(r);
        if rough_tex.is_some() {
            out.roughness = 1.0;
        }
        if metal_tex.is_some() {
            out.metallic = 1.0;
        }
    }
    out.normal_texture = map_texture(textures, &p.normal_map);
    out.occlusion_texture = map_texture(textures, &p.ambient_occlusion);
    let emission_weight = map_real(&p.emission_factor).unwrap_or(1.0).max(0.0);
    out.emissive_texture = map_texture(textures, &p.emission_color);
    out.emissive = match out.emissive_texture {
        Some(_) => Vec3::splat(emission_weight),
        None => linear_color3(map_color(&p.emission_color).unwrap_or(Vec3::ZERO)) * emission_weight,
    };
    if m.features.coat.enabled {
        out.clearcoat = map_real(&p.coat_factor).unwrap_or(0.0).clamp(0.0, 1.0);
        out.clearcoat_roughness = map_real(&p.coat_roughness).unwrap_or(0.0).clamp(0.0, 1.0);
    }
    if let Some(ior) = map_real(&p.specular_ior).filter(|&i| i >= 1.0) {
        out.ior = ior;
    }
    let mask = map_texture(textures, &p.opacity);
    apply_opacity(out, textures, mask, opacity);
}

fn phong_material(m: &ufbx::Material, textures: &mut TextureSet, out: &mut Material) {
    let f = &m.fbx;
    let diffuse = map_color(&f.diffuse_color).unwrap_or(Vec3::splat(0.8)) * map_real(&f.diffuse_factor).unwrap_or(1.0);
    // FBX transparency is notoriously inconsistent: prefer an explicit
    // "Opacity" property, else 1 - factor * colour, and treat a fully
    // invisible result as an exporter's mistake.
    let opacity = ufbx::find_prop(&m.element.props, "Opacity")
        .map(|p| p.value_vec4.x as f32)
        .filter(|o| o.is_finite())
        .unwrap_or_else(|| {
            let factor = map_real(&f.transparency_factor);
            let color = map_color(&f.transparency_color).map(|c| c.max_element());
            match (factor, color) {
                (None, None) => 1.0,
                (factor, color) => 1.0 - factor.unwrap_or(1.0) * color.unwrap_or(1.0),
            }
        });
    let opacity = if opacity < 0.01 { 1.0 } else { opacity.clamp(0.0, 1.0) };
    out.base_color = linear_color(diffuse.x, diffuse.y, diffuse.z, opacity);
    out.base_color_texture = map_texture(textures, &f.diffuse_color);
    if out.base_color_texture.is_some() {
        out.base_color = Vec4::new(1.0, 1.0, 1.0, opacity);
    }
    // Blender's mapping of the Phong exponent (0..100) to roughness.
    let lambert = m.shader_type == ufbx::ShaderType::FbxLambert || !f.specular_exponent.has_value;
    out.roughness = match map_real(&f.specular_exponent) {
        Some(s) if !lambert => 1.0 - s.clamp(0.0, 100.0).sqrt() / 10.0,
        _ => 0.9,
    };
    if let Some(spec) = map_color(&f.specular_color) {
        out.specular = (spec.max_element() * map_real(&f.specular_factor).unwrap_or(1.0) * 2.0).clamp(0.0, 1.0);
    }
    let emission = map_color(&f.emission_color).unwrap_or(Vec3::ZERO) * map_real(&f.emission_factor).unwrap_or(1.0);
    out.emissive = linear_color3(emission);
    out.emissive_texture = map_texture(textures, &f.emission_color);
    if out.emissive_texture.is_some() && out.emissive == Vec3::ZERO {
        out.emissive = Vec3::ONE;
    }
    out.normal_texture = map_texture(textures, &f.normal_map);
    if out.normal_texture.is_none() {
        // Maya's bump slot often holds a normal map.
        if let Some(r) = map_texture(textures, &f.bump)
            && textures.get(r.texture).is_some_and(crate::common::looks_like_normal_map)
        {
            out.normal_texture = Some(r);
        }
    }
    let mask = map_texture(textures, &f.transparency_color);
    apply_opacity(out, textures, mask, opacity);
}

/// Alpha blending for partly transparent materials, and alpha masks from a
/// transparency texture (the same picture as the base colour, or merged
/// into its alpha).
fn apply_opacity(out: &mut Material, textures: &mut TextureSet, mask: Option<TexRef>, opacity: f32) {
    if let (Some(mask), Some(base)) = (mask, out.base_color_texture.as_mut())
        && let Some(merged) = textures.merge_alpha(base.texture, mask.texture)
    {
        base.texture = merged;
        out.alpha_mode = AlphaMode::Mask(0.5);
        out.double_sided = true;
        out.base_color.w = 1.0;
        return;
    }
    if opacity < 0.999 {
        out.alpha_mode = AlphaMode::Blend;
    }
}

//endregion

/*┌─────────────────╮
| Animation       |
└─────────────────╯*/
//region Animation

/// Bakes one animation stack into linear channels: translation, rotation
/// and scale per animated node, and one weights channel per node whose
/// mesh has animated blend channels.
fn bake(fbx: &ufbx::Scene, stack: &ufbx::AnimStack, scene: &Scene) -> Result<Option<Animation>, String> {
    let opts = ufbx::BakeOpts { trim_start_time: true, key_reduction_enabled: true, ..Default::default() };
    let baked = ufbx::bake_anim(fbx, &stack.anim, opts).map_err(|e| e.description.to_string())?;
    let mut anim = Animation { name: clip_name(&stack.element.name), duration: 0.0, channels: Vec::new() };
    for bn in baked.nodes.iter() {
        let node = bn.typed_id as usize;
        let Some(rest) = scene.nodes.get(node).map(|n| n.transform) else { continue };
        let times3 = |keys: &[ufbx::BakedVec3]| keys.iter().map(|k| k.time as f32).collect::<Vec<_>>();
        let close = |a: Vec3, b: Vec3| (a - b).abs().max_element() < 1e-6;
        let t_keys = &bn.translation_keys;
        if !t_keys.is_empty() && (!bn.constant_translation || !close(v3(t_keys[0].value), rest.translation)) {
            anim.channels.push(Channel {
                node,
                property: Property::Translation,
                interpolation: Interpolation::Linear,
                times: times3(t_keys),
                values: t_keys.iter().flat_map(|k| v3(k.value).to_array()).collect(),
            });
        }
        let r_keys = &bn.rotation_keys;
        if !r_keys.is_empty() && (!bn.constant_rotation || quat(r_keys[0].value).dot(rest.rotation).abs() <= 0.999_999)
        {
            anim.channels.push(Channel {
                node,
                property: Property::Rotation,
                interpolation: Interpolation::Linear,
                times: r_keys.iter().map(|k| k.time as f32).collect(),
                values: r_keys.iter().flat_map(|k| quat(k.value).to_array()).collect(),
            });
        }
        let s_keys = &bn.scale_keys;
        if !s_keys.is_empty() && (!bn.constant_scale || !close(v3(s_keys[0].value), rest.scale)) {
            anim.channels.push(Channel {
                node,
                property: Property::Scale,
                interpolation: Interpolation::Linear,
                times: times3(s_keys),
                values: s_keys.iter().flat_map(|k| v3(k.value).to_array()).collect(),
            });
        }
    }
    morph_channels(fbx, &baked, scene, &mut anim);
    anim.duration = anim.channels.iter().filter_map(|c| c.times.last()).fold(0.0f32, |a, &b| a.max(b));
    if anim.duration <= 0.0 {
        anim.duration = (baked.playback_duration as f32).max(0.0);
    }
    Ok((!anim.channels.is_empty()).then_some(anim))
}

/// Blend channel weights ("DeformPercent", 0-100) resampled onto one time
/// list per mesh node, since a weights channel carries every target.
fn morph_channels(fbx: &ufbx::Scene, baked: &ufbx::BakedAnim, scene: &Scene, anim: &mut Animation) {
    let mut by_element: FastMap<u32, &[ufbx::BakedVec3]> = FastMap::default();
    for el in baked.elements.iter() {
        if let Some(p) = el.props.iter().find(|p| &*p.name == "DeformPercent") {
            by_element.insert(el.element_id, &p.keys);
        }
    }
    if by_element.is_empty() {
        return;
    }
    for mesh in fbx.meshes.iter() {
        let channels: Vec<&ufbx::BlendChannel> =
            mesh.blend_deformers.iter().flat_map(|d| d.channels.iter().map(|c| &**c)).collect();
        if channels.is_empty() || !channels.iter().any(|c| by_element.contains_key(&c.element.element_id)) {
            continue;
        }
        let mut times: Vec<f32> = channels
            .iter()
            .filter_map(|c| by_element.get(&c.element.element_id))
            .flat_map(|keys| keys.iter().map(|k| k.time as f32))
            .collect();
        times.sort_by(f32::total_cmp);
        times.dedup();
        let mut values = Vec::with_capacity(times.len() * channels.len());
        for &t in &times {
            for c in &channels {
                values.push(match by_element.get(&c.element.element_id) {
                    Some(keys) => sample_baked(keys, t) / 100.0,
                    None => c.weight as f32,
                });
            }
        }
        for node in mesh.element.instances.iter() {
            let index = node.element.typed_id as usize;
            let matches = scene
                .nodes
                .get(index)
                .and_then(|n| n.mesh)
                .and_then(|m| scene.meshes.get(m))
                .is_some_and(|m| m.target_names.len() == channels.len());
            if matches {
                anim.channels.push(Channel {
                    node: index,
                    property: Property::Weights,
                    interpolation: Interpolation::Linear,
                    times: times.clone(),
                    values: values.clone(),
                });
            }
        }
    }
}

/// Linear interpolation of the x component of baked keys.
fn sample_baked(keys: &[ufbx::BakedVec3], t: f32) -> f32 {
    let t = t as f64;
    let i = keys.partition_point(|k| k.time <= t);
    match (i.checked_sub(1).and_then(|p| keys.get(p)), keys.get(i)) {
        (Some(a), Some(b)) if b.time > a.time => {
            (a.value.x + (b.value.x - a.value.x) * (t - a.time) / (b.time - a.time)) as f32
        }
        (Some(a), _) => a.value.x as f32,
        (None, Some(b)) => b.value.x as f32,
        (None, None) => 0.0,
    }
}

//endregion

#[cfg(test)]
mod tests {
    use super::*;

    /// A one-triangle ASCII FBX 7.4 with a Phong material.
    const TRIANGLE: &str = r#"; FBX 7.4.0 project file
FBXHeaderExtension:  {
	FBXHeaderVersion: 1003
	FBXVersion: 7400
	Creator: "Slerp test"
}
GlobalSettings:  {
	Version: 1000
	Properties70:  {
		P: "UpAxis", "int", "Integer", "",2
		P: "UpAxisSign", "int", "Integer", "",1
		P: "FrontAxis", "int", "Integer", "",1
		P: "FrontAxisSign", "int", "Integer", "",-1
		P: "CoordAxis", "int", "Integer", "",0
		P: "CoordAxisSign", "int", "Integer", "",1
		P: "UnitScaleFactor", "double", "Number", "",1
	}
}
Objects:  {
	Geometry: 1000, "Geometry::Tri", "Mesh" {
		Vertices: *9 {
			a: 0,0,0,1,0,0,0,1,0
		}
		PolygonVertexIndex: *3 {
			a: 0,1,-3
		}
	}
	Model: 2000, "Model::Tri", "Mesh" {
		Version: 232
	}
	Material: 3000, "Material::Red", "" {
		ShadingModel: "phong"
		Properties70:  {
			P: "DiffuseColor", "Color", "", "A",1,0,0
			P: "Shininess", "double", "Number", "",25
		}
	}
}
Connections:  {
	C: "OO",2000,0
	C: "OO",1000,2000
	C: "OO",3000,2000
}
"#;

    #[test]
    fn loads_ascii_triangle_z_up_in_centimetres() {
        let scene = load(Path::new("t.fbx"), TRIANGLE.as_bytes(), &Limits::viewer()).unwrap();
        assert_eq!(scene.info.format, "FBX 7.4 (ASCII)");
        assert_eq!(scene.info.generator.as_deref(), Some("Slerp test"));
        assert_eq!(scene.triangle_count(), 1);
        let m = &scene.materials[0];
        assert_eq!(m.name, "Red");
        assert_eq!(m.base_color, Vec4::new(1.0, 0.0, 0.0, 1.0));
        assert!((m.roughness - 0.5).abs() < 1e-6);
        // Z-up centimetres, as 3ds Max writes them: the vertex at +Y (away
        // from a Z-up viewer) ends up at -Z, one centimetre away.
        let pose = slerp_scene::Pose::rest(&scene);
        let b = scene.bounds(&pose);
        assert!((b.size().max_element() - 0.01).abs() < 1e-6, "{b:?}");
        assert!(b.min.z < -0.009, "{b:?}");
        assert_eq!(scene.meshes[0].primitives[0].normals.len(), 3);
    }

    #[test]
    fn survives_truncation_and_garbage() {
        let bytes = TRIANGLE.as_bytes();
        for cut in (0..bytes.len()).step_by(7) {
            let _ = load(Path::new("t.fbx"), &bytes[..cut], &Limits::viewer());
        }
        let mut binary = BINARY_MAGIC.to_vec();
        binary.extend([0x1a, 0, 0xe8, 0x1c, 0, 0]);
        binary.extend(std::iter::repeat_n(0xff, 200));
        assert!(load(Path::new("t.fbx"), &binary, &Limits::viewer()).is_err());
        let limits = Limits { max_triangles: 0, ..Limits::viewer() };
        assert!(matches!(load(Path::new("t.fbx"), bytes, &limits), Err(LoadError::TooLarge(_))));
    }
}

/// A take's name without the armature Blender prefixes it with
/// (`Armature|Walk_Loop` becomes `Walk_Loop`), so clip lists read cleanly
/// and match the same clips from glTF files.
fn clip_name(name: &str) -> String {
    match name.rsplit_once('|') {
        Some((_, clip)) if !clip.is_empty() => clip.to_string(),
        _ => name.to_string(),
    }
}
