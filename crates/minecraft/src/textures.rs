//! Block textures from a Minecraft client jar or resource pack that is
//! already on the user's computer, used at run time only: nothing is copied,
//! cached or shipped.  Without one, blocks wear the colour table and the
//! procedural patterns.
//!
//! A pack is a zip (a client jar is one too) or a folder holding
//! `assets/minecraft/`.  Each block state names models in
//! `blockstates/<block>.json`; a model names its textures, through its
//! parents (`block/cube_all`, `block/cube_column`, ...), and its first
//! element says which texture each face wears.  Grass, leaves, water and a
//! few others are grey in the files and tinted by the biome in the game;
//! they get a temperate biome's tint here.

use crate::blocks::BlockState;
use serde_json::Value;
use slerp_archive::Archive;
use slerp_scene::textures::{ColorSpace, DecodeOptions, Pixels, Texture};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// The largest JSON or PNG file read from a pack.
const MAX_FILE: usize = 4 << 20;

/// Where block textures come from, from the user's settings.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum Source {
    /// A resource pack beside the schematic, else an installed game's jar.
    #[default]
    Auto,
    /// Never: the colour table only.
    Off,
    /// This jar, resource pack zip or folder.
    Path(PathBuf),
}

enum Files {
    Zip(Box<Archive>),
    Dir(PathBuf),
}

impl Files {
    fn open(path: &Path) -> Option<Files> {
        if path.is_dir() {
            return path.join("assets/minecraft").is_dir().then(|| Files::Dir(path.to_path_buf()));
        }
        let a = Archive::open(path).ok()?;
        (a.kind() == slerp_archive::ArchiveKind::Zip && !a.find("assets/minecraft/textures/block").is_empty()
            || a.entries().iter().any(|e| e.path.starts_with("assets/minecraft/textures/block/")))
        .then(|| Files::Zip(Box::new(a)))
    }

    fn read(&self, rel: &str) -> Option<Vec<u8>> {
        match self {
            Files::Zip(a) => {
                let &i = a.find(rel).first()?;
                let e = a.entry(i)?;
                if e.size > MAX_FILE as u64 {
                    return None;
                }
                a.read(e).ok()
            }
            Files::Dir(root) => {
                let p = root.join(rel);
                if std::fs::metadata(&p).ok()?.len() > MAX_FILE as u64 {
                    return None;
                }
                std::fs::read(p).ok()
            }
        }
    }
}

/// The textures one block state wears: top, sides and bottom, each with
/// the tint it takes, if any.
#[derive(Clone, Debug, PartialEq)]
pub struct Faces {
    pub top: (String, Option<[u8; 3]>),
    pub side: (String, Option<[u8; 3]>),
    pub bottom: (String, Option<[u8; 3]>),
}

/// A resource pack and the game's jar behind it.
pub struct Pack {
    files: Vec<(PathBuf, Files)>,
    textures: Mutex<HashMap<String, Option<Arc<Texture>>>>,
}

impl Pack {
    /// Finds the textures for a schematic at `schematic`: the given path, or
    /// a resource pack beside the schematic and the newest installed
    /// Minecraft jar.
    pub fn find(schematic: &Path, source: &Source) -> Option<Pack> {
        let mut files = Vec::new();
        match source {
            Source::Off => return None,
            Source::Path(p) => files.extend(Files::open(p).map(|f| (p.clone(), f))),
            Source::Auto => {
                if let Some(dir) = schematic.parent() {
                    let mut zips: Vec<PathBuf> = std::fs::read_dir(dir)
                        .into_iter()
                        .flatten()
                        .flatten()
                        .map(|e| e.path())
                        .filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("zip")))
                        .take(16)
                        .collect();
                    zips.sort();
                    for z in zips {
                        if let Some(f) = Files::open(&z) {
                            files.push((z, f));
                            break;
                        }
                    }
                }
                if let Some(jar) = installed_jar() {
                    files.extend(Files::open(&jar).map(|f| (jar, f)));
                }
            }
        }
        (!files.is_empty()).then(|| Pack { files, textures: Mutex::new(HashMap::new()) })
    }

    /// What was used, for the inspector.
    pub fn describe(&self) -> String {
        self.files.iter().map(|(p, _)| p.display().to_string()).collect::<Vec<_>>().join(", ")
    }

    fn read(&self, rel: &str) -> Option<Vec<u8>> {
        self.files.iter().find_map(|(_, f)| f.read(rel))
    }

    fn json(&self, rel: &str) -> Option<Value> {
        serde_json::from_slice(&self.read(rel)?).ok()
    }

    /// The textures of a block state, through its block state file and
    /// models.
    pub fn faces(&self, state: &BlockState) -> Option<Faces> {
        let (ns, name) = state.name.split_once(':').unwrap_or(("minecraft", &state.name));
        let bs = self.json(&format!("assets/{ns}/blockstates/{name}.json"))?;
        let model = variant_model(&bs, state)?;
        let (textures, faces) = self.model(&model)?;
        let tint = tint(&state.name);
        let resolve = |key: &str| -> Option<(String, Option<[u8; 3]>)> {
            let (tex, tinted) = match faces.get(key) {
                Some((t, tinted)) => (t.clone(), *tinted),
                None => (
                    ["#all", "#texture", "#cross", "#particle"]
                        .iter()
                        .find(|k| textures.contains_key(&k[1..]))?
                        .to_string(),
                    false,
                ),
            };
            let tex = resolve_var(&textures, &tex)?;
            // Water's model has no elements: it's tinted all the same.
            let tinted = tinted || faces.is_empty() && tint.is_some();
            Some((tex, if tinted { tint } else { None }))
        };
        let side = resolve("north").or_else(|| resolve("east"))?;
        Some(Faces {
            top: resolve("up").unwrap_or_else(|| side.clone()),
            bottom: resolve("down").unwrap_or_else(|| side.clone()),
            side,
        })
    }

    /// A model's textures (with its parents') and which texture each face
    /// of its first element wears, and whether that face is tinted.
    #[allow(clippy::type_complexity)]
    fn model(&self, name: &str) -> Option<(HashMap<String, String>, HashMap<String, (String, bool)>)> {
        let mut textures: HashMap<String, String> = HashMap::new();
        let mut faces: Option<HashMap<String, (String, bool)>> = None;
        let mut next = Some(name.to_string());
        for _ in 0..12 {
            let Some(current) = next.take() else { break };
            let (ns, path) = current.split_once(':').unwrap_or(("minecraft", &current));
            if path.starts_with("builtin/") {
                break;
            }
            let Some(m) = self.json(&format!("assets/{ns}/models/{path}.json")) else { break };
            if let Some(t) = m.get("textures").and_then(Value::as_object) {
                for (k, v) in t {
                    if let Some(v) = v.as_str() {
                        textures.entry(k.clone()).or_insert_with(|| v.to_string());
                    }
                }
            }
            if faces.is_none()
                && let Some(first) = m.get("elements").and_then(Value::as_array).and_then(|e| e.first())
            {
                let mut f = HashMap::new();
                if let Some(obj) = first.get("faces").and_then(Value::as_object) {
                    for (dir, face) in obj {
                        if let Some(t) = face.get("texture").and_then(Value::as_str) {
                            f.insert(dir.clone(), (t.to_string(), face.get("tintindex").is_some()));
                        }
                    }
                }
                faces = Some(f);
            }
            next = m.get("parent").and_then(Value::as_str).map(str::to_string);
        }
        (!textures.is_empty()).then(|| (textures, faces.unwrap_or_default()))
    }

    /// A texture by its model name (`minecraft:block/stone`), decoded once:
    /// the first frame of animated ones.
    pub fn texture(&self, name: &str) -> Option<Arc<Texture>> {
        if let Some(t) = self.textures.lock().ok()?.get(name) {
            return t.clone();
        }
        let (ns, path) = name.split_once(':').unwrap_or(("minecraft", name));
        let decoded =
            self.read(&format!("assets/{ns}/textures/{path}.png")).and_then(|b| first_frame(&b)).map(Arc::new);
        self.textures.lock().ok()?.insert(name.to_string(), decoded.clone());
        decoded
    }
}

/// The model of the block state's variant: the first whose properties all
/// match, or the first part of a multipart file (a fence's post).
fn variant_model(bs: &Value, state: &BlockState) -> Option<String> {
    let model = |v: &Value| -> Option<String> {
        let v = if let Some(a) = v.as_array() { a.first()? } else { v };
        v.get("model")?.as_str().map(str::to_string)
    };
    if let Some(variants) = bs.get("variants").and_then(Value::as_object) {
        let fits = |key: &str| {
            key.split(',').filter(|kv| !kv.is_empty()).all(|kv| match kv.split_once('=') {
                Some((k, v)) => state.prop(k).is_none_or(|have| have == v),
                None => true,
            })
        };
        return variants.iter().find(|(k, _)| fits(k)).or_else(|| variants.iter().next()).and_then(|(_, v)| model(v));
    }
    let parts = bs.get("multipart")?.as_array()?;
    parts.iter().find(|p| p.get("when").is_none()).or(parts.first()).and_then(|p| model(p.get("apply")?))
}

/// Follows `#name` references through a model's textures.
fn resolve_var(textures: &HashMap<String, String>, value: &str) -> Option<String> {
    let mut v = value.to_string();
    for _ in 0..8 {
        match v.strip_prefix('#') {
            Some(k) => v = textures.get(k)?.clone(),
            None => return Some(v),
        }
    }
    None
}

/// The biome tint of grass, leaves and water in a temperate biome (plains),
/// and the fixed tints of a few others.
fn tint(name: &str) -> Option<[u8; 3]> {
    Some(match name {
        "grass_block" | "short_grass" | "grass" | "tall_grass" | "fern" | "large_fern" | "potted_fern"
        | "sugar_cane" => [0x91, 0xbd, 0x59],
        "spruce_leaves" => [0x61, 0x99, 0x61],
        "birch_leaves" => [0x80, 0xa7, 0x55],
        "oak_leaves" | "jungle_leaves" | "acacia_leaves" | "dark_oak_leaves" | "mangrove_leaves" | "vine" => {
            [0x77, 0xab, 0x2f]
        }
        "water" | "bubble_column" | "water_cauldron" => [0x3f, 0x76, 0xe4],
        "lily_pad" => [0x20, 0x80, 0x30],
        "redstone_wire" => [0xb0, 0x10, 0x10],
        "melon_stem" | "pumpkin_stem" | "attached_melon_stem" | "attached_pumpkin_stem" => [0x60, 0xa0, 0x30],
        _ => return None,
    })
}

/// Decodes a PNG and keeps its first square frame (animated textures are
/// frames stacked downwards).
fn first_frame(bytes: &[u8]) -> Option<Texture> {
    let t = std::panic::catch_unwind(|| slerp_scene::textures::decode(bytes, &DecodeOptions::full())).ok()?.ok()?;
    let (w, h) = (t.width(), t.height());
    if w == 0 || w > 1024 {
        return None;
    }
    let Pixels::Rgba8(px) = t.base() else { return None };
    let rows = h.min(w) as usize;
    let rgba = px[..rows * w as usize * 4].to_vec();
    Some(Texture::from_rgba8(w, rows as u32, rgba, ColorSpace::Srgb))
}

/// The newest client jar of an installed Minecraft: the official
/// launcher's folders on Linux, Windows and macOS, its Flatpak, and Prism
/// Launcher's and MultiMC's libraries.
pub fn installed_jar() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let mut roots: Vec<PathBuf> = Vec::new();
    if let Some(h) = &home {
        roots.push(h.join(".minecraft/versions"));
        roots.push(h.join(".var/app/com.mojang.Minecraft/.minecraft/versions"));
        roots.push(h.join("Library/Application Support/minecraft/versions"));
        for prism in [
            ".local/share/PrismLauncher",
            ".var/app/org.prismlauncher.PrismLauncher/data/PrismLauncher",
            ".local/share/multimc",
        ] {
            roots.push(h.join(prism).join("libraries/com/mojang/minecraft"));
        }
    }
    if let Some(appdata) = std::env::var_os("APPDATA") {
        roots.push(PathBuf::from(appdata).join(".minecraft/versions"));
    }
    let mut best: Option<(std::time::SystemTime, PathBuf)> = None;
    for root in roots {
        let Ok(versions) = std::fs::read_dir(&root) else { continue };
        for v in versions.flatten().take(256) {
            let Ok(files) = std::fs::read_dir(v.path()) else { continue };
            for f in files.flatten().take(64) {
                let p = f.path();
                let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
                // Server jars and other libraries have no block textures.
                if !name.ends_with(".jar") || name.contains("server") {
                    continue;
                }
                let Ok(m) = f.metadata() else { continue };
                if m.len() < 1 << 20 {
                    continue;
                }
                let time = m.modified().unwrap_or(std::time::UNIX_EPOCH);
                if best.as_ref().is_none_or(|(t, _)| time > *t) {
                    best = Some((time, p));
                }
            }
        }
    }
    best.map(|(_, p)| p)
}
