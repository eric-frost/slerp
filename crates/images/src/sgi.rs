//! SGI images (`.sgi`, `.rgb`, `.rgba`, `.bw`, `.int`, `.inta`), Silicon
//! Graphics' format: a 512-byte big-endian header, then each channel's rows
//! bottom first, stored as they are or run-length coded with a table of
//! where each row starts.  One to four channels (grey, grey and alpha,
//! RGB, RGBA) of 8 or 16 bits.

pub struct Sgi {
    pub width: u32,
    pub height: u32,
    pub channels: u16,
    pub bytes_per_sample: u8,
    pub rle: bool,
    pub name: String,
    pub rgba: Vec<u8>,
    pub warnings: Vec<String>,
}

const MAGIC: u16 = 474;

/// The header's fields must make sense together: the two magic bytes alone
/// are too weak to claim a file.
pub fn is_sgi(head: &[u8]) -> bool {
    if head.len() < 12 {
        return false;
    }
    let be = |at: usize| u16::from_be_bytes([head[at], head[at + 1]]);
    be(0) == MAGIC
        && head[2] <= 1
        && matches!(head[3], 1 | 2)
        && matches!(be(4), 1..=3)
        && be(6) > 0
        && (be(4) == 1 || be(8) > 0)
        && (be(4) < 3 || be(10) > 0)
}

pub fn decode(d: &[u8], max_pixels: u64) -> Result<Sgi, String> {
    if d.len() < 512 || !is_sgi(d) {
        return Err("not an SGI image".into());
    }
    let be16 = |at: usize| u16::from_be_bytes([d[at], d[at + 1]]) as usize;
    let rle = d[2] == 1;
    let bpc = d[3] as usize;
    let dimension = be16(4);
    let width = be16(6);
    let height = if dimension == 1 { 1 } else { be16(8) };
    let zsize = if dimension < 3 { 1 } else { be16(10) };
    let colormap = u32::from_be_bytes([d[104], d[105], d[106], d[107]]);
    if colormap == 3 {
        return Err("this SGI file holds only a colour map, no picture".into());
    }
    if width as u64 * height as u64 > max_pixels {
        return Err(format!("a picture of {width} × {height} pixels is too large"));
    }
    let name: String = d[24..104].iter().take_while(|&&b| b != 0).map(|&b| b as char).collect();
    let mut warnings = Vec::new();
    if matches!(colormap, 1 | 2) {
        warnings.push(
            "the file says its pixels are dithered or screen colours, an obsolete layout read as ordinary".into(),
        );
    }
    let channels = zsize.min(4);
    if zsize > 4 {
        warnings.push(format!("{zsize} channels; the first four are shown"));
    }
    // Planes of samples scaled to 8 bits, rows top first.
    let mut planes = vec![vec![0u8; width * height]; channels];
    let mut missing = 0usize;
    for (z, plane) in planes.iter_mut().enumerate() {
        for row in 0..height {
            let out = &mut plane[(height - 1 - row) * width..(height - row) * width];
            let ok = if rle {
                // One table entry per row and channel: the rows' offsets,
                // then their lengths.
                let entry = (z * height + row) * 4;
                rle_row(d, 512 + entry, 512 + height * zsize * 4 + entry, width, bpc, out)
            } else {
                raw_row(d, z, row, height, width, bpc, out)
            };
            if !ok {
                missing += 1;
            }
        }
    }
    if missing > 0 {
        warnings.push(format!("{missing} rows couldn't be read (the file is damaged or cut short)"));
    }
    let mut rgba = vec![0u8; width * height * 4];
    for (i, px) in rgba.chunks_exact_mut(4).enumerate() {
        let s = |c: usize| planes[c][i];
        let v = match channels {
            1 => [s(0), s(0), s(0), 255],
            2 => [s(0), s(0), s(0), s(1)],
            3 => [s(0), s(1), s(2), 255],
            _ => [s(0), s(1), s(2), s(3)],
        };
        px.copy_from_slice(&v);
    }
    Ok(Sgi {
        width: width as u32,
        height: height as u32,
        channels: zsize as u16,
        bytes_per_sample: bpc as u8,
        rle,
        name,
        rgba,
        warnings,
    })
}

/// 16-bit samples become 8-bit by their high byte, rounded.
fn to8(d: &[u8], at: usize, bpc: usize) -> Option<u8> {
    if bpc == 1 {
        d.get(at).copied()
    } else {
        let v = u16::from_be_bytes([*d.get(at)?, *d.get(at + 1)?]) as u32;
        Some(((v * 255 + 32767) / 65535) as u8)
    }
}

fn raw_row(d: &[u8], z: usize, row: usize, height: usize, width: usize, bpc: usize, out: &mut [u8]) -> bool {
    let start = 512 + ((z * height + row) * width) * bpc;
    for (x, o) in out.iter_mut().enumerate() {
        match to8(d, start + x * bpc, bpc) {
            Some(v) => *o = v,
            None => return false,
        }
    }
    true
}

/// One run-length coded row: a count byte (or 16-bit word) whose low seven
/// bits say how many, with the top bit set for that many literal samples
/// and clear for one sample repeated; a zero count ends the row.
fn rle_row(d: &[u8], start_at: usize, length_at: usize, width: usize, bpc: usize, out: &mut [u8]) -> bool {
    let be32 = |at: usize| d.get(at..at + 4).map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]) as usize);
    let (Some(start), Some(len)) = (be32(start_at), be32(length_at)) else { return false };
    let Some(end) = start.checked_add(len) else { return false };
    let Some(src) = d.get(start..end.min(d.len())) else { return false };
    let mut i = 0;
    let mut x = 0;
    while x < width {
        let Some(head) = to_count(src, i, bpc) else { return false };
        i += bpc;
        let count = (head & 0x7f) as usize;
        if count == 0 {
            break;
        }
        if head & 0x80 != 0 {
            for _ in 0..count {
                let Some(v) = to8(src, i, bpc) else { return false };
                i += bpc;
                if x < width {
                    out[x] = v;
                    x += 1;
                }
            }
        } else {
            let Some(v) = to8(src, i, bpc) else { return false };
            i += bpc;
            for _ in 0..count {
                if x < width {
                    out[x] = v;
                    x += 1;
                }
            }
        }
    }
    x == width
}

fn to_count(d: &[u8], at: usize, bpc: usize) -> Option<u16> {
    if bpc == 1 { d.get(at).map(|&b| b as u16) } else { Some(u16::from_be_bytes([*d.get(at)?, *d.get(at + 1)?])) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header(rle: bool, bpc: u8, dim: u16, w: u16, h: u16, z: u16) -> Vec<u8> {
        let mut d = vec![0u8; 512];
        d[0..2].copy_from_slice(&MAGIC.to_be_bytes());
        d[2] = rle as u8;
        d[3] = bpc;
        d[4..6].copy_from_slice(&dim.to_be_bytes());
        d[6..8].copy_from_slice(&w.to_be_bytes());
        d[8..10].copy_from_slice(&h.to_be_bytes());
        d[10..12].copy_from_slice(&z.to_be_bytes());
        d[24..28].copy_from_slice(b"test");
        d
    }

    /// Planes of 8-bit samples, bottom row first, written verbatim.
    fn verbatim(w: u16, h: u16, z: u16, f: impl Fn(usize, usize, usize) -> u8) -> Vec<u8> {
        let mut d = header(false, 1, 3, w, h, z);
        for c in 0..z as usize {
            for row in 0..h as usize {
                for x in 0..w as usize {
                    d.push(f(c, x, row));
                }
            }
        }
        d
    }

    /// The same, run-length coded with literal runs only, except rows of one
    /// value, which repeat.
    fn rle(w: u16, h: u16, z: u16, f: impl Fn(usize, usize, usize) -> u8) -> Vec<u8> {
        let mut rows = Vec::new();
        for c in 0..z as usize {
            for row in 0..h as usize {
                let vals: Vec<u8> = (0..w as usize).map(|x| f(c, x, row)).collect();
                let mut r = Vec::new();
                if vals.iter().all(|&v| v == vals[0]) {
                    for chunk in vals.chunks(127) {
                        r.extend_from_slice(&[chunk.len() as u8, chunk[0]]);
                    }
                } else {
                    for chunk in vals.chunks(127) {
                        r.push(0x80 | chunk.len() as u8);
                        r.extend_from_slice(chunk);
                    }
                }
                r.push(0);
                rows.push(r);
            }
        }
        let mut d = header(true, 1, 3, w, h, z);
        let n = rows.len();
        let mut offset = 512 + n * 8;
        let (mut starts, mut lens) = (Vec::new(), Vec::new());
        for r in &rows {
            starts.extend_from_slice(&(offset as u32).to_be_bytes());
            lens.extend_from_slice(&(r.len() as u32).to_be_bytes());
            offset += r.len();
        }
        d.extend(starts);
        d.extend(lens);
        rows.iter().for_each(|r| d.extend_from_slice(r));
        d
    }

    fn sample(c: usize, x: usize, row: usize) -> u8 {
        if row == 1 { 77 } else { (c * 60 + x * 3 + row * 11) as u8 }
    }

    #[test]
    fn verbatim_and_rle_agree() {
        for z in 1..=4u16 {
            let a = decode(&verbatim(150, 4, z, sample), u64::MAX).unwrap();
            let b = decode(&rle(150, 4, z, sample), u64::MAX).unwrap();
            assert!(a.warnings.is_empty() && b.warnings.is_empty(), "{:?}", b.warnings);
            assert_eq!(a.rgba, b.rgba, "{z} channels");
            assert_eq!(a.name, "test");
            // The bottom row is stored first.
            let top_left = &a.rgba[0..4];
            let want = sample(0, 0, 3);
            assert_eq!(top_left[0], want);
            match z {
                1 => assert_eq!(top_left, [want, want, want, 255]),
                2 => assert_eq!(top_left[3], sample(1, 0, 3)),
                3 => assert_eq!(top_left, [want, sample(1, 0, 3), sample(2, 0, 3), 255]),
                _ => assert_eq!(top_left[3], sample(3, 0, 3)),
            }
        }
    }

    #[test]
    fn sixteen_bit_samples_scale_down() {
        let mut d = header(false, 2, 2, 2, 1, 1);
        d.extend_from_slice(&[0xff, 0xff, 0x80, 0x00]);
        let s = decode(&d, u64::MAX).unwrap();
        assert_eq!(&s.rgba[..], &[255, 255, 255, 255, 128, 128, 128, 255]);
    }

    #[test]
    fn damaged_files_are_bounded() {
        let good = rle(64, 32, 4, sample);
        for cut in (0..good.len()).step_by(7) {
            if let Ok(s) = decode(&good[..cut], u64::MAX) {
                assert_eq!(s.rgba.len(), 64 * 32 * 4);
            }
        }
        let mut bad = good.clone();
        // Offsets pointing past the end, lengths of 4 GB.
        bad[512..516].copy_from_slice(&u32::MAX.to_be_bytes());
        bad[512 + 32 * 4 * 4..512 + 32 * 4 * 4 + 4].copy_from_slice(&u32::MAX.to_be_bytes());
        let s = decode(&bad, u64::MAX).unwrap();
        assert!(!s.warnings.is_empty());
        assert!(decode(&header(false, 1, 3, 65535, 65535, 4), 1 << 26).is_err());
        assert!(!is_sgi(&[1, 0xda, 2, 1, 0, 3, 0, 1, 0, 1, 0, 1]));
    }
}
