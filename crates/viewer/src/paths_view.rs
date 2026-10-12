//! G-code toolpaths and GPS tracks in the viewer (`slerp_core::paths`).
//!
//! A toolpath opens with one node per layer, so the layer slider only
//! switches nodes on and off; the feature and travel toggles rebuild the
//! meshes.  A track opens in 3D over its own elevation; the elevation is
//! exaggerated through the root node's scale, so its slider moves smoothly
//! without rebuilding, while the colour and curtain choices rebuild.  The
//! elevation profile sits in a panel below the view: hovering it marks the
//! point on the 3D track, and hovering the track marks the profile.
//! Statistics reach the inspector through the scene's details.
//!
//! For screenshots, `SLERP_PATHS_LAYER=<n>` shows a toolpath up to layer n
//! (from 1), and `SLERP_PATHS_HOVER=<f>` marks the track point a fraction f
//! of the way along, as hovering the profile there does.

use crate::document::{Content, Document};
use crate::gfx::Gfx;
use crate::scene_view::{SceneView, project};
use egui::{Color32, Pos2, Rect, Slider, Stroke, Ui, vec2};
use glam::Vec3;
use slerp_core::paths::{
    ColorBy, PathData, Target, Toolpath, ToolpathLook, Track, TrackLook, format_duration, metric_color, toolpath_scene,
};
use slerp_scene::View;
use slerp_theme::widgets;

/// What a G-code or track file shows and how.
pub enum PathsState {
    Toolpath(Box<ToolpathState>),
    Track(Box<TrackState>),
}

impl PathsState {
    pub fn new(data: PathData) -> PathsState {
        match data {
            PathData::Toolpath(tp) => {
                let tp = *tp;
                let last = tp.layers.len().saturating_sub(1);
                let asked = crate::env::var("SLERP_PATHS_LAYER").and_then(|v| v.trim().parse::<usize>().ok());
                let top = asked.map_or(last, |n| n.saturating_sub(1).min(last));
                PathsState::Toolpath(Box::new(ToolpathState {
                    tp,
                    look: ToolpathLook::all(),
                    built: ToolpathLook::all(),
                    top,
                    only: false,
                    applied: None,
                }))
            }
            PathData::Track(track) => {
                let track = *track;
                let look = TrackLook::for_profile(&track.profile);
                let base = slerp_core::paths::track::base_elevation(&track.set, &track.profile);
                let mut last = track.profile.ele.iter().copied().find(|e| e.is_finite()).unwrap_or(base);
                let heights = track
                    .profile
                    .ele
                    .iter()
                    .map(|&e| {
                        if e.is_finite() {
                            last = e;
                        }
                        last - base
                    })
                    .collect();
                let mut state = TrackState {
                    track,
                    look,
                    built: look,
                    show_profile: true,
                    base,
                    heights,
                    colors: Vec::new(),
                    range: None,
                    colors_for: None,
                    chart_hover: None,
                    view_hover: None,
                    framed: false,
                    pinned: None,
                    chart: None,
                    available: Vec::new(),
                    has_height: false,
                };
                state.has_height = state.track.profile.has_elevation();
                state.available = ColorBy::available(&state.track.profile);
                state.chart = ChartData::of(&state.track.profile).map(std::sync::Arc::new);
                if let Some(f) = crate::env::var("SLERP_PATHS_HOVER").and_then(|v| v.trim().parse::<f64>().ok()) {
                    let p = &state.track.profile;
                    let d = p.dist.last().copied().unwrap_or(0.0) * f.clamp(0.0, 1.0);
                    state.pinned = Some(p.dist.partition_point(|&x| x < d).min(p.len().saturating_sub(1)));
                }
                state.refresh_colors();
                PathsState::Track(Box::new(state))
            }
        }
    }
}

pub struct ToolpathState {
    pub tp: Toolpath,
    pub look: ToolpathLook,
    /// What the scene was built with.
    built: ToolpathLook,
    /// The highest layer shown, from 0.
    pub top: usize,
    /// Show only the layer `top`.
    pub only: bool,
    /// The layer choice the nodes' visibility reflects.
    applied: Option<(usize, bool)>,
}

pub struct TrackState {
    pub track: Track,
    pub look: TrackLook,
    built: TrackLook,
    pub show_profile: bool,
    /// The elevation heights are measured from.
    base: f32,
    /// Each profile point's height above `base`, gaps filled, as the scene
    /// draws it before the exaggeration.
    heights: Vec<f32>,
    /// Each profile point's colour (sRGB) and the range the scale spans.
    colors: Vec<[f32; 3]>,
    range: Option<(f32, f32)>,
    colors_for: Option<ColorBy>,
    /// The profile point under the pointer in the chart, and in the view.
    chart_hover: Option<usize>,
    view_hover: Option<usize>,
    /// The camera has been set to look down on the track.
    framed: bool,
    /// A point marked by `SLERP_PATHS_HOVER`, for screenshots.
    pinned: Option<usize>,
    /// What the profile plots, made once rather than every frame.
    chart: Option<std::sync::Arc<ChartData>>,
    /// The metrics the track can be coloured by.
    available: Vec<ColorBy>,
    /// The elevation varies, so heights and the curtain mean something.
    has_height: bool,
}

/// The values a profile chart plots: elevation, or speed for a track
/// without heights.
struct ChartData {
    values: Vec<f32>,
    unit: &'static str,
    name: &'static str,
    /// The range of `values`.
    lo: f32,
    hi: f32,
}

impl ChartData {
    fn of(p: &slerp_core::paths::Profile) -> Option<ChartData> {
        let (values, unit, name) = if p.has_elevation() {
            (p.ele.clone(), "m", "Elevation")
        } else {
            (ColorBy::Speed.values(p)?, "km/h", "Speed")
        };
        let (lo, hi) = slerp_core::paths::track::value_range(&values);
        Some(ChartData { values, unit, name, lo, hi })
    }
}

impl TrackState {
    fn refresh_colors(&mut self) {
        if self.colors_for != Some(self.look.color_by) {
            let (colors, range) = slerp_core::paths::track::point_colors(&self.track.profile, self.look.color_by);
            self.colors = colors;
            self.range = range;
            self.colors_for = Some(self.look.color_by);
        }
    }

    /// Where profile point `i` is in the scene, exaggerated.
    fn point(&self, i: usize) -> Vec3 {
        let p = self.track.profile.pos[i];
        Vec3::new(p[0], self.heights[i] * self.look.exaggeration, -p[1])
    }

    fn hovered(&self) -> Option<usize> {
        self.chart_hover.or(self.view_hover).or(self.pinned).filter(|&i| i < self.track.profile.len())
    }

    /// What is known at profile point `i`, for the hover text.  The short
    /// form (distance, elevation, speed, time) fits beside the pointer in
    /// the 3D view; the full one adds the sensors, for the chart.
    fn describe(&self, i: usize, full: bool) -> String {
        let p = &self.track.profile;
        let mut parts = vec![slerp_core::paths::track::format_distance(p.dist[i])];
        if p.ele[i].is_finite() {
            parts.push(format!("{:.0} m", p.ele[i]));
        }
        if p.speed[i].is_finite() {
            parts.push(format!("{:.1} km/h", p.speed[i] * 3.6));
        }
        if full && p.hr[i].is_finite() {
            parts.push(format!("{:.0} bpm", p.hr[i]));
        }
        if full && p.cad[i].is_finite() {
            parts.push(format!("{:.0} rpm", p.cad[i]));
        }
        if full && p.power[i].is_finite() {
            parts.push(format!("{:.0} W", p.power[i]));
        }
        let start = p.time.iter().copied().find(|t| t.is_finite());
        if let (Some(s), true) = (start, p.time[i].is_finite()) {
            parts.push(format_duration(p.time[i] - s));
        }
        parts.join("  ·  ")
    }
}

fn srgb(c: [f32; 3], a: f32) -> Color32 {
    let b = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    Color32::from_rgba_unmultiplied(b(c[0]), b(c[1]), b(c[2]), b(a))
}

/*┌─────────────────╮
| Scene updates   |
└─────────────────╯*/
//region Scene updates

/// Applies what the panels changed: rebuilds the meshes after a toggle,
/// shows the chosen layers, and sets the elevation's exaggeration.
pub fn update(view: &mut SceneView, gfx: &mut Gfx) {
    let Some(mut state) = view.paths.take() else { return };
    // 1. Rebuilds.
    let rebuilt = match &mut state {
        PathsState::Toolpath(t) if t.look != t.built => {
            t.built = t.look.clone();
            t.applied = None;
            Some(toolpath_scene(&t.tp, &t.look, Target::Viewer))
        }
        PathsState::Track(t) if (t.look.color_by, t.look.curtain) != (t.built.color_by, t.built.curtain) => {
            t.built = t.look;
            t.refresh_colors();
            Some(t.track.scene(&t.look, false))
        }
        _ => None,
    };
    if let Some(scene) = rebuilt {
        let mut fresh = SceneView::new(scene, gfx, 1.0);
        std::mem::swap(&mut fresh.cameras, &mut view.cameras);
        *view = fresh;
    }
    // 2. Layers and exaggeration, which only change the pose.
    match &mut state {
        PathsState::Toolpath(t) => {
            let want = (t.top, t.only);
            if t.applied != Some(want) {
                for i in 0..view.scene.nodes.len() {
                    view.set_visible(i, if t.only { i == t.top } else { i <= t.top });
                }
                t.applied = Some(want);
            }
        }
        PathsState::Track(t) => {
            if let Some(root) = view.scene.nodes.first_mut()
                && root.transform.scale.y != t.look.exaggeration
            {
                root.transform.scale.y = t.look.exaggeration;
                view.pose_dirty = true;
            }
            // A route is wide and low: seen from higher than a model, its
            // shape shows rather than its edge.
            if !t.framed {
                let mut orbit = view.cameras.orbit;
                orbit.pitch = 40f32.to_radians();
                orbit.frame(&view.frame_bounds(), 1.0);
                view.cameras.set_home_orbit(orbit);
                t.framed = true;
            }
        }
    }
    view.paths = Some(state);
}

//endregion

/*┌─────────────────╮
| Controls        |
└─────────────────╯*/
//region Controls

/// The sections in the right-hand panel.
pub fn controls(ui: &mut Ui, view: &mut SceneView) {
    match view.paths.as_mut() {
        Some(PathsState::Toolpath(t)) => {
            let title = if t.tp.cnc { "Toolpath" } else { "Print" };
            crate::sections::section(ui, "Toolpath", title, true, |ui| toolpath_controls(ui, t));
        }
        Some(PathsState::Track(t)) => {
            crate::sections::section(ui, "Route", "Route", true, |ui| track_controls(ui, t));
        }
        None => {}
    }
}

fn form(ui: &mut Ui, id: &str, add: impl FnOnce(&mut Ui)) {
    egui::Grid::new(id).num_columns(2).spacing([8.0, 4.0]).show(ui, add);
}

fn toolpath_controls(ui: &mut Ui, t: &mut ToolpathState) {
    let n = t.tp.layers.len();
    if n > 1 && !t.tp.cnc {
        form(ui, "toolpath-layers", |ui| {
            ui.label("Layers:");
            let mut shown = t.top + 1;
            ui.scope(|ui| {
                ui.set_min_width(ui.available_width());
                widgets::slider(ui, Slider::new(&mut shown, 1..=n).integer().prefix("up to "));
            });
            t.top = shown.clamp(1, n) - 1;
            ui.end_row();
            ui.label("");
            let z = t.tp.layers[t.top].z;
            ui.label(egui::RichText::new(format!("Layer {} of {n} at {z:.2} mm", t.top + 1)).weak());
            ui.end_row();
        });
        ui.add(widgets::CheckBox::new(&mut t.only, "Only this layer"));
        ui.add_space(4.0);
    }
    let total: f64 = t.tp.feature_length.iter().sum::<f64>().max(1e-9);
    for f in t.tp.features() {
        ui.horizontal(|ui| {
            let (rect, _) = ui.allocate_exact_size(vec2(12.0, 12.0), egui::Sense::hover());
            let [r, g, b] = f.srgb();
            ui.painter().rect_filled(rect, 2.0, Color32::from_rgb(r, g, b));
            let share = t.tp.feature_length[f.index()] / total * 100.0;
            let label = format!("{}  ({share:.0} %)", f.name());
            ui.add(widgets::CheckBox::new(&mut t.look.shown[f.index()], label));
        });
    }
    let travel = if t.tp.cnc { "Rapid moves" } else { "Travel moves" };
    ui.horizontal(|ui| {
        let (rect, _) = ui.allocate_exact_size(vec2(12.0, 12.0), egui::Sense::hover());
        ui.painter()
            .line_segment([rect.left_center(), rect.right_center()], Stroke::new(1.5, Color32::from_rgb(40, 120, 250)));
        ui.add(widgets::CheckBox::new(&mut t.look.travel, travel));
    });
}

fn track_controls(ui: &mut Ui, t: &mut TrackState) {
    let has_height = t.has_height;
    form(ui, "track-form", |ui| {
        ui.label("Colour by:");
        let items = t.available.iter().map(|c| (*c, c.name().to_string()));
        widgets::Choice::new("track-color-by", &mut t.look.color_by, items).width(160.0).show(ui);
        ui.end_row();
        if has_height {
            ui.label("Height:");
            ui.scope(|ui| {
                ui.set_min_width(ui.available_width());
                widgets::slider(
                    ui,
                    Slider::new(&mut t.look.exaggeration, 1.0..=25.0).logarithmic(true).suffix("×").fixed_decimals(1),
                );
            });
            ui.end_row();
        }
    });
    if has_height {
        ui.add(widgets::CheckBox::new(&mut t.look.curtain, "Curtain to the lowest point"));
    }
    ui.add(widgets::CheckBox::new(&mut t.show_profile, "Profile below the view"));
    // The colour scale.
    t.refresh_colors();
    if let Some((lo, hi)) = t.range {
        ui.add_space(4.0);
        let width = ui.available_width().min(260.0);
        let (rect, _) = ui.allocate_exact_size(vec2(width, 10.0), egui::Sense::hover());
        let steps = 48;
        for k in 0..steps {
            let x0 = rect.left() + rect.width() * k as f32 / steps as f32;
            let x1 = rect.left() + rect.width() * (k + 1) as f32 / steps as f32;
            let c = metric_color((k as f32 + 0.5) / steps as f32);
            ui.painter().rect_filled(
                Rect::from_min_max(egui::pos2(x0, rect.top()), egui::pos2(x1 + 0.5, rect.bottom())),
                0.0,
                srgb(c, 1.0),
            );
        }
        let unit = t.look.color_by.unit();
        ui.horizontal(|ui| {
            ui.set_width(width);
            ui.label(egui::RichText::new(format!("{lo:.0} {unit}")).weak());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(egui::RichText::new(format!("{hi:.0} {unit}")).weak());
            });
        });
    }
}

//endregion

/*┌─────────────────╮
| The 3D view     |
└─────────────────╯*/
//region The 3D view

/// The track point nearest the pointer in the view (within 12 points),
/// which the profile marks, and its description for the hover text.
pub fn hover(ctx: &egui::Context, view: &mut SceneView, v: &View, rect: Rect, pointer: Option<Pos2>) -> Option<String> {
    let Some(PathsState::Track(t)) = view.paths.as_mut() else { return None };
    let before = t.view_hover.take();
    let found = pointer.and_then(|pointer| nearest(t, v, rect, pointer));
    // The profile is laid out before the view, so it needs another frame
    // to show where the pointer went.
    t.view_hover = found;
    if found != before {
        ctx.request_repaint();
    }
    found.map(|i| t.describe(i, false))
}

/// The track point nearest `pointer` in the view, within 12 points.
fn nearest(t: &TrackState, v: &View, rect: Rect, pointer: Pos2) -> Option<usize> {
    let vp = v.view_proj();
    let n = t.track.profile.len();
    // A long track is searched at a stride, then finely around the best.
    let step = (n / 100_000).max(1);
    let near = |i: usize, best: &mut (f32, Option<usize>)| {
        if let Some(s) = project(&vp, t.point(i), rect) {
            let d = s.distance_sq(pointer);
            if d < best.0 {
                *best = (d, Some(i));
            }
        }
    };
    let mut best = (144.0f32, None);
    for i in (0..n).step_by(step) {
        near(i, &mut best);
    }
    if step > 1
        && let Some(c) = best.1
    {
        for i in c.saturating_sub(step)..(c + step).min(n) {
            near(i, &mut best);
        }
    }
    best.1
}

/// Marks over the view: a track's start, end and waypoints, and the point
/// hovered here or in the profile.
pub fn overlay(painter: &egui::Painter, rect: Rect, view: &SceneView, aspect: f32) {
    let Some(PathsState::Track(t)) = view.paths.as_ref() else { return };
    let vp = view.cameras.view(aspect).view_proj();
    let n = t.track.profile.len();
    let ring = Stroke::new(1.5, Color32::WHITE);
    // 1. Waypoints, named when there are few.
    let few = t.track.set.waypoints.len() <= 40;
    let font = egui::FontId::proportional(11.0);
    for w in &t.track.set.waypoints {
        let (e, north) = t.track.projection.project(w.lat, w.lon);
        let y = if w.ele.is_finite() { (w.ele - t.base) * t.look.exaggeration } else { 0.0 };
        if let Some(s) = project(&vp, Vec3::new(e as f32, y, -(north as f32)), rect) {
            painter.circle(s, 3.5, Color32::from_rgb(250, 250, 250), Stroke::new(1.0, Color32::from_black_alpha(200)));
            if few && !w.name.is_empty() {
                painter.text(s + vec2(6.0, -6.0), egui::Align2::LEFT_BOTTOM, &w.name, font.clone(), Color32::WHITE);
            }
        }
    }
    // 2. Start and end.
    if n > 0 {
        let start = project(&vp, t.point(0), rect);
        if let Some(s) = project(&vp, t.point(n - 1), rect) {
            // A loop's end would hide under its start: a larger red disc
            // shows around it instead.
            let loop_end = start.is_some_and(|a| a.distance(s) < 10.0);
            painter.circle(s, if loop_end { 9.5 } else { 6.0 }, Color32::from_rgb(215, 30, 30), ring);
        }
        if let Some(s) = start {
            painter.circle(s, 5.0, Color32::from_rgb(40, 190, 65), ring);
        }
    }
    // 3. The hovered point, with a line down to the ground.
    if let Some(i) = t.hovered().filter(|&i| i < n) {
        let top = t.point(i);
        let ground = Vec3::new(top.x, 0.0, top.z);
        if let (Some(a), Some(b)) = (project(&vp, top, rect), project(&vp, ground, rect)) {
            painter.line_segment([a, b], Stroke::new(1.0, Color32::from_white_alpha(160)));
        }
        if let Some(s) = project(&vp, top, rect) {
            painter.circle(s, 6.0, Color32::from_white_alpha(40), Stroke::new(2.0, Color32::WHITE));
        }
    }
}

//endregion

/*┌─────────────────╮
| Profile         |
└─────────────────╯*/
//region Profile

/// The elevation profile (or speed, for a track without heights) in a
/// panel below the view, for an open track that has one.
pub fn profile_panel(ui: &mut Ui, doc: Option<&mut Document>) {
    let Some(Document { content: Content::Scene(view), .. }) = doc else { return };
    let Some(PathsState::Track(t)) = view.paths.as_mut() else { return };
    if !t.show_profile || t.track.profile.len() < 2 || t.chart.is_none() {
        t.chart_hover = None;
        return;
    }
    let ctx = ui.ctx().clone();
    egui::Panel::bottom("track-profile")
        .frame(widgets::side_panel_frame(&ctx))
        .resizable(true)
        .default_size(150.0)
        .min_size(80.0)
        .show(ui, |ui| chart(ui, t));
}

fn chart(ui: &mut Ui, t: &mut TrackState) {
    let Some(data) = t.chart.clone() else { return };
    let (values, unit, name) = (&data.values, data.unit, data.name);
    t.refresh_colors();
    let p = &t.track.profile;
    let rect = ui.available_rect_before_wrap();
    let response = ui.allocate_rect(rect, egui::Sense::hover());
    let painter = ui.painter_at(rect);
    let text = ui.visuals().text_color();
    let weak = ui.visuals().weak_text_color();
    let font = egui::TextStyle::Small.resolve(ui.style());
    let plot = Rect::from_min_max(rect.min + vec2(52.0, 10.0), rect.max - vec2(14.0, 22.0));
    if plot.width() < 20.0 || plot.height() < 10.0 {
        return;
    }
    let total = p.dist.last().copied().unwrap_or(0.0).max(1e-6);
    let (lo, hi) = if data.hi > data.lo { (data.lo, data.hi) } else { (data.lo - 1.0, data.lo + 1.0) };
    let pad = (hi - lo) * 0.08;
    let (lo, hi) = (if unit == "m" { lo - pad } else { 0.0f32.min(lo) }, hi + pad);
    let x_of = |d: f64| plot.left() + (d / total) as f32 * plot.width();
    let y_of = |v: f32| plot.bottom() - (v - lo) / (hi - lo) * plot.height();

    // 1. Columns coloured like the track, under the line.
    let columns = plot.width().round().max(1.0) as usize;
    let mut line = Vec::with_capacity(columns + 1);
    let mut last_value = values.iter().copied().find(|v| v.is_finite()).unwrap_or(lo);
    for c in 0..=columns {
        let d = total * c as f64 / columns as f64;
        let i = p.dist.partition_point(|&x| x < d).min(p.len() - 1);
        if values[i].is_finite() {
            last_value = values[i];
        }
        let x = plot.left() + c as f32;
        let y = y_of(last_value);
        line.push(egui::pos2(x, y));
        if c < columns {
            let color = t.colors.get(i).copied().unwrap_or([0.5, 0.5, 0.5]);
            painter.rect_filled(
                Rect::from_min_max(egui::pos2(x, y), egui::pos2(x + 1.0, plot.bottom())),
                0.0,
                srgb(color, 0.55),
            );
        }
    }
    painter.add(egui::Shape::line(line, Stroke::new(1.5, text)));
    painter.line_segment([plot.left_bottom(), plot.right_bottom()], Stroke::new(1.0, weak));

    // 2. Labels: the range up the side, distance along the bottom.
    painter.text(
        egui::pos2(plot.left() - 6.0, plot.top()),
        egui::Align2::RIGHT_TOP,
        format!("{hi:.0} {unit}"),
        font.clone(),
        weak,
    );
    painter.text(
        egui::pos2(plot.left() - 6.0, plot.bottom()),
        egui::Align2::RIGHT_BOTTOM,
        format!("{lo:.0} {unit}"),
        font.clone(),
        weak,
    );
    painter.text(egui::pos2(rect.left() + 6.0, plot.center().y), egui::Align2::LEFT_CENTER, name, font.clone(), weak);
    let step = nice_step(total / 6.0);
    let mut d = 0.0;
    while d <= total + 1e-6 {
        let x = x_of(d);
        painter
            .line_segment([egui::pos2(x, plot.bottom()), egui::pos2(x, plot.bottom() + 3.0)], Stroke::new(1.0, weak));
        let label = if step >= 1000.0 {
            format!("{:.0} km", d / 1000.0)
        } else if total >= 1000.0 {
            format!("{:.1} km", d / 1000.0)
        } else {
            format!("{d:.0} m")
        };
        painter.text(egui::pos2(x, plot.bottom() + 4.0), egui::Align2::CENTER_TOP, label, font.clone(), weak);
        d += step;
    }

    // 3. Hovering: the point under the pointer, or the one hovered in the
    //    view.
    t.chart_hover = None;
    if let Some(pos) = response.hover_pos()
        && plot.x_range().contains(pos.x)
    {
        let d = (pos.x - plot.left()) as f64 / plot.width() as f64 * total;
        t.chart_hover = Some(p.dist.partition_point(|&x| x < d).min(p.len() - 1));
    }
    if let Some(i) = t.hovered() {
        let x = x_of(p.dist[i]);
        painter.line_segment([egui::pos2(x, plot.top()), egui::pos2(x, plot.bottom())], Stroke::new(1.0, text));
        if values[i].is_finite() {
            painter.circle(egui::pos2(x, y_of(values[i])), 4.0, Color32::WHITE, Stroke::new(1.5, Color32::BLACK));
        }
        let label = t.describe(i, true);
        let galley = painter.layout_no_wrap(label, font, text);
        let left = (x + 8.0).min(plot.right() - galley.size().x).max(plot.left());
        let at = egui::pos2(left, plot.top());
        painter.rect_filled(
            Rect::from_min_size(at - vec2(4.0, 2.0), galley.size() + vec2(8.0, 4.0)),
            3.0,
            ui.visuals().extreme_bg_color,
        );
        painter.galley(at, galley, text);
    }
}

/// A round step near `raw`: 1, 2 or 5 times a power of ten.
fn nice_step(raw: f64) -> f64 {
    if raw.is_nan() || raw <= 0.0 {
        return 1.0;
    }
    let p = 10f64.powf(raw.log10().floor());
    let m = raw / p;
    p * if m <= 1.0 {
        1.0
    } else if m <= 2.0 {
        2.0
    } else if m <= 5.0 {
        5.0
    } else {
        10.0
    }
}

//endregion

#[cfg(test)]
mod tests {
    use super::*;
    use slerp_core::paths::gcode::{Keep, Parser};
    use slerp_core::paths::track::{Line, LineKind, TrackPoint, TrackSet};

    #[test]
    fn steps_are_round() {
        assert_eq!(nice_step(1800.0), 2000.0);
        assert_eq!(nice_step(420.0), 500.0);
        assert_eq!(nice_step(0.0), 1.0);
    }

    /// A ride north at about 5 m/s, speeding up a little all the time, that
    /// climbs 1 m a second, with heart rate but no power.
    fn ride(heights: bool) -> TrackState {
        let points = (0..120)
            .map(|i| {
                // Distance covered after i seconds at 5 m/s plus 0.02 m/s².
                let metres = 5.0 * i as f64 + 0.01 * (i * i) as f64;
                let mut p = TrackPoint::new(45.0 + metres / 111_195.0, 7.0);
                p.time = 1_700_000_000.0 + i as f64;
                p.ele = if heights { 100.0 + i as f32 } else { f32::NAN };
                p.hr = 120.0 + i as f32 * 0.5;
                p
            })
            .collect();
        let set =
            TrackSet { lines: vec![Line { name: String::new(), kind: LineKind::Track, points }], ..Default::default() };
        let track = Track::new(set, 1000).unwrap();
        match PathsState::new(PathData::Track(Box::new(track))) {
            PathsState::Track(t) => *t,
            PathsState::Toolpath(_) => unreachable!(),
        }
    }

    #[test]
    fn a_ride_offers_what_it_recorded() {
        let t = ride(true);
        assert_eq!(t.available, [ColorBy::Elevation, ColorBy::Speed, ColorBy::HeartRate, ColorBy::Plain]);
        assert_eq!(t.look.color_by, ColorBy::Elevation);
        assert!(t.has_height);
        let chart = t.chart.as_ref().unwrap();
        assert_eq!((chart.name, chart.unit), ("Elevation", "m"));
        assert_eq!((chart.lo, chart.hi), (100.0, 219.0));
        // The 3D point of the profile's last sample is its height, exaggerated.
        let end = t.point(119);
        assert!((end.y - 119.0 * t.look.exaggeration).abs() < 1e-3, "{end:?}");
        // 5 m/s and a little more: the 60th sample is about 336 m along.
        assert!(t.describe(60, true).contains("336 m") && t.describe(60, true).contains("160 m"));
        assert!(t.describe(60, true).contains("1 min 00 s") && t.describe(60, true).contains("bpm"));
        assert!(!t.describe(60, false).contains("bpm"), "the 3D tooltip is the short one");
        // The chart, the view or a pinned point mark one.
        assert_eq!(t.hovered(), None);
    }

    #[test]
    fn a_flat_ride_charts_its_speed() {
        let t = ride(false);
        assert!(!t.has_height);
        assert_eq!(t.available[0], ColorBy::Speed);
        let chart = t.chart.as_ref().unwrap();
        assert_eq!((chart.name, chart.unit), ("Speed", "km/h"));
        // 5 m/s is 18 km/h; two minutes of 0.02 m/s² adds 8.6 km/h.
        assert!(chart.hi > 25.0 && chart.hi < 27.0, "{}", chart.hi);
    }

    #[test]
    fn a_toolpath_starts_on_its_top_layer() {
        let mut p = Parser::new(Keep::All, 1000);
        p.line(b"M83");
        for layer in 1..=5 {
            for l in [
                format!(";LAYER_CHANGE\n;Z:{}\n;HEIGHT:0.2", layer as f32 * 0.2),
                format!("G1 Z{}", layer as f32 * 0.2),
                "G1 X10 Y10".to_string(),
                "G1 X20 Y10 E1".to_string(),
            ] {
                for line in l.lines() {
                    p.line(line.as_bytes());
                }
            }
        }
        let PathsState::Toolpath(t) = PathsState::new(PathData::Toolpath(Box::new(p.finish()))) else { panic!() };
        assert_eq!((t.top, t.tp.layers.len()), (4, 5));
        assert!(!t.only);
        assert_eq!(t.look, ToolpathLook::all());
    }
}
