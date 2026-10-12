//! Pages of documents that draw again at every zoom (`slerp_core::pages`:
//! PDF and Illustrator files, and other vector formats): shown on grey like
//! a picture, with pan and zoom, but drawn again at every zoom, so text and
//! lines stay sharp however far one goes in.  A picture of the part of the
//! page in view (and a margin around it) is drawn in the renderer's child
//! process on a thread, then put on the screen as a texture; while a new
//! one is being drawn, the last stays on screen, moved and scaled to
//! match.  The pages are the file's parts: a picker in the panel, Page Up
//! and Page Down and the arrow keys step through them.
//!
//! Text on a page can be selected and copied as in a PDF reader.  The
//! page's text, with each glyph's place, is read in the renderer's child
//! the first time the pointer is over the page (or a copy asks for it), on
//! a thread.  Over text the pointer shows the text cursor; a drag that
//! starts on text selects the glyphs between the press and the pointer in
//! the order the page draws them, which is reading order for nearly every
//! document, and one that starts on paper pans.  A double-click on a word
//! selects it, Ctrl+A the page, Ctrl+C or the right-click menu copies, a
//! click or Esc clears, and turning the page drops the selection.  Pages
//! without text (scans, lettering turned into outlines) never show the
//! text cursor.
//!
//! EPS and PostScript files are pictures (their stored preview, or
//! Ghostscript's drawing at a fixed size) and use the texture view.

use crate::gfx::Gfx;
use crate::loader::{Task, Waker, poll, spawn};
use crate::texture_view::TextureView;
use egui::{CursorIcon, Key, Modifiers, PointerButton, Ui};
use glam::DVec2;
use slerp_core::documents::PageText;
use slerp_core::pages::Pages;
use slerp_scene::{ColorSpace, Image8, Texture};
use slerp_theme::widgets;
use std::ops::Range;
use std::sync::Arc;

/// How far past the window a picture reaches on each side, in pixels, so
/// panning shows page while the next picture is drawn.
const MARGIN: u32 = 128;
/// The picture drawn while the loader opens a document, before the window's
/// size is known: its long side.
const FIRST_SIDE: u32 = 1600;
/// How far out one may zoom, relative to the page fitting, and how far in,
/// relative to the actual size (6400 %).
const MIN_ZOOM: f64 = 0.05;
const MAX_ZOOM: f64 = 64.0;
/// The grey around pages, as displayed.
const BACKGROUND: [f32; 3] = [0.46, 0.46, 0.47];
/// How much of the theme's selection colour a text highlight lets through
/// over the page, so the selected words stay readable.
const HIGHLIGHT_OPACITY: f32 = 0.4;

/// What to draw: a part of a page at a scale.  `left` and `top` are the
/// page point (in the file's units, from the page's top left) at the
/// picture's top-left corner; `scale` is pixels per unit.
#[derive(Clone, Copy, Debug, PartialEq)]
struct View {
    page: usize,
    left: f64,
    top: f64,
    scale: f64,
    width: u32,
    height: u32,
}

impl View {
    /// The whole of page `page` fitted into `fit` pixels on its longer side.
    fn fit(page: usize, (w, h): (f64, f64), fit: u32) -> View {
        let (w, h) = (w.max(1e-3), h.max(1e-3));
        let scale = fit as f64 / w.max(h);
        let max = slerp_core::documents::MAX_SIDE;
        let width = ((w * scale).round() as u32).clamp(1, max);
        let height = ((h * scale).round() as u32).clamp(1, max);
        View { page, left: 0.0, top: 0.0, scale, width, height }
    }

    fn draw(&self, pages: &dyn Pages) -> Result<Image8, String> {
        let region = [
            self.left,
            self.top,
            self.left + self.width as f64 / self.scale,
            self.top + self.height as f64 / self.scale,
        ];
        pages.draw(self.page, region, self.width, self.height)
    }
}

/// A document opened on the loader's thread, on page `page`; the loader's
/// asset is the picture of that page.
pub struct Opened {
    pub pages: Arc<dyn Pages>,
    pub page: usize,
    first_view: View,
}

impl Opened {
    /// The pages' names when there are several: "Page 1", "Artboard 2".
    pub fn names(&self) -> Vec<String> {
        page_names(&*self.pages)
    }
}

fn page_names(pages: &dyn Pages) -> Vec<String> {
    match pages.count() {
        0 | 1 => Vec::new(),
        n => (1..=n).map(|i| format!("{} {i}", pages.page_word())).collect(),
    }
}

/// Starts showing `pages` on `part` (a page name) or the first page, and
/// draws that page to show at once.
pub fn open(pages: Arc<dyn Pages>, part: Option<&str>) -> Result<(Opened, Image8), String> {
    if pages.count() == 0 {
        return Err("the document has no pages".into());
    }
    let page = part.and_then(|name| page_names(&*pages).iter().position(|n| n == name)).unwrap_or(0);
    let first_view = View::fit(page, pages.size(page), FIRST_SIDE);
    let first = first_view.draw(&*pages)?;
    Ok((Opened { pages, page, first_view }, first))
}

struct Rendered {
    view: View,
    texture: TextureView,
}

/// What waits for the shown page's text to arrive.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Then {
    Nothing,
    SelectAll,
    CopyPage,
}

pub struct PagesView {
    pages: Arc<dyn Pages>,
    /// The page shown, from 0.
    pub page: usize,
    /// The page point in the middle of the view (in the file's units), and
    /// physical pixels per unit.
    center: DVec2,
    scale: f64,
    /// Keeps the whole page in view until the user pans or zooms; `width`
    /// fits the page's width instead.
    fit: bool,
    fit_width: bool,
    image: Option<Rendered>,
    pending: Option<(View, Task<Image8>)>,
    /// A page that couldn't be drawn, and why; it isn't tried again until
    /// the document is reopened.
    pub error: Option<(usize, String)>,
    waker: Waker,
    name: String,
    /// The view's size and the screen's scale in the last frame.
    size: (u32, u32),
    ppp: f32,
    /// The text of the page last read, and the reading under way, each
    /// with its page.  Only the shown page's text is kept.
    text: Option<(usize, Arc<PageText>)>,
    reading: Option<(usize, Task<PageText>)>,
    then: Then,
    /// The selected text on the shown page, as two carets (places between
    /// glyphs, see `PageText`): where the selection started and where it
    /// ends now.
    selection: Option<(usize, usize)>,
    /// A drag that started on text, which selects rather than pans.
    selecting: bool,
    /// How the last copy went, shown under the page controls.
    note: Option<String>,
}

impl PagesView {
    pub fn new(
        name: String,
        opened: Opened,
        first: Arc<Texture>,
        gfx: &mut Gfx,
        waker: Waker,
    ) -> Result<PagesView, String> {
        let Opened { pages, page, first_view } = opened;
        let texture = TextureView::new(name.clone(), first, gfx)?;
        let (w, h) = (first_view.width, first_view.height);
        let mut view = PagesView {
            pages,
            page,
            center: DVec2::ZERO,
            scale: first_view.scale,
            fit: true,
            fit_width: false,
            image: Some(Rendered { view: first_view, texture }),
            pending: None,
            error: None,
            waker,
            name,
            size: (w, h),
            ppp: 1.0,
            text: None,
            reading: None,
            then: Then::Nothing,
            selection: None,
            selecting: false,
            note: None,
        };
        view.fit_now();
        Ok(view)
    }

    /// Keeps the zoom and position after the file reloaded.
    pub fn carry_over(&mut self, old: &PagesView) {
        if self.page != old.page {
            return;
        }
        (self.center, self.scale, self.fit, self.fit_width) = (old.center, old.scale, old.fit, old.fit_width);
    }

    /// The document's facts and warnings, for the inspector.
    pub fn facts(&self) -> (Vec<(String, String)>, Vec<String>) {
        (self.pages.facts(), self.pages.warnings())
    }

    pub fn count(&self) -> usize {
        self.pages.count()
    }

    /// The shown page's name, which reloading opens again.
    pub fn part_name(&self) -> String {
        format!("{} {}", self.pages.page_word(), self.page + 1)
    }

    pub fn summary(&self) -> String {
        let word = self.pages.page_word().to_lowercase();
        match self.count() {
            1 => format!("1 {word}"),
            n => format!("{} {} of {n}", word, self.page + 1),
        }
    }

    /// Shows page `page`, fitted.  Returns false when there is no such page.
    pub fn set_page(&mut self, page: usize) -> bool {
        if page >= self.count() {
            return false;
        }
        if page != self.page {
            self.page = page;
            // The old page's picture would be wrong; blank paper of the new
            // page's shape stands in until its own arrives.
            self.image = None;
            self.fit_width_or_page();
            self.clear_selection();
            self.then = Then::Nothing;
            self.note = None;
        }
        true
    }

    /// Steps `delta` pages; false at the first or last.
    pub fn step(&mut self, delta: isize) -> bool {
        let next = self.page as isize + delta;
        next >= 0 && self.set_page(next as usize)
    }

    fn page_size(&self) -> DVec2 {
        let (w, h) = self.pages.size(self.page);
        DVec2::new(w.max(1e-3), h.max(1e-3))
    }

    /// Physical pixels per unit at 100 %: an inch is 96 logical pixels, as
    /// other viewers show documents.
    fn actual_scale(&self) -> f64 {
        96.0 / self.pages.units_per_inch().max(1e-6) * self.ppp.max(0.1) as f64
    }

    /// The scale at which the whole page fits the view, with a little grey
    /// around it.
    fn fit_scale(&self) -> f64 {
        let p = self.page_size();
        (self.size.0 as f64 / p.x).min(self.size.1 as f64 / p.y) * 0.96
    }

    pub fn fit_now(&mut self) {
        self.center = self.page_size() / 2.0;
        self.scale = self.fit_scale();
        self.fit = true;
        self.fit_width = false;
    }

    /// The page's width across the view, its top in view.
    pub fn fit_width_now(&mut self) {
        let p = self.page_size();
        self.scale = (self.size.0 as f64 / p.x * 0.96).min(MAX_ZOOM * self.actual_scale());
        self.center = DVec2::new(p.x / 2.0, (self.size.1 as f64 / 2.0 / self.scale).min(p.y / 2.0));
        self.fit = false;
        self.fit_width = true;
    }

    fn fit_width_or_page(&mut self) {
        if self.fit_width { self.fit_width_now() } else { self.fit_now() }
    }

    /// 100 %.
    pub fn actual_size(&mut self) {
        self.scale = self.actual_scale();
        self.fit = false;
        self.fit_width = false;
    }

    /// The zoom as a percentage of the actual size.
    pub fn percent(&self) -> f64 {
        self.scale / self.actual_scale() * 100.0
    }

    /// Zooms by `factor` about `at`, a point in physical pixels from the
    /// view's top left, so the page point under it stays.
    pub fn zoom_about(&mut self, factor: f64, at: DVec2) {
        let before = self.to_page(at);
        let fit = self.fit_scale();
        self.scale = (self.scale * factor).clamp(fit * MIN_ZOOM, (MAX_ZOOM * self.actual_scale()).max(fit));
        let after = self.to_page(at);
        self.center += before - after;
        self.fit = false;
        self.fit_width = false;
    }

    pub fn zoom(&mut self, factor: f64) {
        self.zoom_about(factor, DVec2::new(self.size.0 as f64, self.size.1 as f64) / 2.0);
    }

    /// The page point under a point of the view (physical pixels).
    fn to_page(&self, at: DVec2) -> DVec2 {
        self.center + (at - DVec2::new(self.size.0 as f64, self.size.1 as f64) / 2.0) / self.scale
    }

    /// The picture wanted now: the window and a margin all round.
    fn wanted(&self) -> View {
        let m = (MARGIN as f32 * self.ppp) as u32;
        let max = slerp_core::documents::MAX_SIDE;
        let (w, h) = ((self.size.0 + 2 * m).min(max), (self.size.1 + 2 * m).min(max));
        let left = self.center.x - w as f64 / 2.0 / self.scale;
        let top = self.center.y - h as f64 / 2.0 / self.scale;
        View { page: self.page, left, top, scale: self.scale, width: w, height: h }
    }

    /// Whether a picture shows what the window does now, sharp.
    fn covers(&self, have: &View) -> bool {
        let half = DVec2::new(self.size.0 as f64, self.size.1 as f64) / 2.0 / self.scale;
        let (lo, hi) = (self.center - half, self.center + half);
        let (right, bottom) = (have.left + have.width as f64 / have.scale, have.top + have.height as f64 / have.scale);
        let eps = 0.5 / self.scale;
        have.page == self.page
            && (have.scale / self.scale - 1.0).abs() < 1e-6
            && have.left <= lo.x + eps
            && have.top <= lo.y + eps
            && right >= hi.x - eps
            && bottom >= hi.y - eps
    }

    /*┌─────────────────╮
    | Text            |
    └─────────────────╯*/
    //region Text

    /// The shown page's text, once it has been read.
    fn page_text(&self) -> Option<&Arc<PageText>> {
        self.text.as_ref().filter(|(p, _)| *p == self.page).map(|(_, t)| t)
    }

    /// Starts reading the shown page's text on a thread, unless it is here
    /// or on its way.
    fn want_text(&mut self) {
        let page = self.page;
        if self.page_text().is_some() || self.reading.as_ref().is_some_and(|(p, _)| *p == page) {
            return;
        }
        let pages = self.pages.clone();
        self.reading = Some((page, spawn(&self.waker, "slerp-page-text", move || pages.text(page))));
    }

    /// Takes in a page's text when it arrives, and does what waited for it.
    /// A page whose text can't be read counts as having none.
    fn poll_text(&mut self, ctx: &egui::Context) {
        let Some(result) = self.reading.as_ref().and_then(|(_, task)| poll(task)) else { return };
        let Some((page, _)) = self.reading.take() else { return };
        let failed = result.as_ref().err().cloned();
        self.text = Some((page, Arc::new(result.unwrap_or_default())));
        if page != self.page {
            return;
        }
        match std::mem::replace(&mut self.then, Then::Nothing) {
            Then::SelectAll => self.select_all(),
            Then::CopyPage if failed.is_some() => {
                self.note = failed.map(|e| format!("Slerp couldn't read this page's text: {e}"));
            }
            Then::CopyPage => self.copy_page(ctx),
            Then::Nothing => {}
        }
    }

    /// The selected glyphs, when any are.
    fn selected(&self) -> Option<Range<usize>> {
        let (a, b) = self.selection?;
        Some(a.min(b)..a.max(b)).filter(|r| !r.is_empty() && self.page_text().is_some())
    }

    pub fn has_selection(&self) -> bool {
        self.selected().is_some()
    }

    /// Drops the selection; returns whether there was one.
    pub fn clear_selection(&mut self) -> bool {
        self.selecting = false;
        self.selection.take().is_some()
    }

    /// Selects the page's text, reading it first if need be.
    pub fn select_all(&mut self) {
        match self.page_text() {
            Some(t) => self.selection = Some((0, t.len())),
            None => {
                self.then = Then::SelectAll;
                self.want_text();
            }
        }
    }

    /// Copies the selected text to the clipboard; false when nothing is
    /// selected.
    pub fn copy_selection(&mut self, ctx: &egui::Context) -> bool {
        let (Some(range), Some(t)) = (self.selected(), self.page_text()) else { return false };
        let text = t.copy_text(range);
        self.copy(ctx, text, false);
        true
    }

    /// Copies the whole page's text, reading it first if need be.
    pub fn copy_page(&mut self, ctx: &egui::Context) {
        match self.page_text() {
            Some(t) => {
                let (text, truncated) = (t.all_text(), t.truncated());
                self.copy(ctx, text, truncated);
            }
            None => {
                self.then = Then::CopyPage;
                self.note = Some("Reading the page's text…".into());
                self.want_text();
            }
        }
    }

    fn copy(&mut self, ctx: &egui::Context, text: String, truncated: bool) {
        let word = self.pages.page_word().to_lowercase();
        if text.trim().is_empty() {
            self.note = Some(format!(
                "This {word} has no text to copy: it may be a scan, or its lettering may be drawn as shapes."
            ));
            return;
        }
        let n = text.chars().count();
        ctx.copy_text(text);
        let cut = if truncated { format!(", all Slerp reads of a very long {word}") } else { String::new() };
        self.note = Some(format!("Copied {n} characters{cut}."));
    }

    /// The page point under a point of the window (in points), given the
    /// view's rectangle on the window.
    fn page_point(&self, rect: egui::Rect, p: egui::Pos2) -> [f32; 2] {
        let at = (p - rect.min) * self.ppp;
        let q = self.to_page(DVec2::new(at.x as f64, at.y as f64));
        [q.x as f32, q.y as f32]
    }

    /// Where a page rectangle (left, top, right, bottom) is on the window,
    /// in points: the page's drawing transform, so a highlight stays on its
    /// words while zooming and panning.
    fn screen_rect(&self, rect: egui::Rect, r: [f32; 4]) -> egui::Rect {
        let half = DVec2::new(self.size.0 as f64, self.size.1 as f64) / 2.0;
        let at = |x: f32, y: f32| {
            let v = (DVec2::new(x as f64, y as f64) - self.center) * self.scale + half;
            rect.min + egui::vec2(v.x as f32, v.y as f32) / self.ppp.max(0.1)
        };
        egui::Rect::from_two_pos(at(r[0], r[1]), at(r[2], r[3]))
    }

    /// The highlight's rectangles on the window.
    fn highlight(&self, rect: egui::Rect) -> Vec<egui::Rect> {
        let (Some(range), Some(t)) = (self.selected(), self.page_text()) else { return Vec::new() };
        t.selection_rects(range).into_iter().map(|r| self.screen_rect(rect, r)).collect()
    }

    //endregion

    /// Starts and finishes pictures: the one wanted is drawn on a thread,
    /// and when it arrives it goes up to the GPU.
    fn update(&mut self, gfx: &mut Gfx) {
        if let Some((view, task)) = &self.pending
            && let Some(result) = poll(task)
        {
            let view = *view;
            self.pending = None;
            match result {
                Ok(img) => match TextureView::new(self.name.clone(), image_texture(img), gfx) {
                    Ok(texture) if view.page == self.page => self.image = Some(Rendered { view, texture }),
                    Ok(_) => {}
                    Err(e) => self.error = Some((view.page, e)),
                },
                Err(e) => self.error = Some((view.page, e)),
            }
        }
        let failed = self.error.as_ref().is_some_and(|(p, _)| *p == self.page);
        let current = self.image.as_ref().is_some_and(|i| self.covers(&i.view));
        if self.pending.is_none() && !current && !failed && self.size.0 > 0 && self.size.1 > 0 {
            let view = self.wanted();
            let pages = self.pages.clone();
            let task = spawn(&self.waker, "slerp-page-draw", move || view.draw(&*pages));
            self.pending = Some((view, task));
        }
        // Blank paper while the first picture of a page is drawn.
        if self.image.is_none() {
            let p = self.page_size();
            let k = 16.0 / p.x.max(p.y);
            let (w, h) = (((p.x * k).ceil() as u32).max(1), ((p.y * k).ceil() as u32).max(1));
            let paper = Image8 { width: w, height: h, pixels: vec![255; (w * h * 4) as usize] };
            if let Ok(texture) = TextureView::new(self.name.clone(), image_texture(paper), gfx) {
                let view = View { page: self.page, left: 0.0, top: 0.0, scale: k, width: w, height: h };
                self.image = Some(Rendered { view, texture });
            }
        }
    }

    /// Puts the picture on the target, moved and scaled to the current view.
    fn draw(&mut self, gfx: &mut Gfx) {
        let (center, scale, size) = (self.center, self.scale, self.size);
        let Some(img) = &mut self.image else { return };
        let k = img.view;
        let t = &mut img.texture;
        t.fit = false;
        t.set_size(size);
        // The texel at the middle of the window.
        t.params.center = [((center.x - k.left) * k.scale) as f32, ((center.y - k.top) * k.scale) as f32];
        t.params.zoom = (scale / k.scale) as f32;
        t.params.checkerboard = false;
        t.params.background = BACKGROUND;
        t.draw(gfx);
    }

    /// Words for the overlay in the view's corner.
    pub fn overlay_parts(&self) -> Vec<String> {
        let mut parts = Vec::new();
        if self.count() > 1 {
            parts.push(format!("{} {} of {}", self.pages.page_word(), self.page + 1, self.count()));
        }
        parts.push(format!("{:.0} %", self.percent()));
        parts
    }
}

pub fn image_texture(img: Image8) -> Arc<Texture> {
    Arc::new(Texture::from_rgba8(img.width, img.height, img.pixels, ColorSpace::Srgb))
}

/// Input, drawing and the status text for a page.  Returns whether it
/// needs another frame.
#[allow(clippy::too_many_arguments)]
pub fn drive(
    c: &mut PagesView,
    gfx: &mut Gfx,
    drag: Option<(PointerButton, glam::Vec2)>,
    pointer: Option<egui::Pos2>,
    scroll: f32,
    zoom: f32,
    response: &egui::Response,
    rect: egui::Rect,
    ppp: f32,
    size: (u32, u32),
    hover: &mut Option<String>,
) -> bool {
    let ctx = response.ctx.clone();
    c.ppp = ppp;
    let resized = size != c.size;
    c.size = size;
    if resized && c.fit {
        c.fit_now();
    } else if resized && c.fit_width {
        c.fit_width_now();
    }
    c.poll_text(&ctx);
    // 1. The page's text is read the first time the pointer is over the
    //    page; then the text cursor shows over text.
    let s = c.page_size();
    let on_page = |q: [f32; 2]| q[0] >= 0.0 && q[1] >= 0.0 && q[0] as f64 <= s.x && q[1] as f64 <= s.y;
    let over = pointer.filter(|_| response.hovered()).map(|p| c.page_point(rect, p)).filter(|&q| on_page(q));
    if over.is_some() {
        c.want_text();
    }
    let over_text = over.is_some_and(|q| c.page_text().is_some_and(|t| t.on_text(q)));
    // 2. A drag that starts on text selects from where it started to the
    //    pointer; anywhere else it pans.
    if response.drag_started_by(PointerButton::Primary)
        && let Some(origin) = ctx.input(|i| i.pointer.press_origin())
    {
        let q = c.page_point(rect, origin);
        let anchor = c.page_text().filter(|t| on_page(q) && t.on_text(q)).and_then(|t| t.caret_at(q));
        if let Some(a) = anchor {
            c.selection = Some((a, a));
            c.selecting = true;
        }
    }
    if c.selecting {
        let at = pointer.or_else(|| response.interact_pointer_pos());
        match (response.dragged_by(PointerButton::Primary), at) {
            (true, Some(p)) => {
                let q = c.page_point(rect, p);
                if let (Some((anchor, _)), Some(focus)) = (c.selection, c.page_text().and_then(|t| t.caret_at(q))) {
                    c.selection = Some((anchor, focus));
                }
            }
            (true, None) => {}
            (false, _) => c.selecting = false,
        }
    }
    if c.selecting || over_text {
        ctx.set_cursor_icon(CursorIcon::Text);
    }
    if let Some((_, d)) = drag.filter(|_| !c.selecting) {
        // Dragging moves the page with the pointer.
        c.center -= DVec2::new(d.x as f64, d.y as f64) * ppp as f64 / c.scale;
        c.fit = false;
        c.fit_width = false;
    }
    let at = |p: egui::Pos2| {
        let v = (p - rect.min) * ppp;
        DVec2::new(v.x as f64, v.y as f64)
    };
    if response.hovered()
        && let Some(p) = pointer
    {
        if scroll != 0.0 {
            c.zoom_about(1.25f64.powf(scroll as f64), at(p));
        }
        if zoom != 1.0 {
            c.zoom_about(zoom as f64, at(p));
        }
    }
    // 3. A click clears the selection.  A double-click on a word selects
    //    it, and elsewhere zooms in for a close look, or back out.
    if response.clicked() {
        c.clear_selection();
    }
    if response.double_clicked() {
        let word = over.and_then(|q| c.page_text()?.word_at(q));
        if let Some(w) = word {
            c.selection = Some((w.start, w.end));
        } else if c.fit {
            let m = pointer.map_or(DVec2::new(size.0 as f64 / 2.0, size.1 as f64 / 2.0), at);
            c.zoom_about(4.0, m);
        } else {
            c.fit_now();
        }
    }
    // 4. A right-click on the selection offers to copy it.
    let on_selection =
        || response.secondary_clicked() && pointer.is_some_and(|p| c.highlight(rect).iter().any(|r| r.contains(p)));
    if response.context_menu_opened() || on_selection() {
        response.context_menu(|ui| {
            if ui.add_enabled(c.has_selection(), egui::Button::new("Copy")).clicked() {
                c.copy_selection(&ctx);
                ui.close();
            }
        });
    }
    // 5. Keys the app didn't hand on before its own shortcuts.
    keys(c, &ctx, response.has_focus());
    if let Some(q) = over {
        let mm = 25.4 / c.pages.units_per_inch();
        *hover = Some(format!("{:.1}, {:.1} mm from the top left", q[0] as f64 * mm, q[1] as f64 * mm));
    }
    c.update(gfx);
    c.draw(gfx);
    c.pending.is_some()
}

/// Ctrl+A selects the page's text, and while text is selected, Ctrl+C
/// copies it and Esc clears it.  Only while the view has the keyboard
/// (`view_keys`: it has the focus, or nothing has), and never with Shift,
/// which Ctrl+Shift+C (copying the view state) uses.  The app calls this
/// before its own shortcuts, so a selection takes Ctrl+C from the Contents
/// panel and Esc from leaving full screen or closing the window.
pub fn keys(c: &mut PagesView, ctx: &egui::Context, view_keys: bool) {
    if !view_keys {
        return;
    }
    let selected = c.has_selection();
    let (all, copy, clear) = ctx.input_mut(|i| {
        if i.modifiers.shift {
            return (false, false, false);
        }
        let all = i.consume_key(Modifiers::COMMAND, Key::A);
        let mut copy = false;
        if selected {
            copy = i.consume_key(Modifiers::COMMAND, Key::C);
            // egui's window layer turns Ctrl+C into a copy event.
            let before = i.events.len();
            i.events.retain(|e| !matches!(e, egui::Event::Copy));
            copy |= i.events.len() != before;
        }
        let clear = selected && i.consume_key(Modifiers::NONE, Key::Escape);
        (all, copy, clear)
    });
    if all {
        c.select_all();
    }
    if copy {
        c.copy_selection(ctx);
    }
    if clear {
        c.clear_selection();
    }
}

/// The page for the window's screenshot command: the part in view, drawn
/// at the size asked for.
pub fn screenshot(c: &PagesView, width: u32, height: u32) -> Option<Image8> {
    let k = width as f64 / c.size.0.max(1) as f64;
    let scale = c.scale * k;
    let left = c.center.x - width as f64 / 2.0 / scale;
    let top = c.center.y - height as f64 / 2.0 / scale;
    let view = View { page: c.page, left, top, scale, width, height };
    let mut img = view.draw(&*c.pages).ok()?;
    // The grey around the page, as the window shows it.
    let g = BACKGROUND.map(|v| (v * 255.0 + 0.5) as u32);
    for p in img.pixels.chunks_exact_mut(4) {
        let a = p[3] as u32;
        for i in 0..3 {
            p[i] = ((p[i] as u32 * a + g[i] * (255 - a) + 127) / 255) as u8;
        }
        p[3] = 255;
    }
    Some(img)
}

/// What goes over the page: the selected text's highlight, in the theme's
/// selection colour, and a message when the page couldn't be drawn.
pub fn overlay(painter: &egui::Painter, rect: egui::Rect, c: &PagesView) {
    let fill = painter.ctx().global_style().visuals.selection.bg_fill.gamma_multiply(HIGHLIGHT_OPACITY);
    for r in c.highlight(rect) {
        painter.rect_filled(r, 0.0, fill);
    }
    let Some((page, why)) = &c.error else { return };
    if *page != c.page {
        return;
    }
    let text = format!("Slerp couldn't draw {} {}: {why}", c.pages.page_word().to_lowercase(), page + 1);
    // Light text on the grey around pages, as the volume view's labels.
    let color = egui::Color32::from_rgb(255, 220, 120);
    painter.text(rect.center(), egui::Align2::CENTER_CENTER, text, egui::FontId::proportional(15.0), color);
}

/// The right-hand panel: which page, and how it fits.  Returns whether the
/// page changed.
pub fn controls(ui: &mut Ui, c: &mut PagesView) -> bool {
    let mut changed = false;
    let word = c.pages.page_word();
    let title = format!("{word}s");
    crate::sections::section(ui, "Pages", title, true, |ui| {
        let n = c.count();
        let mut page = c.page;
        if n > 1 {
            if n <= 300 {
                let items = (0..n).map(|i| (i, format!("{word} {} of {n}", i + 1)));
                widgets::Choice::new("pdf-page", &mut page, items).width(200.0).show(ui);
            } else {
                crate::slice_view::counter_slider(ui, &format!("{word} "), &mut page, n);
            }
            ui.horizontal(|ui| {
                if ui.add_enabled(c.page > 0, egui::Button::new("Previous")).on_hover_text("Page Up").clicked() {
                    page = c.page - 1;
                }
                if ui.add_enabled(c.page + 1 < n, egui::Button::new("Next")).on_hover_text("Page Down").clicked() {
                    page = c.page + 1;
                }
            });
            if page != c.page {
                changed = c.set_page(page);
            }
        }
        // PDF page sizes in paper's words; other units as they are.
        let (w, h) = c.pages.size(c.page);
        let inch = c.pages.units_per_inch();
        ui.label(slerp_core::documents::describe_size(w / inch * 72.0, h / inch * 72.0));
        ui.horizontal(|ui| {
            if ui.button("Fit Page").on_hover_text("Home or F").clicked() {
                c.fit_now();
            }
            if ui.button("Fit Width").clicked() {
                c.fit_width_now();
            }
            if ui.button("100 %").on_hover_text("Ctrl+0").clicked() {
                c.actual_size();
            }
        });
        let copy = ui.button(format!("Copy {word} Text"));
        if copy.on_hover_text(format!("Copies the {}'s text without selecting it", word.to_lowercase())).clicked() {
            c.copy_page(ui.ctx());
        }
        if let Some(note) = &c.note {
            ui.label(egui::RichText::new(note).weak().small());
        }
        if let Some((p, why)) = &c.error
            && *p == c.page
        {
            ui.label(egui::RichText::new(why).color(ui.visuals().warn_fg_color));
        }
        let help = "Drag across text to select it, elsewhere to pan; scroll to zoom; double-click a word to select it, elsewhere to zoom in or out; Ctrl+A selects all text, Ctrl+C copies it.";
        ui.label(egui::RichText::new(help).weak().small());
    });
    changed
}
