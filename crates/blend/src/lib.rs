//! Blender `.blend` files: the preview picture Blender stores in them, and
//! the scene itself, read through the structure description (SDNA) every
//! file carries, so files from Blender 2.4 to 5.x open with one reader.
//!
//! What a scene holds:
//!
//! - The objects of the active scene with their parent hierarchy and
//!   transforms (Blender is Z up; the scene is turned to Y up), collection
//!   instances, and only what is visible in both the viewport and renders.
//! - The collections the objects are organised in, as
//!   `Scene::collections`, for the Contents panel to list.
//! - Meshes in every layout Blender has used (2.4's faces, 2.6 to 3.4's
//!   polygons and loops, generic attributes since 3.5, and Blender 5's
//!   attribute storage): positions, n-gons triangulated, UV maps, colour
//!   attributes, flat and smooth shading with sharp edges, and custom
//!   normals.
//! - The modifiers that make many models look finished, applied in stack
//!   order: Mirror, Array, Subdivision Surface, Solidify, Triangulate,
//!   Weld, Edge Split and Smooth by Angle.  Others are listed in a warning.
//! - Materials from their node trees: Principled BSDF and the image
//!   textures feeding its colour, roughness, metallic, normal, emission and
//!   alpha inputs, packed in the file or beside it.
//!
//! Every reader here runs on files a user merely browses past, so nothing
//! may panic, hang or allocate without bound: reads are bounds-checked
//! against their blocks, counts are checked against the bytes present, and
//! [`Limits`] caps textures, triangles and subdivision.

// Geometry code walks faces by index into several parallel arrays at once,
// which iterators over one of them would only obscure.
#![allow(clippy::needless_range_loop)]

mod debug;
mod file;
mod image;
mod link;
mod material;
mod mesh;
mod modifiers;
mod normals;
mod poly;
mod preview;
mod scene;
mod sdna;
mod subsurf;
#[cfg(test)]
mod tests;
mod triangulate;
mod view;

pub use file::Header;

use slerp_scene::{Limits, LoadError, Scene, Texture};
use std::io::Read;
use std::path::Path;

/// Recognises a `.blend` file from its first bytes and name.  Plain files
/// start with "BLENDER", gzip-compressed ones (Blender 2.x) inflate to it,
/// and Zstandard-compressed ones (Blender 3.0 and later) are recognised by
/// the Zstandard magic and the extension, since their first frame is far
/// larger than the bytes sniffing looks at.
pub fn sniff(path: &Path, head: &[u8]) -> bool {
    match file::Packing::of(head) {
        Some(file::Packing::Plain) => true,
        Some(file::Packing::Gzip) => {
            let mut magic = [0u8; 7];
            flate2::read::GzDecoder::new(head).read_exact(&mut magic).is_ok() && &magic == b"BLENDER"
        }
        Some(file::Packing::Zstd) => has_blend_extension(path),
        None => false,
    }
}

/// `.blend`, and the backups Blender keeps beside it (`.blend1`,
/// `.blend2`, ...).
fn has_blend_extension(path: &Path) -> bool {
    let ext = path.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase).unwrap_or_default();
    ext.strip_prefix("blend").is_some_and(|rest| rest.chars().all(|c| c.is_ascii_digit()))
}

/// The preview picture stored in the file, reading only the start of it.
/// `Ok(None)` when the file was saved without one.
pub fn preview_path(path: &Path) -> Result<Option<Texture>, LoadError> {
    preview::read(std::io::BufReader::new(std::fs::File::open(path)?))
}

/// The preview picture of a file already in memory.
pub fn preview(bytes: &[u8]) -> Result<Option<Texture>, LoadError> {
    preview::read(bytes)
}

/// A text dump of the file's structure, for developing the reader; see
/// `debug.rs` for what `what` can ask.
#[doc(hidden)]
pub fn dump(bytes: &[u8], what: &str) -> Result<String, LoadError> {
    let (data, packing) = file::unpack(bytes, 16 << 30)?;
    let f = file::BlendFile::parse(data, packing)?;
    Ok(debug::dump(&f, what))
}

/// Loads the active scene of a `.blend` file whose bytes are `bytes` (as
/// read from `path`, whose folder holds any external textures).
pub fn load(path: &Path, bytes: &[u8], limits: &Limits) -> Result<Scene, LoadError> {
    let (result, neighbours) = slerp_scene::collect_neighbours(|| {
        // An uncompressed file may be up to the size limit; a compressed one
        // may inflate to 64 times its size, which no real file approaches.
        let cap = limits.max_file_bytes.min((bytes.len() as u64).saturating_mul(64).saturating_add(64 << 20));
        let (data, packing) = file::unpack(bytes, cap.max(bytes.len() as u64))?;
        let f = file::BlendFile::parse(data, packing)?;
        let libs = link::Libraries::read(&f, path.parent(), limits);
        scene::build(&f, &libs, limits, path.parent())
    });
    let mut scene = result?;
    scene.info.neighbours = neighbours;
    Ok(scene)
}
