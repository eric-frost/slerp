//! Clip Studio Paint illustrations.  A `.clip` file is `CSFCHUNK`, the
//! file's size and the header's, then chunks of an eight-byte tag
//! (`CHNKHead`, `CHNKExta` for each block of layer data, `CHNKSQLi`,
//! `CHNKFoot`) and a big-endian 64-bit length; `CHNKHead` holds the
//! offset of the database chunk.  The `CHNKSQLi` chunk is an
//! SQLite database describing the document, whose `CanvasPreview` table
//! holds the preview as a PNG in its `ImageData` column.  Free pages of the
//! database can hold older previews, so the table is read through its
//! b-tree rather than by looking for a PNG.

use crate::source::Source;
use crate::sqlite::{Db, Value};
use slerp_scene::textures::Texture;
use slerp_scene::{Limits, LoadError};

pub const MAGIC: &[u8] = b"CSFCHUNK";

/// The most chunks walked before the database is given up on.
const MAX_CHUNKS: usize = 1 << 16;

pub fn looks_like_clip(head: &[u8]) -> bool {
    head.starts_with(MAGIC)
}

pub fn preview(src: &mut Source, limits: &Limits) -> Result<Texture, LoadError> {
    // 1. The database chunk, found by hopping over the others.
    let h = src.exact(0, 24)?;
    if !h.starts_with(MAGIC) {
        return Err(LoadError::invalid("not a Clip Studio file"));
    }
    let mut at = u64::from_be_bytes(h[16..24].try_into().unwrap_or([0; 8])).max(24);
    let mut db = None;
    // `CHNKHead` says where the database chunk is, which saves walking
    // past every layer's chunk; files where it doesn't are walked.
    if let Ok(o) = src.exact(48, 8)
        && let sqli = u64::from_be_bytes(o.try_into().unwrap_or([0; 8]))
        && src.exact(sqli, 8).is_ok_and(|tag| tag == b"CHNKSQLi")
    {
        at = sqli;
    }
    for _ in 0..MAX_CHUNKS {
        let Ok(c) = src.exact(at, 16) else { break };
        let len = u64::from_be_bytes(c[8..16].try_into().unwrap_or([0; 8]));
        if &c[..8] == b"CHNKSQLi" {
            db = Some((at + 16, len.min(src.len().saturating_sub(at + 16))));
            break;
        }
        if &c[..4] != b"CHNK" || &c[..8] == b"CHNKFoot" {
            break;
        }
        at = match at.checked_add(16).and_then(|a| a.checked_add(len)) {
            Some(next) => next,
            None => break,
        };
    }
    let Some((base, len)) = db else { return Err(LoadError::invalid("Clip Studio file without its database")) };
    // 2. The preview row.
    let mut db = Db::open(src, base, len)?;
    let Some((root, columns)) = db.table("CanvasPreview")? else {
        return Err(crate::no_preview("Clip Studio file"));
    };
    let column = columns.iter().position(|c| c == "ImageData").unwrap_or(2);
    let Some(row) = db.find_row(root, |_| true)? else { return Err(crate::no_preview("Clip Studio file")) };
    match row.into_iter().nth(column) {
        Some(Value::Blob(png)) => crate::decode_image(&png, limits),
        _ => Err(crate::no_preview("Clip Studio file")),
    }
}
