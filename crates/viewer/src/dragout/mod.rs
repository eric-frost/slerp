//! Dragging items out of a Slerp window into a file manager, or any program
//! that takes files, where each lands as an ordinary file.  The Contents
//! panel uses it to drag the entries of a WAD, PK3 or PAK (later the pieces
//! of a glTF) into Dolphin, Explorer or Finder.
//!
//! winit can drag files *into* a window but has no call to start a drag
//! *out* of one, so each platform does it with its own window-system API:
//!
//! - **Wayland** (`wayland.rs`): a `wl_data_source` offering `text/uri-list`
//!   and `text/plain`, started with `wl_data_device.start_drag`.  winit's own
//!   connection is reused (the `wl_display` pointer comes from
//!   `raw-window-handle`, as `smithay-clipboard` does for the clipboard), with
//!   an event queue and a thread of our own.  Wayland only lets a drag start
//!   from a button press it has told the client about, with that press's
//!   serial number, which winit doesn't pass on; a second `wl_pointer` on
//!   our queue receives the same events and keeps the serial.  That is why
//!   [`prepare`] should be called as soon as the window exists.  X11
//!   sessions aren't supported (`DragError::Unsupported`).
//! - **Windows** (`ole.rs`): OLE's `DoDragDrop` with an `IDataObject` that
//!   makes the files when the drop target asks for `CF_HDROP`.  The call
//!   returns when the drag is over.
//! - **macOS** (`appkit.rs`): an `NSDraggingSession` of `NSFilePromiseProvider`
//!   items, so Finder asks for each file as it is dropped.
//!
//! # Using it
//!
//! ```ignore
//! // Once, when the window exists (cheap, and safe to repeat):
//! let _ = dragout::prepare(frame, ctx);
//!
//! // When the user has pressed on an item and moved the pointer a few pixels:
//! let items = names.into_iter().map(|name| DragItem::from_write(name, |dest| entry.save(dest)));
//! match dragout::start(frame, items.collect::<Vec<_>>()) {
//!     Ok(()) => {}
//!     Err(e) => status = e.to_string(),   // Unsupported says "use Ctrl+C and paste instead"
//! }
//!
//! // In the viewer's `raw_input_hook`, every frame: after the window system took
//! // the pointer, egui never sees the button go up.
//! raw_input.events.extend(dragout::take_pointer_release(ctx));
//! ```
//!
//! Items hand over their files lazily ([`DragItem`]): nothing is written until
//! a drop target asks for the data.  On Wayland that happens when the pointer
//! first reaches a window that reads the list (Dolphin does it while hovering,
//! to see whether the files already are in the folder), not only at the drop;
//! on Windows when the target reads `CF_HDROP`; on macOS when Finder asks for
//! each file.  A drag that is cancelled before any target asked writes nothing.
//!
//! Files that drag-out writes go in a private folder of the system's temporary
//! folder, deleted when the drag is cancelled and ten minutes after a drop
//! (the target may still be copying), or by the next run's sweep.
//!
//! One drag at a time: a second [`start`] while one is under way returns
//! [`DragError::Busy`].

mod payload;

#[cfg(target_os = "linux")]
mod wayland;
#[cfg(target_os = "linux")]
use wayland as platform;

#[cfg(windows)]
mod ole;
#[cfg(windows)]
use ole as platform;
// The Windows byte-level code needs no Windows API, so its tests run here too.
#[cfg(all(test, not(windows)))]
#[path = "ole/bytes.rs"]
mod ole_bytes;

#[cfg(target_os = "macos")]
mod appkit;
#[cfg(target_os = "macos")]
use appkit as platform;

#[cfg(not(any(target_os = "linux", windows, target_os = "macos")))]
mod platform {
    use super::*;

    pub fn prepare(_: RawWindowHandle, _: RawDisplayHandle) -> Result<(), DragError> {
        Err(DragError::Unsupported("this platform".into()))
    }

    pub fn start(_: RawWindowHandle, _: RawDisplayHandle, _: Drag) -> Result<(), DragError> {
        Err(DragError::Unsupported("this platform".into()))
    }
}

pub use payload::{Drag, DragError, DragIcon, DragItem, Outcome};

use raw_window_handle::{HasDisplayHandle, HasWindowHandle, RawDisplayHandle, RawWindowHandle};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

/// How the last drag ended, until [`take_outcome`] collects it.
static OUTCOME: Mutex<Option<Outcome>> = Mutex::new(None);

/// Set when a drag started and egui hasn't been told its buttons went up.
static RELEASE_PENDING: AtomicBool = AtomicBool::new(false);

/// The egui context to wake when a drag starts or ends: an idle window draws
/// no frames, so nothing else would notice the end of a drag that happened
/// elsewhere on the screen.
static CONTEXT: Mutex<Option<egui::Context>> = Mutex::new(None);

/// Gets the window system ready for drags out of `window`.  Cheap, and safe
/// to call again.  Wayland needs this before the user presses the button,
/// since it learns which press started the drag by watching presses; a
/// first drag without it fails with [`DragError::NoGesture`].  `ctx` is
/// asked for a new frame when a drag starts and when it ends.
pub fn prepare(window: &(impl HasWindowHandle + HasDisplayHandle), ctx: &egui::Context) -> Result<(), DragError> {
    *CONTEXT.lock().unwrap_or_else(|e| e.into_inner()) = Some(ctx.clone());
    let (win, display) = handles(window)?;
    platform::prepare(win, display)
}

/// Starts dragging `items` out of `window`, with the mouse button the user
/// is holding.  Call it while the button is down (when the pointer has moved
/// a few pixels after the press).  Wayland and macOS return at once and the
/// drag proceeds on its own; Windows returns when it is over.  Either way,
/// follow with [`take_pointer_release`].
pub fn start(window: &(impl HasWindowHandle + HasDisplayHandle), drag: impl Into<Drag>) -> Result<(), DragError> {
    let drag = drag.into();
    if drag.items.is_empty() {
        return Err(DragError::NoItems);
    }
    let (win, display) = handles(window)?;
    platform::start(win, display, drag)?;
    RELEASE_PENDING.store(true, Ordering::Release);
    wake();
    Ok(())
}

/// Stops what [`prepare`] started (Wayland's thread), for the end of the
/// event loop, before winit closes its connection.  Harmless elsewhere, and a
/// later [`prepare`] or [`start`] starts again.
pub fn shutdown() {
    #[cfg(target_os = "linux")]
    platform::shutdown();
}

/// Whether a drag started by [`start`] is still under way.
pub fn is_active() -> bool {
    payload::ActiveGuard::is_held()
}

/// How the last drag ended, once, so the interface can say something such as
/// "Copied 3 files" or leave the status alone.
pub fn take_outcome() -> Option<Outcome> {
    OUTCOME.lock().unwrap_or_else(|e| e.into_inner()).take()
}

/// The release events egui needs after a drag started.  The window system
/// takes the pointer for the drag and the button goes up inside it, so the
/// window never sees the release and egui would think the button is still
/// down (the next hover would carry on dragging the item, or turn the
/// camera).  Call from `raw_input_hook` on every frame and add the events to
/// `raw_input.events`; it returns nothing except on the first frame after a
/// successful [`start`].
pub fn take_pointer_release(ctx: &egui::Context) -> Vec<egui::Event> {
    if !RELEASE_PENDING.swap(false, Ordering::AcqRel) {
        return Vec::new();
    }
    ctx.input(|i| {
        let pos = i.pointer.latest_pos().or(i.pointer.hover_pos()).unwrap_or_default();
        [
            (i.pointer.primary_down(), egui::PointerButton::Primary),
            (i.pointer.secondary_down(), egui::PointerButton::Secondary),
            (i.pointer.middle_down(), egui::PointerButton::Middle),
        ]
        .into_iter()
        .filter(|(down, _)| *down)
        .map(|(_, button)| egui::Event::PointerButton { pos, button, pressed: false, modifiers: i.modifiers })
        .collect()
    })
}

/// Records how a drag ended (for the platform code).
pub(crate) fn report(outcome: Outcome) {
    *OUTCOME.lock().unwrap_or_else(|e| e.into_inner()) = Some(outcome);
    wake();
}

fn wake() {
    if let Some(ctx) = CONTEXT.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
        ctx.request_repaint();
    }
}

fn handles(
    window: &(impl HasWindowHandle + HasDisplayHandle),
) -> Result<(RawWindowHandle, RawDisplayHandle), DragError> {
    let win = window.window_handle().map_err(|e| DragError::Platform(e.to_string()))?.as_raw();
    let display = window.display_handle().map_err(|e| DragError::Platform(e.to_string()))?.as_raw();
    Ok((win, display))
}
