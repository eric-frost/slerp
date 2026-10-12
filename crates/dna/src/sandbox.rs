//! Running openriglogic's reader where a fault can't take Slerp down.
//!
//! The reader is C++ that trusts much of what a DNA file says: it bounds
//! container sizes by the file's length, but some indices into its own
//! tables are only checked by asserts.  A damaged file could make it read
//! out of bounds.  Thumbnails read files the user merely browses past,
//! inside the helper that serves every file manager window, so on Unix the
//! read runs in a forked child process that sends back plain data through
//! a pipe.  A crash, a hang or a runaway allocation then costs one
//! thumbnail.  Windows has no fork; there the read runs in the calling
//! process, which for thumbnails is the helper rather than Explorer.

use std::time::Duration;

/// Runs `f` in a child process and returns the bytes it produced.  The
/// child gets `timeout` before it is killed, and may answer with at most
/// `max_bytes`.
#[cfg(unix)]
pub(crate) fn isolated(
    f: impl FnOnce() -> Result<Vec<u8>, String>,
    timeout: Duration,
    max_bytes: usize,
) -> Result<Vec<u8>, String> {
    use std::time::Instant;

    // 1. A pipe for the answer, closed on exec (nothing execs, but other
    //    threads of the caller might).
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

    // SAFETY: the child only runs `f` (openriglogic and plain Rust that
    // allocates, both of which glibc and macOS make safe after fork),
    // writes to the pipe and leaves with _exit, never returning into the
    // caller's code or running its destructors.  It doesn't touch locks
    // other threads might have held at the fork, such as stdout's.
    let pid = unsafe { libc::fork() };
    if pid < 0 {
        // SAFETY: closing descriptors this function owns.
        unsafe {
            libc::close(read_fd);
            libc::close(write_fd);
        }
        return Err(format!("couldn't start a reader process: {}", std::io::Error::last_os_error()));
    }
    if pid == 0 {
        // 2. The child.
        // SAFETY: closing the end the child doesn't use.
        unsafe { libc::close(read_fd) };
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f))
            .unwrap_or_else(|_| Err("the DNA reader panicked".to_string()));
        let (tag, body) = match result {
            Ok(bytes) => (0u8, bytes),
            Err(message) => (1u8, message.into_bytes()),
        };
        let ok = write_all(write_fd, &[tag]) && write_all(write_fd, &body);
        // SAFETY: _exit ends the child without running the parent's
        // destructors or atexit handlers.
        unsafe { libc::_exit(if ok { 0 } else { 2 }) };
    }

    // 3. The parent reads until the child closes the pipe, it runs out of
    //    time, or it sends too much.
    // SAFETY: closing the end the parent doesn't use.
    unsafe { libc::close(write_fd) };
    let deadline = Instant::now() + timeout;
    let mut out = Vec::new();
    let mut buf = vec![0u8; 1 << 16];
    let failure = loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            break Some(format!("the DNA reader took longer than {} s", timeout.as_secs()));
        }
        let mut pfd = libc::pollfd { fd: read_fd, events: libc::POLLIN, revents: 0 };
        let ms = left.as_millis().min(i32::MAX as u128) as i32;
        // SAFETY: pfd is one valid pollfd.
        let ready = unsafe { libc::poll(&mut pfd, 1, ms) };
        if ready < 0 {
            if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            break Some(format!("couldn't wait for the DNA reader: {}", std::io::Error::last_os_error()));
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
            break Some(format!("couldn't read the DNA reader's answer: {}", std::io::Error::last_os_error()));
        }
        if n == 0 {
            break None;
        }
        if out.len() + n as usize > max_bytes.saturating_add(1) {
            break Some("the DNA reader's answer was too large".to_string());
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
        return Err(format!("the DNA reader crashed (signal {}) on this file", libc::WTERMSIG(status)));
    }
    match out.split_first() {
        Some((0, body)) => Ok(body.to_vec()),
        Some((1, message)) => Err(String::from_utf8_lossy(message).into_owned()),
        _ => Err("the DNA reader stopped without an answer".into()),
    }
}

/// Without fork, the reader runs here.
#[cfg(not(unix))]
pub(crate) fn isolated(
    f: impl FnOnce() -> Result<Vec<u8>, String>,
    _timeout: Duration,
    _max_bytes: usize,
) -> Result<Vec<u8>, String> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).unwrap_or_else(|_| Err("the DNA reader panicked".into()))
}

#[cfg(unix)]
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

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn answers_come_back() {
        let out = isolated(|| Ok(vec![7u8; 300_000]), Duration::from_secs(10), 1 << 20).unwrap();
        assert_eq!(out.len(), 300_000);
        assert!(out.iter().all(|&b| b == 7));
    }

    #[test]
    fn errors_come_back() {
        let err = isolated(|| Err("no".into()), Duration::from_secs(10), 1 << 20).unwrap_err();
        assert_eq!(err, "no");
    }

    #[test]
    fn crashes_and_hangs_are_contained() {
        let err = isolated(
            || {
                // SAFETY: deliberately ends the child the way a fault would.
                unsafe { libc::abort() }
            },
            Duration::from_secs(10),
            1 << 20,
        )
        .unwrap_err();
        assert!(err.contains("crashed"), "{err}");
        let start = std::time::Instant::now();
        let err = isolated(
            || {
                std::thread::sleep(Duration::from_secs(30));
                Ok(Vec::new())
            },
            Duration::from_millis(300),
            1 << 20,
        )
        .unwrap_err();
        assert!(err.contains("longer"), "{err}");
        assert!(start.elapsed() < Duration::from_secs(5));
        let err = isolated(|| Ok(vec![0u8; 4 << 20]), Duration::from_secs(10), 1 << 20).unwrap_err();
        assert!(err.contains("too large"), "{err}");
    }
}
