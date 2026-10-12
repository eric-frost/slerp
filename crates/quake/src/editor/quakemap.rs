//! Quake-family `.map` files as levels: Quake, Half-Life, Quake 2 and
//! Quake 3, as TrenchBroom, J.A.C.K., Radiant and ericw-tools read them.
//!
//! The world, TrenchBroom's layers and each brush entity become nodes;
//! faces take their textures from the WADs the map names, from the map's
//! own compiled BSP beside it, or from loose image files, and missing ones
//! show a tinted checker.  Tool textures are hidden, skies drawn as skies
//! and liquids see-through.

use super::brush::{self, Plane};
use super::level::{self, Group, Surface};
use super::mapfile::{self, MapEntity, MapFile};
use super::{outside, patch, sky};
use crate::entities::Entity;
use crate::wad::{self, Found};
use crate::{decode_image, find_in, find_relative, read_neighbour};
use rayon::prelude::*;
use slerp_scene::glam::{DVec3, Vec2, Vec3, Vec4};
use slerp_scene::textures::Pixels;
use slerp_scene::{AlphaMode, Effect, Info, Limits, LoadError, Material, Scene, SceneKind, Texture};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Segments per side of each 3x3 piece of a patch.
const PATCH_STEPS: usize = 4;

/// Texture size assumed for missing textures, so a checker square covers
/// 16 units as an editor's grid does.
const MISSING_SIZE: f64 = 64.0;

/*┌─────────────────╮
| Games           |
└─────────────────╯*/
//region Games

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Game {
    Quake,
    HalfLife,
    Quake2,
    Quake3,
}

impl Game {
    fn name(self) -> &'static str {
        match self {
            Game::Quake => "Quake",
            Game::HalfLife => "Half-Life",
            Game::Quake2 => "Quake 2",
            Game::Quake3 => "Quake 3",
        }
    }

    /// Height of the eyes above a player start.
    fn eye_height(self) -> f32 {
        match self {
            Game::Quake | Game::Quake2 => 22.0,
            Game::HalfLife => 28.0,
            Game::Quake3 => 26.0,
        }
    }
}

/// Half-Life entity classes that Quake doesn't have, for maps without
/// TrenchBroom's game comment.
const HALF_LIFE_CLASSES: &[&str] = &[
    "func_breakable",
    "func_pushable",
    "env_sprite",
    "env_glow",
    "ambient_generic",
    "info_node",
    "monster_scientist",
    "monster_barney",
    "scripted_sequence",
    "multi_manager",
    "cycler_sprite",
];

/// Which game a map is for: TrenchBroom's comment when there is one, else
/// what its faces and entities look like.
fn which_game(map: &MapFile) -> Game {
    if let Some(g) = map.game.as_deref().map(str::to_ascii_lowercase) {
        if g.contains("half") || g == "hl" || g.contains("xash") {
            return Game::HalfLife;
        }
        if g.contains("quake 2") || g.contains("quake2") || g.contains("q2") {
            return Game::Quake2;
        }
        if g.contains("quake 3") || g.contains("quake3") || g.contains("openarena") || g.contains("q3") {
            return Game::Quake3;
        }
        if g.contains("quake") || g.contains("hexen") {
            return Game::Quake;
        }
    }
    let dialect3 = map.entities.iter().any(|e| {
        !e.patches.is_empty()
            || e.brushes.iter().any(|b| b.sides.iter().any(|s| matches!(s.tex, mapfile::TexSpec::Matrix(_))))
    });
    if dialect3 {
        return Game::Quake3;
    }
    if map.surface_flags {
        // Quake 2's textures sit in per-unit folders such as `e1u1/`; its
        // worldspawn names a sky box.  Otherwise these are Quake 3's.
        let unit_folder = |t: &str| {
            let b = t.as_bytes();
            b.len() > 5
                && b[0].eq_ignore_ascii_case(&b'e')
                && b[1].is_ascii_digit()
                && b[2].eq_ignore_ascii_case(&b'u')
                && b[3].is_ascii_digit()
                && b[4] == b'/'
        };
        let world = map.entities.first().map(|e| &e.keys);
        let q2 = world.is_some_and(|w| w.get("sky").is_some())
            || map.entities.iter().flat_map(|e| &e.brushes).flat_map(|b| &b.sides).any(|s| unit_folder(&s.texture));
        return if q2 { Game::Quake2 } else { Game::Quake3 };
    }
    let world = map.entities.first().map(|e| &e.keys);
    let wads = world.and_then(|w| w.get("wad")).unwrap_or("").to_ascii_lowercase();
    let hl_wads = ["halflife.wad", "liquids.wad", "decals.wad", "xeno.wad", "zhlt.wad", "sdhlt.wad"];
    let hl_class = map.entities.iter().any(|e| HALF_LIFE_CLASSES.contains(&e.keys.classname()));
    let hl_texture =
        map.entities.iter().flat_map(|e| &e.brushes).flat_map(|b| &b.sides).any(|s| s.texture.starts_with('!'));
    if hl_wads.iter().any(|w| wads.contains(w)) || hl_class || hl_texture { Game::HalfLife } else { Game::Quake }
}

//endregion

/*┌─────────────────╮
| Surfaces        |
└─────────────────╯*/
//region Surfaces

/// What kind of surface a texture makes.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Look {
    Normal,
    /// Translucent by this much (Quake 2's 33 and 66 per cent surfaces).
    Translucent(f32),
    Sky,
    /// Water, slime, lava: unlit, wobbling, and see-through by this much.
    Liquid(f32),
    Tool,
}

/// Textures that only steer the compilers or the game, never drawn: Quake's
/// and Half-Life's by name, Quake 2's and 3's `common/` and `e1u1/` tools.
const TOOLS: &[&str] = &[
    "clip",
    "trigger",
    "skip",
    "hint",
    "hintskip",
    "origin",
    "nodraw",
    "aaatrigger",
    "null",
    "bevel",
    "bevelbrush",
    "clipbevel",
    "clipbevelbrush",
    "solidhint",
    "caulk",
    "caulkwater",
    "areaportal",
    "botclip",
    "donotenter",
    "full_clip",
    "weapclip",
    "nodrop",
    "nodrawnonsolid",
    "lightgrid",
    "antiportal",
    "cushion",
    "invisible",
    "missileclip",
    "monsterclip",
    "playerclip",
    "clusterportal",
    "trigger_multiple",
];

// Quake 2 and 3 surface flags and contents (the same bits in both, except
// where noted).
const SURF_SKY: i64 = 0x4;
/// Quake 2 only; Quake 3's ladder.
const SURF_WARP: i64 = 0x8;
/// Quake 2 only; Quake 3's no-impact and no-marks.
const SURF_TRANS33: i64 = 0x10;
const SURF_TRANS66: i64 = 0x20;
const SURF_NODRAW: i64 = 0x80;
const SURF_HINT: i64 = 0x100;
const SURF_SKIP: i64 = 0x200;
const CONTENTS_LIQUID: i64 = 8 | 16 | 32;
const CONTENTS_TOOL: i64 = 0x8000 | 0x10000 | 0x20000 | 0x100_0000 | 0x4000_0000;

fn classify(name: &str, flags: Option<[i64; 3]>, game: Game, world: &Entity) -> Look {
    let lower = name.to_ascii_lowercase();
    let base = lower.rsplit(['/', '\\']).next().unwrap_or(&lower);
    let common = lower.starts_with("common/") || lower.starts_with("e1u1/") || !lower.contains('/');
    if (common && TOOLS.contains(&base))
        || ["clip", "trigger", "skip", "hint", "nodraw"].iter().any(|p| base.starts_with(p))
    {
        return Look::Tool;
    }
    let (contents, surf) = flags.map_or((0, 0), |f| (f[0], f[1]));
    if surf & (SURF_NODRAW | SURF_HINT | SURF_SKIP) != 0 || contents & CONTENTS_TOOL != 0 {
        return Look::Tool;
    }
    // Quake 3 marks skies in shader scripts, which aren't read; their names
    // say it often enough (`skies/...`, `.../fake_sky`, `lg_sky_01_bk`).
    let q3_sky = game == Game::Quake3 && (base.contains("sky") || lower.contains("skies/"));
    if base.starts_with("sky") || q3_sky || surf & SURF_SKY != 0 {
        return Look::Sky;
    }
    let alpha = |key: &str, default: f32| world.float(key).map_or(default, |a| a.clamp(0.0, 1.0));
    let liquid = match game {
        Game::Quake => base.starts_with('*'),
        Game::HalfLife => base.starts_with('*') || base.starts_with('!'),
        Game::Quake2 => surf & SURF_WARP != 0 || contents & CONTENTS_LIQUID != 0,
        Game::Quake3 => contents & CONTENTS_LIQUID != 0 || lower.starts_with("liquids/"),
    };
    if game == Game::Quake2 && surf & (SURF_TRANS33 | SURF_TRANS66) != 0 {
        let a = if surf & SURF_TRANS33 != 0 { 0.33 } else { 0.66 };
        return if liquid { Look::Liquid(a) } else { Look::Translucent(a) };
    }
    if liquid {
        // Quake's worldspawn keys for liquid see-through-ness, as source
        // ports read them; water and slime are see-through by default.
        let a = if base.contains("lava") {
            alpha("_lavaalpha", 1.0)
        } else if base.contains("slime") {
            alpha("_slimealpha", 0.7)
        } else if base.contains("tele") {
            alpha("_telealpha", 1.0)
        } else {
            alpha("_wateralpha", 0.6)
        };
        return Look::Liquid(a);
    }
    Look::Normal
}

//endregion

/*┌─────────────────╮
| Textures        |
└─────────────────╯*/
//region Textures

/// A texture's pixels and the size its coordinates are measured in.
struct Pic {
    found: Found,
    size: (f64, f64),
    /// An 8-bit palette texture of Quake or Half-Life (from a WAD or BSP).
    palette: bool,
}

/// Finds the pixels of `names` (lowercase): the WADs the map names, the
/// map's own BSP, then loose image files under the map's folders.
fn find_textures(
    map: &MapFile,
    path: &Path,
    names: &HashSet<String>,
    game: Game,
    limits: &Limits,
    warnings: &mut Vec<String>,
    details: &mut Vec<(String, String)>,
) -> (HashMap<String, Pic>, bool) {
    let mut out: HashMap<String, Pic> = HashMap::new();
    let mut half_life = false;
    if names.is_empty() || !limits.neighbours {
        return (out, half_life);
    }
    let world = map.entities.first().map(|e| &e.keys);
    let pic = |f: Found, palette: bool| {
        let t = &f.base;
        let w = if t.source.width > 0 { t.source.width } else { t.width() };
        let h = if t.source.height > 0 { t.source.height } else { t.height() };
        Pic { found: f, size: (w.max(1) as f64, h.max(1) as f64), palette }
    };

    // 1. WADs: the worldspawn's `wad` key, and WADs among TrenchBroom's
    //    texture collections.
    let mut list: Vec<String> = Vec::new();
    for key in ["wad", "_wad", "_tb_textures"] {
        if let Some(v) = world.and_then(|w| w.get(key)) {
            list.extend(
                v.split(';').map(str::trim).filter(|s| s.to_ascii_lowercase().ends_with(".wad")).map(String::from),
            );
        }
    }
    if !list.is_empty() {
        let found = wad::lookup(&list.join(";"), path, names, limits, warnings);
        half_life = found.half_life;
        if !found.files.is_empty() {
            let shown: Vec<String> =
                found.files.iter().filter_map(|f| f.file_name()).map(|n| n.to_string_lossy().into()).collect();
            details.push(("WADs".into(), shown.join(", ")));
        }
        // Some tools write WADs of blank stand-ins (chalk keeps its real
        // textures elsewhere); a set of all-black pictures shows nothing.
        let blank = !found.found.is_empty() && found.found.values().all(|f| is_black(&f.base));
        if blank {
            warnings.push("the WADs hold only blank placeholder textures".into());
        } else {
            out.extend(found.found.into_iter().map(|(k, f)| (k, pic(f, true))));
        }
    }

    // 2. The map's compiled BSP, which carries every texture it used.
    let missing: HashSet<String> = names.iter().filter(|n| !out.contains_key(*n)).cloned().collect();
    if !missing.is_empty()
        && matches!(game, Game::Quake | Game::HalfLife)
        && let Some(bsp) = compiled_bsp(path)
        && let Some(bytes) = read_neighbour(&bsp)
    {
        let found = crate::bsp::embedded_textures(&bytes, &missing, limits.max_texture_size);
        if !found.is_empty() {
            details.push(("Textures from".into(), bsp.file_name().unwrap_or_default().to_string_lossy().into()));
        }
        out.extend(found.into_iter().map(|(k, f)| (k, pic(f, true))));
    }

    // 3. Loose pictures: Quake 2's `textures/<name>.wal` (with its palette
    //    from `pics/colormap.pcx`), Quake 3's TGA and JPEG, and source
    //    ports' replacement textures.
    let missing: Vec<String> = names.iter().filter(|n| !out.contains_key(*n)).cloned().collect();
    if !missing.is_empty() {
        let mut q2_palette = None;
        let loaded: Vec<(String, Found)> = missing
            .iter()
            .filter_map(|n| loose(path, n, &mut q2_palette, limits.max_texture_size).map(|f| (n.clone(), f)))
            .collect();
        out.extend(loaded.into_iter().map(|(k, f)| (k, pic(f, false))));
    }
    (out, half_life)
}

/// True when every texel is opaque black.
fn is_black(t: &Texture) -> bool {
    match t.base() {
        Pixels::Rgba8(v) => v.chunks_exact(4).all(|p| p == [0, 0, 0, 255]),
        _ => false,
    }
}

/// The map's own BSP: same name, beside it or in a `maps` folder above it
/// (LibreQuake keeps `maps/src/dm/lqdm1.map` and `maps/lqdm1.bsp`).
fn compiled_bsp(path: &Path) -> Option<PathBuf> {
    let name = format!("{}.bsp", path.file_stem()?.to_str()?);
    let dir = path.parent()?;
    dir.ancestors().take(4).find_map(|a| find_in(a, &name).or_else(|| find_relative(a, &format!("maps/{name}"))))
}

/// Image extensions tried for loose textures, best first.
const LOOSE_EXTENSIONS: &[&str] = &["png", "tga", "jpg", "jpeg", "pcx", "bmp", "wal"];

/// A loose texture file for `name`: `textures/<name>.<ext>` or
/// `<name>.<ext>` below the map's folder or the folders above it.
fn loose(path: &Path, name: &str, q2_palette: &mut Option<Option<[u8; 768]>>, max: Option<u32>) -> Option<Found> {
    let dir = path.parent()?;
    let name = name.trim_start_matches(['/', '\\']);
    // Some editors keep the image's extension in the name.
    let name = match name.rsplit_once('.') {
        Some((stem, ext)) if LOOSE_EXTENSIONS.contains(&ext.to_ascii_lowercase().as_str()) => stem,
        _ => name,
    };
    for a in dir.ancestors().take(5) {
        for prefix in ["textures/", ""] {
            for ext in LOOSE_EXTENSIONS {
                let rel = format!("{prefix}{name}.{ext}");
                let Some(file) = find_relative(a, &rel) else { continue };
                let bytes = read_neighbour(&file)?;
                if *ext == "wal" {
                    let palette = *q2_palette.get_or_insert_with(|| quake2_palette(dir));
                    return palette.and_then(|p| wal(&bytes, &p, max));
                }
                let t = decode_image(&bytes, max).ok()?;
                return Some(Found { masked: t.has_alpha, base: Arc::new(t), emissive: None });
            }
        }
    }
    None
}

/// Quake 2's palette: the last 768 bytes of `pics/colormap.pcx` in a folder
/// above the map.
fn quake2_palette(dir: &Path) -> Option<[u8; 768]> {
    let file = dir.ancestors().take(5).find_map(|a| find_relative(a, "pics/colormap.pcx"))?;
    let bytes = read_neighbour(&file)?;
    let tail = bytes.get(bytes.len().checked_sub(769)?..)?;
    (tail[0] == 0x0c).then(|| tail[1..].try_into().ok()).flatten()
}

/// A Quake 2 `.wal` texture: a 100-byte header (name, size, four mip
/// offsets) and 8-bit texels in Quake 2's palette.
fn wal(bytes: &[u8], palette: &[u8; 768], max: Option<u32>) -> Option<Found> {
    let b = crate::read::Bytes::new(bytes);
    let (w, h) = (b.u32(32).ok()?, b.u32(36).ok()?);
    let n = crate::pixels::check_size(w, h).ok()?;
    let texels = b.slice(b.u32(40).ok()? as usize, n).ok()?;
    let rgba = crate::pixels::expand(texels, w, h, palette, None);
    let t = crate::pixels::texture(w, h, rgba, max, "Quake 2 WAL", &format!("{w}x{h} 8-bit"));
    Some(Found { base: Arc::new(t), emissive: None, masked: false })
}

/// A sky box's six faces named after `base` (such as `gfx/env/desert`),
/// looked up below the map's folders, or `None` unless all six exist.
pub(crate) fn sky_box(path: &Path, bases: &[String], max: Option<u32>) -> Option<[Texture; 6]> {
    let dir = path.parent()?;
    for base in bases {
        let mut faces = Vec::with_capacity(6);
        for suffix in sky::SUFFIXES {
            let face = dir.ancestors().take(5).find_map(|a| {
                ["tga", "png", "jpg", "pcx", "bmp"]
                    .iter()
                    .find_map(|e| find_relative(a, &format!("{base}{suffix}.{e}")))
            });
            let Some(texture) = face.and_then(|f| read_neighbour(&f)).and_then(|b| decode_image(&b, max).ok()) else {
                break;
            };
            faces.push(texture);
        }
        if let Ok(faces) = faces.try_into() {
            return Some(faces);
        }
    }
    None
}

//endregion

/*┌─────────────────╮
| Building        |
└─────────────────╯*/
//region Building

/// Where an entity's brushes go.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Owner {
    /// The world node (index 0) or a TrenchBroom layer's node.
    Group(usize),
    /// A brush entity's own node.
    Own,
    /// Never drawn (triggers).
    Skip,
}

/// Brush entities that compilers merge into the world.
const WORLD_CLASSES: &[&str] =
    &["worldspawn", "func_group", "func_detail", "func_detail_illusionary", "func_detail_wall", "func_detail_fence"];

/// What makes faces share a material.
#[derive(Clone, PartialEq, Eq, Hash)]
struct MatKey {
    texture: String,
    look: LookKey,
    /// Half-Life's entity render mode and amount.
    render: (u8, u8),
    double_sided: bool,
}

/// `Look` with its alpha as a whole number, so it can be a map key.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum LookKey {
    Normal,
    Translucent(u8),
    Sky,
    Liquid(u8),
}

fn look_key(l: Look) -> Option<LookKey> {
    let byte = |a: f32| (a * 255.0).round() as u8;
    match l {
        Look::Normal => Some(LookKey::Normal),
        Look::Translucent(a) => Some(LookKey::Translucent(byte(a))),
        Look::Sky => Some(LookKey::Sky),
        Look::Liquid(a) => Some(LookKey::Liquid(byte(a))),
        Look::Tool => None,
    }
}

/// Builds materials on demand and the textures they use, once each.
struct Materials<'a> {
    scene: &'a mut Scene,
    pics: &'a HashMap<String, Pic>,
    game: Game,
    by_key: HashMap<MatKey, (usize, (f64, f64))>,
    textures: HashMap<String, (usize, Option<usize>)>,
    checkers: HashMap<String, usize>,
    missing: Vec<String>,
    /// The panorama sky's material, made on first use.
    panorama: Option<usize>,
    sky_box: Option<Texture>,
}

impl Materials<'_> {
    /// The material for a face and the size its texel coordinates are
    /// divided by; `None` for tool faces.
    fn get(&mut self, texture: &str, look: Look, render: (u8, u8), double_sided: bool) -> Option<(usize, (f64, f64))> {
        let lk = look_key(look)?;
        let key = MatKey { texture: texture.to_ascii_lowercase(), look: lk, render, double_sided };
        if let Some(&(m, size)) = self.by_key.get(&key) {
            return Some((m, size));
        }
        let pic = self.pics.get(&key.texture);
        let size = pic.map_or((MISSING_SIZE, MISSING_SIZE), |p| p.size);

        // Skies: Quake's own two-layer sky when its picture is there and
        // no sky box was found, else the panorama.
        if look == Look::Sky && !(self.game == Game::Quake && pic.is_some() && self.sky_box.is_none()) {
            // Only Quake draws the sky texture itself; the other games' are
            // placeholders for their sky boxes.
            if pic.is_none() && self.game == Game::Quake && !self.missing.contains(&key.texture) {
                self.missing.push(key.texture.clone());
            }
            let m = self.panorama();
            self.by_key.insert(key, (m, size));
            return Some((m, size));
        }

        let (base, emissive) = match pic {
            Some(p) => *self.textures.entry(key.texture.clone()).or_insert_with(|| {
                self.scene.textures.push(p.found.base.clone());
                let base = self.scene.textures.len() - 1;
                let emissive = p.found.emissive.as_ref().map(|e| {
                    self.scene.textures.push(e.clone());
                    self.scene.textures.len() - 1
                });
                (base, emissive)
            }),
            None => {
                if !self.missing.contains(&key.texture) {
                    self.missing.push(key.texture.clone());
                }
                let c = *self.checkers.entry(key.texture.clone()).or_insert_with(|| {
                    self.scene.textures.push(Arc::new(level::checker(&key.texture)));
                    self.scene.textures.len() - 1
                });
                (c, None)
            }
        };
        let mut mat = level::matte(texture);
        mat.base_color_texture = Some(level::tex_ref(base));
        // Palette textures (WADs, BSPs) were drawn under doubled light.
        if pic.is_some_and(|p| p.palette) {
            mat.base_color = Vec4::new(level::OVERBRIGHT, level::OVERBRIGHT, level::OVERBRIGHT, 1.0);
        }
        mat.double_sided = double_sided;
        if let Some(e) = emissive {
            mat.emissive = Vec3::ONE;
            mat.emissive_texture = Some(level::tex_ref(e));
        }
        if pic.is_some_and(|p| p.found.masked) {
            mat.alpha_mode = AlphaMode::Mask(0.5);
        }
        match look {
            Look::Sky => {
                // Quake's sky texture: both layers side by side.
                mat.unlit = true;
                mat.effect = Effect::Sky;
            }
            Look::Liquid(a) => {
                mat.unlit = true;
                if self.game != Game::Quake3 {
                    mat.effect = Effect::Warp;
                }
                if a < 1.0 {
                    mat.alpha_mode = AlphaMode::Blend;
                    mat.base_color.w = a;
                }
            }
            Look::Translucent(a) => {
                mat.alpha_mode = AlphaMode::Blend;
                mat.base_color.w = a;
            }
            _ => {}
        }
        if render.0 != 0 && render.0 != 4 {
            mat.alpha_mode = AlphaMode::Blend;
            mat.base_color.w = render.1 as f32 / 255.0;
        }
        self.scene.materials.push(mat);
        let m = self.scene.materials.len() - 1;
        self.by_key.insert(key, (m, size));
        Some((m, size))
    }

    fn panorama(&mut self) -> usize {
        if let Some(m) = self.panorama {
            return m;
        }
        let tex = self.sky_box.take().unwrap_or_else(sky::gradient);
        self.scene.textures.push(Arc::new(tex));
        let t = self.scene.textures.len() - 1;
        let mat = Material {
            name: "sky".into(),
            unlit: true,
            effect: Effect::PanoramaSky,
            base_color: Vec4::ONE,
            base_color_texture: Some(sky::sky_ref(t)),
            ..Default::default()
        };
        self.scene.materials.push(mat);
        let m = self.scene.materials.len() - 1;
        self.panorama = Some(m);
        m
    }
}

/// Textures whose brushes don't seal a level from the void (as qbsp
/// counts it): clip and trigger volumes, origin brushes, portals.
const NON_SOLID: &[&str] = &[
    "areaportal",
    "botclip",
    "clusterportal",
    "donotenter",
    "full_clip",
    "invisible",
    "lightgrid",
    "missileclip",
    "monsterclip",
    "nodrawnonsolid",
    "nodrop",
    "playerclip",
    "weapclip",
    "antiportal",
    "cushion",
];

/// Whether a brush seals the level: no liquid or non-solid tool faces, and
/// not made only of hint and skip faces (a hint brush).
fn seals(b: &mapfile::MapBrush, looks: &HashMap<(String, Option<[i64; 3]>), Look>) -> bool {
    let base = |t: &str| t.rsplit(['/', '\\']).next().unwrap_or(t).to_ascii_lowercase();
    let mut only_hints = true;
    for s in &b.sides {
        let name = base(&s.texture);
        if matches!(looks.get(&(s.texture.to_ascii_lowercase(), s.flags)), Some(Look::Liquid(_))) {
            return false;
        }
        if ["clip", "trigger", "origin"].iter().any(|p| name.starts_with(p)) || NON_SOLID.contains(&name.as_str()) {
            return false;
        }
        if s.flags.is_some_and(|f| f[0] & (CONTENTS_TOOL | CONTENTS_LIQUID) != 0) {
            return false;
        }
        only_hints &= name.starts_with("hint") || name.starts_with("skip");
    }
    !only_hints
}

/// Loads a `.map` file.
pub(crate) fn load(path: &Path, bytes: &[u8], limits: &Limits) -> Result<Scene, LoadError> {
    if bytes.len() as u64 > limits.max_file_bytes {
        return Err(LoadError::TooLarge(format!("{} bytes", bytes.len())));
    }
    let map = mapfile::parse(bytes);
    if map.entities.is_empty() {
        return Err(LoadError::invalid("the map has no entities"));
    }
    let game = which_game(&map);
    let mut warnings = Vec::new();
    let mut details = Vec::new();
    let world_keys = map.entities[0].keys.clone();

    // 1. Groups: the world, TrenchBroom's layers, and brush entities.
    let mut groups = vec![Group { name: "world".into(), visible: true, surfaces: Vec::new() }];
    let mut layer_of: HashMap<String, usize> = HashMap::new();
    for e in &map.entities {
        if e.keys.get("_tb_type") == Some("_tb_layer") {
            let name = e.keys.get("_tb_name").unwrap_or("layer");
            let hidden = e.keys.get("_tb_layer_hidden") == Some("1");
            groups.push(Group { name: format!("layer {name}"), visible: !hidden, surfaces: Vec::new() });
            if let Some(id) = e.keys.get("_tb_id") {
                layer_of.insert(id.to_string(), groups.len() - 1);
            }
        }
    }
    let tb_groups: HashMap<&str, &Entity> = map
        .entities
        .iter()
        .filter(|e| e.keys.get("_tb_type") == Some("_tb_group"))
        .filter_map(|e| e.keys.get("_tb_id").map(|id| (id, &e.keys)))
        .collect();
    // An entity's layer: its own `_tb_layer` key, or its group's, up the
    // chain of nested groups.
    let layer = |keys: &Entity| -> usize {
        let mut k = keys;
        for _ in 0..64 {
            if let Some(l) = k.get("_tb_layer").and_then(|id| layer_of.get(id)) {
                return *l;
            }
            match k.get("_tb_group").and_then(|id| tb_groups.get(id)) {
                Some(parent) => k = parent,
                None => break,
            }
        }
        0
    };
    let owners: Vec<Owner> = map
        .entities
        .iter()
        .map(|e| {
            let class = e.keys.classname().to_ascii_lowercase();
            if class.starts_with("trigger_") {
                Owner::Skip
            } else if WORLD_CLASSES.contains(&class.as_str()) {
                if e.keys.get("_tb_type") == Some("_tb_layer") {
                    e.keys.get("_tb_id").and_then(|id| layer_of.get(id)).map_or(Owner::Group(0), |&g| Owner::Group(g))
                } else {
                    Owner::Group(layer(&e.keys))
                }
            } else {
                Owner::Own
            }
        })
        .collect();

    // 2. How every texture looks, and the pixels of those drawn.
    let mut looks: HashMap<(String, Option<[i64; 3]>), Look> = HashMap::new();
    for e in &map.entities {
        for s in e.brushes.iter().flat_map(|b| &b.sides) {
            looks
                .entry((s.texture.to_ascii_lowercase(), s.flags))
                .or_insert_with(|| classify(&s.texture, s.flags, game, &world_keys));
        }
        for p in &e.patches {
            looks
                .entry((p.texture.to_ascii_lowercase(), None))
                .or_insert_with(|| classify(&p.texture, None, game, &world_keys));
        }
    }
    // A map of nothing but tool brushes (a clip hull, a prefab of
    // triggers) shows them, rather than nothing.
    if !looks.is_empty() && looks.values().all(|l| *l == Look::Tool) {
        warnings.push("the map has only tool brushes, shown as they are".into());
        looks.values_mut().for_each(|l| *l = Look::Normal);
    }
    let wanted: HashSet<String> =
        looks.iter().filter(|(_, l)| !matches!(l, Look::Tool)).map(|((n, _), _)| n.clone()).collect();
    let (pics, half_life_wads) = find_textures(&map, path, &wanted, game, limits, &mut warnings, &mut details);
    let game = if half_life_wads && game == Game::Quake { Game::HalfLife } else { game };
    // Classification depends on the game, which the WADs may have changed.
    if half_life_wads {
        for ((name, flags), look) in looks.iter_mut() {
            *look = classify(name, *flags, game, &world_keys);
        }
    }

    // 3. Brush faces, built in parallel.  The starting polygons must reach
    //    past the whole map.
    let reach = map
        .entities
        .iter()
        .flat_map(|e| &e.brushes)
        .flat_map(|b| &b.sides)
        .map(|s| s.plane.dist.abs())
        .fold(0.0f64, f64::max);
    let world = (reach * 4.0).max(brush::MIN_WORLD);
    let jobs: Vec<(usize, &mapfile::MapBrush)> =
        map.entities.iter().enumerate().flat_map(|(ei, e)| e.brushes.iter().map(move |b| (ei, b))).collect();
    let built: Vec<_> = jobs
        .par_iter()
        .map(|(ei, b)| {
            let planes: Vec<Plane> = b.sides.iter().map(|s| s.plane).collect();
            (*ei, *b, brush::build(&planes, world))
        })
        .collect();

    // 4. The void around the level: world brushes seal it (not detail,
    //    which qbsp doesn't let seal), and point entities must be inside.
    let in_world = |ei: usize| {
        matches!(owners[ei], Owner::Group(_))
            && !map.entities[ei].keys.classname().to_ascii_lowercase().starts_with("func_detail")
    };
    let planes_of: Vec<Vec<Plane>> = built.iter().map(|(_, b, _)| b.sides.iter().map(|s| s.plane).collect()).collect();
    let sealers: Vec<outside::Sealer> = built
        .iter()
        .zip(&planes_of)
        .filter(|((ei, b, _), _)| in_world(*ei) && seals(b, &looks))
        .filter_map(|((_, _, r), planes)| {
            let faces = r.as_ref().ok()?;
            let mut lo = DVec3::splat(f64::MAX);
            let mut hi = DVec3::splat(f64::MIN);
            for v in faces.iter().flatten().flatten() {
                lo = lo.min(*v);
                hi = hi.max(*v);
            }
            Some(outside::Sealer { planes, min: lo, max: hi })
        })
        .collect();
    let origins: Vec<DVec3> = map
        .entities
        .iter()
        .skip(1)
        .filter(|e| e.brushes.is_empty() && e.patches.is_empty() && e.keys.get("_tb_type").is_none())
        .filter_map(|e| e.keys.vec3("origin").map(|o| o.as_dvec3()))
        .collect();
    let hull = outside::Hull::new(&sealers, &origins);
    drop(sealers);

    // 5. Materials and surfaces, entity by entity.
    let mut scene = Scene { kind: SceneKind::Level, fill: level::FILL, ..Default::default() };
    let bases: Vec<String> = sky_names(&world_keys, game);
    let sky_box = if limits.neighbours { sky_box(path, &bases, limits.max_texture_size) } else { None };
    let sky_box = sky_box.and_then(|f| sky::panorama(&f, limits.max_texture_size.map_or(4096, |m| m * 2)));
    if sky_box.is_some() {
        details.push(("Sky box".into(), bases.first().cloned().unwrap_or_default()));
    }
    let mut mats = Materials {
        scene: &mut scene,
        pics: &pics,
        game,
        by_key: HashMap::new(),
        textures: HashMap::new(),
        checkers: HashMap::new(),
        missing: Vec::new(),
        panorama: None,
        sky_box,
    };
    let mut own: HashMap<usize, usize> = HashMap::new();
    let (mut bad, mut unbounded, mut faces, mut outside_faces) = (0usize, 0usize, 0usize, 0usize);
    let mut outside_group: Option<usize> = None;
    let render_of = |e: &MapEntity| -> (u8, u8) {
        let mode = e.keys.float("rendermode").unwrap_or(0.0).clamp(0.0, 5.0) as u8;
        let amount = e.keys.float("renderamt").unwrap_or(0.0).clamp(0.0, 255.0) as u8;
        if game == Game::HalfLife && mode != 0 && mode != 4 { (mode, amount) } else { (0, 255) }
    };
    let mut group_for = |ei: usize, groups: &mut Vec<Group>| -> Option<usize> {
        match owners[ei] {
            Owner::Skip => None,
            Owner::Group(g) => Some(g),
            Owner::Own => Some(*own.entry(ei).or_insert_with(|| {
                let keys = &map.entities[ei].keys;
                let class = keys.classname();
                let name = match keys.get("targetname") {
                    Some(t) => format!("{class} {t}"),
                    None => format!("{class} #{ei}"),
                };
                let l = layer(keys);
                groups.push(Group { name, visible: groups[l].visible, surfaces: Vec::new() });
                groups.len() - 1
            })),
        }
    };
    for (ei, b, result) in built {
        let Some(g) = group_for(ei, &mut groups) else { continue };
        let render = render_of(&map.entities[ei]);
        if render.0 != 0 && render.1 == 0 {
            continue;
        }
        let polys = match result {
            Ok(p) => p.into_iter().enumerate().filter_map(|(i, f)| f.map(|f| (i, f))),
            Err(brush::BrushError::Unbounded) => {
                unbounded += 1;
                continue;
            }
            Err(_) => {
                bad += 1;
                continue;
            }
        };
        for (si, poly) in polys {
            let side = &b.sides[si];
            let look = looks[&(side.texture.to_ascii_lowercase(), side.flags)];
            let Some((material, size)) = mats.get(&side.texture, look, render, false) else { continue };
            let vecs = side.tex.vecs(side.plane.normal);
            let uvs = poly
                .iter()
                .map(|&p| {
                    let st = vecs.at(p);
                    if vecs.normalized {
                        st.as_vec2()
                    } else {
                        Vec2::new((st.x / size.0) as f32, (st.y / size.1) as f32)
                    }
                })
                .collect();
            let n = side.plane.normal.as_vec3();
            // Faces only the void sees go to their own hidden node.
            let to = match &hull {
                Some(h) if in_world(ei) && h.faces_outside(&poly, side.plane.normal) => {
                    outside_faces += 1;
                    *outside_group.get_or_insert_with(|| {
                        groups.push(Group { name: "outside the level".into(), visible: false, surfaces: Vec::new() });
                        groups.len() - 1
                    })
                }
                _ => g,
            };
            groups[to].surfaces.push(Surface {
                material,
                normals: vec![n; poly.len()],
                positions: poly,
                uvs,
                colors: Vec::new(),
                indices: Vec::new(),
            });
            faces += 1;
        }
    }
    let mut patches = 0;
    for (ei, e) in map.entities.iter().enumerate() {
        if e.patches.is_empty() {
            continue;
        }
        let Some(g) = group_for(ei, &mut groups) else { continue };
        for p in &e.patches {
            let look = looks[&(p.texture.to_ascii_lowercase(), None)];
            let Some((material, _)) = mats.get(&p.texture, look, (0, 255), true) else { continue };
            match patch::tessellate(p, PATCH_STEPS, material) {
                Some(s) => {
                    groups[g].surfaces.push(s);
                    patches += 1;
                }
                None => bad += 1,
            }
        }
    }
    let mut missing = std::mem::take(&mut mats.missing);
    drop(mats);
    if let Some(w) = level::missing_warning("textures", &mut missing) {
        warnings.push(w);
    }
    if bad + map.bad_brushes + map.bad_patches > 0 {
        warnings.push(format!("{} brushes or patches could not be built", bad + map.bad_brushes + map.bad_patches));
    }
    if unbounded > 0 {
        warnings.push(format!("{unbounded} brushes are not closed"));
    }
    let group_visible: Vec<bool> = groups.iter().map(|g| g.visible).collect();
    let triangles = level::add_groups(&mut scene, groups, limits)?;
    if scene.nodes.is_empty() {
        return Err(LoadError::invalid("the map has no drawable brushes"));
    }

    // 5. Entities: the spawn point and point entities as markers, except
    //    those in hidden layers.
    let points: Vec<Entity> = map
        .entities
        .iter()
        .filter(|e| e.brushes.is_empty() && e.patches.is_empty() && e.keys.get("_tb_type").is_none())
        .filter(|e| group_visible.get(layer(&e.keys)).copied().unwrap_or(true))
        .map(|e| e.keys.clone())
        .collect();
    let starts = ["info_player_start", "info_player_deathmatch", "info_player_coop", "info_player_start2"];
    scene.spawn = crate::bsp::spawn_at(&points, &starts, game.eye_height());
    scene.markers = points
        .iter()
        .filter(|e| e.classname() != "worldspawn")
        .filter_map(|e| crate::bsp::marker(e, e.classname().to_string()))
        .collect();

    // 6. Facts for the inspector.
    let dialect = if map.valve_axes {
        "Valve 220"
    } else if map
        .entities
        .iter()
        .flat_map(|e| &e.brushes)
        .flat_map(|b| &b.sides)
        .any(|s| matches!(s.tex, mapfile::TexSpec::Matrix(_)))
    {
        "brush primitives"
    } else {
        "standard"
    };
    let title = world_keys.get("message").map(|m| m.replace("\\n", " ").trim().to_string()).filter(|m| !m.is_empty());
    scene.name =
        title.clone().unwrap_or_else(|| path.file_stem().and_then(|s| s.to_str()).unwrap_or("map").to_string());
    let mut facts = vec![("Game".to_string(), game.name().to_string())];
    if let Some(t) = title {
        facts.push(("Map".into(), t));
    }
    if let Some(f) = &map.format {
        facts.push(("Editor format".into(), f.clone()));
    }
    let brushes: usize = map.entities.iter().map(|e| e.brushes.len()).sum();
    facts.push(("Brushes".into(), brushes.to_string()));
    facts.push(("Faces".into(), faces.to_string()));
    if outside_faces > 0 {
        facts.push(("Faces outside the level".into(), format!("{outside_faces} (hidden)")));
    }
    if patches > 0 {
        facts.push(("Patches".into(), patches.to_string()));
    }
    facts.push(("Triangles".into(), triangles.to_string()));
    facts.push(("Entities".into(), map.entities.len().to_string()));
    facts.push(("Textures".into(), format!("{} ({} found)", wanted.len(), pics.len())));
    facts.extend(details);
    scene.info =
        Info { format: format!("{} map ({dialect})", game.name()), warnings, details: facts, ..Info::default() };
    Ok(scene)
}

/// Where a map's sky box pictures may be, by game: Quake source ports'
/// `gfx/env/<sky>`, Half-Life's `gfx/env/<skyname>` (default "desert"),
/// Quake 2's `env/<sky>`.
fn sky_names(world: &Entity, game: Game) -> Vec<String> {
    let name = world.get("skyname").or_else(|| world.get("sky")).map(str::trim).filter(|s| !s.is_empty());
    match (game, name) {
        (Game::HalfLife, n) => vec![format!("gfx/env/{}", n.unwrap_or("desert"))],
        (Game::Quake, Some(n)) => vec![format!("gfx/env/{n}"), format!("env/{n}")],
        (Game::Quake2, Some(n)) => vec![format!("env/{n}")],
        _ => Vec::new(),
    }
}

//endregion

#[cfg(test)]
pub(crate) mod tests;
