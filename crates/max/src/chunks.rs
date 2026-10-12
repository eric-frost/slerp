//! The chunk trees 3ds Max writes into its streams.
//!
//! A chunk is a 16-bit id and a 32-bit size counting the six-byte header;
//! a size of 0 means a 64-bit size follows (a fourteen-byte header).  The
//! top bit of the size marks a container, whose data is more chunks.  The
//! `Scene` stream of newer files may be gzip-compressed as a whole.

use slerp_scene::LoadError;
use std::io::Read;

/// Deepest nesting followed; real files nest about eight deep.
const MAX_DEPTH: u32 = 48;
/// Most chunks read from one stream, against a damaged file that is
/// nothing but empty headers (each costs some 40 bytes held).
const MAX_CHUNKS: usize = 16_000_000;

#[derive(Debug)]
pub struct Chunk<'a> {
    pub id: u16,
    pub body: Body<'a>,
}

#[derive(Debug)]
pub enum Body<'a> {
    Data(&'a [u8]),
    Children(Vec<Chunk<'a>>),
}

impl<'a> Chunk<'a> {
    pub fn children(&self) -> &[Chunk<'a>] {
        match &self.body {
            Body::Children(c) => c,
            Body::Data(_) => &[],
        }
    }

    pub fn data(&self) -> Option<&'a [u8]> {
        match self.body {
            Body::Data(d) => Some(d),
            Body::Children(_) => None,
        }
    }

    /// The first child with id `id`.
    pub fn child(&self, id: u16) -> Option<&Chunk<'a>> {
        self.children().iter().find(|c| c.id == id)
    }

    /// The data of the first child with id `id`.
    pub fn child_data(&self, id: u16) -> Option<&'a [u8]> {
        self.child(id).and_then(Chunk::data)
    }

    /// The first chunk with id `id` anywhere below this one.
    pub fn find(&self, id: u16) -> Option<&Chunk<'a>> {
        for c in self.children() {
            if c.id == id {
                return Some(c);
            }
            if let Some(f) = c.find(id) {
                return Some(f);
            }
        }
        None
    }
}

/// Reads the chunks of a stream.
pub fn parse(data: &[u8]) -> Vec<Chunk<'_>> {
    let mut count = 0;
    level(data, 0, &mut count)
}

fn level<'a>(data: &'a [u8], depth: u32, count: &mut usize) -> Vec<Chunk<'a>> {
    let mut out = Vec::new();
    let mut at = 0usize;
    while at + 6 <= data.len() && *count < MAX_CHUNKS {
        let id = u16::from_le_bytes([data[at], data[at + 1]]);
        let small = u32::from_le_bytes([data[at + 2], data[at + 3], data[at + 4], data[at + 5]]);
        let (header, size, container) = if small == 0 {
            let Some(b) = data.get(at + 6..at + 14) else { break };
            let big = u64::from_le_bytes(b.try_into().unwrap_or([0; 8]));
            (14usize, big & !(1 << 63), big >> 63 != 0)
        } else {
            (6usize, (small & 0x7FFF_FFFF) as u64, small >> 31 != 0)
        };
        if size < header as u64 || at as u64 + size > data.len() as u64 {
            break;
        }
        let end = at + size as usize;
        let body = &data[at + header..end];
        *count += 1;
        let body = if container && depth < MAX_DEPTH {
            Body::Children(level(body, depth + 1, count))
        } else {
            Body::Data(body)
        };
        out.push(Chunk { id, body });
        at = end;
    }
    out
}

/// A stream's bytes ready for `parse`: gunzipped when compressed, with at
/// most `max` bytes coming out (a small compressed stream can claim
/// gigabytes).
pub fn decompress(stream: Vec<u8>, max: u64) -> Result<Vec<u8>, LoadError> {
    if !stream.starts_with(&[0x1F, 0x8B]) {
        return Ok(stream);
    }
    let mut out = Vec::new();
    let mut reader = flate2::read::GzDecoder::new(&stream[..]).take(max.saturating_add(1));
    reader.read_to_end(&mut out).map_err(|e| LoadError::invalid(format!("compressed scene: {e}")))?;
    if out.len() as u64 > max {
        return Err(LoadError::TooLarge(format!("the scene unpacks to more than {max} bytes")));
    }
    Ok(out)
}

/*┌─────────────────╮
| Values          |
└─────────────────╯*/
//region Values

pub fn u16_at(b: &[u8], i: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(i..i + 2)?.try_into().ok()?))
}

pub fn u32_at(b: &[u8], i: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(i..i + 4)?.try_into().ok()?))
}

pub fn i32_at(b: &[u8], i: usize) -> Option<i32> {
    u32_at(b, i).map(|v| v as i32)
}

pub fn f32_at(b: &[u8], i: usize) -> Option<f32> {
    u32_at(b, i).map(f32::from_bits)
}

/// `n` floats from offset `i`.
pub fn floats(b: &[u8], i: usize, n: usize) -> Option<Vec<f32>> {
    let end = i.checked_add(n.checked_mul(4)?)?;
    Some(b.get(i..end)?.chunks_exact(4).map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect())
}

/// UTF-16 text, up to a terminating zero.
pub fn utf16(b: &[u8]) -> String {
    let units: Vec<u16> = b.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).take_while(|&u| u != 0).collect();
    String::from_utf16_lossy(&units)
}

//endregion
