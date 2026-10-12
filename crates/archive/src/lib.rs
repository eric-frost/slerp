//! Read-only access to game archives: Doom WADs, PK3 (zip) and PK7 (7z)
//! files, Quake and Quake 2 PAK files, Quake WAD2 and Half-Life WAD3 texture
//! packs, Godot PCK packs, Valve VPK packs and Build engine GRP files.
//!
//! ```no_run
//! # fn main() -> Result<(), slerp_archive::ArchiveError> {
//! let archive = slerp_archive::Archive::open(std::path::Path::new("doom2.wad"))?;
//! for entry in archive.entries() {
//!     println!("{} ({} bytes, {})", entry.path, entry.size, entry.kind.label());
//! }
//! let bytes = archive.read(&archive.entries()[0])?;
//! # let _ = bytes;
//! # Ok(()) }
//! ```
//!
//! # Safety against hostile files
//!
//! Archives come from downloads, so every number in a directory is
//! distrusted.  Reads are checked against the real length of the file before
//! anything is allocated; counts, name lengths and sizes have caps
//! ([`Limits`]); compressed entries are inflated as a stream that stops at
//! the size the directory declares, and entries that declare more than
//! deflate can produce are refused up front; offsets that lead outside the
//! file make that entry unreadable rather than the archive.  Nothing here
//! panics on bad input (the 7z reader, which is not ours, runs inside a
//! panic guard).
//!
//! # Structure
//!
//! Entries come in the archive's own order, duplicates included (later Doom
//! lumps override earlier ones), each with a `path` whose `/` separators
//! make a folder tree.  A Doom WAD shows its structure as it is: lumps
//! between `S_START` and `S_END` (and the other marker pairs, see
//! [`Namespace`]) sit in a folder named for the namespace, and a map's
//! marker lump and the lumps after it sit in a folder named for the map
//! ([`Archive::groups`] lists both kinds of folder).  Lump names keep the
//! case and spelling they were stored with.
//!
//! # Formats
//!
//! [`sniff`] recognises the formats by their first bytes, never by
//! extension.  A `.pak` file is claimed only when it starts with `PACK`
//! (or `SPAK`), since other games reuse the extension.

mod doom;
mod grp;
mod kinds;
mod pak;
mod pck;
mod read;
mod sevenz;
mod source;
pub mod unity;
mod vpk;
mod zip;

#[cfg(test)]
mod tests;
#[cfg(any(test, feature = "testutil"))]
pub mod testutil;

pub use kinds::{
    DataKind, EntryKind, Namespace, doom_lump, doom_picture_header, flat_size, from_extension, from_name, is_dmx_sound,
    looks_like_text, quake_picture_size, sniff_magic,
};
pub use zip::first_name as first_zip_name;

use source::Source;
use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

/*┌─────────────────╮
| Types           |
└─────────────────╯*/
//region Types

#[derive(Debug, thiserror::Error)]
pub enum ArchiveError {
    /// The file isn't in a format this crate reads.
    #[error("not a game archive")]
    NotMine,
    #[error("damaged archive: {0}")]
    Invalid(String),
    /// A count, name or size over one of the [`Limits`].
    #[error("too large: {0}")]
    TooLarge(String),
    #[error("not supported: {0}")]
    Unsupported(String),
    /// Encrypted data, which is never read.
    #[error("encrypted: {0}")]
    Encrypted(String),
    #[error("{0}")]
    Io(#[from] std::io::Error),
}

impl ArchiveError {
    pub(crate) fn invalid(what: impl Into<String>) -> ArchiveError {
        ArchiveError::Invalid(what.into())
    }
}

pub type Result<T> = std::result::Result<T, ArchiveError>;

/// The caps that keep a hostile archive from taking over the machine.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    /// The most entries an archive may list.  Megawads have about ten
    /// thousand, and game packs a few hundred thousand.
    pub max_entries: usize,
    /// The longest name kept, in bytes; entries with longer names are
    /// skipped (and a warning says how many).
    pub max_name_bytes: usize,
    /// The largest entry `read` returns in memory.  `read_to` streams and
    /// has no such cap.
    pub max_read: u64,
    /// The most a deflated entry may declare per stored byte.  Deflate
    /// can't exceed 1032 to 1, so more is a lie.
    pub max_ratio: u64,
    /// The largest directory read into memory (a zip's central directory, a
    /// VPK's tree, a 7z header).
    pub max_directory_bytes: u64,
    /// The largest 7z block unpacked at once, and the largest dictionary
    /// its LZMA coders may ask for.
    pub max_block_bytes: u64,
    pub max_dictionary_bytes: u64,
}

impl Default for Limits {
    fn default() -> Limits {
        Limits {
            max_entries: 1 << 20,
            max_name_bytes: 4096,
            max_read: 1 << 30,
            max_ratio: 1100,
            max_directory_bytes: 256 << 20,
            max_block_bytes: 4 << 30,
            max_dictionary_bytes: 1 << 30,
        }
    }
}

/// The archive formats.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ArchiveKind {
    /// Doom's IWAD and PWAD.
    DoomWad,
    /// A zip file: GZDoom's and Quake 3's PK3 among them.
    Zip,
    /// A 7z file: GZDoom's PK7.
    SevenZip,
    /// Quake and Quake 2 PAK (`PACK`), and Soldier of Fortune's (`SPAK`).
    Pak,
    /// Quake's texture and picture pack.
    Wad2,
    /// Half-Life's texture pack.
    Wad3,
    /// Godot's resource pack.
    Pck,
    /// Valve's pack, version 1 or 2.
    Vpk,
    /// The Build engine's group file (Duke Nukem 3D, Blood, Shadow Warrior).
    Grp,
    /// A Unity package: a gzip-compressed tar of assets in GUID folders.
    UnityPackage,
}

impl ArchiveKind {
    /// A name for titles and lists.
    pub fn label(self) -> &'static str {
        match self {
            ArchiveKind::DoomWad => "Doom WAD",
            ArchiveKind::Zip => "PK3 (zip)",
            ArchiveKind::SevenZip => "PK7 (7z)",
            ArchiveKind::Pak => "Quake PAK",
            ArchiveKind::Wad2 => "Quake WAD2",
            ArchiveKind::Wad3 => "Half-Life WAD3",
            ArchiveKind::Pck => "Godot PCK",
            ArchiveKind::Vpk => "Valve VPK",
            ArchiveKind::Grp => "Build GRP",
            ArchiveKind::UnityPackage => "Unity package",
        }
    }

    /// Whether Doom editors (SLADE) open this format, as opposed to the
    /// general game-pack editors (VPKEdit).
    pub fn is_doom_format(self) -> bool {
        matches!(self, ArchiveKind::DoomWad | ArchiveKind::Zip | ArchiveKind::SevenZip)
    }
}

/// One file in an archive.
#[derive(Clone, Debug)]
pub struct Entry {
    /// Position in [`Archive::entries`], which never changes while the
    /// archive is open.
    pub index: usize,
    /// The path inside the archive, `/`-separated with no leading slash, as
    /// stored; a WAD's lumps are put in the folder of their namespace or
    /// map.  Treat it as untrusted text: it may hold `..` or anything else.
    pub path: String,
    /// The unpacked size the directory declares.
    pub size: u64,
    /// The bytes it takes in the archive.
    pub stored_size: u64,
    pub kind: EntryKind,
    pub data: DataKind,
    pub namespace: Option<Namespace>,
    /// Encrypted, or compressed in a way this crate can't read: listed, but
    /// `read` fails.
    pub locked: bool,
}

impl Entry {
    /// The last part of the path: a lump's name as stored.
    pub fn name(&self) -> &str {
        self.path.rsplit('/').next().unwrap_or(&self.path)
    }

    /// The folder part of the path (empty at the top level).
    pub fn folder(&self) -> &str {
        self.path.rsplit_once('/').map_or("", |(f, _)| f)
    }
}

/// A folder that stands for something in a Doom WAD.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Group {
    /// The folder's path, as the entries' paths begin.
    pub path: String,
    pub kind: GroupKind,
    /// The entries' indices, first to last.
    pub first: usize,
    pub last: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GroupKind {
    /// The lumps between a pair of markers such as `F_START` and `F_END`.
    Namespace(Namespace),
    /// A map: its marker lump and the lumps after it.
    Map,
}

//endregion

/*┌─────────────────╮
| Sniffing        |
└─────────────────╯*/
//region Sniffing

/// Which archive format `head` (the first bytes of a file) starts, by magic
/// alone.  A zip is any zip; whether it is a game's is for the caller to
/// judge from the entries.
pub fn sniff(head: &[u8]) -> Option<ArchiveKind> {
    let starts = |m: &[u8]| head.starts_with(m);
    if starts(b"IWAD") || starts(b"PWAD") {
        Some(ArchiveKind::DoomWad)
    } else if starts(b"PACK") || starts(b"SPAK") {
        // Git pack files start with PACK too; `open` checks the directory
        // fields, which differ.
        Some(ArchiveKind::Pak)
    } else if starts(b"WAD2") {
        Some(ArchiveKind::Wad2)
    } else if starts(b"WAD3") {
        Some(ArchiveKind::Wad3)
    } else if starts(b"GDPC") {
        Some(ArchiveKind::Pck)
    } else if starts(&[0x34, 0x12, 0xAA, 0x55]) {
        Some(ArchiveKind::Vpk)
    } else if starts(b"KenSilverman") {
        Some(ArchiveKind::Grp)
    } else if starts(b"PK\x03\x04") || starts(b"PK\x05\x06") {
        Some(ArchiveKind::Zip)
    } else if starts(b"7z\xBC\xAF\x27\x1C") {
        Some(ArchiveKind::SevenZip)
    } else if unity::sniff(head) {
        Some(ArchiveKind::UnityPackage)
    } else {
        None
    }
}

//endregion

/*┌─────────────────╮
| Archive         |
└─────────────────╯*/
//region Archive

/// How an entry's bytes are found; one per entry, parallel to `entries`.
#[derive(Clone, Debug)]
pub(crate) enum Slot {
    /// Stored as they are, in the main file.
    Stored { offset: u64, len: u64 },
    /// A zip entry: the local header sits at `local`, and the data follows
    /// it.
    Zip { local: u64, method: u16, compressed: u64 },
    /// A VPK file: preload bytes in the directory tree, then a stretch of
    /// one of the pack's numbered files (or the directory file itself).
    Vpk { preload_at: usize, preload_len: u16, part: u16, offset: u64, len: u64 },
    /// A file in a 7z archive (an index into its file list).
    SevenZ { file: usize },
    /// A Unity package's asset: a stretch of the inflated tar.
    Unity { offset: u64, len: u64 },
    /// Something this crate won't read.
    Locked(String),
}

/// What a format's reader hands back.
pub(crate) struct Parsed {
    pub kind: ArchiveKind,
    pub entries: Vec<Entry>,
    pub slots: Vec<Slot>,
    pub groups: Vec<Group>,
    pub warnings: Vec<String>,
    pub label: String,
    pub extra: Extra,
}

pub(crate) enum Extra {
    None,
    Vpk(Box<vpk::Tree>),
    SevenZ(Box<sevenz::SevenZ>),
    Unity(Box<unity::Unity>),
}

/// An open archive.  Opening reads only the directory, so a 600 MB PK3 opens
/// in milliseconds; entries are read when asked for.
pub struct Archive {
    kind: ArchiveKind,
    label: String,
    path: Option<PathBuf>,
    source: Source,
    entries: Vec<Entry>,
    slots: Vec<Slot>,
    groups: Vec<Group>,
    warnings: Vec<String>,
    limits: Limits,
    extra: Extra,
    by_path: OnceLock<HashMap<String, Vec<usize>>>,
}

impl std::fmt::Debug for Archive {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Archive({}, {} entries)", self.label, self.entries.len())
    }
}

impl Archive {
    /// Opens the archive at `path`, finding its format by its first bytes.
    pub fn open(path: &Path) -> Result<Archive> {
        Archive::open_with(path, Limits::default())
    }

    pub fn open_with(path: &Path, limits: Limits) -> Result<Archive> {
        let source = Source::open(path)?;
        Archive::build(source, Some(path.to_path_buf()), limits)
    }

    /// Opens an archive held in memory (one inside another archive, or a
    /// download).  A VPK read this way can only reach the files stored in
    /// its own directory.
    pub fn from_bytes(bytes: impl Into<Arc<[u8]>>) -> Result<Archive> {
        Archive::from_bytes_with(bytes, Limits::default())
    }

    pub fn from_bytes_with(bytes: impl Into<Arc<[u8]>>, limits: Limits) -> Result<Archive> {
        Archive::build(Source::Memory(bytes.into()), None, limits)
    }

    fn build(source: Source, path: Option<PathBuf>, limits: Limits) -> Result<Archive> {
        // Unity packages are known by the tar headers in their first
        // kilobytes; everything else by its first bytes.
        let head = source.head(0, 4096);
        let kind = sniff(&head).ok_or(ArchiveError::NotMine)?;
        let p = match kind {
            ArchiveKind::DoomWad => doom::parse(&source, &limits)?,
            ArchiveKind::Zip => zip::parse(&source, &limits)?,
            ArchiveKind::SevenZip => sevenz::parse(&source, &limits)?,
            ArchiveKind::Pak => pak::parse(&source, &limits)?,
            ArchiveKind::Wad2 | ArchiveKind::Wad3 => pak::parse_wad(&source, &limits, kind)?,
            ArchiveKind::Pck => pck::parse(&source, &limits)?,
            ArchiveKind::Vpk => vpk::parse(&source, &limits)?,
            ArchiveKind::Grp => grp::parse(&source, &limits)?,
            ArchiveKind::UnityPackage => unity::parse(&source, &limits)?,
        };
        Ok(Archive {
            kind: p.kind,
            label: p.label,
            path,
            source,
            entries: p.entries,
            slots: p.slots,
            groups: p.groups,
            warnings: p.warnings,
            limits,
            extra: p.extra,
            by_path: OnceLock::new(),
        })
    }

    pub fn kind(&self) -> ArchiveKind {
        self.kind
    }

    /// A description with the variant: "Doom IWAD", "Valve VPK (version 2)".
    pub fn label(&self) -> &str {
        &self.label
    }

    /// The file the archive was opened from.
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// Every file the archive's entries live in, for watching: the archive
    /// itself and, for a VPK, the numbered files beside its directory.
    pub fn files(&self) -> Vec<PathBuf> {
        let Some(path) = &self.path else { return Vec::new() };
        let mut out = vec![path.clone()];
        if let Extra::Vpk(tree) = &self.extra {
            out.extend(tree.parts(path));
        }
        out
    }

    /// The entries in the archive's own order, duplicates included.
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// The folders that stand for Doom namespaces and maps.
    pub fn groups(&self) -> &[Group] {
        &self.groups
    }

    /// Problems met while reading the directory that didn't stop it:
    /// entries skipped for their names, lumps that run past the file.
    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }

    pub fn limits(&self) -> &Limits {
        &self.limits
    }

    /// Whether this is a 7z archive, whose entries are cheaper read
    /// together ([`Archive::read_many`]).
    pub fn is_solid(&self) -> bool {
        matches!(self.extra, Extra::SevenZ(_))
    }

    /// The entries at `path`, compared without case and with either slash,
    /// in archive order; the last one is the one a game would use.
    pub fn find(&self, path: &str) -> &[usize] {
        let map = self.by_path.get_or_init(|| {
            let mut map: HashMap<String, Vec<usize>> = HashMap::new();
            for e in &self.entries {
                map.entry(e.path.to_ascii_lowercase()).or_default().push(e.index);
            }
            map
        });
        let key = path.replace('\\', "/");
        map.get(key.trim_start_matches('/').to_ascii_lowercase().as_str()).map_or(&[], Vec::as_slice)
    }

    /// The whole file the archive was opened from, for readers that need
    /// all of it in memory (composing a WAD's textures).  Refused over
    /// [`Limits::max_read`].
    pub fn source_bytes(&self) -> Result<Vec<u8>> {
        if self.source.len() > self.limits.max_read {
            return Err(ArchiveError::TooLarge(format!("the archive is {} bytes", self.source.len())));
        }
        self.source.bytes(0, self.source.len())
    }

    /// The entry the directory's index `i` names.
    pub fn entry(&self, i: usize) -> Option<&Entry> {
        self.entries.get(i)
    }

    /// An entry's whole contents.  Refuses entries over [`Limits::max_read`];
    /// stream larger ones with [`Archive::read_to`].
    pub fn read(&self, entry: &Entry) -> Result<Vec<u8>> {
        if entry.size > self.limits.max_read {
            return Err(ArchiveError::TooLarge(format!("{} is {} bytes", entry.path, entry.size)));
        }
        // The declared size can lie, so the buffer starts small and grows
        // with what actually arrives.
        let mut out = Vec::with_capacity(entry.size.min(16 << 20) as usize);
        self.stream(entry, u64::MAX, &mut |chunk| {
            out.extend_from_slice(chunk);
            Ok(true)
        })?;
        Ok(out)
    }

    /// Writes an entry's contents to `out` as they are unpacked, without
    /// holding them in memory.  Returns the number of bytes written.
    pub fn read_to<W: Write + ?Sized>(&self, entry: &Entry, out: &mut W) -> Result<u64> {
        self.stream(entry, u64::MAX, &mut |chunk| {
            out.write_all(chunk)?;
            Ok(true)
        })
    }

    /// The first `max` bytes of an entry (fewer when it is shorter), for
    /// recognising what it is without unpacking all of it.
    pub fn read_head(&self, entry: &Entry, max: usize) -> Result<Vec<u8>> {
        let mut out = Vec::with_capacity(max.min(1 << 20));
        self.stream(entry, max as u64, &mut |chunk| {
            let room = max - out.len();
            out.extend_from_slice(&chunk[..chunk.len().min(room)]);
            Ok(out.len() < max)
        })?;
        Ok(out)
    }

    /// Several entries' contents, each an error when it can't be read.  For
    /// a 7z archive this unpacks each solid block once rather than once per
    /// entry.
    pub fn read_many(&self, wanted: &[usize]) -> Vec<Result<Vec<u8>>> {
        match &self.extra {
            Extra::SevenZ(z) => sevenz::read_many(self, z, wanted),
            _ => wanted
                .iter()
                .map(|&i| {
                    self.entries.get(i).ok_or_else(|| ArchiveError::invalid("no such entry")).and_then(|e| self.read(e))
                })
                .collect(),
        }
    }

    /// Streams up to `max` bytes of an entry into `sink`, which returns
    /// `false` to stop early.  Returns the bytes delivered.
    pub(crate) fn stream(&self, entry: &Entry, max: u64, sink: &mut dyn FnMut(&[u8]) -> Result<bool>) -> Result<u64> {
        let slot = self.slots.get(entry.index).ok_or_else(|| ArchiveError::invalid("no such entry"))?;
        let size = self.entries[entry.index].size;
        match slot {
            Slot::Locked(why) => Err(ArchiveError::Encrypted(format!("{}: {why}", entry.path))),
            Slot::Stored { offset, len } => read::copy(&self.source, *offset, *len, max, sink),
            Slot::Zip { local, method, compressed } => {
                zip::stream(&self.source, &self.limits, &entry.path, *local, *method, *compressed, size, max, sink)
            }
            Slot::Vpk { preload_at, preload_len, part, offset, len } => {
                let Extra::Vpk(tree) = &self.extra else { return Err(ArchiveError::invalid("VPK without a tree")) };
                tree.stream(self, *preload_at, *preload_len, *part, *offset, *len, max, sink)
            }
            Slot::SevenZ { file } => {
                let Extra::SevenZ(z) = &self.extra else { return Err(ArchiveError::invalid("7z without a header")) };
                sevenz::stream(z, *file, size, max, sink)
            }
            Slot::Unity { offset, len } => {
                let Extra::Unity(u) = &self.extra else {
                    return Err(ArchiveError::invalid("a package without its stream"));
                };
                unity::stream(&self.source, u, *offset, *len, max, sink)
            }
        }
    }

    pub(crate) fn slot(&self, i: usize) -> Option<&Slot> {
        self.slots.get(i)
    }

    pub(crate) fn main_source(&self) -> &Source {
        &self.source
    }
}

//endregion
