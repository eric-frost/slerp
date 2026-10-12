//! A scene uploaded to the GPU: geometry packed into storage buffers,
//! materials with their textures, and the per-draw records that a pose
//! updates.

use crate::Gpu;
use crate::instancing::{Group, group_draws, list_instances};
use crate::renderer::{Renderer, buffer_init};
use crate::resources::{
    DrawUniform, GpuTexture, MaterialUniform, NONE, SLOTS, Surfaces, upload_preview, upload_texture,
};
use glam::{Mat4, Vec3};
use slerp_scene::{
    Aabb, AlphaMode, Effect, Marker, Material, MorphTarget, Pose, Primitive, Scene, SceneKind, TexRef, Topology,
};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// The largest stored mip level `prepare_untextured` shows while a texture
/// waits.
const PREVIEW_SIDE: u32 = 64;

/// Below this many bytes of texture data, `prepare_untextured` uploads the
/// textures at once: they take a few milliseconds, less than a frame.
const LATER_ABOVE_BYTES: u64 = 24 << 20;

/// Bytes of texture data the scene's materials use, about as they upload.
fn texture_bytes(scene: &Scene) -> u64 {
    use slerp_scene::textures::Pixels;
    let bytes = |p: &Pixels| match p {
        Pixels::Rgba8(v) => v.len() as u64,
        Pixels::Rgba32F(v) => v.len() as u64 * 2,
        Pixels::Blocks(_, v) => v.len() as u64,
    };
    let mut used = vec![false; scene.textures.len()];
    for r in scene.materials.iter().flat_map(slots).flatten() {
        if let Some(u) = used.get_mut(r.texture) {
            *u = true;
        }
    }
    let used = scene.textures.iter().zip(used).filter(|(_, u)| *u).map(|(t, _)| t);
    used.flat_map(|t| &t.levels).flat_map(|l| &l.surfaces).map(bytes).sum()
}

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

/// One storage buffer of 32-bit words holding the indices, attributes and
/// morph deltas of several primitives; also bound as their index buffer.
pub(crate) struct Chunk {
    pub buffer: wgpu::Buffer,
    pub group: wgpu::BindGroup,
}

pub(crate) struct GpuPrim {
    pub chunk: usize,
    /// Word offset of the first index in the chunk (which is also its index
    /// in the chunk viewed as an index buffer).
    pub index_first: u32,
    pub index_count: u32,
    pub vertex_count: u32,
    /// Word offsets: positions, normals, tangents, uv0.
    pub attrs0: [u32; 4],
    /// Word offsets: uv1, colours, joints, weights.
    pub attrs1: [u32; 4],
    /// Word offset of each morph target's deltas, NONE when it is empty.
    pub targets: Vec<u32>,
    pub morph_normals: bool,
    pub topology: Topology,
    pub material: usize,
    pub local_bounds: Aabb,
    /// For skinned primitives: the box of the vertices each joint mostly
    /// moves, in mesh space, to bound the posed mesh cheaply.
    pub joint_boxes: Vec<(u32, Aabb)>,
    /// For points: the typical distance between neighbours, in mesh
    /// units, which sizes their dots.
    pub point_spacing: f32,
    /// For points: ambient occlusion shapes them.  Clouds with colours
    /// (scans in RGB) show what was captured, light and shade included,
    /// and occlusion would only speckle their sparser patches; plain and
    /// grey (intensity) clouds have nothing else to show their shape.
    pub point_ao: bool,
}

pub(crate) struct GpuMaterial {
    pub group: wgpu::BindGroup,
    /// The `MaterialUniform` the group binds, rewritten by
    /// `Renderer::update_material`.
    pub uniform: wgpu::Buffer,
    pub blend: bool,
    pub double_sided: bool,
    /// Skies and unlit effects cast no shadow, nor does glass.
    pub casts_shadow: bool,
    /// Lets light through (KHR_materials_transmission), so it is drawn
    /// after the opaque scene it shows.
    pub transmissive: bool,
    /// Lit by the environment, so ambient occlusion changes it: not unlit,
    /// lightmapped, sky or liquid surfaces.
    pub lit: bool,
    /// The vertex shader moves vertices past the mesh's own box: billboards
    /// turn to the camera and height maps displace, so draws wearing it
    /// can't be culled by that box.
    pub moves_vertices: bool,
}

pub(crate) struct DrawItem {
    pub node: usize,
    pub prim: usize,
    /// The skin, for skinned primitives on skinned nodes.
    pub skin: Option<usize>,
    pub visible: bool,
    /// Quake sky surfaces, which wrap levels in a box much larger than the
    /// playable space and so are left out of the bounds.
    pub sky: bool,
    /// The node's world matrix mirrors (negative determinant), so its
    /// triangles wind the other way round, as glTF says.
    pub mirrored: bool,
    /// Centre of the posed bounds, for sorting transparent draws.
    pub center: Vec3,
    /// The posed primitive's box in world space.
    pub bounds: Aabb,
    /// No skin or active morph target moves the vertices, so `bounds` holds
    /// all of them and the draw can be culled by it.
    pub rigid: bool,
}

/// A scene on the GPU, ready to render.  Made by `Renderer::prepare`, posed
/// by `Renderer::update_pose`, and only usable with the renderer that made
/// it.
pub struct GpuScene {
    pub(crate) id: u64,
    pub(crate) pose_version: u64,
    pub(crate) chunks: Vec<Chunk>,
    pub(crate) prims: Vec<GpuPrim>,
    pub(crate) materials: Vec<GpuMaterial>,
    textures: Vec<Option<GpuTexture>>,
    /// Textures `prepare_untextured` left for `upload_textures`, in the
    /// order they go up, with whether a colour slot uses them.
    pending: Vec<(usize, bool)>,
    pub(crate) draws: Vec<DrawItem>,
    /// The draws grouped by primitive, each group drawn by one instanced
    /// call (see `instancing`).
    pub(crate) groups: Vec<Group>,
    /// The first half of the instance list: each group's visible draws, as
    /// uploaded to `instances`.
    pub(crate) listed: Vec<u32>,
    /// Draw indices that instanced calls read their records through, room
    /// for twice the draws: those `listed`, then a frame's culled lists.
    pub(crate) instances: wgpu::Buffer,
    parents: Vec<Option<usize>>,
    reachable: Vec<bool>,
    draw_data: Vec<DrawUniform>,
    joint_data: Vec<Mat4>,
    morph_data: Vec<[u32; 2]>,
    buffers: [(wgpu::Buffer, u64); 3],
    pub(crate) frame_groups: Vec<wgpu::BindGroup>,
    /// Bounds in the rest pose, which place the ground.
    pub rest_bounds: Aabb,
    /// Bounds of what is visible in the current pose, leaving out Quake
    /// sky surfaces (a level's sky box can be far larger than the level).
    /// Skinned meshes are bounded by boxes that follow their joints, exact
    /// in the rest pose and slightly loose when posed.  Good for framing.
    pub bounds: Aabb,
    pub(crate) ground_height: f32,
    /// Whether the soft ground shadow suits the scene (not for levels).
    pub(crate) ground_shadow: bool,
    /// Joint-to-parent segments of the current pose, with the child node.
    pub(crate) bones: Vec<(Vec3, Vec3, usize)>,
    pub(crate) markers: Vec<Marker>,
    /// Problems met while uploading, such as a texture that didn't fit.
    pub warnings: Vec<String>,
    /// Gaussian splats, drawn by their own passes.
    pub(crate) splats: Option<crate::splats::GpuSplats>,
}

impl GpuScene {
    /// Bytes of GPU memory held by textures and geometry.
    pub fn gpu_bytes(&self) -> u64 {
        let tex: u64 = self.textures.iter().flatten().map(|t| t.bytes).sum();
        let geo: u64 = self.chunks.iter().map(|c| c.buffer.size()).sum();
        tex + geo + self.splat_bytes()
    }

    /// Gaussian splats drawn: all of the scene's, unless the GPU couldn't
    /// hold them.
    pub fn splat_count(&self) -> usize {
        self.splats.as_ref().map_or(0, |s| s.count as usize)
    }

    /// Bytes of GPU memory held by Gaussian splats, including what sorting
    /// and drawing them needs.
    pub fn splat_bytes(&self) -> u64 {
        self.splats.as_ref().map_or(0, |s| s.bytes)
    }
}

/*┌─────────────────╮
| Geometry        |
└─────────────────╯*/
//region Geometry

fn push_f32s(words: &mut Vec<u32>, data: &[f32]) {
    words.extend(data.iter().map(|f| f.to_bits()));
}

/// Packs one primitive's data at the end of `words`.  Returns `None` for
/// primitives with nothing drawable.
fn pack(prim: &Primitive, words: &mut Vec<u32>, warnings: &mut Vec<String>) -> Option<GpuPrim> {
    let n = prim.positions.len();
    if n == 0 || n > u32::MAX as usize / 64 {
        return None;
    }
    let has = |len: usize| len == n;

    // 1. Indices, dropping whole triangles (or lines) that point outside
    //    the vertices.
    let group = match prim.topology {
        Topology::Triangles => 3,
        Topology::Lines => 2,
        Topology::Points => 1,
    };
    let index_first = words.len() as u32;
    let mut dropped = 0usize;
    for g in prim.indices.chunks_exact(group) {
        if g.iter().all(|&i| (i as usize) < n) {
            words.extend_from_slice(g);
        } else {
            dropped += 1;
        }
    }
    if dropped > 0 {
        warnings.push(format!("dropped {dropped} primitives with indices past the vertex data"));
    }
    let index_count = words.len() as u32 - index_first;
    if index_count == 0 {
        words.truncate(index_first as usize);
        return None;
    }

    // 2. Attributes.  Triangles without normals get smooth ones.
    let mut attrs0 = [NONE; 4];
    let mut attrs1 = [NONE; 4];
    attrs0[0] = words.len() as u32;
    push_f32s(words, bytemuck::cast_slice(&prim.positions));
    if has(prim.normals.len()) {
        attrs0[1] = words.len() as u32;
        push_f32s(words, bytemuck::cast_slice(&prim.normals));
    } else if prim.topology == Topology::Triangles {
        let mut tmp = Primitive {
            positions: prim.positions.clone(),
            indices: words[index_first as usize..(index_first + index_count) as usize].to_vec(),
            ..Default::default()
        };
        tmp.ensure_normals();
        attrs0[1] = words.len() as u32;
        push_f32s(words, bytemuck::cast_slice(&tmp.normals));
    }
    if has(prim.tangents.len()) {
        attrs0[2] = words.len() as u32;
        push_f32s(words, bytemuck::cast_slice(&prim.tangents));
    }
    if has(prim.uv0.len()) {
        attrs0[3] = words.len() as u32;
        push_f32s(words, bytemuck::cast_slice(&prim.uv0));
    }
    if has(prim.uv1.len()) {
        attrs1[0] = words.len() as u32;
        push_f32s(words, bytemuck::cast_slice(&prim.uv1));
    }
    if has(prim.colors.len()) {
        attrs1[1] = words.len() as u32;
        push_f32s(words, bytemuck::cast_slice(&prim.colors));
    }
    let skinned = has(prim.joints.len()) && has(prim.weights.len());
    let mut joint_boxes: Vec<(u32, Aabb)> = Vec::new();
    if skinned {
        attrs1[2] = words.len() as u32;
        for j in &prim.joints {
            words.push(j[0] as u32 | (j[1] as u32) << 16);
            words.push(j[2] as u32 | (j[3] as u32) << 16);
        }
        attrs1[3] = words.len() as u32;
        push_f32s(words, bytemuck::cast_slice(&prim.weights));
        // Group vertices by their strongest joint.
        let mut boxes: std::collections::BTreeMap<u32, Aabb> = Default::default();
        for ((p, j), w) in prim.positions.iter().zip(&prim.joints).zip(&prim.weights) {
            let k = (0..4).max_by(|&a, &b| w[a].total_cmp(&w[b])).unwrap_or(0);
            boxes.entry(j[k] as u32).or_insert(Aabb::EMPTY).add(*p);
        }
        joint_boxes = boxes.into_iter().collect();
    }

    // 3. Morph targets, skipping ones that move nothing (a face's shapes
    //    are often split over primitives that never move).
    let morph_normals = prim.targets.iter().any(|t| has(t.normals.len()));
    let mut targets = Vec::with_capacity(prim.targets.len());
    for t in &prim.targets {
        let moves = has(t.positions.len()) && t.positions.iter().any(|d| *d != Vec3::ZERO);
        let bends = has(t.normals.len()) && t.normals.iter().any(|d| *d != Vec3::ZERO);
        if !moves && !bends {
            targets.push(NONE);
            continue;
        }
        targets.push(words.len() as u32);
        if has(t.positions.len()) {
            push_f32s(words, bytemuck::cast_slice(&t.positions));
        } else {
            words.extend(std::iter::repeat_n(0, n * 3));
        }
        if morph_normals {
            if has(t.normals.len()) {
                push_f32s(words, bytemuck::cast_slice(&t.normals));
            } else {
                words.extend(std::iter::repeat_n(0, n * 3));
            }
        }
    }

    let mut local_bounds = Aabb::EMPTY;
    prim.positions.iter().for_each(|p| local_bounds.add(*p));
    let point_spacing = if prim.topology == Topology::Points {
        let s = crate::points::point_spacing(&prim.positions);
        // A lone point (or a pile of them in one place) gets a dot a
        // hundredth of its surroundings' size; the frame sizes it.
        if s > 0.0 {
            s
        } else if local_bounds.is_empty() {
            0.0
        } else {
            local_bounds.size().length() * 0.01
        }
    } else {
        0.0
    };
    let grey = |c: &glam::Vec4| (c.x - c.y).abs() < 1e-3 && (c.y - c.z).abs() < 1e-3;
    let point_ao = prim.topology == Topology::Points && (!has(prim.colors.len()) || prim.colors.iter().all(grey));
    Some(GpuPrim {
        chunk: 0,
        index_first,
        index_count,
        vertex_count: n as u32,
        attrs0,
        attrs1,
        targets,
        morph_normals,
        topology: prim.topology,
        material: 0,
        local_bounds,
        joint_boxes,
        point_spacing,
        point_ao,
    })
}

/// Words a primitive will need, to start a new chunk before it overflows.
fn words_needed(prim: &Primitive) -> u64 {
    prim.indices.len() as u64 + prim.positions.len() as u64 * vertex_words(prim)
}

/// Words each vertex of a primitive takes at most: its position and
/// normal (computed when missing), the optional attributes it has, and
/// every morph target's position and normal offsets.
fn vertex_words(prim: &Primitive) -> u64 {
    let n = prim.positions.len();
    let has = |len: usize| len == n;
    let optional = [(prim.tangents.len(), 4), (prim.uv0.len(), 2), (prim.uv1.len(), 2), (prim.colors.len(), 4)];
    let mut words = 3 + 3 + optional.iter().filter(|(len, _)| has(*len)).map(|(_, w)| w).sum::<u64>();
    if has(prim.joints.len()) && has(prim.weights.len()) {
        words += 2 + 4;
    }
    words + prim.targets.len() as u64 * 6
}

/// Splits a primitive too large for one storage binding of `max_words`
/// into pieces that fit, each holding a run of its triangles (or lines, or
/// points) and only the vertices those use, so a huge mesh (a whole
/// Minecraft terrain's grass sides, a city scan) is drawn in parts rather
/// than skipped.  Missing normals are computed on the whole primitive
/// first, so the pieces' shading meets without seams.  Groups pointing
/// past the vertices are left out, as `pack` would drop them anyway.
fn split(prim: &Primitive, max_words: u64) -> Vec<Primitive> {
    let n = prim.positions.len();
    let group = match prim.topology {
        Topology::Triangles => 3,
        Topology::Lines => 2,
        Topology::Points => 1,
    };
    let smooth;
    let normals = if prim.normals.len() == n || prim.topology != Topology::Triangles {
        &prim.normals
    } else {
        let mut whole =
            Primitive { positions: prim.positions.clone(), indices: prim.indices.clone(), ..Default::default() };
        whole.ensure_normals();
        smooth = whole.normals;
        &smooth
    };
    let per_vertex = vertex_words(prim);
    // 1. Runs of groups, each with the vertices it uses renumbered in the
    //    order first used.  `piece_of` remembers the piece a vertex was
    //    last given to, `new_index` its number there.
    let mut piece_of = vec![usize::MAX; n];
    let mut new_index = vec![0u32; n];
    let mut runs: Vec<(Vec<u32>, Vec<u32>)> = Vec::new();
    let (mut used, mut indices): (Vec<u32>, Vec<u32>) = (Vec::new(), Vec::new());
    for g in prim.indices.chunks_exact(group) {
        if g.iter().any(|&i| i as usize >= n) {
            continue;
        }
        let fresh = g.iter().filter(|&&i| piece_of[i as usize] != runs.len()).count() as u64;
        let words = (indices.len() + group) as u64 + (used.len() as u64 + fresh) * per_vertex;
        if words > max_words && !indices.is_empty() {
            runs.push((std::mem::take(&mut used), std::mem::take(&mut indices)));
        }
        for &i in g {
            let i = i as usize;
            if piece_of[i] != runs.len() {
                piece_of[i] = runs.len();
                new_index[i] = used.len() as u32;
                used.push(i as u32);
            }
            indices.push(new_index[i]);
        }
    }
    if !indices.is_empty() {
        runs.push((used, indices));
    }
    // 2. Each run's vertices gathered from the primitive.
    fn pick<T: Copy>(all: &[T], used: &[u32], n: usize) -> Vec<T> {
        if all.len() == n { used.iter().map(|&i| all[i as usize]).collect() } else { Vec::new() }
    }
    runs.into_iter()
        .map(|(used, indices)| Primitive {
            positions: pick(&prim.positions, &used, n),
            normals: pick(normals, &used, n),
            tangents: pick(&prim.tangents, &used, n),
            uv0: pick(&prim.uv0, &used, n),
            uv1: pick(&prim.uv1, &used, n),
            colors: pick(&prim.colors, &used, n),
            joints: pick(&prim.joints, &used, n),
            weights: pick(&prim.weights, &used, n),
            indices,
            topology: prim.topology,
            targets: prim
                .targets
                .iter()
                .map(|t| MorphTarget { positions: pick(&t.positions, &used, n), normals: pick(&t.normals, &used, n) })
                .collect(),
            material: prim.material,
        })
        .collect()
}

/// Moves a primitive packed on its own to word `base` of its chunk.
fn rebase(gp: &mut GpuPrim, base: u32) {
    gp.index_first += base;
    for w in gp.attrs0.iter_mut().chain(gp.attrs1.iter_mut()).chain(gp.targets.iter_mut()) {
        if *w != NONE {
            *w += base;
        }
    }
}

/// A scene's geometry packed into the words of its storage buffers.  That
/// is CPU work only (a large character's morph targets take tens of
/// milliseconds), so a thread loading the scene can do it with
/// [`pack_geometry`] and hand it to [`Renderer::prepare_packed`], which
/// then only uploads it.
pub struct PackedGeometry {
    chunks: Vec<Vec<u32>>,
    prims: Vec<GpuPrim>,
    mesh_prims: Vec<Vec<usize>>,
    warnings: Vec<String>,
    /// The chunk size it was packed for, in words.
    max_words: u64,
    /// Primitives per mesh, to check it is still the scene's.
    shape: Vec<usize>,
}

/// The storage binding size every GPU allows, which [`pack_geometry`]
/// packs for when the GPU isn't known yet.
pub const DEFAULT_BINDING_BYTES: u64 = 128 << 20;

fn shape(scene: &Scene) -> Vec<usize> {
    scene.meshes.iter().map(|m| m.primitives.len()).collect()
}

/// Packs `scene`'s geometry, primitives in parallel, into chunks of at most
/// `max_binding_bytes` (the GPU's largest storage binding, or
/// [`DEFAULT_BINDING_BYTES`]).
pub fn pack_geometry(scene: &Scene, max_binding_bytes: u64) -> PackedGeometry {
    use rayon::prelude::*;
    let max_words = (max_binding_bytes.min(1 << 30) / 4).max(1);
    // 1. Each primitive on its own, split into pieces when it is larger
    //    than one chunk.
    let flat: Vec<&Primitive> = scene.meshes.iter().flat_map(|m| &m.primitives).collect();
    // Its pieces' words and records (none for nothing drawable), and its
    // warnings.
    type Alone = (Vec<(Vec<u32>, GpuPrim)>, Vec<String>);
    let alone: Vec<Alone> = flat
        .par_iter()
        .map(|prim| {
            let mut warnings = Vec::new();
            let mut pack_one = |prim: &Primitive| {
                let mut words = Vec::with_capacity(words_needed(prim) as usize);
                pack(prim, &mut words, &mut warnings).map(|mut gp| {
                    gp.material = prim.material.filter(|&m| m < scene.materials.len()).unwrap_or(scene.materials.len());
                    (words, gp)
                })
            };
            let packed = if words_needed(prim) > max_words {
                split(prim, max_words).iter().filter_map(&mut pack_one).collect()
            } else {
                pack_one(prim).into_iter().collect()
            };
            (packed, warnings)
        })
        .collect();
    // 2. Into chunks no larger than a binding, in order.
    let mut out = PackedGeometry {
        chunks: Vec::new(),
        prims: Vec::new(),
        mesh_prims: Vec::with_capacity(scene.meshes.len()),
        warnings: Vec::new(),
        max_words,
        shape: shape(scene),
    };
    let mut current: Vec<u32> = Vec::new();
    let mut alone = alone.into_iter();
    for mesh in &scene.meshes {
        let mut list = Vec::new();
        for _ in &mesh.primitives {
            let Some((pieces, warnings)) = alone.next() else { break };
            out.warnings.extend(warnings);
            for (words, mut gp) in pieces {
                if !current.is_empty() && current.len() as u64 + words.len() as u64 > max_words {
                    out.chunks.push(std::mem::take(&mut current));
                }
                rebase(&mut gp, current.len() as u32);
                gp.chunk = out.chunks.len();
                current.extend_from_slice(&words);
                list.push(out.prims.len());
                out.prims.push(gp);
            }
        }
        out.mesh_prims.push(list);
    }
    if !current.is_empty() {
        out.chunks.push(current);
    }
    out
}

//endregion

impl Renderer {
    /// Uploads meshes, materials and textures, and poses the scene at rest.
    pub fn prepare(&mut self, gpu: &Gpu, scene: &Scene) -> GpuScene {
        self.prepare_with(gpu, scene, false, None)
    }

    /// Uploads geometry and materials but leaves the textures for
    /// [`Renderer::upload_textures`], so a first frame can draw sooner.
    /// Until a texture arrives, its slot shows the texture's stored mip
    /// levels of up to 64 texels, when the file has them (Basis and other
    /// GPU textures do), or else the material's plain colour.  A scene with
    /// only a few megabytes of textures gets them at once.
    pub fn prepare_untextured(&mut self, gpu: &Gpu, scene: &Scene) -> GpuScene {
        self.prepare_with(gpu, scene, true, None)
    }

    /// As [`Renderer::prepare_untextured`], with geometry packed beforehand
    /// by [`pack_geometry`] (on a loading thread, say).  It is packed again
    /// here if it doesn't fit this GPU or isn't `scene`'s.
    pub fn prepare_packed(&mut self, gpu: &Gpu, scene: &Scene, geometry: PackedGeometry) -> GpuScene {
        self.prepare_with(gpu, scene, true, Some(geometry))
    }

    /// Uploads textures [`Renderer::prepare_untextured`] left waiting,
    /// colour textures and small ones first, until `budget` has passed
    /// (always at least one), and rebinds the materials that use them.
    /// Returns whether any are still waiting.  `scene` must be the scene `gs`
    /// was prepared from.
    pub fn upload_textures(&mut self, gpu: &Gpu, gs: &mut GpuScene, scene: &Scene, budget: Duration) -> bool {
        let start = Instant::now();
        let mut done = vec![false; gs.textures.len()];
        while !gs.pending.is_empty() {
            let (i, srgb) = gs.pending.remove(0);
            let Some(tex) = scene.textures.get(i) else { continue };
            match upload_texture(gpu, &mut self.mipgen, tex, Surfaces::First, srgb, None) {
                Ok(t) => gs.textures[i] = Some(t),
                Err(e) => {
                    gs.warnings.push(format!("texture {i}: {e}"));
                    gs.textures[i] = None;
                }
            }
            done[i] = true;
            if start.elapsed() >= budget {
                break;
            }
        }
        let default = Material::default();
        for (mi, m) in scene.materials.iter().chain(std::iter::once(&default)).enumerate() {
            let uses = slots(m).iter().flatten().any(|r| done.get(r.texture).copied().unwrap_or(false));
            if uses && mi < gs.materials.len() {
                gs.materials[mi] = self.material(gpu, m, scene, &gs.textures);
            }
        }
        !gs.pending.is_empty()
    }

    /// Whether textures are still waiting for [`Renderer::upload_textures`].
    pub fn textures_pending(gs: &GpuScene) -> bool {
        !gs.pending.is_empty()
    }

    fn prepare_with(&mut self, gpu: &Gpu, scene: &Scene, later: bool, packed: Option<PackedGeometry>) -> GpuScene {
        // Textures that upload in a few milliseconds go up at once, so the
        // first frame is the whole picture; later only pays for large ones.
        let later = later && texture_bytes(scene) > LATER_ABOVE_BYTES;
        let device = &gpu.device;
        let limits = device.limits();
        let mut warnings = Vec::new();

        let step = crate::trace::Step::new("geometry");
        // 1. Geometry, packed into chunks no larger than a storage binding,
        //    unless the loader packed it already.
        let max_words = limits.max_storage_buffer_binding_size.min(limits.max_buffer_size).min(1 << 30) / 4;
        let geometry = match packed {
            Some(g) if g.max_words <= max_words && g.shape == shape(scene) => g,
            _ => pack_geometry(scene, max_words * 4),
        };
        warnings.extend(geometry.warnings);
        let chunks: Vec<Chunk> = geometry
            .chunks
            .iter()
            .map(|words| {
                let buffer = buffer_init(
                    device,
                    "geometry",
                    bytemuck::cast_slice(words),
                    wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::INDEX,
                );
                let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("geometry"),
                    layout: &self.layouts.geometry,
                    entries: &[wgpu::BindGroupEntry { binding: 0, resource: buffer.as_entire_binding() }],
                });
                Chunk { buffer, group }
            })
            .collect();
        let prims = geometry.prims;
        let mesh_prims = geometry.mesh_prims;

        step.done(device);
        let step = crate::trace::Step::new("textures");
        // 2. Textures, uploaded only when a material uses them; colour slots
        //    generate their mips in linear light.
        let mut color_use = vec![false; scene.textures.len()];
        let mut any_use = vec![false; scene.textures.len()];
        for m in &scene.materials {
            for (slot, r) in slots(m).iter().enumerate() {
                if let Some(r) = r
                    && r.texture < scene.textures.len()
                {
                    any_use[r.texture] = true;
                    color_use[r.texture] |= is_color_slot(slot);
                }
            }
        }
        let mut textures = Vec::with_capacity(scene.textures.len());
        let mut pending = Vec::new();
        for (i, tex) in scene.textures.iter().enumerate() {
            if !any_use[i] {
                textures.push(None);
                continue;
            }
            let srgb = color_use[i] && tex.color_space != slerp_scene::ColorSpace::Linear;
            if later {
                textures.push(upload_preview(gpu, tex, PREVIEW_SIDE));
                pending.push((i, color_use[i]));
                continue;
            }
            match upload_texture(gpu, &mut self.mipgen, tex, Surfaces::First, srgb, None) {
                Ok(t) => textures.push(Some(t)),
                Err(e) => {
                    warnings.push(format!("texture {i}: {e}"));
                    textures.push(None);
                }
            }
        }

        // Colour first, since that is what shows most, then the smallest.
        let bytes = |i: usize| scene.textures[i].levels.first().map_or(0, |l| l.width as u64 * l.height as u64);
        pending.sort_by_key(|&(i, color)| (!color, bytes(i)));
        let pending = pending
            .into_iter()
            .map(|(i, _)| (i, color_use[i] && scene.textures[i].color_space != slerp_scene::ColorSpace::Linear))
            .collect();

        step.done(device);
        let step = crate::trace::Step::new("materials and pose");
        // 3. Materials, plus a neutral default at the end.
        let mut materials: Vec<GpuMaterial> = Vec::with_capacity(scene.materials.len() + 1);
        let default = Material::default();
        for m in scene.materials.iter().chain(std::iter::once(&default)) {
            materials.push(self.material(gpu, m, scene, &textures));
        }

        // 4. Draws: every mesh primitive of every node reachable from the
        //    roots.
        let parents = scene.parents();
        let mut reachable = vec![false; scene.nodes.len()];
        let mut stack: Vec<usize> = scene.roots.clone();
        while let Some(n) = stack.pop() {
            if n < reachable.len() && !reachable[n] {
                reachable[n] = true;
                stack.extend(scene.nodes[n].children.iter().copied());
            }
        }
        let mut draws = Vec::new();
        for (ni, node) in scene.nodes.iter().enumerate() {
            let Some(list) = node.mesh.and_then(|m| mesh_prims.get(m)) else { continue };
            if !reachable[ni] {
                continue;
            }
            let skin = node.skin.filter(|&s| s < scene.skins.len());
            for &p in list {
                let skinned = prims[p].attrs1[2] != NONE;
                let sky = scene.materials.get(prims[p].material).is_some_and(|m| m.effect.is_sky());
                draws.push(DrawItem {
                    node: ni,
                    prim: p,
                    skin: if skinned { skin } else { None },
                    visible: true,
                    sky,
                    mirrored: false,
                    center: Vec3::ZERO,
                    bounds: Aabb::EMPTY,
                    rigid: true,
                });
            }
        }
        let groups = group_draws(&draws, prims.len());
        let buffer = |label, size: u64| {
            let size = size.max(256);
            (
                device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some(label),
                    size,
                    usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                }),
                size,
            )
        };
        let buffers = [
            buffer("draws", (draws.len() * std::mem::size_of::<DrawUniform>()) as u64),
            buffer("joints", (scene.skins.iter().map(|s| s.joints.len()).sum::<usize>() * 64) as u64),
            buffer("morphs", 256),
        ];
        let instances = buffer("instances", (draws.len() * 2 * 4) as u64).0;
        // 5. Gaussian splats.
        let splats = match &scene.splats {
            Some(s) if !s.is_empty() => self.prepare_splats(gpu, s, &mut warnings),
            _ => None,
        };
        let mut gs = GpuScene {
            id: NEXT_ID.fetch_add(1, Ordering::Relaxed),
            pose_version: 0,
            chunks,
            prims,
            materials,
            textures,
            pending,
            draws,
            groups,
            listed: Vec::new(),
            instances,
            parents,
            reachable,
            draw_data: Vec::new(),
            joint_data: Vec::new(),
            morph_data: Vec::new(),
            buffers,
            frame_groups: Vec::new(),
            rest_bounds: Aabb::EMPTY,
            bounds: Aabb::EMPTY,
            ground_height: 0.0,
            ground_shadow: scene.kind != SceneKind::Level,
            bones: Vec::new(),
            markers: scene.markers.clone(),
            warnings,
            splats,
        };
        self.make_frame_groups(gpu, &mut gs);
        let rest = Pose::rest(scene);
        self.update_pose(gpu, &mut gs, scene, &rest);
        gs.rest_bounds = gs.bounds;
        gs.ground_height = if gs.bounds.is_empty() { 0.0 } else { gs.bounds.min.y };
        step.done(device);
        gs
    }

    fn make_frame_groups(&self, gpu: &Gpu, gs: &mut GpuScene) {
        gs.frame_groups = self
            .frame_buffers
            .iter()
            .map(|frame| {
                gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("frame"),
                    layout: &self.layouts.frame,
                    entries: &[
                        wgpu::BindGroupEntry { binding: 0, resource: frame.as_entire_binding() },
                        wgpu::BindGroupEntry { binding: 1, resource: gs.buffers[0].0.as_entire_binding() },
                        wgpu::BindGroupEntry { binding: 2, resource: gs.buffers[1].0.as_entire_binding() },
                        wgpu::BindGroupEntry { binding: 3, resource: gs.buffers[2].0.as_entire_binding() },
                        wgpu::BindGroupEntry { binding: 4, resource: gs.instances.as_entire_binding() },
                    ],
                })
            })
            .collect();
    }

    fn material(&mut self, gpu: &Gpu, m: &Material, scene: &Scene, textures: &[Option<GpuTexture>]) -> GpuMaterial {
        let refs = slots(m);
        let mut views = Vec::with_capacity(SLOTS);
        let mut samplers = Vec::with_capacity(SLOTS);
        for (slot, r) in refs.iter().enumerate() {
            match r.and_then(|r| slot_texture(&r, textures).map(|t| (r, t))) {
                Some((r, t)) => {
                    let srgb =
                        is_color_slot(slot) && scene.textures[r.texture].color_space != slerp_scene::ColorSpace::Linear;
                    views.push(t.view(srgb));
                    samplers.push(self.samplers.get(&gpu.device, &r.sampler));
                }
                None => {
                    views.push(if slot == 2 { self.flat_normal.clone() } else { self.white.clone() });
                    samplers.push(self.linear_clamp.clone());
                }
            }
        }
        let u = material_uniform(m, textures);
        let usage = wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST;
        let uniform = buffer_init(&gpu.device, "material", bytemuck::bytes_of(&u), usage);
        let mut entries = vec![wgpu::BindGroupEntry { binding: 0, resource: uniform.as_entire_binding() }];
        for (i, v) in views.iter().enumerate() {
            entries
                .push(wgpu::BindGroupEntry { binding: 1 + i as u32, resource: wgpu::BindingResource::TextureView(v) });
        }
        for (i, s) in samplers.iter().enumerate() {
            let binding = 1 + (SLOTS + i) as u32;
            entries.push(wgpu::BindGroupEntry { binding, resource: wgpu::BindingResource::Sampler(s) });
        }
        let group = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("material"),
            layout: &self.layouts.material,
            entries: &entries,
        });
        // A transmission texture alone can't make a surface see-through.
        let transmissive = u.transmission[0] > 0.0;
        GpuMaterial {
            group,
            uniform,
            blend: m.alpha_mode == AlphaMode::Blend,
            // Quake's faces, sky included, point into the playable space, so
            // culling back faces lets a view from outside see into a map.
            double_sided: m.double_sided,
            casts_shadow: !m.effect.is_sky() && !transmissive,
            transmissive,
            lit: !m.unlit && m.effect == Effect::None && m.lightmap_texture.is_none(),
            moves_vertices: moves_vertices(m),
        }
    }

    /// Rewrites material `index`'s uniform from `scene.materials[index]`
    /// after its factors, texture transforms or height scale changed, keeping
    /// its textures and bind group: a buffer write of a few hundred bytes,
    /// cheap enough for every frame of a slider drag.  Changes to its
    /// textures, alpha mode, transmission or sidedness need `prepare`.
    pub fn update_material(&self, gpu: &Gpu, gs: &mut GpuScene, scene: &Scene, index: usize) {
        let (Some(m), Some(g)) = (scene.materials.get(index), gs.materials.get_mut(index)) else { return };
        let u = material_uniform(m, &gs.textures);
        gpu.queue.write_buffer(&g.uniform, 0, bytemuck::bytes_of(&u));
        g.moves_vertices = moves_vertices(m);
        // Displacement and alpha change what the shadow maps hold, which are
        // drawn again only when the pose changes.
        gs.pose_version += 1;
    }

    /// Uploads joint matrices, morph weights and node transforms for a
    /// pose whose world matrices are current (`Pose::update_world`), and
    /// picks up nodes' visibility.  The viewer calls this whenever the pose
    /// or a visibility toggle changes.
    pub fn update_pose(&mut self, gpu: &Gpu, gs: &mut GpuScene, scene: &Scene, pose: &Pose) {
        // 1. Effective visibility: a hidden node hides its subtree.
        let n = scene.nodes.len().min(gs.parents.len());
        let mut visible = vec![None::<bool>; n];
        fn vis(i: usize, scene: &Scene, parents: &[Option<usize>], memo: &mut [Option<bool>], depth: u32) -> bool {
            if let Some(v) = memo[i] {
                return v;
            }
            let own = scene.nodes[i].visible;
            let v = own
                && match parents[i] {
                    Some(p) if p < memo.len() && depth < 4096 => vis(p, scene, parents, memo, depth + 1),
                    _ => true,
                };
            memo[i] = Some(v);
            v
        }

        // 2. Joint matrices, one block per skin.
        gs.joint_data.clear();
        let mut skin_base = Vec::with_capacity(scene.skins.len());
        for s in 0..scene.skins.len() {
            skin_base.push(gs.joint_data.len() as u32);
            gs.joint_data.extend(pose.joint_matrices(scene, s));
        }

        // 3. Draw records, active morphs and bounds.
        gs.draw_data.clear();
        gs.morph_data.clear();
        let mut bounds = Aabb::EMPTY;
        for d in gs.draws.iter_mut() {
            let prim = &gs.prims[d.prim];
            d.visible = d.node < n && vis(d.node, scene, &gs.parents, &mut visible, 0) && gs.reachable[d.node];
            let world = pose.world.get(d.node).copied().unwrap_or(Mat4::IDENTITY);
            // glTF takes the winding from the node's world matrix, skinned
            // or not.
            d.mirrored = world.determinant() < 0.0;
            let model = if d.skin.is_some() { Mat4::IDENTITY } else { world };
            let normal = if model.determinant().abs() > 1e-20 { model.inverse().transpose() } else { Mat4::IDENTITY };
            let first = gs.morph_data.len() as u32;
            if let Some(weights) = pose.weights.get(d.node) {
                for (t, &w) in weights.iter().enumerate() {
                    let Some(&off) = prim.targets.get(t) else { break };
                    if off != NONE && w.is_finite() && w.abs() > 1e-5 {
                        gs.morph_data.push([off, w.to_bits()]);
                    }
                }
            }
            let count = gs.morph_data.len() as u32 - first;
            let (joint_base, attrs1) = match d.skin {
                Some(s) => (skin_base[s], prim.attrs1),
                None => (0, [prim.attrs1[0], prim.attrs1[1], NONE, NONE]),
            };
            // Points carry their spacing in world units (the mesh's spacing
            // times the node's scale), negative when ambient occlusion
            // leaves them alone.
            let spacing = prim.point_spacing * model.determinant().abs().cbrt();
            let spacing = if prim.point_ao { spacing } else { -spacing };
            gs.draw_data.push(DrawUniform {
                model: model.to_cols_array_2d(),
                normal: normal.to_cols_array_2d(),
                attrs0: prim.attrs0,
                attrs1,
                morph: [first, count, prim.vertex_count, prim.morph_normals as u32],
                misc: [joint_base, prim.index_first, spacing.to_bits(), prim.material as u32],
            });

            let mut b = Aabb::EMPTY;
            match d.skin {
                Some(s) => {
                    let base = skin_base[s] as usize;
                    let count = scene.skins[s].joints.len();
                    for (j, jb) in &prim.joint_boxes {
                        let m = if (*j as usize) < count { gs.joint_data[base + *j as usize] } else { Mat4::IDENTITY };
                        add_box(&mut b, jb, &m);
                    }
                }
                None => add_box(&mut b, &prim.local_bounds, &model),
            }
            d.center = if b.is_empty() { world.w_axis.truncate() } else { b.center() };
            d.bounds = b;
            d.rigid = d.skin.is_none() && count == 0;
            if d.visible && !d.sky {
                bounds.union(&b);
            }
        }
        list_instances(&mut gs.groups, &gs.draws, &mut gs.listed);
        if !gs.listed.is_empty() {
            gpu.queue.write_buffer(&gs.instances, 0, bytemuck::cast_slice(&gs.listed));
        }
        if let Some(s) = &gs.splats {
            bounds.union(&s.bounds);
        }
        gs.bounds = bounds;

        // 4. Upload, growing buffers (and rebinding) when needed.
        let mut grown = false;
        let datas: [&[u8]; 3] = [
            bytemuck::cast_slice(&gs.draw_data),
            bytemuck::cast_slice(&gs.joint_data),
            bytemuck::cast_slice(&gs.morph_data),
        ];
        for (i, data) in datas.iter().enumerate() {
            if data.len() as u64 > gs.buffers[i].1 {
                let size = (data.len() as u64).next_power_of_two();
                gs.buffers[i] = (
                    gpu.device.create_buffer(&wgpu::BufferDescriptor {
                        label: Some("scene data"),
                        size,
                        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                        mapped_at_creation: false,
                    }),
                    size,
                );
                grown = true;
            }
            if !data.is_empty() {
                gpu.queue.write_buffer(&gs.buffers[i].0, 0, data);
            }
        }
        if grown {
            self.make_frame_groups(gpu, gs);
        }

        // 5. Bones for the skeleton overlay.
        gs.bones.clear();
        let mut is_joint = vec![false; scene.nodes.len()];
        for s in &scene.skins {
            for &j in &s.joints {
                if j < is_joint.len() {
                    is_joint[j] = true;
                }
            }
        }
        for (j, _) in is_joint.iter().enumerate().filter(|(_, b)| **b) {
            let Some(p) = gs.parents.get(j).copied().flatten() else { continue };
            if p < is_joint.len() && is_joint[p] {
                let a = pose.world.get(p).map_or(Vec3::ZERO, |m| m.w_axis.truncate());
                let b = pose.world.get(j).map_or(Vec3::ZERO, |m| m.w_axis.truncate());
                gs.bones.push((a, b, j));
            }
        }
        gs.pose_version += 1;
    }
}

/// A slot's texture, when the scene has it on the GPU.
fn slot_texture<'a>(r: &TexRef, textures: &'a [Option<GpuTexture>]) -> Option<&'a GpuTexture> {
    textures.get(r.texture).and_then(|t| t.as_ref())
}

/// What a material's uniform holds: its factors, flags, height scale and
/// each texture slot's transform (identity rows for empty slots).
fn material_uniform(m: &Material, textures: &[Option<GpuTexture>]) -> MaterialUniform {
    let mut slot_rows = [[0f32; 8]; SLOTS];
    for (slot, r) in slots(m).iter().enumerate() {
        slot_rows[slot] = match r.filter(|r| slot_texture(r, textures).is_some()) {
            Some(r) => {
                let mt = r.transform.map(|t| t.matrix()).unwrap_or(glam::Mat3::IDENTITY);
                let uv_set = if r.uv_set > 0 { 1.0 } else { 0.0 };
                [mt.x_axis.x, mt.y_axis.x, mt.z_axis.x, uv_set, mt.x_axis.y, mt.y_axis.y, mt.z_axis.y, 1.0]
            }
            None => [1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0],
        };
    }
    let (alpha_mode, cutoff) = match m.alpha_mode {
        AlphaMode::Opaque => (0, 0.5),
        AlphaMode::Mask(c) => (1, c),
        AlphaMode::Blend => (2, 0.5),
    };
    let effect = match m.effect {
        Effect::None => 0,
        Effect::Warp => 1,
        Effect::Sky => 2,
        Effect::PanoramaSky => 3,
        Effect::Billboard => 4,
    };
    let finite = |v: f32, d: f32| if v.is_finite() { v } else { d };
    let transmission = finite(m.transmission, 0.0).clamp(0.0, 1.0);
    MaterialUniform {
        base_color: m.base_color.to_array().map(|v| finite(v, 1.0)),
        emissive: m.emissive.extend(0.0).to_array().map(|v| finite(v, 0.0)),
        specular: m.specular_color.extend(m.specular).to_array().map(|v| finite(v, 1.0)),
        pbr: [
            finite(m.metallic, 0.0),
            finite(m.roughness, 0.5),
            finite(m.normal_scale, 1.0),
            finite(m.occlusion_strength, 1.0),
        ],
        extra: [
            finite(cutoff, 0.5),
            finite(m.ior, 1.5).max(1.0),
            finite(m.clearcoat, 0.0),
            finite(m.clearcoat_roughness, 0.0),
        ],
        extra2: [finite(m.lightmap_scale, 1.0), finite(m.height_scale, 0.0), 0.0, 0.0],
        flags: [alpha_mode, m.unlit as u32, effect, m.double_sided as u32],
        transmission: [
            transmission,
            finite(m.thickness, 0.0).max(0.0),
            // Infinite (or nonsense) means nothing is absorbed: 0.
            if m.attenuation_distance.is_finite() { m.attenuation_distance.max(0.0) } else { 0.0 },
            0.0,
        ],
        attenuation: m.attenuation_color.extend(0.0).to_array().map(|v| finite(v, 1.0).clamp(0.0, 1.0)),
        fog: m.fog.map_or([0.0; 4], |f| {
            let c = f.color.to_array().map(|v| finite(v, 0.0).clamp(0.0, 1.0));
            [c[0], c[1], c[2], finite(f.density, 0.0).max(0.0)]
        }),
        fog_near: m.fog.map_or([1.0, 0.0, 0.0, 0.0], |f| {
            [finite(f.near_light, 1.0).clamp(0.0, 16.0), finite(f.near_distance, 0.0).max(0.0), 1.0, 0.0]
        }),
        slots: slot_rows,
    }
}

fn add_box(out: &mut Aabb, b: &Aabb, m: &Mat4) {
    if b.is_empty() {
        return;
    }
    for c in 0..8 {
        let p = Vec3::new(
            if c & 1 == 0 { b.min.x } else { b.max.x },
            if c & 2 == 0 { b.min.y } else { b.max.y },
            if c & 4 == 0 { b.min.z } else { b.max.z },
        );
        out.add(m.transform_point3(p));
    }
}

/// Whether the vertex shader moves a material's vertices away from their
/// positions: billboards and height-displaced surfaces.
fn moves_vertices(m: &Material) -> bool {
    m.effect == Effect::Billboard || (m.height_texture.is_some() && m.height_scale != 0.0)
}

/// A material's texture slots in the shader's order: base colour,
/// metallic-roughness, normal, occlusion, emissive, lightmap, height,
/// transmission, thickness.
fn slots(m: &Material) -> [Option<TexRef>; SLOTS] {
    [
        m.base_color_texture,
        m.metallic_roughness_texture,
        m.normal_texture,
        m.occlusion_texture,
        m.emissive_texture,
        m.lightmap_texture,
        m.height_texture,
        m.transmission_texture,
        m.thickness_texture,
    ]
}

/// Base colour and emissive hold colours (sRGB); the rest hold data.
fn is_color_slot(slot: usize) -> bool {
    slot == 0 || slot == 4
}

#[cfg(test)]
mod tests {
    use super::*;
    use slerp_scene::{Mesh, Node};

    /// A primitive too large for one binding is drawn in pieces that each
    /// fit, and together hold every triangle with its vertices unchanged.
    #[test]
    fn oversized_primitives_are_split_not_skipped() {
        // 1. A 40 x 40 grid of quads with UVs and a morph target, unshared
        //    corners, so it has 6,400 vertices.
        let mut prim = Primitive::default();
        for y in 0..40 {
            for x in 0..40 {
                let base = prim.positions.len() as u32;
                for (dx, dy) in [(0, 0), (1, 0), (1, 1), (0, 1)] {
                    prim.positions.push(Vec3::new((x + dx) as f32, (y + dy) as f32, 0.0));
                    prim.uv0.push(glam::Vec2::new(dx as f32, dy as f32));
                }
                prim.indices.extend([base, base + 1, base + 2, base, base + 2, base + 3]);
            }
        }
        let n = prim.positions.len();
        prim.targets.push(MorphTarget { positions: vec![Vec3::Z; n], normals: Vec::new() });
        let triangles: Vec<[Vec3; 3]> =
            prim.indices.chunks_exact(3).map(|t| [0, 1, 2].map(|k| prim.positions[t[k] as usize])).collect();
        let mut scene = Scene::default();
        scene.meshes.push(Mesh { primitives: vec![prim], ..Default::default() });
        let mut node = Node::new("grid".to_string());
        node.mesh = Some(0);
        scene.nodes.push(node);
        scene.roots = vec![0];

        // 2. Packed for bindings of 64 KiB, a tenth of what it needs.
        let max_bytes = 64 << 10;
        let packed = pack_geometry(&scene, max_bytes);
        assert!(packed.warnings.is_empty(), "{:?}", packed.warnings);
        assert!(packed.prims.len() > 5, "{} pieces", packed.prims.len());
        assert_eq!(packed.mesh_prims[0].len(), packed.prims.len());
        assert!(packed.chunks.iter().all(|c| c.len() as u64 <= max_bytes / 4));

        // 3. Every triangle comes back, in order, from the pieces' words.
        let mut back = Vec::new();
        for gp in &packed.prims {
            let words = &packed.chunks[gp.chunk];
            assert_ne!(gp.targets[0], NONE, "the morph target went with the piece");
            let at = |v: u32| {
                let o = (gp.attrs0[0] + v * 3) as usize;
                Vec3::new(f32::from_bits(words[o]), f32::from_bits(words[o + 1]), f32::from_bits(words[o + 2]))
            };
            let indices = &words[gp.index_first as usize..(gp.index_first + gp.index_count) as usize];
            back.extend(indices.chunks_exact(3).map(|t| [at(t[0]), at(t[1]), at(t[2])]));
        }
        assert_eq!(back, triangles);
    }
}
