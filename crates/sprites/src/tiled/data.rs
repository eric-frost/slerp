//! Tile layer data: CSV, base64, and base64 compressed with zlib, gzip or
//! zstd.  The decoded size is known in advance (four bytes per tile), so
//! decompression stops there and a small file can't claim gigabytes.

use base64::Engine;
use slerp_scene::LoadError;

/// Decodes `count` tile ids from a layer's text.
pub fn decode(text: &str, encoding: &str, compression: &str, count: usize) -> Result<Vec<u32>, LoadError> {
    match encoding {
        "csv" => Ok(csv(text, count)),
        "base64" => {
            let packed = base64_bytes(text)?;
            from_bytes(&decompress(&packed, compression, count * 4)?, count)
        }
        e => Err(LoadError::Unsupported(format!("layer encoding {e:?}"))),
    }
}

/// Comma-separated ids, with any whitespace.
pub fn csv(text: &str, count: usize) -> Vec<u32> {
    let mut out: Vec<u32> = text
        .split(|c: char| c == ',' || c.is_ascii_whitespace())
        .filter(|s| !s.is_empty())
        .take(count)
        .map(|s| s.parse::<u64>().map_or(0, |v| v as u32))
        .collect();
    out.resize(count, 0);
    out
}

pub fn base64_bytes(text: &str) -> Result<Vec<u8>, LoadError> {
    let clean: Vec<u8> = text.bytes().filter(|b| !b.is_ascii_whitespace()).collect();
    base64::engine::general_purpose::STANDARD
        .decode(&clean)
        .map_err(|e| LoadError::invalid(format!("layer data isn't base64: {e}")))
}

/// Inflates layer data to at most `len` bytes.
pub fn decompress(data: &[u8], compression: &str, len: usize) -> Result<Vec<u8>, LoadError> {
    let bad = |what: &str| LoadError::invalid(format!("damaged {what} layer data"));
    match compression {
        "" => Ok(data.to_vec()),
        "zlib" => miniz_oxide::inflate::decompress_to_vec_zlib_with_limit(data, len).map_err(|_| bad("zlib")),
        "gzip" => {
            let body = gzip_body(data).ok_or_else(|| bad("gzip"))?;
            miniz_oxide::inflate::decompress_to_vec_with_limit(body, len).map_err(|_| bad("gzip"))
        }
        "zstd" => zstd::bulk::decompress(data, len).map_err(|_| bad("zstd")),
        c => Err(LoadError::Unsupported(format!("layer compression {c:?}"))),
    }
}

/// The deflate stream inside a gzip member (RFC 1952), past its header.
fn gzip_body(d: &[u8]) -> Option<&[u8]> {
    if d.len() < 18 || d[0] != 0x1f || d[1] != 0x8b || d[2] != 8 {
        return None;
    }
    let flags = d[3];
    let mut at = 10usize;
    if flags & 4 != 0 {
        let n = u16::from_le_bytes([*d.get(at)?, *d.get(at + 1)?]) as usize;
        at += 2 + n;
    }
    for bit in [8u8, 16] {
        if flags & bit != 0 {
            at += d.get(at..)?.iter().position(|&b| b == 0)? + 1;
        }
    }
    if flags & 2 != 0 {
        at += 2;
    }
    d.get(at..)
}

/// Little-endian ids, exactly `count` of them: missing ones are empty.
pub fn from_bytes(bytes: &[u8], count: usize) -> Result<Vec<u32>, LoadError> {
    let mut out: Vec<u32> =
        bytes.chunks_exact(4).take(count).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]])).collect();
    out.resize(count, 0);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn le(ids: &[u32]) -> Vec<u8> {
        ids.iter().flat_map(|v| v.to_le_bytes()).collect()
    }

    #[test]
    fn every_encoding_gives_the_same_ids() {
        let ids = [1u32, 2, 0x8000_0003, 0, 7, 9];
        let raw = le(&ids);
        let b64 = |b: &[u8]| base64::engine::general_purpose::STANDARD.encode(b);
        assert_eq!(decode("1,2,2147483651,\n0,7,9", "csv", "", 6).unwrap(), ids);
        assert_eq!(decode(&b64(&raw), "base64", "", 6).unwrap(), ids);
        let zlib = miniz_oxide::deflate::compress_to_vec_zlib(&raw, 6);
        assert_eq!(decode(&format!("\n  {}\n", b64(&zlib)), "base64", "zlib", 6).unwrap(), ids);
        // gzip: a header, the deflate stream, and the CRC and length.
        let mut gz = vec![0x1f, 0x8b, 8, 0, 0, 0, 0, 0, 0, 3];
        gz.extend(miniz_oxide::deflate::compress_to_vec(&raw, 6));
        gz.extend([0u8; 8]);
        assert_eq!(decode(&b64(&gz), "base64", "gzip", 6).unwrap(), ids);
        let zst = zstd::bulk::compress(&raw, 3).unwrap();
        assert_eq!(decode(&b64(&zst), "base64", "zstd", 6).unwrap(), ids);
        // Short data pads with empty tiles; long data is cut.
        assert_eq!(decode("1,2", "csv", "", 4).unwrap(), [1, 2, 0, 0]);
        assert_eq!(decode(&b64(&raw), "base64", "", 2).unwrap(), [1, 2]);
        // A bomb stops at the size asked for.
        let bomb = miniz_oxide::deflate::compress_to_vec_zlib(&vec![0u8; 1 << 20], 9);
        assert!(decode(&b64(&bomb), "base64", "zlib", 4).is_err());
        assert!(decode("!!!", "base64", "zlib", 4).is_err());
        assert!(decode("AAAA", "base64", "lzma", 1).is_err());
    }
}
