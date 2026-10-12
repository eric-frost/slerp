//! Display stretches for linear scientific data.
//!
//! Raw astronomy frames, film scans and other measurements store light
//! linearly, and nearly all of it sits in the darkest few percent of the
//! range: shown as they are, they look black.  Astronomy programs
//! (PixInsight, Siril, DS9) solve this with a nonlinear stretch applied
//! only for display, chosen from robust statistics of the picture.  This
//! module does the same for thumbnails and for the viewer's controls.
//!
//! The default, [`StretchMode::Auto`], is the "screen transfer function"
//! those programs use: clip the darkest noise, then bend the midtones with
//! a rational curve so the sky background lands at a fixed brightness.
//! [`StretchMode::Asinh`] and [`StretchMode::Log`] compress the bright end
//! instead, which keeps star cores and nebula brightness ratios readable,
//! and [`StretchMode::Linear`] shows the data as they are.
//!
//! The statistics are computed once from the pixels ([`DataStats`]); the
//! parameters ([`Stretch`]) are cheap to change, so the viewer can redraw
//! while a slider moves.

use rayon::prelude::*;

/// How data values become display values.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum StretchMode {
    /// Black point to white point, linearly: what the numbers say.
    Linear,
    /// The midtones transfer function of PixInsight's and Siril's automatic
    /// stretch.
    #[default]
    Auto,
    /// Inverse hyperbolic sine: linear near black, logarithmic near white.
    Asinh,
    /// Logarithm.
    Log,
}

impl StretchMode {
    pub const ALL: [StretchMode; 4] = [StretchMode::Linear, StretchMode::Auto, StretchMode::Asinh, StretchMode::Log];

    pub fn name(self) -> &'static str {
        match self {
            StretchMode::Linear => "Linear",
            StretchMode::Auto => "Auto",
            StretchMode::Asinh => "Asinh",
            StretchMode::Log => "Log",
        }
    }

    /// The mode called `name` (as [`StretchMode::name`] gives it, any case).
    pub fn from_name(name: &str) -> Option<StretchMode> {
        Self::ALL.into_iter().find(|m| m.name().eq_ignore_ascii_case(name))
    }
}

/// How bright the typical background (the median) is drawn, 0 to 1, unless
/// the user changes it.  PixInsight uses the same 0.25.
pub const DEFAULT_TARGET: f32 = 0.25;

/// The most values per channel used for the median and its deviation.
/// Every pixel is still used for the minimum and maximum.
const SAMPLE_MAX: usize = 1 << 20;

/// Distances in the noise estimate below which a channel counts as flat.
const TINY: f32 = 1e-30;

/// Where the automatic black point sits, in noise deviations from the
/// median, as in PixInsight's automatic stretch.
const SHADOW_CLIP: f32 = -2.8;

/// Steps in the lookup tables.
const TABLE_STEPS: usize = 1 << 14;

/*┌─────────────────╮
| Statistics      |
└─────────────────╯*/
//region Statistics

/// Robust statistics of one channel of valid (finite, opaque) values.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ChannelStats {
    pub min: f32,
    pub max: f32,
    pub median: f32,
    /// The median absolute deviation times 1.4826, which equals the
    /// standard deviation for Gaussian noise but is not thrown off by
    /// stars.  Never zero.
    pub sigma: f32,
}

impl ChannelStats {
    const FLAT: ChannelStats = ChannelStats { min: 0.0, max: 1.0, median: 0.0, sigma: 1.0 };

    /// The black point the automatic modes start from: the median less a
    /// few noise deviations, never below the data's minimum.
    pub fn auto_black(&self) -> f32 {
        (self.median + SHADOW_CLIP * self.sigma).clamp(self.min, self.max)
    }
}

/// Statistics of the three colour channels of a picture.  A grey picture
/// has three equal channels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DataStats {
    pub channels: [ChannelStats; 3],
}

impl DataStats {
    /// Statistics as if the three channels were one, which is what linked
    /// stretches use so colour balance is kept.
    pub fn linked(&self) -> ChannelStats {
        let c = &self.channels;
        let mean = |f: fn(&ChannelStats) -> f32| (f(&c[0]) + f(&c[1]) + f(&c[2])) / 3.0;
        ChannelStats {
            min: c.iter().map(|s| s.min).fold(f32::INFINITY, f32::min),
            max: c.iter().map(|s| s.max).fold(f32::NEG_INFINITY, f32::max),
            median: mean(|s| s.median),
            sigma: mean(|s| s.sigma).max(TINY),
        }
    }
}

/// Computes the statistics of RGBA float pixels.  Pixels whose alpha is
/// below one half are undefined (blank or NaN in the file) and ignored, as
/// are non-finite values.
pub fn analyse(rgba: &[f32]) -> DataStats {
    let channels = std::array::from_fn(|c| channel_stats(rgba, c));
    DataStats { channels }
}

fn channel_stats(rgba: &[f32], c: usize) -> ChannelStats {
    let valid = |p: &[f32]| p[3] >= 0.5 && p[c].is_finite();
    // 1. The extremes need every pixel.
    let (min, max) = rgba
        .par_chunks(4 * 4096)
        .map(|chunk| {
            chunk
                .chunks_exact(4)
                .filter(|p| valid(p))
                .fold((f32::INFINITY, f32::NEG_INFINITY), |(lo, hi), p| (lo.min(p[c]), hi.max(p[c])))
        })
        .reduce(|| (f32::INFINITY, f32::NEG_INFINITY), |a, b| (a.0.min(b.0), a.1.max(b.1)));
    if min > max {
        return ChannelStats::FLAT;
    }
    // 2. A regular sample is enough for the median and its deviation.
    let count = rgba.len() / 4;
    let step = count.div_ceil(SAMPLE_MAX).max(1);
    let mut sample: Vec<f32> = rgba.chunks_exact(4).step_by(step).filter(|p| valid(p)).map(|p| p[c]).collect();
    if sample.is_empty() {
        return ChannelStats { min, max, median: min, sigma: ((max - min) * 1e-4).max(TINY) };
    }
    let median = median_of(&mut sample);
    let mut deviations: Vec<f32> = sample.iter().map(|v| (v - median).abs()).collect();
    let mut sigma = 1.4826 * median_of(&mut deviations);
    if sigma <= TINY {
        // More than half the values equal the median (a masked or clipped
        // background): fall back to the mean deviation of the rest.
        let off: Vec<f32> = deviations.iter().copied().filter(|d| *d > 0.0).collect();
        sigma = if off.is_empty() { (max - min) * 1e-4 } else { off.iter().sum::<f32>() / off.len() as f32 };
    }
    ChannelStats { min, max, median, sigma: sigma.max(TINY) }
}

/// The median, reordering `v`; the mean of the middle two for an even
/// count.  `v` must not be empty.
fn median_of(v: &mut [f32]) -> f32 {
    let len = v.len();
    let mid = len / 2;
    let (low, upper, _) = v.select_nth_unstable_by(mid, f32::total_cmp);
    if len.is_multiple_of(2) && !low.is_empty() {
        let lower = low.iter().copied().fold(f32::NEG_INFINITY, f32::max);
        (lower + *upper) * 0.5
    } else {
        *upper
    }
}

//endregion

/*┌─────────────────╮
| Parameters      |
└─────────────────╯*/
//region Parameters

/// What the user chose.  Everything else follows from the statistics, so a
/// new file starts from sensible black and white points whatever its units.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Stretch {
    pub mode: StretchMode,
    /// One set of points for all three channels, which keeps colour balance;
    /// otherwise each channel is stretched on its own, which neutralises a
    /// colour cast in the background.
    pub linked: bool,
    /// Where the median is drawn, 0 to 1, for the nonlinear modes.
    pub target: f32,
    /// Moves of the black and white points from their automatic places, in
    /// the slider units of [`Points::to_slider`].
    pub black_shift: f32,
    pub white_shift: f32,
}

impl Default for Stretch {
    fn default() -> Self {
        Stretch { mode: StretchMode::Auto, linked: false, target: DEFAULT_TARGET, black_shift: 0.0, white_shift: 0.0 }
    }
}

impl Stretch {
    /// The same choices with the points back at their automatic places.
    pub fn reset_points(self) -> Stretch {
        Stretch { black_shift: 0.0, white_shift: 0.0, ..self }
    }
}

/// The resolved black and white points of one channel, and the curve's
/// strength: everything [`apply_table`] needs.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Points {
    pub black: f32,
    pub white: f32,
    /// The midtones balance for `Auto`, the softening `a` for `Asinh`, the
    /// gain `k` for `Log`.
    pub strength: f32,
    pub mode: StretchMode,
    /// The channel's value range, which the sliders cover.
    pub min: f32,
    pub max: f32,
}

impl Points {
    /// Sliders cover the data range with a quadratic scale, which gives
    /// the dark end, where backgrounds live, most of the travel.
    pub fn to_slider(&self, value: f32) -> f32 {
        let span = (self.max - self.min).max(TINY);
        ((value - self.min) / span).clamp(0.0, 1.0).sqrt()
    }

    pub fn from_slider(&self, position: f32) -> f32 {
        let p = position.clamp(0.0, 1.0);
        self.min + (self.max - self.min) * p * p
    }
}

/// Resolves a stretch against the statistics: the points for each channel
/// (all equal when linked).
pub fn resolve(stats: &DataStats, stretch: &Stretch) -> [Points; 3] {
    let linked = stretch.linked.then(|| stats.linked());
    let top = stats.channels.iter().map(|c| c.max).fold(f32::NEG_INFINITY, f32::max);
    std::array::from_fn(|c| {
        let s = linked.unwrap_or(stats.channels[c]);
        // A flat channel has no noise or range to stretch from: show its
        // value from zero against the brightest channel, or against 1 when
        // all values are in the unit range (normalised integers, floats
        // from 0 to 1), where the value itself is the brightness.
        if s.max - s.min <= 1e-5 * s.max.abs().max(s.min.abs()) + TINY {
            let white = if top <= 1.0 { 1.0 } else { top };
            let (black, white) = (s.min.min(0.0), white.max(s.max).max(black_floor(s.min)));
            return Points { black, white, strength: 0.5, mode: StretchMode::Linear, min: black, max: white };
        }
        let (black, white) = automatic_points(&s, stretch.mode);
        let mut p = Points { black, white, strength: 0.5, mode: stretch.mode, min: s.min, max: s.max };
        // Slider moves are in slider units so one drag shifts every channel
        // by the same visual amount, whatever its own range.
        let b = p.from_slider((p.to_slider(black) + stretch.black_shift).clamp(0.0, 1.0));
        let w = p.from_slider((p.to_slider(white) + stretch.white_shift).clamp(0.0, 1.0));
        p.black = b.min(w);
        p.white = w.max(b);
        p.strength = strength_for(&s, &p, stretch.target);
        p
    })
}

/// A white point just above zero for a flat channel whose value is zero or
/// negative, so the span is never empty.
fn black_floor(value: f32) -> f32 {
    if value > 0.0 { value } else { 1.0 }
}

/// The black and white points a mode starts from.
fn automatic_points(s: &ChannelStats, mode: StretchMode) -> (f32, f32) {
    match mode {
        StretchMode::Linear => (s.min, s.max),
        _ => (s.auto_black(), s.max),
    }
}

/// Fits the curve's strength so the median is drawn at `target`.
fn strength_for(s: &ChannelStats, p: &Points, target: f32) -> f32 {
    let span = p.white - p.black;
    if span <= TINY || p.mode == StretchMode::Linear {
        return 0.5;
    }
    let x = ((s.median - p.black) / span).clamp(1e-7, 1.0 - 1e-7);
    let target = target.clamp(0.01, 0.95);
    match p.mode {
        StretchMode::Linear => 0.5,
        // The midtones balance that maps x to the target.
        StretchMode::Auto => mtf(x, target),
        StretchMode::Asinh => bisect(x, target, asinh_curve, 1e-9, 1e3),
        StretchMode::Log => bisect(x, target, log_curve, 1e-3, 1e9),
    }
}

/// Finds the strength in `[lo, hi]` (searched on a log scale) at which
/// `curve(x, strength)` equals `target`.  Curves here get weaker as the
/// strength grows for asinh (its softening) and stronger for log (its gain),
/// so the direction is read from the end points.
fn bisect(x: f32, target: f32, curve: impl Fn(f32, f32) -> f32, lo: f32, hi: f32) -> f32 {
    let rising = curve(x, hi) > curve(x, lo);
    let (mut lo, mut hi) = (lo.ln(), hi.ln());
    for _ in 0..60 {
        let mid = 0.5 * (lo + hi);
        let above = curve(x, mid.exp()) > target;
        if above == rising {
            hi = mid;
        } else {
            lo = mid;
        }
    }
    (0.5 * (lo + hi)).exp()
}

//endregion

/*┌─────────────────╮
| Curves          |
└─────────────────╯*/
//region Curves

/// The midtones transfer function: 0 at 0, 1 at 1, and `m` maps to one half.
/// With `m` below one half it brightens, as the automatic stretch needs.
pub fn mtf(x: f32, m: f32) -> f32 {
    if x <= 0.0 {
        0.0
    } else if x >= 1.0 {
        1.0
    } else if m == 0.5 {
        x
    } else {
        ((m - 1.0) * x) / ((2.0 * m - 1.0) * x - m)
    }
}

fn asinh_curve(x: f32, a: f32) -> f32 {
    let x = x.clamp(0.0, 1.0) as f64;
    let a = a as f64;
    ((x / a).asinh() / (1.0 / a).asinh()) as f32
}

fn log_curve(x: f32, k: f32) -> f32 {
    let x = x.clamp(0.0, 1.0) as f64;
    let k = k as f64;
    ((k * x).ln_1p() / k.ln_1p()) as f32
}

/// The display value, 0 to 1, for a position `x` between a channel's black
/// and white points.
pub fn curve(p: &Points, x: f32) -> f32 {
    match p.mode {
        StretchMode::Linear => x.clamp(0.0, 1.0),
        StretchMode::Auto => mtf(x, p.strength),
        StretchMode::Asinh => asinh_curve(x, p.strength),
        StretchMode::Log => log_curve(x, p.strength),
    }
}

/// A table of the curve in the square root of the position, so the dark end
/// that holds a sky background is sampled finely.
fn table_for(p: &Points) -> Vec<u8> {
    (0..TABLE_STEPS)
        .map(|i| {
            let r = i as f32 / (TABLE_STEPS - 1) as f32;
            (curve(p, r * r) * 255.0 + 0.5) as u8
        })
        .collect()
}

//endregion

/*┌─────────────────╮
| Applying        |
└─────────────────╯*/
//region Applying

/// Stretches RGBA float pixels to 8-bit display colour.  Undefined pixels
/// (alpha below one half) come out black.  Alpha is opaque.
pub fn apply(rgba: &[f32], points: &[Points; 3]) -> Vec<u8> {
    let tables: Vec<Vec<u8>> = points.iter().map(table_for).collect();
    let scale: Vec<f32> = points.iter().map(|p| 1.0 / (p.white - p.black).max(TINY)).collect();
    let mut out = vec![0u8; rgba.len()];
    out.par_chunks_mut(4 * 4096).zip(rgba.par_chunks(4 * 4096)).for_each(|(dst, src)| {
        for (d, p) in dst.chunks_exact_mut(4).zip(src.chunks_exact(4)) {
            d[3] = 255;
            if p[3] < 0.5 {
                continue;
            }
            for c in 0..3 {
                // NaN fails the comparison and lands on black.
                let x = (p[c] - points[c].black) * scale[c];
                let x = if x > 0.0 { x.min(1.0) } else { 0.0 };
                d[c] = tables[c][(x.sqrt() * (TABLE_STEPS - 1) as f32 + 0.5) as usize];
            }
        }
    });
    out
}

/// The automatic stretch of a picture, as thumbnails use: statistics, the
/// default parameters and one pass over the pixels.
pub fn auto_render(rgba: &[f32]) -> Vec<u8> {
    let stats = analyse(rgba);
    apply(rgba, &resolve(&stats, &Stretch::default()))
}

//endregion

#[cfg(test)]
mod tests {
    use super::*;

    /// A noisy, dim background with a few bright stars, as raw astronomy
    /// data looks: values near 1000 out of 65535.
    fn sky() -> Vec<f32> {
        let mut state = 12345u32;
        let mut noise = || {
            state = state.wrapping_mul(1664525).wrapping_add(1013904223);
            (state >> 8) as f32 / (1 << 24) as f32 - 0.5
        };
        let mut v = Vec::new();
        for i in 0..100 * 100 {
            let star = if i % 997 == 0 { 40000.0 } else { 0.0 };
            let value = 1000.0 + 60.0 * noise() + star;
            v.extend([value, value, value, 1.0]);
        }
        v
    }

    #[test]
    fn statistics_ignore_stars_and_undefined_pixels() {
        let mut v = sky();
        v[3] = 0.0;
        v[0] = 9e9;
        let s = analyse(&v);
        let c = s.channels[0];
        assert!((c.median - 1000.0).abs() < 5.0, "{c:?}");
        assert!(c.sigma > 5.0 && c.sigma < 60.0, "{c:?}");
        assert!(c.max > 40000.0 && c.max < 1e9, "{c:?}");
        assert_eq!(s.channels[0], s.channels[1]);
    }

    #[test]
    fn median_of_even_and_odd_counts() {
        assert_eq!(median_of(&mut [3.0, 1.0, 2.0]), 2.0);
        assert_eq!(median_of(&mut [4.0, 1.0, 3.0, 2.0]), 2.5);
        assert_eq!(median_of(&mut [7.0]), 7.0);
    }

    #[test]
    fn mtf_basics() {
        assert_eq!(mtf(0.0, 0.2), 0.0);
        assert_eq!(mtf(1.0, 0.2), 1.0);
        assert!((mtf(0.2, 0.2) - 0.5).abs() < 1e-6);
        assert_eq!(mtf(0.3, 0.5), 0.3);
        // Monotonic.
        let mut last = 0.0;
        for i in 1..100 {
            let y = mtf(i as f32 / 100.0, 0.05);
            assert!(y > last);
            last = y;
        }
    }

    #[test]
    fn every_mode_draws_the_median_at_the_target() {
        let v = sky();
        let stats = analyse(&v);
        for mode in [StretchMode::Auto, StretchMode::Asinh, StretchMode::Log] {
            for target in [0.1, 0.25, 0.4] {
                let st = Stretch { mode, target, ..Stretch::default() };
                let p = resolve(&stats, &st)[0];
                let x = (stats.channels[0].median - p.black) / (p.white - p.black);
                let y = curve(&p, x);
                assert!((y - target).abs() < 0.01, "{mode:?} target {target}: {y}");
            }
        }
    }

    #[test]
    fn auto_stretch_makes_a_dim_sky_readable() {
        let v = sky();
        let out = auto_render(&v);
        // The background was 1.5% of the range; its median is now the
        // target grey, and the stars are white.
        let mut reds: Vec<u8> = out.chunks_exact(4).map(|p| p[0]).collect();
        reds.sort_unstable();
        let median = reds[reds.len() / 2] as i32;
        assert!((60..=68).contains(&median), "{median}");
        assert_eq!(out[0], 255);
        // Linear leaves it nearly black.
        let stats = analyse(&v);
        let lin = apply(&v, &resolve(&stats, &Stretch { mode: StretchMode::Linear, ..Stretch::default() }));
        assert!(lin[4 * 5] < 10);
    }

    #[test]
    fn undefined_and_nan_pixels_are_black() {
        let v = vec![f32::NAN, 5.0, 5.0, 1.0, 100.0, 100.0, 100.0, 0.0, 1.0, 2.0, 3.0, 1.0];
        let out = auto_render(&v);
        // NaN is black in its channel; an undefined pixel is black.
        assert_eq!(out[0], 0);
        assert_eq!(out[3], 255);
        assert_eq!(&out[4..8], &[0, 0, 0, 255]);
    }

    #[test]
    fn slider_round_trip_and_shifts() {
        let v = sky();
        let stats = analyse(&v);
        let base = resolve(&stats, &Stretch::default())[0];
        for value in [base.black, 1500.0, base.max] {
            assert!((base.from_slider(base.to_slider(value)) - value).abs() < 1.0);
        }
        let darker = resolve(&stats, &Stretch { black_shift: 0.05, ..Stretch::default() })[0];
        assert!(darker.black > base.black);
        let lower_white = resolve(&stats, &Stretch { white_shift: -0.3, ..Stretch::default() })[0];
        assert!(lower_white.white < base.white);
        // Crossed points are kept ordered.
        let crossed = resolve(&stats, &Stretch { black_shift: 1.0, white_shift: -1.0, ..Stretch::default() })[0];
        assert!(crossed.black <= crossed.white);
    }

    #[test]
    fn flat_images_do_not_divide_by_zero() {
        let v = vec![[0.5f32, 0.5, 0.5, 1.0]; 16].concat();
        let out = auto_render(&v);
        assert_eq!(out.len(), v.len());
        let empty = auto_render(&[]);
        assert!(empty.is_empty());
        let all_undefined = auto_render(&[1.0, 1.0, 1.0, 0.0]);
        assert_eq!(all_undefined, vec![0, 0, 0, 255]);
    }

    #[test]
    fn flat_channels_show_their_value() {
        // A uniform colour keeps its proportions rather than turning grey.
        let v = [[1.0f32, 0.5, 0.25, 1.0]; 9].concat();
        let out = auto_render(&v);
        assert_eq!(&out[..4], &[255, 128, 64, 255]);
        let zero = auto_render(&[0.0, 0.0, 0.0, 1.0]);
        assert_eq!(zero, vec![0, 0, 0, 255]);
        // Values in the unit range are their own brightness.
        let half = auto_render(&[0.5, 0.5, 0.5, 1.0]);
        assert_eq!(half, vec![128, 128, 128, 255]);
        let grey = auto_render(&[7.0, 7.0, 7.0, 1.0]);
        assert_eq!(grey, vec![255, 255, 255, 255]);
    }

    #[test]
    fn linked_stretch_keeps_colour_balance() {
        // A red cast: unlinked equalises backgrounds, linked keeps the cast.
        let mut v = Vec::new();
        for i in 0..1000 {
            let n = (i % 7) as f32;
            v.extend([2000.0 + n, 1000.0 + n, 500.0 + n, 1.0]);
        }
        let stats = analyse(&v);
        let unlinked = apply(&v, &resolve(&stats, &Stretch::default()));
        let linked = apply(&v, &resolve(&stats, &Stretch { linked: true, ..Stretch::default() }));
        let spread = |px: &[u8]| px[0].abs_diff(px[2]);
        assert!(spread(&unlinked) < 8, "{unlinked:?}");
        assert!(spread(&linked) > 20, "{linked:?}");
    }
}
