//! Creative apps whose documents are zips with a preview picture inside:
//! Procreate, Sketch, Figma (local copies), Adobe XD, FreeCAD and Pixelmator
//! Pro.  Only the zip's directory and the one member are read.  Older Figma
//! copies aren't zips but bare `fig-kiwi` files, read here too.
//!
//! Pixelmator Pro documents (`.pxd`) are packages on a Mac, sent and saved
//! as stored zips of the package: an SQLite `metadata.info`, the layers in
//! `data/`, and `QuickLook/Thumbnail.webp` (the size the canvas fits in 1024
//! pixels or so) and `QuickLook/Icon.webp`.

use crate::PreviewFormat;
use crate::source::Source;
use crate::zip::Zip;
use slerp_scene::textures::Texture;
use slerp_scene::{Limits, LoadError};

/// The largest preview inflated.
const MAX_PICTURE: u64 = 64 << 20;

/// Each format's preview members, best first.  Adobe XD stores the first
/// artboard twice: `thumbnail.png` (512 pixels across) and `preview.png`
/// (2048); thumbnails take the small one, the viewer the large one.
fn members(format: PreviewFormat, small: bool) -> &'static [&'static str] {
    match format {
        PreviewFormat::Procreate => &["QuickLook/Thumbnail.png"],
        PreviewFormat::Sketch => &["previews/preview.png"],
        PreviewFormat::Figma => &["thumbnail.png"],
        PreviewFormat::AdobeXd if small => &["thumbnail.png", "preview.png"],
        PreviewFormat::AdobeXd => &["preview.png", "thumbnail.png"],
        PreviewFormat::FreeCad => &["thumbnails/Thumbnail.png"],
        PreviewFormat::Pixelmator => &["QuickLook/Thumbnail.webp", "QuickLook/Thumbnail.png", "QuickLook/Icon.webp"],
        _ => &[],
    }
}

pub fn preview(format: PreviewFormat, src: &mut Source, limits: &Limits) -> Result<Texture, LoadError> {
    if format == PreviewFormat::Figma && src.head(8)? == FIG_KIWI {
        return fig_kiwi(src, limits);
    }
    let zip = Zip::open(src)?;
    let small = limits.max_texture_size.is_some_and(|n| n <= 512);
    for name in members(format, small) {
        if let Some(png) = zip.read_named(src, name, MAX_PICTURE)? {
            return crate::decode_image(&png, limits);
        }
    }
    Err(crate::no_preview(format.name()))
}

/// The magic of a bare Figma file, as older versions saved local copies
/// and as the `canvas.fig` inside newer ones.
pub const FIG_KIWI: &[u8] = b"fig-kiwi";

/// A bare Figma file: `fig-kiwi`, a 32-bit version, then chunks of a
/// little-endian 32-bit length and data: the compressed schema, the
/// compressed document and, in saved files, a PNG thumbnail.  A
/// `canvas.fig` taken from a zip has no thumbnail chunk.
fn fig_kiwi(src: &mut Source, limits: &Limits) -> Result<Texture, LoadError> {
    let mut at = 12u64;
    for _ in 0..16 {
        let Ok(h) = src.exact(at, 12) else { break };
        let len = u32::from_le_bytes([h[0], h[1], h[2], h[3]]) as u64;
        if h[4..12] == *b"\x89PNG\r\n\x1a\n" {
            if len > MAX_PICTURE {
                return Err(LoadError::TooLarge("Figma thumbnail".into()));
            }
            let png = src.exact(at + 4, len)?;
            return crate::decode_image(&png, limits);
        }
        at += 4 + len;
    }
    Err(crate::no_preview("Figma file"))
}
