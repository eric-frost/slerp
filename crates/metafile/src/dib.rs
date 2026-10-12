//! Device-independent bitmaps (DIBs), the pictures inside metafiles: a
//! `BITMAPINFO` header, an optional colour table and the pixel rows.
//! Everything is decoded to straight-alpha RGBA, whatever the depth or
//! compression, with sizes checked before anything is allocated.

use crate::Rgb;

/// The most pixels one bitmap may have.  A metafile can carry several; the
/// cap keeps a hostile header from asking for gigabytes.
pub const MAX_PIXELS: u64 = 1 << 24;

/// A decoded bitmap.
#[derive(Clone, Debug)]
pub struct Bitmap {
    pub width: u32,
    pub height: u32,
    /// Straight (not premultiplied) RGBA, top row first.
    pub rgba: Vec<u8>,
    /// True for one-bit device-dependent bitmaps: GDI paints those with
    /// the device context's text and background colours.
    pub mono: bool,
    /// True when a 32-bit bitmap's fourth byte holds anything: most
    /// writers leave it zero, which must not mean transparent.
    pub has_alpha: bool,
}

impl Bitmap {
    /// Forces every pixel opaque, which is how GDI treats bitmaps outside
    /// `AlphaBlend`.
    pub fn make_opaque(&mut self) {
        for p in self.rgba.chunks_exact_mut(4) {
            p[3] = 255;
        }
        self.has_alpha = false;
    }

    /// Premultiplied pixels for tiny-skia.
    pub fn to_pixmap(&self) -> Option<tiny_skia::Pixmap> {
        let mut pm = tiny_skia::Pixmap::new(self.width, self.height)?;
        for (d, s) in pm.data_mut().chunks_exact_mut(4).zip(self.rgba.chunks_exact(4)) {
            let a = s[3] as u32;
            if a == 255 {
                d.copy_from_slice(s);
            } else {
                d[0] = ((s[0] as u32 * a + 127) / 255) as u8;
                d[1] = ((s[1] as u32 * a + 127) / 255) as u8;
                d[2] = ((s[2] as u32 * a + 127) / 255) as u8;
                d[3] = a as u8;
            }
        }
        Some(pm)
    }

    /// The pixel at `(x, y)` as RGB, clamping coordinates to the bitmap.
    pub fn pixel(&self, x: i64, y: i64) -> [u8; 4] {
        let x = x.clamp(0, self.width as i64 - 1) as usize;
        let y = y.clamp(0, self.height as i64 - 1) as usize;
        let i = (y * self.width as usize + x) * 4;
        [self.rgba[i], self.rgba[i + 1], self.rgba[i + 2], self.rgba[i + 3]]
    }
}

fn u16_at(b: &[u8], i: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(i..i.checked_add(2)?)?.try_into().ok()?))
}

fn u32_at(b: &[u8], i: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(i..i.checked_add(4)?)?.try_into().ok()?))
}

fn i32_at(b: &[u8], i: usize) -> Option<i32> {
    u32_at(b, i).map(|v| v as i32)
}

/// The fields of the header that decoding needs.
struct Header {
    size: usize,
    width: u32,
    height: u32,
    top_down: bool,
    bits: u32,
    compression: u32,
    colors_used: u32,
    /// Colour table entries are 3 bytes in the old `BITMAPCOREHEADER`.
    core: bool,
}

fn header(bmi: &[u8]) -> Option<Header> {
    let size = u32_at(bmi, 0)? as usize;
    if size == 12 {
        let width = u16_at(bmi, 4)? as u32;
        let height = u16_at(bmi, 6)? as u32;
        let bits = u16_at(bmi, 10)? as u32;
        return Some(Header { size, width, height, top_down: false, bits, compression: 0, colors_used: 0, core: true });
    }
    if size < 40 || size > bmi.len() {
        return None;
    }
    let width = i32_at(bmi, 4)?;
    let height = i32_at(bmi, 8)?;
    if width <= 0 || height == 0 {
        return None;
    }
    Some(Header {
        size,
        width: width as u32,
        height: height.unsigned_abs(),
        top_down: height < 0,
        bits: u16_at(bmi, 14)? as u32,
        compression: u32_at(bmi, 16)?,
        colors_used: u32_at(bmi, 32)?,
        core: false,
    })
}

impl Header {
    /// Bytes of colour masks that sit between a plain 40-byte header and
    /// the colour table.  Larger headers hold their masks inside.
    fn separate_masks(&self) -> usize {
        match (self.size, self.compression) {
            (40, 3) => 12,
            (40, 6) => 16,
            _ => 0,
        }
    }

    /// Entries in the colour table.
    fn table_len(&self) -> usize {
        match self.bits {
            1 | 4 | 8 if self.colors_used == 0 || self.core => 1usize << self.bits,
            _ => (self.colors_used as usize).min(256),
        }
    }
}

/// Where a packed DIB's pixels start: after the header, its colour masks
/// and its colour table (WMF stores header and pixels in one block).
pub fn packed_pixel_offset(dib: &[u8]) -> Option<usize> {
    let h = header(dib)?;
    Some(h.size + h.separate_masks() + h.table_len() * if h.core { 3 } else { 4 })
}

/// Decodes a packed DIB: header, colour table and pixels in one block.
pub fn decode_packed(dib: &[u8], palette: &[Rgb]) -> Option<Bitmap> {
    let off = packed_pixel_offset(dib)?;
    decode(dib, dib.get(off..)?, 0, palette)
}

/// Decodes a DIB whose header (with its colour table) and pixels are
/// separate, as in EMF.  `usage` is `DIB_RGB_COLORS` (0) or
/// `DIB_PAL_COLORS` (1), in which case the colour table holds 16-bit
/// indices into the logical `palette`.
pub fn decode(bmi: &[u8], bits: &[u8], usage: u32, palette: &[Rgb]) -> Option<Bitmap> {
    let h = header(bmi)?;
    if h.width == 0 || h.height == 0 || h.width as u64 * h.height as u64 > MAX_PIXELS {
        return None;
    }
    if h.compression == 4 || h.compression == 5 {
        return embedded_image(bits);
    }
    // 1. Colour masks for 16 and 32-bit pixels, from the header or right
    //    after it.  Order: red, green, blue, alpha.
    let mut masks = [0u32; 4];
    let has_masks = matches!(h.compression, 3 | 6) && matches!(h.bits, 16 | 32);
    if has_masks {
        let at = if h.size >= 52 { 40 } else { h.size };
        let n = if h.size >= 56 || h.compression == 6 { 4 } else { 3 };
        for (i, m) in masks.iter_mut().enumerate().take(n) {
            *m = u32_at(bmi, at + i * 4).unwrap_or(0);
        }
    }
    // 2. The colour table, after the header and any separate masks.
    let entry = if h.core {
        3
    } else if usage == 1 {
        2
    } else {
        4
    };
    let table_at = h.size + h.separate_masks();
    let mut table: Vec<Rgb> = Vec::with_capacity(h.table_len());
    for i in 0..h.table_len() {
        let at = table_at + i * entry;
        if usage == 1 && !h.core {
            let idx = u16_at(bmi, at).unwrap_or(0) as usize;
            table.push(palette.get(idx).copied().unwrap_or([0, 0, 0]));
        } else {
            // A short table is common; missing entries are black.
            table.push(bmi.get(at..at + 3).map_or([0, 0, 0], |c| [c[2], c[1], c[0]]));
        }
    }
    let (w, ht) = (h.width as usize, h.height as usize);
    let mut rgba = vec![0u8; w * ht * 4];
    let mut has_alpha = false;
    let mono = false;
    // 3. Pixel rows, bottom-up unless the height was negative.
    match h.compression {
        1 | 2 => {
            if !matches!((h.compression, h.bits), (1, 8) | (2, 4)) {
                return None;
            }
            let idx = rle(bits, w, ht, h.compression == 2);
            for y in 0..ht {
                let dy = if h.top_down { y } else { ht - 1 - y };
                for x in 0..w {
                    let c = table.get(idx[y * w + x] as usize).copied().unwrap_or([0, 0, 0]);
                    let o = (dy * w + x) * 4;
                    rgba[o..o + 4].copy_from_slice(&[c[0], c[1], c[2], 255]);
                }
            }
        }
        0 | 3 | 6 => {
            let row_bytes = (w * h.bits as usize).div_ceil(8);
            let stride = (w * h.bits as usize).div_ceil(32) * 4;
            if stride == 0 || bits.len() < stride * (ht - 1) + row_bytes {
                return None;
            }
            for y in 0..ht {
                let dy = if h.top_down { y } else { ht - 1 - y };
                let row = &bits[y * stride..];
                let out = &mut rgba[dy * w * 4..(dy + 1) * w * 4];
                match h.bits {
                    1 | 4 | 8 => {
                        let per = (8 / h.bits) as usize;
                        for x in 0..w {
                            let byte = row.get(x / per).copied().unwrap_or(0);
                            let shift = 8 - h.bits as usize * (x % per + 1);
                            let i = ((byte >> shift) as usize) & ((1 << h.bits) - 1);
                            let c = table.get(i).copied().unwrap_or([0, 0, 0]);
                            out[x * 4..x * 4 + 4].copy_from_slice(&[c[0], c[1], c[2], 255]);
                        }
                    }
                    16 => {
                        let m = if has_masks { masks } else { [0x7C00, 0x03E0, 0x001F, 0] };
                        for x in 0..w {
                            let v = u16_at(row, x * 2).unwrap_or(0) as u32;
                            out[x * 4..x * 4 + 4].copy_from_slice(&[
                                extract(v, m[0]),
                                extract(v, m[1]),
                                extract(v, m[2]),
                                255,
                            ]);
                        }
                    }
                    24 => {
                        for x in 0..w {
                            let p = row.get(x * 3..x * 3 + 3).unwrap_or(&[0, 0, 0]);
                            out[x * 4..x * 4 + 4].copy_from_slice(&[p[2], p[1], p[0], 255]);
                        }
                    }
                    32 => {
                        // Plain 32-bit bitmaps keep alpha in the fourth byte.
                        // BI_BITFIELDS headers name no alpha mask, but the bits
                        // the colour masks leave over are what AlphaBlend reads.
                        let (m, alpha_mask) = if has_masks {
                            let colours = masks[0] | masks[1] | masks[2];
                            let alpha = if masks[3] != 0 { masks[3] } else { !colours };
                            ([masks[0], masks[1], masks[2]], alpha)
                        } else {
                            ([0xFF0000, 0xFF00, 0xFF], 0xFF00_0000)
                        };
                        for x in 0..w {
                            let v = u32_at(row, x * 4).unwrap_or(0);
                            let a = if alpha_mask == 0 { 0 } else { extract(v, alpha_mask) };
                            has_alpha |= a != 0;
                            out[x * 4..x * 4 + 4].copy_from_slice(&[
                                extract(v, m[0]),
                                extract(v, m[1]),
                                extract(v, m[2]),
                                a,
                            ]);
                        }
                    }
                    _ => return None,
                }
            }
        }
        _ => return None,
    }
    if !has_alpha {
        for p in rgba.chunks_exact_mut(4) {
            p[3] = 255;
        }
    }
    Some(Bitmap { width: h.width, height: h.height, rgba, mono, has_alpha })
}

/// Scales the bits under `mask` to eight bits.
fn extract(v: u32, mask: u32) -> u8 {
    if mask == 0 {
        return 0;
    }
    let shift = mask.trailing_zeros();
    let max = mask >> shift;
    let bits = (v & mask) >> shift;
    if max == 0 { 0 } else { ((bits as u64 * 255 + max as u64 / 2) / max as u64) as u8 }
}

/// Run-length decoding (`BI_RLE8` and `BI_RLE4`) to one index per pixel,
/// bottom row first, as the stream stores them.
fn rle(data: &[u8], w: usize, h: usize, four: bool) -> Vec<u8> {
    let mut out = vec![0u8; w * h];
    let (mut x, mut y, mut i) = (0usize, 0usize, 0usize);
    let put = |out: &mut Vec<u8>, x: usize, y: usize, v: u8| {
        if x < w && y < h {
            out[y * w + x] = v;
        }
    };
    while i + 1 < data.len() && y < h {
        let (n, v) = (data[i] as usize, data[i + 1]);
        i += 2;
        if n > 0 {
            for k in 0..n {
                let p = if four { if k % 2 == 0 { v >> 4 } else { v & 15 } } else { v };
                put(&mut out, x, y, p);
                x += 1;
            }
            continue;
        }
        match v {
            0 => {
                x = 0;
                y += 1;
            }
            1 => break,
            2 => {
                x += data.get(i).copied().unwrap_or(0) as usize;
                y += data.get(i + 1).copied().unwrap_or(0) as usize;
                i += 2;
            }
            count => {
                let count = count as usize;
                let bytes = if four { count.div_ceil(2) } else { count };
                for k in 0..count {
                    let b = data.get(i + if four { k / 2 } else { k }).copied().unwrap_or(0);
                    let p = if four { if k % 2 == 0 { b >> 4 } else { b & 15 } } else { b };
                    put(&mut out, x, y, p);
                    x += 1;
                }
                i += bytes + (bytes & 1);
            }
        }
    }
    out
}

/// `BI_JPEG` and `BI_PNG` bitmaps carry an ordinary image file.
fn embedded_image(bytes: &[u8]) -> Option<Bitmap> {
    let tex = slerp_scene::textures::decode(bytes, &slerp_scene::textures::DecodeOptions::full()).ok()?;
    let level = tex.levels.first()?;
    let slerp_scene::textures::Pixels::Rgba8(px) = level.surfaces.first()? else { return None };
    if level.width as u64 * level.height as u64 > MAX_PIXELS {
        return None;
    }
    Some(Bitmap { width: level.width, height: level.height, rgba: px.clone(), mono: false, has_alpha: tex.has_alpha })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header40(w: i32, h: i32, bits: u16, compression: u32, used: u32) -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(&40u32.to_le_bytes());
        v.extend_from_slice(&w.to_le_bytes());
        v.extend_from_slice(&h.to_le_bytes());
        v.extend_from_slice(&1u16.to_le_bytes());
        v.extend_from_slice(&bits.to_le_bytes());
        v.extend_from_slice(&compression.to_le_bytes());
        v.extend_from_slice(&[0; 12]);
        v.extend_from_slice(&used.to_le_bytes());
        v.extend_from_slice(&[0; 4]);
        v
    }

    #[test]
    fn decodes_24_bit_bottom_up() {
        let mut dib = header40(2, 2, 24, 0, 0);
        // Bottom row first, rows padded to four bytes: red, green / blue, white.
        dib.extend_from_slice(&[0, 0, 255, 0, 255, 0, 0, 0]);
        dib.extend_from_slice(&[255, 0, 0, 255, 255, 255, 0, 0]);
        let b = decode_packed(&dib, &[]).unwrap();
        // The first stored row is the bottom one.
        assert_eq!(b.pixel(0, 1), [255, 0, 0, 255]);
        assert_eq!(b.pixel(1, 1), [0, 255, 0, 255]);
        assert_eq!(b.pixel(0, 0), [0, 0, 255, 255]);
        assert_eq!(b.pixel(1, 0), [255, 255, 255, 255]);
    }

    #[test]
    fn decodes_8_bit_with_palette() {
        let mut dib = header40(4, 1, 8, 0, 2);
        dib.extend_from_slice(&[0, 0, 255, 0, 0, 255, 0, 0]);
        dib.extend_from_slice(&[0, 1, 1, 0]);
        let b = decode_packed(&dib, &[]).unwrap();
        assert_eq!(b.pixel(0, 0), [255, 0, 0, 255]);
        assert_eq!(b.pixel(1, 0), [0, 255, 0, 255]);
    }

    #[test]
    fn decodes_one_bit_with_its_palette() {
        let mut dib = header40(8, 1, 1, 0, 2);
        dib.extend_from_slice(&[0, 0, 0, 0, 255, 255, 255, 0]);
        dib.extend_from_slice(&[0b1010_0000, 0, 0, 0]);
        let b = decode_packed(&dib, &[]).unwrap();
        assert_eq!(b.pixel(0, 0), [255, 255, 255, 255]);
        assert_eq!(b.pixel(1, 0), [0, 0, 0, 255]);
    }

    #[test]
    fn decodes_rle8() {
        let mut dib = header40(4, 1, 8, 1, 2);
        dib.extend_from_slice(&[0, 0, 0, 0, 255, 255, 255, 0]);
        // Two of index 1, two of index 0, end of bitmap.
        dib.extend_from_slice(&[2, 1, 2, 0, 0, 1]);
        let b = decode_packed(&dib, &[]).unwrap();
        assert_eq!(b.pixel(0, 0), [255, 255, 255, 255]);
        assert_eq!(b.pixel(3, 0), [0, 0, 0, 255]);
    }

    #[test]
    fn refuses_absurd_sizes_and_short_data() {
        let dib = header40(100_000, 100_000, 24, 0, 0);
        assert!(decode_packed(&dib, &[]).is_none());
        let mut small = header40(100, 100, 24, 0, 0);
        small.extend_from_slice(&[0; 10]);
        assert!(decode_packed(&small, &[]).is_none());
        assert!(decode_packed(&[1, 2, 3], &[]).is_none());
    }

    #[test]
    fn bitfields_565() {
        let mut dib = header40(1, 1, 16, 3, 0);
        dib.extend_from_slice(&0xF800u32.to_le_bytes());
        dib.extend_from_slice(&0x07E0u32.to_le_bytes());
        dib.extend_from_slice(&0x001Fu32.to_le_bytes());
        dib.extend_from_slice(&0xF800u16.to_le_bytes());
        dib.extend_from_slice(&[0, 0]);
        let b = decode_packed(&dib, &[]).unwrap();
        assert_eq!(b.pixel(0, 0), [255, 0, 0, 255]);
    }
}
