//! EMF+, the GDI+ records that Office, Visio and .NET programs tuck into
//! `EMR_COMMENT` records of an Enhanced Metafile: anti-aliased paths with
//! alpha, gradient and texture brushes, dashed pens, images, strings.
//!
//! Most files carry both forms ("EMF+ dual"): the GDI records are a plain
//! fallback for old readers.  Dual files are played from their GDI
//! records, which are complete and need no object model.  Files marked "EMF+
//! only" have nothing useful in their GDI records, so these are played
//! here, together with the GDI records that follow an `EmfPlusGetDC` record.

use crate::Rgb;
use crate::affine::Affine;
use crate::bytes::Rec;
use crate::canvas::Source;
use crate::dib::Bitmap;
use crate::gdi::Gdi;
use crate::shapes::Geo;
use std::f64::consts::TAU;
use std::rc::Rc;
use tiny_skia::{
    BlendMode, FillRule, GradientStop, LineCap, LineJoin, LinearGradient, Mask, Path, PathBuilder, Pixmap, Point,
    RadialGradient, SpreadMode, Stroke, StrokeDash, Transform,
};

/// Nested metafile images (an EMF+ image that is itself a metafile, which
/// may hold images) are drawn to this depth and no further.
const MAX_DEPTH: u32 = 3;

/// An ARGB colour as GDI+ stores it (bytes B, G, R, A).
#[derive(Clone, Copy, Debug)]
pub struct Argb(pub Rgb, pub u8);

fn argb(v: u32) -> Argb {
    Argb([(v >> 16) as u8, (v >> 8) as u8, v as u8], (v >> 24) as u8)
}

#[derive(Clone)]
enum Brush {
    Solid(Argb),
    Hatch { style: u32, fore: Argb, back: Argb },
    Linear { rect: [f32; 4], start: Argb, end: Argb, matrix: Affine, wrap: u32, stops: Vec<(f32, Argb)> },
    Radial { centre: (f32, f32), centre_colour: Argb, surround: Argb, matrix: Affine, bounds: [f32; 4] },
    Texture { image: Rc<Bitmap>, matrix: Affine, wrap: u32 },
}

#[derive(Clone)]
struct Pen {
    /// True when the pen asks for line caps (anchors, custom shapes) that
    /// aren't drawn: callers then prefer a file's GDI records.
    fancy_caps: bool,
    width: f32,
    brush: Brush,
    start_cap: u32,
    end_cap: u32,
    join: u32,
    miter: f32,
    dash: Vec<f32>,
    dash_offset: f32,
    transform: Affine,
}

enum Image {
    Bitmap(Rc<Bitmap>),
    Metafile(Rc<Vec<u8>>),
}

#[derive(Clone)]
struct Font {
    size: f32,
    unit: u32,
    style: u32,
    family: String,
}

#[derive(Clone)]
enum Region {
    Rect([f32; 4]),
    Path(Rc<PlusPath>),
    Empty,
    Infinite,
    /// A combining operation's operands.  GDI+ has several (intersect,
    /// exclude, xor); outlines are all added together here.
    Combine(Box<Region>, Box<Region>),
}

/// A path as GDI+ stores it: points and one type byte each.
#[derive(Clone)]
struct PlusPath {
    points: Vec<(f64, f64)>,
    types: Vec<u8>,
}

enum Obj {
    Brush(Brush),
    Pen(Pen),
    Path(Rc<PlusPath>),
    Region(Rc<Region>),
    Image(Image),
    Font(Font),
    /// A string format: alignment across and down the layout rectangle.
    Format {
        align: u32,
        line_align: u32,
    },
    /// Something not understood: the name says what, for the warnings.
    Unsupported(&'static str),
    /// Something that doesn't change the drawing (image attributes).
    Other,
}

#[derive(Clone)]
struct GState {
    world: Affine,
    /// Device pixels per page unit, which includes the page scale.
    page_scale: f64,
    /// Device pixels per page unit before the page scale: what a font size
    /// in pixels is measured against.
    page_unit_px: f64,
    clip: Option<Rc<Mask>>,
}

/// A chunked object: its type and id, the bytes so far and the total size.
struct Pending {
    id: usize,
    ty: u32,
    data: Vec<u8>,
    total: usize,
}

pub struct Plus {
    objects: Vec<Option<Obj>>,
    state: GState,
    saved: Vec<GState>,
    containers: Vec<(u32, GState)>,
    pub dual: bool,
    /// True while GDI records are to be played (after `GetDC`).
    pub get_dc: bool,
    dpi: (f64, f64),
    pending: Option<Pending>,
    /// While GDI records run inside a `GetDC` section: the canvas mapping
    /// they started from, which the GDI+ transform was composed onto.
    gdi_base: Option<Affine>,
}

impl Plus {
    pub fn new() -> Plus {
        Plus {
            objects: (0..64).map(|_| None).collect(),
            state: GState { world: Affine::IDENTITY, page_scale: 1.0, page_unit_px: 1.0, clip: None },
            saved: Vec::new(),
            containers: Vec::new(),
            dual: true,
            get_dc: false,
            dpi: (96.0, 96.0),
            pending: None,
            gdi_base: None,
        }
    }
}

/// Is this comment's payload an EMF+ record stream?  (The comment data is
/// a length and then `EMF+`.)
pub fn is_plus(comment: &Rec) -> bool {
    comment.bytes(12, 4) == b"EMF+"
}

/// What one EMF+ comment says about the file: the dual flag of a header
/// record in it, and whether its last record is `GetDC` (after which GDI
/// records are meant to be played).
pub fn comment_info(comment: &Rec) -> (Option<bool>, bool) {
    let data = comment.tail(16);
    let total = (comment.u32(8) as usize).saturating_sub(4).min(data.len());
    let (mut dual, mut last_get_dc) = (None, false);
    let mut at = 0usize;
    while at + 12 <= total {
        let r = Rec::new(0, &data[at..total]);
        let (ty, flags, size) = (r.u16(0), r.u16(2), r.u32(4) as usize);
        if ty == 0x4001 {
            dual = Some(flags & 1 != 0);
        }
        last_get_dc = ty == 0x4004;
        if size < 12 {
            break;
        }
        at += size;
    }
    (dual, last_get_dc)
}

/// Plays the EMF+ records of one comment.  `emf_depth` limits nesting.
pub fn play_comment(g: &mut Gdi, p: &mut Plus, comment: &Rec, depth: u32) {
    let data = comment.tail(16);
    let mut at = 0usize;
    // The comment's records fill its data size; each is type, flags, size
    // (the whole record), data size.
    let total = (comment.u32(8) as usize).saturating_sub(4).min(data.len());
    p.get_dc = false;
    // The GDI records of a `GetDC` section end where EMF+ records resume.
    if let Some(base) = p.gdi_base.take() {
        g.base = base;
        g.update_transform();
    }
    while at + 12 <= total {
        if g.out_of_time() {
            return;
        }
        let r = Rec::new(0, &data[at..total]);
        let (ty, flags, size, dsize) = (r.u16(0), r.u16(2), r.u32(4) as usize, r.u32(8) as usize);
        if size < 12 || at + size > total + 3 {
            break;
        }
        let body = r.bytes(12, dsize.min(size - 12));
        record(g, p, ty, flags, body, depth);
        at += size;
    }
}

fn record(g: &mut Gdi, p: &mut Plus, ty: u16, flags: u16, body: &[u8], depth: u32) {
    if crate::trace() {
        eprintln!("  EMF+ 0x{ty:04X} flags 0x{flags:04X} {} bytes", body.len());
    }
    let d = Rec::new(ty, body);
    match ty {
        0x4001 => {
            // Header: version, flags, logical dpi.
            p.dpi = (d.u32(8) as f64, d.u32(12) as f64);
            if !(p.dpi.0 > 1.0 && p.dpi.0 < 10_000.0 && p.dpi.1 > 1.0 && p.dpi.1 < 10_000.0) {
                p.dpi = (96.0, 96.0);
            }
            p.dual = flags & 1 != 0;
        }
        0x4002 | 0x4003 => {}
        0x4004 => {
            // GDI records that follow draw through the GDI+ transform.
            p.get_dc = true;
            if p.gdi_base.is_none() {
                let m = p.state.world.then(Affine::scale(p.state.page_scale, p.state.page_scale));
                p.gdi_base = Some(g.base);
                g.base = m.then(g.base);
                g.update_transform();
            }
        }
        0x4008 => object(g, p, flags, body),
        0x4009 => clear(g, d.u32(0)),
        0x400A => {
            let (brush, off) = brush_arg(p, flags, &d, 0);
            let n = d.u32(off) as usize;
            let rects = rects(&d, off + 4, n, flags & 0x4000 != 0);
            let mut geo = geo(g, p);
            for r in &rects {
                geo.rect(r[0], r[1], r[0] + r[2], r[1] + r[3]);
            }
            fill(g, p, geo.finish(), brush, FillRule::Winding);
        }
        0x400B => {
            let n = d.u32(0) as usize;
            let rs = rects(&d, 4, n, flags & 0x4000 != 0);
            let mut geo = geo(g, p);
            for r in &rs {
                geo.rect(r[0], r[1], r[0] + r[2], r[1] + r[3]);
            }
            stroke(g, p, geo.finish(), pen_of(p, flags));
        }
        0x400C => {
            let (brush, off) = brush_arg(p, flags, &d, 0);
            let n = d.u32(off) as usize;
            let pts = points(&d, off + 4, n, flags & 0x4000 != 0);
            let mut geo = geo(g, p);
            geo.polygon(&pts);
            fill(g, p, geo.finish(), brush, FillRule::EvenOdd);
        }
        0x400D => {
            let n = d.u32(0) as usize;
            let pts = points(&d, 4, n, flags & 0x4000 != 0);
            let mut geo = geo(g, p);
            geo.polyline(&pts);
            // Bit 0x200 closes the figure.
            if flags & 0x200 != 0 {
                geo.close();
            }
            stroke(g, p, geo.finish(), pen_of(p, flags));
        }
        0x400E | 0x400F => {
            let (brush, off) = if ty == 0x400E { brush_arg(p, flags, &d, 0) } else { (None, 0) };
            let r = rect(&d, off, flags & 0x4000 != 0);
            let mut geo = geo(g, p);
            geo.ellipse(r[0], r[1], r[0] + r[2], r[1] + r[3]);
            if ty == 0x400E {
                fill(g, p, geo.finish(), brush, FillRule::Winding);
            } else {
                stroke(g, p, geo.finish(), pen_of(p, flags));
            }
        }
        0x4010..=0x4012 => {
            let (brush, off) = if ty == 0x4010 { brush_arg(p, flags, &d, 0) } else { (None, 0) };
            let (start, sweep) = (d.f32(off) as f64, d.f32(off + 4) as f64);
            let r = rect(&d, off + 8, flags & 0x4000 != 0);
            let c = (r[0] + r[2] / 2.0, r[1] + r[3] / 2.0);
            let radii = (r[2] / 2.0, r[3] / 2.0);
            let (t0, dt) = (arc_param(start, radii), sweep.to_radians().clamp(-TAU, TAU));
            let mut geo = geo(g, p);
            if ty == 0x4012 {
                geo.arc(c, radii, t0, dt, false);
                stroke(g, p, geo.finish(), pen_of(p, flags));
            } else {
                geo.move_to(c.0, c.1);
                geo.arc(c, radii, t0, dt, true);
                geo.close();
                if ty == 0x4010 {
                    fill(g, p, geo.finish(), brush, FillRule::Winding);
                } else {
                    stroke(g, p, geo.finish(), pen_of(p, flags));
                }
            }
        }
        0x4013 => {
            let (brush, _) = brush_arg(p, flags, &d, 0);
            let region = match p.objects.get((flags & 0xFF) as usize) {
                Some(Some(Obj::Region(r))) => Some(r.clone()),
                _ => None,
            };
            if let Some(path) = region.and_then(|r| region_path(g, p, &r)) {
                fill(g, p, Some(path), brush, FillRule::Winding);
            }
        }
        0x4014 => {
            let (brush, _) = brush_arg(p, flags, &d, 0);
            if let Some(path) = path_obj(p, flags) {
                let rule = if path_winding(&path) { FillRule::Winding } else { FillRule::EvenOdd };
                let built = build_path(g, p, &path);
                fill(g, p, built, brush, rule);
            }
        }
        0x4015 => {
            let pen = match p.objects.get(d.u32(0) as usize) {
                Some(Some(Obj::Pen(pen))) => Some(pen.clone()),
                _ => None,
            };
            if let Some(path) = path_obj(p, flags) {
                let built = build_path(g, p, &path);
                stroke(g, p, built, pen);
            }
        }
        0x4037 => {
            // StrokeFillPath: not seen in files, played as a fill.
            g.warnings.skip("EMF+ StrokeFillPath");
        }
        0x4016 | 0x4017 => {
            let fill_it = ty == 0x4016;
            let (brush, off) = if fill_it { brush_arg(p, flags, &d, 0) } else { (None, 0) };
            let tension = d.f32(off) as f64;
            let n = d.u32(off + 4) as usize;
            let pts = points(&d, off + 8, n, flags & 0x4000 != 0);
            let mut geo = geo(g, p);
            cardinal(&mut geo, &pts, tension, true);
            if fill_it {
                let rule = if flags & 0x2000 != 0 { FillRule::Winding } else { FillRule::EvenOdd };
                fill(g, p, geo.finish(), brush, rule);
            } else {
                stroke(g, p, geo.finish(), pen_of(p, flags));
            }
        }
        0x4018 => {
            let tension = d.f32(0) as f64;
            let (offset, segs, n) = (d.u32(4) as usize, d.u32(8) as usize, d.u32(12) as usize);
            let pts = points(&d, 16, n, flags & 0x4000 != 0);
            let pts: Vec<(f64, f64)> = pts.into_iter().skip(offset).take(segs + 1).collect();
            let mut geo = geo(g, p);
            cardinal(&mut geo, &pts, tension, false);
            stroke(g, p, geo.finish(), pen_of(p, flags));
        }
        0x4019 => {
            let n = d.u32(0) as usize;
            let pts = points(&d, 4, n, flags & 0x4000 != 0);
            let mut geo = geo(g, p);
            geo.bezier(&pts, false);
            stroke(g, p, geo.finish(), pen_of(p, flags));
        }
        0x401A => {
            let compressed = flags & 0x4000 != 0;
            let src = [d.f32(8), d.f32(12), d.f32(16), d.f32(20)];
            let dst = rect(&d, 24, compressed);
            let parallelogram = [(dst[0], dst[1]), (dst[0] + dst[2], dst[1]), (dst[0], dst[1] + dst[3])];
            draw_image(g, p, (flags & 0xFF) as usize, src, parallelogram, depth);
        }
        0x401B => {
            let compressed = flags & 0x4000 != 0;
            let src = [d.f32(8), d.f32(12), d.f32(16), d.f32(20)];
            let n = d.u32(24) as usize;
            let pts = points(&d, 28, n.min(3), compressed);
            if pts.len() == 3 {
                draw_image(g, p, (flags & 0xFF) as usize, src, [pts[0], pts[1], pts[2]], depth);
            }
        }
        0x401C => draw_string(g, p, flags, &d),
        0x4036 => draw_driver_string(g, p, flags, &d),
        0x4025 => p.saved.push(p.state_with_clip(g)),
        0x4026 => {
            if let Some(s) = p.saved.pop() {
                p.restore(g, s);
            }
        }
        0x4027 | 0x4028 => {
            let id = d.u32(if ty == 0x4027 { 32 } else { 0 });
            let s = p.state_with_clip(g);
            p.containers.push((id, s));
        }
        0x4029 => {
            let id = d.u32(0);
            while let Some((cid, s)) = p.containers.pop() {
                if cid == id {
                    p.restore(g, s);
                    break;
                }
            }
        }
        0x402A => p.state.world = matrix(&d, 0),
        0x402B => p.state.world = Affine::IDENTITY,
        0x402C => {
            let m = matrix(&d, 0);
            p.state.world = if flags & 0x2000 != 0 { p.state.world.then(m) } else { m.then(p.state.world) };
        }
        0x402D..=0x402F => {
            let m = match ty {
                0x402D => Affine::translate(d.f32(0) as f64, d.f32(4) as f64),
                0x402E => Affine::scale(d.f32(0) as f64, d.f32(4) as f64),
                _ => Affine::rotate((d.f32(0) as f64).to_radians()),
            };
            p.state.world = if flags & 0x2000 != 0 { p.state.world.then(m) } else { m.then(p.state.world) };
        }
        0x4030 => {
            let unit = (flags & 0xFF) as u32;
            let scale = d.f32(0) as f64;
            let per = match unit {
                3 => p.dpi.0 / 72.0,
                4 => p.dpi.0,
                5 => p.dpi.0 / 300.0,
                6 => p.dpi.0 / 25.4,
                _ => 1.0,
            };
            p.state.page_scale = if scale > 0.0 && scale.is_finite() { per * scale } else { 1.0 };
            p.state.page_unit_px = per;
        }
        0x4031 => g.reset_clip(),
        0x4032 => {
            let r = rect(&d, 0, flags & 0x4000 != 0);
            let mut geo = geo(g, p);
            geo.rect(r[0], r[1], r[0] + r[2], r[1] + r[3]);
            clip_with(g, geo.finish(), FillRule::Winding, ((flags >> 8) & 0xF) as u32);
        }
        0x4033 => {
            if let Some(path) = path_obj(p, flags) {
                let rule = if path_winding(&path) { FillRule::Winding } else { FillRule::EvenOdd };
                let built = build_path(g, p, &path);
                clip_with(g, built, rule, ((flags >> 8) & 0xF) as u32);
            }
        }
        0x4034 => {
            let region = match p.objects.get((flags & 0xFF) as usize) {
                Some(Some(Obj::Region(r))) => Some(r.clone()),
                _ => None,
            };
            if let Some(r) = region {
                let path = region_path(g, p, &r);
                clip_with(g, path, FillRule::Winding, ((flags >> 8) & 0xF) as u32);
            }
        }
        0x4035 => {
            let m = p.canvas_matrix(g);
            let (dx, dy) = m.apply_vector((d.f32(0) as f64, d.f32(4) as f64));
            let _ = (dx, dy);
            g.offset_clip_canvas(dx, dy);
        }
        // Rendering hints and the like change nothing we draw differently.
        0x401D..=0x4024 | 0x4038..=0x403A => {}
        other => g.warnings.skip(format!("EMF+ record 0x{other:04X}")),
    }
}

impl Plus {
    fn state_with_clip(&self, g: &Gdi) -> GState {
        GState { clip: g.dc.clip.clone(), ..self.state.clone() }
    }

    fn restore(&mut self, g: &mut Gdi, s: GState) {
        g.dc.clip = s.clip.clone();
        self.state = s;
    }

    /// Page units to canvas pixels.
    fn canvas_matrix(&self, g: &Gdi) -> Affine {
        self.state.world.then(Affine::scale(self.state.page_scale, self.state.page_scale)).then(g.base)
    }
}

/*┌─────────────────────╮
| Reading values        |
└─────────────────────╯*/
//region Reading values

fn matrix(d: &Rec, o: usize) -> Affine {
    Affine::new(
        d.f32(o) as f64,
        d.f32(o + 4) as f64,
        d.f32(o + 8) as f64,
        d.f32(o + 12) as f64,
        d.f32(o + 16) as f64,
        d.f32(o + 20) as f64,
    )
}

/// `n` points as floats or, when compressed, as 16-bit integers.
fn points(d: &Rec, at: usize, n: usize, compressed: bool) -> Vec<(f64, f64)> {
    let size = if compressed { 4 } else { 8 };
    let n = n.min(d.len().saturating_sub(at) / size).min(1 << 20);
    (0..n)
        .map(|i| {
            if compressed {
                (d.i16(at + i * 4) as f64, d.i16(at + i * 4 + 2) as f64)
            } else {
                (d.f32(at + i * 8) as f64, d.f32(at + i * 8 + 4) as f64)
            }
        })
        .collect()
}

fn rect(d: &Rec, at: usize, compressed: bool) -> [f64; 4] {
    if compressed {
        [d.i16(at) as f64, d.i16(at + 2) as f64, d.i16(at + 4) as f64, d.i16(at + 6) as f64]
    } else {
        [d.f32(at) as f64, d.f32(at + 4) as f64, d.f32(at + 8) as f64, d.f32(at + 12) as f64]
    }
}

fn rects(d: &Rec, at: usize, n: usize, compressed: bool) -> Vec<[f64; 4]> {
    let size = if compressed { 8 } else { 16 };
    let n = n.min(d.len().saturating_sub(at) / size).min(1 << 18);
    (0..n).map(|i| rect(d, at + i * size, compressed)).collect()
}

/// The brush a fill record names: a colour in the record when flag `S`
/// (0x8000) is set, otherwise an object id.  Returns the brush and the
/// offset after it.
fn brush_arg(p: &Plus, flags: u16, d: &Rec, at: usize) -> (Option<Brush>, usize) {
    let v = d.u32(at);
    if flags & 0x8000 != 0 {
        return (Some(Brush::Solid(argb(v))), at + 4);
    }
    match p.objects.get(v as usize) {
        Some(Some(Obj::Brush(b))) => (Some(b.clone()), at + 4),
        _ => (None, at + 4),
    }
}

fn pen_of(p: &Plus, flags: u16) -> Option<Pen> {
    match p.objects.get((flags & 0xFF) as usize) {
        Some(Some(Obj::Pen(pen))) => Some(pen.clone()),
        _ => None,
    }
}

fn path_obj(p: &Plus, flags: u16) -> Option<Rc<PlusPath>> {
    match p.objects.get((flags & 0xFF) as usize) {
        Some(Some(Obj::Path(path))) => Some(path.clone()),
        _ => None,
    }
}

/// A sweep given by a start angle in degrees on an ellipse: GDI+ measures
/// angles clockwise from the x axis on the square the ellipse fits, which
/// is the parametric angle.
fn arc_param(start_deg: f64, _radii: (f64, f64)) -> f64 {
    start_deg.to_radians()
}

//endregion

/*┌─────────────────────╮
| Objects               |
└─────────────────────╯*/
//region Objects

fn object(g: &mut Gdi, p: &mut Plus, flags: u16, body: &[u8]) {
    let id = (flags & 0xFF) as usize;
    let ty = ((flags >> 8) & 0x7F) as u32;
    let continued = flags & 0x8000 != 0;
    if id >= p.objects.len() {
        return;
    }
    // Large objects (images) arrive in chunks: the first record starts with
    // the total size, the rest continue the data.
    if continued {
        let total = Rec::new(0, body).u32(0) as usize;
        if total > 256 << 20 {
            return;
        }
        let chunk = body.get(4..).unwrap_or_default().to_vec();
        match &mut p.pending {
            Some(pn) if pn.id == id && pn.ty == ty => {
                pn.data.extend_from_slice(&chunk);
                if pn.data.len() >= pn.total {
                    let done = p.pending.take().expect("pending set");
                    let obj = parse_object(done.ty, &done.data);
                    store(g, p, id, obj);
                }
            }
            _ => {
                let mut pn = Pending { id, ty, data: Vec::new(), total };
                pn.data.extend_from_slice(&chunk);
                if pn.data.len() >= total {
                    let obj = parse_object(ty, &pn.data);
                    store(g, p, id, obj);
                } else {
                    p.pending = Some(pn);
                }
            }
        }
        return;
    }
    let obj = parse_object(ty, body);
    store(g, p, id, obj);
}

/// Puts an object in the table, noting objects that weren't understood.
fn store(g: &mut Gdi, p: &mut Plus, id: usize, obj: Obj) {
    // Images stay alive in the table: charge their pixels to the budget.
    let bytes = match &obj {
        Obj::Image(Image::Bitmap(b)) => b.rgba.len(),
        Obj::Brush(Brush::Texture { image, .. }) => image.rgba.len(),
        Obj::Pen(Pen { brush: Brush::Texture { image, .. }, .. }) => image.rgba.len(),
        _ => 0,
    };
    let obj = if bytes > 0 && !g.reserve(bytes) { Obj::Unsupported("image") } else { obj };
    if let Obj::Unsupported(what) = &obj {
        g.warnings.skip(format!("EMF+ {what} object"));
    }
    p.objects[id] = Some(obj);
}

fn parse_object(ty: u32, data: &[u8]) -> Obj {
    let d = Rec::new(0, data);
    match ty {
        1 => parse_brush(&d, 0).map_or(Obj::Unsupported("brush"), |(b, _)| Obj::Brush(b)),
        2 => parse_pen(&d).map_or(Obj::Unsupported("pen"), Obj::Pen),
        3 => parse_path(&d).map_or(Obj::Unsupported("path"), |p| Obj::Path(Rc::new(p))),
        4 => parse_region(&d, &mut 8, 0).map_or(Obj::Unsupported("region"), |r| Obj::Region(Rc::new(r))),
        5 => parse_image(&d).map_or(Obj::Unsupported("image"), Obj::Image),
        7 => Obj::Format { align: d.u32(12), line_align: d.u32(16) },
        6 => {
            let n = (d.u32(20) as usize).min(64);
            let units: Vec<u16> = (0..n).map(|i| d.u16(24 + i * 2)).take_while(|u| *u != 0).collect();
            Obj::Font(Font {
                size: d.f32(4),
                unit: d.u32(8),
                style: d.u32(12),
                family: String::from_utf16_lossy(&units),
            })
        }
        _ => Obj::Other,
    }
}

fn parse_brush(d: &Rec, at: usize) -> Option<(Brush, usize)> {
    let ty = d.u32(at + 4);
    let o = at + 8;
    Some(match ty {
        0 => (Brush::Solid(argb(d.u32(o))), o + 4),
        1 => (Brush::Hatch { style: d.u32(o), fore: argb(d.u32(o + 4)), back: argb(d.u32(o + 8)) }, o + 12),
        2 => {
            let flags = d.u32(o);
            let wrap = d.u32(o + 4);
            let mut at = o + 8;
            let mut m = Affine::IDENTITY;
            if flags & 2 != 0 {
                m = matrix(d, at);
                at += 24;
            }
            let Image::Bitmap(img) = parse_image(&Rec::new(0, d.tail(at)))? else { return None };
            (Brush::Texture { image: img, matrix: m, wrap }, d.len())
        }
        3 => {
            let flags = d.u32(o);
            let wrap = d.u32(o + 4);
            let centre_colour = argb(d.u32(o + 8));
            let centre = (d.f32(o + 12), d.f32(o + 16));
            let count = (d.u32(o + 20) as usize).min(256);
            let surround = if count > 0 { argb(d.u32(o + 24)) } else { centre_colour };
            let mut at = o + 24 + count * 4;
            let _ = wrap;
            // The boundary: a path or a point list, whose box sets the extent.
            let mut bounds = [centre.0 - 50.0, centre.1 - 50.0, centre.0 + 50.0, centre.1 + 50.0];
            if flags & 1 != 0 {
                let len = d.u32(at) as usize;
                if let Some(b) = parse_path(&Rec::new(0, d.bytes(at + 4, len))).and_then(|path| bbox(&path.points)) {
                    bounds = b;
                }
                at += 4 + len;
            } else {
                let n = (d.u32(at) as usize).min(4096);
                let pts = points(d, at + 4, n, false);
                if let Some(b) = bbox(&pts) {
                    bounds = b;
                }
                at += 4 + n * 8;
            }
            let mut m = Affine::IDENTITY;
            if flags & 2 != 0 {
                m = matrix(d, at);
            }
            (Brush::Radial { centre, centre_colour, surround, matrix: m, bounds }, d.len())
        }
        4 => {
            let flags = d.u32(o);
            let wrap = d.u32(o + 4);
            let rect = [d.f32(o + 8), d.f32(o + 12), d.f32(o + 16), d.f32(o + 20)];
            let (start, end) = (argb(d.u32(o + 24)), argb(d.u32(o + 28)));
            let mut at = o + 40;
            let mut m = Affine::IDENTITY;
            if flags & 2 != 0 {
                m = matrix(d, at);
                at += 24;
            }
            let mut stops = Vec::new();
            if flags & 4 != 0 {
                // Preset colours: positions and colours.
                let n = (d.u32(at) as usize).min(256);
                for i in 0..n {
                    stops.push((d.f32(at + 4 + i * 4), argb(d.u32(at + 4 + n * 4 + i * 4))));
                }
            } else if flags & 8 != 0 {
                // Blend factors: positions and the share of the end colour.
                let n = (d.u32(at) as usize).min(256);
                for i in 0..n {
                    let t = d.f32(at + 4 + n * 4 + i * 4).clamp(0.0, 1.0);
                    stops.push((d.f32(at + 4 + i * 4), lerp(start, end, t)));
                }
            }
            (Brush::Linear { rect, start, end, matrix: m, wrap, stops }, d.len())
        }
        _ => return None,
    })
}

fn lerp(a: Argb, b: Argb, t: f32) -> Argb {
    let mix = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round().clamp(0.0, 255.0) as u8;
    Argb([mix(a.0[0], b.0[0]), mix(a.0[1], b.0[1]), mix(a.0[2], b.0[2])], mix(a.1, b.1))
}

fn bbox(pts: &[(f64, f64)]) -> Option<[f32; 4]> {
    let first = pts.first()?;
    let mut b = [first.0 as f32, first.1 as f32, first.0 as f32, first.1 as f32];
    for p in pts {
        b[0] = b[0].min(p.0 as f32);
        b[1] = b[1].min(p.1 as f32);
        b[2] = b[2].max(p.0 as f32);
        b[3] = b[3].max(p.1 as f32);
    }
    Some(b)
}

fn parse_pen(d: &Rec) -> Option<Pen> {
    // Version, type, then PenData: flags, unit, width, optional fields.
    let flags = d.u32(8);
    let width = d.f32(16);
    let mut at = 20;
    let mut pen = Pen {
        fancy_caps: flags & 0x1800 != 0,
        width,
        brush: Brush::Solid(Argb([0, 0, 0], 255)),
        start_cap: 0,
        end_cap: 0,
        join: 0,
        miter: 10.0,
        dash: Vec::new(),
        dash_offset: 0.0,
        transform: Affine::IDENTITY,
    };
    if flags & 0x1 != 0 {
        pen.transform = matrix(d, at);
        at += 24;
    }
    if flags & 0x2 != 0 {
        pen.start_cap = d.u32(at);
        at += 4;
    }
    if flags & 0x4 != 0 {
        pen.end_cap = d.u32(at);
        at += 4;
    }
    // Caps from 0x10 up are anchors (arrows, diamonds) and 0xFF custom ones.
    pen.fancy_caps |= pen.start_cap >= 0x10 || pen.end_cap >= 0x10;
    if flags & 0x8 != 0 {
        pen.join = d.u32(at);
        at += 4;
    }
    if flags & 0x10 != 0 {
        pen.miter = d.f32(at);
        at += 4;
    }
    let mut style = 0;
    if flags & 0x20 != 0 {
        style = d.u32(at);
        at += 4;
    }
    if flags & 0x40 != 0 {
        at += 4;
    }
    if flags & 0x80 != 0 {
        pen.dash_offset = d.f32(at);
        at += 4;
    }
    if flags & 0x100 != 0 {
        let n = (d.u32(at) as usize).min(64);
        pen.dash = (0..n).map(|i| d.f32(at + 4 + i * 4)).collect();
        at += 4 + n * 4;
    } else {
        pen.dash = match style {
            1 => vec![3.0, 1.0],
            2 => vec![1.0, 1.0],
            3 => vec![3.0, 1.0, 1.0, 1.0],
            4 => vec![3.0, 1.0, 1.0, 1.0, 1.0, 1.0],
            _ => Vec::new(),
        };
    }
    if flags & 0x200 != 0 {
        at += 4;
    }
    if flags & 0x400 != 0 {
        let n = d.u32(at) as usize;
        at += 4 + n.min(64) * 4;
    }
    if flags & 0x800 != 0 {
        at += 4 + d.u32(at) as usize;
    }
    if flags & 0x1000 != 0 {
        at += 4 + d.u32(at) as usize;
    }
    // The pen's brush follows as a brush object.
    let (brush, _) = parse_brush(&Rec::new(0, d.tail(at)), 0)?;
    pen.brush = brush;
    Some(pen)
}

fn parse_path(d: &Rec) -> Option<PlusPath> {
    let n = d.u32(4) as usize;
    let flags = d.u32(8);
    if n == 0 || n > 1 << 20 || n * 5 > d.len().saturating_mul(2) + 64 {
        return None;
    }
    let compressed = flags & 0x4000 != 0;
    let relative = flags & 0x800 != 0;
    let psize = if compressed { 4 } else { 8 };
    let mut pts = points(d, 12, n, compressed);
    let types_at = 12 + n * psize;
    if pts.len() != n {
        return None;
    }
    if relative {
        // Each point is an offset from the one before.
        let (mut x, mut y) = (0.0, 0.0);
        for p in &mut pts {
            x += p.0;
            y += p.1;
            *p = (x, y);
        }
    }
    let mut types: Vec<u8> = Vec::with_capacity(n);
    if flags & 0x1000 != 0 {
        // Run-length coded types: a type byte and a count.
        let mut at = types_at;
        while types.len() < n && at + 1 < d.len() + 1 {
            let (t, run) = (d.u8(at), (d.u8(at + 1) & 0x3F) as usize);
            for _ in 0..run {
                if types.len() < n {
                    types.push(t & 0xBF);
                }
            }
            if run == 0 {
                break;
            }
            at += 2;
        }
    } else {
        types.extend((0..n).map(|i| d.u8(types_at + i)));
    }
    types.resize(n, 1);
    Some(PlusPath { points: pts, types })
}

fn path_winding(_p: &PlusPath) -> bool {
    false
}

fn parse_region(d: &Rec, at: &mut usize, depth: u32) -> Option<Region> {
    if depth > 24 || *at + 4 > d.len() {
        return None;
    }
    let ty = d.u32(*at);
    *at += 4;
    Some(match ty {
        1..=5 => {
            let a = parse_region(d, at, depth + 1)?;
            let b = parse_region(d, at, depth + 1)?;
            Region::Combine(Box::new(a), Box::new(b))
        }
        0x1000_0000 => {
            let r = [d.f32(*at), d.f32(*at + 4), d.f32(*at + 8), d.f32(*at + 12)];
            *at += 16;
            Region::Rect(r)
        }
        0x1000_0001 => {
            let len = d.u32(*at) as usize;
            let path = parse_path(&Rec::new(0, d.bytes(*at + 4, len)))?;
            *at += 4 + len;
            Region::Path(Rc::new(path))
        }
        0x1000_0002 => Region::Empty,
        0x1000_0003 => Region::Infinite,
        _ => return None,
    })
}

fn parse_image(d: &Rec) -> Option<Image> {
    let ty = d.u32(4);
    match ty {
        1 => {
            let (w, h, stride, format, kind) = (d.u32(8) as usize, d.u32(12) as usize, d.i32(16), d.u32(20), d.u32(24));
            if kind != 1 && (w == 0 || h == 0 || w.saturating_mul(h) > crate::dib::MAX_PIXELS as usize) {
                return None;
            }
            if kind == 1 {
                // A PNG, JPEG or other image file.
                let tex =
                    slerp_scene::textures::decode(d.tail(28), &slerp_scene::textures::DecodeOptions::full()).ok()?;
                let level = tex.levels.first()?;
                let slerp_scene::textures::Pixels::Rgba8(px) = level.surfaces.first()? else { return None };
                return Some(Image::Bitmap(Rc::new(Bitmap {
                    width: level.width,
                    height: level.height,
                    rgba: px.clone(),
                    mono: false,
                    has_alpha: tex.has_alpha,
                })));
            }
            let bpp = (format >> 8) & 0xFF;
            let stride = stride.unsigned_abs() as usize;
            let data = d.tail(28);
            if stride == 0 || data.len() < stride.saturating_mul(h - 1) + (w * bpp as usize).div_ceil(8) {
                return None;
            }
            let premultiplied = format & 0x8000 != 0 || format == 0xE200B;
            let mut rgba = vec![255u8; w * h * 4];
            let has_alpha_format = format & 0x40000 != 0;
            let mut has_alpha = false;
            for y in 0..h {
                let row = &data[y * stride..];
                for x in 0..w {
                    let o = (y * w + x) * 4;
                    match bpp {
                        32 => {
                            let (b, gg, r, a) = (row[x * 4], row[x * 4 + 1], row[x * 4 + 2], row[x * 4 + 3]);
                            let a = if has_alpha_format { a } else { 255 };
                            has_alpha |= a != 255;
                            let un = |c: u8| {
                                if premultiplied && a > 0 && a < 255 {
                                    ((c as u32 * 255 + a as u32 / 2) / a as u32).min(255) as u8
                                } else {
                                    c
                                }
                            };
                            rgba[o..o + 4].copy_from_slice(&[un(r), un(gg), un(b), a]);
                        }
                        24 => rgba[o..o + 3].copy_from_slice(&[row[x * 3 + 2], row[x * 3 + 1], row[x * 3]]),
                        16 => {
                            let v = u16::from_le_bytes([row[x * 2], row[x * 2 + 1]]);
                            let (r, gg, b) = if format == 0x21005 {
                                (
                                    ((v >> 10) & 31) as u32 * 255 / 31,
                                    ((v >> 5) & 31) as u32 * 255 / 31,
                                    (v & 31) as u32 * 255 / 31,
                                )
                            } else {
                                (
                                    ((v >> 11) & 31) as u32 * 255 / 31,
                                    ((v >> 5) & 63) as u32 * 255 / 63,
                                    (v & 31) as u32 * 255 / 31,
                                )
                            };
                            rgba[o..o + 3].copy_from_slice(&[r as u8, gg as u8, b as u8]);
                        }
                        8 => {
                            let v = row[x];
                            rgba[o..o + 3].copy_from_slice(&[v, v, v]);
                        }
                        _ => return None,
                    }
                }
            }
            Some(Image::Bitmap(Rc::new(Bitmap { width: w as u32, height: h as u32, rgba, mono: false, has_alpha })))
        }
        2 => {
            let size = d.u32(12) as usize;
            Some(Image::Metafile(Rc::new(d.bytes(16, size).to_vec())))
        }
        _ => None,
    }
}

//endregion

/*┌─────────────────────╮
| Drawing               |
└─────────────────────╯*/
//region Drawing

fn geo(g: &Gdi, p: &Plus) -> Geo {
    Geo::new(p.canvas_matrix(g))
}

fn clear(g: &mut Gdi, color: u32) {
    let c = argb(color);
    let (w, h) = (g.cv.width() as f32, g.cv.height() as f32);
    let mut pb = PathBuilder::new();
    if let Some(r) = tiny_skia::Rect::from_ltrb(0.0, 0.0, w, h) {
        pb.push_rect(r);
    }
    let clip = g.dc.clip.clone();
    if let Some(path) = pb.finish() {
        g.cv.fill(
            &path,
            &Source::Solid(c.0, c.1 as f32 / 255.0),
            FillRule::Winding,
            BlendMode::Source,
            clip.as_deref(),
        );
    }
}

/// Builds the path a path object describes, transformed to the canvas.
fn build_path(g: &Gdi, p: &Plus, path: &PlusPath) -> Option<Path> {
    let mut geo = geo(g, p);
    let n = path.points.len();
    let mut i = 0;
    let mut open = false;
    while i < n {
        let kind = path.types[i] & 0x0F;
        let close = path.types[i] & 0x80 != 0;
        match kind {
            0 => {
                geo.move_to(path.points[i].0, path.points[i].1);
                open = true;
                i += 1;
            }
            1 => {
                let pt = path.points[i];
                if !open {
                    geo.move_to(pt.0, pt.1);
                    open = true;
                } else {
                    geo.line_to(pt.0, pt.1);
                }
                i += 1;
            }
            3 => {
                if i + 2 < n {
                    geo.cubic_to(path.points[i], path.points[i + 1], path.points[i + 2]);
                    // The closing flag sits on the last point of the curve.
                    let close_here = path.types[i + 2] & 0x80 != 0;
                    i += 3;
                    if close_here {
                        geo.close();
                        open = false;
                    }
                    continue;
                }
                i = n;
            }
            _ => i += 1,
        }
        if close {
            geo.close();
            open = false;
        }
    }
    geo.finish()
}

/// A cardinal spline through `pts`, as cubic Beziers.
fn cardinal(geo: &mut Geo, pts: &[(f64, f64)], tension: f64, closed: bool) {
    let n = pts.len();
    if n < 2 {
        return;
    }
    let t = tension / 3.0;
    let at = |i: isize| -> (f64, f64) {
        if closed { pts[i.rem_euclid(n as isize) as usize] } else { pts[i.clamp(0, n as isize - 1) as usize] }
    };
    let tangent = |i: isize| -> (f64, f64) {
        let (a, b) = (at(i - 1), at(i + 1));
        ((b.0 - a.0) * t, (b.1 - a.1) * t)
    };
    geo.move_to(pts[0].0, pts[0].1);
    let segments = if closed { n } else { n - 1 };
    for i in 0..segments as isize {
        let (p0, p1) = (at(i), at(i + 1));
        let (t0, t1) = (tangent(i), tangent(i + 1));
        geo.cubic_to((p0.0 + t0.0, p0.1 + t0.1), (p1.0 - t1.0, p1.1 - t1.1), p1);
    }
    if closed {
        geo.close();
    }
}

fn spread(wrap: u32) -> SpreadMode {
    match wrap {
        1 | 3 => SpreadMode::Reflect,
        2 => SpreadMode::Pad,
        _ => SpreadMode::Repeat,
    }
}

fn tiny_color(c: Argb) -> tiny_skia::Color {
    tiny_skia::Color::from_rgba8(c.0[0], c.0[1], c.0[2], c.1)
}

fn fill(g: &mut Gdi, p: &Plus, path: Option<Path>, brush: Option<Brush>, rule: FillRule) {
    let (Some(path), Some(brush)) = (path, brush) else { return };
    paint(g, p, &path, &brush, |g, src, clip| g.cv.fill(&path, src, rule, BlendMode::SourceOver, clip));
}

/// Calls `draw` with the tiny-skia source a brush makes.
fn paint(g: &mut Gdi, p: &Plus, _path: &Path, brush: &Brush, draw: impl FnOnce(&mut Gdi, &Source, Option<&Mask>)) {
    let clip = g.dc.clip.clone();
    let clip = clip.as_deref();
    let canvas = p.canvas_matrix(g);
    match brush {
        Brush::Solid(c) => draw(g, &Source::Solid(c.0, c.1 as f32 / 255.0), clip),
        Brush::Hatch { style, fore, back } => {
            let tile = hatch_tile(*style, *fore, *back);
            let Some(pm) = tile else { return };
            // One tile pixel is one device pixel.
            let xf = Affine::scale(1.0, 1.0).then(g.base).to_skia();
            draw(g, &Source::Tile(pm.as_ref(), xf, false), clip);
        }
        Brush::Linear { rect, start, end, matrix, wrap, stops } => {
            let (x0, x1, y) = (rect[0], rect[0] + rect[2], rect[1] + rect[3] / 2.0);
            let mut gs: Vec<GradientStop> = if stops.is_empty() {
                vec![GradientStop::new(0.0, tiny_color(*start)), GradientStop::new(1.0, tiny_color(*end))]
            } else {
                stops.iter().map(|(pos, c)| GradientStop::new(pos.clamp(0.0, 1.0), tiny_color(*c))).collect()
            };
            if gs.len() == 1 {
                gs.push(gs[0]);
            }
            let xf = matrix.then(canvas).to_skia();
            if let Some(shader) =
                LinearGradient::new(Point::from_xy(x0, y), Point::from_xy(x1, y), gs, spread(*wrap), xf)
            {
                draw(g, &Source::Shader(shader), clip);
            }
        }
        Brush::Radial { centre, centre_colour, surround, matrix, bounds } => {
            let radius = ((bounds[2] - bounds[0]).max(bounds[3] - bounds[1]) / 2.0).max(1.0);
            let stops =
                vec![GradientStop::new(0.0, tiny_color(*centre_colour)), GradientStop::new(1.0, tiny_color(*surround))];
            let xf = matrix.then(canvas).to_skia();
            let c = Point::from_xy(centre.0, centre.1);
            if let Some(shader) = RadialGradient::new(c, c, radius, stops, SpreadMode::Pad, xf) {
                draw(g, &Source::Shader(shader), clip);
            }
        }
        Brush::Texture { image, matrix, wrap } => {
            let Some(pm) = image.to_pixmap() else { return };
            let _ = wrap;
            let xf = matrix.then(canvas).to_skia();
            draw(g, &Source::Tile(pm.as_ref(), xf, true), clip);
        }
    }
}

fn hatch_tile(style: u32, fore: Argb, back: Argb) -> Option<Pixmap> {
    let mut pm = Pixmap::new(8, 8)?;
    // The six line hatches by their numbers; the rest as dot densities.
    let on: Vec<bool> = match style {
        0..=5 => crate::state::hatch_tile(style).on,
        _ => {
            let density = match style {
                6 => 1, // 5 percent
                7 => 2,
                8 => 3,
                9 => 4,
                10 => 5,
                11 => 6,
                12 => 7,
                _ => 16,
            };
            (0..64).map(|i| (i % 8 + i / 8 * 3) % 16 < density).collect()
        }
    };
    for (i, px) in pm.data_mut().chunks_exact_mut(4).enumerate() {
        let c = if on[i] { fore } else { back };
        let a = c.1 as u32;
        px.copy_from_slice(&[
            (c.0[0] as u32 * a / 255) as u8,
            (c.0[1] as u32 * a / 255) as u8,
            (c.0[2] as u32 * a / 255) as u8,
            c.1,
        ]);
    }
    Some(pm)
}

fn stroke(g: &mut Gdi, p: &Plus, path: Option<Path>, pen: Option<Pen>) {
    let (Some(path), Some(pen)) = (path, pen) else { return };
    if pen.fancy_caps {
        g.warnings.skip("EMF+ anchor and custom line caps");
    }
    let scale = p.canvas_matrix(g).scale_factor();
    let width = (pen.width as f64 * scale * pen.transform.scale_factor().max(1e-6)).clamp(1.0, 16384.0);
    let cap = match pen.start_cap {
        1 => LineCap::Square,
        2 | 3 => LineCap::Round,
        _ => LineCap::Butt,
    };
    let join = match pen.join {
        1 => LineJoin::Bevel,
        2 => LineJoin::Round,
        _ => LineJoin::Miter,
    };
    let mut stroke = Stroke {
        width: width as f32,
        line_cap: cap,
        line_join: join,
        miter_limit: pen.miter.max(1.0),
        ..Stroke::default()
    };
    if !pen.dash.is_empty() {
        let unit = pen.width.max(0.001) as f64 * scale;
        let mut lens: Vec<f32> = pen.dash.iter().map(|v| (*v as f64 * unit).max(0.5) as f32).collect();
        if lens.len() % 2 == 1 {
            let again = lens.clone();
            lens.extend(again);
        }
        stroke.dash = StrokeDash::new(lens, (pen.dash_offset as f64 * unit) as f32);
    }
    let brush = pen.brush.clone();
    paint(g, p, &path, &brush, |g, src, clip| g.cv.stroke(&path, &stroke, src, BlendMode::SourceOver, clip));
}

fn clip_with(g: &mut Gdi, path: Option<Path>, rule: FillRule, mode: u32) {
    let Some(path) = path else { return };
    if g.cv.measure {
        return;
    }
    let Some(mut mask) = Mask::new(g.cv.width(), g.cv.height()) else { return };
    crate::canvas::guarded(|| mask.fill_path(&path, rule, true, Transform::identity()));
    // CombineMode: 0 replace, 1 intersect, 2 union, 3 xor, 4 exclude, 5 complement.
    let gdi_mode = match mode {
        0 => 5,
        1 => 1,
        2 => 2,
        3 => 3,
        4 => 4,
        _ => 1,
    };
    g.combine_clip(Some(mask), gdi_mode);
}

/// A region as a path on the canvas; combined regions are flattened into
/// the first operand's shape when the second is empty, otherwise built as a
/// union of both outlines (exact for the common rectangle and path cases
/// with union).
fn region_path(g: &Gdi, p: &Plus, r: &Region) -> Option<Path> {
    let mut geo = geo(g, p);
    add_region(&mut geo, r, g, p);
    geo.finish()
}

fn add_region(geo: &mut Geo, r: &Region, g: &Gdi, p: &Plus) {
    match r {
        Region::Rect(b) => geo.rect(b[0] as f64, b[1] as f64, (b[0] + b[2]) as f64, (b[1] + b[3]) as f64),
        Region::Path(path) => {
            if let Some(built) = build_path(g, p, path) {
                // Already on the canvas: add it untransformed.
                let _ = built;
                for (i, pt) in path.points.iter().enumerate() {
                    if path.types[i] & 0x0F == 0 {
                        geo.move_to(pt.0, pt.1);
                    } else {
                        geo.line_to(pt.0, pt.1);
                    }
                }
                geo.close();
            }
        }
        Region::Infinite => geo.rect(-1.0e6, -1.0e6, 1.0e6, 1.0e6),
        Region::Empty => {}
        Region::Combine(a, b) => {
            add_region(geo, a, g, p);
            add_region(geo, b, g, p);
        }
    }
}

fn draw_image(g: &mut Gdi, p: &Plus, id: usize, src: [f32; 4], dst: [(f64, f64); 3], depth: u32) {
    let image = match p.objects.get(id) {
        Some(Some(Obj::Image(i))) => i,
        _ => return,
    };
    let bmp: Rc<Bitmap> = match image {
        Image::Bitmap(b) => b.clone(),
        Image::Metafile(bytes) => {
            if depth >= MAX_DEPTH || g.cv.measure {
                return;
            }
            // Draw the nested metafile at a size that suits where it lands.
            let Ok(tex) = crate::render_nested(bytes, 1024, depth + 1) else { return };
            let slerp_scene::textures::Pixels::Rgba8(px) = &tex.levels[0].surfaces[0] else { return };
            Rc::new(Bitmap {
                width: tex.levels[0].width,
                height: tex.levels[0].height,
                rgba: px.clone(),
                mono: false,
                has_alpha: true,
            })
        }
    };
    let is_meta = matches!(image, Image::Metafile(_));
    let (sw, sh) = (bmp.width as f64, bmp.height as f64);
    // The source rectangle is in pixels for bitmaps; for metafiles it is in
    // the nested picture's own units, so use the whole picture.
    let r = if is_meta || src[2] <= 0.0 || src[3] <= 0.0 {
        [0.0, 0.0, sw, sh]
    } else {
        [src[0] as f64, src[1] as f64, src[2] as f64, src[3] as f64]
    };
    let x0 = (r[0].max(0.0)).floor() as i64;
    let y0 = (r[1].max(0.0)).floor() as i64;
    let x1 = ((r[0] + r[2]).min(sw)).ceil() as i64;
    let y1 = ((r[1] + r[3]).min(sh)).ceil() as i64;
    if x1 <= x0 || y1 <= y0 {
        return;
    }
    let (cw, ch) = ((x1 - x0) as u32, (y1 - y0) as u32);
    let mut rgba = Vec::with_capacity(cw as usize * ch as usize * 4);
    for y in y0..y1 {
        let start = ((y as usize) * bmp.width as usize + x0 as usize) * 4;
        rgba.extend_from_slice(&bmp.rgba[start..start + cw as usize * 4]);
    }
    let cropped = Bitmap { width: cw, height: ch, rgba, mono: false, has_alpha: bmp.has_alpha };
    let Some(pm) = cropped.to_pixmap() else { return };
    // Pixels to page units: a parallelogram given by three corners.
    let (a, b, c) = (dst[0], dst[1], dst[2]);
    let to_page = Affine::new(
        (b.0 - a.0) / cw as f64,
        (b.1 - a.1) / cw as f64,
        (c.0 - a.0) / ch as f64,
        (c.1 - a.1) / ch as f64,
        a.0,
        a.1,
    );
    let xf = to_page.then(p.canvas_matrix(g));
    if !xf.is_finite() {
        return;
    }
    let clip = g.dc.clip.clone();
    g.cv.draw_bitmap(&pm, xf.to_skia(), xf.a.hypot(xf.b) < 3.0, 1.0, clip.as_deref());
}

fn draw_string(g: &mut Gdi, p: &Plus, flags: u16, d: &Rec) {
    let (brush, off) = brush_arg(p, flags, d, 0);
    let format = d.u32(off) as usize;
    let n = (d.u32(off + 4) as usize).min(1 << 16);
    let layout = [d.f32(off + 8) as f64, d.f32(off + 12) as f64, d.f32(off + 16) as f64, d.f32(off + 20) as f64];
    let units: Vec<u16> = (0..n).map(|i| d.u16(off + 24 + i * 2)).collect();
    let text = crate::make::decode_utf16(&units);
    let Some(color) = brush.as_ref().map(brush_colour) else { return };
    let font = match p.objects.get((flags & 0xFF) as usize) {
        Some(Some(Obj::Font(f))) => f.clone(),
        _ => return,
    };
    // The string format aligns the line within the layout rectangle.
    let (align, line_align) = match p.objects.get(format) {
        Some(Some(Obj::Format { align, line_align })) => (*align, *line_align),
        _ => (0, 0),
    };
    let per = font_unit_px(p, font.unit);
    let em = font.size as f64 * per / p.state.page_unit_px.max(1e-9);
    let cell = em * 1.117;
    let x = match align {
        1 => layout[0] + layout[2] / 2.0,
        2 => layout[0] + layout[2],
        _ => layout[0],
    };
    let y = match line_align {
        1 => layout[1] + (layout[3] - cell) / 2.0,
        2 => layout[1] + layout[3] - cell,
        _ => layout[1],
    };
    let h_align = match align {
        1 => crate::state::TA_CENTER,
        2 => crate::state::TA_RIGHT,
        _ => 0,
    };
    place_text(g, p, &font, &text, (x, y), None, color, h_align);
}

/// One colour standing for a brush, for text and other things drawn
/// with a plain colour.
fn brush_colour(b: &Brush) -> Argb {
    match b {
        Brush::Solid(c) => *c,
        Brush::Hatch { fore, .. } => *fore,
        Brush::Linear { start, .. } => *start,
        Brush::Radial { centre_colour, .. } => *centre_colour,
        Brush::Texture { image, .. } => {
            let px = image.rgba.chunks_exact(4).take(1024);
            let n = px.len().max(1) as u32;
            let sum = px.fold([0u32; 3], |a, p| [a[0] + p[0] as u32, a[1] + p[1] as u32, a[2] + p[2] as u32]);
            Argb([(sum[0] / n) as u8, (sum[1] / n) as u8, (sum[2] / n) as u8], 255)
        }
    }
}

fn font_unit_px(p: &Plus, unit: u32) -> f64 {
    match unit {
        3 => p.dpi.0 / 72.0,
        4 => p.dpi.0,
        5 => p.dpi.0 / 300.0,
        6 => p.dpi.0 / 25.4,
        _ => 1.0,
    }
}

fn draw_driver_string(g: &mut Gdi, p: &Plus, flags: u16, d: &Rec) {
    let (brush, off) = brush_arg(p, flags, d, 0);
    let opts = d.u32(off);
    let has_matrix = d.u32(off + 4) != 0;
    let n = (d.u32(off + 8) as usize).min(1 << 16);
    let glyph_at = off + 12;
    let glyphs: Vec<u16> = (0..n).map(|i| d.u16(glyph_at + i * 2)).collect();
    // The positions follow the glyphs directly, without padding.
    let pos_at = glyph_at + n * 2;
    let pos = points(d, pos_at, n, false);
    let Some(color) = brush.as_ref().map(brush_colour) else { return };
    let font = match p.objects.get((flags & 0xFF) as usize) {
        Some(Some(Obj::Font(f))) => f.clone(),
        _ => return,
    };
    // Glyph indices mean nothing without the font; with the cmap option they
    // are characters.
    if opts & 1 == 0 {
        g.warnings.skip("EMF+ driver string with glyph indices");
        return;
    }
    let text = crate::make::decode_utf16(&glyphs);
    let _ = has_matrix;
    let start = pos.first().copied().unwrap_or((0.0, 0.0));
    let dx: Vec<f64> = if opts & 4 != 0 { Vec::new() } else { pos.windows(2).map(|w| w[1].0 - w[0].0).collect() };
    place_text(g, p, &font, &text, start, if dx.is_empty() { None } else { Some(dx) }, color, 0);
}

/// Draws text with the GDI text routine, in page units (the font's size is
/// in its own unit).
#[allow(clippy::too_many_arguments)]
fn place_text(
    g: &mut Gdi,
    p: &Plus,
    font: &Font,
    text: &[char],
    at: (f64, f64),
    dx: Option<Vec<f64>>,
    color: Argb,
    h_align: u32,
) {
    let per = font_unit_px(p, font.unit);
    // The device context's logical coordinates are the page units here.
    let saved = (g.dc.total, g.dc.font.clone(), g.dc.text_color, g.dc.text_align, g.dc.bk_opaque);
    g.dc.total = p.canvas_matrix(g);
    g.dc.font = Rc::new(crate::state::Font {
        // The em size in page units: the unit's pixels over the page scale.
        height: -(font.size as f64 * per / p.state.page_unit_px.max(1e-9)),
        weight: if font.style & 1 != 0 { 700 } else { 400 },
        italic: font.style & 2 != 0,
        underline: font.style & 4 != 0,
        strike_out: font.style & 8 != 0,
        face: font.family.clone(),
        ..crate::state::Font::default()
    });
    g.dc.text_color = color.0;
    g.dc.text_alpha = color.1 as f32 / 255.0;
    g.dc.text_align = h_align;
    g.dc.bk_opaque = false;
    let dx = dx.unwrap_or_default();
    g.text_out(at, text, &dx, 0, None);
    (g.dc.total, g.dc.font, g.dc.text_color, g.dc.text_align, g.dc.bk_opaque) = saved;
    g.dc.text_alpha = 1.0;
}

//endregion
