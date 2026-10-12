//! Radiance HDR (`.hdr`, RGBE): a text header, then scanlines that are
//! flat, old-style run-length encoded, or (usually) new-style RLE with each
//! channel run-length encoded separately.
//!
//! Sky panoramas reach 16K by 8K, which is 2 GiB as floats, so the decoder
//! reads a file in chunks and can shrink the image by a whole factor while
//! the scanlines arrive: an 8K sky becomes a thumbnail without ever being
//! held whole.

use crate::resize::reduce_factor;
use crate::util::{MAX_DIMENSION, check_size, invalid, unsupported};
use crate::{DecodeOptions, Pixels, Texture, TextureError};
use rayon::prelude::*;
use std::io::Read;

/// Bytes read from a file at a time.
const CHUNK: usize = 4 << 20;

/// A window onto the data: the whole of an in-memory file, or a refillable
/// buffer over a file being read.
struct Feed<'a> {
    reader: Option<&'a mut dyn Read>,
    owned: Vec<u8>,
    slice: &'a [u8],
    pos: usize,
    eof: bool,
}

impl<'a> Feed<'a> {
    fn memory(data: &'a [u8]) -> Self {
        Feed { reader: None, owned: Vec::new(), slice: data, pos: 0, eof: true }
    }

    fn reader(r: &'a mut dyn Read) -> Self {
        Feed { reader: Some(r), owned: Vec::new(), slice: &[], pos: 0, eof: false }
    }

    fn data(&self) -> &[u8] {
        if self.reader.is_some() { &self.owned[self.pos..] } else { &self.slice[self.pos..] }
    }

    /// Makes at least `n` bytes available unless the file ends first.
    fn ensure(&mut self, n: usize) -> Result<(), TextureError> {
        let Some(r) = self.reader.as_mut() else { return Ok(()) };
        if self.owned.len() - self.pos >= n || self.eof {
            return Ok(());
        }
        self.owned.drain(..self.pos);
        self.pos = 0;
        while self.owned.len() < n.max(CHUNK) && !self.eof {
            let start = self.owned.len();
            self.owned.resize(start + CHUNK, 0);
            let got = r.read(&mut self.owned[start..])?;
            self.owned.truncate(start + got);
            self.eof = got == 0;
        }
        Ok(())
    }

    fn consume(&mut self, n: usize) {
        self.pos += n;
    }
}

/// Where rows go and which way they run.
struct Header {
    width: u32,
    height: u32,
    flip_y: bool,
    flip_x: bool,
    xyz: bool,
}

fn parse_header(feed: &mut Feed) -> Result<Header, TextureError> {
    feed.ensure(64 << 10)?;
    let data = feed.data();
    let mut xyz = false;
    let mut at = 0;
    // 1. Header lines, up to an empty line.
    loop {
        let end = data[at..].iter().position(|&b| b == b'\n').ok_or_else(|| invalid("HDR: no end of header"))?;
        let line = &data[at..at + end];
        at += end + 1;
        if line.is_empty() || line == b"\r" {
            break;
        }
        if let Some(f) = line.strip_prefix(b"FORMAT=") {
            match f.trim_ascii() {
                b"32-bit_rle_rgbe" => {}
                b"32-bit_rle_xyze" => xyz = true,
                other => return Err(unsupported(format!("HDR format {}", String::from_utf8_lossy(other)))),
            }
        }
    }
    // 2. The resolution line: "-Y 1024 +X 2048" for ordinary images.
    let end = data[at..].iter().position(|&b| b == b'\n').ok_or_else(|| invalid("HDR: no resolution"))?;
    let line = String::from_utf8_lossy(&data[at..at + end]).into_owned();
    at += end + 1;
    let parts: Vec<&str> = line.split_whitespace().collect();
    let [ya, h, xa, w] = parts.as_slice() else { return Err(invalid(format!("HDR: bad resolution {line:?}"))) };
    if !matches!(*ya, "-Y" | "+Y") || !matches!(*xa, "+X" | "-X") {
        return Err(unsupported(format!("HDR orientation {line:?}")));
    }
    let width: u32 = w.parse().map_err(|_| invalid("HDR: bad width"))?;
    let height: u32 = h.parse().map_err(|_| invalid("HDR: bad height"))?;
    if width == 0 || height == 0 || width > MAX_DIMENSION || height > MAX_DIMENSION {
        return Err(invalid(format!("HDR: size {width}x{height}")));
    }
    feed.consume(at);
    Ok(Header { width, height, flip_y: *ya == "+Y", flip_x: *xa == "-X", xyz })
}

/// Decodes one scanline into RGBE quadruples, returning the bytes it used.
fn scanline(data: &[u8], width: usize, out: &mut [u8], planes: &mut [u8]) -> Result<usize, TextureError> {
    let short = || invalid("HDR: truncated scanline");
    let new_rle = (8..0x8000).contains(&width)
        && data.len() >= 4
        && data[0] == 2
        && data[1] == 2
        && data[2] & 0x80 == 0
        && ((data[2] as usize) << 8 | data[3] as usize) == width;
    if new_rle {
        // Each of R, G, B and E is run-length encoded on its own.
        let mut at = 4;
        for plane in planes.chunks_exact_mut(width) {
            let mut x = 0;
            while x < width {
                let count = *data.get(at).ok_or_else(short)? as usize;
                at += 1;
                if count > 128 {
                    let n = count - 128;
                    let v = *data.get(at).ok_or_else(short)?;
                    at += 1;
                    plane.get_mut(x..x + n).ok_or_else(|| invalid("HDR: run overflows scanline"))?.fill(v);
                    x += n;
                } else {
                    let n = count;
                    if n == 0 {
                        return Err(invalid("HDR: empty run"));
                    }
                    let src = data.get(at..at + n).ok_or_else(short)?;
                    plane.get_mut(x..x + n).ok_or_else(|| invalid("HDR: run overflows scanline"))?.copy_from_slice(src);
                    at += n;
                    x += n;
                }
            }
        }
        let (r, rest) = planes.split_at(width);
        let (g, rest) = rest.split_at(width);
        let (b, e) = rest.split_at(width);
        for (i, o) in out.chunks_exact_mut(4).enumerate() {
            o.copy_from_slice(&[r[i], g[i], b[i], e[i]]);
        }
        return Ok(at);
    }
    // Flat pixels, possibly with old-style runs (1,1,1,n repeats the last
    // pixel n << shift times).
    let mut at = 0;
    let mut x = 0;
    let mut shift = 0;
    while x < width {
        let p = data.get(at..at + 4).ok_or_else(short)?;
        at += 4;
        if p[0] == 1 && p[1] == 1 && p[2] == 1 {
            let n = (p[3] as usize).checked_shl(shift).unwrap_or(usize::MAX);
            if x == 0 || n > width - x {
                return Err(invalid("HDR: bad run"));
            }
            let last = [out[x * 4 - 4], out[x * 4 - 3], out[x * 4 - 2], out[x * 4 - 1]];
            for o in out[x * 4..(x + n) * 4].chunks_exact_mut(4) {
                o.copy_from_slice(&last);
            }
            x += n;
            shift += 8;
        } else {
            out[x * 4..x * 4 + 4].copy_from_slice(p);
            x += 1;
            shift = 0;
        }
    }
    Ok(at)
}

/// RGBE to linear floats: the shared exponent scales all three mantissas.
fn rgbe_table() -> [f32; 256] {
    std::array::from_fn(|e| if e == 0 { 0.0 } else { 2f32.powi(e as i32 - 136) })
}

/// CIE XYZ to linear sRGB primaries.
fn xyz_to_rgb(c: [f32; 3]) -> [f32; 3] {
    [
        3.2406 * c[0] - 1.5372 * c[1] - 0.4986 * c[2],
        -0.9689 * c[0] + 1.8758 * c[1] + 0.0415 * c[2],
        0.0557 * c[0] - 0.2040 * c[1] + 1.0570 * c[2],
    ]
}

/// The length of one encoded scanline, found by walking its run headers
/// without decoding, or `None` when the data ends first or is malformed.
fn line_len(data: &[u8], width: usize) -> Option<usize> {
    let new_rle = (8..0x8000).contains(&width)
        && data.len() >= 4
        && data[0] == 2
        && data[1] == 2
        && data[2] & 0x80 == 0
        && ((data[2] as usize) << 8 | data[3] as usize) == width;
    let mut at = 0;
    if new_rle {
        at = 4;
        for _ in 0..4 {
            let mut x = 0;
            while x < width {
                let count = *data.get(at)? as usize;
                let (n, size) = if count > 128 { (count - 128, 2) } else { (count, 1 + count) };
                if n == 0 || x + n > width {
                    return None;
                }
                at += size;
                x += n;
            }
        }
        return (at <= data.len()).then_some(at);
    }
    let mut x = 0;
    let mut shift = 0u32;
    while x < width {
        let p = data.get(at..at + 4)?;
        at += 4;
        if p[0] == 1 && p[1] == 1 && p[2] == 1 {
            let n = (p[3] as usize).checked_shl(shift)?;
            if x == 0 || n > width - x {
                return None;
            }
            x += n;
            shift += 8;
        } else {
            x += 1;
            shift = 0;
        }
    }
    Some(at)
}

/// Averages a band of up to `factor` scanlines into one output row, in
/// boxes of `factor` texels.
fn reduce_band(lines: &[&[u8]], w: usize, factor: usize, flip_x: bool, table: &[f32; 256], out: &mut [f32]) {
    let mut rgbe = vec![0u8; w * 4];
    let mut planes = vec![0u8; w * 4];
    let mut rows = 0;
    for line in lines {
        if scanline(line, w, &mut rgbe, &mut planes).is_err() {
            continue;
        }
        if flip_x {
            let texels: Vec<[u8; 4]> = rgbe.chunks_exact(4).rev().map(|p| [p[0], p[1], p[2], p[3]]).collect();
            rgbe = texels.concat();
        }
        rows += 1;
        for (ox, o) in out.chunks_exact_mut(4).enumerate() {
            let x1 = ((ox + 1) * factor).min(w);
            let (mut r, mut g, mut b) = (0f32, 0f32, 0f32);
            for p in rgbe[ox * factor * 4..x1 * 4].chunks_exact(4) {
                let s = table[p[3] as usize];
                r += p[0] as f32 * s;
                g += p[1] as f32 * s;
                b += p[2] as f32 * s;
            }
            o[0] += r;
            o[1] += g;
            o[2] += b;
        }
    }
    for (ox, o) in out.chunks_exact_mut(4).enumerate() {
        let cols = ((ox + 1) * factor).min(w) - ox * factor;
        let inv = 1.0 / (cols * rows.max(1)) as f32;
        o[0] *= inv;
        o[1] *= inv;
        o[2] *= inv;
        o[3] = 1.0;
    }
}

pub(crate) fn decode(bytes: &[u8], opts: &DecodeOptions) -> Result<Texture, TextureError> {
    let mut feed = Feed::memory(bytes);
    decode_feed(&mut feed, bytes.len() as u64, opts)
}

pub(crate) fn decode_reader(r: &mut dyn Read, file_bytes: u64, opts: &DecodeOptions) -> Result<Texture, TextureError> {
    let mut feed = Feed::reader(r);
    decode_feed(&mut feed, file_bytes, opts)
}

fn decode_feed(feed: &mut Feed, file_bytes: u64, opts: &DecodeOptions) -> Result<Texture, TextureError> {
    let h = parse_header(feed)?;
    let (w, ht) = (h.width as usize, h.height as usize);

    // 1. Shrink by the largest whole factor that keeps the longer side at
    //    least the size asked for; without one, keep every texel.  Each
    //    output row is the average of one band of `factor` scanlines.
    let factor = match opts.min_size.or(opts.max_size) {
        Some(min) => reduce_factor(h.width.max(h.height), min) as usize,
        None => 1,
    };
    let (ow, oh) = (w.div_ceil(factor), ht.div_ceil(factor));
    check_size(ow as u32, oh as u32, 1, 16)?;
    let mut out = vec![0f32; ow * oh * 4];
    let table = rgbe_table();

    // 2. Take the data a chunk at a time.  Finding where scanlines end only
    //    means walking run headers, which is quick; the bands found are then
    //    decoded and averaged in parallel.  A file that ends early keeps the
    //    rows it had, since a partly downloaded sky still makes a useful
    //    thumbnail.
    let band_max = factor * (4 + w * 8);
    let mut row = 0;
    while row < oh {
        feed.ensure(band_max.max(CHUNK))?;
        let data = feed.data();
        let mut bands: Vec<Vec<&[u8]>> = Vec::new();
        let mut at = 0;
        let mut ended = false;
        'bands: while row + bands.len() < oh {
            let lines = factor.min(ht - (row + bands.len()) * factor);
            let mut band = Vec::with_capacity(lines);
            let mut band_at = at;
            for _ in 0..lines {
                match line_len(&data[band_at..], w) {
                    Some(n) => {
                        band.push(&data[band_at..band_at + n]);
                        band_at += n;
                    }
                    None => {
                        // Incomplete: wait for more data, unless there is no
                        // more or a whole band was already available.
                        if feed.eof || data.len() - at >= band_max {
                            if !band.is_empty() {
                                bands.push(band);
                            }
                            ended = true;
                        }
                        break 'bands;
                    }
                }
            }
            bands.push(band);
            at = band_at;
        }
        let done = bands.len();
        out[row * ow * 4..(row + done) * ow * 4]
            .par_chunks_mut(ow * 4)
            .zip(bands.par_iter())
            .for_each(|(o, band)| reduce_band(band, w, factor, h.flip_x, &table, o));
        row += done;
        feed.consume(at);
        if ended || done == 0 {
            break;
        }
    }

    let mut tex = Texture::from_rgba32f(ow as u32, oh as u32, out);
    if h.xyz {
        let Pixels::Rgba32F(v) = &mut tex.levels[0].surfaces[0] else { unreachable!() };
        for p in v.chunks_exact_mut(4) {
            let c = xyz_to_rgb([p[0], p[1], p[2]]);
            p[..3].copy_from_slice(&c);
        }
    }
    if h.flip_y {
        crate::assemble::flip(&mut tex.levels[0].surfaces[0], ow as u32, oh as u32, 1, false, true);
    }
    tex.has_alpha = false;
    tex.source.container = "Radiance HDR".into();
    tex.source.format = if h.xyz { "XYZE" } else { "RGBE" }.into();
    tex.source.width = h.width;
    tex.source.height = h.height;
    tex.source.file_bytes = file_bytes;
    Ok(tex)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Encodes a scanline the way Radiance does, with runs for repeats.
    fn rle_line(px: &[[u8; 4]]) -> Vec<u8> {
        let w = px.len();
        let mut out = vec![2, 2, (w >> 8) as u8, w as u8];
        for c in 0..4 {
            let vals: Vec<u8> = px.iter().map(|p| p[c]).collect();
            let mut i = 0;
            while i < w {
                let mut run = 1;
                while i + run < w && run < 127 && vals[i + run] == vals[i] {
                    run += 1;
                }
                if run >= 3 {
                    out.extend_from_slice(&[128 + run as u8, vals[i]]);
                    i += run;
                } else {
                    let n = (w - i).min(128);
                    out.push(n as u8);
                    out.extend_from_slice(&vals[i..i + n]);
                    i += n;
                }
            }
        }
        out
    }

    fn file(w: usize, h: usize, rle: bool, row: impl Fn(usize) -> Vec<[u8; 4]>) -> Vec<u8> {
        let mut f = format!("#?RADIANCE\nFORMAT=32-bit_rle_rgbe\n\n-Y {h} +X {w}\n").into_bytes();
        for y in 0..h {
            let px = row(y);
            if rle {
                f.extend(rle_line(&px));
            } else {
                f.extend(px.iter().flatten());
            }
        }
        f
    }

    #[test]
    fn values() {
        // 128 with exponent 129 is 1.0, 64 is 0.5; 48 with exponent 130 is
        // 48 / 64.
        let f = file(16, 2, true, |y| {
            (0..16).map(|x| if y == 0 { [128, 64, 0, 129] } else { [x as u8 * 16, 0, 0, 130] }).collect()
        });
        let tex = decode(&f, &DecodeOptions::full()).unwrap();
        let crate::Pixels::Rgba32F(v) = tex.base() else { panic!() };
        assert_eq!(&v[..4], &[1.0, 0.5, 0.0, 1.0]);
        assert_eq!(v[(16 + 3) * 4], 0.75);
    }

    #[test]
    fn flat_and_old_runs() {
        let mut f = b"#?RGBE\n\n-Y 1 +X 10\n".to_vec();
        f.extend([10, 20, 30, 128]);
        f.extend([1, 1, 1, 9]);
        let tex = decode(&f, &DecodeOptions::full()).unwrap();
        let crate::Pixels::Rgba32F(v) = tex.base() else { panic!() };
        assert_eq!(v[9 * 4 + 1], 20.0 / 256.0);
    }

    #[test]
    fn streaming_reduction_matches_whole() {
        let f = file(300, 200, true, |y| {
            (0..300).map(|x| [(x % 200) as u8, (y % 250) as u8, 7, 128 + (x % 3) as u8]).collect()
        });
        let small = decode(&f, &DecodeOptions::preview(64)).unwrap();
        assert_eq!((small.width(), small.height()), (75, 50));
        let mut r = &f[..];
        let streamed = decode_reader(&mut r, f.len() as u64, &DecodeOptions::preview(64)).unwrap();
        let (crate::Pixels::Rgba32F(a), crate::Pixels::Rgba32F(b)) = (small.base(), streamed.base()) else { panic!() };
        assert_eq!(a, b);
        assert_eq!(small.source.width, 300);
    }

    #[test]
    fn truncated_keeps_rows() {
        let f = file(64, 64, true, |_| vec![[100, 100, 100, 128]; 64]);
        for cut in [10, 40, 200, f.len() / 2, f.len() - 1] {
            let r = decode(&f[..cut], &DecodeOptions::full());
            if let Ok(t) = r {
                assert_eq!(t.width(), 64);
            }
        }
    }

    #[test]
    fn garbage_is_rejected() {
        for bad in [
            &b"#?RADIANCE\n"[..],
            b"#?RADIANCE\n\n-Y 0 +X 5\n",
            b"#?RADIANCE\n\n+X 5 -Y 5\n",
            b"#?RADIANCE\n\n-Y 99999999 +X 5\n",
        ] {
            assert!(decode(bad, &DecodeOptions::full()).is_err());
        }
    }
}
