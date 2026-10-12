//! Optional pieces for a more native look where egui's global style knobs
//! fall short: Breeze's chevron arrows for combo boxes and collapsing
//! headers, Breeze menus and menu bars, panel frames with Breeze's margins
//! and colours, and flat tool bar buttons.  Each reads the palette that
//! [`crate::apply`] stored.
//!
//! The form controls also behave like Qt's at the keyboard.  A click gives
//! a control the keyboard focus, which shows in the focus colour, and Tab
//! and Shift+Tab move it on.  While a control has the focus, the keys that
//! operate it are its own: a [`Choice`] (combo box) steps through its items
//! with the arrows, Home and End without opening, [`slider`]s step with the
//! arrows and take bigger steps with Page Up and Page Down, a [`List`]'s
//! arrows move its selection, a [`radio_group`]'s arrows move between its
//! options, and Space toggles a [`CheckBox`].  The mouse wheel over a
//! combo box steps it, once the window passes its input through
//! [`wheel_hook`].  An application's own single-key shortcuts should apply
//! only while no control has the focus.
//!
//! KDE keyboard conventions the viewer should follow (KStandardShortcut):
//! Ctrl+O open, Ctrl+Q quit, Ctrl+W close, F11 or Ctrl+Shift+F full
//! screen, Ctrl+Plus and Ctrl+Minus zoom, Ctrl+0 actual size, Ctrl+F find,
//! F5 reload, Ctrl+Shift+, configure, F1 help, Alt+Left and Alt+Right or
//! Back and Forward for previous and next, Ctrl+M show or hide the menu
//! bar, Ctrl+C copy, and Escape to leave full screen.  Alt opens the menu
//! bar's first menu when the bar is visible.

mod choice;
mod keys;
mod list;
mod tree;
#[cfg(test)]
mod tests;

pub use choice::Choice;
pub use keys::wheel_hook;
pub use list::{List, radio_group};
pub use tree::{TreeEvent, TreeOutput, TreeRow, tree};

use egui::containers::menu::{MenuBar, MenuConfig};
use egui::style::{StyleModifier, WidgetVisuals};
use egui::{
    Color32, CornerRadius, Frame, Image, ImageSource, Margin, Pos2, Rect, Response, Sense, Shape, Stroke, Style, Ui,
    Vec2, Widget, pos2, vec2,
};

use crate::style::palette;
use crate::{Palette, icon};

/// Breeze's arrow (`Helper::renderArrow`): an open chevron drawn with a
/// one pixel pen, pointing down when `down`, otherwise right.
pub fn paint_arrow(ui: &Ui, rect: Rect, color: Color32, angle: f32) {
    let size = rect.width().min(rect.height()).min(10.0);
    let c = rect.center();
    // The chevron spans the arrow box's width and half its height.
    let points = [vec2(-0.5, -0.25), vec2(0.0, 0.25), vec2(0.5, -0.25)];
    let rot = egui::emath::Rot2::from_angle(angle);
    let pts: Vec<Pos2> = points.iter().map(|p| c + rot * (*p * size)).collect();
    ui.painter().add(Shape::line(pts, Stroke::new(1.0, color)));
}

/// A combo box arrow in Breeze's style, for `egui::ComboBox::icon`.  Its
/// colour follows Breeze: the arrow colour normally, the hover colour when
/// hovered or open.
pub fn combo_icon(ui: &Ui, rect: Rect, visuals: &WidgetVisuals, open: bool) {
    let p = palette(ui.ctx());
    let hovered = visuals.bg_stroke.color == p.highlight;
    let color = if open || hovered { p.hover } else { p.arrow };
    let rect = Rect::from_center_size(rect.center(), Vec2::splat(10.0));
    paint_arrow(ui, rect, color, 0.0);
}

/// A combo box with Breeze's arrow and a Breeze menu as its list, where
/// the current item is marked like a hovered one rather than in solid
/// highlight.  Configure it further as usual (`width`, `show_ui`).
pub fn combo_box(
    ctx: &egui::Context,
    id_salt: impl std::hash::Hash + std::fmt::Debug,
    selected_text: impl Into<egui::WidgetText>,
) -> egui::ComboBox {
    let p = palette(ctx);
    let menu = menu_style(&p, &p.metrics);
    let (hover_fill, text) = (p.hover_fill, p.window_text);
    let popup = StyleModifier::new(move |style: &mut Style| {
        menu(style);
        style.visuals.selection.bg_fill = hover_fill;
        style.visuals.selection.stroke.color = text;
    });
    egui::ComboBox::from_id_salt(id_salt).selected_text(selected_text).icon(combo_icon).popup_style(popup)
}

/// A collapsing header arrow in Breeze's tree view style, for
/// `egui::CollapsingHeader::icon`: a chevron that turns from right to down.
pub fn collapsing_icon(ui: &mut Ui, openness: f32, response: &Response) {
    let p = palette(ui.ctx());
    let color = if response.hovered() { p.hover } else { p.arrow };
    let rect = Rect::from_center_size(response.rect.center(), Vec2::splat(10.0));
    let angle = (1.0 - openness) * -std::f32::consts::FRAC_PI_2;
    paint_arrow(ui, rect, color, angle);
}

/// Breeze's popup menu look (`Style::drawMenuItemControl`): items 29 px
/// tall with text 12 px in, a hovered item tinted with the focus colour and
/// outlined, and no frame on resting items.
pub fn menu_style(p: &Palette, m: &crate::Metrics) -> impl Fn(&mut Style) + Send + Sync + 'static {
    let (hover_fill, hover_outline, focus, highlighted_text) =
        (p.hover_fill, p.hover_outline, p.focus, p.highlighted_text);
    let padding = Vec2::from(m.menu_item_padding);
    let radius = CornerRadius::same((m.frame_radius - 0.5).round().max(0.0) as u8);
    move |style: &mut Style| {
        style.spacing.button_padding = padding;
        style.spacing.item_spacing.y = 0.0;
        style.spacing.interact_size.y = 0.0;
        let w = &mut style.visuals.widgets;
        w.inactive.weak_bg_fill = Color32::TRANSPARENT;
        w.inactive.bg_stroke = Stroke::NONE;
        for state in [&mut w.hovered, &mut w.open] {
            state.weak_bg_fill = hover_fill;
            state.bg_fill = hover_fill;
            state.bg_stroke = Stroke::new(1.0, hover_outline);
            state.corner_radius = radius;
        }
        w.active.weak_bg_fill = focus;
        w.active.bg_fill = focus;
        w.active.bg_stroke = Stroke::new(1.0, focus);
        w.active.fg_stroke.color = highlighted_text;
        w.active.corner_radius = radius;
    }
}

/// Breeze's menu bar look (`Style::drawMenuBarItemControl`): flat items
/// with 10 px side margins that fill with the hover colour, text turning
/// to the highlighted-text colour.
pub fn menu_bar_style(p: &Palette, m: &crate::Metrics) -> impl Fn(&mut Style) + Send + Sync + 'static {
    let (hover, highlighted_text) = (p.hover, p.highlighted_text);
    let padding = Vec2::from(m.menu_bar_item_padding);
    let radius = CornerRadius::same((m.frame_radius - 0.5).round().max(0.0) as u8);
    move |style: &mut Style| {
        style.spacing.button_padding = padding;
        style.spacing.item_spacing.x = 0.0;
        style.spacing.interact_size.y = 0.0;
        // An open menu's title is a selected button (see `menu_button`).
        style.visuals.selection.bg_fill = hover;
        style.visuals.selection.stroke.color = highlighted_text;
        let w = &mut style.visuals.widgets;
        w.inactive.weak_bg_fill = Color32::TRANSPARENT;
        w.inactive.bg_stroke = Stroke::NONE;
        for state in [&mut w.hovered, &mut w.open, &mut w.active] {
            state.weak_bg_fill = hover;
            state.bg_fill = hover;
            state.bg_stroke = Stroke::NONE;
            state.fg_stroke.color = highlighted_text;
            state.corner_radius = radius;
        }
    }
}

/// A menu bar styled like Breeze's, whose menus are styled like Breeze's
/// too.  Use in place of `egui::MenuBar::new()`.
pub fn menu_bar(ctx: &egui::Context) -> MenuBar {
    let p = palette(ctx);
    MenuBar::new().style(StyleModifier::new(menu_bar_style(&p, &p.metrics))).config(menu_config(ctx))
}

/// A menu bar entry that stays highlighted while its menu is open, as
/// Breeze's do.  Use inside [`menu_bar`] in place of `ui.menu_button`.
pub fn menu_button<R>(ui: &mut Ui, title: &str, add_contents: impl FnOnce(&mut Ui) -> R) -> Response {
    // egui's menu buttons look the same whether or not their menu is open,
    // so find out first and draw the button as selected.  The popup's id is
    // derived from the id the button is about to get.
    let open = egui::Popup::is_id_open(ui.ctx(), ui.next_auto_id().with("popup"));
    let button = egui::Button::new(title).selected(open);
    egui::containers::menu::MenuButton::from_button(button).ui(ui, add_contents).0
}

/// A menu configuration with Breeze's menu item style, for menus opened
/// outside a [`menu_bar`] (`MenuButton::config`, `Popup::menu`).
pub fn menu_config(ctx: &egui::Context) -> MenuConfig {
    let p = palette(ctx);
    MenuConfig::new().style(StyleModifier::new(menu_style(&p, &p.metrics)))
}

/// The frame for a menu bar, in KDE's Header colours: Breeze paints the
/// menu bar and tool bars as one tool area continuing the title bar.
pub fn menu_bar_frame(ctx: &egui::Context) -> Frame {
    let p = palette(ctx);
    Frame::new().fill(p.header).inner_margin(Margin::symmetric(0, 0))
}

/// The frame for a tool bar under the menu bar: Header colours and the
/// margins that make Breeze's tool bars 45 px tall around 34 px buttons.
pub fn tool_bar_frame(ctx: &egui::Context) -> Frame {
    let p = palette(ctx);
    Frame::new().fill(p.header).inner_margin(Margin { left: 6, right: 6, top: 5, bottom: 6 })
}

/// The frame for a side panel: window colour with Breeze's 6 px child
/// margin.
pub fn side_panel_frame(ctx: &egui::Context) -> Frame {
    let p = palette(ctx);
    Frame::new().fill(p.window).inner_margin(Margin::same(6))
}

/// Makes `ui`'s rows compact: check box height rather than control height,
/// for status bars and dense lists, where Qt's rows are text-high too.
pub fn compact(ui: &mut Ui) {
    let row = palette(ui.ctx()).metrics.row_height;
    ui.spacing_mut().interact_size.y = row;
}

/// The frame for a status bar: window colour, small vertical margins.
pub fn status_bar_frame(ctx: &egui::Context) -> Frame {
    let p = palette(ctx);
    Frame::new().fill(p.window).inner_margin(Margin::symmetric(6, 3))
}

/// The frame for a content view (a list, table or the 3D view's
/// surroundings): the view colour inside a frame outline, like Breeze's
/// `drawFramePrimitive`.
pub fn view_frame(ctx: &egui::Context) -> Frame {
    let p = palette(ctx);
    Frame::new()
        .fill(p.view)
        .stroke(Stroke::new(1.0, p.frame_outline))
        .corner_radius(CornerRadius::same(p.metrics.frame_radius.round() as u8))
        .inner_margin(Margin::same(2))
}

/// A flat tool bar button showing a theme icon, like a KDE tool bar's
/// auto-raise buttons: 22 px icon in a 34 px square, an outline on hover,
/// a highlight tint when pressed or `checked`.  Falls back to `fallback`
/// text when the icon isn't found.
pub fn tool_button(ui: &mut Ui, icon_name: &str, fallback: &str, checked: bool) -> Response {
    let p = palette(ui.ctx());
    let size = Vec2::splat(34.0);
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    if ui.is_rect_visible(rect) {
        let radius = CornerRadius::same(p.metrics.frame_radius.round() as u8);
        let pressed = response.is_pointer_button_down_on();
        // Helper::renderButtonFrame for flat buttons.
        let fill = if pressed || checked { p.highlight.gamma_multiply(0.3) } else { Color32::TRANSPARENT };
        let outline = if response.hovered() || pressed {
            Stroke::new(1.0, p.highlight)
        } else if checked {
            Stroke::new(1.0, p.button_outline)
        } else {
            Stroke::NONE
        };
        ui.painter().rect(rect, radius, fill, outline, egui::StrokeKind::Inside);
        let icon_rect = Rect::from_center_size(rect.center(), Vec2::splat(22.0));
        match icon(icon_name, 22) {
            Some(source) => {
                Image::new(source).fit_to_exact_size(icon_rect.size()).paint_at(ui, icon_rect);
            }
            None => {
                ui.painter().text(
                    rect.center(),
                    egui::Align2::CENTER_CENTER,
                    fallback,
                    egui::TextStyle::Button.resolve(ui.style()),
                    p.button_text,
                );
            }
        }
    }
    response
}

/// A separator between groups of tool bar buttons, Breeze's 8 px gap with a
/// one pixel line.
pub fn tool_separator(ui: &mut Ui) {
    let p = palette(ui.ctx());
    let (rect, _) = ui.allocate_exact_size(vec2(8.0, 22.0), Sense::hover());
    let x = rect.center().x.round() + 0.5;
    ui.painter().vline(x, rect.y_range(), Stroke::new(1.0, p.separator));
}

/// A single-line text field with the desktop's field padding (Breeze's
/// line edits are 32 px tall with text 8 px in).  egui's own default
/// padding is much tighter and can't be changed globally.
pub fn text_field<'t>(ctx: &egui::Context, text: &'t mut dyn egui::TextBuffer) -> egui::TextEdit<'t> {
    let p = palette(ctx);
    let [x, y] = p.metrics.field_padding;
    egui::TextEdit::singleline(text).margin(Margin::symmetric(x.round() as i8, y.round() as i8))
}

/// Shows an egui slider drawn like Breeze's (`drawSliderComplexControl`):
/// a grey groove filled with the highlight up to the handle, and a handle
/// in the button colour inside a faint outline that turns to the hover
/// colour on hover and the focus colour while dragged or focused.
///
/// egui colours the handle's outline like button text and its resting fill
/// like the groove, so a plain `ui.add(slider)` gets a bright outline and a
/// handle that blends into its groove.  This scopes a different outline
/// colour (pinning the value's text colour so it stays readable) and
/// repaints the groove of a horizontal slider: it reserves a shape slot
/// before the slider and replaces the next shape, which in egui 0.35 is
/// always the groove.
///
/// It also gives the slider Qt's keyboard: clicking or dragging it focuses
/// it, and while it has the focus all four arrows step it (egui's own
/// slider takes only the arrows along it), Page Up and Page Down take a
/// tenth of its length at a time, and Home and End go to its ends.  egui's
/// slider hides its range, so these keys reach it as the arrow presses
/// that move its handle as far.
pub fn slider(ui: &mut Ui, slider: egui::Slider<'_>) -> Response {
    let p = palette(ui.ctx());
    let m = &p.metrics;
    // The slider as it was last frame, found by where it is called from.
    let key = ui.next_auto_id().with("slerp-slider");
    let last: Option<(egui::Id, f32)> = ui.data(|d| d.get_temp(key));
    if let Some((id, length)) = last {
        slider_keys(ui, id, length);
    }
    // 1. A row of control height with its items centred, so the slider lines
    //    up with labels beside it the way other controls do.
    let size = vec2(ui.available_width(), m.control_height);
    let layout = egui::Layout::left_to_right(egui::Align::Center);
    let response = ui
        .allocate_ui_with_layout(size, layout, |ui| {
            // 2. egui sizes the handle from the slider's row height (its
            //    radius is two fifths of it), so a compact row gives Breeze's
            //    18 px handle; the value box shrinks to the same height so the
            //    two stay centred on each other.
            let compact = ((m.slider_handle - 2.0) * 1.25).round();
            let text_height = ui.text_style_height(&egui::TextStyle::Body);
            let spacing = ui.spacing_mut();
            spacing.interact_size.y = compact;
            spacing.button_padding.y = ((compact - text_height) / 2.0).max(0.0);
            let v = ui.visuals_mut();
            v.override_text_color = Some(p.window_text);
            v.widgets.inactive.fg_stroke = Stroke::new(1.0, p.button_outline);
            v.widgets.hovered.fg_stroke = Stroke::new(1.0, p.hover);
            v.widgets.active.fg_stroke = Stroke::new(1.0, p.focus);
            // 3. Reserve a shape slot; the slider's first shape, its groove,
            //    is replaced below.
            let reserved = ui.painter().add(Shape::Noop);
            let response = ui.add(slider);
            let rect = response.rect;
            if rect.width() > rect.height() && ui.is_rect_visible(rect) {
                // The groove spans the slider's own rect, the first part of
                // the row, centred in the row's height like egui's.
                let height = ui.spacing().slider_rail_height;
                let width = ui.spacing().slider_width.min(rect.width());
                let thickness = text_height.max(compact);
                let center_y = rect.top() + thickness / 2.0;
                let groove = Rect::from_min_size(pos2(rect.left(), center_y - height / 2.0), vec2(width, height));
                let radius = CornerRadius::same((height / 2.0).round() as u8);
                let outline = Stroke::new(1.0, p.window_text.gamma_multiply(0.2));
                let shape = egui::epaint::RectShape::new(groove, radius, p.groove, outline, egui::StrokeKind::Inside);
                ui.painter().set(egui::layers::ShapeIdx(reserved.0 + 1), shape);
            }
            response
        })
        .inner;
    // 4. The focus: a press on the slider takes it.  The value box beside
    //    it takes the focus by itself, for typing.
    let length = ui.spacing().slider_width.min(response.rect.width());
    // A quick click can press and release within one frame, which a slider
    // (it senses drags, not clicks) doesn't notice; it focuses it too.
    let at = response.interact_pointer_pos().or_else(|| {
        let clicked = ui.input(|i| i.pointer.any_click());
        (clicked && response.hovered()).then(|| ui.input(|i| i.pointer.interact_pos())).flatten()
    });
    let on_rail = at.is_some_and(|p| p.x < response.rect.left() + length);
    if on_rail && !response.has_focus() {
        response.request_focus();
    }
    ui.data_mut(|d| d.insert_temp(key, (response.id, length)));
    response
}

/// Turns the keys meant for the focused slider `id`, `length` points long,
/// into the presses of Left and Right that egui's slider reads: each moves
/// its handle a point, or one step of a stepped slider.
fn slider_keys(ui: &Ui, id: egui::Id, length: f32) {
    if !ui.memory(|m| m.has_focus(id)) {
        return;
    }
    keys::lock_arrows(ui, id);
    // Home and End press enough to reach the end of any slider whose steps
    // are no finer than its points.
    let page = (length / 10.0).ceil().max(1.0) as usize;
    let end = 4096;
    let mut presses: Vec<(egui::Key, usize)> = Vec::new();
    let mut vertical = false;
    ui.input_mut(|i| {
        i.events.retain(|e| {
            let egui::Event::Key { key, pressed: true, modifiers, .. } = e else { return true };
            if !modifiers.is_none() {
                return true;
            }
            let press = match key {
                egui::Key::ArrowUp => (egui::Key::ArrowRight, 1),
                egui::Key::ArrowDown => (egui::Key::ArrowLeft, 1),
                egui::Key::PageUp => (egui::Key::ArrowRight, page),
                egui::Key::PageDown => (egui::Key::ArrowLeft, page),
                egui::Key::Home => (egui::Key::ArrowLeft, end),
                egui::Key::End => (egui::Key::ArrowRight, end),
                _ => return true,
            };
            vertical |= matches!(key, egui::Key::ArrowUp | egui::Key::ArrowDown);
            presses.push(press);
            false
        });
        for (key, n) in presses {
            let press = egui::Event::Key {
                key,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            };
            i.events.extend(std::iter::repeat_n(press, n));
        }
    });
    if vertical {
        ui.memory_mut(|m| m.move_focus(egui::FocusDirection::None));
    }
}

/// A table header cell like Breeze's (`drawHeaderSectionControl`): the
/// button colour with a faint line below and to the right, text centred.
/// For `egui_extras::TableBuilder::header`'s cells.
pub fn header_cell(ui: &mut Ui, text: &str) -> Response {
    let p = palette(ui.ctx());
    let rect = ui.max_rect();
    let line = Stroke::new(1.0, p.window_text.gamma_multiply(0.2));
    let response = ui.allocate_rect(rect, Sense::hover());
    let fill = if response.hovered() { mix(p.button, p.hover, 0.2) } else { p.button };
    let painter = ui.painter();
    painter.rect_filled(rect, 0.0, fill);
    painter.hline(rect.x_range(), rect.bottom() - 0.5, line);
    painter.vline(rect.right() - 0.5, rect.y_range(), line);
    painter.text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        text,
        egui::TextStyle::Body.resolve(ui.style()),
        p.button_text,
    );
    response
}

/// An icon from the theme as an `Image` of exactly `size` points, ready to
/// add to a `Ui` or put on a `Button`.
pub fn icon_image(name: &str, size: u32) -> Option<Image<'static>> {
    let source: ImageSource<'static> = icon(name, size)?;
    Some(Image::new(source).fit_to_exact_size(Vec2::splat(size as f32)))
}

/// A check box drawn like Breeze's (`Helper::renderCheckBox`): the box
/// fills with the highlight tint and takes a highlight outline when
/// checked, which egui's own check box can't show.
pub struct CheckBox<'a> {
    checked: &'a mut bool,
    text: egui::WidgetText,
}

impl<'a> CheckBox<'a> {
    pub fn new(checked: &'a mut bool, text: impl Into<egui::WidgetText>) -> Self {
        CheckBox { checked, text: text.into() }
    }
}

impl Widget for CheckBox<'_> {
    fn ui(self, ui: &mut Ui) -> Response {
        let checked = *self.checked;
        let mut response = indicator(ui, self.text, checked, false, true);
        keys::focus_on_click(&response);
        if response.clicked() {
            *self.checked = !checked;
            response.mark_changed();
        }
        response
    }
}

/// A radio button drawn like Breeze's (`Helper::renderRadioButton`): the
/// selected one is outlined in the highlight with a tinted fill.  For a
/// group of them, [`radio_group`] adds the keyboard.
pub struct RadioButton {
    selected: bool,
    text: egui::WidgetText,
}

impl RadioButton {
    pub fn new(selected: bool, text: impl Into<egui::WidgetText>) -> Self {
        RadioButton { selected, text: text.into() }
    }
}

impl Widget for RadioButton {
    fn ui(self, ui: &mut Ui) -> Response {
        let response = indicator(ui, self.text, self.selected, true, true);
        keys::focus_on_click(&response);
        response
    }
}

/// `ui.radio_value` with [`RadioButton`].  Prefer [`radio_group`] for a
/// group, which the arrow keys move through.
pub fn radio_value<V: PartialEq>(
    ui: &mut Ui,
    current: &mut V,
    alternative: V,
    text: impl Into<egui::WidgetText>,
) -> Response {
    let mut response = ui.add(RadioButton::new(*current == alternative, text));
    if response.clicked() && *current != alternative {
        *current = alternative;
        response.mark_changed();
    }
    response
}

/// Lays out and paints a check box or radio button with its label.  A
/// control that isn't a `tab_stop` can still be clicked and focused, but
/// Tab passes it by (the unselected buttons of a radio group).
fn indicator(ui: &mut Ui, text: egui::WidgetText, checked: bool, radio: bool, tab_stop: bool) -> Response {
    let p = palette(ui.ctx());
    let spacing = ui.spacing();
    let (box_size, gap) = (spacing.icon_width, spacing.icon_spacing);
    let wrap_width = ui.available_width() - box_size - gap;
    let galley = text.into_galley(ui, Some(egui::TextWrapMode::Extend), wrap_width, egui::TextStyle::Body);
    let height = galley.size().y.max(p.metrics.row_height);
    let size = vec2(box_size + gap + galley.size().x, height);
    let sense = if tab_stop { Sense::click() } else { Sense::CLICK };
    let (rect, response) = ui.allocate_exact_size(size, sense);
    let kind = if radio { egui::WidgetType::RadioButton } else { egui::WidgetType::Checkbox };
    response.widget_info(|| egui::WidgetInfo::selected(kind, ui.is_enabled(), checked, galley.text()));
    if ui.is_rect_visible(rect) {
        let box_rect =
            Rect::from_center_size(pos2(rect.left() + box_size / 2.0, rect.center().y), Vec2::splat(box_size));
        // Helper::renderCheckBoxBackground: the button colour, darkened
        // by QColor::darker(110) while pressed, tinted with the highlight
        // when checked, and outlined in the separator colour or highlight.
        let pressed = response.is_pointer_button_down_on();
        let fill = if checked { p.checked_fill } else { p.button };
        let fill = if pressed { darker(fill, 1.1) } else { fill };
        let outline = Stroke::new(1.0, if checked { p.highlight } else { p.separator });
        let radius = if radio {
            CornerRadius::same(u8::MAX)
        } else {
            CornerRadius::same((p.metrics.frame_radius - 1.0).max(0.0) as u8)
        };
        let painter = ui.painter();
        painter.rect(box_rect, radius, fill, outline, egui::StrokeKind::Inside);
        // Hovering adds an inner ring in the focus colour.
        if response.hovered() {
            painter.rect_stroke(box_rect.shrink(1.0), radius, Stroke::new(1.0, p.focus), egui::StrokeKind::Inside);
        }
        if checked && radio {
            // Helper::renderRadioButton: a dot 6 px across in the text colour.
            painter.circle_filled(box_rect.center(), box_rect.width() * 3.0 / 16.0, p.button_text);
        } else if checked {
            // The tick of Helper::renderCheckBox, scaled to the box.
            let r = box_rect;
            let s = r.width() / 16.0;
            let pts = vec![
                pos2(r.left() + 4.0 * s, r.center().y),
                pos2(r.center().x - 1.0 * s, r.bottom() - 5.0 * s),
                pos2(r.center().x + 4.5 * s, r.top() + 5.5 * s),
            ];
            painter.add(Shape::line(pts, Stroke::new(2.0, p.button_text)));
        }
        let text_pos = pos2(box_rect.right() + gap, rect.center().y - galley.size().y / 2.0);
        let text_rect = Rect::from_min_size(text_pos, galley.size());
        painter.galley(text_pos, galley, p.window_text);
        // Breeze marks a focused check box or radio button with a line
        // under its label.
        if response.has_focus() {
            keys::paint_focus_line(ui, text_rect);
        }
    }
    response
}

fn darker(c: Color32, factor: f32) -> Color32 {
    let [r, g, b, a] = c.to_srgba_unmultiplied();
    let d = |v: u8| (v as f32 / factor).round() as u8;
    Color32::from_rgba_unmultiplied(d(r), d(g), d(b), a)
}

fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    let to = |c: Color32| {
        let [r, g, b, a] = c.to_srgba_unmultiplied();
        crate::Rgba::rgba8(r, g, b, a)
    };
    to(a).mix(to(b), t).to_color32()
}
