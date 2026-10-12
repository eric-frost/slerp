//! A small reader for OLE compound documents (Microsoft's "Compound File
//! Binary" format, [MS-CFB]): a file system in a file, made of fixed-size
//! sectors chained through an allocation table, with a directory of named
//! streams.  Small streams live in a "mini stream" of 64-byte sectors with
//! its own table.  Only what finding and reading one stream needs is
//! implemented, and every chain is followed with a step limit, since a
//! damaged table can loop.

use crate::source::Source;
use slerp_scene::LoadError;
use std::collections::HashMap;

pub const MAGIC: &[u8] = b"\xD0\xCF\x11\xE0\xA1\xB1\x1A\xE1";

/// Sector numbers with special meanings.
const END_OF_CHAIN: u32 = 0xFFFF_FFFE;
const MAX_REGULAR: u32 = 0xFFFF_FFFA;

/// Most directory entries scanned: the summary stream comes among the
/// first few, and 65,536 entries are eight megabytes of directory.
const MAX_ENTRIES: usize = 1 << 16;

fn bad(what: &str) -> LoadError {
    LoadError::invalid(format!("OLE file: {what}"))
}

fn u32_at(b: &[u8], i: usize) -> u32 {
    b.get(i..i + 4).map_or(0, |s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}

fn u64_at(b: &[u8], i: usize) -> u64 {
    b.get(i..i + 8).map_or(0, |s| u64::from_le_bytes(s.try_into().unwrap_or([0; 8])))
}

/// An opened compound document.
pub struct Ole<'a> {
    src: &'a mut Source,
    sector: u64,
    mini_sector: u64,
    mini_cutoff: u64,
    /// The sectors of the allocation table, from the header and the
    /// DIFAT chain.
    fat_sectors: Vec<u32>,
    /// Allocation table sectors already read.
    fat_cache: HashMap<u32, Vec<u8>>,
    first_dir: u32,
    first_mini_fat: u32,
    /// The longest chain any stream can have.
    max_chain: u64,
}

/// A directory entry for a stream.
pub struct Entry {
    pub start: u32,
    pub size: u64,
}

impl<'a> Ole<'a> {
    pub fn open(src: &'a mut Source) -> Result<Ole<'a>, LoadError> {
        let h = src.exact(0, 512)?;
        if !h.starts_with(MAGIC) {
            return Err(bad("wrong signature"));
        }
        let shift = u16::from_le_bytes([h[30], h[31]]);
        let mini_shift = u16::from_le_bytes([h[32], h[33]]);
        if !(shift == 9 || shift == 12) || mini_shift != 6 {
            return Err(bad("unusual sector size"));
        }
        let sector = 1u64 << shift;
        let num_fat = u32_at(&h, 44) as u64;
        let first_dir = u32_at(&h, 48);
        let mini_cutoff = u32_at(&h, 56) as u64;
        let first_mini_fat = u32_at(&h, 60);
        let mut next_difat = u32_at(&h, 68);
        let num_difat = u32_at(&h, 72) as u64;
        let max_chain = src.len() / sector + 2;
        if num_fat > max_chain || num_difat > max_chain {
            return Err(bad("allocation table larger than the file"));
        }
        // 1. The allocation table's own sectors: 109 in the header, the
        //    rest in a chain of DIFAT sectors whose last entry links on.
        let mut fat_sectors: Vec<u32> = (0..109).map(|i| u32_at(&h, 76 + i * 4)).take(num_fat as usize).collect();
        let per = (sector / 4) as usize;
        let mut steps = 0;
        while fat_sectors.len() < num_fat as usize && next_difat <= MAX_REGULAR && steps <= num_difat {
            steps += 1;
            let d = src.read_at((next_difat as u64 + 1) * sector, sector)?;
            for i in 0..per - 1 {
                if fat_sectors.len() < num_fat as usize {
                    fat_sectors.push(u32_at(&d, i * 4));
                }
            }
            next_difat = u32_at(&d, (per - 1) * 4);
        }
        Ok(Ole {
            src,
            sector,
            mini_sector: 1 << mini_shift,
            mini_cutoff,
            fat_sectors,
            fat_cache: HashMap::new(),
            first_dir,
            first_mini_fat,
            max_chain,
        })
    }

    /// The sector after `s` in its chain.
    fn next(&mut self, s: u32) -> Result<u32, LoadError> {
        let per = (self.sector / 4) as u32;
        let fat_index = s / per;
        let Some(&fat_sector) = self.fat_sectors.get(fat_index as usize) else {
            return Err(bad("chain runs past the allocation table"));
        };
        if !self.fat_cache.contains_key(&fat_index) {
            if fat_sector > MAX_REGULAR {
                return Err(bad("missing allocation table sector"));
            }
            let b = self.src.read_at((fat_sector as u64 + 1) * self.sector, self.sector)?;
            self.fat_cache.insert(fat_index, b);
        }
        Ok(u32_at(&self.fat_cache[&fat_index], ((s % per) * 4) as usize))
    }

    /// The sectors of the chain starting at `start`, at most `limit`.
    fn chain(&mut self, start: u32, limit: u64) -> Result<Vec<u32>, LoadError> {
        let mut out = Vec::new();
        let mut s = start;
        while s <= MAX_REGULAR {
            if out.len() as u64 >= limit.min(self.max_chain) {
                break;
            }
            out.push(s);
            s = self.next(s)?;
        }
        // Chains should end with END_OF_CHAIN; writers that end them with
        // another special value are forgiven.
        let _ = END_OF_CHAIN;
        Ok(out)
    }

    /// Reads up to `size` bytes of a chain of regular sectors.
    fn read_chain(&mut self, start: u32, size: u64) -> Result<Vec<u8>, LoadError> {
        let sectors = self.chain(start, size.div_ceil(self.sector))?;
        let mut out = Vec::with_capacity(size.min(crate::source::MAX_READ) as usize);
        for s in sectors {
            let b = self.src.read_at((s as u64 + 1) * self.sector, self.sector)?;
            out.extend_from_slice(&b);
            if out.len() as u64 >= size {
                break;
            }
        }
        out.truncate(size as usize);
        Ok(out)
    }

    /// Finds a stream by name (compared without case, as the format asks),
    /// and the root entry, which holds the mini stream.
    pub fn find(&mut self, name: &str) -> Result<Option<(Entry, Entry)>, LoadError> {
        let wanted: Vec<u16> = name.encode_utf16().map(upper).collect();
        let sectors = self.chain(self.first_dir, (MAX_ENTRIES as u64 * 128).div_ceil(self.sector))?;
        let mut root = None;
        let mut index = 0usize;
        for s in sectors {
            let b = self.src.read_at((s as u64 + 1) * self.sector, self.sector)?;
            for e in b.chunks_exact(128) {
                let name_len = u16::from_le_bytes([e[64], e[65]]) as usize;
                let kind = e[66];
                let size = if self.sector == 512 { u32_at(e, 120) as u64 } else { u64_at(e, 120) };
                let entry = Entry { start: u32_at(e, 116), size };
                if index == 0 && kind == 5 {
                    root = Some(entry);
                    index += 1;
                    continue;
                }
                index += 1;
                if kind != 2 || !(2..=64).contains(&name_len) {
                    continue;
                }
                let chars: Vec<u16> =
                    e[..name_len - 2].chunks_exact(2).map(|c| upper(u16::from_le_bytes([c[0], c[1]]))).collect();
                if chars == wanted {
                    let root = root.ok_or_else(|| bad("no root entry"))?;
                    return Ok(Some((entry, root)));
                }
            }
        }
        Ok(None)
    }

    /// Reads a stream found with `find`, at most `max` bytes of it.
    pub fn read(&mut self, entry: &Entry, root: &Entry, max: u64) -> Result<Vec<u8>, LoadError> {
        let size = entry.size.min(max);
        if entry.size >= self.mini_cutoff {
            return self.read_chain(entry.start, size);
        }
        // A small stream: mini sectors, chained through the mini table,
        // inside the mini stream, which is the root entry's chain.
        let mini_count = size.div_ceil(self.mini_sector);
        let per = self.sector / 4;
        let mut mini_fat: HashMap<u64, Vec<u8>> = HashMap::new();
        let fat_chain = self.chain(self.first_mini_fat, self.max_chain)?;
        let stream_chain = self.chain(root.start, root.size.div_ceil(self.sector))?;
        let mut out = Vec::with_capacity(size as usize);
        let mut m = entry.start;
        for _ in 0..mini_count {
            if m > MAX_REGULAR {
                break;
            }
            let offset = m as u64 * self.mini_sector;
            let Some(&s) = stream_chain.get((offset / self.sector) as usize) else {
                return Err(bad("mini sector outside the mini stream"));
            };
            let b = self.src.read_at((s as u64 + 1) * self.sector + offset % self.sector, self.mini_sector)?;
            out.extend_from_slice(&b);
            // The next mini sector.
            let fi = m as u64 / per;
            let Some(&fs) = fat_chain.get(fi as usize) else { break };
            if let std::collections::hash_map::Entry::Vacant(v) = mini_fat.entry(fi) {
                v.insert(self.src.read_at((fs as u64 + 1) * self.sector, self.sector)?);
            }
            m = u32_at(&mini_fat[&fi], ((m as u64 % per) * 4) as usize);
        }
        out.truncate(size as usize);
        Ok(out)
    }
}

/// Upper case for the ASCII letters, which is all stream names here use.
fn upper(c: u16) -> u16 {
    if (b'a' as u16..=b'z' as u16).contains(&c) { c - 32 } else { c }
}
