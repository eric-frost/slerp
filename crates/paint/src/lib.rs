//! Layered paint documents: Photoshop (`.psd`, `.psb`), GIMP (`.xcf`, also
//! gzip and bzip2 compressed), OpenRaster (`.ora`) and Krita (`.kra`).
//!
//! Photoshop, GIMP and OpenRaster files become a [`Canvas`] whose layers the
//! viewer lists and can hide one by one: the picture the file stores
//! (Photoshop's composite, OpenRaster's `mergedimage.png`) is shown while
//! every layer keeps its saved visibility, and the layers are composed here
//! otherwise, or when the stored picture is missing or blank, as GIMP files
//! always need.  32-bit Photoshop files open as a floating-point picture for
//! the exposure slider instead, with their layers listed.  Krita files are
//! shown by their merged picture, with the layer list in the inspector.
//!
//! What can't be reproduced (layer styles, adjustment layers, GIMP 3's
//! filters, unknown blend modes) is said in the warnings.  Files are read
//! into memory, capped by [`slerp_scene::Limits::max_file_bytes`], and every
//! reader bounds-checks what it reads.

pub mod compose;
mod ora;
mod psd;
mod psd_vector;
mod xcf;

use compose::{Kind, Mode, Node, Raster, Rules, Samples, Source};
use slerp_scene::{Asset, Canvas, CanvasLayer, FrameClip, Image8, Info, Limits, LoadError, Texture};
use std::path::Path;
use std::sync::Arc;

/// The largest composition, in pixels: about 4096 × 4096.  Larger documents
/// are composed scaled down; four canvases of floats stay near a gigabyte.
const MAX_COMPOSE_PIXELS: f64 = 16.0 * 1024.0 * 1024.0;
/// Files larger than this are refused: they are read whole.
const MAX_FILE_BYTES: u64 = 4 << 30;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PaintFormat {
    Psd,
    Xcf,
    Ora,
    Kra,
}

impl PaintFormat {
    pub fn name(self) -> &'static str {
        match self {
            PaintFormat::Psd => "Photoshop",
            PaintFormat::Xcf => "GIMP",
            PaintFormat::Ora => "OpenRaster",
            PaintFormat::Kra => "Krita",
        }
    }
}

/// Recognises a file by its first bytes; compressed GIMP files also by
/// name, since gzip and bzip2 say nothing about what they hold.
pub fn sniff(path: &Path, head: &[u8]) -> Option<PaintFormat> {
    if psd::is_psd(head) {
        return Some(PaintFormat::Psd);
    }
    if xcf::is_xcf(head) {
        return Some(PaintFormat::Xcf);
    }
    match ora::zip_kind(head) {
        Some("ora") => return Some(PaintFormat::Ora),
        Some("kra") => return Some(PaintFormat::Kra),
        _ => {}
    }
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("").to_ascii_lowercase();
    let gz = head.starts_with(&[0x1f, 0x8b]) && (name.ends_with(".xcf.gz") || name.ends_with(".xcfgz"));
    let bz = head.starts_with(b"BZh") && (name.ends_with(".xcf.bz2") || name.ends_with(".xcfbz2"));
    if gz || bz {
        return Some(PaintFormat::Xcf);
    }
    // Zips whose first entry isn't the mimetype, named for what they are.
    if head.starts_with(b"PK\x03\x04") {
        if name.ends_with(".ora") {
            return Some(PaintFormat::Ora);
        }
        if name.ends_with(".kra") || name.ends_with(".krz") {
            return Some(PaintFormat::Kra);
        }
    }
    None
}

pub fn load(format: PaintFormat, path: &Path, limits: &Limits) -> Result<Asset, LoadError> {
    let guard = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match format {
        PaintFormat::Psd => load_psd(path, limits),
        PaintFormat::Xcf => load_xcf(path, limits),
        PaintFormat::Ora => load_ora(path, limits),
        PaintFormat::Kra => load_kra(path, limits),
    }));
    guard.unwrap_or_else(|_| Err(LoadError::Invalid("internal decoder error".into())))
}

/// The picture the painting program saved of the document, read without
/// composing layers: Photoshop's composite (the "Maximize Compatibility"
/// picture), else the small preview Photoshop embeds; OpenRaster's and
/// Krita's merged picture, or for a thumbnail no larger than their small
/// preview, that preview.  `None` for GIMP files, which keep no picture of
/// themselves, for 32-bit Photoshop files (their composite is linear
/// light, which the viewer and thumbnails tone-map from the full load) and
/// for files without one.  Slerp's core shows it as the file's thumbnail
/// in place of a composition; see `slerp_core::saved_picture`.
pub fn saved_picture(format: PaintFormat, path: &Path, limits: &Limits) -> Result<Option<Texture>, LoadError> {
    let guard = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match format {
        PaintFormat::Psd => saved_psd(path, limits),
        PaintFormat::Ora => saved_zip(path, limits, "OpenRaster", "Thumbnails/thumbnail.png"),
        PaintFormat::Kra => saved_zip(path, limits, "Krita", "preview.png"),
        PaintFormat::Xcf => Ok(None),
    }));
    guard.unwrap_or_else(|_| Err(LoadError::Invalid("internal decoder error".into())))
}

fn saved_psd(path: &Path, limits: &Limits) -> Result<Option<Texture>, LoadError> {
    let bytes = read_file(path, limits)?;
    let file_bytes = bytes.len() as u64;
    let psd = psd::Psd::parse(Arc::new(bytes)).map_err(invalid)?;
    if psd.depth == 32 {
        return Ok(None);
    }
    let step = step_for(psd.width, psd.height, limits);
    let (mut tex, what) = match psd.merged(step) {
        Ok((w, h, s)) if !psd.merged_is_blank(&s) => {
            let img = orient(image8(w, h, &s), psd.orientation);
            (Texture::from_rgba8(img.width, img.height, img.pixels, slerp_scene::ColorSpace::Srgb), "composite")
        }
        _ => match psd.thumbnail() {
            Some(t) => (t, "embedded preview"),
            None => return Ok(None),
        },
    };
    tex.source.container = "Photoshop".into();
    tex.source.format = format!("the {what} Photoshop saved");
    tex.source.file_bytes = file_bytes;
    Ok(Some(tex))
}

/// The merged picture of an OpenRaster or Krita file, or its small
/// `preview` when that is at least the thumbnail's size (or the only one).
fn saved_zip(path: &Path, limits: &Limits, container: &str, preview: &str) -> Result<Option<Texture>, LoadError> {
    let archive = open_zip(path, limits)?;
    let read = |name: &str| {
        let i = *archive.find(name).last()?;
        archive.read(archive.entry(i)?).ok().and_then(|b| ora::png_raster(&b, limits.max_texture_size).ok())
    };
    let small = read(preview);
    let raster = match small {
        Some(p) if preview_will_do(&p, limits) => Some(p),
        small => read("mergedimage.png").or(small),
    };
    Ok(raster.map(|r| {
        let img = raster_image(&r);
        let mut tex = Texture::from_rgba8(img.width, img.height, img.pixels, slerp_scene::ColorSpace::Srgb);
        tex.source.container = container.into();
        tex.source.format = format!("the merged picture {container} saved");
        tex.source.file_bytes = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
        tex
    }))
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

/// Every `step`th pixel each way that keeps a picture of `w` × `h` near
/// what the limits ask for: a thumbnail's size, else what is composed.
fn step_for(w: u32, h: u32, limits: &Limits) -> usize {
    match limits.max_texture_size {
        Some(max) => (w.max(h) / (2 * max.max(1))).max(1) as usize,
        None => ((w as f64 * h as f64 / MAX_COMPOSE_PIXELS).sqrt().ceil() as usize).max(1),
    }
}

/*┌─────────────────╮
| The canvas      |
└─────────────────╯*/
//region The canvas

/// A layered document as the viewer and thumbnails see it.
struct Layered {
    info: Info,
    width: u32,
    height: u32,
    nodes: Vec<Node>,
    layers: Vec<CanvasLayer>,
    /// Each slot's visibility as saved.
    saved: Vec<bool>,
    /// The picture the file stores, possibly smaller than the document.
    stored: Option<Image8>,
    source: Arc<dyn Source>,
    rules: Rules,
    /// EXIF orientation applied to every picture (Photoshop files only).
    orientation: u8,
}

impl Layered {
    fn new(
        info: Info,
        (width, height): (u32, u32),
        mut nodes: Vec<Node>,
        stored: Option<Image8>,
        source: Arc<dyn Source>,
        rules: Rules,
    ) -> Layered {
        let count = compose::flatten(&mut nodes);
        let mut layers = Vec::with_capacity(count);
        let mut saved = Vec::with_capacity(count);
        compose::visit(&nodes, 0, &mut |n, depth| {
            layers.push(CanvasLayer {
                name: n.name.clone(),
                visible: n.visible,
                depth,
                group: matches!(n.kind, Kind::Group { .. }),
                note: layer_note(n),
            });
            saved.push(n.visible);
        });
        Layered { info, width, height, nodes, layers, saved, stored, source, rules, orientation: 1 }
    }
}

/// "multiply, 50 %, masked" and the like.
fn layer_note(n: &Node) -> String {
    let mut parts: Vec<String> = Vec::new();
    match &n.kind {
        Kind::Group { isolated: false, .. } => parts.push("pass through".into()),
        Kind::Empty => parts.push("nothing to draw".into()),
        _ => {}
    }
    if !matches!(n.mode, Mode::Normal | Mode::PassThrough) {
        parts.push(n.mode.name().into());
    }
    if n.opacity < 0.995 {
        parts.push(format!("{:.0} %", n.opacity * 100.0));
    }
    if n.mask.is_some() {
        parts.push("masked".into());
    }
    if n.clip {
        parts.push("clipped".into());
    }
    parts.join(", ")
}

impl Canvas for Layered {
    fn info(&self) -> &Info {
        &self.info
    }

    fn size(&self) -> (u32, u32) {
        if self.orientation >= 5 { (self.height, self.width) } else { (self.width, self.height) }
    }

    fn layers(&self) -> &[CanvasLayer] {
        &self.layers
    }

    fn frames(&self) -> &[f32] {
        &[0.1]
    }

    fn clips(&self) -> &[FrameClip] {
        &[]
    }

    fn compose(&self, _frame: usize, shown: &[bool], scale: f32) -> Image8 {
        let (w, h) = (self.width as f64, self.height as f64);
        let fits = (MAX_COMPOSE_PIXELS / (w * h).max(1.0)).sqrt() as f32;
        let scale = if scale.is_finite() && scale > 0.0 { scale.min(1.0).min(fits) } else { fits.min(1.0) };
        let tw = ((w as f32 * scale).round() as u32).max(1);
        let th = ((h as f32 * scale).round() as u32).max(1);
        let visibility: Vec<bool> =
            (0..self.saved.len()).map(|i| shown.get(i).copied().unwrap_or(self.saved[i])).collect();
        // SLERP_PAINT_COMPOSE=1 composes the layers even when the file's own
        // picture would do, for comparing the compositor with references.
        let force = std::env::var_os("SLERP_PAINT_COMPOSE").is_some();
        if let Some(stored) = &self.stored
            && visibility == self.saved
            && !force
        {
            return orient(resample(stored, tw, th), self.orientation);
        }
        let mut problems = Vec::new();
        let canvas = compose::compose(
            &self.nodes,
            &*self.source,
            self.rules,
            self.width,
            self.height,
            scale,
            &visibility,
            &mut problems,
        );
        for p in problems.iter().take(3) {
            eprintln!("slerp: {p}");
        }
        let linear = !matches!(self.rules, Rules::Adobe { linear: false });
        orient(compose::to_image8(&canvas, linear), self.orientation)
    }
}

/// A picture resized to `w` × `h`: averaged when shrinking (weighted by
/// alpha), nearest when growing.
fn resample(img: &Image8, w: u32, h: u32) -> Image8 {
    if (img.width, img.height) == (w, h) {
        return img.clone();
    }
    let (sw, sh) = (img.width as usize, img.height as usize);
    let mut pixels = vec![0u8; w as usize * h as usize * 4];
    use rayon::prelude::*;
    pixels.par_chunks_mut(w as usize * 4).enumerate().for_each(|(y, row)| {
        let y0 = y * sh / h as usize;
        let y1 = ((y + 1) * sh / h as usize).max(y0 + 1).min(sh);
        for x in 0..w as usize {
            let x0 = x * sw / w as usize;
            let x1 = ((x + 1) * sw / w as usize).max(x0 + 1).min(sw);
            let mut acc = [0u64; 4];
            let mut n = 0u64;
            for sy in y0..y1 {
                for sx in x0..x1 {
                    let p = &img.pixels[(sy * sw + sx) * 4..(sy * sw + sx) * 4 + 4];
                    let a = p[3] as u64;
                    for c in 0..3 {
                        acc[c] += p[c] as u64 * a;
                    }
                    acc[3] += a;
                    n += 1;
                }
            }
            let out = &mut row[x * 4..x * 4 + 4];
            if acc[3] > 0 {
                for c in 0..3 {
                    out[c] = (acc[c] / acc[3]) as u8;
                }
            }
            out[3] = (acc[3] / n.max(1)) as u8;
        }
    });
    Image8 { width: w, height: h, pixels }
}

/// Turns a picture the way EXIF orientation `o` (1 to 8) says.
fn orient(img: Image8, o: u8) -> Image8 {
    if !(2..=8).contains(&o) {
        return img;
    }
    let (w, h) = (img.width as usize, img.height as usize);
    let (ow, oh) = if o >= 5 { (h, w) } else { (w, h) };
    let mut pixels = vec![0u8; w * h * 4];
    for y in 0..h {
        for x in 0..w {
            // Where pixel (x, y) of the stored picture goes.
            let (dx, dy) = match o {
                2 => (w - 1 - x, y),
                3 => (w - 1 - x, h - 1 - y),
                4 => (x, h - 1 - y),
                5 => (y, x),
                6 => (h - 1 - y, x),
                7 => (h - 1 - y, w - 1 - x),
                _ => (y, w - 1 - x),
            };
            let (s, d) = ((y * w + x) * 4, (dy * ow + dx) * 4);
            pixels[d..d + 4].copy_from_slice(&img.pixels[s..s + 4]);
        }
    }
    Image8 { width: ow as u32, height: oh as u32, pixels }
}

/// RGBA samples to an 8-bit picture (16-bit by their high byte, floats
/// sRGB-encoded).
fn image8(w: u32, h: u32, s: &Samples) -> Image8 {
    let pixels = match s {
        Samples::U8(v) | Samples::Srgb8(v) => v.clone(),
        Samples::U16(v) | Samples::Srgb16(v) => v.iter().map(|&x| ((x as u32 * 255 + 32767) / 65535) as u8).collect(),
        Samples::F32(v) => v
            .iter()
            .enumerate()
            .map(|(i, &x)| {
                let x = if i % 4 == 3 { x } else { compose::srgb_encode(x) };
                (x.clamp(0.0, 1.0) * 255.0 + 0.5) as u8
            })
            .collect(),
    };
    Image8 { width: w, height: h, pixels }
}

fn picture(img: Image8, container: &str, format: &str, header: Vec<(String, String)>, file_bytes: u64) -> Asset {
    let mut tex = Texture::from_rgba8(img.width, img.height, img.pixels, slerp_scene::ColorSpace::Srgb);
    tex.source.container = container.into();
    tex.source.format = format.into();
    tex.source.header = header;
    tex.source.file_bytes = file_bytes;
    Asset::Image(Arc::new(tex))
}

/// The layer list as inspector lines, for pictures that aren't canvases.
fn layer_lines(nodes: &[Node]) -> Vec<(String, String)> {
    let mut out = Vec::new();
    compose::visit(nodes, 0, &mut |n, depth| {
        if out.len() < 200 {
            let note = layer_note(n);
            let hidden = if n.visible { "" } else { " (hidden)" };
            let note = if note.is_empty() { String::new() } else { format!(" [{note}]") };
            out.push(("Layer".to_string(), format!("{}{}{note}{hidden}", "  ".repeat(depth as usize), n.name)));
        }
    });
    out
}

fn has_pixels(nodes: &[Node]) -> bool {
    let mut any = false;
    compose::visit(nodes, 0, &mut |n, _| any |= matches!(n.kind, Kind::Raster { .. }));
    any
}

//endregion

/*┌─────────────────╮
| Photoshop       |
└─────────────────╯*/
//region Photoshop

fn load_psd(path: &Path, limits: &Limits) -> Result<Asset, LoadError> {
    let bytes = read_file(path, limits)?;
    let file_bytes = bytes.len() as u64;
    let psd = Arc::new(psd::Psd::parse(Arc::new(bytes)).map_err(invalid)?);
    let mut warnings = psd.notes.clone();
    let tree = psd.tree(&mut warnings);
    let layered = has_pixels(&tree);
    let step = step_for(psd.width, psd.height, limits);
    let stored = match psd.merged(step) {
        Ok((w, h, s)) if !psd.merged_is_blank(&s) => Some((w, h, s)),
        Ok(_) => {
            warnings.push("the file stores no composite picture (\"Maximize Compatibility\" was off); the layers are composed here".into());
            None
        }
        Err(e) => {
            warnings.push(format!("the composite picture couldn't be read ({e})"));
            None
        }
    };
    let mut details = vec![
        ("Colour mode".to_string(), psd::mode_name(psd.mode).to_string()),
        ("Bits per channel".into(), psd.depth.to_string()),
        ("Channels".into(), psd.channels.to_string()),
        ("Layers".into(), psd.layers.iter().filter(|l| l.section != 3).count().to_string()),
    ];
    if psd.psb {
        details.push(("Large document".into(), "PSB".into()));
    }
    if let Some(w) = &psd.writer {
        details.push(("Written by".into(), w.clone()));
    }
    if psd.icc {
        details.push(("Colour profile".into(), "embedded (not applied)".into()));
    }
    match psd.mode {
        4 => warnings.push("CMYK is converted without the colour profile, so colours are approximate".into()),
        8 => warnings.push("duotone is shown as its grey channel, without the inks".into()),
        7 => warnings.push("multichannel is shown as process inks".into()),
        _ => {}
    }
    let shown = match (&stored, layered) {
        (Some(_), _) => "the composite the file stores",
        (None, true) => "composed from the layers",
        (None, false) => "the embedded thumbnail",
    };
    details.push(("Picture".into(), shown.into()));
    let format =
        format!("Photoshop {} ({}-bit {})", if psd.psb { "PSB" } else { "PSD" }, psd.depth, psd::mode_name(psd.mode));
    // 1. 32-bit documents hold linear light: a float picture for the
    //    exposure slider, composed in floats when the composite is missing.
    if psd.depth == 32 {
        let mut header = details.clone();
        header.extend(warnings.iter().map(|w| ("Warning".to_string(), w.clone())));
        header.extend(layer_lines(&tree));
        let (w, h, px) = match stored {
            Some((w, h, compose::Samples::F32(v))) => (w, h, v),
            _ if layered => {
                let scale = 1.0 / step as f32;
                let c = compose::compose(
                    &tree,
                    &*psd,
                    Rules::Adobe { linear: true },
                    psd.width,
                    psd.height,
                    scale,
                    &[],
                    &mut Vec::new(),
                );
                (c.width, c.height, c.px)
            }
            _ => return Err(LoadError::Invalid("the file has neither a composite nor layers".into())),
        };
        let mut tex = Texture::from_rgba32f(w, h, px);
        tex.source.container = "Photoshop".into();
        tex.source.format = format;
        tex.source.header = header;
        tex.source.file_bytes = file_bytes;
        return Ok(Asset::Image(Arc::new(tex)));
    }
    let stored_img = stored.map(|(w, h, s)| image8(w, h, &s));
    // 2. Layers: a canvas the viewer can hide them in.
    if layered {
        details.push(("File size".into(), format!("{file_bytes} bytes")));
        let info = Info { format, warnings, details, ..Default::default() };
        let rules = Rules::Adobe { linear: false };
        let orientation = psd.orientation;
        let mut canvas = Layered::new(info, (psd.width, psd.height), tree, stored_img, psd, rules);
        canvas.orientation = orientation;
        return Ok(Asset::Canvas(Arc::new(canvas)));
    }
    // 3. A flat document, or failing everything the thumbnail.
    let mut header = details;
    if let Some(img) = stored_img {
        header.extend(warnings.iter().map(|w| ("Warning".to_string(), w.clone())));
        return Ok(picture(orient(img, psd.orientation), "Photoshop", &format, header, file_bytes));
    }
    let mut tex = psd.thumbnail().ok_or_else(|| LoadError::Invalid("the file has no readable picture".into()))?;
    warnings.push("only the small preview Photoshop embeds could be shown".into());
    header.extend(warnings.iter().map(|w| ("Warning".to_string(), w.clone())));
    tex.source.container = "Photoshop".into();
    tex.source.format = format;
    tex.source.header = header;
    tex.source.file_bytes = file_bytes;
    Ok(Asset::Image(Arc::new(tex)))
}

//endregion

/*┌─────────────────╮
| GIMP            |
└─────────────────╯*/
//region GIMP

fn load_xcf(path: &Path, limits: &Limits) -> Result<Asset, LoadError> {
    let raw = read_file(path, limits)?;
    let file_bytes = raw.len() as u64;
    let cap = limits.max_file_bytes.min(MAX_FILE_BYTES) as usize;
    // GIMP opens gzip and bzip2 compressed files directly.
    let (bytes, compressed) = if raw.starts_with(&[0x1f, 0x8b]) {
        (gunzip(&raw, cap)?, Some("gzip"))
    } else if raw.starts_with(b"BZh") {
        use std::io::Read;
        let mut out = Vec::new();
        bzip2_rs::DecoderReader::new(raw.as_slice()).take(cap as u64 + 1).read_to_end(&mut out).map_err(invalid)?;
        if out.len() > cap {
            return Err(LoadError::TooLarge("the unpacked file is too large".into()));
        }
        (out, Some("bzip2"))
    } else {
        (raw, None)
    };
    let x = Arc::new(xcf::Xcf::parse(Arc::new(bytes)).map_err(invalid)?);
    let mut warnings = x.notes.clone();
    let tree = x.tree(&mut warnings);
    let precision = match x.component {
        xcf::Component::U8 => "8-bit integer",
        xcf::Component::U16 => "16-bit integer",
        xcf::Component::U32 => "32-bit integer",
        xcf::Component::Half => "16-bit float",
        xcf::Component::Float => "32-bit float",
        xcf::Component::Double => "64-bit float",
    };
    let base = ["RGB", "greyscale", "indexed"][x.base as usize];
    let mut details = vec![
        ("File version".to_string(), x.version.to_string()),
        ("Mode".into(), base.into()),
        ("Precision".into(), format!("{precision}, {}", if x.linear { "linear" } else { "perceptual (sRGB)" })),
        ("Layers".into(), x.layers.len().to_string()),
    ];
    if let Some(c) = compressed {
        details.push(("Compressed".into(), c.into()));
    }
    details.push(("File size".into(), format!("{file_bytes} bytes")));
    let format = format!("GIMP XCF ({base}, {precision})");
    let info = Info { format, warnings, details, ..Default::default() };
    let canvas = Layered::new(info, (x.width, x.height), tree, None, x, Rules::Gimp);
    Ok(Asset::Canvas(Arc::new(canvas)))
}

fn gunzip(data: &[u8], cap: usize) -> Result<Vec<u8>, LoadError> {
    // The gzip header: magic, method, flags, then optional fields.
    let bad = || LoadError::Invalid("a damaged gzip file".into());
    if data.len() < 18 || data[2] != 8 {
        return Err(bad());
    }
    let flags = data[3];
    let mut at = 10;
    if flags & 4 != 0 {
        let n = u16::from_le_bytes([*data.get(at).ok_or_else(bad)?, *data.get(at + 1).ok_or_else(bad)?]) as usize;
        at += 2 + n;
    }
    for bit in [8, 16] {
        if flags & bit != 0 {
            at += data.get(at..).ok_or_else(bad)?.iter().position(|&b| b == 0).ok_or_else(bad)? + 1;
        }
    }
    if flags & 2 != 0 {
        at += 2;
    }
    let body = data.get(at..).ok_or_else(bad)?;
    miniz_oxide::inflate::decompress_to_vec_with_limit(body, cap).map_err(|_| bad())
}

//endregion

/*┌─────────────────╮
| OpenRaster      |
└─────────────────╯*/
//region OpenRaster

fn open_zip(path: &Path, limits: &Limits) -> Result<slerp_archive::Archive, LoadError> {
    let size = std::fs::metadata(path)?.len();
    if size > limits.max_file_bytes.min(MAX_FILE_BYTES) {
        return Err(LoadError::TooLarge(format!("{} MiB", size >> 20)));
    }
    slerp_archive::Archive::open(path).map_err(invalid)
}

fn raster_image(r: &Raster) -> Image8 {
    image8(r.width, r.height, &r.samples)
}

/// A thumbnail-sized request is answered from a small stored preview when
/// that is at least as large.
fn preview_will_do(r: &Raster, limits: &Limits) -> bool {
    limits.max_texture_size.is_some_and(|max| r.width.max(r.height) >= max)
}

fn load_ora(path: &Path, limits: &Limits) -> Result<Asset, LoadError> {
    let archive = open_zip(path, limits)?;
    let file_bytes = std::fs::metadata(path)?.len();
    let ora = match ora::Ora::open(archive) {
        Ok(o) => o,
        Err(e) => {
            // Without a usable stack.xml, the stored pictures still show.
            let archive = open_zip(path, limits)?;
            let fallback = ["mergedimage.png", "Thumbnails/thumbnail.png"].iter().find_map(|p| {
                let i = *archive.find(p).last()?;
                archive.read(archive.entry(i)?).ok().and_then(|b| ora::png_raster(&b, limits.max_texture_size).ok())
            });
            let r = fallback.ok_or_else(|| invalid(&e))?;
            let header = vec![("Warning".to_string(), format!("the layer stack couldn't be read ({e})"))];
            return Ok(picture(raster_image(&r), "OpenRaster", "merged picture", header, file_bytes));
        }
    };
    let mut warnings = ora.notes.clone();
    let mut details = vec![("Layers".to_string(), ora.tree.len().to_string())];
    if !ora.version.is_empty() {
        details.push(("Version".into(), ora.version.clone()));
    }
    let layered = has_pixels(&ora.tree);
    // 1. The merged picture.  (Thumbnails take the saved pictures before
    //    loading; see `saved_picture`.)
    let thumb = ora.thumbnail(limits.max_texture_size).and_then(|t| t.ok());
    let stored = match ora.merged(limits.max_texture_size) {
        Some(Ok(m)) => Some(raster_image(&m)),
        Some(Err(e)) => {
            warnings.push(format!("mergedimage.png couldn't be read ({e})"));
            None
        }
        None => {
            if layered {
                warnings.push("the file has no mergedimage.png; the layers are composed here".into());
            }
            None
        }
    };
    let (w, h) = if ora.width > 0 && ora.height > 0 {
        (ora.width, ora.height)
    } else {
        stored.as_ref().map(|s| (s.width, s.height)).ok_or_else(|| invalid("the stack has no size"))?
    };
    details.push((
        "Picture".into(),
        if stored.is_some() {
            "the merged picture the file stores"
        } else if layered {
            "composed from the layers"
        } else {
            "the thumbnail"
        }
        .into(),
    ));
    details.push(("File size".into(), format!("{file_bytes} bytes")));
    if layered {
        let tree = ora.tree.clone();
        let info = Info { format: "OpenRaster".into(), warnings, details, ..Default::default() };
        let canvas = Layered::new(info, (w, h), tree, stored, Arc::new(ora), Rules::Adobe { linear: false });
        return Ok(Asset::Canvas(Arc::new(canvas)));
    }
    let img = match stored {
        Some(s) => s,
        None => {
            let t = thumb.ok_or_else(|| invalid("the file has no layers, merged picture or thumbnail"))?;
            warnings.push("only the small thumbnail could be shown".into());
            raster_image(&t)
        }
    };
    let mut header = details;
    header.extend(warnings.into_iter().map(|w| ("Warning".to_string(), w)));
    Ok(picture(img, "OpenRaster", "merged picture", header, file_bytes))
}

//endregion

/*┌─────────────────╮
| Krita           |
└─────────────────╯*/
//region Krita

fn load_kra(path: &Path, limits: &Limits) -> Result<Asset, LoadError> {
    let archive = open_zip(path, limits)?;
    let file_bytes = std::fs::metadata(path)?.len();
    let kra = ora::Kra::open(archive).map_err(invalid)?;
    let mut warnings = kra.notes.clone();
    let preview = kra.preview(limits.max_texture_size).and_then(|p| p.ok());
    let (img, shown) = match (&preview, kra.merged(limits.max_texture_size)) {
        (_, Some(Ok(m))) => (raster_image(&m), "the merged picture the file stores"),
        (p, merged) => {
            if let Some(Err(e)) = merged {
                warnings.push(format!("mergedimage.png couldn't be read ({e})"));
            } else {
                warnings.push("the file has no merged picture (Krita before 2.9)".into());
            }
            let p = p.as_ref().ok_or_else(|| invalid("the file has neither a merged picture nor a preview"))?;
            warnings.push("only the small preview Krita stores could be shown".into());
            (raster_image(p), "the stored preview")
        }
    };
    let mut header = Vec::new();
    if kra.width > 0 {
        header.push(("Document size".to_string(), format!("{} × {}", kra.width, kra.height)));
    }
    if !kra.color_space.is_empty() {
        header.push(("Colour space".into(), kra.color_space.clone()));
    }
    header.push(("Layers".into(), kra.layers.len().to_string()));
    header.push(("Picture".into(), shown.into()));
    header.extend(warnings.into_iter().map(|w| ("Warning".to_string(), w)));
    for (depth, name, kind, visible, op, opacity) in kra.layers.iter().take(200) {
        let mut note = vec![kind.trim_end_matches("layer").trim().to_string()];
        if !op.is_empty() && op != "normal" {
            note.push(op.clone());
        }
        if *opacity < 255 {
            note.push(format!("{:.0} %", *opacity as f32 / 2.55));
        }
        let note: Vec<String> = note.into_iter().filter(|s| !s.is_empty()).collect();
        let hidden = if *visible { "" } else { " (hidden)" };
        header.push(("Layer".into(), format!("{}{name} [{}]{hidden}", "  ".repeat(*depth as usize), note.join(", "))));
    }
    Ok(picture(img, "Krita", "merged picture", header, file_bytes))
}

//endregion

#[cfg(test)]
mod tests;
