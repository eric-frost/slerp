//! Tile-compressed FITS images (FITS 4.0, section 10).  The image is cut
//! into rectangular tiles, each compressed on its own and stored as one row
//! of a binary table; the compressed bytes live in the table's heap, after
//! the fixed-size part.  Floating-point images are first quantised to
//! integers with a per-tile scale and offset, optionally with a pseudo-random
//! dither so the quantisation error has no pattern.

use super::header::{Header, Value};
use super::{Hdu, Spec, hcompress, plio, rice};
use crate::TextureError;
use crate::util::{Stream, gunzip, invalid, read_fully, too_large, unsupported};
use rayon::prelude::*;
use std::io::SeekFrom;
use std::sync::OnceLock;

/// One decoded plane: values in physical units, rows in file order (bottom
/// row first), NaN for undefined pixels.
pub(crate) struct Plane {
    pub width: u32,
    pub height: u32,
    pub values: Vec<f32>,
}

/// Largest binary table read into memory.
const MAX_TABLE_BYTES: u64 = 512 << 20;

/// Compressed bytes read before a batch of tiles is decoded.
const BATCH_BYTES: u64 = 48 << 20;

/// Pixels decoded at once across a batch of tiles (each costs 8 bytes while
/// decoding), so memory stays bounded however many tiles a file has.
const BATCH_PIXELS: usize = 16 << 20;

/// The most pixels a tile may claim, and the most per byte of compressed
/// data once it is larger than `FREE_PIXELS`: gzip at best squeezes about
/// 1000 to 1 and PLIO about 2000 to 1, so a tiny file can't make a decoder
/// allocate a huge tile.
const MAX_TILE_PIXELS: usize = 1 << 27;
const FREE_PIXELS: usize = 1 << 22;
const MAX_RATIO: usize = 4096;

/// Pseudo-random numbers in the dither table.
const N_RANDOM: usize = 10000;

/// The values cfitsio writes into integer data for undefined and zero pixels
/// of quantised images.
const NULL_VALUE: i32 = -2147483647;
const ZERO_VALUE: i32 = -2147483646;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Algorithm {
    Rice,
    Gzip1,
    Gzip2,
    Plio,
    Hcompress,
    None,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Dither {
    None,
    One,
    Two,
}

/*┌─────────────────╮
| Table layout    |
└─────────────────╯*/
//region Table layout

/// A column of the binary table.
#[derive(Clone, Copy, Debug, Default)]
struct Column {
    offset: usize,
    /// Variable-length column: bytes per element and whether the descriptor
    /// has 64-bit fields (`Q`).
    elem: usize,
    wide: bool,
    present: bool,
}

#[derive(Default)]
struct Columns {
    compressed: Column,
    gzip: Column,
    uncompressed: Column,
    scale: Column,
    zero: Column,
    blank: Column,
}

fn parse_columns(h: &Header, row_bytes: usize) -> Result<Columns, TextureError> {
    let fields = h.int("TFIELDS").unwrap_or(0).clamp(0, 999) as usize;
    let mut cols = Columns::default();
    let mut offset = 0usize;
    for n in 1..=fields {
        let name = h.string(&format!("TTYPE{n}")).unwrap_or("").trim().to_ascii_uppercase();
        let form = h.string(&format!("TFORM{n}")).unwrap_or("").trim().to_ascii_uppercase();
        let digits: String = form.chars().take_while(|c| c.is_ascii_digit()).collect();
        let repeat: usize =
            if digits.is_empty() { 1 } else { digits.parse().map_err(|_| invalid("FITS: bad TFORM"))? };
        let code = form[digits.len()..].chars().next().ok_or_else(|| invalid("FITS: bad TFORM"))?;
        let rest = &form[digits.len() + 1..];
        let elem_size = |c: char| -> Option<usize> {
            match c {
                'L' | 'B' | 'A' => Some(1),
                'I' => Some(2),
                'J' | 'E' => Some(4),
                'K' | 'D' | 'C' => Some(8),
                'M' => Some(16),
                _ => None,
            }
        };
        let (size, elem, wide) = match code {
            'P' | 'Q' => {
                let e = rest
                    .chars()
                    .next()
                    .and_then(elem_size)
                    .ok_or_else(|| invalid("FITS: bad variable-length TFORM"))?;
                (repeat.min(1) * if code == 'P' { 8 } else { 16 }, e, code == 'Q')
            }
            'X' => (repeat.div_ceil(8), 1, false),
            c => (
                elem_size(c)
                    .ok_or_else(|| invalid("FITS: bad TFORM"))?
                    .checked_mul(repeat)
                    .ok_or_else(|| invalid("FITS: bad TFORM"))?,
                0,
                false,
            ),
        };
        let col = Column { offset, elem, wide, present: true };
        match name.as_str() {
            "COMPRESSED_DATA" => cols.compressed = col,
            "GZIP_COMPRESSED_DATA" => cols.gzip = col,
            "UNCOMPRESSED_DATA" => cols.uncompressed = col,
            "ZSCALE" => cols.scale = col,
            "ZZERO" => cols.zero = col,
            "ZBLANK" => cols.blank = col,
            _ => {}
        }
        offset = offset.checked_add(size).ok_or_else(|| invalid("FITS: table too wide"))?;
    }
    if offset > row_bytes {
        return Err(invalid("FITS: columns wider than the table row"));
    }
    Ok(cols)
}

//endregion

/*┌─────────────────╮
| Parameters      |
└─────────────────╯*/
//region Parameters

struct Params {
    algorithm: Algorithm,
    /// Rice: pixels per block and bytes per pixel.
    block: usize,
    bytepix: usize,
    /// HCOMPRESS: the scale factor and whether to smooth.
    hscale: i32,
    smooth: bool,
    float: bool,
    /// Floating-point images were quantised to integers.
    quantised: bool,
    dither: Dither,
    dither_seed: i64,
    bitpix: i32,
    bzero: f64,
    bscale: f64,
    /// Fallbacks for ZSCALE, ZZERO and ZBLANK when the columns are absent.
    scale: Option<f64>,
    zero: Option<f64>,
    blank: Option<i64>,
}

fn parse_params(h: &Header, spec: &Spec, cols: &Columns) -> Result<Params, TextureError> {
    let name = h.string("ZCMPTYPE").unwrap_or("").trim().to_ascii_uppercase();
    let algorithm = match name.as_str() {
        "RICE_1" | "RICE_ONE" => Algorithm::Rice,
        "GZIP_1" => Algorithm::Gzip1,
        "GZIP_2" => Algorithm::Gzip2,
        "PLIO_1" => Algorithm::Plio,
        "HCOMPRESS_1" => Algorithm::Hcompress,
        "NOCOMPRESS" => Algorithm::None,
        other => return Err(unsupported(format!("FITS compression {other:?}"))),
    };
    let mut block = 32usize;
    let mut bytepix = 4usize;
    let mut hscale = 0i32;
    let mut smooth = false;
    for n in 1..=16 {
        let Some(key) = h.string(&format!("ZNAME{n}")) else { continue };
        let value = h.get(&format!("ZVAL{n}"));
        let int = value.and_then(Value::as_i64);
        match key.trim().to_ascii_uppercase().as_str() {
            "BLOCKSIZE" => block = int.unwrap_or(32).clamp(1, 1 << 16) as usize,
            "BYTEPIX" => bytepix = int.unwrap_or(4).clamp(1, 8) as usize,
            "SCALE" => hscale = value.and_then(Value::as_f64).unwrap_or(0.0).round() as i32,
            "SMOOTH" => smooth = int.unwrap_or(0) != 0,
            _ => {}
        }
    }
    let float = spec.bitpix < 0;
    // Floating-point tiles hold quantised integers exactly when they come
    // with a scale and zero point; otherwise they are the floats themselves
    // (lossless gzip), whatever ZQUANTIZ says.
    let quantised = float && (cols.scale.present || h.float("ZSCALE").is_some());
    let dither = match h.string("ZQUANTIZ").map(|s| s.trim().to_ascii_uppercase()).as_deref() {
        Some("SUBTRACTIVE_DITHER_1") => Dither::One,
        Some("SUBTRACTIVE_DITHER_2") => Dither::Two,
        _ => Dither::None,
    };
    Ok(Params {
        algorithm,
        block,
        bytepix,
        hscale,
        smooth,
        float,
        quantised,
        dither,
        dither_seed: h.int("ZDITHER0").unwrap_or(1),
        bitpix: spec.bitpix,
        bzero: spec.bzero,
        bscale: spec.bscale,
        scale: h.float("ZSCALE"),
        zero: h.float("ZZERO"),
        blank: spec.blank,
    })
}

/// Cfitsio's table of 10,000 pseudo-random numbers in [0, 1), the minimal
/// standard generator of Park and Miller.  Both the writer and the reader
/// derive it the same way, so the dither never needs to be stored.
fn random_table() -> &'static [f32] {
    static TABLE: OnceLock<Vec<f32>> = OnceLock::new();
    TABLE.get_or_init(|| {
        let (a, m) = (16807.0f64, 2147483647.0f64);
        let mut seed = 1.0f64;
        (0..N_RANDOM)
            .map(|_| {
                let t = a * seed;
                seed = t - m * (t / m).trunc();
                (seed / m) as f32
            })
            .collect()
    })
}

//endregion

/*┌─────────────────╮
| Reading tiles   |
└─────────────────╯*/
//region Reading tiles

/// A tile's place in the image and what the table row says about it.
struct Job {
    plane: usize,
    x0: usize,
    y0: usize,
    w: usize,
    h: usize,
    /// One-based row number in the table, which seeds the dither.
    number: i64,
    compressed: Vec<u8>,
    gzip: Vec<u8>,
    uncompressed: Vec<u8>,
    scale: Option<f64>,
    zero: Option<f64>,
    blank: Option<i64>,
}

/// Where a tile's accumulated pixels go: straight into the plane at full
/// size, or into box sums when the picture is being reduced.
struct Sink {
    factor: usize,
    out_w: usize,
    values: Vec<f32>,
    sum: Vec<f64>,
    count: Vec<u32>,
}

impl Sink {
    fn new(width: usize, height: usize, factor: usize) -> Sink {
        let (out_w, out_h) = (width.div_ceil(factor), height.div_ceil(factor));
        if factor == 1 {
            Sink { factor, out_w, values: vec![f32::NAN; out_w * out_h], sum: Vec::new(), count: Vec::new() }
        } else {
            Sink { factor, out_w, values: Vec::new(), sum: vec![0.0; out_w * out_h], count: vec![0; out_w * out_h] }
        }
    }

    fn put(&mut self, job: &Job, tile: &[f32]) {
        let f = self.factor;
        if f == 1 {
            for y in 0..job.h {
                let dst = (job.y0 + y) * self.out_w + job.x0;
                self.values[dst..dst + job.w].copy_from_slice(&tile[y * job.w..(y + 1) * job.w]);
            }
            return;
        }
        for y in 0..job.h {
            let row = ((job.y0 + y) / f) * self.out_w;
            for x in 0..job.w {
                let v = tile[y * job.w + x];
                if !v.is_nan() {
                    let i = row + (job.x0 + x) / f;
                    self.sum[i] += v as f64;
                    self.count[i] += 1;
                }
            }
        }
    }

    fn finish(self) -> Vec<f32> {
        if self.factor == 1 {
            self.values
        } else {
            self.sum
                .iter()
                .zip(&self.count)
                .map(|(s, &c)| if c == 0 { f32::NAN } else { (*s / c as f64) as f32 })
                .collect()
        }
    }
}

/// Decodes the first `planes` planes of a tile-compressed image, shrunk by
/// `factor`.
pub(crate) fn decode(
    r: &mut dyn Stream,
    len: u64,
    hdu: &Hdu,
    spec: &Spec,
    factor: u32,
    planes: usize,
) -> Result<Vec<Plane>, TextureError> {
    let h = &hdu.header;
    // 1. The table.
    let row_bytes = h.int("NAXIS1").unwrap_or(0).max(0) as u64;
    let rows = h.int("NAXIS2").unwrap_or(0).max(0) as u64;
    let table_bytes =
        row_bytes.checked_mul(rows).filter(|&b| b <= MAX_TABLE_BYTES).ok_or_else(|| too_large("tile table"))?;
    if hdu.data_start.saturating_add(table_bytes) > len {
        return Err(invalid("FITS: tile table runs past the end of the file"));
    }
    let cols = parse_columns(h, row_bytes as usize)?;
    if !cols.compressed.present && !cols.gzip.present && !cols.uncompressed.present {
        return Err(invalid("FITS: compressed image has no data column"));
    }
    r.seek(SeekFrom::Start(hdu.data_start))?;
    let mut table = vec![0u8; table_bytes as usize];
    r.read_exact(&mut table)?;
    let heap = hdu.data_start + h.int("THEAP").map(|t| t.max(0) as u64).unwrap_or(table_bytes);

    // 2. The tiling.
    let params = parse_params(h, spec, &cols)?;
    let (w, ht) = (spec.width as usize, spec.height as usize);
    let tile_w = h.indexed("ZTILE", 1).filter(|&t| t > 0).unwrap_or(w as i64) as usize;
    let tile_h = h.indexed("ZTILE", 2).filter(|&t| t > 0).unwrap_or(1) as usize;
    let tile_3 = h.indexed("ZTILE", 3).unwrap_or(1);
    if tile_3 > 1 || h.indexed("ZTILE", 4).is_some_and(|t| t > 1) {
        return Err(unsupported("FITS tiles spanning several planes"));
    }
    let tiles_x = w.div_ceil(tile_w.max(1));
    let tiles_y = ht.div_ceil(tile_h.max(1));
    let per_plane = tiles_x.checked_mul(tiles_y).ok_or_else(|| too_large("tiles"))?;
    let planes = planes.min(spec.planes as usize).max(1);
    let total = per_plane.checked_mul(planes).ok_or_else(|| too_large("tiles"))?;
    if (total as u64) > rows && per_plane as u64 * planes as u64 > rows {
        return Err(invalid("FITS: fewer table rows than tiles"));
    }
    let f = factor.max(1) as usize;
    let mut sinks: Vec<Sink> = (0..planes).map(|_| Sink::new(w, ht, f)).collect();

    // 3. Decode in batches: read a run of tiles' bytes, decode them in
    //    parallel, put them in place.
    let mut failure: Option<TextureError> = None;
    let mut decoded_any = false;
    let mut next = 0usize;
    while next < total {
        let mut jobs: Vec<Job> = Vec::new();
        let mut bytes = 0u64;
        let mut pixels = 0usize;
        while next < total && bytes < BATCH_BYTES && pixels < BATCH_PIXELS && jobs.len() < 4096 {
            let plane = next / per_plane;
            let within = next % per_plane;
            let (tx, ty) = (within % tiles_x, within / tiles_x);
            let (x0, y0) = (tx * tile_w, ty * tile_h);
            let row = &table[next * row_bytes as usize..(next + 1) * row_bytes as usize];
            let mut job = Job {
                plane,
                x0,
                y0,
                w: tile_w.min(w - x0),
                h: tile_h.min(ht - y0),
                number: next as i64 + 1,
                compressed: Vec::new(),
                gzip: Vec::new(),
                uncompressed: Vec::new(),
                scale: row_f64(row, &cols.scale),
                zero: row_f64(row, &cols.zero),
                blank: row_i32(row, &cols.blank),
            };
            for (col, target) in [(&cols.compressed, 0), (&cols.gzip, 1), (&cols.uncompressed, 2)] {
                let Some((count, offset)) = descriptor(row, col) else { continue };
                let size = count.saturating_mul(col.elem.max(1) as u64);
                bytes += size;
                let data = read_heap(r, heap, offset, size, len);
                match target {
                    0 => job.compressed = data,
                    1 => job.gzip = data,
                    _ => job.uncompressed = data,
                }
            }
            pixels += job.w * job.h;
            jobs.push(job);
            next += 1;
        }
        let decoded: Vec<(Job, Result<Vec<f32>, TextureError>)> = jobs
            .into_par_iter()
            .map(|job| {
                let result = decode_tile(&job, &params, &cols);
                (job, result)
            })
            .collect();
        for (job, result) in decoded {
            match result {
                Ok(tile) => {
                    decoded_any = true;
                    sinks[job.plane].put(&job, &tile);
                }
                Err(e) => {
                    failure.get_or_insert(e);
                }
            }
        }
    }
    if !decoded_any && let Some(e) = failure {
        return Err(e);
    }
    let (ow, oh) = (w.div_ceil(f) as u32, ht.div_ceil(f) as u32);
    Ok(sinks.into_iter().map(|s| Plane { width: ow, height: oh, values: s.finish() }).collect())
}

fn be_u64(b: &[u8]) -> u64 {
    b.iter().fold(0u64, |acc, &x| (acc << 8) | x as u64)
}

/// The (element count, heap offset) a variable-length column holds in a row.
fn descriptor(row: &[u8], col: &Column) -> Option<(u64, u64)> {
    if !col.present || col.elem == 0 {
        return None;
    }
    if col.wide {
        let d = row.get(col.offset..col.offset + 16)?;
        Some((be_u64(&d[..8]), be_u64(&d[8..])))
    } else {
        let d = row.get(col.offset..col.offset + 8)?;
        Some((be_u64(&d[..4]), be_u64(&d[4..])))
    }
}

fn row_f64(row: &[u8], col: &Column) -> Option<f64> {
    if !col.present {
        return None;
    }
    let b = row.get(col.offset..col.offset + 8)?;
    Some(f64::from_be_bytes(b.try_into().ok()?))
}

fn row_i32(row: &[u8], col: &Column) -> Option<i64> {
    if !col.present {
        return None;
    }
    let b = row.get(col.offset..col.offset + 4)?;
    Some(i32::from_be_bytes(b.try_into().ok()?) as i64)
}

/// Reads `size` bytes of the heap; an unreadable range (a truncated file)
/// gives no data, which decodes as an undefined tile.
fn read_heap(r: &mut dyn Stream, heap: u64, offset: u64, size: u64, file_len: u64) -> Vec<u8> {
    let start = heap.saturating_add(offset);
    if size == 0 || start.saturating_add(size) > file_len || size > 1 << 30 {
        return Vec::new();
    }
    let mut buf = vec![0u8; size as usize];
    if r.seek(SeekFrom::Start(start)).is_err() || read_fully(r, &mut buf).map_or(true, |n| n < buf.len()) {
        return Vec::new();
    }
    buf
}

//endregion

/*┌─────────────────╮
| One tile        |
└─────────────────╯*/
//region One tile

/// Decodes one tile to physical values.
fn decode_tile(job: &Job, p: &Params, cols: &Columns) -> Result<Vec<f32>, TextureError> {
    let n = job.w * job.h;
    let stored = job.compressed.len() + job.gzip.len() + job.uncompressed.len();
    if n > MAX_TILE_PIXELS || (n > FREE_PIXELS && n / MAX_RATIO > stored) {
        return Err(too_large(format!("a tile of {n} pixels stored in {stored} bytes")));
    }
    let mut out = vec![f32::NAN; n];
    let blank = job.blank.or(p.blank);
    // 1. A tile stored uncompressed or as raw gzipped floats.
    if job.compressed.is_empty() {
        let raw = if !job.gzip.is_empty() {
            gunzip(&job.gzip, n * 8)?
        } else if !job.uncompressed.is_empty() {
            job.uncompressed.clone()
        } else {
            // No data: undefined pixels.
            return Ok(out);
        };
        let width = p.bitpix.unsigned_abs() as usize / 8;
        read_raw(&raw, width, p, blank, &mut out)?;
        return Ok(out);
    }
    // 2. Compressed data.
    let ints: Vec<i32>;
    let data: &[u8] = &job.compressed;
    let elem_bytes = if p.quantised { 4 } else { p.bitpix.unsigned_abs() as usize / 8 };
    match p.algorithm {
        Algorithm::Rice => {
            ints = rice::decode(data, n, p.block, p.bytepix)?;
        }
        Algorithm::Plio => {
            if !data.len().is_multiple_of(2) {
                return Err(invalid("PLIO: odd byte count"));
            }
            let words: Vec<i16> = data.chunks_exact(2).map(|b| i16::from_be_bytes([b[0], b[1]])).collect();
            ints = plio::decode(&words, n)?;
        }
        Algorithm::Hcompress => {
            ints = hcompress::decode(data, job.w, job.h, p.hscale, p.smooth)?;
        }
        Algorithm::Gzip1 | Algorithm::Gzip2 => {
            let mut raw = gunzip(data, n * 8)?;
            if p.algorithm == Algorithm::Gzip2 {
                raw = unshuffle(&raw, elem_bytes);
            }
            if !p.quantised {
                read_raw(&raw, elem_bytes, p, blank, &mut out)?;
                return Ok(out);
            }
            ints = raw.chunks_exact(4).map(|b| i32::from_be_bytes([b[0], b[1], b[2], b[3]])).collect();
        }
        Algorithm::None => {
            if !p.quantised {
                read_raw(data, elem_bytes, p, blank, &mut out)?;
                return Ok(out);
            }
            ints = data.chunks_exact(4).map(|b| i32::from_be_bytes([b[0], b[1], b[2], b[3]])).collect();
        }
    }
    if ints.len() < n {
        return Err(invalid("FITS: tile decoded to too few pixels"));
    }
    // 3. Integers to physical values.
    if p.quantised {
        let scale = job.scale.or(p.scale).unwrap_or(1.0);
        let zero = job.zero.or(p.zero).unwrap_or(0.0);
        let null = blank.unwrap_or(NULL_VALUE as i64);
        let random = random_table();
        let mut seed = ((job.number - 1 + p.dither_seed - 1).rem_euclid(N_RANDOM as i64)) as usize;
        let mut next = (random[seed] * 500.0) as usize;
        for (o, &v) in out.iter_mut().zip(&ints) {
            let dithered = p.dither != Dither::None;
            *o = if v as i64 == null {
                f32::NAN
            } else if p.dither == Dither::Two && v == ZERO_VALUE {
                0.0
            } else if dithered {
                ((v as f64 - random[next] as f64 + 0.5) * scale + zero) as f32
            } else {
                (v as f64 * scale + zero) as f32
            };
            if dithered {
                next += 1;
                if next == N_RANDOM {
                    seed = (seed + 1) % N_RANDOM;
                    next = (random[seed] * 500.0) as usize;
                }
            }
        }
        // NaN pixels in the data column's source are NULLs.
        let _ = cols;
    } else {
        for (o, &v) in out.iter_mut().zip(&ints) {
            *o = if blank == Some(v as i64) { f32::NAN } else { (p.bzero + p.bscale * v as f64) as f32 };
        }
    }
    Ok(out)
}

/// Raw big-endian samples of `width` bytes to physical values.
fn read_raw(raw: &[u8], width: usize, p: &Params, blank: Option<i64>, out: &mut [f32]) -> Result<(), TextureError> {
    if raw.len() < out.len() * width {
        return Err(invalid("FITS: tile data too short"));
    }
    let physical = |v: f64| (p.bzero + p.bscale * v) as f32;
    for (o, b) in out.iter_mut().zip(raw.chunks_exact(width)) {
        *o = match (p.bitpix, width) {
            (8, 1) => physical(b[0] as f64),
            (16, 2) => physical(i16::from_be_bytes([b[0], b[1]]) as f64),
            (32, 4) => physical(i32::from_be_bytes([b[0], b[1], b[2], b[3]]) as f64),
            (64, 8) => physical(i64::from_be_bytes(b.try_into().expect("8 bytes")) as f64),
            (-32, 4) => physical(f32::from_be_bytes([b[0], b[1], b[2], b[3]]) as f64),
            (-64, 8) => physical(f64::from_be_bytes(b.try_into().expect("8 bytes"))),
            _ => return Err(invalid("FITS: tile sample size mismatch")),
        };
        if p.bitpix > 0 && blank.is_some() {
            let stored = match width {
                1 => b[0] as i64,
                2 => i16::from_be_bytes([b[0], b[1]]) as i64,
                4 => i32::from_be_bytes([b[0], b[1], b[2], b[3]]) as i64,
                _ => i64::from_be_bytes(b.try_into().expect("8 bytes")),
            };
            if Some(stored) == blank {
                *o = f32::NAN;
            }
        }
        if p.float && !o.is_finite() {
            *o = f32::NAN;
        }
    }
    Ok(())
}

/// Undoes GZIP_2's byte shuffling: the file lists every pixel's first byte,
/// then every second byte, and so on.
fn unshuffle(raw: &[u8], width: usize) -> Vec<u8> {
    if width <= 1 {
        return raw.to_vec();
    }
    let n = raw.len() / width;
    let mut out = vec![0u8; n * width];
    for b in 0..width {
        for i in 0..n {
            out[i * width + b] = raw[b * n + i];
        }
    }
    out
}

//endregion

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_dither_table_matches_cfitsio() {
        // cfitsio documents that the 10,000th value of its generator's
        // seed sequence is 1043618065.
        let t = random_table();
        assert_eq!(t.len(), N_RANDOM);
        let mut seed = 1.0f64;
        for _ in 0..N_RANDOM {
            let x = 16807.0 * seed;
            seed = x - 2147483647.0 * (x / 2147483647.0).trunc();
        }
        assert_eq!(seed as i64, 1043618065);
        assert!(t.iter().all(|v| (0.0..1.0).contains(v)));
    }

    #[test]
    fn unshuffle_restores_pixels() {
        // Two 4-byte pixels 01020304 and 05060708, shuffled by byte plane.
        let shuffled = [1, 5, 2, 6, 3, 7, 4, 8];
        assert_eq!(unshuffle(&shuffled, 4), vec![1, 2, 3, 4, 5, 6, 7, 8]);
        assert_eq!(unshuffle(&[1, 2, 3], 1), vec![1, 2, 3]);
    }

    #[test]
    fn variable_length_forms() {
        let mut h = Header::default();
        let mut add = |k: &str, v: Value| {
            h.cards.push(super::super::header::Card { key: k.into(), value: v, comment: String::new() })
        };
        add("TFIELDS", Value::Int(3));
        add("TTYPE1", Value::Str("COMPRESSED_DATA".into()));
        add("TFORM1", Value::Str("1PB(100)".into()));
        add("TTYPE2", Value::Str("ZSCALE".into()));
        add("TFORM2", Value::Str("1D".into()));
        add("TTYPE3", Value::Str("GZIP_COMPRESSED_DATA".into()));
        add("TFORM3", Value::Str("1QB".into()));
        let cols = parse_columns(&h, 8 + 8 + 16).unwrap();
        assert_eq!((cols.compressed.offset, cols.compressed.elem, cols.compressed.wide), (0, 1, false));
        assert_eq!(cols.scale.offset, 8);
        assert_eq!((cols.gzip.offset, cols.gzip.wide), (16, true));
        assert!(parse_columns(&h, 10).is_err());
    }
}
