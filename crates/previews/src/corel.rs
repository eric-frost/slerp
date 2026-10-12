//! CorelDRAW files.  Documents from CorelDRAW 7 to 13 (X3) and Corel
//! Presentation Exchange (`.cmx`) files are RIFF containers with a `DISP`
//! chunk near the start: the clipboard format number 8 (`CF_DIB`) followed by
//! a 96x96 device-independent bitmap with 256 colours.  X4 and later keep a
//! zip: X4 to X6 hold a 24-bit BMP at `metadata/thumbnails/thumbnail.bmp`,
//! and X7 and later a PNG at `previews/thumbnail.png`.

use crate::Stored;
use crate::source::Source;
use crate::zip::{self, Zip};
use slerp_scene::{Limits, LoadError};

/// The largest preview inflated.
const MAX_PICTURE: u64 = 64 << 20;

/// Is this the start of a CorelDRAW RIFF file (`CDR` plus a version letter
/// or digit) or a Presentation Exchange one?
pub fn looks_like_riff(head: &[u8]) -> bool {
    head.len() >= 12
        && head.starts_with(b"RIFF")
        && (head[8..11] == *b"CDR" && head[11].is_ascii_alphanumeric() || head[8..12] == *b"CMX1")
}

/// The zip members that hold the picture, best first.
const MEMBERS: [&str; 4] = [
    "previews/thumbnail.png",
    "metadata/thumbnails/thumbnail.bmp",
    "metadata/thumbnails/thumbnail.png",
    "previews/page1.png",
];

pub fn preview(src: &mut Source, limits: &Limits) -> Result<Stored, LoadError> {
    let head = src.head(12)?;
    if zip::is_zip(&head) {
        let zip = Zip::open(src)?;
        for name in MEMBERS {
            if let Some(bytes) = zip.read_named(src, name, MAX_PICTURE)? {
                return crate::decode_image(&bytes, limits).map(Stored::Image);
            }
        }
        return Err(crate::no_preview("CorelDRAW file"));
    }
    if !looks_like_riff(&head) {
        return Err(LoadError::NotMine);
    }
    // The chunks follow the form type: a four-character name and a little
    // endian size each, padded to even sizes.  `DISP` comes first or second;
    // some versions nest it in a list, so look inside lists too.
    let mut at = 12u64;
    for _ in 0..64 {
        let Ok(h) = src.exact(at, 8) else { break };
        let size = u32::from_le_bytes([h[4], h[5], h[6], h[7]]) as u64;
        if &h[..4] == b"DISP" {
            let data = src.exact(at + 8, size.min(MAX_PICTURE))?;
            // Four bytes of clipboard format, then the DIB.
            let dib = data.get(4..).ok_or_else(|| LoadError::invalid("CorelDRAW preview: short DISP chunk"))?;
            let bmp =
                crate::bmp_from_dib(dib).ok_or_else(|| LoadError::invalid("CorelDRAW preview: short DISP chunk"))?;
            return crate::decode_image(&bmp, limits).map(Stored::Image);
        }
        if &h[..4] == b"LIST" && size >= 4 {
            // Step into the list: its own four-character type comes first.
            at += 12;
            continue;
        }
        at = at.saturating_add(8).saturating_add(size).saturating_add(size & 1);
    }
    Err(crate::no_preview("CorelDRAW file"))
}
