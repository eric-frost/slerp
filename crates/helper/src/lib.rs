//! The resident thumbnail helper: a per-user process that keeps the GPU
//! loaded between thumbnails, and the client that thumbnailer processes use
//! to reach it.
//!
//! File managers start one thumbnailer process per file, and starting a
//! Vulkan device costs 100-800 ms, so `slerp thumbnail` hands 3D work to
//! a long-lived helper over a Unix socket at
//! `$XDG_RUNTIME_DIR/slerp/socket`.  This crate is that socket: the
//! client ([`request`]), the server ([`Server`], [`serve`]) and the wire
//! protocol between them.  It knows nothing about rendering; the server runs
//! whatever handler its caller gives it.
//!
//! - The client connects, sends a [`Request`] and waits for a [`Reply`] until
//!   the request's deadline.  When no helper is listening it starts one and
//!   returns [`ClientError::NotRunning`] at once, so the caller renders that
//!   file in-process while the helper starts.  Every [`ClientError`] means
//!   the same thing to the caller: render in-process if there is time left.
//! - The server takes a per-user lock, binds the socket, serves requests in
//!   parallel, abandons replies past their deadline, and exits when idle,
//!   when stopped, when a client from a newer build talks to it, or when its
//!   executable is replaced on disk.  If a job overruns its deadline by a
//!   large margin it assumes the GPU hung and exits at once, so the next
//!   client starts a fresh helper.
//! - Only the same user can talk to a helper: the socket directory is 0700
//!   and both ends check the other's uid with `SO_PEERCRED` (`getpeereid`
//!   on macOS).
//!
//! The same directory holds the warm viewer's socket: `slerp view` hands
//! its file to a viewer that stayed loaded after its window closed
//! ([`hand_to_viewer`], [`ViewerListener`]), with the same rules.
//!
//! On macOS the socket lives in the user's private temporary folder
//! (`$TMPDIR/slerp/socket`), since there is no `$XDG_RUNTIME_DIR`.
//! Windows has no helper: [`request`] fails at once with
//! [`ClientError::Unsupported`], so thumbnails render in-process, and
//! Explorer's thumbnail host is a long-lived process anyway.
//!
//! The `thumbnail` command uses it like this:
//!
//! ```no_run
//! use slerp_helper::{ClientError, Reply, Request, request, spawn_detached};
//! use std::process::Command;
//! use std::time::{Duration, Instant};
//!
//! let req = Request {
//!     path: "model.glb".into(),
//!     size: 256,
//!     frames: 0,
//!     deadline: Instant::now() + Duration::from_millis(1500),
//! };
//! let start = || spawn_detached(Command::new(std::env::current_exe()?).arg("helper"));
//! match request(&req, start) {
//!     Ok(Reply::Png(png)) => { /* write png to the output file */ }
//!     Ok(_) => { /* no thumbnail */ }
//!     Err(ClientError::TimedOut) => { /* too late to do anything else */ }
//!     Err(_) => { /* render in-process */ }
//! }
//! ```
//!
//! And `slerp helper` like this, taking the lock before starting the GPU
//! so that helpers which lose a start race exit cheaply:
//!
//! ```no_run
//! use slerp_helper::{Reply, ServeError, ServeOptions, Server};
//!
//! let server = match Server::bind(ServeOptions::default()) {
//!     Ok(server) => server,
//!     Err(ServeError::AlreadyRunning) => return,
//!     Err(err) => panic!("{err}"),
//! };
//! // Start the GPU here, or lazily in the handler.
//! let exit = server.run(|_req| Reply::NoThumbnail);
//! ```
//!
//! Set `SLERP_LOG` to any value to log events to stderr.  An
//! auto-started helper's stderr is the file `SLERP_LOG` names when it is
//! an absolute path, and /dev/null otherwise (see [`spawn_detached`]).

#[cfg(unix)]
mod client;
#[cfg(unix)]
mod handoff;
#[cfg(unix)]
mod protocol;
#[cfg(unix)]
mod server;
#[cfg(unix)]
mod sys;
mod types;
#[cfg(not(unix))]
mod unsupported;

#[cfg(unix)]
pub use client::{request, request_at, spawn_detached, stop, stop_at};
#[cfg(unix)]
pub use handoff::{
    ClientWatch, HandoffError, VIEWER_PROTOCOL_VERSION, ViewRequest, ViewerListener, hand_to_viewer, hand_to_viewer_at,
    viewer_socket_path,
};
#[cfg(unix)]
pub use server::{Server, serve};
pub use types::{ClientError, Exit, ServeError, ServeOptions};
#[cfg(not(unix))]
pub use unsupported::{Server, request, request_at, serve, spawn_detached, stop, stop_at};

use std::ffi::OsString;
use std::path::PathBuf;
use std::time::Instant;

/// The protocol version both ends must share.  A client and helper from
/// different builds that disagree get a clean [`ClientError::VersionMismatch`],
/// and a helper older than its client exits so the next request starts the
/// newly installed one.
pub const PROTOCOL_VERSION: u8 = 1;

/// The largest thumbnail side, in pixels, a request may ask for.
pub const MAX_SIZE: u32 = 8192;

/// The most turntable frames a request may ask for.
pub const MAX_FRAMES: u32 = 1024;

/// The exit status of a helper whose watchdog found a job stuck far past
/// its deadline, which usually means a hung GPU.
pub const HANG_EXIT_CODE: i32 = 75;

/// A thumbnail request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Request {
    /// The file to thumbnail.  The client makes it absolute before sending
    /// it, since the helper's working directory is `/`.
    pub path: PathBuf,
    /// The thumbnail's longer side in pixels, 1 to [`MAX_SIZE`].
    pub size: u32,
    /// How many turntable frames to render, for the viewer plugin, up to
    /// [`MAX_FRAMES`]; 0 asks for one still picture as PNG.
    pub frames: u32,
    /// When the caller stops waiting.  The client gives up at this moment,
    /// and the helper skips the job if it hasn't started by then or drops
    /// its result if it finishes later.  The helper's handler sees the same
    /// moment in its own process (to within the time the request took to
    /// arrive), so it can choose a cheaper render when time is short.
    pub deadline: Instant,
}

/// What the helper answers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reply {
    /// An encoded PNG, which the client writes to the thumbnail file as is.
    Png(Vec<u8>),
    /// Uncompressed RGBA frames for the viewer plugin's turntable, each
    /// `width * height * 4` bytes.
    Frames { width: u32, height: u32, rgba: Vec<Vec<u8>> },
    /// The file isn't something Slerp makes thumbnails of, or holds
    /// nothing to show.  Rendering in-process would give the same answer.
    NoThumbnail,
    /// The handler failed or panicked.  The message is for logs.  Whether
    /// rendering in-process is worth trying depends on why, which only the
    /// handler knows; a handler that wants the client to retry in-process
    /// (after a GPU error, say) should say so in a way its caller recognises.
    Failed(String),
}

/// The socket every client and helper of this user meets at:
/// `$XDG_RUNTIME_DIR/slerp/socket`.  The `slerp` directory is
/// created with mode 0700 by whichever side needs it first.
///
/// Returns `None` when `$XDG_RUNTIME_DIR` is unset, relative or doesn't
/// exist.  GNOME's thumbnail sandbox passes the variable through but hides
/// `/run`, so this is how the client knows at once that no helper can be
/// reached there.
///
/// macOS has no runtime directory, so the socket goes in the user's own
/// temporary folder (`$TMPDIR`, else the one `confstr` names, both under
/// `/var/folders` and readable only by the user).  Windows has none.
pub fn socket_path() -> Option<PathBuf> {
    if cfg!(windows) {
        return None;
    }
    let dir = runtime_dir(std::env::var_os("XDG_RUNTIME_DIR"));
    #[cfg(target_os = "macos")]
    let dir = dir.or_else(|| runtime_dir(std::env::var_os("TMPDIR"))).or_else(darwin_user_temp_dir);
    dir.map(|dir| dir.join("slerp").join("socket"))
}

/// The per-user temporary folder macOS keeps under `/var/folders`, for
/// processes started without `$TMPDIR`.
#[cfg(target_os = "macos")]
fn darwin_user_temp_dir() -> Option<PathBuf> {
    use std::os::unix::ffi::OsStringExt;
    let mut buf = vec![0u8; 1024];
    // SAFETY: buf is valid for writes of buf.len() bytes, and confstr writes
    // a NUL-terminated string of at most that many.
    let len = unsafe { libc::confstr(libc::_CS_DARWIN_USER_TEMP_DIR, buf.as_mut_ptr().cast(), buf.len()) };
    if len == 0 || len > buf.len() {
        return None;
    }
    buf.truncate(len - 1);
    runtime_dir(Some(OsString::from_vec(buf)))
}

/// The runtime directory `value` names, if it is an existing absolute
/// directory.
fn runtime_dir(value: Option<OsString>) -> Option<PathBuf> {
    let dir = PathBuf::from(value?);
    if !dir.is_absolute() {
        return None;
    }
    std::fs::metadata(&dir).ok()?.is_dir().then_some(dir)
}

/*┌─────────────────╮
| Logging         |
└─────────────────╯*/
//region Logging

/// Whether `SLERP_LOG` asks for log lines, read once per process.
#[cfg(unix)]
fn log_enabled() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var_os("SLERP_LOG").is_some_and(|v| !v.is_empty()))
}

/// Writes one log line to stderr, ignoring failures: a log line must never
/// take the helper down, and `eprintln!` panics when stderr is a closed pipe.
#[cfg(unix)]
fn write_log(args: std::fmt::Arguments) {
    use std::io::Write;
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
    let (secs, ms) = (now.as_secs() % 86_400, now.subsec_millis());
    let _ = writeln!(
        std::io::stderr().lock(),
        "{:02}:{:02}:{:02}.{ms:03} slerp-helper[{}] {args}",
        secs / 3600,
        secs / 60 % 60,
        secs % 60,
        std::process::id()
    );
}

/// Logs a line when `SLERP_LOG` is set.  The message is only formatted
/// when logging is on.
#[cfg(unix)]
macro_rules! log {
    ($($arg:tt)*) => {
        if $crate::log_enabled() {
            $crate::write_log(format_args!($($arg)*))
        }
    };
}
#[cfg(unix)]
pub(crate) use log;

//endregion

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn runtime_dir_needs_an_existing_absolute_directory() {
        assert_eq!(runtime_dir(None), None);
        assert_eq!(runtime_dir(Some("".into())), None);
        assert_eq!(runtime_dir(Some("relative/dir".into())), None);
        assert_eq!(runtime_dir(Some("/nonexistent/slerp-runtime".into())), None);
        // A file isn't a directory.
        assert_eq!(runtime_dir(Some("/proc/self/status".into())), None);
        assert_eq!(runtime_dir(Some("/tmp".into())), Some(PathBuf::from("/tmp")));
    }
}
