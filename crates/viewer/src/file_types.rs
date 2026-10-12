//! Settings > File Types: which formats get Slerp's thumbnails in the file
//! manager and open in Slerp by default.  Formats are grouped by category
//! with a switch for each row and each category, a search box, what makes
//! each format's thumbnails and opens it on this system now, and the notes
//! the association manager has for it (KDE's own plugins winning in
//! Dolphin, Windows' protected defaults).  Changes take effect with Apply,
//! through `slerp_associate`, as `slerp associate` does them.

use slerp_associate::{Context, Report, Status};
use slerp_core::choices::Choices;
use slerp_core::formats::{Category, FORMATS, Format};
use slerp_theme::widgets;

pub struct FileTypes {
    ctx: Option<Context>,
    /// The choices as saved.
    saved: Choices,
    /// The choices being edited.
    edit: Choices,
    /// What handles each format now, read when the page opens and after
    /// each Apply.
    statuses: Vec<Status>,
    pub search: String,
    /// The outcome of the last Apply or Undo, and whether it failed.
    message: Option<(String, bool)>,
    /// Puts the keyboard in the search box on the first frame.
    focus_search: bool,
}

impl FileTypes {
    pub fn open() -> FileTypes {
        let ctx = Context::system();
        let saved = ctx.as_ref().map(Context::load_choices).unwrap_or_default();
        let statuses = ctx.as_ref().map(|c| slerp_associate::status(c, &saved)).unwrap_or_default();
        let message = ctx.as_ref().err().map(|e| (format!("Can't change file types here: {e}"), true));
        FileTypes {
            ctx: ctx.ok(),
            edit: saved.clone(),
            saved,
            statuses,
            search: String::new(),
            message,
            focus_search: true,
        }
    }

    fn status(&self, f: &Format) -> Option<&Status> {
        self.statuses.iter().find(|s| s.format.id == f.id)
    }

    fn matches(&self, f: &Format) -> bool {
        let q = self.search.trim().to_lowercase();
        if q.is_empty() {
            return true;
        }
        let q = q.trim_start_matches('.');
        f.name.to_lowercase().contains(q)
            || f.id.contains(q)
            || f.category.name().to_lowercase().contains(q)
            || f.extensions().iter().any(|e| e.contains(q))
            || f.mime_names().any(|m| m.contains(q))
    }

    /// Draws the page as a window.  Returns false once it is closed.
    pub fn show(&mut self, ctx: &egui::Context) -> bool {
        let mut open = true;
        let mut close = false;
        let screen = ctx.content_rect().size();
        egui::Window::new("File Types")
            .open(&mut open)
            .collapsible(false)
            .default_size([(screen.x * 0.9).min(1280.0), (screen.y * 0.85).min(960.0)])
            .min_width(640.0)
            .show(ctx, |ui| close = self.ui(ui));
        open && !close
    }

    fn ui(&mut self, ui: &mut egui::Ui) -> bool {
        let p = slerp_theme::palette(ui.ctx());
        let weak = p.inactive_text;
        // 1. Search and what the switches mean.
        ui.horizontal(|ui| {
            ui.label("Search:");
            let field =
                ui.add(widgets::text_field(ui.ctx(), &mut self.search).hint_text("Name, extension or MIME type"));
            if std::mem::take(&mut self.focus_search) {
                field.request_focus();
            }
        });
        ui.label(
            egui::RichText::new(
                "Thumbnails: Slerp draws the file manager's previews of the format.  Open by default: Slerp opens \
                 the format when you double-click it (otherwise Slerp is only offered in \"Open with\").",
            )
            .color(weak),
        );
        for limit in slerp_associate::limits() {
            ui.label(egui::RichText::new(*limit).color(weak).small());
        }
        ui.add_space(4.0);

        // 2. The formats, by category.
        let footer = 2.0 * ui.spacing().interact_size.y + 24.0;
        let height = (ui.available_height() - footer).max(160.0);
        egui::ScrollArea::vertical().max_height(height).auto_shrink([false, false]).show(ui, |ui| {
            for category in Category::ALL {
                let formats: Vec<&'static Format> =
                    FORMATS.iter().filter(|f| f.category == category && self.matches(f)).collect();
                if !formats.is_empty() {
                    self.category_ui(ui, category, &formats);
                }
            }
        });

        // 3. The buttons and the last outcome.
        ui.separator();
        let mut close = false;
        ui.horizontal(|ui| {
            let changed = self.edit != self.saved;
            if ui.add_enabled(changed && self.ctx.is_some(), egui::Button::new("Apply")).clicked() {
                self.apply();
            }
            if ui
                .add_enabled(self.edit != Choices::default(), egui::Button::new("Restore Defaults"))
                .on_hover_text("Slerp's own choice for every format; Apply makes it take effect")
                .clicked()
            {
                self.edit = Choices::default();
            }
            if ui
                .add_enabled(self.ctx.is_some(), egui::Button::new("Undo Slerp's Changes"))
                .on_hover_text(
                    "Puts back everything Slerp changed outside its own settings, other apps' defaults \
                     included (slerp associate --reset)",
                )
                .clicked()
            {
                self.reset();
            }
            if ui.button("Close").clicked() {
                close = true;
            }
            if changed {
                ui.label(egui::RichText::new("Not applied yet").color(weak));
            }
        });
        if let Some((text, error)) = &self.message {
            let color = if *error { p.negative } else { weak };
            ui.label(egui::RichText::new(text).color(color));
        }
        close
    }

    fn category_ui(&mut self, ui: &mut egui::Ui, category: Category, formats: &[&'static Format]) {
        let p = slerp_theme::palette(ui.ctx());
        let weak = p.inactive_text;
        // 1. The category's own switches, on when every format is.
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(category.name()).strong());
            ui.add_space(12.0);
            let can: Vec<&&Format> = formats.iter().filter(|f| f.can_thumbnail()).collect();
            let mut all_thumbs = !can.is_empty() && can.iter().all(|f| self.edit.thumbnails(f));
            if !can.is_empty()
                && ui
                    .add(widgets::CheckBox::new(&mut all_thumbs, "Thumbnails"))
                    .on_hover_text(format!("Slerp's thumbnails for every format in {}", category.name()))
                    .changed()
            {
                for f in &can {
                    self.edit.set_thumbnails(f, all_thumbs);
                }
            }
            let mut all_default = formats.iter().all(|f| self.edit.open_by_default(f));
            if ui
                .add(widgets::CheckBox::new(&mut all_default, "Open by default"))
                .on_hover_text(format!("Slerp as the default app for every format in {}", category.name()))
                .changed()
            {
                for f in formats {
                    self.edit.set_open_by_default(f, all_default);
                }
            }
        });

        // 2. One row per format, in columns as wide in every category, so
        //    they line up down the page: the format and its extensions, the
        //    two switches, what makes its thumbnails and what opens it now.
        let row = ui.spacing().interact_size.y;
        let widths = [380.0, 130.0, 170.0, 250.0];
        for (i, f) in formats.iter().enumerate() {
            let status = self.status(f);
            let thumbs_by = status.and_then(|s| s.thumbnails_by.clone()).unwrap_or_else(|| "nothing".into());
            let opens_with = status.and_then(|s| s.opens_with.clone()).unwrap_or_else(|| "nothing".into());
            let notes: Vec<String> = status.map(|s| s.notes.clone()).unwrap_or_default();
            let rivals: Vec<String> = status.map(|s| s.kde_rivals.clone()).unwrap_or_default();
            let exts: Vec<String> = f
                .extensions()
                .iter()
                .chain(f.by_content.iter())
                .map(|e| format!(".{e}"))
                .chain(f.mime.iter().flat_map(|m| m.patterns.iter().map(|p| p.to_string())))
                .collect();
            let mut shown = exts.iter().take(5).cloned().collect::<Vec<_>>().join(" ");
            if exts.len() > 5 {
                shown.push_str(&format!(" +{}", exts.len() - 5));
            }
            let background = ui.painter().add(egui::Shape::Noop);
            let cell = |ui: &mut egui::Ui, width: f32, add: &mut dyn FnMut(&mut egui::Ui)| {
                ui.allocate_ui_with_layout(
                    egui::vec2(width, row),
                    egui::Layout::left_to_right(egui::Align::Center),
                    |ui| {
                        ui.set_min_width(width);
                        add(ui);
                    },
                );
            };
            let response = ui.horizontal(|ui| {
                cell(ui, widths[0], &mut |ui| {
                    ui.label(f.name).on_hover_text(f.mime_names().collect::<Vec<_>>().join("\n"));
                    ui.add(egui::Label::new(egui::RichText::new(&shown).color(weak).small()).truncate())
                        .on_hover_text(exts.join(" "));
                });
                cell(ui, widths[1], &mut |ui| {
                    let mut thumbs = self.edit.thumbnails(f);
                    let r = ui.add_enabled(f.can_thumbnail(), widgets::CheckBox::new(&mut thumbs, "Thumbnails"));
                    if r.changed() {
                        self.edit.set_thumbnails(f, thumbs);
                    }
                });
                cell(ui, widths[2], &mut |ui| {
                    let mut default = self.edit.open_by_default(f);
                    if ui.add(widgets::CheckBox::new(&mut default, "Open by default")).changed() {
                        self.edit.set_open_by_default(f, default);
                    }
                });
                cell(ui, widths[3], &mut |ui| {
                    ui.add(
                        egui::Label::new(egui::RichText::new(format!("Thumbnails by {thumbs_by}")).color(weak))
                            .truncate(),
                    );
                });
                ui.add(
                    egui::Label::new(egui::RichText::new(format!("Opens with {opens_with}")).color(weak)).truncate(),
                );
            });
            // Notes and the extra KDE choice, under the format.
            if !notes.is_empty() || (!rivals.is_empty() && self.edit.thumbnails(f)) {
                ui.indent(("notes", f.id), |ui| {
                    for n in &notes {
                        ui.label(egui::RichText::new(n).color(p.neutral).small());
                    }
                    if !rivals.is_empty() && self.edit.thumbnails(f) {
                        let mut replace = self.edit.replace_kde_previews.get(f.id).copied().unwrap_or(false);
                        let text =
                            format!("Use Slerp's thumbnails instead of KDE's {} in Dolphin", rivals.join(" and "));
                        if ui
                            .add(widgets::CheckBox::new(&mut replace, text))
                            .on_hover_text(
                                "Switches KDE's plugin off in Dolphin's preview settings: Dolphin then previews \
                                 none of the other types it handles either, unless Slerp's thumbnails are on for them",
                            )
                            .changed()
                        {
                            self.edit.set_replace_kde_previews(f, replace);
                        }
                    }
                });
            }
            if i % 2 == 1 {
                let rect = response.response.rect.expand2(egui::vec2(4.0, 1.0));
                ui.painter().set(background, egui::Shape::rect_filled(rect, 2.0, p.alternate_row));
            }
        }
    }

    fn apply(&mut self) {
        let Some(ctx) = &self.ctx else { return };
        // Windows lets only its Settings choose default apps: open its page
        // when a format newly asks for Slerp.
        #[cfg(windows)]
        let new_default = FORMATS.iter().any(|f| self.edit.open_by_default(f) && !self.saved.open_by_default(f));
        let saved = self.edit.save_to(&ctx.choices_path);
        let result = saved.map_err(|e| e.to_string()).and_then(|()| slerp_associate::apply(ctx, &mut self.edit, false));
        #[cfg(windows)]
        if new_default && result.is_ok() {
            slerp_associate::windows::open_default_apps_settings();
        }
        self.finish(result, "Applied");
        self.saved = self.edit.clone();
    }

    fn reset(&mut self) {
        let Some(ctx) = &self.ctx else { return };
        let result = slerp_associate::reset(ctx, false);
        self.finish(result, "Undone");
    }

    /// Reads the system again and says what happened.
    fn finish(&mut self, result: Result<Report, String>, verb: &str) {
        self.message = Some(match result {
            Ok(report) => {
                let mut text = match report.changes.len() {
                    0 => format!("{verb}: nothing needed changing."),
                    1 => format!("{verb}: 1 change."),
                    n => format!("{verb}: {n} changes."),
                };
                for n in report.notes {
                    text.push(' ');
                    text.push_str(&n);
                }
                (text, false)
            }
            Err(e) => (format!("Couldn't apply: {e}"), true),
        });
        if let Some(ctx) = &self.ctx {
            self.statuses = slerp_associate::status(ctx, &self.edit);
        }
    }
}
