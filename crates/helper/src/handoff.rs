//! Handing `slerp view` to a viewer that is already running: after its
//! last window closes, a viewer process can stay loaded for a while, with
//! its GPU, compiled pipelines and theme ready, and listen on
//! `$XDG_RUNTIME_DIR/slerp/viewer` (beside the helper's socket).  A new
//! `slerp view` sends it the file and its arguments ([`ViewRequest`]) and
//! exits, and the warm viewer opens a window at once.  When nothing
//! listens, or anything goes wrong, [`hand_to_viewer`] fails fast and the
//! command starts a viewer of its own.
//!
//! The rules are the helper's: only the same user may talk to a viewer
//! (`SO_PEERCRED`, a private directory), both ends check each other, a
//! version byte keeps different builds apart, and an exclusive `flock` on
//! `viewer.lock` makes one process the listener.  A viewer listens only
//! while it has no window, so a second `slerp view` while one is open
//! simply starts its own.
//!
//! The wire format: the client sends `SLVW`, the version, a little-endian
//! u32 length and that many bytes of fields (a tag byte, a u32 length and
//! the value each: 1 the file, 2 the window class, 3 an environment
//! variable as `NAME=value`); the viewer answers `SLVW`, the version and a
//! status byte, 0 when it takes the request.

use crate::server::{ExeId, private_dir};
use crate::sys;
use crate::types::ServeError;
use std::ffi::OsString;
use std::fs::{File, OpenOptions, TryLockError};
use std::io::{self, ErrorKind};
use std::os::fd::AsRawFd;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// The version both ends must share.
pub const VIEWER_PROTOCOL_VERSION: u8 = 1;

const MAGIC: &[u8; 4] = b"SLVW";

/// The most a request may hold, far more than a path, a class and a few
/// variables need.
const MAX_REQUEST: u32 = 1 << 20;

const TAG_FILE: u8 = 1;
const TAG_CLASS: u8 = 2;
const TAG_ENV: u8 = 3;

/// What a `slerp view` command asks a warm viewer for.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ViewRequest {
    /// The file to open, absolute (the viewer's working directory is its
    /// own), or `None` for an empty window.
    pub file: Option<PathBuf>,
    /// The window class or Wayland app id, when not the default.
    pub class: Option<String>,
    /// Environment variables of the command that matter to the window
    /// (the viewer's testing hooks), as name and value.
    pub env: Vec<(String, String)>,
}

/// Why a request didn't reach a warm viewer.  The caller starts a viewer
/// of its own whatever the reason.
#[derive(Debug, thiserror::Error)]
pub enum HandoffError {
    #[error("no runtime directory")]
    NoRuntimeDir,
    #[error("no warm viewer is listening")]
    NotRunning,
    #[error("the warm viewer turned the request down")]
    Refused,
    #[error("the warm viewer speaks version {0}")]
    VersionMismatch(u8),
    #[error("the warm viewer's socket belongs to someone else")]
    WrongUser,
    #[error(transparent)]
    Io(#[from] io::Error),
}

/// Where warm viewers listen: `viewer` beside the helper's socket.
pub fn viewer_socket_path() -> Option<PathBuf> {
    Some(crate::socket_path()?.with_file_name("viewer"))
}

/*┌─────────────────╮
| Client          |
└─────────────────╯*/
//region Client

/// Sends `req` to the warm viewer and waits up to `timeout` for it to take
/// it.  Fails at once when none is listening.
pub fn hand_to_viewer(req: &ViewRequest, timeout: Duration) -> Result<(), HandoffError> {
    let socket = viewer_socket_path().ok_or(HandoffError::NoRuntimeDir)?;
    hand_to_viewer_at(&socket, req, timeout)
}

/// [`hand_to_viewer`] at a given socket path, for tests.
pub fn hand_to_viewer_at(socket: &Path, req: &ViewRequest, timeout: Duration) -> Result<(), HandoffError> {
    let deadline = Instant::now() + timeout;
    let stream = match sys::connect(socket, timeout) {
        Ok(s) => s,
        Err(e) if matches!(e.kind(), ErrorKind::NotFound | ErrorKind::ConnectionRefused) => {
            return Err(HandoffError::NotRunning);
        }
        Err(e) => return Err(e.into()),
    };
    if sys::peer_uid(&stream)? != sys::euid() {
        return Err(HandoffError::WrongUser);
    }
    let payload = encode(req);
    let mut out = Vec::with_capacity(9 + payload.len());
    out.extend_from_slice(MAGIC);
    out.push(VIEWER_PROTOCOL_VERSION);
    out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    out.extend_from_slice(&payload);
    sys::send_all(&stream, &out, deadline)?;
    let mut reply = [0u8; 6];
    match sys::read_exact_by(&stream, &mut reply, deadline) {
        Ok(()) => {}
        // The viewer closed without answering: it is busy or going away.
        Err(e) if matches!(e.kind(), ErrorKind::UnexpectedEof | ErrorKind::ConnectionReset) => {
            return Err(HandoffError::Refused);
        }
        Err(e) => return Err(e.into()),
    }
    if &reply[..4] != MAGIC {
        return Err(HandoffError::Refused);
    }
    if reply[4] != VIEWER_PROTOCOL_VERSION {
        return Err(HandoffError::VersionMismatch(reply[4]));
    }
    if reply[5] != 0 {
        return Err(HandoffError::Refused);
    }
    Ok(())
}

fn encode(req: &ViewRequest) -> Vec<u8> {
    let mut out = Vec::new();
    let mut field = |tag: u8, value: &[u8]| {
        out.push(tag);
        out.extend_from_slice(&(value.len() as u32).to_le_bytes());
        out.extend_from_slice(value);
    };
    if let Some(f) = &req.file {
        field(TAG_FILE, f.as_os_str().as_bytes());
    }
    if let Some(c) = &req.class {
        field(TAG_CLASS, c.as_bytes());
    }
    for (k, v) in &req.env {
        field(TAG_ENV, format!("{k}={v}").as_bytes());
    }
    out
}

/// Reads a request's fields, rejecting anything malformed.  Unknown tags
/// are skipped, for newer clients of the same version.
fn decode(payload: &[u8]) -> Option<ViewRequest> {
    let mut req = ViewRequest::default();
    let mut rest = payload;
    while !rest.is_empty() {
        let tag = *rest.first()?;
        let len = u32::from_le_bytes(rest.get(1..5)?.try_into().ok()?) as usize;
        let value = rest.get(5..5usize.checked_add(len)?)?;
        rest = &rest[5 + len..];
        match tag {
            TAG_FILE => {
                if value.is_empty() || value.contains(&0) || value[0] != b'/' {
                    return None;
                }
                req.file = Some(PathBuf::from(OsString::from_vec(value.to_vec())));
            }
            TAG_CLASS => req.class = Some(String::from_utf8(value.to_vec()).ok()?),
            TAG_ENV => {
                let text = std::str::from_utf8(value).ok()?;
                let (k, v) = text.split_once('=')?;
                req.env.push((k.to_string(), v.to_string()));
            }
            _ => {}
        }
    }
    Some(req)
}

//endregion

/*┌─────────────────╮
| Viewer side     |
└─────────────────╯*/
//region Viewer side

/// A warm viewer's end: the lock that makes it the one listener and the
/// socket.  Dropping it removes the socket and lets the lock go.
pub struct ViewerListener {
    listener: UnixListener,
    socket: PathBuf,
    _lock: File,
    exe: Option<ExeId>,
}

impl ViewerListener {
    /// Takes the warm-viewer lock and binds the socket, or fails with
    /// [`ServeError::AlreadyRunning`] when another viewer listens.
    pub fn bind() -> Result<ViewerListener, ServeError> {
        let socket = viewer_socket_path().ok_or(ServeError::NoRuntimeDir)?;
        ViewerListener::bind_at(&socket)
    }

    /// [`ViewerListener::bind`] at a given socket path, for tests.
    pub fn bind_at(socket: &Path) -> Result<ViewerListener, ServeError> {
        let dir = socket.parent().filter(|d| !d.as_os_str().is_empty()).unwrap_or(Path::new("."));
        private_dir(dir)?;
        let name = socket.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(dir.join(format!("{name}.lock")))?;
        match lock.try_lock() {
            Ok(()) => {}
            Err(TryLockError::WouldBlock) => return Err(ServeError::AlreadyRunning),
            Err(TryLockError::Error(err)) => return Err(err.into()),
        }
        // Whoever holds the lock owns the path, so a socket left by a
        // viewer that died can go.
        match std::fs::remove_file(socket) {
            Ok(()) => {}
            Err(err) if err.kind() == ErrorKind::NotFound => {}
            Err(err) => return Err(err.into()),
        }
        let listener = UnixListener::bind(socket)?;
        listener.set_nonblocking(true)?;
        Ok(ViewerListener { listener, socket: socket.to_path_buf(), _lock: lock, exe: ExeId::current() })
    }

    /// Waits up to `timeout` for a request and takes it, answering the
    /// client.  `None` when the time ran out, or the executable was
    /// replaced on disk (a reinstall), which the caller should take as the
    /// end: an old build mustn't serve the new one's commands.
    pub fn accept(&self, timeout: Duration) -> Option<ViewRequest> {
        let deadline = Instant::now() + timeout;
        loop {
            if self.exe.as_ref().is_some_and(|e| e.replaced()) {
                return None;
            }
            let left = deadline.checked_duration_since(Instant::now())?;
            // Wake at least once a second to notice a reinstall.
            let wait = left.min(Duration::from_secs(1));
            if !sys::wait_readable([self.listener.as_raw_fd()], Some(wait)).ok()?[0] {
                continue;
            }
            let stream = match self.listener.accept() {
                Ok((s, _)) => s,
                Err(e) if e.kind() == ErrorKind::WouldBlock => continue,
                Err(_) => continue,
            };
            if let Some(req) = serve(&stream) {
                return Some(req);
            }
        }
    }

    /// Whether this program's executable was replaced on disk since the
    /// listener was bound (a reinstall), after which it should stop
    /// listening and exit, as [`ViewerListener::accept`] does.
    pub fn executable_replaced(&self) -> bool {
        self.exe.as_ref().is_some_and(|e| e.replaced())
    }

    /// Calls `wake` from a thread of its own whenever a client is waiting to
    /// be taken, until the returned [`ClientWatch`] is dropped: for a caller
    /// that waits in an event loop which can't watch the socket itself
    /// (macOS's run loop), and then calls [`ViewerListener::accept`].
    pub fn watch(&self, wake: impl Fn() + Send + 'static) -> io::Result<ClientWatch> {
        let listener = self.listener.try_clone()?;
        let (stop, stopped) = UnixStream::pair()?;
        let thread = std::thread::Builder::new().name("slerp-viewer-watch".into()).spawn(move || {
            let fds = [listener.as_raw_fd(), stopped.as_raw_fd()];
            loop {
                match sys::wait_readable(fds, None) {
                    Ok([_, true]) | Err(_) => return,
                    Ok([true, false]) => {
                        wake();
                        // The owner takes the client at once; until it has,
                        // the socket stays readable, so wait a moment (or for
                        // the stop) before looking again.
                        let pause = sys::wait_readable([stopped.as_raw_fd()], Some(Duration::from_millis(20)));
                        if !matches!(pause, Ok([false])) {
                            return;
                        }
                    }
                    Ok([false, false]) => {}
                }
            }
        })?;
        Ok(ClientWatch { stop, thread: Some(thread) })
    }

    pub fn socket(&self) -> &Path {
        &self.socket
    }
}

impl Drop for ViewerListener {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.socket);
    }
}

/// The thread [`ViewerListener::watch`] started; dropping this stops it at
/// once and waits for it.
pub struct ClientWatch {
    stop: UnixStream,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Drop for ClientWatch {
    fn drop(&mut self) {
        // The thread sees the end of the stream; unlike a write, this can't
        // raise SIGPIPE when the thread has already gone.
        let _ = self.stop.shutdown(std::net::Shutdown::Both);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// Reads one request from a connection and answers it: taken, or turned
/// down when malformed or from another version.
fn serve(stream: &UnixStream) -> Option<ViewRequest> {
    sys::prepare_accepted(stream).ok()?;
    if sys::peer_uid(stream).ok()? != sys::euid() {
        return None;
    }
    let deadline = Instant::now() + Duration::from_millis(500);
    let mut head = [0u8; 9];
    sys::read_exact_by(stream, &mut head, deadline).ok()?;
    let answer = |status: u8| {
        let mut out = [0u8; 6];
        out[..4].copy_from_slice(MAGIC);
        out[4] = VIEWER_PROTOCOL_VERSION;
        out[5] = status;
        sys::send_all(stream, &out, deadline)
    };
    if &head[..4] != MAGIC || head[4] != VIEWER_PROTOCOL_VERSION {
        let _ = answer(1);
        return None;
    }
    let len = u32::from_le_bytes(head[5..9].try_into().ok()?);
    if len > MAX_REQUEST {
        let _ = answer(1);
        return None;
    }
    let payload = sys::read_vec_by(stream, len as usize, deadline).ok()?;
    let Some(req) = decode(&payload) else {
        let _ = answer(1);
        return None;
    };
    // Taken only once the client knows, so it never starts a second
    // window for the same command.
    answer(0).ok()?;
    Some(req)
}

//endregion

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_socket(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("slerp-handoff-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir.join("viewer")
    }

    #[test]
    fn requests_round_trip() {
        let req = ViewRequest {
            file: Some("/home/me/a model.glb".into()),
            class: Some("agent-test".into()),
            env: vec![("SLERP_TIMING".into(), "1".into()), ("SLERP_INPUT".into(), "Down Down=x".into())],
        };
        assert_eq!(decode(&encode(&req)), Some(req));
        assert_eq!(decode(&encode(&ViewRequest::default())), Some(ViewRequest::default()));
        // Relative paths and damaged fields are refused.
        let relative = ViewRequest { file: Some("a.glb".into()), ..Default::default() };
        assert_eq!(decode(&encode(&relative)), None);
        let mut cut = encode(&ViewRequest { file: Some("/a.glb".into()), ..Default::default() });
        cut.pop();
        assert_eq!(decode(&cut), None);
    }

    #[test]
    fn a_listening_viewer_takes_one_request() {
        let socket = temp_socket("take");
        // 1. Nobody listens: the client fails at once.
        let start = Instant::now();
        let req = ViewRequest { file: Some("/tmp/x.glb".into()), ..Default::default() };
        assert!(matches!(hand_to_viewer_at(&socket, &req, Duration::from_secs(2)), Err(HandoffError::NotRunning)));
        assert!(start.elapsed() < Duration::from_millis(100));

        // 2. One listener at a time.
        let listener = ViewerListener::bind_at(&socket).unwrap();
        assert!(matches!(ViewerListener::bind_at(&socket), Err(ServeError::AlreadyRunning)));
        let server = std::thread::spawn(move || {
            let got = listener.accept(Duration::from_secs(5));
            (got, listener)
        });
        hand_to_viewer_at(&socket, &req, Duration::from_secs(2)).unwrap();
        let (got, listener) = server.join().unwrap();
        assert_eq!(got, Some(req.clone()));

        // 3. Nothing comes: the wait ends.
        assert_eq!(listener.accept(Duration::from_millis(50)), None);

        // 4. Dropping it removes the socket, and the next client is told
        //    at once.
        drop(listener);
        assert!(!socket.exists());
        assert!(matches!(hand_to_viewer_at(&socket, &req, Duration::from_secs(2)), Err(HandoffError::NotRunning)));
        let _ = std::fs::remove_dir_all(socket.parent().unwrap());
    }

    /// A watch wakes its owner when a client connects, the owner then takes
    /// the request, and dropping the watch ends its thread at once.
    #[test]
    fn a_watch_wakes_the_owner_for_each_client() {
        let socket = temp_socket("watch");
        let listener = ViewerListener::bind_at(&socket).unwrap();
        let (woke, wakes) = std::sync::mpsc::channel();
        let watch = listener.watch(move || woke.send(()).unwrap()).unwrap();
        assert!(wakes.recv_timeout(Duration::from_millis(100)).is_err(), "no client, no wake");
        let path = socket.clone();
        let req = ViewRequest { file: Some("/tmp/w.glb".into()), ..Default::default() };
        let sent = req.clone();
        let client = std::thread::spawn(move || hand_to_viewer_at(&path, &sent, Duration::from_secs(2)).is_ok());
        wakes.recv_timeout(Duration::from_secs(2)).expect("woken for the client");
        assert_eq!(listener.accept(Duration::from_millis(1)), Some(req));
        assert!(client.join().unwrap());
        let start = Instant::now();
        drop(watch);
        assert!(start.elapsed() < Duration::from_millis(100));
        drop(listener);
        let _ = std::fs::remove_dir_all(socket.parent().unwrap());
    }

    #[test]
    fn garbage_is_turned_down() {
        use std::io::{Read, Write};
        let socket = temp_socket("garbage");
        let listener = ViewerListener::bind_at(&socket).unwrap();
        let path = socket.clone();
        let client = std::thread::spawn(move || {
            let mut s = UnixStream::connect(&path).unwrap();
            // A listener that never accepts fails the test instead of
            // hanging it.
            s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            s.write_all(b"SLVW\x63\0\0\0\0").unwrap();
            let mut reply = Vec::new();
            let _ = s.read_to_end(&mut reply);
            reply
        });
        assert_eq!(listener.accept(Duration::from_millis(500)), None);
        let reply = client.join().unwrap();
        assert_eq!(reply.get(5), Some(&1), "another version is refused");
        drop(listener);
        let _ = std::fs::remove_dir_all(socket.parent().unwrap());
    }
}
