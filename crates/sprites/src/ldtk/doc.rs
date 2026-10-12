//! The parts of an LDtk project that drawing needs, read from its JSON
//! (documented at ldtk.io/json).  LDtk writes many fields twice: plainly,
//! and again with two leading underscores already resolved for readers
//! (a layer's tile size, its tileset's path, an entity's colour).  The
//! resolved ones are preferred; files from before LDtk 1.0 lack some of
//! them and fall back on the plain ones.
//!
//! Everything here is bounded by the file's own size: tiles, cells and
//! entities are only ever as many as the JSON spells out, and IntGrid cells
//! are kept sparse, so a level claiming a billion cells costs nothing until
//! it has them.

use crate::blend::Px;
use serde_json::Value;
use std::collections::HashMap;

/// Most levels, layers per level and worlds read from one project, far
/// beyond real ones, so a hostile file can't make the loader churn.
const MAX_LEVELS: usize = 100_000;
const MAX_LAYERS: usize = 1_000;
const MAX_WORLDS: usize = 1_000;

/*┌─────────────────╮
| Types           |
└─────────────────╯*/
//region Types

pub struct ProjectDoc {
    pub json_version: String,
    pub app_version: Option<String>,
    /// Layer definitions in file order, which is display order: the top
    /// layer first.
    pub layer_defs: Vec<LayerDef>,
    pub entity_defs: HashMap<i64, EntityDef>,
    pub tilesets: HashMap<i64, TilesetDef>,
    /// One for classic projects; several in LDtk's multi-world projects.
    pub worlds: Vec<WorldDoc>,
    pub default_grid: u32,
    /// Whether levels live in their own `.ldtkl` files.
    pub external_levels: bool,
}

pub struct WorldDoc {
    pub identifier: String,
    pub layout: Layout,
    pub levels: Vec<LevelDoc>,
}

/// How a world places its levels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Layout {
    /// Anywhere, at the coordinates each level gives.
    Free,
    /// On a grid of rooms, also at the levels' own coordinates.
    GridVania,
    /// In a row, in file order; the levels carry no coordinates.
    LinearHorizontal,
    /// In a column, in file order.
    LinearVertical,
}

impl Layout {
    pub fn name(self) -> &'static str {
        match self {
            Layout::Free => "free",
            Layout::GridVania => "GridVania",
            Layout::LinearHorizontal => "horizontal",
            Layout::LinearVertical => "vertical",
        }
    }

    /// Levels sit where their coordinates say, rather than in file order.
    pub fn spatial(self) -> bool {
        matches!(self, Layout::Free | Layout::GridVania)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LayerType {
    /// A grid of integer values (collisions, terrain kinds), which rules
    /// may turn into tiles.
    IntGrid,
    Entities,
    /// Tiles painted by hand.
    Tiles,
    /// Tiles placed by rules from another layer's IntGrid values.
    AutoLayer,
}

impl LayerType {
    fn parse(s: &str) -> Option<LayerType> {
        Some(match s {
            "IntGrid" => LayerType::IntGrid,
            "Entities" => LayerType::Entities,
            "Tiles" => LayerType::Tiles,
            "AutoLayer" => LayerType::AutoLayer,
            _ => return None,
        })
    }

    pub fn note(self) -> &'static str {
        match self {
            LayerType::IntGrid => "IntGrid",
            LayerType::Entities => "entities",
            LayerType::Tiles => "tiles",
            LayerType::AutoLayer => "auto-layer",
        }
    }
}

pub struct LayerDef {
    pub uid: i64,
    pub identifier: String,
    pub kind: LayerType,
    /// IntGrid values' colours, by value.
    pub int_colors: HashMap<i64, Px>,
}

/// How an entity is drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EntityShape {
    Rectangle,
    Ellipse,
    Cross,
    Tile,
}

/// How an entity's tile fills its rectangle.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TileMode {
    /// Scaled to fit inside, keeping proportions.
    FitInside,
    /// Scaled to cover, keeping proportions, the rest cut off.
    Cover,
    Stretch,
    /// Repeated at its own size.
    Repeat,
    /// At its own size, cut to the rectangle.
    FullSizeCropped,
    /// At its own size, overhanging the rectangle.
    FullSizeUncropped,
    /// Corners kept, edges and middle stretched.
    NineSlice,
}

#[derive(Clone, Debug)]
pub struct EntityDef {
    pub shape: EntityShape,
    pub tile_mode: TileMode,
    pub fill_opacity: f32,
    pub line_opacity: f32,
    pub tile_opacity: f32,
    pub color: Px,
    /// A tile the definition shows when an instance names none.
    pub tile: Option<TileRect>,
    /// Nine-slice borders in the tile's pixels: top, right, bottom, left.
    pub nine_slice: [f32; 4],
}

impl Default for EntityDef {
    fn default() -> Self {
        EntityDef {
            shape: EntityShape::Rectangle,
            tile_mode: TileMode::FitInside,
            fill_opacity: 1.0,
            line_opacity: 1.0,
            tile_opacity: 1.0,
            color: [190, 74, 47, 255],
            tile: None,
            nine_slice: [0.0; 4],
        }
    }
}

pub struct TilesetDef {
    /// The picture, relative to the project file.
    pub rel_path: Option<String>,
    /// One of LDtk's own built-in atlases (its icon set), which only LDtk
    /// has.
    pub embedded: bool,
    pub grid: u32,
}

/// A rectangle of a tileset picture.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TileRect {
    pub tileset: i64,
    pub rect: [u32; 4],
}

pub struct LevelDoc {
    pub identifier: String,
    pub iid: String,
    pub world_x: i64,
    pub world_y: i64,
    pub depth: i64,
    pub width: u32,
    pub height: u32,
    pub bg_color: Px,
    pub background: Option<Background>,
    /// Layer instances, top first as the file lists them.  Empty for an
    /// external level until its file is read.
    pub layers: Vec<LayerDoc>,
    /// The level's own `.ldtkl` file, relative to the project, when the
    /// project saves levels separately.
    pub external: Option<String>,
}

/// A level's background picture, already fitted by LDtk: the part of the
/// picture `crop` shown at `top_left`, scaled by `scale`.
pub struct Background {
    pub rel_path: String,
    pub top_left: (f32, f32),
    pub scale: (f32, f32),
    pub crop: [f32; 4],
    pub repeat: bool,
}

pub struct LayerDoc {
    pub identifier: String,
    pub def_uid: i64,
    pub kind: LayerType,
    pub grid: u32,
    pub opacity: f32,
    /// Where the layer sits in the level, in pixels.
    pub offset: (f32, f32),
    pub tileset: Option<i64>,
    pub tileset_path: Option<String>,
    pub visible: bool,
    /// Columns of the IntGrid.
    pub columns: u32,
    /// Non-zero IntGrid cells: index (row by row) and value.
    pub cells: Vec<(u32, i64)>,
    /// Tiles in drawing order, back first: painted ones for tile layers,
    /// rule-made ones for auto-layers and IntGrid layers with rules.
    pub tiles: Vec<TileDoc>,
    pub entities: Vec<EntityDoc>,
}

pub struct TileDoc {
    /// Top left in the layer, in pixels.
    pub px: (f32, f32),
    /// Top left in the tileset picture.
    pub src: (u32, u32),
    /// Bit 0 mirrors horizontally, bit 1 vertically.
    pub flip: u8,
    pub alpha: f32,
    /// The tile's number in its tileset, for colouring it when the picture
    /// is missing.
    pub id: u32,
}

pub struct EntityDoc {
    pub def_uid: i64,
    /// The pivot point in the layer, in pixels.
    pub px: (f32, f32),
    pub size: (f32, f32),
    pub pivot: (f32, f32),
    pub color: Option<Px>,
    pub tile: Option<TileRect>,
}

//endregion

/*┌─────────────────╮
| Reading         |
└─────────────────╯*/
//region Reading

fn num(v: &Value, key: &str) -> Option<f64> {
    v.get(key).and_then(Value::as_f64).filter(|n| n.is_finite())
}

fn int(v: &Value, key: &str) -> Option<i64> {
    v.get(key).and_then(|x| x.as_i64().or_else(|| x.as_f64().filter(|f| f.is_finite()).map(|f| f as i64)))
}

/// A size or count: non-negative and below a few billion.
fn size(v: &Value, key: &str) -> Option<u32> {
    int(v, key).map(|n| n.clamp(0, u32::MAX as i64) as u32)
}

fn text<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(Value::as_str)
}

fn array<'a>(v: &'a Value, key: &str) -> &'a [Value] {
    v.get(key).and_then(Value::as_array).map_or(&[], Vec::as_slice)
}

/// A pair of numbers written as a two-element array, such as `px`.
fn pair(v: &Value, key: &str) -> Option<(f64, f64)> {
    let a = v.get(key)?.as_array()?;
    let x = a.first()?.as_f64()?;
    let y = a.get(1)?.as_f64()?;
    (x.is_finite() && y.is_finite()).then_some((x, y))
}

/// LDtk's colours: `#rrggbb`, or (in old files) a number.
pub fn color(v: Option<&Value>) -> Option<Px> {
    match v? {
        Value::String(s) => {
            let hex = s.trim().trim_start_matches('#');
            if hex.len() != 6 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
                return None;
            }
            let n = u32::from_str_radix(hex, 16).ok()?;
            Some([(n >> 16) as u8, (n >> 8) as u8, n as u8, 255])
        }
        Value::Number(n) => {
            let n = n.as_u64()? as u32;
            Some([(n >> 16) as u8, (n >> 8) as u8, n as u8, 255])
        }
        _ => None,
    }
}

fn tile_rect(v: &Value) -> Option<TileRect> {
    if !v.is_object() {
        return None;
    }
    let tileset = int(v, "tilesetUid")?;
    // 1.0 and later spell out x, y, w and h; earlier files a srcRect.
    let rect = match v.get("srcRect").and_then(Value::as_array) {
        Some(r) if r.len() == 4 => {
            let n =
                |i: usize| r[i].as_f64().filter(|f| f.is_finite()).map_or(0, |f| f.clamp(0.0, u32::MAX as f64) as u32);
            [n(0), n(1), n(2), n(3)]
        }
        _ => [size(v, "x")?, size(v, "y")?, size(v, "w")?, size(v, "h")?],
    };
    (rect[2] > 0 && rect[3] > 0).then_some(TileRect { tileset, rect })
}

/// Reads the project's definitions and its levels' descriptions.  Levels
/// saved in their own files are listed with their path and no layers.
pub fn project(root: &Value) -> Result<ProjectDoc, String> {
    let defs = root.get("defs").ok_or("no definitions")?;
    let header = root.get("__header__");
    let mut doc = ProjectDoc {
        json_version: text(root, "jsonVersion").unwrap_or("").to_string(),
        app_version: header.and_then(|h| text(h, "appVersion")).map(str::to_string),
        layer_defs: Vec::new(),
        entity_defs: HashMap::new(),
        tilesets: HashMap::new(),
        worlds: Vec::new(),
        default_grid: size(root, "defaultGridSize").filter(|&g| g > 0).unwrap_or(16),
        external_levels: root.get("externalLevels").and_then(Value::as_bool).unwrap_or(false),
    };
    // 1. Definitions.
    for l in array(defs, "layers").iter().take(MAX_LAYERS) {
        let (Some(uid), Some(kind)) = (int(l, "uid"), text(l, "type").and_then(LayerType::parse)) else { continue };
        let mut int_colors = HashMap::new();
        for v in array(l, "intGridValues") {
            if let (Some(value), Some(c)) = (int(v, "value"), color(v.get("color"))) {
                int_colors.insert(value, c);
            }
        }
        doc.layer_defs.push(LayerDef {
            uid,
            identifier: text(l, "identifier").unwrap_or("").to_string(),
            kind,
            int_colors,
        });
    }
    for e in array(defs, "entities") {
        let Some(uid) = int(e, "uid") else { continue };
        let fallback = EntityDef::default();
        let hollow = e.get("hollow").and_then(Value::as_bool).unwrap_or(false);
        let borders = array(e, "nineSliceBorders");
        let border =
            |i: usize| borders.get(i).and_then(Value::as_f64).filter(|f| f.is_finite()).unwrap_or(0.0).max(0.0) as f32;
        doc.entity_defs.insert(
            uid,
            EntityDef {
                shape: match text(e, "renderMode") {
                    Some("Ellipse") => EntityShape::Ellipse,
                    Some("Cross") => EntityShape::Cross,
                    Some("Tile") => EntityShape::Tile,
                    _ => EntityShape::Rectangle,
                },
                tile_mode: match text(e, "tileRenderMode") {
                    Some("Cover") => TileMode::Cover,
                    Some("Stretch") => TileMode::Stretch,
                    Some("Repeat") => TileMode::Repeat,
                    Some("FullSizeCropped") => TileMode::FullSizeCropped,
                    Some("FullSizeUncropped") => TileMode::FullSizeUncropped,
                    Some("NineSlice") => TileMode::NineSlice,
                    _ => TileMode::FitInside,
                },
                fill_opacity: num(e, "fillOpacity")
                    .map_or(if hollow { 0.0 } else { fallback.fill_opacity }, |f| f as f32)
                    .clamp(0.0, 1.0),
                line_opacity: num(e, "lineOpacity").map_or(1.0, |f| f as f32).clamp(0.0, 1.0),
                tile_opacity: num(e, "tileOpacity").map_or(1.0, |f| f as f32).clamp(0.0, 1.0),
                color: color(e.get("color")).unwrap_or(fallback.color),
                tile: e.get("tileRect").and_then(tile_rect),
                nine_slice: [border(0), border(1), border(2), border(3)],
            },
        );
    }
    for t in array(defs, "tilesets") {
        let Some(uid) = int(t, "uid") else { continue };
        doc.tilesets.insert(
            uid,
            TilesetDef {
                rel_path: text(t, "relPath").filter(|p| !p.is_empty()).map(str::to_string),
                embedded: t.get("embedAtlas").is_some_and(|a| !a.is_null()),
                grid: size(t, "tileGridSize").filter(|&g| g > 0).unwrap_or(doc.default_grid),
            },
        );
    }

    // 2. Levels: in worlds for multi-world projects, else at the top.
    let bg = color(root.get("defaultLevelBgColor")).or_else(|| color(root.get("bgColor"))).unwrap_or([64, 70, 91, 255]);
    let worlds = array(root, "worlds");
    if worlds.is_empty() || !array(root, "levels").is_empty() {
        let layout = layout(text(root, "worldLayout"));
        let levels = array(root, "levels").iter().take(MAX_LEVELS).map(|l| level(l, &doc, bg)).collect();
        doc.worlds.push(WorldDoc { identifier: String::new(), layout, levels });
    } else {
        let mut budget = MAX_LEVELS;
        for w in worlds.iter().take(MAX_WORLDS) {
            let all = array(w, "levels");
            let levels: Vec<LevelDoc> = all.iter().take(budget).map(|l| level(l, &doc, bg)).collect();
            budget -= levels.len();
            doc.worlds.push(WorldDoc {
                identifier: text(w, "identifier").unwrap_or("").to_string(),
                layout: layout(text(w, "worldLayout")),
                levels,
            });
        }
    }
    Ok(doc)
}

fn layout(s: Option<&str>) -> Layout {
    match s {
        Some("Free") => Layout::Free,
        Some("GridVania") => Layout::GridVania,
        Some("LinearVertical") => Layout::LinearVertical,
        _ => Layout::LinearHorizontal,
    }
}

/// Reads a level: one in the project, or the content of a `.ldtkl` file.
/// `bg` is the project's default background colour.
pub fn level(l: &Value, project: &ProjectDoc, bg: Px) -> LevelDoc {
    let background = text(l, "bgRelPath").filter(|p| !p.is_empty()).and_then(|path| {
        let pos = l.get("__bgPos").filter(|p| p.is_object())?;
        let crop = array(pos, "cropRect");
        let c = |i: usize| crop.get(i).and_then(Value::as_f64).filter(|f| f.is_finite()).unwrap_or(0.0) as f32;
        let (tx, ty) = pair(pos, "topLeftPx").unwrap_or((0.0, 0.0));
        let (sx, sy) = pair(pos, "scale").unwrap_or((1.0, 1.0));
        Some(Background {
            rel_path: path.to_string(),
            top_left: (tx as f32, ty as f32),
            scale: (sx as f32, sy as f32),
            crop: [c(0), c(1), c(2), c(3)],
            repeat: text(l, "bgPos") == Some("Repeat"),
        })
    });
    let layers = array(l, "layerInstances").iter().take(MAX_LAYERS).filter_map(|li| layer(li, project)).collect();
    LevelDoc {
        identifier: text(l, "identifier").unwrap_or("").to_string(),
        iid: text(l, "iid").unwrap_or("").to_string(),
        world_x: int(l, "worldX").unwrap_or(-1),
        world_y: int(l, "worldY").unwrap_or(-1),
        depth: int(l, "worldDepth").unwrap_or(0),
        width: size(l, "pxWid").unwrap_or(0),
        height: size(l, "pxHei").unwrap_or(0),
        bg_color: color(l.get("__bgColor")).or_else(|| color(l.get("bgColor"))).unwrap_or(bg),
        background,
        layers,
        external: text(l, "externalRelPath").filter(|p| !p.is_empty()).map(str::to_string),
    }
}

fn layer(li: &Value, project: &ProjectDoc) -> Option<LayerDoc> {
    let def_uid = int(li, "layerDefUid").unwrap_or(-1);
    let def = project.layer_defs.iter().find(|d| d.uid == def_uid);
    let kind = text(li, "__type").and_then(LayerType::parse).or(def.map(|d| d.kind))?;
    let grid = size(li, "__gridSize").filter(|&g| g > 0).unwrap_or(project.default_grid);
    let columns = size(li, "__cWid").unwrap_or(0);
    let offset = match (num(li, "__pxTotalOffsetX"), num(li, "__pxTotalOffsetY")) {
        (Some(x), Some(y)) => (x as f32, y as f32),
        _ => (num(li, "pxOffsetX").unwrap_or(0.0) as f32, num(li, "pxOffsetY").unwrap_or(0.0) as f32),
    };
    let tileset = int(li, "__tilesetDefUid").or_else(|| int(li, "overrideTilesetUid"));

    // 1. IntGrid values: a CSV array of every cell (1.0 on), or a sparse
    //    list of {coordId, v} before that.
    let mut cells = Vec::new();
    if let Some(csv) = li.get("intGridCsv").and_then(Value::as_array) {
        for (i, v) in csv.iter().enumerate().take(u32::MAX as usize) {
            if let Some(n) = v.as_i64().filter(|&n| n != 0) {
                cells.push((i as u32, n));
            }
        }
    } else {
        for c in array(li, "intGrid") {
            if let (Some(i), Some(v)) = (int(c, "coordId"), int(c, "v")) {
                // Old files stored values from 0, shown as 1 and up.
                if let Ok(i) = u32::try_from(i) {
                    cells.push((i, v + 1));
                }
            }
        }
    }

    // 2. Tiles.  Auto-layer tiles come in drawing order already; painted
    //    ones too, stacked tiles included.
    let source = if kind == LayerType::Tiles { "gridTiles" } else { "autoLayerTiles" };
    let src_grid = tileset.and_then(|t| project.tilesets.get(&t)).map_or(grid, |t| t.grid);
    let tiles = array(li, source)
        .iter()
        .filter_map(|t| {
            let (x, y) = pair(t, "px")?;
            let (sx, sy) = pair(t, "src")?;
            let src = (sx.clamp(0.0, u32::MAX as f64) as u32, sy.clamp(0.0, u32::MAX as f64) as u32);
            let id = int(t, "t").map(|n| n.clamp(0, u32::MAX as i64) as u32).unwrap_or_else(|| {
                // Before 1.0 the id was left to readers: work it out from
                // the picture position, assuming no spacing.
                src.1 / src_grid.max(1) * 1024 + src.0 / src_grid.max(1)
            });
            Some(TileDoc {
                px: (x as f32, y as f32),
                src,
                flip: int(t, "f").unwrap_or(0) as u8 & 3,
                alpha: num(t, "a").unwrap_or(1.0).clamp(0.0, 1.0) as f32,
                id,
            })
        })
        .collect();

    // 3. Entities.
    let entities = array(li, "entityInstances")
        .iter()
        .filter_map(|e| {
            let def_uid = int(e, "defUid").unwrap_or(-1);
            let (x, y) = pair(e, "px")?;
            let w = num(e, "width").unwrap_or(grid as f64).clamp(0.0, 1e7) as f32;
            let h = num(e, "height").unwrap_or(grid as f64).clamp(0.0, 1e7) as f32;
            let (pivot_x, pivot_y) = pair(e, "__pivot").unwrap_or((0.0, 0.0));
            Some(EntityDoc {
                def_uid,
                px: (x as f32, y as f32),
                size: (w, h),
                pivot: (pivot_x.clamp(0.0, 1.0) as f32, pivot_y.clamp(0.0, 1.0) as f32),
                color: color(e.get("__smartColor")),
                tile: e.get("__tile").and_then(tile_rect),
            })
        })
        .collect();

    Some(LayerDoc {
        identifier: text(li, "__identifier")
            .map(str::to_string)
            .or(def.map(|d| d.identifier.clone()))
            .unwrap_or_default(),
        def_uid,
        kind,
        grid,
        opacity: num(li, "__opacity").unwrap_or(1.0).clamp(0.0, 1.0) as f32,
        offset,
        tileset,
        tileset_path: text(li, "__tilesetRelPath").filter(|p| !p.is_empty()).map(str::to_string),
        visible: li.get("visible").and_then(Value::as_bool).unwrap_or(true),
        columns,
        cells,
        tiles,
        entities,
    })
}

//endregion
