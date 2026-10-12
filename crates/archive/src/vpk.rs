//! Valve's VPK packs, versions 1 and 2.  The `*_dir.vpk` file holds a
//! directory tree (by extension, then folder, then file name) and the bytes
//! of small files; the rest live in numbered archives beside it
//! (`pak01_000.vpk`, `pak01_001.vpk`, ...).  A file's bytes are its preload
//! bytes from the directory tree followed by its stretch of an archive.

use crate::source::{Le, Source};
use crate::{Archive, ArchiveError, ArchiveKind, DataKind, Entry, Extra, Limits, Parsed, Result, Slot};
use std::path::{Path, PathBuf};

const SIGNATURE: u32 = 0x55aa_1234;
/// The archive index meaning "in the directory file itself".
const IN_DIRECTORY: u16 = 0x7fff;

/// What the directory file keeps for reading: the tree (preload bytes live
/// in it) and where the data stored in the directory file starts.
pub(crate) struct Tree {
    bytes: Vec<u8>,
    data_start: u64,
}

impl Tree {
    /// The numbered archives that exist beside `dir`, for watching.
    pub fn parts(&self, dir: &Path) -> Vec<PathBuf> {
        (0u16..4096).map_while(|n| archive_path(dir, n).filter(|p| p.is_file())).collect()
    }

    #[allow(clippy::too_many_arguments)]
    pub fn stream(
        &self,
        archive: &Archive,
        preload_at: usize,
        preload_len: u16,
        part: u16,
        offset: u64,
        len: u64,
        max: u64,
        sink: &mut dyn FnMut(&[u8]) -> Result<bool>,
    ) -> Result<u64> {
        let mut done = 0u64;
        // 1. The preload bytes, from the tree in memory.
        let pre = self
            .bytes
            .get(preload_at..preload_at + preload_len as usize)
            .ok_or_else(|| ArchiveError::invalid("a VPK file's preload bytes lie outside the tree"))?;
        let take = pre.len().min(max.min(usize::MAX as u64) as usize);
        if take > 0 {
            done += take as u64;
            if !sink(&pre[..take])? || done >= max {
                return Ok(done);
            }
        }
        // 2. The rest, from the directory file or a numbered archive.
        if len == 0 {
            return Ok(done);
        }
        let left = max.saturating_sub(done);
        if part == IN_DIRECTORY {
            let start = self.data_start.saturating_add(offset);
            done += crate::read::copy(archive.main_source(), start, len, left, sink)?;
        } else {
            let dir = archive.path().ok_or_else(|| {
                ArchiveError::Unsupported("a VPK in memory has no numbered archives beside it".into())
            })?;
            let path = archive_path(dir, part)
                .ok_or_else(|| ArchiveError::Unsupported("only a *_dir.vpk has numbered archives".into()))?;
            let source = Source::open(&path).map_err(|e| ArchiveError::invalid(format!("{}: {e}", path.display())))?;
            done += crate::read::copy(&source, offset, len, left, sink)?;
        }
        Ok(done)
    }
}

/// `pak01_dir.vpk`'s archive `n`: `pak01_00n.vpk`.
fn archive_path(dir: &Path, n: u16) -> Option<PathBuf> {
    let name = dir.file_name()?.to_str()?;
    let stem = name.strip_suffix("_dir.vpk").or_else(|| name.strip_suffix("_DIR.VPK"))?;
    Some(dir.with_file_name(format!("{stem}_{n:03}.vpk")))
}

/// Why reading the tree stopped.
enum TreeError {
    /// The bytes ran out in the middle of the tree.
    Short,
    Bad(ArchiveError),
}

struct TreeRead {
    entries: Vec<Entry>,
    slots: Vec<Slot>,
    too_long: usize,
    outside: usize,
}

/// Reads the tree: extensions, each with folders, each with files.  A lone
/// space stands for "none".
fn read_tree(tree: &[u8], data_start: u64, file_len: u64, limits: &Limits) -> std::result::Result<TreeRead, TreeError> {
    let t = Le(tree);
    let mut at = 0usize;
    let string = |at: &mut usize| -> std::result::Result<String, TreeError> {
        let rest = tree.get(*at..).ok_or(TreeError::Short)?;
        let end = rest.iter().position(|&c| c == 0).ok_or(TreeError::Short)?;
        let s = String::from_utf8_lossy(&rest[..end]).into_owned();
        *at += end + 1;
        Ok(s)
    };
    let mut entries: Vec<Entry> = Vec::new();
    let mut slots: Vec<Slot> = Vec::new();
    let mut too_long = 0usize;
    let mut outside = 0usize;
    loop {
        // A tree with nothing in it may lack even its closing byte.
        if at >= tree.len() {
            break;
        }
        let ext = string(&mut at)?;
        if ext.is_empty() {
            break;
        }
        loop {
            let folder = string(&mut at)?;
            if folder.is_empty() {
                break;
            }
            loop {
                let name = string(&mut at)?;
                if name.is_empty() {
                    break;
                }
                // CRC, preload length, archive index, offset, length, and a
                // 0xffff terminator; then the preload bytes.
                let preload_len = t.u16(at + 4).ok_or(TreeError::Short)?;
                let part = t.u16(at + 6).ok_or(TreeError::Short)?;
                let offset = t.u32(at + 8).ok_or(TreeError::Short)? as u64;
                let len = t.u32(at + 12).ok_or(TreeError::Short)? as u64;
                let preload_at = at + 18;
                at = preload_at + preload_len as usize;
                if at > tree.len() {
                    return Err(TreeError::Short);
                }
                if entries.len() >= limits.max_entries {
                    return Err(TreeError::Bad(ArchiveError::TooLarge(
                        "a VPK of more than the allowed number of files".into(),
                    )));
                }
                let mut path = String::new();
                if folder != " " {
                    path.push_str(&folder);
                    path.push('/');
                }
                path.push_str(&name);
                if ext != " " {
                    path.push('.');
                    path.push_str(&ext);
                }
                let path = path.replace('\\', "/").trim_start_matches('/').to_string();
                if path.len() > limits.max_name_bytes {
                    too_long += 1;
                    continue;
                }
                // Files stored in the directory file can be checked against
                // its length here; the numbered archives are checked when a
                // file is read.
                let inside = part != IN_DIRECTORY
                    || data_start.checked_add(offset).and_then(|s| s.checked_add(len)).is_some_and(|e| e <= file_len);
                if !inside {
                    outside += 1;
                }
                let data = crate::from_name(&path, None);
                let size = preload_len as u64 + len;
                entries.push(Entry {
                    index: entries.len(),
                    path,
                    size,
                    stored_size: size,
                    kind: data.kind(None),
                    data,
                    namespace: None,
                    locked: !inside,
                });
                slots.push(if inside {
                    Slot::Vpk { preload_at, preload_len, part, offset, len }
                } else {
                    Slot::Locked("the file lies outside the pack".into())
                });
            }
        }
    }
    Ok(TreeRead { entries, slots, too_long, outside })
}

pub(crate) fn parse(source: &Source, limits: &Limits) -> Result<Parsed> {
    let short = || ArchiveError::invalid("the VPK header is cut short");
    let mut head = [0u8; 28];
    let n = source.len().min(28) as usize;
    source.read_at(0, &mut head[..n]).map_err(|_| short())?;
    let h = Le(&head);
    let (sig, version, tree_len) =
        (h.u32(0).ok_or_else(short)?, h.u32(4).ok_or_else(short)?, h.u32(8).ok_or_else(short)?);
    if sig != SIGNATURE {
        return Err(ArchiveError::NotMine);
    }
    if !(1..=2).contains(&version) {
        return Err(ArchiveError::Unsupported(format!("VPK version {version}")));
    }
    let header_len: u64 = if version == 2 { 28 } else { 12 };
    if tree_len as u64 > limits.max_directory_bytes {
        return Err(ArchiveError::TooLarge(format!("a VPK directory tree of {tree_len} bytes")));
    }
    let mut tree = source
        .bytes(header_len, tree_len as u64)
        .map_err(|_| ArchiveError::invalid("the VPK's directory tree is cut short"))?;
    let data_start = header_len + tree_len as u64;

    // The tree is read as it is laid out, not as long as the header says:
    // some writers get the length wrong.  When the tree runs out inside a
    // record, more of the file is read in and the tree is read again.
    let tree_read = loop {
        match read_tree(&tree, data_start, source.len(), limits) {
            Ok(done) => break done,
            Err(TreeError::Short) => {
                let have = header_len + tree.len() as u64;
                let more = (tree.len() as u64).max(64 * 1024).min(source.len().saturating_sub(have));
                if more == 0 || tree.len() as u64 + more > limits.max_directory_bytes {
                    return Err(ArchiveError::invalid("the VPK's directory tree is cut short"));
                }
                tree.extend_from_slice(&source.bytes(have, more)?);
            }
            Err(TreeError::Bad(e)) => return Err(e),
        }
    };
    let TreeRead { mut entries, slots, too_long, outside } = tree_read;
    let mut warnings = Vec::new();
    if too_long > 0 {
        warnings.push(format!("{too_long} files with names over {} bytes were left out", limits.max_name_bytes));
    }
    if outside > 0 {
        warnings.push(format!("{outside} files lie outside the directory file and can't be read"));
    }
    // Magic is checked for what the name can't tell.
    for e in entries.iter_mut() {
        if e.data == DataKind::Unknown && e.size == 0 {
            e.data = DataKind::Empty;
        }
    }
    Ok(Parsed {
        kind: ArchiveKind::Vpk,
        entries,
        slots,
        groups: Vec::new(),
        warnings,
        label: format!("Valve VPK (version {version})"),
        extra: Extra::Vpk(Box::new(Tree { bytes: tree, data_start })),
    })
}
