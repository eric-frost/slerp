//! HDF5 files (`.h5`, `.hdf5`, `.hdf`, NetCDF-4 and MATLAB v7.3 `.mat`):
//! a reader for the parts of the format that hold arrays.
//!
//! The file is read by seeking, never mapped or loaded whole, and every
//! length, count and address that comes from the file is checked against the
//! file's size before anything is allocated, so a damaged or hostile file
//! costs an error, not memory.  Supported: superblocks 0 to 3 (with a user
//! block), object headers 1 and 2, old and new style groups (symbol tables,
//! link messages, and dense storage in fractal heaps), compact, contiguous
//! and chunked datasets (every chunk index HDF5 1.10 to 1.14 writes) with
//! deflate, shuffle, LZF and Fletcher-32 filters, and integer, floating
//! point and boolean types.  Compound, string, variable-length and reference
//! types are listed but not shown, with the reason.

mod chunks;
mod dense;

use crate::dtype::{DType, Endian};
use crate::layout::array_roles;
use crate::source::{ByteSource, FileSource, MemSource, read_exact_at};
use crate::volume::{Axis, Volume};
use slerp_scene::LoadError;
use std::collections::HashSet;
use std::fs::File;
use std::path::Path;
use std::sync::Arc;

pub(crate) const UNDEF: u64 = u64::MAX;
/// Largest metadata block read at once.
const MAX_META: usize = 1 << 26;
const SIGNATURE: [u8; 8] = [0x89, b'H', b'D', b'F', b'\r', b'\n', 0x1a, b'\n'];
/// Most objects and links followed when listing a file.
const MAX_OBJECTS: usize = 8192;
const MAX_DEPTH: usize = 24;

/// True when the bytes start an HDF5 file, with or without a user block.
pub fn sniff(head: &[u8]) -> bool {
    [0usize, 512, 1024, 2048].iter().any(|&o| head.get(o..o + 8) == Some(&SIGNATURE[..]))
}

/// An open file.
pub struct H5 {
    pub(crate) file: Arc<File>,
    pub(crate) len: u64,
    /// Where the superblock is: addresses in the file count from here.
    pub(crate) base: u64,
    /// Bytes in an address and in a length.
    pub(crate) so: usize,
    pub(crate) sl: usize,
    pub(crate) root: u64,
    pub sb_version: u8,
    /// The user block's text, which MATLAB fills with its header.
    pub user_block: String,
}

pub(crate) fn le(b: &[u8], at: usize, n: usize) -> u64 {
    let mut v = 0u64;
    for i in 0..n.min(8) {
        v |= (*b.get(at + i).unwrap_or(&0) as u64) << (8 * i);
    }
    v
}

impl H5 {
    pub fn open(path: &Path) -> Result<H5, String> {
        let file = File::open(path).map_err(|e| e.to_string())?;
        let len = file.metadata().map_err(|e| e.to_string())?.len();
        // The superblock sits at 0, 512, 1024, 2048... after a user block.
        let mut at = 0u64;
        let mut sig = [0u8; 8];
        let base = loop {
            if at + 8 > len || at > 1 << 24 {
                return Err("this isn't an HDF5 file".into());
            }
            read_exact_at(&file, &mut sig, at).map_err(|e| e.to_string())?;
            if sig == SIGNATURE {
                break at;
            }
            at = if at == 0 { 512 } else { at * 2 };
        };
        let mut sb = vec![0u8; ((len - base).min(128)) as usize];
        read_exact_at(&file, &mut sb, base).map_err(|e| e.to_string())?;
        let version = *sb.get(8).ok_or("the superblock is cut short")?;
        let (so, sl, root) = match version {
            0 | 1 => {
                let (so, sl) = (sb.get(13).copied().unwrap_or(0) as usize, sb.get(14).copied().unwrap_or(0) as usize);
                if !matches!(so, 2 | 4 | 8) || !matches!(sl, 2 | 4 | 8) {
                    return Err("the superblock's address sizes are invalid".into());
                }
                let start = if version == 0 { 24 } else { 28 };
                // base, free space, end of file, driver info, then the root entry.
                let entry = start + 4 * so;
                let root = le(&sb, entry + sl, so);
                (so, sl, root)
            }
            2 | 3 => {
                let (so, sl) = (sb.get(9).copied().unwrap_or(0) as usize, sb.get(10).copied().unwrap_or(0) as usize);
                if !matches!(so, 2 | 4 | 8) || !matches!(sl, 2 | 4 | 8) {
                    return Err("the superblock's address sizes are invalid".into());
                }
                // base, extension, end of file, root header.
                (so, sl, le(&sb, 12 + 3 * so, so))
            }
            v => return Err(format!("superblock version {v} isn't known")),
        };
        let mut user_block = String::new();
        if base > 0 {
            let mut ub = vec![0u8; base.min(256) as usize];
            read_exact_at(&file, &mut ub, 0).map_err(|e| e.to_string())?;
            user_block = String::from_utf8_lossy(&ub)
                .trim_end_matches(['\0', ' '])
                .chars()
                .filter(|c| !c.is_control())
                .collect();
        }
        Ok(H5 { file: Arc::new(file), len, base, so, sl, root, sb_version: version, user_block })
    }

    /// Reads `n` bytes at a file address.
    pub(crate) fn rd(&self, addr: u64, n: usize) -> Result<Vec<u8>, String> {
        if addr == UNDEF || n > MAX_META {
            return Err("an address or size in the file is invalid".into());
        }
        let abs = self.base.checked_add(addr).ok_or("an address overflows")?;
        if abs.checked_add(n as u64).is_none_or(|e| e > self.len) {
            return Err("the file ends early".into());
        }
        let mut b = vec![0u8; n];
        read_exact_at(&self.file, &mut b, abs).map_err(|e| e.to_string())?;
        Ok(b)
    }

    /// Reads up to `n` bytes at an address, as many as the file has.
    pub(crate) fn rd_upto(&self, addr: u64, n: usize) -> Result<Vec<u8>, String> {
        if addr == UNDEF {
            return Err("an address in the file is invalid".into());
        }
        let abs = self.base.checked_add(addr).ok_or("an address overflows")?;
        let have = self.len.saturating_sub(abs).min(n as u64) as usize;
        self.rd(addr, have)
    }

    pub(crate) fn off(&self, b: &[u8], at: usize) -> u64 {
        let v = le(b, at, self.so);
        if v == (u64::MAX >> (64 - 8 * self.so as u32)) { UNDEF } else { v }
    }

    pub(crate) fn len_at(&self, b: &[u8], at: usize) -> u64 {
        le(b, at, self.sl)
    }
}

/*┌─────────────────╮
| Messages        |
└─────────────────╯*/
//region Messages

#[derive(Clone, Debug)]
pub(crate) struct Msg {
    pub typ: u16,
    pub data: Vec<u8>,
}

impl H5 {
    /// All the messages of the object header at `addr`, continuations
    /// followed.
    pub(crate) fn header(&self, addr: u64) -> Result<Vec<Msg>, String> {
        let first = self.rd_upto(addr, 16)?;
        let mut msgs = Vec::new();
        let mut budget = 16usize << 20;
        if first.starts_with(b"OHDR") {
            let flags = first[5];
            let mut p = 6usize;
            // Access, modification, change and birth times; phase change values.
            if flags & 0x20 != 0 {
                p += 16;
            }
            if flags & 0x10 != 0 {
                p += 4;
            }
            let size_len = 1usize << (flags & 3);
            let head = self.rd(addr, p + size_len)?;
            let size = le(&head, p, size_len) as usize;
            let start = addr + (p + size_len) as u64;
            let mut blocks = vec![(start, size)];
            let mut seen = HashSet::new();
            while let Some((a, n)) = blocks.pop() {
                if !seen.insert(a) || seen.len() > 64 || n > budget {
                    continue;
                }
                budget -= n;
                let b = self.rd(a, n)?;
                self.messages_v2(&b, flags & 4 != 0, &mut msgs, &mut blocks)?;
                if msgs.len() > 4096 {
                    return Err("an object header has too many messages".into());
                }
            }
        } else if first.first() == Some(&1) {
            let size = le(&first, 8, 4) as usize;
            let mut blocks = vec![(addr + 16, size)];
            let mut seen = HashSet::new();
            while let Some((a, n)) = blocks.pop() {
                if !seen.insert(a) || seen.len() > 64 || n > budget {
                    continue;
                }
                budget -= n;
                let b = self.rd(a, n)?;
                let mut q = 0usize;
                while q + 8 <= b.len() {
                    let (typ, sz) = (le(&b, q, 2) as u16, le(&b, q + 2, 2) as usize);
                    q += 8;
                    let end =
                        q.checked_add(sz).filter(|&e| e <= b.len()).ok_or("a header message runs past its block")?;
                    if typ == 0x10 {
                        let (o, l) = (self.off(&b[q..end], 0), self.len_at(&b[q..end], self.so));
                        if o != UNDEF && l <= MAX_META as u64 {
                            blocks.push((o, l as usize));
                        }
                    } else {
                        msgs.push(Msg { typ, data: b[q..end].to_vec() });
                    }
                    q = end;
                    if msgs.len() > 4096 {
                        return Err("an object header has too many messages".into());
                    }
                }
            }
        } else {
            return Err("an object header is damaged".into());
        }
        Ok(msgs)
    }

    /// Messages of a version 2 header chunk (the bytes after its prefix, and
    /// before its checksum when it is a continuation).
    fn messages_v2(
        &self,
        b: &[u8],
        creation: bool,
        msgs: &mut Vec<Msg>,
        blocks: &mut Vec<(u64, usize)>,
    ) -> Result<(), String> {
        let mut q = 0usize;
        let mut body = b;
        // A continuation chunk starts with "OCHK" and ends with a checksum.
        if body.starts_with(b"OCHK") {
            q = 4;
            body = &b[..b.len().saturating_sub(4)];
        }
        let head = if creation { 6 } else { 4 };
        while q + head <= body.len() {
            let (typ, sz) = (body[q] as u16, le(body, q + 1, 2) as usize);
            q += head;
            let end = q.checked_add(sz).filter(|&e| e <= body.len()).ok_or("a header message runs past its block")?;
            if typ == 0x10 {
                let (o, l) = (self.off(&body[q..end], 0), self.len_at(&body[q..end], self.so));
                if o != UNDEF && l <= MAX_META as u64 {
                    blocks.push((o, l as usize));
                }
            } else if typ != 0 {
                msgs.push(Msg { typ, data: body[q..end].to_vec() });
            }
            q = end;
        }
        Ok(())
    }
}

//endregion

/*┌─────────────────╮
| Datatypes       |
└─────────────────╯*/
//region Datatypes

/// What a dataset stores per element.
#[derive(Clone, Debug, PartialEq)]
pub enum Elem {
    Number(DType, Endian),
    /// Not a number Slerp can show, with what it is.
    Other(String),
}

/// Parses a datatype message; returns the element and its size in bytes.
pub(crate) fn parse_datatype(b: &[u8]) -> Result<(Elem, usize, usize), String> {
    if b.len() < 8 {
        return Err("a datatype is cut short".into());
    }
    let class = b[0] & 0x0f;
    let bits = le(b, 1, 3) as u32;
    let size = le(b, 4, 4) as usize;
    let endian = if bits & 1 == 0 { Endian::Little } else { Endian::Big };
    match class {
        0 => {
            let signed = bits & 8 != 0;
            let (offset, precision) = (le(b, 8, 2) as usize, le(b, 10, 2) as usize);
            if offset != 0 || precision != size * 8 {
                return Ok((Elem::Other(format!("{precision}-bit integers at a bit offset")), size, 12));
            }
            let t = match (size, signed) {
                (1, false) => DType::U8,
                (1, true) => DType::I8,
                (2, false) => DType::U16,
                (2, true) => DType::I16,
                (4, false) => DType::U32,
                (4, true) => DType::I32,
                (8, false) => DType::U64,
                (8, true) => DType::I64,
                _ => return Ok((Elem::Other(format!("{size}-byte integers")), size, 12)),
            };
            Ok((Elem::Number(t, endian), size, 12))
        }
        1 => {
            // exponent location, size, mantissa location, size, bias
            let (eloc, esz, mloc, msz) = (
                b.get(12).copied().unwrap_or(0),
                b.get(13).copied().unwrap_or(0),
                b.get(14).copied().unwrap_or(0),
                b.get(15).copied().unwrap_or(0),
            );
            let bias = le(b, 16, 4);
            let vax = bits & 0x40 != 0;
            let t = match (size, eloc, esz, mloc, msz, bias) {
                (2, 10, 5, 0, 10, 15) => DType::F16,
                (4, 23, 8, 0, 23, 127) => DType::F32,
                (8, 52, 11, 0, 52, 1023) => DType::F64,
                _ => {
                    return Ok((
                        Elem::Other(format!("{size}-byte floating point numbers of a non-IEEE layout")),
                        size,
                        24,
                    ));
                }
            };
            if vax {
                return Ok((Elem::Other("VAX floating point numbers".into()), size, 24));
            }
            Ok((Elem::Number(t, endian), size, 24))
        }
        3 => Ok((Elem::Other("fixed-length strings".into()), size, 8)),
        4 => Ok((Elem::Other("bit fields".into()), size, 12)),
        5 => Ok((Elem::Other("opaque data".into()), size, 8)),
        6 => Ok((Elem::Other("compound records".into()), size, 8)),
        7 => Ok((Elem::Other("object references".into()), size, 8)),
        8 => {
            // An enumeration over integers; h5py stores booleans as one with
            // FALSE and TRUE.
            let n = (bits & 0xffff) as usize;
            let (base, bsz, used) = parse_datatype(&b[8..]).map_err(|_| "an enumeration is damaged".to_string())?;
            let mut p = 8 + used;
            let version = b[0] >> 4;
            let mut names = Vec::new();
            for _ in 0..n.min(1024) {
                let end = b.get(p..).and_then(|s| s.iter().position(|&c| c == 0)).ok_or("an enumeration is damaged")?;
                names.push(String::from_utf8_lossy(&b[p..p + end]).into_owned());
                // Older versions pad each name to a multiple of 8 bytes.
                p += if version < 3 { (end + 1 + 7) & !7 } else { end + 1 };
            }
            let is_bool = n == 2
                && names.first().is_some_and(|s| s == "FALSE")
                && names.get(1).is_some_and(|s| s == "TRUE")
                && bsz == 1;
            match base {
                Elem::Number(t, e) => Ok((Elem::Number(if is_bool { DType::Bool } else { t }, e), size, 8 + used)),
                other => Ok((other, size, 8)),
            }
        }
        9 => Ok((Elem::Other("variable-length data".into()), size, 8)),
        10 => Ok((Elem::Other("array types".into()), size, 8)),
        c => Ok((Elem::Other(format!("datatype class {c}")), size, 8)),
    }
}

//endregion

/*┌─────────────────╮
| Datasets        |
└─────────────────╯*/
//region Datasets

#[derive(Clone, Debug)]
pub(crate) enum Layout {
    Compact(Vec<u8>),
    Contiguous { addr: u64, size: u64 },
    Chunked { dims: Vec<u64>, index: chunks::Index },
}

#[derive(Clone, Debug)]
pub(crate) struct Filter {
    pub id: u16,
    pub values: Vec<u32>,
}

#[derive(Clone, Debug)]
pub enum Attr {
    Text(String),
    Numbers(Vec<f64>),
}

/// A dataset's description, read from its object header.
#[derive(Clone, Debug)]
pub struct Dataset {
    pub path: String,
    pub shape: Vec<u64>,
    pub elem: Elem,
    pub elem_size: usize,
    pub(crate) layout: Layout,
    pub(crate) filters: Vec<Filter>,
    pub attrs: Vec<(String, Attr)>,
    pub chunk_shape: Option<Vec<u64>>,
}

fn pad8(n: usize) -> usize {
    (n + 7) & !7
}

impl H5 {
    /// Parses an attribute message into a name and value, when it is text or
    /// numbers.
    fn attribute(&self, d: &[u8]) -> Option<(String, Attr)> {
        let ver = *d.first()?;
        let (name_len, dt_len, ds_len) = (le(d, 2, 2) as usize, le(d, 4, 2) as usize, le(d, 6, 2) as usize);
        let (mut p, pad) = match ver {
            1 => (8usize, true),
            2 => (8, false),
            3 => (9, false),
            _ => return None,
        };
        let take = |p: &mut usize, n: usize| -> Option<&[u8]> {
            let s = d.get(*p..p.checked_add(n)?)?;
            *p += if pad { pad8(n) } else { n };
            Some(s)
        };
        let name = String::from_utf8_lossy(take(&mut p, name_len)?).trim_end_matches('\0').to_string();
        let dt = take(&mut p, dt_len)?;
        let ds = take(&mut p, ds_len)?;
        let data = d.get(p..)?;
        let (elem, size, _) = parse_datatype(dt).ok()?;
        // The number of elements from the dataspace (any number in a
        // damaged file: the product saturates and is capped below).
        let count = match *ds.first()? {
            1 => {
                let r = *ds.get(1)? as usize;
                (0..r).map(|i| le(ds, 8 + i * self.sl, self.sl)).fold(1u64, u64::saturating_mul)
            }
            2 => {
                let r = *ds.get(1)? as usize;
                if ds.get(3) == Some(&0) {
                    1
                } else {
                    (0..r).map(|i| le(ds, 4 + i * self.sl, self.sl)).fold(1u64, u64::saturating_mul)
                }
            }
            _ => return None,
        };
        let count = count.min(1 << 16) as usize;
        match elem {
            Elem::Other(ref w) if w == "fixed-length strings" => {
                let n = (count * size).min(data.len());
                Some((name, Attr::Text(String::from_utf8_lossy(&data[..n]).trim_end_matches('\0').to_string())))
            }
            Elem::Number(t, e) => {
                let vals =
                    data.chunks_exact(size.max(1)).take(count).map(|c| crate::dtype::decode_f64(t, e, c)).collect();
                Some((name, Attr::Numbers(vals)))
            }
            _ => None,
        }
    }

    /// Reads a dataset's description from the header at `addr`; `None` when
    /// the object isn't a dataset.
    fn dataset_at(&self, addr: u64, path: &str, msgs: &[Msg]) -> Result<Option<Dataset>, String> {
        let find = |t: u16| msgs.iter().find(|m| m.typ == t);
        let (Some(ds), Some(dt), Some(lay)) = (find(0x01), find(0x03), find(0x08)) else { return Ok(None) };
        let _ = addr;
        // Dataspace.
        let d = &ds.data;
        let rank = *d.get(1).ok_or("a dataspace is cut short")? as usize;
        let (head, scalar) = match d.first() {
            Some(1) => (8, false),
            Some(2) => (4, d.get(3) == Some(&0)),
            _ => return Err("an unknown dataspace version".into()),
        };
        if rank > 32 {
            return Err("a dataset has too many dimensions".into());
        }
        let shape: Vec<u64> =
            if scalar { Vec::new() } else { (0..rank).map(|i| le(d, head + i * self.sl, self.sl)).collect() };
        if d.len() < head + shape.len() * self.sl {
            return Err("a dataspace is cut short".into());
        }
        let (elem, elem_size, _) = parse_datatype(&dt.data)?;
        // Layout.
        let l = &lay.data;
        let (ver, class) = (*l.first().ok_or("a layout is cut short")?, *l.get(1).ok_or("a layout is cut short")?);
        let mut chunk_shape = None;
        let layout = match (ver, class) {
            (3 | 4, 0) => {
                let n = le(l, 2, 2) as usize;
                Layout::Compact(l.get(4..4 + n).ok_or("compact data is cut short")?.to_vec())
            }
            (3 | 4, 1) => Layout::Contiguous { addr: self.off(l, 2), size: self.len_at(l, 2 + self.so) },
            (3, 2) => {
                let nd = *l.get(2).ok_or("a layout is cut short")? as usize;
                let btree = self.off(l, 3);
                if !(2..=33).contains(&nd) {
                    return Err("a chunked layout has an invalid rank".into());
                }
                let dims: Vec<u64> = (0..nd).map(|i| le(l, 3 + self.so + 4 * i, 4)).collect();
                chunk_shape = Some(dims[..nd - 1].to_vec());
                Layout::Chunked { dims: dims[..nd - 1].to_vec(), index: chunks::Index::BTreeV1(btree) }
            }
            (4, 2) => {
                let flags = *l.get(2).ok_or("a layout is cut short")?;
                let nd = *l.get(3).ok_or("a layout is cut short")? as usize;
                let enc = *l.get(4).ok_or("a layout is cut short")? as usize;
                if !(2..=33).contains(&nd) || !(1..=8).contains(&enc) {
                    return Err("a chunked layout is damaged".into());
                }
                let dims: Vec<u64> = (0..nd).map(|i| le(l, 5 + i * enc, enc)).collect();
                let p = 5 + nd * enc;
                let kind = *l.get(p).ok_or("a layout is cut short")?;
                let info = p + 1;
                chunk_shape = Some(dims[..nd - 1].to_vec());
                let index = match kind {
                    1 => {
                        let filtered = flags & 2 != 0;
                        let (size, mask, a) = if filtered {
                            (self.len_at(l, info), le(l, info + self.sl, 4) as u32, info + self.sl + 4)
                        } else {
                            (0, 0, info)
                        };
                        chunks::Index::Single { addr: self.off(l, a), filtered: filtered.then_some((size, mask)) }
                    }
                    2 => chunks::Index::Implicit(self.off(l, info)),
                    3 => chunks::Index::Fixed {
                        page_bits: *l.get(info).ok_or("a layout is cut short")?,
                        addr: self.off(l, info + 1),
                    },
                    4 => chunks::Index::Extensible,
                    5 => chunks::Index::BTreeV2 { addr: self.off(l, info + 6) },
                    k => return Err(format!("the chunk index type {k} isn't known")),
                };
                Layout::Chunked { dims: dims[..nd - 1].to_vec(), index }
            }
            (v, _) if v > 4 => return Err(format!("data layout version {v} (written by a newer HDF5) isn't read")),
            (_, c) => return Err(format!("the layout class {c} isn't supported")),
        };
        // Filters.
        let mut filters = Vec::new();
        if let Some(f) = find(0x0b) {
            filters = parse_filters(&f.data)?;
        }
        let mut attrs = Vec::new();
        for m in msgs.iter().filter(|m| m.typ == 0x0c) {
            if let Some(a) = self.attribute(&m.data) {
                attrs.push(a);
            }
        }
        Ok(Some(Dataset { path: path.to_string(), shape, elem, elem_size, layout, filters, attrs, chunk_shape }))
    }
}

fn parse_filters(d: &[u8]) -> Result<Vec<Filter>, String> {
    let ver = *d.first().ok_or("a filter pipeline is cut short")?;
    let n = *d.get(1).ok_or("a filter pipeline is cut short")? as usize;
    let mut p = if ver == 1 { 8 } else { 2 };
    let mut out = Vec::new();
    for _ in 0..n {
        let id = le(d, p, 2) as u16;
        p += 2;
        let name_len = if ver == 1 || id >= 256 {
            let n = le(d, p, 2) as usize;
            p += 2;
            n
        } else {
            0
        };
        let _flags = le(d, p, 2);
        let nvals = le(d, p + 2, 2) as usize;
        p += 4;
        p += if ver == 1 { pad8(name_len) } else { name_len };
        if nvals > 64 || p + 4 * nvals > d.len() {
            return Err("a filter pipeline is damaged".into());
        }
        let values = (0..nvals).map(|i| le(d, p + 4 * i, 4) as u32).collect();
        p += 4 * nvals;
        if ver == 1 && nvals % 2 == 1 {
            p += 4;
        }
        out.push(Filter { id, values });
    }
    Ok(out)
}

//endregion

/*┌─────────────────╮
| Groups          |
└─────────────────╯*/
//region Groups

/// One member of a group.
#[derive(Clone, Debug)]
pub(crate) struct Link {
    pub name: String,
    pub addr: u64,
}

impl H5 {
    /// The hard links of the group whose header messages are `msgs`.
    fn links(&self, msgs: &[Msg]) -> Result<Vec<Link>, String> {
        let mut out = Vec::new();
        // Old style: a B-tree of symbol table nodes with names in a local heap.
        if let Some(m) = msgs.iter().find(|m| m.typ == 0x11) {
            let (tree, heap) = (self.off(&m.data, 0), self.off(&m.data, self.so));
            self.symbol_table(tree, heap, &mut out)?;
        }
        // New style: link messages in the header, or a dense heap.
        for m in msgs.iter().filter(|m| m.typ == 0x06) {
            if let Some(l) = self.link_message(&m.data) {
                out.push(l);
            }
        }
        if let Some(m) = msgs.iter().find(|m| m.typ == 0x02) {
            let d = &m.data;
            let flags = d.get(1).copied().unwrap_or(0);
            let p = 2 + if flags & 1 != 0 { 8 } else { 0 };
            let (heap, tree) = (self.off(d, p), self.off(d, p + self.so));
            if heap != UNDEF && tree != UNDEF {
                for body in self.dense_links(heap, tree)? {
                    if let Some(l) = self.link_message(&body) {
                        out.push(l);
                    }
                }
            }
        }
        if out.len() > MAX_OBJECTS {
            return Err("a group has too many members".into());
        }
        Ok(out)
    }

    fn link_message(&self, d: &[u8]) -> Option<Link> {
        if *d.first()? != 1 {
            return None;
        }
        let flags = *d.get(1)?;
        let mut p = 2usize;
        let kind = if flags & 8 != 0 {
            p += 1;
            *d.get(2)?
        } else {
            0
        };
        if flags & 4 != 0 {
            p += 8;
        }
        if flags & 16 != 0 {
            p += 1;
        }
        let nl = 1usize << (flags & 3);
        let name_len = le(d, p, nl) as usize;
        p += nl;
        let name = String::from_utf8_lossy(d.get(p..p.checked_add(name_len)?)?).into_owned();
        p += name_len;
        // Only hard links lead to objects in this file.
        (kind == 0).then(|| Link { name, addr: self.off(d, p) })
    }

    fn symbol_table(&self, tree: u64, heap: u64, out: &mut Vec<Link>) -> Result<(), String> {
        let h = self.rd(heap, 8 + 2 * self.sl + self.so)?;
        if &h[..4] != b"HEAP" {
            return Err("a group's name heap is damaged".into());
        }
        let data_addr = self.off(&h, 8 + 2 * self.sl);
        let data_size = self.len_at(&h, 8) as usize;
        let names = self.rd(data_addr, data_size.min(MAX_META))?;
        let mut stack = vec![tree];
        let mut seen = HashSet::new();
        while let Some(a) = stack.pop() {
            if a == UNDEF || !seen.insert(a) || seen.len() > 1 << 16 {
                continue;
            }
            let h = self.rd(a, 8 + 2 * self.so)?;
            if &h[..4] != b"TREE" {
                return Err("a group's B-tree is damaged".into());
            }
            let (level, used) = (h[5], le(&h, 6, 2) as usize);
            if used > 1 << 12 {
                return Err("a group's B-tree node is too large".into());
            }
            // Keys (a length each) alternate with children (an address each).
            let body = self.rd(a + (8 + 2 * self.so) as u64, self.sl + used * (self.so + self.sl))?;
            for i in 0..used {
                let child = self.off(&body, self.sl + i * (self.sl + self.so));
                if level > 0 {
                    stack.push(child);
                } else {
                    let node = self.rd(child, 8)?;
                    if &node[..4] != b"SNOD" {
                        return Err("a group's symbol table node is damaged".into());
                    }
                    let n = le(&node, 6, 2) as usize;
                    let esz = 2 * self.so + 8 + 16;
                    if n > 64 {
                        return Err("a symbol table node is too large".into());
                    }
                    let entries = self.rd(child + 8, n * esz)?;
                    for k in 0..n {
                        let e = &entries[k * esz..(k + 1) * esz];
                        let name_off = le(e, 0, self.so) as usize;
                        let obj = self.off(e, self.so);
                        let tail = names.get(name_off..).ok_or("a name lies outside its heap")?;
                        let end = tail.iter().position(|&c| c == 0).unwrap_or(tail.len());
                        out.push(Link { name: String::from_utf8_lossy(&tail[..end]).into_owned(), addr: obj });
                    }
                }
            }
        }
        Ok(())
    }
}

//endregion

/// Everything a file lists: its datasets, and why some objects were passed over.
pub fn datasets(h: &H5) -> Result<(Vec<Dataset>, Vec<String>), String> {
    let mut out = Vec::new();
    let mut problems = Vec::new();
    let mut seen = HashSet::new();
    let mut work = vec![(h.root, String::new(), 0usize)];
    let mut objects = 0usize;
    while let Some((addr, path, depth)) = work.pop() {
        if addr == UNDEF || !seen.insert(addr) || depth > MAX_DEPTH {
            continue;
        }
        objects += 1;
        if objects > MAX_OBJECTS {
            return Err("the file has too many objects".into());
        }
        let msgs = h.header(addr)?;
        match h.dataset_at(addr, &path, &msgs) {
            Ok(Some(d)) => out.push(d),
            Ok(None) => {
                // A group: follow its members, in file order.
                let mut links = match h.links(&msgs) {
                    Ok(l) => l,
                    Err(e) => {
                        problems.push(format!("{}: {e}", if path.is_empty() { "/" } else { &path }));
                        Vec::new()
                    }
                };
                links.sort_by(|a, b| b.name.cmp(&a.name));
                for l in links {
                    work.push((l.addr, format!("{path}/{}", l.name), depth + 1));
                }
            }
            // A dataset this reader can't describe is skipped with its siblings intact.
            Err(e) => problems.push(format!("{path}: {e}")),
        }
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    Ok((out, problems))
}

impl Dataset {
    pub fn elements(&self) -> u64 {
        self.shape.iter().fold(1u64, |n, &d| n.saturating_mul(d))
    }

    pub fn bytes(&self) -> u64 {
        self.elements().saturating_mul(self.elem_size as u64)
    }

    /// Why this dataset can't be shown as a picture, if it can't.
    pub fn problem(&self) -> Option<String> {
        if let Elem::Other(w) = &self.elem {
            return Some(format!("holds {w}"));
        }
        None
    }

    /// Candidate pictures: numbers in two to four dimensions with enough pixels.
    pub fn is_picture(&self) -> bool {
        let n = self.shape.len();
        if self.problem().is_some() || !(2..=4).contains(&n) {
            return false;
        }
        // The last two dimensions are the picture, or the last is a colour
        // axis and the two before it are.
        let colour = matches!(self.shape[n - 1], 3 | 4) && n >= 3;
        let (a, b) =
            if colour { (self.shape[n - 3], self.shape[n - 2]) } else { (self.shape[n - 2], self.shape[n - 1]) };
        a >= 4 && b >= 4
    }
}

/// The bytes of a dataset as one flat run in row-major order.
pub fn source(h: &Arc<H5>, d: &Dataset) -> Result<Arc<dyn ByteSource>, String> {
    let total = d.bytes();
    match &d.layout {
        Layout::Compact(b) => Ok(Arc::new(MemSource(b.clone()))),
        Layout::Contiguous { addr, size } => {
            if *addr == UNDEF {
                // Never written: all zeros.
                return Ok(Arc::new(chunks::Zeros(total)));
            }
            let start = h.base.checked_add(*addr).filter(|&s| s <= h.len).ok_or("a dataset lies outside the file")?;
            let avail = h.len - start;
            Ok(Arc::new(FileSource::new(
                h.file.try_clone().map_err(|e| e.to_string())?,
                start,
                total.min(*size).min(avail),
            )))
        }
        Layout::Chunked { dims, index } => {
            Ok(Arc::new(chunks::ChunkedSource::new(h.clone(), d, dims.clone(), index.clone())?))
        }
    }
}

fn text_attr(d: &Dataset, key: &str) -> String {
    d.attrs
        .iter()
        .find(|(k, _)| k == key)
        .and_then(|(_, v)| if let Attr::Text(t) = v { Some(t.clone()) } else { None })
        .unwrap_or_default()
}

fn num_attr(d: &Dataset, key: &str) -> Option<f64> {
    d.attrs
        .iter()
        .find(|(k, _)| k == key)
        .and_then(|(_, v)| if let Attr::Numbers(n) = v { n.first().copied() } else { None })
}

fn bad(e: impl std::fmt::Display) -> LoadError {
    LoadError::Invalid(e.to_string())
}

/// The picture datasets of a file, MATLAB files included, and which names to
/// list as parts.
pub struct Listing {
    pub h: Arc<H5>,
    pub all: Vec<Dataset>,
    pub pictures: Vec<usize>,
    pub matlab: bool,
    /// Objects that couldn't be read, and why.
    pub problems: Vec<String>,
}

pub fn list(path: &Path) -> Result<Listing, LoadError> {
    let h = Arc::new(H5::open(path).map_err(bad)?);
    let (all, problems) = datasets(&h).map_err(bad)?;
    let matlab =
        h.user_block.starts_with("MATLAB 7.3") || path.extension().is_some_and(|e| e.eq_ignore_ascii_case("mat"));
    // Numbers only; MATLAB strings are uint16 arrays tagged "char".
    let shown = |i: usize| -> bool {
        let d = &all[i];
        // Coordinate variables (NetCDF-4's, HDF5 dimension scales) only label axes.
        d.problem().is_none()
            && !d.shape.is_empty()
            && d.elements() >= 2
            && text_attr(d, "CLASS") != "DIMENSION_SCALE"
            && !(matlab && text_attr(d, "MATLAB_class") == "char")
    };
    // Pictures first, so the first part is the one to show; other arrays open
    // with their statistics.
    let mut pictures: Vec<usize> = (0..all.len()).filter(|&i| shown(i) && all[i].is_picture()).collect();
    pictures.extend((0..all.len()).filter(|&i| shown(i) && !all[i].is_picture()));
    Ok(Listing { h, all, pictures, matlab, problems })
}

impl Listing {
    pub fn part_names(&self) -> Vec<String> {
        self.pictures.iter().map(|&i| self.all[i].path.trim_start_matches('/').to_string()).collect()
    }

    /// Opens the picture dataset called `part`, or the first.
    pub fn open(&self, part: Option<&str>) -> Result<(Volume, String), LoadError> {
        if let Some(p) = part {
            let idx = *self
                .pictures
                .iter()
                .find(|&&i| self.all[i].path.trim_start_matches('/') == p)
                .ok_or_else(|| LoadError::Unsupported(format!("no dataset called {p}")))?;
            return self.open_index(idx);
        }
        // With no part named, the first dataset that opens.
        let mut first_error = None;
        for &i in &self.pictures {
            match self.open_index(i) {
                Ok(v) => return Ok(v),
                Err(e) => {
                    first_error.get_or_insert(e);
                }
            }
        }
        Err(first_error.unwrap_or_else(|| {
            let why = self
                .all
                .iter()
                .find_map(|d| d.problem().map(|p| format!("{} {p}", d.path)))
                .or_else(|| self.problems.first().cloned());
            LoadError::Unsupported(format!(
                "the file has no array Slerp can read{}",
                why.map(|w| format!(" ({w})")).unwrap_or_default()
            ))
        }))
    }

    fn open_index(&self, idx: usize) -> Result<(Volume, String), LoadError> {
        let d = &self.all[idx];
        let source = source(&self.h, d).map_err(bad)?;
        let Elem::Number(dtype, endian) = d.elem else {
            return Err(LoadError::Unsupported("the dataset's type".into()));
        };
        let name = d.path.trim_start_matches('/').to_string();
        // Logical shape: MATLAB stores its arrays with the dimensions reversed.
        let n = d.shape.len();
        let mut strides = vec![0usize; n];
        let mut acc = 1u64;
        for k in (0..n).rev() {
            strides[k] = usize::try_from(acc).map_err(|_| bad("the dataset is absurdly large"))?;
            acc = acc
                .checked_mul(d.shape[k].max(1))
                .filter(|&a| a < 1 << 50)
                .ok_or_else(|| bad("the dataset is absurdly large"))?;
        }
        let order: Vec<usize> = if self.matlab { (0..n).rev().collect() } else { (0..n).collect() };
        let shape: Vec<u64> = order.iter().map(|&k| d.shape[k]).collect();
        let (mut roles, guess, no_picture) = array_roles(&shape);
        if self.matlab && n == 3 && shape[2] != 3 && shape[2] != 4 {
            // MATLAB volumes are rows, columns, slices.
            roles = vec![crate::volume::Role::Y, crate::volume::Role::X, crate::volume::Role::Z];
        }
        let mut axes = Vec::new();
        for (i, &k) in order.iter().enumerate() {
            let len = usize::try_from(d.shape[k]).map_err(|_| bad("a dimension is absurdly large"))?;
            axes.push(Axis::new(len, strides[k], roles[i]).named(format!("axis {i}")));
        }
        let mut v = Volume::new(
            name.clone(),
            if self.matlab { "MATLAB v7.3 (HDF5)" } else { "HDF5" },
            axes,
            dtype,
            endian,
            source,
        );
        v.slice_axis_guess = guess && !self.matlab;
        v.no_picture = no_picture;
        v.fact("Dataset", d.path.clone());
        v.fact("Shape", format!("({})", d.shape.iter().map(|s| s.to_string()).collect::<Vec<_>>().join(", ")));
        v.fact("Type", dtype.name());
        if let Some(c) = &d.chunk_shape {
            v.fact("Chunks", format!("({})", c.iter().map(|s| s.to_string()).collect::<Vec<_>>().join(", ")));
        }
        if !d.filters.is_empty() {
            v.fact("Filters", d.filters.iter().map(|f| filter_name(f.id)).collect::<Vec<_>>().join(", "));
        }
        if self.matlab {
            v.fact("MATLAB class", text_attr(d, "MATLAB_class"));
            v.fact("Axis order", "MATLAB column-major: rows, columns, slices");
        }
        v.fact("Units", text_attr(d, "units"));
        v.fact("Long name", text_attr(d, "long_name"));
        v.value_name = text_attr(d, "units");
        if let Some(s) = num_attr(d, "scale_factor").filter(|s| *s != 0.0) {
            v.slope = s;
        }
        if let Some(o) = num_attr(d, "add_offset") {
            v.inter = o;
        }
        v.fill = num_attr(d, "_FillValue").or_else(|| num_attr(d, "missing_value"));
        if self.matlab && n >= 2 {
            let _ = guess;
        }
        // NetCDF-4: a latitude variable beside the data tells which way is north.
        if n >= 2 {
            self.orient_latitude(&mut v, d);
        }
        for (k, val) in &d.attrs {
            if matches!(
                k.as_str(),
                "units"
                    | "long_name"
                    | "MATLAB_class"
                    | "CLASS"
                    | "NAME"
                    | "REFERENCE_LIST"
                    | "DIMENSION_LIST"
                    | "_FillValue"
                    | "scale_factor"
                    | "add_offset"
            ) {
                continue;
            }
            match val {
                Attr::Text(t) if !t.is_empty() => v.facts.push((k.clone(), t.chars().take(300).collect())),
                Attr::Numbers(x) if !x.is_empty() => {
                    v.facts.push((k.clone(), x.iter().take(6).map(|y| y.to_string()).collect::<Vec<_>>().join(", ")))
                }
                _ => {}
            }
        }
        v.fit().map_err(bad)?;
        Ok((v, name))
    }

    /// A sibling 1-D dataset named like a latitude, as long as the rows: when it
    /// counts upwards the rows are drawn with north up.
    fn orient_latitude(&self, v: &mut Volume, d: &Dataset) {
        let n = d.shape.len();
        let rows = d.shape[n - 2];
        let dir = d.path.rsplit_once('/').map_or("", |x| x.0);
        for c in &self.all {
            let (cdir, cname) = c.path.rsplit_once('/').unwrap_or(("", &c.path));
            let lname = cname.to_ascii_lowercase();
            if cdir != dir
                || c.shape.len() != 1
                || c.shape[0] != rows
                || !matches!(lname.as_str(), "lat" | "latitude" | "y")
            {
                continue;
            }
            let (Elem::Number(t, e), Ok(src)) = (&c.elem, source(&self.h, c)) else { continue };
            let sz = c.elem_size;
            let (mut a, mut b) = ([0u8; 8], [0u8; 8]);
            if sz <= 8
                && src.read_at(0, &mut a[..sz]).is_ok()
                && src.read_at((rows - 1) * sz as u64, &mut b[..sz]).is_ok()
            {
                let (first, last) =
                    (crate::dtype::decode_f64(*t, *e, &a[..sz]), crate::dtype::decode_f64(*t, *e, &b[..sz]));
                if last > first {
                    let y = v.axes.len() - 2;
                    v.axes[y].reverse = true;
                }
            }
            return;
        }
    }
}

fn filter_name(id: u16) -> String {
    match id {
        1 => "deflate".into(),
        2 => "shuffle".into(),
        3 => "Fletcher-32".into(),
        4 => "SZIP".into(),
        5 => "N-bit".into(),
        6 => "scale-offset".into(),
        32000 => "LZF".into(),
        other => format!("filter {other}"),
    }
}
