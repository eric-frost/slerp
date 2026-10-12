//! Colour table mode: a picture split in two, the original on the left and
//! graded through the table on the right, with the split dragged across
//! it.  The bundled photos and charts are offered, and any photo dropped on
//! the window (or chosen in the panel) is graded instead.
//!
//! Both halves are egui textures drawn side by side, so dragging the split
//! only moves where one stops and the other starts: nothing is graded or
//! uploaded again until the picture or the table changes.

use slerp_scene::{ColorLut, Image8};
use slerp_theme::widgets;
use std::path::Path;
use std::sync::Arc;

/// Photos are graded at most this many pixels across.
const MAX_PHOTO: u32 = 2048;

/// The picture being graded.
#[derive(Clone)]
pub enum Picture {
    /// One of `slerp_lut::test_images()`.
    Builtin(usize),
    /// A photo the user chose, already shrunk to [`MAX_PHOTO`].
    Photo { name: String, image: Arc<Image8> },
}

pub struct LutView {
    pub lut: Arc<dyn ColorLut>,
    pub picture: Picture,
    /// Where the split is, as a fraction of the picture's width.
    pub split: f32,
    /// The original and graded picture as egui textures, made on first
    /// show.
    textures: Option<(egui::TextureHandle, egui::TextureHandle)>,
}

impl LutView {
    pub fn new(lut: Arc<dyn ColorLut>) -> LutView {
        LutView { lut, picture: Picture::Builtin(0), split: 0.5, textures: None }
    }

    /// Keeps the picture and split of the table shown before, so stepping
    /// through a folder of tables compares them on the same photo.
    pub fn carry_over(&mut self, previous: &LutView) {
        self.picture = previous.picture.clone();
        self.split = previous.split;
        self.textures = None;
    }

    pub fn image(&self) -> &Image8 {
        match &self.picture {
            Picture::Builtin(i) => {
                let all = slerp_lut::test_images();
                &all[(*i).min(all.len() - 1)].image
            }
            Picture::Photo { image, .. } => image,
        }
    }

    pub fn picture_name(&self) -> String {
        match &self.picture {
            Picture::Builtin(i) => slerp_lut::test_images().get(*i).map_or("", |t| t.name).to_string(),
            Picture::Photo { name, .. } => name.clone(),
        }
    }

    pub fn set_picture(&mut self, picture: Picture) {
        self.picture = picture;
        self.textures = None;
    }

    /// Grades a photo from disk instead of the bundled pictures.
    pub fn use_photo(&mut self, path: &Path) -> Result<(), String> {
        let opts = slerp_scene::textures::DecodeOptions::preview(MAX_PHOTO);
        let tex = slerp_scene::textures::decode_file(path, &opts).map_err(|e| e.to_string())?;
        // The texture thumbnail is the picture as people see it: HDR tone
        // mapped, alpha dropped.
        let image = slerp_scene::textures::thumbnail(&tex, MAX_PHOTO.min(tex.width().max(tex.height())));
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        self.set_picture(Picture::Photo { name, image: Arc::new(image) });
        Ok(())
    }

    fn textures(&mut self, ctx: &egui::Context) -> (egui::TextureHandle, egui::TextureHandle) {
        if self.textures.is_none() {
            let original = self.image().clone();
            let graded = slerp_lut::graded(&*self.lut, &original);
            let load = |name: &str, img: &Image8| {
                let color =
                    egui::ColorImage::from_rgba_unmultiplied([img.width as usize, img.height as usize], &img.pixels);
                ctx.load_texture(name, color, egui::TextureOptions::LINEAR)
            };
            self.textures = Some((load("lut-original", &original), load("lut-graded", &graded)));
        }
        self.textures.clone().expect("made above")
    }

    /// The split picture at `width` x `height` at most, for screenshots.
    pub fn screenshot(&self, width: u32, height: u32) -> Image8 {
        let img = self.image();
        let k = (width as f32 / img.width as f32).min(height as f32 / img.height as f32);
        let (w, h) = (((img.width as f32 * k) as u32).max(1), ((img.height as f32 * k) as u32).max(1));
        let original = slerp_sprites::resize(img, w, h);
        let graded = slerp_lut::graded(&*self.lut, &original);
        slerp_lut::split_vertical(&original, &graded, self.split)
    }
}

/// Draws the split picture filling `rect` and lets dragging or clicking
/// move the split.
pub fn view(ui: &mut egui::Ui, rect: egui::Rect, response: &egui::Response, v: &mut LutView) {
    let (original, graded) = v.textures(ui.ctx());
    let size = original.size_vec2();
    // 1. The picture fitted into the view with a margin.
    let room = rect.shrink(16.0);
    let k = (room.width() / size.x).min(room.height() / size.y).max(0.01);
    let shown = egui::Rect::from_center_size(rect.center(), size * k);
    // 2. Dragging, or clicking, puts the split under the pointer.
    if (response.dragged() || response.clicked())
        && let Some(p) = response.interact_pointer_pos()
    {
        v.split = ((p.x - shown.left()) / shown.width()).clamp(0.0, 1.0);
    }
    if response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
    }
    // 3. The two halves and the line between them.
    let painter = ui.painter_at(rect);
    let x = shown.left() + shown.width() * v.split;
    let left = egui::Rect::from_min_max(shown.min, egui::pos2(x, shown.max.y));
    let right = egui::Rect::from_min_max(egui::pos2(x, shown.min.y), shown.max);
    let uv = |r: egui::Rect| {
        egui::Rect::from_min_max(
            egui::pos2((r.min.x - shown.min.x) / shown.width(), 0.0),
            egui::pos2((r.max.x - shown.min.x) / shown.width(), 1.0),
        )
    };
    painter.image(original.id(), left, uv(left), egui::Color32::WHITE);
    painter.image(graded.id(), right, uv(right), egui::Color32::WHITE);
    let line = egui::Stroke::new(2.0, egui::Color32::from_white_alpha(230));
    painter.line_segment([egui::pos2(x, shown.top()), egui::pos2(x, shown.bottom())], line);
    painter.circle(egui::pos2(x, shown.center().y), 9.0, egui::Color32::from_black_alpha(140), line);
    // 4. Labels in the top corners.
    let font = egui::TextStyle::Small.resolve(ui.style());
    for (text, at, align) in [
        ("Original", shown.left_top() + egui::vec2(8.0, 8.0), egui::Align2::LEFT_TOP),
        ("Graded", shown.right_top() + egui::vec2(-8.0, 8.0), egui::Align2::RIGHT_TOP),
    ] {
        let galley = painter.layout_no_wrap(text.to_string(), font.clone(), egui::Color32::WHITE);
        let r = align.anchor_size(at, galley.size()).expand2(egui::vec2(4.0, 2.0));
        painter.rect_filled(r, 3.0, egui::Color32::from_black_alpha(120));
        painter.galley(r.min + egui::vec2(4.0, 2.0), galley, egui::Color32::WHITE);
    }
}

/// The right-hand panel: the picture to grade and the split.
pub fn controls(ui: &mut egui::Ui, v: &mut LutView, choose_photo: &mut bool) {
    crate::sections::section(ui, "Colour Table", "Colour Table", true, |ui| {
        ui.label("Picture:");
        let mut pick = None;
        let mut list = slerp_theme::widgets::List::new(ui, "lut-pictures");
        let count = slerp_lut::test_images().len();
        for (i, t) in slerp_lut::test_images().iter().enumerate() {
            let selected = matches!(v.picture, Picture::Builtin(j) if j == i);
            if list.row(ui, i, selected, t.name).clicked() {
                pick = Some(Picture::Builtin(i));
            }
        }
        // A photo dropped on the view is the last row, while it is shown.
        if let Picture::Photo { name, .. } = &v.picture {
            let _ = list.row(ui, count, true, name.as_str());
        }
        if let Some(i) = list.finish(ui)
            && i < count
        {
            pick = Some(Picture::Builtin(i));
        }
        if let Some(p) = pick {
            v.set_picture(p);
        }
        ui.add_space(4.0);
        if ui.button("Grade a Photo…").on_hover_text("Or drop a photo onto the view").clicked() {
            *choose_photo = true;
        }
        ui.add_space(4.0);
        egui::Grid::new("lut-form").num_columns(2).spacing([8.0, 4.0]).show(ui, |ui| {
            ui.label("Split:");
            let mut pct = v.split * 100.0;
            if widgets::slider(ui, egui::Slider::new(&mut pct, 0.0..=100.0).suffix(" %").integer()).changed() {
                v.split = pct / 100.0;
            }
            ui.end_row();
        });
        ui.label(egui::RichText::new("Drag across the picture to move the split.").weak().small());
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn invert() -> Arc<dyn ColorLut> {
        Arc::new(slerp_lut::parse(slerp_lut::LutFormat::Cube, b"LUT_1D_SIZE 2\n1 1 1\n0 0 0\n").unwrap())
    }

    #[test]
    fn photos_and_screenshots() {
        let mut v = LutView::new(invert());
        assert_eq!(v.picture_name(), "Market");
        // A dropped photo replaces the bundled picture.
        let path = std::env::temp_dir().join(format!("slerp-lut-photo-{}.png", std::process::id()));
        let photo = Image8 { width: 4, height: 2, pixels: [10u8, 20, 30, 255].repeat(8) };
        std::fs::write(&path, slerp_core::encode_png(&photo)).unwrap();
        v.use_photo(&path).unwrap();
        std::fs::remove_file(&path).ok();
        assert_eq!((v.image().width, v.image().height), (4, 2));
        assert!(v.picture_name().starts_with("slerp-lut-photo"));
        assert!(v.use_photo(Path::new("/nonexistent.png")).is_err());
        // A screenshot keeps the picture's proportions; with the split at
        // the far left, all of it is graded.
        v.split = 0.0;
        let shot = v.screenshot(400, 400);
        assert_eq!((shot.width, shot.height), (400, 200));
        assert_eq!(&shot.pixels[(100 * 400 + 300) * 4..][..3], &[245, 235, 225]);
        // The next table keeps the photo and split.
        let mut next = LutView::new(invert());
        next.carry_over(&v);
        assert_eq!(next.image().width, 4);
        assert_eq!(next.split, 0.0);
    }
}
