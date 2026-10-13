//! Page-description documents: PDF, Adobe Illustrator, EPS and PostScript.
//!
//! - PDF pages are drawn by `hayro`, a renderer written in Rust (MIT or
//!   Apache-2.0), at whatever scale is asked for, so a viewer can draw
//!   again as it zooms and text stays sharp.  The same interpreter reads
//!   a page's text with each glyph's place, for selecting and copying it
//!   ([`PageText`]).  PDFs from the internet are
//!   hostile: they nest, loop, inflate and claim enormous pictures, and no
//!   renderer bounds all of it, so `hayro` runs in a child process with a
//!   time and memory limit (`slerp-isolate`), where a crash, a hang or a
//!   runaway allocation costs one picture.  Files that need a password are
//!   refused with a message; those with only an owner password (printing
//!   or copying restrictions) open.
//! - Illustrator files saved with "Create PDF Compatible File", the default
//!   since Illustrator 9, are PDFs whose pages are the artboards.  Older
//!   ones are PostScript and go the EPS way.
//! - EPS files show the preview picture they store: a TIFF or a Windows
//!   Metafile behind a DOS EPS binary header, or an EPSI bitmap in the
//!   PostScript comments.  The metafile is returned for the caller to draw,
//!   since loaders don't depend on each other.  Without a preview, and for
//!   PostScript documents, the system's Ghostscript draws the page when it
//!   is installed: as a separate program, never linked or bundled (it is
//!   AGPL), in safe mode, with a time limit.
//!
//! Everything read from a file is bounded: page counts, picture sizes, the
//! lengths of header sections, the time and memory a page may take.

mod eps;
mod ghostscript;
mod page_text;
mod pdf;
mod stored;
mod text;

#[cfg(test)]
mod tests;

pub use eps::{BinaryHeader, DRAWN, Dsc, EpsiHeader, STORED};
pub use ghostscript::{ghostscript, ghostscript_version};
pub use page_text::{Direction, MAX_GLYPHS, PageText};
pub use pdf::{DrawView, JOBS, PdfPages};

use slerp_scene::textures::{ColorSpace, Texture};
use slerp_scene::{Limits, LoadError};
use std::path::Path;
use std::time::Duration;

/// The documents this crate reads, as users know them.  Which language a
/// file's drawing is written in (PDF or PostScript) is told by its content:
/// an Illustrator file may be either.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DocumentFormat {
    Pdf,
    /// Adobe Illustrator: a PDF when saved with PDF compatibility, else
    /// PostScript.
    Illustrator,
    /// Encapsulated PostScript, with or without a preview picture.
    Eps,
    /// A PostScript document of one or more pages.
    PostScript,
}

impl DocumentFormat {
    pub fn name(self) -> &'static str {
        match self {
            DocumentFormat::Pdf => "PDF document",
            DocumentFormat::Illustrator => "Adobe Illustrator drawing",
            DocumentFormat::Eps => "Encapsulated PostScript",
            DocumentFormat::PostScript => "PostScript document",
        }
    }

    /// What one page is called: Illustrator's pages are its artboards.
    pub fn page_word(self) -> &'static str {
        match self {
            DocumentFormat::Illustrator => "Artboard",
            _ => "Page",
        }
    }
}

/*┌─────────────────╮
| Limits          |
└─────────────────╯*/
//region Limits

/// The most pages a document lists; beyond this they are left out, with a
/// warning.  The page tree is built in the child, where time and memory are
/// bounded too.
pub const MAX_PAGES: usize = 100_000;
/// The largest picture a page is drawn into, per side and in all.
pub const MAX_SIDE: u32 = 8192;
pub const MAX_PIXELS: u64 = 48 << 20;
/// Time and memory to open a document and draw its first page for a
/// thumbnail: file managers give a thumbnailer two seconds or so.
const THUMBNAIL_TIME: Duration = Duration::from_millis(1500);
const THUMBNAIL_MEMORY: u64 = 1 << 30;
/// Time to open a document in the viewer, and to draw one picture of a
/// page there; the viewer draws off its window's thread and keeps showing
/// the last picture meanwhile.
pub const OPEN_TIME: Duration = Duration::from_secs(20);
pub const DRAW_TIME: Duration = Duration::from_secs(30);
const VIEWER_MEMORY: u64 = 3 << 30;
/// The long side of the picture `load` draws for something other than a
/// thumbnail (`slerp info`, `slerp render`), in pixels.
const DEFAULT_SIDE: u32 = 2048;

//endregion

/*┌─────────────────╮
| Recognising     |
└─────────────────╯*/
//region Recognising

/// Whether a file starts like a PDF: `%PDF-` within its first kilobyte,
/// as readers accept (some writers put junk before it).
pub fn is_pdf(head: &[u8]) -> bool {
    head[..head.len().min(1024)].windows(5).any(|w| w == b"%PDF-")
}

/// Whether a file starts like PostScript: `%!` (sometimes after a ^D), or
/// a DOS EPS binary header.
pub fn is_postscript(head: &[u8]) -> bool {
    head.starts_with(b"%!") || head.starts_with(b"\x04%!") || head.starts_with(&eps::DOS_MAGIC)
}

/// Recognises a document from its first bytes and name.  PDF and the DOS
/// EPS header are told by content alone; PostScript by content, with the
/// name telling an EPS from a document of pages (an EPS says `EPSF` in its
/// first line too).
pub fn sniff(path: &Path, head: &[u8]) -> Option<DocumentFormat> {
    let ext = path.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase).unwrap_or_default();
    if ext == "ai" && (is_pdf(head) || is_postscript(head)) {
        return Some(DocumentFormat::Illustrator);
    }
    if head.starts_with(&eps::DOS_MAGIC) {
        return Some(DocumentFormat::Eps);
    }
    if is_pdf(head) {
        return Some(DocumentFormat::Pdf);
    }
    if is_postscript(head) {
        let first = head.split(|&b| b == b'\n' || b == b'\r').next().unwrap_or(&[]);
        let says_eps = first.windows(4).any(|w| w == b"EPSF");
        return Some(match ext.as_str() {
            "eps" | "epsi" | "epsf" | "ept" => DocumentFormat::Eps,
            "ps" => DocumentFormat::PostScript,
            _ if says_eps => DocumentFormat::Eps,
            // Only PostScript that says it is a conforming document: plain
            // text files start with `%!` too (shell scripts don't, but
            // configuration snippets might).
            _ if first.starts_with(b"%!PS-Adobe") || first.starts_with(b"\x04%!PS-Adobe") => DocumentFormat::PostScript,
            _ => return None,
        });
    }
    None
}

fn head_of(path: &Path) -> std::io::Result<Vec<u8>> {
    use std::io::Read;
    let mut head = Vec::with_capacity(1024);
    std::fs::File::open(path)?.take(1024).read_to_end(&mut head)?;
    Ok(head)
}

/// Whether the file's drawing is PDF (so its pages can be drawn at any
/// scale), rather than PostScript.
pub fn is_pdf_file(path: &Path) -> bool {
    head_of(path).is_ok_and(|h| is_pdf(&h))
}

//endregion

/*┌─────────────────╮
| Pictures        |
└─────────────────╯*/
//region Pictures

/// A picture of a document's page, or the Windows Metafile an EPS stores
/// as its preview, which the caller draws (with the facts to attach).
pub enum Picture {
    Image(Texture),
    Metafile { bytes: Vec<u8>, facts: Vec<(String, String)> },
}

/// Reads `path` and draws its first page: fitted into the limits' texture
/// size for a thumbnail, else at a size that shows it well.
pub fn load_path(format: DocumentFormat, path: &Path, limits: &Limits) -> Result<Picture, LoadError> {
    load_page_index(format, path, limits, 0)
}

/// The names of a document's parts, when it has more than one: its pages
/// ("Page 1", "Artboard 2"), or for an EPS with a stored preview, the
/// preview and Ghostscript's drawing when Ghostscript is installed.  Empty
/// for single pictures and unreadable files.
pub fn page_names(format: DocumentFormat, path: &Path) -> Vec<String> {
    let count = match head_of(path) {
        Ok(h) if is_pdf(&h) => pdf::page_count(path).unwrap_or(0),
        Ok(h) if is_postscript(&h) && format == DocumentFormat::PostScript => {
            eps::read_dsc(path).ok().and_then(|d| d.pages).unwrap_or(0)
        }
        Ok(h) if is_postscript(&h) => return eps::choices(path),
        _ => 0,
    };
    if count < 2 {
        return Vec::new();
    }
    (1..=count.min(MAX_PAGES)).map(|i| format!("{} {i}", format.page_word())).collect()
}

/// Draws the part called `name`, one that [`page_names`] lists.
pub fn load_page(format: DocumentFormat, path: &Path, limits: &Limits, name: &str) -> Result<Picture, LoadError> {
    let index = match name {
        eps::STORED => Some(0),
        eps::DRAWN => Some(1),
        _ => page_index(format, name),
    };
    let index = index.ok_or_else(|| LoadError::Unsupported(format!("no page called {name}")))?;
    load_page_index(format, path, limits, index)
}

/// The index of a page from its name, "Page 3" or "Artboard 3" (or just
/// "3").
pub fn page_index(format: DocumentFormat, name: &str) -> Option<usize> {
    let number = name.trim().strip_prefix(format.page_word()).unwrap_or(name).trim();
    number.parse::<usize>().ok().filter(|&n| n >= 1).map(|n| n - 1)
}

fn load_page_index(format: DocumentFormat, path: &Path, limits: &Limits, index: usize) -> Result<Picture, LoadError> {
    let head = head_of(path)?;
    let side = limits.max_texture_size;
    if is_pdf(&head) {
        let (time, memory) = match side {
            Some(_) => (THUMBNAIL_TIME, THUMBNAIL_MEMORY),
            None => (OPEN_TIME, VIEWER_MEMORY),
        };
        let fit = side.unwrap_or(DEFAULT_SIDE);
        return pdf::picture(format, path, index, fit, time, memory).map(Picture::Image);
    }
    if is_postscript(&head) {
        return eps::picture(format, path, index, side);
    }
    Err(LoadError::invalid("neither PDF nor PostScript"))
}

/// The picture the file's program saved of it, read without drawing: the
/// XMP metadata's thumbnail (Illustrator saves one in nearly every file),
/// else the first page's own `/Thumb` (Acrobat's "embed page thumbnails"),
/// for PDF and Illustrator files; the stored preview (TIFF, metafile, EPSI
/// or XMP) for EPS.  `None` without one, and for PostScript documents.
/// Slerp's core shows it as the file's thumbnail in place of a drawing;
/// see `slerp_core::saved_picture`.
pub fn saved_picture(format: DocumentFormat, path: &Path) -> Result<Option<Picture>, LoadError> {
    let head = head_of(path)?;
    if is_pdf(&head) {
        if let Some(t) = xmp_picture(format, path) {
            return Ok(Some(Picture::Image(t)));
        }
        return Ok(pdf::page_thumbnail(format, path)?.map(Picture::Image));
    }
    if is_postscript(&head) {
        return eps::stored(format, path);
    }
    Ok(None)
}

/// The thumbnail in the file's XMP metadata, as a texture.  It is looked
/// for first, since finding it needs no renderer; a PDF page's own `/Thumb`
/// is looked for next, in the renderer's child.
pub(crate) fn xmp_picture(format: DocumentFormat, path: &Path) -> Option<Texture> {
    let (w, h, rgba) = stored::xmp_thumbnail(path)?;
    let note = ("Shown".to_string(), "The thumbnail stored in the file's metadata (XMP)".to_string());
    let container = match format {
        DocumentFormat::Illustrator => "Adobe Illustrator",
        DocumentFormat::Pdf => "PDF",
        _ => "EPS",
    };
    Some(texture(w, h, rgba, container, vec![note]))
}

/// A texture from straight-alpha RGBA bytes, with the facts to show.
fn texture(width: u32, height: u32, rgba: Vec<u8>, container: &str, facts: Vec<(String, String)>) -> Texture {
    let mut t = Texture::from_rgba8(width, height, rgba, ColorSpace::Srgb);
    t.source.container = container.to_string();
    t.source.header = facts;
    t
}

/// A page size in points as people say it: "210 × 297 mm (A4)", or in
/// inches for the American sizes.
pub fn describe_size(width: f64, height: f64) -> String {
    const PAPER: &[(&str, f64, f64)] = &[
        ("A3", 841.89, 1190.55),
        ("A4", 595.28, 841.89),
        ("A5", 419.53, 595.28),
        ("B5", 498.9, 708.66),
        ("Letter", 612.0, 792.0),
        ("Legal", 612.0, 1008.0),
        ("Tabloid", 792.0, 1224.0),
    ];
    let (short, long) = (width.min(height), width.max(height));
    let paper = PAPER.iter().find(|(_, w, h)| (short - w).abs() < 2.0 && (long - h).abs() < 2.0);
    let landscape = if width > height + 1.0 { ", landscape" } else { "" };
    match paper {
        Some((name @ ("Letter" | "Legal" | "Tabloid"), ..)) => {
            format!("{:.1} × {:.1} in ({name}{landscape})", width / 72.0, height / 72.0)
        }
        Some((name, ..)) => format!("{:.0} × {:.0} mm ({name}{landscape})", width / 72.0 * 25.4, height / 72.0 * 25.4),
        None => {
            format!("{:.0} × {:.0} pt ({:.0} × {:.0} mm)", width, height, width / 72.0 * 25.4, height / 72.0 * 25.4)
        }
    }
}

//endregion
