//! Colour spaces, in the small part that matters for showing a picture:
//! working out which primaries and which transfer curve its values use, and
//! turning them into sRGB (or, for HDR curves, into linear light).
//!
//! Colour comes from two places.  ICC profiles (JPEG, PNG, WebP, TIFF and
//! JPEG 2000 carry them) are read only when they are the usual matrix and
//! curve kind, which covers sRGB, Display P3, Adobe RGB, ProPhoto, Rec. 2020
//! and the camera and monitor profiles built the same way; profiles made of
//! lookup tables (CMYK printers, scanners, the v4 "perceptual" sRGB) are left
//! alone and the picture is shown as if it were sRGB.  Coding-independent
//! code points (CICP, from AVIF, HEIF, PNG and JPEG XL) name the primaries
//! and transfer function directly, including the HDR curves PQ and HLG.
//!
//! Everything is done per channel through lookup tables and one 3 by 3
//! matrix, so a 20 megapixel picture converts in a few tens of milliseconds
//! on several threads.

use rayon::prelude::*;
use std::sync::OnceLock;

/// A 3 by 3 matrix, row major.
pub type Mat3 = [[f64; 3]; 3];

pub const IDENTITY: Mat3 = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];

pub fn mul(a: &Mat3, b: &Mat3) -> Mat3 {
    let mut out = [[0.0; 3]; 3];
    for (i, row) in out.iter_mut().enumerate() {
        for (j, v) in row.iter_mut().enumerate() {
            *v = (0..3).map(|k| a[i][k] * b[k][j]).sum();
        }
    }
    out
}

pub fn invert(m: &Mat3) -> Option<Mat3> {
    let det = m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1]) - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
        + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0]);
    if det.abs() < 1e-12 || !det.is_finite() {
        return None;
    }
    let d = 1.0 / det;
    Some([
        [
            (m[1][1] * m[2][2] - m[1][2] * m[2][1]) * d,
            (m[0][2] * m[2][1] - m[0][1] * m[2][2]) * d,
            (m[0][1] * m[1][2] - m[0][2] * m[1][1]) * d,
        ],
        [
            (m[1][2] * m[2][0] - m[1][0] * m[2][2]) * d,
            (m[0][0] * m[2][2] - m[0][2] * m[2][0]) * d,
            (m[0][2] * m[1][0] - m[0][0] * m[1][2]) * d,
        ],
        [
            (m[1][0] * m[2][1] - m[1][1] * m[2][0]) * d,
            (m[0][1] * m[2][0] - m[0][0] * m[2][1]) * d,
            (m[0][0] * m[1][1] - m[0][1] * m[1][0]) * d,
        ],
    ])
}

fn apply(m: &Mat3, v: [f64; 3]) -> [f64; 3] {
    std::array::from_fn(|i| m[i][0] * v[0] + m[i][1] * v[1] + m[i][2] * v[2])
}

/*┌─────────────────╮
| Primaries       |
└─────────────────╯*/
//region Primaries

/// Chromaticities (CIE xy) of red, green, blue and white.
type Chromaticities = [[f64; 2]; 4];

const D65: [f64; 2] = [0.3127, 0.3290];

const BT709: Chromaticities = [[0.640, 0.330], [0.300, 0.600], [0.150, 0.060], D65];

/// The matrix from linear RGB with these primaries to CIE XYZ.
fn rgb_to_xyz(c: &Chromaticities) -> Option<Mat3> {
    let xyz = |p: [f64; 2]| [p[0] / p[1], 1.0, (1.0 - p[0] - p[1]) / p[1]];
    let (r, g, b, w) = (xyz(c[0]), xyz(c[1]), xyz(c[2]), xyz(c[3]));
    let m = [[r[0], g[0], b[0]], [r[1], g[1], b[1]], [r[2], g[2], b[2]]];
    let s = apply(&invert(&m)?, w);
    Some([
        [m[0][0] * s[0], m[0][1] * s[1], m[0][2] * s[2]],
        [m[1][0] * s[0], m[1][1] * s[1], m[1][2] * s[2]],
        [m[2][0] * s[0], m[2][1] * s[1], m[2][2] * s[2]],
    ])
}

/// Bradford chromatic adaptation from one white point to another.
fn bradford(from: [f64; 2], to: [f64; 2]) -> Option<Mat3> {
    const B: Mat3 = [[0.8951, 0.2664, -0.1614], [-0.7502, 1.7135, 0.0367], [0.0389, -0.0685, 1.0296]];
    let xyz = |p: [f64; 2]| [p[0] / p[1], 1.0, (1.0 - p[0] - p[1]) / p[1]];
    let (a, b) = (apply(&B, xyz(from)), apply(&B, xyz(to)));
    let scale = [[b[0] / a[0], 0.0, 0.0], [0.0, b[1] / a[1], 0.0], [0.0, 0.0, b[2] / a[2]]];
    Some(mul(&invert(&B)?, &mul(&scale, &B)))
}

/// The chromaticities H.273 gives to a colour primaries code, for the ones
/// pictures use.
fn primaries_of(code: u8) -> Option<Chromaticities> {
    Some(match code {
        1 | 2 => BT709,
        4 => [[0.67, 0.33], [0.21, 0.71], [0.14, 0.08], [0.310, 0.316]],
        5 => [[0.64, 0.33], [0.29, 0.60], [0.15, 0.06], D65],
        6 | 7 => [[0.630, 0.340], [0.310, 0.595], [0.155, 0.070], D65],
        8 => [[0.681, 0.319], [0.243, 0.692], [0.145, 0.049], [0.310, 0.316]],
        9 => [[0.708, 0.292], [0.170, 0.797], [0.131, 0.046], D65],
        10 => [[1.0, 0.0], [0.0, 1.0], [0.0, 0.0], [1.0 / 3.0, 1.0 / 3.0]],
        11 => [[0.680, 0.320], [0.265, 0.690], [0.150, 0.060], [0.314, 0.351]],
        12 => [[0.680, 0.320], [0.265, 0.690], [0.150, 0.060], D65],
        22 => [[0.630, 0.340], [0.295, 0.605], [0.155, 0.077], D65],
        _ => return None,
    })
}

/// The name of a colour primaries code, for the inspector.
pub fn primaries_name(code: u8) -> &'static str {
    match code {
        1 => "BT.709 (sRGB)",
        4 => "BT.470 M",
        5 => "BT.601 (625 lines)",
        6 | 7 => "BT.601 (525 lines)",
        8 => "generic film",
        9 => "BT.2020",
        10 => "CIE XYZ",
        11 => "DCI P3",
        12 => "Display P3",
        22 => "EBU Tech 3213",
        _ => "unspecified",
    }
}

//endregion

/*┌─────────────────╮
| Transfer curves |
└─────────────────╯*/
//region Transfer curves

/// How encoded values map to light.  Decoding gives linear light scaled so
/// that 1.0 is SDR white: for PQ the 203 nanits the ITU recommends for
/// reference white (BT.2408), for HLG the level of 75 % signal.
#[derive(Clone, Debug, PartialEq)]
pub enum Transfer {
    Srgb,
    Linear,
    Gamma(f64),
    Pq,
    Hlg,
    /// A sampled curve (ICC `curv`): linear light at evenly spaced encoded
    /// values from 0 to 1.
    Table(Vec<f32>),
    /// ICC parametric curve: type and the parameters g, a, b, c, d, e, f.
    Parametric(u16, [f64; 7]),
}

/// Reference white for PQ, in nanits (BT.2408).
const PQ_WHITE: f64 = 203.0;

impl Transfer {
    /// The transfer curve of an H.273 code, for the ones pictures use.
    pub fn from_code(code: u8) -> Option<Transfer> {
        Some(match code {
            // The Rec. 709 curve is how a camera encodes light, not how a screen shows
            // it: pictures (and libavif, and browsers) show such colour as if it were sRGB.
            1 | 2 | 6 | 13 | 14 | 15 => Transfer::Srgb,
            4 => Transfer::Gamma(2.2),
            5 => Transfer::Gamma(2.8),
            8 => Transfer::Linear,
            16 => Transfer::Pq,
            18 => Transfer::Hlg,
            _ => return None,
        })
    }

    /// The Rec. 709 camera curve taken literally, as JPEG XL files that name
    /// it ask for (H.273 code 1 in AVIF and HEIF is shown as sRGB instead).
    #[cfg_attr(not(feature = "jxl"), allow(dead_code))]
    pub fn bt709() -> Transfer {
        Transfer::Parametric(3, [1.0 / 0.45, 1.0 / 1.099, 0.099 / 1.099, 1.0 / 4.5, 0.081, 0.0, 0.0])
    }

    /// Whether this curve carries more than SDR's range.
    pub fn is_hdr(&self) -> bool {
        matches!(self, Transfer::Pq | Transfer::Hlg)
    }

    /// Linear light for an encoded value in 0 to 1.
    pub fn decode(&self, v: f64) -> f64 {
        let v = if v.is_nan() { 0.0 } else { v.clamp(0.0, 1.0) };
        match self {
            Transfer::Srgb => {
                if v <= 0.04045 {
                    v / 12.92
                } else {
                    ((v + 0.055) / 1.055).powf(2.4)
                }
            }
            Transfer::Linear => v,
            Transfer::Gamma(g) => v.powf(*g),
            Transfer::Pq => {
                const M1: f64 = 2610.0 / 16384.0;
                const M2: f64 = 2523.0 / 4096.0 * 128.0;
                const C1: f64 = 3424.0 / 4096.0;
                const C2: f64 = 2413.0 / 4096.0 * 32.0;
                const C3: f64 = 2392.0 / 4096.0 * 32.0;
                let p = v.powf(1.0 / M2);
                let nits = 10000.0 * ((p - C1).max(0.0) / (C2 - C3 * p)).powf(1.0 / M1);
                nits / PQ_WHITE
            }
            Transfer::Hlg => {
                // The inverse OETF gives scene light, 0 to 1; a 1000 nit
                // display shows it at 1000 nits, so 75 % signal (0.265 scene
                // light) lands near 203 nits.  The system gamma of 1.2 is
                // applied per channel, which is exact for grey and close for
                // colour.
                const A: f64 = 0.17883277;
                const B: f64 = 0.28466892;
                const C: f64 = 0.55991073;
                let scene = if v <= 0.5 { v * v / 3.0 } else { (((v - C) / A).exp() + B) / 12.0 };
                1000.0 * scene.powf(1.2) / PQ_WHITE
            }
            Transfer::Table(t) => {
                let x = v * (t.len() - 1) as f64;
                let i = (x as usize).min(t.len() - 1);
                let j = (i + 1).min(t.len() - 1);
                let f = x - i as f64;
                t[i] as f64 * (1.0 - f) + t[j] as f64 * f
            }
            Transfer::Parametric(kind, p) => {
                let [g, a, b, c, d, e, f] = *p;
                let pow = |x: f64| x.max(0.0).powf(g);
                let y = match kind {
                    0 => pow(v),
                    1 => {
                        if v >= -b / a {
                            pow(a * v + b)
                        } else {
                            0.0
                        }
                    }
                    2 => {
                        if v >= -b / a {
                            pow(a * v + b) + c
                        } else {
                            c
                        }
                    }
                    3 => {
                        if v >= d {
                            pow(a * v + b)
                        } else {
                            c * v
                        }
                    }
                    _ => {
                        if v >= d {
                            pow(a * v + b) + e
                        } else {
                            c * v + f
                        }
                    }
                };
                if y.is_nan() { 0.0 } else { y.clamp(0.0, 1.0) }
            }
        }
    }

    /// The decoded value of each of `n` evenly spaced encoded values from 0
    /// to 1 (code values of an `n`-level channel).
    pub fn lut(&self, n: usize) -> Vec<f32> {
        let top = (n.max(2) - 1) as f64;
        (0..n).map(|i| self.decode(i as f64 / top) as f32).collect()
    }

    /// The name of the curve for the inspector.
    pub fn name(&self) -> String {
        match self {
            Transfer::Srgb => "sRGB".into(),
            Transfer::Linear => "linear".into(),
            Transfer::Gamma(g) => format!("gamma {g}"),
            Transfer::Pq => "PQ (SMPTE ST 2084)".into(),
            Transfer::Hlg => "HLG".into(),
            Transfer::Table(_) | Transfer::Parametric(..) => "curve from the profile".into(),
        }
    }
}

/// The name of a transfer characteristics code.
pub fn transfer_name(code: u8) -> String {
    Transfer::from_code(code).map_or_else(|| "unspecified".into(), |t| t.name())
}

//endregion

/*┌─────────────────╮
| Conversion      |
└─────────────────╯*/
//region Conversion

/// A way to turn a picture's values into linear light in the sRGB (Rec. 709)
/// primaries.
#[derive(Clone, Debug)]
pub struct Conversion {
    /// One transfer curve per channel (the same one thrice for CICP).
    pub curves: [Transfer; 3],
    /// From linear RGB in the source primaries to linear sRGB.
    pub matrix: Mat3,
    /// What the inspector says: "Display P3 profile", "BT.2020 primaries,
    /// PQ transfer".
    pub name: String,
}

impl Conversion {
    /// The conversion a CICP quadruple (primaries, transfer, matrix, full
    /// range) asks for, or `None` when it names something unknown.
    pub fn from_cicp(cicp: [u8; 4]) -> Option<Conversion> {
        let chroma = primaries_of(cicp[0])?;
        let transfer = Transfer::from_code(cicp[1])?;
        let src = rgb_to_xyz(&chroma)?;
        let dst = invert(&rgb_to_xyz(&BT709)?)?;
        let adapt = if chroma[3] == D65 { IDENTITY } else { bradford(chroma[3], D65)? };
        let matrix = mul(&dst, &mul(&adapt, &src));
        let name = format!("{} primaries, {} transfer", primaries_name(cicp[0]), transfer.name());
        Some(Conversion { curves: [transfer.clone(), transfer.clone(), transfer], matrix, name })
    }

    /// The conversion for RGB values with these primaries and white point
    /// (CIE xy of red, green, blue and white) and one transfer curve, as JPEG
    /// XL describes its colour.
    #[cfg_attr(not(feature = "jxl"), allow(dead_code))]
    pub fn from_chromaticities(chroma: [[f64; 2]; 4], transfer: Transfer, name: String) -> Option<Conversion> {
        let src = rgb_to_xyz(&chroma)?;
        let dst = invert(&rgb_to_xyz(&BT709)?)?;
        let adapt = if chroma[3] == D65 { IDENTITY } else { bradford(chroma[3], D65)? };
        let matrix = mul(&dst, &mul(&adapt, &src));
        Some(Conversion { curves: [transfer.clone(), transfer.clone(), transfer], matrix, name })
    }

    /// The conversion for grey values: only the transfer curve applies.
    #[cfg_attr(not(feature = "jxl"), allow(dead_code))]
    pub fn grey(transfer: Transfer, name: String) -> Conversion {
        Conversion { curves: [transfer.clone(), transfer.clone(), transfer], matrix: IDENTITY, name }
    }

    /// The conversion an ICC profile asks for, when it is a matrix and curve
    /// profile for RGB.
    pub fn from_icc(profile: &[u8]) -> Option<Conversion> {
        let p = Icc::parse(profile)?;
        let src = p.colorants;
        // The profile connection space is D50 XYZ; sRGB's colorants in it
        // are fixed by the sRGB profile itself.
        let dst = invert(&SRGB_D50)?;
        Some(Conversion { curves: p.curves, matrix: mul(&dst, &src), name: p.description })
    }

    /// True when this changes nothing: sRGB primaries and the sRGB curve (to
    /// within what 8 bits can show).
    pub fn is_srgb(&self) -> bool {
        let close = (0..3).all(|i| (0..3).all(|j| (self.matrix[i][j] - IDENTITY[i][j]).abs() < 0.004));
        close
            && self.curves.iter().all(|c| {
                (0..=16).all(|k| {
                    let v = k as f64 / 16.0;
                    (c.decode(v) - Transfer::Srgb.decode(v)).abs() < 0.004
                })
            })
    }

    pub fn is_hdr(&self) -> bool {
        self.curves[0].is_hdr()
    }

    /// Linear sRGB of one pixel of encoded values in 0 to 1.
    #[cfg_attr(not(feature = "jxl"), allow(dead_code))]
    fn linear(&self, v: [f32; 3]) -> [f32; 3] {
        let lin = [
            self.curves[0].decode(v[0] as f64),
            self.curves[1].decode(v[1] as f64),
            self.curves[2].decode(v[2] as f64),
        ];
        let out = apply(&self.matrix, lin);
        [out[0] as f32, out[1] as f32, out[2] as f32]
    }

    fn tables(&self, levels: usize) -> [Vec<f32>; 3] {
        [self.curves[0].lut(levels), self.curves[1].lut(levels), self.curves[2].lut(levels)]
    }

    fn matrix32(&self) -> [[f32; 3]; 3] {
        std::array::from_fn(|i| std::array::from_fn(|j| self.matrix[i][j] as f32))
    }

    /// Converts 8-bit RGBA in place to sRGB.  HDR curves are tone mapped,
    /// since 8 bits can't hold them.
    pub fn convert_rgba8(&self, rgba: &mut [u8]) {
        let tabs = self.tables(256);
        let m = self.matrix32();
        let hdr = self.is_hdr();
        rgba.par_chunks_mut(4 * 1024).for_each(|chunk| {
            for p in chunk.chunks_exact_mut(4) {
                let lin = [tabs[0][p[0] as usize], tabs[1][p[1] as usize], tabs[2][p[2] as usize]];
                let mut out = std::array::from_fn(|i| m[i][0] * lin[0] + m[i][1] * lin[1] + m[i][2] * lin[2]);
                if hdr {
                    out = tone_map(out);
                }
                for (dst, v) in p.iter_mut().zip(out) {
                    *dst = encode_srgb8(v);
                }
            }
        });
    }

    /// Converts samples of `bits` bits (stored in `u16`) as RGBA pixels of
    /// four values into linear sRGB floats.  The alpha value is scaled to 0
    /// to 1 and carried over.
    pub fn to_linear_rgba(&self, samples: &[u16], bits: u32) -> Vec<f32> {
        let levels = 1usize << bits.clamp(1, 16);
        let tabs = self.tables(levels);
        let m = self.matrix32();
        let max = (levels - 1) as f32;
        let mut out = vec![0f32; samples.len()];
        out.par_chunks_mut(4 * 1024).zip(samples.par_chunks(4 * 1024)).for_each(|(o, s)| {
            for (o, p) in o.chunks_exact_mut(4).zip(s.chunks_exact(4)) {
                let at = |v: u16| (v as usize).min(levels - 1);
                let lin = [tabs[0][at(p[0])], tabs[1][at(p[1])], tabs[2][at(p[2])]];
                for i in 0..3 {
                    o[i] = m[i][0] * lin[0] + m[i][1] * lin[1] + m[i][2] * lin[2];
                }
                o[3] = p[3] as f32 / max;
            }
        });
        out
    }

    /// Converts floating point RGBA whose colour is encoded with this
    /// conversion's curves into linear sRGB.
    #[cfg_attr(not(feature = "jxl"), allow(dead_code))]
    pub fn float_rgba_to_linear(&self, rgba: &mut [f32]) {
        rgba.par_chunks_mut(4 * 1024).for_each(|chunk| {
            for p in chunk.chunks_exact_mut(4) {
                let lin = self.linear([p[0], p[1], p[2]]);
                p[..3].copy_from_slice(&lin);
            }
        });
    }
}

/// Khronos PBR Neutral tone mapping of linear colour: base colours stay as
/// they are up to a bright threshold, and highlights roll off towards white.
pub fn tone_map(c: [f32; 3]) -> [f32; 3] {
    const START: f32 = 0.8 - 0.04;
    const DESATURATION: f32 = 0.15;
    let x = c[0].min(c[1]).min(c[2]);
    let offset = if x < 0.08 { x - 6.25 * x * x } else { 0.04 };
    let c = [c[0] - offset, c[1] - offset, c[2] - offset];
    let peak = c[0].max(c[1]).max(c[2]);
    if peak < START {
        return c;
    }
    let d = 1.0 - START;
    let new_peak = 1.0 - d * d / (peak + d - START);
    let s = new_peak / peak;
    let c = [c[0] * s, c[1] * s, c[2] * s];
    let g = 1.0 - 1.0 / (DESATURATION * (peak - new_peak) + 1.0);
    [c[0] + (new_peak - c[0]) * g, c[1] + (new_peak - c[1]) * g, c[2] + (new_peak - c[2]) * g]
}

/// A linear value as an sRGB-encoded byte.  A table of 16384 steps is exact
/// to the byte except in the darkest few values.
pub fn encode_srgb8(v: f32) -> u8 {
    const STEPS: usize = 1 << 14;
    static TABLE: OnceLock<Vec<u8>> = OnceLock::new();
    let table = TABLE.get_or_init(|| {
        (0..STEPS)
            .map(|i| {
                let v = i as f64 / (STEPS - 1) as f64;
                let e = if v <= 0.0031308 { v * 12.92 } else { 1.055 * v.powf(1.0 / 2.4) - 0.055 };
                (e * 255.0 + 0.5) as u8
            })
            .collect()
    });
    // NaN fails both comparisons and lands on 0.
    let v = if v > 0.0 { v.min(1.0) } else { 0.0 };
    table[(v * (STEPS - 1) as f32 + 0.5) as usize]
}

//endregion

/*┌─────────────────╮
| ICC profiles    |
└─────────────────╯*/
//region ICC profiles

/// sRGB's colorants in the profile connection space (D50 XYZ), as the
/// sRGB IEC61966-2.1 profile stores them: red, green and blue as columns.
const SRGB_D50: Mat3 =
    [[0.4360747, 0.3850649, 0.1430804], [0.2225045, 0.7168786, 0.0606169], [0.0139322, 0.0971045, 0.7141733]];

/// What a matrix and curve RGB profile holds.
struct Icc {
    /// Red, green and blue colorants in D50 XYZ, as columns.
    colorants: Mat3,
    curves: [Transfer; 3],
    description: String,
}

fn be32(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

fn s15(b: &[u8], at: usize) -> Option<f64> {
    Some(be32(b, at)? as i32 as f64 / 65536.0)
}

impl Icc {
    /// Reads an RGB matrix and curve profile; `None` for anything else
    /// (lookup-table profiles, grey, CMYK, Lab) or damaged data.
    fn parse(p: &[u8]) -> Option<Icc> {
        if p.len() < 132 || p.get(36..40)? != b"acsp" || p.get(16..20)? != b"RGB " {
            return None;
        }
        let count = be32(p, 128)? as usize;
        if count == 0 || count > 200 {
            return None;
        }
        let tag = |sig: &[u8; 4]| -> Option<&[u8]> {
            for i in 0..count {
                let e = 132 + i * 12;
                if p.get(e..e + 4)? == sig {
                    let off = be32(p, e + 4)? as usize;
                    let len = be32(p, e + 8)? as usize;
                    return p.get(off..off.checked_add(len)?);
                }
            }
            None
        };
        let xyz = |sig: &[u8; 4]| -> Option<[f64; 3]> {
            let t = tag(sig)?;
            (t.get(..4)? == b"XYZ ").then_some(())?;
            Some([s15(t, 8)?, s15(t, 12)?, s15(t, 16)?])
        };
        let (r, g, b) = (xyz(b"rXYZ")?, xyz(b"gXYZ")?, xyz(b"bXYZ")?);
        let colorants = [[r[0], g[0], b[0]], [r[1], g[1], b[1]], [r[2], g[2], b[2]]];
        let curve = |sig: &[u8; 4]| -> Option<Transfer> { parse_curve(tag(sig)?) };
        let curves = [curve(b"rTRC")?, curve(b"gTRC")?, curve(b"bTRC")?];
        let description = tag(b"desc").and_then(description_of).unwrap_or_else(|| "colour profile".into());
        Some(Icc { colorants, curves, description })
    }
}

/// An ICC `curv` or `para` curve.
fn parse_curve(t: &[u8]) -> Option<Transfer> {
    match t.get(..4)? {
        b"curv" => {
            let n = be32(t, 8)? as usize;
            match n {
                0 => Some(Transfer::Linear),
                1 => {
                    let g = u16::from_be_bytes([*t.get(12)?, *t.get(13)?]) as f64 / 256.0;
                    Some(Transfer::Gamma(g))
                }
                _ if n <= 1 << 16 => {
                    let body = t.get(12..12 + n * 2)?;
                    Some(Transfer::Table(
                        body.chunks_exact(2).map(|c| u16::from_be_bytes([c[0], c[1]]) as f32 / 65535.0).collect(),
                    ))
                }
                _ => None,
            }
        }
        b"para" => {
            let kind = u16::from_be_bytes([*t.get(8)?, *t.get(9)?]);
            let count = match kind {
                0 => 1,
                1 => 3,
                2 => 4,
                3 => 5,
                4 => 7,
                _ => return None,
            };
            let mut p = [0.0f64; 7];
            for (i, v) in p.iter_mut().take(count).enumerate() {
                *v = s15(t, 12 + i * 4)?;
            }
            Some(Transfer::Parametric(kind, p))
        }
        _ => None,
    }
}

/// The text of a profile's description tag (`desc` in version 2 profiles,
/// `mluc` in version 4).
fn description_of(t: &[u8]) -> Option<String> {
    match t.get(..4)? {
        b"desc" => {
            let n = be32(t, 8)? as usize;
            let text = t.get(12..12 + n.min(256))?;
            Some(String::from_utf8_lossy(text).trim_end_matches('\0').trim().to_string())
        }
        b"mluc" => {
            let records = be32(t, 8)? as usize;
            if records == 0 {
                return None;
            }
            let len = be32(t, 20)? as usize;
            let off = be32(t, 24)? as usize;
            let units: Vec<u16> = t
                .get(off..off.checked_add(len.min(512))?)?
                .chunks_exact(2)
                .map(|c| u16::from_be_bytes([c[0], c[1]]))
                .collect();
            Some(String::from_utf16_lossy(&units).trim_end_matches('\0').trim().to_string())
        }
        _ => None,
    }
}

/// A short description of a profile for the inspector, whether or not it can
/// be applied: its name, or what kind of profile it is.
pub fn describe_icc(profile: &[u8]) -> String {
    let space = profile.get(16..20).map(|s| String::from_utf8_lossy(s).trim().to_string()).unwrap_or_default();
    let name = (|| {
        let count = be32(profile, 128)? as usize;
        for i in 0..count.min(200) {
            let e = 132 + i * 12;
            if profile.get(e..e + 4)? == b"desc" {
                let off = be32(profile, e + 4)? as usize;
                let len = be32(profile, e + 8)? as usize;
                return description_of(profile.get(off..off.checked_add(len)?)?);
            }
        }
        None
    })();
    match name.filter(|n| !n.is_empty()) {
        Some(n) => format!("{n} ({space})"),
        None => format!("{space} profile"),
    }
}

//endregion

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matrix_inverse() {
        let m = [[2.0, 0.0, 1.0], [0.0, 3.0, 0.0], [1.0, 0.0, 1.0]];
        let i = mul(&m, &invert(&m).unwrap());
        for (r, row) in i.iter().enumerate() {
            for (c, v) in row.iter().enumerate() {
                assert!((v - IDENTITY[r][c]).abs() < 1e-12);
            }
        }
        assert!(invert(&[[1.0, 2.0, 3.0], [2.0, 4.0, 6.0], [1.0, 1.0, 1.0]]).is_none());
    }

    #[test]
    fn bt709_cicp_is_srgb() {
        let c = Conversion::from_cicp([1, 13, 0, 1]).unwrap();
        assert!(c.is_srgb(), "{c:?}");
        // The Rec. 709 curve is shown as sRGB's.
        for code in [1, 2, 6, 14, 15] {
            assert!(Conversion::from_cicp([1, code, 1, 1]).unwrap().is_srgb(), "transfer {code}");
        }
        // Other curves are not.
        assert!(!Conversion::from_cicp([1, 8, 1, 1]).unwrap().is_srgb());
        assert!(!Conversion::from_cicp([1, 4, 1, 1]).unwrap().is_srgb());
    }

    #[test]
    fn display_p3_red_is_outside_srgb() {
        // Pure P3 red is more saturated than sRGB can show: green and blue
        // come out negative, red above 1.
        let c = Conversion::from_cicp([12, 13, 0, 1]).unwrap();
        let lin = c.to_linear_rgba(&[255, 0, 0, 255], 8);
        assert!(
            (lin[0] - 1.2249).abs() < 0.001 && (lin[1] + 0.0421).abs() < 0.001 && (lin[2] + 0.0196).abs() < 0.001,
            "{lin:?}"
        );
        let mut px = [255, 0, 0, 255];
        c.convert_rgba8(&mut px);
        assert_eq!(px, [255, 0, 0, 255]);
        // A mid grey stays grey (the white points agree).
        let mut grey = [128, 128, 128, 200];
        c.convert_rgba8(&mut grey);
        assert_eq!(grey, [128, 128, 128, 200]);
    }

    #[test]
    fn dci_p3_white_is_adapted() {
        let c = Conversion::from_cicp([11, 13, 0, 1]).unwrap();
        let lin = c.to_linear_rgba(&[255, 255, 255, 255], 8);
        for v in &lin[..3] {
            assert!((v - 1.0).abs() < 0.01, "{lin:?}");
        }
    }

    #[test]
    fn pq_white_levels() {
        // PQ code 0.5081 is 100 nits and 0.5807 is 203 nits.
        let v = Transfer::Pq.decode(0.5807);
        assert!((v - 1.0).abs() < 0.02, "{v}");
        assert!(Transfer::Pq.decode(1.0) > 40.0);
        assert_eq!(Transfer::Pq.decode(0.0), 0.0);
    }

    #[test]
    fn hlg_reference_white() {
        // 75 % HLG signal is about 203 nits.
        let v = Transfer::Hlg.decode(0.75);
        assert!((v - 1.0).abs() < 0.1, "{v}");
        assert!(Transfer::Hlg.decode(1.0) > 4.0);
    }

    #[test]
    fn curves_are_monotonic() {
        for t in [Transfer::Srgb, Transfer::Pq, Transfer::Hlg, Transfer::Gamma(2.2)] {
            let lut = t.lut(1024);
            assert!(lut.windows(2).all(|w| w[1] >= w[0]), "{t:?}");
            assert_eq!(lut[0], 0.0);
        }
        // Out of range and NaN inputs stay finite.
        assert!(Transfer::Pq.decode(f64::NAN).is_finite());
        assert!(
            Transfer::Parametric(3, [2.4, 1.0 / 1.055, 0.055 / 1.055, 1.0 / 12.92, 0.04045, 0.0, 0.0])
                .decode(2.0)
                .is_finite()
        );
    }

    /// A minimal profile with the tags a matrix and curve profile needs.
    fn profile(colorants: Mat3, curve: &[u8], desc: &str) -> Vec<u8> {
        let tags: Vec<(&[u8; 4], Vec<u8>)> = {
            let xyz = |col: usize| {
                let mut t = b"XYZ \0\0\0\0".to_vec();
                for row in &colorants {
                    t.extend(((row[col] * 65536.0).round() as i32).to_be_bytes());
                }
                t
            };
            let mut d = b"desc\0\0\0\0".to_vec();
            d.extend(((desc.len() + 1) as u32).to_be_bytes());
            d.extend(desc.as_bytes());
            d.push(0);
            vec![
                (b"desc", d),
                (b"rXYZ", xyz(0)),
                (b"gXYZ", xyz(1)),
                (b"bXYZ", xyz(2)),
                (b"rTRC", curve.to_vec()),
                (b"gTRC", curve.to_vec()),
                (b"bTRC", curve.to_vec()),
            ]
        };
        let mut body = (tags.len() as u32).to_be_bytes().to_vec();
        let mut data = Vec::new();
        let base = 128 + 4 + tags.len() * 12;
        for (sig, t) in &tags {
            body.extend(*sig);
            body.extend(((base + data.len()) as u32).to_be_bytes());
            body.extend((t.len() as u32).to_be_bytes());
            data.extend(t);
            while data.len() % 4 != 0 {
                data.push(0);
            }
        }
        let mut p = vec![0u8; 128];
        p[16..20].copy_from_slice(b"RGB ");
        p[20..24].copy_from_slice(b"XYZ ");
        p[36..40].copy_from_slice(b"acsp");
        p.extend(body);
        p.extend(data);
        let len = p.len() as u32;
        p[..4].copy_from_slice(&len.to_be_bytes());
        p
    }

    fn gamma_curve(g: f64) -> Vec<u8> {
        let mut c = b"curv\0\0\0\0".to_vec();
        c.extend(1u32.to_be_bytes());
        c.extend(((g * 256.0).round() as u16).to_be_bytes());
        c
    }

    #[test]
    fn a_profile_that_is_srgb_changes_nothing() {
        let mut para = b"para\0\0\0\0\0\x03\0\0".to_vec();
        for v in [2.4, 1.0 / 1.055, 0.055 / 1.055, 1.0 / 12.92, 0.04045] {
            para.extend(((v * 65536.0f64).round() as i32).to_be_bytes());
        }
        let c = Conversion::from_icc(&profile(SRGB_D50, &para, "sRGB test")).unwrap();
        assert!(c.is_srgb(), "{c:?}");
        assert_eq!(c.name, "sRGB test");
    }

    #[test]
    fn adobe_rgb_is_wider_than_srgb() {
        // Adobe RGB (1998) colorants in D50, gamma 2.2.
        let adobe = [[0.60974, 0.20528, 0.14919], [0.31111, 0.62567, 0.06322], [0.01947, 0.06087, 0.74457]];
        let c = Conversion::from_icc(&profile(adobe, &gamma_curve(2.2), "Adobe RGB (1998)")).unwrap();
        assert!(!c.is_srgb());
        let mut green = [0, 255, 0, 255];
        c.convert_rgba8(&mut green);
        // Adobe's green is beyond sRGB's: red goes negative, green clips.
        assert_eq!((green[0], green[1], green[2]), (0, 255, 0));
        let mut grey = [100, 100, 100, 255];
        c.convert_rgba8(&mut grey);
        assert!((grey[0] as i32 - 100).abs() <= 2 && (grey[1] as i32 - 100).abs() <= 2, "{grey:?}");
    }

    #[test]
    fn lookup_table_and_broken_profiles_are_refused() {
        assert!(Conversion::from_icc(b"").is_none());
        assert!(Conversion::from_icc(&[0u8; 200]).is_none());
        let good = profile(SRGB_D50, &gamma_curve(2.2), "x");
        for cut in (0..good.len()).step_by(7) {
            // Truncations never panic.
            let _ = Conversion::from_icc(&good[..cut]);
        }
        let mut cmyk = good.clone();
        cmyk[16..20].copy_from_slice(b"CMYK");
        assert!(Conversion::from_icc(&cmyk).is_none());
        // A table claiming more entries than the profile holds.
        let mut lying = b"curv\0\0\0\0".to_vec();
        lying.extend(60000u32.to_be_bytes());
        assert!(Conversion::from_icc(&profile(SRGB_D50, &lying, "x")).is_none());
    }

    #[test]
    fn srgb_encoding_round_trips() {
        let t = Transfer::Srgb.lut(256);
        for i in 0..=255u8 {
            assert_eq!(encode_srgb8(t[i as usize]), i);
        }
        assert_eq!(encode_srgb8(f32::NAN), 0);
        assert_eq!(encode_srgb8(7.0), 255);
    }

    #[test]
    fn tone_mapping_keeps_mid_tones() {
        let c = tone_map([0.5, 0.3, 0.2]);
        assert!((c[0] - 0.46).abs() < 1e-5);
        let w = tone_map([100.0, 100.0, 100.0]);
        assert!(w[0] > 0.99 && w[0] <= 1.0);
    }
}
