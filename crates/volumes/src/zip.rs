//! A minimal reader for zip archives (`.npz` files): the central directory
//! only, with Zip64, and no decompression (callers inflate members
//! themselves, as a stream).

use crate::source::read_exact_at;
use std::fs::File;

/// One member of an archive.
#[derive(Clone, Debug)]
pub struct Entry {
    pub name: String,
    /// 0 stored, 8 deflate.
    pub method: u16,
    pub compressed: u64,
    pub size: u64,
    header_offset: u64,
}

/// The most members and the most central-directory bytes a file may claim.
const MAX_ENTRIES: usize = 1 << 16;
const MAX_DIRECTORY: u64 = 64 << 20;

fn u16le(b: &[u8], o: usize) -> u16 {
    b.get(o..o + 2).map_or(0, |s| u16::from_le_bytes([s[0], s[1]]))
}

fn u32le(b: &[u8], o: usize) -> u32 {
    b.get(o..o + 4).map_or(0, |s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}

fn u64le(b: &[u8], o: usize) -> u64 {
    b.get(o..o + 8).map_or(0, |s| u64::from_le_bytes(s.try_into().unwrap_or([0; 8])))
}

/// True when the bytes begin like a zip archive.
pub fn looks_like_zip(head: &[u8]) -> bool {
    head.starts_with(b"PK\x03\x04") || head.starts_with(b"PK\x05\x06")
}

/// Lists the members of the archive in `file`, which is `len` bytes long.
pub fn list(file: &File, len: u64) -> Result<Vec<Entry>, String> {
    // 1. The end-of-central-directory record, in the last 64 KB + 22 bytes.
    let tail_len = len.min(65535 + 22) as usize;
    let mut tail = vec![0u8; tail_len];
    read_exact_at(file, &mut tail, len - tail_len as u64).map_err(|e| e.to_string())?;
    let eocd = (0..=tail_len.saturating_sub(22))
        .rev()
        .find(|&i| tail[i..].starts_with(b"PK\x05\x06"))
        .ok_or("this isn't a zip archive")?;
    let mut count = u16le(&tail, eocd + 10) as u64;
    let mut dir_size = u32le(&tail, eocd + 12) as u64;
    let mut dir_offset = u32le(&tail, eocd + 16) as u64;
    // 2. Zip64 keeps the real numbers in a record before it.
    if (count == 0xffff || dir_size == 0xffff_ffff || dir_offset == 0xffff_ffff) && eocd >= 20 {
        let loc = eocd - 20;
        if tail[loc..].starts_with(b"PK\x06\x07") {
            let rec_at = u64le(&tail, loc + 8);
            let mut rec = [0u8; 56];
            read_exact_at(file, &mut rec, rec_at).map_err(|e| e.to_string())?;
            if rec.starts_with(b"PK\x06\x06") {
                count = u64le(&rec, 32);
                dir_size = u64le(&rec, 40);
                dir_offset = u64le(&rec, 48);
            }
        }
    }
    if dir_size > MAX_DIRECTORY || dir_offset.checked_add(dir_size).is_none_or(|e| e > len) {
        return Err("the archive's directory is damaged".into());
    }
    let mut dir = vec![0u8; dir_size as usize];
    read_exact_at(file, &mut dir, dir_offset).map_err(|e| e.to_string())?;
    // 3. The entries.
    let mut out = Vec::new();
    let mut at = 0usize;
    while at + 46 <= dir.len() && out.len() < MAX_ENTRIES && (out.len() as u64) < count.max(1) * 2 {
        if !dir[at..].starts_with(b"PK\x01\x02") {
            break;
        }
        let method = u16le(&dir, at + 10);
        let mut compressed = u32le(&dir, at + 20) as u64;
        let mut size = u32le(&dir, at + 24) as u64;
        let name_len = u16le(&dir, at + 28) as usize;
        let extra_len = u16le(&dir, at + 30) as usize;
        let comment_len = u16le(&dir, at + 32) as usize;
        let mut header_offset = u32le(&dir, at + 42) as u64;
        let name_end = at + 46 + name_len;
        let extra_end = name_end + extra_len;
        if extra_end + comment_len > dir.len() {
            break;
        }
        let name = String::from_utf8_lossy(&dir[at + 46..name_end]).into_owned();
        // Zip64 extra field: the fields that overflowed, in this order.
        let extra = &dir[name_end..extra_end];
        let mut e = 0;
        while e + 4 <= extra.len() {
            let (id, sz) = (u16le(extra, e), u16le(extra, e + 2) as usize);
            if id == 1 {
                let mut p = e + 4;
                for field in [&mut size, &mut compressed, &mut header_offset] {
                    let is_max = *field == 0xffff_ffff;
                    if is_max && p + 8 <= e + 4 + sz {
                        *field = u64le(extra, p);
                        p += 8;
                    }
                }
            }
            e += 4 + sz;
        }
        out.push(Entry { name, method, compressed, size, header_offset });
        at = extra_end + comment_len;
    }
    Ok(out)
}

impl Entry {
    /// Where the member's data begins, from its local header.
    pub fn data_start(&self, file: &File, len: u64) -> Result<u64, String> {
        let mut h = [0u8; 30];
        if self.header_offset.checked_add(30).is_none_or(|e| e > len) {
            return Err("a member's header is outside the file".into());
        }
        read_exact_at(file, &mut h, self.header_offset).map_err(|e| e.to_string())?;
        if &h[..4] != b"PK\x03\x04" {
            return Err("a member's header is damaged".into());
        }
        let start = self.header_offset + 30 + u16le(&h, 26) as u64 + u16le(&h, 28) as u64;
        if start.checked_add(self.compressed).is_none_or(|e| e > len) {
            return Err("a member runs past the end of the file".into());
        }
        Ok(start)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A zip with stored members, as a byte vector.
    pub fn build(members: &[(&str, &[u8])]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut dir = Vec::new();
        for (name, data) in members {
            let offset = out.len() as u32;
            out.extend_from_slice(b"PK\x03\x04");
            out.extend_from_slice(&[20, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
            out.extend_from_slice(&(data.len() as u32).to_le_bytes());
            out.extend_from_slice(&(data.len() as u32).to_le_bytes());
            out.extend_from_slice(&(name.len() as u16).to_le_bytes());
            out.extend_from_slice(&0u16.to_le_bytes());
            out.extend_from_slice(name.as_bytes());
            out.extend_from_slice(data);
            dir.extend_from_slice(b"PK\x01\x02");
            dir.extend_from_slice(&[20, 0, 20, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
            dir.extend_from_slice(&(data.len() as u32).to_le_bytes());
            dir.extend_from_slice(&(data.len() as u32).to_le_bytes());
            dir.extend_from_slice(&(name.len() as u16).to_le_bytes());
            dir.extend_from_slice(&[0; 12]);
            dir.extend_from_slice(&offset.to_le_bytes());
            dir.extend_from_slice(name.as_bytes());
        }
        let dir_offset = out.len() as u32;
        out.extend_from_slice(&dir);
        out.extend_from_slice(b"PK\x05\x06");
        out.extend_from_slice(&[0, 0, 0, 0]);
        out.extend_from_slice(&(members.len() as u16).to_le_bytes());
        out.extend_from_slice(&(members.len() as u16).to_le_bytes());
        out.extend_from_slice(&(dir.len() as u32).to_le_bytes());
        out.extend_from_slice(&dir_offset.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out
    }

    #[test]
    fn lists_stored_members() {
        let z = build(&[("a.npy", b"hello"), ("b.npy", b"world!!")]);
        let dir = std::env::temp_dir().join(format!("slerp-zip-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("t.zip");
        std::fs::write(&p, &z).unwrap();
        let f = File::open(&p).unwrap();
        let entries = list(&f, z.len() as u64).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[1].name, "b.npy");
        let start = entries[1].data_start(&f, z.len() as u64).unwrap() as usize;
        assert_eq!(&z[start..start + 7], b"world!!");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn garbage_is_an_error_not_a_panic() {
        let dir = std::env::temp_dir().join(format!("slerp-zip2-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("g.zip");
        for bytes in [vec![], vec![0u8; 100], b"PK\x05\x06".repeat(20), vec![0xffu8; 70000]] {
            std::fs::write(&p, &bytes).unwrap();
            let f = File::open(&p).unwrap();
            let _ = list(&f, bytes.len() as u64);
        }
        // A directory that claims to be enormous.
        let mut z = build(&[("a.npy", b"x")]);
        let n = z.len();
        z[n - 6..n - 2].copy_from_slice(&u32::MAX.to_le_bytes());
        std::fs::write(&p, &z).unwrap();
        let f = File::open(&p).unwrap();
        assert!(list(&f, z.len() as u64).is_err());
        std::fs::remove_dir_all(&dir).ok();
    }
}
