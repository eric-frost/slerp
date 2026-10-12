//! The few system calls std doesn't offer on Rust 1.93: peer credentials,
//! a connect that can't block forever, sends that can't raise SIGPIPE, and
//! poll.  Everything here works on raw sockets and keeps its `unsafe` small.
//!
//! macOS lacks three Linux conveniences: `SO_PEERCRED` (it has
//! `getpeereid`), `MSG_NOSIGNAL` (it has the `SO_NOSIGPIPE` socket option)
//! and `SOCK_CLOEXEC` (`fcntl` sets the flag instead).  BSD kernels also
//! hand accepted sockets the listener's non-blocking mode, which Linux
//! doesn't, so [`prepare_accepted`] undoes it, and macOS refuses to set a
//! timeout on a connection the peer has closed.

use std::io::{self, ErrorKind, Read};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::{Duration, Instant};

pub(crate) fn euid() -> u32 {
    // SAFETY: geteuid has no preconditions and can't fail.
    unsafe { libc::geteuid() }
}

/// The uid of the process at the other end of a Unix socket, from
/// `SO_PEERCRED`.  For a connecting client that is the helper's uid when it
/// called `listen`; for the helper, the client's uid when it connected.
#[cfg(target_os = "linux")]
pub(crate) fn peer_uid(socket: &impl AsRawFd) -> io::Result<u32> {
    let mut cred = libc::ucred { pid: 0, uid: 0, gid: 0 };
    let mut len = size_of::<libc::ucred>() as libc::socklen_t;
    // SAFETY: cred and len are valid for writes of the sizes passed.
    let rc = unsafe {
        libc::getsockopt(socket.as_raw_fd(), libc::SOL_SOCKET, libc::SO_PEERCRED, (&raw mut cred).cast(), &raw mut len)
    };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    if len as usize != size_of::<libc::ucred>() {
        return Err(io::Error::other("short SO_PEERCRED answer"));
    }
    Ok(cred.uid)
}

/// The uid of the process at the other end of a Unix socket, from
/// `getpeereid`, which BSD kernels (macOS included) answer from the
/// credentials saved at `connect` and `listen`, as Linux does.
#[cfg(not(target_os = "linux"))]
pub(crate) fn peer_uid(socket: &impl AsRawFd) -> io::Result<u32> {
    let (mut uid, mut gid) = (0, 0);
    // SAFETY: uid and gid are valid for writes.
    if unsafe { libc::getpeereid(socket.as_raw_fd(), &raw mut uid, &raw mut gid) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(uid)
}

/// Makes a socket return `EPIPE` rather than raise SIGPIPE when written
/// after the peer has gone, where there is no per-send `MSG_NOSIGNAL`.
#[cfg(target_vendor = "apple")]
fn no_sigpipe(socket: &impl AsRawFd) -> io::Result<()> {
    let on: libc::c_int = 1;
    // SAFETY: on is valid for reads of its size.
    let rc = unsafe {
        libc::setsockopt(
            socket.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_NOSIGPIPE,
            (&raw const on).cast(),
            size_of::<libc::c_int>() as libc::socklen_t,
        )
    };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Readies a socket the helper accepted: blocking, as Linux hands them out
/// (BSD kernels copy the listener's non-blocking flag, which would make
/// every timed read fail at once), and without SIGPIPE on macOS.
#[cfg(not(target_os = "linux"))]
pub(crate) fn prepare_accepted(socket: &UnixStream) -> io::Result<()> {
    socket.set_nonblocking(false)?;
    #[cfg(target_vendor = "apple")]
    no_sigpipe(socket)?;
    Ok(())
}

/// Readies a socket the helper accepted.  On Linux there is nothing to do:
/// accepted sockets block, and sends pass `MSG_NOSIGNAL`.
#[cfg(target_os = "linux")]
pub(crate) fn prepare_accepted(_socket: &UnixStream) -> io::Result<()> {
    Ok(())
}

/// Connects to a Unix socket, waiting at most `timeout` for room in the
/// listener's queue.  std's `UnixStream::connect` would wait forever when a
/// stopped or stuck helper's queue is full, and a thumbnailer must never
/// hang.  A full queue gives `WouldBlock`.
pub(crate) fn connect(path: &Path, timeout: Duration) -> io::Result<UnixStream> {
    let bytes = path.as_os_str().as_bytes();
    // SAFETY: sockaddr_un is plain data, for which all zeroes is valid.
    let mut addr: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    if bytes.len() >= addr.sun_path.len() || bytes.contains(&0) {
        return Err(io::Error::new(ErrorKind::InvalidInput, "socket path too long or contains NUL"));
    }
    addr.sun_family = libc::AF_UNIX as libc::sa_family_t;
    for (dst, &src) in addr.sun_path.iter_mut().zip(bytes) {
        *dst = src as libc::c_char;
    }
    let addr_len = (std::mem::offset_of!(libc::sockaddr_un, sun_path) + bytes.len() + 1) as libc::socklen_t;

    // SAFETY: socket has no memory preconditions; a non-negative result is
    // a new descriptor that nothing else owns.
    #[cfg(not(target_vendor = "apple"))]
    let fd = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_STREAM | libc::SOCK_CLOEXEC, 0) };
    #[cfg(target_vendor = "apple")]
    let fd = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_STREAM, 0) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: fd is a fresh descriptor owned by nothing else.
    let socket = UnixStream::from(unsafe { OwnedFd::from_raw_fd(fd) });
    #[cfg(target_vendor = "apple")]
    {
        // SAFETY: fd is open; F_SETFD takes an int argument.
        if unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) } != 0 {
            return Err(io::Error::last_os_error());
        }
        no_sigpipe(&socket)?;
    }
    // Linux bounds a blocking Unix connect by the send timeout.
    socket.set_write_timeout(Some(timeout.max(Duration::from_millis(1))))?;
    // SAFETY: addr is a valid sockaddr_un and addr_len doesn't exceed it.
    let rc = unsafe { libc::connect(socket.as_raw_fd(), (&raw const addr).cast(), addr_len) };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(socket)
}

/// Sends all of `buf` by `deadline`.  It uses `MSG_NOSIGNAL`, so a peer that
/// has gone gives `BrokenPipe` rather than SIGPIPE, which would kill a host
/// process that doesn't ignore it (a Qt plugin inside Dolphin, say).  On
/// macOS the sockets carry `SO_NOSIGPIPE` instead.
pub(crate) fn send_all(socket: &UnixStream, mut buf: &[u8], deadline: Instant) -> io::Result<()> {
    #[cfg(not(target_vendor = "apple"))]
    const FLAGS: libc::c_int = libc::MSG_NOSIGNAL;
    #[cfg(target_vendor = "apple")]
    const FLAGS: libc::c_int = 0;
    while !buf.is_empty() {
        timeout_set(socket.set_write_timeout(Some(time_left(deadline)?)))?;
        // SAFETY: buf is valid for reads of buf.len() bytes.
        let n = unsafe { libc::send(socket.as_raw_fd(), buf.as_ptr().cast(), buf.len(), FLAGS) };
        if n < 0 {
            match io::Error::last_os_error() {
                e if e.kind() == ErrorKind::Interrupted => continue,
                e if e.kind() == ErrorKind::WouldBlock => return Err(ErrorKind::TimedOut.into()),
                e => return Err(e),
            }
        }
        if n == 0 {
            return Err(ErrorKind::WriteZero.into());
        }
        buf = &buf[n as usize..];
    }
    Ok(())
}

/// Fills `buf` by `deadline`.  The peer closing early gives
/// `UnexpectedEof`; the deadline passing gives `TimedOut`.
pub(crate) fn read_exact_by(socket: &UnixStream, mut buf: &mut [u8], deadline: Instant) -> io::Result<()> {
    while !buf.is_empty() {
        timeout_set(socket.set_read_timeout(Some(time_left(deadline)?)))?;
        match (&*socket).read(buf) {
            Ok(0) => return Err(ErrorKind::UnexpectedEof.into()),
            Ok(n) => buf = &mut buf[n..],
            Err(e) if e.kind() == ErrorKind::Interrupted => {}
            Err(e) if e.kind() == ErrorKind::WouldBlock => return Err(ErrorKind::TimedOut.into()),
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

/// Reads `len` bytes by `deadline` into a new buffer that grows as data
/// arrives, so a header that lies about its length can't make the reader
/// allocate much more than was actually sent.
pub(crate) fn read_vec_by(socket: &UnixStream, len: usize, deadline: Instant) -> io::Result<Vec<u8>> {
    const STEP: usize = 1 << 20;
    let mut out = Vec::with_capacity(len.min(STEP));
    while out.len() < len {
        let start = out.len();
        out.resize(start + (len - start).min(STEP), 0);
        read_exact_by(socket, &mut out[start..], deadline)?;
    }
    Ok(out)
}

/// The result of setting a socket's timeout.  macOS refuses (`EINVAL`) once
/// the other end has shut the connection down; reads and writes then end
/// at once anyway (buffered data, then end of file, or `EPIPE`), so the
/// refusal is no reason to fail.
fn timeout_set(result: io::Result<()>) -> io::Result<()> {
    #[cfg(target_vendor = "apple")]
    if let Err(err) = &result
        && err.raw_os_error() == Some(libc::EINVAL)
    {
        return Ok(());
    }
    result
}

/// The time until `deadline`, or `TimedOut` when it has passed.
pub(crate) fn time_left(deadline: Instant) -> io::Result<Duration> {
    deadline.checked_duration_since(Instant::now()).filter(|d| !d.is_zero()).ok_or_else(|| ErrorKind::TimedOut.into())
}

/// Waits until any of `fds` is readable, or `timeout` passes (`None` waits
/// forever), and says which are.  A signal interrupting the wait counts as
/// a timeout.  The descriptors must differ: macOS's `poll` reports a
/// descriptor listed twice only in its last slot.
pub(crate) fn wait_readable<const N: usize>(fds: [RawFd; N], timeout: Option<Duration>) -> io::Result<[bool; N]> {
    let mut polls = fds.map(|fd| libc::pollfd { fd, events: libc::POLLIN, revents: 0 });
    // Round up, so a wait for 0.4 ms doesn't become a busy loop of 0 ms waits.
    let ms = timeout.map_or(-1, |t| t.as_micros().div_ceil(1000).min(i32::MAX as u128) as i32);
    // SAFETY: polls is a valid array of N pollfd structs.
    let rc = unsafe { libc::poll(polls.as_mut_ptr(), N as libc::nfds_t, ms) };
    if rc < 0 {
        let err = io::Error::last_os_error();
        return if err.kind() == ErrorKind::Interrupted { Ok([false; N]) } else { Err(err) };
    }
    Ok(polls.map(|p| p.revents != 0))
}

/// Whether this user may write into and search `dir`.  A client checks this
/// before starting a helper that would only fail to create its socket.
pub(crate) fn can_write_dir(dir: &Path) -> bool {
    let Ok(c) = std::ffi::CString::new(dir.as_os_str().as_bytes()) else { return false };
    // SAFETY: c is a valid NUL-terminated string.
    unsafe { libc::access(c.as_ptr(), libc::W_OK | libc::X_OK) == 0 }
}

/// Ends the process at once, without running destructors or `atexit`
/// handlers, which could block forever in a hung GPU driver.
pub(crate) fn exit_now(code: i32) -> ! {
    // SAFETY: _exit has no preconditions.
    unsafe { libc::_exit(code) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// Each slot reports its own descriptor, also a single one (the hand-off
    /// listener's wait).
    #[test]
    fn waits_report_each_readable_descriptor() {
        let (mut a_in, a_out) = UnixStream::pair().unwrap();
        let (_b_in, b_out) = UnixStream::pair().unwrap();
        let short = Some(Duration::from_millis(10));
        assert_eq!(wait_readable([a_out.as_raw_fd()], short).unwrap(), [false]);
        a_in.write_all(b"x").unwrap();
        assert_eq!(wait_readable([a_out.as_raw_fd()], short).unwrap(), [true]);
        assert_eq!(wait_readable([b_out.as_raw_fd(), a_out.as_raw_fd()], short).unwrap(), [false, true]);
        assert_eq!(wait_readable([a_out.as_raw_fd(), b_out.as_raw_fd()], short).unwrap(), [true, false]);
    }
}
