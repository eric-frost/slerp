//! SketchUp models.  Every version starts with the UTF-16 text
//! "SketchUp Model" and a version in braces.  Versions 6 to 2020 continue
//! as one serialised object stream whose first picture, a PNG a few
//! kilobytes in, is the thumbnail.  Versions from 2021 put a short `VFF`
//! header after the version and then a zip, whose first entry is
//! `meta/model_thumbnail.png`.

use crate::source::Source;
use crate::zip::Zip;
use slerp_scene::textures::Texture;
use slerp_scene::{Limits, LoadError};

/// The UTF-16 byte-order mark and text that start every SketchUp file.
const MAGIC: &[u8] = b"\xFF\xFE\xFF\x0ES\0k\0e\0t\0c\0h\0U\0p\0 \0M\0o\0d\0e\0l\0";

/// How far into an old-style file the thumbnail is looked for: it comes
/// after a few kilobytes of header objects.
const SEARCH: u64 = 256 << 10;

/// The largest thumbnail read.
const MAX_PICTURE: u64 = 32 << 20;

const PNG: &[u8] = b"\x89PNG\r\n\x1a\n";

pub fn looks_like_skp(head: &[u8]) -> bool {
    head.starts_with(MAGIC)
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

pub fn preview(src: &mut Source, limits: &Limits) -> Result<Texture, LoadError> {
    let head = src.head(SEARCH)?;
    // 1. 2021 and later: the zip after the version, read through its
    //    directory at the end of the file.
    if find(&head[..head.len().min(4096)], b"PK\x03\x04").is_some()
        && let Ok(zip) = Zip::open(src)
        && let Some(png) = zip.read_named(src, "meta/model_thumbnail.png", MAX_PICTURE)?
    {
        return crate::decode_image(&png, limits);
    }
    // 2. Earlier: the first PNG, read to its end chunk.
    let Some(at) = find(&head, PNG) else { return Err(crate::no_preview("SketchUp model")) };
    let png = png_at(src, at as u64)?;
    crate::decode_image(&png, limits)
}

/// Reads the PNG starting at `at`: its chunks up to `IEND`.
fn png_at(src: &mut Source, at: u64) -> Result<Vec<u8>, LoadError> {
    let mut end = at + 8;
    loop {
        let h = src.exact(end, 8)?;
        let len = u32::from_be_bytes([h[0], h[1], h[2], h[3]]) as u64;
        end = end.checked_add(12 + len).ok_or_else(|| LoadError::invalid("SketchUp thumbnail: bad chunk"))?;
        if end - at > MAX_PICTURE {
            return Err(LoadError::TooLarge("SketchUp thumbnail".into()));
        }
        if &h[4..8] == b"IEND" {
            break;
        }
    }
    src.exact(at, end - at)
}
