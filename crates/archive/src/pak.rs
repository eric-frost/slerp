//! Quake's PAK files and its WAD2 (and Half-Life's WAD3) texture packs.
//!
//! A PAK is `PACK`, the offset and length of a directory, and 64-byte
//! records of a 56-byte path, an offset and a size.  Soldier of Fortune's
//! `SPAK` is the same with 128-byte records.  A WAD2 or WAD3 is a count and
//! the offset of a directory of 32-byte records with a size, a type byte
//! and a 16-byte name; the type says whether a lump is a texture, a picture
//! or a palette.

use crate::source::{Le, Source};
use crate::{ArchiveError, ArchiveKind, DataKind, Entry, Extra, Limits, Parsed, Result, Slot};

/// A NUL-padded name as text, with `\` as `/` and no leading slash.
fn path_of(raw: &[u8]) -> String {
    let end = raw.iter().position(|&b| b == 0).unwrap_or(raw.len());
    let text: String = raw[..end].iter().map(|&b| if b < 0x20 || b == 0x7F { '?' } else { b as char }).collect();
    text.replace('\\', "/").trim_start_matches('/').to_string()
}

pub(crate) fn parse(source: &Source, limits: &Limits) -> Result<Parsed> {
    let mut header = [0u8; 12];
    source.read_at(0, &mut header).map_err(|_| ArchiveError::invalid("the PAK header is cut short"))?;
    let sof = header.starts_with(b"SPAK");
    let record = if sof { 128u64 } else { 64 };
    let h = Le(&header);
    let (dir, dir_len) = (h.u32(4).unwrap_or(0) as u64, h.u32(8).unwrap_or(0) as u64);
    // A git pack file also starts with PACK; its "directory offset" is a
    // small version number.
    if dir < 12 || dir_len % record != 0 || dir.checked_add(dir_len).is_none_or(|e| e > source.len()) {
        return Err(ArchiveError::NotMine);
    }
    let count = (dir_len / record) as usize;
    if count > limits.max_entries {
        return Err(ArchiveError::TooLarge(format!("a PAK of {count} files")));
    }
    let table = source.bytes(dir, dir_len)?;
    let t = Le(&table);
    let name_bytes = record as usize - 8;
    let mut entries = Vec::with_capacity(count);
    let mut slots = Vec::with_capacity(count);
    let mut outside = 0usize;
    for i in 0..count {
        let at = i * record as usize;
        let path = path_of(&table[at..at + name_bytes]);
        if path.is_empty() {
            continue;
        }
        let offset = t.u32(at + name_bytes).unwrap_or(0) as u64;
        let size = t.u32(at + name_bytes + 4).unwrap_or(0) as u64;
        let inside = offset.checked_add(size).is_some_and(|e| e <= source.len());
        let head = if inside && count <= 100_000 { source.head(offset, (size as usize).min(4224)) } else { Vec::new() };
        let data = quake_file(&path, &head, size);
        let slot = if inside {
            Slot::Stored { offset, len: size }
        } else {
            outside += 1;
            Slot::Locked("the file lies outside the PAK".into())
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
        warnings.push(format!("{outside} files lie outside the PAK and can't be read"));
    }
    Ok(Parsed {
        kind: ArchiveKind::Pak,
        entries,
        slots,
        groups: Vec::new(),
        warnings,
        label: if sof { "Soldier of Fortune PAK" } else { "Quake PAK" }.into(),
        extra: Extra::None,
    })
}

/// A file in a PAK: by its bytes where they have a signature, else by its
/// name.  Quake's `.lmp` files are pictures (or the palette, or colour
/// maps), which only their size and place tell apart.
fn quake_file(path: &str, head: &[u8], size: u64) -> DataKind {
    let magic = crate::sniff_magic(head);
    if !matches!(magic, DataKind::Unknown | DataKind::Empty) {
        return magic;
    }
    let lower = path.to_ascii_lowercase();
    let name = lower.rsplit('/').next().unwrap_or(&lower);
    if name == "palette.lmp" && size == 768 {
        return DataKind::Palette768;
    }
    if name == "colormap.lmp" {
        return DataKind::Colormap;
    }
    if name.ends_with(".lmp") && crate::quake_picture_size(head).is_some_and(|(w, h)| 8 + w as u64 * h as u64 == size) {
        return DataKind::QuakePicture;
    }
    let by_name = crate::from_extension(name);
    if by_name != DataKind::Unknown {
        return by_name;
    }
    if size == 0 {
        return DataKind::Empty;
    }
    if crate::looks_like_text(head) { DataKind::Text } else { DataKind::Unknown }
}

pub(crate) fn parse_wad(source: &Source, limits: &Limits, kind: ArchiveKind) -> Result<Parsed> {
    let mut header = [0u8; 12];
    source.read_at(0, &mut header).map_err(|_| ArchiveError::invalid("the WAD header is cut short"))?;
    let h = Le(&header);
    let (count, dir) = (h.u32(4).unwrap_or(0) as u64, h.u32(8).unwrap_or(0) as u64);
    if count > limits.max_entries as u64 {
        return Err(ArchiveError::TooLarge(format!("a WAD of {count} lumps")));
    }
    let table = source
        .bytes(dir, count * 32)
        .map_err(|_| ArchiveError::invalid("the WAD's directory lies outside the file"))?;
    let t = Le(&table);
    let half_life = kind == ArchiveKind::Wad3;
    let mut entries = Vec::with_capacity(count as usize);
    let mut slots = Vec::with_capacity(count as usize);
    let mut outside = 0usize;
    for i in 0..count as usize {
        let at = i * 32;
        let offset = t.u32(at).unwrap_or(0) as u64;
        let disk_size = t.u32(at + 4).unwrap_or(0) as u64;
        let size = t.u32(at + 8).unwrap_or(0) as u64;
        let type_byte = t.u8(at + 12).unwrap_or(0);
        let compression = t.u8(at + 13).unwrap_or(0);
        let path = path_of(&table[at + 16..at + 32]);
        if path.is_empty() {
            continue;
        }
        let inside = offset.checked_add(disk_size).is_some_and(|e| e <= source.len());
        let data = wad_lump(half_life, type_byte, &path, size);
        let slot = if compression != 0 {
            Slot::Locked("the lump is compressed, which no game uses".into())
        } else if !inside {
            outside += 1;
            Slot::Locked("the lump lies outside the file".into())
        } else {
            Slot::Stored { offset, len: size.min(disk_size) }
        };
        entries.push(Entry {
            index: entries.len(),
            path,
            size,
            stored_size: disk_size,
            kind: data.kind(None),
            data,
            namespace: None,
            locked: matches!(slot, Slot::Locked(_)),
        });
        slots.push(slot);
    }
    let mut warnings = Vec::new();
    if outside > 0 {
        warnings.push(format!("{outside} lumps lie outside the file and can't be read"));
    }
    Ok(Parsed { kind, entries, slots, groups: Vec::new(), warnings, label: kind.label().into(), extra: Extra::None })
}

/// A WAD2 or WAD3 lump's format from its type byte.
fn wad_lump(half_life: bool, type_byte: u8, name: &str, size: u64) -> DataKind {
    if size == 0 {
        return DataKind::Empty;
    }
    match (half_life, type_byte) {
        (_, b'@') if size == 768 || name.eq_ignore_ascii_case("palette") => DataKind::Palette768,
        (false, b'D') if name.eq_ignore_ascii_case("conchars") => DataKind::Unknown,
        (false, b'D') => DataKind::Miptex,
        (true, b'C') => DataKind::Wad3Miptex,
        (false, b'B') => DataKind::QuakePicture,
        (true, b'B') | (true, b'@') => DataKind::Wad3Picture,
        (true, b'F') => DataKind::Wad3Font,
        _ => DataKind::Unknown,
    }
}
