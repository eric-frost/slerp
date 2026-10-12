//! The warm viewer.  Starting a viewer costs a GPU device (150 to 800 ms),
//! pipelines and lighting, so after its last window closes the process
//! stays loaded for `viewerKeepAliveSeconds` (30 by default, 0 for never),
//! without a window, and a later `slerp view` hands it the file instead of
//! starting a viewer of its own ([`hand_off`]).  The window then opens with
//! everything ready.
//!
//! It keeps the GPU, the renderer's pipelines and its lighting, and nothing
//! of the files it showed: the window's documents, textures and the
//! targets sized to the window go when the window closes (see `gfx::Kept`
//! and `Renderer::trim`), and freed memory goes back to the system.
//!
//! A viewer listens only while it has no window, one request at a time:
//! a `slerp view` while a window is open starts a viewer of its own, which
//! listens in turn after its window closes, unless the first is listening
//! by then.  Requests travel over a per-user socket beside the thumbnail
//! helper's (`slerp_helper::ViewerListener`), with its checks: the same
//! user only, a protocol version, and a client that gives up within half a
//! second and starts the viewer itself.  A viewer whose executable was
//! replaced (a reinstall) stops listening and exits.  The request carries
//! the command's window class and testing hooks (`SLERP_*`), which the
//! window then reads instead of the process's own environment.
//!
//! On Linux the thumbnail helper also starts a viewer like this when it
//! starts (`viewerWithHelper`, see `stay_ready` in `lib.rs`): a file manager
//! showing Slerp's thumbnails is a sign that a file may be opened next.
//! That viewer starts without a window and waits for as long as the helper
//! runs.
//!
//! On macOS, Finder's "Open With" reaches a running viewer as an Apple
//! Event; the warm viewer keeps receiving those while it waits, and opens
//! a window for them too.  Windows has no warm viewer.

use std::path::PathBuf;
use std::time::Duration;
use winit::event_loop::EventLoop;

/// What a window is opened for: the file, the window class and the
/// command's testing hooks.
#[derive(Clone, Debug, Default)]
pub struct Request {
    pub file: Option<PathBuf>,
    pub class: Option<String>,
    pub env: Vec<(String, String)>,
}

/// How long `slerp view` waits for a warm viewer to take its request
/// before starting a viewer of its own.
#[cfg(unix)]
const HANDOFF_TIMEOUT: Duration = Duration::from_millis(500);

/// Hands `file` to a warm viewer, as `slerp view` does first.  Returns
/// whether one took it; if not, the caller opens the viewer itself.
/// `SLERP_NO_HANDOFF` skips it, for tests that need a viewer process of
/// their own.
#[cfg(unix)]
pub fn hand_off(file: Option<&std::path::Path>, class: Option<&str>) -> bool {
    if std::env::var_os("SLERP_NO_HANDOFF").is_some() {
        return false;
    }
    let start = std::time::Instant::now();
    let env = crate::env::hooks_of_this_process();
    let timing = env.iter().any(|(k, _)| k == "SLERP_TIMING");
    let request = slerp_helper::ViewRequest {
        file: file.map(|f| std::path::absolute(f).unwrap_or_else(|_| f.to_path_buf())),
        class: class.map(str::to_string),
        env,
    };
    let taken = slerp_helper::hand_to_viewer(&request, HANDOFF_TIMEOUT).is_ok();
    if timing {
        let ms = start.elapsed().as_secs_f64() * 1000.0;
        let what = if taken { "handed to the warm viewer" } else { "no warm viewer; starting one" };
        eprintln!("slerp view: {what} in {ms:.1} ms");
    }
    taken
}

/// Waits for the next window to open: a `slerp view` command handed over,
/// or on macOS a file Finder opens.  It waits up to `keep`, and also for as
/// long as the process `helper` runs, when given (the thumbnail helper that
/// started this viewer).  `None` means it's time to exit: the time ran out,
/// another viewer is already listening, or the executable was replaced.
#[cfg(unix)]
pub fn wait(
    event_loop: &mut EventLoop<crate::shell::UserEvent>,
    keep: Duration,
    helper: Option<u32>,
) -> Option<Request> {
    let end = std::time::Instant::now() + keep;
    let stay = || std::time::Instant::now() < end || helper.is_some_and(alive);
    if !stay() {
        return None;
    }
    let listener = slerp_helper::ViewerListener::bind().ok()?;
    release_memory();
    #[cfg(not(target_os = "macos"))]
    {
        let _ = event_loop;
        while stay() && !listener.executable_replaced() {
            // A second at a time, to notice the helper exit.
            if let Some(r) = listener.accept(Duration::from_secs(1)) {
                return Some(from_view_request(r));
            }
        }
        None
    }
    #[cfg(target_os = "macos")]
    {
        // Apple Events (Finder's "Open With", the Dock's Quit) arrive through
        // the event loop, so it runs while the socket is watched.  It runs
        // anew rather than being pumped: winit keeps the last window's
        // `exit` until the loop is next run, and a pumped loop that still
        // has it returns at once without handling any event.
        use winit::platform::run_on_demand::EventLoopExtRunOnDemand;
        let mut waiting = Waiting { listener: &listener, stay: &stay, request: None };
        let _ = event_loop.run_app_on_demand(&mut waiting);
        waiting.request
    }
}

/// The event loop's handler while a macOS viewer waits without a window: it
/// looks for a file Finder sent and for a `slerp view` command on the
/// socket every [`Waiting::POLL`], and ends the loop when one comes or the
/// wait is over.
#[cfg(target_os = "macos")]
struct Waiting<'a> {
    listener: &'a slerp_helper::ViewerListener,
    stay: &'a dyn Fn() -> bool,
    request: Option<Request>,
}

#[cfg(target_os = "macos")]
impl Waiting<'_> {
    const POLL: Duration = Duration::from_millis(50);

    fn check(&mut self, event_loop: &winit::event_loop::ActiveEventLoop) {
        if self.request.is_some() || !(self.stay)() || self.listener.executable_replaced() {
            return event_loop.exit();
        }
        if let Some(file) = crate::macos::take_pending() {
            self.request = Some(Request { file: Some(file), ..Default::default() });
        } else if let Some(r) = self.listener.accept(Duration::from_millis(1)) {
            self.request = Some(from_view_request(r));
        }
        if self.request.is_some() {
            event_loop.exit();
        }
    }
}

#[cfg(target_os = "macos")]
impl winit::application::ApplicationHandler<crate::shell::UserEvent> for Waiting<'_> {
    fn new_events(&mut self, event_loop: &winit::event_loop::ActiveEventLoop, _: winit::event::StartCause) {
        self.check(event_loop);
    }

    fn resumed(&mut self, _: &winit::event_loop::ActiveEventLoop) {}

    fn window_event(
        &mut self,
        _: &winit::event_loop::ActiveEventLoop,
        _: winit::window::WindowId,
        _: winit::event::WindowEvent,
    ) {
    }

    fn about_to_wait(&mut self, event_loop: &winit::event_loop::ActiveEventLoop) {
        event_loop.set_control_flow(winit::event_loop::ControlFlow::WaitUntil(std::time::Instant::now() + Self::POLL));
    }
}

#[cfg(not(unix))]
pub fn wait(
    _event_loop: &mut EventLoop<crate::shell::UserEvent>,
    _keep: Duration,
    _helper: Option<u32>,
) -> Option<Request> {
    None
}

/// Whether process `pid` still runs.
#[cfg(target_os = "linux")]
fn alive(pid: u32) -> bool {
    let Ok(pid) = i32::try_from(pid) else { return false };
    // SAFETY: signal 0 only checks that the process exists and may be
    // signalled.
    unsafe { libc::kill(pid, 0) == 0 }
}

/// Only Linux starts viewers with the helper.
#[cfg(all(unix, not(target_os = "linux")))]
fn alive(_pid: u32) -> bool {
    false
}

#[cfg(unix)]
fn from_view_request(r: slerp_helper::ViewRequest) -> Request {
    Request { file: r.file, class: r.class, env: r.env }
}

/// Gives memory the closed window's files used back to the system.  glibc
/// keeps freed memory for reuse, which for a warm viewer that just showed a
/// large model would mean holding on to hundreds of megabytes.  Trimmed
/// again a moment later, for what threads of the window let go of last.
#[cfg(unix)]
fn release_memory() {
    #[cfg(target_os = "linux")]
    {
        // SAFETY: malloc_trim has no preconditions.
        unsafe { libc::malloc_trim(0) };
        let _ = std::thread::Builder::new().name("slerp-trim".into()).spawn(|| {
            std::thread::sleep(Duration::from_secs(2));
            // SAFETY: as above.
            unsafe { libc::malloc_trim(0) };
        });
    }
}

/// Makes the allocator give freed memory back to the system rather than
/// keep it for reuse, so that a warm viewer lets go of a closed model.
/// By default glibc raises its thresholds as large blocks are freed, and
/// keeps up to 64 MB unused at the end of each thread's heap, which
/// `malloc_trim` does not touch: a warm viewer grew to over 500 MB after a
/// few large models.  Fixing the threshold for returning the end of a heap
/// at 1 MB brings that back to its idle size (about 120 MB) at little cost
/// to loading.  Blocks up to 32 MB (the highest glibc would use by itself)
/// still come from the heaps, since returning each of those at once
/// doubled the time to upload a textured model.  Called first thing,
/// before other threads allocate.
pub fn return_freed_memory() {
    #[cfg(target_os = "linux")]
    // SAFETY: mallopt only changes the allocator's settings; it runs before
    // other threads start.
    unsafe {
        libc::mallopt(libc::M_MMAP_THRESHOLD, 32 << 20);
        libc::mallopt(libc::M_TRIM_THRESHOLD, 1 << 20);
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    #[test]
    fn tells_running_processes_from_ended_ones() {
        assert!(super::alive(std::process::id()));
        let mut child = std::process::Command::new("true").spawn().unwrap();
        let pid = child.id();
        child.wait().unwrap();
        assert!(!super::alive(pid));
        assert!(!super::alive(u32::MAX));
    }
}
