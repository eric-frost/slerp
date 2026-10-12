//! Texture thumbnails: what a file manager shows for a texture, which is not
//! quite the texture.  Alpha is dropped (a mask would otherwise hide the
//! colour), cubemaps unfold into a cross, arrays and volumes show their first
//! layer or slice, one- and two-channel data is made readable, and HDR is
//! exposed automatically and tone mapped.

use crate::color::{linear_to_srgb8, luminance, pbr_neutral, unorm8};
use crate::resize::{fit, resize};
use crate::{ColorSpace, Image8, Pixels, Texture, TextureKind};
use std::borrow::Cow;

/// How much of the colour a texture's format actually stores, which decides
/// how its thumbnail presents it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Channels {
    /// Red only, shown as grey.
    R,
    /// Red and green, usually a tangent-space normal map's X and Y: shown
    /// with blue rebuilt so it looks like a normal map.
    Rg,
    /// Alpha only, shown as grey.
    A,
    /// One channel of measurements rather than colour, such as a 16-bit
    /// heightmap: shown as grey stretched from its lowest to its highest
    /// value, since heights often use a sliver of the range and would
    /// otherwise look flat black.
    Data,
    /// Linear measurements (astronomy frames, film scans) that need an
    /// automatic stretch to be visible: see [`crate::stretch`].
    Stretch,
    Rgba,
}

/// The middle grey that automatic exposure aims the image's log-average
/// luminance at.
const EXPOSURE_KEY: f32 = 0.18;

pub(crate) fn thumbnail(tex: &Texture, size: u32) -> Image8 {
    let size = size.max(1);
    let level = &tex.levels[0];
    let (w, h) = (level.width, level.height);
    let srgb = tex.color_space != ColorSpace::Linear;
    let channels = channels_of(&tex.source.format);

    // 1. Lay out what to show: the six faces of the first cube as a cross,
    //    otherwise the first surface (and first slice of a volume).
    let mut img = if tex.kind == TextureKind::Cube && level.surfaces.len() >= 6 {
        let face = (size / 4).max(1);
        let face = if w > face { face } else { w * (face / w).max(1) };
        let mut cross = Canvas::new(face * 4, face * 3, tex.is_hdr());
        // The usual horizontal cross: +Y above -X +Z +X -Z, then -Y below.
        const SPOTS: [(u32, u32); 6] = [(2, 1), (0, 1), (1, 0), (1, 2), (1, 1), (3, 1)];
        for (i, &(cx, cy)) in SPOTS.iter().enumerate() {
            let s = scale_surface(first_slice(&level.surfaces[i], w, h), w, h, face, face, srgb);
            cross.paste(&s, face, face, cx * face, cy * face);
        }
        cross
    } else {
        let (tw, th) = target_size(w, h, size);
        let s = scale_surface(first_slice(&level.surfaces[0], w, h), w, h, tw, th, srgb);
        let mut c = Canvas::new(tw, th, s.is_hdr());
        c.paste(&s, tw, th, 0, 0);
        c
    };

    // 2. Turn it into opaque 8-bit colour.
    if channels == Channels::Data
        && let Pixels::Rgba32F(v) = &img.pixels
    {
        return stretch_data(v, &img.covered, img.width, img.height);
    }
    if channels == Channels::Stretch
        && let Pixels::Rgba32F(v) = &img.pixels
    {
        let mut pixels = crate::stretch::auto_render(v);
        for (p, &cov) in pixels.chunks_exact_mut(4).zip(&img.covered) {
            p[3] = if cov { 255 } else { 0 };
        }
        return Image8 { width: img.width, height: img.height, pixels };
    }
    match &mut img.pixels {
        Pixels::Rgba32F(v) => tone_map(v, &img.covered, channels),
        Pixels::Rgba8(v) => {
            for (p, &cov) in v.chunks_exact_mut(4).zip(&img.covered) {
                fix_channels_u8(p, channels);
                p[3] = if cov { 255 } else { 0 };
            }
        }
        // The canvas is always RGBA; `crate::thumbnail` decodes blocks.
        Pixels::Blocks(..) => {}
    }
    let pixels = match img.pixels {
        Pixels::Rgba8(v) => v,
        Pixels::Blocks(..) => vec![0; img.width as usize * img.height as usize * 4],
        Pixels::Rgba32F(v) => v
            .chunks_exact(4)
            .zip(&img.covered)
            .flat_map(|(p, &cov)| {
                [linear_to_srgb8(p[0]), linear_to_srgb8(p[1]), linear_to_srgb8(p[2]), if cov { 255 } else { 0 }]
            })
            .collect(),
    };
    Image8 { width: img.width, height: img.height, pixels }
}

/// The thumbnail size of a `w` x `h` picture: shrunk to fit `size`, or
/// enlarged by a whole factor when it is less than half of `size`, so tiny
/// textures keep square, even pixels.
fn target_size(w: u32, h: u32, size: u32) -> (u32, u32) {
    let long = w.max(h);
    if long > size {
        fit(w, h, size)
    } else {
        let k = (size / long).max(1);
        (w * k, h * k)
    }
}

/// The first `w` x `h` slice of a surface, which is the whole surface except
/// for 3D textures.
fn first_slice(p: &Pixels, w: u32, h: u32) -> Cow<'_, Pixels> {
    let n = w as usize * h as usize * 4;
    match p {
        Pixels::Rgba8(v) if v.len() > n => Cow::Owned(Pixels::Rgba8(v[..n].to_vec())),
        Pixels::Rgba32F(v) if v.len() > n => Cow::Owned(Pixels::Rgba32F(v[..n].to_vec())),
        _ => Cow::Borrowed(p),
    }
}

/// Shrinks with the box filter, or enlarges with nearest neighbour so pixel
/// edges stay hard.
fn scale_surface(p: Cow<'_, Pixels>, w: u32, h: u32, tw: u32, th: u32, srgb: bool) -> Pixels {
    if (tw, th) == (w, h) {
        p.into_owned()
    } else if tw >= w && th >= h {
        nearest(&p, w, h, tw, th)
    } else {
        resize(&p, w, h, 1, tw, th, srgb)
    }
}

fn nearest(p: &Pixels, w: u32, h: u32, tw: u32, th: u32) -> Pixels {
    let index = |x: u32, y: u32| {
        let sx = (x as u64 * w as u64 / tw as u64) as usize;
        let sy = (y as u64 * h as u64 / th as u64) as usize;
        (sy * w as usize + sx) * 4
    };
    let coords = (0..th).flat_map(|y| (0..tw).map(move |x| (x, y)));
    match p {
        Pixels::Rgba8(v) => Pixels::Rgba8(
            coords
                .flat_map(|(x, y)| {
                    let i = index(x, y);
                    [v[i], v[i + 1], v[i + 2], v[i + 3]]
                })
                .collect(),
        ),
        Pixels::Rgba32F(v) => Pixels::Rgba32F(
            coords
                .flat_map(|(x, y)| {
                    let i = index(x, y);
                    [v[i], v[i + 1], v[i + 2], v[i + 3]]
                })
                .collect(),
        ),
        Pixels::Blocks(..) => Pixels::Rgba8(vec![0; tw as usize * th as usize * 4]),
    }
}

/// A picture being assembled, with a note of which texels hold an image
/// (the rest of a cubemap's cross stays transparent).
struct Canvas {
    width: u32,
    height: u32,
    pixels: Pixels,
    covered: Vec<bool>,
}

impl Canvas {
    fn new(width: u32, height: u32, hdr: bool) -> Self {
        let n = width as usize * height as usize;
        let pixels = if hdr { Pixels::Rgba32F(vec![0.0; n * 4]) } else { Pixels::Rgba8(vec![0; n * 4]) };
        Canvas { width, height, pixels, covered: vec![false; n] }
    }

    fn paste(&mut self, src: &Pixels, w: u32, h: u32, x0: u32, y0: u32) {
        for y in 0..h.min(self.height.saturating_sub(y0)) {
            let cols = w.min(self.width.saturating_sub(x0)) as usize;
            let dst = ((y0 + y) * self.width + x0) as usize;
            let s = (y * w) as usize;
            match (&mut self.pixels, src) {
                (Pixels::Rgba8(d), Pixels::Rgba8(v)) => {
                    d[dst * 4..(dst + cols) * 4].copy_from_slice(&v[s * 4..(s + cols) * 4])
                }
                (Pixels::Rgba32F(d), Pixels::Rgba32F(v)) => {
                    d[dst * 4..(dst + cols) * 4].copy_from_slice(&v[s * 4..(s + cols) * 4])
                }
                _ => {}
            }
            self.covered[dst..dst + cols].fill(true);
        }
    }
}

/// Shows one channel of data as grey from its lowest value (black) to its
/// highest (white).
fn stretch_data(v: &[f32], covered: &[bool], width: u32, height: u32) -> Image8 {
    let (mut lo, mut hi) = (f32::INFINITY, f32::NEG_INFINITY);
    for (p, _) in v.chunks_exact(4).zip(covered).filter(|(p, c)| **c && p[0].is_finite()) {
        lo = lo.min(p[0]);
        hi = hi.max(p[0]);
    }
    let scale = if hi > lo { 1.0 / (hi - lo) } else { 0.0 };
    let pixels = v
        .chunks_exact(4)
        .zip(covered)
        .flat_map(|(p, &cov)| {
            let g = unorm8(if p[0].is_finite() { (p[0] - lo) * scale } else { 0.0 });
            [g, g, g, if cov { 255 } else { 0 }]
        })
        .collect();
    Image8 { width, height, pixels }
}

/// Makes one- and two-channel data readable: red alone becomes grey, and
/// red and green become a normal map's X and Y with Z rebuilt.
fn fix_channels_u8(p: &mut [u8], channels: Channels) {
    match channels {
        Channels::R | Channels::Data => {
            p[1] = p[0];
            p[2] = p[0];
        }
        Channels::Rg => {
            let x = p[0] as f32 / 127.5 - 1.0;
            let y = p[1] as f32 / 127.5 - 1.0;
            let z = (1.0 - x * x - y * y).max(0.0).sqrt();
            p[2] = unorm8(z * 0.5 + 0.5);
        }
        Channels::A => {
            p[0] = p[3];
            p[1] = p[3];
            p[2] = p[3];
        }
        Channels::Rgba | Channels::Stretch => {}
    }
}

/// Exposes linear HDR so its log-average luminance lands on middle grey,
/// then applies PBR Neutral.  The result stays linear, for sRGB encoding.
fn tone_map(v: &mut [f32], covered: &[bool], channels: Channels) {
    for p in v.chunks_exact_mut(4) {
        for c in &mut p[..3] {
            // Negative values (signed BC6H) and NaN show as black; infinity
            // as the brightest finite value.
            *c = if c.is_nan() || *c < 0.0 { 0.0 } else { c.min(65504.0) };
        }
        match channels {
            Channels::R | Channels::Data => {
                p[1] = p[0];
                p[2] = p[0];
            }
            Channels::Rg => {
                let (x, y) = (p[0] * 2.0 - 1.0, p[1] * 2.0 - 1.0);
                p[2] = (1.0 - x * x - y * y).max(0.0).sqrt() * 0.5 + 0.5;
            }
            Channels::A => {
                let a = p[3];
                p[..3].fill(a);
            }
            Channels::Rgba | Channels::Stretch => {}
        }
    }
    let (mut sum, mut n) = (0.0f64, 0usize);
    for (p, _) in v.chunks_exact(4).zip(covered).filter(|(_, c)| **c) {
        sum += (luminance(p[0], p[1], p[2]).max(1e-4) as f64).ln();
        n += 1;
    }
    let average = if n > 0 { (sum / n as f64).exp() as f32 } else { 1.0 };
    let exposure = (EXPOSURE_KEY / average).clamp(1.0 / 4096.0, 4096.0);
    for p in v.chunks_exact_mut(4) {
        let c = pbr_neutral([p[0] * exposure, p[1] * exposure, p[2] * exposure]);
        p[..3].copy_from_slice(&c);
    }
}

/// Reads the channel count back from the format name a decoder stored in
/// `SourceInfo::format`.  Every decoder names its formats from the tables in
/// this crate, so the names are known.
pub(crate) fn channels_of(format: &str) -> Channels {
    crate::formats::channels_by_name(format).unwrap_or(Channels::Rgba)
}
