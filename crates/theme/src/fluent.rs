//! Windows 11: the Fluent palette of WinUI's theme resources, expressed as
//! KDE colour sets so the same egui mapping serves every desktop, with the
//! light or dark app mode, the accent colour and the text size read from
//! the user's settings.
//!
//! WinUI derives its accent shades from the palette Windows stores next to
//! the accent (`AccentPalette`: three lighter, the accent, three darker):
//! accent-coloured fills use the first darker shade in light mode and the
//! second lighter one in dark mode, with black text on them in dark mode.

use crate::color::Rgba;
use crate::fonts::{FontSpec, Fonts, Hinting};
use crate::scheme::{ColorScheme, ColorSet};
use crate::{Desktop, DesktopTheme, Environment, Metrics};

/// Windows' default accent, `#0078d4`, and its palette.
const DEFAULT_PALETTE: [Rgba; 7] = [
    Rgba::rgb8(0x99, 0xeb, 0xff),
    Rgba::rgb8(0x4c, 0xc2, 0xff),
    Rgba::rgb8(0x00, 0x91, 0xf8),
    Rgba::rgb8(0x00, 0x78, 0xd4),
    Rgba::rgb8(0x00, 0x67, 0xc0),
    Rgba::rgb8(0x00, 0x3e, 0x92),
    Rgba::rgb8(0x00, 0x1a, 0x68),
];

/// What the user's settings say; `None` where a setting is missing.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Settings {
    /// `AppsUseLightTheme` is 0.
    pub dark: Option<bool>,
    /// DWM's colourisation colour, which is the accent colour.
    pub accent: Option<Rgba>,
    /// The accent's shades, lightest first, from `AccentPalette`.
    pub palette: Option<[Rgba; 7]>,
    /// Settings, Accessibility, Text size, as a factor (1.0 to 2.25).
    pub text_scale: Option<f32>,
}

/// The accent's seven shades: the stored palette when it belongs to this
/// accent, else shades mixed towards white and black.
pub fn accent_palette(accent: Option<Rgba>, stored: Option<[Rgba; 7]>) -> [Rgba; 7] {
    let Some(accent) = accent else { return stored.unwrap_or(DEFAULT_PALETTE) };
    match stored {
        Some(p) if distance(p[3], accent) < 0.02 => p,
        _ => [
            accent.mix(Rgba::WHITE, 0.6),
            accent.mix(Rgba::WHITE, 0.4),
            accent.mix(Rgba::WHITE, 0.2),
            accent,
            accent.mix(Rgba::BLACK, 0.15),
            accent.mix(Rgba::BLACK, 0.4),
            accent.mix(Rgba::BLACK, 0.65),
        ],
    }
}

/// The largest difference of two colours' channels, 0 to 1.
fn distance(a: Rgba, b: Rgba) -> f32 {
    let (a, b) = (a.to_u8(), b.to_u8());
    (0..3).map(|i| a[i].abs_diff(b[i])).max().unwrap_or(0) as f32 / 255.0
}

/// The Fluent colour scheme, light or dark, for an accent palette.
pub fn scheme(dark: bool, palette: [Rgba; 7]) -> ColorScheme {
    // 1. WinUI's base colours: SolidBackgroundFillColorBase for windows,
    //    cards and fields a step lighter, text as translucent black or
    //    white flattened onto each background.
    let (window_bg, view_bg, header_bg, menu_bg) = if dark {
        (
            Rgba::rgb8(0x20, 0x20, 0x20),
            Rgba::rgb8(0x1c, 0x1c, 0x1c),
            Rgba::rgb8(0x20, 0x20, 0x20),
            Rgba::rgb8(0x2c, 0x2c, 0x2c),
        )
    } else {
        (
            Rgba::rgb8(0xf3, 0xf3, 0xf3),
            Rgba::rgb8(0xff, 0xff, 0xff),
            Rgba::rgb8(0xee, 0xee, 0xee),
            Rgba::rgb8(0xfc, 0xfc, 0xfc),
        )
    };
    let ink = if dark { Rgba::WHITE } else { Rgba::BLACK };
    let (primary, secondary) = if dark { (1.0, 0.786) } else { (0.894, 0.62) };
    // AccentFillColorDefault, and the text drawn on it.
    let (accent_fill, on_accent, accent_text) =
        if dark { (palette[1], Rgba::BLACK, palette[0]) } else { (palette[4], Rgba::WHITE, palette[5]) };
    let (negative, neutral, positive) = if dark {
        (Rgba::rgb8(0xff, 0x99, 0xa4), Rgba::rgb8(0xfc, 0xe1, 0x00), Rgba::rgb8(0x6c, 0xcb, 0x5f))
    } else {
        (Rgba::rgb8(0xc4, 0x2b, 0x1c), Rgba::rgb8(0x9d, 0x5d, 0x00), Rgba::rgb8(0x0f, 0x7b, 0x0f))
    };
    let set = |bg: Rgba| ColorSet {
        background: bg,
        alternate_background: ink.with_alpha(if dark { 0.04 } else { 0.02 }).over(bg),
        foreground: ink.with_alpha(primary).over(bg),
        inactive: ink.with_alpha(secondary).over(bg),
        active: accent_text,
        link: accent_text,
        visited: accent_text.mix(Rgba::rgb8(0x88, 0x17, 0x98), 0.4),
        negative,
        neutral,
        positive,
        focus: accent_fill,
        hover: accent_fill,
    };

    // 2. Buttons are ControlFillColorDefault over the window: white at 70 %
    //    in light mode, white at 6 % in dark mode.
    let button_bg =
        if dark { Rgba::WHITE.with_alpha(0.0605).over(window_bg) } else { Rgba::WHITE.with_alpha(0.7).over(window_bg) };
    let mut button = set(button_bg);
    button.alternate_background = accent_fill;
    let selection = ColorSet {
        background: accent_fill,
        alternate_background: accent_fill.with_alpha(0.3).over(view_bg),
        foreground: on_accent,
        inactive: on_accent.with_alpha(0.7).over(accent_fill),
        active: on_accent,
        ..set(accent_fill)
    };
    let tooltip = set(menu_bg);
    let complementary = scheme_complement(dark, accent_fill);
    ColorScheme {
        name: Some(if dark { "Windows Dark".into() } else { "Windows Light".into() }),
        window: set(window_bg),
        view: set(view_bg),
        button,
        selection,
        tooltip,
        header: set(header_bg),
        complementary: ColorSet { alternate_background: menu_bg, ..complementary },
        accent: Some(palette[3]),
        // ControlStrokeColorDefault is black at 6 % (white at 7 % in dark
        // mode); frames a little stronger read better in egui, which has
        // no bottom-edge shading.
        frame_contrast: 0.12,
        disabled_fade: 0.6,
    }
}

/// The inverted set, for dark areas in light mode and the reverse.
fn scheme_complement(dark: bool, accent: Rgba) -> ColorSet {
    let bg = if dark { Rgba::rgb8(0xf3, 0xf3, 0xf3) } else { Rgba::rgb8(0x20, 0x20, 0x20) };
    let fg = if dark { Rgba::rgb8(0x1b, 0x1b, 0x1b) } else { Rgba::WHITE };
    ColorSet {
        background: bg,
        alternate_background: bg,
        foreground: fg,
        inactive: fg.mix(bg, 0.4),
        active: accent,
        link: accent,
        visited: accent,
        negative: fg,
        neutral: fg,
        positive: fg,
        focus: accent,
        hover: accent,
    }
}

/// The theme from `settings`: Segoe UI at Windows 11's 14 px body size
/// (10.5 pt), scaled by the text size setting, and Fluent's metrics.
pub fn theme(settings: &Settings) -> DesktopTheme {
    let dark = settings.dark.unwrap_or(false);
    let palette = accent_palette(settings.accent, settings.palette);
    let scale = settings.text_scale.unwrap_or(1.0).clamp(1.0, 2.25);
    let general = FontSpec::new("Segoe UI", 10.5 * scale);
    let fixed = FontSpec::new("Consolas", 10.5 * scale);
    let small = FontSpec { size_pt: 9.0 * scale, ..general.clone() };
    DesktopTheme {
        desktop: Desktop::Windows,
        dark,
        colors: scheme(dark, palette),
        fonts: Fonts {
            general,
            fixed,
            small,
            dpi: 96.0,
            // ClearType snaps to the pixel grid horizontally as well;
            // egui's greyscale text looks closest with full hinting.
            hinting: Hinting::Full,
            general_file: None,
            bold_file: None,
            fixed_file: None,
        },
        metrics: Metrics::fluent(),
        icon_theme: String::new(),
        animation_factor: 1.0,
        watched_files: Vec::new(),
    }
}

/// Reads the theme from Windows' settings.  `SLERP_THEME=windows` on
/// another system gives the default light theme.
pub fn detect(_env: &Environment) -> DesktopTheme {
    theme(&read_settings())
}

/// What the theme depends on, cheap enough for the watcher to check twice
/// a second: the settings live in the registry, not in files it can watch.
pub fn settings_stamp() -> u64 {
    use std::hash::{Hash, Hasher};
    let s = read_settings();
    let mut h = std::collections::hash_map::DefaultHasher::new();
    s.dark.hash(&mut h);
    s.accent.map(Rgba::to_u8).hash(&mut h);
    s.text_scale.map(f32::to_bits).hash(&mut h);
    h.finish()
}

#[cfg(windows)]
pub fn read_settings() -> Settings {
    use windows_sys::Win32::Foundation::{ERROR_SUCCESS, FALSE};
    use windows_sys::Win32::Graphics::Dwm::DwmGetColorizationColor;
    use windows_sys::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_BINARY, RRF_RT_REG_DWORD, RegGetValueW};

    /// Reads a value of `flags`' type into `out`, returning its length.
    fn value(key: &str, name: &str, flags: u32, out: &mut [u8]) -> Option<usize> {
        let wide = |s: &str| s.encode_utf16().chain(Some(0)).collect::<Vec<u16>>();
        let (key, name) = (wide(key), wide(name));
        let mut len = out.len() as u32;
        // SAFETY: key and name are NUL-terminated; out is valid for len bytes.
        let rc = unsafe {
            RegGetValueW(
                HKEY_CURRENT_USER,
                key.as_ptr(),
                name.as_ptr(),
                flags,
                std::ptr::null_mut(),
                out.as_mut_ptr().cast(),
                &mut len,
            )
        };
        (rc == ERROR_SUCCESS).then_some(len as usize)
    }
    let dword = |key: &str, name: &str| {
        let mut b = [0u8; 4];
        value(key, name, RRF_RT_REG_DWORD, &mut b).map(|_| u32::from_le_bytes(b))
    };

    // 1. Light or dark apps.
    let personalize = r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize";
    let dark = dword(personalize, "AppsUseLightTheme").map(|v| v == 0);

    // 2. The accent: DWM's colourisation colour, 0xAARRGGBB.
    let (mut color, mut opaque) = (0u32, FALSE);
    // SAFETY: both pointers are valid for writes.
    let accent = (unsafe { DwmGetColorizationColor(&mut color, &mut opaque) } >= 0)
        .then(|| Rgba::rgb8((color >> 16) as u8, (color >> 8) as u8, color as u8));

    // 3. Its shades: eight RGBA entries, the last unused.
    let mut bytes = [0u8; 32];
    let palette = value(
        r"Software\Microsoft\Windows\CurrentVersion\Explorer\Accent",
        "AccentPalette",
        RRF_RT_REG_BINARY,
        &mut bytes,
    )
    .filter(|&len| len >= 28)
    .map(|_| std::array::from_fn(|i| Rgba::rgb8(bytes[i * 4], bytes[i * 4 + 1], bytes[i * 4 + 2])));

    // 4. Text size, in percent.
    let text_scale = dword(r"Software\Microsoft\Accessibility", "TextScaleFactor").map(|p| p as f32 / 100.0);
    Settings { dark, accent, palette, text_scale }
}

#[cfg(not(windows))]
pub fn read_settings() -> Settings {
    Settings::default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_accent_follows_winui() {
        let light = scheme(false, accent_palette(None, None));
        assert!(!light.is_dark());
        // AccentFillColorDefault: the accent's first darker shade in light
        // mode, its second lighter shade with black text in dark mode.
        assert_eq!(light.selection.background, Rgba::rgb8(0x00, 0x67, 0xc0));
        assert_eq!(light.selection.foreground, Rgba::WHITE);
        let dark = scheme(true, accent_palette(None, None));
        assert!(dark.is_dark());
        assert_eq!(dark.selection.background, Rgba::rgb8(0x4c, 0xc2, 0xff));
        assert_eq!(dark.selection.foreground, Rgba::BLACK);
        assert!(dark.window.foreground.contrast_ratio(dark.window.background) > 10.0);
        assert!(light.window.foreground.contrast_ratio(light.window.background) > 10.0);
    }

    #[test]
    fn a_stored_palette_must_match_the_accent() {
        let red = Rgba::rgb8(0xe8, 0x11, 0x23);
        // The default palette belongs to blue, so red gets mixed shades.
        let p = accent_palette(Some(red), Some(DEFAULT_PALETTE));
        assert_eq!(p[3], red);
        assert!(distance(p[1], Rgba::WHITE) < distance(red, Rgba::WHITE));
        assert!(distance(p[4], Rgba::BLACK) < distance(red, Rgba::BLACK));
        assert_eq!(accent_palette(Some(DEFAULT_PALETTE[3]), Some(DEFAULT_PALETTE)), DEFAULT_PALETTE);
    }

    #[test]
    fn segoe_ui_scales_with_the_text_size() {
        let t = theme(&Settings { dark: Some(true), text_scale: Some(1.5), ..Settings::default() });
        assert!(t.dark);
        assert_eq!(t.desktop, Desktop::Windows);
        assert_eq!(t.fonts.general.family, "Segoe UI");
        assert!((t.fonts.general.size_px(t.fonts.dpi) - 21.0).abs() < 0.01);
    }
}
