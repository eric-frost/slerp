//! Controls for pictures of linear measurements (astronomy frames, film
//! scans): a stretch picker, black and white point sliders, and the list of
//! header cards the file carried.
//!
//! Raw data is nearly black, so these pictures open through an automatic
//! stretch (see `slerp_textures::stretch`); the controls change it, and the
//! choice of curve is remembered for each format.

use crate::saved::{Saved, SavedStretch};
use crate::texture_view::{TextureView, remember_stretch};
use egui::{Slider, Ui};
use slerp_scene::Texture;
use slerp_scene::textures::stretch::{DEFAULT_TARGET, Stretch, StretchMode};
use slerp_theme::widgets;

/// Loads the stretch settings saved for each format, at start.
pub fn load_saved(saved: &Saved) {
    for (format, s) in &saved.stretch {
        let mode = StretchMode::from_name(&s.mode).unwrap_or_default();
        let target = if s.target.is_finite() { s.target.clamp(0.02, 0.9) } else { DEFAULT_TARGET };
        let shift = |v: f32| if v.is_finite() { v.clamp(-1.0, 1.0) } else { 0.0 };
        let (black_shift, white_shift) = (shift(s.black_shift), shift(s.white_shift));
        remember_stretch(format, &Stretch { mode, linked: s.linked, target, black_shift, white_shift });
    }
}

fn save(saved: &mut Saved, format: &str, s: &Stretch) {
    remember_stretch(format, s);
    let entry = SavedStretch {
        mode: s.mode.name().into(),
        linked: s.linked,
        target: s.target,
        black_shift: s.black_shift,
        white_shift: s.white_shift,
    };
    if saved.stretch.get(format) != Some(&entry) {
        saved.stretch.insert(format.to_string(), entry);
        saved.save();
    }
}

/// The stretch section of the right-hand panel.  Returns whether the
/// picture needs drawing again.
pub fn controls(ui: &mut Ui, t: &mut TextureView, saved: &mut Saved, gfx: &mut crate::gfx::Gfx) {
    let tex = t.texture.clone();
    let Some(s) = &mut t.stretch else { return };
    crate::sections::section(ui, "Stretch", "Stretch", true, |ui| {
        let before = (s.settings, s.layer);
        let stats = s.stats(&tex);
        let defaults = s.default_points(&tex);
        let colour = !tex.levels[0].surfaces.is_empty() && channels_differ(&stats);
        egui::Grid::new("stretch-form").num_columns(2).spacing([8.0, 6.0]).show(ui, |ui| {
            ui.label("Curve:");
            ui.horizontal_wrapped(|ui| {
                let modes: Vec<(StretchMode, &str)> = StretchMode::ALL.iter().map(|m| (*m, m.name())).collect();
                if widgets::radio_group(ui, &mut s.settings.mode, &modes).changed() {
                    // Each curve starts from its own black and white points.
                    s.settings = s.settings.reset_points();
                }
            });
            ui.end_row();
            if s.layers > 1 {
                ui.label("Plane:");
                let mut layer = s.layer;
                widgets::slider(ui, Slider::new(&mut layer, 0..=s.layers - 1));
                s.layer = layer;
                ui.end_row();
            }
            // Positions of the points on the sliders, from the defaults.
            let d = defaults[1];
            let mut black = (d.to_slider(d.black) + s.settings.black_shift).clamp(0.0, 1.0);
            let mut white = (d.to_slider(d.white) + s.settings.white_shift).clamp(0.0, 1.0);
            ui.label("Black point:");
            let r = widgets::slider(
                ui,
                Slider::new(&mut black, 0.0..=1.0)
                    .custom_formatter(|v, _| format_value(d.from_slider(v as f32)))
                    .custom_parser(|text| text.trim().parse::<f64>().ok().map(|v| d.to_slider(v as f32) as f64)),
            );
            if r.changed() {
                s.settings.black_shift = black - d.to_slider(d.black);
            }
            ui.end_row();
            ui.label("White point:");
            let r = widgets::slider(
                ui,
                Slider::new(&mut white, 0.0..=1.0)
                    .custom_formatter(|v, _| format_value(d.from_slider(v as f32)))
                    .custom_parser(|text| text.trim().parse::<f64>().ok().map(|v| d.to_slider(v as f32) as f64)),
            );
            if r.changed() {
                s.settings.white_shift = white - d.to_slider(d.white);
            }
            ui.end_row();
            if s.settings.mode != StretchMode::Linear {
                ui.label("Brightness:");
                widgets::slider(ui, Slider::new(&mut s.settings.target, 0.02..=0.8).fixed_decimals(2)).on_hover_text(
                    "How bright the typical background (the median) is drawn, as in PixInsight's stretch; 0.25 is its default.  The linear curve has no such setting: its white point sets the brightness",
                );
                ui.end_row();
            }
        });
        if colour {
            let mut linked = s.settings.linked;
            if ui
                .checkbox(&mut linked, "Link colour channels")
                .on_hover_text("One set of points for red, green and blue keeps the colour balance; separate ones neutralise a colour cast")
                .changed()
            {
                s.settings.linked = linked;
            }
        }
        ui.horizontal(|ui| {
            if ui.button("Reset points").clicked() {
                s.settings = s.settings.reset_points();
            }
        });
        let c = stats.channels[1];
        egui::Grid::new("stretch-stats").num_columns(2).spacing([8.0, 2.0]).show(ui, |ui| {
            for (k, v) in [
                ("Lowest", format_value(c.min)),
                ("Median", format_value(c.median)),
                ("Noise", format_value(c.sigma)),
                ("Highest", format_value(c.max)),
            ] {
                ui.label(egui::RichText::new(k).weak());
                ui.label(v);
                ui.end_row();
            }
        });
        if (s.settings, s.layer) != before {
            let format = s.container.clone();
            let settings = s.settings;
            save(saved, &format, &settings);
        }
    });
    if let Err(e) = t.restretch(gfx) {
        eprintln!("slerp view: {e}");
    }
}

/// Whether a colour picture's channels have different statistics, so
/// linking them makes a difference.
fn channels_differ(stats: &slerp_scene::textures::stretch::DataStats) -> bool {
    let c = &stats.channels;
    c[0] != c[1] || c[1] != c[2]
}

/// A data value to a useful number of digits: integers plainly, tiny and
/// huge ones in scientific notation.
pub fn format_value(v: f32) -> String {
    if !v.is_finite() {
        return "-".into();
    }
    let a = v.abs();
    if a == 0.0 {
        "0".into()
    } else if !(1e-3..1e7).contains(&a) {
        format!("{v:.3e}")
    } else if a >= 1000.0 || v.fract() == 0.0 {
        format!("{v:.0}")
    } else if a >= 100.0 {
        format!("{v:.1}")
    } else {
        format!("{v:.4}")
    }
}

/// The inspector's list of header cards (FITS keywords, DPX film and
/// television fields, XISF properties).
pub fn header(ui: &mut Ui, tex: &Texture) {
    if tex.source.header.is_empty() {
        return;
    }
    let title = format!("Header ({})", tex.source.header.len());
    crate::sections::section(ui, "Header", title, false, |ui| {
        egui::ScrollArea::vertical().id_salt("header-cards").max_height(320.0).show(ui, |ui| {
            egui::Grid::new("header-form").num_columns(2).spacing([8.0, 2.0]).striped(true).show(ui, |ui| {
                for (k, v) in &tex.source.header {
                    ui.label(egui::RichText::new(k).weak());
                    ui.add(egui::Label::new(v).wrap());
                    ui.end_row();
                }
            });
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::texture_view::remembered_stretch;

    #[test]
    fn values_print_with_a_fitting_number_of_digits() {
        assert_eq!(format_value(0.0), "0");
        assert_eq!(format_value(1234.0), "1234");
        assert_eq!(format_value(12.3456), "12.3456");
        assert_eq!(format_value(123.456), "123.5");
        assert_eq!(format_value(0.00001234), "1.234e-5");
        assert_eq!(format_value(f32::NAN), "-");
        assert_eq!(format_value(65535.0), "65535");
    }

    #[test]
    fn saved_settings_load_by_format() {
        let mut saved = Saved::default();
        saved.stretch.insert(
            "TESTFMT".into(),
            SavedStretch { mode: "Asinh".into(), linked: true, target: 0.3, black_shift: 0.1, white_shift: -0.2 },
        );
        load_saved(&saved);
        let s = remembered_stretch("TESTFMT");
        assert_eq!((s.mode, s.linked, s.target), (StretchMode::Asinh, true, 0.3));
        assert_eq!((s.black_shift, s.white_shift), (0.1, -0.2));
        assert_eq!(remembered_stretch("OTHER"), Stretch::default());
    }
}
