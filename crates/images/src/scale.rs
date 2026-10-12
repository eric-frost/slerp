//! Shrinking pictures by averaging, for thumbnails and for animations too
//! large to show at full size.

use rayon::prelude::*;

/// The size a `w` by `h` picture has at `scale` (at most 1), never less than
/// one pixel in either direction.
pub fn scaled_size(w: u32, h: u32, scale: f32) -> (u32, u32) {
    let s = if scale.is_finite() { scale.clamp(0.0, 1.0) } else { 1.0 };
    (((w as f32 * s).round() as u32).clamp(1, w.max(1)), ((h as f32 * s).round() as u32).clamp(1, h.max(1)))
}

/// Shrinks straight-alpha RGBA to `ow` by `oh` pixels, averaging every
/// source pixel into the output pixel that covers it, weighted by alpha so
/// the colour of transparent pixels doesn't bleed in.
pub fn box_shrink(src: &[u8], w: u32, h: u32, ow: u32, oh: u32) -> Vec<u8> {
    let (w, h, ow, oh) = (w as usize, h as usize, ow as usize, oh as usize);
    if (ow, oh) == (w, h) || ow == 0 || oh == 0 || src.len() != w * h * 4 {
        return src.to_vec();
    }
    let mut out = vec![0u8; ow * oh * 4];
    out.par_chunks_mut(ow * 4).enumerate().for_each(|(oy, row)| {
        let y0 = oy * h / oh;
        let y1 = (((oy + 1) * h).div_ceil(oh)).clamp(y0 + 1, h);
        for (ox, px) in row.chunks_exact_mut(4).enumerate() {
            let x0 = ox * w / ow;
            let x1 = (((ox + 1) * w).div_ceil(ow)).clamp(x0 + 1, w);
            let (mut r, mut g, mut b, mut a) = (0u64, 0u64, 0u64, 0u64);
            for y in y0..y1 {
                for p in src[(y * w + x0) * 4..(y * w + x1) * 4].chunks_exact(4) {
                    let pa = p[3] as u64;
                    r += p[0] as u64 * pa;
                    g += p[1] as u64 * pa;
                    b += p[2] as u64 * pa;
                    a += pa;
                }
            }
            let n = ((y1 - y0) * (x1 - x0)) as u64;
            if a > 0 {
                px[0] = ((r + a / 2) / a) as u8;
                px[1] = ((g + a / 2) / a) as u8;
                px[2] = ((b + a / 2) / a) as u8;
            }
            px[3] = ((a + n / 2) / n) as u8;
        }
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes() {
        assert_eq!(scaled_size(100, 50, 0.5), (50, 25));
        assert_eq!(scaled_size(100, 50, 2.0), (100, 50));
        assert_eq!(scaled_size(100, 50, 0.0), (1, 1));
        assert_eq!(scaled_size(100, 50, f32::NAN), (100, 50));
    }

    #[test]
    fn averages_and_weights_by_alpha() {
        // Two opaque pixels, black and white, average to grey.
        let src = [0, 0, 0, 255, 255, 255, 255, 255];
        assert_eq!(box_shrink(&src, 2, 1, 1, 1), [128, 128, 128, 255]);
        // A transparent red pixel does not tint an opaque blue one.
        let src = [255, 0, 0, 0, 0, 0, 255, 255];
        assert_eq!(box_shrink(&src, 2, 1, 1, 1), [0, 0, 255, 128]);
        // Odd sizes cover every source pixel.
        let src: Vec<u8> = (0..3 * 3).flat_map(|_| [10, 20, 30, 255]).collect();
        assert_eq!(box_shrink(&src, 3, 3, 2, 2), [10, 20, 30, 255].repeat(4));
    }

    #[test]
    fn nonsense_is_returned_unchanged() {
        let src = [1, 2, 3, 4];
        assert_eq!(box_shrink(&src, 2, 2, 1, 1), src);
        assert_eq!(box_shrink(&src, 1, 1, 1, 1), src);
    }
}
