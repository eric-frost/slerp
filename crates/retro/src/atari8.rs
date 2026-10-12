//! Atari 8-bit pictures: MicroPainter and Micro Illustrator `.mic`
//! (Graphics 15, 160x192 in 4 colours), `.gr9` (Graphics 9, 80x192 in 16
//! shades of one hue) and `.gr8` (Graphics 8, 320x192 in two shades).
//!
//! All of them are 7680 bytes of screen memory, 40 bytes a line, after an
//! optional 4-byte trailer of colour registers (`.mic` only).  The 8-bit
//! Atari's colours are a hue (0 to 15) and a luminance (0 to 15); the
//! palette here is computed from the NTSC colour wheel rather than copied
//! from any emulator, so the hues are close rather than exact.

use crate::pic::{Pic, Rgb};
use slerp_scene::LoadError;

/// The Atari colour `hue * 16 + luminance` as RGB.  Hue 0 is grey; hues 1
/// to 15 step round the colour wheel in 24-degree steps, starting at gold.
pub fn colour(value: u8) -> Rgb {
    let (hue, lum) = ((value >> 4) as f32, (value & 15) as f32);
    let y = 0.04 + 0.92 * lum / 15.0;
    let (i, q) = if hue == 0.0 {
        (0.0, 0.0)
    } else {
        let theta = (330.0 + 24.0 * (hue - 1.0)).to_radians();
        (0.28 * theta.cos(), 0.28 * theta.sin())
    };
    let r = y + 0.956 * i + 0.621 * q;
    let g = y - 0.272 * i - 0.647 * q;
    let b = y - 1.106 * i + 1.703 * q;
    let to8 = |v: f32| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
    [to8(r), to8(g), to8(b)]
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Graphics 15 (`.mic`).
    Gr15,
    /// Graphics 9 (`.gr9`).
    Gr9,
    /// Graphics 8 (`.gr8`).
    Gr8,
}

impl Mode {
    pub fn from_extension(ext: &str) -> Option<Mode> {
        Some(match ext {
            "mic" | "gr15" | "g15" => Mode::Gr15,
            "gr9" => Mode::Gr9,
            "gr8" => Mode::Gr8,
            _ => return None,
        })
    }
}

pub fn plausible(len: u64) -> bool {
    matches!(len, 7680 | 7684 | 7685)
}

/// Draws a 7680-byte screen.  Colour registers come from where the writer
/// put them: MicroPainter's `.mic` appends four bytes (background, then
/// registers 708, 709, 710); `.g15` files start with five (registers
/// 708 to 712).  Without them the OS defaults are used: orange, green, blue
/// on black.
pub fn decode(mode: Mode, bytes: &[u8]) -> Result<(Pic, String), LoadError> {
    let (screen, regs) = match bytes.len() {
        7685 => (&bytes[5..], Some([bytes[4], bytes[0], bytes[1], bytes[2]])),
        7684 => (&bytes[..7680], Some([bytes[7680], bytes[7681], bytes[7682], bytes[7683]])),
        n if n >= 7680 => (&bytes[..7680], None),
        _ => return Err(LoadError::invalid("an Atari 8-bit screen is 7680 bytes")),
    };
    // Background first, then the colours of pixel values 1, 2 and 3.
    let regs = regs.unwrap_or([0x00, 0x28, 0xca, 0x94]);
    let width = match mode {
        Mode::Gr15 => 160,
        Mode::Gr9 => 80,
        Mode::Gr8 => 320,
    };
    let mut pic = Pic::new(width, 192);
    let (note, stretch) = match mode {
        Mode::Gr15 => {
            for y in 0..192usize {
                for x in 0..160usize {
                    let v = (screen[y * 40 + x / 4] >> (6 - 2 * (x % 4))) & 3;
                    pic.set(x as u32, y as u32, colour(regs[v as usize]));
                }
            }
            ("Graphics 15, 160x192 in 4 colours", 2)
        }
        Mode::Gr9 => {
            // The hue is the background register's; 16 luminances.
            let hue = regs[0] & 0xf0;
            for y in 0..192usize {
                for x in 0..80usize {
                    let v = (screen[y * 40 + x / 2] >> (4 * (1 - x % 2))) & 15;
                    pic.set(x as u32, y as u32, colour(hue | v));
                }
            }
            ("Graphics 9, 80x192 in 16 shades", 4)
        }
        Mode::Gr8 => {
            let (paper, ink) = (colour(0x94), colour(0x9a));
            for y in 0..192usize {
                for x in 0..320usize {
                    let on = (screen[y * 40 + x / 8] >> (7 - x % 8)) & 1 == 1;
                    pic.set(x as u32, y as u32, if on { ink } else { paper });
                }
            }
            ("Graphics 8, 320x192 in two shades", 1)
        }
    };
    let colours = if regs_given(bytes.len()) { "colours from the file" } else { "default colours" };
    Ok((pic.stretch(stretch, 1), format!("{note}, {colours}, computed NTSC palette")))
}

fn regs_given(len: usize) -> bool {
    len == 7684 || len == 7685
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn palette_is_grey_for_hue_zero() {
        assert_eq!(colour(0x00), [10, 10, 10]);
        let top = colour(0x0f);
        assert_eq!(top[0], top[1]);
        assert!(top[0] > 240);
        // A gold hue is warm: red above blue.
        let gold = colour(0x1a);
        assert!(gold[0] > gold[2]);
        // Hue 7 is blue.
        let blue = colour(0x76);
        assert!(blue[2] > blue[0] && blue[2] > blue[1]);
    }

    #[test]
    fn graphics_15_has_four_pixels_per_byte() {
        // Pixels 1, 2, 3, 0 in the first byte; trailer: background 0x84 and
        // registers 708 to 710.
        let mut f = vec![0u8; 7684];
        f[0] = 0b01_10_11_00;
        f[7680..].copy_from_slice(&[0x84, 0x0f, 0x38, 0x68]);
        let (pic, detail) = decode(Mode::Gr15, &f).unwrap();
        assert_eq!((pic.width, pic.height), (320, 192));
        assert_eq!(pic.get(0, 0), colour(0x0f));
        assert_eq!(pic.get(2, 0), colour(0x38));
        assert_eq!(pic.get(4, 0), colour(0x68));
        assert_eq!(pic.get(6, 0), colour(0x84));
        assert!(detail.contains("from the file"));
        // A `.g15` file keeps the five registers in front.
        let mut g = vec![0x0f, 0x38, 0x68, 0, 0x84];
        g.extend(&f[..7680]);
        let (pic, _) = decode(Mode::Gr15, &g).unwrap();
        assert_eq!(pic.get(0, 0), colour(0x0f));
        assert_eq!(pic.get(6, 0), colour(0x84));
    }

    #[test]
    fn graphics_9_and_8_sizes() {
        let (pic, _) = decode(Mode::Gr9, &[0x5a; 7680]).unwrap();
        assert_eq!((pic.width, pic.height), (320, 192));
        assert_eq!(pic.get(0, 0), colour(5));
        assert_eq!(pic.get(4, 0), colour(10));
        let (pic, _) = decode(Mode::Gr8, &[0x80; 7680]).unwrap();
        assert_eq!((pic.width, pic.height), (320, 192));
        assert_ne!(pic.get(0, 0), pic.get(1, 0));
        assert!(decode(Mode::Gr8, &[0; 10]).is_err());
    }
}
