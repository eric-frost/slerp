//! MetaHuman DNA files in the viewer.  The character comes from
//! `slerp_dna::open` with its body, textures and rigs; every time the pose
//! is recomputed (each frame while a clip plays, or when a slider moves),
//! RigLogic evaluates the face controls and the body's joint rotations
//! into joint offsets and blend shape weights, so corrective shapes and
//! joints move as they do in Unreal.
//!
//! The panels: a level of detail picker, a face panel of Apple's 52 ARKit
//! shapes (the names lip-sync and face-capture tools use) mapped onto the
//! rig's raw controls the way chalk bakes them, and the raw controls
//! themselves, grouped by face area with a search.  Body clips come from
//! "Add Animations from File" like any character's; the body's correctives
//! follow them.
//!
//! For screenshots, `SLERP_DNA_FACE="jawOpen=0.6,mouthSmileLeft=1"` sets
//! ARKit shapes and `SLERP_DNA_LOD=<n>` the level of detail when a DNA file
//! opens.

use crate::anim::{ARKIT_GROUPS, ARKIT_PRESETS};
use crate::gfx::Gfx;
use crate::scene_view::SceneView;
use egui::{Slider, Ui};
use slerp_dna::{Character, RigKind};
use slerp_scene::{Pose, Scene};
use slerp_theme::widgets;
use std::collections::BTreeMap;
use std::path::Path;
use std::time::Instant;

/// Opens a DNA file for the viewer: the scene to show and the character
/// that drives it.
pub fn open(path: &Path) -> Result<(Scene, Box<Character>), String> {
    let mut c = slerp_dna::open(path, &slerp_dna::Options::viewer()).map_err(|e| e.to_string())?;
    if let Some(lod) = crate::env::var("SLERP_DNA_LOD").and_then(|s| s.parse().ok()) {
        c.set_lod(lod).map_err(|e| e.to_string())?;
    }
    let scene = std::mem::take(&mut c.scene);
    Ok((scene, Box::new(c)))
}

/// A DNA character's rigs and what the panels set on them.
pub struct DnaState {
    pub character: Box<Character>,
    /// ARKit shapes set in the face panel.
    pub face: BTreeMap<&'static str, f32>,
    /// Raw controls set by hand, by index, overriding the face panel.
    pub raw: BTreeMap<usize, f32>,
    /// Each ARKit shape's raw controls, by index.
    arkit: Vec<(&'static str, Vec<usize>)>,
    pub filter: String,
    /// A level of detail picked in the panel, applied by `update`, which
    /// has the GPU.
    pending_lod: Option<u16>,
    pub error: Option<String>,
    /// Milliseconds the last evaluation took.
    pub eval_ms: f64,
}

impl DnaState {
    pub fn new(character: Box<Character>) -> DnaState {
        let arkit = match &character.head_rig {
            Some(rig) if rig.kind == RigKind::Head => slerp_dna::ARKIT_CONTROLS
                .iter()
                .map(|(shape, controls)| (*shape, controls.iter().filter_map(|c| rig.control(c)).collect()))
                .collect(),
            _ => Vec::new(),
        };
        let mut state = DnaState {
            character,
            face: BTreeMap::new(),
            raw: BTreeMap::new(),
            arkit,
            filter: String::new(),
            pending_lod: None,
            error: None,
            eval_ms: 0.0,
        };
        for (name, w) in
            crate::env::var("SLERP_DNA_FACE").unwrap_or_default().split(',').filter_map(|p| p.split_once('='))
        {
            if let Some((shape, _)) = state.arkit.iter().find(|(s, _)| s.eq_ignore_ascii_case(name.trim())) {
                state.face.insert(shape, w.trim().parse().unwrap_or(1.0));
            }
        }
        state
    }

    /// Takes over what the panels set on `old`, the character before the
    /// file was reloaded: face shapes, raw controls, the search and the
    /// level of detail.
    pub fn keep_from(&mut self, old: &DnaState) {
        self.face = old.face.clone();
        let controls = self.character.head_rig.as_ref().map_or(0, |r| r.values.len());
        self.raw = old.raw.iter().filter(|(c, _)| **c < controls).map(|(c, w)| (*c, *w)).collect();
        self.filter = old.filter.clone();
        let lod = old.pending_lod.unwrap_or(old.character.lod());
        if lod != self.character.lod() && lod < self.character.lod_count() {
            self.pending_lod = Some(lod);
        }
    }

    /// Asks for level of detail `lod`, which `update` builds when it next
    /// runs (it has the GPU): the Contents panel's detail levels.
    pub fn request_lod(&mut self, lod: u16) {
        if lod < self.character.lod_count() && lod != self.character.lod() {
            self.pending_lod = Some(lod);
        }
    }

    /// Whether the face panels apply: the main file is a head.
    pub fn has_face(&self) -> bool {
        !self.arkit.is_empty()
    }

    /// Sets the face rig's controls from the panels: each ARKit shape's
    /// controls to its weight (the strongest wins where two share one),
    /// then the raw controls set by hand.
    fn set_controls(&mut self) {
        let Some(rig) = self.character.head_rig.as_mut().filter(|r| r.kind == RigKind::Head) else { return };
        let mut values = vec![0f32; rig.values.len()];
        for (shape, controls) in &self.arkit {
            if let Some(&w) = self.face.get(shape) {
                for &c in controls {
                    values[c] = values[c].max(w);
                }
            }
        }
        for (&c, &w) in &self.raw {
            if let Some(v) = values.get_mut(c) {
                *v = w;
            }
        }
        for (c, v) in values.into_iter().enumerate() {
            if !rig.is_joint_input(c) {
                rig.values[c] = v;
            }
        }
    }

    /// Evaluates the rigs into `pose`, after a clip has set it and before
    /// the morph sliders and world matrices.
    pub fn apply(&mut self, pose: &mut Pose) {
        self.set_controls();
        let start = Instant::now();
        match self.character.apply(pose) {
            Ok(()) => self.error = None,
            Err(e) => self.error = Some(e),
        }
        self.eval_ms = start.elapsed().as_secs_f64() * 1e3;
    }
}

/// Applies a level of detail picked in the panel: rebuilds the scene at
/// that level, keeping the camera, the clips added from libraries (joints
/// keep their node indices at every level) and the player.
pub fn update(view: &mut SceneView, gfx: &mut Gfx) {
    let Some(lod) = view.dna.as_mut().and_then(|d| d.pending_lod.take()) else { return };
    let Some(mut dna) = view.dna.take() else { return };
    match dna.character.set_lod(lod) {
        Ok(()) => {
            let mut scene = std::mem::take(&mut dna.character.scene);
            scene.animations = std::mem::take(&mut view.scene.animations);
            let mut fresh = SceneView::new(scene, gfx, 1.0);
            std::mem::swap(&mut fresh.cameras, &mut view.cameras);
            std::mem::swap(&mut fresh.player, &mut view.player);
            std::mem::swap(&mut fresh.morphs, &mut view.morphs);
            fresh.cameras.set_bounds(fresh.gpu.bounds);
            fresh.own_clips = view.own_clips;
            fresh.foot_ik = std::mem::take(&mut view.foot_ik);
            *view = fresh;
        }
        Err(e) => dna.error = Some(e.to_string()),
    }
    view.dna = Some(dna);
    view.pose_dirty = true;
}

/*┌─────────────────╮
| Panels          |
└─────────────────╯*/
//region Panels

fn section(ui: &mut Ui, title: &str, open: bool, add: impl FnOnce(&mut Ui)) {
    crate::sections::section(ui, title, title, open, add);
}

fn form(ui: &mut Ui, id: &str, add: impl FnOnce(&mut Ui)) {
    egui::Grid::new(id).num_columns(2).spacing([8.0, 4.0]).show(ui, add);
}

/// "browDownLeft" or "CTRL_expressions.mouthLipsPurseUL" as "Brow down
/// left" and "Mouth lips purse UL": camel case as words, runs of capitals
/// (a side such as L, UL or INL) kept as they are.
fn words(name: &str) -> String {
    let base = name.rsplit('.').next().unwrap_or(name);
    let chars: Vec<char> = base.chars().collect();
    let mut tokens: Vec<String> = Vec::new();
    for (i, &c) in chars.iter().enumerate() {
        if c == '_' {
            tokens.push(String::new());
            continue;
        }
        let prev = i.checked_sub(1).map(|p| chars[p]);
        let next = chars.get(i + 1).copied();
        let starts = c.is_uppercase()
            && (prev.is_some_and(|p| p.is_lowercase() || p.is_ascii_digit())
                || (prev.is_some_and(char::is_uppercase) && next.is_some_and(char::is_lowercase)));
        if starts || tokens.is_empty() {
            tokens.push(String::new());
        }
        tokens.last_mut().expect("just pushed").push(c);
    }
    let words: Vec<String> = tokens
        .into_iter()
        .filter(|t| !t.is_empty())
        .enumerate()
        .map(|(i, t)| {
            let acronym = t.chars().all(|c| c.is_uppercase()) && (t.len() > 1 || i > 0);
            if acronym {
                t
            } else if i == 0 {
                let mut cs = t.chars();
                cs.next().map(|f| f.to_uppercase().chain(cs).collect()).unwrap_or_default()
            } else {
                t.to_lowercase()
            }
        })
        .collect();
    words.join(" ")
}

/// The MetaHuman sections of the right-hand panel.
pub fn controls(ui: &mut Ui, view: &mut SceneView) {
    let SceneView { dna, pose_dirty, .. } = view;
    let Some(dna) = dna.as_deref_mut() else { return };
    let mut changed = false;
    section(ui, "MetaHuman", true, |ui| changed |= rig_section(ui, dna));
    if dna.has_face() {
        section(ui, "Face", true, |ui| changed |= face_section(ui, dna));
        section(ui, "Rig Controls", false, |ui| changed |= raw_section(ui, dna));
    }
    if changed {
        *pose_dirty = true;
    }
}

fn rig_section(ui: &mut Ui, dna: &mut DnaState) -> bool {
    let c = &dna.character;
    let count = c.lod_count();
    let mut lod = dna.pending_lod.unwrap_or(c.lod());
    form(ui, "dna-form", |ui| {
        ui.label("Level of detail:");
        let name = |l: u16| if l == 0 { "0 (finest)".to_string() } else { l.to_string() };
        widgets::Choice::new("dna-lod", &mut lod, (0..count).map(|l| (l, name(l)))).width(140.0).show(ui);
        ui.end_row();
    });
    if lod != c.lod() && dna.pending_lod != Some(lod) {
        dna.pending_lod = Some(lod);
    }
    let rigs: Vec<String> = [&c.head_rig, &c.body_rig]
        .into_iter()
        .flatten()
        .map(|r| match r.kind {
            RigKind::Head => format!("face, {} controls", r.controls.len()),
            RigKind::Body => format!("body, {} joint inputs", r.controls.len()),
        })
        .collect();
    ui.label(format!("RigLogic: {}.", rigs.join("; ")));
    ui.label(format!("Last evaluation {:.2} ms.", dna.eval_ms));
    if c.lod() > 0 && dna.has_face() {
        ui.label("Blend shapes exist at level 0 only; coarser levels move by joints alone, as in Unreal.");
    }
    if c.body_rig.is_some() {
        ui.label("The body's correctives follow its clips.");
    }
    if let Some(e) = &dna.error {
        ui.colored_label(ui.visuals().error_fg_color, e);
    }
    false
}

/// The ARKit face panel: presets, then a slider per shape by face area.
fn face_section(ui: &mut Ui, dna: &mut DnaState) -> bool {
    let mut changed = false;
    ui.label("Expressions:");
    ui.horizontal_wrapped(|ui| {
        for (name, shapes) in ARKIT_PRESETS {
            if ui.button(*name).clicked() {
                dna.face.clear();
                for (shape, w) in *shapes {
                    if let Some((s, _)) = dna.arkit.iter().find(|(s, _)| s == shape) {
                        dna.face.insert(s, *w);
                    }
                }
                changed = true;
            }
        }
    });
    if ui.button("Reset Face").on_hover_text("Every shape and control back to rest").clicked() {
        dna.face.clear();
        dna.raw.clear();
        changed = true;
    }
    for (group, names) in ARKIT_GROUPS {
        let open = matches!(*group, "Brows" | "Jaw");
        crate::sections::section(ui, &format!("Face/{group}"), *group, open, |ui| {
            form(ui, &format!("dna-face-{group}"), |ui| {
                for name in *names {
                    let Some((shape, _)) = dna.arkit.iter().find(|(s, _)| s == name) else { continue };
                    let shape: &'static str = shape;
                    let mut w = dna.face.get(shape).copied().unwrap_or(0.0);
                    ui.label(words(name));
                    if widgets::slider(ui, Slider::new(&mut w, 0.0..=1.0).fixed_decimals(2)).changed() {
                        dna.face.insert(shape, w);
                        changed = true;
                    }
                    ui.end_row();
                }
            });
        });
    }
    changed
}

/// The raw controls, by face area, with a search.  Each slider shows the
/// value RigLogic last got, from the face panel or set here.
fn raw_section(ui: &mut Ui, dna: &mut DnaState) -> bool {
    let mut changed = false;
    let Some(rig) = &dna.character.head_rig else { return false };
    ui.add(widgets::text_field(ui.ctx(), &mut dna.filter).hint_text("Find a control").desired_width(f32::INFINITY));
    if ui.button("Reset Controls").on_hover_text("Back to what the face panel sets").clicked() {
        dna.raw.clear();
        changed = true;
    }
    let filter = dna.filter.to_lowercase();
    let mut groups: BTreeMap<&'static str, Vec<usize>> = BTreeMap::new();
    for (i, name) in rig.controls.iter().enumerate() {
        if rig.is_joint_input(i) || !(filter.is_empty() || name.to_lowercase().contains(&filter)) {
            continue;
        }
        groups.entry(slerp_dna::control_group(name)).or_default().push(i);
    }
    let names: Vec<String> = rig.controls.clone();
    let values: Vec<f32> = rig.values.clone();
    for (group, list) in groups {
        let title = format!("{group} ({})", list.len());
        crate::sections::section(ui, &format!("Rig Controls/{group}"), title, !filter.is_empty(), |ui| {
            form(ui, &format!("dna-raw-{group}"), |ui| {
                for i in list {
                    let mut w = dna.raw.get(&i).copied().unwrap_or(values[i]);
                    ui.label(words(&names[i])).on_hover_text(&names[i]);
                    if widgets::slider(ui, Slider::new(&mut w, 0.0..=1.0).fixed_decimals(2)).changed() {
                        dna.raw.insert(i, w);
                        changed = true;
                    }
                    ui.end_row();
                }
            });
        });
    }
    changed
}

//endregion

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn control_names_read_as_words() {
        assert_eq!(words("browDownLeft"), "Brow down left");
        assert_eq!(words("CTRL_expressions.mouthCornerPullL"), "Mouth corner pull L");
        assert_eq!(words("CTRL_expressions.mouthLipsPurseUL"), "Mouth lips purse UL");
        assert_eq!(words("CTRL_expressions.jawOpen"), "Jaw open");
        assert_eq!(words("CTRL_expressions.eyelashesUpINL"), "Eyelashes up INL");
    }
}
