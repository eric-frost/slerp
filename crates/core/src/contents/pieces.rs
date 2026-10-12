//! The pieces of files that aren't archives, listed from what their loader
//! produced: a scene's nodes, collections, meshes, materials, textures,
//! clips, skeletons, blend shapes and expressions; a picture's mip levels, array layers and
//! cube faces; a layered or animated picture's layers, frames and tags; a
//! gallery's pictures; a material file's textures and its ball; the parts
//! of a file shown one at a time (a PDF's pages, an EXR's layers, an LDtk
//! project's levels); the member files of a zip-based format (a 3MF's plate
//! pictures, a USDZ's textures); and a MetaHuman's levels of detail and rig
//! controls.
//!
//! Listing only walks what is already in memory, so it costs next to
//! nothing beside the load itself.  What a piece is made of when it is
//! saved is `save`'s business.

use super::{Contents, ContentsError, Format, Item, ItemId, ItemKind, Result, Source};
use slerp_scene::{Asset, Canvas, Limits, MaterialAsset, Scene, Texture};
use slerp_textures::{Pixels, TextureKind};
use std::path::Path;
use std::sync::{Arc, OnceLock};

/// The most nodes listed one by one; a scene with more (a voxel world, a
/// CAD assembly) lists the rest as a warning.
const MAX_NODES: usize = 50_000;
/// The most items of any other one kind.
const MAX_PER_KIND: usize = 20_000;
/// Collections deeper inside others than this are left out: real files nest
/// a few levels, and a damaged one could nest without end.
const MAX_COLLECTION_DEPTH: usize = 64;

/*┌─────────────────╮
| Model           |
└─────────────────╯*/
//region Model

/// A piece of what a file's loader produced.  Indices are into the loaded
/// scene (or picture, canvas, gallery, material) the listing was made from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Piece {
    /// The whole scene.
    Scene,
    /// A node, with everything beneath it.
    Node(usize),
    /// A collection (`Scene::collections`): its nodes and those of the
    /// collections inside it, each with everything beneath it.
    Collection(usize),
    Mesh(usize),
    Material(usize),
    /// One of the scene's textures, or of a material file's.
    Texture(usize),
    Animation(usize),
    /// A skin's joints.
    Skeleton(usize),
    /// One morph target of a mesh.
    BlendShape {
        mesh: usize,
        target: usize,
    },
    Expression(usize),
    /// One surface of the file's picture: a mip level of an array layer's
    /// cube face.
    Surface {
        level: u32,
        layer: u32,
        face: u32,
    },
    /// An array layer of the file's cube map as one equirectangular
    /// panorama.
    Panorama {
        layer: u32,
    },
    /// A layer of a layered picture (with what is inside it, for a group).
    Layer(usize),
    /// A frame of an animated picture, with the layers the file shows.
    Frame(usize),
    /// A tag's run of frames, saved as a sprite sheet.
    Tag(usize),
    /// A picture of a gallery.
    Picture(usize),
    /// A material file's material drawn on a ball.
    MaterialBall,
    /// A material file's own preview picture.
    MaterialPreview,
    /// The picture the file's authoring program saved of it (Blender's
    /// preview, a 3MF's thumbnail, a VRM portrait, a Photoshop composite).
    SavedPicture,
    /// A part of the file shown one at a time (`crate::parts`).
    Part(String),
    /// A MetaHuman's character at one level of detail.
    Detail(u16),
    /// Something that only means something inside its file (a camera, a
    /// light, a rig's control): listed, never saved.
    Listed,
}

impl Piece {
    /// Whether saving it needs the scene.
    pub fn needs_scene(&self) -> bool {
        matches!(
            self,
            Piece::Scene
                | Piece::Node(_)
                | Piece::Collection(_)
                | Piece::Mesh(_)
                | Piece::Material(_)
                | Piece::Animation(_)
                | Piece::Skeleton(_)
        )
    }
}

/// What showing an item means, for the viewer.
#[derive(Clone, Debug, PartialEq)]
pub enum Show {
    /// Prepare a preview of its own ([`Contents::preview`]) and show that,
    /// as for an archive's entries and a scene's textures.
    Preview,
    /// The file as it opened: everything visible, the first view.
    Whole,
    /// These nodes of the loaded scene alone, framed; with everything
    /// beneath them when `subtree`.
    Nodes { nodes: Vec<usize>, subtree: bool },
    /// Play this clip of the loaded scene.
    Clip(usize),
    /// Show a mesh alone with one of its morph targets at full weight.
    BlendShape { nodes: Vec<usize>, mesh: usize, target: usize },
    /// Show this expression at full weight.
    Expression(usize),
    /// Pick this mip level, array layer and cube face of the picture.
    Surface { level: u32, layer: u32, face: u32 },
    /// Show this layer of a layered picture alone.
    Layer(usize),
    /// Show this frame, paused.
    Frame(usize),
    /// Play this tag's run of frames.
    Tag(usize),
    /// Open this picture of the gallery.
    Picture(usize),
    /// Load this part of the file.
    Part(String),
    /// Load the character at this level of detail.
    Detail(u16),
    /// Nothing to show but the item's facts.
    Facts,
}

/// What the pieces were listed from, kept for saving them.  Everything in
/// here is shared with the loader's own copy, not copied.
#[derive(Default)]
pub(crate) struct Held {
    /// The scene's textures, or a material file's.
    pub textures: Vec<Arc<Texture>>,
    /// The file's picture, for its mip levels, layers and faces.
    pub picture: Option<Arc<Texture>>,
    pub canvas: Option<Arc<dyn Canvas>>,
    pub gallery: Vec<(String, Arc<Texture>)>,
    /// A material file's material, for its ball.
    pub material: Option<(String, slerp_scene::Material)>,
    pub material_preview: Option<Arc<Texture>>,
    pub saved_picture: Option<Arc<Texture>>,
    /// Each scene node's mesh and skin, and the materials each mesh uses:
    /// enough to find the nodes a piece stands for without the scene.
    pub node_refs: Vec<(Option<usize>, Option<usize>)>,
    pub mesh_materials: Vec<Vec<usize>>,
    /// The scene's collections, for the nodes each stands for.
    pub collections: Vec<slerp_scene::Collection>,
    /// Each node's parent, for placing pieces drawn alone.
    pub parents: Vec<Option<usize>>,
}

//endregion

/*┌─────────────────╮
| Opening         |
└─────────────────╯*/
//region Opening

/// Lists a file that isn't an archive, loading it whole once.
pub(super) fn open(kind: crate::Kind, path: &Path) -> Result<Contents> {
    let limits = Limits::viewer();
    let part_names = crate::parts(path);
    // Files whose parts are what they hold (a PDF's pages, a volume's
    // arrays) are listed without loading any part.
    if !part_names.is_empty() && !loads_whole_with_parts(kind) {
        let mut contents = empty(kind, path, &describe_kind(kind, path));
        add_parts(&mut contents, kind, &part_names, None);
        add_zip_members(&mut contents, path);
        return Ok(contents);
    }
    let failed = |e: String| ContentsError::Unsupported(format!("it couldn't be read: {e}"));
    if kind == crate::Kind::Dna {
        // The rigs are built too, for their controls and levels of detail.
        let mut character = slerp_dna::open(path, &slerp_dna::Options::viewer()).map_err(|e| failed(e.to_string()))?;
        let mut contents = of_scene_only(kind, path, &character.scene);
        add_character(&mut contents, &character);
        contents.set_scene(Arc::new(std::mem::take(&mut character.scene)));
        return Ok(contents);
    }
    let asset = crate::load_kind(kind, path, &limits).map_err(|e| failed(e.to_string()))?;
    let part = part_names.first().map(String::as_str);
    let contents = of_asset(kind, path, &asset, &part_names, part);
    if let Asset::Scene(scene) = asset {
        contents.set_scene(Arc::new(scene));
    }
    Ok(contents)
}

/// Lists a file the caller has loaded; `None` when there is little to list.
pub(super) fn of_loaded(
    kind: crate::Kind,
    path: &Path,
    asset: &Asset,
    part_names: &[String],
    part: Option<&str>,
) -> Option<Contents> {
    let contents = of_asset(kind, path, asset, part_names, part);
    worth_listing(&contents).then_some(contents)
}

/// Whether a listing holds enough to be worth a panel: at least two pieces
/// besides the whole scene.
fn worth_listing(c: &Contents) -> bool {
    c.items.iter().filter(|i| !matches!(i.source, Source::Piece(Piece::Scene))).count() >= 2
}

/// Formats whose parts are alternatives to the whole file, so the whole is
/// loaded and listed with the parts beside it (an LDtk project's first
/// level, a Source model's first skin family).
fn loads_whole_with_parts(kind: crate::Kind) -> bool {
    matches!(kind, crate::Kind::Sprite(_) | crate::Kind::Quake(_))
}

fn empty(kind: crate::Kind, path: &Path, label: &str) -> Contents {
    Contents {
        archive: None,
        path: Some(path.to_path_buf()),
        kind: Some(kind),
        label: label.to_string(),
        items: Vec::new(),
        entry_items: Vec::new(),
        palettes: OnceLock::new(),
        wad: OnceLock::new(),
        held: Held::default(),
        scene: OnceLock::new(),
        small_textures: Default::default(),
        warnings: Vec::new(),
    }
}

/// The format's name, for the panel's heading.
fn describe_kind(kind: crate::Kind, path: &Path) -> String {
    kind.format(path).map(|f| f.name.to_string()).unwrap_or_else(|| format!("{kind:?}"))
}

/// The listing of an asset, with parts and zip members.
fn of_asset(kind: crate::Kind, path: &Path, asset: &Asset, part_names: &[String], part: Option<&str>) -> Contents {
    let mut c = match asset {
        Asset::Scene(scene) => of_scene_only(kind, path, scene),
        Asset::Image(texture) => {
            let mut c = empty(kind, path, &picture_label(kind, path, texture));
            add_surfaces(&mut c, texture);
            c.held.picture = Some(texture.clone());
            c
        }
        Asset::Canvas(canvas) => {
            let mut c = empty(kind, path, &info_label(canvas.info(), kind, path));
            add_canvas(&mut c, canvas.as_ref());
            c.held.canvas = Some(canvas.clone());
            c
        }
        Asset::Gallery(pictures) => {
            let mut c = empty(kind, path, &describe_kind(kind, path));
            add_gallery(&mut c, pictures);
            c
        }
        Asset::Material(m) => {
            let mut c = empty(kind, path, &info_label(&m.info, kind, path));
            add_material_file(&mut c, m);
            c
        }
        Asset::Lut(_) => empty(kind, path, &describe_kind(kind, path)),
    };
    if part_names.len() > 1 || (part_names.len() == 1 && part.is_none()) {
        add_parts(&mut c, kind, part_names, part);
    }
    add_saved_picture(&mut c, kind, path);
    add_zip_members(&mut c, path);
    c
}

/// The picture the file's authoring program saved in it, which thumbnails
/// show (see `crate::saved_picture`), as a piece of its own.
fn add_saved_picture(c: &mut Contents, kind: crate::Kind, path: &Path) {
    if !crate::shows_saved_picture(kind) && !matches!(kind, crate::Kind::Paint(_)) {
        return;
    }
    let Some(saved) = crate::carried_picture(kind, path, &Limits::viewer()) else { return };
    let t = saved.texture;
    push(c, "Saved picture".into(), ItemKind::Picture, texture_detail(&t), texture_formats(&t), Piece::SavedPicture);
    c.held.saved_picture = Some(t);
}

/// A scene's listing, without parts or zip members.
fn of_scene_only(kind: crate::Kind, path: &Path, scene: &Scene) -> Contents {
    let mut c = empty(kind, path, &info_label(&scene.info, kind, path));
    add_scene(&mut c, scene);
    c.held.textures = scene.textures.clone();
    hold_shape(&mut c.held, scene);
    if kind == crate::Kind::Gltf {
        add_gltf_extras(&mut c, path);
    }
    c
}

fn info_label(info: &slerp_scene::Info, kind: crate::Kind, path: &Path) -> String {
    if info.format.is_empty() { describe_kind(kind, path) } else { info.format.clone() }
}

fn picture_label(kind: crate::Kind, path: &Path, t: &Texture) -> String {
    let s = &t.source;
    match (s.container.is_empty(), s.format.is_empty()) {
        (false, false) => format!("{} ({})", s.container, s.format),
        (false, true) => s.container.clone(),
        _ => describe_kind(kind, path),
    }
}

//endregion

/*┌─────────────────╮
| Listing         |
└─────────────────╯*/
//region Listing

/// Adds an item; `formats` empty means listed only.
fn push(c: &mut Contents, path: String, kind: ItemKind, detail: String, formats: Vec<Format>, piece: Piece) -> ItemId {
    let id = ItemId(c.items.len());
    c.items.push(Item { id, path, kind, size: 0, detail, formats, source: Source::Piece(piece), locked: false });
    id
}

/// A name as one part of an item path: no slashes (which would make
/// folders), no control characters, never empty.
pub(crate) fn part_name(name: &str, fallback: impl FnOnce() -> String) -> String {
    let cleaned: String = name
        .trim()
        .chars()
        .map(|c| {
            if c == '/' || c == '\\' {
                '_'
            } else if c.is_control() {
                ' '
            } else {
                c
            }
        })
        .collect();
    let cleaned = cleaned.trim();
    if cleaned.is_empty() { fallback() } else { cleaned.chars().take(120).collect() }
}

/// Names made unique within one folder by " 2", " 3" (scenes repeat names
/// freely: every MetaHuman LOD mesh is "head_lod0_mesh" in some file).
struct Unique(std::collections::HashMap<String, usize>);

impl Unique {
    fn new() -> Unique {
        Unique(Default::default())
    }

    fn name(&mut self, folder: &str, name: String) -> String {
        let key = format!("{folder}/{}", name.to_lowercase());
        let n = self.0.entry(key).or_insert(0);
        *n += 1;
        if *n == 1 { name } else { format!("{name} {n}") }
    }
}

fn count(n: usize, one: &str, many: &str) -> String {
    if n == 1 { format!("1 {one}") } else { format!("{} {many}", group_digits(n)) }
}

/// 12345 as "12,345".
fn group_digits(n: usize) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// Whether a texture holds values a PNG can't keep.
fn is_float(t: &Texture) -> bool {
    t.levels.first().and_then(|l| l.surfaces.first()).is_some_and(Pixels::is_hdr)
}

/// The formats a decoded picture saves as when it has no file of its own.
fn picture_formats(t: &Texture) -> Vec<Format> {
    if is_float(t) { vec![Format::Exr, Format::Png] } else { vec![Format::Png] }
}

//endregion

/*┌─────────────────╮
| Scenes          |
└─────────────────╯*/
//region Scenes

fn add_scene(c: &mut Contents, scene: &Scene) {
    let mut unique = Unique::new();
    let glb = || vec![Format::Glb];
    // 1. The whole scene.
    let triangles = scene.triangle_count();
    let whole = count(triangles, "triangle", "triangles");
    push(c, "Whole scene".into(), ItemKind::Scene, whole, glb(), Piece::Scene);

    // 2. The nodes, as their hierarchy, parents first.
    let mut stack: Vec<(usize, String)> = scene.roots.iter().rev().map(|&r| (r, "Nodes".to_string())).collect();
    let mut seen = vec![false; scene.nodes.len()];
    let mut listed = 0usize;
    while let Some((i, folder)) = stack.pop() {
        let Some(node) = scene.nodes.get(i) else { continue };
        if std::mem::replace(&mut seen[i], true) {
            continue;
        }
        if listed >= MAX_NODES {
            c.warnings.push(format!("only the first {MAX_NODES} nodes are listed"));
            break;
        }
        listed += 1;
        let name = unique.name(&folder, part_name(&node.name, || format!("node {i}")));
        let path = format!("{folder}/{name}");
        let mut detail = Vec::new();
        if let Some(m) = node.mesh.and_then(|m| scene.meshes.get(m)) {
            let tris: usize = m.primitives.iter().map(|p| p.triangle_count()).sum();
            detail.push(count(tris, "triangle", "triangles"));
        }
        if !node.visible {
            detail.push("hidden".into());
        }
        push(c, path.clone(), ItemKind::Node, detail.join(", "), glb(), Piece::Node(i));
        for &child in node.children.iter().rev() {
            stack.push((child, path.clone()));
        }
    }

    // 2a. The collections the nodes are organised in, as folders of their
    //     inner collections and their nodes.
    add_collections(c, scene, &mut unique);

    // 3. Meshes, with their blend shapes beneath them.
    for (i, mesh) in scene.meshes.iter().enumerate().take(MAX_PER_KIND) {
        let name = unique.name("Meshes", part_name(&mesh.name, || format!("mesh {i}")));
        let path = format!("Meshes/{name}");
        let tris: usize = mesh.primitives.iter().map(|p| p.triangle_count()).sum();
        let points: usize = mesh
            .primitives
            .iter()
            .filter(|p| p.topology != slerp_scene::Topology::Triangles)
            .map(|p| p.positions.len())
            .sum();
        let targets = mesh.primitives.iter().map(|p| p.targets.len()).max().unwrap_or(0);
        let mut detail = if tris == 0 && points > 0 {
            count(points, "point", "points")
        } else {
            count(tris, "triangle", "triangles")
        };
        if targets > 0 {
            detail += &format!(", {}", count(targets, "blend shape", "blend shapes"));
        }
        push(c, path.clone(), ItemKind::Mesh, detail, glb(), Piece::Mesh(i));
        for t in 0..targets.min(MAX_PER_KIND) {
            let target_name = mesh.target_names.get(t).map(String::as_str).unwrap_or("");
            let name = unique.name(&path, part_name(target_name, || format!("target {t}")));
            push(
                c,
                format!("{path}/{name}"),
                ItemKind::BlendShape,
                String::new(),
                Vec::new(),
                Piece::BlendShape { mesh: i, target: t },
            );
        }
    }

    // 4. Materials and textures.  Textures are named after the first
    //    material slot that uses them.
    let mut texture_names: Vec<Option<String>> = vec![None; scene.textures.len()];
    for (i, m) in scene.materials.iter().enumerate().take(MAX_PER_KIND) {
        let name = unique.name("Materials", part_name(&m.name, || format!("material {i}")));
        let slots = material_slots(m);
        for (slot, t) in &slots {
            if let Some(n) = texture_names.get_mut(*t).filter(|n| n.is_none()) {
                *n = Some(format!("{name} {slot}"));
            }
        }
        let detail = slots.iter().map(|(s, _)| *s).collect::<Vec<_>>().join(", ");
        push(c, format!("Materials/{name}"), ItemKind::Material, detail, glb(), Piece::Material(i));
    }
    for (i, t) in scene.textures.iter().enumerate().take(MAX_PER_KIND) {
        let base = own_name(t).or_else(|| texture_names[i].take()).unwrap_or_else(|| format!("texture {i}"));
        let name = unique.name("Textures", part_name(&base, || format!("texture {i}")));
        push(
            c,
            format!("Textures/{name}"),
            ItemKind::Texture,
            texture_detail(t),
            texture_formats(t),
            Piece::Texture(i),
        );
    }

    // 5. Clips, skeletons and expressions.
    for (i, a) in scene.animations.iter().enumerate().take(MAX_PER_KIND) {
        let name = unique.name("Animations", part_name(&a.name, || format!("clip {i}")));
        push(c, format!("Animations/{name}"), ItemKind::Animation, seconds(a.duration), glb(), Piece::Animation(i));
    }
    let parents = scene.parents();
    for (i, s) in scene.skins.iter().enumerate().take(MAX_PER_KIND) {
        // Unnamed skins go by their root joint.
        let root = s.skeleton.or_else(|| {
            let joints: std::collections::HashSet<usize> = s.joints.iter().copied().collect();
            s.joints.iter().copied().find(|&j| parents.get(j).copied().flatten().is_none_or(|p| !joints.contains(&p)))
        });
        // Loaders name skins the file left unnamed "skin 3".
        let generated = s.name.is_empty() || s.name.strip_prefix("skin ").is_some_and(|n| n.parse::<usize>().is_ok());
        let base = if generated {
            root.and_then(|r| scene.nodes.get(r)).map(|n| n.name.clone()).unwrap_or_default()
        } else {
            s.name.clone()
        };
        let name = unique.name("Skeletons", part_name(&base, || format!("skin {i}")));
        let detail = count(s.joints.len(), "joint", "joints");
        push(c, format!("Skeletons/{name}"), ItemKind::Skeleton, detail, glb(), Piece::Skeleton(i));
    }
    for (i, e) in scene.expressions.iter().enumerate().take(MAX_PER_KIND) {
        let name = unique.name("Expressions", part_name(&e.name, || format!("expression {i}")));
        let detail = e.preset.clone().filter(|p| !p.eq_ignore_ascii_case(&e.name)).unwrap_or_default();
        push(c, format!("Expressions/{name}"), ItemKind::Expression, detail, Vec::new(), Piece::Expression(i));
    }
    if let Some(s) = scene.splats.as_ref().filter(|s| !s.is_empty()) {
        push(c, "Splats".into(), ItemKind::Other, count(s.len(), "splat", "splats"), Vec::new(), Piece::Listed);
    }
}

/// Lists a scene's collections (Blender's) under "Collections", as Blender's
/// outliner shows them: each collection holds the collections inside it,
/// then its nodes.  A collection inside several others is listed in each,
/// and so is a node in several collections.
fn add_collections(c: &mut Contents, scene: &Scene, unique: &mut Unique) {
    /// What is left to list: a collection in a folder, with the
    /// collections above it (to cut cycles), or a listed collection's nodes.
    enum Step {
        Collection(usize, String, Vec<usize>),
        Nodes(usize, String),
    }
    let all = &scene.collections;
    let inner: std::collections::HashSet<usize> = all.iter().flat_map(|g| g.children.iter().copied()).collect();
    let mut stack: Vec<Step> = (0..all.len())
        .rev()
        .filter(|i| !inner.contains(i))
        .map(|i| Step::Collection(i, "Collections".to_string(), Vec::new()))
        .collect();
    let mut listed = 0usize;
    while let Some(step) = stack.pop() {
        if listed >= MAX_NODES {
            c.warnings.push(format!("only the first {MAX_NODES} collections and their nodes are listed"));
            return;
        }
        match step {
            Step::Collection(g, folder, mut above) => {
                if above.contains(&g) || above.len() >= MAX_COLLECTION_DEPTH {
                    continue;
                }
                let name = unique.name(&folder, part_name(&all[g].name, || format!("collection {g}")));
                let path = format!("{folder}/{name}");
                let nodes = collection_nodes(all, g, scene.nodes.len());
                let formats = if nodes.is_empty() { Vec::new() } else { vec![Format::Glb] };
                let detail = count(nodes.len(), "node", "nodes");
                push(c, path.clone(), ItemKind::Collection, detail, formats, Piece::Collection(g));
                listed += 1;
                // Off the stack in order: the inner collections, then the
                // nodes.
                stack.push(Step::Nodes(g, path.clone()));
                above.push(g);
                for &child in all[g].children.iter().rev().filter(|&&k| k < all.len()) {
                    stack.push(Step::Collection(child, path.clone(), above.clone()));
                }
            }
            Step::Nodes(g, path) => {
                for &n in all[g].nodes.iter().filter(|&&n| n < scene.nodes.len()) {
                    if listed >= MAX_NODES {
                        break;
                    }
                    listed += 1;
                    let node = &scene.nodes[n];
                    let name = unique.name(&path, part_name(&node.name, || format!("node {n}")));
                    let detail = if node.visible { String::new() } else { "hidden".into() };
                    push(c, format!("{path}/{name}"), ItemKind::Node, detail, vec![Format::Glb], Piece::Node(n));
                }
            }
        }
    }
}

/// The nodes of collection `g` and of the collections inside it, each once,
/// in order: what saving or showing the collection takes.
pub(crate) fn collection_nodes(all: &[slerp_scene::Collection], g: usize, node_count: usize) -> Vec<usize> {
    let mut out = Vec::new();
    let mut seen_nodes = std::collections::HashSet::new();
    let mut seen = vec![false; all.len()];
    let mut stack = vec![g];
    while let Some(k) = stack.pop() {
        let Some(group) = all.get(k) else { continue };
        if std::mem::replace(&mut seen[k], true) {
            continue;
        }
        out.extend(group.nodes.iter().copied().filter(|&n| n < node_count && seen_nodes.insert(n)));
        stack.extend(group.children.iter().rev());
    }
    out
}

/// The texture slots a material fills, by name, with the texture of each.
pub(crate) fn material_slots(m: &slerp_scene::Material) -> Vec<(&'static str, usize)> {
    let slots = [
        ("base colour", m.base_color_texture),
        ("metal-rough", m.metallic_roughness_texture),
        ("normal", m.normal_texture),
        ("occlusion", m.occlusion_texture),
        ("emissive", m.emissive_texture),
        ("transmission", m.transmission_texture),
        ("thickness", m.thickness_texture),
        ("lightmap", m.lightmap_texture),
        ("height", m.height_texture),
    ];
    slots.into_iter().filter_map(|(name, t)| Some((name, t?.texture))).collect()
}

/// A texture's own name in its file (a glTF image's name or URI, an FBX
/// texture's file name), as a file stem: "textures/Wood%20Floor.png" gives
/// "Wood Floor".
fn own_name(t: &Texture) -> Option<String> {
    let name = t.source.name.trim();
    let file = name.rsplit(['/', '\\']).next().unwrap_or(name);
    let file = percent_decoded(file);
    let stem = match file.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() && ext.len() <= 5 => stem.to_string(),
        _ => file,
    };
    (!stem.trim().is_empty()).then_some(stem)
}

/// URI text with `%20` and the like decoded, when that gives UTF-8.
fn percent_decoded(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = |b: u8| (b as char).to_digit(16);
        if bytes[i] == b'%'
            && let (Some(h), Some(l)) = (bytes.get(i + 1).and_then(|&b| hex(b)), bytes.get(i + 2).and_then(|&b| hex(b)))
        {
            out.push((h * 16 + l) as u8);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8(out).unwrap_or_else(|_| s.to_string())
}

fn texture_detail(t: &Texture) -> String {
    let s = &t.source;
    let (w, h) = (s.width.max(t.width()), s.height.max(t.height()));
    let mut out = format!("{w} × {h}");
    if !s.container.is_empty() {
        out += &format!(", {}", s.container);
    }
    out
}

/// What a scene's texture saves as: the bytes the file stored when the
/// loader kept them (see `save::has_original`), else a conversion.
fn texture_formats(t: &Texture) -> Vec<Format> {
    let mut formats = Vec::new();
    if super::save::has_original(t) {
        formats.push(Format::Raw);
    }
    formats.extend(picture_formats(t));
    formats
}

fn seconds(s: f32) -> String {
    if s < 1.0 { format!("{:.0} ms", s * 1000.0) } else { format!("{s:.2} s") }
}

/// Notes each node's mesh and skin and each mesh's materials, for
/// [`nodes_of`].
fn hold_shape(held: &mut Held, scene: &Scene) {
    held.node_refs = scene.nodes.iter().map(|n| (n.mesh, n.skin)).collect();
    held.collections = scene.collections.clone();
    held.parents = scene.parents();
    held.mesh_materials = scene
        .meshes
        .iter()
        .map(|m| {
            let mut v: Vec<usize> = m.primitives.iter().filter_map(|p| p.material).collect();
            v.sort_unstable();
            v.dedup();
            v
        })
        .collect();
}

/// The nodes a piece stands for in the scene: what showing it isolates and
/// what hovering it outlines.
pub(super) fn nodes_of(held: &Held, piece: &Piece) -> Vec<usize> {
    let users = |want: &dyn Fn(Option<usize>, Option<usize>) -> bool| -> Vec<usize> {
        held.node_refs.iter().enumerate().filter(|(_, (m, s))| want(*m, *s)).map(|(i, _)| i).collect()
    };
    match piece {
        Piece::Node(i) => vec![*i],
        Piece::Collection(g) => collection_nodes(&held.collections, *g, held.node_refs.len()),
        Piece::Mesh(m) | Piece::BlendShape { mesh: m, .. } => users(&|mesh, _| mesh == Some(*m)),
        Piece::Material(mat) => {
            users(&|mesh, _| mesh.and_then(|mesh| held.mesh_materials.get(mesh)).is_some_and(|ms| ms.contains(mat)))
        }
        Piece::Skeleton(s) => users(&|_, skin| skin == Some(*s)),
        _ => Vec::new(),
    }
}

/// A copy of a scene, for handing the one on screen to the contents model
/// while the view keeps its own.  Textures are shared, not copied.  Nodes
/// keep the visibility `visible` gives (the file's own), not what the
/// viewer has hidden since.
pub fn clone_scene(scene: &Scene, visible: Option<&[bool]>) -> Scene {
    let mut copy = scene.clone();
    if let Some(v) = visible {
        for (n, &shown) in copy.nodes.iter_mut().zip(v) {
            n.visible = shown;
        }
    }
    copy
}

//endregion

/*┌─────────────────╮
| Pictures        |
└─────────────────╯*/
//region Pictures

/// The name of cube face `f`, in the order textures store them.
pub(crate) fn face_name(f: u32) -> &'static str {
    ["+X", "-X", "+Y", "-Y", "+Z", "-Z"].get(f as usize).copied().unwrap_or("face")
}

/// A picture's mip levels, array layers and cube faces.  Each layer or face
/// is an item (its largest level), with its smaller levels beneath it; a
/// picture of one surface lists its levels at the top.
fn add_surfaces(c: &mut Contents, t: &Texture) {
    let levels = t.levels.len() as u32;
    let (layers, faces) = (t.layers.max(1), t.faces.max(1));
    if t.kind == TextureKind::D3 || levels * layers * faces <= 1 {
        return;
    }
    let formats = picture_formats(t);
    let size =
        |level: u32| t.levels.get(level as usize).map_or(String::new(), |l| format!("{} × {}", l.width, l.height));
    let surfaces = layers * faces;
    for layer in 0..layers {
        for face in 0..faces {
            if (layer * faces + face) as usize >= MAX_PER_KIND {
                c.warnings.push(format!("only the first {MAX_PER_KIND} layers are listed"));
                return;
            }
            let folder = match (layers > 1, faces > 1) {
                (true, true) => format!("Layer {layer} {}", face_name(face)),
                (true, false) => format!("Layer {layer}"),
                (false, true) => face_name(face).to_string(),
                (false, false) => String::new(),
            };
            let kind = match (layers > 1, faces > 1) {
                (_, true) => ItemKind::Face,
                (true, false) => ItemKind::Layer,
                _ => ItemKind::Mip,
            };
            let first = if surfaces > 1 {
                push(c, folder.clone(), kind, size(0), formats.clone(), Piece::Surface { level: 0, layer, face });
                1
            } else {
                0
            };
            for level in first..levels {
                let name = format!("Mip {level}");
                let path = if folder.is_empty() { name } else { format!("{folder}/{name}") };
                push(c, path, ItemKind::Mip, size(level), formats.clone(), Piece::Surface { level, layer, face });
            }
        }
        // A cube's six faces as one panorama, after them.
        if super::panorama::is_cube(t) {
            let (w, h) = super::panorama::size(t, super::panorama::MAX_WIDTH);
            let path = if layers > 1 { format!("Layer {layer} panorama") } else { "Panorama".to_string() };
            let detail = format!("{w} × {h}, equirectangular");
            push(c, path, ItemKind::Picture, detail, formats.clone(), Piece::Panorama { layer });
        }
    }
}

/// A layered or animated picture's layers (groups as folders), frames and
/// tags, each where there is more than one.
fn add_canvas(c: &mut Contents, canvas: &dyn Canvas) {
    let png = || vec![Format::Png];
    let layers = canvas.layers();
    if layers.len() > 1 {
        let mut unique = Unique::new();
        // The open groups: (depth, path).
        let mut groups: Vec<(u32, String)> = Vec::new();
        for (i, l) in layers.iter().enumerate().take(MAX_PER_KIND) {
            while groups.last().is_some_and(|(d, _)| *d >= l.depth) {
                groups.pop();
            }
            let folder = groups.last().map_or("Layers".to_string(), |(_, p)| p.clone());
            let name = unique.name(&folder, part_name(&l.name, || format!("layer {i}")));
            let path = format!("{folder}/{name}");
            let mut detail = l.note.clone();
            if !l.visible {
                detail = if detail.is_empty() { "hidden".into() } else { format!("{detail}, hidden") };
            }
            push(c, path.clone(), ItemKind::Layer, detail, png(), Piece::Layer(i));
            if l.group {
                groups.push((l.depth, path));
            }
        }
    }
    let frames = canvas.frames();
    if frames.len() > 1 {
        for (i, d) in frames.iter().enumerate().take(MAX_PER_KIND) {
            push(c, format!("Frames/Frame {}", i + 1), ItemKind::Frame, seconds(*d), png(), Piece::Frame(i));
        }
    }
    let mut unique = Unique::new();
    for (i, clip) in canvas.clips().iter().enumerate().take(MAX_PER_KIND) {
        let name = unique.name("Tags", part_name(&clip.name, || format!("tag {i}")));
        let detail = format!("frames {} to {}", clip.from + 1, clip.to + 1);
        push(c, format!("Tags/{name}"), ItemKind::Tag, detail, png(), Piece::Tag(i));
    }
}

fn add_gallery(c: &mut Contents, pictures: &[slerp_scene::NamedImage]) {
    if pictures.len() < 2 {
        return;
    }
    let mut unique = Unique::new();
    for (i, p) in pictures.iter().enumerate().take(MAX_PER_KIND) {
        let name = unique.name("Pictures", part_name(&p.name, || format!("picture {i}")));
        let detail = format!("{} × {}", p.texture.width(), p.texture.height());
        push(c, format!("Pictures/{name}"), ItemKind::Picture, detail, picture_formats(&p.texture), Piece::Picture(i));
        c.held.gallery.push((p.name.clone(), p.texture.clone()));
    }
}

/// A material file's textures, named by the slot they fill, its own
/// preview picture, and the ball it is shown on.
fn add_material_file(c: &mut Contents, m: &MaterialAsset) {
    let slots = material_slots(&m.material);
    let mut unique = Unique::new();
    for (i, t) in m.textures.iter().enumerate().take(MAX_PER_KIND) {
        let slot = slots.iter().find(|(_, s)| *s == i).map(|(n, _)| n.to_string());
        let base = own_name(t).or(slot).unwrap_or_else(|| format!("texture {i}"));
        let name = unique.name("Textures", part_name(&base, || format!("texture {i}")));
        push(
            c,
            format!("Textures/{name}"),
            ItemKind::Texture,
            texture_detail(t),
            texture_formats(t),
            Piece::Texture(i),
        );
    }
    c.held.textures = m.textures.clone();
    if let Some(p) = &m.preview {
        push(c, "Preview".into(), ItemKind::Picture, texture_detail(p), texture_formats(p), Piece::MaterialPreview);
        c.held.material_preview = Some(p.clone());
    }
    push(
        c,
        "Material ball".into(),
        ItemKind::Picture,
        "drawn on the GPU".into(),
        vec![Format::Png],
        Piece::MaterialBall,
    );
    c.held.material = Some((m.name.clone(), m.material.clone()));
}

//endregion

/*┌─────────────────╮
| Parts and files |
└─────────────────╯*/
//region Parts and files

/// The word for a file's parts, as a folder name.
fn parts_folder(kind: crate::Kind) -> &'static str {
    use crate::Kind as K;
    match kind {
        K::Sprite(_) => "Levels",
        K::Image(_) => "Layers",
        K::Document(_) => "Pages",
        K::Texture(_) => "Extensions",
        K::Preview(_) => "Sheets",
        K::Volume(_) => "Arrays",
        K::Quake(slerp_quake::QuakeFormat::SourceMdl) => "Skins",
        _ => "Parts",
    }
}

/// What a part saves as: what loading it gives.
fn part_formats(kind: crate::Kind) -> Vec<Format> {
    use crate::Kind as K;
    match kind {
        K::Image(slerp_images::ImageFormat::Exr) | K::Texture(_) => vec![Format::Exr, Format::Png],
        K::Quake(_) => vec![Format::Glb],
        _ => vec![Format::Png],
    }
}

fn add_parts(c: &mut Contents, kind: crate::Kind, names: &[String], shown: Option<&str>) {
    let folder = parts_folder(kind);
    let mut unique = Unique::new();
    for name in names.iter().take(MAX_PER_KIND) {
        let item = unique.name(folder, part_name(name, || "part".into()));
        let detail = if shown == Some(name.as_str()) { "shown".to_string() } else { String::new() };
        push(c, format!("{folder}/{item}"), ItemKind::Part, detail, part_formats(kind), Piece::Part(name.clone()));
    }
}

/// The member files of a zip-based format (3MF, USDZ, OpenRaster, Krita),
/// under "Files": a 3MF's plate pictures, a USDZ's textures, each saved
/// exactly as stored.
fn add_zip_members(c: &mut Contents, path: &Path) {
    let mut head = [0u8; 4];
    let is_zip = std::fs::File::open(path).and_then(|mut f| std::io::Read::read_exact(&mut f, &mut head)).is_ok()
        && head == *b"PK\x03\x04";
    if !is_zip || c.archive.is_some() {
        return;
    }
    let Ok(archive) = slerp_archive::Archive::open(path) else { return };
    let entry_items = super::list_archive(&archive, &mut c.items, &mut c.warnings, "Files/");
    c.entry_items = entry_items;
    c.archive = Some(archive);
}

/// Adds what a MetaHuman character holds beyond its scene: the character
/// at each level of detail (saved as GLB) and the controls of its rigs
/// (listed only: they mean something only inside RigLogic).
pub(crate) fn add_character(c: &mut Contents, character: &slerp_dna::Character) {
    for lod in 0..character.lod_count() {
        let detail = if lod == character.lod() { "shown".to_string() } else { String::new() };
        push(c, format!("Detail levels/LOD {lod}"), ItemKind::Detail, detail, vec![Format::Glb], Piece::Detail(lod));
    }
    let mut unique = Unique::new();
    for (folder, rig) in [("Rig controls", &character.head_rig), ("Body rig controls", &character.body_rig)] {
        let Some(rig) = rig else { continue };
        for (i, control) in rig.controls.iter().enumerate().take(MAX_PER_KIND) {
            let name = unique.name(folder, part_name(control, || format!("control {i}")));
            push(c, format!("{folder}/{name}"), ItemKind::Control, String::new(), Vec::new(), Piece::Listed);
        }
    }
}

/// A glTF file's cameras and punctual lights, which the scene model doesn't
/// hold, read from its JSON: listed for what they are.
fn add_gltf_extras(c: &mut Contents, path: &Path) {
    let Some(root) = gltf_json(path) else { return };
    let mut unique = Unique::new();
    for (i, cam) in root.get("cameras").and_then(|v| v.as_array()).into_iter().flatten().enumerate().take(MAX_PER_KIND)
    {
        let name = cam.get("name").and_then(|n| n.as_str()).unwrap_or("");
        let name = unique.name("Cameras", part_name(name, || format!("camera {i}")));
        let kind = cam.get("type").and_then(|t| t.as_str()).unwrap_or("");
        let detail = match (kind, cam.pointer("/perspective/yfov").and_then(|v| v.as_f64())) {
            ("perspective", Some(fov)) => format!("perspective, {:.0}°", fov.to_degrees()),
            (k, _) => k.to_string(),
        };
        push(c, format!("Cameras/{name}"), ItemKind::Camera, detail, Vec::new(), Piece::Listed);
    }
    let lights = root.pointer("/extensions/KHR_lights_punctual/lights").and_then(|v| v.as_array());
    for (i, light) in lights.into_iter().flatten().enumerate().take(MAX_PER_KIND) {
        let name = light.get("name").and_then(|n| n.as_str()).unwrap_or("");
        let name = unique.name("Lights", part_name(name, || format!("light {i}")));
        let detail = light.get("type").and_then(|t| t.as_str()).unwrap_or("").to_string();
        push(c, format!("Lights/{name}"), ItemKind::Light, detail, Vec::new(), Piece::Listed);
    }
}

/// The JSON of a glTF file (a GLB's first chunk, or a `.gltf` whole), when
/// it is small enough to read for a listing.
fn gltf_json(path: &Path) -> Option<serde_json::Value> {
    use std::io::Read;
    const MAX_JSON: u64 = 64 << 20;
    let mut file = std::fs::File::open(path).ok()?;
    let mut head = [0u8; 20];
    let n = file.read(&mut head).ok()?;
    if n == 20 && &head[..4] == b"glTF" {
        let len = u32::from_le_bytes([head[12], head[13], head[14], head[15]]) as u64;
        if len > MAX_JSON || &head[16..20] != b"JSON" {
            return None;
        }
        let mut json = vec![0u8; len as usize];
        file.read_exact(&mut json).ok()?;
        return serde_json::from_slice(&json).ok();
    }
    if file.metadata().ok()?.len() > MAX_JSON {
        return None;
    }
    serde_json::from_slice(&std::fs::read(path).ok()?).ok()
}

//endregion

/*┌─────────────────╮
| Showing         |
└─────────────────╯*/
//region Showing

pub(super) fn show(c: &Contents, piece: &Piece) -> Show {
    match piece {
        Piece::Scene => Show::Whole,
        Piece::Node(i) => Show::Nodes { nodes: vec![*i], subtree: true },
        Piece::Collection(_) => Show::Nodes { nodes: nodes_of(&c.held, piece), subtree: true },
        Piece::Mesh(_) | Piece::Material(_) | Piece::Skeleton(_) => {
            Show::Nodes { nodes: nodes_of(&c.held, piece), subtree: false }
        }
        Piece::BlendShape { mesh, target } => {
            Show::BlendShape { nodes: nodes_of(&c.held, piece), mesh: *mesh, target: *target }
        }
        Piece::Animation(i) => Show::Clip(*i),
        Piece::Expression(i) => Show::Expression(*i),
        Piece::Surface { level, layer, face } => Show::Surface { level: *level, layer: *layer, face: *face },
        Piece::Panorama { .. } => Show::Preview,
        Piece::Layer(i) => Show::Layer(*i),
        Piece::Frame(i) => Show::Frame(*i),
        Piece::Tag(i) => Show::Tag(*i),
        Piece::Picture(i) => Show::Picture(*i),
        Piece::Part(name) => Show::Part(name.clone()),
        Piece::Detail(lod) => Show::Detail(*lod),
        Piece::Texture(_) | Piece::MaterialBall | Piece::MaterialPreview | Piece::SavedPicture => Show::Preview,
        Piece::Listed => Show::Facts,
    }
}

impl Contents {
    /// Adds what a MetaHuman character holds beyond the scene it was listed
    /// from: the character at each level of detail and its rig controls.
    pub fn add_character(&mut self, character: &slerp_dna::Character) {
        add_character(self, character);
    }

    /// The scene nodes an item stands for: a node, a collection's nodes, or
    /// the nodes using a mesh, a material or a skeleton; empty for items
    /// that aren't parts of a scene.  The viewer outlines them when the item is hovered.
    pub fn nodes(&self, id: ItemId) -> Vec<usize> {
        match self.item(id).map(|i| &i.source) {
            Some(Source::Piece(p)) => nodes_of(&self.held, p),
            _ => Vec::new(),
        }
    }
}

//endregion
