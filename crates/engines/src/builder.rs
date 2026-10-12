//! Assembling the output scene from many sources: nodes the engine file
//! describes, meshes the engines generate, and model files placed once or
//! thousands of times.
//!
//! A model file is loaded once.  Its meshes, materials and textures are
//! appended to the output scene the first time, and every placement copies
//! only its nodes (and skins, whose joints are nodes), pointing at the
//! shared meshes.  The builder also keeps the budgets that stop a hostile
//! or merely enormous scene from exhausting memory: a cap on nodes and on
//! the triangles all placements draw together.

use slerp_scene::glam::{Mat4, Quat, Vec2, Vec3, Vec4};
use slerp_scene::{
    Aabb, Info, Limits, LoadError, Marker, Material, Mesh, Node, Pose, Primitive, Scene, SceneKind, Skin, Spawn,
    TexRef, Texture, Transform,
};
use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Scenes wider than this many metres open as levels: in the viewer's fly
/// camera, framed from above in thumbnails.  A room is smaller.
pub(crate) const LEVEL_EXTENT: f32 = 20.0;

/// The most nodes one scene may produce, counting every placement of every
/// model.  A real level has tens of thousands.
const MAX_NODES: usize = 2_000_000;

const MAX_WARNINGS: usize = 100;

/// The most vertices copied to give meshes other materials.
const MAX_COPIED_VERTICES: u64 = 30_000_000;

/// One model file as loaded, ready to be placed.
pub(crate) struct Model {
    /// The model's nodes with their mesh indices already pointing into the
    /// output scene.
    nodes: Vec<Node>,
    roots: Vec<usize>,
    skins: Vec<Skin>,
    /// Triangles one placement draws.
    triangles: u64,
    /// The model's materials in the output scene, with their names.
    materials: Vec<(String, usize)>,
    /// The size of the file's unit in metres (0.01 for FBX files in
    /// centimetres), which the model's root node scales by; the meshes
    /// themselves keep the file's numbers.
    pub unit: f32,
}

impl Model {
    /// The model's node names in its own order, for overrides that address
    /// nodes inside a placed model by name.
    pub fn node_names(&self) -> impl Iterator<Item = &str> {
        self.nodes.iter().map(|n| n.name.as_str())
    }

    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    pub fn mesh(&self, node: usize) -> Option<usize> {
        self.nodes.get(node).and_then(|n| n.mesh)
    }

    /// The first node, in the model's order, for which `pick(name, has a
    /// mesh)` holds.
    pub fn find(&self, pick: impl Fn(&str, bool) -> bool) -> Option<usize> {
        self.nodes.iter().position(|n| pick(&n.name, n.mesh.is_some()))
    }

    pub fn material_names(&self) -> impl Iterator<Item = &str> {
        self.materials.iter().map(|(n, _)| n.as_str())
    }

    /// The output material the model calls `name`.
    pub fn material(&self, name: &str) -> Option<usize> {
        self.materials.iter().find(|(n, _)| n == name).map(|&(_, i)| i)
    }
}

/// Where a placement's nodes went.
pub(crate) struct Placed {
    /// Output node of each of the model's nodes.
    pub nodes: Vec<usize>,
}

/// A camera found in the scene, a candidate for the level's spawn point.
pub(crate) struct CameraInfo {
    pub node: usize,
    /// The engine marks it as the one in use.
    pub current: bool,
}

pub(crate) struct Builder<'a> {
    pub limits: &'a Limits,
    pub scene: Scene,
    models: HashMap<PathBuf, Option<usize>>,
    model_list: Vec<Model>,
    /// Meshes with some materials replaced, by original mesh and
    /// replacements.
    overridden: HashMap<(usize, Vec<Option<usize>>), usize>,
    warnings: Vec<String>,
    pub cameras: Vec<CameraInfo>,
    /// Triangles drawn by all placements so far.
    drawn: u64,
    /// Set once a budget ran out, so the warning is given once.
    over_budget: bool,
    /// Vertices copied by `with_materials` so far.
    copied: u64,
    /// Force `SceneKind::Level` whatever the size (a GridMap level).
    pub level: bool,
}

impl<'a> Builder<'a> {
    pub fn new(limits: &'a Limits) -> Self {
        Builder {
            limits,
            scene: Scene::default(),
            models: HashMap::new(),
            model_list: Vec::new(),
            overridden: HashMap::new(),
            warnings: Vec::new(),
            cameras: Vec::new(),
            drawn: 0,
            over_budget: false,
            copied: 0,
            level: false,
        }
    }

    /// Notes a problem once.
    pub fn warn(&mut self, message: impl Into<String>) {
        let message = message.into();
        if self.warnings.len() < MAX_WARNINGS && !self.warnings.contains(&message) {
            self.warnings.push(message);
        }
    }

    fn budget_exhausted(&mut self) {
        if !self.over_budget {
            self.over_budget = true;
            self.warn("the scene is too large: only part of it is shown");
        }
    }

    /*┌─────────────────╮
    | Nodes and meshes |
    └─────────────────╯*/
    //region Nodes and meshes

    /// How many more nodes the scene can hold.
    pub fn room(&self) -> usize {
        MAX_NODES.saturating_sub(self.scene.nodes.len())
    }

    /// Adds a node under `parent`, or returns None when the node budget is
    /// used up.
    pub fn add_node(&mut self, parent: Option<usize>, node: Node) -> Option<usize> {
        if self.scene.nodes.len() >= MAX_NODES {
            self.budget_exhausted();
            return None;
        }
        let index = self.scene.nodes.len();
        if let Some(mesh) = node.mesh {
            let triangles = self.scene.meshes.get(mesh).map_or(0, mesh_triangles);
            if self.drawn.saturating_add(triangles) > self.limits.max_triangles {
                self.budget_exhausted();
                return None;
            }
            self.drawn += triangles;
        }
        self.scene.nodes.push(node);
        if let Some(p) = parent.and_then(|p| self.scene.nodes.get_mut(p)) {
            p.children.push(index);
        }
        Some(index)
    }

    /// Adds a node holding `mesh` (an index into the output meshes).
    pub fn add_mesh_node(
        &mut self,
        parent: Option<usize>,
        name: &str,
        transform: Transform,
        mesh: usize,
    ) -> Option<usize> {
        let mut node = Node::new(name);
        node.transform = transform;
        node.mesh = Some(mesh);
        self.add_node(parent, node)
    }

    pub fn add_mesh(&mut self, mesh: Mesh) -> usize {
        self.scene.meshes.push(mesh);
        self.scene.meshes.len() - 1
    }

    pub fn add_material(&mut self, material: Material) -> usize {
        self.scene.materials.push(material);
        self.scene.materials.len() - 1
    }

    pub fn add_texture(&mut self, texture: Arc<Texture>) -> usize {
        self.scene.textures.push(texture);
        self.scene.textures.len() - 1
    }

    /// A copy of `mesh` whose primitives wear the given materials instead:
    /// `materials[i]` replaces primitive `i`'s when it is `Some`.  Copies are
    /// shared by every node with the same replacements.
    pub fn with_materials(&mut self, mesh: usize, materials: &[Option<usize>]) -> usize {
        if materials.iter().all(Option::is_none) || mesh >= self.scene.meshes.len() {
            return mesh;
        }
        let key = (mesh, materials.to_vec());
        if let Some(&m) = self.overridden.get(&key) {
            return m;
        }
        // Each copy repeats the mesh's vertices, so their total is capped:
        // past it, nodes keep the mesh's own materials.
        let vertices = self.scene.meshes[mesh].primitives.iter().map(|p| p.positions.len() as u64).sum::<u64>();
        if self.copied.saturating_add(vertices) > MAX_COPIED_VERTICES {
            self.warn("too many material variants: some meshes keep their own materials");
            return mesh;
        }
        self.copied += vertices;
        let mut copy = self.scene.meshes[mesh].clone();
        for (p, replacement) in copy.primitives.iter_mut().zip(materials) {
            if let Some(r) = replacement {
                p.material = Some(*r);
            }
        }
        let index = self.add_mesh(copy);
        self.overridden.insert(key, index);
        index
    }

    /// A marker for a light or a camera at the world position of `node`,
    /// filled in by `finish`, which knows world transforms.
    pub fn add_marker(&mut self, label: String, color: Vec4) {
        self.scene.markers.push(Marker { label, position: Vec3::ZERO, color });
    }

    //endregion

    /*┌─────────────────╮
    | Models          |
    └─────────────────╯*/
    //region Models

    /// Loads the model file at `path` (glTF, GLB, FBX, OBJ, COLLADA, and the
    /// other mesh formats `slerp-models` reads) once, and returns its handle,
    /// or None with a warning when it can't be read.
    pub fn model(&mut self, path: &Path) -> Option<usize> {
        if let Some(&m) = self.models.get(path) {
            return m;
        }
        let result = self.load_model(path);
        let handle = match result {
            Ok(model) => {
                self.model_list.push(model);
                Some(self.model_list.len() - 1)
            }
            Err(e) => {
                let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                self.warn(format!("{name}: {e}"));
                None
            }
        };
        self.models.insert(path.to_path_buf(), handle);
        handle
    }

    pub fn model_info(&self, model: usize) -> Option<&Model> {
        self.model_list.get(model)
    }

    fn load_model(&mut self, path: &Path) -> Result<Model, LoadError> {
        slerp_scene::record_neighbour(path);
        if !self.limits.neighbours {
            return Err(LoadError::Unsupported("neighbouring files are off".into()));
        }
        // 1. Read and recognise it.
        let size = std::fs::metadata(path).map_err(|_| LoadError::Unsupported("missing".into()))?.len();
        if size > self.limits.max_file_bytes {
            return Err(LoadError::TooLarge(format!("{size} bytes")));
        }
        let mut head = Vec::new();
        std::fs::File::open(path)?.take(4096).read_to_end(&mut head)?;
        let asset = if slerp_gltf::sniff(path, &head) {
            let bytes = std::fs::read(path)?;
            slerp_gltf::load(path, &bytes, self.limits)?
        } else if let Some(format) = slerp_models::sniff(path, &head) {
            let bytes = std::fs::read(path)?;
            slerp_models::load(format, path, &bytes, self.limits)?
        } else {
            return Err(LoadError::Unsupported("not a model format Slerp reads".into()));
        };
        let slerp_scene::Asset::Scene(mut loaded) = asset else {
            return Err(LoadError::Unsupported("not a model".into()));
        };
        for n in &loaded.info.neighbours {
            slerp_scene::record_neighbour(n);
        }
        for w in loaded.info.warnings.iter().take(5) {
            let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            self.warn(format!("{name}: {w}"));
        }

        // 2. Append what placements share.
        let texture_base = self.scene.textures.len();
        let material_base = self.scene.materials.len();
        let mesh_base = self.scene.meshes.len();
        self.scene.textures.append(&mut loaded.textures);
        let mut materials = Vec::new();
        for mut m in loaded.materials.drain(..) {
            shift_textures(&mut m, texture_base);
            materials.push((m.name.clone(), self.scene.materials.len()));
            self.scene.materials.push(m);
        }
        let mut triangles = 0u64;
        let mesh_triangles: Vec<u64> = loaded.meshes.iter().map(mesh_triangles).collect();
        for mut mesh in loaded.meshes.drain(..) {
            for p in &mut mesh.primitives {
                p.material = p.material.map(|m| m + material_base);
            }
            self.scene.meshes.push(mesh);
        }
        // 3. Keep the nodes as a template.  Hidden nodes stay hidden, as
        //    the file wants.
        let mut nodes = std::mem::take(&mut loaded.nodes);
        for n in &mut nodes {
            if let Some(m) = n.mesh {
                triangles += mesh_triangles.get(m).copied().unwrap_or(0);
                n.mesh = Some(m + mesh_base);
            }
        }
        // The unit: FBX files keep their scale to metres on the root node
        // (under the node turning Z-up files upright, when there is one).
        let mut top = loaded.roots.first().copied();
        if let Some(t) = top
            && nodes[t].name == "root"
            && nodes[t].children.len() == 1
        {
            top = nodes[t].children.first().copied();
        }
        let unit = top
            .and_then(|t| nodes.get(t))
            .map(|n| n.transform.scale.x)
            .filter(|u| u.is_finite() && *u > 0.0)
            .unwrap_or(1.0);
        Ok(Model { nodes, roots: loaded.roots, skins: loaded.skins, triangles, materials, unit })
    }

    /// Places a loaded model under `parent` inside a new node called `name`
    /// with `transform`.  Returns where its nodes went, or None when the
    /// scene's budgets are used up.
    pub fn place_model(
        &mut self,
        model: usize,
        parent: Option<usize>,
        name: &str,
        transform: Transform,
    ) -> Option<Placed> {
        let (triangles, count) = {
            let template = self.model_list.get(model)?;
            (template.triangles, template.nodes.len())
        };
        if self.drawn.saturating_add(triangles) > self.limits.max_triangles
            || self.scene.nodes.len().saturating_add(count + 1) > MAX_NODES
        {
            self.budget_exhausted();
            return None;
        }
        self.drawn += triangles;
        // 1. The wrapper, then the model's nodes in their own order, so
        //    child indices shift by a constant.
        let mut root = Node::new(name);
        root.transform = transform;
        let root_index = self.scene.nodes.len();
        self.scene.nodes.push(root);
        if let Some(p) = parent.and_then(|p| self.scene.nodes.get_mut(p)) {
            p.children.push(root_index);
        }
        let base = self.scene.nodes.len();
        let skin_base = self.scene.skins.len();
        let template = &self.model_list[model];
        for n in &template.nodes {
            let mut copy = n.clone();
            copy.children = n.children.iter().filter(|&&c| c < template.nodes.len()).map(|&c| c + base).collect();
            copy.skin = n.skin.filter(|&s| s < template.skins.len()).map(|s| s + skin_base);
            self.scene.nodes.push(copy);
        }
        for s in &template.skins {
            self.scene.skins.push(Skin {
                name: s.name.clone(),
                joints: s.joints.iter().map(|&j| j + base).collect(),
                inverse_bind: s.inverse_bind.clone(),
                skeleton: s.skeleton.map(|j| j + base),
            });
        }
        let roots: Vec<usize> =
            template.roots.iter().filter(|&&r| r < template.nodes.len()).map(|&r| r + base).collect();
        let count = template.nodes.len();
        self.scene.nodes[root_index].children.extend(roots);
        Some(Placed { nodes: (base..base + count).collect() })
    }

    //endregion

    /*┌─────────────────╮
    | Finishing       |
    └─────────────────╯*/
    //region Finishing

    /// The finished scene: roots, normals, markers at their world
    /// positions, the kind (a level when it is wide or the loader said so)
    /// and a spawn point at the scene's camera.
    pub fn finish(mut self, format: String, marker_nodes: &[usize]) -> Scene {
        self.scene.compute_roots();
        self.scene.ensure_normals();
        let pose = Pose::rest(&self.scene);
        let bounds = self.scene.bounds(&pose);
        // 1. Markers sit where their nodes ended up.
        for (marker, &node) in self.scene.markers.iter_mut().zip(marker_nodes) {
            if let Some(m) = pose.world.get(node) {
                marker.position = m.w_axis.truncate();
            }
        }
        // 2. Thumbnails frame what was built, not the kilometres of ground
        //    a level puts around it.
        if self.limits.max_texture_size.is_some() {
            self.crop_grounds(&pose);
        }
        // 3. Levels open in the fly camera, at the game's camera when it
        //    looks roughly level.
        let size = bounds.size();
        if !bounds.is_empty() && (self.level || size.x.max(size.z) > LEVEL_EXTENT) {
            self.scene.kind = SceneKind::Level;
            let camera = self.cameras.iter().find(|c| c.current).or_else(|| self.cameras.first());
            if let Some(m) = camera.and_then(|c| pose.world.get(c.node)) {
                let forward = m.transform_vector3(Vec3::NEG_Z).normalize_or_zero();
                let pitch = forward.y.clamp(-1.0, 1.0).asin();
                if forward != Vec3::ZERO && pitch.abs() < 50f32.to_radians() && m.is_finite() {
                    self.scene.spawn =
                        Some(Spawn { position: m.w_axis.truncate(), yaw: (-forward.x).atan2(-forward.z) });
                }
            }
        }
        self.scene.info = Info {
            format,
            warnings: self.warnings,
            details: std::mem::take(&mut self.scene.info.details),
            ..Default::default()
        };
        self.scene
    }

    /// Cuts ground meshes that reach far beyond everything else down to the
    /// area around the rest of the scene: a level's floor or terrain often
    /// runs for kilometres around a town a hundred metres across, which a
    /// thumbnail framing the whole would shrink to a dot.  A ground is a
    /// nearly flat visible mesh node over 200 m wide and at least six times
    /// as wide as all other meshes together (an arena's floor around a few
    /// players is not one); its triangles are clipped to the others' extent
    /// with a margin.
    fn crop_grounds(&mut self, pose: &Pose) {
        // 1. World boxes of the visible mesh nodes.
        let boxes: Vec<(usize, Aabb)> = self
            .scene
            .nodes
            .iter()
            .enumerate()
            .filter(|(_, n)| n.visible)
            .filter_map(|(i, n)| {
                let mesh = self.scene.meshes.get(n.mesh?)?;
                let m = pose.world.get(i)?;
                let mut b = Aabb::EMPTY;
                for p in mesh.primitives.iter().flat_map(|p| &p.positions) {
                    b.add(m.transform_point3(*p));
                }
                (!b.is_empty()).then_some((i, b))
            })
            .collect();
        let wide = |b: &Aabb| b.size().x.max(b.size().z);
        let flat = |b: &Aabb| b.size().y <= 0.1 * wide(b);
        // 2. Grounds are measured against everything that isn't one.
        let mut content = Aabb::EMPTY;
        let mut candidates = Vec::new();
        for (i, b) in &boxes {
            if flat(b) && wide(b) > 200.0 {
                candidates.push((*i, *b));
            } else {
                content.union(b);
            }
        }
        if content.is_empty() || wide(&content) < 1.0 {
            return;
        }
        let grounds: Vec<usize> =
            candidates.iter().filter(|(_, b)| wide(b) > 6.0 * wide(&content)).map(|&(i, _)| i).collect();
        let margin = content.size() * 0.25 + Vec3::splat(2.0);
        let (lo, hi) = (content.min - margin, content.max + margin);
        // 3. Each ground gets its own clipped copy of its mesh.
        for node in grounds {
            let (Some(mesh), Some(&m)) = (self.scene.nodes[node].mesh, pose.world.get(node)) else { continue };
            let inverse = m.inverse();
            if !inverse.is_finite() {
                continue;
            }
            let mut copy = self.scene.meshes[mesh].clone();
            for p in &mut copy.primitives {
                clip_primitive(p, m, inverse, lo, hi);
            }
            copy.primitives.retain(|p| !p.indices.is_empty());
            let index = self.add_mesh(copy);
            self.scene.nodes[node].mesh = Some(index);
        }
    }

    //endregion
}

/// Clips a triangle primitive to the box `lo`..`hi` on X and Z in world
/// space (`m` takes it there, `inverse` back), carrying every vertex
/// attribute along.
fn clip_primitive(p: &mut Primitive, m: Mat4, inverse: Mat4, lo: Vec3, hi: Vec3) {
    if p.topology != slerp_scene::Topology::Triangles {
        return;
    }
    /// One vertex with everything it carries, interpolated linearly.
    #[derive(Clone, Copy)]
    struct V {
        world: Vec3,
        normal: Vec3,
        uv0: Vec2,
        uv1: Vec2,
        color: Vec4,
    }
    fn mix(a: V, b: V, t: f32) -> V {
        V {
            world: a.world.lerp(b.world, t),
            normal: a.normal.lerp(b.normal, t),
            uv0: a.uv0.lerp(b.uv0, t),
            uv1: a.uv1.lerp(b.uv1, t),
            color: a.color.lerp(b.color, t),
        }
    }
    let get = |i: usize| V {
        world: m.transform_point3(p.positions[i]),
        normal: p.normals.get(i).copied().unwrap_or(Vec3::Y),
        uv0: p.uv0.get(i).copied().unwrap_or(Vec2::ZERO),
        uv1: p.uv1.get(i).copied().unwrap_or(Vec2::ZERO),
        color: p.colors.get(i).copied().unwrap_or(Vec4::ONE),
    };
    let mut out = Primitive { material: p.material, ..Default::default() };
    let n = p.positions.len();
    for t in p.indices.chunks_exact(3) {
        if t.iter().any(|&i| i as usize >= n) {
            continue;
        }
        // Sutherland-Hodgman against the four sides.
        let mut poly: Vec<V> = t.iter().map(|&i| get(i as usize)).collect();
        for (axis, limit, keep_below) in [(0, lo.x, false), (0, hi.x, true), (2, lo.z, false), (2, hi.z, true)] {
            let inside = |v: &V| if keep_below { v.world[axis] <= limit } else { v.world[axis] >= limit };
            let mut next = Vec::with_capacity(poly.len() + 2);
            for k in 0..poly.len() {
                let (a, b) = (poly[k], poly[(k + 1) % poly.len()]);
                if inside(&a) {
                    next.push(a);
                }
                if inside(&a) != inside(&b) {
                    let t = (limit - a.world[axis]) / (b.world[axis] - a.world[axis]);
                    next.push(mix(a, b, t.clamp(0.0, 1.0)));
                }
            }
            poly = next;
            if poly.len() < 3 {
                break;
            }
        }
        if poly.len() < 3 {
            continue;
        }
        let base = out.positions.len() as u32;
        for v in &poly {
            out.positions.push(inverse.transform_point3(v.world));
            out.normals.push(v.normal.normalize_or(Vec3::Y));
            out.uv0.push(v.uv0);
            out.uv1.push(v.uv1);
            out.colors.push(v.color);
        }
        for k in 1..poly.len() as u32 - 1 {
            out.indices.extend_from_slice(&[base, base + k, base + k + 1]);
        }
    }
    // Attributes the original lacked stay absent.
    if p.normals.is_empty() {
        out.normals.clear();
    }
    if p.uv0.is_empty() {
        out.uv0.clear();
    }
    if p.uv1.is_empty() {
        out.uv1.clear();
    }
    if p.colors.is_empty() {
        out.colors.clear();
    }
    *p = out;
}

/// Triangles in a mesh.
fn mesh_triangles(mesh: &Mesh) -> u64 {
    mesh.primitives.iter().map(|p| p.triangle_count() as u64).sum()
}

/// Moves every texture reference of a material by `base`, for materials
/// whose textures were appended to a larger list.
pub(crate) fn shift_textures(m: &mut Material, base: usize) {
    let shift = |t: &mut Option<TexRef>| {
        if let Some(t) = t {
            t.texture += base;
        }
    };
    shift(&mut m.base_color_texture);
    shift(&mut m.metallic_roughness_texture);
    shift(&mut m.normal_texture);
    shift(&mut m.occlusion_texture);
    shift(&mut m.emissive_texture);
    shift(&mut m.transmission_texture);
    shift(&mut m.thickness_texture);
    shift(&mut m.lightmap_texture);
    shift(&mut m.height_texture);
}

/// A transform from a matrix, or identity when the matrix isn't usable
/// (infinite, or so degenerate it can't be split).
pub(crate) fn transform_from(m: Mat4) -> Transform {
    if !m.is_finite() {
        return Transform::IDENTITY;
    }
    let t = Transform::from_matrix(m);
    if t.translation.is_finite() && t.rotation.is_finite() && t.scale.is_finite() {
        Transform { rotation: t.rotation.normalize(), ..t }
    } else {
        Transform::IDENTITY
    }
}

/// A rotation from four numbers, or identity when they don't make one.
pub(crate) fn quat(x: f32, y: f32, z: f32, w: f32) -> Quat {
    let q = Quat::from_xyzw(x, y, z, w);
    if q.is_finite() && q.length_squared() > 1e-12 { q.normalize() } else { Quat::IDENTITY }
}
