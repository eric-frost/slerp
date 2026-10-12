//! Drawing text.  A thumbnailer can't count on any font being installed,
//! so glyphs come from a built-in stroke font (see `hershey`), or from a
//! system font when one is found (`sysfont`).  Positions follow GDI: the
//! reference point and alignment flags, the font's height and angle, and the
//! per-character advances that `ExtTextOut` carries, which keep the layout
//! right even though the letters are not the original font's.

use crate::gdi::Gdi;
use crate::hershey;
use crate::state::*;
use tiny_skia::{BlendMode, FillRule, LineCap, LineJoin, Path, Stroke};

use crate::affine::Affine;
use crate::canvas::Source;
use crate::shapes::Geo;

pub const ETO_OPAQUE: u32 = 2;
pub const ETO_CLIPPED: u32 = 4;

/// Letters' height from the baseline to the top of a capital, and the
/// space above and below the baseline in a typical text cell, as fractions
/// of the em size.  A font given by cell height (positive `lfHeight`) is
/// this much taller than its em.
const ASCENT: f64 = 0.905;
const DESCENT: f64 = 0.212;

/// What the text drawing keeps between calls: the system fonts found.
#[derive(Default)]
pub struct TextEngine {
    pub fonts: crate::sysfont::SystemFonts,
}

use crate::sysfont::Resolved;

/// One glyph ready to draw, in em units.
enum Shape {
    /// Polylines, drawn with a pen.
    Strokes(&'static [Vec<(f32, f32)>]),
    /// A filled outline.
    Outline(Path),
    /// Nothing visible (spaces and control characters).
    Blank,
    /// A box for a character no font could draw.
    Missing,
}

struct Glyph {
    advance: f64,
    shape: Shape,
}

impl Gdi {
    fn em_size(&self, font: &Font) -> f64 {
        let h = font.height;
        if h < 0.0 {
            -h
        } else if h > 0.0 {
            h / (ASCENT + DESCENT)
        } else {
            // No size given: twelve device pixels.
            let s = self.dc.total.scale_factor();
            if s > 0.0 { 12.0 * self.base.scale_factor() / s } else { 12.0 }
        }
    }

    fn glyph(&mut self, resolved: Option<&Resolved>, c: char) -> Glyph {
        if c.is_control() || c == '\u{200B}' {
            return Glyph { advance: 0.0, shape: Shape::Blank };
        }
        if let Some((outline, advance)) = resolved.and_then(|r| self.text.fonts.glyph(r, c)) {
            let shape = outline.map_or(Shape::Blank, Shape::Outline);
            return Glyph { advance, shape };
        }
        match hershey::glyph(c) {
            Some(g) if g.strokes.is_empty() => Glyph { advance: g.advance as f64, shape: Shape::Blank },
            Some(g) => Glyph { advance: g.advance as f64, shape: Shape::Strokes(&g.strokes) },
            None if c.is_whitespace() => Glyph { advance: 0.3, shape: Shape::Blank },
            None => Glyph { advance: 0.6, shape: Shape::Missing },
        }
    }

    /// Draws `chars` with their reference point at `pos` (logical units).
    /// `dx` holds the advance of each character in logical units when the
    /// record gives them.
    pub fn text_out(&mut self, pos: (f64, f64), chars: &[char], dx: &[f64], opts: u32, rect: Option<[f64; 4]>) {
        if chars.is_empty() || self.in_path() {
            if self.in_path() {
                self.warnings.skip("text in a path");
            }
            return;
        }
        let font = self.dc.font.clone();
        let em = self.em_size(&font);
        if !(em.is_finite() && em > 0.0) {
            return;
        }
        let chars = &chars[..chars.len().min(4096)];
        let align = self.dc.text_align;
        let origin = if align & TA_UPDATECP != 0 { self.dc.cur } else { pos };

        // 1. Each character's advance, in logical units.
        let resolved = self.text.fonts.resolve(&font);
        let mut glyphs: Vec<Glyph> = Vec::with_capacity(chars.len());
        let mut advances: Vec<f64> = Vec::with_capacity(chars.len());
        for (i, &c) in chars.iter().enumerate() {
            let g = self.glyph(resolved.as_ref(), c);
            let natural = g.advance * em + self.dc.text_extra;
            advances.push(dx.get(i).copied().filter(|v| v.is_finite()).unwrap_or(natural));
            glyphs.push(g);
        }
        let width: f64 = advances.iter().sum();

        // 2. Where the baseline starts, in a frame whose x runs along the
        //    text and whose y runs down the page, then in logical units.
        let a = (font.escapement / 10.0).to_radians();
        let flips = self.dc.total.det() < 0.0;
        let s = if flips { -1.0 } else { 1.0 };
        let u = (a.cos(), -s * a.sin());
        let v = (a.sin(), s * a.cos());
        let start_x = match align & TA_CENTER {
            TA_RIGHT => -width,
            TA_CENTER => -width / 2.0,
            _ => 0.0,
        };
        let baseline = match align & TA_BASELINE {
            TA_BASELINE => 0.0,
            TA_BOTTOM => -DESCENT * em,
            _ => ASCENT * em,
        };
        let at = |x: f64, y: f64| (origin.0 + x * u.0 + y * v.0, origin.1 + x * u.1 + y * v.1);

        // 3. The background behind the text, then the text itself.
        let saved_clip = self.dc.clip.clone();
        if let Some(r) = rect.filter(|_| opts & ETO_CLIPPED != 0) {
            self.clip_rect(r[0], r[1], r[2], r[3], 1);
        }
        if opts & ETO_OPAQUE != 0 {
            if let Some(r) = rect {
                let mut g = Geo::new(self.dc.total);
                g.rect(r[0], r[1], r[2], r[3]);
                self.fill_solid(g.finish(), self.dc.bk_color, 1.0);
            }
        } else if self.dc.bk_opaque {
            let (c0, c1) = (baseline - ASCENT * em, baseline + DESCENT * em);
            let mut g = Geo::new(self.dc.total);
            let quad = [at(start_x, c0), at(start_x + width, c0), at(start_x + width, c1), at(start_x, c1)];
            g.polygon(&quad);
            self.fill_solid(g.finish(), self.dc.bk_color, 1.0);
        }
        // Slant and thicken only what the font doesn't already have.
        let italic = if font.italic && !resolved.as_ref().is_some_and(|r| r.italic_real) { 0.2 } else { 0.0 };
        let bold_real = resolved.as_ref().is_none_or(|r| r.bold_real);
        let weight = if font.weight == 0 { 400 } else { font.weight };
        let color = self.dc.text_color;
        let alpha = self.dc.text_alpha;
        let mut pen_x = start_x;
        for (g, adv) in glyphs.iter().zip(&advances) {
            if self.out_of_time() {
                break;
            }
            // Glyph space (x right, y up, in ems) to logical coordinates.
            let o = at(pen_x, baseline);
            let xcol = (u.0 * em, u.1 * em);
            let ycol = (-v.0 * em + italic * u.0 * em, -v.1 * em + italic * u.1 * em);
            let m = Affine::new(xcol.0, xcol.1, ycol.0, ycol.1, o.0, o.1);
            let total = m.then(self.dc.total);
            match &g.shape {
                Shape::Blank => {}
                Shape::Strokes(strokes) => {
                    let mut geo = Geo::new(total);
                    for s in strokes.iter() {
                        let pts: Vec<(f64, f64)> = s.iter().map(|p| (p.0 as f64, p.1 as f64)).collect();
                        geo.polyline(&pts);
                    }
                    let w = em * (0.045 + 0.05 * ((weight as f64 - 400.0) / 300.0).clamp(-0.5, 1.5));
                    let px = (w * self.dc.total.scale_factor()).max(0.8);
                    let stroke = Stroke {
                        width: px as f32,
                        line_cap: LineCap::Round,
                        line_join: LineJoin::Round,
                        ..Stroke::default()
                    };
                    if let Some(p) = geo.finish() {
                        self.stroke_solid(&p, &stroke, color, alpha);
                    }
                }
                Shape::Outline(path) => {
                    if let Some(p) = path.clone().transform(total.to_skia()) {
                        self.fill_solid(Some(p.clone()), color, alpha);
                        if weight >= 600 && !bold_real {
                            let px = (em * 0.03 * self.dc.total.scale_factor()).max(0.3);
                            let stroke = Stroke { width: px as f32, line_join: LineJoin::Round, ..Stroke::default() };
                            self.stroke_solid(&p, &stroke, color, alpha);
                        }
                    }
                }
                Shape::Missing => {
                    let mut geo = Geo::new(total);
                    geo.rect(0.08, 0.0, 0.52, 0.72);
                    let px = (em * 0.04 * self.dc.total.scale_factor()).max(0.6);
                    let stroke = Stroke { width: px as f32, ..Stroke::default() };
                    if let Some(p) = geo.finish() {
                        self.stroke_solid(&p, &stroke, color, alpha);
                    }
                }
            }
            pen_x += *adv;
        }

        // 4. Underline and strike-out, as lines along the baseline.
        let line = |this: &mut Gdi, offset: f64, thick: f64| {
            let (p0, p1) = (at(start_x, baseline + offset * em), at(start_x + width, baseline + offset * em));
            let mut g = Geo::new(this.dc.total);
            g.polyline(&[p0, p1]);
            let px = (thick * em * this.dc.total.scale_factor()).max(0.8);
            let stroke = Stroke { width: px as f32, ..Stroke::default() };
            if let Some(p) = g.finish() {
                this.stroke_solid(&p, &stroke, color, alpha);
            }
        };
        if font.underline {
            line(self, 0.1, 0.06);
        }
        if font.strike_out {
            line(self, -0.28, 0.06);
        }
        self.dc.clip = saved_clip;
        if align & TA_UPDATECP != 0 {
            self.dc.cur = at(start_x + width, 0.0);
        }
    }

    /// Fills a canvas-space outline with a plain colour, ignoring the brush.
    pub fn fill_solid(&mut self, path: Option<Path>, color: crate::Rgb, alpha: f32) {
        let Some(path) = path else { return };
        let clip = self.dc.clip.clone();
        let clip = if self.cv.measure { None } else { clip.as_deref() };
        self.cv.fill(&path, &Source::Solid(color, alpha), FillRule::Winding, BlendMode::SourceOver, clip);
    }

    pub fn stroke_solid(&mut self, path: &Path, stroke: &Stroke, color: crate::Rgb, alpha: f32) {
        let clip = self.dc.clip.clone();
        let clip = if self.cv.measure { None } else { clip.as_deref() };
        self.cv.stroke(path, stroke, &Source::Solid(color, alpha), BlendMode::SourceOver, clip);
    }
}
