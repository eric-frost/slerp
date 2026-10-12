//! What a Tiled map says, whichever file format it came in, before the
//! files it names are read.

use crate::blend::Px;
use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};

/// Global tile ids carry flip flags in their top four bits.
pub const GID_MASK: u32 = 0x0FFF_FFFF;
pub const FLIP_X: u32 = 0x8000_0000;
pub const FLIP_Y: u32 = 0x4000_0000;
pub const FLIP_DIAGONAL: u32 = 0x2000_0000;
/// On hexagonal maps the diagonal flag turns a tile 60 degrees instead,
/// and this one 120 degrees.
pub const ROTATE_120: u32 = 0x1000_0000;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Orientation {
    #[default]
    Orthogonal,
    Isometric,
    Staggered,
    Hexagonal,
}

/// The order an orthogonal map's tiles are drawn in, which matters only
/// when tiles are taller or wider than the grid.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum RenderOrder {
    #[default]
    RightDown,
    RightUp,
    LeftDown,
    LeftUp,
}

#[derive(Default)]
pub struct MapDoc {
    pub orientation: Orientation,
    pub render_order: RenderOrder,
    /// In tiles.
    pub width: u32,
    pub height: u32,
    /// The grid's cell size in pixels.
    pub tile_width: u32,
    pub tile_height: u32,
    pub hex_side: u32,
    pub stagger_x: bool,
    pub stagger_even: bool,
    pub infinite: bool,
    pub background: Option<Px>,
    pub tiled_version: Option<String>,
    pub tilesets: Vec<TilesetRef>,
    /// Bottom first, each group before its contents.
    pub layers: Vec<LayerDoc>,
    /// Layer data encodings met, for the inspector.
    pub encodings: BTreeSet<String>,
    pub warnings: Vec<String>,
}

impl MapDoc {
    /// A first global id beyond every tileset's, for tilesets only
    /// templates bring in.
    pub fn next_free_gid(&self) -> u32 {
        self.tilesets.iter().map(|t| t.first_gid.saturating_add(t.source.tile_span())).max().unwrap_or(1).max(1)
    }
}

#[derive(Clone)]
pub struct TilesetRef {
    pub first_gid: u32,
    pub source: TilesetSource,
}

#[derive(Clone)]
pub enum TilesetSource {
    /// A `.tsx` or `.tsj` file, its path already joined to the folder of
    /// the file that names it.
    External(PathBuf),
    Inline(Box<TilesetDoc>),
}

impl TilesetSource {
    pub fn name(&self) -> String {
        match self {
            TilesetSource::External(p) => p.file_stem().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
            TilesetSource::Inline(t) => t.name.clone(),
        }
    }

    /// How many ids the tileset may use, when known without reading it;
    /// otherwise a generous guess.
    pub fn tile_span(&self) -> u32 {
        match self {
            TilesetSource::Inline(t) => t.tile_count.unwrap_or(0).max(t.tiles.keys().max().map_or(0, |m| m + 1)).max(1),
            TilesetSource::External(_) => 1 << 16,
        }
    }

    /// Whether two references name the same tileset file.
    pub fn same_file(&self, other: &TilesetSource) -> bool {
        match (self, other) {
            (TilesetSource::External(a), TilesetSource::External(b)) => same_path(a, b),
            _ => false,
        }
    }
}

fn same_path(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(x), Ok(y)) => x == y,
        _ => a == b,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Alignment {
    /// Bottom left on orthogonal maps, bottom centre on isometric ones.
    #[default]
    Unspecified,
    TopLeft,
    Top,
    TopRight,
    Left,
    Center,
    Right,
    BottomLeft,
    Bottom,
    BottomRight,
}

impl Alignment {
    pub fn parse(s: &str) -> Alignment {
        match s {
            "topleft" => Alignment::TopLeft,
            "top" => Alignment::Top,
            "topright" => Alignment::TopRight,
            "left" => Alignment::Left,
            "center" => Alignment::Center,
            "right" => Alignment::Right,
            "bottomleft" => Alignment::BottomLeft,
            "bottom" => Alignment::Bottom,
            "bottomright" => Alignment::BottomRight,
            _ => Alignment::Unspecified,
        }
    }

    /// Where the anchor sits, as fractions of the width and height from the
    /// top left.
    pub fn fractions(self, isometric: bool) -> (f32, f32) {
        match self {
            Alignment::Unspecified if isometric => (0.5, 1.0),
            Alignment::Unspecified | Alignment::BottomLeft => (0.0, 1.0),
            Alignment::TopLeft => (0.0, 0.0),
            Alignment::Top => (0.5, 0.0),
            Alignment::TopRight => (1.0, 0.0),
            Alignment::Left => (0.0, 0.5),
            Alignment::Center => (0.5, 0.5),
            Alignment::Right => (1.0, 0.5),
            Alignment::Bottom => (0.5, 1.0),
            Alignment::BottomRight => (1.0, 1.0),
        }
    }
}

#[derive(Clone, Default)]
pub struct ImageRef {
    /// Relative to the tileset's or map's folder.
    pub source: String,
    /// A colour shown as transparent.
    pub trans: Option<[u8; 3]>,
}

#[derive(Clone, Default)]
pub struct TileDoc {
    /// An image collection's picture of this tile.
    pub image: Option<ImageRef>,
    /// The part of that picture to show: x, y, width, height.
    pub rect: Option<[u32; 4]>,
    /// An animated tile shows its first frame.
    pub first_frame: Option<u32>,
}

#[derive(Clone, Default)]
pub struct TilesetDoc {
    pub name: String,
    pub tile_width: u32,
    pub tile_height: u32,
    pub spacing: u32,
    pub margin: u32,
    pub columns: Option<u32>,
    pub tile_count: Option<u32>,
    /// Drawing offset of every tile, in pixels.
    pub offset: (f32, f32),
    /// The one picture of an atlas tileset.
    pub image: Option<ImageRef>,
    pub tiles: HashMap<u32, TileDoc>,
    pub alignment: Alignment,
    /// Tiles are stretched to the map's grid (`tilerendersize="grid"`).
    pub render_grid: bool,
    /// When stretched, keep the tile's proportions.
    pub preserve_aspect: bool,
    /// The folder the tileset's pictures are relative to.
    pub base_dir: PathBuf,
}

pub struct LayerDoc {
    pub name: String,
    /// As saved; a hidden group hides its contents too.
    pub visible: bool,
    /// Effective opacity, already multiplied by every group above it.
    pub opacity: f32,
    /// Effective offset in pixels, groups included.
    pub offset: (f32, f32),
    /// Effective tint, multiplied into the layer's colours.
    pub tint: Option<[f32; 4]>,
    pub depth: u32,
    pub parent: Option<usize>,
    pub kind: LayerKind,
}

pub enum LayerKind {
    Tiles(TileGrid),
    Objects { color: Option<Px>, objects: Vec<ObjectDoc> },
    Image { image: Option<ImageRef>, repeat_x: bool, repeat_y: bool },
    Group,
}

/// A tile layer's grid of global ids, row by row.  Infinite maps put
/// their chunks together into one grid starting at tile `x0`, `y0`.
#[derive(Default)]
pub struct TileGrid {
    pub x0: i32,
    pub y0: i32,
    pub width: u32,
    pub height: u32,
    pub gids: Vec<u32>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Shape {
    Rect,
    Ellipse,
    Point,
    Polygon(Vec<(f32, f32)>),
    Polyline(Vec<(f32, f32)>),
    Text,
}

/// An object; fields a template may supply are optional.
#[derive(Clone, Default)]
pub struct ObjectDoc {
    pub x: f32,
    pub y: f32,
    pub width: Option<f32>,
    pub height: Option<f32>,
    /// Degrees clockwise.
    pub rotation: Option<f32>,
    pub gid: Option<u32>,
    pub visible: Option<bool>,
    pub shape: Option<Shape>,
    pub template: Option<String>,
}

/// An object template: an object and the tileset its tile comes from.
pub struct TemplateDoc {
    pub tileset: Option<TilesetRef>,
    pub object: ObjectDoc,
}

/// Parses Tiled's `#AARRGGBB` or `#RRGGBB` colours (the `#` optional).
pub fn parse_color(s: &str) -> Option<Px> {
    let hex = s.trim().trim_start_matches('#');
    let v = u32::from_str_radix(hex, 16).ok()?;
    match hex.len() {
        6 => Some([(v >> 16) as u8, (v >> 8) as u8, v as u8, 255]),
        8 => Some([(v >> 16) as u8, (v >> 8) as u8, v as u8, (v >> 24) as u8]),
        _ => None,
    }
}

/// Parses `points="x,y x,y ..."`.
pub fn parse_points(s: &str) -> Vec<(f32, f32)> {
    s.split_whitespace()
        .filter_map(|p| {
            let (x, y) = p.split_once(',')?;
            Some((x.trim().parse().ok()?, y.trim().parse().ok()?))
        })
        .take(1 << 12)
        .collect()
}

/// Carries a group's opacity, offset, tint and visibility down to a layer
/// inside it.
pub struct Inherited {
    pub opacity: f32,
    pub offset: (f32, f32),
    pub tint: Option<[f32; 4]>,
    pub depth: u32,
    pub parent: Option<usize>,
}

impl Inherited {
    pub const TOP: Inherited = Inherited { opacity: 1.0, offset: (0.0, 0.0), tint: None, depth: 0, parent: None };

    /// A layer's effective values, from its own attributes.
    pub fn apply(&self, opacity: f32, offset: (f32, f32), tint: Option<Px>) -> (f32, (f32, f32), Option<[f32; 4]>) {
        let opacity = (self.opacity * opacity).clamp(0.0, 1.0);
        let offset = (self.offset.0 + offset.0, self.offset.1 + offset.1);
        let own = tint.map(|t| t.map(|c| c as f32 / 255.0));
        let tint = match (self.tint, own) {
            (Some(a), Some(b)) => Some([a[0] * b[0], a[1] * b[1], a[2] * b[2], a[3] * b[3]]),
            (a, b) => a.or(b),
        };
        (opacity, offset, tint)
    }
}

/// Groups nest at most this deep; deeper ones are skipped, so reading
/// never recurses without bound.
pub const MAX_NESTING: u32 = 32;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colours_and_points() {
        assert_eq!(parse_color("#ff8000"), Some([255, 128, 0, 255]));
        assert_eq!(parse_color("80ff8000"), Some([255, 128, 0, 128]));
        assert_eq!(parse_color("#12"), None);
        assert_eq!(parse_points("0,0 10.5,-2"), vec![(0.0, 0.0), (10.5, -2.0)]);
    }
}
