//! A small GIF reader for the cover pictures TIC-80 cartridges carry: the
//! first frame, with local or global palette, interlacing and transparency
//! (transparent pixels come out black).

use crate::pic::{Pic, check_size};
use slerp_scene::LoadError;

pub fn is_gif(b: &[u8]) -> bool {
    b.starts_with(b"GIF87a") || b.starts_with(b"GIF89a")
}

/// The most pixels a cover picture may have; TIC-80's are 240x136.
const MAX_COVER_PIXELS: u64 = 1 << 22;

pub fn decode(b: &[u8]) -> Result<Pic, LoadError> {
    if !is_gif(b) || b.len() < 13 {
        return Err(LoadError::invalid("not a GIF"));
    }
    let (sw, sh) = (u16::from_le_bytes([b[6], b[7]]) as u32, u16::from_le_bytes([b[8], b[9]]) as u32);
    check_size(sw, sh)?;
    if sw as u64 * sh as u64 > MAX_COVER_PIXELS {
        return Err(LoadError::invalid("GIF is larger than a cover picture"));
    }
    let flags = b[10];
    let mut at = 13;
    let mut global: Vec<[u8; 3]> = Vec::new();
    if flags & 0x80 != 0 {
        let n = 2usize << (flags & 7);
        global = palette(b, at, n)?;
        at += n * 3;
    }
    let mut transparent: Option<u8> = None;
    loop {
        match *b.get(at).ok_or_else(|| LoadError::invalid("GIF has no picture"))? {
            0x21 => {
                let label = *b.get(at + 1).unwrap_or(&0);
                at += 2;
                if label == 0xf9 && b.get(at) == Some(&4) && b.len() > at + 4 {
                    transparent = (b[at + 1] & 1 != 0).then_some(b[at + 4]);
                }
                at = skip_blocks(b, at)?;
            }
            0x2c => break,
            _ => return Err(LoadError::invalid("GIF has no picture")),
        }
    }
    let d = b.get(at + 1..at + 10).ok_or_else(|| LoadError::invalid("GIF is cut off"))?;
    let (ix, iy) = (u16::from_le_bytes([d[0], d[1]]) as u32, u16::from_le_bytes([d[2], d[3]]) as u32);
    let (iw, ih) = (u16::from_le_bytes([d[4], d[5]]) as u32, u16::from_le_bytes([d[6], d[7]]) as u32);
    let iflags = d[8];
    at += 10;
    check_size(iw.max(1), ih.max(1))?;
    let mut table = global;
    if iflags & 0x80 != 0 {
        let n = 2usize << (iflags & 7);
        table = palette(b, at, n)?;
        at += n * 3;
    }
    let min_code = *b.get(at).ok_or_else(|| LoadError::invalid("GIF is cut off"))?;
    at += 1;
    if !(1..=11).contains(&min_code) {
        return Err(LoadError::invalid("bad GIF code size"));
    }
    let mut data = Vec::new();
    loop {
        let n = *b.get(at).unwrap_or(&0) as usize;
        at += 1;
        if n == 0 {
            break;
        }
        match b.get(at..at + n) {
            Some(chunk) => data.extend_from_slice(chunk),
            None => {
                data.extend_from_slice(b.get(at..).unwrap_or(&[]));
                break;
            }
        }
        at += n;
    }
    let indices = lzw(&data, min_code, iw as usize * ih as usize);
    let mut pic = Pic::new(sw, sh);
    // Rows of interlaced pictures come in four passes.
    let order: Vec<u32> = if iflags & 0x40 != 0 {
        let mut o = Vec::new();
        for (start, step) in [(0, 8), (4, 8), (2, 4), (1, 2)] {
            o.extend((start..ih).step_by(step));
        }
        o
    } else {
        (0..ih).collect()
    };
    for (k, &row) in order.iter().enumerate() {
        for x in 0..iw {
            let Some(&i) = indices.get(k * iw as usize + x as usize) else { continue };
            if Some(i) == transparent {
                continue;
            }
            if let Some(&c) = table.get(i as usize) {
                pic.set(ix + x, iy + row, c);
            }
        }
    }
    Ok(pic)
}

fn palette(b: &[u8], at: usize, n: usize) -> Result<Vec<[u8; 3]>, LoadError> {
    let raw = b.get(at..at + n * 3).ok_or_else(|| LoadError::invalid("GIF palette is cut off"))?;
    Ok(raw.chunks_exact(3).map(|c| [c[0], c[1], c[2]]).collect())
}

fn skip_blocks(b: &[u8], mut at: usize) -> Result<usize, LoadError> {
    loop {
        let n = *b.get(at).ok_or_else(|| LoadError::invalid("GIF is cut off"))? as usize;
        at += 1 + n;
        if n == 0 {
            return Ok(at);
        }
    }
}

/// Variable-width LZW as GIF uses it, stopping after `limit` indices.
fn lzw(data: &[u8], min_code: u8, limit: usize) -> Vec<u8> {
    let clear = 1u16 << min_code;
    let end = clear + 1;
    // Each table entry is a previous code plus one byte; codes below
    // `clear` are the literal bytes.
    let mut prefix = vec![0u16; 4096];
    let mut suffix = vec![0u8; 4096];
    let mut next = end + 1;
    let mut size = min_code as u32 + 1;
    let mut out: Vec<u8> = Vec::with_capacity(limit.min(1 << 24));
    let (mut bits, mut nbits) = (0u32, 0u32);
    let mut prev: Option<u16> = None;
    let mut stack: Vec<u8> = Vec::new();
    // Writes the string for `code` into `stack`, last byte first, and
    // returns its first byte.
    let expand = |code: u16, prefix: &[u16], suffix: &[u8], stack: &mut Vec<u8>| -> Option<u8> {
        stack.clear();
        let mut c = code;
        while c >= clear {
            if stack.len() > 4096 || c <= end {
                return None;
            }
            stack.push(suffix[c as usize]);
            c = prefix[c as usize];
        }
        stack.push(c as u8);
        Some(c as u8)
    };
    for &byte in data {
        bits |= (byte as u32) << nbits;
        nbits += 8;
        while nbits >= size {
            let code = (bits & ((1 << size) - 1)) as u16;
            bits >>= size;
            nbits -= size;
            if code == clear {
                next = end + 1;
                size = min_code as u32 + 1;
                prev = None;
                continue;
            }
            if code == end {
                return out;
            }
            let Some(p) = prev else {
                if code >= clear {
                    return out;
                }
                out.push(code as u8);
                prev = Some(code);
                continue;
            };
            if code > next {
                return out;
            }
            // 1. The string this code stands for (for the code about to be
            //    defined, the previous string plus its own first byte).
            let (first, extra) = if code < next {
                let Some(f) = expand(code, &prefix, &suffix, &mut stack) else { return out };
                (f, None)
            } else {
                let Some(f) = expand(p, &prefix, &suffix, &mut stack) else { return out };
                (f, Some(f))
            };
            for &s in stack.iter().rev().chain(extra.iter()) {
                if out.len() >= limit {
                    return out;
                }
                out.push(s);
            }
            // 2. Define the next table entry.
            if next < 4096 {
                prefix[next as usize] = p;
                suffix[next as usize] = first;
                next += 1;
                if next == (1 << size) && size < 12 {
                    size += 1;
                }
            }
            prev = Some(code);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 2x2 GIF with a 4-colour palette, made by ImageMagick-style tools:
    /// indices 0 1 / 2 3.
    const TINY: &[u8] = &[
        b'G', b'I', b'F', b'8', b'9', b'a', 2, 0, 2, 0, 0x81, 0, 0, // header, global table of 4
        255, 0, 0, 0, 255, 0, 0, 0, 255, 255, 255, 0, // palette
        0x2c, 0, 0, 0, 0, 2, 0, 2, 0, 0, // image descriptor
        2, 3, 0x44, 0x34, 0x05, 0, // min code 2, one block: codes 4 0 1 2 3 5 packed
        0x3b,
    ];

    #[test]
    fn decodes_a_tiny_picture() {
        // The packed block above was produced for indices 0,1,2,3.
        let pic = decode(TINY).unwrap();
        assert_eq!((pic.width, pic.height), (2, 2));
        assert_eq!(pic.get(0, 0), [255, 0, 0]);
        assert_eq!(pic.get(1, 0), [0, 255, 0]);
        assert_eq!(pic.get(0, 1), [0, 0, 255]);
        assert_eq!(pic.get(1, 1), [255, 255, 0]);
    }

    #[test]
    fn rejects_garbage() {
        assert!(decode(b"GIF89a").is_err());
        assert!(decode(&TINY[..20]).is_err());
        let mut bad = TINY.to_vec();
        bad[6] = 0xff;
        bad[7] = 0xff;
        bad[8] = 0xff;
        bad[9] = 0xff;
        assert!(decode(&bad).is_err());
    }
}
