//! Palette mode: every colour of a palette file as a swatch card with its
//! name, hex value and, for colours not stored as RGB, the numbers in the
//! file's own colour model (CMYK, Lab, HSB, grey), grouped as the file
//! groups them.  Clicking a card copies its hex value.  The background
//! switches between light and dark, since colours read differently on
//! each.
//!
//! Palettes run to thousands of colours (Pantone libraries), so the cards
//! are laid out by hand and only the visible rows are drawn.

use slerp_core::palette::{Palette, Swatch, SwatchKind};
use slerp_theme::widgets;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// A card's size and the gaps between cards, in points.
const CARD: egui::Vec2 = egui::vec2(232.0, 60.0);
const GAP: f32 = 10.0;
const MARGIN: f32 = 16.0;
const HEADER: f32 = 30.0;

/// How long the "Copied" note stays.
const NOTE: Duration = Duration::from_millis(1800);

pub struct PaletteView {
    pub palette: Arc<Palette>,
    /// Whether the view's background is dark.
    pub dark: bool,
    /// Only swatches whose name or hex contains this are shown.
    pub filter: String,
    copied: Option<(String, Instant)>,
}

impl PaletteView {
    pub fn new(palette: Arc<Palette>) -> PaletteView {
        PaletteView { palette, dark: false, filter: String::new(), copied: None }
    }

    /// Keeps the background of the palette shown before, so stepping
    /// through a folder compares palettes on the same ground.
    pub fn carry_over(&mut self, previous: &PaletteView) {
        self.dark = previous.dark;
    }

    pub fn summary(&self) -> String {
        let n = self.palette.len();
        if n == 1 { "1 colour".into() } else { format!("{n} colours") }
    }

    /// The filter as matched: lower case, without a leading `#`.
    fn needle(&self) -> String {
        self.filter.trim().trim_start_matches('#').to_lowercase()
    }
}

/// Whether a swatch matches the filter `needle` by name or hex value.
fn shown(s: &Swatch, needle: &str) -> bool {
    needle.is_empty() || s.name.to_lowercase().contains(needle) || s.hex().to_lowercase().contains(needle)
}

/// The colours drawn on the light or dark background.
struct Ink {
    background: egui::Color32,
    card: egui::Color32,
    hover: egui::Color32,
    text: egui::Color32,
    weak: egui::Color32,
    rim: egui::Color32,
}

fn ink(dark: bool) -> Ink {
    if dark {
        Ink {
            background: egui::Color32::from_gray(28),
            card: egui::Color32::from_gray(40),
            hover: egui::Color32::from_gray(58),
            text: egui::Color32::from_gray(235),
            weak: egui::Color32::from_gray(160),
            rim: egui::Color32::from_white_alpha(40),
        }
    } else {
        Ink {
            background: egui::Color32::from_gray(244),
            card: egui::Color32::WHITE,
            hover: egui::Color32::from_gray(228),
            text: egui::Color32::from_gray(20),
            weak: egui::Color32::from_gray(100),
            rim: egui::Color32::from_black_alpha(40),
        }
    }
}

/// One row of the layout: a group's heading, or cards (indices into
/// the group's swatches).
enum Row {
    Header(String),
    Cards(usize, Vec<usize>),
}

/// Draws the palette filling `ui`'s area.
pub fn view(ui: &mut egui::Ui, v: &mut PaletteView) {
    let rect = ui.max_rect();
    let ink = ink(v.dark);
    ui.painter().rect_filled(rect, 0.0, ink.background);
    // 1. Rows of cards as wide as the view allows.
    let cols = (((rect.width() - 2.0 * MARGIN + GAP) / (CARD.x + GAP)).floor() as usize).max(1);
    let palette = v.palette.clone();
    let mut rows = Vec::new();
    let needle = v.needle();
    for (gi, g) in palette.groups.iter().enumerate() {
        let visible: Vec<usize> = (0..g.swatches.len()).filter(|&i| shown(&g.swatches[i], &needle)).collect();
        if visible.is_empty() {
            continue;
        }
        if !g.name.is_empty() {
            rows.push(Row::Header(g.name.clone()));
        }
        rows.extend(visible.chunks(cols).map(|c| Row::Cards(gi, c.to_vec())));
    }
    let height = |r: &Row| match r {
        Row::Header(_) => HEADER,
        Row::Cards(..) => CARD.y + GAP,
    };
    let total: f32 = rows.iter().map(height).sum::<f32>() + 2.0 * MARGIN;
    // 2. Only the rows in sight are drawn.
    let mut copy = None;
    egui::ScrollArea::vertical().auto_shrink([false, false]).show_viewport(ui, |ui, viewport| {
        let (area, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), total), egui::Sense::hover());
        let mut y = MARGIN;
        for row in &rows {
            let h = height(row);
            if y + h >= viewport.min.y && y <= viewport.max.y {
                let top = area.min.y + y;
                match row {
                    Row::Header(name) => {
                        let font = egui::TextStyle::Heading.resolve(ui.style());
                        let pos = egui::pos2(area.min.x + MARGIN, top + HEADER - 8.0);
                        ui.painter().text(pos, egui::Align2::LEFT_BOTTOM, name, font, ink.text);
                    }
                    Row::Cards(gi, items) => {
                        for (c, &i) in items.iter().enumerate() {
                            let min = egui::pos2(area.min.x + MARGIN + c as f32 * (CARD.x + GAP), top);
                            let swatch = &palette.groups[*gi].swatches[i];
                            if card(ui, egui::Rect::from_min_size(min, CARD), swatch, (*gi, i), &ink) {
                                copy = Some(swatch.hex());
                            }
                        }
                    }
                }
            }
            y += h;
        }
        if rows.is_empty() {
            let pos = area.min + egui::vec2(MARGIN, MARGIN);
            let font = egui::TextStyle::Body.resolve(ui.style());
            ui.painter().text(pos, egui::Align2::LEFT_TOP, "No colour matches the filter.", font, ink.weak);
        }
    });
    // 3. Clicking copies, with a note that fades.
    if let Some(hex) = copy {
        ui.ctx().copy_text(hex.clone());
        v.copied = Some((hex, Instant::now()));
    }
    if let Some((hex, at)) = &v.copied {
        let age = at.elapsed();
        if age < NOTE {
            let alpha = (1.0 - (age.as_secs_f32() / NOTE.as_secs_f32()).powi(3)).clamp(0.0, 1.0);
            let font = egui::TextStyle::Body.resolve(ui.style());
            let painter = ui.painter_at(rect);
            let galley = painter.layout_no_wrap(format!("Copied {hex}"), font, egui::Color32::WHITE);
            let r = egui::Rect::from_center_size(
                egui::pos2(rect.center().x, rect.bottom() - 36.0),
                galley.size() + egui::vec2(24.0, 12.0),
            );
            painter.rect_filled(r, 6.0, egui::Color32::from_black_alpha((200.0 * alpha) as u8));
            painter.galley(r.min + egui::vec2(12.0, 6.0), galley, egui::Color32::WHITE.gamma_multiply(alpha));
            ui.ctx().request_repaint();
        } else {
            v.copied = None;
        }
    }
}

/// Draws one swatch card; true when it was clicked (or activated with the
/// keyboard).
fn card(ui: &mut egui::Ui, rect: egui::Rect, s: &Swatch, id: (usize, usize), ink: &Ink) -> bool {
    let hex = s.hex();
    let response = ui.interact(rect, ui.id().with(("swatch", id)), egui::Sense::click());
    let label = if s.name.is_empty() { hex.clone() } else { format!("{}, {hex}", s.name) };
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, label.clone()));
    let p = ui.painter();
    let fill = if response.hovered() || response.has_focus() { ink.hover } else { ink.card };
    p.rect_filled(rect, 6.0, fill);
    if response.has_focus() {
        p.rect_stroke(rect, 6.0, ui.visuals().selection.stroke, egui::StrokeKind::Inside);
    }
    // 1. The colour, over a checkerboard where it is transparent.
    let square = egui::Rect::from_min_size(rect.min + egui::vec2(6.0, 6.0), egui::vec2(CARD.y - 12.0, CARD.y - 12.0));
    let [r, g, b, a] = s.rgba8();
    if a < 255 {
        let n = 4;
        let cell = square.width() / n as f32;
        for y in 0..n {
            for x in 0..n {
                let c = if (x + y) % 2 == 0 { 255 } else { 200 };
                let r = egui::Rect::from_min_size(
                    square.min + egui::vec2(x as f32, y as f32) * cell,
                    egui::vec2(cell, cell),
                );
                p.rect_filled(r, 0.0, egui::Color32::from_gray(c));
            }
        }
    }
    p.rect_filled(square, 4.0, egui::Color32::from_rgba_unmultiplied(r, g, b, a));
    p.rect_stroke(square, 4.0, egui::Stroke::new(1.0, ink.rim), egui::StrokeKind::Inside);
    // 2. Name, hex and the original values.
    let x = square.right() + 10.0;
    let width = rect.right() - x - 6.0;
    let body = egui::TextStyle::Body.resolve(ui.style());
    let mono = egui::TextStyle::Monospace.resolve(ui.style());
    let small = egui::TextStyle::Small.resolve(ui.style());
    let mut lines: Vec<(String, egui::FontId, egui::Color32)> = Vec::new();
    if !s.name.is_empty() {
        lines.push((s.name.clone(), body, ink.text));
    }
    let kind = match s.kind {
        SwatchKind::Spot => "  spot",
        SwatchKind::Global => "  global",
        SwatchKind::Normal => "",
    };
    let alpha = if a < 255 { format!("  {}% opaque", (s.alpha * 100.0).round()) } else { String::new() };
    lines.push((format!("{hex}{kind}{alpha}"), mono, if s.name.is_empty() { ink.text } else { ink.weak }));
    if let Some(values) = s.values() {
        lines.push((values, small, ink.weak));
    }
    let galleys: Vec<_> = lines
        .into_iter()
        .map(|(text, font, color)| {
            let mut job = egui::text::LayoutJob::simple_singleline(text, font, color);
            job.wrap = egui::text::TextWrapping::truncate_at_width(width);
            (p.layout_job(job), color)
        })
        .collect();
    let height: f32 = galleys.iter().map(|(g, _)| g.size().y).sum::<f32>() + 2.0 * (galleys.len() as f32 - 1.0);
    let mut y = rect.center().y - height / 2.0;
    for (g, color) in galleys {
        let h = g.size().y;
        p.galley(egui::pos2(x, y), g, color);
        y += h + 2.0;
    }
    response.on_hover_text(format!("Click to copy {hex}")).clicked()
}

/// The right-hand panel: what the palette holds, the background and a
/// filter.
pub fn controls(ui: &mut egui::Ui, v: &mut PaletteView) {
    crate::sections::section(ui, "Palette", "Palette", true, |ui| {
        let p = &v.palette;
        if !p.name.is_empty() {
            ui.label(egui::RichText::new(&p.name).strong());
        }
        let groups = p.groups.iter().filter(|g| !g.name.is_empty()).count();
        let mut text = v.summary();
        if groups > 0 {
            text += &format!(" in {groups} group{}", if groups == 1 { "" } else { "s" });
        }
        ui.label(text);
        ui.add_space(4.0);
        egui::Grid::new("palette-form").num_columns(2).spacing([8.0, 6.0]).show(ui, |ui| {
            ui.label("Background:");
            widgets::radio_group(ui, &mut v.dark, &[(false, "Light"), (true, "Dark")]);
            ui.end_row();
            ui.label("Find:");
            ui.add(widgets::text_field(ui.ctx(), &mut v.filter).hint_text("Name or hex").desired_width(160.0));
            ui.end_row();
        });
        ui.label(egui::RichText::new("Click a colour to copy its hex value.").weak().small());
        if p.swatches()
            .any(|s| matches!(s.color, slerp_core::palette::Color::Cmyk(_) | slerp_core::palette::Color::Lab(_)))
        {
            ui.label(
                egui::RichText::new(
                    "CMYK and Lab colours are converted without colour profiles, so they are approximate.",
                )
                .weak()
                .small(),
            );
        }
    });
}

/// A picture of the swatches for the window's screenshot command.
pub fn screenshot(v: &PaletteView, width: u32, height: u32) -> slerp_core::scene::Image8 {
    let side = width.min(height).max(8);
    slerp_core::palette::thumbnail(&v.palette, side)
}

#[cfg(test)]
mod tests {
    use super::*;
    use slerp_core::palette::{Color, Group, PaletteFormat};

    fn palette() -> Arc<Palette> {
        let mut p = slerp_core::palette::Palette {
            format: PaletteFormat::Gpl,
            name: "Test".into(),
            columns: None,
            groups: vec![Group::default()],
            warnings: Vec::new(),
        };
        p.groups[0].swatches.push(Swatch::new("Red", Color::Rgb([1.0, 0.0, 0.0])));
        p.groups[0].swatches.push(Swatch::new("", Color::Cmyk([0.0, 1.0, 0.0, 0.0])));
        Arc::new(p)
    }

    #[test]
    fn filters_by_name_and_hex() {
        let mut v = PaletteView::new(palette());
        let s: Vec<Swatch> = v.palette.swatches().cloned().collect();
        let matches = |v: &PaletteView| (shown(&s[0], &v.needle()), shown(&s[1], &v.needle()));
        v.filter = "red".into();
        assert_eq!(matches(&v), (true, false));
        v.filter = "#ff00".into();
        assert_eq!(matches(&v), (true, true));
        v.filter = "00FF".into();
        assert_eq!(matches(&v), (false, true));
        assert_eq!(v.summary(), "2 colours");
    }

    /// A click on a card asks the window to copy its hex value (through
    /// egui's output, so the test never touches the real clipboard), and a
    /// click outside the cards copies nothing.
    #[test]
    fn clicking_a_swatch_copies_its_hex() {
        let mut v = PaletteView::new(palette());
        let ctx = egui::Context::default();
        let frame = |v: &mut PaletteView, events: Vec<egui::Event>| {
            ctx.run_ui(egui::RawInput { events, ..Default::default() }, |ui| view(ui, v))
        };
        let click = |at: egui::Pos2| {
            let button = |pressed| egui::Event::PointerButton {
                pos: at,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::NONE,
            };
            vec![button(true), button(false)]
        };
        let copies = |out: &egui::FullOutput| -> Vec<String> {
            out.platform_output
                .commands
                .iter()
                .filter_map(|c| match c {
                    egui::OutputCommand::CopyText(t) => Some(t.clone()),
                    _ => None,
                })
                .collect()
        };
        // 1. A frame to lay the cards out, then the pointer over the first.
        //    The scroll area starts a few points in, so the first card's
        //    left edge is found by clicking along the row.
        frame(&mut v, vec![]);
        let y = MARGIN + 30.0;
        let click_at = |v: &mut PaletteView, x: f32| {
            let at = egui::pos2(x, y);
            frame(v, vec![egui::Event::PointerMoved(at)]);
            copies(&frame(v, click(at)))
        };
        let left = (0..100).find(|&x| !click_at(&mut v, x as f32).is_empty()).expect("a card on the first row") as f32;
        assert!((MARGIN - 8.0..MARGIN + 8.0).contains(&left), "{left}");
        assert_eq!(click_at(&mut v, MARGIN + 40.0), ["#FF0000"]);
        assert!(v.copied.is_some());
        // 2. The second card is the CMYK magenta, one card pitch to the right.
        assert_eq!(click_at(&mut v, MARGIN + CARD.x + GAP + 40.0), ["#FF00FF"]);
        // 3. Clicks well outside the cards copy nothing: left of the first,
        //    and past the second (the row holds two).
        assert!(click_at(&mut v, 1.0).is_empty());
        assert!(click_at(&mut v, MARGIN + 2.0 * (CARD.x + GAP) + 40.0).is_empty());
    }

    #[test]
    fn draws_without_panicking() {
        let mut v = PaletteView::new(palette());
        let ctx = egui::Context::default();
        let _ = ctx.run_ui(egui::RawInput::default(), |ui| view(ui, &mut v));
        v.dark = true;
        v.filter = "nothing".into();
        let _ = ctx.run_ui(egui::RawInput::default(), |ui| view(ui, &mut v));
    }
}
