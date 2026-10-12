//! Macintosh PICT, the drawing format of QuickDraw: versions 1 and 2, with
//! lines, rectangles, rounded rectangles, ovals, arcs, polygons, regions,
//! text, patterns, and bitmaps (packed, indexed and direct).  Images
//! compressed with QuickTime are drawn only when a PNG or JPEG can be found
//! inside; other QuickTime codecs are skipped.
//!
//! A PICT file starts with 512 bytes of application data (often left out of
//! files that were exported rather than saved), then a size, the picture
//! frame in points and the opcodes.  Version 1 opcodes are one byte; version
//! 2 pads to even offsets and uses two-byte opcodes.

use crate::affine::Affine;
use crate::canvas::Canvas;
use crate::dib::Bitmap;
use crate::gdi::{Gdi, Region};
use crate::shapes::Geo;
use crate::state::*;
use crate::{Drawn, LoadError, Rgb};
use std::rc::Rc;
use std::time::Instant;
use tiny_skia::LineCap;

/// Is there a PICT at the start of `head`?  Version 2 has a distinctive
/// version opcode; version 1 only a one-byte opcode pair, so it also needs a
/// PICT-like file name.
pub fn looks_like_pict(head: &[u8], named: bool) -> bool {
    [0usize, 512].iter().any(|&o| {
        let Some(h) = head.get(o..o + 14) else { return false };
        let frame_ok = {
            let (t, l, b, r) = (
                i16::from_be_bytes([h[2], h[3]]),
                i16::from_be_bytes([h[4], h[5]]),
                i16::from_be_bytes([h[6], h[7]]),
                i16::from_be_bytes([h[8], h[9]]),
            );
            b > t && r > l
        };
        let v2 = h[10..14] == [0x00, 0x11, 0x02, 0xFF];
        let v1 = h[10..12] == [0x11, 0x01];
        frame_ok && (v2 || (v1 && named))
    })
}

/// A big-endian reader over the picture's bytes that never panics.
struct Cur<'a> {
    d: &'a [u8],
    p: usize,
}

impl<'a> Cur<'a> {
    fn u8(&mut self) -> u8 {
        let v = self.d.get(self.p).copied().unwrap_or(0);
        self.p += 1;
        v
    }
    fn i8(&mut self) -> i8 {
        self.u8() as i8
    }
    fn u16(&mut self) -> u16 {
        let v = self.d.get(self.p..self.p + 2).map_or(0, |b| u16::from_be_bytes([b[0], b[1]]));
        self.p += 2;
        v
    }
    fn i16(&mut self) -> i16 {
        self.u16() as i16
    }
    fn u32(&mut self) -> u32 {
        let v = self.d.get(self.p..self.p + 4).map_or(0, |b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]));
        self.p += 4;
        v
    }
    fn skip(&mut self, n: usize) {
        self.p = self.p.saturating_add(n);
    }
    fn at_end(&self) -> bool {
        self.p >= self.d.len()
    }
    /// A rectangle as top, left, bottom, right, returned as left, top,
    /// right, bottom.
    fn rect(&mut self) -> [i32; 4] {
        let (t, l, b, r) = (self.i16() as i32, self.i16() as i32, self.i16() as i32, self.i16() as i32);
        [l, t, r, b]
    }
    /// A point stored vertical first, returned as x, y.
    fn point(&mut self) -> (i32, i32) {
        let (v, h) = (self.i16() as i32, self.i16() as i32);
        (h, v)
    }
    fn bytes(&mut self, n: usize) -> &'a [u8] {
        let start = self.p.min(self.d.len());
        let end = self.p.saturating_add(n).min(self.d.len());
        self.p = self.p.saturating_add(n);
        &self.d[start..end]
    }
}

/// QuickDraw's drawing state.
struct Qd {
    pen_size: (i32, i32),
    pen_pat: [u8; 8],
    fill_pat: [u8; 8],
    bk_pat: [u8; 8],
    fg: Rgb,
    bk: Rgb,
    pen_loc: (i32, i32),
    text_size: i32,
    text_face: u8,
    text_font: u16,
    font_name: String,
    last_rect: [i32; 4],
    last_poly: Vec<(f64, f64)>,
    last_region: Vec<[f64; 4]>,
    last_arc: (i32, i32),
}

const BLACK: [u8; 8] = [0xFF; 8];
const WHITE: [u8; 8] = [0; 8];

/// Opcode data sizes for the opcodes that have a fixed one.
fn fixed_size(op: u16) -> Option<usize> {
    Some(match op {
        0x00 => 0,
        0x03 | 0x05 | 0x0D | 0x15 | 0x16 => 2,
        0x04 => 1,
        0x06 | 0x07 | 0x0B | 0x0C | 0x0E | 0x0F | 0x21 => 4,
        0x08 => 2,
        0x09 | 0x0A | 0x02 => 8,
        0x10 => 8,
        0x1A | 0x1B | 0x1D | 0x1F => 6,
        0x1C | 0x1E => 0,
        0x20 => 8,
        0x22 => 6,
        0x23 => 2,
        0x2D => 10,
        0x2E => 6,
        0x30..=0x37 | 0x40..=0x47 | 0x50..=0x57 => 8,
        0x38..=0x3F | 0x48..=0x4F | 0x58..=0x5F | 0x7D..=0x7F | 0x8D..=0x8F => 0,
        0x60..=0x67 => 12,
        0x68..=0x6F => 4,
        0xA0 => 2,
        _ => return None,
    })
}

const MAC_ROMAN: [char; 128] = [
    'Ä', 'Å', 'Ç', 'É', 'Ñ', 'Ö', 'Ü', 'á', 'à', 'â', 'ä', 'ã', 'å', 'ç', 'é', 'è', 'ê', 'ë', 'í', 'ì', 'î', 'ï', 'ñ',
    'ó', 'ò', 'ô', 'ö', 'õ', 'ú', 'ù', 'û', 'ü', '†', '°', '¢', '£', '§', '•', '¶', 'ß', '®', '©', '™', '´', '¨', '≠',
    'Æ', 'Ø', '∞', '±', '≤', '≥', '¥', 'µ', '∂', '∑', '∏', 'π', '∫', 'ª', 'º', 'Ω', 'æ', 'ø', '¿', '¡', '¬', '√', 'ƒ',
    '≈', '∆', '«', '»', '…', '\u{A0}', 'À', 'Ã', 'Õ', 'Œ', 'œ', '–', '—', '“', '”', '‘', '’', '÷', '◊', 'ÿ', 'Ÿ', '⁄',
    '€', '‹', '›', 'ﬁ', 'ﬂ', '‡', '·', '‚', '„', '‰', 'Â', 'Ê', 'Á', 'Ë', 'È', 'Í', 'Î', 'Ï', 'Ì', 'Ó', 'Ô',
    '\u{F8FF}', 'Ò', 'Ú', 'Û', 'Ù', 'ı', 'ˆ', '˜', '¯', '˘', '˙', '˚', '¸', '˝', '˛', 'ˇ',
];

fn mac_roman(bytes: &[u8]) -> Vec<char> {
    bytes.iter().map(|&b| if b < 0x80 { b as char } else { MAC_ROMAN[(b - 0x80) as usize] }).collect()
}

/// QuickDraw's eight standard colours as `ForeColor` constants.
fn color_code(c: u32) -> Rgb {
    match c {
        30 => [255, 255, 255],
        33 => [0, 0, 0],
        69 => [255, 255, 0],
        137 => [255, 0, 255],
        205 => [255, 0, 0],
        273 => [0, 255, 255],
        341 => [0, 255, 0],
        409 => [0, 0, 255],
        _ => [0, 0, 0],
    }
}

pub fn render(data: &[u8], side: Option<u32>, deadline: Instant) -> Result<Drawn, LoadError> {
    // A picture at offset 0 wins; otherwise the 512-byte header comes first.
    let marks =
        |o: usize| data.get(o + 10..o + 14).is_some_and(|m| m == [0x00, 0x11, 0x02, 0xFF] || m[..2] == [0x11, 0x01]);
    let start = if marks(0) { 0 } else { 512 };
    let pic = data.get(start..).ok_or_else(|| LoadError::invalid("not a PICT"))?;
    let mut c = Cur { d: pic, p: 2 };
    let mut frame = c.rect();
    // An extended version 2 header (opcode 0x0C00, header version -2)
    // repeats the frame as a source rectangle in the picture's own
    // coordinates, which some writers leave offset from the header's frame
    // (a frame at the origin, drawing opcodes far away); the opcodes use
    // the source rectangle's coordinates.
    if pic.get(10..18) == Some(&[0x00, 0x11, 0x02, 0xFF, 0x0C, 0x00, 0xFF, 0xFE]) {
        let mut h = Cur { d: pic, p: 28 };
        let src = h.rect();
        if src[2] > src[0] && src[3] > src[1] {
            frame = src;
        }
    }
    let (w, h) = ((frame[2] - frame[0]) as f64, (frame[3] - frame[1]) as f64);
    if !(w > 0.0 && h > 0.0) {
        return Err(LoadError::invalid("the PICT frame is empty"));
    }
    let v2 = pic.get(10..14) == Some(&[0x00, 0x11, 0x02, 0xFF]);
    c.p = 10;
    let (cw, ch) =
        crate::canvas_size(w / h, w.max(h), side).ok_or_else(|| LoadError::invalid("the PICT's size is not usable"))?;
    let base_xf = Affine::scale(cw as f64 / w, ch as f64 / h);
    let canvas = Canvas::new(cw, ch).ok_or_else(|| LoadError::TooLarge(format!("{cw}x{ch}")))?;
    let mut g = Gdi::new(canvas, base_xf, (72.0 / 25.4, 72.0 / 25.4), deadline);
    g.dc.win_org = (frame[0] as f64, frame[1] as f64);
    g.dc.text_align = TA_BASELINE | TA_UPDATECP;
    g.dc.bk_opaque = false;
    g.update_transform();
    let mut qd = Qd {
        pen_size: (1, 1),
        pen_pat: BLACK,
        fill_pat: BLACK,
        bk_pat: WHITE,
        fg: [0, 0, 0],
        bk: [255, 255, 255],
        pen_loc: (0, 0),
        text_size: 12,
        text_face: 0,
        text_font: 0,
        font_name: String::new(),
        last_rect: [0; 4],
        last_poly: Vec::new(),
        last_region: Vec::new(),
        last_arc: (0, 90),
    };
    run(&mut g, &mut c, &mut qd, v2);
    let timed_out = g.timed_out;
    let skipped = g.warnings.skipped.iter().map(|(k, v)| (k.clone(), *v)).collect();
    Ok(Drawn { width: cw, height: ch, rgba: g.cv.into_rgba(None), timed_out, skipped })
}

fn run(g: &mut Gdi, c: &mut Cur, qd: &mut Qd, v2: bool) {
    let mut ops = 0u32;
    while !c.at_end() && ops < 5_000_000 {
        ops += 1;
        if g.out_of_time() {
            break;
        }
        if v2 && c.p % 2 == 1 {
            c.p += 1;
        }
        let op = if v2 { c.u16() } else { c.u8() as u16 };
        if op == 0xFF {
            break;
        }
        if let Some(n) = fixed_size(op) {
            let at = c.p;
            apply_fixed(g, c, qd, op);
            c.p = at + n;
            continue;
        }
        match op {
            0x11 => {
                c.skip(if v2 { 2 } else { 1 });
            }
            0x0C00 => c.skip(24),
            0x01 => {
                let region = read_region(c);
                match region {
                    Some(rects) if !rects.is_empty() => g.clip_region(&Region { rects, device: false }, 5),
                    _ => g.reset_clip(),
                }
            }
            0x12..=0x14 => {
                // Pixel patterns: a pattern type, a pixmap and a colour table;
                // the old 8x8 pattern comes first.
                let kind = c.u16();
                let pat = c.bytes(8);
                if kind == 1 {
                    let _ = (pat, read_pixmap_header(c));
                } else {
                    c.skip(6);
                }
                g.warnings.skip("pixel pattern");
            }
            0x28..=0x2B => text(g, c, qd, op),
            0x2C => {
                let len = c.u16() as usize;
                let end = c.p + len;
                c.skip(2);
                let n = c.u8() as usize;
                qd.font_name = String::from_utf8_lossy(c.bytes(n)).into_owned();
                c.p = end;
            }
            0x75..=0x77 | 0x85..=0x87 => {
                // Reserved polygon and region opcodes: a size and data.
                let n = c.u16() as usize;
                c.skip(n.saturating_sub(2));
            }
            0x70..=0x74 => {
                let _size = c.u16();
                let _bbox = c.rect();
                let n = (_size as usize).saturating_sub(10) / 4;
                let pts: Vec<(f64, f64)> = (0..n)
                    .map(|_| {
                        let p = c.point();
                        (p.0 as f64, p.1 as f64)
                    })
                    .collect();
                qd.last_poly = pts.clone();
                shape_poly(g, qd, (op - 0x70) as u8, &pts);
            }
            0x78..=0x7C => {
                let pts = qd.last_poly.clone();
                shape_poly(g, qd, (op - 0x78) as u8, &pts);
            }
            0x80..=0x84 => {
                let rects = read_region(c).unwrap_or_default();
                qd.last_region = rects.clone();
                shape_region(g, qd, (op - 0x80) as u8, &rects);
            }
            0x88..=0x8C => {
                let rects = qd.last_region.clone();
                shape_region(g, qd, (op - 0x88) as u8, &rects);
            }
            0x90 | 0x91 | 0x98 | 0x99 | 0x9A | 0x9B => bits(g, c, qd, op),
            0xA1 => {
                c.skip(2);
                let n = c.u16() as usize;
                c.skip(n);
            }
            0xA2..=0xAF => {
                let n = c.u16() as usize;
                c.skip(n);
            }
            0xB0..=0xCF => {}
            0xD0..=0xFE => {
                let n = c.u32() as usize;
                c.skip(n);
            }
            0x8200 | 0x8201 => quicktime(g, c, op),
            0x8000..=0x80FF => {}
            0x0100..=0x7FFF => c.skip((op >> 8) as usize * 2),
            0x8100..=0xFFFF => {
                let n = c.u32() as usize;
                c.skip(n);
            }
            _ => {
                // An opcode we know nothing about; its length is unknowable.
                g.warnings.skip(format!("PICT opcode 0x{op:02X}"));
                break;
            }
        }
    }
}

/// Opcodes whose data has a fixed size: state changes and shapes.
fn apply_fixed(g: &mut Gdi, c: &mut Cur, qd: &mut Qd, op: u16) {
    match op {
        0x02 => qd.bk_pat = pat(c),
        0x03 => qd.text_font = c.u16(),
        0x04 => qd.text_face = c.u8(),
        0x07 => {
            let (h, v) = c.point();
            qd.pen_size = (h, v);
        }
        0x09 => qd.pen_pat = pat(c),
        0x0A => qd.fill_pat = pat(c),
        0x0D => qd.text_size = c.u16() as i32,
        0x0E => qd.fg = color_code(c.u32()),
        0x0F => qd.bk = color_code(c.u32()),
        0x1A => {
            qd.fg = rgb16(c);
            g.dc.text_color = qd.fg;
        }
        0x1B => qd.bk = rgb16(c),
        0x20 => {
            let p0 = c.point();
            let p1 = c.point();
            line(g, qd, p0, p1);
        }
        0x21 => {
            let p1 = c.point();
            let p0 = qd.pen_loc;
            line(g, qd, p0, p1);
        }
        0x22 => {
            let p0 = c.point();
            let (dh, dv) = (c.i8() as i32, c.i8() as i32);
            line(g, qd, p0, (p0.0 + dh, p0.1 + dv));
        }
        0x23 => {
            let (dh, dv) = (c.i8() as i32, c.i8() as i32);
            let p0 = qd.pen_loc;
            line(g, qd, p0, (p0.0 + dh, p0.1 + dv));
        }
        0x30..=0x34 | 0x38..=0x3C => {
            if op < 0x38 {
                qd.last_rect = c.rect();
            }
            shape_rect(g, qd, (op & 7) as u8, ShapeKind::Rect);
        }
        0x40..=0x44 | 0x48..=0x4C => {
            if op < 0x48 {
                qd.last_rect = c.rect();
            }
            shape_rect(g, qd, (op & 7) as u8, ShapeKind::Round);
        }
        0x50..=0x54 | 0x58..=0x5C => {
            if op < 0x58 {
                qd.last_rect = c.rect();
            }
            shape_rect(g, qd, (op & 7) as u8, ShapeKind::Oval);
        }
        0x60..=0x64 | 0x68..=0x6C => {
            if op < 0x68 {
                qd.last_rect = c.rect();
            }
            qd.last_arc = (c.i16() as i32, c.i16() as i32);
            shape_rect(g, qd, (op & 7) as u8, ShapeKind::Arc);
        }
        0x05 | 0x06 | 0x08 | 0x0B | 0x0C | 0x10 | 0x15 | 0x16 | 0x1D | 0x1F | 0x2D | 0x2E | 0xA0 => {}
        _ => {}
    }
}

fn pat(c: &mut Cur) -> [u8; 8] {
    let b = c.bytes(8);
    let mut p = [0u8; 8];
    p[..b.len()].copy_from_slice(b);
    p
}

fn rgb16(c: &mut Cur) -> Rgb {
    [(c.u16() >> 8) as u8, (c.u16() >> 8) as u8, (c.u16() >> 8) as u8]
}

/// A pattern as a brush: solid when all one colour, else tiled in both.
fn pattern_brush(p: [u8; 8], fg: Rgb, bg: Rgb) -> (Brush, Rgb) {
    if p == BLACK {
        return (Brush::Solid(fg), bg);
    }
    if p == WHITE {
        return (Brush::Solid(bg), bg);
    }
    let on = (0..64).map(|i| p[i / 8] & (0x80 >> (i % 8)) != 0).collect();
    (Brush::Hatch(Rc::new(MonoTile { width: 8, height: 8, on }), fg), bg)
}

/// What a shape opcode does: frame, paint, erase, invert, fill.
fn style(g: &mut Gdi, qd: &Qd, verb: u8) -> bool {
    let pen_w = ((qd.pen_size.0 + qd.pen_size.1) as f64 / 2.0).max(0.0);
    g.dc.rop2 = 13;
    match verb {
        0 => {
            g.dc.brush = Rc::new(Brush::Null);
            let mut pen = Pen::solid(qd.fg);
            pen.width = pen_w;
            pen.geometric = true;
            pen.cap = LineCap::Butt;
            pen.join = tiny_skia::LineJoin::Miter;
            pen.inside_frame = true;
            pen.null = qd.pen_size.0 == 0 || qd.pen_size.1 == 0;
            g.dc.pen = Rc::new(pen);
        }
        1..=4 => {
            let (p, fg, bg) = match verb {
                1 => (qd.pen_pat, qd.fg, qd.bk),
                2 => (qd.bk_pat, qd.bk, qd.bk),
                3 => (BLACK, [255, 255, 255], [255, 255, 255]),
                _ => (qd.fill_pat, qd.fg, qd.bk),
            };
            let (brush, bg) = pattern_brush(p, fg, bg);
            g.dc.bk_color = bg;
            g.dc.bk_opaque = true;
            g.dc.brush = Rc::new(brush);
            g.dc.pen = Rc::new(Pen::none());
            if verb == 3 {
                g.dc.rop2 = 6;
            }
        }
        _ => return false,
    }
    true
}

#[derive(Clone, Copy, PartialEq)]
enum ShapeKind {
    Rect,
    Round,
    Oval,
    Arc,
}

fn shape_rect(g: &mut Gdi, qd: &Qd, verb: u8, kind: ShapeKind) {
    if !style(g, qd, verb) {
        return;
    }
    let r = qd.last_rect.map(|v| v as f64);
    match kind {
        ShapeKind::Rect => g.rectangle(r[0], r[1], r[2], r[3]),
        // The corner diameters default to a fifth of the box.
        ShapeKind::Round => g.round_rect(r[0], r[1], r[2], r[3], (r[2] - r[0]) / 5.0, (r[3] - r[1]) / 5.0),
        ShapeKind::Oval => g.ellipse(r[0], r[1], r[2], r[3]),
        ShapeKind::Arc => {
            // Angles run clockwise from straight up.
            let (start, sweep) = (qd.last_arc.0 as f64 - 90.0, qd.last_arc.1 as f64);
            let c = ((r[0] + r[2]) / 2.0, (r[1] + r[3]) / 2.0);
            let radii = ((r[2] - r[0]).abs() / 2.0, (r[3] - r[1]).abs() / 2.0);
            let mut geo = Geo::new(g.dc.total);
            if verb != 0 {
                geo.move_to(c.0, c.1);
                geo.arc(c, radii, start.to_radians(), sweep.to_radians(), true);
                geo.close();
            } else {
                geo.arc(c, radii, start.to_radians(), sweep.to_radians(), false);
            }
            g.emit(geo.finish(), verb != 0, verb == 0);
        }
    }
}

fn shape_poly(g: &mut Gdi, qd: &Qd, verb: u8, pts: &[(f64, f64)]) {
    if pts.len() < 2 || !style(g, qd, verb) {
        return;
    }
    // A framed polygon is an open outline; painted ones are closed.
    g.poly(pts, verb != 0, verb != 0, verb == 0, false);
}

fn shape_region(g: &mut Gdi, qd: &Qd, verb: u8, rects: &[[f64; 4]]) {
    if rects.is_empty() || !style(g, qd, verb) {
        return;
    }
    let region = Region { rects: rects.to_vec(), device: false };
    if let Some(p) = g.region_path(&region) {
        g.emit(Some(p), verb != 0, verb == 0);
    }
}

fn line(g: &mut Gdi, qd: &mut Qd, a: (i32, i32), b: (i32, i32)) {
    style(g, qd, 0);
    g.move_to(a.0 as f64, a.1 as f64);
    g.line_to(b.0 as f64, b.1 as f64);
    qd.pen_loc = b;
}

fn text(g: &mut Gdi, c: &mut Cur, qd: &mut Qd, op: u16) {
    match op {
        0x28 => qd.pen_loc = c.point(),
        0x29 => qd.pen_loc.0 += c.u8() as i32,
        0x2A => qd.pen_loc.1 += c.u8() as i32,
        _ => {
            let (dh, dv) = (c.u8() as i32, c.u8() as i32);
            qd.pen_loc = (qd.pen_loc.0 + dh, qd.pen_loc.1 + dv);
        }
    }
    let n = c.u8() as usize;
    let chars = mac_roman(c.bytes(n));
    let face = qd.text_face;
    g.dc.font = Rc::new(Font {
        height: -(qd.text_size.max(1) as f64),
        weight: if face & 1 != 0 { 700 } else { 400 },
        italic: face & 2 != 0,
        underline: face & 4 != 0,
        face: if qd.font_name.is_empty() { font_name(qd.text_font).to_string() } else { qd.font_name.clone() },
        ..Font::default()
    });
    g.dc.text_color = qd.fg;
    g.dc.bk_opaque = false;
    g.dc.cur = (qd.pen_loc.0 as f64, qd.pen_loc.1 as f64);
    g.text_out(g.dc.cur, &chars, &[], 0, None);
    qd.pen_loc = (g.dc.cur.0.round() as i32, g.dc.cur.1.round() as i32);
}

/// The names of the classic Macintosh fonts by their numbers.
fn font_name(id: u16) -> &'static str {
    match id {
        0 => "Chicago",
        1 => "Application",
        2 => "New York",
        3 => "Geneva",
        4 => "Monaco",
        20 => "Times",
        21 => "Helvetica",
        22 => "Courier",
        23 => "Symbol",
        _ => "Geneva",
    }
}

/// A region: size, bounding box, and for complex ones scan-line data.
fn read_region(c: &mut Cur) -> Option<Vec<[f64; 4]>> {
    let start = c.p;
    let size = c.u16() as usize;
    let bbox = c.rect();
    let end = start + size.max(10);
    if size <= 10 {
        c.p = end;
        return Some(vec![bbox.map(|v| v as f64)]);
    }
    // Scan lines: a y, then pairs of x toggles, ended by 0x7FFF; a y of
    // 0x7FFF ends the region.
    let mut rects: Vec<[f64; 4]> = Vec::new();
    let mut state: Vec<i32> = Vec::new();
    let mut prev_y: Option<i32> = None;
    let flush = |state: &Vec<i32>, from: i32, to: i32, rects: &mut Vec<[f64; 4]>| {
        for pair in state.chunks_exact(2) {
            if rects.len() < 1 << 16 && to > from {
                rects.push([pair[0] as f64, from as f64, pair[1] as f64, to as f64]);
            }
        }
    };
    while c.p + 2 <= end.min(c.d.len()) {
        let y = c.i16() as i32;
        if y == 0x7FFF {
            break;
        }
        let mut toggles: Vec<i32> = Vec::new();
        while c.p + 2 <= end.min(c.d.len()) {
            let x = c.i16() as i32;
            if x == 0x7FFF {
                break;
            }
            toggles.push(x);
            if toggles.len() > 4096 {
                break;
            }
        }
        if let Some(py) = prev_y {
            flush(&state, py, y, &mut rects);
        }
        // Each listed x flips the inside/outside state from there on.
        for x in toggles {
            match state.binary_search(&x) {
                Ok(i) => {
                    state.remove(i);
                }
                Err(i) => state.insert(i, x),
            }
        }
        prev_y = Some(y);
    }
    c.p = end;
    Some(rects)
}

struct PixMap {
    row_bytes: usize,
    bounds: [i32; 4],
    pack_type: u16,
    pixel_size: u16,
    cmp_count: u16,
}

fn read_pixmap_header(c: &mut Cur) -> PixMap {
    let rb = c.u16();
    let bounds = c.rect();
    c.skip(2); // pmVersion
    let pack_type = c.u16();
    c.skip(4 + 4 + 4 + 2); // packSize, hRes, vRes, pixelType
    let pixel_size = c.u16();
    let cmp_count = c.u16();
    c.skip(2 + 4 + 4 + 4); // cmpSize, planeBytes, pmTable, pmReserved
    PixMap { row_bytes: (rb & 0x3FFF) as usize, bounds, pack_type, pixel_size, cmp_count }
}

/// Expands PackBits data to exactly `len` bytes (padding with zeros).  With
/// `unit` 2 the repeat counts count 16-bit units.
fn unpack(src: &[u8], len: usize, unit: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(len);
    let mut i = 0;
    while i < src.len() && out.len() < len {
        let n = src[i] as i8;
        i += 1;
        if n >= 0 {
            let count = (n as usize + 1) * unit;
            let chunk = src.get(i..(i + count).min(src.len())).unwrap_or(&[]);
            out.extend_from_slice(chunk);
            i += count;
        } else if n != -128 {
            let times = (1 - n as isize) as usize;
            let Some(unit_bytes) = src.get(i..i + unit) else { break };
            for _ in 0..times {
                out.extend_from_slice(unit_bytes);
                if out.len() >= len {
                    break;
                }
            }
            i += unit;
        }
    }
    out.resize(len, 0);
    out.truncate(len);
    out
}

/// How many bytes of PackBits output a packed row makes.
fn unpacked_len(src: &[u8], unit: usize) -> usize {
    let (mut i, mut out) = (0usize, 0usize);
    while i < src.len() {
        let n = src[i] as i8;
        i += 1;
        if n >= 0 {
            out += (n as usize + 1) * unit;
            i += (n as usize + 1) * unit;
        } else if n != -128 {
            out += (1 - n as isize) as usize * unit;
            i += unit;
        }
    }
    out
}

/// The width in bytes (1 or 2) of the count before each packed row, found
/// by trying the usual width first and checking that the row it frames
/// unpacks to the right length.
fn count_width(data: &[u8], row_len: usize, unit: usize, wide_first: bool) -> usize {
    let order = if wide_first { [2usize, 1] } else { [1, 2] };
    for width in order {
        let n = if width == 2 {
            data.get(..2).map_or(0, |b| u16::from_be_bytes([b[0], b[1]]) as usize)
        } else {
            data.first().copied().unwrap_or(0) as usize
        };
        if let Some(raw) = data.get(width..width + n).filter(|_| n > 0)
            && unpacked_len(raw, unit) == row_len
        {
            return width;
        }
    }
    if wide_first { 2 } else { 1 }
}

/// The bitmap opcodes: `BitsRect`, `PackBitsRect`, `DirectBitsRect` and
/// their region forms.
fn bits(g: &mut Gdi, c: &mut Cur, qd: &Qd, op: u16) {
    let has_region = matches!(op, 0x91 | 0x99 | 0x9B);
    let direct = matches!(op, 0x9A | 0x9B);
    if direct {
        c.skip(4); // base address
    }
    // The pixmap header, or the old bitmap's row bytes and bounds.
    let flag = c.d.get(c.p).copied().unwrap_or(0) & 0x80 != 0;
    let pm = if flag || direct {
        read_pixmap_header(c)
    } else {
        let rb = c.u16() as usize;
        let bounds = c.rect();
        PixMap { row_bytes: rb & 0x3FFF, bounds, pack_type: 0, pixel_size: 1, cmp_count: 1 }
    };
    let mut table: Vec<Rgb> = Vec::new();
    if pm.pixel_size <= 8 && (flag || op == 0x98 || op == 0x99) && !direct {
        let _seed = c.u32();
        let flags = c.u16();
        let n = (c.u16() as usize + 1).min(256);
        table = vec![[0, 0, 0]; 256];
        for pos in 0..n {
            let value = c.u16() as usize;
            let rgb = [(c.u16() >> 8) as u8, (c.u16() >> 8) as u8, (c.u16() >> 8) as u8];
            let idx = if flags & 0x8000 != 0 { value & 0xFF } else { pos };
            table[idx] = rgb;
        }
    }
    let src = c.rect();
    let dst = c.rect();
    let mode = c.u16();
    if has_region {
        let _ = read_region(c);
    }
    let (w, h) = ((pm.bounds[2] - pm.bounds[0]).max(0) as usize, (pm.bounds[3] - pm.bounds[1]).max(0) as usize);
    if w == 0 || h == 0 || w * h > crate::dib::MAX_PIXELS as usize || pm.row_bytes == 0 {
        return;
    }
    // Rows are either stored plain or packed behind a byte count.
    let packed = pm.row_bytes >= 8 && pm.pack_type != 1;
    let mut rgba = vec![255u8; w * h * 4];
    let mut mono = false;
    let (row_len, unit) = match pm.pixel_size {
        16 => (pm.row_bytes, 2),
        32 => (w * pm.cmp_count.max(3) as usize, 1),
        _ => (pm.row_bytes, 1),
    };
    // Rows of wide pictures are preceded by a two-byte count, narrower ones
    // by a one-byte count; some writers decide by the unpacked length rather
    // than the row bytes, so check the first row against its own length.
    let count_width =
        if packed { count_width(&c.d[c.p.min(c.d.len())..], row_len, unit, pm.row_bytes > 250) } else { 1 };
    for y in 0..h {
        let row: Vec<u8> = if packed {
            let n = if count_width == 2 { c.u16() as usize } else { c.u8() as usize };
            let raw = c.bytes(n);
            unpack(raw, row_len, unit)
        } else {
            c.bytes(pm.row_bytes).to_vec()
        };
        for x in 0..w {
            let px: Rgb = match pm.pixel_size {
                1 | 2 | 4 | 8 => {
                    let bits = pm.pixel_size as usize;
                    let per = 8 / bits;
                    let byte = row.get(x / per).copied().unwrap_or(0);
                    let shift = 8 - bits * (x % per + 1);
                    let i = ((byte >> shift) as usize) & ((1 << bits) - 1);
                    if table.is_empty() {
                        mono = true;
                        // Old bitmaps: a set bit is black.
                        if i == 1 { qd.fg } else { qd.bk }
                    } else {
                        table.get(i).copied().unwrap_or([0, 0, 0])
                    }
                }
                16 => {
                    let v = ((row.get(x * 2).copied().unwrap_or(0) as u16) << 8)
                        | row.get(x * 2 + 1).copied().unwrap_or(0) as u16;
                    let e = |s: u32| ((((v >> s) & 31) as u32 * 255 + 15) / 31) as u8;
                    [e(10), e(5), e(0)]
                }
                32 => {
                    let planes = pm.cmp_count.max(3) as usize;
                    let o = planes - 3;
                    if pm.pack_type == 4 || packed {
                        [
                            row.get(o * w + x).copied().unwrap_or(0),
                            row.get((o + 1) * w + x).copied().unwrap_or(0),
                            row.get((o + 2) * w + x).copied().unwrap_or(0),
                        ]
                    } else {
                        [
                            row.get(x * 4 + 1).copied().unwrap_or(0),
                            row.get(x * 4 + 2).copied().unwrap_or(0),
                            row.get(x * 4 + 3).copied().unwrap_or(0),
                        ]
                    }
                }
                _ => [0, 0, 0],
            };
            let i = (y * w + x) * 4;
            rgba[i..i + 3].copy_from_slice(&px);
        }
    }
    // Source-or mode draws a bitmap's white as see-through.
    if mode & 0x3F == 1 && mono {
        for p in rgba.chunks_exact_mut(4) {
            if p[0] == qd.bk[0] && p[1] == qd.bk[1] && p[2] == qd.bk[2] {
                p[3] = 0;
            }
        }
    }
    let bmp = Bitmap { width: w as u32, height: h as u32, rgba, mono: false, has_alpha: mode & 0x3F == 1 };
    let s = [
        (src[0] - pm.bounds[0]) as i64,
        (src[1] - pm.bounds[1]) as i64,
        (src[2] - src[0]) as i64,
        (src[3] - src[1]) as i64,
    ];
    let d = [dst[0] as f64, dst[1] as f64, (dst[2] - dst[0]) as f64, (dst[3] - dst[1]) as f64];
    g.draw_bitmap(&bmp, s, d, 0x00CC_0020, None);
}

/// QuickTime-compressed images: when the data holds a PNG or JPEG file, draw
/// it over the picture frame; other codecs aren't supported.
fn quicktime(g: &mut Gdi, c: &mut Cur, op: u16) {
    let n = c.u32() as usize;
    let payload = c.bytes(n);
    let find = |magic: &[u8]| payload.windows(magic.len()).position(|w| w == magic);
    let start = find(b"\x89PNG\r\n\x1a\n").or_else(|| find(&[0xFF, 0xD8, 0xFF]));
    let Some(start) = start else {
        g.warnings.skip(if op == 0x8200 { "QuickTime image" } else { "uncompressed QuickTime image" });
        return;
    };
    let Ok(tex) = slerp_scene::textures::decode(&payload[start..], &slerp_scene::textures::DecodeOptions::full())
    else {
        g.warnings.skip("QuickTime image");
        return;
    };
    let level = &tex.levels[0];
    let slerp_scene::textures::Pixels::Rgba8(px) = &level.surfaces[0] else { return };
    let bmp =
        Bitmap { width: level.width, height: level.height, rgba: px.clone(), mono: false, has_alpha: tex.has_alpha };
    // The destination rectangle sits in the QuickTime matrix; without
    // parsing it, the image covers the frame.
    let win = g.dc.win_org;
    let ext = g.base.invert().map(|b| b.apply((g.cv.width() as f64, g.cv.height() as f64))).unwrap_or((0.0, 0.0));
    g.draw_bitmap(
        &bmp,
        [0, 0, level.width as i64, level.height as i64],
        [win.0, win.1, ext.0, ext.1],
        0x00CC_0020,
        None,
    );
}
