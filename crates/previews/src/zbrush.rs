//! ZBrush files (`.ztl` tools, `.zpr` projects, `.zbr` documents).  They
//! start with the text "ZBrush File." and a 0x5C-byte header, then a tree
//! of blocks: each starts with the marker `03 80 01 00`, a 16-bit block
//! number and a 32-bit length that counts the whole block, and ends with
//! `03 88 00 00`.  The first blocks hold the file's thumbnail:
//!
//! - block 1: the picture's width and height as 32-bit floats, 18 bytes in;
//! - block 10, holding block 6: the pixels, as a header (the unpacked size,
//!   the channel count, the row length, a table of strip lengths ending in
//!   a zero) and then the strips, each a run of horizontal rows packed with
//!   a byte-wise run-length code (a byte below 128 repeats the next byte
//!   that many times; a byte from 128 up is followed by 256 minus it
//!   literal bytes).  Unpacked, each strip stores its rows plane by plane:
//!   blue, green, red, and a fourth plane that is always zero.
//!
//! Older ZBrush versions write a 96-pixel square in one strip; newer ones a
//! 512-pixel square in eight.  The picture is the model on black, opaque.

use crate::source::Source;
use slerp_scene::LoadError;
use slerp_scene::textures::{ColorSpace, Texture};

const BANNER: &[u8] = b"ZBrush File.";
const HEADER: u64 = 0x5C;
const BLOCK: [u8; 4] = [0x03, 0x80, 0x01, 0x00];

/// The largest thumbnail side accepted, and how much of the file is read
/// to find it: the thumbnail comes first.
const MAX_SIDE: u32 = 4096;
const MAX_READ: u64 = 4 << 20;

pub fn looks_like_zbrush(head: &[u8]) -> bool {
    head.starts_with(BANNER)
}

fn bad(what: &str) -> LoadError {
    LoadError::invalid(format!("ZBrush thumbnail: {what}"))
}

fn u16_at(b: &[u8], i: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(i..i + 2)?.try_into().ok()?))
}

fn u32_at(b: &[u8], i: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(i..i + 4)?.try_into().ok()?))
}

fn f32_at(b: &[u8], i: usize) -> Option<f32> {
    Some(f32::from_le_bytes(b.get(i..i + 4)?.try_into().ok()?))
}

/// Blocks in `b[start..end]`: number, start and length.
fn blocks(b: &[u8], start: usize, end: usize) -> Vec<(u16, usize, usize)> {
    let mut out = Vec::new();
    let mut i = start;
    while i + 10 <= end.min(b.len()) && out.len() < 64 {
        if b[i..i + 4] != BLOCK {
            break;
        }
        let (Some(id), Some(len)) = (u16_at(b, i + 4), u32_at(b, i + 6)) else { break };
        let len = len as usize;
        if len < 10 || i + len > end.min(b.len()) {
            out.push((id, i, end.min(b.len()) - i));
            break;
        }
        out.push((id, i, len));
        i += len;
    }
    out
}

pub fn preview(src: &mut Source) -> Result<Texture, LoadError> {
    let b = src.head(MAX_READ)?;
    let top = blocks(&b, HEADER as usize, b.len());
    // 1. The picture's size.
    let &(_, size_at, _) = top.iter().find(|t| t.0 == 1).ok_or_else(|| crate::no_preview("ZBrush file"))?;
    let (w, h) = (f32_at(&b, size_at + 18).unwrap_or(0.0), f32_at(&b, size_at + 22).unwrap_or(0.0));
    if !(w >= 1.0 && h >= 1.0 && w <= MAX_SIDE as f32 && h <= MAX_SIDE as f32) {
        return Err(bad("implausible size"));
    }
    let (w, h) = (w as usize, h as usize);
    // 2. The pixel block, inside block 10.
    let &(_, outer, outer_len) = top.iter().find(|t| t.0 == 10).ok_or_else(|| crate::no_preview("ZBrush file"))?;
    let inner_start = (outer + 10..outer + 10 + 64)
        .find(|&i| b.get(i..i + 6) == Some(&[3, 0x80, 1, 0, 6, 0]))
        .ok_or_else(|| bad("no pixel block"))?;
    let (_, at, len) = blocks(&b, inner_start, outer + outer_len)
        .into_iter()
        .find(|t| t.0 == 6)
        .ok_or_else(|| bad("no pixel block"))?;
    let p = at + 10;
    let raw = u32_at(&b, p).ok_or_else(|| bad("truncated"))? as usize;
    let channels = u16_at(&b, p + 4).ok_or_else(|| bad("truncated"))? as usize;
    if channels != 4 || raw != w * h * 4 {
        return Err(bad("unexpected pixel layout"));
    }
    let n = u32_at(&b, p + 16).ok_or_else(|| bad("truncated"))? as usize;
    if n > 1024 {
        return Err(bad("too many strips"));
    }
    let mut strips = Vec::new();
    for k in 0..n {
        match u32_at(&b, p + 20 + k * 4).ok_or_else(|| bad("truncated"))? {
            0 => break,
            s => strips.push(s as usize),
        }
    }
    if strips.is_empty() || h % strips.len() != 0 {
        return Err(bad("unexpected strips"));
    }
    // 3. Unpack each strip and interleave its planes.
    let rows = h / strips.len();
    let plane = rows * w;
    let mut rgba = vec![0u8; w * h * 4];
    let mut pos = p + 20 + n * 4 + 4;
    for (si, &s) in strips.iter().enumerate() {
        let end = pos.checked_add(s).filter(|&e| e <= at + len).ok_or_else(|| bad("strip runs past its block"))?;
        let data = unpack(&b[pos..end], plane * 4);
        for y in 0..rows {
            for x in 0..w {
                let o = ((si * rows + y) * w + x) * 4;
                let i = y * w + x;
                rgba[o] = data[2 * plane + i];
                rgba[o + 1] = data[plane + i];
                rgba[o + 2] = data[i];
                rgba[o + 3] = 255;
            }
        }
        pos = end;
    }
    let mut t = Texture::from_rgba8(w as u32, h as u32, rgba, ColorSpace::Srgb);
    t.source.format = "ZBrush thumbnail".into();
    Ok(t)
}

/// ZBrush's run-length code, unpacked to exactly `want` bytes (zeros when
/// the data runs out).
fn unpack(packed: &[u8], want: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(want);
    let mut i = 0;
    while i < packed.len() && out.len() < want {
        let c = packed[i] as usize;
        i += 1;
        if c >= 128 {
            let n = (256 - c).min(packed.len() - i).min(want - out.len());
            out.extend_from_slice(&packed[i..i + n]);
            i += 256 - c;
        } else {
            let Some(&v) = packed.get(i) else { break };
            i += 1;
            out.resize(out.len() + c.min(want - out.len()), v);
        }
    }
    out.resize(want, 0);
    out
}
