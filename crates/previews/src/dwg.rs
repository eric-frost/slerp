//! AutoCAD drawings (`.dwg`).  The drawing itself is a closed format whose
//! one open reader, LibreDWG, is GPL, so Slerp shows the preview picture
//! AutoCAD keeps in every drawing from R13 (1994) on.
//!
//! The file header holds the preview section's address as a little-endian
//! 32-bit number at byte 0x0D.  The section starts with a 16-byte sentinel,
//! then its size (32 bits), a count of entries (8 bits), and per entry a
//! type (8 bits), an absolute address and a size (32 bits each).  Type 1 is
//! 80 bytes of header data, 2 a BMP without its 14-byte file header (8-bit,
//! sometimes RLE8 compressed), 3 a Windows metafile with an Aldus placeable
//! header, and 6 a PNG (R2013 and later write PNGs instead of BMPs).  Files
//! from R2004 on encrypt their header past byte 0x80 and keep sections in
//! pages, but the address at 0x0D still leads straight to the preview,
//! whose page is stored uncompressed (R2007's too).  Drawings saved by
//! programs other than AutoCAD sometimes have a section with only the
//! header data, and R9 to R12 drawings have no section: neither has a
//! picture.
//!
//! The layout follows cad-preview (github.com/cinderblock/cad-preview, MIT)
//! and the Open Design Alliance's published DWG specification.

use crate::Stored;
use crate::source::Source;
use slerp_scene::{Limits, LoadError};

const SENTINEL: [u8; 16] =
    [0x1F, 0x25, 0x6D, 0x07, 0xD4, 0x36, 0x28, 0x28, 0x9D, 0x57, 0xCA, 0x3F, 0x9D, 0x44, 0x10, 0x2B];

/// When the address doesn't lead to the sentinel, it is looked for in this
/// much of the start of the file, where AutoCAD always puts the section.
const SEARCH: u64 = 4 << 20;

/// The largest picture read; real ones are a few kilobytes to a few
/// hundred.
const MAX_PICTURE: u64 = 32 << 20;

const BMP: u8 = 2;
const WMF: u8 = 3;
const PNG: u8 = 6;

/// Is this the start of a DWG file: `AC10` and two more digits, the
/// release's number (`AC1032` is AutoCAD 2018 to 2026)?
pub fn looks_like_dwg(head: &[u8]) -> bool {
    head.len() >= 6 && head.starts_with(b"AC10") && head[4].is_ascii_digit() && head[5].is_ascii_digit()
}

/// The AutoCAD release that introduced a version code, for the inspector.
fn release(code: &[u8]) -> Option<&'static str> {
    Some(match code {
        b"AC1012" => "R13",
        b"AC1014" => "R14",
        b"AC1015" => "2000",
        b"AC1018" => "2004",
        b"AC1021" => "2007",
        b"AC1024" => "2010",
        b"AC1027" => "2013",
        b"AC1032" => "2018",
        _ => return None,
    })
}

pub fn preview(src: &mut Source, limits: &Limits) -> Result<Stored, LoadError> {
    let head = src.head(0x15)?;
    if !looks_like_dwg(&head) || head.len() < 0x15 {
        return Err(LoadError::NotMine);
    }
    let version = &head[..6];
    if version < b"AC1012".as_slice() {
        return Err(LoadError::Unsupported("AutoCAD drawings older than R13 have no preview picture".into()));
    }
    // 1. The section: where the header says, else searched for.
    let pointer = u32::from_le_bytes([head[0x0D], head[0x0E], head[0x0F], head[0x10]]) as u64;
    let at = if src.read_at(pointer, 16).is_ok_and(|s| s == SENTINEL) { pointer } else { search(src)? };
    // 2. The table of entries after the sentinel, size and count.
    let count = src.exact(at + 20, 1)?[0] as u64;
    if count == 0 || count > 16 {
        return Err(crate::no_preview("AutoCAD drawing"));
    }
    let table = src.exact(at + 21, count * 9)?;
    let entries: Vec<(u8, u64, u64)> = table
        .chunks_exact(9)
        .map(|e| {
            let start = u32::from_le_bytes([e[1], e[2], e[3], e[4]]) as u64;
            let size = u32::from_le_bytes([e[5], e[6], e[7], e[8]]) as u64;
            (e[0], start, size)
        })
        .collect();
    // 3. The best picture: a PNG, then a BMP, then a metafile.
    let mut stored = None;
    for wanted in [PNG, BMP, WMF] {
        let Some(&(_, start, size)) = entries.iter().find(|e| e.0 == wanted && e.2 >= 8) else { continue };
        if size > MAX_PICTURE {
            return Err(LoadError::TooLarge("AutoCAD preview".into()));
        }
        let bytes = src.exact(start, size)?;
        stored = Some(match wanted {
            BMP => {
                let bmp =
                    crate::bmp_from_dib(&bytes).ok_or_else(|| LoadError::invalid("AutoCAD preview: not a bitmap"))?;
                Stored::Image(crate::decode_image(&bmp, limits)?)
            }
            WMF => Stored::Metafile(bytes),
            _ => Stored::Image(crate::decode_image(&bytes, limits)?),
        });
        break;
    }
    let mut stored = stored.ok_or_else(|| crate::no_preview("AutoCAD drawing"))?;
    if let (Stored::Image(t), Some(name)) = (&mut stored, release(version)) {
        t.source
            .header
            .push(("Saved as".into(), format!("AutoCAD {name} drawing ({})", String::from_utf8_lossy(version))));
    }
    Ok(stored)
}

/// Finds the sentinel near the start of the file.
fn search(src: &mut Source) -> Result<u64, LoadError> {
    let bytes = src.head(SEARCH)?;
    bytes
        .windows(SENTINEL.len())
        .position(|w| w == SENTINEL)
        .map(|i| i as u64)
        .ok_or_else(|| crate::no_preview("AutoCAD drawing"))
}
