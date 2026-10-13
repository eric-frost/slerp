//! Ordinary image formats for Slerp's viewer, the ones every photo viewer
//! opens: GIF, APNG and animated WebP (played as animations), ICO and CUR
//! (every size of an icon), PNM, ordinary TIFF, JPEG 2000, JPEG XL, AVIF and
//! HEIC; and from phase 23 QOI, Apple icons (ICNS), SGI, XPM, XBM and
//! OpenEXR (through `slerp-openexr`, one layer or part at a time).  Still
//! JPEG, PNG and WebP photographs get their EXIF orientation and colour
//! profile applied too (see [`photo`]).
//!
//! File managers already make thumbnails of these formats, so Slerp does not
//! offer to; the viewer, `slerp info` and `slerp render` open them.
//!
//! - Animations become a [`slerp_scene::Canvas`] whose frames are decoded
//!   as they are played and kept while they fit [`slerp_scene::Limits::frame_bytes`].
//! - Icons become a gallery with one picture per size.
//! - Everything else is one [`slerp_scene::Texture`]: 8-bit pictures as
//!   sRGB, wide-gamut and HDR ones as linear floats the viewer's exposure
//!   slider handles.
//!
//! Like every decoder here, these treat files as hostile: sizes are checked
//! before anything is allocated, a damaged file gives an error or the frames
//! that were readable, and nothing loops without a bound.

mod animation;
mod apng;
mod av1;
mod avif;
mod color;
mod exif;
mod gif;
#[cfg(feature = "heic")]
mod heif;
mod icns;
mod ico;
mod isobmff;
mod jp2;
#[cfg(feature = "jxl")]
mod jxl;
mod openexr;
mod orient;
pub mod photo;
mod pnm;
mod qoi;
mod scale;
mod sgi;
mod tiff;
mod webp;
mod x11_colors;
mod xbm;
mod xpm;
mod yuv;

pub use animation::{AnimatedImage, MAX_CANVAS_PIXELS, MAX_FRAMES, delay_seconds};
/// The container reader, for the `heif_dump` example.
#[doc(hidden)]
pub use isobmff::Heif as HeifForTest;
/// Turns RGBA pixels as EXIF orientation 1 to 8 asks (for tests of other crates and this one's).
#[doc(hidden)]
pub use orient::orient as orient_for_test;

use slerp_scene::{Asset, ColorSpace, Info, Limits, LoadError, Texture};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

/// The decoders that run isolated (see `slerp-isolate`): the AV1 decoder
/// for AVIF and the system's libheif for HEIC, which are C or behind a C
/// interface where a damaged file can crash or hang them.
pub static JOBS: slerp_isolate::Jobs = &[
    &avif::STILL_JOB,
    &avif::SEQUENCE_JOB,
    #[cfg(feature = "heic")]
    &heif::DECODE_JOB,
];

/// What an isolated decoder of a picture whose pixels take `pixel_bytes`
/// may use: 30 seconds, a few times the picture in working memory, and the
/// picture as its answer.
fn limits_for_pixels(pixel_bytes: usize) -> slerp_isolate::Limits {
    slerp_isolate::Limits {
        time: Duration::from_secs(30),
        memory: (pixel_bytes as u64).saturating_mul(6).saturating_add(768 << 20),
        output: pixel_bytes.saturating_add(1 << 16),
    }
}

/// The formats this crate opens.  JPEG, PNG and WebP stills belong to the
/// texture crate; animated PNG and WebP files are recognised here.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImageFormat {
    Gif,
    /// Animated PNG: a PNG with an `acTL` chunk.
    Apng,
    /// Animated WebP.
    AnimatedWebp,
    /// A TIFF that is a picture, as opposed to a stack of microscope slices
    /// (see `slerp_volumes`).
    Tiff,
    /// Windows icons (`.ico`) and cursors (`.cur`).
    Ico,
    /// Netpbm's PBM, PGM and PPM, ASCII or binary.
    Pnm,
    /// JPEG 2000 (`.jp2`, `.j2k`, `.jpf`).
    Jp2,
    /// JPEG XL.
    Jxl,
    /// AVIF.
    Avif,
    /// HEIC and other HEIF files whose pictures are not AV1.
    Heif,
    /// The Quite OK Image format.
    Qoi,
    /// Apple icons, every size.
    Icns,
    /// Silicon Graphics' format (`.sgi`, `.rgb`, `.rgba`, `.bw`).
    Sgi,
    /// The X PixMap.
    Xpm,
    /// The X BitMap.
    Xbm,
    /// OpenEXR, one layer or part at a time.
    Exr,
}

impl ImageFormat {
    pub fn name(self) -> &'static str {
        match self {
            ImageFormat::Gif => "GIF",
            ImageFormat::Apng => "APNG",
            ImageFormat::AnimatedWebp => "WebP (animated)",
            ImageFormat::Tiff => "TIFF",
            ImageFormat::Ico => "Windows icon",
            ImageFormat::Pnm => "Netpbm",
            ImageFormat::Jp2 => "JPEG 2000",
            ImageFormat::Jxl => "JPEG XL",
            ImageFormat::Avif => "AVIF",
            ImageFormat::Heif => "HEIF",
            ImageFormat::Qoi => "QOI",
            ImageFormat::Icns => "Apple icon",
            ImageFormat::Sgi => "SGI",
            ImageFormat::Xpm => "XPM",
            ImageFormat::Xbm => "XBM",
            ImageFormat::Exr => "OpenEXR",
        }
    }
}

/// Files larger than this are refused outright; the readers here hold the
/// file in memory.
const MAX_FILE_BYTES: u64 = 1 << 30;

/// Recognises a file from its first bytes and name.
pub fn sniff(path: &Path, head: &[u8]) -> Option<ImageFormat> {
    if gif::Gif::is_gif(head) {
        return Some(ImageFormat::Gif);
    }
    if webp::is_animated(head) {
        return Some(ImageFormat::AnimatedWebp);
    }
    if jp2::is_jp2(head) {
        return Some(ImageFormat::Jp2);
    }
    if jxl_is(path, head) {
        return Some(ImageFormat::Jxl);
    }
    if ico::is_icon(head) {
        return Some(ImageFormat::Ico);
    }
    if head.starts_with(&[0x76, 0x2f, 0x31, 0x01]) {
        return Some(ImageFormat::Exr);
    }
    if qoi::is_qoi(head) {
        return Some(ImageFormat::Qoi);
    }
    if icns::is_icns(head) {
        return Some(ImageFormat::Icns);
    }
    if sgi::is_sgi(head) {
        return Some(ImageFormat::Sgi);
    }
    if xpm::is_xpm(head) {
        return Some(ImageFormat::Xpm);
    }
    if xbm::is_xbm(head) {
        return Some(ImageFormat::Xbm);
    }
    if pnm::is_pnm(head) {
        return Some(ImageFormat::Pnm);
    }
    if tiff::is_tiff(head) && tiff::probe(path) {
        return Some(ImageFormat::Tiff);
    }
    if let Some(b) = isobmff::brands_of(head) {
        if b.has(b"avif") || b.has(b"avis") {
            return Some(ImageFormat::Avif);
        }
        const HEIF: [&[u8; 4]; 10] =
            [b"heic", b"heix", b"hevc", b"hevx", b"heim", b"heis", b"hevm", b"hevs", b"mif1", b"msf1"];
        if HEIF.iter().any(|x| b.has(x)) {
            return Some(ImageFormat::Heif);
        }
    }
    if head.starts_with(b"\x89PNG\r\n\x1a\n") {
        // The animation chunk comes before the image data; a big colour
        // profile can push it past the bytes sniffing is given.
        let animated = apng::head_is_animated(head).unwrap_or_else(|| file_is_animated_png(path));
        return animated.then_some(ImageFormat::Apng);
    }
    None
}

/// Whether the file is JPEG XL.  The signature is looked for even when the
/// `jxl` feature is off, so that the viewer can say what is missing instead
/// of showing nothing.
fn jxl_is(path: &Path, head: &[u8]) -> bool {
    const CONTAINER: [u8; 12] = [0, 0, 0, 0x0c, b'J', b'X', b'L', b' ', 0x0d, 0x0a, 0x87, 0x0a];
    head.starts_with(&CONTAINER)
        || head.starts_with(&[0xff, 0x0a]) && path.extension().is_some_and(|e| e.eq_ignore_ascii_case("jxl"))
}

/// Reads further into a PNG to see whether it is animated.
fn file_is_animated_png(path: &Path) -> bool {
    use std::io::Read;
    let mut head = Vec::new();
    let read = std::fs::File::open(path).and_then(|f| f.take(4 << 20).read_to_end(&mut head));
    read.is_ok() && apng::head_is_animated(&head).unwrap_or(false)
}

/// Opens a file of the given format.
pub fn load(format: ImageFormat, path: &Path, limits: &Limits) -> Result<Asset, LoadError> {
    let guard = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match format {
        ImageFormat::Gif => load_gif(path, limits),
        ImageFormat::Apng => load_apng(path, limits),
        ImageFormat::AnimatedWebp => load_webp(path, limits),
        ImageFormat::Ico => load_ico(path, limits),
        ImageFormat::Pnm => load_pnm(path, limits),
        ImageFormat::Tiff => load_tiff(path, limits),
        ImageFormat::Jp2 => load_jp2(path, limits),
        ImageFormat::Avif | ImageFormat::Heif => load_heif(path, limits),
        ImageFormat::Jxl => load_jxl(path, limits),
        ImageFormat::Qoi => load_qoi(path, limits),
        ImageFormat::Icns => load_icns(path, limits),
        ImageFormat::Sgi => load_sgi(path, limits),
        ImageFormat::Xpm => load_xpm(path, limits),
        ImageFormat::Xbm => load_xbm(path, limits),
        ImageFormat::Exr => openexr::load(path, limits, None).map(|t| Asset::Image(Arc::new(t))),
    }));
    guard.unwrap_or_else(|_| Err(LoadError::Invalid("internal decoder error".into())))
}

/// The picture a file stores of itself, read without decoding the rest:
/// an OpenEXR file's header preview.  `None` for the other formats, which
/// are pictures themselves.  Slerp's core uses it for thumbnails; see
/// `slerp_core::saved_picture`.
pub fn saved_picture(format: ImageFormat, path: &Path) -> Result<Option<Texture>, LoadError> {
    match format {
        ImageFormat::Exr => openexr::saved_picture(path),
        _ => Ok(None),
    }
}

/// The layers and parts of an OpenEXR file, when it has several, for the
/// viewer's picker.
pub fn exr_parts(path: &Path) -> Vec<String> {
    openexr::parts(path)
}

/// Loads one layer or part of an OpenEXR file, by a name [`exr_parts`] gave.
pub fn load_exr_part(path: &Path, limits: &Limits, name: &str) -> Result<Asset, LoadError> {
    let guard = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| openexr::load(path, limits, Some(name))));
    let tex = guard.unwrap_or_else(|_| Err(LoadError::Invalid("internal decoder error".into())))?;
    Ok(Asset::Image(Arc::new(tex)))
}

fn invalid(e: impl std::fmt::Display) -> LoadError {
    LoadError::Invalid(e.to_string())
}

fn read_file(path: &Path, limits: &Limits) -> Result<Vec<u8>, LoadError> {
    let size = std::fs::metadata(path)?.len();
    if size > limits.max_file_bytes.min(MAX_FILE_BYTES) {
        return Err(LoadError::TooLarge(format!("{} MiB", size >> 20)));
    }
    Ok(std::fs::read(path)?)
}

/// A picture as a texture, with the facts the inspector lists under
/// "Header".
pub(crate) fn still(
    container: &str,
    format: &str,
    w: u32,
    h: u32,
    rgba: Vec<u8>,
    header: Vec<(String, String)>,
) -> Texture {
    let mut tex = Texture::from_rgba8(w, h, rgba, ColorSpace::Srgb);
    tex.source.container = container.into();
    tex.source.format = format.into();
    tex.source.header = header;
    tex
}

/// "2.4 s" or "1 min 5 s".
pub(crate) fn duration_text(seconds: f32) -> String {
    if seconds < 60.0 {
        format!("{seconds:.1} s")
    } else {
        format!("{} min {:.0} s", (seconds / 60.0) as u32, seconds % 60.0)
    }
}

/// "once", "3 times" or "for ever".
pub(crate) fn plays_text(plays: Option<u32>) -> String {
    match plays {
        None => "for ever".into(),
        Some(1) => "once".into(),
        Some(n) => format!("{n} times"),
    }
}

/// What the inspector lists about an animation.
fn animation_details(size: (u32, u32), delays: &[f32], plays: Option<u32>) -> Vec<(String, String)> {
    let mut details =
        vec![("Size".to_string(), format!("{} × {}", size.0, size.1)), ("Frames".into(), delays.len().to_string())];
    if delays.len() > 1 {
        details.push(("Duration".into(), duration_text(delays.iter().sum())));
        details.push(("Plays".into(), plays_text(plays)));
    }
    details
}

/// A single-frame animation is a still picture; a longer one a canvas.
fn animation_asset(
    container: &str,
    format: &str,
    info: Info,
    animation: animation::Animation,
    limits: &Limits,
    file_bytes: u64,
) -> Asset {
    let n = animation.delays.len();
    let (w, h) = animation.size;
    let canvas = AnimatedImage::new(animation, limits.frame_bytes as usize);
    if n > 1 {
        return Asset::Canvas(Arc::new(canvas));
    }
    let px = canvas.frame_pixels(0);
    let mut header = info.details.clone();
    header.retain(|(k, _)| k != "Size" && k != "File size");
    let mut tex = still(container, format, w, h, px.to_vec(), header);
    tex.source.file_bytes = file_bytes;
    Asset::Image(Arc::new(tex))
}

fn load_gif(path: &Path, limits: &Limits) -> Result<Asset, LoadError> {
    let bytes = read_file(path, limits)?;
    let file_bytes = bytes.len() as u64;
    let gif = Arc::new(gif::Gif::parse(Arc::new(bytes)).map_err(invalid)?);
    let local = gif.frames.iter().filter(|f| f.has_local_palette()).count();
    let interlaced = gif.frames.iter().filter(|f| f.interlaced).count();
    let mut details = animation_details((gif.width, gif.height), &gif.delays(), gif.plays());
    if gif.screen != (gif.width, gif.height) {
        details
            .push(("Logical screen".into(), format!("{} × {}, grown to fit the frames", gif.screen.0, gif.screen.1)));
    }
    details.push((
        "Colours".into(),
        format!("indexed, {} palette", if local > 0 { "local and global" } else { "global" }),
    ));
    if gif.has_transparency() {
        details.push(("Transparency".into(), "yes".into()));
    }
    if interlaced > 0 {
        details.push(("Interlaced frames".into(), interlaced.to_string()));
    }
    for c in &gif.comments {
        details.push(("Comment".into(), c.clone()));
    }
    details.push(("File size".into(), format!("{file_bytes} bytes")));
    let info =
        Info { format: format!("GIF {}", gif.version), warnings: gif.warnings.clone(), details, ..Default::default() };
    let animation = gif.into_animation(info.clone());
    Ok(animation_asset("GIF", "indexed colour", info, animation, limits, file_bytes))
}

fn load_apng(path: &Path, limits: &Limits) -> Result<Asset, LoadError> {
    let bytes = read_file(path, limits)?;
    let file_bytes = bytes.len() as u64;
    let apng = Arc::new(apng::Apng::parse(Arc::new(bytes)).map_err(invalid)?);
    if apng.frames.len() < 2 {
        // An animation of one frame is a picture; the texture crate reads it.
        return load_png_still(path, limits);
    }
    let mut details = animation_details((apng.width, apng.height), &apng.delays(), apng.plays());
    details.push(("Pixel format".into(), apng.pixel_format()));
    if !apng.default_is_frame {
        details.push(("Default image".into(), "stored apart from the animation (not shown)".into()));
    }
    if apng.has_transparency() {
        details.push(("Transparency".into(), "yes".into()));
    }
    details.push(("File size".into(), format!("{file_bytes} bytes")));
    let info = Info { format: "APNG".into(), warnings: apng.warnings.clone(), details, ..Default::default() };
    let animation = apng.clone().into_animation(info.clone());
    Ok(animation_asset("APNG", &apng.pixel_format(), info, animation, limits, file_bytes))
}

fn load_webp(path: &Path, limits: &Limits) -> Result<Asset, LoadError> {
    let bytes = read_file(path, limits)?;
    let file_bytes = bytes.len() as u64;
    let anim = Arc::new(webp::WebpAnim::parse(Arc::new(bytes)).map_err(invalid)?);
    if anim.frames.is_empty() {
        return Err(LoadError::Invalid("the animated WebP holds no frames".into()));
    }
    let mut details = animation_details((anim.width, anim.height), &anim.delays(), anim.plays());
    if anim.has_alpha() {
        details.push(("Transparency".into(), "yes".into()));
    }
    details.push(("File size".into(), format!("{file_bytes} bytes")));
    let info =
        Info { format: "WebP (animated)".into(), warnings: anim.warnings.clone(), details, ..Default::default() };
    let animation = anim.clone().into_animation(info.clone());
    Ok(animation_asset("WebP", "animated", info, animation, limits, file_bytes))
}

/// A PNG the texture crate decodes, with orientation and colour applied.
fn load_png_still(path: &Path, limits: &Limits) -> Result<Asset, LoadError> {
    let opts = match limits.max_texture_size {
        Some(n) => slerp_scene::textures::DecodeOptions::preview(n),
        None => slerp_scene::textures::DecodeOptions::full(),
    };
    let tex = slerp_scene::textures::decode_file(path, &opts)?;
    Ok(Asset::Image(Arc::new(photo::refine(path, slerp_scene::textures::Format::Png, tex))))
}

#[cfg(feature = "jxl")]
fn load_jxl(path: &Path, limits: &Limits) -> Result<Asset, LoadError> {
    jxl::load(path, limits)
}

#[cfg(not(feature = "jxl"))]
fn load_jxl(_path: &Path, _limits: &Limits) -> Result<Asset, LoadError> {
    Err(LoadError::Unsupported("JPEG XL support was left out of this build of Slerp (the `jxl` Cargo feature)".into()))
}

fn load_ico(path: &Path, limits: &Limits) -> Result<Asset, LoadError> {
    let bytes = read_file(path, limits)?;
    let file_bytes = bytes.len() as u64;
    let icon = ico::parse(&bytes).map_err(invalid)?;
    let kind = if icon.cursor { "Windows cursor" } else { "Windows icon" };
    let mut items = Vec::new();
    for e in &icon.entries {
        let mut name = format!("{} × {}, {}-bit{}", e.width, e.height, e.bits, if e.png { " PNG" } else { "" });
        if let Some((x, y)) = e.hotspot {
            name.push_str(&format!(" ({x},{y})"));
        }
        let mut header = vec![("Pictures".to_string(), icon.entries.len().to_string())];
        header.extend(icon.warnings.iter().map(|w| ("Warning".to_string(), w.clone())));
        let mut tex = still(kind, &format!("{}-bit", e.bits), e.width, e.height, e.rgba.clone(), header);
        tex.source.file_bytes = file_bytes;
        items.push(slerp_scene::NamedImage { name, texture: Arc::new(tex) });
    }
    if items.len() == 1 {
        return Ok(Asset::Image(items.remove(0).texture));
    }
    Ok(Asset::Gallery(items))
}

fn load_pnm(path: &Path, limits: &Limits) -> Result<Asset, LoadError> {
    let bytes = read_file(path, limits)?;
    let p = pnm::decode(&bytes).map_err(invalid)?;
    let mut header = vec![
        (
            "Variant".to_string(),
            format!("P{}, {}, {}", p.variant, p.name(), if p.is_ascii() { "text" } else { "binary" }),
        ),
        ("Largest sample value".into(), p.maxval.to_string()),
    ];
    for c in &p.comments {
        header.push(("Comment".into(), c.clone()));
    }
    if p.missing_rows > 0 {
        header.push(("Warning".into(), format!("The file is cut short: {} rows are missing", p.missing_rows)));
    }
    let mut tex = match p.pixels {
        pnm::Pixels::Rgba8(px) => still("Netpbm", &format!("P{}", p.variant), p.width, p.height, px, header),
        pnm::Pixels::Grey16(px) => {
            let mut t = Texture::from_rgba32f(p.width, p.height, px.iter().flat_map(|&v| [v, v, v, 1.0]).collect());
            t.source.container = "Netpbm".into();
            t.source.format = "Grayscale 16-bit (data)".into();
            t.source.header = header;
            t
        }
    };
    tex.source.file_bytes = bytes.len() as u64;
    Ok(Asset::Image(Arc::new(tex)))
}

fn load_tiff(path: &Path, limits: &Limits) -> Result<Asset, LoadError> {
    let min_long_side = limits.max_texture_size.unwrap_or(0) as usize;
    let size = std::fs::metadata(path)?.len();
    let mut pic = tiff::load(path, min_long_side).map_err(invalid)?;
    pic.texture.source.file_bytes = size;
    Ok(Asset::Image(Arc::new(pic.texture)))
}

fn load_jp2(path: &Path, limits: &Limits) -> Result<Asset, LoadError> {
    let bytes = read_file(path, limits)?;
    let j = jp2::decode(&bytes).map_err(invalid)?;
    let mut header = vec![
        ("Colour space".to_string(), j.space.clone()),
        ("Components".into(), j.components.to_string()),
        ("Bits per component".into(), j.bit_depth.to_string()),
    ];
    if j.has_alpha {
        header.push(("Transparency".into(), "yes".into()));
    }
    if let Some(n) = &j.profile_note {
        header.push(("Colour profile".into(), n.clone()));
    }
    let mut tex = match j.pixels {
        jp2::Pixels::Rgba8(px) => still("JPEG 2000", &format!("{}-bit", j.bit_depth), j.width, j.height, px, header),
        jp2::Pixels::Grey16(px) => {
            let mut t = Texture::from_rgba32f(j.width, j.height, px);
            t.source.container = "JPEG 2000".into();
            t.source.format = format!("Grayscale {}-bit (data)", j.bit_depth);
            t.source.header = header;
            t
        }
    };
    tex.source.file_bytes = bytes.len() as u64;
    Ok(Asset::Image(Arc::new(tex)))
}

/// The most pixels the simple formats below may decode to.
const MAX_PIXELS: u64 = 1 << 28;

fn load_qoi(path: &Path, limits: &Limits) -> Result<Asset, LoadError> {
    let bytes = read_file(path, limits)?;
    let q = qoi::decode(&bytes, MAX_PIXELS).map_err(invalid)?;
    let mut header = vec![
        ("Channels".to_string(), if q.channels == 4 { "RGBA".to_string() } else { "RGB".into() }),
        ("Colour space".into(), if q.linear { "linear".into() } else { "sRGB with linear alpha".to_string() }),
    ];
    if q.missing > 0 {
        header.push(("Warning".into(), format!("the file is cut short: {} pixels are missing", q.missing)));
    }
    let space = if q.linear { ColorSpace::Linear } else { ColorSpace::Srgb };
    let mut tex = Texture::from_rgba8(q.width, q.height, q.rgba, space);
    tex.source.container = "QOI".into();
    tex.source.format = format!("{}-bit", q.channels as u32 * 8);
    tex.source.header = header;
    tex.source.file_bytes = bytes.len() as u64;
    Ok(Asset::Image(Arc::new(tex)))
}

fn load_icns(path: &Path, limits: &Limits) -> Result<Asset, LoadError> {
    let bytes = read_file(path, limits)?;
    let file_bytes = bytes.len() as u64;
    let icns = icns::decode(&bytes).map_err(invalid)?;
    let count = icns.pictures.len();
    let mut items = Vec::new();
    for p in icns.pictures {
        let name = format!("{} × {} ({})", p.width, p.height, p.label);
        let mut header = vec![("Pictures".to_string(), count.to_string()), ("Element".into(), p.label.clone())];
        header.extend(icns.warnings.iter().map(|w| ("Warning".to_string(), w.clone())));
        let mut tex = still("Apple icon", &p.label, p.width, p.height, p.rgba, header);
        tex.source.file_bytes = file_bytes;
        items.push(slerp_scene::NamedImage { name, texture: Arc::new(tex) });
    }
    if items.len() == 1 {
        return Ok(Asset::Image(items.remove(0).texture));
    }
    Ok(Asset::Gallery(items))
}

fn load_sgi(path: &Path, limits: &Limits) -> Result<Asset, LoadError> {
    let bytes = read_file(path, limits)?;
    let s = sgi::decode(&bytes, MAX_PIXELS).map_err(invalid)?;
    let layout = match s.channels {
        1 => "grey",
        2 => "grey and alpha",
        3 => "RGB",
        _ => "RGBA",
    };
    let mut header = vec![
        ("Channels".to_string(), format!("{} ({layout})", s.channels)),
        ("Bits per sample".into(), (s.bytes_per_sample as u32 * 8).to_string()),
        ("Storage".into(), if s.rle { "run-length coded".into() } else { "verbatim".to_string() }),
    ];
    if !s.name.trim().is_empty() {
        header.push(("Name".into(), s.name.clone()));
    }
    header.extend(s.warnings.iter().map(|w| ("Warning".to_string(), w.clone())));
    let format = format!("{}-bit {layout}", s.bytes_per_sample as u32 * 8);
    let mut tex = still("SGI", &format, s.width, s.height, s.rgba, header);
    tex.source.file_bytes = bytes.len() as u64;
    Ok(Asset::Image(Arc::new(tex)))
}

fn load_xpm(path: &Path, limits: &Limits) -> Result<Asset, LoadError> {
    let bytes = read_file(path, limits)?;
    let x = xpm::decode(&bytes, MAX_PIXELS).map_err(invalid)?;
    let mut header = vec![
        ("Colours".to_string(), x.colors.to_string()),
        ("Characters per pixel".into(), x.chars_per_pixel.to_string()),
    ];
    if let Some((hx, hy)) = x.hotspot {
        header.push(("Hot spot".into(), format!("{hx}, {hy}")));
    }
    header.extend(x.warnings.iter().map(|w| ("Warning".to_string(), w.clone())));
    let mut tex = still("XPM", "indexed colour", x.width, x.height, x.rgba, header);
    tex.source.file_bytes = bytes.len() as u64;
    Ok(Asset::Image(Arc::new(tex)))
}

fn load_xbm(path: &Path, limits: &Limits) -> Result<Asset, LoadError> {
    let bytes = read_file(path, limits)?;
    let x = xbm::decode(&bytes, MAX_PIXELS).map_err(invalid)?;
    let mut header = vec![("Style".to_string(), if x.x10 { "X10 (16-bit words)" } else { "X11 (bytes)" }.to_string())];
    if let Some((hx, hy)) = x.hotspot {
        header.push(("Hot spot".into(), format!("{hx}, {hy}")));
    }
    header.extend(x.warnings.iter().map(|w| ("Warning".to_string(), w.clone())));
    let mut tex = still("XBM", "1-bit", x.width, x.height, x.rgba, header);
    tex.source.file_bytes = bytes.len() as u64;
    Ok(Asset::Image(Arc::new(tex)))
}

/// AVIF and HEIC.  Both are ISO base media files; the codec of the primary
/// picture decides who decodes it: AV1 (every AVIF, and HEIF files made of
/// AV1) is ours, anything else (HEVC) goes to the system's libheif.
fn load_heif(path: &Path, limits: &Limits) -> Result<Asset, LoadError> {
    let bytes = read_file(path, limits)?;
    let file_bytes = bytes.len() as u64;
    let h = isobmff::Heif::parse(&bytes).map_err(invalid)?;
    let sequence = h.brands.has(b"avis") && h.picture_track().is_some();
    let av1 = sequence || avif::codec_of(&h) == Some(avif::Codec::Av1);
    if !av1 {
        return load_hevc(&h, &bytes, file_bytes);
    }
    let mut details = vec![("Brands".to_string(), brand_text(&h))];
    details.push(("File size".into(), format!("{file_bytes} bytes")));
    let info = Info { format: "AVIF".into(), warnings: h.warnings.clone(), details, ..Default::default() };
    if sequence {
        let moved = bytes.clone();
        if let Some(anim) = avif::load_sequence(&h, moved, info.clone()) {
            let mut anim = anim.map_err(invalid)?;
            let mut details = animation_details(anim.size, &anim.delays, anim.plays);
            details.extend(info.details.iter().cloned());
            anim.info.details = details;
            let animation = anim;
            return Ok(animation_asset(
                "AVIF",
                "AV1 sequence",
                Info { details: animation.info.details.clone(), ..info },
                animation,
                limits,
                file_bytes,
            ));
        }
    }
    let mut still = avif::load_still(&h, &bytes).map_err(invalid)?;
    still.texture.source.file_bytes = file_bytes;
    still.texture.source.header.insert(0, ("Brands".into(), brand_text(&h)));
    Ok(Asset::Image(Arc::new(still.texture)))
}

fn brand_text(h: &isobmff::Heif) -> String {
    let b = &h.brands;
    let mut names = vec![String::from_utf8_lossy(&b.major).trim().to_string()];
    names.extend(b.compatible.iter().map(|c| String::from_utf8_lossy(c).trim().to_string()));
    names.dedup();
    names.join(", ")
}

#[cfg(feature = "heic")]
fn load_hevc(h: &isobmff::Heif, bytes: &[u8], file_bytes: u64) -> Result<Asset, LoadError> {
    use exif::Meta;
    use isobmff::{Colour, Property};
    let id = h.primary.ok_or_else(|| LoadError::Invalid("the file has no primary picture".into()))?;
    let props = h.properties_of(id);
    let (mut icc, mut cicp) = (None, None);
    for p in &props {
        match p {
            Property::Colr(Colour::Icc(profile)) => icc = Some(profile.clone()),
            Property::Colr(Colour::Nclx { primaries, transfer, matrix, full_range }) => {
                cicp = Some([*primaries as u8, *transfer as u8, *matrix as u8, *full_range as u8]);
            }
            _ => {}
        }
    }
    let hdr = cicp.is_some_and(|c| matches!(c[1], 16 | 18));
    let max_pixels = avif::MAX_PIXELS;
    let d = heif::decode(bytes, hdr, max_pixels)
        .map_err(|e| if e.contains("libheif library") { LoadError::Unsupported(e) } else { LoadError::Invalid(e) })?;
    let mut header =
        vec![("Brands".to_string(), brand_text(h)), ("Bit depth".into(), format!("{} bits per channel", d.bit_depth))];
    if let Some(kind) = h.item(id).map(|i| i.kind) {
        let codec = match &kind {
            b"hvc1" | b"hev1" => "HEVC (H.265)",
            b"vvc1" => "VVC (H.266)",
            b"avc1" => "H.264",
            b"jpeg" => "JPEG",
            b"grid" => "grid of tiles",
            b"unci" => "uncompressed",
            _ => "another codec",
        };
        header.push(("Codec".into(), codec.into()));
    }
    header.push(("Decoded by".into(), "the system's libheif".into()));
    let mut tex = if d.wide {
        // 16-bit samples of a PQ or HLG picture: linear light for the HDR path.
        let samples: Vec<u16> = d.pixels.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        let conversion = cicp
            .and_then(color::Conversion::from_cicp)
            .ok_or_else(|| LoadError::Invalid("unknown colour codes".into()))?;
        header.push(("Colour".into(), format!("{}, shown through the HDR path", conversion.name)));
        let lin = conversion.to_linear_rgba(&samples, d.bit_depth.clamp(8, 16) as u32);
        let mut t = Texture::from_rgba32f(d.width, d.height, lin);
        t.source.format = format!("HEVC {}-bit HDR", d.bit_depth);
        t
    } else {
        let mut t = Texture::from_rgba8(d.width, d.height, d.pixels, ColorSpace::Srgb);
        t.source.format = format!("HEVC {}-bit{}", d.bit_depth, if d.has_alpha { " with alpha" } else { "" });
        t.source.header = header.clone();
        photo::apply(&mut t, &Meta { orientation: 1, icc, cicp });
        header = std::mem::take(&mut t.source.header);
        t
    };
    tex.source.container = "HEIF".into();
    tex.source.header = header;
    tex.source.file_bytes = file_bytes;
    Ok(Asset::Image(Arc::new(tex)))
}

#[cfg(not(feature = "heic"))]
fn load_hevc(_h: &isobmff::Heif, _bytes: &[u8], _file_bytes: u64) -> Result<Asset, LoadError> {
    Err(LoadError::Unsupported("HEIC support was left out of this build of Slerp".into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jpeg_xl_signatures() {
        let jxl = Path::new("a.jxl");
        assert!(jxl_is(jxl, &[0xff, 0x0a, 1, 2]));
        assert!(!jxl_is(Path::new("a.bin"), &[0xff, 0x0a, 1, 2]));
        assert!(jxl_is(Path::new("a.bin"), b"\0\0\0\x0cJXL \r\n\x87\n"));
        assert!(!jxl_is(jxl, b"\xff\xd8\xff\xe0"));
        assert!(!jxl_is(jxl, b""));
    }
}
