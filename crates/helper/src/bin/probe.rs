//! A one-shot client for tests that need the client in another process: in
//! GNOME's thumbnail sandbox, as another user, or timing a whole
//! thumbnailer process.
//!
//! ```text
//! probe [--socket PATH] [--deadline-ms N] [--size N] [--raw | --noop] FILE
//! ```
//!
//! It prints one line, `outcome=<result> start_called=<bool>
//! request_us=<time in request>`, and never starts a helper: its `start`
//! only records that it was asked to.  Without `--socket` it uses the real
//! [`socket_path`](slerp_helper::socket_path), as `slerp thumbnail`
//! would.  `--raw` skips the client library and sends a hand-made request,
//! printing `received=<bytes>`, to test the helper's own checks.  `--noop`
//! exits at once, to time process start alone.

use slerp_helper::{Reply, Request, request, request_at};
use std::cell::Cell;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::time::{Duration, Instant};

fn main() {
    let (mut socket, mut deadline_ms, mut size, mut raw) = (None::<PathBuf>, 1500, 256, false);
    let mut file = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--socket" => socket = args.next().map(Into::into),
            "--deadline-ms" => deadline_ms = args.next().and_then(|v| v.parse().ok()).unwrap_or(deadline_ms),
            "--size" => size = args.next().and_then(|v| v.parse().ok()).unwrap_or(size),
            "--raw" => raw = true,
            "--noop" => return,
            _ => file = Some(PathBuf::from(arg)),
        }
    }
    let file = file.unwrap_or_else(|| PathBuf::from("/probe/png"));

    // A C or C++ host doesn't ignore SIGPIPE, so the library mode doesn't
    // either; the raw mode reports a closed socket rather than dying of it.
    // SAFETY: called before any other thread exists.
    unsafe { libc::signal(libc::SIGPIPE, if raw { libc::SIG_IGN } else { libc::SIG_DFL }) };

    if raw {
        let Some(socket) = socket else { return println!("received=error no socket") };
        return println!("received={}", raw_request(&socket, &file, size));
    }

    let req = Request { path: file, size, frames: 0, deadline: Instant::now() + Duration::from_millis(deadline_ms) };
    let started = Cell::new(false);
    let start = || {
        started.set(true);
        Ok(())
    };
    let t = Instant::now();
    let result = match &socket {
        Some(socket) => request_at(socket, &req, start),
        None => request(&req, start),
    };
    let us = t.elapsed().as_micros();
    let outcome = match result {
        Ok(Reply::Png(png)) => format!("Png({})", png.len()),
        Ok(reply) => format!("{reply:?}"),
        Err(err) => format!("{err:?}"),
    };
    println!("outcome={outcome} start_called={} request_us={us}", started.get());
}

/// Sends a valid version 1 request by hand and counts the reply's bytes,
/// however the connection ends.
fn raw_request(socket: &std::path::Path, file: &std::path::Path, size: u32) -> String {
    use std::os::unix::ffi::OsStrExt;
    let path = file.as_os_str().as_bytes();
    let mut message = b"PT\x01\x01".to_vec();
    message.extend_from_slice(&(12 + path.len() as u32).to_le_bytes());
    message.extend_from_slice(&size.to_le_bytes());
    message.extend_from_slice(&0u32.to_le_bytes());
    message.extend_from_slice(&2000u32.to_le_bytes());
    message.extend_from_slice(path);
    let mut stream = match std::os::unix::net::UnixStream::connect(socket) {
        Ok(stream) => stream,
        Err(err) => return format!("error connect: {err}"),
    };
    let _ = stream.set_read_timeout(Some(Duration::from_secs(3)));
    // A helper that refuses the connection may close it before the request
    // is written (a broken pipe) or with it unread (a reset); either way
    // what matters is that no reply arrives.
    let _ = stream.write_all(&message);
    let mut reply = Vec::new();
    let _ = stream.read_to_end(&mut reply);
    reply.len().to_string()
}
