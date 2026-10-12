//! Material files and USD stages: MaterialX documents (`.mtlx`), USD
//! layers and packages (`.usda`, `.usdc`, `.usd`, `.usdz`), Godot material
//! resources (`.tres`) and Substance archives (`.sbsar`).
//!
//! A material file becomes `Asset::Material`.  Thumbnails show its preview
//! picture when it has one (a USD `AssetPreviewsAPI` thumbnail, or an image
//! next to the file named like it, as ambientCG and Poly Haven ship them) and
//! otherwise a ball wearing the material.  The material is mapped onto the
//! glTF-style `slerp_scene::Material`: MaterialX and USD shading networks
//! are followed from the surface shader through texture, normal-map and
//! displacement nodes; procedural nodes that can't be evaluated fall back to
//! their constant inputs or the shader's defaults.  USD stages with meshes
//! become `Asset::Scene`, with UsdSkel skeletons, skins, blend shapes and
//! animation as Slerp's skins, morph targets and clips, and point
//! instancers as nodes sharing their prototypes' meshes (see `usd/skel.rs`
//! and `usd/instancer.rs`).  A `.sbsar` archive can't be evaluated without
//! Substance's engine, so it becomes the picture of its embedded icon.
//!
//! Every reader here runs on files a user merely browses past, so none of
//! them trusts the file: sizes are checked before allocating, recursion is
//! bounded, and malformed input is an error rather than a panic.
//!
//! When a thumbnail-sized load (`Limits::max_texture_size` set) finds a
//! preview picture, the material's own textures aren't decoded at all, since
//! the thumbnail shows only the picture; their file names are still listed
//! in `Info::details`.

mod godot;
mod graph;
mod mtlx;
mod preview;
mod realize;
mod sbsar;
mod usd;

use slerp_scene::{Asset, Limits, LoadError};
use std::path::Path;

/// The file formats this crate reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MaterialFormat {
    /// A MaterialX XML document.
    MaterialX,
    /// A USD layer, as text (`#usda`) or binary crate (`PXR-USDC`), whatever
    /// its extension.
    Usd,
    /// A USDZ package: an uncompressed zip whose first file is a USD layer.
    Usdz,
    /// A Godot text resource (`[gd_resource ...]`).
    GodotResource,
    /// A Substance archive (7z).
    Sbsar,
}

/// Whether a package entry's name is a USD layer.
fn is_usd_name(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    name.ends_with(".usdc") || name.ends_with(".usda") || name.ends_with(".usd")
}

/// Recognises a format from the file's name and first bytes.  `head` should
/// hold at least the first 64 bytes when the file is that long.
pub fn sniff(path: &Path, head: &[u8]) -> Option<MaterialFormat> {
    let ext = path.extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase()).unwrap_or_default();
    if head.starts_with(b"PXR-USDC") || head.starts_with(b"#usda ") || head.starts_with(b"#usda\t") {
        return Some(MaterialFormat::Usd);
    }
    if head.starts_with(b"PK\x03\x04") {
        if ext == "usdz" || slerp_archive::first_zip_name(head).is_some_and(|n| is_usd_name(&n)) {
            return Some(MaterialFormat::Usdz);
        }
        return None;
    }
    if head.starts_with(b"[gd_resource") {
        return Some(MaterialFormat::GodotResource);
    }
    if head.starts_with(b"7z\xBC\xAF\x27\x1C") {
        return (ext == "sbsar").then_some(MaterialFormat::Sbsar);
    }
    let text = strip_bom(head);
    let first = text.iter().position(|b| !b.is_ascii_whitespace()).map(|i| &text[i..]).unwrap_or(&[]);
    if first.starts_with(b"<") && (ext == "mtlx" || contains(text, b"<materialx")) {
        return Some(MaterialFormat::MaterialX);
    }
    None
}

/// Loads a file of a format `sniff` recognised.  `bytes` is the whole file;
/// `path` locates the files it refers to (textures, other layers, preview
/// pictures), which are read only when `limits.neighbours` allows.
///
/// - MaterialX and Godot materials become `Asset::Material`, with `preview`
///   set when a preview picture exists.  Godot resources that aren't
///   materials are `LoadError::Unsupported`.
/// - USD and USDZ become `Asset::Material` when the stage holds only
///   materials and `Asset::Scene` when it has meshes.
/// - `.sbsar` becomes `Asset::Image` of its embedded icon.
pub fn load(format: MaterialFormat, path: &Path, bytes: &[u8], limits: &Limits) -> Result<Asset, LoadError> {
    if bytes.len() as u64 > limits.max_file_bytes {
        return Err(LoadError::TooLarge(format!("{} bytes", bytes.len())));
    }
    // Textures, layers and includes on disk are recorded where they are
    // read (`realize`, `usd`, `mtlx`).
    let (mut result, neighbours) = slerp_scene::collect_neighbours(|| match format {
        MaterialFormat::MaterialX => mtlx::load(path, bytes, limits),
        MaterialFormat::Usd | MaterialFormat::Usdz => usd::load(format, path, bytes, limits),
        MaterialFormat::GodotResource => godot::load(path, bytes, limits),
        MaterialFormat::Sbsar => sbsar::load(bytes, limits),
    });
    match &mut result {
        Ok(Asset::Scene(scene)) => scene.info.neighbours = neighbours,
        Ok(Asset::Material(material)) => material.info.neighbours = neighbours,
        _ => {}
    }
    result
}

/// Godot materials read together by `godot_materials`.
pub struct GodotMaterials {
    /// One per resource given, in order; `None` for a resource that isn't a
    /// material Godot's text format describes (or doesn't parse).
    pub materials: Vec<Option<slerp_scene::Material>>,
    /// The textures the materials' slots index.
    pub textures: Vec<std::sync::Arc<slerp_scene::Texture>>,
    pub warnings: Vec<String>,
}

/// Reads several Godot material resources at once, for scenes
/// (`slerp-engines`) that use many materials, so that a texture several of
/// them share is decoded once.  Each resource is the text of a `.tres` file
/// (`[gd_resource type="StandardMaterial3D" ...]` with its `ext_resource`
/// sections and `[resource]`) plus the path it is resolved from: `res://`
/// paths are found from the nearest `project.godot` above it, plain paths
/// beside it.  No preview pictures are looked for.  Texture files read are
/// noted with `slerp_scene::record_neighbour`.
pub fn godot_materials(resources: &[(&Path, &[u8])], limits: &Limits) -> GodotMaterials {
    godot::many(resources, limits)
}

fn strip_bom(b: &[u8]) -> &[u8] {
    b.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(b)
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

/// Converts an sRGB-encoded component to linear, for formats that store
/// colours the way a colour picker shows them (Godot).
fn srgb_to_linear(c: f32) -> f32 {
    if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
}

#[cfg(test)]
mod tests;
