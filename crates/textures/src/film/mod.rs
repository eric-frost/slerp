//! Film scans: DPX (SMPTE 268M, "Digital Picture Exchange") and Kodak's
//! Cineon, the formats scanners, film recorders and compositing programs
//! trade frames in.  A frame is one file; a scan runs to 100 MB or more.
//!
//! The two headers differ (see `dpx` and `cineon`), but both describe the
//! same thing: one or more *elements* of samples (red, green and blue
//! together or apart, luma, chroma, alpha), each with its bit depth,
//! packing and transfer characteristic.  This module reads those elements
//! row by row, converts samples to linear light, and shrinks the picture
//! while it reads (see [`crate::reduce`]), so a thumbnail never holds the
//! whole frame.
//!
//! What the samples mean decides how the picture is shown:
//!
//! - Printing density and logarithmic scans (Cineon) go through the usual
//!   film curve (reference black 95, reference white 685 on a 0..1023
//!   scale, 0.6 negative gamma) to linear light, with a soft shoulder above
//!   white, and then to sRGB: the picture as the scan was meant to print.
//! - Linear data stays linear (floating point), so the viewer's exposure
//!   control and the HDR thumbnail path apply.
//! - Everything else (video transfers, user-defined, unspecified) is taken
//!   as display-referred.

mod cineon;
mod dpx;

use crate::color::{linear_to_srgb8, srgb_to_linear};
use crate::formats::channel_tag;
use crate::reduce::{Fill, Raster, Reducer, factor_for, rows_per_read};
use crate::util::{Stream, invalid, read_fully, unsupported};
use crate::{ColorSpace, DecodeOptions, SourceInfo, Texture, TextureError};
use rayon::prelude::*;
use std::io::SeekFrom;

pub(crate) use cineon::{decode as decode_cineon, sniff as sniff_cineon};
pub(crate) use dpx::{decode as decode_dpx, sniff as sniff_dpx};

/// What the codes of an element measure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Transfer {
    /// Gamma-encoded picture values (video transfers, unspecified).
    Display,
    /// Printing density, as film scans store it.
    Log,
    /// Light, linearly.
    Linear,
}

/// How samples are packed into the file's bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Packing {
    /// 8 and 16-bit samples, and floats: one after another.
    Plain,
    /// 10 and 12-bit samples as one continuous bit stream, filled from the
    /// low bit of each 32-bit word.
    Packed,
    /// Samples in 32-bit words (three 10-bit) or 16-bit units (one 12-bit),
    /// with the spare bits at the low end (method A) ...
    FilledA,
    /// ... or at the high end (method B).
    FilledB,
}

/// What the samples of one pixel are, in file order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Layout {
    Gray,
    Red,
    Green,
    Blue,
    Alpha,
    Rgb,
    Rgba,
    Abgr,
    /// 4:4:4 chroma-luma-chroma.
    YCbCr,
    /// 4:4:4 with alpha.
    YCbCrA,
    /// 4:2:2: Cb Y Cr Y per two pixels.
    YCbCr422,
}

impl Layout {
    /// Samples per pixel.
    pub fn samples(self) -> usize {
        match self {
            Layout::Gray | Layout::Red | Layout::Green | Layout::Blue | Layout::Alpha => 1,
            Layout::Rgb | Layout::YCbCr => 3,
            Layout::Rgba | Layout::Abgr | Layout::YCbCrA => 4,
            // Two samples per pixel on average.
            Layout::YCbCr422 => 2,
        }
    }
}

/// A run of rows of samples in the file.
#[derive(Clone, Debug)]
pub(crate) struct Element {
    pub offset: u64,
    /// Bytes from the start of one row to the start of the next.
    pub row_stride: u64,
    pub layout: Layout,
    pub bits: u32,
    pub packing: Packing,
    pub transfer: Transfer,
    /// Reference codes for black and white, when the header gives usable
    /// ones.
    pub low: Option<f64>,
    pub high: Option<f64>,
    /// ITU-R BT.601 rather than 709 for chroma-luma elements.
    pub bt601: bool,
}

/// Everything a header says about a frame.
pub(crate) struct Frame {
    pub container: &'static str,
    pub width: usize,
    pub height: usize,
    pub big_endian: bool,
    /// The elements to read, in the order they fill red, green, blue and
    /// alpha when they are single channels.
    pub elements: Vec<Element>,
    /// Mirror the picture horizontally or vertically.
    pub flip_x: bool,
    pub flip_y: bool,
    /// The shape of a pixel, width over height, when the header says it is
    /// not square.
    pub aspect: Option<f64>,
    pub format: String,
    pub header: Vec<(String, String)>,
}

/*┌─────────────────╮
| Header helpers  |
└─────────────────╯*/
//region Header helpers

/// A cursor over a header block with the file's byte order.
pub(crate) struct Fields<'a> {
    pub data: &'a [u8],
    pub big: bool,
}

impl Fields<'_> {
    pub fn u8(&self, at: usize) -> u8 {
        self.data.get(at).copied().unwrap_or(0)
    }

    pub fn u16(&self, at: usize) -> u16 {
        let b = [self.u8(at), self.u8(at + 1)];
        if self.big { u16::from_be_bytes(b) } else { u16::from_le_bytes(b) }
    }

    pub fn u32(&self, at: usize) -> u32 {
        let b = [self.u8(at), self.u8(at + 1), self.u8(at + 2), self.u8(at + 3)];
        if self.big { u32::from_be_bytes(b) } else { u32::from_le_bytes(b) }
    }

    /// A float, or `None` for the undefined pattern (all ones, a NaN) or
    /// any other non-finite value.
    pub fn f32(&self, at: usize) -> Option<f32> {
        let v = f32::from_bits(self.u32(at));
        v.is_finite().then_some(v)
    }

    /// A fixed-size text field with its NUL padding and anything unprintable
    /// removed.
    pub fn text(&self, at: usize, len: usize) -> String {
        let bytes = self.data.get(at..(at + len).min(self.data.len())).unwrap_or(&[]);
        let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
        let s: String = bytes[..end].iter().map(|&b| if (32..127).contains(&b) { b as char } else { ' ' }).collect();
        s.trim().to_string()
    }

    /// An unsigned integer, or `None` for the undefined pattern.
    pub fn defined_u32(&self, at: usize) -> Option<u32> {
        let v = self.u32(at);
        (v != u32::MAX).then_some(v)
    }
}

pub(crate) fn push_text(out: &mut Vec<(String, String)>, name: &str, value: String) {
    if !value.is_empty() {
        out.push((name.to_string(), value));
    }
}

//endregion

/*┌─────────────────╮
| Conversions     |
└─────────────────╯*/
//region Conversions

/// Reference black and white of the printing density scale, in 10-bit
/// codes, and the negative's gamma per 0.002 of density.
const LOG_BLACK: f64 = 95.0;
const LOG_WHITE: f64 = 685.0;
const LOG_STEP: f64 = 0.002 / 0.6;

/// Where the soft shoulder starts, as a fraction of white.
const KNEE: f64 = 0.9;

/// Cineon's printing density to linear light with white at 1: a code of
/// 685 is 1.0 and 95 is 0.  Highlights above the knee roll off smoothly to
/// 1.0, as a print does, rather than clipping.
pub(crate) fn log_to_linear(code10: f64) -> f64 {
    let floor = 10f64.powf((LOG_BLACK - LOG_WHITE) * LOG_STEP);
    let lin = (10f64.powf((code10 - LOG_WHITE) * LOG_STEP) - floor) / (1.0 - floor);
    if lin <= KNEE { lin.max(0.0) } else { KNEE + (1.0 - KNEE) * (1.0 - (-(lin - KNEE) / (1.0 - KNEE)).exp()) }
}

/// A table from every code of `bits` to linear light.
fn build_lut(el: &Element, max_code: f64) -> Vec<f32> {
    (0..(1u64 << el.bits.min(16))).map(|c| to_linear(el, c as f64, max_code) as f32).collect()
}

/// Linear light for one sample code.
fn to_linear(el: &Element, code: f64, max_code: f64) -> f64 {
    match el.transfer {
        Transfer::Linear => code / max_code,
        Transfer::Log => log_to_linear(code / max_code * 1023.0),
        Transfer::Display => {
            let (low, high) = match (el.low, el.high) {
                (Some(l), Some(h)) if h > l && h <= max_code => (l, h),
                _ => (0.0, max_code),
            };
            srgb_to_linear((((code - low) / (high - low)).clamp(0.0, 1.0)) as f32) as f64
        }
    }
}

//endregion

/*┌─────────────────╮
| Reading rows    |
└─────────────────╯*/
//region Reading rows

/// Unpacks `n` rows of samples (`per_row` each) from `raw`, rows `stride`
/// bytes apart, into codes as floats (the float elements' own values).
fn unpack(el: &Element, big: bool, raw: &[u8], stride: usize, n: usize, per_row: usize, out: &mut [f32]) {
    out.par_chunks_mut(per_row).zip(raw.par_chunks(stride)).take(n).for_each(|(o, row)| unpack_row(el, big, row, o));
}

fn unpack_row(el: &Element, big: bool, row: &[u8], out: &mut [f32]) {
    let word = |at: usize| -> u32 {
        let b = [row[at], row[at + 1], row[at + 2], row[at + 3]];
        if big { u32::from_be_bytes(b) } else { u32::from_le_bytes(b) }
    };
    let half = |b: &[u8]| -> u32 {
        (if big { u16::from_be_bytes([b[0], b[1]]) } else { u16::from_le_bytes([b[0], b[1]]) }) as u32
    };
    match (el.bits, el.packing) {
        (8, _) => out.iter_mut().zip(row).for_each(|(o, &b)| *o = b as f32),
        (16, _) => out.iter_mut().zip(row.chunks_exact(2)).for_each(|(o, b)| *o = half(b) as f32),
        (32, _) => out.iter_mut().enumerate().for_each(|(i, o)| {
            *o = f32::from_bits(word(i * 4));
        }),
        (64, _) => out.iter_mut().enumerate().for_each(|(i, o)| {
            let b: [u8; 8] = row[i * 8..i * 8 + 8].try_into().expect("8 bytes");
            *o = (if big { f64::from_be_bytes(b) } else { f64::from_le_bytes(b) }) as f32;
        }),
        (10, Packing::FilledA | Packing::FilledB) => {
            // Three samples to a word, from the top (method A) or from the
            // second bit pair (method B).
            let top = if el.packing == Packing::FilledA { 22 } else { 20 };
            for (o, w) in out.chunks_mut(3).zip(row.chunks_exact(4)) {
                let w = if big {
                    u32::from_be_bytes([w[0], w[1], w[2], w[3]])
                } else {
                    u32::from_le_bytes([w[0], w[1], w[2], w[3]])
                };
                for (k, v) in o.iter_mut().enumerate() {
                    *v = ((w >> (top - 10 * k as u32)) & 0x3ff) as f32;
                }
            }
        }
        (12, Packing::FilledA) => out.iter_mut().zip(row.chunks_exact(2)).for_each(|(o, b)| *o = (half(b) >> 4) as f32),
        (12, Packing::FilledB) => {
            out.iter_mut().zip(row.chunks_exact(2)).for_each(|(o, b)| *o = (half(b) & 0xfff) as f32)
        }
        (bits @ (10 | 12), Packing::Packed | Packing::Plain) => {
            // A continuous stream filled from the low bit of each 32-bit
            // word upward, a sample carrying over into the next word.
            let bits = bits as u64;
            let mask = (1u64 << bits) - 1;
            let (mut acc, mut have, mut next) = (0u64, 0u64, 0usize);
            for o in out.iter_mut() {
                while have < bits {
                    acc |= (word(next * 4) as u64) << have;
                    next += 1;
                    have += 32;
                }
                *o = (acc & mask) as f32;
                acc >>= bits;
                have -= bits;
            }
        }
        _ => out.fill(0.0),
    }
}

/// Bytes of samples in a row of `samples` samples, rounded up to the
/// storage unit of the packing (a 32-bit word for 10 and 12-bit packed and
/// filled, as the formats pad them).
pub(crate) fn row_bytes(bits: u32, packing: Packing, samples: usize) -> usize {
    match (bits, packing) {
        (8, _) => samples,
        (16, _) => samples * 2,
        (32, _) => samples * 4,
        (64, _) => samples * 8,
        (10, Packing::FilledA | Packing::FilledB) => samples.div_ceil(3) * 4,
        (12, Packing::FilledA | Packing::FilledB) => samples * 2,
        (b, _) => (samples * b as usize).div_ceil(32) * 4,
    }
}

/// Samples in a row for an element, which for 4:2:2 is two per pixel.
fn samples_per_row(width: usize, layout: Layout) -> usize {
    match layout {
        Layout::YCbCr422 => width.div_ceil(2) * 4,
        l => width * l.samples(),
    }
}

/// The red, green, blue and alpha rows of `n` rows of one element, in
/// linear light, written into `out` (resized as needed and reused from one
/// piece to the next).  Elements that are one channel fill one of them; the
/// returned flags say which were filled.
fn channels_of(
    el: &Element,
    codes: &[f32],
    n: usize,
    width: usize,
    max_code: f64,
    lut: &[f32],
    out: &mut [Vec<f32>; 4],
) -> [bool; 4] {
    let per_row = samples_per_row(width, el.layout);
    let lin = |code: f32| -> f32 {
        if el.bits == 32 || el.bits == 64 {
            to_linear(el, code as f64 * max_code, max_code) as f32
        } else {
            lut[(code as usize).min(lut.len() - 1)]
        }
    };
    let px = n * width;
    for v in out.iter_mut() {
        v.resize(px, 0.0);
    }
    // One channel of every pixel, `step` samples apart starting at `first`,
    // through `convert`.
    fn take(
        dst: &mut [f32],
        codes: &[f32],
        per_row: usize,
        width: usize,
        first: usize,
        step: usize,
        convert: &(dyn Fn(f32) -> f32 + Sync),
    ) {
        dst.par_chunks_mut(width).enumerate().for_each(|(r, d)| {
            let row = &codes[r * per_row..(r + 1) * per_row];
            for (x, v) in d.iter_mut().enumerate() {
                *v = convert(row[first + x * step]);
            }
        });
    }
    // Opacity is a plain fraction of the largest code, whatever the colour
    // channels' transfer function.
    let alpha = |code: f32| if el.bits >= 32 { code } else { code / max_code as f32 };
    let mut got = [false; 4];
    let [r, g, b, a] = out;
    match el.layout {
        Layout::Gray | Layout::Red => {
            take(r, codes, per_row, width, 0, 1, &lin);
            got[0] = true;
        }
        Layout::Green => {
            take(g, codes, per_row, width, 0, 1, &lin);
            got[1] = true;
        }
        Layout::Blue => {
            take(b, codes, per_row, width, 0, 1, &lin);
            got[2] = true;
        }
        Layout::Alpha => {
            take(a, codes, per_row, width, 0, 1, &alpha);
            got[3] = true;
        }
        Layout::Rgb | Layout::Rgba | Layout::Abgr => {
            let step = el.layout.samples();
            let (ri, gi, bi, ai) = match el.layout {
                Layout::Rgb => (0, 1, 2, None),
                Layout::Rgba => (0, 1, 2, Some(3)),
                _ => (3, 2, 1, Some(0)),
            };
            take(r, codes, per_row, width, ri, step, &lin);
            take(g, codes, per_row, width, gi, step, &lin);
            take(b, codes, per_row, width, bi, step, &lin);
            got[..3].fill(true);
            if let Some(ai) = ai {
                take(a, codes, per_row, width, ai, step, &alpha);
                got[3] = true;
            }
        }
        Layout::YCbCr | Layout::YCbCrA | Layout::YCbCr422 => {
            let [yr, yg, yb] = ycbcr_rows(el, codes, n, width, per_row, max_code);
            (*r, *g, *b) = (yr, yg, yb);
            got[..3].fill(true);
            if el.layout == Layout::YCbCrA {
                take(a, codes, per_row, width, 3, 4, &alpha);
                got[3] = true;
            }
        }
    }
    got
}

/// Red, green and blue rows (linear light) of chroma-luma elements: 4:4:4
/// or 4:2:2, studio range when the header's reference black is above zero,
/// BT.709 or BT.601 coefficients.
fn ycbcr_rows(el: &Element, codes: &[f32], n: usize, width: usize, per_row: usize, max_code: f64) -> [Vec<f32>; 3] {
    let studio = el.low.is_some_and(|l| l > 0.0);
    let (kr, kb) = if el.bt601 { (0.299f32, 0.114f32) } else { (0.2126f32, 0.0722f32) };
    let scale = 1.0 / max_code as f32;
    let step = el.layout.samples();
    let mut rgb = [vec![0f32; n * width], vec![0f32; n * width], vec![0f32; n * width]];
    let [r, g, b] = &mut rgb;
    r.par_chunks_mut(width).zip(g.par_chunks_mut(width)).zip(b.par_chunks_mut(width)).enumerate().for_each(
        |(row_i, ((rr, gg), bb))| {
            let row = &codes[row_i * per_row..(row_i + 1) * per_row];
            for x in 0..width {
                // Cb Y Cr for 4:4:4; Cb Y0 Cr Y1 per pair for 4:2:2, where
                // the pair shares its chroma.
                let [cb, y, cr] = if el.layout == Layout::YCbCr422 {
                    let base = (x / 2) * 4;
                    [row[base], row[base + 1 + (x & 1) * 2], row[base + 2]]
                } else {
                    [row[x * step], row[x * step + 1], row[x * step + 2]]
                };
                let (y, pb, pr) = if studio {
                    // Studio range: luma 16..235, chroma 16..240 of 255.
                    (
                        (y * scale - 16.0 / 255.0) * (255.0 / 219.0),
                        (cb * scale - 128.0 / 255.0) * (255.0 / 224.0),
                        (cr * scale - 128.0 / 255.0) * (255.0 / 224.0),
                    )
                } else {
                    (y * scale, cb * scale - 0.5, cr * scale - 0.5)
                };
                let red = y + 2.0 * (1.0 - kr) * pr;
                let blue = y + 2.0 * (1.0 - kb) * pb;
                let green = (y - kr * red - kb * blue) / (1.0 - kr - kb);
                let linear = |e: f32| match el.transfer {
                    Transfer::Display => srgb_to_linear(e.clamp(0.0, 1.0)),
                    _ => e.max(0.0),
                };
                (rr[x], gg[x], bb[x]) = (linear(red), linear(green), linear(blue));
            }
        },
    );
    rgb
}

/*┌─────────────────╮
| Decoding        |
└─────────────────╯*/
//region Decoding

/// Decodes the frame's elements into a picture, shrunk as `opts` asks.
pub(crate) fn decode_frame(
    r: &mut dyn Stream,
    len: u64,
    frame: Frame,
    opts: &DecodeOptions,
) -> Result<Texture, TextureError> {
    let (w, h) = (frame.width, frame.height);
    let (w32, h32) = (w as u32, h as u32);
    let mut factor = factor_for(w32, h32, opts);
    let limit = crate::util::MAX_DECODED_BYTES / 2;
    while w32.div_ceil(factor) as u64 * h32.div_ceil(factor) as u64 * 16 > limit {
        factor += 1;
    }
    // A file with under an eighth of the samples its header declares is not
    // a partial download but a bad header; don't allocate for it.
    for el in &frame.elements {
        let per_row = row_bytes(el.bits, el.packing, samples_per_row(w, el.layout)) as u64;
        let declared = per_row.saturating_mul(h as u64);
        if declared / 8 > len.saturating_sub(el.offset) {
            return Err(invalid("the file holds far less data than its header declares"));
        }
    }
    let mut raster = Raster::new(w32.div_ceil(factor), h32.div_ceil(factor));
    let transfer = frame.elements.first().map_or(Transfer::Display, |e| e.transfer);
    let big = frame.big_endian;
    let mut has_alpha = false;

    for el in &frame.elements {
        let max_code = if el.bits >= 32 { 1.0 } else { ((1u64 << el.bits) - 1) as f64 };
        let lut = if el.bits <= 16 { build_lut(el, max_code) } else { Vec::new() };
        let samples = samples_per_row(w, el.layout);
        let bytes = row_bytes(el.bits, el.packing, samples);
        let stride = el.row_stride.max(bytes as u64) as usize;
        let mut reducers: [Option<Reducer>; 4] = [None, None, None, None];
        let mut fills = [Fill::Red, Fill::Green, Fill::Blue, Fill::Alpha];
        // Which channels this element supplies.
        let supplies: &[usize] = match el.layout {
            Layout::Gray => {
                fills[0] = Fill::Grey;
                &[0]
            }
            Layout::Red => &[0],
            Layout::Green => &[1],
            Layout::Blue => &[2],
            Layout::Alpha => &[3],
            Layout::Rgb | Layout::YCbCr | Layout::YCbCr422 => &[0, 1, 2],
            Layout::Rgba | Layout::Abgr | Layout::YCbCrA => &[0, 1, 2, 3],
        };
        if supplies.contains(&3) {
            has_alpha = true;
        }
        for &c in supplies {
            reducers[c] = Some(Reducer::new(w32, h32, factor, frame.flip_y));
        }
        let band = reducers.iter().flatten().next().map_or(1, |r| r.factor());
        let piece = rows_per_read(stride, band);
        r.seek(SeekFrom::Start(el.offset))?;
        let mut buf = vec![0u8; stride * piece];
        let mut codes = vec![0f32; samples * piece];
        let mut chans: [Vec<f32>; 4] = [Vec::new(), Vec::new(), Vec::new(), Vec::new()];
        let mut row = 0;
        while row < h {
            let n = piece.min(h - row);
            let got = read_row_block(r, &mut buf, stride, bytes, n, len)?;
            let whole = got;
            if whole == 0 {
                break;
            }
            unpack(el, big, &buf, stride, whole, samples, &mut codes);
            let filled = channels_of(el, &codes, whole, w, max_code, &lut, &mut chans);
            if frame.flip_x {
                for (ch, on) in chans.iter_mut().zip(filled) {
                    if on {
                        ch.par_chunks_mut(w).for_each(|r| r.reverse());
                    }
                }
            }
            for (c, on) in filled.into_iter().enumerate() {
                if let (true, Some(red)) = (on, reducers[c].as_mut()) {
                    red.push_rows(&chans[c], whole, &mut raster, fills[c]);
                }
            }
            row += whole;
            if whole < n {
                break;
            }
        }
        for red in reducers.iter_mut().flatten() {
            red.finish(&mut raster);
        }
    }

    let linear_float = transfer == Transfer::Linear;
    let (ow, oh) = (raster.width, raster.height);
    let mut tex = if linear_float {
        let mut t = Texture::from_rgba32f(ow, oh, raster.rgba);
        t.has_alpha = has_alpha || t.has_alpha;
        t
    } else {
        let rgba: Vec<u8> = raster
            .rgba
            .par_chunks(4)
            .flat_map_iter(|p| {
                [
                    linear_to_srgb8(p[0]),
                    linear_to_srgb8(p[1]),
                    linear_to_srgb8(p[2]),
                    (p[3].clamp(0.0, 1.0) * 255.0 + 0.5) as u8,
                ]
            })
            .collect();
        Texture::from_rgba8(ow, oh, rgba, ColorSpace::Srgb)
    };
    if let Some(pixel_aspect) = frame.aspect {
        tex = apply_pixel_aspect(tex, pixel_aspect);
    }
    tex.source = SourceInfo {
        container: frame.container.into(),
        format: frame.format.clone() + if linear_float { "" } else { channel_tag(crate::thumb::Channels::Rgba) },
        supercompression: None,
        width: frame.width as u32,
        height: frame.height as u32,
        depth: 1,
        layers: 1,
        faces: 1,
        mip_count: 1,
        file_bytes: len,
        header: frame.header,
        ..Default::default()
    };
    Ok(tex)
}

/// Reads `n` rows (`bytes` of samples each, `stride` apart) into `buf`
/// and returns how many whole rows arrived.
fn read_row_block(
    r: &mut dyn Stream,
    buf: &mut [u8],
    stride: usize,
    bytes: usize,
    n: usize,
    _len: u64,
) -> Result<usize, TextureError> {
    let got = read_fully(r, &mut buf[..stride * n])?;
    if got >= stride * n {
        return Ok(n);
    }
    // The last row of the file may lack its end-of-line padding.
    let whole = got / stride;
    if whole < n && got - whole * stride >= bytes {
        buf[got..(whole + 1) * stride].fill(0);
        return Ok(whole + 1);
    }
    Ok(whole)
}

/// Resamples to square pixels when the header gives a pixel shape (an
/// anamorphic scan): the width is scaled by the pixel's aspect ratio.
fn apply_pixel_aspect(tex: Texture, aspect: f64) -> Texture {
    let (w, h) = (tex.width(), tex.height());
    if !(aspect.is_finite() && aspect > 0.25 && aspect < 4.0) || (aspect - 1.0).abs() < 0.02 {
        return tex;
    }
    let nw = (w as f64 * aspect).round().max(1.0);
    if nw > 65536.0 {
        return tex;
    }
    let nw = nw as u32;
    let srgb = tex.color_space != ColorSpace::Linear;
    let pixels = crate::resize::resize(tex.base(), w, h, 1, nw, h, srgb);
    let mut out = tex;
    out.levels[0].width = nw;
    out.levels[0].surfaces[0] = pixels;
    out
}

//endregion

/// A message for formats this module can't read.
pub(crate) fn refuse(what: &str) -> TextureError {
    unsupported(what.to_string())
}

pub(crate) fn bad(what: &str) -> TextureError {
    invalid(what.to_string())
}
