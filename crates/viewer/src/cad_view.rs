//! Flat CAD drawings (DXF): shown on paper with pan and zoom like a texture,
//! but drawn again at every zoom, so lines stay crisp however far one goes
//! in.  A picture of the part of the drawing in view (and a margin around
//! it) is made on a thread and put on the screen as a texture; while a new
//! one is being made, the last stays on screen, moved and scaled to match.
//!
//! The side panel lists the layers with a switch each, picks light or dark
//! paper, and zooms to the whole drawing.  Pointing at the drawing shows
//! its coordinates in the drawing's units.
//!
//! Drawings with depth (3D faces, solids) and Rhino models are scenes and
//! use the ordinary 3D view, with one node per layer.

use crate::gfx::Gfx;
use crate::loader::{Task, Waker, poll, spawn};
use crate::texture_view::TextureView;
use egui::{PointerButton, Ui};
use glam::DVec2;
use slerp_core::cad::{FlatDrawing, Paint, View};
use slerp_scene::{ColorSpace, Image8, Texture};
use slerp_theme::widgets;
use std::sync::Arc;

/// How far past the window a picture reaches on each side, in pixels, so
/// panning shows drawing while the next picture is made.
const MARGIN: u32 = 160;
/// GPU textures hold at least this many pixels on a side everywhere.
const MAX_SIDE: u32 = 8192;
/// How far in or out one may go, relative to the whole drawing fitting.
const MIN_ZOOM: f64 = 0.02;
const MAX_ZOOM: f64 = 1.0e7;

/// What a picture was made from, which says whether it still fits what is
/// wanted.
#[derive(Clone, Debug, PartialEq)]
struct Key {
    center: [f64; 2],
    scale: f64,
    size: (u32, u32),
    shown: u64,
    dark: bool,
    line: u32,
}

struct Rendered {
    key: Key,
    texture: TextureView,
}

pub struct CadView {
    pub drawing: Arc<FlatDrawing>,
    /// Which layers show, one per layer.
    pub shown: Vec<bool>,
    pub dark: bool,
    /// The line width in logical pixels, in tenths.
    pub line_tenths: u32,
    /// The drawing point in the middle of the view, and pixels per unit.
    center: DVec2,
    scale: f64,
    /// Keeps the whole drawing in view until the user pans or zooms.
    fit: bool,
    /// Frame everything, strays included, rather than the bulk.
    everything: bool,
    image: Option<Rendered>,
    pending: Option<(Key, Task<Image8>)>,
    waker: Waker,
    name: String,
    /// The view's size and the screen's scale in the last frame.
    size: (u32, u32),
    ppp: f32,
}

impl CadView {
    /// Shows `drawing`.  `first` is its picture fitted into
    /// [`FIRST_SIZE`], made on the loading thread, which stands in until the
    /// window's size is known.
    pub fn new(
        name: String,
        drawing: Arc<FlatDrawing>,
        first: Image8,
        gfx: &mut Gfx,
        waker: Waker,
    ) -> Result<CadView, String> {
        let shown = drawing.drawing.saved_visibility();
        let texture = TextureView::new(name.clone(), image_texture(first), gfx)?;
        let (w, h) = (texture.texture.width(), texture.texture.height());
        let view = fit_view(&drawing, false, (w, h));
        let key = Key {
            center: view.center.to_array(),
            scale: view.scale,
            size: (w, h),
            shown: hash(&shown),
            dark: false,
            line: 14,
        };
        Ok(CadView {
            center: view.center,
            scale: view.scale,
            drawing,
            shown,
            dark: false,
            line_tenths: 14,
            fit: true,
            everything: false,
            image: Some(Rendered { key, texture }),
            pending: None,
            waker,
            name,
            size: (w, h),
            ppp: 1.0,
        })
    }

    /// Keeps what the user chose after the file reloaded.
    pub fn carry_over(&mut self, old: &CadView) {
        let names = |v: &CadView| v.drawing.drawing.layers.iter().map(|l| l.name.clone()).collect::<Vec<_>>();
        if names(old) == names(self) {
            self.shown = old.shown.clone();
        }
        self.dark = old.dark;
        self.line_tenths = old.line_tenths;
        self.everything = old.everything;
        self.fit = old.fit;
        self.center = old.center;
        self.scale = old.scale;
    }

    pub fn summary(&self) -> String {
        let d = &self.drawing.drawing;
        let items = d.polys.len() + d.arcs.len() + d.fills.len() + d.dots.len();
        format!("{} items on {} layers", group(items as u64), d.layers.len())
    }

    fn paint(&self) -> Paint {
        let base = if self.dark { Paint::DARK } else { Paint::LIGHT };
        Paint { line_px: self.line_tenths as f64 / 10.0 * self.ppp as f64, ..base }
    }

    fn view(&self) -> View {
        View { center: self.center, scale: self.scale, width: self.size.0, height: self.size.1 }
    }

    fn wanted(&self) -> Key {
        // The picture covers the window and a margin all round.
        let m = (MARGIN as f32 * self.ppp) as u32;
        let size = ((self.size.0 + 2 * m).min(MAX_SIDE), (self.size.1 + 2 * m).min(MAX_SIDE));
        Key {
            center: self.center.to_array(),
            scale: self.scale,
            size,
            shown: hash(&self.shown),
            dark: self.dark,
            line: (self.line_tenths as f32 * self.ppp * 10.0) as u32,
        }
    }

    /// The whole picture of the drawing in a window of `size`.
    fn fit_now(&mut self) {
        let v = fit_view(&self.drawing, self.everything, self.size);
        self.center = v.center;
        self.scale = v.scale;
        self.fit = true;
    }

    fn fit_scale(&self) -> f64 {
        fit_view(&self.drawing, self.everything, self.size).scale
    }

    /// Zooms by `factor` about `at`, a point in physical pixels from the
    /// view's top left, so the drawing point under it stays.
    fn zoom_about(&mut self, factor: f64, at: DVec2) {
        let before = self.view().to_world(at);
        let fit = self.fit_scale();
        self.scale = (self.scale * factor).clamp(fit * MIN_ZOOM, fit * MAX_ZOOM);
        let after = self.view().to_world(at);
        self.center += before - after;
        self.fit = false;
    }

    /// Starts and finishes pictures: the one wanted is made on a thread, and
    /// when it arrives it goes up to the GPU.
    fn update(&mut self, gfx: &mut Gfx) {
        if let Some((key, task)) = &self.pending
            && let Some(result) = poll(task)
        {
            let key = key.clone();
            self.pending = None;
            if let Ok(img) = result
                && let Ok(texture) = TextureView::new(self.name.clone(), image_texture(img), gfx)
            {
                self.image = Some(Rendered { key, texture });
            }
        }
        let wanted = self.wanted();
        let current = self.image.as_ref().map(|i| &i.key);
        let pending = self.pending.as_ref().map(|p| &p.0);
        if current != Some(&wanted) && pending != Some(&wanted) && self.pending.is_none() {
            let drawing = self.drawing.clone();
            let shown = self.shown.clone();
            let paint = self.paint();
            let view = View { center: self.center, scale: self.scale, width: wanted.size.0, height: wanted.size.1 };
            let task =
                spawn(&self.waker, "slerp-cad-render", move || Ok(drawing.drawing.render(&view, &shown, &paint)));
            self.pending = Some((wanted, task));
        }
    }

    /// Puts the picture on the target, moved and scaled to the current view.
    fn draw(&mut self, gfx: &mut Gfx) {
        let view = self.view();
        let Some(img) = &mut self.image else { return };
        let k = &img.key;
        let t = &mut img.texture;
        t.fit = false;
        t.set_size(self.size);
        let ratio = view.scale / k.scale;
        let (w, h) = (k.size.0 as f64, k.size.1 as f64);
        // The texel at the middle of the window.
        let u = w / 2.0 + (view.center.x - k.center[0]) * k.scale;
        let v = h / 2.0 - (view.center.y - k.center[1]) * k.scale;
        t.params.center = [u as f32, v as f32];
        t.params.zoom = ratio as f32;
        t.params.checkerboard = false;
        let bg = Paint { dark: k.dark, line_px: 1.0 }.background();
        // Paper shows as drawn: the target is sRGB-encoded already.
        t.params.background = [bg[0] as f32 / 255.0, bg[1] as f32 / 255.0, bg[2] as f32 / 255.0];
        t.draw(gfx);
    }
}

/// The viewer's first picture of a drawing: its size in pixels.
pub const FIRST_SIZE: (u32, u32) = (1280, 800);

/// A picture of the whole drawing, for the loading thread to make.
pub fn first_picture(drawing: &FlatDrawing) -> Image8 {
    let paint = Paint { line_px: 1.4, ..Paint::LIGHT };
    let view = fit_view(drawing, false, FIRST_SIZE);
    drawing.drawing.render(&view, &drawing.drawing.saved_visibility(), &paint)
}

fn image_texture(img: Image8) -> Arc<Texture> {
    Arc::new(Texture::from_rgba8(img.width, img.height, img.pixels, ColorSpace::Srgb))
}

fn hash(shown: &[bool]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    shown.hash(&mut h);
    h.finish()
}

fn fit_view(d: &FlatDrawing, everything: bool, size: (u32, u32)) -> View {
    let b = if everything { d.drawing.full_bounds.as_ref() } else { d.drawing.bounds.as_ref() };
    match b {
        Some(b) => View::fit(b, size.0, size.1, 0.04),
        None => View { center: DVec2::ZERO, scale: 1.0, width: size.0, height: size.1 },
    }
}

/// Input, drawing and the status text for a drawing.  Returns whether it
/// needs another frame.
#[allow(clippy::too_many_arguments)]
pub fn drive(
    c: &mut CadView,
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
    c.ppp = ppp;
    let resized = size != c.size;
    c.size = size;
    if resized && c.fit {
        c.fit_now();
    }
    if let Some((_, d)) = drag {
        // Dragging moves the drawing with the pointer.
        c.center.x -= d.x as f64 * ppp as f64 / c.scale;
        c.center.y += d.y as f64 * ppp as f64 / c.scale;
        c.fit = false;
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
    if response.double_clicked() {
        if c.fit {
            // A close look at what is under the pointer.
            let m = pointer.map_or(DVec2::new(size.0 as f64 / 2.0, size.1 as f64 / 2.0), at);
            c.zoom_about(8.0, m);
        } else {
            c.fit_now();
        }
    }
    if response.has_focus() {
        let (home, f) = response.ctx.input(|i| (i.key_pressed(egui::Key::Home), i.key_pressed(egui::Key::F)));
        if home || f {
            c.fit_now();
        }
    }
    if let (Some(p), true) = (pointer, response.hovered()) {
        let w = c.view().to_world(at(p));
        // Digits to about a tenth of a pixel.
        let decimals = (-(0.1 / c.scale.max(1e-12)).log10().floor()).clamp(0.0, 9.0) as usize;
        *hover = Some(format!("{:.*}, {:.*}", decimals, w.x, decimals, w.y));
    }
    c.update(gfx);
    c.draw(gfx);
    c.pending.is_some()
}

/// The drawing for the window's screenshot command: the part in view, drawn
/// at the size asked for.
pub fn screenshot(c: &CadView, width: u32, height: u32) -> Image8 {
    let k = c.size.0.max(1) as f64;
    let view = View { center: c.center, scale: c.scale * width as f64 / k, width, height };
    c.drawing.drawing.render(
        &view,
        &c.shown,
        &Paint { line_px: c.line_tenths as f64 / 10.0 * (width as f64 / k), ..c.paint() },
    )
}

/// The right-hand panel: the paper, zooming, and the layer list.
pub fn controls(ui: &mut Ui, c: &mut CadView) {
    crate::sections::section(ui, "Drawing", "Drawing", true, |ui| {
        let d = &c.drawing.drawing;
        ui.label(c.summary());
        if let Some(b) = &d.bounds {
            let s = b.size();
            ui.label(format!("{} × {} units", nice(s.x), nice(s.y)));
        }
        ui.add_space(4.0);
        egui::Grid::new("cad-form").num_columns(2).spacing([8.0, 6.0]).show(ui, |ui| {
            ui.label("Paper:");
            widgets::radio_group(ui, &mut c.dark, &[(false, "Light"), (true, "Dark")]);
            ui.end_row();
            ui.label("Lines:");
            let mut line = c.line_tenths as f32 / 10.0;
            widgets::slider(ui, egui::Slider::new(&mut line, 0.8..=5.0).suffix(" px").fixed_decimals(1));
            c.line_tenths = (line * 10.0).round() as u32;
            ui.end_row();
        });
        ui.horizontal(|ui| {
            if ui.button("Zoom to Fit").on_hover_text("Home or F").clicked() {
                c.everything = false;
                c.fit_now();
            }
            if c.drawing.drawing.outliers > 0
                && ui
                    .button("Zoom to Everything")
                    .on_hover_text("Includes items far from the rest, which are left out of Fit")
                    .clicked()
            {
                c.everything = true;
                c.fit_now();
            }
        });
        ui.label(egui::RichText::new("Drag to pan, scroll to zoom, double-click to zoom in or out.").weak().small());
    });
    layers(ui, c);
}

fn layers(ui: &mut Ui, c: &mut CadView) {
    let d = c.drawing.clone();
    let layers = &d.drawing.layers;
    if layers.is_empty() {
        return;
    }
    crate::sections::section(ui, "Layers", "Layers", true, |ui| {
        ui.horizontal(|ui| {
            if ui.button("Show All").clicked() {
                c.shown.iter_mut().for_each(|s| *s = true);
            }
            if ui.button("Hide All").clicked() {
                c.shown.iter_mut().for_each(|s| *s = false);
            }
            if ui.button("As Saved").clicked() {
                c.shown = d.drawing.saved_visibility();
            }
        });
        let paint = c.paint();
        egui::ScrollArea::vertical().id_salt("cad-layers").max_height(460.0).show(ui, |ui| {
            for (i, l) in layers.iter().enumerate() {
                ui.horizontal(|ui| {
                    let (rect, _) = ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
                    let [r, g, b] = paint.resolve(l.color);
                    ui.painter().rect_filled(rect, 2.0, egui::Color32::from_rgb(r, g, b));
                    let name = if l.name.is_empty() { format!("Layer {i}") } else { l.name.clone() };
                    ui.add(widgets::CheckBox::new(&mut c.shown[i], name));
                    if l.items == 0 {
                        ui.label(egui::RichText::new("empty").weak().small());
                    } else {
                        ui.label(egui::RichText::new(group(l.items as u64)).weak().small());
                    }
                });
            }
        });
    });
}

/// A size with a sensible number of digits.
fn nice(v: f64) -> String {
    if v >= 1000.0 {
        format!("{v:.0}")
    } else if v >= 10.0 {
        format!("{v:.1}")
    } else {
        format!("{v:.3}")
    }
}

/// 129388 as "129,388".
fn group(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}
