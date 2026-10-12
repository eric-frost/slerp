//! Palettes and the 8-bit picture helpers every format here shares: palette
//! lookup, Quake's fullbright colours, transparent texels, downscaling to
//! `Limits::max_texture_size`, and wrapping the result in a `Texture`.
//!
//! The Quake palette is LibreQuake's `gfx/palette.lmp`, and the Doom palette
//! is the first PLAYPAL entry of Freedoom 0.13's `freedoom2.wad`.  Both
//! projects license these files under the BSD 3-clause licence, reproduced
//! here as it requires:
//!
//! Copyright (c) 2019-2023 Contributors to the LibreQuake project.
//! Copyright (c) 2001-2024 Contributors to the Freedoom project.
//! All rights reserved.
//!
//! Redistribution and use in source and binary forms, with or without
//! modification, are permitted provided that the following conditions are
//! met:
//!
//! * Redistributions of source code must retain the above copyright notice,
//!   this list of conditions and the following disclaimer.
//! * Redistributions in binary form must reproduce the above copyright
//!   notice, this list of conditions and the following disclaimer in the
//!   documentation and/or other materials provided with the distribution.
//! * Neither the name of the LibreQuake project or the Freedoom project nor
//!   the names of their contributors may be used to endorse or promote
//!   products derived from this software without specific prior written
//!   permission.
//!
//! THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS "AS
//! IS" AND ANY EXPRESS OR IMPLIED WARRANTIES, INCLUDING, BUT NOT LIMITED
//! TO, THE IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS FOR A
//! PARTICULAR PURPOSE ARE DISCLAIMED.  IN NO EVENT SHALL THE COPYRIGHT OWNER
//! OR CONTRIBUTORS BE LIABLE FOR ANY DIRECT, INDIRECT, INCIDENTAL, SPECIAL,
//! EXEMPLARY, OR CONSEQUENTIAL DAMAGES (INCLUDING, BUT NOT LIMITED TO,
//! PROCUREMENT OF SUBSTITUTE GOODS OR SERVICES; LOSS OF USE, DATA, OR
//! PROFITS; OR BUSINESS INTERRUPTION) HOWEVER CAUSED AND ON ANY THEORY OF
//! LIABILITY, WHETHER IN CONTRACT, STRICT LIABILITY, OR TORT (INCLUDING
//! NEGLIGENCE OR OTHERWISE) ARISING IN ANY WAY OUT OF THE USE OF THIS
//! SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.

use crate::read::Result;
use slerp_scene::{ColorSpace, LoadError, Texture};

/// Quake's 256 colours, three bytes each.
pub static QUAKE_PALETTE: &[u8; 768] = include_bytes!("../data/quake.pal");

/// Doom's 256 colours, for PWADs that don't carry their own PLAYPAL.
pub static DOOM_PALETTE: &[u8; 768] = include_bytes!("../data/doom.pal");

/// Quake draws palette entries from here up at full brightness whatever the
/// lighting, which is how lamps, runes and eyes glow.
pub const QUAKE_FULLBRIGHT: u8 = 224;

/// The largest side any 8-bit picture here may have.  Quake engines stop
/// at 4096; this leaves room for modern WAD3 textures while keeping a corrupt
/// size from asking for gigabytes.
pub const MAX_SIDE: u32 = 8192;

/// Checks a picture's size before anything is allocated for it.
pub fn check_size(width: u32, height: u32) -> Result<usize> {
    if width == 0 || height == 0 || width > MAX_SIDE || height > MAX_SIDE {
        return Err(LoadError::invalid(format!("picture size {width}x{height} is out of range")));
    }
    Ok(width as usize * height as usize)
}

/// Looks every index up in `palette` (768 bytes).  Texels whose index is
/// `transparent` become fully transparent, with their colour bled in from
/// opaque neighbours so filtering doesn't darken the edges.
pub fn expand(indices: &[u8], width: u32, height: u32, palette: &[u8], transparent: Option<u8>) -> Vec<u8> {
    let mut out = Vec::with_capacity(indices.len() * 4);
    for &i in indices {
        let c = &palette[i as usize * 3..i as usize * 3 + 3];
        let a = if Some(i) == transparent { 0 } else { 255 };
        out.extend_from_slice(&[c[0], c[1], c[2], a]);
    }
    if transparent.is_some_and(|t| indices.contains(&t)) {
        bleed(&mut out, width, height);
    }
    out
}

/// The emissive picture for Quake's fullbright colours: those texels keep
/// their colour and the rest are black.  `None` when nothing glows.
pub fn fullbrights(indices: &[u8], palette: &[u8], transparent: Option<u8>) -> Option<Vec<u8>> {
    let glows = |i: u8| i >= QUAKE_FULLBRIGHT && Some(i) != transparent;
    if !indices.iter().any(|&i| glows(i)) {
        return None;
    }
    let mut out = Vec::with_capacity(indices.len() * 4);
    for &i in indices {
        if glows(i) {
            let c = &palette[i as usize * 3..i as usize * 3 + 3];
            out.extend_from_slice(&[c[0], c[1], c[2], 255]);
        } else {
            out.extend_from_slice(&[0, 0, 0, 255]);
        }
    }
    Some(out)
}

/// Gives each fully transparent texel the average colour of its opaque
/// neighbours (or black), so bilinear filtering and mipmaps don't pull a
/// key colour such as Half-Life's blue into the edges.
pub fn bleed(rgba: &mut [u8], width: u32, height: u32) {
    let (w, h) = (width as usize, height as usize);
    if rgba.len() < w * h * 4 {
        return;
    }
    let src = rgba.to_vec();
    for y in 0..h {
        for x in 0..w {
            let i = (y * w + x) * 4;
            if src[i + 3] != 0 {
                continue;
            }
            let mut sum = [0u32; 3];
            let mut n = 0;
            for dy in [-1i32, 0, 1] {
                for dx in [-1i32, 0, 1] {
                    let (nx, ny) = (x as i32 + dx, y as i32 + dy);
                    if nx < 0 || ny < 0 || nx >= w as i32 || ny >= h as i32 {
                        continue;
                    }
                    let j = (ny as usize * w + nx as usize) * 4;
                    if src[j + 3] != 0 {
                        sum[0] += src[j] as u32;
                        sum[1] += src[j + 1] as u32;
                        sum[2] += src[j + 2] as u32;
                        n += 1;
                    }
                }
            }
            for c in 0..3 {
                rgba[i + c] = if n > 0 { (sum[c] / n) as u8 } else { 0 };
            }
        }
    }
}

/// The size a `width` x `height` picture becomes when its longer side is
/// limited to `max`.
pub fn fit(width: u32, height: u32, max: Option<u32>) -> (u32, u32) {
    match max {
        Some(max) if max > 0 && width.max(height) > max => {
            let scale = max as f64 / width.max(height) as f64;
            (((width as f64 * scale).round() as u32).max(1), ((height as f64 * scale).round() as u32).max(1))
        }
        _ => (width, height),
    }
}

/// Shrinks an RGBA picture by averaging the area each new texel covers.
/// Colour is weighted by alpha so transparent texels don't darken edges.
/// Returns the input unchanged when it already fits.
pub fn downscale(rgba: Vec<u8>, width: u32, height: u32, max: Option<u32>) -> (u32, u32, Vec<u8>) {
    let (nw, nh) = fit(width, height, max);
    if (nw, nh) == (width, height) || rgba.len() < width as usize * height as usize * 4 {
        return (width, height, rgba);
    }
    // Premultiply, filter rows then columns, and divide back out.
    let mut src: Vec<f32> = Vec::with_capacity(rgba.len());
    for p in rgba.chunks_exact(4) {
        let a = p[3] as f32 / 255.0;
        src.extend_from_slice(&[p[0] as f32 * a, p[1] as f32 * a, p[2] as f32 * a, p[3] as f32]);
    }
    let rows = resample(&src, width as usize, height as usize, nw as usize, true);
    let both = resample(&rows, nw as usize, height as usize, nh as usize, false);
    let mut out = Vec::with_capacity(nw as usize * nh as usize * 4);
    for p in both.chunks_exact(4) {
        let a = p[3] / 255.0;
        let un = |v: f32| if a > 0.0 { (v / a).round().clamp(0.0, 255.0) as u8 } else { 0 };
        out.extend_from_slice(&[un(p[0]), un(p[1]), un(p[2]), p[3].round().clamp(0.0, 255.0) as u8]);
    }
    (nw, nh, out)
}

/// Box-filters one axis of a four-channel float picture from its current
/// length to `to`, along rows when `horizontal`, else along columns.
fn resample(src: &[f32], width: usize, height: usize, to: usize, horizontal: bool) -> Vec<f32> {
    let from = if horizontal { width } else { height };
    let (ow, oh) = if horizontal { (to, height) } else { (width, to) };
    let mut out = vec![0.0f32; ow * oh * 4];
    let step = from as f64 / to as f64;
    for o in 0..to {
        let (start, end) = (o as f64 * step, (o + 1) as f64 * step);
        let mut s = start.floor() as usize;
        while (s as f64) < end && s < from {
            let weight = ((s + 1) as f64).min(end) - (s as f64).max(start);
            let weight = (weight / step) as f32;
            if horizontal {
                for y in 0..height {
                    let (si, oi) = ((y * width + s) * 4, (y * ow + o) * 4);
                    for c in 0..4 {
                        out[oi + c] += src[si + c] * weight;
                    }
                }
            } else {
                for x in 0..width {
                    let (si, oi) = ((s * width + x) * 4, (o * width + x) * 4);
                    for c in 0..4 {
                        out[oi + c] += src[si + c] * weight;
                    }
                }
            }
            s += 1;
        }
    }
    out
}

/// Wraps RGBA texels in a `Texture`, downscaled to `max` first, and records
/// where it came from for the viewer's inspector.
pub fn texture(width: u32, height: u32, rgba: Vec<u8>, max: Option<u32>, container: &str, format: &str) -> Texture {
    let (w, h, rgba) = downscale(rgba, width, height, max);
    let mut tex = Texture::from_rgba8(w, h, rgba, ColorSpace::Srgb);
    tex.source.container = container.to_string();
    tex.source.format = format.to_string();
    tex.source.width = width;
    tex.source.height = height;
    tex
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn palettes_are_the_known_ones() {
        // Quake and Freedoom both start with black; Quake's last entry is
        // the pinkish colour engines use as a transparency key.
        assert_eq!(&QUAKE_PALETTE[..6], &[0, 0, 0, 15, 15, 15]);
        assert_eq!(&QUAKE_PALETTE[765..], &[159, 91, 83]);
        assert_eq!(&DOOM_PALETTE[..3], &[0, 0, 0]);
    }

    #[test]
    fn transparent_texels_take_neighbour_colours() {
        let indices = [15u8, 255, 15, 15];
        let rgba = expand(&indices, 2, 2, QUAKE_PALETTE, Some(255));
        assert_eq!(rgba[7], 0);
        assert_eq!(&rgba[4..7], &rgba[0..3]);
    }

    #[test]
    fn downscale_averages_and_keeps_aspect() {
        let mut rgba = Vec::new();
        for i in 0..16 {
            let v = if i % 2 == 0 { 0 } else { 200 };
            rgba.extend_from_slice(&[v, v, v, 255]);
        }
        let (w, h, out) = downscale(rgba, 8, 2, Some(4));
        assert_eq!((w, h), (4, 1));
        assert!(out.chunks_exact(4).all(|p| p[0] == 100 && p[3] == 255));
        assert_eq!(fit(300, 100, Some(512)), (300, 100));
        assert_eq!(fit(1024, 100, Some(512)), (512, 50));
    }

    #[test]
    fn fullbright_mask_only_glowing_colours() {
        let fb = fullbrights(&[0, 230, 255], QUAKE_PALETTE, Some(255)).unwrap();
        assert_eq!(&fb[0..3], &[0, 0, 0]);
        assert_ne!(&fb[4..7], &[0, 0, 0]);
        assert_eq!(&fb[8..11], &[0, 0, 0]);
        assert!(fullbrights(&[0, 1, 2], QUAKE_PALETTE, None).is_none());
    }
}
