//! Affinity Photo, Designer and Publisher documents, from version 1 to the
//! single Affinity 3 format (`.af`).  All share one little-endian
//! container: `00 FF 4B 41`, a format version, `nsrP#Inf`, then at 0x10
//! the offset of the file table and at 0x18 the offset of a trailer of
//! records.  Each record is `FF FF FF FF`, a four-byte tag, a version, the
//! size of the rest of the record, the offset of its data from the
//! record's start, the data's length, one byte and the data.  The first
//! record, `Thmb`, holds the preview as an uncompressed PNG (512 pixels
//! on its long side); `.af` files follow it with a `Meta` record of JSON.
//! So the preview takes two small reads, whatever the document's size.

use crate::source::Source;
use slerp_scene::textures::Texture;
use slerp_scene::{Limits, LoadError};

pub const MAGIC: &[u8] = b"\x00\xFFKA";

/// The largest preview read.
const MAX_PICTURE: u64 = 64 << 20;

/// The most trailer records passed over looking for the preview.
const MAX_RECORDS: usize = 64;

pub fn looks_like_affinity(head: &[u8]) -> bool {
    head.starts_with(MAGIC) && head.get(8..12) == Some(b"nsrP")
}

pub fn preview(src: &mut Source, limits: &Limits) -> Result<Texture, LoadError> {
    let h = src.exact(0, 0x20)?;
    if !h.starts_with(MAGIC) {
        return Err(LoadError::invalid("not an Affinity document"));
    }
    let mut at = u64::from_le_bytes(h[0x18..0x20].try_into().unwrap_or([0; 8]));
    for _ in 0..MAX_RECORDS {
        let Ok(r) = src.exact(at, 29) else { break };
        if r[..4] != [0xFF; 4] {
            break;
        }
        let le32 = |i: usize| u32::from_le_bytes([r[i], r[i + 1], r[i + 2], r[i + 3]]) as u64;
        let size = le32(12);
        let data = u64::from_le_bytes(r[16..24].try_into().unwrap_or([0; 8]));
        let len = le32(24);
        if &r[4..8] == b"Thmb" {
            if len > MAX_PICTURE {
                return Err(LoadError::TooLarge("Affinity preview".into()));
            }
            let start = at.checked_add(data).ok_or_else(|| LoadError::invalid("Affinity: bad preview offset"))?;
            let png = src.exact(start, len)?;
            return crate::decode_image(&png, limits);
        }
        at = match at.checked_add(16 + size) {
            Some(next) => next,
            None => break,
        };
    }
    Err(crate::no_preview("Affinity document"))
}
