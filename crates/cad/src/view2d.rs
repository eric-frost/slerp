//! A flat drawing as a layered picture (`scene::Canvas`), which is how
//! `slerp info` and generic callers see it, and its thumbnail.

use crate::drawing::{Bounds, Drawing};
use crate::raster::{Paint, View};
use glam::DVec2;
use slerp_scene::{Canvas, CanvasLayer, Image8, Info};

/// A flat drawing with the layer list a viewer shows.
pub struct FlatDrawing {
    pub drawing: Drawing,
    layers: Vec<CanvasLayer>,
    size: (u32, u32),
}

/// The longest side `size()` reports, in pixels.
const FULL_SIZE: f64 = 2048.0;

impl FlatDrawing {
    pub fn new(drawing: Drawing) -> FlatDrawing {
        let layers = drawing
            .layers
            .iter()
            .map(|l| CanvasLayer {
                name: l.name.clone(),
                visible: l.visible,
                depth: 0,
                group: false,
                note: match l.items {
                    0 => "empty".to_string(),
                    1 => "1 item".to_string(),
                    n => format!("{n} items"),
                },
            })
            .collect();
        let size = picture_size(drawing.bounds.as_ref(), FULL_SIZE as u32);
        FlatDrawing { drawing, layers, size }
    }

    /// The view that shows the whole drawing in a window of this size.
    pub fn fit(&self, width: u32, height: u32) -> View {
        match &self.drawing.bounds {
            Some(b) => View::fit(b, width, height, 0.03),
            None => View { center: DVec2::ZERO, scale: 1.0, width, height },
        }
    }
}

/// Width and height of a picture of `b` whose longer side is `long`, with
/// the shape limited to 4:1 either way.
pub fn picture_size(b: Option<&Bounds>, long: u32) -> (u32, u32) {
    let aspect = match b {
        Some(b) if b.size().x > 0.0 && b.size().y > 0.0 => (b.size().x / b.size().y).clamp(0.25, 4.0),
        _ => 1.0,
    };
    if aspect >= 1.0 {
        (long, ((long as f64 / aspect).round() as u32).max(16))
    } else {
        (((long as f64 * aspect).round() as u32).max(16), long)
    }
}

impl Canvas for FlatDrawing {
    fn info(&self) -> &Info {
        &self.drawing.info
    }

    fn size(&self) -> (u32, u32) {
        self.size
    }

    fn layers(&self) -> &[CanvasLayer] {
        &self.layers
    }

    fn frames(&self) -> &[f32] {
        &[0.0]
    }

    fn clips(&self) -> &[slerp_scene::FrameClip] {
        &[]
    }

    fn compose(&self, _frame: usize, shown: &[bool], scale: f32) -> Image8 {
        let (w, h) = self.size;
        let (w, h) = (((w as f32 * scale).round() as u32).max(1), ((h as f32 * scale).round() as u32).max(1));
        self.drawing.render(&self.fit(w, h), shown, &Paint::LIGHT)
    }
}

/// A thumbnail: the drawing on paper, shaped like the drawing (up to 4:1).
pub fn thumbnail(d: &Drawing, size: u32) -> Image8 {
    let (w, h) = picture_size(d.bounds.as_ref(), size);
    // Lines grow a little with the thumbnail so they stay readable.
    let paint = Paint { line_px: 1.3 + size as f64 / 512.0, ..Paint::LIGHT };
    d.render_fit(w, h, &d.saved_visibility(), &paint)
}
