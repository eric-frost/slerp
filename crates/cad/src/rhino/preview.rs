//! The preview picture a Rhino file stores in its properties table: a
//! Windows bitmap (the 40-byte BITMAPINFOHEADER fields, a palette, and the
//! rows of pixels from the bottom up), either as it is or compressed
//! with zlib in openNURBS's "compressed buffer" wrapper.

use super::chunks::{ANONYMOUS_CHUNK, Archive, Chunk, Cursor, PROPERTIES_PREVIEW, PROPERTIES_PREVIEW_COMPRESSED};
use miniz_oxide::inflate::decompress_to_vec_zlib_with_limit;
use slerp_scene::textures::{ColorSpace, Texture};

/// The largest preview read, in pixels.  Rhino saves 320 x 240 or so; this
/// is for damaged and hostile files.
const MAX_PIXELS: u64 = 16 << 20;
/// The most a compressed buffer may claim to hold.
pub const MAX_BUFFER: usize = 1 << 30;

/// Reads one openNURBS compressed buffer (`ReadCompressedBufferSize` and
/// `ReadCompressedBuffer`): the size of the uncompressed data, its checksum
/// and a method byte, then either the bytes themselves or an anonymous chunk
/// holding a zlib stream.  Returns the uncompressed bytes.  `size` caps the
/// length a file may claim.
pub fn read_compressed(a: &Archive, cur: &mut Cursor, size: usize) -> Option<Vec<u8>> {
    if size > MAX_BUFFER {
        return None;
    }
    let _crc = cur.u32()?;
    match cur.u8()? {
        0 => cur.bytes(size).map(<[u8]>::to_vec),
        1 => {
            // The zlib stream sits in a chunk of its own, with a checksum.
            let sub = Archive { data: cur.data, version: a.version };
            let chunk = sub.chunk_at(cur.pos, cur.data.len())?;
            if chunk.tcode != ANONYMOUS_CHUNK {
                return None;
            }
            cur.pos = chunk.next;
            // The decompressor may need a little room past the exact size to
            // see the end of the stream; the length is checked after.
            let out = decompress_to_vec_zlib_with_limit(&cur.data[chunk.body], size + 64).ok()?;
            (out.len() == size).then_some(out)
        }
        _ => None,
    }
}

/// Reads the preview of the properties table `table` as a texture.
pub fn read(a: &Archive, table: &Chunk) -> Option<Texture> {
    let mut found: Option<Texture> = None;
    for c in a.chunks(table.body.clone()) {
        let body = a.body(&c);
        let texture = match c.tcode {
            PROPERTIES_PREVIEW_COMPRESSED => decode(a, body, true),
            PROPERTIES_PREVIEW => decode(a, body, false),
            _ => continue,
        };
        // The compressed one is the one Rhino writes; take any.
        if texture.is_some() {
            found = texture;
        }
    }
    found
}

/// Decodes a bitmap: header fields, palette, pixels.
fn decode(a: &Archive, body: &[u8], compressed: bool) -> Option<Texture> {
    let mut cur = Cursor::new(body);
    let _size = cur.i32()?;
    let width = cur.i32()?;
    let height = cur.i32()?;
    let _planes = cur.i16()?;
    let bits = cur.i16()? as i32;
    let _compression = cur.i32()?;
    let size_image = cur.i32()?;
    cur.skip(8)?; // pixels per metre
    let colors_used = cur.i32()?;
    let _important = cur.i32()?;
    let (w, h) = (width as i64, (height as i64).abs());
    if w <= 0 || h <= 0 || (w as u64) * (h as u64) > MAX_PIXELS || !matches!(bits, 1 | 4 | 8 | 16 | 24 | 32) {
        return None;
    }
    // The palette has 2^bits entries for 8 bits and fewer, unless the
    // header says how many are used.
    let palette_len = if bits <= 8 {
        let n = if colors_used > 0 { colors_used as usize } else { 1usize << bits };
        n.min(256)
    } else {
        0
    };
    let stride = ((w as usize * bits as usize).div_ceil(32)) * 4;
    let image_len = stride.checked_mul(h as usize)?;
    // The header's size is trusted only when it is plausible.
    let size_image = if size_image > 0 && size_image as usize >= image_len { size_image as usize } else { image_len };
    let (palette_bytes, pixels) = if compressed {
        // One buffer of palette and pixels together, or the palette first
        // and the pixels in a second buffer.
        let palette_size = palette_len * 4;
        let total = cur.u32()? as usize;
        let mut first = if total == palette_size + size_image {
            read_compressed(a, &mut cur, total)?
        } else if total == palette_size {
            let pal = read_compressed(a, &mut cur, total)?;
            let n = cur.u32()? as usize;
            if n != size_image {
                return None;
            }
            let mut v = pal;
            v.extend(read_compressed(a, &mut cur, n)?);
            v
        } else {
            return None;
        };
        let pixels = first.split_off(palette_size.min(first.len()));
        (first, pixels)
    } else {
        let pal = cur.bytes(palette_len * 4)?.to_vec();
        (pal, cur.bytes(size_image.min(cur.remaining()))?.to_vec())
    };
    if pixels.len() < image_len {
        return None;
    }
    let palette: Vec<[u8; 3]> = palette_bytes.chunks_exact(4).map(|c| [c[2], c[1], c[0]]).collect();
    let (w, h) = (w as usize, h as usize);
    let bottom_up = height > 0;
    let mut rgba = vec![255u8; w * h * 4];
    for row in 0..h {
        let src_row = if bottom_up { h - 1 - row } else { row };
        let line = &pixels[src_row * stride..src_row * stride + stride];
        let out = &mut rgba[row * w * 4..(row + 1) * w * 4];
        for x in 0..w {
            let px = &mut out[x * 4..x * 4 + 4];
            match bits {
                32 => px[..3].copy_from_slice(&[line[x * 4 + 2], line[x * 4 + 1], line[x * 4]]),
                24 => px[..3].copy_from_slice(&[line[x * 3 + 2], line[x * 3 + 1], line[x * 3]]),
                16 => {
                    // 5-5-5 bits.
                    let v = u16::from_le_bytes([line[x * 2], line[x * 2 + 1]]);
                    let ex = |s: u32| (((v >> s) & 31) as u32 * 255 / 31) as u8;
                    px[..3].copy_from_slice(&[ex(10), ex(5), ex(0)]);
                }
                8 => px[..3].copy_from_slice(&palette.get(line[x] as usize).copied().unwrap_or([0; 3])),
                4 => {
                    let n = if x % 2 == 0 { line[x / 2] >> 4 } else { line[x / 2] & 15 };
                    px[..3].copy_from_slice(&palette.get(n as usize).copied().unwrap_or([0; 3]));
                }
                _ => {
                    let n = (line[x / 8] >> (7 - x % 8)) & 1;
                    px[..3].copy_from_slice(&palette.get(n as usize).copied().unwrap_or([0; 3]));
                }
            }
        }
    }
    let mut t = Texture::from_rgba8(w as u32, h as u32, rgba, ColorSpace::Srgb);
    t.source.container = "Rhino 3dm".into();
    t.source.format = format!("{bits}-bit bitmap{}", if compressed { " (compressed)" } else { "" });
    Some(t)
}
