//! Colour filter array ("Bayer") sensors: one value per pixel, each behind
//! a red, green or blue filter.  Astronomy cameras and DSLR raw frames
//! store the mosaic as it is read out.
//!
//! Large pictures only need a thumbnail, and for that each 2x2 cell is
//! simply one RGB pixel (or the average of several cells), which keeps the
//! colours exact and costs nothing.  A picture shown at full size gets
//! bilinear interpolation, the simplest demosaic that doesn't visibly
//! stair-step.

use crate::reduce::Raster;
use rayon::prelude::*;

/// The colour of each pixel of a repeating 2x2 cell, row by row: 0 red,
/// 1 green, 2 blue.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Pattern {
    cell: [u8; 4],
    /// The mosaic's own shift: the column and row of the cell the picture's
    /// top left pixel is in (FITS `XBAYROFF` and `YBAYROFF`).
    x_offset: u8,
    y_offset: u8,
}

impl Pattern {
    /// Reads "RGGB", "BGGR", "GRBG" or "GBRG" (any case, other letters
    /// refuse).
    pub fn parse(name: &str, x_offset: i64, y_offset: i64) -> Option<Pattern> {
        let letters: Vec<u8> = name.trim().bytes().map(|b| b.to_ascii_uppercase()).collect();
        if letters.len() != 4 {
            return None;
        }
        let mut cell = [0u8; 4];
        for (c, l) in cell.iter_mut().zip(&letters) {
            *c = match l {
                b'R' => 0,
                b'G' => 1,
                b'B' => 2,
                _ => return None,
            };
        }
        // A cell needs both greens and one each of red and blue.
        let count = |k: u8| cell.iter().filter(|&&c| c == k).count();
        if count(0) != 1 || count(1) != 2 || count(2) != 1 {
            return None;
        }
        Some(Pattern { cell, x_offset: (x_offset & 1) as u8, y_offset: (y_offset & 1) as u8 })
    }

    pub fn name(&self) -> String {
        self.cell.iter().map(|c| ['R', 'G', 'B'][*c as usize]).collect()
    }

    /// The colour of the pixel at column `x`, row `y` of the picture.
    pub fn colour(&self, x: usize, y: usize) -> usize {
        let col = (x + self.x_offset as usize) & 1;
        let row = (y + self.y_offset as usize) & 1;
        self.cell[row * 2 + col] as usize
    }
}

/// Bilinear demosaic of a whole mosaic (`width` x `height` values, rows top
/// to bottom, NaN for undefined) into a raster.
pub(crate) fn demosaic(mosaic: &[f32], width: usize, height: usize, pattern: &Pattern, out: &mut Raster) {
    debug_assert_eq!(mosaic.len(), width * height);
    out.rgba.par_chunks_mut(width * 4).enumerate().for_each(|(y, row)| {
        for (x, px) in row.chunks_exact_mut(4).enumerate() {
            let mut sum = [0.0f32; 3];
            let mut n = [0u32; 3];
            for yy in y.saturating_sub(1)..(y + 2).min(height) {
                for xx in x.saturating_sub(1)..(x + 2).min(width) {
                    let v = mosaic[yy * width + xx];
                    if v.is_nan() {
                        continue;
                    }
                    let c = pattern.colour(xx, yy);
                    sum[c] += v;
                    n[c] += 1;
                }
            }
            if n.iter().all(|&k| k == 0) {
                px[3] = 0.0;
                continue;
            }
            // A channel with no sample in reach (a corner with undefined
            // neighbours) borrows the average of the others.
            let known: f32 = (0..3).filter(|&c| n[c] > 0).map(|c| sum[c] / n[c] as f32).sum::<f32>()
                / (0..3).filter(|&c| n[c] > 0).count() as f32;
            for c in 0..3 {
                px[c] = if n[c] > 0 { sum[c] / n[c] as f32 } else { known };
            }
        }
    });
}

/// Averages boxes of the mosaic into RGB pixels while rows arrive (see
/// [`crate::reduce::Reducer`], which this mirrors); `factor` must be at
/// least 2 so every box holds all three colours.
pub(crate) struct BayerReducer {
    pattern: Pattern,
    width: usize,
    height: usize,
    factor: usize,
    out_w: usize,
    out_h: usize,
    flip: bool,
    next: usize,
    rows_in: usize,
    sum: Vec<[f64; 3]>,
    count: Vec<[u32; 3]>,
}

impl BayerReducer {
    pub fn new(width: u32, height: u32, factor: u32, flip: bool, pattern: Pattern) -> BayerReducer {
        let factor = factor.max(2) as usize;
        let (width, height) = (width as usize, height as usize);
        let out_w = width.div_ceil(factor);
        BayerReducer {
            pattern,
            width,
            height,
            factor,
            out_w,
            out_h: height.div_ceil(factor),
            flip,
            next: 0,
            rows_in: 0,
            sum: vec![[0.0; 3]; out_w],
            count: vec![[0; 3]; out_w],
        }
    }

    pub fn factor(&self) -> usize {
        self.factor
    }

    pub fn is_done(&self) -> bool {
        self.next >= self.out_h
    }

    fn band_rows(&self) -> usize {
        self.factor.min(self.height.saturating_sub(self.next * self.factor))
    }

    pub fn push_rows(&mut self, rows: &[f32], n: usize, out: &mut Raster) {
        let mut offset = 0;
        let mut left = n;
        while left > 0 && !self.is_done() {
            let take = left.min(self.band_rows() - self.rows_in);
            self.accumulate(&rows[offset * self.width..(offset + take) * self.width], take);
            offset += take;
            left -= take;
            self.rows_in += take;
            if self.rows_in >= self.band_rows() {
                self.flush(out);
            }
        }
    }

    fn accumulate(&mut self, rows: &[f32], n: usize) {
        let (w, f, h) = (self.width, self.factor, self.height);
        // The pattern is defined on the upright picture, so when the file
        // stores the bottom row first the row number flips.
        let first_row = self.next * f + self.rows_in;
        let (flip, pattern) = (self.flip, self.pattern);
        let add = |t: usize, sum: &mut [[f64; 3]], count: &mut [[u32; 3]]| {
            for (k, (s, c)) in sum.iter_mut().zip(count.iter_mut()).enumerate() {
                let ox = t * 32 + k;
                let (x0, x1) = (ox * f, ((ox + 1) * f).min(w));
                for r in 0..n {
                    let y = if flip { h - 1 - (first_row + r) } else { first_row + r };
                    for x in x0..x1 {
                        let v = rows[r * w + x];
                        if !v.is_nan() {
                            let ch = pattern.colour(x, y);
                            s[ch] += v as f64;
                            c[ch] += 1;
                        }
                    }
                }
            }
        };
        if self.out_w * n * f >= 1 << 16 {
            self.sum
                .par_chunks_mut(32)
                .zip(self.count.par_chunks_mut(32))
                .enumerate()
                .for_each(|(t, (s, c))| add(t, s, c));
        } else {
            add(0, &mut self.sum, &mut self.count);
        }
    }

    fn flush(&mut self, out: &mut Raster) {
        let out_y = if self.flip { self.out_h - 1 - self.next } else { self.next };
        let row = &mut out.rgba[out_y * self.out_w * 4..(out_y + 1) * self.out_w * 4];
        for ((px, s), c) in row.chunks_exact_mut(4).zip(&mut self.sum).zip(&mut self.count) {
            let have = c.iter().filter(|&&k| k > 0).count();
            if have == 0 {
                px[3] = 0.0;
            } else {
                // A colour missing from an edge box borrows the others' mean.
                let known = (0..3).filter(|&ch| c[ch] > 0).map(|ch| s[ch] / c[ch] as f64).sum::<f64>() / have as f64;
                for ch in 0..3 {
                    px[ch] = if c[ch] > 0 { (s[ch] / c[ch] as f64) as f32 } else { known as f32 };
                }
            }
            *s = [0.0; 3];
            *c = [0; 3];
        }
        self.next += 1;
        self.rows_in = 0;
    }

    /// Marks the output rows never written, because the file ended early,
    /// undefined.
    pub fn finish(&mut self, out: &mut Raster) {
        for k in self.next..self.out_h {
            let out_y = if self.flip { self.out_h - 1 - k } else { k };
            let row = &mut out.rgba[out_y * self.out_w * 4..(out_y + 1) * self.out_w * 4];
            row.chunks_exact_mut(4).for_each(|p| p[3] = 0.0);
        }
        self.next = self.out_h;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn patterns() {
        let p = Pattern::parse("rggb", 0, 0).unwrap();
        assert_eq!((p.colour(0, 0), p.colour(1, 0), p.colour(0, 1), p.colour(1, 1)), (0, 1, 1, 2));
        let shifted = Pattern::parse("RGGB", 1, 0).unwrap();
        assert_eq!(shifted.colour(0, 0), 1);
        assert_eq!(Pattern::parse("BGGR", 0, 0).unwrap().colour(0, 0), 2);
        assert!(Pattern::parse("RGBX", 0, 0).is_none());
        assert!(Pattern::parse("RRGG", 0, 0).is_none());
        assert!(Pattern::parse("RGG", 0, 0).is_none());
        assert_eq!(Pattern::parse("GBRG", 0, 0).unwrap().name(), "GBRG");
    }

    #[test]
    fn bilinear_recovers_flat_colours() {
        // A mosaic of a uniform colour (R 0.8, G 0.5, B 0.2) demosaics back
        // to that colour everywhere, edges included.
        let p = Pattern::parse("RGGB", 0, 0).unwrap();
        let (w, h) = (8, 6);
        let mosaic: Vec<f32> = (0..w * h).map(|i| [0.8, 0.5, 0.2][p.colour(i % w, i / w)]).collect();
        let mut out = Raster::new(w as u32, h as u32);
        demosaic(&mosaic, w, h, &p, &mut out);
        for px in out.rgba.chunks_exact(4) {
            assert!((px[0] - 0.8).abs() < 1e-6 && (px[1] - 0.5).abs() < 1e-6 && (px[2] - 0.2).abs() < 1e-6, "{px:?}");
        }
    }

    #[test]
    fn superpixels_keep_each_colour() {
        let p = Pattern::parse("GRBG", 0, 0).unwrap();
        let (w, h) = (8, 8);
        let mosaic: Vec<f32> = (0..w * h).map(|i| [0.8, 0.5, 0.2][p.colour(i % w, i / w)]).collect();
        let mut r = BayerReducer::new(w as u32, h as u32, 2, false, p);
        let mut out = Raster::new(4, 4);
        r.push_rows(&mosaic, h, &mut out);
        assert!(r.is_done());
        for px in out.rgba.chunks_exact(4) {
            assert!((px[0] - 0.8).abs() < 1e-6 && (px[1] - 0.5).abs() < 1e-6 && (px[2] - 0.2).abs() < 1e-6, "{px:?}");
        }
    }

    #[test]
    fn flipped_files_use_the_upright_pattern() {
        // Stored bottom-up, 4 rows: stream row 0 is upright row 3.
        let p = Pattern::parse("RGGB", 0, 0).unwrap();
        let (w, h) = (4usize, 4usize);
        let upright: Vec<f32> = (0..w * h).map(|i| [0.8, 0.5, 0.2][p.colour(i % w, i / w)]).collect();
        let mut stream = Vec::new();
        for y in (0..h).rev() {
            stream.extend_from_slice(&upright[y * w..(y + 1) * w]);
        }
        let mut r = BayerReducer::new(w as u32, h as u32, 2, true, p);
        let mut out = Raster::new(2, 2);
        for row in 0..h {
            r.push_rows(&stream[row * w..(row + 1) * w], 1, &mut out);
        }
        for px in out.rgba.chunks_exact(4) {
            assert!((px[0] - 0.8).abs() < 1e-6 && (px[2] - 0.2).abs() < 1e-6, "{px:?}");
        }
    }
}
