//! Colours and the colour arithmetic KDE uses to derive one colour from
//! another.  Breeze draws most frames, separators and grooves as mixes of a
//! foreground into a background (KColorUtils::mix), so matching it means
//! doing the same arithmetic in the same space: plain sRGB, not linear.

use egui::Color32;

/// An sRGB colour with straight (not premultiplied) alpha, components in
/// 0..=1.  egui's `Color32` is premultiplied, which makes mixing awkward, so
/// the theme works in this type and converts at the end.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rgba {
    pub r: f32,
    pub g: f32,
    pub b: f32,
    pub a: f32,
}

impl Rgba {
    pub const TRANSPARENT: Rgba = Rgba { r: 0.0, g: 0.0, b: 0.0, a: 0.0 };
    pub const WHITE: Rgba = Rgba { r: 1.0, g: 1.0, b: 1.0, a: 1.0 };
    pub const BLACK: Rgba = Rgba { r: 0.0, g: 0.0, b: 0.0, a: 1.0 };

    pub const fn rgb8(r: u8, g: u8, b: u8) -> Rgba {
        Rgba { r: r as f32 / 255.0, g: g as f32 / 255.0, b: b as f32 / 255.0, a: 1.0 }
    }

    pub const fn rgba8(r: u8, g: u8, b: u8, a: u8) -> Rgba {
        Rgba { r: r as f32 / 255.0, g: g as f32 / 255.0, b: b as f32 / 255.0, a: a as f32 / 255.0 }
    }

    pub fn from_f32(r: f32, g: f32, b: f32, a: f32) -> Rgba {
        Rgba { r: clamp01(r), g: clamp01(g), b: clamp01(b), a: clamp01(a) }
    }

    /// Parses a colour the ways KConfig writes them: `r,g,b`, `r,g,b,a`, or
    /// `#rrggbb` / `#aarrggbb` (QColor's name format).
    pub fn parse(text: &str) -> Option<Rgba> {
        let text = text.trim();
        if let Some(hex) = text.strip_prefix('#') {
            let value = u32::from_str_radix(hex, 16).ok()?;
            return match hex.len() {
                6 => Some(Rgba::rgb8((value >> 16) as u8, (value >> 8) as u8, value as u8)),
                8 => Some(Rgba::rgba8((value >> 16) as u8, (value >> 8) as u8, value as u8, (value >> 24) as u8)),
                3 => {
                    let nibble = |shift: u32| (((value >> shift) & 0xf) * 17) as u8;
                    Some(Rgba::rgb8(nibble(8), nibble(4), nibble(0)))
                }
                _ => None,
            };
        }
        let mut parts = text.split(',').map(|p| p.trim().parse::<i64>().ok().map(|v| v.clamp(0, 255) as u8));
        let r = parts.next()??;
        let g = parts.next()??;
        let b = parts.next()??;
        let a = match parts.next() {
            Some(a) => a?,
            None => 255,
        };
        if parts.next().is_some() {
            return None;
        }
        Some(Rgba::rgba8(r, g, b, a))
    }

    pub fn to_u8(self) -> [u8; 4] {
        let q = |v: f32| (clamp01(v) * 255.0).round() as u8;
        [q(self.r), q(self.g), q(self.b), q(self.a)]
    }

    pub fn to_color32(self) -> Color32 {
        let [r, g, b, a] = self.to_u8();
        Color32::from_rgba_unmultiplied(r, g, b, a)
    }

    /// `#rrggbb`, for SVG style sheets.  Alpha is dropped.
    pub fn to_hex(self) -> String {
        let [r, g, b, _] = self.to_u8();
        format!("#{r:02x}{g:02x}{b:02x}")
    }

    /// The same colour with its alpha multiplied by `alpha`, like Breeze's
    /// `Helper::alphaColor`.
    pub fn alpha(self, alpha: f32) -> Rgba {
        Rgba { a: clamp01(self.a * alpha), ..self }
    }

    /// The same colour with exactly this alpha, like Breeze's
    /// `transparentize`.
    pub fn with_alpha(self, alpha: f32) -> Rgba {
        Rgba { a: clamp01(alpha), ..self }
    }

    /// KColorUtils::mix: interpolates premultiplied components in sRGB, so
    /// `mix(bg, fg, 0.2)` is the 20 % foreground tint Breeze draws frames in.
    pub fn mix(self, other: Rgba, bias: f32) -> Rgba {
        if bias.is_nan() || bias <= 0.0 {
            return self;
        }
        if bias >= 1.0 {
            return other;
        }
        let a = lerp(self.a, other.a, bias);
        if a <= 0.0 {
            return Rgba::TRANSPARENT;
        }
        let channel = |c1: f32, c2: f32| clamp01(lerp(c1 * self.a, c2 * other.a, bias)) / a;
        Rgba::from_f32(channel(self.r, other.r), channel(self.g, other.g), channel(self.b, other.b), a)
    }

    /// Paints `paint` over this colour with source-over blending, like
    /// KColorUtils::overlayColors.  The result is opaque when this colour
    /// is.
    pub fn over(self, base: Rgba) -> Rgba {
        let a = self.a + base.a * (1.0 - self.a);
        if a <= 0.0 {
            return Rgba::TRANSPARENT;
        }
        let channel = |top: f32, bottom: f32| (top * self.a + bottom * base.a * (1.0 - self.a)) / a;
        Rgba::from_f32(channel(self.r, base.r), channel(self.g, base.g), channel(self.b, base.b), a)
    }

    /// KDE's luma (KColorUtils::luma): Rec. 709 weights on gamma-2.2
    /// linearised channels.
    pub fn luma(self) -> f32 {
        let lin = |c: f32| clamp01(c).powf(2.2);
        0.2126 * lin(self.r) + 0.7152 * lin(self.g) + 0.0722 * lin(self.b)
    }

    /// KColorUtils::contrastRatio, the WCAG ratio on KDE's luma.
    pub fn contrast_ratio(self, other: Rgba) -> f32 {
        let (y1, y2) = (self.luma(), other.luma());
        let (hi, lo) = if y1 > y2 { (y1, y2) } else { (y2, y1) };
        (hi + 0.05) / (lo + 0.05)
    }

    /// QColor::darker(factor): divides HSV value by `factor / 100`.  Breeze
    /// uses it for pressed check boxes and inactive tabs.
    pub fn darker(self, factor: f32) -> Rgba {
        if factor <= 0.0 {
            return self;
        }
        let (h, s, v) = rgb_to_hsv(self.r, self.g, self.b);
        let (r, g, b) = hsv_to_rgb(h, s, v * 100.0 / factor);
        Rgba::from_f32(r, g, b, self.a)
    }

    /// QColor::lighter(factor): multiplies HSV value by `factor / 100`, and
    /// once value saturates, lowers saturation, as Qt does.
    pub fn lighter(self, factor: f32) -> Rgba {
        if factor <= 0.0 {
            return self;
        }
        let (h, mut s, mut v) = rgb_to_hsv(self.r, self.g, self.b);
        v *= factor / 100.0;
        if v > 1.0 {
            s = (s - (v - 1.0)).max(0.0);
            v = 1.0;
        }
        let (r, g, b) = hsv_to_rgb(h, s, v);
        Rgba::from_f32(r, g, b, self.a)
    }

    /// Whether this is a dark colour, judged the way KDE picks light or dark
    /// icon variants: by its luma against the middle grey.
    pub fn is_dark(self) -> bool {
        self.luma() < 0.25
    }
}

fn clamp01(v: f32) -> f32 {
    if v.is_nan() { 0.0 } else { v.clamp(0.0, 1.0) }
}

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

fn rgb_to_hsv(r: f32, g: f32, b: f32) -> (f32, f32, f32) {
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let d = max - min;
    let h = if d <= 0.0 {
        0.0
    } else if max == r {
        ((g - b) / d).rem_euclid(6.0)
    } else if max == g {
        (b - r) / d + 2.0
    } else {
        (r - g) / d + 4.0
    };
    let s = if max <= 0.0 { 0.0 } else { d / max };
    (h / 6.0, s, max)
}

fn hsv_to_rgb(h: f32, s: f32, v: f32) -> (f32, f32, f32) {
    let v = clamp01(v);
    let h6 = (h.rem_euclid(1.0)) * 6.0;
    let c = v * s;
    let x = c * (1.0 - ((h6 % 2.0) - 1.0).abs());
    let (r, g, b) = match h6 as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = v - c;
    (r + m, g + m, b + m)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_kconfig_colours() {
        assert_eq!(Rgba::parse("41,44,48"), Some(Rgba::rgb8(41, 44, 48)));
        assert_eq!(Rgba::parse(" 1, 2, 3, 4 "), Some(Rgba::rgba8(1, 2, 3, 4)));
        assert_eq!(Rgba::parse("#3daee9"), Some(Rgba::rgb8(0x3d, 0xae, 0xe9)));
        assert_eq!(Rgba::parse("#803daee9"), Some(Rgba::rgba8(0x3d, 0xae, 0xe9, 0x80)));
        for bad in ["", "1,2", "a,b,c", "1,2,3,4,5", "#12345", "#zzzzzz", ","] {
            assert_eq!(Rgba::parse(bad), None, "{bad:?}");
        }
        // Out-of-range numbers clamp rather than wrap, as QColor does.
        assert_eq!(Rgba::parse("300,-5,10"), Some(Rgba::rgb8(255, 0, 10)));
    }

    #[test]
    fn mixes_like_kcolorutils() {
        // Breeze Dark's separator: Window 32,35,38 mixed 20 % towards
        // WindowText 252,252,252.
        let separator = Rgba::rgb8(32, 35, 38).mix(Rgba::rgb8(252, 252, 252), 0.2);
        assert_eq!(separator.to_u8(), [76, 78, 81, 255]);
        let half = Rgba::BLACK.alpha(0.5).over(Rgba::WHITE);
        assert_eq!(half.to_u8(), [128, 128, 128, 255]);
        // Plasma's accent background: the accent at 70 % over the view
        // background (Plasma truncates where this rounds, writing 5,85,185).
        let accent = Rgba::rgb8(0, 112, 255).with_alpha(0.7).over(Rgba::rgb8(20, 22, 24));
        assert_eq!(accent.to_u8(), [6, 85, 186, 255]);
    }

    #[test]
    fn darker_and_lighter_follow_qcolor() {
        assert_eq!(Rgba::rgb8(200, 100, 50).darker(200.0).to_u8(), [100, 50, 25, 255]);
        assert_eq!(Rgba::rgb8(100, 50, 25).lighter(200.0).to_u8(), [200, 100, 50, 255]);
    }
}
