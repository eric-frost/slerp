//! Documents of pages that the viewer draws again at every zoom, so text
//! and lines stay sharp however far one goes in: PDF and PDF-compatible
//! Illustrator files (through `slerp-documents`), and any other vector
//! format that implements [`Pages`].  A file with several pages lists them
//! as its parts ("Page 1", "Artboard 2").  PDF pages also give their text
//! with each glyph's place, which the viewer lets people select and copy.

use crate::Kind;
use slerp_documents::{DocumentFormat, DrawView, PageText, PdfPages};
use slerp_scene::Image8;
use std::path::Path;
use std::sync::{Arc, Mutex};

/// A document whose pages can be drawn at any scale.  Sizes and regions are
/// in the file's own units (PDF points, SVG pixels), measured from the
/// page's top left with y going down.
pub trait Pages: Send + Sync {
    fn count(&self) -> usize;

    /// The size of page `page`, as shown (a PDF page's rotation applied).
    fn size(&self, page: usize) -> (f64, f64);

    /// Draws `region` (`[x0, y0, x1, y1]`) of page `page` into a picture of
    /// `width` by `height` pixels, as straight-alpha RGBA.  Pixels past the
    /// page's edges are transparent; what the page itself shows is the
    /// format's (PDF draws white paper).  This may take a while: the viewer
    /// calls it on a thread of its own.
    fn draw(&self, page: usize, region: [f64; 4], width: u32, height: u32) -> Result<Image8, String>;

    /// The text on page `page`, each glyph placed in the units of
    /// [`Pages::size`], for selecting and copying.  Empty for pages without
    /// text (a scan, lettering turned into outlines) and for formats that
    /// don't give theirs.  Like drawing, this may take a while.
    fn text(&self, _page: usize) -> Result<PageText, String> {
        Ok(PageText::default())
    }

    /// Facts for the inspector: title, author, page size and the like.
    fn facts(&self) -> Vec<(String, String)> {
        Vec::new()
    }

    fn warnings(&self) -> Vec<String> {
        Vec::new()
    }

    /// What a page is called: "Page", or "Artboard" for Illustrator.
    fn page_word(&self) -> &'static str {
        "Page"
    }

    /// The file's units per inch: 72 for PDF points, 96 for SVG pixels.
    /// The viewer's 100 % shows an inch as 96 screen pixels.
    fn units_per_inch(&self) -> f64 {
        72.0
    }
}

/// Opens `path` as pages when its format is one that draws again at every
/// zoom; `None` for every other file.
pub fn open(kind: Kind, path: &Path) -> Option<Result<Arc<dyn Pages>, String>> {
    match kind {
        Kind::Document(f) if slerp_documents::is_pdf_file(path) => {
            Some(PdfDocument::open(f, path).map(|d| Arc::new(d) as Arc<dyn Pages>))
        }
        Kind::Svg => Some(
            slerp_svg::Drawing::open(path, &slerp_scene::Limits::viewer())
                .map(|d| Arc::new(SvgDrawing(d)) as Arc<dyn Pages>),
        ),
        _ => None,
    }
}

/// A PDF kept open in its renderer's child process.  Drawing is one page
/// at a time, so the renderer sits behind a lock.
struct PdfDocument {
    format: DocumentFormat,
    sizes: Vec<(f32, f32)>,
    facts: Vec<(String, String)>,
    warnings: Vec<String>,
    pages: Mutex<PdfPages>,
}

impl PdfDocument {
    fn open(format: DocumentFormat, path: &Path) -> Result<PdfDocument, String> {
        let pages = PdfPages::open(format, path).map_err(|e| e.to_string())?;
        Ok(PdfDocument {
            format,
            sizes: pages.sizes.clone(),
            facts: pages.facts.clone(),
            warnings: pages.warnings.clone(),
            pages: Mutex::new(pages),
        })
    }
}

impl Pages for PdfDocument {
    fn count(&self) -> usize {
        self.sizes.len()
    }

    fn size(&self, page: usize) -> (f64, f64) {
        self.sizes.get(page).map_or((612.0, 792.0), |&(w, h)| (w as f64, h as f64))
    }

    fn draw(&self, page: usize, region: [f64; 4], width: u32, height: u32) -> Result<Image8, String> {
        let scale = width as f64 / (region[2] - region[0]);
        let view = DrawView { page, left: region[0], top: region[1], scale, width, height };
        self.pages.lock().map_err(|_| "the document is unavailable".to_string())?.draw(&view)
    }

    fn text(&self, page: usize) -> Result<PageText, String> {
        self.pages.lock().map_err(|_| "the document is unavailable".to_string())?.text(page)
    }

    fn facts(&self) -> Vec<(String, String)> {
        self.facts.clone()
    }

    fn warnings(&self) -> Vec<String> {
        let mut w = self.warnings.clone();
        if let Ok(mut p) = self.pages.lock() {
            w.extend(p.draw_warnings());
        }
        w
    }

    fn page_word(&self) -> &'static str {
        self.format.page_word()
    }
}

/// An SVG drawing: one page in CSS pixels, kept parsed in its drawing child.
struct SvgDrawing(slerp_svg::Drawing);

impl Pages for SvgDrawing {
    fn count(&self) -> usize {
        1
    }

    fn size(&self, _page: usize) -> (f64, f64) {
        (self.0.width, self.0.height)
    }

    fn draw(&self, _page: usize, region: [f64; 4], width: u32, height: u32) -> Result<Image8, String> {
        self.0.draw(region, width, height)
    }

    fn facts(&self) -> Vec<(String, String)> {
        self.0.facts.clone()
    }

    fn warnings(&self) -> Vec<String> {
        self.0.warnings.clone()
    }

    fn page_word(&self) -> &'static str {
        "Drawing"
    }

    fn units_per_inch(&self) -> f64 {
        96.0
    }
}
