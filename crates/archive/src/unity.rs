//! Unity packages (`.unitypackage`), the files the Unity editor exports and
//! the Asset Store sells: a gzip-compressed tar with one folder per asset,
//! named by the asset's GUID, holding `asset` (the file itself),
//! `asset.meta` (Unity's import settings), `pathname` (where the asset goes
//! in a project, such as `Assets/Models/Ship.fbx`) and, for assets the
//! editor drew a preview of (models, materials, prefabs, textures),
//! `preview.png`.  A project folder has a GUID folder too, with a pathname
//! and no asset.  Some packers put `./` in front of every name, and some
//! macOS ones add `._` resource-fork files, which are skipped.
//!
//! A gzip stream can't be entered in the middle, so opening a package
//! inflates all of it once to list the assets under their paths.  A package
//! whose tar fits in [`KEEP_BYTES`] stays in memory; reading an asset of a
//! larger one inflates the stream again up to it.  Thumbnails don't open the
//! package: [`pictures`] inflates only until it has found enough preview
//! pictures, or its time is up.

use crate::source::Source;
use crate::{ArchiveError, ArchiveKind, Entry, Extra, Limits, Parsed, Result, Slot};
use miniz_oxide::inflate::stream::{InflateState, inflate};
use miniz_oxide::{DataFormat, MZError, MZFlush, MZStatus};
use std::collections::HashMap;
use std::path::Path;
use std::time::{Duration, Instant};

/// The largest tar kept in memory once inflated (small in tests, so they
/// read large packages' way too).
const KEEP_BYTES: u64 = if cfg!(test) { 64 << 10 } else { 128 << 20 };

/// The largest `pathname` file read; real ones are a line of text.
const MAX_PATHNAME: u64 = 4096;

/// The largest picture [`pictures`] returns.
const MAX_PICTURE: u64 = 16 << 20;

const CHUNK: usize = 64 * 1024;

/*┌─────────────────╮
| Gzip stream     |
└─────────────────╯*/
//region Gzip stream

/// Where the deflate data of the gzip member at the start of `source`
/// begins, past the header and its optional fields.
fn deflate_start(source: &Source) -> Result<u64> {
    let h = source.head(0, 10);
    if h.len() < 10 || h[..3] != [0x1F, 0x8B, 8] {
        return Err(ArchiveError::NotMine);
    }
    let flags = h[3];
    let mut at = 10u64;
    if flags & 4 != 0 {
        let x = source.head(at, 2);
        let len = u16::from_le_bytes([*x.first().unwrap_or(&0), *x.get(1).unwrap_or(&0)]) as u64;
        at += 2 + len;
    }
    // The file name and comment end with a zero byte.
    for flag in [8u8, 16] {
        if flags & flag != 0 {
            let mut steps = 0;
            loop {
                let b = source.head(at, 1);
                at += 1;
                steps += 1;
                if b.first().is_none_or(|&b| b == 0) || steps > 65536 {
                    break;
                }
            }
        }
    }
    if flags & 2 != 0 {
        at += 2;
    }
    if at >= source.len() {
        return Err(ArchiveError::invalid("the gzip header runs past the end"));
    }
    Ok(at)
}

/// The inflated bytes of a deflate stream, pulled a chunk at a time, with
/// a cap on how much it may produce (a small file can claim gigabytes).
struct Inflater<'a> {
    source: &'a Source,
    next: u64,
    state: Box<InflateState>,
    input: Vec<u8>,
    used: usize,
    filled: usize,
    output: Vec<u8>,
    out_at: usize,
    out_len: usize,
    /// Where the next byte handed out sits in the inflated stream.
    position: u64,
    /// Bytes inflated so far, and the most allowed.
    produced: u64,
    cap: u64,
    ended: bool,
}

impl<'a> Inflater<'a> {
    fn new(source: &'a Source, at: u64, cap: u64) -> Inflater<'a> {
        Inflater {
            source,
            next: at,
            state: InflateState::new_boxed(DataFormat::Raw),
            input: vec![0; CHUNK],
            used: 0,
            filled: 0,
            output: vec![0; CHUNK],
            out_at: 0,
            out_len: 0,
            position: 0,
            produced: 0,
            cap,
            ended: false,
        }
    }

    /// The bytes ready to hand out, inflating more when there are none;
    /// empty at the end of the stream.
    fn fill(&mut self) -> Result<&[u8]> {
        while self.out_at == self.out_len && !self.ended {
            if self.used == self.filled {
                let n = (self.source.len().saturating_sub(self.next)).min(self.input.len() as u64) as usize;
                if n > 0 {
                    self.source.read_at(self.next, &mut self.input[..n])?;
                }
                self.next += n as u64;
                (self.filled, self.used) = (n, 0);
            }
            let r = inflate(&mut self.state, &self.input[self.used..self.filled], &mut self.output, MZFlush::None);
            self.used += r.bytes_consumed;
            (self.out_at, self.out_len) = (0, r.bytes_written);
            self.produced += r.bytes_written as u64;
            if self.produced > self.cap {
                return Err(ArchiveError::TooLarge(format!("the package inflates to more than {} bytes", self.cap)));
            }
            match r.status {
                Ok(MZStatus::StreamEnd) => self.ended = true,
                Ok(_) | Err(MZError::Buf) => {}
                Err(e) => return Err(ArchiveError::invalid(format!("damaged gzip data ({e:?})"))),
            }
            if r.bytes_consumed == 0
                && r.bytes_written == 0
                && self.used == self.filled
                && self.next >= self.source.len()
            {
                return Err(ArchiveError::invalid("the gzip data is cut short"));
            }
        }
        Ok(&self.output[self.out_at..self.out_len])
    }

    fn consume(&mut self, n: usize) {
        self.out_at += n;
        self.position += n as u64;
    }

    /// Fills `buf`; false when the stream ends first.
    fn read_exact(&mut self, buf: &mut [u8]) -> Result<bool> {
        let mut done = 0;
        while done < buf.len() {
            let chunk = self.fill()?;
            if chunk.is_empty() {
                return Ok(false);
            }
            let n = chunk.len().min(buf.len() - done);
            buf[done..done + n].copy_from_slice(&chunk[..n]);
            self.consume(n);
            done += n;
        }
        Ok(true)
    }

    /// Hands the next `n` bytes to `sink`, which returns false to drop the
    /// rest (they are still skipped).
    fn pass(&mut self, mut n: u64, sink: &mut dyn FnMut(&[u8]) -> Result<bool>) -> Result<bool> {
        let mut wanted = true;
        while n > 0 {
            let chunk = self.fill()?;
            if chunk.is_empty() {
                return Ok(false);
            }
            let k = (chunk.len() as u64).min(n) as usize;
            if wanted {
                wanted = sink(&chunk[..k])?;
            }
            self.consume(k);
            n -= k as u64;
        }
        Ok(true)
    }
}

/// The cap on what a package may inflate to: generous for real packages,
/// which compress a few times at most, and bounded for a small file that
/// claims to hold gigabytes of zeros.
fn inflate_cap(source: &Source) -> u64 {
    let least = if cfg!(test) { 8 << 20 } else { 512 << 20 };
    source.len().saturating_mul(64).clamp(least, 64 << 30)
}

//endregion

/*┌─────────────────╮
| Tar             |
└─────────────────╯*/
//region Tar

/// One member of the tar.
struct Member {
    name: String,
    size: u64,
    regular: bool,
    /// Where its data starts in the inflated stream.
    at: u64,
}

/// A tar header's number: octal digits, or base 256 when the first byte has
/// its top bit set.
fn tar_number(field: &[u8]) -> Option<u64> {
    if field.first().is_some_and(|b| b & 0x80 != 0) {
        let mut v = (field[0] & 0x7F) as u64;
        for &b in &field[1..] {
            v = v.checked_mul(256)?.checked_add(b as u64)?;
        }
        return Some(v);
    }
    let text = std::str::from_utf8(field).ok()?.trim_matches(|c: char| c == '\0' || c == ' ');
    if text.is_empty() {
        return Some(0);
    }
    u64::from_str_radix(text, 8).ok()
}

fn text(field: &[u8]) -> String {
    let end = field.iter().position(|&b| b == 0).unwrap_or(field.len());
    String::from_utf8_lossy(&field[..end]).into_owned()
}

/// Whether a 512-byte block is a tar header whose checksum adds up.
fn header_checks(h: &[u8]) -> bool {
    let Some(stored) = tar_number(&h[148..156]) else { return false };
    let sum: u64 = h.iter().enumerate().map(|(i, &b)| if (148..156).contains(&i) { 32 } else { b as u64 }).sum();
    sum == stored
}

/// Walks the tar members, calling `visit` with each; `visit` reads or skips
/// the member's data and returns false to stop.
fn walk(inflater: &mut Inflater, mut visit: impl FnMut(&Member, &mut Inflater) -> Result<bool>) -> Result<()> {
    let mut header = [0u8; 512];
    let mut long_name: Option<String> = None;
    loop {
        if !inflater.read_exact(&mut header)? || header.iter().all(|&b| b == 0) {
            return Ok(());
        }
        if !header_checks(&header) {
            return Err(ArchiveError::invalid("a damaged tar header"));
        }
        let size = tar_number(&header[124..136]).ok_or_else(|| ArchiveError::invalid("a damaged tar size"))?;
        let padded = size.checked_add(511).ok_or_else(|| ArchiveError::invalid("a damaged tar size"))? / 512 * 512;
        let flag = header[156];
        let mut name = text(&header[..100]);
        if &header[257..262] == b"ustar" {
            let prefix = text(&header[345..500]);
            if !prefix.is_empty() {
                name = format!("{prefix}/{name}");
            }
        }
        match flag {
            // A GNU long name for the next member.
            b'L' if size <= 4096 => {
                let mut bytes = vec![0u8; padded as usize];
                if !inflater.read_exact(&mut bytes)? {
                    return Ok(());
                }
                long_name = Some(text(&bytes[..size as usize]));
                continue;
            }
            // PAX headers may rename the next member: `path=` records.
            b'x' if size <= 65536 => {
                let mut bytes = vec![0u8; padded as usize];
                if !inflater.read_exact(&mut bytes)? {
                    return Ok(());
                }
                let records = String::from_utf8_lossy(&bytes[..size as usize]).into_owned();
                if let Some(p) = records.lines().find_map(|l| l.split_once(" path=").map(|(_, p)| p.to_string())) {
                    long_name = Some(p);
                }
                continue;
            }
            _ => {}
        }
        let member = Member {
            name: long_name.take().unwrap_or(name),
            size,
            regular: matches!(flag, b'0' | 0 | b'7'),
            at: inflater.position,
        };
        let start = inflater.position;
        if !visit(&member, inflater)? {
            return Ok(());
        }
        // Skip what `visit` left of the data and the padding.
        let read = inflater.position - start;
        if read < padded && !inflater.pass(padded - read, &mut |_| Ok(false))? {
            return Ok(());
        }
    }
}

/// A member name as the GUID folder and the file in it (`asset`,
/// `pathname`, ...), without a leading `./`.
fn split(name: &str) -> Option<(&str, &str)> {
    let name = name.trim_start_matches("./");
    let (guid, file) = name.split_once('/')?;
    (!guid.is_empty() && !file.is_empty() && !file.contains('/') && !file.starts_with("._")).then_some((guid, file))
}

//endregion

/*┌─────────────────╮
| Recognising     |
└─────────────────╯*/
//region Recognising

/// Whether the start of a file is a Unity package: gzip whose tar's first
/// members are GUID folders (32 hexadecimal digits) or files in them.
pub fn sniff(head: &[u8]) -> bool {
    if head.len() < 18 || head[..3] != [0x1F, 0x8B, 8] {
        return false;
    }
    let source = Source::Memory(head.to_vec().into());
    let Ok(at) = deflate_start(&source) else { return false };
    let mut inflater = Inflater::new(&source, at, 1 << 20);
    let mut header = [0u8; 512];
    for _ in 0..3 {
        // The head is cut short, so a stream that ends early isn't an error
        // here: whatever headers did inflate decide.
        match inflater.read_exact(&mut header) {
            Ok(true) => {}
            _ => return false,
        }
        if !header_checks(&header) {
            return false;
        }
        let name = text(&header[..100]);
        let name = name.trim_start_matches("./");
        let guid = name.split('/').next().unwrap_or("");
        if guid.len() == 32 && guid.bytes().all(|b| b.is_ascii_hexdigit()) {
            return true;
        }
        if !name.is_empty() {
            return false;
        }
        // `./` itself: look at the next member, skipping any data.
        let size = tar_number(&header[124..136]).unwrap_or(u64::MAX);
        if size != 0 {
            return false;
        }
    }
    false
}

//endregion

/*┌─────────────────╮
| Archive         |
└─────────────────╯*/
//region Archive

/// What reading a package's assets needs.
pub(crate) struct Unity {
    /// Where the deflate data starts in the file.
    deflate_at: u64,
    /// The inflated tar, when it was small enough to keep.
    tar: Option<std::sync::Arc<[u8]>>,
}

#[derive(Default)]
struct Asset {
    pathname: Option<String>,
    asset: Option<(u64, u64)>,
}

pub(crate) fn parse(source: &Source, limits: &Limits) -> Result<Parsed> {
    let deflate_at = deflate_start(source)?;
    let mut inflater = Inflater::new(source, deflate_at, inflate_cap(source));
    let mut assets: HashMap<String, Asset> = HashMap::new();
    let mut order: Vec<String> = Vec::new();
    let mut kept: Option<Vec<u8>> = Some(Vec::new());
    let mut warnings = Vec::new();
    // 1. One pass over the tar, keeping it while it is small.  Headers and
    //    padding stay zeros in the copy: only data is ever read from it.
    walk(&mut inflater, |m, inf| {
        if let Some(k) = kept.as_mut() {
            k.resize(m.at as usize, 0);
        }
        let Some((guid, file)) = split(&m.name).filter(|_| m.regular) else {
            return Ok(true);
        };
        if !assets.contains_key(guid) {
            if order.len() >= limits.max_entries {
                warnings.push(format!("only the first {} assets are listed", limits.max_entries));
                return Ok(false);
            }
            order.push(guid.to_string());
        }
        let a = assets.entry(guid.to_string()).or_default();
        match file {
            "pathname" if m.size <= MAX_PATHNAME => {
                let mut bytes = vec![0u8; m.size as usize];
                if inf.read_exact(&mut bytes)? {
                    // Old packages add a line with "00" after the path.
                    let line = String::from_utf8_lossy(&bytes).lines().next().unwrap_or("").trim().to_string();
                    a.pathname = Some(line);
                }
                if let Some(k) = kept.as_mut() {
                    k.extend_from_slice(&bytes);
                }
            }
            "asset" => a.asset = Some((m.at, m.size)),
            _ => {}
        }
        // Keep the data of the members not read above while the tar is small.
        if let Some(k) = kept.as_mut() {
            if m.at.saturating_add(m.size) > KEEP_BYTES {
                kept = None;
            } else if file != "pathname" || m.size > MAX_PATHNAME {
                inf.pass(m.size, &mut |chunk| {
                    k.extend_from_slice(chunk);
                    Ok(true)
                })?;
            }
        }
        Ok(true)
    })?;
    // 2. Entries under their paths, sorted.
    let mut listed: Vec<(String, (u64, u64))> = Vec::new();
    let mut nameless = 0usize;
    for guid in &order {
        let a = &assets[guid];
        let Some(asset) = a.asset else { continue };
        match &a.pathname {
            Some(p) if !p.is_empty() && p.len() <= limits.max_name_bytes => {
                listed.push((p.replace('\\', "/").trim_start_matches('/').to_string(), asset));
            }
            _ => nameless += 1,
        }
    }
    if nameless > 0 {
        warnings.push(format!("{nameless} assets without a path were left out"));
    }
    listed.sort_by(|a, b| a.0.cmp(&b.0));
    let mut entries = Vec::with_capacity(listed.len());
    let mut slots = Vec::with_capacity(listed.len());
    for (path, (offset, len)) in listed {
        let data = crate::from_name(&path, None);
        entries.push(Entry {
            index: entries.len(),
            path,
            size: len,
            stored_size: len,
            kind: data.kind(None),
            data,
            namespace: None,
            locked: false,
        });
        slots.push(Slot::Unity { offset, len });
    }
    let tar = kept.map(|mut k| {
        k.truncate(inflater.position as usize);
        k.into()
    });
    Ok(Parsed {
        kind: ArchiveKind::UnityPackage,
        entries,
        slots,
        groups: Vec::new(),
        warnings,
        label: "Unity package".into(),
        extra: Extra::Unity(Box::new(Unity { deflate_at, tar })),
    })
}

/// Streams a stretch of the inflated tar: from memory when it was kept,
/// else by inflating the package again up to it.
pub(crate) fn stream(
    source: &Source,
    unity: &Unity,
    offset: u64,
    len: u64,
    max: u64,
    sink: &mut dyn FnMut(&[u8]) -> Result<bool>,
) -> Result<u64> {
    let want = len.min(max);
    if let Some(tar) = &unity.tar {
        let end = offset.checked_add(want).filter(|&e| e <= tar.len() as u64);
        let Some(end) = end else { return Err(ArchiveError::invalid("an asset runs past the end of the package")) };
        let mut done = offset;
        while done < end {
            let n = (end - done).min(CHUNK as u64);
            if !sink(&tar[done as usize..(done + n) as usize])? {
                break;
            }
            done += n;
        }
        return Ok(done - offset);
    }
    let mut inflater = Inflater::new(source, unity.deflate_at, inflate_cap(source));
    if !inflater.pass(offset, &mut |_| Ok(false))? {
        return Err(ArchiveError::invalid("an asset runs past the end of the package"));
    }
    let mut delivered = 0u64;
    inflater.pass(want, &mut |chunk| {
        delivered += chunk.len() as u64;
        sink(chunk)
    })?;
    Ok(delivered)
}

//endregion

/*┌─────────────────╮
| Pictures        |
└─────────────────╯*/
//region Pictures

/// A picture found in a package: an asset's stored preview, or (when the
/// package has none) a picture asset itself.
pub struct Picture {
    /// The asset's path in a project.
    pub path: String,
    pub bytes: Vec<u8>,
    /// Whether it is Unity's preview of the asset rather than the asset.
    pub stored_preview: bool,
}

/// Up to `max` preview pictures of the package at `path`, in the package's
/// order, inflating only as far as needed and stopping at `budget`.  When
/// the package has no previews, its first PNG or JPEG asset stands in.
pub fn pictures(path: &Path, max: usize, budget: Duration) -> Result<Vec<Picture>> {
    let started = Instant::now();
    let source = Source::open(path)?;
    let deflate_at = deflate_start(&source)?;
    let mut inflater = Inflater::new(&source, deflate_at, inflate_cap(&source));
    let mut names: HashMap<String, String> = HashMap::new();
    let mut found: Vec<(String, Vec<u8>, bool)> = Vec::new();
    let mut previews = 0;
    let result = walk(&mut inflater, |m, inf| {
        if previews >= max || started.elapsed() > budget {
            return Ok(false);
        }
        let Some((guid, file)) = split(&m.name).filter(|_| m.regular) else { return Ok(true) };
        let picture_asset = file == "asset" && previews == 0 && !found.iter().any(|f| !f.2) && m.size <= MAX_PICTURE;
        match file {
            "pathname" if m.size <= MAX_PATHNAME => {
                let mut bytes = vec![0u8; m.size as usize];
                if !inf.read_exact(&mut bytes)? {
                    return Ok(false);
                }
                let line = String::from_utf8_lossy(&bytes).lines().next().unwrap_or("").trim().to_string();
                names.insert(guid.to_string(), line);
            }
            "preview.png" if m.size <= MAX_PICTURE => {
                let mut bytes = vec![0u8; m.size as usize];
                if !inf.read_exact(&mut bytes)? {
                    return Ok(false);
                }
                // A stand-in asset gives way to the first real preview.
                if previews == 0 {
                    found.clear();
                }
                previews += 1;
                found.push((guid.to_string(), bytes, true));
            }
            // An asset stands in when it starts like a PNG or JPEG.
            _ if picture_asset && m.size >= 8 => {
                let mut bytes = vec![0u8; 8];
                if !inf.read_exact(&mut bytes)? {
                    return Ok(false);
                }
                if bytes.starts_with(b"\x89PNG") || bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
                    bytes.resize(m.size as usize, 0);
                    if !inf.read_exact(&mut bytes[8..])? {
                        return Ok(false);
                    }
                    found.push((guid.to_string(), bytes, false));
                }
            }
            _ => {}
        }
        Ok(true)
    });
    // A damaged package still shows what was found before the damage.
    if let Err(e) = result
        && found.is_empty()
    {
        return Err(e);
    }
    Ok(found
        .into_iter()
        .map(|(guid, bytes, stored_preview)| {
            let path = names.get(&guid).cloned().filter(|p| !p.is_empty()).unwrap_or(guid);
            Picture { path, bytes, stored_preview }
        })
        .collect())
}

//endregion
