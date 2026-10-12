//! Drawing LDtk levels the way LDtk's editor shows them: each level's
//! background colour and picture, then its layers from the bottom up.
//! IntGrid layers show the tiles their rules made, or their values'
//! colours when there are none (or the tileset picture is missing);
//! entities show their tile, or the rectangle, ellipse or cross their
//! definition asks for.  Everything a level draws stays inside it.

use super::doc::*;
use super::{Ldtk, Listed};
use crate::blend::Px;
use crate::pixels::{MAX_PIXELS, get};
use crate::raster::{Raster, cell_color};
use slerp_scene::{CanvasLayer, Image8};

/// A rectangle in canvas pixels: left, top, right, bottom.
type Clip = [f32; 4];

fn intersect(a: Clip, b: Clip) -> Clip {
    [a[0].max(b[0]), a[1].max(b[1]), a[2].min(b[2]), a[3].min(b[3])]
}

/// Opacity 0..=1 as the raster's 0..=255.
fn alpha(a: f32) -> i32 {
    (a.clamp(0.0, 1.0) * 255.0 + 0.5) as i32
}

pub fn compose(ldtk: &Ldtk, listed: &[CanvasLayer], shown: &[bool], scale: f32) -> Image8 {
    let [left, top, bw, bh] = ldtk.bounds.map(|v| v as f32);
    let fits = (MAX_PIXELS as f64 / (bw.max(1.0) as f64 * bh.max(1.0) as f64)).sqrt() as f32;
    let scale = if scale.is_finite() && scale > 0.0 { scale.min(1.0).min(fits) } else { 1.0f32.min(fits) };
    let width = ((bw * scale).ceil() as u32).max(1);
    let height = ((bh * scale).ceil() as u32).max(1);
    let mut r = Raster { width, height, scale, left, top, pixels: vec![0; width as usize * height as usize * 4] };
    let show = |i: usize| shown.get(i).copied().unwrap_or(listed[i].visible);
    for placed in &ldtk.levels {
        let level = &placed.level;
        let (x, y) = (placed.x as f32, placed.y as f32);
        let clip = [x, y, x + level.width as f32, y + level.height as f32];
        for (i, key) in ldtk.keys.iter().enumerate() {
            if !show(i) {
                continue;
            }
            match *key {
                Listed::Background => background(ldtk, &mut r, level, clip),
                Listed::Layer(uid) => {
                    // A layer the file hides shows everywhere once it is
                    // turned on; otherwise each level's own setting holds.
                    let forced = !listed[i].visible;
                    for layer in level.layers.iter().rev().filter(|l| l.def_uid == uid) {
                        if layer.visible || forced {
                            draw_layer(ldtk, &mut r, layer, (x, y), clip);
                        }
                    }
                }
            }
        }
    }
    Image8 { width, height, pixels: r.pixels }
}

fn background(ldtk: &Ldtk, r: &mut Raster, level: &LevelDoc, clip: Clip) {
    fill_rect(r, clip, clip, level.bg_color, 255);
    let Some(bg) = &level.background else { return };
    let Some(img) = ldtk.pictures.get(&bg.rel_path) else { return };
    let [cx, cy, cw, ch] = bg.crop;
    let (cw, ch) = if cw > 0.0 && ch > 0.0 { (cw, ch) } else { (img.width as f32, img.height as f32) };
    let (w, h) = (cw * bg.scale.0, ch * bg.scale.1);
    if !(w > 0.0 && h > 0.0) {
        return;
    }
    let at = (clip[0] + bg.top_left.0, clip[1] + bg.top_left.1);
    if bg.repeat {
        // Copies of the picture across the level.
        let mut ty = at.1 - ((at.1 - clip[1]) / h).ceil().max(0.0) * h;
        while ty < clip[3] {
            let mut tx = at.0 - ((at.0 - clip[0]) / w).ceil().max(0.0) * w;
            while tx < clip[2] {
                picture(r, clip, img, [cx, cy, cw, ch], [tx, ty, w, h], 0, 255);
                tx += w;
            }
            ty += h;
        }
    } else {
        picture(r, clip, img, [cx, cy, cw, ch], [at.0, at.1, w, h], 0, 255);
    }
}

fn draw_layer(ldtk: &Ldtk, r: &mut Raster, layer: &LayerDoc, level_at: (f32, f32), clip: Clip) {
    let (ox, oy) = (level_at.0 + layer.offset.0, level_at.1 + layer.offset.1);
    let opacity = alpha(layer.opacity);
    if opacity == 0 {
        return;
    }
    let grid = layer.grid as f32;
    let atlas = ldtk.layer_picture(layer);
    match layer.kind {
        LayerType::Entities => {
            for e in &layer.entities {
                entity(ldtk, r, e, (ox, oy), clip, opacity);
            }
        }
        // IntGrid layers whose rules made tiles show the tiles, as LDtk
        // does; without the picture, their values' colours say more than
        // the tiles' made-up ones.
        LayerType::IntGrid if atlas.is_none() || layer.tiles.is_empty() => {
            let cols = layer.columns.max(1);
            let colors = ldtk.int_colors.get(&layer.def_uid);
            for &(i, value) in &layer.cells {
                let (cx, cy) = ((i % cols) as f32 * grid + ox, (i / cols) as f32 * grid + oy);
                let color = colors.and_then(|c| c.get(&value)).copied().unwrap_or_else(|| {
                    cell_color(layer.def_uid.unsigned_abs() as usize, value.clamp(0, u32::MAX as i64) as u32)
                });
                fill_rect(r, clip, [cx, cy, cx + grid, cy + grid], color, opacity);
            }
        }
        _ => {
            let src = layer.tileset.and_then(|t| ldtk.tileset_grids.get(&t)).copied().unwrap_or(layer.grid) as f32;
            for t in &layer.tiles {
                let (tx, ty) = (ox + t.px.0, oy + t.px.1);
                let a = alpha(t.alpha * layer.opacity);
                match atlas {
                    Some(img) => {
                        let s = [t.src.0 as f32, t.src.1 as f32, src, src];
                        picture(r, clip, img, s, [tx, ty, grid, grid], t.flip, a);
                    }
                    None => {
                        let set = layer.tileset.unwrap_or(layer.def_uid).unsigned_abs() as usize;
                        fill_rect(r, clip, [tx, ty, tx + grid, ty + grid], cell_color(set, t.id), a);
                    }
                }
            }
        }
    }
}

/// Draws an entity as LDtk's editor does (`EntityRender.renderCore`).
fn entity(ldtk: &Ldtk, r: &mut Raster, e: &EntityDoc, layer_at: (f32, f32), clip: Clip, opacity: i32) {
    let fallback = EntityDef::default();
    let def = ldtk.entity_defs.get(&e.def_uid).unwrap_or(&fallback);
    let color = e.color.unwrap_or(def.color);
    let (w, h) = e.size;
    let (x0, y0) = (layer_at.0 + e.px.0 - e.pivot.0 * w, layer_at.1 + e.px.1 - e.pivot.1 * h);
    let rect = [x0, y0, x0 + w, y0 + h];
    // Outlines are LDtk's colour lightened by 30 %, one pixel wide.
    let light = |c: Px| {
        let mix = |v: u8| (v as f32 + (255.0 - v as f32) * 0.3 + 0.5) as u8;
        [mix(c[0]), mix(c[1]), mix(c[2]), 255]
    };
    let stroke = r.scale.max(1.0);
    let scaled = |a: f32| (a * opacity as f32 + 0.5) as i32;
    let tile = e.tile.or(def.tile);
    match tile {
        Some(t) => {
            fill_rect(r, clip, rect, color, scaled(def.fill_opacity));
            outline(r, rect, light(color), stroke, scaled(def.line_opacity));
            match ldtk.tile_picture(&t) {
                Some(img) => entity_tile(r, clip, img, &t, def, e, rect, scaled(def.tile_opacity)),
                // LDtk's own icons, or a picture that wasn't read: a box in
                // the entity's colour keeps it visible.
                None => {
                    fill_rect(r, clip, rect, color, scaled(0.4));
                    outline(r, rect, light(color), stroke, opacity);
                }
            }
        }
        None => match def.shape {
            EntityShape::Rectangle | EntityShape::Tile => {
                fill_rect(r, clip, rect, color, scaled(def.fill_opacity));
                outline(r, rect, light(color), stroke, scaled(def.line_opacity));
            }
            EntityShape::Ellipse => {
                let points: Vec<(f32, f32)> = (0..48)
                    .map(|i| {
                        let a = i as f32 / 48.0 * std::f32::consts::TAU;
                        r.to_raster((x0 + w / 2.0 * (1.0 + a.cos()), y0 + h / 2.0 * (1.0 + a.sin())))
                    })
                    .collect();
                r.fill(&points, color, scaled(def.fill_opacity));
                for i in 0..points.len() {
                    r.line(points[i], points[(i + 1) % points.len()], stroke, light(color), scaled(def.line_opacity));
                }
            }
            EntityShape::Cross => {
                let width = (2.0 * r.scale).max(1.5);
                let a = scaled(def.line_opacity.max(0.5));
                r.line(r.to_raster((x0, y0)), r.to_raster((x0 + w, y0 + h)), width, color, a);
                r.line(r.to_raster((x0, y0 + h)), r.to_raster((x0 + w, y0)), width, color, a);
            }
        },
    }
}

/// Draws an entity's tile into its rectangle the way its definition's
/// tile mode says, the tile's own pivot on the entity's.
#[allow(clippy::too_many_arguments)]
fn entity_tile(
    r: &mut Raster,
    clip: Clip,
    img: &Image8,
    t: &TileRect,
    def: &EntityDef,
    e: &EntityDoc,
    rect: Clip,
    a: i32,
) {
    let [sx, sy, sw, sh] = t.rect.map(|v| v as f32);
    let (w, h) = (rect[2] - rect[0], rect[3] - rect[1]);
    let (px, py) = e.pivot;
    // Where the entity's pivot is.
    let (ax, ay) = (rect[0] + px * w, rect[1] + py * h);
    let inside = intersect(clip, rect);
    match def.tile_mode {
        TileMode::Stretch => picture(r, clip, img, [sx, sy, sw, sh], [rect[0], rect[1], w, h], 0, a),
        TileMode::FitInside => {
            let s = (w / sw).min(h / sh);
            let (dw, dh) = (sw * s, sh * s);
            picture(r, clip, img, [sx, sy, sw, sh], [ax - px * dw, ay - py * dh, dw, dh], 0, a);
        }
        TileMode::Cover => {
            let s = (w / sw).max(h / sh);
            let (fw, fh) = ((w.min(sw * s)) / s, (h.min(sh * s)) / s);
            let part = [sx + (sw - fw) * px, sy + (sh - fh) * py, fw, fh];
            picture(r, inside, img, part, [ax - px * fw * s, ay - py * fh * s, fw * s, fh * s], 0, a);
        }
        TileMode::FullSizeCropped => {
            let (fw, fh) = (w.min(sw), h.min(sh));
            let part = [sx + (sw - fw) * px, sy + (sh - fh) * py, fw, fh];
            picture(r, inside, img, part, [ax - px * fw, ay - py * fh, fw, fh], 0, a);
        }
        TileMode::FullSizeUncropped => {
            picture(r, clip, img, [sx, sy, sw, sh], [ax - px * sw, ay - py * sh, sw, sh], 0, a)
        }
        TileMode::Repeat => tiled(r, inside, img, [sx, sy, sw, sh], rect, a),
        TileMode::NineSlice => {
            // Borders kept at their size; edges and middle repeated, as
            // LDtk's scale grid draws them.
            let [top, right, bottom, left] = def.nine_slice;
            let (l, rt) = (left.min(sw / 2.0).min(w / 2.0), right.min(sw / 2.0).min(w / 2.0));
            let (tp, bt) = (top.min(sh / 2.0).min(h / 2.0), bottom.min(sh / 2.0).min(h / 2.0));
            let src_x = [sx, sx + l, sx + sw - rt, sx + sw];
            let src_y = [sy, sy + tp, sy + sh - bt, sy + sh];
            let dst_x = [rect[0], rect[0] + l, rect[2] - rt, rect[2]];
            let dst_y = [rect[1], rect[1] + tp, rect[3] - bt, rect[3]];
            for j in 0..3 {
                for i in 0..3 {
                    let part = [src_x[i], src_y[j], src_x[i + 1] - src_x[i], src_y[j + 1] - src_y[j]];
                    let dest = [dst_x[i], dst_y[j], dst_x[i + 1], dst_y[j + 1]];
                    if part[2] > 0.0 && part[3] > 0.0 && dest[2] > dest[0] && dest[3] > dest[1] {
                        tiled(r, intersect(inside, dest), img, part, dest, a);
                    }
                }
            }
        }
    }
}

/*┌─────────────────╮
| Painting        |
└─────────────────╯*/
//region Painting

/// Fills a rectangle (canvas pixels: left, top, right, bottom) inside
/// `clip`.
fn fill_rect(r: &mut Raster, clip: Clip, rect: Clip, color: Px, opacity: i32) {
    if opacity <= 0 {
        return;
    }
    let [x0, y0, x1, y1] = intersect(clip, rect);
    for ry in r.rows(y0, y1) {
        for rx in r.cols(x0, x1) {
            r.put(rx, ry, color, opacity);
        }
    }
}

fn outline(r: &mut Raster, rect: Clip, color: Px, width: f32, opacity: i32) {
    if opacity <= 0 {
        return;
    }
    let corners =
        [(rect[0], rect[1]), (rect[2], rect[1]), (rect[2], rect[3]), (rect[0], rect[3])].map(|p| r.to_raster(p));
    for i in 0..4 {
        r.line(corners[i], corners[(i + 1) % 4], width, color, opacity);
    }
}

/// Draws the part `src` (x, y, width, height in picture pixels) of a
/// picture into `dest` (x, y, width, height in canvas pixels) inside
/// `clip`, sampling at pixel centres so pixels stay hard-edged.  Bit 0 of
/// `flip` mirrors it horizontally, bit 1 vertically.
fn picture(r: &mut Raster, clip: Clip, img: &Image8, src: [f32; 4], dest: [f32; 4], flip: u8, opacity: i32) {
    let [dx, dy, dw, dh] = dest;
    if !(dw > 0.0 && dh > 0.0 && src[2] > 0.0 && src[3] > 0.0) || opacity <= 0 {
        return;
    }
    let [x0, y0, x1, y1] = intersect(clip, [dx, dy, dx + dw, dy + dh]);
    for ry in r.rows(y0, y1) {
        let mut v = (r.map_y(ry) - dy) / dh;
        if flip & 2 != 0 {
            v = 1.0 - v;
        }
        let sy = (src[1] + (v * src[3]).clamp(0.0, src[3] - 0.001)).floor();
        if sy < 0.0 || sy >= img.height as f32 {
            continue;
        }
        for rx in r.cols(x0, x1) {
            let mut u = (r.map_x(rx) - dx) / dw;
            if flip & 1 != 0 {
                u = 1.0 - u;
            }
            let sx = (src[0] + (u * src[2]).clamp(0.0, src[2] - 0.001)).floor();
            if sx >= 0.0 && sx < img.width as f32 {
                r.put(rx, ry, get(img, sx as u32, sy as u32), opacity);
            }
        }
    }
}

/// Repeats the part `src` of a picture at its own size across `dest`
/// (left, top, right, bottom), starting at its top left, inside `clip`.
fn tiled(r: &mut Raster, clip: Clip, img: &Image8, src: [f32; 4], dest: Clip, opacity: i32) {
    let [sx, sy, sw, sh] = src;
    if !(sw > 0.0 && sh > 0.0) || opacity <= 0 {
        return;
    }
    let [x0, y0, x1, y1] = intersect(clip, dest);
    for ry in r.rows(y0, y1) {
        let py = (sy + (r.map_y(ry) - dest[1]).rem_euclid(sh)).floor();
        if py < 0.0 || py >= img.height as f32 {
            continue;
        }
        for rx in r.cols(x0, x1) {
            let px = (sx + (r.map_x(rx) - dest[0]).rem_euclid(sw)).floor();
            if px >= 0.0 && px < img.width as f32 {
                r.put(rx, ry, get(img, px as u32, py as u32), opacity);
            }
        }
    }
}

//endregion
