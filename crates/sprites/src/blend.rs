//! Layer blending in 8 bits, with Aseprite's arithmetic so composites match
//! what Aseprite itself exports: straight (not premultiplied) alpha, its
//! rounding multiply, and its way of fading a blend mode in where the
//! backdrop is partly transparent.
//!
//! Every mode first works out a blended colour from the backdrop and the
//! source (the formulas of the PDF and W3C compositing specifications) and
//! then lays it over the backdrop like a normal layer with the source's
//! alpha.  Where the backdrop is translucent, the result is mixed back
//! towards a plain normal blend by the backdrop's alpha, so a multiply layer
//! over nothing looks like the layer itself rather than black.

/// A pixel: red, green, blue, alpha.
pub type Px = [u8; 4];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Blend {
    #[default]
    Normal,
    Multiply,
    Screen,
    Overlay,
    Darken,
    Lighten,
    ColorDodge,
    ColorBurn,
    HardLight,
    SoftLight,
    Difference,
    Exclusion,
    Hue,
    Saturation,
    Color,
    Luminosity,
    Addition,
    Subtract,
    Divide,
}

impl Blend {
    /// Aseprite's numbering, from its file format.  Unknown modes blend
    /// normally.
    pub fn from_aseprite(mode: u16) -> Blend {
        use Blend::*;
        const MODES: [Blend; 19] = [
            Normal, Multiply, Screen, Overlay, Darken, Lighten, ColorDodge, ColorBurn, HardLight, SoftLight,
            Difference, Exclusion, Hue, Saturation, Color, Luminosity, Addition, Subtract, Divide,
        ];
        MODES.get(mode as usize).copied().unwrap_or(Normal)
    }

    pub fn name(self) -> &'static str {
        use Blend::*;
        match self {
            Normal => "normal",
            Multiply => "multiply",
            Screen => "screen",
            Overlay => "overlay",
            Darken => "darken",
            Lighten => "lighten",
            ColorDodge => "color dodge",
            ColorBurn => "color burn",
            HardLight => "hard light",
            SoftLight => "soft light",
            Difference => "difference",
            Exclusion => "exclusion",
            Hue => "hue",
            Saturation => "saturation",
            Color => "color",
            Luminosity => "luminosity",
            Addition => "addition",
            Subtract => "subtract",
            Divide => "divide",
        }
    }

    /// Lays `src` over `backdrop` with this mode, the source's alpha scaled
    /// by `opacity`.
    #[inline]
    pub fn apply(self, backdrop: Px, src: Px, opacity: u8) -> Px {
        use Blend::*;
        let op = opacity as i32;
        match self {
            Normal => normal(backdrop, src, op),
            Multiply => separable(backdrop, src, op, mul_un8),
            Screen => separable(backdrop, src, op, screen),
            Overlay => separable(backdrop, src, op, |b, s| hard_light(s, b)),
            Darken => separable(backdrop, src, op, |b, s| b.min(s)),
            Lighten => separable(backdrop, src, op, |b, s| b.max(s)),
            ColorDodge => separable(backdrop, src, op, color_dodge),
            ColorBurn => separable(backdrop, src, op, color_burn),
            HardLight => separable(backdrop, src, op, hard_light),
            SoftLight => separable(backdrop, src, op, soft_light),
            Difference => separable(backdrop, src, op, |b, s| (b - s).abs()),
            Exclusion => separable(backdrop, src, op, |b, s| b + s - 2 * mul_un8(b, s)),
            Addition => separable(backdrop, src, op, |b, s| (b + s).min(255)),
            Subtract => separable(backdrop, src, op, |b, s| (b - s).max(0)),
            Divide => separable(backdrop, src, op, divide),
            Hue | Saturation | Color | Luminosity => {
                let blended = |b: Px, s: Px| {
                    let (bc, sc) = (unit(b), unit(s));
                    let c = match self {
                        Hue => set_lum(set_sat(sc, sat(bc)), lum(bc)),
                        Saturation => set_lum(set_sat(bc, sat(sc)), lum(bc)),
                        Color => set_lum(sc, lum(bc)),
                        _ => set_lum(bc, lum(sc)),
                    };
                    // Aseprite truncates rather than rounds here.
                    let to8 = |v: f64| ((v * 255.0) as i32).clamp(0, 255) as u8;
                    [to8(c[0]), to8(c[1]), to8(c[2]), s[3]]
                };
                fade_in(backdrop, src, op, blended(backdrop, src))
            }
        }
    }
}

/*┌─────────────────╮
| Arithmetic      |
└─────────────────╯*/
//region Arithmetic

/// `a * b / 255`, rounded, for `b` in 0..=255 (`a` may be negative).
#[inline]
pub fn mul_un8(a: i32, b: i32) -> i32 {
    let t = a * b + 0x80;
    ((t >> 8) + t) >> 8
}

/// `a * 255 / b`, rounded, for `b > 0`.
#[inline]
fn div_un8(a: i32, b: i32) -> i32 {
    (a * 255 + b / 2) / b
}

/// The source over the backdrop, with straight alpha.
#[inline]
pub fn normal(b: Px, s: Px, opacity: i32) -> Px {
    if b[3] == 0 {
        return [s[0], s[1], s[2], mul_un8(s[3] as i32, opacity) as u8];
    }
    if s[3] == 0 {
        return b;
    }
    let sa = mul_un8(s[3] as i32, opacity);
    let ra = sa + b[3] as i32 - mul_un8(b[3] as i32, sa);
    let mix = |bc: u8, sc: u8| (bc as i32 + (sc as i32 - bc as i32) * sa / ra) as u8;
    [mix(b[0], s[0]), mix(b[1], s[1]), mix(b[2], s[2]), ra as u8]
}

/// Moves `b` towards `s` by `amount` (0..=255), colour and alpha alike.
#[inline]
fn merge(b: Px, s: Px, amount: i32) -> Px {
    let rgb = |i: usize| {
        if b[3] == 0 {
            s[i]
        } else if s[3] == 0 {
            b[i]
        } else {
            (b[i] as i32 + mul_un8(s[i] as i32 - b[i] as i32, amount)) as u8
        }
    };
    let a = b[3] as i32 + mul_un8(s[3] as i32 - b[3] as i32, amount);
    if a == 0 { [0, 0, 0, 0] } else { [rgb(0), rgb(1), rgb(2), a as u8] }
}

/// A blend mode whose colour is `f(backdrop, source)` per channel.
#[inline]
fn separable(b: Px, s: Px, opacity: i32, f: impl Fn(i32, i32) -> i32) -> Px {
    let c = |i: usize| f(b[i] as i32, s[i] as i32).clamp(0, 255) as u8;
    fade_in(b, s, opacity, [c(0), c(1), c(2), s[3]])
}

/// Lays the blended colour over the backdrop, mixed back towards a normal
/// blend where the backdrop is translucent.
#[inline]
fn fade_in(b: Px, s: Px, opacity: i32, blended_src: Px) -> Px {
    if b[3] == 0 {
        return normal(b, s, opacity);
    }
    let plain = normal(b, s, opacity);
    let blended = normal(b, blended_src, opacity);
    let towards = merge(plain, blended, b[3] as i32);
    let src_alpha = mul_un8(s[3] as i32, opacity);
    merge(towards, blended, mul_un8(b[3] as i32, src_alpha))
}

fn screen(b: i32, s: i32) -> i32 {
    b + s - mul_un8(b, s)
}

fn hard_light(b: i32, s: i32) -> i32 {
    if s < 128 { mul_un8(b, s << 1) } else { screen(b, (s << 1) - 255) }
}

fn color_dodge(b: i32, s: i32) -> i32 {
    let s = 255 - s;
    if b == 0 {
        0
    } else if b >= s {
        255
    } else {
        div_un8(b, s)
    }
}

fn color_burn(b: i32, s: i32) -> i32 {
    let b = 255 - b;
    if b == 0 {
        255
    } else if b >= s {
        0
    } else {
        255 - div_un8(b, s)
    }
}

fn divide(b: i32, s: i32) -> i32 {
    if b == 0 {
        0
    } else if b >= s {
        255
    } else {
        div_un8(b, s)
    }
}

/// The PDF specification's soft light, in floating point as Aseprite does.
fn soft_light(b: i32, s: i32) -> i32 {
    let (b, s) = (b as f64 / 255.0, s as f64 / 255.0);
    let d = if b <= 0.25 { ((16.0 * b - 12.0) * b + 4.0) * b } else { b.sqrt() };
    let r = if s <= 0.5 { b - (1.0 - 2.0 * s) * b * (1.0 - b) } else { b + (2.0 * s - 1.0) * (d - b) };
    (r * 255.0 + 0.5) as i32
}

//endregion

/*┌─────────────────╮
| Hue and colour  |
└─────────────────╯*/
//region Hue and colour

fn unit(p: Px) -> [f64; 3] {
    [p[0] as f64 / 255.0, p[1] as f64 / 255.0, p[2] as f64 / 255.0]
}

fn lum(c: [f64; 3]) -> f64 {
    0.3 * c[0] + 0.59 * c[1] + 0.11 * c[2]
}

fn sat(c: [f64; 3]) -> f64 {
    c[0].max(c[1]).max(c[2]) - c[0].min(c[1]).min(c[2])
}

fn set_lum(c: [f64; 3], l: f64) -> [f64; 3] {
    let d = l - lum(c);
    clip_color([c[0] + d, c[1] + d, c[2] + d])
}

fn clip_color(mut c: [f64; 3]) -> [f64; 3] {
    let l = lum(c);
    let min = c[0].min(c[1]).min(c[2]);
    let max = c[0].max(c[1]).max(c[2]);
    if min < 0.0 {
        c = c.map(|v| l + (v - l) * l / (l - min));
    }
    if max > 1.0 {
        c = c.map(|v| l + (v - l) * (1.0 - l) / (max - l));
    }
    c
}

/// Gives `c` the saturation `s`, keeping its hue.  The smallest, middle
/// and largest channels are picked with Aseprite's comparisons, which can
/// name the same channel twice when two are equal; matching that keeps
/// composites identical to Aseprite's even where the specification's
/// answer differs.
fn set_sat(mut c: [f64; 3], s: f64) -> [f64; 3] {
    let [r, g, b] = c;
    let min = if r < g.min(b) {
        0
    } else if g < b {
        1
    } else {
        2
    };
    let max = if r > g.max(b) {
        0
    } else if g > b {
        1
    } else {
        2
    };
    let mid = if r > g {
        if g > b {
            1
        } else if r > b {
            2
        } else {
            0
        }
    } else if g > b {
        if b > r { 2 } else { 0 }
    } else {
        1
    };
    if c[max] > c[min] {
        c[mid] = (c[mid] - c[min]) * s / (c[max] - c[min]);
        c[max] = s;
    } else {
        c[mid] = 0.0;
        c[max] = 0.0;
    }
    c[min] = 0.0;
    c
}

//endregion

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normal_matches_hand_arithmetic() {
        // Opaque over opaque at half opacity: halfway, rounded down.
        assert_eq!(normal([0, 205, 249, 255], [237, 118, 20, 255], 128), [118, 162, 135, 255]);
        // Over nothing: the source with its alpha scaled.
        assert_eq!(normal([0, 0, 0, 0], [10, 20, 30, 200], 128), [10, 20, 30, 100]);
        // Nothing over something leaves it alone.
        assert_eq!(normal([1, 2, 3, 4], [9, 9, 9, 0], 255), [1, 2, 3, 4]);
        assert_eq!(mul_un8(255, 255), 255);
        assert_eq!(mul_un8(128, 255), 128);
        assert_eq!(mul_un8(-70, 128), -35);
    }

    #[test]
    fn modes_over_opaque_backdrops() {
        let b = [200, 100, 50, 255];
        let s = [100, 200, 250, 255];
        let full = |m: Blend| m.apply(b, s, 255);
        assert_eq!(full(Blend::Multiply), [78, 78, 49, 255]);
        assert_eq!(full(Blend::Screen), [222, 222, 251, 255]);
        assert_eq!(full(Blend::Darken), [100, 100, 50, 255]);
        assert_eq!(full(Blend::Lighten), [200, 200, 250, 255]);
        assert_eq!(full(Blend::Difference), [100, 100, 200, 255]);
        assert_eq!(full(Blend::Addition), [255, 255, 255, 255]);
        assert_eq!(full(Blend::Subtract), [100, 0, 0, 255]);
        // Exclusion: b + s - 2bs.
        assert_eq!(full(Blend::Exclusion), [144, 144, 202, 255]);
        // Divide: b / s, capped.
        assert_eq!(full(Blend::Divide), [255, 128, 51, 255]);
        // Luminosity keeps the backdrop's colour at the source's lightness;
        // a grey source makes it that grey's lightness.
        let grey = Blend::Luminosity.apply([255, 0, 0, 255], [128, 128, 128, 255], 255);
        let l = 0.3 * grey[0] as f64 + 0.59 * grey[1] as f64 + 0.11 * grey[2] as f64;
        assert!((l - 128.0).abs() < 2.0, "{grey:?}");
        assert!(grey[0] > grey[1] && grey[1] == grey[2], "{grey:?}");
        // A transparent source changes nothing, whatever the mode.
        for m in 0..19 {
            assert_eq!(Blend::from_aseprite(m).apply(b, [1, 2, 3, 0], 255), b);
        }
    }

    #[test]
    fn half_opacity_mixes_halfway() {
        let b = [200, 100, 50, 255];
        let s = [100, 200, 250, 255];
        let half = Blend::Multiply.apply(b, s, 128);
        // Halfway between the backdrop and the full multiply.
        assert_eq!(half, [139, 89, 50, 255]);
    }
}
