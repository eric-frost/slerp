//! The chunk structure of a Rhino `.3dm` file (openNURBS's "binary archive").
//!
//! After a 32-byte header (`3D Geometry File Format ` and a right-justified
//! version number) the file is a sequence of chunks.  A chunk starts with a
//! four-byte type code and a length: four bytes in archive versions 1 to 4
//! and eight from version 50 (Rhino 5) on.  The length counts the bytes
//! that follow it.  If the type code has its top bit set (`TCODE_SHORT`), the
//! "length" is the chunk's value and nothing follows.  If bit 15
//! (`TCODE_CRC`) is set, the last four bytes of the body are a checksum.
//! Chunks nest: tables hold records, records hold more chunks.
//!
//! Everything here works on a byte slice with bounds checks, so damaged
//! files end the walk early instead of reading out of bounds.

use std::ops::Range;

/// The first 24 bytes of every file.
pub const MAGIC: &[u8; 24] = b"3D Geometry File Format ";

pub const TCODE_SHORT: u32 = 0x8000_0000;
pub const TCODE_CRC: u32 = 0x0000_8000;

// Tables (the top-level chunks).
pub const TABLE_MATERIAL: u32 = 0x1000_0010;
pub const TABLE_LAYER: u32 = 0x1000_0011;
pub const TABLE_OBJECT: u32 = 0x1000_0013;
pub const TABLE_PROPERTIES: u32 = 0x1000_0014;
pub const TABLE_SETTINGS: u32 = 0x1000_0015;
pub const TABLE_INSTANCE_DEFINITION: u32 = 0x1000_0021;
pub const END_OF_FILE: u32 = 0x0000_7FFF;
#[allow(dead_code)]
pub const END_OF_TABLE: u32 = 0xFFFF_FFFF;

// Table records.
pub const MATERIAL_RECORD: u32 = 0x2000_8040;
pub const LAYER_RECORD: u32 = 0x2000_8050;
pub const OBJECT_RECORD: u32 = 0x2000_8070;
#[allow(dead_code)]
pub const OBJECT_RECORD_TYPE: u32 = 0x8200_0071;
pub const OBJECT_RECORD_ATTRIBUTES: u32 = 0x0200_8072;
pub const INSTANCE_DEFINITION_RECORD: u32 = 0x2000_8076;
pub const SETTINGS_UNITS: u32 = 0x2000_8031;

// Properties table.
pub const PROPERTIES_PREVIEW: u32 = 0x2000_8023;
pub const PROPERTIES_PREVIEW_COMPRESSED: u32 = 0x2000_8025;
pub const PROPERTIES_OPENNURBS_VERSION: u32 = 0xA000_0026;

// Objects: a class (with its id and data) and the pieces of a class chunk.
pub const OPENNURBS_CLASS: u32 = 0x0002_7FFA;
pub const OPENNURBS_CLASS_UUID: u32 = 0x0002_FFFB;
pub const OPENNURBS_CLASS_DATA: u32 = 0x0002_FFFC;
pub const ANONYMOUS_CHUNK: u32 = 0x4000_8000;

/// One chunk: its type code, its value (the length, or the value of a short
/// chunk) and where its body is.
#[derive(Clone, Debug)]
pub struct Chunk {
    pub tcode: u32,
    pub value: i64,
    /// The body without the trailing checksum; empty for short chunks.
    pub body: Range<usize>,
    /// Where the chunk after this one starts.
    pub next: usize,
}

impl Chunk {
    #[cfg(test)]
    pub fn is_short(&self) -> bool {
        self.tcode & TCODE_SHORT != 0
    }
}

/// The bytes of a file and what the header says about how to read them.
#[derive(Clone, Copy)]
pub struct Archive<'a> {
    pub data: &'a [u8],
    /// 1 to 4 for Rhino 1 to 4, then 50, 60, 70, 80, 90.
    pub version: u32,
}

impl<'a> Archive<'a> {
    /// Checks the header.  Rhino files can't be told apart by anything else.
    pub fn open(data: &'a [u8]) -> Option<Archive<'a>> {
        let head = data.get(..32)?;
        if &head[..24] != MAGIC {
            return None;
        }
        let digits = std::str::from_utf8(&head[24..32]).ok()?.trim();
        let version: u32 = digits.parse().ok()?;
        (1..=1000).contains(&version).then_some(Archive { data, version })
    }

    /// Whether lengths are eight bytes (Rhino 5 and later).
    pub fn big(&self) -> bool {
        self.version >= 50
    }

    /// The chunk starting at `pos`, which must end by `end`.
    pub fn chunk_at(&self, pos: usize, end: usize) -> Option<Chunk> {
        let end = end.min(self.data.len());
        let value_len = if self.big() { 8 } else { 4 };
        let header = 4 + value_len;
        if pos.checked_add(header)? > end {
            return None;
        }
        let tcode = u32::from_le_bytes(self.data[pos..pos + 4].try_into().ok()?);
        let value = if self.big() {
            i64::from_le_bytes(self.data[pos + 4..pos + 12].try_into().ok()?)
        } else {
            let v = i32::from_le_bytes(self.data[pos + 4..pos + 8].try_into().ok()?);
            // Unsigned values of short chunks with the high bit are common
            // (`0xFFFFFFFF` ends a table), but lengths are never negative.
            if tcode & TCODE_SHORT != 0 { v as u32 as i64 } else { v as i64 }
        };
        let start = pos + header;
        if tcode & TCODE_SHORT != 0 {
            return Some(Chunk { tcode, value, body: start..start, next: start });
        }
        let len = usize::try_from(value).ok()?;
        let stop = start.checked_add(len)?;
        if stop > end {
            return None;
        }
        let body_end = if tcode & TCODE_CRC != 0 && len >= 4 { stop - 4 } else { stop };
        Some(Chunk { tcode, value, body: start..body_end, next: stop })
    }

    /// The type code and value in the header of the chunk at the start of
    /// `bytes` (which may hold only the header).
    pub fn header(&self, bytes: &[u8]) -> Option<(u32, i64)> {
        let tcode = u32::from_le_bytes(bytes.get(..4)?.try_into().ok()?);
        let value = if self.big() {
            i64::from_le_bytes(bytes.get(4..12)?.try_into().ok()?)
        } else {
            let v = i32::from_le_bytes(bytes.get(4..8)?.try_into().ok()?);
            if tcode & TCODE_SHORT != 0 { v as u32 as i64 } else { v as i64 }
        };
        Some((tcode, value))
    }

    /// The chunks from `range.start` to `range.end`, stopping at the first
    /// one that doesn't fit.
    pub fn chunks(&self, range: Range<usize>) -> Chunks<'a> {
        Chunks { archive: *self, pos: range.start, end: range.end }
    }

    pub fn body(&self, c: &Chunk) -> &'a [u8] {
        &self.data[c.body.clone()]
    }
}

pub struct Chunks<'a> {
    archive: Archive<'a>,
    pos: usize,
    end: usize,
}

impl Iterator for Chunks<'_> {
    type Item = Chunk;

    fn next(&mut self) -> Option<Chunk> {
        if self.pos >= self.end {
            return None;
        }
        let c = self.archive.chunk_at(self.pos, self.end)?;
        // A chunk always moves on, so a loop can't happen.
        self.pos = c.next.max(self.pos + 1);
        Some(c)
    }
}

/// Reads values from the body of a chunk, never past its end.  Every read
/// returns `None` at the end, so parsers chain with `?`.
#[derive(Clone)]
pub struct Cursor<'a> {
    pub data: &'a [u8],
    pub pos: usize,
}

impl<'a> Cursor<'a> {
    pub fn new(data: &'a [u8]) -> Cursor<'a> {
        Cursor { data, pos: 0 }
    }

    pub fn remaining(&self) -> usize {
        self.data.len().saturating_sub(self.pos)
    }

    pub fn bytes(&mut self, n: usize) -> Option<&'a [u8]> {
        let end = self.pos.checked_add(n)?;
        let s = self.data.get(self.pos..end)?;
        self.pos = end;
        Some(s)
    }

    pub fn skip(&mut self, n: usize) -> Option<()> {
        self.bytes(n).map(|_| ())
    }

    pub fn u8(&mut self) -> Option<u8> {
        self.bytes(1).map(|b| b[0])
    }

    pub fn i16(&mut self) -> Option<i16> {
        Some(i16::from_le_bytes(self.bytes(2)?.try_into().ok()?))
    }

    pub fn u32(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(self.bytes(4)?.try_into().ok()?))
    }

    pub fn i32(&mut self) -> Option<i32> {
        Some(i32::from_le_bytes(self.bytes(4)?.try_into().ok()?))
    }

    pub fn f64(&mut self) -> Option<f64> {
        Some(f64::from_le_bytes(self.bytes(8)?.try_into().ok()?))
    }

    pub fn uuid(&mut self) -> Option<[u8; 16]> {
        self.bytes(16)?.try_into().ok()
    }

    /// A string: a count of UTF-16 units including the terminator, then the
    /// units.
    pub fn string(&mut self) -> Option<String> {
        let n = self.u32()? as usize;
        if n == 0 {
            return Some(String::new());
        }
        let raw = self.bytes(n.checked_mul(2)?)?;
        let units: Vec<u16> = raw.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        let end = units.iter().position(|&u| u == 0).unwrap_or(units.len());
        Some(String::from_utf16_lossy(&units[..end]))
    }

    /// The version byte most classes start with: major in the high nibble,
    /// minor in the low one.
    pub fn version(&mut self) -> Option<(u8, u8)> {
        let v = self.u8()?;
        Some((v >> 4, v & 15))
    }

    /// The version pair (two integers) at the start of a versioned
    /// anonymous chunk.
    pub fn chunk_version(&mut self) -> Option<(i32, i32)> {
        Some((self.i32()?, self.i32()?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tiny file: header, a comment chunk and an end marker, in the
    /// four-byte-length layout of old versions or the eight-byte one.
    fn file(version: u32, big: bool) -> Vec<u8> {
        let mut d = MAGIC.to_vec();
        d.extend(format!("{version:>8}").as_bytes());
        let put = |d: &mut Vec<u8>, tcode: u32, value: i64| {
            d.extend(tcode.to_le_bytes());
            if big { d.extend(value.to_le_bytes()) } else { d.extend((value as i32).to_le_bytes()) }
        };
        put(&mut d, 1, 5);
        d.extend(b"hello");
        put(&mut d, 0x2000_8023 | TCODE_CRC, 6);
        d.extend(b"ab");
        d.extend([1, 2, 3, 4]);
        put(&mut d, 0xA000_0026, 201209145);
        put(&mut d, END_OF_FILE, 8);
        d.extend([0; 8]);
        d
    }

    #[test]
    fn walks_both_layouts() {
        for (version, big) in [(4, false), (50, true), (80, true)] {
            let d = file(version, big);
            let a = Archive::open(&d).expect("opens");
            assert_eq!(a.version, version);
            let chunks: Vec<_> = a.chunks(32..d.len()).collect();
            assert_eq!(chunks.len(), 4, "{version}");
            assert_eq!(a.body(&chunks[0]), b"hello");
            // The checksum is not part of the body.
            assert_eq!(a.body(&chunks[1]), b"ab");
            assert!(chunks[2].is_short());
            assert_eq!(chunks[2].value, 201209145);
            assert_eq!(chunks[3].tcode, END_OF_FILE);
        }
    }

    #[test]
    fn rejects_other_files_and_bad_versions() {
        assert!(Archive::open(b"3D Geometry File Format        X").is_none());
        assert!(Archive::open(b"3D Geometry File Format       0 ").is_none());
        assert!(Archive::open(b"3D Geometry File Format ").is_none());
        assert!(Archive::open(b"Rhino file").is_none());
        assert!(Archive::open(&[]).is_none());
    }

    #[test]
    fn damaged_lengths_stop_the_walk() {
        let mut d = file(50, true);
        // A first chunk claiming to be gigabytes long.
        d[36..44].copy_from_slice(&(i64::MAX).to_le_bytes());
        let a = Archive::open(&d).unwrap();
        assert_eq!(a.chunks(32..d.len()).count(), 0);
        // Negative.
        d[36..44].copy_from_slice(&(-5i64).to_le_bytes());
        assert_eq!(Archive::open(&d).unwrap().chunks(32..d.len()).count(), 0);
        // Every truncation of a good file walks without panicking.
        let good = file(50, true);
        for n in 0..good.len() {
            if let Some(a) = Archive::open(&good[..n]) {
                for c in a.chunks(32..n) {
                    let _ = a.body(&c);
                }
            }
        }
    }

    #[test]
    fn cursor_reads_stay_in_bounds() {
        let mut c = Cursor::new(&[1, 0, 0, 0, 0xFF, 2]);
        assert_eq!(c.u32(), Some(1));
        assert_eq!(c.u32(), None);
        assert_eq!(c.u8(), Some(0xFF));
        assert_eq!(c.remaining(), 1);
        assert!(c.bytes(usize::MAX).is_none());
        // A string with a count far beyond the data.
        let mut c = Cursor::new(&[0xFF, 0xFF, 0xFF, 0xFF, 1, 0]);
        assert!(c.string().is_none());
        let mut c = Cursor::new(&[3, 0, 0, 0, b'h', 0, b'i', 0, 0, 0]);
        assert_eq!(c.string().as_deref(), Some("hi"));
    }
}
