//! The public types of the client and the server, shared by the Unix
//! implementation and the stand-in on platforms without one.

use crate::PROTOCOL_VERSION;
use std::io::{self, ErrorKind};
use std::path::PathBuf;
use std::thread;
use std::time::Duration;

/// Why the helper didn't produce a reply.  Every variant means the same
/// thing to a thumbnailer: render in-process if there is time left.  They
/// are told apart for logs, tests and the rare caller that cares.
#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    /// There's no runtime directory to meet a helper in: `$XDG_RUNTIME_DIR`
    /// is unset, missing (as in GNOME's thumbnail sandbox) or unwritable.
    /// No helper was started, since none could listen.
    #[error("no usable runtime directory for the helper socket")]
    NoRuntimeDir,
    /// No helper was listening, so `start` was called to start one for the
    /// next request.
    #[error("the helper wasn't running; started one")]
    NotRunning,
    /// No helper was listening and `start` failed.
    #[error("the helper wasn't running and couldn't be started: {0}")]
    StartFailed(io::Error),
    /// The helper's connection queue is full, or it's serving as many
    /// connections as it allows.
    #[error("the helper is busy")]
    Busy,
    /// The socket exists but couldn't be used: permissions, or a socket that
    /// belongs to another user.
    #[error("the helper socket can't be used: {0}")]
    Unreachable(io::Error),
    /// The helper speaks another protocol version.  A helper older than this
    /// client exits by itself, so the next request starts the new build.
    #[error("the helper speaks protocol version {helper}, not {}", PROTOCOL_VERSION)]
    VersionMismatch { helper: u8 },
    /// The helper closed the connection without replying: it crashed, was
    /// killed, refused the request or is shutting down.
    #[error("the helper closed the connection")]
    Disconnected,
    /// The deadline passed without a reply.  The helper drops the job.
    #[error("the helper didn't reply in time")]
    TimedOut,
    /// The request can't be sent: the path can't be made absolute or is too
    /// long, or the size or frame count is out of range.
    #[error("invalid request: {0}")]
    InvalidRequest(&'static str),
    /// The reply broke the protocol.
    #[error("malformed reply: {0}")]
    Protocol(&'static str),
    /// This platform has no helper (Windows, where Explorer's thumbnail
    /// host is long-lived anyway), so thumbnails render in-process.
    #[error("this platform has no thumbnail helper")]
    Unsupported,
    #[error(transparent)]
    Io(io::Error),
}

impl From<io::Error> for ClientError {
    fn from(err: io::Error) -> Self {
        match err.kind() {
            ErrorKind::TimedOut | ErrorKind::WouldBlock => ClientError::TimedOut,
            ErrorKind::UnexpectedEof | ErrorKind::BrokenPipe | ErrorKind::ConnectionReset => ClientError::Disconnected,
            _ => ClientError::Io(err),
        }
    }
}

/// How the helper runs.
#[derive(Clone, Debug)]
pub struct ServeOptions {
    /// Where to listen; `None` means [`crate::socket_path`].  The directory
    /// is created with mode 0700 if missing, and must belong to this user.
    pub socket: Option<PathBuf>,
    /// Exit after this long without a connection, to free the GPU's memory.
    /// Default 120 s (the design's `helperIdleSeconds`).
    pub idle: Duration,
    /// How many jobs run at once.  Default: half the CPU threads, 1 to 8.
    /// The handler is expected to serialise its own GPU submissions.
    pub threads: usize,
    /// How long past its deadline a job may run before the helper decides
    /// the handler is stuck (usually a hung GPU) and exits at once with
    /// [`crate::HANG_EXIT_CODE`], so the next client starts a fresh helper.
    /// Default 10 s.
    pub hang_margin: Duration,
    /// How many connections are served at once.  Clients beyond it get
    /// [`crate::ClientError::Busy`] and render in-process.  Default 64.
    pub max_connections: usize,
    /// Exit when the running executable's file is replaced or removed, so a
    /// reinstall takes effect with the next thumbnail rather than after the
    /// idle time.  Default true.
    pub exit_when_replaced: bool,
}

impl Default for ServeOptions {
    fn default() -> Self {
        let cpus = thread::available_parallelism().map_or(2, |n| n.get());
        ServeOptions {
            socket: None,
            idle: Duration::from_secs(120),
            threads: (cpus / 2).clamp(1, 8),
            hang_margin: Duration::from_secs(10),
            max_connections: 64,
            exit_when_replaced: true,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ServeError {
    /// No `$XDG_RUNTIME_DIR` (on macOS, no temporary folder) to put the
    /// socket in.
    #[error("no runtime directory for the helper socket")]
    NoRuntimeDir,
    /// Another helper holds the lock: it is serving, starting or finishing.
    /// This is the normal outcome for helpers that lose a start race, which
    /// should exit quietly.
    #[error("another helper is already running")]
    AlreadyRunning,
    /// The socket's directory belongs to someone else or isn't a directory.
    #[error("{0} isn't a private directory of this user")]
    UnsafeDirectory(PathBuf),
    /// This platform has no helper (Windows).
    #[error("this platform has no thumbnail helper")]
    Unsupported,
    #[error(transparent)]
    Io(#[from] io::Error),
}

/// Why [`crate::Server::run`] returned.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Exit {
    /// No connection for [`ServeOptions::idle`].
    Idle,
    /// A client asked it to stop.
    Stopped,
    /// A client from a build with a newer protocol connected.
    Superseded,
    /// The executable was replaced or removed on disk.
    Replaced,
}
