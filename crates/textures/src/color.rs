//! Colour helpers: sRGB transfer functions, half floats, shared-exponent
//! formats and the Khronos PBR Neutral tone mapper.

use std::sync::OnceLock;

/// sRGB-encoded byte to linear value, for all 256 bytes.
pub(crate) fn srgb_to_linear_table() -> &'static [f32; 256] {
    static TABLE: OnceLock<[f32; 256]> = OnceLock::new();
    TABLE.get_or_init(|| std::array::from_fn(|i| srgb_to_linear(i as f32 / 255.0)))
}

pub(crate) fn srgb_to_linear(v: f32) -> f32 {
    if v <= 0.04045 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
}

pub(crate) fn linear_to_srgb(v: f32) -> f32 {
    if v <= 0.0031308 { v * 12.92 } else { 1.055 * v.powf(1.0 / 2.4) - 0.055 }
}

const ENCODE_STEPS: usize = 1 << 14;

/// Encodes a linear value in 0..1 as an sRGB byte.  A table with 16384
/// steps is exact to the byte except in the darkest few values, and far
/// cheaper than `powf` per channel.
pub(crate) fn linear_to_srgb8(v: f32) -> u8 {
    static TABLE: OnceLock<Vec<u8>> = OnceLock::new();
    let table = TABLE.get_or_init(|| {
        (0..ENCODE_STEPS).map(|i| (linear_to_srgb(i as f32 / (ENCODE_STEPS - 1) as f32) * 255.0 + 0.5) as u8).collect()
    });
    // NaN fails both comparisons and lands on 0.
    let v = if v > 0.0 { v.min(1.0) } else { 0.0 };
    table[(v * (ENCODE_STEPS - 1) as f32 + 0.5) as usize]
}

pub(crate) fn unorm8(v: f32) -> u8 {
    // NaN becomes 0 through the saturating cast.
    (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8
}

/// IEEE 754 half to single precision, including subnormals, infinities and
/// NaN.
pub(crate) fn half_to_f32(h: u16) -> f32 {
    let sign = ((h as u32) & 0x8000) << 16;
    let exp = ((h >> 10) & 0x1f) as u32;
    let man = (h & 0x3ff) as u32;
    let bits = match exp {
        0 if man == 0 => sign,
        0 => {
            // Subnormal: normalise the mantissa.
            let shift = man.leading_zeros() - 21;
            let man = (man << shift) & 0x3ff;
            sign | ((113 - shift) << 23) | (man << 13)
        }
        31 => sign | 0x7f80_0000 | (man << 13),
        _ => sign | ((exp + 112) << 23) | (man << 13),
    };
    f32::from_bits(bits)
}

/// An unsigned float with a 5-bit exponent and `man_bits` mantissa bits, as
/// used by B10G11R11_UFLOAT.
pub(crate) fn small_ufloat(v: u32, man_bits: u32) -> f32 {
    let man = v & ((1 << man_bits) - 1);
    let exp = (v >> man_bits) & 0x1f;
    match exp {
        0 => man as f32 * 2f32.powi(-14 - man_bits as i32),
        31 => {
            if man == 0 {
                f32::INFINITY
            } else {
                f32::NAN
            }
        }
        _ => f32::from_bits(((exp + 112) << 23) | (man << (23 - man_bits))),
    }
}

/// E5B9G9R9: three 9-bit mantissas sharing a 5-bit exponent.
pub(crate) fn rgb9e5(v: u32) -> [f32; 3] {
    let scale = 2f32.powi(((v >> 27) & 0x1f) as i32 - 15 - 9);
    [(v & 0x1ff) as f32 * scale, ((v >> 9) & 0x1ff) as f32 * scale, ((v >> 18) & 0x1ff) as f32 * scale]
}

/// Rec. 709 luminance of linear RGB.
pub(crate) fn luminance(r: f32, g: f32, b: f32) -> f32 {
    0.2126 * r + 0.7152 * g + 0.0722 * b
}

/// Khronos PBR Neutral tone mapping, which keeps base colours accurate up to
/// a bright threshold and compresses highlights above it smoothly towards
/// white.  Input and output are linear.
pub(crate) fn pbr_neutral(c: [f32; 3]) -> [f32; 3] {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn halves() {
        assert_eq!(half_to_f32(0x3c00), 1.0);
        assert_eq!(half_to_f32(0xc000), -2.0);
        assert_eq!(half_to_f32(0x7bff), 65504.0);
        assert_eq!(half_to_f32(0x0001), 2f32.powi(-24));
        assert_eq!(half_to_f32(0x0200), 2f32.powi(-15));
        assert!(half_to_f32(0x7c00).is_infinite());
        assert!(half_to_f32(0x7e00).is_nan());
    }

    #[test]
    fn srgb_round_trip() {
        let table = srgb_to_linear_table();
        for i in 0..=255u8 {
            assert_eq!(linear_to_srgb8(table[i as usize]), i);
        }
        assert_eq!(linear_to_srgb8(f32::NAN), 0);
        assert_eq!(linear_to_srgb8(5.0), 255);
    }

    #[test]
    fn neutral_keeps_mid_tones() {
        let c = pbr_neutral([0.5, 0.3, 0.2]);
        assert!((c[0] - 0.46).abs() < 1e-5 && (c[2] - 0.16).abs() < 1e-5);
        let w = pbr_neutral([100.0, 100.0, 100.0]);
        assert!(w[0] > 0.99 && w[0] <= 1.0);
    }

    #[test]
    fn packed_floats() {
        assert_eq!(small_ufloat(15 << 6, 6), 1.0);
        assert_eq!(rgb9e5((15 << 27) | (256 << 18) | (256 << 9) | 256), [0.5, 0.5, 0.5]);
    }
}
