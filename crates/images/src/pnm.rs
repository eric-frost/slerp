//! Netpbm's PBM, PGM and PPM, in their ASCII (P1, P2, P3) and binary (P4,
//! P5, P6) forms: a magic number, the width and height and for grey and
//! colour the largest sample value (`maxval`), with `#` comments allowed
//! between the numbers, and then the samples.  Values are scaled from
//! `maxval` to the full range of 8 bits.  A grey picture with a `maxval`
//! above 255 is almost always data (a height map, a depth capture), so it
//! comes out as 16-bit data to be stretched, like 16-bit grey PNG; colour
//! with more than 8 bits is reduced to 8.  (PAM, `P7`, is read by the
//! texture crate.)

/// The most pixels a picture may have.
const MAX_PIXELS: u64 = 100 << 20;

/// A decoded picture.
pub enum Pixels {
    /// 8-bit RGBA.
    Rgba8(Vec<u8>),
    /// 16-bit grey as floats from 0 to 1.
    Grey16(Vec<f32>),
}

pub struct Pnm {
    /// 1 to 6, the digit of the magic number.
    pub variant: u8,
    pub width: u32,
    pub height: u32,
    pub maxval: u32,
    pub pixels: Pixels,
    /// Rows the file held fewer than its header promised.
    pub missing_rows: u32,
    pub comments: Vec<String>,
}

impl Pnm {
    pub fn name(&self) -> &'static str {
        match self.variant {
            1 | 4 => "PBM (black and white)",
            2 | 5 => "PGM (grey)",
            _ => "PPM (colour)",
        }
    }

    pub fn is_ascii(&self) -> bool {
        self.variant <= 3
    }
}

/// `P1` to `P6` followed by white space or a comment.
pub fn is_pnm(head: &[u8]) -> bool {
    head.len() >= 3
        && head[0] == b'P'
        && (b'1'..=b'6').contains(&head[1])
        && matches!(head[2], b' ' | b'\t' | b'\n' | b'\r' | b'#')
}

struct Reader<'a> {
    d: &'a [u8],
    at: usize,
    comments: Vec<String>,
}

impl<'a> Reader<'a> {
    /// Skips white space and comments.
    fn skip(&mut self) {
        while let Some(&c) = self.d.get(self.at) {
            if c == b'#' {
                let start = self.at + 1;
                while self.d.get(self.at).is_some_and(|&c| c != b'\n' && c != b'\r') {
                    self.at += 1;
                }
                if self.comments.len() < 4 {
                    let text = String::from_utf8_lossy(&self.d[start..self.at]).trim().to_string();
                    if !text.is_empty() {
                        self.comments.push(text.chars().take(200).collect());
                    }
                }
            } else if c.is_ascii_whitespace() {
                self.at += 1;
            } else {
                break;
            }
        }
    }

    /// The next unsigned decimal number.
    fn number(&mut self) -> Option<u64> {
        self.skip();
        let start = self.at;
        let mut v = 0u64;
        while let Some(&c) = self.d.get(self.at).filter(|c| c.is_ascii_digit()) {
            v = v.checked_mul(10)?.checked_add((c - b'0') as u64)?;
            self.at += 1;
            if v > u32::MAX as u64 {
                return None;
            }
        }
        (self.at > start).then_some(v)
    }
}

pub fn decode(d: &[u8]) -> Result<Pnm, String> {
    if !is_pnm(d) {
        return Err("this isn't a PBM, PGM or PPM file".into());
    }
    let variant = d[1] - b'0';
    let mut r = Reader { d, at: 2, comments: Vec::new() };
    let width = r.number().ok_or("the width is missing")? as u32;
    let height = r.number().ok_or("the height is missing")? as u32;
    let bilevel = matches!(variant, 1 | 4);
    let maxval = if bilevel { 1 } else { r.number().ok_or("the maximum value is missing")? as u32 };
    if width == 0 || height == 0 {
        return Err("the picture is empty".into());
    }
    if maxval == 0 || maxval > 65535 {
        return Err(format!("a maximum value of {maxval} isn't valid"));
    }
    if width as u64 * height as u64 > MAX_PIXELS {
        return Err(format!("{width} × {height} pixels is more than is opened"));
    }
    let channels = if matches!(variant, 3 | 6) { 3usize } else { 1 };
    let (w, h) = (width as usize, height as usize);
    let wide = maxval > 255;
    // 1. The samples, as integers, for as many rows as the file has.
    let samples: Vec<u16>;
    let rows_found;
    if variant >= 4 {
        // Exactly one white space character separates the header from the raster.
        let start = r.at + 1;
        let body = d.get(start..).unwrap_or(&[]);
        let row_bytes = if variant == 4 { w.div_ceil(8) } else { w * channels * if wide { 2 } else { 1 } };
        rows_found = (body.len() / row_bytes.max(1)).min(h);
        if rows_found == 0 {
            return Err("the file holds no pixels".into());
        }
        let mut s = Vec::with_capacity(rows_found * w * channels);
        for row in body.chunks_exact(row_bytes).take(rows_found) {
            match variant {
                4 => s.extend((0..w).map(|x| ((row[x / 8] >> (7 - x % 8)) & 1) as u16)),
                _ if wide => s.extend(row.chunks_exact(2).map(|c| u16::from_be_bytes([c[0], c[1]]))),
                _ => s.extend(row.iter().map(|&v| v as u16)),
            }
        }
        samples = s;
    } else {
        let want = w * h * channels;
        let mut s: Vec<u16> = Vec::with_capacity(want.min(d.len()));
        if variant == 1 {
            // Black and white pixels may run together: "0110".
            r.skip();
            while s.len() < want {
                match d.get(r.at) {
                    Some(b'0') => s.push(0),
                    Some(b'1') => s.push(1),
                    Some(_) => {}
                    None => break,
                }
                r.at += 1;
                r.skip();
            }
        } else {
            while s.len() < want {
                match r.number() {
                    Some(v) => s.push(v.min(maxval as u64) as u16),
                    None => break,
                }
            }
        }
        rows_found = s.len() / (w * channels);
        if rows_found == 0 {
            return Err("the file holds no pixels".into());
        }
        s.truncate(rows_found * w * channels);
        samples = s;
    }
    // 2. Scale them.  A truncated picture shows its top rows.
    let shown_h = rows_found;
    let pixels = if channels == 1 && wide {
        Pixels::Grey16(samples.iter().map(|&v| v as f32 / maxval as f32).collect())
    } else {
        let scale = |v: u16| -> u8 { ((v as u32 * 255 + maxval / 2) / maxval).min(255) as u8 };
        let mut out = Vec::with_capacity(shown_h * w * 4);
        if channels == 3 {
            for p in samples.chunks_exact(3) {
                out.extend([scale(p[0]), scale(p[1]), scale(p[2]), 255]);
            }
        } else {
            for &v in &samples {
                // In a bitmap 1 is black.
                let g = if bilevel { 255 - scale(v) } else { scale(v) };
                out.extend([g, g, g, 255]);
            }
        }
        Pixels::Rgba8(out)
    };
    Ok(Pnm {
        variant,
        width,
        height: shown_h as u32,
        maxval,
        pixels,
        missing_rows: (h - shown_h) as u32,
        comments: r.comments,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rgba(p: &Pnm) -> &[u8] {
        match &p.pixels {
            Pixels::Rgba8(v) => v,
            _ => panic!("not 8-bit"),
        }
    }

    #[test]
    fn ascii_bitmap_with_comments_and_packed_digits() {
        let p = decode(b"P1\n# a comment\n4 2\n0110\n1 0 0 1\n").unwrap();
        assert_eq!((p.width, p.height, p.variant), (4, 2, 1));
        assert_eq!(p.comments, ["a comment"]);
        let grey: Vec<u8> = rgba(&p).chunks_exact(4).map(|c| c[0]).collect();
        // 1 is black.
        assert_eq!(grey, [255, 0, 0, 255, 0, 255, 255, 0]);
    }

    #[test]
    fn binary_bitmap_pads_rows_to_bytes() {
        let p = decode(b"P4 10 2\n\xff\xc0\x00\x40").unwrap();
        let grey: Vec<u8> = rgba(&p).chunks_exact(4).map(|c| c[0]).collect();
        assert_eq!(&grey[..10], &[0; 10]);
        assert_eq!(&grey[10..20], &[255, 255, 255, 255, 255, 255, 255, 255, 255, 0]);
    }

    #[test]
    fn grey_and_colour_are_scaled_to_maxval() {
        let p = decode(b"P2\n3 1\n10\n0 5 10").unwrap();
        let g: Vec<u8> = rgba(&p).chunks_exact(4).map(|c| c[0]).collect();
        assert_eq!(g, [0, 128, 255]);
        let p = decode(b"P3\n1 2\n255\n255 0 0\n0 0 255\n").unwrap();
        assert_eq!(rgba(&p), [255, 0, 0, 255, 0, 0, 255, 255]);
        let p = decode(b"P6\n2 1\n255\n\x01\x02\x03\x04\x05\x06").unwrap();
        assert_eq!(rgba(&p), [1, 2, 3, 255, 4, 5, 6, 255]);
        let p = decode(b"P5 2 1 255\n\x10\x20").unwrap();
        assert_eq!(rgba(&p)[0], 0x10);
    }

    #[test]
    fn sixteen_bit_grey_is_data_and_sixteen_bit_colour_is_reduced() {
        let p = decode(b"P5\n2 1\n65535\n\x00\x00\xff\xff").unwrap();
        let Pixels::Grey16(v) = &p.pixels else { panic!() };
        assert_eq!(v, &[0.0, 1.0]);
        // A maxval between 256 and 65535 uses two bytes per sample.
        let p = decode(b"P5\n1 1\n1000\n\x01\xf4").unwrap();
        let Pixels::Grey16(v) = &p.pixels else { panic!() };
        assert!((v[0] - 0.5).abs() < 1e-3);
        let p = decode(b"P6\n1 1\n65535\n\xff\xff\x80\x00\x00\x00").unwrap();
        assert_eq!(rgba(&p), [255, 128, 0, 255]);
    }

    #[test]
    fn a_short_picture_shows_the_rows_it_has() {
        let p = decode(b"P6\n2 3\n255\n\x01\x02\x03\x04\x05\x06\x07\x08\x09").unwrap();
        assert_eq!((p.height, p.missing_rows), (1, 2));
        let p = decode(b"P2\n2 3\n9\n1 2 3 4 5").unwrap();
        assert_eq!((p.height, p.missing_rows), (2, 1));
        assert!(decode(b"P6\n2 3\n255\n\x01").is_err());
        assert!(decode(b"P2\n2 3\n9\n").is_err());
    }

    #[test]
    fn nonsense_is_refused_without_allocating() {
        assert!(decode(b"P6\n100000 100000\n255\nabc").is_err());
        assert!(decode(b"P6\n0 5\n255\n").is_err());
        assert!(decode(b"P6\n5 5\n0\nabc").is_err());
        assert!(decode(b"P6\n5 5\n70000\nabc").is_err());
        assert!(decode(b"P6\n99999999999999999999 5\n255\n").is_err());
        assert!(decode(b"P7\n").is_err());
        assert!(decode(b"").is_err());
        assert!(!is_pnm(b"P7\n"));
        assert!(!is_pnm(b"P6"));
        assert!(is_pnm(b"P4#c"));
        // Cuts and flips of a real one never panic.
        let good = b"P6\n# c\n3 2\n255\n\x01\x02\x03\x04\x05\x06\x07\x08\x09\x0a\x0b\x0c\x0d\x0e\x0f\x10\x11\x12";
        for cut in 0..good.len() {
            let _ = decode(&good[..cut]);
        }
        for at in 0..good.len() {
            let mut bad = good.to_vec();
            bad[at] ^= 0xff;
            let _ = decode(&bad);
        }
    }
}
