//! The VOI transform: which range of modality values the display shows
//! (window centre and width, a VOI LUT, or the image's own range), CT
//! presets, and turning a frame into display pixels.

use crate::image::{Frame, FramePixels};
use crate::lut::Lut;
use rayon::prelude::*;
use std::sync::Arc;

/// How a window maps values (VOI LUT Function).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum VoiFunction {
    /// The standard's linear ramp, which treats values as integer bins.
    #[default]
    Linear,
    /// An exact linear ramp from `c - w/2` to `c + w/2`.
    LinearExact,
    Sigmoid,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Window {
    pub center: f64,
    pub width: f64,
    pub function: VoiFunction,
}

impl Window {
    pub fn new(center: f64, width: f64) -> Window {
        Window { center, width, function: VoiFunction::Linear }
    }

    /// Display brightness 0 to 1 of a modality value (PS3.3 C.11.2.1.2).
    #[inline]
    pub fn apply(&self, x: f32) -> f32 {
        let (c, w) = (self.center as f32, self.width as f32);
        match self.function {
            VoiFunction::Linear => {
                let w = w.max(1.0);
                let lo = c - 0.5 - (w - 1.0) / 2.0;
                if x <= lo {
                    0.0
                } else if x > c - 0.5 + (w - 1.0) / 2.0 {
                    // Width 1 lands here too: a threshold at the centre.
                    1.0
                } else {
                    (x - (c - 0.5)) / (w - 1.0) + 0.5
                }
            }
            VoiFunction::LinearExact => ((x - c) / w.max(f32::MIN_POSITIVE) + 0.5).clamp(0.0, 1.0),
            VoiFunction::Sigmoid => 1.0 / (1.0 + (-4.0 * (x - c) / w.max(f32::MIN_POSITIVE)).exp()),
        }
    }

    /// The window from `lo` (black) to `hi` (white), exactly.
    pub fn spanning(lo: f64, hi: f64) -> Window {
        Window { center: (lo + hi) / 2.0, width: (hi - lo).max(0.0), function: VoiFunction::LinearExact }
    }
}

/// A VOI transform.
#[derive(Clone, Debug, PartialEq)]
pub enum Voi {
    Window(Window),
    Lut(Arc<Lut>),
}

impl Voi {
    #[inline]
    pub fn apply(&self, x: f32) -> f32 {
        match self {
            Voi::Window(w) => w.apply(x),
            Voi::Lut(l) => (l.lookup(x as f64) as f64 / l.full_scale.max(1.0)).clamp(0.0, 1.0) as f32,
        }
    }

    /// A linear window close to this transform, for dragging from a LUT.
    pub fn as_window(&self) -> Window {
        match self {
            Voi::Window(w) => *w,
            Voi::Lut(l) => {
                // The input range over which the table rises.
                let lo = l.data.iter().position(|&v| v > l.data[0]).unwrap_or(0).saturating_sub(1);
                let last = *l.data.last().unwrap_or(&0);
                let hi = l.data.iter().position(|&v| v >= last).unwrap_or(l.data.len() - 1);
                Window::spanning(l.first as f64 + lo as f64, l.first as f64 + hi.max(lo + 1) as f64)
            }
        }
    }
}

/// A window the file stores, with its explanation ("BRAIN", "BONE").
#[derive(Clone, Debug, PartialEq)]
pub struct StoredVoi {
    pub voi: Voi,
    pub name: String,
}

/// Window presets for CT, in Hounsfield units (centre, width), as
/// radiology references (Radiopaedia) give them.
pub const CT_PRESETS: &[(&str, f64, f64)] = &[
    ("Brain", 40.0, 80.0),
    ("Lung", -600.0, 1500.0),
    ("Bone", 400.0, 1800.0),
    ("Soft tissue", 50.0, 350.0),
    ("Abdomen", 50.0, 400.0),
];

/// Whether a window gives at least one pixel in fifty a grey between black
/// and white.  Windows that don't (the whole picture black or white) can't
/// be meant for the image.  Sampled, so a 16-megapixel frame costs
/// microseconds.  LUTs and colour frames are taken as meant.
pub fn shows_detail(voi: &Voi, frame: &Frame) -> bool {
    let (Voi::Window(w), FramePixels::Grey(values)) = (voi, &frame.pixels) else { return true };
    let step = (values.len() / (1 << 18)).max(1);
    let (mut inside, mut total) = (0usize, 0usize);
    for &x in values.iter().step_by(step) {
        let g = w.apply(x);
        inside += (g > 0.0 && g < 1.0) as usize;
        total += 1;
    }
    inside * 50 >= total
}

/// The lowest and highest finite value of a greyscale frame, leaving out
/// `padding` (values marking pixels outside the scanned area).
pub fn value_range(values: &[f32], padding: Option<(f32, f32)>) -> Option<(f32, f32)> {
    let (lo, hi) = values
        .par_chunks(1 << 16)
        .map(|chunk| {
            let (mut lo, mut hi) = (f32::INFINITY, f32::NEG_INFINITY);
            for &v in chunk {
                if !v.is_finite() || padding.is_some_and(|(a, b)| v >= a && v <= b) {
                    continue;
                }
                lo = lo.min(v);
                hi = hi.max(v);
            }
            (lo, hi)
        })
        .reduce(|| (f32::INFINITY, f32::NEG_INFINITY), |a, b| (a.0.min(b.0), a.1.max(b.1)));
    (lo <= hi).then_some((lo, hi))
}

/// The frame's display pixels as 8-bit RGBA: greyscale through `voi`
/// (inverted for MONOCHROME1 when `invert`), colour as it is.
pub fn render(frame: &Frame, voi: &Voi, invert: bool) -> Vec<u8> {
    let n = frame.width as usize * frame.height as usize;
    let mut out = vec![255u8; n * 4];
    match &frame.pixels {
        FramePixels::Grey(values) => {
            // Integer data sweeps a small range; a table over it saves the
            // transform per pixel.
            out.par_chunks_mut(4 * 4096).zip(values.par_chunks(4096)).for_each(|(o, v)| {
                for (px, &x) in o.chunks_exact_mut(4).zip(v) {
                    let mut g = voi.apply(x);
                    if invert {
                        g = 1.0 - g;
                    }
                    let g = (g * 255.0 + 0.5) as u8;
                    px[0] = g;
                    px[1] = g;
                    px[2] = g;
                }
            });
        }
        FramePixels::Rgb(rgb) => {
            out.par_chunks_mut(4 * 4096).zip(rgb.par_chunks(3 * 4096)).for_each(|(o, v)| {
                for (px, c) in o.chunks_exact_mut(4).zip(v.chunks_exact(3)) {
                    if invert {
                        px[..3].copy_from_slice(&[255 - c[0], 255 - c[1], 255 - c[2]]);
                    } else {
                        px[..3].copy_from_slice(c);
                    }
                }
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linear_window_matches_the_standard() {
        // Centre 40, width 80 (brain): -0.5 and below black, 79.5 and above
        // white, the centre mid-grey.
        let w = Window::new(40.0, 80.0);
        assert_eq!(w.apply(-1.0), 0.0);
        assert_eq!(w.apply(0.0), 0.0 + (0.0 - 39.5) / 79.0 + 0.5);
        assert!((w.apply(39.5) - 0.5).abs() < 1e-6);
        assert_eq!(w.apply(80.0), 1.0);
        // Width 1 is a threshold at the centre.
        let t = Window::new(100.0, 1.0);
        assert_eq!(t.apply(99.0), 0.0);
        assert_eq!(t.apply(100.0), 1.0);
        let exact = Window { function: VoiFunction::LinearExact, ..Window::new(0.0, 100.0) };
        assert_eq!(exact.apply(-50.0), 0.0);
        assert_eq!(exact.apply(25.0), 0.75);
        let sig = Window { function: VoiFunction::Sigmoid, ..Window::new(0.0, 100.0) };
        assert!((sig.apply(0.0) - 0.5).abs() < 1e-6);
        assert!(sig.apply(100.0) > 0.98);
    }

    #[test]
    fn spanning_shows_both_ends() {
        let w = Window::spanning(-1024.0, 3071.0);
        assert_eq!(w.apply(-1024.0), 0.0);
        assert_eq!(w.apply(3071.0), 1.0);
        assert_eq!(w.apply(1023.5), 0.5);
        assert_eq!(value_range(&[f32::NAN, -5.0, 3.0, -2000.0], Some((-2000.0, -2000.0))), Some((-5.0, 3.0)));
        assert_eq!(value_range(&[f32::NAN], None), None);
    }

    #[test]
    fn monochrome1_inverts() {
        let frame = Frame { width: 2, height: 1, pixels: FramePixels::Grey(vec![0.0, 100.0]) };
        let voi = Voi::Window(Window::spanning(0.0, 100.0));
        let normal = render(&frame, &voi, false);
        let inverted = render(&frame, &voi, true);
        assert_eq!((normal[0], normal[4]), (0, 255));
        assert_eq!((inverted[0], inverted[4]), (255, 0));
        assert_eq!(normal[3], 255);
    }

    #[test]
    fn windows_that_hide_the_picture_are_noticed() {
        // A 14-bit X-ray's values with a CT lung window (centre -600,
        // width 1600) stored by mistake: everything above 200 is white.
        let values: Vec<f32> = (0..10_000).map(|i| 2000.0 + (i % 100) as f32 * 100.0).collect();
        let frame = Frame { width: 100, height: 100, pixels: FramePixels::Grey(values) };
        assert!(!shows_detail(&Voi::Window(Window::new(-600.0, 1600.0)), &frame));
        assert!(shows_detail(&Voi::Window(Window::new(6000.0, 10000.0)), &frame));
        // Nothing to judge for colour, or for tables.
        let rgb = Frame { width: 1, height: 1, pixels: FramePixels::Rgb(vec![0, 0, 0]) };
        assert!(shows_detail(&Voi::Window(Window::new(-600.0, 1600.0)), &rgb));
    }

    #[test]
    fn voi_luts() {
        let lut = Lut {
            first: -10,
            bits: 8,
            data: vec![0, 0, 51, 102, 255, 255],
            full_scale: 255.0,
            explanation: String::new(),
        };
        let voi = Voi::Lut(Arc::new(lut));
        assert_eq!(voi.apply(-100.0), 0.0);
        assert!((voi.apply(-7.0) - 0.4).abs() < 1e-6);
        assert_eq!(voi.apply(100.0), 1.0);
        let w = voi.as_window();
        assert!(w.apply(-11.0) < 0.1 && w.apply(-5.0) > 0.9, "{w:?}");
    }
}
