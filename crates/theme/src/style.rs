//! Maps a [`DesktopTheme`] onto egui's style.
//!
//! The colour derivations follow Breeze (`breeze/kstyle/breezehelper.cpp`,
//! `breezestyle.cpp`): frames and separators are the text colour mixed into
//! the background by the frame contrast (0.2), menus are the window colour
//! mixed 30 % towards the view colour, pressed buttons are the button colour
//! mixed 30 % towards the highlight, slider grooves are the text colour at
//! 20 % then 70 % opacity over the window, and scroll bar handles are the
//! text colour at 50 % then 70 %.
//!
//! What egui's knobs can't express, and so doesn't match Breeze:
//! - Push buttons' soft drop shadow.
//! - Check boxes and radio buttons look the same checked and unchecked
//!   apart from the mark; Breeze also tints a checked box with the
//!   highlight colour and outlines it in the highlight.
//! - A focused text field's outline uses the selected-text colour, because
//!   egui draws both with `selection.stroke`; Breeze uses the focus colour.
//! - The slider handle's outline is the text colour (it shares
//!   `fg_stroke` with check marks); Breeze's is a faint mix that turns into
//!   the hover colour.
//! - Tooltips share the menu background; Breeze gives them the tooltip
//!   colour set.
//! - Text is greyscale anti-aliased; Qt on KDE uses sub-pixel rendering.
//!   egui also places glyphs at their exact advances where Qt rounds them
//!   to whole pixels, so a line of text runs about 3 % wider.

use std::sync::Arc;

use egui::style::{
    HandleShape, Interaction, ScrollAnimation, ScrollFadeStyle, ScrollStyle, Selection, TextCursorStyle, WidgetVisuals,
    Widgets,
};
use egui::{Color32, CornerRadius, FontFamily, FontId, Margin, Shadow, Stroke, Style, TextStyle, Theme, Vec2, Visuals};

use crate::color::Rgba;
use crate::fonts::BOLD;
use crate::{DesktopTheme, Metrics, icons};

/// The theme's colours resolved for drawing, including the colours Breeze
/// derives from the scheme, and its metrics.  [`apply`] stores it in the egui context;
/// [`palette`] reads it back for custom painting.
#[derive(Clone, Debug, PartialEq)]
pub struct Palette {
    pub dark: bool,
    pub window: Color32,
    pub window_text: Color32,
    /// Secondary text, such as captions and placeholders.
    pub inactive_text: Color32,
    pub view: Color32,
    pub view_text: Color32,
    /// The second colour of alternating rows.
    pub alternate_row: Color32,
    pub button: Color32,
    pub button_text: Color32,
    pub highlight: Color32,
    pub highlighted_text: Color32,
    /// The outline of focused widgets.
    pub focus: Color32,
    /// The outline of hovered fields.
    pub hover: Color32,
    pub link: Color32,
    pub negative: Color32,
    pub neutral: Color32,
    pub positive: Color32,
    pub tooltip: Color32,
    pub tooltip_text: Color32,
    /// Tool bar and header background.
    pub header: Color32,
    pub header_text: Color32,
    /// Frames around views and fields.
    pub frame_outline: Color32,
    /// Push button outlines.
    pub button_outline: Color32,
    pub separator: Color32,
    /// Menu, popup and combo box list background.
    pub menu: Color32,
    pub pressed_button: Color32,
    /// Empty part of slider and progress bar grooves.
    pub groove: Color32,
    /// Background of a hovered menu item or list row.
    pub hover_fill: Color32,
    /// Outline of a hovered menu item.
    pub hover_outline: Color32,
    /// Arrows on combo boxes, tree branches and menus.
    pub arrow: Color32,
    /// Fill of a checked check box or radio button.
    pub checked_fill: Color32,
    /// The theme's widget sizes, which the painters in `widgets` share.
    pub metrics: Metrics,
}

impl Palette {
    pub fn new(theme: &DesktopTheme) -> Palette {
        let c = &theme.colors;
        let contrast = c.frame_contrast;
        let window = c.window.background;
        let window_text = c.window.foreground;
        let view = c.view.background;
        let button = c.button.background;
        let button_text = c.button.foreground;
        let highlight = c.selection.background;
        let focus = c.view.focus;
        let g = Rgba::to_color32;
        Palette {
            dark: theme.dark,
            window: g(window),
            window_text: g(window_text),
            inactive_text: g(c.window.inactive),
            view: g(view),
            view_text: g(c.view.foreground),
            alternate_row: g(c.view.alternate_background),
            button: g(button),
            button_text: g(button_text),
            highlight: g(highlight),
            highlighted_text: g(c.selection.foreground),
            focus: g(focus),
            hover: g(c.view.hover),
            link: g(c.view.link),
            negative: g(c.window.negative),
            neutral: g(c.window.neutral),
            positive: g(c.window.positive),
            tooltip: g(c.tooltip.background),
            tooltip_text: g(c.tooltip.foreground),
            header: g(c.header.background),
            header_text: g(c.header.foreground),
            // Helper::frameOutlineColor and separatorColor.
            frame_outline: g(window.mix(window_text, contrast)),
            separator: g(window.mix(window_text, contrast)),
            // Helper::renderButtonFrame, normal state.
            button_outline: g(button.mix(button_text, contrast)),
            // Helper::frameBackgroundColor (Metrics::Blend_Value = 0.3).
            menu: g(window.mix(view, 0.3)),
            // Helper::renderButtonFrame, pressed.
            pressed_button: g(button.mix(highlight, 0.3)),
            // Style::drawSliderComplexControl and renderSliderGroove.
            groove: g(window_text.alpha(0.2).alpha(0.7).over(window)),
            // Style::drawMenuItemControl: focus colour at Blend_Value,
            // outlined by Helper::focusOutlineColor.
            hover_fill: g(focus.alpha(0.3).over(window.mix(view, 0.3))),
            hover_outline: g(focus.mix(window_text, 0.15)),
            // Helper::arrowColor (arrowShade = 0.15).
            arrow: g(window_text.mix(window, 0.15)),
            // Helper::renderCheckBoxBackground: highlight at Blend_Value.
            checked_fill: g(highlight.alpha(0.3).over(button)),
            metrics: theme.metrics.clone(),
        }
    }
}

fn stroke(color: Color32) -> Stroke {
    Stroke::new(1.0, color)
}

fn radius(r: f32) -> CornerRadius {
    CornerRadius::same(r.round().clamp(0.0, 255.0) as u8)
}

fn margin(m: f32) -> Margin {
    Margin::same(m.round().clamp(0.0, 127.0) as i8)
}

/// The egui style for a theme, without touching any context.
pub fn style_for(theme: &DesktopTheme) -> Style {
    let p = Palette::new(theme);
    let m = &theme.metrics;
    let mut style = Style::default();

    // 1. Text styles.  KDE sizes everything from the general font; headings
    //    follow Kirigami's level 1 (1.35 x) and level 2 (1.2 x).
    let fonts = &theme.fonts;
    let general = fonts.general.size_px(fonts.dpi);
    let fixed = fonts.fixed.size_px(fonts.dpi);
    let small = fonts.small.size_px(fonts.dpi);
    style.text_styles = [
        (TextStyle::Small, FontId::new(small, FontFamily::Proportional)),
        (TextStyle::Body, FontId::new(general, FontFamily::Proportional)),
        (TextStyle::Button, FontId::new(general, FontFamily::Proportional)),
        (TextStyle::Heading, FontId::new(general * 1.35, FontFamily::Proportional)),
        (TextStyle::Monospace, FontId::new(fixed, FontFamily::Monospace)),
        (TextStyle::Name("Heading2".into()), FontId::new(general * 1.2, FontFamily::Proportional)),
        (TextStyle::Name("Bold".into()), FontId::new(general, FontFamily::Name(BOLD.into()))),
    ]
    .into();

    // 2. Sizes.
    let text_row = style_row_height(general);
    let s = &mut style.spacing;
    s.item_spacing = Vec2::splat(m.spacing);
    s.window_margin = margin(m.window_margin);
    s.menu_margin = margin(m.menu_margin);
    s.button_padding = Vec2::from(m.button_padding);
    s.indent = m.indent;
    s.interact_size = Vec2::new(40.0, m.control_height.max(text_row));
    s.slider_width = 160.0;
    s.slider_rail_height = m.slider_groove;
    s.icon_width = m.check_size;
    s.icon_width_inner = (m.check_size * 0.5).round();
    // Breeze reserves 2 px around its 16 px box inside a 20 px indicator,
    // so its text starts 6 px after the box.
    s.icon_spacing = 6.0;
    s.menu_spacing = 0.0;
    s.combo_width = 120.0;
    s.scroll = scroll_style(m);

    style.interaction = Interaction {
        tooltip_delay: m.tooltip_delay,
        tooltip_grace_time: m.tooltip_grace,
        // QLabel text isn't selectable unless an app asks for it.
        selectable_labels: false,
        ..Interaction::default()
    };
    style.animation_time = 0.1 * theme.animation_factor;
    style.scroll_animation =
        if theme.animation_factor <= 0.0 { ScrollAnimation::none() } else { ScrollAnimation::default() };

    // 3. Colours.
    style.visuals = visuals(theme, &p, m);
    style
}

/// The height egui gives a row of body text: the font's ascent plus
/// descent, about 1.36 em for Noto Sans.  Used so check box rows are never
/// shorter than their text.
fn style_row_height(font_px: f32) -> f32 {
    (font_px * 1.36).ceil()
}

/// Breeze's scroll bars: no visible groove, an 8 px rounded handle centred
/// in a 21 px strip, the text colour at 35 % that strengthens to 50 % on
/// hover.  egui's "floating" bars are the ones that can hide their groove,
/// so this uses them with a fixed width and allocated space.
fn scroll_style(m: &Metrics) -> ScrollStyle {
    let side = ((m.scroll_bar_extent - m.scroll_handle_width) / 2.0).max(0.0);
    ScrollStyle {
        floating: true,
        bar_width: m.scroll_handle_width,
        floating_width: m.scroll_handle_width,
        floating_allocated_width: m.scroll_bar_extent,
        handle_min_length: m.scroll_handle_min_length,
        bar_inner_margin: side,
        bar_outer_margin: side,
        foreground_color: true,
        dormant_background_opacity: 0.0,
        active_background_opacity: 0.0,
        interact_background_opacity: 0.0,
        dormant_handle_opacity: 0.35,
        active_handle_opacity: 0.35,
        interact_handle_opacity: 0.5,
        fade: ScrollFadeStyle { strength: 0.0, size: 0.0 },
        ..ScrollStyle::solid()
    }
}

fn visuals(theme: &DesktopTheme, p: &Palette, m: &Metrics) -> Visuals {
    let mut v = if theme.dark { Visuals::dark() } else { Visuals::light() };
    let r = radius(m.frame_radius);
    let outline = |color: Color32| if m.button_outline { stroke(color) } else { Stroke::NONE };
    // Check marks are drawn with the foreground stroke; Breeze's check
    // pen is twice its frame width.
    let mark = 2.0;
    v.widgets = Widgets {
        noninteractive: WidgetVisuals {
            bg_fill: p.window,
            weak_bg_fill: p.window,
            bg_stroke: stroke(p.separator),
            fg_stroke: stroke(p.window_text),
            corner_radius: r,
            expansion: 0.0,
        },
        inactive: WidgetVisuals {
            bg_fill: p.button,
            weak_bg_fill: p.button,
            bg_stroke: outline(p.button_outline),
            fg_stroke: Stroke::new(mark, p.button_text),
            corner_radius: r,
            expansion: 0.0,
        },
        // Breeze keeps the fill on hover and outlines the button in the
        // highlight colour.
        hovered: WidgetVisuals {
            bg_fill: p.button,
            weak_bg_fill: if m.button_outline { p.button } else { mix32(p.button, p.button_text, 0.05) },
            bg_stroke: stroke(p.highlight),
            fg_stroke: Stroke::new(mark, p.button_text),
            corner_radius: r,
            expansion: 0.0,
        },
        active: WidgetVisuals {
            bg_fill: p.pressed_button,
            weak_bg_fill: p.pressed_button,
            bg_stroke: stroke(p.highlight),
            fg_stroke: Stroke::new(mark, p.button_text),
            corner_radius: r,
            expansion: 0.0,
        },
        // A combo box with its list open looks pressed in Breeze.
        open: WidgetVisuals {
            bg_fill: p.pressed_button,
            weak_bg_fill: p.pressed_button,
            bg_stroke: stroke(p.highlight),
            fg_stroke: Stroke::new(mark, p.button_text),
            corner_radius: r,
            expansion: 0.0,
        },
    };
    v.dark_mode = theme.dark;
    // Linear coverage, as FreeType gives Qt.  egui's dark-mode default
    // (2c - c²) thickens light-on-dark text noticeably beyond KDE's.
    v.text_options.color_transfer_function = egui::epaint::FontColorTransferFunction::Off;
    v.override_text_color = None;
    v.weak_text_color = Some(p.inactive_text);
    v.selection = Selection { bg_fill: p.highlight, stroke: stroke(p.highlighted_text) };
    v.hyperlink_color = p.link;
    v.faint_bg_color = p.alternate_row;
    // egui paints progress bar grooves with this, and Frame::canvas.
    v.extreme_bg_color = p.groove;
    v.text_edit_bg_color = Some(p.view);
    v.code_bg_color = mix32(p.window, p.window_text, 0.08);
    v.warn_fg_color = p.neutral;
    v.error_fg_color = p.negative;
    v.window_corner_radius = radius(m.popup_radius);
    v.menu_corner_radius = radius(m.popup_radius);
    let shadow_alpha = if theme.dark { 110 } else { 50 };
    v.window_shadow = Shadow { offset: [0, 4], blur: 20, spread: 0, color: Color32::from_black_alpha(shadow_alpha) };
    v.popup_shadow = Shadow { offset: [0, 2], blur: 10, spread: 0, color: Color32::from_black_alpha(shadow_alpha) };
    v.window_fill = p.menu;
    v.window_stroke = stroke(p.frame_outline);
    v.window_highlight_topmost = false;
    v.panel_fill = p.window;
    v.text_cursor = TextCursorStyle {
        stroke: stroke(p.view_text),
        preview: false,
        blink: true,
        // Qt's default cursor flash time is 1 s per cycle.
        on_duration: 0.5,
        off_duration: 0.5,
    };
    v.button_frame = true;
    v.collapsing_header_frame = false;
    v.indent_has_left_vline = false;
    v.striped = false;
    v.slider_trailing_fill = true;
    v.handle_shape = HandleShape::Circle;
    v.interact_cursor = None;
    v.disabled_alpha = (1.0 - theme.colors.disabled_fade).clamp(0.3, 0.7);
    v
}

fn mix32(a: Color32, b: Color32, t: f32) -> Color32 {
    let to = |c: Color32| {
        let [r, g, b, a] = c.to_srgba_unmultiplied();
        Rgba::rgba8(r, g, b, a)
    };
    to(a).mix(to(b), t).to_color32()
}

/// The context key the palette is stored under.
fn palette_id() -> egui::Id {
    egui::Id::new("slerp_theme::Palette")
}

/// Set once the fallback fonts' loading thread has started.
static FALLBACKS_STARTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Styles `ctx` after `theme`: colours, sizes, fonts and the icon theme.
/// Call once at start-up; [`crate::ThemeWatcher`] calls it again when the
/// desktop's theme changes.  Fonts are only reloaded when they changed,
/// since that rebuilds egui's glyph atlas.
pub fn apply(ctx: &egui::Context, theme: &DesktopTheme) {
    // 1. Style, the same for egui's light and dark slots, so egui following
    //    the window system's theme can't swap in its own colours.
    let style = Arc::new(style_for(theme));
    ctx.set_style_of(Theme::Dark, style.clone());
    ctx.set_style_of(Theme::Light, style);
    ctx.set_theme(if theme.dark { Theme::Dark } else { Theme::Light });

    // 2. Fonts, when they differ from what was applied last.
    let fonts_id = egui::Id::new("slerp_theme::fonts");
    let previous = ctx.data(|d| d.get_temp::<Arc<crate::Fonts>>(fonts_id));
    if previous.as_deref() != Some(&theme.fonts) {
        ctx.set_fonts(theme.fonts.font_definitions());
        ctx.data_mut(|d| d.insert_temp(fonts_id, Arc::new(theme.fonts.clone())));
    }
    // 2a. Fonts for CJK and other scripts, read on a thread of their own
    //     (tens of megabytes), then added behind the desktop fonts.
    if !crate::fonts::fallbacks_loaded() && !FALLBACKS_STARTED.swap(true, std::sync::atomic::Ordering::Relaxed) {
        let ctx = ctx.clone();
        let spawned = std::thread::Builder::new().name("slerp-fonts".into()).spawn(move || {
            if crate::fonts::load_fallbacks()
                && let Some(fonts) = ctx.data(|d| d.get_temp::<Arc<crate::Fonts>>(fonts_id))
            {
                ctx.set_fonts(fonts.font_definitions());
                ctx.request_repaint();
            }
        });
        if spawned.is_err() {
            FALLBACKS_STARTED.store(false, std::sync::atomic::Ordering::Relaxed);
        }
    }

    // 3. Icons, and the image loaders that draw them.
    egui_extras::install_image_loaders(ctx);
    let forgotten = icons::configure(&theme.icon_theme, theme.icon_colors());
    for uri in forgotten {
        ctx.forget_image(&uri);
    }

    // 4. The palette, for the painters in `widgets`.
    ctx.data_mut(|d| d.insert_temp(palette_id(), Arc::new(Palette::new(theme))));
    ctx.request_repaint();
}

/// The palette [`apply`] stored, or one detected now if it wasn't called.
pub fn palette(ctx: &egui::Context) -> Arc<Palette> {
    if let Some(palette) = ctx.data(|d| d.get_temp::<Arc<Palette>>(palette_id())) {
        return palette;
    }
    let mut env = crate::Environment::from_env();
    env.resolve_fonts = false;
    let palette = Arc::new(Palette::new(&crate::detect_with(&env)));
    ctx.data_mut(|d| d.insert_temp(palette_id(), palette.clone()));
    palette
}
