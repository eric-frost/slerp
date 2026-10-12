//! NRRD (`.nrrd`, and `.nhdr` with a detached data file): a text header of
//! `field: value` lines, a blank line, then the data, raw, gzip- or
//! bzip2-compressed, or written as text or hex.  Teem, ITK and 3D Slicer
//! write them.

use crate::dtype::{DType, Endian};
use crate::layout::{LAS, LPS, RAS, dirs_from_vectors};
use crate::source::{ByteSource, FileRange, FileSource, Inflater, MemSource, StreamSource, Wrapper, read_exact_at};
use crate::volume::{Axis, Dir, Role, Volume};
use slerp_scene::LoadError;
use std::fs::File;
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

const MAX_HEADER: usize = 1 << 20;
/// The most samples text and hex data are parsed into memory for.
const MAX_TEXT_SAMPLES: u64 = 1 << 26;

#[derive(Clone, Debug)]
pub struct Header {
    pub magic: String,
    /// `field: value` lines, lowercase field names with single spaces.
    pub fields: Vec<(String, String)>,
    /// `key:=value` lines.
    pub keys: Vec<(String, String)>,
    /// Where the data begins when it follows the header.
    pub data_start: u64,
}

impl Header {
    pub fn get(&self, field: &str) -> Option<&str> {
        self.fields.iter().find(|(k, _)| k == field).map(|(_, v)| v.as_str())
    }
}

/// True for the first line of a NRRD file.
pub fn sniff(head: &[u8]) -> bool {
    head.len() >= 8
        && head.starts_with(b"NRRD000")
        && (b'1'..=b'5').contains(&head[7])
        && matches!(head.get(8), Some(b'\n' | b'\r'))
}

/// Parses the header at the start of `b`.
pub fn parse_header(b: &[u8]) -> Result<Header, String> {
    if !sniff(b) {
        return Err("this isn't a NRRD file".into());
    }
    let mut fields: Vec<(String, String)> = Vec::new();
    let mut keys = Vec::new();
    let mut at = 0usize;
    let mut first = true;
    let mut magic = String::new();
    while at < b.len() {
        let end = b[at..].iter().position(|&c| c == b'\n').map_or(b.len(), |p| at + p);
        let line = String::from_utf8_lossy(&b[at..end]).trim_end_matches('\r').to_string();
        let next = (end + 1).min(b.len());
        if first {
            magic = line.clone();
            first = false;
        } else if line.is_empty() {
            return Ok(Header { magic, fields, keys, data_start: next as u64 });
        } else if line.starts_with('#') {
            // A comment.
        } else if let Some((k, v)) = line.split_once(":=") {
            if keys.len() < 512 {
                keys.push((k.trim().to_string(), v.trim().to_string()));
            }
        } else if let Some((k, v)) = line.split_once(':') {
            if fields.len() >= 128 {
                return Err("the header has too many fields".into());
            }
            fields
                .push((k.split_whitespace().collect::<Vec<_>>().join(" ").to_ascii_lowercase(), v.trim().to_string()));
        } else {
            return Err("a header line has no colon".into());
        }
        at = next;
        if at > MAX_HEADER {
            return Err("the header is absurdly long".into());
        }
    }
    // A detached header may end without the blank line.
    Ok(Header { magic, fields, keys, data_start: at as u64 })
}

fn element_type(s: &str) -> Result<DType, String> {
    Ok(match s.trim().to_ascii_lowercase().as_str() {
        "signed char" | "int8" | "int8_t" => DType::I8,
        "uchar" | "unsigned char" | "uint8" | "uint8_t" => DType::U8,
        "short" | "short int" | "signed short" | "signed short int" | "int16" | "int16_t" => DType::I16,
        "ushort" | "unsigned short" | "unsigned short int" | "uint16" | "uint16_t" => DType::U16,
        "int" | "signed int" | "int32" | "int32_t" => DType::I32,
        "uint" | "unsigned int" | "uint32" | "uint32_t" => DType::U32,
        "longlong"
        | "long long"
        | "long long int"
        | "signed long long"
        | "signed long long int"
        | "int64"
        | "int64_t" => DType::I64,
        "ulonglong" | "unsigned long long" | "unsigned long long int" | "uint64" | "uint64_t" => DType::U64,
        "float" => DType::F32,
        "double" => DType::F64,
        "block" => return Err("NRRD block data isn't supported".into()),
        other => return Err(format!("the NRRD type '{other}' isn't known")),
    })
}

/// A space vector: `(1,0,0)` or `none`, one for each axis.
fn space_vectors(s: &str) -> Vec<Option<Vec<f64>>> {
    let mut out = Vec::new();
    let mut rest = s.trim();
    while !rest.is_empty() {
        if let Some(r) = rest.strip_prefix("none") {
            out.push(None);
            rest = r.trim_start();
        } else if let Some(r) = rest.strip_prefix('(') {
            let Some(close) = r.find(')') else { break };
            out.push(Some(r[..close].split(',').filter_map(|t| t.trim().parse().ok()).collect()));
            rest = r[close + 1..].trim_start();
        } else {
            break;
        }
    }
    out
}

fn numbers(s: &str) -> Vec<f64> {
    s.split_whitespace().filter_map(|t| t.parse().ok()).collect()
}

fn space_world(s: &str) -> Option<[Dir; 3]> {
    let s = s.trim().to_ascii_lowercase();
    match s.as_str() {
        "right-anterior-superior" | "ras" | "scanner-xyz" => Some(RAS),
        "left-anterior-superior" | "las" => Some(LAS),
        "left-posterior-superior" | "lps" => Some(LPS),
        _ => None,
    }
}

/// The size of one sample's data, counting the samples the header gives.
pub fn sizes(h: &Header) -> Result<Vec<u64>, String> {
    let n: usize = h.get("dimension").and_then(|d| d.trim().parse().ok()).ok_or("the header has no dimension")?;
    let sizes: Vec<u64> = numbers(h.get("sizes").ok_or("the header has no sizes")?)
        .iter()
        .map(|&s| if s >= 1.0 { s as u64 } else { 0 })
        .collect();
    if n == 0 || n > 8 || sizes.len() != n || sizes.contains(&0) {
        return Err("dimension and sizes disagree, or a size is zero".into());
    }
    Ok(sizes)
}

/// Builds the volume described by `h` whose data is `source`.
pub fn volume(
    h: &Header,
    name: &str,
    source: Arc<dyn ByteSource>,
    endian: Endian,
    dtype: DType,
) -> Result<Volume, String> {
    let sizes = sizes(h)?;
    let n = sizes.len();
    let kinds: Vec<String> =
        h.get("kinds").map(|k| k.split_whitespace().map(|t| t.to_ascii_lowercase()).collect()).unwrap_or_default();
    let kind = |i: usize| kinds.get(i).map(String::as_str).unwrap_or("domain");
    let vectors = h.get("space directions").map(space_vectors).unwrap_or_default();
    let spacings = h.get("spacings").map(numbers).unwrap_or_default();
    let mut v_axes = Vec::new();
    let mut stride = 1u64;
    let mut spatial = Vec::new();
    let mut note = None;
    let mut spatial_time = false;
    #[allow(clippy::needless_range_loop)]
    for i in 0..n {
        let st = usize::try_from(stride).map_err(|_| "the volume is absurdly large")?;
        let len = sizes[i] as usize;
        let k = kind(i);
        let role = match k {
            "rgb-color" | "3-color" | "rgba-color" | "4-color" if len == 3 || len == 4 => Role::Color,
            "3-vector" | "covariant-vector" | "vector" | "4-vector" if len == 3 || len == 4 => {
                note = Some(format!("axis {i} holds vector components, shown as colour channels"));
                Role::Color
            }
            "time" => {
                spatial_time = true;
                Role::T
            }
            // A list of volumes (diffusion gradients) is shown one at a time,
            // wherever in the axis order Teem put it.
            "list" if !spatial_time => {
                spatial_time = true;
                Role::T
            }
            "list" | "none" | "scalar" => Role::Other,
            "domain" | "space" | "centered" | "scalar-domain" | "time-domain" => {
                spatial.push(i);
                Role::Other
            }
            _ if k.contains("color") || k.contains("vector") || k.contains("matrix") || k.contains("quaternion") => {
                Role::Other
            }
            _ => {
                spatial.push(i);
                Role::Other
            }
        };
        v_axes.push(Axis::new(len, st, role).named(format!("axis {i}")));
        stride = stride.checked_mul(sizes[i]).filter(|&s| s < 1 << 50).ok_or("the volume is absurdly large")?;
    }
    // The spatial axes in order become x, y and z (the next is time).
    let mut vecs: Vec<[f64; 3]> = Vec::new();
    for (rank, &a) in spatial.iter().enumerate() {
        v_axes[a].role = match rank {
            0 => Role::X,
            1 => Role::Y,
            2 => Role::Z,
            3 => Role::T,
            _ => Role::Other,
        };
        let vec = vectors.get(a).and_then(|v| v.as_ref());
        if let Some(c) = vec {
            if rank < 3 && c.len() >= 3 {
                vecs.push([c[0], c[1], c[2]]);
            }
            v_axes[a].spacing = c.iter().map(|x| x * x).sum::<f64>().sqrt();
        } else if let Some(&s) = spacings.get(a) {
            v_axes[a].spacing = s;
        }
        if !(v_axes[a].spacing.is_finite() && v_axes[a].spacing > 0.0) {
            v_axes[a].spacing = 0.0;
        }
    }
    if vecs.len() == 3
        && spatial.len() >= 3
        && let Some((dirs, _)) = h.get("space").and_then(space_world).and_then(|w| {
            let m: [[f64; 3]; 3] = [vecs[0], vecs[1], vecs[2]];
            dirs_from_vectors(&m, w)
        })
    {
        for (rank, d) in dirs.iter().enumerate() {
            v_axes[spatial[rank]].dir = Some(*d);
            v_axes[spatial[rank]].role = d.role();
        }
    }
    let mut v = Volume::new(name, format!("NRRD ({})", h.magic), v_axes, dtype, endian, source);
    v.unit = h
        .get("space units")
        .map(|u| u.replace('"', ""))
        .map(|u| u.split_whitespace().next().unwrap_or("").to_string())
        .filter(|u| !u.is_empty())
        .unwrap_or_else(|| "mm".into());
    if let Some(note) = note {
        v.warnings.push(note);
    }
    for (k, val) in h.fields.iter().chain(h.keys.iter()) {
        if !val.is_empty() && k != "data file" && k != "datafile" {
            v.facts.push((k.clone(), val.clone()));
        }
    }
    v.fit()?;
    Ok(v)
}

fn bad(e: impl std::fmt::Display) -> LoadError {
    LoadError::Invalid(e.to_string())
}

/// Opens a `.nrrd` or `.nhdr`.
pub fn open(path: &Path, reach: u64) -> Result<(Volume, Vec<PathBuf>), LoadError> {
    let file = File::open(path)?;
    let len = file.metadata()?.len();
    let mut buf = vec![0u8; len.min(MAX_HEADER as u64) as usize];
    read_exact_at(&file, &mut buf, 0)?;
    let h = parse_header(&buf).map_err(bad)?;
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let dtype = element_type(h.get("type").ok_or_else(|| bad("the header has no type"))?).map_err(bad)?;
    let sz = sizes(&h).map_err(bad)?;
    let mut count = 1u64;
    for &s in &sz {
        count = count.checked_mul(s).ok_or_else(|| bad("the volume is absurdly large"))?;
    }
    let expect = count.checked_mul(dtype.size() as u64).ok_or_else(|| bad("the volume is absurdly large"))?;
    let endian = match h.get("endian").map(|e| e.to_ascii_lowercase()).as_deref() {
        Some("big") => Endian::Big,
        _ => Endian::Little,
    };
    let encoding = h.get("encoding").map(|e| e.to_ascii_lowercase()).unwrap_or_else(|| "raw".into());
    let skip: i64 = h.get("byte skip").and_then(|s| s.trim().parse().ok()).unwrap_or(0);
    let line_skip: u64 = h.get("line skip").and_then(|s| s.trim().parse().ok()).unwrap_or(0);
    // Where the data is: this file after the header, or a detached file.
    let mut neighbours = Vec::new();
    let (data_path, start) = match h.get("data file").or_else(|| h.get("datafile")) {
        None => (path.to_path_buf(), h.data_start),
        Some(d) => {
            let d = d.trim();
            if d.starts_with("LIST") || d.contains('%') || d.contains(' ') {
                return Err(LoadError::Unsupported("NRRD data split over several files".into()));
            }
            let rel = Path::new(d);
            if rel.is_absolute() || rel.components().any(|c| matches!(c, Component::ParentDir)) {
                return Err(LoadError::Unsupported("a data file outside the header's folder".into()));
            }
            let p = path.parent().unwrap_or(Path::new(".")).join(rel);
            neighbours.push(p.clone());
            (p, 0)
        }
    };
    let df = Arc::new(crate::source::open_data_file(&data_path)?);
    let dlen = df.metadata()?.len();
    // Lines to skip first: found by scanning, since lines have no fixed length.
    let mut start = start;
    if line_skip > 0 {
        let mut b = vec![0u8; (dlen - start.min(dlen)).min(1 << 20) as usize];
        read_exact_at(&df, &mut b, start)?;
        let mut seen = 0;
        let mut at = 0;
        while seen < line_skip {
            let p = b[at..].iter().position(|&c| c == b'\n').ok_or_else(|| bad("line skip runs past the data"))?;
            at += p + 1;
            seen += 1;
        }
        start += at as u64;
    }
    let source: Arc<dyn ByteSource> = match encoding.as_str() {
        "raw" => {
            // A byte skip of -1 counts back from the end of the file.
            let off = if skip < 0 { dlen.saturating_sub(expect) } else { start + skip as u64 };
            Arc::new(FileSource::new(crate::source::open_data_file(&data_path)?, off, dlen.saturating_sub(off)))
        }
        "gz" | "gzip" => {
            let f = df.clone();
            let skip_after = skip.max(0) as u64;
            let total = expect + skip_after;
            Arc::new(StreamSource::new(expect, reach, move || {
                let mut r = Inflater::new(FileRange::new(f.clone(), start, dlen), Wrapper::Gzip, total);
                std::io::copy(&mut (&mut r).take(skip_after), &mut std::io::sink())?;
                Ok(Box::new(r))
            }))
        }
        "bz2" | "bzip2" => {
            let f = df.clone();
            let skip_after = skip.max(0) as u64;
            Arc::new(StreamSource::new(expect, reach, move || {
                let r = bzip2_rs::DecoderReader::new(FileRange::new(f.clone(), start, dlen));
                let mut r = r.take(expect + skip_after);
                std::io::copy(&mut (&mut r).take(skip_after), &mut std::io::sink())?;
                Ok(Box::new(r))
            }))
        }
        "ascii" | "text" | "txt" | "hex" => {
            if count > MAX_TEXT_SAMPLES || dlen > 1 << 30 {
                return Err(LoadError::TooLarge("text-encoded data this large".into()));
            }
            let mut text = vec![0u8; (dlen - start.min(dlen)) as usize];
            read_exact_at(&df, &mut text, start)?;
            let bytes = if encoding == "hex" { decode_hex(&text, expect) } else { decode_text(&text, dtype, count) };
            return finish(&h, &name, Arc::new(MemSource(bytes.0)), bytes.1, bytes.2, neighbours);
        }
        other => return Err(LoadError::Unsupported(format!("NRRD encoding '{other}'"))),
    };
    finish(&h, &name, source, endian, dtype, neighbours)
}

fn finish(
    h: &Header,
    name: &str,
    source: Arc<dyn ByteSource>,
    endian: Endian,
    dtype: DType,
    neighbours: Vec<PathBuf>,
) -> Result<(Volume, Vec<PathBuf>), LoadError> {
    Ok((volume(h, name, source, endian, dtype).map_err(bad)?, neighbours))
}

/// Hex data: two digits per byte, whitespace ignored, in the file's own byte order.
fn decode_hex(text: &[u8], expect: u64) -> (Vec<u8>, Endian, DType) {
    let mut out = Vec::new();
    let mut hi = None;
    for &c in text {
        let d = match c {
            b'0'..=b'9' => c - b'0',
            b'a'..=b'f' => c - b'a' + 10,
            b'A'..=b'F' => c - b'A' + 10,
            _ => continue,
        };
        match hi.take() {
            None => hi = Some(d),
            Some(h) => out.push(h << 4 | d),
        }
        if out.len() as u64 >= expect {
            break;
        }
    }
    (out, Endian::Little, DType::U8)
}

/// Text data: numbers separated by whitespace, stored as doubles.
fn decode_text(text: &[u8], _dtype: DType, count: u64) -> (Vec<u8>, Endian, DType) {
    let s = String::from_utf8_lossy(text);
    let mut out = Vec::with_capacity((count.min(1 << 24) * 8) as usize);
    for t in s.split_whitespace().take(count as usize) {
        out.extend_from_slice(&t.parse::<f64>().unwrap_or(f64::NAN).to_le_bytes());
    }
    (out, Endian::Little, DType::F64)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::volume::{ChannelSel, PlaneKind, SliceView};

    fn header(lines: &[&str]) -> Vec<u8> {
        let mut s = String::from("NRRD0004\n# a comment\n");
        for l in lines {
            s.push_str(l);
            s.push('\n');
        }
        s.push('\n');
        s.into_bytes()
    }

    fn vol(lines: &[&str], data: &[u8]) -> Result<Volume, String> {
        let hb = header(lines);
        let h = parse_header(&hb)?;
        assert_eq!(h.data_start as usize, hb.len());
        let dtype = element_type(h.get("type").ok_or("no type")?)?;
        let endian = if h.get("endian") == Some("big") { Endian::Big } else { Endian::Little };
        volume(&h, "t", Arc::new(MemSource(data.to_vec())), endian, dtype)
    }

    #[test]
    fn reads_a_small_array() {
        let data: Vec<u8> = (0..24i16).flat_map(|v| v.to_be_bytes()).collect();
        let v = vol(
            &[
                "type: short",
                "dimension: 3",
                "sizes: 4 3 2",
                "endian: big",
                "encoding: raw",
                "spacings: 1 2 3",
                "key:=value",
            ],
            &data,
        )
        .unwrap();
        assert_eq!(v.shape(), vec![4, 3, 2]);
        assert_eq!(v.value_at(&[3, 2, 1]).unwrap(), 23.0);
        assert_eq!(v.axes[1].spacing, 2.0);
        assert!(v.facts.iter().any(|(k, val)| k == "key" && val == "value"));
    }

    #[test]
    fn space_directions_orient_the_axes() {
        let data = vec![0u8; 24];
        let v = vol(
            &[
                "type: uchar",
                "dimension: 3",
                "sizes: 4 3 2",
                "space: left-posterior-superior",
                "space directions: (0,-2,0) (1,0,0) (0,0,3)",
                "kinds: domain domain domain",
            ],
            &data,
        )
        .unwrap();
        let d: Vec<_> = v.axes.iter().map(|a| a.dir).collect();
        assert_eq!(d, vec![Some(Dir::A), Some(Dir::L), Some(Dir::S)]);
        assert_eq!(v.axes[0].spacing, 2.0);
        assert_eq!(v.axes[2].spacing, 3.0);
        let v = vol(
            &[
                "type: uchar",
                "dimension: 3",
                "sizes: 4 3 2",
                "space: right-anterior-superior",
                "space directions: (1,0,0) (0,1,0) (0,0,1)",
            ],
            &data,
        )
        .unwrap();
        assert_eq!(v.axes[0].dir, Some(Dir::R));
    }

    #[test]
    fn kinds_find_time_colour_and_vectors() {
        let data = vec![0u8; 3 * 4 * 3 * 2];
        let v = vol(&["type: uchar", "dimension: 4", "sizes: 3 4 3 2", "kinds: RGB-color domain domain domain"], &data)
            .unwrap();
        assert_eq!(v.axes[0].role, Role::Color);
        assert_eq!(v.axes[1].role, Role::X);
        let m = v.default_map().unwrap();
        let s = v
            .extract(
                &m,
                &SliceView { kind: PlaneKind::Axial, slice: 1, t: 0, neurological: false },
                ChannelSel::All,
                None,
            )
            .unwrap();
        assert_eq!(s.samples, 3);
        // A diffusion set: three space axes and a list.
        let data = vec![0u8; 4 * 3 * 2 * 5];
        let v =
            vol(&["type: uchar", "dimension: 4", "sizes: 4 3 2 5", "kinds: space space space list"], &data).unwrap();
        assert_eq!(v.axes[3].role, Role::T);
    }

    #[test]
    fn broken_headers_are_errors() {
        let d = vec![0u8; 100];
        assert!(vol(&["type: uchar", "dimension: 3", "sizes: 4 3"], &d).is_err());
        assert!(vol(&["type: uchar", "dimension: 2", "sizes: 4 0"], &d).is_err());
        assert!(vol(&["type: uchar", "dimension: 2", "sizes: 99999999 99999999"], &d).is_err());
        assert!(vol(&["type: blob", "dimension: 1", "sizes: 4"], &d).is_err());
        assert!(vol(&["type: block", "dimension: 1", "sizes: 4"], &d).is_err());
        assert!(parse_header(b"NRRD0004\nno colon here\n\n").is_err());
        assert!(parse_header(b"PNG").is_err());
    }

    #[test]
    fn hex_and_text_decode() {
        let (b, _, t) = decode_hex(b"01 02\nAb ff", 100);
        assert_eq!((b, t), (vec![1, 2, 0xab, 0xff], DType::U8));
        let (b, _, t) = decode_text(b"1 2.5\n-3", DType::U8, 3);
        assert_eq!(t, DType::F64);
        assert_eq!(f64::from_le_bytes(b[8..16].try_into().unwrap()), 2.5);
    }

    #[test]
    fn sniffing_wants_the_magic_line() {
        assert!(sniff(b"NRRD0005\ntype: uchar\n"));
        assert!(sniff(b"NRRD0001\r\n"));
        assert!(!sniff(b"NRRD0009\n"));
        assert!(!sniff(b"NRRD000"));
    }
}
