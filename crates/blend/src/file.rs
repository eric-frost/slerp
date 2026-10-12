//! The container: the file header, the blocks, and finding a block by the
//! address it had in Blender's memory when the file was saved.
//!
//! A `.blend` file is Blender's memory written out block by block.  Each
//! block starts with a small header (a four-letter code, the size, the
//! old memory address, which structure it holds and how many), and
//! pointers inside blocks are those old addresses, so following one means
//! looking up the block that was written from that address.  The `DNA1`
//! block near the end describes every structure (see `sdna.rs`).
//!
//! Three header layouts exist: 32-bit and 64-bit pointers in the original
//! 12-byte file header, and Blender 5's 17-byte header whose 32-byte block
//! headers allow blocks over 2 GB.  Files may be compressed whole: gzip
//! before Blender 3.0, Zstandard since.

use crate::sdna::Sdna;
use slerp_scene::LoadError;
use std::collections::HashMap;
use std::io::Read;

/*┌─────────────────╮
| Header          |
└─────────────────╯*/
//region Header

/// What the file header says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Header {
    /// 4 or 8.
    pub pointer_size: u8,
    pub big_endian: bool,
    /// Blender's version times 100 plus the minor version, such as 279,
    /// 405 or 500.
    pub version: u32,
    /// 0 for the original block headers, 1 for Blender 5's large ones.
    pub format: u8,
    /// Bytes of the file header: 12 or 17.
    pub len: usize,
}

impl Header {
    /// Bytes of each block header.
    pub fn block_header_len(&self) -> usize {
        match (self.format, self.pointer_size) {
            (1, _) => 32,
            (_, 4) => 20,
            _ => 24,
        }
    }
}

/// Reads the file header from the first bytes of an uncompressed file.
pub fn parse_header(b: &[u8]) -> Result<Header, LoadError> {
    if b.len() < 12 || &b[..7] != b"BLENDER" {
        return Err(LoadError::NotMine);
    }
    let digits = |s: &[u8]| -> Option<u32> {
        s.iter().try_fold(0u32, |n, &c| c.is_ascii_digit().then(|| n * 10 + (c - b'0') as u32))
    };
    match b[7] {
        // 1. "BLENDER" + '_' (4-byte pointers) or '-' (8) + 'v' (little
        //    endian) or 'V' (big) + three version digits.
        b'_' | b'-' => {
            let big_endian = match b[8] {
                b'v' => false,
                b'V' => true,
                _ => return Err(LoadError::invalid("unknown byte order in the .blend header")),
            };
            let version = digits(&b[9..12]).ok_or_else(|| LoadError::invalid("bad .blend version"))?;
            Ok(Header { pointer_size: if b[7] == b'_' { 4 } else { 8 }, big_endian, version, format: 0, len: 12 })
        }
        // 2. Blender 5: "BLENDER" + header size "17" + '-' (8-byte
        //    pointers, the only kind) + format "01" + 'v' + four version
        //    digits.
        b'0'..=b'9' => {
            if b.len() < 17 {
                return Err(LoadError::invalid("truncated .blend header"));
            }
            let size = digits(&b[7..9]);
            let format = digits(&b[10..12]);
            let version = digits(&b[13..17]);
            match (size, b[9], format, b[12], version) {
                (Some(17), b'-', Some(1), b'v', Some(version)) => {
                    Ok(Header { pointer_size: 8, big_endian: false, version, format: 1, len: 17 })
                }
                _ => Err(LoadError::Unsupported("a newer .blend file format than Slerp knows".into())),
            }
        }
        _ => Err(LoadError::invalid("unknown .blend header")),
    }
}

//endregion

/*┌─────────────────╮
| Compression     |
└─────────────────╯*/
//region Compression

/// How the file is stored on disk.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Packing {
    Plain,
    Gzip,
    Zstd,
}

impl Packing {
    pub fn of(head: &[u8]) -> Option<Packing> {
        if head.starts_with(b"BLENDER") {
            Some(Packing::Plain)
        } else if head.starts_with(&[0x1f, 0x8b]) {
            Some(Packing::Gzip)
        } else if head.starts_with(&[0x28, 0xb5, 0x2f, 0xfd]) {
            Some(Packing::Zstd)
        } else {
            None
        }
    }

    pub fn name(self) -> Option<&'static str> {
        match self {
            Packing::Plain => None,
            Packing::Gzip => Some("gzip"),
            Packing::Zstd => Some("Zstandard"),
        }
    }
}

/// A reader of the uncompressed file.
pub fn unpacking<'a, R: Read + 'a>(packing: Packing, r: R) -> std::io::Result<Box<dyn Read + 'a>> {
    Ok(match packing {
        Packing::Plain => Box::new(r),
        Packing::Gzip => Box::new(flate2::read::GzDecoder::new(r)),
        Packing::Zstd => Box::new(zstd::stream::read::Decoder::new(r)?),
    })
}

/// The whole uncompressed file, refusing to inflate more than `limit`
/// bytes, so a small malicious file can't claim gigabytes.
pub fn unpack(bytes: &[u8], limit: u64) -> Result<(Vec<u8>, Packing), LoadError> {
    let packing = Packing::of(bytes).ok_or(LoadError::NotMine)?;
    if packing == Packing::Plain {
        return Ok((bytes.to_vec(), packing));
    }
    let mut out = Vec::new();
    let reader = unpacking(packing, bytes)?;
    // A truncated stream still gives what came before the cut, which may
    // hold the preview; only the block parser decides whether it's enough.
    let result = reader.take(limit.saturating_add(1)).read_to_end(&mut out);
    if out.len() as u64 > limit {
        return Err(LoadError::TooLarge(format!("more than {limit} bytes uncompressed")));
    }
    if let Err(e) = result
        && out.len() < 12
    {
        return Err(LoadError::invalid(format!("can't decompress: {e}")));
    }
    Ok((out, packing))
}

//endregion

/*┌─────────────────╮
| Blocks          |
└─────────────────╯*/
//region Blocks

pub const DNA1: [u8; 4] = *b"DNA1";
pub const ENDB: [u8; 4] = *b"ENDB";
pub const GLOB: [u8; 4] = *b"GLOB";
pub const REND: [u8; 4] = *b"REND";
pub const TEST: [u8; 4] = *b"TEST";

/// One block: what it holds and where its bytes are.
#[derive(Clone, Copy, Debug)]
pub struct Block {
    /// Four letters such as `DATA`, or an ID type's two letters and two
    /// zero bytes, such as `OB\0\0`.
    pub code: [u8; 4],
    /// Index of its structure in the SDNA.
    pub sdna: u32,
    /// Its address in Blender's memory when the file was saved.
    pub old: u64,
    /// Where its bytes start in [`BlendFile::data`].
    pub start: usize,
    pub len: usize,
}

impl Block {
    /// True for blocks that start a data-block (an ID: object, mesh,
    /// material, ...), which have two-letter codes.
    pub fn is_id(&self) -> bool {
        self.code[2] == 0 && self.code[3] == 0 && self.code[0] != 0
    }

    pub fn id_code(&self) -> [u8; 2] {
        [self.code[0], self.code[1]]
    }
}

/// A block header read from `b` at `pos`, and where its data starts.
pub fn read_block_header(h: &Header, b: &[u8], pos: usize) -> Option<(Block, usize)> {
    let n = h.block_header_len();
    let raw = b.get(pos..pos.checked_add(n)?)?;
    let rd = Endian(h.big_endian);
    let mut code = [raw[0], raw[1], raw[2], raw[3]];
    // Big-endian files store ID codes as the int 0x0000'O''B', so the two
    // letters come last.
    if h.big_endian && code[0] == 0 && code[1] == 0 && code[2] != 0 {
        code = [code[2], code[3], 0, 0];
    }
    let (sdna, old, len, count) = match (h.format, h.pointer_size) {
        (1, _) => (rd.u32(&raw[4..]), rd.u64(&raw[8..]), rd.i64(&raw[16..]), rd.i64(&raw[24..])),
        (_, 4) => (rd.u32(&raw[12..]), rd.u32(&raw[8..]) as u64, rd.i32(&raw[4..]) as i64, rd.i32(&raw[16..]) as i64),
        _ => (rd.u32(&raw[16..]), rd.u64(&raw[8..]), rd.i32(&raw[4..]) as i64, rd.i32(&raw[20..]) as i64),
    };
    let start = pos + n;
    let len = usize::try_from(len).ok()?;
    let _ = count;
    Some((Block { code, sdna, old, start, len }, start))
}

/// Reads numbers in the file's byte order.
#[derive(Clone, Copy, Debug)]
pub struct Endian(pub bool);

impl Endian {
    #[inline]
    pub fn u16(self, b: &[u8]) -> u16 {
        let a = [b[0], b[1]];
        if self.0 { u16::from_be_bytes(a) } else { u16::from_le_bytes(a) }
    }
    #[inline]
    pub fn u32(self, b: &[u8]) -> u32 {
        let a = [b[0], b[1], b[2], b[3]];
        if self.0 { u32::from_be_bytes(a) } else { u32::from_le_bytes(a) }
    }
    #[inline]
    pub fn i32(self, b: &[u8]) -> i32 {
        self.u32(b) as i32
    }
    #[inline]
    pub fn u64(self, b: &[u8]) -> u64 {
        let a = [b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]];
        if self.0 { u64::from_be_bytes(a) } else { u64::from_le_bytes(a) }
    }
    #[inline]
    pub fn i64(self, b: &[u8]) -> i64 {
        self.u64(b) as i64
    }
    #[inline]
    pub fn f32(self, b: &[u8]) -> f32 {
        f32::from_bits(self.u32(b))
    }
    #[inline]
    pub fn f64(self, b: &[u8]) -> f64 {
        f64::from_bits(self.u64(b))
    }
}

/// An uncompressed `.blend` file with its blocks indexed.
pub struct BlendFile {
    pub data: Vec<u8>,
    pub header: Header,
    pub blocks: Vec<Block>,
    pub sdna: Sdna,
    /// Block index by old address: the first block written from it.
    /// Older files wrote real pointers, unique per file.  Blender 5 writes
    /// made-up addresses that are only unique within one data-block, so two
    /// data-blocks' DATA blocks can share one; `dups` keeps the others.
    by_old: HashMap<u64, u32>,
    dups: HashMap<u64, Vec<u32>>,
    /// The data-block (ID block) each block belongs to: the last ID block
    /// at or before it.
    owner: Vec<u32>,
    /// Blocks sorted by old address, for the rare pointer into the middle
    /// of a block.
    sorted: Vec<(u64, u32)>,
    /// Problems met while indexing, such as a file cut short.
    pub warnings: Vec<String>,
    pub packing: Packing,
}

impl BlendFile {
    /// Indexes an uncompressed file.  A file cut short keeps the blocks
    /// before the cut, but without the structure description at the end it
    /// can't be read at all.
    pub fn parse(data: Vec<u8>, packing: Packing) -> Result<BlendFile, LoadError> {
        let header = parse_header(&data)?;
        let mut blocks = Vec::new();
        let mut warnings = Vec::new();
        let mut pos = header.len;
        let mut dna = None;
        loop {
            let Some((block, start)) = read_block_header(&header, &data, pos) else {
                warnings.push("the file ends without its end marker; it may be cut short".into());
                break;
            };
            if block.code == ENDB {
                break;
            }
            let Some(end) = start.checked_add(block.len).filter(|&e| e <= data.len()) else {
                warnings.push("the file ends inside a block; it may be cut short".into());
                break;
            };
            if block.code == DNA1 {
                dna = Some((start, end));
            }
            blocks.push(block);
            pos = end;
        }
        let (start, end) =
            dna.ok_or_else(|| LoadError::invalid("no structure description (DNA1); the file may be cut short"))?;
        let sdna = Sdna::parse(&data[start..end], Endian(header.big_endian), header.pointer_size)?;
        let mut by_old: HashMap<u64, u32> = HashMap::with_capacity(blocks.len());
        let mut dups: HashMap<u64, Vec<u32>> = HashMap::new();
        let mut owner = Vec::with_capacity(blocks.len());
        let mut current = 0u32;
        for (i, b) in blocks.iter().enumerate() {
            if b.is_id() {
                current = i as u32;
            }
            owner.push(current);
            if b.old != 0 {
                match by_old.entry(b.old) {
                    std::collections::hash_map::Entry::Vacant(e) => {
                        e.insert(i as u32);
                    }
                    std::collections::hash_map::Entry::Occupied(_) => dups.entry(b.old).or_default().push(i as u32),
                }
            }
        }
        let mut sorted: Vec<(u64, u32)> = by_old.iter().map(|(&old, &i)| (old, i)).collect();
        sorted.sort_unstable();
        Ok(BlendFile { data, header, blocks, sdna, by_old, dups, owner, sorted, warnings, packing })
    }

    pub fn endian(&self) -> Endian {
        Endian(self.header.big_endian)
    }

    /// The block written from `old`, and the offset of `old` within it.
    /// Use [`BlendFile::find_near`] for pointers inside a data-block, which
    /// may point at blocks whose addresses other data-blocks reuse.
    pub fn find(&self, old: u64) -> Option<(u32, usize)> {
        if old == 0 {
            return None;
        }
        if let Some(&i) = self.by_old.get(&old) {
            return Some((i, 0));
        }
        let at = self.sorted.partition_point(|&(o, _)| o <= old).checked_sub(1)?;
        let (base, i) = self.sorted[at];
        let offset = usize::try_from(old - base).ok()?;
        (offset < self.blocks[i as usize].len).then_some((i, offset))
    }

    /// Like [`BlendFile::find`], for a pointer read from block `from`:
    /// among blocks sharing the address, the one in the same data-block.
    pub fn find_near(&self, old: u64, from: u32) -> Option<(u32, usize)> {
        if let Some(others) = self.dups.get(&old) {
            let want = self.owner.get(from as usize).copied();
            let first = self.by_old[&old];
            if self.owner.get(first as usize).copied() != want
                && let Some(&i) = others.iter().find(|&&i| self.owner.get(i as usize).copied() == want)
            {
                return Some((i, 0));
            }
        }
        self.find(old)
    }

    pub fn block(&self, i: u32) -> &Block {
        &self.blocks[i as usize]
    }

    pub fn block_bytes(&self, i: u32) -> &[u8] {
        let b = &self.blocks[i as usize];
        &self.data[b.start..b.start + b.len]
    }

    /// Reads a pointer stored at `pos`.
    pub fn read_ptr(&self, pos: usize) -> u64 {
        let n = self.header.pointer_size as usize;
        match self.data.get(pos..pos + n) {
            Some(b) if n == 4 => self.endian().u32(b) as u64,
            Some(b) => self.endian().u64(b),
            None => 0,
        }
    }
}

//endregion
