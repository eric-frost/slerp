//! What opens a MIME type and what thumbnails it on a freedesktop system:
//! the MIME database's aliases and parent types, the installed desktop
//! entries, the `mimeapps.list` files in the order the MIME Applications
//! Associations specification gives, and the `.thumbnailer` files.

use crate::ini::{self, Ini};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use super::Env;

/*┌─────────────────╮
| MIME database   |
└─────────────────╯*/
//region MIME database

/// The parts of the shared-mime-info database the manager needs: which
/// types exist, their aliases and their parents.
#[derive(Default)]
pub struct MimeDb {
    /// Alias to canonical name.
    aliases: HashMap<String, String>,
    /// Type to its parents.
    parents: HashMap<String, Vec<String>>,
    /// Every type with a glob, magic or a parent (from `types`).
    types: std::collections::HashSet<String>,
}

impl MimeDb {
    /// Reads `mime/aliases`, `mime/subclasses` and `mime/types` from every
    /// data folder.
    pub fn load(env: &Env) -> MimeDb {
        let mut db = MimeDb::default();
        for dir in env.data_dirs_all() {
            let mime = dir.join("mime");
            for line in read_lines(&mime.join("aliases")) {
                if let Some((a, c)) = line.split_once(' ') {
                    db.aliases.entry(a.to_owned()).or_insert_with(|| c.to_owned());
                }
            }
            for line in read_lines(&mime.join("subclasses")) {
                if let Some((t, p)) = line.split_once(' ') {
                    let list = db.parents.entry(t.to_owned()).or_default();
                    if !list.iter().any(|x| x == p) {
                        list.push(p.to_owned());
                    }
                }
            }
            for line in read_lines(&mime.join("types")) {
                db.types.insert(line.to_owned());
            }
        }
        db
    }

    pub fn canonical<'a>(&'a self, mime: &'a str) -> &'a str {
        self.aliases.get(mime).map(String::as_str).unwrap_or(mime)
    }

    /// Whether this system knows the type (by itself or as an alias).
    pub fn exists(&self, mime: &str) -> bool {
        self.types.contains(mime) || self.aliases.contains_key(mime)
    }

    /// The type's ancestors, nearest first.  Every type descends from
    /// `application/octet-stream`, and text from `text/plain`, which the
    /// database doesn't list.
    pub fn ancestors(&self, mime: &str) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        let mut queue = vec![self.canonical(mime).to_owned()];
        while let Some(t) = queue.pop() {
            for p in self.parents.get(&t).into_iter().flatten() {
                let p = self.canonical(p).to_owned();
                if !out.contains(&p) && out.len() < 32 {
                    out.push(p.clone());
                    queue.insert(0, p);
                }
            }
        }
        out
    }
}

fn read_lines(path: &Path) -> Vec<String> {
    std::fs::read_to_string(path)
        .map(|t| t.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')).map(str::to_owned).collect())
        .unwrap_or_default()
}

//endregion

/*┌─────────────────╮
| Applications    |
└─────────────────╯*/
//region Applications

/// An installed application's desktop entry.
#[derive(Clone, Debug)]
pub struct App {
    /// The desktop file id (`org.kde.kate.desktop`).
    pub id: String,
    pub name: String,
    pub mime_types: Vec<String>,
    /// KDE's tie-breaker between apps for a type; higher wins.
    pub preference: i32,
}

/// The installed applications, the first entry of each id winning (the
/// user's own folder before the system's).
pub fn apps(env: &Env) -> Vec<App> {
    let mut out: Vec<App> = Vec::new();
    for dir in env.data_dirs_all() {
        let root = dir.join("applications");
        let mut stack = vec![root.clone()];
        while let Some(d) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&d) else { continue };
            for e in entries.flatten() {
                let path = e.path();
                if path.is_dir() {
                    if stack.len() < 64 {
                        stack.push(path);
                    }
                    continue;
                }
                if path.extension().is_none_or(|x| x != "desktop") {
                    continue;
                }
                let Ok(rel) = path.strip_prefix(&root) else { continue };
                let id = rel.to_string_lossy().replace('/', "-");
                if out.iter().any(|a| a.id == id) {
                    continue;
                }
                let Ok(text) = std::fs::read_to_string(&path) else { continue };
                let ini = Ini::parse(&text);
                let s = "Desktop Entry";
                if ini.get(s, "Hidden").as_deref() == Some("true") {
                    // A hidden entry deletes the id for everything after it.
                    out.push(App { id, name: String::new(), mime_types: Vec::new(), preference: i32::MIN });
                    continue;
                }
                out.push(App {
                    id,
                    name: ini.get(s, "Name").unwrap_or_default(),
                    mime_types: ini.get(s, "MimeType").map(|v| ini::list(&v, ';')).unwrap_or_default(),
                    preference: ini.get(s, "InitialPreference").and_then(|v| v.parse().ok()).unwrap_or(1),
                });
            }
        }
    }
    out.retain(|a| a.preference != i32::MIN);
    out
}

/// The `mimeapps.list` files in the specification's order: desktop-specific
/// before general, the user's before the system's, config before the
/// deprecated data locations.
pub fn mimeapps_files(env: &Env) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let names: Vec<String> =
        env.desktops.iter().map(|d| format!("{d}-mimeapps.list")).chain(["mimeapps.list".to_owned()]).collect();
    let config: Vec<PathBuf> =
        std::iter::once(env.config_home.clone()).chain(env.config_dirs.iter().cloned()).collect();
    for dir in &config {
        for n in &names {
            out.push(dir.join(n));
        }
    }
    for dir in env.data_dirs_all() {
        for n in &names {
            out.push(dir.join("applications").join(n));
        }
    }
    out
}

/// Who opens a type by default, and why.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Handler {
    pub app: String,
    pub name: String,
    /// The `mimeapps.list` that names it, or `None` when the desktop picks
    /// it among the apps that list the type.
    pub file: Option<PathBuf>,
    /// The parent type it opens the file as, when no app takes the type
    /// itself (`application/xml` for MaterialX).
    pub as_parent: Option<String>,
}

/// The default app for `mime`, as freedesktop desktops resolve it (KDE
/// breaks ties by `InitialPreference`, which this follows; GNOME by the
/// order of its caches, which can differ).
pub fn default_app(env: &Env, db: &MimeDb, apps: &[App], mime: &str) -> Option<Handler> {
    let mime = db.canonical(mime).to_owned();
    let lists: Vec<(PathBuf, Ini)> = mimeapps_files(env)
        .into_iter()
        .filter_map(|p| std::fs::read_to_string(&p).ok().map(|t| (p, Ini::parse(&t))))
        .collect();
    for t in std::iter::once(mime.clone()).chain(db.ancestors(&mime)) {
        if t == "application/octet-stream" {
            break;
        }
        let as_parent = (t != mime).then(|| t.clone());
        // 1. A default some mimeapps.list names, if it's installed.
        for (path, ini) in &lists {
            for id in ini.get("Default Applications", &t).map(|v| ini::list(&v, ';')).unwrap_or_default() {
                if let Some(app) = apps.iter().find(|a| a.id == id) {
                    return Some(Handler {
                        app: id,
                        name: app.name.clone(),
                        file: Some(path.clone()),
                        as_parent: as_parent.clone(),
                    });
                }
            }
        }
        // 2. Otherwise the apps associated with it: added associations
        //    first, then those listing it, minus removed ones.
        let mut removed: Vec<String> = Vec::new();
        let mut candidates: Vec<&App> = Vec::new();
        for (_, ini) in &lists {
            for id in ini.get("Added Associations", &t).map(|v| ini::list(&v, ';')).unwrap_or_default() {
                if let Some(a) = apps.iter().find(|a| a.id == id)
                    && !removed.contains(&id)
                    && !candidates.iter().any(|c| c.id == id)
                {
                    candidates.push(a);
                }
            }
            removed.extend(ini.get("Removed Associations", &t).map(|v| ini::list(&v, ';')).unwrap_or_default());
        }
        let mut listing: Vec<&App> = apps
            .iter()
            .filter(|a| a.mime_types.iter().any(|m| db.canonical(m) == t) && !removed.contains(&a.id))
            .filter(|a| !candidates.iter().any(|c| c.id == a.id))
            .collect();
        listing.sort_by(|a, b| b.preference.cmp(&a.preference).then(a.id.cmp(&b.id)));
        candidates.extend(listing);
        if let Some(a) = candidates.first() {
            return Some(Handler { app: a.id.clone(), name: a.name.clone(), file: None, as_parent });
        }
    }
    None
}

//endregion

/*┌─────────────────╮
| Thumbnailers    |
└─────────────────╯*/
//region Thumbnailers

/// A freedesktop `.thumbnailer` file.
#[derive(Clone, Debug)]
pub struct Thumbnailer {
    pub path: PathBuf,
    /// The file name without `.thumbnailer`, which KIO calls its plugin id.
    pub id: String,
    pub try_exec: Option<String>,
    pub exec: String,
    pub mime_types: Vec<String>,
}

/// Every `.thumbnailer` file, the user's folder first, then each data
/// folder, each folder's files in name order.
pub fn thumbnailers(env: &Env) -> Vec<Thumbnailer> {
    let mut out = Vec::new();
    for dir in env.data_dirs_all() {
        let Ok(entries) = std::fs::read_dir(dir.join("thumbnailers")) else { continue };
        let mut paths: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
        paths.sort();
        for path in paths.into_iter().filter(|p| p.extension().is_some_and(|e| e == "thumbnailer")) {
            let Ok(text) = std::fs::read_to_string(&path) else { continue };
            let ini = Ini::parse(&text);
            let s = "Thumbnailer Entry";
            let Some(exec) = ini.get(s, "Exec") else { continue };
            out.push(Thumbnailer {
                id: path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default(),
                try_exec: ini.get(s, "TryExec"),
                exec,
                mime_types: ini.get(s, "MimeType").map(|v| ini::list(&v, ';')).unwrap_or_default(),
                path,
            });
        }
    }
    out
}

/// The thumbnailers KDE, Thunar and PCManFM-Qt use: one per file name,
/// the first folder's (so a file in the user's folder hides the system's
/// of the same name).
pub fn by_name_first(all: &[Thumbnailer]) -> Vec<&Thumbnailer> {
    let mut out: Vec<&Thumbnailer> = Vec::new();
    for t in all {
        if !out.iter().any(|o| o.id == t.id) {
            out.push(t);
        }
    }
    out
}

/// The thumbnailer GNOME's thumbnail factory (Nautilus, and its forks in
/// Nemo and Caja) uses for `mime`: every file counts, with no hiding by
/// name, and the first to claim a type keeps it, the user's folder first.
pub fn gnome_choice<'a>(all: &'a [Thumbnailer], mime: &str) -> Option<&'a Thumbnailer> {
    all.iter().filter(|t| t.try_exec.as_deref().is_none_or(on_path)).find(|t| t.mime_types.iter().any(|m| m == mime))
}

/// Whether `program` (a name or a path) can be run, as `TryExec` asks.
pub fn on_path(program: &str) -> bool {
    if program.contains('/') {
        return Path::new(program).is_file();
    }
    std::env::var_os("PATH").is_some_and(|p| std::env::split_paths(&p).any(|d| d.join(program).is_file()))
}

//endregion
