//! Files that Finder, the Dock or `open` ask the viewer to open, on macOS.
//!
//! macOS hands files to an app as an Apple Event, which AppKit turns into
//! a call of the application delegate's `application:openURLs:`.  winit
//! 0.30 owns the delegate and has no such method, so "Open With Slerp"
//! used to start an empty viewer.  This module adds the method to winit's
//! delegate class at run time, when AppKit announces that launching is
//! about to finish: winit has installed its delegate by then, and AppKit
//! hasn't yet handled the event that launched the app.  Files opened while
//! the viewer runs take the same path.
//!
//! The method only notes the path and wakes the window, which opens it on
//! its next frame in place of the current file.

use objc2::runtime::{AnyClass, AnyObject, Imp, Sel};
use objc2::sel;
use objc2_app_kit::{NSApplication, NSApplicationWillFinishLaunchingNotification};
use objc2_foundation::{MainThreadMarker, NSArray, NSNotification, NSNotificationCenter, NSURL};
use std::io::Write;
use std::path::PathBuf;
use std::ptr::NonNull;
use std::sync::Mutex;

/// The latest file asked for and not yet opened.
static PENDING: Mutex<Option<PathBuf>> = Mutex::new(None);

/// The window to wake when a file arrives: the one open now, since a warm
/// viewer opens one after another.
static CONTEXT: Mutex<Option<egui::Context>> = Mutex::new(None);

/// Starts listening for files to open.  Call on the main thread before
/// the event loop starts.
pub fn listen() {
    let block = block2::RcBlock::new(|_: NonNull<NSNotification>| add_open_method());
    let center = NSNotificationCenter::defaultCenter();
    // SAFETY: the notification name is AppKit's own constant; with no queue
    // the block runs on the posting thread, which is the main thread.
    let observer = unsafe {
        center.addObserverForName_object_queue_usingBlock(
            Some(NSApplicationWillFinishLaunchingNotification),
            None,
            None,
            &block,
        )
    };
    // Launching finishes once per process, so the observer can stay.
    std::mem::forget(observer);
}

/// Lets a file that arrives while the viewer runs wake its window.
pub fn set_context(ctx: &egui::Context) {
    *CONTEXT.lock().unwrap_or_else(|p| p.into_inner()) = Some(ctx.clone());
}

/// The file Finder last asked for, if the window hasn't opened it yet.
pub fn take_pending() -> Option<PathBuf> {
    PENDING.lock().unwrap_or_else(|p| p.into_inner()).take()
}

/// Gives winit's application delegate an `application:openURLs:` method.
fn add_open_method() {
    let Some(mtm) = MainThreadMarker::new() else { return };
    let Some(delegate) = NSApplication::sharedApplication(mtm).delegate() else { return };
    let class: &AnyClass = AsRef::<AnyObject>::as_ref(&*delegate).class();
    type OpenUrls = extern "C-unwind" fn(&AnyObject, Sel, &NSApplication, &NSArray<NSURL>);
    // SAFETY: `open_urls` has the method's Objective-C signature (void
    // return; self, _cmd, an NSApplication and an NSArray of NSURL), which
    // the type string "v@:@@" spells out.  `class_addMethod` leaves a
    // method the class already has alone, should winit add one.
    unsafe {
        let imp = std::mem::transmute::<OpenUrls, Imp>(open_urls);
        objc2::ffi::class_addMethod(
            std::ptr::from_ref(class).cast_mut(),
            sel!(application:openURLs:),
            imp,
            c"v@:@@".as_ptr(),
        );
    }
}

/// `-[delegate application:openURLs:]`.  The viewer shows one file, so of
/// several it opens the first.  Nothing here can panic, which would unwind
/// into AppKit.
extern "C-unwind" fn open_urls(_this: &AnyObject, _cmd: Sel, _app: &NSApplication, urls: &NSArray<NSURL>) {
    let Some(path) = urls.iter().find_map(|url| url.to_file_path()) else { return };
    if std::env::var_os("SLERP_TIMING").is_some() {
        let _ = writeln!(std::io::stderr(), "slerp view: asked to open {}", path.display());
    }
    *PENDING.lock().unwrap_or_else(|p| p.into_inner()) = Some(path);
    if let Some(ctx) = &*CONTEXT.lock().unwrap_or_else(|p| p.into_inner()) {
        ctx.request_repaint();
    }
}
