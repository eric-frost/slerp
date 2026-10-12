//! HCOMPRESS (`HCOMPRESS_1`), Richard White's compressor for astronomical
//! images: a two-dimensional Haar-like "H transform" of the tile, optional
//! division by a scale factor (lossy), and quadtree coding of the bit
//! planes of the transform's four quadrants.
//!
//! Decoding undoes that in reverse: read the bit planes, restore the sign
//! bits, multiply by the scale, and run the inverse transform, which at each
//! of log2(n) levels interleaves the coefficients and combines each 2x2
//! group of them into four pixels.  With the smoothing option the
//! coefficients are first nudged to remove the blockiness a coarse scale
//! leaves.

use crate::TextureError;
use crate::util::invalid;

/// Marks an HCOMPRESS stream.
const MAGIC: [u8; 2] = [0xdd, 0x99];

/// The Huffman code for 4-bit quadtree values: (code, length in bits), in
/// order of value.
const CODES: [(u32, u32); 16] = [
    (0x3e, 6),
    (0x00, 3),
    (0x01, 3),
    (0x08, 4),
    (0x02, 3),
    (0x09, 4),
    (0x1a, 5),
    (0x1b, 5),
    (0x03, 3),
    (0x1c, 5),
    (0x0a, 4),
    (0x1d, 5),
    (0x0b, 4),
    (0x1e, 5),
    (0x3f, 6),
    (0x0c, 4),
];

/// Decodes a tile of `width` columns and `rows` rows to integers.
pub(crate) fn decode(
    data: &[u8],
    width: usize,
    rows: usize,
    scale: i32,
    smooth: bool,
) -> Result<Vec<i32>, TextureError> {
    let bad = |what: &str| invalid(format!("HCOMPRESS: {what}"));
    if data.len() < 14 + 8 + 3 || data[..2] != MAGIC {
        return Err(bad("not an HCOMPRESS stream"));
    }
    let int = |at: usize| i32::from_be_bytes([data[at], data[at + 1], data[at + 2], data[at + 3]]);
    // The stream counts rows first and columns second.
    let (nx, ny, stream_scale) = (int(2), int(6), int(10));
    if nx < 0 || ny < 0 || nx as usize != rows || ny as usize != width {
        return Err(bad("size differs from the tile's"));
    }
    let (nx, ny) = (nx as usize, ny as usize);
    let sum = i64::from_be_bytes(data[14..22].try_into().expect("8 bytes"));
    let planes = [data[22], data[23], data[24]];
    if planes.iter().any(|&p| p > 32) {
        return Err(bad("too many bit planes"));
    }
    let nel = nx.checked_mul(ny).filter(|&n| n > 0 && n <= 1 << 28).ok_or_else(|| bad("empty or huge tile"))?;
    let mut a = vec![0i32; nel];
    let mut bits = Bits { data: &data[25..], pos: 0, buf: 0, count: 0 };

    // 1. The four quadrants' bit planes.
    let (nx2, ny2) = (nx.div_ceil(2), ny.div_ceil(2));
    qtree_decode(&mut bits, &mut a, 0, ny, nx2, ny2, planes[0])?;
    qtree_decode(&mut bits, &mut a, ny2, ny, nx2, ny / 2, planes[1])?;
    qtree_decode(&mut bits, &mut a, ny * nx2, ny, nx / 2, ny2, planes[1])?;
    qtree_decode(&mut bits, &mut a, ny * nx2 + ny2, ny, nx / 2, ny / 2, planes[2])?;
    if bits.nybble()? != 0 {
        return Err(bad("bad bit plane values"));
    }
    // 2. Signs follow, starting on a byte boundary, one bit for each
    //    nonzero coefficient.
    bits.align();
    for v in a.iter_mut() {
        if *v != 0 && bits.bit()? != 0 {
            *v = -*v;
        }
    }
    a[0] = sum as i32;

    // 3. Undo the scaling and the transform.
    // The stream's own scale is the one that was applied.
    let _ = scale;
    let scale = stream_scale;
    if scale > 1 {
        for v in a.iter_mut() {
            *v = v.wrapping_mul(scale);
        }
    }
    hinv(&mut a, nx, ny, smooth, scale);
    Ok(a)
}

/*┌─────────────────╮
| Bit input       |
└─────────────────╯*/
//region Bit input

/// Most significant bit first, as the format packs them.
struct Bits<'a> {
    data: &'a [u8],
    pos: usize,
    buf: u64,
    count: u32,
}

impl Bits<'_> {
    fn take(&mut self, n: u32) -> Result<u32, TextureError> {
        while self.count < n {
            let b = *self.data.get(self.pos).ok_or_else(|| invalid("HCOMPRESS: data ends early"))?;
            self.pos += 1;
            self.buf = (self.buf << 8) | b as u64;
            self.count += 8;
        }
        self.count -= n;
        let v = ((self.buf >> self.count) & ((1u64 << n) - 1)) as u32;
        self.buf &= (1u64 << self.count) - 1;
        Ok(v)
    }

    /// Skips the rest of the byte being read.
    fn align(&mut self) {
        self.count = 0;
        self.buf = 0;
    }

    fn bit(&mut self) -> Result<u32, TextureError> {
        self.take(1)
    }

    fn nybble(&mut self) -> Result<u32, TextureError> {
        self.take(4)
    }

    /// Reads one Huffman-coded 4-bit value.
    fn huffman(&mut self) -> Result<u8, TextureError> {
        let (mut code, mut len) = (0u32, 0u32);
        while len < 6 {
            code = (code << 1) | self.bit()?;
            len += 1;
            if let Some(v) = CODES.iter().position(|&(c, l)| l == len && c == code) {
                return Ok(v as u8);
            }
        }
        Err(invalid("HCOMPRESS: bad Huffman code"))
    }
}

//endregion

/*┌─────────────────╮
| Quadtree        |
└─────────────────╯*/
//region Quadtree

/// Reads `bitplanes` bit planes of one quadrant (`nqx` rows by `nqy`
/// columns starting at `start` in an array whose rows are `n` long) and sets
/// the bits in the array.
fn qtree_decode(
    bits: &mut Bits,
    a: &mut [i32],
    start: usize,
    n: usize,
    nqx: usize,
    nqy: usize,
    bitplanes: u8,
) -> Result<(), TextureError> {
    if nqx == 0 || nqy == 0 {
        return Ok(());
    }
    let nqmax = nqx.max(nqy);
    let log2n = usize::BITS - (nqmax - 1).leading_zeros();
    let cells = nqx.div_ceil(2) * nqy.div_ceil(2);
    let mut scratch = vec![0u8; cells];
    for bit in (0..bitplanes as u32).rev() {
        match bits.nybble()? {
            0 => {
                // Written directly, four pixels to a nybble.
                for s in scratch.iter_mut() {
                    *s = bits.nybble()? as u8;
                }
            }
            0xf => {
                scratch[0] = bits.huffman()?;
                let (mut px, mut py) = (1usize, 1usize);
                for k in 1..log2n {
                    // The size of the level: the quadrant's size halved
                    // log2n - k times, rounded up.
                    let shift = log2n - k;
                    let nx = nqx.div_ceil(1 << shift);
                    let ny = nqy.div_ceil(1 << shift);
                    expand(bits, &mut scratch, px, py, nx, ny)?;
                    (px, py) = (nx, ny);
                }
            }
            _ => return Err(invalid("HCOMPRESS: bad format code")),
        }
        insert_plane(&scratch, nqx, nqy, a, start, n, bit);
    }
    Ok(())
}

/// Expands a `px` x `py` grid of 4-bit values in `scratch` to `nx` x `ny`:
/// each value's four bits say which of its four children are nonzero, and
/// every nonzero child then reads its own value from the stream (in
/// reverse order, as the writer emitted them).
fn expand(bits: &mut Bits, scratch: &mut [u8], px: usize, py: usize, nx: usize, ny: usize) -> Result<(), TextureError> {
    let old: Vec<u8> = scratch[..px * py].to_vec();
    for i in 0..nx {
        for j in 0..ny {
            let parent = old[(i / 2) * py + j / 2];
            let child = 3 - ((i & 1) * 2 + (j & 1));
            scratch[i * ny + j] = (parent >> child) & 1;
        }
    }
    for i in (0..nx * ny).rev() {
        if scratch[i] != 0 {
            scratch[i] = bits.huffman()?;
        }
    }
    Ok(())
}

/// Sets bit `bit` of the coefficients the 4-bit values in `scratch` mark
/// (each value covers a 2x2 block of the quadrant).
fn insert_plane(scratch: &[u8], nqx: usize, nqy: usize, a: &mut [i32], start: usize, n: usize, bit: u32) {
    let plane = 1i32 << bit;
    let cols = nqy.div_ceil(2);
    for (k, &v) in scratch.iter().enumerate().take(nqx.div_ceil(2) * cols) {
        let (bi, bj) = ((k / cols) * 2, (k % cols) * 2);
        for (child, (di, dj)) in [(3, (0, 0)), (2, (0, 1)), (1, (1, 0)), (0, (1, 1))] {
            if (v >> child) & 1 != 0 && bi + di < nqx && bj + dj < nqy {
                a[start + (bi + di) * n + bj + dj] |= plane;
            }
        }
    }
}

//endregion

/*┌─────────────────╮
| Inverse H       |
└─────────────────╯*/
//region Inverse H

/// Interleaves the first and second halves of `n` elements spaced `n2`
/// apart: the smooth coefficients go to the even places and the detail
/// coefficients to the odd ones.
fn unshuffle(a: &mut [i32], start: usize, n: usize, n2: usize, tmp: &mut [i32]) {
    let nhalf = n.div_ceil(2);
    for (t, i) in tmp.iter_mut().zip(nhalf..n) {
        *t = a[start + n2 * i];
    }
    for i in (0..nhalf).rev() {
        a[start + n2 * 2 * i] = a[start + n2 * i];
    }
    for (t, i) in (1..n).step_by(2).enumerate() {
        a[start + n2 * i] = tmp[t];
    }
}

fn hinv(a: &mut [i32], nx: usize, ny: usize, smooth: bool, scale: i32) {
    let nmax = nx.max(ny);
    let log2n = (usize::BITS - (nmax.max(1) - 1).leading_zeros()) as i32;
    let mut tmp = vec![0i32; nmax.div_ceil(2) + 1];

    // Rounding and shifting masks, halved at each level.
    let mut shift = 1;
    let mut bit0: i32 = 1 << (log2n - 1).max(0);
    let mut bit1 = bit0 << 1;
    let mut bit2 = bit0 << 2;
    let mut mask0 = bit0.wrapping_neg();
    let mut mask1 = mask0 << 1;
    let mask2 = mask0 << 2;
    let mut prnd0 = bit0 >> 1;
    let mut prnd1 = bit1 >> 1;
    let prnd2 = bit2 >> 1;
    let mut nrnd0 = prnd0 - 1;
    let mut nrnd1 = prnd1 - 1;
    let nrnd2 = prnd2 - 1;

    // Round the overall mean to a multiple of bit2.
    a[0] = a[0].wrapping_add(if a[0] >= 0 { prnd2 } else { nrnd2 }) & mask2;

    let (mut nxtop, mut nytop) = (1usize, 1usize);
    let (mut nxf, mut nyf) = (nx, ny);
    let mut c: usize = 1 << log2n;
    for k in (0..log2n).rev() {
        // The sequence ntop[k-1] = (ntop[k] + 1) / 2 ending at the size.
        c >>= 1;
        nxtop <<= 1;
        nytop <<= 1;
        if nxf <= c {
            nxtop -= 1;
        } else {
            nxf -= c;
        }
        if nyf <= c {
            nytop -= 1;
        } else {
            nyf -= c;
        }
        // The last pass divides by 4, and has no rounding for bit 0.
        if k == 0 {
            nrnd0 = 0;
            shift = 2;
        }
        for i in 0..nxtop {
            unshuffle(a, ny * i, nytop, 1, &mut tmp);
        }
        for j in 0..nytop {
            unshuffle(a, j, nxtop, ny, &mut tmp);
        }
        if smooth {
            hsmooth(a, nxtop, nytop, ny, scale);
        }
        let (oddx, oddy) = (nxtop % 2, nytop % 2);
        let mut i = 0;
        while i < nxtop - oddx {
            let mut s00 = ny * i;
            let mut s10 = s00 + ny;
            let mut j = 0;
            while j < nytop - oddy {
                let h0 = a[s00];
                let mut hx = a[s10];
                let mut hy = a[s00 + 1];
                let mut hc = a[s10 + 1];
                // Round hx and hy to multiples of bit1, hc to bit0.
                hx = hx.wrapping_add(if hx >= 0 { prnd1 } else { nrnd1 }) & mask1;
                hy = hy.wrapping_add(if hy >= 0 { prnd1 } else { nrnd1 }) & mask1;
                hc = hc.wrapping_add(if hc >= 0 { prnd0 } else { nrnd0 }) & mask0;
                // Pass the low bits of the detail coefficients on to the mean.
                let lowbit0 = hc & bit0;
                hx = if hx >= 0 { hx.wrapping_sub(lowbit0) } else { hx.wrapping_add(lowbit0) };
                hy = if hy >= 0 { hy.wrapping_sub(lowbit0) } else { hy.wrapping_add(lowbit0) };
                let lowbit1 = (hc ^ hx ^ hy) & bit1;
                let h0 = if h0 >= 0 {
                    h0.wrapping_add(lowbit0).wrapping_sub(lowbit1)
                } else {
                    h0.wrapping_add(if lowbit0 == 0 { lowbit1 } else { lowbit0.wrapping_sub(lowbit1) })
                };
                // Sums divided by 2 (by 4 on the last pass).
                a[s10 + 1] = h0.wrapping_add(hx).wrapping_add(hy).wrapping_add(hc) >> shift;
                a[s10] = h0.wrapping_add(hx).wrapping_sub(hy).wrapping_sub(hc) >> shift;
                a[s00 + 1] = h0.wrapping_sub(hx).wrapping_add(hy).wrapping_sub(hc) >> shift;
                a[s00] = h0.wrapping_sub(hx).wrapping_sub(hy).wrapping_add(hc) >> shift;
                s00 += 2;
                s10 += 2;
                j += 2;
            }
            if oddy == 1 {
                // The last element of an odd-length row: s00 + 1 and
                // s10 + 1 are past the edge.
                let h0 = a[s00];
                let mut hx = a[s10];
                hx = hx.wrapping_add(if hx >= 0 { prnd1 } else { nrnd1 }) & mask1;
                let lowbit1 = hx & bit1;
                let h0 = if h0 >= 0 { h0.wrapping_sub(lowbit1) } else { h0.wrapping_add(lowbit1) };
                a[s10] = h0.wrapping_add(hx) >> shift;
                a[s00] = h0.wrapping_sub(hx) >> shift;
            }
            i += 2;
        }
        if oddx == 1 {
            // The last row of an odd number of rows.
            let mut s00 = ny * i;
            let mut j = 0;
            while j < nytop - oddy {
                let h0 = a[s00];
                let mut hy = a[s00 + 1];
                hy = hy.wrapping_add(if hy >= 0 { prnd1 } else { nrnd1 }) & mask1;
                let lowbit1 = hy & bit1;
                let h0 = if h0 >= 0 { h0.wrapping_sub(lowbit1) } else { h0.wrapping_add(lowbit1) };
                a[s00 + 1] = h0.wrapping_add(hy) >> shift;
                a[s00] = h0.wrapping_sub(hy) >> shift;
                s00 += 2;
                j += 2;
            }
            if oddy == 1 {
                // The corner element.
                a[s00] >>= shift;
            }
        }
        // Halve the masks and roundings for the next, finer level.
        bit2 = bit1;
        bit1 = bit0;
        bit0 >>= 1;
        mask1 = mask0;
        mask0 >>= 1;
        prnd1 = prnd0;
        prnd0 >>= 1;
        nrnd1 = nrnd0;
        nrnd0 = prnd0 - 1;
    }
    let _ = bit2;
}

/// Smooths the coefficients of a coarse level by interpolating the detail
/// coefficients from their neighbours, within the error the scale factor
/// could have caused.  Arithmetic is in 64 bits so damaged data can't
/// overflow.
fn hsmooth(a: &mut [i32], nxtop: usize, nytop: usize, ny: usize, scale: i32) {
    let smax = (scale >> 1) as i64;
    if smax <= 0 {
        return;
    }
    let ny2 = ny << 1;
    let g = |a: &[i32], i: usize| a[i] as i64;
    // Division by 8 or 64 rounding toward zero.
    let down = |s: i64, bits: u32| if s >= 0 { s >> bits } else { (s + (1 << bits) - 1) >> bits };
    let put = |a: &mut [i32], i: usize, v: i64| a[i] = v.clamp(i32::MIN as i64, i32::MAX as i64) as i32;
    // The x difference.
    let mut i = 2;
    while i + 2 < nxtop {
        let mut s00 = ny * i;
        let mut s10 = s00 + ny;
        let mut j = 0;
        while j < nytop {
            let (hm, h0, hp) = (g(a, s00 - ny2), g(a, s00), g(a, s00 + ny2));
            let diff = hp - hm;
            let dmax = ((hp - h0).min(h0 - hm) * 4).max(0);
            let dmin = ((hp - h0).max(h0 - hm) * 4).min(0);
            if dmin < dmax {
                let diff = diff.min(dmax).max(dmin);
                let s = down(diff - (g(a, s10) << 3), 3).min(smax).max(-smax);
                put(a, s10, g(a, s10) + s);
            }
            s00 += 2;
            s10 += 2;
            j += 2;
        }
        i += 2;
    }
    // The y difference.
    let mut i = 0;
    while i < nxtop {
        let mut s00 = ny * i + 2;
        let mut j = 2;
        while j + 2 < nytop {
            let (hm, h0, hp) = (g(a, s00 - 2), g(a, s00), g(a, s00 + 2));
            let diff = hp - hm;
            let dmax = ((hp - h0).min(h0 - hm) * 4).max(0);
            let dmin = ((hp - h0).max(h0 - hm) * 4).min(0);
            if dmin < dmax {
                let diff = diff.min(dmax).max(dmin);
                let s = down(diff - (g(a, s00 + 1) << 3), 3).min(smax).max(-smax);
                put(a, s00 + 1, g(a, s00 + 1) + s);
            }
            s00 += 2;
            j += 2;
        }
        i += 2;
    }
    // The curvature difference.
    let mut i = 2;
    while i + 2 < nxtop {
        let mut s00 = ny * i + 2;
        let mut s10 = s00 + ny;
        let mut j = 2;
        while j + 2 < nytop {
            let hmm = g(a, s00 - ny2 - 2);
            let hpm = g(a, s00 + ny2 - 2);
            let hmp = g(a, s00 - ny2 + 2);
            let hpp = g(a, s00 + ny2 + 2);
            let h0 = g(a, s00);
            let diff = hpp + hmm - hmp - hpm;
            let hx2 = g(a, s10) << 1;
            let hy2 = g(a, s00 + 1) << 1;
            let m1 = ((hpp - h0).max(0) - hx2 - hy2).min((h0 - hpm).max(0) + hx2 - hy2);
            let m2 = ((h0 - hmp).max(0) - hx2 + hy2).min((hmm - h0).max(0) + hx2 + hy2);
            let dmax = m1.min(m2) * 16;
            let m1 = ((hpp - h0).min(0) - hx2 - hy2).max((h0 - hpm).min(0) + hx2 - hy2);
            let m2 = ((h0 - hmp).min(0) - hx2 + hy2).max((hmm - h0).min(0) + hx2 + hy2);
            let dmin = m1.max(m2) * 16;
            if dmin < dmax {
                let diff = diff.min(dmax).max(dmin);
                let s = down(diff - (g(a, s10 + 1) << 6), 6).min(smax).max(-smax);
                put(a, s10 + 1, g(a, s10 + 1) + s);
            }
            s00 += 2;
            s10 += 2;
            j += 2;
        }
        i += 2;
    }
}

//endregion

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn huffman_codes_are_a_prefix_code() {
        for (i, &(c, l)) in CODES.iter().enumerate() {
            for (j, &(c2, l2)) in CODES.iter().enumerate() {
                if i != j && l <= l2 {
                    assert_ne!(c, c2 >> (l2 - l), "{i} is a prefix of {j}");
                }
            }
        }
    }

    #[test]
    fn bit_reading() {
        let mut b = Bits { data: &[0b1010_0110, 0b1111_0000], pos: 0, buf: 0, count: 0 };
        assert_eq!(b.take(3).unwrap(), 0b101);
        assert_eq!(b.take(5).unwrap(), 0b00110);
        assert_eq!(b.nybble().unwrap(), 0b1111);
        assert_eq!(b.bit().unwrap(), 0);
        assert!(b.take(8).is_err());
    }

    #[test]
    fn damaged_streams_error() {
        assert!(decode(&[], 4, 4, 0, false).is_err());
        let mut junk = vec![0xdd, 0x99];
        junk.extend([0, 0, 0, 4, 0, 0, 0, 4, 0, 0, 0, 0]);
        junk.extend([0u8; 8]);
        junk.extend([3, 3, 3]);
        junk.extend([0xff; 40]);
        let _ = decode(&junk, 4, 4, 0, false);
        // A size that disagrees with the tile is refused.
        assert!(decode(&junk, 5, 4, 0, false).is_err());
        let mut state = 5u32;
        for _ in 0..300 {
            let mut v = junk.clone();
            for b in v.iter_mut().skip(2) {
                state = state.wrapping_mul(1664525).wrapping_add(1013904223);
                *b = (state >> 16) as u8;
            }
            v[2..6].copy_from_slice(&[0, 0, 0, 4]);
            v[6..10].copy_from_slice(&[0, 0, 0, 4]);
            let _ = decode(&v, 4, 4, 0, true);
        }
    }
}
