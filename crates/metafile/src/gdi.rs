//! The device context machinery shared by the WMF and EMF players: the
//! current mapping and the objects selected into it, and the drawing calls
//! (shapes, paths, bitmaps, clipping) that turn records into pixels on a
//! canvas.  The players only decode records and call into this.

use crate::Rgb;
use crate::affine::Affine;
use crate::canvas::{Canvas, Source, rop2_paint, rop3};
use crate::dib::Bitmap;
use crate::shapes::{ArcKind, Geo, arc_spec};
use crate::state::*;
use crate::text::TextEngine;
use std::collections::{BTreeMap, HashMap};
use std::rc::Rc;
use std::time::Instant;
use tiny_skia::{
    BlendMode, FillRule, LineCap, LineJoin, Mask, Path, PathBuilder, PathSegment, Pixmap, Stroke, StrokeDash, Transform,
};

/// The widest line drawn, in canvas pixels: a pen wider than the picture
/// only fills it, and absurd widths confuse the rasteriser.
const MAX_LINE: f64 = 16384.0;
/// The thinnest line drawn, in canvas pixels.  A one-pixel line in a large
/// drawing shrunk to thumbnail size would otherwise fade to nothing.
const MIN_LINE: f64 = 1.0;
/// The deepest `SaveDC` nesting honoured.
const MAX_SAVED: usize = 32;
/// The most objects a file may keep alive at once.
const MAX_OBJECTS: usize = 1 << 16;
/// The most pixels in a pattern brush's tile.
const MAX_TILE_PIXELS: u64 = 1 << 18;
/// Bytes a replay may keep alive in objects (pattern tiles, regions, images)
/// beyond the canvas and the file itself.
const RETAINED_BUDGET: usize = 192 << 20;

/*┌───────────────────╮
| Objects             |
└───────────────────╯*/
//region Objects

/// A region: a set of rectangles.
#[derive(Debug)]
pub struct Region {
    pub rects: Vec<[f64; 4]>,
    /// True when the rectangles are in device units, false for logical.
    pub device: bool,
}

/// A GDI object in the table a metafile builds.
#[derive(Clone)]
pub enum Object {
    Pen(Rc<Pen>),
    Brush(Rc<Brush>),
    Font(Rc<Font>),
    Region(Rc<Region>),
    Palette(Rc<Vec<Rgb>>),
    /// Something we don't draw with (a bitmap, a colour space), kept so the
    /// slot numbering stays right.
    Other,
}

/// Records that couldn't be played, by name, for the load warnings.
#[derive(Default, Debug)]
pub struct Warnings {
    pub skipped: BTreeMap<String, u32>,
}

impl Warnings {
    pub fn skip(&mut self, what: impl Into<String>) {
        *self.skipped.entry(what.into()).or_insert(0) += 1;
    }
}

//endregion

pub struct Gdi {
    pub cv: Canvas,
    pub dc: Dc,
    saved: Vec<Dc>,
    objects: HashMap<u32, Object>,
    /// Device units to canvas pixels.
    pub base: Affine,
    /// Device pixels per millimetre of the device the picture was made on,
    /// which the fixed mapping modes (`MM_LOMETRIC` and friends) use.
    pub px_per_mm: (f64, f64),
    /// Between `BeginPath` and `EndPath`: the outline so far, in canvas
    /// pixels.
    bracket: Option<PathBuilder>,
    /// The last finished path, which stroke, fill and clip calls consume.
    pub last_path: Option<Path>,
    pub warnings: Warnings,
    /// What is left of the budget for retained objects.
    budget: usize,
    deadline: Instant,
    ticks: u32,
    pub timed_out: bool,
    pub text: TextEngine,
}

impl Gdi {
    pub fn new(cv: Canvas, base: Affine, px_per_mm: (f64, f64), deadline: Instant) -> Gdi {
        let mut g = Gdi {
            cv,
            dc: Dc::new(),
            saved: Vec::new(),
            objects: HashMap::new(),
            base,
            px_per_mm,
            bracket: None,
            last_path: None,
            warnings: Warnings::default(),
            budget: RETAINED_BUDGET,
            deadline,
            ticks: 0,
            timed_out: false,
            text: TextEngine::default(),
        };
        g.update_transform();
        g
    }

    /// Asks for `bytes` of the budget for objects that stay alive between
    /// records.  A hostile file could otherwise keep thousands of large
    /// patterns or images in its object table.
    pub fn reserve(&mut self, bytes: usize) -> bool {
        if bytes <= self.budget {
            self.budget -= bytes;
            true
        } else {
            self.warnings.skip("objects over the memory budget");
            false
        }
    }

    /// Called once per record: true when the time budget is spent, in
    /// which case the player stops and the picture so far is the result.
    pub fn out_of_time(&mut self) -> bool {
        self.ticks = self.ticks.wrapping_add(1);
        if self.ticks.is_multiple_of(32) && Instant::now() > self.deadline {
            self.timed_out = true;
        }
        self.timed_out
    }

    /*┌───────────────────╮
    | Mapping             |
    └───────────────────╯*/
    //region Mapping

    /// Logical to device units, by the mapping mode.
    fn page_to_device(&self) -> Affine {
        let dc = &self.dc;
        let (mx, my) = self.px_per_mm;
        let safe = |v: f64| if v == 0.0 || !v.is_finite() { 1.0 } else { v };
        let (sx, sy) = match dc.map_mode {
            MM_TEXT => (1.0, 1.0),
            MM_LOMETRIC => (mx / 10.0, -my / 10.0),
            MM_HIMETRIC => (mx / 100.0, -my / 100.0),
            MM_LOENGLISH => (mx * 0.254, -my * 0.254),
            MM_HIENGLISH => (mx * 0.0254, -my * 0.0254),
            MM_TWIPS => (mx * 25.4 / 1440.0, -my * 25.4 / 1440.0),
            MM_ISOTROPIC => {
                let (sx, sy) = (dc.vp_ext.0 / safe(dc.win_ext.0), dc.vp_ext.1 / safe(dc.win_ext.1));
                let s = sx.abs().min(sy.abs());
                (s.copysign(sx), s.copysign(sy))
            }
            _ => (dc.vp_ext.0 / safe(dc.win_ext.0), dc.vp_ext.1 / safe(dc.win_ext.1)),
        };
        Affine::new(sx, 0.0, 0.0, sy, dc.vp_org.0 - dc.win_org.0 * sx, dc.vp_org.1 - dc.win_org.1 * sy)
    }

    /// Recomputes the logical-to-canvas transform after a change of
    /// mapping mode, origin, extent or world transform.
    pub fn update_transform(&mut self) {
        let t = self.dc.world.then(self.page_to_device()).then(self.base);
        // A degenerate mapping would collapse everything; keep drawing as
        // if the offending part were identity rather than lose the picture.
        self.dc.total = if t.is_finite() { t } else { self.base };
    }

    pub fn set_map_mode(&mut self, mode: u32) {
        if (MM_TEXT..=MM_ANISOTROPIC).contains(&mode) {
            self.dc.map_mode = mode;
            self.update_transform();
        }
    }

    pub fn set_window_org(&mut self, x: f64, y: f64) {
        self.dc.win_org = (x, y);
        self.update_transform();
    }

    pub fn set_window_ext(&mut self, x: f64, y: f64) {
        self.dc.win_ext = (x, y);
        self.update_transform();
    }

    pub fn set_viewport_org(&mut self, x: f64, y: f64) {
        self.dc.vp_org = (x, y);
        self.update_transform();
    }

    pub fn set_viewport_ext(&mut self, x: f64, y: f64) {
        self.dc.vp_ext = (x, y);
        self.update_transform();
    }

    pub fn scale_window_ext(&mut self, xn: f64, xd: f64, yn: f64, yd: f64) {
        if xd != 0.0 && yd != 0.0 {
            self.dc.win_ext = (self.dc.win_ext.0 * xn / xd, self.dc.win_ext.1 * yn / yd);
            self.update_transform();
        }
    }

    pub fn scale_viewport_ext(&mut self, xn: f64, xd: f64, yn: f64, yd: f64) {
        if xd != 0.0 && yd != 0.0 {
            self.dc.vp_ext = (self.dc.vp_ext.0 * xn / xd, self.dc.vp_ext.1 * yn / yd);
            self.update_transform();
        }
    }

    pub fn set_world(&mut self, w: Affine) {
        self.dc.world = w;
        self.update_transform();
    }

    /// `ModifyWorldTransform`: 1 identity, 2 left multiply, 3 right
    /// multiply, 4 set.
    pub fn modify_world(&mut self, x: Affine, mode: u32) {
        let w = self.dc.world;
        self.dc.world = match mode {
            1 => Affine::IDENTITY,
            2 => x.then(w),
            3 => w.then(x),
            _ => x,
        };
        self.update_transform();
    }

    /// A logical point on the canvas.
    pub fn to_canvas(&self, p: (f64, f64)) -> (f64, f64) {
        self.dc.total.apply(p)
    }

    //endregion

    /*┌───────────────────╮
    | Device context      |
    └───────────────────╯*/
    //region Device context

    pub fn save(&mut self) {
        if self.saved.len() < MAX_SAVED {
            self.saved.push(self.dc.clone());
        }
    }

    /// `RestoreDC`: a negative count goes back that many saves, a positive
    /// one to the n-th save counted from the first.
    pub fn restore(&mut self, n: i32) {
        let len = self.saved.len();
        if n == 0 {
            return;
        }
        let k = if n < 0 { n.unsigned_abs() as usize } else { len.saturating_sub((n as usize).saturating_sub(1)) };
        if k == 0 || k > len {
            return;
        }
        self.dc = self.saved[len - k].clone();
        self.saved.truncate(len - k);
    }

    pub fn set_object(&mut self, index: u32, obj: Object) {
        if self.objects.len() < MAX_OBJECTS || self.objects.contains_key(&index) {
            self.objects.insert(index, obj);
        }
    }

    /// Stores an object in the lowest free slot, as WMF numbers them.
    pub fn add_object(&mut self, obj: Object) {
        let mut i = 0u32;
        while self.objects.contains_key(&i) {
            i += 1;
        }
        self.set_object(i, obj);
    }

    pub fn delete_object(&mut self, index: u32) {
        self.objects.remove(&index);
    }

    pub fn object(&self, index: u32) -> Option<&Object> {
        self.objects.get(&index)
    }

    pub fn select(&mut self, obj: Object) {
        match obj {
            Object::Pen(p) => self.dc.pen = p,
            Object::Brush(b) => self.dc.brush = b,
            Object::Font(f) => self.dc.font = f,
            Object::Palette(p) => self.dc.palette = p,
            Object::Region(_) | Object::Other => {}
        }
        self.update_transform();
    }

    /// Resolves a colour reference's flag byte: palette indices name an
    /// entry of the selected (or the default) palette.
    pub fn color(&self, c: u32) -> Rgb {
        if c & 0xFF00_0000 == PALETTE_INDEX {
            let i = (c & 0xFFFF) as usize;
            let pal = &self.dc.palette;
            if let Some(v) = pal.get(i) {
                return *v;
            }
            return match i {
                0..=9 => SYSTEM_PALETTE[i],
                246..=255 => SYSTEM_PALETTE[i - 246 + 10],
                _ => [0, 0, 0],
            };
        }
        [c as u8, (c >> 8) as u8, (c >> 16) as u8]
    }

    /// A stock object by its `GetStockObject` number.
    pub fn stock(&self, n: u32) -> Option<Object> {
        Some(match n {
            0 => Object::Brush(Rc::new(Brush::Solid([255, 255, 255]))),
            1 => Object::Brush(Rc::new(Brush::Solid([192, 192, 192]))),
            2 => Object::Brush(Rc::new(Brush::Solid([128, 128, 128]))),
            3 => Object::Brush(Rc::new(Brush::Solid([64, 64, 64]))),
            4 => Object::Brush(Rc::new(Brush::Solid([0, 0, 0]))),
            5 => Object::Brush(Rc::new(Brush::Null)),
            6 => Object::Pen(Rc::new(Pen::solid([255, 255, 255]))),
            7 => Object::Pen(Rc::new(Pen::solid([0, 0, 0]))),
            8 => Object::Pen(Rc::new(Pen::none())),
            10..=14 | 16 | 17 => Object::Font(Rc::new(stock_font(n))),
            15 => Object::Palette(Rc::new(SYSTEM_PALETTE.to_vec())),
            18 => Object::Brush(Rc::new(Brush::Solid(self.dc.bk_color))),
            19 => Object::Pen(Rc::new(Pen::solid(self.dc.text_color))),
            _ => return None,
        })
    }

    //endregion

    /*┌───────────────────╮
    | Painting            |
    └───────────────────╯*/
    //region Painting

    /// The tile a pattern brush repeats, as a pixmap, plus the transform
    /// from tile pixels to the canvas.  One tile pixel is one device pixel.
    fn tile_for(&self, brush: &Brush) -> Option<(Pixmap, Transform)> {
        let tile_xf = Affine::translate(self.dc.brush_org.0, self.dc.brush_org.1).then(self.base).to_skia();
        match brush {
            Brush::Hatch(tile, color) => {
                let mut pm = Pixmap::new(tile.width as u32, tile.height as u32)?;
                let bg = if self.dc.bk_opaque { Some(self.dc.bk_color) } else { None };
                for (i, px) in pm.data_mut().chunks_exact_mut(4).enumerate() {
                    let c = if tile.on[i] { Some(*color) } else { bg };
                    if let Some(c) = c {
                        px.copy_from_slice(&[c[0], c[1], c[2], 255]);
                    }
                }
                Some((pm, tile_xf))
            }
            Brush::Mono(tile) => {
                let mut pm = Pixmap::new(tile.width as u32, tile.height as u32)?;
                for (i, px) in pm.data_mut().chunks_exact_mut(4).enumerate() {
                    let c = if tile.on[i] { self.dc.bk_color } else { self.dc.text_color };
                    px.copy_from_slice(&[c[0], c[1], c[2], 255]);
                }
                Some((pm, tile_xf))
            }
            Brush::Pattern(pm) => Some(((**pm).clone(), tile_xf)),
            _ => None,
        }
    }

    pub fn fill_with_brush(&mut self, path: &Path, rule: FillRule) {
        let brush = self.dc.brush.clone();
        let clip = self.dc.clip.clone();
        let clip = if self.cv.measure { None } else { clip.as_deref() };
        match &*brush {
            Brush::Null => {}
            Brush::Solid(c) => {
                if let Some((color, blend)) = rop2_paint(self.dc.rop2, *c) {
                    self.cv.fill(path, &Source::Solid(color, 1.0), rule, blend, clip);
                }
            }
            other => {
                if let Some((pm, xf)) = self.tile_for(other) {
                    let smooth = matches!(other, Brush::Pattern(_));
                    self.cv.fill(path, &Source::Tile(pm.as_ref(), xf, smooth), rule, BlendMode::SourceOver, clip);
                }
            }
        }
    }

    /// The pen's width in canvas pixels, and its dash pattern.
    fn stroke_style(&self, pen: &Pen) -> Option<Stroke> {
        let base_scale = self.base.scale_factor();
        let scale = self.dc.total.scale_factor();
        let one_px = base_scale.max(1e-9);
        let natural = if pen.width <= 0.0 || !pen.geometric && pen.width <= 1.0 { one_px } else { pen.width * scale };
        let width = natural.clamp(MIN_LINE, MAX_LINE);
        let mut stroke = Stroke { width: width as f32, miter_limit: self.dc.miter_limit.max(1.0), ..Stroke::default() };
        if pen.geometric || pen.width > 1.0 {
            stroke.line_cap = pen.cap;
            stroke.line_join = pen.join;
        } else {
            stroke.line_cap = LineCap::Butt;
            stroke.line_join = LineJoin::Miter;
        }
        let unit = if pen.geometric { natural.max(1.0) } else { one_px };
        let pattern: Vec<f32> = match &pen.dashes {
            Dashes::Solid => return Some(stroke),
            Dashes::Dash if pen.geometric => vec![3.0, 1.0],
            Dashes::Dot if pen.geometric => vec![1.0, 1.0],
            Dashes::DashDot if pen.geometric => vec![3.0, 1.0, 1.0, 1.0],
            Dashes::DashDotDot if pen.geometric => vec![3.0, 1.0, 1.0, 1.0, 1.0, 1.0],
            Dashes::Dash => vec![18.0, 6.0],
            Dashes::Dot => vec![3.0, 3.0],
            Dashes::DashDot => vec![9.0, 6.0, 3.0, 6.0],
            Dashes::DashDotDot => vec![9.0, 3.0, 3.0, 3.0, 3.0, 3.0],
            Dashes::Alternate => vec![1.0, 1.0],
            Dashes::User(v) => v.clone(),
        };
        let mut lens: Vec<f32> = pattern.iter().map(|v| (*v as f64 * unit).max(0.0) as f32).collect();
        if lens.len() % 2 == 1 {
            let again = lens.clone();
            lens.extend(again);
        }
        // Hairline dashes shorter than a pixel shimmer; stretch them to a
        // pixel at least.
        for l in &mut lens {
            *l = l.max(0.5);
        }
        stroke.dash = StrokeDash::new(lens, 0.0);
        Some(stroke)
    }

    pub fn stroke_with_pen(&mut self, path: &Path) {
        let pen = self.dc.pen.clone();
        if pen.null {
            return;
        }
        let Some((color, blend)) = rop2_paint(self.dc.rop2, pen.color) else { return };
        let Some(stroke) = self.stroke_style(&pen) else { return };
        let clip = self.dc.clip.clone();
        let clip = if self.cv.measure { None } else { clip.as_deref() };
        self.cv.stroke(path, &stroke, &Source::Solid(color, 1.0), blend, clip);
    }

    fn fill_rule(&self) -> FillRule {
        if self.dc.winding { FillRule::Winding } else { FillRule::EvenOdd }
    }

    /// Draws a finished outline, or adds it to the path being recorded.
    pub fn emit(&mut self, path: Option<Path>, fill: bool, stroke: bool) {
        let Some(path) = path else { return };
        if let Some(b) = &mut self.bracket {
            b.push_path(&path);
            return;
        }
        if fill {
            self.fill_with_brush(&path, self.fill_rule());
        }
        if stroke {
            self.stroke_with_pen(&path);
        }
    }

    fn geo(&self) -> Geo {
        Geo::new(self.dc.total)
    }

    //endregion

    /*┌───────────────────╮
    | Shapes              |
    └───────────────────╯*/
    //region Shapes

    pub fn move_to(&mut self, x: f64, y: f64) {
        self.dc.cur = (x, y);
        if self.bracket.is_some() {
            let (cx, cy) = self.to_canvas((x, y));
            if let Some(b) = &mut self.bracket {
                b.move_to(clamp32(cx), clamp32(cy));
            }
        }
    }

    /// Emits a path whose first point is the current position.  Inside a
    /// path bracket its segments continue the open figure; otherwise it is
    /// drawn as a stroke.
    fn emit_connected(&mut self, path: Option<Path>) {
        let Some(path) = path else { return };
        let Some(b) = &mut self.bracket else {
            return self.emit(Some(path), false, true);
        };
        for seg in path.segments() {
            match seg {
                PathSegment::MoveTo(p) => {
                    if b.is_empty() {
                        b.move_to(p.x, p.y);
                    }
                }
                PathSegment::LineTo(p) => b.line_to(p.x, p.y),
                PathSegment::QuadTo(c, p) => b.quad_to(c.x, c.y, p.x, p.y),
                PathSegment::CubicTo(c1, c2, p) => b.cubic_to(c1.x, c1.y, c2.x, c2.y, p.x, p.y),
                PathSegment::Close => b.close(),
            }
        }
    }

    pub fn line_to(&mut self, x: f64, y: f64) {
        let from = self.dc.cur;
        let mut g = self.geo();
        g.move_to(from.0, from.1);
        g.line_to(x, y);
        self.dc.cur = (x, y);
        self.emit_connected(g.finish());
    }

    /// `PolylineTo`: lines from the current position, which moves to the
    /// last point.
    pub fn polyline_to(&mut self, pts: &[(f64, f64)]) {
        let mut all = Vec::with_capacity(pts.len() + 1);
        all.push(self.dc.cur);
        all.extend_from_slice(pts);
        if let Some(&last) = pts.last() {
            self.dc.cur = last;
        }
        if all.len() < 2 {
            return;
        }
        let mut g = self.geo();
        g.polyline(&all);
        self.emit_connected(g.finish());
    }

    /// Polylines and polygons.  `close` closes the outline (polygons);
    /// `fill` and `stroke` choose what is drawn.
    pub fn poly(&mut self, pts: &[(f64, f64)], close: bool, fill: bool, stroke: bool, _single: bool) {
        if pts.len() < 2 {
            return;
        }
        let mut g = self.geo();
        if close {
            g.polygon(pts);
        } else {
            g.polyline(pts);
        }
        self.emit(g.finish(), fill, stroke);
    }

    /// Several polygons or polylines as one shape, so overlaps follow the
    /// fill mode.
    pub fn poly_poly(&mut self, polys: &[Vec<(f64, f64)>], close: bool) {
        let mut g = self.geo();
        for p in polys {
            if p.len() < 2 {
                continue;
            }
            if close {
                g.polygon(p);
            } else {
                g.polyline(p);
            }
        }
        self.emit(g.finish(), close, true);
    }

    pub fn bezier(&mut self, pts: &[(f64, f64)], from_current: bool) {
        let mut all = Vec::with_capacity(pts.len() + 1);
        if from_current {
            all.push(self.dc.cur);
        }
        all.extend_from_slice(pts);
        let mut g = self.geo();
        g.bezier(&all, false);
        if let Some(&last) = pts.last().filter(|_| all.len() >= 4) {
            self.dc.cur = last;
        }
        if from_current {
            self.emit_connected(g.finish());
        } else {
            self.emit(g.finish(), false, true);
        }
    }

    pub fn rectangle(&mut self, l: f64, t: f64, r: f64, b: f64) {
        let (l, r) = (l.min(r), l.max(r));
        let (t, b) = (t.min(b), t.max(b));
        let (fl, ft, fr, fb) = self.inset(l, t, r, b);
        let mut g = self.geo();
        g.rect(fl, ft, fr, fb);
        self.emit(g.finish(), true, true);
    }

    pub fn round_rect(&mut self, l: f64, t: f64, r: f64, b: f64, cw: f64, ch: f64) {
        let (fl, ft, fr, fb) = self.inset(l.min(r), t.min(b), l.max(r), t.max(b));
        let mut g = self.geo();
        g.round_rect(fl, ft, fr, fb, cw, ch);
        self.emit(g.finish(), true, true);
    }

    pub fn ellipse(&mut self, l: f64, t: f64, r: f64, b: f64) {
        let (fl, ft, fr, fb) = self.inset(l.min(r), t.min(b), l.max(r), t.max(b));
        let mut g = self.geo();
        g.ellipse(fl, ft, fr, fb);
        self.emit(g.finish(), true, true);
    }

    /// `PS_INSIDEFRAME` pens draw inside the shape's box, so the box shrinks
    /// by half the line width.
    fn inset(&self, l: f64, t: f64, r: f64, b: f64) -> (f64, f64, f64, f64) {
        let pen = &self.dc.pen;
        if !pen.inside_frame || pen.width <= 1.0 {
            return (l, t, r, b);
        }
        let h = (pen.width / 2.0).min((r - l) / 2.0).min((b - t) / 2.0).max(0.0);
        (l + h, t + h, r - h, b - h)
    }

    /// The sign that turns "counter-clockwise on screen" into a direction
    /// of logical angle, given any mirroring in the mapping.
    fn increasing_angles(&self, ccw: bool) -> bool {
        let flips = self.dc.total.det() < 0.0;
        // In logical y-down angles counter-clockwise decreases the angle.
        ccw == flips
    }

    /// Arcs, pies and chords from a bounding box and two points that
    /// define the rays.
    pub fn arc_shape(&mut self, kind: ArcKind, bbox: [f64; 4], start: (f64, f64), end: (f64, f64)) {
        let inc = self.increasing_angles(self.dc.arc_ccw);
        let Some(spec) = arc_spec(bbox, start, end, inc) else { return };
        let mut g = self.geo();
        match kind {
            ArcKind::Arc => g.arc(spec.centre, spec.radii, spec.start, spec.sweep, false),
            ArcKind::Pie => {
                g.move_to(spec.centre.0, spec.centre.1);
                g.arc(spec.centre, spec.radii, spec.start, spec.sweep, true);
                g.close();
            }
            ArcKind::Chord => {
                g.arc(spec.centre, spec.radii, spec.start, spec.sweep, false);
                g.close();
            }
        }
        self.emit(g.finish(), kind != ArcKind::Arc, true);
    }

    /// `ArcTo`: an arc preceded by a line from the current position, which
    /// then moves to the arc's end.
    pub fn arc_to(&mut self, bbox: [f64; 4], start: (f64, f64), end: (f64, f64)) {
        let inc = self.increasing_angles(self.dc.arc_ccw);
        let Some(spec) = arc_spec(bbox, start, end, inc) else { return };
        let mut g = self.geo();
        g.move_to(self.dc.cur.0, self.dc.cur.1);
        g.arc(spec.centre, spec.radii, spec.start, spec.sweep, true);
        let t = spec.start + spec.sweep;
        self.dc.cur = (spec.centre.0 + spec.radii.0 * t.cos(), spec.centre.1 + spec.radii.1 * t.sin());
        self.emit_connected(g.finish());
    }

    /// `AngleArc`: a line to the start of a circular arc, then the arc,
    /// angles in degrees counter-clockwise on screen.
    pub fn angle_arc(&mut self, centre: (f64, f64), radius: f64, start_deg: f64, sweep_deg: f64) {
        // Screen counter-clockwise is a decreasing logical angle unless the
        // mapping mirrors y.
        let sign = if self.dc.total.det() < 0.0 { 1.0 } else { -1.0 };
        let t0 = sign * start_deg.to_radians();
        let dt = (sign * sweep_deg.to_radians()).clamp(-std::f64::consts::TAU, std::f64::consts::TAU);
        let mut g = self.geo();
        g.move_to(self.dc.cur.0, self.dc.cur.1);
        g.arc(centre, (radius, radius), t0, dt, true);
        let t = t0 + dt;
        self.dc.cur = (centre.0 + radius * t.cos(), centre.1 + radius * t.sin());
        self.emit_connected(g.finish());
    }

    //endregion

    /*┌───────────────────╮
    | Paths               |
    └───────────────────╯*/
    //region Paths

    pub fn begin_path(&mut self) {
        self.bracket = Some(PathBuilder::new());
        self.last_path = None;
    }

    pub fn end_path(&mut self) {
        if let Some(b) = self.bracket.take() {
            self.last_path = b.finish();
        }
    }

    pub fn abort_path(&mut self) {
        self.bracket = None;
        self.last_path = None;
    }

    pub fn close_figure(&mut self) {
        if let Some(b) = &mut self.bracket {
            b.close();
        }
    }

    pub fn in_path(&self) -> bool {
        self.bracket.is_some()
    }

    /// `StrokePath`, `FillPath`, `StrokeAndFillPath` on the last path.
    pub fn draw_last_path(&mut self, fill: bool, stroke: bool) {
        if let Some(p) = self.last_path.take() {
            if fill {
                self.fill_with_brush(&p, self.fill_rule());
            }
            if stroke {
                self.stroke_with_pen(&p);
            }
        }
    }

    /// `WidenPath`: replaces the last path with the outline of its stroke.
    pub fn widen_path(&mut self) {
        let pen = self.dc.pen.clone();
        if let (Some(p), Some(style)) = (self.last_path.take(), self.stroke_style(&pen)) {
            let mut style = style;
            style.dash = None;
            self.last_path = crate::canvas::guarded_value(|| p.stroke(&style, 1.0)).flatten();
        }
    }

    //endregion

    /*┌───────────────────╮
    | Clipping            |
    └───────────────────╯*/
    //region Clipping

    /// Combines a coverage mask into the clip: 1 AND, 2 OR, 3 XOR,
    /// 4 DIFF (clip minus the shape), 5 COPY.
    pub fn combine_clip(&mut self, shape: Option<Mask>, mode: u32) {
        if self.cv.measure {
            return;
        }
        let Some(mut shape) = shape else { return };
        let (w, h) = (self.cv.width(), self.cv.height());
        let new = match (mode, self.dc.clip.clone()) {
            (5, _) | (1, None) => shape,
            // OR with "everything" stays everything.
            (2, None) => return,
            // XOR with, or subtracting from, everything is the outside.
            (_, None) => {
                shape.invert();
                shape
            }
            (_, Some(old)) => {
                let Some(mut m) = Mask::new(w, h) else { return };
                for ((o, a), b) in m.data_mut().iter_mut().zip(old.data()).zip(shape.data()) {
                    let (a, b) = (*a as u32, *b as u32);
                    *o = match mode {
                        1 => a * b / 255,
                        2 => a + b - a * b / 255,
                        3 => a.abs_diff(b),
                        4 => a * (255 - b) / 255,
                        _ => b,
                    } as u8;
                }
                m
            }
        };
        self.dc.clip = Some(Rc::new(new));
    }

    fn path_mask(&self, path: &Path, rule: FillRule) -> Option<Mask> {
        let mut m = Mask::new(self.cv.width(), self.cv.height())?;
        crate::canvas::guarded(|| m.fill_path(path, rule, true, Transform::identity()));
        Some(m)
    }

    /// Clips with a rectangle given in logical coordinates.
    pub fn clip_rect(&mut self, l: f64, t: f64, r: f64, b: f64, mode: u32) {
        if self.cv.measure {
            return;
        }
        let mut g = self.geo();
        g.rect(l, t, r, b);
        if let Some(p) = g.finish() {
            let m = self.path_mask(&p, FillRule::Winding);
            self.combine_clip(m, mode);
        }
    }

    /// Clips with the last path (`SelectClipPath`).
    pub fn clip_last_path(&mut self, mode: u32) {
        if let Some(p) = self.last_path.take() {
            let m = self.path_mask(&p, self.fill_rule());
            self.combine_clip(m, mode);
        }
    }

    pub fn clip_region(&mut self, region: &Region, mode: u32) {
        if self.cv.measure {
            return;
        }
        let Some(p) = self.region_path(region) else {
            if mode == 5 {
                self.dc.clip = None;
            }
            return;
        };
        let m = self.path_mask(&p, FillRule::Winding);
        self.combine_clip(m, mode);
    }

    pub fn reset_clip(&mut self) {
        self.dc.clip = None;
    }

    /// Moves the clip by a logical offset (`OffsetClipRgn`).
    pub fn offset_clip(&mut self, dx: f64, dy: f64) {
        let (ox, oy) = self.dc.total.apply_vector((dx, dy));
        self.offset_clip_canvas(ox, oy);
    }

    /// Moves the clip by an offset in canvas pixels.
    pub fn offset_clip_canvas(&mut self, ox: f64, oy: f64) {
        let Some(old) = self.dc.clip.clone() else { return };
        let (w, h) = (self.cv.width() as i64, self.cv.height() as i64);
        let Some(mut m) = Mask::new(w as u32, h as u32) else { return };
        let (ox, oy) = (ox.clamp(-1.0e9, 1.0e9).round() as i64, oy.clamp(-1.0e9, 1.0e9).round() as i64);
        for y in 0..h {
            let sy = y - oy;
            if !(0..h).contains(&sy) {
                continue;
            }
            for x in 0..w {
                let sx = x - ox;
                if (0..w).contains(&sx) {
                    m.data_mut()[(y * w + x) as usize] = old.data()[(sy * w + sx) as usize];
                }
            }
        }
        self.dc.clip = Some(Rc::new(m));
    }

    /// A region as a path in canvas pixels.
    pub fn region_path(&self, region: &Region) -> Option<Path> {
        let xf = if region.device { self.base } else { self.dc.total };
        let mut g = Geo::new(xf);
        for r in region.rects.iter().take(1 << 16) {
            g.rect(r[0], r[1], r[2], r[3]);
        }
        g.finish()
    }

    //endregion

    /*┌───────────────────╮
    | Bitmaps             |
    └───────────────────╯*/
    //region Bitmaps

    /// The pattern brush's colour at a canvas pixel, for raster operations
    /// that use the pattern.
    fn pattern_at(&self, x: i32, y: i32) -> Rgb {
        let brush = &*self.dc.brush;
        match brush {
            Brush::Solid(c) => *c,
            Brush::Null => [255, 255, 255],
            other => {
                let inv = self.base.invert().unwrap_or(Affine::IDENTITY);
                let (dx, dy) = inv.apply((x as f64 + 0.5, y as f64 + 0.5));
                let (dx, dy) = (dx - self.dc.brush_org.0, dy - self.dc.brush_org.1);
                match other {
                    Brush::Hatch(t, c) => {
                        let i = (dy.floor().rem_euclid(t.height as f64) as usize) * t.width
                            + dx.floor().rem_euclid(t.width as f64) as usize;
                        if t.on[i] { *c } else { self.dc.bk_color }
                    }
                    Brush::Mono(t) => {
                        let i = (dy.floor().rem_euclid(t.height as f64) as usize) * t.width
                            + dx.floor().rem_euclid(t.width as f64) as usize;
                        if t.on[i] { self.dc.bk_color } else { self.dc.text_color }
                    }
                    Brush::Pattern(pm) => {
                        let (w, h) = (pm.width() as f64, pm.height() as f64);
                        let i = (dy.floor().rem_euclid(h) as usize * pm.width() as usize
                            + dx.floor().rem_euclid(w) as usize)
                            * 4;
                        let d = pm.data();
                        [d[i], d[i + 1], d[i + 2]]
                    }
                    _ => [0, 0, 0],
                }
            }
        }
    }

    /// Fills a rectangle (logical coordinates) with a raster operation that
    /// needs no source: `PatBlt`.
    pub fn pat_blt(&mut self, x: f64, y: f64, w: f64, h: f64, rop: u32) {
        let code = rop & 0x00FF_0000;
        let (l, r) = (x.min(x + w), x.max(x + w));
        let (t, b) = (y.min(y + h), y.max(y + h));
        // The plain cases are paint calls; anything reading the page goes
        // through the per-pixel operation.
        match code {
            0x00F0_0000 => {
                let mut g = self.geo();
                g.rect(l, t, r, b);
                if let Some(p) = g.finish() {
                    self.fill_with_brush(&p, FillRule::Winding);
                }
            }
            _ => self.rop_blt(l, t, r, b, rop, None),
        }
    }

    /// Per-pixel raster operation over an axis-aligned logical rectangle,
    /// optionally with a source bitmap stretched over it.
    fn rop_blt(&mut self, l: f64, t: f64, r: f64, b: f64, rop: u32, src: Option<(&Bitmap, Affine)>) {
        // The rectangle's corners on the canvas; a rotated mapping falls
        // back to the bounding box.
        let pts = [(l, t), (r, t), (r, b), (l, b)].map(|p| self.to_canvas(p));
        let x0 = pts.iter().map(|p| p.0).fold(f64::MAX, f64::min).floor().max(-1.0e6) as i32;
        let x1 = pts.iter().map(|p| p.0).fold(f64::MIN, f64::max).ceil().min(1.0e6) as i32;
        let y0 = pts.iter().map(|p| p.1).fold(f64::MAX, f64::min).floor().max(-1.0e6) as i32;
        let y1 = pts.iter().map(|p| p.1).fold(f64::MIN, f64::max).ceil().min(1.0e6) as i32;
        let (text, bk) = (self.dc.text_color, self.dc.bk_color);
        let inv = src.as_ref().and_then(|(_, m)| m.invert());
        // The pattern needs `self`, which the canvas borrow excludes, so
        // collect what the closure reads first.
        let clip = self.dc.clip.clone();
        let mut colors: HashMap<(i32, i32), Rgb> = HashMap::new();
        if rop & 0x00FF_0000 != 0x00CC_0000 && uses_pattern(rop) {
            for y in y0.max(0)..y1.min(self.cv.height() as i32) {
                for x in x0.max(0)..x1.min(self.cv.width() as i32) {
                    colors.insert((x, y), self.pattern_at(x, y));
                    if colors.len() > 4_000_000 {
                        break;
                    }
                }
            }
        }
        let cv = &mut self.cv;
        let clip = if cv.measure { None } else { clip.as_deref() };
        cv.rop_rect([x0, y0, x1, y1], clip, |x, y, d| {
            let s = match (&src, &inv) {
                (Some((bmp, _)), Some(inv)) => {
                    let (u, v) = inv.apply((x as f64 + 0.5, y as f64 + 0.5));
                    if u < 0.0 || v < 0.0 || u >= bmp.width as f64 || v >= bmp.height as f64 {
                        return None;
                    }
                    let px = bmp.pixel(u as i64, v as i64);
                    if bmp.mono { if px[0] > 127 { bk } else { text } } else { [px[0], px[1], px[2]] }
                }
                _ => [0, 0, 0],
            };
            let p = colors.get(&(x, y)).copied().unwrap_or([0, 0, 0]);
            Some([rop3(rop, p[0], s[0], d[0]), rop3(rop, p[1], s[1], d[1]), rop3(rop, p[2], s[2], d[2])])
        });
    }

    /// Draws part of a bitmap over a destination rectangle: `StretchDIBits`,
    /// `BitBlt` and relatives.  The source rectangle is in bitmap pixels,
    /// the destination in logical units; negative sizes mirror.
    pub fn draw_bitmap(&mut self, bmp: &Bitmap, src: [i64; 4], dst: [f64; 4], rop: u32, alpha: Option<f32>) {
        let (mut sx, mut sy, mut sw, mut sh) = (src[0], src[1], src[2], src[3]);
        let (mut dx, mut dy, mut dw, mut dh) = (dst[0], dst[1], dst[2], dst[3]);
        if sw == 0 || sh == 0 || dw == 0.0 || dh == 0.0 {
            return;
        }
        let (mut flip_x, mut flip_y) = (false, false);
        if sw < 0 {
            sx += sw;
            sw = -sw;
            flip_x = !flip_x;
        }
        if sh < 0 {
            sy += sh;
            sh = -sh;
            flip_y = !flip_y;
        }
        if dw < 0.0 {
            dx += dw;
            dw = -dw;
            flip_x = !flip_x;
        }
        if dh < 0.0 {
            dy += dh;
            dh = -dh;
            flip_y = !flip_y;
        }
        // Keep only the part of the source inside the bitmap, shrinking the
        // destination in proportion.
        let (bw, bh) = (bmp.width as i64, bmp.height as i64);
        let (cx0, cy0) = (sx.max(0), sy.max(0));
        let (cx1, cy1) = ((sx + sw).min(bw), (sy + sh).min(bh));
        if cx0 >= cx1 || cy0 >= cy1 {
            return;
        }
        let fx = |v: i64| (v - sx) as f64 / sw as f64;
        let fy = |v: i64| (v - sy) as f64 / sh as f64;
        let (u0, u1) = (fx(cx0), fx(cx1));
        let (v0, v1) = (fy(cy0), fy(cy1));
        // Destination interval of the kept part (mirrored when flipped).
        let (a0, a1) = if flip_x { (1.0 - u1, 1.0 - u0) } else { (u0, u1) };
        let (b0, b1) = if flip_y { (1.0 - v1, 1.0 - v0) } else { (v0, v1) };
        let (ndx, ndw) = (dx + dw * a0, dw * (a1 - a0));
        let (ndy, ndh) = (dy + dh * b0, dh * (b1 - b0));
        let (cw, ch) = ((cx1 - cx0) as u32, (cy1 - cy0) as u32);
        // Pixels (u, v) of the kept source go to the logical rectangle.
        let sx_scale = ndw / cw as f64 * if flip_x { -1.0 } else { 1.0 };
        let sy_scale = ndh / ch as f64 * if flip_y { -1.0 } else { 1.0 };
        let ox = if flip_x { ndx + ndw } else { ndx };
        let oy = if flip_y { ndy + ndh } else { ndy };
        let to_logical = Affine::new(sx_scale, 0.0, 0.0, sy_scale, ox, oy);
        let xf = to_logical.then(self.dc.total);
        if !xf.is_finite() {
            return;
        }
        let code = rop & 0x00FF_0000;
        if code != 0x00CC_0000 && alpha.is_none() {
            // Raster operations other than a plain copy.
            if matches!(code, 0x0000_0000 | 0x00FF_0000 | 0x0055_0000 | 0x00F0_0000 | 0x005A_0000) {
                return self.pat_blt(ndx, ndy, ndw, ndh, rop);
            }
            let cropped = crop(bmp, cx0, cy0, cw, ch);
            let (l, t, r, b) = (ndx, ndy, ndx + ndw, ndy + ndh);
            return self.rop_blt(l, t, r, b, rop, Some((&cropped, xf)));
        }
        // A plain copy, drawn as a transformed image.  Shrinking by much
        // more than a half would skip pixels, so average down first.
        let cropped = crop(bmp, cx0, cy0, cw, ch);
        let scale = (xf.a.hypot(xf.b)).min(xf.c.hypot(xf.d));
        let (cropped, xf) = if scale > 0.0 && scale < 0.45 {
            let k = ((1.0 / scale).floor() as u32).clamp(2, 64);
            let small = shrink(&cropped, k);
            let kk = k as f64;
            (small, Affine::scale(kk, kk).then(xf))
        } else {
            (cropped, xf)
        };
        let Some(pm) = cropped.to_pixmap() else { return };
        let smooth = scale < 3.0 || !bmp.mono;
        let clip = self.dc.clip.clone();
        let clip = if self.cv.measure { None } else { clip.as_deref() };
        self.cv.draw_bitmap(&pm, xf.to_skia(), smooth, alpha.unwrap_or(1.0), clip);
    }

    //endregion

    /*┌───────────────────╮
    | Gradients           |
    └───────────────────╯*/
    //region Gradients

    /// A rectangle between two vertices filled with a colour ramp from the
    /// first to the second, left to right or top to bottom.
    pub fn gradient_rect(&mut self, a: (f64, f64), ca: Rgb, b: (f64, f64), cb: Rgb, horizontal: bool) {
        let mut g = self.geo();
        g.rect(a.0.min(b.0), a.1.min(b.1), a.0.max(b.0), a.1.max(b.1));
        let Some(path) = g.finish() else { return };
        let end = if horizontal { (b.0, a.1) } else { (a.0, b.1) };
        let (p0, p1) = (self.to_canvas(a), self.to_canvas(end));
        let color = |c: Rgb| tiny_skia::Color::from_rgba8(c[0], c[1], c[2], 255);
        let stops = vec![tiny_skia::GradientStop::new(0.0, color(ca)), tiny_skia::GradientStop::new(1.0, color(cb))];
        let shader = tiny_skia::LinearGradient::new(
            tiny_skia::Point::from_xy(clamp32(p0.0), clamp32(p0.1)),
            tiny_skia::Point::from_xy(clamp32(p1.0), clamp32(p1.1)),
            stops,
            tiny_skia::SpreadMode::Pad,
            Transform::identity(),
        );
        if let Some(shader) = shader {
            let clip = self.dc.clip.clone();
            let clip = if self.cv.measure { None } else { clip.as_deref() };
            self.cv.fill(&path, &Source::Shader(shader), FillRule::Winding, BlendMode::SourceOver, clip);
        }
    }

    /// A triangle with a colour at each corner, interpolated across it.
    pub fn gradient_triangle(&mut self, v: [((f64, f64), Rgb); 3]) {
        let p = v.map(|(pos, _)| self.to_canvas(pos));
        let (min_x, max_x) =
            (p.iter().map(|q| q.0).fold(f64::MAX, f64::min), p.iter().map(|q| q.0).fold(f64::MIN, f64::max));
        let (min_y, max_y) =
            (p.iter().map(|q| q.1).fold(f64::MAX, f64::min), p.iter().map(|q| q.1).fold(f64::MIN, f64::max));
        if ![min_x, max_x, min_y, max_y].iter().all(|v| v.is_finite()) {
            return;
        }
        let det = (p[1].1 - p[2].1) * (p[0].0 - p[2].0) + (p[2].0 - p[1].0) * (p[0].1 - p[2].1);
        if det.abs() < 1e-9 {
            return;
        }
        let rect = [
            min_x.floor().max(-1.0e6) as i32,
            min_y.floor().max(-1.0e6) as i32,
            max_x.ceil().min(1.0e6) as i32,
            max_y.ceil().min(1.0e6) as i32,
        ];
        let clip = self.dc.clip.clone();
        let clip = if self.cv.measure { None } else { clip.as_deref() };
        self.cv.rop_rect(rect, clip, |x, y, _| {
            let (px, py) = (x as f64 + 0.5, y as f64 + 0.5);
            let l0 = ((p[1].1 - p[2].1) * (px - p[2].0) + (p[2].0 - p[1].0) * (py - p[2].1)) / det;
            let l1 = ((p[2].1 - p[0].1) * (px - p[2].0) + (p[0].0 - p[2].0) * (py - p[2].1)) / det;
            let l2 = 1.0 - l0 - l1;
            let eps = -0.02;
            if l0 < eps || l1 < eps || l2 < eps {
                return None;
            }
            let mix = |k: usize| {
                (v[0].1[k] as f64 * l0 + v[1].1[k] as f64 * l1 + v[2].1[k] as f64 * l2).clamp(0.0, 255.0) as u8
            };
            Some([mix(0), mix(1), mix(2)])
        });
    }

    //endregion
}

fn clamp32(v: f64) -> f32 {
    v.clamp(-1.0e7, 1.0e7) as f32
}

/// True for raster operations that read the pattern (the brush): those
/// whose truth table differs when only the pattern bit changes.
fn uses_pattern(rop: u32) -> bool {
    let t = (rop >> 16) as u8;
    (0..8u8).any(|i| (t >> i) & 1 != (t >> (i ^ 4)) & 1)
}

fn stock_font(n: u32) -> Font {
    let fixed = matches!(n, 10 | 11 | 16);
    Font { height: -13.0, weight: 400, face: if fixed { "Courier".into() } else { "Arial".into() }, ..Font::default() }
}

/// A sub-rectangle of a bitmap.
fn crop(b: &Bitmap, x: i64, y: i64, w: u32, h: u32) -> Bitmap {
    if x == 0 && y == 0 && w == b.width && h == b.height {
        return b.clone();
    }
    let mut rgba = Vec::with_capacity(w as usize * h as usize * 4);
    for row in 0..h as i64 {
        let start = (((y + row) as usize) * b.width as usize + x as usize) * 4;
        rgba.extend_from_slice(&b.rgba[start..start + w as usize * 4]);
    }
    Bitmap { width: w, height: h, rgba, mono: b.mono, has_alpha: b.has_alpha }
}

/// Averages `k` by `k` blocks of pixels into one.
fn shrink(b: &Bitmap, k: u32) -> Bitmap {
    let (w, h) = (b.width.div_ceil(k).max(1), b.height.div_ceil(k).max(1));
    let mut rgba = vec![0u8; w as usize * h as usize * 4];
    for y in 0..h {
        for x in 0..w {
            let mut sum = [0u32; 4];
            let mut n = 0u32;
            for yy in (y * k)..((y + 1) * k).min(b.height) {
                for xx in (x * k)..((x + 1) * k).min(b.width) {
                    let i = (yy as usize * b.width as usize + xx as usize) * 4;
                    let a = b.rgba[i + 3] as u32;
                    sum[0] += b.rgba[i] as u32 * a;
                    sum[1] += b.rgba[i + 1] as u32 * a;
                    sum[2] += b.rgba[i + 2] as u32 * a;
                    sum[3] += a;
                    n += 1;
                }
            }
            let o = (y as usize * w as usize + x as usize) * 4;
            if sum[3] > 0 {
                rgba[o] = (sum[0] / sum[3]) as u8;
                rgba[o + 1] = (sum[1] / sum[3]) as u8;
                rgba[o + 2] = (sum[2] / sum[3]) as u8;
                rgba[o + 3] = (sum[3] / n.max(1)) as u8;
            }
        }
    }
    Bitmap { width: w, height: h, rgba, mono: b.mono, has_alpha: b.has_alpha }
}

impl Gdi {
    /// Builds a pattern brush's pixmap tile from a bitmap, if it is small
    /// enough and the budget allows.
    pub fn pattern_pixmap(&mut self, bmp: &Bitmap) -> Option<Pixmap> {
        let pixels = bmp.width as u64 * bmp.height as u64;
        if pixels > MAX_TILE_PIXELS || !self.reserve(pixels as usize * 4) {
            return None;
        }
        let mut b = bmp.clone();
        b.make_opaque();
        b.to_pixmap()
    }
}
