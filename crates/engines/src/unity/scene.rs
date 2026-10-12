//! Building a Unity scene or prefab: the tree of GameObjects with prefab
//! instances expanded, then the output scene.

use super::material::{self, Assets, MatProps, TextureCache};
use super::mesh;
use super::project::Project;
use super::xxh64::model_file_id;
use super::yaml::{self, Yaml};
use crate::builder::{Builder, CameraInfo, quat};
use slerp_scene::glam::{Quat, Vec3, Vec4};
use slerp_scene::{Limits, LoadError, Mesh, Node, Scene, Texture, Transform};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

/// How deep prefabs may nest.
const MAX_PREFAB_DEPTH: usize = 16;
/// The most nodes one load's trees may hold, counting every prefab copy.
const MAX_UNODES: usize = 2_000_000;
const MAX_MESH_VERTICES: usize = 50_000_000;
const MASK: i64 = i64::MAX;

pub(crate) fn load(path: &Path, bytes: &[u8], limits: &Limits) -> Result<Scene, LoadError> {
    let text = String::from_utf8_lossy(bytes).into_owned();
    let mut ctx = Ctx::new(path, limits);
    let main = ctx.add_file(path.to_path_buf(), text);
    if ctx.files[main].docs.is_empty() {
        return Err(LoadError::invalid("no Unity objects in the file"));
    }
    let tree = ctx.tree(main, 0);
    if tree.nodes.is_empty() {
        return Err(LoadError::Unsupported("nothing to show: the file has no GameObjects".into()));
    }
    let mut visited = vec![false; tree.nodes.len()];
    for &r in &tree.roots {
        ctx.emit(&tree, r, None, true, &mut visited, 0);
    }
    for w in std::mem::take(&mut ctx.textures.warnings) {
        ctx.b.warn(w);
    }
    if !ctx.missing.is_empty() {
        let first: Vec<&str> = ctx.missing.iter().take(3).map(String::as_str).collect();
        let more = if ctx.missing.len() > 3 { ", ..." } else { "" };
        let n = ctx.missing.len();
        ctx.b.warn(format!("{n} assets the scene refers to aren't in the project ({}{more})", first.join(", ")));
    }
    let kind = if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("prefab")) { "prefab" } else { "scene" };
    let mut b = ctx.b;
    b.scene.info.details.push(("GameObjects".into(), tree.nodes.len().to_string()));
    b.scene.info.details.push(("Prefab files".into(), ctx.trees.len().to_string()));
    if ctx.project.in_project {
        b.scene.info.details.push(("Project".into(), ctx.project.root.display().to_string()));
    }
    let version = ctx.version.map(|v| format!(" ({v})")).unwrap_or_default();
    Ok(b.finish(format!("Unity {kind}{version}"), &ctx.marker_nodes))
}

/*┌─────────────────╮
| Files           |
└─────────────────╯*/
//region Files

struct DocInfo {
    class: i64,
    file_id: i64,
    stripped: bool,
    range: Range<usize>,
}

/// A Unity YAML file, its documents parsed on demand.
struct UFile {
    path: PathBuf,
    text: String,
    docs: Vec<DocInfo>,
    by_id: HashMap<i64, usize>,
    parsed: RefCell<HashMap<usize, Rc<Yaml>>>,
}

impl UFile {
    fn new(path: PathBuf, text: String) -> UFile {
        let docs: Vec<DocInfo> = yaml::split(&text)
            .into_iter()
            .map(|d| {
                let start = d.text.as_ptr() as usize - text.as_ptr() as usize;
                DocInfo { class: d.class, file_id: d.file_id, stripped: d.stripped, range: start..start + d.text.len() }
            })
            .collect();
        let by_id = docs.iter().enumerate().map(|(i, d)| (d.file_id, i)).collect();
        UFile { path, text, docs, by_id, parsed: RefCell::new(HashMap::new()) }
    }

    fn doc(&self, i: usize) -> Rc<Yaml> {
        if let Some(d) = self.parsed.borrow().get(&i) {
            return d.clone();
        }
        let range = self.docs[i].range.clone();
        let parsed = Rc::new(yaml::parse_doc(&self.text[range]).unwrap_or_default());
        self.parsed.borrow_mut().insert(i, parsed.clone());
        parsed
    }

    fn doc_by_id(&self, id: i64) -> Option<(usize, Rc<Yaml>)> {
        let &i = self.by_id.get(&id)?;
        Some((i, self.doc(i)))
    }
}

//endregion

/*┌─────────────────╮
| Trees           |
└─────────────────╯*/
//region Trees

/// A reference to an object: in the same file (no GUID, `file` says which)
/// or in the asset a GUID names.
#[derive(Clone, Debug, PartialEq)]
struct ObjRef {
    file_id: i64,
    guid: Option<String>,
    file: usize,
}

fn obj_ref(v: &Yaml, file: usize) -> Option<ObjRef> {
    let file_id = v.get("fileID").i64()?;
    if file_id == 0 {
        return None;
    }
    let guid = v.get("guid").str().map(str::to_string);
    Some(ObjRef { file_id, guid, file })
}

/// A change a prefab instance makes inside a placed model, resolved once
/// the model is loaded.
#[derive(Clone, Debug)]
struct Mod {
    target: i64,
    path: String,
    value: String,
    obj: Option<ObjRef>,
}

#[derive(Clone, Debug)]
struct UNode {
    name: String,
    active: bool,
    tag: String,
    /// Local transform in Unity's left-handed space.
    t: Vec3,
    r: [f32; 4],
    s: Vec3,
    children: Vec<usize>,
    mesh: Option<ObjRef>,
    materials: Vec<Option<ObjRef>>,
    /// Has a renderer, enabled.
    renderer: bool,
    light: Option<Vec4>,
    camera: bool,
    /// The ProBuilder component that can rebuild the node's mesh when the
    /// file keeps none.
    probuilder: Option<ObjRef>,
    /// The model file this node places (an instance of a model prefab).
    model: Option<PathBuf>,
    /// Shared between the copies of a prefab until one is changed, so
    /// instancing never copies them.
    model_mods: Rc<Vec<Mod>>,
    removed: bool,
}

impl Default for UNode {
    fn default() -> Self {
        UNode {
            name: String::new(),
            active: true,
            tag: String::new(),
            t: Vec3::ZERO,
            r: [0.0, 0.0, 0.0, 1.0],
            s: Vec3::ONE,
            children: Vec::new(),
            mesh: None,
            materials: Vec::new(),
            renderer: false,
            light: None,
            camera: false,
            probuilder: None,
            model: None,
            model_mods: Rc::new(Vec::new()),
            removed: false,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Comp {
    GameObject,
    Transform,
    MeshFilter,
    Renderer,
    Light,
    Camera,
    Other,
}

fn comp_of(class: i64) -> Comp {
    match class {
        1 => Comp::GameObject,
        4 | 224 => Comp::Transform,
        33 => Comp::MeshFilter,
        23 | 137 => Comp::Renderer,
        108 => Comp::Light,
        20 => Comp::Camera,
        _ => Comp::Other,
    }
}

#[derive(Clone, Default)]
struct Tree {
    nodes: Vec<UNode>,
    roots: Vec<usize>,
    /// Every object of the file, nested prefabs' included, by file id.
    ids: HashMap<i64, (usize, Comp)>,
}

/// Applies one property modification to a node of a tree.
fn apply_mod(node: &mut UNode, comp: Comp, path: &str, value: &str, obj: Option<ObjRef>) {
    let f = || value.trim().parse::<f32>().ok().filter(|v| v.is_finite());
    match (comp, path) {
        (Comp::GameObject, "m_Name") => node.name = value.to_string(),
        (Comp::GameObject, "m_IsActive") => node.active = value.trim() != "0",
        (Comp::GameObject, "m_TagString") => node.tag = value.to_string(),
        (Comp::Transform, p) => {
            let Some(v) = f() else { return };
            match p {
                "m_LocalPosition.x" => node.t.x = v,
                "m_LocalPosition.y" => node.t.y = v,
                "m_LocalPosition.z" => node.t.z = v,
                "m_LocalRotation.x" => node.r[0] = v,
                "m_LocalRotation.y" => node.r[1] = v,
                "m_LocalRotation.z" => node.r[2] = v,
                "m_LocalRotation.w" => node.r[3] = v,
                "m_LocalScale.x" => node.s.x = v,
                "m_LocalScale.y" => node.s.y = v,
                "m_LocalScale.z" => node.s.z = v,
                _ => {}
            }
        }
        (Comp::Renderer, "m_Enabled") => node.renderer = value.trim() != "0",
        (Comp::Renderer, "m_Materials.Array.size") => {
            if let Ok(n) = value.trim().parse::<usize>() {
                node.materials.resize(n.min(256), None);
            }
        }
        (Comp::Renderer | Comp::MeshFilter, "m_Mesh") => node.mesh = obj,
        (Comp::Renderer, p) => {
            if let Some(i) = p
                .strip_prefix("m_Materials.Array.data[")
                .and_then(|r| r.strip_suffix(']'))
                .and_then(|i| i.parse::<usize>().ok())
                .filter(|&i| i < 256)
            {
                if node.materials.len() <= i {
                    node.materials.resize(i + 1, None);
                }
                node.materials[i] = obj;
            }
        }
        (Comp::Light, p) => {
            let Some(v) = f() else { return };
            let c = node.light.get_or_insert(Vec4::ONE);
            match p {
                "m_Color.r" => c.x = v,
                "m_Color.g" => c.y = v,
                "m_Color.b" => c.z = v,
                _ => {}
            }
        }
        _ => {}
    }
}

fn is_model_path(path: &Path) -> bool {
    let ext = path.extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase()).unwrap_or_default();
    matches!(ext.as_str(), "fbx" | "obj" | "dae" | "3ds" | "dxf" | "blend" | "gltf" | "glb" | "ply" | "stl")
}

//endregion

/*┌─────────────────╮
| Context         |
└─────────────────╯*/
//region Context

/// An object by the file it is in and its file id.
type ObjKey = (PathBuf, i64);

/// Primitives with the sub-mesh number each draws.
type SubMeshes = Vec<(usize, slerp_scene::Primitive)>;

/// What Unity's importer did to a model, from its `.meta` file.
#[derive(Default)]
struct ModelMeta {
    global_scale: f32,
    use_file_units: bool,
    /// Object names by file id, from the tables older Unity versions keep.
    names: HashMap<i64, String>,
    /// Materials replacing the model's own, by material name.
    external: HashMap<String, ObjRef>,
}

pub(crate) struct Ctx<'a> {
    b: Builder<'a>,
    limits: &'a Limits,
    project: Project,
    files: Vec<Rc<UFile>>,
    file_index: HashMap<PathBuf, Option<usize>>,
    trees: HashMap<usize, Rc<Tree>>,
    stack: Vec<usize>,
    unodes: usize,
    textures: TextureCache,
    materials: HashMap<(PathBuf, i64), Option<usize>>,
    meshes: HashMap<(PathBuf, i64), Option<(usize, Transform)>>,
    probuilders: HashMap<ObjKey, Option<Rc<SubMeshes>>>,
    probuilder_meshes: HashMap<(ObjKey, Vec<Option<usize>>), usize>,
    model_metas: HashMap<PathBuf, Rc<ModelMeta>>,
    marker_nodes: Vec<usize>,
    warned: HashSet<String>,
    version: Option<String>,
    /// GUIDs the scene refers to that the project doesn't have.
    missing: Vec<String>,
}

impl Assets for Ctx<'_> {
    fn asset(&mut self, guid: &str) -> Option<PathBuf> {
        // Unity's built-in resources live in the editor, not the project.
        if guid.starts_with("0000000000000000") {
            return None;
        }
        let p = self.project.asset(guid);
        if p.is_none() && !self.missing.iter().any(|m| m == guid) && self.missing.len() < 10_000 {
            self.missing.push(guid.to_string());
        }
        p
    }

    fn add_texture(&mut self, texture: Arc<Texture>) -> usize {
        self.b.add_texture(texture)
    }

    fn limits(&self) -> &Limits {
        self.limits
    }

    fn texture_meta(&mut self, path: &Path) -> Option<Rc<Yaml>> {
        self.project.meta(path)
    }
}

impl<'a> Ctx<'a> {
    fn new(path: &Path, limits: &'a Limits) -> Self {
        let project = Project::find(path);
        let version = std::fs::read_to_string(project.root.join("ProjectSettings/ProjectVersion.txt"))
            .ok()
            .and_then(|t| t.lines().find_map(|l| l.strip_prefix("m_EditorVersion: ").map(|v| v.trim().to_string())));
        Ctx {
            b: Builder::new(limits),
            limits,
            project,
            files: Vec::new(),
            file_index: HashMap::new(),
            trees: HashMap::new(),
            stack: Vec::new(),
            unodes: 0,
            textures: TextureCache::default(),
            materials: HashMap::new(),
            meshes: HashMap::new(),
            probuilders: HashMap::new(),
            probuilder_meshes: HashMap::new(),
            model_metas: HashMap::new(),
            marker_nodes: Vec::new(),
            warned: HashSet::new(),
            version,
            missing: Vec::new(),
        }
    }

    fn warn_once(&mut self, message: String) {
        if self.warned.len() < 200 && self.warned.insert(message.clone()) {
            self.b.warn(message);
        }
    }

    fn add_file(&mut self, path: PathBuf, text: String) -> usize {
        self.files.push(Rc::new(UFile::new(path.clone(), text)));
        let i = self.files.len() - 1;
        self.file_index.insert(path, Some(i));
        i
    }

    /// Reads a YAML asset (prefab, material, `.asset`) once.
    fn load_file(&mut self, path: &Path) -> Option<usize> {
        if let Some(&i) = self.file_index.get(path) {
            return i;
        }
        slerp_scene::record_neighbour(path);
        let text = if !self.limits.neighbours {
            None
        } else {
            std::fs::metadata(path)
                .ok()
                .filter(|m| m.len() <= self.limits.max_file_bytes)
                .and_then(|_| std::fs::read(path).ok())
                .filter(|b| b.starts_with(b"%YAML"))
                .map(|b| String::from_utf8_lossy(&b).into_owned())
        };
        match text {
            Some(t) => Some(self.add_file(path.to_path_buf(), t)),
            None => {
                let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                self.warn_once(format!("{name}: missing, or not saved as text"));
                self.file_index.insert(path.to_path_buf(), None);
                None
            }
        }
    }

    /// The file an object reference points into.
    fn ref_file(&mut self, r: &ObjRef) -> Option<usize> {
        match &r.guid {
            None => Some(r.file),
            Some(g) => {
                let path = self.asset(g)?;
                if is_model_path(&path) { None } else { self.load_file(&path) }
            }
        }
    }

    fn model_meta(&mut self, path: &Path) -> Rc<ModelMeta> {
        if let Some(m) = self.model_metas.get(path) {
            return m.clone();
        }
        let mut meta = ModelMeta { global_scale: 1.0, use_file_units: true, ..Default::default() };
        if let Some(y) = self.project.meta(path) {
            let mi = y.get("ModelImporter");
            meta.global_scale = mi.at("meshes.globalScale").f32().filter(|s| *s > 0.0).unwrap_or(1.0);
            meta.use_file_units = mi.at("meshes.useFileUnits").i64().unwrap_or(1) != 0;
            for (id, name) in mi.get("fileIDToRecycleName").map() {
                if let (Ok(id), Some(n)) = (id.parse::<i64>(), name.str()) {
                    meta.names.insert(id, n.to_string());
                }
            }
            for entry in mi.get("internalIDToNameTable").seq() {
                let name = entry.get("second").str().unwrap_or("").to_string();
                for (_, id) in entry.get("first").map() {
                    if let Some(id) = id.i64() {
                        meta.names.insert(id, name.clone());
                    }
                }
            }
            for entry in mi.get("externalObjects").seq() {
                let first = entry.get("first");
                if first.get("type").str().is_some_and(|t| t.ends_with("Material"))
                    && let (Some(name), Some(r)) = (first.get("name").str(), obj_ref(entry.get("second"), usize::MAX))
                {
                    meta.external.insert(name.to_string(), r);
                }
            }
        }
        let meta = Rc::new(meta);
        self.model_metas.insert(path.to_path_buf(), meta.clone());
        meta
    }
}

//endregion

/*┌─────────────────╮
| Building trees  |
└─────────────────╯*/
//region Building trees

impl Ctx<'_> {
    /// The tree of file `f`, built once.
    fn tree(&mut self, f: usize, depth: usize) -> Rc<Tree> {
        if let Some(t) = self.trees.get(&f) {
            return t.clone();
        }
        self.stack.push(f);
        let t = Rc::new(self.build_tree(f, depth));
        self.stack.pop();
        self.trees.insert(f, t.clone());
        t
    }

    fn build_tree(&mut self, f: usize, depth: usize) -> Tree {
        let file = self.files[f].clone();
        let mut t = Tree::default();
        // 1. GameObjects, one node each.
        let mut go_node: HashMap<i64, usize> = HashMap::new();
        // Objects copied in full from a prefab by older Unity versions
        // name that prefab instance; such instances aren't expanded again.
        let mut present_instances: HashSet<i64> = HashSet::new();
        for (i, d) in file.docs.iter().enumerate() {
            if d.class != 1 || d.stripped {
                continue;
            }
            if self.unodes >= MAX_UNODES {
                self.warn_once("the scene has too many objects: only part of it is shown".into());
                break;
            }
            let doc = file.doc(i);
            if let Some(p) = doc.at("m_PrefabInternal.fileID").i64().filter(|&p| p != 0 && p != 100100000) {
                present_instances.insert(p);
            }
            let node = UNode {
                name: doc.get("m_Name").str().unwrap_or("").to_string(),
                active: doc.get("m_IsActive").i64().unwrap_or(1) != 0,
                tag: doc.get("m_TagString").str().unwrap_or("").to_string(),
                ..Default::default()
            };
            t.nodes.push(node);
            self.unodes += 1;
            let n = t.nodes.len() - 1;
            go_node.insert(d.file_id, n);
            t.ids.insert(d.file_id, (n, Comp::GameObject));
        }
        // 2. Transforms and components.
        let mut fathers: Vec<(usize, i64)> = Vec::new();
        for (i, d) in file.docs.iter().enumerate() {
            if d.stripped || !matches!(d.class, 4 | 224 | 33 | 23 | 137 | 108 | 20 | 218 | 114) {
                continue;
            }
            // Of scripts, only ProBuilder's matters; checking the text first
            // spares parsing every other script's data.
            if d.class == 114 && !file.text[d.range.clone()].contains(mesh::PROBUILDER_SCRIPT) {
                continue;
            }
            let doc = file.doc(i);
            let Some(&n) = doc.at("m_GameObject.fileID").i64().and_then(|g| go_node.get(&g)) else { continue };
            t.ids.insert(d.file_id, (n, comp_of(d.class)));
            let node = &mut t.nodes[n];
            let v3 = |k: &str, d: Vec3| {
                let v = doc.get(k);
                Vec3::new(
                    v.get("x").f32().unwrap_or(d.x),
                    v.get("y").f32().unwrap_or(d.y),
                    v.get("z").f32().unwrap_or(d.z),
                )
            };
            match d.class {
                4 | 224 => {
                    node.t = v3("m_LocalPosition", Vec3::ZERO);
                    node.s = v3("m_LocalScale", Vec3::ONE);
                    let r = doc.get("m_LocalRotation");
                    node.r = ["x", "y", "z", "w"].map(|k| r.get(k).f32().unwrap_or(if k == "w" { 1.0 } else { 0.0 }));
                    fathers.push((n, doc.at("m_Father.fileID").i64().unwrap_or(0)));
                }
                33 => node.mesh = obj_ref(doc.get("m_Mesh"), f),
                23 | 137 => {
                    node.renderer = doc.get("m_Enabled").i64().unwrap_or(1) != 0;
                    node.materials = doc.get("m_Materials").seq().iter().map(|m| obj_ref(m, f)).collect();
                    if d.class == 137 {
                        node.mesh = obj_ref(doc.get("m_Mesh"), f);
                    }
                }
                108 => {
                    if doc.get("m_Enabled").i64().unwrap_or(1) != 0 {
                        let c = doc.get("m_Color");
                        node.light = Some(Vec4::new(
                            c.get("r").f32().unwrap_or(1.0),
                            c.get("g").f32().unwrap_or(1.0),
                            c.get("b").f32().unwrap_or(1.0),
                            1.0,
                        ));
                    }
                }
                20 => node.camera = doc.get("m_Enabled").i64().unwrap_or(1) != 0,
                114 if doc.at("m_Script.guid").str() == Some(mesh::PROBUILDER_SCRIPT) => {
                    node.probuilder = Some(ObjRef { file_id: d.file_id, guid: None, file: f });
                }
                218 => self.warn_once("Unity terrain isn't drawn".into()),
                _ => {}
            }
        }
        // 3. Prefab instances, then the stripped objects that stand for
        //    objects inside them.
        let mut instance_roots: Vec<(usize, i64)> = Vec::new();
        let mut model_instances: HashMap<i64, usize> = HashMap::new();
        for (i, d) in file.docs.iter().enumerate() {
            if d.class != 1001 || d.stripped || present_instances.contains(&d.file_id) {
                continue;
            }
            let doc = file.doc(i);
            // Older prefab files carry a Prefab object for the asset itself.
            if !doc.get("m_RootGameObject").is_null() || doc.get("m_IsPrefabParent").i64() == Some(1) {
                continue;
            }
            if let Some((root, parent, model)) = self.instance(&mut t, f, d.file_id, &doc, depth) {
                instance_roots.push((root, parent));
                if model {
                    model_instances.insert(d.file_id, root);
                }
            }
        }
        for (i, d) in file.docs.iter().enumerate() {
            if !d.stripped {
                continue;
            }
            let doc = file.doc(i);
            let inst = doc
                .at("m_PrefabInstance.fileID")
                .i64()
                .or_else(|| doc.at("m_PrefabInternal.fileID").i64())
                .unwrap_or(0);
            let src = doc
                .at("m_CorrespondingSourceObject.fileID")
                .i64()
                .or_else(|| doc.at("m_PrefabParentObject.fileID").i64())
                .unwrap_or(0);
            if let Some(&found) = t.ids.get(&((inst ^ src) & MASK)) {
                t.ids.insert(d.file_id, found);
            } else if let Some(&root) = model_instances.get(&inst) {
                t.ids.insert(d.file_id, (root, comp_of(d.class)));
            }
        }
        // 4. The hierarchy: Transforms name their fathers, instances their
        //    parent Transform.
        for (n, father) in fathers.into_iter().chain(instance_roots) {
            match t.ids.get(&father) {
                Some(&(p, _)) if father != 0 && p != n => t.nodes[p].children.push(n),
                _ => t.roots.push(n),
            }
        }
        t
    }

    /// Expands a prefab instance into `t`.  Returns its root, the file id
    /// of the Transform it hangs from, and whether it places a model.
    fn instance(&mut self, t: &mut Tree, f: usize, id: i64, doc: &Yaml, depth: usize) -> Option<(usize, i64, bool)> {
        let src = doc.get("m_SourcePrefab");
        let src = if src.is_null() { doc.get("m_ParentPrefab") } else { src };
        let guid = src.get("guid").str()?.to_string();
        let path = self.asset(&guid)?;
        let m = doc.get("m_Modification");
        let parent = m.at("m_TransformParent.fileID").i64().unwrap_or(0);
        let mods: Vec<Mod> = m
            .get("m_Modifications")
            .seq()
            .iter()
            .map(|x| Mod {
                target: x.at("target.fileID").i64().unwrap_or(0),
                path: x.get("propertyPath").str().unwrap_or("").to_string(),
                value: x.get("value").str().unwrap_or("").to_string(),
                obj: obj_ref(x.get("objectReference"), f),
            })
            .collect();

        // 1. A model file: one node that places it.  Changes to the model's
        //    root (its place, name and activity) apply to that node; the
        //    root is "//RootNode" in the `.meta` file's table, or, where
        //    Unity keeps none, the object most of the transform changes
        //    aim at (Unity writes all seven of a placed root's).
        if is_model_path(&path) {
            let name = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
            let meta = self.model_meta(&path);
            let is_root_change =
                |m: &Mod| m.path.starts_with("m_Local") || m.path == "m_Name" || m.path == "m_IsActive";
            let mut counts: HashMap<i64, usize> = HashMap::new();
            for m in mods.iter().filter(|m| m.path.starts_with("m_Local")) {
                *counts.entry(m.target).or_default() += 1;
            }
            let busiest = counts.iter().max_by(|a, b| a.1.cmp(b.1).then(b.0.cmp(a.0))).map(|(&id, _)| id);
            let root_ids: HashSet<i64> = mods
                .iter()
                .filter(|m| is_root_change(m))
                .map(|m| m.target)
                .filter(|id| match meta.names.get(id) {
                    Some(n) => n == "//RootNode",
                    // Unity names a placed root's GameObject in every
                    // instance; children rarely are.
                    None => Some(*id) == busiest || mods.iter().any(|x| x.target == *id && x.path == "m_Name"),
                })
                .collect();
            let mut node = UNode { name, model: Some(path), ..Default::default() };
            for m in mods {
                if root_ids.contains(&m.target) && is_root_change(&m) {
                    let comp = if m.path.starts_with("m_Local") { Comp::Transform } else { Comp::GameObject };
                    apply_mod(&mut node, comp, &m.path, &m.value, m.obj);
                } else {
                    Rc::make_mut(&mut node.model_mods).push(m);
                }
            }
            t.nodes.push(node);
            self.unodes += 1;
            return Some((t.nodes.len() - 1, parent, true));
        }

        // 2. A prefab: a copy of its tree, with the instance's changes.
        if depth >= MAX_PREFAB_DEPTH {
            self.warn_once("prefabs nested too deeply".into());
            return None;
        }
        let sub = self.load_file(&path)?;
        if self.stack.contains(&sub) {
            self.warn_once(format!("{} contains itself", path.display()));
            return None;
        }
        let template = self.tree(sub, depth + 1);
        if template.roots.is_empty() {
            return None;
        }
        if self.unodes + template.nodes.len() > MAX_UNODES {
            self.warn_once("the scene has too many objects: only part of it is shown".into());
            return None;
        }
        self.unodes += template.nodes.len();
        let offset = t.nodes.len();
        for n in &template.nodes {
            let mut copy = n.clone();
            copy.children = n.children.iter().map(|c| c + offset).collect();
            t.nodes.push(copy);
        }
        for (&sid, &(n, c)) in &template.ids {
            t.ids.insert((id ^ sid) & MASK, (n + offset, c));
        }
        for m in mods {
            match template.ids.get(&m.target) {
                Some(&(n, c)) => {
                    let node = &mut t.nodes[n + offset];
                    if node.model.is_some() && c != Comp::GameObject && c != Comp::Transform {
                        Rc::make_mut(&mut node.model_mods).push(m);
                    } else {
                        apply_mod(node, c, &m.path, &m.value, m.obj);
                    }
                }
                // Aimed inside a model the prefab places.
                None => {
                    if let Some(&r) = template.roots.first() {
                        let root = &mut t.nodes[r + offset];
                        if root.model.is_some() {
                            Rc::make_mut(&mut root.model_mods).push(m);
                        }
                    }
                }
            }
        }
        for removed in doc.at("m_Modification.m_RemovedComponents").seq() {
            if let Some(&(n, c)) = removed.get("fileID").i64().and_then(|r| template.ids.get(&r)) {
                let node = &mut t.nodes[n + offset];
                match c {
                    Comp::Renderer => node.renderer = false,
                    Comp::MeshFilter => node.mesh = None,
                    Comp::Light => node.light = None,
                    _ => {}
                }
            }
        }
        for removed in doc.at("m_Modification.m_RemovedGameObjects").seq() {
            if let Some(&(n, _)) = removed.get("fileID").i64().and_then(|r| template.ids.get(&r)) {
                t.nodes[n + offset].removed = true;
            }
        }
        let root = template.roots[0] + offset;
        // A prefab with several roots (only possible in damaged files)
        // keeps the others as roots.
        for &r in &template.roots[1..] {
            t.roots.push(r + offset);
        }
        Some((root, parent, false))
    }
}

//endregion

/*┌─────────────────╮
| Emitting        |
└─────────────────╯*/
//region Emitting

/// Unity's left-handed local transform in Slerp's right-handed space:
/// mirroring Z negates Z and turns rotations the other way about X and Y.
fn convert(n: &UNode) -> Transform {
    let q = quat(-n.r[0], -n.r[1], n.r[2], n.r[3]);
    let t = Vec3::new(n.t.x, n.t.y, -n.t.z);
    let s = if n.s.is_finite() { n.s } else { Vec3::ONE };
    Transform { translation: if t.is_finite() { t } else { Vec3::ZERO }, rotation: q, scale: s }
}

/// Where a model's own (right-handed) content goes under a Unity node: the
/// importer mirrors X, Slerp mirrors Z back, which together turn it half
/// way round Y.
fn model_root(scale: f32) -> Transform {
    Transform {
        rotation: Quat::from_rotation_y(std::f32::consts::PI),
        scale: Vec3::splat(scale),
        ..Transform::IDENTITY
    }
}

impl Ctx<'_> {
    fn emit(
        &mut self,
        tree: &Tree,
        n: usize,
        parent: Option<usize>,
        visible: bool,
        visited: &mut [bool],
        depth: usize,
    ) {
        // Deeper than this is a hostile file, and recursing on would
        // overflow the stack.
        if n >= tree.nodes.len() || visited[n] || depth > 1000 {
            return;
        }
        visited[n] = true;
        let u = &tree.nodes[n];
        if u.removed {
            return;
        }
        let visible = visible && u.active;
        let mut node = Node::new(u.name.clone());
        node.transform = convert(u);
        node.visible = visible;
        let Some(out) = self.b.add_node(parent, node) else { return };
        // 1. What it draws.
        if u.renderer {
            let drawn = match u.mesh.clone() {
                Some(mesh_ref) => self.emit_mesh(u, &mesh_ref, out, visible),
                None => false,
            };
            if !drawn && let Some(pb) = u.probuilder.clone() {
                self.emit_probuilder(u, &pb, out);
            }
        }
        if let Some(path) = u.model.clone() {
            self.emit_model(u, &path, out, visible);
        }
        if let Some(color) = u.light {
            self.b.add_marker(format!("{} (light)", u.name), color);
            self.marker_nodes.push(out);
        }
        if u.camera {
            self.b.cameras.push(CameraInfo { node: out, current: u.tag == "MainCamera" });
            self.b.add_marker(format!("{} (camera)", u.name), Vec4::new(0.6, 0.8, 1.0, 1.0));
            self.marker_nodes.push(out);
        }
        // 2. Children.
        for &c in &u.children {
            self.emit(tree, c, Some(out), visible, visited, depth + 1);
        }
    }

    fn renderer_materials(&mut self, u: &UNode) -> Vec<Option<usize>> {
        u.materials.iter().map(|m| m.as_ref().and_then(|m| self.material(m))).collect()
    }

    /// Draws a ProBuilder mesh from its component, each sub-mesh wearing
    /// the renderer's material of that number.
    fn emit_probuilder(&mut self, u: &UNode, pb: &ObjRef, out: usize) {
        let key = (self.files[pb.file].path.clone(), pb.file_id);
        let subs = match self.probuilders.get(&key) {
            Some(s) => s.clone(),
            None => {
                let file = self.files[pb.file].clone();
                let made =
                    file.doc_by_id(pb.file_id).and_then(|(_, doc)| match mesh::probuilder(&doc, MAX_MESH_VERTICES) {
                        Ok(subs) => Some(Rc::new(subs)),
                        Err(e) => {
                            self.warn_once(format!("ProBuilder mesh {}: {e}", u.name));
                            None
                        }
                    });
                self.probuilders.insert(key.clone(), made.clone());
                made
            }
        };
        let Some(subs) = subs else { return };
        let materials = self.renderer_materials(u);
        // One mesh for every copy of a prefab with the same materials.
        let made_key = (key, materials.clone());
        if let Some(&index) = self.probuilder_meshes.get(&made_key) {
            self.b.scene.nodes[out].mesh = Some(index);
            return;
        }
        let primitives = subs
            .iter()
            .map(|(sub, p)| slerp_scene::Primitive { material: materials.get(*sub).copied().flatten(), ..p.clone() })
            .collect();
        let index = self.b.add_mesh(Mesh { name: u.name.clone(), primitives, target_names: vec![] });
        self.probuilder_meshes.insert(made_key, index);
        self.b.scene.nodes[out].mesh = Some(index);
    }

    /// Draws a node's mesh; false when the reference can't be resolved.
    fn emit_mesh(&mut self, u: &UNode, r: &ObjRef, out: usize, visible: bool) -> bool {
        let Some((mesh, transform)) = self.mesh(r) else { return false };
        let materials = self.renderer_materials(u);
        let mesh = self.b.with_materials(mesh, &materials);
        if transform == Transform::IDENTITY {
            self.b.scene.nodes[out].mesh = Some(mesh);
        } else if let Some(n) = self.b.add_mesh_node(Some(out), "mesh", transform, mesh) {
            self.b.scene.nodes[n].visible = visible;
        }
        true
    }

    /// The output mesh an object reference names, and the transform it
    /// needs under its node (meshes from model files).
    fn mesh(&mut self, r: &ObjRef) -> Option<(usize, Transform)> {
        // 1. Unity's built-in primitives.
        if r.guid.as_deref() == Some(mesh::BUILTIN_EXTRA) {
            let key = (PathBuf::from("builtin"), r.file_id);
            if let Some(m) = self.meshes.get(&key) {
                return *m;
            }
            let made = mesh::builtin(r.file_id).map(|(name, prim)| {
                let index = self.b.add_mesh(Mesh { name: name.into(), primitives: vec![prim], target_names: vec![] });
                (index, Transform::IDENTITY)
            });
            self.meshes.insert(key, made);
            return made;
        }
        // 2. A mesh inside a model file.
        if let Some(g) = &r.guid
            && let Some(path) = self.asset(g)
            && is_model_path(&path)
        {
            let key = (path.clone(), r.file_id);
            if let Some(m) = self.meshes.get(&key) {
                return *m;
            }
            let made = self.model_mesh(&path, r.file_id);
            self.meshes.insert(key, made);
            return made;
        }
        // 3. A mesh serialized in YAML: in this file, another scene or
        //    prefab, or an `.asset` file.
        let f = self.ref_file(r)?;
        let key = (self.files[f].path.clone(), r.file_id);
        if let Some(m) = self.meshes.get(&key) {
            return *m;
        }
        let file = self.files[f].clone();
        let made = match file.doc_by_id(r.file_id).or_else(|| {
            // An `.asset` file named by GUID with its usual id.
            file.docs.iter().position(|d| d.class == 43).map(|i| (i, file.doc(i)))
        }) {
            Some((i, doc)) if file.docs[i].class == 43 => match mesh::decode(&doc, MAX_MESH_VERTICES) {
                Ok(prims) => {
                    let name = doc.get("m_Name").str().unwrap_or("").to_string();
                    Some((self.b.add_mesh(Mesh { name, primitives: prims, target_names: vec![] }), Transform::IDENTITY))
                }
                Err(e) => {
                    self.warn_once(format!("mesh {}: {e}", doc.get("m_Name").str().unwrap_or("?")));
                    None
                }
            },
            _ => None,
        };
        self.meshes.insert(key, made);
        made
    }

    /// The name Unity gave object `file_id` of class `kind` inside a model:
    /// from the `.meta` file's tables, or by trying the hash Unity uses
    /// when it keeps none against the model's own names.
    fn model_object_name(&mut self, path: &Path, model: usize, kind: &str, file_id: i64) -> Option<String> {
        let meta = self.model_meta(path);
        if let Some(n) = meta.names.get(&file_id) {
            return Some(n.clone());
        }
        let info = self.b.model_info(model)?;
        let mut candidates: Vec<String> = info.node_names().map(str::to_string).collect();
        if kind == "Material" {
            candidates = info.material_names().map(str::to_string).collect();
        }
        candidates.into_iter().find(|n| model_file_id(kind, n) == file_id)
    }

    fn model_mesh(&mut self, path: &Path, file_id: i64) -> Option<(usize, Transform)> {
        let model = self.b.model(path)?;
        let name = self.model_object_name(path, model, "Mesh", file_id);
        let info = self.b.model_info(model)?;
        let index = match &name {
            Some(n) => info
                .find(|node, has_mesh| has_mesh && node == n)
                .or_else(|| info.find(|node, has_mesh| has_mesh && node.starts_with(n.as_str()))),
            None => None,
        }
        .or_else(|| {
            // A model with one mesh needs no name.
            let meshes: Vec<usize> = (0..info.len()).filter(|&i| info.mesh(i).is_some()).collect();
            (meshes.len() == 1).then(|| meshes[0])
        });
        let Some(index) = index else {
            let file = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            self.warn_once(format!("{file}: mesh {} not found", name.unwrap_or_else(|| file_id.to_string())));
            return None;
        };
        let mesh = info.mesh(index)?;
        let unit = info.unit;
        // Unity scales a mesh's vertices to metres by the file's unit and
        // the import's scale factor; node transforms (which a mesh used on
        // its own goes without) keep any other scales the file has.
        let meta = self.model_meta(path);
        let unit = if meta.use_file_units { unit } else { 1.0 };
        let mesh = self.remap_model_materials(path, model, mesh);
        Some((mesh, model_root(meta.global_scale * unit)))
    }

    /// The model's mesh with the materials the project maps over the
    /// model's own (`.meta` external objects).
    fn remap_model_materials(&mut self, path: &Path, model: usize, mesh: usize) -> usize {
        let meta = self.model_meta(path);
        if meta.external.is_empty() {
            return mesh;
        }
        let names: Vec<Option<String>> = self.b.scene.meshes[mesh]
            .primitives
            .iter()
            .map(|p| p.material.and_then(|m| self.b.scene.materials.get(m)).map(|m| m.name.clone()))
            .collect();
        let _ = model;
        let mut replacements = Vec::with_capacity(names.len());
        for n in names {
            let r = n.and_then(|n| meta.external.get(&n).cloned());
            replacements.push(r.and_then(|r| self.material(&ObjRef { file: usize::MAX, ..r })));
        }
        self.b.with_materials(mesh, &replacements)
    }

    fn emit_model(&mut self, u: &UNode, path: &Path, out: usize, visible: bool) {
        let Some(model) = self.b.model(path) else { return };
        let meta = self.model_meta(path);
        let unit_fix = match self.b.model_info(model) {
            Some(info) if !meta.use_file_units && info.unit > 0.0 => 1.0 / info.unit,
            _ => 1.0,
        };
        let name = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        let Some(placed) = self.b.place_model(model, Some(out), &name, model_root(meta.global_scale * unit_fix)) else {
            return;
        };
        // 1. The instance's changes: transforms of the model's own nodes
        //    are left as the model has them; visibility and materials of
        //    named nodes are applied.
        let names: Vec<String> =
            self.b.model_info(model).map(|i| i.node_names().map(str::to_string).collect()).unwrap_or_default();
        let mut per_node: HashMap<usize, Vec<Option<usize>>> = HashMap::new();
        for m in u.model_mods.iter() {
            let target_name = self
                .model_object_name(path, model, "GameObject", m.target)
                .or_else(|| self.model_object_name(path, model, "MeshRenderer", m.target));
            let Some(i) = target_name.and_then(|n| names.iter().position(|x| *x == n)) else { continue };
            let Some(&node) = placed.nodes.get(i) else { continue };
            if m.path == "m_IsActive" && m.value.trim() == "0" {
                self.b.scene.nodes[node].visible = false;
            }
            if let Some(slot) = m
                .path
                .strip_prefix("m_Materials.Array.data[")
                .and_then(|r| r.strip_suffix(']'))
                .and_then(|s| s.parse::<usize>().ok())
                .filter(|&s| s < 256)
            {
                let mat = m.obj.as_ref().and_then(|r| self.material(r));
                let list = per_node.entry(node).or_default();
                if list.len() <= slot {
                    list.resize(slot + 1, None);
                }
                list[slot] = mat;
            }
        }
        // 2. Materials the project maps over the model's, then the
        //    instance's own choices.
        for &node in &placed.nodes {
            if let Some(mesh) = self.b.scene.nodes[node].mesh {
                let mut mesh = self.remap_model_materials(path, model, mesh);
                if let Some(list) = per_node.get(&node) {
                    mesh = self.b.with_materials(mesh, list);
                }
                self.b.scene.nodes[node].mesh = Some(mesh);
            }
            if !visible {
                self.b.scene.nodes[node].visible = false;
            }
        }
    }

    /// The output material an object reference names.
    fn material(&mut self, r: &ObjRef) -> Option<usize> {
        let guid = r.guid.clone();
        if guid.as_deref().is_some_and(|g| g.starts_with("0000000000000000")) {
            return None;
        }
        // A material inside a model file, by its name.
        if let Some(g) = &guid
            && let Some(path) = self.asset(g)
            && is_model_path(&path)
        {
            let model = self.b.model(&path)?;
            let name = self.model_object_name(&path, model, "Material", r.file_id)?;
            return self.b.model_info(model)?.material(&name);
        }
        let f = if r.file == usize::MAX && guid.is_none() { return None } else { self.ref_file(r)? };
        let key = (self.files[f].path.clone(), r.file_id);
        if let Some(&m) = self.materials.get(&key) {
            return m;
        }
        let file = self.files[f].clone();
        let doc = file
            .doc_by_id(r.file_id)
            .filter(|(i, _)| file.docs[*i].class == 21)
            .or_else(|| file.docs.iter().position(|d| d.class == 21).map(|i| (i, file.doc(i))));
        let made = doc.map(|(_, doc)| {
            let props = MatProps::read(&doc);
            let mut cache = std::mem::take(&mut self.textures);
            let m = material::build(&props, &mut cache, self);
            self.textures = cache;
            self.b.add_material(m)
        });
        self.materials.insert(key, made);
        made
    }
}

//endregion

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_handedness() {
        let h = std::f32::consts::FRAC_1_SQRT_2;
        let n = UNode { t: Vec3::new(1.0, 2.0, 3.0), r: [0.0, h, 0.0, h], ..Default::default() };
        let t = convert(&n);
        assert_eq!(t.translation, Vec3::new(1.0, 2.0, -3.0));
        // Unity's +90 degrees about Y turns +Z (forward) to +X; mirrored,
        // Slerp's -Z (forward) goes to +X as well.
        let fwd = t.rotation * Vec3::NEG_Z;
        assert!((fwd - Vec3::X).length() < 1e-5, "{fwd}");
        let m = slerp_scene::glam::Mat4::from_quat(t.rotation);
        assert!(m.is_finite());
    }
}
