//! PDF documents through `hayro`, always in a child process (see the crate
//! documentation): one-off pictures for thumbnails, and [`PdfPages`], a
//! document kept open in a worker so the viewer can draw any part of any
//! page at any scale.
//!
//! The worker speaks a small binary protocol over its pipe: a request is a
//! [`DrawView`], answered with the picture's size and its straight-alpha
//! RGBA bytes; `text` and a page number, answered with the page's glyphs
//! (see [`PageText`]); or `warnings`.  The opening answer lists the page
//! sizes and the document's facts.

use crate::page_text::{Builder, Direction, PageText};
use crate::{DocumentFormat, MAX_PAGES, MAX_PIXELS, MAX_SIDE, text};
use hayro::hayro_interpret::font::{Glyph, GlyphRun};
use hayro::hayro_interpret::hayro_cmap::BfString;
use hayro::hayro_interpret::util::TransformExt;
use hayro::hayro_interpret::{
    BlendMode, ClipPath, Context, Device, DrawMode, DrawProps, Image, ImageDrawProps, InterpreterCache,
    InterpreterSettings, InterpreterWarning, SoftMask,
};
use hayro::hayro_syntax::object::stream::ImageDecodeParams;
use hayro::hayro_syntax::object::{Name, Stream};
use hayro::hayro_syntax::{DecryptionError, LoadPdfError, Pdf, PdfData};
use hayro::kurbo::{Affine, BezPath, Point, Rect};
use hayro::vello_cpu::color::palette::css::{TRANSPARENT, WHITE};
use hayro::vello_cpu::{Pixmap, RasterizerSettings, RenderContext, Resources, TargetInit};
use slerp_isolate::{Limits, Worker};
use slerp_scene::textures::Texture;
use slerp_scene::{Image8, LoadError};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/*┌─────────────────╮
| Reading         |
└─────────────────╯*/
//region Reading

/// The file's bytes.  In the child on Unix they are mapped rather than
/// read, so a thumbnail of a 500 MB scan touches only the cross-reference
/// table and the first page's objects; a file cut short while mapped
/// faults the child alone.  Windows reads the file, since there the
/// renderer shares the caller's process.
fn file_data(path: &Path) -> Result<PdfData, String> {
    let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        // SAFETY: the map is read-only and lives in a child process whose
        // only job is this file; if another program changes or shortens
        // the file meanwhile, the worst outcome is that child faulting,
        // which its parent reports as an error.
        if let Ok(map) = unsafe { memmap2::Mmap::map(&file) } {
            return Ok(PdfData::from(Arc::new(map)));
        }
    }
    let mut bytes = Vec::new();
    std::io::Read::read_to_end(&mut &file, &mut bytes).map_err(|e| e.to_string())?;
    Ok(PdfData::from(bytes))
}

/// Opens a PDF, saying in words why it can't be.
fn open(path: &Path) -> Result<Pdf, String> {
    let data = file_data(path)?;
    Pdf::new(data).map_err(|e| match e {
        LoadPdfError::Decryption(DecryptionError::PasswordProtected) => {
            "this PDF is locked with a password; Slerp doesn't ask for passwords".to_string()
        }
        LoadPdfError::Decryption(DecryptionError::UnsupportedAlgorithm) => {
            "this PDF is encrypted in a way Slerp can't read".to_string()
        }
        LoadPdfError::Decryption(_) => "this PDF's encryption is damaged".to_string(),
        LoadPdfError::Invalid => "this isn't a readable PDF".to_string(),
    })
}

/// Warnings the interpreter reported while drawing, collected in the
/// child: hayro reports them through a callback.
type WarningLog = Arc<Mutex<Vec<InterpreterWarning>>>;

fn settings(log: &WarningLog) -> InterpreterSettings {
    let log = log.clone();
    InterpreterSettings {
        warning_sink: Arc::new(move |w| {
            if let Ok(mut l) = log.lock()
                && l.len() < 64
            {
                l.push(w);
            }
        }),
        ..Default::default()
    }
}

fn warning_text(w: &InterpreterWarning) -> &'static str {
    match w {
        InterpreterWarning::UnsupportedFont => "Some text uses a kind of font Slerp can't draw",
        InterpreterWarning::ImageDecodeFailure => "Some pictures in the document couldn't be decoded",
        InterpreterWarning::UnresolvedAnnotationAppearance => "Some annotations couldn't be drawn",
        _ => "Some of the page couldn't be drawn",
    }
}

//endregion

/*┌─────────────────╮
| Drawing         |
└─────────────────╯*/
//region Drawing

/// What to draw: a part of a page at a scale.  `left` and `top` are the
/// page point (1/72 inch, from the page's top left as shown, rotation
/// applied) at the picture's top-left corner; `scale` is pixels per point.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DrawView {
    pub page: usize,
    pub left: f64,
    pub top: f64,
    pub scale: f64,
    pub width: u32,
    pub height: u32,
}

impl DrawView {
    /// The whole page `page` of size `size` (points), fitted into `fit`
    /// pixels on its longer side.
    pub fn fit(page: usize, size: (f32, f32), fit: u32) -> DrawView {
        let (w, h) = (size.0.max(1.0) as f64, size.1.max(1.0) as f64);
        let scale = fit.clamp(1, MAX_SIDE) as f64 / w.max(h);
        let width = ((w * scale).round() as u32).clamp(1, MAX_SIDE);
        let height = ((h * scale).round() as u32).clamp(1, MAX_SIDE);
        DrawView { page, left: 0.0, top: 0.0, scale, width, height }
    }

    fn check(&self) -> Result<(), String> {
        let sized = (1..=MAX_SIDE).contains(&self.width)
            && (1..=MAX_SIDE).contains(&self.height)
            && self.width as u64 * self.height as u64 <= MAX_PIXELS;
        let finite = self.left.is_finite() && self.top.is_finite() && self.scale.is_finite();
        if !sized || !finite || !(1e-4..=1e4).contains(&self.scale) {
            return Err("that view of the page can't be drawn".into());
        }
        Ok(())
    }

    fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(40);
        out.extend((self.page as u64).to_le_bytes());
        out.extend(self.left.to_le_bytes());
        out.extend(self.top.to_le_bytes());
        out.extend(self.scale.to_le_bytes());
        out.extend(self.width.to_le_bytes());
        out.extend(self.height.to_le_bytes());
        out
    }

    fn decode(b: &[u8]) -> Option<DrawView> {
        let f = |o: usize| Some(f64::from_le_bytes(b.get(o..o + 8)?.try_into().ok()?));
        let u = |o: usize| Some(u32::from_le_bytes(b.get(o..o + 4)?.try_into().ok()?));
        let page = u64::from_le_bytes(b.get(0..8)?.try_into().ok()?);
        Some(DrawView {
            page: usize::try_from(page).ok()?,
            left: f(8)?,
            top: f(16)?,
            scale: f(24)?,
            width: u(32)?,
            height: u(36)?,
        })
    }
}

/// Draws a view of a page as straight-alpha RGBA: the page white, anything
/// past its edges transparent.  The answer is the width, the height and
/// the pixels.
fn draw(pdf: &Pdf, view: &DrawView, log: &WarningLog) -> Result<Vec<u8>, String> {
    view.check()?;
    let pages = pdf.pages();
    let page = pages.get(view.page).ok_or_else(|| format!("the document has no page {}", view.page + 1))?;
    let (w, h) = (view.width as u16, view.height as u16);
    let mut ctx = RenderContext::new(w, h);
    let place = Affine::translate((-view.left * view.scale, -view.top * view.scale)) * Affine::scale(view.scale);
    // 1. The paper.
    let (pw, ph) = page.render_dimensions();
    ctx.set_transform(place);
    ctx.set_paint(WHITE);
    ctx.fill_rect(&Rect::new(0.0, 0.0, pw as f64, ph as f64));
    // 2. The page's content over it.
    let transform = place * page.initial_transform(true).to_kurbo();
    let cache = hayro::RenderCache::new();
    hayro::render_into(page, &cache, &settings(log), &hayro::RenderSettings::default(), &mut ctx, transform);
    ctx.flush();
    let mut pixmap = Pixmap::new(w, h);
    let raster = RasterizerSettings { target_init: TargetInit::Clear(TRANSPARENT), ..Default::default() };
    ctx.render_with(&mut pixmap, &mut Resources::default(), raster);
    // 3. Straight alpha, as textures hold it.
    let mut rgba = pixmap.data_as_u8_slice().to_vec();
    for p in rgba.chunks_exact_mut(4) {
        let a = p[3] as u32;
        if a > 0 && a < 255 {
            for c in &mut p[..3] {
                *c = ((*c as u32 * 255 + a / 2) / a).min(255) as u8;
            }
        }
    }
    let mut out = Vec::with_capacity(8 + rgba.len());
    out.extend(view.width.to_le_bytes());
    out.extend(view.height.to_le_bytes());
    out.extend(rgba);
    Ok(out)
}

/// The picture in an answer from [`draw`].
fn decode_image(answer: Vec<u8>) -> Result<Image8, String> {
    let bad = || "the renderer's answer was damaged".to_string();
    let w = u32::from_le_bytes(answer.get(0..4).ok_or_else(bad)?.try_into().map_err(|_| bad())?);
    let h = u32::from_le_bytes(answer.get(4..8).ok_or_else(bad)?.try_into().map_err(|_| bad())?);
    if answer.len() as u64 != 8 + w as u64 * h as u64 * 4 {
        return Err(bad());
    }
    let mut pixels = answer;
    pixels.drain(..8);
    Ok(Image8 { width: w, height: h, pixels })
}

/// The most bytes a drawn picture's answer has.
fn answer_bytes() -> usize {
    (MAX_PIXELS as usize) * 4 + 64
}

//endregion

/*┌─────────────────╮
| Text            |
└─────────────────╯*/
//region Text

/// The request for a page's text: these bytes, then the page number.
const TEXT_REQUEST: &[u8] = b"text";
/// How far a glyph's box reaches below and above its baseline, in
/// thousandths of the font size: the em box of most Latin fonts.  The
/// interpreter doesn't pass on the font's own ascent and descent, and a
/// box of the usual height highlights as readers do.
const DESCENT: f64 = -200.0;
const ASCENT: f64 = 800.0;
/// The advance of a glyph whose font gives none and that ends its run (a
/// Type 3 glyph), in thousandths of the font size.
const DEFAULT_ADVANCE: f64 = 500.0;

/// A device that draws nothing and notes where each glyph goes and what it
/// says.  The interpreter hands it every run of text: filled, stroked,
/// and invisible (the text layer OCR puts over a scanned page), in the
/// order the page draws them.
struct TextDevice {
    /// The page as shown, in points; glyphs wholly outside it are left out.
    page: Rect,
    glyphs: Builder,
    /// Text both filled and stroked comes as the same run twice in a row;
    /// the second is skipped.  The run's length, its first glyph's place
    /// and the drawing transform tell it.
    last_run: Option<(usize, [f64; 6], [f64; 6])>,
    full: bool,
}

impl<'a> Device<'a> for TextDevice {
    fn draw_path(&mut self, _: &BezPath, _: DrawProps<'a>, _: &DrawMode) {}
    fn push_clip_path(&mut self, _: &ClipPath) {}
    fn push_transparency_group(&mut self, _: f32, _: Option<SoftMask<'a>>, _: BlendMode) {}
    fn draw_image(&mut self, _: Image<'a, '_>, _: ImageDrawProps<'a>) {}
    fn pop_clip(&mut self) {}
    fn pop_transparency_group(&mut self) {}

    fn draw_glyph_run(&mut self, run: &GlyphRun<'_, 'a>, props: DrawProps<'a>, _: &DrawMode) {
        let glyphs = run.glyphs();
        let key =
            (glyphs.len(), glyphs.first().map_or([0.0; 6], |g| g.transform().as_coeffs()), props.transform.as_coeffs());
        if self.full || self.last_run == Some(key) {
            return;
        }
        self.last_run = Some(key);
        for (i, g) in glyphs.iter().enumerate() {
            let Some(text) = g.as_unicode() else { continue };
            // 1. The glyph's advance in its own space (thousandths of an
            //    em): the font's width, else the way to the next glyph.
            let place = props.transform * g.transform();
            let width = match &**g {
                Glyph::Outline(o) => o.advance_width().map(f64::from),
                Glyph::Type3(_) => None,
            };
            let to_next = || {
                let next = glyphs.get(i + 1)?;
                Some((place.inverse() * props.transform * next.transform() * Point::ORIGIN).x)
            };
            let advance =
                [width, to_next()].into_iter().flatten().find(|a| a.is_finite() && *a > 0.0).unwrap_or(DEFAULT_ADVANCE);
            // 2. Its em box on the page, as an upright rectangle.
            let corners = [(0.0, DESCENT), (advance, DESCENT), (0.0, ASCENT), (advance, ASCENT)]
                .map(|(x, y)| place * Point::new(x, y));
            let (mut lo, mut hi) = (corners[0], corners[0]);
            for c in &corners[1..] {
                (lo.x, lo.y, hi.x, hi.y) = (lo.x.min(c.x), lo.y.min(c.y), hi.x.max(c.x), hi.y.max(c.y));
            }
            let page = self.page;
            if hi.x < page.x0 || lo.x > page.x1 || hi.y < page.y0 || lo.y > page.y1 {
                continue;
            }
            // 3. Which way it runs, and what it says.
            let rect = [lo.x as f32, lo.y as f32, hi.x as f32, hi.y as f32];
            let baseline = place * Point::new(advance, 0.0) - place * Point::ORIGIN;
            let dir = Direction::of(baseline.x, baseline.y);
            let pushed = match &text {
                BfString::Char(c) => self.glyphs.push(rect, dir, c.encode_utf8(&mut [0; 4])),
                BfString::String(s) => self.glyphs.push(rect, dir, s),
            };
            if !pushed {
                self.full = true;
                return;
            }
        }
    }
}

/// Reads the text on page `index`, as an answer for [`PageText::decode`].
/// The interpreter runs over the page as for drawing, with the page as
/// shown (rotation applied, y going down) as the device's space, so the
/// boxes come out in the units the viewer measures pages in.
fn read_text(pdf: &Pdf, index: usize, log: &WarningLog) -> Result<Vec<u8>, String> {
    let pages = pdf.pages();
    let page = pages.get(index).ok_or_else(|| format!("the document has no page {}", index + 1))?;
    let (w, h) = page.render_dimensions();
    let bounds = Rect::new(0.0, 0.0, w as f64, h as f64);
    let cache = InterpreterCache::new();
    let mut context = Context::new(page.initial_transform(true).to_kurbo(), bounds, &cache, page.xref(), settings(log));
    let mut device = TextDevice { page: bounds, glyphs: Builder::default(), last_run: None, full: false };
    hayro::hayro_interpret::interpret_page(page, &mut context, &mut device);
    Ok(device.glyphs.encode())
}

//endregion

/// Page `index`'s `/Thumb` picture, which some writers store (Acrobat's
/// "embed page thumbnails"), as an answer like [`draw`]'s.  Only the usual
/// kinds, 8-bit grey or RGB, are taken; anything else is drawn instead.
fn page_thumb(pdf: &Pdf, index: usize) -> Option<Vec<u8>> {
    let page = pdf.pages().get(index)?;
    let stream = page.raw().get::<Stream<'_>>(b"Thumb")?;
    let d = stream.dict();
    let (w, h) = (d.get::<u32>(b"Width")?, d.get::<u32>(b"Height")?);
    if !crate::stored::usable(w, h) || d.get::<u32>(b"BitsPerComponent").unwrap_or(8) != 8 {
        return None;
    }
    let n: u8 = match d.get::<Name<'_>>(b"ColorSpace")?.as_str() {
        "DeviceRGB" => 3,
        "DeviceGray" => 1,
        _ => return None,
    };
    let params = ImageDecodeParams {
        is_indexed: false,
        bpc: Some(8),
        num_components: Some(n),
        target_dimension: None,
        width: w,
        height: h,
    };
    let data = stream.decoded_image(&params).ok()?.data;
    let count = w as usize * h as usize;
    if data.len() < count * n as usize {
        return None;
    }
    let mut out = Vec::with_capacity(8 + count * 4);
    out.extend(w.to_le_bytes());
    out.extend(h.to_le_bytes());
    match n {
        3 => data.chunks_exact(3).take(count).for_each(|p| out.extend([p[0], p[1], p[2], 255])),
        _ => data.iter().take(count).for_each(|&g| out.extend([g, g, g, 255])),
    }
    Some(out)
}

/*┌─────────────────╮
| Facts           |
└─────────────────╯*/
//region Facts

/// What the viewer learns when a document opens.
#[derive(Debug, Default)]
struct Opening {
    /// Page sizes in points as shown (rotation applied), at most
    /// [`MAX_PAGES`].
    sizes: Vec<(f32, f32)>,
    /// How many pages the document has, which may be more than listed.
    total: usize,
    facts: Vec<(String, String)>,
}

impl Opening {
    fn read(format: DocumentFormat, path: &Path, pdf: &Pdf) -> Opening {
        let pages = pdf.pages();
        let sizes: Vec<(f32, f32)> = pages.iter().take(MAX_PAGES).map(|p| p.render_dimensions()).collect();
        let mut facts = Vec::new();
        let mut fact = |k: &str, v: String| {
            if !v.is_empty() {
                facts.push((k.to_string(), v));
            }
        };
        let version = format!("{:?}", pdf.version()).replace("Pdf", "");
        let version = match version.as_bytes() {
            [a, b] => format!("{}.{}", *a as char, *b as char),
            _ => version,
        };
        fact(
            "Format",
            match format {
                DocumentFormat::Illustrator => format!("Adobe Illustrator, PDF-compatible (PDF {version})"),
                _ => format!("PDF {version}"),
            },
        );
        let word = if format == DocumentFormat::Illustrator { "Artboards" } else { "Pages" };
        fact(word, pages.len().to_string());
        if let Some(&(w, h)) = sizes.first() {
            let same = sizes.iter().all(|&(a, b)| (a - w).abs() < 1.0 && (b - h).abs() < 1.0);
            let label = if same || sizes.len() == 1 { "Page size" } else { "First page" };
            fact(label, crate::describe_size(w as f64, h as f64));
        }
        let m = pdf.metadata();
        let t = |v: &Option<Vec<u8>>| v.as_deref().map(text::pdf_text).unwrap_or_default();
        fact("Title", t(&m.title));
        fact("Author", t(&m.author));
        fact("Subject", t(&m.subject));
        fact("Keywords", t(&m.keywords));
        fact("Made with", t(&m.creator));
        fact("Producer", t(&m.producer));
        fact("Created", m.creation_date.as_ref().map(text::pdf_date).unwrap_or_default());
        fact("Modified", m.modification_date.as_ref().map(text::pdf_date).unwrap_or_default());
        if encrypted(path) {
            fact("Encrypted", "yes, without a password to open (printing or copying may be restricted)".into());
        }
        Opening { sizes, total: pages.len(), facts }
    }

    fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend((self.total as u64).to_le_bytes());
        out.extend((self.sizes.len() as u64).to_le_bytes());
        for (w, h) in &self.sizes {
            out.extend(w.to_le_bytes());
            out.extend(h.to_le_bytes());
        }
        for (k, v) in &self.facts {
            out.extend(k.replace(['\t', '\n'], " ").bytes());
            out.push(b'\t');
            out.extend(v.replace(['\t', '\n'], " ").bytes());
            out.push(b'\n');
        }
        out
    }

    fn decode(b: &[u8]) -> Option<Opening> {
        let total = u64::from_le_bytes(b.get(0..8)?.try_into().ok()?) as usize;
        let n = u64::from_le_bytes(b.get(8..16)?.try_into().ok()?) as usize;
        if n > MAX_PAGES {
            return None;
        }
        let mut sizes = Vec::with_capacity(n);
        for i in 0..n {
            let o = 16 + i * 8;
            let w = f32::from_le_bytes(b.get(o..o + 4)?.try_into().ok()?);
            let h = f32::from_le_bytes(b.get(o + 4..o + 8)?.try_into().ok()?);
            sizes.push((w, h));
        }
        let rest = String::from_utf8_lossy(b.get(16 + n * 8..)?);
        let facts =
            rest.lines().filter_map(|l| l.split_once('\t')).map(|(k, v)| (k.to_string(), v.to_string())).collect();
        Some(Opening { sizes, total, facts })
    }
}

/// Whether the file's trailer names an encryption dictionary.  It opened
/// without a password, so only an owner password is set.
fn encrypted(path: &Path) -> bool {
    use std::io::{Read, Seek, SeekFrom};
    let Ok(mut f) = std::fs::File::open(path) else { return false };
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    let mut tail = Vec::new();
    let from = len.saturating_sub(64 << 10);
    if f.seek(SeekFrom::Start(from)).is_err() || f.take(64 << 10).read_to_end(&mut tail).is_err() {
        return false;
    }
    tail.windows(8).any(|w| w == b"/Encrypt")
}

//endregion

/*┌─────────────────╮
| One-off work    |
└─────────────────╯*/
//region One-off work

/// The first page's own `/Thumb` picture, read in a child process without
/// drawing anything; `None` when the page has none Slerp takes.
pub fn page_thumbnail(format: DocumentFormat, path: &Path) -> Result<Option<Texture>, LoadError> {
    let owned = path.to_path_buf();
    let limits = Limits { time: crate::THUMBNAIL_TIME, memory: crate::THUMBNAIL_MEMORY, output: answer_bytes() };
    let answer = slerp_isolate::run(limits, move || Ok(page_thumb(&open(&owned)?, 0).unwrap_or_default()))
        .map_err(|e| LoadError::Unsupported(sentence(&e)))?;
    if answer.is_empty() {
        return Ok(None);
    }
    let image = decode_image(answer).map_err(LoadError::Invalid)?;
    let container = match format {
        DocumentFormat::Illustrator => "Adobe Illustrator (PDF)",
        _ => "PDF",
    };
    let facts = vec![("Shown".into(), "The page thumbnail stored in the file".into())];
    let mut t = crate::texture(image.width, image.height, image.pixels, container, facts);
    t.source.file_bytes = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    Ok(Some(t))
}

/// Draws page `index` (counting from 0) fitted into `fit` pixels, for a
/// thumbnail or `slerp info`, in a child process.
pub fn picture(
    format: DocumentFormat,
    path: &Path,
    index: usize,
    fit: u32,
    time: Duration,
    memory: u64,
) -> Result<Texture, LoadError> {
    let owned = path.to_path_buf();
    let limits = Limits { time, memory, output: answer_bytes() + (1 << 20) };
    let answer = slerp_isolate::run(limits, move || {
        let pdf = open(&owned)?;
        let opening = Opening::read(format, &owned, &pdf);
        let size = *opening.sizes.get(index).ok_or_else(|| {
            if opening.total == 0 {
                "the document has no pages".to_string()
            } else {
                format!("the document has no page {}", index + 1)
            }
        })?;
        let image = draw(&pdf, &DrawView::fit(index, size, fit), &WarningLog::default())?;
        let opened = opening.encode();
        let mut out = Vec::new();
        out.extend((opened.len() as u64).to_le_bytes());
        out.extend(opened);
        out.extend(image);
        Ok(out)
    })
    .map_err(|e| LoadError::Unsupported(sentence(&e)))?;
    let bad = || LoadError::invalid("the renderer's answer was damaged");
    let n = u64::from_le_bytes(answer.get(..8).ok_or_else(bad)?.try_into().map_err(|_| bad())?) as usize;
    let opening = answer.get(8..8usize.saturating_add(n)).and_then(Opening::decode).ok_or_else(bad)?;
    let image = decode_image(answer.get(8 + n..).ok_or_else(bad)?.to_vec()).map_err(LoadError::Invalid)?;
    let container = match format {
        DocumentFormat::Illustrator => "Adobe Illustrator (PDF)",
        _ => "PDF",
    };
    let mut facts = opening.facts;
    if opening.total > 1 {
        facts.push(("Shown".into(), format!("{} {} of {}", format.page_word(), index + 1, opening.total)));
    }
    let mut t = crate::texture(image.width, image.height, image.pixels, container, facts);
    t.source.format = "vector page (RGBA)".into();
    t.source.file_bytes = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    Ok(t)
}

/// How many pages the document has, read in a child process.
pub fn page_count(path: &Path) -> Result<usize, String> {
    let owned = path.to_path_buf();
    let limits = Limits { time: Duration::from_secs(10), memory: 1 << 30, output: 64 };
    let answer = slerp_isolate::run(limits, move || Ok((open(&owned)?.pages().len() as u64).to_le_bytes().to_vec()))?;
    Ok(u64::from_le_bytes(answer.get(..8).and_then(|b| b.try_into().ok()).ok_or("a damaged answer")?) as usize)
}

/// A child's failure as the end of a sentence that starts "Slerp couldn't
/// draw this document: ".
fn sentence(why: &str) -> String {
    match why.strip_prefix("it ") {
        Some(rest) => format!("the PDF renderer {rest}"),
        None => why.to_string(),
    }
}

//endregion

/*┌─────────────────╮
| Open documents  |
└─────────────────╯*/
//region Open documents

/// A PDF kept open in a worker process, whose pages the viewer draws at any
/// scale.  When a page crashes the worker or runs out of time, the next
/// picture starts a new one.
pub struct PdfPages {
    pub format: DocumentFormat,
    path: PathBuf,
    worker: Option<Worker>,
    /// Page sizes in points as shown, at most [`MAX_PAGES`].
    pub sizes: Vec<(f32, f32)>,
    /// How many pages the document has (more than `sizes` lists when it
    /// has more than [`MAX_PAGES`]).
    pub total: usize,
    pub facts: Vec<(String, String)>,
    pub warnings: Vec<String>,
}

impl PdfPages {
    /// Opens the document in a new worker.
    pub fn open(format: DocumentFormat, path: &Path) -> Result<PdfPages, LoadError> {
        let (worker, opening) = start(format, path).map_err(|e| LoadError::Unsupported(sentence(&e)))?;
        let mut warnings = Vec::new();
        if opening.total == 0 {
            return Err(LoadError::Unsupported("the document has no pages".into()));
        }
        if opening.total > opening.sizes.len() {
            warnings.push(format!("Only the first {} of {} pages are listed", opening.sizes.len(), opening.total));
        }
        Ok(PdfPages {
            format,
            path: path.to_path_buf(),
            worker: Some(worker),
            sizes: opening.sizes,
            total: opening.total,
            facts: opening.facts,
            warnings,
        })
    }

    /// The pages' names, "Page 1" or "Artboard 1" onwards.
    pub fn names(&self) -> Vec<String> {
        (1..=self.sizes.len()).map(|i| format!("{} {i}", self.format.page_word())).collect()
    }

    /// Draws a view of a page.  The picture's pixels past the page's edges
    /// are transparent.
    pub fn draw(&mut self, view: &DrawView) -> Result<Image8, String> {
        view.check()?;
        let answer = self.ask(view.page, &view.encode())?;
        decode_image(answer)
    }

    /// The text on page `page` (from 0), with each glyph's box in points
    /// from the page's top left as shown, for selecting and copying.  Empty
    /// for pages without text, such as scans.
    pub fn text(&mut self, page: usize) -> Result<PageText, String> {
        let mut request = TEXT_REQUEST.to_vec();
        request.extend((page as u64).to_le_bytes());
        let answer = self.ask(page, &request)?;
        PageText::decode(&answer).ok_or_else(|| "the renderer's answer was damaged".to_string())
    }

    /// Asks the worker about page `page`, starting a new worker first if
    /// the last one stopped.
    fn ask(&mut self, page: usize, request: &[u8]) -> Result<Vec<u8>, String> {
        if page >= self.sizes.len() {
            return Err(format!("the document has no page {}", page + 1));
        }
        if !self.worker.as_ref().is_some_and(Worker::alive) {
            self.worker = Some(start(self.format, &self.path).map_err(|e| sentence(&e))?.0);
        }
        let worker = self.worker.as_mut().ok_or("no renderer")?;
        worker.ask_within(request, crate::DRAW_TIME).map_err(|e| sentence(&e))
    }

    /// Warnings the renderer gave while drawing so far, in words.
    pub fn draw_warnings(&mut self) -> Vec<String> {
        let Some(worker) = self.worker.as_mut().filter(|w| w.alive()) else { return Vec::new() };
        match worker.ask_within(b"warnings", Duration::from_secs(2)) {
            Ok(text) => String::from_utf8_lossy(&text).lines().map(str::to_string).collect(),
            Err(_) => Vec::new(),
        }
    }
}

/// Starts a worker on the document: it opens the file and answers with
/// the [`Opening`], then draws views and reads pages' text until it is
/// dropped.
fn start(format: DocumentFormat, path: &Path) -> Result<(Worker, Opening), String> {
    let owned = path.to_path_buf();
    let limits = Limits { time: crate::OPEN_TIME, memory: crate::VIEWER_MEMORY, output: answer_bytes() };
    let (worker, opening) = Worker::start(
        limits,
        move || {
            let pdf = open(&owned)?;
            let opening = Opening::read(format, &owned, &pdf).encode();
            Ok(((pdf, WarningLog::default()), opening))
        },
        |(pdf, log), request| {
            if request == b"warnings" {
                let log = log.lock().map(|l| l.clone()).unwrap_or_default();
                let mut lines: Vec<&str> = log.iter().map(warning_text).collect();
                lines.dedup();
                return Ok(lines.join("\n").into_bytes());
            }
            if let Some(page) = request.strip_prefix(TEXT_REQUEST) {
                let page = u64::from_le_bytes(page.try_into().map_err(|_| "a damaged request")?);
                return read_text(pdf, usize::try_from(page).map_err(|_| "a damaged request")?, log);
            }
            let view = DrawView::decode(request).ok_or("a damaged request")?;
            draw(pdf, &view, log)
        },
    )?;
    let opening = Opening::decode(&opening).ok_or("the renderer's answer was damaged")?;
    Ok((worker, opening))
}

//endregion
