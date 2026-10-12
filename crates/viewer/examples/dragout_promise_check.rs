//! Checks on a Mac, without a window, that drag-out's file promises behave
//! the way Finder uses them.  The promises go on a private pasteboard (AppKit
//! asks the delegate for their names there, and nothing may be written), then
//! this plays the drop target's request: it asks the delegate for the queue
//! and the file name and calls `writePromiseToURL:completionHandler:` on
//! that queue.  Each file must be written once, off the main thread, under
//! the right name, into the folder chosen.
//!
//! ```text
//! cargo run --release -p slerp-viewer --example dragout_promise_check
//! ```
//!
//! It prints one line per step and exits with an error at the first step
//! that fails.  AppKit fulfils pasteboard promises through the main thread,
//! which is why this is an example (its `main` is the main thread) and not
//! a unit test; it needs a login session for the pasteboard server.  What it
//! cannot do is `NSFilePromiseReceiver`'s own request: on a private
//! pasteboard the receiver looks for the paste location of a Finder window
//! (`com.apple.pastelocation`) and gives up, so the request is made here,
//! with the calls AppKit makes on a drop.

#![allow(dead_code)]

#[cfg(target_os = "macos")]
#[path = "../src/dragout/payload.rs"]
mod payload;

#[cfg(target_os = "macos")]
#[path = "../src/dragout/appkit.rs"]
mod appkit;

/// What `appkit.rs` calls when a drag ends; no drag runs here.
#[cfg(target_os = "macos")]
fn report(_: payload::Outcome) {}

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("dragout_promise_check is for macOS");
}

#[cfg(target_os = "macos")]
fn main() {
    use block2::RcBlock;
    use objc2::rc::Retained;
    use objc2::runtime::ProtocolObject;
    use objc2::{ClassType, msg_send};
    use objc2_app_kit::{NSFilePromiseProvider, NSFilePromiseReceiver, NSPasteboard, NSPasteboardWriting};
    use objc2_foundation::{
        MainThreadMarker, NSArray, NSDate, NSError, NSOperationQueue, NSRunLoop, NSString, NSURL, ns_string,
    };
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use std::thread::ThreadId;
    use std::time::{Duration, Instant};

    fn step(what: &str) {
        println!("ok: {what}");
    }

    MainThreadMarker::new().expect("this must run on the main thread");
    // The delegate's trace of what AppKit asks, which this example exists to show.
    // SAFETY: nothing else is running yet.
    unsafe { std::env::set_var("SLERP_TIMING", "1") };

    // 1. Two items that count how often they are asked to write, and where.
    let names = ["first.txt", "second #2 (50%).bin"];
    let bodies: [&'static [u8]; 2] = [b"one", b"two"];
    let counts = [Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0))];
    let calls = || counts.iter().map(|c| c.load(Ordering::SeqCst)).collect::<Vec<_>>();
    let main_thread: ThreadId = std::thread::current().id();
    let on_main = Arc::new(AtomicBool::new(false));
    let items = (0..2)
        .map(|i| {
            let (count, on_main) = (counts[i].clone(), on_main.clone());
            payload::DragItem::from_write(names[i], move |dest| {
                count.fetch_add(1, Ordering::SeqCst);
                if std::thread::current().id() == main_thread {
                    on_main.store(true, Ordering::SeqCst);
                }
                std::fs::write(dest, bodies[i])
            })
        })
        .collect();
    let promises = appkit::PromiseDelegate::new(payload::Payload::new(items));

    // 2. The promises go on a pasteboard and are read back as a drop target
    //    reads them.  Nothing is written.
    let providers: Vec<Retained<NSFilePromiseProvider>> =
        (0..2).map(|i| appkit::file_promise(&promises, i, names[i])).collect();
    assert!(providers.iter().all(|p| p.delegate().is_some()), "the providers know their delegate");
    let writers: Vec<Retained<ProtocolObject<dyn NSPasteboardWriting>>> =
        providers.iter().map(|p| ProtocolObject::from_retained(p.clone())).collect();
    let pasteboard = NSPasteboard::pasteboardWithUniqueName();
    pasteboard.clearContents();
    assert!(pasteboard.writeObjects(&NSArray::from_retained_slice(&writers)), "the pasteboard took the promises");
    let classes = NSArray::from_slice(&[NSFilePromiseReceiver::class()]);
    // SAFETY: the class array holds only classes, and there are no options.
    let read =
        unsafe { pasteboard.readObjectsForClasses_options(&classes, None) }.expect("receivers on the pasteboard");
    let receivers = read.iter().filter(|o| o.downcast_ref::<NSFilePromiseReceiver>().is_some()).count();
    assert_eq!(receivers, 2, "one receiver per promise");
    assert_eq!(calls(), [0, 0], "offering and reading the promises must not write anything");
    step("two promises offered and read back; nothing written");

    // 3. The drop target's request, as AppKit makes it: the delegate's queue
    //    and the file name, then the write on that queue.
    let dir = std::env::temp_dir().join(format!("slerp-dragout-check-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let done: Arc<Mutex<Vec<bool>>> = Arc::default();
    let file_name = |provider: &Retained<NSFilePromiseProvider>| -> String {
        // SAFETY: this is the protocol's selector with the protocol's argument types.
        let name: Retained<NSString> = unsafe {
            msg_send![&*promises, filePromiseProvider: &**provider, fileNameForType: ns_string!("public.data")]
        };
        name.to_string()
    };
    let request_write = |provider: &Retained<NSFilePromiseProvider>, dest: PathBuf| {
        // SAFETY: as above.
        let queue: Retained<NSOperationQueue> =
            unsafe { msg_send![&*promises, operationQueueForFilePromiseProvider: &**provider] };
        let url = NSURL::from_file_path(dest).expect("a file URL");
        let completion = RcBlock::new({
            let done = done.clone();
            move |error: *mut NSError| done.lock().unwrap().push(!error.is_null())
        });
        let write = RcBlock::new({
            let (promises, provider) = (promises.clone(), provider.clone());
            move || {
                // SAFETY: the protocol's selector, with a completion block of the protocol's type.
                let _: () = unsafe {
                    msg_send![
                        &*promises,
                        filePromiseProvider: &*provider,
                        writePromiseToURL: &*url,
                        completionHandler: &*completion
                    ]
                };
            }
        });
        // SAFETY: the block is a plain closure that owns what it uses.
        unsafe { queue.addOperationWithBlock(&write) };
    };
    let wait_for = |count: usize| {
        let deadline = Instant::now() + Duration::from_secs(15);
        while done.lock().unwrap().len() < count && Instant::now() < deadline {
            NSRunLoop::currentRunLoop().runUntilDate(&NSDate::dateWithTimeIntervalSinceNow(0.02));
        }
        assert_eq!(done.lock().unwrap().len(), count, "the completion handlers should all run within 15 s");
    };
    let asked: Vec<String> = providers.iter().map(&file_name).collect();
    assert_eq!(asked, names);
    let started = Instant::now();
    for (provider, name) in providers.iter().zip(&asked) {
        request_write(provider, dir.join(name));
    }
    wait_for(2);
    assert!(done.lock().unwrap().iter().all(|failed| !failed), "no write failed");
    step(&format!("both files delivered in {:.3} s", started.elapsed().as_secs_f64()));

    // 4. Each producer ran once, off the main thread, and the files are right.
    assert_eq!(calls(), [1, 1], "each producer runs exactly once");
    assert!(!on_main.load(Ordering::SeqCst), "the main thread must not do the extracting");
    for i in 0..2 {
        assert_eq!(std::fs::read(dir.join(names[i])).unwrap(), bodies[i], "{}", names[i]);
    }
    let mut found: Vec<String> =
        std::fs::read_dir(&dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    found.sort();
    assert_eq!(found, names, "nothing else was written");
    step("each producer ran once, off the main thread; names and contents are right");

    // 5. A second request for the same item (a drop target asking twice)
    //    copies the file made before instead of extracting again.
    request_write(&providers[0], dir.join("again.txt"));
    wait_for(3);
    assert_eq!(calls(), [1, 1], "no second extraction");
    assert_eq!(std::fs::read(dir.join("again.txt")).unwrap(), bodies[0]);
    step("a repeated request copies without extracting again");

    let _ = std::fs::remove_dir_all(&dir);
}
