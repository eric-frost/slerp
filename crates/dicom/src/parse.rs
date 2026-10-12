//! Reading data sets: the Part 10 file meta information, then the data set
//! in its transfer syntax (explicit or implicit VR, little or big endian,
//! deflated), sequences of defined and undefined length, and encapsulated
//! pixel data.
//!
//! Files are read through a small cache with seeks rather than whole, so a
//! thumbnail of a 500 MB cine loop reads its header and one frame.  Large
//! binary values (pixel data, overlays, waveforms) stay in the file as
//! [`Span`]s.  Every length is checked against what is left of the file,
//! nesting and the number of elements are capped, and damaged files end in
//! an error or a partial data set, never a panic or an unbounded
//! allocation.

use crate::data::{DataSet, Element, Span, Tag, Value, Vr, tag, tags};
use crate::dict;
use crate::transfer::TransferSyntax;
use slerp_scene::LoadError;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Values longer than this stay in the file.
const INLINE_MAX: u64 = 1 << 20;
/// Sequences nest at most this deep (structured reports nest a dozen or so).
const MAX_DEPTH: u32 = 64;
/// At most this many elements and items in a file, which bounds the memory
/// a file of millions of empty items (a "sequence bomb") can take.
const MAX_ELEMENTS: usize = 400_000;
/// At most this many fragments of encapsulated pixel data.
const MAX_FRAGMENTS: usize = 1 << 20;
/// A deflated data set may inflate to at most this.
const MAX_INFLATED: usize = 1 << 30;

/*┌─────────────────╮
| Input           |
└─────────────────╯*/
//region Input

/// Where a file's bytes come from: the file itself, read with seeks, or
/// memory (deflated data sets, tests).
#[derive(Clone, Debug)]
pub enum Source {
    File(PathBuf),
    Memory(Arc<Vec<u8>>),
}

impl Source {
    /// Reads a span of the file, at most `max` bytes.
    pub fn read(&self, span: Span, max: u64) -> std::io::Result<Vec<u8>> {
        if span.len > max {
            return Err(std::io::Error::other(format!("value of {} bytes is over the {max} byte limit", span.len)));
        }
        match self {
            Source::Memory(m) => {
                let end = span.offset.checked_add(span.len).filter(|&e| e <= m.len() as u64);
                match end {
                    Some(end) => Ok(m[span.offset as usize..end as usize].to_vec()),
                    None => Err(std::io::ErrorKind::UnexpectedEof.into()),
                }
            }
            Source::File(path) => {
                let mut f = std::fs::File::open(path)?;
                let len = f.metadata()?.len();
                if span.offset.checked_add(span.len).is_none_or(|e| e > len) {
                    return Err(std::io::ErrorKind::UnexpectedEof.into());
                }
                f.seek(SeekFrom::Start(span.offset))?;
                let mut out = vec![0; span.len as usize];
                f.read_exact(&mut out)?;
                Ok(out)
            }
        }
    }

    /// Reads several spans and joins them (a frame's fragments).
    pub fn read_joined(&self, spans: &[Span], max: u64) -> std::io::Result<Vec<u8>> {
        let total: u64 = spans.iter().map(|s| s.len).sum();
        if total > max {
            return Err(std::io::Error::other(format!("frame of {total} bytes is over the {max} byte limit")));
        }
        let mut out = Vec::with_capacity(total as usize);
        match self {
            Source::Memory(_) => {
                for s in spans {
                    out.extend(self.read(*s, max)?);
                }
            }
            Source::File(path) => {
                let mut f = std::fs::File::open(path)?;
                let len = f.metadata()?.len();
                for s in spans {
                    if s.offset.checked_add(s.len).is_none_or(|e| e > len) {
                        return Err(std::io::ErrorKind::UnexpectedEof.into());
                    }
                    f.seek(SeekFrom::Start(s.offset))?;
                    let start = out.len();
                    out.resize(start + s.len as usize, 0);
                    f.read_exact(&mut out[start..])?;
                }
            }
        }
        Ok(out)
    }
}

/// Random access to a file's bytes for the parser.
trait Input {
    fn len(&self) -> u64;
    /// Fills `buf` from `pos`; false when the input ends first.
    fn read_at(&mut self, pos: u64, buf: &mut [u8]) -> bool;
}

struct MemInput<'a>(&'a [u8]);

impl Input for MemInput<'_> {
    fn len(&self) -> u64 {
        self.0.len() as u64
    }

    fn read_at(&mut self, pos: u64, buf: &mut [u8]) -> bool {
        let Some(end) = pos.checked_add(buf.len() as u64).filter(|&e| e <= self.0.len() as u64) else {
            return false;
        };
        buf.copy_from_slice(&self.0[pos as usize..end as usize]);
        true
    }
}

/// A file read through a 64 KB window, so the parser's many small reads
/// cost few system calls and large values are skipped with a seek.
struct FileInput {
    file: std::fs::File,
    len: u64,
    window: Vec<u8>,
    start: u64,
}

const WINDOW: usize = 64 << 10;

impl Input for FileInput {
    fn len(&self) -> u64 {
        self.len
    }

    fn read_at(&mut self, pos: u64, buf: &mut [u8]) -> bool {
        let Some(end) = pos.checked_add(buf.len() as u64).filter(|&e| e <= self.len) else { return false };
        if pos >= self.start && end <= self.start + self.window.len() as u64 {
            let at = (pos - self.start) as usize;
            buf.copy_from_slice(&self.window[at..at + buf.len()]);
            return true;
        }
        if buf.len() > WINDOW / 2 {
            return self.file.seek(SeekFrom::Start(pos)).is_ok() && self.file.read_exact(buf).is_ok();
        }
        let want = (self.len - pos).min(WINDOW as u64) as usize;
        self.window.resize(want, 0);
        if self.file.seek(SeekFrom::Start(pos)).is_err() || self.file.read_exact(&mut self.window).is_err() {
            self.window.clear();
            return false;
        }
        self.start = pos;
        buf.copy_from_slice(&self.window[..buf.len()]);
        true
    }
}

//endregion

/*┌─────────────────╮
| Files           |
└─────────────────╯*/
//region Files

/// A parsed file: its meta information, data set and transfer syntax.
#[derive(Clone, Debug)]
pub struct Parsed {
    /// The file meta information (group 0002); empty for files without it.
    pub meta: DataSet,
    pub data: DataSet,
    pub transfer: TransferSyntax,
    /// Where values left in the file are read from.
    pub source: Source,
    /// The file has the 128-byte preamble and `DICM`.
    pub part10: bool,
    /// Problems that didn't stop reading, such as a truncated last element.
    pub warnings: Vec<String>,
}

/// How much of a data set to read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Extent {
    /// Everything (pixel data stays in the file either way).
    All,
    /// Stop at the pixel data group, for sorting a series by its headers.
    Header,
}

/// True when `head` (the start of a file) is a Part 10 file or looks like
/// a bare data set.
pub fn looks_like_dicom(head: &[u8]) -> bool {
    if head.len() >= 132 && &head[128..132] == b"DICM" {
        return true;
    }
    bare_data_set(head).is_some()
}

/// Recognises a data set without the preamble: a run of elements with
/// ascending tags starting in group 0002 or 0008, explicit or implicit
/// VR, either byte order, with short even lengths.  Returns (explicit VR,
/// big endian).  Three elements must parse, since two bytes of zeros and
/// an 8 start plenty of other files.
pub(crate) fn bare_data_set(head: &[u8]) -> Option<(bool, bool)> {
    for (explicit, big) in [(true, false), (false, false), (true, true)] {
        let mut pos = 0usize;
        let mut last: Option<Tag> = None;
        let mut count = 0;
        while pos + 8 <= head.len() && count < 3 {
            let rd16 = |p: usize| {
                if big {
                    u16::from_be_bytes([head[p], head[p + 1]])
                } else {
                    u16::from_le_bytes([head[p], head[p + 1]])
                }
            };
            let rd32 = |p: usize| {
                let b = [head[p], head[p + 1], head[p + 2], head[p + 3]];
                if big { u32::from_be_bytes(b) } else { u32::from_le_bytes(b) }
            };
            let t = tag(rd16(pos), rd16(pos + 2));
            let group = t >> 16;
            if count == 0 && group != 0x0002 && group != 0x0008 {
                break;
            }
            if last.is_some_and(|l| t <= l) || group > 0x7FE0 || (big && group == 0x0002) {
                break;
            }
            let (len, header) = if explicit {
                let vr = Vr([head[pos + 4], head[pos + 5]]);
                if !vr.is_known() {
                    break;
                }
                if vr.long_length() {
                    if pos + 12 > head.len() {
                        break;
                    }
                    (rd32(pos + 8), 12)
                } else {
                    (rd16(pos + 6) as u32, 8)
                }
            } else {
                (rd32(pos + 4), 8)
            };
            // Header elements are short, and every value has an even length.
            // (The meta group is always little-endian.)
            if len > 1024 || len % 2 == 1 {
                break;
            }
            pos += header + len as usize;
            last = Some(t);
            count += 1;
        }
        if count >= 3 {
            return Some((explicit, big));
        }
    }
    None
}

/// Parses a file.  Large values stay in the file and are read later
/// through [`Parsed::source`].
pub fn parse_file(path: &Path, extent: Extent) -> Result<Parsed, LoadError> {
    let file = std::fs::File::open(path)?;
    let len = file.metadata()?.len();
    let mut input = FileInput { file, len, window: Vec::new(), start: 0 };
    parse(&mut input, Source::File(path.to_path_buf()), extent)
}

/// Parses a file held in memory.
pub fn parse_bytes(bytes: Vec<u8>, extent: Extent) -> Result<Parsed, LoadError> {
    let bytes = Arc::new(bytes);
    let mut input = MemInput(&bytes);
    // The memory input borrows from `bytes`, so the source gets a second
    // reference to it.
    parse(&mut input, Source::Memory(bytes.clone()), extent)
}

fn parse(input: &mut dyn Input, source: Source, extent: Extent) -> Result<Parsed, LoadError> {
    let mut warnings = Vec::new();
    // 1. The preamble and `DICM`, or a bare data set.
    let mut magic = [0u8; 132];
    let part10 = input.read_at(0, &mut magic) && &magic[128..132] == b"DICM";
    let mut pos = if part10 { 132 } else { 0 };
    let mut head = vec![0u8; (input.len() - pos).min(512) as usize];
    if !input.read_at(pos, &mut head) {
        return Err(LoadError::invalid("file too short for DICOM"));
    }
    let bare = if part10 { None } else { Some(bare_data_set(&head).ok_or(LoadError::NotMine)?) };

    // 2. The file meta information: group 0002, always explicit VR little
    //    endian, when it's there.
    let mut budget = MAX_ELEMENTS;
    let mut meta = DataSet::default();
    let starts_with_meta = head.len() >= 2 && u16::from_le_bytes([head[0], head[1]]) == 0x0002;
    if starts_with_meta {
        let mut p = Parser {
            input,
            explicit: true,
            big: false,
            budget: &mut budget,
            extent: Extent::All,
            warnings: &mut warnings,
        };
        meta = p.data_set(&mut pos, None, 0, Some(0x0002))?;
    }

    // 3. The data set's encoding: the meta information's transfer syntax,
    //    checked against what the data set's first bytes look like (files
    //    that say explicit but are implicit exist, and so on).
    let mut transfer = match meta.string(tags::TRANSFER_SYNTAX) {
        Some(uid) => TransferSyntax::from_uid(&uid),
        None => match bare {
            Some((true, true)) => TransferSyntax::from_uid(crate::transfer::EXPLICIT_BE),
            Some((true, false)) => TransferSyntax::from_uid(crate::transfer::EXPLICIT_LE),
            _ => TransferSyntax::implicit_le(),
        },
    };
    let mut source = source;
    let inflated: Option<Arc<Vec<u8>>> = if transfer.deflated {
        // 3a. Deflated: inflate the rest of the file and parse that.
        let rest = input.len() - pos;
        if rest > MAX_INFLATED as u64 {
            return Err(LoadError::TooLarge("deflated data set".into()));
        }
        let mut packed = vec![0u8; rest as usize];
        if !input.read_at(pos, &mut packed) {
            return Err(LoadError::invalid("truncated deflated data set"));
        }
        let inflated = miniz_oxide::inflate::decompress_to_vec_with_limit(&packed, MAX_INFLATED)
            .map_err(|e| LoadError::invalid(format!("deflated data set: {:?}", e.status)))?;
        let inflated = Arc::new(inflated);
        source = Source::Memory(inflated.clone());
        pos = 0;
        Some(inflated)
    } else {
        None
    };
    let mut mem;
    let data_input: &mut dyn Input = match &inflated {
        Some(bytes) => {
            mem = MemInput(bytes);
            &mut mem
        }
        None => input,
    };
    let mut first = [0u8; 6];
    if data_input.read_at(pos, &mut first) {
        let vr = Vr([first[4], first[5]]);
        let group_le = u16::from_le_bytes([first[0], first[1]]);
        if transfer.explicit_vr && !vr.is_known() {
            transfer.explicit_vr = false;
            warnings.push("the data set is implicit VR although the transfer syntax says explicit".into());
        } else if !transfer.explicit_vr && vr.is_known() {
            // Implicit VR data never has two capital letters where the
            // length's low bytes are, unless the length is about 16,000 or
            // more; header elements are short.
            transfer.explicit_vr = true;
            warnings.push("the data set is explicit VR although the transfer syntax says implicit".into());
        }
        // Big-endian files labelled little-endian start with group 0800
        // (0008), 1800 (0018) or 2800 (0028).
        if !transfer.big_endian && matches!(group_le, 0x0800 | 0x1800 | 0x2800) {
            transfer.big_endian = true;
            warnings.push("the data set is big-endian although the transfer syntax says little-endian".into());
        }
    }

    // 4. The data set.
    let mut p = Parser {
        input: data_input,
        explicit: transfer.explicit_vr,
        big: transfer.big_endian,
        budget: &mut budget,
        extent,
        warnings: &mut warnings,
    };
    let data = p.data_set(&mut pos, None, 0, None)?;
    if data.elements.is_empty() {
        return Err(LoadError::invalid("empty data set"));
    }
    Ok(Parsed { meta, data, transfer, source, part10, warnings })
}

//endregion

/*┌─────────────────╮
| Parser          |
└─────────────────╯*/
//region Parser

struct Parser<'a> {
    input: &'a mut dyn Input,
    explicit: bool,
    big: bool,
    budget: &'a mut usize,
    extent: Extent,
    warnings: &'a mut Vec<String>,
}

/// How a data set ended.
enum End {
    /// Its defined length or the end of the input.
    Length,
    /// An item delimitation item.
    ItemDelimiter,
}

impl Parser<'_> {
    fn u16_at(&mut self, pos: u64) -> Option<u16> {
        let mut b = [0u8; 2];
        self.input.read_at(pos, &mut b).then(|| if self.big { u16::from_be_bytes(b) } else { u16::from_le_bytes(b) })
    }

    fn u32_at(&mut self, pos: u64) -> Option<u32> {
        let mut b = [0u8; 4];
        self.input.read_at(pos, &mut b).then(|| if self.big { u32::from_be_bytes(b) } else { u32::from_le_bytes(b) })
    }

    fn tag_at(&mut self, pos: u64) -> Option<Tag> {
        Some(tag(self.u16_at(pos)?, self.u16_at(pos + 2)?))
    }

    fn spend(&mut self) -> Result<(), LoadError> {
        if *self.budget == 0 {
            return Err(LoadError::TooLarge(format!("more than {MAX_ELEMENTS} elements")));
        }
        *self.budget -= 1;
        Ok(())
    }

    /// Reads elements from `*pos` until `end` (a defined length), an item
    /// delimiter, or the end of the input.  With `only_group`, stops at the
    /// first element of another group (the meta information).
    fn data_set(
        &mut self,
        pos: &mut u64,
        end: Option<u64>,
        depth: u32,
        only_group: Option<u16>,
    ) -> Result<DataSet, LoadError> {
        let (ds, _) = self.data_set_until(pos, end, depth, only_group)?;
        Ok(ds)
    }

    fn data_set_until(
        &mut self,
        pos: &mut u64,
        end: Option<u64>,
        depth: u32,
        only_group: Option<u16>,
    ) -> Result<(DataSet, End), LoadError> {
        if depth > MAX_DEPTH {
            return Err(LoadError::invalid("sequences nested too deep"));
        }
        let limit = end.unwrap_or(self.input.len()).min(self.input.len());
        let mut ds = DataSet { big_endian: self.big, ..Default::default() };
        loop {
            if *pos >= limit {
                break;
            }
            if *pos + 8 > limit {
                self.warnings.push(format!("{} stray bytes at the end", limit - *pos));
                *pos = limit;
                break;
            }
            let start = *pos;
            let Some(t) = self.tag_at(start) else { break };
            if t == tags::ITEM_DELIMITATION {
                *pos = start + 8;
                ds.finish();
                return Ok((ds, End::ItemDelimiter));
            }
            if t == tags::SEQUENCE_DELIMITATION || t == tags::ITEM {
                // Out of place: a broken item.  Let the sequence see it.
                if depth == 0 {
                    self.warnings.push(format!("stray item tag at byte {start}"));
                    break;
                }
                ds.finish();
                return Ok((ds, End::Length));
            }
            if only_group.is_some_and(|g| (t >> 16) as u16 != g) {
                break;
            }
            if self.extent == Extent::Header && depth == 0 && t >= tag(0x7FE0, 0) {
                break;
            }
            self.spend()?;
            match self.element(pos, t, limit, depth) {
                Ok(Some(e)) => ds.elements.push(e),
                Ok(None) => break,
                Err(e) if depth == 0 && !ds.elements.is_empty() && !matches!(e, LoadError::TooLarge(_)) => {
                    // A damaged element after a readable header: keep what
                    // came before (the pixel data then reports it).
                    self.warnings.push(format!("stopped reading at byte {start}: {e}"));
                    break;
                }
                Err(e) => return Err(e),
            }
        }
        ds.finish();
        Ok((ds, End::Length))
    }

    /// Reads one element whose tag `t` is at `*pos`.  `None` means the
    /// input ended inside the element's header.
    fn element(&mut self, pos: &mut u64, t: Tag, limit: u64, depth: u32) -> Result<Option<Element>, LoadError> {
        let start = *pos;
        // 1. VR and length.  An unknown VR in an explicit file means this
        //    element was written implicitly (some files mix them).
        let mut vr_bytes = [0u8; 2];
        if !self.input.read_at(start + 4, &mut vr_bytes) {
            return Ok(None);
        }
        let explicit_vr = Vr(vr_bytes);
        let (vr, len, header) = if self.explicit && explicit_vr.is_known() {
            if explicit_vr.long_length() {
                let Some(len) = self.u32_at(start + 8) else { return Ok(None) };
                (explicit_vr, len, 12)
            } else {
                let Some(len) = self.u16_at(start + 6) else { return Ok(None) };
                (explicit_vr, len as u32, 8)
            }
        } else {
            let Some(len) = self.u32_at(start + 4) else { return Ok(None) };
            (dict::implicit_vr(t), len, 8)
        };
        let implicit_here = !(self.explicit && explicit_vr.is_known());
        *pos = start + header;

        // 2. Undefined length: a sequence, or encapsulated pixel data.
        if len == u32::MAX {
            if t == tags::PIXEL_DATA || vr == Vr::OB || vr == Vr::OW {
                let fragments = self.fragments(pos, limit)?;
                return Ok(Some(Element { tag: t, vr, value: Value::Fragments(fragments) }));
            }
            // An undefined-length UN value is a sequence in implicit VR
            // little endian (CP-246).
            let items = if vr == Vr::UN && !implicit_here {
                self.with_encoding(false, false, |p| p.sequence(pos, None, limit, depth))?
            } else {
                self.sequence(pos, None, limit, depth)?
            };
            return Ok(Some(Element { tag: t, vr: Vr::SQ, value: Value::Sequence(items) }));
        }

        // 3. Defined length.
        let len = len as u64;
        if len > limit - *pos {
            return Err(LoadError::invalid(format!(
                "{} {} claims {len} bytes, {} are left",
                crate::data::tag_text(t),
                crate::data::tag_name(t),
                limit - *pos
            )));
        }
        let value_start = *pos;
        *pos += len;
        if vr == Vr::SQ {
            let mut p = value_start;
            let items = self.sequence(&mut p, Some(value_start + len), limit, depth)?;
            return Ok(Some(Element { tag: t, vr, value: Value::Sequence(items) }));
        }
        let span = Span { offset: value_start, len };
        let is_pixels = matches!(t, tags::PIXEL_DATA | tags::FLOAT_PIXEL_DATA | tags::DOUBLE_PIXEL_DATA);
        let value = if is_pixels || len > INLINE_MAX {
            Value::Deferred(span)
        } else {
            let mut bytes = vec![0u8; len as usize];
            if !self.input.read_at(value_start, &mut bytes) {
                return Err(LoadError::invalid("truncated value"));
            }
            Value::Bytes(bytes)
        };
        // Implicit VR pixel data is OW whatever the dictionary's "OB or OW"
        // says first.
        let vr = if implicit_here && t == tags::PIXEL_DATA { Vr::OW } else { vr };
        Ok(Some(Element { tag: t, vr, value }))
    }

    /// Runs `f` with another encoding, restoring this one afterwards.
    fn with_encoding<T>(&mut self, explicit: bool, big: bool, f: impl FnOnce(&mut Self) -> T) -> T {
        let saved = (self.explicit, self.big);
        (self.explicit, self.big) = (explicit, big);
        let r = f(self);
        (self.explicit, self.big) = saved;
        r
    }

    /// Reads a sequence's items, up to `end` or a sequence delimiter.
    fn sequence(&mut self, pos: &mut u64, end: Option<u64>, limit: u64, depth: u32) -> Result<Vec<DataSet>, LoadError> {
        let limit = end.unwrap_or(limit).min(limit);
        let mut items = Vec::new();
        while *pos + 8 <= limit {
            let start = *pos;
            let Some(t) = self.tag_at(start) else { break };
            let Some(len) = self.u32_at(start + 4) else { break };
            *pos = start + 8;
            if t == tags::SEQUENCE_DELIMITATION {
                return Ok(items);
            }
            if t != tags::ITEM {
                return Err(LoadError::invalid(format!("expected an item, found {}", crate::data::tag_text(t))));
            }
            self.spend()?;
            let mut item = if len == u32::MAX {
                let (item, how) = self.data_set_until(pos, Some(limit), depth + 1, None)?;
                if matches!(how, End::Length) && *pos < limit {
                    // Ended on a stray item or sequence delimiter: the
                    // loop sees it next.
                }
                item
            } else {
                // An item that runs past its sequence (a cut-short file) is
                // read as far as it goes.
                let item_end = match pos.checked_add(len as u64) {
                    Some(e) if e <= limit => e,
                    _ => {
                        self.warnings.push(format!("an item of {len} bytes runs past its sequence"));
                        limit
                    }
                };
                let mut p = *pos;
                let item = self.data_set(&mut p, Some(item_end), depth + 1, None)?;
                *pos = item_end;
                item
            };
            item.offset = start;
            items.push(item);
        }
        if end.is_none() {
            // An undefined-length sequence that never ended: the file was
            // cut short.
            self.warnings.push("a sequence runs to the end of the file".into());
        }
        *pos = (*pos).max(end.unwrap_or(*pos)).min(limit);
        Ok(items)
    }

    /// Reads encapsulated pixel data's items up to the sequence delimiter.
    fn fragments(&mut self, pos: &mut u64, limit: u64) -> Result<Vec<Span>, LoadError> {
        let mut out = Vec::new();
        while *pos + 8 <= limit {
            let start = *pos;
            let (Some(t), Some(len)) = (self.tag_at(start), self.u32_at(start + 4)) else { break };
            *pos = start + 8;
            if t == tags::SEQUENCE_DELIMITATION {
                return Ok(out);
            }
            if t != tags::ITEM || len == u32::MAX {
                return Err(LoadError::invalid("damaged encapsulated pixel data"));
            }
            let len = len as u64;
            if len > limit - *pos {
                // The last fragment of a cut-short file: keep what is there.
                self.warnings.push("the pixel data is cut short".into());
                out.push(Span { offset: *pos, len: limit - *pos });
                *pos = limit;
                return Ok(out);
            }
            if out.len() >= MAX_FRAGMENTS {
                return Err(LoadError::TooLarge("too many pixel data fragments".into()));
            }
            out.push(Span { offset: *pos, len });
            *pos += len;
        }
        self.warnings.push("the pixel data has no end marker".into());
        Ok(out)
    }
}

//endregion
