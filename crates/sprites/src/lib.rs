//! 2D game formats: Aseprite sprites and Tiled maps, both read into a
//! [`Canvas`] that composes any frame with any set of layers on request.
//!
//! - **Aseprite** (`.aseprite`, `.ase`): every colour mode, blend mode,
//!   layer and group opacity, linked cels, tilemap layers, tags.
//! - **Sprite-sheet descriptors** (libGDX/Spine `.atlas`, TexturePacker
//!   `.tpsheet`, TexturePacker and Aseprite JSON): the sheet picture the
//!   file names with each sprite outlined.
//! - **LDtk** (`.ldtk` projects and `.ldtkl` level files): IntGrid, tile,
//!   auto-layer and entity layers; a project's world of levels, and each
//!   level on its own as a part of the file (see [`parts`]).
//! - **Piskel** (`.piskel`) sprites and **Pyxel Edit** (`.pyxel`) documents:
//!   whole-picture layers with opacity (and Pyxel Edit's blend modes), and
//!   Piskel's frames at the file's frame rate.
//! - **Tiled** (`.tmx` XML and `.tmj` JSON): orthogonal, isometric,
//!   staggered and hexagonal maps; CSV, base64, zlib, gzip and zstd layer
//!   data; embedded, external (`.tsx`, `.tsj`) and image-collection
//!   tilesets; flipped tiles, image layers, object layers and templates.
//!
//! Tileset pictures are read from beside the file when [`Limits`] allows;
//! without them, tiles are drawn as coloured cells.
//!
//! Thumbnails keep hard pixel edges: small sprites grow by whole factors,
//! and large maps are composed a little above the thumbnail's size and
//! then averaged down.

mod aseprite;
mod atlas;
mod blend;
mod ldtk;
mod piskel;
mod pixels;
mod pyxel;
mod raster;
mod stack;
mod tiled;

#[cfg(test)]
mod tests;

pub use blend::Blend;
pub use pixels::{fit_size, resize};

use slerp_scene::{Asset, Canvas, Image8, Limits, LoadError};
use std::path::Path;
use std::sync::Arc;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpriteFormat {
    Aseprite,
    /// Tiled's XML map.
    Tmx,
    /// Tiled's JSON map.
    Tmj,
    /// An LDtk project (`.ldtk`) or a level saved on its own (`.ldtkl`).
    Ldtk,
    /// A sprite-sheet descriptor (libGDX `.atlas`, TexturePacker `.tpsheet`
    /// and JSON): the sheet picture with every sprite outlined.
    Atlas,
    /// A Piskel sprite.
    Piskel,
    /// A Pyxel Edit document.
    Pyxel,
}

/// Recognises a file from its first bytes and name.  Aseprite files carry
/// magic numbers; Tiled maps are XML or JSON, recognised by their root
/// element or their extension.
pub fn sniff(path: &Path, head: &[u8]) -> Option<SpriteFormat> {
    if aseprite::sniff(head) {
        return Some(SpriteFormat::Aseprite);
    }
    let ext = path.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase).unwrap_or_default();
    if pyxel::sniff(&ext, head) {
        return Some(SpriteFormat::Pyxel);
    }
    let text = String::from_utf8_lossy(head);
    if piskel::sniff(&ext, &text) {
        return Some(SpriteFormat::Piskel);
    }
    if atlas::sniff(&ext, &text) {
        return Some(SpriteFormat::Atlas);
    }
    let text = text.trim_start_matches('\u{feff}').trim_start();
    if text.starts_with('<') && tiled::looks_like_tmx(text) && (ext == "tmx" || text.contains("tiledversion")) {
        return Some(SpriteFormat::Tmx);
    }
    if ldtk::looks_like_ldtk(text, &ext) {
        return Some(SpriteFormat::Ldtk);
    }
    if text.starts_with('{')
        && (ext == "tmj" || (ext == "json" && text.contains("\"tiledversion\"") && text.contains("\"orientation\"")))
    {
        return Some(SpriteFormat::Tmj);
    }
    None
}

/// Reads a sprite or map.  `path` locates a map's tilesets, templates and
/// images.
pub fn load(format: SpriteFormat, path: &Path, bytes: &[u8], limits: &Limits) -> Result<Asset, LoadError> {
    let canvas: Arc<dyn Canvas> = match format {
        SpriteFormat::Aseprite => Arc::new(aseprite::load(bytes, limits)?),
        SpriteFormat::Tmx | SpriteFormat::Tmj => Arc::new(tiled::load(format, path, bytes, limits)?),
        SpriteFormat::Ldtk => Arc::new(ldtk::load(path, bytes, limits, ldtk::Pick::World)?),
        SpriteFormat::Atlas => {
            let ext = path.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase).unwrap_or_default();
            Arc::new(atlas::load(path, &ext, &String::from_utf8_lossy(bytes), limits.neighbours)?)
        }
        SpriteFormat::Piskel => Arc::new(piskel::load(bytes, limits)?),
        SpriteFormat::Pyxel => Arc::new(pyxel::load(bytes, limits)?),
    };
    Ok(Asset::Canvas(canvas))
}

/// The most bytes `parts` reads: LDtk projects with every level inside run
/// to tens of megabytes, never this much.
const MAX_PARTS_FILE: u64 = 256 << 20;

/// The names of a file's parts, when it holds several things shown one at
/// a time: an LDtk project's levels.  Empty for files that are one thing,
/// projects with a single level, and unreadable files.  `load` gives the
/// whole file (an LDtk project's world of levels); `load_part` one level.
pub fn parts(format: SpriteFormat, path: &Path) -> Vec<String> {
    if format != SpriteFormat::Ldtk {
        return Vec::new();
    }
    match pixels::read_neighbour(path, MAX_PARTS_FILE) {
        Ok(bytes) => ldtk::parts(&bytes).unwrap_or_default(),
        Err(_) => Vec::new(),
    }
}

/// Loads the part of a file called `name`, one of those `parts` lists.
pub fn load_part(format: SpriteFormat, path: &Path, limits: &Limits, name: &str) -> Result<Asset, LoadError> {
    if format != SpriteFormat::Ldtk {
        return Err(LoadError::Unsupported("this file has no parts".into()));
    }
    let bytes = pixels::read_neighbour(path, limits.max_file_bytes)?;
    Ok(Asset::Canvas(Arc::new(ldtk::load(path, &bytes, limits, ldtk::Pick::Level(name))?)))
}

/// The thumbnail of a canvas: its first frame with the layers the file
/// shows, no larger than `size`.
pub fn thumbnail(canvas: &dyn Canvas, size: u32) -> Image8 {
    let (w, h) = canvas.size();
    let long = w.max(h).max(1);
    // Compose at up to twice the thumbnail's size, so shrinking averages
    // several pixels into each and thin lines don't flicker away, but never
    // above full size.
    let scale = (2.0 * size.max(1) as f32 / long as f32).min(1.0);
    let shown: Vec<bool> = canvas.layers().iter().map(|l| l.visible).collect();
    let img = canvas.compose(0, &shown, scale);
    pixels::thumbnail(&img, size)
}
