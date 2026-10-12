//! The record of what the manager changed outside Slerp's own settings, so
//! `--reset` puts back exactly what was there: the text of files it edited
//! (`mimeapps.list`, `dolphinrc`) before its first change, the entries it
//! changed in them, the files it created and what they replaced, and on
//! Windows and macOS the registry values and default handlers it replaced.
//!
//! The record describes this machine, so it lives in the state folder
//! (`~/.local/state/slerp/`), not with the synced settings.

use crate::ini::{self, Ini};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Record {
    pub version: u32,
    /// Files belonging to other programs that the manager edited, by path.
    pub edited: BTreeMap<String, FileRecord>,
    /// Files the manager wrote whole (thumbnailer files, a desktop entry),
    /// by path, with what was there before (`None`: nothing).
    pub created: BTreeMap<String, Option<String>>,
    /// Folders the manager created, removed again on reset when empty.
    pub created_dirs: Vec<String>,
    /// The MIME types Slerp made thumbnails for when last applied, to know
    /// whose cached thumbnails to clear when they change.
    pub thumbnail_types: Vec<String>,
    /// Linux: someone took Slerp out of Dolphin's preview plugins after the
    /// manager put it there, so it stays out until the user changes which
    /// formats get Slerp's thumbnails.
    pub dolphin_declined: bool,
    /// Windows: registry values the manager replaced (a key path and value
    /// name joined by `|`), with their data before (`None`: absent).
    pub registry: BTreeMap<String, Option<String>>,
    /// macOS: the default app for each type identifier before Slerp was
    /// made its default.
    pub mac_defaults: BTreeMap<String, Option<String>>,
    /// macOS: whether the manager switched the Quick Look extension off
    /// (because every format's thumbnails were off).
    pub mac_extension_ignored: bool,
}

impl Record {
    pub const VERSION: u32 = 1;

    pub fn load(path: &Path) -> Record {
        std::fs::read_to_string(path).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
    }

    pub fn is_empty(&self) -> bool {
        self.edited.is_empty()
            && self.created.is_empty()
            && self.created_dirs.is_empty()
            && self.registry.is_empty()
            && self.mac_defaults.is_empty()
            && !self.mac_extension_ignored
            && !self.dolphin_declined
    }

    /// Saves the record, or removes its file when nothing is left to undo.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if self.is_empty() && self.thumbnail_types.is_empty() {
            return match std::fs::remove_file(path) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
                _ => Ok(()),
            };
        }
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let mut copy = self.clone();
        copy.version = Record::VERSION;
        let text = serde_json::to_string_pretty(&copy).map_err(std::io::Error::other)?;
        slerp_core::write_atomic(path, text.as_bytes())
    }
}

/// An edited file.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct FileRecord {
    /// The whole file before the manager's first change (`None`: there was
    /// no file).
    pub original: Option<String>,
    /// The text the manager wrote last.  When the file still holds it,
    /// reset writes `original` back byte for byte; when someone has edited
    /// the file since, reset undoes the manager's entries one by one.
    pub written: String,
    pub entries: Vec<EntryRecord>,
}

/// One entry the manager changed.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct EntryRecord {
    pub section: String,
    pub key: String,
    /// The value before (`None`: the key wasn't there).
    pub before: Option<String>,
    /// The value the manager left (`None`: it removed the key).
    pub after: Option<String>,
    /// For list values: the items added and removed, undone item by item
    /// so other programs' changes to the same list survive.
    pub added: Vec<String>,
    pub removed: Vec<String>,
    /// The list separator (`;` or `,`) and whether lists end with it, for
    /// list entries.
    pub sep: Option<char>,
    pub trailing: bool,
    /// Whether the manager created the section for it.
    pub created_section: bool,
}

/// A change the manager wants in a file another program owns.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Edit {
    /// Sets a value outright.
    Value { section: String, key: String, value: String },
    /// Adds and removes items of a list value, leaving the rest.  When the
    /// key is missing, `default` is the list it stands for (KDE's default
    /// preview plugins), or nothing.
    List {
        section: String,
        key: String,
        sep: char,
        trailing: bool,
        add: Vec<String>,
        remove: Vec<String>,
        default: Option<Vec<String>>,
    },
}

/// What applying edits to a file came to.
#[derive(Debug, Default)]
pub struct FileOutcome {
    /// The text to leave in the file (`None`: no file).
    pub text: Option<String>,
    /// The record to keep for it (`None`: nothing of the manager's is left).
    pub record: Option<FileRecord>,
    /// Entries of the manager's that someone changed since, so the manager
    /// gives them up: `(section, key, their value)`.
    pub overridden: Vec<(String, String, Option<String>)>,
}

/// Works out the text of a file holding `current` (`None`: no file) when
/// the manager wants exactly `edits` in it, given what it did before
/// (`record`).  The edits are made against the file as it would be without
/// the manager, so dropping an edit restores what was there.
pub fn plan_file(current: Option<&str>, edits: &[Edit], record: Option<&FileRecord>) -> FileOutcome {
    // 1. The file without the manager's changes.
    let mut overridden = Vec::new();
    let base: Option<String> = match record {
        Some(r) if current == Some(r.written.as_str()) => r.original.clone(),
        Some(r) => {
            let (text, lost) = revert(current.unwrap_or(""), &r.entries);
            overridden = lost;
            // A file the manager created and nobody else touched is gone
            // once its entries are; one someone deleted stays deleted.
            if r.original.is_none() && Ini::parse(&text).keys_count() == 0 { None } else { current.map(|_| text) }
        }
        None => current.map(str::to_owned),
    };
    // Entries someone else changed are theirs now.
    let edits: Vec<&Edit> = edits
        .iter()
        .filter(|e| {
            let (s, k) = e.target();
            !overridden.iter().any(|(os, ok, _)| os == s && ok == k)
        })
        .collect();

    // 2. The manager's edits on top.
    let mut ini = Ini::parse(base.as_deref().unwrap_or(""));
    let mut entries = Vec::new();
    for edit in edits {
        if let Some(entry) = apply(&mut ini, edit) {
            entries.push(entry);
        }
    }
    if entries.is_empty() {
        return FileOutcome { text: base, record: None, overridden };
    }
    let text = ini.to_text();
    FileOutcome {
        record: Some(FileRecord { original: base, written: text.clone(), entries }),
        text: Some(text),
        overridden,
    }
}

/// The text of a file the manager edited, with its changes undone: exactly
/// the original when nobody else has touched it.
pub fn reset_file(current: Option<&str>, record: &FileRecord) -> Option<String> {
    plan_file(current, &[], Some(record)).text
}

impl Edit {
    fn target(&self) -> (&str, &str) {
        match self {
            Edit::Value { section, key, .. } | Edit::List { section, key, .. } => (section, key),
        }
    }
}

impl Ini {
    fn keys_count(&self) -> usize {
        self.sections().iter().map(|s| self.keys(s).len()).sum()
    }
}

/// Makes one edit, returning what it changed (nothing when the file
/// already says so).
fn apply(ini: &mut Ini, edit: &Edit) -> Option<EntryRecord> {
    let (section, key) = edit.target();
    let created_section = !ini.has_section(section);
    let before = ini.get(section, key);
    let mut entry = EntryRecord {
        section: section.to_owned(),
        key: key.to_owned(),
        before: before.clone(),
        created_section,
        ..Default::default()
    };
    match edit {
        Edit::Value { value, .. } => {
            if before.as_deref() == Some(value.as_str()) {
                return None;
            }
            ini.set(section, key, value);
            entry.after = Some(value.clone());
        }
        Edit::List { sep, trailing, add, remove, default, .. } => {
            let mut items = match (&before, default) {
                (Some(v), _) => ini::list(v, *sep),
                (None, Some(d)) => d.clone(),
                // No list, and nothing it stands for: removing is moot.
                (None, None) if add.is_empty() => return None,
                (None, None) => Vec::new(),
            };
            for a in add {
                if !items.contains(a) {
                    items.push(a.clone());
                    entry.added.push(a.clone());
                }
            }
            for r in remove {
                if let Some(i) = items.iter().position(|x| x == r) {
                    items.remove(i);
                    entry.removed.push(r.clone());
                }
            }
            if entry.added.is_empty() && entry.removed.is_empty() {
                return None;
            }
            let value = ini::join(&items, *sep, *trailing);
            ini.set(section, key, &value);
            entry.after = Some(value);
            entry.sep = Some(*sep);
            entry.trailing = *trailing;
        }
    }
    Some(entry)
}

/// Undoes recorded entries in `text`, newest first.  Returns the text and
/// the entries someone else has changed since, which are left as they are.
fn revert(text: &str, entries: &[EntryRecord]) -> (String, Vec<(String, String, Option<String>)>) {
    let mut ini = Ini::parse(text);
    let mut lost = Vec::new();
    for e in entries.iter().rev() {
        let now = ini.get(&e.section, &e.key);
        match e.sep {
            Some(sep) => {
                // A list: take out what was added, put back what was taken
                // out, unless someone already did.
                let Some(now) = now else {
                    lost.push((e.section.clone(), e.key.clone(), None));
                    continue;
                };
                let mut items = ini::list(&now, sep);
                let mut changed = false;
                for a in &e.added {
                    if let Some(i) = items.iter().position(|x| x == a) {
                        items.remove(i);
                        changed = true;
                    }
                }
                for r in &e.removed {
                    if !items.contains(r) {
                        items.push(r.clone());
                        changed = true;
                    }
                }
                if !changed {
                    lost.push((e.section.clone(), e.key.clone(), Some(now)));
                    continue;
                }
                let restored = ini::join(&items, sep, e.trailing);
                match &e.before {
                    // A list the manager made, now with only others' items.
                    None if items.is_empty() => {
                        ini.remove(&e.section, &e.key);
                    }
                    Some(before) if ini::list(before, sep) == items => ini.set(&e.section, &e.key, before),
                    _ => ini.set(&e.section, &e.key, &restored),
                }
            }
            None => {
                if now != e.after {
                    lost.push((e.section.clone(), e.key.clone(), now));
                    continue;
                }
                match &e.before {
                    Some(v) => ini.set(&e.section, &e.key, v),
                    None => {
                        ini.remove(&e.section, &e.key);
                    }
                }
            }
        }
        if e.created_section {
            ini.remove_section_if_empty(&e.section);
        }
    }
    (ini.to_text(), lost)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIMEAPPS: &str = "[Added Associations]\ntext/csv=org.kde.kate.desktop;\n\n[Default Applications]\nmodel/gltf-binary=f3d.desktop;\n";

    fn set(t: &str, v: &str) -> Edit {
        Edit::Value { section: "Default Applications".into(), key: t.into(), value: v.into() }
    }

    #[test]
    fn edits_and_undoing_them_restore_the_file_exactly() {
        let edits = [set("model/gltf-binary", "slerp.desktop;"), set("model/x-vrm", "slerp.desktop;")];
        let out = plan_file(Some(MIMEAPPS), &edits, None);
        let text = out.text.unwrap();
        assert!(text.contains("model/gltf-binary=slerp.desktop;\nmodel/x-vrm=slerp.desktop;\n"));
        let record = out.record.unwrap();
        assert_eq!(record.original.as_deref(), Some(MIMEAPPS));
        // Unchanged since: the original comes back byte for byte.
        assert_eq!(reset_file(Some(&text), &record).as_deref(), Some(MIMEAPPS));
        // Applying again is a no-op; dropping one edit restores its entry.
        let again = plan_file(Some(&text), &edits, Some(&record));
        assert_eq!(again.text.as_deref(), Some(text.as_str()));
        let fewer = plan_file(Some(&text), &edits[1..], Some(&record));
        assert!(fewer.text.unwrap().contains("model/gltf-binary=f3d.desktop;"));
    }

    #[test]
    fn others_changes_survive_and_win() {
        let edits = [set("model/gltf-binary", "slerp.desktop;"), set("model/x-vrm", "slerp.desktop;")];
        let out = plan_file(Some(MIMEAPPS), &edits, None);
        let record = out.record.unwrap();
        // Someone adds an unrelated default and picks another app for VRM.
        let mut ini = Ini::parse(out.text.as_deref().unwrap());
        ini.set("Default Applications", "text/plain", "kate.desktop;");
        ini.set("Default Applications", "model/x-vrm", "vroid.desktop;");
        let edited = ini.to_text();
        let reset = reset_file(Some(&edited), &record).unwrap();
        assert!(reset.contains("model/gltf-binary=f3d.desktop;") && reset.contains("text/plain=kate.desktop;"));
        assert!(reset.contains("model/x-vrm=vroid.desktop;"));
        // Applying again leaves their VRM choice and reports it.
        let again = plan_file(Some(&edited), &edits, Some(&record));
        assert_eq!(again.overridden.len(), 1);
        assert!(again.text.unwrap().contains("model/x-vrm=vroid.desktop;"));
    }

    #[test]
    fn lists_and_new_files() {
        let dolphin = "[PreviewSettings]\nPlugins=imagethumbnail,jpegthumbnail\n";
        let add = Edit::List {
            section: "PreviewSettings".into(),
            key: "Plugins".into(),
            sep: ',',
            trailing: false,
            add: vec!["slerp".into()],
            remove: vec!["imagethumbnail".into()],
            default: None,
        };
        let out = plan_file(Some(dolphin), std::slice::from_ref(&add), None);
        assert_eq!(out.text.as_deref(), Some("[PreviewSettings]\nPlugins=jpegthumbnail,slerp\n"));
        // Someone enables another plugin meanwhile; reset keeps it.
        let edited = "[PreviewSettings]\nPlugins=jpegthumbnail,slerp,fontthumbnail\n";
        let reset = reset_file(Some(edited), out.record.as_ref().unwrap()).unwrap();
        assert_eq!(reset, "[PreviewSettings]\nPlugins=jpegthumbnail,fontthumbnail,imagethumbnail\n");
        // A file that didn't exist is removed again.
        let created = plan_file(None, &[set("a/b", "slerp.desktop;")], None);
        assert_eq!(created.record.as_ref().unwrap().original, None);
        assert_eq!(reset_file(created.text.as_deref(), created.record.as_ref().unwrap()), None);
    }
}
