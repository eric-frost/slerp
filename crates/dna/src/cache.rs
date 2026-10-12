//! Work kept on disk between runs, so a second open of the same DNA file
//! skips most of it: the data openriglogic read from the file (in `wire`'s
//! format, the bytes the reading process sends anyway) and the head's
//! skin weights refitted at each level of detail (see `fit`).  Reading a
//! MetaHuman head in a child process takes about 400 ms and refitting
//! about 260 ms; reading them back takes a few tens.
//!
//! Entries live in `$XDG_CACHE_HOME/slerp/dna/` (`~/Library/Caches/Slerp/dna`
//! on macOS, `%LOCALAPPDATA%\Slerp\cache\dna` on Windows), named by a hash
//! of the file's absolute path, size and modification time, of what was
//! read, and of the running executable's size and modification time, so
//! an edited file, a different request or another build of Slerp (whose
//! reader may read differently) simply misses.  `SLERP_DNA_NO_CACHE` turns
//! the cache off; tests never use it.  An entry repeats its key and
//! ends with a marker, and is written to a temporary name and renamed, so a
//! torn or foreign file is a miss, never wrong data; `wire` checks every
//! length while decoding, too.  The folder is kept under a size limit by
//! dropping the entries least recently used.
//!
//! Data from the cache skips the child process that guards against
//! crashes in openriglogic: it was written only after a child read the
//! same file, unchanged, without harm, and the bytes decode in Rust.

use std::io::Write;
use std::path::{Path, PathBuf};

/// Bump when what is cached, or how, changes.
const VERSION: u64 = 1;

const MAGIC: &[u8; 4] = b"SLDC";
const END: &[u8; 4] = b"CDLS";

/// The folder is trimmed to this size, oldest entries first.  A MetaHuman
/// head's data is about 100 MB.
const MAX_BYTES: u64 = 2 << 30;

/// Names one cached result.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Key(pub u64);

impl Key {
    /// The key of reading `path` as `what` says, or `None` when the file
    /// can't be looked at (then nothing is cached).
    pub fn of_file(path: &Path, what: &[u8]) -> Option<Key> {
        let meta = std::fs::metadata(path).ok()?;
        let mtime = meta.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?;
        let path = std::path::absolute(path).ok()?;
        let mut h = Fnv::new();
        h.write(&VERSION.to_le_bytes());
        h.write(&build_id().to_le_bytes());
        h.write(path.as_os_str().as_encoded_bytes());
        h.write(&[0]);
        h.write(&meta.len().to_le_bytes());
        h.write(&mtime.as_nanos().to_le_bytes());
        h.write(what);
        Some(Key(h.0))
    }

    /// A key derived from this one and `what`.
    pub fn with(self, what: &[u8]) -> Key {
        let mut h = Fnv::new();
        h.write(&self.0.to_le_bytes());
        h.write(what);
        Key(h.0)
    }
}

/// Tells builds of Slerp apart: the size and modification time of the
/// running executable, read once.
fn build_id() -> u64 {
    static ID: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    *ID.get_or_init(|| {
        let Some(meta) = std::env::current_exe().ok().and_then(|p| std::fs::metadata(p).ok()) else { return 0 };
        let mtime = meta.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok());
        let mut h = Fnv::new();
        h.write(&meta.len().to_le_bytes());
        h.write(&mtime.map_or(0, |d| d.as_nanos()).to_le_bytes());
        h.0
    })
}

fn enabled() -> bool {
    !cfg!(test) && std::env::var_os("SLERP_DNA_NO_CACHE").is_none()
}

/// 64-bit FNV-1a: stable across builds and Rust versions, unlike std's
/// hasher, which matters for names that outlive the process.
struct Fnv(u64);

impl Fnv {
    fn new() -> Fnv {
        Fnv(0xcbf2_9ce4_8422_2325)
    }

    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 ^= b as u64;
            self.0 = self.0.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
}

fn dir() -> Option<PathBuf> {
    #[cfg(not(any(windows, target_os = "macos")))]
    let base = std::env::var_os("XDG_CACHE_HOME")
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").filter(|s| !s.is_empty()).map(|h| PathBuf::from(h).join(".cache")))?
        .join("slerp");
    #[cfg(target_os = "macos")]
    let base = std::env::home_dir()?.join("Library/Caches/Slerp");
    #[cfg(windows)]
    let base = PathBuf::from(std::env::var_os("LOCALAPPDATA")?).join("Slerp").join("cache");
    Some(base.join("dna"))
}

fn path_of(dir: &Path, key: Key, kind: &str) -> PathBuf {
    dir.join(format!("{:016x}.{kind}", key.0))
}

/// The payload stored under `key`, if there is a whole one.
pub(crate) fn read(key: Key, kind: &str) -> Option<Vec<u8>> {
    if !enabled() {
        return None;
    }
    let path = path_of(&dir()?, key, kind);
    let bytes = std::fs::read(&path).ok()?;
    let body = bytes.strip_prefix(MAGIC.as_slice())?.strip_suffix(END.as_slice())?;
    let (stored, payload) = body.split_at_checked(8)?;
    if u64::from_le_bytes(stored.try_into().ok()?) != key.0 {
        return None;
    }
    // Used now: the size limit drops entries least recently used.
    let _ =
        std::fs::File::options().append(true).open(&path).and_then(|f| f.set_modified(std::time::SystemTime::now()));
    Some(payload.to_vec())
}

/// Stores `payload` under `key`, on a thread of its own so the caller
/// doesn't wait for the disk.  Failures only mean a later miss.
pub(crate) fn write(key: Key, kind: &'static str, payload: Vec<u8>) {
    if !enabled() {
        return;
    }
    let Some(dir) = dir() else { return };
    let _ = std::thread::Builder::new().name("slerp-dna-cache".into()).spawn(move || {
        let _ = write_now(&dir, key, kind, &payload);
        trim(&dir, MAX_BYTES);
    });
}

fn write_now(dir: &Path, key: Key, kind: &str, payload: &[u8]) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let path = path_of(dir, key, kind);
    let temp = dir.join(format!(".{:016x}.{kind}.{}", key.0, std::process::id()));
    let mut f = std::fs::File::create(&temp)?;
    f.write_all(MAGIC)?;
    f.write_all(&key.0.to_le_bytes())?;
    f.write_all(payload)?;
    f.write_all(END)?;
    drop(f);
    std::fs::rename(&temp, &path).inspect_err(|_| {
        let _ = std::fs::remove_file(&temp);
    })
}

/// Deletes the entries least recently used until the folder holds at most
/// `limit` bytes.
fn trim(dir: &Path, limit: u64) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let mut files: Vec<(std::time::SystemTime, u64, PathBuf)> = entries
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let m = e.metadata().ok()?;
            m.is_file().then(|| (m.modified().unwrap_or(std::time::UNIX_EPOCH), m.len(), e.path()))
        })
        .collect();
    let mut total: u64 = files.iter().map(|f| f.1).sum();
    files.sort();
    for (_, len, path) in files {
        if total <= limit {
            break;
        }
        if std::fs::remove_file(&path).is_ok() {
            total = total.saturating_sub(len);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_follow_the_file() {
        let dir = std::env::temp_dir().join(format!("slerp-dna-key-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("head.dna");
        std::fs::write(&f, b"one").unwrap();
        let a = Key::of_file(&f, b"viewer").unwrap();
        assert_eq!(Key::of_file(&f, b"viewer"), Some(a));
        assert_ne!(Key::of_file(&f, b"thumbnail"), Some(a), "what was read counts");
        std::fs::write(&f, b"three").unwrap();
        assert_ne!(Key::of_file(&f, b"viewer"), Some(a), "an edit misses");
        assert_ne!(a.with(b"lod 0"), a.with(b"lod 1"));
        assert_eq!(Key::of_file(&dir.join("missing.dna"), b""), None);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn entries_round_trip_and_damage_misses() {
        let dir = std::env::temp_dir().join(format!("slerp-dna-cache-{}", std::process::id()));
        let key = Key(0x1234);
        write_now(&dir, key, "data", b"payload").unwrap();
        let path = path_of(&dir, key, "data");
        let read_back = || {
            let bytes = std::fs::read(&path).ok()?;
            let body = bytes.strip_prefix(MAGIC.as_slice())?.strip_suffix(END.as_slice())?;
            let (stored, payload) = body.split_at_checked(8)?;
            (u64::from_le_bytes(stored.try_into().ok()?) == key.0).then(|| payload.to_vec())
        };
        assert_eq!(read_back().as_deref(), Some(b"payload".as_slice()));
        // A torn write has no end marker.
        let mut bytes = std::fs::read(&path).unwrap();
        bytes.truncate(bytes.len() - 2);
        std::fs::write(&path, &bytes).unwrap();
        assert_eq!(read_back(), None);
        // Trimming keeps the newest within the limit.
        write_now(&dir, Key(1), "data", &[0; 100]).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        write_now(&dir, Key(2), "data", &[0; 100]).unwrap();
        trim(&dir, 150);
        assert!(path_of(&dir, Key(2), "data").exists());
        assert!(!path_of(&dir, Key(1), "data").exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
