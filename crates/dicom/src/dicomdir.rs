//! DICOMDIR files: the index a CD or DVD of DICOM images carries, a tree of
//! patient, study, series and image records linked by file offsets, each
//! image record naming its file by a relative path.

use crate::data::{DataSet, tags};
use std::path::{Path, PathBuf};

/// One directory record and the records below it.
#[derive(Clone, Debug, Default)]
pub struct Record {
    /// PATIENT, STUDY, SERIES, IMAGE, ...
    pub kind: String,
    /// The record's elements (patient name, study date, modality, ...).
    pub data: DataSet,
    /// The file an IMAGE (or other leaf) record names, resolved against the
    /// DICOMDIR's folder; `None` when it names none or it doesn't exist.
    pub file: Option<PathBuf>,
    pub children: Vec<Record>,
}

/// Records below which a tree can't go deeper, against loops of offsets.
const MAX_DEPTH: usize = 16;

/// Reads the record tree of a DICOMDIR.
pub fn records(ds: &DataSet, dir: &Path) -> Vec<Record> {
    let items = ds.items(tags::DIRECTORY_RECORD_SEQUENCE);
    if items.is_empty() {
        return Vec::new();
    }
    let by_offset = |offset: u64| items.iter().position(|i| i.offset == offset);
    let root = ds.int(tags::FIRST_ROOT_RECORD).and_then(|o| by_offset(o as u64));
    let mut used = vec![false; items.len()];
    let tree = match root {
        Some(first) => chain(items, first, &by_offset, dir, &mut used, 0),
        None => Vec::new(),
    };
    if !tree.is_empty() {
        return tree;
    }
    // Offsets that don't resolve (rewritten files): nest the records by
    // their order and level instead.
    nest_by_order(items, dir)
}

/// A chain of sibling records starting at item `first`, with their
/// children.
fn chain(
    items: &[DataSet],
    first: usize,
    by_offset: &dyn Fn(u64) -> Option<usize>,
    dir: &Path,
    used: &mut [bool],
    depth: usize,
) -> Vec<Record> {
    let mut out = Vec::new();
    let mut next = Some(first);
    while let Some(i) = next {
        if used[i] || depth > MAX_DEPTH {
            break;
        }
        used[i] = true;
        let item = &items[i];
        let mut record = record_of(item, dir);
        if let Some(lower) = item.int(tags::LOWER_LEVEL_RECORD).filter(|&o| o > 0).and_then(|o| by_offset(o as u64)) {
            record.children = chain(items, lower, by_offset, dir, used, depth + 1);
        }
        out.push(record);
        next = item.int(tags::NEXT_RECORD).filter(|&o| o > 0).and_then(|o| by_offset(o as u64));
    }
    out
}

fn level(kind: &str) -> usize {
    match kind {
        "PATIENT" => 0,
        "STUDY" => 1,
        "SERIES" => 2,
        _ => 3,
    }
}

fn nest_by_order(items: &[DataSet], dir: &Path) -> Vec<Record> {
    let mut roots: Vec<Record> = Vec::new();
    for item in items {
        let record = record_of(item, dir);
        let lvl = level(&record.kind);
        // Walk down the last records to the parent level.
        let mut list = &mut roots;
        for _ in 0..lvl {
            if list.last().is_none() {
                break;
            }
            let last = list.len() - 1;
            list = &mut list[last].children;
        }
        list.push(record);
    }
    roots
}

fn record_of(item: &DataSet, dir: &Path) -> Record {
    let kind = item.string(tags::RECORD_TYPE).unwrap_or_else(|| "RECORD".into()).to_ascii_uppercase();
    let file = {
        let parts = item.strings(tags::REFERENCED_FILE_ID);
        let parts: Vec<&str> = parts.iter().map(|s| s.as_str()).filter(|s| !s.is_empty()).collect();
        (!parts.is_empty()).then(|| resolve(dir, &parts)).flatten()
    };
    Record { kind, data: item.clone(), file, children: Vec::new() }
}

/// Finds a file from its path components, matching each case-insensitively
/// when the exact name isn't there (CDs are written in capitals, and file
/// systems disagree about case).  Components that try to leave the folder
/// are refused.
pub fn resolve(dir: &Path, parts: &[&str]) -> Option<PathBuf> {
    let mut path = dir.to_path_buf();
    for part in parts {
        if *part == ".." || part.contains('/') || part.contains('\\') {
            return None;
        }
        let exact = path.join(part);
        if exact.exists() {
            path = exact;
            continue;
        }
        let entry = std::fs::read_dir(&path)
            .ok()?
            .filter_map(|e| e.ok())
            .find(|e| e.file_name().to_string_lossy().eq_ignore_ascii_case(part))?;
        path = entry.path();
    }
    path.is_file().then_some(path)
}

/// The first image file the records name, depth first.
pub fn first_image(records: &[Record]) -> Option<PathBuf> {
    for r in records {
        if let Some(f) = &r.file {
            return Some(f.clone());
        }
        if let Some(f) = first_image(&r.children) {
            return Some(f);
        }
    }
    None
}

/// Counts the records of each kind, for a summary.
pub fn count(records: &[Record], kind: &str) -> usize {
    records.iter().map(|r| (r.kind == kind) as usize + count(&r.children, kind)).sum()
}
