//! OLE compound documents (Microsoft's "Compound File Binary" format,
//! [MS-CFB]) held in memory: a file system in a file, made of fixed-size
//! sectors chained through an allocation table, with a directory of named
//! streams.  Small streams live in a "mini stream" of 64-byte sectors with
//! its own table.  Every chain is followed with a step limit, since a
//! damaged table can loop, and every stream read is capped.

use slerp_scene::LoadError;

pub const MAGIC: &[u8] = b"\xD0\xCF\x11\xE0\xA1\xB1\x1A\xE1";

/// Sector numbers above this are special (free, end of chain, ...).
const MAX_REGULAR: u32 = 0xFFFF_FFFA;

fn bad(what: &str) -> LoadError {
    LoadError::invalid(format!("OLE file: {what}"))
}

fn u16_at(b: &[u8], i: usize) -> u16 {
    b.get(i..i + 2).map_or(0, |s| u16::from_le_bytes([s[0], s[1]]))
}

fn u32_at(b: &[u8], i: usize) -> u32 {
    b.get(i..i + 4).map_or(0, |s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}

/// An opened compound document.
pub struct Ole<'a> {
    data: &'a [u8],
    sector: usize,
    mini_cutoff: u64,
    /// The allocation table: the next sector of each sector.
    fat: Vec<u32>,
    mini_fat: Vec<u32>,
    /// The mini stream (the root entry's stream), read whole.
    mini_stream: Vec<u8>,
    pub entries: Vec<Entry>,
}

/// A directory entry.
#[derive(Clone, Debug)]
pub struct Entry {
    pub name: String,
    /// 1 storage, 2 stream, 5 root.
    pub kind: u8,
    pub start: u32,
    pub size: u64,
}

impl<'a> Ole<'a> {
    pub fn open(data: &'a [u8]) -> Result<Ole<'a>, LoadError> {
        if !data.starts_with(MAGIC) || data.len() < 512 {
            return Err(bad("wrong signature"));
        }
        let shift = u16_at(data, 30);
        if !(shift == 9 || shift == 12) || u16_at(data, 32) != 6 {
            return Err(bad("unusual sector size"));
        }
        let sector = 1usize << shift;
        let sectors_in_file = data.len().div_ceil(sector) as u64 + 1;
        let num_fat = u32_at(data, 44) as u64;
        let first_dir = u32_at(data, 48);
        let mini_cutoff = u32_at(data, 56) as u64;
        let first_mini_fat = u32_at(data, 60);
        let mut next_difat = u32_at(data, 68);
        let num_difat = u32_at(data, 72) as u64;
        if num_fat > sectors_in_file || num_difat > sectors_in_file {
            return Err(bad("allocation table larger than the file"));
        }
        let mut ole = Ole {
            data,
            sector,
            mini_cutoff,
            fat: Vec::new(),
            mini_fat: Vec::new(),
            mini_stream: Vec::new(),
            entries: Vec::new(),
        };
        // 1. The allocation table's sectors: 109 in the header, the rest in
        //    a chain of DIFAT sectors whose last entry links on.
        let mut fat_sectors: Vec<u32> = (0..109).map(|i| u32_at(data, 76 + i * 4)).take(num_fat as usize).collect();
        let per = sector / 4;
        let mut steps = 0;
        while (fat_sectors.len() as u64) < num_fat && next_difat <= MAX_REGULAR && steps <= num_difat {
            steps += 1;
            let Some(d) = ole.sector_bytes(next_difat) else { break };
            for i in 0..per - 1 {
                if (fat_sectors.len() as u64) < num_fat {
                    fat_sectors.push(u32_at(d, i * 4));
                }
            }
            next_difat = u32_at(d, (per - 1) * 4);
        }
        for s in fat_sectors {
            let Some(b) = ole.sector_bytes(s) else { continue };
            ole.fat.extend(b.chunks_exact(4).map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]])));
        }
        // 2. The directory.
        let dir = ole.read_chain(first_dir, u64::MAX)?;
        for e in dir.chunks_exact(128).take(1 << 20) {
            let name_len = (u16_at(e, 64) as usize).min(64);
            let units: Vec<u16> =
                e[..name_len.saturating_sub(2)].chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
            let size = if sector == 512 {
                u32_at(e, 120) as u64
            } else {
                u32_at(e, 120) as u64 | (u32_at(e, 124) as u64) << 32
            };
            ole.entries.push(Entry {
                name: String::from_utf16_lossy(&units),
                kind: e[66],
                start: u32_at(e, 116),
                size,
            });
        }
        // 3. The mini stream and its table.
        if let Some(root) = ole.entries.first().filter(|e| e.kind == 5).cloned() {
            ole.mini_stream = ole.read_chain(root.start, root.size).unwrap_or_default();
            let mf = ole.read_chain(first_mini_fat, u64::MAX).unwrap_or_default();
            ole.mini_fat = mf.chunks_exact(4).map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect();
        }
        Ok(ole)
    }

    fn sector_bytes(&self, s: u32) -> Option<&'a [u8]> {
        let start = (s as usize).checked_add(1)?.checked_mul(self.sector)?;
        let data: &'a [u8] = self.data;
        let end = start.checked_add(self.sector)?.min(data.len());
        data.get(start..end)
    }

    /// Reads up to `size` bytes of a chain of regular sectors.
    fn read_chain(&self, start: u32, size: u64) -> Result<Vec<u8>, LoadError> {
        let cap = size.min(self.data.len() as u64) as usize;
        let mut out = Vec::with_capacity(cap.min(64 << 20));
        let mut s = start;
        let mut steps = 0usize;
        while s <= MAX_REGULAR && out.len() < cap {
            steps += 1;
            if steps > self.fat.len() + 1 {
                return Err(bad("a sector chain loops"));
            }
            let b = self.sector_bytes(s).ok_or_else(|| bad("a sector lies beyond the end of the file"))?;
            out.extend_from_slice(b);
            s = *self.fat.get(s as usize).ok_or_else(|| bad("chain runs past the allocation table"))?;
        }
        out.truncate(cap);
        Ok(out)
    }

    /// The stream called `name` (compared without case), at most `max`
    /// bytes of it; `None` when there is no such stream.
    pub fn stream(&self, name: &str, max: u64) -> Result<Option<Vec<u8>>, LoadError> {
        let Some(e) = self.entries.iter().find(|e| e.kind == 2 && e.name.eq_ignore_ascii_case(name)) else {
            return Ok(None);
        };
        if e.size > max {
            return Err(LoadError::TooLarge(format!("the {name} stream is {} bytes", e.size)));
        }
        if e.size >= self.mini_cutoff {
            return self.read_chain(e.start, e.size).map(Some);
        }
        // A small stream: 64-byte sectors of the mini stream.
        let mut out = Vec::with_capacity(e.size as usize);
        let mut m = e.start;
        let mut steps = 0usize;
        while m <= MAX_REGULAR && (out.len() as u64) < e.size {
            steps += 1;
            if steps > self.mini_fat.len() + 1 {
                return Err(bad("a mini sector chain loops"));
            }
            let at = m as usize * 64;
            let b = self.mini_stream.get(at..at + 64).ok_or_else(|| bad("mini sector outside the mini stream"))?;
            out.extend_from_slice(b);
            m = *self.mini_fat.get(m as usize).ok_or_else(|| bad("mini chain runs past its table"))?;
        }
        out.truncate(e.size as usize);
        Ok(Some(out))
    }
}
