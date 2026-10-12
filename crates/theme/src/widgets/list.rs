//! Lists of rows of which one is selected (animation clips, sprite tags,
//! materials, test pictures), driven by the keyboard as Qt's item views
//! are.

use super::keys;
use egui::{Id, Response, Sense, Stroke, Ui, WidgetText};

/// Rows the keyboard moved through last frame, and which was selected.
#[derive(Clone, Default)]
struct Rows {
    keys: Vec<(usize, Id)>,
    selected: Option<usize>,
}

/// Rows a click selects.  Clicking a row also gives it the keyboard focus,
/// and while one of the list's rows has the focus, Up and Down (and Left
/// and Right) select the previous or next row, Page Up and Page Down ten
/// rows on, and Home and End the first and last; [`List::finish`] says
/// which.  As in Qt's views, the list is one stop for Tab: only the
/// selected row (or the first, when none is) takes the focus from Tab.
///
/// ```
/// # egui::__run_test_ui(|ui| {
/// # let clips = ["Idle", "Walk"];
/// let mut chosen: Option<usize> = Some(0);
/// let mut list = slerp_theme::widgets::List::new(ui, "clips");
/// let mut pick = None;
/// for (i, name) in clips.iter().enumerate() {
///     if list.row(ui, i, chosen == Some(i), *name).clicked() {
///         pick = Some(i);
///     }
/// }
/// if let Some(i) = list.finish(ui).or(pick) {
///     chosen = Some(i);
/// }
/// # });
/// ```
pub struct List {
    id: Id,
    last: Rows,
    now: Rows,
    /// The row the keyboard chose this frame.
    chosen: Option<usize>,
}

/// Rows a Page Up or Page Down moves.
const PAGE: isize = 10;

impl List {
    /// Starts a list.  Its rows must be told apart by keys, such as their
    /// indices, which stay the same from frame to frame.
    pub fn new(ui: &Ui, id_salt: impl egui::AsIdSalt) -> List {
        let id = ui.make_persistent_id(id_salt);
        let last: Rows = ui.data(|d| d.get_temp(id)).unwrap_or_default();
        let focused = ui.memory(|m| m.focused());
        let mut chosen = None;
        if let Some(at) = last.keys.iter().position(|(_, rid)| Some(*rid) == focused)
            && let Some(step) = keys::take_steps(ui, last.keys[at].1, PAGE)
            && let Some(to) = step.apply(Some(at), last.keys.len())
            && (to != at || last.selected != Some(last.keys[at].0))
        {
            let (key, row) = last.keys[to];
            ui.memory_mut(|m| m.request_focus(row));
            chosen = Some(key);
        }
        List { id, last, now: Rows::default(), chosen }
    }

    /// A row showing `text`, highlighted when `selected`.  Its response is
    /// clicked when a click (or Space with the focus on it) selects it.
    pub fn row(&mut self, ui: &mut Ui, key: usize, selected: bool, text: impl Into<WidgetText>) -> Response {
        let chosen = self.chosen == Some(key);
        // Rows keep their ids from frame to frame, so last frame's says
        // whether this one has the focus before it is drawn.
        let before = self.last.keys.iter().find(|(k, _)| *k == key).map(|(_, id)| *id);
        let focused = before.is_some_and(|id| ui.memory(|m| m.has_focus(id)));
        // One stop for Tab: the selected row, or the first row when none
        // is, and whichever row has the focus.
        let tab_stop = selected || focused || (self.last.selected.is_none() && self.now.keys.is_empty());
        let sense = if tab_stop { Sense::click() } else { Sense::CLICK };
        let response =
            ui.push_id((self.id, key), |ui| ui.add(egui::Button::selectable(selected, text).sense(sense))).inner;
        keys::focus_on_click(&response);
        if chosen {
            response.scroll_to_me(None);
        }
        if response.has_focus() {
            keys::lock_arrows(ui, response.id);
            let p = crate::style::palette(ui.ctx());
            ui.painter().rect_stroke(
                response.rect.shrink(0.5),
                ui.visuals().widgets.hovered.corner_radius,
                Stroke::new(1.0, p.focus),
                egui::StrokeKind::Inside,
            );
        }
        if selected {
            self.now.selected = Some(key);
        }
        self.now.keys.push((key, response.id));
        response
    }

    /// Ends the list.  Returns the row the keyboard selected this frame,
    /// which the caller selects as it would a clicked one.
    pub fn finish(self, ui: &Ui) -> Option<usize> {
        if self.chosen.is_some() {
            ui.ctx().request_repaint();
        }
        ui.data_mut(|d| d.insert_temp(self.id, self.now));
        self.chosen
    }
}

/// Radio buttons for `options`, `current` being the one selected, laid out
/// in the `ui`'s direction (wrap it in `ui.horizontal` for a row).  As in Qt,
/// the group is one stop for Tab: only the selected button takes the focus,
/// a click gives it, and while it has it the arrow keys select the previous
/// or next option.  The response, which covers the whole group, says
/// `changed()` when `current` changed.
pub fn radio_group<V: PartialEq + Clone>(ui: &mut Ui, current: &mut V, options: &[(V, &str)]) -> Response {
    let selected = options.iter().position(|(v, _)| v == current);
    let mut responses = Vec::with_capacity(options.len());
    for (i, (_, label)) in options.iter().enumerate() {
        // The selected option, or the first when none is, is the tab stop.
        let tab_stop = Some(i) == selected || (selected.is_none() && i == 0);
        let r = ui.push_id(i, |ui| super::indicator(ui, (*label).into(), Some(i) == selected, true, tab_stop)).inner;
        responses.push(r);
    }
    let mut changed = false;
    let mut to = None;
    for (i, r) in responses.iter().enumerate() {
        keys::focus_on_click(r);
        if r.clicked() && Some(i) != selected {
            to = Some(i);
        }
        if r.has_focus()
            && let Some(step) = keys::take_steps(ui, r.id, 1)
        {
            to = step.apply(Some(i), options.len()).filter(|&t| t != i).or(to);
        }
    }
    if let Some(t) = to {
        *current = options[t].0.clone();
        responses[t].request_focus();
        changed = true;
        ui.ctx().request_repaint();
    }
    let mut response = responses.into_iter().reduce(|a, b| a.union(b)).unwrap_or_else(|| ui.response());
    if changed {
        response.mark_changed();
    }
    response
}
