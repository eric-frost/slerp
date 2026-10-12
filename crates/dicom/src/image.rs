//! The image pixel module: what the pixel data holds (size, bits, sign,
//! photometric interpretation, frames), where each frame lies, and how a
//! frame's stored samples become modality values (Hounsfield units for
//! CT) or display colours.

use crate::codec::{Decoded, Samples};
use crate::data::{DataSet, Span, Value, tags};
use crate::lut::{Lut, Palette};

/// Frames are capped at this many, whatever Number of Frames says.
pub const MAX_FRAMES: u32 = 100_000;
/// A frame may hold at most this many samples (rows x columns x samples):
/// 11,000 x 11,000 pixels of greyscale, far past any single frame of a real
/// scan (whole-slide images are tiled into many frames).  The decoders keep
/// the same cap against streams that claim more than the header says.  At
/// the cap a frame takes about 1.5 GB while it is decoded.
pub const MAX_FRAME_SAMPLES: u64 = 1 << 27;

#[derive(Clone, Debug, PartialEq)]
pub enum Photometric {
    /// Greyscale, low values bright (radiographs as printed on film).
    Monochrome1,
    /// Greyscale, low values dark.
    Monochrome2,
    Rgb,
    YbrFull,
    /// YCbCr with chroma halved horizontally.
    YbrFull422,
    /// Retired studio-range YCbCr.
    YbrPartial422,
    YbrPartial420,
    /// JPEG 2000's irreversible and reversible colour transforms, undone by
    /// the decoder.
    YbrIct,
    YbrRct,
    /// Indices into a colour palette.
    Palette,
    Other(String),
}

impl Photometric {
    pub fn parse(s: &str) -> Photometric {
        match s.trim().to_ascii_uppercase().as_str() {
            "MONOCHROME1" => Photometric::Monochrome1,
            "MONOCHROME2" | "" => Photometric::Monochrome2,
            "RGB" => Photometric::Rgb,
            "YBR_FULL" => Photometric::YbrFull,
            "YBR_FULL_422" => Photometric::YbrFull422,
            "YBR_PARTIAL_422" => Photometric::YbrPartial422,
            "YBR_PARTIAL_420" => Photometric::YbrPartial420,
            "YBR_ICT" => Photometric::YbrIct,
            "YBR_RCT" => Photometric::YbrRct,
            "PALETTE COLOR" | "PALETTE COLOUR" => Photometric::Palette,
            other => Photometric::Other(other.to_string()),
        }
    }

    pub fn is_grey(&self) -> bool {
        matches!(self, Photometric::Monochrome1 | Photometric::Monochrome2)
    }

    /// Native data with two bytes per pixel (Y Y Cb Cr per pair).
    fn is_422(&self) -> bool {
        matches!(self, Photometric::YbrFull422 | Photometric::YbrPartial422)
    }
}

/// The pixel module of a data set.
#[derive(Clone, Debug)]
pub struct ImageInfo {
    pub rows: u32,
    pub columns: u32,
    pub samples: u32,
    pub bits_allocated: u32,
    pub bits_stored: u32,
    pub high_bit: u32,
    pub signed: bool,
    pub photometric: Photometric,
    /// Colour planes one after another rather than interleaved.
    pub planar: bool,
    /// Frames, as many as the data holds (Number of Frames capped).
    pub frames: u32,
    /// Float Pixel Data (32) or Double Float Pixel Data (64); 0 for
    /// integers.
    pub float_bits: u32,
}

/// Where the frames' bytes are.
#[derive(Clone, Debug)]
pub enum Store {
    /// Native data: the pixel data value, frames one after another.
    Native { span: Span, big_endian: bool, ow: bool },
    /// Encapsulated data: the fragments, the Basic Offset Table first.
    Encapsulated(Vec<Span>),
}

impl ImageInfo {
    /// Reads the pixel module and finds the frames.  `None` when the data
    /// set has no pixel data (structured reports, DICOMDIRs).
    pub fn read(ds: &DataSet) -> Result<Option<(ImageInfo, Store)>, String> {
        let (pixel, float_bits) = if let Some(e) = ds.get(tags::PIXEL_DATA) {
            (e, 0)
        } else if let Some(e) = ds.get(tags::FLOAT_PIXEL_DATA) {
            (e, 32)
        } else if let Some(e) = ds.get(tags::DOUBLE_PIXEL_DATA) {
            (e, 64)
        } else {
            return Ok(None);
        };
        let rows = ds.u32(tags::ROWS).filter(|&r| r > 0).ok_or("the image has no Rows")?;
        let columns = ds.u32(tags::COLUMNS).filter(|&c| c > 0).ok_or("the image has no Columns")?;
        let samples = ds.u32(tags::SAMPLES_PER_PIXEL).unwrap_or(1);
        let photometric = Photometric::parse(&ds.string(tags::PHOTOMETRIC).unwrap_or_default());
        let samples = match (samples, &photometric) {
            (1 | 3, _) => samples,
            (_, p) if p.is_grey() || *p == Photometric::Palette => 1,
            _ => return Err(format!("{samples} samples per pixel isn't supported")),
        };
        let bits_allocated = if float_bits > 0 { float_bits } else { ds.u32(tags::BITS_ALLOCATED).unwrap_or(16) };
        if !matches!(bits_allocated, 1 | 8 | 16 | 32 | 64) || (bits_allocated == 64 && float_bits == 0) {
            return Err(format!("{bits_allocated} bits allocated isn't supported"));
        }
        let bits_stored = ds.u32(tags::BITS_STORED).filter(|&b| b > 0).unwrap_or(bits_allocated).min(bits_allocated);
        let high_bit =
            ds.u32(tags::HIGH_BIT).filter(|&h| h < bits_allocated && h + 1 >= bits_stored).unwrap_or(bits_stored - 1);
        let signed = ds.u32(tags::PIXEL_REPRESENTATION) == Some(1);
        let planar = samples == 3 && ds.u32(tags::PLANAR_CONFIGURATION) == Some(1);
        let declared = ds.int(tags::NUMBER_OF_FRAMES).filter(|&n| n > 0).unwrap_or(1).min(MAX_FRAMES as i64) as u32;
        let frame_samples = rows as u64 * columns as u64 * samples as u64;
        if frame_samples > MAX_FRAME_SAMPLES {
            return Err(format!("{columns} x {rows} pixels is too large"));
        }
        let mut info = ImageInfo {
            rows,
            columns,
            samples,
            bits_allocated,
            bits_stored,
            high_bit,
            signed,
            photometric,
            planar,
            frames: declared,
            float_bits,
        };
        let store = match &pixel.value {
            Value::Deferred(span) => {
                let available = (span.len * 8 / info.frame_bits().max(1)).min(MAX_FRAMES as u64) as u32;
                if available == 0 {
                    return Err(format!(
                        "the pixel data holds {} bytes, a frame needs {}",
                        span.len,
                        info.frame_bits() / 8
                    ));
                }
                info.frames = declared.min(available);
                Store::Native { span: *span, big_endian: ds.big_endian, ow: pixel.vr == crate::data::Vr::OW }
            }
            Value::Bytes(b) if b.is_empty() => return Err("the pixel data is empty".into()),
            Value::Fragments(fragments) => {
                if fragments.iter().skip(1).all(|f| f.len == 0) {
                    return Err("the pixel data has no fragments".into());
                }
                // The caller groups the fragments into frames, which can
                // mean reading the offset table.
                Store::Encapsulated(fragments.clone())
            }
            _ => return Err("the pixel data is not where it should be".into()),
        };
        Ok(Some((info, store)))
    }

    /// Bits one native frame takes.
    pub fn frame_bits(&self) -> u64 {
        let per_pixel = if self.photometric.is_422() && self.samples == 3 { 2 } else { self.samples as u64 };
        self.rows as u64 * self.columns as u64 * per_pixel * self.bits_allocated as u64
    }

    pub fn is_colour(&self) -> bool {
        self.samples == 3 || self.photometric == Photometric::Palette
    }
}

/// Splits fragments (after the Basic Offset Table) into frames by the
/// frames' offsets from the first fragment's item tag, as the Basic and
/// Extended Offset Tables give them.
pub(crate) fn split_by_table(items: &[Span], offsets: &[u64]) -> Option<Vec<Vec<Span>>> {
    let first_item = items.first()?.offset.saturating_sub(8);
    if offsets.is_empty() || offsets[0] != 0 || offsets.windows(2).any(|w| w[1] <= w[0]) {
        return None;
    }
    let mut frames = Vec::with_capacity(offsets.len());
    for (i, &start) in offsets.iter().enumerate() {
        let end = offsets.get(i + 1).copied().unwrap_or(u64::MAX);
        let frame: Vec<Span> = items
            .iter()
            .copied()
            .filter(|s| {
                let at = s.offset.saturating_sub(8).saturating_sub(first_item);
                at >= start && at < end
            })
            .collect();
        if frame.is_empty() {
            return None;
        }
        frames.push(frame);
    }
    Some(frames)
}

/// Splits fragments into frames where a fragment begins a JPEG (`FFD8`)
/// or JPEG 2000 (`FF4F`) codestream.  `starts` holds each fragment's first
/// two bytes.
pub(crate) fn split_by_markers(items: &[Span], starts: &[[u8; 2]]) -> Vec<Vec<Span>> {
    let mut frames: Vec<Vec<Span>> = Vec::new();
    for (s, start) in items.iter().zip(starts) {
        let begins = matches!(start, [0xFF, 0xD8] | [0xFF, 0x4F]);
        match frames.last_mut() {
            Some(frame) if !begins => frame.push(*s),
            _ => frames.push(vec![*s]),
        }
    }
    frames
}

/*┌─────────────────╮
| Native frames   |
└─────────────────╯*/
//region Native frames

/// Unpacks a native frame's bytes into samples.  `bit_offset` is where
/// the frame starts within `bytes` for 1-bit data, which isn't byte
/// aligned.
pub fn native(bytes: &[u8], info: &ImageInfo, big_endian: bool, ow: bool, bit_offset: u64) -> Result<Decoded, String> {
    let (w, h) = (info.columns, info.rows);
    let pixels = w as usize * h as usize;
    let per_pixel = if info.photometric.is_422() && info.samples == 3 { 2 } else { info.samples as usize };
    let n = pixels * per_pixel;
    let need = (bit_offset + n as u64 * info.bits_allocated as u64).div_ceil(8) as usize;
    if bytes.len() < need {
        return Err(format!("the frame needs {need} bytes, {} are there", bytes.len()));
    }
    let shift = (info.high_bit + 1).saturating_sub(info.bits_stored);
    let data = match info.bits_allocated {
        1 => Samples::U8(
            (0..n as u64)
                .map(|i| {
                    let bit = bit_offset + i;
                    (bytes[(bit / 8) as usize] >> (bit % 8)) & 1
                })
                .collect(),
        ),
        8 => {
            // Big-endian OW data swaps each pair of 8-bit samples, the
            // padding byte of an odd count included.
            let swap = big_endian && ow;
            let mut v = bytes[..if swap { (n + 1).min(bytes.len()) } else { n }].to_vec();
            if swap {
                for pair in v.chunks_exact_mut(2) {
                    pair.swap(0, 1);
                }
                v.truncate(n);
            }
            if shift > 0 {
                v.iter_mut().for_each(|s| *s >>= shift);
            }
            Samples::U8(v)
        }
        16 => Samples::U16(
            bytes[..n * 2]
                .chunks_exact(2)
                .map(|c| if big_endian { u16::from_be_bytes([c[0], c[1]]) } else { u16::from_le_bytes([c[0], c[1]]) } >> shift)
                .collect(),
        ),
        32 if info.float_bits == 32 => Samples::F32(
            bytes[..n * 4]
                .chunks_exact(4)
                .map(|c| {
                    let b = [c[0], c[1], c[2], c[3]];
                    if big_endian { f32::from_be_bytes(b) } else { f32::from_le_bytes(b) }
                })
                .collect(),
        ),
        32 => Samples::U32(
            bytes[..n * 4]
                .chunks_exact(4)
                .map(|c| {
                    let b = [c[0], c[1], c[2], c[3]];
                    (if big_endian { u32::from_be_bytes(b) } else { u32::from_le_bytes(b) }) >> shift
                })
                .collect(),
        ),
        64 => Samples::F32(
            bytes[..n * 8]
                .chunks_exact(8)
                .map(|c| {
                    let b: [u8; 8] = c.try_into().unwrap_or([0; 8]);
                    (if big_endian { f64::from_be_bytes(b) } else { f64::from_le_bytes(b) }) as f32
                })
                .collect(),
        ),
        b => return Err(format!("{b} bits allocated isn't supported")),
    };
    let data = if info.photometric.is_422() && info.samples == 3 {
        expand_422(data, w as usize, h as usize)?
    } else if info.planar {
        interleave_planes(data, pixels)
    } else {
        data
    };
    Ok(Decoded { width: w, height: h, comps: info.samples, precision: info.bits_stored, data, rgb: false })
}

/// YBR_FULL_422 native data: `Y Y Cb Cr` for each pair of pixels.
fn expand_422(data: Samples, w: usize, h: usize) -> Result<Samples, String> {
    let Samples::U8(v) = data else { return Err("only 8-bit YBR_FULL_422 is supported".into()) };
    let mut out = vec![0u8; w * h * 3];
    let row_in = w.div_ceil(2) * 4;
    for y in 0..h {
        for x in 0..w {
            let group = y * row_in + (x / 2) * 4;
            let Some(g) = v.get(group..group + 4) else { continue };
            let o = (y * w + x) * 3;
            out[o] = g[x % 2];
            out[o + 1] = g[2];
            out[o + 2] = g[3];
        }
    }
    Ok(Samples::U8(out))
}

fn interleave_planes(data: Samples, pixels: usize) -> Samples {
    fn go<T: Copy + Default>(v: Vec<T>, pixels: usize) -> Vec<T> {
        let mut out = vec![T::default(); v.len()];
        for c in 0..3 {
            for i in 0..pixels {
                if let (Some(o), Some(&s)) = (out.get_mut(i * 3 + c), v.get(c * pixels + i)) {
                    *o = s;
                }
            }
        }
        out
    }
    match data {
        Samples::U8(v) => Samples::U8(go(v, pixels)),
        Samples::U16(v) => Samples::U16(go(v, pixels)),
        Samples::U32(v) => Samples::U32(go(v, pixels)),
        Samples::F32(v) => Samples::F32(go(v, pixels)),
    }
}

//endregion

/*┌─────────────────╮
| Frames          |
└─────────────────╯*/
//region Frames

/// A decoded frame: modality values for greyscale, display colours for
/// colour images.
#[derive(Clone, Debug)]
pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub pixels: FramePixels,
}

#[derive(Clone, Debug)]
pub enum FramePixels {
    /// Modality values (after Rescale Slope and Intercept or the Modality
    /// LUT): Hounsfield units for CT.
    Grey(Vec<f32>),
    /// 8-bit RGB.
    Rgb(Vec<u8>),
}

/// How stored values become modality values.
#[derive(Clone, Debug)]
pub struct Modality {
    pub slope: f64,
    pub intercept: f64,
    pub lut: Option<Lut>,
}

impl Default for Modality {
    fn default() -> Self {
        Modality { slope: 1.0, intercept: 0.0, lut: None }
    }
}

impl Modality {
    pub fn apply(&self, stored: f64) -> f64 {
        match &self.lut {
            Some(lut) => lut.lookup(stored) as f64,
            None => stored * self.slope + self.intercept,
        }
    }
}

/// Sign-extends or masks a sample to `bits`.
#[inline]
fn integer(pattern: u32, bits: u32, signed: bool) -> i64 {
    if bits >= 32 {
        return if signed { pattern as i32 as i64 } else { pattern as i64 };
    }
    let v = pattern & ((1u32 << bits) - 1);
    if signed && v & (1 << (bits - 1)) != 0 { v as i64 - (1i64 << bits) } else { v as i64 }
}

/// Turns decoded samples into a frame: greyscale through the modality
/// transform, palette indices through the palette, colour to 8-bit RGB.
/// `codec` is false for native data, whose masking already happened.
pub fn to_frame(
    d: Decoded,
    info: &ImageInfo,
    modality: &Modality,
    palette: Option<&Palette>,
    codec: bool,
) -> Result<Frame, String> {
    let (w, h) = (d.width, d.height);
    let pixels = w as usize * h as usize;
    if d.data.len() < pixels * d.comps as usize {
        return Err("the decoded frame is smaller than its header says".into());
    }
    // Unsigned codec output may use more bits than Bits Stored says (a
    // known writer mistake); masking to Bits Stored would wrap it.
    let bits = if codec && !info.signed { info.bits_stored.max(d.precision) } else { info.bits_stored };
    let signed = info.signed;
    if info.photometric == Photometric::Palette {
        let palette = palette.ok_or("the palette is missing")?;
        let mut out = vec![0u8; pixels * 3];
        for (i, o) in out.chunks_exact_mut(3).enumerate() {
            let index = integer(d.data.get(i * d.comps as usize), bits, signed);
            o.copy_from_slice(&palette.lookup(index));
        }
        return Ok(Frame { width: w, height: h, pixels: FramePixels::Rgb(out) });
    }
    if d.comps == 1 {
        let values: Vec<f32> = match &d.data {
            Samples::F32(v) => v.iter().map(|&x| modality.apply(x as f64) as f32).collect(),
            data => {
                // A table for small sample ranges, the transform per sample
                // otherwise.
                if bits <= 16 && modality.lut.is_none() {
                    let (slope, intercept) = (modality.slope, modality.intercept);
                    let table: Vec<f32> = (0..1u32 << bits)
                        .map(|p| (integer(p, bits, signed) as f64 * slope + intercept) as f32)
                        .collect();
                    let mask = (1u32 << bits) - 1;
                    (0..pixels).map(|i| table[(data.get(i) & mask) as usize]).collect()
                } else {
                    (0..pixels).map(|i| modality.apply(integer(data.get(i), bits, signed) as f64) as f32).collect()
                }
            }
        };
        return Ok(Frame { width: w, height: h, pixels: FramePixels::Grey(values) });
    }
    if d.comps != 3 {
        return Err(format!("{} components per pixel isn't supported", d.comps));
    }
    // Colour: scale to 8 bits, then undo YCbCr unless the codec did.
    let precision = if codec { d.precision } else { info.bits_stored };
    let down = precision.saturating_sub(8);
    let mut rgb = vec![0u8; pixels * 3];
    match &d.data {
        Samples::U8(v) if down == 0 => rgb.copy_from_slice(&v[..pixels * 3]),
        Samples::F32(v) => {
            for (o, &s) in rgb.iter_mut().zip(v) {
                *o = (s.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
            }
        }
        data => {
            for (i, o) in rgb.iter_mut().enumerate() {
                *o = (data.get(i) >> down).min(255) as u8;
            }
        }
    }
    if !d.rgb {
        match info.photometric {
            Photometric::YbrFull | Photometric::YbrFull422 | Photometric::YbrIct => ybr_full_to_rgb(&mut rgb),
            Photometric::YbrPartial422 | Photometric::YbrPartial420 => ybr_partial_to_rgb(&mut rgb),
            _ => {}
        }
    }
    Ok(Frame { width: w, height: h, pixels: FramePixels::Rgb(rgb) })
}

/// Full-range YCbCr (ITU-R BT.601 as JPEG uses it) to RGB, in place.
pub fn ybr_full_to_rgb(px: &mut [u8]) {
    for p in px.chunks_exact_mut(3) {
        let (y, cb, cr) = (p[0] as f32, p[1] as f32 - 128.0, p[2] as f32 - 128.0);
        p[0] = (y + 1.402 * cr).round().clamp(0.0, 255.0) as u8;
        p[1] = (y - 0.344136 * cb - 0.714136 * cr).round().clamp(0.0, 255.0) as u8;
        p[2] = (y + 1.772 * cb).round().clamp(0.0, 255.0) as u8;
    }
}

/// Studio-range YCbCr (Y 16 to 235) to RGB, in place.
pub fn ybr_partial_to_rgb(px: &mut [u8]) {
    for p in px.chunks_exact_mut(3) {
        let (y, cb, cr) = (1.1644 * (p[0] as f32 - 16.0), p[1] as f32 - 128.0, p[2] as f32 - 128.0);
        p[0] = (y + 1.5960 * cr).round().clamp(0.0, 255.0) as u8;
        p[1] = (y - 0.3918 * cb - 0.8130 * cr).round().clamp(0.0, 255.0) as u8;
        p[2] = (y + 2.0172 * cb).round().clamp(0.0, 255.0) as u8;
    }
}

/// Shrinks a frame by averaging `factor` x `factor` blocks, for
/// thumbnails of large images.
pub fn shrink(frame: Frame, factor: u32) -> Frame {
    if factor <= 1 {
        return frame;
    }
    let (w, h) = (frame.width as usize, frame.height as usize);
    let f = factor as usize;
    let (ow, oh) = (w.div_ceil(f), h.div_ceil(f));
    let pixels = match frame.pixels {
        FramePixels::Grey(v) => {
            let mut out = vec![0f32; ow * oh];
            for (oy, row) in out.chunks_exact_mut(ow).enumerate() {
                for (ox, o) in row.iter_mut().enumerate() {
                    let (mut sum, mut n) = (0f64, 0u32);
                    for y in oy * f..((oy + 1) * f).min(h) {
                        for x in ox * f..((ox + 1) * f).min(w) {
                            let s = v[y * w + x];
                            if s.is_finite() {
                                sum += s as f64;
                                n += 1;
                            }
                        }
                    }
                    *o = if n > 0 { (sum / n as f64) as f32 } else { 0.0 };
                }
            }
            FramePixels::Grey(out)
        }
        FramePixels::Rgb(v) => {
            let mut out = vec![0u8; ow * oh * 3];
            for oy in 0..oh {
                for ox in 0..ow {
                    let mut sum = [0u32; 3];
                    let mut n = 0;
                    for y in oy * f..((oy + 1) * f).min(h) {
                        for x in ox * f..((ox + 1) * f).min(w) {
                            for c in 0..3 {
                                sum[c] += v[(y * w + x) * 3 + c] as u32;
                            }
                            n += 1;
                        }
                    }
                    for c in 0..3 {
                        out[(oy * ow + ox) * 3 + c] = (sum[c] / n.max(1)) as u8;
                    }
                }
            }
            FramePixels::Rgb(out)
        }
    };
    Frame { width: ow as u32, height: oh as u32, pixels }
}

//endregion

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ybr_full_is_jpegs_conversion() {
        // Neutral grey, white and black stay put; red's YCbCr gives red.
        let mut px = [128, 128, 128, 255, 128, 128, 0, 128, 128, 76, 85, 255];
        ybr_full_to_rgb(&mut px);
        assert_eq!(&px[..9], &[128, 128, 128, 255, 255, 255, 0, 0, 0]);
        assert!(px[9] >= 253 && px[10] <= 2 && px[11] <= 2, "{:?}", &px[9..]);
    }

    #[test]
    fn ybr_partial_maps_studio_range_to_full() {
        // Y 16 is black and 235 white; chroma 128 is neutral.
        let mut px = [16, 128, 128, 235, 128, 128, 126, 128, 128];
        ybr_partial_to_rgb(&mut px);
        assert_eq!(&px[..6], &[0, 0, 0, 255, 255, 255]);
        // (126 - 16) * 255 / 219 = 128.07
        assert!((px[6] as i32 - 128).abs() <= 1, "{}", px[6]);
    }

    #[test]
    fn photometric_names() {
        assert_eq!(Photometric::parse("MONOCHROME1 "), Photometric::Monochrome1);
        assert_eq!(Photometric::parse("monochrome2"), Photometric::Monochrome2);
        assert_eq!(Photometric::parse("PALETTE COLOR"), Photometric::Palette);
        assert_eq!(Photometric::parse("YBR_FULL_422"), Photometric::YbrFull422);
        assert_eq!(Photometric::parse("YBR_RCT"), Photometric::YbrRct);
        assert!(matches!(Photometric::parse("HSV"), Photometric::Other(_)));
        assert!(Photometric::Monochrome1.is_grey() && !Photometric::Rgb.is_grey());
    }

    #[test]
    fn signed_values_sign_extend_from_bits_stored() {
        assert_eq!(integer(0x0FFF, 12, true), -1);
        assert_eq!(integer(0x0800, 12, true), -2048);
        assert_eq!(integer(0x07FF, 12, true), 2047);
        assert_eq!(integer(0xF7FF, 12, false), 2047);
        assert_eq!(integer(0xFFFF_FFFF, 32, true), -1);
        assert_eq!(integer(0xFFFF_FFFF, 32, false), 4_294_967_295);
    }

    #[test]
    fn shrinking_averages_blocks() {
        let f =
            Frame { width: 4, height: 2, pixels: FramePixels::Grey(vec![0.0, 2.0, 10.0, 10.0, 4.0, 6.0, 10.0, 30.0]) };
        let s = shrink(f, 2);
        assert_eq!((s.width, s.height), (2, 1));
        let FramePixels::Grey(v) = s.pixels else { panic!() };
        assert_eq!(v, [3.0, 15.0]);
    }
}
