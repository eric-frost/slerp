//! Reading a very large text G-code file for a thumbnail without reading
//! all of it.
//!
//! A file manager gives a thumbnail about two seconds, and a G-code file of
//! hundreds of megabytes takes longer than that just to scan.  A picture of
//! the print doesn't need every layer, though, and slicers mark where each
//! layer starts, so the file can be read by jumping:
//!
//! 1. The head is read whole: the start G-code (which sets the units and
//!    whether the extruder counts absolute or relative), the first layers,
//!    and the settings that slicers such as Cura and Bambu Studio write
//!    first.
//! 2. Single layers are read from evenly spaced places: jump, skip to the
//!    next layer comment, and read up to the one after.  The parser is told
//!    of each jump (`Parser::skip`), since the tool's position isn't known
//!    from the layer before.
//! 3. The tail is read from its first layer comment to the end: the top of
//!    the print, the end G-code, and the settings and estimates that
//!    PrusaSlicer and OrcaSlicer write last.
//!
//! The layers read are then drawn as thick as the gaps they stand for
//! (`Toolpath::sampled`), so walls stay solid.  A file without layer
//! comments can't be sampled, and is read whole.

use super::Parser;
use super::parse::is_layer_marker;
use std::fs::File;
use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::Path;

/// Files smaller than this are read whole: sampling is for files whose
/// scan would take noticeable time.
pub const SAMPLE_FILE_BYTES: u64 = 48 << 20;
/// The head read whole.
const HEAD_BYTES: u64 = 3 << 20;
/// The tail read from its first layer comment.
const TAIL_BYTES: u64 = 2 << 20;
/// About how many bytes of layers are read from the middle, at most; a file
/// not much over `SAMPLE_FILE_BYTES` gives a quarter of itself.
const SAMPLE_BYTES: u64 = 24 << 20;
/// The fewest and most layers sampled from the middle.
const MIN_WINDOWS: u64 = 16;
const MAX_WINDOWS: u64 = 600;
/// How far past a jump to look for a layer comment, and how much of one
/// layer to read.  A layer or a search that runs on longer than this is
/// something other than G-code.
const SEARCH_BYTES: u64 = 2 << 20;
const LAYER_BYTES: u64 = 8 << 20;
/// The longest line read; longer ones (settings written as JSON) are cut.
const MAX_LINE: u64 = 64 << 10;

/// Reads lines from a file and keeps track of where in it they are.
struct Lines {
    reader: BufReader<File>,
    buf: Vec<u8>,
    /// The offset of the next line.
    pos: u64,
}

impl Lines {
    fn open(path: &Path) -> io::Result<Lines> {
        Ok(Lines { reader: BufReader::with_capacity(256 << 10, File::open(path)?), buf: Vec::new(), pos: 0 })
    }

    /// The next line, without its line break, or `None` at the end.
    fn next(&mut self) -> io::Result<Option<&[u8]>> {
        self.buf.clear();
        let n = (&mut self.reader).take(MAX_LINE).read_until(b'\n', &mut self.buf)?;
        if n == 0 {
            return Ok(None);
        }
        self.pos += n as u64;
        while matches!(self.buf.last(), Some(b'\n' | b'\r')) {
            self.buf.pop();
        }
        Ok(Some(&self.buf))
    }

    /// Moves to `offset`, then on to the start of the next line.
    fn jump(&mut self, offset: u64) -> io::Result<()> {
        self.reader.seek(SeekFrom::Start(offset))?;
        self.pos = offset;
        if offset > 0 {
            self.next()?;
        }
        Ok(())
    }
}

/// Whether the start of the file has layer comments, which sampling needs
/// to find where layers begin.
pub fn has_layer_markers(path: &Path) -> io::Result<bool> {
    let mut lines = Lines::open(path)?;
    while lines.pos < HEAD_BYTES {
        match lines.next()? {
            Some(l) if is_layer_marker(l) => return Ok(true),
            Some(_) => {}
            None => break,
        }
    }
    Ok(false)
}

/// Feeds `parser` the head, a sample of layers from the middle, and the
/// tail of the file at `path`, which is `len` bytes long.  Returns how many
/// layers were sampled from the middle.
pub fn read_sampled(path: &Path, len: u64, parser: &mut Parser) -> io::Result<usize> {
    let mut lines = Lines::open(path)?;

    // 1. The head, counting its layers to learn how large they are.
    let mut head_layers = 0u64;
    while lines.pos < HEAD_BYTES {
        let Some(l) = lines.next()? else { return Ok(0) };
        head_layers += is_layer_marker(l) as u64;
        parser.line(l);
    }
    let head_end = lines.pos;
    let tail_start = len.saturating_sub(TAIL_BYTES).max(head_end);
    let layer_bytes = (head_end / head_layers.max(1)).max(2000);
    let windows = (SAMPLE_BYTES.min(len / 4) / layer_bytes).clamp(MIN_WINDOWS, MAX_WINDOWS);

    // 2. One layer from each of evenly spaced places in between.
    let span = tail_start - head_end;
    let mut sampled = 0;
    // Where the last window stopped: the next starts no earlier, so two
    // never read the same layer.
    let mut last_end = head_end;
    for i in 0..windows {
        let at = (head_end + span * (2 * i + 1) / (2 * windows)).max(last_end);
        if at >= tail_start {
            break;
        }
        lines.jump(at)?;
        // The next layer comment.
        let limit = lines.pos + SEARCH_BYTES;
        let mut found = false;
        while lines.pos < limit.min(tail_start) {
            match lines.next()? {
                Some(l) if is_layer_marker(l) => {
                    parser.skip();
                    parser.line(l);
                    found = true;
                    break;
                }
                Some(_) => {}
                None => break,
            }
        }
        if !found {
            last_end = lines.pos;
            continue;
        }
        // Up to the one after.
        let end = lines.pos + LAYER_BYTES;
        while lines.pos < end {
            match lines.next()? {
                Some(l) if is_layer_marker(l) => break,
                Some(l) => parser.line(l),
                None => break,
            }
        }
        last_end = lines.pos;
        sampled += 1;
    }

    // 3. The tail, from its first layer comment.  Until then only comments
    //    are read, which keeps the settings and estimates of a tail that
    //    has no layer comment (a long end block).
    lines.jump(tail_start)?;
    let mut started = false;
    while let Some(l) = lines.next()? {
        if !started {
            if is_layer_marker(l) {
                parser.skip();
                started = true;
            } else {
                if l.first() == Some(&b';') {
                    parser.line(l);
                }
                continue;
            }
        }
        parser.line(l);
    }
    Ok(sampled)
}
