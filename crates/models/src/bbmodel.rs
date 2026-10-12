//! Blockbench projects (`.bbmodel`): JSON models of boxes ("cubes") and,
//! since Blockbench 4, free polygon meshes, arranged in a tree of groups
//! that pivot about their own origins, with textures usually embedded as
//! base64 PNGs and keyframed animations of the groups.
//!
//! Blockbench works in pixels of a Minecraft block, 16 to the block, which
//! this loader takes as 16 to the metre; Y is up as in Blockbench.  Every
//! coordinate in the file is absolute: a group or cube turns about its
//! `origin`, and its children's coordinates are given in the model's space,
//! not relative to their parent.  So each group becomes a node placed at its
//! origin, and the geometry under it is stored relative to that origin.
//! Rotations are Euler angles in degrees, applied X first, then Y, then Z.
//!
//! What a face shows follows Blockbench's own drawing of it: each side of a
//! cube maps its UV rectangle corner to corner (turned by the face's
//! rotation), box-UV cubes lay out Minecraft's unfolded-box pattern from one
//! offset, and a mesh's four-sided faces are triangulated after their
//! corners are put in order.  Textures are pixel art, so they are sampled
//! with hard edges.  Animations play the groups' rotation, position and
//! scale keys with Blockbench's linear, step, Catmull-Rom and Bézier
//! interpolation, baked into linear keys; values written as Molang
//! expressions are evaluated where they only use arithmetic, `math.*` and
//! the animation time.
//!
//! Blockbench 5's armatures are read too: their bones become a skeleton and
//! meshes bound to it carry the bones' vertex weights.

use crate::common::{self, TextureSet};
use base64::Engine;
use glam::{Mat4, Quat, Vec2, Vec3, Vec4};
use serde_json::Value;
use slerp_scene::{
    AlphaMode, Animation, Channel, Filter, Interpolation, Limits, LoadError, Material, Mesh, Node, Primitive, Property,
    Sampler, Scene, Skin, TexRef, Transform,
};
use std::collections::HashMap;
use std::path::Path;

/// Blockbench pixels per metre.
const UNIT: f32 = 1.0 / 16.0;

/// Deepest group nesting followed, and most elements, groups, faces,
/// keyframes and baked samples read: far beyond real models.
const MAX_DEPTH: usize = 256;
const MAX_ITEMS: usize = 1_000_000;
const MAX_SAMPLES: usize = 4_000_000;

/// The largest project read: parsed JSON takes several times the file's
/// size in memory, and real projects, textures embedded, stay in the tens
/// of megabytes.
const MAX_PROJECT: usize = 256 << 20;

/// Rate animations are baked at, in samples per second.
const BAKE_RATE: f32 = 30.0;

/// Whether a file is a Blockbench project: JSON whose `meta` names a
/// Blockbench format.
pub fn looks_like_bbmodel(head: &[u8], is_bbmodel: bool) -> bool {
    let text = String::from_utf8_lossy(head);
    let text = text.trim_start_matches('\u{feff}').trim_start();
    if !text.starts_with('{') {
        return false;
    }
    is_bbmodel
        || (text.contains("\"meta\"")
            && (text.contains("\"model_format\"") || text.contains("\"format_version\""))
            && text.contains("\"box_uv\""))
}

/*┌─────────────────╮
| Reading values  |
└─────────────────╯*/
//region Reading values

fn num(v: &Value) -> Option<f32> {
    match v {
        Value::Number(n) => n.as_f64().map(|f| f as f32).filter(|f| f.is_finite()),
        Value::String(s) => s.trim().parse::<f32>().ok().filter(|f| f.is_finite()),
        _ => None,
    }
}

fn field(v: &Value, key: &str) -> Option<f32> {
    v.get(key).and_then(num)
}

fn vec3(v: &Value, key: &str) -> Option<Vec3> {
    let a = v.get(key)?.as_array()?;
    Some(Vec3::new(num(a.first()?)?, num(a.get(1)?)?, num(a.get(2)?)?))
}

fn flag(v: &Value, key: &str) -> Option<bool> {
    match v.get(key)? {
        Value::Bool(b) => Some(*b),
        Value::String(s) => Some(s == "true" || s == "True"),
        _ => None,
    }
}

fn text<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(Value::as_str)
}

fn array<'a>(v: &'a Value, key: &str) -> &'a [Value] {
    v.get(key).and_then(Value::as_array).map_or(&[], Vec::as_slice)
}

/// "4.10" as (4, 10), so versions compare by number.
fn version(s: &str) -> (u32, u32) {
    let mut parts = s.split('.').map(|p| p.trim().parse::<u32>().unwrap_or(0));
    (parts.next().unwrap_or(0), parts.next().unwrap_or(0))
}

/// Blockbench's rotation: X first, then Y, then Z, in degrees.
fn euler(deg: Vec3) -> Quat {
    let r = deg * (std::f32::consts::PI / 180.0);
    Quat::from_rotation_z(r.z) * Quat::from_rotation_y(r.y) * Quat::from_rotation_x(r.x)
}

//endregion

/*┌─────────────────╮
| The project     |
└─────────────────╯*/
//region The project

/// How the project's format treats textures, from Blockbench's format
/// definitions.
struct FormatRules {
    /// Every textured face uses the first texture (Bedrock entities,
    /// skins, modded entities).
    single_texture: bool,
    /// Faces without a texture of their own use the first one.
    default_texture: bool,
    /// Each texture has its own UV size; otherwise the project's
    /// resolution applies to all.
    per_texture_uv: bool,
    /// Groups pivot; Java block models' groups are only folders.
    bone_rig: bool,
    /// Box-UV unless an element says otherwise.
    box_uv: bool,
    /// Files before 5.0 stored animation X (and rotation Y) negated.
    old_animation_signs: bool,
    /// Files before 3.2 stored group Z rotation negated.
    old_group_z: bool,
}

impl FormatRules {
    fn new(meta: &Value) -> FormatRules {
        let format = text(meta, "model_format").unwrap_or("");
        let v = version(text(meta, "format_version").or_else(|| text(meta, "format")).unwrap_or("0"));
        FormatRules {
            single_texture: matches!(format, "bedrock" | "bedrock_old" | "modded_entity" | "skin" | "image"),
            default_texture: matches!(format, "bedrock_block" | "optifine_entity" | "optifine_part"),
            per_texture_uv: matches!(format, "free" | "image" | "optifine_entity" | "optifine_part"),
            bone_rig: format != "java_block" || flag(meta, "bone_rig").unwrap_or(false),
            box_uv: flag(meta, "box_uv").unwrap_or(false),
            old_animation_signs: v < (5, 0),
            old_group_z: v < (3, 2),
        }
    }
}

/// A texture as faces see it: which material it makes, and the size of
/// the UV space its coordinates count in.
struct Tex {
    material: usize,
    uv_size: Vec2,
    /// Animated textures stack frames vertically; faces show the first.
    frames: f32,
}

/// Reads a project into a scene.  The `image` format (a picture being
/// painted, with no model) returns its texture as `Err(Ok(texture))`.
pub fn load(path: &Path, bytes: &[u8], limits: &Limits) -> Result<Loaded, LoadError> {
    if bytes.len() > MAX_PROJECT {
        return Err(LoadError::TooLarge(format!("{} bytes", bytes.len())));
    }
    let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
    let root: Value = serde_json::from_slice(bytes).map_err(|e| LoadError::invalid(format!("JSON: {e}")))?;
    let meta =
        root.get("meta").filter(|m| m.is_object()).ok_or_else(|| LoadError::invalid("not a Blockbench project"))?;
    let rules = FormatRules::new(meta);
    let mut b = Builder {
        scene: Scene::default(),
        rules,
        textures: Vec::new(),
        default_material: None,
        buffers: HashMap::new(),
        group_nodes: HashMap::new(),
        warnings: Vec::new(),
        items: 0,
        vertex_keys: Vec::new(),
        vars: placeholders(&root),
        project_uv: Vec2::splat(16.0),
    };
    let format = text(meta, "model_format").unwrap_or("");

    // 1. Textures and their materials.
    let resolution = root.get("resolution");
    let project_uv = Vec2::new(
        resolution.and_then(|r| field(r, "width")).filter(|&w| w > 0.0).unwrap_or(16.0),
        resolution.and_then(|r| field(r, "height")).filter(|&h| h > 0.0).unwrap_or(16.0),
    );
    b.project_uv = project_uv;
    let mut set = TextureSet::new(path, limits);
    for (i, t) in array(&root, "textures").iter().enumerate().take(4096) {
        let name = text(t, "name").unwrap_or("texture").to_string();
        let mut index = None;
        if let Some(data) = text(t, "source").and_then(|s| s.strip_prefix("data:")).and_then(|s| s.split_once(','))
            && data.0.ends_with(";base64")
        {
            match base64::engine::general_purpose::STANDARD.decode(data.1.trim()) {
                Ok(png) => index = set.embedded(&format!("{i}"), &png),
                Err(e) => b.warnings.push(format!("texture {name}: {e}")),
            }
        }
        if index.is_none() {
            // Linked textures: beside the project, by its relative path,
            // its absolute path or its bare name.
            let candidates = [text(t, "relative_path"), text(t, "path"), Some(name.as_str())];
            index = candidates.iter().flatten().filter(|c| !c.is_empty()).find_map(|c| set.file_quiet(c));
            if index.is_none() {
                set.missing(&name);
            }
        }
        let image = index.and_then(|i| set.get(i)).map(|t| (t.width() as f32, t.height() as f32));
        let uv_size = if b.rules.per_texture_uv {
            let w = field(t, "uv_width").filter(|&w| w > 0.0);
            let h = field(t, "uv_height").filter(|&h| h > 0.0);
            match (w, h) {
                (Some(w), Some(h)) => Vec2::new(w, h),
                // Files from before textures had their own UV size use the
                // project's.
                _ => project_uv,
            }
        } else {
            project_uv
        };
        // Frames stacked vertically: a texture taller than its UV space's
        // shape.
        let frames = image.map_or(1.0, |(w, h)| {
            let n = ((uv_size.x / uv_size.y) / (w / h.max(1.0)) - 0.05).ceil();
            if n.is_finite() && n > 1.0 { n.min(4096.0) } else { 1.0 }
        });
        let material = b.material(&name, index, &set, t);
        b.textures.push(Tex { material, uv_size, frames });
    }
    if format == "image" {
        if let Some(tex) = set.textures.first() {
            return Ok(Loaded::Picture((**tex).clone()));
        }
        return Err(LoadError::invalid("the image project has no picture"));
    }
    b.warnings.append(&mut set.warnings);
    b.scene.textures = std::mem::take(&mut set.textures);

    // 2. Elements and groups by uuid.  Old files list cubes as `cubes`;
    //    5.0 keeps groups apart from the outliner, which then names them
    //    by uuid only.
    let elements = root.get("elements").or_else(|| root.get("cubes")).and_then(Value::as_array);
    let mut by_uuid: HashMap<&str, &Value> = HashMap::new();
    for e in elements.map_or(&[][..], Vec::as_slice).iter().take(MAX_ITEMS) {
        if let Some(u) = text(e, "uuid") {
            by_uuid.insert(u, e);
        }
    }
    let mut groups: HashMap<&str, &Value> = HashMap::new();
    for g in array(&root, "groups").iter().take(MAX_ITEMS) {
        if let Some(u) = text(g, "uuid") {
            groups.insert(u, g);
        }
    }

    // 3. The tree.  Elements the outliner doesn't mention hang from the
    //    root, as Blockbench shows them.
    let root_node = b.add_node("root", None, Transform::IDENTITY, true);
    let mut placed: std::collections::HashSet<&str> = std::collections::HashSet::new();
    let ctx = Ctx { by_uuid: &by_uuid, groups: &groups };
    for item in array(&root, "outliner") {
        b.walk(item, root_node, Vec3::ZERO, &ctx, &mut placed, 0);
    }
    for e in elements.map_or(&[][..], Vec::as_slice).iter().take(MAX_ITEMS) {
        if text(e, "uuid").is_none_or(|u| !placed.contains(u)) {
            b.element(e, root_node, Vec3::ZERO);
        }
    }
    if format == "skin" && b.buffers.is_empty() {
        player(&mut b, text(&root, "skin_model").unwrap_or("steve"), root_node);
    }
    b.finish_meshes();

    // 4. Skins and animations.
    b.bind_armatures(&ctx);
    if limits.animations {
        for a in array(&root, "animations").iter().take(4096) {
            if let Some(clip) = b.animation(a) {
                b.scene.animations.push(clip);
            }
        }
    }
    b.scene.compute_roots();
    b.scene.name = text(&root, "name").unwrap_or("").to_string();
    let mut details = vec![("Blockbench format".to_string(), format.to_string())];
    if let Some(v) = text(meta, "format_version") {
        details.push(("File version".into(), v.to_string()));
    }
    details.push(("Elements".into(), elements.map_or(0, Vec::len).to_string()));
    b.scene.info = slerp_scene::Info {
        format: "Blockbench model".into(),
        generator: Some("Blockbench".into()),
        warnings: b.warnings,
        details,
        neighbours: Vec::new(),
    };
    Ok(Loaded::Scene(Box::new(b.scene)))
}

/// What a project holds.
pub enum Loaded {
    Scene(Box<Scene>),
    /// An image project: a picture and nothing else.
    Picture(slerp_scene::Texture),
}

/// Each mesh vertex's element uuid and vertex key.
type VertexKeys = Vec<(String, String)>;

/// A player-model part: name, pivot, box corner, box size, base layer UV
/// offset, outer layer UV offset, how far the outer layer stands out.
type PlayerPart = (&'static str, [f32; 3], [f32; 3], [f32; 3], [f32; 2], [f32; 2], f32);

/// Lookup tables for the outliner walk.
struct Ctx<'a> {
    by_uuid: &'a HashMap<&'a str, &'a Value>,
    groups: &'a HashMap<&'a str, &'a Value>,
}

//endregion

/*┌─────────────────╮
| Building        |
└─────────────────╯*/
//region Building

/// Geometry collected for one node and material, in the node's space.
#[derive(Default)]
struct Buffer {
    positions: Vec<Vec3>,
    normals: Vec<Vec3>,
    uvs: Vec<Vec2>,
    indices: Vec<u32>,
    /// Bone weights per vertex, for meshes bound to an armature: the
    /// mesh's uuid and vertex key, looked up once the bones are read.
    keys: Vec<(String, String)>,
}

pub(crate) struct Builder {
    pub(crate) scene: Scene,
    rules: FormatRules,
    textures: Vec<Tex>,
    default_material: Option<usize>,
    /// By node and material.
    buffers: HashMap<(usize, usize), Buffer>,
    /// Group (and bone) uuid to node, and the group's origin and rest
    /// rotation in degrees, for animations.
    group_nodes: HashMap<String, (usize, Vec3, Vec3)>,
    warnings: Vec<String>,
    items: usize,
    /// Mesh vertices' uuid and key, by node and primitive, for binding
    /// them to armature bones.
    vertex_keys: Vec<(usize, usize, VertexKeys)>,
    /// The UV size of faces without a texture: the project's resolution.
    project_uv: Vec2,
    /// Molang variables the project defines for previewing animations
    /// (`variable.x = ...`, `degrees=180`), by name.
    vars: Vec<(String, String)>,
}

impl Builder {
    pub(crate) fn add_node(&mut self, name: &str, parent: Option<usize>, transform: Transform, visible: bool) -> usize {
        let mut node = Node::new(name);
        node.transform = transform;
        node.visible = visible;
        self.scene.nodes.push(node);
        let i = self.scene.nodes.len() - 1;
        if let Some(p) = parent {
            self.scene.nodes[p].children.push(i);
        }
        i
    }

    /// A material for a texture: unlit-looking pixel art, hard-edged when
    /// magnified, cut out or blended where the picture is transparent.
    fn material(&mut self, name: &str, index: Option<usize>, set: &TextureSet, t: &Value) -> usize {
        let mut m = Material { name: name.to_string(), roughness: 1.0, double_sided: true, ..Material::default() };
        m.specular = 0.2;
        match index {
            Some(i) => {
                m.base_color = Vec4::ONE;
                let sampler = Sampler { mag: Filter::Nearest, ..Sampler::default() };
                m.base_color_texture = Some(TexRef { sampler, ..TexRef::new(i) });
                m.alpha_mode = set.get(i).map_or(AlphaMode::Opaque, alpha_mode);
                if text(t, "render_mode") == Some("emissive") {
                    m.emissive = Vec3::ONE;
                    m.emissive_texture = m.base_color_texture;
                }
            }
            None => m.base_color = Vec4::new(0.6, 0.6, 0.62, 1.0),
        }
        self.scene.materials.push(m);
        self.scene.materials.len() - 1
    }

    /// The grey material faces without a texture wear.
    fn plain(&mut self) -> usize {
        *self.default_material.get_or_insert_with(|| {
            self.scene.materials.push(Material {
                name: "untextured".into(),
                base_color: Vec4::new(0.6, 0.6, 0.62, 1.0),
                roughness: 1.0,
                double_sided: true,
                ..Material::default()
            });
            self.scene.materials.len() - 1
        })
    }

    /// The texture a face shows, by Blockbench's rules for the format:
    /// `None` means the face isn't drawn.
    fn face_texture(&mut self, face: &Value) -> Option<(usize, Vec2, f32)> {
        let given = face.get("texture");
        if matches!(given, Some(Value::Null)) {
            return None;
        }
        let index = given.and_then(num).map(|n| n as usize);
        let wanted =
            if self.rules.single_texture || (index.is_none() && self.rules.default_texture) { Some(0) } else { index };
        match wanted.and_then(|i| self.textures.get(i)) {
            Some(t) => Some((t.material, t.uv_size, t.frames)),
            None => Some((self.plain(), self.project_uv, 1.0)),
        }
    }

    /// Follows one outliner entry: a group (an object with children) or an
    /// element named by its uuid.
    fn walk<'a>(
        &mut self,
        item: &'a Value,
        parent: usize,
        parent_origin: Vec3,
        ctx: &Ctx<'a>,
        placed: &mut std::collections::HashSet<&'a str>,
        depth: usize,
    ) {
        self.items += 1;
        if depth > MAX_DEPTH || self.items > MAX_ITEMS {
            return;
        }
        match item {
            Value::String(uuid) => {
                if let Some(e) = ctx.by_uuid.get(uuid.as_str()) {
                    placed.insert(uuid.as_str());
                    match text(e, "type") {
                        // A bone named without children of its own.
                        Some("armature_bone" | "armature") => {
                            self.group(e, e, parent, parent_origin, ctx, placed, depth);
                        }
                        _ => self.element(e, parent, parent_origin),
                    }
                }
            }
            Value::Object(_) => {
                // The group's settings: in the outliner entry itself (files
                // before 5.0), in `groups`, or for armatures among the
                // elements.
                let uuid = text(item, "uuid").unwrap_or("");
                let data = ctx.groups.get(uuid).or_else(|| ctx.by_uuid.get(uuid)).copied().unwrap_or(item);
                if ctx.by_uuid.contains_key(uuid) {
                    placed.insert(uuid);
                }
                self.group(data, item, parent, parent_origin, ctx, placed, depth);
            }
            _ => {}
        }
    }

    /// Makes a node for a group or bone and walks its children.
    #[allow(clippy::too_many_arguments)]
    fn group<'a>(
        &mut self,
        data: &Value,
        entry: &'a Value,
        parent: usize,
        parent_origin: Vec3,
        ctx: &Ctx<'a>,
        placed: &mut std::collections::HashSet<&'a str>,
        depth: usize,
    ) {
        let origin = vec3(data, "origin").unwrap_or(Vec3::ZERO);
        let mut rotation = if self.rules.bone_rig { vec3(data, "rotation").unwrap_or(Vec3::ZERO) } else { Vec3::ZERO };
        if self.rules.old_group_z {
            rotation.z = -rotation.z;
        }
        let scale = vec3(data, "scale").filter(|s| s.cmpne(Vec3::ZERO).all()).unwrap_or(Vec3::ONE);
        let transform = Transform { translation: (origin - parent_origin) * UNIT, rotation: euler(rotation), scale };
        let visible = flag(data, "visibility").unwrap_or(true);
        let name = text(data, "name").unwrap_or("group");
        let node = self.add_node(name, Some(parent), transform, visible);
        if let Some(uuid) = text(data, "uuid").or_else(|| text(entry, "uuid")) {
            self.group_nodes.insert(uuid.to_string(), (node, origin, rotation));
            self.group_nodes.entry(name.to_string()).or_insert((node, origin, rotation));
        }
        for child in array(entry, "children") {
            self.walk(child, node, origin, ctx, placed, depth + 1);
        }
    }

    /// Adds an element's geometry to its group's buffers, in the group's
    /// space (`group_origin` in model pixels).
    pub(crate) fn element(&mut self, e: &Value, group: usize, group_origin: Vec3) {
        self.items += 1;
        if self.items > MAX_ITEMS || flag(e, "visibility") == Some(false) || flag(e, "export") == Some(false) {
            return;
        }
        match text(e, "type").unwrap_or("cube") {
            "cube" => self.cube(e, group, group_origin),
            "mesh" => self.mesh(e, group, group_origin),
            "armature_bone" | "armature" | "group" => {}
            other => {
                let note = format!("{other} elements aren't shown");
                if !self.warnings.contains(&note) && !matches!(other, "locator" | "null_object") {
                    self.warnings.push(note);
                }
            }
        }
    }

    /// The element's own turn about its origin, as a matrix in model
    /// pixels.  Old files wrote a cube's rotation as one angle about one
    /// axis.
    fn element_matrix(&self, e: &Value, origin: Vec3) -> Mat4 {
        let rotation = match e.get("rotation") {
            Some(Value::Object(_)) => {
                let r = e.get("rotation").unwrap();
                let angle = field(r, "angle").unwrap_or(0.0);
                match text(r, "axis") {
                    Some("x") => Vec3::new(angle, 0.0, 0.0),
                    Some("z") => Vec3::new(0.0, 0.0, angle),
                    _ => Vec3::new(0.0, angle, 0.0),
                }
            }
            _ => vec3(e, "rotation").unwrap_or(Vec3::ZERO),
        };
        // Java block models may stretch a turned cube back to its grid.
        let mut scale = Vec3::ONE;
        if flag(e, "rescale") == Some(true) {
            let r = |a: f32| {
                let a = a.abs() % 90.0;
                let a = if a > 45.0 { 90.0 - a } else { a };
                1.0 / a.to_radians().cos()
            };
            let (x, y, z) = (r(rotation.x), r(rotation.y), r(rotation.z));
            scale = Vec3::new(y * z, x * z, x * y);
        }
        Mat4::from_translation(origin) * Mat4::from_quat(euler(rotation)) * Mat4::from_scale(scale)
    }

    fn cube(&mut self, e: &Value, group: usize, group_origin: Vec3) {
        let (Some(mut from), Some(mut to)) = (vec3(e, "from"), vec3(e, "to")) else { return };
        let origin = vec3(e, "origin").unwrap_or(Vec3::ZERO);
        // 1. The box, grown by `inflate` and stretched about its centre.
        let size = to - from;
        let inflate = field(e, "inflate").unwrap_or(0.0);
        let stretch = vec3(e, "stretch").unwrap_or(Vec3::ONE);
        let centre = from + size / 2.0;
        from = centre - (size / 2.0 + Vec3::splat(inflate)) * stretch;
        to = centre + (size / 2.0 + Vec3::splat(inflate)) * stretch;
        for a in 0..3 {
            if from[a] == to[a] {
                to[a] += 0.001;
            }
        }
        let (f, t) = (from - origin, to - origin);
        let m = Mat4::from_translation(-group_origin) * self.element_matrix(e, origin);

        // 2. Box UV: every face's rectangle from one offset and the box's
        //    whole-pixel size, as Minecraft unfolds a box.
        let faces = e.get("faces");
        let box_uv = flag(e, "box_uv").unwrap_or(self.rules.box_uv);
        let mut box_rects: HashMap<&str, [f32; 4]> = HashMap::new();
        if box_uv {
            let offset = e.get("uv_offset").and_then(Value::as_array);
            let o = |i: usize| offset.and_then(|a| a.get(i)).and_then(num).unwrap_or(0.0);
            let s = size.map(|v| (v + 1e-7).floor());
            let (w, h, d) = (s.x, s.y, s.z);
            let mut list = [
                ("east", [0.0, d], [d, h]),
                ("west", [d + w, d], [d, h]),
                ("up", [d + w, d], [-w, -d]),
                ("down", [d + w * 2.0, 0.0], [-w, d]),
                ("south", [d * 2.0 + w, d], [w, h]),
                ("north", [d, d], [w, h]),
            ];
            if flag(e, "mirror_uv") == Some(true) {
                for f in &mut list {
                    f.1[0] += f.2[0];
                    f.2[0] = -f.2[0];
                }
                let (east, west) = (list[0], list[1]);
                list[0] = ("east", west.1, west.2);
                list[1] = ("west", east.1, east.2);
            }
            for (name, start, extent) in list {
                let mut r =
                    [start[0] + o(0), start[1] + o(1), start[0] + extent[0] + o(0), start[1] + extent[1] + o(1)];
                // Blockbench pulls box-UV edges in by 1/64 pixel so
                // neighbouring faces' texels don't bleed in.
                for k in 0..2 {
                    let margin = if r[k] > r[k + 2] { -1.0 / 64.0 } else { 1.0 / 64.0 };
                    r[k] += margin;
                    r[k + 2] -= margin;
                }
                box_rects.insert(name, r);
            }
        }

        // 3. The six sides, corners in Blockbench's order: the UV
        //    rectangle's (u1, v1), (u2, v1), (u1, v2), (u2, v2) go to
        //    these corners.
        let sides: [(&str, [Vec3; 4]); 6] = [
            ("east", [v(t.x, t.y, t.z), v(t.x, t.y, f.z), v(t.x, f.y, t.z), v(t.x, f.y, f.z)]),
            ("west", [v(f.x, t.y, f.z), v(f.x, t.y, t.z), v(f.x, f.y, f.z), v(f.x, f.y, t.z)]),
            ("up", [v(f.x, t.y, f.z), v(t.x, t.y, f.z), v(f.x, t.y, t.z), v(t.x, t.y, t.z)]),
            ("down", [v(f.x, f.y, t.z), v(t.x, f.y, t.z), v(f.x, f.y, f.z), v(t.x, f.y, f.z)]),
            ("south", [v(f.x, t.y, t.z), v(t.x, t.y, t.z), v(f.x, f.y, t.z), v(t.x, f.y, t.z)]),
            ("north", [v(t.x, t.y, f.z), v(f.x, t.y, f.z), v(t.x, f.y, f.z), v(f.x, f.y, f.z)]),
        ];
        for (name, corners) in sides {
            let Some(face) = faces.and_then(|f| f.get(name)) else {
                // Very old box-UV files list no faces at all.
                if box_uv && faces.is_none() {
                    let tex = self.face_texture(&Value::Object(Default::default()));
                    if let (Some(tex), Some(&r)) = (tex, box_rects.get(name)) {
                        self.quad(group, tex, corners, r, 0, m);
                    }
                }
                continue;
            };
            let Some(tex) = self.face_texture(face) else { continue };
            let rect = match box_rects.get(name) {
                Some(&r) => r,
                None => {
                    let uv = array(face, "uv");
                    if uv.len() < 4 {
                        continue;
                    }
                    [num(&uv[0]), num(&uv[1]), num(&uv[2]), num(&uv[3])].map(|x| x.unwrap_or(0.0))
                }
            };
            let turn = field(face, "rotation").unwrap_or(0.0);
            self.quad(group, tex, corners, rect, (turn / 90.0).round().rem_euclid(4.0) as u32, m);
        }
    }

    /// Adds one cube side: corners in Blockbench's order, its UV rectangle
    /// in the texture's UV units, turned `turns` quarter turns.
    fn quad(&mut self, group: usize, tex: (usize, Vec2, f32), corners: [Vec3; 4], r: [f32; 4], turns: u32, m: Mat4) {
        let (material, size, frames) = tex;
        let uv = |u: f32, v: f32| Vec2::new(u / size.x, v / (size.y * frames));
        let mut uvs = [uv(r[0], r[1]), uv(r[2], r[1]), uv(r[0], r[3]), uv(r[2], r[3])];
        for _ in 0..turns {
            uvs = [uvs[2], uvs[0], uvs[3], uvs[1]];
        }
        let buf = self.buffers.entry((group, material)).or_default();
        let base = buf.positions.len() as u32;
        for (c, t) in corners.iter().zip(uvs) {
            buf.positions.push(m.transform_point3(*c) * UNIT);
            buf.uvs.push(t);
        }
        buf.indices.extend([0, 2, 1, 2, 3, 1].map(|i| base + i));
    }

    /// A Blockbench 4 mesh: named vertices relative to its origin, faces of
    /// three or four of them, each with its own UV per corner.
    fn mesh(&mut self, e: &Value, group: usize, group_origin: Vec3) {
        let origin = vec3(e, "origin").unwrap_or(Vec3::ZERO);
        let m = Mat4::from_translation(-group_origin) * self.element_matrix(e, origin);
        let Some(vertices) = e.get("vertices").and_then(Value::as_object) else { return };
        let point = |k: &str| {
            vertices.get(k).and_then(|p| {
                let a = p.as_array()?;
                Some(Vec3::new(num(a.first()?)?, num(a.get(1)?)?, num(a.get(2)?)?))
            })
        };
        let smooth = text(e, "shading") == Some("smooth");
        let uuid = text(e, "uuid").unwrap_or("").to_string();
        let Some(faces) = e.get("faces").and_then(Value::as_object) else { return };
        // Smooth shading averages the faces around each vertex.
        let mut smooth_normals: HashMap<&str, Vec3> = HashMap::new();
        let mut ordered: Vec<(&Value, Vec<&str>)> = Vec::new();
        for face in faces.values().take(MAX_ITEMS) {
            let keys: Vec<&str> = array(face, "vertices").iter().filter_map(Value::as_str).collect();
            if keys.len() < 3 || keys.iter().any(|k| point(k).is_none()) {
                continue;
            }
            let sorted = sort_quad(&keys, &point);
            if smooth {
                let pts: Vec<Vec3> = sorted.iter().filter_map(|k| point(k)).collect();
                let n = common::newell_normal(&pts);
                for k in &sorted {
                    *smooth_normals.entry(k).or_default() += n;
                }
            }
            ordered.push((face, sorted));
        }
        let normal_matrix = m.inverse().transpose();
        for (face, sorted) in ordered {
            let Some(tex) = self.face_texture(face) else { continue };
            let (material, size, frames) = tex;
            let uv_of = |k: &str| {
                let a = face.get("uv").and_then(|u| u.get(k)).and_then(Value::as_array);
                let c = |i: usize| a.and_then(|a| a.get(i)).and_then(num).unwrap_or(0.0);
                Vec2::new(c(0) / size.x, c(1) / (size.y * frames))
            };
            let pts: Vec<Vec3> = sorted.iter().filter_map(|k| point(k)).collect();
            let flat = common::newell_normal(&pts);
            let buf = self.buffers.entry((group, material)).or_default();
            let base = buf.positions.len() as u32;
            for (k, p) in sorted.iter().zip(&pts) {
                buf.positions.push(m.transform_point3(*p) * UNIT);
                buf.uvs.push(uv_of(k));
                let n = if smooth { smooth_normals.get(k).copied().unwrap_or(flat) } else { flat };
                buf.normals.push(normal_matrix.transform_vector3(n).normalize_or_zero());
                buf.keys.push((uuid.clone(), k.to_string()));
            }
            // A fan, which for quads is Blockbench's (0, 1, 2), (0, 2, 3).
            for i in 1..pts.len() as u32 - 1 {
                buf.indices.extend([base, base + i, base + i + 1]);
            }
        }
    }

    /// Turns the buffers into meshes on their nodes.
    fn finish_meshes(&mut self) {
        let mut keys: Vec<(usize, usize)> = self.buffers.keys().copied().collect();
        keys.sort();
        let mut by_node: HashMap<usize, Mesh> = HashMap::new();
        for key in keys {
            let buf = self.buffers.remove(&key).unwrap_or_default();
            if buf.indices.is_empty() {
                continue;
            }
            // Cubes leave normals to be computed; meshes bring their own.
            let normals = if buf.normals.len() == buf.positions.len() { buf.normals } else { Vec::new() };
            let has_keys = buf.keys.len() == buf.positions.len();
            let mut p = Primitive {
                positions: buf.positions,
                normals,
                uv0: buf.uvs,
                indices: buf.indices,
                material: Some(key.1),
                ..Primitive::default()
            };
            p.ensure_normals();
            let mesh = by_node
                .entry(key.0)
                .or_insert_with(|| Mesh { name: self.scene.nodes[key.0].name.clone(), ..Mesh::default() });
            mesh.primitives.push(p);
            if has_keys {
                self.vertex_keys.push((key.0, mesh.primitives.len() - 1, buf.keys));
            }
        }
        let mut nodes: Vec<usize> = by_node.keys().copied().collect();
        nodes.sort();
        for n in nodes {
            let mesh = by_node.remove(&n).unwrap_or_default();
            self.scene.meshes.push(mesh);
            self.scene.nodes[n].mesh = Some(self.scene.meshes.len() - 1);
        }
    }
}

fn v(x: f32, y: f32, z: f32) -> Vec3 {
    Vec3::new(x, y, z)
}

/// How a texture's alpha should be drawn: opaque, cut out (only fully
/// transparent or opaque texels, as most pixel art) or blended.
fn alpha_mode(tex: &slerp_scene::Texture) -> AlphaMode {
    let Some((_, _, px)) = common::rgba8(tex) else { return AlphaMode::Opaque };
    let (mut clear, mut partial) = (false, false);
    for a in px.chunks_exact(4).map(|p| p[3]) {
        match a {
            0..=5 => clear = true,
            250..=255 => {}
            _ => partial = true,
        }
    }
    if partial {
        AlphaMode::Blend
    } else if clear {
        AlphaMode::Mask(0.5)
    } else {
        AlphaMode::Opaque
    }
}

/// Puts a four-sided face's corners in order around it, as Blockbench
/// does: the file lists them in any order.  Triangles are left alone.
fn sort_quad<'a>(keys: &[&'a str], point: &dyn Fn(&str) -> Option<Vec3>) -> Vec<&'a str> {
    if keys.len() != 4 {
        return keys.to_vec();
    }
    let p: Vec<Vec3> = keys.iter().filter_map(|k| point(k)).collect();
    // Whether `check` lies on the other side of the line through `a` and
    // `b` from `top`, within the face's plane.
    let across = |a: Vec3, b: Vec3, top: Vec3, check: Vec3| {
        let dir = (b - a).normalize_or_zero();
        let foot = a + dir * (top - a).dot(dir);
        let normal = foot - top;
        normal.dot(check - b) > 0.0
    };
    if across(p[1], p[2], p[0], p[3]) {
        vec![keys[2], keys[0], keys[1], keys[3]]
    } else if across(p[0], p[1], p[2], p[3]) {
        vec![keys[0], keys[2], keys[1], keys[3]]
    } else {
        keys.to_vec()
    }
}

//endregion

/*┌─────────────────╮
| Armatures       |
└─────────────────╯*/
//region Armatures

impl Builder {
    /// Binds meshes to Blockbench 5 armature bones by the vertex weights
    /// the bones list, keyed by the first six characters of the mesh's
    /// uuid and the vertex's key.
    fn bind_armatures(&mut self, ctx: &Ctx) {
        // 1. Every bone's weights.
        let mut weights: HashMap<&str, Vec<(usize, f32)>> = HashMap::new();
        for (uuid, e) in ctx.by_uuid.iter() {
            if text(e, "type") != Some("armature_bone") {
                continue;
            }
            let Some(&(node, _, _)) = self.group_nodes.get(*uuid) else { continue };
            let Some(list) = e.get("vertex_weights").and_then(Value::as_object) else { continue };
            for (key, w) in list.iter().take(MAX_ITEMS) {
                let Some(w) = num(w) else { continue };
                if w > 0.0 && key.contains(':') {
                    weights.entry(key.as_str()).or_default().push((node, w));
                }
            }
        }
        if weights.is_empty() {
            return;
        }
        // 2. Rest matrices of every node.
        let parents = self.scene.parents();
        let mut world: Vec<Option<Mat4>> = vec![None; self.scene.nodes.len()];
        fn world_of(
            i: usize,
            nodes: &[Node],
            parents: &[Option<usize>],
            world: &mut [Option<Mat4>],
            depth: usize,
        ) -> Mat4 {
            if let Some(m) = world[i] {
                return m;
            }
            let local = nodes[i].transform.matrix();
            let m = match parents[i] {
                Some(p) if depth < MAX_DEPTH => world_of(p, nodes, parents, world, depth + 1) * local,
                _ => local,
            };
            world[i] = Some(m);
            m
        }
        // 3. Each node holding weighted vertices gets a skin over every
        //    bone it uses plus itself, for vertices no bone holds.
        let keyed = std::mem::take(&mut self.vertex_keys);
        let mut by_node: HashMap<usize, Vec<(usize, VertexKeys)>> = HashMap::new();
        for (node, prim, keys) in keyed {
            by_node.entry(node).or_default().push((prim, keys));
        }
        let mut nodes: Vec<usize> = by_node.keys().copied().collect();
        nodes.sort();
        for node in nodes {
            let prims = by_node.remove(&node).unwrap_or_default();
            let lookup =
                |uuid: &str, key: &str| weights.get(format!("{}:{key}", uuid.get(..6).unwrap_or(uuid)).as_str());
            if !prims.iter().any(|(_, keys)| keys.iter().any(|(u, k)| lookup(u, k).is_some())) {
                continue;
            }
            let Some(mesh) = self.scene.nodes[node].mesh else { continue };
            let mut joints: Vec<usize> = vec![node];
            let slot = |n: usize, joints: &mut Vec<usize>| match joints.iter().position(|&j| j == n) {
                Some(i) => i,
                None => {
                    joints.push(n);
                    joints.len() - 1
                }
            };
            for (pi, keys) in &prims {
                let Some(p) = self.scene.meshes[mesh].primitives.get_mut(*pi) else { continue };
                let mut js = Vec::with_capacity(keys.len());
                let mut ws = Vec::with_capacity(keys.len());
                for (u, k) in keys {
                    let mut list: Vec<(usize, f32)> = lookup(u, k).cloned().unwrap_or_default();
                    list.sort_by(|a, b| b.1.total_cmp(&a.1));
                    list.truncate(4);
                    let total: f32 = list.iter().map(|x| x.1).sum();
                    // What the bones don't hold stays with the mesh.
                    if total < 1.0 && list.len() < 4 {
                        list.push((node, 1.0 - total));
                    }
                    let total: f32 = list.iter().map(|x| x.1).sum::<f32>().max(1e-6);
                    let mut j = [0u16; 4];
                    let mut w = Vec4::ZERO;
                    for (i, &(n, wt)) in list.iter().enumerate() {
                        j[i] = slot(n, &mut joints).min(u16::MAX as usize) as u16;
                        w[i] = wt / total;
                    }
                    js.push(j);
                    ws.push(w);
                }
                p.joints = js;
                p.weights = ws;
            }
            // Primitives of the node without vertex keys (cubes) follow
            // the node itself.
            for p in &mut self.scene.meshes[mesh].primitives {
                if p.joints.is_empty() {
                    p.joints = vec![[0; 4]; p.positions.len()];
                    p.weights = vec![Vec4::X; p.positions.len()];
                }
            }
            let mesh_world = world_of(node, &self.scene.nodes, &parents, &mut world, 0);
            let inverse_bind = joints
                .iter()
                .map(|&j| world_of(j, &self.scene.nodes, &parents, &mut world, 0).inverse() * mesh_world)
                .collect();
            self.scene.skins.push(Skin {
                name: self.scene.nodes[node].name.clone(),
                joints,
                inverse_bind,
                skeleton: None,
            });
            self.scene.nodes[node].skin = Some(self.scene.skins.len() - 1);
        }
    }
}

//endregion

/*┌─────────────────╮
| Animations      |
└─────────────────╯*/
//region Animations

#[derive(Clone, Copy, PartialEq, Eq)]
enum Lerp {
    Linear,
    Step,
    CatmullRom,
    Bezier,
}

/// One keyframe of one channel: up to two values per axis (before and
/// after the key, for jumps), each a number or a Molang expression.
struct Key<'a> {
    time: f32,
    lerp: Lerp,
    points: Vec<[&'a Value; 3]>,
    /// Bézier handles per axis: times and values, left then right.
    handles: [[f32; 3]; 4],
}

static ZERO: Value = Value::Null;

/// A data point's x, y and z values.
fn axes(p: &Value) -> [&Value; 3] {
    ["x", "y", "z"].map(|ax| p.get(ax).unwrap_or(&ZERO))
}

impl Builder {
    /// Bakes one of the project's animations into linear keys.
    fn animation(&mut self, a: &Value) -> Option<Animation> {
        let name = text(a, "name").unwrap_or("animation").to_string();
        let animators = a.get("animators")?.as_object()?;
        let looping = text(a, "loop") == Some("loop");
        let mut longest = 0.0f32;
        let mut parsed: Vec<(usize, [Vec<Key>; 3])> = Vec::new();
        for (key, an) in animators.iter().take(MAX_ITEMS) {
            // Old files keyed animators by bone name and gave the
            // keyframes directly.
            let kfs: &[Value] = match an {
                Value::Array(list) => list,
                _ => array(an, "keyframes"),
            };
            if !matches!(text(an, "type").unwrap_or("bone"), "bone" | "armature_bone") {
                continue;
            }
            let found =
                self.group_nodes.get(key.as_str()).or_else(|| text(an, "name").and_then(|n| self.group_nodes.get(n)));
            let Some(&(node, _, _)) = found else { continue };
            let mut channels: [Vec<Key>; 3] = [Vec::new(), Vec::new(), Vec::new()];
            for kf in kfs.iter().take(MAX_ITEMS) {
                let c = match text(kf, "channel") {
                    Some("rotation") => 0,
                    Some("position") => 1,
                    Some("scale") => 2,
                    _ => continue,
                };
                let Some(time) = field(kf, "time").filter(|t| *t >= 0.0 && *t < 1e5) else { continue };
                let lerp = match text(kf, "interpolation") {
                    Some("step") => Lerp::Step,
                    Some("catmullrom") => Lerp::CatmullRom,
                    Some("bezier") => Lerp::Bezier,
                    _ => Lerp::Linear,
                };
                let dps = array(kf, "data_points");
                let points: Vec<[&Value; 3]> =
                    if dps.is_empty() { vec![axes(kf)] } else { dps.iter().take(2).map(axes).collect() };
                let handle = |k: &str| {
                    let h = array(kf, k);
                    [0, 1, 2].map(|i| h.get(i).and_then(num).unwrap_or(0.0))
                };
                let handles = [
                    handle("bezier_left_time"),
                    handle("bezier_left_value"),
                    handle("bezier_right_time"),
                    handle("bezier_right_value"),
                ];
                longest = longest.max(time);
                channels[c].push(Key { time, lerp, points, handles });
            }
            for c in &mut channels {
                c.sort_by(|a, b| a.time.total_cmp(&b.time));
            }
            if channels.iter().any(|c| !c.is_empty()) {
                parsed.push((node, channels));
            }
        }
        if parsed.is_empty() {
            return None;
        }
        let mut length = field(a, "length").filter(|l| *l > 0.0 && *l < 1e5).unwrap_or(longest);
        // Bedrock animations often have no length and move only through
        // expressions of the time; two seconds of them show the motion.
        let driven = parsed
            .iter()
            .flat_map(|(_, c)| c.iter().flatten())
            .any(|k| k.points.iter().flatten().any(|v| v.as_str().is_some_and(|s| uses_time(s, &self.vars))));
        if length <= 0.0 && driven {
            length = 2.0;
        }
        // 1. When to sample: every key, and a steady rate between them, so
        //    curves and Euler-angle blends survive as linear keys.
        let steps = ((length * BAKE_RATE).ceil() as usize).min(MAX_SAMPLES / parsed.len().max(1) / 3);
        let mut clip = Animation { name, duration: length, channels: Vec::new() };
        let mut molang_failed = false;
        for (node, channels) in parsed {
            let rest = self.scene.nodes[node].transform;
            let rest_rotation = self.group_nodes.values().find(|g| g.0 == node).map_or(Vec3::ZERO, |g| g.2);
            for (c, keys) in channels.iter().enumerate() {
                if keys.is_empty() {
                    continue;
                }
                let mut times: Vec<f32> = (0..=steps).map(|i| i as f32 * length / steps.max(1) as f32).collect();
                times.extend(keys.iter().map(|k| k.time).filter(|&t| t <= length));
                times.sort_by(f32::total_cmp);
                times.dedup_by(|a, b| (*a - *b).abs() < 1e-4);
                let mut values = Vec::with_capacity(times.len() * 4);
                for &t in &times {
                    let mut axis = [0.0f32; 3];
                    for (i, value) in axis.iter_mut().enumerate() {
                        *value = sample(keys, i, t, looping, c == 2, &self.vars, &mut molang_failed);
                    }
                    let mut d = Vec3::from(axis);
                    if self.rules.old_animation_signs && c < 2 {
                        d.x = -d.x;
                        if c == 0 {
                            d.y = -d.y;
                        }
                    }
                    match c {
                        0 => values.extend(euler(rest_rotation + d).to_array()),
                        1 => values.extend((rest.translation + d * UNIT).to_array()),
                        _ => values.extend((rest.scale * d).to_array()),
                    }
                }
                clip.channels.push(Channel {
                    node,
                    property: [Property::Rotation, Property::Translation, Property::Scale][c],
                    interpolation: Interpolation::Linear,
                    times,
                    values,
                });
            }
        }
        if molang_failed {
            let note = "some animation values use Molang beyond arithmetic and are taken as 0".to_string();
            if !self.warnings.contains(&note) {
                self.warnings.push(note);
            }
        }
        Some(clip)
    }
}

/// The value of one axis of a channel at time `t`, interpolated the way
/// Blockbench's timeline does.
fn sample(keys: &[Key], axis: usize, t: f32, looping: bool, scale: bool, vars: &Vars, failed: &mut bool) -> f32 {
    const EPSILON: f32 = 1.0 / 1200.0;
    let default = if scale { 1.0 } else { 0.0 };
    let mut eval = |k: &Key, point: usize| -> f32 {
        let p = k.points.get(point.min(k.points.len() - 1)).map_or(&ZERO, |p| p[axis]);
        match p {
            Value::Number(_) => num(p).unwrap_or(default),
            Value::String(s) if s.trim().is_empty() => default,
            Value::String(s) => molang(s, t, vars).unwrap_or_else(|| {
                *failed = true;
                0.0
            }),
            _ => default,
        }
    };
    let last = |k: &Key| k.points.len().saturating_sub(1);
    // 1. The keys on either side.
    let b = keys.iter().rposition(|k| k.time < t);
    let a = keys.iter().position(|k| k.time >= t);
    match (b, a) {
        (Some(i), _) if (keys[i].time - t).abs() < EPSILON => eval(&keys[i], 0),
        (_, Some(j)) if (keys[j].time - t).abs() < EPSILON => eval(&keys[j], 0),
        (Some(i), _) if keys[i].lerp == Lerp::Step => eval(&keys[i], last(&keys[i])),
        (Some(i), None) => eval(&keys[i], last(&keys[i])),
        (None, Some(j)) => eval(&keys[j], 0),
        (None, None) => default,
        (Some(i), Some(j)) => {
            let (before, after) = (&keys[i], &keys[j]);
            let span = (after.time - before.time).max(1e-6);
            let alpha = ((t - before.time) / span).clamp(0.0, 1.0);
            let v1 = eval(before, last(before));
            let v2 = eval(after, 0);
            let either = |l: Lerp| before.lerp == l || after.lerp == l;
            if before.lerp == Lerp::Linear && matches!(after.lerp, Lerp::Linear | Lerp::Step) {
                v1 + (v2 - v1) * alpha
            } else if either(Lerp::CatmullRom) {
                // 2. Catmull-Rom through the keys around, wrapping round a
                //    looping clip.
                let mut before_plus = i.checked_sub(1).map(|k| &keys[k]);
                let mut after_plus = keys.get(j + 1);
                if looping && keys.len() >= 3 {
                    before_plus = before_plus.or(keys.get(keys.len() - 2));
                    after_plus = after_plus.or(keys.get(1));
                }
                let v0 = match before_plus {
                    Some(k) if before.points.len() == 1 => eval(k, last(k)),
                    _ => v1,
                };
                let v3 = match after_plus {
                    Some(k) if after.points.len() == 1 => eval(k, 0),
                    _ => v2,
                };
                catmull_rom(alpha, v0, v1, v2, v3)
            } else {
                // 3. Bézier, with handles in seconds and value offsets.
                let h_out = before.handles[2][axis].clamp(0.0, span);
                let h_in = after.handles[0][axis].clamp(-span, 0.0);
                let p = [
                    Vec2::new(before.time, v1),
                    Vec2::new(before.time + h_out, v1 + before.handles[3][axis]),
                    Vec2::new(after.time + h_in, v2 + after.handles[1][axis]),
                    Vec2::new(after.time, v2),
                ];
                bezier_at(p, t)
            }
        }
    }
}

/// Uniform Catmull-Rom between `p1` and `p2`.
fn catmull_rom(t: f32, p0: f32, p1: f32, p2: f32, p3: f32) -> f32 {
    let (v0, v1) = ((p2 - p0) * 0.5, (p3 - p1) * 0.5);
    let (t2, t3) = (t * t, t * t * t);
    (2.0 * p1 - 2.0 * p2 + v0 + v1) * t3 + (-3.0 * p1 + 3.0 * p2 - 2.0 * v0 - v1) * t2 + v0 * t + p1
}

/// The value of a cubic Bézier curve in (time, value) at time `x`: its
/// parameter found by bisection, since time grows along it.
fn bezier_at(p: [Vec2; 4], x: f32) -> f32 {
    let at = |s: f32| {
        let r = 1.0 - s;
        p[0] * (r * r * r) + p[1] * (3.0 * r * r * s) + p[2] * (3.0 * r * s * s) + p[3] * (s * s * s)
    };
    let (mut lo, mut hi) = (0.0f32, 1.0f32);
    for _ in 0..40 {
        let mid = (lo + hi) / 2.0;
        if at(mid).x < x {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    at((lo + hi) / 2.0).y
}

/// Evaluates the arithmetic part of a Molang expression: numbers, `+ - *
/// /`, parentheses, `math.` functions (angles in degrees, as Molang has
/// them) and the animation time (`query.anim_time`, `q.life_time`).
/// Anything else, such as variables or conditions, gives `None`.
pub(crate) fn molang(s: &str, t: f32, vars: &Vars) -> Option<f32> {
    molang_at(s, t, vars, 0)
}

fn molang_at(s: &str, t: f32, vars: &Vars, depth: u32) -> Option<f32> {
    let mut p = Molang { s: s.as_bytes(), i: 0, t, depth, vars };
    let v = p.expr()?;
    p.skip();
    // A trailing semicolon ends a statement.
    if p.s.get(p.i) == Some(&b';') {
        p.i += 1;
        p.skip();
    }
    (p.i == p.s.len() && v.is_finite()).then_some(v)
}

/// Variables by name, each an expression.
pub(crate) type Vars = Vec<(String, String)>;

struct Molang<'a> {
    s: &'a [u8],
    i: usize,
    t: f32,
    depth: u32,
    vars: &'a Vars,
}

impl Molang<'_> {
    fn skip(&mut self) {
        while self.s.get(self.i).is_some_and(|c| c.is_ascii_whitespace()) {
            self.i += 1;
        }
    }

    fn eat(&mut self, c: u8) -> bool {
        self.skip();
        if self.s.get(self.i) == Some(&c) {
            self.i += 1;
            true
        } else {
            false
        }
    }

    fn expr(&mut self) -> Option<f32> {
        self.depth += 1;
        if self.depth > 64 {
            return None;
        }
        let mut v = self.term()?;
        loop {
            if self.eat(b'+') {
                v += self.term()?;
            } else if self.eat(b'-') {
                v -= self.term()?;
            } else {
                break;
            }
        }
        self.depth -= 1;
        Some(v)
    }

    fn term(&mut self) -> Option<f32> {
        let mut v = self.unary()?;
        loop {
            if self.eat(b'*') {
                v *= self.unary()?;
            } else if self.eat(b'/') {
                let d = self.unary()?;
                v = if d == 0.0 { 0.0 } else { v / d };
            } else {
                break;
            }
        }
        Some(v)
    }

    fn unary(&mut self) -> Option<f32> {
        if self.eat(b'-') {
            self.depth += 1;
            if self.depth > 64 {
                return None;
            }
            let v = self.unary();
            self.depth -= 1;
            return v.map(|v| -v);
        }
        if self.eat(b'+') {
            return self.unary();
        }
        self.primary()
    }

    fn primary(&mut self) -> Option<f32> {
        self.skip();
        if self.eat(b'(') {
            let v = self.expr()?;
            return self.eat(b')').then_some(v);
        }
        let start = self.i;
        let c = *self.s.get(self.i)?;
        if c.is_ascii_digit() || c == b'.' {
            while self.s.get(self.i).is_some_and(|c| c.is_ascii_digit() || *c == b'.') {
                self.i += 1;
            }
            if self.s.get(self.i).is_some_and(|c| *c == b'e' || *c == b'E') {
                self.i += 1;
                if self.s.get(self.i).is_some_and(|c| *c == b'-' || *c == b'+') {
                    self.i += 1;
                }
                while self.s.get(self.i).is_some_and(u8::is_ascii_digit) {
                    self.i += 1;
                }
            }
            return std::str::from_utf8(&self.s[start..self.i]).ok()?.parse().ok();
        }
        if !c.is_ascii_alphabetic() {
            return None;
        }
        while self.s.get(self.i).is_some_and(|c| c.is_ascii_alphanumeric() || *c == b'_' || *c == b'.') {
            self.i += 1;
        }
        let name = canonical(std::str::from_utf8(&self.s[start..self.i]).ok()?);
        let mut args = Vec::new();
        if self.eat(b'(') && !self.eat(b')') {
            loop {
                args.push(self.expr()?);
                if self.eat(b')') {
                    break;
                }
                if !self.eat(b',') || args.len() > 8 {
                    return None;
                }
            }
        }
        let a = |i: usize| args.get(i).copied().unwrap_or(0.0);
        let rad = |d: f32| d.to_radians();
        Some(match name.as_str() {
            "query.anim_time" | "q.anim_time" | "query.life_time" | "q.life_time" => self.t,
            "math.pi" => std::f32::consts::PI,
            "math.sin" => rad(a(0)).sin(),
            "math.cos" => rad(a(0)).cos(),
            "math.abs" => a(0).abs(),
            "math.sqrt" => a(0).max(0.0).sqrt(),
            "math.floor" => a(0).floor(),
            "math.ceil" => a(0).ceil(),
            "math.round" => a(0).round(),
            "math.trunc" => a(0).trunc(),
            "math.min" => a(0).min(a(1)),
            "math.max" => a(0).max(a(1)),
            "math.clamp" => a(0).clamp(a(1).min(a(2)), a(2).max(a(1))),
            "math.pow" => a(0).powf(a(1)),
            "math.mod" => {
                if a(1) == 0.0 {
                    0.0
                } else {
                    a(0) % a(1)
                }
            }
            "math.lerp" => a(0) + (a(1) - a(0)) * a(2),
            "math.exp" => a(0).exp(),
            "math.ln" => a(0).ln(),
            "math.atan" => a(0).atan().to_degrees(),
            "math.atan2" => a(0).atan2(a(1)).to_degrees(),
            "math.asin" => a(0).clamp(-1.0, 1.0).asin().to_degrees(),
            "math.acos" => a(0).clamp(-1.0, 1.0).acos().to_degrees(),
            // The value being animated, before this key: taken as rest.
            "this" => 0.0,
            _ => {
                let (_, expr) = self.vars.iter().find(|(n, _)| *n == name)?;
                if self.depth > 48 {
                    return None;
                }
                // Variables Blockbench shows as sliders and toggles start
                // at 0.
                molang_at(expr, self.t, self.vars, self.depth + 8).unwrap_or(0.0)
            }
        })
    }
}

//endregion

/// Molang's short prefixes spelled out, and lower case, so `v.x`,
/// `Variable.x` and `variable.x` are one name.
fn canonical(name: &str) -> String {
    let lower = name.to_ascii_lowercase();
    for (short, long) in [("v.", "variable."), ("q.", "query."), ("t.", "temp."), ("c.", "context.")] {
        if let Some(rest) = lower.strip_prefix(short) {
            return format!("{long}{rest}");
        }
    }
    lower
}

/// The variables a project defines for previews, one `name = expression`
/// per line or statement.
fn placeholders(root: &Value) -> Vars {
    let mut vars = Vars::new();
    for key in ["animation_variable_placeholders", "variable_placeholders"] {
        for statement in text(root, key).unwrap_or("").split(['\n', ';']).take(1024) {
            if let Some((name, expr)) = statement.split_once('=') {
                let name = canonical(name.trim());
                if !name.is_empty() && !vars.iter().any(|(n, _)| *n == name) {
                    vars.push((name, expr.trim().to_string()));
                }
            }
        }
    }
    vars
}

/// Whether an expression depends on the time, directly or through
/// variables.
fn uses_time(s: &str, vars: &Vars) -> bool {
    let lower = s.to_ascii_lowercase();
    lower.contains("anim_time")
        || lower.contains("life_time")
        || vars.iter().any(|(n, e)| {
            let short = n.strip_prefix("variable.").map(|r| format!("v.{r}"));
            (lower.contains(n.as_str()) || short.is_some_and(|s| lower.contains(&s)))
                && (e.contains("anim_time") || e.contains("life_time"))
        })
}

/*┌─────────────────╮
| Player skins    |
└─────────────────╯*/
//region Player skins

/// Builds Minecraft's player model for a skin project, which stores only
/// the skin picture and which model wears it: "steve" (four-pixel arms)
/// or "alex" (three-pixel arms).  The boxes are Minecraft's standard skin
/// layout: each part's box UV offset for the base layer and the outer
/// layer, which sits a little outside it.
fn player(b: &mut Builder, model: &str, root: usize) {
    let slim = model.starts_with("alex");
    let arm = if slim { 3.0 } else { 4.0 };
    let parts: [PlayerPart; 6] = [
        ("Head", [0.0, 24.0, 0.0], [-4.0, 24.0, -4.0], [8.0, 8.0, 8.0], [0.0, 0.0], [32.0, 0.0], 0.5),
        ("Body", [0.0, 24.0, 0.0], [-4.0, 12.0, -2.0], [8.0, 12.0, 4.0], [16.0, 16.0], [16.0, 32.0], 0.25),
        ("Right Arm", [-5.0, 22.0, 0.0], [-4.0 - arm, 12.0, -2.0], [arm, 12.0, 4.0], [40.0, 16.0], [40.0, 32.0], 0.25),
        ("Left Arm", [5.0, 22.0, 0.0], [4.0, 12.0, -2.0], [arm, 12.0, 4.0], [32.0, 48.0], [48.0, 48.0], 0.25),
        ("Right Leg", [-1.9, 12.0, 0.0], [-3.9, 0.0, -2.0], [4.0, 12.0, 4.0], [0.0, 16.0], [0.0, 32.0], 0.25),
        ("Left Leg", [1.9, 12.0, 0.0], [-0.1, 0.0, -2.0], [4.0, 12.0, 4.0], [16.0, 48.0], [0.0, 48.0], 0.25),
    ];
    for (name, pivot, from, size, base, outer, grow) in parts {
        let pivot = Vec3::from(pivot);
        let t = Transform { translation: pivot * UNIT, ..Transform::IDENTITY };
        let node = b.add_node(name, Some(root), t, true);
        b.group_nodes.insert(name.to_string(), (node, pivot, Vec3::ZERO));
        for (offset, inflate) in [(base, 0.0), (outer, grow)] {
            let to = [from[0] + size[0], from[1] + size[1], from[2] + size[2]];
            let cube = serde_json::json!({
                "from": from, "to": to, "origin": pivot.to_array(), "uv_offset": offset,
                "inflate": inflate, "box_uv": true,
            });
            b.cube(&cube, node, pivot);
        }
    }
}

//endregion

#[cfg(test)]
#[path = "bbmodel_tests.rs"]
mod tests;
