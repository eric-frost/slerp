//! TMX maps, TSX tilesets and TX templates.

use super::data;
use super::doc::*;
use slerp_scene::LoadError;
use std::path::Path;

type Node<'a, 'i> = roxmltree::Node<'a, 'i>;

fn parse(bytes: &[u8]) -> Result<roxmltree::Document<'_>, LoadError> {
    let text = std::str::from_utf8(bytes).map_err(|_| LoadError::invalid("the file isn't UTF-8 text"))?;
    let options = roxmltree::ParsingOptions { allow_dtd: true, nodes_limit: 1 << 24, ..Default::default() };
    roxmltree::Document::parse_with_options(text, options).map_err(|e| LoadError::invalid(e.to_string()))
}

fn attr<T: std::str::FromStr>(n: Node, name: &str) -> Option<T> {
    n.attribute(name).and_then(|v| v.trim().parse().ok())
}

fn attr_or<T: std::str::FromStr>(n: Node, name: &str, default: T) -> T {
    attr(n, name).unwrap_or(default)
}

fn child<'a, 'i>(n: Node<'a, 'i>, name: &str) -> Option<Node<'a, 'i>> {
    n.children().find(|c| c.has_tag_name(name))
}

/// Reads a `.tmx` map.  `budget` caps the tile ids read across layers.
pub fn map(bytes: &[u8], dir: &Path, budget: u64) -> Result<MapDoc, LoadError> {
    let xml = parse(bytes)?;
    let root = xml.root_element();
    if !root.has_tag_name("map") {
        return Err(LoadError::NotMine);
    }
    let mut doc = MapDoc {
        orientation: match root.attribute("orientation").unwrap_or("orthogonal") {
            "isometric" => Orientation::Isometric,
            "staggered" => Orientation::Staggered,
            "hexagonal" => Orientation::Hexagonal,
            "orthogonal" => Orientation::Orthogonal,
            o => return Err(LoadError::Unsupported(format!("{o} maps"))),
        },
        render_order: match root.attribute("renderorder").unwrap_or("") {
            "right-up" => RenderOrder::RightUp,
            "left-down" => RenderOrder::LeftDown,
            "left-up" => RenderOrder::LeftUp,
            _ => RenderOrder::RightDown,
        },
        width: attr_or(root, "width", 0),
        height: attr_or(root, "height", 0),
        tile_width: attr_or(root, "tilewidth", 0),
        tile_height: attr_or(root, "tileheight", 0),
        hex_side: attr_or(root, "hexsidelength", 0),
        stagger_x: root.attribute("staggeraxis") == Some("x"),
        stagger_even: root.attribute("staggerindex") == Some("even"),
        infinite: attr_or(root, "infinite", 0) != 0,
        background: root.attribute("backgroundcolor").and_then(parse_color),
        tiled_version: root.attribute("tiledversion").map(str::to_string),
        ..MapDoc::default()
    };
    for ts in root.children().filter(|c| c.has_tag_name("tileset")) {
        let first_gid = attr_or(ts, "firstgid", 1u32).max(1);
        let source = match ts.attribute("source") {
            Some(s) => TilesetSource::External(dir.join(s)),
            None => TilesetSource::Inline(Box::new(tileset(ts, dir))),
        };
        doc.tilesets.push(TilesetRef { first_gid, source });
    }
    doc.tilesets.sort_by_key(|t| t.first_gid);
    let mut budget = budget;
    layers(root, &mut doc, &Inherited::TOP, &mut budget);
    Ok(doc)
}

/// Reads the layers inside `parent` (the map or a group), in order.
fn layers(parent: Node, doc: &mut MapDoc, up: &Inherited, budget: &mut u64) {
    for n in parent.children().filter(|c| c.is_element()) {
        let kind = match n.tag_name().name() {
            "layer" => match tile_layer(n, doc, budget) {
                Ok(grid) => LayerKind::Tiles(grid),
                Err(e) => {
                    doc.warnings.push(format!("layer {:?}: {e}", n.attribute("name").unwrap_or("")));
                    LayerKind::Tiles(TileGrid::default())
                }
            },
            "objectgroup" => LayerKind::Objects {
                color: n.attribute("color").and_then(parse_color),
                objects: n.children().filter(|c| c.has_tag_name("object")).take(1 << 20).map(object).collect(),
            },
            "imagelayer" => LayerKind::Image {
                image: child(n, "image").map(|i| image(i)),
                repeat_x: attr_or(n, "repeatx", 0) != 0,
                repeat_y: attr_or(n, "repeaty", 0) != 0,
            },
            "group" => LayerKind::Group,
            _ => continue,
        };
        let offset = (attr_or(n, "offsetx", 0.0), attr_or(n, "offsety", 0.0));
        let tint = n.attribute("tintcolor").and_then(parse_color);
        let (opacity, offset, tint) = up.apply(attr_or(n, "opacity", 1.0), offset, tint);
        let group = matches!(kind, LayerKind::Group);
        doc.layers.push(LayerDoc {
            name: n.attribute("name").unwrap_or("").to_string(),
            visible: attr_or(n, "visible", 1) != 0,
            opacity,
            offset,
            tint,
            depth: up.depth,
            parent: up.parent,
            kind,
        });
        if group && up.depth < MAX_NESTING {
            let inner = Inherited { opacity, offset, tint, depth: up.depth + 1, parent: Some(doc.layers.len() - 1) };
            layers(n, doc, &inner, budget);
        }
    }
}

fn tile_layer(n: Node, doc: &mut MapDoc, budget: &mut u64) -> Result<TileGrid, LoadError> {
    let Some(data_node) = child(n, "data") else { return Ok(TileGrid::default()) };
    let encoding = data_node.attribute("encoding").unwrap_or("xml");
    let compression = data_node.attribute("compression").unwrap_or("");
    doc.encodings.insert(if compression.is_empty() {
        encoding.to_string()
    } else {
        format!("{encoding} {compression}")
    });
    let mut take = |count: u64| -> Result<usize, LoadError> {
        if count > *budget {
            return Err(LoadError::TooLarge(format!("{count} tiles")));
        }
        *budget -= count;
        Ok(count as usize)
    };
    let decode = |node: Node, count: usize| -> Result<Vec<u32>, LoadError> {
        if encoding == "xml" {
            let mut ids: Vec<u32> = node
                .children()
                .filter(|c| c.has_tag_name("tile"))
                .take(count)
                .map(|t| attr_or(t, "gid", 0u32))
                .collect();
            ids.resize(count, 0);
            Ok(ids)
        } else {
            data::decode(node.text().unwrap_or(""), encoding, compression, count)
        }
    };
    let chunks: Vec<Node> = data_node.children().filter(|c| c.has_tag_name("chunk")).collect();
    if chunks.is_empty() {
        let (w, h) = (attr_or(n, "width", doc.width), attr_or(n, "height", doc.height));
        let count = take(w as u64 * h as u64)?;
        let gids = decode(data_node, count)?;
        return Ok(TileGrid { x0: attr_or(n, "x", 0), y0: attr_or(n, "y", 0), width: w, height: h, gids });
    }
    // An infinite map's chunks, put together into one grid.
    let mut parts = Vec::new();
    for c in chunks {
        let (x, y, w, h) =
            (attr_or(c, "x", 0i32), attr_or(c, "y", 0i32), attr_or(c, "width", 0u32), attr_or(c, "height", 0u32));
        let count = take(w as u64 * h as u64)?;
        parts.push((x, y, w, h, decode(c, count)?));
    }
    merge_chunks(parts, budget)
}

/// Puts chunks at tile positions into one grid covering them all.
pub fn merge_chunks(parts: Vec<(i32, i32, u32, u32, Vec<u32>)>, budget: &mut u64) -> Result<TileGrid, LoadError> {
    let x0 = parts.iter().map(|p| p.0 as i64).min().unwrap_or(0);
    let y0 = parts.iter().map(|p| p.1 as i64).min().unwrap_or(0);
    let x1 = parts.iter().map(|p| p.0 as i64 + p.2 as i64).max().unwrap_or(0);
    let y1 = parts.iter().map(|p| p.1 as i64 + p.3 as i64).max().unwrap_or(0);
    let (w, h) = ((x1 - x0).max(0) as u64, (y1 - y0).max(0) as u64);
    let count = w * h;
    if count > *budget {
        return Err(LoadError::TooLarge(format!("chunks spanning {w} × {h} tiles")));
    }
    *budget -= count;
    let mut gids = vec![0u32; count as usize];
    for (x, y, cw, ch, ids) in parts {
        for row in 0..ch as u64 {
            let ty = (y as i64 - y0) as u64 + row;
            let tx = (x as i64 - x0) as u64;
            let src = (row * cw as u64) as usize;
            let dst = (ty * w + tx) as usize;
            gids[dst..dst + cw as usize].copy_from_slice(&ids[src..src + cw as usize]);
        }
    }
    Ok(TileGrid { x0: x0 as i32, y0: y0 as i32, width: w as u32, height: h as u32, gids })
}

fn image(n: Node) -> ImageRef {
    ImageRef {
        source: n.attribute("source").unwrap_or("").to_string(),
        trans: n.attribute("trans").and_then(parse_color).map(|c| [c[0], c[1], c[2]]),
    }
}

fn object(n: Node) -> ObjectDoc {
    let shape = if child(n, "ellipse").is_some() {
        Some(Shape::Ellipse)
    } else if child(n, "point").is_some() {
        Some(Shape::Point)
    } else if let Some(p) = child(n, "polygon") {
        Some(Shape::Polygon(parse_points(p.attribute("points").unwrap_or(""))))
    } else if let Some(p) = child(n, "polyline") {
        Some(Shape::Polyline(parse_points(p.attribute("points").unwrap_or(""))))
    } else if child(n, "text").is_some() {
        Some(Shape::Text)
    } else if n.attribute("gid").is_some() || n.attribute("template").is_some() {
        None
    } else {
        Some(Shape::Rect)
    };
    ObjectDoc {
        x: attr_or(n, "x", 0.0),
        y: attr_or(n, "y", 0.0),
        width: attr(n, "width"),
        height: attr(n, "height"),
        rotation: attr(n, "rotation"),
        gid: n.attribute("gid").and_then(|g| g.trim().parse::<u64>().ok()).map(|g| g as u32),
        visible: attr::<u8>(n, "visible").map(|v| v != 0),
        shape,
        template: n.attribute("template").map(str::to_string),
    }
}

/// A `<tileset>` element, embedded or the root of a `.tsx` file in `dir`.
fn tileset(n: Node, dir: &Path) -> TilesetDoc {
    let mut t = TilesetDoc {
        name: n.attribute("name").unwrap_or("").to_string(),
        tile_width: attr_or(n, "tilewidth", 0),
        tile_height: attr_or(n, "tileheight", 0),
        spacing: attr_or(n, "spacing", 0),
        margin: attr_or(n, "margin", 0),
        columns: attr(n, "columns"),
        tile_count: attr(n, "tilecount"),
        image: child(n, "image").map(|i| image(i)),
        alignment: Alignment::parse(n.attribute("objectalignment").unwrap_or("")),
        render_grid: n.attribute("tilerendersize") == Some("grid"),
        preserve_aspect: n.attribute("fillmode") == Some("preserve-aspect-fit"),
        base_dir: dir.to_path_buf(),
        ..TilesetDoc::default()
    };
    if let Some(o) = child(n, "tileoffset") {
        t.offset = (attr_or(o, "x", 0.0), attr_or(o, "y", 0.0));
    }
    for tile in n.children().filter(|c| c.has_tag_name("tile")).take(1 << 20) {
        let id: u32 = attr_or(tile, "id", 0);
        let image = child(tile, "image").map(|i| image(i));
        let rect = match (attr(tile, "width"), attr(tile, "height")) {
            (Some(w), Some(h)) => Some([attr_or(tile, "x", 0), attr_or(tile, "y", 0), w, h]),
            _ => None,
        };
        let first_frame =
            child(tile, "animation").and_then(|a| child(a, "frame")).and_then(|f| attr::<u32>(f, "tileid"));
        if image.is_some() || rect.is_some() || first_frame.is_some() {
            t.tiles.insert(id, TileDoc { image, rect, first_frame });
        }
    }
    t
}

/// Reads a `.tsx` file whose pictures are relative to `dir`.
pub fn tileset_file(bytes: &[u8], dir: &Path) -> Result<TilesetDoc, LoadError> {
    let xml = parse(bytes)?;
    let root = xml.root_element();
    if !root.has_tag_name("tileset") {
        return Err(LoadError::invalid("not a Tiled tileset"));
    }
    Ok(tileset(root, dir))
}

/// Reads a `.tx` object template in `dir`.
pub fn template(bytes: &[u8], dir: &Path) -> Result<TemplateDoc, LoadError> {
    let xml = parse(bytes)?;
    let root = xml.root_element();
    let o = child(root, "object").ok_or_else(|| LoadError::invalid("a template without an object"))?;
    let tileset = child(root, "tileset").map(|ts| TilesetRef {
        first_gid: attr_or(ts, "firstgid", 1u32).max(1),
        source: match ts.attribute("source") {
            Some(s) => TilesetSource::External(dir.join(s)),
            None => TilesetSource::Inline(Box::new(tileset(ts, dir))),
        },
    });
    Ok(TemplateDoc { tileset, object: object(o) })
}
