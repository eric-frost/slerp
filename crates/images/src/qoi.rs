//! QOI, the "Quite OK Image" format (qoiformat.org): a 14-byte header and
//! a stream of one- to five-byte chunks, each giving the next pixel (or a
//! run of them) as an index into a 64-entry table of recent colours, a
//! small difference from the previous pixel, or literal values.

/// The decoded picture: 8-bit RGBA.
pub struct Qoi {
    pub width: u32,
    pub height: u32,
    pub channels: u8,
    /// The header's colour space: sRGB with linear alpha, or all linear.
    pub linear: bool,
    pub rgba: Vec<u8>,
    /// Pixels the data ran out before.
    pub missing: u64,
}

/// The reference decoder refuses pictures above 400 million pixels.
const MAX_PIXELS: u64 = 400_000_000;

pub fn is_qoi(head: &[u8]) -> bool {
    head.len() >= 14 && head.starts_with(b"qoif") && matches!(head[12], 3 | 4) && head[13] <= 1
}

pub fn decode(d: &[u8], max_pixels: u64) -> Result<Qoi, String> {
    if !is_qoi(d) {
        return Err("not a QOI file".into());
    }
    let be = |at: usize| u32::from_be_bytes([d[at], d[at + 1], d[at + 2], d[at + 3]]);
    let (width, height) = (be(4), be(8));
    let pixels = width as u64 * height as u64;
    if width == 0 || height == 0 || pixels > MAX_PIXELS.min(max_pixels) {
        return Err(format!("a picture of {width} × {height} pixels is not allowed"));
    }
    // A chunk of one byte gives at most 62 pixels, so a short file can't
    // claim a huge picture: anything past that is a decompression bomb.
    let data = &d[14..];
    if pixels > (data.len() as u64 + 1) * 62 {
        return Err(format!("{width} × {height} pixels can't come from {} bytes", d.len()));
    }
    let mut rgba = Vec::with_capacity(pixels as usize * 4);
    let mut table = [[0u8; 4]; 64];
    let mut px = [0u8, 0, 0, 255];
    let mut i = 0;
    let hash = |p: [u8; 4]| (p[0] as usize * 3 + p[1] as usize * 5 + p[2] as usize * 7 + p[3] as usize * 11) % 64;
    while (rgba.len() / 4) < pixels as usize {
        let Some(&b) = data.get(i) else { break };
        i += 1;
        let mut run = 1usize;
        match b {
            0xfe => {
                let Some(v) = data.get(i..i + 3) else { break };
                (px[0], px[1], px[2]) = (v[0], v[1], v[2]);
                i += 3;
            }
            0xff => {
                let Some(v) = data.get(i..i + 4) else { break };
                px = [v[0], v[1], v[2], v[3]];
                i += 4;
            }
            _ => match b >> 6 {
                0 => px = table[(b & 63) as usize],
                1 => {
                    px[0] = px[0].wrapping_add(((b >> 4) & 3).wrapping_sub(2));
                    px[1] = px[1].wrapping_add(((b >> 2) & 3).wrapping_sub(2));
                    px[2] = px[2].wrapping_add((b & 3).wrapping_sub(2));
                }
                2 => {
                    let Some(&n) = data.get(i) else { break };
                    i += 1;
                    let dg = (b & 63).wrapping_sub(32);
                    px[1] = px[1].wrapping_add(dg);
                    px[0] = px[0].wrapping_add(dg.wrapping_add((n >> 4).wrapping_sub(8)));
                    px[2] = px[2].wrapping_add(dg.wrapping_add((n & 15).wrapping_sub(8)));
                }
                _ => run = (b & 63) as usize + 1,
            },
        }
        table[hash(px)] = px;
        let room = pixels as usize - rgba.len() / 4;
        for _ in 0..run.min(room) {
            rgba.extend_from_slice(&px);
        }
    }
    let missing = pixels - (rgba.len() / 4) as u64;
    rgba.resize(pixels as usize * 4, 0);
    if d[12] == 3 {
        rgba.chunks_exact_mut(4).for_each(|p| p[3] = 255);
    }
    Ok(Qoi { width, height, channels: d[12], linear: d[13] == 1, rgba, missing })
}

#[cfg(test)]
pub mod tests {
    use super::*;

    /// A minimal encoder, enough to make test files with every chunk kind.
    pub fn encode(w: u32, h: u32, channels: u8, rgba: &[u8]) -> Vec<u8> {
        let mut out = b"qoif".to_vec();
        out.extend_from_slice(&w.to_be_bytes());
        out.extend_from_slice(&h.to_be_bytes());
        out.extend_from_slice(&[channels, 0]);
        let mut table = [[0u8; 4]; 64];
        let mut prev = [0u8, 0, 0, 255];
        let mut run = 0u8;
        let px: Vec<[u8; 4]> = rgba.chunks_exact(4).map(|p| [p[0], p[1], p[2], p[3]]).collect();
        for (n, &p) in px.iter().enumerate() {
            if p == prev {
                run += 1;
                if run == 62 || n + 1 == px.len() {
                    out.push(0xc0 | (run - 1));
                    run = 0;
                }
                continue;
            }
            if run > 0 {
                out.push(0xc0 | (run - 1));
                run = 0;
            }
            let h = (p[0] as usize * 3 + p[1] as usize * 5 + p[2] as usize * 7 + p[3] as usize * 11) % 64;
            if table[h] == p {
                out.push(h as u8);
            } else {
                table[h] = p;
                let d = |a: u8, b: u8| a.wrapping_sub(b) as i8 as i32;
                let (dr, dg, db) = (d(p[0], prev[0]), d(p[1], prev[1]), d(p[2], prev[2]));
                if p[3] != prev[3] {
                    out.extend_from_slice(&[0xff, p[0], p[1], p[2], p[3]]);
                } else if (-2..2).contains(&dr) && (-2..2).contains(&dg) && (-2..2).contains(&db) {
                    out.push(0x40 | ((dr + 2) << 4 | (dg + 2) << 2 | (db + 2)) as u8);
                } else if (-32..32).contains(&dg) && (-8..8).contains(&(dr - dg)) && (-8..8).contains(&(db - dg)) {
                    out.push(0x80 | (dg + 32) as u8);
                    out.push(((dr - dg + 8) << 4 | (db - dg + 8)) as u8);
                } else {
                    out.extend_from_slice(&[0xfe, p[0], p[1], p[2]]);
                }
            }
            prev = p;
        }
        out.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0, 1]);
        out
    }

    fn picture(w: u32, h: u32) -> Vec<u8> {
        (0..w * h)
            .flat_map(|i| {
                let (x, y) = (i % w, i / w);
                if x < 5 {
                    [10, 20, 30, 255]
                } else {
                    [(x * 7) as u8, (y * 3 + x) as u8, (x ^ y) as u8, if y % 3 == 0 { 128 } else { 255 }]
                }
            })
            .collect()
    }

    #[test]
    fn round_trips_every_chunk_kind() {
        let px = picture(70, 9);
        let file = encode(70, 9, 4, &px);
        let q = decode(&file, u64::MAX).unwrap();
        assert_eq!((q.width, q.height, q.channels, q.missing), (70, 9, 4, 0));
        assert_eq!(q.rgba, px);
        // Three channels force alpha opaque.
        let q = decode(&encode(70, 9, 3, &px), u64::MAX).unwrap();
        assert!(q.rgba.chunks_exact(4).all(|p| p[3] == 255));
    }

    #[test]
    fn damaged_files_are_bounded() {
        let px = picture(40, 40);
        let file = encode(40, 40, 4, &px);
        for cut in 0..file.len() {
            if let Ok(q) = decode(&file[..cut], u64::MAX) {
                assert_eq!(q.rgba.len(), 40 * 40 * 4);
            }
        }
        // A tiny file claiming 20000 × 20000 pixels is refused at once.
        let mut bomb = encode(1, 1, 4, &[1, 2, 3, 4]);
        bomb[4..8].copy_from_slice(&20000u32.to_be_bytes());
        bomb[8..12].copy_from_slice(&20000u32.to_be_bytes());
        assert!(decode(&bomb, u64::MAX).is_err());
        assert!(!is_qoi(b"qoif\0\0\0\x01\0\0\0\x01\x05\0"));
    }
}
