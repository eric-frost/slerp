//! PK3 and PK7 archives: the zip and 7z files GZDoom and other source
//! ports load as resource files.  Their folders stand for Doom's namespaces
//! (`flats/`, `textures/`, `patches/`, `sprites/`, `maps/` and so on), and
//! a file's lump name is its name without extension, cut to 8 characters.
//!
//! Megawads run to hundreds of megabytes, so an archive is read piece by
//! piece: the directory first, then only the entries a map needs.  The
//! reading itself is `slerp-archive`'s; this module gives it the shape the
//! Doom loader uses.

use crate::read::Result;
use slerp_archive::ArchiveError;
use slerp_scene::LoadError;
use std::path::{Path, PathBuf};

/// Where an archive's bytes come from: memory (a file already read, or an
/// archive inside another) or a file on disk, read at offsets so a
/// gigabyte archive costs only the parts that are used.
#[derive(Clone)]
pub(crate) enum Source<'a> {
    Memory(&'a [u8]),
    File(PathBuf),
}

impl Source<'_> {
    pub fn open(path: &Path) -> std::io::Result<Source<'static>> {
        std::fs::metadata(path)?;
        Ok(Source::File(path.to_path_buf()))
    }
}

/*┌─────────────────╮
| Archives        |
└─────────────────╯*/
//region Archives

/// One file in an archive.
#[derive(Clone, Debug)]
pub(crate) struct Entry {
    /// The path inside the archive, with `/` separators and no leading
    /// slash, as stored (source ports compare it without regard to case).
    pub path: String,
    /// The unpacked size the directory claims.
    pub size: u64,
}

pub(crate) struct Archive {
    pub entries: Vec<Entry>,
    inner: slerp_archive::Archive,
}

/// Whether the first bytes are a zip's (a local file header, or the end
/// record of an empty zip) or a 7z's signature.
pub fn is_zip(head: &[u8]) -> bool {
    slerp_archive::sniff(head) == Some(slerp_archive::ArchiveKind::Zip)
}

pub fn is_7z(head: &[u8]) -> bool {
    slerp_archive::sniff(head) == Some(slerp_archive::ArchiveKind::SevenZip)
}

fn load_error(e: ArchiveError) -> LoadError {
    match e {
        ArchiveError::NotMine => LoadError::NotMine,
        ArchiveError::TooLarge(m) => LoadError::TooLarge(m),
        ArchiveError::Unsupported(m) | ArchiveError::Encrypted(m) => LoadError::Unsupported(m),
        e => LoadError::invalid(e.to_string()),
    }
}

impl Archive {
    /// Reads an archive's directory.  `source` must be a zip or 7z.
    pub fn open(source: Source) -> Result<Archive> {
        let inner = match source {
            Source::Memory(bytes) => slerp_archive::Archive::from_bytes(bytes),
            Source::File(path) => slerp_archive::Archive::open(&path),
        }
        .map_err(load_error)?;
        if !matches!(inner.kind(), slerp_archive::ArchiveKind::Zip | slerp_archive::ArchiveKind::SevenZip) {
            return Err(LoadError::NotMine);
        }
        let entries = inner.entries().iter().map(|e| Entry { path: e.path.clone(), size: e.size }).collect();
        Ok(Archive { entries, inner })
    }

    /// Unpacks entry `i`, refusing entries larger than `max` bytes.
    pub fn read(&self, i: usize, max: u64) -> Result<Vec<u8>> {
        let e = self.entries.get(i).ok_or_else(|| LoadError::invalid("no such archive entry"))?;
        if e.size > max {
            return Err(LoadError::TooLarge(format!("{}: {} bytes", e.path, e.size)));
        }
        let entry = self.inner.entry(i).ok_or_else(|| LoadError::invalid("no such archive entry"))?;
        self.inner.read(entry).map_err(load_error)
    }

    /// Unpacks several entries at once, each `None` when it can't be read.
    /// For 7z archives this decodes each solid block once rather than once
    /// per entry.
    pub fn read_many(&self, wanted: &[usize], max: u64) -> Vec<Option<Vec<u8>>> {
        let allowed: Vec<usize> =
            wanted.iter().copied().filter(|&i| self.entries.get(i).is_some_and(|e| e.size <= max)).collect();
        let mut done = self.inner.read_many(&allowed).into_iter();
        wanted.iter().map(|&i| if allowed.contains(&i) { done.next().and_then(|r| r.ok()) } else { None }).collect()
    }

    /// Whether this is a 7z archive, whose entries are cheaper read
    /// together.
    pub fn is_solid(&self) -> bool {
        self.inner.is_solid()
    }
}

//endregion

/*┌─────────────────╮
| Namespaces      |
└─────────────────╯*/
//region Namespaces

/// Doom's lump namespaces, which an archive's top folders stand for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Namespace {
    /// Files at the archive's root, and a WAD's lumps outside markers.
    Global,
    Flats,
    /// `textures/`, or a WAD's TX_START: pictures used as they are.
    Textures,
    Patches,
    Sprites,
    Graphics,
    /// High-resolution replacements, drawn at the size of what they
    /// replace.
    Hires,
    /// `maps/`: one WAD per map, named after the map.
    Maps,
    /// Folders no namespace stands for (`models/`, `shaders/`, ...):
    /// reachable only by full path.
    Other,
}

/// The namespace an archive path belongs to and its lump name: the file
/// name without its last extension, upper case, cut to 8 characters.
pub(crate) fn classify(path: &str) -> (Namespace, String) {
    let lower = path.to_ascii_lowercase();
    let (dir, file) = match lower.split_once('/') {
        Some((d, _)) => (d, lower.rsplit('/').next().unwrap_or("")),
        None => ("", lower.as_str()),
    };
    let ns = match dir {
        "" => Namespace::Global,
        "flats" => Namespace::Flats,
        "textures" => Namespace::Textures,
        "patches" => Namespace::Patches,
        "sprites" => Namespace::Sprites,
        "graphics" => Namespace::Graphics,
        "hires" => Namespace::Hires,
        "maps" => Namespace::Maps,
        _ => Namespace::Other,
    };
    let stem = file.rsplit_once('.').map_or(file, |(s, _)| s);
    let name: String = stem.chars().take(8).collect::<String>().to_ascii_uppercase();
    (ns, name)
}

//endregion
