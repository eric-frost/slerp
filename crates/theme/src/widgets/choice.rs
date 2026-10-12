//! A combo box that knows its items, so the keyboard and the mouse wheel
//! can step through them without opening the list, as Qt's do.

use super::keys::{self, Step};
use egui::{AsIdSalt, CornerRadius, IdSalt, Response, Stroke, Ui, WidgetText};

/// A combo box choosing `current` among `items`, drawn like Breeze's (see
/// [`super::combo_box`]).  Clicking it opens the list and gives it the
/// keyboard focus; while it has the focus, Up and Left choose the previous
/// item, Down and Right the next, Page Up and Page Down likewise, and Home
/// and End the first and last, without opening the list, and Space or
/// Enter opens it.  The mouse wheel over it steps through the items too,
/// once the window calls [`super::wheel_hook`].  The response says
/// `changed()` when `current` changed.
///
/// ```
/// # egui::__run_test_ui(|ui| {
/// let mut lod: Option<u32> = None;
/// let items = [(None, "All".to_string()), (Some(0), "LOD 0".into()), (Some(1), "LOD 1".into())];
/// slerp_theme::widgets::Choice::new("lod", &mut lod, items).width(150.0).show(ui);
/// # });
/// ```
pub struct Choice<'a, V> {
    id_salt: IdSalt,
    current: &'a mut V,
    items: Vec<(V, String)>,
    width: Option<f32>,
    separators: Vec<usize>,
    text: Option<String>,
    extra: Option<Extra<'a>>,
}

/// Entries added at the end of a [`Choice`]'s list.
type Extra<'a> = Box<dyn FnOnce(&mut Ui) + 'a>;

impl<'a, V: PartialEq + Clone> Choice<'a, V> {
    pub fn new(id_salt: impl AsIdSalt, current: &'a mut V, items: impl IntoIterator<Item = (V, String)>) -> Self {
        Choice {
            id_salt: IdSalt::new(id_salt),
            current,
            items: items.into_iter().collect(),
            width: None,
            separators: Vec::new(),
            text: None,
            extra: None,
        }
    }

    /// The width of the closed box.
    pub fn width(mut self, width: f32) -> Self {
        self.width = Some(width);
        self
    }

    /// Draws a separator in the list before item `index`.
    pub fn separator_before(mut self, index: usize) -> Self {
        self.separators.push(index);
        self
    }

    /// What the closed box says when `current` isn't among the items.
    pub fn text_when_other(mut self, text: impl Into<String>) -> Self {
        self.text = Some(text.into());
        self
    }

    /// More entries at the end of the list, after a separator, such as
    /// "Choose File…".  The keyboard and wheel don't step onto them.
    pub fn extra(mut self, add: impl FnOnce(&mut Ui) + 'a) -> Self {
        self.extra = Some(Box::new(add));
        self
    }

    pub fn show(self, ui: &mut Ui) -> Response {
        let Choice { id_salt, current, items, width, separators, text, extra } = self;
        // The id egui's combo box gives its button: it salts the salt it is
        // given once more.
        let id = ui.make_persistent_id(IdSalt::new(id_salt));
        let open = egui::ComboBox::is_open(ui.ctx(), id);
        let index = items.iter().position(|(v, _)| v == current);
        let mut changed = false;

        // 1. Keys and wheel turns meant for the closed box, which the
        //    picture drawn below already shows.
        if !open && ui.is_enabled() {
            let mut target = index;
            if let Some(step) = keys::take_steps(ui, id, 1) {
                target = step.apply(target, items.len());
            }
            let wheel = keys::take_wheel(ui, id);
            if wheel != 0 {
                target = Step::By(wheel).apply(target, items.len());
            }
            if let Some(t) = target
                && target != index
            {
                *current = items[t].0.clone();
                changed = true;
            }
        }

        // 2. The box and its list.
        let index = items.iter().position(|(v, _)| v == current);
        let shown = match index {
            Some(i) => items[i].1.clone(),
            None => text.unwrap_or_default(),
        };
        let mut combo = super::combo_box(ui.ctx(), id_salt, WidgetText::from(shown));
        if let Some(w) = width {
            combo = combo.width(w);
        }
        // egui draws a focused widget as if pressed; Breeze draws a focused
        // combo box as it rests, with the focus outline painted below.
        let pressed = ui.input(|i| i.pointer.any_down());
        let resting = (ui.memory(|m| m.has_focus(id)) && !pressed).then(|| {
            let v = &mut ui.visuals_mut().widgets;
            std::mem::replace(&mut v.active, v.inactive)
        });
        let mut picked = false;
        let mut response = combo
            .show_ui(ui, |ui| {
                for (i, (v, label)) in items.iter().enumerate() {
                    if separators.contains(&i) {
                        ui.separator();
                    }
                    if ui.selectable_label(Some(i) == index, label.as_str()).clicked() && v != current {
                        *current = v.clone();
                        picked = true;
                    }
                }
                if let Some(extra) = extra {
                    ui.separator();
                    extra(ui);
                }
            })
            .response;
        if let Some(active) = resting {
            ui.visuals_mut().widgets.active = active;
        }

        // 3. Focus: a click gives it, and choosing from the list hands it
        //    back to the box (the click in the list took it away), so the
        //    arrows go on from the item chosen.
        keys::focus_on_click(&response);
        if picked {
            response.request_focus();
        }
        if ui.is_enabled() {
            keys::wheel_target(ui, id, response.rect);
        }
        if response.has_focus() && !open {
            let p = crate::style::palette(ui.ctx());
            let radius = CornerRadius::same(p.metrics.frame_radius.round() as u8);
            ui.painter().rect_stroke(response.rect, radius, Stroke::new(1.0, p.focus), egui::StrokeKind::Inside);
        }
        if changed || picked {
            response.mark_changed();
            ui.ctx().request_repaint();
        }
        response
    }
}
