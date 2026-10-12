//! A tall list of rows with folders, drawn only where it can be seen, with
//! the keyboard behaviour of Qt's tree views: the list is one stop for Tab,
//! a click gives it the focus, and while it has the focus the arrows move
//! the current row (with Shift, they extend the selection), Left and Right
//! close and open folders (or go to the parent and into the folder), Page
//! Up, Page Down, Home and End jump, Ctrl+A selects everything, and Enter
//! and Space act on the current row.
//!
//! The widget keeps no selection of its own: the caller says which rows are
//! selected and which is current, and gets back what the user asked for, as
//! [`TreeEvent`]s, to apply however it models the tree.  Rows are made on
//! demand by a closure, so a hundred thousand rows cost only the ones in view.

use super::keys;
use egui::{Align2, Event, FontId, Key, Rect, Response, Sense, Stroke, TextStyle, Ui, pos2, vec2};

/// What to draw for one row.
#[derive(Clone, Debug, Default)]
pub struct TreeRow {
    /// How deep the row sits: 0 is the top level.
    pub depth: usize,
    pub label: String,
    /// `Some(open)` for a folder (a row with children).
    pub folder: Option<bool>,
    /// Weak text in a column before the right edge (a kind).
    pub kind: String,
    /// Weak text at the right edge (a size).
    pub detail: String,
    /// Drawn weaker, for things that hold nothing (markers).
    pub dim: bool,
    pub selected: bool,
    /// A small picture drawn before the label (a texture, a frame), fitted
    /// into a square of the row's height, keeping its shape.  `None` draws
    /// no square, so rows without one keep their labels where they were.
    pub thumb: Option<(egui::TextureId, egui::Vec2)>,
}

/// What the user did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TreeEvent {
    /// A click or a key chose `row`; `extend` means Shift was held (extend
    /// the selection from its anchor) and `toggle` that Ctrl was (add or
    /// remove the row).
    Pick { row: usize, extend: bool, toggle: bool },
    /// A folder's arrow was clicked, or Left or Right asked to close or
    /// open it.
    SetOpen { row: usize, open: bool },
    /// Left on a row that isn't an open folder: go to the folder it is in.
    GoToParent(usize),
    /// A double click, or Enter or Space.
    Activate(usize),
    /// Ctrl+A.
    SelectAll,
    /// A right click.
    Context(usize),
}

/// What [`tree`] returns.
pub struct TreeOutput {
    pub events: Vec<TreeEvent>,
    /// A drag began on this row (for starting a drag out of the window).
    pub drag_started: Option<usize>,
    /// The list's own response.
    pub response: Response,
    /// Whether the list has the keyboard focus (as egui's memory has it,
    /// whether or not the window itself is the active one).
    pub focused: bool,
    /// The rows between these indices (first inclusive, last exclusive) were
    /// drawn, so the caller can tell what is in view.
    pub visible: std::ops::Range<usize>,
    /// The row under the pointer, if any.
    pub hovered: Option<usize>,
}

/// Width of the folder arrow's column and of one level of indentation.
const ARROW: f32 = 16.0;
const INDENT: f32 = 16.0;

/// Draws `count` rows made by `row`, scrolling to `scroll_to` first when
/// given.  `cursor` is the current row, which keys move from and which gets
/// the focus ring.  `id_salt` tells this list from the others in the window.
pub fn tree(
    ui: &mut Ui,
    id_salt: impl egui::AsIdSalt,
    count: usize,
    cursor: Option<usize>,
    scroll_to: Option<usize>,
    mut row: impl FnMut(usize) -> TreeRow,
) -> TreeOutput {
    let id = ui.make_persistent_id(id_salt);
    let palette = crate::style::palette(ui.ctx());
    let row_h = (ui.text_style_height(&TextStyle::Body) + 6.0).max(20.0);
    let mut events = Vec::new();
    let mut drag_started = None;
    let mut hovered = None;
    let area = ui.available_rect_before_wrap();
    let page = ((area.height() / row_h) as isize - 1).max(1);

    // The list as one stop for Tab.  It is registered before its rows, so a
    // click on a row (which gives the list the focus, below) isn't taken for
    // a click elsewhere, which would take the focus away again.
    let response = ui.interact(area, id, Sense::focusable_noninteractive());
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Other, true, "Contents"));

    // 1. The keys, taken from the frame's input while the list has the focus.
    let focused = ui.memory(|m| m.has_focus(id));
    if focused {
        keys::lock_arrows(ui, id);
        let current = cursor.filter(|&c| c < count);
        let current_row = current.map(&mut row);
        let to = |delta: isize, extend: bool| TreeEvent::Pick {
            row: current.map_or(if delta >= 0 { 0 } else { count.saturating_sub(1) }, |c| {
                (c as isize).saturating_add(delta).clamp(0, count.saturating_sub(1) as isize) as usize
            }),
            extend,
            toggle: false,
        };
        let mut moved = false;
        ui.input_mut(|i| {
            i.events.retain(|e| {
                let Event::Key { key, pressed: true, modifiers, .. } = e else { return true };
                if count == 0 || modifiers.alt || (modifiers.command && (*key != Key::A || modifiers.shift)) {
                    return true;
                }
                let extend = modifiers.shift;
                let event = match key {
                    Key::ArrowDown => to(1, extend),
                    Key::ArrowUp => to(-1, extend),
                    Key::PageDown => to(page, extend),
                    Key::PageUp => to(-page, extend),
                    Key::Home => TreeEvent::Pick { row: 0, extend, toggle: false },
                    Key::End => TreeEvent::Pick { row: count - 1, extend, toggle: false },
                    Key::ArrowRight => match (current, current_row.as_ref().and_then(|r| r.folder)) {
                        (Some(c), Some(false)) => TreeEvent::SetOpen { row: c, open: true },
                        (Some(c), Some(true)) if c + 1 < count => TreeEvent::Pick { row: c + 1, extend, toggle: false },
                        _ => return true,
                    },
                    Key::ArrowLeft => match (current, current_row.as_ref().and_then(|r| r.folder)) {
                        (Some(c), Some(true)) => TreeEvent::SetOpen { row: c, open: false },
                        (Some(c), _) => TreeEvent::GoToParent(c),
                        _ => return true,
                    },
                    Key::Enter | Key::Space => match current {
                        Some(c) => match (key, current_row.as_ref().and_then(|r| r.folder)) {
                            (Key::Space, Some(open)) => TreeEvent::SetOpen { row: c, open: !open },
                            _ => TreeEvent::Activate(c),
                        },
                        None => return true,
                    },
                    Key::A if modifiers.command => TreeEvent::SelectAll,
                    _ => return true,
                };
                moved |= matches!(event, TreeEvent::Pick { .. } | TreeEvent::GoToParent(_));
                events.push(event);
                false
            });
        });
        if moved {
            ui.ctx().request_repaint();
        }
    }

    // 2. The rows in view.
    let mut shown = 0..0;
    let outer =
        egui::ScrollArea::vertical().id_salt(id).auto_shrink([false, false]).show_viewport(ui, |ui, viewport| {
            let width = ui.available_width();
            let (content, _) = ui.allocate_exact_size(vec2(width, count as f32 * row_h), Sense::hover());
            let row_rect =
                |i: usize| Rect::from_min_size(content.min + vec2(0.0, i as f32 * row_h), vec2(width, row_h));
            if let Some(i) = scroll_to.filter(|&i| i < count) {
                ui.scroll_to_rect(row_rect(i), None);
            }
            let first = ((viewport.min.y / row_h).floor().max(0.0) as usize).min(count);
            let last = (((viewport.max.y / row_h).ceil().max(0.0) as usize) + 1).min(count);
            shown = first..last;
            let font: FontId = TextStyle::Body.resolve(ui.style());
            let painter = ui.painter_at(ui.clip_rect());
            for i in first..last {
                let r = row(i);
                let rect = row_rect(i);
                let response = ui.interact(rect, id.with(i), Sense::click_and_drag());
                let mods = ui.input(|i| i.modifiers);
                if response.clicked() || response.drag_started() {
                    ui.memory_mut(|m| m.request_focus(id));
                }
                // The arrow's column opens and closes a folder; elsewhere the
                // row is picked.
                let arrow = Rect::from_min_size(
                    pos2(rect.left() + 4.0 + r.depth as f32 * INDENT, rect.top()),
                    vec2(ARROW, row_h),
                );
                let on_arrow = response.interact_pointer_pos().is_some_and(|p| arrow.contains(p));
                if response.clicked() {
                    match r.folder {
                        Some(open) if on_arrow => events.push(TreeEvent::SetOpen { row: i, open: !open }),
                        _ => events.push(TreeEvent::Pick { row: i, extend: mods.shift, toggle: mods.command }),
                    }
                }
                if response.double_clicked() && !on_arrow {
                    events.push(TreeEvent::Activate(i));
                }
                if response.secondary_clicked() {
                    events.push(TreeEvent::Context(i));
                }
                if response.drag_started() && !on_arrow {
                    drag_started = Some(i);
                }
                if response.hovered() {
                    hovered = Some(i);
                }

                // Draw: background, arrow, label, kind, detail.
                let (bg, fg) = if r.selected {
                    (Some(palette.highlight), palette.highlighted_text)
                } else if response.hovered() {
                    (Some(palette.hover_fill.gamma_multiply(0.45)), palette.view_text)
                } else {
                    (None, if r.dim { palette.view_text.gamma_multiply(0.55) } else { palette.view_text })
                };
                if let Some(bg) = bg {
                    painter.rect_filled(rect, 0.0, bg);
                }
                let weak = fg.gamma_multiply(0.65);
                if let Some(open) = r.folder {
                    super::paint_arrow(ui, arrow, fg, if open { 0.0 } else { -std::f32::consts::FRAC_PI_2 });
                }
                let mut text_x = arrow.right() + 2.0;
                if let Some((texture, size)) = r.thumb {
                    let side = row_h - 4.0;
                    let scale = side / size.x.max(size.y).max(1.0);
                    let fitted = vec2(size.x * scale, size.y * scale);
                    let square = Rect::from_min_size(pos2(text_x, rect.top() + 2.0), vec2(side, side));
                    let at = Rect::from_center_size(square.center(), fitted);
                    painter.image(
                        texture,
                        at,
                        Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
                        egui::Color32::WHITE,
                    );
                    text_x += side + 4.0;
                }
                // The detail and kind columns are at least their usual width
                // and grow to fit longer text (a mesh's "48,004 triangles"),
                // up to half the row between them; the label gives way.
                let right = rect.right() - 8.0;
                let width_of = |text: &str| {
                    if text.is_empty() {
                        0.0
                    } else {
                        painter.layout_no_wrap(text.to_string(), font.clone(), weak).size().x
                    }
                };
                let room = (right - text_x) * 0.5;
                let detail_full = width_of(&r.detail);
                let detail_w = detail_full.clamp(84.0, room.max(84.0));
                let kind_full = width_of(&r.kind);
                let kind_w = kind_full.clamp(64.0, (room - detail_w).max(64.0));
                let label_clip =
                    Rect::from_min_max(pos2(text_x, rect.top()), pos2(right - detail_w - kind_w - 12.0, rect.bottom()));
                painter.with_clip_rect(label_clip.intersect(painter.clip_rect())).text(
                    pos2(text_x, rect.center().y),
                    Align2::LEFT_CENTER,
                    &r.label,
                    font.clone(),
                    fg,
                );
                // Right-aligned in its column, or, when it doesn't fit, from
                // the column's start with the end cut off.
                let column = |text: &str, full: f32, end: f32, width: f32| {
                    let clip = Rect::from_min_max(pos2(end - width, rect.top()), pos2(end, rect.bottom()));
                    let (at, align) = if full > width {
                        (pos2(end - width, rect.center().y), Align2::LEFT_CENTER)
                    } else {
                        (pos2(end, rect.center().y), Align2::RIGHT_CENTER)
                    };
                    painter.with_clip_rect(clip.intersect(painter.clip_rect())).text(
                        at,
                        align,
                        text,
                        font.clone(),
                        weak,
                    );
                };
                if !r.kind.is_empty() {
                    column(&r.kind, kind_full, right - detail_w - 6.0, kind_w);
                }
                if !r.detail.is_empty() {
                    column(&r.detail, detail_full, right, detail_w);
                }
                if focused && cursor == Some(i) {
                    painter.rect_stroke(
                        rect.shrink(0.5),
                        0.0,
                        Stroke::new(1.0, palette.focus),
                        egui::StrokeKind::Inside,
                    );
                }
            }
        });

    let _ = outer;
    TreeOutput { events, drag_started, response, focused, visible: shown, hovered }
}
