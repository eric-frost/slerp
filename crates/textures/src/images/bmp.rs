//! Windows and OS/2 bitmaps: 1, 2, 4, 8, 16, 24 and 32 bits per pixel,
//! bit-field masks, RLE4 and RLE8, and the rare BMPs that wrap a PNG or JPEG.

use super::Picture;
use crate::util::{Reader, check_size, invalid, unsupported};
use crate::{DecodeOptions, TextureError};

/// One channel described by a bit mask, such as 0x7c00 for 5-bit red.
#[derive(Clone, Copy)]
struct Mask {
    shift: u32,
    bits: u32,
}

impl Mask {
    fn new(mask: u32) -> Self {
        if mask == 0 {
            return Mask { shift: 0, bits: 0 };
        }
        let shift = mask.trailing_zeros();
        Mask { shift, bits: (mask >> shift).trailing_ones().min(16) }
    }

    fn get(&self, v: u32) -> Option<u8> {
        if self.bits == 0 {
            return None;
        }
        let x = (v >> self.shift) & ((1 << self.bits) - 1);
        Some(((x * 255 + ((1 << self.bits) - 1) / 2) / ((1 << self.bits) - 1)) as u8)
    }
}

pub(crate) fn decode(bytes: &[u8], opts: &DecodeOptions) -> Result<Picture, TextureError> {
    // 1. File header and the DIB header, whose size names its version.
    let mut r = Reader::new(bytes);
    if r.bytes(2)? != b"BM" {
        return Err(invalid("not a BMP file"));
    }
    r.skip(8)?;
    let data_offset = r.u32()? as usize;
    let header_size = r.u32()? as usize;
    let (width, height, bits, compression, colors_used);
    if header_size == 12 {
        width = r.u16()? as i32;
        height = r.u16()? as i16 as i32;
        r.skip(2)?;
        bits = r.u16()?;
        compression = 0;
        colors_used = 0;
    } else if (16..=256).contains(&header_size) {
        let mut h = Reader::new(r.bytes(header_size - 4)?);
        width = h.i32()?;
        height = h.i32()?;
        h.skip(2)?;
        bits = h.u16()?;
        compression = h.u32().unwrap_or(0);
        h.skip(12).ok();
        colors_used = h.u32().unwrap_or(0) as usize;
    } else {
        return Err(unsupported(format!("BMP header of {header_size} bytes")));
    }
    let top_down = height < 0;
    let (w, h) = (width.unsigned_abs(), height.unsigned_abs());
    if width <= 0 {
        return Err(invalid("BMP: bad width"));
    }

    // 2. Embedded PNG or JPEG data.
    if compression == 4 || compression == 5 {
        let data = bytes.get(data_offset..).ok_or_else(|| invalid("BMP: bad data offset"))?;
        let format = if compression == 4 { crate::Format::Jpeg } else { crate::Format::Png };
        let tex = super::decode(format, data, opts)?;
        let crate::Pixels::Rgba8(rgba) = tex.levels[0].surfaces[0].clone() else { unreachable!() };
        return Ok(Picture::new(tex.width(), tex.height(), rgba, format!("embedded {format:?}")));
    }
    let n = check_size(w, h, 1, 4)?;

    // 3. Channel masks: from the header (V2 and later), after a 40-byte
    //    header for BI_BITFIELDS, or the defaults.
    let masks_at = 14 + 40;
    let read_mask =
        |i: usize| bytes.get(masks_at + i * 4..masks_at + i * 4 + 4).map(|b| u32::from_le_bytes(b.try_into().unwrap()));
    let has_masks = compression == 3 || compression == 6;
    let mut extra = 0;
    let masks = if has_masks {
        let alpha_in_header = header_size >= 56 || compression == 6;
        if header_size == 40 {
            extra = if compression == 6 { 16 } else { 12 };
        }
        let m = [read_mask(0), read_mask(1), read_mask(2), if alpha_in_header { read_mask(3) } else { Some(0) }];
        [
            Mask::new(m[0].unwrap_or(0)),
            Mask::new(m[1].unwrap_or(0)),
            Mask::new(m[2].unwrap_or(0)),
            Mask::new(m[3].unwrap_or(0)),
        ]
    } else if bits == 16 {
        [Mask::new(0x7c00), Mask::new(0x3e0), Mask::new(0x1f), Mask::new(0)]
    } else {
        [Mask::new(0xff0000), Mask::new(0xff00), Mask::new(0xff), Mask::new(0)]
    };

    // 4. The palette, for 8 bits or fewer.
    let mut palette = Vec::new();
    if bits <= 8 {
        let entry = if header_size == 12 { 3 } else { 4 };
        let count = if colors_used == 0 { 1usize << bits } else { colors_used.min(256) };
        let start = 14 + header_size + extra;
        for i in 0..count {
            let at = start + i * entry;
            let Some(p) = bytes.get(at..at + 3) else { break };
            palette.push([p[2], p[1], p[0], 255]);
        }
        if palette.is_empty() {
            return Err(invalid("BMP: missing palette"));
        }
    }
    let lookup = |i: usize| *palette.get(i).unwrap_or(&[0, 0, 0, 255]);

    // 5. Pixels, bottom row first unless the height was negative.
    let data = bytes.get(data_offset..).ok_or_else(|| invalid("BMP: bad data offset"))?;
    let mut px = vec![0u8; n * 4];
    let row_out = w as usize * 4;
    let dest_row = |y: usize| if top_down { y } else { h as usize - 1 - y };
    match (compression, bits) {
        (1, 8) | (2, 4) => {
            decode_rle(data, w as usize, h as usize, bits == 4, |x, y, i| {
                let at = dest_row(y) * row_out + x * 4;
                px[at..at + 4].copy_from_slice(&lookup(i));
            });
        }
        (0 | 3 | 6, 1 | 2 | 4 | 8 | 16 | 24 | 32) => {
            let stride = (w as usize * bits as usize).div_ceil(32) * 4;
            let mut any_alpha = false;
            for y in 0..h as usize {
                let Some(src) = data.get(y * stride..y * stride + stride) else { break };
                let out = &mut px[dest_row(y) * row_out..][..row_out];
                for x in 0..w as usize {
                    let p = match bits {
                        1 | 2 | 4 | 8 => {
                            let bit = x * bits as usize;
                            let v = (src[bit / 8] >> (8 - bits as usize - bit % 8)) & ((1u16 << bits) - 1) as u8;
                            lookup(v as usize)
                        }
                        16 | 32 if has_masks || bits == 16 => {
                            let v = if bits == 16 {
                                u16::from_le_bytes([src[x * 2], src[x * 2 + 1]]) as u32
                            } else {
                                u32::from_le_bytes(src[x * 4..x * 4 + 4].try_into().unwrap())
                            };
                            let a = masks[3].get(v).unwrap_or(255);
                            [
                                masks[0].get(v).unwrap_or(0),
                                masks[1].get(v).unwrap_or(0),
                                masks[2].get(v).unwrap_or(0),
                                a,
                            ]
                        }
                        24 => [src[x * 3 + 2], src[x * 3 + 1], src[x * 3], 255],
                        _ => {
                            any_alpha |= src[x * 4 + 3] != 0;
                            [src[x * 4 + 2], src[x * 4 + 1], src[x * 4], src[x * 4 + 3]]
                        }
                    };
                    out[x * 4..x * 4 + 4].copy_from_slice(&p);
                }
            }
            // A plain 32-bit BMP's fourth byte is usually unused and zero;
            // treat it as alpha only when something set it.
            if bits == 32 && !has_masks && !any_alpha {
                px.chunks_exact_mut(4).for_each(|p| p[3] = 255);
            }
        }
        _ => return Err(unsupported(format!("BMP with compression {compression} and {bits} bits"))),
    }
    let format = format!("{bits}-bit{}", if compression == 1 || compression == 2 { " RLE" } else { "" });
    Ok(Picture::new(w, h, px, format))
}

/// Decodes RLE8 or RLE4 data, calling `put(x, y, palette_index)` for each
/// pixel; `y` counts rows in file order.  Stops quietly at the end of data.
fn decode_rle(data: &[u8], w: usize, h: usize, four: bool, mut put: impl FnMut(usize, usize, usize)) {
    let (mut x, mut y, mut i) = (0usize, 0usize, 0usize);
    let mut emit = |x: &mut usize, y: usize, v: usize| {
        if *x < w && y < h {
            put(*x, y, v);
        }
        *x += 1;
    };
    while i + 1 < data.len() && y < h {
        let (a, b) = (data[i] as usize, data[i + 1]);
        i += 2;
        if a > 0 {
            for k in 0..a {
                let v = if four { if k % 2 == 0 { b >> 4 } else { b & 15 } } else { b };
                emit(&mut x, y, v as usize);
            }
            continue;
        }
        match b {
            0 => {
                x = 0;
                y += 1;
            }
            1 => break,
            2 => {
                let (Some(&dx), Some(&dy)) = (data.get(i), data.get(i + 1)) else { break };
                x += dx as usize;
                y += dy as usize;
                i += 2;
            }
            n => {
                let n = n as usize;
                let len = if four { n.div_ceil(2) } else { n };
                let Some(run) = data.get(i..i + len) else { break };
                for k in 0..n {
                    let v = if four { if k % 2 == 0 { run[k / 2] >> 4 } else { run[k / 2] & 15 } } else { run[k] };
                    emit(&mut x, y, v as usize);
                }
                i += len.div_ceil(2) * 2;
            }
        }
    }
}

/// Whether a header looks like a BMP: the magic plus a known DIB header
/// size, since two letters alone are too weak.
pub(crate) fn plausible(h: &[u8]) -> bool {
    h.len() >= 18
        && &h[..2] == b"BM"
        && matches!(u32::from_le_bytes([h[14], h[15], h[16], h[17]]), 12 | 16 | 40 | 52 | 56 | 64 | 108 | 124)
}
