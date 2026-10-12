//! Drawing into a picture at any scale: the raster that Tiled maps and
//! LDtk levels are composed in, sampling source pictures at pixel centres
//! so tiles keep hard edges, plus filled polygons and soft lines for
//! objects and entities.

use crate::blend::{Px, normal};

/// The picture being drawn: the map's bounds at a scale.
pub(crate) struct Raster {
    pub width: u32,
    pub height: u32,
    pub scale: f32,
    pub left: f32,
    pub top: f32,
    pub pixels: Vec<u8>,
}

impl Raster {
    /// Map pixels to raster pixels.
    pub fn to_raster(&self, (x, y): (f32, f32)) -> (f32, f32) {
        ((x - self.left) * self.scale, (y - self.top) * self.scale)
    }

    /// The centre of raster pixel `i` along an axis, in map pixels.
    pub fn map_x(&self, i: u32) -> f32 {
        self.left + (i as f32 + 0.5) / self.scale
    }

    pub fn map_y(&self, i: u32) -> f32 {
        self.top + (i as f32 + 0.5) / self.scale
    }

    /// Raster pixels whose centres fall in `a..b` (map pixels) along x.
    pub fn cols(&self, a: f32, b: f32) -> std::ops::Range<u32> {
        let lo = ((a - self.left) * self.scale - 0.5).ceil().clamp(0.0, self.width as f32) as u32;
        let hi = ((b - self.left) * self.scale - 0.5).ceil().clamp(0.0, self.width as f32) as u32;
        lo..hi
    }

    pub fn rows(&self, a: f32, b: f32) -> std::ops::Range<u32> {
        let lo = ((a - self.top) * self.scale - 0.5).ceil().clamp(0.0, self.height as f32) as u32;
        let hi = ((b - self.top) * self.scale - 0.5).ceil().clamp(0.0, self.height as f32) as u32;
        lo..hi
    }

    #[inline]
    pub fn put(&mut self, x: u32, y: u32, src: Px, opacity: i32) {
        if src[3] == 0 {
            return;
        }
        let i = (y as usize * self.width as usize + x as usize) * 4;
        let b = [self.pixels[i], self.pixels[i + 1], self.pixels[i + 2], self.pixels[i + 3]];
        self.pixels[i..i + 4].copy_from_slice(&normal(b, src, opacity));
    }

    /// Fills a polygon given in raster pixels, even-odd, at pixel centres.
    pub fn fill(&mut self, pts: &[(f32, f32)], color: Px, opacity: i32) {
        if pts.len() < 3 {
            return;
        }
        let ymin = pts.iter().map(|p| p.1).fold(f32::INFINITY, f32::min);
        let ymax = pts.iter().map(|p| p.1).fold(f32::NEG_INFINITY, f32::max);
        let y0 = (ymin - 0.5).ceil().clamp(0.0, self.height as f32) as u32;
        let y1 = (ymax - 0.5).ceil().clamp(0.0, self.height as f32) as u32;
        let mut xs = Vec::new();
        for y in y0..y1 {
            let cy = y as f32 + 0.5;
            xs.clear();
            for (i, &(ax, ay)) in pts.iter().enumerate() {
                let (bx, by) = pts[(i + 1) % pts.len()];
                if (ay <= cy) != (by <= cy) {
                    xs.push(ax + (cy - ay) / (by - ay) * (bx - ax));
                }
            }
            xs.sort_by(f32::total_cmp);
            for pair in xs.chunks_exact(2) {
                let x0 = (pair[0] - 0.5).ceil().clamp(0.0, self.width as f32) as u32;
                let x1 = (pair[1] - 0.5).ceil().clamp(0.0, self.width as f32) as u32;
                for x in x0..x1 {
                    self.put(x, y, color, opacity);
                }
            }
        }
    }

    /// Draws a line `width` raster pixels wide with soft edges.  Each row
    /// visits only the pixels near the line, so long diagonals cost their
    /// length, not their bounding box.
    pub fn line(&mut self, a: (f32, f32), b: (f32, f32), width: f32, color: Px, opacity: i32) {
        let r = width / 2.0 + 0.5;
        let y0 = (a.1.min(b.1) - r).floor().clamp(0.0, self.height as f32) as u32;
        let y1 = (a.1.max(b.1) + r).ceil().clamp(0.0, self.height as f32) as u32;
        let (dx, dy) = (b.0 - a.0, b.1 - a.1);
        let len2 = (dx * dx + dy * dy).max(1e-6);
        for y in y0..y1 {
            // The part of the segment within `r` of this row.
            let py = y as f32 + 0.5;
            let (t0, t1) = if dy.abs() > 1e-6 {
                let (p, q) = ((py - r - a.1) / dy, (py + r - a.1) / dy);
                (p.min(q).max(0.0), p.max(q).min(1.0))
            } else {
                (0.0, 1.0)
            };
            if t0 > t1 {
                continue;
            }
            let (xa, xb) = (a.0 + t0 * dx, a.0 + t1 * dx);
            let x0 = (xa.min(xb) - r).floor().clamp(0.0, self.width as f32) as u32;
            let x1 = (xa.max(xb) + r).ceil().clamp(0.0, self.width as f32) as u32;
            for x in x0..x1 {
                let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
                let t = (((px - a.0) * dx + (py - a.1) * dy) / len2).clamp(0.0, 1.0);
                let (qx, qy) = (a.0 + t * dx - px, a.1 + t * dy - py);
                let cover = (width / 2.0 + 0.5 - (qx * qx + qy * qy).sqrt()).clamp(0.0, 1.0);
                if cover > 0.0 {
                    let c = [color[0], color[1], color[2], (color[3] as f32 * cover + 0.5) as u8];
                    self.put(x, y, c, opacity);
                }
            }
        }
    }
}

/// The colour a tile is drawn in when its picture is missing: a hue from
/// its tileset and id, so different tiles tell apart.
pub fn cell_color(set: usize, id: u32) -> Px {
    let mut h = (set as u32).wrapping_mul(0x9E37_79B9) ^ id.wrapping_mul(0x85EB_CA6B);
    h ^= h >> 15;
    h = h.wrapping_mul(0x2C1B_3C6D);
    h ^= h >> 12;
    let hue = (h % 360) as f32;
    // HSL with 45 % saturation and 58 % lightness.
    let (s, l) = (0.45f32, 0.58f32);
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let x = c * (1.0 - ((hue / 60.0) % 2.0 - 1.0).abs());
    let (r, g, b) = match (hue / 60.0) as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = l - c / 2.0;
    let to = |v: f32| ((v + m) * 255.0 + 0.5) as u8;
    [to(r), to(g), to(b), 255]
}
