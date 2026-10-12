//! DICOM RLE Lossless (PS3.5 annex G): a 64-byte header with up to 15
//! segment offsets, and each segment a PackBits run of one byte plane.
//! Planes go sample by sample, most significant byte first, so a 16-bit
//! greyscale frame has two segments and 8-bit RGB three.

use super::{Decoded, Samples};

/// Decodes one frame of `rows` x `columns` with `samples` samples of
/// `bytes_per_sample` bytes each.
pub fn decode(data: &[u8], rows: u32, columns: u32, samples: u32, bytes_per_sample: u32) -> Result<Decoded, String> {
    if data.len() < 64 {
        return Err("RLE frame shorter than its header".into());
    }
    let le = |at: usize| u32::from_le_bytes([data[at], data[at + 1], data[at + 2], data[at + 3]]) as usize;
    let count = le(0);
    let planes = (samples * bytes_per_sample) as usize;
    if count == 0 || count > 15 || count < planes {
        return Err(format!("RLE frame has {count} segments, {planes} expected"));
    }
    let pixels = rows as usize * columns as usize;
    let mut out_planes: Vec<Vec<u8>> = Vec::with_capacity(planes);
    for s in 0..planes {
        let start = le(4 + 4 * s);
        let end = if s + 1 < count { le(8 + 4 * s) } else { data.len() };
        if start < 64 || start > end || end > data.len() {
            return Err(format!("RLE segment {s} lies outside the frame"));
        }
        out_planes.push(unpack(&data[start..end], pixels));
    }
    // Interleave: sample by sample, bytes most significant first.
    let n = pixels * samples as usize;
    let samples_out = if bytes_per_sample == 1 {
        let mut v = vec![0u8; n];
        for (s, plane) in out_planes.iter().enumerate() {
            for (i, &b) in plane.iter().enumerate() {
                v[i * samples as usize + s] = b;
            }
        }
        Samples::U8(v)
    } else {
        // 16 and 32-bit samples; 32-bit keeps the top 16 bits' worth
        // through `U32`.
        let mut v = vec![0u32; n];
        for s in 0..samples as usize {
            for byte in 0..bytes_per_sample as usize {
                let plane = &out_planes[s * bytes_per_sample as usize + byte];
                let shift = 8 * (bytes_per_sample as usize - 1 - byte);
                for (i, &b) in plane.iter().enumerate() {
                    v[i * samples as usize + s] |= (b as u32) << shift;
                }
            }
        }
        Samples::U32(v)
    };
    Ok(Decoded {
        width: columns,
        height: rows,
        comps: samples,
        precision: 8 * bytes_per_sample,
        data: samples_out,
        rgb: false,
    })
}

/// Unpacks one PackBits segment to exactly `len` bytes; a short segment is
/// padded with zeros and a long one cut.
pub fn unpack(src: &[u8], len: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(len);
    let mut i = 0;
    while i < src.len() && out.len() < len {
        let n = src[i] as i8;
        i += 1;
        if n >= 0 {
            let count = (n as usize + 1).min(src.len() - i).min(len - out.len());
            out.extend_from_slice(&src[i..i + count]);
            i += n as usize + 1;
        } else if n != -128 {
            let Some(&b) = src.get(i) else { break };
            i += 1;
            let count = (1 - n as isize) as usize;
            let count = count.min(len - out.len());
            out.resize(out.len() + count, b);
        }
    }
    out.resize(len, 0);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packbits() {
        // A literal run of 3, a repeat of 4, the no-op, a literal of 1.
        let src = [2, 1, 2, 3, (-3i8) as u8, 9, 0x80, 0, 7];
        assert_eq!(unpack(&src, 9), vec![1, 2, 3, 9, 9, 9, 9, 7, 0]);
        // Runs that claim more than there is stop cleanly.
        assert_eq!(unpack(&[100, 1, 2], 4), vec![1, 2, 0, 0]);
        assert_eq!(unpack(&[(-100i8) as u8], 2), vec![0, 0]);
    }

    #[test]
    fn frame_with_two_planes() {
        // One 16-bit pixel row of 2: values 0x0102 and 0x0304.
        let mut f = vec![0u8; 64];
        f[0] = 2;
        f[4] = 64;
        f[8] = 67;
        f.extend([1, 1, 3]); // high bytes: literal of 2
        f.extend([1, 2, 4]); // low bytes
        let d = decode(&f, 1, 2, 1, 2).unwrap();
        assert!(matches!(d.data, Samples::U32(ref v) if v == &[0x0102, 0x0304]));
        assert!(decode(&f[..60], 1, 2, 1, 2).is_err());
        let mut bad = f.clone();
        bad[0] = 16;
        assert!(decode(&bad, 1, 2, 1, 2).is_err());
    }
}
