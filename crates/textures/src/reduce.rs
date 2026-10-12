//! Shrinking rows of float samples while a file is read, so a 100 MB
//! astronomy frame or film scan becomes a thumbnail without ever being held
//! whole.  The same code also stores a full-size picture (factor 1), so
//! every format has one path.
//!
//! Samples that are not numbers (NaN) are undefined pixels: they are left
//! out of the averages, and an output pixel with no defined sample is
//! marked undefined by a zero alpha.
//!
//! Rows may arrive in pieces of any size; the reducer keeps one running sum
//! per output column, so memory stays proportional to the output no matter
//! how tall a band of source rows is.

use rayon::prelude::*;

/// Floating-point RGBA pixels, rows top to bottom, undefined pixels with
/// alpha 0.
pub(crate) struct Raster {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<f32>,
}

impl Raster {
    /// A raster of opaque black pixels.
    pub fn new(width: u32, height: u32) -> Raster {
        let n = width as usize * height as usize;
        let mut rgba = vec![0.0f32; n * 4];
        rgba.par_chunks_mut(4 * 8192).for_each(|c| c.chunks_exact_mut(4).for_each(|p| p[3] = 1.0));
        Raster { width, height, rgba }
    }
}

/// Which of the red, green and blue channels a plane of samples fills.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Fill {
    /// A grey plane: all three.
    Grey,
    Red,
    Green,
    Blue,
    /// Opacity, for formats that store it as a channel.
    Alpha,
}

impl Fill {
    fn channels(self) -> &'static [usize] {
        match self {
            Fill::Grey => &[0, 1, 2],
            Fill::Red => &[0],
            Fill::Green => &[1],
            Fill::Blue => &[2],
            Fill::Alpha => &[],
        }
    }
}

/// Output columns handled by one parallel task.
const COLUMNS_PER_TASK: usize = 32;

/// Rows to read at a time for a source `row_bytes` wide, so a piece and
/// what is made from it stay in cache (about 1 MiB of source) whatever the
/// width, and no more than a band.
pub(crate) fn rows_per_read(row_bytes: usize, band_rows: usize) -> usize {
    ((1 << 20) / row_bytes.max(1)).clamp(1, band_rows.max(1))
}

/// Averages boxes of `factor` x `factor` samples as rows arrive.
pub(crate) struct Reducer {
    width: usize,
    height: usize,
    factor: usize,
    out_w: usize,
    out_h: usize,
    flip: bool,
    /// The next output row to write, counted in reading order.
    next: usize,
    rows_in: usize,
    sum: Vec<f64>,
    count: Vec<u32>,
}

impl Reducer {
    /// `flip` places the first band at the bottom of the output, for formats
    /// that store the bottom row first (FITS).
    pub fn new(width: u32, height: u32, factor: u32, flip: bool) -> Reducer {
        let factor = factor.max(1) as usize;
        let (width, height) = (width as usize, height as usize);
        let out_w = width.div_ceil(factor);
        Reducer {
            width,
            height,
            factor,
            out_w,
            out_h: height.div_ceil(factor),
            flip,
            next: 0,
            rows_in: 0,
            sum: vec![0.0; out_w],
            count: vec![0; out_w],
        }
    }

    #[cfg(test)]
    pub fn out_size(&self) -> (u32, u32) {
        (self.out_w as u32, self.out_h as u32)
    }

    pub fn factor(&self) -> usize {
        self.factor
    }

    pub fn is_done(&self) -> bool {
        self.next >= self.out_h
    }

    /// Rows of the band being collected: `factor`, or what is left at the end.
    fn band_rows(&self) -> usize {
        self.factor.min(self.height.saturating_sub(self.next * self.factor))
    }

    /// Adds `n` source rows (`width` samples each) to the output, writing a
    /// finished output row to `out` whenever a band completes.  The channels
    /// `fill` names get the average; a pixel with no defined sample gets
    /// alpha 0.
    pub fn push_rows(&mut self, rows: &[f32], n: usize, out: &mut Raster, fill: Fill) {
        if self.factor == 1 {
            self.copy_rows(rows, n, out, fill);
            return;
        }
        let mut offset = 0;
        let mut left = n;
        while left > 0 && !self.is_done() {
            // A piece may straddle bands.
            let take = left.min(self.band_rows() - self.rows_in);
            self.accumulate(&rows[offset * self.width..(offset + take) * self.width], take);
            offset += take;
            left -= take;
            self.rows_in += take;
            if self.rows_in >= self.band_rows() {
                self.flush(out, fill);
            }
        }
    }

    /// Full size: rows go straight into the raster, in parallel.
    fn copy_rows(&mut self, rows: &[f32], n: usize, out: &mut Raster, fill: Fill) {
        let n = n.min(self.out_h - self.next);
        let w = self.width;
        // The rows of this piece are consecutive in the raster, upward when
        // the file stores the bottom row first.
        let first = if self.flip { self.out_h - self.next - n } else { self.next };
        let flip = self.flip;
        let channels = fill.channels();
        out.rgba[first * w * 4..(first + n) * w * 4].par_chunks_mut(w * 4).enumerate().for_each(|(i, row)| {
            let src = &rows[(if flip { n - 1 - i } else { i }) * w..][..w];
            for (px, &v) in row.chunks_exact_mut(4).zip(src) {
                if v.is_nan() {
                    px[3] = 0.0;
                } else if fill == Fill::Alpha {
                    px[3] = v;
                } else {
                    for &c in channels {
                        px[c] = v;
                    }
                }
            }
        });
        self.next += n;
    }

    fn accumulate(&mut self, rows: &[f32], n: usize) {
        let (w, f) = (self.width, self.factor);
        let add = |tasks: usize, sum: &mut [f64], count: &mut [u32]| {
            for (k, (s, c)) in sum.iter_mut().zip(count.iter_mut()).enumerate() {
                let ox = tasks * COLUMNS_PER_TASK + k;
                let (x0, x1) = (ox * f, ((ox + 1) * f).min(w));
                for r in 0..n {
                    for &v in &rows[r * w + x0..r * w + x1] {
                        if !v.is_nan() {
                            *s += v as f64;
                            *c += 1;
                        }
                    }
                }
            }
        };
        if self.out_w * n * f >= 1 << 16 {
            self.sum
                .par_chunks_mut(COLUMNS_PER_TASK)
                .zip(self.count.par_chunks_mut(COLUMNS_PER_TASK))
                .enumerate()
                .for_each(|(t, (s, c))| add(t, s, c));
        } else {
            add(0, &mut self.sum, &mut self.count);
        }
    }

    fn flush(&mut self, out: &mut Raster, fill: Fill) {
        let out_y = if self.flip { self.out_h - 1 - self.next } else { self.next };
        let row = &mut out.rgba[out_y * self.out_w * 4..(out_y + 1) * self.out_w * 4];
        let channels = fill.channels();
        for ((px, s), c) in row.chunks_exact_mut(4).zip(&mut self.sum).zip(&mut self.count) {
            if *c == 0 {
                px[3] = 0.0;
            } else {
                let v = (*s / *c as f64) as f32;
                if fill == Fill::Alpha {
                    px[3] = v;
                }
                for &ch in channels {
                    px[ch] = v;
                }
            }
            *s = 0.0;
            *c = 0;
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

/// The reduction factor for a decode: the whole factor that leaves the
/// longer side at least the size asked for, 1 when everything is wanted.
pub(crate) fn factor_for(width: u32, height: u32, opts: &crate::DecodeOptions) -> u32 {
    match opts.min_size.or(opts.max_size) {
        Some(min) => crate::resize::reduce_factor(width.max(height), min),
        None => 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn averages_boxes_and_flips() {
        // A 4x4 picture of row numbers, reduced by 2 and flipped.
        let samples: Vec<f32> = (0..16).map(|i| (i / 4) as f32).collect();
        let mut r = Reducer::new(4, 4, 2, true);
        let mut out = Raster::new(2, 2);
        r.push_rows(&samples, 4, &mut out, Fill::Grey);
        assert!(r.is_done());
        // The first band (rows 0 and 1, mean 0.5) lands at the bottom.
        assert_eq!(out.rgba[0], 2.5);
        assert_eq!(out.rgba[2 * 4], 0.5);
    }

    #[test]
    fn pieces_of_any_size_give_the_same_result() {
        let samples: Vec<f32> = (0..7 * 5).map(|i| (i * i % 13) as f32).collect();
        let whole = {
            let mut r = Reducer::new(7, 5, 3, false);
            let mut out = Raster::new(3, 2);
            r.push_rows(&samples, 5, &mut out, Fill::Grey);
            out.rgba
        };
        let pieces = {
            let mut r = Reducer::new(7, 5, 3, false);
            let mut out = Raster::new(3, 2);
            for row in 0..5 {
                r.push_rows(&samples[row * 7..(row + 1) * 7], 1, &mut out, Fill::Grey);
            }
            out.rgba
        };
        assert_eq!(whole, pieces);
    }

    #[test]
    fn full_size_copies_rows_in_place_flipped_or_not() {
        let samples: Vec<f32> = (0..6).map(|i| (i / 2) as f32).collect();
        for flip in [false, true] {
            // Two pieces of two rows and one of one, as a file may arrive.
            let mut r = Reducer::new(2, 3, 1, flip);
            let mut out = Raster::new(2, 3);
            r.push_rows(&samples[..4], 2, &mut out, Fill::Grey);
            r.push_rows(&samples[4..], 1, &mut out, Fill::Grey);
            assert!(r.is_done());
            let rows: Vec<f32> = out.rgba.iter().step_by(8).copied().collect();
            assert_eq!(rows, if flip { vec![2.0, 1.0, 0.0] } else { vec![0.0, 1.0, 2.0] });
        }
        let mut r = Reducer::new(2, 1, 1, false);
        let mut out = Raster::new(2, 1);
        r.push_rows(&[f32::NAN, 0.5], 1, &mut out, Fill::Alpha);
        assert_eq!((out.rgba[3], out.rgba[7]), (0.0, 0.5));
    }

    #[test]
    fn undefined_samples_are_skipped() {
        let samples = [1.0, f32::NAN, f32::NAN, f32::NAN];
        let mut r = Reducer::new(2, 2, 2, false);
        let mut out = Raster::new(1, 1);
        r.push_rows(&samples, 2, &mut out, Fill::Grey);
        assert_eq!(out.rgba, vec![1.0, 1.0, 1.0, 1.0]);
        let mut r = Reducer::new(2, 2, 2, false);
        let mut out = Raster::new(1, 1);
        r.push_rows(&[f32::NAN; 4], 2, &mut out, Fill::Grey);
        assert_eq!(out.rgba[3], 0.0);
    }

    #[test]
    fn odd_sizes_keep_the_last_partial_box() {
        let samples = [1.0f32, 2.0, 3.0];
        let mut r = Reducer::new(3, 1, 2, false);
        assert_eq!(r.out_size(), (2, 1));
        let mut out = Raster::new(2, 1);
        r.push_rows(&samples, 1, &mut out, Fill::Red);
        assert_eq!(out.rgba[0], 1.5);
        assert_eq!(out.rgba[4], 3.0);
        assert_eq!(out.rgba[1], 0.0);
    }

    #[test]
    fn early_end_leaves_the_rest_undefined() {
        let mut r = Reducer::new(2, 4, 1, false);
        let mut out = Raster::new(2, 4);
        r.push_rows(&[1.0; 4], 2, &mut out, Fill::Grey);
        r.finish(&mut out);
        assert_eq!(out.rgba[3], 1.0);
        assert_eq!(out.rgba[2 * 2 * 4 + 3], 0.0);
    }

    #[test]
    fn read_sizes() {
        assert_eq!(rows_per_read(1 << 14, 1000), 64);
        assert_eq!(rows_per_read(1 << 30, 1000), 1);
        assert_eq!(rows_per_read(100, 4), 4);
    }
}
