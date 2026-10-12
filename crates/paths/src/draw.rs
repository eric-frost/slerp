//! A small anti-aliased 2D rasteriser for thumbnails drawn on the CPU:
//! thick polylines with colours that change along them, dark halos that
//! keep them readable on any background, and round markers.  Pixels are
//! premultiplied sRGB floats; coverage comes from each pixel centre's
//! distance to the shape.

use glam::Vec2;
use slerp_scene::Image8;

/// sRGB colour with alpha, 0 to 1.
pub type Rgba = [f32; 4];

pub struct Canvas {
    pub width: u32,
    pub height: u32,
    pixels: Vec<Rgba>,
}

/// Coverage of a pixel at distance `d` from a shape's centre line, for a
/// shape `r` wide on each side.
fn coverage(d: f32, r: f32) -> f32 {
    (r + 0.5 - d).clamp(0.0, 1.0)
}

impl Canvas {
    pub fn new(width: u32, height: u32) -> Canvas {
        let (width, height) = (width.max(1), height.max(1));
        Canvas { width, height, pixels: vec![[0.0; 4]; width as usize * height as usize] }
    }

    fn blend(&mut self, x: u32, y: u32, c: Rgba, cov: f32) {
        let a = c[3] * cov;
        if a <= 0.0 {
            return;
        }
        let px = &mut self.pixels[(y * self.width + x) as usize];
        for i in 0..3 {
            px[i] = c[i] * a + px[i] * (1.0 - a);
        }
        px[3] = a + px[3] * (1.0 - a);
    }

    /// The pixels within `r` (plus a pixel of anti-aliasing) of the segment
    /// `a`-`b`, with their coverage and how far along the segment each is.
    fn segment_pixels(&self, a: Vec2, b: Vec2, r: f32, mut f: impl FnMut(u32, u32, f32, f32)) {
        let pad = r + 1.0;
        let x0 = (a.x.min(b.x) - pad).floor().max(0.0) as i64;
        let x1 = (a.x.max(b.x) + pad).ceil().min(self.width as f32 - 1.0) as i64;
        let y0 = (a.y.min(b.y) - pad).floor().max(0.0) as i64;
        let y1 = (a.y.max(b.y) + pad).ceil().min(self.height as f32 - 1.0) as i64;
        let ab = b - a;
        let len2 = ab.length_squared();
        for y in y0..=y1 {
            for x in x0..=x1 {
                let p = Vec2::new(x as f32 + 0.5, y as f32 + 0.5);
                let t = if len2 > 0.0 { ((p - a).dot(ab) / len2).clamp(0.0, 1.0) } else { 0.0 };
                let d = (a + ab * t - p).length();
                let cov = coverage(d, r);
                if cov > 0.0 {
                    f(x as u32, y as u32, cov, t);
                }
            }
        }
    }

    /// Draws a polyline `r` pixels wide on each side, coloured per point
    /// (or all in `colors[0]` when there is one colour).
    pub fn stroke(&mut self, pts: &[Vec2], colors: &[Rgba], r: f32) {
        if pts.is_empty() || colors.is_empty() {
            return;
        }
        if pts.len() == 1 {
            self.disc(pts[0], r, colors[0]);
            return;
        }
        let color = |i: usize| colors.get(i).copied().unwrap_or(colors[0]);
        for i in 1..pts.len() {
            let (ca, cb) = (color(i - 1), color(i));
            let (a, b) = (pts[i - 1], pts[i]);
            let mut hits = Vec::new();
            self.segment_pixels(a, b, r, |x, y, cov, t| hits.push((x, y, cov, t)));
            for (x, y, cov, t) in hits {
                let c = [0, 1, 2, 3].map(|k| ca[k] + (cb[k] - ca[k]) * t);
                self.blend(x, y, c, cov);
            }
        }
    }

    /// Draws a polyline's halo: every pixel within `r` gets `color` once, at
    /// the largest coverage any segment gives it, so overlapping segments
    /// don't darken where they meet.
    pub fn halo(&mut self, lines: &[Vec<Vec2>], r: f32, color: Rgba) {
        let mut mask = vec![0.0f32; self.pixels.len()];
        let w = self.width;
        for pts in lines {
            if pts.len() == 1 {
                self.segment_pixels(pts[0], pts[0], r, |x, y, cov, _| {
                    let m = &mut mask[(y * w + x) as usize];
                    *m = m.max(cov);
                });
            }
            for s in pts.windows(2) {
                self.segment_pixels(s[0], s[1], r, |x, y, cov, _| {
                    let m = &mut mask[(y * w + x) as usize];
                    *m = m.max(cov);
                });
            }
        }
        for (i, m) in mask.into_iter().enumerate() {
            if m > 0.0 {
                let (x, y) = (i as u32 % w, i as u32 / w);
                self.blend(x, y, color, m);
            }
        }
    }

    /// A filled circle.
    pub fn disc(&mut self, c: Vec2, r: f32, color: Rgba) {
        let mut hits = Vec::new();
        self.segment_pixels(c, c, r, |x, y, cov, _| hits.push((x, y, cov)));
        for (x, y, cov) in hits {
            self.blend(x, y, color, cov);
        }
    }

    /// A marker: a filled circle with a ring around it.
    pub fn marker(&mut self, c: Vec2, r: f32, fill: Rgba, ring: Rgba, ring_width: f32) {
        self.disc(c, r + ring_width, ring);
        self.disc(c, r, fill);
    }

    /// The picture with straight alpha.
    pub fn into_image(self) -> Image8 {
        let mut img = Image8::new(self.width, self.height);
        for (px, out) in self.pixels.iter().zip(img.pixels.chunks_exact_mut(4)) {
            let a = px[3];
            if a <= 0.0 {
                continue;
            }
            for i in 0..3 {
                out[i] = ((px[i] / a).clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
            }
            out[3] = (a.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
        }
        img
    }
}

/// Drops points closer than `min` pixels to the last one kept, keeping the
/// ends, so a long track costs what its picture shows.
pub fn thin(pts: &[Vec2], min: f32) -> Vec<usize> {
    let mut kept = Vec::new();
    let mut last: Option<Vec2> = None;
    for (i, &p) in pts.iter().enumerate() {
        if last.is_none_or(|l| l.distance_squared(p) >= min * min) || i + 1 == pts.len() {
            kept.push(i);
            last = Some(p);
        }
    }
    kept
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn draws_an_anti_aliased_line() {
        let mut c = Canvas::new(32, 16);
        c.stroke(&[Vec2::new(4.0, 8.3), Vec2::new(28.0, 8.3)], &[[1.0, 0.0, 0.0, 1.0]], 2.0);
        let img = c.into_image();
        let at = |x: u32, y: u32| &img.pixels[((y * 32 + x) * 4) as usize..][..4];
        assert_eq!(at(16, 8), [255, 0, 0, 255]);
        assert_eq!(at(16, 2)[3], 0);
        assert!(at(16, 10)[3] > 0 && at(16, 10)[3] < 255);
        assert_eq!(thin(&[Vec2::ZERO, Vec2::new(0.1, 0.0), Vec2::new(5.0, 0.0)], 1.0), [0, 2]);
    }
}
