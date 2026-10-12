//! The side panels.  On the right, controls for what is shown: animation,
//! the face, morph targets and VRM expressions, the camera, overlays,
//! lighting, and the texture, material and gallery modes.  On the left,
//! the inspector: the file, statistics, the scene tree, materials,
//! textures and the GPU.

use crate::anim::{ARKIT_GROUPS, ARKIT_PRESETS, arkit_name, is_arkit_name, vrm_group};
use crate::app::{Action, Viewer, bytes, short_name};
use crate::camera::Mode;
use crate::document::{Content, Document};
use crate::look::Backdrop;
use crate::scene_view::SceneView;
use crate::texture_view::GalleryView;
use egui::{Slider, Ui};
use slerp_core::MaterialState;
use slerp_render::{DebugChannel, Tonemap};
use slerp_scene::{AlphaMode, Effect, Material, Scene, Shape, TexRef};
use slerp_theme::widgets;

/*┌─────────────────╮
| Shared pieces   |
└─────────────────╯*/
//region Shared pieces

/// A collapsible section that remembers whether it is open (see
/// `sections`); `open` is how it starts the first time.
fn section(ui: &mut Ui, title: &str, open: bool, add: impl FnOnce(&mut Ui)) {
    crate::sections::section(ui, title, title, open, add);
}

fn check(ui: &mut Ui, value: &mut bool, text: &str) -> egui::Response {
    ui.add(widgets::CheckBox::new(value, text))
}

fn slider(ui: &mut Ui, s: Slider) -> egui::Response {
    widgets::slider(ui, s)
}

/// A two-column grid of labels and controls, as in a Qt form layout.
fn form(ui: &mut Ui, id: &str, add: impl FnOnce(&mut Ui)) {
    egui::Grid::new(id).num_columns(2).spacing([8.0, 4.0]).show(ui, add);
}

/// Lays out a form's control so it claims the rest of its row, which keeps
/// the grid column at the width of the row instead of the width of the
/// control.  A slider's value box changes width as its value changes when
/// the font's digits aren't all equally wide, as in San Francisco, macOS's
/// system font; while a clip plays, a column sized by the time readout
/// would change width every frame and make the grid jitter.
fn fill_row<R>(ui: &mut Ui, add: impl FnOnce(&mut Ui) -> R) -> R {
    ui.scope(|ui| {
        ui.set_min_width(ui.available_width());
        add(ui)
    })
    .inner
}

/// "browDownLeft" as "Brow down left".
fn words(name: &str) -> String {
    let mut out = String::with_capacity(name.len() + 4);
    for (i, c) in name.chars().enumerate() {
        if c == '_' {
            out.push(' ');
        } else if c.is_uppercase() && i > 0 {
            out.push(' ');
            out.extend(c.to_lowercase());
        } else if i == 0 {
            out.extend(c.to_uppercase());
        } else {
            out.push(c);
        }
    }
    out
}

//endregion

/*┌─────────────────╮
| Controls        |
└─────────────────╯*/
//region Controls

/// The Controls panel: camera, display, lighting and whatever else the
/// file offers (clips, parts, material, stretch).  `dock` frames it.
pub(crate) fn controls_contents(v: &mut Viewer, ui: &mut Ui) {
    let aspect = v.view_size.0 as f32 / v.view_size.1.max(1) as f32;
    let libraries = v.libraries();
    let Viewer { doc, look, gfx, ui: state, actions, saved, .. } = v;
    let Some(doc) = doc else {
        lighting(ui, look, &slerp_scene::Scene::default(), Tonemap::PbrNeutral, &v.saved.recent_environments, actions);
        return;
    };
    parts_section(ui, doc, state.requested_part.as_deref(), actions);
    match &mut doc.content {
        Content::Volume(v) => crate::volume_view::controls(ui, v),
        Content::Scene(view) => {
            scene_controls(ui, view, look, state, actions, &libraries);
        }
        Content::Material(m) => {
            section(ui, "Material", true, |ui| {
                // A new shape is a new mesh; tiling and displacement only
                // rewrite the material's uniform, every frame of a drag.
                let (mut reshaped, mut retuned) = (false, false);
                ui.horizontal(|ui| {
                    ui.label("Shape:");
                    let mut shape = m.state.shape();
                    let shapes = [(Shape::Sphere, "Sphere"), (Shape::Cube, "Cube"), (Shape::Plane, "Plane")];
                    reshaped |= widgets::radio_group(ui, &mut shape, &shapes).changed();
                    m.state.shape = MaterialState::shape_name(shape).into();
                });
                form(ui, "material-form", |ui| {
                    ui.label("Tiling:");
                    let r = slider(ui, Slider::new(&mut m.state.tiling, 0.25..=8.0).logarithmic(true).suffix("×"));
                    retuned |= r.changed();
                    ui.end_row();
                    if m.asset.material.height_texture.is_some() {
                        ui.label("Displacement:");
                        let mut d = m.displacement() * 100.0;
                        let r = slider(ui, Slider::new(&mut d, 0.0..=10.0).suffix(" cm").fixed_decimals(1));
                        if r.changed() {
                            m.state.displacement = Some(d / 100.0);
                            retuned = true;
                        }
                        ui.end_row();
                    }
                });
                if reshaped {
                    m.rebuild(gfx, aspect);
                } else if retuned {
                    m.retune(gfx);
                }
                if let Some(preview) = m.preview(ui.ctx()) {
                    ui.add_space(4.0);
                    ui.label("The file's own preview:");
                    let size = preview.size_vec2();
                    let w = ui.available_width().min(256.0);
                    ui.add(egui::Image::new(&preview).fit_to_exact_size(egui::vec2(w, w * size.y / size.x.max(1.0))));
                }
            });
            scene_controls(ui, &mut m.view, look, state, actions, &[]);
        }
        Content::Texture(t) => {
            if t.data_range.is_some() {
                relief_controls(ui, t, gfx, aspect);
            }
            crate::stretch_view::controls(ui, t, saved, gfx);
            match &mut t.relief {
                Some(view) => scene_controls(ui, view, look, state, actions, &[]),
                None => texture_controls(ui, t, actions, true),
            }
        }
        Content::Gallery(g) => {
            section(ui, "Gallery", true, |ui| {
                ui.label(format!("{} pictures", g.items.len()));
                form(ui, "gallery-form", |ui| {
                    ui.label("Find:");
                    ui.add(widgets::text_field(ui.ctx(), &mut g.filter).hint_text("Name").desired_width(160.0));
                    ui.end_row();
                    ui.label("Size:");
                    slider(ui, Slider::new(&mut g.cell, 64.0..=320.0).suffix(" px").integer());
                    ui.end_row();
                });
                if let Some((i, _)) = &g.open {
                    let i = *i;
                    if ui.button("Back to the Gallery").clicked() {
                        actions.push(Action::BackToGallery);
                    }
                    let mut pick = i;
                    let items = g.items.iter().enumerate().map(|(j, item)| (j, item.name.clone()));
                    widgets::Choice::new("gallery-pick", &mut pick, items).width(200.0).show(ui);
                    if pick != i
                        && let Err(e) = g.open_item(pick, gfx)
                    {
                        eprintln!("slerp view: {e}");
                    }
                }
            });
            if let Some((_, t)) = &mut g.open {
                texture_controls(ui, t, actions, false);
            }
        }
        Content::Canvas(c) => crate::canvas_view::controls(ui, c),
        Content::Lut(l) => {
            let mut choose = false;
            crate::lut_view::controls(ui, l, &mut choose);
            if choose {
                actions.push(Action::ChoosePhoto);
            }
        }
        Content::Palette(p) => crate::palette_view::controls(ui, p),
        Content::Dicom(d) => crate::dicom_view::controls(ui, d, actions),
        Content::Cad(c) => crate::cad_view::controls(ui, c),
        Content::Pages(p) => {
            if crate::pages_view::controls(ui, p) {
                doc.part = Some(p.part_name());
            }
        }
    }
    if let Some(view) = doc.scene() {
        let tonemap = slerp_core::default_tonemap(&view.scene);
        lighting(ui, look, &view.scene, tonemap, &v.saved.recent_environments, actions);
    }
}

/// A file's parts (a WAD's maps, an LDtk project's levels): a picker that
/// opens one (a WAD's map as a 3D level), and a switch to the whole file
/// (a WAD's 2D gallery of title screen and automaps, an LDtk world).
/// `requested` is a part asked for and still loading.
fn parts_section(ui: &mut Ui, doc: &Document, requested: Option<&str>, actions: &mut Vec<Action>) {
    use crate::loader::Part;
    if doc.part_names.is_empty() {
        return;
    }
    // A PDF's pages have their own picker in the view's section; a
    // PostScript document's pages and an EPS's two pictures a plain one.
    if let Content::Pages(_) = doc.content {
        return;
    }
    if let slerp_core::Kind::Document(f) = doc.kind {
        let current = requested.map(str::to_string).or(doc.part.clone()).unwrap_or_else(|| doc.part_names[0].clone());
        let title = if f == slerp_core::documents::DocumentFormat::PostScript { "Pages" } else { "Picture" };
        crate::volume_view::parts_controls(ui, &doc.part_names, &current, title, actions);
        return;
    }
    // A Source model's skin families: a plain picker; each is the whole
    // model in another set of materials.
    if doc.kind.format(&doc.path).is_some_and(|f| f.id == "source-mdl") {
        let current = requested.map(str::to_string).or(doc.part.clone()).unwrap_or_else(|| doc.part_names[0].clone());
        crate::volume_view::parts_controls(ui, &doc.part_names, &current, "Skins", actions);
        return;
    }
    // An OpenEXR file's layers and parts: a plain picker.
    if let slerp_core::Kind::Image(slerp_core::images::ImageFormat::Exr) = doc.kind {
        let current = requested.map(str::to_string).or(doc.part.clone()).unwrap_or_else(|| doc.part_names[0].clone());
        crate::volume_view::parts_controls(ui, &doc.part_names, &current, "Layers", actions);
        return;
    }
    // Arrays, datasets and slide levels: a plain picker.
    if let slerp_core::Kind::Volume(f) = doc.kind {
        let current = requested.map(str::to_string).or(doc.part.clone()).unwrap_or_else(|| doc.part_names[0].clone());
        crate::volume_view::parts_controls(ui, &doc.part_names, &current, crate::volume_view::parts_title(f), actions);
        return;
    }
    // The map the picker shows: the one asked for and loading, else the one
    // in 3D, else the automap open in the gallery, else the first.
    let in_gallery = match &doc.content {
        Content::Gallery(g) => g.open.as_ref().and_then(|(i, _)| g.items.get(*i)).map(|item| item.name.clone()),
        _ => None,
    };
    let current = requested
        .filter(|n| doc.part_names.iter().any(|p| p == n))
        .map(str::to_string)
        .or(doc.part.clone())
        .or(in_gallery.filter(|n| doc.part_names.contains(n)))
        .unwrap_or_else(|| doc.part_names[0].clone());
    // A WAD's maps open as 3D levels or as the whole file's automaps; an
    // LDtk project's levels one at a time or together as its world.
    let (title, label, one, whole) = match doc.content {
        Content::Canvas(_) => ("Levels", "Level:", "One level", "World"),
        _ => ("Maps", "Map:", "3D level", "2D automaps"),
    };
    section(ui, title, true, |ui| {
        form(ui, "parts-form", |ui| {
            ui.label(label);
            let mut pick = current.clone();
            let items = doc.part_names.iter().map(|n| (n.clone(), n.clone()));
            widgets::Choice::new("part-pick", &mut pick, items).width(160.0).show(ui);
            if pick != current {
                actions.push(Action::OpenPart(Part::Named(pick)));
            }
            ui.end_row();
            ui.label("Show:");
            ui.horizontal(|ui| {
                let mut three_d = doc.part.is_some();
                widgets::radio_group(ui, &mut three_d, &[(true, one), (false, whole)]);
                if three_d != doc.part.is_some() {
                    actions.push(Action::OpenPart(if three_d { Part::Named(current.clone()) } else { Part::Whole }));
                }
            });
            ui.end_row();
        });
    });
}

fn scene_controls(
    ui: &mut Ui,
    view: &mut SceneView,
    look: &mut crate::look::Look,
    state: &mut crate::app::UiState,
    actions: &mut Vec<Action>,
    libraries: &[std::path::PathBuf],
) {
    if !view.scene.animations.is_empty() || view.scene.skins.iter().any(|s| !s.joints.is_empty()) || view.has_effects {
        section(ui, "Animation", true, |ui| animation(ui, view, look, state, actions, libraries));
    }
    crate::dna::controls(ui, view);
    crate::paths_view::controls(ui, view);
    if view.arkit {
        section(ui, "Face", true, |ui| face(ui, view));
    }
    if !view.scene.expressions.is_empty() {
        section(ui, "Expressions", true, |ui| expressions(ui, view));
    }
    let extra =
        view.morph_list.names.iter().any(|n| !(view.arkit && is_arkit_name(n))) || !view.morph_list.unnamed.is_empty();
    if extra {
        section(ui, "Morph Targets", !view.arkit && view.dna.is_none(), |ui| morphs(ui, view, &mut state.morph_filter));
    }
    section(ui, "Camera", false, |ui| camera(ui, view, actions));
    section(ui, "Display", false, |ui| display(ui, view, look));
}

fn animation(
    ui: &mut Ui,
    view: &mut SceneView,
    look: &mut crate::look::Look,
    state: &mut crate::app::UiState,
    actions: &mut Vec<Action>,
    libraries: &[std::path::PathBuf],
) {
    let SceneView { scene, player, pose_dirty, has_effects, foot_ik, own_clips, .. } = view;
    if !scene.animations.is_empty() {
        // 1. Transport.
        ui.horizontal(|ui| {
            let playing = player.playing && player.clip.is_some();
            let (icon, text) =
                if playing { ("media-playback-pause", "Pause") } else { ("media-playback-start", "Play") };
            if icon_button(ui, icon, text).on_hover_text("Space").clicked() {
                actions.push(Action::PlayPause);
            }
            if icon_button(ui, "media-playback-stop", "Rest Pose")
                .on_hover_text("Show the skeleton's rest pose")
                .clicked()
            {
                player.select(scene, None);
            }
            if icon_button(ui, "media-skip-backward", "").on_hover_text("Previous clip ([)").clicked() {
                actions.push(Action::StepClip(-1));
            }
            if icon_button(ui, "media-skip-forward", "").on_hover_text("Next clip (])").clicked() {
                actions.push(Action::StepClip(1));
            }
        });
        let duration = player.duration(scene);
        form(ui, "anim-form", |ui| {
            ui.label("Time:");
            let mut t = player.time;
            let r = fill_row(ui, |ui| {
                slider(ui, Slider::new(&mut t, 0.0..=duration.max(0.001)).suffix(" s").fixed_decimals(2))
            });
            if r.changed() {
                player.time = t;
                *pose_dirty = true;
            }
            ui.end_row();
            ui.label("Speed:");
            fill_row(ui, |ui| {
                slider(ui, Slider::new(&mut player.speed, 0.1..=3.0).logarithmic(true).suffix("×").fixed_decimals(2))
            });
            ui.end_row();
        });
        ui.horizontal(|ui| {
            check(ui, &mut player.looping, "Loop");
            if check(ui, &mut player.root_motion, "Root motion")
                .on_hover_text("Let the clip move the model; off keeps walks on the spot")
                .changed()
            {
                player.update_hold(scene);
                *pose_dirty = true;
            }
        });
        check(ui, &mut look.overlays.skeleton, "Show skeleton").on_hover_text("Point at a joint to see its name");

        // 2. Clips.
        ui.add_space(4.0);
        ui.label(
            egui::RichText::new(format!("Clips ({})", scene.animations.len()))
                .text_style(egui::TextStyle::Name("Bold".into())),
        );
        if scene.animations.len() > 10 {
            ui.add(
                widgets::text_field(ui.ctx(), &mut state.clip_filter)
                    .hint_text("Find a clip")
                    .desired_width(f32::INFINITY),
            );
        }
        let filter = state.clip_filter.to_lowercase();
        egui::ScrollArea::vertical().id_salt("clips").max_height(260.0).show(ui, |ui| {
            ui.set_width(ui.available_width());
            let mut pick = None;
            let mut list = widgets::List::new(ui, "clips");
            for (i, a) in scene.animations.iter().enumerate() {
                if !filter.is_empty() && !a.name.to_lowercase().contains(&filter) {
                    continue;
                }
                let text = format!("{}  ({:.2} s)", if a.name.is_empty() { "unnamed" } else { &a.name }, a.duration);
                let r = list.row(ui, i, player.clip == Some(i), text);
                if r.clicked() {
                    pick = Some(i);
                }
                // Keep a clip chosen elsewhere (keys, a new library) in
                // sight.
                if player.clip == Some(i) && state.shown_clip != player.clip {
                    r.scroll_to_me(None);
                }
            }
            pick = list.finish(ui).or(pick);
            state.shown_clip = player.clip;
            if let Some(i) = pick {
                player.select(scene, Some(i));
                state.shown_clip = Some(i);
            }
        });
    } else if !scene.skins.is_empty() {
        ui.label("This file has a skeleton but no clips.");
        check(ui, &mut look.overlays.skeleton, "Show skeleton");
    }
    if *has_effects {
        check(ui, &mut look.animate_effects, "Animate liquids and sky");
    }
    if !scene.skins.is_empty() || !scene.animations.is_empty() {
        ui.add_space(4.0);
        ui.horizontal_wrapped(|ui| {
            if ui.button("Add Animations…").on_hover_text("Clips from another GLB, FBX or VRMA file").clicked() {
                actions.push(Action::ChooseLibrary);
            }
            if !libraries.is_empty() {
                ui.menu_button("Recent", |ui| {
                    for lib in libraries {
                        if ui.button(short_name(lib)).on_hover_text(lib.display().to_string()).clicked() {
                            actions.push(Action::AddLibrary(lib.clone()));
                        }
                    }
                });
            }
            let mut on = foot_ik.on;
            let tip = "Keep planted feet in place on clips from characters with other proportions";
            if check(ui, &mut on, "Foot IK").on_hover_text(tip).changed() {
                foot_ik.set(on, scene, *own_clips);
                player.update_hold(scene);
                *pose_dirty = true;
            }
        });
    }
}

fn icon_button(ui: &mut Ui, icon: &str, text: &str) -> egui::Response {
    match slerp_theme::icon(icon, 16) {
        Some(src) if text.is_empty() => {
            ui.add(egui::Button::image(egui::Image::new(src).fit_to_exact_size(egui::vec2(16.0, 16.0))))
        }
        Some(src) => {
            ui.add(egui::Button::image_and_text(egui::Image::new(src).fit_to_exact_size(egui::vec2(16.0, 16.0)), text))
        }
        // Without an icon theme (Windows, macOS), icon-only buttons need words.
        None => ui.button(if text.is_empty() { icon_words(icon) } else { text }),
    }
}

/// A label for an icon-only button when the icon can't be found.
fn icon_words(icon: &str) -> &str {
    match icon {
        "media-skip-backward" => "Previous",
        "media-skip-forward" => "Next",
        other => other,
    }
}

/// The current weight of a named target: the user's setting, else what
/// the pose shows.
fn named_weight(view: &SceneView, name: &str) -> f32 {
    if let Some(&w) = view.morphs.named.get(name) {
        return w;
    }
    let i = view.morph_list.names.iter().position(|n| n == name);
    i.and_then(|i| view.morph_list.sources.get(i))
        .and_then(|&(node, t)| view.pose.weights.get(node).and_then(|w| w.get(t)).copied())
        .unwrap_or(0.0)
}

fn face(ui: &mut Ui, view: &mut SceneView) {
    ui.label("Expressions:");
    ui.horizontal_wrapped(|ui| {
        for (name, shapes) in ARKIT_PRESETS {
            if ui.button(*name).clicked() {
                view.morphs.named.retain(|n, _| !is_arkit_name(n));
                for (shape, w) in *shapes {
                    if let Some(n) = arkit_name(&view.morph_list, shape) {
                        view.morphs.named.insert(n.to_string(), *w);
                    }
                }
                // Neutral pins every shape at zero, so a clip's face
                // animation can't show through.
                if shapes.is_empty() {
                    for n in view.morph_list.names.iter().filter(|n| is_arkit_name(n)) {
                        view.morphs.named.insert(n.clone(), 0.0);
                    }
                }
                view.pose_dirty = true;
            }
        }
    });
    if ui.button("Reset Face").on_hover_text("Back to what the clip shows").clicked() {
        view.morphs.named.retain(|n, _| !is_arkit_name(n));
        view.pose_dirty = true;
    }
    for (group, names) in ARKIT_GROUPS {
        let open = matches!(*group, "Brows" | "Jaw");
        crate::sections::section(ui, &format!("Face/{group}"), *group, open, |ui| {
            form(ui, &format!("face-{group}"), |ui| {
                for name in *names {
                    let Some(file_name) = arkit_name(&view.morph_list, name).map(str::to_string) else { continue };
                    let mut w = named_weight(view, &file_name);
                    ui.label(words(name));
                    if slider(ui, Slider::new(&mut w, 0.0..=1.0).fixed_decimals(2)).changed() {
                        view.morphs.named.insert(file_name, w);
                        view.pose_dirty = true;
                    }
                    ui.end_row();
                }
            });
        });
    }
}

fn expressions(ui: &mut Ui, view: &mut SceneView) {
    let exprs: Vec<(usize, String, &'static str)> = view
        .scene
        .expressions
        .iter()
        .enumerate()
        .map(|(i, e)| (i, e.name.clone(), vrm_group(e.preset.as_deref(), &e.name)))
        .collect();
    // Emotions as one-click buttons: that expression fully, the others off.
    let emotions: Vec<&(usize, String, &str)> = exprs.iter().filter(|e| e.2 == "Emotions").collect();
    if !emotions.is_empty() {
        ui.horizontal_wrapped(|ui| {
            for (i, name, _) in &emotions {
                if ui.button(words(name)).clicked() {
                    for (j, _, g) in &exprs {
                        if *g == "Emotions" {
                            view.morphs.expressions[*j] = 0.0;
                        }
                    }
                    view.morphs.expressions[*i] = 1.0;
                    view.pose_dirty = true;
                }
            }
        });
    }
    if ui.button("Reset Expressions").clicked() {
        view.morphs.expressions.iter_mut().for_each(|w| *w = 0.0);
        view.pose_dirty = true;
    }
    for group in ["Emotions", "Mouth", "Blink", "Gaze", "Custom"] {
        let members: Vec<&(usize, String, &str)> = exprs.iter().filter(|e| e.2 == group).collect();
        if members.is_empty() {
            continue;
        }
        crate::sections::section(ui, &format!("Expressions/{group}"), group, group != "Custom", |ui| {
            form(ui, &format!("vrm-{group}"), |ui| {
                for (i, name, _) in members {
                    ui.label(words(name));
                    if slider(ui, Slider::new(&mut view.morphs.expressions[*i], 0.0..=1.0).fixed_decimals(2)).changed()
                    {
                        view.pose_dirty = true;
                    }
                    ui.end_row();
                }
            });
        });
    }
}

fn morphs(ui: &mut Ui, view: &mut SceneView, filter: &mut String) {
    let names: Vec<String> =
        view.morph_list.names.iter().filter(|n| !(view.arkit && is_arkit_name(n))).cloned().collect();
    let total = names.len() + view.morph_list.unnamed.iter().map(|u| u.2).sum::<usize>();
    if total > 12 {
        ui.add(widgets::text_field(ui.ctx(), filter).hint_text("Find a target").desired_width(f32::INFINITY));
    }
    let f = filter.to_lowercase();
    if ui.button("Reset Targets").clicked() {
        view.morphs.named.retain(|n, _| view.arkit && is_arkit_name(n));
        view.morphs.indexed.clear();
        view.pose_dirty = true;
    }
    egui::ScrollArea::vertical().id_salt("morphs").max_height(360.0).show(ui, |ui| {
        form(ui, "morph-form", |ui| {
            for name in names.iter().filter(|n| f.is_empty() || n.to_lowercase().contains(&f)) {
                let mut w = named_weight(view, name);
                ui.label(name);
                if slider(ui, Slider::new(&mut w, 0.0..=1.0).fixed_decimals(2)).changed() {
                    view.morphs.named.insert(name.clone(), w);
                    view.pose_dirty = true;
                }
                ui.end_row();
            }
            let unnamed = view.morph_list.unnamed.clone();
            for (node, node_name, count) in unnamed {
                for t in 0..count {
                    let label = format!("{node_name} {t}");
                    if !f.is_empty() && !label.to_lowercase().contains(&f) {
                        continue;
                    }
                    let mut w = view
                        .morphs
                        .indexed
                        .get(&(node, t))
                        .copied()
                        .or_else(|| view.pose.weights.get(node).and_then(|ws| ws.get(t)).copied())
                        .unwrap_or(0.0);
                    ui.label(label);
                    if slider(ui, Slider::new(&mut w, 0.0..=1.0).fixed_decimals(2)).changed() {
                        view.morphs.indexed.insert((node, t), w);
                        view.pose_dirty = true;
                    }
                    ui.end_row();
                }
            }
        });
    });
}

fn camera(ui: &mut Ui, view: &mut SceneView, actions: &mut Vec<Action>) {
    let mut mode = view.cameras.mode;
    ui.horizontal(|ui| {
        widgets::radio_group(ui, &mut mode, &[(Mode::Orbit, "Orbit"), (Mode::Fly, "First person")]);
    });
    if mode != view.cameras.mode {
        actions.push(Action::ToggleCamera);
    }
    form(ui, "camera-form", |ui| {
        ui.label("Field of view:");
        let fov = match view.cameras.mode {
            Mode::Orbit => &mut view.cameras.orbit.fov_y,
            Mode::Fly => &mut view.cameras.fly.fov_y,
        };
        let mut deg = fov.to_degrees();
        if slider(ui, Slider::new(&mut deg, 10.0..=120.0).suffix("°").integer()).changed() {
            *fov = deg.to_radians();
        }
        ui.end_row();
        if view.cameras.mode == Mode::Fly {
            ui.label("Speed:");
            slider(ui, Slider::new(&mut view.cameras.fly_speed, 0.05..=100.0).logarithmic(true).suffix(" m/s"));
            ui.end_row();
        }
    });
    if view.cameras.mode == Mode::Orbit {
        let mut ortho = view.cameras.orbit.ortho;
        if check(ui, &mut ortho, "Orthographic (5)").changed() {
            view.cameras.set_ortho(ortho);
        }
    }
    ui.horizontal_wrapped(|ui| {
        for (text, tip, yaw, pitch) in [
            ("Front", "1", 0.0, 0.0),
            ("Side", "3", 90.0, 0.0),
            ("Top", "7", 0.0, 90.0),
            ("Back", "Ctrl+1", 180.0, 0.0),
        ] {
            if ui.button(text).on_hover_text(crate::keys(tip)).clicked() {
                actions.push(Action::View(yaw, pitch));
            }
        }
    });
    ui.horizontal(|ui| {
        if ui.button("Frame").on_hover_text("F").clicked() {
            actions.push(Action::Frame);
        }
        if ui.button("Reset View").on_hover_text("Home").clicked() {
            actions.push(Action::Home);
        }
    });
}

/// Whether a scene holds point clouds, which get the point size controls.
pub fn has_points(scene: &Scene) -> bool {
    scene.meshes.iter().flat_map(|m| &m.primitives).any(|p| p.topology == slerp_scene::Topology::Points)
}

fn display(ui: &mut Ui, view: &mut SceneView, look: &mut crate::look::Look) {
    let o = &mut look.overlays;
    ui.columns(2, |cols| {
        check(&mut cols[0], &mut o.wireframe, "Wireframe");
        check(&mut cols[0], &mut o.normals, "Normals");
        check(&mut cols[0], &mut o.tangents, "Tangents");
        check(&mut cols[0], &mut o.bounds, "Bounds");
        check(&mut cols[1], &mut o.skeleton, "Skeleton");
        check(&mut cols[1], &mut o.grid, "Grid (G)");
        if !view.scene.markers.is_empty() {
            check(&mut cols[1], &mut o.markers, "Markers");
        }
        check(&mut cols[1], &mut look.ao, "Ambient occlusion")
            .on_hover_text("Darkens creases, corners and where things touch");
    });
    form(ui, "display-form", |ui| {
        ui.label("Show:");
        let names = [
            (DebugChannel::None, "Lit"),
            (DebugChannel::BaseColor, "Base colour"),
            (DebugChannel::Normal, "Normals"),
            (DebugChannel::Metallic, "Metallic"),
            (DebugChannel::Roughness, "Roughness"),
            (DebugChannel::Occlusion, "Occlusion"),
            (DebugChannel::Emissive, "Emissive"),
            (DebugChannel::UvChecker, "UV checker"),
        ];
        let items = names.iter().map(|(d, name)| (*d, name.to_string()));
        widgets::Choice::new("debug-channel", &mut look.debug, items).width(150.0).show(ui);
        ui.end_row();
        if look.ao {
            ui.label("Occlusion:");
            slider(ui, Slider::new(&mut look.ao_strength, 0.0..=3.0).fixed_decimals(2))
                .on_hover_text("How strongly ambient occlusion darkens");
            ui.end_row();
        }
        if view.gpu.splat_count() > 0 {
            ui.label("Splat size:");
            slider(ui, Slider::new(&mut look.splat_scale, 0.05..=2.0).logarithmic(true).suffix("×").fixed_decimals(2));
            ui.end_row();
        }
        if has_points(&view.scene) {
            ui.label("Point size:");
            slider(ui, Slider::new(&mut look.point_size, 0.1..=8.0).logarithmic(true).suffix("×").fixed_decimals(2))
                .on_hover_text("1 is the size at which neighbouring points meet");
            ui.end_row();
            ui.label("");
            check(ui, &mut look.point_world_size, "Size in the world")
                .on_hover_text("Points grow as the camera comes closer, instead of keeping their size on screen");
            ui.end_row();
        }
        if let Some(lods) = &view.lods {
            ui.label("Detail level:");
            let mut level = lods.current;
            let items = std::iter::once((None, "All".to_string()))
                .chain(lods.levels.iter().map(|&l| (Some(l), format!("LOD {l}"))));
            widgets::Choice::new("lod", &mut level, items).width(150.0).show(ui);
            if level != view.lods.as_ref().and_then(|l| l.current) {
                view.set_lod(level);
            }
            ui.end_row();
        }
    });
}

/// The tone mapping choices, as the radio buttons name them.
const TONEMAPS: [(Tonemap, &str); 3] =
    [(Tonemap::PbrNeutral, "Neutral"), (Tonemap::Aces, "ACES"), (Tonemap::None, "None")];

/// `tonemap` is the shown scene's default tone mapping, which the radio
/// buttons show until the user picks one.
fn lighting(
    ui: &mut Ui,
    look: &mut crate::look::Look,
    scene: &slerp_scene::Scene,
    tonemap: Tonemap,
    recent: &[std::path::PathBuf],
    actions: &mut Vec<Action>,
) {
    section(ui, "Lighting", false, |ui| {
        form(ui, "light-form", |ui| {
            ui.label("Environment:");
            let current = look.environment.as_deref().map_or("Studio".into(), short_name);
            // Studio, the recent pictures, then the other HDRs beside the
            // current one, since they tend to be kept together.
            let mut items: Vec<(Option<std::path::PathBuf>, String)> = vec![(None, "Studio".into())];
            items.extend(recent.iter().map(|p| (Some(p.clone()), short_name(p))));
            let first_sibling = items.len();
            let siblings = look.environment.as_deref().map(sibling_hdrs).unwrap_or_default();
            items.extend(
                siblings.into_iter().filter(|p| !recent.contains(p)).map(|p| (Some(p.clone()), short_name(&p))),
            );
            let mut env = look.environment.clone();
            let mut choose = false;
            let mut choice =
                widgets::Choice::new("environment", &mut env, items.clone()).width(170.0).text_when_other(current);
            if items.len() > first_sibling {
                choice = choice.separator_before(first_sibling);
            }
            choice
                .extra(|ui| {
                    if ui.selectable_label(false, "Choose File…").clicked() {
                        choose = true;
                    }
                })
                .show(ui);
            if choose {
                actions.push(Action::ChooseEnvironment);
            }
            if env != look.environment {
                actions.push(Action::SetEnvironment(env));
            }
            ui.end_row();
            ui.label("Turn:");
            slider(ui, Slider::new(&mut look.rotation, -180.0..=180.0).suffix("°").integer());
            ui.end_row();
            ui.label("Intensity:");
            slider(ui, Slider::new(&mut look.intensity, 0.0..=4.0).fixed_decimals(2));
            ui.end_row();
            ui.label("Key light:");
            slider(ui, Slider::new(&mut look.key_light, 0.0..=4.0).fixed_decimals(2));
            ui.end_row();
            ui.label("Exposure:");
            slider(ui, Slider::new(&mut look.exposure, -6.0..=6.0).suffix(" EV").fixed_decimals(1));
            ui.end_row();
            // Files that carry no light of their own (level-editor maps)
            // are lit by a fill, which each format remembers.
            if scene.fill > 0.0 {
                ui.label("Fill light:");
                let mut fill = look.fill(scene);
                if slider(ui, Slider::new(&mut fill, 0.0..=2.0).fixed_decimals(2)).changed() {
                    look.fills.insert(scene.info.format.clone(), fill);
                    actions.push(Action::SaveFill);
                }
                ui.end_row();
            }
            ui.label("Tone mapping:");
            ui.horizontal(|ui| {
                let mut t = look.tonemap.unwrap_or(tonemap);
                widgets::radio_group(ui, &mut t, &TONEMAPS);
                if t != look.tonemap.unwrap_or(tonemap) {
                    look.tonemap = Some(t);
                }
            });
            ui.end_row();
            ui.label("Background:");
            ui.horizontal(|ui| {
                let backdrops = [(Backdrop::Environment, "Environment"), (Backdrop::Color, "Colour")];
                widgets::radio_group(ui, &mut look.backdrop, &backdrops);
            });
            ui.end_row();
            match look.backdrop {
                Backdrop::Environment => {
                    ui.label("Blur:");
                    slider(ui, Slider::new(&mut look.blur, 0.0..=1.0).fixed_decimals(2));
                }
                Backdrop::Color => {
                    ui.label("Colour:");
                    egui::color_picker::color_edit_button_rgb(ui, &mut look.color);
                }
            }
            ui.end_row();
        });
        check(ui, &mut look.shadows, "Shadows");
        check(ui, &mut look.ground_shadow, "Shadow on the ground");
        if look.is_loading() {
            ui.horizontal(|ui| {
                crate::app::spinner(ui, 16.0);
                ui.label("Preparing the lighting…");
            });
        }
    });
}

fn relief_controls(ui: &mut Ui, t: &mut crate::texture_view::TextureView, gfx: &mut crate::gfx::Gfx, aspect: f32) {
    section(ui, "Height Map", true, |ui| {
        let mut on = t.relief.is_some();
        if check(ui, &mut on, "Show as relief")
            .on_hover_text("A 3D surface raised by the values and tinted by height")
            .changed()
        {
            t.set_relief(on, gfx, aspect);
        }
        if on {
            form(ui, "relief-form", |ui| {
                ui.label("Height:");
                let mut pct = t.relief_height * 100.0;
                let r = slider(ui, Slider::new(&mut pct, 1.0..=100.0).suffix(" % of width").integer());
                if r.changed() {
                    t.relief_height = pct / 100.0;
                }
                // Rebuilding half a million vertices on every step of a drag
                // would stutter, so it waits for the drag to end.
                if r.drag_stopped() || (r.changed() && !r.dragged()) {
                    t.set_relief(true, gfx, aspect);
                }
                ui.end_row();
            });
        }
    });
}

/// Radiance HDR files in the same folder as `path`, in natural order,
/// at most 24 of them.
/// The HDRs in the folder of `path`, read again at most every two seconds:
/// the lighting picker lists them in every frame the panel is drawn, for
/// the keyboard and wheel to step through.
fn sibling_hdrs(path: &std::path::Path) -> Vec<std::path::PathBuf> {
    type Cache = Option<(std::path::PathBuf, std::time::Instant, Vec<std::path::PathBuf>)>;
    static CACHE: std::sync::Mutex<Cache> = std::sync::Mutex::new(None);
    let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((p, at, list)) = &*cache
        && p == path
        && at.elapsed() < std::time::Duration::from_secs(2)
    {
        return list.clone();
    }
    let list = read_sibling_hdrs(path);
    *cache = Some((path.to_path_buf(), std::time::Instant::now(), list.clone()));
    list
}

fn read_sibling_hdrs(path: &std::path::Path) -> Vec<std::path::PathBuf> {
    let Some(dir) = path.parent() else { return Vec::new() };
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut out: Vec<std::path::PathBuf> = entries
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("hdr")))
        .collect();
    out.sort_by(|a, b| crate::loader::natural_cmp(&short_name(a), &short_name(b)));
    out.truncate(24);
    out
}

fn texture_controls(ui: &mut Ui, t: &mut crate::texture_view::TextureView, actions: &mut Vec<Action>, own_file: bool) {
    section(ui, "Texture", true, |ui| {
        let tex = t.texture.clone();
        form(ui, "texture-form", |ui| {
            ui.label("Mip level:");
            let mips = (0..t.gpu.mips()).map(|m| {
                let stored = if m < t.gpu.file_mips() { "" } else { " (generated)" };
                (Some(m), format!("{m}: {} × {}{stored}", (t.gpu.width() >> m).max(1), (t.gpu.height() >> m).max(1)))
            });
            let items: Vec<_> = std::iter::once((None, "Automatic".to_string())).chain(mips).collect();
            widgets::Choice::new("mip", &mut t.params.mip, items).width(170.0).show(ui);
            ui.end_row();
            let layers = t.layer_count();
            if layers > 1 {
                let is_3d = tex.kind == slerp_scene::textures::TextureKind::D3;
                ui.label(if is_3d { "Slice:" } else { "Layer:" });
                slider(ui, Slider::new(&mut t.params.layer, 0..=layers - 1));
                ui.end_row();
            }
            if tex.faces == 6 {
                ui.label("Face:");
                ui.horizontal_wrapped(|ui| {
                    let faces = [(0, "+X"), (1, "−X"), (2, "+Y"), (3, "−Y"), (4, "+Z"), (5, "−Z")];
                    widgets::radio_group(ui, &mut t.params.face, &faces);
                });
                ui.end_row();
            }
            ui.label("Channels:");
            ui.horizontal(|ui| {
                for (c, name) in ["R", "G", "B", "A"].iter().enumerate() {
                    if ui
                        .selectable_label(t.params.channels[c], *name)
                        .on_hover_text("Show or hide this channel")
                        .clicked()
                    {
                        t.params.channels[c] = !t.params.channels[c];
                    }
                }
            });
            ui.end_row();
            if t.gpu.is_hdr() {
                ui.label("Exposure:");
                slider(ui, Slider::new(&mut t.params.exposure, -10.0..=10.0).suffix(" EV").fixed_decimals(1));
                ui.end_row();
                ui.label("Tone mapping:");
                ui.horizontal(|ui| {
                    widgets::radio_group(ui, &mut t.params.tonemap, &TONEMAPS);
                });
                ui.end_row();
            }
            ui.label("Zoom:");
            ui.horizontal(|ui| {
                ui.label(format!("{:.0} %", t.params.zoom * 100.0 / ui.ctx().pixels_per_point()));
                if ui.button("Fit").on_hover_text("F").clicked() {
                    t.fit_now();
                }
                if ui.button("1:1").on_hover_text(crate::keys("Ctrl+0")).clicked() {
                    t.actual_size();
                }
            });
            ui.end_row();
        });
        check(ui, &mut t.params.checkerboard, "Checkerboard behind transparency");
        if t.is_panorama() {
            ui.add_space(4.0);
            let mut inside = t.inside.is_some();
            if check(ui, &mut inside, "Look around inside")
                .on_hover_text("Drag to look, scroll to zoom, Esc leaves")
                .changed()
            {
                actions.push(Action::Panorama(inside));
            }
            if own_file
                && ui.button("Use as Lighting").on_hover_text("Light the models you open with this picture").clicked()
            {
                actions.push(Action::UseAsLighting);
            }
        }
        // A file's own facts are in the inspector; a gallery picture's
        // aren't, so they show here.
        if !own_file {
            let mut facts = Vec::new();
            crate::document::texture_facts(&tex, &mut facts);
            ui.add_space(4.0);
            form(ui, "texture-facts", |ui| {
                for (k, v) in facts {
                    ui.label(egui::RichText::new(k).weak());
                    ui.label(v);
                    ui.end_row();
                }
            });
        }
    });
}

//endregion

/*┌─────────────────╮
| Gallery         |
└─────────────────╯*/
//region Gallery

/// The grid of a gallery's pictures.  Returns the one clicked.
pub fn gallery_grid(ui: &mut Ui, g: &mut GalleryView) -> Option<usize> {
    let mut clicked = None;
    let filter = g.filter.to_lowercase();
    let cell = g.cell;
    let ctx = ui.ctx().clone();
    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        ui.add_space(8.0);
        if g.items.is_empty() {
            // An archive with no pictures of its own: its entries are in the
            // Contents list.
            ui.add_space(24.0);
            ui.vertical_centered(|ui| {
                ui.label("Nothing to draw here.");
                ui.label(egui::RichText::new("Choose an entry in the Contents list.").weak());
            });
            return;
        }
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = egui::vec2(10.0, 10.0);
            ui.add_space(8.0);
            for i in 0..g.items.len() {
                if !filter.is_empty() && !g.items[i].name.to_lowercase().contains(&filter) {
                    continue;
                }
                let thumb = g.thumb(&ctx, i);
                let item = &g.items[i];
                let label_h = ui.text_style_height(&egui::TextStyle::Small) + 4.0;
                let (rect, response) = ui.allocate_exact_size(egui::vec2(cell, cell + label_h), egui::Sense::click());
                let size = format!("{} × {}", item.texture.width(), item.texture.height());
                let name = item.name.clone();
                response.widget_info(|| {
                    egui::WidgetInfo::labeled(egui::WidgetType::Button, true, format!("{name}, {size}"))
                });
                if ui.is_rect_visible(rect) {
                    let visuals = ui.style().interact(&response);
                    let image_rect = egui::Rect::from_min_size(rect.min, egui::vec2(cell, cell));
                    if response.hovered() || response.has_focus() {
                        ui.painter().rect_filled(rect.expand(3.0), 4.0, visuals.weak_bg_fill);
                    }
                    if let Some(t) = &thumb {
                        let s = t.size_vec2();
                        let scale = (cell / s.x.max(1.0)).min(cell / s.y.max(1.0));
                        let r = egui::Rect::from_center_size(image_rect.center(), s * scale);
                        ui.painter().image(
                            t.id(),
                            r,
                            egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                            egui::Color32::WHITE,
                        );
                    }
                    let font = egui::TextStyle::Small.resolve(ui.style());
                    let galley = ui.painter().layout(item.name.clone(), font, visuals.text_color(), cell);
                    let pos = egui::pos2(rect.center().x - galley.size().x / 2.0, image_rect.bottom() + 2.0);
                    ui.painter().with_clip_rect(rect).galley(pos, galley, visuals.text_color());
                }
                if response.on_hover_text(size).clicked() {
                    clicked = Some(i);
                }
            }
        });
    });
    clicked
}

//endregion

/*┌─────────────────╮
| Inspector       |
└─────────────────╯*/
//region Inspector

/// A short description of the document for the status bar.
pub fn summary(doc: &Document) -> String {
    match &doc.content {
        Content::Scene(v) => {
            let s = &v.scene;
            let mut parts = match &s.splats {
                Some(sp) if s.meshes.is_empty() => vec![format!("{} splats", group(sp.len() as u64))],
                _ => vec![format!("{} triangles", group(s.drawn_triangle_count()))],
            };
            if !s.animations.is_empty() {
                parts.push(format!("{} clips", s.animations.len()));
            }
            parts.join(", ")
        }
        Content::Material(m) => format!("material {}", m.asset.name),
        Content::Texture(t) => format!("{} × {}", t.texture.width(), t.texture.height()),
        Content::Gallery(g) => format!("{} pictures", g.items.len()),
        Content::Canvas(c) => c.summary(),
        Content::Lut(l) => format!("graded {}", l.picture_name()),
        Content::Palette(p) => p.summary(),
        Content::Dicom(d) => crate::dicom_view::summary(d),
        Content::Volume(v) => crate::volume_view::summary(v),
        Content::Cad(c) => c.summary(),
        Content::Pages(p) => p.summary(),
    }
}

/// 129388 as "129,388".
fn group(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// The Inspector panel: what the file is and holds.  `dock` frames it.
pub(crate) fn inspector_contents(v: &mut Viewer, ui: &mut Ui) {
    let Viewer { doc, gfx, ui: state, actions, .. } = v;
    if let Some(doc) = doc {
        section(ui, "File", true, |ui| file_section(ui, doc));
        // A document's header facts are in the File section already.
        if let Content::Texture(t) = &doc.content
            && !matches!(doc.kind, slerp_core::Kind::Document(_))
        {
            crate::stretch_view::header(ui, &t.texture);
        }
        if let Content::Dicom(d) = &mut doc.content {
            crate::dicom_view::inspector(ui, d, actions);
        }
        if let Content::Volume(v) = &doc.content {
            crate::volume_view::inspector(ui, v);
        }
        if let Some(view) = doc.scene_mut() {
            section(ui, "Statistics", true, |ui| stats(ui, view, gfx));
            section(ui, "Scene", false, |ui| tree(ui, view, &mut state.tree_filter, &mut state.show_all_nodes));
            if !view.scene.materials.is_empty() {
                section(ui, "Materials", false, |ui| materials(ui, view, &mut state.thumbs));
            }
            if !view.scene.textures.is_empty() {
                section(ui, "Textures", false, |ui| textures(ui, &view.scene));
            }
        }
    }
    section(ui, "GPU", false, |ui| {
        let g = &gfx.gpu;
        form(ui, "gpu-form", |ui| {
            for (k, val) in [
                ("Device", g.info.name.clone()),
                ("Driver", format!("{} {}", g.info.driver, g.info.driver_info)),
                ("Chosen as", g.chosen_by.clone()),
                ("Started in", format!("{:.0} ms", g.startup.as_secs_f64() * 1000.0)),
                ("View frames", gfx.draws.to_string()),
            ] {
                ui.label(egui::RichText::new(k).weak());
                ui.label(val);
                ui.end_row();
            }
        });
    });
}

fn file_section(ui: &mut Ui, doc: &Document) {
    form(ui, "file-form", |ui| {
        let rows: Vec<(String, String)> = [
            ("Name".to_string(), doc.name()),
            (
                "Folder".to_string(),
                match &doc.origin {
                    // An archive entry lives in a temporary file; say where it really is.
                    Some(o) => format!("In {}", short_name(&o.archive)),
                    None => doc.path.parent().map(|p| p.display().to_string()).unwrap_or_default(),
                },
            ),
            ("Size".to_string(), bytes(doc.file_bytes)),
            ("Read in".to_string(), format!("{:.0} ms", doc.load_time.as_secs_f64() * 1000.0)),
            ("Uploaded in".to_string(), format!("{:.0} ms", doc.upload_time.as_secs_f64() * 1000.0)),
        ]
        .into_iter()
        .chain(doc.facts.iter().cloned())
        .collect();
        for (k, val) in rows {
            ui.label(egui::RichText::new(k).weak());
            ui.add(egui::Label::new(val).wrap());
            ui.end_row();
        }
    });
    if doc.parts.len() > 1 {
        ui.add_space(4.0);
        ui.label(egui::RichText::new("Size by part").text_style(egui::TextStyle::Name("Bold".into())));
        let total: u64 = doc.parts.iter().map(|p| p.1).sum::<u64>().max(1);
        form(ui, "parts", |ui| {
            for (k, n) in &doc.parts {
                ui.label(k);
                ui.label(format!("{}  ({:.0} %)", bytes(*n), *n as f64 * 100.0 / total as f64));
                ui.end_row();
            }
        });
    }
    if !doc.warnings.is_empty() {
        crate::sections::section(ui, "Warnings", format!("Warnings ({})", doc.warnings.len()), false, |ui| {
            for w in &doc.warnings {
                ui.add(egui::Label::new(egui::RichText::new(w).color(ui.visuals().warn_fg_color)).wrap());
            }
        });
    }
}

fn stats(ui: &mut Ui, view: &SceneView, gfx: &crate::gfx::Gfx) {
    let s = &view.scene;
    let visible = view.effective_visibility();
    let draws: usize = s
        .nodes
        .iter()
        .enumerate()
        .filter(|(i, _)| visible.get(*i).copied().unwrap_or(false))
        .filter_map(|(_, n)| n.mesh.and_then(|m| s.meshes.get(m)))
        .map(|m| m.primitives.len())
        .sum();
    let joints: std::collections::HashSet<usize> = s.skins.iter().flat_map(|k| k.joints.iter().copied()).collect();
    let tex_bytes: u64 = s.textures.iter().map(|t| texture_bytes(t)).sum();
    let morphs = view.morph_list.names.len() + view.morph_list.unnamed.iter().map(|u| u.2).sum::<usize>();
    let _ = gfx;
    // A splat capture holds nothing else, so its mesh rows would be noise.
    let mut rows: Vec<(&str, String)> = Vec::new();
    if let Some(sp) = s.splats.as_ref().filter(|sp| !sp.is_empty()) {
        let drawn = view.gpu.splat_count();
        let count = if drawn < sp.len() {
            format!("{} ({} drawn)", group(sp.len() as u64), group(drawn as u64))
        } else {
            group(sp.len() as u64)
        };
        rows.push(("Splats", count));
        rows.push(("Splat memory", format!("{} on the GPU", bytes(view.gpu.splat_bytes()))));
        if s.meshes.is_empty() {
            rows.push(("GPU memory", bytes(view.gpu.gpu_bytes())));
            form(ui, "stats", |ui| {
                for (k, val) in rows {
                    ui.label(egui::RichText::new(k).weak());
                    ui.label(val);
                    ui.end_row();
                }
            });
            return;
        }
    }
    form(ui, "stats", |ui| {
        for (k, val) in rows.into_iter().chain([
            ("Triangles", {
                let (drawn, stored) = (s.drawn_triangle_count(), s.triangle_count() as u64);
                if drawn == stored { group(drawn) } else { format!("{} ({} stored)", group(drawn), group(stored)) }
            }),
            ("Vertices", group(s.vertex_count() as u64)),
            ("Draw calls", group(draws as u64)),
            ("Nodes", group(s.nodes.len() as u64)),
            (
                "Meshes",
                format!(
                    "{} ({} primitives)",
                    s.meshes.len(),
                    s.meshes.iter().map(|m| m.primitives.len()).sum::<usize>()
                ),
            ),
            ("Materials", s.materials.len().to_string()),
            ("Textures", format!("{} ({} decoded)", s.textures.len(), bytes(tex_bytes))),
            ("GPU memory", bytes(view.gpu.gpu_bytes())),
            ("Skins", format!("{} ({} joints)", s.skins.len(), joints.len())),
            ("Morph targets", morphs.to_string()),
            ("Clips", s.animations.len().to_string()),
            ("Expressions", s.expressions.len().to_string()),
            ("Markers", s.markers.len().to_string()),
        ]) {
            ui.label(egui::RichText::new(k).weak());
            ui.label(val);
            ui.end_row();
        }
    });
}

fn texture_bytes(t: &slerp_scene::Texture) -> u64 {
    t.levels
        .iter()
        .flat_map(|l| &l.surfaces)
        .map(|p| match p {
            slerp_scene::textures::Pixels::Rgba8(v) => v.len() as u64,
            slerp_scene::textures::Pixels::Rgba32F(v) => v.len() as u64 * 4,
            slerp_scene::textures::Pixels::Blocks(_, v) => v.len() as u64,
        })
        .sum()
}

fn tree(ui: &mut Ui, view: &mut SceneView, filter: &mut String, show_all: &mut bool) {
    ui.add(widgets::text_field(ui.ctx(), filter).hint_text("Find a node").desired_width(f32::INFINITY));
    check(ui, show_all, "Show joints and empty nodes");
    let s = &view.scene;
    // Which nodes hold a mesh at or below them, computed leaves first.
    let n = s.nodes.len();
    let mut has_mesh = vec![false; n];
    let parents = s.parents();
    let mut order: Vec<usize> = Vec::with_capacity(n);
    let mut stack: Vec<usize> = s.roots.clone();
    let mut seen = vec![false; n];
    while let Some(i) = stack.pop() {
        if i < n && !seen[i] {
            seen[i] = true;
            order.push(i);
            stack.extend(s.nodes[i].children.iter().copied());
        }
    }
    for &i in order.iter().rev() {
        if s.nodes[i].mesh.is_some() {
            has_mesh[i] = true;
        }
        if has_mesh[i]
            && let Some(p) = parents[i]
        {
            has_mesh[p] = true;
        }
    }
    let mut changes: Vec<(usize, bool)> = Vec::new();
    let f = filter.to_lowercase();
    egui::ScrollArea::vertical().id_salt("tree").max_height(420.0).show(ui, |ui| {
        if !f.is_empty() {
            for (i, node) in s.nodes.iter().enumerate() {
                if node.name.to_lowercase().contains(&f) && (*show_all || has_mesh[i]) {
                    node_row(ui, s, i, &mut changes);
                }
            }
        } else {
            let mut visited = vec![false; n];
            for &r in &s.roots {
                node_tree(ui, s, r, 0, *show_all, &has_mesh, &mut visited, &mut changes);
            }
        }
    });
    for (i, vis) in changes {
        view.set_visible(i, vis);
    }
}

#[allow(clippy::too_many_arguments)]
fn node_tree(
    ui: &mut Ui,
    s: &Scene,
    i: usize,
    depth: usize,
    show_all: bool,
    has_mesh: &[bool],
    visited: &mut [bool],
    changes: &mut Vec<(usize, bool)>,
) {
    if i >= s.nodes.len() || visited[i] || !(show_all || has_mesh[i]) {
        return;
    }
    visited[i] = true;
    let node = &s.nodes[i];
    let kids: Vec<usize> =
        node.children.iter().copied().filter(|&c| c < s.nodes.len() && (show_all || has_mesh[c])).collect();
    if kids.is_empty() {
        node_row(ui, s, i, changes);
        return;
    }
    let id = ui.make_persistent_id(("node", i));
    egui::collapsing_header::CollapsingState::load_with_default_open(ui.ctx(), id, depth < 1)
        .show_header(ui, |ui| node_row(ui, s, i, changes))
        .body(|ui| {
            for c in kids {
                node_tree(ui, s, c, depth + 1, show_all, has_mesh, visited, changes);
            }
        });
}

fn node_row(ui: &mut Ui, s: &Scene, i: usize, changes: &mut Vec<(usize, bool)>) {
    let node = &s.nodes[i];
    let mut visible = node.visible;
    let kind = if node.skin.is_some() {
        "skinned mesh"
    } else if node.mesh.is_some() {
        "mesh"
    } else if s.skins.iter().any(|k| k.joints.contains(&i)) {
        "joint"
    } else {
        "node"
    };
    let name = if node.name.is_empty() { format!("#{i}") } else { node.name.clone() };
    let tip = match node.mesh.and_then(|m| s.meshes.get(m)) {
        Some(m) => format!(
            "{kind}: {} triangles, {} primitives",
            group(m.primitives.iter().map(|p| p.triangle_count() as u64).sum()),
            m.primitives.len()
        ),
        None => kind.to_string(),
    };
    if ui.add(widgets::CheckBox::new(&mut visible, name)).on_hover_text(tip).changed() {
        changes.push((i, visible));
    }
}

fn materials(
    ui: &mut Ui,
    view: &mut SceneView,
    thumbs: &mut std::collections::HashMap<(u64, usize), egui::TextureHandle>,
) {
    let s = &view.scene;
    egui::ScrollArea::vertical().id_salt("materials").max_height(200.0).show(ui, |ui| {
        let mut list = widgets::List::new(ui, "materials");
        for (i, m) in s.materials.iter().enumerate() {
            let name = if m.name.is_empty() { format!("Material {i}") } else { m.name.clone() };
            if list.row(ui, i, view.selected_material == Some(i), name).clicked() {
                view.selected_material = Some(i);
            }
        }
        if let Some(i) = list.finish(ui) {
            view.selected_material = Some(i);
        }
    });
    let Some(m) = view.selected_material.and_then(|i| s.materials.get(i)) else {
        ui.label(egui::RichText::new("Pick a material to see its channels.").weak());
        return;
    };
    ui.separator();
    // One thumbnail made per frame at most, so opening a material with
    // several 4K maps doesn't stall the window.
    let mut budget = 1;
    let ctx = ui.ctx().clone();
    let mut slot = |ui: &mut Ui, label: &str, r: &Option<TexRef>| {
        let Some(r) = r else { return };
        let key = (view.id, r.texture);
        if !thumbs.contains_key(&key)
            && budget > 0
            && let Some(t) = s.textures.get(r.texture)
        {
            budget -= 1;
            let img = slerp_scene::textures::thumbnail(t, 128);
            let color =
                egui::ColorImage::from_rgba_unmultiplied([img.width as usize, img.height as usize], &img.pixels);
            thumbs.insert(
                key,
                ctx.load_texture(format!("mat-{}-{}", view.id, r.texture), color, egui::TextureOptions::LINEAR),
            );
        }
        ui.label(egui::RichText::new(label).weak());
        match thumbs.get(&key) {
            Some(h) => {
                let t = &s.textures[r.texture];
                let info = format!(
                    "{} × {}, {} {}, UV set {}",
                    t.width(),
                    t.height(),
                    t.source.container,
                    t.source.format,
                    r.uv_set
                );
                ui.add(egui::Image::new(h).fit_to_exact_size(egui::vec2(64.0, 64.0))).on_hover_ui(|ui| {
                    ui.add(egui::Image::new(h).fit_to_exact_size(egui::vec2(256.0, 256.0)));
                    ui.label(&info);
                });
            }
            None => {
                crate::app::spinner(ui, 16.0);
                ctx.request_repaint();
            }
        }
        ui.end_row();
    };
    form(ui, "material-detail", |ui| {
        let row = |ui: &mut Ui, k: &str, v: String| {
            ui.label(egui::RichText::new(k).weak());
            ui.label(v);
            ui.end_row();
        };
        let c = m.base_color;
        ui.label(egui::RichText::new("Base colour").weak());
        let color = egui::Color32::from(egui::Rgba::from_rgba_unmultiplied(c.x, c.y, c.z, c.w));
        let (rect, _) = ui.allocate_exact_size(egui::vec2(40.0, 16.0), egui::Sense::hover());
        ui.painter().rect_filled(rect, 3.0, color);
        ui.end_row();
        slot(ui, "Base colour map", &m.base_color_texture);
        row(ui, "Metallic", format!("{:.2}", m.metallic));
        row(ui, "Roughness", format!("{:.2}", m.roughness));
        slot(ui, "Metal/rough map", &m.metallic_roughness_texture);
        slot(ui, "Normal map", &m.normal_texture);
        slot(ui, "Occlusion map", &m.occlusion_texture);
        if m.emissive != glam::Vec3::ZERO || m.emissive_texture.is_some() {
            row(ui, "Emissive", format!("{:.2} {:.2} {:.2}", m.emissive.x, m.emissive.y, m.emissive.z));
            slot(ui, "Emissive map", &m.emissive_texture);
        }
        slot(ui, "Height map", &m.height_texture);
        slot(ui, "Lightmap", &m.lightmap_texture);
        row(ui, "Alpha", alpha_name(m));
        row(ui, "Sides", if m.double_sided { "both".into() } else { "front".into() });
        if m.unlit {
            row(ui, "Shading", "unlit".into());
        }
        if m.clearcoat > 0.0 {
            row(ui, "Clearcoat", format!("{:.2}, roughness {:.2}", m.clearcoat, m.clearcoat_roughness));
        }
        if m.ior != 1.5 || m.specular != 1.0 {
            row(ui, "Specular", format!("{:.2}, IOR {:.2}", m.specular, m.ior));
        }
        if m.effect != Effect::None {
            row(ui, "Effect", format!("{:?}", m.effect));
        }
    });
    ui.label(egui::RichText::new("Display ▸ Show picks one channel for the whole scene.").weak().small());
}

fn alpha_name(m: &Material) -> String {
    match m.alpha_mode {
        AlphaMode::Opaque => "opaque".into(),
        AlphaMode::Mask(c) => format!("cut out below {c:.2}"),
        AlphaMode::Blend => "blended".into(),
    }
}

fn textures(ui: &mut Ui, s: &Scene) {
    egui::ScrollArea::vertical().id_salt("textures").max_height(260.0).show(ui, |ui| {
        form(ui, "texture-list", |ui| {
            for (i, t) in s.textures.iter().enumerate() {
                ui.label(egui::RichText::new(format!("{i}")).weak());
                let format =
                    if t.source.format.is_empty() { t.source.container.clone() } else { t.source.format.clone() };
                ui.label(format!("{} × {}  {}  {}", t.width(), t.height(), format, bytes(texture_bytes(t))));
                ui.end_row();
            }
        });
    });
}

//endregion

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::heading2;

    #[test]
    fn words_split_camel_case() {
        assert_eq!(words("browDownLeft"), "Brow down left");
        assert_eq!(words("blink_l"), "Blink l");
        assert_eq!(words("aa"), "Aa");
    }

    #[test]
    fn groups_thousands() {
        assert_eq!(group(129388), "129,388");
        assert_eq!(group(7), "7");
        assert_eq!(group(1000), "1,000");
    }

    #[test]
    fn heading_style_name() {
        assert_eq!(heading2(), egui::TextStyle::Name("Heading2".into()));
    }
}
