//! Amiga IFF pictures: ILBM (interleaved bitmap) and PBM (chunky bitmap)
//! `.iff`, `.lbm`, `.ilbm`, `.pbm` files.
//!
//! An IFF file is a `FORM` of chunks.  `BMHD` gives the size, depth,
//! masking and compression, `CMAP` the palette, `CAMG` the Amiga's display
//! mode flags, and `BODY` the pixels: for each line, one row of bits per
//! bitplane (and a mask plane when the header asks for one), optionally
//! packed with ByteRun1.  Handled: any depth up to 8 planes with a palette,
//! Extra Half-Brite (the second 32 colours are half as bright), HAM6 and
//! HAM8 (each pixel changes one colour channel of its left neighbour), 24
//! and 32-plane true colour, mask planes and transparent colours as alpha,
//! and PBM's one byte per pixel.
//!
//! Pictures whose pixels were not square (the Amiga's hires mode has pixels
//! twice as tall as wide, interlaced low resolution twice as wide as tall)
//! are repeated by two across or down to give them their shape.

use crate::pic::{MAX_PIXELS, Pic, Rgb, be16, be32, expand4};
use slerp_scene::{ColorSpace, LoadError, Texture};

const CAMG_HAM: u32 = 0x800;
const CAMG_EHB: u32 = 0x80;

pub fn sniff(head: &[u8]) -> bool {
    head.len() >= 12 && &head[..4] == b"FORM" && matches!(&head[8..12], b"ILBM" | b"PBM ")
}

struct Header {
    width: usize,
    height: usize,
    planes: usize,
    masking: u8,
    compression: u8,
    transparent: u16,
    aspect: (u8, u8),
}

/// ByteRun1: a control byte `n` from 0 to 127 copies the next `n + 1`
/// bytes, from 129 to 255 repeats the next byte `257 - n` times, and 128
/// does nothing.  Fills `out` exactly; returns false when the data ran out.
fn byte_run(input: &[u8], at: &mut usize, out: &mut [u8]) -> bool {
    let mut o = 0;
    while o < out.len() {
        let Some(&n) = input.get(*at) else { return false };
        *at += 1;
        match n {
            0..=127 => {
                let len = n as usize + 1;
                let take = len.min(out.len() - o);
                let Some(src) = input.get(*at..*at + len) else { return false };
                out[o..o + take].copy_from_slice(&src[..take]);
                *at += len;
                o += take;
            }
            128 => {}
            _ => {
                let Some(&b) = input.get(*at) else { return false };
                *at += 1;
                let len = (257 - n as usize).min(out.len() - o);
                out[o..o + len].fill(b);
                o += len;
            }
        }
    }
    true
}

pub fn decode(bytes: &[u8]) -> Result<(Texture, String), LoadError> {
    if !sniff(bytes) {
        return Err(LoadError::invalid("not an IFF picture"));
    }
    let pbm = &bytes[8..12] == b"PBM ";
    let end = (8 + be32(bytes, 4) as usize).min(bytes.len());

    // 1. Walk the chunks.
    let (mut header, mut cmap, mut camg, mut body) = (None, None, None, None);
    let mut at = 12;
    while at + 8 <= end {
        let id = &bytes[at..at + 4];
        let len = be32(bytes, at + 4) as usize;
        let data = bytes.get(at + 8..(at + 8).saturating_add(len).min(end)).unwrap_or(&[]);
        match id {
            b"BMHD" if data.len() >= 20 => {
                header = Some(Header {
                    width: be16(data, 0) as usize,
                    height: be16(data, 2) as usize,
                    planes: data[8] as usize,
                    masking: data[9],
                    compression: data[10],
                    transparent: be16(data, 12),
                    aspect: (data[14], data[15]),
                });
            }
            b"CMAP" => cmap = Some(data),
            b"CAMG" if data.len() >= 4 => camg = Some(be32(data, 0)),
            b"BODY" => body = Some(data),
            _ => {}
        }
        at = (at + 8).saturating_add(len).saturating_add(len & 1);
    }
    let h = header.ok_or_else(|| LoadError::invalid("the IFF picture has no BMHD chunk"))?;
    let body = body.ok_or_else(|| LoadError::invalid("the IFF picture has no BODY chunk"))?;
    if h.width == 0 || h.height == 0 || (h.width * h.height) as u64 > MAX_PIXELS {
        return Err(LoadError::invalid(format!("IFF picture size {}x{} is out of range", h.width, h.height)));
    }
    if h.compression > 1 {
        return Err(LoadError::Unsupported(format!("IFF compression {}", h.compression)));
    }
    if pbm && h.planes != 8 {
        return Err(LoadError::Unsupported(format!("PBM with {} planes", h.planes)));
    }
    let true_colour = h.planes == 24 || h.planes == 32;
    if !pbm && !(1..=8).contains(&h.planes) && !true_colour {
        return Err(LoadError::Unsupported(format!("{} bitplanes", h.planes)));
    }

    // 2. The palette.  Old Amiga programs stored 4-bit colours in the high
    //    nibble of each byte; a palette whose low nibbles are all zero is
    //    taken as one of those.
    let colours = cmap.map_or(0, |c| c.len() / 3);
    let four_bit = cmap.is_some_and(|c| c.iter().all(|&v| v & 15 == 0));
    let mut palette: Vec<Rgb> = match cmap {
        Some(c) => c
            .chunks_exact(3)
            .map(|p| {
                if four_bit { [expand4(p[0] >> 4), expand4(p[1] >> 4), expand4(p[2] >> 4)] } else { [p[0], p[1], p[2]] }
            })
            .collect(),
        None => {
            let n = 1usize << h.planes.min(8);
            (0..n).map(|i| [(i * 255 / (n - 1).max(1)) as u8; 3]).collect()
        }
    };
    let has_camg = camg.is_some();
    let camg = camg.unwrap_or(0);
    // Files from before CAMG existed: six planes with a 16-colour palette
    // are HAM, with a 32-colour palette Extra Half-Brite.
    let ham = !pbm
        && !true_colour
        && (5..=8).contains(&h.planes)
        && (camg & CAMG_HAM != 0 || (!has_camg && h.planes == 6 && colours == 16));
    let ehb = !pbm && !ham && h.planes == 6 && (camg & CAMG_EHB != 0 || (!has_camg && colours == 32));
    if ehb {
        palette.resize(32, [0, 0, 0]);
        let half: Vec<Rgb> = palette.iter().map(|c| [c[0] / 2, c[1] / 2, c[2] / 2]).collect();
        palette.extend(half);
    }

    // 3. Unpack the body line by line.
    let (w, ht) = (h.width, h.height);
    let row_bytes = w.div_ceil(16) * 2;
    let stored_planes = if pbm { 0 } else { h.planes + (h.masking == 1) as usize };
    // A body far too short for the size in the header is a damaged or hostile
    // file: refuse before allocating the picture.  ByteRun1 turns a row of
    // bytes into at best two bytes per 128, and the check allows half of that.
    let stored_row = if pbm { w + (w & 1) } else { row_bytes };
    let rows = ht * stored_planes.max(1);
    let least = if h.compression == 1 { rows * stored_row.div_ceil(128) * 2 } else { rows * stored_row } / 2;
    if body.len() < least {
        return Err(LoadError::invalid("the picture data is far shorter than the header says"));
    }
    let mut rgba = vec![0u8; w * ht * 4];
    let mut at = 0usize;
    let mut chunky = vec![0u8; w + (w & 1)];
    let mut planes_bits: Vec<Vec<u8>> = vec![vec![0u8; row_bytes]; stored_planes];
    let mut cut = false;
    for y in 0..ht {
        let ok = if pbm {
            read_row(body, &mut at, &mut chunky, h.compression)
        } else {
            planes_bits.iter_mut().all(|p| read_row(body, &mut at, p, h.compression))
        };
        if !ok {
            cut = true;
            break;
        }
        // HAM starts each line from the background colour.
        let mut prev = palette.first().copied().unwrap_or([0, 0, 0]);
        for x in 0..w {
            let idx = if pbm {
                chunky[x] as usize
            } else {
                let mut v = 0usize;
                for (p, bits) in planes_bits.iter().enumerate().take(h.planes) {
                    v |= (((bits[x >> 3] >> (7 - (x & 7))) & 1) as usize) << p;
                }
                v
            };
            let visible = if h.masking == 1 && !pbm {
                (planes_bits[h.planes][x >> 3] >> (7 - (x & 7))) & 1 == 1
            } else {
                !(h.masking == 2 && idx == h.transparent as usize)
            };
            let colour: Rgb = if true_colour {
                [(idx & 255) as u8, ((idx >> 8) & 255) as u8, ((idx >> 16) & 255) as u8]
            } else if ham {
                // The top two planes say what to change, the rest is the new
                // value (or palette entry): 3 bits in HAM5, 4 in HAM6, 6 in
                // HAM8.
                let bits = h.planes - 2;
                let (ctrl, data) = (idx >> bits, idx & ((1 << bits) - 1));
                let value = widen(data as u8, bits);
                let mut c = prev;
                match ctrl {
                    0 => c = palette.get(data).copied().unwrap_or([0, 0, 0]),
                    1 => c[2] = value,
                    2 => c[0] = value,
                    _ => c[1] = value,
                }
                prev = c;
                c
            } else {
                palette.get(idx).copied().unwrap_or([0, 0, 0])
            };
            let o = (y * w + x) * 4;
            rgba[o..o + 3].copy_from_slice(&colour);
            rgba[o + 3] = if !visible {
                0
            } else if h.planes == 32 {
                (idx >> 24) as u8
            } else {
                255
            };
        }
    }
    if cut {
        // Lines that never came are opaque black rather than see-through.
        for px in rgba.chunks_exact_mut(4) {
            if px[3] == 0 && px[..3] == [0, 0, 0] {
                px[3] = 255;
            }
        }
    }

    // 4. Pixel shape.
    let (ax, ay) = h.aspect;
    let pic = Pic { width: w as u32, height: ht as u32, rgba };
    let (sx, sy) = if ax as u32 * 2 >= ay as u32 * 3 && ay > 0 {
        (2, 1)
    } else if ay as u32 * 2 >= ax as u32 * 3 && ax > 0 {
        (1, 2)
    } else {
        (1, 1)
    };
    let pic = pic.stretch(sx, sy);
    let (pw, ph) = (pic.width, pic.height);
    let mut tex = Texture::from_rgba8(pw, ph, pic.rgba, ColorSpace::Srgb);
    tex.source.container = if pbm { "IFF PBM".into() } else { "IFF ILBM".into() };
    let mode = if ham {
        format!("HAM{}", h.planes)
    } else if ehb {
        "Extra Half-Brite".to_string()
    } else if true_colour {
        format!("{}-bit colour", h.planes)
    } else {
        format!("{} colours", 1usize << h.planes)
    };
    let mut detail = format!("{w}x{ht}, {} bitplanes, {mode}", h.planes);
    if h.compression == 1 {
        detail.push_str(", ByteRun1");
    }
    match h.masking {
        1 => detail.push_str(", mask plane"),
        2 => detail.push_str(&format!(", transparent colour {}", h.transparent)),
        _ => {}
    }
    if (sx, sy) != (1, 1) {
        detail.push_str(&format!(", pixel aspect {ax}:{ay} applied"));
    }
    if cut {
        detail.push_str(", picture data cut off");
    }
    tex.source.format = detail.clone();
    Ok((tex, detail))
}

/// A `bits`-wide channel value spread over 8 bits by repeating its bits.
fn widen(v: u8, bits: usize) -> u8 {
    let mut out = (v as u32) << (8 - bits);
    let mut shift = bits;
    while shift < 8 {
        out |= out >> shift;
        shift *= 2;
    }
    out as u8
}

fn read_row(body: &[u8], at: &mut usize, out: &mut [u8], compression: u8) -> bool {
    if compression == 1 {
        byte_run(body, at, out)
    } else {
        match body.get(*at..*at + out.len()) {
            Some(src) => {
                out.copy_from_slice(src);
                *at += out.len();
                true
            }
            None => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use slerp_scene::textures::Pixels;

    fn chunk(id: &[u8; 4], data: &[u8]) -> Vec<u8> {
        let mut v = id.to_vec();
        v.extend((data.len() as u32).to_be_bytes());
        v.extend(data);
        if data.len() & 1 == 1 {
            v.push(0);
        }
        v
    }

    fn bmhd(w: u16, h: u16, planes: u8, masking: u8, compression: u8, aspect: (u8, u8)) -> Vec<u8> {
        let mut d = Vec::new();
        d.extend(w.to_be_bytes());
        d.extend(h.to_be_bytes());
        d.extend([0, 0, 0, 0, planes, masking, compression, 0, 0, 0, aspect.0, aspect.1]);
        d.extend(w.to_be_bytes());
        d.extend(h.to_be_bytes());
        d
    }

    fn form(kind: &[u8; 4], chunks: &[Vec<u8>]) -> Vec<u8> {
        let body: Vec<u8> = chunks.concat();
        let mut f = b"FORM".to_vec();
        f.extend((body.len() as u32 + 4).to_be_bytes());
        f.extend(kind);
        f.extend(body);
        f
    }

    /// Planar rows for `idx` (one index per pixel), `planes` deep.
    fn planar(idx: &[u8], w: usize, planes: usize) -> Vec<u8> {
        let rb = w.div_ceil(16) * 2;
        let mut out = Vec::new();
        for row in idx.chunks(w) {
            for p in 0..planes {
                let mut bytes = vec![0u8; rb];
                for (x, &v) in row.iter().enumerate() {
                    if v >> p & 1 == 1 {
                        bytes[x >> 3] |= 0x80 >> (x & 7);
                    }
                }
                out.extend(bytes);
            }
        }
        out
    }

    fn pixel(t: &Texture, x: usize, y: usize) -> [u8; 4] {
        let Pixels::Rgba8(v) = t.base() else { panic!() };
        let i = (y * t.width() as usize + x) * 4;
        [v[i], v[i + 1], v[i + 2], v[i + 3]]
    }

    fn rle(data: &[u8]) -> Vec<u8> {
        // Literal runs only, plus a repeat for runs of 3 or more.
        let mut out = Vec::new();
        let mut i = 0;
        while i < data.len() {
            let run = data[i..].iter().take(128).take_while(|&&b| b == data[i]).count();
            if run >= 3 {
                out.push((257 - run) as u8);
                out.push(data[i]);
                i += run;
            } else {
                out.push(0);
                out.push(data[i]);
                i += 1;
            }
        }
        out
    }

    #[test]
    fn indexed_picture_with_a_palette() {
        let idx: Vec<u8> = (0..4 * 2).map(|i| (i % 4) as u8).collect();
        let cmap = [0, 0, 0, 255, 0, 0, 0, 255, 0, 0, 0, 255];
        let f = form(
            b"ILBM",
            &[chunk(b"BMHD", &bmhd(4, 2, 2, 0, 0, (1, 1))), chunk(b"CMAP", &cmap), chunk(b"BODY", &planar(&idx, 4, 2))],
        );
        let (t, detail) = decode(&f).unwrap();
        assert_eq!((t.width(), t.height()), (4, 2));
        assert_eq!(pixel(&t, 1, 0), [255, 0, 0, 255]);
        assert_eq!(pixel(&t, 2, 1), [0, 255, 0, 255]);
        assert_eq!(pixel(&t, 3, 0), [0, 0, 255, 255]);
        assert!(detail.contains("4 colours"));
    }

    #[test]
    fn byte_run_compression_matches() {
        let idx: Vec<u8> = (0..32 * 4).map(|i| if i % 32 < 20 { 1 } else { 2 }).collect();
        let planes = planar(&idx, 32, 2);
        let rb = 4;
        let mut packed = Vec::new();
        for row in planes.chunks(rb) {
            packed.extend(rle(row));
        }
        let cmap = [0, 0, 0, 10, 20, 30, 40, 50, 60, 0, 0, 0];
        let a = form(
            b"ILBM",
            &[chunk(b"BMHD", &bmhd(32, 4, 2, 0, 0, (1, 1))), chunk(b"CMAP", &cmap), chunk(b"BODY", &planes)],
        );
        let b = form(
            b"ILBM",
            &[chunk(b"BMHD", &bmhd(32, 4, 2, 0, 1, (1, 1))), chunk(b"CMAP", &cmap), chunk(b"BODY", &packed)],
        );
        let (ta, _) = decode(&a).unwrap();
        let (tb, detail) = decode(&b).unwrap();
        let (Pixels::Rgba8(pa), Pixels::Rgba8(pb)) = (ta.base(), tb.base()) else { panic!() };
        assert_eq!(pa, pb);
        assert!(detail.contains("ByteRun1"));
    }

    #[test]
    fn ham6_modifies_one_channel() {
        // Pixel 0 palette colour 1 (red 255), pixel 1 modifies blue to 15,
        // pixel 2 modifies green to 8, pixel 3 modifies red to 0.
        let idx = [1u8, 0b01_1111, 0b11_1000, 0b10_0000];
        let mut cmap = vec![0u8; 16 * 3];
        cmap[3..6].copy_from_slice(&[255, 0, 0]);
        let camg = chunk(b"CAMG", &CAMG_HAM.to_be_bytes());
        let f = form(
            b"ILBM",
            &[
                chunk(b"BMHD", &bmhd(4, 1, 6, 0, 0, (1, 1))),
                chunk(b"CMAP", &cmap),
                camg,
                chunk(b"BODY", &planar(&idx, 4, 6)),
            ],
        );
        let (t, detail) = decode(&f).unwrap();
        assert_eq!(pixel(&t, 0, 0), [255, 0, 0, 255]);
        assert_eq!(pixel(&t, 1, 0), [255, 0, 255, 255]);
        assert_eq!(pixel(&t, 2, 0), [255, 136, 255, 255]);
        assert_eq!(pixel(&t, 3, 0), [0, 136, 255, 255]);
        assert!(detail.contains("HAM6"));
    }

    #[test]
    fn widen_repeats_bits() {
        assert_eq!(widen(15, 4), 255);
        assert_eq!(widen(8, 4), 136);
        assert_eq!(widen(63, 6), 255);
        assert_eq!(widen(7, 3), 255);
        assert_eq!(widen(0, 3), 0);
    }

    #[test]
    fn ham5_has_three_bit_values() {
        // Pixel 0 palette 1; pixel 1 modifies blue to 3-bit 7.
        let idx = [1u8, 0b01_111];
        let mut cmap = vec![0u8; 8 * 3];
        cmap[3..6].copy_from_slice(&[255, 0, 0]);
        let f = form(
            b"ILBM",
            &[
                chunk(b"BMHD", &bmhd(2, 1, 5, 0, 0, (1, 1))),
                chunk(b"CMAP", &cmap),
                chunk(b"CAMG", &CAMG_HAM.to_be_bytes()),
                chunk(b"BODY", &planar(&idx, 2, 5)),
            ],
        );
        let (t, detail) = decode(&f).unwrap();
        assert_eq!(pixel(&t, 1, 0), [255, 0, 255, 255]);
        assert!(detail.contains("HAM5"));
    }

    #[test]
    fn extra_half_brite_halves_the_upper_colours() {
        let idx = [1u8, 33];
        let mut cmap = vec![0u8; 32 * 3];
        cmap[3..6].copy_from_slice(&[200, 100, 50]);
        let f = form(
            b"ILBM",
            &[
                chunk(b"BMHD", &bmhd(2, 1, 6, 0, 0, (1, 1))),
                chunk(b"CMAP", &cmap),
                chunk(b"CAMG", &CAMG_EHB.to_be_bytes()),
                chunk(b"BODY", &planar(&idx, 2, 6)),
            ],
        );
        let (t, _) = decode(&f).unwrap();
        assert_eq!(pixel(&t, 0, 0), [200, 100, 50, 255]);
        assert_eq!(pixel(&t, 1, 0), [100, 50, 25, 255]);
    }

    #[test]
    fn mask_planes_become_alpha() {
        let idx = [1u8, 1];
        let mut body = planar(&idx, 2, 1);
        // A mask plane after the one bitplane: only the first pixel opaque.
        body.extend([0x80, 0]);
        let f = form(
            b"ILBM",
            &[chunk(b"BMHD", &bmhd(2, 1, 1, 1, 0, (1, 1))), chunk(b"CMAP", &[0, 0, 0, 9, 9, 9]), chunk(b"BODY", &body)],
        );
        let (t, _) = decode(&f).unwrap();
        assert_eq!(pixel(&t, 0, 0)[3], 255);
        assert_eq!(pixel(&t, 1, 0)[3], 0);
        assert!(t.has_alpha);
    }

    #[test]
    fn pbm_is_chunky_and_pixel_aspect_applies() {
        let body = [0u8, 1, 1, 0];
        let f = form(
            b"PBM ",
            &[
                chunk(b"BMHD", &bmhd(2, 2, 8, 0, 0, (5, 11))),
                chunk(b"CMAP", &[0, 0, 0, 255, 255, 255]),
                chunk(b"BODY", &body),
            ],
        );
        let (t, _) = decode(&f).unwrap();
        // 5:11 pixels are tall, so lines are repeated.
        assert_eq!((t.width(), t.height()), (2, 4));
        assert_eq!(pixel(&t, 1, 0), [255, 255, 255, 255]);
        assert_eq!(pixel(&t, 1, 1), [255, 255, 255, 255]);
        let f = form(
            b"PBM ",
            &[
                chunk(b"BMHD", &bmhd(2, 2, 8, 0, 0, (20, 11))),
                chunk(b"CMAP", &[0, 0, 0, 255, 255, 255]),
                chunk(b"BODY", &body),
            ],
        );
        let (t, _) = decode(&f).unwrap();
        assert_eq!((t.width(), t.height()), (4, 2));
    }

    #[test]
    fn truncated_and_absurd_files_fail_cleanly() {
        let idx: Vec<u8> = (0..64).map(|i| (i % 4) as u8).collect();
        let f = form(
            b"ILBM",
            &[
                chunk(b"BMHD", &bmhd(8, 8, 2, 0, 0, (1, 1))),
                chunk(b"CMAP", &[0; 12]),
                chunk(b"BODY", &planar(&idx, 8, 2)),
            ],
        );
        for len in 0..f.len() {
            let _ = decode(&f[..len]);
        }
        let huge = form(b"ILBM", &[chunk(b"BMHD", &bmhd(65535, 65535, 8, 0, 0, (1, 1))), chunk(b"BODY", &[0; 16])]);
        assert!(decode(&huge).is_err());
        // The largest size the cap allows, with a body of a few bytes (a
        // ByteRun1 bomb): refused before the picture is allocated.
        let bomb =
            form(b"ILBM", &[chunk(b"BMHD", &bmhd(8192, 4096, 8, 0, 1, (1, 1))), chunk(b"BODY", &[0x81, 0, 0x81, 0])]);
        assert!(decode(&bomb).is_err());
        let pbm_bomb = form(b"PBM ", &[chunk(b"BMHD", &bmhd(8192, 4096, 8, 0, 1, (1, 1))), chunk(b"BODY", &[0x81, 0])]);
        assert!(decode(&pbm_bomb).is_err());
        let zero = form(b"ILBM", &[chunk(b"BMHD", &bmhd(0, 5, 2, 0, 0, (1, 1))), chunk(b"BODY", &[0; 16])]);
        assert!(decode(&zero).is_err());
        let deep = form(b"ILBM", &[chunk(b"BMHD", &bmhd(8, 8, 12, 0, 0, (1, 1))), chunk(b"BODY", &[0; 16])]);
        assert!(decode(&deep).is_err());
    }
}
