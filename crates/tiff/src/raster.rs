//! Assembling a TIFF page from its strips or tiles: decompression, the
//! predictor, bit depths from 1 to 64, planar or interleaved samples, and
//! the photometric interpretations pictures and microscopes use.  Pages come
//! out as little-endian samples of one `Sample`, interleaved.

use crate::{Page, Tiff, codec, read_exact_at, tag};
use rayon::prelude::*;

/// Compression codes.
pub mod comp {
    pub const NONE: u16 = 1;
    pub const CCITT_G3: u16 = 3;
    pub const CCITT_G4: u16 = 4;
    pub const LZW: u16 = 5;
    pub const JPEG: u16 = 7;
    pub const DEFLATE: u16 = 8;
    pub const PACKBITS: u16 = 32773;
    pub const DEFLATE_OLD: u16 = 32946;
    pub const APERIO_J2K_YCC: u16 = 33003;
    pub const APERIO_J2K_RGB: u16 = 33005;
    pub const J2K: u16 = 34712;
    pub const ZSTD: u16 = 50000;
    pub const ZSTD_OLD: u16 = 34926;
}

pub fn compression_name(c: u16) -> String {
    match c {
        1 => "none".into(),
        5 => "LZW".into(),
        6 => "old-style JPEG".into(),
        7 => "JPEG".into(),
        8 | 32946 => "Deflate".into(),
        32773 => "PackBits".into(),
        33003 | 33005 | 34712 => "JPEG 2000".into(),
        34925 => "LZMA".into(),
        34926 | 50000 => "Zstandard".into(),
        50001 => "WebP".into(),
        50002 => "JPEG XL".into(),
        2..=4 => "CCITT fax".into(),
        other => format!("code {other}"),
    }
}

pub fn photometric_name(p: u16) -> &'static str {
    match p {
        0 => "WhiteIsZero",
        1 => "BlackIsZero",
        2 => "RGB",
        3 => "palette",
        4 => "mask",
        5 => "CMYK",
        6 => "YCbCr",
        8..=10 => "CIELab",
        32803 => "colour filter array (camera raw)",
        34892 => "linear raw",
        _ => "unknown",
    }
}

/// What one sample of a decoded page is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Sample {
    U8,
    I8,
    U16,
    I16,
    U32,
    I32,
    U64,
    I64,
    /// IEEE half precision.
    F16,
    F32,
    F64,
}

impl Sample {
    /// Bytes per sample.
    pub const fn size(self) -> usize {
        match self {
            Sample::U8 | Sample::I8 => 1,
            Sample::U16 | Sample::I16 | Sample::F16 => 2,
            Sample::U32 | Sample::I32 | Sample::F32 => 4,
            Sample::U64 | Sample::I64 | Sample::F64 => 8,
        }
    }

    pub const fn is_float(self) -> bool {
        matches!(self, Sample::F16 | Sample::F32 | Sample::F64)
    }
}

/// A decoded page.
#[derive(Clone, Debug)]
pub struct Raster {
    pub width: usize,
    pub height: usize,
    pub samples: usize,
    pub sample: Sample,
    /// Little-endian samples, rows from the top, pixels left to right, the
    /// samples of a pixel together.
    pub bytes: Vec<u8>,
    /// Low values are bright (WhiteIsZero).
    pub invert: bool,
    /// The alpha samples of a grey page that has them, in the same sample
    /// type as `bytes` (which then holds the grey alone).
    pub alpha: Option<Vec<u8>>,
    /// How many pages' pixels each of these covers along each axis: more
    /// than 1 when the page was decoded at reduced size.
    pub reduction: usize,
    pub warning: Option<String>,
}

/// What to decode.
#[derive(Clone, Copy, Debug)]
pub struct Options {
    /// Decode at 1/n size when the compression allows (JPEG): n is 1, 2, 4
    /// or 8, the largest reduction that keeps the long side at least this
    /// many pixels.  0 decodes everything.
    pub min_long_side: usize,
    /// Refuse pages with more pixels than this.
    pub max_pixels: u64,
}

impl Options {
    pub fn full() -> Options {
        Options { min_long_side: 0, max_pixels: 1 << 27 }
    }
}

/// The sample type of a page whose compression keeps the file's own samples.
pub fn native_sample(p: &Page) -> Result<Sample, String> {
    native_type(p).map(|t| t.0)
}

fn native_type(p: &Page) -> Result<(Sample, usize), String> {
    let bits = p.bits.first().copied().unwrap_or(8);
    if p.bits.iter().any(|&b| b != bits) && p.bits.len() >= p.samples.min(p.bits.len()) {
        return Err("samples of different sizes aren't supported".into());
    }
    let t = match (bits, p.sample_format) {
        // Samples of 1 to 7 bits are widened to bytes, of 9 to 15 to words.
        (1, _) | (2, _) | (4, _) | (8, 1 | 4) | (3 | 5..=7, 1 | 4) => Sample::U8,
        (9..=15, 1 | 4) => Sample::U16,
        (8, 2) => Sample::I8,
        (16, 1 | 4) => Sample::U16,
        (16, 2) => Sample::I16,
        (16, 3) => Sample::F16,
        (32, 1 | 4) => Sample::U32,
        (32, 2) => Sample::I32,
        (32, 3) => Sample::F32,
        (64, 1 | 4) => Sample::U64,
        (64, 2) => Sample::I64,
        (64, 3) => Sample::F64,
        (b, f) => return Err(format!("{b}-bit samples of format {f} aren't supported")),
    };
    Ok((t, bits as usize))
}

/// Why a page can't be shown, without reading any of it.
pub fn check(p: &Page) -> Result<(), String> {
    match p.photometric {
        0..=3 | 5 | 6 | 34892 => {}
        32803 => return Err("this is camera raw data (a colour filter array), which Slerp doesn't develop".into()),
        8..=10 => return Err("CIE L*a*b* colour isn't supported".into()),
        other => return Err(format!("photometric interpretation {other} isn't supported")),
    }
    match p.compression {
        comp::NONE
        | comp::LZW
        | comp::JPEG
        | comp::DEFLATE
        | comp::DEFLATE_OLD
        | comp::PACKBITS
        | comp::CCITT_G3
        | comp::CCITT_G4
        | comp::APERIO_J2K_YCC
        | comp::APERIO_J2K_RGB
        | comp::J2K
        | comp::ZSTD
        | comp::ZSTD_OLD => {}
        c => return Err(format!("{} compression isn't supported", compression_name(c))),
    }
    if matches!(p.compression, comp::CCITT_G3 | comp::CCITT_G4) {
        return if p.bits.first() == Some(&1) && p.samples == 1 {
            Ok(())
        } else {
            Err("fax compression holds one-bit pictures only".into())
        };
    }
    if p.photometric == 6
        && !is_lossy_image_codec(p.compression)
        && !(p.bits.first() == Some(&8) && p.samples == 3 && p.planar == 1)
    {
        return Err("YCbCr other than 8-bit interleaved isn't supported".into());
    }
    if !is_lossy_image_codec(p.compression) {
        native_type(p)?;
    }
    Ok(())
}

fn is_lossy_image_codec(c: u16) -> bool {
    matches!(c, comp::JPEG | comp::APERIO_J2K_YCC | comp::APERIO_J2K_RGB | comp::J2K)
}

/// One decoded strip or tile.
struct Chunk {
    x: usize,
    y: usize,
    plane: usize,
    width: usize,
    height: usize,
    samples: usize,
    bytes: Vec<u8>,
    dtype: Sample,
}

/// Unpacks rows of `bits`-bit samples (1 to 7) to one byte each, padding
/// each row to a byte boundary as TIFF does.
fn unpack_bits(src: &[u8], row_samples: usize, rows: usize, bits: usize, scale: bool) -> Vec<u8> {
    let row_bytes = (row_samples * bits).div_ceil(8);
    let max = (1u32 << bits) - 1;
    let mut out = vec![0u8; row_samples * rows];
    for r in 0..rows {
        let Some(row) = src.get(r * row_bytes..(r + 1) * row_bytes) else { break };
        for i in 0..row_samples {
            // Samples of 3, 5, 6 or 7 bits straddle bytes: read bit by bit.
            let mut v = 0u32;
            for k in 0..bits {
                let at = i * bits + k;
                v = (v << 1) | ((row[at / 8] >> (7 - at % 8)) & 1) as u32;
            }
            out[r * row_samples + i] = if scale { (v * 255 / max) as u8 } else { v as u8 };
        }
    }
    out
}

/// Unpacks rows of `bits`-bit samples (9 to 15) to little-endian 16-bit
/// words, scaled to the full range when `scale` is set.
fn unpack_wide(src: &[u8], row_samples: usize, rows: usize, bits: usize, scale: bool) -> Vec<u8> {
    let row_bytes = (row_samples * bits).div_ceil(8);
    let max = (1u32 << bits) - 1;
    let mut out = vec![0u8; row_samples * rows * 2];
    for r in 0..rows {
        let Some(row) = src.get(r * row_bytes..(r + 1) * row_bytes) else { break };
        for i in 0..row_samples {
            let bit = i * bits;
            let mut v = 0u32;
            for k in 0..bits {
                let at = bit + k;
                v = (v << 1) | ((row[at / 8] >> (7 - at % 8)) & 1) as u32;
            }
            let v = if scale { v * 65535 / max } else { v } as u16;
            out[(r * row_samples + i) * 2..][..2].copy_from_slice(&v.to_le_bytes());
        }
    }
    out
}

impl Page {
    /// Decodes the page.
    pub fn decode(&self, t: &Tiff, o: &Options) -> Result<Raster, String> {
        check(self)?;
        if self.pixels() > o.max_pixels {
            return Err(format!(
                "the page has {} million pixels, more than is decoded at once",
                self.pixels() / 1_000_000
            ));
        }
        let (offsets, counts) = self.chunk_table(t)?;
        let tables = match self.ifd.get(tag::JPEG_TABLES) {
            Some(e) => Some(t.bytes(e)?),
            None => None,
        };
        let lossy = is_lossy_image_codec(self.compression);
        // 1. How much smaller to decode.
        let (cw, ch) = match self.tile {
            Some((w, h)) => (w as usize, h as usize),
            None => (self.width as usize, self.rows_per_strip as usize),
        };
        let long = self.width.max(self.height) as usize;
        let mut reduction = 1usize;
        if lossy && self.compression == comp::JPEG && o.min_long_side > 0 {
            while reduction < 8
                && long / (reduction * 2) >= o.min_long_side
                && cw % (reduction * 2) == 0
                && ch % (reduction * 2) == 0
            {
                reduction *= 2;
            }
        }
        let (out_w, out_h) = ((self.width as usize).div_ceil(reduction), (self.height as usize).div_ceil(reduction));
        // 2. The first chunk that has data tells what the samples are.
        let first = counts.iter().zip(&offsets).position(|(&c, &off)| c > 0 && off > 0);
        let Some(first) = first else {
            return Err("the page has no data".into());
        };
        let c0 = self.decode_chunk(t, first, offsets[first], counts[first], tables.as_deref(), reduction)?;
        let (samples, dtype) =
            if self.planar == 2 && !lossy { (self.samples, c0.dtype) } else { (c0.samples, c0.dtype) };
        let esize = dtype.size();
        let total = (out_w as u64).checked_mul(out_h as u64).and_then(|n| n.checked_mul(samples as u64 * esize as u64));
        let total = total.filter(|&n| n <= 3 << 30).ok_or("the decoded page would be too large")?;
        let background = if matches!(self.photometric, 2 | 6) || self.photometric == 0 { 255u8 } else { 0 };
        let mut bytes = vec![background; total as usize];
        // 3. Decode the chunks in batches, in parallel, and place them.
        let (across, _down, _planes) = self.chunk_grid();
        let mut failed = 0usize;
        let mut first_error: Option<String> = None;
        let place = |c: Chunk, bytes: &mut Vec<u8>| {
            let (cw_s, ch_s) = (c.width, c.height);
            let (x0, y0) = (c.x / reduction, c.y / reduction);
            for r in 0..ch_s {
                let y = y0 + r;
                if y >= out_h {
                    break;
                }
                let w = cw_s.min(out_w.saturating_sub(x0));
                if w == 0 {
                    break;
                }
                if c.plane_interleave(samples) {
                    for xx in 0..w {
                        let dst = ((y * out_w + x0 + xx) * samples + c.plane) * esize;
                        let src = (r * cw_s + xx) * esize;
                        bytes[dst..dst + esize].copy_from_slice(&c.bytes[src..src + esize]);
                    }
                } else {
                    let dst = (y * out_w + x0) * samples * esize;
                    let src = r * cw_s * samples * esize;
                    let n = w * samples * esize;
                    bytes[dst..dst + n].copy_from_slice(&c.bytes[src..src + n]);
                }
            }
        };
        place(c0, &mut bytes);
        let todo: Vec<usize> = (0..offsets.len()).filter(|&i| i != first && counts[i] > 0 && offsets[i] > 0).collect();
        for batch in todo.chunks(256) {
            let results: Vec<(usize, Result<Chunk, String>)> = batch
                .par_iter()
                .map(|&i| (i, self.decode_chunk(t, i, offsets[i], counts[i], tables.as_deref(), reduction)))
                .collect();
            for (i, r) in results {
                match r {
                    Ok(c) if c.dtype == dtype && (c.samples == samples || self.planar == 2) => place(c, &mut bytes),
                    Ok(_) => {
                        failed += 1;
                        first_error.get_or_insert_with(|| format!("strip or tile {i} has a different sample layout"));
                    }
                    Err(e) => {
                        failed += 1;
                        first_error.get_or_insert(e);
                    }
                }
            }
        }
        let _ = across;
        let mut warning = None;
        if failed > 0 {
            warning =
                Some(format!("{failed} strips or tiles couldn't be decoded ({})", first_error.unwrap_or_default()));
        }
        // 4. Photometric conversions.
        let mut r = Raster {
            width: out_w,
            height: out_h,
            samples,
            sample: dtype,
            bytes,
            invert: false,
            alpha: None,
            reduction,
            warning,
        };
        self.finish(t, &mut r)?;
        Ok(r)
    }

    /// Reads and decodes one strip or tile.
    fn decode_chunk(
        &self,
        t: &Tiff,
        index: usize,
        offset: u64,
        count: u64,
        tables: Option<&[u8]>,
        reduction: usize,
    ) -> Result<Chunk, String> {
        let (across, down, planes) = self.chunk_grid();
        let per_plane = (across * down) as usize;
        let plane = if self.planar == 2 { index / per_plane.max(1) } else { 0 };
        let within = index % per_plane.max(1);
        let (tx, ty) = (within as u64 % across, within as u64 / across);
        let _ = (down, planes);
        let (cw, ch, x, y) = match self.tile {
            Some((tw, th)) => (tw as usize, th as usize, (tx * tw) as usize, (ty * th) as usize),
            None => {
                let y0 = ty * self.rows_per_strip;
                let rows = self.rows_per_strip.min(self.height - y0.min(self.height));
                (self.width as usize, rows as usize, 0, y0 as usize)
            }
        };
        if count > 1 << 30 || offset.checked_add(count).is_none_or(|e| e > t.len) {
            return Err("a strip or tile lies outside the file".into());
        }
        let mut data = vec![0u8; count as usize];
        read_exact_at(&t.file, &mut data, offset).map_err(|e| e.to_string())?;
        // Some writers store the bits of each byte in reverse order.
        if self.fill_order == 2 && !is_lossy_image_codec(self.compression) {
            data.iter_mut().for_each(|b| *b = b.reverse_bits());
        }
        let spp = if self.planar == 2 { 1 } else { self.samples };
        match self.compression {
            comp::CCITT_G3 | comp::CCITT_G4 => {
                let black = codec::ccitt(&data, self.compression == comp::CCITT_G4, self.t4_options, cw, ch)?;
                // Fax "black" is the bit 1 and "white" the bit 0, whatever the
                // photometric interpretation says they look like (a MinIsBlack
                // file with a white page codes the page as black runs).
                let bytes = black.iter().map(|&b| if b == 1 { 255 } else { 0 }).collect();
                Ok(Chunk { x, y, plane, width: cw, height: ch, samples: 1, bytes, dtype: Sample::U8 })
            }
            comp::JPEG | comp::APERIO_J2K_YCC | comp::APERIO_J2K_RGB | comp::J2K => {
                let cap = cw.saturating_mul(ch).saturating_mul(8).max(1 << 20);
                let px = if self.compression == comp::JPEG {
                    codec::jpeg(tables, &data, reduction, cap, self.photometric == 2 && self.samples == 3)?
                } else {
                    codec::jpeg2000(&data, self.compression == comp::APERIO_J2K_YCC, reduction, cap)?
                };
                let (bytes, comps) = if px.comps == 4 && px.bytes == 1 && self.photometric != 2 {
                    (cmyk_to_rgb(&px.data), 3)
                } else {
                    (px.data, px.comps)
                };
                let dtype = if px.bytes == 2 { Sample::U16 } else { Sample::U8 };
                Ok(Chunk { x, y, plane, width: px.width, height: px.height, samples: comps, bytes, dtype })
            }
            _ => {
                let (dtype, bits) = native_type(self)?;
                let row_samples = cw * spp;
                let row_bytes = (row_samples * bits).div_ceil(8);
                // Subsampled YCbCr stores units of h * v luma samples and a Cb and a Cr.
                let ycc = self.photometric == 6 && self.planar == 1 && bits == 8 && spp == 3;
                let (hs, vs) = (self.ycbcr_subsampling.0 as usize, self.ycbcr_subsampling.1 as usize);
                let expected = if ycc {
                    cw.div_ceil(hs.max(1)).saturating_mul(ch.div_ceil(vs.max(1))).saturating_mul(hs * vs + 2)
                } else {
                    row_bytes.checked_mul(ch).ok_or("a chunk is too large")?
                };
                if expected > 1 << 30 {
                    return Err("a strip or tile is too large".into());
                }
                let mut raw = match self.compression {
                    comp::NONE => data,
                    comp::LZW => codec::lzw(&data, expected)?,
                    comp::DEFLATE | comp::DEFLATE_OLD => codec::deflate(&data, expected)?,
                    comp::PACKBITS => codec::packbits(&data, expected)?,
                    comp::ZSTD | comp::ZSTD_OLD => codec::zstd(&data, expected)?,
                    c => return Err(format!("{} compression isn't supported", compression_name(c))),
                };
                // A short chunk keeps the rows it has; the rest stays blank.
                if raw.len() < expected {
                    raw.resize(expected, 0);
                }
                raw.truncate(expected);
                if ycc {
                    let bytes = codec::ycbcr_to_rgb(
                        &raw,
                        cw,
                        ch,
                        (hs, vs),
                        self.ycbcr_coefficients,
                        self.reference_black_white,
                    )?;
                    return Ok(Chunk { x, y, plane, width: cw, height: ch, samples: 3, bytes, dtype: Sample::U8 });
                }
                let esize = dtype.size();
                if matches!(bits, 8 | 16 | 32 | 64) {
                    if self.predictor == 3 {
                        codec::unpredict_float(&mut raw, row_samples, spp, esize);
                    } else {
                        if !t.le {
                            codec::swap_bytes(&mut raw, esize);
                        }
                        if self.predictor == 2 {
                            codec::unpredict_horizontal(&mut raw, row_samples, spp, esize);
                        }
                    }
                    if self.predictor == 3 && !cfg!(target_endian = "little") {
                        codec::swap_bytes(&mut raw, esize);
                    }
                    return Ok(Chunk { x, y, plane, width: cw, height: ch, samples: spp, bytes: raw, dtype });
                }
                // Sub-byte samples: bilevel and grey scale to 8 bits; palette
                // indices stay as they are.
                if self.predictor == 2 {
                    return Err("a predictor on samples smaller than a byte isn't supported".into());
                }
                let scale = self.photometric != 3;
                if bits > 8 {
                    let bytes = unpack_wide(&raw, row_samples, ch, bits, scale);
                    return Ok(Chunk { x, y, plane, width: cw, height: ch, samples: spp, bytes, dtype: Sample::U16 });
                }
                let bytes = unpack_bits(&raw, row_samples, ch, bits, scale);
                Ok(Chunk { x, y, plane, width: cw, height: ch, samples: spp, bytes, dtype: Sample::U8 })
            }
        }
    }

    /// Photometric conversions after assembly: palettes, CMYK, alpha, and the
    /// sense of grey.
    fn finish(&self, t: &Tiff, r: &mut Raster) -> Result<(), String> {
        match self.photometric {
            3 => {
                let e = self.ifd.get(tag::COLOR_MAP).ok_or("a palette image has no colour map")?;
                let map = t.ints(e, 3 * 65536)?;
                let n = map.len() / 3;
                if n == 0 || r.samples != 1 {
                    return Err("the colour map doesn't fit the image".into());
                }
                let idx = |i: usize| -> usize {
                    if r.sample == Sample::U16 {
                        u16::from_le_bytes([r.bytes[i * 2], r.bytes[i * 2 + 1]]) as usize
                    } else {
                        r.bytes[i] as usize
                    }
                };
                let mut rgb = vec![0u8; r.width * r.height * 3];
                for i in 0..r.width * r.height {
                    let k = idx(i).min(n - 1);
                    for c in 0..3 {
                        rgb[i * 3 + c] = (map[c * n + k] >> 8) as u8;
                    }
                }
                r.bytes = rgb;
                r.samples = 3;
                r.sample = Sample::U8;
            }
            5 if r.samples >= 4 && r.sample == Sample::U8 => {
                // Inks first, then an alpha sample if the file says one is there.
                let alpha = r.samples >= 5 && matches!(self.extra_samples.first(), Some(1 | 2));
                let n = r.samples;
                let mut out = Vec::with_capacity(r.width * r.height * if alpha { 4 } else { 3 });
                for p in r.bytes.chunks_exact(n) {
                    out.extend_from_slice(&cmyk_to_rgb(&p[..4]));
                    if alpha {
                        out.push(p[4]);
                    }
                }
                r.bytes = out;
                r.samples = if alpha { 4 } else { 3 };
            }
            0 => r.invert = r.samples == 1,
            _ => {}
        }
        // Grey with an alpha sample: the grey is the picture, the alpha kept apart.
        if r.samples == 2 && matches!(self.photometric, 0 | 1) {
            let e = r.sample.size();
            let mut g = Vec::with_capacity(r.bytes.len() / 2);
            let mut a = Vec::with_capacity(r.bytes.len() / 2);
            for px in r.bytes.chunks_exact(2 * e) {
                g.extend_from_slice(&px[..e]);
                a.extend_from_slice(&px[e..2 * e]);
            }
            r.bytes = g;
            r.alpha = Some(a);
            r.samples = 1;
        }
        Ok(())
    }
}

impl Chunk {
    /// Planar chunks hold one sample of every pixel and are interleaved into
    /// place.
    fn plane_interleave(&self, total_samples: usize) -> bool {
        self.samples == 1 && total_samples > 1
    }
}

/// CMYK bytes to RGB.
pub fn cmyk_to_rgb(cmyk: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(cmyk.len() / 4 * 3);
    for p in cmyk.chunks_exact(4) {
        let k = 255 - p[3] as u32;
        for &v in &p[..3] {
            out.push(((255 - v as u32) * k / 255) as u8);
        }
    }
    out
}
