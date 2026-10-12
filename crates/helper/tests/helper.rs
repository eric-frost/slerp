//! Integration tests: a real helper process (`src/bin/fake_helper.rs`,
//! whose handler sleeps, panics or hangs on request) and clients in this
//! process, in `src/bin/probe.rs`, under bubblewrap and as another uid.
//! The helper speaks over a Unix socket; on Windows, Explorer's handler
//! uses a named pipe instead (`slerp-winshell`).
#![cfg(unix)]

use slerp_helper::{
    ClientError, Exit, HANG_EXIT_CODE, Reply, Request, ServeError, ServeOptions, Server, request_at, spawn_detached,
    stop_at,
};
use std::collections::HashSet;
use std::ffi::OsStr;
use std::fs;
use std::io::{self, Read, Write};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::Barrier;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::{Duration, Instant};

/*┌─────────────────╮
| Fixture         |
└─────────────────╯*/
//region Fixture

/// The stand-in helper, one of the crate's test binaries.  Cargo builds a
/// package's binaries before its integration tests and passes their paths
/// in `CARGO_BIN_EXE_*`, which stays right when `build.build-dir` keeps the
/// test binary itself away from the target directory.
const FAKE_HELPER: &str = env!("CARGO_BIN_EXE_fake_helper");

/// The one-shot client, the crate's other test binary.
const PROBE: &str = env!("CARGO_BIN_EXE_probe");

/// A scratch directory with a socket path inside, and the helpers started
/// on it, which are killed or stopped when the test ends.
struct Fixture {
    root: PathBuf,
    socket: PathBuf,
    helpers: Vec<Child>,
}

impl Fixture {
    fn new(name: &str) -> Fixture {
        // macOS's $TMPDIR is long, and its socket paths end at 104 bytes.
        let temp = if cfg!(target_os = "macos") { PathBuf::from("/tmp") } else { std::env::temp_dir() };
        let root = temp.join("slerp-helper-tests").join(format!("{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let socket = root.join("slerp").join("socket");
        Fixture { root, socket, helpers: Vec::new() }
    }

    /// The fake helper's command for this fixture, logging to `log`.
    fn command(&self, exe: &Path, args: &[&str]) -> Command {
        let mut cmd = Command::new(exe);
        cmd.arg("--socket").arg(&self.socket).arg("--record").arg(&self.root).args(args);
        cmd.env("SLERP_LOG", self.root.join("log"));
        cmd
    }

    /// Starts a helper as a child of the test and waits until it listens.
    fn start(&mut self, args: &[&str]) {
        self.start_exe(Path::new(FAKE_HELPER), args);
    }

    fn start_exe(&mut self, exe: &Path, args: &[&str]) {
        let log = fs::OpenOptions::new().create(true).append(true).open(self.root.join("log")).unwrap();
        let mut cmd = self.command(exe, args);
        cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(log);
        // A freshly copied executable gives ETXTBSY while another test
        // thread's fork still holds the copy's write descriptor.
        let mut tries = 0;
        let child = loop {
            match cmd.spawn() {
                Err(err) if err.kind() == io::ErrorKind::ExecutableFileBusy && tries < 100 => tries += 1,
                result => break result.unwrap(),
            }
            thread::sleep(Duration::from_millis(5));
        };
        self.helpers.push(child);
        self.wait_ready();
    }

    fn wait_ready(&self) {
        let until = Instant::now() + Duration::from_secs(10);
        while UnixStream::connect(&self.socket).is_err() {
            assert!(Instant::now() < until, "no helper started; log:\n{}", self.log());
            thread::sleep(Duration::from_millis(2));
        }
    }

    /// Waits for the last helper started with `start` to exit.
    fn wait_exit(&mut self, timeout: Duration) -> ExitStatus {
        let until = Instant::now() + timeout;
        loop {
            if let Some(status) = self.helpers.last_mut().unwrap().try_wait().unwrap() {
                return status;
            }
            assert!(Instant::now() < until, "the helper didn't exit; log:\n{}", self.log());
            thread::sleep(Duration::from_millis(2));
        }
    }

    fn log(&self) -> String {
        fs::read_to_string(self.root.join("log")).unwrap_or_default()
    }

    /// The lines the fake helper recorded in `file`.
    fn lines(&self, file: &str) -> Vec<String> {
        fs::read_to_string(self.root.join(file)).unwrap_or_default().lines().map(String::from).collect()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        // Detached helpers aren't our children; ask them to go.
        let _ = stop_at(&self.socket, Duration::from_millis(500));
        for child in &mut self.helpers {
            let _ = child.kill();
            let _ = child.wait();
        }
        if !thread::panicking() {
            let _ = fs::remove_dir_all(&self.root);
        }
    }
}

fn req(name: &str, ms: u64) -> Request {
    Request {
        path: Path::new("/fake").join(name),
        size: 64,
        frames: 0,
        deadline: Instant::now() + Duration::from_millis(ms),
    }
}

fn no_start() -> io::Result<()> {
    panic!("the client shouldn't start a helper here")
}

fn ask(f: &Fixture, name: &str) -> Result<Reply, ClientError> {
    request_at(&f.socket, &req(name, 5000), no_start)
}

fn png(reply: Result<Reply, ClientError>) -> Vec<u8> {
    match reply {
        Ok(Reply::Png(bytes)) => bytes,
        other => panic!("expected a PNG, got {other:?}"),
    }
}

fn text(reply: Result<Reply, ClientError>) -> String {
    String::from_utf8(png(reply)).unwrap()
}

/// A version 1 thumbnail request built by hand, pinning the documented
/// wire format.
fn raw_request(path: &str, version: u8, size: u32) -> Vec<u8> {
    let mut message = vec![b'P', b'T', version, 0x01];
    message.extend((12 + path.len() as u32).to_le_bytes());
    message.extend(size.to_le_bytes());
    message.extend(0u32.to_le_bytes());
    message.extend(5000u32.to_le_bytes());
    message.extend(path.as_bytes());
    message
}

/// Sends `bytes` on a new connection and reads until the helper hangs up.
fn raw(f: &Fixture, bytes: &[u8]) -> (Vec<u8>, Duration) {
    let t = Instant::now();
    let mut stream = UnixStream::connect(&f.socket).unwrap();
    stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let _ = stream.write_all(bytes);
    let mut out = Vec::new();
    let _ = stream.read_to_end(&mut out);
    (out, t.elapsed())
}

const VERSION_REPLY: &[u8] = b"PT\x01\x47\0\0\0\0";

//endregion

/*┌─────────────────╮
| Serving         |
└─────────────────╯*/
//region Serving

#[test]
fn replies_round_trip() {
    let mut f = Fixture::new("round-trip");
    f.start(&[]);
    assert_eq!(png(ask(&f, "png")).len(), 67);
    assert_eq!(ask(&f, "none").unwrap(), Reply::NoThumbnail);
    assert_eq!(ask(&f, "fail").unwrap(), Reply::Failed("this file is broken".into()));

    // Relative paths arrive absolute, with the size and frame count.
    let relative = Request { path: "some dir/echo".into(), size: 300, ..req("x", 5000) };
    let cwd = std::env::current_dir().unwrap();
    assert_eq!(
        text(request_at(&f.socket, &relative, no_start)),
        format!("{} 300 0", cwd.join("some dir/echo").display())
    );

    // The handler sees the client's deadline.
    let left: u64 = text(request_at(&f.socket, &req("left", 2000), no_start)).parse().unwrap();
    assert!((1900..=2000).contains(&left), "{left} ms left");

    // Turntable frames.
    let frames = Request { size: 16, frames: 5, ..req("frames", 5000) };
    match request_at(&f.socket, &frames, no_start) {
        Ok(Reply::Frames { width: 16, height: 16, rgba }) => {
            assert_eq!(rgba.len(), 5);
            for (i, frame) in rgba.iter().enumerate() {
                assert_eq!(frame.len(), 16 * 16 * 4);
                assert!(frame.iter().all(|&b| b == i as u8));
            }
        }
        other => panic!("{other:?}"),
    }
    // A handler's malformed frames become a failure rather than garbage.
    assert!(matches!(ask(&f, "bad-frames"), Ok(Reply::Failed(_))));

    // A reply as large as an uncompressed 2048 px picture.
    assert_eq!(png(ask(&f, "big-16777216")).len(), 16 << 20);
}

#[test]
fn a_panic_fails_only_its_request() {
    let mut f = Fixture::new("panic");
    f.start(&["--threads", "1"]);
    match ask(&f, "panic") {
        Ok(Reply::Failed(why)) => assert!(why.contains("panics on request"), "{why}"),
        other => panic!("{other:?}"),
    }
    // The only worker survived.
    assert_eq!(png(ask(&f, "png")).len(), 67);
}

#[test]
fn many_clients_at_once() {
    let mut f = Fixture::new("parallel");
    f.start(&["--threads", "4"]);
    let t = Instant::now();
    let results: Vec<_> = thread::scope(|s| {
        let clients: Vec<_> = (0..32).map(|_| s.spawn(|| ask(&f, "sleep-100"))).collect();
        clients.into_iter().map(|c| c.join().unwrap()).collect()
    });
    let elapsed = t.elapsed();
    for r in results {
        png(r);
    }
    // 32 jobs of 100 ms on 4 workers take 800 ms.
    assert!(elapsed >= Duration::from_millis(790) && elapsed < Duration::from_secs(3), "{elapsed:?}");

    // As many quick ones as the connection limit allows.
    thread::scope(|s| {
        let clients: Vec<_> = (0..64).map(|_| s.spawn(|| ask(&f, "png"))).collect();
        for c in clients {
            png(c.join().unwrap());
        }
    });
}

#[test]
fn late_jobs_are_abandoned() {
    let mut f = Fixture::new("late");
    f.start(&["--threads", "1"]);

    // A job that outlives its deadline: the client gives up on time.
    let t = Instant::now();
    let r = request_at(&f.socket, &req("sleep-600", 100), no_start);
    let e = t.elapsed();
    assert!(matches!(r, Err(ClientError::TimedOut)), "{r:?}");
    assert!(e >= Duration::from_millis(95) && e < Duration::from_millis(250), "{e:?}");

    // The worker is still busy with it, so a job queued now passes its
    // deadline in the queue and is skipped rather than run.
    let r = request_at(&f.socket, &req("touch-skipped", 100), no_start);
    assert!(matches!(r, Err(ClientError::TimedOut)), "{r:?}");
    thread::sleep(Duration::from_millis(700));
    assert_eq!(png(ask(&f, "png")).len(), 67);
    assert!(!f.root.join("skipped").exists(), "a job whose client had gone was run");
}

#[test]
fn busy_helper_refuses_extra_connections() {
    let mut f = Fixture::new("busy");
    f.start(&["--threads", "1", "--max-connections", "2"]);
    // Let the readiness check's connection close.
    thread::sleep(Duration::from_millis(50));
    thread::scope(|s| {
        let a = s.spawn(|| request_at(&f.socket, &req("sleep-400", 3000), no_start));
        let b = s.spawn(|| request_at(&f.socket, &req("sleep-400", 3000), no_start));
        thread::sleep(Duration::from_millis(150));
        let t = Instant::now();
        let r = ask(&f, "png");
        assert!(matches!(r, Err(ClientError::Busy)), "{r:?}");
        assert!(t.elapsed() < Duration::from_millis(50), "{:?}", t.elapsed());
        png(a.join().unwrap());
        png(b.join().unwrap());
    });
    assert_eq!(png(ask(&f, "png")).len(), 67);
}

#[test]
fn borrowed_handler_in_process() {
    let f = Fixture::new("in-process");
    let opts = || ServeOptions { socket: Some(f.socket.clone()), ..ServeOptions::default() };

    // The lock: one server per socket.
    let first = Server::bind(opts()).unwrap();
    assert!(matches!(Server::bind(opts()), Err(ServeError::AlreadyRunning)));
    // A server that ends without cleaning up leaves its socket; the next
    // one replaces it.
    drop(first);
    assert!(f.socket.exists());
    // Another test thread's fork may hold the lock's descriptor until it
    // executes its program, so give it a moment.
    let until = Instant::now() + Duration::from_secs(2);
    let server = loop {
        match Server::bind(opts()) {
            Err(ServeError::AlreadyRunning) if Instant::now() < until => thread::sleep(Duration::from_millis(2)),
            result => break result.unwrap(),
        }
    };
    assert_eq!(fs::metadata(f.socket.parent().unwrap()).unwrap().mode() & 0o777, 0o700);

    // The handler may borrow from the caller's stack.
    let calls = AtomicUsize::new(0);
    thread::scope(|s| {
        let running = s.spawn(|| {
            server.run(|_req| {
                calls.fetch_add(1, Ordering::SeqCst);
                Reply::NoThumbnail
            })
        });
        assert_eq!(ask(&f, "anything").unwrap(), Reply::NoThumbnail);
        assert!(stop_at(&f.socket, Duration::from_secs(1)).unwrap());
        assert_eq!(running.join().unwrap().unwrap(), Exit::Stopped);
    });
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(!f.socket.exists());
}

//endregion

/*┌─────────────────╮
| Lifecycle       |
└─────────────────╯*/
//region Lifecycle

#[test]
fn racing_starts_leave_one_helper() {
    let f = Fixture::new("race");
    let exe = Path::new(FAKE_HELPER);
    let barrier = Barrier::new(16);
    let starts = AtomicUsize::new(0);
    let results: Vec<_> = thread::scope(|s| {
        let clients: Vec<_> = (0..16)
            .map(|_| {
                s.spawn(|| {
                    barrier.wait();
                    request_at(&f.socket, &req("pid", 3000), || {
                        starts.fetch_add(1, Ordering::SeqCst);
                        spawn_detached(&mut f.command(exe, &["--idle-ms", "20000"]))
                    })
                })
            })
            .collect();
        clients.into_iter().map(|c| c.join().unwrap()).collect()
    });
    for r in &results {
        assert!(matches!(r, Err(ClientError::NotRunning) | Ok(Reply::Png(_))), "{r:?}");
    }
    let starts = starts.load(Ordering::SeqCst);
    assert!(starts >= 1);
    f.wait_ready();

    // Every client now reaches the same helper.
    let pids: HashSet<String> = thread::scope(|s| {
        let clients: Vec<_> = (0..16).map(|_| s.spawn(|| text(ask(&f, "pid")))).collect();
        clients.into_iter().map(|c| c.join().unwrap()).collect()
    });
    assert_eq!(pids.len(), 1, "{pids:?}");

    // Every started helper but one lost the lock and exited.
    let until = Instant::now() + Duration::from_secs(5);
    while f.lines("winners").len() + f.lines("losers").len() < starts || helpers_alive(&f.socket) > 1 {
        assert!(Instant::now() < until, "winners {:?}, losers {:?}", f.lines("winners"), f.lines("losers"));
        thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(f.lines("winners"), pids.into_iter().collect::<Vec<_>>());
    assert_eq!(f.lines("losers").len(), starts - 1);
    assert_eq!(helpers_alive(&f.socket), 1);
    eprintln!("racing starts: {starts} of 16 clients started a helper; 1 won");
}

/// How many live processes have `socket` on their command line.
#[cfg(target_os = "linux")]
fn helpers_alive(socket: &Path) -> usize {
    let needle = socket.as_os_str().as_encoded_bytes();
    fs::read_dir("/proc")
        .unwrap()
        .flatten()
        .filter_map(|entry| fs::read(entry.path().join("cmdline")).ok())
        .filter(|cmdline| cmdline.split(|&b| b == 0).any(|arg| arg == needle))
        .count()
}

/// How many live processes have `socket` on their command line, from `ps`
/// where there is no `/proc`.
#[cfg(not(target_os = "linux"))]
fn helpers_alive(socket: &Path) -> usize {
    let out = Command::new("ps").args(["-axww", "-o", "args="]).output().unwrap();
    let needle = format!(" {} ", socket.display());
    String::from_utf8_lossy(&out.stdout).lines().filter(|line| format!("{line} ").contains(&needle)).count()
}

#[test]
fn killed_helper_mid_request() {
    let mut f = Fixture::new("killed");
    f.start(&[]);
    let socket = f.socket.clone();
    let (result, killed_at, returned_at) = thread::scope(|s| {
        let client = s.spawn(|| {
            let r = request_at(&socket, &req("sleep-5000", 10_000), no_start);
            (r, Instant::now())
        });
        thread::sleep(Duration::from_millis(200));
        let killed_at = Instant::now();
        f.helpers[0].kill().unwrap();
        let (r, returned_at) = client.join().unwrap();
        (r, killed_at, returned_at)
    });
    assert!(matches!(result, Err(ClientError::Disconnected)), "{result:?}");
    let noticed = returned_at - killed_at;
    assert!(noticed < Duration::from_millis(100), "{noticed:?}");
    eprintln!("killed helper: the client noticed in {:.2} ms", noticed.as_secs_f64() * 1e3);
    f.helpers[0].wait().unwrap();

    // The dead helper's socket is still there.  The next client starts a
    // new helper, which replaces it.
    assert!(f.socket.exists());
    let mut started = false;
    let r = request_at(&f.socket, &req("png", 2000), || {
        started = true;
        Ok(())
    });
    assert!(matches!(r, Err(ClientError::NotRunning)) && started, "{r:?}");
    f.start(&[]);
    assert_eq!(png(ask(&f, "png")).len(), 67);
}

#[test]
fn stale_socket_is_replaced() {
    let mut f = Fixture::new("stale");
    fs::create_dir_all(f.socket.parent().unwrap()).unwrap();
    drop(UnixListener::bind(&f.socket).unwrap());
    assert!(f.socket.exists());
    // Another test thread's fork may hold the listener until it executes
    // its program; a dead helper's socket refuses connections at once.
    let until = Instant::now() + Duration::from_secs(2);
    while UnixStream::connect(&f.socket).is_ok() {
        assert!(Instant::now() < until);
        thread::sleep(Duration::from_millis(2));
    }

    let t = Instant::now();
    let mut started = false;
    let r = request_at(&f.socket, &req("png", 2000), || {
        started = true;
        Ok(())
    });
    assert!(matches!(r, Err(ClientError::NotRunning)) && started, "{r:?}");
    assert!(t.elapsed() < Duration::from_millis(50), "{:?}", t.elapsed());
    f.start(&[]);
    assert_eq!(png(ask(&f, "png")).len(), 67);
}

#[test]
fn idle_helper_exits() {
    let mut f = Fixture::new("idle");
    f.start(&["--idle-ms", "300"]);
    png(ask(&f, "png"));
    let t = Instant::now();
    assert!(f.wait_exit(Duration::from_secs(5)).success());
    let waited = t.elapsed();
    assert!(waited >= Duration::from_millis(250) && waited < Duration::from_millis(1500), "{waited:?}");
    assert_eq!(f.lines("exits"), ["Idle"]);
    assert!(!f.socket.exists());
}

#[test]
fn hung_job_ends_the_helper() {
    let mut f = Fixture::new("hang");
    f.start(&["--hang-ms", "300"]);
    let t = Instant::now();
    let r = request_at(&f.socket, &req("hang", 100), no_start);
    assert!(matches!(r, Err(ClientError::TimedOut)), "{r:?}");
    assert_eq!(f.wait_exit(Duration::from_secs(5)).code(), Some(HANG_EXIT_CODE));
    let e = t.elapsed();
    assert!(e >= Duration::from_millis(380) && e < Duration::from_millis(1500), "{e:?}");
    assert!(!f.socket.exists());

    // The next client starts a fresh one.
    let mut started = false;
    let r = request_at(&f.socket, &req("png", 2000), || {
        started = true;
        Ok(())
    });
    assert!(matches!(r, Err(ClientError::NotRunning)) && started, "{r:?}");
}

#[test]
fn stop_request() {
    let mut f = Fixture::new("stop");
    f.start(&[]);
    assert!(stop_at(&f.socket, Duration::from_secs(1)).unwrap());
    assert!(f.wait_exit(Duration::from_secs(3)).success());
    assert_eq!(f.lines("exits"), ["Stopped"]);
    assert!(!stop_at(&f.socket, Duration::from_secs(1)).unwrap());
}

#[test]
fn replaced_executable_ends_the_helper() {
    let mut f = Fixture::new("replaced");
    let bin = f.root.join("bin");
    fs::create_dir(&bin).unwrap();
    let exe = bin.join("fake_helper");
    fs::copy(FAKE_HELPER, &exe).unwrap();
    f.start_exe(&exe, &[]);
    png(ask(&f, "png"));
    thread::sleep(Duration::from_millis(300));

    // A reinstall writes the new file beside the old one and renames it over.
    fs::copy(FAKE_HELPER, bin.join("new")).unwrap();
    fs::rename(bin.join("new"), &exe).unwrap();

    // The request that notices is still served, by the old build.
    png(ask(&f, "png"));
    assert!(f.wait_exit(Duration::from_secs(3)).success());
    assert_eq!(f.lines("exits"), ["Replaced"]);
    assert!(!f.socket.exists());
}

#[test]
fn spawned_helpers_are_detached() {
    let f = Fixture::new("detached");
    let pid_file = f.root.join("pid");
    let mut cmd = Command::new("sh");
    cmd.arg("-c").arg(format!("echo $$ > '{}'; exec sleep 10", pid_file.display())).env_remove("SLERP_LOG");
    let t = Instant::now();
    spawn_detached(&mut cmd).unwrap();
    let took = t.elapsed();

    let until = Instant::now() + Duration::from_secs(5);
    let pid: i32 = loop {
        if let Ok(pid) = fs::read_to_string(&pid_file).unwrap_or_default().trim().parse() {
            break pid;
        }
        assert!(Instant::now() < until);
        thread::sleep(Duration::from_millis(2));
    };
    // SAFETY: getsid has no preconditions.
    let (our_session, session) = unsafe { (libc::getsid(0), libc::getsid(pid)) };
    assert_ne!(session, our_session, "still in our session");
    #[cfg(target_os = "linux")]
    {
        let stat = fs::read_to_string(format!("/proc/{pid}/stat")).unwrap();
        let fields: Vec<&str> = stat[stat.rfind(')').unwrap() + 2..].split(' ').collect();
        let ppid: u32 = fields[1].parse().unwrap();
        assert_ne!(ppid, std::process::id(), "still our child");
        for fd in 0..3 {
            assert_eq!(fs::read_link(format!("/proc/{pid}/fd/{fd}")).unwrap(), Path::new("/dev/null"));
        }
        assert_eq!(fs::read_link(format!("/proc/{pid}/cwd")).unwrap(), Path::new("/"));
    }
    // Elsewhere, ps and lsof tell the parent, standard streams and folder.
    #[cfg(not(target_os = "linux"))]
    {
        let ps = Command::new("ps").args(["-o", "ppid=", "-p", &pid.to_string()]).output().unwrap();
        let ppid: u32 = String::from_utf8_lossy(&ps.stdout).trim().parse().unwrap();
        assert_ne!(ppid, std::process::id(), "still our child");
        let lsof =
            Command::new("lsof").args(["-a", "-p", &pid.to_string(), "-d", "cwd,0,1,2", "-Fn"]).output().unwrap();
        let names: Vec<String> = String::from_utf8_lossy(&lsof.stdout)
            .lines()
            .filter_map(|l| l.strip_prefix('n').map(String::from))
            .collect();
        assert_eq!(names, ["/", "/dev/null", "/dev/null", "/dev/null"], "{lsof:?}");
    }
    // SAFETY: pid is the sleep we started.
    unsafe { libc::kill(pid, libc::SIGTERM) };
    eprintln!("spawn_detached took {:.2} ms", took.as_secs_f64() * 1e3);
}

//endregion

/*┌─────────────────╮
| Bad input       |
└─────────────────╯*/
//region Bad input

#[test]
fn version_mismatch() {
    let mut f = Fixture::new("version");
    f.start(&[]);

    // An older client gets the helper's version back, and the helper
    // carries on.
    assert_eq!(raw(&f, &raw_request("/fake/png", 0, 64)).0, VERSION_REPLY);
    png(ask(&f, "png"));

    // A newer client means a newer build is installed: the same answer,
    // and the helper steps down.
    assert_eq!(raw(&f, &raw_request("/fake/png", 2, 64)).0, VERSION_REPLY);
    assert!(f.wait_exit(Duration::from_secs(3)).success());
    assert_eq!(f.lines("exits"), ["Superseded"]);
    assert!(!f.socket.exists());

    // A client meeting an older helper gets a clean error.
    let listener = UnixListener::bind(&f.socket).unwrap();
    thread::scope(|s| {
        s.spawn(|| {
            let (mut conn, _) = listener.accept().unwrap();
            let mut header = [0; 8];
            conn.read_exact(&mut header).unwrap();
            conn.write_all(b"PT\x00\x47\0\0\0\0").unwrap();
        });
        let r = request_at(&f.socket, &req("png", 2000), no_start);
        assert!(matches!(r, Err(ClientError::VersionMismatch { helper: 0 })), "{r:?}");
    });
}

#[test]
fn garbage_and_oversized_requests() {
    let mut f = Fixture::new("garbage");
    f.start(&[]);
    let quick = Duration::from_millis(500);

    // Not Slerp at all.
    let (reply, t) = raw(&f, b"GET / HTTP/1.1\r\nHost: localhost\r\n\r\n");
    assert!(reply.is_empty() && t < quick, "{reply:?} {t:?}");
    let noise: Vec<u8> = (0..4096u32).map(|i| (i.wrapping_mul(2_654_435_761) >> 13) as u8).collect();
    let (reply, t) = raw(&f, &noise);
    assert!(reply.is_empty() && t < quick, "{reply:?} {t:?}");

    // A request claiming to be 4 GiB is refused without reading it.
    let (reply, t) = raw(&f, b"PT\x01\x01\xff\xff\xff\xff");
    assert!(reply.is_empty() && t < quick, "{reply:?} {t:?}");
    // An unknown kind, and a reply kind sent as a request.
    assert!(raw(&f, b"PT\x01\x7f\0\0\0\0").0.is_empty());
    assert!(raw(&f, b"PT\x01\x41\0\0\0\0").0.is_empty());

    // A request cut short: the helper waits a second for the rest.
    let (reply, t) = raw(&f, &raw_request("/fake/png", 1, 64)[..15]);
    assert!(reply.is_empty() && t >= Duration::from_millis(900) && t < Duration::from_secs(3), "{t:?}");
    // A client that connects and says nothing.
    let (reply, t) = raw(&f, b"");
    assert!(reply.is_empty() && t >= Duration::from_millis(900) && t < Duration::from_secs(3), "{t:?}");

    // Well-formed but invalid requests get a reason.
    for (message, why) in [
        (raw_request("relative/png", 1, 64), "path not absolute"),
        (raw_request("/fake/png", 1, 0), "size out of range"),
        (raw_request("/fake/png", 1, 100_000), "size out of range"),
    ] {
        let (reply, _) = raw(&f, &message);
        assert_eq!(reply[..4], *b"PT\x01\x44", "{reply:?}");
        let text = String::from_utf8_lossy(&reply[8..]);
        assert!(text.contains(why), "{text}");
    }

    // Through it all, the helper kept serving.
    assert_eq!(png(ask(&f, "png")).len(), 67);
}

#[test]
fn clients_disconnecting_mid_reply() {
    let mut f = Fixture::new("disconnect");
    f.start(&["--threads", "2"]);
    // Hang up after the first 4 KiB of a 50 MB reply.  The helper would die
    // of SIGPIPE here if it wrote without MSG_NOSIGNAL, since the fake
    // helper doesn't ignore the signal.
    for _ in 0..5 {
        let mut stream = UnixStream::connect(&f.socket).unwrap();
        stream.write_all(&raw_request("/fake/big-50000000", 1, 64)).unwrap();
        let mut first = [0; 4096];
        stream.read_exact(&mut first).unwrap();
    }
    // Hang up before the reply is ready.
    for _ in 0..5 {
        let mut stream = UnixStream::connect(&f.socket).unwrap();
        stream.write_all(&raw_request("/fake/sleep-50", 1, 64)).unwrap();
    }
    thread::sleep(Duration::from_millis(300));
    assert!(f.helpers[0].try_wait().unwrap().is_none(), "the helper died; log:\n{}", f.log());
    assert_eq!(png(ask(&f, "png")).len(), 67);
}

#[test]
fn missing_or_unwritable_runtime_dir() {
    // No runtime directory: fails at once, starting nothing.
    let t = Instant::now();
    let r = request_at(Path::new("/nonexistent-slerp/slerp/socket"), &req("png", 1500), no_start);
    assert!(matches!(r, Err(ClientError::NoRuntimeDir)), "{r:?}");
    assert!(t.elapsed() < Duration::from_millis(50), "{:?}", t.elapsed());

    // A read-only runtime directory: the same, and no helper could start.
    let f = Fixture::new("read-only");
    let read_only = f.root.join("ro");
    fs::create_dir(&read_only).unwrap();
    fs::set_permissions(&read_only, fs::Permissions::from_mode(0o500)).unwrap();
    let socket = read_only.join("slerp").join("socket");
    let r = request_at(&socket, &req("png", 1500), no_start);
    assert!(matches!(r, Err(ClientError::NoRuntimeDir)), "{r:?}");
    let opts = ServeOptions { socket: Some(socket), ..ServeOptions::default() };
    assert!(matches!(Server::bind(opts), Err(ServeError::Io(_))));
    fs::set_permissions(&read_only, fs::Permissions::from_mode(0o700)).unwrap();

    // A usable runtime directory with no helper: the client makes the
    // private directory and starts one, also at once.
    let t = Instant::now();
    let mut started = false;
    let r = request_at(&f.socket, &req("png", 1500), || {
        started = true;
        Ok(())
    });
    assert!(matches!(r, Err(ClientError::NotRunning)) && started, "{r:?}");
    assert!(t.elapsed() < Duration::from_millis(50), "{:?}", t.elapsed());
    assert_eq!(fs::metadata(f.socket.parent().unwrap()).unwrap().mode() & 0o777, 0o700);

    // A start that fails is reported as such.
    let r = request_at(&f.socket, &req("png", 1500), || Err(io::Error::other("no such program")));
    assert!(matches!(r, Err(ClientError::StartFailed(_))), "{r:?}");
}

//endregion

/*┌─────────────────╮
| Isolation       |
└─────────────────╯*/
//region Isolation

#[test]
fn other_uid_is_refused() {
    let mut f = Fixture::new("other-uid");
    // Another uid without root: a user namespace that maps this user's
    // subordinate uids (newuidmap and /etc/subuid), where uid 1 is a
    // different user to the kernel.  The probe is copied where that user
    // can run it.
    let probe = f.root.join("probe");
    fs::copy(PROBE, &probe).unwrap();
    for (path, mode) in [(f.root.parent().unwrap(), 0o755), (f.root.as_path(), 0o755), (probe.as_path(), 0o755)] {
        fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
    }
    let as_other_uid = |args: &[&OsStr]| {
        Command::new("unshare")
            .args(["--user", "--map-root-user", "--map-auto", "--"])
            .args(["setpriv", "--reuid=1", "--regid=1", "--clear-groups", "--"])
            .arg(&probe)
            .args(args)
            .output()
    };
    match as_other_uid(&["--noop".as_ref()]) {
        Ok(out) if out.status.success() => {}
        other => return eprintln!("skipped: can't run a process as another uid: {other:?}"),
    }

    // Open the helper's directory and socket to everyone, as a careless
    // user might.
    f.start(&[]);
    fs::set_permissions(f.socket.parent().unwrap(), fs::Permissions::from_mode(0o711)).unwrap();
    fs::set_permissions(&f.socket, fs::Permissions::from_mode(0o777)).unwrap();

    // The helper hangs up on a raw request from the other uid without a
    // reply.
    let socket = f.socket.as_os_str();
    let out = as_other_uid(&["--raw".as_ref(), "--socket".as_ref(), socket, "/fake/png".as_ref()]).unwrap();
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "received=0", "{out:?}");
    assert!(f.log().contains("refused a connection from uid "), "{}", f.log());

    // The client refuses too: the helper isn't its user.
    let out = as_other_uid(&["--socket".as_ref(), socket, "/fake/png".as_ref()]).unwrap();
    let line = String::from_utf8_lossy(&out.stdout);
    assert!(line.starts_with("outcome=Unreachable") && line.contains("start_called=false"), "{line}");

    // The owner is still served.
    assert_eq!(png(ask(&f, "png")).len(), 67);
}

#[test]
fn gnome_sandbox_fails_fast() {
    let f = Fixture::new("gnome");
    let out = f.root.join("out");
    fs::create_dir(&out).unwrap();
    let input = f.root.join("model.glb");
    fs::write(&input, b"glTF\x02\0\0\0").unwrap();

    // The arguments add_bwrap in gnome-desktop-thumbnail-script.c gives
    // (minus the seccomp filter, which allows Unix sockets anyway), in its
    // order.
    let mut cmd = Command::new("bwrap");
    cmd.args(["--ro-bind", "/usr", "/usr", "--ro-bind-try", "/etc/ld.so.cache", "/etc/ld.so.cache"]);
    for dir in ["/bin", "/lib64", "/lib", "/sbin"] {
        let Ok(meta) = fs::metadata(dir) else { continue };
        let usr = format!("/usr{dir}");
        if fs::metadata(&usr).is_ok_and(|m| m.dev() == meta.dev() && m.ino() == meta.ino()) {
            cmd.args(["--symlink", &format!("/usr/{dir}"), dir]);
        } else {
            cmd.args(["--ro-bind", dir, dir]);
        }
    }
    cmd.args(["--ro-bind-try", "/var/cache/fontconfig", "/var/cache/fontconfig"]);
    cmd.args(["--ro-bind-try", "/etc/alternatives", "/etc/alternatives"]);
    cmd.args(["--proc", "/proc", "--dev", "/dev", "--chdir", "/", "--setenv", "GIO_USE_VFS", "local"]);
    cmd.args(["--unshare-all", "--die-with-parent"]);
    cmd.arg("--bind").arg(&out).arg("/tmp");
    cmd.arg("--ro-bind").arg(&input).arg("/tmp/gnome-desktop-file-to-thumbnail.glb");
    // Not GNOME's: the probe stands in for /usr/bin/slerp, which the
    // /usr bind would show.
    cmd.arg("--ro-bind").arg(PROBE).arg("/slerp-probe");
    cmd.args(["/slerp-probe", "/tmp/gnome-desktop-file-to-thumbnail.glb"]);
    // bwrap passes the environment through, so the client sees a runtime
    // directory the sandbox doesn't have.
    // SAFETY: geteuid has no preconditions.
    cmd.env("XDG_RUNTIME_DIR", format!("/run/user/{}", unsafe { libc::geteuid() }));

    let t = Instant::now();
    let output = match cmd.output() {
        Ok(output) if output.status.success() => output,
        other => return eprintln!("skipped: bwrap doesn't run here: {other:?}"),
    };
    let whole = t.elapsed();
    let line = String::from_utf8_lossy(&output.stdout);
    assert!(line.starts_with("outcome=NoRuntimeDir start_called=false"), "{line}");
    let us: u64 = line.trim().rsplit('=').next().unwrap().parse().unwrap();
    assert!(us < 50_000, "{line}");
    eprintln!(
        "GNOME sandbox: the client gave up in {us} us; bwrap and the probe took {:.1} ms",
        whole.as_secs_f64() * 1e3
    );
}

//endregion
