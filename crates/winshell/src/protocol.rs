//! The messages between the thumbnail handler inside Explorer and the
//! thumbnail helper process that does the work, over a named pipe.
//!
//! One connection carries one request and its reply.  A request is the
//! magic `PTS1`, the size in pixels and the file's path; a reply is a
//! status byte followed by an RGBA picture or an error message.  All
//! numbers are little-endian `u32`.  Both sides check every length, since
//! neither trusts the other to be well-formed.

use std::io::{self, Read, Write};

const MAGIC: &[u8; 4] = b"PTS1";

/// Paths longer than this aren't paths (Windows' own limit is 32767
/// UTF-16 units).
const MAX_PATH_BYTES: u32 = 32767 * 3;

/// The largest picture a reply may carry, per side.  Thumbnails are at most
/// 2048 pixels; this leaves room without letting a broken reply allocate
/// gigabytes.
pub const MAX_SIDE: u32 = 4096;

/// Error messages are cut to this length.
const MAX_MESSAGE_BYTES: u32 = 4096;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Request {
    pub size: u32,
    pub path: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reply {
    /// A thumbnail, `width * height` RGBA pixels with straight alpha.
    Image { width: u32, height: u32, rgba: Vec<u8> },
    /// Not a format Slerp reads.
    Unknown,
    /// Reading or drawing the file failed.
    Failed(String),
}

pub fn write_request(w: &mut impl Write, req: &Request) -> io::Result<()> {
    let path = req.path.as_bytes();
    let len =
        u32::try_from(path.len()).ok().filter(|&n| n <= MAX_PATH_BYTES).ok_or_else(|| invalid("path too long"))?;
    let mut buf = Vec::with_capacity(12 + path.len());
    buf.extend_from_slice(MAGIC);
    buf.extend_from_slice(&req.size.to_le_bytes());
    buf.extend_from_slice(&len.to_le_bytes());
    buf.extend_from_slice(path);
    w.write_all(&buf)
}

pub fn read_request(r: &mut impl Read) -> io::Result<Request> {
    let mut magic = [0u8; 4];
    r.read_exact(&mut magic)?;
    if &magic != MAGIC {
        return Err(invalid("not a Slerp request"));
    }
    let size = read_u32(r)?;
    let len = read_u32(r)?;
    if len > MAX_PATH_BYTES {
        return Err(invalid("path too long"));
    }
    let mut path = vec![0u8; len as usize];
    r.read_exact(&mut path)?;
    let path = String::from_utf8(path).map_err(|_| invalid("path is not UTF-8"))?;
    Ok(Request { size, path })
}

pub fn write_reply(w: &mut impl Write, reply: &Reply) -> io::Result<()> {
    let mut buf = Vec::new();
    match reply {
        Reply::Image { width, height, rgba } => {
            if *width > MAX_SIDE || *height > MAX_SIDE || rgba.len() != *width as usize * *height as usize * 4 {
                return Err(invalid("bad picture"));
            }
            buf.reserve(9 + rgba.len());
            buf.push(0);
            buf.extend_from_slice(&width.to_le_bytes());
            buf.extend_from_slice(&height.to_le_bytes());
            buf.extend_from_slice(rgba);
        }
        Reply::Unknown => buf.push(1),
        Reply::Failed(message) => {
            let mut end = message.len().min(MAX_MESSAGE_BYTES as usize);
            while !message.is_char_boundary(end) {
                end -= 1;
            }
            buf.push(2);
            buf.extend_from_slice(&(end as u32).to_le_bytes());
            buf.extend_from_slice(&message.as_bytes()[..end]);
        }
    }
    w.write_all(&buf)
}

pub fn read_reply(r: &mut impl Read) -> io::Result<Reply> {
    let mut status = [0u8; 1];
    r.read_exact(&mut status)?;
    match status[0] {
        0 => {
            let (width, height) = (read_u32(r)?, read_u32(r)?);
            if width == 0 || height == 0 || width > MAX_SIDE || height > MAX_SIDE {
                return Err(invalid("bad picture size"));
            }
            let mut rgba = vec![0u8; width as usize * height as usize * 4];
            r.read_exact(&mut rgba)?;
            Ok(Reply::Image { width, height, rgba })
        }
        1 => Ok(Reply::Unknown),
        2 => {
            let len = read_u32(r)?;
            if len > MAX_MESSAGE_BYTES {
                return Err(invalid("message too long"));
            }
            let mut text = vec![0u8; len as usize];
            r.read_exact(&mut text)?;
            Ok(Reply::Failed(String::from_utf8_lossy(&text).into_owned()))
        }
        _ => Err(invalid("unknown reply")),
    }
}

fn read_u32(r: &mut impl Read) -> io::Result<u32> {
    let mut b = [0u8; 4];
    r.read_exact(&mut b)?;
    Ok(u32::from_le_bytes(b))
}

fn invalid(what: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, what)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip_reply(reply: Reply) -> Reply {
        let mut buf = Vec::new();
        write_reply(&mut buf, &reply).unwrap();
        let mut cursor = &buf[..];
        let back = read_reply(&mut cursor).unwrap();
        assert!(cursor.is_empty());
        back
    }

    #[test]
    fn requests_and_replies_round_trip() {
        let req = Request { size: 256, path: r"C:\Models\Ünïcode\helmet.gltf".into() };
        let mut buf = Vec::new();
        write_request(&mut buf, &req).unwrap();
        assert_eq!(read_request(&mut &buf[..]).unwrap(), req);

        let image = Reply::Image { width: 2, height: 1, rgba: vec![1, 2, 3, 4, 5, 6, 7, 8] };
        assert_eq!(round_trip_reply(image.clone()), image);
        assert_eq!(round_trip_reply(Reply::Unknown), Reply::Unknown);
        assert_eq!(round_trip_reply(Reply::Failed("no GPU".into())), Reply::Failed("no GPU".into()));
    }

    #[test]
    fn long_messages_are_cut_at_a_character_boundary() {
        let long = "é".repeat(5000);
        let Reply::Failed(back) = round_trip_reply(Reply::Failed(long)) else { panic!() };
        assert!(back.len() <= MAX_MESSAGE_BYTES as usize && back.chars().all(|c| c == 'é'));
    }

    #[test]
    fn malformed_messages_are_errors_not_panics_or_huge_allocations() {
        // Wrong magic, oversized path, truncated path.
        assert!(read_request(&mut &b"XXXX\0\0\0\0\0\0\0\0"[..]).is_err());
        assert!(read_request(&mut &b"PTS1\x00\x01\0\0\xff\xff\xff\xff"[..]).is_err());
        assert!(read_request(&mut &b"PTS1\x00\x01\0\0\x05\0\0\0ab"[..]).is_err());
        // A picture claiming 100000 pixels a side, a truncated picture, an
        // unknown status, an oversized message.
        assert!(read_reply(&mut &b"\0\xa0\x86\x01\0\x01\0\0\0"[..]).is_err());
        assert!(read_reply(&mut &b"\0\x02\0\0\0\x02\0\0\0\x01\x02"[..]).is_err());
        assert!(read_reply(&mut &b"\x07"[..]).is_err());
        assert!(read_reply(&mut &b"\x02\xff\xff\0\0"[..]).is_err());
        assert!(read_reply(&mut &b""[..]).is_err());
        // Every prefix of a valid reply fails cleanly.
        let mut buf = Vec::new();
        write_reply(&mut buf, &Reply::Image { width: 3, height: 2, rgba: vec![9; 24] }).unwrap();
        for n in 0..buf.len() {
            assert!(read_reply(&mut &buf[..n]).is_err(), "prefix {n}");
        }
    }

    #[test]
    fn pictures_must_match_their_size() {
        let bad = Reply::Image { width: 2, height: 2, rgba: vec![0; 4] };
        assert!(write_reply(&mut Vec::new(), &bad).is_err());
    }
}
