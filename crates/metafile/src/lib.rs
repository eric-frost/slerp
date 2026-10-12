//! Vector metafiles: Windows Metafile (`.wmf`), Enhanced Metafile (`.emf`),
//! their gzip-compressed forms (`.wmz`, `.emz`) and Macintosh PICT.  They
//! are recordings of drawing calls, so showing one means replaying the calls
//! onto a canvas, here on the CPU with tiny-skia: lines, curves, filled
//! shapes with patterns, bitmaps with raster operations, clipping, world
//! transforms, paths and text.
//!
//! Records that can't be played are skipped and counted, never fatal: the
//! picture so far is better than none.  Everything read from the file is
//! checked (sizes, indices, nesting), and playback has a time budget.
//!
//! The result is an ordinary picture at the file's aspect ratio on a
//! transparent page; thumbnails put it on white.

mod affine;
mod bytes;
mod canvas;
mod dib;
mod emf;
mod emfplus;
mod gdi;
mod hershey;
mod make;
mod pict;
mod shapes;
mod state;
mod sysfont;
mod text;
mod wmf;

#[cfg(test)]
mod tests;

use slerp_scene::textures::{ColorSpace, Texture};
use slerp_scene::{Asset, Limits, LoadError};
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Set `SLERP_METAFILE_TRACE=1` to print each record played to stderr.
pub(crate) fn trace() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("SLERP_METAFILE_TRACE").is_some())
}

/// A colour as red, green and blue bytes.
pub type Rgb = [u8; 3];

/// The metafile formats this crate replays.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MetafileFormat {
    /// Windows Metafile, with or without the placeable header.
    Wmf,
    /// Enhanced Metafile.
    Emf,
    /// Macintosh PICT, version 1 or 2.
    Pict,
}

impl MetafileFormat {
    pub fn name(self) -> &'static str {
        match self {
            MetafileFormat::Wmf => "Windows Metafile",
            MetafileFormat::Emf => "Enhanced Metafile",
            MetafileFormat::Pict => "Macintosh PICT",
        }
    }
}

/// The largest size of a rendered picture's longer side.
pub const MAX_SIDE: u32 = 4096;
/// The most pixels a rendered picture may have.
const MAX_PIXELS: u64 = 16 << 20;
/// The most a compressed file may expand to.
const MAX_INFLATED: usize = 192 << 20;
/// Time allowed to replay a file for a thumbnail, and for the viewer.
const THUMBNAIL_BUDGET: Duration = Duration::from_millis(900);
const VIEWER_BUDGET: Duration = Duration::from_secs(10);

/// Recognises a metafile from the start of the file and its name.  WMF and
/// EMF have strong signatures; PICT version 1 has none, so it needs its
/// extension; the compressed forms are told by theirs.
pub fn sniff(path: &Path, head: &[u8]) -> Option<MetafileFormat> {
    let ext = path.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase).unwrap_or_default();
    if emf::looks_like_emf(head) {
        return Some(MetafileFormat::Emf);
    }
    if wmf::looks_like_wmf(head) {
        return Some(MetafileFormat::Wmf);
    }
    if head.starts_with(&[0x1F, 0x8B]) {
        return match ext.as_str() {
            "emz" => Some(MetafileFormat::Emf),
            "wmz" => Some(MetafileFormat::Wmf),
            _ => None,
        };
    }
    let named_pict = matches!(ext.as_str(), "pct" | "pict" | "pic" | "pict1" | "pict2");
    if pict::looks_like_pict(head, named_pict) {
        return Some(MetafileFormat::Pict);
    }
    None
}

/// Does this look like a WMF without the placeable header?  (`sniff` says
/// yes for those too; this exists for tests of the magic itself.)
pub fn is_bare_wmf(head: &[u8]) -> bool {
    wmf::looks_like_wmf(head)
}

/// Reads and replays the file at `path`.
pub fn load_path(format: MetafileFormat, path: &Path, limits: &Limits) -> Result<Asset, LoadError> {
    let size = std::fs::metadata(path)?.len();
    if size > limits.max_file_bytes.min(512 << 20) {
        return Err(LoadError::TooLarge(format!("{size} bytes")));
    }
    let bytes = std::fs::read(path)?;
    Ok(Asset::Image(Arc::new(load(format, &bytes, limits)?)))
}

/// Replays a metafile held in memory.
pub fn load(format: MetafileFormat, bytes: &[u8], limits: &Limits) -> Result<Texture, LoadError> {
    render(format, bytes, limits)
}

thread_local! {
    /// How many metafiles are being replayed inside one another (an EMF+
    /// image that is a metafile), and when the outermost one must stop.
    static NESTING: std::cell::Cell<(u32, Option<Instant>)> = const { std::cell::Cell::new((0, None)) };
}

/// Replays a metafile embedded in another, at most a few levels deep and
/// within the outer replay's time.
pub(crate) fn render_nested(bytes: &[u8], side: u32, _depth: u32) -> Result<Texture, LoadError> {
    let limits = Limits { max_texture_size: Some(side), ..Limits::thumbnail() };
    load_sniffed(bytes, &limits)
}

/// Replays any metafile found by its content: used for pictures stored
/// inside other files (Visio's thumbnail), where the format is not known
/// from a name.
pub fn load_sniffed(bytes: &[u8], limits: &Limits) -> Result<Texture, LoadError> {
    let format = sniff(Path::new(""), bytes).ok_or_else(|| LoadError::Unsupported("not a metafile".into()))?;
    render(format, bytes, limits)
}

fn render(format: MetafileFormat, bytes: &[u8], limits: &Limits) -> Result<Texture, LoadError> {
    let inflated;
    let data: &[u8] = if bytes.starts_with(&[0x1F, 0x8B]) {
        inflated = gunzip(bytes)?;
        &inflated
    } else {
        bytes
    };
    let thumbnail = limits.max_texture_size.is_some();
    let started = Instant::now();
    let budget = if thumbnail { THUMBNAIL_BUDGET } else { VIEWER_BUDGET };
    // Nested replays share the outermost one's deadline and stop at depth 3.
    let (depth, outer) = NESTING.with(|n| n.get());
    if depth >= 3 {
        return Err(LoadError::invalid("metafiles nested too deeply"));
    }
    let deadline = outer.map_or(started + budget, |d| d.min(started + budget));
    NESTING.with(|n| n.set((depth + 1, Some(deadline))));
    struct Leave(u32, Option<Instant>);
    impl Drop for Leave {
        fn drop(&mut self) {
            NESTING.with(|n| n.set((self.0, self.1)));
        }
    }
    let _leave = Leave(depth, outer);
    let side = limits.max_texture_size;
    let drawn = match format {
        MetafileFormat::Wmf => {
            let w = wmf::Wmf::parse(data).ok_or_else(|| LoadError::invalid("not a Windows Metafile"))?;
            replay(side, deadline, (wmf::PX_PER_MM, wmf::PX_PER_MM), w.plan().map(|p| (p, w.native_long())), |g| {
                w.play(g)
            })?
        }
        MetafileFormat::Emf => {
            let e = emf::Emf::parse(data).ok_or_else(|| LoadError::invalid("not an Enhanced Metafile"))?;
            replay(side, deadline, e.px_per_mm(), e.plan().map(|p| (p, None)), |g| e.play(g))?
        }
        MetafileFormat::Pict => pict::render(data, side, deadline)?,
    };
    let mut tex = Texture::from_rgba8(drawn.width, drawn.height, drawn.rgba, ColorSpace::Srgb);
    tex.source.container = format.name().to_string();
    tex.source.format = "vector drawing (RGBA)".to_string();
    tex.source.file_bytes = bytes.len() as u64;
    let mut header: Vec<(String, String)> = Vec::new();
    if drawn.timed_out {
        header.push(("Stopped early".into(), format!("after {:.1} s", budget.as_secs_f32())));
    }
    for (what, n) in &drawn.skipped {
        header.push(("Skipped".into(), format!("{what} x{n}")));
    }
    tex.source.header = header;
    Ok(tex)
}

/// What a replay produced.
pub(crate) struct Drawn {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
    pub timed_out: bool,
    pub skipped: Vec<(String, u32)>,
}

/// The size to draw at: thumbnails fill the size asked for, the viewer
/// draws at twice the file's natural size within sensible bounds.
pub(crate) fn canvas_size(aspect: f64, natural_long: f64, side: Option<u32>) -> Option<(u32, u32)> {
    if !(aspect.is_finite() && aspect > 0.0) {
        return None;
    }
    let long = match side {
        Some(n) => n.clamp(16, MAX_SIDE) as f64,
        None => (natural_long * 2.0).clamp(384.0, 3072.0),
    };
    let (w, h) = if aspect >= 1.0 { (long, long / aspect) } else { (long * aspect, long) };
    let (w, h) = (w.round().max(1.0), h.round().max(1.0));
    // An extreme aspect ratio would make a sliver; keep it drawable.
    let (w, h) = (w.min(MAX_SIDE as f64) as u32, h.min(MAX_SIDE as f64) as u32);
    if w as u64 * h as u64 > MAX_PIXELS { None } else { Some((w, h)) }
}

/// Plays a metafile onto a canvas, measuring first when the header gives no
/// usable frame.
fn replay(
    side: Option<u32>,
    deadline: Instant,
    px_per_mm: (f64, f64),
    planned: Option<(wmf::Plan, Option<f64>)>,
    play: impl Fn(&mut gdi::Gdi),
) -> Result<Drawn, LoadError> {
    use affine::Affine;
    let (plan, natural) = match planned {
        Some(p) => p,
        None => {
            // No frame in the header: play once without drawing, to see how
            // far the geometry reaches.
            let mut g = gdi::Gdi::new(canvas::Canvas::measuring(), Affine::IDENTITY, px_per_mm, deadline);
            play(&mut g);
            let b = g.cv.bounds.ok_or_else(|| LoadError::Unsupported("the metafile draws nothing".into()))?;
            let (w, h) = ((b[2] - b[0]) as f64, (b[3] - b[1]) as f64);
            if !(w > 0.0 && h > 0.0) {
                return Err(LoadError::Unsupported("the metafile draws nothing visible".into()));
            }
            let pad = 1.0;
            let frame = [b[0] as f64 - pad, b[1] as f64 - pad, b[2] as f64 + pad, b[3] as f64 + pad];
            (wmf::Plan { dev_frame: frame, aspect: (w + 2.0 * pad) / (h + 2.0 * pad) }, None)
        }
    };
    let [l, t, r, b] = plan.dev_frame;
    let (fw, fh) = (r - l, b - t);
    let natural_long = natural.unwrap_or(fw.abs().max(fh.abs()));
    let (cw, ch) = canvas_size(plan.aspect, natural_long, side)
        .ok_or_else(|| LoadError::invalid("the metafile's size is not usable"))?;
    let base = Affine::translate(-l, -t).then(Affine::scale(cw as f64 / fw, ch as f64 / fh));
    if !base.is_finite() {
        return Err(LoadError::invalid("the metafile's frame is not usable"));
    }
    let canvas = canvas::Canvas::new(cw, ch).ok_or_else(|| LoadError::TooLarge(format!("{cw}x{ch}")))?;
    let mut g = gdi::Gdi::new(canvas, base, px_per_mm, deadline);
    play(&mut g);
    let timed_out = g.timed_out;
    let skipped = g.warnings.skipped.iter().map(|(k, v)| (k.clone(), *v)).collect();
    Ok(Drawn { width: cw, height: ch, rgba: g.cv.into_rgba(None), timed_out, skipped })
}

/// Inflates a gzip file (`.wmz`, `.emz`), refusing to grow past a limit.
pub fn gunzip(bytes: &[u8]) -> Result<Vec<u8>, LoadError> {
    gunzip_limited(bytes, MAX_INFLATED)
}

fn gunzip_limited(bytes: &[u8], limit: usize) -> Result<Vec<u8>, LoadError> {
    let bad = || LoadError::invalid("damaged gzip data");
    if bytes.len() < 18 || bytes[0] != 0x1F || bytes[1] != 0x8B || bytes[2] != 8 {
        return Err(bad());
    }
    let flags = bytes[3];
    let mut at = 10usize;
    if flags & 4 != 0 {
        let n = u16::from_le_bytes([*bytes.get(at).ok_or_else(bad)?, *bytes.get(at + 1).ok_or_else(bad)?]) as usize;
        at += 2 + n;
    }
    for bit in [8u8, 16] {
        if flags & bit != 0 {
            let end = bytes.get(at..).ok_or_else(bad)?.iter().position(|b| *b == 0).ok_or_else(bad)?;
            at += end + 1;
        }
    }
    if flags & 2 != 0 {
        at += 2;
    }
    let body = bytes.get(at..bytes.len() - 8).ok_or_else(bad)?;
    miniz_oxide::inflate::decompress_to_vec_with_limit(body, limit).map_err(|e| match e.status {
        miniz_oxide::inflate::TINFLStatus::HasMoreOutput => {
            LoadError::TooLarge("the compressed metafile inflates past the limit".into())
        }
        _ => bad(),
    })
}
