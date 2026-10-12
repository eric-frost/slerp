//! Binary palettes: Adobe Swatch Exchange, Photoshop swatches, Adobe
//! colour tables (all big-endian) and Microsoft's RIFF palettes.

use super::{Color, Palette, PaletteFormat, Swatch, SwatchKind, utf16_be};
use slerp_scene::LoadError;

fn be16(b: &[u8], i: usize) -> Option<u16> {
    Some(u16::from_be_bytes(b.get(i..i + 2)?.try_into().ok()?))
}

fn be32(b: &[u8], i: usize) -> Option<u32> {
    Some(u32::from_be_bytes(b.get(i..i + 4)?.try_into().ok()?))
}

/// A float that is a usable colour component: NaN and infinities become 0.
fn finite(v: f32) -> f32 {
    if v.is_finite() { v } else { 0.0 }
}

/*┌─────────────────╮
| Swatch Exchange |
└─────────────────╯*/
//region Swatch Exchange

const ASE_GROUP_START: u16 = 0xC001;
const ASE_GROUP_END: u16 = 0xC002;
const ASE_COLOR: u16 = 0x0001;

/// Adobe Swatch Exchange: `ASEF`, a version, a block count, then blocks
/// of a type, a length and a body.  Colour bodies hold a UTF-16 name, a
/// four-letter model, its components as floats and a kind (global, spot
/// or normal); group blocks open and close named groups.
pub fn ase(b: &[u8]) -> Result<Palette, LoadError> {
    let mut p = Palette::new(PaletteFormat::Ase);
    if b.len() < 12 {
        return Err(LoadError::invalid("Swatch Exchange file: truncated header"));
    }
    // The block count isn't trusted: some writers count only top-level
    // blocks.  Blocks are read to the end of the file instead; each takes
    // at least six bytes, so the loop is bounded.
    let mut at = 12usize;
    for _ in 0..b.len() / 6 {
        let (Some(kind), Some(len)) = (be16(b, at), be32(b, at + 2)) else { break };
        let start = at + 6;
        let Some(body) = start.checked_add(len as usize).and_then(|end| b.get(start..end)) else {
            p.warnings.push("the file ends inside a block".into());
            break;
        };
        at = start + len as usize;
        match kind {
            ASE_GROUP_START => {
                let (name, used) = ase_name(body).unwrap_or_default();
                p.begin_group(name);
                // Some writers make a group's block span its colours too;
                // they are read as the blocks they are.
                if used < body.len() {
                    at = start + used;
                }
            }
            // Colours after a group's end belong to no group.
            ASE_GROUP_END => p.begin_group(String::new()),
            ASE_COLOR => match ase_color(body) {
                Some(s) => {
                    if !p.push(s) {
                        break;
                    }
                }
                None => p.warnings.push("a damaged colour entry was skipped".into()),
            },
            _ => {}
        }
    }
    Ok(p)
}

/// A block's name: a count of UTF-16 units (with the closing NUL), then
/// the units.  Returns the name and where the rest of the body starts.
fn ase_name(body: &[u8]) -> Option<(String, usize)> {
    let units = be16(body, 0)? as usize;
    Some((utf16_be(body, 2, units)?, 2 + units * 2))
}

fn ase_color(body: &[u8]) -> Option<Swatch> {
    let (name, at) = ase_name(body)?;
    let model = body.get(at..at + 4)?;
    let f = |i: usize| be32(body, at + 4 + i * 4).map(|v| finite(f32::from_bits(v)));
    let (color, n) = match model {
        b"RGB " => (Color::Rgb([f(0)?, f(1)?, f(2)?]), 3),
        b"CMYK" => (Color::Cmyk([f(0)?, f(1)?, f(2)?, f(3)?]), 4),
        // Lightness is stored from 0 to 1.
        b"LAB " => (Color::Lab([f(0)? * 100.0, f(1)?, f(2)?]), 3),
        b"Gray" | b"GRAY" => (Color::Gray(f(0)?), 1),
        _ => return None,
    };
    let kind = match be16(body, at + 4 + n * 4) {
        Some(0) => SwatchKind::Global,
        Some(1) => SwatchKind::Spot,
        _ => SwatchKind::Normal,
    };
    Some(Swatch { kind, ..Swatch::new(name, color) })
}

//endregion

/*┌─────────────────╮
| Photoshop       |
└─────────────────╯*/
//region Photoshop

/// Colour spaces Photoshop writes: RGB, HSB, CMYK, Lab, greyscale, and the
/// 0-10000 CMYK that GIMP reads as id 9.
const ACO_SPACES: [u16; 6] = [0, 1, 2, 7, 8, 9];

/// Photoshop swatches start with a version (1 or 2), a count of colours
/// and the first colour's space.
pub fn looks_like_aco(head: &[u8]) -> bool {
    let (Some(version), Some(count)) = (be16(head, 0), be16(head, 2)) else { return false };
    let space_ok = be16(head, 4).is_none_or(|s| ACO_SPACES.contains(&s));
    let fits = head.len() >= 4096 || head.len() >= 4 + count as usize * 10;
    matches!(version, 1 | 2) && count > 0 && space_ok && fits && head.len() >= 14
}

/// Photoshop swatches: a version 1 section of ten-byte colours (a space
/// and four 16-bit values), usually followed by a version 2 section with
/// the same colours and their names.  The named section wins when it reads
/// cleanly.
pub fn aco(b: &[u8]) -> Result<Palette, LoadError> {
    let mut p = Palette::new(PaletteFormat::Aco);
    let mut colors = Vec::new();
    let mut at = 0usize;
    // 1. The first section, of either version.
    let version = be16(b, 0).unwrap_or(0);
    let first = aco_section(b, &mut at, version == 2, &mut p.warnings);
    // 2. A second, named section after a first unnamed one.
    if version == 1 && be16(b, at) == Some(2) {
        let mut at2 = at;
        let named = aco_section(b, &mut at2, true, &mut Vec::new());
        if !named.is_empty() && named.len() >= first.len() {
            colors = named;
        }
    }
    if colors.is_empty() {
        colors = first;
    }
    for s in colors {
        if !p.push(s) {
            break;
        }
    }
    Ok(p)
}

fn aco_section(b: &[u8], at: &mut usize, named: bool, warnings: &mut Vec<String>) -> Vec<Swatch> {
    let count = be16(b, *at + 2).unwrap_or(0) as usize;
    *at += 4;
    let mut out = Vec::with_capacity(count.min(b.len() / 10));
    for _ in 0..count {
        let Some(c) = b.get(*at..*at + 10) else {
            warnings.push("the file ends before its last colour".into());
            break;
        };
        *at += 10;
        let mut name = String::new();
        if named {
            // A 32-bit count of UTF-16 units, the closing NUL included.
            let Some(units) = be32(b, *at) else { break };
            let Some(n) = utf16_be(b, *at + 4, units as usize) else { break };
            name = n;
            *at += 4 + units as usize * 2;
        }
        let v = |i: usize| u16::from_be_bytes([c[2 + i * 2], c[3 + i * 2]]);
        let s = |i: usize| v(i) as i16 as f32;
        let color = match u16::from_be_bytes([c[0], c[1]]) {
            0 => Color::Rgb([0, 1, 2].map(|i| v(i) as f32 / 65535.0)),
            1 => Color::Hsb([v(0) as f32 / 65535.0 * 360.0, v(1) as f32 / 65535.0, v(2) as f32 / 65535.0]),
            // 0 means full ink, 65535 none.
            2 => Color::Cmyk([0, 1, 2, 3].map(|i| 1.0 - v(i) as f32 / 65535.0)),
            7 => Color::Lab([v(0) as f32 / 100.0, s(1) / 100.0, s(2) / 100.0]),
            // The ink's coverage from 0 to 10000, as GIMP reads it.
            8 => Color::Gray(1.0 - (v(0) as f32 / 10000.0).min(1.0)),
            9 => Color::Cmyk([0, 1, 2, 3].map(|i| 1.0 - (v(i) as f32 / 10000.0).min(1.0))),
            space => {
                warnings.push(format!("a colour in unknown colour space {space} was skipped"));
                continue;
            }
        };
        out.push(Swatch::new(name, color));
    }
    out
}

//endregion

/*┌─────────────────╮
| Colour tables   |
└─────────────────╯*/
//region Colour tables

/// Adobe colour tables have no magic: 256 RGB triples, optionally followed
/// by a count and a transparent index.  `head` is the whole file.
pub fn looks_like_act(head: &[u8]) -> bool {
    match head.len() {
        768 => true,
        772 => {
            let (count, transparent) = (be16(head, 768).unwrap_or(0), be16(head, 770).unwrap_or(0));
            count <= 256 && (transparent == 0xFFFF || transparent < 256)
        }
        _ => false,
    }
}

pub fn act(b: &[u8]) -> Result<Palette, LoadError> {
    if !looks_like_act(b) {
        return Err(LoadError::invalid("an Adobe colour table is 768 or 772 bytes"));
    }
    let mut p = Palette::new(PaletteFormat::Act);
    let (mut count, mut transparent) = (256usize, None);
    if b.len() == 772 {
        // A count of 0 is written by some tools for a full table.
        count = match be16(b, 768).unwrap_or(0) {
            0 => 256,
            n => n as usize,
        };
        transparent = be16(b, 770).filter(|&t| t != 0xFFFF).map(|t| t as usize);
    }
    for (i, c) in b[..768].chunks_exact(3).take(count).enumerate() {
        let mut s = Swatch::new(String::new(), Color::Rgb([c[0], c[1], c[2]].map(|v| v as f32 / 255.0)));
        if transparent == Some(i) {
            s.alpha = 0.0;
            s.name = "Transparent".into();
        }
        p.push(s);
    }
    Ok(p)
}

//endregion

/*┌─────────────────╮
| RIFF            |
└─────────────────╯*/
//region RIFF

/// Microsoft's RIFF palette: `RIFF`, a size, `PAL `, then chunks; the
/// `data` chunk holds a version, a count and four bytes per colour (red,
/// green, blue and flags).
pub fn riff(b: &[u8]) -> Result<Palette, LoadError> {
    let mut p = Palette::new(PaletteFormat::RiffPal);
    let le16 = |i: usize| b.get(i..i + 2).map(|v| u16::from_le_bytes([v[0], v[1]]) as usize);
    let le32 = |i: usize| b.get(i..i + 4).map(|v| u32::from_le_bytes([v[0], v[1], v[2], v[3]]) as usize);
    let mut at = 12usize;
    while let (Some(id), Some(len)) = (b.get(at..at + 4), le32(at + 4)) {
        let body = at + 8;
        if id == b"data" {
            let room = len.min(b.len().saturating_sub(body)).saturating_sub(4) / 4;
            // Some writers (Krita's, once) put the count big-endian.
            let mut count = le16(body + 2).unwrap_or(0);
            let swapped = (count as u16).swap_bytes() as usize;
            if count > room && swapped <= room {
                count = swapped;
            }
            if count > room {
                p.warnings.push(format!("{count} colours announced, {room} stored"));
            }
            for i in 0..count.min(room) {
                let c = &b[body + 4 + i * 4..body + 8 + i * 4];
                p.push(Swatch::new(String::new(), Color::Rgb([c[0], c[1], c[2]].map(|v| v as f32 / 255.0))));
            }
            break;
        }
        // Chunks are padded to an even length.
        at = match body.checked_add(len).and_then(|e| e.checked_add(len & 1)) {
            Some(next) if next > at => next,
            _ => break,
        };
    }
    Ok(p)
}

//endregion
