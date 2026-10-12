//! Drag-out for the Contents panel: when the user drags entries out of the
//! tree, they are handed to [`crate::dragout`], which asks for their files
//! only when a drop target wants them.  The panel itself only notes which
//! items the drag began with (`contents_view::take_drag`); this file turns
//! them into drag items and reports what the window system said.
//!
//! This module is a child of `app`, like `contents_view`.

use super::{Viewer, contents_view};
use crate::dragout::{self, Drag, DragItem, Outcome};
use std::sync::atomic::{AtomicBool, Ordering};

/// Whether `dragout::prepare` has run for the window system connection in use.
static PREPARED: AtomicBool = AtomicBool::new(false);

/// The window is closing: stops drag-out's thread before the connection to
/// the window system goes, and lets the next window prepare again.
pub fn closed() {
    dragout::shutdown();
    PREPARED.store(false, Ordering::Release);
}

/// Called on every frame after the panel is drawn.
pub fn run(v: &mut Viewer, ctx: &egui::Context, frame: &crate::shell::Frame) {
    if v.contents.is_none() {
        return;
    }
    // 1. Getting ready before the first press: Wayland only lets a drag start
    //    from a press it watched happen.  Once per window; an X11 session
    //    answers with `Unsupported` here and again when a drag starts.
    if !PREPARED.swap(true, Ordering::AcqRel) {
        let _ = dragout::prepare(frame, ctx);
    }

    // 2. How the last drag ended.  A drop needs no message (the file manager
    //    shows its own); the notice is for a drag that went nowhere.
    if let Some(Outcome::Cancelled) = dragout::take_outcome() {
        v.notify("Drag cancelled; nothing was copied");
    }

    // 3. A drag began on some items.  Each is written to the panel's
    //    temporary folder in the panel's chosen format, one at a time, when a
    //    drop target first asks (`contents_view::materialize`).  Items that
    //    can't stand alone (a camera, a rig control) aren't dragged.
    let Some((ids, options)) = contents_view::take_drag(v) else { return };
    let Some(state) = v.contents.as_ref() else { return };
    let contents = state.contents.clone();
    let items: Vec<DragItem> = ids
        .into_iter()
        .filter_map(|id| {
            let name = contents.file_name(id, options.format)?;
            let label = name.clone();
            let options = options.clone();
            Some(DragItem::from_path(name, move || {
                contents_view::materialize(&[id], &options)
                    .into_iter()
                    .next()
                    .ok_or_else(|| std::io::Error::other(format!("{label} couldn't be written")))
            }))
        })
        .collect();
    if let Err(e) = dragout::start(frame, Drag::new(items)) {
        v.warn(e.to_string());
    }
}
