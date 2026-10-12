//! Atari ST pictures: DEGAS and DEGAS Elite (`.pi1`-`.pi3`, and the
//! PackBits-compressed `.pc1`-`.pc3`), NEOchrome (`.neo`) and Tiny Stuff
//! (`.tn1`-`.tn3`, `.tny`).
//!
//! All of them are dumps of the ST's screen memory, so they share one
//! screen layout and one palette form.  The ST has three resolutions:
//! low (320x200, 16 colours), medium (640x200, 4 colours) and high
//! (640x400, black and white).  A medium-resolution pixel is twice as tall
//! as wide on the monitor, so those pictures are drawn with every line
//! repeated.  DEGAS Elite's colour-cycling ranges are ignored.

use crate::pic::{self, Pic, Rgb, be16, slice};
use slerp_scene::LoadError;

/// The three screen resolutions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Res {
    Low,
    Medium,
    High,
}

impl Res {
    fn from_word(w: u16) -> Result<Res, LoadError> {
        match w {
            0 => Ok(Res::Low),
            1 => Ok(Res::Medium),
            2 => Ok(Res::High),
            n => Err(LoadError::invalid(format!("unknown ST resolution {n}"))),
        }
    }

    fn planes(self) -> usize {
        match self {
            Res::Low => 4,
            Res::Medium => 2,
            Res::High => 1,
        }
    }

    pub fn size(self) -> (u32, u32) {
        match self {
            Res::Low => (320, 200),
            Res::Medium => (640, 200),
            Res::High => (640, 400),
        }
    }

    fn name(self) -> &'static str {
        match self {
            Res::Low => "low resolution, 16 colours",
            Res::Medium => "medium resolution, 4 colours",
            Res::High => "high resolution, monochrome",
        }
    }
}

/// Bytes of screen memory in every resolution.
const SCREEN: usize = 32000;

/// The 16 palette words as RGB.  The ST has 3 bits per channel (`0x0RGB`);
/// the STE adds a fourth, least significant, bit in bits 3, 7 and 11, which
/// shows up in files saved on one.
pub fn palette(words: &[u8]) -> Vec<Rgb> {
    let w: Vec<u16> = (0..16).map(|i| be16(words, i * 2)).collect();
    let ste = w.iter().any(|c| c & 0x888 != 0);
    w.iter()
        .map(|&c| {
            let (r, g, b) = ((c >> 8) & 15, (c >> 4) & 15, c & 15);
            if ste {
                // Bit 3 is the least significant bit of the 4-bit value.
                let f = |v: u16| pic::expand4((((v & 7) << 1) | (v >> 3)) as u8);
                [f(r), f(g), f(b)]
            } else {
                [pic::expand3(r as u8), pic::expand3(g as u8), pic::expand3(b as u8)]
            }
        })
        .collect()
}

/// Draws planar screen memory.  `screen` holds `SCREEN` bytes laid out as
/// the ST's video chip reads them: groups of one 16-bit word per bitplane
/// for each run of 16 pixels.
fn draw_screen(screen: &[u8], res: Res, pal: &[Rgb]) -> Pic {
    let (w, h) = res.size();
    let planes = res.planes();
    let mut idx = vec![0u8; w as usize * h as usize];
    let line_bytes = w as usize / 16 * planes * 2;
    for y in 0..h as usize {
        let line = &screen[y * line_bytes..(y + 1) * line_bytes];
        for g in 0..w as usize / 16 {
            let mut words = [0u16; 4];
            for (p, word) in words.iter_mut().enumerate().take(planes) {
                *word = be16(line, (g * planes + p) * 2);
            }
            for bit in 0..16 {
                let mut v = 0u8;
                for (p, word) in words.iter().enumerate().take(planes) {
                    v |= (((word >> (15 - bit)) & 1) as u8) << p;
                }
                idx[y * w as usize + g * 16 + bit] = v;
            }
        }
    }
    finish(Pic::from_indexed(w, h, &idx, &monochrome_fix(res, pal)), res)
}

/// In high resolution the monitor shows a clear bit as white and a set bit
/// as black whatever the file's palette says (the programs write all sorts
/// in it), so the palette is ignored there.
fn monochrome_fix(res: Res, pal: &[Rgb]) -> Vec<Rgb> {
    if res == Res::High { vec![[255, 255, 255], [0, 0, 0]] } else { pal.to_vec() }
}

/// Gives the picture the shape of the ST's monitor: medium resolution's
/// pixels are twice as tall as wide.
fn finish(pic: Pic, res: Res) -> Pic {
    if res == Res::Medium { pic.stretch(1, 2) } else { pic }
}

/// What a picture was read as, for the inspector.
fn describe(res: Res, what: &str) -> String {
    let (w, h) = res.size();
    format!("{w}x{h}, {}{what}", res.name())
}

//region DEGAS

/// A DEGAS or DEGAS Elite picture: a resolution word (its top bit marks
/// compression), 16 palette words, then the screen.  Compressed files
/// pack each bitplane of each scan line separately.
pub fn degas(bytes: &[u8]) -> Result<(Pic, String), LoadError> {
    let word = be16(bytes, 0);
    let compressed = word & 0x8000 != 0;
    let res = Res::from_word(word & 0x7fff)?;
    let pal = palette(slice(bytes, 2, 32, "the DEGAS palette")?);
    let body = &bytes[34..];
    let mut notes = String::new();
    let screen = if compressed {
        let mut out = vec![0u8; SCREEN];
        let line_bytes = res.size().0 as usize / 8;
        let planes = res.planes();
        let mut from = 0;
        let mut truncated = false;
        // Plane rows are unpacked one at a time into planar form first.
        let mut plane_rows = vec![0u8; line_bytes];
        for y in 0..res.size().1 as usize {
            for p in 0..planes {
                let used = unpack_bits(&body[from.min(body.len())..], &mut plane_rows);
                from += used.0;
                truncated |= used.1;
                // Pack the plane row back into the word-interleaved layout.
                let base = y * line_bytes * planes;
                for (i, &b) in plane_rows.iter().enumerate() {
                    let group = i / 2;
                    out[base + (group * planes + p) * 2 + (i & 1)] = b;
                }
            }
        }
        if truncated {
            notes.push_str(", data cut off");
        }
        out
    } else {
        let mut out = vec![0u8; SCREEN];
        let n = body.len().min(SCREEN);
        out[..n].copy_from_slice(&body[..n]);
        if body.len() < SCREEN {
            notes.push_str(", data cut off");
        }
        out
    };
    let what = if compressed { ", compressed" } else { "" };
    let detail = format!("{}{notes}", describe(res, what));
    Ok((draw_screen(&screen, res, &pal), detail))
}

/// Unpacks PackBits-style data into `out`, which it fills completely
/// (zeros where the input ran out).  A control byte `n` from 0 to 127 is
/// followed by `n + 1` literal bytes, one from 129 to 255 by a byte to
/// repeat `257 - n` times, and 128 does nothing.  Returns the bytes of
/// input used and whether the input ended too soon.
pub fn unpack_bits(input: &[u8], out: &mut [u8]) -> (usize, bool) {
    let (mut i, mut o) = (0usize, 0usize);
    while o < out.len() {
        let Some(&n) = input.get(i) else { break };
        i += 1;
        match n {
            0..=127 => {
                let len = (n as usize + 1).min(out.len() - o);
                let have = len.min(input.len() - i);
                out[o..o + have].copy_from_slice(&input[i..i + have]);
                i += have;
                o += have;
                if have < len {
                    break;
                }
            }
            128 => {}
            _ => {
                let Some(&b) = input.get(i) else { break };
                i += 1;
                let len = (257 - n as usize).min(out.len() - o);
                out[o..o + len].fill(b);
                o += len;
            }
        }
    }
    let short = o < out.len();
    out[o..].fill(0);
    (i, short)
}

//endregion

//region NEOchrome

/// A NEOchrome picture: 128 bytes of header (flag word, resolution word,
/// 16 palette words, a file name and colour-cycling data), then the screen.
pub fn neochrome(bytes: &[u8]) -> Result<(Pic, String), LoadError> {
    let res = Res::from_word(be16(bytes, 2))?;
    let pal = palette(slice(bytes, 4, 32, "the NEOchrome palette")?);
    let screen = slice(bytes, 128, SCREEN, "the NEOchrome screen")?;
    Ok((draw_screen(screen, res, &pal), describe(res, "")))
}

//endregion

//region Tiny Stuff

/// A Tiny Stuff picture: a resolution byte (3 and up mean the same
/// resolution minus 3 plus four bytes of colour-cycling data), the
/// palette, and a compressed screen kept as a control-byte stream and a
/// word stream.  The unpacked words are the screen memory stored one
/// 16-pixel column of one bitplane at a time, top to bottom (see below).
pub fn tiny(bytes: &[u8]) -> Result<(Pic, String), LoadError> {
    let mut res_byte = *bytes.first().ok_or_else(|| LoadError::invalid("empty Tiny Stuff file"))?;
    let mut at = 1;
    if res_byte > 2 {
        res_byte -= 3;
        at += 4;
    }
    let res = Res::from_word(res_byte as u16)?;
    let pal = palette(slice(bytes, at, 32, "the Tiny Stuff palette")?);
    at += 32;
    let control_len = be16(bytes, at) as usize;
    let data_words = be16(bytes, at + 2) as usize;
    at += 4;
    let control = slice(bytes, at, control_len, "the Tiny Stuff control bytes")?;
    let data = bytes.get(at + control_len..).unwrap_or(&[]);
    let data = &data[..data.len().min(data_words * 2)];

    // 1. Unpack to 16000 words.
    let mut words: Vec<u16> = Vec::with_capacity(SCREEN / 2);
    let (mut c, mut d) = (0usize, 0usize);
    let mut cut = false;
    let word = |d: usize| -> Option<u16> { data.get(d * 2..d * 2 + 2).map(|w| u16::from_be_bytes([w[0], w[1]])) };
    while words.len() < SCREEN / 2 {
        let Some(&n) = control.get(c) else {
            cut = true;
            break;
        };
        c += 1;
        let (repeat, literal) = match n as i8 {
            0 | 1 => {
                if c + 2 > control.len() {
                    cut = true;
                    break;
                }
                let count = be16(control, c) as usize;
                c += 2;
                if n == 0 { (count, 0) } else { (0, count) }
            }
            2..=127 => (n as usize, 0),
            v => (0, (-(v as i32)) as usize),
        };
        if repeat > 0 {
            let Some(w) = word(d) else {
                cut = true;
                break;
            };
            d += 1;
            let k = repeat.min(SCREEN / 2 - words.len());
            words.extend(std::iter::repeat_n(w, k));
        } else {
            for _ in 0..literal {
                let Some(w) = word(d) else {
                    cut = true;
                    break;
                };
                d += 1;
                if words.len() < SCREEN / 2 {
                    words.push(w);
                }
            }
            if cut {
                break;
            }
        }
    }
    words.resize(SCREEN / 2, 0);

    // 2. Undo the column order.  The words come as four sets (one per
    //    bitplane) of twenty columns of 200 words; screen memory has the four
    //    planes of a 16-pixel column side by side.  (High resolution has a
    //    single plane, forty columns of 400 words.)
    let mut screen = vec![0u8; SCREEN];
    for (k, w) in words.iter().enumerate() {
        let (columns, row, word_col) =
            if res == Res::High { (40, k % 400, k / 400) } else { (80, k % 200, (k / 200 % 20) * 4 + k / 4000) };
        let at = (row * columns + word_col) * 2;
        screen[at..at + 2].copy_from_slice(&w.to_be_bytes());
    }
    let mut detail = describe(res, ", Tiny Stuff compression");
    if cut {
        detail.push_str(", data cut off");
    }
    Ok((draw_screen(&screen, res, &pal), detail))
}

//endregion

#[cfg(test)]
mod tests {
    use super::*;

    /// Packs an index image into ST screen memory, the way tests need it.
    pub(crate) fn pack(idx: &[u8], res: Res) -> Vec<u8> {
        let (w, h) = res.size();
        let planes = res.planes();
        let mut out = vec![0u8; SCREEN];
        for y in 0..h as usize {
            for g in 0..w as usize / 16 {
                for p in 0..planes {
                    let mut word = 0u16;
                    for bit in 0..16 {
                        let v = idx[y * w as usize + g * 16 + bit];
                        word |= (((v >> p) & 1) as u16) << (15 - bit);
                    }
                    let at = y * (w as usize / 16 * planes * 2) + (g * planes + p) * 2;
                    out[at..at + 2].copy_from_slice(&word.to_be_bytes());
                }
            }
        }
        out
    }

    pub(crate) fn test_palette() -> Vec<u8> {
        // Colour 1 pure red, 2 pure green, 3 pure blue, 15 white.
        let mut p = vec![0u8; 32];
        for (i, c) in [(1, 0x700u16), (2, 0x070), (3, 0x007), (15, 0x777)] {
            p[i * 2..i * 2 + 2].copy_from_slice(&c.to_be_bytes());
        }
        p
    }

    fn test_image(res: Res) -> Vec<u8> {
        let (w, h) = res.size();
        let n = [16usize, 4, 2][res as usize];
        (0..w as usize * h as usize).map(|i| ((i % w as usize + i / w as usize / 3) % n) as u8).collect()
    }

    #[test]
    fn degas_low_resolution_draws_known_pixels() {
        let idx = test_image(Res::Low);
        let mut file = vec![0, 0];
        file.extend(test_palette());
        file.extend(pack(&idx, Res::Low));
        assert_eq!(file.len(), 32034);
        let (pic, detail) = degas(&file).unwrap();
        assert_eq!((pic.width, pic.height), (320, 200));
        assert_eq!(pic.get(0, 0), [0, 0, 0]);
        assert_eq!(pic.get(1, 0), [255, 0, 0]);
        assert_eq!(pic.get(2, 0), [0, 255, 0]);
        assert_eq!(pic.get(3, 0), [0, 0, 255]);
        assert_eq!(pic.get(15, 0), [255, 255, 255]);
        assert!(detail.contains("low resolution"));
    }

    #[test]
    fn degas_medium_resolution_doubles_lines() {
        let idx = test_image(Res::Medium);
        let mut file = vec![0, 1];
        file.extend(test_palette());
        file.extend(pack(&idx, Res::Medium));
        let (pic, _) = degas(&file).unwrap();
        assert_eq!((pic.width, pic.height), (640, 400));
        assert_eq!(pic.get(1, 0), pic.get(1, 1));
        assert_eq!(pic.get(1, 0), [255, 0, 0]);
        assert_eq!(pic.get(2, 0), [0, 255, 0]);
    }

    #[test]
    fn degas_high_resolution_is_black_on_white_whatever_the_palette() {
        let idx = test_image(Res::High);
        let mut pal = vec![0u8; 32];
        pal[2..4].copy_from_slice(&0x777u16.to_be_bytes());
        let mut file = vec![0, 2];
        file.extend(pal);
        file.extend(pack(&idx, Res::High));
        let (pic, _) = degas(&file).unwrap();
        assert_eq!((pic.width, pic.height), (640, 400));
        assert_eq!(pic.get(0, 0), [255, 255, 255]);
        assert_eq!(pic.get(1, 0), [0, 0, 0]);
    }

    #[test]
    fn ste_palettes_use_the_extra_bit() {
        let mut p = vec![0u8; 32];
        p[2..4].copy_from_slice(&0x888u16.to_be_bytes());
        assert_eq!(palette(&p)[1], [17, 17, 17]);
        p[2..4].copy_from_slice(&0xfffu16.to_be_bytes());
        assert_eq!(palette(&p)[1], [255, 255, 255]);
    }

    /// PackBits as DEGAS Elite writes it, one plane row at a time.
    fn pack_rows(screen: &[u8], res: Res) -> Vec<u8> {
        let planes = res.planes();
        let line_bytes = res.size().0 as usize / 8;
        let mut out = Vec::new();
        for y in 0..res.size().1 as usize {
            for p in 0..planes {
                let mut row = vec![0u8; line_bytes];
                for (i, b) in row.iter_mut().enumerate() {
                    let group = i / 2;
                    *b = screen[y * line_bytes * planes + (group * planes + p) * 2 + (i & 1)];
                }
                let mut i = 0;
                while i < row.len() {
                    let run = row[i..].iter().take(128).take_while(|&&b| b == row[i]).count();
                    if run >= 3 {
                        out.push((257 - run) as u8);
                        out.push(row[i]);
                        i += run;
                    } else {
                        let mut lit = 1;
                        while i + lit < row.len()
                            && lit < 128
                            && row[i + lit..].iter().take(3).any(|&b| b != row[i + lit])
                        {
                            lit += 1;
                        }
                        out.push((lit - 1) as u8);
                        out.extend(&row[i..i + lit]);
                        i += lit;
                    }
                }
            }
        }
        out
    }

    #[test]
    fn compressed_degas_matches_uncompressed() {
        for res in [Res::Low, Res::Medium, Res::High] {
            let idx = test_image(res);
            let screen = pack(&idx, res);
            let mut plain = vec![0, res as u8];
            plain.extend(test_palette());
            plain.extend(&screen);
            let mut packed = vec![0x80, res as u8];
            packed.extend(test_palette());
            packed.extend(pack_rows(&screen, res));
            let a = degas(&plain).unwrap().0;
            let b = degas(&packed).unwrap().0;
            assert_eq!(a.rgba, b.rgba, "{res:?}");
        }
    }

    #[test]
    fn neochrome_reads_the_screen_after_128_bytes() {
        let idx = test_image(Res::Low);
        let mut file = vec![0u8; 4];
        file.extend(test_palette());
        file.resize(128, 0);
        file.extend(pack(&idx, Res::Low));
        assert_eq!(file.len(), 32128);
        let (pic, _) = neochrome(&file).unwrap();
        assert_eq!(pic.get(1, 0), [255, 0, 0]);
        assert_eq!(pic.get(3, 0), [0, 0, 255]);
    }

    #[test]
    fn tiny_stuff_unpacks_columns() {
        let idx = test_image(Res::Low);
        let screen = pack(&idx, Res::Low);
        // Words in column order, stored as literals of up to 128 words.
        // Four sets (planes) of twenty columns of 200 words each.
        let mut words = Vec::new();
        for set in 0..4 {
            for col in 0..20 {
                for row in 0..200 {
                    let at = (row * 80 + col * 4 + set) * 2;
                    words.push(u16::from_be_bytes([screen[at], screen[at + 1]]));
                }
            }
        }
        let mut control = Vec::new();
        let mut data = Vec::new();
        for chunk in words.chunks(128) {
            control.push((256 - chunk.len() as i32) as u8);
            for w in chunk {
                data.extend(w.to_be_bytes());
            }
        }
        let mut file = vec![0u8];
        file.extend(test_palette());
        file.extend((control.len() as u16).to_be_bytes());
        file.extend((words.len() as u16).to_be_bytes());
        file.extend(&control);
        file.extend(&data);
        let (pic, detail) = tiny(&file).unwrap();
        assert_eq!(pic.get(2, 0), [0, 255, 0]);
        assert_eq!(pic.get(15, 100), pic.get(15, 100));
        assert!(!detail.contains("cut off"));
        let reference = {
            let mut f = vec![0, 0];
            f.extend(test_palette());
            f.extend(&screen);
            degas(&f).unwrap().0
        };
        assert_eq!(pic.rgba, reference.rgba);
    }

    #[test]
    fn tiny_stuff_repeats() {
        // 16000 words of 0xFFFF as a single long repeat (control 0 + count).
        let mut file = vec![0u8];
        file.extend(test_palette());
        let control = [0u8, 0x3e, 0x80];
        file.extend((control.len() as u16).to_be_bytes());
        file.extend(1u16.to_be_bytes());
        file.extend(control);
        file.extend([0xff, 0xff]);
        let (pic, _) = tiny(&file).unwrap();
        assert_eq!(pic.get(5, 5), [255, 255, 255]);
    }

    #[test]
    fn damaged_files_never_panic() {
        let idx = test_image(Res::Low);
        let mut file = vec![0x80, 0];
        file.extend(test_palette());
        file.extend(pack_rows(&pack(&idx, Res::Low), Res::Low));
        for len in [0, 1, 2, 33, 34, 35, 200, file.len() / 2] {
            let _ = degas(&file[..len.min(file.len())]);
            let _ = neochrome(&file[..len.min(file.len())]);
            let _ = tiny(&file[..len.min(file.len())]);
        }
        let mut seed = 12345u32;
        for _ in 0..200 {
            let mut f = file.clone();
            for _ in 0..20 {
                seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                let at = (seed >> 8) as usize % f.len();
                f[at] = (seed >> 24) as u8;
            }
            let _ = degas(&f);
            let _ = tiny(&f);
            let _ = neochrome(&f);
        }
        assert!(degas(&[0x80, 9]).is_err());
    }
}
