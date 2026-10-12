//! The form controls at the keyboard and mouse wheel, driven through egui
//! input events frame by frame.

use super::{CheckBox, Choice, List, TreeEvent, TreeRow, radio_group, slider, tree, wheel_hook};
use egui::{Context, Event, Key, Modifiers, MouseWheelUnit, PointerButton, Pos2, RawInput, Rect, Ui, pos2, vec2};

/// One window of a fixed size, run a frame at a time.
struct Window {
    ctx: Context,
}

impl Window {
    fn new() -> Window {
        let ctx = Context::default();
        ctx.set_fonts(egui::FontDefinitions::default());
        Window { ctx }
    }

    /// Runs one frame with `events`, after the wheel hook, as the viewer
    /// does.
    fn frame(&self, events: Vec<Event>, mut ui_fn: impl FnMut(&mut Ui)) {
        // The modifiers held come from the events that carry them, as the
        // windowing layer would report them.
        let modifiers = events
            .iter()
            .find_map(|e| match e {
                Event::PointerButton { modifiers, .. } | Event::Key { modifiers, .. } => Some(*modifiers),
                _ => None,
            })
            .unwrap_or_default();
        let mut raw = RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(800.0, 600.0))),
            events,
            modifiers,
            ..Default::default()
        };
        wheel_hook(&self.ctx, &mut raw);
        let _ = self.ctx.run_ui(raw, |ui| ui_fn(ui));
    }

    fn focused(&self) -> Option<egui::Id> {
        self.ctx.memory(|m| m.focused())
    }
}

fn key(k: Key) -> Event {
    Event::Key { key: k, physical_key: None, pressed: true, repeat: false, modifiers: Modifiers::NONE }
}

fn click(at: Pos2) -> Vec<Event> {
    let button =
        |pressed| Event::PointerButton { pos: at, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE };
    vec![Event::PointerMoved(at), button(true), button(false)]
}

const MAPS: [&str; 4] = ["MAP01", "MAP02", "MAP03", "MAP04"];

fn maps() -> Vec<(usize, String)> {
    MAPS.iter().enumerate().map(|(i, m)| (i, m.to_string())).collect()
}

/// Shows the map picker for a frame: its rectangle, and how many key
/// presses the rest of the frame still finds.
fn picker(w: &Window, events: Vec<Event>, map: &mut usize) -> (Rect, usize) {
    let mut out = (Rect::NOTHING, 0);
    w.frame(events, |ui| {
        let r = Choice::new("maps", map, maps()).width(160.0).show(ui);
        let left = ui.input(|i| i.events.iter().filter(|e| matches!(e, Event::Key { pressed: true, .. })).count());
        out = (r.rect, left);
    });
    out
}

/// A focused combo box steps with the arrows, Home and End without opening
/// its list, and keeps the keys from the rest of the frame.
#[test]
fn focused_combo_box_steps_with_the_keys() {
    let w = Window::new();
    let mut map = 0usize;
    let (rect, _) = picker(&w, Vec::new(), &mut map);
    // Unfocused, the arrows are left alone.
    let (_, left) = picker(&w, vec![key(Key::ArrowDown)], &mut map);
    assert_eq!((map, left), (0, 1));

    // A click opens the list and focuses the box.
    picker(&w, click(rect.center()), &mut map);
    let id = w.focused().expect("a click focuses the combo box");
    egui::Popup::close_all(&w.ctx);
    picker(&w, Vec::new(), &mut map);
    assert_eq!(w.focused(), Some(id));

    let (_, left) = picker(&w, vec![key(Key::ArrowDown)], &mut map);
    assert_eq!((map, left), (1, 0), "the combo box took the key");
    picker(&w, vec![key(Key::ArrowRight), key(Key::ArrowRight)], &mut map);
    assert_eq!(map, 3);
    picker(&w, vec![key(Key::ArrowDown)], &mut map);
    assert_eq!(map, 3, "stops at the last item");
    picker(&w, vec![key(Key::Home)], &mut map);
    assert_eq!(map, 0);
    picker(&w, vec![key(Key::End)], &mut map);
    assert_eq!(map, 3);
    picker(&w, vec![key(Key::ArrowUp)], &mut map);
    assert_eq!(map, 2);
    assert_eq!(w.focused(), Some(id), "the arrows don't move the focus away");
    assert!(!egui::ComboBox::is_open(&w.ctx, id), "stepping doesn't open the list");

    // Keys with modifiers stay for the window's shortcuts.
    let alt =
        Event::Key { key: Key::ArrowLeft, physical_key: None, pressed: true, repeat: false, modifiers: Modifiers::ALT };
    let (_, left) = picker(&w, vec![alt], &mut map);
    assert_eq!((map, left), (2, 1));
}

/// Choosing from the open list hands the focus back to the box, so the
/// arrows carry on from the map chosen.
#[test]
fn choosing_from_the_list_keeps_the_focus() {
    let w = Window::new();
    let mut map = 0usize;
    let (rect, _) = picker(&w, Vec::new(), &mut map);
    picker(&w, click(rect.center()), &mut map);
    picker(&w, Vec::new(), &mut map);
    // The list opens under the box, one row per map.
    let third = pos2(rect.left() + 20.0, rect.bottom() + 2.5 * rect.height());
    picker(&w, click(third), &mut map);
    assert!(map > 0, "a map was chosen from the list");
    let chosen = map;
    picker(&w, Vec::new(), &mut map);
    picker(&w, vec![key(Key::ArrowDown)], &mut map);
    assert_eq!(map, (chosen + 1).min(3), "the arrows go on from the map chosen");
}

/// The mouse wheel over a combo box steps it, and over anything else
/// passes through to scroll.
#[test]
fn wheel_over_combo_box_steps_it() {
    let w = Window::new();
    let mut map = 1usize;
    let wheel = |lines: f32| Event::MouseWheel {
        unit: MouseWheelUnit::Line,
        delta: vec2(0.0, lines),
        phase: egui::TouchPhase::Move,
        modifiers: Modifiers::NONE,
    };
    let run = |events: Vec<Event>, map: &mut usize| -> (Rect, usize) {
        let mut out = (Rect::NOTHING, 0);
        w.frame(events, |ui| {
            let rect = Choice::new("maps", map, maps()).width(160.0).show(ui).rect;
            let left = ui.input(|i| i.raw.events.iter().filter(|e| matches!(e, Event::MouseWheel { .. })).count());
            out = (rect, left);
        });
        out
    };
    let (rect, _) = run(Vec::new(), &mut map);
    // Towards the user: the next item.
    let (_, left) = run(vec![Event::PointerMoved(rect.center()), wheel(-1.0)], &mut map);
    assert_eq!((map, left), (2, 0), "the combo box steps, and the scroll area around it gets nothing");
    run(vec![wheel(1.0), wheel(1.0)], &mut map);
    assert_eq!(map, 0);
    // Elsewhere the wheel scrolls as usual.
    let (_, left) = run(vec![Event::PointerMoved(pos2(700.0, 500.0)), wheel(-1.0)], &mut map);
    assert_eq!((map, left), (0, 1));
}

/// A focused slider steps with every arrow, Page Up and Page Down move it
/// further, and Home and End reach its ends.
#[test]
fn focused_slider_takes_arrows_and_pages() {
    let w = Window::new();
    let mut value = 50.0f32;
    let run = |events: Vec<Event>, value: &mut f32| -> Rect {
        let mut rect = Rect::NOTHING;
        w.frame(events, |ui| {
            ui.spacing_mut().slider_width = 200.0;
            rect = slider(ui, egui::Slider::new(value, 0.0..=100.0)).rect;
        });
        rect
    };
    let rect = run(Vec::new(), &mut value);
    run(vec![key(Key::ArrowUp)], &mut value);
    assert_eq!(value, 50.0, "unfocused, the keys are left alone");
    // A click quick enough to press and release within one frame focuses
    // it too.
    run(click(pos2(rect.left() + 100.0, rect.center().y)), &mut value);
    run(Vec::new(), &mut value);
    assert!(w.focused().is_some(), "a quick click focuses the slider");
    // Pressing on the slider focuses it (and moves it to the press); the
    // press and the release come in frames of their own, as from a mouse.
    let mut press = click(pos2(rect.left() + 100.0, rect.center().y));
    let release = press.pop().unwrap();
    run(press, &mut value);
    run(vec![release], &mut value);
    run(Vec::new(), &mut value);
    assert!(w.focused().is_some(), "a press focuses the slider");
    let start = value;
    run(vec![key(Key::ArrowUp)], &mut value);
    assert!(value > start, "Up steps up");
    let up = value;
    run(vec![key(Key::ArrowDown), key(Key::ArrowDown)], &mut value);
    assert!(value < up, "Down steps down");
    let small = up - value;
    let before = value;
    run(vec![key(Key::PageUp)], &mut value);
    assert!(value - before > small * 3.0, "Page Up takes a bigger step: {before} to {value}");
    run(vec![key(Key::End)], &mut value);
    assert_eq!(value, 100.0);
    run(vec![key(Key::Home)], &mut value);
    assert_eq!(value, 0.0);
    assert!(w.focused().is_some(), "the vertical arrows don't move the focus away");
}

/// In a list, the arrows move the selection once a row has the focus, and
/// only one row at a time is a stop for Tab.
#[test]
fn list_arrows_move_the_selection() {
    let w = Window::new();
    let mut selected = Some(1usize);
    let run = |events: Vec<Event>, selected: &mut Option<usize>| -> Vec<Rect> {
        let mut rects = Vec::new();
        w.frame(events, |ui| {
            rects.clear();
            let mut list = List::new(ui, "clips");
            let mut pick = None;
            for i in 0..5 {
                let resp = list.row(ui, i, *selected == Some(i), format!("Clip {i}"));
                if resp.clicked() {
                    pick = Some(i);
                }
                rects.push(resp.rect);
            }
            if let Some(i) = list.finish(ui).or(pick) {
                *selected = Some(i);
            }
            let _ = ui.button("After the list");
        });
        rects
    };
    let rects = run(Vec::new(), &mut selected);
    run(vec![key(Key::ArrowDown)], &mut selected);
    assert_eq!(selected, Some(1), "no row has the focus yet");
    run(click(rects[2].center()), &mut selected);
    assert_eq!(selected, Some(2));
    for want in [3, 4, 4] {
        run(Vec::new(), &mut selected);
        run(vec![key(Key::ArrowDown)], &mut selected);
        assert_eq!(selected, Some(want));
    }
    run(vec![key(Key::Home)], &mut selected);
    assert_eq!(selected, Some(0));
    run(Vec::new(), &mut selected);
    run(vec![key(Key::PageDown)], &mut selected);
    assert_eq!(selected, Some(4));

    // Tab from the selected row leaves the list rather than walking its
    // rows.
    run(Vec::new(), &mut selected);
    let in_list = w.focused();
    run(vec![key(Key::Tab)], &mut selected);
    run(Vec::new(), &mut selected);
    let after = w.focused();
    assert!(after.is_some() && after != in_list, "Tab moved on");
    run(vec![key(Key::Tab)], &mut selected);
    run(Vec::new(), &mut selected);
    assert_eq!(w.focused(), in_list, "Tab comes back to the selected row, past the others");
    assert_eq!(selected, Some(4));
}

/// The arrows move a radio group's selection.
#[test]
fn radio_group_arrows_move_between_options() {
    let w = Window::new();
    let mut tonemap = "Neutral";
    let options = [("Neutral", "Neutral"), ("ACES", "ACES"), ("None", "None")];
    let run = |events: Vec<Event>, tonemap: &mut &str| -> Rect {
        let mut rect = Rect::NOTHING;
        w.frame(events, |ui| {
            ui.horizontal(|ui| rect = radio_group(ui, tonemap, &options).rect);
        });
        rect
    };
    let rect = run(Vec::new(), &mut tonemap);
    // A click on the first button focuses the group.
    run(click(pos2(rect.left() + 8.0, rect.center().y)), &mut tonemap);
    run(Vec::new(), &mut tonemap);
    for (k, want) in
        [(Key::ArrowRight, "ACES"), (Key::ArrowDown, "None"), (Key::ArrowDown, "None"), (Key::ArrowLeft, "ACES")]
    {
        run(vec![key(k)], &mut tonemap);
        assert_eq!(tonemap, want);
        run(Vec::new(), &mut tonemap);
    }
}

/// Space toggles a focused check box, and a click focuses it.
#[test]
fn space_toggles_a_focused_check_box() {
    let w = Window::new();
    let mut on = false;
    let run = |events: Vec<Event>, on: &mut bool| -> Rect {
        let mut rect = Rect::NOTHING;
        w.frame(events, |ui| rect = ui.add(CheckBox::new(on, "Ground grid")).rect);
        rect
    };
    let rect = run(Vec::new(), &mut on);
    run(vec![key(Key::Space)], &mut on);
    assert!(!on, "unfocused, Space does nothing");
    run(click(rect.center()), &mut on);
    assert!(on);
    run(Vec::new(), &mut on);
    run(vec![key(Key::Space)], &mut on);
    assert!(!on);
}

/*┌─────────────────╮
| Tree            |
└─────────────────╯*/
//region Tree

fn tree_row(i: usize) -> TreeRow {
    // Row 0 is an open folder, 1 and 2 are its items, 3 is a closed folder.
    TreeRow {
        depth: usize::from(i == 1 || i == 2),
        label: format!("row {i}"),
        folder: match i {
            0 => Some(true),
            3 => Some(false),
            _ => None,
        },
        kind: "kind".into(),
        detail: "10".into(),
        ..Default::default()
    }
}

/// Shows a six-row tree for a frame: its events, rectangle and the key
/// presses left over.
fn show_tree(w: &Window, events: Vec<Event>, cursor: Option<usize>) -> (Vec<TreeEvent>, Rect, usize) {
    let mut out = (Vec::new(), Rect::NOTHING, 0);
    w.frame(events, |ui| {
        let o = tree(ui, "t", 6, cursor, None, tree_row);
        let left = ui.input(|i| i.events.iter().filter(|e| matches!(e, Event::Key { pressed: true, .. })).count());
        out = (o.events, o.response.rect, left);
    });
    out
}

fn row_center(rect: Rect, i: usize) -> Pos2 {
    let row_h = (Window::new().ctx.global_style().text_styles[&egui::TextStyle::Body].size + 6.0).max(20.0);
    pos2(rect.left() + 120.0, rect.top() + (i as f32 + 0.5) * row_h)
}

fn key_with(k: Key, modifiers: Modifiers) -> Event {
    Event::Key { key: k, physical_key: None, pressed: true, repeat: false, modifiers }
}

#[test]
fn tree_clicks_and_keys() {
    let w = Window::new();
    show_tree(&w, Vec::new(), None);
    let (_, rect, _) = show_tree(&w, Vec::new(), None);
    // Unfocused, the arrows are left alone.
    let (events, _, left) = show_tree(&w, vec![key(Key::ArrowDown)], Some(1));
    assert_eq!((events, left), (Vec::new(), 1));

    // A click picks the row and focuses the tree.
    let (events, _, _) = show_tree(&w, click(row_center(rect, 2)), None);
    assert_eq!(events, [TreeEvent::Pick { row: 2, extend: false, toggle: false }]);
    assert!(w.focused().is_some());

    // The keys move from the current row, without reaching the rest of the frame.
    let (events, _, left) = show_tree(&w, vec![key(Key::ArrowDown)], Some(2));
    assert_eq!((events, left), (vec![TreeEvent::Pick { row: 3, extend: false, toggle: false }], 0));
    let shift = Modifiers::SHIFT;
    let (events, _, _) = show_tree(&w, vec![key_with(Key::ArrowUp, shift)], Some(2));
    assert_eq!(events, [TreeEvent::Pick { row: 1, extend: true, toggle: false }]);
    let (events, _, _) = show_tree(&w, vec![key(Key::Home), key(Key::End)], Some(2));
    assert_eq!(
        events,
        [
            TreeEvent::Pick { row: 0, extend: false, toggle: false },
            TreeEvent::Pick { row: 5, extend: false, toggle: false }
        ]
    );
    // Stops at the ends.
    let (events, _, _) = show_tree(&w, vec![key(Key::ArrowDown)], Some(5));
    assert_eq!(events, [TreeEvent::Pick { row: 5, extend: false, toggle: false }]);
    // Right opens a closed folder, Left closes an open one or goes to the parent.
    let (events, _, _) = show_tree(&w, vec![key(Key::ArrowRight)], Some(3));
    assert_eq!(events, [TreeEvent::SetOpen { row: 3, open: true }]);
    let (events, _, _) = show_tree(&w, vec![key(Key::ArrowLeft)], Some(0));
    assert_eq!(events, [TreeEvent::SetOpen { row: 0, open: false }]);
    let (events, _, _) = show_tree(&w, vec![key(Key::ArrowLeft)], Some(2));
    assert_eq!(events, [TreeEvent::GoToParent(2)]);
    // Right on an open folder goes into it.
    let (events, _, _) = show_tree(&w, vec![key(Key::ArrowRight)], Some(0));
    assert_eq!(events, [TreeEvent::Pick { row: 1, extend: false, toggle: false }]);
    // Enter and Space act; Space on a folder opens or closes it; Ctrl+A selects all.
    let (events, _, _) = show_tree(&w, vec![key(Key::Enter), key(Key::Space)], Some(1));
    assert_eq!(events, [TreeEvent::Activate(1), TreeEvent::Activate(1)]);
    let (events, _, _) = show_tree(&w, vec![key(Key::Space)], Some(3));
    assert_eq!(events, [TreeEvent::SetOpen { row: 3, open: true }]);
    let (events, _, left) = show_tree(&w, vec![key_with(Key::A, Modifiers::COMMAND)], Some(1));
    assert_eq!((events, left), (vec![TreeEvent::SelectAll], 0));
    // Ctrl+C is left for the application.
    let (events, _, left) = show_tree(&w, vec![key_with(Key::C, Modifiers::COMMAND)], Some(1));
    assert_eq!((events, left), (Vec::new(), 1));
}

#[test]
fn tree_clicks_with_modifiers_and_on_arrows() {
    let w = Window::new();
    let (_, rect, _) = show_tree(&w, Vec::new(), None);
    let click_with = |at: Pos2, modifiers: Modifiers| {
        let button = |pressed| Event::PointerButton { pos: at, button: PointerButton::Primary, pressed, modifiers };
        vec![Event::PointerMoved(at), button(true), button(false)]
    };
    let (events, _, _) = show_tree(&w, click_with(row_center(rect, 4), Modifiers::SHIFT), None);
    assert_eq!(events, [TreeEvent::Pick { row: 4, extend: true, toggle: false }]);
    // The folder's arrow column, at the left of its row, opens or closes it.
    let arrow = pos2(rect.left() + 4.0 + 8.0, row_center(rect, 3).y);
    let (events, _, _) = show_tree(&w, click(arrow), None);
    assert_eq!(events, [TreeEvent::SetOpen { row: 3, open: true }]);
}

//endregion
