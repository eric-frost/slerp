//! Reading single members of a zip file by seeking: the end record, then
//! the central directory, then only the member asked for.  Procreate,
//! Sketch, Figma, Adobe XD, FreeCAD, Krita palettes, Procreate swatches and
//! SketchUp 2021+ files are zips that can hold hundreds of megabytes of
//! layers, while their preview is one small member.
//!
//! Sizes come from the central directory, so members written with data
//! descriptors (sizes only after the data) read fine.  Members are stored,
//! deflated or (Fusion's) Zstandard compressed.  Zip64 sizes and
//! offsets are understood.  Data put in front of the zip (SketchUp's
//! header) is allowed for: offsets are corrected by the difference between
//! where the directory says it starts and where it really ends.

use crate::source::{MAX_READ, Source};
use slerp_scene::LoadError;

const LOCAL: &[u8] = b"PK\x03\x04";
const CENTRAL: &[u8] = b"PK\x01\x02";
const END: &[u8] = b"PK\x05\x06";
const END64: &[u8] = b"PK\x06\x06";
const LOCATOR64: &[u8] = b"PK\x06\x07";

/// The end record (22 bytes) plus the longest comment it may carry.
const END_SEARCH: u64 = 22 + 0xFFFF;

/// The most entries listed.  A directory entry takes at least 46 bytes,
/// so the 64 MB read cap holds fewer than this anyway.
const MAX_ENTRIES: usize = 1 << 20;

/// True when `head` starts like a zip file.
pub fn is_zip(head: &[u8]) -> bool {
    head.starts_with(LOCAL)
}

/// The name of the first member, from its local header at the start.
pub fn first_name(head: &[u8]) -> Option<&[u8]> {
    let len = u16::from_le_bytes(head.get(26..28)?.try_into().ok()?) as usize;
    is_zip(head).then(|| head.get(30..30 + len)).flatten()
}

/// One member of the archive, as its central directory describes it.
#[derive(Clone, Debug)]
pub struct Entry {
    pub name: String,
    method: u16,
    packed: u64,
    pub size: u64,
    /// Where its local header starts, already corrected for data in front
    /// of the zip.
    local: u64,
}

/// A zip's directory.
pub struct Zip {
    pub entries: Vec<Entry>,
}

fn u16_at(b: &[u8], i: usize) -> u64 {
    u16::from_le_bytes([b[i], b[i + 1]]) as u64
}

fn u32_at(b: &[u8], i: usize) -> u64 {
    u32::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]]) as u64
}

fn u64_at(b: &[u8], i: usize) -> u64 {
    u64::from_le_bytes(b[i..i + 8].try_into().unwrap_or([0; 8]))
}

fn bad(what: &str) -> LoadError {
    LoadError::invalid(format!("zip: {what}"))
}

impl Zip {
    /// Reads the directory of the zip in `src`.
    pub fn open(src: &mut Source) -> Result<Zip, LoadError> {
        // 1. The end record, searched for backwards from the end.
        let len = src.len();
        if len < 22 {
            return Err(bad("too short"));
        }
        let tail_start = len.saturating_sub(END_SEARCH);
        let tail = src.exact(tail_start, len - tail_start)?;
        let at = (0..=tail.len() - 22).rev().find(|&i| &tail[i..i + 4] == END).ok_or_else(|| bad("no end record"))?;
        let end_pos = tail_start + at as u64;
        let e = &tail[at..at + 22];
        let (mut count, mut dir_size, mut dir_offset) = (u16_at(e, 10), u32_at(e, 12), u32_at(e, 16));
        let mut dir_end = end_pos;

        // 2. Zip64: the locator just before the end record points at a
        //    larger end record with 64-bit fields.
        if (count == 0xFFFF || dir_size == 0xFFFF_FFFF || dir_offset == 0xFFFF_FFFF) && end_pos >= 20 {
            let loc = src.exact(end_pos - 20, 20)?;
            if &loc[..4] == LOCATOR64 {
                let rec_at = u64_at(&loc, 8);
                // The record sits right before the locator; trust where it
                // really is over the offset when data precedes the zip.
                let guess = end_pos - 20;
                let rec_at = if rec_at < guess && src.exact(rec_at, 4).is_ok_and(|m| m == END64) {
                    rec_at
                } else {
                    guess.checked_sub(56).ok_or_else(|| bad("zip64 end record"))?
                };
                let r = src.exact(rec_at, 56)?;
                if &r[..4] != END64 {
                    return Err(bad("zip64 end record"));
                }
                (count, dir_size, dir_offset) = (u64_at(&r, 32), u64_at(&r, 40), u64_at(&r, 48));
                dir_end = rec_at;
            }
        }

        // 3. The directory ends where the end records start, so its real
        //    start tells how far everything was shifted.
        if dir_size > MAX_READ || dir_size > dir_end {
            return Err(LoadError::TooLarge("zip directory".into()));
        }
        let dir_start = dir_end - dir_size;
        let shift = dir_start as i128 - dir_offset as i128;
        let dir = src.exact(dir_start, dir_size)?;
        let mut entries = Vec::with_capacity((count as usize).min(dir.len() / 46).min(MAX_ENTRIES));
        let mut i = 0usize;
        while i + 46 <= dir.len() && entries.len() < MAX_ENTRIES {
            if &dir[i..i + 4] != CENTRAL {
                break;
            }
            let method = u16_at(&dir, i + 10) as u16;
            let (mut packed, mut size) = (u32_at(&dir, i + 20), u32_at(&dir, i + 24));
            let (name_len, extra_len, comment_len) =
                (u16_at(&dir, i + 28) as usize, u16_at(&dir, i + 30) as usize, u16_at(&dir, i + 32) as usize);
            let mut local = u32_at(&dir, i + 42);
            let name_end = i + 46 + name_len;
            let extra_end = name_end + extra_len;
            if extra_end > dir.len() {
                break;
            }
            let name = String::from_utf8_lossy(&dir[i + 46..name_end]).into_owned();
            // Zip64 extra field: only the fields whose 32-bit values are
            // all ones are present, in this order.
            let extra = &dir[name_end..extra_end];
            let mut j = 0;
            while j + 4 <= extra.len() {
                let (id, n) = (u16_at(extra, j), u16_at(extra, j + 2) as usize);
                let body = &extra[(j + 4).min(extra.len())..(j + 4 + n).min(extra.len())];
                if id == 1 {
                    let mut k = 0;
                    let mut next = |v: &mut u64| {
                        if *v == 0xFFFF_FFFF && k + 8 <= body.len() {
                            *v = u64_at(body, k);
                            k += 8;
                        }
                    };
                    next(&mut size);
                    next(&mut packed);
                    next(&mut local);
                }
                j += 4 + n;
            }
            let local = u64::try_from(local as i128 + shift).map_err(|_| bad("bad member offset"))?;
            entries.push(Entry { name, method, packed, size, local });
            i = extra_end + comment_len;
        }
        Ok(Zip { entries })
    }

    /// The member called `name`, matched exactly first and then ignoring
    /// ASCII case.
    pub fn find(&self, name: &str) -> Option<&Entry> {
        self.entries
            .iter()
            .find(|e| e.name == name)
            .or_else(|| self.entries.iter().find(|e| e.name.eq_ignore_ascii_case(name)))
    }

    /// The bytes of a member, inflated.  Members larger than `max` (as
    /// declared, or as they inflate) are refused, so a zip bomb costs at
    /// most `max` bytes.
    pub fn read(&self, src: &mut Source, entry: &Entry, max: u64) -> Result<Vec<u8>, LoadError> {
        if entry.size > max || entry.packed > max.max(1 << 20).saturating_mul(2) {
            return Err(LoadError::TooLarge(format!("{} in the zip", entry.name)));
        }
        let h = src.exact(entry.local, 30)?;
        if &h[..4] != LOCAL {
            return Err(bad("bad local header"));
        }
        let data_at = entry.local + 30 + u16_at(&h, 26) + u16_at(&h, 28);
        let data = src.exact(data_at, entry.packed)?;
        match entry.method {
            0 => Ok(data),
            8 => miniz_oxide::inflate::decompress_to_vec_with_limit(&data, entry.size.max(1) as usize)
                .map_err(|_| bad(&format!("bad deflate data in {}", entry.name))),
            // Zstandard, which Fusion uses since 2026; decompressing past
            // the declared size is an error, so a bomb stops there too.
            93 => zstd::bulk::decompress(&data, entry.size as usize)
                .map_err(|_| bad(&format!("bad Zstandard data in {}", entry.name))),
            m => Err(LoadError::Unsupported(format!("{} compressed with zip method {m}", entry.name))),
        }
    }

    /// Reads the member called `name`, if there is one.
    pub fn read_named(&self, src: &mut Source, name: &str, max: u64) -> Result<Option<Vec<u8>>, LoadError> {
        match self.find(name) {
            Some(e) => self.read(src, e, max).map(Some),
            None => Ok(None),
        }
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;

    /// A zip of stored members, for tests.
    pub fn stored_zip(members: &[(&str, &[u8])]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut dir = Vec::new();
        for (name, data) in members {
            let offset = out.len() as u32;
            let crc = 0u32;
            let fields = |sig: &[u8], central: bool| {
                let mut h = sig.to_vec();
                if central {
                    h.extend_from_slice(&20u16.to_le_bytes());
                }
                h.extend_from_slice(&20u16.to_le_bytes());
                h.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0, 0]);
                h.extend_from_slice(&crc.to_le_bytes());
                h.extend_from_slice(&(data.len() as u32).to_le_bytes());
                h.extend_from_slice(&(data.len() as u32).to_le_bytes());
                h.extend_from_slice(&(name.len() as u16).to_le_bytes());
                h.extend_from_slice(&0u16.to_le_bytes());
                if central {
                    h.extend_from_slice(&[0; 10]);
                    h.extend_from_slice(&offset.to_le_bytes());
                }
                h.extend_from_slice(name.as_bytes());
                h
            };
            out.extend(fields(LOCAL, false));
            out.extend_from_slice(data);
            dir.extend(fields(CENTRAL, true));
        }
        let dir_at = out.len() as u32;
        out.extend_from_slice(&dir);
        out.extend_from_slice(END);
        out.extend_from_slice(&[0; 4]);
        out.extend_from_slice(&(members.len() as u16).to_le_bytes());
        out.extend_from_slice(&(members.len() as u16).to_le_bytes());
        out.extend_from_slice(&(dir.len() as u32).to_le_bytes());
        out.extend_from_slice(&dir_at.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out
    }

    #[test]
    fn reads_members_with_data_in_front() {
        let zip = stored_zip(&[("a.txt", b"hello"), ("dir/B.png", b"picture")]);
        let mut prefixed = b"some header".to_vec();
        prefixed.extend_from_slice(&zip);
        for bytes in [zip, prefixed] {
            let mut src = Source::memory(bytes);
            let z = Zip::open(&mut src).unwrap();
            assert_eq!(z.read_named(&mut src, "a.txt", 100).unwrap().unwrap(), b"hello");
            assert_eq!(z.read_named(&mut src, "dir/b.png", 100).unwrap().unwrap(), b"picture");
            assert!(z.read_named(&mut src, "missing", 100).unwrap().is_none());
            assert!(z.read_named(&mut src, "a.txt", 2).is_err());
        }
    }

    #[test]
    fn damaged_zips_fail_cleanly() {
        let zip = stored_zip(&[("a.txt", b"hello world"), ("b.bin", &[7u8; 300])]);
        for cut in 0..zip.len() {
            let mut src = Source::memory(zip[..cut].to_vec());
            if let Ok(z) = Zip::open(&mut src) {
                for e in z.entries.clone() {
                    let _ = z.read(&mut src, &e, 1 << 20);
                }
            }
        }
        let mut seed = 0x1234_5678u32;
        for _ in 0..2000 {
            let mut copy = zip.clone();
            for _ in 0..3 {
                seed ^= seed << 13;
                seed ^= seed >> 17;
                seed ^= seed << 5;
                let i = seed as usize % copy.len();
                copy[i] = (seed >> 8) as u8;
            }
            let mut src = Source::memory(copy);
            if let Ok(z) = Zip::open(&mut src) {
                for e in z.entries.clone() {
                    let _ = z.read(&mut src, &e, 1 << 20);
                }
            }
        }
    }

    /// A zip64 archive: the central directory entry and the end record hold
    /// 0xFFFFFFFF placeholders, and the real values sit in extra fields, a
    /// zip64 end record and its locator, with data in front of the zip.
    #[test]
    fn reads_zip64_archives() {
        let picture = b"zip64 member data";
        let name = b"thumbnails/a.png";
        let prefix = b"HEADER-BEFORE-THE-ZIP".to_vec();
        let mut zip = prefix.clone();
        let local_at = zip.len() as u64;
        zip.extend_from_slice(LOCAL);
        zip.extend_from_slice(&[45, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        zip.extend_from_slice(&(picture.len() as u32).to_le_bytes());
        zip.extend_from_slice(&(picture.len() as u32).to_le_bytes());
        zip.extend_from_slice(&(name.len() as u16).to_le_bytes());
        zip.extend_from_slice(&0u16.to_le_bytes());
        zip.extend_from_slice(name);
        zip.extend_from_slice(picture);
        // Offsets in the directory count from the start of the zip proper,
        // after the prefix.
        let offset = local_at - prefix.len() as u64;
        let dir_at = zip.len() as u64;
        zip.extend_from_slice(CENTRAL);
        // Versions, flags, method, time, date and CRC.
        zip.extend_from_slice(&[45, 0, 45, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        zip.extend_from_slice(&u32::MAX.to_le_bytes());
        zip.extend_from_slice(&u32::MAX.to_le_bytes());
        zip.extend_from_slice(&(name.len() as u16).to_le_bytes());
        zip.extend_from_slice(&28u16.to_le_bytes());
        // No comment, disk 0, no attributes.
        zip.extend_from_slice(&[0; 10]);
        zip.extend_from_slice(&u32::MAX.to_le_bytes());
        zip.extend_from_slice(name);
        zip.extend_from_slice(&1u16.to_le_bytes());
        zip.extend_from_slice(&24u16.to_le_bytes());
        zip.extend_from_slice(&(picture.len() as u64).to_le_bytes());
        zip.extend_from_slice(&(picture.len() as u64).to_le_bytes());
        zip.extend_from_slice(&offset.to_le_bytes());
        let dir_size = zip.len() as u64 - dir_at;
        let end64_at = zip.len() as u64;
        zip.extend_from_slice(END64);
        zip.extend_from_slice(&44u64.to_le_bytes());
        zip.extend_from_slice(&[45, 0, 45, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        zip.extend_from_slice(&1u64.to_le_bytes());
        zip.extend_from_slice(&1u64.to_le_bytes());
        zip.extend_from_slice(&dir_size.to_le_bytes());
        zip.extend_from_slice(&(dir_at - prefix.len() as u64).to_le_bytes());
        zip.extend_from_slice(LOCATOR64);
        zip.extend_from_slice(&0u32.to_le_bytes());
        zip.extend_from_slice(&(end64_at - prefix.len() as u64).to_le_bytes());
        zip.extend_from_slice(&1u32.to_le_bytes());
        zip.extend_from_slice(END);
        zip.extend_from_slice(&[0; 4]);
        zip.extend_from_slice(&0xFFFFu16.to_le_bytes());
        zip.extend_from_slice(&0xFFFFu16.to_le_bytes());
        zip.extend_from_slice(&u32::MAX.to_le_bytes());
        zip.extend_from_slice(&u32::MAX.to_le_bytes());
        zip.extend_from_slice(&0u16.to_le_bytes());
        let mut src = Source::memory(zip.clone());
        let z = Zip::open(&mut src).unwrap();
        assert_eq!(z.entries.len(), 1);
        assert_eq!(z.read_named(&mut src, "thumbnails/A.png", 1000).unwrap().unwrap(), picture);
        // Without data in front, as writers usually make them.
        let plain = zip[prefix.len()..].to_vec();
        let mut src = Source::memory(plain);
        let z = Zip::open(&mut src).unwrap();
        assert_eq!(z.read_named(&mut src, "thumbnails/a.png", 1000).unwrap().unwrap(), picture);
        // Damage anywhere is an error or a different archive, never a panic.
        for cut in 0..zip.len() {
            let mut src = Source::memory(zip[..cut].to_vec());
            if let Ok(z) = Zip::open(&mut src) {
                for e in z.entries.clone() {
                    let _ = z.read(&mut src, &e, 1 << 20);
                }
            }
        }
    }

    /// A member declaring 4 GB inflated is refused before anything is
    /// allocated.
    #[test]
    fn huge_declared_sizes_are_refused() {
        let mut zip = stored_zip(&[("big", b"x")]);
        let dir = zip.windows(4).position(|w| w == CENTRAL).unwrap();
        zip[dir + 24..dir + 28].copy_from_slice(&u32::MAX.to_le_bytes());
        let mut src = Source::memory(zip);
        let z = Zip::open(&mut src).unwrap();
        let e = z.entries[0].clone();
        assert!(matches!(z.read(&mut src, &e, 64 << 20), Err(LoadError::TooLarge(_))));
    }

    /// Deflate data that inflates far beyond its declared size stops at
    /// the declared size.
    #[test]
    fn zip_bombs_stop_at_the_declared_size() {
        let big = vec![0u8; 8 << 20];
        let packed = miniz_oxide::deflate::compress_to_vec(&big, 9);
        let mut zip = stored_zip(&[("bomb", &packed)]);
        let dir = zip.windows(4).position(|w| w == CENTRAL).unwrap();
        zip[dir + 10] = 8;
        zip[dir + 24..dir + 28].copy_from_slice(&1000u32.to_le_bytes());
        let mut src = Source::memory(zip);
        let z = Zip::open(&mut src).unwrap();
        let e = z.entries[0].clone();
        assert!(z.read(&mut src, &e, 64 << 20).is_err());
    }
}
