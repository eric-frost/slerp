//! Blizzard BLP textures: Warcraft III's BLP1 and World of Warcraft's BLP2.
//!
//! - BLP1 holds either a JPEG (a shared header plus per-mip data, four
//!   components in blue, green, red, alpha order) or a 256-colour palette
//!   with an optional alpha channel of 1, 4 or 8 bits per pixel.
//! - BLP2 holds a palette (with the same alpha depths, stored after the
//!   indices), DXT1, DXT3 or DXT5 blocks, or uncompressed BGRA.
//!
//! The mip level shown is the smallest one that still fills the size a
//! thumbnail asks for, else the largest.

use crate::common::{Block, Cursor, check_size, decode_blocks, expect_data, texture};
use slerp_scene::{LoadError, Texture};

pub fn sniff(head: &[u8]) -> bool {
    head.len() >= 8 && (&head[..4] == b"BLP1" || &head[..4] == b"BLP2")
}

/// The level to decode: the smallest whose longer side is at least `want`,
/// else level 0.  `offsets` and `sizes` are the header's 16 entries.
fn pick_level(offsets: &[u32; 16], sizes: &[u32; 16], w: u32, h: u32, want: Option<u32>) -> usize {
    let Some(want) = want else { return 0 };
    let mut best = 0;
    for i in 1..16 {
        if offsets[i] == 0 || sizes[i] == 0 {
            break;
        }
        let (lw, lh) = ((w >> i).max(1), (h >> i).max(1));
        if lw.max(lh) >= want {
            best = i;
        } else {
            break;
        }
    }
    best
}

fn read16(c: &mut Cursor) -> Result<[u32; 16], LoadError> {
    let mut a = [0u32; 16];
    for v in &mut a {
        *v = c.u32()?;
    }
    Ok(a)
}

/// The bytes of one level, clipped to what the file holds.
fn level(bytes: &[u8], offset: u32, size: u32) -> &[u8] {
    let start = (offset as usize).min(bytes.len());
    let end = start.saturating_add(size as usize).min(bytes.len());
    &bytes[start..end]
}

pub fn decode(bytes: &[u8], want: Option<u32>) -> Result<Texture, LoadError> {
    if !sniff(bytes) {
        return Err(LoadError::invalid("not a BLP file"));
    }
    if &bytes[..4] == b"BLP1" { blp1(bytes, want) } else { blp2(bytes, want) }
}

//region BLP1

fn blp1(bytes: &[u8], want: Option<u32>) -> Result<Texture, LoadError> {
    let mut c = Cursor::new(bytes);
    c.take(4)?;
    let content = c.u32()?;
    let alpha_bits = c.u32()?;
    let (w, h) = (c.u32()?, c.u32()?);
    let _picture_type = c.u32()?;
    let _sub_type = c.u32()?;
    let offsets = read16(&mut c)?;
    let sizes = read16(&mut c)?;
    check_size(w, h)?;
    let lv = pick_level(&offsets, &sizes, w, h, want);
    let (lw, lh) = ((w >> lv).max(1), (h >> lv).max(1));
    let data = level(bytes, offsets[lv], sizes[lv]);
    // JPEG data is compressed, so only palettes can be checked against the size.
    if content == 1 {
        expect_data(data.len(), lw as usize * lh as usize)?;
    }
    let (rgba, kind) = match content {
        0 => (blp1_jpeg(&mut c, data, lw, lh)?, "JPEG".to_string()),
        1 => {
            let pal = c.take(1024)?;
            let palette: Vec<[u8; 3]> = pal.chunks_exact(4).map(|p| [p[2], p[1], p[0]]).collect();
            (paletted(data, &palette, lw, lh, alpha_bits, false), format!("256-colour palette, {alpha_bits}-bit alpha"))
        }
        n => return Err(LoadError::Unsupported(format!("BLP1 content type {n}"))),
    };
    let detail = format!("BLP1 {w}x{h}, {kind}, mip {lv} of {} shown at {lw}x{lh}", mips(&offsets, &sizes));
    Ok(texture(lw, lh, rgba, "Blizzard BLP1", &detail))
}

fn mips(offsets: &[u32; 16], sizes: &[u32; 16]) -> usize {
    offsets.iter().zip(sizes).take_while(|&(&o, &s)| o != 0 && s != 0).count().max(1)
}

/// BLP1's JPEG: a header shared by every mip (its length at 156) followed,
/// at the level's offset, by that level's entropy-coded data.  The four
/// components are stored as blue, green, red and alpha.
fn blp1_jpeg(c: &mut Cursor, level: &[u8], w: u32, h: u32) -> Result<Vec<u8>, LoadError> {
    let header_len = c.u32()? as usize;
    if header_len > 1024 {
        return Err(LoadError::invalid("BLP1 JPEG header is too large"));
    }
    let header = c.take(header_len)?;
    let mut jpeg = Vec::with_capacity(header.len() + level.len());
    jpeg.extend_from_slice(header);
    jpeg.extend_from_slice(level);
    // Every mip shares one header whose frame size is the largest level's
    // (and some writers leave other values there), so the level's own size
    // goes into the first baseline frame marker, wherever the header ends.
    if let Some(at) =
        jpeg.windows(4).position(|w| w[0] == 0xff && w[1] == 0xc0 && w[3] >= 11).filter(|&at| at + 9 <= jpeg.len())
    {
        jpeg[at + 5..at + 7].copy_from_slice(&(h.min(65535) as u16).to_be_bytes());
        jpeg[at + 7..at + 9].copy_from_slice(&(w.min(65535) as u16).to_be_bytes());
    }
    let mut dec = jpeg_decoder::Decoder::new(&jpeg[..]);
    let px = dec.decode().map_err(|e| LoadError::invalid(format!("BLP1 JPEG: {e}")))?;
    let info = dec.info().ok_or_else(|| LoadError::invalid("BLP1 JPEG has no header"))?;
    if info.width as u32 != w || info.height as u32 != h {
        return Err(LoadError::invalid(format!("BLP1 JPEG is {}x{}, expected {w}x{h}", info.width, info.height)));
    }
    let n = w as usize * h as usize;
    match info.pixel_format {
        jpeg_decoder::PixelFormat::CMYK32 => {
            // jpeg-decoder gives the four components untouched (inverted
            // for Adobe CMYK, which these files are not).
            let adobe_inverted = false;
            let _ = adobe_inverted;
            Ok(px.chunks_exact(4).take(n).flat_map(|p| [p[2], p[1], p[0], p[3]]).collect())
        }
        jpeg_decoder::PixelFormat::RGB24 => {
            Ok(px.chunks_exact(3).take(n).flat_map(|p| [p[0], p[1], p[2], 255]).collect())
        }
        jpeg_decoder::PixelFormat::L8 => Ok(px.iter().take(n).flat_map(|&v| [v, v, v, 255]).collect()),
        other => Err(LoadError::Unsupported(format!("BLP1 JPEG pixel format {other:?}"))),
    }
}

//endregion

//region BLP2

fn blp2(bytes: &[u8], want: Option<u32>) -> Result<Texture, LoadError> {
    let mut c = Cursor::new(bytes);
    c.take(4)?;
    let version = c.u32()?;
    if version != 1 {
        return Err(LoadError::Unsupported(format!("BLP version {version}")));
    }
    let (encoding, alpha_depth, alpha_encoding, has_mips) = (c.u8()?, c.u8()?, c.u8()?, c.u8()?);
    let (w, h) = (c.u32()?, c.u32()?);
    let offsets = read16(&mut c)?;
    let sizes = read16(&mut c)?;
    let pal = c.take(1024)?;
    check_size(w, h)?;
    let lv = if has_mips != 0 { pick_level(&offsets, &sizes, w, h, want) } else { 0 };
    let (lw, lh) = ((w >> lv).max(1), (h >> lv).max(1));
    let data = level(bytes, offsets[lv], sizes[lv]);
    expect_data(
        data.len(),
        match encoding {
            2 => Block::Dxt1.level_bytes(lw, lh),
            3 => lw as usize * lh as usize * 4,
            _ => lw as usize * lh as usize,
        },
    )?;
    let (rgba, kind) = match encoding {
        1 => {
            let palette: Vec<[u8; 3]> = pal.chunks_exact(4).map(|p| [p[2], p[1], p[0]]).collect();
            (
                paletted(data, &palette, lw, lh, alpha_depth as u32, true),
                format!("256-colour palette, {alpha_depth}-bit alpha"),
            )
        }
        2 => {
            let (format, name) = match alpha_encoding {
                1 => (Block::Dxt3, "DXT3"),
                7 => (Block::Dxt5, "DXT5"),
                _ => (Block::Dxt1, "DXT1"),
            };
            (decode_blocks(format, data, lw, lh), name.to_string())
        }
        3 => {
            let n = lw as usize * lh as usize;
            let mut out: Vec<u8> = data.chunks_exact(4).take(n).flat_map(|p| [p[2], p[1], p[0], p[3]]).collect();
            out.resize(n * 4, 0);
            (out, "uncompressed BGRA".to_string())
        }
        n => return Err(LoadError::Unsupported(format!("BLP2 encoding {n}"))),
    };
    let detail = format!("BLP2 {w}x{h}, {kind}, mip {lv} of {} shown at {lw}x{lh}", mips(&offsets, &sizes));
    Ok(texture(lw, lh, rgba, "Blizzard BLP2", &detail))
}

//endregion

/// Palette indices plus an alpha plane of `alpha_bits` (0, 1, 4 or 8) bits
/// per pixel behind them.  BLP2 packs 1-bit alpha least significant bit
/// first and 4-bit alpha low nibble first; BLP1 packs alpha the same way.
fn paletted(data: &[u8], palette: &[[u8; 3]], w: u32, h: u32, alpha_bits: u32, _blp2: bool) -> Vec<u8> {
    let n = w as usize * h as usize;
    let mut out = vec![0u8; n * 4];
    let alpha = data.get(n..).unwrap_or(&[]);
    for i in 0..n {
        let Some(&idx) = data.get(i) else { break };
        let c = palette.get(idx as usize).copied().unwrap_or([0, 0, 0]);
        let a = match alpha_bits {
            1 => alpha.get(i / 8).map_or(255, |b| if (b >> (i % 8)) & 1 != 0 { 255 } else { 0 }),
            4 => alpha.get(i / 2).map_or(255, |b| (if i % 2 == 0 { b & 15 } else { b >> 4 }) * 17),
            8 => alpha.get(i).copied().unwrap_or(255),
            _ => 255,
        };
        out[i * 4..i * 4 + 4].copy_from_slice(&[c[0], c[1], c[2], a]);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use slerp_scene::textures::Pixels;

    fn px(t: &Texture) -> &[u8] {
        let Pixels::Rgba8(v) = t.base() else { panic!() };
        v
    }

    /// A BLP2 header with one mip chain entry per given level.
    fn blp2_file(
        encoding: u8,
        alpha_depth: u8,
        alpha_enc: u8,
        w: u32,
        h: u32,
        palette: &[u8; 1024],
        levels: &[Vec<u8>],
    ) -> Vec<u8> {
        let mut f = b"BLP2".to_vec();
        f.extend(1u32.to_le_bytes());
        f.extend([encoding, alpha_depth, alpha_enc, (levels.len() > 1) as u8]);
        f.extend(w.to_le_bytes());
        f.extend(h.to_le_bytes());
        let mut offsets = [0u32; 16];
        let mut sizes = [0u32; 16];
        let mut at = 148 + 1024;
        for (i, l) in levels.iter().enumerate() {
            offsets[i] = at as u32;
            sizes[i] = l.len() as u32;
            at += l.len();
        }
        for o in offsets {
            f.extend(o.to_le_bytes());
        }
        for s in sizes {
            f.extend(s.to_le_bytes());
        }
        f.extend(palette);
        for l in levels {
            f.extend(l);
        }
        f
    }

    fn palette() -> [u8; 1024] {
        let mut p = [0u8; 1024];
        // Entry 1: red (stored BGRA), entry 2: green.
        p[4..8].copy_from_slice(&[0, 0, 255, 255]);
        p[8..12].copy_from_slice(&[0, 255, 0, 255]);
        p
    }

    #[test]
    fn blp2_paletted_with_each_alpha_depth() {
        let idx = [1u8, 2, 2, 1];
        let a8 = [idx.to_vec(), vec![255, 128, 0, 64]].concat();
        let t = decode(&blp2_file(1, 8, 0, 2, 2, &palette(), &[a8]), None).unwrap();
        assert_eq!(&px(&t)[..8], &[255, 0, 0, 255, 0, 255, 0, 128]);
        assert_eq!(px(&t)[11], 0);
        // 4-bit alpha: low nibble first.
        let a4 = [idx.to_vec(), vec![0xf1, 0x8a]].concat();
        let t = decode(&blp2_file(1, 4, 0, 2, 2, &palette(), &[a4]), None).unwrap();
        assert_eq!(px(&t)[3], 17);
        assert_eq!(px(&t)[7], 255);
        assert_eq!(px(&t)[11], 170);
        // 1-bit alpha: least significant bit first.
        let a1 = [idx.to_vec(), vec![0b0000_0101]].concat();
        let t = decode(&blp2_file(1, 1, 0, 2, 2, &palette(), &[a1]), None).unwrap();
        assert_eq!([px(&t)[3], px(&t)[7], px(&t)[11], px(&t)[15]], [255, 0, 255, 0]);
        assert!(t.source.format.contains("BLP2"));
    }

    #[test]
    fn blp2_dxt_and_uncompressed() {
        let block = [0x00, 0xf8, 0x00, 0x00, 0, 0, 0, 0];
        let t = decode(&blp2_file(2, 0, 0, 4, 4, &palette(), &[block.to_vec()]), None).unwrap();
        assert_eq!(&px(&t)[..4], &[255, 0, 0, 255]);
        let mut dxt5 = vec![200u8, 200, 0, 0, 0, 0, 0, 0];
        dxt5.extend(block);
        let t = decode(&blp2_file(2, 8, 7, 4, 4, &palette(), &[dxt5]), None).unwrap();
        assert_eq!(&px(&t)[..4], &[255, 0, 0, 200]);
        let bgra = [10u8, 20, 30, 40];
        let t = decode(&blp2_file(3, 8, 0, 1, 1, &palette(), &[bgra.to_vec()]), None).unwrap();
        assert_eq!(px(&t), &[30, 20, 10, 40]);
    }

    #[test]
    fn mip_selection_follows_the_requested_size() {
        let l0 = vec![1u8; 16];
        let l1 = vec![2u8; 4];
        let f = blp2_file(1, 0, 0, 4, 4, &palette(), &[l0, l1, vec![1]]);
        assert_eq!(decode(&f, None).unwrap().width(), 4);
        assert_eq!(decode(&f, Some(2)).unwrap().width(), 2);
        assert_eq!(decode(&f, Some(1000)).unwrap().width(), 4);
    }

    fn blp1_paletted(w: u32, h: u32, alpha_bits: u32, data: &[u8]) -> Vec<u8> {
        let mut f = b"BLP1".to_vec();
        for v in [1u32, alpha_bits, w, h, 4, 1] {
            f.extend(v.to_le_bytes());
        }
        let mut offsets = [0u32; 16];
        let mut sizes = [0u32; 16];
        offsets[0] = 156 + 1024;
        sizes[0] = data.len() as u32;
        for o in offsets {
            f.extend(o.to_le_bytes());
        }
        for s in sizes {
            f.extend(s.to_le_bytes());
        }
        f.extend(palette());
        f.extend(data);
        f
    }

    #[test]
    fn blp1_paletted_with_alpha() {
        let t = decode(&blp1_paletted(2, 1, 8, &[1, 2, 100, 200]), None).unwrap();
        assert_eq!(px(&t), &[255, 0, 0, 100, 0, 255, 0, 200]);
        assert_eq!(t.source.container, "Blizzard BLP1");
        let t = decode(&blp1_paletted(2, 1, 0, &[2, 1]), None).unwrap();
        assert_eq!(px(&t), &[0, 255, 0, 255, 255, 0, 0, 255]);
    }

    #[test]
    fn size_bombs_are_refused_before_allocating() {
        // 16384 x 16384 BLP2 in a file of a few hundred bytes (the pixel cap
        // would allow 8192 x 8192).
        for (encoding, alpha) in [(1u8, 0u8), (2, 0), (3, 8)] {
            let f = blp2_file(encoding, alpha, 0, 8192, 8192, &palette(), &[vec![0u8; 64]]);
            assert!(decode(&f, None).is_err(), "encoding {encoding}");
        }
        let f = blp1_paletted(8192, 8192, 8, &[1; 64]);
        assert!(decode(&f, None).is_err());
    }

    #[test]
    fn damaged_files_never_panic() {
        let f = blp1_paletted(4, 4, 8, &[1; 32]);
        for len in 0..f.len() {
            let _ = decode(&f[..len], None);
        }
        let g = blp2_file(2, 0, 0, 8, 8, &palette(), &[vec![0u8; 32]]);
        for len in 0..g.len() {
            let _ = decode(&g[..len], None);
        }
        let mut huge = g.clone();
        huge[12..16].copy_from_slice(&0xffff_ffffu32.to_le_bytes());
        assert!(decode(&huge, None).is_err());
        let mut seed = 99u32;
        for _ in 0..300 {
            let mut m = g.clone();
            for _ in 0..8 {
                seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                let at = (seed >> 8) as usize % m.len();
                m[at] = (seed >> 24) as u8;
            }
            let _ = decode(&m, Some(64));
        }
    }
}
