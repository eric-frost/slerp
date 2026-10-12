//! The pictures tables are shown on: two bundled CC0 photos (see
//! `images/README.md`), a colour sweep and a colour chart drawn here, and
//! the before-and-after splits.

use slerp_scene::textures::{ColorSpace, DecodeOptions, Texture};
use slerp_scene::{ColorLut, Image8};
use std::sync::OnceLock;

/// A picture to grade, with its name for the viewer's list.
pub struct TestImage {
    pub name: &'static str,
    pub image: Image8,
}

const MARKET: &[u8] = include_bytes!("../images/market.jpg");
const PORTRAIT: &[u8] = include_bytes!("../images/portrait.jpg");

fn decode(jpeg: &[u8]) -> Image8 {
    let tex = slerp_scene::textures::decode(jpeg, &DecodeOptions::full()).ok();
    match tex {
        Some(t) => slerp_scene::textures::thumbnail(&t, t.width().max(t.height())),
        // Bundled files decode; this only keeps a broken build from
        // panicking.
        None => Image8::new(1, 1),
    }
}

/// The photo thumbnails show: a fruit seller's stall, with skin, orange,
/// greens, red and blue denim and neutral metal, 512 x 341.
pub fn reference_photo() -> &'static Image8 {
    static PHOTO: OnceLock<Image8> = OnceLock::new();
    PHOTO.get_or_init(|| decode(MARKET))
}

/// Every picture the viewer offers, the reference photo first.
pub fn test_images() -> &'static [TestImage] {
    static IMAGES: OnceLock<Vec<TestImage>> = OnceLock::new();
    IMAGES.get_or_init(|| {
        vec![
            TestImage { name: "Market", image: reference_photo().clone() },
            TestImage { name: "Portrait", image: decode(PORTRAIT) },
            TestImage { name: "Colour sweep", image: sweep(512, 341) },
            TestImage { name: "Colour chart", image: chart(512, 341) },
        ]
    })
}

/// The reference photo at most `size` pixels across, never enlarged.
pub(crate) fn photo_at(size: u32) -> Image8 {
    let photo = reference_photo();
    if size >= photo.width.max(photo.height) {
        return photo.clone();
    }
    let tex = Texture::from_rgba8(photo.width, photo.height, photo.pixels.clone(), ColorSpace::Srgb);
    slerp_scene::textures::thumbnail(&tex, size.max(1))
}

/// Grades a copy of `image`.
pub fn graded(lut: &dyn ColorLut, image: &Image8) -> Image8 {
    let mut out = image.clone();
    lut.apply(&mut out.pixels);
    out
}

/*┌─────────────────╮
| Generated       |
└─────────────────╯*/
//region Generated

fn hsv(h: f32, s: f32, v: f32) -> [u8; 3] {
    let h = h.rem_euclid(1.0) * 6.0;
    let f = h - h.floor();
    let (p, q, t) = (v * (1.0 - s), v * (1.0 - s * f), v * (1.0 - s * (1.0 - f)));
    let (r, g, b) = match h as u32 {
        0 => (v, t, p),
        1 => (q, v, p),
        2 => (p, v, t),
        3 => (p, q, v),
        4 => (t, p, v),
        _ => (v, p, q),
    };
    [r, g, b].map(|c| (c * 255.0 + 0.5) as u8)
}

/// Every hue from left to right, pale at the top through full colour in
/// the middle to black near the bottom, over a grey ramp, so a table's
/// effect on any colour and on neutrals shows at a glance.
fn sweep(width: u32, height: u32) -> Image8 {
    let mut img = Image8::new(width, height);
    let band = height * 5 / 6;
    for y in 0..height {
        for x in 0..width {
            let u = x as f32 / (width - 1) as f32;
            let rgb = if y < band {
                let t = y as f32 / (band - 1) as f32;
                if t < 0.5 { hsv(u, t * 2.0, 1.0) } else { hsv(u, 1.0, 1.0 - (t - 0.5) * 2.0) }
            } else {
                let g = (u * 255.0 + 0.5) as u8;
                [g, g, g]
            };
            let i = ((y * width + x) * 4) as usize;
            img.pixels[i..i + 4].copy_from_slice(&[rgb[0], rgb[1], rgb[2], 255]);
        }
    }
    img
}

/// The 24 patches of the ColorChecker chart, in their usual sRGB values.
const PATCHES: [[u8; 3]; 24] = [
    [115, 82, 68],
    [194, 150, 130],
    [98, 122, 157],
    [87, 108, 67],
    [133, 128, 177],
    [103, 189, 170],
    [214, 126, 44],
    [80, 91, 166],
    [193, 90, 99],
    [94, 60, 108],
    [157, 188, 64],
    [224, 163, 46],
    [56, 61, 150],
    [70, 148, 73],
    [175, 54, 60],
    [231, 199, 31],
    [187, 86, 149],
    [8, 133, 161],
    [243, 243, 242],
    [200, 200, 200],
    [160, 160, 160],
    [122, 122, 121],
    [85, 85, 85],
    [52, 52, 52],
];

fn chart(width: u32, height: u32) -> Image8 {
    let mut img = Image8::new(width, height);
    for p in img.pixels.chunks_exact_mut(4) {
        p.copy_from_slice(&[24, 24, 24, 255]);
    }
    let gap = width / 64;
    let cell_w = (width - gap * 7) / 6;
    let cell_h = (height - gap * 5) / 4;
    for (i, rgb) in PATCHES.iter().enumerate() {
        let (cx, cy) = (i as u32 % 6, i as u32 / 6);
        let (x0, y0) = (gap + cx * (cell_w + gap), gap + cy * (cell_h + gap));
        for y in y0..(y0 + cell_h).min(height) {
            for x in x0..(x0 + cell_w).min(width) {
                let at = ((y * width + x) * 4) as usize;
                img.pixels[at..at + 3].copy_from_slice(rgb);
            }
        }
    }
    img
}

//endregion

/*┌─────────────────╮
| Splits          |
└─────────────────╯*/
//region Splits

/// `original` at the top left and `graded` at the bottom right of the
/// diagonal from the top right corner to the bottom left, with a thin
/// light line along it.
pub fn split_diagonal(original: &Image8, graded: &Image8) -> Image8 {
    let (w, h) = (original.width as f32, original.height as f32);
    let len = (w * w + h * h).sqrt();
    split(original, graded, |x, y| (h * x + w * y - w * h) / len)
}

/// `original` left of `at` (a fraction of the width) and `graded` right of
/// it, with a thin light line between.
pub fn split_vertical(original: &Image8, graded: &Image8, at: f32) -> Image8 {
    let line = at.clamp(0.0, 1.0) * original.width as f32;
    split(original, graded, |x, _| x - line)
}

/// Picks each pixel by the sign of `side` (its signed distance in pixels
/// from the dividing line, negative for the original), with a soft line
/// about 1.5 pixels wide.
fn split(original: &Image8, graded: &Image8, side: impl Fn(f32, f32) -> f32) -> Image8 {
    let mut out = original.clone();
    if graded.pixels.len() != original.pixels.len() {
        return out;
    }
    let w = original.width as usize;
    for (i, p) in out.pixels.chunks_exact_mut(4).enumerate() {
        let (x, y) = ((i % w) as f32 + 0.5, (i / w) as f32 + 0.5);
        let d = side(x, y);
        if d > 0.0 {
            p.copy_from_slice(&graded.pixels[i * 4..i * 4 + 4]);
        }
        let line = (1.25 - d.abs()).clamp(0.0, 1.0) * 0.85;
        if line > 0.0 {
            for c in &mut p[..3] {
                *c = (*c as f32 + (255.0 - *c as f32) * line + 0.5) as u8;
            }
            p[3] = p[3].max((line * 255.0) as u8);
        }
    }
    out
}

//endregion
