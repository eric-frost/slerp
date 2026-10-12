//! Just enough of the zip format to take a KML file out of a KMZ archive:
//! the central directory, stored and deflated entries, every size checked.

use slerp_scene::LoadError;

pub struct Entry {
    pub name: String,
    method: u16,
    compressed: u64,
    size: u64,
    offset: u64,
}

fn u16_at(d: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(d.get(at..at + 2)?.try_into().ok()?))
}

fn u32_at(d: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(d.get(at..at + 4)?.try_into().ok()?))
}

/// The archive's entries, from its central directory, or by walking its
/// local headers when that is missing (a truncated download).
pub fn entries(data: &[u8]) -> Vec<Entry> {
    central(data).unwrap_or_else(|| local(data))
}

fn central(data: &[u8]) -> Option<Vec<Entry>> {
    let from = data.len().saturating_sub(22 + 65535);
    let eocd = (from..data.len().saturating_sub(21)).rev().find(|&i| u32_at(data, i) == Some(0x0605_4b50))?;
    let count = u16_at(data, eocd + 10)? as usize;
    let mut at = u32_at(data, eocd + 16)? as usize;
    let mut out = Vec::new();
    for _ in 0..count {
        if u32_at(data, at)? != 0x0201_4b50 {
            break;
        }
        let method = u16_at(data, at + 10)?;
        let compressed = u32_at(data, at + 20)? as u64;
        let size = u32_at(data, at + 24)? as u64;
        let (n, x, c) =
            (u16_at(data, at + 28)? as usize, u16_at(data, at + 30)? as usize, u16_at(data, at + 32)? as usize);
        let offset = u32_at(data, at + 42)? as u64;
        let name = String::from_utf8_lossy(data.get(at + 46..at + 46 + n)?).into_owned();
        out.push(Entry { name, method, compressed, size, offset });
        at += 46 + n + x + c;
    }
    Some(out)
}

fn local(data: &[u8]) -> Vec<Entry> {
    let mut out = Vec::new();
    let mut at = 0usize;
    while let Some(0x0403_4b50) = u32_at(data, at) {
        let (Some(method), Some(compressed), Some(size), Some(n), Some(x)) = (
            u16_at(data, at + 8),
            u32_at(data, at + 18),
            u32_at(data, at + 22),
            u16_at(data, at + 26),
            u16_at(data, at + 28),
        ) else {
            break;
        };
        let Some(name) = data.get(at + 30..at + 30 + n as usize) else { break };
        out.push(Entry {
            name: String::from_utf8_lossy(name).into_owned(),
            method,
            compressed: compressed as u64,
            size: size as u64,
            offset: at as u64,
        });
        // Sizes may follow the data (bit 3); then nothing more can be found
        // this way.
        if compressed == 0 {
            break;
        }
        at += 30 + n as usize + x as usize + compressed as usize;
    }
    out
}

/// An entry's contents, at most `limit` bytes.
pub fn read(data: &[u8], e: &Entry, limit: usize) -> Result<Vec<u8>, LoadError> {
    let at = e.offset as usize;
    if u32_at(data, at) != Some(0x0403_4b50) {
        return Err(LoadError::invalid("broken zip entry"));
    }
    let n = u16_at(data, at + 26).unwrap_or(0) as usize;
    let x = u16_at(data, at + 28).unwrap_or(0) as usize;
    let start = at + 30 + n + x;
    let end = (start as u64 + e.compressed).min(data.len() as u64) as usize;
    let raw = data.get(start..end).ok_or_else(|| LoadError::invalid("zip entry past the end"))?;
    if e.size > limit as u64 {
        return Err(LoadError::TooLarge(format!("{} is {} bytes", e.name, e.size)));
    }
    match e.method {
        0 => Ok(raw.to_vec()),
        8 => match miniz_oxide::inflate::decompress_to_vec_with_limit(raw, limit) {
            Ok(v) => Ok(v),
            Err(err) if !err.output.is_empty() => Ok(err.output),
            Err(_) => Err(LoadError::invalid("zip entry doesn't inflate")),
        },
        m => Err(LoadError::Unsupported(format!("zip compression method {m}"))),
    }
}
