//! The harness every Slerp fuzz target shares.
//!
//! Slerp's readers run on every file a user browses past, downloads
//! included, so a reader must never panic, hang, overflow the stack or
//! allocate without bound.  libFuzzer finds panics (debug assertions and
//! overflow checks are on in fuzz builds), stack overflows and hangs by
//! itself.  This crate adds what it can't see without a sanitizer:
//!
//! - [`Capped`], the global allocator, aborts on one allocation over
//!   `SLERP_FUZZ_MAX_ALLOC_MB` (512 MB by default) or on more than
//!   `SLERP_FUZZ_MAX_LIVE_MB` (1536 MB) held at once.  libFuzzer's own
//!   `-rss_limit_mb` only counts pages touched, so a reader that reserves
//!   gigabytes because a header said so, and touches a few, slips past it.
//! - [`isolated`] keeps inputs that crashed or hung code Slerp runs in a
//!   forked child (`slerp-isolate`: C and C++ decoders, PDF, SVG), which the
//!   child's isolation turns into an ordinary error.  They are written to
//!   `$SLERP_FUZZ_DIR/isolated/<target>/` to be looked at separately.
//!
//! # Inputs
//!
//! A target's input is one selector byte (two when a target lists more than
//! 256 extensions), one byte of [`Flags`], then the file.  The selector picks
//! the file's extension from the target's list, since many formats are
//! known by extension (OBJ, DXF, G-code) and the sniffers look at it.  The
//! file is written to a private scratch folder (on `/tmp`) for readers that
//! take a path, and handed over as bytes to those that take bytes.
//!
//! Running a target binary with `SLERP_FUZZ_LIST_EXTS=1` prints its
//! extension list, one per line, which `tools/fuzz.sh` reads to build seed
//! inputs from the test corpus: the line number is the selector.

use slerp_scene::{Asset, Limits, Pose, Scene, Texture};
use std::alloc::{GlobalAlloc, Layout, System};
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

/*┌─────────────────╮
  | Allocator       |
  └─────────────────╯*/
//region Allocator

/// The system allocator with caps, which abort with a backtrace so that
/// libFuzzer records the input as a crash.
pub struct Capped;

#[global_allocator]
static ALLOCATOR: Capped = Capped;

static LIVE: AtomicUsize = AtomicUsize::new(0);
static MAX_ONE: AtomicUsize = AtomicUsize::new(512 << 20);
static MAX_LIVE: AtomicUsize = AtomicUsize::new(1536 << 20);
/// Set while a cap is being reported, so the report's own allocations pass.
static REPORTING: AtomicBool = AtomicBool::new(false);

/// Aborts when `size` more bytes would break a cap.
fn check(size: usize) {
    if REPORTING.load(Ordering::Relaxed) {
        return;
    }
    let live = LIVE.load(Ordering::Relaxed);
    let one = MAX_ONE.load(Ordering::Relaxed);
    let all = MAX_LIVE.load(Ordering::Relaxed);
    if size <= one && live.saturating_add(size) <= all {
        return;
    }
    if REPORTING.swap(true, Ordering::SeqCst) {
        return;
    }
    let what = if size > one { "one allocation over the cap" } else { "live allocations over the cap" };
    eprintln!(
        "==slerp-fuzz== {what}: asked for {size} bytes with {live} live (caps {} MB each, {} MB in all)",
        one >> 20,
        all >> 20
    );
    eprintln!("{}", std::backtrace::Backtrace::force_capture());
    std::process::abort();
}

// SAFETY: every call goes straight to the system allocator with the same
// layout; the counters only watch.
unsafe impl GlobalAlloc for Capped {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        check(layout.size());
        // SAFETY: the caller's layout is passed on unchanged.
        let p = unsafe { System.alloc(layout) };
        if !p.is_null() {
            LIVE.fetch_add(layout.size(), Ordering::Relaxed);
        }
        p
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        check(layout.size());
        // SAFETY: as in `alloc`.
        let p = unsafe { System.alloc_zeroed(layout) };
        if !p.is_null() {
            LIVE.fetch_add(layout.size(), Ordering::Relaxed);
        }
        p
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: as in `alloc`.
        unsafe { System.dealloc(ptr, layout) };
        LIVE.fetch_sub(layout.size(), Ordering::Relaxed);
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if new_size > layout.size() {
            check(new_size - layout.size());
            if new_size > MAX_ONE.load(Ordering::Relaxed) {
                check(new_size);
            }
        }
        // SAFETY: as in `alloc`.
        let p = unsafe { System.realloc(ptr, layout, new_size) };
        if !p.is_null() {
            LIVE.fetch_add(new_size, Ordering::Relaxed);
            LIVE.fetch_sub(layout.size(), Ordering::Relaxed);
        }
        p
    }
}

//endregion

/*┌─────────────────╮
  | Setup           |
  └─────────────────╯*/
//region Setup

/// What one target fuzzes: its name (for the isolated-crash folder) and the
/// extensions its selector byte chooses from.
pub struct Target {
    pub name: &'static str,
    pub exts: Vec<&'static str>,
}

static SCRATCH: Mutex<Option<PathBuf>> = Mutex::new(None);
static NAME: Mutex<&'static str> = Mutex::new("unknown");

/// Prepares the process; call it from the target's `init:` block.
///
/// 1. With `SLERP_FUZZ_LIST_EXTS` set, prints the extensions and exits.
/// 2. Reads the allocator caps from the environment.
/// 3. Points `HOME` and the XDG folders at a scratch folder, so readers that
///    cache (the DNA reader, Unity's GUID index) or look for the user's
///    files (Minecraft jars, settings) touch nothing real.
/// 4. Ignores SIGPIPE as Rust's own `main` does, and resets the signal
///    handlers in forked children, so a crash in a `slerp-isolate` child
///    kills only the child, as it does outside the fuzzer, rather than
///    running libFuzzer's crash report in it.
/// 5. Lets panics in other people's crates unwind (see the body).
pub fn init(target: &Target) {
    if std::env::var_os("SLERP_FUZZ_LIST_EXTS").is_some() {
        for e in &target.exts {
            println!("{e}");
        }
        std::process::exit(0);
    }
    *NAME.lock().unwrap() = target.name;
    let mb = |var: &str| std::env::var(var).ok().and_then(|v| v.parse::<usize>().ok()).map(|v| v << 20);
    if let Some(v) = mb("SLERP_FUZZ_MAX_ALLOC_MB") {
        MAX_ONE.store(v, Ordering::Relaxed);
    }
    if let Some(v) = mb("SLERP_FUZZ_MAX_LIVE_MB") {
        MAX_LIVE.store(v, Ordering::Relaxed);
    }
    let base = std::env::var_os("SLERP_FUZZ_SCRATCH").map(PathBuf::from).unwrap_or_else(|| "/tmp/slerp-fuzz".into());
    let scratch = base.join(format!("{}-{}", target.name, std::process::id()));
    let _ = std::fs::remove_dir_all(&scratch);
    for sub in ["in", "home", "cache", "config", "state", "data", "run", "tmp"] {
        std::fs::create_dir_all(scratch.join(sub)).expect("scratch folder");
    }
    // SAFETY: libFuzzer calls the init block before it starts any thread.
    unsafe {
        std::env::set_var("HOME", scratch.join("home"));
        std::env::set_var("XDG_CACHE_HOME", scratch.join("cache"));
        std::env::set_var("XDG_CONFIG_HOME", scratch.join("config"));
        std::env::set_var("XDG_STATE_HOME", scratch.join("state"));
        std::env::set_var("XDG_DATA_HOME", scratch.join("data"));
        std::env::set_var("XDG_RUNTIME_DIR", scratch.join("run"));
        std::env::set_var("TMPDIR", scratch.join("tmp"));
        // Rust's own `main` ignores SIGPIPE, so a write to a reader that went
        // away (Ghostscript's input) fails with an error; libFuzzer's `main`
        // doesn't, and the signal would end the fuzzer.
        libc::signal(libc::SIGPIPE, libc::SIG_IGN);
        libc::pthread_atfork(None, None, Some(child_signals));
        libc::atexit(remove_scratch);
    }
    *SCRATCH.lock().unwrap() = Some(scratch);
    // 5. Panics in other people's crates unwind, as they do in release
    //    builds, to the `catch_unwind` Slerp puts around such calls (laz,
    //    tiny-skia, earcut); one that nothing catches still reaches
    //    libfuzzer-sys's own guard, which aborts.  Slerp's own panics abort
    //    at once, caught or not, since its code shouldn't panic at all.
    let abort_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if info.location().is_some_and(|l| l.file().contains("/.cargo/registry/")) {
            return;
        }
        abort_hook(info)
    }));
}

/// In a forked child: the default action for the signals libFuzzer handles.
extern "C" fn child_signals() {
    for s in [libc::SIGSEGV, libc::SIGBUS, libc::SIGABRT, libc::SIGILL, libc::SIGFPE, libc::SIGALRM, libc::SIGXFSZ] {
        // SAFETY: signal() is async-signal-safe and SIG_DFL is valid.
        unsafe { libc::signal(s, libc::SIG_DFL) };
    }
}

/// Removes the scratch folder when the fuzzer exits normally (a crash
/// leaves it for the next run of `tools/fuzz.sh` to clear).
extern "C" fn remove_scratch() {
    if let Ok(guard) = SCRATCH.try_lock()
        && let Some(dir) = guard.as_ref()
    {
        let _ = std::fs::remove_dir_all(dir);
    }
}

//endregion

/*┌─────────────────╮
  | Inputs          |
  └─────────────────╯*/
//region Inputs

/// What the flags byte switches.
#[derive(Clone, Copy, Debug)]
pub struct Flags(pub u8);

impl Flags {
    /// Load animation clips, as the viewer does; thumbnails skip them.
    pub fn animations(self) -> bool {
        self.0 & 1 != 0
    }

    /// Don't read neighbouring files, as in GNOME's thumbnail sandbox.
    pub fn no_neighbours(self) -> bool {
        self.0 & 2 != 0
    }

    /// The thumbnail size: 128 or 256 pixels.
    pub fn size(self) -> u32 {
        if self.0 & 4 != 0 { 128 } else { 256 }
    }
}

/// One fuzz input taken apart.
pub struct Input<'a> {
    pub ext: &'static str,
    pub flags: Flags,
    pub body: &'a [u8],
}

impl Input<'_> {
    /// The limits a thumbnail of this size would load with, changed by the
    /// flags.
    pub fn limits(&self) -> Limits {
        Limits {
            max_texture_size: Some(self.flags.size()),
            animations: self.flags.animations(),
            neighbours: !self.flags.no_neighbours(),
            ..Limits::thumbnail()
        }
    }

    /// Writes the file to the scratch folder as `in.<ext>` and returns its
    /// path.  It is the only file there, so readers looking for neighbours
    /// find none.
    pub fn file(&self) -> PathBuf {
        let dir = SCRATCH.lock().unwrap().as_ref().expect("slerp_fuzz::init wasn't called").join("in");
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for e in entries.flatten() {
                let p = e.path();
                if p.is_dir() {
                    let _ = std::fs::remove_dir_all(&p);
                } else {
                    let _ = std::fs::remove_file(&p);
                }
            }
        }
        let path = dir.join(format!("in.{}", self.ext));
        std::fs::write(&path, self.body).expect("scratch file");
        path
    }
}

/// Splits `data` into its selector, flags and body.  `None` when it is too
/// short to hold them.
pub fn split<'a>(target: &Target, data: &'a [u8]) -> Option<Input<'a>> {
    let n = target.exts.len();
    let (index, rest) = if n > 256 {
        let (head, rest) = data.split_first_chunk::<2>()?;
        (u16::from_le_bytes(*head) as usize, rest)
    } else {
        let (head, rest) = data.split_first()?;
        (*head as usize, rest)
    };
    let (flags, body) = rest.split_first()?;
    Some(Input { ext: target.exts[index % n], flags: Flags(*flags), body })
}

//endregion

/*┌─────────────────╮
  | Isolated faults |
  └─────────────────╯*/
//region Isolated faults

static SAVED: AtomicUsize = AtomicUsize::new(0);

/// Whether an error message is `slerp-isolate`'s report of a child that
/// crashed, hung or ran out of memory, rather than a reader's own refusal.
pub fn is_isolated_fault(message: &str) -> bool {
    [
        "it crashed",
        "it used too much processor time",
        "it took longer than",
        "it stopped without an answer",
        "its answer was too large",
        "it failed inside",
    ]
    .iter()
    .any(|p| message.contains(p))
}

/// Keeps `data` when `message` says isolated code crashed or hung, with the
/// message beside it, at most 500 per process.  Returns whether it did.
pub fn isolated(data: &[u8], message: &str) -> bool {
    if !is_isolated_fault(message) || SAVED.fetch_add(1, Ordering::Relaxed) >= 500 {
        return false;
    }
    use std::hash::{DefaultHasher, Hash, Hasher};
    let mut h = DefaultHasher::new();
    data.hash(&mut h);
    let base = std::env::var_os("SLERP_FUZZ_DIR").map(PathBuf::from).unwrap_or_else(|| concat!(env!("CARGO_MANIFEST_DIR"), "/../local/fuzz").into());
    let dir = base.join("isolated").join(*NAME.lock().unwrap());
    if std::fs::create_dir_all(&dir).is_err() {
        return false;
    }
    let name = format!("{:016x}", h.finish());
    let _ = std::fs::write(dir.join(format!("{name}.bin")), data);
    let _ = std::fs::write(dir.join(format!("{name}.txt")), message);
    true
}

/// Notes an isolated fault in `result`'s error, if it is one.
pub fn note<T, E: std::fmt::Display>(data: &[u8], result: &Result<T, E>) {
    if let Err(e) = result {
        isolated(data, &e.to_string());
    }
}

//endregion

/*┌─────────────────╮
  | After loading   |
  └─────────────────╯*/
//region After loading

/// The start of a file, as `slerp_core::sniff_file` reads it.
pub fn head(body: &[u8]) -> &[u8] {
    &body[..body.len().min(4096)]
}

/// Does on the CPU what the thumbnailer does with a loaded asset before
/// (or instead of) the GPU: shrinks pictures, composes canvases, and for
/// scenes poses them and measures their bounds, which index vertex
/// attributes by the documented rule that each is empty or one per
/// position.
pub fn finish(asset: &Asset, size: u32) {
    match asset {
        Asset::Image(tex) => {
            slerp_textures::thumbnail(tex, size);
        }
        Asset::Gallery(items) => {
            for item in items.iter().take(4) {
                slerp_textures::thumbnail(&item.texture, size);
            }
        }
        Asset::Canvas(canvas) => {
            slerp_sprites::thumbnail(&**canvas, size);
        }
        Asset::Lut(lut) => {
            slerp_lut::thumbnail(&**lut, size);
        }
        Asset::Material(m) => {
            if let Some(p) = &m.preview {
                slerp_textures::thumbnail(p, size);
            }
        }
        Asset::Scene(scene) => scene_cpu(scene),
    }
}

/// The CPU half of drawing a scene: a rest pose and the scene's bounds.
pub fn scene_cpu(scene: &Scene) {
    let mut pose = Pose::rest(scene);
    pose.update_world(scene);
    let _ = scene.bounds(&pose);
}

/// Every extension the format table knows (the viewer's list: extensions
/// and the ones known by content), for targets that cover all formats.
pub fn all_extensions() -> Vec<&'static str> {
    let mut exts = slerp_formats::viewer_extensions();
    exts.sort_unstable();
    exts.dedup();
    exts
}

/// [`finish`] for a texture.
pub fn finish_texture(tex: &Texture, size: u32) {
    slerp_textures::thumbnail(tex, size);
}

//endregion
