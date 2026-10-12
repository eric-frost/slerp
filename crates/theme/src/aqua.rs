//! macOS: AppKit's system colours, expressed as KDE colour sets so the same
//! egui mapping serves every desktop, with the light or dark appearance and
//! the accent colour from the user's global preferences, and San Francisco,
//! the system font, at AppKit's 13 pt.
//!
//! macOS stores the appearance as `AppleInterfaceStyle` (`Dark`, or absent
//! for light; with automatic switching the system keeps it current) and the
//! accent as `AppleAccentColor`, a number (absent for the default blue).

use crate::color::Rgba;
use crate::fonts::{FontSpec, Fonts, Hinting};
use crate::scheme::{ColorScheme, ColorSet};
use crate::{Desktop, DesktopTheme, Environment, Metrics};

/// What the user's preferences say; `None` where one is missing.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Settings {
    /// `AppleInterfaceStyle` is `Dark`.
    pub dark: bool,
    /// `AppleAccentColor`: -1 graphite, 0 red, 1 orange, 2 yellow, 3 green,
    /// 4 blue, 5 purple, 6 pink; `None` for the default (blue).
    pub accent: Option<i64>,
}

/// AppKit's `controlAccentColor` for an `AppleAccentColor` value, in the
/// light or dark appearance.
pub fn accent_color(accent: Option<i64>, dark: bool) -> Rgba {
    let (light, dark_variant) = match accent {
        Some(-1) => (Rgba::rgb8(0x98, 0x98, 0x98), Rgba::rgb8(0x8c, 0x8c, 0x8c)),
        Some(0) => (Rgba::rgb8(0xe0, 0x38, 0x3e), Rgba::rgb8(0xff, 0x52, 0x57)),
        Some(1) => (Rgba::rgb8(0xf7, 0x82, 0x1b), Rgba::rgb8(0xf7, 0x82, 0x1b)),
        Some(2) => (Rgba::rgb8(0xff, 0xc6, 0x00), Rgba::rgb8(0xff, 0xc6, 0x00)),
        Some(3) => (Rgba::rgb8(0x62, 0xba, 0x46), Rgba::rgb8(0x62, 0xba, 0x46)),
        Some(5) => (Rgba::rgb8(0x95, 0x3d, 0x96), Rgba::rgb8(0xa5, 0x50, 0xa7)),
        Some(6) => (Rgba::rgb8(0xf7, 0x4f, 0x9e), Rgba::rgb8(0xf7, 0x4f, 0x9e)),
        _ => (Rgba::rgb8(0x00, 0x7a, 0xff), Rgba::rgb8(0x0a, 0x84, 0xff)),
    };
    if dark { dark_variant } else { light }
}

/// The macOS colour scheme, light or dark, with an accent colour.
pub fn scheme(dark: bool, accent: Rgba) -> ColorScheme {
    // 1. windowBackgroundColor, controlBackgroundColor (lists and fields),
    //    and labelColor and secondaryLabelColor, translucent black or white
    //    flattened onto each background.
    let (window_bg, view_bg, header_bg, tooltip_bg) = if dark {
        (
            Rgba::rgb8(0x32, 0x32, 0x32),
            Rgba::rgb8(0x1e, 0x1e, 0x1e),
            Rgba::rgb8(0x2c, 0x2c, 0x2c),
            Rgba::rgb8(0x2a, 0x2a, 0x2a),
        )
    } else {
        (Rgba::rgb8(0xec, 0xec, 0xec), Rgba::WHITE, Rgba::rgb8(0xe6, 0xe6, 0xe6), Rgba::rgb8(0xf6, 0xf6, 0xf6))
    };
    let ink = if dark { Rgba::WHITE } else { Rgba::BLACK };
    let (negative, neutral, positive, link) = if dark {
        (
            Rgba::rgb8(0xff, 0x45, 0x3a),
            Rgba::rgb8(0xff, 0x9f, 0x0a),
            Rgba::rgb8(0x32, 0xd7, 0x4b),
            Rgba::rgb8(0x41, 0x9c, 0xff),
        )
    } else {
        (
            Rgba::rgb8(0xd7, 0x00, 0x15),
            Rgba::rgb8(0xc9, 0x34, 0x00),
            Rgba::rgb8(0x24, 0x8a, 0x3d),
            Rgba::rgb8(0x00, 0x68, 0xda),
        )
    };
    let set = |bg: Rgba| ColorSet {
        background: bg,
        // alternatingContentBackgroundColors.
        alternate_background: ink.with_alpha(if dark { 0.05 } else { 0.03 }).over(bg),
        foreground: ink.with_alpha(0.85).over(bg),
        inactive: ink.with_alpha(if dark { 0.55 } else { 0.5 }).over(bg),
        active: accent,
        link,
        visited: link,
        negative,
        neutral,
        positive,
        focus: accent,
        hover: accent,
    };

    // 2. Push buttons are white in light mode and white at 25 % in dark
    //    mode; selected rows (selectedContentBackgroundColor) are the accent
    //    a little darker, with white text.
    let button_bg = if dark { Rgba::WHITE.with_alpha(0.25).over(window_bg) } else { Rgba::WHITE };
    let mut button = set(button_bg);
    button.alternate_background = accent;
    let selected = accent.mix(Rgba::BLACK, if dark { 0.15 } else { 0.08 });
    let selection = ColorSet {
        background: selected,
        alternate_background: ink.with_alpha(if dark { 0.15 } else { 0.1 }).over(view_bg),
        foreground: Rgba::WHITE,
        inactive: Rgba::WHITE.with_alpha(0.7).over(selected),
        active: Rgba::WHITE,
        ..set(selected)
    };
    let complementary_bg = if dark { Rgba::rgb8(0xec, 0xec, 0xec) } else { Rgba::rgb8(0x32, 0x32, 0x32) };
    let complementary = ColorSet {
        foreground: if dark {
            Rgba::BLACK.with_alpha(0.85).over(complementary_bg)
        } else {
            Rgba::WHITE.with_alpha(0.85).over(complementary_bg)
        },
        ..set(complementary_bg)
    };
    ColorScheme {
        name: Some(if dark { "macOS Dark".into() } else { "macOS Light".into() }),
        window: set(window_bg),
        view: set(view_bg),
        button,
        selection,
        tooltip: set(tooltip_bg),
        header: set(header_bg),
        complementary,
        accent: Some(accent),
        // separatorColor is black at 10 % (white at 10 % in dark mode).
        frame_contrast: if dark { 0.14 } else { 0.12 },
        disabled_fade: 0.6,
    }
}

/// The theme from `settings`.  AppKit's points are logical pixels, so the
/// fonts are sized at 72 DPI: the system font at 13 pt is 13 px.
pub fn theme(settings: &Settings) -> DesktopTheme {
    let dark = settings.dark;
    let general = FontSpec::new(".AppleSystemUIFont", 13.0);
    let fixed = FontSpec::new("SF Mono", 12.0);
    let small = FontSpec { size_pt: 11.0, ..general.clone() };
    DesktopTheme {
        desktop: Desktop::MacOs,
        dark,
        colors: scheme(dark, accent_color(settings.accent, dark)),
        fonts: Fonts {
            general,
            fixed,
            small,
            dpi: 72.0,
            // macOS draws text unhinted.
            hinting: Hinting::None,
            general_file: None,
            bold_file: None,
            fixed_file: None,
        },
        metrics: Metrics::aqua(),
        icon_theme: String::new(),
        animation_factor: 1.0,
        watched_files: Vec::new(),
    }
}

/// Reads the theme from the user's preferences.  `SLERP_THEME=macos` on
/// another system gives the default light theme.
pub fn detect(_env: &Environment) -> DesktopTheme {
    theme(&read_settings())
}

/// What the theme depends on, cheap enough for the watcher to check twice
/// a second.
pub fn settings_stamp() -> u64 {
    let s = read_settings();
    (u64::from(s.dark) << 32) | (s.accent.map_or(0xffff, |a| (a + 16) as u64 & 0xffff))
}

#[cfg(target_os = "macos")]
pub fn read_settings() -> Settings {
    let style = cf::global_string("AppleInterfaceStyle");
    Settings {
        dark: style.is_some_and(|s| s.eq_ignore_ascii_case("dark")),
        accent: cf::global_integer("AppleAccentColor"),
    }
}

#[cfg(not(target_os = "macos"))]
pub fn read_settings() -> Settings {
    Settings::default()
}

/// The few CoreFoundation calls that read the global preferences domain.
/// They go through `cfprefsd`, so they see changes before they reach the
/// preferences file on disk.
#[cfg(target_os = "macos")]
mod cf {
    use std::ffi::{c_char, c_void};

    type CFTypeRef = *const c_void;
    type CFStringRef = *const c_void;
    const UTF8: u32 = 0x0800_0100;
    const SINT64: isize = 4;

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        static kCFPreferencesAnyApplication: CFStringRef;
        fn CFStringCreateWithBytes(
            alloc: CFTypeRef,
            bytes: *const u8,
            len: isize,
            encoding: u32,
            external: u8,
        ) -> CFStringRef;
        fn CFPreferencesAppSynchronize(app: CFStringRef) -> u8;
        fn CFPreferencesCopyAppValue(key: CFStringRef, app: CFStringRef) -> CFTypeRef;
        fn CFGetTypeID(cf: CFTypeRef) -> usize;
        fn CFStringGetTypeID() -> usize;
        fn CFNumberGetTypeID() -> usize;
        fn CFStringGetCString(s: CFStringRef, buf: *mut c_char, size: isize, encoding: u32) -> u8;
        fn CFNumberGetValue(n: CFTypeRef, kind: isize, out: *mut c_void) -> u8;
        fn CFRelease(cf: CFTypeRef);
    }

    /// A value from the global domain, released when dropped.
    struct Value(CFTypeRef);

    impl Drop for Value {
        fn drop(&mut self) {
            // SAFETY: the value came from a Copy function and is released once.
            unsafe { CFRelease(self.0) }
        }
    }

    fn global(key: &str) -> Option<Value> {
        // SAFETY: the bytes are valid UTF-8 of the given length; the key is
        // released after the lookup, and the result is owned (a Copy call).
        unsafe {
            let name = CFStringCreateWithBytes(std::ptr::null(), key.as_ptr(), key.len() as isize, UTF8, 0);
            if name.is_null() {
                return None;
            }
            let _name = Value(name);
            // Drop this process's cached copy, so the watcher sees changes.
            CFPreferencesAppSynchronize(kCFPreferencesAnyApplication);
            let value = CFPreferencesCopyAppValue(name, kCFPreferencesAnyApplication);
            (!value.is_null()).then(|| Value(value))
        }
    }

    pub fn global_string(key: &str) -> Option<String> {
        let value = global(key)?;
        let mut buf = [0 as c_char; 256];
        // SAFETY: the type is checked before the string call; buf is valid
        // for its length, and CFStringGetCString NUL-terminates on success.
        unsafe {
            if CFGetTypeID(value.0) != CFStringGetTypeID()
                || CFStringGetCString(value.0, buf.as_mut_ptr(), buf.len() as isize, UTF8) == 0
            {
                return None;
            }
            Some(std::ffi::CStr::from_ptr(buf.as_ptr()).to_string_lossy().into_owned())
        }
    }

    pub fn global_integer(key: &str) -> Option<i64> {
        let value = global(key)?;
        let mut out = 0i64;
        // SAFETY: the type is checked first; out is valid for an SInt64.
        unsafe {
            (CFGetTypeID(value.0) == CFNumberGetTypeID()
                && CFNumberGetValue(value.0, SINT64, (&raw mut out).cast()) != 0)
                .then_some(out)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The preferences as CoreFoundation reads them match what `defaults`
    /// prints for the same keys.
    #[test]
    #[cfg(target_os = "macos")]
    fn reads_the_global_preferences() {
        let read = |key: &str| {
            let out = std::process::Command::new("defaults").args(["read", "-g", key]).output().unwrap();
            out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().to_owned())
        };
        let s = read_settings();
        assert_eq!(s.dark, read("AppleInterfaceStyle").is_some_and(|v| v.eq_ignore_ascii_case("dark")));
        assert_eq!(s.accent, read("AppleAccentColor").and_then(|v| v.parse().ok()));
        let t = crate::detect();
        assert_eq!(t.desktop, Desktop::MacOs);
        assert_eq!(t.dark, s.dark);
        // San Francisco is found, with the Text optical size.
        let file = t.fonts.general_file.clone().expect("SF found");
        assert!(file.path.ends_with("SFNS.ttf"), "{file:?}");
        assert!(t.fonts.bold_file.is_some() && t.fonts.fixed_file.is_some(), "{:?}", t.fonts);
        eprintln!("macOS theme: dark {}, accent {:?}, {file:?}", s.dark, s.accent);
    }

    #[test]
    fn default_and_chosen_accents() {
        assert_eq!(accent_color(None, false), Rgba::rgb8(0x00, 0x7a, 0xff));
        assert_eq!(accent_color(Some(4), true), Rgba::rgb8(0x0a, 0x84, 0xff));
        assert_eq!(accent_color(Some(-1), false), Rgba::rgb8(0x98, 0x98, 0x98));
        // Values from future releases fall back to blue.
        assert_eq!(accent_color(Some(42), false), accent_color(None, false));
    }

    #[test]
    fn light_and_dark() {
        let light = theme(&Settings::default());
        assert!(!light.dark && !light.colors.is_dark());
        assert_eq!(light.colors.view.background, Rgba::WHITE);
        let dark = theme(&Settings { dark: true, accent: Some(0) });
        assert!(dark.dark && dark.colors.is_dark());
        assert!(dark.colors.window.foreground.contrast_ratio(dark.colors.window.background) > 7.0);
        // AppKit points are pixels: 13 pt San Francisco is 13 px.
        assert!((dark.fonts.general.size_px(dark.fonts.dpi) - 13.0).abs() < 0.01);
    }
}
