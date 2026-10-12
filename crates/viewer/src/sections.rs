//! Collapsible panel sections that remember whether they are open: from
//! file to file, because each section is known by its name rather than by
//! where it sits in one document's panel, and from run to run, through the
//! viewer's state file (`Saved::sections`).
//!
//! A section the user never opened or closed follows its default, so a
//! default changed in the code still reaches it.  egui's own memory of a
//! header is overridden every frame from this one, which is what makes the
//! state independent of egui's ids.

use egui::{CollapsingHeader, CollapsingResponse, Id, Ui, WidgetText};
use std::collections::BTreeMap;

/// The sections the user opened (`true`) or closed (`false`), by name, kept
/// in egui's context so the panel code reaches it through any `Ui`.
#[derive(Clone, Debug, Default)]
struct Memory {
    open: BTreeMap<String, bool>,
    changed: bool,
}

fn memory_id() -> Id {
    Id::new("slerp-sections")
}

/// Starts from what the state file remembered.
pub fn load(ctx: &egui::Context, open: BTreeMap<String, bool>) {
    ctx.data_mut(|d| d.insert_temp(memory_id(), Memory { open, changed: false }));
}

/// Everything remembered, if the user opened or closed a section since the
/// last call, for saving.
pub fn take_changes(ctx: &egui::Context) -> Option<BTreeMap<String, bool>> {
    ctx.data_mut(|d| {
        let m = d.get_temp_mut_or_default::<Memory>(memory_id());
        std::mem::take(&mut m.changed).then(|| m.open.clone())
    })
}

/// Whether the section `key` shows open: as the user left it, else
/// `default_open`.
pub fn is_open(ctx: &egui::Context, key: &str, default_open: bool) -> bool {
    ctx.data_mut(|d| d.get_temp_mut_or_default::<Memory>(memory_id()).open.get(key).copied()).unwrap_or(default_open)
}

fn set_open(ctx: &egui::Context, key: &str, open: bool) {
    ctx.data_mut(|d| {
        let m = d.get_temp_mut_or_default::<Memory>(memory_id());
        m.open.insert(key.to_string(), open);
        m.changed = true;
    });
}

/// A collapsible section with the theme's arrow, remembered as `key`.
/// Nested sections use keys such as `"Face/Brows"`, so they don't share
/// their state with a top-level section of the same name.
pub fn section<R>(
    ui: &mut Ui,
    key: &str,
    title: impl Into<WidgetText>,
    default_open: bool,
    add: impl FnOnce(&mut Ui) -> R,
) -> CollapsingResponse<R> {
    let ctx = ui.ctx().clone();
    let open = is_open(&ctx, key, default_open);
    // The default only matters the first time egui sees the header: it
    // then appears as it should rather than animating there.
    let response = CollapsingHeader::new(title)
        .id_salt(("slerp-section", key))
        .default_open(open)
        .open(Some(open))
        .icon(slerp_theme::widgets::collapsing_icon)
        .show(ui, add);
    // With the state given, egui leaves clicks to the caller; the new
    // state shows from the next frame.
    if response.header_response.clicked() {
        set_open(&ctx, key, !open);
        ctx.request_repaint();
    }
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remembers_what_the_user_changed_and_round_trips_through_the_state_file() {
        let ctx = egui::Context::default();
        load(&ctx, BTreeMap::from([("Lighting".to_string(), false)]));
        assert!(!is_open(&ctx, "Lighting", true), "remembered");
        assert!(is_open(&ctx, "Camera", true), "default");
        assert_eq!(take_changes(&ctx), None);
        set_open(&ctx, "Face/Brows", false);
        let saved = take_changes(&ctx).expect("changed");
        assert_eq!(take_changes(&ctx), None, "taken once");
        // Saved, read back in another run.
        let mut state = crate::saved::Saved { sections: saved, ..Default::default() };
        state = serde_json::from_str(&serde_json::to_string(&state).unwrap()).unwrap();
        let ctx = egui::Context::default();
        load(&ctx, state.sections);
        assert!(!is_open(&ctx, "Face/Brows", true));
        assert!(!is_open(&ctx, "Lighting", true));
        assert!(!is_open(&ctx, "Brows", false));
    }

    #[test]
    fn a_click_on_the_header_flips_it_from_the_next_frame() {
        let ctx = egui::Context::default();
        load(&ctx, BTreeMap::new());
        let mut header = egui::Rect::NOTHING;
        let mut shown = Vec::new();
        let frame = |events: Vec<egui::Event>, shown: &mut Vec<bool>, header: &mut egui::Rect| {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(400.0, 400.0))),
                events,
                ..Default::default()
            };
            let _ = ctx.run_ui(input, |ui| {
                let r = section(ui, "Statistics", "Statistics", true, |_| ());
                *header = r.header_response.rect;
                shown.push(r.body_returned.is_some());
            });
        };
        frame(Vec::new(), &mut shown, &mut header);
        let at = header.center();
        let click = |pressed| egui::Event::PointerButton {
            pos: at,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        frame(vec![egui::Event::PointerMoved(at), click(true)], &mut shown, &mut header);
        frame(vec![click(false)], &mut shown, &mut header);
        // Closing animates, so give it frames.
        for _ in 0..30 {
            frame(Vec::new(), &mut shown, &mut header);
        }
        assert!(shown[0], "open by default");
        assert!(!shown.last().unwrap(), "closed after the click");
        assert_eq!(take_changes(&ctx), Some(BTreeMap::from([("Statistics".to_string(), false)])));
    }
}
