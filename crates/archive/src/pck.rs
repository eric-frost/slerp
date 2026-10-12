//! Godot's resource packs (`.pck`), format versions 1 (Godot 3), 2 (Godot
//! 4.0 to 4.4), 3 (Godot 4.5 and 4.6) and 4 (Godot 4.7).  Versions 3 and 4
//! share one layout; their paths have no `res://` prefix.
//!
//! The header is `GDPC`, the format version and Godot's own version, then
//! (from version 2) flags and the offset the files' offsets count from, and
//! (version 3) the directory's offset.  The directory is a count and, for
//! each file, a path (its length padded to four bytes), an offset, a size,
//! an MD5 hash and (from version 2) flags.  Packs whose directory is
//! encrypted are refused with a message; files that are encrypted are
//! listed but not read.

use crate::source::{Le, Source, Stream};
use crate::{ArchiveError, ArchiveKind, Entry, Extra, Limits, Parsed, Result, Slot};

/// Header flags.
const DIR_ENCRYPTED: u32 = 1;
/// File flags.
const FILE_ENCRYPTED: u32 = 1;
const FILE_REMOVAL: u32 = 2;

pub(crate) fn parse(source: &Source, limits: &Limits) -> Result<Parsed> {
    let short = || ArchiveError::invalid("the PCK header is cut short");
    let mut head = [0u8; 32];
    let n = source.len().min(32) as usize;
    source.read_at(0, &mut head[..n]).map_err(|_| short())?;
    let h = Le(&head);
    let version = h.u32(4).ok_or_else(short)?;
    if !(1..=4).contains(&version) {
        return Err(ArchiveError::Unsupported(format!("Godot pack format version {version}")));
    }
    let (major, minor, patch) = (h.u32(8).unwrap_or(0), h.u32(12).unwrap_or(0), h.u32(16).unwrap_or(0));
    // 1. The rest of the header, by version, and where the directory is.
    let (flags, file_base, dir_at) = match version {
        1 => (0, 0u64, 20 + 16 * 4),
        2 => {
            let mut extra = [0u8; 12];
            source.read_at(20, &mut extra).map_err(|_| short())?;
            let e = Le(&extra);
            (e.u32(0).unwrap_or(0), e.u64(4).unwrap_or(0), 20 + 12 + 16 * 4)
        }
        _ => {
            let mut extra = [0u8; 20];
            source.read_at(20, &mut extra).map_err(|_| short())?;
            let e = Le(&extra);
            (e.u32(0).unwrap_or(0), e.u64(4).unwrap_or(0), e.u64(12).unwrap_or(0))
        }
    };
    if flags & DIR_ENCRYPTED != 0 {
        return Err(ArchiveError::Encrypted(
            "this Godot pack's directory is encrypted, so its files can't be listed".into(),
        ));
    }
    let mut count_bytes = [0u8; 4];
    source
        .read_at(dir_at, &mut count_bytes)
        .map_err(|_| ArchiveError::invalid("the PCK's directory lies outside the file"))?;
    let count = u32::from_le_bytes(count_bytes) as u64;
    if count > limits.max_entries as u64 {
        return Err(ArchiveError::TooLarge(format!("a PCK of {count} files")));
    }

    // 2. The records, read as a stream: the directory's length isn't known
    //    until it has been read.
    let mut dir = Stream::new(source, dir_at + 4, limits.max_directory_bytes);
    let mut entries = Vec::with_capacity((count as usize).min(65_536));
    let mut slots = Vec::with_capacity(entries.capacity());
    let mut outside = 0usize;
    let mut removed = 0usize;
    for _ in 0..count {
        let Some(path_len) = dir.u32() else { break };
        let path_len = path_len as usize;
        if path_len > limits.max_name_bytes + 4 {
            return Err(ArchiveError::TooLarge(format!("a path of {path_len} bytes in the PCK")));
        }
        let Some(raw) = dir.take(path_len).map(<[u8]>::to_vec) else { break };
        let (Some(offset), Some(size)) = (dir.u64(), dir.u64()) else { break };
        if dir.take(16).is_none() {
            break;
        }
        let file_flags = if version >= 2 { dir.u32().unwrap_or(0) } else { 0 };
        let end = raw.iter().position(|&b| b == 0).unwrap_or(raw.len());
        let name = String::from_utf8_lossy(&raw[..end]).replace('\\', "/");
        let path = name.strip_prefix("res://").unwrap_or(&name).trim_start_matches('/').to_string();
        if path.is_empty() || file_flags & FILE_REMOVAL != 0 {
            removed += 1;
            continue;
        }
        let start = offset.saturating_add(file_base);
        let inside = start.checked_add(size).is_some_and(|e| e <= source.len());
        let encrypted = file_flags & FILE_ENCRYPTED != 0;
        let slot = if encrypted {
            Slot::Locked("the file is encrypted".into())
        } else if !inside {
            outside += 1;
            Slot::Locked("the file lies outside the pack".into())
        } else {
            Slot::Stored { offset: start, len: size }
        };
        let head = if inside && !encrypted && count <= 100_000 {
            source.head(start, (size as usize).min(64))
        } else {
            Vec::new()
        };
        let magic = crate::sniff_magic(&head);
        let data = if matches!(magic, crate::DataKind::Unknown | crate::DataKind::Empty) {
            crate::from_name(&path, None)
        } else {
            magic
        };
        entries.push(Entry {
            index: entries.len(),
            path,
            size,
            stored_size: size,
            kind: data.kind(None),
            data,
            namespace: None,
            locked: matches!(slot, Slot::Locked(_)),
        });
        slots.push(slot);
    }
    let mut warnings = Vec::new();
    if outside > 0 {
        warnings.push(format!("{outside} files lie outside the pack and can't be read"));
    }
    if removed > 0 {
        warnings.push(format!("{removed} entries (removal markers or unusable paths) were left out"));
    }
    Ok(Parsed {
        kind: ArchiveKind::Pck,
        entries,
        slots,
        groups: Vec::new(),
        warnings,
        label: format!("Godot PCK (format {version}, Godot {major}.{minor}.{patch})"),
        extra: Extra::None,
    })
}
