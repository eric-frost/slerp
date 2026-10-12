//! Finding and decoding the pictures a model names.  Names come from
//! Windows tools (backslashes, any letter case, sometimes absolute paths
//! from the author's machine) or are engine paths (Doom 3's
//! `models/monsters/imp/imp`, Unreal material names), so lookups ignore
//! case, try the bare file name beside the model and a few usual folders,
//! and try the usual picture extensions.  Every file read is recorded as a
//! neighbour for the viewer's watch mode, found or not.

use slerp_scene::textures::{self, DecodeOptions, Texture};
use slerp_scene::{Limits, record_neighbour};
use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Folders beside a model where its pictures often are.
const FOLDERS: [&str; 6] = ["textures", "texture", "tex", "materials", "images", "maps"];

/// Picture extensions the texture decoders read, in the order tried.
pub const EXTENSIONS: [&str; 7] = ["tga", "png", "dds", "jpg", "jpeg", "bmp", "ktx2"];

/// The largest picture file read.
const MAX_PICTURE_BYTES: u64 = 256 << 20;

/// The pictures of one model, each decoded once.
pub struct Textures<'a> {
    /// The model's folder, when neighbouring files may be read.
    dir: Option<PathBuf>,
    limits: &'a Limits,
    pub list: Vec<Arc<Texture>>,
    by_path: HashMap<PathBuf, Option<usize>>,
    listings: HashMap<PathBuf, Vec<(String, PathBuf)>>,
    pub warnings: Vec<String>,
}

impl<'a> Textures<'a> {
    pub fn new(model: &Path, limits: &'a Limits) -> Self {
        let dir = limits.neighbours.then(|| model.parent().map(Path::to_path_buf).unwrap_or_default());
        Textures {
            dir,
            limits,
            list: Vec::new(),
            by_path: HashMap::new(),
            listings: HashMap::new(),
            warnings: Vec::new(),
        }
    }

    /// The model's folder, when neighbouring files may be read.
    pub fn dir(&self) -> Option<&Path> {
        self.dir.as_deref()
    }

    /// Finds a file named relative to `base` (a folder), ignoring case in
    /// every part of the name.  `..` and `.` parts are followed; an
    /// absolute name is tried as given.
    pub fn find(&mut self, base: &Path, name: &str) -> Option<PathBuf> {
        let name = name.trim().trim_matches('"').replace('\\', "/");
        if name.is_empty() {
            return None;
        }
        if Path::new(&name).is_absolute() && is_file(Path::new(&name)) {
            return Some(PathBuf::from(name));
        }
        let mut at = base.to_path_buf();
        let parts: Vec<&str> = name.split('/').filter(|p| !p.is_empty() && *p != ".").collect();
        for (i, part) in parts.iter().enumerate() {
            if *part == ".." {
                at.pop();
                continue;
            }
            let last = i + 1 == parts.len();
            let direct = at.join(part);
            if if last { is_file(&direct) } else { direct.is_dir() } {
                at = direct;
                continue;
            }
            let wanted = part.to_lowercase();
            let listing = self.listings.entry(at.clone()).or_insert_with(|| list(&at));
            at = listing.iter().find(|(n, _)| *n == wanted)?.1.clone();
        }
        is_file(&at).then_some(at)
    }

    /// Finds a picture the model names: the name relative to the model's
    /// folder, then its bare file name there and in the usual picture
    /// folders.  With `any_extension`, other picture extensions are tried
    /// too (`.tga` named, `.png` shipped).
    pub fn locate(&mut self, name: &str, any_extension: bool) -> Option<PathBuf> {
        let dir = self.dir.clone()?;
        let clean = name.trim().trim_matches('"').replace('\\', "/");
        let file = clean.rsplit('/').next().unwrap_or(&clean).to_string();
        let mut candidates = vec![clean.clone(), file.clone()];
        candidates.extend(FOLDERS.iter().map(|f| format!("{f}/{file}")));
        for c in &candidates {
            if let Some(p) = self.find(&dir, c) {
                return Some(p);
            }
        }
        if any_extension {
            for c in &candidates {
                let stem =
                    c.rsplit_once('.').filter(|(_, e)| e.len() <= 4 && !e.contains('/')).map_or(c.as_str(), |s| s.0);
                for e in EXTENSIONS {
                    if let Some(p) = self.find(&dir, &format!("{stem}.{e}")) {
                        return Some(p);
                    }
                }
            }
        }
        None
    }

    /// Loads a picture the model names (see `locate`), with a warning when
    /// it can't be found or read.
    pub fn load(&mut self, name: &str, any_extension: bool) -> Option<usize> {
        if self.dir.is_none() || name.trim().is_empty() {
            return None;
        }
        match self.locate(name, any_extension) {
            Some(path) => self.load_path(&path),
            None => {
                self.warn(format!("texture not found: {}", name.trim()));
                None
            }
        }
    }

    /// Loads a picture file once, sharing it between materials.
    pub fn load_path(&mut self, path: &Path) -> Option<usize> {
        if let Some(&cached) = self.by_path.get(path) {
            return cached;
        }
        let max = self.limits.max_file_bytes.min(MAX_PICTURE_BYTES);
        let result = match read_limited(path, max) {
            Some(bytes) => self.decode(path, &bytes),
            None => {
                self.warn(format!("texture unreadable or too large: {}", path.display()));
                None
            }
        };
        self.by_path.insert(path.to_path_buf(), result);
        result
    }

    fn decode(&mut self, path: &Path, bytes: &[u8]) -> Option<usize> {
        let opts = self.limits.max_texture_size.map(DecodeOptions::preview).unwrap_or_else(DecodeOptions::full);
        // The decoders promise not to panic; a picture is never worth the
        // whole model anyway.
        match std::panic::catch_unwind(|| textures::decode(bytes, &opts)) {
            Ok(Ok(mut tex)) => {
                // Where the encoded picture is, so it can be saved unchanged.
                tex.source.name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                tex.source.original =
                    Some(textures::Original::File { path: path.to_path_buf(), offset: 0, len: bytes.len() as u64 });
                self.list.push(Arc::new(tex));
                Some(self.list.len() - 1)
            }
            Ok(Err(e)) => {
                self.warn(format!("texture {}: {e}", path.display()));
                None
            }
            Err(_) => {
                self.warn(format!("texture {}: the decoder failed", path.display()));
                None
            }
        }
    }

    pub fn warn(&mut self, message: String) {
        if self.warnings.len() < 100 && !self.warnings.contains(&message) {
            self.warnings.push(message);
        }
    }
}

/// Lower-cased names of a folder's entries.
fn list(dir: &Path) -> Vec<(String, PathBuf)> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    entries.flatten().take(20_000).map(|e| (e.file_name().to_string_lossy().to_lowercase(), e.path())).collect()
}

fn is_file(p: &Path) -> bool {
    std::fs::metadata(p).is_ok_and(|m| m.is_file())
}

/// Reads a regular file of at most `max` bytes, recording it as a
/// neighbour.  Devices and pipes are refused before opening, since opening
/// a FIFO a model names would hang.
pub fn read_limited(path: &Path, max: u64) -> Option<Vec<u8>> {
    record_neighbour(path);
    let meta = std::fs::metadata(path).ok()?;
    if !meta.is_file() || meta.len() > max {
        return None;
    }
    let mut out = Vec::with_capacity(meta.len().min(1 << 30) as usize);
    std::fs::File::open(path).ok()?.take(max).read_to_end(&mut out).ok()?;
    Some(out)
}

/// Files in `dir` (not below it) with one of `extensions`, ignoring case,
/// sorted by name, at most `max` of them: the animation files that sit
/// beside a model.
pub fn siblings(dir: &Path, extensions: &[&str], max: usize) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut out: Vec<PathBuf> = entries
        .flatten()
        .take(20_000)
        .map(|e| e.path())
        .filter(|p| {
            p.extension().and_then(|e| e.to_str()).is_some_and(|e| extensions.iter().any(|x| e.eq_ignore_ascii_case(x)))
        })
        .filter(|p| is_file(p))
        .collect();
    out.sort();
    out.truncate(max);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_names_ignoring_case_and_separators() {
        let dir = std::env::temp_dir().join(format!("slerp-rigged-tex-{}", std::process::id()));
        let sub = dir.join("Tex");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(sub.join("Hair.PNG"), b"x").unwrap();
        std::fs::write(dir.join("body.tga"), b"x").unwrap();
        let limits = Limits::viewer();
        let mut t = Textures::new(&dir.join("model.pmx"), &limits);
        // Compared as real paths: a file system that ignores case (macOS)
        // finds the name as asked for, which is the same file.
        let real = |p: Option<PathBuf>| p.map(|p| std::fs::canonicalize(p).unwrap());
        let hair = real(Some(sub.join("Hair.PNG")));
        assert_eq!(real(t.find(&dir, "tex\\hair.png")), hair);
        assert_eq!(real(t.locate("C:\\Users\\me\\tex\\HAIR.png", false)), hair);
        assert_eq!(t.find(&dir, "missing.png"), None);
        // The bare name in the usual folder, and another extension.
        assert_eq!(real(t.locate("models/x/hair.png", false)), hair);
        assert_eq!(t.locate("body.png", true), Some(dir.join("body.tga")));
        assert_eq!(t.locate("body.png", false), None);
        let found = siblings(&dir, &["tga"], 10);
        assert_eq!(found, vec![dir.join("body.tga")]);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
