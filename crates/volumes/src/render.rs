//! Turning a plane of numbers into a picture: a brightness window, a colour
//! map for single-channel data, direct colour for RGB, and additive
//! compositing for microscope channels.

use crate::colormap_data as data;
use crate::dtype::DType;
use crate::volume::{Slice, Volume};

/// Colour maps for single-channel data.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum ColorMap {
    #[default]
    Gray,
    Viridis,
    Inferno,
    Magma,
    Plasma,
    Cividis,
    Turbo,
    Hot,
    Coolwarm,
    Jet,
}

impl ColorMap {
    pub const ALL: [ColorMap; 10] = [
        ColorMap::Gray,
        ColorMap::Viridis,
        ColorMap::Inferno,
        ColorMap::Magma,
        ColorMap::Plasma,
        ColorMap::Cividis,
        ColorMap::Turbo,
        ColorMap::Hot,
        ColorMap::Coolwarm,
        ColorMap::Jet,
    ];

    pub fn name(self) -> &'static str {
        match self {
            ColorMap::Gray => "Greyscale",
            ColorMap::Viridis => "Viridis",
            ColorMap::Inferno => "Inferno",
            ColorMap::Magma => "Magma",
            ColorMap::Plasma => "Plasma",
            ColorMap::Cividis => "Cividis",
            ColorMap::Turbo => "Turbo",
            ColorMap::Hot => "Hot",
            ColorMap::Coolwarm => "Coolwarm",
            ColorMap::Jet => "Jet",
        }
    }

    fn stops(self) -> Option<&'static [[u8; 3]; 64]> {
        Some(match self {
            ColorMap::Gray => return None,
            ColorMap::Viridis => &data::VIRIDIS,
            ColorMap::Inferno => &data::INFERNO,
            ColorMap::Magma => &data::MAGMA,
            ColorMap::Plasma => &data::PLASMA,
            ColorMap::Cividis => &data::CIVIDIS,
            ColorMap::Turbo => &data::TURBO,
            ColorMap::Hot => &data::HOT,
            ColorMap::Coolwarm => &data::COOLWARM,
            ColorMap::Jet => &data::JET,
        })
    }

    /// 256 colours from low to high.
    pub fn lut(self) -> [[u8; 3]; 256] {
        let mut lut = [[0u8; 3]; 256];
        for (i, c) in lut.iter_mut().enumerate() {
            *c = match self.stops() {
                None => [i as u8; 3],
                Some(s) => {
                    let p = i as f32 / 255.0 * 63.0;
                    let (a, f) = (p.floor() as usize, p.fract());
                    let b = (a + 1).min(63);
                    let mix = |k: usize| (s[a][k] as f32 * (1.0 - f) + s[b][k] as f32 * f).round() as u8;
                    [mix(0), mix(1), mix(2)]
                }
            };
        }
        lut
    }
}

/// How the samples of a pixel combine into a colour.
#[derive(Clone, Debug)]
pub enum Layout {
    /// One value, windowed and put through a colour map.
    Single { map: ColorMap, invert: bool },
    /// Red, green, blue and maybe alpha, each windowed.
    Rgb,
    /// Channels added together, each with its own window and colour.
    Channels(Vec<ChannelStyle>),
}

#[derive(Clone, Debug)]
pub struct ChannelStyle {
    pub color: [u8; 3],
    pub window: (f32, f32),
    pub visible: bool,
}

/// What a picture is drawn with.
#[derive(Clone, Debug)]
pub struct Style {
    /// Values from `window.0` to `window.1` span black to white (or the
    /// colour map's ends).
    pub window: (f32, f32),
    pub layout: Layout,
}

/// The picture of a plane: RGBA8 rows, and whether any pixel is not opaque.
pub fn render(slice: &Slice, style: &Style) -> (Vec<u8>, bool) {
    let n = slice.width * slice.height;
    let mut out = vec![255u8; n * 4];
    let (lo, hi) = style.window;
    let scale = if hi > lo { 1.0 / (hi - lo) } else { 0.0 };
    let unit = |v: f32| -> f32 {
        if hi > lo {
            ((v - lo) * scale).clamp(0.0, 1.0)
        } else if v > lo {
            1.0
        } else {
            0.0
        }
    };
    let mut alpha = false;
    match &style.layout {
        Layout::Single { map, invert } => {
            let lut = map.lut();
            for (px, &v) in out.chunks_exact_mut(4).zip(slice.data.iter().step_by(slice.samples.max(1))) {
                if !v.is_finite() && !v.is_infinite() {
                    // NaN is transparent.
                    px[3] = 0;
                    alpha = true;
                    continue;
                }
                let mut t = unit(v);
                if *invert {
                    t = 1.0 - t;
                }
                let c = lut[(t * 255.0 + 0.5) as usize];
                px[..3].copy_from_slice(&c);
            }
        }
        Layout::Rgb => {
            let s = slice.samples;
            for (px, v) in out.chunks_exact_mut(4).zip(slice.data.chunks_exact(s.max(1))) {
                for k in 0..3 {
                    let t = unit(v[k.min(s - 1)]);
                    px[k] = (t * 255.0 + 0.5) as u8;
                }
                if s >= 4 {
                    px[3] = (unit(v[3]) * 255.0 + 0.5) as u8;
                    alpha |= px[3] != 255;
                }
            }
        }
        Layout::Channels(chs) => {
            let s = slice.samples;
            for (px, v) in out.chunks_exact_mut(4).zip(slice.data.chunks_exact(s.max(1))) {
                let mut acc = [0f32; 3];
                for (c, ch) in chs.iter().enumerate().take(s) {
                    if !ch.visible {
                        continue;
                    }
                    let (l, h) = ch.window;
                    let t = if h > l { ((v[c] - l) / (h - l)).clamp(0.0, 1.0) } else { 0.0 };
                    for (a, &c) in acc.iter_mut().zip(&ch.color) {
                        *a += t * c as f32;
                    }
                }
                for k in 0..3 {
                    px[k] = acc[k].clamp(0.0, 255.0) as u8;
                }
            }
        }
    }
    (out, alpha)
}

/*┌─────────────────╮
| Windows         |
└─────────────────╯*/
//region Windows

/// The window from the `lo`th to the `hi`th percentile (0-100) of the finite
/// values.  When they coincide, the full range; when that does too, a window
/// one unit wide around the value, so a flat plane shows as mid grey.
pub fn percentile_window(values: &[f32], lo: f64, hi: f64) -> (f32, f32) {
    let mut v: Vec<f32> = values.iter().copied().filter(|x| x.is_finite()).collect();
    if v.is_empty() {
        return (0.0, 1.0);
    }
    // A large plane is judged by an even sample of it.
    if v.len() > 1 << 20 {
        let step = v.len() / (1 << 20) + 1;
        v = v.into_iter().step_by(step).collect();
    }
    let n = v.len();
    let at = |v: &mut Vec<f32>, q: f64| -> f32 {
        let i = (((n - 1) as f64) * q / 100.0).round() as usize;
        *v.select_nth_unstable_by(i, |a, b| a.total_cmp(b)).1
    };
    let (a, b) = (at(&mut v, lo), at(&mut v, hi));
    if b > a {
        return (a, b);
    }
    let (mn, mx) = v.iter().fold((f32::MAX, f32::MIN), |(l, h), &x| (l.min(x), h.max(x)));
    if mx > mn { (mn, mx) } else { (mn - 0.5, mn + 0.5) }
}

/// The window a thumbnail or a first view uses: the 1st to 99th percentile
/// of a plane's values, the type's own range for 8-bit colour and booleans.
pub fn auto_window(volume: &Volume, slice: &Slice) -> (f32, f32) {
    if volume.dtype == DType::Bool {
        return (0.0, 1.0);
    }
    if slice.samples >= 3 && volume.channels.is_empty() {
        if volume.dtype == DType::U8 {
            return (0.0, 255.0);
        }
        let (mn, mx) =
            slice.data.iter().filter(|v| v.is_finite()).fold((f32::MAX, f32::MIN), |(l, h), &x| (l.min(x), h.max(x)));
        // Floating-point colour is nearly always 0 to 1.
        if volume.dtype.is_float() && mn >= 0.0 && mx <= 1.0 + 1e-4 {
            return (0.0, 1.0);
        }
        if volume.dtype == DType::U16 && mx > 255.0 {
            return percentile_window(&slice.data, 0.0, 99.9).min_max_with(0.0);
        }
        return percentile_window(&slice.data, 0.5, 99.5);
    }
    percentile_window(&slice.data, 1.0, 99.0)
}

trait WindowExt {
    fn min_max_with(self, lo: f32) -> (f32, f32);
}

impl WindowExt for (f32, f32) {
    fn min_max_with(self, lo: f32) -> (f32, f32) {
        (lo.min(self.0), self.1)
    }
}

//endregion

/*┌─────────────────╮
| Pixel shape     |
└─────────────────╯*/
//region Pixel shape

/// Stretches RGBA rows or columns so that pixels cover equal lengths, when
/// the volume says a pixel is taller than it is wide or the reverse (MRI
/// slices cut across thick slabs).  The coarser axis grows, up to `limit`
/// pixels, with bilinear interpolation.  Returns the new size too.
pub fn square_pixels(
    w: usize,
    h: usize,
    rgba: Vec<u8>,
    col_spacing: f64,
    row_spacing: f64,
    limit: usize,
) -> (usize, usize, Vec<u8>) {
    if w == 0 || h == 0 || col_spacing <= 0.0 || row_spacing <= 0.0 {
        return (w, h, rgba);
    }
    let aspect = row_spacing / col_spacing;
    if (aspect - 1.0).abs() < 0.02 {
        return (w, h, rgba);
    }
    let (ow, oh) = if aspect > 1.0 {
        (w, ((h as f64 * aspect).round() as usize).clamp(1, limit.max(h)))
    } else {
        (((w as f64 / aspect).round() as usize).clamp(1, limit.max(w)), h)
    };
    let mut out = vec![0u8; ow * oh * 4];
    for y in 0..oh {
        let sy = if oh == h { y as f64 } else { (y as f64 + 0.5) * h as f64 / oh as f64 - 0.5 };
        let y0 = (sy.floor().max(0.0) as usize).min(h - 1);
        let y1 = (y0 + 1).min(h - 1);
        let fy = (sy - y0 as f64).clamp(0.0, 1.0);
        for x in 0..ow {
            let sx = if ow == w { x as f64 } else { (x as f64 + 0.5) * w as f64 / ow as f64 - 0.5 };
            let x0 = (sx.floor().max(0.0) as usize).min(w - 1);
            let x1 = (x0 + 1).min(w - 1);
            let fx = (sx - x0 as f64).clamp(0.0, 1.0);
            for k in 0..4 {
                let p = |xx: usize, yy: usize| rgba[(yy * w + xx) * 4 + k] as f64;
                let top = p(x0, y0) * (1.0 - fx) + p(x1, y0) * fx;
                let bot = p(x0, y1) * (1.0 - fx) + p(x1, y1) * fx;
                out[(y * ow + x) * 4 + k] = (top * (1.0 - fy) + bot * fy).round() as u8;
            }
        }
    }
    (ow, oh, out)
}

//endregion

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dtype::Endian;
    use crate::source::MemSource;
    use crate::volume::{Axis, Role};
    use std::sync::Arc;

    fn slice(data: Vec<f32>, w: usize, h: usize, samples: usize) -> Slice {
        Slice {
            width: w,
            height: h,
            samples,
            data,
            col_spacing: 0.0,
            row_spacing: 0.0,
            step: 1,
            edges: [None; 4],
            col_axis: 0,
            row_axis: 0,
            fixed: vec![0; 2],
            flip_rows: false,
            flip_cols: false,
            rows: h,
            cols: w,
            projected_axis: None,
        }
    }

    #[test]
    fn grey_window_maps_ends_to_black_and_white() {
        let s = slice(vec![0.0, 5.0, 10.0, 20.0], 2, 2, 1);
        let (px, alpha) =
            render(&s, &Style { window: (0.0, 10.0), layout: Layout::Single { map: ColorMap::Gray, invert: false } });
        assert!(!alpha);
        assert_eq!(&px[0..3], &[0, 0, 0]);
        assert_eq!(&px[4..7], &[128, 128, 128]);
        assert_eq!(&px[8..11], &[255, 255, 255]);
        assert_eq!(&px[12..15], &[255, 255, 255]);
        let (inv, _) =
            render(&s, &Style { window: (0.0, 10.0), layout: Layout::Single { map: ColorMap::Gray, invert: true } });
        assert_eq!(&inv[0..3], &[255, 255, 255]);
    }

    #[test]
    fn nan_is_transparent_and_infinity_is_clamped() {
        let s = slice(vec![f32::NAN, f32::INFINITY, f32::NEG_INFINITY, 1.0], 2, 2, 1);
        let (px, alpha) =
            render(&s, &Style { window: (0.0, 1.0), layout: Layout::Single { map: ColorMap::Gray, invert: false } });
        assert!(alpha);
        assert_eq!(px[3], 0);
        assert_eq!(&px[4..8], &[255, 255, 255, 255]);
        assert_eq!(&px[8..12], &[0, 0, 0, 255]);
    }

    #[test]
    fn colour_maps_start_and_end_where_matplotlib_does() {
        let v = ColorMap::Viridis.lut();
        assert_eq!(v[0], [68, 1, 84]);
        assert_eq!(v[255], [253, 231, 37]);
        let g = ColorMap::Gray.lut();
        assert_eq!((g[0], g[128], g[255]), ([0; 3], [128; 3], [255; 3]));
        for m in ColorMap::ALL {
            let l = m.lut();
            assert_ne!(l[0], l[255], "{m:?}");
        }
    }

    #[test]
    fn percentile_windows_ignore_outliers_and_flat_planes() {
        let mut v: Vec<f32> = (0..1000).map(|i| i as f32).collect();
        v[0] = -1e9;
        v[999] = 1e9;
        let (lo, hi) = percentile_window(&v, 1.0, 99.0);
        assert!(lo > 0.0 && hi < 1000.0, "{lo} {hi}");
        assert_eq!(percentile_window(&[3.0; 10], 1.0, 99.0), (2.5, 3.5));
        assert_eq!(percentile_window(&[f32::NAN; 4], 1.0, 99.0), (0.0, 1.0));
        // A mask with few ones still shows them.
        let mut mask = vec![0.0f32; 1000];
        mask[10] = 1.0;
        assert_eq!(percentile_window(&mask, 1.0, 99.0), (0.0, 1.0));
    }

    #[test]
    fn channels_add_up_in_their_colours() {
        let s = slice(vec![1.0, 0.0, 0.0, 1.0, 1.0, 1.0], 3, 1, 2);
        let chs = vec![
            ChannelStyle { color: [255, 0, 0], window: (0.0, 1.0), visible: true },
            ChannelStyle { color: [0, 255, 0], window: (0.0, 1.0), visible: true },
        ];
        let (px, _) = render(&s, &Style { window: (0.0, 1.0), layout: Layout::Channels(chs) });
        assert_eq!(&px[0..3], &[255, 0, 0]);
        assert_eq!(&px[4..7], &[0, 255, 0]);
        assert_eq!(&px[8..11], &[255, 255, 0]);
    }

    #[test]
    fn rgb_8bit_uses_its_own_range() {
        let v = Volume::new(
            "x",
            "t",
            vec![Axis::new(1, 3, Role::Y), Axis::new(1, 3, Role::X), Axis::new(3, 1, Role::Color)],
            DType::U8,
            Endian::Little,
            Arc::new(MemSource(vec![1, 2, 3])),
        );
        let s = slice(vec![1.0, 2.0, 3.0], 1, 1, 3);
        assert_eq!(auto_window(&v, &s), (0.0, 255.0));
    }

    #[test]
    fn stretching_fixes_thick_slices() {
        let rgba = vec![200u8; 4 * 4 * 4];
        let (w, h, out) = square_pixels(4, 4, rgba.clone(), 1.0, 3.0, 4096);
        assert_eq!((w, h), (4, 12));
        assert_eq!(out.len(), 4 * 12 * 4);
        let (w, h, _) = square_pixels(4, 4, rgba.clone(), 2.0, 1.0, 4096);
        assert_eq!((w, h), (8, 4));
        let (w, h, _) = square_pixels(4, 4, rgba, 0.0, 1.0, 4096);
        assert_eq!((w, h), (4, 4));
    }
}
