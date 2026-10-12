//! Downscaling with an area-averaging (box) filter, in linear light: sRGB
//! bytes are decoded before averaging and encoded again after, so fine
//! detail doesn't darken.

use crate::Pixels;
use crate::color::{linear_to_srgb8, srgb_to_linear_table, unorm8};
use rayon::prelude::*;

/// The size that fits `w` x `h` within `max` on its longer side, keeping the
/// aspect ratio.  Never below 1.
pub(crate) fn fit(w: u32, h: u32, max: u32) -> (u32, u32) {
    let long = w.max(h);
    if long <= max {
        return (w, h);
    }
    let s = max as f64 / long as f64;
    (((w as f64 * s).round() as u32).clamp(1, max), ((h as f64 * s).round() as u32).clamp(1, max))
}

/// For each output column, the first source column and the weights of the
/// source columns it covers, which sum to 1.
struct Taps {
    start: Vec<u32>,
    offset: Vec<u32>,
    weights: Vec<f32>,
}

impl Taps {
    fn new(src: u32, dst: u32) -> Self {
        let scale = src as f64 / dst as f64;
        let mut t = Taps {
            start: Vec::with_capacity(dst as usize),
            offset: Vec::with_capacity(dst as usize + 1),
            weights: Vec::new(),
        };
        for i in 0..dst {
            let a = i as f64 * scale;
            let b = ((i + 1) as f64 * scale).min(src as f64);
            let first = (a.floor() as u32).min(src - 1);
            let last = (b.ceil() as u32).clamp(first + 1, src);
            t.start.push(first);
            t.offset.push(t.weights.len() as u32);
            let mut sum = 0.0;
            for s in first..last {
                let w = (b.min(s as f64 + 1.0) - a.max(s as f64)).max(0.0);
                t.weights.push(w as f32);
                sum += w;
            }
            // Upscaling covers less than one texel; normalise either way.
            let n = t.weights.len();
            let k = t.offset[i as usize] as usize;
            if sum > 0.0 {
                for w in &mut t.weights[k..n] {
                    *w = (*w as f64 / sum) as f32;
                }
            } else {
                t.weights[k] = 1.0;
            }
        }
        t.offset.push(t.weights.len() as u32);
        t
    }

    fn get(&self, i: usize) -> (usize, &[f32]) {
        (self.start[i] as usize, &self.weights[self.offset[i] as usize..self.offset[i + 1] as usize])
    }
}

/// Resamples one surface (or each slice of a 3D one) to `dw` x `dh`.  With
/// `srgb`, 8-bit colour channels are averaged in linear light.
pub(crate) fn resize(pixels: &Pixels, w: u32, h: u32, depth: u32, dw: u32, dh: u32, srgb: bool) -> Pixels {
    let slice_in = w as usize * h as usize * 4;
    match pixels {
        Pixels::Rgba8(src) => {
            let lut = srgb_to_linear_table();
            let mut out = vec![0u8; dw as usize * dh as usize * 4 * depth as usize];
            for (z, dst) in out.chunks_exact_mut(dw as usize * dh as usize * 4).enumerate() {
                let s = &src[z * slice_in..(z + 1) * slice_in];
                let read = |i: usize, c: usize| -> f32 {
                    let v = s[i * 4 + c];
                    if srgb && c < 3 { lut[v as usize] } else { v as f32 * (1.0 / 255.0) }
                };
                let write = |o: &mut [u8], v: [f32; 4]| {
                    for c in 0..4 {
                        o[c] = if srgb && c < 3 { linear_to_srgb8(v[c]) } else { unorm8(v[c]) };
                    }
                };
                resample(w, h, dw, dh, read, dst, 4, write);
            }
            Pixels::Rgba8(out)
        }
        Pixels::Rgba32F(src) => {
            let mut out = vec![0f32; dw as usize * dh as usize * 4 * depth as usize];
            for (z, dst) in out.chunks_exact_mut(dw as usize * dh as usize * 4).enumerate() {
                let s = &src[z * slice_in..(z + 1) * slice_in];
                let read = |i: usize, c: usize| -> f32 {
                    let v = s[i * 4 + c];
                    // Infinities and NaN would smear across the whole box.
                    if v.is_finite() { v } else { 0.0 }
                };
                resample(w, h, dw, dh, read, dst, 4, |o: &mut [f32], v: [f32; 4]| o[..4].copy_from_slice(&v));
            }
            Pixels::Rgba32F(out)
        }
        Pixels::Blocks(format, data) => match crate::blocks::decode_gpu_blocks(*format, w, h, data, srgb) {
            Ok(rgba) if depth == 1 => resize(&rgba, w, h, 1, dw, dh, srgb),
            _ => Pixels::Rgba8(vec![0; dw as usize * dh as usize * 4 * depth as usize]),
        },
    }
}

/// The separable box filter behind `resize`, run in parallel over output
/// rows.  `read(texel, channel)` gives linear source values; `write` stores
/// one averaged texel.
#[allow(clippy::too_many_arguments)]
fn resample<T: Send, R, W>(w: u32, h: u32, dw: u32, dh: u32, read: R, dst: &mut [T], stride: usize, write: W)
where
    R: Fn(usize, usize) -> f32 + Sync,
    W: Fn(&mut [T], [f32; 4]) + Sync,
{
    let xt = Taps::new(w, dw);
    let yt = Taps::new(h, dh);
    dst.par_chunks_mut(dw as usize * stride).enumerate().for_each(|(y, out_row)| {
        let mut acc = vec![[0f32; 4]; dw as usize];
        let (y0, yw) = yt.get(y);
        for (j, &wy) in yw.iter().enumerate() {
            let row = (y0 + j) * w as usize;
            for (x, a) in acc.iter_mut().enumerate() {
                let (x0, xw) = xt.get(x);
                let mut v = [0f32; 4];
                for (i, &wx) in xw.iter().enumerate() {
                    let t = row + x0 + i;
                    for (c, vc) in v.iter_mut().enumerate() {
                        *vc += read(t, c) * wx;
                    }
                }
                for c in 0..4 {
                    a[c] += v[c] * wy;
                }
            }
        }
        for (x, a) in acc.iter().enumerate() {
            write(&mut out_row[x * stride..(x + 1) * stride], *a);
        }
    });
}

/// The whole factor that shrinks `long` to no less than `min`, for decoders
/// that average boxes of texels while reading (Radiance HDR, large PNGs).
pub(crate) fn reduce_factor(long: u32, min: u32) -> u32 {
    (long / min.max(1)).max(1)
}

/// Converts linear float RGBA to 8-bit, sRGB-encoding the colour channels
/// when `srgb` is set.
pub(crate) fn to_rgba8(v: &[f32], srgb: bool) -> Vec<u8> {
    v.chunks_exact(4)
        .flat_map(|p| {
            if srgb {
                [linear_to_srgb8(p[0]), linear_to_srgb8(p[1]), linear_to_srgb8(p[2]), unorm8(p[3])]
            } else {
                [unorm8(p[0]), unorm8(p[1]), unorm8(p[2]), unorm8(p[3])]
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fits() {
        assert_eq!(fit(4096, 2048, 256), (256, 128));
        assert_eq!(fit(100, 50, 256), (100, 50));
        assert_eq!(fit(10000, 3, 256), (256, 1));
    }

    #[test]
    fn box_average_is_linear_light() {
        // Black and white texels average to linear grey 0.5, which is sRGB
        // byte 188, not 128.
        let src = Pixels::Rgba8(vec![0, 0, 0, 255, 255, 255, 255, 255]);
        let Pixels::Rgba8(out) = resize(&src, 2, 1, 1, 1, 1, true) else { panic!() };
        assert_eq!(out, vec![188, 188, 188, 255]);
        let Pixels::Rgba8(out) = resize(&src, 2, 1, 1, 1, 1, false) else { panic!() };
        assert_eq!(out, vec![128, 128, 128, 255]);
    }

    #[test]
    fn uneven_factor_keeps_mean() {
        let src: Vec<f32> = (0..7 * 4).map(|i| (i / 4) as f32).collect();
        let Pixels::Rgba32F(out) = resize(&Pixels::Rgba32F(src), 7, 1, 1, 3, 1, false) else { panic!() };
        let mean: f32 = out.chunks(4).map(|p| p[0]).sum::<f32>() / 3.0;
        assert!((mean - 3.0).abs() < 1e-4, "{out:?}");
    }

    #[test]
    fn reduce_factors() {
        assert_eq!(reduce_factor(8192, 256), 32);
        assert_eq!(reduce_factor(8000, 256), 31);
        assert_eq!(reduce_factor(100, 256), 1);
        assert_eq!(reduce_factor(100, 0), 100);
    }
}
