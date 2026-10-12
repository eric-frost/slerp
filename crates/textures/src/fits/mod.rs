//! FITS (Flexible Image Transport System), the format astronomy keeps its
//! images in: 80-character header cards in 2880-byte blocks, then big-endian
//! integers or floats, bottom row first.
//!
//! What is read:
//!
//! - The first image in the file.  Many files keep an empty primary unit
//!   and the picture in an extension, so the image is the first unit with
//!   two or more axes; the other images are the file's parts.
//! - 8, 16, 32 and 64-bit integers and 32 and 64-bit floats, with
//!   `BZERO` and `BSCALE` (that is how unsigned 16-bit data is stored) and
//!   `BLANK`.  Values stay in the file's physical units; the picture is
//!   stretched for display (see [`crate::stretch`]).
//! - Three planes as red, green and blue; any other number of planes as
//!   the layers of one texture.
//! - One-shot-colour cameras' mosaics (`BAYERPAT`), demosaiced.
//! - Tile-compressed images (`ZIMAGE`: Rice, gzip, PLIO and HCOMPRESS, with
//!   the quantisation and dithering of floating-point images).
//!
//! Thumbnails never hold the whole frame: rows are read in bands and
//! averaged down as they arrive.

mod hcompress;
mod header;
mod plio;
mod rice;
mod tile;

use crate::bayer::{self, BayerReducer, Pattern};
use crate::formats::channel_tag;
use crate::reduce::{Fill, Raster, Reducer, factor_for, rows_per_read};
use crate::thumb::Channels;
use crate::util::{MAX_DECODED_BYTES, MAX_DIMENSION, Stream, invalid, read_fully, too_large, unsupported};
use crate::{ColorSpace, DecodeOptions, Level, Pixels, SourceInfo, Texture, TextureError, TextureKind};
use header::{BLOCK, Header, Value};
use rayon::prelude::*;
use std::io::SeekFrom;

/// Most header units read from one file.
const MAX_HDUS: usize = 4096;

/// Most header cards listed in the inspector.
const MAX_LISTED_CARDS: usize = 600;

/// Most pixels (planes included) a texture holds before the picture is
/// reduced to fit, so a huge frame still opens.
const MAX_TEXTURE_BYTES: u64 = MAX_DECODED_BYTES / 2;

/// FITS files start with this card.
pub(crate) fn sniff(head: &[u8]) -> bool {
    head.starts_with(b"SIMPLE  =")
}

/*┌─────────────────╮
| Header units    |
└─────────────────╯*/
//region Header units

/// How an image's samples are stored.
#[derive(Clone, Debug)]
pub(crate) struct Spec {
    pub bitpix: i32,
    pub width: u32,
    pub height: u32,
    /// The product of all axes after the second.
    pub planes: u64,
    pub bzero: f64,
    pub bscale: f64,
    pub blank: Option<i64>,
    /// Set for tile-compressed images, whose pixels are in a binary table.
    pub compressed: bool,
}

impl Spec {
    fn bytes_per_sample(&self) -> usize {
        self.bitpix.unsigned_abs() as usize / 8
    }
}

/// One header and data unit.
pub(crate) struct Hdu {
    pub index: usize,
    pub header: Header,
    pub data_start: u64,
    pub image: Option<Spec>,
}

/// Reads the header of every unit in the file, skipping their data.  The
/// scan stops quietly at the first unreadable header after the first unit.
pub(crate) fn scan(r: &mut dyn Stream, len: u64) -> Result<Vec<Hdu>, TextureError> {
    let mut hdus: Vec<Hdu> = Vec::new();
    let mut at = 0u64;
    while at + BLOCK <= len && hdus.len() < MAX_HDUS {
        let (header, data_start) = match header::read(r, at, len) {
            Ok(h) => h,
            Err(e) if hdus.is_empty() => return Err(e),
            Err(_) => break,
        };
        let primary = hdus.is_empty();
        if primary && header.get("SIMPLE").is_none() {
            return Err(invalid("FITS: no SIMPLE card"));
        }
        let (image, data_len) = describe(&header, primary)?;
        let padded = data_len.div_ceil(BLOCK).checked_mul(BLOCK).ok_or_else(|| invalid("FITS: data size overflows"))?;
        let index = hdus.len();
        hdus.push(Hdu { index, header, data_start, image });
        at = match data_start.checked_add(padded) {
            Some(next) => next,
            None => break,
        };
    }
    if hdus.is_empty() {
        return Err(invalid("FITS: no header"));
    }
    Ok(hdus)
}

/// The image a unit holds, if it holds one, and the size of its data.
fn describe(h: &Header, primary: bool) -> Result<(Option<Spec>, u64), TextureError> {
    let bitpix = h.int("BITPIX").ok_or_else(|| invalid("FITS: no BITPIX"))?;
    if !matches!(bitpix, 8 | 16 | 32 | 64 | -32 | -64) {
        return Err(invalid(format!("FITS: BITPIX {bitpix}")));
    }
    let naxis = h.int("NAXIS").ok_or_else(|| invalid("FITS: no NAXIS"))?;
    if !(0..=999).contains(&naxis) {
        return Err(invalid(format!("FITS: NAXIS {naxis}")));
    }
    let mut axes = Vec::new();
    for n in 1..=naxis as usize {
        let a = h.indexed("NAXIS", n).ok_or_else(|| invalid(format!("FITS: no NAXIS{n}")))?;
        if a < 0 {
            return Err(invalid(format!("FITS: NAXIS{n} {a}")));
        }
        axes.push(a as u64);
    }
    let pcount = h.int("PCOUNT").unwrap_or(0).max(0) as u128;
    let gcount = h.int("GCOUNT").unwrap_or(1).max(0) as u128;
    // Random groups store a leading zero axis; they hold no picture.
    let random_groups = primary && axes.first() == Some(&0) && h.flag("GROUPS") == Some(true);
    let elements: u128 = if naxis == 0 || random_groups {
        0
    } else {
        axes.iter().try_fold(1u128, |acc, &a| acc.checked_mul(a as u128)).ok_or_else(|| invalid("FITS: huge image"))?
    };
    let bytes = (bitpix.unsigned_abs() as u128 / 8)
        .checked_mul(gcount)
        .and_then(|b| b.checked_mul(pcount.checked_add(elements)?))
        .filter(|&b| b < 1 << 62)
        .ok_or_else(|| invalid("FITS: data size overflows"))?;
    let data_len = bytes as u64;
    let xtension = h.string("XTENSION").unwrap_or("");
    let is_image_unit = primary || xtension == "IMAGE";
    if is_image_unit {
        let spec = if naxis >= 2 && elements > 0 && !random_groups {
            let planes = axes[2..]
                .iter()
                .try_fold(1u64, |acc, &a| acc.checked_mul(a))
                .ok_or_else(|| invalid("FITS: huge image"))?;
            Some(spec_for(h, bitpix as i32, axes[0], axes[1], planes, false))
        } else {
            None
        };
        return Ok((spec, data_len));
    }
    if xtension == "BINTABLE" && h.flag("ZIMAGE") == Some(true) {
        let zbitpix = h.int("ZBITPIX").ok_or_else(|| invalid("FITS: no ZBITPIX"))?;
        if !matches!(zbitpix, 8 | 16 | 32 | 64 | -32 | -64) {
            return Err(invalid(format!("FITS: ZBITPIX {zbitpix}")));
        }
        let znaxis = h.int("ZNAXIS").unwrap_or(0).clamp(0, 99) as usize;
        let mut zaxes = Vec::new();
        for n in 1..=znaxis {
            let a = h.indexed("ZNAXIS", n).filter(|a| *a >= 0).ok_or_else(|| invalid(format!("FITS: no ZNAXIS{n}")))?;
            zaxes.push(a as u64);
        }
        let spec = if znaxis >= 2 && zaxes.iter().all(|&a| a > 0) {
            let planes = zaxes[2..]
                .iter()
                .try_fold(1u64, |acc, &a| acc.checked_mul(a))
                .ok_or_else(|| invalid("FITS: huge image"))?;
            Some(spec_for(h, zbitpix as i32, zaxes[0], zaxes[1], planes, true))
        } else {
            None
        };
        return Ok((spec, data_len));
    }
    Ok((None, data_len))
}

fn spec_for(h: &Header, bitpix: i32, width: u64, height: u64, planes: u64, compressed: bool) -> Spec {
    Spec {
        bitpix,
        width: width.min(u32::MAX as u64) as u32,
        height: height.min(u32::MAX as u64) as u32,
        planes,
        bzero: h.float("BZERO").unwrap_or(0.0),
        bscale: h.float("BSCALE").filter(|s| *s != 0.0).unwrap_or(1.0),
        blank: if compressed { h.int("ZBLANK").or_else(|| h.int("BLANK")) } else { h.int("BLANK") },
        compressed,
    }
}

//endregion

/*┌─────────────────╮
| Parts           |
└─────────────────╯*/
//region Parts

/// Names the images of a file that holds several, in file order, for
/// pickers.  Empty when there is at most one.  A name starts with the unit's
/// number, which [`decode`] reads back.
pub(crate) fn parts(r: &mut dyn Stream, len: u64) -> Vec<String> {
    let Ok(hdus) = scan(r, len) else { return Vec::new() };
    let images: Vec<&Hdu> = hdus.iter().filter(|h| h.image.is_some()).collect();
    if images.len() < 2 {
        return Vec::new();
    }
    images
        .iter()
        .map(|h| {
            let spec = h.image.as_ref().expect("filtered on images");
            let label = h.header.string("EXTNAME").or(h.header.string("OBJECT")).unwrap_or("image");
            let version = h.header.int("EXTVER").map(|v| format!(" {v}")).unwrap_or_default();
            let planes = if spec.planes > 1 { format!("x{}", spec.planes) } else { String::new() };
            format!("{}: {label}{version} ({}x{}{planes})", h.index, spec.width, spec.height)
        })
        .collect()
}

fn unit_of(part: &str) -> Option<usize> {
    part.split(':').next()?.trim().parse().ok()
}

//endregion

/*┌─────────────────╮
| Decoding        |
└─────────────────╯*/
//region Decoding

/// Decodes the first image of a file, or the unit a part name from [`parts`]
/// picks.
pub(crate) fn decode(
    r: &mut dyn Stream,
    len: u64,
    opts: &DecodeOptions,
    part: Option<&str>,
) -> Result<Texture, TextureError> {
    // 1. Find the unit.
    let hdus = scan(r, len)?;
    let hdu = match part.and_then(unit_of) {
        Some(i) => hdus.get(i).filter(|h| h.image.is_some()).ok_or_else(|| invalid("FITS: no such image"))?,
        None => hdus.iter().find(|h| h.image.is_some()).ok_or_else(|| {
            unsupported("FITS file holds no two-dimensional image (spectra and tables are not shown)")
        })?,
    };
    let spec = hdu.image.as_ref().expect("picked an image unit");
    if spec.width > MAX_DIMENSION * 4 || spec.height > MAX_DIMENSION * 4 {
        return Err(too_large(format!("{}x{}", spec.width, spec.height)));
    }

    // 2. How the picture is laid out: colour planes, mosaic, row order.
    let primary = &hdus[0].header;
    let find = |key: &str| hdu.header.get(key).or_else(|| primary.get(key));
    let pattern_name = match find("BAYERPAT") {
        Some(Value::Str(s)) => Some(s.clone()),
        _ => None,
    };
    let x_off = find("XBAYROFF").and_then(Value::as_i64).unwrap_or(0);
    let y_off = find("YBAYROFF").and_then(Value::as_i64).unwrap_or(0);
    // Mosaics are demosaiced as they are read; tile-compressed ones (rare)
    // show as the grey mosaic they are.
    let bayer = if spec.planes == 1 && !spec.compressed {
        pattern_name.as_deref().and_then(|n| Pattern::parse(n, x_off, y_off))
    } else {
        None
    };
    let top_down = matches!(find("ROWORDER"), Some(Value::Str(s)) if s.eq_ignore_ascii_case("TOP-DOWN"));
    let planes_in_file = spec.planes;
    let colour = planes_in_file == 3;
    let wanted: u64 = if colour {
        3
    } else if opts.first_layer_only {
        1
    } else {
        planes_in_file
    };

    // 3. How much to shrink: what the caller asked for, and more if even
    //    one full-size surface would not fit.
    let mut factor = factor_for(spec.width, spec.height, opts);
    while spec.width.div_ceil(factor) as u64 * spec.height.div_ceil(factor) as u64 * 16 > MAX_TEXTURE_BYTES {
        factor += 1;
    }
    let (ow, oh) = (spec.width.div_ceil(factor), spec.height.div_ceil(factor));
    // A cube keeps as many planes as fit.
    let fit = (MAX_TEXTURE_BYTES / (ow as u64 * oh as u64 * 16)).max(1);
    let surfaces = if colour { 1 } else { wanted.min(fit).min(1 << 16) as usize };

    // 4. Read the pixels.  A file that holds under an eighth of what it
    //    declares is no partial download but a bad header, and allocating
    //    the whole picture for it would let a few bytes claim gigabytes.
    if !spec.compressed {
        let declared = (spec.width as u64 * spec.height as u64).saturating_mul(spec.bytes_per_sample() as u64);
        let wanted_planes = if colour { 3 } else { surfaces as u64 };
        let present = len.saturating_sub(hdu.data_start);
        if declared.saturating_mul(wanted_planes) / 8 > present {
            return Err(invalid("FITS: the file holds far less data than its header declares"));
        }
    }
    let mut rasters: Vec<Raster> = Vec::with_capacity(surfaces);
    if spec.compressed {
        let mut planes = tile::decode(r, len, hdu, spec, factor, if colour { 3 } else { surfaces })?;
        for chunk in planes.chunks_mut(if colour { 3 } else { 1 }) {
            let mut raster = Raster::new(ow, oh);
            for (p, plane) in chunk.iter_mut().enumerate() {
                let fill = if colour { [Fill::Red, Fill::Green, Fill::Blue][p] } else { Fill::Grey };
                place_plane(plane, ow, oh, !top_down, fill, &mut raster);
            }
            rasters.push(raster);
        }
    } else {
        for s in 0..surfaces {
            let mut raster = Raster::new(ow, oh);
            let plane_bytes = (spec.width as u64 * spec.height as u64).checked_mul(spec.bytes_per_sample() as u64);
            let plane_bytes = plane_bytes.ok_or_else(|| too_large("plane"))?;
            let planes_in_raster = if colour { 3 } else { 1 };
            for p in 0..planes_in_raster {
                let index = if colour { p } else { s } as u64;
                let start = hdu.data_start.saturating_add(index.saturating_mul(plane_bytes));
                let fill = if colour { [Fill::Red, Fill::Green, Fill::Blue][p] } else { Fill::Grey };
                read_plane(r, spec, start, factor, !top_down, fill, bayer.filter(|_| !colour), &mut raster)?;
            }
            rasters.push(raster);
        }
    }

    // 5. Make the texture.
    let has_alpha = rasters.iter().any(|ra| ra.rgba.chunks_exact(4).any(|p| p[3] < 1.0));
    let count = rasters.len() as u32;
    let mut format = describe_format(spec);
    if colour {
        format.push_str(", RGB");
    } else if let Some(p) = bayer {
        format.push_str(&format!(", {} mosaic", p.name()));
    }
    format.push_str(channel_tag(Channels::Stretch));
    let file_bytes = len;
    Ok(Texture {
        kind: TextureKind::D2,
        layers: count,
        faces: 1,
        levels: vec![Level {
            width: ow,
            height: oh,
            depth: 1,
            surfaces: rasters.into_iter().map(|ra| Pixels::Rgba32F(ra.rgba)).collect(),
        }],
        color_space: ColorSpace::Linear,
        has_alpha,
        source: SourceInfo {
            container: "FITS".into(),
            format,
            supercompression: spec.compressed.then(|| hdu.header.string("ZCMPTYPE").unwrap_or("tiles").to_string()),
            width: spec.width,
            height: spec.height,
            depth: 1,
            layers: if colour { 1 } else { planes_in_file.min(u32::MAX as u64) as u32 },
            faces: 1,
            mip_count: 1,
            file_bytes,
            header: listed_cards(&hdus, hdu),
            ..Default::default()
        },
    })
}

/// The sample type as astronomers name it.
fn describe_format(s: &Spec) -> String {
    let unsigned = s.bscale == 1.0
        && match s.bitpix {
            16 => s.bzero == 32768.0,
            32 => s.bzero == 2147483648.0,
            64 => s.bzero == 9223372036854775808.0,
            _ => false,
        };
    let kind = match s.bitpix {
        8 => "8-bit",
        16 if unsigned => "16-bit unsigned",
        16 => "16-bit integer",
        32 if unsigned => "32-bit unsigned",
        32 => "32-bit integer",
        64 if unsigned => "64-bit unsigned",
        64 => "64-bit integer",
        -32 => "32-bit float",
        _ => "64-bit float",
    };
    format!("{kind}{}", if s.compressed { ", tile-compressed" } else { "" })
}

/// The cards the inspector lists: the primary header's, then the unit's own
/// that are not repeats.  Structural cards are left out.
fn listed_cards(hdus: &[Hdu], hdu: &Hdu) -> Vec<(String, String)> {
    const STRUCTURAL: [&str; 8] = ["SIMPLE", "BITPIX", "EXTEND", "XTENSION", "PCOUNT", "GCOUNT", "END", "GROUPS"];
    let skip = |key: &str| {
        STRUCTURAL.contains(&key)
            || key.strip_prefix("NAXIS").is_some_and(|n| n.is_empty() || n.bytes().all(|b| b.is_ascii_digit()))
            || key.strip_prefix("TTYPE").is_some()
            || key.strip_prefix("TFORM").is_some()
            || key.strip_prefix("TDIM").is_some()
    };
    let mut out: Vec<(String, String)> = Vec::new();
    let mut seen: Vec<&str> = Vec::new();
    let units: Vec<&Hdu> = if hdu.index == 0 { vec![hdu] } else { vec![&hdus[0], hdu] };
    for unit in units {
        for card in &unit.header.cards {
            if out.len() >= MAX_LISTED_CARDS {
                return out;
            }
            if skip(&card.key) {
                continue;
            }
            if card.key == "COMMENT" || card.key == "HISTORY" || card.key.is_empty() {
                if !card.comment.is_empty() {
                    out.push((card.key.clone(), card.comment.clone()));
                }
                continue;
            }
            if unit.index != 0 && seen.contains(&card.key.as_str()) {
                out.retain(|(k, _)| *k != card.key);
            }
            let mut value = card.value.display();
            if !card.comment.is_empty() && !matches!(card.value, Value::None) {
                value.push_str(" / ");
                value.push_str(&card.comment);
            }
            out.push((card.key.clone(), value));
            if unit.index == 0 {
                seen.push(&card.key);
            }
        }
    }
    out
}

//endregion

/*┌─────────────────╮
| Reading samples |
└─────────────────╯*/
//region Reading samples

/// Turns big-endian stored samples into physical float values: `BZERO +
/// BSCALE * stored`, with `BLANK` and NaN as undefined (NaN).
struct Convert {
    bitpix: i32,
    bzero: f64,
    bscale: f64,
    blank: Option<i64>,
    /// For 8 and 16-bit data every possible stored value, so a pixel costs
    /// one table lookup.
    table: Vec<f32>,
}

impl Convert {
    fn new(spec: &Spec) -> Convert {
        let (bzero, bscale, blank) = (spec.bzero, spec.bscale, spec.blank);
        let physical = |stored: i64| -> f32 {
            if blank == Some(stored) { f32::NAN } else { (bzero + bscale * stored as f64) as f32 }
        };
        let table = match spec.bitpix {
            8 => (0..256).map(physical).collect(),
            16 => (0..65536u32).map(|v| physical(v as u16 as i16 as i64)).collect(),
            _ => Vec::new(),
        };
        Convert { bitpix: spec.bitpix, bzero, bscale, blank, table }
    }

    /// Converts `raw` (whole samples) into `out`.
    fn run(&self, raw: &[u8], out: &mut [f32]) {
        const CHUNK: usize = 1 << 16;
        let width = self.bitpix.unsigned_abs() as usize / 8;
        out.par_chunks_mut(CHUNK).zip(raw.par_chunks(CHUNK * width)).for_each(|(o, r)| match self.bitpix {
            8 => o.iter_mut().zip(r).for_each(|(o, &b)| *o = self.table[b as usize]),
            16 => o.iter_mut().zip(r.chunks_exact(2)).for_each(|(o, b)| {
                *o = self.table[u16::from_be_bytes([b[0], b[1]]) as usize];
            }),
            32 => o.iter_mut().zip(r.chunks_exact(4)).for_each(|(o, b)| {
                let v = i32::from_be_bytes([b[0], b[1], b[2], b[3]]);
                *o = if self.blank == Some(v as i64) { f32::NAN } else { (self.bzero + self.bscale * v as f64) as f32 };
            }),
            64 => o.iter_mut().zip(r.chunks_exact(8)).for_each(|(o, b)| {
                let v = i64::from_be_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]);
                *o = if self.blank == Some(v) { f32::NAN } else { (self.bzero + self.bscale * v as f64) as f32 };
            }),
            -32 => o.iter_mut().zip(r.chunks_exact(4)).for_each(|(o, b)| {
                let v = f32::from_be_bytes([b[0], b[1], b[2], b[3]]);
                *o = if v.is_finite() { self.scale(v as f64) } else { f32::NAN };
            }),
            _ => o.iter_mut().zip(r.chunks_exact(8)).for_each(|(o, b)| {
                let v = f64::from_be_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]);
                *o = if v.is_finite() { self.scale(v) } else { f32::NAN };
            }),
        });
    }

    fn scale(&self, v: f64) -> f32 {
        if self.bzero == 0.0 && self.bscale == 1.0 { v as f32 } else { (self.bzero + self.bscale * v) as f32 }
    }
}

/// Reads one plane (`width` x `height` samples starting at `start`) in
/// pieces of `piece_rows` rows, calling `consume` with each piece's
/// physical values and row count.  Stops quietly when the file ends early
/// (a partial download), so cost follows the file's size and not what its
/// header claims.
fn for_each_piece(
    r: &mut dyn Stream,
    spec: &Spec,
    start: u64,
    piece_rows: usize,
    mut consume: impl FnMut(&[f32], usize),
) -> Result<(), TextureError> {
    let convert = Convert::new(spec);
    let width = spec.width as usize;
    let height = spec.height as usize;
    let sample = spec.bytes_per_sample();
    let row_bytes = width * sample;
    let piece_rows = piece_rows.clamp(1, height);
    let mut raw = vec![0u8; row_bytes * piece_rows];
    let mut values = vec![0f32; width * piece_rows];
    r.seek(SeekFrom::Start(start))?;
    let mut row = 0;
    while row < height {
        let rows = piece_rows.min(height - row);
        let want = rows * row_bytes;
        let got = read_fully(r, &mut raw[..want])?;
        // Whole rows only; a row cut off by the end of the file is dropped.
        let whole_rows = got / row_bytes;
        if whole_rows > 0 {
            convert.run(&raw[..whole_rows * row_bytes], &mut values[..whole_rows * width]);
            consume(&values[..whole_rows * width], whole_rows);
        }
        if got < want {
            break;
        }
        row += rows;
    }
    Ok(())
}

/// Reads one plane into the raster, shrinking by `factor`.  `flip` places
/// the file's first row at the bottom (FITS' own order).
#[allow(clippy::too_many_arguments)]
fn read_plane(
    r: &mut dyn Stream,
    spec: &Spec,
    start: u64,
    factor: u32,
    flip: bool,
    fill: Fill,
    bayer: Option<Pattern>,
    out: &mut Raster,
) -> Result<(), TextureError> {
    let (w, h) = (spec.width, spec.height);
    let row_bytes = w as usize * spec.bytes_per_sample();
    match bayer {
        Some(pattern) if factor >= 2 => {
            let mut red = BayerReducer::new(w, h, factor, flip, pattern);
            let piece = rows_per_read(row_bytes, red.factor());
            for_each_piece(r, spec, start, piece, |rows, n| red.push_rows(rows, n, out))?;
            red.finish(out);
            Ok(())
        }
        Some(pattern) => {
            let mut mosaic: Vec<f32> = Vec::with_capacity(w as usize * h as usize);
            for_each_piece(r, spec, start, rows_per_read(row_bytes, 256), |rows, _| mosaic.extend_from_slice(rows))?;
            // A file that ended early leaves the lower rows undefined.
            mosaic.resize(w as usize * h as usize, f32::NAN);
            upright(&mut mosaic, w as usize, flip);
            bayer::demosaic(&mosaic, w as usize, h as usize, &pattern, out);
            Ok(())
        }
        None => {
            let mut red = Reducer::new(w, h, factor, flip);
            let piece = rows_per_read(row_bytes, red.factor());
            for_each_piece(r, spec, start, piece, |rows, n| red.push_rows(rows, n, out, fill))?;
            red.finish(out);
            Ok(())
        }
    }
}

/// Reverses the order of the rows when the file stores the bottom first.
fn upright(samples: &mut [f32], width: usize, flip: bool) {
    if !flip || width == 0 {
        return;
    }
    let rows = samples.len() / width;
    for y in 0..rows / 2 {
        let (a, b) = samples.split_at_mut((rows - 1 - y) * width);
        a[y * width..(y + 1) * width].swap_with_slice(&mut b[..width]);
    }
}

/// Puts a decoded plane (rows in file order, values with NaN for undefined)
/// into a raster, flipping rows as `flip` asks.
fn place_plane(plane: &mut tile::Plane, ow: u32, oh: u32, flip: bool, fill: Fill, out: &mut Raster) {
    debug_assert_eq!((plane.width, plane.height), (ow, oh));
    let w = ow as usize;
    for y in 0..oh as usize {
        let out_y = if flip { oh as usize - 1 - y } else { y };
        let src = &plane.values[y * w..(y + 1) * w];
        let dst = &mut out.rgba[out_y * w * 4..(out_y + 1) * w * 4];
        for (v, px) in src.iter().zip(dst.chunks_exact_mut(4)) {
            if v.is_nan() {
                px[3] = 0.0;
            } else {
                match fill {
                    Fill::Grey => px[..3].fill(*v),
                    Fill::Red => px[0] = *v,
                    Fill::Green => px[1] = *v,
                    Fill::Blue => px[2] = *v,
                    Fill::Alpha => px[3] = *v,
                }
            }
        }
    }
}

//endregion
