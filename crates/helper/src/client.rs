//! The client side: what a thumbnailer process does to get a thumbnail from
//! the helper, start one when none is running, or stop one.

use crate::protocol::{self, HEADER_LEN, kind};
use crate::sys;
use crate::types::ClientError;
use crate::{PROTOCOL_VERSION, Reply, Request, log, socket_path};
use std::fs::{DirBuilder, OpenOptions};
use std::io::{self, ErrorKind};
use std::os::unix::fs::DirBuilderExt;
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// How long a client waits for room in a helper's connection queue.  A full
/// queue means the helper is stopped or far behind, so rendering in-process
/// is quicker.
const CONNECT_TIMEOUT: Duration = Duration::from_millis(50);

/// Asks this user's helper (at [`socket_path`]) for a thumbnail, waiting
/// for the reply until `req.deadline`.
///
/// When no helper is listening, it calls `start` to start one and returns
/// [`ClientError::NotRunning`] at once, so the caller renders this file
/// in-process while the helper starts; `start` normally runs
/// [`spawn_detached`] on `current_exe() helper`, which takes about 1 ms.
/// Apart from that, it returns within microseconds whenever no helper can be
/// reached, including inside GNOME's sandbox.  A round trip to a running
/// helper costs about 60 us plus the handler's work.
pub fn request(req: &Request, start: impl FnOnce() -> io::Result<()>) -> Result<Reply, ClientError> {
    let socket = socket_path().ok_or(ClientError::NoRuntimeDir)?;
    request_at(&socket, req, start)
}

/// [`request`] with the socket at `socket`, for tests and for callers that
/// put the socket elsewhere.
pub fn request_at(socket: &Path, req: &Request, start: impl FnOnce() -> io::Result<()>) -> Result<Reply, ClientError> {
    // 1. Encode first, so a bad request fails before anything is started.
    let path =
        std::path::absolute(&req.path).map_err(|_| ClientError::InvalidRequest("path can't be made absolute"))?;
    let message = protocol::encode_request(&path, req, Instant::now()).map_err(ClientError::InvalidRequest)?;

    // 2. Connect, or start a helper when none is listening.
    let stream = match connect(socket, req.deadline) {
        Ok(stream) => stream,
        Err(ConnectError::Absent) => return Err(start_helper(socket, start)),
        Err(ConnectError::Other(err)) => return Err(err),
    };

    // 3. Send and wait.  A helper that refuses the connection (busy, or
    // another version) may answer and hang up before the request is
    // written, so a failed send still reads whatever answer is waiting.
    if let Err(err) = sys::send_all(&stream, &message, req.deadline) {
        let refused = matches!(err.kind(), ErrorKind::BrokenPipe | ErrorKind::ConnectionReset);
        return match read_reply(&stream, req.deadline) {
            Err(why @ (ClientError::Busy | ClientError::VersionMismatch { .. })) if refused => Err(why),
            _ => Err(err.into()),
        };
    }
    read_reply(&stream, req.deadline)
}

/// Asks this user's helper to exit.  Returns whether one was running and
/// agreed; it exits once its in-flight requests are done.  Install scripts
/// use it so the next thumbnail starts the newly installed build.
pub fn stop(timeout: Duration) -> Result<bool, ClientError> {
    match socket_path() {
        Some(socket) => stop_at(&socket, timeout),
        None => Ok(false),
    }
}

/// [`stop`] with the socket at `socket`.
pub fn stop_at(socket: &Path, timeout: Duration) -> Result<bool, ClientError> {
    let deadline = Instant::now() + timeout;
    let stream = match connect(socket, deadline) {
        Ok(stream) => stream,
        Err(ConnectError::Absent) => return Ok(false),
        Err(ConnectError::Other(err)) => return Err(err),
    };
    sys::send_all(&stream, &protocol::header(kind::STOP, 0), deadline)?;
    let header = read_header(&stream, deadline)?;
    match header.kind {
        kind::OK => Ok(true),
        kind::BUSY => Err(ClientError::Busy),
        _ => Err(ClientError::Protocol("unexpected reply to stop")),
    }
}

enum ConnectError {
    /// Nothing is listening: no socket file, or a stale one left by a
    /// helper that died.
    Absent,
    Other(ClientError),
}

/// Connects and checks that the helper runs as this user.
fn connect(socket: &Path, deadline: Instant) -> Result<UnixStream, ConnectError> {
    let left = sys::time_left(deadline).map_err(|_| ConnectError::Other(ClientError::TimedOut))?;
    let stream = match sys::connect(socket, left.min(CONNECT_TIMEOUT)) {
        Ok(stream) => stream,
        Err(err) => {
            return Err(match err.raw_os_error() {
                Some(libc::ENOENT | libc::ECONNREFUSED) => ConnectError::Absent,
                Some(libc::EAGAIN) => ConnectError::Other(ClientError::Busy),
                _ => ConnectError::Other(ClientError::Unreachable(err)),
            });
        }
    };
    // The directory is private, so this only fails if someone replaced it;
    // talking to another user's helper would hand them our file names.
    match sys::peer_uid(&stream) {
        Ok(uid) if uid == sys::euid() => Ok(stream),
        Ok(uid) => Err(ConnectError::Other(ClientError::Unreachable(io::Error::new(
            ErrorKind::PermissionDenied,
            format!("the helper socket belongs to uid {uid}"),
        )))),
        Err(err) => Err(ConnectError::Other(ClientError::Unreachable(err))),
    }
}

/// Starts a helper when its socket's directory can hold one, so a missing or
/// read-only runtime directory doesn't start a doomed helper per thumbnail.
fn start_helper(socket: &Path, start: impl FnOnce() -> io::Result<()>) -> ClientError {
    let Some(dir) = socket.parent() else { return ClientError::NoRuntimeDir };
    match DirBuilder::new().mode(0o700).create(dir) {
        Ok(()) => {}
        Err(err) if err.kind() == ErrorKind::AlreadyExists => {}
        Err(_) => return ClientError::NoRuntimeDir,
    }
    if !sys::can_write_dir(dir) {
        return ClientError::NoRuntimeDir;
    }
    match start() {
        Ok(()) => {
            log!("no helper at {}; started one", socket.display());
            ClientError::NotRunning
        }
        Err(err) => {
            log!("no helper at {}; starting one failed: {err}", socket.display());
            ClientError::StartFailed(err)
        }
    }
}

fn read_header(stream: &UnixStream, deadline: Instant) -> Result<protocol::Header, ClientError> {
    let mut bytes = [0; HEADER_LEN];
    sys::read_exact_by(stream, &mut bytes, deadline)?;
    let header = protocol::parse_header(&bytes).ok_or(ClientError::Protocol("bad magic"))?;
    if header.version != PROTOCOL_VERSION {
        log!("the helper speaks protocol version {}, this client {PROTOCOL_VERSION}", header.version);
        return Err(ClientError::VersionMismatch { helper: header.version });
    }
    Ok(header)
}

fn read_reply(stream: &UnixStream, deadline: Instant) -> Result<Reply, ClientError> {
    let header = read_header(stream, deadline)?;
    let len = header.len as usize;
    match header.kind {
        kind::PNG if header.len <= protocol::MAX_PNG => Ok(Reply::Png(sys::read_vec_by(stream, len, deadline)?)),
        kind::FRAMES => read_frames(stream, header.len, deadline),
        kind::NO_THUMBNAIL if len == 0 => Ok(Reply::NoThumbnail),
        kind::FAILED if len <= protocol::MAX_MESSAGE => {
            let text = sys::read_vec_by(stream, len, deadline)?;
            Ok(Reply::Failed(String::from_utf8_lossy(&text).into_owned()))
        }
        kind::EXPIRED => Err(ClientError::TimedOut),
        kind::BUSY => Err(ClientError::Busy),
        _ => Err(ClientError::Protocol("unexpected reply kind or length")),
    }
}

/// Reads turntable frames straight into one buffer each, checking the
/// dimensions against the length before allocating anything.
fn read_frames(stream: &UnixStream, len: u32, deadline: Instant) -> Result<Reply, ClientError> {
    let mut fixed = [0; protocol::FRAMES_FIXED];
    if (len as usize) < fixed.len() {
        return Err(ClientError::Protocol("frames reply too short"));
    }
    sys::read_exact_by(stream, &mut fixed, deadline)?;
    let word = |i: usize| u32::from_le_bytes([fixed[i], fixed[i + 1], fixed[i + 2], fixed[i + 3]]);
    let (width, height, count) = (word(0), word(4), word(8));
    let Some((frame, total)) = protocol::frames_len(width, height, u64::from(count)) else {
        return Err(ClientError::Protocol("frame dimensions out of range"));
    };
    if u64::from(len) != protocol::FRAMES_FIXED as u64 + total {
        return Err(ClientError::Protocol("frames reply length doesn't match its dimensions"));
    }
    let rgba = (0..count).map(|_| sys::read_vec_by(stream, frame as usize, deadline)).collect::<io::Result<_>>()?;
    Ok(Reply::Frames { width, height, rgba })
}

/// Starts `command` as a daemon that outlives its caller: in a new session,
/// with its working directory at `/`, stdin and stdout on /dev/null, and
/// stderr on /dev/null or on the file `SLERP_LOG` names when that is an
/// absolute path (set on `command` or inherited).  It returns as soon as the
/// program has been executed.
///
/// The child forks once more and the middle process exits, so the helper is
/// adopted by init (or the session's subreaper) and a long-running caller,
/// such as a plugin inside Dolphin, isn't left with a zombie.  The standard
/// streams matter too: launchers like tumbler read a thumbnailer's output
/// until it closes, so a helper holding them open would stall every
/// thumbnail until it exits.
pub fn spawn_detached(command: &mut Command) -> io::Result<()> {
    let log = match command.get_envs().find(|(key, _)| *key == "SLERP_LOG") {
        Some((_, value)) => value.map(ToOwned::to_owned),
        None => std::env::var_os("SLERP_LOG"),
    };
    let stderr = match log {
        Some(path) if Path::new(&path).is_absolute() => {
            OpenOptions::new().create(true).append(true).open(&path).map_or_else(|_| Stdio::null(), Stdio::from)
        }
        _ => Stdio::null(),
    };
    command.stdin(Stdio::null()).stdout(Stdio::null()).stderr(stderr).current_dir("/");
    // SAFETY: the closure runs between fork and exec, where only
    // async-signal-safe calls are allowed; setsid, fork and _exit are.
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() < 0 {
                return Err(io::Error::last_os_error());
            }
            match libc::fork() {
                -1 => Err(io::Error::last_os_error()),
                0 => Ok(()),
                _ => libc::_exit(0),
            }
        });
    }
    // spawn returns once the grandchild has executed the program (or reports
    // why it couldn't); waiting reaps the middle process, which has exited.
    command.spawn()?.wait()?;
    Ok(())
}
