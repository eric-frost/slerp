//! The one list of every file format Slerp handles ([`FORMATS`]), and what
//! each platform needs to know about it: MIME types and file name
//! extensions for freedesktop systems, uniform type identifiers for macOS,
//! extensions for Windows, the formats the Qt image plugin claims, and
//! whether Slerp makes thumbnails of a format and offers to open it unless
//! the user says otherwise.
//!
//! Every list of formats elsewhere is derived from this one.  Some are read
//! at run time (the viewer's folder stepping, Explorer's registration, the
//! association manager); the rest are files a desktop or build reads
//! without running Slerp, which [`generate`] writes and a test compares
//! with the checked-in copies, so packaging never needs to run anything:
//!
//! - `data/slerp.thumbnailer`: the MIME types file managers ask Slerp to
//!   thumbnail by default.
//! - `data/slerp.desktop`: the MIME types the viewer is offered for in
//!   "Open with".
//! - `data/macos/Info.plist` and `Thumbnails-Info.plist`: the app's
//!   document types and type declarations, and the Quick Look extension's
//!   content types.
//! - `qt/slerp.json` and `qt/formats_table.inc`: the Qt image plugin's keys
//!   and MIME types.
//!
//! `data/slerp.xml`, the MIME database package, stays written by hand: its
//! magic numbers, priorities and glob weights settle clashes between formats
//! that share an extension, and each carries the reasoning in a comment.  A
//! test checks that it and this table name the same types, globs and
//! aliases.
//!
//! A format here is what a user would call one kind of file and switch on or
//! off as a whole: "Quake and Half-Life models" covers two MIME types that
//! share `.mdl`; "USD scene" covers the text, binary and plain `.usd`
//! layers.  Format ids are stable, since users' choices are saved under
//! them.

pub mod generate;
mod table;

pub use table::FORMATS;

/*┌─────────────────╮
| Types           |
└─────────────────╯*/
//region Types

/// One kind of file Slerp reads.
#[derive(Debug)]
pub struct Format {
    /// Stable id, used in saved choices and on the command line: lower case,
    /// digits and hyphens ("glb", "quake-bsp").
    pub id: &'static str,
    /// What users call it, for lists and settings ("glTF binary model").
    pub name: &'static str,
    pub category: Category,
    /// The format's MIME types: the main one first, then the others the
    /// same files go by (other databases' names, sibling types sharing an
    /// extension).  The globs on them are the format's extensions.
    pub mime: &'static [Mime],
    /// Extensions the viewer steps through in a folder and confirms by
    /// content, because unrelated files share them (`.map`, `.pal`,
    /// `.nc`).  They are never claimed by name: no glob, no Windows
    /// handler, no macOS type.
    pub by_content: &'static [&'static str],
    /// Whether Slerp makes this format's thumbnails unless the user says
    /// otherwise.
    pub thumbnails: Thumbs,
    /// Whether the viewer is offered in "Open with" for it, on desktops
    /// that don't come with a viewer of their own (see [`Format::viewers`]).
    /// Slerp never makes itself the default app on its own: the manager
    /// does that only when the user asks (a desktop may still pick Slerp
    /// when it is the only app for a type).
    pub open_with: bool,
    /// Desktops that come with an app that opens the format, bits of
    /// [`KDE`], [`GNOME`], [`WINDOWS`] and [`MACOS`]: Slerp isn't offered in
    /// "Open with" there, since people already have one (PDF everywhere,
    /// PostScript in Okular and GNOME's document viewers).
    pub viewers: u8,
    /// The Qt image plugin's treatment: a picture, a turning model, or not
    /// claimed (formats other Qt plugins read, or nothing a picture viewer
    /// should open).
    pub qt: Qt,
    /// Qt format keys with their MIME types, when they aren't simply the
    /// extensions (Qt pairs keys and MIME types by position).
    pub qt_keys: &'static [(&'static str, &'static str)],
    /// macOS type identifiers.
    pub mac: Mac,
    /// Desktops whose own thumbnailers already preview the format, bits of
    /// [`KDE`], [`GNOME`], [`WINDOWS`] and [`MACOS`].
    pub others: u8,
    /// Why Slerp's thumbnails are on by default although KDE and GNOME
    /// both preview the format: what Slerp does better.
    pub better: &'static str,
}

/// Groups of formats, as the settings list them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Category {
    Model,
    PointCloud,
    Scene,
    Level,
    Material,
    Texture,
    Sprite,
    Image,
    Retro,
    Colour,
    Drawing,
    Preview,
    Archive,
    Path,
    Medical,
    Science,
}

impl Category {
    pub const ALL: [Category; 16] = [
        Category::Model,
        Category::PointCloud,
        Category::Scene,
        Category::Level,
        Category::Material,
        Category::Texture,
        Category::Sprite,
        Category::Image,
        Category::Retro,
        Category::Colour,
        Category::Drawing,
        Category::Preview,
        Category::Archive,
        Category::Path,
        Category::Medical,
        Category::Science,
    ];

    /// The id used on the command line and in saved choices.
    pub fn id(self) -> &'static str {
        match self {
            Category::Model => "model",
            Category::PointCloud => "point-cloud",
            Category::Scene => "scene",
            Category::Level => "level",
            Category::Material => "material",
            Category::Texture => "texture",
            Category::Sprite => "sprite",
            Category::Image => "image",
            Category::Retro => "retro",
            Category::Colour => "colour",
            Category::Drawing => "drawing",
            Category::Preview => "preview",
            Category::Archive => "archive",
            Category::Path => "path",
            Category::Medical => "medical",
            Category::Science => "science",
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Category::Model => "3D models",
            Category::PointCloud => "Point clouds and splats",
            Category::Scene => "Scenes and projects",
            Category::Level => "Levels and maps",
            Category::Material => "Materials",
            Category::Texture => "Textures",
            Category::Sprite => "Sprites and tile maps",
            Category::Image => "Images",
            Category::Retro => "Retro computer art",
            Category::Colour => "Palettes and colour tables",
            Category::Drawing => "Drawings and CAD",
            Category::Preview => "Documents with stored previews",
            Category::Archive => "Game archives",
            Category::Path => "Toolpaths and GPS tracks",
            Category::Medical => "Medical images",
            Category::Science => "Scientific data",
        }
    }

    pub fn from_id(id: &str) -> Option<Category> {
        Category::ALL.into_iter().find(|c| c.id().eq_ignore_ascii_case(id))
    }
}

/// One MIME type of a format.
#[derive(Debug)]
pub struct Mime {
    pub name: &'static str,
    pub origin: Origin,
    /// File name extensions, lower case without the dot (`"glb"`,
    /// `"nii.gz"`), each the glob `*.<ext>`.
    pub exts: &'static [&'static str],
    /// Other globs, which aren't plain extensions (`*_height.png`).
    pub patterns: &'static [&'static str],
}

/// Where a MIME type is defined.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Origin {
    /// In Slerp's `data/slerp.xml`.
    Own,
    /// An alias `data/slerp.xml` declares for one of its own types.
    Alias,
    /// In shared-mime-info, with globs or magic added by `data/slerp.xml`.
    Extends,
    /// In shared-mime-info (or KDE's or another package's database).
    System,
    /// A name only other databases or newer shared-mime-info releases use,
    /// claimed so Slerp is found whichever name a system gives the file.
    Foreign,
}

/// Whether Slerp makes thumbnails of a format.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Thumbs {
    /// Yes, unless the user turns them off.
    On,
    /// Only when the user turns them on: the desktops already make good
    /// ones (ordinary pictures).
    Off,
    /// Never: there is nothing to draw (VRM animations).
    Never,
}

/// How the Qt image plugin shows a format.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Qt {
    No,
    /// One picture, at up to its own size.
    Still,
    /// A model, level or material turning around, as an animation.
    Turntable,
}

/// A format's macOS type identifiers (UTIs).  macOS knows a file's type by
/// its extension alone.
#[derive(Debug)]
pub enum Mac {
    /// No declaration: macOS gives the extension to something else (CorelDRAW's
    /// `.cdr` is a disk image there), or the format has no extension of its
    /// own.
    None,
    /// macOS declares the type itself.  Slerp only joins "Open With";
    /// Quick Look keeps thumbnails of these for Apple's own thumbnailers.
    System(&'static [&'static str]),
    /// Slerp imports these declarations (it owns none of the formats, so it
    /// exports nothing, and an app that does export one wins).
    Import(&'static [Uti]),
}

/// A type declaration Slerp imports for macOS.
#[derive(Debug)]
pub struct Uti {
    /// Under the format owner's domain, so their app and Slerp agree.
    pub id: &'static str,
    /// Its extensions; empty for all of the format's.
    pub exts: &'static [&'static str],
    /// The types it conforms to; empty for the category's usual ones.
    pub conforms: &'static [&'static str],
    /// The description Finder shows; empty for the format's name.
    pub description: &'static str,
}

/// [`Format::others`]: KDE's own thumbnail plugins (Dolphin) preview it.
pub const KDE: u8 = 1;
/// [`Format::others`]: GNOME's thumbnailers (glycin) preview it.
pub const GNOME: u8 = 2;
/// [`Format::others`]: Windows Explorer previews it without Slerp.
pub const WINDOWS: u8 = 4;
/// [`Format::others`]: macOS Finder previews it without Slerp.
pub const MACOS: u8 = 8;
/// The Linux desktops, which share one desktop entry.
pub const LINUX: u8 = KDE | GNOME;

//endregion

/*┌─────────────────╮
| Lookups         |
└─────────────────╯*/
//region Lookups

impl Format {
    /// Whether the viewer is offered in "Open with" on `desktops` (bits of
    /// [`KDE`], [`GNOME`], [`WINDOWS`] and [`MACOS`]): when none of them
    /// comes with its own viewer for the format.
    pub fn offered_on(&self, desktops: u8) -> bool {
        self.open_with && self.viewers & desktops == 0
    }

    /// The main MIME type.
    pub fn main_mime(&self) -> &'static str {
        self.mime[0].name
    }

    /// Every MIME type's name.
    pub fn mime_names(&self) -> impl Iterator<Item = &'static str> + '_ {
        self.mime.iter().map(|m| m.name)
    }

    /// The MIME types a thumbnailer or desktop entry lists: all but the
    /// aliases, which the MIME database turns into the main type before
    /// anything looks them up.
    pub fn listed_mime(&self) -> impl Iterator<Item = &'static str> + '_ {
        self.mime.iter().filter(|m| m.origin != Origin::Alias).map(|m| m.name)
    }

    /// The extensions claimed by name, in order, without repeats.
    pub fn extensions(&self) -> Vec<&'static str> {
        let mut out: Vec<&'static str> = Vec::new();
        for e in self.mime.iter().flat_map(|m| m.exts.iter()) {
            if !out.contains(e) {
                out.push(e);
            }
        }
        out
    }

    /// Extensions with a single dot-free part, the only kind Windows and
    /// macOS match (`nii.gz` and `ome.tif` would mean every `.gz` and
    /// `.tif`).
    pub fn simple_extensions(&self) -> Vec<&'static str> {
        self.extensions().into_iter().filter(|e| !e.contains('.')).collect()
    }

    /// Whether the user may switch thumbnails on.
    pub fn can_thumbnail(&self) -> bool {
        self.thumbnails != Thumbs::Never
    }

    /// Whether thumbnails are on when the user hasn't chosen.
    pub fn thumbnails_by_default(&self) -> bool {
        self.thumbnails == Thumbs::On
    }

    /// The Qt keys and MIME types, paired.
    pub fn qt_pairs(&self) -> Vec<(&'static str, &'static str)> {
        if self.qt == Qt::No {
            return Vec::new();
        }
        if !self.qt_keys.is_empty() {
            return self.qt_keys.to_vec();
        }
        self.mime.iter().flat_map(|m| m.exts.iter().filter(|e| !e.contains('.')).map(move |e| (*e, m.name))).collect()
    }

    /// The UTIs Slerp imports for it.
    pub fn imported_utis(&self) -> &'static [Uti] {
        match self.mac {
            Mac::Import(list) => list,
            _ => &[],
        }
    }

    /// Every UTI the format goes by on macOS, imported or the system's.
    pub fn utis(&self) -> Vec<&'static str> {
        match self.mac {
            Mac::None => Vec::new(),
            Mac::System(ids) => ids.to_vec(),
            Mac::Import(list) => list.iter().map(|u| u.id).collect(),
        }
    }
}

/// The format with this id.
pub fn by_id(id: &str) -> Option<&'static Format> {
    FORMATS.iter().find(|f| f.id == id)
}

/// The format that claims this MIME type (each type belongs to one).
pub fn by_mime(mime: &str) -> Option<&'static Format> {
    FORMATS.iter().find(|f| f.mime.iter().any(|m| m.name == mime))
}

/// The formats a file name could be, by its extension (longest match, so
/// `brain.nii.gz` is NIfTI, not every `.gz`), and by the extensions
/// confirmed by content.  Several when formats share an extension (`.art`,
/// `.ase`, `.fit`, `.scn`); only content tells those apart.
pub fn by_file_name(name: &str) -> Vec<&'static Format> {
    let lower = name.to_ascii_lowercase();
    let mut best: Vec<&'static Format> = Vec::new();
    let mut best_len = 0;
    for f in FORMATS {
        // Name patterns (heightmaps) count by their fixed letters.
        for p in f.mime.iter().flat_map(|m| m.patterns.iter()) {
            let fixed = p.len() - p.matches('*').count();
            if glob_matches(p, &lower) && fixed >= best_len {
                if fixed > best_len {
                    best_len = fixed;
                    best.clear();
                }
                if !best.iter().any(|b| b.id == f.id) {
                    best.push(f);
                }
            }
        }
        let names = f.mime.iter().flat_map(|m| m.exts.iter()).chain(f.by_content.iter());
        for e in names {
            let matches =
                lower.len() > e.len() + 1 && lower.ends_with(e) && lower.as_bytes()[lower.len() - e.len() - 1] == b'.';
            if !matches {
                continue;
            }
            if e.len() > best_len {
                best_len = e.len();
                best.clear();
            }
            if e.len() == best_len && !best.iter().any(|b| b.id == f.id) {
                best.push(f);
            }
        }
    }
    best
}

/// Whether a lower-case file name matches a shared-mime-info glob, whose
/// only wildcard here is `*`.
pub fn glob_matches(pattern: &str, name: &str) -> bool {
    let parts: Vec<&str> = pattern.split('*').collect();
    if parts.len() == 1 {
        return pattern == name;
    }
    let (first, last) = (parts[0], parts[parts.len() - 1]);
    if name.len() < first.len() + last.len() || !name.starts_with(first) || !name.ends_with(last) {
        return false;
    }
    let mut rest = &name[first.len()..name.len() - last.len()];
    for middle in &parts[1..parts.len() - 1] {
        match rest.find(middle) {
            Some(i) => rest = &rest[i + middle.len()..],
            None => return false,
        }
    }
    true
}

/// The formats in a category, in table order.
pub fn in_category(category: Category) -> impl Iterator<Item = &'static Format> {
    FORMATS.iter().filter(move |f| f.category == category)
}

/// Extensions the viewer passes over when stepping through a folder,
/// because they mostly belong to unrelated files: `.dd` is D's
/// documentation sources far more often than a Commodore 64 Doodle picture.
/// Such files still open when chosen.
const NOT_STEPPED: &[&str] = &["dd"];

/// Every extension the viewer opens when stepping through a folder: the
/// formats' own and those confirmed by content.  Some have two parts
/// (`nii.gz`, `ome.tif`).
pub fn viewer_extensions() -> Vec<&'static str> {
    let mut out: Vec<&'static str> = Vec::new();
    for f in FORMATS {
        for e in f.extensions().into_iter().chain(f.by_content.iter().copied()) {
            if !out.contains(&e) && !NOT_STEPPED.contains(&e) {
                out.push(e);
            }
        }
    }
    out
}

/// Whether the viewer steps to a file of this name when going through its
/// folder (its content is checked when it is opened).
pub fn viewer_steps_to(file_name: &str) -> bool {
    let lower = file_name.to_ascii_lowercase();
    viewer_extensions()
        .iter()
        .any(|e| lower.len() > e.len() + 1 && lower.ends_with(e) && lower.as_bytes()[lower.len() - e.len() - 1] == b'.')
}

/// The MIME types of the formats whose thumbnails are on by default, in
/// table order: the system-wide thumbnailer's list.
pub fn default_thumbnail_types() -> Vec<&'static str> {
    FORMATS.iter().filter(|f| f.thumbnails_by_default()).flat_map(|f| f.listed_mime()).collect()
}

/// The MIME types the viewer is offered for in "Open with" on Linux
/// desktops.
pub fn open_with_types() -> Vec<&'static str> {
    FORMATS.iter().filter(|f| f.offered_on(LINUX)).flat_map(|f| f.listed_mime()).collect()
}

//endregion

#[cfg(test)]
mod tests;
