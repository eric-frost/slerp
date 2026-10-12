//! The wire format between client and helper.
//!
//! Every message is an 8-byte header followed by a payload:
//!
//! | Bytes | Field |
//! |---|---|
//! | 0-1 | magic `PT` |
//! | 2 | protocol version ([`PROTOCOL_VERSION`]) |
//! | 3 | message kind |
//! | 4-7 | payload length, little-endian `u32` |
//!
//! The header's layout is frozen across versions, so two builds that
//! disagree on everything else still understand each other's version byte.
//! A helper that gets a request with another version answers with a
//! [`kind::VERSION`] header carrying its own version and closes the
//! connection.  All integers are little-endian.
//!
//! A connection carries one request and one reply.  The payloads:
//!
//! - [`kind::THUMBNAIL`]: size `u32`, frames `u32`, milliseconds until the
//!   deadline `u32`, then the absolute path's bytes to the end.
//! - [`kind::STOP`]: empty; the helper answers [`kind::OK`] and exits.
//! - [`kind::PNG`]: the PNG bytes.
//! - [`kind::FRAMES`]: width `u32`, height `u32`, frame count `u32`, then the
//!   frames' RGBA bytes one after another.
//! - [`kind::FAILED`]: a UTF-8 message.
//! - [`kind::NO_THUMBNAIL`], [`kind::EXPIRED`], [`kind::BUSY`],
//!   [`kind::VERSION`], [`kind::OK`]: empty.

use crate::{MAX_FRAMES, MAX_SIZE, PROTOCOL_VERSION, Reply, Request};
use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub(crate) const MAGIC: [u8; 2] = *b"PT";
pub(crate) const HEADER_LEN: usize = 8;

/// Message kinds.  Requests are below 0x40, replies above.
pub(crate) mod kind {
    pub const THUMBNAIL: u8 = 0x01;
    pub const STOP: u8 = 0x02;
    pub const PNG: u8 = 0x41;
    pub const FRAMES: u8 = 0x42;
    pub const NO_THUMBNAIL: u8 = 0x43;
    pub const FAILED: u8 = 0x44;
    /// The deadline passed before the job finished.
    pub const EXPIRED: u8 = 0x45;
    /// The helper is serving as many connections as it allows.
    pub const BUSY: u8 = 0x46;
    /// The request's version isn't the helper's; the header carries the
    /// helper's version.
    pub const VERSION: u8 = 0x47;
    /// A stop request was accepted.
    pub const OK: u8 = 0x48;
}

/// The longest path a request may carry, Linux's `PATH_MAX`.
pub(crate) const MAX_PATH: usize = 4096;
/// The fixed fields before a thumbnail request's path.
const REQUEST_FIXED: usize = 12;
/// The largest request payload a helper reads.
pub(crate) const MAX_REQUEST: u32 = (REQUEST_FIXED + MAX_PATH) as u32;
/// The longest deadline a request may carry.  Thumbnailers wait seconds,
/// so anything longer is a bug or garbage.
pub(crate) const MAX_TIMEOUT_MS: u32 = 10 * 60 * 1000;
/// The largest PNG a reply may carry; a 8192 px RGBA PNG that doesn't
/// compress at all is 256 MiB, but thumbnails are a few hundred KiB.
pub(crate) const MAX_PNG: u32 = 64 << 20;
/// The largest total of turntable frame bytes a reply may carry.
pub(crate) const MAX_FRAME_BYTES: u64 = 256 << 20;
/// The fixed fields before a frames reply's pixels.
pub(crate) const FRAMES_FIXED: usize = 12;
/// The longest failure message a reply carries; longer ones are cut.
pub(crate) const MAX_MESSAGE: usize = 4096;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Header {
    pub version: u8,
    pub kind: u8,
    pub len: u32,
}

pub(crate) fn header(kind: u8, len: u32) -> [u8; HEADER_LEN] {
    header_with_version(PROTOCOL_VERSION, kind, len)
}

pub(crate) fn header_with_version(version: u8, kind: u8, len: u32) -> [u8; HEADER_LEN] {
    let l = len.to_le_bytes();
    [MAGIC[0], MAGIC[1], version, kind, l[0], l[1], l[2], l[3]]
}

/// Reads a header, or `None` when the magic is wrong (garbage, or something
/// that isn't Slerp on the other end).
pub(crate) fn parse_header(bytes: &[u8; HEADER_LEN]) -> Option<Header> {
    (bytes[..2] == MAGIC).then(|| Header {
        version: bytes[2],
        kind: bytes[3],
        len: u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]),
    })
}

/*┌─────────────────╮
| Requests        |
└─────────────────╯*/
//region Requests

/// Encodes a thumbnail request, header included.  `path` must already be
/// absolute.  The deadline travels as the time left at `now`, since the two
/// processes don't share `Instant`s.
pub(crate) fn encode_request(path: &Path, req: &Request, now: Instant) -> Result<Vec<u8>, &'static str> {
    let path = path.as_os_str().as_bytes();
    check_path(path)?;
    check_numbers(req.size, req.frames)?;
    let left = req.deadline.saturating_duration_since(now);
    let timeout_ms = u32::try_from(left.as_millis()).unwrap_or(u32::MAX).min(MAX_TIMEOUT_MS);
    if timeout_ms == 0 {
        return Err("the deadline has passed");
    }
    let len = REQUEST_FIXED + path.len();
    let mut out = Vec::with_capacity(HEADER_LEN + len);
    out.extend_from_slice(&header(kind::THUMBNAIL, len as u32));
    out.extend_from_slice(&req.size.to_le_bytes());
    out.extend_from_slice(&req.frames.to_le_bytes());
    out.extend_from_slice(&timeout_ms.to_le_bytes());
    out.extend_from_slice(path);
    Ok(out)
}

/// Decodes a thumbnail request's payload received at `received`.
pub(crate) fn decode_request(payload: &[u8], received: Instant) -> Result<Request, &'static str> {
    if payload.len() < REQUEST_FIXED {
        return Err("request too short");
    }
    let word = |i: usize| u32::from_le_bytes([payload[i], payload[i + 1], payload[i + 2], payload[i + 3]]);
    let (size, frames, timeout_ms) = (word(0), word(4), word(8));
    check_numbers(size, frames)?;
    if timeout_ms == 0 || timeout_ms > MAX_TIMEOUT_MS {
        return Err("deadline out of range");
    }
    let path = &payload[REQUEST_FIXED..];
    check_path(path)?;
    Ok(Request {
        path: PathBuf::from(OsStr::from_bytes(path)),
        size,
        frames,
        deadline: received + Duration::from_millis(u64::from(timeout_ms)),
    })
}

fn check_path(path: &[u8]) -> Result<(), &'static str> {
    if path.first() != Some(&b'/') {
        return Err("path not absolute");
    }
    if path.len() > MAX_PATH {
        return Err("path too long");
    }
    if path.contains(&0) {
        return Err("path contains NUL");
    }
    Ok(())
}

fn check_numbers(size: u32, frames: u32) -> Result<(), &'static str> {
    if size == 0 || size > MAX_SIZE {
        return Err("size out of range");
    }
    if frames > MAX_FRAMES {
        return Err("too many frames");
    }
    Ok(())
}

//endregion

/*┌─────────────────╮
| Replies         |
└─────────────────╯*/
//region Replies

/// A reply ready to send: a header with any fixed fields, then body slices
/// sent one after another, so turntable frames aren't copied into one
/// buffer first.
pub(crate) struct Encoded<'a> {
    pub head: Vec<u8>,
    pub body: Vec<&'a [u8]>,
}

/// Encodes a handler's reply.  A reply that breaks the protocol's limits
/// (a huge PNG, frames of the wrong size) becomes a `FAILED` reply, so a
/// handler bug can't produce something the client would reject as garbage.
pub(crate) fn encode_reply(reply: &Reply) -> Encoded<'_> {
    match reply {
        Reply::Png(png) if png.len() <= MAX_PNG as usize => {
            Encoded { head: header(kind::PNG, png.len() as u32).to_vec(), body: vec![png] }
        }
        Reply::Png(_) => failed("the thumbnail PNG is too large"),
        Reply::Frames { width, height, rgba } => match frames_len(*width, *height, rgba.len() as u64) {
            Some((frame, total)) if rgba.iter().all(|f| f.len() as u64 == frame) => {
                let mut head = header(kind::FRAMES, (FRAMES_FIXED as u64 + total) as u32).to_vec();
                head.extend_from_slice(&width.to_le_bytes());
                head.extend_from_slice(&height.to_le_bytes());
                head.extend_from_slice(&(rgba.len() as u32).to_le_bytes());
                Encoded { head, body: rgba.iter().map(Vec::as_slice).collect() }
            }
            _ => failed("the handler returned malformed frames"),
        },
        Reply::NoThumbnail => Encoded { head: header(kind::NO_THUMBNAIL, 0).to_vec(), body: Vec::new() },
        Reply::Failed(message) => failed(message),
    }
}

fn failed(message: &str) -> Encoded<'static> {
    let mut end = message.len().min(MAX_MESSAGE);
    while !message.is_char_boundary(end) {
        end -= 1;
    }
    let mut head = header(kind::FAILED, end as u32).to_vec();
    head.extend_from_slice(&message.as_bytes()[..end]);
    Encoded { head, body: Vec::new() }
}

/// The bytes per frame and in total for `count` frames of `width` by
/// `height`, or `None` when the dimensions or total break the limits.
pub(crate) fn frames_len(width: u32, height: u32, count: u64) -> Option<(u64, u64)> {
    if !(1..=MAX_SIZE).contains(&width) || !(1..=MAX_SIZE).contains(&height) {
        return None;
    }
    if !(1..=u64::from(MAX_FRAMES)).contains(&count) {
        return None;
    }
    let frame = u64::from(width) * u64::from(height) * 4;
    let total = frame.checked_mul(count)?;
    (total <= MAX_FRAME_BYTES).then_some((frame, total))
}

//endregion

#[cfg(test)]
mod tests {
    use super::*;

    fn req(path: &str) -> Request {
        Request { path: path.into(), size: 256, frames: 3, deadline: Instant::now() + Duration::from_secs(2) }
    }

    fn payload(message: &[u8]) -> &[u8] {
        &message[HEADER_LEN..]
    }

    #[test]
    fn request_round_trip() {
        let now = Instant::now();
        let r = req("/home/user/model with spaces.glb");
        let bytes = encode_request(&r.path, &r, now).unwrap();
        let h = parse_header(bytes[..HEADER_LEN].try_into().unwrap()).unwrap();
        assert_eq!(h, Header { version: PROTOCOL_VERSION, kind: kind::THUMBNAIL, len: (bytes.len() - 8) as u32 });
        let back = decode_request(payload(&bytes), now).unwrap();
        assert_eq!(back.path, r.path);
        assert_eq!((back.size, back.frames), (256, 3));
        let left = back.deadline - now;
        assert!(left <= Duration::from_secs(2) && left >= Duration::from_millis(1990), "{left:?}");
    }

    #[test]
    fn non_utf8_paths_survive() {
        let path = PathBuf::from(OsStr::from_bytes(b"/tmp/\xff\xfe.glb"));
        let r = Request { path: path.clone(), ..req("/x") };
        let bytes = encode_request(&path, &r, Instant::now()).unwrap();
        assert_eq!(decode_request(payload(&bytes), Instant::now()).unwrap().path, path);
    }

    #[test]
    fn bad_requests_are_refused() {
        let now = Instant::now();
        let ok = req("/a.glb");
        assert!(encode_request(Path::new("relative.glb"), &ok, now).is_err());
        assert!(encode_request(Path::new("/a\0b"), &ok, now).is_err());
        assert!(encode_request(Path::new(&format!("/{}", "a".repeat(MAX_PATH))), &ok, now).is_err());
        assert!(encode_request(&ok.path, &Request { size: 0, ..ok.clone() }, now).is_err());
        assert!(encode_request(&ok.path, &Request { size: MAX_SIZE + 1, ..ok.clone() }, now).is_err());
        assert!(encode_request(&ok.path, &Request { frames: MAX_FRAMES + 1, ..ok.clone() }, now).is_err());
        assert!(encode_request(&ok.path, &Request { deadline: now, ..ok.clone() }, now).is_err());

        // Every truncation of a good payload is refused or decodes to a
        // shorter absolute path, and nothing panics.
        let good = encode_request(&ok.path, &ok, now).unwrap();
        for end in 0..payload(&good).len() {
            if let Ok(r) = decode_request(&payload(&good)[..end], now) {
                assert!(r.path.is_absolute());
            }
        }
        // Zero deadline, huge deadline.
        let mut p = payload(&good).to_vec();
        p[8..12].copy_from_slice(&0u32.to_le_bytes());
        assert!(decode_request(&p, now).is_err());
        p[8..12].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(decode_request(&p, now).is_err());
    }

    #[test]
    fn deadlines_far_away_are_capped() {
        let now = Instant::now();
        let r = Request { deadline: now + Duration::from_secs(86_400), ..req("/a") };
        let bytes = encode_request(&r.path, &r, now).unwrap();
        let back = decode_request(payload(&bytes), now).unwrap();
        assert_eq!(back.deadline - now, Duration::from_millis(u64::from(MAX_TIMEOUT_MS)));
    }

    #[test]
    fn garbage_headers() {
        assert_eq!(parse_header(b"GET / HT"), None);
        assert_eq!(parse_header(&[0; 8]), None);
        let h = parse_header(&header_with_version(9, kind::STOP, 5)).unwrap();
        assert_eq!((h.version, h.kind, h.len), (9, kind::STOP, 5));
    }

    #[test]
    fn replies_that_break_limits_become_failures() {
        let frames = Reply::Frames { width: 2, height: 2, rgba: vec![vec![0; 16], vec![0; 15]] };
        assert_eq!(encode_reply(&frames).head[3], kind::FAILED);
        let none = Reply::Frames { width: 2, height: 2, rgba: vec![] };
        assert_eq!(encode_reply(&none).head[3], kind::FAILED);
        let huge = Reply::Frames { width: MAX_SIZE, height: MAX_SIZE, rgba: vec![vec![]; 2] };
        assert_eq!(encode_reply(&huge).head[3], kind::FAILED);

        let good = Reply::Frames { width: 2, height: 1, rgba: vec![vec![1; 8], vec![2; 8]] };
        let e = encode_reply(&good);
        assert_eq!(e.head[3], kind::FRAMES);
        assert_eq!(e.head.len(), HEADER_LEN + FRAMES_FIXED);
        assert_eq!(u32::from_le_bytes(e.head[4..8].try_into().unwrap()), 12 + 16);
        assert_eq!(e.body.len(), 2);
    }

    #[test]
    fn long_failure_messages_are_cut_at_a_character() {
        let message = "é".repeat(MAX_MESSAGE);
        let reply = Reply::Failed(message);
        let e = encode_reply(&reply);
        let text = std::str::from_utf8(&e.head[HEADER_LEN..]).unwrap();
        assert!(text.len() <= MAX_MESSAGE && text.len() >= MAX_MESSAGE - 1);
    }
}
