//! MSX screen dumps: `.sc2` (screen 2), `.sc5`/`.sr5` (screen 5), `.sc6`/
//! `.sr6`, `.sc7`/`.sr7`, `.sc8`/`.sr8` and `.sc12`/`.sr12`, saved with
//! BASIC's BSAVE (a 7-byte header starting `0xFE` giving the video memory
//! address range) or as bare video memory.
//!
//! - Screen 2: 256x192, a pattern table and a colour table, each with an
//!   ink and a paper colour per 8x1 row of a tile, from the TMS9918's fixed
//!   16 colours.
//! - Screen 5: 256x212, 16 colours of a 512-colour palette; screen 6:
//!   512x212 in 4 colours; screen 7: 512x212 in 16.  The palette sits in
//!   video memory after the picture when the file includes it, otherwise
//!   the MSX2's power-on palette is used.
//! - Screen 8: 256x212 with fixed 8-bit colours (3 bits green, 3 red, 2
//!   blue).  Screen 12: 256x212 YJK colour, up to 19268 colours.
//!
//! The 512-pixel screens have half-width pixels, so they are drawn with
//! every line repeated.

use crate::pic::{Pic, Rgb, expand3, slice};
use slerp_scene::LoadError;

/// The TMS9918A's colours as measured by openMSX; entry 0 is transparent
/// and shows the backdrop, drawn black.
pub const TMS9918: [Rgb; 16] = [
    [0x00, 0x00, 0x00],
    [0x00, 0x00, 0x00],
    [0x21, 0xc8, 0x42],
    [0x5e, 0xdc, 0x78],
    [0x54, 0x55, 0xed],
    [0x7d, 0x76, 0xfc],
    [0xd4, 0x52, 0x4d],
    [0x42, 0xeb, 0xf5],
    [0xfc, 0x55, 0x54],
    [0xff, 0x79, 0x78],
    [0xd4, 0xc1, 0x54],
    [0xe6, 0xce, 0x80],
    [0x21, 0xb0, 0x3b],
    [0xc9, 0x5b, 0xba],
    [0xcc, 0xcc, 0xcc],
    [0xff, 0xff, 0xff],
];

/// The MSX2's power-on palette, as red, green and blue levels from 0 to 7.
const MSX2_DEFAULT: [[u8; 3]; 16] = [
    [0, 0, 0],
    [0, 0, 0],
    [1, 6, 1],
    [3, 7, 3],
    [1, 1, 7],
    [2, 3, 7],
    [5, 1, 1],
    [2, 6, 7],
    [7, 1, 1],
    [7, 3, 3],
    [6, 6, 1],
    [6, 6, 4],
    [1, 4, 1],
    [6, 2, 5],
    [5, 5, 5],
    [7, 7, 7],
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Screen {
    S2,
    S5,
    S6,
    S7,
    S8,
    S12,
}

impl Screen {
    pub fn from_extension(ext: &str) -> Option<Screen> {
        Some(match ext {
            "sc2" | "sr2" => Screen::S2,
            "sc5" | "sr5" => Screen::S5,
            "sc6" | "sr6" => Screen::S6,
            "sc7" | "sr7" => Screen::S7,
            "sc8" | "sr8" => Screen::S8,
            "sc12" | "sr12" | "s12" | "srs" => Screen::S12,
            _ => return None,
        })
    }
}

/// The video memory a file holds: the BSAVE header's load address and the
/// bytes after it.
fn video_memory(bytes: &[u8]) -> (usize, &[u8]) {
    if bytes.first() == Some(&0xfe) && bytes.len() > 7 {
        let start = u16::from_le_bytes([bytes[1], bytes[2]]) as usize;
        return (start, &bytes[7..]);
    }
    (0, bytes)
}

/// A rough check that the file is a dump of this screen: right extension
/// and enough bytes.
pub fn plausible(screen: Screen, len: u64) -> bool {
    let min = match screen {
        Screen::S2 => 0x1800 + 0x1800 - 7,
        // Partial pictures are fine: at least a few lines.
        Screen::S5 | Screen::S6 | Screen::S7 | Screen::S8 | Screen::S12 => 1024,
    };
    len >= min as u64 && len <= 0x20000
}

/// A Graph Saurus palette file (`.pl5`, `.pl6`, `.pl7`) holds the 16 (or 4)
/// two-byte palette entries as a BSAVE of video memory at 0x1B80.
pub fn palette_extension(screen: Screen) -> Option<&'static str> {
    match screen {
        Screen::S5 => Some("pl5"),
        Screen::S6 => Some("pl6"),
        Screen::S7 => Some("pl7"),
        _ => None,
    }
}

pub fn decode(screen: Screen, bytes: &[u8], palette_file: Option<&[u8]>) -> Result<(Pic, String), LoadError> {
    let (start, data) = video_memory(bytes);
    // Pad the dump out to where it sits in video memory.
    let mut vram = vec![0u8; start.min(0x10000)];
    vram.extend_from_slice(data);
    // The palette file, when beside the picture, overrides what the picture
    // holds: copy its entries to where the picture's own palette would be.
    let companion = palette_file.and_then(|p| {
        let (at, raw) = video_memory(p);
        (at == 0x1b80 && raw.len() >= 4).then(|| raw[..raw.len().min(32)].to_vec())
    });
    // 256 lines when the file holds them, else 212, else as many as it has
    // (Graph Saurus saves partial pictures).
    let lines = |bytes_per_line: usize| match data.len() / bytes_per_line {
        n if n >= 256 => 256,
        n if n >= 212 => 212,
        n => n.max(1),
    };
    let (pic, detail) = match screen {
        Screen::S2 => screen2(&vram),
        Screen::S5 => bitmap4(&vram, 256, 4, 0x7680, "screen 5", lines(128), companion.as_deref()),
        Screen::S6 => bitmap4(&vram, 512, 2, 0x7680, "screen 6", lines(128), companion.as_deref()),
        Screen::S7 => bitmap4(&vram, 512, 4, 0xfa80, "screen 7", lines(256), companion.as_deref()),
        Screen::S8 => screen8(&vram, lines(256)),
        Screen::S12 => screen12(&vram, lines(256)),
    }?;
    Ok((pic, detail))
}

fn screen2(vram: &[u8]) -> Result<(Pic, String), LoadError> {
    let patterns = slice(vram, 0, 0x1800, "the pattern table")?;
    let colours = slice(vram, 0x2000, 0x1800, "the colour table")?;
    // The name table says which pattern each of the 768 tiles shows; a dump
    // taken before a program set it up has zeros, meaning the usual
    // numbering 0-255 in each third.
    let names = vram.get(0x1800..0x1b00).filter(|n| n.iter().any(|&b| b != 0));
    let mut pic = Pic::new(256, 192);
    for t in 0..768usize {
        let (tx, ty) = (t % 32, t / 32);
        let n = names.map_or(t % 256, |n| n[t] as usize);
        let base = (ty / 8) * 0x800 + n * 8;
        for r in 0..8usize {
            let (p, c) = (patterns[base + r], colours[base + r]);
            let (ink, paper) = (TMS9918[(c >> 4) as usize], TMS9918[(c & 15) as usize]);
            for b in 0..8usize {
                pic.set((tx * 8 + b) as u32, (ty * 8 + r) as u32, if p & (0x80 >> b) != 0 { ink } else { paper });
            }
        }
    }
    let note = if names.is_some() { "name table read" } else { "name table empty, tiles numbered in order" };
    Ok((pic, format!("256x192, screen 2, TMS9918 colours, {note}")))
}

/// A palette from video memory: two bytes per colour, red and blue in the
/// first (`0RRR0BBB`), green in the second (`00000GGG`).
fn palette(vram: &[u8], at: usize) -> (Vec<Rgb>, bool) {
    match vram.get(at..at + 32) {
        Some(p) if p.iter().any(|&b| b != 0) => {
            let pal =
                p.chunks_exact(2).map(|c| [expand3((c[0] >> 4) & 7), expand3(c[1] & 7), expand3(c[0] & 7)]).collect();
            (pal, true)
        }
        _ => (MSX2_DEFAULT.iter().map(|c| [expand3(c[0]), expand3(c[1]), expand3(c[2])]).collect(), false),
    }
}

/// Screens 5, 6 and 7: 4 bits (screens 5, 7) or 2 bits (screen 6) per pixel,
/// high bits first, 212 lines.
fn bitmap4(
    vram: &[u8],
    width: usize,
    bits: usize,
    pal_at: usize,
    name: &str,
    lines: usize,
    companion: Option<&[u8]>,
) -> Result<(Pic, String), LoadError> {
    let per_byte = 8 / bits;
    let line = width / per_byte;
    let data = slice(vram, 0, line * lines, "the picture")?;
    let (pal, stored) = match companion {
        Some(c) => {
            let mut v = vec![0u8; 32];
            v[..c.len()].copy_from_slice(c);
            palette(&v, 0)
        }
        None => palette(vram, pal_at),
    };
    let mut idx = vec![0u8; width * lines];
    for y in 0..lines {
        for x in 0..width {
            let b = data[y * line + x / per_byte];
            let shift = 8 - bits * (x % per_byte + 1);
            idx[y * width + x] = (b >> shift) & ((1 << bits) - 1);
        }
    }
    let pic = Pic::from_indexed(width as u32, lines as u32, &idx, &pal);
    let pic = if width == 512 { pic.stretch(1, 2) } else { pic };
    let source = if companion.is_some() {
        "palette from the .pl file beside it"
    } else if stored {
        "palette from the file"
    } else {
        "MSX2 default palette"
    };
    Ok((pic, format!("{width}x{lines}, {name}, {} colours, {source}", 1 << bits)))
}

/// Screen 8: one byte per pixel as `GGGRRRBB`.
fn screen8(vram: &[u8], lines: usize) -> Result<(Pic, String), LoadError> {
    let data = slice(vram, 0, 256 * lines, "the picture")?;
    let mut pic = Pic::new(256, lines as u32);
    for (i, &v) in data.iter().enumerate() {
        let (g, r, b) = (v >> 5, (v >> 2) & 7, v & 3);
        pic.set((i % 256) as u32, (i / 256) as u32, [expand3(r), expand3(g), b * 85]);
    }
    Ok((pic, format!("256x{lines}, screen 8, fixed 256 colours")))
}

/// Screen 12: groups of four pixels sharing a chroma pair (J and K, six
/// bits each, split over the low three bits of the four bytes) with a 5-bit
/// luminance Y of their own in the high bits.
fn screen12(vram: &[u8], lines: usize) -> Result<(Pic, String), LoadError> {
    let data = slice(vram, 0, 256 * lines, "the picture")?;
    let mut pic = Pic::new(256, lines as u32);
    let signed6 = |v: i32| if v >= 32 { v - 64 } else { v };
    for (g, group) in data.chunks_exact(4).enumerate() {
        let k = signed6((group[0] & 7) as i32 | ((group[1] & 7) as i32) << 3);
        let j = signed6((group[2] & 7) as i32 | ((group[3] & 7) as i32) << 3);
        for (n, &byte) in group.iter().enumerate() {
            let y = (byte >> 3) as i32;
            let r = (y + j).clamp(0, 31);
            let gr = (y + k).clamp(0, 31);
            let b = ((5 * y - 2 * j - k) / 4).clamp(0, 31);
            let scale = |v: i32| ((v << 3) | (v >> 2)) as u8;
            pic.set(((g * 4 + n) % 256) as u32, ((g * 4 + n) / 256) as u32, [scale(r), scale(gr), scale(b)]);
        }
    }
    Ok((pic, format!("256x{lines}, screen 12, YJK colour")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn screen5_reads_nibbles_and_the_stored_palette() {
        // A Graph Saurus "GE5" file: 212 lines of picture, the palette at
        // 0x7680 and the file ending at 0x769F.
        let mut vram = vec![0u8; 0x76a0];
        vram[0] = 0x21; // pixels 0 and 1: colours 2 and 1
        vram[1] = 0xf0;
        // Palette entry 2: R7 B0 G0 -> red; entry 1: R0 B7 G7.
        vram[0x7680 + 4] = 0x70;
        vram[0x7680 + 5] = 0x00;
        vram[0x7680 + 2] = 0x07;
        vram[0x7680 + 3] = 0x07;
        let mut file = vec![0xfe, 0, 0, 0x9f, 0x76, 0, 0];
        file.extend(&vram);
        let (pic, detail) = decode(Screen::S5, &file, None).unwrap();
        assert_eq!((pic.width, pic.height), (256, 212));
        assert_eq!(pic.get(0, 0), [255, 0, 0]);
        assert_eq!(pic.get(1, 0), [0, 255, 255]);
        assert!(detail.contains("palette from the file"));
    }

    #[test]
    fn screen5_without_a_palette_uses_the_msx2_defaults() {
        let mut vram = vec![0u8; 0x6a00];
        vram[0] = 0xf0;
        let (pic, detail) = decode(Screen::S5, &vram, None).unwrap();
        assert_eq!(pic.get(0, 0), [255, 255, 255]);
        assert!(detail.contains("default"));
    }

    #[test]
    fn a_pl_file_beside_the_picture_sets_the_palette_and_256_lines_are_kept() {
        let mut vram = vec![0u8; 128 * 256];
        vram[0] = 0x10; // pixel 0 is colour 1
        // BSAVE of video memory at 0x1B80: entry 1 = red 7, blue 0, green 0.
        let mut pl = vec![0xfe, 0x80, 0x1b, 0x9f, 0x1b, 0, 0];
        pl.extend([0, 0, 0x70, 0]);
        let (pic, detail) = decode(Screen::S5, &vram, Some(&pl)).unwrap();
        assert_eq!((pic.width, pic.height), (256, 256));
        assert_eq!(pic.get(0, 0), [255, 0, 0]);
        assert_eq!(pic.get(1, 0), [0, 0, 0]);
        assert!(detail.contains(".pl file"));
    }

    #[test]
    fn screen6_and_7_double_lines() {
        let vram = vec![0u8; 0xd400 + 8];
        let (pic, _) = decode(Screen::S7, &vram, None).unwrap();
        assert_eq!((pic.width, pic.height), (512, 424));
        let vram = vec![0u8; 0x6a00];
        let (pic, detail) = decode(Screen::S6, &vram, None).unwrap();
        assert_eq!((pic.width, pic.height), (512, 424));
        assert!(detail.contains("4 colours"));
    }

    #[test]
    fn screen8_colours_are_grb() {
        let mut vram = vec![0u8; 256 * 212];
        vram[0] = 0xe0; // green 7
        vram[1] = 0x1c; // red 7
        vram[2] = 0x03; // blue 3
        let (pic, _) = decode(Screen::S8, &vram, None).unwrap();
        assert_eq!(pic.get(0, 0), [0, 255, 0]);
        assert_eq!(pic.get(1, 0), [255, 0, 0]);
        assert_eq!(pic.get(2, 0), [0, 0, 255]);
    }

    #[test]
    fn screen2_uses_tile_patterns_and_colours() {
        let mut vram = vec![0u8; 0x4000];
        // Tile 1 (second tile on the first row), row 0: ink light blue (5),
        // paper dark red (6).
        vram[8] = 0b1000_0000;
        vram[0x2000 + 8] = 0x56;
        let (pic, detail) = decode(Screen::S2, &vram, None).unwrap();
        assert_eq!(pic.get(8, 0), TMS9918[5]);
        assert_eq!(pic.get(9, 0), TMS9918[6]);
        assert!(detail.contains("tiles numbered in order"));
        // A name table that maps tile 0 of the screen to pattern 1.
        vram[0x1800] = 1;
        let (pic, detail) = decode(Screen::S2, &vram, None).unwrap();
        assert_eq!(pic.get(0, 0), TMS9918[5]);
        assert!(detail.contains("name table read"));
    }

    #[test]
    fn yjk_grey_needs_equal_j_and_k() {
        // Luminance 14 with J = K = 2 is grey 16 in all three channels (YJK
        // stores Y = R/4 + G/2 + B/8, so greys have small positive J and K).
        let group = [(14u8 << 3) | 2, 14 << 3, (14 << 3) | 2, 14 << 3];
        let vram: Vec<u8> = group.iter().copied().cycle().take(256 * 212).collect();
        let (pic, _) = decode(Screen::S12, &vram, None).unwrap();
        let c = pic.get(5, 5);
        assert_eq!(c[0], c[1]);
        assert_eq!(c[1], c[2]);
        assert_eq!(c[0], 132);
    }

    #[test]
    fn short_files_fail_cleanly() {
        for s in [Screen::S2, Screen::S5, Screen::S6, Screen::S7, Screen::S8, Screen::S12] {
            assert!(decode(s, &[0xfe, 0, 0, 1, 0, 0, 0, 1, 2], None).is_err());
            assert!(decode(s, &[], None).is_err());
        }
    }
}
