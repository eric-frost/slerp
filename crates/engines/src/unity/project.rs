//! A Unity project around a scene: its root folder, and the index from
//! asset GUIDs to files that every reference in a scene goes through.
//!
//! Unity names each asset by the GUID in the `.meta` file beside it, so
//! resolving a reference means knowing every `.meta` file of the project.
//! Scanning a large project takes a while, so the index is kept in
//! `$XDG_CACHE_HOME/slerp/unity/` (one file per project) and rescanned only
//! when a GUID a scene asks for is missing from it or points at a file that
//! is gone, and the last scan is over a minute old: scenes often name assets
//! the project no longer has, and a folder of such scenes should pay for one
//! scan, not one each.

use super::xxh64::xxh64;
use super::yaml::{self, Yaml};
use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};

/// The most `.meta` files a scan reads; more is not a project but a disk.
const MAX_META_FILES: usize = 2_000_000;
/// Seconds a scan stays fresh enough not to repeat for a missing GUID.
const RESCAN_AFTER: u64 = 60;

/// The most directory entries a scan looks at, which bounds the time a
/// scene lying in a large folder outside any project costs.
const MAX_ENTRIES: usize = 4_000_000;

pub(crate) struct Project {
    /// The folder holding `Assets/` (and `ProjectSettings/`), or the scene's
    /// own folder outside a project.
    pub root: PathBuf,
    pub in_project: bool,
    index: HashMap<String, PathBuf>,
    scanned: bool,
    cache_loaded: bool,
    /// When the cached index was made, in seconds since 1970.
    cache_time: u64,
    metas: HashMap<PathBuf, Option<std::rc::Rc<Yaml>>>,
}

impl Project {
    /// The project containing `path`.
    pub fn find(path: &Path) -> Project {
        let start = path.parent().unwrap_or(Path::new(".")).to_path_buf();
        let mut dir = start.clone();
        let mut found = None;
        for _ in 0..64 {
            if dir.join("Assets").is_dir() && dir.join("ProjectSettings").is_dir() {
                found = Some(dir.clone());
                break;
            }
            if dir.file_name().is_some_and(|n| n == "Assets") {
                found = dir.parent().map(Path::to_path_buf);
                break;
            }
            if !dir.pop() {
                break;
            }
        }
        let in_project = found.is_some();
        Project {
            root: found.unwrap_or(start),
            in_project,
            index: HashMap::new(),
            scanned: false,
            cache_loaded: false,
            cache_time: 0,
            metas: HashMap::new(),
        }
    }

    fn cache_file(&self) -> Option<PathBuf> {
        let base = std::env::var_os("XDG_CACHE_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))?;
        let key = xxh64(self.root.to_string_lossy().as_bytes());
        Some(base.join("slerp").join("unity").join(format!("{key:016x}.tsv")))
    }

    /// The file whose `.meta` holds `guid`.
    pub fn asset(&mut self, guid: &str) -> Option<PathBuf> {
        if guid.len() != 32 || guid.bytes().all(|b| b == b'0') || !guid.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        if !self.cache_loaded {
            self.cache_loaded = true;
            self.load_cache();
        }
        if let Some(p) = self.index.get(guid).filter(|p| p.exists()) {
            return Some(p.clone());
        }
        if !self.scanned && now().saturating_sub(self.cache_time) > RESCAN_AFTER {
            self.scan();
            return self.index.get(guid).cloned();
        }
        None
    }

    fn load_cache(&mut self) {
        let Some(file) = self.cache_file() else { return };
        let Ok(text) = std::fs::read_to_string(&file) else { return };
        for line in text.lines().take(MAX_META_FILES) {
            if let Some(t) = line.strip_prefix("#scanned ") {
                self.cache_time = t.trim().parse().unwrap_or(0);
            } else if let Some((guid, rel)) = line.split_once('\t') {
                self.index.insert(guid.to_string(), self.root.join(rel));
            }
        }
    }

    /// Reads every `.meta` file under `Assets/` and `Packages/` (or the
    /// scene's folder outside a project) and saves the index.
    fn scan(&mut self) {
        self.scanned = true;
        let tops: Vec<PathBuf> = if self.in_project {
            vec![self.root.join("Assets"), self.root.join("Packages")]
        } else {
            vec![self.root.clone()]
        };
        // Outside a project, only a few levels below the scene: its
        // folder could be someone's whole downloads.
        let max_depth = if self.in_project { 64 } else { 3 };
        let mut metas = Vec::new();
        let mut seen = 0usize;
        for top in tops {
            walk(&top, &mut metas, 0, max_depth, &mut seen);
        }
        use rayon::prelude::*;
        let found: Vec<(String, PathBuf)> = metas
            .par_iter()
            .filter_map(|meta| {
                let guid = read_guid(meta)?;
                let asset = meta.with_extension("");
                Some((guid, asset))
            })
            .collect();
        self.index = found.into_iter().collect();
        // The index is a cache: failing to save it costs only time later.
        if let Some(file) = self.cache_file() {
            let mut text = format!("#scanned {}\n", now());
            for (guid, path) in &self.index {
                if let Ok(rel) = path.strip_prefix(&self.root) {
                    text.push_str(guid);
                    text.push('\t');
                    text.push_str(&rel.to_string_lossy());
                    text.push('\n');
                }
            }
            if let Some(dir) = file.parent()
                && std::fs::create_dir_all(dir).is_ok()
            {
                let tmp = file.with_extension(format!("{}.tmp", std::process::id()));
                if std::fs::write(&tmp, text).is_ok() {
                    let _ = std::fs::rename(&tmp, &file);
                }
            }
        }
    }

    /// The parsed `.meta` file of an asset, once.
    pub fn meta(&mut self, asset: &Path) -> Option<std::rc::Rc<Yaml>> {
        if let Some(m) = self.metas.get(asset) {
            return m.clone();
        }
        let mut name = asset.as_os_str().to_os_string();
        name.push(".meta");
        let parsed = std::fs::read(PathBuf::from(name))
            .ok()
            .filter(|b| b.len() < 64 << 20)
            .and_then(|b| yaml::parse(&String::from_utf8_lossy(&b)).ok())
            .map(std::rc::Rc::new);
        self.metas.insert(asset.to_path_buf(), parsed.clone());
        parsed
    }
}

fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// Collects `.meta` files below `dir`, skipping what Unity ignores (hidden
/// folders and those ending in `~`).
fn walk(dir: &Path, out: &mut Vec<PathBuf>, depth: usize, max_depth: usize, seen: &mut usize) {
    if depth > max_depth || out.len() >= MAX_META_FILES || *seen >= MAX_ENTRIES {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for e in entries.flatten() {
        *seen += 1;
        if *seen >= MAX_ENTRIES {
            return;
        }
        let name = e.file_name();
        let name = name.to_string_lossy();
        if name.starts_with('.') {
            continue;
        }
        let Ok(kind) = e.file_type() else { continue };
        if kind.is_dir() {
            if !name.ends_with('~') {
                walk(&e.path(), out, depth + 1, max_depth, seen);
            }
        } else if name.ends_with(".meta") {
            out.push(e.path());
            if out.len() >= MAX_META_FILES {
                return;
            }
        }
    }
}

/// The `guid:` line near the start of a `.meta` file.
fn read_guid(meta: &Path) -> Option<String> {
    let mut head = [0u8; 512];
    let mut f = std::fs::File::open(meta).ok()?;
    let n = f.read(&mut head).ok()?;
    let text = std::str::from_utf8(&head[..n]).ok().or_else(|| std::str::from_utf8(&head[..n.min(256)]).ok())?;
    let at = text.find("guid: ")? + 6;
    let guid = text.get(at..at + 32)?;
    guid.bytes().all(|b| b.is_ascii_hexdigit()).then(|| guid.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_guids_without_a_project() {
        let dir = std::env::temp_dir().join(format!("slerp-unity-project-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("sub/a.mat"), b"").unwrap();
        std::fs::write(dir.join("sub/a.mat.meta"), b"fileFormatVersion: 2\nguid: 0123456789abcdef0123456789abcdef\n")
            .unwrap();
        let mut p = Project::find(&dir.join("scene.unity"));
        assert!(!p.in_project);
        assert_eq!(p.asset("0123456789abcdef0123456789abcdef"), Some(dir.join("sub/a.mat")));
        assert_eq!(p.asset("ffffffffffffffffffffffffffffffff"), None);
        assert_eq!(p.asset("../../etc/passwd"), None);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
