//! MetaImage (`.mha`, and `.mhd` with a `.raw` or `.zraw` beside it): a text
//! header of `Key = Value` lines followed by the data, which may be
//! zlib-compressed.  ITK, SimpleITK, 3D Slicer and VTK write them.

use crate::dtype::{DType, Endian};
use crate::layout::{LPS, dirs_from_vectors, fastest_first};
use crate::source::{ByteSource, FileRange, FileSource, Inflater, StreamSource, Wrapper, read_exact_at};
use crate::volume::{Dir, Volume};
use slerp_scene::LoadError;
use std::fs::File;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

/// Headers longer than this are refused.
const MAX_HEADER: usize = 1 << 20;

/// The header's fields in file order, and where the data begins when it
/// follows the header.
#[derive(Clone, Debug)]
pub struct Header {
    pub fields: Vec<(String, String)>,
    pub data_start: u64,
}

impl Header {
    pub fn get(&self, key: &str) -> Option<&str> {
        self.fields.iter().find(|(k, _)| k.eq_ignore_ascii_case(key)).map(|(_, v)| v.as_str())
    }

    fn numbers(&self, key: &str) -> Vec<f64> {
        self.get(key).map(|v| v.split_whitespace().filter_map(|t| t.parse().ok()).collect()).unwrap_or_default()
    }

    fn flag(&self, keys: &[&str]) -> bool {
        keys.iter().filter_map(|k| self.get(k)).any(|v| v.eq_ignore_ascii_case("true") || v == "1")
    }
}

/// Recognises a MetaImage header from its first bytes.
pub fn sniff(path: &Path, head: &[u8]) -> bool {
    let ext = path.extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
    let text = String::from_utf8_lossy(&head[..head.len().min(4096)]);
    let starts = text.starts_with("ObjectType") || ext == "mha" || ext == "mhd";
    // Both keys must open a line, as in "NDims = 3", not merely appear.
    let has_key = |key: &str| text.lines().any(|l| l.split_once('=').is_some_and(|(k, _)| k.trim() == key));
    starts
        && has_key("NDims")
        && has_key("DimSize")
        && head.iter().take(256).all(|&b| b == b'\n' || b == b'\r' || b == b'\t' || (32..127).contains(&b))
}

/// Parses the header at the start of `bytes`.
pub fn parse_header(bytes: &[u8]) -> Result<Header, String> {
    let mut fields = Vec::new();
    let mut at = 0usize;
    while at < bytes.len() {
        let end = bytes[at..].iter().position(|&b| b == b'\n').map_or(bytes.len(), |p| at + p);
        let line = String::from_utf8_lossy(&bytes[at..end]);
        let next = (end + 1).min(bytes.len());
        if let Some((k, v)) = line.split_once('=') {
            let (k, v) = (k.trim().to_string(), v.trim().to_string());
            let last = k.eq_ignore_ascii_case("ElementDataFile");
            if fields.len() >= 256 {
                return Err("the header has too many fields".into());
            }
            fields.push((k, v));
            if last {
                return Ok(Header { fields, data_start: next as u64 });
            }
        } else if !line.trim().is_empty() {
            return Err("a header line isn't Key = Value".into());
        }
        at = next;
    }
    Err("the header has no ElementDataFile line".into())
}

fn element_type(s: &str) -> Result<DType, String> {
    let base = s.trim().trim_end_matches("_ARRAY");
    Ok(match base {
        "MET_CHAR" => DType::I8,
        "MET_UCHAR" => DType::U8,
        "MET_SHORT" => DType::I16,
        "MET_USHORT" => DType::U16,
        "MET_INT" | "MET_LONG" => DType::I32,
        "MET_UINT" | "MET_ULONG" => DType::U32,
        "MET_LONG_LONG" => DType::I64,
        "MET_ULONG_LONG" => DType::U64,
        "MET_FLOAT" => DType::F32,
        "MET_DOUBLE" => DType::F64,
        other => return Err(format!("the element type {other} isn't supported")),
    })
}

/// A body direction from a letter of `AnatomicalOrientation`: the side the
/// axis starts from, so the axis points the other way.
fn orientation_letters(s: &str) -> Option<[Dir; 3]> {
    let l: Vec<char> = s.trim().chars().collect();
    if l.len() != 3 {
        return None;
    }
    let mut dirs = [Dir::R; 3];
    for (d, c) in dirs.iter_mut().zip(&l) {
        *d = match c.to_ascii_uppercase() {
            'R' => Dir::L,
            'L' => Dir::R,
            'A' => Dir::P,
            'P' => Dir::A,
            'S' => Dir::I,
            'I' => Dir::S,
            _ => return None,
        };
    }
    let pair = |d: Dir| d.role();
    (pair(dirs[0]) != pair(dirs[1]) && pair(dirs[1]) != pair(dirs[2]) && pair(dirs[0]) != pair(dirs[2])).then_some(dirs)
}

/// Builds the volume described by `h` whose data is `source`.
pub fn volume(h: &Header, name: &str, source: Arc<dyn ByteSource>) -> Result<Volume, String> {
    if let Some(o) = h.get("ObjectType")
        && !o.eq_ignore_ascii_case("Image")
    {
        return Err(format!("this MetaIO file holds a {o}, not an image"));
    }
    let dims: Vec<u64> = h.numbers("DimSize").iter().map(|&d| if d >= 1.0 { d as u64 } else { 0 }).collect();
    let nd = h.numbers("NDims").first().copied().unwrap_or(dims.len() as f64) as usize;
    if dims.is_empty() || dims.len() != nd || nd > 7 {
        return Err("DimSize and NDims disagree or are missing".into());
    }
    let dtype = element_type(h.get("ElementType").ok_or("the header has no ElementType")?)?;
    let channels = h.numbers("ElementNumberOfChannels").first().map_or(1, |&c| c as usize).clamp(1, 16);
    let spacing = {
        let s = h.numbers("ElementSpacing");
        if s.is_empty() { h.numbers("ElementSize") } else { s }
    };
    // Orientation: the direction matrix (rows are the axes' directions,
    // in ITK's left-posterior-superior world), else the orientation letters.
    let matrix = {
        let m = h.numbers("TransformMatrix");
        if m.len() >= 9 { m } else { h.numbers("Rotation") }
    };
    let oriented = if nd >= 3 && matrix.len() == nd * nd {
        let rows: [[f64; 3]; 3] = std::array::from_fn(|i| std::array::from_fn(|j| matrix[i * nd + j]));
        dirs_from_vectors(&rows, LPS).map(|(d, _)| d)
    } else if nd >= 3 {
        h.get("AnatomicalOrientation").or_else(|| h.get("Orientation")).and_then(orientation_letters)
    } else {
        None
    };
    let endian = if h.flag(&["BinaryDataByteOrderMSB", "ElementByteOrderMSB"]) { Endian::Big } else { Endian::Little };
    let axes = fastest_first(&dims, channels, oriented, &spacing)?;
    let mut v = Volume::new(name, "MetaImage", axes, dtype, endian, source);
    v.unit = "mm".into();
    for (k, val) in &h.fields {
        if !k.eq_ignore_ascii_case("ElementDataFile") && !val.is_empty() {
            v.facts.push((k.clone(), val.clone()));
        }
    }
    v.fit()?;
    Ok(v)
}

fn bad(e: impl std::fmt::Display) -> LoadError {
    LoadError::Invalid(e.to_string())
}

/// Opens a `.mha` or `.mhd`, returning the file read beside it, if any.
pub fn open(path: &Path, reach: u64) -> Result<(Volume, Vec<PathBuf>), LoadError> {
    let file = File::open(path)?;
    let len = file.metadata()?.len();
    let mut buf = vec![0u8; len.min(MAX_HEADER as u64) as usize];
    read_exact_at(&file, &mut buf, 0)?;
    let h = parse_header(&buf).map_err(bad)?;
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    // The size the header promises: more than that is never read.
    let dims: Vec<u64> = h.numbers("DimSize").iter().map(|&d| d.max(0.0) as u64).collect();
    let channels = h.numbers("ElementNumberOfChannels").first().map_or(1, |&c| c as u64).clamp(1, 16);
    let esize = element_type(h.get("ElementType").unwrap_or("")).map(|d| d.size() as u64).map_err(bad)?;
    let mut expect = channels * esize;
    for &d in &dims {
        expect = expect.checked_mul(d).ok_or_else(|| bad("the volume is absurdly large"))?;
    }
    if h.get("BinaryData").is_some_and(|b| b.eq_ignore_ascii_case("false")) {
        return Err(LoadError::Unsupported("MetaImage data written as text".into()));
    }
    let data_file = h.get("ElementDataFile").unwrap_or("LOCAL").to_string();
    let mut neighbours = Vec::new();
    let (data_path, start) = if data_file.eq_ignore_ascii_case("LOCAL") {
        (path.to_path_buf(), h.data_start)
    } else {
        if data_file.eq_ignore_ascii_case("LIST") || data_file.contains('%') || data_file.contains(' ') {
            return Err(LoadError::Unsupported("MetaImage data split over several files".into()));
        }
        let rel = Path::new(&data_file);
        if rel.is_absolute() || rel.components().any(|c| matches!(c, Component::ParentDir)) {
            return Err(LoadError::Unsupported("a data file outside the header's folder".into()));
        }
        let p = path.parent().unwrap_or(Path::new(".")).join(rel);
        neighbours.push(p.clone());
        (p, 0)
    };
    let df = Arc::new(crate::source::open_data_file(&data_path)?);
    let dlen = df.metadata()?.len();
    let source: Arc<dyn ByteSource> = if h.flag(&["CompressedData"]) {
        let csize = h.numbers("CompressedDataSize").first().map(|&c| c as u64);
        let end = csize.map_or(dlen, |c| start.saturating_add(c).min(dlen));
        let f = df.clone();
        Arc::new(StreamSource::new(expect, reach, move || {
            Ok(Box::new(Inflater::new(FileRange::new(f.clone(), start, end), Wrapper::Zlib, expect)))
        }))
    } else {
        Arc::new(FileSource::new(crate::source::open_data_file(&data_path)?, start, dlen.saturating_sub(start)))
    };
    let v = volume(&h, &name, source).map_err(bad)?;
    Ok((v, neighbours))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::source::MemSource;
    use crate::volume::{ChannelSel, PlaneKind, Role, SliceView};

    pub fn header(fields: &[(&str, &str)]) -> Vec<u8> {
        let mut s = String::new();
        for (k, v) in fields {
            s.push_str(&format!("{k} = {v}\r\n"));
        }
        s.push_str("ElementDataFile = LOCAL\n");
        s.into_bytes()
    }

    fn build(fields: &[(&str, &str)], data: &[u8]) -> Result<Volume, String> {
        let hb = header(fields);
        let h = parse_header(&hb)?;
        assert_eq!(h.data_start as usize, hb.len());
        volume(&h, "t", Arc::new(MemSource(data.to_vec())))
    }

    #[test]
    fn reads_a_short_volume() {
        let data: Vec<u8> = (0..24i16).flat_map(|v| v.to_le_bytes()).collect();
        let v = build(
            &[
                ("ObjectType", "Image"),
                ("NDims", "3"),
                ("BinaryData", "True"),
                ("BinaryDataByteOrderMSB", "False"),
                ("DimSize", "4 3 2"),
                ("ElementSpacing", "1 2 3"),
                ("ElementType", "MET_SHORT"),
            ],
            &data,
        )
        .unwrap();
        assert_eq!(v.shape(), vec![4, 3, 2]);
        assert_eq!(v.value_at(&[1, 2, 1]).unwrap(), (1 + 8 + 12) as f64);
        assert_eq!(v.axes[2].spacing, 3.0);
        // No orientation given: array order.
        assert_eq!(v.axes[0].role, Role::X);
    }

    #[test]
    fn big_endian_and_channels() {
        let data: Vec<u8> = (0..12u16).flat_map(|v| v.to_be_bytes()).collect();
        let v = build(
            &[
                ("NDims", "2"),
                ("BinaryDataByteOrderMSB", "True"),
                ("DimSize", "2 2"),
                ("ElementNumberOfChannels", "3"),
                ("ElementType", "MET_USHORT_ARRAY"),
            ],
            &data,
        )
        .unwrap();
        let m = v.default_map().unwrap();
        let s = v
            .extract(
                &m,
                &SliceView { kind: PlaneKind::Axial, slice: 0, t: 0, neurological: false },
                ChannelSel::All,
                None,
            )
            .unwrap();
        assert_eq!(s.samples, 3);
        assert_eq!(&s.data[3..6], &[3.0, 4.0, 5.0]);
    }

    #[test]
    fn the_direction_matrix_orients_in_lps() {
        let data = vec![0u8; 24];
        // Axis 0 points right, 1 front, 2 head: RAS in LPS terms means x = -1, y = -1.
        let v = build(
            &[
                ("NDims", "3"),
                ("DimSize", "4 3 2"),
                ("ElementType", "MET_UCHAR"),
                ("TransformMatrix", "-1 0 0 0 -1 0 0 0 1"),
            ],
            &data,
        )
        .unwrap();
        let d: Vec<_> = v.axes.iter().map(|a| a.dir).collect();
        assert_eq!(d, vec![Some(Dir::R), Some(Dir::A), Some(Dir::S)]);
        // Letters alone: RAI means the axes start at R, A and I.
        let v = build(
            &[("NDims", "3"), ("DimSize", "4 3 2"), ("ElementType", "MET_UCHAR"), ("AnatomicalOrientation", "RAI")],
            &data,
        )
        .unwrap();
        let d: Vec<_> = v.axes.iter().map(|a| a.dir).collect();
        assert_eq!(d, vec![Some(Dir::L), Some(Dir::P), Some(Dir::S)]);
    }

    #[test]
    fn wrong_headers_are_errors() {
        let d = vec![0u8; 100];
        assert!(
            build(&[("ObjectType", "Tube"), ("NDims", "1"), ("DimSize", "4"), ("ElementType", "MET_UCHAR")], &d)
                .is_err()
        );
        assert!(build(&[("NDims", "3"), ("DimSize", "4 3"), ("ElementType", "MET_UCHAR")], &d).is_err());
        assert!(build(&[("NDims", "2"), ("DimSize", "4 3"), ("ElementType", "MET_FOO")], &d).is_err());
        assert!(build(&[("NDims", "2"), ("DimSize", "4 -3"), ("ElementType", "MET_UCHAR")], &d).is_err());
        assert!(build(&[("NDims", "2"), ("DimSize", "99999999 99999999"), ("ElementType", "MET_DOUBLE")], &d).is_err());
        assert!(parse_header(b"NDims = 2\nDimSize = 2 2\n").is_err());
        assert!(parse_header(b"this is not a header\n").is_err());
    }

    #[test]
    fn sniffing_needs_the_structure() {
        let h = header(&[("ObjectType", "Image"), ("NDims", "2"), ("DimSize", "2 2"), ("ElementType", "MET_UCHAR")]);
        assert!(sniff(Path::new("a.mha"), &h));
        assert!(sniff(Path::new("a.bin"), &h));
        assert!(!sniff(Path::new("a.txt"), b"NDims is a word and so is DimSize"));
        assert!(!sniff(Path::new("a.mha"), &[0u8; 100]));
    }
}
