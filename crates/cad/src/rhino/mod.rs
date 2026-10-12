//! Rhino `.3dm` files.
//!
//! A `.3dm` file always carries a small preview picture in its properties
//! table, near the start, which is read without reading the rest.  Files
//! saved the usual way also hold the render meshes Rhino shades objects
//! with: meshes themselves, and the meshes cached in Breps (one per face)
//! and extrusions.  Those are read here, with layers, materials and
//! visibility, so the viewer shows the model.  Files saved with "Save
//! small" have no meshes and show their preview picture; thumbnails always
//! show it (Slerp's rule for files that carry a picture of themselves).  Curves, points, annotations and SubD surfaces are not drawn.

mod chunks;
mod mesh;
mod model;
mod object;
mod preview;
mod scene;

use chunks::{Archive, Chunk};
use slerp_scene::textures::Texture;
use slerp_scene::{Limits, LoadError, Scene};
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

/// True when `head` starts a Rhino file.
pub fn sniff(head: &[u8]) -> bool {
    Archive::open(head).is_some()
}

/// The most the preview reader takes of a properties table.
const MAX_TABLE: u64 = 64 << 20;

/// Rhino's release name for an archive version.
fn release(version: u32) -> String {
    match version {
        1..=4 => format!("Rhino {version}"),
        v if v % 10 == 0 && (50..=200).contains(&v) => format!("Rhino {}", v / 10),
        v => format!("archive version {v}"),
    }
}

/// Reads the preview picture of the file at `path` by seeking from chunk to
/// chunk to the properties table: a few kilobytes, whatever the file size.
pub fn preview_path(path: &Path) -> Result<Option<Texture>, LoadError> {
    let mut f = std::fs::File::open(path)?;
    let len = f.metadata()?.len();
    let mut head = [0u8; 32];
    f.read_exact(&mut head).map_err(|_| LoadError::NotMine)?;
    let a = Archive::open(&head).ok_or(LoadError::NotMine)?;
    let header = 4 + if a.big() { 8 } else { 4 };
    let mut pos = 32u64;
    // The start section, then the properties table: the second chunk or so.
    for _ in 0..16 {
        if pos + header as u64 > len {
            break;
        }
        f.seek(SeekFrom::Start(pos))?;
        let mut buf = [0u8; 12];
        f.read_exact(&mut buf[..header])?;
        let Some((tcode, value)) = a.header(&buf[..header]) else { break };
        if tcode == chunks::END_OF_FILE || tcode & chunks::TCODE_SHORT != 0 {
            break;
        }
        let body = value.max(0) as u64;
        if tcode == chunks::TABLE_PROPERTIES {
            if body > MAX_TABLE || pos + header as u64 + body > len {
                break;
            }
            let mut data = vec![0u8; header + body as usize];
            f.seek(SeekFrom::Start(pos))?;
            f.read_exact(&mut data)?;
            let sub = Archive { data: &data, version: a.version };
            let chunk = sub.chunk_at(0, data.len()).ok_or(LoadError::NotMine)?;
            return Ok(preview::read(&sub, &chunk));
        }
        pos += header as u64 + body;
    }
    Ok(None)
}

/// A line or two on what the reader found, for debugging.
pub fn summary(data: &[u8], limits: &Limits) -> String {
    let Some(a) = Archive::open(data) else { return "not a Rhino file".into() };
    let m = model::read(&a, limits.max_triangles);
    let meshes = m.objects.iter().filter(|o| matches!(o.geometry, model::Geometry::Meshes(_))).count();
    let instances = m.objects.len() - meshes;
    let in_blocks = m.objects.iter().filter(|o| o.attrs.mode == 3).count();
    let resolved: usize =
        m.definitions.values().flatten().filter(|id| m.objects.iter().any(|o| &o.attrs.id == *id)).count();
    let members: usize = m.definitions.values().map(Vec::len).sum();
    format!(
        "{} objects ({} with meshes, {} instances, {} in blocks), {} block definitions with {members} members ({resolved} found), {} layers, {} materials, {} without meshes, skipped {:?}",
        m.object_count,
        meshes,
        instances,
        in_blocks,
        m.definitions.len(),
        m.layers.len(),
        m.materials.len(),
        m.without_meshes,
        m.skipped
    )
}

/// What a Rhino file shows.
pub enum Content {
    /// Meshes, as a scene.
    Model(Box<Scene>),
    /// Only the stored picture.
    Preview(Texture),
}

/// Reads a whole file for what it shows: its meshes if it has any,
/// otherwise its preview picture.
pub fn load_data(data: &[u8], limits: &Limits) -> Result<Content, LoadError> {
    let a = Archive::open(data).ok_or(LoadError::NotMine)?;
    if a.version < 2 {
        // Rhino 1 files have another layout; only their preview could be
        // read, and it is stored differently too.
        return Err(LoadError::Unsupported("Rhino 1 files".into()));
    }
    let model = model::read(&a, limits.max_triangles);
    if !model.objects.is_empty() {
        let scene = scene::build(&a, &model, limits);
        // Objects can all be hidden or in blocks that nothing places.
        if !scene.meshes.is_empty() {
            return Ok(Content::Model(Box::new(scene)));
        }
    }
    let table = a.chunks(32..data.len()).find(|c: &Chunk| c.tcode == chunks::TABLE_PROPERTIES);
    match table.and_then(|t| preview::read(&a, &t)) {
        Some(p) => Ok(Content::Preview(p)),
        None => Err(LoadError::Unsupported(format!(
            "a {} file with neither render meshes nor a preview picture",
            release(a.version)
        ))),
    }
}

/// The content of the file at `path`, for the viewer and `slerp info`.
pub fn load_path(path: &Path, limits: &Limits) -> Result<Content, LoadError> {
    let size = std::fs::metadata(path)?.len();
    if size > limits.max_file_bytes {
        // Too big to read whole; its preview still shows.
        return match preview_path(path)? {
            Some(p) => Ok(Content::Preview(p)),
            None => Err(LoadError::TooLarge(format!("{size} bytes"))),
        };
    }
    load_data(&std::fs::read(path)?, limits)
}

#[cfg(test)]
mod tests;
