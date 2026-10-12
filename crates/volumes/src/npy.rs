//! NumPy `.npy` and `.npz` files.
//!
//! The header is a Python dictionary literal, which this module parses as
//! data and never evaluates.  Object arrays (pickled Python objects) and
//! structured arrays are refused with an explanation; nothing here ever
//! unpickles anything.

use crate::dtype::{DType, Endian};
use crate::source::{FileRange, FileSource, Inflater, StreamSource, Wrapper, read_exact_at};
use crate::volume::{Axis, Role, Volume};
use crate::zip;
use slerp_scene::LoadError;
use std::fs::File;
use std::io::Read;
use std::sync::Arc;

pub const MAGIC: &[u8] = b"\x93NUMPY";

/// The longest header read; NumPy itself refuses more than 10000 bytes by
/// default.
const MAX_HEADER: usize = 1 << 20;

/// What an `.npy` header says.
#[derive(Clone, Debug, PartialEq)]
pub struct Header {
    pub dtype: DType,
    pub endian: Endian,
    pub fortran: bool,
    pub shape: Vec<u64>,
    pub version: (u8, u8),
    /// Where the array's bytes begin, counted from the start of the `.npy`
    /// data.
    pub data_offset: u64,
    pub descr: String,
}

/*┌─────────────────╮
| Python literals |
└─────────────────╯*/
//region Python literals

#[derive(Clone, Debug, PartialEq)]
enum Py {
    Str(String),
    Int(i64),
    Bool(bool),
    None,
    Seq(Vec<Py>),
    Dict(Vec<(String, Py)>),
}

struct Lexer<'a> {
    s: &'a [u8],
    i: usize,
    depth: usize,
}

impl Lexer<'_> {
    fn ws(&mut self) {
        while self.s.get(self.i).is_some_and(|c| c.is_ascii_whitespace()) {
            self.i += 1;
        }
    }

    fn eat(&mut self, c: u8) -> bool {
        self.ws();
        if self.s.get(self.i) == Some(&c) {
            self.i += 1;
            true
        } else {
            false
        }
    }

    fn value(&mut self) -> Result<Py, String> {
        self.ws();
        self.depth += 1;
        if self.depth > 8 {
            return Err("the header nests too deeply".into());
        }
        let v = match self.s.get(self.i).copied() {
            Some(q @ (b'\'' | b'"')) => {
                self.i += 1;
                let start = self.i;
                while self.s.get(self.i).is_some_and(|&c| c != q) {
                    self.i += 1;
                }
                if self.i >= self.s.len() {
                    return Err("unterminated string in the header".into());
                }
                let t: String = self.s[start..self.i].iter().map(|&b| b as char).collect();
                self.i += 1;
                Py::Str(t)
            }
            Some(b'(') | Some(b'[') => {
                let close = if self.s[self.i] == b'(' { b')' } else { b']' };
                self.i += 1;
                let mut items = Vec::new();
                loop {
                    if self.eat(close) {
                        break;
                    }
                    if items.len() >= 64 {
                        return Err("the header's lists are too long".into());
                    }
                    items.push(self.value()?);
                    if !self.eat(b',') && !self.eat(close) {
                        return Err("expected a comma in the header".into());
                    }
                    if self.s.get(self.i.wrapping_sub(1)) == Some(&close) {
                        break;
                    }
                }
                Py::Seq(items)
            }
            Some(b'{') => {
                self.i += 1;
                let mut items = Vec::new();
                loop {
                    if self.eat(b'}') {
                        break;
                    }
                    if items.len() >= 64 {
                        return Err("the header has too many keys".into());
                    }
                    let Py::Str(key) = self.value()? else { return Err("a header key isn't text".into()) };
                    if !self.eat(b':') {
                        return Err("expected a colon in the header".into());
                    }
                    let v = self.value()?;
                    items.push((key, v));
                    if !self.eat(b',') {
                        if self.eat(b'}') {
                            break;
                        }
                        return Err("expected a comma in the header".into());
                    }
                }
                Py::Dict(items)
            }
            Some(c) if c.is_ascii_digit() || c == b'-' => {
                let start = self.i;
                self.i += 1;
                while self.s.get(self.i).is_some_and(|c| c.is_ascii_digit()) {
                    self.i += 1;
                }
                let t = std::str::from_utf8(&self.s[start..self.i]).unwrap_or("0");
                // Python 2 wrote long integers with a trailing L.
                if self.s.get(self.i) == Some(&b'L') {
                    self.i += 1;
                }
                Py::Int(t.parse::<i64>().map_err(|_| "a number in the header is out of range".to_string())?)
            }
            Some(c) if c.is_ascii_alphabetic() => {
                let start = self.i;
                while self.s.get(self.i).is_some_and(|c| c.is_ascii_alphanumeric() || *c == b'_') {
                    self.i += 1;
                }
                match &self.s[start..self.i] {
                    b"True" => Py::Bool(true),
                    b"False" => Py::Bool(false),
                    b"None" => Py::None,
                    _ => return Err("the header holds something other than plain data".into()),
                }
            }
            _ => return Err("the header isn't a dictionary of plain data".into()),
        };
        self.depth -= 1;
        Ok(v)
    }
}

//endregion

/// Reads the dtype string of a header: `<f4`, `|u1`, `>c16`.
fn parse_descr(s: &str) -> Result<(DType, Endian), String> {
    let (order, rest) = match s.chars().next() {
        Some(c @ ('<' | '>' | '|' | '=')) => (c, &s[1..]),
        _ => ('=', s),
    };
    let endian = match order {
        '>' => Endian::Big,
        '<' => Endian::Little,
        _ => Endian::NATIVE,
    };
    let kind = rest.chars().next().ok_or("the dtype is empty")?;
    let size: usize = rest[kind.len_utf8()..].parse().unwrap_or(0);
    let t = match (kind, size) {
        ('b', 1) | ('?', 1) => DType::Bool,
        ('u', 1) => DType::U8,
        ('u', 2) => DType::U16,
        ('u', 4) => DType::U32,
        ('u', 8) => DType::U64,
        ('i', 1) => DType::I8,
        ('i', 2) => DType::I16,
        ('i', 4) => DType::I32,
        ('i', 8) => DType::I64,
        ('f', 2) => DType::F16,
        ('f', 4) => DType::F32,
        ('f', 8) => DType::F64,
        ('c', 8) => DType::C64,
        ('c', 16) => DType::C128,
        ('f', _) | ('c', _) => return Err(format!("extended-precision floats ({s}) aren't supported")),
        ('O', _) => {
            return Err("object arrays hold pickled Python objects, which Slerp never loads".into());
        }
        ('S' | 'a' | 'U', _) => return Err("text arrays have no picture".into()),
        ('V', _) => return Err("structured (record) arrays aren't supported".into()),
        ('M' | 'm', _) => return Err("date and time arrays aren't supported".into()),
        _ => return Err(format!("the dtype '{s}' isn't supported")),
    };
    Ok((t, endian))
}

/// Parses the start of an `.npy` file.  `prefix` must hold the whole header,
/// which `header_length` finds out from the first 12 bytes.
pub fn parse_header(prefix: &[u8]) -> Result<Header, String> {
    if !prefix.starts_with(MAGIC) || prefix.len() < 10 {
        return Err("this isn't a NumPy file".into());
    }
    let version = (prefix[6], prefix[7]);
    let (len, start) = header_length(prefix)?;
    let end = start + len;
    let text = prefix.get(start..end).ok_or("the file ends inside the header")?;
    let mut lx = Lexer { s: text, i: 0, depth: 0 };
    let Py::Dict(dict) = lx.value()? else { return Err("the header isn't a dictionary".into()) };
    let get = |k: &str| dict.iter().find(|(n, _)| n == k).map(|(_, v)| v);
    let descr = match get("descr") {
        Some(Py::Str(s)) => s.clone(),
        Some(Py::Seq(_)) => return Err("structured (record) arrays aren't supported".into()),
        _ => return Err("the header has no dtype".into()),
    };
    let (dtype, endian) = parse_descr(&descr)?;
    let fortran = matches!(get("fortran_order"), Some(Py::Bool(true)));
    let shape = match get("shape") {
        Some(Py::Seq(items)) => items
            .iter()
            .map(|i| match i {
                Py::Int(n) if *n >= 0 => Ok(*n as u64),
                _ => Err("the shape holds something other than sizes".to_string()),
            })
            .collect::<Result<Vec<_>, _>>()?,
        _ => return Err("the header has no shape".into()),
    };
    Ok(Header { dtype, endian, fortran, shape, version, data_offset: end as u64, descr })
}

/// The header's length and where it starts, from an `.npy` file's first
/// bytes.
pub fn header_length(prefix: &[u8]) -> Result<(usize, usize), String> {
    let (len, start) = match prefix.get(6) {
        Some(1) => (u16::from_le_bytes([prefix[8], prefix[9]]) as usize, 10),
        Some(2 | 3) if prefix.len() >= 12 => {
            (u32::from_le_bytes([prefix[8], prefix[9], prefix[10], prefix[11]]) as usize, 12)
        }
        Some(v) => return Err(format!("NumPy file version {v} isn't known")),
        None => return Err("the file is too short".into()),
    };
    if len > MAX_HEADER {
        return Err("the header is absurdly long".into());
    }
    Ok((len, start))
}

/// Builds the volume for an array of `header`'s shape whose bytes are in
/// `source`.
pub fn volume(
    header: &Header,
    name: &str,
    source: Arc<dyn crate::source::ByteSource>,
    format: &str,
) -> Result<Volume, String> {
    let n = header.shape.len();
    // 1. Strides, in C or Fortran order, without overflow.
    let mut strides = vec![0u64; n];
    let mut acc = 1u64;
    let order: Vec<usize> = if header.fortran { (0..n).collect() } else { (0..n).rev().collect() };
    for &k in &order {
        strides[k] = acc;
        acc = acc.checked_mul(header.shape[k].max(1)).filter(|&a| a < 1 << 50).ok_or("the shape is absurdly large")?;
    }
    let len = |k: usize| usize::try_from(header.shape[k]).map_err(|_| "the shape is absurdly large".to_string());
    // 2. What each axis is.
    let (roles, guess, no_picture) = crate::layout::array_roles(&header.shape);
    let mut axes = Vec::new();
    for k in 0..n {
        let stride = usize::try_from(strides[k]).map_err(|_| "the shape is absurdly large".to_string())?;
        axes.push(Axis::new(len(k)?, stride, roles[k]).named(format!("axis {k}")));
    }
    let mut v = Volume::new(name, format, axes, header.dtype, header.endian, source);
    v.slice_axis_guess = guess;
    v.no_picture = no_picture;
    let dims = header.shape.iter().map(|s| s.to_string()).collect::<Vec<_>>().join(", ");
    v.fact("Shape", if n == 1 { format!("({dims},)") } else { format!("({dims})") });
    v.fact("Type", format!("{} ({})", header.dtype.name(), header.descr));
    v.fact("Order", if header.fortran { "Fortran (column-major)" } else { "C (row-major)" });
    v.fact("NumPy format", format!("{}.{}", header.version.0, header.version.1));
    if matches!(header.dtype, DType::C64 | DType::C128) {
        v.fact("Complex values", "shown as magnitude");
    }
    if n == 0 {
        // A scalar still has one value; give it an axis so statistics work.
        v.axes.push(Axis::new(1, 1, Role::X));
    }
    if guess {
        v.fact("Slice axis", "a guess; the viewer can change it");
    }
    v.fit()?;
    Ok(v)
}

/*┌─────────────────╮
| .npy and .npz   |
└─────────────────╯*/
//region Files

fn io_err(e: impl std::fmt::Display) -> LoadError {
    LoadError::Invalid(e.to_string())
}

/// True when the bytes start a NumPy file.
pub fn sniff_npy(head: &[u8]) -> bool {
    head.starts_with(MAGIC)
}

/// Reads the header of the `.npy` data at `offset` of `file` (a whole `.npy`
/// file, or a stored zip member), `avail` bytes long.
fn header_at(file: &File, offset: u64, avail: u64) -> Result<Header, String> {
    let mut first = [0u8; 12];
    let n = (first.len() as u64).min(avail) as usize;
    read_exact_at(file, &mut first[..n], offset).map_err(|e| e.to_string())?;
    let (len, start) = header_length(&first[..n])?;
    let total = (start + len) as u64;
    if total > avail {
        return Err("the file ends inside the header".into());
    }
    let mut buf = vec![0u8; total as usize];
    read_exact_at(file, &mut buf, offset).map_err(|e| e.to_string())?;
    parse_header(&buf)
}

/// Opens an `.npy` file.
pub fn open_npy(path: &std::path::Path) -> Result<Volume, LoadError> {
    let file = File::open(path)?;
    let len = file.metadata()?.len();
    let h = header_at(&file, 0, len).map_err(io_err)?;
    let source = Arc::new(FileSource::new(file, h.data_offset, len - h.data_offset));
    let name = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    volume(&h, &name, source, "NumPy .npy").map_err(io_err)
}

/// The members of an `.npz` archive that are NumPy arrays.
pub fn npz_members(path: &std::path::Path) -> Result<Vec<zip::Entry>, LoadError> {
    let file = File::open(path)?;
    let len = file.metadata()?.len();
    let mut entries = zip::list(&file, len).map_err(io_err)?;
    entries.retain(|e| e.name.ends_with(".npy"));
    Ok(entries)
}

/// The name an `.npz` member goes by: without `.npy`.
pub fn member_name(e: &zip::Entry) -> String {
    e.name.strip_suffix(".npy").unwrap_or(&e.name).to_string()
}

/// Opens one member of an `.npz` archive.  `reach` limits how far into a
/// deflated member reads may go.
pub fn open_member(path: &std::path::Path, entry: &zip::Entry, reach: u64) -> Result<Volume, LoadError> {
    let file = Arc::new(File::open(path)?);
    let len = file.metadata()?.len();
    let start = entry.data_start(&file, len).map_err(io_err)?;
    let name = member_name(entry);
    let (header, source): (Header, Arc<dyn crate::source::ByteSource>) = match entry.method {
        0 => {
            let h = header_at(&file, start, entry.size.min(entry.compressed)).map_err(io_err)?;
            let size = entry.size - h.data_offset.min(entry.size);
            let f = File::open(path)?;
            (h.clone(), Arc::new(FileSource::new(f, start + h.data_offset, size)))
        }
        8 => {
            // Inflate the first bytes for the header, then stream the rest.
            let mut head = Vec::new();
            Inflater::new(
                FileRange::new(file.clone(), start, start + entry.compressed),
                Wrapper::Raw,
                MAX_HEADER as u64 + 12,
            )
            .take(12)
            .read_to_end(&mut head)
            .map_err(io_err)?;
            let (hl, hs) = header_length(&head).map_err(io_err)?;
            let mut full = Vec::new();
            Inflater::new(
                FileRange::new(file.clone(), start, start + entry.compressed),
                Wrapper::Raw,
                MAX_HEADER as u64 + 12,
            )
            .take((hl + hs) as u64)
            .read_to_end(&mut full)
            .map_err(io_err)?;
            let h = parse_header(&full).map_err(io_err)?;
            let data_offset = h.data_offset;
            let size = entry.size.saturating_sub(data_offset);
            let (f2, end) = (file.clone(), start + entry.compressed);
            let src = StreamSource::new(size, reach, move || {
                let mut r =
                    Inflater::new(FileRange::new(f2.clone(), start, end), Wrapper::Raw, entry_limit(data_offset, size));
                std::io::copy(&mut (&mut r).take(data_offset), &mut std::io::sink())?;
                Ok(Box::new(r))
            });
            (h, Arc::new(src))
        }
        m => return Err(LoadError::Unsupported(format!("zip compression method {m}"))),
    };
    volume(&header, &name, source, "NumPy .npz").map_err(io_err)
}

fn entry_limit(offset: u64, size: u64) -> u64 {
    offset.saturating_add(size)
}

//endregion

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Writes an `.npy` byte stream by hand.
    pub fn npy(descr: &str, fortran: bool, shape: &[usize], data: &[u8], version: u8) -> Vec<u8> {
        let shape_s = match shape.len() {
            0 => "()".to_string(),
            1 => format!("({},)", shape[0]),
            _ => format!("({})", shape.iter().map(|s| s.to_string()).collect::<Vec<_>>().join(", ")),
        };
        let mut dict = format!(
            "{{'descr': '{descr}', 'fortran_order': {}, 'shape': {shape_s}, }}",
            if fortran { "True" } else { "False" }
        );
        let pre = if version == 1 { 10 } else { 12 };
        while (pre + dict.len() + 1) % 64 != 0 {
            dict.push(' ');
        }
        dict.push('\n');
        let mut out = MAGIC.to_vec();
        out.extend([version, 0]);
        if version == 1 {
            out.extend((dict.len() as u16).to_le_bytes());
        } else {
            out.extend((dict.len() as u32).to_le_bytes());
        }
        out.extend(dict.as_bytes());
        out.extend(data);
        out
    }

    fn f32s(v: &[f32]) -> Vec<u8> {
        v.iter().flat_map(|x| x.to_le_bytes()).collect()
    }

    #[test]
    fn reads_headers_of_all_versions() {
        for version in [1u8, 2, 3] {
            let b = npy("<f4", false, &[2, 3], &f32s(&[0.0; 6]), version);
            let h = parse_header(&b).unwrap();
            assert_eq!(h.shape, vec![2, 3]);
            assert_eq!(h.dtype, DType::F32);
            assert_eq!(h.version.0, version);
            assert_eq!(h.data_offset % 64, 0);
        }
    }

    #[test]
    fn refuses_object_and_structured_arrays_clearly() {
        let b = npy("|O", false, &[3], &[0; 24], 1);
        assert!(parse_header(&b).unwrap_err().contains("never loads"));
        let mut dict = b"{'descr': [('a', '<i4'), ('b', '<f8')], 'fortran_order': False, 'shape': (4,), }".to_vec();
        while !(10 + dict.len() + 1).is_multiple_of(64) {
            dict.push(b' ');
        }
        dict.push(b'\n');
        let mut b = MAGIC.to_vec();
        b.extend([1, 0]);
        b.extend((dict.len() as u16).to_le_bytes());
        b.extend(dict);
        assert!(parse_header(&b).unwrap_err().contains("structured"));
        for d in ["<U5", "|S3", "<M8[ns]", "<f16", "<c32", "|V8"] {
            let b = npy(d, false, &[2], &[0; 16], 1);
            assert!(parse_header(&b).is_err(), "{d}");
        }
    }

    #[test]
    fn rejects_python_code_in_headers() {
        let hostile = b"{'descr': __import__('os').system('x'), 'fortran_order': False, 'shape': (1,), }";
        let mut b = MAGIC.to_vec();
        b.extend([1, 0]);
        b.extend((hostile.len() as u16).to_le_bytes());
        b.extend(hostile);
        assert!(parse_header(&b).is_err());
    }

    #[test]
    fn shapes_pick_roles() {
        let roles = |shape: &[usize]| -> Vec<Role> {
            let n: usize = shape.iter().product();
            let b = npy("|u1", false, shape, &vec![0; n], 1);
            let h = parse_header(&b).unwrap();
            let s: Arc<dyn crate::source::ByteSource> = Arc::new(crate::source::MemSource(vec![0; n]));
            volume(&h, "t", s, "t").unwrap().axes.iter().map(|a| a.role).collect()
        };
        assert_eq!(roles(&[5, 6]), vec![Role::Y, Role::X]);
        assert_eq!(roles(&[5, 6, 3]), vec![Role::Y, Role::X, Role::Color]);
        assert_eq!(roles(&[5, 6, 4]), vec![Role::Y, Role::X, Role::Color]);
        assert_eq!(roles(&[5, 6, 7]), vec![Role::Z, Role::Y, Role::X]);
        assert_eq!(roles(&[2, 5, 6, 3]), vec![Role::Z, Role::Y, Role::X, Role::Color]);
        assert_eq!(roles(&[2, 5, 6, 7]), vec![Role::T, Role::Z, Role::Y, Role::X]);
        let b = npy("|u1", false, &[5], &[0; 5], 1);
        let v = volume(&parse_header(&b).unwrap(), "t", Arc::new(crate::source::MemSource(vec![0; 5])), "t").unwrap();
        assert!(v.no_picture.is_some());
    }

    #[test]
    fn fortran_order_and_big_endian_read_correctly() {
        use crate::volume::{ChannelSel, PlaneKind, SliceView};
        // A 2 x 3 array [[1,2,3],[4,5,6]] stored column by column, big-endian i2.
        let col_major: Vec<i16> = vec![1, 4, 2, 5, 3, 6];
        let data: Vec<u8> = col_major.iter().flat_map(|v| v.to_be_bytes()).collect();
        let b = npy(">i2", true, &[2, 3], &data, 1);
        let h = parse_header(&b).unwrap();
        let v = volume(&h, "t", Arc::new(crate::source::MemSource(b[h.data_offset as usize..].to_vec())), "t").unwrap();
        let m = v.default_map().unwrap();
        let s = v
            .extract(
                &m,
                &SliceView { kind: PlaneKind::Axial, slice: 0, t: 0, neurological: false },
                ChannelSel::All,
                None,
            )
            .unwrap();
        assert_eq!((s.width, s.height), (3, 2));
        assert_eq!(s.data, vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0]);
    }

    #[test]
    fn absurd_shapes_are_errors() {
        let b = npy("<f8", false, &[1 << 40, 1 << 40], &[0; 16], 1);
        let h = parse_header(&b).unwrap();
        assert!(volume(&h, "t", Arc::new(crate::source::MemSource(vec![0; 16])), "t").is_err());
        let b = npy("<f8", false, &[1 << 30, 1 << 30], &[0; 16], 1);
        let h = parse_header(&b).unwrap();
        assert!(volume(&h, "t", Arc::new(crate::source::MemSource(vec![0; 16])), "t").is_err());
    }

    #[test]
    fn truncated_files_clip_or_fail() {
        let data = f32s(&(0..24).map(|i| i as f32).collect::<Vec<_>>());
        let b = npy("<f4", false, &[4, 3, 2], &data[..data.len() - 4 * 7], 1);
        let h = parse_header(&b).unwrap();
        let v = volume(&h, "t", Arc::new(crate::source::MemSource(b[h.data_offset as usize..].to_vec())), "t").unwrap();
        assert_eq!(v.axes[0].len, 2);
        assert!(!v.warnings.is_empty());
        for cut in [0, 5, 9, 20, 60] {
            let _ = parse_header(&b[..cut.min(b.len())]);
        }
    }
}
