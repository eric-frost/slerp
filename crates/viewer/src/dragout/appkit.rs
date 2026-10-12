//! Drag-out on macOS, with AppKit's file promises.
//!
//! A drag starts with `-[NSView beginDraggingSessionWithItems:event:source:]`
//! on winit's content view.  Each item is an `NSFilePromiseProvider`: the
//! pasteboard holds only a promise ("a file named X, which I will write when
//! you ask"), and Finder, or any other app that takes file promises, asks for
//! it after the drop by calling the delegate's
//! `filePromiseProvider:writePromiseToURL:completionHandler:` with the place
//! it wants the file.  Nothing is written before then, and a drag that is
//! cancelled writes nothing.  Two Objective-C classes carry this:
//!
//! - [`PromiseDelegate`] answers the promise protocol: the file name
//!   (immediately, from a copy of the names), and the write itself, which
//!   AppKit calls on the operation queue the delegate names, so the main
//!   thread never waits for an extraction.  It holds the [`Payload`] and
//!   outlives the drag, since Finder often collects the files after
//!   `endedAtPoint` has been delivered.  AppKit's provider keeps its delegate
//!   only weakly, so a static list holds it until every promise was kept
//!   or ten minutes have passed.
//! - [`DragSource`] is the `NSDraggingSource`, on the main thread for the
//!   length of the session.  It says which drops it allows, and reports the
//!   outcome when the session ends.
//!
//! The mouse event that AppKit needs to start a session is
//! `NSApp.currentEvent`, which inside winit's event handling is the mouse
//! event being delivered.  If there is none (the redraw ran off a timer, say),
//! a left-mouse-dragged event is made at the pointer's current place.  With
//! no mouse button down the drag is refused (`DragError::NoGesture`).  The
//! drag's picture is drawn at one pixel per point, as on the other platforms.
//! `SLERP_TIMING=1` prints what AppKit asks the delegate and which event the
//! session started from.
//!
//! No plain `file://` fallback is offered next to the promises: it would need
//! the files to exist when the drag starts, which is what the promises avoid.
//! Apps that read only file URLs (the pasteboard type `public.file-url`) get
//! nothing from the drag; Finder, Mail, Xcode, Preview and every other
//! AppKit app that asks for promises (`NSFilePromiseReceiver`) work.

use super::payload::{ActiveGuard, Drag, DragError, DragIcon, KEEP_AFTER_DROP, Outcome, Payload};
use block2::DynBlock;
use objc2::rc::{Retained, Weak};
use objc2::runtime::{AnyClass, AnyObject, NSObject, NSObjectProtocol, ProtocolObject};
use objc2::{AnyThread, DefinedClass, MainThreadMarker, MainThreadOnly, Message, define_class, msg_send};
use objc2_app_kit::{
    NSApplication, NSBitmapImageRep, NSDeviceRGBColorSpace, NSDragOperation, NSDraggingContext, NSDraggingItem,
    NSDraggingSession, NSDraggingSource, NSEvent, NSEventModifierFlags, NSEventType, NSFilePromiseProvider,
    NSFilePromiseProviderDelegate, NSImage, NSView, NSWorkspace,
};
use objc2_foundation::{
    NSArray, NSError, NSNumber, NSOperationQueue, NSPoint, NSProcessInfo, NSRect, NSSize, NSString, NSURL, ns_string,
};
use raw_window_handle::{RawDisplayHandle, RawWindowHandle};
use std::cell::RefCell;
use std::ffi::c_void;
use std::io;
use std::path::Path;
use std::ptr::NonNull;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

/// The picture of an item that has no icon of its own, in points.
const FALLBACK_ICON_SIDE: f64 = 48.0;

/// How far each further item's picture is moved down and to the right; the
/// fifth and later items sit on the fourth.
const STACK_STEP: f64 = 4.0;

//region Starting

pub fn prepare(win: RawWindowHandle, _display: RawDisplayHandle) -> Result<(), DragError> {
    view_of(win).map(|_| ())
}

/// Begins the session; the drag then runs on the main run loop and this
/// returns at once.  It must be called from winit's event handling, on the
/// main thread, since AppKit's session wants the mouse event being handled.
pub fn start(win: RawWindowHandle, _display: RawDisplayHandle, drag: Drag) -> Result<(), DragError> {
    // 1. The window's view, on the main thread, and the one-drag-at-a-time
    //    guard.
    let view = view_of(win)?;
    let mtm = MainThreadMarker::new().ok_or_else(|| {
        DragError::Platform("a drag must start on the main thread, inside winit's event handling".into())
    })?;
    // SAFETY: the handle comes from a window the caller holds and is being
    // used inside its event handling, so the view is alive; AppKit retains it
    // for the session.
    let view: &NSView = unsafe { view.cast::<NSView>().as_ref() };
    let guard = ActiveGuard::acquire()?;

    // 2. The mouse event, and where the pointer is in the view.  A session
    //    begun with every button up would wait for a release that is past.
    if NSEvent::pressedMouseButtons() == 0 {
        return Err(DragError::NoGesture);
    }
    let event = mouse_event(mtm, view)
        .ok_or_else(|| DragError::Platform("the window has no mouse event to start a drag with".into()))?;
    let pointer = view.convertPoint_fromView(event.locationInWindow(), None);

    // 3. One file promise per item, each in its own dragging item.
    let Drag { items, icon } = drag;
    let promises = PromiseDelegate::new(Payload::new(items));
    let picture = icon
        .filter(DragIcon::is_valid)
        .and_then(|icon| bitmap_image(&icon))
        .unwrap_or_else(|| file_icon(promises.ivars().names.first().map_or("", String::as_str)));
    let dragging_items = dragging_items(&promises, &picture, pointer);

    // 4. Begin.  From here the session reports back through `DragSource`,
    //    which owns the guard until it ends.
    let source = DragSource::new(mtm, promises.clone(), guard);
    alive().push(promises);
    CURRENT.with_borrow_mut(|slot| *slot = Some(source.clone()));
    let array = NSArray::from_retained_slice(&dragging_items);
    view.beginDraggingSessionWithItems_event_source(&array, &event, ProtocolObject::from_ref(&*source));
    Ok(())
}

/// A dragging item for each promise of `promises`, all showing `picture`
/// around `pointer` (in the view's coordinates), each a little further down
/// and to the right so that a drag of several files looks like a small stack.
pub(crate) fn dragging_items(
    promises: &PromiseDelegate,
    picture: &NSImage,
    pointer: NSPoint,
) -> Vec<Retained<NSDraggingItem>> {
    let size = picture.size();
    promises
        .ivars()
        .names
        .iter()
        .enumerate()
        .map(|(index, name)| {
            let provider = file_promise(promises, index, name);
            let item =
                NSDraggingItem::initWithPasteboardWriter(NSDraggingItem::alloc(), ProtocolObject::from_ref(&*provider));
            let step = STACK_STEP * index.min(3) as f64;
            let origin = NSPoint::new(pointer.x - size.width / 2.0 + step, pointer.y - size.height / 2.0 + step);
            // SAFETY: the contents are an NSImage, which a dragging item may draw.
            unsafe {
                item.setDraggingFrame_contents(NSRect::new(origin, size), Some(AsRef::<AnyObject>::as_ref(picture)))
            };
            item
        })
        .collect()
}

/// Prints a step of a drag to the terminal when `SLERP_TIMING` is set, as the
/// viewer's other startup traces do.
fn trace(message: impl FnOnce() -> String) {
    if std::env::var_os("SLERP_TIMING").is_some() {
        eprintln!("slerp dragout: {}", message());
    }
}

/// The pasteboard writer for item `index`: the promise of a file called
/// `name`, answered by `promises`.  The provider holds its delegate only
/// weakly, so `promises` must be kept alive (see `alive`).
pub(crate) fn file_promise(promises: &PromiseDelegate, index: usize, name: &str) -> Retained<NSFilePromiseProvider> {
    let provider = NSFilePromiseProvider::initWithFileType_delegate(
        NSFilePromiseProvider::alloc(),
        &content_type(name),
        ProtocolObject::from_ref(promises),
    );
    // SAFETY: the user info is an NSNumber, which `PromiseDelegate::index_of` reads back.
    unsafe { provider.setUserInfo(Some(&NSNumber::new_usize(index))) };
    provider
}

fn view_of(win: RawWindowHandle) -> Result<NonNull<c_void>, DragError> {
    match win {
        RawWindowHandle::AppKit(handle) => Ok(handle.ns_view),
        _ => Err(DragError::Unsupported("this isn't an AppKit window".into())),
    }
}

/// The mouse event the session starts from: the one being handled, if it is a
/// button press or drag in this view's window, or else a left-button drag
/// made at the pointer's place now.
fn mouse_event(mtm: MainThreadMarker, view: &NSView) -> Option<Retained<NSEvent>> {
    let window = view.window()?;
    if let Some(event) = NSApplication::sharedApplication(mtm).currentEvent() {
        let kind = event.r#type();
        let mouse = matches!(
            kind,
            NSEventType::LeftMouseDown
                | NSEventType::LeftMouseDragged
                | NSEventType::RightMouseDown
                | NSEventType::RightMouseDragged
                | NSEventType::OtherMouseDown
                | NSEventType::OtherMouseDragged
        );
        trace(|| format!("current event type {} (mouse: {mouse})", kind.0));
        if mouse && event.windowNumber() == window.windowNumber() {
            return Some(event);
        }
    } else {
        trace(|| "no current event".into());
    }
    let screen = NSRect::new(NSEvent::mouseLocation(), NSSize::new(0.0, 0.0));
    let location = window.convertRectFromScreen(screen).origin;
    NSEvent::mouseEventWithType_location_modifierFlags_timestamp_windowNumber_context_eventNumber_clickCount_pressure(
        NSEventType::LeftMouseDragged,
        location,
        NSEventModifierFlags::empty(),
        NSProcessInfo::processInfo().systemUptime(),
        window.windowNumber(),
        None,
        0,
        1,
        1.0,
    )
}

//endregion

//region Pictures and types

/// The drag's picture from straight-alpha RGBA: AppKit's bitmaps hold
/// premultiplied colour.  One pixel is one point.
fn bitmap_image(icon: &DragIcon) -> Option<Retained<NSImage>> {
    let (width, height) = (icon.width as usize, icon.height as usize);
    // SAFETY: no planes are given, so the representation allocates its own
    // buffer of `height` rows of `width * 4` bytes.
    let rep = unsafe {
        NSBitmapImageRep::initWithBitmapDataPlanes_pixelsWide_pixelsHigh_bitsPerSample_samplesPerPixel_hasAlpha_isPlanar_colorSpaceName_bytesPerRow_bitsPerPixel(
            NSBitmapImageRep::alloc(),
            std::ptr::null_mut(),
            width as isize,
            height as isize,
            8,
            4,
            true,
            false,
            NSDeviceRGBColorSpace,
            (width * 4) as isize,
            32,
        )
    }?;
    let data = rep.bitmapData();
    if data.is_null() || rep.bytesPerRow() != (width * 4) as isize {
        return None;
    }
    // SAFETY: the buffer holds `height * bytesPerRow` bytes and is ours alone until the image uses it.
    let pixels = unsafe { std::slice::from_raw_parts_mut(data, width * height * 4) };
    for (out, px) in pixels.chunks_exact_mut(4).zip(icon.rgba.chunks_exact(4)) {
        let alpha = u16::from(px[3]);
        for channel in 0..3 {
            out[channel] = ((u16::from(px[channel]) * alpha + 127) / 255) as u8;
        }
        out[3] = px[3];
    }
    let image = NSImage::initWithSize(NSImage::alloc(), NSSize::new(width as f64, height as f64));
    image.addRepresentation(&rep);
    Some(image)
}

/// The system's icon for a file of this name's type, as a drag picture.
#[allow(deprecated)] // iconForContentType: wants a UTType, which these bindings can't make.
fn file_icon(name: &str) -> Retained<NSImage> {
    let icon = NSWorkspace::sharedWorkspace().iconForFileType(&content_type(name));
    icon.setSize(NSSize::new(FALLBACK_ICON_SIDE, FALLBACK_ICON_SIDE));
    icon
}

/// The uniform type identifier of a file name's extension (`public.png`,
/// or a dynamic `dyn.` one for an extension macOS doesn't know), so drop
/// targets that take only some kinds of file accept it; `public.data` when
/// the name has no extension.  UTType is looked up by name because the
/// bindings for its framework would be one more dependency for this.
fn content_type(name: &str) -> Retained<NSString> {
    let known = Path::new(name).extension().and_then(|e| e.to_str()).and_then(|extension| {
        let class = AnyClass::get(c"UTType")?;
        let extension = NSString::from_str(extension);
        // SAFETY: `+[UTType typeWithFilenameExtension:]` takes an NSString and returns a UTType or nil;
        // `-identifier` returns an NSString.
        unsafe {
            let ty: Option<Retained<AnyObject>> = msg_send![class, typeWithFilenameExtension: &*extension];
            msg_send![&*ty?, identifier]
        }
    });
    known.unwrap_or_else(|| NSString::from_str("public.data"))
}

//endregion

//region File promises

/// What the promise delegate shares between the main thread and the queue
/// AppKit calls it on.
pub(crate) struct Promises {
    /// The files, made when first asked for.  Locked for a whole extraction.
    payload: Mutex<Payload>,
    /// The names of the files, copied out so that asking for one never waits
    /// for an extraction in progress.
    names: Vec<String>,
    /// Whether each item's promise has been kept (or has failed for good).
    kept: Vec<AtomicBool>,
    /// How many are still to be kept.
    unkept: AtomicUsize,
    /// Set when the drag ended with a drop.
    dropped: AtomicBool,
    /// Where AppKit runs the writes: one at a time, in item order.
    queue: Retained<NSOperationQueue>,
}

/// The promise delegates that may still be asked for files: AppKit's
/// providers hold their delegate weakly, and Finder collects dropped files
/// after the drag session is over.
fn alive() -> std::sync::MutexGuard<'static, Vec<Retained<PromiseDelegate>>> {
    static ALIVE: Mutex<Vec<Retained<PromiseDelegate>>> = Mutex::new(Vec::new());
    ALIVE.lock().unwrap_or_else(|e| e.into_inner())
}

define_class!(
    /// Answers `NSFilePromiseProvider`'s questions for one drag.
    // SAFETY: NSObject has no subclassing requirements, and this class
    // has no `Drop` that could call back into overridden methods.
    #[unsafe(super(NSObject))]
    #[name = "SlerpFilePromiseDelegate"]
    #[ivars = Promises]
    pub(crate) struct PromiseDelegate;

    unsafe impl NSObjectProtocol for PromiseDelegate {}

    unsafe impl NSFilePromiseProviderDelegate for PromiseDelegate {
        #[unsafe(method_id(filePromiseProvider:fileNameForType:))]
        fn file_name(&self, provider: &NSFilePromiseProvider, _file_type: &NSString) -> Retained<NSString> {
            let name = Self::index_of(provider).and_then(|index| self.ivars().names.get(index));
            trace(|| format!("name asked for: {name:?}"));
            NSString::from_str(name.map_or("Untitled", String::as_str))
        }

        #[unsafe(method(filePromiseProvider:writePromiseToURL:completionHandler:))]
        fn write_promise(
            &self,
            provider: &NSFilePromiseProvider,
            url: &NSURL,
            completion: &DynBlock<dyn Fn(*mut NSError)>,
        ) {
            trace(|| format!("write asked for: item {:?} to {:?}", Self::index_of(provider), url.to_file_path()));
            let result = match (Self::index_of(provider), url.to_file_path()) {
                (Some(index), Some(dest)) => self.keep(index, &dest),
                _ => Err(io::Error::other("the drop target named no file to write")),
            };
            match result {
                Ok(()) => completion.call((std::ptr::null_mut(),)),
                Err(e) => {
                    eprintln!("slerp dragout: {e}");
                    let error = Self::cocoa_error();
                    completion.call((Retained::as_ptr(&error).cast_mut(),));
                }
            }
        }

        #[unsafe(method_id(operationQueueForFilePromiseProvider:))]
        fn operation_queue(&self, _provider: &NSFilePromiseProvider) -> Retained<NSOperationQueue> {
            trace(|| "operation queue asked for".into());
            self.ivars().queue.clone()
        }
    }
);

impl PromiseDelegate {
    pub(crate) fn new(payload: Payload) -> Retained<Self> {
        let names = payload.names().to_vec();
        let queue = NSOperationQueue::new();
        queue.setMaxConcurrentOperationCount(1);
        queue.setName(Some(ns_string!("Slerp drag-out")));
        let promises = Promises {
            unkept: AtomicUsize::new(names.len()),
            kept: names.iter().map(|_| AtomicBool::new(false)).collect(),
            names,
            payload: Mutex::new(payload),
            dropped: AtomicBool::new(false),
            queue,
        };
        let this = Self::alloc().set_ivars(promises);
        // SAFETY: NSObject's `init` takes no arguments.
        unsafe { msg_send![super(this), init] }
    }

    /// The item a provider was made for.
    fn index_of(provider: &NSFilePromiseProvider) -> Option<usize> {
        let info = provider.userInfo()?;
        Some(info.downcast_ref::<NSNumber>()?.as_usize())
    }

    /// Writes item `index` to `dest`, the place the drop target chose.
    fn keep(&self, index: usize, dest: &Path) -> io::Result<()> {
        let promises = self.ivars();
        let result = promises.payload.lock().unwrap_or_else(|e| e.into_inner()).export_one(index, dest);
        // Whether it worked or not, there is nothing more to wait for: a
        // failed item stays failed.
        if promises.kept.get(index).is_some_and(|kept| !kept.swap(true, Ordering::AcqRel)) {
            promises.unkept.fetch_sub(1, Ordering::AcqRel);
        }
        self.release_if_done();
        result
    }

    /// What the drop target is told when a file couldn't be made:
    /// `NSFileWriteUnknownError` in the Cocoa error domain.  The reason is
    /// printed instead, since Finder shows its own words for the error.
    fn cocoa_error() -> Retained<NSError> {
        // SAFETY: the domain is a string and no user info is given.
        unsafe { NSError::errorWithDomain_code_userInfo(ns_string!("NSCocoaErrorDomain"), 512, None) }
    }

    /// The drag was cancelled: nothing will be asked for, so drop what was
    /// kept for it.
    fn cancel(&self) {
        self.ivars().payload.lock().unwrap_or_else(|e| e.into_inner()).cancel();
        self.release();
    }

    /// The drag ended with a drop.  The files are collected, usually already
    /// or in a moment; a target that never asks for the rest is given ten
    /// minutes, as for a drop on the other platforms.
    fn dropped(&self) {
        let promises = self.ivars();
        promises.dropped.store(true, Ordering::Release);
        if promises.unkept.load(Ordering::Acquire) == 0 {
            self.release();
            return;
        }
        let me: Weak<PromiseDelegate> = Weak::from_retained(&self.retain());
        let spawned = std::thread::Builder::new().name("dragout-cleanup".into()).spawn(move || {
            std::thread::sleep(KEEP_AFTER_DROP);
            if let Some(delegate) = me.load() {
                delegate.ivars().payload.lock().unwrap_or_else(|e| e.into_inner()).finish();
                delegate.release();
            }
        });
        // Without the thread the promises can't expire; keeping them is harmless.
        drop(spawned);
    }

    /// Lets go of the delegate once the drag is over and every promise is kept.
    fn release_if_done(&self) {
        let promises = self.ivars();
        if promises.dropped.load(Ordering::Acquire) && promises.unkept.load(Ordering::Acquire) == 0 {
            self.release();
        }
    }

    fn release(&self) {
        // Held across the removal, since this may be the last reference
        // to the object whose method is running.
        let me = self.retain();
        alive().retain(|delegate| !std::ptr::eq(&**delegate, &*me));
    }
}

//endregion

//region The dragging source

/// What the dragging source holds.
struct SourceState {
    promises: Retained<PromiseDelegate>,
    /// Released when the session ends.
    guard: RefCell<Option<ActiveGuard>>,
}

thread_local! {
    /// The source of the session under way.  AppKit doesn't retain it, so
    /// this does, until the session ends.  Sessions run on the main thread.
    static CURRENT: RefCell<Option<Retained<DragSource>>> = const { RefCell::new(None) };
}

define_class!(
    /// The `NSDraggingSource` of one drag.
    // SAFETY: NSObject has no subclassing requirements, and this class
    // has no `Drop`.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "SlerpDragSource"]
    #[ivars = SourceState]
    struct DragSource;

    unsafe impl NSObjectProtocol for DragSource {}

    unsafe impl NSDraggingSource for DragSource {
        #[unsafe(method(draggingSession:sourceOperationMaskForDraggingContext:))]
        fn source_operation_mask(&self, _session: &NSDraggingSession, _context: NSDraggingContext) -> NSDragOperation {
            // The files are written afresh for the target, so a drop is
            // always a copy.
            NSDragOperation::Copy
        }

        #[unsafe(method(draggingSession:endedAtPoint:operation:))]
        fn ended(&self, _session: &NSDraggingSession, _point: NSPoint, operation: NSDragOperation) {
            // Kept until the end of this method: the slot was what held us.
            let _keep = CURRENT.with_borrow_mut(Option::take);
            let state = self.ivars();
            let outcome = if operation == NSDragOperation::None {
                state.promises.cancel();
                Outcome::Cancelled
            } else {
                state.promises.dropped();
                Outcome::Dropped
            };
            // The guard goes first, so a drag started in answer to the
            // outcome isn't refused as busy.
            state.guard.borrow_mut().take();
            super::report(outcome);
        }
    }
);

impl DragSource {
    fn new(mtm: MainThreadMarker, promises: Retained<PromiseDelegate>, guard: ActiveGuard) -> Retained<Self> {
        let state = SourceState { promises, guard: RefCell::new(Some(guard)) };
        let this = Self::alloc(mtm).set_ivars(state);
        // SAFETY: NSObject's `init` takes no arguments.
        unsafe { msg_send![super(this), init] }
    }
}

//endregion

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dragout::DragItem;
    use std::sync::Arc;

    /// An item that writes `body` and counts how often it was asked to.
    fn counted(name: &str, body: &'static [u8], count: &Arc<AtomicUsize>) -> DragItem {
        let count = count.clone();
        DragItem::from_write(name, move |dest| {
            count.fetch_add(1, Ordering::SeqCst);
            std::fs::write(dest, body)
        })
    }

    fn is_alive(delegate: &PromiseDelegate) -> bool {
        alive().iter().any(|d| std::ptr::eq(&**d, delegate))
    }

    #[test]
    fn file_types_follow_the_extension() {
        assert_eq!(content_type("picture.png").to_string(), "public.png");
        assert_eq!(content_type("no extension").to_string(), "public.data");
        assert!(content_type("level.zzqq9").to_string().starts_with("dyn."));
    }

    #[test]
    fn the_picture_is_premultiplied_and_sized_in_points() {
        let icon = DragIcon { width: 2, height: 1, rgba: vec![200, 100, 50, 128, 10, 20, 30, 255] };
        let image = bitmap_image(&icon).expect("an image");
        assert_eq!((image.size().width, image.size().height), (2.0, 1.0));
    }

    #[test]
    fn the_system_icon_and_the_dragging_items_are_made_without_a_window() {
        let picture = file_icon("level.wad");
        assert_eq!((picture.size().width, picture.size().height), (FALLBACK_ICON_SIDE, FALLBACK_ICON_SIDE));
        let count = Arc::new(AtomicUsize::new(0));
        let items =
            vec![counted("a.txt", b"a", &count), counted("b.txt", b"b", &count), counted("c.txt", b"c", &count)];
        let promises = PromiseDelegate::new(Payload::new(items));
        let dragging = dragging_items(&promises, &picture, NSPoint::new(100.0, 50.0));
        assert_eq!(dragging.len(), 3);
        let frames: Vec<NSRect> = dragging.iter().map(|item| item.draggingFrame()).collect();
        assert_eq!((frames[0].origin.x, frames[0].origin.y), (100.0 - 24.0, 50.0 - 24.0));
        assert_eq!((frames[2].origin.x, frames[2].origin.y), (100.0 - 24.0 + 8.0, 50.0 - 24.0 + 8.0));
        assert_eq!(count.load(Ordering::SeqCst), 0, "building the items writes nothing");
    }

    #[test]
    fn a_cancelled_drag_writes_nothing_and_is_forgotten() {
        let count = Arc::new(AtomicUsize::new(0));
        let promises = PromiseDelegate::new(Payload::new(vec![counted("a.txt", b"a", &count)]));
        alive().push(promises.clone());
        assert!(is_alive(&promises));
        promises.cancel();
        assert!(!is_alive(&promises));
        let dest = std::env::temp_dir().join("slerp-dragout-never-written");
        assert!(promises.keep(0, &dest).is_err());
        assert_eq!(count.load(Ordering::SeqCst), 0);
        assert!(!dest.exists());
    }

    /// The session's ending as AppKit reports it, without a window: the guard
    /// is released, the outcome recorded, and the promises dealt with.
    #[test]
    #[ignore = "takes the global one-drag guard, which payload's tests also take: cargo test -- --ignored --test-threads=1"]
    fn the_source_reports_how_the_session_ended() {
        // SAFETY: libtest runs tests on threads of its own, never the main one.  Nothing here touches what
        // AppKit keeps to the main thread: the source's methods only read their own state.
        let mtm = unsafe { MainThreadMarker::new_unchecked() };
        let end = |operation: NSDragOperation| {
            let count = Arc::new(AtomicUsize::new(0));
            let promises = PromiseDelegate::new(Payload::new(vec![counted("a.txt", b"a", &count)]));
            alive().push(promises.clone());
            let source = DragSource::new(mtm, promises.clone(), ActiveGuard::acquire().expect("no other drag"));
            CURRENT.with_borrow_mut(|slot| *slot = Some(source.clone()));
            assert!(crate::dragout::is_active());
            let session: *const NSDraggingSession = std::ptr::null();
            let mask: NSDragOperation = unsafe {
                msg_send![&*source, draggingSession: session, sourceOperationMaskForDraggingContext: NSDraggingContext::OutsideApplication]
            };
            assert_eq!(mask, NSDragOperation::Copy);
            // SAFETY: the method ignores its session argument.
            let _: () = unsafe {
                msg_send![&*source, draggingSession: session, endedAtPoint: NSPoint::new(0.0, 0.0), operation: operation]
            };
            assert!(!crate::dragout::is_active(), "the guard is released");
            assert!(CURRENT.with_borrow(Option::is_none), "the source is let go");
            (promises, count)
        };
        let (promises, count) = end(NSDragOperation::None);
        assert_eq!(crate::dragout::take_outcome(), Some(Outcome::Cancelled));
        assert!(!is_alive(&promises));
        assert_eq!(count.load(Ordering::SeqCst), 0);
        // A drop leaves the delegate alive for the files not yet collected.
        let (promises, count) = end(NSDragOperation::Copy);
        assert_eq!(crate::dragout::take_outcome(), Some(Outcome::Dropped));
        assert!(is_alive(&promises));
        promises.keep(0, &std::env::temp_dir().join(format!("slerp-dragout-source-{}", std::process::id()))).unwrap();
        assert_eq!(count.load(Ordering::SeqCst), 1);
        assert!(!is_alive(&promises), "kept after the drop, so let go");
        let _ = std::fs::remove_file(std::env::temp_dir().join(format!("slerp-dragout-source-{}", std::process::id())));
    }
}
