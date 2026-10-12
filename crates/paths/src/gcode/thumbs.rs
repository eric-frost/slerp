//! Preview pictures embedded in text G-code.  Slicers write them as
//! comment blocks near the top of the file (a few put them at the end), so
//! only the head and tail of a large file are read:
//!
//! - PrusaSlicer, SuperSlicer, OrcaSlicer, Bambu Studio, Anycubic and
//!   Cura's "Create Thumbnail" script: `; thumbnail begin 300x300 12345`,
//!   base64 lines, `; thumbnail end`, with `thumbnail_PNG`, `thumbnail_JPG`
//!   and `thumbnail_QOI` variants (Bambu wraps them in
//!   `; THUMBNAIL_BLOCK_START`).
//! - Creality Print and Creality Slicer: `; png begin` or `; jpg begin`.
//! - Snapmaker Luban: one line, `;thumbnail: data:image/png;base64,...`.
//! - MKS printers (and Elegoo's and others' plugins for them):
//!   `;simage:` and `;;gimage:`, 100 and 200 pixels square of RGB565 in
//!   hexadecimal, continued on `M10086 ;` lines.
//! - Weedo printers: a picture file in hexadecimal on `W220` lines between
//!   `W221` and `W222`.

use crate::text::{Lines, base64_decode, for_chunks, hex_decode, starts_with_ci, trim};
use slerp_scene::textures::{ColorSpace, DecodeOptions, Texture};
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

/// How far into a file thumbnails are looked for, at most: they come
/// before the first move, which is usually within a few hundred kilobytes.
const HEAD_BYTES: u64 = 32 << 20;
/// How much of the end of a file is searched.
const TAIL_BYTES: u64 = 1 << 20;
/// The longest encoded picture accepted.
const MAX_ENCODED: usize = 24 << 20;

/// A picture found in a file, still encoded.
#[derive(Clone, Debug)]
pub struct Embedded {
    /// The size the file declares, or 0 when it doesn't.
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
    /// MKS pictures are raw RGB565 rather than a picture file.
    pub rgb565: bool,
}

impl Embedded {
    fn area(&self) -> u64 {
        self.width as u64 * self.height as u64
    }

    /// Decodes the picture at its full size.
    pub fn decode(&self) -> Option<Texture> {
        if self.rgb565 {
            return decode_rgb565(&self.data, self.width, self.height);
        }
        decode_picture(&self.data)
    }
}

/// Decodes a PNG, JPEG, BMP or QOI picture file.
pub fn decode_picture(data: &[u8]) -> Option<Texture> {
    if super::qoi::is_qoi(data) {
        let mut t = super::qoi::decode(data)?;
        t.source.container.clear();
        return Some(t);
    }
    let mut t = slerp_scene::textures::decode(data, &DecodeOptions::full()).ok()?;
    t.source.format = std::mem::take(&mut t.source.container);
    Some(t)
}

/// The largest picture that decodes, from a list of embedded ones.
pub fn best(mut found: Vec<Embedded>) -> Option<Texture> {
    found.sort_by_key(|e| std::cmp::Reverse(e.area()));
    found.iter().find_map(|e| e.decode().filter(|t| t.width() > 1 && t.height() > 1))
}

/// Finds the pictures embedded in the text G-code at `path`, reading its
/// head (up to the first move) and its last megabyte.
pub fn embedded_thumbnails(path: &Path) -> std::io::Result<Vec<Embedded>> {
    let mut file = std::fs::File::open(path)?;
    let len = file.metadata()?.len();
    let mut scan = Scanner::new(true);
    let mut lines = Lines::new(MAX_ENCODED);
    let mut read = 0u64;
    let head = (&mut file).take(HEAD_BYTES);
    for_chunks(head, 1 << 20, |chunk| {
        read += chunk.len() as u64;
        lines.push(chunk, &mut |l| scan.line(l))
    })?;
    if !scan.moved {
        lines.finish(&mut |l| scan.line(l));
    }
    scan.flush();
    if len > read {
        let from = read.max(len.saturating_sub(TAIL_BYTES));
        file.seek(SeekFrom::Start(from))?;
        let mut tail = Scanner { skip_first: from > 0, ..Default::default() };
        let mut lines = Lines::new(MAX_ENCODED);
        for_chunks(file, 1 << 20, |chunk| lines.push(chunk, &mut |l| tail.line(l)))?;
        lines.finish(&mut |l| tail.line(l));
        tail.flush();
        scan.found.extend(tail.found);
    }
    Ok(scan.found)
}

/// Collects pictures from G-code lines.
#[derive(Default)]
pub struct Scanner {
    pub found: Vec<Embedded>,
    block: Option<Block>,
    /// A move was seen: the head holds no more pictures.
    moved: bool,
    /// The first line read is part of one cut by a seek.
    skip_first: bool,
    /// Stop at the first move (reading a file's head).
    stop_at_move: bool,
}

struct Block {
    kind: BlockKind,
    /// The tag that opened it, which closes it with " end".
    tag: Vec<u8>,
    width: u32,
    height: u32,
    data: Vec<u8>,
}

#[derive(PartialEq)]
enum BlockKind {
    Base64,
    Mks,
    Weedo,
}

impl Scanner {
    /// A scanner that stops at the first move (for a file's head) or
    /// reads on (for its tail, or a whole stream).
    pub fn new(stop_at_move: bool) -> Scanner {
        Scanner { stop_at_move, ..Default::default() }
    }

    /// Reads one line; false once the head's pictures are behind.
    pub fn line(&mut self, line: &[u8]) -> bool {
        if std::mem::take(&mut self.skip_first) {
            return true;
        }
        let s = trim(line);
        if let Some(block) = &mut self.block {
            match block.kind {
                BlockKind::Base64 => {
                    if let Some(c) = s.strip_prefix(b";") {
                        let c = trim(c);
                        let end = c.len() == block.tag.len() + 4
                            && starts_with_ci(c, &block.tag)
                            && c[block.tag.len()..].eq_ignore_ascii_case(b" end");
                        if end {
                            self.flush();
                        } else if block.data.len() + c.len() <= MAX_ENCODED {
                            block.data.extend_from_slice(c);
                        }
                        return true;
                    }
                    if !s.is_empty() {
                        self.block = None;
                    }
                }
                BlockKind::Mks => {
                    if let Some(hex) = s.strip_prefix(b"M10086 ;").or_else(|| s.strip_prefix(b"M10086;")) {
                        if hex.is_empty() {
                            self.flush();
                        } else if block.data.len() + hex.len() <= MAX_ENCODED {
                            block.data.extend_from_slice(hex);
                        }
                        return true;
                    }
                    self.flush();
                }
                BlockKind::Weedo => {
                    if let Some(hex) = s.strip_prefix(b"W220") {
                        if block.data.len() + hex.len() <= MAX_ENCODED {
                            block.data.extend_from_slice(hex);
                        }
                        return true;
                    }
                    if s.starts_with(b"W222") {
                        self.flush();
                        return true;
                    }
                    self.block = None;
                }
            }
        }
        if let Some(c) = s.strip_prefix(b";") {
            self.comment(trim(c));
            return true;
        }
        if s.starts_with(b"W221") {
            self.block = Some(Block { kind: BlockKind::Weedo, tag: Vec::new(), width: 0, height: 0, data: Vec::new() });
            return true;
        }
        // The first move ends the header.
        let first_move = s.len() >= 2
            && (s[0] == b'G' || s[0] == b'g')
            && matches!(s[1], b'0'..=b'3')
            && s.get(2).is_none_or(|c| !c.is_ascii_digit());
        if first_move && self.stop_at_move {
            self.moved = true;
            return false;
        }
        true
    }

    fn comment(&mut self, c: &[u8]) {
        // `; thumbnail begin 300x300 12345`, `; png begin 300*300 ...`
        let words: Vec<&[u8]> = c.splitn(3, |&b| b == b' ').collect();
        if words.len() >= 2 && words[1].eq_ignore_ascii_case(b"begin") {
            let tag = words[0];
            let known = [
                &b"thumbnail"[..],
                b"thumbnail_png",
                b"thumbnail_jpg",
                b"thumbnail_qoi",
                b"png",
                b"jpg",
                b"jpeg",
                b"qoi",
            ];
            if known.iter().any(|k| tag.eq_ignore_ascii_case(k)) {
                let (width, height) = words.get(2).map_or((0, 0), |d| dimensions(d));
                self.block =
                    Some(Block { kind: BlockKind::Base64, tag: tag.to_vec(), width, height, data: Vec::new() });
            }
            return;
        }
        // Snapmaker Luban: `;thumbnail: data:image/png;base64,iVBOR...`
        if starts_with_ci(c, b"thumbnail:") {
            let rest = &c[10..];
            if let Some(i) = memchr::memmem::find(rest, b"base64,") {
                let data = base64_decode(&rest[i + 7..], MAX_ENCODED);
                self.found.push(Embedded { width: 0, height: 0, data, rgb565: false });
            }
            return;
        }
        // MKS: `;simage:<hex>` (100 px) and `;;gimage:<hex>` (200 px).
        let (mks, size) = if let Some(h) = c.strip_prefix(b"simage:") {
            (Some(h), 100)
        } else if let Some(h) = c.strip_prefix(b";gimage:") {
            (Some(h), 200)
        } else {
            (None, 0)
        };
        if let Some(hex) = mks {
            self.block =
                Some(Block { kind: BlockKind::Mks, tag: Vec::new(), width: size, height: size, data: hex.to_vec() });
        }
    }

    /// Ends the open block, keeping its picture.
    pub fn flush(&mut self) {
        let Some(b) = self.block.take() else { return };
        let (data, rgb565) = match b.kind {
            BlockKind::Base64 => (base64_decode(&b.data, MAX_ENCODED), false),
            BlockKind::Mks => (hex_decode(&b.data, MAX_ENCODED), true),
            BlockKind::Weedo => (hex_decode(&b.data, MAX_ENCODED), false),
        };
        if !data.is_empty() {
            self.found.push(Embedded { width: b.width, height: b.height, data, rgb565 });
        }
    }
}

/// "300x300", "300*300" or "300 300 12345".
fn dimensions(d: &[u8]) -> (u32, u32) {
    let mut nums = d.split(|&b| b == b'x' || b == b'X' || b == b'*' || b == b' ').filter(|p| !p.is_empty());
    let mut next =
        || nums.next().and_then(|p| std::str::from_utf8(p).ok()).and_then(|p| p.parse::<u32>().ok()).unwrap_or(0);
    let (w, h) = (next(), next());
    if w > 0 && h > 0 && w <= 8192 && h <= 8192 { (w, h) } else { (0, 0) }
}

/// MKS pictures: 16-bit RGB565 pixels, low byte first.  Elegoo's newer
/// plugin writes a compressed variant under the same names, which this
/// rejects by its length.
fn decode_rgb565(data: &[u8], width: u32, height: u32) -> Option<Texture> {
    let n = width as usize * height as usize;
    if n == 0 || data.len() != n * 2 {
        return None;
    }
    let mut out = Vec::with_capacity(n * 4);
    for px in data.chunks_exact(2) {
        let v = u16::from_le_bytes([px[0], px[1]]);
        let (r, g, b) = ((v >> 11) & 31, (v >> 5) & 63, v & 31);
        out.extend_from_slice(&[(r * 255 / 31) as u8, (g * 255 / 63) as u8, (b * 255 / 31) as u8, 255]);
    }
    Some(Texture::from_rgba8(width, height, out, ColorSpace::Srgb))
}
