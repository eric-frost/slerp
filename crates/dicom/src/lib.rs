//! DICOM medical images: CT, MR, X-ray, ultrasound and the rest, as Part
//! 10 files (a 128-byte preamble and `DICM`) or bare data sets, in any of
//! the standard transfer syntaxes (explicit and implicit VR, little and
//! big endian, deflated) with uncompressed, RLE, JPEG (baseline, 12-bit
//! extended and lossless), JPEG-LS and JPEG 2000 pixel data.
//!
//! Greyscale frames decode to floating-point modality values (Hounsfield
//! units for CT), so a viewer can move the brightness window without
//! decoding again; [`window`] maps them for display with the stored window,
//! a VOI LUT or the image's own range.  Thumbnails show the middle frame
//! with the stored window, in the pixels' true proportions.
//!
//! The reader is this crate's own: small, and careful with damaged files
//! (lengths checked against the file, nesting and element counts capped,
//! large values left in the file until needed).  JPEG 2000 comes from
//! `hayro-jpeg2000` and 8-bit and lossless JPEG from `jpeg-decoder`, both
//! pure Rust; 12-bit JPEG, JPEG-LS and RLE are decoded here.

pub(crate) mod codec;
pub mod data;
pub mod dicomdir;
mod dict;
mod dict_data;
pub mod image;
pub mod lut;
pub mod parse;
pub mod series;
pub mod text;
pub mod transfer;
pub mod window;

#[cfg(test)]
mod tests;

pub use data::{DataSet, Element, Tag, Value, Vr, tag, tag_name, tag_text, tags};
pub use dict::uid_name;
pub use image::{Frame, FramePixels, ImageInfo, Modality, Photometric};
pub use window::{CT_PRESETS, StoredVoi, Voi, VoiFunction, Window};

use crate::data::Span;
use crate::image::Store;
use crate::lut::{Lut, Palette};
use crate::parse::{Extent, Parsed};
use crate::transfer::Codec;
use slerp_scene::textures::{ColorSpace, SourceInfo, Texture};
use slerp_scene::{Asset, Limits, LoadError};
use std::path::Path;
use std::sync::Arc;

/// One frame's pixel data may take at most this many bytes.
const MAX_FRAME_BYTES: u64 = 2 << 30;

/// The SOP class of DICOMDIR files.
const DIRECTORY_SOP_CLASS: &str = "1.2.840.10008.1.3.10";

/// Recognises DICOM from a file's first bytes: `DICM` after the preamble,
/// or a run of plausible header elements for files without one.
pub fn sniff(_path: &Path, head: &[u8]) -> bool {
    parse::looks_like_dicom(head)
}

/// An opened DICOM file: its data set, and for images, how to decode each
/// frame.
pub struct Dicom {
    pub parsed: Parsed,
    pub info: Option<ImageInfo>,
    store: Option<Store>,
    /// Encapsulated frames' fragments.
    frames: Vec<Vec<Span>>,
    palette: Option<Palette>,
    /// Why the image can't be shown, for files whose header reads but
    /// whose pixel module doesn't (an unsupported layout).
    pub image_error: Option<String>,
}

impl Dicom {
    /// Reads a file's data set.  Pixel data stays in the file until a frame
    /// is decoded.
    pub fn open(path: &Path) -> Result<Dicom, LoadError> {
        Dicom::from_parsed(parse::parse_file(path, Extent::All)?)
    }

    /// Reads a file held in memory.
    pub fn from_bytes(bytes: Vec<u8>) -> Result<Dicom, LoadError> {
        Dicom::from_parsed(parse::parse_bytes(bytes, Extent::All)?)
    }

    fn from_parsed(parsed: Parsed) -> Result<Dicom, LoadError> {
        let mut d = Dicom { parsed, info: None, store: None, frames: Vec::new(), palette: None, image_error: None };
        match ImageInfo::read(&d.parsed.data) {
            Ok(Some((mut info, store))) => {
                if let Store::Encapsulated(fragments) = &store {
                    d.frames = d.split_frames(fragments, info.frames);
                    info.frames = d.frames.len() as u32;
                    if d.frames.is_empty() {
                        d.image_error = Some("the compressed pixel data has no frames".into());
                    }
                }
                if info.photometric == Photometric::Palette {
                    d.palette = Palette::read(&d.parsed.data);
                    if d.palette.is_none() {
                        d.image_error = Some("the colour palette is missing or damaged".into());
                    }
                }
                if d.image_error.is_none() {
                    d.info = Some(info);
                    d.store = Some(store);
                }
            }
            Ok(None) => {
                if let Some(w) = d.parsed.warnings.iter().find(|w| w.starts_with("stopped reading")) {
                    d.image_error = Some(format!("the file is damaged or cut short ({w})"));
                }
            }
            Err(e) => d.image_error = Some(e),
        }
        Ok(d)
    }

    pub fn data(&self) -> &DataSet {
        &self.parsed.data
    }

    pub fn meta(&self) -> &DataSet {
        &self.parsed.meta
    }

    /// True for DICOMDIR files (the index of a CD or DVD).
    pub fn is_directory(&self) -> bool {
        self.meta().string(tags::MEDIA_STORAGE_SOP_CLASS).as_deref() == Some(DIRECTORY_SOP_CLASS)
            || self.data().contains(tags::DIRECTORY_RECORD_SEQUENCE)
    }

    pub fn frame_count(&self) -> u32 {
        self.info.as_ref().map_or(0, |i| i.frames)
    }

    /// Groups encapsulated fragments into frames (PS3.5 A.4).
    fn split_frames(&self, fragments: &[Span], declared: u32) -> Vec<Vec<Span>> {
        let Some((bot, rest)) = fragments.split_first() else { return Vec::new() };
        let items: Vec<Span> = rest.iter().copied().filter(|s| s.len > 0).collect();
        if items.is_empty() {
            return Vec::new();
        }
        let ds = self.data();
        // 1. The Extended Offset Table.
        if let Some(table) = ds.bytes(tags::EXTENDED_OFFSET_TABLE) {
            let offsets: Vec<u64> = table
                .chunks_exact(8)
                .take(image::MAX_FRAMES as usize)
                .map(|c| u64::from_le_bytes(c.try_into().unwrap_or([0; 8])))
                .collect();
            if let Some(frames) = image::split_by_table(&items, &offsets) {
                return frames;
            }
        }
        // 2. The Basic Offset Table.
        if bot.len >= 4
            && bot.len % 4 == 0
            && bot.len <= 4 * image::MAX_FRAMES as u64
            && let Ok(b) = self.parsed.source.read(*bot, bot.len)
        {
            let offsets: Vec<u64> = b
                .chunks_exact(4)
                .map(|c| {
                    let a = [c[0], c[1], c[2], c[3]];
                    (if ds.big_endian { u32::from_be_bytes(a) } else { u32::from_le_bytes(a) }) as u64
                })
                .collect();
            if let Some(frames) = image::split_by_table(&items, &offsets) {
                return frames;
            }
        }
        // 3. One frame of everything, or one fragment per frame.
        if declared <= 1 {
            return vec![items];
        }
        if items.len() as u32 == declared || self.parsed.transfer.codec == Codec::Rle {
            return items.into_iter().map(|s| vec![s]).collect();
        }
        // 4. Where each frame's codestream begins.
        let starts: Vec<[u8; 2]> = items
            .iter()
            .map(|s| {
                let b = self.parsed.source.read(Span { offset: s.offset, len: s.len.min(2) }, 2).unwrap_or_default();
                [b.first().copied().unwrap_or(0), b.get(1).copied().unwrap_or(0)]
            })
            .collect();
        image::split_by_markers(&items, &starts)
    }

    /// The functional group item `seq` for a frame of an enhanced
    /// multi-frame image: the frame's own, else the shared one.
    pub fn group_item(&self, index: u32, seq: Tag) -> Option<&DataSet> {
        let ds = self.data();
        let own = ds.items(tags::PER_FRAME_FUNCTIONAL_GROUPS).get(index as usize).and_then(|g| g.items(seq).first());
        own.or_else(|| ds.items(tags::SHARED_FUNCTIONAL_GROUPS).first().and_then(|g| g.items(seq).first()))
    }

    fn signed(&self) -> bool {
        self.info.as_ref().is_some_and(|i| i.signed)
    }

    /// How a frame's stored values become modality values.
    pub fn modality_transform(&self, index: u32) -> Modality {
        let ds = self.data();
        let src = self
            .group_item(index, tags::PIXEL_VALUE_TRANSFORMATION_SEQUENCE)
            .filter(|g| g.contains(tags::RESCALE_SLOPE) || g.contains(tags::RESCALE_INTERCEPT))
            .unwrap_or(ds);
        let lut = if src.contains(tags::RESCALE_SLOPE) {
            None
        } else {
            ds.items(tags::MODALITY_LUT_SEQUENCE).first().and_then(|item| Lut::from_item(item, self.signed()))
        };
        Modality {
            slope: src.float(tags::RESCALE_SLOPE).filter(|s| *s != 0.0).unwrap_or(1.0),
            intercept: src.float(tags::RESCALE_INTERCEPT).unwrap_or(0.0),
            lut,
        }
    }

    /// Decodes a frame.  `max_size` is a hint for thumbnails: codecs that
    /// can decode smaller do, and large results are shrunk to about twice
    /// that.
    pub fn decode_frame(&self, index: u32, max_size: Option<u32>) -> Result<Frame, LoadError> {
        let (Some(info), Some(store)) = (&self.info, &self.store) else {
            return Err(LoadError::Unsupported(
                self.image_error.clone().unwrap_or_else(|| "the file has no image".into()),
            ));
        };
        let index = index.min(info.frames.saturating_sub(1));
        let modality = self.modality_transform(index);
        let frame = match store {
            Store::Native { span, big_endian, ow } => {
                let frame_bits = info.frame_bits();
                let start_bit = index as u64 * frame_bits;
                let offset = span.offset + start_bit / 8;
                // One byte more when there is one: big-endian OW pairs of an odd
                // count of 8-bit samples include the padding byte.
                let len = (((start_bit % 8) + frame_bits).div_ceil(8) + 1).min(span.offset + span.len - offset);
                let bytes = self.parsed.source.read(Span { offset, len }, MAX_FRAME_BYTES)?;
                let decoded =
                    image::native(&bytes, info, *big_endian, *ow, start_bit % 8).map_err(LoadError::Invalid)?;
                image::to_frame(decoded, info, &modality, self.palette.as_ref(), false).map_err(LoadError::Invalid)?
            }
            Store::Encapsulated(_) => {
                let spans = self.frames.get(index as usize).ok_or_else(|| LoadError::invalid("frame out of range"))?;
                let bytes = self.parsed.source.read_joined(spans, MAX_FRAME_BYTES)?;
                let codec = self.parsed.transfer.codec;
                let (decoded, from_codec) = if codec == Codec::EncapsulatedNative {
                    (image::native(&bytes, info, false, false, 0).map_err(LoadError::Invalid)?, false)
                } else if codec == Codec::DeflatedFrames {
                    let limit = info.frame_bits().div_ceil(8) as usize;
                    let raw = miniz_oxide::inflate::decompress_to_vec_with_limit(&bytes, limit)
                        .map_err(|e| LoadError::invalid(format!("deflated frame: {:?}", e.status)))?;
                    (image::native(&raw, info, false, false, 0).map_err(LoadError::Invalid)?, false)
                } else {
                    (codec::decode(codec, &bytes, info, max_size).map_err(LoadError::Unsupported)?, true)
                };
                if decoded.comps != info.samples && !(info.samples == 1 && decoded.comps == 3) {
                    return Err(LoadError::invalid(format!(
                        "the frame decoded to {} components, the header says {}",
                        decoded.comps, info.samples
                    )));
                }
                let decoded = if info.samples == 1 && decoded.comps == 3 { first_component(decoded) } else { decoded };
                image::to_frame(decoded, info, &modality, self.palette.as_ref(), from_codec)
                    .map_err(LoadError::Invalid)?
            }
        };
        Ok(match max_size {
            Some(m) => {
                let factor = frame.width.max(frame.height) / (2 * m.max(1));
                image::shrink(frame, factor)
            }
            None => frame,
        })
    }

    /// The windows and VOI LUTs the file stores for a frame, in order.
    pub fn stored_vois(&self, index: u32) -> Vec<StoredVoi> {
        let ds = self.data();
        let src = self
            .group_item(index, tags::FRAME_VOI_LUT_SEQUENCE)
            .filter(|g| g.contains(tags::WINDOW_CENTER))
            .unwrap_or(ds);
        let centers = src.numbers(tags::WINDOW_CENTER);
        let widths = src.numbers(tags::WINDOW_WIDTH);
        let names = src.strings(tags::WINDOW_EXPLANATION);
        let function = match src.string(tags::VOI_LUT_FUNCTION).as_deref() {
            Some("LINEAR_EXACT") => VoiFunction::LinearExact,
            Some("SIGMOID") => VoiFunction::Sigmoid,
            _ => VoiFunction::Linear,
        };
        let mut out = Vec::new();
        for (i, (&c, &w)) in centers.iter().zip(&widths).enumerate() {
            if !(c.is_finite() && w.is_finite() && w > 0.0) {
                continue;
            }
            let name =
                names.get(i).filter(|n| !n.is_empty()).cloned().unwrap_or_else(|| format!("Stored window {}", i + 1));
            out.push(StoredVoi { voi: Voi::Window(Window { center: c, width: w, function }), name });
        }
        let negative = self.signed() || self.modality_transform(index).intercept < 0.0;
        for (i, item) in ds.items(tags::VOI_LUT_SEQUENCE).iter().enumerate() {
            if let Some(lut) = Lut::from_item(item, negative) {
                let name = if lut.explanation.is_empty() {
                    format!("Stored VOI LUT {}", i + 1)
                } else {
                    lut.explanation.clone()
                };
                out.push(StoredVoi { voi: Voi::Lut(Arc::new(lut)), name });
            }
        }
        out
    }

    /// The window the image's own values span, leaving out padding.
    pub fn auto_voi(&self, frame: &Frame) -> Voi {
        let FramePixels::Grey(values) = &frame.pixels else { return Voi::Window(Window::spanning(0.0, 255.0)) };
        let modality = self.modality_transform(0);
        let stored = |t: Tag| self.data().float(t).map(|v| if self.signed() && v > 32767.0 { v - 65536.0 } else { v });
        let padding = stored(tags::PIXEL_PADDING_VALUE).map(|a| {
            let b = stored(tags::PIXEL_PADDING_RANGE_LIMIT).unwrap_or(a);
            let (a, b) = (modality.apply(a) as f32, modality.apply(b) as f32);
            (a.min(b), a.max(b))
        });
        let range = window::value_range(values, padding).or_else(|| window::value_range(values, None));
        let (lo, hi) = range.unwrap_or((0.0, 1.0));
        Voi::Window(Window::spanning(lo as f64, hi as f64))
    }

    /// The stored window, else the image's range.  A stored window that
    /// shows almost nothing of this image (some converters copy a CT window
    /// onto X-rays, which turns them solid white) gives way to the range.
    pub fn default_voi(&self, index: u32, frame: &Frame) -> Voi {
        match self.stored_vois(index).into_iter().next() {
            Some(s) if window::shows_detail(&s.voi, frame) => s.voi,
            _ => self.auto_voi(frame),
        }
    }

    /// Whether greyscale shows inverted: MONOCHROME1 (bright for low
    /// values), flipped again by an INVERSE presentation LUT shape.
    pub fn inverted(&self) -> bool {
        let mono1 = self.info.as_ref().is_some_and(|i| i.photometric == Photometric::Monochrome1);
        let inverse = self.data().string(tags::PRESENTATION_LUT_SHAPE).as_deref() == Some("INVERSE");
        mono1 != inverse
    }

    /// A pixel's height over its width, from Pixel Spacing, Imager Pixel
    /// Spacing or Pixel Aspect Ratio; 1 when none says.
    pub fn pixel_aspect(&self, index: u32) -> f64 {
        let ds = self.data();
        let src = self
            .group_item(index, tags::PIXEL_MEASURES_SEQUENCE)
            .filter(|g| g.contains(tags::PIXEL_SPACING))
            .unwrap_or(ds);
        let pair = |v: Vec<f64>| {
            (v.len() >= 2 && v[0] > 0.0 && v[1] > 0.0 && v[0].is_finite() && v[1].is_finite()).then(|| v[0] / v[1])
        };
        let aspect = pair(src.numbers(tags::PIXEL_SPACING))
            .or_else(|| pair(ds.numbers(tags::IMAGER_PIXEL_SPACING)))
            .or_else(|| pair(ds.numbers(tags::PIXEL_ASPECT_RATIO)))
            .unwrap_or(1.0);
        aspect.clamp(0.1, 10.0)
    }

    /// The modality: CT, MR, CR, DX, US, ...
    pub fn modality(&self) -> String {
        self.data().string(tags::MODALITY).unwrap_or_default()
    }

    pub fn is_ct(&self) -> bool {
        self.modality() == "CT"
            || self.data().string(tags::RESCALE_TYPE).as_deref() == Some("HU")
            || self.data().string(tags::SOP_CLASS).is_some_and(|s| s.starts_with("1.2.840.10008.5.1.4.1.1.2"))
    }

    /// Seconds between frames of a cine loop, from Frame Time, Cine Rate or
    /// the recommended display rate.
    pub fn frame_interval(&self) -> Option<f64> {
        let ds = self.data();
        if let Some(ms) = ds.float(tags::FRAME_TIME).filter(|&t| t > 0.0) {
            return Some((ms / 1000.0).clamp(0.005, 10.0));
        }
        // Philips ultrasound writes the time since the previous frame for
        // each frame (the first is 0), in milliseconds.
        let steps: Vec<f64> =
            ds.numbers(tags::FRAME_TIME_VECTOR).into_iter().filter(|&t| t > 0.0 && t.is_finite()).collect();
        if !steps.is_empty() {
            return Some((steps.iter().sum::<f64>() / steps.len() as f64 / 1000.0).clamp(0.005, 10.0));
        }
        [tags::CINE_RATE, tags::RECOMMENDED_DISPLAY_FRAME_RATE]
            .iter()
            .find_map(|&t| ds.float(t).filter(|&r| r > 0.0))
            .map(|rate| (1.0 / rate).clamp(0.005, 10.0))
    }

    /// A short description of the image's encoding, such as "MONOCHROME2,
    /// 12 of 16 bits, signed, JPEG Lossless".
    pub fn encoding(&self) -> String {
        let ts = self.parsed.transfer.name();
        let Some(i) = &self.info else { return ts };
        let photometric = self.data().string(tags::PHOTOMETRIC).unwrap_or_else(|| "MONOCHROME2".into());
        let bits = if i.bits_stored == i.bits_allocated {
            format!("{} bits", i.bits_allocated)
        } else {
            format!("{} of {} bits", i.bits_stored, i.bits_allocated)
        };
        let sign = if i.float_bits > 0 {
            ", float"
        } else if i.signed {
            ", signed"
        } else {
            ""
        };
        format!("{photometric}, {bits}{sign}, {ts}")
    }

    /// Facts for the viewer's inspector, none identifying anyone.
    pub fn facts(&self) -> Vec<(String, String)> {
        let ds = self.data();
        let mut f = Vec::new();
        let mut put = |k: &str, v: Option<String>| {
            if let Some(v) = v.filter(|v| !v.is_empty()) {
                f.push((k.to_string(), v));
            }
        };
        put("Modality", ds.string(tags::MODALITY));
        let sop = ds.string(tags::SOP_CLASS).or_else(|| self.meta().string(tags::MEDIA_STORAGE_SOP_CLASS));
        put("Kind", sop.map(|s| uid_name(&s).map(str::to_string).unwrap_or(s)));
        put("Encoding", Some(self.encoding()));
        if let Some(i) = &self.info {
            put("Pixels", Some(format!("{} × {}", i.columns, i.rows)));
            if i.frames > 1 {
                put("Frames", Some(i.frames.to_string()));
            }
        }
        let spacing = ds.numbers(tags::PIXEL_SPACING);
        if spacing.len() >= 2 {
            put("Pixel spacing", Some(format!("{} × {} mm", spacing[1], spacing[0])));
        }
        put("Slice thickness", ds.float(tags::SLICE_THICKNESS).map(|t| format!("{t} mm")));
        put("Body part", ds.string(tags::BODY_PART));
        put("Study", ds.string(tags::STUDY_DESCRIPTION));
        put("Series", ds.string(tags::SERIES_DESCRIPTION));
        put("Series number", ds.string(tags::SERIES_NUMBER));
        put("Instance number", ds.string(tags::INSTANCE_NUMBER));
        let maker = [ds.string(tags::MANUFACTURER), ds.string(tags::MANUFACTURER_MODEL)]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(" ");
        put("Made with", Some(maker));
        if let Some(e) = &self.image_error {
            put("Image", Some(e.clone()));
        }
        f
    }

    /// A frame as a display picture: windowed, inverted if asked, and
    /// stretched to square pixels when `aspect` (pixel height over width)
    /// isn't 1.
    pub fn picture(&self, frame: &Frame, voi: &Voi, invert: bool, aspect: f64) -> Texture {
        let rgba = window::render(frame, voi, invert);
        let (w, h, rgba) = square_pixels(frame.width, frame.height, rgba, aspect);
        let mut tex = Texture::from_rgba8(w, h, rgba, ColorSpace::Srgb);
        tex.has_alpha = false;
        let (cols, rows) = self.info.as_ref().map_or((w, h), |i| (i.columns, i.rows));
        tex.source = SourceInfo {
            container: "DICOM".into(),
            format: self.encoding(),
            width: cols,
            height: rows,
            depth: 1,
            layers: self.frame_count().max(1),
            faces: 1,
            mip_count: 1,
            ..Default::default()
        };
        tex
    }

    /// The middle frame with its stored window, as thumbnails show it.
    pub fn middle_picture(&self, max_size: Option<u32>) -> Result<Texture, LoadError> {
        let index = self.frame_count() / 2;
        let frame = self.decode_frame(index, max_size)?;
        let voi = self.default_voi(index, &frame);
        Ok(self.picture(&frame, &voi, self.inverted(), self.pixel_aspect(index)))
    }
}

/// Keeps the first of three components (a monochrome image a writer
/// compressed as colour).
fn first_component(d: codec::Decoded) -> codec::Decoded {
    use codec::Samples;
    let take = |n: usize| -> Vec<usize> { (0..n).map(|i| i * 3).collect() };
    let pixels = d.width as usize * d.height as usize;
    let data = match d.data {
        Samples::U8(v) => Samples::U8(take(pixels).into_iter().map(|i| v[i]).collect()),
        Samples::U16(v) => Samples::U16(take(pixels).into_iter().map(|i| v[i]).collect()),
        Samples::U32(v) => Samples::U32(take(pixels).into_iter().map(|i| v[i]).collect()),
        Samples::F32(v) => Samples::F32(take(pixels).into_iter().map(|i| v[i]).collect()),
    };
    codec::Decoded { comps: 1, data, ..d }
}

/// Stretches RGBA rows or columns so pixels come out square, by linear
/// interpolation along the stretched direction.  The result stays within
/// 16384 pixels a side.
pub fn square_pixels(w: u32, h: u32, rgba: Vec<u8>, aspect: f64) -> (u32, u32, Vec<u8>) {
    if (aspect - 1.0).abs() < 0.01 || w == 0 || h == 0 {
        return (w, h, rgba);
    }
    let (ow, oh) = if aspect > 1.0 {
        (w, ((h as f64 * aspect).round() as u32).min(16384))
    } else {
        (((w as f64 / aspect).round() as u32).min(16384), h)
    };
    let (w_, h_, ow_, oh_) = (w as usize, h as usize, ow as usize, oh as usize);
    let mut out = vec![0u8; ow_ * oh_ * 4];
    let sample = |sx: f64, sy: f64| -> [u8; 4] {
        let (x0, y0) = (sx.floor().max(0.0) as usize, sy.floor().max(0.0) as usize);
        let (x0, y0) = (x0.min(w_ - 1), y0.min(h_ - 1));
        let (x1, y1) = ((x0 + 1).min(w_ - 1), (y0 + 1).min(h_ - 1));
        let (fx, fy) = ((sx - x0 as f64).clamp(0.0, 1.0), (sy - y0 as f64).clamp(0.0, 1.0));
        let px = |x: usize, y: usize, c: usize| rgba[(y * w_ + x) * 4 + c] as f64;
        let mut o = [0u8; 4];
        for (c, oc) in o.iter_mut().enumerate() {
            let top = px(x0, y0, c) * (1.0 - fx) + px(x1, y0, c) * fx;
            let bottom = px(x0, y1, c) * (1.0 - fx) + px(x1, y1, c) * fx;
            *oc = (top * (1.0 - fy) + bottom * fy).round() as u8;
        }
        o
    };
    for y in 0..oh_ {
        let sy = if oh_ == h_ { y as f64 } else { (y as f64 + 0.5) * h_ as f64 / oh_ as f64 - 0.5 };
        for x in 0..ow_ {
            let sx = if ow_ == w_ { x as f64 } else { (x as f64 + 0.5) * w_ as f64 / ow_ as f64 - 0.5 };
            out[(y * ow_ + x) * 4..][..4].copy_from_slice(&sample(sx, sy));
        }
    }
    (ow, oh, out)
}

/// The picture a file shows: an image's middle frame with its stored
/// window, or a DICOMDIR's first image when `neighbours` allows reading
/// other files.  `max_size` is the thumbnail size, if any.
pub fn picture_file(path: &Path, max_size: Option<u32>, neighbours: bool) -> Result<Texture, LoadError> {
    let dicom = Dicom::open(path)?;
    if dicom.is_directory() {
        if !neighbours {
            return Err(LoadError::Unsupported("a DICOMDIR's images are in other files".into()));
        }
        let dir = path.parent().filter(|d| !d.as_os_str().is_empty()).unwrap_or(Path::new("."));
        let records = dicomdir::records(dicom.data(), dir);
        let first = dicomdir::first_image(&records).ok_or_else(|| LoadError::invalid("the DICOMDIR names no image"))?;
        let image = Dicom::open(&first)?;
        if image.is_directory() {
            return Err(LoadError::invalid("the DICOMDIR names another DICOMDIR"));
        }
        return image.middle_picture(max_size);
    }
    dicom.middle_picture(max_size)
}

/// Loads a file as Slerp's other loaders do: the picture thumbnails show,
/// at most `limits.max_texture_size` on its longer side.
pub fn load_path(path: &Path, limits: &Limits) -> Result<Asset, LoadError> {
    let tex = picture_file(path, limits.max_texture_size, limits.neighbours)?;
    Ok(Asset::Image(Arc::new(tex)))
}
