//! Putting a folder's images in series order: the files sharing the open
//! file's Series Instance UID, sorted by Instance Number, or by position
//! along the slice normal when numbers are missing or repeat.  Only
//! headers are read, so a folder of 500 CT slices sorts in well under a
//! second.  Files are recognised by content, so CD exports without
//! extensions (`IM000001`) are found too.

use crate::data::tags;
use crate::parse::{Extent, looks_like_dicom, parse_file};
use std::io::Read;
use std::path::{Path, PathBuf};

/// At most this many files of a folder are looked at.
const MAX_FILES: usize = 20_000;

/// What sorting needs from one file.
#[derive(Clone, Debug, PartialEq)]
pub struct Slice {
    pub path: PathBuf,
    pub series: String,
    /// Series Number, Series Description and Modality, for listing the
    /// folder's series.
    pub number: Option<i64>,
    pub description: String,
    pub modality: String,
    pub instance: Option<i64>,
    /// Image Position (Patient): the first pixel's position, in mm.
    pub position: Option<[f64; 3]>,
    /// The slice normal: the cross product of the row and column
    /// directions from Image Orientation (Patient).
    pub normal: Option<[f64; 3]>,
}

impl Slice {
    /// Reads a file's header (stopping before the pixel data).
    pub fn read(path: &Path) -> Option<Slice> {
        let parsed = parse_file(path, Extent::Header).ok()?;
        let ds = &parsed.data;
        let series = ds.string(tags::SERIES_INSTANCE_UID).unwrap_or_default();
        let position = <[f64; 3]>::try_from(ds.numbers(tags::IMAGE_POSITION)).ok();
        let normal = <[f64; 6]>::try_from(ds.numbers(tags::IMAGE_ORIENTATION)).ok().map(|o| {
            let (r, c) = ([o[0], o[1], o[2]], [o[3], o[4], o[5]]);
            [r[1] * c[2] - r[2] * c[1], r[2] * c[0] - r[0] * c[2], r[0] * c[1] - r[1] * c[0]]
        });
        Some(Slice {
            path: path.to_path_buf(),
            series,
            number: ds.int(tags::SERIES_NUMBER),
            description: ds.string(tags::SERIES_DESCRIPTION).unwrap_or_default(),
            modality: ds.string(tags::MODALITY).unwrap_or_default(),
            instance: ds.int(tags::INSTANCE_NUMBER),
            position,
            normal,
        })
    }

    /// Distance along `normal` (falling back to the slice's own).
    fn along(&self, normal: Option<[f64; 3]>) -> Option<f64> {
        let n = normal.or(self.normal)?;
        let p = self.position?;
        Some(p[0] * n[0] + p[1] * n[1] + p[2] * n[2])
    }
}

/// Sorts slices of one series: by Instance Number when every slice has a
/// different one, else by position along the slice normal when every
/// slice has a position, else by file name.
pub fn sort(slices: &mut [Slice]) {
    let name = |s: &Slice| s.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let mut numbers: Vec<i64> = slices.iter().filter_map(|s| s.instance).collect();
    numbers.sort_unstable();
    numbers.dedup();
    if numbers.len() == slices.len() {
        slices.sort_by(|a, b| a.instance.cmp(&b.instance).then_with(|| natural_cmp(&name(a), &name(b))));
        return;
    }
    let normal = slices.iter().find_map(|s| s.normal);
    if slices.iter().all(|s| s.along(normal).is_some()) {
        slices.sort_by(|a, b| {
            let (x, y) = (a.along(normal).unwrap_or(0.0), b.along(normal).unwrap_or(0.0));
            x.total_cmp(&y).then_with(|| a.instance.cmp(&b.instance)).then_with(|| natural_cmp(&name(a), &name(b)))
        });
        return;
    }
    slices.sort_by(|a, b| a.instance.cmp(&b.instance).then_with(|| natural_cmp(&name(a), &name(b))));
}

/// Compares names with runs of digits by value ("IM2" before "IM10").
pub fn natural_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    let key = |s: &str| {
        let mut parts: Vec<(String, u128)> = Vec::new();
        let mut text = String::new();
        let mut digits = String::new();
        for ch in s.chars() {
            if ch.is_ascii_digit() {
                digits.push(ch);
            } else {
                if !digits.is_empty() {
                    parts.push((std::mem::take(&mut text), digits.parse().unwrap_or(u128::MAX)));
                    digits.clear();
                }
                text.push(ch.to_ascii_lowercase());
            }
        }
        parts.push((text, if digits.is_empty() { 0 } else { digits.parse().unwrap_or(u128::MAX) }));
        parts
    };
    key(a).cmp(&key(b)).then_with(|| a.cmp(b))
}

/// True when the file at `path` starts like DICOM.
pub fn is_dicom_file(path: &Path) -> bool {
    let Ok(mut f) = std::fs::File::open(path) else { return false };
    let mut head = [0u8; 512];
    let mut n = 0;
    while n < head.len() {
        match f.read(&mut head[n..]) {
            Ok(0) => break,
            Ok(k) => n += k,
            Err(_) => return false,
        }
    }
    looks_like_dicom(&head[..n])
}

/// The DICOM files in a folder, recognised by content.  DICOMDIR files
/// and hidden files are left out.
pub fn folder_files(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    entries
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().map(|t| t.is_file() || t.is_symlink()).unwrap_or(false))
        .map(|e| e.path())
        .filter(|p| {
            let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
            !name.starts_with('.') && !name.eq_ignore_ascii_case("dicomdir")
        })
        .take(MAX_FILES)
        .filter(|p| is_dicom_file(p))
        .collect()
}

/// One series of a folder: its files in slice order.
#[derive(Clone, Debug, PartialEq)]
pub struct Series {
    pub uid: String,
    pub number: Option<i64>,
    pub description: String,
    pub modality: String,
    pub files: Vec<PathBuf>,
}

impl Series {
    /// "3: Chest 1.25 mm (CT, 133 images)".
    pub fn label(&self) -> String {
        let mut s = String::new();
        if let Some(n) = self.number {
            s.push_str(&format!("{n}: "));
        }
        s.push_str(if self.description.is_empty() { "Series" } else { &self.description });
        let images = if self.files.len() == 1 { "1 image".to_string() } else { format!("{} images", self.files.len()) };
        if self.modality.is_empty() { format!("{s} ({images})") } else { format!("{s} ({}, {images})", self.modality) }
    }
}

/// Reads the headers of a folder's DICOM files (a few threads at once:
/// most of the time is opening files).
fn read_slices(files: &[PathBuf]) -> Vec<Slice> {
    std::thread::scope(|s| {
        let chunks: Vec<&[PathBuf]> = files.chunks(files.len().div_ceil(4).max(1)).collect();
        let handles: Vec<_> = chunks
            .into_iter()
            .map(|chunk| s.spawn(move || chunk.iter().filter_map(|p| Slice::read(p)).collect::<Vec<_>>()))
            .collect();
        handles.into_iter().flat_map(|h| h.join().unwrap_or_default()).collect()
    })
}

/// Every series in a folder, ordered by series number, each with its files
/// in slice order.  Files without a Series Instance UID are each a series
/// of their own.
pub fn folder_series(dir: &Path) -> Vec<Series> {
    let mut slices = read_slices(&folder_files(dir));
    let mut out: Vec<Series> = Vec::new();
    slices.sort_by(|a, b| a.series.cmp(&b.series));
    let mut i = 0;
    while i < slices.len() {
        let uid = slices[i].series.clone();
        let end = if uid.is_empty() { i + 1 } else { i + slices[i..].iter().take_while(|s| s.series == uid).count() };
        let group = &mut slices[i..end];
        sort(group);
        let first = &group[0];
        out.push(Series {
            uid: uid.clone(),
            number: first.number,
            description: first.description.clone(),
            modality: first.modality.clone(),
            files: group.iter().map(|s| s.path.clone()).collect(),
        });
        i = end;
    }
    out.sort_by(|a, b| {
        a.number.unwrap_or(i64::MAX).cmp(&b.number.unwrap_or(i64::MAX)).then_with(|| a.description.cmp(&b.description))
    });
    out
}

/// The open file's series, in order: every file in its folder with the
/// same Series Instance UID.  Files without a series UID form a series of
/// their own.  Always includes `current` when it is readable.
pub fn series_of(current: &Path) -> Vec<PathBuf> {
    let Some(me) = Slice::read(current) else { return vec![current.to_path_buf()] };
    if me.series.is_empty() {
        return vec![current.to_path_buf()];
    }
    let dir = current.parent().filter(|d| !d.as_os_str().is_empty()).unwrap_or(Path::new("."));
    let mut slices = read_slices(&folder_files(dir));
    slices.retain(|s| s.series == me.series);
    if !slices.iter().any(|s| same_file(&s.path, current)) {
        slices.push(me);
    }
    sort(&mut slices);
    slices.into_iter().map(|s| s.path).collect()
}

/// Whether two paths name the same file, comparing folders by their
/// canonical form.
pub fn same_file(a: &Path, b: &Path) -> bool {
    a == b
        || a.file_name() == b.file_name()
            && a.parent().map(|p| p.canonicalize().ok()) == b.parent().map(|p| p.canonicalize().ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn slice(name: &str, instance: Option<i64>, z: Option<f64>) -> Slice {
        Slice {
            path: PathBuf::from(name),
            series: "1.2".into(),
            number: None,
            description: String::new(),
            modality: String::new(),
            instance,
            position: z.map(|z| [0.0, 0.0, z]),
            normal: Some([0.0, 0.0, 1.0]),
        }
    }

    fn names(v: &[Slice]) -> Vec<&str> {
        v.iter().map(|s| s.path.to_str().unwrap()).collect()
    }

    #[test]
    fn orders_by_instance_number() {
        let mut v = vec![slice("c", Some(3), None), slice("a", Some(10), None), slice("b", Some(2), None)];
        sort(&mut v);
        assert_eq!(names(&v), ["b", "c", "a"]);
    }

    #[test]
    fn falls_back_to_position_along_the_normal() {
        // Repeated instance numbers: position decides.
        let mut v = vec![slice("x", Some(1), Some(5.0)), slice("y", Some(1), Some(-2.5)), slice("z", None, Some(0.0))];
        sort(&mut v);
        assert_eq!(names(&v), ["y", "z", "x"]);
        // A tilted series sorts along its own normal, not z.
        let n = [0.0, 0.6, 0.8];
        let mut t: Vec<Slice> = [("p", [0.0, 1.0, 0.0]), ("q", [0.0, 0.0, 0.5]), ("r", [0.0, -3.0, 0.0])]
            .into_iter()
            .map(|(name, p)| Slice { position: Some(p), normal: Some(n), instance: None, ..slice(name, None, None) })
            .collect();
        sort(&mut t);
        assert_eq!(names(&t), ["r", "q", "p"]);
        // Neither: file names in natural order.
        let mut f = vec![slice("IM10", None, None), slice("IM2", None, None)];
        sort(&mut f);
        assert_eq!(names(&f), ["IM2", "IM10"]);
    }

    #[test]
    fn natural_order() {
        assert!(natural_cmp("IM000002", "IM000010").is_lt());
        assert!(natural_cmp("im2", "IM10").is_lt());
        assert!(natural_cmp("b", "a10").is_gt());
    }
}
