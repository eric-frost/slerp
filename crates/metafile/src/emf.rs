//! Enhanced Metafiles (EMF): a 32-bit GDI recording.  Unlike WMF the file
//! knows the size of the device it was made on, so the picture's frame in
//! millimetres can be turned into device pixels exactly, and records carry
//! world transforms, paths and extended pens.

use crate::Rgb;
use crate::affine::Affine;
use crate::bytes::Rec;
use crate::dib::{self, Bitmap};
use crate::emfplus;
use crate::gdi::{Gdi, Object, Region};
use crate::make;
use crate::shapes::ArcKind;
use crate::state::*;
use crate::wmf::Plan;
use std::rc::Rc;
use tiny_skia::FillRule;

const SIGNATURE: u32 = 0x464D_4520;

pub struct Emf<'a> {
    data: &'a [u8],
    /// Device-unit extent of the drawing, inclusive.
    pub bounds: [i32; 4],
    /// The intended picture frame in 0.01 mm, inclusive.
    pub frame: [i32; 4],
    pub device: (i32, i32),
    pub millimeters: (i32, i32),
    /// The device size in micrometers when the header is long enough.
    pub micrometers: (i32, i32),
}

pub fn looks_like_emf(head: &[u8]) -> bool {
    head.len() >= 44
        && u32::from_le_bytes([head[0], head[1], head[2], head[3]]) == 1
        && u32::from_le_bytes([head[40], head[41], head[42], head[43]]) == SIGNATURE
}

impl<'a> Emf<'a> {
    pub fn parse(data: &'a [u8]) -> Option<Emf<'a>> {
        if !looks_like_emf(data) {
            return None;
        }
        let r = Rec::new(1, data);
        let size = r.u32(4) as usize;
        if size < 88 || size > data.len() {
            return None;
        }
        let rect = |o: usize| [r.i32(o), r.i32(o + 4), r.i32(o + 8), r.i32(o + 12)];
        Some(Emf {
            data,
            bounds: rect(8),
            frame: rect(24),
            device: (r.i32(72), r.i32(76)),
            millimeters: (r.i32(80), r.i32(84)),
            micrometers: if size >= 108 { (r.i32(100), r.i32(104)) } else { (0, 0) },
        })
    }

    /// Device pixels per millimetre on the device the picture was made for.
    pub fn px_per_mm(&self) -> (f64, f64) {
        // Millimetres are always filled in; micrometers refine them when
        // they agree (some writers leave nonsense there).
        let per = |px: i32, mm: i32, um: i32| {
            let mm = if um > 0 && mm > 0 && (um as f64 / 1000.0 - mm as f64).abs() <= 1.0 {
                um as f64 / 1000.0
            } else {
                mm as f64
            };
            let v = if mm > 0.0 { px as f64 / mm } else { 0.0 };
            if v.is_finite() && v > 0.01 && v < 1000.0 { v } else { crate::wmf::PX_PER_MM }
        };
        (
            per(self.device.0, self.millimeters.0, self.micrometers.0),
            per(self.device.1, self.millimeters.1, self.micrometers.1),
        )
    }

    /// The records, starting with the header.
    fn records(&self) -> impl Iterator<Item = Rec<'a>> {
        let data = self.data;
        let mut at = 0usize;
        let mut done = false;
        std::iter::from_fn(move || {
            if done {
                return None;
            }
            let ty = u32::from_le_bytes(data.get(at..at + 4)?.try_into().ok()?);
            let size = u32::from_le_bytes(data.get(at + 4..at + 8)?.try_into().ok()?) as usize;
            if size < 8 || at.checked_add(size)? > data.len() {
                return None;
            }
            let rec = Rec::new(if ty < 0x1_0000 { ty as u16 } else { 0xFFFF }, &data[at..at + size]);
            at += size;
            if ty == 14 {
                done = true;
            }
            Some(rec)
        })
    }

    /// The frame in device pixels, from the header's millimetre frame (or its
    /// device-unit bounds when the frame is empty).
    pub fn plan(&self) -> Option<Plan> {
        let (mx, my) = self.px_per_mm();
        let f = self.frame;
        let (fw, fh) = ((f[2] as i64 - f[0] as i64) as f64, (f[3] as i64 - f[1] as i64) as f64);
        if fw > 0.0 && fh > 0.0 {
            let (l, t, r, b) = (
                f[0] as f64 / 100.0 * mx,
                f[1] as f64 / 100.0 * my,
                f[2] as f64 / 100.0 * mx,
                f[3] as f64 / 100.0 * my,
            );
            return Some(Plan { dev_frame: [l, t, r, b], aspect: fw / fh });
        }
        let b = self.bounds;
        let (bw, bh) = ((b[2] as i64 - b[0] as i64 + 1) as f64, (b[3] as i64 - b[1] as i64 + 1) as f64);
        if bw > 0.0 && bh > 0.0 && b[2] >= b[0] && b[3] >= b[1] {
            return Some(Plan {
                dev_frame: [b[0] as f64, b[1] as f64, b[2] as f64 + 1.0, b[3] as f64 + 1.0],
                aspect: bw / bh,
            });
        }
        None
    }

    /// Plays the EMF+ records without drawing, to see whether any is left
    /// out: if so the GDI records of a dual file are the safer choice.
    fn plus_is_complete(&self, g: &Gdi) -> bool {
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(250);
        let mut scratch = Gdi::new(crate::canvas::Canvas::measuring(), Affine::IDENTITY, g.px_per_mm, deadline);
        let mut plus = emfplus::Plus::new();
        for rec in self.records().skip(1) {
            if rec.func == 70 && emfplus::is_plus(&rec) {
                emfplus::play_comment(&mut scratch, &mut plus, &rec, 0);
            }
            if scratch.timed_out {
                return false;
            }
        }
        scratch.warnings.skipped.keys().all(|k| !k.starts_with("EMF+"))
    }

    pub fn play(&self, g: &mut Gdi) {
        // Files with EMF+ records are either "dual" (the GDI records draw the
        // same picture for readers that don't know EMF+) or "EMF+ only", whose
        // GDI records draw nothing.  The header's flag isn't reliable (some
        // writers set it on files with no GDI drawing at all), so a file is
        // played from its EMF+ records when it says so or when no GDI
        // drawing record stands outside an `EmfPlusGetDC` section.
        let (mut has_plus, mut flag, mut outside, mut in_dc) = (false, None, 0usize, false);
        for rec in self.records() {
            if rec.func == 70 && emfplus::is_plus(&rec) {
                has_plus = true;
                let (dual, get_dc) = emfplus::comment_info(&rec);
                flag = flag.or(dual);
                in_dc = get_dc;
            } else if is_drawing(rec.func) && !in_dc {
                outside += 1;
            }
        }
        let mut plus_only = has_plus && (flag == Some(false) || outside == 0);
        // A dual file's EMF+ records are the richer version (transparency,
        // gradients) when everything in them can be played.
        if has_plus && !plus_only {
            plus_only = self.plus_is_complete(g);
        }
        let mut plus = emfplus::Plus::new();
        for rec in self.records().skip(1) {
            if g.out_of_time() {
                break;
            }
            if crate::trace() {
                eprintln!("EMF record {} (plus_only {plus_only}, get_dc {})", rec.func, plus.get_dc);
            }
            if rec.func == 70 && emfplus::is_plus(&rec) {
                if plus_only {
                    emfplus::play_comment(g, &mut plus, &rec, 0);
                }
                continue;
            }
            if plus_only && !plus.get_dc && is_drawing(rec.func) {
                continue;
            }
            play_record(g, &rec);
        }
    }
}

/// The records that put marks on the page.
fn is_drawing(ty: u16) -> bool {
    matches!(ty, 2..=8 | 15 | 41..=47 | 53..=56 | 62..=64 | 71..=74 | 76..=92 | 96 | 97 | 108 | 114 | 116 | 118)
}

fn pt(r: &Rec, o: usize) -> (f64, f64) {
    (r.i32(o) as f64, r.i32(o + 4) as f64)
}

fn rect(r: &Rec, o: usize) -> [f64; 4] {
    [r.i32(o) as f64, r.i32(o + 4) as f64, r.i32(o + 8) as f64, r.i32(o + 12) as f64]
}

/// The most points one record's list may hold.
const MAX_POINTS: usize = 1 << 20;

/// A list of points: 32-bit (`wide`) or 16-bit.
fn points(r: &Rec, at: usize, count: usize, wide: bool) -> Vec<(f64, f64)> {
    let size = if wide { 8 } else { 4 };
    let n = count.min(MAX_POINTS).min(r.len().saturating_sub(at) / size);
    (0..n)
        .map(|i| if wide { pt(r, at + i * 8) } else { (r.i16(at + i * 4) as f64, r.i16(at + i * 4 + 2) as f64) })
        .collect()
}

fn xform(r: &Rec, o: usize) -> Affine {
    Affine::new(
        r.f32(o) as f64,
        r.f32(o + 4) as f64,
        r.f32(o + 8) as f64,
        r.f32(o + 12) as f64,
        r.f32(o + 16) as f64,
        r.f32(o + 20) as f64,
    )
}

/// Region data (`RGNDATA`): a header and a list of rectangles, in device
/// units.
fn region_data(r: &Rec, at: usize, size: usize) -> Option<Region> {
    if size < 32 {
        return None;
    }
    let count = (r.u32(at + 8) as usize).min(size.saturating_sub(32) / 16).min(1 << 16);
    let rects = (0..count).map(|i| rect(r, at + 32 + i * 16)).collect();
    Some(Region { rects, device: true })
}

/// The DIB parts of a bitmap record: header and pixel bytes.
fn dib_parts<'a>(r: &Rec<'a>, off_bmi: usize, cb_bmi: usize, off_bits: usize, cb_bits: usize) -> (&'a [u8], &'a [u8]) {
    (r.bytes(off_bmi, cb_bmi), r.bytes(off_bits, cb_bits))
}

fn decode_bitmap(g: &Gdi, r: &Rec, o: usize, usage: u32) -> Option<Bitmap> {
    let (off_bmi, cb_bmi, off_bits, cb_bits) =
        (r.u32(o) as usize, r.u32(o + 4) as usize, r.u32(o + 8) as usize, r.u32(o + 12) as usize);
    if cb_bmi == 0 {
        return None;
    }
    let (bmi, bits) = dib_parts(r, off_bmi, cb_bmi, off_bits, cb_bits);
    dib::decode(bmi, bits, usage, &g.dc.palette)
}

fn play_record(g: &mut Gdi, r: &Rec) {
    let wide = |t: u16| matches!(t, 2..=6);
    match r.func {
        2..=6 | 85..=89 => {
            let is_wide = wide(r.func);
            let pts = points(r, 28, r.u32(24) as usize, is_wide);
            let kind = if is_wide { r.func } else { r.func - 83 };
            match kind {
                2 => g.bezier(&pts, false),
                3 => g.poly(&pts, true, true, true, false),
                4 => g.poly(&pts, false, false, true, false),
                5 => g.bezier(&pts, true),
                _ => g.polyline_to(&pts),
            }
        }
        7 | 8 | 90 | 91 => {
            let is_wide = r.func <= 8;
            let n = (r.u32(24) as usize).min(r.len() / 4);
            let total = r.u32(28) as usize;
            let counts: Vec<usize> = (0..n).map(|i| r.u32(32 + i * 4) as usize).collect();
            let mut at = 32 + n * 4;
            let stride = if is_wide { 8 } else { 4 };
            let mut polys = Vec::new();
            let mut used = 0usize;
            for c in counts {
                used = used.saturating_add(c);
                if used > total.max(MAX_POINTS) {
                    break;
                }
                polys.push(points(r, at, c, is_wide));
                at = at.saturating_add(c.saturating_mul(stride));
            }
            g.poly_poly(&polys, matches!(r.func, 8 | 91));
        }
        9 => g.set_window_ext(r.i32(8) as f64, r.i32(12) as f64),
        10 => g.set_window_org(r.i32(8) as f64, r.i32(12) as f64),
        11 => g.set_viewport_ext(r.i32(8) as f64, r.i32(12) as f64),
        12 => g.set_viewport_org(r.i32(8) as f64, r.i32(12) as f64),
        13 => g.dc.brush_org = pt(r, 8),
        15 => {
            let c = g.color(r.u32(16));
            let (x, y) = pt(r, 8);
            let old = std::mem::replace(&mut g.dc.brush, Rc::new(Brush::Solid(c)));
            g.pat_blt(x, y, 1.0, 1.0, 0x00F0_0021);
            g.dc.brush = old;
        }
        17 => g.set_map_mode(r.u32(8)),
        18 => g.dc.bk_opaque = r.u32(8) == 2,
        19 => g.dc.winding = r.u32(8) == 2,
        20 => g.dc.rop2 = r.u32(8),
        21 => g.dc.stretch_mode = r.u32(8),
        22 => g.dc.text_align = r.u32(8),
        24 => g.dc.text_color = g.color(r.u32(8)),
        25 => g.dc.bk_color = g.color(r.u32(8)),
        26 => g.offset_clip(r.i32(8) as f64, r.i32(12) as f64),
        27 => g.move_to(r.i32(8) as f64, r.i32(12) as f64),
        28 => g.reset_clip(),
        29 => {
            let b = rect(r, 8);
            g.clip_rect(b[0], b[1], b[2], b[3], 4);
        }
        30 => {
            let b = rect(r, 8);
            g.clip_rect(b[0], b[1], b[2], b[3], 1);
        }
        31 => g.scale_viewport_ext(r.i32(8) as f64, r.i32(12) as f64, r.i32(16) as f64, r.i32(20) as f64),
        32 => g.scale_window_ext(r.i32(8) as f64, r.i32(12) as f64, r.i32(16) as f64, r.i32(20) as f64),
        33 => g.save(),
        34 => g.restore(r.i32(8)),
        35 => g.set_world(xform(r, 8)),
        36 => g.modify_world(xform(r, 8), r.u32(32)),
        37 => {
            let ih = r.u32(8);
            let obj = if ih & 0x8000_0000 != 0 { g.stock(ih & 0x7FFF_FFFF) } else { g.object(ih).cloned() };
            if let Some(o) = obj {
                g.select(o);
            }
        }
        38 => {
            let color = g.color(r.u32(24));
            let pen = make::pen(r.u32(12), r.i32(16) as f64, color, Vec::new());
            g.set_object(r.u32(8), Object::Pen(Rc::new(pen)));
        }
        39 => {
            let color = g.color(r.u32(16));
            let brush = make::simple_brush(r.u32(12), color, r.u32(20));
            g.set_object(r.u32(8), Object::Brush(Rc::new(brush)));
        }
        40 => g.delete_object(r.u32(8)),
        41 => g.angle_arc(pt(r, 8), r.u32(16) as f64, r.f32(20) as f64, r.f32(24) as f64),
        42 => {
            let b = rect(r, 8);
            g.ellipse(b[0], b[1], b[2], b[3]);
        }
        43 => {
            let b = rect(r, 8);
            g.rectangle(b[0], b[1], b[2], b[3]);
        }
        44 => {
            let b = rect(r, 8);
            g.round_rect(b[0], b[1], b[2], b[3], r.i32(24) as f64, r.i32(28) as f64);
        }
        45..=47 => {
            let kind = match r.func {
                45 => ArcKind::Arc,
                46 => ArcKind::Chord,
                _ => ArcKind::Pie,
            };
            g.arc_shape(kind, rect(r, 8), pt(r, 24), pt(r, 32));
        }
        55 => g.arc_to(rect(r, 8), pt(r, 24), pt(r, 32)),
        48 => {
            if let Some(Object::Palette(p)) = g.object(r.u32(8)).cloned() {
                g.dc.palette = p;
            } else if r.u32(8) == 0x8000_000F {
                g.dc.palette = Rc::new(SYSTEM_PALETTE.to_vec());
            }
        }
        49 => {
            let n = (r.u16(14) as usize).min(r.len() / 4);
            let pal: Vec<Rgb> = (0..n).map(|i| [r.u8(16 + i * 4), r.u8(17 + i * 4), r.u8(18 + i * 4)]).collect();
            g.set_object(r.u32(8), Object::Palette(Rc::new(pal)));
        }
        50..=52 => {}
        54 => g.line_to(r.i32(8) as f64, r.i32(12) as f64),
        56 | 92 => {
            let wide = r.func == 56;
            let n = (r.u32(24) as usize).min(MAX_POINTS);
            let pts = points(r, 28, n, wide);
            let types = r.tail(28 + pts.len() * if wide { 8 } else { 4 });
            poly_draw(g, &pts, types);
        }
        57 => g.dc.arc_ccw = r.u32(8) != 2,
        58 => {
            let v = r.f32(8);
            g.dc.miter_limit = if v >= 1.0 { v } else { r.u32(8) as f32 };
        }
        59 => g.begin_path(),
        60 => g.end_path(),
        61 => g.close_figure(),
        62 => g.draw_last_path(true, false),
        63 => g.draw_last_path(true, true),
        64 => g.draw_last_path(false, true),
        65 => {}
        66 => g.widen_path(),
        67 => g.clip_last_path(match r.u32(8) {
            m @ 1..=5 => m,
            _ => 1,
        }),
        68 => g.abort_path(),
        70 => comment(g, r),
        71..=74 => region_fill(g, r),
        75 => {
            let (cb, mode) = (r.u32(8) as usize, r.u32(12));
            if cb == 0 {
                g.reset_clip();
            } else if let Some(reg) = region_data(r, 16, cb) {
                g.clip_region(&reg, if (1..=5).contains(&mode) { mode } else { 5 });
            }
        }
        76 | 77 => bit_blt(g, r, r.func == 77),
        78 => {
            // MaskBlt: the mask decides which pixels are copied.
            if let Some(mut bmp) = decode_bitmap(g, r, 84, r.u32(80)) {
                if let Some(mask) = decode_bitmap(g, r, 112, r.u32(108)) {
                    apply_mask(&mut bmp, &mask);
                }
                g.draw_bitmap(
                    &bmp,
                    [r.i32(44) as i64, r.i32(48) as i64, r.i32(32) as i64, r.i32(36) as i64],
                    [r.i32(24) as f64, r.i32(28) as f64, r.i32(32) as f64, r.i32(36) as f64],
                    0x00CC_0020,
                    None,
                );
            }
        }
        79 => g.warnings.skip("PlgBlt"),
        80 => {
            if let Some(mut bmp) = decode_bitmap(g, r, 48, r.u32(64)) {
                bmp.make_opaque();
                let (w, h) = (r.i32(40) as i64, r.i32(44) as i64);
                let src = [r.i32(32) as i64, r.i32(36) as i64, w, h];
                g.draw_bitmap(&bmp, src, [r.i32(24) as f64, r.i32(28) as f64, w as f64, h as f64], 0x00CC_0020, None);
            }
        }
        81 => {
            if let Some(mut bmp) = decode_bitmap(g, r, 48, r.u32(64)) {
                bmp.make_opaque();
                let src = [r.i32(32) as i64, r.i32(36) as i64, r.i32(40) as i64, r.i32(44) as i64];
                let dst = [r.i32(24) as f64, r.i32(28) as f64, r.i32(72) as f64, r.i32(76) as f64];
                g.draw_bitmap(&bmp, src, dst, r.u32(68), None);
            }
        }
        82 => {
            let units: Vec<u16> = (0..32).map(|i| r.u16(40 + i * 2)).take_while(|u| *u != 0).collect();
            let face: String = char::decode_utf16(units).map(|c| c.unwrap_or('?')).collect();
            let f =
                make::font(r.i32(12), r.i32(20), r.i32(28), [r.u8(32), r.u8(33), r.u8(34)], r.u8(35), r.u8(39), face);
            g.set_object(r.u32(8), Object::Font(Rc::new(f)));
        }
        83 | 84 => ext_text_out(g, r),
        108 => small_text_out(g, r),
        93 | 94 => {
            let usage = r.u32(12);
            let brush = match decode_bitmap_at(g, r, 16, usage) {
                Some(b) if r.func == 93 || b.mono && b.width <= 64 && b.height <= 64 => {
                    let on = b.rgba.chunks_exact(4).map(|p| p[0] > 127).collect();
                    Brush::Mono(Rc::new(MonoTile { width: b.width as usize, height: b.height as usize, on }))
                }
                Some(b) => g.pattern_pixmap(&b).map_or(Brush::Solid([128, 128, 128]), |p| Brush::Pattern(Rc::new(p))),
                None => Brush::Solid([128, 128, 128]),
            };
            g.set_object(r.u32(8), Object::Brush(Rc::new(brush)));
        }
        95 => ext_create_pen(g, r),
        96 | 97 => g.warnings.skip("PolyTextOut"),
        98 | 100 | 101 | 99 | 122 | 104 | 112 | 113 | 111 | 23 | 16 | 105 | 106 | 110 | 109 | 119 | 121 => {}
        114 => alpha_blend(g, r),
        115 => g.dc.rtl = r.u32(8) & 1 != 0,
        116 => transparent_blt(g, r),
        118 => gradient_fill(g, r),
        120 => {
            g.dc.break_extra = r.i32(8) as f64;
            g.dc.break_count = r.i32(12) as f64;
        }
        14 => {}
        0xFFFF => g.warnings.skip("EMF record of unknown type"),
        53 => g.warnings.skip("flood fill"),
        f => g.warnings.skip(format!("EMF record {f}")),
    }
}

fn decode_bitmap_at(g: &Gdi, r: &Rec, o: usize, usage: u32) -> Option<Bitmap> {
    decode_bitmap(g, r, o, usage)
}

/// Pixels where the mask is white are left transparent (a mask bitmap's
/// black pixels select the source).
fn apply_mask(bmp: &mut Bitmap, mask: &Bitmap) {
    for y in 0..bmp.height as i64 {
        for x in 0..bmp.width as i64 {
            let m = mask.pixel(x * mask.width as i64 / bmp.width as i64, y * mask.height as i64 / bmp.height as i64);
            if m[0] > 127 {
                let i = (y as usize * bmp.width as usize + x as usize) * 4;
                bmp.rgba[i + 3] = 0;
            }
        }
    }
    bmp.has_alpha = true;
}

fn bit_blt(g: &mut Gdi, r: &Rec, stretch: bool) {
    let (x, y, w, h, rop) = (r.i32(24) as f64, r.i32(28) as f64, r.i32(32) as f64, r.i32(36) as f64, r.u32(40));
    if r.u32(88) == 0 {
        return g.pat_blt(x, y, w, h, rop);
    }
    let Some(mut bmp) = decode_bitmap(g, r, 84, r.u32(80)) else { return };
    bmp.make_opaque();
    let (sw, sh) = if stretch { (r.i32(100) as i64, r.i32(104) as i64) } else { (w as i64, h as i64) };
    g.draw_bitmap(&bmp, [r.i32(44) as i64, r.i32(48) as i64, sw, sh], [x, y, w, h], rop, None);
}

fn alpha_blend(g: &mut Gdi, r: &Rec) {
    let Some(mut bmp) = decode_bitmap(g, r, 84, r.u32(80)) else { return };
    let constant = r.u8(42) as f32 / 255.0;
    let per_pixel = r.u8(43) & 1 != 0 && bmp.has_alpha;
    if per_pixel {
        // The bitmap's colours are premultiplied by its alpha.
        for p in bmp.rgba.chunks_exact_mut(4) {
            let a = p[3] as u32;
            if a > 0 && a < 255 {
                for c in p.iter_mut().take(3) {
                    *c = ((*c as u32 * 255 + a / 2) / a).min(255) as u8;
                }
            }
        }
    } else {
        bmp.make_opaque();
    }
    let src = [r.i32(44) as i64, r.i32(48) as i64, r.i32(100) as i64, r.i32(104) as i64];
    let dst = [r.i32(24) as f64, r.i32(28) as f64, r.i32(32) as f64, r.i32(36) as f64];
    g.draw_bitmap(&bmp, src, dst, 0x00CC_0020, Some(constant));
}

fn transparent_blt(g: &mut Gdi, r: &Rec) {
    let Some(mut bmp) = decode_bitmap(g, r, 84, r.u32(80)) else { return };
    let key = g.color(r.u32(40));
    bmp.make_opaque();
    for p in bmp.rgba.chunks_exact_mut(4) {
        if p[0] == key[0] && p[1] == key[1] && p[2] == key[2] {
            p[3] = 0;
        }
    }
    bmp.has_alpha = true;
    let src = [r.i32(44) as i64, r.i32(48) as i64, r.i32(100) as i64, r.i32(104) as i64];
    let dst = [r.i32(24) as f64, r.i32(28) as f64, r.i32(32) as f64, r.i32(36) as f64];
    g.draw_bitmap(&bmp, src, dst, 0x00CC_0020, None);
}

fn gradient_fill(g: &mut Gdi, r: &Rec) {
    let (nv, nt, mode) = (r.u32(24) as usize, r.u32(28) as usize, r.u32(32));
    let nv = nv.min(r.len().saturating_sub(36) / 16).min(1 << 16);
    let vert = |i: usize| {
        let o = 36 + i * 16;
        (
            (r.i32(o) as f64, r.i32(o + 4) as f64),
            [(r.u16(o + 8) >> 8) as u8, (r.u16(o + 10) >> 8) as u8, (r.u16(o + 12) >> 8) as u8],
        )
    };
    let idx_at = 36 + nv * 16;
    match mode {
        0 | 1 => {
            for k in 0..nt.min(1 << 12) {
                let (a, b) = (r.u32(idx_at + k * 8) as usize, r.u32(idx_at + k * 8 + 4) as usize);
                if a >= nv || b >= nv {
                    continue;
                }
                let (va, vb) = (vert(a), vert(b));
                g.gradient_rect(va.0, va.1, vb.0, vb.1, mode == 0);
            }
        }
        2 => {
            for k in 0..nt.min(1 << 14) {
                let ix = [
                    r.u32(idx_at + k * 12) as usize,
                    r.u32(idx_at + k * 12 + 4) as usize,
                    r.u32(idx_at + k * 12 + 8) as usize,
                ];
                if ix.iter().any(|i| *i >= nv) {
                    continue;
                }
                let v = [vert(ix[0]), vert(ix[1]), vert(ix[2])];
                g.gradient_triangle(v);
            }
        }
        _ => g.warnings.skip("gradient fill"),
    }
}

fn poly_draw(g: &mut Gdi, pts: &[(f64, f64)], types: &[u8]) {
    let mut i = 0;
    let mut sub: Vec<(f64, f64)> = Vec::new();
    let mut curve: Vec<(f64, f64)> = Vec::new();
    let flush = |g: &mut Gdi, sub: &mut Vec<(f64, f64)>, close: bool| {
        if sub.len() >= 2 {
            g.poly(sub, close, false, true, false);
        }
        sub.clear();
    };
    while i < pts.len() && i < types.len() {
        let t = types[i] & !1;
        let closing = types[i] & 1 != 0;
        match t {
            6 => {
                flush(g, &mut sub, false);
                sub.push(pts[i]);
                g.dc.cur = pts[i];
            }
            2 => {
                if sub.is_empty() {
                    sub.push(g.dc.cur);
                }
                sub.push(pts[i]);
                g.dc.cur = pts[i];
            }
            4 => {
                curve.push(pts[i]);
                if curve.len() == 3 {
                    flush(g, &mut sub, false);
                    let mut all = vec![g.dc.cur];
                    all.extend_from_slice(&curve);
                    g.bezier(&all, false);
                    g.dc.cur = curve[2];
                    sub.push(g.dc.cur);
                    curve.clear();
                }
            }
            _ => {}
        }
        if closing {
            flush(g, &mut sub, true);
        }
        i += 1;
    }
    flush(g, &mut sub, false);
}

fn ext_create_pen(g: &mut Gdi, r: &Rec) {
    let style = r.u32(28);
    let width = r.u32(32) as f64;
    let color = g.color(r.u32(40));
    let n = (r.u32(48) as usize).min(r.len().saturating_sub(52) / 4).min(64);
    let geometric = style & 0xF_0000 == 0x1_0000;
    let dashes: Vec<f32> = (0..n)
        .map(|i| {
            let v = r.u32(52 + i * 4) as f32;
            if geometric && width > 0.0 { v / width as f32 } else { v }
        })
        .collect();
    let pen = make::pen(style, width, color, dashes);
    g.set_object(r.u32(8), Object::Pen(Rc::new(pen)));
}

fn ext_text_out(g: &mut Gdi, r: &Rec) {
    let (pos, n, off, opts) = (pt(r, 36), (r.u32(44) as usize).min(1 << 16), r.u32(48) as usize, r.u32(52));
    let rc = rect(r, 56);
    let off_dx = r.u32(72) as usize;
    // Glyph indices (ETO_GLYPH_INDEX) mean nothing without the original font.
    if opts & 0x10 != 0 {
        g.warnings.skip("text drawn by glyph index");
        return;
    }
    let text = if r.func == 84 {
        let units: Vec<u16> = (0..n).map(|i| r.u16(off + i * 2)).collect();
        make::decode_utf16(&units)
    } else {
        make::decode_ansi(r.bytes(off, n), g.dc.font.charset)
    };
    // `ETO_PDY` doubles the array: x and y advances for each character.
    let pdy = opts & 0x2000 != 0;
    let dx: Vec<f64> = if off_dx != 0 {
        let step = if pdy { 2 } else { 1 };
        (0..n).map(|i| r.i32(off_dx + i * 4 * step) as f64).collect()
    } else {
        Vec::new()
    };
    // A UTF-16 string with surrogate pairs has fewer characters than
    // advances; the advances stay with their code units, so drop extras.
    let dx = if dx.len() != text.len() && r.func == 84 { surrogate_dx(r, off, n, &dx) } else { dx };
    let has_rect = opts & 0x100 == 0;
    g.text_out(pos, &text, &dx, opts, if has_rect { Some(rc) } else { None });
}

/// Keeps the advance of the first unit of each surrogate pair and adds the
/// second's to it.
fn surrogate_dx(r: &Rec, off: usize, n: usize, dx: &[f64]) -> Vec<f64> {
    let mut out = Vec::with_capacity(dx.len());
    let mut i = 0;
    while i < n && i < dx.len() {
        let u = r.u16(off + i * 2);
        if (0xD800..0xDC00).contains(&u) && i + 1 < dx.len() {
            out.push(dx[i] + dx[i + 1]);
            i += 2;
        } else {
            out.push(dx[i]);
            i += 1;
        }
    }
    out
}

fn small_text_out(g: &mut Gdi, r: &Rec) {
    let (x, y, n, opts) = (r.i32(8) as f64, r.i32(12) as f64, (r.u32(16) as usize).min(1 << 16), r.u32(20));
    let mut at = 36;
    if opts & 0x100 == 0 {
        at += 16;
    }
    let text = if opts & 0x200 != 0 {
        make::decode_ansi(r.bytes(at, n), g.dc.font.charset)
    } else {
        make::decode_utf16(&(0..n).map(|i| r.u16(at + i * 2)).collect::<Vec<_>>())
    };
    // The reference point is scaled by the record's own scales; ignore them.
    g.text_out((x, y), &text, &[], opts, None);
}

fn region_fill(g: &mut Gdi, r: &Rec) {
    let cb = r.u32(24) as usize;
    let Some(reg) = region_data(
        r,
        if r.func == 71 {
            32
        } else if r.func == 72 {
            40
        } else {
            28
        },
        cb,
    ) else {
        return;
    };
    let Some(path) = g.region_path(&reg) else { return };
    match r.func {
        71 => {
            let ih = r.u32(28);
            let brush = if ih & 0x8000_0000 != 0 { g.stock(ih & 0x7FFF_FFFF) } else { g.object(ih).cloned() };
            if let Some(Object::Brush(b)) = brush {
                let old = std::mem::replace(&mut g.dc.brush, b);
                g.fill_with_brush(&path, FillRule::Winding);
                g.dc.brush = old;
            }
        }
        72 => {
            // FrameRgn: an outline drawn with the brush, `w` by `h` thick.
            let ih = r.u32(28);
            let brush = if ih & 0x8000_0000 != 0 { g.stock(ih & 0x7FFF_FFFF) } else { g.object(ih).cloned() };
            if let Some(Object::Brush(b)) = brush
                && let Brush::Solid(color) = &*b
            {
                let mut pen = Pen::solid(*color);
                pen.width = (r.i32(32).abs() as f64 + r.i32(36).abs() as f64) / 2.0;
                pen.geometric = true;
                let old = std::mem::replace(&mut g.dc.pen, Rc::new(pen));
                g.stroke_with_pen(&path);
                g.dc.pen = old;
            }
        }
        73 => {
            // InvertRgn: the page's colours inverted inside the region.
            let old = (std::mem::replace(&mut g.dc.brush, Rc::new(Brush::Solid([255, 255, 255]))), g.dc.rop2);
            g.dc.rop2 = 6;
            g.fill_with_brush(&path, FillRule::Winding);
            (g.dc.brush, g.dc.rop2) = old;
        }
        74 => g.fill_with_brush(&path, FillRule::Winding),
        _ => {}
    }
}

/// `EMR_COMMENT`: public comments and EMF+ records are noted, not played.
fn comment(g: &mut Gdi, r: &Rec) {
    // EMF+ records are handled by the loop in `play`.
    let _ = (g, r);
}
