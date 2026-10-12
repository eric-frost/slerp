//! Cinema 4D scenes.  Files start with an eight-byte magic (`QC4DC4D6`, or
//! `XC4DC4D6` and `6C4DC4D6` in older versions) and then Maxon's
//! "HyperFile" chunks.  From R13 on, the first chunk holds the document's
//! preview as a JPEG: a type byte, a big-endian length and the JPEG's
//! bytes, a few dozen bytes into the file.  Older files have no preview.

use crate::source::Source;
use slerp_scene::textures::Texture;
use slerp_scene::{Limits, LoadError};

/// The largest preview read.
const MAX_PICTURE: u64 = 32 << 20;

pub fn looks_like_c4d(head: &[u8]) -> bool {
    head.len() >= 8 && head[1..8] == *b"C4DC4D6" && matches!(head[0], b'Q' | b'X' | b'6')
}

pub fn preview(src: &mut Source, limits: &Limits) -> Result<Texture, LoadError> {
    let head = src.head(256)?;
    // The JPEG's start marker, with its length in the four bytes before.
    let Some(at) = head.windows(3).position(|w| w == [0xFF, 0xD8, 0xFF]).filter(|&i| i >= 4) else {
        return Err(crate::no_preview("Cinema 4D scene"));
    };
    let len = u32::from_be_bytes([head[at - 4], head[at - 3], head[at - 2], head[at - 1]]) as u64;
    // Files whose length field doesn't fit are read to the JPEG decoder's
    // satisfaction from what is there.
    let len = if len > 0 && len <= MAX_PICTURE && at as u64 + len <= src.len() {
        len
    } else {
        (src.len() - at as u64).min(MAX_PICTURE)
    };
    let jpeg = src.exact(at as u64, len)?;
    crate::decode_image(&jpeg, limits)
}
