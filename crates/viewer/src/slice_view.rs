//! Controls shared by the viewers of stacks of slices: DICOM series
//! (`dicom_view`) and volumes (`volume_view`: NIfTI, NRRD, TIFF stacks,
//! HDF5 arrays).
//!
//! Brightness and contrast are a window, as medical viewers have it: a
//! centre and a width, where everything below the window is black and
//! everything above it white.  Dragging with the right mouse button (or
//! Ctrl and the left) moves it, and a list of presets sets it.  The window
//! is plain numbers here; each viewer keeps its own values and its own kinds
//! of preset (a DICOM file's stored windows and VOI tables, a volume's
//! percentiles).

use egui::{Slider, Ui};
use slerp_theme::widgets;

/// A number as short as it reads well: 40, -600, 0.25.
pub fn number(v: f64) -> String {
    if (v - v.round()).abs() < 1e-6 && v.abs() < 1e9 {
        format!("{}", v.round() as i64)
    } else if v.abs() >= 100.0 {
        format!("{v:.0}")
    } else if v.abs() >= 1.0 {
        format!("{v:.2}")
    } else {
        format!("{v:.4}")
    }
}

/// The window after a drag of `delta` physical pixels: sideways changes its
/// width (right widens it, more contrast to less), up and down its centre
/// (down raises the centre, which darkens), at a speed that crosses the
/// data's `range` in about 600 pixels.
pub fn dragged_window((center, width): (f64, f64), range: (f32, f32), delta: egui::Vec2) -> (f64, f64) {
    let k = ((range.1 - range.0).max(1.0) / 600.0) as f64;
    let narrowest = if k < 0.01 { 1e-6 } else { 1.0 };
    ((center + delta.y as f64 * k), (width + delta.x as f64 * k).max(narrowest))
}

/// What the window controls changed.
pub enum WindowEdit<V> {
    /// A preset was picked.
    Preset(String, V),
    /// Centre and width were typed or dragged.
    Custom { center: f64, width: f64 },
}

/// The window section: a list of presets, and the centre and width as number
/// boxes.  `choices` are the presets in order, `current` names the one in
/// use (any other name reads "Custom"), `window` is (centre, width), and
/// `range` the data's lowest and highest value, which sets how fast the
/// number boxes drag.  `id` keeps two sets of controls apart.
pub fn window_controls<V: Clone>(
    ui: &mut Ui,
    id: &str,
    choices: &[(String, V)],
    current: &str,
    window: (f64, f64),
    range: (f32, f32),
) -> Option<WindowEdit<V>> {
    let index = choices.iter().position(|(n, _)| n == current).unwrap_or(usize::MAX);
    let mut pick = index;
    let mut edit = None;
    egui::Grid::new(format!("{id}-window")).num_columns(2).spacing([8.0, 4.0]).show(ui, |ui| {
        ui.label("Preset:");
        let items = choices.iter().enumerate().map(|(i, (n, _))| (i, n.clone()));
        widgets::Choice::new(format!("{id}-preset"), &mut pick, items).width(170.0).text_when_other("Custom").show(ui);
        ui.end_row();
        let (mut c, mut w) = window;
        let speed = ((range.1 - range.0).max(1.0) / 600.0) as f64;
        ui.label("Centre:");
        let rc = ui.add(egui::DragValue::new(&mut c).speed(speed).max_decimals(2));
        ui.end_row();
        ui.label("Width:");
        let rw = ui.add(egui::DragValue::new(&mut w).speed(speed).range(1e-6..=f64::MAX).max_decimals(2));
        ui.end_row();
        if rc.changed() || rw.changed() {
            edit = Some(WindowEdit::Custom { center: c, width: w });
        }
    });
    if pick != index
        && let Some((name, v)) = choices.get(pick)
    {
        return Some(WindowEdit::Preset(name.clone(), v.clone()));
    }
    edit
}

/// A slider that counts from 1 (`index` counts from 0), labelled "Slice 3",
/// for `count` slices.  True when the user moved it.
pub fn counter_slider(ui: &mut Ui, prefix: &str, index: &mut usize, count: usize) -> bool {
    let mut shown = *index + 1;
    let r = widgets::slider(ui, Slider::new(&mut shown, 1..=count.max(1)).prefix(prefix.to_string()));
    if r.changed() {
        *index = shown - 1;
        true
    } else {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_read_short() {
        assert_eq!(number(40.0), "40");
        assert_eq!(number(-600.0), "-600");
        assert_eq!(number(1500.0), "1500");
        assert_eq!(number(18.55), "18.55");
        assert_eq!(number(2817.2), "2817");
        assert_eq!(number(0.25), "0.2500");
    }

    #[test]
    fn dragging_widens_right_and_darkens_down() {
        // A range of 600 values moves one value per pixel.
        let (c, w) = dragged_window((100.0, 200.0), (0.0, 600.0), egui::vec2(10.0, -5.0));
        assert_eq!((c, w), (95.0, 210.0));
        // The window never closes: at least one value wide (for integers), or a tiny sliver for small ranges.
        let (_, w) = dragged_window((0.0, 5.0), (0.0, 600.0), egui::vec2(-100.0, 0.0));
        assert_eq!(w, 1.0);
        let (_, w) = dragged_window((0.0, 0.001), (0.0, 1.0), egui::vec2(-100.0, 0.0));
        assert_eq!(w, 1e-6);
    }
}
