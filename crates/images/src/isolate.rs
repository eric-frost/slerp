//! Running a decoder that is not ours in a child process.
//!
//! Some decoders here are C or C++ libraries (the system's libheif for HEIC,
//! maybe an AV1 decoder), and those are the code that file-format bugs
//! reach: a crafted picture can crash or hang them, and a viewer runs on
//! whatever someone downloaded.  So on Unix the decode runs in a forked
//! child with a CPU limit and a memory limit and sends its answer back
//! through a pipe; a crash, a hang or a runaway allocation costs one picture
//! and an error message.  rav1d, the AV1 decoder, is Rust but its only
//! interface is dav1d's C one, through which a panic on a damaged file
//! aborts the whole process, so it runs here too.  Windows has no fork; there the decode runs in the
//! calling process, behind a panic guard only.

/// What a child may use (Windows runs decoders in-process and checks none).
#[derive(Clone, Copy, Debug)]
#[cfg_attr(not(unix), allow(dead_code))]
pub struct Limits {
    /// Seconds before the child is killed.
    pub seconds: u64,
    /// Bytes of memory the child may add to what the parent holds (Linux
    /// only; elsewhere unchecked).
    pub memory: u64,
    /// The most bytes the child may send back.
    pub output: usize,
}

impl Limits {
    /// Limits for decoding a picture whose pixels take `pixel_bytes`.
    pub fn for_pixels(pixel_bytes: usize) -> Limits {
        Limits {
            seconds: 30,
            memory: (pixel_bytes as u64).saturating_mul(6).saturating_add(768 << 20),
            output: pixel_bytes.saturating_add(1 << 16),
        }
    }
}

/// Runs `work` where a fault in it cannot take the caller down, and returns
/// the bytes it produced or why it failed.
pub fn run(limits: Limits, work: impl FnOnce() -> Result<Vec<u8>, String>) -> Result<Vec<u8>, String> {
    imp::run(limits, work)
}

/// A decoder kept running in a child process between requests, for work
/// that carries state from one request to the next (the frames of an AV1
/// sequence).  Each request gets `limits.seconds` to answer; a crash, a hang
/// or a too large answer ends the child, and that request and every later
/// one fail.  On Windows the work runs in the calling process.
pub struct Worker(imp::Worker);

impl Worker {
    /// Starts the child, which answers every request with `serve`.
    pub fn start(
        limits: Limits,
        serve: impl FnMut(&[u8]) -> Result<Vec<u8>, String> + Send + 'static,
    ) -> Result<Worker, String> {
        imp::Worker::start(limits, serve).map(Worker)
    }

    /// Sends a request and waits for its answer.
    pub fn ask(&mut self, request: &[u8]) -> Result<Vec<u8>, String> {
        self.0.ask(request)
    }

    /// Whether the child is still there to ask.
    pub fn alive(&self) -> bool {
        self.0.alive()
    }
}

#[cfg(unix)]
mod imp {
    use super::Limits;
    use std::time::{Duration, Instant};

    pub fn run(limits: Limits, work: impl FnOnce() -> Result<Vec<u8>, String>) -> Result<Vec<u8>, String> {
        // 1. A pipe for the answer.
        let mut fds = [0i32; 2];
        // SAFETY: fds has room for the two descriptors pipe writes.
        if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
            return Err(format!("couldn't make a pipe: {}", std::io::Error::last_os_error()));
        }
        let (read_fd, write_fd) = (fds[0], fds[1]);
        for fd in fds {
            // SAFETY: fd is a descriptor this function owns.
            unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) };
        }
        #[cfg_attr(not(target_os = "linux"), allow(unused_variables))]
        let data_limit = current_data_bytes().map(|d| d.saturating_add(limits.memory));

        // SAFETY: the child only runs the decoder, writes to the pipe and
        // leaves with _exit, never returning into the caller's code or
        // running its destructors.  glibc and macOS make allocation safe
        // after fork.
        let pid = unsafe { libc::fork() };
        if pid < 0 {
            // SAFETY: closing descriptors this function owns.
            unsafe {
                libc::close(read_fd);
                libc::close(write_fd);
            }
            return Err(format!("couldn't start a decoder process: {}", std::io::Error::last_os_error()));
        }
        if pid == 0 {
            // 2. The child: limits first, then the work.
            // SAFETY: closing the end the child doesn't use; setrlimit gets
            // valid structs; the descriptors duplicated onto are its own.
            unsafe {
                libc::close(read_fd);
                // Libraries print complaints; they must not reach the parent's output.
                let null = libc::open(c"/dev/null".as_ptr(), libc::O_WRONLY);
                if null >= 0 {
                    libc::dup2(null, 1);
                    libc::dup2(null, 2);
                    libc::close(null);
                }
                let cpu = libc::rlimit { rlim_cur: limits.seconds as _, rlim_max: limits.seconds as _ };
                libc::setrlimit(libc::RLIMIT_CPU, &cpu);
                #[cfg(target_os = "linux")]
                if let Some(limit) = data_limit {
                    let data = libc::rlimit { rlim_cur: limit as _, rlim_max: limit as _ };
                    libc::setrlimit(libc::RLIMIT_DATA, &data);
                }
            }
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(work))
                .unwrap_or_else(|_| Err("the decoder panicked".to_string()));
            let (tag, body) = match result {
                Ok(bytes) => (0u8, bytes),
                Err(message) => (1u8, message.into_bytes()),
            };
            let ok = write_all(write_fd, &[tag]) && write_all(write_fd, &body);
            // SAFETY: _exit ends the child without the parent's destructors.
            unsafe { libc::_exit(if ok { 0 } else { 2 }) };
        }

        // 3. The parent reads until the child closes the pipe, it runs out
        //    of time, or it sends more than it should.
        // SAFETY: closing the end the parent doesn't use.
        unsafe { libc::close(write_fd) };
        let deadline = Instant::now() + Duration::from_secs(limits.seconds);
        let cap = limits.output.saturating_add(4096);
        let mut out = Vec::new();
        let mut buf = vec![0u8; 1 << 16];
        let failure = loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break Some(format!("the decoder took longer than {} s", limits.seconds));
            }
            let mut pfd = libc::pollfd { fd: read_fd, events: libc::POLLIN, revents: 0 };
            let ms = left.as_millis().min(i32::MAX as u128) as i32;
            // SAFETY: pfd is one valid pollfd.
            let ready = unsafe { libc::poll(&mut pfd, 1, ms) };
            if ready < 0 {
                if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                break Some(format!("couldn't wait for the decoder: {}", std::io::Error::last_os_error()));
            }
            if ready == 0 {
                continue;
            }
            // SAFETY: buf is valid for writes of its length.
            let n = unsafe { libc::read(read_fd, buf.as_mut_ptr().cast(), buf.len()) };
            if n < 0 {
                if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                break Some(format!("couldn't read the decoder's answer: {}", std::io::Error::last_os_error()));
            }
            if n == 0 {
                break None;
            }
            if out.len() + n as usize > cap {
                break Some("the decoder's answer was too large".to_string());
            }
            out.extend_from_slice(&buf[..n as usize]);
        };
        // SAFETY: closing the parent's end.
        unsafe { libc::close(read_fd) };

        // 4. Reap the child, killing it first if it is still running.
        if failure.is_some() {
            // SAFETY: pid is our own child, not yet reaped.
            unsafe { libc::kill(pid, libc::SIGKILL) };
        }
        let mut status = 0i32;
        // SAFETY: pid is our own child; status is a valid out pointer.
        let reaped = loop {
            let r = unsafe { libc::waitpid(pid, &mut status, 0) };
            if r >= 0 || std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted {
                break r;
            }
        };
        if let Some(message) = failure {
            return Err(message);
        }
        if reaped == pid && libc::WIFSIGNALED(status) {
            return Err(format!("the decoder crashed (signal {}) on this file", libc::WTERMSIG(status)));
        }
        match out.split_first() {
            Some((0, body)) => Ok(body.to_vec()),
            Some((1, message)) => Err(String::from_utf8_lossy(message).into_owned()),
            _ => Err("the decoder stopped without an answer".into()),
        }
    }

    /// The parent's side of a child that serves requests.
    pub struct Worker {
        pid: libc::pid_t,
        to_child: i32,
        from_child: i32,
        limits: Limits,
    }

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
                let ms = left.as_millis().min(i32::MAX as u128) as i32;
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

    impl Worker {
        pub fn start(
            limits: Limits,
            mut serve: impl FnMut(&[u8]) -> Result<Vec<u8>, String> + Send + 'static,
        ) -> Result<Worker, String> {
            let (req_read, req_write) = pipe()?;
            let (resp_read, resp_write) = match pipe() {
                Ok(p) => p,
                Err(e) => {
                    close(req_read);
                    close(req_write);
                    return Err(e);
                }
            };
            #[cfg_attr(not(target_os = "linux"), allow(unused_variables))]
            let data_limit = current_data_bytes().map(|d| d.saturating_add(limits.memory));
            // SAFETY: as in `run`: the child serves, writes to its pipe and
            // leaves with _exit, never returning into the caller's code.
            let pid = unsafe { libc::fork() };
            if pid < 0 {
                for fd in [req_read, req_write, resp_read, resp_write] {
                    close(fd);
                }
                return Err(format!("couldn't start a decoder process: {}", std::io::Error::last_os_error()));
            }
            if pid == 0 {
                close(req_write);
                close(resp_read);
                // SAFETY: as in `run`.
                unsafe {
                    let null = libc::open(c"/dev/null".as_ptr(), libc::O_WRONLY);
                    if null >= 0 {
                        libc::dup2(null, 1);
                        libc::dup2(null, 2);
                        libc::close(null);
                    }
                    #[cfg(target_os = "linux")]
                    if let Some(limit) = data_limit {
                        let data = libc::rlimit { rlim_cur: limit as _, rlim_max: limit as _ };
                        libc::setrlimit(libc::RLIMIT_DATA, &data);
                    }
                }
                // Requests until the parent closes its end; the parent times each answer.
                loop {
                    let mut len = [0u8; 4];
                    if !matches!(read_exact(req_read, &mut len, None), Ok(true)) {
                        break;
                    }
                    let mut request = vec![0u8; u32::from_le_bytes(len) as usize];
                    if !matches!(read_exact(req_read, &mut request, None), Ok(true)) {
                        break;
                    }
                    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| serve(&request)))
                        .unwrap_or_else(|_| Err("the decoder panicked".to_string()));
                    let (tag, body) = match result {
                        Ok(bytes) => (0u8, bytes),
                        Err(message) => (1u8, message.into_bytes()),
                    };
                    let mut head = vec![tag];
                    head.extend((body.len() as u64).to_le_bytes());
                    if !(write_all(resp_write, &head) && write_all(resp_write, &body)) {
                        break;
                    }
                }
                // SAFETY: _exit ends the child without the parent's destructors.
                unsafe { libc::_exit(0) };
            }
            close(req_read);
            close(resp_write);
            Ok(Worker { pid, to_child: req_write, from_child: resp_read, limits })
        }

        pub fn alive(&self) -> bool {
            self.pid > 0
        }

        pub fn ask(&mut self, request: &[u8]) -> Result<Vec<u8>, String> {
            if !self.alive() {
                return Err("the decoder process has stopped".into());
            }
            match self.exchange(request) {
                Ok(answer) => answer,
                Err(failure) => Err(self.stop(failure)),
            }
        }

        /// The child's answer (which may be its own error), or why there was none.
        fn exchange(&mut self, request: &[u8]) -> Result<Result<Vec<u8>, String>, String> {
            let mut message = (request.len() as u32).to_le_bytes().to_vec();
            message.extend_from_slice(request);
            if !write_all(self.to_child, &message) {
                return Err("the decoder process stopped listening".into());
            }
            let deadline = Some(Instant::now() + Duration::from_secs(self.limits.seconds));
            let late = |e: String| {
                if e == "timeout" { format!("the decoder took longer than {} s", self.limits.seconds) } else { e }
            };
            let mut head = [0u8; 9];
            if !read_exact(self.from_child, &mut head, deadline).map_err(late)? {
                return Err("the decoder stopped without an answer".into());
            }
            let len = u64::from_le_bytes(head[1..].try_into().unwrap_or([0; 8]));
            if len > self.limits.output as u64 {
                return Err("the decoder's answer was too large".into());
            }
            let mut body = vec![0u8; len as usize];
            if !read_exact(self.from_child, &mut body, deadline).map_err(late)? && len > 0 {
                return Err("the decoder stopped without an answer".into());
            }
            Ok(match head[0] {
                0 => Ok(body),
                _ => Err(String::from_utf8_lossy(&body).into_owned()),
            })
        }

        /// Ends the child after a failure and says what happened to it.
        fn stop(&mut self, why: String) -> String {
            let signal = self.end();
            match signal {
                Some(s) if why.contains("without an answer") || why.contains("stopped") => {
                    format!("the decoder crashed (signal {s}) on this file")
                }
                _ => why,
            }
        }

        /// Kills and reaps the child, returning the signal that ended it, if any.
        fn end(&mut self) -> Option<i32> {
            if self.pid <= 0 {
                return None;
            }
            close(self.to_child);
            close(self.from_child);
            (self.to_child, self.from_child) = (-1, -1);
            let mut status = 0i32;
            // SAFETY: pid is our own child, not yet reaped.
            unsafe {
                // A child that has already died keeps its status; one still running is killed.
                if libc::waitpid(self.pid, &mut status, libc::WNOHANG) == 0 {
                    libc::kill(self.pid, libc::SIGKILL);
                    libc::waitpid(self.pid, &mut status, 0);
                }
            }
            self.pid = -1;
            libc::WIFSIGNALED(status).then(|| libc::WTERMSIG(status)).filter(|&s| s != libc::SIGKILL)
        }
    }

    impl Drop for Worker {
        fn drop(&mut self) {
            self.end();
        }
    }

    /// Bytes of heap and private data the process holds now, which a limit on
    /// the child's data segment must allow for since it inherits them (Linux).
    fn current_data_bytes() -> Option<u64> {
        let status = std::fs::read_to_string("/proc/self/status").ok()?;
        let line = status.lines().find(|l| l.starts_with("VmData:"))?;
        let kb: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
        Some(kb * 1024)
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
}

#[cfg(not(unix))]
mod imp {
    use super::Limits;

    pub struct Worker {
        serve: Box<dyn FnMut(&[u8]) -> Result<Vec<u8>, String> + Send>,
    }

    impl Worker {
        pub fn start(
            _limits: Limits,
            serve: impl FnMut(&[u8]) -> Result<Vec<u8>, String> + Send + 'static,
        ) -> Result<Worker, String> {
            Ok(Worker { serve: Box::new(serve) })
        }

        pub fn alive(&self) -> bool {
            true
        }

        pub fn ask(&mut self, request: &[u8]) -> Result<Vec<u8>, String> {
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| (self.serve)(request)))
                .unwrap_or_else(|_| Err("the decoder panicked".into()))
        }
    }

    pub fn run(_limits: Limits, work: impl FnOnce() -> Result<Vec<u8>, String>) -> Result<Vec<u8>, String> {
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(work))
            .unwrap_or_else(|_| Err("the decoder panicked".into()))
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::time::Duration;

    fn limits() -> Limits {
        Limits { seconds: 5, memory: 256 << 20, output: 1 << 20 }
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
        assert!(e.contains("panicked"), "{e}");
    }

    #[test]
    fn a_crash_is_survived() {
        let e = run(limits(), || {
            // SAFETY: deliberately kills the child process with a signal.
            unsafe { libc::abort() }
        })
        .unwrap_err();
        assert!(e.contains("crashed"), "{e}");
        // The parent is still here and can run again.
        assert!(run(limits(), || Ok(vec![1])).is_ok());
    }

    #[test]
    fn a_worker_keeps_its_state_and_survives_a_crash() {
        let mut count = 0u8;
        let mut w = Worker::start(limits(), move |req| {
            count += 1;
            match req {
                b"crash" => unsafe { libc::abort() },
                b"fail" => Err("no".into()),
                _ => Ok(vec![count]),
            }
        })
        .unwrap();
        assert_eq!(w.ask(b"a").unwrap(), [1]);
        assert_eq!(w.ask(b"b").unwrap(), [2]);
        assert_eq!(w.ask(b"fail").unwrap_err(), "no");
        assert!(w.alive());
        let e = w.ask(b"crash").unwrap_err();
        assert!(e.contains("crashed"), "{e}");
        assert!(!w.alive());
        assert!(w.ask(b"a").is_err());
    }

    #[test]
    fn a_hung_worker_is_cut_off() {
        let mut w = Worker::start(Limits { seconds: 1, ..limits() }, |_| {
            std::thread::sleep(Duration::from_secs(30));
            Ok(Vec::new())
        })
        .unwrap();
        let started = std::time::Instant::now();
        let e = w.ask(b"x").unwrap_err();
        assert!(e.contains("longer than"), "{e}");
        assert!(started.elapsed() < Duration::from_secs(10));
        assert!(!w.alive());
    }

    #[test]
    fn a_hang_is_cut_off() {
        let l = Limits { seconds: 1, ..limits() };
        let started = std::time::Instant::now();
        let e = run(l, || {
            std::thread::sleep(Duration::from_secs(30));
            Ok(Vec::new())
        })
        .unwrap_err();
        assert!(e.contains("longer than"), "{e}");
        assert!(started.elapsed() < Duration::from_secs(10));
    }

    #[test]
    fn too_much_output_is_cut_off() {
        let l = Limits { output: 1000, ..limits() };
        let e = run(l, || Ok(vec![7u8; 10_000_000])).unwrap_err();
        assert!(e.contains("too large"), "{e}");
    }

    /// Only Linux limits the child's memory: `RLIMIT_DATA` doesn't count
    /// the memory macOS's malloc maps, and macOS doesn't enforce `RLIMIT_AS`.
    #[test]
    #[cfg(target_os = "linux")]
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
}
