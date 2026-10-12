//! egui styling that follows the desktop: colours, fonts, metrics and icons
//! from KDE Plasma (Breeze, Breeze Dark or any colour scheme), the light or
//! dark preference and accent colour on GNOME and other desktops, and on
//! Windows 11 and macOS their own palettes, system fonts, light or dark
//! mode and accent colour.
//!
//! ```no_run
//! # fn start(ctx: &egui::Context) -> slerp_theme::ThemeWatcher {
//! // Once at start-up:
//! let theme = slerp_theme::detect();
//! slerp_theme::apply(ctx, &theme);
//! let watcher = slerp_theme::ThemeWatcher::new(ctx, theme);
//! # watcher }
//! # fn frame(ctx: &egui::Context, watcher: &mut slerp_theme::ThemeWatcher) {
//! // Every frame, cheap; re-applies when the user changes their theme:
//! watcher.poll(ctx);
//! // Icons from the user's icon theme:
//! let open = slerp_theme::icon("document-open", 22);
//! # }
//! ```
//!
//! KDE apps draw with Qt's Breeze style, which reads `kdeglobals` and mixes
//! its colours in fixed proportions (`breeze/kstyle/breezehelper.cpp`).
//! This crate reads the same files and does the same arithmetic, then maps
//! the result onto egui's style knobs.  egui can't draw everything Breeze
//! does through those knobs, so [`widgets`] adds Breeze-style check boxes,
//! radio buttons, sliders, combo boxes, menus, text fields and table
//! headers the viewer can use in place of egui's own; the notes at the top
//! of `style.rs` list what still can't match.
//!
//! # Window integration
//!
//! Some of looking native is up to the window, not egui:
//!
//! - Set the Wayland app id (and X11 class) to `slerp`, the name of the
//!   installed `slerp.desktop`, with
//!   `ViewportBuilder::with_app_id("slerp")`.  KWin and GNOME Shell find
//!   the window's icon and name through it, and the `.desktop` file should
//!   say `StartupWMClass=slerp` for X11.  Tests behind `hide-windows`
//!   use `agent-test` instead.
//! - Keep the window manager's decorations (eframe's default).  On KWin,
//!   winit asks for server-side decorations, so the title bar is the user's
//!   Breeze one.  Compositors without server-side decorations (GNOME) get
//!   winit's own client-side title bar, which does not follow this theme.
//! - egui's own floating `egui::Window`s are drawn inside the app, so they
//!   never get the desktop's title bars; prefer side panels and menus.
//! - Rows in egui centre their items within [`Metrics::control_height`], so
//!   a label next to a combo box or slider lines up as in a Qt form.  Use
//!   [`widgets::compact`] for dense areas such as status bars.

mod adwaita;
mod aqua;
mod bundled;
mod color;
mod fluent;
mod fonts;
mod icons;
mod kconfig;
mod portal;
mod scheme;
mod style;
mod watch;
pub mod widgets;

use std::path::{Path, PathBuf};

pub use color::Rgba;
pub use fonts::{
    BOLD, FontFile, FontSpec, Fonts, Hinting, fallback_files, fallbacks_loaded, load_fallbacks, system_font,
};
pub use icons::{IconColors, IconLookup, recolor_svg};
pub use kconfig::KConfig;
pub use scheme::{ColorScheme, ColorSet};
pub use style::{Palette, apply, palette, style_for};
pub use watch::ThemeWatcher;

/// Which desktop the theme was read from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Desktop {
    /// KDE Plasma: colours, fonts and icons come from `kdeglobals`.
    Kde,
    /// GNOME: libadwaita's palette with the portal's dark preference and
    /// accent colour.
    Gnome,
    /// Anything else: as GNOME, from the portal and GTK settings files.
    Other,
    /// Windows 11: the Fluent palette with the app mode and accent colour
    /// from the registry and DWM, and Segoe UI.
    Windows,
    /// macOS: AppKit's colours with the appearance and accent colour from
    /// the global preferences, and San Francisco.
    MacOs,
}

/// Widget sizes and shapes, in egui points (logical pixels).  The Breeze
/// values come from `breeze/kstyle/breezemetrics.h`.
#[derive(Clone, Debug, PartialEq)]
pub struct Metrics {
    /// Corner radius of buttons, fields, check boxes and frames.
    pub frame_radius: f32,
    /// Corner radius of menus, popups and tooltips.
    pub popup_radius: f32,
    /// Space between a push button's frame and its text, per side.
    pub button_padding: [f32; 2],
    /// Space between a text field's frame and its text, per side.
    pub field_padding: [f32; 2],
    /// Height of buttons, combo boxes, fields and spin boxes, which is also
    /// the height of a row of controls.  egui centres a row's items within
    /// this height, so labels line up with the controls beside them, as in
    /// a Qt form layout.
    pub control_height: f32,
    /// Height of a check box or radio button row, and of compact rows such
    /// as status bars.
    pub row_height: f32,
    /// Default spacing between widgets in a layout.
    pub spacing: f32,
    /// Margin inside top-level windows and dialogs.
    pub window_margin: f32,
    /// Margin inside panels and group boxes.
    pub panel_margin: f32,
    /// Side of a check box's square and a radio button's circle.
    pub check_size: f32,
    /// Width of the scroll bar handle.
    pub scroll_handle_width: f32,
    /// Width reserved for a scroll bar, handle plus margins.
    pub scroll_bar_extent: f32,
    pub scroll_handle_min_length: f32,
    /// Thickness of a slider's groove.
    pub slider_groove: f32,
    /// Diameter of a slider's handle.
    pub slider_handle: f32,
    /// Margin inside a popup menu, around its items.
    pub menu_margin: f32,
    /// Padding of a popup menu item around its text.
    pub menu_item_padding: [f32; 2],
    /// Padding of a menu bar item around its text.
    pub menu_bar_item_padding: [f32; 2],
    /// Indentation of nested items.
    pub indent: f32,
    /// Seconds the pointer must rest before a tooltip shows.
    pub tooltip_delay: f32,
    /// Seconds after one tooltip during which the next shows at once.
    pub tooltip_grace: f32,
    /// Whether push buttons have an outline (Breeze) or are plain fills
    /// (Adwaita).
    pub button_outline: bool,
    /// Icon size for tool bars.
    pub toolbar_icon: u32,
    /// Icon size for menus, buttons and lists.
    pub small_icon: u32,
}

impl Metrics {
    /// Breeze's metrics.  Heights in Qt come from the font's height plus
    /// margins, and egui works the same way, so these are paddings rather
    /// than fixed heights.  They are tuned so egui's widgets come out the
    /// size Breeze's measure on screen with Noto Sans 10: push buttons and
    /// text fields 32 px tall, check box rows 22 px, menu bar items 30 px.
    pub fn breeze() -> Metrics {
        Metrics {
            frame_radius: 5.0,
            popup_radius: 5.0,
            button_padding: [8.0, 7.0],
            field_padding: [8.0, 7.0],
            control_height: 32.0,
            row_height: 22.0,
            spacing: 6.0,
            window_margin: 10.0,
            panel_margin: 6.0,
            check_size: 16.0,
            scroll_handle_width: 8.0,
            scroll_bar_extent: 21.0,
            scroll_handle_min_length: 20.0,
            slider_groove: 6.0,
            slider_handle: 20.0,
            menu_margin: 4.0,
            menu_item_padding: [12.0, 5.5],
            menu_bar_item_padding: [10.0, 6.0],
            indent: 20.0,
            tooltip_delay: 0.7,
            tooltip_grace: 2.0,
            button_outline: true,
            toolbar_icon: 22,
            small_icon: 16,
        }
    }

    /// libadwaita's metrics: 34 px buttons and fields with 6 px corners,
    /// 12 px popover corners, overlay scroll bars.
    pub fn adwaita() -> Metrics {
        Metrics {
            frame_radius: 6.0,
            popup_radius: 12.0,
            button_padding: [10.0, 8.0],
            field_padding: [9.0, 8.0],
            control_height: 34.0,
            row_height: 24.0,
            spacing: 6.0,
            window_margin: 12.0,
            panel_margin: 6.0,
            check_size: 18.0,
            scroll_handle_width: 8.0,
            scroll_bar_extent: 14.0,
            scroll_handle_min_length: 40.0,
            slider_groove: 4.0,
            slider_handle: 20.0,
            menu_margin: 6.0,
            menu_item_padding: [12.0, 7.0],
            menu_bar_item_padding: [10.0, 6.0],
            indent: 20.0,
            tooltip_delay: 0.5,
            tooltip_grace: 1.0,
            button_outline: false,
            toolbar_icon: 16,
            small_icon: 16,
        }
    }

    /// Windows 11's metrics (WinUI 3): 32 px buttons and fields with 4 px
    /// corners, 8 px corners on menus and flyouts, 20 px check boxes and an
    /// 8 px spacing grid.
    pub fn fluent() -> Metrics {
        Metrics {
            frame_radius: 4.0,
            popup_radius: 8.0,
            button_padding: [11.0, 6.0],
            field_padding: [10.0, 6.0],
            control_height: 32.0,
            row_height: 24.0,
            spacing: 8.0,
            window_margin: 12.0,
            panel_margin: 8.0,
            check_size: 20.0,
            scroll_handle_width: 6.0,
            scroll_bar_extent: 16.0,
            scroll_handle_min_length: 30.0,
            slider_groove: 4.0,
            slider_handle: 20.0,
            menu_margin: 4.0,
            menu_item_padding: [11.0, 7.0],
            menu_bar_item_padding: [10.0, 6.0],
            indent: 20.0,
            tooltip_delay: 0.5,
            tooltip_grace: 1.0,
            button_outline: true,
            toolbar_icon: 16,
            small_icon: 16,
        }
    }

    /// macOS's metrics (AppKit since Big Sur): regular-size controls 22 px
    /// tall with 5 px corners, 6 px menu corners, 14 px check boxes.
    pub fn aqua() -> Metrics {
        Metrics {
            frame_radius: 5.0,
            popup_radius: 6.0,
            button_padding: [10.0, 3.0],
            field_padding: [6.0, 3.0],
            control_height: 22.0,
            row_height: 20.0,
            spacing: 8.0,
            window_margin: 20.0,
            panel_margin: 8.0,
            check_size: 14.0,
            scroll_handle_width: 7.0,
            scroll_bar_extent: 15.0,
            scroll_handle_min_length: 20.0,
            slider_groove: 4.0,
            slider_handle: 18.0,
            menu_margin: 5.0,
            menu_item_padding: [10.0, 3.0],
            menu_bar_item_padding: [8.0, 3.0],
            indent: 16.0,
            tooltip_delay: 1.0,
            tooltip_grace: 1.0,
            button_outline: true,
            toolbar_icon: 16,
            small_icon: 16,
        }
    }
}

/// Everything the theme needs to style egui: what `detect` read from the
/// desktop.  Fields are public so an application can adjust them before
/// calling [`apply`].
#[derive(Clone, Debug, PartialEq)]
pub struct DesktopTheme {
    pub desktop: Desktop,
    pub dark: bool,
    /// Colours as KDE colour sets.  On GNOME they are synthesised from
    /// libadwaita's palette.
    pub colors: ColorScheme,
    pub fonts: Fonts,
    pub metrics: Metrics,
    /// The icon theme name, such as `breeze-dark` or `Adwaita`.
    pub icon_theme: String,
    /// Multiplies animation durations; 0 turns animations off (KDE's
    /// `AnimationDurationFactor`).
    pub animation_factor: f32,
    /// Files whose changes mean the theme changed, for [`ThemeWatcher`].
    pub watched_files: Vec<PathBuf>,
}

impl DesktopTheme {
    /// The colours `icon()` writes into recolourable icons.
    pub fn icon_colors(&self) -> IconColors {
        let c = &self.colors;
        IconColors {
            text: c.window.foreground,
            background: c.window.background,
            highlight: c.selection.background,
            highlighted_text: c.selection.foreground,
            positive: c.window.positive,
            neutral: c.window.neutral,
            negative: c.window.negative,
            accent: c.accent.unwrap_or(c.selection.background),
        }
    }
}

/// Where to look for settings.  [`Environment::from_env`] reads the real
/// environment; tests point it at fixture directories.
#[derive(Clone, Debug)]
pub struct Environment {
    /// `$XDG_CONFIG_HOME`, normally `~/.config`.
    pub config_home: PathBuf,
    /// `$XDG_CONFIG_DIRS`, most important first.
    pub config_dirs: Vec<PathBuf>,
    /// `$XDG_DATA_HOME`, normally `~/.local/share`.
    pub data_home: PathBuf,
    /// `$XDG_DATA_DIRS`, most important first.
    pub data_dirs: Vec<PathBuf>,
    /// `$XDG_CURRENT_DESKTOP`, split on colons.
    pub current_desktop: Vec<String>,
    /// `$XDG_SESSION_TYPE`: `wayland` or `x11`.
    pub session_type: String,
    /// `$SLERP_THEME`, which forces `kde`, `gnome`, `other`, `windows`
    /// or `macos`.
    pub forced_desktop: Option<String>,
    /// Ask the XDG desktop portal (off in tests, which must not depend on
    /// the session bus).
    pub use_portal: bool,
    /// Resolve font files with fontconfig.
    pub resolve_fonts: bool,
}

impl Environment {
    pub fn from_env() -> Environment {
        let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/"));
        let path_var = |name: &str| std::env::var_os(name).filter(|v| !v.is_empty()).map(PathBuf::from);
        let list_var = |name: &str, default: &str| -> Vec<PathBuf> {
            let value = std::env::var(name).ok().filter(|v| !v.is_empty());
            value.as_deref().unwrap_or(default).split(':').filter(|p| !p.is_empty()).map(PathBuf::from).collect()
        };
        Environment {
            config_home: path_var("XDG_CONFIG_HOME").unwrap_or_else(|| home.join(".config")),
            config_dirs: list_var("XDG_CONFIG_DIRS", "/etc/xdg"),
            data_home: path_var("XDG_DATA_HOME").unwrap_or_else(|| home.join(".local/share")),
            data_dirs: list_var("XDG_DATA_DIRS", "/usr/local/share:/usr/share"),
            current_desktop: std::env::var("XDG_CURRENT_DESKTOP")
                .unwrap_or_default()
                .split(':')
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
                .collect(),
            session_type: std::env::var("XDG_SESSION_TYPE").unwrap_or_default(),
            forced_desktop: std::env::var("SLERP_THEME").ok().filter(|v| !v.is_empty()),
            use_portal: true,
            resolve_fonts: true,
        }
    }

    /// Which desktop's settings to follow.
    pub fn desktop(&self) -> Desktop {
        if let Some(forced) = &self.forced_desktop {
            match forced.to_ascii_lowercase().as_str() {
                "kde" | "plasma" | "breeze" => return Desktop::Kde,
                "gnome" | "adwaita" => return Desktop::Gnome,
                "other" | "gtk" => return Desktop::Other,
                "windows" | "fluent" => return Desktop::Windows,
                "macos" | "mac" | "aqua" => return Desktop::MacOs,
                _ => {}
            }
        }
        let has = |name: &str| self.current_desktop.iter().any(|d| d.eq_ignore_ascii_case(name));
        if has("KDE") {
            Desktop::Kde
        } else if has("GNOME") {
            Desktop::Gnome
        } else if cfg!(windows) {
            Desktop::Windows
        } else if cfg!(target_os = "macos") {
            Desktop::MacOs
        } else {
            Desktop::Other
        }
    }

    /// The `kdeglobals` cascade, least important first.
    fn kdeglobals_paths(&self) -> Vec<PathBuf> {
        let mut paths: Vec<PathBuf> = self.config_dirs.iter().rev().map(|d| d.join("kdeglobals")).collect();
        paths.push(self.config_home.join("kdeglobals"));
        paths
    }

    /// Finds `<subdir>/<name>` in the data directories, user first.
    fn find_data(&self, subdir: &str, name: &str) -> Option<PathBuf> {
        std::iter::once(&self.data_home).chain(&self.data_dirs).map(|d| d.join(subdir).join(name)).find(|p| p.is_file())
    }
}

/// Reads the desktop's theme from the real environment.  Takes 10 to
/// 20 ms, mostly fontconfig; never fails, falling back to Breeze.
pub fn detect() -> DesktopTheme {
    detect_with(&Environment::from_env())
}

/// Reads the theme from the given environment.
pub fn detect_with(env: &Environment) -> DesktopTheme {
    let mut theme = match env.desktop() {
        Desktop::Kde => detect_kde(env),
        Desktop::Windows => fluent::detect(env),
        Desktop::MacOs => aqua::detect(env),
        desktop => detect_gtk(env, desktop),
    };
    if env.resolve_fonts {
        theme.fonts.resolve_files();
    }
    theme
}

/// The icon theme to use, swapping Breeze for its matching light or dark
/// variant when the colour scheme calls for it, since Breeze Dark's
/// folder holds the light-on-dark versions of colourful icons.
fn matching_breeze_variant(name: &str, dark: bool, env: &Environment) -> String {
    let swapped = match (name, dark) {
        ("breeze", true) => "breeze-dark",
        ("breeze-dark", false) => "breeze",
        _ => return name.to_owned(),
    };
    let installed = std::iter::once(&env.data_home)
        .chain(&env.data_dirs)
        .any(|d| d.join("icons").join(swapped).join("index.theme").is_file());
    if installed { swapped.to_owned() } else { name.to_owned() }
}

fn detect_kde(env: &Environment) -> DesktopTheme {
    // 1. kdeglobals, cascaded over the system defaults, and the colour
    //    scheme file it names.
    let paths = env.kdeglobals_paths();
    let globals = KConfig::cascade(&paths);
    let scheme_path = globals
        .get("General", "ColorScheme")
        .filter(|n| !n.is_empty() && !n.contains('/'))
        .and_then(|name| env.find_data("color-schemes", &format!("{name}.colors")));
    let scheme_file = scheme_path.as_deref().and_then(KConfig::load);
    let colors = ColorScheme::load(&globals, scheme_file.as_ref());
    let dark = colors.is_dark();

    // 2. Fonts.  KDE's defaults are Noto Sans 10 and Hack 10, with 8 pt the
    //    smallest readable size.  On X11 the user can force a font DPI.
    let font = |key: &str, default: &str| {
        globals.get("General", key).and_then(FontSpec::parse_qt).unwrap_or_else(|| FontSpec::new(default, 10.0))
    };
    let general = font("font", "Noto Sans");
    let fixed = font("fixed", "Hack");
    let small = globals
        .get("General", "smallestReadableFont")
        .and_then(FontSpec::parse_qt)
        .unwrap_or(FontSpec { size_pt: 8.0, ..general.clone() });
    let kcmfonts_path = env.config_home.join("kcmfonts");
    let forced_dpi = KConfig::load(&kcmfonts_path)
        .and_then(|k| k.get_f32("General", "forceFontDPI"))
        .filter(|dpi| *dpi >= 48.0 && *dpi <= 480.0);
    let dpi = match forced_dpi {
        Some(dpi) if env.session_type.eq_ignore_ascii_case("x11") => dpi,
        _ => 96.0,
    };
    let hinting = match globals.get("General", "XftHintStyle").map(str::trim) {
        Some("hintnone") => Hinting::None,
        Some("hintmedium") | Some("hintfull") => Hinting::Full,
        _ => Hinting::Slight,
    };

    // 3. Icons, animation speed.
    let default_icons = if dark { "breeze-dark" } else { "breeze" };
    let icon_theme = globals.get("Icons", "Theme").filter(|t| !t.is_empty()).unwrap_or(default_icons);
    let icon_theme = matching_breeze_variant(icon_theme, dark, env);
    let animation_factor = globals.get_f32("KDE", "AnimationDurationFactor").map_or(1.0, |f| f.clamp(0.0, 10.0));

    let mut watched_files = paths;
    watched_files.extend(scheme_path);
    watched_files.push(kcmfonts_path);

    DesktopTheme {
        desktop: Desktop::Kde,
        dark,
        colors,
        fonts: Fonts { general, fixed, small, dpi, hinting, general_file: None, bold_file: None, fixed_file: None },
        metrics: Metrics::breeze(),
        icon_theme,
        animation_factor,
        watched_files,
    }
}

fn detect_gtk(env: &Environment, desktop: Desktop) -> DesktopTheme {
    // 1. The portal answers on GNOME and on most other desktops; GTK's
    //    settings files fill in what it doesn't say.
    let from_portal = if env.use_portal { portal::read_portal() } else { None };
    let from_files = portal::read_gtk_settings(&env.config_home);
    let pick = |a: Option<String>, b: Option<String>| a.filter(|s| !s.is_empty()).or(b);
    let portal = from_portal.unwrap_or_default();
    let dark = portal.dark.or(from_files.dark).unwrap_or(false);
    let accent = portal.accent;

    // 2. Fonts, sized by GNOME's text scaling factor.
    let scale = portal.text_scale.unwrap_or(1.0);
    let default_family = if desktop == Desktop::Gnome { "Adwaita Sans 11" } else { "Sans 10" };
    let mut general = pick(portal.font.clone(), from_files.font.clone())
        .and_then(|f| FontSpec::parse_pango(&f))
        .unwrap_or_else(|| FontSpec::parse_pango(default_family).unwrap_or(FontSpec::new("Sans", 10.0)));
    let mut fixed = portal
        .monospace_font
        .as_deref()
        .and_then(FontSpec::parse_pango)
        .unwrap_or_else(|| FontSpec::new("Monospace", general.size_pt));
    general.size_pt *= scale;
    fixed.size_pt *= scale;
    let small = FontSpec { size_pt: (general.size_pt * 0.8).max(7.0), ..general.clone() };

    let icon_theme = pick(portal.icon_theme.clone(), from_files.icon_theme.clone())
        .unwrap_or_else(|| if desktop == Desktop::Gnome { "Adwaita".into() } else { "hicolor".into() });

    let mut watched_files =
        vec![env.config_home.join("gtk-4.0/settings.ini"), env.config_home.join("gtk-3.0/settings.ini")];
    watched_files.dedup();

    DesktopTheme {
        desktop,
        dark,
        colors: adwaita::scheme(dark, accent, portal.high_contrast),
        fonts: Fonts {
            general,
            fixed,
            small,
            dpi: 96.0,
            hinting: Hinting::Slight,
            general_file: None,
            bold_file: None,
            fixed_file: None,
        },
        metrics: Metrics::adwaita(),
        icon_theme,
        animation_factor: 1.0,
        watched_files,
    }
}

/// An icon from the desktop's icon theme, `size` logical pixels square,
/// for `egui::Image::new` or `Button::image`.  Breeze icons come in the
/// current colour scheme's colours.  When no theme in the inheritance chain
/// has the icon, as on Windows and macOS, which have no icon themes, the
/// names in [`VIEWER_ICONS`] fall back to a built-in set in the text
/// colour; other names return `None`.  The result is cached, so calling
/// this every frame is cheap.
///
/// SVG icons draw at their design size (`22` for Breeze's tool bar icons),
/// so give the image an explicit size if the theme might only have another
/// size.  [`apply`] configures the theme; before that, the first call
/// detects it.
pub fn icon(name: &str, size: u32) -> Option<egui::ImageSource<'static>> {
    ensure_icons();
    icons::icon(name, size, false)
}

/// Like [`icon`], for icons drawn on a selection highlight: Breeze swaps
/// their text and highlight colours so they stay visible.
pub fn selected_icon(name: &str, size: u32) -> Option<egui::ImageSource<'static>> {
    ensure_icons();
    icons::icon(name, size, true)
}

fn ensure_icons() {
    if !icons::is_configured() {
        let mut env = Environment::from_env();
        env.resolve_fonts = false;
        let theme = detect_with(&env);
        icons::configure(&theme.icon_theme, theme.icon_colors());
    }
}

/// The icon names the viewer uses, with the freedesktop or KDE names that
/// every common theme (Breeze, Adwaita, Papirus) provides.  The test suite
/// checks they resolve in the installed Breeze theme, and that each has a
/// built-in stand-in for desktops without a theme.
pub const VIEWER_ICONS: &[&str] = &[
    "document-open",
    "document-save",
    "document-properties",
    "edit-copy",
    "media-playback-start",
    "media-playback-pause",
    "media-playback-stop",
    "media-skip-backward",
    "media-skip-forward",
    "zoom-in",
    "zoom-out",
    "zoom-fit-best",
    "zoom-original",
    "view-fullscreen",
    "view-restore",
    "view-grid",
    "view-refresh",
    "camera-photo",
    "go-previous",
    "go-next",
    "go-up",
    "go-down",
    "help-about",
    "application-exit",
    "configure",
    "edit-find",
    "object-rotate-left",
    "object-rotate-right",
    "visibility",
    "hint",
];

/// Reads a file's modification time, for change detection.
fn modified(path: &Path) -> Option<std::time::SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}
