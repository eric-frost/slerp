//! The user's choices of which file types Slerp makes thumbnails for and
//! opens by default, kept in `file-types.jsonc` beside `config.jsonc` (so
//! they are backed up and synced with the rest of the settings).  The file
//! lists only where the user differs from the format table's defaults
//! (`slerp_formats`), so formats a later version adds start with their
//! defaults.  `slerp associate` and the viewer's File Types page change
//! it; the association manager (`slerp_associate`) applies it to the
//! desktop, and the macOS and Windows thumbnail handlers read it to decline
//! the formats switched off.

use serde::{Deserialize, Serialize};
use slerp_formats::{FORMATS, Format};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::SystemTime;

/// Per-format choices, by format id.  Ids no format has (from a newer or
/// older version) are kept, so saving doesn't drop them, but ignored.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Choices {
    /// Thumbnails switched on or off where that differs from the default.
    pub thumbnails: BTreeMap<String, bool>,
    /// Formats Slerp should be the default app for.  Off by default for
    /// every format: Slerp doesn't take over a type unless asked.
    pub open_by_default: BTreeMap<String, bool>,
    /// Formats for which Dolphin should stop using KDE's own thumbnail
    /// plugin, which otherwise wins over Slerp (KDE's DDS previews, say).
    /// Switching that plugin off affects every type it previews.
    pub replace_kde_previews: BTreeMap<String, bool>,
}

/// The header written above the choices.
const HEADER: &str = "// Which file types Slerp makes thumbnails for and opens by default.  Changed by
// `slerp associate` and the viewer's Settings > File Types; `slerp associate
// --apply` makes the desktop follow it.  Only differences from Slerp's
// defaults are listed, by format id (`slerp associate --list` shows them).
";

impl Choices {
    /// `file-types.jsonc` in the settings folder.
    pub fn path() -> Option<PathBuf> {
        Some(crate::config::config_dir()?.join("file-types.jsonc"))
    }

    /// The user's choices, or the defaults when there are none or the file
    /// can't be read (a thumbnail handler must never fail over a typo).
    pub fn load() -> Choices {
        Choices::path().map(|p| Choices::load_from(&p)).unwrap_or_default()
    }

    pub fn load_from(path: &Path) -> Choices {
        std::fs::read_to_string(path).ok().and_then(|t| Choices::parse(&t).ok()).unwrap_or_default()
    }

    pub fn parse(text: &str) -> Result<Choices, serde_json::Error> {
        serde_json::from_str(&crate::config::strip_jsonc(text))
    }

    /// The file's text: a short explanation, then the choices.
    pub fn to_text(&self) -> String {
        let json = serde_json::to_string_pretty(self).unwrap_or_else(|_| "{}".into());
        format!("{HEADER}{json}\n")
    }

    pub fn save(&self) -> std::io::Result<()> {
        let path = Choices::path().ok_or_else(|| std::io::Error::other("no settings folder"))?;
        self.save_to(&path)
    }

    pub fn save_to(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        crate::write_atomic(path, self.to_text().as_bytes())
    }

    /// Whether Slerp makes thumbnails of `format`.
    pub fn thumbnails(&self, format: &Format) -> bool {
        format.can_thumbnail()
            && self.thumbnails.get(format.id).copied().unwrap_or_else(|| format.thumbnails_by_default())
    }

    /// Whether Slerp should be the default app for `format`.
    pub fn open_by_default(&self, format: &Format) -> bool {
        self.open_by_default.get(format.id).copied().unwrap_or(false)
    }

    /// Whether KDE's competing thumbnail plugin should be switched off in
    /// Dolphin for `format`.  Only meaningful while Slerp makes its
    /// thumbnails.
    pub fn replace_kde_previews(&self, format: &Format) -> bool {
        self.thumbnails(format) && self.replace_kde_previews.get(format.id).copied().unwrap_or(false)
    }

    /// Switches thumbnails on or off, remembering only a difference from
    /// the default.  Formats with nothing to draw stay off.
    pub fn set_thumbnails(&mut self, format: &Format, on: bool) {
        let on = on && format.can_thumbnail();
        if on == format.thumbnails_by_default() {
            self.thumbnails.remove(format.id);
        } else {
            self.thumbnails.insert(format.id.to_owned(), on);
        }
    }

    pub fn set_open_by_default(&mut self, format: &Format, on: bool) {
        if on {
            self.open_by_default.insert(format.id.to_owned(), true);
        } else {
            self.open_by_default.remove(format.id);
        }
    }

    pub fn set_replace_kde_previews(&mut self, format: &Format, on: bool) {
        if on {
            self.replace_kde_previews.insert(format.id.to_owned(), true);
        } else {
            self.replace_kde_previews.remove(format.id);
        }
    }

    /// The MIME types whose thumbnails are on, in table order.
    pub fn thumbnail_types(&self) -> Vec<&'static str> {
        FORMATS.iter().filter(|f| self.thumbnails(f)).flat_map(|f| f.listed_mime()).collect()
    }

    /// The formats whose thumbnails are on.
    pub fn thumbnail_formats(&self) -> impl Iterator<Item = &'static Format> + '_ {
        FORMATS.iter().filter(|f| self.thumbnails(f))
    }
}

/*┌─────────────────╮
| Thumbnail gate  |
└─────────────────╯*/
//region Thumbnail gate

/// Whether the user wants Slerp's thumbnails for a file of `kind` at
/// `path`, reading the choices at `choices` (`Choices::path()` when
/// `None`).  For thumbnail handlers that see every file of their
/// registered types (Quick Look and Explorer, which can't be told per
/// format); on Linux the thumbnailer's MIME list does this.  The file is
/// read again only when it changes, so asking costs a `stat`.
pub fn thumbnails_wanted(kind: crate::Kind, path: &Path, choices: Option<&Path>) -> bool {
    let Some(format) = kind.format(path) else { return true };
    let Some(file) = choices.map(Path::to_path_buf).or_else(Choices::path) else {
        return format.thumbnails_by_default();
    };
    static CACHE: Mutex<Option<(PathBuf, Option<SystemTime>, Choices)>> = Mutex::new(None);
    let modified = std::fs::metadata(&file).and_then(|m| m.modified()).ok();
    let mut cache = CACHE.lock().unwrap_or_else(|p| p.into_inner());
    let fresh = matches!(&*cache, Some((p, m, _)) if *p == file && *m == modified);
    if !fresh {
        *cache = Some((file.clone(), modified, Choices::load_from(&file)));
    }
    cache.as_ref().is_some_and(|(_, _, c)| c.thumbnails(format))
}

/// [`thumbnails_wanted`] for the file at `path`, sniffing it first.  Files
/// Slerp doesn't recognise count as wanted: the caller finds out itself.
pub fn thumbnails_wanted_for(path: &Path, choices: Option<&Path>) -> bool {
    match crate::sniff_file(path) {
        Ok(Some(kind)) => thumbnails_wanted(kind, path, choices),
        _ => true,
    }
}

//endregion

#[cfg(test)]
mod tests {
    use super::*;

    fn f(id: &str) -> &'static Format {
        slerp_formats::by_id(id).unwrap()
    }

    #[test]
    fn defaults_come_from_the_table_and_only_differences_are_kept() {
        let mut c = Choices::default();
        assert!(c.thumbnails(f("glb")) && !c.thumbnails(f("png")) && !c.thumbnails(f("vrma")));
        assert!(!c.open_by_default(f("glb")));
        c.set_thumbnails(f("glb"), true);
        c.set_thumbnails(f("vrma"), true);
        assert!(c.thumbnails.is_empty(), "defaults and impossible choices aren't stored");
        c.set_thumbnails(f("glb"), false);
        c.set_thumbnails(f("gif"), true);
        assert_eq!(c.thumbnails.len(), 2);
        assert!(!c.thumbnail_types().contains(&"model/gltf-binary") && c.thumbnail_types().contains(&"image/gif"));
        c.set_thumbnails(f("glb"), true);
        assert_eq!(c.thumbnails.len(), 1);
    }

    #[test]
    fn thumbnail_handlers_decline_formats_switched_off() {
        let dir = std::env::temp_dir().join(format!("slerp-choices-gate-{}", std::process::id()));
        let path = dir.join("file-types.jsonc");
        let mut c = Choices::default();
        c.set_thumbnails(f("glb"), false);
        c.save_to(&path).unwrap();
        let glb = crate::Kind::Gltf;
        let ktx2 = crate::Kind::Texture(slerp_textures::Format::Ktx2);
        assert!(!thumbnails_wanted(glb, Path::new("a.glb"), Some(&path)));
        assert!(thumbnails_wanted(ktx2, Path::new("a.ktx2"), Some(&path)));
        // A VRM is a glTF file too, with a format of its own.
        assert!(thumbnails_wanted(glb, Path::new("a.vrm"), Some(&path)));
        // Changed on disk: read again.
        std::thread::sleep(std::time::Duration::from_millis(20));
        Choices::default().save_to(&path).unwrap();
        let _ =
            std::fs::OpenOptions::new().append(true).open(&path).map(|f| f.set_modified(std::time::SystemTime::now()));
        assert!(thumbnails_wanted(glb, Path::new("a.glb"), Some(&path)));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn saves_and_loads_with_comments() {
        let dir = std::env::temp_dir().join(format!("slerp-choices-{}", std::process::id()));
        let path = dir.join("file-types.jsonc");
        let mut c = Choices::default();
        c.set_thumbnails(f("dds"), false);
        c.set_open_by_default(f("glb"), true);
        c.set_replace_kde_previews(f("blend"), true);
        c.thumbnails.insert("from-a-newer-version".into(), true);
        c.save_to(&path).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.starts_with("//") && text.contains("\"openByDefault\""));
        assert_eq!(Choices::load_from(&path), c);
        assert!(c.replace_kde_previews(f("blend")) && !c.replace_kde_previews(f("dds")));
        // A broken file reads as the defaults.
        std::fs::write(&path, "{ \"thumbnails\": ").unwrap();
        assert_eq!(Choices::load_from(&path), Choices::default());
        let _ = std::fs::remove_dir_all(dir);
    }
}
