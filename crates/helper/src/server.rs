//! The helper side: the lock, the socket, the connection threads, the worker
//! pool that runs the handler, and the watchdog.
//!
//! Threads, all scoped to [`Server::run`]:
//!
//! - The calling thread accepts connections and decides when to exit.  It
//!   sleeps in `poll` until a connection arrives, a connection thread wakes
//!   it, or the idle time runs out, so an idle helper costs no wake-ups.
//! - One short-lived thread per connection reads the request, queues it,
//!   waits for the result until the deadline and writes the reply.  Slow or
//!   silent clients only ever hold up their own thread.
//! - `threads` workers run the handler, one job each at a time.
//! - The watchdog sleeps until the earliest running job's deadline plus the
//!   hang margin, and ends the process if a job is still running then.

use crate::protocol::{self, HEADER_LEN, kind};
use crate::sys;
use crate::types::{Exit, ServeError, ServeOptions};
use crate::{HANG_EXIT_CODE, PROTOCOL_VERSION, Reply, Request, log, socket_path};
use std::collections::VecDeque;
use std::fs::{self, DirBuilder, File, OpenOptions, TryLockError};
use std::io::{self, ErrorKind, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Condvar, Mutex, MutexGuard, PoisonError};
use std::thread::{self, Scope, ScopedJoinHandle};
use std::time::{Duration, Instant};

/// How long a client has to send its request after connecting.  Real
/// clients send it at once; this only bounds broken ones.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(1);
/// How long past the deadline a reply may take to send.  The client stops
/// reading at its deadline, so this only bounds a client that went away.
const SEND_GRACE: Duration = Duration::from_secs(1);
/// How often, at most, the helper checks whether its executable changed.
const EXE_CHECK_INTERVAL: Duration = Duration::from_millis(250);
/// Connection threads only parse requests and copy replies.
const CONNECTION_STACK: usize = 128 << 10;
/// Workers run the handler: file parsers and the renderer.
const WORKER_STACK: usize = 8 << 20;

/// Binds the socket and serves requests with `handler` until it's time to
/// exit.  The same as [`Server::bind`] followed by [`Server::run`].
pub fn serve<H>(handler: H, opts: ServeOptions) -> Result<Exit, ServeError>
where
    H: Fn(&Request) -> Reply + Sync,
{
    Server::bind(opts)?.run(handler)
}

/// A helper that holds this user's lock and listens on the socket, but
/// hasn't started serving.
///
/// Splitting [`bind`](Server::bind) from [`run`](Server::run) lets the
/// helper start its GPU only after winning the lock: when a folder of
/// models opens, several thumbnailers may start helpers at once, and all but
/// one should exit before doing anything expensive.  Clients that connect
/// between the two wait in the socket's queue until `run` starts.
pub struct Server {
    listener: UnixListener,
    socket: PathBuf,
    /// An exclusive `flock` on `lock` next to the socket, held for the
    /// helper's life and released by the kernel when the process dies.
    _lock: File,
    exe: Option<ExeId>,
    opts: ServeOptions,
}

impl Server {
    /// Takes this user's helper lock and binds the socket.
    ///
    /// The lock, rather than the bind, is what makes one helper win a start
    /// race.  Binding alone can't replace a stale socket safely: two new
    /// helpers could both see the dead socket, and the second to unlink and
    /// bind would orphan the first.  Whoever holds the lock owns the socket
    /// path, so it may delete a stale socket left by a helper that crashed,
    /// and the kernel releases the lock however the process ends.
    pub fn bind(opts: ServeOptions) -> Result<Server, ServeError> {
        // 1. A private directory.
        let socket = opts.socket.clone().or_else(socket_path).ok_or(ServeError::NoRuntimeDir)?;
        let dir = socket.parent().filter(|d| !d.as_os_str().is_empty()).unwrap_or(Path::new("."));
        private_dir(dir)?;

        // 2. The lock.
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(dir.join("lock"))?;
        match lock.try_lock() {
            Ok(()) => {}
            Err(TryLockError::WouldBlock) => return Err(ServeError::AlreadyRunning),
            Err(TryLockError::Error(err)) => return Err(err.into()),
        }

        // 3. The socket, replacing a stale one.
        match fs::remove_file(&socket) {
            Ok(()) => log!("removed a stale socket at {}", socket.display()),
            Err(err) if err.kind() == ErrorKind::NotFound => {}
            Err(err) => return Err(err.into()),
        }
        let listener = UnixListener::bind(&socket)?;
        listener.set_nonblocking(true)?;

        let exe = if opts.exit_when_replaced { ExeId::current() } else { None };
        log!("listening on {} with {} workers", socket.display(), opts.threads.max(1));
        Ok(Server { listener, socket, _lock: lock, exe, opts })
    }

    /// The socket this helper listens on.
    pub fn socket(&self) -> &Path {
        &self.socket
    }

    /// Serves requests with `handler` until it's time to exit, and says why.
    ///
    /// `handler` runs on [`ServeOptions::threads`] workers at once.  A panic
    /// in it becomes a [`Reply::Failed`] and the helper carries on.  On exit
    /// the socket is removed first, so new clients start a new helper, and
    /// connections already made are still served.  A job stuck far past its
    /// deadline ends the process instead of returning (see
    /// [`ServeOptions::hang_margin`]).
    pub fn run<H>(self, handler: H) -> Result<Exit, ServeError>
    where
        H: Fn(&Request) -> Reply + Sync,
    {
        let threads = self.opts.threads.max(1);
        let shared = Shared::new(threads)?;
        let exit = thread::scope(|scope| -> io::Result<Exit> {
            // 1. The watchdog, then the workers.
            let watchdog = thread::Builder::new()
                .name("watchdog".into())
                .spawn_scoped(scope, || watchdog(&shared, &self.socket, self.opts.hang_margin))?;
            let mut pool = Pool { shared: &shared, workers: Vec::new(), watchdog: Some(watchdog) };
            for i in 0..threads {
                let (shared, handler) = (&shared, &handler);
                let worker = thread::Builder::new()
                    .name(format!("worker {i}"))
                    .stack_size(WORKER_STACK)
                    .spawn_scoped(scope, move || worker(shared, handler, i))?;
                pool.workers.push(worker);
            }

            // 2. Accept until it's time to go.
            let exit = self.accept_loop(scope, &shared)?;
            log!("exiting: {exit:?}");

            // 3. Remove the socket so no new client connects, then serve the
            // connections already queued and wait for them all.  Dropping
            // the pool then lets the workers finish and stops the watchdog.
            if let Err(err) = fs::remove_file(&self.socket) {
                log!("couldn't remove {}: {err}", self.socket.display());
            }
            loop {
                match self.listener.accept() {
                    Ok((stream, _)) => self.admit(scope, &shared, stream),
                    Err(err) if err.kind() == ErrorKind::Interrupted => {}
                    Err(_) => break,
                }
            }
            let mut conns = lock(&shared.conns);
            while conns.open > 0 {
                conns = shared.conns_cv.wait(conns).unwrap_or_else(PoisonError::into_inner);
            }
            Ok(exit)
        })?;
        Ok(exit)
    }

    fn accept_loop<'scope>(&self, scope: &'scope Scope<'scope, '_>, shared: &'scope Shared) -> io::Result<Exit> {
        let mut exe_checked = Instant::now();
        loop {
            // 1. A connection thread asked to stop.
            if let Some(exit) = *lock(&shared.exit) {
                return Ok(exit);
            }

            // 2. Sleep until a connection or a wake-up, or until idle.
            let timeout = {
                let conns = lock(&shared.conns);
                let quiet = conns.last_activity.elapsed();
                match conns.open {
                    0 if quiet >= self.opts.idle => return Ok(Exit::Idle),
                    0 => Some(self.opts.idle - quiet),
                    _ => None,
                }
            };
            let [incoming, woken] =
                sys::wait_readable([self.listener.as_raw_fd(), shared.wake_rx.as_raw_fd()], timeout)?;
            if woken {
                let mut sink = [0; 64];
                while matches!((&shared.wake_rx).read(&mut sink), Ok(n) if n > 0) {}
            }
            if !incoming {
                continue;
            }

            // 3. Take every waiting connection.
            loop {
                match self.listener.accept() {
                    Ok((stream, _)) => self.admit(scope, shared, stream),
                    Err(err) if err.kind() == ErrorKind::WouldBlock => break,
                    Err(err) if err.kind() == ErrorKind::Interrupted => {}
                    // Out of descriptors and the like: keep serving what we have.
                    Err(err) => {
                        log!("accept failed: {err}");
                        break;
                    }
                }
            }

            // 4. A reinstall replaces the executable; the connections just
            // taken are still served by this build.
            if exe_checked.elapsed() >= EXE_CHECK_INTERVAL {
                exe_checked = Instant::now();
                if self.exe.as_ref().is_some_and(ExeId::replaced) {
                    return Ok(Exit::Replaced);
                }
            }
        }
    }

    /// Checks a new connection's uid and gives it a thread.
    fn admit<'scope>(&self, scope: &'scope Scope<'scope, '_>, shared: &'scope Shared, stream: UnixStream) {
        if let Err(err) = sys::prepare_accepted(&stream) {
            return log!("refused a connection: {err}");
        }
        // 1. Only this user.  The directory's mode already keeps others out;
        // this guards against a directory someone loosened.
        match sys::peer_uid(&stream) {
            Ok(uid) if peer_allowed(uid, sys::euid()) => {}
            Ok(uid) => return log!("refused a connection from uid {uid}"),
            Err(err) => return log!("refused a connection: no peer credentials: {err}"),
        }

        // 2. Within the connection limit.
        {
            let mut conns = lock(&shared.conns);
            conns.last_activity = Instant::now();
            if conns.open >= self.opts.max_connections {
                drop(conns);
                log!("busy: refused a connection");
                let busy = protocol::header(kind::BUSY, 0);
                let _ = sys::send_all(&stream, &busy, Instant::now() + Duration::from_millis(10));
                return;
            }
            conns.open += 1;
        }

        // 3. A thread of its own.
        let spawned = thread::Builder::new().name("connection".into()).stack_size(CONNECTION_STACK).spawn_scoped(
            scope,
            move || {
                let _done = ConnectionDone(shared);
                serve_connection(shared, &stream);
            },
        );
        if let Err(err) = spawned {
            log!("couldn't start a connection thread: {err}");
            ConnectionDone(shared).finish();
        }
    }
}

/// Whether a client with uid `peer` may use the helper of uid `me`: only
/// the same user, root included, since requests name the user's files and
/// replies show their contents.
fn peer_allowed(peer: u32, me: u32) -> bool {
    peer == me
}

/// Creates `dir` with mode 0700 if missing, and checks that it's a real
/// directory of this user that nobody else can write to.
pub(crate) fn private_dir(dir: &Path) -> Result<(), ServeError> {
    match DirBuilder::new().mode(0o700).create(dir) {
        Ok(()) => {}
        Err(err) if err.kind() == ErrorKind::AlreadyExists => {}
        Err(err) => return Err(err.into()),
    }
    let meta = fs::symlink_metadata(dir)?;
    if !meta.is_dir() || meta.uid() != sys::euid() {
        return Err(ServeError::UnsafeDirectory(dir.to_path_buf()));
    }
    if meta.mode() & 0o077 != 0 {
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/*┌─────────────────╮
| Shared state    |
└─────────────────╯*/
//region Shared state

struct Shared {
    queue: Mutex<Queue>,
    queue_cv: Condvar,
    /// The deadline of each worker's running job, for the watchdog.
    watch: Mutex<Watch>,
    watch_cv: Condvar,
    conns: Mutex<Conns>,
    conns_cv: Condvar,
    /// Set by a connection thread to make the accept loop exit.
    exit: Mutex<Option<Exit>>,
    /// A socket pair whose read end the accept loop polls, so connection
    /// threads can wake it without it polling on a timer.
    wake_tx: UnixStream,
    wake_rx: UnixStream,
}

struct Queue {
    jobs: VecDeque<Job>,
    closed: bool,
}

struct Job {
    request: Request,
    reply: mpsc::SyncSender<Reply>,
}

struct Watch {
    running: Vec<Option<Instant>>,
    stop: bool,
}

struct Conns {
    open: usize,
    last_activity: Instant,
}

impl Shared {
    fn new(threads: usize) -> io::Result<Self> {
        let (wake_tx, wake_rx) = UnixStream::pair()?;
        wake_tx.set_nonblocking(true)?;
        wake_rx.set_nonblocking(true)?;
        Ok(Shared {
            queue: Mutex::new(Queue { jobs: VecDeque::new(), closed: false }),
            queue_cv: Condvar::new(),
            watch: Mutex::new(Watch { running: vec![None; threads], stop: false }),
            watch_cv: Condvar::new(),
            conns: Mutex::new(Conns { open: 0, last_activity: Instant::now() }),
            conns_cv: Condvar::new(),
            exit: Mutex::new(None),
            wake_tx,
            wake_rx,
        })
    }

    fn wake(&self) {
        // A full pipe already means a wake-up is pending.
        let _ = (&self.wake_tx).write(&[1]);
    }

    fn request_exit(&self, exit: Exit) {
        lock(&self.exit).get_or_insert(exit);
        self.wake();
    }
}

/// Locks a mutex, carrying on after a panic elsewhere: every critical
/// section here leaves its state consistent.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Closes the queue, joins the workers and stops the watchdog, in that
/// order, however `run` leaves its scope.  The watchdog keeps running while
/// the workers finish, so a stuck job still ends the process.
struct Pool<'scope> {
    shared: &'scope Shared,
    workers: Vec<ScopedJoinHandle<'scope, ()>>,
    watchdog: Option<ScopedJoinHandle<'scope, ()>>,
}

impl Drop for Pool<'_> {
    fn drop(&mut self) {
        lock(&self.shared.queue).closed = true;
        self.shared.queue_cv.notify_all();
        for worker in self.workers.drain(..) {
            let _ = worker.join();
        }
        lock(&self.shared.watch).stop = true;
        self.shared.watch_cv.notify_all();
        if let Some(watchdog) = self.watchdog.take() {
            let _ = watchdog.join();
        }
    }
}

/// Marks a connection finished when its thread ends, however it ends.
struct ConnectionDone<'a>(&'a Shared);

impl ConnectionDone<'_> {
    fn finish(self) {}
}

impl Drop for ConnectionDone<'_> {
    fn drop(&mut self) {
        let mut conns = lock(&self.0.conns);
        conns.open -= 1;
        conns.last_activity = Instant::now();
        let last = conns.open == 0;
        drop(conns);
        if last {
            // The accept loop sleeps without a timeout while connections are
            // open; it now needs to start the idle countdown.
            self.0.conns_cv.notify_all();
            self.0.wake();
        }
    }
}

//endregion

/*┌─────────────────╮
| Connections     |
└─────────────────╯*/
//region Connections

/// Reads one request, has it handled and writes the reply.
fn serve_connection(shared: &Shared, stream: &UnixStream) {
    // 1. The header.
    let read_by = Instant::now() + REQUEST_TIMEOUT;
    let mut bytes = [0; HEADER_LEN];
    if let Err(err) = sys::read_exact_by(stream, &mut bytes, read_by) {
        return log!("dropped a connection before its request: {err}");
    }
    let Some(header) = protocol::parse_header(&bytes) else {
        return log!("dropped a connection that sent garbage");
    };

    // 2. Another protocol version gets ours back, and nothing more.  A newer
    // client means a newer build is installed, so this helper steps down.
    if header.version != PROTOCOL_VERSION {
        let _ = sys::send_all(stream, &protocol::header(kind::VERSION, 0), Instant::now() + SEND_GRACE);
        if header.version > PROTOCOL_VERSION {
            log!("a client speaks protocol version {}; stepping down", header.version);
            shared.request_exit(Exit::Superseded);
        } else {
            log!("refused a client with old protocol version {}", header.version);
        }
        return;
    }

    // 3. The request.
    let request = match header.kind {
        kind::STOP if header.len == 0 => {
            log!("asked to stop");
            shared.request_exit(Exit::Stopped);
            let _ = sys::send_all(stream, &protocol::header(kind::OK, 0), Instant::now() + SEND_GRACE);
            return;
        }
        kind::THUMBNAIL if header.len <= protocol::MAX_REQUEST => {
            let mut payload = vec![0; header.len as usize];
            if let Err(err) = sys::read_exact_by(stream, &mut payload, read_by) {
                return log!("dropped a connection mid-request: {err}");
            }
            match protocol::decode_request(&payload, Instant::now()) {
                Ok(request) => request,
                Err(why) => {
                    log!("refused a request: {why}");
                    return send_reply(stream, &Reply::Failed(format!("invalid request: {why}")), read_by);
                }
            }
        }
        _ => return log!("dropped a connection: message kind {:#x}, length {}", header.kind, header.len),
    };

    // 4. Queue it and wait until its deadline.
    let deadline = request.deadline;
    let started = Instant::now();
    let path = request.path.clone();
    let (tx, rx) = mpsc::sync_channel(1);
    lock(&shared.queue).jobs.push_back(Job { request, reply: tx });
    shared.queue_cv.notify_one();
    let left = deadline.saturating_duration_since(Instant::now());
    match rx.recv_timeout(left) {
        Ok(reply) => {
            log!("{} in {:.1} ms: {}", path.display(), started.elapsed().as_secs_f64() * 1e3, describe(&reply));
            send_reply(stream, &reply, deadline.max(Instant::now() + SEND_GRACE));
        }
        Err(RecvTimeoutError::Timeout | RecvTimeoutError::Disconnected) => {
            log!("{}: abandoned at its deadline", path.display());
            let _ = sys::send_all(stream, &protocol::header(kind::EXPIRED, 0), Instant::now() + SEND_GRACE);
        }
    }
}

fn send_reply(stream: &UnixStream, reply: &Reply, deadline: Instant) {
    let encoded = protocol::encode_reply(reply);
    let sent = std::iter::once(encoded.head.as_slice())
        .chain(encoded.body.iter().copied())
        .try_for_each(|part| sys::send_all(stream, part, deadline));
    if let Err(err) = sent {
        // Usually the client gave up at its deadline or was killed.
        log!("couldn't send a reply: {err}");
    }
}

fn describe(reply: &Reply) -> String {
    match reply {
        Reply::Png(png) => format!("PNG of {} bytes", png.len()),
        Reply::Frames { width, height, rgba } => format!("{} frames of {width}x{height}", rgba.len()),
        Reply::NoThumbnail => "no thumbnail".into(),
        Reply::Failed(why) => format!("failed: {why}"),
    }
}

//endregion

/*┌─────────────────╮
| Workers         |
└─────────────────╯*/
//region Workers

fn worker<H>(shared: &Shared, handler: &H, index: usize)
where
    H: Fn(&Request) -> Reply + Sync,
{
    loop {
        // 1. The next job, or the end.
        let job = {
            let mut queue = lock(&shared.queue);
            loop {
                if let Some(job) = queue.jobs.pop_front() {
                    break job;
                }
                if queue.closed {
                    return;
                }
                queue = shared.queue_cv.wait(queue).unwrap_or_else(PoisonError::into_inner);
            }
        };

        // 2. Skip jobs whose client has stopped waiting.
        let deadline = job.request.deadline;
        if Instant::now() >= deadline {
            log!("{}: skipped, its deadline passed in the queue", job.request.path.display());
            continue;
        }

        // 3. Run it under the watchdog's eye.
        lock(&shared.watch).running[index] = Some(deadline);
        shared.watch_cv.notify_all();
        let reply = catch_unwind(AssertUnwindSafe(|| handler(&job.request))).unwrap_or_else(|panic| {
            let why = panic
                .downcast_ref::<&str>()
                .map(|s| s.to_string())
                .or_else(|| panic.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "unknown".into());
            log!("{}: the handler panicked: {why}", job.request.path.display());
            Reply::Failed(format!("panic: {why}"))
        });
        lock(&shared.watch).running[index] = None;

        // 4. The connection thread may have given up; then nobody listens.
        let _ = job.reply.send(reply);
    }
}

/// Ends the process if a job runs `margin` past its deadline.  It sleeps
/// until the earliest such moment, or indefinitely while nothing runs.
fn watchdog(shared: &Shared, socket: &Path, margin: Duration) {
    let mut watch = lock(&shared.watch);
    loop {
        if watch.stop {
            return;
        }
        let Some(limit) = watch.running.iter().flatten().min().map(|&deadline| deadline + margin) else {
            watch = shared.watch_cv.wait(watch).unwrap_or_else(PoisonError::into_inner);
            continue;
        };
        let now = Instant::now();
        if now >= limit {
            log!(
                "a job is {:.1} s past its deadline; assuming a hang and exiting",
                (now - limit + margin).as_secs_f64()
            );
            // New clients should start a fresh helper rather than queue here.
            let _ = fs::remove_file(socket);
            sys::exit_now(HANG_EXIT_CODE);
        }
        watch = shared.watch_cv.wait_timeout(watch, limit - now).unwrap_or_else(PoisonError::into_inner).0;
    }
}

//endregion

/// The identity of the running executable, to notice a reinstall.
#[derive(Debug)]
pub(crate) struct ExeId {
    path: PathBuf,
    dev: u64,
    ino: u64,
}

impl ExeId {
    #[cfg(target_os = "linux")]
    pub(crate) fn current() -> Option<ExeId> {
        // /proc/self/exe opens the running image even after its file has
        // been replaced; the path names whatever is installed now.
        let running = fs::metadata("/proc/self/exe").ok()?;
        let path = std::env::current_exe().ok()?;
        Some(ExeId { path, dev: running.dev(), ino: running.ino() })
    }

    /// Without `/proc`, the file at the executable's path when the helper
    /// starts stands for the running image; a replacement in the moment
    /// between starting and this call would go unnoticed.
    #[cfg(not(target_os = "linux"))]
    pub(crate) fn current() -> Option<ExeId> {
        let path = std::env::current_exe().ok()?;
        let running = fs::metadata(&path).ok()?;
        Some(ExeId { path, dev: running.dev(), ino: running.ino() })
    }

    /// Whether the file at the executable's path is no longer the running
    /// image: replaced by a reinstall or rebuild, or removed.
    pub(crate) fn replaced(&self) -> bool {
        match fs::metadata(&self.path) {
            Ok(meta) => meta.dev() != self.dev || meta.ino() != self.ino,
            Err(_) => true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_same_uid_is_allowed() {
        assert!(peer_allowed(1000, 1000));
        assert!(peer_allowed(0, 0));
        assert!(!peer_allowed(0, 1000));
        assert!(!peer_allowed(1001, 1000));
        assert!(!peer_allowed(100_000, 1000));
    }

    #[test]
    fn the_running_executable_is_not_replaced() {
        let exe = ExeId::current().unwrap();
        assert!(!exe.replaced());
        let gone = ExeId { path: "/nonexistent/slerp".into(), ..exe };
        assert!(gone.replaced());
    }
}
