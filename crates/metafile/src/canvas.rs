//! The drawing surface: tiny-skia's pixmap with the few operations a
//! metafile needs, or, for a first pass over a file whose header gives no
//! usable size, a "measuring" canvas that draws nothing and only records
//! how far the geometry reaches.

use crate::Rgb;
use tiny_skia::{
    BlendMode, FillRule, FilterQuality, Mask, Paint, Path, Pattern, Pixmap, PixmapPaint, PixmapRef, SpreadMode, Stroke,
    Transform,
};

/// What fills a shape.
pub enum Source<'a> {
    Solid(Rgb, f32),
    /// A tiled bitmap, mapped to the canvas by the transform; `true` smooths
    /// it when it is scaled, `false` keeps hard pixels (hatches).
    Tile(PixmapRef<'a>, Transform, bool),
    /// Any other tiny-skia shader, such as a gradient.
    Shader(tiny_skia::Shader<'a>),
}

pub struct Canvas {
    pub pix: Pixmap,
    /// Draws nothing; geometry only grows `bounds`.
    pub measure: bool,
    /// The extent of everything drawn so far while measuring, in the units
    /// the caller drew in: left, top, right, bottom.
    pub bounds: Option<[f32; 4]>,
}

impl Canvas {
    pub fn new(width: u32, height: u32) -> Option<Canvas> {
        Some(Canvas { pix: Pixmap::new(width, height)?, measure: false, bounds: None })
    }

    pub fn measuring() -> Canvas {
        Canvas { pix: Pixmap::new(1, 1).expect("1x1 pixmap"), measure: true, bounds: None }
    }

    pub fn width(&self) -> u32 {
        self.pix.width()
    }

    pub fn height(&self) -> u32 {
        self.pix.height()
    }

    pub fn extend_bounds(&mut self, l: f32, t: f32, r: f32, b: f32) {
        if ![l, t, r, b].iter().all(|v| v.is_finite()) {
            return;
        }
        let nb = match self.bounds {
            Some(o) => [o[0].min(l), o[1].min(t), o[2].max(r), o[3].max(b)],
            None => [l, t, r, b],
        };
        self.bounds = Some(nb);
    }

    fn paint<'a>(src: &'a Source, blend: BlendMode, quality: FilterQuality) -> Paint<'a> {
        let mut p = Paint { anti_alias: true, blend_mode: blend, ..Paint::default() };
        match src {
            Source::Solid(c, a) => p.set_color_rgba8(c[0], c[1], c[2], (a.clamp(0.0, 1.0) * 255.0).round() as u8),
            Source::Tile(pm, xf, smooth) => {
                let q = if *smooth { quality } else { FilterQuality::Nearest };
                p.shader = Pattern::new(*pm, SpreadMode::Repeat, q, 1.0, *xf)
            }
            Source::Shader(s) => p.shader = s.clone(),
        }
        p
    }

    /// Fills `path`, given in canvas pixels.
    pub fn fill(&mut self, path: &Path, src: &Source, rule: FillRule, blend: BlendMode, clip: Option<&Mask>) {
        if self.measure {
            let b = path.bounds();
            self.extend_bounds(b.left(), b.top(), b.right(), b.bottom());
            return;
        }
        let paint = Self::paint(src, blend, FilterQuality::Bilinear);
        let pix = &mut self.pix;
        guarded(|| pix.fill_path(path, &paint, rule, Transform::identity(), clip));
    }

    /// Strokes `path`, given in canvas pixels, with a width in pixels.
    pub fn stroke(&mut self, path: &Path, stroke: &Stroke, src: &Source, blend: BlendMode, clip: Option<&Mask>) {
        if self.measure {
            let b = path.bounds();
            let h = stroke.width * 0.5;
            self.extend_bounds(b.left() - h, b.top() - h, b.right() + h, b.bottom() + h);
            return;
        }
        let paint = Self::paint(src, blend, FilterQuality::Bilinear);
        let pix = &mut self.pix;
        guarded(|| pix.stroke_path(path, &paint, stroke, Transform::identity(), clip));
    }

    /// Draws a bitmap whose pixel `(0, 0)` lands where `xf` puts it.
    pub fn draw_bitmap(&mut self, bmp: &Pixmap, xf: Transform, smooth: bool, opacity: f32, clip: Option<&Mask>) {
        if self.measure {
            let (w, h) = (bmp.width() as f32, bmp.height() as f32);
            let mut l = f32::MAX;
            let mut t = f32::MAX;
            let mut r = f32::MIN;
            let mut b = f32::MIN;
            for (x, y) in [(0.0, 0.0), (w, 0.0), (w, h), (0.0, h)] {
                let px = xf.sx * x + xf.kx * y + xf.tx;
                let py = xf.ky * x + xf.sy * y + xf.ty;
                l = l.min(px);
                r = r.max(px);
                t = t.min(py);
                b = b.max(py);
            }
            self.extend_bounds(l, t, r, b);
            return;
        }
        let paint = PixmapPaint {
            opacity,
            blend_mode: BlendMode::SourceOver,
            quality: if smooth { FilterQuality::Bilinear } else { FilterQuality::Nearest },
        };
        let pix = &mut self.pix;
        guarded(|| pix.draw_pixmap(0, 0, bmp.as_ref(), &paint, xf, clip));
    }

    /// Applies a bitwise raster operation over the pixels of an integer
    /// rectangle.  `f` gets the pixel position and the destination colour,
    /// composited over white (a metafile assumes a white page, and its masks
    /// and inversions only make sense on one), and returns the new colour or
    /// `None` to leave the pixel alone.  Touched pixels become opaque, to the
    /// degree the clip allows.
    pub fn rop_rect(&mut self, rect: [i32; 4], clip: Option<&Mask>, mut f: impl FnMut(i32, i32, Rgb) -> Option<Rgb>) {
        let (w, h) = (self.width() as i32, self.height() as i32);
        let (x0, y0, x1, y1) = (rect[0].max(0), rect[1].max(0), rect[2].min(w), rect[3].min(h));
        if self.measure {
            self.extend_bounds(rect[0] as f32, rect[1] as f32, rect[2] as f32, rect[3] as f32);
            return;
        }
        let stride = w as usize * 4;
        for y in y0..y1 {
            for x in x0..x1 {
                let i = y as usize * stride + x as usize * 4;
                let d = &mut self.pix.data_mut()[i..i + 4];
                let a = d[3] as u32;
                // Premultiplied over white: c + (255 - a).
                let over = [
                    (d[0] as u32 + 255 - a).min(255) as u8,
                    (d[1] as u32 + 255 - a).min(255) as u8,
                    (d[2] as u32 + 255 - a).min(255) as u8,
                ];
                let Some(new) = f(x, y, over) else { continue };
                let cov = clip.map_or(255, |m| m.data()[y as usize * w as usize + x as usize] as u32);
                if cov == 0 {
                    continue;
                }
                for k in 0..3 {
                    d[k] = ((new[k] as u32 * cov + d[k] as u32 * (255 - cov)) / 255).min(255) as u8;
                }
                d[3] = (cov + a * (255 - cov) / 255).min(255) as u8;
            }
        }
    }

    /// The finished picture as straight RGBA, optionally over a background.
    pub fn into_rgba(self, background: Option<Rgb>) -> Vec<u8> {
        let mut out = self.pix.take();
        for p in out.chunks_exact_mut(4) {
            let a = p[3] as u32;
            match background {
                Some(bg) => {
                    for k in 0..3 {
                        p[k] = (p[k] as u32 + (bg[k] as u32 * (255 - a) + 127) / 255).min(255) as u8;
                    }
                    p[3] = 255;
                }
                None if a != 0 && a != 255 => {
                    for c in p.iter_mut().take(3) {
                        *c = ((*c as u32 * 255 + a / 2) / a).min(255) as u8;
                    }
                }
                None => {}
            }
        }
        out
    }
}

/// Runs a rasteriser call, dropping the drawing if tiny-skia asserts on
/// geometry it can't handle (hostile files make such paths): one bad shape
/// shouldn't cost the rest of the picture, or the process.
pub(crate) fn guarded(f: impl FnOnce()) {
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
}

/// Like `guarded`, for a call that gives a value.
pub(crate) fn guarded_value<T>(f: impl FnOnce() -> T) -> Option<T> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).ok()
}

/// How a pen's raster operation changes what it draws: the colour to use
/// and the blend mode, or `None` for operations that leave the page alone.
pub fn rop2_paint(rop2: u32, color: Rgb) -> Option<(Rgb, BlendMode)> {
    let inv = |c: Rgb| [255 - c[0], 255 - c[1], 255 - c[2]];
    Some(match rop2 {
        1 => ([0, 0, 0], BlendMode::SourceOver),
        3 => (inv(color), BlendMode::Multiply),
        4 => (inv(color), BlendMode::SourceOver),
        6 => ([255, 255, 255], BlendMode::Difference),
        7 => (color, BlendMode::Difference),
        9 => (color, BlendMode::Multiply),
        11 => return None,
        12 => (inv(color), BlendMode::Screen),
        15 => (color, BlendMode::Screen),
        16 => ([255, 255, 255], BlendMode::SourceOver),
        _ => (color, BlendMode::SourceOver),
    })
}

/// Evaluates a ternary raster operation (the high word of a `ROP3` code is
/// a truth table over pattern, source and destination bits) on one colour
/// channel.
pub fn rop3(code: u32, p: u8, s: u8, d: u8) -> u8 {
    let table = (code >> 16) as u8;
    let mut out = 0u8;
    for bit in 0..8 {
        let idx = (((p >> bit) & 1) << 2) | (((s >> bit) & 1) << 1) | ((d >> bit) & 1);
        out |= ((table >> idx) & 1) << bit;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rop3_known_codes() {
        assert_eq!(rop3(0x00CC0020, 7, 0xAA, 0x55), 0xAA, "SRCCOPY");
        assert_eq!(rop3(0x00EE0086, 0, 0xF0, 0x0F), 0xFF, "SRCPAINT is OR");
        assert_eq!(rop3(0x008800C6, 0, 0xF0, 0x3C), 0x30, "SRCAND is AND");
        assert_eq!(rop3(0x00660046, 0, 0xFF, 0x0F), 0xF0, "SRCINVERT is XOR");
        assert_eq!(rop3(0x00550009, 0, 0, 0x0F), 0xF0, "DSTINVERT");
        assert_eq!(rop3(0x00000042, 1, 2, 3), 0, "BLACKNESS");
        assert_eq!(rop3(0x00FF0062, 1, 2, 3), 255, "WHITENESS");
        assert_eq!(rop3(0x00F00021, 0x3C, 0, 0), 0x3C, "PATCOPY");
        assert_eq!(rop3(0x00330008, 0, 0x0F, 0), 0xF0, "NOTSRCCOPY");
    }

    #[test]
    fn rop_rect_works_over_white() {
        let mut cv = Canvas::new(4, 4).unwrap();
        cv.rop_rect([0, 0, 2, 2], None, |_, _, d| Some([255 - d[0], 255 - d[1], 255 - d[2]]));
        // A transparent pixel counts as white, so inverting gives black.
        let out = cv.into_rgba(None);
        assert_eq!(&out[0..4], &[0, 0, 0, 255]);
        assert_eq!(&out[(2 * 4 + 2) * 4..(2 * 4 + 2) * 4 + 4], &[0, 0, 0, 0]);
    }
}
