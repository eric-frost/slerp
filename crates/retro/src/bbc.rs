//! BBC Micro screen memory dumps (`.bbc`), raw or in the VideoNuLA wrapper.
//!
//! The BBC's screen memory is stored character cell by cell: each byte
//! column of a character row holds 8 consecutive bytes, one per pixel line
//! of the cell, so a byte's address is `char_row * 640 + column * 8 +
//! line` (`320` instead of `640` in the 40-column modes 4 and 5).  Pixels
//! are packed in each byte in the way the video ULA wants them:
//!
//! - Mode 0: 640x256, 1 bit per pixel.  Mode 4: 320x256, 1 bit.
//! - Mode 1: 320x256, 4 colours; pixel `p` of a byte is bit `7 - p` plus
//!   twice bit `3 - p`.  Mode 5: 160x256 in the same packing.
//! - Mode 2: 160x256, 16 logical colours (8 physical, 8 flashing ones drawn
//!   in their first colour); pixel `p` of two is bits `7 - p`, `5 - p`,
//!   `3 - p`, `1 - p`.
//!
//! A raw dump says nothing of its mode, so 20480 bytes are read in the mode
//! that gives the most coherent picture, and 10240 bytes as mode 4 or 5 the
//! same way.  The NuLA wrapper (`bbc-nula` by Bitshifters) is a 64-byte
//! header with the mode, 16 BBC colours and 16 colours of NuLA's 12-bit
//! palette, then the screen.  The default palette is black, red, green,
//! yellow, blue, magenta, cyan, white, and mode 1 and 5's four colours are
//! black, red, yellow, white.

use crate::pic::{Pic, Rgb, expand4};
use slerp_scene::LoadError;

const PHYSICAL: [Rgb; 8] =
    [[0, 0, 0], [255, 0, 0], [0, 255, 0], [255, 255, 0], [0, 0, 255], [255, 0, 255], [0, 255, 255], [255, 255, 255]];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    M0,
    M1,
    M2,
    M4,
    M5,
}

impl Mode {
    fn number(self) -> u8 {
        match self {
            Mode::M0 => 0,
            Mode::M1 => 1,
            Mode::M2 => 2,
            Mode::M4 => 4,
            Mode::M5 => 5,
        }
    }

    fn columns(self) -> usize {
        if matches!(self, Mode::M4 | Mode::M5) { 40 } else { 80 }
    }

    fn screen_bytes(self) -> usize {
        self.columns() * 256
    }

    fn pixels_per_byte(self) -> usize {
        match self {
            Mode::M0 | Mode::M4 => 8,
            Mode::M1 | Mode::M5 => 4,
            Mode::M2 => 2,
        }
    }

    fn from_number(n: u8) -> Option<Mode> {
        Some(match n {
            0 => Mode::M0,
            1 => Mode::M1,
            2 => Mode::M2,
            4 => Mode::M4,
            5 => Mode::M5,
            _ => return None,
        })
    }
}

/// Raw dumps are the screen memory of mode 0, 1 or 2 (20480 bytes) or of
/// mode 4 or 5 (10240).
pub fn plausible(len: u64, head: &[u8]) -> bool {
    len == 20480 || len == 10240 || nula(head, len).is_some()
}

/// A NuLA wrapper: version 1, a known mode, and the header plus the
/// mode's screen as the whole file.
fn nula(head: &[u8], len: u64) -> Option<Mode> {
    let mode = Mode::from_number(*head.get(1)?)?;
    (head.first() == Some(&1) && head.len() >= 64 && len == 64 + mode.screen_bytes() as u64).then_some(mode)
}

/// The pen number of each pixel: `columns * pixels_per_byte` wide, 256 tall.
fn pens(mode: Mode, screen: &[u8]) -> (usize, Vec<u8>) {
    let (cols, per) = (mode.columns(), mode.pixels_per_byte());
    let width = cols * per;
    let mut out = vec![0u8; width * 256];
    let bit = |b: u8, n: usize| (b >> n) & 1;
    for y in 0..256usize {
        for c in 0..cols {
            let b = screen.get((y / 8) * cols * 8 + c * 8 + (y & 7)).copied().unwrap_or(0);
            for p in 0..per {
                out[y * width + c * per + p] = match mode {
                    Mode::M0 | Mode::M4 => bit(b, 7 - p),
                    Mode::M1 | Mode::M5 => bit(b, 7 - p) | bit(b, 3 - p) << 1,
                    Mode::M2 => bit(b, 7 - p) | bit(b, 5 - p) << 1 | bit(b, 3 - p) << 2 | bit(b, 1 - p) << 3,
                };
            }
        }
    }
    (width, out)
}

/// How much more often neighbouring pixels match than chance (see the CPC
/// reader, which makes the same guess).
fn coherence(mode: Mode, screen: &[u8]) -> f32 {
    let (w, px) = pens(mode, screen);
    let mut counts = [0u64; 16];
    let (mut same, mut total) = (0u64, 0u64);
    for y in 0..256usize {
        for x in 0..w {
            counts[px[y * w + x] as usize & 15] += 1;
            if x > 0 {
                total += 1;
                same += (px[y * w + x] == px[y * w + x - 1]) as u64;
            }
            if y > 0 {
                total += 1;
                same += (px[y * w + x] == px[(y - 1) * w + x]) as u64;
            }
        }
    }
    let n = (w * 256) as f32;
    let chance: f32 = counts.iter().map(|&c| (c as f32 / n).powi(2)).sum();
    let observed = same as f32 / total.max(1) as f32;
    if chance >= 0.999 { 0.0 } else { (observed - chance) / (1.0 - chance) }
}

fn guess(screen: &[u8]) -> Mode {
    let candidates: &[Mode] =
        if screen.len() == 10240 { &[Mode::M4, Mode::M5] } else { &[Mode::M1, Mode::M2, Mode::M0] };
    // Four-colour modes are the usual ones, and a picture in them reads as
    // a slightly more coherent mode 2 or mode 0 picture of the same data
    // more often than the other way round, so they get a head start.
    let bias = |m: Mode| if matches!(m, Mode::M1 | Mode::M5) { 1.2 } else { 1.0 };
    let mut best = candidates[0];
    let mut best_c = f32::MIN;
    for &m in candidates {
        let c = coherence(m, screen) * bias(m);
        if c > best_c {
            best = m;
            best_c = c;
        }
    }
    best
}

pub fn decode(bytes: &[u8]) -> Result<(Pic, String), LoadError> {
    let (mode, screen, palette, how, pal_note) = match nula(bytes, bytes.len() as u64) {
        Some(mode) => {
            // 16 NuLA entries of two bytes: (index << 4 | red), (green << 4 | blue).
            let mut pal = [[0u8; 3]; 16];
            for i in 0..16 {
                let (a, b) = (bytes[32 + i * 2], bytes[33 + i * 2]);
                pal[(a >> 4) as usize & 15] = [expand4(a & 15), expand4(b >> 4), expand4(b & 15)];
            }
            (mode, &bytes[64..], pal.to_vec(), "mode from the NuLA header", "NuLA palette from the header")
        }
        None => {
            if bytes.len() != 20480 && bytes.len() != 10240 {
                return Err(LoadError::invalid(format!("{} bytes is not a BBC Micro screen", bytes.len())));
            }
            let mode = guess(bytes);
            let pal: Vec<Rgb> = match mode {
                Mode::M0 | Mode::M4 => vec![PHYSICAL[0], PHYSICAL[7]],
                Mode::M1 | Mode::M5 => vec![PHYSICAL[0], PHYSICAL[1], PHYSICAL[3], PHYSICAL[7]],
                Mode::M2 => (0..16).map(|i| PHYSICAL[i & 7]).collect(),
            };
            (mode, bytes, pal, "mode guessed from the picture", "default palette")
        }
    };
    let (width, px) = pens(mode, screen);
    let pic = Pic::from_indexed(width as u32, 256, &px, &palette);
    // Modes 0 and 4 pixels are half as wide as tall on the monitor.
    let pic = match mode {
        Mode::M0 => pic.stretch(1, 2),
        Mode::M2 | Mode::M5 => pic.stretch(2, 1),
        _ => pic,
    };
    Ok((pic, format!("mode {}, {how}, {pal_note}", mode.number())))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn character_cell_layout() {
        // Mode 1: byte column 1 of character row 1, line 2 is at
        // 1 * 640 + 1 * 8 + 2.
        let mut s = vec![0u8; 20480];
        // Pixel 0: bit 7 (low) and bit 3 (high) = pen 3.
        s[640 + 8 + 2] = 0b1000_1000;
        let (w, px) = pens(Mode::M1, &s);
        assert_eq!(w, 320);
        assert_eq!(px[(8 + 2) * 320 + 4], 3);
        assert_eq!(px[(8 + 2) * 320 + 5], 0);
    }

    #[test]
    fn pixel_packing_per_mode() {
        let mut px = [0u8; 8];
        let one = |mode, byte: u8| {
            let mut s = vec![0u8; if matches!(mode, Mode::M4 | Mode::M5) { 10240 } else { 20480 }];
            s[0] = byte;
            let (_, p) = pens(mode, &s);
            p
        };
        px[..8].copy_from_slice(&one(Mode::M0, 0b1010_0001)[..8]);
        assert_eq!(px, [1, 0, 1, 0, 0, 0, 0, 1]);
        // Mode 2: pixel 1 takes bits 6, 4, 2, 0.
        let p = one(Mode::M2, 0b0100_0001);
        assert_eq!(&p[..2], &[0, 1 | 8]);
        let p = one(Mode::M5, 0b0100_0000);
        assert_eq!(&p[..4], &[0, 1, 0, 0]);
    }

    #[test]
    fn nula_wrapper_sets_mode_and_palette() {
        let mut f = vec![0u8; 64 + 20480];
        f[0] = 1;
        f[1] = 2;
        // NuLA entry for logical colour 1: red 15, green 0, blue 0.
        f[32 + 2] = 0x1f;
        f[33 + 2] = 0x00;
        f[64] = 0b1000_0000; // pixel 0 of the first byte: pen 1
        assert!(plausible(f.len() as u64, &f));
        let (pic, detail) = decode(&f).unwrap();
        assert_eq!((pic.width, pic.height), (320, 256));
        assert_eq!(pic.get(0, 0), [255, 0, 0]);
        assert!(detail.contains("NuLA"));
    }

    #[test]
    fn raw_dumps_are_guessed() {
        // A flat mode 1 picture: left half pen 1, right half pen 2.
        let mut s = vec![0u8; 20480];
        for y in 0..256usize {
            for c in 0..80usize {
                s[(y / 8) * 640 + c * 8 + (y & 7)] = if c < 40 { 0b1111_0000 } else { 0b0000_1111 };
            }
        }
        let (pic, detail) = decode(&s).unwrap();
        assert!(detail.contains("mode 1"));
        assert_eq!(pic.get(0, 0), PHYSICAL[1]);
        assert_eq!(pic.get(319, 0), [255, 255, 0]);
        assert!(decode(&[0; 100]).is_err());
        assert!(!plausible(100, &[]));
    }
}
