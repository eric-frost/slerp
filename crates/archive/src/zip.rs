//! Zip files, which GZDoom calls PK3 (and Quake 3 and its relatives too):
//! the central directory is read when the archive opens, and an entry's
//! local header only when the entry is.  Stored and deflated entries, zip64
//! and archives with data in front of them (self-extractors) are read.
//! Other compression methods (bzip2, LZMA, Zstandard) are listed as locked.

use crate::source::{Le, Source};
use crate::{ArchiveError, ArchiveKind, Entry, Extra, Limits, Namespace, Parsed, Result, Slot};

/// The end-of-central-directory record is at most this far from the end: its
/// 22 bytes and a comment of up to 65,535.
const MAX_TAIL: u64 = 22 + 0xFFFF;

pub(crate) fn parse(source: &Source, limits: &Limits) -> Result<Parsed> {
    let len = source.len();
    // 1. The end record, searched for backwards over a possible comment.
    let tail_len = len.min(MAX_TAIL);
    let tail_at = len - tail_len;
    let tail = source.bytes(tail_at, tail_len)?;
    let eocd = (0..tail.len().saturating_sub(21))
        .rev()
        .find(|&i| tail[i..].starts_with(b"PK\x05\x06"))
        .ok_or_else(|| ArchiveError::invalid("the zip has no end of central directory"))?;
    let e = Le(&tail[eocd..]);
    let bad = || ArchiveError::invalid("the zip's end record is cut short");
    let mut count = e.u16(10).ok_or_else(bad)? as u64;
    let mut dir_size = e.u32(12).ok_or_else(bad)? as u64;
    let mut dir_at = e.u32(16).ok_or_else(bad)? as u64;
    let eocd_at = tail_at + eocd as u64;

    // 2. Zip64 moves those fields to a record that a locator just before
    //    the end record points to.  The directory then ends where that record
    //    starts, not where the end record does.
    let mut dir_end = eocd_at;
    if (count == 0xFFFF || dir_size == 0xFFFF_FFFF || dir_at == 0xFFFF_FFFF) && eocd >= 20 {
        let loc = Le(&tail[eocd - 20..eocd]);
        if tail[eocd - 20..].starts_with(b"PK\x06\x07") {
            // The locator's offset, or (for an archive with data in front of
            // it) the place the record would be right before the locator.
            let named = loc.u64(8);
            let adjacent = eocd_at.checked_sub(20 + 56);
            for at in [named, adjacent].into_iter().flatten() {
                if let Ok(rec) = source.bytes(at, 56)
                    && rec.starts_with(b"PK\x06\x06")
                {
                    let rec_le = Le(&rec);
                    count = rec_le.u64(32).unwrap_or(count);
                    dir_size = rec_le.u64(40).unwrap_or(dir_size);
                    dir_at = rec_le.u64(48).unwrap_or(dir_at);
                    dir_end = at;
                    break;
                }
            }
        }
    }
    if count > limits.max_entries as u64 {
        return Err(ArchiveError::TooLarge(format!("a zip directory of {count} entries")));
    }
    if dir_size > limits.max_directory_bytes {
        return Err(ArchiveError::TooLarge(format!("a zip directory of {dir_size} bytes")));
    }
    // Data in front of the archive (a self-extractor) shifts every offset:
    // the directory ends where the end record starts.
    let base = if dir_at.checked_add(dir_size) != Some(dir_end) && dir_end >= dir_size {
        (dir_end - dir_size).saturating_sub(dir_at)
    } else {
        0
    };
    let dir_at = dir_at.checked_add(base).ok_or_else(|| ArchiveError::invalid("the zip directory is out of range"))?;
    let dir = source.bytes(dir_at, dir_size)?;
    let d = Le(&dir);

    // 3. The directory's records.
    let capacity = (count as usize).min(dir.len() / 46);
    let mut entries = Vec::with_capacity(capacity);
    let mut slots = Vec::with_capacity(capacity);
    let mut at = 0usize;
    let mut too_long = 0usize;
    let mut records = 0usize;
    for _ in 0..count {
        if d.slice(at, 4) != Some(b"PK\x01\x02") {
            break;
        }
        records += 1;
        let truncated = || ArchiveError::invalid("the zip directory is cut short");
        let flags = d.u16(at + 8).ok_or_else(truncated)?;
        let method = d.u16(at + 10).ok_or_else(truncated)?;
        let mut compressed = d.u32(at + 20).ok_or_else(truncated)? as u64;
        let mut size = d.u32(at + 24).ok_or_else(truncated)? as u64;
        let name_len = d.u16(at + 28).ok_or_else(truncated)? as usize;
        let extra_len = d.u16(at + 30).ok_or_else(truncated)? as usize;
        let comment_len = d.u16(at + 32).ok_or_else(truncated)? as usize;
        let mut local = d.u32(at + 42).ok_or_else(truncated)? as u64;
        let name = d.slice(at + 46, name_len).ok_or_else(truncated)?;
        let extra = d.slice(at + 46 + name_len, extra_len).ok_or_else(truncated)?;
        zip64_fields(extra, &mut size, &mut compressed, &mut local);
        at += 46 + name_len + extra_len + comment_len;

        let path = clean_name(name);
        // Folders have no data.
        if path.is_empty() || path.ends_with('/') {
            continue;
        }
        if path.len() > limits.max_name_bytes {
            too_long += 1;
            continue;
        }
        let top = path.split_once('/').map(|(f, _)| f);
        let namespace = top.and_then(Namespace::of_folder);
        let data = crate::from_name(&path, namespace);
        let slot = if flags & 1 != 0 {
            Slot::Locked("the entry is encrypted".into())
        } else if method != 0 && method != 8 {
            Slot::Locked(format!("zip compression method {method} isn't supported"))
        } else {
            Slot::Zip { local: local.saturating_add(base), method, compressed }
        };
        entries.push(Entry {
            index: entries.len(),
            path,
            size,
            stored_size: compressed,
            kind: data.kind(namespace),
            data,
            namespace,
            locked: matches!(slot, Slot::Locked(_)),
        });
        slots.push(slot);
    }
    let mut warnings = Vec::new();
    if too_long > 0 {
        warnings.push(format!("{too_long} entries with names over {} bytes were left out", limits.max_name_bytes));
    }
    if count > 0 && records == 0 {
        return Err(ArchiveError::invalid("the zip directory doesn't hold the entries it declares"));
    }
    Ok(Parsed {
        kind: ArchiveKind::Zip,
        entries,
        slots,
        groups: Vec::new(),
        warnings,
        label: "PK3 (zip)".into(),
        extra: Extra::None,
    })
}

/// Reads the 64-bit sizes and offset a zip64 extra field holds for each
/// 32-bit field that is all ones.
fn zip64_fields(extra: &[u8], size: &mut u64, compressed: &mut u64, local: &mut u64) {
    let e = Le(extra);
    let mut x = 0usize;
    while let (Some(id), Some(n)) = (e.u16(x), e.u16(x + 2)) {
        let body = e.slice(x + 4, n as usize).unwrap_or(&[]);
        if id == 1 {
            let b = Le(body);
            let mut k = 0usize;
            for field in [&mut *size, &mut *compressed, &mut *local] {
                if *field == 0xFFFF_FFFF
                    && let Some(v) = b.u64(k)
                {
                    *field = v;
                    k += 8;
                }
            }
            return;
        }
        x += 4 + n as usize;
    }
}

/// A stored name as a path: UTF-8, else the DOS code page 437 that zip
/// tools used before they wrote UTF-8; `\` becomes `/`, and leading slashes
/// go.
fn clean_name(raw: &[u8]) -> String {
    let text = match std::str::from_utf8(raw) {
        Ok(s) => s.to_string(),
        Err(_) => raw.iter().map(|&b| if b < 0x80 { b as char } else { CP437[b as usize - 0x80] }).collect(),
    };
    text.replace('\\', "/").trim_start_matches('/').to_string()
}

const CP437: [char; 128] = [
    'Ç', 'ü', 'é', 'â', 'ä', 'à', 'å', 'ç', 'ê', 'ë', 'è', 'ï', 'î', 'ì', 'Ä', 'Å', 'É', 'æ', 'Æ', 'ô', 'ö', 'ò', 'û',
    'ù', 'ÿ', 'Ö', 'Ü', '¢', '£', '¥', '₧', 'ƒ', 'á', 'í', 'ó', 'ú', 'ñ', 'Ñ', 'ª', 'º', '¿', '⌐', '¬', '½', '¼', '¡',
    '«', '»', '░', '▒', '▓', '│', '┤', '╡', '╢', '╖', '╕', '╣', '║', '╗', '╝', '╜', '╛', '┐', '└', '┴', '┬', '├', '─',
    '┼', '╞', '╟', '╚', '╔', '╩', '╦', '╠', '═', '╬', '╧', '╨', '╤', '╥', '╙', '╘', '╒', '╓', '╫', '╪', '┘', '┌', '█',
    '▄', '▌', '▐', '▀', 'α', 'ß', 'Γ', 'π', 'Σ', 'σ', 'µ', 'τ', 'Φ', 'Θ', 'Ω', 'δ', '∞', 'φ', 'ε', '∩', '≡', '±', '≥',
    '≤', '⌠', '⌡', '÷', '≈', '°', '∙', '·', '√', 'ⁿ', '²', '■', '\u{a0}',
];

/// Streams one entry: finds where its data starts through the local header,
/// then copies or inflates it.
#[allow(clippy::too_many_arguments)]
pub(crate) fn stream(
    source: &Source,
    limits: &Limits,
    name: &str,
    local: u64,
    method: u16,
    compressed: u64,
    size: u64,
    max: u64,
    sink: &mut dyn FnMut(&[u8]) -> Result<bool>,
) -> Result<u64> {
    if method == 8 && size == 0 {
        return Ok(0);
    }
    let mut header = [0u8; 30];
    source
        .read_at(local, &mut header)
        .map_err(|_| ArchiveError::invalid(format!("{name}: the local header is missing")))?;
    if !header.starts_with(b"PK\x03\x04") {
        return Err(ArchiveError::invalid(format!("{name}: the local header is damaged")));
    }
    let h = Le(&header);
    let skip = 30 + h.u16(26).unwrap_or(0) as u64 + h.u16(28).unwrap_or(0) as u64;
    let start = local.saturating_add(skip);
    match method {
        0 => {
            if compressed != size {
                return Err(ArchiveError::invalid(format!("{name}: stored with two different sizes")));
            }
            crate::read::copy(source, start, compressed, max, sink)
        }
        8 => {
            // Deflate cannot make more than about 1032 bytes of one, so a
            // directory claiming more is lying (a zip bomb's signature).
            if size > compressed.saturating_mul(limits.max_ratio).saturating_add(1024) {
                return Err(ArchiveError::TooLarge(format!(
                    "{name} claims {size} bytes from {compressed}, more than deflate can make"
                )));
            }
            crate::read::inflate_to(source, start, compressed, size, max, name, sink)
        }
        m => Err(ArchiveError::Unsupported(format!("{name}: zip compression method {m}"))),
    }
}

/// The name in the first local file header of a zip, from the file's first
/// bytes (about 30 plus the name's length), to recognise what a zip holds
/// without opening its directory: USDZ packages start with a USD layer.
pub fn first_name(head: &[u8]) -> Option<String> {
    if !head.starts_with(b"PK\x03\x04") {
        return None;
    }
    let h = Le(head);
    let len = h.u16(26)? as usize;
    Some(clean_name(h.slice(30, len)?))
}
