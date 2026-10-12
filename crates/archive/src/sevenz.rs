//! 7z archives (GZDoom's PK7).  The format is solid: files are packed into
//! blocks that can only be unpacked from their start, so reading one file
//! unpacks the files before it in its block, and reading several should
//! share the work ([`crate::Archive::read_many`]).  The decoder is the
//! `sevenz-rust2` crate, run inside a panic guard because it isn't ours;
//! blocks and dictionaries over the limits are refused before any unpacking.

use crate::source::{Source, SourceReader};
use crate::{Archive, ArchiveError, ArchiveKind, Entry, Extra, Limits, Parsed, Result, Slot};
use std::collections::HashMap;
use std::io::Read;
use std::sync::Mutex;

/// 7z blocks that claim more than this many unpacked bytes per packed byte
/// are refused: LZMA does reach thousands to one on runs of zeros, but game
/// data never does.
const MAX_BLOCK_RATIO: u64 = 20_000;

pub(crate) struct SevenZ {
    archive: sevenz_rust2::Archive,
    reader: Mutex<SourceReader>,
}

pub(crate) fn parse(source: &Source, limits: &Limits) -> Result<Parsed> {
    let mut reader = SourceReader { source: source.clone(), pos: 0 };
    let archive = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        sevenz_rust2::Archive::read(&mut reader, &sevenz_rust2::Password::empty())
    }))
    .map_err(|_| ArchiveError::invalid("7z: unreadable header"))?
    .map_err(|e| ArchiveError::invalid(format!("7z: {e}")))?;
    if archive.files.len() > limits.max_entries {
        return Err(ArchiveError::TooLarge(format!("a 7z archive of {} files", archive.files.len())));
    }
    let mut total_unpacked = 0u64;
    for block in &archive.blocks {
        let unpacked = block.get_unpack_size();
        if unpacked > limits.max_block_bytes {
            return Err(ArchiveError::TooLarge(format!("a 7z block of {unpacked} bytes")));
        }
        total_unpacked = total_unpacked.saturating_add(unpacked);
        for coder in &block.coders {
            let p = coder.properties();
            // LZMA2's dictionary size comes from one property byte.
            if coder.encoder_method_id() == [0x21] && !p.is_empty() {
                let bits = u32::from(p[0]);
                let dict = if bits > 40 { u64::MAX } else { u64::from(2 | (bits & 1)) << (bits / 2 + 11) };
                if dict > limits.max_dictionary_bytes {
                    return Err(ArchiveError::TooLarge("a 7z dictionary".into()));
                }
            }
        }
    }
    let total_packed: u64 = archive.pack_sizes().iter().sum::<u64>().max(1);
    if total_unpacked / total_packed > MAX_BLOCK_RATIO && total_unpacked > (64 << 20) {
        return Err(ArchiveError::TooLarge(format!(
            "a 7z archive that unpacks {total_unpacked} bytes from {total_packed}"
        )));
    }
    let mut entries = Vec::new();
    let mut slots = Vec::new();
    let mut too_long = 0usize;
    for (fi, f) in archive.files.iter().enumerate() {
        if f.is_directory() || f.is_anti_item() {
            continue;
        }
        let path = f.name().replace('\\', "/").trim_start_matches('/').to_string();
        if path.is_empty() {
            continue;
        }
        if path.len() > limits.max_name_bytes {
            too_long += 1;
            continue;
        }
        let top = path.split_once('/').map(|(t, _)| t);
        let namespace = top.and_then(crate::Namespace::of_folder);
        let data = crate::from_name(&path, namespace);
        entries.push(Entry {
            index: entries.len(),
            path,
            size: f.size(),
            stored_size: f.compressed_size,
            kind: data.kind(namespace),
            data,
            namespace,
            locked: false,
        });
        slots.push(Slot::SevenZ { file: fi });
    }
    let mut warnings = Vec::new();
    if too_long > 0 {
        warnings.push(format!("{too_long} files with names over {} bytes were left out", limits.max_name_bytes));
    }
    Ok(Parsed {
        kind: ArchiveKind::SevenZip,
        entries,
        slots,
        groups: Vec::new(),
        warnings,
        label: "PK7 (7z)".into(),
        extra: Extra::SevenZ(Box::new(SevenZ { archive, reader: Mutex::new(reader) })),
    })
}

/// Unpacks the block holding `wanted` files (by index in the archive's file
/// list) and calls `each` with every wanted file's bytes as they come, in
/// the block's order.  `each` returns `false` to stop the block.
fn unpack_block(
    z: &SevenZ,
    block: usize,
    wanted: &dyn Fn(usize) -> bool,
    each: &mut dyn FnMut(usize, &mut dyn Read) -> std::io::Result<bool>,
) -> Result<()> {
    let mut reader = z.reader.lock().map_err(|_| ArchiveError::invalid("7z: the reader is poisoned"))?;
    let password = sevenz_rust2::Password::empty();
    let run = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let first = z.archive.stream_map.block_first_file_index.get(block).copied().unwrap_or(0);
        let decoder = sevenz_rust2::BlockDecoder::new(1, block, &z.archive, &password, &mut *reader);
        let mut fi = first;
        decoder.for_each_entries(&mut |_entry: &sevenz_rust2::ArchiveEntry, r: &mut dyn Read| {
            let here = fi;
            fi += 1;
            if wanted(here) {
                Ok(each(here, r)?)
            } else {
                std::io::copy(r, &mut std::io::sink())?;
                Ok(true)
            }
        })
    }));
    match run {
        Ok(Ok(_)) => Ok(()),
        Ok(Err(e)) => Err(ArchiveError::invalid(format!("7z: {e}"))),
        Err(_) => Err(ArchiveError::invalid("7z: the decoder failed")),
    }
}

/// Streams one file.
pub(crate) fn stream(
    z: &SevenZ,
    file: usize,
    size: u64,
    max: u64,
    sink: &mut dyn FnMut(&[u8]) -> Result<bool>,
) -> Result<u64> {
    // A file without data is empty.
    let Some(block) = z.archive.stream_map.file_block_index.get(file).copied().flatten() else { return Ok(0) };
    let mut delivered = 0u64;
    let mut failure: Option<ArchiveError> = None;
    let mut buf = vec![0u8; 64 * 1024];
    unpack_block(z, block, &|f| f == file, &mut |_, r| {
        // The declared size bounds the output however the block behaves.
        let mut r = r.take(size.saturating_add(1));
        loop {
            let n = r.read(&mut buf)?;
            if n == 0 {
                break;
            }
            let room = max - delivered;
            let n = n.min(room.min(usize::MAX as u64) as usize);
            delivered += n as u64;
            if delivered > size {
                failure = Some(ArchiveError::invalid("a 7z file unpacks to more than it declares"));
                return Ok(false);
            }
            match sink(&buf[..n]) {
                Ok(true) if delivered < max => {}
                Ok(_) => return Ok(false),
                Err(e) => {
                    failure = Some(e);
                    return Ok(false);
                }
            }
        }
        Ok(false)
    })?;
    match failure {
        Some(e) => Err(e),
        None => Ok(delivered),
    }
}

/// Reads several files, unpacking each block once.
pub(crate) fn read_many(archive: &Archive, z: &SevenZ, wanted: &[usize]) -> Vec<Result<Vec<u8>>> {
    let mut out: Vec<Result<Vec<u8>>> = wanted.iter().map(|_| Err(ArchiveError::invalid("not read"))).collect();
    // File index -> positions in `wanted`.
    let mut by_file: HashMap<usize, Vec<usize>> = HashMap::new();
    let mut blocks: Vec<usize> = Vec::new();
    for (k, &i) in wanted.iter().enumerate() {
        let Some(entry) = archive.entry(i) else {
            out[k] = Err(ArchiveError::invalid("no such entry"));
            continue;
        };
        if entry.size > archive.limits().max_read {
            out[k] = Err(ArchiveError::TooLarge(format!("{} is {} bytes", entry.path, entry.size)));
            continue;
        }
        let Some(Slot::SevenZ { file }) = archive.slot(i) else { continue };
        match z.archive.stream_map.file_block_index.get(*file).copied().flatten() {
            Some(b) => {
                by_file.entry(*file).or_default().push(k);
                blocks.push(b);
            }
            None => out[k] = Ok(Vec::new()),
        }
    }
    blocks.sort_unstable();
    blocks.dedup();
    for b in blocks {
        let mut got: Vec<(usize, Vec<u8>)> = Vec::new();
        let _ = unpack_block(z, b, &|f| by_file.contains_key(&f), &mut |f, r| {
            let size = by_file[&f].first().and_then(|&k| archive.entry(wanted[k])).map_or(0, |e| e.size);
            let mut buf = Vec::new();
            r.take(size.saturating_add(1)).read_to_end(&mut buf)?;
            got.push((f, buf));
            Ok(true)
        });
        for (f, data) in got {
            let size = by_file[&f].first().and_then(|&k| archive.entry(wanted[k])).map_or(0, |e| e.size);
            for &k in &by_file[&f] {
                out[k] = if data.len() as u64 > size {
                    Err(ArchiveError::invalid("a 7z file unpacks to more than it declares"))
                } else {
                    Ok(data.clone())
                };
            }
        }
    }
    out
}
