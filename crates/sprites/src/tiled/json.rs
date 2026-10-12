//! Tiled's JSON formats: `.tmj` maps, `.tsj` tilesets and `.tj` templates.

use super::data;
use super::doc::*;
use super::xml::merge_chunks;
use serde_json::Value;
use slerp_scene::LoadError;
use std::path::Path;

fn parse(bytes: &[u8]) -> Result<Value, LoadError> {
    serde_json::from_slice(bytes).map_err(|e| LoadError::invalid(format!("JSON: {e}")))
}

fn num(v: &Value, key: &str) -> Option<f64> {
    v.get(key).and_then(Value::as_f64)
}

fn uint(v: &Value, key: &str) -> Option<u32> {
    v.get(key).and_then(Value::as_u64).map(|n| n.min(u32::MAX as u64) as u32)
}

fn int(v: &Value, key: &str) -> i32 {
    v.get(key).and_then(Value::as_i64).map_or(0, |n| n.clamp(i32::MIN as i64, i32::MAX as i64) as i32)
}

fn text<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key).and_then(Value::as_str).unwrap_or("")
}

fn flag(v: &Value, key: &str) -> Option<bool> {
    v.get(key).and_then(Value::as_bool)
}

fn array<'a>(v: &'a Value, key: &str) -> &'a [Value] {
    v.get(key).and_then(Value::as_array).map_or(&[], Vec::as_slice)
}

/// Reads a `.tmj` map.  `budget` caps the tile ids read across layers.
pub fn map(bytes: &[u8], dir: &Path, budget: u64) -> Result<MapDoc, LoadError> {
    let root = parse(bytes)?;
    if !root.is_object() || root.get("layers").is_none() {
        return Err(LoadError::invalid("not a Tiled map"));
    }
    let mut doc = MapDoc {
        orientation: match text(&root, "orientation") {
            "isometric" => Orientation::Isometric,
            "staggered" => Orientation::Staggered,
            "hexagonal" => Orientation::Hexagonal,
            "orthogonal" | "" => Orientation::Orthogonal,
            o => return Err(LoadError::Unsupported(format!("{o} maps"))),
        },
        render_order: match text(&root, "renderorder") {
            "right-up" => RenderOrder::RightUp,
            "left-down" => RenderOrder::LeftDown,
            "left-up" => RenderOrder::LeftUp,
            _ => RenderOrder::RightDown,
        },
        width: uint(&root, "width").unwrap_or(0),
        height: uint(&root, "height").unwrap_or(0),
        tile_width: uint(&root, "tilewidth").unwrap_or(0),
        tile_height: uint(&root, "tileheight").unwrap_or(0),
        hex_side: uint(&root, "hexsidelength").unwrap_or(0),
        stagger_x: text(&root, "staggeraxis") == "x",
        stagger_even: text(&root, "staggerindex") == "even",
        infinite: flag(&root, "infinite").unwrap_or(false),
        background: parse_color(text(&root, "backgroundcolor")),
        tiled_version: root.get("tiledversion").and_then(Value::as_str).map(str::to_string),
        ..MapDoc::default()
    };
    for ts in array(&root, "tilesets") {
        if let Some(r) = tileset_ref(ts, dir) {
            doc.tilesets.push(r);
        }
    }
    doc.tilesets.sort_by_key(|t| t.first_gid);
    let mut budget = budget;
    layers(array(&root, "layers"), &mut doc, &Inherited::TOP, &mut budget);
    Ok(doc)
}

fn tileset_ref(ts: &Value, dir: &Path) -> Option<TilesetRef> {
    let first_gid = uint(ts, "firstgid").unwrap_or(1).max(1);
    let source = match ts.get("source").and_then(Value::as_str) {
        Some(s) => TilesetSource::External(dir.join(s)),
        None if ts.is_object() => TilesetSource::Inline(Box::new(tileset(ts, dir))),
        None => return None,
    };
    Some(TilesetRef { first_gid, source })
}

fn layers(list: &[Value], doc: &mut MapDoc, up: &Inherited, budget: &mut u64) {
    for l in list {
        let kind = match text(l, "type") {
            "tilelayer" => match tile_layer(l, doc, budget) {
                Ok(grid) => LayerKind::Tiles(grid),
                Err(e) => {
                    doc.warnings.push(format!("layer {:?}: {e}", text(l, "name")));
                    LayerKind::Tiles(TileGrid::default())
                }
            },
            "objectgroup" => LayerKind::Objects {
                color: parse_color(text(l, "color")),
                objects: array(l, "objects").iter().take(1 << 20).map(object).collect(),
            },
            "imagelayer" => LayerKind::Image {
                image: Some(ImageRef {
                    source: text(l, "image").to_string(),
                    trans: parse_color(text(l, "transparentcolor")).map(|c| [c[0], c[1], c[2]]),
                })
                .filter(|i| !i.source.is_empty()),
                repeat_x: flag(l, "repeatx").unwrap_or(false),
                repeat_y: flag(l, "repeaty").unwrap_or(false),
            },
            "group" => LayerKind::Group,
            _ => continue,
        };
        let offset = (num(l, "offsetx").unwrap_or(0.0) as f32, num(l, "offsety").unwrap_or(0.0) as f32);
        let tint = parse_color(text(l, "tintcolor"));
        let (opacity, offset, tint) = up.apply(num(l, "opacity").unwrap_or(1.0) as f32, offset, tint);
        let group = matches!(kind, LayerKind::Group);
        doc.layers.push(LayerDoc {
            name: text(l, "name").to_string(),
            visible: flag(l, "visible").unwrap_or(true),
            opacity,
            offset,
            tint,
            depth: up.depth,
            parent: up.parent,
            kind,
        });
        if group && up.depth < MAX_NESTING {
            let inner = Inherited { opacity, offset, tint, depth: up.depth + 1, parent: Some(doc.layers.len() - 1) };
            layers(array(l, "layers"), doc, &inner, budget);
        }
    }
}

fn tile_layer(l: &Value, doc: &mut MapDoc, budget: &mut u64) -> Result<TileGrid, LoadError> {
    let encoding = match text(l, "encoding") {
        "" => "csv",
        e => e,
    };
    let compression = text(l, "compression");
    doc.encodings.insert(match (encoding, compression) {
        ("csv", _) => "JSON array".to_string(),
        (e, "") => e.to_string(),
        (e, c) => format!("{e} {c}"),
    });
    let mut take = |count: u64| -> Result<usize, LoadError> {
        if count > *budget {
            return Err(LoadError::TooLarge(format!("{count} tiles")));
        }
        *budget -= count;
        Ok(count as usize)
    };
    let decode = |data: Option<&Value>, count: usize| -> Result<Vec<u32>, LoadError> {
        match data {
            Some(Value::Array(ids)) => {
                let mut v: Vec<u32> = ids.iter().take(count).map(|n| n.as_u64().unwrap_or(0) as u32).collect();
                v.resize(count, 0);
                Ok(v)
            }
            Some(Value::String(s)) => data::decode(s, "base64", compression, count),
            _ => Ok(vec![0; count]),
        }
    };
    let chunks = array(l, "chunks");
    if chunks.is_empty() {
        let w = uint(l, "width").unwrap_or(doc.width);
        let h = uint(l, "height").unwrap_or(doc.height);
        let count = take(w as u64 * h as u64)?;
        let gids = decode(l.get("data"), count)?;
        return Ok(TileGrid { x0: int(l, "x"), y0: int(l, "y"), width: w, height: h, gids });
    }
    let mut parts = Vec::new();
    for c in chunks {
        let (w, h) = (uint(c, "width").unwrap_or(0), uint(c, "height").unwrap_or(0));
        let count = take(w as u64 * h as u64)?;
        parts.push((int(c, "x"), int(c, "y"), w, h, decode(c.get("data"), count)?));
    }
    merge_chunks(parts, budget)
}

fn points(v: &Value) -> Vec<(f32, f32)> {
    v.as_array()
        .map(|a| {
            a.iter()
                .take(1 << 12)
                .map(|p| (num(p, "x").unwrap_or(0.0) as f32, num(p, "y").unwrap_or(0.0) as f32))
                .collect()
        })
        .unwrap_or_default()
}

fn object(o: &Value) -> ObjectDoc {
    let shape = if flag(o, "ellipse") == Some(true) {
        Some(Shape::Ellipse)
    } else if flag(o, "point") == Some(true) {
        Some(Shape::Point)
    } else if let Some(p) = o.get("polygon") {
        Some(Shape::Polygon(points(p)))
    } else if let Some(p) = o.get("polyline") {
        Some(Shape::Polyline(points(p)))
    } else if o.get("text").is_some() {
        Some(Shape::Text)
    } else if o.get("gid").is_some() || o.get("template").is_some() {
        None
    } else {
        Some(Shape::Rect)
    };
    ObjectDoc {
        x: num(o, "x").unwrap_or(0.0) as f32,
        y: num(o, "y").unwrap_or(0.0) as f32,
        width: num(o, "width").map(|v| v as f32),
        height: num(o, "height").map(|v| v as f32),
        rotation: num(o, "rotation").map(|v| v as f32),
        gid: o.get("gid").and_then(Value::as_u64).map(|g| g as u32),
        visible: flag(o, "visible"),
        shape,
        template: o.get("template").and_then(Value::as_str).map(str::to_string),
    }
}

/// A tileset object, embedded in a map or the root of a `.tsj` file whose
/// pictures are relative to `dir`.
fn tileset(t: &Value, dir: &Path) -> TilesetDoc {
    let image = Some(ImageRef {
        source: text(t, "image").to_string(),
        trans: parse_color(text(t, "transparentcolor")).map(|c| [c[0], c[1], c[2]]),
    })
    .filter(|i| !i.source.is_empty());
    let mut doc = TilesetDoc {
        name: text(t, "name").to_string(),
        tile_width: uint(t, "tilewidth").unwrap_or(0),
        tile_height: uint(t, "tileheight").unwrap_or(0),
        spacing: uint(t, "spacing").unwrap_or(0),
        margin: uint(t, "margin").unwrap_or(0),
        columns: uint(t, "columns"),
        tile_count: uint(t, "tilecount"),
        image,
        alignment: Alignment::parse(text(t, "objectalignment")),
        render_grid: text(t, "tilerendersize") == "grid",
        preserve_aspect: text(t, "fillmode") == "preserve-aspect-fit",
        base_dir: dir.to_path_buf(),
        ..TilesetDoc::default()
    };
    if let Some(o) = t.get("tileoffset") {
        doc.offset = (num(o, "x").unwrap_or(0.0) as f32, num(o, "y").unwrap_or(0.0) as f32);
    }
    for tile in array(t, "tiles").iter().take(1 << 20) {
        let Some(id) = uint(tile, "id") else { continue };
        let image =
            Some(ImageRef { source: text(tile, "image").to_string(), trans: None }).filter(|i| !i.source.is_empty());
        let rect = match (uint(tile, "width"), uint(tile, "height")) {
            (Some(w), Some(h)) => Some([uint(tile, "x").unwrap_or(0), uint(tile, "y").unwrap_or(0), w, h]),
            _ => None,
        };
        let first_frame = array(tile, "animation").first().and_then(|f| uint(f, "tileid"));
        if image.is_some() || rect.is_some() || first_frame.is_some() {
            doc.tiles.insert(id, TileDoc { image, rect, first_frame });
        }
    }
    doc
}

/// Reads a `.tsj` file whose pictures are relative to `dir`.
pub fn tileset_file(bytes: &[u8], dir: &Path) -> Result<TilesetDoc, LoadError> {
    let root = parse(bytes)?;
    if !root.is_object() {
        return Err(LoadError::invalid("not a Tiled tileset"));
    }
    Ok(tileset(&root, dir))
}

/// Reads a `.tj` object template in `dir`.
pub fn template(bytes: &[u8], dir: &Path) -> Result<TemplateDoc, LoadError> {
    let root = parse(bytes)?;
    let o = root.get("object").ok_or_else(|| LoadError::invalid("a template without an object"))?;
    let tileset = root.get("tileset").and_then(|t| tileset_ref(t, dir));
    Ok(TemplateDoc { tileset, object: object(o) })
}
