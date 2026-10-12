//! The little a viewer needs from a photograph's metadata: its EXIF
//! orientation, its colour profile (ICC) and its coding-independent code
//! points (CICP), found by walking the segments or chunks of JPEG, PNG and
//! WebP files without decoding any pixels.

use std::io::{Read, Seek, SeekFrom};

/// What a file says about how to show its pixels.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Meta {
    /// EXIF orientation, 1 to 8; 1 when the file doesn't say.
    pub orientation: u8,
    /// An embedded ICC profile.
    pub icc: Option<Vec<u8>>,
    /// Colour primaries, transfer characteristics, matrix and full-range
    /// flag, as PNG's `cICP` chunk and AVIF's `nclx` box store them.
    pub cicp: Option<[u8; 4]>,
}

impl Meta {
    pub fn upright(&self) -> bool {
        !(2..=8).contains(&self.orientation)
    }
}

/// The most a profile or EXIF block may take, so a hostile length can't
/// make the reader allocate without bound.
const MAX_BLOCK: usize = 16 << 20;

/*┌─────────────────╮
| EXIF block      |
└─────────────────╯*/
//region EXIF block

/// The orientation stored in an EXIF block: TIFF-structured bytes starting
/// with `II*\0` or `MM\0*`.  A leading `Exif\0\0` (JPEG's APP1 prefix) is
/// skipped.  `None` when there is no valid orientation tag.
pub fn orientation_of_exif(block: &[u8]) -> Option<u8> {
    let block = block.strip_prefix(b"Exif\0\0").unwrap_or(block);
    let le = match block.get(..2)? {
        b"II" => true,
        b"MM" => false,
        _ => return None,
    };
    let u16_at = |at: usize| -> Option<u16> {
        let b = block.get(at..at + 2)?;
        Some(if le { u16::from_le_bytes([b[0], b[1]]) } else { u16::from_be_bytes([b[0], b[1]]) })
    };
    let u32_at = |at: usize| -> Option<u32> {
        let b = block.get(at..at + 4)?;
        let a = [b[0], b[1], b[2], b[3]];
        Some(if le { u32::from_le_bytes(a) } else { u32::from_be_bytes(a) })
    };
    if u16_at(2)? != 42 {
        return None;
    }
    let ifd = u32_at(4)? as usize;
    let count = u16_at(ifd)? as usize;
    for i in 0..count.min(512) {
        let at = ifd.checked_add(2 + i * 12)?;
        if u16_at(at)? == 0x0112 {
            // A SHORT in the first two bytes of the value field.
            let v = u16_at(at + 8)?;
            return (1..=8).contains(&v).then_some(v as u8);
        }
    }
    None
}

//endregion

/*┌─────────────────╮
| JPEG            |
└─────────────────╯*/
//region JPEG

/// Reads the EXIF orientation and ICC profile out of a JPEG's segments
/// (they come before the image data, so only the head of the file is read).
pub fn read_jpeg(file: &mut (impl Read + Seek)) -> Meta {
    let mut meta = Meta { orientation: 1, ..Meta::default() };
    let mut soi = [0u8; 2];
    if file.read_exact(&mut soi).is_err() || soi != [0xff, 0xd8] {
        return meta;
    }
    // ICC profiles are split over several APP2 segments, numbered from 1.
    let mut icc_parts: Vec<(u8, Vec<u8>)> = Vec::new();
    let mut icc_total = 0usize;
    for _ in 0..4096 {
        // 1. The next marker, skipping fill bytes.
        let mut b = [0u8; 1];
        loop {
            if file.read_exact(&mut b).is_err() {
                return finish_jpeg(meta, icc_parts);
            }
            if b[0] == 0xff {
                break;
            }
        }
        let marker = loop {
            if file.read_exact(&mut b).is_err() {
                return finish_jpeg(meta, icc_parts);
            }
            if b[0] != 0xff {
                break b[0];
            }
        };
        match marker {
            // Markers without a length.
            0x00 | 0x01 | 0xd0..=0xd7 | 0xd8 => continue,
            // The image data (or its end) follows: nothing more to learn.
            0xda | 0xd9 => break,
            _ => {}
        }
        let mut len = [0u8; 2];
        if file.read_exact(&mut len).is_err() {
            break;
        }
        let len = u16::from_be_bytes(len) as usize;
        if len < 2 {
            break;
        }
        let body = len - 2;
        let wanted = matches!(marker, 0xe1 | 0xe2);
        if !wanted {
            if file.seek(SeekFrom::Current(body as i64)).is_err() {
                break;
            }
            continue;
        }
        let mut data = vec![0u8; body];
        if file.read_exact(&mut data).is_err() {
            break;
        }
        if marker == 0xe1 && data.starts_with(b"Exif\0\0") && meta.orientation == 1 {
            meta.orientation = orientation_of_exif(&data).unwrap_or(1);
        } else if marker == 0xe2 && data.starts_with(b"ICC_PROFILE\0") && data.len() > 14 {
            icc_total += data.len();
            if icc_total <= MAX_BLOCK {
                icc_parts.push((data[12], data[14..].to_vec()));
            }
        }
    }
    finish_jpeg(meta, icc_parts)
}

fn finish_jpeg(mut meta: Meta, mut parts: Vec<(u8, Vec<u8>)>) -> Meta {
    if !parts.is_empty() {
        parts.sort_by_key(|p| p.0);
        meta.icc = Some(parts.into_iter().flat_map(|p| p.1).collect());
    }
    meta
}

//endregion

/*┌─────────────────╮
| PNG             |
└─────────────────╯*/
//region PNG

/// Reads `eXIf`, `iCCP` and `cICP` out of a PNG's chunks.  Chunks are
/// skipped by seeking, so the image data is not read (an `eXIf` chunk may
/// come after it, so the walk goes to the end of the file).
pub fn read_png(file: &mut (impl Read + Seek)) -> Meta {
    let mut meta = Meta { orientation: 1, ..Meta::default() };
    let mut sig = [0u8; 8];
    if file.read_exact(&mut sig).is_err() || &sig != b"\x89PNG\r\n\x1a\n" {
        return meta;
    }
    for _ in 0..100_000 {
        let mut head = [0u8; 8];
        if file.read_exact(&mut head).is_err() {
            break;
        }
        let len = u32::from_be_bytes([head[0], head[1], head[2], head[3]]) as usize;
        let kind = [head[4], head[5], head[6], head[7]];
        let wanted = matches!(&kind, b"eXIf" | b"iCCP" | b"cICP") && len <= MAX_BLOCK;
        if kind == *b"IEND" {
            break;
        }
        if !wanted {
            // Chunk data and its checksum.
            if file.seek(SeekFrom::Current(len as i64 + 4)).is_err() {
                break;
            }
            continue;
        }
        let mut data = vec![0u8; len];
        if file.read_exact(&mut data).is_err() {
            break;
        }
        let mut crc = [0u8; 4];
        let _ = file.read_exact(&mut crc);
        match &kind {
            b"eXIf" => meta.orientation = orientation_of_exif(&data).unwrap_or(1),
            b"cICP" if data.len() == 4 => meta.cicp = Some([data[0], data[1], data[2], data[3]]),
            b"iCCP" => {
                // A name ending in NUL, a compression method (0 = zlib) and
                // the deflated profile.
                if let Some(nul) = data.iter().position(|&b| b == 0)
                    && data.get(nul + 1) == Some(&0)
                    && let Ok(profile) =
                        miniz_oxide::inflate::decompress_to_vec_zlib_with_limit(&data[nul + 2..], MAX_BLOCK)
                {
                    meta.icc = Some(profile);
                }
            }
            _ => {}
        }
    }
    meta
}

//endregion

/*┌─────────────────╮
| WebP            |
└─────────────────╯*/
//region WebP

/// Reads the `EXIF` and `ICCP` chunks of a WebP file.
pub fn read_webp(file: &mut (impl Read + Seek)) -> Meta {
    let mut meta = Meta { orientation: 1, ..Meta::default() };
    let mut head = [0u8; 12];
    if file.read_exact(&mut head).is_err() || &head[..4] != b"RIFF" || &head[8..] != b"WEBP" {
        return meta;
    }
    for _ in 0..100_000 {
        let mut ch = [0u8; 8];
        if file.read_exact(&mut ch).is_err() {
            break;
        }
        let len = u32::from_le_bytes([ch[4], ch[5], ch[6], ch[7]]) as usize;
        let padded = len + (len & 1);
        let wanted = matches!(&ch[..4], b"EXIF" | b"ICCP") && len <= MAX_BLOCK;
        if !wanted {
            if file.seek(SeekFrom::Current(padded as i64)).is_err() {
                break;
            }
            continue;
        }
        let mut data = vec![0u8; len];
        if file.read_exact(&mut data).is_err() {
            break;
        }
        if padded > len {
            let _ = file.seek(SeekFrom::Current(1));
        }
        if &ch[..4] == b"EXIF" {
            meta.orientation = orientation_of_exif(&data).unwrap_or(1);
        } else {
            meta.icc = Some(data);
        }
    }
    meta
}

//endregion

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    /// An EXIF block with one tag, the orientation, in either byte order.
    fn exif(le: bool, orientation: u16) -> Vec<u8> {
        let mut b = Vec::new();
        type Bytes<T, const N: usize> = fn(T) -> [u8; N];
        let (u16b, u32b): (Bytes<u16, 2>, Bytes<u32, 4>) =
            if le { (u16::to_le_bytes, u32::to_le_bytes) } else { (u16::to_be_bytes, u32::to_be_bytes) };
        b.extend(if le { b"II" } else { b"MM" });
        b.extend(u16b(42));
        b.extend(u32b(8));
        b.extend(u16b(2));
        // A tag before the orientation that must be skipped.
        b.extend(u16b(0x010f));
        b.extend(u16b(2));
        b.extend(u32b(1));
        b.extend(u32b(0));
        b.extend(u16b(0x0112));
        b.extend(u16b(3));
        b.extend(u32b(1));
        b.extend(u16b(orientation));
        b.extend([0, 0]);
        b.extend(u32b(0));
        b
    }

    #[test]
    fn orientation_in_both_byte_orders() {
        for le in [true, false] {
            for o in 1..=8 {
                assert_eq!(orientation_of_exif(&exif(le, o)), Some(o as u8));
            }
            assert_eq!(orientation_of_exif(&exif(le, 0)), None);
            assert_eq!(orientation_of_exif(&exif(le, 9)), None);
        }
        let mut prefixed = b"Exif\0\0".to_vec();
        prefixed.extend(exif(false, 6));
        assert_eq!(orientation_of_exif(&prefixed), Some(6));
    }

    #[test]
    fn damaged_exif_is_ignored() {
        let good = exif(true, 6);
        for cut in 0..good.len() {
            // Never panics, whatever is cut off.
            let _ = orientation_of_exif(&good[..cut]);
        }
        assert_eq!(orientation_of_exif(b""), None);
        assert_eq!(orientation_of_exif(b"II*\0\xff\xff\xff\xff"), None);
        // A tag count far larger than the block.
        assert_eq!(orientation_of_exif(b"II\x2a\0\x08\0\0\0\xff\xff"), None);
    }

    #[test]
    fn jpeg_segments() {
        let mut jpeg = vec![0xff, 0xd8];
        // A comment, then EXIF, then two ICC parts in the wrong order.
        jpeg.extend([0xff, 0xfe, 0, 5, b'h', b'i', b'!']);
        let e = exif(true, 8);
        jpeg.extend([0xff, 0xe1]);
        jpeg.extend(((e.len() + 8) as u16).to_be_bytes());
        jpeg.extend(b"Exif\0\0");
        jpeg.extend(&e);
        for (seq, body) in [(2u8, b"world".as_slice()), (1, b"hello ".as_slice())] {
            jpeg.extend([0xff, 0xe2]);
            jpeg.extend(((body.len() + 16) as u16).to_be_bytes());
            jpeg.extend(b"ICC_PROFILE\0");
            jpeg.extend([seq, 2]);
            jpeg.extend(body);
        }
        jpeg.extend([0xff, 0xda, 0, 2, 1, 2, 3]);
        let meta = read_jpeg(&mut Cursor::new(&jpeg));
        assert_eq!(meta.orientation, 8);
        assert_eq!(meta.icc.as_deref(), Some(b"hello world".as_slice()));
        assert!(!meta.upright());
    }

    #[test]
    fn truncated_jpeg_headers_do_not_panic() {
        let mut jpeg = vec![0xff, 0xd8, 0xff, 0xe1, 0xff, 0xff];
        jpeg.extend(b"Exif\0\0");
        for cut in 0..jpeg.len() {
            let _ = read_jpeg(&mut Cursor::new(&jpeg[..cut]));
        }
        assert_eq!(read_jpeg(&mut Cursor::new(b"not a jpeg".as_slice())).orientation, 1);
    }

    fn png_chunk(kind: &[u8; 4], data: &[u8]) -> Vec<u8> {
        let mut c = (data.len() as u32).to_be_bytes().to_vec();
        c.extend(kind);
        c.extend(data);
        c.extend([0, 0, 0, 0]);
        c
    }

    #[test]
    fn png_chunks_after_the_image_data() {
        let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
        png.extend(png_chunk(b"IHDR", &[0; 13]));
        png.extend(png_chunk(b"cICP", &[9, 16, 0, 1]));
        png.extend(png_chunk(b"IDAT", &[1, 2, 3]));
        // eXIf may follow the data.
        png.extend(png_chunk(b"eXIf", &exif(false, 3)));
        png.extend(png_chunk(b"IEND", &[]));
        let meta = read_png(&mut Cursor::new(&png));
        assert_eq!(meta.orientation, 3);
        assert_eq!(meta.cicp, Some([9, 16, 0, 1]));
        let mut profile = b"sRGB\0\0".to_vec();
        profile.extend(miniz_oxide::deflate::compress_to_vec_zlib(b"profile bytes", 1));
        let mut with_icc = b"\x89PNG\r\n\x1a\n".to_vec();
        with_icc.extend(png_chunk(b"iCCP", &profile));
        let meta = read_png(&mut Cursor::new(&with_icc));
        assert_eq!(meta.icc.as_deref(), Some(b"profile bytes".as_slice()));
    }

    #[test]
    fn webp_chunks() {
        let mut body = b"WEBP".to_vec();
        for (kind, data) in [(b"VP8X", vec![0u8; 10]), (b"EXIF", exif(true, 5)), (b"ICCP", b"abc".to_vec())] {
            body.extend(kind);
            body.extend((data.len() as u32).to_le_bytes());
            body.extend(&data);
            if data.len() % 2 == 1 {
                body.push(0);
            }
        }
        let mut webp = b"RIFF".to_vec();
        webp.extend((body.len() as u32).to_le_bytes());
        webp.extend(&body);
        let meta = read_webp(&mut Cursor::new(&webp));
        assert_eq!(meta.orientation, 5);
        assert_eq!(meta.icc.as_deref(), Some(b"abc".as_slice()));
    }
}
