//! SAI 2's canvas file: a 64-byte header, a table of parts, and the parts.
//! `thum` is a JSSF thumbnail and `intg` the merged canvas.

use crate::source::Source;
use slerp_scene::textures::{ColorSpace, Texture};
use slerp_scene::{Limits, LoadError};

const MAGIC: &[u8] = b"SAI-CANVAS-TYPE0";
/// Parts listed in the table at most.
const MAX_PARTS: u64 = 1 << 16;
/// The largest part read, and canvas decoded.
const MAX_PART: u64 = 512 << 20;
const MAX_PIXELS: u64 = 1 << 28;
const TILE: u32 = 256;

fn bad(what: &str) -> LoadError {
    LoadError::invalid(format!("SAI 2 canvas: {what}"))
}

pub fn looks_like_sai2(head: &[u8]) -> bool {
    head.starts_with(MAGIC)
}

fn u16_at(b: &[u8], i: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(i..i.checked_add(2)?)?.try_into().ok()?))
}

fn u32_at(b: &[u8], i: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(i..i.checked_add(4)?)?.try_into().ok()?))
}

pub fn preview(src: &mut Source, limits: &Limits) -> Result<Texture, LoadError> {
    let h = src.exact(0, 64)?;
    if !looks_like_sai2(&h) {
        return Err(LoadError::NotMine);
    }
    let background = h[17];
    let (width, height) = (u32_at(&h, 20).unwrap_or(0), u32_at(&h, 24).unwrap_or(0));
    let count = u32_at(&h, 32).unwrap_or(0) as u64;
    if count > MAX_PARTS {
        return Err(bad("too many parts"));
    }
    // 1. The table: type, layer id, offset; each part runs to the next.
    let table = src.exact(64, count * 16)?;
    let offsets: Vec<(&[u8], u64)> =
        table.chunks_exact(16).map(|e| (&e[..4], u64::from_le_bytes(e[8..16].try_into().unwrap_or([0; 8])))).collect();
    let part = |kind: &[u8]| -> Option<(u64, u64)> {
        let i = offsets.iter().position(|(t, _)| *t == kind)?;
        let start = offsets[i].1;
        let end = offsets.get(i + 1).map_or(src.len(), |n| n.1);
        (end > start).then_some((start, end - start))
    };
    let (thum, intg) = (part(b"thum"), part(b"intg"));
    // 2. The viewer wants the canvas, thumbnails the thumbnail; each falls
    //    back on the other.
    let order = if limits.max_texture_size.is_some() { [thum, intg] } else { [intg, thum] };
    let mut error = crate::no_preview("SAI 2 canvas");
    for (i, found) in order.into_iter().enumerate() {
        let Some((at, len)) = found else { continue };
        if len > MAX_PART {
            error = LoadError::TooLarge("SAI 2 part".into());
            continue;
        }
        let bytes = src.exact(at, len)?;
        let canvas = (Some((at, len)) == intg) && i < 2;
        let result = if canvas {
            dpcm(&bytes, width, height, background).map(|rgba| texture(width, height, rgba, limits, true))
        } else {
            jssf(&bytes).and_then(|jpeg| crate::decode_image(&jpeg, limits)).map(|mut t| {
                t.source.header.push(("Shown".into(), "The thumbnail SAI saved in the file".into()));
                t
            })
        };
        match result {
            Ok(t) => return Ok(t),
            Err(e) => error = e,
        }
    }
    Err(error)
}

fn texture(width: u32, height: u32, rgba: Vec<u8>, limits: &Limits, merged: bool) -> Texture {
    let mut t = Texture::from_rgba8(width, height, rgba, ColorSpace::Srgb);
    if let Some(n) = limits.max_texture_size
        && width.max(height) > n
    {
        let small = slerp_scene::textures::thumbnail(&t, n);
        t = Texture::from_rgba8(small.width, small.height, small.pixels, ColorSpace::Srgb);
    }
    (t.source.width, t.source.height) = (width, height);
    if merged {
        t.source.header.push(("Shown".into(), "The merged canvas SAI saved in the file".into()));
    }
    t
}

/*┌─────────────────╮
| JSSF thumbnail  |
└─────────────────╯*/
//region JSSF thumbnail

/// The standard JPEG Huffman tables JSSF leaves out: DC and AC for
/// luminance, then for chrominance (class and id byte, 16 counts, symbols).
const DC_LUMA: [u8; 29] = [
    0x00, 0x00, 0x01, 0x05, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01,
    0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0A, 0x0B,
];
const AC_LUMA: [u8; 179] = [
    0x10, 0x00, 0x02, 0x01, 0x03, 0x03, 0x02, 0x04, 0x03, 0x05, 0x05, 0x04, 0x04, 0x00, 0x00, 0x01, 0x7D, 0x01, 0x02,
    0x03, 0x00, 0x04, 0x11, 0x05, 0x12, 0x21, 0x31, 0x41, 0x06, 0x13, 0x51, 0x61, 0x07, 0x22, 0x71, 0x14, 0x32, 0x81,
    0x91, 0xA1, 0x08, 0x23, 0x42, 0xB1, 0xC1, 0x15, 0x52, 0xD1, 0xF0, 0x24, 0x33, 0x62, 0x72, 0x82, 0x09, 0x0A, 0x16,
    0x17, 0x18, 0x19, 0x1A, 0x25, 0x26, 0x27, 0x28, 0x29, 0x2A, 0x34, 0x35, 0x36, 0x37, 0x38, 0x39, 0x3A, 0x43, 0x44,
    0x45, 0x46, 0x47, 0x48, 0x49, 0x4A, 0x53, 0x54, 0x55, 0x56, 0x57, 0x58, 0x59, 0x5A, 0x63, 0x64, 0x65, 0x66, 0x67,
    0x68, 0x69, 0x6A, 0x73, 0x74, 0x75, 0x76, 0x77, 0x78, 0x79, 0x7A, 0x83, 0x84, 0x85, 0x86, 0x87, 0x88, 0x89, 0x8A,
    0x92, 0x93, 0x94, 0x95, 0x96, 0x97, 0x98, 0x99, 0x9A, 0xA2, 0xA3, 0xA4, 0xA5, 0xA6, 0xA7, 0xA8, 0xA9, 0xAA, 0xB2,
    0xB3, 0xB4, 0xB5, 0xB6, 0xB7, 0xB8, 0xB9, 0xBA, 0xC2, 0xC3, 0xC4, 0xC5, 0xC6, 0xC7, 0xC8, 0xC9, 0xCA, 0xD2, 0xD3,
    0xD4, 0xD5, 0xD6, 0xD7, 0xD8, 0xD9, 0xDA, 0xE1, 0xE2, 0xE3, 0xE4, 0xE5, 0xE6, 0xE7, 0xE8, 0xE9, 0xEA, 0xF1, 0xF2,
    0xF3, 0xF4, 0xF5, 0xF6, 0xF7, 0xF8, 0xF9, 0xFA,
];
const DC_CHROMA: [u8; 29] = [
    0x01, 0x00, 0x03, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01,
    0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0A, 0x0B,
];
const AC_CHROMA: [u8; 179] = [
    0x11, 0x00, 0x02, 0x01, 0x02, 0x04, 0x04, 0x03, 0x04, 0x07, 0x05, 0x04, 0x04, 0x00, 0x01, 0x02, 0x77, 0x00, 0x01,
    0x02, 0x03, 0x11, 0x04, 0x05, 0x21, 0x31, 0x06, 0x12, 0x41, 0x51, 0x07, 0x61, 0x71, 0x13, 0x22, 0x32, 0x81, 0x08,
    0x14, 0x42, 0x91, 0xA1, 0xB1, 0xC1, 0x09, 0x23, 0x33, 0x52, 0xF0, 0x15, 0x62, 0x72, 0xD1, 0x0A, 0x16, 0x24, 0x34,
    0xE1, 0x25, 0xF1, 0x17, 0x18, 0x19, 0x1A, 0x26, 0x27, 0x28, 0x29, 0x2A, 0x35, 0x36, 0x37, 0x38, 0x39, 0x3A, 0x43,
    0x44, 0x45, 0x46, 0x47, 0x48, 0x49, 0x4A, 0x53, 0x54, 0x55, 0x56, 0x57, 0x58, 0x59, 0x5A, 0x63, 0x64, 0x65, 0x66,
    0x67, 0x68, 0x69, 0x6A, 0x73, 0x74, 0x75, 0x76, 0x77, 0x78, 0x79, 0x7A, 0x82, 0x83, 0x84, 0x85, 0x86, 0x87, 0x88,
    0x89, 0x8A, 0x92, 0x93, 0x94, 0x95, 0x96, 0x97, 0x98, 0x99, 0x9A, 0xA2, 0xA3, 0xA4, 0xA5, 0xA6, 0xA7, 0xA8, 0xA9,
    0xAA, 0xB2, 0xB3, 0xB4, 0xB5, 0xB6, 0xB7, 0xB8, 0xB9, 0xBA, 0xC2, 0xC3, 0xC4, 0xC5, 0xC6, 0xC7, 0xC8, 0xC9, 0xCA,
    0xD2, 0xD3, 0xD4, 0xD5, 0xD6, 0xD7, 0xD8, 0xD9, 0xDA, 0xE2, 0xE3, 0xE4, 0xE5, 0xE6, 0xE7, 0xE8, 0xE9, 0xEA, 0xF2,
    0xF3, 0xF4, 0xF5, 0xF6, 0xF7, 0xF8, 0xF9, 0xFA,
];

/// Rebuilds a standard JPEG from a `thum` part: width and height, `jssf`,
/// the JPEG's own width, height and channel count, its quantisation tables,
/// then each row of 8x8 blocks as a length and its entropy-coded data.  The
/// rows become restart intervals, as SAI decodes them.
fn jssf(part: &[u8]) -> Result<Vec<u8>, LoadError> {
    if part.get(8..12) != Some(b"jssf") {
        return Err(bad("a thumbnail that isn't JSSF"));
    }
    let (w, h, channels) =
        (u16_at(part, 12).unwrap_or(0), u16_at(part, 14).unwrap_or(0), u16_at(part, 16).unwrap_or(0));
    if w == 0 || h == 0 || !(channels == 1 || channels == 3) {
        return Err(bad("an odd JSSF thumbnail"));
    }
    let tables = if channels > 1 { 128 } else { 64 };
    let quant = part.get(18..18 + tables).ok_or_else(|| bad("a short JSSF thumbnail"))?;
    let mut rows = &part[18 + tables..];
    let mut out = vec![0xFF, 0xD8];
    let segment = |marker: u8, body: &[u8], out: &mut Vec<u8>| {
        out.extend_from_slice(&[0xFF, marker]);
        out.extend_from_slice(&((body.len() + 2) as u16).to_be_bytes());
        out.extend_from_slice(body);
    };
    // Quantisation tables, Huffman tables, the frame and the restart
    // interval (one row of blocks).
    let mut dqt = vec![0u8];
    dqt.extend_from_slice(&quant[..64]);
    if channels > 1 {
        dqt.push(1);
        dqt.extend_from_slice(&quant[64..128]);
    }
    segment(0xDB, &dqt, &mut out);
    let mut dht = [DC_LUMA.as_slice(), &AC_LUMA].concat();
    if channels > 1 {
        dht.extend_from_slice(&DC_CHROMA);
        dht.extend_from_slice(&AC_CHROMA);
    }
    segment(0xC4, &dht, &mut out);
    let mut sof = vec![8];
    sof.extend_from_slice(&h.to_be_bytes());
    sof.extend_from_slice(&w.to_be_bytes());
    sof.push(channels as u8);
    for c in 0..channels as u8 {
        sof.extend_from_slice(&[c + 1, 0x11, (c != 0) as u8]);
    }
    segment(0xC0, &sof, &mut out);
    segment(0xDD, &w.div_ceil(8).to_be_bytes(), &mut out);
    let mut sos = vec![channels as u8];
    for c in 0..channels as u8 {
        sos.extend_from_slice(&[c + 1, if c == 0 { 0x00 } else { 0x11 }]);
    }
    sos.extend_from_slice(&[0x00, 0x3F, 0x00]);
    segment(0xDA, &sos, &mut out);
    // The rows of blocks, with restart markers between them.
    let count = h.div_ceil(8);
    for row in 0..count {
        let n = u16_at(rows, 0).ok_or_else(|| bad("a short JSSF thumbnail"))? as usize;
        let data = rows.get(2..2 + n).ok_or_else(|| bad("a short JSSF thumbnail"))?;
        out.extend_from_slice(data);
        rows = &rows[2 + n..];
        if row + 1 < count {
            out.extend_from_slice(&[0xFF, 0xD0 | (row & 7) as u8]);
        }
    }
    out.extend_from_slice(&[0xFF, 0xD9]);
    Ok(out)
}

//endregion

/*┌─────────────────╮
| Merged canvas   |
└─────────────────╯*/
//region Merged canvas

/// Bits read from a little-endian stream of 32-bit words, the way SAI reads
/// them: up to 64 held at once, the lowest first.
struct Bits<'a> {
    data: &'a [u8],
    at: usize,
    mask: u64,
    held: u32,
}

impl Bits<'_> {
    /// Shifts in words until at least 32 bits are held (or the data ends).
    fn fill(&mut self) {
        while self.held < 32 && self.at < self.data.len() {
            let rest = &self.data[self.at..];
            let (word, bits) = match rest.len() {
                n if n >= 4 => (u32::from_le_bytes([rest[0], rest[1], rest[2], rest[3]]) as u64, 32),
                n if n >= 2 => (u16::from_le_bytes([rest[0], rest[1]]) as u64, 16),
                _ => (rest[0] as u64, 8),
            };
            self.mask |= word << self.held;
            self.held += bits;
            self.at += bits as usize / 8;
        }
    }

    fn take(&mut self, n: u32) -> Option<u64> {
        if n > self.held {
            return None;
        }
        let v = if n == 64 { self.mask } else { self.mask & ((1u64 << n) - 1) };
        self.mask = if n >= 64 { 0 } else { self.mask >> n };
        self.held -= n;
        Some(v)
    }
}

/// One row of a tile: `channels` channels of `count` signed deltas each,
/// written into `out` four to a pixel.  Each value is an opcode (a run of
/// zero bits and one more bit) and then that many bits plus a sign, or a
/// run of zeros.  Returns the bytes used, giving back whole bytes not
/// needed, as SAI does.
fn unpack_row(data: &[u8], out: &mut [i16], count: usize, channels: usize) -> Option<usize> {
    let mut bits = Bits { data, at: 0, mask: 0, held: 0 };
    for channel in 0..channels {
        let mut pixel = 0usize;
        while pixel < count {
            bits.fill();
            if bits.mask == 0 {
                return None;
            }
            let first = bits.mask.trailing_zeros();
            let opcode = 2 * first + ((bits.mask >> (first + 1)) & 1) as u32;
            bits.take(first + 2)?;
            match opcode {
                0 => {
                    *out.get_mut(pixel * 4 + channel)? = 0;
                    pixel += 1;
                }
                1..=14 => {
                    let raw = bits.take(opcode + 1)?;
                    let base = ((1u64 << opcode) | (raw & ((1u64 << opcode) - 1))) - 1;
                    let negative = raw >> opcode & 1 != 0;
                    let v = if negative { base.wrapping_neg() } else { base };
                    *out.get_mut(pixel * 4 + channel)? = v as i16;
                    pixel += 1;
                }
                15 => {
                    let run = bits.take(7)? as usize + 8;
                    for i in 0..run {
                        if let Some(v) = out.get_mut((pixel + i) * 4 + channel) {
                            *v = 0;
                        }
                    }
                    pixel += run;
                }
                _ => return None,
            }
        }
    }
    Some(bits.at - bits.held as usize / 8)
}

/// Turns a row of deltas into pixels: each value is predicted from the
/// pixel to the left, the one above and the one above-left, clamped to a
/// byte, and the delta added.
fn undelta_row(dest: &mut [[u8; 4]], above: &[[u8; 4]], deltas: &[i16]) {
    let mut sum = [0u16; 4];
    let mut above_left = [0u16; 4];
    for (i, d) in dest.iter_mut().enumerate() {
        let up = above.get(i).map_or([0u16; 4], |p| p.map(u16::from));
        for c in 0..4 {
            let predicted = sum[c].wrapping_add(up[c]).saturating_sub(above_left[c]).min(0xFF);
            sum[c] = predicted.wrapping_add(deltas.get(i * 4 + c).copied().unwrap_or(0) as u16);
            d[c] = (sum[c] as i16).clamp(0, 255) as u8;
        }
        above_left = up;
    }
}

/// Decodes an `intg` part: `dpcm`, then a 32-bit size for each 256-pixel
/// square tile, then the tiles row by row, each a 16-bit check and its rows
/// of deltas, with a 16-bit check after each row of tiles.  Pixels are blue,
/// green, red and (on a transparent canvas) alpha.
fn dpcm(part: &[u8], width: u32, height: u32, background: u8) -> Result<Vec<u8>, LoadError> {
    if part.get(..4) != Some(b"dpcm") {
        return Err(bad("a merged canvas that isn't dpcm"));
    }
    let pixels = width as u64 * height as u64;
    if pixels == 0 || pixels > MAX_PIXELS {
        return Err(bad(&format!("a canvas of {width} × {height}")));
    }
    let channels = if background & 7 == 0 { 4 } else { 3 };
    let (tiles_x, tiles_y) = (width.div_ceil(TILE), height.div_ceil(TILE));
    let tiles = (tiles_x * tiles_y) as usize;
    let sizes: Vec<usize> = (0..tiles).map(|i| u32_at(part, 4 + i * 4).unwrap_or(0) as usize).collect();
    let mut at = 4 + tiles * 4;
    let w = width as usize;
    let mut bgra = vec![[0u8; 4]; pixels as usize];
    let mut deltas = vec![0i16; TILE as usize * 4];
    for ty in 0..tiles_y {
        let y0 = (ty * TILE) as usize;
        let rows = (height - ty * TILE).min(TILE) as usize;
        for tx in 0..tiles_x {
            let size = sizes[(ty * tiles_x + tx) as usize];
            let tile = part
                .get(at..at.checked_add(size).ok_or_else(|| bad("a tile size"))?)
                .ok_or_else(|| bad("a short tile"))?;
            at += size;
            let x0 = (tx * TILE) as usize;
            let cols = (width - tx * TILE).min(TILE) as usize;
            let mut data = tile.get(2..).ok_or_else(|| bad("a short tile"))?;
            let mut above = vec![[0u8; 4]; cols];
            for row in 0..rows {
                deltas.fill(-1);
                let used = unpack_row(data, &mut deltas, cols, channels).ok_or_else(|| bad("damaged canvas data"))?;
                data = data.get(used..).unwrap_or_default();
                let start = (y0 + row) * w + x0;
                let dest = &mut bgra[start..start + cols];
                undelta_row(dest, &above, &deltas[..cols * 4]);
                above.copy_from_slice(dest);
            }
        }
        at += 2;
    }
    Ok(bgra.into_iter().flat_map(|[b, g, r, a]| [r, g, b, if channels == 3 { 255 } else { a }]).collect())
}

//endregion
