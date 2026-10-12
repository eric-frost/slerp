//! A map drawn the way Doom's own automap colours its lines, for the WAD's
//! gallery.

use super::map::{ML_DONTDRAW, ML_SECRET, MapData};
use crate::pixels;
use slerp_scene::Texture;

/// Doom's automap colours, as indices into the game's palette: red walls,
/// teleporters a lighter red, brown where the floor height changes, yellow
/// where the ceiling does, and grey for other two-sided lines (which Doom
/// shows only with the map cheat).
pub const WALL: u8 = 176;
pub const TELEPORTER: u8 = 184;
pub const FLOOR_STEP: u8 = 64;
pub const CEILING_STEP: u8 = 231;
pub const FLAT: u8 = 96;

struct Line {
    a: [f64; 2],
    b: [f64; 2],
    color: u8,
}

/// How Doom's automap colours a line, from its sides' floor and ceiling
/// heights.
fn line_color(flags: u32, teleporter: bool, front: Option<(f64, f64)>, back: Option<(f64, f64)>) -> Option<u8> {
    if flags & ML_DONTDRAW != 0 {
        return None;
    }
    Some(match (front, back) {
        (_, None) | (None, _) => WALL,
        _ if teleporter => TELEPORTER,
        _ if flags & ML_SECRET != 0 => WALL,
        (Some(f), Some(b)) if f.0 != b.0 => FLOOR_STEP,
        (Some(f), Some(b)) if f.1 != b.1 => CEILING_STEP,
        _ => FLAT,
    })
}

fn lines(map: &MapData) -> Vec<Line> {
    let heights = |side: Option<usize>| map.sector_of(side).map(|s| (map.sectors[s].floor, map.sectors[s].ceiling));
    let hexen = map.format.hexen_specials();
    let mut out = Vec::new();
    for l in &map.lines {
        let Some((a, b)) = map.ends(l) else { continue };
        let teleporter = if hexen { matches!(l.special, 70 | 71) } else { matches!(l.special, 39 | 97 | 125 | 126) };
        if let Some(color) = line_color(l.flags, teleporter, heights(l.front), heights(l.back)) {
            out.push(Line { a, b, color });
        }
    }
    out
}

/// Draws a map's lines on black, fitted to `size` pixels on its longer
/// side, grey lines first and walls last so walls stay on top.
pub fn draw(map: &MapData, palette: &[u8], size: u32) -> Option<Texture> {
    let lines = lines(map);
    if lines.is_empty() {
        return None;
    }
    let (mut lo, mut hi) = ([f64::MAX; 2], [f64::MIN; 2]);
    for l in &lines {
        for p in [l.a, l.b] {
            lo = [lo[0].min(p[0]), lo[1].min(p[1])];
            hi = [hi[0].max(p[0]), hi[1].max(p[1])];
        }
    }
    let span = (hi[0] - lo[0]).max(hi[1] - lo[1]).max(1.0);
    let margin = (size as f64 * 0.03).max(2.0);
    let scale = (size as f64 - 2.0 * margin) / span;
    let w = (((hi[0] - lo[0]) * scale + 2.0 * margin).ceil() as u32).clamp(1, size);
    let h = (((hi[1] - lo[1]) * scale + 2.0 * margin).ceil() as u32).clamp(1, size);
    let thick = (size / 512).max(1) as i64;
    let mut rgba = vec![0u8; w as usize * h as usize * 4];
    for p in rgba.chunks_exact_mut(4) {
        p[3] = 255;
    }
    let rank = |c: u8| match c {
        FLAT => 0,
        FLOOR_STEP | CEILING_STEP => 1,
        WALL => 2,
        _ => 3,
    };
    let mut order: Vec<&Line> = lines.iter().collect();
    order.sort_by_key(|l| rank(l.color));
    let to_px = |p: [f64; 2]| -> (i64, i64) {
        (
            ((p[0] - lo[0]) * scale + margin).round() as i64,
            (h as f64 - ((p[1] - lo[1]) * scale + margin)).round() as i64,
        )
    };
    for l in order {
        let rgb = &palette[l.color as usize * 3..l.color as usize * 3 + 3];
        let (mut x0, mut y0) = to_px(l.a);
        let (x1, y1) = to_px(l.b);
        let (dx, dy) = ((x1 - x0).abs(), -(y1 - y0).abs());
        let (sx, sy) = (if x0 < x1 { 1 } else { -1 }, if y0 < y1 { 1 } else { -1 });
        let mut err = dx + dy;
        for _ in 0..=(dx - dy) {
            for oy in 0..thick {
                for ox in 0..thick {
                    let (x, y) = (x0 + ox, y0 + oy);
                    if x >= 0 && y >= 0 && x < w as i64 && y < h as i64 {
                        let i = (y as usize * w as usize + x as usize) * 4;
                        rgba[i..i + 3].copy_from_slice(rgb);
                    }
                }
            }
            if x0 == x1 && y0 == y1 {
                break;
            }
            let e2 = 2 * err;
            if e2 >= dy {
                err += dy;
                x0 += sx;
            }
            if e2 <= dx {
                err += dx;
                y0 += sy;
            }
        }
    }
    Some(pixels::texture(w, h, rgba, None, "Doom WAD", &format!("automap of {} lines", lines.len())))
}
