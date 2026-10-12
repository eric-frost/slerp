//! Amstrad CPC screen dumps (`.scr`).
//!
//! The CPC's 16 KB screen memory interleaves eight blocks of 2 KB, each
//! holding every eighth pixel line, 80 bytes a line.  A byte holds 2 pixels
//! in mode 0 (16 pens), 4 in mode 1 (4 pens) or 8 in mode 2 (2 pens), with
//! the bits of each pixel's pen number scattered across it.  The file says
//! nothing of the mode or the palette, which live in the hardware registers,
//! so they come from a `.pal` file beside the picture when there is one
//! (OCP Art Studio's palette file) and otherwise the mode is guessed from
//! how smooth the picture comes out in each, with the firmware's default
//! palette for that mode.
//!
//! Pixels are drawn at the shape of the CPC's monitor: mode 0 pixels are
//! twice as wide as mode 1's, and mode 2's half as wide again, so mode 0 is
//! drawn 320 wide and mode 2 with every line repeated.

use crate::pic::{Pic, Rgb};
use slerp_scene::LoadError;

/// The 27 colours the gate array can make, by firmware ink number: three
/// levels (0, half, full) of green, red and blue, with green the slowest
/// digit and blue the fastest.
pub fn ink(n: u8) -> Rgb {
    let n = (n % 27) as usize;
    let level = |v: usize| [0u8, 0x80, 0xff][v];
    [level(n / 3 % 3), level(n / 9), level(n % 3)]
}

/// Default pens at power-on, as firmware ink numbers: mode 0 has 16,
/// mode 1 four, mode 2 two.
const DEFAULT_PENS: [u8; 16] = [1, 24, 20, 6, 26, 0, 2, 8, 10, 12, 14, 16, 18, 22, 1, 24];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    M0,
    M1,
    M2,
}

impl Mode {
    fn pens(self) -> usize {
        match self {
            Mode::M0 => 16,
            Mode::M1 => 4,
            Mode::M2 => 2,
        }
    }

    fn number(self) -> u8 {
        self as u8
    }
}

pub fn plausible(len: u64) -> bool {
    matches!(len, 16384 | 16512)
}

/// The byte offset of line `y`, column `c`.
pub fn offset(y: usize, c: usize) -> usize {
    (y & 7) * 2048 + (y >> 3) * 80 + c
}

/// Pen numbers of the pixels in `byte`.
fn pixels(mode: Mode, byte: u8, out: &mut [u8; 8]) -> usize {
    let bit = |n: u32| (byte >> n) & 1;
    match mode {
        Mode::M0 => {
            out[0] = bit(7) | bit(3) << 1 | bit(5) << 2 | bit(1) << 3;
            out[1] = bit(6) | bit(2) << 1 | bit(4) << 2 | bit(0) << 3;
            2
        }
        Mode::M1 => {
            for p in 0..4u32 {
                out[p as usize] = bit(7 - p) | bit(3 - p) << 1;
            }
            4
        }
        Mode::M2 => {
            for p in 0..8u32 {
                out[p as usize] = bit(7 - p);
            }
            8
        }
    }
}

/// Pen numbers for the whole picture: 200 lines of `80 * pixels per byte`.
fn pens(mode: Mode, screen: &[u8]) -> (usize, Vec<u8>) {
    let mut px = [0u8; 8];
    let per = pixels(mode, 0, &mut px);
    let width = 80 * per;
    let mut out = vec![0u8; width * 200];
    for y in 0..200usize {
        for c in 0..80usize {
            pixels(mode, screen[offset(y, c)], &mut px);
            for p in 0..per {
                out[y * width + c * per + p] = px[p];
            }
        }
    }
    (width, out)
}

/// How coherent a picture is when read in `mode`: how much more often
/// neighbouring pixels (left and above) match than two pixels drawn at
/// random from the same pens would.  In its own mode a picture has flat
/// areas and edges; in another mode the pen bits are scrambled and
/// neighbours match no more often than chance.  Coherence is 0 for noise
/// and 1 for a flat picture.
fn coherence(mode: Mode, screen: &[u8]) -> f32 {
    let (w, px) = pens(mode, screen);
    let mut counts = [0u64; 16];
    let (mut same, mut total) = (0u64, 0u64);
    for y in 0..200usize {
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
    let n = (w * 200) as f32;
    let chance: f32 = counts.iter().map(|&c| (c as f32 / n).powi(2)).sum();
    let observed = same as f32 / total.max(1) as f32;
    if chance >= 0.999 { 0.0 } else { (observed - chance) / (1.0 - chance) }
}

/// The most plausible mode: the most coherent reading.
pub fn guess_mode(screen: &[u8]) -> Mode {
    let c = [Mode::M0, Mode::M1, Mode::M2].map(|m| coherence(m, screen));
    let mut best = Mode::M1;
    let mut best_c = c[1];
    for (m, v) in [(Mode::M0, c[0]), (Mode::M2, c[2])] {
        if v > best_c {
            best = m;
            best_c = v;
        }
    }
    best
}

/// The gate array's hardware colour numbers (0 to 31) as firmware ink
/// numbers (0 to 26), for palette files that store what the hardware takes.
const HARDWARE_TO_FIRMWARE: [u8; 32] = [
    13, 13, 19, 25, 1, 7, 10, 16, 7, 25, 24, 26, 6, 8, 15, 17, 1, 19, 18, 20, 0, 2, 9, 11, 4, 22, 21, 23, 3, 5, 12, 14,
];

/// A palette file made by the program that drew the picture: OCP Art
/// Studio's `.pal`, or the 16-byte `.ink` that Martine writes.
pub struct PaletteFile {
    pub mode: Option<Mode>,
    /// The ink of each pen as a firmware number.
    pub pens: [u8; 16],
}

/// Drops an AMSDOS header (a 128-byte block whose checksum word at 67 is the
/// sum of its first 67 bytes) if there is one.
fn strip_amsdos(bytes: &[u8]) -> &[u8] {
    if bytes.len() > 128 && {
        let sum: u32 = bytes[..67].iter().map(|&b| b as u32).sum();
        sum as u16 == u16::from_le_bytes([bytes[67], bytes[68]])
    } {
        &bytes[128..]
    } else {
        bytes
    }
}

/// OCP's `.pal`: a mode byte, two more bytes, then for each of the 16 pens
/// twelve gate-array values (`0x40 | hardware ink`), one per band of lines
/// so that rasters can change colours down the screen; the first band is
/// used.  Martine's `.ink`: 16 hardware ink numbers.
pub fn parse_palette(bytes: &[u8]) -> Option<PaletteFile> {
    let body = strip_amsdos(bytes);
    let mut pens = [0u8; 16];
    if body.len() >= 3 + 16 * 12 && body[0] <= 2 {
        for (p, pen) in pens.iter_mut().enumerate() {
            let v = body[3 + p * 12];
            // Pens the mode doesn't use may hold anything.
            *pen = HARDWARE_TO_FIRMWARE[(v & 0x1f) as usize];
        }
        let mode = [Mode::M0, Mode::M1, Mode::M2][body[0] as usize];
        // Reject files whose used pens aren't gate-array commands.
        let sane = (0..mode.pens()).all(|p| body[3 + p * 12] & 0xe0 == 0x40);
        return sane.then_some(PaletteFile { mode: Some(mode), pens });
    }
    if body.len() == 16 && body.iter().all(|&v| v < 32) {
        for (pen, &v) in pens.iter_mut().zip(body) {
            *pen = HARDWARE_TO_FIRMWARE[v as usize];
        }
        return Some(PaletteFile { mode: None, pens });
    }
    None
}

/// Draws a screen dump.  `palette` is the companion file's bytes if any.
pub fn decode(bytes: &[u8], palette: Option<&[u8]>) -> Result<(Pic, String), LoadError> {
    let screen = match bytes.len() {
        16384 => bytes,
        16512 => &bytes[128..],
        n => return Err(LoadError::invalid(format!("{n} bytes is not a CPC screen"))),
    };
    let file = palette.and_then(parse_palette);
    let (mode, how) = match file.as_ref().and_then(|f| f.mode) {
        Some(m) => (m, "mode from the .pal file"),
        None => (guess_mode(screen), "mode guessed from the picture"),
    };
    let used: Vec<Rgb> = match &file {
        Some(f) => f.pens[..mode.pens()].iter().map(|&p| ink(p)).collect(),
        None => DEFAULT_PENS[..mode.pens()].iter().map(|&p| ink(p)).collect(),
    };
    let (width, px) = pens(mode, screen);
    let pic = Pic::from_indexed(width as u32, 200, &px, &used);
    let pic = match mode {
        Mode::M0 => pic.stretch(2, 1),
        Mode::M1 => pic,
        Mode::M2 => pic.stretch(1, 2),
    };
    let colours = if file.is_some() { "palette from the .pal file" } else { "firmware default palette" };
    Ok((pic, format!("mode {}, {} pens, {how}, {colours}", mode.number(), mode.pens())))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn firmware_inks() {
        assert_eq!(ink(0), [0, 0, 0]);
        assert_eq!(ink(26), [255, 255, 255]);
        assert_eq!(ink(1), [0, 0, 0x80]);
        assert_eq!(ink(6), [255, 0, 0]);
        assert_eq!(ink(9), [0, 0x80, 0]);
        assert_eq!(ink(24), [255, 255, 0]);
    }

    #[test]
    fn lines_are_interleaved_in_2k_blocks() {
        assert_eq!(offset(0, 0), 0);
        assert_eq!(offset(1, 0), 2048);
        assert_eq!(offset(8, 0), 80);
        assert_eq!(offset(199, 79), 7 * 2048 + 24 * 80 + 79);
    }

    #[test]
    fn mode_1_pen_bits_are_scattered() {
        let mut px = [0u8; 8];
        // Pixel 0 takes bits 7 and 3, pixel 3 bits 4 and 0.
        pixels(Mode::M1, 0b1000_1000, &mut px);
        assert_eq!(&px[..4], &[3, 0, 0, 0]);
        pixels(Mode::M1, 0b0001_0001, &mut px);
        assert_eq!(&px[..4], &[0, 0, 0, 3]);
        pixels(Mode::M0, 0b1000_0000, &mut px);
        assert_eq!(&px[..2], &[1, 0]);
        pixels(Mode::M0, 0b0000_0001, &mut px);
        assert_eq!(&px[..2], &[0, 8]);
        pixels(Mode::M2, 0b1000_0001, &mut px);
        assert_eq!(px, [1, 0, 0, 0, 0, 0, 0, 1]);
    }

    /// A mode 1 picture made of flat colour areas.
    fn flat_mode1() -> Vec<u8> {
        let mut s = vec![0u8; 16384];
        for y in 0..200 {
            for c in 0..80 {
                // Left half pen 1 (bit 7 and not 3 set in every pixel's
                // low plane), right half pen 2.
                s[offset(y, c)] = if c < 40 { 0b1111_0000 } else { 0b0000_1111 };
            }
        }
        s
    }

    #[test]
    fn a_flat_mode_1_picture_is_guessed_as_mode_1() {
        let s = flat_mode1();
        assert_eq!(guess_mode(&s), Mode::M1);
        let (pic, detail) = decode(&s, None).unwrap();
        assert_eq!((pic.width, pic.height), (320, 200));
        assert_eq!(pic.get(0, 0), ink(24));
        assert_eq!(pic.get(319, 0), ink(20));
        assert!(detail.contains("mode 1") && detail.contains("guessed"));
    }

    #[test]
    fn an_ocp_pal_file_sets_mode_and_pens() {
        // Mode 1, three spare bytes, then twelve bands of gate-array values
        // per pen: pen 0 hardware ink 0x14 (black), pen 1 0x0c (bright red),
        // pens 2 and 3 as pens 0 and 1.
        let mut pal = vec![1u8, 0, 0];
        for hw in [0x14u8, 0x0c, 0x14, 0x0c] {
            pal.extend([0x40 | hw; 12]);
        }
        pal.resize(3 + 16 * 12 + 44, 0x36);
        let file = parse_palette(&pal).unwrap();
        assert_eq!(file.mode, Some(Mode::M1));
        assert_eq!(file.pens[0], 0);
        assert_eq!(file.pens[1], 6);
        // Pen 1 is the left half of the flat picture.
        let (pic, detail) = decode(&flat_mode1(), Some(&pal)).unwrap();
        assert_eq!(pic.get(0, 0), ink(6));
        assert_eq!(pic.get(319, 0), ink(0));
        assert!(detail.contains("from the .pal file"));
    }

    #[test]
    fn an_ink_file_sets_pens_and_leaves_the_mode_to_the_guess() {
        let ink_file: Vec<u8> = [0x14u8, 0x1c, 0x00, 0x07].iter().copied().chain([0; 12]).collect();
        let file = parse_palette(&ink_file).unwrap();
        assert_eq!(file.mode, None);
        assert_eq!(&file.pens[..3], &[0, 3, 13]);
        let (pic, detail) = decode(&flat_mode1(), Some(&ink_file)).unwrap();
        assert_eq!(pic.get(0, 0), ink(3));
        assert!(detail.contains("guessed"));
        assert!(parse_palette(&[9; 5]).is_none());
    }

    #[test]
    fn amsdos_headers_are_skipped() {
        let mut f = vec![0u8; 128];
        f.extend(flat_mode1());
        assert!(decode(&f, None).is_ok());
        assert!(decode(&[0; 100], None).is_err());
    }
}
