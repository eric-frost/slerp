//! Windows Metafiles (WMF): a 16-bit GDI recording.  The file is a header
//! (optionally preceded by the "placeable" header that Aldus added to say
//! how big the picture is) and a list of records, each a function number and
//! its arguments in reverse of the order the GDI call takes them.

use crate::dib;
use crate::gdi::{Gdi, Object, Region};
use crate::make;
use crate::shapes::ArcKind;
use crate::state::*;
use crate::{Rgb, bytes::Rec};
use std::rc::Rc;

pub const PLACEABLE_KEY: u32 = 0x9AC6_CDD7;

/// The parts of the header the player needs.
pub struct Wmf<'a> {
    data: &'a [u8],
    /// The picture's bounding box in logical units, from the placeable header.
    pub bbox: Option<[i32; 4]>,
    /// Logical units per inch, from the placeable header.
    pub inch: u32,
    /// Where the records start.
    start: usize,
}

/// Where the picture lies in device space and how wide it is for its
/// height.
pub struct Plan {
    pub dev_frame: [f64; 4],
    pub aspect: f64,
}

/// Do these bytes start a WMF, with or without the placeable header?
pub fn looks_like_wmf(head: &[u8]) -> bool {
    if head.len() >= 4 && u32::from_le_bytes([head[0], head[1], head[2], head[3]]) == PLACEABLE_KEY {
        return true;
    }
    head.len() >= 6
        && matches!(head[0], 1 | 2)
        && head[1] == 0
        && head[2] == 9
        && head[3] == 0
        && matches!((head[4], head[5]), (0, 1) | (0, 3))
}

impl<'a> Wmf<'a> {
    pub fn parse(data: &'a [u8]) -> Option<Wmf<'a>> {
        let r = Rec::new(0, data);
        let (mut start, mut bbox, mut inch) = (0usize, None, 0u32);
        if r.u32(0) == PLACEABLE_KEY {
            bbox = Some([r.i16(6) as i32, r.i16(8) as i32, r.i16(10) as i32, r.i16(12) as i32]);
            inch = r.u16(14) as u32;
            start = 22;
        }
        // GDI+ stores the placeable header padded to 24 bytes.
        let looks_standard = |at: usize| {
            let h = Rec::new(0, data.get(at..).unwrap_or_default());
            matches!(h.u16(0), 1 | 2) && h.u16(2) == 9
        };
        if start == 22 && !looks_standard(22) && looks_standard(24) {
            start = 24;
        }
        let h = Rec::new(0, data.get(start..)?);
        if !matches!(h.u16(0), 1 | 2) || h.u16(2) != 9 {
            return None;
        }
        // The header's own size field is in words.
        Some(Wmf { data, bbox, inch, start: start + 18 })
    }

    /// The records, as (function, arguments), stopping at damage or the
    /// end record.
    fn records(&self) -> impl Iterator<Item = Rec<'a>> {
        let data = self.data;
        let mut at = self.start;
        std::iter::from_fn(move || {
            let words = u32::from_le_bytes(data.get(at..at + 4)?.try_into().ok()?) as usize;
            let func = u16::from_le_bytes(data.get(at + 4..at + 6)?.try_into().ok()?);
            if words < 3 {
                return None;
            }
            let end = at.checked_add(words.checked_mul(2)?)?;
            let params = data.get(at + 6..end.min(data.len()))?;
            at = end;
            if func == 0 {
                return None;
            }
            Some(Rec::new(func, params))
        })
    }

    /// The logical rectangle the picture covers: the placeable header's
    /// bounding box, or else the window the file sets before it draws.
    fn logical_frame(&self) -> Option<[f64; 4]> {
        if let Some(b) = self.bbox.filter(|b| b[0] != b[2] && b[1] != b[3]) {
            return Some([b[0] as f64, b[1] as f64, b[2] as f64, b[3] as f64]);
        }
        let mut g = scratch_gdi();
        for rec in self.records() {
            if is_drawing(rec.func) {
                break;
            }
            mapping(&mut g, &rec);
        }
        let (o, e) = (g.dc.win_org, g.dc.win_ext);
        let set = e != (1.0, 1.0) || g.dc.map_mode != MM_TEXT;
        (set && e.0 != 0.0 && e.1 != 0.0).then_some([o.0, o.1, o.0 + e.0, o.1 + e.1])
    }

    /// The picture's frame.  The player plays into a device whose pixels
    /// are the logical units (for a file without a header) or 96 to the
    /// inch of the header's units; that device rectangle is the frame.
    pub fn plan(&self) -> Option<Plan> {
        let f = self.logical_frame()?;
        let (w, h) = ((f[2] - f[0]).abs(), (f[3] - f[1]).abs());
        if !(w > 0.0 && h > 0.0) {
            return None;
        }
        let (nw, nh) = self.device_size(w, h);
        Some(Plan { dev_frame: [0.0, 0.0, nw, nh], aspect: w / h })
    }

    /// The size in device pixels the frame is played into.
    fn device_size(&self, w: f64, h: f64) -> (f64, f64) {
        if self.bbox.is_some() && self.inch > 0 {
            let k = 96.0 / self.inch as f64;
            (w * k, h * k)
        } else {
            (w, h)
        }
    }

    /// The picture's longer side in pixels at 96 dots per inch, from the
    /// placeable header.
    pub fn native_long(&self) -> Option<f64> {
        let b = self.bbox?;
        (self.inch > 0).then(|| (b[2] - b[0]).abs().max((b[3] - b[1]).abs()) as f64 * 96.0 / self.inch as f64)
    }

    pub fn play(&self, g: &mut Gdi) {
        // The device the picture is played into: the frame, in pixels.
        if let Some(f) = self.logical_frame() {
            let (nw, nh) = self.device_size((f[2] - f[0]).abs(), (f[3] - f[1]).abs());
            g.dc.map_mode = MM_ANISOTROPIC;
            g.dc.win_org = (f[0], f[1]);
            g.dc.win_ext = (f[2] - f[0], f[3] - f[1]);
            g.dc.vp_org = (0.0, 0.0);
            g.dc.vp_ext = (nw, nh);
            g.update_transform();
        }
        let mut state = State::default();
        for rec in self.records() {
            if g.out_of_time() {
                break;
            }
            play_record(g, &rec, &mut state);
        }
    }
}

/// The reference device for fixed mapping modes: 96 dots per inch.
pub const PX_PER_MM: f64 = 96.0 / 25.4;

fn scratch_gdi() -> Gdi {
    Gdi::new(
        crate::canvas::Canvas::measuring(),
        crate::affine::Affine::IDENTITY,
        (PX_PER_MM, PX_PER_MM),
        std::time::Instant::now() + std::time::Duration::from_secs(3600),
    )
}

/// Function numbers of records that put marks on the page (or read the
/// page, like the flood fill).
fn is_drawing(func: u16) -> bool {
    matches!(
        func,
        0x0213
            | 0x0325
            | 0x0324
            | 0x0538
            | 0x041B
            | 0x061C
            | 0x0418
            | 0x0817
            | 0x081A
            | 0x0830
            | 0x0521
            | 0x0A32
            | 0x0922
            | 0x0940
            | 0x0B23
            | 0x0B41
            | 0x0F43
            | 0x0D33
            | 0x061D
            | 0x0228
            | 0x0429
            | 0x012B
            | 0x012A
            | 0x0419
            | 0x0548
            | 0x041F
    )
}

/// Per-playback bits that aren't part of the device context.
#[derive(Default)]
struct State {}

/// Mapping records, applied alone to find the frame and again in the real
/// pass.  Returns true when the record was one of them.
fn mapping(g: &mut Gdi, r: &Rec) -> bool {
    let (a, b) = (r.i16(0) as f64, r.i16(2) as f64);
    match r.func {
        0x0103 => g.set_map_mode(r.u16(0) as u32),
        0x020B => g.set_window_org(b, a),
        0x020C => g.set_window_ext(b, a),
        0x020D => g.set_viewport_org(b, a),
        0x020E => g.set_viewport_ext(b, a),
        0x020F => g.set_window_org(g.dc.win_org.0 + b, g.dc.win_org.1 + a),
        0x0211 => g.set_viewport_org(g.dc.vp_org.0 + b, g.dc.vp_org.1 + a),
        0x0410 => g.scale_window_ext(r.i16(6) as f64, r.i16(4) as f64, r.i16(2) as f64, r.i16(0) as f64),
        0x0412 => g.scale_viewport_ext(r.i16(6) as f64, r.i16(4) as f64, r.i16(2) as f64, r.i16(0) as f64),
        _ => return false,
    }
    true
}

fn pt(r: &Rec, o: usize) -> (f64, f64) {
    (r.i16(o) as f64, r.i16(o + 2) as f64)
}

/// Points stored as pairs of 16-bit coordinates, as many as the data holds.
fn points(r: &Rec, offset: usize, count: usize) -> Vec<(f64, f64)> {
    let count = count.min(r.len().saturating_sub(offset) / 4);
    (0..count).map(|i| pt(r, offset + i * 4)).collect()
}

/// Plays one record.
fn play_record(g: &mut Gdi, r: &Rec, _st: &mut State) {
    if mapping(g, r) {
        return;
    }
    match r.func {
        0x001E => g.save(),
        0x0127 => g.restore(r.i16(0) as i32),
        0x0102 => g.dc.bk_opaque = r.u16(0) == 2,
        0x0104 => g.dc.rop2 = r.u16(0) as u32,
        0x0106 => g.dc.winding = r.u16(0) == 2,
        0x0107 => g.dc.stretch_mode = r.u16(0) as u32,
        0x0108 => g.dc.text_extra = r.i16(0) as f64,
        0x0201 => g.dc.bk_color = g.color(r.u32(0)),
        0x0209 => g.dc.text_color = g.color(r.u32(0)),
        0x012E => g.dc.text_align = r.u16(0) as u32,
        0x020A => {
            g.dc.break_extra = r.i16(2) as f64;
            g.dc.break_count = r.i16(0) as f64;
        }
        0x0214 => g.move_to(r.i16(2) as f64, r.i16(0) as f64),
        0x0213 => g.line_to(r.i16(2) as f64, r.i16(0) as f64),
        0x0325 => {
            let pts = points(r, 2, r.u16(0) as usize);
            g.poly(&pts, false, false, true, false);
        }
        0x0324 => {
            let pts = points(r, 2, r.u16(0) as usize);
            g.poly(&pts, true, true, true, false);
        }
        0x0538 => {
            let n = (r.u16(0) as usize).min(r.len() / 2);
            let counts: Vec<usize> = (0..n).map(|i| r.u16(2 + i * 2) as usize).collect();
            let mut at = 2 + n * 2;
            let mut polys = Vec::new();
            for c in counts {
                polys.push(points(r, at, c));
                at += c * 4;
            }
            g.poly_poly(&polys, true);
        }
        0x041B => g.rectangle(r.i16(6) as f64, r.i16(4) as f64, r.i16(2) as f64, r.i16(0) as f64),
        0x0418 => g.ellipse(r.i16(6) as f64, r.i16(4) as f64, r.i16(2) as f64, r.i16(0) as f64),
        0x061C => g.round_rect(
            r.i16(10) as f64,
            r.i16(8) as f64,
            r.i16(6) as f64,
            r.i16(4) as f64,
            r.i16(2) as f64,
            r.i16(0) as f64,
        ),
        0x0817 | 0x081A | 0x0830 => {
            let kind = match r.func {
                0x0817 => ArcKind::Arc,
                0x081A => ArcKind::Pie,
                _ => ArcKind::Chord,
            };
            let bbox = [r.i16(14) as f64, r.i16(12) as f64, r.i16(10) as f64, r.i16(8) as f64];
            g.arc_shape(kind, bbox, (r.i16(6) as f64, r.i16(4) as f64), (r.i16(2) as f64, r.i16(0) as f64));
        }
        0x0521 => {
            let n = (r.u16(0) as usize).min(r.len());
            let text = make::decode_ansi(r.bytes(2, n), g.dc.font.charset);
            let at = 2 + n + (n & 1);
            g.text_out((r.i16(at + 2) as f64, r.i16(at) as f64), &text, &[], 0, None);
        }
        0x0A32 => ext_text_out(g, r),
        0x02FA => {
            let color = g.color(r.u32(6));
            let pen = make::pen(r.u16(0) as u32, r.i16(2) as f64, color, Vec::new());
            g.add_object(Object::Pen(Rc::new(pen)));
        }
        0x02FC => {
            let color = g.color(r.u32(2));
            g.add_object(Object::Brush(Rc::new(make::simple_brush(r.u16(0) as u32, color, r.u16(6) as u32))));
        }
        0x02FB => {
            let face = String::from_utf8_lossy(r.bytes(18, 32).split(|b| *b == 0).next().unwrap_or(&[])).into_owned();
            let f = make::font(
                r.i16(0) as i32,
                r.i16(4) as i32,
                r.i16(8) as i32,
                [r.u8(10), r.u8(11), r.u8(12)],
                r.u8(13),
                r.u8(17),
                face,
            );
            g.add_object(Object::Font(Rc::new(f)));
        }
        0x0142 => {
            let brush = pattern_brush(g, r);
            g.add_object(Object::Brush(Rc::new(brush)));
        }
        0x01F9 => {
            // The old device-dependent pattern brush: an 8x8 monochrome tile.
            let brush = bitmap16(r, 0).map_or(Brush::Solid([128, 128, 128]), |b| mono_brush(&b));
            g.add_object(Object::Brush(Rc::new(brush)));
        }
        0x00F7 => {
            let n = (r.u16(2) as usize).min(r.len() / 4);
            let pal: Vec<Rgb> = (0..n).map(|i| [r.u8(4 + i * 4), r.u8(5 + i * 4), r.u8(6 + i * 4)]).collect();
            g.add_object(Object::Palette(Rc::new(pal)));
        }
        0x06FF => {
            let mut region = region_object(r);
            if !g.reserve(region.rects.len() * 32) {
                region.rects.clear();
            }
            g.add_object(Object::Region(Rc::new(region)));
        }
        0x02FD | 0x02FE => g.add_object(Object::Other),
        0x012D => {
            let i = r.u16(0) as u32;
            if let Some(o) = g.object(i).cloned() {
                if let Object::Region(reg) = &o {
                    g.clip_region(reg, 5);
                } else {
                    g.select(o);
                }
            }
        }
        0x01F0 => g.delete_object(r.u16(0) as u32),
        0x0234 => {
            if let Some(Object::Palette(p)) = g.object(r.u16(0) as u32).cloned() {
                g.dc.palette = p;
            }
        }
        0x012C => match g.object(r.u16(0) as u32).cloned() {
            Some(Object::Region(reg)) => g.clip_region(&reg, 5),
            _ => g.reset_clip(),
        },
        0x0415 => g.clip_rect(r.i16(6) as f64, r.i16(4) as f64, r.i16(2) as f64, r.i16(0) as f64, 4),
        0x0416 => g.clip_rect(r.i16(6) as f64, r.i16(4) as f64, r.i16(2) as f64, r.i16(0) as f64, 1),
        0x0220 => g.offset_clip(r.i16(2) as f64, r.i16(0) as f64),
        0x0228 => {
            if let (Some(Object::Region(reg)), Some(Object::Brush(b))) =
                (g.object(r.u16(0) as u32).cloned(), g.object(r.u16(2) as u32).cloned())
            {
                let old = std::mem::replace(&mut g.dc.brush, b);
                if let Some(p) = g.region_path(&reg) {
                    g.fill_with_brush(&p, tiny_skia::FillRule::Winding);
                }
                g.dc.brush = old;
            }
        }
        0x012B => {
            if let Some(Object::Region(reg)) = g.object(r.u16(0) as u32).cloned()
                && let Some(p) = g.region_path(&reg)
            {
                g.fill_with_brush(&p, tiny_skia::FillRule::Winding);
            }
        }
        0x061D => g.pat_blt(r.i16(10) as f64, r.i16(8) as f64, r.i16(6) as f64, r.i16(4) as f64, r.u32(0)),
        0x0922 | 0x0940 | 0x0B23 | 0x0B41 => blit(g, r),
        0x0F43 => {
            // StretchDIBits: raster op, usage, source and destination
            // rectangles (height first), then the DIB.
            let bmp = dib::decode_packed(r.tail(22), &g.dc.palette);
            if let Some(mut bmp) = bmp {
                bmp.make_opaque();
                let src = [r.i16(12) as i64, r.i16(10) as i64, r.i16(8) as i64, r.i16(6) as i64];
                let dst = [r.i16(20) as f64, r.i16(18) as f64, r.i16(16) as f64, r.i16(14) as f64];
                g.draw_bitmap(&bmp, src, dst, r.u32(0), None);
            }
        }
        0x0D33 => {
            let bmp = dib::decode_packed(r.tail(18), &g.dc.palette);
            if let Some(mut bmp) = bmp {
                bmp.make_opaque();
                let (w, h) = (r.i16(12) as i64, r.i16(10) as i64);
                let src = [r.i16(8) as i64, r.i16(6) as i64, w, h];
                let dst = [r.i16(16) as f64, r.i16(14) as f64, w as f64, h as f64];
                g.draw_bitmap(&bmp, src, dst, 0x00CC_0020, None);
            }
        }
        0x041F => {
            let c = g.color(r.u32(0));
            let (x, y) = (r.i16(6) as f64, r.i16(4) as f64);
            let old = std::mem::replace(&mut g.dc.brush, Rc::new(Brush::Solid(c)));
            g.pat_blt(x, y, 1.0, 1.0, 0x00F0_0021);
            g.dc.brush = old;
        }
        0x0626 | 0x0231 | 0x0105 | 0x0139 | 0x0035 | 0x0037 | 0x0036 => {}
        0x0419 | 0x0548 => g.warnings.skip("flood fill"),
        0x0429 => {
            if let (Some(Object::Region(reg)), Some(Object::Brush(b))) =
                (g.object(r.u16(0) as u32).cloned(), g.object(r.u16(2) as u32).cloned())
                && let Brush::Solid(color) = &*b
                && let Some(path) = g.region_path(&reg)
            {
                let mut pen = Pen::solid(*color);
                pen.width = (r.i16(4).abs() as f64 + r.i16(6).abs() as f64) / 2.0;
                pen.geometric = true;
                let old = std::mem::replace(&mut g.dc.pen, Rc::new(pen));
                g.stroke_with_pen(&path);
                g.dc.pen = old;
            }
        }
        0x012A => {
            if let Some(Object::Region(reg)) = g.object(r.u16(0) as u32).cloned()
                && let Some(path) = g.region_path(&reg)
            {
                let old = (std::mem::replace(&mut g.dc.brush, Rc::new(Brush::Solid([255, 255, 255]))), g.dc.rop2);
                g.dc.rop2 = 6;
                g.fill_with_brush(&path, tiny_skia::FillRule::Winding);
                (g.dc.brush, g.dc.rop2) = old;
            }
        }
        0x0149 => g.dc.rtl = r.u32(0) & 1 != 0,
        f => g.warnings.skip(format!("WMF record 0x{f:04X}")),
    }
}

fn ext_text_out(g: &mut Gdi, r: &Rec) {
    let (y, x) = (r.i16(0) as f64, r.i16(2) as f64);
    let n = (r.u16(4) as usize).min(r.len());
    let opts = r.u16(6) as u32;
    let mut at = 8;
    let rect = if opts & (crate::text::ETO_OPAQUE | crate::text::ETO_CLIPPED) != 0 {
        let rc = [r.i16(8) as f64, r.i16(10) as f64, r.i16(12) as f64, r.i16(14) as f64];
        at += 8;
        Some(rc)
    } else {
        None
    };
    let spans = make::decode_ansi_spans(r.bytes(at, n), g.dc.font.charset);
    let text: Vec<char> = spans.iter().map(|s| s.0).collect();
    let text_at = at;
    at += n + (n & 1);
    // The advances are spaced by byte; a double-byte character takes the
    // sum of its bytes' entries.
    let dx: Vec<f64> = if r.len() >= at + n * 2 {
        let mut byte = 0usize;
        spans
            .iter()
            .map(|(_, k)| {
                let sum: i32 = (0..*k as usize).map(|j| r.i16(at + (byte + j) * 2) as i32).sum();
                byte += *k as usize;
                sum as f64
            })
            .collect()
    } else {
        Vec::new()
    };
    let _ = text_at;
    g.text_out((x, y), &text, &dx, opts, rect);
}

/// `BitBlt`, `StretchBlt` and their DIB forms.
fn blit(g: &mut Gdi, r: &Rec) {
    let stretch = matches!(r.func, 0x0B23 | 0x0B41);
    let is_dib = matches!(r.func, 0x0940 | 0x0B41);
    let words = (r.func >> 8) as usize;
    // Records without a bitmap are two bytes longer in their fixed part and
    // carry a reserved word after the source origin.
    let no_bitmap = r.len() / 2 == words;
    let rop = r.u32(0);
    let (src_h, src_w, y_src, x_src, mut at);
    if stretch {
        src_h = r.i16(4) as i64;
        src_w = r.i16(6) as i64;
        y_src = r.i16(8) as i64;
        x_src = r.i16(10) as i64;
        at = 12;
    } else {
        y_src = r.i16(4) as i64;
        x_src = r.i16(6) as i64;
        src_h = 0;
        src_w = 0;
        at = 8;
    }
    if no_bitmap {
        at += 2;
    }
    let (dst_h, dst_w, y_dst, x_dst) =
        (r.i16(at) as f64, r.i16(at + 2) as f64, r.i16(at + 4) as f64, r.i16(at + 6) as f64);
    at += 8;
    if no_bitmap {
        g.pat_blt(x_dst, y_dst, dst_w, dst_h, rop);
        return;
    }
    let bmp = if is_dib { dib::decode_packed(r.tail(at), &g.dc.palette) } else { bitmap16(r, at) };
    let Some(mut bmp) = bmp else { return };
    if !bmp.mono {
        bmp.make_opaque();
    }
    let (sw, sh) = if stretch { (src_w, src_h) } else { (dst_w as i64, dst_h as i64) };
    let src = [x_src, y_src, sw, sh];
    g.draw_bitmap(&bmp, src, [x_dst, y_dst, dst_w, dst_h], rop, None);
}

/// A device-dependent bitmap (`Bitmap16`): a small header and rows padded
/// to 16 bits.
fn bitmap16(r: &Rec, at: usize) -> Option<crate::dib::Bitmap> {
    let (w, h, stride) = (r.u16(at + 2) as usize, r.u16(at + 4) as usize, r.u16(at + 6) as usize);
    let (planes, bits) = (r.u8(at + 8) as usize, r.u8(at + 9) as usize);
    if w == 0 || h == 0 || w * h > crate::dib::MAX_PIXELS as usize || planes != 1 || stride == 0 {
        return None;
    }
    let data = r.tail(at + 10);
    if data.len() < stride * h && data.len() < stride * (h - 1) + (w * bits).div_ceil(8) {
        return None;
    }
    let mut rgba = vec![255u8; w * h * 4];
    for y in 0..h {
        let row = &data[(y * stride).min(data.len())..];
        for x in 0..w {
            let c: Rgb = match bits {
                1 => {
                    if row.get(x / 8).copied().unwrap_or(0) & (0x80 >> (x % 8)) != 0 {
                        [255, 255, 255]
                    } else {
                        [0, 0, 0]
                    }
                }
                4 => {
                    let b = row.get(x / 2).copied().unwrap_or(0);
                    let i = if x % 2 == 0 { b >> 4 } else { b & 15 } as usize;
                    EGA[i]
                }
                8 => {
                    let v = row.get(x).copied().unwrap_or(0);
                    [v, v, v]
                }
                24 => row.get(x * 3..x * 3 + 3).map_or([0; 3], |p| [p[2], p[1], p[0]]),
                32 => row.get(x * 4..x * 4 + 3).map_or([0; 3], |p| [p[2], p[1], p[0]]),
                _ => return None,
            };
            rgba[(y * w + x) * 4..(y * w + x) * 4 + 3].copy_from_slice(&c);
        }
    }
    Some(crate::dib::Bitmap { width: w as u32, height: h as u32, rgba, mono: bits == 1, has_alpha: false })
}

const EGA: [Rgb; 16] = [
    [0, 0, 0],
    [128, 0, 0],
    [0, 128, 0],
    [128, 128, 0],
    [0, 0, 128],
    [128, 0, 128],
    [0, 128, 128],
    [192, 192, 192],
    [128, 128, 128],
    [255, 0, 0],
    [0, 255, 0],
    [255, 255, 0],
    [0, 0, 255],
    [255, 0, 255],
    [0, 255, 255],
    [255, 255, 255],
];

fn mono_brush(b: &crate::dib::Bitmap) -> Brush {
    // Clear bits take the text colour, set bits the background colour.
    let on = b.rgba.chunks_exact(4).map(|p| p[0] > 127).collect();
    Brush::Mono(Rc::new(MonoTile { width: b.width as usize, height: b.height as usize, on }))
}

fn pattern_brush(g: &mut Gdi, r: &Rec) -> Brush {
    let usage = r.u16(2) as u32;
    // The spec says style 3 carries a device-dependent bitmap, but writers
    // put a DIB there too; a DIB starts with its header size.
    let is_dib = matches!(r.u32(4), 12 | 40 | 52 | 56 | 108 | 124);
    let bmp = if is_dib {
        let dib = r.tail(4);
        dib::packed_pixel_offset(dib).and_then(|off| dib::decode(dib, dib.get(off..)?, usage, &g.dc.palette))
    } else {
        bitmap16(r, 4)
    };
    match bmp {
        Some(b) if b.width <= 64 && b.height <= 64 && is_two_colour(&b) => mono_brush(&b),
        Some(b) => g.pattern_pixmap(&b).map_or(Brush::Solid([128, 128, 128]), |p| Brush::Pattern(Rc::new(p))),
        None => Brush::Solid([128, 128, 128]),
    }
}

/// True for bitmaps of only black and white, which pattern brushes paint
/// in the text and background colours instead.
fn is_two_colour(b: &crate::dib::Bitmap) -> bool {
    b.mono || b.rgba.chunks_exact(4).all(|p| (p[0] == 0 || p[0] == 255) && p[0] == p[1] && p[1] == p[2])
}

/// A region object: scan lines of left and right edges.
fn region_object(r: &Rec) -> Region {
    // Next, Type, Size, Count, MaxScan, bounding box, then the scans.
    let count = r.u16(8) as usize;
    let mut rects = Vec::new();
    let mut at = 22;
    for _ in 0..count.min(1 << 14) {
        let n = r.u16(at) as usize;
        let (top, bottom) = (r.i16(at + 2) as f64, r.i16(at + 4) as f64);
        for k in 0..(n / 2).min(1024) {
            rects.push([r.i16(at + 6 + k * 4) as f64, top, r.i16(at + 8 + k * 4) as f64, bottom]);
        }
        at += 6 + n * 2 + 2;
        if at >= r.len() {
            break;
        }
    }
    if rects.is_empty() {
        // Fall back to the bounding box.
        rects.push([r.i16(14) as f64, r.i16(16) as f64, r.i16(18) as f64, r.i16(20) as f64]);
    }
    Region { rects, device: false }
}
