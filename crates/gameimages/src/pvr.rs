//! PowerVR textures (`.pvr`, versions 1 to 3) and Ericsson's `.pkm` ETC
//! containers.
//!
//! PVR v3 names its pixel format with a 64-bit code: a small number for
//! compressed formats (PVRTC 1, ETC1, ETC2, DXT) or four channel letters
//! plus their bit depths for uncompressed ones.  The older v1 and v2
//! headers (44 or 52 bytes) carry a format number in their flags, may have
//! the pixels stored in Morton ("twiddled") order, and use the same
//! PVRTC data.  PKM is a 16-byte header before ETC1 or ETC2 blocks.
//!
//! Only the largest mip level is decoded.  PVRTC II, EAC, BC4-7 and ASTC
//! are reported as unsupported.

use crate::common::{Block, Cursor, check_size, decode_blocks, decode_blocks_ordered, expect_data, texture};
use slerp_scene::{LoadError, Texture};

/// `named_pvr`: the file is called `.pvr`, which the older headers (with no
/// magic number of their own, in v1) need before they are believed.
pub fn sniff_pvr(head: &[u8], named_pvr: bool) -> bool {
    if head.starts_with(b"PVR\x03") {
        return true;
    }
    let word = |at: usize| u32::from_le_bytes([head[at], head[at + 1], head[at + 2], head[at + 3]]);
    if head.len() < 52 {
        return false;
    }
    match word(0) {
        // v2 ends its header with the tag "PVR!".
        52 => &head[44..48] == b"PVR!" && word(8) > 0 && word(4) > 0,
        // v1 has the same fields without the tag; the format byte and size
        // check keep other files out.
        44 => {
            named_pvr
                && word(8) > 0
                && word(8) <= 16384
                && word(4) > 0
                && word(4) <= 16384
                && legacy_format(word(16) as u8).is_some()
        }
        _ => false,
    }
}

pub fn sniff_pkm(head: &[u8]) -> bool {
    head.len() >= 16 && &head[..4] == b"PKM " && matches!(&head[4..6], b"10" | b"20")
}

//region PVR

pub fn decode_pvr(bytes: &[u8]) -> Result<Texture, LoadError> {
    if bytes.starts_with(b"PVR\x03") { pvr3(bytes) } else { pvr_legacy(bytes) }
}

/// The compressed or uncompressed layout a PVR holds.
enum Layout {
    Blocks(Block),
    /// Bytes per pixel and a converter from one pixel's bytes to RGBA.
    Raw {
        bytes: usize,
        convert: fn(&[u8]) -> [u8; 4],
        name: &'static str,
    },
}

fn pvr3(bytes: &[u8]) -> Result<Texture, LoadError> {
    let mut c = Cursor::new(bytes);
    c.take(4)?;
    let _flags = c.u32()?;
    let (lo, hi) = (c.u32()?, c.u32()?);
    let colour_space = c.u32()?;
    let channel_type = c.u32()?;
    let (h, w, depth, surfaces, faces, mips, meta) =
        (c.u32()?, c.u32()?, c.u32()?, c.u32()?, c.u32()?, c.u32()?, c.u32()?);
    check_size(w, h)?;
    let layout = if hi == 0 {
        match lo {
            0 => Layout::Blocks(Block::Pvrtc { two_bpp: true }),
            1 => Layout::Blocks(Block::Pvrtc { two_bpp: true }),
            2 | 3 => Layout::Blocks(Block::Pvrtc { two_bpp: false }),
            6 => Layout::Blocks(Block::Etc1),
            7 => Layout::Blocks(Block::Dxt1),
            9 => Layout::Blocks(Block::Dxt3),
            11 => Layout::Blocks(Block::Dxt5),
            22 => Layout::Blocks(Block::Etc2Rgb),
            23 => Layout::Blocks(Block::Etc2Rgba8),
            24 => Layout::Blocks(Block::Etc2Rgba1),
            4 | 5 => return Err(LoadError::Unsupported("PVRTC II".into())),
            n => return Err(LoadError::Unsupported(format!("PVR pixel format {n}"))),
        }
    } else {
        // Channel letters in the low bytes, bit depths in the high bytes.
        let names = lo.to_le_bytes();
        let depths = hi.to_le_bytes();
        let order: String = names.iter().filter(|&&b| b != 0).map(|&b| b as char).collect();
        let bits: Vec<u8> = depths.iter().copied().filter(|&b| b != 0).collect();
        if !matches!(channel_type, 0 | 2 | 4 | 6) {
            return Err(LoadError::Unsupported(format!("PVR channel type {channel_type}")));
        }
        match (order.as_str(), bits.as_slice()) {
            ("rgba", [8, 8, 8, 8]) => Layout::Raw { bytes: 4, convert: |p| [p[0], p[1], p[2], p[3]], name: "RGBA8888" },
            ("bgra", [8, 8, 8, 8]) => Layout::Raw { bytes: 4, convert: |p| [p[2], p[1], p[0], p[3]], name: "BGRA8888" },
            ("rgb", [8, 8, 8]) => Layout::Raw { bytes: 3, convert: |p| [p[0], p[1], p[2], 255], name: "RGB888" },
            ("rgb", [5, 6, 5]) => Layout::Raw { bytes: 2, convert: rgb565, name: "RGB565" },
            ("rgba", [4, 4, 4, 4]) => Layout::Raw { bytes: 2, convert: rgba4444, name: "RGBA4444" },
            ("rgba", [5, 5, 5, 1]) => Layout::Raw { bytes: 2, convert: rgba5551, name: "RGBA5551" },
            ("l", [8]) => Layout::Raw { bytes: 1, convert: |p| [p[0], p[0], p[0], 255], name: "L8" },
            ("a", [8]) => Layout::Raw { bytes: 1, convert: |p| [255, 255, 255, p[0]], name: "A8" },
            ("la", [8, 8]) => Layout::Raw { bytes: 2, convert: |p| [p[0], p[0], p[0], p[1]], name: "LA88" },
            (o, b) => return Err(LoadError::Unsupported(format!("PVR pixel layout {o} {b:?}"))),
        }
    };
    c.take(meta as usize)?;
    let data = bytes.get(c.pos..).unwrap_or(&[]);
    let (rgba, name) = decode_layout(&layout, data, w, h, false)?;
    let space = if colour_space == 1 { "sRGB" } else { "linear" };
    let detail =
        format!("PVR v3 {name} {w}x{h}, {space}, {mips} mips, {surfaces} surfaces, {faces} faces, depth {depth}");
    Ok(texture(w, h, rgba, "PowerVR PVR", &detail))
}

fn rgb565(p: &[u8]) -> [u8; 4] {
    let v = u16::from_le_bytes([p[0], p[1]]);
    [(((v >> 11) & 31) * 255 / 31) as u8, (((v >> 5) & 63) * 255 / 63) as u8, ((v & 31) * 255 / 31) as u8, 255]
}

fn rgba4444(p: &[u8]) -> [u8; 4] {
    let v = u16::from_le_bytes([p[0], p[1]]);
    [((v >> 12) * 17) as u8, (((v >> 8) & 15) * 17) as u8, (((v >> 4) & 15) * 17) as u8, ((v & 15) * 17) as u8]
}

fn argb4444(p: &[u8]) -> [u8; 4] {
    let v = u16::from_le_bytes([p[0], p[1]]);
    [(((v >> 8) & 15) * 17) as u8, (((v >> 4) & 15) * 17) as u8, ((v & 15) * 17) as u8, ((v >> 12) * 17) as u8]
}

fn rgba5551(p: &[u8]) -> [u8; 4] {
    let v = u16::from_le_bytes([p[0], p[1]]);
    [
        ((v >> 11) * 255 / 31) as u8,
        (((v >> 6) & 31) * 255 / 31) as u8,
        (((v >> 1) & 31) * 255 / 31) as u8,
        if v & 1 != 0 { 255 } else { 0 },
    ]
}

fn decode_layout(layout: &Layout, data: &[u8], w: u32, h: u32, twiddled: bool) -> Result<(Vec<u8>, String), LoadError> {
    match layout {
        Layout::Blocks(b) => {
            expect_data(data.len(), b.level_bytes(w, h))?;
            Ok((decode_blocks(*b, data, w, h), b.name().to_string()))
        }
        Layout::Raw { bytes, convert, name } => {
            let n = w as usize * h as usize;
            expect_data(data.len(), n * bytes)?;
            if twiddled && (w != h || !w.is_power_of_two()) {
                return Err(LoadError::Unsupported("twiddled PVR that is not a square power of two".into()));
            }
            let mut out = vec![0u8; n * 4];
            for y in 0..h as usize {
                for x in 0..w as usize {
                    let i = if twiddled { morton(x, y) } else { y * w as usize + x };
                    if let Some(p) = data.get(i * bytes..(i + 1) * bytes) {
                        out[(y * w as usize + x) * 4..(y * w as usize + x) * 4 + 4].copy_from_slice(&convert(p));
                    }
                }
            }
            Ok((out, name.to_string()))
        }
    }
}

/// Interleaves the bits of `x` (even positions) and `y` (odd positions).
fn morton(x: usize, y: usize) -> usize {
    let spread = |mut v: usize| {
        let mut r = 0;
        let mut bit = 0;
        while v != 0 {
            r |= (v & 1) << (2 * bit);
            v >>= 1;
            bit += 1;
        }
        r
    };
    spread(x) | (spread(y) << 1)
}

/// The pixel format byte of a v1 or v2 header, as a layout.
fn legacy_format(t: u8) -> Option<(Layout, &'static str)> {
    Some(match t {
        0x0c | 0x18 => (Layout::Blocks(Block::Pvrtc { two_bpp: true }), "PVRTC 2bpp"),
        0x0d | 0x19 => (Layout::Blocks(Block::Pvrtc { two_bpp: false }), "PVRTC 4bpp"),
        0x20 => (Layout::Blocks(Block::Dxt1), "DXT1"),
        0x22 => (Layout::Blocks(Block::Dxt3), "DXT3"),
        0x24 => (Layout::Blocks(Block::Dxt5), "DXT5"),
        0x36 => (Layout::Blocks(Block::Etc1), "ETC1"),
        0x00 => (Layout::Raw { bytes: 2, convert: argb4444, name: "ARGB4444" }, "ARGB4444"),
        0x10 => (Layout::Raw { bytes: 2, convert: rgba4444, name: "RGBA4444" }, "RGBA4444"),
        0x11 => (Layout::Raw { bytes: 2, convert: rgba5551, name: "RGBA5551" }, "RGBA5551"),
        0x12 => (Layout::Raw { bytes: 4, convert: |p| [p[0], p[1], p[2], p[3]], name: "RGBA8888" }, "RGBA8888"),
        0x05 | 0x1a => (Layout::Raw { bytes: 4, convert: |p| [p[2], p[1], p[0], p[3]], name: "BGRA8888" }, "BGRA8888"),
        0x02 | 0x13 => (Layout::Raw { bytes: 2, convert: rgb565, name: "RGB565" }, "RGB565"),
        0x04 | 0x15 => (Layout::Raw { bytes: 3, convert: |p| [p[0], p[1], p[2], 255], name: "RGB888" }, "RGB888"),
        0x07 | 0x16 => (Layout::Raw { bytes: 1, convert: |p| [p[0], p[0], p[0], 255], name: "I8" }, "I8"),
        0x1b => (Layout::Raw { bytes: 1, convert: |p| [255, 255, 255, p[0]], name: "A8" }, "A8"),
        0x08 | 0x17 => (Layout::Raw { bytes: 2, convert: |p| [p[0], p[0], p[0], p[1]], name: "AI88" }, "AI88"),
        _ => return None,
    })
}

fn pvr_legacy(bytes: &[u8]) -> Result<Texture, LoadError> {
    let mut c = Cursor::new(bytes);
    let header = c.u32()? as usize;
    if header != 44 && header != 52 {
        return Err(LoadError::invalid("not a PVR header"));
    }
    let (h, w, mips, flags, _data_size, _bpp) = (c.u32()?, c.u32()?, c.u32()?, c.u32()?, c.u32()?, c.u32()?);
    check_size(w, h)?;
    let (layout, name) = legacy_format((flags & 0xff) as u8)
        .ok_or_else(|| LoadError::Unsupported(format!("PVR pixel type {:#04x}", flags & 0xff)))?;
    let data = bytes.get(header..).unwrap_or(&[]);
    let twiddled = flags & 0x200 != 0 && matches!(layout, Layout::Raw { .. });
    let (rgba, _) = decode_layout(&layout, data, w, h, twiddled)?;
    let version = if header == 52 { 2 } else { 1 };
    let extra = if twiddled { ", twiddled" } else { "" };
    let detail = format!("PVR v{version} {name} {w}x{h}, {} mips{extra}", mips.saturating_add(1));
    Ok(texture(w, h, rgba, "PowerVR PVR", &detail))
}

//endregion

//region PKM

pub fn decode_pkm(bytes: &[u8]) -> Result<Texture, LoadError> {
    if !sniff_pkm(bytes) {
        return Err(LoadError::invalid("not a PKM file"));
    }
    let be = |at: usize| u16::from_be_bytes([bytes[at], bytes[at + 1]]) as u32;
    let (kind, ew, eh, w, h) = (be(6), be(8), be(10), be(12), be(14));
    check_size(ew.max(1), eh.max(1))?;
    check_size(w.max(1), h.max(1))?;
    if w > ew || h > eh {
        return Err(LoadError::invalid("the PKM's picture is larger than the blocks that cover it"));
    }
    let (format, name) = match kind {
        0 => (Block::Etc1, "ETC1"),
        1 => (Block::Etc2Rgb, "ETC2 RGB"),
        2 | 3 => (Block::Etc2Rgba8, "ETC2 RGBA"),
        4 => (Block::Etc2Rgba1, "ETC2 RGB+A1"),
        n => return Err(LoadError::Unsupported(format!("PKM format {n} (EAC)"))),
    };
    let data = &bytes[16..];
    expect_data(data.len(), format.level_bytes(ew, eh))?;
    // Writers disagree on the order of the blocks (Ericsson's tools store
    // columns, most others rows); the right order has the smoother seams
    // between blocks.
    let rows = decode_blocks_ordered(format, data, ew, eh, false);
    let cols = decode_blocks_ordered(format, data, ew, eh, true);
    let column_major = seam_error(&cols, ew, eh) < seam_error(&rows, ew, eh);
    let full = if column_major { cols } else { rows };
    let mut rgba = Vec::with_capacity(w as usize * h as usize * 4);
    for y in 0..h as usize {
        let start = y * ew as usize * 4;
        rgba.extend_from_slice(&full[start..start + w as usize * 4]);
    }
    let order = if column_major { "blocks stored by column" } else { "blocks stored by row" };
    let detail = format!(
        "PKM v{} {name} {w}x{h} (blocks cover {ew}x{eh}), {order}",
        std::str::from_utf8(&bytes[4..6]).unwrap_or("?")
    );
    Ok(texture(w, h, rgba, "Ericsson PKM", &detail))
}

/// How much neighbouring pixels differ across the seams between 4x4 blocks:
/// large when blocks are in the wrong places.
fn seam_error(rgba: &[u8], w: u32, h: u32) -> u64 {
    let (w, h) = (w as usize, h as usize);
    let px = |x: usize, y: usize| &rgba[(y * w + x) * 4..(y * w + x) * 4 + 3];
    let diff =
        |a: &[u8], b: &[u8]| a.iter().zip(b).map(|(&p, &q)| (p as i32 - q as i32).unsigned_abs() as u64).sum::<u64>();
    let mut total = 0;
    for y in 0..h {
        for x in (4..w).step_by(4) {
            total += diff(px(x - 1, y), px(x, y));
        }
    }
    for y in (4..h).step_by(4) {
        for x in 0..w {
            total += diff(px(x, y - 1), px(x, y));
        }
    }
    total
}

//endregion

#[cfg(test)]
mod tests {
    use super::*;
    use slerp_scene::textures::Pixels;

    fn px(t: &Texture) -> &[u8] {
        let Pixels::Rgba8(v) = t.base() else { panic!() };
        v
    }

    fn pvr3_header(lo: u32, hi: u32, w: u32, h: u32, mips: u32) -> Vec<u8> {
        let mut f = b"PVR\x03".to_vec();
        for v in [0u32, lo, hi, 0, 0, h, w, 1, 1, 1, mips, 0] {
            f.extend(v.to_le_bytes());
        }
        f
    }

    /// Found by fuzzing: a PKM whose padded height is 0 but which has block
    /// data divided by zero blocks per column while guessing the order.
    #[test]
    fn pkm_of_no_rows_is_harmless() {
        let mut f = b"PKM 10".to_vec();
        f.extend([0, 0, 0, 4, 0, 0, 0, 0, 0, 0]);
        f.extend([0u8; 85]);
        let _ = decode_pkm(&f);
    }

    #[test]
    fn pvr3_uncompressed_formats() {
        let mut f = pvr3_header(u32::from_le_bytes(*b"rgba"), u32::from_le_bytes([8, 8, 8, 8]), 2, 1, 1);
        f.extend([1, 2, 3, 4, 5, 6, 7, 8]);
        assert!(sniff_pvr(&f, false));
        let t = decode_pvr(&f).unwrap();
        assert_eq!(px(&t), &[1, 2, 3, 4, 5, 6, 7, 8]);
        let mut f = pvr3_header(u32::from_le_bytes(*b"bgra"), u32::from_le_bytes([8, 8, 8, 8]), 1, 1, 1);
        f.extend([1, 2, 3, 4]);
        assert_eq!(px(&decode_pvr(&f).unwrap()), &[3, 2, 1, 4]);
        let mut f = pvr3_header(u32::from_le_bytes([b'r', b'g', b'b', 0]), u32::from_le_bytes([5, 6, 5, 0]), 1, 1, 1);
        f.extend(0xf800u16.to_le_bytes());
        assert_eq!(px(&decode_pvr(&f).unwrap()), &[255, 0, 0, 255]);
    }

    #[test]
    fn pvr3_block_formats() {
        let block = [0x00, 0xf8, 0x00, 0x00, 0, 0, 0, 0];
        let mut f = pvr3_header(7, 0, 4, 4, 1);
        f.extend(block);
        assert_eq!(&px(&decode_pvr(&f).unwrap())[..4], &[255, 0, 0, 255]);
        // PVRTC 4bpp: an 8x8 texture is 32 bytes; all-zero data decodes to
        // something without panicking.
        let mut f = pvr3_header(3, 0, 8, 8, 1);
        f.extend([0u8; 32]);
        assert_eq!(decode_pvr(&f).unwrap().width(), 8);
        let f = pvr3_header(5, 0, 8, 8, 1);
        assert!(decode_pvr(&f).is_err());
    }

    #[test]
    fn legacy_headers_and_twiddling() {
        // v2: 52-byte header, 4x4 RGBA8888 (0x12) twiddled.
        let mut f = Vec::new();
        for v in [52u32, 4, 4, 0, 0x12 | 0x200, 64, 32, 0, 0, 0, 0] {
            f.extend(v.to_le_bytes());
        }
        f.extend(b"PVR!");
        f.extend(1u32.to_le_bytes());
        assert!(sniff_pvr(&f, false));
        // Twiddled order: the pixel at (x, y) is at morton(x, y).
        let mut data = vec![0u8; 64];
        data[morton(1, 0) * 4..morton(1, 0) * 4 + 4].copy_from_slice(&[9, 8, 7, 6]);
        data[morton(0, 1) * 4..morton(0, 1) * 4 + 4].copy_from_slice(&[1, 2, 3, 4]);
        f.extend(data);
        let t = decode_pvr(&f).unwrap();
        assert_eq!(&px(&t)[4..8], &[9, 8, 7, 6]);
        assert_eq!(&px(&t)[16..20], &[1, 2, 3, 4]);
        assert!(t.source.format.contains("twiddled"));
        assert_eq!(morton(1, 0), 1);
        assert_eq!(morton(0, 1), 2);
        assert_eq!(morton(3, 3), 15);
    }

    /// Two ETC1 blocks (differential mode would be needed for real
    /// pictures; here individual mode with flat colours) side by side.
    fn flat_etc1(colour: u8) -> [u8; 8] {
        // Individual mode: two 4-bit base colours, diff bit 0, flip bit 0,
        // modifier table 0 and pixel indices zero: modifier +2 on every
        // pixel (index 0 maps to +2 with table 0: [2, 8, -2, -8]).
        let c = colour >> 4;
        [c << 4 | c, c << 4 | c, c << 4 | c, 0, 0, 0, 0, 0]
    }

    #[test]
    fn pkm_decodes_and_finds_the_block_order() {
        // A 12x8 picture that brightens from left to right in 4-pixel
        // steps, stored by column: two blocks per column, so the stream is
        // s0 s0 s1 s1 s2 s2.  Read by row it would put s1 at the top right
        // of the first row and scramble the seams.
        let shades = [0x20u8, 0x50, 0x80];
        let mut f = b"PKM 10".to_vec();
        f.extend(0u16.to_be_bytes());
        for v in [12u16, 8, 12, 8] {
            f.extend(v.to_be_bytes());
        }
        for s in shades {
            f.extend(flat_etc1(s));
            f.extend(flat_etc1(s));
        }
        let t = decode_pkm(&f).unwrap();
        assert_eq!((t.width(), t.height()), (12, 8));
        assert!(sniff_pkm(&f));
        let p = px(&t);
        let at = |x: usize, y: usize| p[(y * 12 + x) * 4];
        assert_eq!(at(0, 0), at(0, 4));
        assert_eq!(at(4, 0), at(4, 4));
        assert!(at(0, 0) < at(4, 0) && at(4, 0) < at(8, 0));
        assert!(t.source.format.contains("by column"));
        // The same blocks stored by row decode as rows.
        let mut g = f[..16].to_vec();
        for _ in 0..2 {
            for s in shades {
                g.extend(flat_etc1(s));
            }
        }
        let t = decode_pkm(&g).unwrap();
        assert!(t.source.format.contains("by row"));
        assert!(px(&t)[0] < px(&t)[8 * 4]);
    }

    #[test]
    fn size_bombs_are_refused_before_allocating() {
        for lo in [3u32, 7, 11, 6] {
            let mut f = pvr3_header(lo, 0, 8192, 8192, 1);
            f.extend([0u8; 64]);
            assert!(decode_pvr(&f).is_err(), "format {lo}");
        }
        let mut f = pvr3_header(u32::from_le_bytes(*b"rgba"), u32::from_le_bytes([8, 8, 8, 8]), 8192, 8192, 1);
        f.extend([0u8; 64]);
        assert!(decode_pvr(&f).is_err());
        let mut k = b"PKM 10".to_vec();
        k.extend(0u16.to_be_bytes());
        for v in [8192u16, 8192, 8192, 8192] {
            k.extend(v.to_be_bytes());
        }
        k.extend([0u8; 64]);
        assert!(decode_pkm(&k).is_err());
    }

    #[test]
    fn damaged_files_never_panic() {
        let mut f = pvr3_header(3, 0, 8, 8, 1);
        f.extend([0xffu8; 32]);
        for len in 0..f.len() {
            let _ = decode_pvr(&f[..len]);
            let _ = sniff_pvr(&f[..len], true);
        }
        let mut huge = pvr3_header(3, 0, 70000, 70000, 1);
        huge.extend([0u8; 32]);
        assert!(decode_pvr(&huge).is_err());
        let mut k = b"PKM 20".to_vec();
        k.extend(3u16.to_be_bytes());
        for v in [65535u16, 65535, 65535, 65535] {
            k.extend(v.to_be_bytes());
        }
        k.extend([0u8; 64]);
        assert!(decode_pkm(&k).is_err());
        // An original size beyond the block-covered size.
        let mut k = b"PKM 10".to_vec();
        k.extend(0u16.to_be_bytes());
        for v in [8u16, 8, 100, 100] {
            k.extend(v.to_be_bytes());
        }
        k.extend([0u8; 64]);
        assert!(decode_pkm(&k).is_err());
        for len in 0..30 {
            let _ = decode_pkm(&k[..len.min(k.len())]);
        }
    }
}
