//! LDtk projects (ldtk.io): 2D levels made of IntGrid, tile, auto-layer
//! and entity layers, gathered into worlds.  A project is a `.ldtk` JSON
//! file; projects that save levels separately keep each level in a `.ldtkl`
//! file in a folder named after the project.
//!
//! A project shows one of two ways.  The whole file is its world: every
//! level placed where the world puts it (free and GridVania worlds) or, for
//! worlds that keep levels in a row or column, laid out in file order and
//! wrapped into a rough square so a thumbnail shows them all.  Each level is
//! also a part of the file (see `slerp_core::parts`), shown on its own.
//!
//! Tileset pictures (PNG and other images, or Aseprite files) and level
//! backgrounds are read from beside the project only when
//! [`Limits::neighbours`] allows; without them, tiles are drawn as coloured
//! cells and IntGrid layers as their values' colours, so a level always
//! shows its layout.  A `.ldtkl` file opened on its own finds its project in
//! the folder above; failing that, it draws with the colours the level file
//! itself carries.

mod doc;
mod draw;
#[cfg(test)]
mod tests;

use crate::pixels::{self, MAX_PIXELS};
use doc::*;
use serde_json::Value;
use slerp_scene::{Canvas, CanvasLayer, FrameClip, Image8, Info, Limits, LoadError};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

/// The largest level file or tileset picture read from beside a project.
const MAX_SIDE_FILE: u64 = 256 << 20;

/// The largest project read.  Parsed JSON takes several times the file's
/// size in memory, and real projects, levels embedded, run to tens of
/// megabytes.
const MAX_PROJECT: u64 = 256 << 20;

/// The gap between levels that a row or column world lays out, in pixels.
const LINEAR_GAP: i64 = 32;

/// Whether JSON text is an LDtk project or level.  `head` is the start of
/// the file, which LDtk begins with a header naming the file type.
pub fn looks_like_ldtk(head: &str, ext: &str) -> bool {
    if !head.starts_with('{') {
        return false;
    }
    if head.contains("\"LDtk Project JSON\"") || head.contains("\"LDtk Level\"") {
        return true;
    }
    // Files from before the header existed.
    (ext == "ldtk" && head.contains("\"jsonVersion\"")) || (ext == "ldtkl" && head.contains("\"layerInstances\""))
}

/*┌─────────────────╮
| The canvas      |
└─────────────────╯*/
//region The canvas

/// A level placed on the canvas: its top left in canvas pixels.
pub(crate) struct Placed {
    pub level: LevelDoc,
    pub x: i64,
    pub y: i64,
}

/// What one entry of the layer list stands for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Listed {
    /// The levels' background colours and pictures.
    Background,
    /// Every instance of the layer definition with this uid.
    Layer(i64),
}

/// One or more levels of a project, ready to draw.
pub struct Ldtk {
    info: Info,
    listed: Vec<CanvasLayer>,
    pub(crate) keys: Vec<Listed>,
    pub(crate) levels: Vec<Placed>,
    /// The area the levels cover: left, top, width and height in pixels.
    pub(crate) bounds: [i64; 4],
    /// Tileset pictures and level backgrounds, by their path relative to
    /// the project.
    pub(crate) pictures: HashMap<String, Image8>,
    /// Each tileset's picture path, by tileset uid.
    pub(crate) tileset_paths: HashMap<i64, String>,
    /// Each tileset's tile size, by tileset uid.
    pub(crate) tileset_grids: HashMap<i64, u32>,
    pub(crate) int_colors: HashMap<i64, HashMap<i64, crate::blend::Px>>,
    pub(crate) entity_defs: HashMap<i64, EntityDef>,
    frames: Vec<f32>,
}

impl Ldtk {
    /// The picture a layer's tiles come from, if it was read.
    pub(crate) fn layer_picture(&self, layer: &LayerDoc) -> Option<&Image8> {
        let path = layer.tileset.and_then(|t| self.tileset_paths.get(&t)).or(layer.tileset_path.as_ref())?;
        self.pictures.get(path)
    }

    /// The picture and tile rectangle an entity shows, if it was read.
    pub(crate) fn tile_picture(&self, tile: &TileRect) -> Option<&Image8> {
        self.pictures.get(self.tileset_paths.get(&tile.tileset)?)
    }
}

impl Canvas for Ldtk {
    fn info(&self) -> &Info {
        &self.info
    }

    fn size(&self) -> (u32, u32) {
        let clamp = |v: i64| v.clamp(0, u32::MAX as i64) as u32;
        (clamp(self.bounds[2]), clamp(self.bounds[3]))
    }

    fn layers(&self) -> &[CanvasLayer] {
        &self.listed
    }

    fn frames(&self) -> &[f32] {
        &self.frames
    }

    fn clips(&self) -> &[FrameClip] {
        &[]
    }

    fn compose(&self, _frame: usize, shown: &[bool], scale: f32) -> Image8 {
        draw::compose(self, &self.listed, shown, scale)
    }
}

//endregion

/*┌─────────────────╮
| Loading         |
└─────────────────╯*/
//region Loading

/// Which levels to show.
#[derive(Clone, Copy)]
pub enum Pick<'a> {
    /// The first world, all its levels.
    World,
    /// One level by its part name (see `parts`).
    Level(&'a str),
}

fn parse(bytes: &[u8]) -> Result<Value, LoadError> {
    let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
    let root: Value = serde_json::from_slice(bytes).map_err(|e| LoadError::invalid(format!("JSON: {e}")))?;
    if !root.is_object() {
        return Err(LoadError::invalid("not an LDtk file"));
    }
    Ok(root)
}

/// A level file holds one level, not a project.
fn is_level_file(root: &Value) -> bool {
    let kind = root.get("__header__").and_then(|h| h.get("fileType")).and_then(Value::as_str);
    kind == Some("LDtk Level") || (root.get("defs").is_none() && root.get("layerInstances").is_some())
}

/// The names of a project's levels, as parts: the level's identifier, or
/// `world/level` in projects with several worlds.  Empty for a project
/// with one level, and for level files.
pub fn parts(bytes: &[u8]) -> Result<Vec<String>, LoadError> {
    if bytes.len() as u64 > MAX_PROJECT {
        return Err(LoadError::TooLarge(format!("{} bytes", bytes.len())));
    }
    let root = parse(bytes)?;
    if is_level_file(&root) {
        return Ok(Vec::new());
    }
    let project = doc::project(&root).map_err(LoadError::invalid)?;
    let names = part_names(&project);
    Ok(if names.len() > 1 { names } else { Vec::new() })
}

fn part_names(project: &ProjectDoc) -> Vec<String> {
    let several = project.worlds.len() > 1;
    project
        .worlds
        .iter()
        .flat_map(|w| {
            w.levels
                .iter()
                .map(move |l| if several { format!("{}/{}", w.identifier, l.identifier) } else { l.identifier.clone() })
        })
        .collect()
}

/// Reads a project or level file and the pictures it needs.
pub fn load(path: &Path, bytes: &[u8], limits: &Limits, pick: Pick) -> Result<Ldtk, LoadError> {
    if bytes.len() as u64 > limits.max_file_bytes.min(MAX_PROJECT) {
        return Err(LoadError::TooLarge(format!("{} bytes", bytes.len())));
    }
    let root = parse(bytes)?;
    let mut files = Files { allowed: limits.neighbours, warnings: Vec::new(), paths: Vec::new() };
    if is_level_file(&root) {
        return load_level_file(path, &root, limits, files);
    }
    let dir = path.parent().unwrap_or(Path::new("")).to_path_buf();
    let mut project = doc::project(&root).map_err(LoadError::invalid)?;
    drop(root);
    let names = part_names(&project);
    // The world and, for one level, its index there.
    let (world, index) = match pick {
        Pick::World => (0, None),
        Pick::Level(name) => {
            let mut i =
                names.iter().position(|n| n == name).ok_or_else(|| LoadError::invalid(format!("no level {name}")))?;
            let mut found = None;
            for (wi, w) in project.worlds.iter().enumerate() {
                if i < w.levels.len() {
                    found = Some((wi, Some(i)));
                    break;
                }
                i -= w.levels.len();
            }
            found.ok_or_else(|| LoadError::invalid(format!("no level {name}")))?
        }
    };
    if project.worlds.get(world).is_none_or(|w| w.levels.is_empty()) {
        return Err(LoadError::invalid("the project has no levels"));
    }
    let w = project.worlds.swap_remove(world);
    let layout = w.layout;
    let world_name = w.identifier.clone();
    let mut chosen: Vec<LevelDoc> = match index {
        None => w.levels,
        Some(i) => w.levels.into_iter().nth(i).into_iter().collect(),
    };
    // 1. Levels saved in their own files.
    for level in &mut chosen {
        if let Some(rel) = level.external.clone()
            && level.layers.is_empty()
        {
            match files.read(&dir.join(&rel)) {
                Some(b) => match parse(&b) {
                    Ok(v) => level.layers = doc::level(&v, &project, level.bg_color).layers,
                    Err(e) => files.warnings.push(format!("{rel}: {e}")),
                },
                None => files.note_missing(&rel),
            }
        }
    }
    // 2. Where the levels go.  A world shows the levels at the first one's
    //    depth; the others are rooms above or below it.
    let single = chosen.len() == 1;
    let placed = if single {
        chosen.into_iter().map(|level| Placed { level, x: 0, y: 0 }).collect()
    } else {
        place(chosen, layout)
    };
    let what = if single { "LDtk level" } else { "LDtk world" };
    let mut details = vec![("Levels in the project".to_string(), names.len().to_string())];
    if !single {
        details.push(("World layout".into(), layout.name().into()));
        details.push(("Levels shown".into(), placed.len().to_string()));
    }
    if !world_name.is_empty() {
        details.push(("World".into(), world_name));
    }
    if project.external_levels {
        details.push(("Levels saved separately".into(), "yes".into()));
    }
    let generator = match &project.app_version {
        Some(v) => format!("LDtk {v}"),
        None if !project.json_version.is_empty() => format!("LDtk (file version {})", project.json_version),
        None => "LDtk".into(),
    };
    finish(&dir, project, placed, files, limits, what, generator, details)
}

/// Opens a `.ldtkl` level file: through its project when that is in the
/// folder above (LDtk keeps levels in a folder named after the project),
/// otherwise from what the level file says by itself.
fn load_level_file(path: &Path, root: &Value, limits: &Limits, mut files: Files) -> Result<Ldtk, LoadError> {
    let folder = path.parent().unwrap_or(Path::new(""));
    let up = folder.parent().unwrap_or(Path::new("")).to_path_buf();
    let iid = root.get("iid").and_then(Value::as_str).unwrap_or("").to_string();
    if files.allowed
        && let Some(name) = folder.file_name()
    {
        let project_path = up.join(format!("{}.ldtk", name.to_string_lossy()));
        if let Some(bytes) = files.read(&project_path)
            && let Ok(proj) = parse(&bytes)
            && let Ok(doc) = doc::project(&proj)
        {
            // The level's part name in its project.
            let names = part_names(&doc);
            let all = doc.worlds.iter().flat_map(|w| w.levels.iter());
            if let Some(i) = all.clone().position(|l| !iid.is_empty() && l.iid == iid) {
                let mut ldtk = load(&project_path, &bytes, limits, Pick::Level(&names[i]))?;
                ldtk.info.neighbours.insert(0, project_path);
                return Ok(ldtk);
            }
        }
    }
    // No project: tilesets are named relative to the project, which LDtk
    // keeps in the folder above the level's.
    let empty = ProjectDoc {
        json_version: root
            .get("__header__")
            .and_then(|h| h.get("appVersion"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .into(),
        app_version: None,
        layer_defs: Vec::new(),
        entity_defs: HashMap::new(),
        tilesets: HashMap::new(),
        worlds: Vec::new(),
        default_grid: 16,
        external_levels: true,
    };
    let level = doc::level(root, &empty, [64, 70, 91, 255]);
    files.warnings.push("the level's project wasn't found, so IntGrid colours and entity shapes are guesses".into());
    let generator =
        if empty.json_version.is_empty() { "LDtk".to_string() } else { format!("LDtk {}", empty.json_version) };
    let placed = vec![Placed { level, x: 0, y: 0 }];
    finish(&up, empty, placed, files, limits, "LDtk level", generator, Vec::new())
}

/// Lays out a world's levels: where they say for free and GridVania
/// worlds, otherwise in file order, wrapped into rows (or columns) that
/// make a roughly square picture.
fn place(levels: Vec<LevelDoc>, layout: Layout) -> Vec<Placed> {
    if layout.spatial() {
        let depth = levels.first().map_or(0, |l| l.depth);
        return levels
            .into_iter()
            .filter(|l| l.depth == depth)
            .map(|l| Placed { x: l.world_x, y: l.world_y, level: l })
            .collect();
    }
    let n = levels.len();
    let along = |l: &LevelDoc| if layout == Layout::LinearVertical { l.height as i64 } else { l.width as i64 };
    let across = |l: &LevelDoc| if layout == Layout::LinearVertical { l.width as i64 } else { l.height as i64 };
    // 1. The number of levels per row that makes the result squarest.
    let lines = |per: usize| -> (i64, i64) {
        let (mut long, mut short) = (0i64, 0i64);
        for chunk in levels.chunks(per) {
            let length: i64 = chunk.iter().map(along).sum::<i64>() + LINEAR_GAP * (chunk.len() as i64 - 1);
            long = long.max(length);
            short += chunk.iter().map(across).max().unwrap_or(0) + LINEAR_GAP;
        }
        (long, short - LINEAR_GAP)
    };
    // Trying every count costs the square of the levels; beyond a few
    // hundred, a square grid's count is close enough.
    let candidates = if n <= 256 {
        1..=n.max(1)
    } else {
        let k = (n as f64).sqrt().ceil() as usize;
        k..=k
    };
    let per = candidates
        .min_by(|&a, &b| {
            let ratio = |p: usize| {
                let (l, s) = lines(p);
                ((l.max(1) as f64) / (s.max(1) as f64)).ln().abs()
            };
            ratio(a).total_cmp(&ratio(b))
        })
        .unwrap_or(1);
    // 2. Place them.
    let mut out = Vec::with_capacity(n);
    let (mut a, mut b, mut line_size, mut count) = (0i64, 0i64, 0i64, 0usize);
    for level in levels {
        if count == per {
            (a, b, count) = (0, b + line_size + LINEAR_GAP, 0);
            line_size = 0;
        }
        let (x, y) = if layout == Layout::LinearVertical { (b, a) } else { (a, b) };
        a += along(&level) + LINEAR_GAP;
        line_size = line_size.max(across(&level));
        count += 1;
        out.push(Placed { level, x, y });
    }
    out
}

/// Reads the pictures the levels use and builds the canvas.
#[allow(clippy::too_many_arguments)]
fn finish(
    dir: &Path,
    project: ProjectDoc,
    placed: Vec<Placed>,
    mut files: Files,
    limits: &Limits,
    what: &str,
    generator: String,
    mut details: Vec<(String, String)>,
) -> Result<Ldtk, LoadError> {
    // 1. The area covered.
    let (mut x0, mut y0, mut x1, mut y1) = (i64::MAX, i64::MAX, i64::MIN, i64::MIN);
    for p in &placed {
        x0 = x0.min(p.x);
        y0 = y0.min(p.y);
        x1 = x1.max(p.x.saturating_add(p.level.width as i64));
        y1 = y1.max(p.y.saturating_add(p.level.height as i64));
    }
    if placed.is_empty() || x1 <= x0 || y1 <= y0 {
        return Err(LoadError::invalid("the level has no area"));
    }
    let bounds = [x0, y0, x1.saturating_sub(x0), y1.saturating_sub(y0)];

    // 2. The pictures needed: tilesets of layers with tiles, of entity
    //    tiles, and level backgrounds.
    let mut tileset_paths: HashMap<i64, String> = HashMap::new();
    let mut tileset_grids: HashMap<i64, u32> = HashMap::new();
    let mut embedded: HashSet<i64> = HashSet::new();
    for (&uid, t) in &project.tilesets {
        if let Some(p) = &t.rel_path {
            tileset_paths.insert(uid, p.clone());
        }
        if t.embedded {
            embedded.insert(uid);
        }
        tileset_grids.insert(uid, t.grid);
    }
    let mut wanted: Vec<String> = Vec::new();
    let mut want = |p: &str| {
        if !wanted.iter().any(|w| w == p) {
            wanted.push(p.to_string());
        }
    };
    let mut icons = false;
    let mut entities = 0usize;
    let mut tiles = 0usize;
    for p in &placed {
        if let Some(bg) = &p.level.background {
            want(&bg.rel_path);
        }
        for l in &p.level.layers {
            tiles += l.tiles.len();
            entities += l.entities.len();
            if !l.tiles.is_empty() {
                match l.tileset.and_then(|t| tileset_paths.get(&t)).or(l.tileset_path.as_ref()) {
                    Some(path) => want(path),
                    None => icons |= l.tileset.is_some_and(|t| embedded.contains(&t)),
                }
            }
            for e in &l.entities {
                let tile = e.tile.or_else(|| project.entity_defs.get(&e.def_uid).and_then(|d| d.tile));
                if let Some(t) = tile {
                    match tileset_paths.get(&t.tileset) {
                        Some(path) => want(path),
                        None => icons |= embedded.contains(&t.tileset),
                    }
                }
            }
        }
    }
    let mut budget = if limits.max_texture_size.is_some() { MAX_PIXELS } else { MAX_PIXELS * 4 };
    let mut pictures = HashMap::new();
    for rel in wanted {
        if let Some(img) = files.picture(dir, &rel, limits, &mut budget) {
            pictures.insert(rel, img);
        }
    }
    if icons {
        files.warnings.push("tiles from LDtk's built-in icon set are shown as boxes".into());
    }
    if !files.allowed && !project.tilesets.is_empty() {
        files.warnings.push("tileset pictures weren't read; tiles are shown as coloured cells".into());
    }

    // 3. The layer list: the background, then the layer definitions from
    //    the bottom up, then any layer the definitions don't name.
    let mut keys = vec![Listed::Background];
    let mut listed = vec![CanvasLayer {
        name: "Background".into(),
        visible: true,
        depth: 0,
        group: false,
        note: "level colour".into(),
    }];
    let instances = |uid: i64| placed.iter().flat_map(|p| p.level.layers.iter()).filter(move |l| l.def_uid == uid);
    let mut order: Vec<(i64, String, LayerType)> =
        project.layer_defs.iter().rev().map(|d| (d.uid, d.identifier.clone(), d.kind)).collect();
    for p in &placed {
        for l in p.level.layers.iter().rev() {
            if !order.iter().any(|o| o.0 == l.def_uid) {
                order.push((l.def_uid, l.identifier.clone(), l.kind));
            }
        }
    }
    for (uid, name, kind) in order {
        let mut any = false;
        let mut visible = false;
        let mut opacity = 1.0f32;
        for l in instances(uid) {
            any = true;
            visible |= l.visible;
            opacity = opacity.min(l.opacity);
        }
        let mut note = kind.note().to_string();
        if kind == LayerType::IntGrid && instances(uid).any(|l| !l.tiles.is_empty()) {
            note.push_str(", auto-tiles");
        }
        if opacity < 1.0 {
            note.push_str(&format!(", {:.0} %", opacity * 100.0));
        }
        keys.push(Listed::Layer(uid));
        listed.push(CanvasLayer { name, visible: visible || !any, depth: 0, group: false, note });
    }

    // 4. What the inspector shows.
    if let [p] = placed.as_slice() {
        details.insert(0, ("Level".into(), p.level.identifier.clone()));
        details.push(("Level size".into(), format!("{} × {}", p.level.width, p.level.height)));
    } else {
        details.push(("World size".into(), format!("{} × {}", bounds[2], bounds[3])));
    }
    details.push(("Layers".into(), (listed.len() - 1).to_string()));
    details.push(("Tilesets".into(), project.tilesets.len().to_string()));
    details.push(("Tiles".into(), tiles.to_string()));
    details.push(("Entities".into(), entities.to_string()));
    let int_colors = project.layer_defs.iter().map(|d| (d.uid, d.int_colors.clone())).collect();
    let info = Info {
        format: what.to_string(),
        generator: Some(generator),
        warnings: files.warnings,
        details,
        neighbours: files.paths,
    };
    Ok(Ldtk {
        info,
        listed,
        keys,
        levels: placed,
        bounds,
        pictures,
        tileset_paths,
        tileset_grids,
        int_colors,
        entity_defs: project.entity_defs,
        frames: vec![0.0],
    })
}

/// Reads files named by the project, within the limits.
struct Files {
    allowed: bool,
    warnings: Vec<String>,
    /// What was read or looked for, for `Info::neighbours`.
    paths: Vec<PathBuf>,
}

impl Files {
    fn note(&mut self, path: &Path) {
        if !self.paths.iter().any(|p| p == path) {
            self.paths.push(path.to_path_buf());
        }
    }

    fn note_missing(&mut self, rel: &str) {
        if self.allowed {
            self.warnings.push(format!("{rel}: not found"));
        } else if !self.warnings.iter().any(|w| w.starts_with("separate level files")) {
            self.warnings.push("separate level files weren't read".into());
        }
    }

    /// Reads a file beside the project: a level file or the project itself.
    fn read(&mut self, path: &Path) -> Option<Vec<u8>> {
        if !self.allowed {
            return None;
        }
        self.note(path);
        pixels::read_neighbour(path, MAX_SIDE_FILE).ok()
    }

    /// Reads a tileset or background picture: an image file, or an
    /// Aseprite file's first frame, as LDtk reads them.
    fn picture(&mut self, dir: &Path, rel: &str, limits: &Limits, budget: &mut u64) -> Option<Image8> {
        if !self.allowed {
            return None;
        }
        let path = dir.join(rel);
        self.note(&path);
        let lower = rel.to_ascii_lowercase();
        let result = if lower.ends_with(".aseprite") || lower.ends_with(".ase") {
            pixels::read_neighbour(&path, MAX_SIDE_FILE).and_then(|b| {
                let sprite = crate::aseprite::load(&b, limits)?;
                let (w, h) = sprite.size();
                if w as u64 * h as u64 > *budget {
                    return Err(LoadError::TooLarge(format!("{w} × {h} pixels")));
                }
                Ok(sprite.compose(0, &[], 1.0))
            })
        } else {
            pixels::read_image(&path, *budget)
        };
        match result {
            Ok(img) => {
                let n = img.width as u64 * img.height as u64;
                if n > *budget {
                    self.warnings.push(format!("{rel}: left out, too many pixels in all"));
                    return None;
                }
                *budget -= n;
                Some(img)
            }
            Err(e) => {
                self.warnings.push(format!("{rel}: {e}"));
                None
            }
        }
    }
}

//endregion
