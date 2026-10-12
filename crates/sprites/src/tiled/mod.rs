//! Tiled maps (mapeditor.org), from the TMX and JSON format documentation:
//! orthogonal, isometric, staggered and hexagonal maps, finite and
//! infinite, with tile, object, image and group layers.
//!
//! Both file formats are read into the same description (`doc`), which
//! is then resolved against the files it names: external tilesets
//! (`.tsx`, `.tsj`), object templates (`.tx`, `.tj`) and pictures.  Those
//! are read only when [`Limits::neighbours`] allows, and only the pictures
//! of tiles the map uses.  Whatever can't be found is drawn as coloured
//! cells or outlines, so a map always shows its layout.

mod data;
mod doc;
mod draw;
mod json;
mod xml;

use crate::SpriteFormat;
use crate::pixels::{self, MAX_PIXELS};
use doc::*;
use slerp_scene::{Canvas, CanvasLayer, FrameClip, Image8, Info, Limits, LoadError};
use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};

/// The largest map description file read from beside a map (tilesets and
/// templates).
const MAX_SIDE_FILE: u64 = 64 << 20;

/// Most external files (tilesets, templates and pictures) one map may
/// make Slerp read.
const MAX_FILES: usize = 4096;

/// Whether XML text starts with a `<map` root, after any declaration and
/// comments.
pub fn looks_like_tmx(text: &str) -> bool {
    let mut rest = text;
    loop {
        rest = rest.trim_start();
        if let Some(r) = rest.strip_prefix("<?") {
            let Some(end) = r.find("?>") else { return false };
            rest = &r[end + 2..];
        } else if let Some(r) = rest.strip_prefix("<!--") {
            let Some(end) = r.find("-->") else { return false };
            rest = &r[end + 3..];
        } else if let Some(r) = rest.strip_prefix("<!DOCTYPE") {
            let Some(end) = r.find('>') else { return false };
            rest = &r[end + 1..];
        } else {
            return rest.starts_with("<map") && rest[4..].starts_with(|c: char| c.is_whitespace() || c == '>');
        }
    }
}

/*┌─────────────────╮
| The map         |
└─────────────────╯*/
//region The map

/// A tileset ready to draw from: its pictures read, or marked missing.
pub(crate) struct Tileset {
    pub first_gid: u32,
    pub doc: TilesetDoc,
    /// The tileset's one picture, for atlas tilesets.
    pub atlas: Option<Image8>,
    /// Pictures of an image-collection tileset's tiles, by tile id.
    pub images: HashMap<u32, Image8>,
    /// The pictures couldn't be read: tiles draw as coloured cells.
    pub missing: bool,
    /// Columns of the atlas.
    pub columns: u32,
    /// The largest tile the map's tile layers draw from this tileset, which
    /// decides how far tiles overhang the map.
    pub largest: (u32, u32),
}

impl Tileset {
    /// The picture and the rectangle of it showing local tile `id`.
    pub fn tile(&self, id: u32) -> Option<(&Image8, [u32; 4])> {
        let doc = &self.doc;
        let id = doc.tiles.get(&id).and_then(|t| t.first_frame).unwrap_or(id);
        // Rectangles from the file are trusted only inside their picture.
        let inside = |img: &Image8, [x, y, w, h]: [u64; 4]| {
            (w > 0 && h > 0 && x + w <= img.width as u64 && y + h <= img.height as u64)
                .then_some([x as u32, y as u32, w as u32, h as u32])
        };
        if let Some(img) = self.images.get(&id) {
            let whole = [0, 0, img.width as u64, img.height as u64];
            let rect = doc.tiles.get(&id).and_then(|t| t.rect).map_or(whole, |r| r.map(u64::from));
            return inside(img, rect).map(|r| (img, r));
        }
        let atlas = self.atlas.as_ref()?;
        if self.columns == 0 {
            return None;
        }
        let (col, row) = ((id % self.columns) as u64, (id / self.columns) as u64);
        let (tw, th) = (doc.tile_width as u64, doc.tile_height as u64);
        let x = doc.margin as u64 + col * (tw + doc.spacing as u64);
        let y = doc.margin as u64 + row * (th + doc.spacing as u64);
        inside(atlas, [x, y, tw, th]).map(|r| (atlas, r))
    }

    /// The size a tile is drawn at, before the map's grid scaling.
    pub fn tile_size(&self, id: u32) -> (u32, u32) {
        match self.tile(id) {
            Some((_, r)) => (r[2], r[3]),
            None => (self.doc.tile_width, self.doc.tile_height),
        }
    }
}

pub struct Map {
    info: Info,
    pub(crate) doc: MapDoc,
    pub(crate) tilesets: Vec<Tileset>,
    pub(crate) geometry: draw::Geometry,
    /// Pictures of image layers, by layer index.
    pub(crate) layer_images: HashMap<usize, Image8>,
    listed: Vec<CanvasLayer>,
    frames: Vec<f32>,
}

impl Map {
    /// The tileset holding global tile id `gid` (flip flags cleared) and
    /// the local id within it.
    pub(crate) fn tileset_of(&self, gid: u32) -> Option<(usize, u32)> {
        let i = self.tilesets.partition_point(|t| t.first_gid <= gid).checked_sub(1)?;
        let t = &self.tilesets[i];
        let local = gid - t.first_gid;
        if t.doc.tile_count.is_some_and(|n| local >= n) && !t.doc.tiles.contains_key(&local) {
            return None;
        }
        Some((i, local))
    }
}

impl Canvas for Map {
    fn info(&self) -> &Info {
        &self.info
    }

    fn size(&self) -> (u32, u32) {
        self.geometry.size()
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
        draw::compose(self, shown, scale)
    }
}

//endregion

/*┌─────────────────╮
| Loading         |
└─────────────────╯*/
//region Loading

/// Reads a map and what it needs from beside it.
pub fn load(format: SpriteFormat, path: &Path, bytes: &[u8], limits: &Limits) -> Result<Map, LoadError> {
    if bytes.len() as u64 > limits.max_file_bytes {
        return Err(LoadError::TooLarge(format!("{} bytes", bytes.len())));
    }
    let dir = path.parent().unwrap_or(Path::new("")).to_path_buf();
    // A budget for tile references across all layers: 16 million, or more
    // for large files, so a small file can't claim gigabytes of empty
    // layers.
    let budget = (bytes.len() as u64).saturating_mul(64).max(16 << 20);
    let mut doc = match format {
        SpriteFormat::Tmj => json::map(bytes, &dir, budget)?,
        _ => xml::map(bytes, &dir, budget)?,
    };
    let mut files =
        Files { dir: dir.clone(), allowed: limits.neighbours, read: 0, warnings: Vec::new(), paths: Vec::new() };
    resolve_templates(&mut doc, &mut files);
    let mut budget = if limits.max_texture_size.is_some() { MAX_PIXELS } else { MAX_PIXELS * 4 };
    let tilesets = load_tilesets(&doc, &mut files, &mut budget);
    let mut layer_images = HashMap::new();
    for (i, l) in doc.layers.iter().enumerate() {
        if let LayerKind::Image { image: Some(image), .. } = &l.kind
            && let Some(img) = files.image(&dir, image, &mut budget)
        {
            layer_images.insert(i, img);
        }
    }
    let geometry = draw::Geometry::new(&doc, &tilesets);
    let (w, h) = geometry.size();
    if w == 0 || h == 0 {
        return Err(LoadError::invalid("the map has no area"));
    }

    // What the inspector and the layer list show.
    let listed = doc
        .layers
        .iter()
        .map(|l| CanvasLayer {
            name: l.name.clone(),
            visible: l.visible,
            depth: l.depth,
            group: matches!(l.kind, LayerKind::Group),
            note: match &l.kind {
                LayerKind::Tiles(_) => "tiles",
                LayerKind::Objects { .. } => "objects",
                LayerKind::Image { .. } => "image",
                LayerKind::Group => "group",
            }
            .to_string(),
        })
        .collect();
    let orientation = match doc.orientation {
        Orientation::Orthogonal => "orthogonal",
        Orientation::Isometric => "isometric",
        Orientation::Staggered => "staggered",
        Orientation::Hexagonal => "hexagonal",
    };
    let mut details = vec![
        ("Orientation".to_string(), orientation.to_string()),
        ("Map size".into(), format!("{} × {} tiles", doc.width, doc.height)),
        ("Tile size".into(), format!("{} × {}", doc.tile_width, doc.tile_height)),
        ("Picture size".into(), format!("{w} × {h}")),
        ("Layers".into(), doc.layers.len().to_string()),
        ("Tilesets".into(), tilesets.len().to_string()),
    ];
    if doc.infinite {
        details.push(("Infinite".into(), "yes".into()));
    }
    if !doc.encodings.is_empty() {
        details.push(("Layer data".into(), doc.encodings.iter().cloned().collect::<Vec<_>>().join(", ")));
    }
    let mut warnings = std::mem::take(&mut doc.warnings);
    warnings.extend(files.warnings);
    let info = Info {
        format: format!("Tiled map ({})", if format == SpriteFormat::Tmj { "JSON" } else { "TMX" }),
        generator: doc.tiled_version.as_ref().map(|v| format!("Tiled {v}")),
        warnings,
        details,
        neighbours: files.paths,
    };
    Ok(Map { info, doc, tilesets, geometry, layer_images, listed, frames: vec![0.0] })
}

/// Reads files named by the map, within the limits.
struct Files {
    dir: PathBuf,
    allowed: bool,
    read: usize,
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

    /// Reads a tileset or template file, or says why not.
    fn text(&mut self, path: &Path) -> Option<Vec<u8>> {
        if !self.allowed || self.read >= MAX_FILES {
            return None;
        }
        self.read += 1;
        self.note(path);
        match pixels::read_neighbour(path, MAX_SIDE_FILE) {
            Ok(b) => Some(b),
            Err(e) => {
                self.warnings.push(format!("{}: {e}", short(path)));
                None
            }
        }
    }

    fn image(&mut self, dir: &Path, image: &ImageRef, budget: &mut u64) -> Option<Image8> {
        if !self.allowed || self.read >= MAX_FILES || image.source.is_empty() {
            return None;
        }
        self.read += 1;
        let path = dir.join(&image.source);
        self.note(&path);
        match pixels::read_image(&path, *budget) {
            Ok(mut img) => {
                let n = img.width as u64 * img.height as u64;
                if n > *budget {
                    self.warnings.push(format!("{}: left out, too many pixels in all", short(&path)));
                    return None;
                }
                *budget -= n;
                if let Some(rgb) = image.trans {
                    pixels::key_out(&mut img, rgb);
                }
                Some(img)
            }
            Err(e) => {
                self.warnings.push(format!("{}: {e}", short(&path)));
                None
            }
        }
    }
}

fn short(p: &Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| p.display().to_string())
}

/// Reads an external tileset, by its content: XML or JSON.
fn read_tileset_file(files: &mut Files, path: &Path) -> Option<TilesetDoc> {
    let bytes = files.text(path)?;
    let dir = path.parent().unwrap_or(Path::new("")).to_path_buf();
    let result = if bytes.iter().find(|b| !b.is_ascii_whitespace()) == Some(&b'{') {
        json::tileset_file(&bytes, &dir)
    } else {
        xml::tileset_file(&bytes, &dir)
    };
    match result {
        Ok(t) => Some(t),
        Err(e) => {
            files.warnings.push(format!("{}: {e}", short(path)));
            None
        }
    }
}

/// Fills in objects made from templates: the template's fields where the
/// object doesn't set its own, with the template's tile moved into the
/// map's numbering (adding its tileset to the map if the map lacks it).
fn resolve_templates(doc: &mut MapDoc, files: &mut Files) {
    let mut cache: HashMap<PathBuf, Option<TemplateDoc>> = HashMap::new();
    let mut bases: HashMap<PathBuf, Option<u32>> = HashMap::new();
    let mut extra: Vec<TilesetRef> = Vec::new();
    let mut next_gid = doc.next_free_gid();
    let dir = files.dir.clone();
    for li in 0..doc.layers.len() {
        let LayerKind::Objects { objects, .. } = &mut doc.layers[li].kind else { continue };
        for object in objects.iter_mut() {
            let Some(name) = object.template.take() else { continue };
            let path = dir.join(&name);
            let template = cache.entry(path.clone()).or_insert_with(|| {
                let bytes = files.text(&path)?;
                let tdir = path.parent().unwrap_or(Path::new("")).to_path_buf();
                let parsed = if bytes.iter().find(|b| !b.is_ascii_whitespace()) == Some(&b'{') {
                    json::template(&bytes, &tdir)
                } else {
                    xml::template(&bytes, &tdir)
                };
                parsed.map_err(|e| files.warnings.push(format!("{name}: {e}"))).ok()
            });
            let Some(t) = template else { continue };
            // 1. The template's tile, renumbered for this map: the first id
            //    of the map's copy of the template's tileset, found once per
            //    template.
            let mut gid = None;
            if let (Some(tg), Some(tref)) = (t.object.gid, &t.tileset) {
                let base = *bases.entry(path).or_insert_with(|| {
                    match doc.tilesets.iter().chain(&extra).find(|r| r.source.same_file(&tref.source)) {
                        Some(r) => Some(r.first_gid),
                        None => {
                            let first = next_gid;
                            next_gid = next_gid.saturating_add(tref.source.tile_span());
                            extra.push(TilesetRef { first_gid: first, source: tref.source.clone() });
                            Some(first)
                        }
                    }
                });
                let local = (tg & GID_MASK).checked_sub(tref.first_gid);
                gid = match (base, local) {
                    (Some(b), Some(l)) => b.checked_add(l).filter(|&g| g <= GID_MASK).map(|g| g | (tg & !GID_MASK)),
                    _ => None,
                };
            }
            // 2. Fields the object leaves to the template.
            object.gid = object.gid.or(gid);
            object.width = object.width.or(t.object.width);
            object.height = object.height.or(t.object.height);
            object.rotation = object.rotation.or(t.object.rotation);
            object.visible = object.visible.or(t.object.visible);
            if object.shape.is_none() {
                object.shape = t.object.shape.clone();
            }
        }
    }
    doc.tilesets.extend(extra);
    doc.tilesets.sort_by_key(|t| t.first_gid);
}

/// Reads every tileset and the pictures of the tiles the map uses.
fn load_tilesets(doc: &MapDoc, files: &mut Files, budget: &mut u64) -> Vec<Tileset> {
    // 1. The tilesets themselves.
    let mut sets: Vec<Tileset> = Vec::new();
    for r in &doc.tilesets {
        let tdoc = match &r.source {
            TilesetSource::Inline(t) => Some(t.as_ref().clone()),
            TilesetSource::External(path) => read_tileset_file(files, path),
        };
        let missing = tdoc.is_none();
        let tdoc = tdoc.unwrap_or_else(|| TilesetDoc {
            name: r.source.name(),
            tile_width: doc.tile_width,
            tile_height: doc.tile_height,
            ..TilesetDoc::default()
        });
        sets.push(Tileset {
            first_gid: r.first_gid,
            doc: tdoc,
            atlas: None,
            images: HashMap::new(),
            missing,
            columns: 0,
            largest: (0, 0),
        });
    }
    // 2. Which tiles are used.
    let mut used: Vec<BTreeSet<u32>> = vec![BTreeSet::new(); sets.len()];
    let mut in_layers: Vec<BTreeSet<u32>> = vec![BTreeSet::new(); sets.len()];
    let lookup = |gid: u32| -> Option<(usize, u32)> {
        let i = sets.partition_point(|t| t.first_gid <= gid).checked_sub(1)?;
        Some((i, gid - sets[i].first_gid))
    };
    for l in &doc.layers {
        match &l.kind {
            LayerKind::Tiles(grid) => {
                let mut last = u32::MAX;
                for &g in &grid.gids {
                    let g = g & GID_MASK;
                    if g == 0 || g == last {
                        continue;
                    }
                    last = g;
                    if let Some((i, id)) = lookup(g) {
                        used[i].insert(id);
                        in_layers[i].insert(id);
                    }
                }
            }
            LayerKind::Objects { objects, .. } => {
                for o in objects {
                    if let Some((i, id)) = o.gid.and_then(|g| lookup(g & GID_MASK)) {
                        used[i].insert(id);
                    }
                }
            }
            _ => {}
        }
    }
    // 3. Their pictures, within a budget of pixels: thumbnails get less.
    for (set, used) in sets.iter_mut().zip(&used) {
        if used.is_empty() || set.missing {
            continue;
        }
        let dir = set.doc.base_dir.clone();
        if let Some(image) = set.doc.image.clone() {
            set.atlas = files.image(&dir, &image, budget);
            set.missing = set.atlas.is_none();
            if let Some(a) = &set.atlas {
                let d = &set.doc;
                let step = d.tile_width as u64 + d.spacing as u64;
                let room = (a.width as u64).saturating_sub(2 * d.margin as u64) + d.spacing as u64;
                let columns = if step == 0 { 0 } else { (room / step) as u32 };
                set.columns = d.columns.filter(|&c| c > 0).unwrap_or(columns);
            }
        } else {
            // An image collection: one picture per tile, animations showing
            // their first frame.
            let mut wanted: BTreeSet<u32> = BTreeSet::new();
            for &id in used {
                wanted.insert(set.doc.tiles.get(&id).and_then(|t| t.first_frame).unwrap_or(id));
            }
            let mut any_missing = false;
            for id in wanted {
                let Some(image) = set.doc.tiles.get(&id).and_then(|t| t.image.clone()) else { continue };
                match files.image(&dir, &image, budget) {
                    Some(img) => {
                        set.images.insert(id, img);
                    }
                    None => any_missing = true,
                }
            }
            set.missing = any_missing && set.images.is_empty();
        }
    }
    for (set, ids) in sets.iter_mut().zip(&in_layers) {
        for &id in ids {
            let (w, h) = set.tile_size(id);
            set.largest = (set.largest.0.max(w), set.largest.1.max(h));
        }
    }
    if !files.allowed && !sets.is_empty() {
        files.warnings.push("tileset pictures weren't read; tiles are shown as coloured cells".into());
    }
    sets
}

//endregion

#[cfg(test)]
pub mod data_for_tests {
    pub fn base64(s: &str) -> Vec<u8> {
        super::data::base64_bytes(s).unwrap()
    }

    pub fn inflate(b: &[u8]) -> Vec<u8> {
        super::data::decompress(b, "zlib", 1 << 30).unwrap()
    }
}
