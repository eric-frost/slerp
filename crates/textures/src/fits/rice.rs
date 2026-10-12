//! Rice decompression, as FITS tile compression uses it (`RICE_1`): blocks
//! of pixels coded as differences from their neighbour, each block with the
//! number of low bits that suit it (Golomb-Rice coding).
//!
//! The stream starts with the first pixel verbatim; every block then gets a
//! small "split" value `fs` giving how many low bits of each difference are
//! stored plainly, the rest as a unary count.  Two special values mean the
//! block is all zeros (nothing stored) and that it is stored raw.

use crate::TextureError;
use crate::util::invalid;

/// Decodes `count` pixels of `bytepix` bytes (1, 2 or 4) with blocks of
/// `block` pixels.  The results are the pixels' values as signed integers
/// (unsigned for bytes).
pub(crate) fn decode(data: &[u8], count: usize, block: usize, bytepix: usize) -> Result<Vec<i32>, TextureError> {
    // fs is stored in this many bits, and the raw-block marker is 2^fsbits - 1 - 6
    // below the largest value that fits.
    let (fsbits, fsmax) = match bytepix {
        1 => (3u32, 6i32),
        2 => (4, 14),
        4 => (5, 25),
        _ => return Err(invalid(format!("Rice: {bytepix}-byte pixels"))),
    };
    if block == 0 {
        return Err(invalid("Rice: block size 0"));
    }
    let bbits = 1u32 << fsbits;
    let truncated = || invalid("Rice: compressed data ends early");
    if data.len() < bytepix + 1 {
        return Err(truncated());
    }
    // 1. The first pixel, then the bit reader's first byte.
    let mut last: u32 = 0;
    for &b in &data[..bytepix] {
        last = (last << 8) | b as u32;
    }
    let mut cp = bytepix;
    let mut b = data[cp] as u32;
    cp += 1;
    let mut nbits: i32 = 8;
    let mask = |bits: i32| -> u32 { if bits >= 32 { u32::MAX } else { (1u32 << bits) - 1 } };
    let wrap = |v: u32| -> u32 {
        match bytepix {
            1 => v & 0xff,
            2 => v & 0xffff,
            _ => v,
        }
    };
    let mut out: Vec<i32> = Vec::with_capacity(count);
    let mut i = 0;
    let next = |cp: &mut usize| -> Result<u32, TextureError> {
        let v = *data.get(*cp).ok_or_else(truncated)?;
        *cp += 1;
        Ok(v as u32)
    };
    while i < count {
        // 2. The block's split value.
        nbits -= fsbits as i32;
        while nbits < 0 {
            b = (b << 8) | next(&mut cp)?;
            nbits += 8;
        }
        let fs = (b >> nbits) as i32 - 1;
        b &= mask(nbits);
        let end = (i + block).min(count);
        if fs < 0 {
            // 3a. Differences all zero.
            for _ in i..end {
                out.push(to_signed(last, bytepix));
            }
            i = end;
        } else if fs == fsmax {
            // 3b. Stored raw, `bbits` bits per difference.
            while i < end {
                let mut k = bbits as i32 - nbits;
                let mut diff: u32 = b.checked_shl(k as u32).unwrap_or(0);
                k -= 8;
                while k >= 0 {
                    b = next(&mut cp)?;
                    diff |= b << k;
                    k -= 8;
                }
                if nbits > 0 {
                    b = next(&mut cp)?;
                    diff |= b >> (-k);
                    b &= mask(nbits);
                } else {
                    b = 0;
                }
                diff = unmap(diff);
                last = wrap(diff.wrapping_add(last));
                out.push(to_signed(last, bytepix));
                i += 1;
            }
        } else {
            // 3c. Unary count of the high part, `fs` plain low bits.
            while i < end {
                while b == 0 {
                    nbits += 8;
                    b = next(&mut cp)?;
                }
                let nzero = nbits - (32 - b.leading_zeros() as i32);
                nbits -= nzero + 1;
                b ^= 1 << nbits;
                nbits -= fs;
                while nbits < 0 {
                    b = (b << 8) | next(&mut cp)?;
                    nbits += 8;
                }
                let diff = ((nzero as u32).checked_shl(fs as u32).unwrap_or(0)) | (b >> nbits);
                b &= mask(nbits);
                last = wrap(unmap(diff).wrapping_add(last));
                out.push(to_signed(last, bytepix));
                i += 1;
            }
        }
    }
    Ok(out)
}

/// Undoes the mapping of signed differences onto unsigned numbers (0, -1,
/// 1, -2, ... become 0, 1, 2, 3, ...).
fn unmap(diff: u32) -> u32 {
    if diff & 1 == 0 { diff >> 1 } else { !(diff >> 1) }
}

fn to_signed(v: u32, bytepix: usize) -> i32 {
    match bytepix {
        1 => (v & 0xff) as i32,
        2 => v as u16 as i16 as i32,
        _ => v as i32,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A compact Rice encoder following the same format, for round trips.
    pub(crate) fn encode(values: &[i32], block: usize, bytepix: usize) -> Vec<u8> {
        let (fsbits, fsmax) = match bytepix {
            1 => (3u32, 6i32),
            2 => (4, 14),
            _ => (5, 25),
        };
        let bbits = 1u32 << fsbits;
        let mut out = Vec::new();
        let first = values[0] as u32;
        for k in (0..bytepix).rev() {
            out.push((first >> (8 * k)) as u8);
        }
        let mut bits: Vec<bool> = Vec::new();
        let push = |bits: &mut Vec<bool>, v: u64, n: u32| {
            for k in (0..n).rev() {
                bits.push((v >> k) & 1 == 1);
            }
        };
        let mut last = first;
        for chunk in values.chunks(block) {
            let mut diffs = Vec::new();
            for &v in chunk {
                let d = (v as u32).wrapping_sub(last);
                let d = match bytepix {
                    1 => d as u8 as i8 as i32,
                    2 => d as u16 as i16 as i32,
                    _ => d as i32,
                };
                last = v as u32;
                // Map signed to unsigned.
                diffs.push(if d >= 0 { (d as u32) << 1 } else { (!(d as u32)) << 1 | 1 });
            }
            let sum: u64 = diffs.iter().map(|&d| d as u64).sum();
            if sum == 0 {
                push(&mut bits, 0, fsbits);
                continue;
            }
            // Pick fs near log2 of the mean.
            let mean = sum / chunk.len() as u64;
            let mut fs = 0i32;
            while (1u64 << (fs + 1)) <= mean.max(1) {
                fs += 1;
            }
            let unary_cost = |fs: i32| -> u64 { diffs.iter().map(|&d| (d as u64 >> fs) + 1 + fs as u64).sum() };
            if fs >= fsmax || unary_cost(fs) > (bbits as u64) * chunk.len() as u64 {
                push(&mut bits, (fsmax + 1) as u64, fsbits);
                for &d in &diffs {
                    push(&mut bits, d as u64, bbits);
                }
                continue;
            }
            push(&mut bits, (fs + 1) as u64, fsbits);
            for &d in &diffs {
                let high = d >> fs;
                bits.resize(bits.len() + high as usize, false);
                bits.push(true);
                push(&mut bits, (d & ((1u32 << fs) - 1)) as u64, fs as u32);
            }
        }
        for chunk in bits.chunks(8) {
            let mut b = 0u8;
            for (k, bit) in chunk.iter().enumerate() {
                if *bit {
                    b |= 0x80 >> k;
                }
            }
            out.push(b);
        }
        out
    }

    #[test]
    fn round_trips_every_pixel_size() {
        let wave: Vec<i32> = (0..500).map(|i| 1000 + ((i as f32 * 0.3).sin() * 50.0) as i32 + (i % 3)).collect();
        for (bytepix, block) in [(4usize, 32usize), (4, 16), (2, 32), (1, 32)] {
            let values: Vec<i32> = match bytepix {
                1 => wave.iter().map(|v| v & 0xff).collect(),
                2 => wave.iter().map(|&v| v - 1200).collect(),
                _ => wave.iter().map(|&v| v * 1000).collect(),
            };
            let data = encode(&values, block, bytepix);
            assert_eq!(decode(&data, values.len(), block, bytepix).unwrap(), values, "bytepix {bytepix} block {block}");
        }
    }

    #[test]
    fn flat_blocks_and_noise() {
        let flat = vec![7i32; 100];
        assert_eq!(decode(&encode(&flat, 32, 4), 100, 32, 4).unwrap(), flat);
        let mut state = 99u32;
        let noise: Vec<i32> = (0..300)
            .map(|_| {
                state = state.wrapping_mul(1664525).wrapping_add(1013904223);
                (state >> 8) as i32 - (1 << 23)
            })
            .collect();
        assert_eq!(decode(&encode(&noise, 32, 4), 300, 32, 4).unwrap(), noise);
    }

    #[test]
    fn damaged_data_errors() {
        let values: Vec<i32> = (0..200).collect();
        let data = encode(&values, 32, 4);
        assert!(decode(&data[..data.len() / 2], 200, 32, 4).is_err());
        assert!(decode(&[], 5, 32, 4).is_err());
        assert!(decode(&data, 200, 0, 4).is_err());
        assert!(decode(&data, 200, 32, 3).is_err());
        for cut in 0..data.len() {
            let _ = decode(&data[..cut], 200, 32, 4);
        }
        // Random garbage never panics.
        let mut state = 7u32;
        for _ in 0..200 {
            let junk: Vec<u8> = (0..64)
                .map(|_| {
                    state = state.wrapping_mul(1664525).wrapping_add(1013904223);
                    (state >> 16) as u8
                })
                .collect();
            let _ = decode(&junk, 100, 32, 4);
            let _ = decode(&junk, 100, 16, 2);
        }
    }
}
