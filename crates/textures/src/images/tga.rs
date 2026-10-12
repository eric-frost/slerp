//! Truevision TGA: colour-mapped, true-colour and greyscale images, plain
//! or run-length encoded, 8 to 32 bits per pixel.

use super::Picture;
use crate::TextureError;
use crate::util::{Reader, check_size, invalid, unsupported};

/// Converts one stored pixel of `bits` bits to RGBA.  TGA stores colour as
/// BGR(A), and 15/16-bit pixels as little-endian A1R5G5B5.
fn pixel(p: &[u8], bits: u8, grey: bool, alpha: bool) -> [u8; 4] {
    match bits {
        8 if grey => [p[0], p[0], p[0], 255],
        16 if grey => [p[0], p[0], p[0], if alpha { p[1] } else { 255 }],
        15 | 16 => {
            let v = u16::from_le_bytes([p[0], p[1]]);
            let c = |s: u16| {
                let x = ((v >> s) & 31) as u8;
                (x << 3) | (x >> 2)
            };
            let a = if bits == 16 && alpha && v & 0x8000 == 0 { 0 } else { 255 };
            [c(10), c(5), c(0), a]
        }
        24 => [p[2], p[1], p[0], 255],
        _ => [p[2], p[1], p[0], if alpha { p[3] } else { 255 }],
    }
}

pub(crate) fn decode(bytes: &[u8]) -> Result<Picture, TextureError> {
    // 1. The 18-byte header.
    let mut r = Reader::new(bytes);
    let id_len = r.u8()? as usize;
    let cmap_type = r.u8()?;
    let image_type = r.u8()?;
    let cmap_first = r.u16()? as usize;
    let cmap_len = r.u16()? as usize;
    let cmap_bits = r.u8()?;
    r.skip(4)?;
    let width = r.u16()? as u32;
    let height = r.u16()? as u32;
    let bits = r.u8()?;
    let desc = r.u8()?;
    let alpha_bits = desc & 15;
    let right_to_left = desc & 0x10 != 0;
    let top_down = desc & 0x20 != 0;
    r.skip(id_len)?;

    let rle = image_type >= 9;
    let (mapped, grey) = match image_type & 7 {
        1 => (true, false),
        2 => (false, false),
        3 => (false, true),
        _ => return Err(unsupported(format!("TGA image type {image_type}"))),
    };
    let valid_bits = if mapped {
        matches!(bits, 8 | 16)
    } else if grey {
        matches!(bits, 8 | 16)
    } else {
        matches!(bits, 15 | 16 | 24 | 32)
    };
    if !valid_bits {
        return Err(unsupported(format!("TGA with {bits} bits per pixel")));
    }
    let n = check_size(width, height, 1, 4)?;

    // 2. The colour map, already converted to RGBA.
    let mut palette = Vec::new();
    if cmap_type == 1 {
        if !matches!(cmap_bits, 15 | 16 | 24 | 32) {
            return Err(unsupported(format!("TGA colour map with {cmap_bits}-bit entries")));
        }
        let size = (cmap_bits as usize).div_ceil(8);
        let data = r.bytes(cmap_len * size)?;
        palette =
            data.chunks_exact(size).map(|p| pixel(p, cmap_bits, false, cmap_bits == 32 || alpha_bits > 0)).collect();
    } else if mapped {
        return Err(invalid("TGA: colour-mapped image without a colour map"));
    }

    // 3. The pixels, in file order.
    let size = (bits as usize).div_ceil(8);
    let alpha = alpha_bits > 0;
    let convert = |p: &[u8]| -> [u8; 4] {
        if mapped {
            let i = if bits == 8 { p[0] as usize } else { u16::from_le_bytes([p[0], p[1]]) as usize };
            *i.checked_sub(cmap_first).and_then(|i| palette.get(i)).unwrap_or(&[0, 0, 0, 255])
        } else {
            pixel(p, bits, grey, alpha)
        }
    };
    let mut px = Vec::with_capacity(n * 4);
    if rle {
        while px.len() < n * 4 {
            let head = r.u8()?;
            let count = (head & 0x7f) as usize + 1;
            if head & 0x80 != 0 {
                let p = convert(r.bytes(size)?);
                for _ in 0..count.min(n - px.len() / 4) {
                    px.extend_from_slice(&p);
                }
            } else {
                for _ in 0..count.min(n - px.len() / 4) {
                    px.extend_from_slice(&convert(r.bytes(size)?));
                }
            }
        }
    } else {
        let data = r.bytes(n * size)?;
        for p in data.chunks_exact(size) {
            px.extend_from_slice(&convert(p));
        }
    }

    // 4. TGA rows run bottom to top unless the descriptor says otherwise.
    let row = width as usize * 4;
    if !top_down {
        let rows: Vec<&[u8]> = px.chunks_exact(row).rev().collect();
        px = rows.concat();
    }
    if right_to_left {
        for line in px.chunks_exact_mut(row) {
            let mut texels: Vec<[u8; 4]> = line.chunks_exact(4).map(|p| [p[0], p[1], p[2], p[3]]).collect();
            texels.reverse();
            line.copy_from_slice(texels.concat().as_slice());
        }
    }
    let kind = if mapped {
        "colour-mapped"
    } else if grey {
        "greyscale"
    } else {
        "true-colour"
    };
    let format = format!("{kind} {bits}-bit{}", if rle { " RLE" } else { "" });
    Ok(Picture::new(width, height, px, format))
}

/// Whether a header looks like a TGA file.  TGA has no magic number, so
/// this checks every field that has few valid values.
pub(crate) fn plausible(h: &[u8]) -> bool {
    if h.len() < 18 {
        return false;
    }
    let (cmap_type, image_type, cmap_bits, bits) = (h[1], h[2], h[7], h[16]);
    let width = u16::from_le_bytes([h[12], h[13]]);
    let height = u16::from_le_bytes([h[14], h[15]]);
    let cmap_len = u16::from_le_bytes([h[5], h[6]]);
    let mapped = matches!(image_type, 1 | 9);
    let ok_type = matches!(image_type, 1 | 2 | 3 | 9 | 10 | 11);
    let ok_cmap = match cmap_type {
        0 => cmap_len == 0 && !mapped,
        1 => mapped && cmap_len > 0 && matches!(cmap_bits, 15 | 16 | 24 | 32),
        _ => false,
    };
    let ok_bits =
        if mapped || image_type & 7 == 3 { matches!(bits, 8 | 16) } else { matches!(bits, 15 | 16 | 24 | 32) };
    ok_type && ok_cmap && ok_bits && width > 0 && height > 0 && h[17] & 0xc0 == 0
}
