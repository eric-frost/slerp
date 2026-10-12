//! Valve Texture Format (`.vtf`), the textures of Source engine materials.
//!
//! A header (versions 7.0 to 7.6) gives the size, flags, frame count, the
//! pixel format of the main image and of a tiny low-resolution thumbnail;
//! from 7.3 on, a resource table says where each part lives.  The main
//! image data holds every mip level from the smallest up, each with all
//! frames, faces and depth slices.  Only the first frame, face and slice
//! of one mip level is decoded: the smallest that is still at least the
//! size asked for, so thumbnails decode little.
//!
//! Formats: DXT1/3/5 and ATI1N/ATI2N blocks (through `bcdec_rs`), the 8-bit
//! RGBA orders, RGB888 and BGR888 (and their blue-screen keyed forms),
//! 565, 5551 and 4444 packings, I8, IA88, A8, UV88 and UVWQ, and the 16-bit
//! and float formats (HDR).

use crate::pixels;
use crate::read::{Bytes, Result};
use slerp_scene::LoadError;
use slerp_scene::textures::{ColorSpace, Texture};

/// Header flag: the texture is a cube map (six faces, or seven with a
/// sphere map before version 7.5).
const ENVMAP: u32 = 0x4000;
/// Header flags for 1-bit and 8-bit alpha, which say alpha means something.
const ONEBITALPHA: u32 = 0x1000;
const EIGHTBITALPHA: u32 = 0x2000;

/// The largest side decoded (Source's own limit is 4096), and the most
/// frames, faces and slices believed, so a corrupt header can't ask for
/// unbounded memory.
const MAX_SIDE: u32 = 8192;
const MAX_LAYERS: u64 = 4096;

/// What a VTF header says.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Header {
    pub version: (u32, u32),
    pub width: u32,
    pub height: u32,
    pub flags: u32,
    pub frames: u32,
    /// 0xffff when a cube map has no sphere map (before version 7.5).
    pub first_frame: u32,
    pub format: i32,
    pub mips: u32,
    pub depth: u32,
    pub reflectivity: [f32; 3],
    /// Where the main image data starts.
    pub data: usize,
}

/// Whether the bytes start like a VTF file.
pub(crate) fn sniff(head: &[u8]) -> bool {
    head.len() >= 12 && &head[..4] == b"VTF\0" && head[4..8] == 7u32.to_le_bytes() && head[8] <= 6
}

/// The pixel format's name, size of a 4x4 block (or of one texel) in bytes,
/// and whether it is block compressed.
fn format_info(format: i32) -> Option<(&'static str, usize, bool)> {
    Some(match format {
        0 => ("RGBA8888", 4, false),
        1 => ("ABGR8888", 4, false),
        2 => ("RGB888", 3, false),
        3 => ("BGR888", 3, false),
        4 => ("RGB565", 2, false),
        5 => ("I8", 1, false),
        6 => ("IA88", 2, false),
        8 => ("A8", 1, false),
        9 => ("RGB888_BLUESCREEN", 3, false),
        10 => ("BGR888_BLUESCREEN", 3, false),
        11 => ("ARGB8888", 4, false),
        12 => ("BGRA8888", 4, false),
        13 => ("DXT1", 8, true),
        14 => ("DXT3", 16, true),
        15 => ("DXT5", 16, true),
        16 => ("BGRX8888", 4, false),
        17 => ("BGR565", 2, false),
        18 => ("BGRX5551", 2, false),
        19 => ("BGRA4444", 2, false),
        20 => ("DXT1_ONEBITALPHA", 8, true),
        21 => ("BGRA5551", 2, false),
        22 => ("UV88", 2, false),
        23 => ("UVWQ8888", 4, false),
        24 => ("RGBA16161616F", 8, false),
        25 => ("RGBA16161616", 8, false),
        26 => ("UVLX8888", 4, false),
        27 => ("R32F", 4, false),
        28 => ("RGB323232F", 12, false),
        29 => ("RGBA32323232F", 16, false),
        37 => ("ATI2N", 16, true),
        38 => ("ATI1N", 8, true),
        _ => return None,
    })
}

/// Bytes of one image of `w` x `h` in `format`.
fn image_bytes(format: i32, w: u32, h: u32) -> Option<u64> {
    let (_, size, blocks) = format_info(format)?;
    let (w, h) = (w.max(1) as u64, h.max(1) as u64);
    Some(if blocks { w.div_ceil(4) * h.div_ceil(4) * size as u64 } else { w * h * size as u64 })
}

pub(crate) fn header(bytes: &[u8]) -> Result<Header> {
    let b = Bytes::new(bytes);
    if !sniff(bytes) {
        return Err(LoadError::NotMine);
    }
    let version = (b.u32(4)?, b.u32(8)?);
    let header_size = b.u32(12)? as usize;
    let (width, height) = (b.u16(16)? as u32, b.u16(18)? as u32);
    let flags = b.u32(20)?;
    let frames = (b.u16(24)? as u32).max(1);
    let first_frame = b.u16(26)? as u32;
    let reflectivity = [b.f32(32)?, b.f32(36)?, b.f32(40)?];
    let format = b.i32(52)?;
    let mips = (b.u8(56)? as u32).max(1);
    let low_format = b.i32(57)?;
    let (low_w, low_h) = (b.u8(61)? as u32, b.u8(62)? as u32);
    let depth = if version.1 >= 2 { (b.u16(63)? as u32).max(1) } else { 1 };
    if width == 0 || height == 0 || width > MAX_SIDE || height > MAX_SIDE || depth > MAX_SIDE {
        return Err(LoadError::invalid(format!("texture size {width}x{height}x{depth} is out of range")));
    }

    // Where the main image is: a resource from 7.3 on, otherwise after
    // the header and the low-resolution thumbnail.
    let data = if version.1 >= 3 {
        let count = b.u32(68)? as usize;
        let mut found = None;
        for i in 0..count.min(32) {
            let at = 80 + i * 8;
            let tag = b.slice(at, 3)?;
            if tag == [0x30, 0, 0] {
                found = Some(b.u32(at + 4)? as usize);
            }
            // Strata Source's 7.6 files may deflate or zstd their levels,
            // described by an "AXC" resource.
            if tag == b"AXC" {
                return Err(LoadError::Unsupported("compressed VTF (version 7.6)".into()));
            }
        }
        found.ok_or_else(|| LoadError::invalid("no image resource"))?
    } else {
        let low = if low_format < 0 || low_w == 0 { 0 } else { image_bytes(low_format, low_w, low_h).unwrap_or(0) };
        header_size.checked_add(low as usize).ok_or_else(|| LoadError::invalid("header size overflows"))?
    };
    Ok(Header { version, width, height, flags, frames, first_frame, format, mips, depth, reflectivity, data })
}

/// Number of faces each frame has.
fn faces(h: &Header) -> u32 {
    if h.flags & ENVMAP == 0 {
        1
    } else if h.version.1 < 5 && h.first_frame != 0xffff {
        // Sphere maps came seventh until 7.5 (VTFLib's rule).
        7
    } else {
        6
    }
}

/// Decodes the first frame, face and slice of the smallest mip level whose
/// longer side is still at least `max` (the largest when `max` is `None`),
/// downscaled to fit `max`.
pub(crate) fn decode(bytes: &[u8], max: Option<u32>) -> Result<Texture> {
    let h = header(bytes)?;
    let b = Bytes::new(bytes);
    let (name, _, _) =
        format_info(h.format).ok_or_else(|| LoadError::Unsupported(format!("VTF pixel format {}", h.format)))?;
    let layers = h.frames as u64 * faces(&h) as u64;
    if layers > MAX_LAYERS {
        return Err(LoadError::invalid(format!("{layers} frames and faces")));
    }
    let mips = h.mips.min(32);

    // 1. Pick the level, then find it: levels run from the smallest to the
    //    largest, each holding every frame, face and slice.
    let size_at = |l: u32| ((h.width >> l).max(1), (h.height >> l).max(1), (h.depth >> l).max(1));
    let mut level = 0;
    if let Some(max) = max {
        while level + 1 < mips {
            let (w, hh, _) = size_at(level + 1);
            if w.max(hh) < max {
                break;
            }
            level += 1;
        }
    }
    let mut at = h.data as u64;
    for l in (level + 1..mips).rev() {
        let (w, hh, d) = size_at(l);
        at += image_bytes(h.format, w, hh).unwrap_or(0) * d as u64 * layers;
    }
    let (w, hh, _) = size_at(level);
    let len = image_bytes(h.format, w, hh).unwrap_or(0);
    let data = b.slice(usize::try_from(at).map_err(|_| LoadError::invalid("offset overflows"))?, len as usize)?;

    // 2. Texels.
    let alpha_flags = h.flags & (ONEBITALPHA | EIGHTBITALPHA) != 0;
    let mut tex = match texels(h.format, data, w, hh)? {
        Texels::Ldr(rgba) => {
            let (fw, fh, rgba) = pixels::downscale(rgba, w, hh, max);
            let mut t = Texture::from_rgba8(fw, fh, rgba, ColorSpace::Srgb);
            // DXT1 without the alpha flag is opaque, whatever its blocks say.
            if h.format == 13 && !alpha_flags {
                t.has_alpha = false;
            }
            t
        }
        Texels::Hdr(rgba) => Texture::from_rgba32f(w, hh, rgba),
    };
    tex.source.container = format!("VTF {}.{}", h.version.0, h.version.1);
    tex.source.format = name.to_string();
    tex.source.width = h.width;
    tex.source.height = h.height;
    tex.source.mip_count = h.mips;
    tex.source.file_bytes = bytes.len() as u64;
    Ok(tex)
}

enum Texels {
    Ldr(Vec<u8>),
    Hdr(Vec<f32>),
}

/// Converts one image to RGBA.
fn texels(format: i32, data: &[u8], w: u32, h: u32) -> Result<Texels> {
    let n = w as usize * h as usize;
    let mut out = vec![0u8; n * 4];
    let px = |f: &dyn Fn(&[u8]) -> [u8; 4], size: usize, out: &mut Vec<u8>| {
        for (o, s) in out.chunks_exact_mut(4).zip(data.chunks_exact(size)) {
            o.copy_from_slice(&f(s));
        }
    };
    let c5 = |v: u16| ((v as u32 * 255 + 15) / 31) as u8;
    let c6 = |v: u16| ((v as u32 * 255 + 31) / 63) as u8;
    let c4 = |v: u16| (v as u8 & 15) * 17;
    match format {
        0 => px(&|s| [s[0], s[1], s[2], s[3]], 4, &mut out),
        1 => px(&|s| [s[3], s[2], s[1], s[0]], 4, &mut out),
        2 => px(&|s| [s[0], s[1], s[2], 255], 3, &mut out),
        3 => px(&|s| [s[2], s[1], s[0], 255], 3, &mut out),
        4 => px(
            &|s| {
                let v = u16::from_le_bytes([s[0], s[1]]);
                [c5(v & 31), c6((v >> 5) & 63), c5(v >> 11), 255]
            },
            2,
            &mut out,
        ),
        5 => px(&|s| [s[0], s[0], s[0], 255], 1, &mut out),
        6 => px(&|s| [s[0], s[0], s[0], s[1]], 2, &mut out),
        8 => px(&|s| [255, 255, 255, s[0]], 1, &mut out),
        9 => px(&|s| [s[0], s[1], s[2], if s[..3] == [0, 0, 255] { 0 } else { 255 }], 3, &mut out),
        10 => px(&|s| [s[2], s[1], s[0], if s[..3] == [255, 0, 0] { 0 } else { 255 }], 3, &mut out),
        // Stored green, blue, alpha, red, as VTFLib and the games read it.
        11 => px(&|s| [s[3], s[0], s[1], s[2]], 4, &mut out),
        12 => px(&|s| [s[2], s[1], s[0], s[3]], 4, &mut out),
        16 => px(&|s| [s[2], s[1], s[0], 255], 4, &mut out),
        17 => px(
            &|s| {
                let v = u16::from_le_bytes([s[0], s[1]]);
                [c5(v >> 11), c6((v >> 5) & 63), c5(v & 31), 255]
            },
            2,
            &mut out,
        ),
        18 | 21 => px(
            &|s| {
                let v = u16::from_le_bytes([s[0], s[1]]);
                let a = if format == 21 && v & 0x8000 == 0 { 0 } else { 255 };
                [c5((v >> 10) & 31), c5((v >> 5) & 31), c5(v & 31), a]
            },
            2,
            &mut out,
        ),
        19 => px(
            &|s| {
                let v = u16::from_le_bytes([s[0], s[1]]);
                [c4(v >> 8), c4(v >> 4), c4(v), c4(v >> 12)]
            },
            2,
            &mut out,
        ),
        // Two-channel normal and du/dv maps: shown as red and green.
        22 => px(&|s| [s[0], s[1], 0, 255], 2, &mut out),
        23 | 26 => px(&|s| [s[0], s[1], s[2], 255], 4, &mut out),
        13 | 20 | 14 | 15 | 37 | 38 => blocks(format, data, w, h, &mut out),
        24 => {
            let half = |s: &[u8]| f16(u16::from_le_bytes([s[0], s[1]]));
            let v =
                data.chunks_exact(8).take(n).flat_map(|s| [half(&s[0..]), half(&s[2..]), half(&s[4..]), half(&s[6..])]);
            return Ok(Texels::Hdr(v.collect()));
        }
        25 => px(&|s| [s[1], s[3], s[5], s[7]], 8, &mut out),
        27..=29 => {
            let f = |s: &[u8], k: usize| f32::from_le_bytes([s[4 * k], s[4 * k + 1], s[4 * k + 2], s[4 * k + 3]]);
            let stride = match format {
                27 => 4,
                28 => 12,
                _ => 16,
            };
            let v = data.chunks_exact(stride).take(n).flat_map(|s| match format {
                27 => [f(s, 0), f(s, 0), f(s, 0), 1.0],
                28 => [f(s, 0), f(s, 1), f(s, 2), 1.0],
                _ => [f(s, 0), f(s, 1), f(s, 2), f(s, 3)],
            });
            let v: Vec<f32> = v.map(|x| if x.is_finite() { x } else { 0.0 }).collect();
            return Ok(Texels::Hdr(v));
        }
        _ => return Err(LoadError::Unsupported(format!("VTF pixel format {format}"))),
    }
    Ok(Texels::Ldr(out))
}

/// Decodes block-compressed data, 4x4 texels at a time, cropping the edge
/// blocks of sizes that aren't multiples of four.
fn blocks(format: i32, data: &[u8], w: u32, h: u32, out: &mut [u8]) {
    let (bw, bh) = (w.div_ceil(4) as usize, h.div_ceil(4) as usize);
    let size = if matches!(format, 13 | 20 | 38) { 8 } else { 16 };
    let mut block = [0u8; 64];
    for by in 0..bh {
        for bx in 0..bw {
            let at = (by * bw + bx) * size;
            let Some(src) = data.get(at..at + size) else { return };
            match format {
                13 | 20 => bcdec_rs::bc1(src, &mut block, 16),
                14 => bcdec_rs::bc2(src, &mut block, 16),
                15 => bcdec_rs::bc3(src, &mut block, 16),
                38 => {
                    let mut r = [0u8; 16];
                    bcdec_rs::bc4(src, &mut r, 4, false);
                    for (i, v) in r.iter().enumerate() {
                        block[i * 4..i * 4 + 4].copy_from_slice(&[*v, *v, *v, 255]);
                    }
                }
                _ => {
                    let mut rg = [0u8; 32];
                    bcdec_rs::bc5(src, &mut rg, 8, false);
                    for i in 0..16 {
                        block[i * 4..i * 4 + 4].copy_from_slice(&[rg[i * 2], rg[i * 2 + 1], 0, 255]);
                    }
                }
            }
            for y in 0..4 {
                let py = by * 4 + y;
                if py >= h as usize {
                    break;
                }
                for x in 0..4 {
                    let px = bx * 4 + x;
                    if px >= w as usize {
                        break;
                    }
                    let o = (py * w as usize + px) * 4;
                    out[o..o + 4].copy_from_slice(&block[(y * 4 + x) * 4..(y * 4 + x) * 4 + 4]);
                }
            }
        }
    }
}

/// An IEEE half-precision float.
fn f16(h: u16) -> f32 {
    let sign = if h & 0x8000 != 0 { -1.0 } else { 1.0 };
    let exp = ((h >> 10) & 31) as i32;
    let man = (h & 1023) as f32;
    let v = match exp {
        0 => man * 2f32.powi(-24),
        31 => 0.0,
        e => (1.0 + man / 1024.0) * 2f32.powi(e - 15),
    };
    sign * v
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use slerp_scene::textures::Pixels;

    /// Builds a version 7.`minor` VTF holding `levels` (largest first) of
    /// `format`, with a 1x1 DXT1 thumbnail before them.
    pub fn build(minor: u32, format: i32, width: u16, height: u16, flags: u32, levels: &[Vec<u8>]) -> Vec<u8> {
        let header_size = if minor >= 3 { 80 + 2 * 8 } else { 80 };
        let mut out = vec![0u8; header_size];
        out[..4].copy_from_slice(b"VTF\0");
        out[4..8].copy_from_slice(&7u32.to_le_bytes());
        out[8..12].copy_from_slice(&minor.to_le_bytes());
        out[12..16].copy_from_slice(&(header_size as u32).to_le_bytes());
        out[16..18].copy_from_slice(&width.to_le_bytes());
        out[18..20].copy_from_slice(&height.to_le_bytes());
        out[20..24].copy_from_slice(&flags.to_le_bytes());
        out[24..26].copy_from_slice(&1u16.to_le_bytes());
        out[52..56].copy_from_slice(&format.to_le_bytes());
        out[56] = levels.len() as u8;
        out[57..61].copy_from_slice(&13i32.to_le_bytes());
        out[61] = 1;
        out[62] = 1;
        if minor >= 2 {
            out[63..65].copy_from_slice(&1u16.to_le_bytes());
        }
        let thumb_at = out.len();
        out.extend_from_slice(&[0; 8]);
        let data_at = out.len();
        for l in levels.iter().rev() {
            out.extend_from_slice(l);
        }
        if minor >= 3 {
            out[68..72].copy_from_slice(&2u32.to_le_bytes());
            out[80..84].copy_from_slice(&[0x01, 0, 0, 0]);
            out[84..88].copy_from_slice(&(thumb_at as u32).to_le_bytes());
            out[88..92].copy_from_slice(&[0x30, 0, 0, 0]);
            out[92..96].copy_from_slice(&(data_at as u32).to_le_bytes());
        }
        out
    }

    fn rgba(t: &Texture) -> Vec<u8> {
        match t.base() {
            Pixels::Rgba8(v) => v.clone(),
            _ => panic!("not 8-bit"),
        }
    }

    #[test]
    fn every_version_finds_its_image() {
        // A 4x2 BGR888 level and its 2x1 and 1x1 mips.
        let l0: Vec<u8> = (0..8).flat_map(|i| [i * 10, 1, 2]).collect();
        let levels = [l0, vec![9, 9, 9, 8, 8, 8], vec![7, 7, 7]];
        for minor in 0..=5 {
            let v = build(minor, 3, 4, 2, 0, &levels);
            assert!(sniff(&v));
            let t = decode(&v, None).unwrap();
            assert_eq!((t.width(), t.height()), (4, 2), "7.{minor}");
            assert_eq!(&rgba(&t)[4..8], &[2, 1, 10, 255], "7.{minor}");
            // A small limit takes a smaller level.
            let t = decode(&v, Some(2)).unwrap();
            assert_eq!((t.width(), t.height()), (2, 1));
            assert_eq!(&rgba(&t)[..4], &[9, 9, 9, 255]);
        }
    }

    #[test]
    fn decodes_the_common_formats() {
        // One 4x4 image per format.
        let cases: Vec<(i32, Vec<u8>, [u8; 4])> = vec![
            (0, [10, 20, 30, 40].repeat(16), [10, 20, 30, 40]),
            (1, [40, 30, 20, 10].repeat(16), [10, 20, 30, 40]),
            (2, [10, 20, 30].repeat(16), [10, 20, 30, 255]),
            (12, [30, 20, 10, 40].repeat(16), [10, 20, 30, 40]),
            (16, [30, 20, 10, 0].repeat(16), [10, 20, 30, 255]),
            (5, [77].repeat(16), [77, 77, 77, 255]),
            (6, [77, 5].repeat(16), [77, 77, 77, 5]),
            (8, [5].repeat(16), [255, 255, 255, 5]),
            (22, [128, 64].repeat(16), [128, 64, 0, 255]),
            (10, [255, 0, 0].repeat(16), [0, 0, 255, 0]),
            // DXT1: colour 0 white, colour 1 black, every index 0.
            (13, vec![0xff, 0xff, 0, 0, 0, 0, 0, 0], [255, 255, 255, 255]),
            // DXT5: alpha 0x80 throughout, colour red.
            (15, vec![0x80, 0x80, 0, 0, 0, 0, 0, 0, 0x00, 0xf8, 0, 0, 0, 0, 0, 0], [255, 0, 0, 0x80]),
        ];
        for (format, data, want) in cases {
            let v = build(4, format, 4, 4, 0, &[data]);
            let t = decode(&v, None).unwrap_or_else(|e| panic!("{format}: {e}"));
            assert_eq!(&rgba(&t)[..4], &want, "format {format}");
        }
        // Half floats: 1.0 and 2.0.
        let one = 0x3c00u16.to_le_bytes();
        let two = 0x4000u16.to_le_bytes();
        let px = [one, two, one, one].concat();
        let t = decode(&build(5, 24, 1, 1, 0, &[px]), None).unwrap();
        let Pixels::Rgba32F(v) = t.base() else { panic!() };
        assert_eq!(&v[..4], &[1.0, 2.0, 1.0, 1.0]);
    }

    #[test]
    fn damaged_files_fail_cleanly() {
        let v = build(5, 13, 64, 64, 0, &[vec![0u8; 16 * 16 * 8]]);
        for cut in [0, 4, 11, 60, 70, v.len() / 2, v.len() - 1] {
            assert!(decode(&v[..cut], None).is_err(), "cut at {cut}");
        }
        for m in crate::testutil::mutations(&v) {
            let _ = decode(&m, None);
            let _ = decode(&m, Some(64));
        }
        let mut huge = v.clone();
        huge[56] = 255; // mip count
        let _ = decode(&huge, None);
        let mut frames = v.clone();
        frames[24..26].copy_from_slice(&u16::MAX.to_le_bytes());
        frames[20..24].copy_from_slice(&ENVMAP.to_le_bytes());
        assert!(decode(&frames, None).is_err());
        let mut fmt = v.clone();
        fmt[52..56].copy_from_slice(&7i32.to_le_bytes());
        assert!(matches!(decode(&fmt, None), Err(LoadError::Unsupported(_))));
    }

    /// sourcepp's samples: one VTF per pixel format, all of the same
    /// 256x256 picture (each compared with the plain RGBA8888 one, so
    /// channel orders are checked), and one per header version.
    #[test]
    fn real_vtfs() {
        let Some(dir) = crate::testutil::asset("source/sourcepp-vtf") else { return };
        let reference = decode(&std::fs::read(dir.join("fmt/RGBA8888.vtf")).unwrap(), None).unwrap();
        let want = rgba(&reference);
        let mut ok = 0;
        for sub in ["fmt", "ver", "extfmt"] {
            let Ok(entries) = std::fs::read_dir(dir.join(sub)) else { continue };
            let mut names: Vec<_> = entries.flatten().map(|e| e.path()).collect();
            names.sort();
            for p in names.iter().filter(|p| p.extension().is_some_and(|e| e == "vtf")) {
                let bytes = std::fs::read(p).unwrap();
                let name = p.file_stem().unwrap().to_string_lossy().to_string();
                let t = match decode(&bytes, None) {
                    Ok(t) => t,
                    Err(e) => {
                        eprintln!("{sub}/{name}: {e}");
                        assert!(name.contains("c9"), "{name}: {e}");
                        continue;
                    }
                };
                ok += 1;
                // Mean colour difference from the source picture, for the
                // formats that keep colour.
                let colour = !["A8", "I8", "IA88", "UV88", "UVWQ8888", "UVLX8888"].contains(&name.as_str());
                let err = match (t.base(), colour && sub == "fmt" && t.width() == 256 && t.height() == 256) {
                    (Pixels::Rgba8(v), true) => {
                        let sum: u64 = v
                            .chunks_exact(4)
                            .zip(want.chunks_exact(4))
                            .filter(|(a, _)| a[3] > 0)
                            .map(|(a, b)| {
                                (0..3).map(|c| (a[c] as i32 - b[c] as i32).unsigned_abs() as u64).sum::<u64>()
                            })
                            .sum();
                        Some(sum as f64 / (v.len() / 4 * 3) as f64)
                    }
                    _ => None,
                };
                eprintln!(
                    "{sub}/{name}: {}x{} {} {}, error {err:?}",
                    t.width(),
                    t.height(),
                    t.source.container,
                    t.source.format
                );
                if let Some(e) = err {
                    let limit = if name.contains("DXT") || name.contains("565") || name.contains("5551") {
                        10.0
                    } else if name.contains("4444") {
                        12.0
                    } else {
                        1.0
                    };
                    assert!(e < limit, "{name}: error {e}");
                }
            }
        }
        assert!(ok > 30, "{ok}");
    }
}
