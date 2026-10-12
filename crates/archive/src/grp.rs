//! The Build engine's group files (Duke Nukem 3D, Blood, Shadow Warrior):
//! `KenSilverman`, a file count, then a 16-byte record of name and size for
//! each file, and the files' bytes one after another.

use crate::source::{Le, Source};
use crate::{ArchiveError, ArchiveKind, DataKind, Entry, Extra, Limits, Parsed, Result, Slot};

pub(crate) fn parse(source: &Source, limits: &Limits) -> Result<Parsed> {
    let mut header = [0u8; 16];
    source.read_at(0, &mut header).map_err(|_| ArchiveError::invalid("the GRP header is cut short"))?;
    let count = Le(&header).u32(12).unwrap_or(0) as u64;
    if count > limits.max_entries as u64 {
        return Err(ArchiveError::TooLarge(format!("a GRP of {count} files")));
    }
    let table = source
        .bytes(16, count * 16)
        .map_err(|_| ArchiveError::invalid("the GRP's file table runs past the end of the file"))?;
    let t = Le(&table);
    let mut at = 16 + count * 16;
    let mut entries = Vec::with_capacity(count as usize);
    let mut slots = Vec::with_capacity(count as usize);
    let mut outside = 0usize;
    for i in 0..count as usize {
        let raw = &table[i * 16..i * 16 + 12];
        let end = raw.iter().position(|&b| b == 0).unwrap_or(12);
        let path: String = raw[..end].iter().map(|&b| if b < 0x20 || b == 0x7F { '?' } else { b as char }).collect();
        let size = t.u32(i * 16 + 12).unwrap_or(0) as u64;
        let offset = at;
        at = at.saturating_add(size);
        if path.is_empty() {
            continue;
        }
        let inside = offset.checked_add(size).is_some_and(|e| e <= source.len());
        let head = if inside && count <= 100_000 { source.head(offset, (size as usize).min(64)) } else { Vec::new() };
        let magic = crate::sniff_magic(&head);
        let data =
            if matches!(magic, DataKind::Unknown | DataKind::Empty) { crate::from_name(&path, None) } else { magic };
        let slot = if inside {
            Slot::Stored { offset, len: size }
        } else {
            outside += 1;
            Slot::Locked("the file lies outside the GRP".into())
        };
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
        slots.push(slot);
    }
    let mut warnings = Vec::new();
    if outside > 0 {
        warnings.push(format!("{outside} files lie outside the GRP and can't be read"));
    }
    Ok(Parsed {
        kind: ArchiveKind::Grp,
        entries,
        slots,
        groups: Vec::new(),
        warnings,
        label: "Build GRP".into(),
        extra: Extra::None,
    })
}
