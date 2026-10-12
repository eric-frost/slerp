//! ZX Spectrum screen dumps (`.scr`).
//!
//! The Spectrum's video memory is 6144 bytes of bitmap, stored in an
//! interleaved order (the three screen thirds, then the eight pixel rows of
//! a character row, then the character rows), followed by 768 attribute
//! bytes, one per 8x8 cell, holding ink, paper, bright and flash.  The
//! size tells the variants apart:
//!
//! - 6912 bytes: the standard screen.
//! - 6144 bytes: a bitmap without attributes, drawn black on white.
//! - 12288 bytes: the Timex 2068's hi-colour mode, 6144 bytes of bitmap and
//!   6144 of attributes with one attribute byte per 8x1 pixels.
//! - 12289 bytes: a Timex dump with a final mode byte; hi-res (512x192,
//!   black on white or the colour pair the byte selects) or hi-colour.
//!
//! Flashing cells are drawn in their first phase.  The hi-res picture is
//! drawn with every line repeated, since its pixels are half as wide as the
//! standard ones on the same 4:3 screen.

use crate::pic::{Pic, Rgb};
use slerp_scene::LoadError;

/// The 8 colours at normal and at bright intensity, indexed by the
/// Spectrum's GRB bit order (bit 0 blue, bit 1 red, bit 2 green).  Normal
/// intensity is 0xCD per channel, as most emulators use.
pub fn colour(index: u8, bright: bool) -> Rgb {
    let level = if bright { 0xff } else { 0xcd };
    let on = |bit: u8| if index & bit != 0 { level } else { 0 };
    [on(2), on(4), on(1)]
}

/// The offset of byte column `col` of pixel row `y` in the bitmap.
pub fn bitmap_offset(y: usize, col: usize) -> usize {
    ((y & 0xc0) << 5) | ((y & 0x07) << 8) | ((y & 0x38) << 2) | col
}

pub fn plausible(len: u64) -> bool {
    matches!(len, 6144 | 6912 | 12288 | 12289)
}

/// Draws a screen dump chosen by its size.
pub fn decode(bytes: &[u8]) -> Result<(Pic, String), LoadError> {
    match bytes.len() {
        6144 => Ok((draw(bytes, Attr::None), "256x192, bitmap only (6144 bytes), drawn black on white".into())),
        6912 => Ok((
            draw(bytes, Attr::Cells(&bytes[6144..])),
            "256x192, standard screen (6912 bytes), flash shown in its first phase".into(),
        )),
        12288 => Ok((
            draw(bytes, Attr::Rows(&bytes[6144..])),
            "256x192, Timex hi-colour (12288 bytes), a colour pair per 8x1 pixels".into(),
        )),
        12289 => {
            let mode = bytes[12288];
            if mode & 7 == 6 {
                let ink = (mode >> 3) & 7;
                Ok((hires(bytes, ink), format!("512x192, Timex hi-res (12289 bytes), mode byte {mode:#04x}")))
            } else {
                Ok((
                    draw(bytes, Attr::Rows(&bytes[6144..12288])),
                    format!("256x192, Timex hi-colour (12289 bytes), mode byte {mode:#04x}"),
                ))
            }
        }
        n => Err(LoadError::invalid(format!("{n} bytes is not a ZX Spectrum screen size"))),
    }
}

enum Attr<'a> {
    None,
    /// One byte per 8x8 cell (768).
    Cells(&'a [u8]),
    /// One byte per 8x1 row of pixels, laid out like the bitmap (6144).
    Rows(&'a [u8]),
}

fn draw(bytes: &[u8], attr: Attr) -> Pic {
    let mut pic = Pic::new(256, 192);
    for y in 0..192usize {
        for col in 0..32usize {
            let bits = bytes[bitmap_offset(y, col)];
            let a = match attr {
                Attr::None => 0b0011_1000,
                Attr::Cells(c) => c[(y / 8) * 32 + col],
                Attr::Rows(r) => r[bitmap_offset(y, col)],
            };
            let bright = a & 0x40 != 0;
            let (ink, paper) = (colour(a & 7, bright), colour((a >> 3) & 7, bright));
            for bit in 0..8usize {
                let on = bits & (0x80 >> bit) != 0;
                pic.set((col * 8 + bit) as u32, y as u32, if on { ink } else { paper });
            }
        }
    }
    pic
}

/// Timex hi-res: even byte columns come from the first screen, odd ones
/// from the second; ink and paper are complementary.
fn hires(bytes: &[u8], ink: u8) -> Pic {
    let (ink, paper) = (colour(ink, false), colour(7 - ink, false));
    // Plain black on white is the default pair; bright white doesn't occur.
    let mut pic = Pic::new(512, 192);
    for y in 0..192usize {
        for col in 0..64usize {
            let bits = bytes[(col & 1) * 6144 + bitmap_offset(y, col / 2)];
            for bit in 0..8usize {
                let on = bits & (0x80 >> bit) != 0;
                pic.set((col * 8 + bit) as u32, y as u32, if on { ink } else { paper });
            }
        }
    }
    pic.stretch(1, 2)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bitmap_offsets_follow_the_interleave() {
        assert_eq!(bitmap_offset(0, 0), 0);
        assert_eq!(bitmap_offset(1, 0), 256);
        assert_eq!(bitmap_offset(8, 0), 32);
        assert_eq!(bitmap_offset(63, 31), 0x07ff);
        assert_eq!(bitmap_offset(64, 0), 0x0800);
        assert_eq!(bitmap_offset(191, 31), 0x17ff);
    }

    #[test]
    fn standard_screen_reads_attributes() {
        let mut f = vec![0u8; 6912];
        // Pixel (3, 1): row 1 is 256 bytes in; bit 3 from the left.
        f[256] = 0x10;
        // Cell (0, 0): bright, paper blue (1), ink red (2).
        f[6144] = 0x40 | (1 << 3) | 2;
        let (pic, detail) = decode(&f).unwrap();
        assert_eq!((pic.width, pic.height), (256, 192));
        assert_eq!(pic.get(3, 1), [0xff, 0, 0]);
        assert_eq!(pic.get(0, 0), [0, 0, 0xff]);
        // Another cell keeps attribute 0: black on black.
        assert_eq!(pic.get(100, 100), [0, 0, 0]);
        assert!(detail.contains("6912"));
    }

    #[test]
    fn monochrome_dump_is_black_on_white() {
        let mut f = vec![0u8; 6144];
        f[0] = 0x80;
        let (pic, _) = decode(&f).unwrap();
        assert_eq!(pic.get(0, 0), [0, 0, 0]);
        assert_eq!(pic.get(1, 0), [0xcd, 0xcd, 0xcd]);
    }

    #[test]
    fn hi_colour_has_an_attribute_per_row() {
        let mut f = vec![0u8; 12288];
        f[bitmap_offset(1, 0)] = 0x80;
        f[6144 + bitmap_offset(1, 0)] = 2; // ink red on black
        f[6144 + bitmap_offset(0, 0)] = 4; // ink green
        f[bitmap_offset(0, 0)] = 0x80;
        let (pic, _) = decode(&f).unwrap();
        assert_eq!(pic.get(0, 0), [0, 0xcd, 0]);
        assert_eq!(pic.get(0, 1), [0xcd, 0, 0]);
    }

    #[test]
    fn hi_res_interleaves_two_screens_and_doubles_lines() {
        let mut f = vec![0u8; 12289];
        f[0] = 0x80; // first screen, byte 0: leftmost pixel
        f[6144] = 0x80; // second screen, byte 0: pixel 8
        f[12288] = 0b00_000_110; // black on white
        let (pic, _) = decode(&f).unwrap();
        assert_eq!((pic.width, pic.height), (512, 384));
        // Mode bits 3-5 are 0: ink black, paper white.
        assert_eq!(pic.get(0, 0), [0, 0, 0]);
        assert_eq!(pic.get(8, 0), [0, 0, 0]);
        assert_eq!(pic.get(0, 1), [0, 0, 0]);
        assert_eq!(pic.get(1, 0), [0xcd, 0xcd, 0xcd]);
    }

    #[test]
    fn wrong_sizes_are_rejected() {
        assert!(decode(&[0; 100]).is_err());
    }
}
