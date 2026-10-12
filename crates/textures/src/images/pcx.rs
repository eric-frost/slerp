//! ZSoft PCX: 8-bit paletted images (Quake 2's skins), 24- and 32-bit
//! planar colour, and the old 1-, 2- and 4-bit EGA and CGA kinds.

use super::Picture;
use crate::TextureError;
use crate::util::{check_size, invalid, u16_le, unsupported};

pub(crate) fn decode(bytes: &[u8]) -> Result<Picture, TextureError> {
    // 1. The 128-byte header.
    let h = bytes.get(..128).ok_or_else(|| invalid("PCX: truncated header"))?;
    if h[0] != 0x0a {
        return Err(invalid("not a PCX file"));
    }
    let rle = h[2] == 1;
    let bits = h[3] as usize;
    let (x0, y0, x1, y1) = (u16_le(h, 4), u16_le(h, 6), u16_le(h, 8), u16_le(h, 10));
    let planes = h[65] as usize;
    let line = u16_le(h, 66) as usize;
    if x1 < x0 || y1 < y0 {
        return Err(invalid("PCX: bad window"));
    }
    let (w, ht) = ((x1 - x0) as u32 + 1, (y1 - y0) as u32 + 1);
    let n = check_size(w, ht, 1, 4)?;
    let supported = matches!((bits, planes), (8, 1) | (8, 3) | (8, 4) | (1, 1) | (1, 3) | (1, 4) | (2, 1) | (4, 1));
    if !supported {
        return Err(unsupported(format!("PCX with {planes} planes of {bits} bits")));
    }
    if line * 8 < w as usize * bits {
        return Err(invalid("PCX: lines shorter than the image"));
    }

    // 2. Decode the run-length data of every scanline into one buffer.
    let scan = line * planes;
    let total = scan.checked_mul(ht as usize).ok_or_else(|| invalid("PCX: too large"))?;
    let mut raw = Vec::with_capacity(total);
    let mut i = 128;
    while raw.len() < total && i < bytes.len() {
        let b = bytes[i];
        i += 1;
        if rle && b >= 0xc0 {
            let v = *bytes.get(i).unwrap_or(&0);
            i += 1;
            let count = ((b & 0x3f) as usize).min(total - raw.len());
            raw.extend(std::iter::repeat_n(v, count));
        } else {
            raw.push(b);
        }
    }
    raw.resize(total, 0);

    // 3. Palettes: 256 colours after a 0x0c marker at the end of the file,
    //    or 16 in the header.
    let vga = (bits == 8 && planes == 1)
        .then(|| bytes.len().checked_sub(769).filter(|&at| bytes[at] == 0x0c).map(|at| &bytes[at + 1..]))
        .flatten();
    let ega = &h[16..64];
    let colour = |i: usize| -> [u8; 4] {
        let p = match vga {
            Some(p) => &p[i * 3..i * 3 + 3],
            None if bits == 8 => return [i as u8, i as u8, i as u8, 255],
            None => &ega[(i % 16) * 3..(i % 16) * 3 + 3],
        };
        [p[0], p[1], p[2], 255]
    };

    // 4. Assemble RGBA from the planes.
    let mut px = Vec::with_capacity(n * 4);
    for y in 0..ht as usize {
        let row = &raw[y * scan..(y + 1) * scan];
        for x in 0..w as usize {
            let p = match (bits, planes) {
                (8, 1) => colour(row[x] as usize),
                (8, 3) => [row[x], row[line + x], row[2 * line + x], 255],
                (8, 4) => [row[x], row[line + x], row[2 * line + x], row[3 * line + x]],
                (1, 1) => {
                    let v = if row[x / 8] >> (7 - x % 8) & 1 != 0 { 255 } else { 0 };
                    [v, v, v, 255]
                }
                (1, _) => {
                    let mut i = 0;
                    for p in 0..planes {
                        i |= ((row[p * line + x / 8] >> (7 - x % 8) & 1) as usize) << p;
                    }
                    colour(i)
                }
                (2, 1) => colour((row[x / 4] >> (6 - 2 * (x % 4)) & 3) as usize),
                _ => colour((row[x / 2] >> (4 - 4 * (x % 2)) & 15) as usize),
            };
            px.extend_from_slice(&p);
        }
    }
    Ok(Picture::new(w, ht, px, format!("{planes}x{bits}-bit")))
}

/// Whether a header looks like a PCX file.
pub(crate) fn plausible(h: &[u8]) -> bool {
    h.len() >= 4 && h[0] == 0x0a && matches!(h[1], 0 | 2 | 3 | 4 | 5) && h[2] <= 1 && matches!(h[3], 1 | 2 | 4 | 8)
}
