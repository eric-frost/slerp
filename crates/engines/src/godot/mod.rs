//! Godot scenes and mesh resources.
//!
//! A scene file is a list of nodes, each naming its parent by path, its
//! class and its properties; properties refer to resources embedded in the
//! file (`SubResource`) or in other files (`ExtResource`).  A node may
//! instance another scene, which brings that scene's nodes along, and later
//! entries may change nodes the instanced scene brought (editable children,
//! inherited scenes).  Loading happens in two passes:
//!
//! 1. `instantiate` builds a tree of `GNode`s: every node of the file and of
//!    the scenes it instances, with overrides applied in file order.  Model
//!    files placed as scenes (`.glb`, `.gltf`, `.fbx`, `.obj`) stay leaves
//!    that remember the overrides aimed inside them.
//! 2. `emit` walks the tree and adds output nodes: meshes, CSG shapes,
//!    GridMap cells, MultiMesh instances, placed models, and markers for
//!    lights and cameras.
//!
//! Materials are collected along the way and read in one batch at the end
//! through `slerp_materials::godot_materials`, so textures they share
//! decode once.

pub(crate) mod binary;
mod mesh;
pub(crate) mod text;
mod variant;

use crate::EngineFormat;
use crate::builder::{Builder, transform_from};
use crate::shapes;
use mesh::Surface;
use slerp_scene::glam::{EulerRot, Mat3, Mat4, Vec2, Vec3, Vec4};
use slerp_scene::{Limits, LoadError, Material, Mesh, Node, Primitive, Scene, Transform};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use variant::{ResFile, Value, prop};

/// How deep scenes may instance scenes.
const MAX_INSTANCE_DEPTH: usize = 24;
/// The most GNodes one load may build, counting instanced scenes.
const MAX_GNODES: usize = 1_000_000;
/// The most vertices one mesh surface may have.
const MAX_SURFACE_VERTICES: usize = 50_000_000;
/// The deepest node tree drawn, and the longest node path followed.
const MAX_TREE_DEPTH: usize = 1000;

pub(crate) fn load(format: EngineFormat, path: &Path, bytes: &[u8], limits: &Limits) -> Result<Scene, LoadError> {
    let file = match format {
        EngineFormat::GodotBinary => binary::parse(bytes, limits.max_file_bytes.saturating_mul(16))?,
        _ => text::parse(bytes)?,
    };
    let godot3 = file.godot3;
    let mut ctx = Ctx::new(path, limits);
    let main = ctx.add_file(path.to_path_buf(), file);
    let kind = ctx.files[main].file.kind.clone();
    // 1. Build what the file is.
    match kind.as_str() {
        "PackedScene" => {
            let root = ctx.instantiate(main, 0).ok_or_else(|| LoadError::invalid("scene without nodes"))?;
            ctx.emit(root, None, true);
        }
        "MeshLibrary" => ctx.mesh_library_gallery(main),
        _ => {
            let used = ctx.mesh_of_resource(Res::File(main));
            match used {
                Some(MeshUse::Res(r)) => {
                    let name = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
                    ctx.b.add_mesh_node(None, &name, Transform::IDENTITY, r.mesh);
                }
                _ => return Err(LoadError::Unsupported(format!("Godot {kind} resource without a mesh"))),
            }
        }
    }
    // 2. A scene of physics shapes alone (physics tests, level blockouts)
    //    shows its shapes, as Godot's editor draws them.
    if !ctx.b.scene.nodes.iter().any(|n| n.mesh.is_some()) {
        ctx.emit_collision_shapes();
    }
    // 3. In the viewer, a level is surrounded by its WorldEnvironment's sky.
    if limits.max_texture_size.is_none()
        && let Some(env) = ctx.environment
    {
        ctx.add_sky(env);
    }
    // 4. Materials, read together.
    ctx.realize_materials();
    let version = if godot3 { "Godot 3" } else { "Godot 4" };
    let what = if kind == "PackedScene" { "scene".to_string() } else { kind.clone() };
    let format = match format {
        EngineFormat::GodotBinary => format!("{version} {what} (binary)"),
        _ => format!("{version} {what}"),
    };
    let stats = [
        ("Scene nodes", ctx.gnodes.len().to_string()),
        ("Scene files", ctx.files.len().to_string()),
        ("Models", ctx.model_count.to_string()),
    ];
    let markers = std::mem::take(&mut ctx.marker_nodes);
    let mut b = ctx.b;
    for (k, v) in stats {
        b.scene.info.details.push((k.to_string(), v));
    }
    if let Some(root) = &ctx.project {
        b.scene.info.details.push(("Project".into(), root.display().to_string()));
    }
    Ok(b.finish(format, &markers))
}

/*┌─────────────────╮
| Context         |
└─────────────────╯*/
//region Context

struct Loaded {
    path: PathBuf,
    file: ResFile,
}

/// A resource, wherever it lives.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Res {
    /// Embedded resource `index` of file `file`.
    Sub(usize, usize),
    /// The main resource of a file.
    File(usize),
}

/// A mesh resource turned into an output mesh.
#[derive(Clone)]
struct MeshRes {
    mesh: usize,
}

enum MeshUse {
    Res(MeshRes),
    /// A model file used as a mesh (Godot imports OBJ files as meshes), with
    /// its import transform.
    Model(usize, Transform),
}

/// A CSG shape's class and the property entries that made it.
type CsgKey = (String, Vec<(usize, usize)>);

/// A node of the scene tree being built.
struct GNode {
    name: String,
    class: String,
    /// The `[node]` entries whose properties apply to this node, as (file,
    /// entry) in the order given, later ones overriding earlier ones.  Only
    /// references: a scene instanced a thousand times shares one copy of
    /// its values.
    props: Vec<(usize, usize)>,
    children: Vec<usize>,
    /// A model file this node places, with the scale its import settings
    /// give.
    model: Option<(PathBuf, f32)>,
    /// Overrides aimed at nodes inside the placed model: their path from
    /// it and the (file, entry) holding the properties.
    model_overrides: Vec<(String, usize, usize)>,
}

impl GNode {
    fn new(name: &str, class: &str) -> Self {
        GNode {
            name: name.to_string(),
            class: class.to_string(),
            props: Vec::new(),
            children: Vec::new(),
            model: None,
            model_overrides: Vec::new(),
        }
    }
}

/// The properties of a node, looked up through the files holding them.
struct Props<'a> {
    files: &'a [Rc<Loaded>],
    lists: &'a [(usize, usize)],
}

impl<'a> Props<'a> {
    /// The last value given for `key`, and the file whose resources it
    /// refers to.
    fn get(&self, key: &str) -> Option<(&'a Value, usize)> {
        let files = self.files;
        self.lists.iter().rev().find_map(|&(f, e)| {
            let entry = files.get(f)?.file.nodes.get(e)?;
            prop(&entry.props, key).map(|v| (v, f))
        })
    }

    /// Every property in order, with its file.
    fn all(&self) -> impl Iterator<Item = (&'a str, &'a Value, usize)> + 'a {
        let files = self.files;
        self.lists.iter().flat_map(move |&(f, e)| {
            files
                .get(f)
                .and_then(|l| l.file.nodes.get(e))
                .into_iter()
                .flat_map(move |entry| entry.props.iter().map(move |(k, v)| (k.as_str(), v, f)))
        })
    }

    fn num(&self, key: &str, default: f32) -> f32 {
        self.get(key).and_then(|(v, _)| v.num()).map_or(default, |v| v as f32)
    }

    fn int(&self, key: &str, default: i64) -> i64 {
        self.get(key).and_then(|(v, _)| v.int()).unwrap_or(default)
    }

    fn flag(&self, key: &str, default: bool) -> bool {
        self.get(key).map_or(default, |(v, _)| v.truthy())
    }

    /// A reference value (a resource), copied out so the caller may use
    /// the context mutably.
    fn reference(&self, key: &str) -> Option<(Value, usize)> {
        self.get(key).map(|(v, f)| (v.clone(), f)).filter(|(v, _)| matches!(v, Value::Ext(_) | Value::Sub(_)))
    }
}

struct Ctx<'a> {
    b: Builder<'a>,
    limits: &'a Limits,
    /// The folder holding `project.godot`.
    project: Option<PathBuf>,
    files: Vec<Rc<Loaded>>,
    file_index: HashMap<PathBuf, Option<usize>>,
    /// Scene files being instanced, for catching cycles.
    stack: Vec<PathBuf>,
    gnodes: Vec<GNode>,
    meshes: HashMap<Res, Option<MeshRes>>,
    materials: HashMap<Res, Option<usize>>,
    /// Material resources to read, as `.tres` text with the path it is
    /// resolved from, and the output material each fills.
    pending: Vec<(PathBuf, String, usize)>,
    marker_nodes: Vec<usize>,
    model_count: usize,
    warned: std::collections::HashSet<String>,
    /// For each node being emitted, whether it is a CSG shape, so its
    /// children know they are combined into it.
    csg_parent: Vec<bool>,
    /// Collision shape nodes and their output nodes, drawn only when the
    /// scene has nothing else to show.
    collisions: Vec<(usize, usize)>,
    /// The Environment resource of the scene's WorldEnvironment.
    environment: Option<Res>,
    /// Generated CSG meshes by class and properties.
    csg_meshes: HashMap<CsgKey, Option<usize>>,
}

impl<'a> Ctx<'a> {
    fn new(path: &Path, limits: &'a Limits) -> Self {
        let project = path.parent().and_then(find_project);
        Ctx {
            b: Builder::new(limits),
            limits,
            project,
            files: Vec::new(),
            file_index: HashMap::new(),
            stack: vec![path.to_path_buf()],
            gnodes: Vec::new(),
            meshes: HashMap::new(),
            materials: HashMap::new(),
            pending: Vec::new(),
            marker_nodes: Vec::new(),
            model_count: 0,
            warned: Default::default(),
            csg_parent: Vec::new(),
            collisions: Vec::new(),
            environment: None,
            csg_meshes: HashMap::new(),
        }
    }

    fn warn(&mut self, message: String) {
        if self.warned.insert(message.clone()) {
            self.b.warn(message);
        }
    }

    /// The properties of GNode `g`.
    fn props(&self, g: usize) -> Props<'_> {
        Props { files: &self.files, lists: &self.gnodes[g].props }
    }

    fn add_file(&mut self, path: PathBuf, file: ResFile) -> usize {
        self.files.push(Rc::new(Loaded { path: path.clone(), file }));
        let index = self.files.len() - 1;
        self.file_index.insert(path, Some(index));
        index
    }

    /// Where a path written in `file` points on disk: `res://` paths from
    /// the project folder (or, outside a project, beside the file), others
    /// relative to the file.
    fn resolve_path(&self, file: usize, written: &str) -> PathBuf {
        let here = self.files[file].path.parent().unwrap_or(Path::new("")).to_path_buf();
        if let Some(rest) = written.strip_prefix("res://") {
            return match &self.project {
                Some(root) => root.join(rest),
                None => here.join(Path::new(rest).file_name().unwrap_or_default()),
            };
        }
        here.join(written)
    }

    /// Reads and parses the resource file at `path`, once.
    fn load_file(&mut self, path: &Path) -> Option<usize> {
        if let Some(&i) = self.file_index.get(path) {
            return i;
        }
        slerp_scene::record_neighbour(path);
        let result = (|| {
            if !self.limits.neighbours {
                return Err(LoadError::Unsupported("neighbouring files are off".into()));
            }
            let size = std::fs::metadata(path).map_err(|_| LoadError::Unsupported("missing".into()))?.len();
            if size > self.limits.max_file_bytes {
                return Err(LoadError::TooLarge(format!("{size} bytes")));
            }
            let bytes = std::fs::read(path)?;
            if bytes.starts_with(b"RSRC") || bytes.starts_with(b"RSCC") {
                binary::parse(&bytes, self.limits.max_file_bytes.saturating_mul(16))
            } else {
                text::parse(&bytes)
            }
        })();
        match result {
            Ok(file) => Some(self.add_file(path.to_path_buf(), file)),
            Err(e) => {
                let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                self.warn(format!("{name}: {e}"));
                self.file_index.insert(path.to_path_buf(), None);
                None
            }
        }
    }

    /// The resource a property value refers to.
    fn resource(&mut self, file: usize, v: &Value) -> Option<Res> {
        match v {
            Value::Sub(id) => {
                let index = self.files[file].file.subs.iter().position(|s| &s.id == id)?;
                Some(Res::Sub(file, index))
            }
            Value::Ext(id) => {
                let ext = self.files[file].file.ext(id)?.path.clone();
                let path = self.resolve_path(file, &ext);
                if is_model_path(&path) {
                    return None;
                }
                self.load_file(&path).map(Res::File)
            }
            _ => None,
        }
    }

    /// A resource's class and properties.
    fn res_data(&self, r: Res) -> (Rc<Loaded>, String, usize) {
        match r {
            Res::Sub(f, i) => {
                let file = self.files[f].clone();
                let kind = file.file.subs[i].kind.clone();
                (file, kind, f)
            }
            Res::File(f) => {
                let file = self.files[f].clone();
                let kind = file.file.kind.clone();
                (file, kind, f)
            }
        }
    }
}

fn res_props(loaded: &Loaded, r: Res) -> &[(String, Value)] {
    match r {
        Res::Sub(_, i) => &loaded.file.subs[i].props,
        Res::File(_) => &loaded.file.main,
    }
}

fn find_project(start: &Path) -> Option<PathBuf> {
    let mut dir = start.to_path_buf();
    for _ in 0..64 {
        if dir.join("project.godot").is_file() {
            return Some(dir);
        }
        if !dir.pop() {
            break;
        }
    }
    None
}

fn is_model_path(path: &Path) -> bool {
    let ext = path.extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase()).unwrap_or_default();
    matches!(ext.as_str(), "glb" | "gltf" | "fbx" | "obj" | "dae" | "blend" | "vrm" | "3ds" | "ply" | "stl")
}

fn is_scene_path(path: &Path) -> bool {
    let ext = path.extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase()).unwrap_or_default();
    matches!(ext.as_str(), "tscn" | "scn" | "escn")
}

/// The scale a model's `.import` file gives its root (`nodes/root_scale`),
/// or 1.
fn import_scale(model: &Path) -> f32 {
    let mut name = model.as_os_str().to_os_string();
    name.push(".import");
    let Ok(text) = std::fs::read_to_string(PathBuf::from(name)) else { return 1.0 };
    for line in text.lines().take(10_000) {
        if let Some(v) = line.trim().strip_prefix("nodes/root_scale=") {
            return v.trim().parse::<f32>().ok().filter(|s| s.is_finite() && *s > 0.0).unwrap_or(1.0);
        }
    }
    1.0
}

//endregion

/*┌─────────────────╮
| Scene tree      |
└─────────────────╯*/
//region Scene tree

impl Ctx<'_> {
    fn new_gnode(&mut self, node: GNode) -> Option<usize> {
        if self.gnodes.len() >= MAX_GNODES {
            self.warn("the scene has too many nodes: only part of it is shown".into());
            return None;
        }
        self.gnodes.push(node);
        Some(self.gnodes.len() - 1)
    }

    /// Builds the tree of scene file `file`, returning its root.
    fn instantiate(&mut self, file: usize, depth: usize) -> Option<usize> {
        let loaded = self.files[file].clone();
        let mut root: Option<usize> = None;
        for (index, entry) in loaded.file.nodes.iter().enumerate() {
            let props = (file, index);
            // 1. The root: a new node, or an inherited scene's root.
            let Some(parent_path) = entry.parent.as_deref() else {
                if root.is_some() {
                    continue;
                }
                let g = match &entry.instance {
                    Some(inst) => self.instance(file, inst, &entry.name, depth),
                    None => None,
                };
                let g = match g {
                    Some(g) => g,
                    None => self.new_gnode(GNode::new(&entry.name, entry.class.as_deref().unwrap_or("Node3D")))?,
                };
                self.gnodes[g].name = entry.name.clone();
                self.gnodes[g].props.push(props);
                root = Some(g);
                continue;
            };
            let Some(root) = root else { continue };
            // 2. Find the parent, which may lie inside an instanced scene or
            //    a placed model.
            let (parent, inside_model) = self.find(root, parent_path);
            if let Some(rest) = inside_model {
                // Aimed inside a model: keep overrides for when it is
                // placed; new nodes hang from the model's root.
                if entry.class.is_none() && entry.instance.is_none() {
                    let path = if rest.is_empty() { entry.name.clone() } else { format!("{rest}/{}", entry.name) };
                    self.gnodes[parent].model_overrides.push((path, file, index));
                    continue;
                }
            }
            // 3. A change to a node an instanced scene brought.
            if entry.class.is_none() && entry.instance.is_none() {
                let existing =
                    self.gnodes[parent].children.iter().copied().find(|&c| self.gnodes[c].name == entry.name);
                if let Some(c) = existing {
                    self.gnodes[c].props.push(props);
                    continue;
                }
            }
            // 4. A new node, or an instance of another scene or model.
            let child = match &entry.instance {
                Some(inst) => self.instance(file, inst, &entry.name, depth),
                None => None,
            };
            let child = match child {
                Some(c) => c,
                None => match self.new_gnode(GNode::new(&entry.name, entry.class.as_deref().unwrap_or("Node3D"))) {
                    Some(c) => c,
                    None => break,
                },
            };
            self.gnodes[child].name = entry.name.clone();
            if let Some(class) = &entry.class {
                // An instance with a type keeps the instanced class unless
                // the script changes it; the type given wins.
                self.gnodes[child].class = class.clone();
            }
            self.gnodes[child].props.push(props);
            self.gnodes[parent].children.push(child);
        }
        root
    }

    /// Follows `path` (names separated by `/`, "." for the root) from
    /// `root`.  Returns the node found, or the deepest node reached; when
    /// that is a placed model, also the rest of the path inside it.
    fn find(&mut self, root: usize, path: &str) -> (usize, Option<String>) {
        let mut at = root;
        let parts: Vec<&str> = path.split('/').filter(|p| !p.is_empty() && *p != ".").take(MAX_TREE_DEPTH).collect();
        for (i, part) in parts.iter().enumerate() {
            if self.gnodes[at].model.is_some() {
                return (at, Some(parts[i..].join("/")));
            }
            match self.gnodes[at].children.iter().copied().find(|&c| self.gnodes[c].name == *part) {
                Some(c) => at = c,
                None => {
                    let message = format!("node path {path} not found; attached higher up");
                    self.warn(message);
                    return (at, None);
                }
            }
        }
        if self.gnodes[at].model.is_some() {
            return (at, Some(String::new()));
        }
        (at, None)
    }

    /// The tree an `instance=` value brings: a scene's nodes or a model.
    fn instance(&mut self, file: usize, inst: &Value, name: &str, depth: usize) -> Option<usize> {
        let Value::Ext(id) = inst else { return None };
        let written = self.files[file].file.ext(id)?.path.clone();
        let path = self.resolve_path(file, &written);
        if is_model_path(&path) {
            let scale = import_scale(&path);
            let mut g = GNode::new(name, "ImportedScene");
            g.model = Some((path, scale));
            return self.new_gnode(g);
        }
        if !is_scene_path(&path) {
            self.warn(format!("{written}: not a scene"));
            return None;
        }
        if depth >= MAX_INSTANCE_DEPTH || self.stack.contains(&path) {
            self.warn(format!("{written}: instanced inside itself"));
            return None;
        }
        let sub = self.load_file(&path)?;
        if self.files[sub].file.kind != "PackedScene" {
            return None;
        }
        self.stack.push(path);
        let root = self.instantiate(sub, depth + 1);
        self.stack.pop();
        root
    }
}

//endregion

/*┌─────────────────╮
| Emitting        |
└─────────────────╯*/
//region Emitting

/// Classes whose subtrees are 2D or user interface, not part of the 3D
/// scene.
fn is_2d(class: &str) -> bool {
    class.ends_with("2D")
        || matches!(
            class,
            "Control"
                | "CanvasLayer"
                | "Container"
                | "Label"
                | "Button"
                | "Panel"
                | "TextureRect"
                | "ColorRect"
                | "MarginContainer"
                | "VBoxContainer"
                | "HBoxContainer"
                | "GridContainer"
                | "CenterContainer"
                | "PanelContainer"
                | "RichTextLabel"
                | "SubViewportContainer"
                | "SubViewport"
                | "Viewport"
                | "ViewportContainer"
                | "TileMap"
                | "TileMapLayer"
                | "CanvasModulate"
                | "ParallaxBackground"
                | "ParallaxLayer"
                | "AnimationPlayer"
                | "AnimationTree"
                | "AudioStreamPlayer"
                | "Timer"
                | "HTTPRequest"
        )
}

/// The local transform a node's properties give.
fn node_transform(p: &Props) -> Mat4 {
    if let Some(m) = p.get("transform").and_then(|(v, _)| v.transform()) {
        return m;
    }
    let position = p.get("position").and_then(|(v, _)| v.vec3()).unwrap_or(Vec3::ZERO);
    let scale = p.get("scale").and_then(|(v, _)| v.vec3()).unwrap_or(Vec3::ONE);
    let basis = if let Some(q) = p.get("quaternion").and_then(|(v, _)| v.math(&["Quaternion"])).filter(|q| q.len() >= 4)
    {
        Mat3::from_quat(crate::builder::quat(q[0] as f32, q[1] as f32, q[2] as f32, q[3] as f32))
    } else if let Some(b) = p.get("basis").and_then(|(v, _)| v.basis()) {
        b
    } else if let Some(r) = p.get("rotation").and_then(|(v, _)| v.vec3()) {
        // Godot's default rotation order applies Y, then X, then Z.
        Mat3::from_euler(EulerRot::YXZ, r.y, r.x, r.z)
    } else {
        Mat3::IDENTITY
    };
    Mat4::from_translation(position) * Mat4::from_mat3(basis * Mat3::from_diagonal(scale))
}

/// GridMap's 24 cell orientations, as rows.
const ORTHO_BASES: [[f32; 9]; 24] = [
    [1., 0., 0., 0., 1., 0., 0., 0., 1.],
    [0., -1., 0., 1., 0., 0., 0., 0., 1.],
    [-1., 0., 0., 0., -1., 0., 0., 0., 1.],
    [0., 1., 0., -1., 0., 0., 0., 0., 1.],
    [1., 0., 0., 0., 0., -1., 0., 1., 0.],
    [0., 0., 1., 1., 0., 0., 0., 1., 0.],
    [-1., 0., 0., 0., 0., 1., 0., 1., 0.],
    [0., 0., -1., -1., 0., 0., 0., 1., 0.],
    [1., 0., 0., 0., -1., 0., 0., 0., -1.],
    [0., 1., 0., 1., 0., 0., 0., 0., -1.],
    [-1., 0., 0., 0., 1., 0., 0., 0., -1.],
    [0., -1., 0., -1., 0., 0., 0., 0., -1.],
    [1., 0., 0., 0., 0., 1., 0., -1., 0.],
    [0., 0., -1., 1., 0., 0., 0., -1., 0.],
    [-1., 0., 0., 0., 0., -1., 0., -1., 0.],
    [0., 0., 1., -1., 0., 0., 0., -1., 0.],
    [0., 0., 1., 0., 1., 0., -1., 0., 0.],
    [0., -1., 0., 0., 0., 1., -1., 0., 0.],
    [0., 0., -1., 0., -1., 0., -1., 0., 0.],
    [0., 1., 0., 0., 0., -1., -1., 0., 0.],
    [0., 0., 1., 0., -1., 0., 1., 0., 0.],
    [0., 1., 0., 0., 0., 1., 1., 0., 0.],
    [0., 0., -1., 0., 1., 0., 1., 0., 0.],
    [0., -1., 0., 0., 0., -1., 1., 0., 0.],
];

fn rows3(r: &[f32; 9]) -> Mat3 {
    Mat3::from_cols(Vec3::new(r[0], r[3], r[6]), Vec3::new(r[1], r[4], r[7]), Vec3::new(r[2], r[5], r[8]))
}

impl Ctx<'_> {
    /// Adds the output nodes for GNode `g` and its subtree.
    fn emit(&mut self, g: usize, parent: Option<usize>, parent_visible: bool) {
        let class = self.gnodes[g].class.clone();
        // Deeper than this is a hostile file, and recursing on would
        // overflow the stack.
        if is_2d(&class) || self.csg_parent.len() > MAX_TREE_DEPTH {
            return;
        }
        let visible = parent_visible && self.props(g).get("visible").is_none_or(|(v, _)| v.truthy());
        let mut node = Node::new(self.gnodes[g].name.clone());
        node.transform = transform_from(node_transform(&self.props(g)));
        node.visible = visible;
        let Some(out) = self.b.add_node(parent, node) else { return };
        let parent_is_csg = parent.is_some() && self.csg_parent.last().copied().unwrap_or(false);
        let is_csg = class.starts_with("CSG");
        // 1. What the node draws.
        match class.as_str() {
            "MeshInstance3D" | "MeshInstance" | "SoftBody3D" | "SoftBody" => self.emit_mesh_instance(g, out, visible),
            "GridMap" => self.emit_grid_map(g, out, visible),
            "MultiMeshInstance3D" | "MultiMeshInstance" => self.emit_multimesh(g, out, visible),
            _ if self.gnodes[g].model.is_some() => self.emit_model(g, out, visible),
            c if is_csg => self.emit_csg(g, c, out, visible, parent_is_csg),
            c if c.contains("Light")
                && (c.starts_with("Omni") || c.starts_with("Spot") || c.starts_with("Directional")) =>
            {
                let color = self.props(g).get("light_color").and_then(|(v, _)| v.color()).unwrap_or(Vec4::ONE);
                let label = format!("{} ({c})", self.gnodes[g].name);
                self.b.add_marker(label, color);
                self.marker_nodes.push(out);
            }
            "Camera3D" | "Camera" | "ClippedCamera" | "InterpolatedCamera" => {
                let current = self.props(g).get("current").is_some_and(|(v, _)| v.truthy());
                self.b.cameras.push(crate::builder::CameraInfo { node: out, current });
                let label = format!("{} (camera)", self.gnodes[g].name);
                self.b.add_marker(label, Vec4::new(0.6, 0.8, 1.0, 1.0));
                self.marker_nodes.push(out);
            }
            "WorldEnvironment" => {
                if let Some((v, f)) = self.props(g).get("environment").map(|(v, f)| (v.clone(), f)) {
                    self.environment = self.resource(f, &v);
                }
            }
            "CollisionShape3D" | "CollisionShape" | "CollisionPolygon3D" | "CollisionPolygon" => {
                self.collisions.push((g, out));
                return;
            }
            _ => {}
        }
        // 2. Children, which CSG shapes combine into their own result.
        self.csg_parent.push(is_csg);
        let children = self.gnodes[g].children.clone();
        for c in children {
            self.emit(c, Some(out), visible);
        }
        self.csg_parent.pop();
    }

    fn emit_mesh_instance(&mut self, g: usize, out: usize, visible: bool) {
        let Some((mesh_value, file)) = self.props(g).get("mesh").map(|(v, f)| (v.clone(), f)) else { return };
        match self.mesh_of_value(file, &mesh_value) {
            Some(MeshUse::Res(r)) => {
                let lists = self.gnodes[g].props.clone();
                let overrides = self.material_overrides(&lists);
                let mesh = self.b.with_materials(r.mesh, &overrides);
                self.b.scene.nodes[out].mesh = Some(mesh);
            }
            Some(MeshUse::Model(model, transform)) => {
                let name = self.gnodes[g].name.clone();
                if let Some(placed) = self.b.place_model(model, Some(out), &name, transform) {
                    self.hide_placed(&placed.nodes, visible);
                }
            }
            None => {}
        }
    }

    /// Hides every node of a placement when its parent is hidden.
    fn hide_placed(&mut self, nodes: &[usize], visible: bool) {
        if !visible {
            for &n in nodes {
                self.b.scene.nodes[n].visible = false;
            }
        }
    }

    /// The materials a GeometryInstance3D puts over its mesh's own: one
    /// for all surfaces (`material_override`) or one per surface
    /// (`surface_material_override/N`, Godot 3's `material/N`).
    fn material_overrides(&mut self, lists: &[(usize, usize)]) -> Vec<Option<usize>> {
        // 1. The references, copied out of the files.
        let (per_surface, all) = {
            let p = Props { files: &self.files, lists };
            let mut per_surface: Vec<(usize, Value, usize)> = Vec::new();
            for (k, v, f) in p.all() {
                let n = k.strip_prefix("surface_material_override/").or_else(|| k.strip_prefix("material/"));
                if let Some(i) = n.and_then(|n| n.parse::<usize>().ok()).filter(|&i| i < 256) {
                    per_surface.push((i, v.clone(), f));
                }
            }
            (per_surface, p.reference("material_override"))
        };
        // 2. The materials.
        if let Some((v, f)) = all
            && let Some(m) = self.material(f, &v)
        {
            return vec![Some(m); 256];
        }
        let mut out: Vec<Option<usize>> = Vec::new();
        for (i, v, f) in per_surface {
            if out.len() <= i {
                out.resize(i + 1, None);
            }
            out[i] = self.material(f, &v);
        }
        out
    }

    fn emit_model(&mut self, g: usize, out: usize, visible: bool) {
        let Some((path, scale)) = self.gnodes[g].model.clone() else { return };
        let Some(model) = self.b.model(&path) else { return };
        self.model_count += 1;
        let transform = Transform { scale: Vec3::splat(scale), ..Transform::IDENTITY };
        let name = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        let Some(placed) = self.b.place_model(model, Some(out), &name, transform) else { return };
        self.hide_placed(&placed.nodes, visible);
        // Overrides aimed inside the model: matched by the last name of
        // their path, since Godot's importer renames and regroups nodes
        // (skeletons) that the model file keeps as they are.
        let overrides = std::mem::take(&mut self.gnodes[g].model_overrides);
        if overrides.is_empty() {
            return;
        }
        let names: Vec<String> =
            self.b.model_info(model).map(|m| m.node_names().map(str::to_string).collect()).unwrap_or_default();
        for (path, file, entry) in overrides {
            let last = path.rsplit('/').next().unwrap_or(&path);
            let Some(i) = names.iter().position(|n| n == last || sanitize(n) == last) else { continue };
            let Some(&target) = placed.nodes.get(i) else { continue };
            let lists = [(file, entry)];
            let (hidden, transform) = {
                let p = Props { files: &self.files, lists: &lists };
                let hidden = p.get("visible").is_some_and(|(v, _)| !v.truthy());
                (hidden, p.get("transform").is_some().then(|| transform_from(node_transform(&p))))
            };
            if hidden {
                self.b.scene.nodes[target].visible = false;
            }
            if let Some(t) = transform {
                self.b.scene.nodes[target].transform = t;
            }
            let overrides = self.material_overrides(&lists);
            if let Some(mesh) = self.b.scene.nodes[target].mesh {
                let m = self.b.with_materials(mesh, &overrides);
                self.b.scene.nodes[target].mesh = Some(m);
            }
        }
    }

    fn emit_grid_map(&mut self, g: usize, out: usize, visible: bool) {
        let Some((library, file)) =
            self.props(g).reference("mesh_library").or_else(|| self.props(g).reference("theme"))
        else {
            return;
        };
        let Some(res) = self.resource(file, &library) else { return };
        let items = self.mesh_library_items(res);
        // 1. Every cell's placement, read in place from the file.
        let placements: Vec<(usize, Mat4, i64)> = {
            let p = self.props(g);
            let cell_size = p.get("cell_size").and_then(|(v, _)| v.vec3()).unwrap_or(Vec3::splat(2.0));
            let center = |k: &str| p.flag(k, true) as u8 as f32;
            let offset =
                cell_size * 0.5 * Vec3::new(center("cell_center_x"), center("cell_center_y"), center("cell_center_z"));
            let cell_scale = p.num("cell_scale", 1.0);
            let cells = p.get("data").and_then(|(v, _)| v.get("cells")).and_then(Value::ints).unwrap_or(&[]);
            // Never more than the scene can still hold.
            let room = self.b.room();
            cells
                .chunks_exact(3)
                .filter_map(|c| {
                    let (key, z, cell) = (c[0] as u32, c[1] as u32, c[2] as u32);
                    let pos =
                        Vec3::new((key & 0xFFFF) as i16 as f32, (key >> 16) as i16 as f32, (z & 0xFFFF) as i16 as f32);
                    let (item, rot) = ((cell & 0xFFFF) as i64, ((cell >> 16) & 31) as usize);
                    let (mesh, mesh_transform) = items.get(&item)?;
                    let basis = rows3(&ORTHO_BASES[rot.min(23)]) * cell_scale;
                    let m = Mat4::from_translation(pos * cell_size + offset) * Mat4::from_mat3(basis) * *mesh_transform;
                    Some((*mesh, m, item))
                })
                .take(room)
                .collect()
        };
        if placements.len() > 100 {
            self.b.level = true;
        }
        // 2. A node for each.
        for (mesh, m, item) in placements {
            let Some(n) = self.b.add_mesh_node(Some(out), &format!("cell {item}"), transform_from(m), mesh) else {
                break;
            };
            self.b.scene.nodes[n].visible = visible;
        }
    }

    /// A MeshLibrary's items: mesh and its transform, by item id.
    fn mesh_library_items(&mut self, res: Res) -> HashMap<i64, (usize, Mat4)> {
        let (loaded, _, file) = self.res_data(res);
        let props = res_props(&loaded, res);
        let mut items: HashMap<i64, (Option<Value>, Mat4)> = HashMap::new();
        for (k, v) in props {
            let Some(rest) = k.strip_prefix("item/") else { continue };
            let Some((id, what)) = rest.split_once('/') else { continue };
            let Ok(id) = id.parse::<i64>() else { continue };
            let entry = items.entry(id).or_insert((None, Mat4::IDENTITY));
            match what {
                "mesh" => entry.0 = Some(v.clone()),
                "mesh_transform" => entry.1 = v.transform().unwrap_or(Mat4::IDENTITY),
                _ => {}
            }
        }
        let mut out = HashMap::new();
        for (id, (mesh, transform)) in items {
            let Some(mesh) = mesh else { continue };
            if let Some(MeshUse::Res(r)) = self.mesh_of_value(file, &mesh) {
                out.insert(id, (r.mesh, transform));
            }
        }
        out
    }

    /// A MeshLibrary file on its own: its items in a row.
    fn mesh_library_gallery(&mut self, file: usize) {
        let items = self.mesh_library_items(Res::File(file));
        let mut ids: Vec<&i64> = items.keys().collect();
        ids.sort();
        let columns = (ids.len() as f32).sqrt().ceil().max(1.0) as usize;
        let spacing = 2.5;
        let ids: Vec<i64> = ids.into_iter().copied().collect();
        for (i, id) in ids.iter().enumerate() {
            let (mesh, transform) = items[id];
            let at = Vec3::new((i % columns) as f32 * spacing, 0.0, (i / columns) as f32 * spacing);
            self.b.add_mesh_node(
                None,
                &format!("item {id}"),
                transform_from(Mat4::from_translation(at) * transform),
                mesh,
            );
        }
    }

    fn emit_multimesh(&mut self, g: usize, out: usize, visible: bool) {
        let Some((mm, file)) = self.props(g).reference("multimesh") else { return };
        let Some(res) = self.resource(file, &mm) else { return };
        let (loaded, _, mm_file) = self.res_data(res);
        let Some(mesh_value) = prop(res_props(&loaded, res), "mesh").cloned() else { return };
        let Some(MeshUse::Res(r)) = self.mesh_of_value(mm_file, &mesh_value) else { return };
        // The instances' transforms, read in place, never more than the
        // scene can still hold.
        let props = res_props(&loaded, res);
        let count = prop(props, "instance_count").and_then(Value::int).unwrap_or(0).clamp(0, 1_000_000) as usize;
        let count = count.min(self.b.room());
        let mut transforms: Vec<Mat4> = Vec::new();
        if let Some(buffer) = prop(props, "buffer").and_then(Value::floats) {
            // Godot 4: per instance a 3x4 transform (rows) and optional
            // colour and custom data.
            let stride = 12
                + if prop(props, "use_colors").is_some_and(Value::truthy) { 4 } else { 0 }
                + if prop(props, "use_custom_data").is_some_and(Value::truthy) { 4 } else { 0 };
            if prop(props, "transform_format").and_then(Value::int).unwrap_or(1) == 1 {
                for c in buffer.chunks_exact(stride).take(count) {
                    let basis = rows3(&[c[0], c[1], c[2], c[4], c[5], c[6], c[8], c[9], c[10]]);
                    transforms.push(Mat4::from_translation(Vec3::new(c[3], c[7], c[11])) * Mat4::from_mat3(basis));
                }
            }
        } else if let Some(array) = prop(props, "transform_array").and_then(Value::floats) {
            // Godot 3: four vectors per instance, the basis columns and
            // the origin.
            for c in array.chunks_exact(12).take(count) {
                transforms.push(Mat4::from_cols(
                    Vec3::new(c[0], c[1], c[2]).extend(0.0),
                    Vec3::new(c[3], c[4], c[5]).extend(0.0),
                    Vec3::new(c[6], c[7], c[8]).extend(0.0),
                    Vec3::new(c[9], c[10], c[11]).extend(1.0),
                ));
            }
        }
        for (i, m) in transforms.into_iter().enumerate() {
            let Some(n) = self.b.add_mesh_node(Some(out), &format!("instance {i}"), transform_from(m), r.mesh) else {
                break;
            };
            self.b.scene.nodes[n].visible = visible;
        }
    }

    /// A CSG shape.  Only unions are drawn exactly; a subtracted or
    /// intersected shape is left out, which shows its parent whole.
    fn emit_csg(&mut self, g: usize, class: &str, out: usize, visible: bool, parent_is_csg: bool) {
        let operation = self.props(g).int("operation", 0);
        if parent_is_csg && operation != 0 {
            let what = if operation == 2 { "subtraction" } else { "intersection" };
            self.warn(format!("CSG {what} isn't evaluated: the shapes it removes are left out"));
            self.b.scene.nodes[out].visible = false;
            return;
        }
        self.b.scene.nodes[out].visible = visible;
        let lists = self.gnodes[g].props.clone();
        // 1. A mesh shape wears its mesh.
        if class.trim_end_matches("3D") == "CSGMesh" {
            if let Some((v, f)) = self.props(g).reference("mesh")
                && let Some(MeshUse::Res(r)) = self.mesh_of_value(f, &v)
            {
                let overrides = self.material_overrides(&lists);
                let mesh = self.b.with_materials(r.mesh, &overrides);
                self.b.scene.nodes[out].mesh = Some(mesh);
            }
            return;
        }
        // 2. Generated shapes, made once for every node with the same
        //    properties (each copy of an instanced scene).
        let key = (class.to_string(), lists.clone());
        if let Some(&mesh) = self.csg_meshes.get(&key) {
            self.b.scene.nodes[out].mesh = mesh;
            return;
        }
        let (prim, material, path_mode) = {
            let p = Props { files: &self.files, lists: &lists };
            let godot3 = !class.ends_with("3D");
            let g3 = |a: f32, b: f32| if godot3 { a } else { b };
            let mut path_mode = false;
            let prim = match class.trim_end_matches("3D") {
                "CSGBox" => {
                    let size = if godot3 {
                        Vec3::new(p.num("width", 2.0), p.num("height", 2.0), p.num("depth", 2.0))
                    } else {
                        p.get("size").and_then(|(v, _)| v.vec3()).unwrap_or(Vec3::ONE)
                    };
                    Some(shapes::cuboid(size))
                }
                "CSGSphere" => {
                    let r = p.num("radius", g3(1.0, 0.5));
                    Some(shapes::sphere(r, 2.0 * r, p.int("radial_segments", 12), p.int("rings", 6), false))
                }
                "CSGCylinder" => {
                    let r = p.num("radius", g3(1.0, 0.5));
                    let top = if p.flag("cone", false) { 0.0 } else { r };
                    let height = p.num("height", g3(1.0, 2.0));
                    Some(shapes::cylinder(
                        top,
                        r,
                        height,
                        p.int("sides", 8),
                        (true, true),
                        p.flag("smooth_faces", true),
                    ))
                }
                "CSGTorus" => Some(shapes::torus(
                    p.num("inner_radius", g3(2.0, 0.5)),
                    p.num("outer_radius", g3(3.0, 1.0)),
                    p.int("sides", 8),
                    p.int("ring_sides", 6),
                )),
                "CSGPolygon" => {
                    let polygon = p.get("polygon").and_then(|(v, _)| v.points2()).unwrap_or_else(|| {
                        vec![Vec2::new(0.0, 0.0), Vec2::new(0.0, 1.0), Vec2::new(1.0, 1.0), Vec2::new(1.0, 0.0)]
                    });
                    match p.int("mode", 0) {
                        0 => shapes::extrude(&polygon, p.num("depth", 1.0)),
                        1 => shapes::spin(&polygon, p.num("spin_degrees", 360.0), p.int("spin_sides", 8)),
                        _ => {
                            path_mode = true;
                            None
                        }
                    }
                }
                _ => None,
            };
            let prim = prim.map(|mut prim| {
                if p.flag("flip_faces", false) {
                    for t in prim.indices.chunks_exact_mut(3) {
                        t.swap(1, 2);
                    }
                    for n in &mut prim.normals {
                        *n = -*n;
                    }
                }
                prim
            });
            (prim, p.reference("material"), path_mode)
        };
        if path_mode {
            self.warn("CSG polygons extruded along a path aren't drawn".into());
        }
        let mesh = prim.map(|mut prim| {
            if let Some((v, f)) = material {
                prim.material = self.material(f, &v);
            }
            let name = self.gnodes[g].name.clone();
            self.b.add_mesh(Mesh { name, primitives: vec![prim], target_names: vec![] })
        });
        self.csg_meshes.insert(key, mesh);
        self.b.scene.nodes[out].mesh = mesh;
    }
}

impl Ctx<'_> {
    /// Draws every collision shape (boxes, spheres, capsules, cylinders,
    /// triangle meshes, height maps, boundary planes) in Godot's debug
    /// colour.
    fn emit_collision_shapes(&mut self) {
        let material = self.b.add_material(Material {
            name: "collision shape".into(),
            base_color: Vec4::new(0.0, 0.45, 0.55, 1.0),
            roughness: 0.8,
            double_sided: true,
            ..Default::default()
        });
        let mut made: HashMap<Res, Option<usize>> = HashMap::new();
        for (g, out) in std::mem::take(&mut self.collisions) {
            if self.props(g).get("disabled").is_some_and(|(v, _)| v.truthy()) {
                continue;
            }
            let Some((v, f)) = self.props(g).get("shape").map(|(v, f)| (v.clone(), f)) else { continue };
            let Some(res) = self.resource(f, &v) else { continue };
            let mesh = match made.get(&res) {
                Some(m) => *m,
                None => {
                    let (loaded, kind, _) = self.res_data(res);
                    let prim = collision_shape(&kind, res_props(&loaded, res), loaded.file.godot3);
                    let m = prim.map(|mut p| {
                        p.material = Some(material);
                        self.b.add_mesh(Mesh { name: kind.clone(), primitives: vec![p], target_names: vec![] })
                    });
                    made.insert(res, m);
                    m
                }
            };
            if let Some(m) = mesh {
                self.b.scene.nodes[out].mesh = Some(m);
            }
        }
    }
}

impl Ctx<'_> {
    /// Surrounds a level with the colours of its environment's sky (a
    /// procedural sky's four colours, or the plain background colour),
    /// drawn as a sky so that it is far away, unlit, and left out of
    /// framing.  Small scenes get none: the viewer's own backdrop suits an
    /// object better.
    fn add_sky(&mut self, env: Res) {
        // 1. Only around levels.
        let bounds = self.b.scene.bounds(&slerp_scene::Pose::rest(&self.b.scene));
        if bounds.is_empty() || (bounds.size().x.max(bounds.size().z) <= crate::builder::LEVEL_EXTENT && !self.b.level)
        {
            return;
        }
        // 2. The colours: top, horizon, ground horizon, ground bottom, as
        //    stored (sRGB).
        let (loaded, _, file) = self.res_data(env);
        let props = res_props(&loaded, env).to_vec();
        let godot3 = loaded.file.godot3;
        let mode = prop(&props, "background_mode").and_then(Value::int).unwrap_or(0);
        let colors = if mode == 2 {
            let sky = prop(&props, if godot3 { "background_sky" } else { "sky" }).cloned();
            let sky = sky.and_then(|v| self.resource(file, &v));
            let material = sky.and_then(|s| {
                let (l, kind, f) = self.res_data(s);
                let p = res_props(&l, s).to_vec();
                if kind.contains("Sky") && kind != "Sky" {
                    // Godot 3's ProceduralSky is the sky itself.
                    return Some((kind, p, l.file.godot3));
                }
                let m = prop(&p, "sky_material").cloned()?;
                let m = self.resource(f, &m)?;
                let (ml, mkind, _) = self.res_data(m);
                Some((mkind, res_props(&ml, m).to_vec(), ml.file.godot3))
            });
            match material {
                Some((kind, p, g3)) if kind.starts_with("ProceduralSky") => {
                    let c = |k: &str, d: [f32; 3]| {
                        prop(&p, k).and_then(Value::color).map_or(Vec3::from(d), |c| c.truncate())
                    };
                    let (top, horizon, ground_horizon, ground) = if g3 {
                        ([0.647, 0.839, 0.945], [0.839, 0.917, 0.98], [0.423, 0.396, 0.372], [0.156, 0.184, 0.211])
                    } else {
                        ([0.385, 0.454, 0.55], [0.6463, 0.6558, 0.6708], [0.6463, 0.6558, 0.6708], [0.2, 0.169, 0.133])
                    };
                    [
                        c("sky_top_color", top),
                        c("sky_horizon_color", horizon),
                        c("ground_horizon_color", ground_horizon),
                        c("ground_bottom_color", ground),
                    ]
                }
                // Physical and panorama skies: a plain daylight sky.
                _ => [Vec3::new(0.35, 0.55, 0.85), Vec3::new(0.75, 0.85, 0.95), Vec3::splat(0.5), Vec3::splat(0.25)],
            }
        } else if mode == 1 {
            let c = prop(&props, "background_color").and_then(Value::color).map_or(Vec3::splat(0.3), |c| c.truncate());
            [c; 4]
        } else {
            return;
        };
        // 3. A picture 64 rows tall: the sky down to the horizon, then the
        //    ground, laid out for `Effect::PanoramaSky`, whose v is the
        //    tangent of the elevation; the transform puts the horizon in
        //    the middle and 45 degrees at the edges.
        let mut px = Vec::with_capacity(64 * 4);
        for row in 0..64 {
            let (a, b, t) = if row < 32 {
                (colors[0], colors[1], row as f32 / 31.0)
            } else {
                (colors[2], colors[3], (row - 32) as f32 / 31.0)
            };
            let c = a.lerp(b, t.powf(if row < 32 { 2.0 } else { 0.5 }));
            px.extend([c.x, c.y, c.z].map(|v| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8));
            px.push(255);
        }
        let texture = self.b.add_texture(std::sync::Arc::new(slerp_scene::Texture::from_rgba8(
            1,
            64,
            px,
            slerp_scene::ColorSpace::Srgb,
        )));
        let sampler = slerp_scene::Sampler { wrap_v: slerp_scene::Wrap::ClampToEdge, ..Default::default() };
        let tex = slerp_scene::TexRef {
            texture,
            uv_set: 0,
            transform: Some(slerp_scene::UvTransform {
                offset: Vec2::new(0.0, 0.5),
                rotation: 0.0,
                scale: Vec2::new(1.0, -0.5),
            }),
            sampler,
        };
        let material = self.b.add_material(Material {
            name: "sky".into(),
            base_color: Vec4::ONE,
            base_color_texture: Some(tex),
            unlit: true,
            double_sided: true,
            effect: slerp_scene::Effect::PanoramaSky,
            ..Default::default()
        });
        // 4. A box well outside the level to draw it on.
        let mut prim = shapes::cuboid(Vec3::splat(bounds.size().max_element() * 4.0 + 100.0));
        for t in prim.indices.chunks_exact_mut(3) {
            t.swap(1, 2);
        }
        for n in &mut prim.normals {
            *n = -*n;
        }
        prim.material = Some(material);
        let mesh = self.b.add_mesh(Mesh { name: "sky".into(), primitives: vec![prim], target_names: vec![] });
        let transform = Transform { translation: bounds.center(), ..Transform::IDENTITY };
        self.b.add_mesh_node(None, "sky", transform, mesh);
    }
}

/// The geometry of a collision shape resource.
fn collision_shape(kind: &str, props: &[(String, Value)], godot3: bool) -> Option<Primitive> {
    let num = |k: &str, d: f32| prop(props, k).and_then(Value::num).map_or(d, |v| v as f32);
    let base = kind.trim_end_matches("3D");
    Some(match base {
        "BoxShape" => {
            let size = if godot3 {
                prop(props, "extents").and_then(Value::vec3).unwrap_or(Vec3::ONE) * 2.0
            } else {
                prop(props, "size").and_then(Value::vec3).unwrap_or(Vec3::ONE)
            };
            shapes::cuboid(size)
        }
        "SphereShape" => {
            let r = num("radius", if godot3 { 1.0 } else { 0.5 });
            shapes::sphere(r, 2.0 * r, 24, 12, false)
        }
        "CapsuleShape" => {
            if godot3 {
                let r = num("radius", 1.0);
                let mut c = shapes::capsule(r, num("height", 1.0) + 2.0 * r, 24, 6);
                for v in c.positions.iter_mut().chain(c.normals.iter_mut()) {
                    *v = Vec3::new(v.x, -v.z, v.y);
                }
                c
            } else {
                shapes::capsule(num("radius", 0.5), num("height", 2.0), 24, 6)
            }
        }
        "CylinderShape" => {
            let r = num("radius", if godot3 { 1.0 } else { 0.5 });
            shapes::cylinder(r, r, num("height", 2.0), 24, (true, true), true)
        }
        "WorldBoundaryShape" | "PlaneShape" => shapes::plane(Vec2::splat(40.0), Vec3::Y, Vec3::ZERO, 1),
        "ConcavePolygonShape" => {
            let data = prop(props, "data").and_then(Value::floats)?;
            let mut p = Primitive::default();
            for t in data.chunks_exact(9).take(10_000_000) {
                // Godot's faces wind clockwise.
                for k in [0, 2, 1] {
                    p.indices.push(p.positions.len() as u32);
                    p.positions.push(Vec3::new(t[k * 3], t[k * 3 + 1], t[k * 3 + 2]));
                }
            }
            if p.positions.iter().any(|v| !v.is_finite()) {
                return None;
            }
            p.make_flat();
            p
        }
        "HeightMapShape" => {
            let (w, d) =
                (prop(props, "map_width").and_then(Value::int)?, prop(props, "map_depth").and_then(Value::int)?);
            let data = prop(props, "map_data").and_then(Value::floats)?;
            let (w, d) = (usize::try_from(w).ok()?, usize::try_from(d).ok()?);
            if w < 2 || d < 2 || w.checked_mul(d)? != data.len() {
                return None;
            }
            let mut p = Primitive::default();
            for z in 0..d {
                for x in 0..w {
                    let h = data[z * w + x];
                    p.positions.push(Vec3::new(
                        x as f32 - (w - 1) as f32 * 0.5,
                        if h.is_finite() { h } else { 0.0 },
                        z as f32 - (d - 1) as f32 * 0.5,
                    ));
                }
            }
            for z in 0..d - 1 {
                for x in 0..w - 1 {
                    let a = (z * w + x) as u32;
                    let (b, c, e) = (a + 1, a + w as u32, a + w as u32 + 1);
                    p.indices.extend_from_slice(&[a, c, b, b, c, e]);
                }
            }
            p
        }
        _ => return None,
    })
}

/// Godot's importer replaces characters node names can't hold.
fn sanitize(name: &str) -> String {
    name.chars().map(|c| if matches!(c, '.' | ':' | '@' | '/' | '"' | '%') { '_' } else { c }).collect()
}

//endregion

/*┌─────────────────╮
| Meshes          |
└─────────────────╯*/
//region Meshes

impl Ctx<'_> {
    /// The mesh a `mesh` property names.
    fn mesh_of_value(&mut self, file: usize, v: &Value) -> Option<MeshUse> {
        if let Value::Ext(id) = v {
            let written = self.files[file].file.ext(id)?.path.clone();
            let path = self.resolve_path(file, &written);
            if is_model_path(&path) {
                // Godot imports an OBJ file as a mesh; its `.import` may
                // scale and move it.
                let model = self.b.model(&path)?;
                self.model_count += 1;
                return Some(MeshUse::Model(model, obj_import_transform(&path)));
            }
        }
        let res = self.resource(file, v)?;
        self.mesh_of_resource(res)
    }

    fn mesh_of_resource(&mut self, res: Res) -> Option<MeshUse> {
        if let Some(m) = self.meshes.get(&res) {
            return m.clone().map(MeshUse::Res);
        }
        let built = self.build_mesh(res);
        self.meshes.insert(res, built.clone());
        built.map(MeshUse::Res)
    }

    fn build_mesh(&mut self, res: Res) -> Option<MeshRes> {
        let (loaded, kind, file) = self.res_data(res);
        let props = res_props(&loaded, res);
        let godot3 = loaded.file.godot3;
        let name = prop(props, "resource_name").and_then(Value::str).unwrap_or("").to_string();
        let surfaces: Vec<Surface> = if kind == "ArrayMesh" || kind == "Mesh" {
            let (surfaces, failed) = mesh::array_mesh(props, MAX_SURFACE_VERTICES);
            if failed > 0 {
                self.warn(format!("{failed} mesh surfaces couldn't be read"));
            }
            surfaces
        } else {
            let prim = mesh::primitive_mesh(&kind, props, godot3)?;
            vec![Surface { prim, material: prop(props, "material").cloned() }]
        };
        let mut primitives: Vec<Primitive> = Vec::with_capacity(surfaces.len());
        for (i, s) in surfaces.into_iter().enumerate() {
            let mut p = s.prim;
            // A surface's material may also be set on the mesh as
            // `surface_N/material`.
            let material = s.material.or_else(|| prop(props, &format!("surface_{i}/material")).cloned());
            let mut vertex_colors = false;
            if let Some(m) = &material {
                vertex_colors = self.material_flag(file, m, "vertex_color_use_as_albedo");
                p.material = self.material(file, m);
            }
            // Godot ignores vertex colours unless the material asks for
            // them; glTF-style rendering always multiplies them in.
            if !vertex_colors {
                p.colors.clear();
            }
            primitives.push(p);
        }
        if primitives.is_empty() {
            return None;
        }
        let mesh = self.b.add_mesh(Mesh { name, primitives, target_names: vec![] });
        Some(MeshRes { mesh })
    }

    //endregion

    /*┌─────────────────╮
    | Materials       |
    └─────────────────╯*/
    //region Materials

    /// Whether the material a value names sets a boolean property.
    fn material_flag(&mut self, file: usize, v: &Value, key: &str) -> bool {
        let Some(res) = self.resource(file, v) else { return false };
        let (loaded, _, _) = self.res_data(res);
        prop(res_props(&loaded, res), key).is_some_and(Value::truthy)
    }

    /// The output material a value names, read later in one batch.
    fn material(&mut self, file: usize, v: &Value) -> Option<usize> {
        let res = self.resource(file, v)?;
        if let Some(&m) = self.materials.get(&res) {
            return m;
        }
        let (loaded, kind, _) = self.res_data(res);
        let is_material = matches!(
            kind.as_str(),
            "StandardMaterial3D" | "ORMMaterial3D" | "SpatialMaterial" | "ShaderMaterial" | "Material"
        );
        let slot = if is_material {
            let props = res_props(&loaded, res);
            let text = tres_text(&loaded.file, &kind, props);
            let name = prop(props, "resource_name").and_then(Value::str).unwrap_or("").to_string();
            let slot = self.b.add_material(Material { name, ..Default::default() });
            self.pending.push((loaded.path.clone(), text, slot));
            Some(slot)
        } else {
            None
        };
        self.materials.insert(res, slot);
        slot
    }

    /// Reads every collected material at once and puts them in their slots.
    fn realize_materials(&mut self) {
        if self.pending.is_empty() {
            return;
        }
        let inputs: Vec<(&Path, &[u8])> = self.pending.iter().map(|(p, t, _)| (p.as_path(), t.as_bytes())).collect();
        let read = slerp_materials::godot_materials(&inputs, self.limits);
        let base = self.b.scene.textures.len();
        self.b.scene.textures.extend(read.textures);
        for w in read.warnings.into_iter().take(20) {
            self.b.warn(w);
        }
        for ((_, _, slot), material) in self.pending.iter().zip(read.materials) {
            let Some(mut m) = material else { continue };
            crate::builder::shift_textures(&mut m, base);
            if m.name.is_empty() {
                m.name = std::mem::take(&mut self.b.scene.materials[*slot].name);
            }
            self.b.scene.materials[*slot] = m;
        }
    }
}

/// A material resource written as a `.tres` file, with the external
/// resources it refers to, for `slerp_materials::godot_materials`.
fn tres_text(file: &ResFile, kind: &str, props: &[(String, Value)]) -> String {
    use std::fmt::Write;
    let mut s = String::new();
    let _ = writeln!(s, "[gd_resource type=\"{kind}\" format={}]\n", if file.godot3 { 2 } else { 3 });
    let mut ids: Vec<&str> = Vec::new();
    fn collect<'a>(v: &'a Value, out: &mut Vec<&'a str>) {
        match v {
            Value::Ext(id) => out.push(id),
            Value::Array(items) => items.iter().for_each(|i| collect(i, out)),
            Value::Dict(pairs) => pairs.iter().for_each(|(_, v)| collect(v, out)),
            _ => {}
        }
    }
    for (_, v) in props {
        collect(v, &mut ids);
    }
    ids.sort();
    ids.dedup();
    for id in ids {
        if let Some(e) = file.ext(id) {
            s.push_str("[ext_resource type=");
            variant::write_string(&mut s, &e.kind);
            s.push_str(" path=");
            variant::write_string(&mut s, &e.path);
            s.push_str(" id=");
            variant::write_string(&mut s, &e.id);
            s.push_str("]\n");
        }
    }
    s.push_str("\n[resource]\n");
    for (k, v) in props {
        if k.contains(['=', '\n']) {
            continue;
        }
        s.push_str(k);
        s.push_str(" = ");
        v.write_text(&mut s);
        s.push('\n');
    }
    s
}

/// The scale and offset an OBJ's `.import` file gives its mesh.
fn obj_import_transform(path: &Path) -> Transform {
    let mut name = path.as_os_str().to_os_string();
    name.push(".import");
    let Ok(text) = std::fs::read_to_string(PathBuf::from(name)) else { return Transform::IDENTITY };
    let mut t = Transform::IDENTITY;
    for line in text.lines().take(10_000) {
        let vector = |rest: &str| -> Option<Vec3> {
            let inner = rest.trim().strip_prefix("Vector3(")?.strip_suffix(')')?;
            let v: Vec<f32> = inner.split(',').filter_map(|x| x.trim().parse().ok()).collect();
            (v.len() == 3).then(|| Vec3::new(v[0], v[1], v[2])).filter(|v| v.is_finite())
        };
        if let Some(v) = line.trim().strip_prefix("scale_mesh=").and_then(vector) {
            t.scale = v;
        } else if let Some(v) = line.trim().strip_prefix("offset_mesh=").and_then(vector) {
            t.translation = v;
        }
    }
    t
}

//endregion

#[cfg(test)]
mod tests;
