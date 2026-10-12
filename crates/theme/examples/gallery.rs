//! A viewer-like window for comparing the theme with real KDE apps: menu
//! bar, tool bar, a side panel of collapsing sections with sliders, combo
//! boxes and check boxes, a text field, a table and a status bar.
//!
//! ```sh
//! cargo run -p slerp-theme --example gallery -- [--app-id ID] [--plain]
//! ```
//!
//! `--plain` shows egui's own style instead, for before-and-after pictures.
//! `GALLERY_NO_DECORATIONS=1` drops the window frame, which compositors
//! without server-side decorations (such as `gui-sandbox`'s cage) otherwise
//! leave a gap for.
//! Set `XDG_CONFIG_HOME` to a folder with another `kdeglobals` to try other
//! colour schemes; `SLERP_THEME=gnome` forces the GNOME look.

use eframe::egui;
use egui::{Align, CollapsingHeader, ComboBox, Layout, RichText, Slider, TextEdit};
use egui_extras::{Column, TableBuilder};
use slerp_theme::widgets;

fn main() -> eframe::Result {
    let args: Vec<String> = std::env::args().collect();
    let app_id = args.iter().position(|a| a == "--app-id").and_then(|i| args.get(i + 1)).cloned();
    let plain = args.iter().any(|a| a == "--plain");
    let options = eframe::NativeOptions {
        renderer: eframe::Renderer::Glow,
        viewport: egui::ViewportBuilder::default()
            .with_title("Slerp theme gallery")
            .with_app_id(app_id.unwrap_or_else(|| "slerp".into()))
            .with_inner_size([1280.0, 720.0])
            .with_decorations(std::env::var_os("GALLERY_NO_DECORATIONS").is_none()),
        ..Default::default()
    };
    eframe::run_native(
        "Slerp theme gallery",
        options,
        Box::new(move |cc| {
            let start = std::time::Instant::now();
            let theme = slerp_theme::detect();
            let detect_ms = start.elapsed().as_secs_f64() * 1000.0;
            if !plain {
                slerp_theme::apply(&cc.egui_ctx, &theme);
            }
            eprintln!("detect {detect_ms:.1} ms, theme {:?} {:?}", theme.desktop, theme.colors.name);
            let watcher = slerp_theme::ThemeWatcher::new(&cc.egui_ctx, theme);
            Ok(Box::new(Gallery::new(watcher, plain)))
        }),
    )
}

struct Gallery {
    watcher: slerp_theme::ThemeWatcher,
    plain: bool,
    camera: usize,
    fov: f32,
    orthographic: bool,
    grid: bool,
    wireframe: bool,
    lighting: usize,
    exposure: f32,
    tone_map: usize,
    playing: bool,
    time: f32,
    speed: f32,
    looping: bool,
    morphs: [f32; 4],
    filter: String,
    selected_clip: usize,
}

impl Gallery {
    fn new(watcher: slerp_theme::ThemeWatcher, plain: bool) -> Self {
        Gallery {
            watcher,
            plain,
            camera: 0,
            fov: 50.0,
            orthographic: false,
            grid: true,
            wireframe: false,
            lighting: 1,
            exposure: 0.5,
            tone_map: 0,
            playing: false,
            time: 1.2,
            speed: 1.0,
            looping: true,
            morphs: [0.0, 0.35, 0.8, 0.1],
            filter: String::new(),
            selected_clip: 1,
        }
    }
}

const CAMERAS: [&str; 3] = ["Orbit", "First person", "2D pan and zoom"];
const LIGHTING: [&str; 4] = ["Studio", "Sunset sky", "Overcast", "Custom HDR…"];
const CLIPS: [(&str, &str, &str); 6] = [
    ("Idle", "2.40 s", "72"),
    ("Walk", "1.10 s", "33"),
    ("Run", "0.73 s", "22"),
    ("Jump", "1.50 s", "45"),
    ("Wave", "3.20 s", "96"),
    ("Sit down", "2.00 s", "60"),
];

impl eframe::App for Gallery {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        if !self.plain {
            self.watcher.poll(&ctx);
        }
        let plain = self.plain;
        let (menu_frame, tool_frame) = if plain {
            (egui::Frame::side_top_panel(ui.style()), egui::Frame::side_top_panel(ui.style()))
        } else {
            (widgets::menu_bar_frame(&ctx), widgets::tool_bar_frame(&ctx))
        };

        egui::Panel::top("menu").frame(menu_frame).show_separator_line(false).show(ui, |ui| {
            let bar = if plain { egui::MenuBar::new() } else { widgets::menu_bar(&ctx) };
            bar.ui(ui, |ui| {
                menu(ui, "File", |ui| {
                    let _ = ui.button("Open…");
                    let _ = ui.button("Open Recent");
                    ui.separator();
                    let _ = ui.button("Save Screenshot…");
                    ui.separator();
                    let _ = ui.button("Quit");
                });
                menu(ui, "View", |ui| {
                    let _ = ui.checkbox(&mut self.grid, "Show Grid");
                    let _ = ui.checkbox(&mut self.wireframe, "Wireframe");
                    let _ = ui.button("Full Screen Mode");
                });
                menu(ui, "Animation", |ui| {
                    let _ = ui.button("Play");
                    let _ = ui.button("Add Animations from File…");
                });
                menu(ui, "Help", |ui| {
                    let _ = ui.button("About Slerp");
                });
            });
        });
        egui::Panel::top("toolbar").frame(tool_frame).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                let tool = |ui: &mut egui::Ui, name: &str, fallback: &str, tip: &str| {
                    if plain {
                        ui.button(fallback).on_hover_text(tip);
                    } else {
                        widgets::tool_button(ui, name, fallback, false).on_hover_text(tip);
                    }
                };
                tool(ui, "document-open", "Open", "Open a file (Ctrl+O)");
                tool(ui, "camera-photo", "Shot", "Save a screenshot");
                if !plain {
                    widgets::tool_separator(ui);
                }
                tool(ui, "go-previous", "<", "Previous file in folder (Page Up)");
                tool(ui, "go-next", ">", "Next file in folder (Page Down)");
                if !plain {
                    widgets::tool_separator(ui);
                }
                tool(ui, "zoom-in", "+", "Zoom in");
                tool(ui, "zoom-out", "-", "Zoom out");
                tool(ui, "zoom-fit-best", "Fit", "Frame the model (F)");
                if !plain {
                    widgets::tool_separator(ui);
                }
                tool(ui, "view-grid", "Grid", "Show the ground grid");
                tool(ui, "view-fullscreen", "Full", "Full screen (F11)");
            });
        });

        let status = if plain { egui::Frame::side_top_panel(ui.style()) } else { widgets::status_bar_frame(&ctx) };
        egui::Panel::bottom("status").frame(status).show_separator_line(false).show(ui, |ui| {
            if !plain {
                widgets::compact(ui);
            }
            ui.horizontal(|ui| {
                ui.label("chair.glb");
                ui.separator();
                ui.label("12,345 triangles");
                ui.separator();
                ui.label("4 materials, 9 textures (21.3 MB)");
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.label(RichText::new("Vulkan · RTX 5070 Ti").small().weak());
                });
            });
        });

        let side = if plain { egui::Frame::side_top_panel(ui.style()) } else { widgets::side_panel_frame(&ctx) };
        egui::Panel::left("side").frame(side).resizable(true).default_size(300.0).show(ui, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| self.side_panel(ui));
        });

        egui::CentralPanel::default().show(ui, |ui| self.central(ui));
    }
}

impl Gallery {
    fn collapsing(&self, title: &str) -> CollapsingHeader {
        let header = CollapsingHeader::new(title).default_open(true);
        if self.plain { header } else { header.icon(widgets::collapsing_icon) }
    }

    fn combo<'a>(&self, ctx: &egui::Context, id: &'a str, text: &'a str) -> ComboBox {
        if self.plain {
            ComboBox::from_id_salt(id).selected_text(text).width(170.0)
        } else {
            widgets::combo_box(ctx, id, text).width(170.0)
        }
    }

    fn radio(&self, ui: &mut egui::Ui, value: &mut usize, alternative: usize, text: &str) {
        if self.plain {
            ui.radio_value(value, alternative, text);
        } else {
            widgets::radio_value(ui, value, alternative, text);
        }
    }

    fn side_panel(&mut self, ui: &mut egui::Ui) {
        let plain = self.plain;
        self.collapsing("Camera").show(ui, |ui| {
            egui::Grid::new("camera").num_columns(2).spacing([8.0, 6.0]).show(ui, |ui| {
                ui.label("Mode:");
                self.combo(&ui.ctx().clone(), "camera", CAMERAS[self.camera]).show_ui(ui, |ui| {
                    for (i, name) in CAMERAS.iter().enumerate() {
                        ui.selectable_value(&mut self.camera, i, *name);
                    }
                });
                ui.end_row();
                ui.label("Field of view:");
                let slider = Slider::new(&mut self.fov, 10.0..=120.0).suffix("°").integer();
                add_slider(plain, ui, slider);
                ui.end_row();
            });
            check(plain, ui, &mut self.orthographic, "Orthographic projection");
            check(plain, ui, &mut self.grid, "Show ground grid");
            check(plain, ui, &mut self.wireframe, "Wireframe overlay");
        });
        self.collapsing("Lighting").show(ui, |ui| {
            egui::Grid::new("lighting").num_columns(2).spacing([8.0, 6.0]).show(ui, |ui| {
                ui.label("Preset:");
                self.combo(&ui.ctx().clone(), "lighting", LIGHTING[self.lighting]).show_ui(ui, |ui| {
                    for (i, name) in LIGHTING.iter().enumerate() {
                        ui.selectable_value(&mut self.lighting, i, *name);
                    }
                });
                ui.end_row();
                ui.label("Exposure:");
                let slider = Slider::new(&mut self.exposure, -4.0..=4.0).fixed_decimals(1);
                add_slider(plain, ui, slider);
                ui.end_row();
            });
            ui.horizontal(|ui| {
                let mut tone_map = self.tone_map;
                self.radio(ui, &mut tone_map, 0, "ACES");
                self.radio(ui, &mut tone_map, 1, "AgX");
                self.radio(ui, &mut tone_map, 2, "None");
                self.tone_map = tone_map;
            });
        });
        self.collapsing("Animation").show(ui, |ui| {
            ui.horizontal(|ui| {
                let (name, text) =
                    if self.playing { ("media-playback-pause", "Pause") } else { ("media-playback-start", "Play") };
                let button = match slerp_theme::icon(name, 16).filter(|_| !self.plain) {
                    Some(icon) => egui::Button::image_and_text(
                        egui::Image::new(icon).fit_to_exact_size(egui::vec2(16.0, 16.0)),
                        text,
                    ),
                    None => egui::Button::new(text),
                };
                if ui.add(button).clicked() {
                    self.playing = !self.playing;
                }
                let stop = match slerp_theme::icon("media-playback-stop", 16).filter(|_| !self.plain) {
                    Some(icon) => egui::Button::image_and_text(
                        egui::Image::new(icon).fit_to_exact_size(egui::vec2(16.0, 16.0)),
                        "Stop",
                    ),
                    None => egui::Button::new("Stop"),
                };
                let _ = ui.add(stop);
                check(plain, ui, &mut self.looping, "Loop");
            });
            egui::Grid::new("anim").num_columns(2).spacing([8.0, 6.0]).show(ui, |ui| {
                ui.label("Time:");
                let slider = Slider::new(&mut self.time, 0.0..=2.4).suffix(" s").fixed_decimals(2);
                add_slider(plain, ui, slider);
                ui.end_row();
                ui.label("Speed:");
                ui.add(egui::DragValue::new(&mut self.speed).speed(0.05).range(0.1..=4.0).suffix("×"));
                ui.end_row();
            });
            ui.add_space(4.0);
            ui.label(RichText::new("Clips").strong());
            for (i, (name, length, _)) in CLIPS.iter().enumerate().take(4) {
                ui.selectable_value(&mut self.selected_clip, i, format!("{name}  ({length})"));
            }
        });
        self.collapsing("Morph targets").show(ui, |ui| {
            for (value, name) in self.morphs.iter_mut().zip(["browInnerUp", "eyeBlinkLeft", "jawOpen", "mouthSmile"]) {
                let slider = Slider::new(value, 0.0..=1.0).text(name);
                if plain {
                    ui.add(slider)
                } else {
                    widgets::slider(ui, slider)
                };
            }
            let _ = ui.button("Reset All");
        });
    }

    fn central(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label("Filter:");
            let field = if self.plain {
                TextEdit::singleline(&mut self.filter)
            } else {
                widgets::text_field(ui.ctx(), &mut self.filter)
            };
            ui.add(field.hint_text("Search clips…").desired_width(230.0));
            let _ = ui.button("Add Animations…").on_hover_text("Load clips from a GLB, FBX or VRMA file");
        });
        ui.add_space(4.0);
        // The 3D view stands in as a dark canvas.
        let height = (ui.available_height() - 200.0).max(120.0);
        let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), height), egui::Sense::hover());
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 0.0, egui::Color32::from_rgb(48, 52, 58));
        for i in 0..12 {
            let t = i as f32 / 11.0;
            let x = egui::lerp(rect.left()..=rect.right(), t);
            painter.vline(x, rect.y_range(), egui::Stroke::new(1.0, egui::Color32::from_rgb(60, 65, 72)));
        }
        painter.circle_filled(rect.center(), height * 0.25, egui::Color32::from_rgb(150, 120, 90));
        painter.text(
            rect.left_top() + egui::vec2(8.0, 6.0),
            egui::Align2::LEFT_TOP,
            "Orbit · Perspective",
            egui::TextStyle::Small.resolve(ui.style()),
            egui::Color32::from_gray(220),
        );
        ui.add_space(6.0);
        let heading = if self.plain { egui::TextStyle::Heading } else { egui::TextStyle::Name("Heading2".into()) };
        ui.label(RichText::new("Animation clips").text_style(heading));
        let filter = self.filter.to_lowercase();
        let plain = self.plain;
        let frame = if plain { egui::Frame::new() } else { widgets::view_frame(ui.ctx()).inner_margin(1) };
        frame.show(ui, |ui| {
            TableBuilder::new(ui)
                .striped(true)
                .resizable(true)
                .column(Column::initial(180.0))
                .column(Column::initial(100.0))
                .column(Column::remainder())
                .header(30.0, |mut header| {
                    for title in ["Name", "Length", "Frames"] {
                        header.col(|ui| {
                            if plain {
                                ui.strong(title);
                            } else {
                                widgets::header_cell(ui, title);
                            }
                        });
                    }
                })
                .body(|mut body| {
                    for (name, length, frames) in CLIPS.iter().filter(|c| c.0.to_lowercase().contains(&filter)) {
                        body.row(30.0, |mut row| {
                            row.col(|ui| {
                                ui.label(*name);
                            });
                            row.col(|ui| {
                                ui.label(*length);
                            });
                            row.col(|ui| {
                                ui.label(*frames);
                            });
                        });
                    }
                });
        });
    }
}

fn add_slider(plain: bool, ui: &mut egui::Ui, slider: Slider) -> egui::Response {
    if plain { ui.add(slider) } else { widgets::slider(ui, slider) }
}

fn menu(ui: &mut egui::Ui, title: &str, add_contents: impl FnOnce(&mut egui::Ui)) {
    if std::env::args().any(|a| a == "--plain") {
        ui.menu_button(title, add_contents);
    } else {
        widgets::menu_button(ui, title, add_contents);
    }
}

fn check(plain: bool, ui: &mut egui::Ui, value: &mut bool, text: &str) {
    if plain {
        ui.checkbox(value, text);
    } else {
        ui.add(widgets::CheckBox::new(value, text));
    }
}
