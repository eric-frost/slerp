//! Running code that reads untrusted files where a fault in it costs one
//! picture, not the program.
//!
//! Decoders run on whatever a user browses past, downloads included.  Some
//! are C libraries, some are Rust whose authors could not bound everything
//! (a PDF can nest, loop and inflate in more ways than any renderer
//! checks), and a crafted file can make them crash, abort on a failed
//! allocation, overflow the stack or run for ever.  On Unix the work runs
//! in a forked child with a memory limit, the parent waits for its answer
//! through a pipe for a set time and kills it after that; a crash, a hang
//! or a runaway allocation becomes an error message.  Windows has no fork:
//! there the work runs on a thread of its own behind a panic guard, and a
//! thread that overruns its time is abandoned (it goes on until it ends,
//! but the caller no longer waits for it).
//!
//! [`run`] does one piece of work.  A [`Worker`] keeps a child for a series
//! of requests that share state, such as the pages of one open document,
//! which is read once and then drawn many times.
//!
//! This crate depends on nothing of Slerp's, so any loader may use it.  It
//! is a generalised copy of `crates/images/src/isolate.rs`.

use std::time::Duration;

/// What the work may use.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    /// Wall-clock time the work may take: all of it for [`run`], each
    /// request (and the setup) for a [`Worker`].
    pub time: Duration,
    /// Bytes of memory the child may add to what the parent holds (Linux
    /// only; elsewhere unchecked).
    pub memory: u64,
    /// The most bytes one answer may have.
    pub output: usize,
}

/// Runs `work` where a fault in it cannot take the caller down, and returns
/// the bytes it produced or why it failed.
pub fn run(limits: Limits, work: impl FnOnce() -> Result<Vec<u8>, String> + Send + 'static) -> Result<Vec<u8>, String> {
    imp::run(limits, work)
}

/// Work kept running between requests, in a child process on Unix, for
/// requests that share state: `setup` makes the state (opening a document,
/// say) and an opening answer, then `serve` answers each request with it.
/// A crash, a hang or a too large answer ends the worker, and that request
/// and every later one fail; the caller may start a new one.
pub struct Worker(imp::Worker);

impl Worker {
    /// Starts the worker and returns it with `setup`'s opening answer, or
    /// `setup`'s error (or why it never answered within `limits.time`).
    pub fn start<S: 'static>(
        limits: Limits,
        setup: impl FnOnce() -> Result<(S, Vec<u8>), String> + Send + 'static,
        serve: impl FnMut(&mut S, &[u8]) -> Result<Vec<u8>, String> + Send + 'static,
    ) -> Result<(Worker, Vec<u8>), String> {
        imp::Worker::start(limits, setup, serve).map(|(w, opening)| (Worker(w), opening))
    }

    /// Sends a request and waits for its answer for the worker's time limit.
    pub fn ask(&mut self, request: &[u8]) -> Result<Vec<u8>, String> {
        let time = self.0.limits.time;
        self.0.ask(request, time)
    }

    /// Sends a request and waits for its answer for `time`.
    pub fn ask_within(&mut self, request: &[u8], time: Duration) -> Result<Vec<u8>, String> {
        self.0.ask(request, time)
    }

    /// Whether the worker is still there to ask.
    pub fn alive(&self) -> bool {
        self.0.alive()
    }
}

/// The message for an answer that took too long.
fn too_slow(time: Duration) -> String {
    format!("it took longer than {:.1} s", time.as_secs_f32())
}

/// Encodes an answer: a tag (0 for a result, 1 for an error message), its
/// length and its bytes.
#[cfg(unix)]
fn encode(result: Result<Vec<u8>, String>) -> (Vec<u8>, Vec<u8>) {
    let (tag, body) = match result {
        Ok(bytes) => (0u8, bytes),
        Err(message) => (1u8, message.into_bytes()),
    };
    let mut head = vec![tag];
    head.extend((body.len() as u64).to_le_bytes());
    (head, body)
}

/// A panic's message, for the error it becomes.
fn panic_text(p: &(dyn std::any::Any + Send)) -> String {
    let text = p.downcast_ref::<&str>().map(|s| s.to_string()).or_else(|| p.downcast_ref::<String>().cloned());
    match text {
        Some(t) => format!("it failed inside ({})", t.chars().take(200).collect::<String>()),
        None => "it failed inside".into(),
    }
}

#[cfg(unix)]
mod imp {
    use super::{Limits, encode, panic_text, too_slow};
    use std::time::{Duration, Instant};

    /// Makes a pipe whose ends close in programs this process runs.
    fn pipe() -> Result<(i32, i32), String> {
        let mut fds = [0i32; 2];
        // SAFETY: fds has room for the two descriptors pipe writes.
        if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
            return Err(format!("couldn't make a pipe: {}", std::io::Error::last_os_error()));
        }
        for fd in fds {
            // SAFETY: fd is a descriptor this function made.
            unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) };
        }
        Ok((fds[0], fds[1]))
    }

    fn close(fd: i32) {
        if fd >= 0 {
            // SAFETY: closing a descriptor the caller owns.
            unsafe { libc::close(fd) };
        }
    }

    /// In a newly forked child: output to /dev/null, so libraries' complaints
    /// never reach the parent's terminal, and the limits.
    fn enter_child(cpu_seconds: Option<u64>, data_limit: Option<u64>) {
        // SAFETY: open, dup2 and setrlimit get valid arguments; the
        // descriptors replaced are the child's own.
        unsafe {
            let null = libc::open(c"/dev/null".as_ptr(), libc::O_WRONLY);
            if null >= 0 {
                libc::dup2(null, 1);
                libc::dup2(null, 2);
                libc::close(null);
            }
            if let Some(s) = cpu_seconds {
                let cpu = libc::rlimit { rlim_cur: s as _, rlim_max: s as _ };
                libc::setrlimit(libc::RLIMIT_CPU, &cpu);
            }
            #[cfg(target_os = "linux")]
            if let Some(limit) = data_limit {
                let data = libc::rlimit { rlim_cur: limit as _, rlim_max: limit as _ };
                libc::setrlimit(libc::RLIMIT_DATA, &data);
            }
            #[cfg(not(target_os = "linux"))]
            let _ = data_limit;
        }
    }

    /// Ends the child process without running the parent's destructors.
    fn leave(code: i32) -> ! {
        // SAFETY: _exit ends the process at once.
        unsafe { libc::_exit(code) }
    }

    /// Bytes of heap and private data the process holds now, which a limit on
    /// the child's data segment must allow for since it inherits them (Linux).
    fn current_data_bytes() -> Option<u64> {
        let status = std::fs::read_to_string("/proc/self/status").ok()?;
        let line = status.lines().find(|l| l.starts_with("VmData:"))?;
        let kb: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
        Some(kb * 1024)
    }

    fn data_limit(limits: &Limits) -> Option<u64> {
        current_data_bytes().map(|d| d.saturating_add(limits.memory))
    }

    fn write_all(fd: i32, mut data: &[u8]) -> bool {
        while !data.is_empty() {
            // SAFETY: data is valid for reads of its length.
            let n = unsafe { libc::write(fd, data.as_ptr().cast(), data.len()) };
            if n < 0 {
                if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                return false;
            }
            data = &data[n as usize..];
        }
        true
    }

    /// Reads exactly `buf.len()` bytes, or reports a clean end of input as
    /// `Ok(false)` when nothing was read.  `deadline` of `None` waits for ever.
    fn read_exact(fd: i32, buf: &mut [u8], deadline: Option<Instant>) -> Result<bool, String> {
        let mut got = 0;
        while got < buf.len() {
            if let Some(d) = deadline {
                let left = d.saturating_duration_since(Instant::now());
                if left.is_zero() {
                    return Err("timeout".into());
                }
                let mut pfd = libc::pollfd { fd, events: libc::POLLIN, revents: 0 };
                let ms = left.as_millis().clamp(1, i32::MAX as u128) as i32;
                // SAFETY: pfd is one valid pollfd.
                let ready = unsafe { libc::poll(&mut pfd, 1, ms) };
                if ready < 0 && std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted {
                    return Err(std::io::Error::last_os_error().to_string());
                }
                if ready <= 0 {
                    continue;
                }
            }
            // SAFETY: the rest of buf is valid for writes.
            let n = unsafe { libc::read(fd, buf[got..].as_mut_ptr().cast(), buf.len() - got) };
            if n < 0 {
                if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(std::io::Error::last_os_error().to_string());
            }
            if n == 0 {
                return if got == 0 { Ok(false) } else { Err("cut short".into()) };
            }
            got += n as usize;
        }
        Ok(true)
    }

    /// Reads one encoded answer: the answer itself (which may be the work's
    /// own error), or why there was none.
    fn read_answer(fd: i32, deadline: Instant, cap: usize, time: Duration) -> Result<Result<Vec<u8>, String>, String> {
        let late = |e: String| if e == "timeout" { too_slow(time) } else { e };
        let mut head = [0u8; 9];
        if !read_exact(fd, &mut head, Some(deadline)).map_err(late)? {
            return Err("it stopped without an answer".into());
        }
        let len = u64::from_le_bytes(head[1..].try_into().unwrap_or([0; 8]));
        if len > cap as u64 {
            return Err("its answer was too large".into());
        }
        let mut body = vec![0u8; len as usize];
        if len > 0 && !read_exact(fd, &mut body, Some(deadline)).map_err(late)? {
            return Err("it stopped without an answer".into());
        }
        Ok(match head[0] {
            0 => Ok(body),
            _ => Err(String::from_utf8_lossy(&body).into_owned()),
        })
    }

    /// Kills (unless it has ended) and reaps a child, returning the signal
    /// that ended it, other than the parent's own kill.
    fn reap(pid: libc::pid_t) -> Option<i32> {
        let mut status = 0i32;
        // SAFETY: pid is our own child, not yet reaped; status is a valid
        // out pointer.
        unsafe {
            let r = libc::waitpid(pid, &mut status, libc::WNOHANG);
            if r == 0 {
                libc::kill(pid, libc::SIGKILL);
                while libc::waitpid(pid, &mut status, 0) < 0
                    && std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted
                {}
            }
        }
        libc::WIFSIGNALED(status).then(|| libc::WTERMSIG(status)).filter(|&s| s != libc::SIGKILL)
    }

    /// What a child that ended by a signal did.
    fn crashed(signal: i32) -> String {
        match signal {
            libc::SIGXCPU => "it used too much processor time".into(),
            libc::SIGABRT => "it crashed or ran out of memory".into(),
            libc::SIGSEGV => "it crashed (perhaps nesting too deep)".into(),
            s => format!("it crashed (signal {s})"),
        }
    }

    pub fn run(limits: Limits, work: impl FnOnce() -> Result<Vec<u8>, String>) -> Result<Vec<u8>, String> {
        // 1. A pipe for the answer, and the limits worked out before the
        //    fork (reading /proc allocates).
        let (read_fd, write_fd) = pipe()?;
        let data = data_limit(&limits);
        let cpu = limits.time.as_secs() + 2;

        // SAFETY: the child only runs the work, writes to the pipe and
        // leaves with _exit, never returning into the caller's code or
        // running its destructors.  glibc and macOS make allocation safe
        // after fork.
        let pid = unsafe { libc::fork() };
        if pid < 0 {
            close(read_fd);
            close(write_fd);
            return Err(format!("couldn't start a process: {}", std::io::Error::last_os_error()));
        }
        if pid == 0 {
            // 2. The child: limits first, then the work.
            close(read_fd);
            enter_child(Some(cpu), data);
            let result =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(work)).unwrap_or_else(|p| Err(panic_text(&*p)));
            let (head, body) = encode(result);
            let ok = write_all(write_fd, &head) && write_all(write_fd, &body);
            leave(if ok { 0 } else { 2 });
        }

        // 3. The parent reads the answer, waiting no longer than allowed.
        close(write_fd);
        let deadline = Instant::now() + limits.time;
        let answer = read_answer(read_fd, deadline, limits.output, limits.time);
        close(read_fd);

        // 4. Reap the child, killing it first if it is still running.
        let signal = reap(pid);
        match answer {
            Ok(result) => result,
            Err(why) => Err(match signal {
                Some(s) if why.contains("without an answer") => crashed(s),
                _ => why,
            }),
        }
    }

    /// The parent's side of a child that serves requests.
    pub struct Worker {
        pid: libc::pid_t,
        to_child: i32,
        from_child: i32,
        pub limits: Limits,
    }

    impl Worker {
        pub fn start<S: 'static>(
            limits: Limits,
            setup: impl FnOnce() -> Result<(S, Vec<u8>), String>,
            mut serve: impl FnMut(&mut S, &[u8]) -> Result<Vec<u8>, String>,
        ) -> Result<(Worker, Vec<u8>), String> {
            let (req_read, req_write) = pipe()?;
            let (resp_read, resp_write) = match pipe() {
                Ok(p) => p,
                Err(e) => {
                    close(req_read);
                    close(req_write);
                    return Err(e);
                }
            };
            let data = data_limit(&limits);
            // SAFETY: as in `run`: the child serves, writes to its pipe and
            // leaves with _exit, never returning into the caller's code.
            let pid = unsafe { libc::fork() };
            if pid < 0 {
                for fd in [req_read, req_write, resp_read, resp_write] {
                    close(fd);
                }
                return Err(format!("couldn't start a process: {}", std::io::Error::last_os_error()));
            }
            if pid == 0 {
                close(req_write);
                close(resp_read);
                // No processor-time limit: the parent times each request.
                enter_child(None, data);
                let answer = |result: Result<Vec<u8>, String>| {
                    let (head, body) = encode(result);
                    write_all(resp_write, &head) && write_all(resp_write, &body)
                };
                // 1. The setup, whose answer comes first.
                let opened = std::panic::catch_unwind(std::panic::AssertUnwindSafe(setup))
                    .unwrap_or_else(|p| Err(panic_text(&*p)));
                let mut state = match opened {
                    Ok((state, opening)) => {
                        if !answer(Ok(opening)) {
                            leave(2);
                        }
                        state
                    }
                    Err(e) => {
                        answer(Err(e));
                        leave(0);
                    }
                };
                // 2. Requests until the parent closes its end.
                loop {
                    let mut len = [0u8; 4];
                    if !matches!(read_exact(req_read, &mut len, None), Ok(true)) {
                        break;
                    }
                    let mut request = vec![0u8; u32::from_le_bytes(len) as usize];
                    if !matches!(read_exact(req_read, &mut request, None), Ok(true)) {
                        break;
                    }
                    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| serve(&mut state, &request)))
                        .unwrap_or_else(|p| Err(panic_text(&*p)));
                    if !answer(result) {
                        break;
                    }
                }
                leave(0);
            }
            close(req_read);
            close(resp_write);
            let mut worker = Worker { pid, to_child: req_write, from_child: resp_read, limits };
            let deadline = Instant::now() + limits.time;
            match read_answer(worker.from_child, deadline, limits.output, limits.time) {
                Ok(Ok(opening)) => Ok((worker, opening)),
                Ok(Err(message)) => {
                    worker.end();
                    Err(message)
                }
                Err(why) => Err(worker.stop(why)),
            }
        }

        pub fn alive(&self) -> bool {
            self.pid > 0
        }

        pub fn ask(&mut self, request: &[u8], time: Duration) -> Result<Vec<u8>, String> {
            if !self.alive() {
                return Err("its process has stopped".into());
            }
            let mut message = (request.len() as u32).to_le_bytes().to_vec();
            message.extend_from_slice(request);
            if !write_all(self.to_child, &message) {
                return Err(self.stop("it stopped without an answer".into()));
            }
            match read_answer(self.from_child, Instant::now() + time, self.limits.output, time) {
                Ok(answer) => answer,
                Err(why) => Err(self.stop(why)),
            }
        }

        /// Ends the child after a failure and says what happened to it.
        fn stop(&mut self, why: String) -> String {
            match self.end() {
                Some(s) if why.contains("without an answer") => crashed(s),
                _ => why,
            }
        }

        /// Kills and reaps the child, returning the signal that ended it.
        fn end(&mut self) -> Option<i32> {
            if self.pid <= 0 {
                return None;
            }
            close(self.to_child);
            close(self.from_child);
            (self.to_child, self.from_child) = (-1, -1);
            let signal = reap(self.pid);
            self.pid = -1;
            signal
        }
    }

    impl Drop for Worker {
        fn drop(&mut self) {
            self.end();
        }
    }
}

#[cfg(not(unix))]
mod imp {
    use super::{Limits, panic_text, too_slow};
    use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
    use std::time::Duration;

    type Answer = Result<Vec<u8>, String>;

    fn guarded<T>(f: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).unwrap_or_else(|p| Err(panic_text(&*p)))
    }

    fn wait(rx: &Receiver<Answer>, time: Duration, output: usize) -> Answer {
        match rx.recv_timeout(time) {
            Ok(Ok(bytes)) if bytes.len() > output => Err("its answer was too large".into()),
            Ok(answer) => answer,
            Err(RecvTimeoutError::Timeout) => Err(too_slow(time)),
            Err(RecvTimeoutError::Disconnected) => Err("it stopped without an answer".into()),
        }
    }

    pub fn run(limits: Limits, work: impl FnOnce() -> Answer + Send + 'static) -> Answer {
        let (tx, rx) = channel();
        std::thread::Builder::new()
            .name("slerp-isolated".into())
            .spawn(move || {
                let _ = tx.send(guarded(work));
            })
            .map_err(|e| format!("couldn't start a thread: {e}"))?;
        wait(&rx, limits.time, limits.output)
    }

    /// A worker thread with the state; the caller stops waiting for one
    /// that overruns, and it ends when it next looks for a request.
    pub struct Worker {
        requests: Option<Sender<Vec<u8>>>,
        answers: Receiver<Answer>,
        pub limits: Limits,
    }

    impl Worker {
        pub fn start<S: 'static>(
            limits: Limits,
            setup: impl FnOnce() -> Result<(S, Vec<u8>), String> + Send + 'static,
            mut serve: impl FnMut(&mut S, &[u8]) -> Answer + Send + 'static,
        ) -> Result<(Worker, Vec<u8>), String> {
            let (req_tx, req_rx) = channel::<Vec<u8>>();
            let (ans_tx, ans_rx) = channel::<Answer>();
            std::thread::Builder::new()
                .name("slerp-isolated".into())
                .spawn(move || {
                    let mut state = match guarded(setup) {
                        Ok((state, opening)) => {
                            let _ = ans_tx.send(Ok(opening));
                            state
                        }
                        Err(e) => {
                            let _ = ans_tx.send(Err(e));
                            return;
                        }
                    };
                    while let Ok(request) = req_rx.recv() {
                        if ans_tx.send(guarded(|| serve(&mut state, &request))).is_err() {
                            break;
                        }
                    }
                })
                .map_err(|e| format!("couldn't start a thread: {e}"))?;
            let mut worker = Worker { requests: Some(req_tx), answers: ans_rx, limits };
            match wait(&worker.answers, limits.time, limits.output) {
                Ok(opening) => Ok((worker, opening)),
                Err(e) => {
                    worker.requests = None;
                    Err(e)
                }
            }
        }

        pub fn alive(&self) -> bool {
            self.requests.is_some()
        }

        pub fn ask(&mut self, request: &[u8], time: Duration) -> Answer {
            let Some(tx) = &self.requests else { return Err("its thread has stopped".into()) };
            if tx.send(request.to_vec()).is_err() {
                self.requests = None;
                return Err("it stopped without an answer".into());
            }
            let answer = wait(&self.answers, time, self.limits.output);
            if matches!(&answer, Err(e) if e.starts_with("it took") || e.starts_with("it stopped")) {
                self.requests = None;
            }
            answer
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::time::Instant;

    fn limits() -> Limits {
        Limits { time: Duration::from_secs(5), memory: 256 << 20, output: 1 << 20 }
    }

    #[test]
    fn a_result_comes_back() {
        assert_eq!(run(limits(), || Ok(b"hello".to_vec())).unwrap(), b"hello");
        assert_eq!(run(limits(), || Err("nope".into())).unwrap_err(), "nope");
        assert_eq!(run(limits(), || Ok(Vec::new())).unwrap(), b"");
    }

    #[test]
    fn a_panic_in_the_child_is_an_error() {
        let e = run(limits(), || -> Result<Vec<u8>, String> { panic!("boom") }).unwrap_err();
        assert!(e.contains("failed inside") && e.contains("boom"), "{e}");
    }

    #[test]
    fn a_crash_is_survived() {
        let e = run(limits(), || {
            // SAFETY: deliberately kills the child process with a signal.
            unsafe { libc::abort() }
        })
        .unwrap_err();
        assert!(e.contains("crashed"), "{e}");
        assert!(run(limits(), || Ok(vec![1])).is_ok());
    }

    #[test]
    fn a_stack_overflow_is_survived() {
        fn deep(n: u64) -> u64 {
            let pad = [n; 64];
            if n == 0 { 0 } else { deep(std::hint::black_box(n - 1)) + pad[3] }
        }
        let e = run(limits(), || Ok(deep(std::hint::black_box(u64::MAX)).to_le_bytes().to_vec())).unwrap_err();
        assert!(e.contains("crashed"), "{e}");
    }

    #[test]
    fn a_hang_is_cut_off() {
        let l = Limits { time: Duration::from_millis(300), ..limits() };
        let started = Instant::now();
        let e = run(l, || {
            std::thread::sleep(Duration::from_secs(30));
            Ok(Vec::new())
        })
        .unwrap_err();
        assert!(e.contains("longer than"), "{e}");
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn too_much_output_is_cut_off() {
        let l = Limits { output: 1000, ..limits() };
        let e = run(l, || Ok(vec![7u8; 10_000_000])).unwrap_err();
        assert!(e.contains("too large"), "{e}");
    }

    #[test]
    fn a_greedy_child_is_stopped_by_the_memory_limit() {
        let l = Limits { memory: 64 << 20, ..limits() };
        let r = run(l, || {
            let mut v: Vec<Vec<u8>> = Vec::new();
            for _ in 0..4096 {
                v.push(vec![1u8; 1 << 20]);
            }
            Ok(vec![v.len() as u8])
        });
        assert!(r.is_err(), "{r:?}");
    }

    #[test]
    fn a_worker_keeps_its_state_and_survives_a_crash() {
        let (mut w, opening) = Worker::start(
            limits(),
            || Ok((10u8, b"open".to_vec())),
            |count, req| {
                *count += 1;
                match req {
                    b"crash" => unsafe { libc::abort() },
                    b"fail" => Err("no".into()),
                    _ => Ok(vec![*count]),
                }
            },
        )
        .unwrap();
        assert_eq!(opening, b"open");
        assert_eq!(w.ask(b"a").unwrap(), [11]);
        assert_eq!(w.ask(b"b").unwrap(), [12]);
        assert_eq!(w.ask(b"fail").unwrap_err(), "no");
        assert!(w.alive());
        let e = w.ask(b"crash").unwrap_err();
        assert!(e.contains("crashed"), "{e}");
        assert!(!w.alive());
        assert!(w.ask(b"a").is_err());
    }

    #[test]
    fn a_failed_setup_is_an_error() {
        let r = Worker::start(limits(), || Err::<((), Vec<u8>), String>("locked".into()), |_, _| Ok(Vec::new()));
        assert_eq!(r.err().unwrap(), "locked");
        let r = Worker::start(limits(), || -> Result<((), Vec<u8>), String> { panic!("bad") }, |_, _| Ok(Vec::new()));
        assert!(r.err().unwrap().contains("bad"));
    }

    #[test]
    fn a_hung_worker_is_cut_off() {
        let (mut w, _) = Worker::start(
            limits(),
            || Ok(((), Vec::new())),
            |_, _| {
                std::thread::sleep(Duration::from_secs(30));
                Ok(Vec::new())
            },
        )
        .unwrap();
        let started = Instant::now();
        let e = w.ask_within(b"x", Duration::from_millis(300)).unwrap_err();
        assert!(e.contains("longer than"), "{e}");
        assert!(started.elapsed() < Duration::from_secs(5));
        assert!(!w.alive());
    }
}
