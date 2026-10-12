//! Source materials: where `.vmt` files and their `.vtf` textures are
//! found, and what a material says about how it's drawn.
//!
//! A map or model names materials relative to a game's `materials/` folder.
//! They are looked up in `materials/` folders beside the `.vmf` or `.mdl`
//! or above it (a map in `<game>/mapsrc/` finds `<game>/materials/`, a
//! model in `<game>/models/props/` too), then in the game's VPK packs: the
//! `*_dir.vpk` files of the game folder around the file (the folder with
//! `gameinfo.txt`) and of its sibling folders such as `hl2/` and
//! `platform/`, where Source games share Half-Life 2's content.

use super::keyvalues::{self, Kv};
use crate::vpk::Vpk;
use crate::{find_relative, read_neighbour};
use slerp_scene::Limits;
use slerp_scene::glam::Vec3;
use slerp_scene::textures::{Pixels, Texture};
use std::path::{Path, PathBuf};

/// The largest material or texture file read.
const MAX_FILE: u64 = 64 << 20;

/// How many `patch` materials may include one another.
const MAX_INCLUDES: usize = 4;

/// How many folders above a map or model are searched for `materials/`
/// and `gameinfo.txt`: models sit a few folders deep below `models/`.
const MAX_DEPTH: usize = 8;

/*┌─────────────────╮
| Finding files   |
└─────────────────╯*/
//region Finding files

/// The places a map's materials may come from.
pub(crate) struct Library {
    /// Folders holding a `materials/` folder, nearest first.
    roots: Vec<PathBuf>,
    packs: Vec<Vpk>,
}

impl Library {
    /// Finds the material folders and packs for the map or model at `map`.
    /// With `limits.neighbours` off, nothing is searched.
    pub fn new(map: &Path, limits: &Limits) -> Library {
        let mut lib = Library { roots: Vec::new(), packs: Vec::new() };
        let Some(dir) = map.parent() else { return lib };
        if !limits.neighbours {
            return lib;
        }
        let ancestors: Vec<&Path> = dir.ancestors().take(MAX_DEPTH).collect();
        for a in &ancestors {
            if is_material_root(&a.join("materials")) {
                lib.roots.push(a.to_path_buf());
            }
        }
        // The game folder: the nearest folder above the map with a
        // `gameinfo.txt`, then the folders beside it.
        let mut pack_dirs: Vec<PathBuf> = Vec::new();
        if let Some(game) = ancestors.iter().find(|a| a.join("gameinfo.txt").is_file()) {
            pack_dirs.push(game.to_path_buf());
            if let Some(install) = game.parent() {
                let mut siblings: Vec<PathBuf> = std::fs::read_dir(install)
                    .into_iter()
                    .flatten()
                    .flatten()
                    .map(|e| e.path())
                    .filter(|p| p.is_dir() && p.as_path() != *game)
                    .take(64)
                    .collect();
                // Half-Life 2's shared content after the game's own.
                siblings.sort_by_key(|p| p.file_name().is_some_and(|n| n != "hl2"));
                pack_dirs.extend(siblings);
            }
            if !lib.roots.iter().any(|r| r == *game) && is_material_root(&game.join("materials")) {
                lib.roots.push(game.to_path_buf());
            }
        } else {
            // Packs right beside the map or above it.
            pack_dirs.extend(ancestors.iter().take(3).map(|a| a.to_path_buf()));
        }
        for d in pack_dirs {
            let Ok(entries) = std::fs::read_dir(&d) else { continue };
            let mut dirs: Vec<PathBuf> = entries
                .flatten()
                .map(|e| e.path())
                .filter(|p| {
                    p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.to_ascii_lowercase().ends_with("_dir.vpk"))
                })
                .collect();
            dirs.sort();
            for p in dirs.into_iter().take(32) {
                if let Ok(v) = Vpk::open(&p) {
                    lib.packs.push(v);
                }
            }
        }
        lib
    }

    /// A library over exactly these folders and packs, for tests.
    #[cfg(test)]
    pub fn with(roots: Vec<PathBuf>, packs: Vec<Vpk>) -> Library {
        Library { roots, packs }
    }

    /// Reads `rel` (such as `materials/dev/dev_a.vmt`, any case), loose
    /// files first, then packs, noting what was read for watching.
    pub fn read(&self, rel: &str) -> Option<Vec<u8>> {
        let rel = rel.replace('\\', "/").to_ascii_lowercase();
        for root in &self.roots {
            if let Some(p) = find_relative(root, &rel)
                && std::fs::metadata(&p).is_ok_and(|m| m.len() <= MAX_FILE)
            {
                return read_neighbour(&p);
            }
        }
        for pack in &self.packs {
            if pack.contains(&rel)
                && let Ok(bytes) = pack.read(&rel)
            {
                slerp_scene::record_neighbour(pack.path());
                return Some(bytes);
            }
        }
        None
    }

    /// What was searched, for the inspector.
    pub fn describe(&self) -> String {
        let mut parts: Vec<String> = self.roots.iter().map(|r| format!("{}/materials", r.display())).collect();
        parts
            .extend(self.packs.iter().map(|p| p.path().file_name().unwrap_or_default().to_string_lossy().into_owned()));
        parts.join(", ")
    }

    pub fn is_empty(&self) -> bool {
        self.roots.is_empty() && self.packs.is_empty()
    }
}

/// Whether `dir` is a Source `materials` folder: one with `.vmt` files in
/// it or in its subfolders down to `materials/models/<author>/<set>/`,
/// unlike the many other folders of that name.  The search stops after a
/// few hundred folders, so a huge unrelated tree costs little.
fn is_material_root(dir: &Path) -> bool {
    let vmt = |p: &Path| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("vmt"));
    let mut level = vec![dir.to_path_buf()];
    let mut visited = 0;
    for _ in 0..4 {
        let mut next = Vec::new();
        for d in &level {
            visited += 1;
            if visited > 512 {
                return false;
            }
            let Ok(entries) = std::fs::read_dir(d) else { continue };
            for e in entries.flatten().take(4096) {
                let p = e.path();
                if vmt(&p) {
                    return true;
                }
                if e.file_type().is_ok_and(|t| t.is_dir()) {
                    next.push(p);
                }
            }
        }
        level = next;
    }
    false
}

//endregion

/*┌─────────────────╮
| Materials       |
└─────────────────╯*/
//region Materials

/// What a `.vmt` says, as far as drawing a level goes.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct SourceMaterial {
    /// The shader, lower case: `lightmappedgeneric`, `worldvertextransition`,
    /// `unlitgeneric`, `water`, ...
    pub shader: String,
    pub base: Option<String>,
    /// The second texture of a blend (`WorldVertexTransition`).
    pub base2: Option<String>,
    /// The normal map (`$bumpmap`, or `$normalmap` in later shaders), with
    /// green pointing down the texture as in Direct3D.
    pub bump: Option<String>,
    pub translucent: bool,
    /// Drawn by adding its colour (`$additive`), which also sets
    /// `translucent`.
    pub additive: bool,
    /// Both sides are drawn (`$nocull`).
    pub nocull: bool,
    pub alphatest: Option<f32>,
    pub selfillum: bool,
    pub alpha: f32,
    /// The compile flags say it's never drawn (nodraw, clip, trigger,
    /// hint, skip, areaportal, occluder, invisible, ...).
    pub hidden: bool,
    pub sky: bool,
    pub water: bool,
    pub fog_color: Option<Vec3>,
}

impl SourceMaterial {
    pub fn unlit(&self) -> bool {
        self.shader == "unlitgeneric" || self.shader == "unlittwotexture"
    }

    pub fn blends(&self) -> bool {
        self.base2.is_some()
            && (self.shader.contains("worldvertextransition") || self.shader == "lightmapped_4wayblend")
    }
}

/// Reads `materials/<name>.vmt`, following `patch` materials to the ones
/// they include.
pub(crate) fn material(lib: &Library, name: &str) -> Option<SourceMaterial> {
    let mut params: Vec<(String, String)> = Vec::new();
    let mut shader = String::new();
    let mut path = format!("materials/{}.vmt", name.trim_start_matches(['/', '\\']));
    // Overrides from patches, applied over what they include.
    let mut overrides: Vec<Vec<(String, String)>> = Vec::new();
    for _ in 0..=MAX_INCLUDES {
        let bytes = lib.read(&path)?;
        let kv = keyvalues::parse(&bytes);
        let root = kv.iter().find(|k| matches!(k.value, keyvalues::Value::Block(_)))?;
        let s = root.key.to_ascii_lowercase();
        if s == "patch" {
            let mut o = Vec::new();
            for section in ["insert", "replace"] {
                if let Some(b) = root.block(section) {
                    o.extend(pairs(b));
                }
            }
            overrides.push(o);
            path = root.get("include")?.to_string();
            continue;
        }
        shader = s;
        params = pairs(root);
        break;
    }
    if shader.is_empty() {
        return None;
    }
    for o in overrides.into_iter().rev() {
        params.extend(o);
    }
    let get = |k: &str| params.iter().rev().find(|(key, _)| key == k).map(|(_, v)| v.as_str());
    let flag = |k: &str| get(k).and_then(|v| v.trim().parse::<f32>().ok()).is_some_and(|v| v != 0.0);
    let texture = |k: &str| {
        get(k).map(|v| v.trim().replace('\\', "/").replace("//", "/").to_ascii_lowercase()).filter(|v| !v.is_empty())
    };
    let hidden_flags = [
        "%compilenodraw",
        "%compileclip",
        "%compileplayerclip",
        "%compilenpcclip",
        "%compiletrigger",
        "%compilehint",
        "%compileskip",
        "%compileareaportal",
        "%compileoccluder",
        "%compileinvisible",
        "%compileladder",
        "%playerclip",
    ];
    let hidden = hidden_flags.iter().any(|f| flag(f));
    Some(SourceMaterial {
        base: texture("$basetexture"),
        base2: texture("$basetexture2"),
        bump: texture("$bumpmap").or_else(|| texture("$normalmap")),
        translucent: flag("$translucent") || flag("$additive"),
        additive: flag("$additive"),
        nocull: flag("$nocull"),
        alphatest: flag("$alphatest").then(|| {
            get("$alphatestreference").and_then(|v| v.trim().parse::<f32>().ok()).unwrap_or(0.5).clamp(0.0, 1.0)
        }),
        selfillum: flag("$selfillum"),
        alpha: get("$alpha").and_then(|v| v.trim().parse::<f32>().ok()).unwrap_or(1.0).clamp(0.0, 1.0),
        hidden,
        sky: flag("%compilesky") || flag("%compile2dsky") || shader == "sky",
        water: flag("%compilewater") || shader == "water",
        fog_color: get("$fogcolor").and_then(color),
        shader,
    })
}

/// A texture shown lit by itself through its alpha: Source's `$selfillum`
/// glows where the base texture's alpha is high.
pub(crate) fn self_illumination(t: &Texture) -> Option<Texture> {
    let Pixels::Rgba8(v) = t.base() else { return None };
    let glow: Vec<u8> = v
        .chunks_exact(4)
        .flat_map(|p| {
            let a = p[3] as u32;
            [(p[0] as u32 * a / 255) as u8, (p[1] as u32 * a / 255) as u8, (p[2] as u32 * a / 255) as u8, 255]
        })
        .collect();
    Some(Texture::from_rgba8(t.width(), t.height(), glow, t.color_space))
}

/// A block's text entries with lower-case keys.
fn pairs(b: &Kv<'_>) -> Vec<(String, String)> {
    b.children().iter().filter_map(|k| k.text().map(|v| (k.key.to_ascii_lowercase(), v.to_string()))).collect()
}

/// A colour as materials write them: `{255 128 0}` (bytes) or
/// `[1 0.5 0]` (fractions).
fn color(v: &str) -> Option<Vec3> {
    let n: Vec<f64> = keyvalues::numbers(v).collect();
    if n.len() < 3 {
        return None;
    }
    let c = Vec3::new(n[0] as f32, n[1] as f32, n[2] as f32);
    Some(if v.trim_start().starts_with('{') { c / 255.0 } else { c }.clamp(Vec3::ZERO, Vec3::ONE))
}

/// Whether a material is one of Hammer's tool materials, by name, and how
/// it is drawn: hidden, as the sky, or (`toolsblack`) black.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Tool {
    Hidden,
    Sky,
    Black,
}

pub(crate) fn tool(name: &str) -> Option<Tool> {
    let n = name.trim_start_matches(['/', '\\']).replace('\\', "/").to_ascii_lowercase();
    let base = n.strip_prefix("tools/")?;
    Some(if base.starts_with("toolsskybox") {
        Tool::Sky
    } else if base.starts_with("toolsblack") {
        Tool::Black
    } else {
        Tool::Hidden
    })
}

//endregion

#[cfg(test)]
mod tests {
    use super::*;

    fn lib_with(files: &[(&str, &str)]) -> (Library, PathBuf) {
        let dir = std::env::temp_dir().join(format!("slerp-vmt-{}-{}", std::process::id(), files.len()));
        let _ = std::fs::remove_dir_all(&dir);
        for (rel, text) in files {
            let p = dir.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, text).unwrap();
        }
        (Library::with(vec![dir.clone()], Vec::new()), dir)
    }

    #[test]
    fn reads_parameters_and_patches() {
        let (lib, dir) = lib_with(&[
            (
                "materials/nature/blend.vmt",
                "\"WorldVertexTransition\"\n{\n\t\"$basetexture\" \"Nature\\Dirt01\"\n\t\"$basetexture2\" \"nature/grass01\"\n\t\"%keywords\" \"x\"\n}\n",
            ),
            ("materials/glass/window.vmt", "LightmappedGeneric { $basetexture glass/window $translucent 1 $alpha .5 }"),
            ("materials/water/w.vmt", "\"Water\" { \"%compilewater\" 1 \"$fogcolor\" \"{25 50 100}\" }"),
            (
                "materials/custom/p.vmt",
                "patch { include \"materials/glass/window.vmt\" replace { $basetexture custom/other } }",
            ),
            ("materials/custom/loop.vmt", "patch { include \"materials/custom/loop.vmt\" }"),
            ("materials/custom/nodraw.vmt", "LightmappedGeneric { %compilenodraw 1 }"),
        ]);
        let m = material(&lib, "NATURE/blend").unwrap();
        assert_eq!(m.shader, "worldvertextransition");
        assert_eq!((m.base.as_deref(), m.base2.as_deref()), (Some("nature/dirt01"), Some("nature/grass01")));
        assert!(m.blends());
        let g = material(&lib, "glass/window").unwrap();
        assert!(g.translucent && (g.alpha - 0.5).abs() < 1e-6);
        let w = material(&lib, "water/w").unwrap();
        assert!(w.water);
        assert!((w.fog_color.unwrap() - Vec3::new(25.0, 50.0, 100.0) / 255.0).length() < 1e-6);
        let p = material(&lib, "custom/p").unwrap();
        assert_eq!(p.base.as_deref(), Some("custom/other"));
        assert!(p.translucent, "patched materials keep what they include");
        assert!(material(&lib, "custom/loop").is_none(), "an include loop ends");
        assert!(material(&lib, "custom/nodraw").unwrap().hidden);
        assert!(material(&lib, "missing/thing").is_none());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn tool_materials() {
        assert_eq!(tool("TOOLS/TOOLSNODRAW"), Some(Tool::Hidden));
        assert_eq!(tool("tools/toolsclip"), Some(Tool::Hidden));
        assert_eq!(tool("tools/toolsskybox2d"), Some(Tool::Sky));
        assert_eq!(tool("tools/toolsblack"), Some(Tool::Black));
        assert_eq!(tool("dev/dev_measuregeneric01b"), None);
    }
}
