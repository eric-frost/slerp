//! KDE colour schemes: the seven colour sets of `kdeglobals` or a `.colors`
//! file, with the fallbacks KColorScheme applies when a set or key is
//! missing.

use crate::color::Rgba;
use crate::kconfig::KConfig;

/// One colour set of a KDE colour scheme, such as `[Colors:View]`.  Each set
/// pairs backgrounds with the foregrounds meant to be read on them.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ColorSet {
    /// `BackgroundNormal`.
    pub background: Rgba,
    /// `BackgroundAlternate`, used for alternating rows.
    pub alternate_background: Rgba,
    /// `ForegroundNormal`, ordinary text.
    pub foreground: Rgba,
    /// `ForegroundInactive`, secondary and placeholder text.
    pub inactive: Rgba,
    /// `ForegroundActive`, text that is "on" or current.
    pub active: Rgba,
    /// `ForegroundLink`.
    pub link: Rgba,
    /// `ForegroundVisited`.
    pub visited: Rgba,
    /// `ForegroundNegative`, errors.
    pub negative: Rgba,
    /// `ForegroundNeutral`, warnings.
    pub neutral: Rgba,
    /// `ForegroundPositive`, success.
    pub positive: Rgba,
    /// `DecorationFocus`, the outline of focused widgets.
    pub focus: Rgba,
    /// `DecorationHover`, the outline of hovered widgets.
    pub hover: Rgba,
}

/// The colour sets of a scheme, named as in `kdeglobals`.
#[derive(Clone, Debug, PartialEq)]
pub struct ColorScheme {
    /// The scheme's file name without `.colors`, such as `BreezeDark`, when
    /// known.
    pub name: Option<String>,
    /// Window backgrounds, labels and panels.
    pub window: ColorSet,
    /// Content areas: lists, text fields, tables.
    pub view: ColorSet,
    /// Push buttons, combo boxes, check boxes.
    pub button: ColorSet,
    /// Selected items and selected text.
    pub selection: ColorSet,
    pub tooltip: ColorSet,
    /// Tool bars and title areas, which Breeze draws a shade darker than
    /// windows in light schemes.
    pub header: ColorSet,
    /// The inverted set Plasma uses for OSDs and dark areas in light themes.
    pub complementary: ColorSet,
    /// `[General] AccentColor`, when the user picked one.  Plasma has already
    /// folded it into the sets above when it wrote `kdeglobals`.
    pub accent: Option<Rgba>,
    /// `[KDE] frameContrast`: how far frames and separators are mixed from
    /// the background towards the text (0.2 by default).
    pub frame_contrast: f32,
    /// How much a disabled widget's text fades, from `[ColorEffects:Disabled]`
    /// when its contrast effect is the default fade (0.65 in Breeze).
    pub disabled_fade: f32,
}

const SETS: [&str; 7] = ["Window", "View", "Button", "Selection", "Tooltip", "Header", "Complementary"];

impl ColorScheme {
    /// Breeze Light, which is also what KColorScheme falls back to for
    /// anything a scheme leaves out.
    pub fn breeze_light() -> ColorScheme {
        let set = |bg, alt, fg, inactive, link| ColorSet {
            background: bg,
            alternate_background: alt,
            foreground: fg,
            inactive,
            active: BREEZE_BLUE,
            link,
            visited: Rgba::rgb8(155, 89, 182),
            negative: Rgba::rgb8(218, 68, 83),
            neutral: Rgba::rgb8(246, 116, 0),
            positive: Rgba::rgb8(39, 174, 96),
            focus: BREEZE_BLUE,
            hover: BREEZE_BLUE,
        };
        let text = Rgba::rgb8(35, 38, 41);
        let inactive = Rgba::rgb8(112, 125, 138);
        let link = Rgba::rgb8(41, 128, 185);
        let dark_text = Rgba::rgb8(252, 252, 252);
        let dark_inactive = Rgba::rgb8(161, 169, 177);
        ColorScheme {
            name: Some("BreezeLight".into()),
            window: set(Rgba::rgb8(239, 240, 241), Rgba::rgb8(227, 229, 231), text, inactive, link),
            view: set(Rgba::rgb8(255, 255, 255), Rgba::rgb8(247, 247, 247), text, inactive, link),
            button: set(Rgba::rgb8(252, 252, 252), Rgba::rgb8(163, 212, 250), text, inactive, link),
            selection: ColorSet {
                active: Rgba::WHITE,
                link: Rgba::rgb8(253, 188, 75),
                negative: Rgba::rgb8(176, 55, 69),
                neutral: Rgba::rgb8(198, 92, 0),
                positive: Rgba::rgb8(23, 104, 57),
                ..set(BREEZE_BLUE, Rgba::rgb8(163, 212, 250), Rgba::WHITE, inactive, link)
            },
            tooltip: set(Rgba::rgb8(247, 247, 247), Rgba::rgb8(239, 240, 241), text, inactive, link),
            header: set(Rgba::rgb8(222, 224, 226), Rgba::rgb8(239, 240, 241), text, inactive, link),
            complementary: set(
                Rgba::rgb8(42, 46, 50),
                Rgba::rgb8(27, 30, 32),
                dark_text,
                dark_inactive,
                Rgba::rgb8(29, 153, 243),
            ),
            accent: None,
            frame_contrast: 0.2,
            disabled_fade: 0.65,
        }
    }

    /// Breeze Dark, the fallback for dark schemes that leave keys out.
    pub fn breeze_dark() -> ColorScheme {
        let text = Rgba::rgb8(252, 252, 252);
        let inactive = Rgba::rgb8(161, 169, 177);
        let link = Rgba::rgb8(29, 153, 243);
        let set = |bg, alt| ColorSet {
            background: bg,
            alternate_background: alt,
            foreground: text,
            inactive,
            active: BREEZE_BLUE,
            link,
            visited: Rgba::rgb8(155, 89, 182),
            negative: Rgba::rgb8(218, 68, 83),
            neutral: Rgba::rgb8(246, 116, 0),
            positive: Rgba::rgb8(39, 174, 96),
            focus: BREEZE_BLUE,
            hover: BREEZE_BLUE,
        };
        ColorScheme {
            name: Some("BreezeDark".into()),
            window: set(Rgba::rgb8(32, 35, 38), Rgba::rgb8(41, 44, 48)),
            view: set(Rgba::rgb8(20, 22, 24), Rgba::rgb8(29, 31, 34)),
            button: set(Rgba::rgb8(41, 44, 48), Rgba::rgb8(30, 87, 116)),
            selection: ColorSet {
                active: text,
                link: Rgba::rgb8(253, 188, 75),
                negative: Rgba::rgb8(176, 55, 69),
                neutral: Rgba::rgb8(198, 92, 0),
                positive: Rgba::rgb8(23, 104, 57),
                ..set(BREEZE_BLUE, Rgba::rgb8(30, 87, 116))
            },
            tooltip: set(Rgba::rgb8(41, 44, 48), Rgba::rgb8(32, 35, 38)),
            header: set(Rgba::rgb8(41, 44, 48), Rgba::rgb8(32, 35, 38)),
            complementary: set(Rgba::rgb8(32, 35, 38), Rgba::rgb8(30, 87, 116)),
            accent: None,
            frame_contrast: 0.2,
            disabled_fade: 0.65,
        }
    }

    /// Whether the scheme is dark, judged by its window background.
    pub fn is_dark(&self) -> bool {
        self.window.background.is_dark()
    }

    /// Reads a scheme the way KColorScheme does, key by key: from
    /// `globals` (the cascaded `kdeglobals`), then from the named scheme
    /// file `scheme` when `kdeglobals` lacks a set, then from Breeze.  The
    /// Breeze fallback is Breeze Dark when the scheme looks dark.
    pub fn load(globals: &KConfig, scheme: Option<&KConfig>) -> ColorScheme {
        let name = globals
            .get("General", "ColorScheme")
            .or_else(|| scheme.and_then(|s| s.get("General", "ColorScheme")))
            .map(str::to_owned);

        // 1. Decide which Breeze variant fills gaps, from whatever window
        //    background is available.
        let window_bg = globals
            .get("Colors:Window", "BackgroundNormal")
            .or_else(|| scheme.and_then(|s| s.get("Colors:Window", "BackgroundNormal")))
            .and_then(Rgba::parse);
        let dark = match window_bg {
            Some(bg) => bg.is_dark(),
            None => name.as_deref().is_some_and(|n| n.to_ascii_lowercase().contains("dark")),
        };
        let defaults = if dark { ColorScheme::breeze_dark() } else { ColorScheme::breeze_light() };

        // 2. Read each set, noting which ones came from kdeglobals (and so
        //    already carry the accent colour).
        let mut out = defaults.clone();
        out.name = name.or(defaults.name.clone());
        let mut from_globals = [false; 7];
        for (i, set_name) in SETS.iter().enumerate() {
            let group = format!("Colors:{set_name}");
            from_globals[i] = globals.has_group(&group);
            let fallback = match *set_name {
                // KColorScheme reads a missing Header set from Window.
                "Header" if !globals.has_group(&group) && !scheme.is_some_and(|s| s.has_group(&group)) => {
                    read_set(globals, scheme, "Colors:Window", *defaults.set(i))
                }
                _ => *defaults.set(i),
            };
            *out.set_mut(i) = read_set(globals, scheme, &group, fallback);
        }

        // 3. General settings.
        let read = |group: &str, key: &str| globals.get(group, key).or_else(|| scheme.and_then(|s| s.get(group, key)));
        out.accent = globals.get("General", "AccentColor").and_then(Rgba::parse).filter(|c| c.a > 0.0);
        out.frame_contrast = read("KDE", "frameContrast")
            .and_then(|v| v.trim().parse::<f32>().ok())
            .filter(|v| v.is_finite())
            .map_or(0.2, |v| v.clamp(0.0, 1.0));
        let fade_effect = read("ColorEffects:Disabled", "ContrastEffect").and_then(|v| v.trim().parse::<i32>().ok());
        let fade_amount = read("ColorEffects:Disabled", "ContrastAmount").and_then(|v| v.trim().parse::<f32>().ok());
        out.disabled_fade = match (fade_effect.unwrap_or(1), fade_amount) {
            (1, Some(amount)) if amount.is_finite() => amount.clamp(0.0, 1.0),
            (1, None) => 0.65,
            _ => 0.5,
        };

        // 4. Apply the accent to sets that came from the scheme file rather
        //    than from kdeglobals, as Plasma's colour settings do when they
        //    write kdeglobals (plasma-workspace kcms/colors/colorsapplicator.cpp).
        if let Some(accent) = out.accent {
            let view_bg = out.view.background;
            let button_bg = out.button.background;
            for (i, came_from_globals) in from_globals.iter().enumerate() {
                if *came_from_globals {
                    continue;
                }
                let set = out.set_mut(i);
                set.active = accent;
                set.link = accent;
                set.focus = accent;
                set.hover = accent;
                match SETS[i] {
                    "Selection" => {
                        let bg = accent.with_alpha(0.7).over(view_bg);
                        set.background = bg;
                        set.alternate_background = bg;
                        set.foreground = accent_foreground(bg, true);
                        set.inactive = accent_foreground(bg, false);
                    }
                    "Button" => set.alternate_background = accent.with_alpha(0.7).over(button_bg),
                    _ => {}
                }
            }
        }
        out
    }

    fn set(&self, i: usize) -> &ColorSet {
        match i {
            0 => &self.window,
            1 => &self.view,
            2 => &self.button,
            3 => &self.selection,
            4 => &self.tooltip,
            5 => &self.header,
            _ => &self.complementary,
        }
    }

    fn set_mut(&mut self, i: usize) -> &mut ColorSet {
        match i {
            0 => &mut self.window,
            1 => &mut self.view,
            2 => &mut self.button,
            3 => &mut self.selection,
            4 => &mut self.tooltip,
            5 => &mut self.header,
            _ => &mut self.complementary,
        }
    }
}

/// Breeze's default highlight blue, `#3daee9`.
pub const BREEZE_BLUE: Rgba = Rgba::rgb8(61, 174, 233);

/// Plasma's `accentForeground`: white or black text, whichever reads on the
/// accent, at full strength for normal text and 60 % for inactive text.
fn accent_foreground(background: Rgba, active: bool) -> Rgba {
    let text = if background.contrast_ratio(Rgba::WHITE) < 4.5 { Rgba::BLACK } else { Rgba::WHITE };
    text.with_alpha(if active { 1.0 } else { 0.6 }).over(background)
}

fn read_set(globals: &KConfig, scheme: Option<&KConfig>, group: &str, fallback: ColorSet) -> ColorSet {
    let read = |key: &str, fallback: Rgba| {
        globals
            .get(group, key)
            .and_then(Rgba::parse)
            .or_else(|| scheme.and_then(|s| s.get(group, key)).and_then(Rgba::parse))
            .unwrap_or(fallback)
    };
    ColorSet {
        background: read("BackgroundNormal", fallback.background),
        alternate_background: read("BackgroundAlternate", fallback.alternate_background),
        foreground: read("ForegroundNormal", fallback.foreground),
        inactive: read("ForegroundInactive", fallback.inactive),
        active: read("ForegroundActive", fallback.active),
        link: read("ForegroundLink", fallback.link),
        visited: read("ForegroundVisited", fallback.visited),
        negative: read("ForegroundNegative", fallback.negative),
        neutral: read("ForegroundNeutral", fallback.neutral),
        positive: read("ForegroundPositive", fallback.positive),
        focus: read("DecorationFocus", fallback.focus),
        hover: read("DecorationHover", fallback.hover),
    }
}
