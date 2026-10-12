//! The image files a scene names, found and decoded once each.
//!
//! Scenes from modelling tools name their textures by the path they had on
//! the author's machine ("D:/Projects/house/sourceimages/brick.png").  The
//! finder tries the path as given, then ever shorter tails of it under the
//! scene's folder and its parent (a Maya project keeps scenes in `scenes/`
//! and pictures in `sourceimages/`), then the bare name in the scene's
//! folder and the usual texture folders, ignoring case.  Every failure is a
//! warning rather than an error.  Adapted from the model formats' texture
//! set (`crates/models/src/common.rs`).

use slerp_scene::textures::{self, DecodeOptions, Pixels};
use slerp_scene::{ColorSpace, Limits, Texture};
use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Folders near a scene where tools and people put its pictures.
const TEXTURE_DIRS: [&str; 12] = [
    "sourceimages",
    "../sourceimages",
    "textures",
    "texture",
    "tex",
    "images",
    "maps",
    "materials",
    "../textures",
    "../images",
    "../maps",
    "..",
];

pub struct TextureSet<'a> {
    dir: Option<PathBuf>,
    limits: &'a Limits,
    pub textures: Vec<Arc<Texture>>,
    by_key: HashMap<String, Option<usize>>,
    listings: HashMap<PathBuf, Vec<(String, PathBuf)>>,
    pub warnings: Vec<String>,
}

impl<'a> TextureSet<'a> {
    /// A set for the scene at `scene`; `None` (a scene from memory) or
    /// `Limits::neighbours` off finds nothing.
    pub fn new(scene: Option<&Path>, limits: &'a Limits) -> Self {
        let dir = scene.filter(|_| limits.neighbours).map(|p| p.parent().map(Path::to_path_buf).unwrap_or_default());
        TextureSet {
            dir,
            limits,
            textures: Vec::new(),
            by_key: HashMap::new(),
            listings: HashMap::new(),
            warnings: Vec::new(),
        }
    }

    /// Loads the picture a scene names, recording a warning when it can't.
    pub fn file(&mut self, name: &str) -> Option<usize> {
        let name = clean_name(name);
        if name.is_empty() {
            return None;
        }
        if let Some(&cached) = self.by_key.get(&name) {
            return cached;
        }
        let path = self.dir.clone().and_then(|dir| self.resolve(&dir, &name));
        let result = match path {
            Some(path) => {
                let key = format!("path:{}", path.display());
                if let Some(&cached) = self.by_key.get(&key) {
                    cached
                } else {
                    let decoded = match read_limited(&path, self.limits.max_file_bytes) {
                        Some(bytes) => self.decode(&path, &bytes),
                        None => {
                            self.warnings.push(format!("texture unreadable or too large: {}", path.display()));
                            None
                        }
                    };
                    self.by_key.insert(key, decoded);
                    decoded
                }
            }
            None => {
                if self.dir.is_some() {
                    self.warnings.push(format!("texture not found: {name}"));
                }
                None
            }
        };
        self.by_key.insert(name, result);
        result
    }

    /// Adds an already decoded texture.
    pub fn push(&mut self, tex: Texture) -> usize {
        self.textures.push(Arc::new(tex));
        self.textures.len() - 1
    }

    pub fn get(&self, index: usize) -> Option<&Texture> {
        self.textures.get(index).map(|t| &**t)
    }

    fn decode(&mut self, path: &Path, bytes: &[u8]) -> Option<usize> {
        let opts = self.limits.max_texture_size.map(DecodeOptions::preview).unwrap_or_else(DecodeOptions::full);
        // Files without a signature are tried as TGA by their name.
        let tga = path.extension().is_some_and(|e| e.eq_ignore_ascii_case("tga"));
        let decoded = std::panic::catch_unwind(|| match textures::decode(bytes, &opts) {
            Err(_) if tga => textures::decode_as(textures::Format::Tga, bytes, &opts),
            other => other,
        });
        match decoded {
            Ok(Ok(mut tex)) => {
                // Where the encoded picture is, so it can be saved unchanged.
                tex.source.name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                tex.source.original =
                    Some(textures::Original::File { path: path.to_path_buf(), offset: 0, len: bytes.len() as u64 });
                Some(self.push(tex))
            }
            Ok(Err(e)) => {
                self.warnings.push(format!("texture {}: {e}", path.display()));
                None
            }
            Err(_) => {
                self.warnings.push(format!("texture {}: decoder failed", path.display()));
                None
            }
        }
    }

    /// Finds the file a scene names.
    fn resolve(&mut self, dir: &Path, name: &str) -> Option<PathBuf> {
        let given = Path::new(name);
        // 1. As given, when it means something on this machine.
        if given.is_absolute() && is_regular_file(given) {
            return Some(given.to_path_buf());
        }
        // 2. Ever shorter tails under the scene's folder and its parent:
        //    "D:/p/sourceimages/a/b.png" tries "p/sourceimages/a/b.png",
        //    "sourceimages/a/b.png", "a/b.png", "b.png".
        let parts: Vec<&str> = name.split('/').filter(|p| !p.is_empty() && !p.ends_with(':')).collect();
        let roots = [Some(dir.to_path_buf()), dir.parent().map(Path::to_path_buf)];
        for k in 0..parts.len() {
            let tail: PathBuf = parts[k..].iter().collect();
            for root in roots.iter().flatten() {
                let c = root.join(&tail);
                if is_regular_file(&c) {
                    return Some(c);
                }
            }
        }
        // 3. The bare name, ignoring case, in the usual folders.
        let wanted = parts.last()?.to_lowercase();
        let mut dirs = vec![dir.to_path_buf()];
        dirs.extend(TEXTURE_DIRS.iter().map(|d| dir.join(d)));
        for d in dirs {
            let listing = self.listings.entry(d.clone()).or_insert_with(|| list_dir(&d));
            if let Some((_, path)) = listing.iter().find(|(n, _)| *n == wanted) {
                return Some(path.clone());
            }
        }
        None
    }

    /// Packs separate roughness and metalness maps into one texture with
    /// roughness in green and metalness in blue, as glTF's material model
    /// wants.  A missing map's channel is white, so the material's constant
    /// factor applies there.  The smaller map is sampled up to the larger.
    pub fn pack_metal_rough(&mut self, rough: Option<ChannelSource>, metal: Option<ChannelSource>) -> Option<usize> {
        if rough.is_none() && metal.is_none() {
            return None;
        }
        let key = format!("pack:{rough:?}:{metal:?}");
        if let Some(&cached) = self.by_key.get(&key) {
            return cached;
        }
        let result = (|| {
            let get = |s: Option<ChannelSource>| -> Option<(ChannelSource, u32, u32, Vec<u8>)> {
                let s = s?;
                let (w, h, px) = rgba8(self.textures.get(s.texture)?)?;
                Some((s, w, h, px.into_owned()))
            };
            let (r, m) = (get(rough), get(metal));
            let (w, h) = match (&r, &m) {
                (Some(a), Some(b)) => (a.1.max(b.1), a.2.max(b.2)),
                (Some(a), None) | (None, Some(a)) => (a.1, a.2),
                (None, None) => return None,
            };
            let mut out = vec![255u8; w as usize * h as usize * 4];
            for (slot, src) in [(1usize, &r), (2usize, &m)] {
                let Some((s, sw, sh, px)) = src else { continue };
                for y in 0..h {
                    let sy = (y as u64 * *sh as u64 / h as u64) as usize;
                    for x in 0..w {
                        let sx = (x as u64 * *sw as u64 / w as u64) as usize;
                        let v = px[(sy * *sw as usize + sx) * 4 + s.channel.min(3) as usize];
                        out[(y as usize * w as usize + x as usize) * 4 + slot] = if s.invert { 255 - v } else { v };
                    }
                }
            }
            Some(self.push(Texture::from_rgba8(w, h, out, ColorSpace::Linear)))
        })();
        self.by_key.insert(key, result);
        result
    }

    /// Copies a separate opacity mask into the base colour texture's alpha:
    /// the mask's alpha when it has one, otherwise its red channel.
    pub fn merge_alpha(&mut self, base: usize, mask: usize) -> Option<usize> {
        if base == mask {
            return Some(base);
        }
        let key = format!("alpha:{base}:{mask}");
        if let Some(&cached) = self.by_key.get(&key) {
            return cached;
        }
        let result = (|| {
            let base_tex = self.textures.get(base)?;
            let mask_tex = self.textures.get(mask)?;
            let channel = if mask_tex.has_alpha { 3 } else { 0 };
            let (w, h, px) = rgba8(base_tex)?;
            let (mw, mh, mpx) = rgba8(mask_tex)?;
            let mut out = px.into_owned();
            for y in 0..h as usize {
                let my = y * mh as usize / h as usize;
                for x in 0..w as usize {
                    let mx = x * mw as usize / w as usize;
                    out[(y * w as usize + x) * 4 + 3] = mpx[(my * mw as usize + mx) * 4 + channel];
                }
            }
            let color_space = base_tex.color_space;
            Some(self.push(Texture::from_rgba8(w, h, out, color_space)))
        })();
        self.by_key.insert(key, result);
        result
    }
}

/// Where a packed metallic-roughness channel comes from.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct ChannelSource {
    pub texture: usize,
    /// 0 red .. 3 alpha.
    pub channel: u8,
    /// Use `1 - value`, for glossiness maps.
    pub invert: bool,
}

/// Lower-cased names of the entries in a folder.  A folder whose name
/// differs only in case ("Textures") is listed too.
fn list_dir(dir: &Path) -> Vec<(String, PathBuf)> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        if let (Some(parent), Some(leaf)) = (dir.parent(), dir.file_name()) {
            let leaf = leaf.to_string_lossy().to_lowercase();
            if let Ok(entries) = std::fs::read_dir(parent) {
                for e in entries.flatten().take(10_000) {
                    if e.file_name().to_string_lossy().to_lowercase() == leaf && e.path() != dir {
                        return list_dir(&e.path());
                    }
                }
            }
        }
        return out;
    };
    for e in entries.flatten().take(10_000) {
        out.push((e.file_name().to_string_lossy().to_lowercase(), e.path()));
    }
    out
}

fn is_regular_file(p: &Path) -> bool {
    std::fs::metadata(p).map(|m| m.is_file()).unwrap_or(false)
}

/// Reads a regular file of at most `max` bytes, recording it as one the
/// scene read.  Devices and pipes are refused before opening.
pub fn read_limited(path: &Path, max: u64) -> Option<Vec<u8>> {
    slerp_scene::record_neighbour(path);
    let meta = std::fs::metadata(path).ok()?;
    if !meta.is_file() || meta.len() > max {
        return None;
    }
    let mut out = Vec::with_capacity(meta.len().min(1 << 30) as usize);
    std::fs::File::open(path).ok()?.take(max).read_to_end(&mut out).ok()?;
    Some(out)
}

/// Normalises a file name from a scene: Windows separators, `file://`
/// prefixes, quotes, and Maya's `<UDIM>` tile token (the first tile is
/// tried).
fn clean_name(name: &str) -> String {
    let mut s = name.trim().trim_matches('"').replace('\\', "/");
    if let Some(rest) = s.strip_prefix("file://") {
        s = rest.to_string();
        if s.as_bytes().get(2) == Some(&b':') {
            s.remove(0);
        }
    }
    s.replace("<UDIM>", "1001").replace("<udim>", "1001")
}

/// The first surface of a texture as 8-bit RGBA, converting HDR by clamping.
pub fn rgba8(tex: &Texture) -> Option<(u32, u32, std::borrow::Cow<'_, [u8]>)> {
    let level = tex.levels.first()?;
    let (w, h) = (level.width, level.height);
    match level.surfaces.first()? {
        Pixels::Rgba8(v) if v.len() >= (w as usize * h as usize * 4) => Some((w, h, std::borrow::Cow::Borrowed(v))),
        Pixels::Rgba32F(v) if v.len() >= (w as usize * h as usize * 4) => {
            Some((w, h, std::borrow::Cow::Owned(v.iter().map(|&f| (f.clamp(0.0, 1.0) * 255.0 + 0.5) as u8).collect())))
        }
        _ => None,
    }
}
