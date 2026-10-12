//! Drawing a map: where each orientation puts its tiles, and painting
//! tiles, coloured cells, objects and image layers into a picture at any
//! scale, sampling pictures at pixel centres so tiles keep hard edges.

use super::doc::*;
use super::{Map, Tileset};
use crate::blend::Px;
use crate::pixels::MAX_PIXELS;
use crate::raster::{Raster, cell_color};
use slerp_scene::Image8;

/*┌─────────────────╮
| Geometry        |
└─────────────────╯*/
//region Geometry

/// Where an orientation puts things, following Tiled's renderers.
pub struct Geometry {
    orientation: Orientation,
    tw: f32,
    th: f32,
    side_x: f32,
    side_y: f32,
    side_off_x: f32,
    side_off_y: f32,
    col_w: f32,
    row_h: f32,
    stagger_x: bool,
    stagger_even: bool,
    /// The map's first tile, which infinite maps put anywhere.
    ox: i32,
    oy: i32,
    /// In tiles.
    width: i32,
    height: i32,
    /// Where an isometric map's tile (0, 0) has its top corner.
    origin_x: f32,
    /// The picture's area in map pixels: left, top, width, height.  It is
    /// the map plus room for tiles taller or wider than the grid.
    pub bounds: [f32; 4],
}

impl Geometry {
    pub fn new(doc: &MapDoc, tilesets: &[Tileset]) -> Geometry {
        // 1. The tiles the map covers.  Infinite maps cover their chunks.
        let (mut ox, mut oy, mut width, mut height) = (0i32, 0i32, doc.width as i32, doc.height as i32);
        if doc.infinite {
            let grids = doc.layers.iter().filter_map(|l| match &l.kind {
                LayerKind::Tiles(g) if g.width > 0 && g.height > 0 => Some(g),
                _ => None,
            });
            let (mut x0, mut y0, mut x1, mut y1) = (i64::MAX, i64::MAX, i64::MIN, i64::MIN);
            for g in grids {
                x0 = x0.min(g.x0 as i64);
                y0 = y0.min(g.y0 as i64);
                x1 = x1.max(g.x0 as i64 + g.width as i64);
                y1 = y1.max(g.y0 as i64 + g.height as i64);
            }
            if x0 < x1 && y0 < y1 {
                (ox, oy) = (x0 as i32, y0 as i32);
                (width, height) = ((x1 - x0).min(i32::MAX as i64) as i32, (y1 - y0).min(i32::MAX as i64) as i32);
            }
        }
        // 2. Cell sizes, as Tiled's hexagonal renderer works them out
        //    (staggered maps are hexagonal ones with no sides).
        let orientation = doc.orientation;
        let hexlike = matches!(orientation, Orientation::Staggered | Orientation::Hexagonal);
        let (tw, th) = if hexlike {
            ((doc.tile_width & !1) as f32, (doc.tile_height & !1) as f32)
        } else {
            (doc.tile_width as f32, doc.tile_height as f32)
        };
        let side = if orientation == Orientation::Hexagonal { doc.hex_side as f32 } else { 0.0 };
        let (side_x, side_y) = if doc.stagger_x { (side, 0.0) } else { (0.0, side) };
        let side_off_x = ((tw - side_x) / 2.0).floor();
        let side_off_y = ((th - side_y) / 2.0).floor();
        // Shifting an infinite map's origin by an odd row or column swaps
        // which ones are staggered.
        let shift = if doc.stagger_x { ox } else { oy };
        let stagger_even = doc.stagger_even ^ (shift.rem_euclid(2) == 1);
        let mut g = Geometry {
            orientation,
            tw,
            th,
            side_x,
            side_y,
            side_off_x,
            side_off_y,
            col_w: side_off_x + side_x,
            row_h: side_off_y + side_y,
            stagger_x: doc.stagger_x,
            stagger_even,
            ox,
            oy,
            width,
            height,
            origin_x: height as f32 * tw / 2.0,
            bounds: [0.0; 4],
        };
        // 3. The map's own rectangle.
        let (w, h) = (width as f32, height as f32);
        let (mw, mh) = match orientation {
            Orientation::Orthogonal => (w * tw, h * th),
            Orientation::Isometric => ((w + h) * tw / 2.0, (w + h) * th / 2.0),
            _ if g.stagger_x => {
                (w * g.col_w + g.side_off_x, h * (th + g.side_y) + if width > 1 { g.row_h } else { 0.0 })
            }
            _ => (w * (tw + g.side_x) + if height > 1 { g.col_w } else { 0.0 }, h * g.row_h + g.side_off_y),
        };
        // 4. Room for tiles that overhang their cells.
        let (mut up, mut right, mut left, mut down) = (0.0f32, 0.0f32, 0.0f32, 0.0f32);
        for t in tilesets {
            let d = &t.doc;
            if t.largest == (0, 0) {
                continue;
            }
            let (tw_max, th_max) = if d.render_grid { (tw, th) } else { (t.largest.0 as f32, t.largest.1 as f32) };
            up = up.max(th_max - th - d.offset.1.min(0.0)).max(-d.offset.1);
            right = right.max(tw_max - tw + d.offset.0.max(0.0));
            left = left.max(-d.offset.0);
            down = down.max(d.offset.1);
        }
        // Image collections can hold huge pictures; keep the margins sane.
        let cap = |v: f32, side: f32| v.clamp(0.0, side.max(64.0) * 4.0);
        let (up, right, left, down) = (cap(up, mh), cap(right, mw), cap(left, mw), cap(down, mh));
        g.bounds = [-left, -up, mw + left + right, mh + up + down];
        g
    }

    /// The picture's size in pixels at full scale.
    pub fn size(&self) -> (u32, u32) {
        let clamp = |v: f32| v.ceil().clamp(0.0, u32::MAX as f32) as u32;
        (clamp(self.bounds[2]), clamp(self.bounds[3]))
    }

    fn staggered(&self, i: i32) -> bool {
        ((i & 1) == 1) ^ self.stagger_even
    }

    /// The top left of tile `x`, `y`'s cell (relative to the map's first
    /// tile), or for isometric maps the top corner of its diamond.
    fn cell(&self, x: i32, y: i32) -> (f32, f32) {
        let (xf, yf) = (x as f32, y as f32);
        match self.orientation {
            Orientation::Orthogonal => (xf * self.tw, yf * self.th),
            Orientation::Isometric => ((xf - yf) * self.tw / 2.0 + self.origin_x, (xf + yf) * self.th / 2.0),
            _ if self.stagger_x => {
                let dy = if self.staggered(x) { self.row_h } else { 0.0 };
                (xf * self.col_w, yf * (self.th + self.side_y) + dy)
            }
            _ => {
                let dx = if self.staggered(y) { self.col_w } else { 0.0 };
                (xf * (self.tw + self.side_x) + dx, yf * self.row_h)
            }
        }
    }

    /// The point tile images hang from: their bottom left corner.
    fn anchor(&self, x: i32, y: i32) -> (f32, f32) {
        let (cx, cy) = self.cell(x, y);
        match self.orientation {
            Orientation::Isometric => (cx - self.tw / 2.0, cy + self.th),
            _ => (cx, cy + self.th),
        }
    }

    /// The outline of tile `x`, `y`'s cell.
    fn outline(&self, x: i32, y: i32) -> Vec<(f32, f32)> {
        let (cx, cy) = self.cell(x, y);
        let (tw, th) = (self.tw, self.th);
        match self.orientation {
            Orientation::Orthogonal => vec![(cx, cy), (cx + tw, cy), (cx + tw, cy + th), (cx, cy + th)],
            Orientation::Isometric => {
                vec![(cx, cy), (cx + tw / 2.0, cy + th / 2.0), (cx, cy + th), (cx - tw / 2.0, cy + th / 2.0)]
            }
            _ if self.stagger_x => {
                let (a, s) = (self.side_off_x, self.side_x);
                vec![
                    (cx + a, cy),
                    (cx + a + s, cy),
                    (cx + tw, cy + th / 2.0),
                    (cx + a + s, cy + th),
                    (cx + a, cy + th),
                    (cx, cy + th / 2.0),
                ]
            }
            _ => {
                let (a, s) = (self.side_off_y, self.side_y);
                vec![
                    (cx + tw / 2.0, cy),
                    (cx + tw, cy + a),
                    (cx + tw, cy + a + s),
                    (cx + tw / 2.0, cy + th),
                    (cx, cy + a + s),
                    (cx, cy + a),
                ]
            }
        }
    }

    /// An object's position (in Tiled's object coordinates) on the map.
    fn object_point(&self, x: f32, y: f32) -> (f32, f32) {
        match self.orientation {
            Orientation::Orthogonal => (x - self.ox as f32 * self.tw, y - self.oy as f32 * self.th),
            Orientation::Isometric => {
                // Isometric object coordinates measure both axes in tile
                // heights along the grid.
                let tx = x / self.th - self.ox as f32;
                let ty = y / self.th - self.oy as f32;
                ((tx - ty) * self.tw / 2.0 + self.origin_x, (tx + ty) * self.th / 2.0)
            }
            _ if self.stagger_x => (x - self.ox as f32 * self.col_w, y - self.oy as f32 * (self.th + self.side_y)),
            _ => (x - self.ox as f32 * (self.tw + self.side_x), y - self.oy as f32 * self.row_h),
        }
    }

    /// The tiles from `x0` to `x1` and `y0` to `y1` (exclusive, relative to
    /// the map's first tile) in drawing order: back to front, as Tiled
    /// draws them.
    fn order(&self, render: RenderOrder, [x0, y0, x1, y1]: [i32; 4]) -> Box<dyn Iterator<Item = (i32, i32)> + '_> {
        if x0 >= x1 || y0 >= y1 {
            return Box::new(std::iter::empty());
        }
        match self.orientation {
            Orientation::Orthogonal => {
                let rows: Box<dyn Iterator<Item = i32>> = match render {
                    RenderOrder::RightUp | RenderOrder::LeftUp => Box::new((y0..y1).rev()),
                    _ => Box::new(y0..y1),
                };
                let left = matches!(render, RenderOrder::LeftDown | RenderOrder::LeftUp);
                Box::new(rows.flat_map(move |y| {
                    let cols: Box<dyn Iterator<Item = i32>> =
                        if left { Box::new((x0..x1).rev()) } else { Box::new(x0..x1) };
                    cols.map(move |x| (x, y))
                }))
            }
            // Diagonal rows of the diamond, from the top corner down.
            Orientation::Isometric => Box::new((x0 + y0..x1 + y1 - 1).flat_map(move |d| {
                let lo = x0.max(d - (y1 - 1));
                let hi = (x1 - 1).min(d - y0);
                (lo..=hi).map(move |x| (x, d - x))
            })),
            _ if self.stagger_x => Box::new((y0..y1).flat_map(move |y| {
                let up = (x0..x1).filter(move |&x| !self.staggered(x));
                let down = (x0..x1).filter(move |&x| self.staggered(x));
                up.chain(down).map(move |x| (x, y))
            })),
            _ => Box::new((y0..y1).flat_map(move |y| (x0..x1).map(move |x| (x, y)))),
        }
    }
}

//endregion

/*┌─────────────────╮
| Composing       |
└─────────────────╯*/
//region Composing

/// A layer's look: opacity (0..=255) and tint.
#[derive(Clone, Copy)]
struct Paint {
    opacity: i32,
    tint: Option<[f32; 4]>,
}

impl Paint {
    #[inline]
    fn tinted(&self, p: Px) -> Px {
        match self.tint {
            None => p,
            Some(t) => [
                (p[0] as f32 * t[0] + 0.5) as u8,
                (p[1] as f32 * t[1] + 0.5) as u8,
                (p[2] as f32 * t[2] + 0.5) as u8,
                (p[3] as f32 * t[3] + 0.5) as u8,
            ],
        }
    }
}

pub fn compose(map: &Map, shown: &[bool], scale: f32) -> Image8 {
    let g = &map.geometry;
    let [left, top, bw, bh] = g.bounds;
    let fits = (MAX_PIXELS as f64 / (bw.max(1.0) as f64 * bh.max(1.0) as f64)).sqrt() as f32;
    let scale = if scale.is_finite() && scale > 0.0 { scale.min(1.0).min(fits) } else { 1.0f32.min(fits) };
    let width = ((bw * scale).ceil() as u32).max(1);
    let height = ((bh * scale).ceil() as u32).max(1);
    let mut r = Raster { width, height, scale, left, top, pixels: vec![0; width as usize * height as usize * 4] };
    if let Some(bg) = map.doc.background {
        for p in r.pixels.chunks_exact_mut(4) {
            p.copy_from_slice(&bg);
        }
    }
    // A layer shows when it and every group above it do.
    let mut visible: Vec<bool> = Vec::with_capacity(map.doc.layers.len());
    for (i, l) in map.doc.layers.iter().enumerate() {
        let own = shown.get(i).copied().unwrap_or(l.visible);
        visible.push(own && l.parent.is_none_or(|p| p < i && visible[p]));
    }
    for (i, layer) in map.doc.layers.iter().enumerate() {
        if !visible[i] || layer.opacity <= 0.0 {
            continue;
        }
        let paint = Paint { opacity: (layer.opacity * 255.0 + 0.5) as i32, tint: layer.tint };
        match &layer.kind {
            LayerKind::Tiles(grid) => draw_tiles(map, &mut r, grid, layer.offset, paint),
            LayerKind::Objects { color, objects } => draw_objects(map, &mut r, objects, *color, layer.offset, paint),
            LayerKind::Image { repeat_x, repeat_y, .. } => {
                if let Some(img) = map.layer_images.get(&i) {
                    draw_image_layer(&mut r, img, layer.offset, *repeat_x, *repeat_y, paint);
                }
            }
            LayerKind::Group => {}
        }
    }
    Image8 { width, height, pixels: r.pixels }
}

fn draw_tiles(map: &Map, r: &mut Raster, grid: &TileGrid, offset: (f32, f32), paint: Paint) {
    let g = &map.geometry;
    // Only where the layer's grid and the map overlap.
    let clamp = |v: i64| v.clamp(0, i32::MAX as i64) as i32;
    let rect = [
        clamp(grid.x0 as i64 - g.ox as i64),
        clamp(grid.y0 as i64 - g.oy as i64),
        clamp(grid.x0 as i64 - g.ox as i64 + grid.width as i64).min(g.width),
        clamp(grid.y0 as i64 - g.oy as i64 + grid.height as i64).min(g.height),
    ];
    for (x, y) in g.order(map.doc.render_order, rect) {
        // The grid's own tile at this map position.
        let gx = x as i64 + g.ox as i64 - grid.x0 as i64;
        let gy = y as i64 + g.oy as i64 - grid.y0 as i64;
        if gx < 0 || gy < 0 || gx >= grid.width as i64 || gy >= grid.height as i64 {
            continue;
        }
        let value = grid.gids[(gy * grid.width as i64 + gx) as usize];
        let gid = value & GID_MASK;
        if gid == 0 {
            continue;
        }
        let Some((si, id)) = map.tileset_of(gid) else {
            let outline: Vec<_> =
                g.outline(x, y).into_iter().map(|p| r.to_raster((p.0 + offset.0, p.1 + offset.1))).collect();
            r.fill(&outline, [128, 128, 128, 255], paint.opacity);
            continue;
        };
        let set = &map.tilesets[si];
        let anchor = g.anchor(x, y);
        let anchor = (anchor.0 + offset.0, anchor.1 + offset.1);
        match set.tile(id) {
            Some((img, rect)) => {
                let (dw, dh) = draw_size(set, rect, g);
                let at = (anchor.0 + set.doc.offset.0, anchor.1 + set.doc.offset.1);
                let hex = map.doc.orientation == Orientation::Hexagonal;
                if hex && value & (FLIP_DIAGONAL | ROTATE_120) != 0 {
                    // Turned about the tile's centre, flipped first.
                    let turns = (value & FLIP_DIAGONAL != 0) as u32 + 2 * (value & ROTATE_120 != 0) as u32;
                    let theta = (60.0 * turns as f32).to_radians();
                    let centre = (at.0 + dw / 2.0, at.1 - dh / 2.0);
                    let flips = value & (FLIP_X | FLIP_Y);
                    draw_object_tile(r, img, rect, centre, (dw, dh), (0.5, 0.5), theta, flips, paint);
                } else {
                    draw_tile(r, img, rect, at, dw, dh, value, paint);
                }
            }
            None => {
                let outline: Vec<_> =
                    g.outline(x, y).into_iter().map(|p| r.to_raster((p.0 + offset.0, p.1 + offset.1))).collect();
                r.fill(&outline, paint.tinted(cell_color(si, id)), paint.opacity);
            }
        }
    }
}

/// The size a tile is drawn at: its own, or stretched to the grid.
fn draw_size(set: &Tileset, rect: [u32; 4], g: &Geometry) -> (f32, f32) {
    let (w, h) = (rect[2] as f32, rect[3] as f32);
    if !set.doc.render_grid || w <= 0.0 || h <= 0.0 {
        return (w, h);
    }
    if set.doc.preserve_aspect {
        let k = (g.tw / w).min(g.th / h);
        (w * k, h * k)
    } else {
        (g.tw, g.th)
    }
}

/// Draws a tile's picture `w` x `h` with its bottom left at `at`, flipped
/// as `flags` (a global id's top bits) say: diagonally first, then
/// horizontally, then vertically.  A diagonal flip turns the picture on
/// its side about its centre.
#[allow(clippy::too_many_arguments)]
fn draw_tile(r: &mut Raster, img: &Image8, rect: [u32; 4], at: (f32, f32), w: f32, h: f32, flags: u32, paint: Paint) {
    let diagonal = flags & FLIP_DIAGONAL != 0;
    let (bw, bh) = if diagonal { (h, w) } else { (w, h) };
    let (x0, y0) = (at.0 + (w - bw) / 2.0, at.1 - h + (h - bh) / 2.0);
    let (fx, fy) = (flags & FLIP_X != 0, flags & FLIP_Y != 0);
    for ry in r.rows(y0, y0 + bh) {
        let mut v = (r.map_y(ry) - y0) / bh;
        if fy {
            v = 1.0 - v;
        }
        for rx in r.cols(x0, x0 + bw) {
            let mut u = (r.map_x(rx) - x0) / bw;
            if fx {
                u = 1.0 - u;
            }
            let (su, sv) = if diagonal { (v, u) } else { (u, v) };
            let sx = rect[0] + ((su * rect[2] as f32) as u32).min(rect[2].saturating_sub(1));
            let sy = rect[1] + ((sv * rect[3] as f32) as u32).min(rect[3].saturating_sub(1));
            if sx < img.width && sy < img.height {
                let p = paint.tinted(crate::pixels::get(img, sx, sy));
                r.put(rx, ry, p, paint.opacity);
            }
        }
    }
}

fn draw_image_layer(r: &mut Raster, img: &Image8, offset: (f32, f32), repeat_x: bool, repeat_y: bool, paint: Paint) {
    if img.width == 0 || img.height == 0 {
        return;
    }
    let (w, h) = (img.width as f32, img.height as f32);
    for ry in 0..r.height {
        let my = r.map_y(ry) - offset.1;
        let v = if repeat_y { my.rem_euclid(h) } else { my };
        if v < 0.0 || v >= h {
            continue;
        }
        for rx in 0..r.width {
            let mx = r.map_x(rx) - offset.0;
            let u = if repeat_x { mx.rem_euclid(w) } else { mx };
            if u < 0.0 || u >= w {
                continue;
            }
            let p = paint.tinted(crate::pixels::get(img, u as u32, v as u32));
            r.put(rx, ry, p, paint.opacity);
        }
    }
}

fn draw_objects(map: &Map, r: &mut Raster, objects: &[ObjectDoc], color: Option<Px>, offset: (f32, f32), paint: Paint) {
    let g = &map.geometry;
    let color = paint.tinted(color.unwrap_or([160, 160, 164, 255]));
    let fill = [color[0], color[1], color[2], (color[3] as u32 * 64 / 255) as u8];
    let stroke = (1.5 * r.scale).max(1.25);
    let iso = map.doc.orientation == Orientation::Isometric;
    for o in objects {
        if o.visible == Some(false) {
            continue;
        }
        let (w, h) = (o.width.unwrap_or(0.0), o.height.unwrap_or(0.0));
        let theta = o.rotation.unwrap_or(0.0).to_radians();
        let (sin, cos) = theta.sin_cos();
        // A point relative to the object's position, turned with it, onto
        // the map and the raster.
        let place = |dx: f32, dy: f32| {
            let (rx, ry) = (dx * cos - dy * sin, dx * sin + dy * cos);
            let p = g.object_point(o.x + rx, o.y + ry);
            r.to_raster((p.0 + offset.0, p.1 + offset.1))
        };
        // 1. Tile objects draw their tile, stretched to the object's size.
        if let Some(value) = o.gid {
            let found = map.tileset_of(value & GID_MASK);
            let tile = found.and_then(|(si, id)| map.tilesets[si].tile(id).map(|t| (si, id, t)));
            let (si, id) = found.unwrap_or((0, value & GID_MASK));
            let align = found.map_or(Alignment::Unspecified, |(si, _)| map.tilesets[si].doc.alignment);
            let (ax, ay) = align.fractions(iso);
            let size = match tile {
                Some((_, _, (_, rect))) => (rect[2] as f32, rect[3] as f32),
                None => found.map_or((g.tw, g.th), |(si, id)| {
                    let (a, b) = map.tilesets[si].tile_size(id);
                    (a as f32, b as f32)
                }),
            };
            let (w, h) = (if w > 0.0 { w } else { size.0 }, if h > 0.0 { h } else { size.1 });
            match tile {
                Some((_, _, (img, rect))) => {
                    let anchor = g.object_point(o.x, o.y);
                    let anchor = (anchor.0 + offset.0, anchor.1 + offset.1);
                    draw_object_tile(r, img, rect, anchor, (w, h), (ax, ay), theta, value, paint);
                }
                None => {
                    let corners =
                        [(0.0, 0.0), (w, 0.0), (w, h), (0.0, h)].map(|(cx, cy)| place(cx - ax * w, cy - ay * h));
                    r.fill(&corners, paint.tinted(cell_color(si, id)), paint.opacity);
                }
            }
            continue;
        }
        // 2. Shapes, outlined and lightly filled like Tiled draws them.
        let outline: Vec<(f32, f32)> = match o.shape.as_ref().unwrap_or(&Shape::Rect) {
            Shape::Rect | Shape::Text => [(0.0, 0.0), (w, 0.0), (w, h), (0.0, h)].map(|(x, y)| place(x, y)).to_vec(),
            Shape::Ellipse => (0..48)
                .map(|i| {
                    let a = i as f32 / 48.0 * std::f32::consts::TAU;
                    place(w / 2.0 + a.cos() * w / 2.0, h / 2.0 + a.sin() * h / 2.0)
                })
                .collect(),
            Shape::Polygon(pts) | Shape::Polyline(pts) => pts.iter().map(|&(x, y)| place(x, y)).collect(),
            Shape::Point => {
                let c = place(0.0, 0.0);
                let rad = (4.0 * r.scale).max(1.5);
                let disc: Vec<_> = (0..16)
                    .map(|i| {
                        let a = i as f32 / 16.0 * std::f32::consts::TAU;
                        (c.0 + a.cos() * rad, c.1 + a.sin() * rad)
                    })
                    .collect();
                r.fill(&disc, color, paint.opacity);
                continue;
            }
        };
        let closed = !matches!(o.shape, Some(Shape::Polyline(_)));
        if closed && outline.len() >= 3 && w.max(h) > 0.0 || matches!(o.shape, Some(Shape::Polygon(_))) {
            r.fill(&outline, fill, paint.opacity);
        }
        let n = outline.len();
        let segments = if closed { n } else { n.saturating_sub(1) };
        for i in 0..segments {
            r.line(outline[i], outline[(i + 1) % n], stroke, color, paint.opacity);
        }
    }
}

/// Draws a tile object: its picture stretched to `size`, the alignment
/// point (`align`, fractions of the size) at `anchor`, turned by `theta`
/// clockwise about it.
#[allow(clippy::too_many_arguments)]
fn draw_object_tile(
    r: &mut Raster,
    img: &Image8,
    rect: [u32; 4],
    anchor: (f32, f32),
    size: (f32, f32),
    align: (f32, f32),
    theta: f32,
    flags: u32,
    paint: Paint,
) {
    let (w, h) = size;
    if w <= 0.0 || h <= 0.0 {
        return;
    }
    let (sin, cos) = theta.sin_cos();
    let (ox, oy) = (align.0 * w, align.1 * h);
    // The corners on the map, for the area to visit.
    let corners = [(0.0, 0.0), (w, 0.0), (w, h), (0.0, h)].map(|(x, y)| {
        let (dx, dy) = (x - ox, y - oy);
        (anchor.0 + dx * cos - dy * sin, anchor.1 + dx * sin + dy * cos)
    });
    let xmin = corners.iter().map(|c| c.0).fold(f32::INFINITY, f32::min);
    let xmax = corners.iter().map(|c| c.0).fold(f32::NEG_INFINITY, f32::max);
    let ymin = corners.iter().map(|c| c.1).fold(f32::INFINITY, f32::min);
    let ymax = corners.iter().map(|c| c.1).fold(f32::NEG_INFINITY, f32::max);
    let (fx, fy, fd) = (flags & FLIP_X != 0, flags & FLIP_Y != 0, flags & FLIP_DIAGONAL != 0);
    for ry in r.rows(ymin, ymax) {
        let my = r.map_y(ry) - anchor.1;
        for rx in r.cols(xmin, xmax) {
            let mx = r.map_x(rx) - anchor.0;
            // Back into the picture's own frame.
            let lx = mx * cos + my * sin + ox;
            let ly = -mx * sin + my * cos + oy;
            if lx < 0.0 || ly < 0.0 || lx >= w || ly >= h {
                continue;
            }
            let (mut u, mut v) = (lx / w, ly / h);
            if fy {
                v = 1.0 - v;
            }
            if fx {
                u = 1.0 - u;
            }
            if fd {
                (u, v) = (v, u);
            }
            let sx = rect[0] + ((u * rect[2] as f32) as u32).min(rect[2].saturating_sub(1));
            let sy = rect[1] + ((v * rect[3] as f32) as u32).min(rect[3].saturating_sub(1));
            if sx < img.width && sy < img.height {
                r.put(rx, ry, paint.tinted(crate::pixels::get(img, sx, sy)), paint.opacity);
            }
        }
    }
}

//endregion
