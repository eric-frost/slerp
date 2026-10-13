//! Unix: the work runs in a forked child, which calls the job's function
//! with the parent's memory as it was and answers through a pipe.

use crate::{Job, Limits, NO_ANSWER, TOO_LARGE, decode, encode, guarded, say, too_slow};
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
/// never reach the parent's terminal, no core dump, and the limits.
fn enter_child(cpu_seconds: Option<u64>, data_limit: Option<u64>) {
    no_core_dump();
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

/// Keeps a crash of this process from writing a core dump.  A crash in an
/// isolated child is an expected outcome of a hostile file, not a bug to
/// debug, and a forked child's dump holds all of its parent's memory: with
/// systemd-coredump, each one took about 130 ms and stored a copy of the
/// thumbnail helper or the viewer.
pub(crate) fn no_core_dump() {
    // SAFETY: setrlimit and prctl with valid arguments, on this process.
    unsafe {
        let none = libc::rlimit { rlim_cur: 0, rlim_max: 0 };
        libc::setrlimit(libc::RLIMIT_CORE, &none);
        // A core pattern that pipes to a program ignores RLIMIT_CORE; a
        // process that isn't dumpable is never dumped.
        #[cfg(any(target_os = "linux", target_os = "android"))]
        libc::prctl(libc::PR_SET_DUMPABLE, 0);
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

/// Why reading from the child stopped.
enum Gone {
    /// The deadline passed.
    Late,
    /// The pipe closed, cleanly or part-way.
    Ended,
    Error(String),
}

/// Reads exactly `buf.len()` bytes, or reports a clean end of input as
/// `Ok(false)` when nothing was read.  `deadline` of `None` waits for ever.
fn read_exact(fd: i32, buf: &mut [u8], deadline: Option<Instant>) -> Result<bool, Gone> {
    let mut got = 0;
    while got < buf.len() {
        if let Some(d) = deadline {
            let left = d.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(Gone::Late);
            }
            let mut pfd = libc::pollfd { fd, events: libc::POLLIN, revents: 0 };
            let ms = left.as_millis().clamp(1, i32::MAX as u128) as i32;
            // SAFETY: pfd is one valid pollfd.
            let ready = unsafe { libc::poll(&mut pfd, 1, ms) };
            if ready < 0 && std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted {
                return Err(Gone::Error(std::io::Error::last_os_error().to_string()));
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
            return Err(Gone::Error(std::io::Error::last_os_error().to_string()));
        }
        if n == 0 {
            return if got == 0 { Ok(false) } else { Err(Gone::Ended) };
        }
        got += n as usize;
    }
    Ok(true)
}

/// Why the child gave no answer, before knowing how it ended.
enum Failure {
    Late(Duration),
    Ended,
    TooLarge,
    Error(String),
}

/// Reads one encoded answer: the answer itself (which may be the job's own
/// error), or why there was none.
fn read_answer(fd: i32, time: Duration, cap: usize) -> Result<Result<Vec<u8>, String>, Failure> {
    let deadline = Instant::now() + time;
    let failed = |g: Gone| match g {
        Gone::Late => Failure::Late(time),
        Gone::Ended => Failure::Ended,
        Gone::Error(e) => Failure::Error(e),
    };
    let mut head = [0u8; 9];
    if !read_exact(fd, &mut head, Some(deadline)).map_err(failed)? {
        return Err(Failure::Ended);
    }
    let len = u64::from_le_bytes(head[1..].try_into().unwrap_or([0; 8]));
    if len > cap as u64 {
        return Err(Failure::TooLarge);
    }
    let mut body = vec![0u8; len as usize];
    if len > 0 && !read_exact(fd, &mut body, Some(deadline)).map_err(failed)? {
        return Err(Failure::Ended);
    }
    Ok(decode(head[0], body))
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

/// What a child that ended by `signal` did, as the end of a sentence.
pub(crate) fn crashed(signal: i32) -> String {
    match signal {
        libc::SIGXCPU => "used too much processor time".into(),
        // A failed allocation, a C `assert`, and a stack overflow (which
        // Rust reports and then aborts) all end in SIGABRT.
        libc::SIGABRT => "crashed or ran out of memory".into(),
        libc::SIGSEGV | libc::SIGBUS => "crashed (it read or wrote memory it doesn't own)".into(),
        s => format!("crashed (signal {s})"),
    }
}

/// The message for a failure, given the signal that ended the child.
fn explain(job: &Job, failure: Failure, signal: Option<i32>) -> String {
    match (failure, signal) {
        (Failure::Ended, Some(s)) => say(job, &crashed(s)),
        (Failure::Ended, None) => say(job, NO_ANSWER),
        (Failure::Late(time), _) => say(job, &too_slow(time)),
        (Failure::TooLarge, _) => say(job, TOO_LARGE),
        (Failure::Error(e), _) => say(job, &format!("couldn't be heard ({e})")),
    }
}

pub fn run(job: &'static Job, input: &[u8], limits: Limits) -> Result<Vec<u8>, String> {
    // 1. A pipe for the answer, and the limits worked out before the
    //    fork (reading /proc allocates).
    let (read_fd, write_fd) = pipe()?;
    let data = data_limit(&limits);
    let cpu = limits.time.as_secs() + 2;

    // SAFETY: the child only runs the job, writes to the pipe and leaves
    // with _exit, never returning into the caller's code or running its
    // destructors.  glibc and macOS make allocation safe after fork.
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
        let result = guarded(job, || job.open(input)).map(|(_, answer)| answer);
        let (head, body) = encode(result);
        let ok = write_all(write_fd, &head) && write_all(write_fd, &body);
        leave(if ok { 0 } else { 2 });
    }

    // 3. The parent reads the answer, waiting no longer than allowed.
    close(write_fd);
    let answer = read_answer(read_fd, limits.time, limits.output);
    close(read_fd);

    // 4. Reap the child, killing it first if it is still running.
    let signal = reap(pid);
    answer.unwrap_or_else(|failure| Err(explain(job, failure, signal)))
}

/// The parent's side of a child that serves requests.
pub struct Worker {
    job: &'static Job,
    pid: libc::pid_t,
    to_child: i32,
    from_child: i32,
    pub limits: Limits,
}

impl Worker {
    pub fn start(job: &'static Job, input: &[u8], limits: Limits) -> Result<(Worker, Vec<u8>), String> {
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
            // 1. The opening, whose answer comes first.
            let mut session = match guarded(job, || job.open(input)) {
                Ok((session, opening)) => {
                    if !answer(Ok(opening)) {
                        leave(2);
                    }
                    match session {
                        Some(s) => s,
                        None => leave(0),
                    }
                }
                Err(e) => {
                    answer(Err(e));
                    leave(0);
                }
            };
            // 2. Requests until the parent closes its end.
            loop {
                let mut len = [0u8; 8];
                if !matches!(read_exact(req_read, &mut len, None), Ok(true)) {
                    break;
                }
                let Ok(len) = usize::try_from(u64::from_le_bytes(len)) else { break };
                let mut request = vec![0u8; len];
                if !matches!(read_exact(req_read, &mut request, None), Ok(true)) && len > 0 {
                    break;
                }
                if !answer(guarded(job, || session(&request))) {
                    break;
                }
            }
            leave(0);
        }
        close(req_read);
        close(resp_write);
        let mut worker = Worker { job, pid, to_child: req_write, from_child: resp_read, limits };
        match read_answer(worker.from_child, limits.time, limits.output) {
            Ok(Ok(opening)) => Ok((worker, opening)),
            Ok(Err(message)) => {
                worker.end();
                Err(message)
            }
            Err(failure) => Err(worker.stop(failure)),
        }
    }

    pub fn alive(&self) -> bool {
        self.pid > 0
    }

    pub fn ask(&mut self, request: &[u8], time: Duration) -> Result<Vec<u8>, String> {
        if !self.alive() {
            return Err(say(self.job, "has stopped"));
        }
        let mut message = (request.len() as u64).to_le_bytes().to_vec();
        message.extend_from_slice(request);
        if !write_all(self.to_child, &message) {
            return Err(self.stop(Failure::Ended));
        }
        match read_answer(self.from_child, time, self.limits.output) {
            Ok(answer) => answer,
            Err(failure) => Err(self.stop(failure)),
        }
    }

    /// Ends the child after a failure and says what happened to it.
    fn stop(&mut self, failure: Failure) -> String {
        let signal = self.end();
        explain(self.job, failure, signal)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing;

    fn limits() -> Limits {
        Limits { time: Duration::from_secs(5), memory: 256 << 20, output: 1 << 20 }
    }

    #[test]
    fn a_result_comes_back() {
        assert_eq!(run(&testing::ECHO, b"hello", limits()).unwrap(), b"hello");
        assert_eq!(run(&testing::FAIL, b"nope", limits()).unwrap_err(), "nope");
        assert_eq!(run(&testing::ECHO, b"", limits()).unwrap(), b"");
    }

    #[test]
    fn a_panic_in_the_child_is_an_error() {
        let e = run(&testing::PANIC, b"boom", limits()).unwrap_err();
        assert!(e.contains("failed inside") && e.contains("boom"), "{e}");
    }

    #[test]
    fn a_crash_is_survived() {
        for job in [&testing::ABORT, &testing::RUST_ABORT, &testing::BAD_POINTER] {
            let e = run(job, b"", limits()).unwrap_err();
            assert!(e.starts_with("the test job crashed"), "{e}");
        }
        assert!(run(&testing::ECHO, b"1", limits()).is_ok());
    }

    #[test]
    fn a_stack_overflow_is_survived() {
        let e = run(&testing::OVERFLOW, b"", limits()).unwrap_err();
        assert!(e.contains("crashed"), "{e}");
    }

    #[test]
    fn a_hang_is_cut_off() {
        let l = Limits { time: Duration::from_millis(300), ..limits() };
        let started = Instant::now();
        let e = run(&testing::HANG, b"", l).unwrap_err();
        assert!(e.contains("took longer than"), "{e}");
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn too_much_output_is_cut_off() {
        let l = Limits { output: 1000, ..limits() };
        let e = run(&testing::BIG, &10_000_000u64.to_le_bytes(), l).unwrap_err();
        assert!(e.contains("too large"), "{e}");
    }

    #[test]
    fn printing_doesnt_reach_the_answer() {
        assert_eq!(run(&testing::NOISY, b"quiet", limits()).unwrap(), b"quiet");
    }

    /// Only Linux limits the child's memory: `RLIMIT_DATA` doesn't count
    /// the memory macOS's malloc maps, and macOS doesn't enforce `RLIMIT_AS`.
    #[test]
    #[cfg(target_os = "linux")]
    fn a_greedy_child_is_stopped_by_the_memory_limit() {
        let l = Limits { memory: 64 << 20, ..limits() };
        let e = run(&testing::GREEDY, b"", l).unwrap_err();
        assert!(e.contains("crashed or ran out of memory"), "{e}");
        let e = run(&testing::HUGE, b"", l).unwrap_err();
        assert!(e.contains("crashed or ran out of memory"), "{e}");
    }

    #[test]
    fn a_worker_keeps_its_state_and_survives_a_crash() {
        let (mut w, opening) = Worker::start(&testing::COUNTER, &[10], limits()).unwrap();
        assert_eq!(opening, b"open");
        assert_eq!(w.ask(b"a", limits().time).unwrap(), [11]);
        assert_eq!(w.ask(b"b", limits().time).unwrap(), [12]);
        assert_eq!(w.ask(b"fail", limits().time).unwrap_err(), "no");
        assert!(w.alive());
        let e = w.ask(b"crash", limits().time).unwrap_err();
        assert!(e.contains("crashed"), "{e}");
        assert!(!w.alive());
        assert!(w.ask(b"a", limits().time).is_err());
    }

    #[test]
    fn a_failed_opening_is_an_error() {
        let e = Worker::start(&testing::COUNTER, b"", limits()).err().unwrap();
        assert_eq!(e, "no starting count");
        let e = Worker::start(&testing::PANIC, b"bad", limits()).err().unwrap();
        assert!(e.contains("bad"), "{e}");
    }

    #[test]
    fn a_hung_worker_is_cut_off() {
        let (mut w, _) = Worker::start(&testing::COUNTER, &[0], limits()).unwrap();
        let started = Instant::now();
        let e = w.ask(b"hang", Duration::from_millis(300)).unwrap_err();
        assert!(e.contains("took longer than"), "{e}");
        assert!(started.elapsed() < Duration::from_secs(5));
        assert!(!w.alive());
    }
}
