//! What is inside a file: the items of an archive (a Doom WAD's lumps and
//! maps, a PK3's files, a Quake PAK's files), the pieces of any other file
//! (a GLB's nodes, meshes, materials, textures and animations, a KTX2's mip
//! levels and faces, an Aseprite sprite's layers, frames and tags, a PDF's
//! pages), the formats each can be saved as, and the functions that save
//! them.
//!
//! An [`Item`] has a path that makes a tree, a kind, a size and the
//! [`Format`]s it can be saved as; where its bytes come from is its
//! [`Source`]: an archive entry, a piece derived from one such as a
//! composite texture, or a [`Piece`] of what the file's loader produced.
//! The panel, the command line and the clipboard code only see items and
//! formats, and ask [`Contents::show`] what showing an item means.
//!
//! Pieces are listed from what the loader already produced
//! ([`Contents::of_loaded`]), so listing a large model costs no second
//! load.  Saving a scene piece as GLB needs the scene itself, which the
//! viewer hands over when it is wanted ([`Contents::set_scene`]); without
//! one the file is loaded again once.
//!
//! ```no_run
//! use slerp_core::contents::{Contents, Format};
//! let contents = Contents::open(std::path::Path::new("doom2.wad"))?;
//! for item in contents.items() {
//!     println!("{}  {} bytes  {:?}", item.path, item.size, item.formats);
//! }
//! let (name, png) = contents.export(contents.items()[3].id, Format::Png)?;
//! # let _ = (name, png);
//! # Ok::<(), slerp_core::contents::ContentsError>(())
//! ```
//!
//! # Pieces
//!
//! - `export` turns an archive item into the bytes of a file (raw, or
//!   converted).
//! - `pieces` lists the pieces of a loaded file; `save` turns one into a
//!   file (original bytes, PNG, EXR, GLB); `exr` writes EXR files;
//!   `panorama` turns a cube map into one equirectangular picture.
//! - `names` makes file names safe and matches globs.
//! - `extract` writes items under a folder without ever leaving it.
//! - `preview` says what the viewer should show for an item.
//! - `temp` is the per-user temporary folder that pasted and dragged items
//!   are written to.
//! - `thumbs` draws the row pictures of meshes, nodes and collections.

mod export;
mod exr;
mod extract;
mod glb;
mod names;
mod panorama;
mod pieces;
mod preview;
mod save;
mod sheet;
mod temp;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_pieces;
mod thumbs;

pub use export::{convert_options, export, png_bytes, png_offsets};
pub use exr::exr_bytes;
pub use extract::{ExtractOptions, ExtractReport, Selected, extract, extract_file, list_file, list_text, select};
pub use names::{glob_match, safe_component, safe_relative_path};
pub use pieces::{Piece, Show, clone_scene};
pub use preview::{Preview, PreviewKind};
pub use save::layer_alone;
pub use sheet::gallery;
pub use temp::{TempFolder, clean_stale};
pub use thumbs::MAX_PIECE_SIZE;

use slerp_archive::{Archive, ArchiveError, DataKind, Entry, EntryKind, GroupKind};
use slerp_quake::convert::CompositeInfo;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

#[derive(Debug, thiserror::Error)]
pub enum ContentsError {
    #[error(transparent)]
    Archive(#[from] ArchiveError),
    #[error("{0}")]
    Unsupported(String),
    #[error("{0}")]
    Convert(String),
    #[error("no such item")]
    NoSuchItem,
    #[error("{0}")]
    Unsafe(String),
    #[error(
        "stopped after writing {0} bytes, more than the {1} an archive of this size should hold; use --max-total to allow more"
    )]
    TooLargeOutput(u64, u64),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, ContentsError>;

/// An item's position in [`Contents::items`], stable while the file is open.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ItemId(pub usize);

/// What an item is, for icons and for choosing how to show it.
///
/// Code that matches on it keeps a catch-all arm, since formats keep adding
/// kinds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ItemKind {
    Picture,
    Flat,
    Sprite,
    Sound,
    Music,
    Texture,
    Map,
    Palette,
    Text,
    Model,
    Marker,
    Archive,
    Other,
    /// A whole scene: everything a model or level file holds.
    Scene,
    /// A node of a scene's hierarchy, with whatever hangs beneath it.
    Node,
    /// A named group of nodes the authoring program organised the scene in
    /// (a Blender collection), with the groups inside it.
    Collection,
    Mesh,
    Material,
    Animation,
    /// The joints a skinned mesh follows.
    Skeleton,
    /// One morph target (blend shape) of a mesh.
    BlendShape,
    /// A named expression that drives several blend shapes (VRM's).
    Expression,
    Camera,
    Light,
    /// One level of detail of a model (a MetaHuman's LOD 0 to 7).
    Detail,
    /// A layer of a layered picture or an EXR file.
    Layer,
    /// One frame of an animated picture or sprite.
    Frame,
    /// A named run of frames (an Aseprite tag).
    Tag,
    /// One mip level of a texture.
    Mip,
    /// One face of a cube map, or one layer of a texture array.
    Face,
    /// One page, sheet, level or other part of a file shown one at a time.
    Part,
    /// A control of a rig, which only means something inside its rig.
    Control,
}

impl ItemKind {
    pub fn label(self) -> &'static str {
        match self {
            ItemKind::Picture => "picture",
            ItemKind::Flat => "flat",
            ItemKind::Sprite => "sprite",
            ItemKind::Sound => "sound",
            ItemKind::Music => "music",
            ItemKind::Texture => "texture",
            ItemKind::Map => "map",
            ItemKind::Palette => "palette",
            ItemKind::Text => "text",
            ItemKind::Model => "model",
            ItemKind::Marker => "marker",
            ItemKind::Archive => "archive",
            ItemKind::Other => "other",
            ItemKind::Scene => "scene",
            ItemKind::Node => "node",
            ItemKind::Collection => "collection",
            ItemKind::Mesh => "mesh",
            ItemKind::Material => "material",
            ItemKind::Animation => "animation",
            ItemKind::Skeleton => "skeleton",
            ItemKind::BlendShape => "blend shape",
            ItemKind::Expression => "expression",
            ItemKind::Camera => "camera",
            ItemKind::Light => "light",
            ItemKind::Detail => "detail",
            ItemKind::Layer => "layer",
            ItemKind::Frame => "frame",
            ItemKind::Tag => "tag",
            ItemKind::Mip => "mip",
            ItemKind::Face => "face",
            ItemKind::Part => "part",
            ItemKind::Control => "control",
        }
    }

    fn of(kind: EntryKind) -> ItemKind {
        match kind {
            EntryKind::Picture => ItemKind::Picture,
            EntryKind::Flat => ItemKind::Flat,
            EntryKind::Sprite => ItemKind::Sprite,
            EntryKind::Sound => ItemKind::Sound,
            EntryKind::Music => ItemKind::Music,
            EntryKind::Texture => ItemKind::Texture,
            EntryKind::Map => ItemKind::Map,
            EntryKind::Palette => ItemKind::Palette,
            EntryKind::Text => ItemKind::Text,
            EntryKind::Model => ItemKind::Model,
            EntryKind::Marker => ItemKind::Marker,
            EntryKind::Archive => ItemKind::Archive,
            EntryKind::Other => ItemKind::Other,
        }
    }
}

/// A file format an item can be saved as.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Format {
    /// Whatever the item's choice is: converted pictures, sounds and music,
    /// the original bytes of an embedded picture, a mesh as GLB, everything
    /// else as it is.  Only a request; no item lists it.
    Auto,
    /// The item's bytes as stored.
    Raw,
    Png,
    /// OpenEXR, for pictures whose values go beyond 0 to 1 (HDR skies,
    /// floating-point textures and EXR layers).
    Exr,
    Wav,
    /// Standard MIDI.
    Mid,
    /// A map as a standalone PWAD of its lumps.
    Wad,
    /// Binary glTF: a scene, node, collection, mesh, material, skeleton or
    /// animation.
    Glb,
}

impl Format {
    pub fn extension(self) -> &'static str {
        match self {
            Format::Auto | Format::Raw => "",
            Format::Png => "png",
            Format::Exr => "exr",
            Format::Wav => "wav",
            Format::Mid => "mid",
            Format::Wad => "wad",
            Format::Glb => "glb",
        }
    }

    /// The word for a format choice, on the command line and in the panel.
    pub fn name(self) -> &'static str {
        match self {
            Format::Auto => "auto",
            Format::Raw => "raw",
            Format::Png => "png",
            Format::Exr => "exr",
            Format::Wav => "wav",
            Format::Mid => "mid",
            Format::Wad => "wad",
            Format::Glb => "glb",
        }
    }

    /// The name shown on a button or in a menu.
    pub fn label(self) -> &'static str {
        match self {
            Format::Auto => "Converted (automatic)",
            Format::Raw => "As stored",
            Format::Png => "PNG picture",
            Format::Exr => "OpenEXR picture",
            Format::Wav => "WAV sound",
            Format::Mid => "MIDI music",
            Format::Wad => "WAD of the map",
            Format::Glb => "GLB model",
        }
    }

    pub fn parse(word: &str) -> Option<Format> {
        Some(match word.to_ascii_lowercase().as_str() {
            "auto" => Format::Auto,
            "raw" => Format::Raw,
            "png" => Format::Png,
            "exr" => Format::Exr,
            "wav" => Format::Wav,
            "mid" | "midi" => Format::Mid,
            "wad" => Format::Wad,
            "glb" => Format::Glb,
            _ => return None,
        })
    }

    /// The formats a choice offers, in the order a menu lists them.
    pub const CHOICES: [Format; 8] =
        [Format::Auto, Format::Raw, Format::Png, Format::Exr, Format::Wav, Format::Mid, Format::Wad, Format::Glb];
}

/// Where an item's bytes come from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
    /// An entry of the archive, by index.
    Entry(usize),
    /// A composite texture a definitions lump (TEXTURE1, TEXTURES) makes of
    /// patches: the lump's entry and the texture's name.
    Composite { lump: usize, name: String },
    /// A piece of what the file's loader produced.
    Piece(Piece),
}

/// One thing in a file that can be shown and saved.
#[derive(Clone, Debug)]
pub struct Item {
    pub id: ItemId,
    /// The path in the tree, `/`-separated, as the archive stores it (a Doom
    /// WAD's lumps in their namespace or map folder).  Untrusted text.
    pub path: String,
    pub kind: ItemKind,
    pub size: u64,
    /// Facts known without reading the item (a composite texture's size).
    pub detail: String,
    /// The formats it can be saved as, [`Format::Raw`] first when it has
    /// bytes of its own.  Empty for items that only mean something inside
    /// their file (a camera, a rig's control), which are listed but can't
    /// be saved or dragged.
    pub formats: Vec<Format>,
    pub source: Source,
    /// Encrypted or compressed in a way that can't be read.
    pub locked: bool,
}

impl Item {
    /// Whether the item can be saved as a file of its own.
    pub fn can_save(&self) -> bool {
        !self.formats.is_empty() && !self.locked
    }

    /// The last part of the path.
    pub fn name(&self) -> &str {
        self.path.rsplit('/').next().unwrap_or(&self.path)
    }

    pub fn folder(&self) -> &str {
        self.path.rsplit_once('/').map_or("", |(f, _)| f)
    }

    /// The format `requested` means for this item.  [`Format::Auto`] picks,
    /// for an archive's entries, the conversion for pictures, textures,
    /// sounds and music and the raw bytes for the rest; for the pieces of
    /// other files, their first format, which is the original bytes when
    /// there are any (an embedded PNG comes out unchanged) and otherwise the
    /// conversion that loses nothing (PNG for 8-bit pictures, EXR for
    /// floating-point ones, GLB for meshes).  A format the item can't be
    /// saved as falls back to the raw bytes (or, for items with none, the
    /// first format).
    pub fn resolve(&self, requested: Format) -> Format {
        let has = |f: Format| self.formats.contains(&f);
        let auto = || {
            if let Source::Piece(_) = self.source {
                return self.formats.first().copied().unwrap_or(Format::Raw);
            }
            let converted = match self.kind {
                ItemKind::Picture | ItemKind::Flat | ItemKind::Sprite | ItemKind::Texture => Format::Png,
                ItemKind::Sound => Format::Wav,
                ItemKind::Music => Format::Mid,
                _ => Format::Raw,
            };
            if has(converted) { converted } else { Format::Raw }
        };
        let wanted = if requested == Format::Auto { auto() } else { requested };
        if has(wanted) {
            wanted
        } else if has(Format::Raw) {
            Format::Raw
        } else {
            self.formats.first().copied().unwrap_or(Format::Raw)
        }
    }
}

/// Identifies an item across reloads of the file: its path and which of the
/// items with that path it is (Doom WADs repeat names).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ItemKey {
    pub path: String,
    pub nth: usize,
}

/// The items of an open file.
pub struct Contents {
    /// The archive the items are entries of: a game archive, or the zip a
    /// USDZ or 3MF file is, whose members are listed beside its pieces.
    archive: Option<Archive>,
    path: Option<PathBuf>,
    /// The format, for files that aren't archives.
    kind: Option<crate::Kind>,
    /// "glTF 2.0 (binary)", "KTX2", for files that aren't archives.
    label: String,
    items: Vec<Item>,
    /// The item of each archive entry.
    entry_items: Vec<ItemId>,
    palettes: OnceLock<Palettes>,
    /// The whole file when a WAD's textures are composed.
    wad: OnceLock<Vec<u8>>,
    /// What the pieces were listed from and are saved from.
    held: pieces::Held,
    /// The scene whose nodes, meshes and clips the pieces index, when one
    /// has been handed over or loaded; scene pieces are saved from it.
    scene: OnceLock<std::result::Result<std::sync::Arc<slerp_scene::Scene>, String>>,
    /// Textures shrunk for the row pictures of scene pieces.
    small_textures: std::sync::Mutex<thumbs::SmallTextures>,
    warnings: Vec<String>,
}

/// The palettes pictures are drawn with, found once.
pub(crate) struct Palettes {
    pub(crate) doom: Vec<u8>,
    pub(crate) quake: Vec<u8>,
}

/// The most items a file's composite textures may add.
const MAX_DERIVED: usize = 200_000;
/// The largest definitions lump read to list its textures.
const MAX_DEFINITIONS: u64 = 16 << 20;

impl Contents {
    /// Opens any file Slerp reads and lists what is inside.  An archive is
    /// found by its first bytes and only its directory is read, with the
    /// small lumps that define composite textures; any other file is loaded
    /// once, whole, and its pieces listed from what the loader produced.
    pub fn open(path: &Path) -> Result<Contents> {
        let kind = crate::sniff_file(path)?;
        if kind.is_none_or(is_archive_kind) {
            let archive = Archive::open(path)?;
            return Ok(Contents::from_archive(archive, Some(path.to_path_buf())));
        }
        let kind = kind.expect("checked above");
        pieces::open(kind, path)
    }

    /// Lists the pieces of a file the caller has already loaded (`part` is
    /// the part shown, for a file whose parts are `part_names`), so a large
    /// file isn't read twice.  Archives are listed from their own
    /// directory.  `None` when the file holds fewer than two pieces worth
    /// listing: a plain picture, a model of one mesh and no textures.
    pub fn of_loaded(
        kind: crate::Kind,
        path: &Path,
        asset: &slerp_scene::Asset,
        part_names: &[String],
        part: Option<&str>,
    ) -> Option<Contents> {
        if is_archive_kind(kind) {
            return Contents::open(path).ok();
        }
        pieces::of_loaded(kind, path, asset, part_names, part)
    }

    /// Lists an archive held in memory.
    pub fn from_bytes(bytes: impl Into<std::sync::Arc<[u8]>>) -> Result<Contents> {
        Ok(Contents::from_archive(Archive::from_bytes(bytes)?, None))
    }

    fn from_archive(archive: Archive, path: Option<PathBuf>) -> Contents {
        let mut items: Vec<Item> = Vec::with_capacity(archive.entries().len());
        let mut warnings: Vec<String> = archive.warnings().to_vec();
        let entry_items = list_archive(&archive, &mut items, &mut warnings, "");
        Contents {
            archive: Some(archive),
            path,
            kind: None,
            label: String::new(),
            items,
            entry_items,
            palettes: OnceLock::new(),
            wad: OnceLock::new(),
            held: pieces::Held::default(),
            scene: OnceLock::new(),
            small_textures: Default::default(),
            warnings,
        }
    }

    /// The archive the items are entries of, if they are.
    pub fn archive(&self) -> Option<&Archive> {
        self.archive.as_ref()
    }

    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// The format of a file that isn't an archive.
    pub fn kind(&self) -> Option<crate::Kind> {
        self.kind
    }

    /// The files the contents live in, for watching.
    pub fn files(&self) -> Vec<PathBuf> {
        match (&self.archive, &self.path) {
            (Some(a), _) if self.kind.is_none() => a.files(),
            (_, Some(p)) => vec![p.clone()],
            _ => Vec::new(),
        }
    }

    /// "Doom IWAD", "Valve VPK (version 2)", "glTF 2.0 (binary)".
    pub fn label(&self) -> &str {
        match &self.archive {
            Some(a) if self.kind.is_none() => a.label(),
            _ => &self.label,
        }
    }

    pub fn items(&self) -> &[Item] {
        &self.items
    }

    pub fn item(&self, id: ItemId) -> Option<&Item> {
        self.items.get(id.0)
    }

    /// Problems found while listing, for the inspector.
    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }

    /// The item of archive entry `i`.
    pub fn item_of_entry(&self, i: usize) -> Option<ItemId> {
        self.entry_items.get(i).copied()
    }

    /// The archive entry behind an item, if it has one.
    pub fn entry(&self, id: ItemId) -> Option<&Entry> {
        match &self.item(id)?.source {
            Source::Entry(i) => self.archive.as_ref()?.entry(*i),
            Source::Composite { .. } | Source::Piece(_) => None,
        }
    }

    /// The key that finds `id` again after a reload.
    pub fn key(&self, id: ItemId) -> Option<ItemKey> {
        let item = self.item(id)?;
        let nth = self.items[..id.0].iter().filter(|i| i.path == item.path).count();
        Some(ItemKey { path: item.path.clone(), nth })
    }

    /// The item a key names in this listing.
    pub fn find_key(&self, key: &ItemKey) -> Option<ItemId> {
        self.items.iter().filter(|i| i.path == key.path).nth(key.nth).map(|i| i.id)
    }

    /// The items beneath a folder path (`Sprites`, `MAP01`), in order.
    pub fn items_in(&self, folder: &str) -> Vec<ItemId> {
        let prefix = format!("{}/", folder.trim_end_matches('/'));
        self.items.iter().filter(|i| i.path.starts_with(&prefix)).map(|i| i.id).collect()
    }

    /// The map folders of a Doom WAD: (folder path, marker item).
    pub fn maps(&self) -> Vec<(String, ItemId)> {
        let Some(archive) = &self.archive else { return Vec::new() };
        archive
            .groups()
            .iter()
            .filter(|g| g.kind == GroupKind::Map)
            .filter_map(|g| Some((g.path.clone(), self.item_of_entry(g.first)?)))
            .collect()
    }

    /// An item's bytes as stored: an archive entry, or a picture embedded in
    /// the file as it was stored there.
    pub fn read(&self, id: ItemId) -> Result<Vec<u8>> {
        let item = self.item(id).ok_or(ContentsError::NoSuchItem)?;
        if let Source::Piece(p) = &item.source {
            return save::original(self, p)
                .ok_or_else(|| ContentsError::Unsupported("this item has no bytes of its own".into()));
        }
        let entry =
            self.entry(id).ok_or_else(|| ContentsError::Unsupported("this item has no bytes of its own".into()))?;
        Ok(self.archive.as_ref().ok_or(ContentsError::NoSuchItem)?.read(entry)?)
    }

    pub(crate) fn palettes(&self) -> &Palettes {
        self.palettes.get_or_init(|| match &self.archive {
            Some(archive) => Palettes {
                doom: find_palette(
                    archive,
                    &["PLAYPAL"],
                    DataKind::Playpal,
                    slerp_quake::convert::default_doom_palette(),
                ),
                quake: find_palette(
                    archive,
                    &["gfx/palette.lmp", "palette"],
                    DataKind::Palette768,
                    slerp_quake::convert::default_quake_palette(),
                ),
            },
            None => Palettes {
                doom: slerp_quake::convert::default_doom_palette().to_vec(),
                quake: slerp_quake::convert::default_quake_palette().to_vec(),
            },
        })
    }

    /// Hands over the scene the pieces were listed from (the viewer's own,
    /// so node, mesh and clip numbers are the ones on screen), for saving
    /// scene pieces.  Ignored when one is already held.
    pub fn set_scene(&self, scene: std::sync::Arc<slerp_scene::Scene>) {
        let _ = self.scene.set(Ok(scene));
    }

    /// Whether saving any of `ids` needs the scene and none is held yet,
    /// so the caller can hand one over first rather than have the file
    /// loaded again.
    pub fn needs_scene(&self, ids: &[ItemId]) -> bool {
        self.scene.get().is_none()
            && ids
                .iter()
                .filter_map(|&id| self.item(id))
                .any(|i| matches!(&i.source, Source::Piece(p) if p.needs_scene()))
    }

    /// What showing an item means: a part of the scene shown alone, a clip
    /// played, a mip level picked, or a preview of its own.
    pub fn show(&self, id: ItemId) -> Show {
        match self.item(id).map(|i| &i.source) {
            Some(Source::Piece(p)) => pieces::show(self, p),
            _ => Show::Preview,
        }
    }

    /// A small picture of an item for the panel's rows, when it has one
    /// that is cheap to make (pictures, mips, layers, frames).  Slow items
    /// (archive entries that need converting) give `None`; call it off the
    /// window's thread.
    pub fn thumbnail(&self, id: ItemId, size: u32) -> Option<slerp_scene::Image8> {
        save::thumbnail(self, id, size)
    }
}

/// Lists an archive's entries (and the composite textures its definitions
/// lumps make) as items under `prefix`, returning the item of each entry.
fn list_archive(archive: &Archive, items: &mut Vec<Item>, warnings: &mut Vec<String>, prefix: &str) -> Vec<ItemId> {
    let mut entry_items = Vec::with_capacity(archive.entries().len());
    let markers: std::collections::HashSet<usize> =
        archive.groups().iter().filter(|g| g.kind == GroupKind::Map).map(|g| g.first).collect();
    let mut derived = 0usize;
    for e in archive.entries() {
        let id = ItemId(items.len());
        entry_items.push(id);
        let mut formats = convert_options(e);
        if markers.contains(&e.index) {
            formats.push(Format::Wad);
        }
        items.push(Item {
            id,
            path: format!("{prefix}{}", e.path),
            kind: ItemKind::of(e.kind),
            size: e.size,
            detail: String::new(),
            formats,
            source: Source::Entry(e.index),
            locked: e.locked,
        });
        // A definitions lump's textures follow it as its children.
        if matches!(e.data, DataKind::TextureDefs | DataKind::ZdoomTextures) && !e.locked {
            for def in composite_defs(archive, e) {
                if derived >= MAX_DERIVED {
                    warnings.push("some composite textures were left out: there are too many".into());
                    break;
                }
                derived += 1;
                let id = ItemId(items.len());
                items.push(Item {
                    id,
                    path: format!("{prefix}{}/{}", e.path, def.name),
                    kind: ItemKind::Texture,
                    size: 0,
                    detail: format!(
                        "{}x{}, {} patch{}",
                        def.width,
                        def.height,
                        def.patches,
                        if def.patches == 1 { "" } else { "es" }
                    ),
                    formats: vec![Format::Png],
                    source: Source::Composite { lump: e.index, name: def.name },
                    locked: false,
                });
            }
        }
    }
    entry_items
}

/// The first 768 bytes of the archive's last entry at one of `names` (or of
/// `kind`), else the default.
fn find_palette(archive: &Archive, names: &[&str], kind: DataKind, default: &[u8; 768]) -> Vec<u8> {
    let found = names
        .iter()
        .flat_map(|n| archive.find(n).last().copied())
        .next()
        .or_else(|| archive.entries().iter().rev().find(|e| e.data == kind && e.size >= 768).map(|e| e.index));
    if let Some(i) = found
        && let Some(e) = archive.entry(i)
        && e.size >= 768
        && let Ok(bytes) = archive.read_head(e, 768)
        && bytes.len() == 768
    {
        return bytes;
    }
    default.to_vec()
}

/// The textures a definitions lump makes.
fn composite_defs(archive: &Archive, lump: &Entry) -> Vec<CompositeInfo> {
    if lump.size > MAX_DEFINITIONS {
        return Vec::new();
    }
    let Ok(bytes) = archive.read(lump) else { return Vec::new() };
    if lump.data == DataKind::ZdoomTextures {
        return slerp_quake::convert::zdoom_texture_defs(&bytes);
    }
    // TEXTURE1's patches name PNAMES entries; the last PNAMES wins.
    let pnames = archive
        .find("PNAMES")
        .last()
        .and_then(|&i| archive.entry(i))
        .filter(|e| e.size <= MAX_DEFINITIONS)
        .and_then(|e| archive.read(e).ok())
        .unwrap_or_default();
    slerp_quake::convert::texture_defs(&bytes, &pnames)
}

/// Whether `kind` is a file whose contents the archive browser shows.
pub fn is_archive_kind(kind: crate::Kind) -> bool {
    use slerp_quake::QuakeFormat;
    matches!(
        kind,
        crate::Kind::Archive(_)
            | crate::Kind::Quake(
                QuakeFormat::DoomWad | QuakeFormat::DoomArchive | QuakeFormat::Wad2 | QuakeFormat::Wad3
            )
    )
}

/// How many items of each kind, for a one-line summary.
pub fn count_kinds(items: &[Item]) -> HashMap<ItemKind, usize> {
    let mut out = HashMap::new();
    for i in items {
        *out.entry(i.kind).or_insert(0) += 1;
    }
    out
}
