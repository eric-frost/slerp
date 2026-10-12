//! Video-style pictures (what AV1 and HEVC decode to) become RGB: planes of
//! luma and chroma at 8, 10 or 12 bits, with the chroma planes possibly
//! smaller than the luma, in limited or full range, under one of several
//! matrices.  The result is 8-bit sRGB, or for HDR transfer curves (PQ,
//! HLG) linear floats the viewer's exposure slider handles.

use crate::color::Conversion;
use rayon::prelude::*;

/// How the chroma planes are sampled.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Layout {
    /// Luma only.
    I400,
    /// Chroma at half the width and height.
    I420,
    /// Chroma at half the width.
    I422,
    I444,
}

impl Layout {
    /// Horizontal and vertical subsampling factors.
    pub fn factors(self) -> (u32, u32) {
        match self {
            Layout::I400 | Layout::I444 => (1, 1),
            Layout::I422 => (2, 1),
            Layout::I420 => (2, 2),
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Layout::I400 => "4:0:0",
            Layout::I420 => "4:2:0",
            Layout::I422 => "4:2:2",
            Layout::I444 => "4:4:4",
        }
    }
}

/// Colour code points as H.273 numbers them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cicp {
    pub primaries: u8,
    pub transfer: u8,
    pub matrix: u8,
    pub full_range: bool,
}

impl Cicp {
    /// What viewers assume when a file doesn't say: Rec. 709 primaries, the
    /// sRGB curve and Rec. 601 matrix coefficients (as libavif does), full range.
    pub const UNSPECIFIED: Cicp = Cicp { primaries: 2, transfer: 2, matrix: 2, full_range: true };

    /// Replaces unspecified codes by the usual assumptions.
    pub fn resolved(self) -> Cicp {
        Cicp {
            primaries: if self.primaries == 2 { 1 } else { self.primaries },
            transfer: if self.transfer == 2 { 13 } else { self.transfer },
            matrix: if self.matrix == 2 { 6 } else { self.matrix },
            full_range: self.full_range,
        }
    }
}

/// A decoded picture: planes of samples, one `u16` each whatever the depth.
#[derive(Clone, Debug)]
pub struct Yuv {
    pub width: u32,
    pub height: u32,
    pub bit_depth: u8,
    pub layout: Layout,
    pub y: Vec<u16>,
    pub u: Vec<u16>,
    pub v: Vec<u16>,
    pub cicp: Cicp,
}

impl Yuv {
    /// A picture of mid grey, to be filled in (tiles of a grid are blitted
    /// into it).
    pub fn blank(width: u32, height: u32, bit_depth: u8, layout: Layout, cicp: Cicp) -> Option<Yuv> {
        let (cw, ch) = Yuv::chroma_size(width, height, layout);
        let mid = 1u16 << (bit_depth - 1);
        let luma = (width as usize).checked_mul(height as usize)?;
        let chroma = if layout == Layout::I400 { 0 } else { (cw as usize).checked_mul(ch as usize)? };
        Some(Yuv {
            width,
            height,
            bit_depth,
            layout,
            y: vec![0; luma],
            u: vec![mid; chroma],
            v: vec![mid; chroma],
            cicp,
        })
    }

    pub fn chroma_size(width: u32, height: u32, layout: Layout) -> (u32, u32) {
        if layout == Layout::I400 {
            return (0, 0);
        }
        let (fx, fy) = layout.factors();
        (width.div_ceil(fx), height.div_ceil(fy))
    }

    /// Copies `tile` into this picture with its top left corner at (`x`,
    /// `y`), clipped to the picture.  Offsets must be multiples of the
    /// chroma subsampling factors (grid tiles are).
    pub fn blit(&mut self, tile: &Yuv, x: u32, y: u32) -> Result<(), String> {
        if tile.layout != self.layout || tile.bit_depth != self.bit_depth {
            return Err("the tiles of a grid differ in format".into());
        }
        let copy = |dst: &mut [u16], dw: u32, dh: u32, src: &[u16], sw: u32, sh: u32, ox: u32, oy: u32| {
            for row in 0..sh.min(dh.saturating_sub(oy)) {
                let w = sw.min(dw.saturating_sub(ox)) as usize;
                let d = ((oy + row) as usize * dw as usize) + ox as usize;
                let s = row as usize * sw as usize;
                if d + w <= dst.len() && s + w <= src.len() {
                    dst[d..d + w].copy_from_slice(&src[s..s + w]);
                }
            }
        };
        copy(&mut self.y, self.width, self.height, &tile.y, tile.width, tile.height, x, y);
        if self.layout != Layout::I400 {
            let (fx, fy) = self.layout.factors();
            let (cw, ch) = Yuv::chroma_size(self.width, self.height, self.layout);
            let (tw, th) = Yuv::chroma_size(tile.width, tile.height, tile.layout);
            copy(&mut self.u, cw, ch, &tile.u, tw, th, x / fx, y / fy);
            copy(&mut self.v, cw, ch, &tile.v, tw, th, x / fx, y / fy);
        }
        Ok(())
    }

    /// The opacity plane of an alpha picture, from 0 to the depth's maximum.
    /// An alpha picture can be coded in limited range like any other AV1
    /// video, which keeps opacity between 16 and 235 (at 8 bits); this
    /// stretches it back, as libavif does.
    pub fn opacity(&self) -> std::borrow::Cow<'_, [u16]> {
        if self.cicp.full_range {
            return std::borrow::Cow::Borrowed(&self.y);
        }
        let shift = self.bit_depth.clamp(8, 16) as u32 - 8;
        let (lo, span) = ((16u32 << shift) as f32, (219u32 << shift) as f32);
        let max = ((1u32 << self.bit_depth.clamp(1, 16)) - 1) as f32;
        std::borrow::Cow::Owned(
            self.y.iter().map(|&v| ((v as f32 - lo) / span * max + 0.5).clamp(0.0, max) as u16).collect(),
        )
    }

    /// Appends the picture to `out` as bytes, to pass it between processes.
    pub fn encode(&self, out: &mut Vec<u8>) {
        let layout = match self.layout {
            Layout::I400 => 0u8,
            Layout::I420 => 1,
            Layout::I422 => 2,
            Layout::I444 => 3,
        };
        let c = self.cicp;
        out.extend(self.width.to_le_bytes());
        out.extend(self.height.to_le_bytes());
        out.extend([self.bit_depth, layout, c.primaries, c.transfer, c.matrix, c.full_range as u8]);
        for plane in [&self.y, &self.u, &self.v] {
            out.extend((plane.len() as u64).to_le_bytes());
            out.extend(plane.iter().flat_map(|v| v.to_le_bytes()));
        }
    }

    /// Reads a picture [`Yuv::encode`] wrote from the front of `b`, leaving
    /// the rest; `None` when the bytes don't hold a consistent one.
    pub fn decode(b: &mut &[u8]) -> Option<Yuv> {
        fn take<'a>(b: &mut &'a [u8], n: usize) -> Option<&'a [u8]> {
            let (head, rest) = (b.get(..n)?, b.get(n..)?);
            *b = rest;
            Some(head)
        }
        let width = u32::from_le_bytes(take(b, 4)?.try_into().ok()?);
        let height = u32::from_le_bytes(take(b, 4)?.try_into().ok()?);
        let h = take(b, 6)?;
        let layout = [Layout::I400, Layout::I420, Layout::I422, Layout::I444].get(h[1] as usize).copied()?;
        let cicp = Cicp { primaries: h[2], transfer: h[3], matrix: h[4], full_range: h[5] != 0 };
        let mut planes = Vec::new();
        for _ in 0..3 {
            let n = usize::try_from(u64::from_le_bytes(take(b, 8)?.try_into().ok()?)).ok()?;
            let bytes = take(b, n.checked_mul(2)?)?;
            planes.push(bytes.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect::<Vec<u16>>());
        }
        let (v, u, y) = (planes.pop()?, planes.pop()?, planes.pop()?);
        let (cw, ch) = Yuv::chroma_size(width, height, layout);
        let chroma = if layout == Layout::I400 { 0 } else { cw as usize * ch as usize };
        let fits = y.len() == width as usize * height as usize && u.len() == chroma && v.len() == chroma;
        (fits && (1..=16).contains(&h[0])).then_some(Yuv { width, height, bit_depth: h[0], layout, y, u, v, cicp })
    }
}

/// The luma weights (Kr, Kb) of a matrix coefficients code.  `None` for the
/// identity matrix (GBR) and for codes this doesn't know.
fn weights(matrix: u8) -> Option<(f32, f32)> {
    Some(match matrix {
        1 => (0.2126, 0.0722),
        4 => (0.30, 0.11),
        5 | 6 => (0.299, 0.114),
        7 => (0.212, 0.087),
        9 | 10 => (0.2627, 0.0593),
        _ => return None,
    })
}

/// Pictures converted to display form.
pub enum Rgba {
    U8(Vec<u8>),
    /// Linear light in sRGB primaries, 1.0 being SDR white.
    F32(Vec<f32>),
}

/// How the colour was handled, for the inspector.
pub struct Converted {
    pub pixels: Rgba,
    pub note: String,
}

/// Linear interpolation of a chroma plane at luma position (`x`, `y`):
/// chroma samples sit between luma samples for 4:2:0 and 4:2:2 vertically
/// (centre siting) and on the luma sample horizontally for 4:2:2 is not
/// assumed; centre siting both ways is used, which is what most decoders do
/// when a file doesn't say.
#[inline]
fn upsample(plane: &[u16], pw: usize, ph: usize, x: usize, y: usize, fx: usize, fy: usize) -> f32 {
    if fx == 1 && fy == 1 {
        return plane[y * pw + x] as f32;
    }
    let sx = (x as f32 + 0.5) / fx as f32 - 0.5;
    let sy = (y as f32 + 0.5) / fy as f32 - 0.5;
    let (x0f, y0f) = (sx.floor(), sy.floor());
    let (tx, ty) = (sx - x0f, sy - y0f);
    let cl = |v: f32, max: usize| (v.max(0.0) as usize).min(max - 1);
    let (x0, x1) = (cl(x0f, pw), cl(x0f + 1.0, pw));
    let (y0, y1) = (cl(y0f, ph), cl(y0f + 1.0, ph));
    let a = plane[y0 * pw + x0] as f32 * (1.0 - tx) + plane[y0 * pw + x1] as f32 * tx;
    let b = plane[y1 * pw + x0] as f32 * (1.0 - tx) + plane[y1 * pw + x1] as f32 * tx;
    a * (1.0 - ty) + b * ty
}

/// Converts to RGB.  `alpha` is an opacity plane of the same size and
/// depth `alpha_depth`; `premultiplied` says the colour has the alpha
/// multiplied in.
pub fn to_rgb(pic: &Yuv, alpha: Option<(&[u16], u8)>, premultiplied: bool) -> Result<Converted, String> {
    let (w, h) = (pic.width as usize, pic.height as usize);
    if w == 0 || h == 0 {
        return Err("the picture is empty".into());
    }
    let cicp = pic.cicp.resolved();
    let bd = pic.bit_depth.clamp(8, 16) as u32;
    let max = ((1u32 << bd) - 1) as f32;
    let (fx, fy) = pic.layout.factors();
    let (fx, fy) = (fx as usize, fy as usize);
    let (cw, ch) = Yuv::chroma_size(pic.width, pic.height, pic.layout);
    let (cw, ch) = (cw as usize, ch as usize);
    if pic.y.len() != w * h || (pic.layout != Layout::I400 && (pic.u.len() != cw * ch || pic.v.len() != cw * ch)) {
        return Err("the planes don't match the picture's size".into());
    }
    // Limited range puts black at 16 and white at 235 (luma), chroma 16 to 240, scaled to the depth.
    let scale = (1u32 << (bd - 8)) as f32;
    let (y_off, y_span, c_mid, c_span) = if cicp.full_range {
        (0.0, max, (1u32 << (bd - 1)) as f32, max)
    } else {
        (16.0 * scale, 219.0 * scale, 128.0 * scale, 224.0 * scale)
    };
    let identity = cicp.matrix == 0;
    let (kr, kb) = weights(cicp.matrix).unwrap_or((0.2126, 0.0722));
    let kg = 1.0 - kr - kb;
    let mono = pic.layout == Layout::I400;
    // 1. Encoded R'G'B' as codes at the picture's depth.
    let mut codes = vec![0u16; w * h * 4];
    codes.par_chunks_mut(w * 4).enumerate().for_each(|(row, out)| {
        for x in 0..w {
            let yy = (pic.y[row * w + x] as f32 - y_off) / y_span;
            let (r, g, b) = if mono {
                (yy, yy, yy)
            } else if identity {
                // GBR: the three planes are green, blue and red, each coded
                // like luma and all at full size.
                let i = row * w + x;
                let plane = |p: &[u16]| (p.get(i).copied().unwrap_or(0) as f32 - y_off) / y_span;
                (plane(&pic.v), yy, plane(&pic.u))
            } else {
                let cb = (upsample(&pic.u, cw, ch, x, row, fx, fy) - c_mid) / c_span;
                let cr = (upsample(&pic.v, cw, ch, x, row, fx, fy) - c_mid) / c_span;
                (
                    yy + 2.0 * (1.0 - kr) * cr,
                    yy - 2.0 * (1.0 - kb) * kb / kg * cb - 2.0 * (1.0 - kr) * kr / kg * cr,
                    yy + 2.0 * (1.0 - kb) * cb,
                )
            };
            let o = &mut out[x * 4..x * 4 + 4];
            o[0] = (r * max + 0.5).clamp(0.0, max) as u16;
            o[1] = (g * max + 0.5).clamp(0.0, max) as u16;
            o[2] = (b * max + 0.5).clamp(0.0, max) as u16;
            o[3] = max as u16;
        }
    });
    // 2. Opacity.
    if let Some((plane, adepth)) = alpha {
        if plane.len() != w * h {
            return Err("the alpha plane doesn't match the picture's size".into());
        }
        let amax = ((1u32 << adepth.clamp(1, 16)) - 1) as f32;
        codes.par_chunks_mut(w * 4).enumerate().for_each(|(row, out)| {
            for x in 0..w {
                let a = (plane[row * w + x] as f32 / amax).clamp(0.0, 1.0);
                let o = &mut out[x * 4..x * 4 + 4];
                if premultiplied && a > 0.0 {
                    for c in &mut o[..3] {
                        *c = (*c as f32 / a).round().min(max) as u16;
                    }
                }
                o[3] = (a * max + 0.5) as u16;
            }
        });
    }
    // 3. Colour: straight to bytes when it is sRGB already, otherwise through linear light.
    let conversion = Conversion::from_cicp([cicp.primaries, cicp.transfer, cicp.matrix, cicp.full_range as u8]);
    let name =
        |c: &Conversion| format!("{}{}", c.name, if cicp.full_range { ", full range" } else { ", limited range" });
    match conversion {
        Some(c) if c.is_hdr() => {
            let lin = c.to_linear_rgba(&codes, bd);
            Ok(Converted { pixels: Rgba::F32(lin), note: format!("{}, shown through the HDR path", name(&c)) })
        }
        Some(c) if !c.is_srgb() => {
            let mut bytes = to_u8(&codes, bd);
            c.convert_rgba8(&mut bytes);
            Ok(Converted { pixels: Rgba::U8(bytes), note: format!("{}, converted to sRGB", name(&c)) })
        }
        Some(c) => Ok(Converted { pixels: Rgba::U8(to_u8(&codes, bd)), note: name(&c) }),
        None => {
            Ok(Converted { pixels: Rgba::U8(to_u8(&codes, bd)), note: "unknown colour codes, shown as sRGB".into() })
        }
    }
}

fn to_u8(codes: &[u16], bd: u32) -> Vec<u8> {
    if bd == 8 {
        return codes.iter().map(|&v| v.min(255) as u8).collect();
    }
    let max = ((1u32 << bd) - 1) as f32;
    codes.par_iter().map(|&v| (v as f32 / max * 255.0 + 0.5) as u8).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flat(layout: Layout, bd: u8, y: u16, u: u16, v: u16, cicp: Cicp) -> Yuv {
        let (cw, ch) = Yuv::chroma_size(4, 4, layout);
        let n = (cw * ch) as usize;
        Yuv { width: 4, height: 4, bit_depth: bd, layout, y: vec![y; 16], u: vec![u; n], v: vec![v; n], cicp }
    }

    fn px(c: &Converted, i: usize) -> [u8; 4] {
        match &c.pixels {
            Rgba::U8(v) => v[i * 4..i * 4 + 4].try_into().unwrap(),
            _ => panic!("not 8-bit"),
        }
    }

    const BT709_LIMITED: Cicp = Cicp { primaries: 1, transfer: 13, matrix: 1, full_range: false };
    const BT601_FULL: Cicp = Cicp { primaries: 1, transfer: 13, matrix: 6, full_range: true };

    #[test]
    fn grey_and_primary_colours_in_limited_range() {
        // Black, white and mid grey: chroma at its middle.
        let c = to_rgb(&flat(Layout::I444, 8, 16, 128, 128, BT709_LIMITED), None, false).unwrap();
        assert_eq!(px(&c, 0), [0, 0, 0, 255]);
        let c = to_rgb(&flat(Layout::I444, 8, 235, 128, 128, BT709_LIMITED), None, false).unwrap();
        assert_eq!(px(&c, 5), [255, 255, 255, 255]);
        // Pure red in BT.709 limited range: Y 63, Cb 102, Cr 240.
        let c = to_rgb(&flat(Layout::I444, 8, 63, 102, 240, BT709_LIMITED), None, false).unwrap();
        let p = px(&c, 0);
        assert!(p[0] >= 253 && p[1] <= 2 && p[2] <= 2, "{p:?}");
    }

    #[test]
    fn full_range_and_the_601_matrix() {
        // BT.601 full range red: Y 76, Cb 85, Cr 255.
        let c = to_rgb(&flat(Layout::I444, 8, 76, 85, 255, BT601_FULL), None, false).unwrap();
        let p = px(&c, 0);
        assert!(p[0] >= 253 && p[1] <= 2 && p[2] <= 2, "{p:?}");
        // Unspecified matrix means 601, as in libavif.
        let un = Cicp { primaries: 2, transfer: 2, matrix: 2, full_range: true };
        let c = to_rgb(&flat(Layout::I444, 8, 76, 85, 255, un), None, false).unwrap();
        assert_eq!(px(&c, 0), px(&to_rgb(&flat(Layout::I444, 8, 76, 85, 255, BT601_FULL), None, false).unwrap(), 0));
    }

    #[test]
    fn ten_and_twelve_bit_scale_to_eight() {
        let c = to_rgb(&flat(Layout::I444, 10, 1023, 512, 512, BT601_FULL), None, false).unwrap();
        assert_eq!(px(&c, 0), [255, 255, 255, 255]);
        let c = to_rgb(&flat(Layout::I444, 12, 2048, 2048, 2048, BT601_FULL), None, false).unwrap();
        let p = px(&c, 0);
        assert!((p[0] as i32 - 128).abs() <= 1 && p[0] == p[1] && p[1] == p[2], "{p:?}");
    }

    #[test]
    fn subsampled_chroma_is_interpolated() {
        // 4:2:0: a flat grey picture stays flat whatever the siting.
        let c = to_rgb(&flat(Layout::I420, 8, 128, 128, 128, BT601_FULL), None, false).unwrap();
        for i in 0..16 {
            assert_eq!(px(&c, i), [128, 128, 128, 255]);
        }
        // Chroma that changes across the picture changes smoothly.
        let mut p = flat(Layout::I420, 8, 128, 128, 128, BT601_FULL);
        p.v = vec![128, 255, 128, 255];
        let c = to_rgb(&p, None, false).unwrap();
        let reds: Vec<i32> = (0..4).map(|x| px(&c, x)[0] as i32).collect();
        assert!(reds[0] < reds[1] && reds[1] < reds[2] && reds[2] <= reds[3] + 1, "{reds:?}");
    }

    #[test]
    fn monochrome_is_grey() {
        let mut p = flat(Layout::I400, 8, 100, 0, 0, BT601_FULL);
        p.u.clear();
        p.v.clear();
        let c = to_rgb(&p, None, false).unwrap();
        assert_eq!(px(&c, 3), [100, 100, 100, 255]);
    }

    #[test]
    fn identity_matrix_is_gbr() {
        // Planes: Y = green 200, U = blue 100, V = red 50.
        let gbr = Cicp { primaries: 1, transfer: 13, matrix: 0, full_range: true };
        let c = to_rgb(&flat(Layout::I444, 8, 200, 100, 50, gbr), None, false).unwrap();
        assert_eq!(px(&c, 0), [50, 200, 100, 255]);
    }

    #[test]
    fn alpha_plane_and_premultiplication() {
        let p = flat(Layout::I444, 8, 255, 128, 128, BT601_FULL);
        let a: Vec<u16> = (0..16).map(|i| if i == 0 { 0 } else { 128 }).collect();
        let c = to_rgb(&p, Some((&a, 8)), false).unwrap();
        assert_eq!(px(&c, 0)[3], 0);
        assert_eq!(px(&c, 1), [255, 255, 255, 128]);
        // Premultiplied: a grey of 64 at alpha 128 is a grey of 128 straight.
        let p = flat(Layout::I444, 8, 64, 128, 128, BT601_FULL);
        let c = to_rgb(&p, Some((&a, 8)), true).unwrap();
        let q = px(&c, 1);
        assert!((q[0] as i32 - 127).abs() <= 1 && q[3] == 128, "{q:?}");
        // A 10-bit alpha plane.
        let a10: Vec<u16> = vec![1023; 16];
        let c = to_rgb(&p, Some((&a10, 10)), false).unwrap();
        assert_eq!(px(&c, 2)[3], 255);
        // The wrong size is refused.
        assert!(to_rgb(&p, Some((&a[..4], 8)), false).is_err());
    }

    #[test]
    fn hdr_comes_out_as_linear_floats() {
        let pq = Cicp { primaries: 9, transfer: 16, matrix: 9, full_range: false };
        let c = to_rgb(&flat(Layout::I444, 10, 940, 512, 512, pq), None, false).unwrap();
        let Rgba::F32(v) = &c.pixels else { panic!("not float") };
        // 940/1023 limited range is PQ code 1.0: 10000 nits, 49 times SDR white.
        assert!(v[0] > 30.0, "{:?}", &v[..4]);
        assert!(c.note.contains("HDR"));
        let c = to_rgb(&flat(Layout::I444, 10, 64, 512, 512, pq), None, false).unwrap();
        let Rgba::F32(v) = &c.pixels else { panic!() };
        assert!(v[0] < 1e-4);
    }

    #[test]
    fn wide_gamut_sdr_is_converted_to_srgb() {
        // Display P3 primaries with the sRGB curve: P3 white is sRGB white.
        let p3 = Cicp { primaries: 12, transfer: 13, matrix: 6, full_range: true };
        let c = to_rgb(&flat(Layout::I444, 8, 255, 128, 128, p3), None, false).unwrap();
        assert_eq!(px(&c, 0), [255, 255, 255, 255]);
        assert!(c.note.contains("converted to sRGB"), "{}", c.note);
    }

    #[test]
    fn bad_planes_are_errors() {
        let mut p = flat(Layout::I420, 8, 1, 1, 1, BT601_FULL);
        p.u.pop();
        assert!(to_rgb(&p, None, false).is_err());
        let mut p = flat(Layout::I444, 8, 1, 1, 1, BT601_FULL);
        p.width = 0;
        assert!(to_rgb(&p, None, false).is_err());
    }

    #[test]
    fn limited_range_opacity_is_stretched() {
        let mut a = flat(Layout::I400, 8, 16, 0, 0, BT709_LIMITED);
        a.y = vec![16, 126, 235, 0, 255, 200, 20, 100, 16, 16, 16, 16, 16, 16, 16, 16];
        let o = a.opacity();
        assert_eq!(&o[..5], [0, 128, 255, 0, 255]);
        // Full range is left as it is.
        let mut full = a.clone();
        full.cicp.full_range = true;
        assert_eq!(&full.opacity()[..5], [16, 126, 235, 0, 255]);
        // At 10 bits the limited range is 64 to 940.
        let mut ten = flat(Layout::I400, 10, 64, 0, 0, BT709_LIMITED);
        ten.y = vec![64, 940, 502, 0, 1023, 64, 64, 64, 64, 64, 64, 64, 64, 64, 64, 64];
        assert_eq!(&ten.opacity()[..5], [0, 1023, 512, 0, 1023]);
    }

    #[test]
    fn pictures_pass_through_bytes() {
        let mut p = flat(Layout::I420, 10, 300, 512, 600, BT709_LIMITED);
        p.y[3] = 1023;
        let mut bytes = Vec::new();
        p.encode(&mut bytes);
        flat(Layout::I400, 8, 9, 0, 0, BT601_FULL).encode(&mut bytes);
        let mut rest = &bytes[..];
        let a = Yuv::decode(&mut rest).unwrap();
        let b = Yuv::decode(&mut rest).unwrap();
        assert!(rest.is_empty());
        assert_eq!(
            (a.y.clone(), a.u.clone(), a.bit_depth, a.layout, a.cicp),
            (p.y, p.u, 10, Layout::I420, BT709_LIMITED)
        );
        assert_eq!((b.layout, b.y[0]), (Layout::I400, 9));
        assert!(Yuv::decode(&mut &bytes[..bytes.len() / 3]).is_none());
    }

    #[test]
    fn grids_are_blitted_with_clipping() {
        let mut big = Yuv::blank(6, 4, 8, Layout::I420, BT601_FULL).unwrap();
        let tile = flat(Layout::I420, 8, 200, 90, 90, BT601_FULL);
        big.blit(&tile, 0, 0).unwrap();
        // The second tile hangs over the right edge and is clipped.
        big.blit(&tile, 4, 0).unwrap();
        assert!(big.y.iter().all(|&v| v == 200));
        assert_eq!(big.u.len(), 3 * 2);
        assert!(big.u.iter().all(|&v| v == 90));
        let wrong = flat(Layout::I444, 8, 1, 1, 1, BT601_FULL);
        assert!(big.blit(&wrong, 0, 0).is_err());
    }
}
