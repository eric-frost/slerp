//! The two ways bytes leave an archive: copied from a stretch of a file, and
//! inflated from a deflate stream.  Both deliver chunks to a sink that may
//! ask to stop, and neither allocates more than one chunk, whatever the
//! directory claims.

use crate::source::Source;
use crate::{ArchiveError, Result};
use miniz_oxide::inflate::stream::{InflateState, inflate};
use miniz_oxide::{DataFormat, MZError, MZFlush, MZStatus};

/// The chunk size for copies and for inflate's input and output.
const CHUNK: usize = 64 * 1024;

/// Copies `len` bytes at `offset` to `sink`, at most `max` of them.
pub(crate) fn copy(
    source: &Source,
    offset: u64,
    len: u64,
    max: u64,
    sink: &mut dyn FnMut(&[u8]) -> Result<bool>,
) -> Result<u64> {
    // Fail before delivering anything when the stretch isn't in the file.
    match offset.checked_add(len) {
        Some(end) if end <= source.len() => {}
        _ => return Err(ArchiveError::invalid("an entry runs past the end of the file")),
    }
    let want = len.min(max);
    let mut buf = vec![0u8; CHUNK.min(want as usize)];
    let mut done = 0u64;
    while done < want {
        let n = (want - done).min(CHUNK as u64) as usize;
        source.read_at(offset + done, &mut buf[..n])?;
        done += n as u64;
        if !sink(&buf[..n])? {
            break;
        }
    }
    Ok(done)
}

/// Inflates the raw deflate stream of `compressed` bytes at `at`, delivering
/// at most `max` bytes.  The stream must not produce more than `size` bytes
/// (the directory's claim), and when `max` allows reading to the end it must
/// produce exactly that many.
pub(crate) fn inflate_to(
    source: &Source,
    at: u64,
    compressed: u64,
    size: u64,
    max: u64,
    what: &str,
    sink: &mut dyn FnMut(&[u8]) -> Result<bool>,
) -> Result<u64> {
    match at.checked_add(compressed) {
        Some(end) if end <= source.len() => {}
        _ => return Err(ArchiveError::invalid(format!("{what}: the data runs past the end of the file"))),
    }
    let mut state = InflateState::new_boxed(DataFormat::Raw);
    let mut input = vec![0u8; CHUNK.min(compressed as usize).max(1)];
    let mut output = vec![0u8; CHUNK];
    let (mut filled, mut used) = (0usize, 0usize);
    let (mut next, mut remaining) = (at, compressed);
    let mut total = 0u64;
    loop {
        if used == filled && remaining > 0 {
            let n = remaining.min(input.len() as u64) as usize;
            source.read_at(next, &mut input[..n])?;
            next += n as u64;
            remaining -= n as u64;
            (filled, used) = (n, 0);
        }
        // Never `Finish`: miniz_oxide then insists on writing everything in
        // one call.  A stream cut short shows as a call that makes no
        // progress with all the input used.
        let r = inflate(&mut state, &input[used..filled], &mut output, MZFlush::None);
        used += r.bytes_consumed;
        if r.bytes_written > 0 {
            let now = total + r.bytes_written as u64;
            // Deliver what the caller asked for and the directory allows;
            // output beyond the declared size is a lie, but only matters
            // when the caller reads that far.
            let deliver_to = now.min(size).min(max);
            if deliver_to > total {
                let go = sink(&output[..(deliver_to - total) as usize])?;
                total = deliver_to;
                if !go || total >= max {
                    return Ok(total);
                }
            }
            if now > size {
                return Err(ArchiveError::invalid(format!("{what} unpacks to more than the {size} bytes it declares")));
            }
            total = now;
        }
        match r.status {
            Ok(MZStatus::StreamEnd) => break,
            Ok(_) | Err(MZError::Buf) => {}
            Err(e) => return Err(ArchiveError::invalid(format!("{what}: damaged deflate data ({e:?})"))),
        }
        // No bytes in or out with nothing left to feed: the data ends
        // before the stream does.
        if r.bytes_consumed == 0 && r.bytes_written == 0 && used == filled && remaining == 0 {
            return Err(ArchiveError::invalid(format!("{what}: the deflate data is cut short")));
        }
    }
    if total != size {
        return Err(ArchiveError::invalid(format!("{what} unpacks to {total} bytes, not the {size} it declares")));
    }
    Ok(total)
}
