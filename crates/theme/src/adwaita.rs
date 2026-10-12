//! libadwaita's palette (GNOME 47 and later), expressed as KDE colour sets
//! so the same egui mapping serves both desktops.  The values are
//! libadwaita's named colours from `src/stylesheet/_colors.scss`; GTK
//! themes other than Adwaita can't be read without a CSS engine, so other
//! GTK desktops get Adwaita with their light or dark preference.

use crate::color::Rgba;
use crate::scheme::{ColorScheme, ColorSet};

/// The Adwaita colour scheme, light or dark, with an optional accent (the
/// default is GNOME's blue, `#3584e4`).
pub fn scheme(dark: bool, accent: Option<Rgba>, high_contrast: bool) -> ColorScheme {
    let accent_bg = accent.unwrap_or(Rgba::rgb8(0x35, 0x84, 0xe4));
    // libadwaita derives readable accent text from the accent: darker on
    // light backgrounds, lighter on dark ones.
    let accent_fg = if dark { accent_bg.lighter(150.0) } else { accent_bg.darker(130.0) };

    // 1. Backgrounds and the near-black or white foreground, flattened onto
    //    each background since libadwaita's light foreground is translucent.
    let (window_bg, view_bg, header_bg, sidebar_bg, popover_bg) = if dark {
        (
            Rgba::rgb8(0x22, 0x22, 0x26),
            Rgba::rgb8(0x1d, 0x1d, 0x20),
            Rgba::rgb8(0x2e, 0x2e, 0x32),
            Rgba::rgb8(0x2e, 0x2e, 0x32),
            Rgba::rgb8(0x36, 0x36, 0x3a),
        )
    } else {
        (Rgba::rgb8(0xfa, 0xfa, 0xfb), Rgba::WHITE, Rgba::WHITE, Rgba::rgb8(0xeb, 0xeb, 0xed), Rgba::WHITE)
    };
    let fg = if dark { Rgba::WHITE } else { Rgba::rgba8(0, 0, 6, 204) };
    let (negative, neutral, positive) = if dark {
        (Rgba::rgb8(0xff, 0x93, 0x8c), Rgba::rgb8(0xff, 0xc2, 0x52), Rgba::rgb8(0x78, 0xe9, 0xab))
    } else {
        (Rgba::rgb8(0xc3, 0x00, 0x00), Rgba::rgb8(0x90, 0x54, 0x00), Rgba::rgb8(0x00, 0x7c, 0x3d))
    };
    let set = |bg: Rgba| {
        let text = fg.over(bg);
        ColorSet {
            background: bg,
            alternate_background: fg.with_alpha(0.03).over(bg),
            foreground: text,
            inactive: fg.with_alpha(if high_contrast { 0.8 } else { 0.55 }).over(bg),
            active: accent_fg,
            link: accent_fg,
            visited: accent_fg.mix(Rgba::rgb8(0x91, 0x41, 0xac), 0.5),
            negative,
            neutral,
            positive,
            focus: accent_bg.with_alpha(0.5).over(bg),
            hover: accent_bg,
        }
    };
    // 2. Buttons are the foreground at 10 % over the window.
    let button_bg = fg.with_alpha(0.1).over(window_bg);
    let selection = ColorSet {
        background: accent_bg,
        alternate_background: accent_bg.with_alpha(0.3).over(view_bg),
        foreground: Rgba::WHITE,
        inactive: Rgba::WHITE.with_alpha(0.7).over(accent_bg),
        active: Rgba::WHITE,
        ..set(accent_bg)
    };
    let tooltip_bg = Rgba::BLACK.with_alpha(0.8).over(window_bg);
    let tooltip = ColorSet { foreground: Rgba::WHITE, inactive: Rgba::rgb8(0xc0, 0xc0, 0xc0), ..set(tooltip_bg) };
    let complementary_bg = if dark { Rgba::rgb8(0xfa, 0xfa, 0xfb) } else { Rgba::rgb8(0x22, 0x22, 0x26) };
    let complementary = ColorSet {
        foreground: if dark { Rgba::rgba8(0, 0, 6, 204).over(complementary_bg) } else { Rgba::WHITE },
        ..set(complementary_bg)
    };
    let mut button = set(button_bg);
    button.alternate_background = accent_bg;
    ColorScheme {
        name: Some(if dark { "Adwaita Dark".into() } else { "Adwaita".into() }),
        window: set(window_bg),
        view: set(view_bg),
        button,
        selection,
        tooltip,
        header: ColorSet { alternate_background: sidebar_bg, ..set(header_bg) },
        complementary: ColorSet { alternate_background: popover_bg, ..complementary },
        accent: Some(accent_bg),
        frame_contrast: if high_contrast { 0.5 } else { 0.15 },
        disabled_fade: 0.5,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn light_and_dark() {
        let light = scheme(false, None, false);
        assert!(!light.is_dark());
        assert_eq!(light.view.background, Rgba::WHITE);
        let dark = scheme(true, Some(Rgba::rgb8(0xe6, 0x2d, 0x42)), false);
        assert!(dark.is_dark());
        assert_eq!(dark.selection.background, Rgba::rgb8(0xe6, 0x2d, 0x42));
        assert!(dark.window.foreground.contrast_ratio(dark.window.background) > 10.0);
    }
}
