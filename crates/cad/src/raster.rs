//! Drawing a flat `Drawing` into a picture on the CPU: anti-aliased lines of
//! a constant width in pixels, filled areas, arcs flattened to suit their
//! size on screen.  It serves thumbnails and the viewer alike; the viewer
//! asks for just the part of the drawing in its window, so zooming in stays
//! crisp however far.

use crate::drawing::{Arc, Bounds, Drawing, FOREGROUND};
use glam::{DVec2, DVec3};
use slerp_scene::Image8;

/// Where the picture looks: the drawing point at its centre, and how many
/// pixels one drawing unit takes.  Drawing y points up the picture.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct View {
    pub center: DVec2,
    pub scale: f64,
    pub width: u32,
    pub height: u32,
}

impl View {
    /// A view of `b` that fills a `width` by `height` picture with a margin
    /// of `margin` (a fraction of the picture's smaller side) around it.
    pub fn fit(b: &Bounds, width: u32, height: u32, margin: f64) -> View {
        let (w, h) = (width.max(1) as f64, height.max(1) as f64);
        let m = margin * w.min(h);
        let size = b.size();
        let sx = (w - 2.0 * m).max(1.0) / size.x.max(1e-12);
        let sy = (h - 2.0 * m).max(1.0) / size.y.max(1e-12);
        let mut scale = sx.min(sy);
        if !scale.is_finite() || scale <= 0.0 {
            scale = 1.0;
        }
        // A single point or a line along an axis: show it at a sane size.
        if size.x.max(size.y) < 1e-12 {
            scale = 1.0;
        }
        View { center: b.center().truncate(), scale, width, height }
    }

    pub fn to_screen(&self, p: DVec3) -> DVec2 {
        DVec2::new(
            (p.x - self.center.x) * self.scale + self.width as f64 * 0.5,
            (self.center.y - p.y) * self.scale + self.height as f64 * 0.5,
        )
    }

    pub fn to_world(&self, s: DVec2) -> DVec2 {
        DVec2::new(
            (s.x - self.width as f64 * 0.5) / self.scale + self.center.x,
            self.center.y - (s.y - self.height as f64 * 0.5) / self.scale,
        )
    }

    /// The part of the drawing the picture shows: its lowest and highest x
    /// and y.
    pub fn world_box(&self) -> ([f64; 2], [f64; 2]) {
        let a = self.to_world(DVec2::ZERO);
        let b = self.to_world(DVec2::new(self.width as f64, self.height as f64));
        ([a.x.min(b.x), a.y.min(b.y)], [a.x.max(b.x), a.y.max(b.y)])
    }
}

/// How to paint.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Paint {
    /// Dark paper: the pen colour contrasting with it is white.
    pub dark: bool,
    /// The width of lines in pixels.
    pub line_px: f64,
}

impl Paint {
    pub const LIGHT: Paint = Paint { dark: false, line_px: 1.2 };
    pub const DARK: Paint = Paint { dark: true, line_px: 1.2 };

    /// The paper colour.
    pub fn background(&self) -> [u8; 3] {
        if self.dark { [30, 33, 38] } else { [250, 250, 247] }
    }

    /// The colour a drawing colour is shown in on this paper: the pen colour
    /// for [`FOREGROUND`], and other colours kept apart from the paper (a
    /// yellow on white would hardly show).
    pub fn resolve(&self, color: u32) -> [u8; 3] {
        if color == FOREGROUND {
            return if self.dark { [236, 236, 232] } else { [28, 28, 30] };
        }
        let mut c = [(color >> 16 & 255) as f64, (color >> 8 & 255) as f64, (color & 255) as f64];
        let luma = (0.299 * c[0] + 0.587 * c[1] + 0.114 * c[2]) / 255.0;
        if !self.dark && luma > 0.6 {
            let k = 0.55 / luma;
            c.iter_mut().for_each(|v| *v *= k);
        } else if self.dark && luma < 0.3 {
            // Mix towards white until the colour reads against the paper.
            let k = (0.4 - luma) / (1.0 - luma);
            c.iter_mut().for_each(|v| *v += (255.0 - *v) * k);
        }
        [c[0].round() as u8, c[1].round() as u8, c[2].round() as u8]
    }
}

/// The pixels being painted, with the scratch mask that gathers coverage
/// for shapes made of many pieces (so overlapping pieces of one shape don't
/// darken each other).
struct Canvas {
    w: usize,
    h: usize,
    pixels: Vec<u8>,
    mask: Vec<u8>,
    touched: Vec<u32>,
    /// One row of triangle coverage, all zero between uses.
    row: Vec<f32>,
}

impl Canvas {
    fn new(w: u32, h: u32, bg: [u8; 3]) -> Canvas {
        let (w, h) = (w as usize, h as usize);
        let mut pixels = vec![255u8; w * h * 4];
        for p in pixels.chunks_exact_mut(4) {
            p[..3].copy_from_slice(&bg);
        }
        Canvas { w, h, pixels, mask: vec![0; w * h], touched: Vec::new(), row: vec![0.0; w] }
    }

    #[inline]
    fn blend(&mut self, i: usize, c: [u8; 3], cov: u8) {
        if cov == 0 {
            return;
        }
        let px = &mut self.pixels[i * 4..i * 4 + 3];
        let a = cov as u32;
        for k in 0..3 {
            px[k] = ((px[k] as u32 * (255 - a) + c[k] as u32 * a + 127) / 255) as u8;
        }
    }

    /// Takes the largest coverage seen so far at a pixel (lines).
    #[inline]
    fn mask_max(&mut self, x: usize, y: usize, cov: f32) {
        let v = (cov * 255.0 + 0.5) as u8;
        if v == 0 {
            return;
        }
        let i = y * self.w + x;
        let cur = self.mask[i];
        if cur == 0 {
            self.touched.push(i as u32);
        }
        if v > cur {
            self.mask[i] = v;
        }
    }

    /// Adds coverage (areas made of triangles that don't overlap).
    #[inline]
    fn mask_add(&mut self, x: usize, y: usize, cov: f32) {
        let v = (cov * 255.0 + 0.5) as u32;
        if v == 0 {
            return;
        }
        let i = y * self.w + x;
        let cur = self.mask[i] as u32;
        if cur == 0 {
            self.touched.push(i as u32);
        }
        self.mask[i] = (cur + v).min(255) as u8;
    }

    /// Paints what the mask gathered in `c` and clears it.
    fn flush(&mut self, c: [u8; 3]) {
        let touched = std::mem::take(&mut self.touched);
        for &i in &touched {
            let cov = self.mask[i as usize];
            self.mask[i as usize] = 0;
            self.blend(i as usize, c, cov);
        }
        self.touched = touched;
        self.touched.clear();
    }

    //region Lines

    /// A polyline of `pts` (screen pixels) in colour `c`.
    fn polyline(&mut self, pts: &[DVec2], r: f64, c: [u8; 3]) {
        let (w, h) = (self.w, self.h);
        if pts.len() == 2 {
            // One segment: nothing overlaps, so paint directly.
            segment(w, h, pts[0], pts[1], r, |x, y, cv| {
                let i = y * w + x;
                let a = (cv * 255.0 + 0.5) as u8;
                let px = &mut self.pixels[i * 4..i * 4 + 3];
                for k in 0..3 {
                    px[k] = ((px[k] as u32 * (255 - a as u32) + c[k] as u32 * a as u32 + 127) / 255) as u8;
                }
            });
            return;
        }
        for pair in pts.windows(2) {
            segment(w, h, pair[0], pair[1], r, |x, y, cv| self.mask_max(x, y, cv));
        }
        self.flush(c);
    }

    fn dot(&mut self, p: DVec2, r: f64, c: [u8; 3]) {
        let w = self.w;
        let mut hits: Vec<(usize, u8)> = Vec::new();
        segment(self.w, self.h, p, p, r, |x, y, cv| hits.push((y * w + x, (cv * 255.0 + 0.5) as u8)));
        for (i, a) in hits {
            self.blend(i, c, a);
        }
    }

    //endregion

    //region Areas

    /// Adds the coverage of a triangle (screen pixels) to the mask: four
    /// sub-scanlines per row, with exact horizontal overlap.
    fn triangle(&mut self, t: [DVec2; 3]) {
        let (w, h) = (self.w as f64, self.h as f64);
        let min_y = t[0].y.min(t[1].y).min(t[2].y);
        let max_y = t[0].y.max(t[1].y).max(t[2].y);
        let min_x = t[0].x.min(t[1].x).min(t[2].x);
        let max_x = t[0].x.max(t[1].x).max(t[2].x);
        if max_y < 0.0 || min_y > h || max_x < 0.0 || min_x > w || !(min_y.is_finite() && max_y.is_finite()) {
            return;
        }
        let y0 = min_y.floor().max(0.0) as usize;
        let y1 = (max_y.ceil().min(h)) as usize;
        let mut row = std::mem::take(&mut self.row);
        for y in y0..y1 {
            let (mut lo, mut hi) = (usize::MAX, 0usize);
            for sub in 0..4 {
                let sy = y as f64 + (sub as f64 + 0.5) * 0.25;
                // Where the sub-scanline crosses the triangle's edges.
                let (mut xl, mut xr) = (f64::INFINITY, f64::NEG_INFINITY);
                for i in 0..3 {
                    let (p, q) = (t[i], t[(i + 1) % 3]);
                    if (p.y <= sy && q.y > sy) || (q.y <= sy && p.y > sy) {
                        let x = p.x + (sy - p.y) * (q.x - p.x) / (q.y - p.y);
                        xl = xl.min(x);
                        xr = xr.max(x);
                    }
                }
                if xl >= xr {
                    continue;
                }
                let (xl, xr) = (xl.max(0.0), xr.min(w));
                if xl >= xr {
                    continue;
                }
                let (i0, i1) = (xl.floor() as usize, (xr.ceil() as usize).min(self.w));
                lo = lo.min(i0);
                hi = hi.max(i1);
                for (x, cell) in row.iter_mut().enumerate().take(i1).skip(i0) {
                    let overlap = (xr.min(x as f64 + 1.0) - xl.max(x as f64)).max(0.0);
                    *cell += (overlap * 0.25) as f32;
                }
            }
            if lo < hi {
                for (x, cell) in row.iter_mut().enumerate().take(hi).skip(lo) {
                    let cover = std::mem::take(cell);
                    if cover > 0.0 {
                        self.mask_add(x, y, cover.min(1.0));
                    }
                }
            }
        }
        self.row = row;
    }

    //endregion
}

/// Calls `f(x, y, coverage)` for the pixels within `r` pixels (plus a
/// pixel of anti-aliasing) of the segment `a`-`b`, which has round ends.
fn segment(w: usize, h: usize, a: DVec2, b: DVec2, r: f64, mut f: impl FnMut(usize, usize, f32)) {
    let (w, h) = (w as f64, h as f64);
    let pad = r + 1.0;
    if a.x.max(b.x) + pad < 0.0 || a.x.min(b.x) - pad > w || a.y.max(b.y) + pad < 0.0 || a.y.min(b.y) - pad > h {
        return;
    }
    let d = b - a;
    let len2 = d.length_squared();
    let cov = |px: f64, py: f64| -> f32 {
        let p = DVec2::new(px, py);
        let t = if len2 > 0.0 { ((p - a).dot(d) / len2).clamp(0.0, 1.0) } else { 0.0 };
        let dist = (a + d * t - p).length();
        (r + 0.5 - dist).clamp(0.0, 1.0) as f32
    };
    let len = len2.sqrt();
    if d.x.abs() >= d.y.abs() {
        // Walk the columns.
        let (x0, x1) = (a.x.min(b.x) - pad, a.x.max(b.x) + pad);
        let (cx0, cx1) = (x0.floor().max(0.0) as i64, (x1.ceil().min(w)) as i64);
        let stretch = if d.x.abs() > 1e-12 { len / d.x.abs() } else { 1.0 };
        let half = pad * stretch;
        for x in cx0..cx1 {
            let cx = x as f64 + 0.5;
            // The line's height at the column, nearest the segment.
            let yc = if d.x.abs() > 1e-12 {
                let t = ((cx - a.x) / d.x).clamp(0.0, 1.0);
                a.y + d.y * t
            } else {
                a.y
            };
            let (y0, y1) = ((yc - half).floor().max(0.0) as i64, ((yc + half).ceil().min(h)) as i64);
            for y in y0..y1 {
                let c = cov(cx, y as f64 + 0.5);
                if c > 0.0 {
                    f(x as usize, y as usize, c);
                }
            }
        }
    } else {
        let (y0, y1) = (a.y.min(b.y) - pad, a.y.max(b.y) + pad);
        let (cy0, cy1) = (y0.floor().max(0.0) as i64, (y1.ceil().min(h)) as i64);
        let stretch = len / d.y.abs();
        let half = pad * stretch;
        for y in cy0..cy1 {
            let cy = y as f64 + 0.5;
            let t = ((cy - a.y) / d.y).clamp(0.0, 1.0);
            let xc = a.x + d.x * t;
            let (x0, x1) = ((xc - half).floor().max(0.0) as i64, ((xc + half).ceil().min(w)) as i64);
            for x in x0..x1 {
                let c = cov(x as f64 + 0.5, cy);
                if c > 0.0 {
                    f(x as usize, y as usize, c);
                }
            }
        }
    }
}

impl Drawing {
    /// Draws the layers `shown` marks (one entry per layer) as `view` sees
    /// them, on paper of `paint`'s colour.
    pub fn render(&self, view: &View, shown: &[bool], paint: &Paint) -> Image8 {
        let mut cv = Canvas::new(view.width.max(1), view.height.max(1), paint.background());
        // A view that isn't a view shows only the paper.
        if !(view.scale.is_finite() && view.scale > 0.0 && view.center.is_finite()) {
            return Image8 { width: view.width.max(1), height: view.height.max(1), pixels: cv.pixels };
        }
        let r = (paint.line_px * 0.5).max(0.25);
        let (lo, hi) = view.world_box();
        // Items are skipped when their box is outside the view, with a
        // margin for the width of the line.
        let m = (r + 2.0) / view.scale;
        let outside = |min: [f64; 2], max: [f64; 2]| {
            max[0] < lo[0] - m || min[0] > hi[0] + m || max[1] < lo[1] - m || min[1] > hi[1] + m
        };
        // 1. Areas under the lines.
        for f in &self.fills {
            if !Drawing::is_shown(shown, f.layer, f.owner) || outside(f.min, f.max) {
                continue;
            }
            for t in self.fill_verts(f).chunks_exact(3) {
                cv.triangle([view.to_screen(t[0]), view.to_screen(t[1]), view.to_screen(t[2])]);
            }
            cv.flush(paint.resolve(f.color));
        }
        // 2. Lines.
        let mut screen: Vec<DVec2> = Vec::new();
        for p in &self.polys {
            if !Drawing::is_shown(shown, p.layer, p.owner) || outside(p.min, p.max) {
                continue;
            }
            screen.clear();
            let mut last = DVec2::splat(f64::NAN);
            for q in self.poly_pts(p) {
                let s = view.to_screen(*q);
                // Points less than a pixel apart add nothing but work.
                if (s - last).length_squared() >= 0.04 || screen.is_empty() {
                    screen.push(s);
                    last = s;
                }
            }
            if screen.len() == 1 {
                // The whole line is within a pixel: a dot.
                cv.dot(screen[0], r, paint.resolve(p.color));
            } else {
                cv.polyline(&screen, r, paint.resolve(p.color));
            }
        }
        // 3. Arcs and circles.
        for a in &self.arcs {
            if !Drawing::is_shown(shown, a.layer, a.owner) {
                continue;
            }
            let (min, max) = a.bbox();
            if outside(min, max) {
                continue;
            }
            self.draw_arc(&mut cv, view, a, r, paint, &mut screen);
        }
        // 4. Points.
        for d in &self.dots {
            if Drawing::is_shown(shown, d.layer, d.owner) && !outside([d.p.x, d.p.y], [d.p.x, d.p.y]) {
                cv.dot(view.to_screen(d.p), (r * 1.8).max(1.5), paint.resolve(d.color));
            }
        }
        Image8 { width: view.width.max(1), height: view.height.max(1), pixels: cv.pixels }
    }

    fn draw_arc(&self, cv: &mut Canvas, view: &View, a: &Arc, r: f64, paint: &Paint, screen: &mut Vec<DVec2>) {
        let radius_px = a.u.truncate().length().max(a.v.truncate().length()) * view.scale;
        let color = paint.resolve(a.color);
        if !(radius_px.is_finite()) || radius_px < 0.4 {
            cv.dot(view.to_screen(a.c), r, color);
            return;
        }
        let sweep = a.t1 - a.t0;
        // Segments short enough to leave 0.15 px between chord and arc.
        let step = (2.0 * (1.0 - (0.15 / radius_px).min(0.5)).acos()).max(0.002);
        let n = ((sweep.abs() / step).ceil() as usize).clamp(2, 4096);
        screen.clear();
        for i in 0..=n {
            let t = a.t0 + sweep * i as f64 / n as f64;
            screen.push(view.to_screen(a.c + a.u * t.cos() + a.v * t.sin()));
        }
        cv.polyline(screen, r, color);
    }

    /// A picture of the whole drawing fitted into `width` by `height`.
    pub fn render_fit(&self, width: u32, height: u32, shown: &[bool], paint: &Paint) -> Image8 {
        let view = match &self.bounds {
            Some(b) => View::fit(b, width, height, 0.04),
            None => View { center: DVec2::ZERO, scale: 1.0, width, height },
        };
        self.render(&view, shown, paint)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::drawing::{DrawLayer, Fill, Poly};

    fn drawing_with_line(a: DVec3, b: DVec3, color: u32) -> Drawing {
        let mut d = Drawing::default();
        d.layers.push(DrawLayer { name: "0".into(), color, visible: true, items: 1 });
        d.pts.extend([a, b]);
        d.polys.push(Poly {
            layer: 0,
            owner: 0,
            color,
            first: 0,
            len: 2,
            min: [a.x.min(b.x), a.y.min(b.y)],
            max: [a.x.max(b.x), a.y.max(b.y)],
        });
        d.finish();
        d
    }

    fn px(img: &Image8, x: u32, y: u32) -> [u8; 3] {
        let i = ((y * img.width + x) * 4) as usize;
        [img.pixels[i], img.pixels[i + 1], img.pixels[i + 2]]
    }

    #[test]
    fn a_horizontal_line_is_drawn_where_it_should() {
        let d = drawing_with_line(DVec3::new(0.0, 0.0, 0.0), DVec3::new(10.0, 0.0, 0.0), 0xFF0000);
        let view = View { center: DVec2::new(5.0, 0.0), scale: 10.0, width: 100, height: 40 };
        let img = d.render(&view, &[true], &Paint::LIGHT);
        // The line crosses the middle row from x = 0 to x = 100.
        let on = px(&img, 50, 19);
        let on2 = px(&img, 50, 20);
        assert!(on[0] > 200 && on[1] < 150, "{on:?}");
        assert!(on2[1] < 250, "{on2:?}");
        assert_eq!(px(&img, 50, 5), Paint::LIGHT.background());
        assert_eq!(px(&img, 50, 35), Paint::LIGHT.background());
    }

    #[test]
    fn y_points_up_the_picture() {
        let d = drawing_with_line(DVec3::new(0.0, 0.0, 0.0), DVec3::new(0.0, 10.0, 0.0), 0x0000FF);
        let view = View { center: DVec2::new(0.0, 5.0), scale: 10.0, width: 40, height: 100 };
        let img = d.render(&view, &[true], &Paint::LIGHT);
        assert!(px(&img, 20, 50)[2] > 200 && px(&img, 20, 50)[0] < 200);
        // y = 0 is at the bottom, y = 10 at the top.
        let s0 = view.to_screen(DVec3::ZERO);
        let s1 = view.to_screen(DVec3::new(0.0, 10.0, 0.0));
        assert!(s0.y > s1.y);
        let w = view.to_world(s1);
        assert!((w - DVec2::new(0.0, 10.0)).length() < 1e-9);
    }

    #[test]
    fn hidden_layers_are_not_drawn() {
        let d = drawing_with_line(DVec3::new(0.0, 0.0, 0.0), DVec3::new(10.0, 0.0, 0.0), 0xFF0000);
        let view = View { center: DVec2::new(5.0, 0.0), scale: 10.0, width: 100, height: 40 };
        let img = d.render(&view, &[false], &Paint::LIGHT);
        assert!(img.pixels.chunks_exact(4).all(|p| p[..3] == Paint::LIGHT.background()));
    }

    #[test]
    fn diagonal_lines_have_even_width() {
        let d = drawing_with_line(DVec3::new(0.0, 0.0, 0.0), DVec3::new(10.0, 10.0, 0.0), FOREGROUND);
        let view = View::fit(&d.bounds.unwrap(), 100, 100, 0.1);
        let img = d.render(&view, &[true], &Paint { line_px: 3.0, ..Paint::LIGHT });
        // Count dark coverage per row away from the ends: about the line's
        // width divided by the cosine of the slope.
        let bg = Paint::LIGHT.background()[0] as f64;
        let fg = Paint::LIGHT.resolve(FOREGROUND)[0] as f64;
        let sum: f64 = (0..100).map(|x| bg - px(&img, x, 50)[0] as f64).sum::<f64>() / (bg - fg);
        // A line 3 px wide crosses a row of pixels 3 / cos(45 deg) wide.
        assert!((sum - 3.0 * std::f64::consts::SQRT_2).abs() < 0.4, "{sum}");
    }

    #[test]
    fn white_is_black_on_paper_and_white_on_dark() {
        assert_eq!(Paint::LIGHT.resolve(FOREGROUND)[0], 28);
        assert_eq!(Paint::DARK.resolve(FOREGROUND)[0], 236);
        // A yellow that would vanish on white paper is darkened.
        let yellow = Paint::LIGHT.resolve(0xFFFF00);
        assert!(yellow[0] < 200 && yellow[2] == 0);
        // A dark blue that would vanish on black paper is lightened.
        let blue = Paint::DARK.resolve(0x0000FF);
        assert!(blue[2] == 255 && blue[0] > 40);
        // Mid colours stay as they are.
        assert_eq!(Paint::LIGHT.resolve(0x2060C0), [0x20, 0x60, 0xC0]);
    }

    #[test]
    fn fills_cover_their_area_without_seams() {
        let mut d = Drawing::default();
        d.layers.push(DrawLayer { name: "0".into(), color: 0, visible: true, items: 1 });
        // A square of two triangles, offset by half a pixel.
        let (a, b, c, e) = (
            DVec3::new(0.0, 0.0, 0.0),
            DVec3::new(10.0, 0.0, 0.0),
            DVec3::new(10.0, 10.0, 0.0),
            DVec3::new(0.0, 10.0, 0.0),
        );
        d.fill_pts.extend([a, b, c, a, c, e]);
        d.fills.push(Fill {
            layer: 0,
            owner: 0,
            color: 0x0000FF,
            first: 0,
            len: 6,
            lit: false,
            min: [0.0, 0.0],
            max: [10.0, 10.0],
        });
        d.finish();
        let view = View { center: DVec2::new(5.5, 5.5), scale: 4.0, width: 60, height: 60 };
        let img = d.render(&view, &[true], &Paint::LIGHT);
        // Inside, including along the diagonal where the triangles meet.
        for i in 12..44 {
            let p = px(&img, i, 60 - i);
            assert!(p[2] > 240 && p[0] < 15, "({i}, {}) = {p:?}", 60 - i);
        }
        assert_eq!(px(&img, 2, 2), Paint::LIGHT.background());
    }

    #[test]
    fn circles_are_round_at_any_zoom() {
        let mut d = Drawing::default();
        d.layers.push(DrawLayer { name: "0".into(), color: 0, visible: true, items: 1 });
        d.arcs.push(Arc {
            layer: 0,
            owner: 0,
            color: FOREGROUND,
            c: DVec3::ZERO,
            u: DVec3::new(1000.0, 0.0, 0.0),
            v: DVec3::new(0.0, 1000.0, 0.0),
            t0: 0.0,
            t1: std::f64::consts::TAU,
        });
        d.finish();
        // Zoomed in on the right-hand edge: the edge is a nearly vertical
        // line, not a flat chord of a coarse polygon.
        let view = View { center: DVec2::new(1000.0, 0.0), scale: 2.0, width: 100, height: 100 };
        let img = d.render(&view, &[true], &Paint { line_px: 2.0, ..Paint::LIGHT });
        let dark_x = |y: u32| (0..100).find(|&x| px(&img, x, y)[0] < 128);
        let (a, b) = (dark_x(5).unwrap() as i32, dark_x(50).unwrap() as i32);
        assert!((a - b).abs() <= 1, "{a} {b}");
    }

    #[test]
    fn giant_and_degenerate_shapes_do_not_hang_or_panic() {
        let d = drawing_with_line(DVec3::new(-1.0e12, -1.0e12, 0.0), DVec3::new(1.0e12, 1.0e12, 0.0), 0xFF0000);
        let view = View { center: DVec2::ZERO, scale: 1.0, width: 200, height: 200 };
        let img = d.render(&view, &[true], &Paint::LIGHT);
        assert!(img.pixels.chunks_exact(4).any(|p| p[1] < 100));
        let d = drawing_with_line(DVec3::ZERO, DVec3::ZERO, 0xFF0000);
        d.render(&View { center: DVec2::ZERO, scale: 1.0, width: 20, height: 20 }, &[true], &Paint::LIGHT);
        let nan = View { center: DVec2::new(f64::NAN, 0.0), scale: 1.0, width: 20, height: 20 };
        d.render(&nan, &[true], &Paint::LIGHT);
        let tiny = View { center: DVec2::ZERO, scale: 1.0e-30, width: 20, height: 20 };
        d.render(&tiny, &[true], &Paint::LIGHT);
    }
}
