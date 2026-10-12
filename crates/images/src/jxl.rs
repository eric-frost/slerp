//! JPEG XL (`.jxl`), decoded by jxl-rs, the libjxl team's Rust decoder.
//!
//! A file is a still picture, or an animation whose frames the decoder
//! composes (blending, layers and patches are its business) into full
//! pictures.  Which of three ways of asking the decoder for pixels is used
//! follows from the colour:
//!
//! - ordinary pictures come as 8-bit RGBA, converted to sRGB here when the
//!   file's colour space is wider (Display P3, Rec. 2020, a matrix profile);
//! - HDR pictures (PQ or HLG), linear light and floating point ones come as
//!   `f32`, which becomes linear sRGB floats for the viewer's exposure
//!   slider;
//! - CMYK pictures come as 8-bit CMYK, which is turned into RGB with the
//!   naive formula (the profile's lookup tables are not read).
//!
//! The decoder reports each picture in the encoded orientation; turning it
//! upright is done here.  Animations are indexed by a first pass that reads
//! only frame headers (delays, where to start decoding each frame), so any
//! frame can be shown without decoding the ones before it more than once.

use crate::animation::{Animation, Blend, Dispose, MAX_CANVAS_PIXELS, MAX_FRAMES, Patch, PatchSource, Rect};
use crate::color::{Conversion, Transfer, describe_icc, encode_srgb8, tone_map};
use crate::orient::{describe, orient, swaps_axes};
use jxl::api::{
    JxlBasicInfo, JxlBitDepth, JxlColorEncoding, JxlColorProfile, JxlDataFormat, JxlDecoder, JxlDecoderOptions,
    JxlOutputBuffer, JxlParallelRunner, JxlParallelRunnerFun, JxlPixelFormat, JxlPrimaries, JxlTransferFunction,
    JxlWhitePoint, ProcessingResult, VisibleFrameInfo, states,
};
use jxl::headers::extra_channels::ExtraChannel;
use slerp_scene::LoadError;
use std::path::Path;
use std::sync::Arc;

/// The most pixels of a picture decoded to 8 bits per channel (640 MB).
pub const MAX_PIXELS: u64 = 160 << 20;

/// The most pixels of a picture decoded to floating point, 16 bytes each.
pub const MAX_FLOAT_PIXELS: u64 = 40 << 20;

/*┌─────────────────╮
| Decoder session |
└─────────────────╯*/
//region Decoder session

/// jxl-rs brings no thread pool: the caller runs its tasks, and rayon's
/// already there.
struct Runner;

impl JxlParallelRunner for Runner {
    fn run(&mut self, num: usize, fun: &JxlParallelRunnerFun<'_>) -> jxl::error::Result<()> {
        let first_error = std::sync::Mutex::new(None);
        rayon::scope(|s| {
            for i in 0..num {
                let first_error = &first_error;
                s.spawn(move |_| {
                    if let Err(e) = fun(i)
                        && let Ok(mut slot) = first_error.lock()
                    {
                        slot.get_or_insert(e);
                    }
                });
            }
        });
        match first_error.into_inner() {
            Ok(Some(e)) => Err(e),
            _ => Ok(()),
        }
    }

    fn num_threads(&self) -> usize {
        rayon::current_num_threads()
    }
}

fn err(e: jxl::error::Error) -> String {
    format!("JPEG XL: {e}")
}

fn cut_short() -> String {
    "the file is cut short".into()
}

/// How the decoder is asked for pixels.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Samples {
    /// 8-bit RGBA.
    Eight,
    /// `f32` RGBA, for HDR, linear light and floating point pictures.
    Float,
    /// 8-bit CMYK, whose K is the picture's black extra channel.
    Cmyk,
}

/// What a picture is made of and how to read what the decoder gives.
struct Plan {
    samples: Samples,
    /// From the decoder's output colour space to sRGB; `None` when it already is.
    conversion: Option<Conversion>,
    /// What the inspector says about colour.
    colour: String,
    /// EXIF orientation, 1 to 8.
    orientation: u8,
    /// The size the decoder gives, before the picture is turned upright.
    size: (usize, usize),
    /// Which extra channel is the opacity of a CMYK picture, which comes apart from the colour.
    alpha: Option<usize>,
}

/// A decoded picture, upright: 8-bit sRGB or linear sRGB floats.
pub enum Pixels {
    Rgba8(Vec<u8>),
    Rgba32F(Vec<f32>),
}

pub struct Picture {
    pub width: u32,
    pub height: u32,
    pub pixels: Pixels,
}

/// A decoder that has read the file's header and is ready for a frame.
struct Session {
    /// `None` after an error, until the next seek starts afresh.
    dec: Option<JxlDecoder<states::WithImageInfo>>,
    /// Where in the file the decoder reads next.
    pos: usize,
    /// The frame the decoder gives next.
    next: usize,
    info: JxlBasicInfo,
    embedded: JxlColorProfile,
    plan: Plan,
}

#[allow(clippy::field_reassign_with_default)]
fn options() -> JxlDecoderOptions {
    let mut o = JxlDecoderOptions::default();
    // Samples are pixels times channels, counting the extra ones.
    o.sample_limit = Some(MAX_PIXELS as usize * 4);
    o
}

fn pixel_limit(samples: Samples) -> u64 {
    match samples {
        Samples::Float => MAX_FLOAT_PIXELS,
        _ => MAX_PIXELS,
    }
}

impl Session {
    /// Reads the header and chooses how to take the pixels.
    fn open(data: &[u8]) -> Result<Session, String> {
        let mut input = data;
        let dec = JxlDecoder::<states::Initialized>::new(options());
        let mut dec = match dec.process(&mut input, Some(&mut Runner)).map_err(err)? {
            ProcessingResult::Complete { result } => result,
            ProcessingResult::NeedsMoreInput { .. } => {
                return Err("the file is cut short before its header ends".into());
            }
        };
        let info = dec.basic_info().clone();
        let embedded = dec.embedded_color_profile().clone();
        let (w, h) = info.size;
        if w == 0 || h == 0 {
            return Err("the picture is empty".into());
        }
        let samples = choose(&info, &embedded);
        let limit = pixel_limit(samples);
        if w as u64 * h as u64 > limit {
            return Err(format!("{w} × {h} pixels is more than is opened ({} megapixels)", limit >> 20));
        }
        let extra = info.extra_channels.len();
        let alpha = (samples == Samples::Cmyk)
            .then(|| info.extra_channels.iter().position(|c| c.ec_type == ExtraChannel::Alpha))
            .flatten();
        let mut format = match samples {
            Samples::Eight => JxlPixelFormat::rgba8(extra),
            Samples::Float => JxlPixelFormat::rgba_f32(extra),
            Samples::Cmyk => JxlPixelFormat::cmyk8(extra),
        };
        if let Some(i) = alpha {
            format.extra_channel_format[i] = Some(JxlDataFormat::U8 { bit_depth: 8 });
        }
        dec.set_pixel_format(format).map_err(err)?;
        let output = dec.output_color_profile().clone();
        let (conversion, colour) = colour_of(&embedded, &output, samples);
        let plan = Plan { samples, conversion, colour, orientation: info.orientation as u8, size: (w, h), alpha };
        Ok(Session { dec: Some(dec), pos: data.len() - input.len(), next: 0, info, embedded, plan })
    }

    /// Decodes the frame the decoder is at and moves on.
    fn frame(&mut self, data: &[u8]) -> Result<Picture, String> {
        let dec = self.dec.take().ok_or("the decoder isn't started")?;
        let mut input = data.get(self.pos..).ok_or("the decoder reached past the file")?;
        let (w, h) = self.plan.size;
        // 1. The frame's header.
        let with_frame = match dec.process(&mut input, Some(&mut Runner)).map_err(err)? {
            ProcessingResult::Complete { result } => result,
            ProcessingResult::NeedsMoreInput { .. } => return Err(cut_short()),
        };
        // 2. Its pixels, into buffers the rows of which are aligned for the sample type.
        let count = w.checked_mul(h).and_then(|n| n.checked_mul(4)).ok_or("the picture is too large")?;
        let (pixels, dec) = match self.plan.samples {
            Samples::Float => {
                let mut v = vec![0f32; count];
                // SAFETY: `v` holds `count` f32s, which is `count * 4` bytes, aligned for f32;
                // the byte view ends before `v` is used again.
                let bytes = unsafe { std::slice::from_raw_parts_mut(v.as_mut_ptr() as *mut u8, count * 4) };
                let dec = run_frame(with_frame, &mut input, &mut [JxlOutputBuffer::new(bytes, h, w * 16)])?;
                (Pixels::Rgba32F(v), dec)
            }
            Samples::Eight => {
                let mut v = vec![0u8; count];
                let dec = run_frame(with_frame, &mut input, &mut [JxlOutputBuffer::new(&mut v, h, w * 4)])?;
                (Pixels::Rgba8(v), dec)
            }
            Samples::Cmyk => {
                // The opacity, when there is any, comes as a plane of its own.
                let mut v = vec![0u8; count];
                let mut alpha = vec![0u8; if self.plan.alpha.is_some() { w * h } else { 0 }];
                let dec = if alpha.is_empty() {
                    run_frame(with_frame, &mut input, &mut [JxlOutputBuffer::new(&mut v, h, w * 4)])?
                } else {
                    let mut buffers = [JxlOutputBuffer::new(&mut v, h, w * 4), JxlOutputBuffer::new(&mut alpha, h, w)];
                    run_frame(with_frame, &mut input, &mut buffers)?
                };
                cmyk_to_rgba(&mut v, &alpha);
                (Pixels::Rgba8(v), dec)
            }
        };
        self.dec = Some(dec);
        self.pos = data.len() - input.len();
        self.next += 1;
        Ok(self.finish(pixels))
    }

    /// Colour to sRGB, then upright.
    fn finish(&self, pixels: Pixels) -> Picture {
        let (w, h) = self.plan.size;
        let pixels = match (pixels, self.plan.samples) {
            // CMYK was turned into RGB as it was read.
            (Pixels::Rgba8(v), Samples::Cmyk) => Pixels::Rgba8(v),
            (Pixels::Rgba8(mut v), _) => {
                if let Some(c) = &self.plan.conversion {
                    c.convert_rgba8(&mut v);
                }
                Pixels::Rgba8(v)
            }
            (Pixels::Rgba32F(mut v), _) => {
                if let Some(c) = &self.plan.conversion {
                    c.float_rgba_to_linear(&mut v);
                }
                Pixels::Rgba32F(v)
            }
        };
        let o = self.plan.orientation;
        let (pixels, w, h) = match pixels {
            Pixels::Rgba8(v) => {
                let (v, w, h) = orient(&v, w, h, 4, o);
                (Pixels::Rgba8(v), w, h)
            }
            Pixels::Rgba32F(v) => {
                let (v, w, h) = orient(&v, w, h, 4, o);
                (Pixels::Rgba32F(v), w, h)
            }
        };
        Picture { width: w as u32, height: h as u32, pixels }
    }
}

/// Decodes the pixels of the frame `dec` has the header of into `buffers`.
fn run_frame(
    dec: JxlDecoder<states::WithFrameInfo>,
    input: &mut &[u8],
    buffers: &mut [JxlOutputBuffer<'_>],
) -> Result<JxlDecoder<states::WithImageInfo>, String> {
    match dec.process(input, buffers, Some(&mut Runner)).map_err(err)? {
        ProcessingResult::Complete { result } => Ok(result),
        ProcessingResult::NeedsMoreInput { .. } => Err(cut_short()),
    }
}

/// Which way of taking pixels suits the picture.
fn choose(info: &JxlBasicInfo, embedded: &JxlColorProfile) -> Samples {
    if embedded.is_cmyk() {
        return Samples::Cmyk;
    }
    let light = matches!(
        embedded.transfer_function(),
        Some(JxlTransferFunction::PQ | JxlTransferFunction::HLG | JxlTransferFunction::Linear)
    );
    if light || matches!(info.bit_depth, JxlBitDepth::Float { .. }) { Samples::Float } else { Samples::Eight }
}

/// The conversion from the colour the decoder outputs to sRGB, and what to
/// tell the inspector about it.
fn colour_of(embedded: &JxlColorProfile, output: &JxlColorProfile, samples: Samples) -> (Option<Conversion>, String) {
    if samples == Samples::Cmyk {
        return (None, format!("{}, converted to RGB without the profile's lookup tables", describe_profile(embedded)));
    }
    let source = describe_profile(embedded);
    let Some(conversion) = conversion_of(output) else {
        let shown = if samples == Samples::Float { "as linear sRGB" } else { "as sRGB" };
        return (None, format!("{source}, not applied (shown {shown})"));
    };
    if samples == Samples::Float {
        let hdr = if conversion.is_hdr() { ", shown through the HDR path" } else { ", linear light" };
        return (Some(conversion), format!("{source}{hdr}"));
    }
    if conversion.is_srgb() {
        (None, format!("{source}, already sRGB"))
    } else {
        (Some(conversion), format!("{source}, converted to sRGB"))
    }
}

/// How colour in `profile` becomes linear sRGB, when it's a simple colour
/// space or a matrix and curve ICC profile.
fn conversion_of(profile: &JxlColorProfile) -> Option<Conversion> {
    let name = describe_profile(profile);
    match profile {
        JxlColorProfile::Simple(JxlColorEncoding::RgbColorSpace {
            white_point, primaries, transfer_function, ..
        }) => {
            // Rounded, so that the white point of sRGB is exactly D65 and needs no adaptation.
            let xy = |p: (f32, f32)| [(p.0 as f64 * 1e5).round() / 1e5, (p.1 as f64 * 1e5).round() / 1e5];
            let [r, g, b] = primaries.to_xy_coords();
            Conversion::from_chromaticities(
                [xy(r), xy(g), xy(b), xy(white_point.to_xy_coords())],
                transfer_of(transfer_function),
                name,
            )
        }
        JxlColorProfile::Simple(JxlColorEncoding::GrayscaleColorSpace { transfer_function, .. }) => {
            Some(Conversion::grey(transfer_of(transfer_function), name))
        }
        JxlColorProfile::Simple(JxlColorEncoding::XYB { .. }) => None,
        JxlColorProfile::Icc(bytes) => Conversion::from_icc(bytes),
    }
}

fn transfer_of(t: &JxlTransferFunction) -> Transfer {
    match t {
        JxlTransferFunction::BT709 => Transfer::bt709(),
        JxlTransferFunction::Linear => Transfer::Linear,
        JxlTransferFunction::SRGB => Transfer::Srgb,
        JxlTransferFunction::PQ => Transfer::Pq,
        JxlTransferFunction::HLG => Transfer::Hlg,
        JxlTransferFunction::DCI => Transfer::Gamma(2.6),
        // JPEG XL gives the exponent that turns light into the encoded value.
        JxlTransferFunction::Gamma(g) if *g > 0.0 => Transfer::Gamma(1.0 / *g as f64),
        JxlTransferFunction::Gamma(_) => Transfer::Srgb,
    }
}

fn describe_transfer(t: &JxlTransferFunction) -> String {
    match t {
        JxlTransferFunction::BT709 => "BT.709".into(),
        JxlTransferFunction::Linear => "linear".into(),
        JxlTransferFunction::SRGB => "sRGB".into(),
        JxlTransferFunction::PQ => "PQ (SMPTE ST 2084)".into(),
        JxlTransferFunction::HLG => "HLG".into(),
        JxlTransferFunction::DCI => "DCI gamma 2.6".into(),
        JxlTransferFunction::Gamma(g) if *g > 0.0 => format!("gamma {:.2}", 1.0 / g),
        JxlTransferFunction::Gamma(g) => format!("gamma {g}"),
    }
}

/// A colour profile in words for the inspector.
fn describe_profile(profile: &JxlColorProfile) -> String {
    match profile {
        JxlColorProfile::Icc(bytes) => format!("ICC profile {}", describe_icc(bytes)),
        JxlColorProfile::Simple(JxlColorEncoding::RgbColorSpace {
            white_point, primaries, transfer_function, ..
        }) => {
            let p = match primaries {
                JxlPrimaries::SRGB => "sRGB (Rec. 709)",
                JxlPrimaries::BT2100 => "Rec. 2020",
                JxlPrimaries::P3 if *white_point == JxlWhitePoint::DCI => "DCI P3",
                JxlPrimaries::P3 => "Display P3",
                JxlPrimaries::Chromaticities { .. } => "custom",
            };
            format!("{p} primaries, {} transfer", describe_transfer(transfer_function))
        }
        JxlColorProfile::Simple(JxlColorEncoding::GrayscaleColorSpace { transfer_function, .. }) => {
            format!("grey, {} transfer", describe_transfer(transfer_function))
        }
        JxlColorProfile::Simple(JxlColorEncoding::XYB { .. }) => "XYB".into(),
    }
}

/// CMYK bytes (255 is full ink) to RGBA in place, with the opacity plane of
/// the picture, or none for an opaque one.  The profile's lookup tables are
/// not applied: this is the textbook formula.
fn cmyk_to_rgba(px: &mut [u8], alpha: &[u8]) {
    for (i, p) in px.chunks_exact_mut(4).enumerate() {
        let paper = 255 - p[3] as u32;
        let ch = |ink: u8| ((255 - ink as u32) * paper / 255) as u8;
        let rgb = [ch(p[0]), ch(p[1]), ch(p[2])];
        p.copy_from_slice(&[rgb[0], rgb[1], rgb[2], alpha.get(i).copied().unwrap_or(255)]);
    }
}

//endregion

/*┌─────────────────╮
| Animations      |
└─────────────────╯*/
//region Animations

/// Reads every frame's header to learn how many there are, how long each
/// shows and where decoding of each can start.  A file that ends or breaks
/// part way gives the frames before the damage and says so.
fn scan(data: &[u8]) -> Result<(Vec<VisibleFrameInfo>, Option<String>), String> {
    let mut opts = options();
    opts.scan_frames_only = true;
    let mut input = data;
    let dec = JxlDecoder::<states::Initialized>::new(opts);
    let mut dec = match dec.process(&mut input, None).map_err(err)? {
        ProcessingResult::Complete { result } => result,
        ProcessingResult::NeedsMoreInput { .. } => return Err(cut_short()),
    };
    // The frames whose sections were read in full.
    let mut seen: Vec<VisibleFrameInfo> = Vec::new();
    let damaged = "the file is damaged or cut short, so its last frames are missing".to_string();
    while dec.has_more_frames() {
        let with_frame = match dec.process(&mut input, None) {
            Ok(ProcessingResult::Complete { result }) => result,
            _ => return finish_scan(seen, Some(damaged)),
        };
        dec = match with_frame.skip_frame(&mut input) {
            Ok(ProcessingResult::Complete { result }) => result,
            _ => return finish_scan(seen, Some(damaged)),
        };
        seen.extend(dec.scanned_frames().get(seen.len()).cloned());
        if seen.len() > MAX_FRAMES {
            return Err("the animation has too many frames".into());
        }
    }
    finish_scan(seen, None)
}

fn finish_scan(
    seen: Vec<VisibleFrameInfo>,
    warning: Option<String>,
) -> Result<(Vec<VisibleFrameInfo>, Option<String>), String> {
    if seen.is_empty() { Err("the animation has no complete frame".into()) } else { Ok((seen, warning)) }
}

/// Where an animation's frames come from: decoded one after the other, with
/// a seek whenever the next frame asked for isn't the one that follows.
struct JxlSource {
    data: Arc<Vec<u8>>,
    frames: Vec<VisibleFrameInfo>,
    session: Session,
    /// The size of the upright pictures.
    size: (u32, u32),
}

impl PatchSource for JxlSource {
    fn patch(&mut self, index: usize) -> Result<Patch, String> {
        let target = self.frames.get(index).ok_or("no such frame")?.seek_target;
        let s = &mut self.session;
        if s.dec.is_none() {
            // After an error: start again from the header.
            *s = Session::open(&self.data)?;
        }
        if s.next != index {
            let dec = s.dec.as_mut().ok_or("the decoder isn't started")?;
            dec.start_new_frame(target);
            s.pos = usize::try_from(target.decode_start_file_offset).map_err(|_| "a frame lies outside the file")?;
            s.next = index;
        }
        let picture = s.frame(&self.data)?;
        if (picture.width, picture.height) != self.size {
            return Err("frames of different sizes aren't supported".into());
        }
        let rgba = match picture.pixels {
            Pixels::Rgba8(v) => v,
            // Animations are 8-bit sRGB: HDR frames are tone mapped.
            Pixels::Rgba32F(v) => v
                .chunks_exact(4)
                .flat_map(|p| {
                    let c = tone_map([p[0], p[1], p[2]]);
                    [
                        encode_srgb8(c[0]),
                        encode_srgb8(c[1]),
                        encode_srgb8(c[2]),
                        (p[3].clamp(0.0, 1.0) * 255.0 + 0.5) as u8,
                    ]
                })
                .collect(),
        };
        let (w, h) = self.size;
        Ok(Patch { rect: Rect { x: 0, y: 0, w, h }, rgba, blend: Blend::Replace, dispose: Dispose::None })
    }
}

//endregion

/*┌─────────────────╮
| Loading         |
└─────────────────╯*/
//region Loading

/// What opening a file gave.
pub enum Opened {
    Still(Picture, Vec<(String, String)>),
    Animation(Animation),
}

/// The facts a JPEG XL file gives, for the inspector.
fn facts(s: &Session) -> Vec<(String, String)> {
    let info = &s.info;
    let mut f = Vec::new();
    f.push((
        "Bit depth".to_string(),
        match &info.bit_depth {
            JxlBitDepth::Int { bits_per_sample } => format!("{bits_per_sample} bits per sample"),
            JxlBitDepth::Float { bits_per_sample, exponent_bits_per_sample } => {
                format!("{bits_per_sample}-bit floating point ({exponent_bits_per_sample} exponent bits)")
            }
        },
    ));
    f.push((
        "Coding".into(),
        if info.uses_original_profile { "original colours (lossless or palette)" } else { "XYB (lossy)" }.into(),
    ));
    let extra: Vec<String> = info.extra_channels.iter().map(|c| format!("{:?}", c.ec_type).to_lowercase()).collect();
    if !extra.is_empty() {
        f.push(("Extra channels".into(), extra.join(", ")));
    }
    if (info.tone_mapping.intensity_target - 255.0).abs() > 0.5 {
        f.push(("Intensity target".into(), format!("{} nits", info.tone_mapping.intensity_target)));
    }
    if let Some((w, h)) = info.preview_size {
        f.push(("Preview".into(), format!("{w} × {h}, not shown")));
    }
    f.push(("Colour".into(), s.plan.colour.clone()));
    if s.plan.orientation != 1 {
        f.push((
            "Orientation".into(),
            format!("{} (EXIF {}), shown upright", describe(s.plan.orientation), s.plan.orientation),
        ));
    }
    let _ = &s.embedded;
    f
}

/// Opens the file's pictures: one still, or the frames of an animation.
pub fn open(data: Vec<u8>) -> Result<(Opened, Vec<String>), String> {
    let data = Arc::new(data);
    let mut session = Session::open(&data)?;
    let mut facts = facts(&session);
    let (w, h) = session.plan.size;
    let upright = if swaps_axes(session.plan.orientation) { (h, w) } else { (w, h) };
    let mut warnings = Vec::new();
    if session.info.animation.is_some() {
        let (frames, warning) = scan(&data)?;
        warnings.extend(warning);
        if frames.len() > 1 {
            if upright.0 as u64 * upright.1 as u64 > MAX_CANVAS_PIXELS {
                return Err(format!("{} × {} pixels is more than animations show", upright.0, upright.1));
            }
            let a = session.info.animation.clone().ok_or("no animation header")?;
            let delays: Vec<f32> =
                frames.iter().map(|f| crate::animation::delay_seconds(f.duration_ms.round() as u32)).collect();
            let plays = (a.num_loops != 0).then_some(a.num_loops);
            facts.push(("Ticks per second".into(), format!("{} / {}", a.tps_numerator, a.tps_denominator)));
            let size = (upright.0 as u32, upright.1 as u32);
            let info = slerp_scene::Info {
                format: "JPEG XL".into(),
                warnings: warnings.clone(),
                details: facts.clone(),
                ..Default::default()
            };
            let source = JxlSource { data: data.clone(), frames, session, size };
            return Ok((
                Opened::Animation(Animation { info, size, delays, plays, source: Box::new(source) }),
                warnings,
            ));
        }
    }
    let picture = session.frame(&data)?;
    Ok((Opened::Still(picture, facts), warnings))
}

/// Opens a `.jxl` file as the viewer wants it.
pub fn load(path: &Path, limits: &slerp_scene::Limits) -> Result<slerp_scene::Asset, LoadError> {
    let bytes = crate::read_file(path, limits)?;
    let file_bytes = bytes.len() as u64;
    let (opened, warnings) = open(bytes).map_err(crate::invalid)?;
    match opened {
        Opened::Still(picture, mut header) => {
            for w in warnings {
                header.push(("Warning".into(), w));
            }
            let format = "JPEG XL".to_string();
            let mut tex = match picture.pixels {
                Pixels::Rgba8(px) => crate::still("JPEG XL", &format, picture.width, picture.height, px, header),
                Pixels::Rgba32F(px) => {
                    let mut t = slerp_scene::Texture::from_rgba32f(picture.width, picture.height, px);
                    t.source.container = "JPEG XL".into();
                    t.source.format = format;
                    t.source.header = header;
                    t
                }
            };
            tex.source.file_bytes = file_bytes;
            Ok(slerp_scene::Asset::Image(Arc::new(tex)))
        }
        Opened::Animation(mut animation) => {
            let mut details = crate::animation_details(animation.size, &animation.delays, animation.plays);
            details.extend(animation.info.details.iter().cloned());
            details.push(("File size".into(), format!("{file_bytes} bytes")));
            animation.info.details = details;
            let info = animation.info.clone();
            Ok(crate::animation_asset("JPEG XL", "JPEG XL animation", info, animation, limits, file_bytes))
        }
    }
}

//endregion

#[cfg(test)]
mod tests {
    use super::*;

    /// Test files of jxl-rs: the same coded pixels with orientation 1 and 6.
    const UPRIGHT: &str =
        "ff0af807c680480804010098004b2825d681828200a8b11b83b117080100900089d31f98bfbf3fd45fffd759f62e9a587a592c";
    const TURNED: &str =
        "ff0af807c65820260804010098004b2825d681828200a8b11b83b117080100900089d31f98bfbf3fd45fffd759f62e9a587a592c";
    const ONE_PIXEL: &str = "ff0a001010090800010030004b188b15c241111e40acf20c";

    fn unhex(s: &str) -> Vec<u8> {
        (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
    }

    fn still(hex: &str) -> Picture {
        match open(unhex(hex)).unwrap().0 {
            Opened::Still(p, _) => p,
            Opened::Animation(_) => panic!("an animation"),
        }
    }

    #[test]
    fn a_tiny_picture_decodes() {
        let p = still(ONE_PIXEL);
        assert_eq!((p.width, p.height), (1, 1));
        let Pixels::Rgba8(v) = &p.pixels else { panic!("not 8-bit") };
        assert_eq!(v.len(), 4);
        assert_eq!(v[3], 255);
    }

    #[test]
    fn orientation_turns_the_picture_upright() {
        // The two files differ only in their header: one stores 100 by 256 pixels upright,
        // the other 256 by 100 pixels to be turned 90 degrees clockwise.
        let a = still(UPRIGHT);
        let b = still(TURNED);
        assert_eq!((a.width, a.height), (100, 256));
        assert_eq!((b.width, b.height), (100, 256));
        let Opened::Still(_, facts) = open(unhex(TURNED)).unwrap().0 else { panic!("an animation") };
        assert!(facts.iter().any(|(k, v)| k == "Orientation" && v.contains("EXIF 6")), "{facts:?}");
    }

    #[test]
    fn damaged_files_are_errors() {
        let good = unhex(UPRIGHT);
        for cut in 0..good.len() {
            let _ = open(good[..cut].to_vec());
        }
        assert!(open(good[..10].to_vec()).is_err());
        assert!(open(Vec::new()).is_err());
        let mut bad = good.clone();
        for i in 2..bad.len() {
            bad[i] ^= 0xa5;
            let _ = open(bad.clone());
            bad[i] ^= 0xa5;
        }
    }

    #[test]
    fn cmyk_ink_is_taken_from_white() {
        // Full black ink is black whatever the colours say, no ink at all is white.
        let mut px = [0u8, 0, 0, 255, 255, 255, 255, 255, 0, 0, 0, 0, 255, 0, 0, 0];
        cmyk_to_rgba(&mut px, &[255, 255, 255, 40]);
        assert_eq!(px, [0, 0, 0, 255, 0, 0, 0, 255, 255, 255, 255, 255, 0, 255, 255, 40]);
    }
}
