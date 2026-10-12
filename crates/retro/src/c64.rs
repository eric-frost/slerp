//! Commodore 64 bitmap pictures: Koala Painter (`.koa`, `.kla`), Art
//! Studio (`.art`, `.aas`), Doodle (`.dd`, `.jj`) and Advanced Art Studio
//! (`.ocp`), plus the loosely specified `.hir` and `.hpi` hires dumps.
//!
//! The VIC-II draws a 320x200 bitmap from three tables: the bitmap itself
//! (8000 bytes, one 8-byte tile per 8x8 cell), the screen RAM (1000 bytes,
//! two colours per cell) and, for multicolour pictures, the colour RAM
//! (1000 bytes, a third colour per cell).  Hires pictures use 1 bit per
//! pixel with an ink and a paper colour per cell; multicolour pictures use
//! 2 bits per pixel on half as many, twice as wide, pixels.
//!
//! The colours are Philip "Pepto" Timmermann's 2001 PAL measurement of the
//! VIC-II (<https://www.pepto.de/projects/colorvic/>), the palette VICE
//! calls "Pepto (PAL)".  A palette is a table of measured numbers, which
//! are facts rather than creative work, so there is nothing to license.

use crate::pic::{Pic, Rgb, check_size, slice};
use slerp_scene::LoadError;

/// The C64's 16 colours.
pub const PALETTE: [Rgb; 16] = [
    [0x00, 0x00, 0x00],
    [0xff, 0xff, 0xff],
    [0x68, 0x37, 0x2b],
    [0x70, 0xa4, 0xb2],
    [0x6f, 0x3d, 0x86],
    [0x58, 0x8d, 0x43],
    [0x35, 0x28, 0x79],
    [0xb8, 0xc7, 0x6f],
    [0x6f, 0x4f, 0x25],
    [0x43, 0x39, 0x00],
    [0x9a, 0x67, 0x59],
    [0x44, 0x44, 0x44],
    [0x6c, 0x6c, 0x6c],
    [0x9a, 0xd2, 0x84],
    [0x6c, 0x5e, 0xb5],
    [0x95, 0x95, 0x95],
];

const BITMAP: usize = 8000;
const CELLS: usize = 1000;

/// The picture formats this module reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Koala,
    ArtStudio,
    Doodle,
    AdvancedArtStudio,
    /// `.hir` and `.hpi`: hires bitmaps whose layout is guessed from the size.
    Hires,
}

impl Kind {
    pub fn name(self) -> &'static str {
        match self {
            Kind::Koala => "Koala Painter",
            Kind::ArtStudio => "Art Studio",
            Kind::Doodle => "Doodle",
            Kind::AdvancedArtStudio => "Advanced Art Studio",
            Kind::Hires => "C64 hires bitmap",
        }
    }
}

/// Whether `len` bytes can be a picture of this kind.
pub fn plausible(kind: Kind, len: u64) -> bool {
    match kind {
        Kind::Koala => matches!(len, 10001 | 10003..=10005),
        Kind::ArtStudio => (9002..=9009).contains(&len),
        Kind::Doodle => len == 9218,
        Kind::AdvancedArtStudio => len == 10018,
        Kind::Hires => (9002..=9009).contains(&len) || len == 9218,
    }
}

/// Where each table sits in a file.
struct Layout {
    bitmap: usize,
    screen: usize,
    colour: Option<usize>,
    background: Option<usize>,
    multicolour: bool,
    /// Whether the file starts with a two-byte load address.
    address: bool,
    /// A remark for the inspector, when the layout was guessed.
    note: &'static str,
}

fn layout(kind: Kind, len: usize) -> Layout {
    let hires = |bitmap, screen, note| Layout {
        bitmap,
        screen,
        colour: None,
        background: None,
        multicolour: false,
        address: true,
        note,
    };
    match kind {
        Kind::Koala => {
            // 10003 bytes with the load address $6000 (a few writers leave
            // up to two bytes of padding after), or 10001 without it.
            let skip = if len == 10001 { 0 } else { 2 };
            Layout {
                bitmap: skip,
                screen: skip + BITMAP,
                colour: Some(skip + BITMAP + CELLS),
                background: Some(skip + BITMAP + 2 * CELLS),
                multicolour: true,
                address: skip == 2,
                note: "",
            }
        }
        Kind::ArtStudio => hires(2, 2 + BITMAP, ""),
        Kind::Doodle => hires(2 + 1024, 2, ""),
        Kind::AdvancedArtStudio => Layout {
            bitmap: 2,
            screen: 2 + BITMAP,
            // The tables of the $2000-$471F memory image: border at $4328,
            // background at $4329, colour RAM at $4338.
            colour: Some(2 + 0x2338),
            background: Some(2 + 0x2329),
            multicolour: true,
            address: true,
            note: "",
        },
        // Size decides: a Doodle dump keeps the screen first, the others the
        // bitmap first.
        Kind::Hires if len == 9218 => hires(2 + 1024, 2, "layout guessed from the size"),
        Kind::Hires => hires(2, 2 + BITMAP, "layout guessed from the size"),
    }
}

/// Draws a picture.  Returns it with a one-line description.
pub fn decode(kind: Kind, bytes: &[u8]) -> Result<(Pic, String), LoadError> {
    let l = layout(kind, bytes.len());
    let bitmap = slice(bytes, l.bitmap, BITMAP, "the bitmap")?;
    let screen = slice(bytes, l.screen, CELLS, "the screen RAM")?;
    let colour = match l.colour {
        Some(at) => Some(slice(bytes, at, CELLS, "the colour RAM")?),
        None => None,
    };
    let background = l.background.and_then(|at| bytes.get(at)).map_or(0, |b| b & 15);
    let (w, h) = (320u32, 200u32);
    check_size(w, h)?;
    let mut pic = Pic::new(w, h);
    for y in 0..200usize {
        for cx in 0..40usize {
            let cell = (y / 8) * 40 + cx;
            let byte = bitmap[cell * 8 + (y & 7)];
            let sc = screen[cell];
            if l.multicolour {
                let cr = colour.map_or(0, |c| c[cell] & 15);
                for px in 0..4usize {
                    let bits = (byte >> (6 - 2 * px)) & 3;
                    let c = match bits {
                        0 => background,
                        1 => sc >> 4,
                        2 => sc & 15,
                        _ => cr,
                    };
                    let rgb = PALETTE[c as usize & 15];
                    // A multicolour pixel is two hires pixels wide.
                    pic.set((cx * 8 + px * 2) as u32, y as u32, rgb);
                    pic.set((cx * 8 + px * 2 + 1) as u32, y as u32, rgb);
                }
            } else {
                let (ink, paper) = (PALETTE[(sc >> 4) as usize], PALETTE[(sc & 15) as usize]);
                for px in 0..8usize {
                    let on = (byte >> (7 - px)) & 1 == 1;
                    pic.set((cx * 8 + px) as u32, y as u32, if on { ink } else { paper });
                }
            }
        }
    }
    let mode = if l.multicolour { "multicolour" } else { "hires" };
    let mut detail = format!("320x200, {mode}, Pepto PAL palette");
    if l.address {
        let addr = u16::from_le_bytes([bytes[0], bytes[1]]);
        detail.push_str(&format!(", load address ${addr:04X}"));
    }
    if !l.note.is_empty() {
        detail.push_str(&format!(", {}", l.note));
    }
    Ok((pic, detail))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A Koala file whose cell `(cx, cy)` has its top pixel row set to the
    /// four colour sources in turn.
    fn koala() -> Vec<u8> {
        let mut f = vec![0x00, 0x60];
        let mut bitmap = vec![0u8; BITMAP];
        bitmap[0] = 0b00_01_10_11;
        f.extend(bitmap);
        let mut screen = vec![0u8; CELLS];
        screen[0] = 0x23; // %01 -> red? colour 2, %10 -> colour 3
        f.extend(screen);
        let mut colour = vec![0u8; CELLS];
        colour[0] = 0x0e;
        f.extend(colour);
        f.push(0x06); // background
        f
    }

    #[test]
    fn koala_colours_come_from_four_places() {
        let f = koala();
        assert_eq!(f.len(), 10003);
        let (pic, detail) = decode(Kind::Koala, &f).unwrap();
        assert_eq!((pic.width, pic.height), (320, 200));
        // Background colour 6 (blue), then screen high nibble 2, low
        // nibble 3, then colour RAM 14, each two pixels wide.
        assert_eq!(pic.get(0, 0), PALETTE[6]);
        assert_eq!(pic.get(1, 0), PALETTE[6]);
        assert_eq!(pic.get(2, 0), PALETTE[2]);
        assert_eq!(pic.get(4, 0), PALETTE[3]);
        assert_eq!(pic.get(7, 0), PALETTE[14]);
        // The next pixel row of the cell is all background.
        assert_eq!(pic.get(2, 1), PALETTE[6]);
        assert!(detail.contains("multicolour") && detail.contains("$6000"));
    }

    #[test]
    fn koala_without_load_address() {
        let f = koala();
        let (pic, detail) = decode(Kind::Koala, &f[2..]).unwrap();
        assert_eq!(pic.get(2, 0), PALETTE[2]);
        assert!(!detail.contains("load address"));
    }

    #[test]
    fn art_studio_is_hires_with_ink_and_paper() {
        let mut f = vec![0x00, 0x20];
        let mut bitmap = vec![0u8; BITMAP];
        bitmap[0] = 0b1000_0001;
        f.extend(bitmap);
        let mut screen = vec![0u8; CELLS];
        screen[0] = 0x1b; // ink white, paper dark grey? 1 = white, 0xb = dark grey
        f.extend(screen);
        f.resize(9009, 0);
        let (pic, _) = decode(Kind::ArtStudio, &f).unwrap();
        assert_eq!(pic.get(0, 0), PALETTE[1]);
        assert_eq!(pic.get(1, 0), PALETTE[11]);
        assert_eq!(pic.get(7, 0), PALETTE[1]);
    }

    #[test]
    fn doodle_keeps_the_screen_first() {
        let mut f = vec![0x00, 0x5c];
        let mut screen = vec![0u8; 1024];
        screen[0] = 0x20;
        f.extend(screen);
        let mut bitmap = vec![0u8; 8192];
        bitmap[0] = 0x80;
        f.extend(bitmap);
        assert_eq!(f.len(), 9218);
        let (pic, _) = decode(Kind::Doodle, &f).unwrap();
        assert_eq!(pic.get(0, 0), PALETTE[2]);
        assert_eq!(pic.get(1, 0), PALETTE[0]);
    }

    #[test]
    fn short_files_are_errors_not_panics() {
        for kind in [Kind::Koala, Kind::ArtStudio, Kind::Doodle, Kind::AdvancedArtStudio, Kind::Hires] {
            for len in [0, 1, 2, 100, 8001, 9001] {
                let _ = decode(kind, &vec![7u8; len]);
            }
        }
    }
}
