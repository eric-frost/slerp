//! Keyboard focus for the controls in [`super`], the way Qt's controls take
//! it: clicking a control gives it the keyboard focus (Qt's `StrongFocus`),
//! and while it has the focus, the keys that operate it are taken out of
//! the frame's input before anything else can act on them.  So the arrow
//! keys step a focused combo box instead of stepping through files or
//! moving the focus to whatever control lies in that direction, which is
//! what egui does with them by default.
//!
//! The mouse wheel over a combo box steps it, as in Qt and KDE.  egui
//! smooths wheel input over several frames before scroll areas see it, and
//! that smoothing can't be undone once it has started, so the wheel events
//! have to be taken before egui reads them: [`wheel_hook`], called from
//! `eframe::App::raw_input_hook`, hands the wheel over a combo box to that
//! combo box, which reads it in the next frame with [`take_wheel`].

use egui::{Context, Event, EventFilter, FocusDirection, Id, Key, MouseWheelUnit, RawInput, Rect, Response, Ui};
use std::sync::Arc;

/// What the keyboard asks a focused control to do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Step {
    /// Move this many items, rows or options; negative is back.
    By(isize),
    First,
    Last,
}

impl Step {
    /// The index `self` leads to from `index` among `len` items, clamped to
    /// the ends, or `None` when there are no items.
    pub(crate) fn apply(self, index: Option<usize>, len: usize) -> Option<usize> {
        let last = len.checked_sub(1)?;
        Some(match self {
            Step::First => 0,
            Step::Last => last,
            Step::By(n) => match index {
                Some(i) => (i as isize).saturating_add(n).clamp(0, last as isize) as usize,
                // Nothing chosen yet: forward starts at the first item, back
                // at the last, as Qt's combo boxes do.
                None if n >= 0 => 0,
                None => last,
            },
        })
    }
}

/// Gives `response`'s control the keyboard focus when it is clicked or a
/// drag on it starts, as Qt does for its controls.
pub(crate) fn focus_on_click(response: &Response) {
    if response.clicked() || response.drag_started() {
        response.request_focus();
    }
}

/// Keeps egui from moving the focus away from the focused control `id`
/// with the arrow keys, which the control uses itself.  Tab and Escape keep
/// moving and dropping the focus as usual.
pub(crate) fn lock_arrows(ui: &Ui, id: Id) {
    let filter = EventFilter { horizontal_arrows: true, vertical_arrows: true, tab: false, escape: false };
    ui.memory_mut(|m| m.set_focus_lock_filter(id, filter));
}

/// Takes the keys that step a control out of this frame's input when the
/// control `id` has the keyboard focus: the arrows (up and left go back),
/// Home and End, and Page Up and Page Down, which move `page` at a time.
/// Keys held with Ctrl, Alt or Shift stay where they are, for shortcuts.
pub(crate) fn take_steps(ui: &Ui, id: Id, page: isize) -> Option<Step> {
    if !ui.memory(|m| m.has_focus(id)) {
        return None;
    }
    lock_arrows(ui, id);
    let mut step = None;
    let mut arrows = false;
    ui.input_mut(|i| {
        i.events.retain(|e| {
            let Event::Key { key, pressed: true, modifiers, .. } = e else { return true };
            if !modifiers.is_none() {
                return true;
            }
            let s = match key {
                Key::ArrowUp | Key::ArrowLeft => Step::By(-1),
                Key::ArrowDown | Key::ArrowRight => Step::By(1),
                Key::PageUp => Step::By(-page),
                Key::PageDown => Step::By(page),
                Key::Home => Step::First,
                Key::End => Step::Last,
                _ => return true,
            };
            arrows |= matches!(key, Key::ArrowUp | Key::ArrowDown | Key::ArrowLeft | Key::ArrowRight);
            step = Some(match (step, s) {
                (Some(Step::By(a)), Step::By(b)) => Step::By(a.saturating_add(b)),
                (_, s) => s,
            });
            false
        });
    });
    if arrows {
        // egui decided where the arrows move the focus when the frame
        // began; the control used them, so the focus stays.
        ui.memory_mut(|m| m.move_focus(FocusDirection::None));
    }
    step
}

/// Paints the focus line Breeze draws under a focused check box's or
/// radio button's label (`Helper::renderFocusLine`).
pub(crate) fn paint_focus_line(ui: &Ui, text_rect: Rect) {
    let p = crate::style::palette(ui.ctx());
    let y = text_rect.bottom().round() + 0.5;
    ui.painter().hline(text_rect.x_range(), y, egui::Stroke::new(1.0, p.focus));
}

/*┌─────────────────╮
| Mouse wheel     |
└─────────────────╯*/
//region Mouse wheel

/// Controls that the mouse wheel steps, as drawn in one pass.
#[derive(Clone, Default)]
struct WheelTargets {
    frame: u64,
    pass: u64,
    rects: Vec<(Id, Rect)>,
}

/// Wheel movement owed to controls, in steps, with the fraction left over
/// from touchpads that scroll by points.
#[derive(Clone, Default)]
struct WheelOwed(Vec<(Id, f32)>);

fn targets_id() -> Id {
    Id::new("slerp_theme::wheel_targets")
}

fn owed_id() -> Id {
    Id::new("slerp_theme::wheel_owed")
}

/// Points of touchpad scrolling that make one step.
const POINTS_PER_STEP: f32 = 50.0;

/// Notes that the mouse wheel over `rect` steps the control `id` (in the
/// next frame, through [`take_wheel`]).
pub(crate) fn wheel_target(ui: &Ui, id: Id, rect: Rect) {
    let (frame, pass) = (ui.ctx().cumulative_frame_nr(), ui.ctx().cumulative_pass_nr());
    ui.data_mut(|d| {
        let t = d.get_temp_mut_or_default::<Arc<WheelTargets>>(targets_id());
        let t = Arc::make_mut(t);
        if t.frame != frame || t.pass != pass {
            *t = WheelTargets { frame, pass, rects: Vec::new() };
        }
        t.rects.push((id, rect));
    });
}

/// The whole steps of wheel movement owed to control `id`: negative when
/// the wheel turned away from the user, which in Qt steps to the previous
/// item.
pub(crate) fn take_wheel(ui: &Ui, id: Id) -> isize {
    ui.data_mut(|d| {
        let owed = d.get_temp_mut_or_default::<WheelOwed>(owed_id());
        let Some(at) = owed.0.iter().position(|(i, _)| *i == id) else { return 0 };
        let total = owed.0[at].1;
        let whole = total.trunc();
        if whole == 0.0 {
            return 0;
        }
        owed.0[at].1 = total - whole;
        whole as isize
    })
}

/// Hands mouse wheel movement over a combo box to the combo box instead of
/// the scroll area around it, as Qt does.  Call it from
/// `eframe::App::raw_input_hook` (or before `Context::run`) with the input
/// for the next frame; combo boxes drawn in the frame before say where they
/// are.  Wheel turns with Ctrl (zoom) or over anything else pass through.
pub fn wheel_hook(ctx: &Context, raw: &mut RawInput) {
    let Some(targets) = ctx.data(|d| d.get_temp::<Arc<WheelTargets>>(targets_id())) else { return };
    // Only the frame that just ended counts: a combo box not drawn in it is
    // gone (a closed panel, another file), whatever stands there now.
    if targets.rects.is_empty() || targets.frame + 1 != ctx.cumulative_frame_nr() {
        return;
    }
    let mut pointer = ctx.input(|i| i.pointer.latest_pos());
    let mut owed: Vec<(Id, f32)> = Vec::new();
    raw.events.retain(|e| match e {
        Event::PointerMoved(p) => {
            pointer = Some(*p);
            true
        }
        Event::PointerGone => {
            pointer = None;
            true
        }
        Event::MouseWheel { unit, delta, modifiers, .. } if !modifiers.command && !modifiers.ctrl => {
            let Some(at) = pointer else { return true };
            let Some(&(id, _)) = targets.rects.iter().rev().find(|(_, r)| r.contains(at)) else { return true };
            let steps = match unit {
                MouseWheelUnit::Line => -delta.y,
                MouseWheelUnit::Point => -delta.y / POINTS_PER_STEP,
                MouseWheelUnit::Page => -delta.y.signum(),
            };
            match owed.iter_mut().find(|(i, _)| *i == id) {
                Some(o) => o.1 += steps,
                None => owed.push((id, steps)),
            }
            false
        }
        _ => true,
    });
    if owed.is_empty() {
        return;
    }
    ctx.data_mut(|d| {
        let all = d.get_temp_mut_or_default::<WheelOwed>(owed_id());
        for (id, steps) in owed {
            match all.0.iter_mut().find(|(i, _)| *i == id) {
                Some(o) => o.1 += steps,
                None => all.0.push((id, steps)),
            }
        }
    });
    ctx.request_repaint();
}

//endregion
