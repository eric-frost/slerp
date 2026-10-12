//! Detection against fixture configurations: complete `kdeglobals` files,
//! a scheme named but not copied into `kdeglobals`, the system-to-user
//! cascade, icon theme lookups, and garbage input.

use std::path::{Path, PathBuf};

use slerp_theme::{
    ColorScheme, Desktop, Environment, FontSpec, Hinting, IconLookup, KConfig, Rgba, detect_with, style_for,
};

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

/// An environment reading only fixture files, with no portal or
/// fontconfig, so results don't depend on the machine.
fn env(config: &str, config_dirs: &[&str]) -> Environment {
    let f = fixtures();
    Environment {
        config_home: f.join(config),
        config_dirs: config_dirs.iter().map(|d| f.join(d)).collect(),
        data_home: f.join("data"),
        data_dirs: vec![f.join("no-such-dir")],
        current_desktop: vec!["KDE".into()],
        session_type: "wayland".into(),
        forced_desktop: None,
        use_portal: false,
        resolve_fonts: false,
    }
}

#[test]
fn complete_kdeglobals() {
    let theme = detect_with(&env("light/config", &[]));
    assert_eq!(theme.desktop, Desktop::Kde);
    assert!(!theme.dark);
    let c = &theme.colors;
    assert_eq!(c.name.as_deref(), Some("BreezeLight"));
    // Values changed from Breeze's in the fixture, so these prove they were
    // read rather than defaulted.
    assert_eq!(c.window.background, Rgba::rgb8(240, 240, 236));
    assert_eq!(c.selection.background, Rgba::rgb8(200, 80, 40));
    assert_eq!(c.view.focus, Rgba::rgb8(200, 80, 40));
    assert_eq!(c.button.foreground, Rgba::rgb8(35, 38, 41));
    assert_eq!(c.frame_contrast, 0.25);
    assert_eq!(c.disabled_fade, 0.65);
    assert_eq!(c.accent, None);

    let f = &theme.fonts;
    assert_eq!(f.general, FontSpec::new("Inter", 11.0));
    assert_eq!(f.fixed, FontSpec::new("JetBrains Mono", 9.0));
    assert_eq!(f.small.size_pt, 8.0);
    assert_eq!(f.dpi, 96.0);
    assert_eq!(f.hinting, Hinting::Full);
    assert_eq!(theme.icon_theme, "child");
    assert_eq!(theme.animation_factor, 0.5);
}

#[test]
fn scheme_file_and_accent() {
    let theme = detect_with(&env("dark-scheme/config", &[]));
    assert!(theme.dark);
    let c = &theme.colors;
    assert_eq!(c.name.as_deref(), Some("TestDark"));
    // From the scheme file.
    assert_eq!(c.window.background, Rgba::rgb8(30, 30, 40));
    assert_eq!(c.view.background, Rgba::rgb8(18, 18, 26));
    // The accent, applied as Plasma's colour settings would.
    let accent = Rgba::rgb8(233, 84, 32);
    assert_eq!(c.accent, Some(accent));
    assert_eq!(c.view.focus, accent);
    assert_eq!(c.window.hover, accent);
    assert_eq!(c.selection.background, accent.with_alpha(0.7).over(Rgba::rgb8(18, 18, 26)));
    // White reads on that orange-brown.
    assert_eq!(c.selection.foreground, Rgba::WHITE);
    // A light Breeze icon theme name is swapped for the dark variant only
    // when that is installed, which it isn't in the fixtures.
    assert_eq!(theme.icon_theme, "breeze");
}

#[test]
fn system_and_user_cascade() {
    let theme = detect_with(&env("system/config-user", &["system/xdg"]));
    // The scheme name comes from the system file...
    assert_eq!(theme.colors.name.as_deref(), Some("TestDark"));
    assert_eq!(theme.colors.window.background, Rgba::rgb8(30, 30, 40));
    assert_eq!(theme.colors.frame_contrast, 0.3);
    // ...the font from the user's, overriding the system's.
    assert_eq!(theme.fonts.general.family, "User Sans");
    assert_eq!(theme.fonts.general.weight, 700);
    // Without a font setting, small text is the general font at 8 pt.
    assert_eq!(theme.fonts.small.family, "User Sans");
    assert_eq!(theme.fonts.small.size_pt, 8.0);
}

#[test]
fn missing_everything_falls_back_to_breeze() {
    let theme = detect_with(&env("no-such-config", &[]));
    assert_eq!(theme.colors, ColorScheme::breeze_light());
    assert_eq!(theme.fonts.general, FontSpec::new("Noto Sans", 10.0));
    assert_eq!(theme.fonts.fixed, FontSpec::new("Hack", 10.0));
    assert_eq!(theme.icon_theme, "breeze");
}

#[test]
fn garbage_kdeglobals_never_panics() {
    let dir = std::env::temp_dir().join(format!("slerp-theme-garbage-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let samples: [&[u8]; 6] = [
        b"[General]\nColorScheme=../../../etc/passwd\nfont=,,,,\n",
        b"[Colors:Window]\nBackgroundNormal=999,-1\nForegroundNormal=#zz\n[Colors:View][[[\n",
        b"\xff\xfe\x00[General\nfont=\x00\x01,10\n",
        b"[KDE]\nframeContrast=NaN\nAnimationDurationFactor=1e30\n[Icons]\nTheme=../x\n",
        b"",
        &[b'['; 10_000],
    ];
    for sample in samples {
        std::fs::write(dir.join("kdeglobals"), sample).unwrap();
        let mut e = env("x", &[]);
        e.config_home = dir.clone();
        let theme = detect_with(&e);
        assert!(theme.colors.frame_contrast.is_finite());
        assert!((0.0..=10.0).contains(&theme.animation_factor));
        let _ = style_for(&theme);
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn style_mapping() {
    let theme = detect_with(&env("light/config", &[]));
    let style = style_for(&theme);
    // 11 pt at 96 DPI is 14.67 logical pixels.
    let body = &style.text_styles[&egui::TextStyle::Body];
    assert!((body.size - 14.667).abs() < 0.01, "{}", body.size);
    assert!((style.text_styles[&egui::TextStyle::Monospace].size - 12.0).abs() < 0.01);
    let v = &style.visuals;
    assert_eq!(v.panel_fill, egui::Color32::from_rgb(240, 240, 236));
    assert_eq!(v.selection.bg_fill, egui::Color32::from_rgb(200, 80, 40));
    // Separators: window mixed 25 % (the fixture's frame contrast) towards
    // the text colour.
    let sep = Rgba::rgb8(240, 240, 236).mix(Rgba::rgb8(35, 38, 41), 0.25).to_color32();
    assert_eq!(v.widgets.noninteractive.bg_stroke.color, sep);
    assert_eq!(v.widgets.inactive.corner_radius, egui::CornerRadius::same(5));
    assert_eq!(style.spacing.item_spacing, egui::vec2(6.0, 6.0));
    // Rows are as tall as Breeze's controls, so labels centre on them.
    assert_eq!(style.spacing.interact_size.y, 32.0);
    assert_eq!(style.spacing.button_padding, egui::vec2(8.0, 7.0));
    assert_eq!(v.text_options.color_transfer_function, egui::epaint::FontColorTransferFunction::Off);
    assert_eq!(style.spacing.scroll.allocated_width(), 21.0);
    assert!(!style.interaction.selectable_labels);
    assert!((style.animation_time - 0.05).abs() < 1e-6);
}

#[test]
fn icon_lookup_sizes_inheritance_and_fallbacks() {
    let data = fixtures().join("data/icons");
    let lookup = IconLookup::new("child", std::slice::from_ref(&data));
    assert_eq!(lookup.theme_name(), Some("child"));
    let find = |name: &str, size: u32| {
        lookup.find(name, size, 1).map(|(p, follows)| (p.strip_prefix(&data).unwrap().to_owned(), follows))
    };
    // Exact sizes.
    assert_eq!(find("document-open", 22), Some(("child/actions/22/document-open.svg".into(), true)));
    assert_eq!(find("document-open", 16), Some(("child/actions/16/document-open.svg".into(), true)));
    // The closest size when there is no exact one.
    assert_eq!(find("document-open", 20).unwrap().0, PathBuf::from("child/actions/22/document-open.svg"));
    // Inherited from the parent theme: the threshold directory matches 32
    // (within 2), the scalable one anything from 8 to 512.
    assert_eq!(find("zoom-in", 32), Some(("parent/actions/32/zoom-in.svg".into(), false)));
    assert_eq!(find("zoom-in", 22).unwrap().0, PathBuf::from("parent/scalable/actions/zoom-in.svg"));
    // hicolor is searched last even though nothing inherits it.
    assert_eq!(find("slerp", 48).unwrap().0, PathBuf::from("hicolor/48x48/apps/slerp.svg"));
    // Dash fallbacks: "media-playback-start-symbolic" finds the plain icon.
    assert_eq!(
        find("media-playback-start-symbolic", 22).unwrap().0,
        PathBuf::from("child/actions/22/media-playback-start.svg")
    );
    assert_eq!(find("no-such-icon", 22), None);
    assert_eq!(find("../child/index", 22), None);
    // An unknown theme still finds hicolor icons.
    let missing = IconLookup::new("missing", std::slice::from_ref(&data));
    assert!(missing.find("slerp", 48, 1).is_some());
    // A theme inheriting itself doesn't loop.
    let _ = IconLookup::new("parent", &[data]);
}

#[test]
fn kconfig_index_theme() {
    let index = KConfig::load(&fixtures().join("data/icons/child/index.theme")).unwrap();
    assert_eq!(index.get_list("Icon Theme", "Directories"), vec!["actions/16", "actions/22"]);
    assert_eq!(index.get_bool("Icon Theme", "FollowsColorScheme"), Some(true));
    assert_eq!(index.get_u32("actions/22", "Size"), Some(22));
}

/// The icons the viewer uses resolve in the installed Breeze themes.  Skipped
/// on machines without Breeze.
#[test]
fn viewer_icons_resolve_in_breeze() {
    let paths = vec![PathBuf::from("/usr/share/icons")];
    for theme in ["breeze", "breeze-dark"] {
        if !Path::new("/usr/share/icons").join(theme).join("index.theme").is_file() {
            eprintln!("{theme} not installed; skipping");
            continue;
        }
        let lookup = IconLookup::new(theme, &paths);
        let missing: Vec<&str> =
            slerp_theme::VIEWER_ICONS.iter().copied().filter(|name| lookup.find(name, 22, 1).is_none()).collect();
        assert!(missing.is_empty(), "{theme} lacks {missing:?}");
        // Tool bar icons come from the 22 px folder, drawn for that size.
        let (path, follows) = lookup.find("document-open", 22, 1).unwrap();
        assert!(path.ends_with("actions/22/document-open.svg"), "{path:?}");
        assert!(follows);
    }
}
