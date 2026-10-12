//! Turning what the viewer shows into a `ViewState` (for "Copy View
//! State", which `slerp render --state` replays) and back (for
//! `SLERP_VIEW_STATE`, which opens a file showing a saved view).

use crate::camera::Mode;
use crate::document::{Content, Document};
use crate::gfx::Gfx;
use crate::loader::Waker;
use crate::look::{Backdrop, Look};
use glam::Vec3;
use slerp_core::{FlyState, OrbitState, ViewState};
use slerp_render::{Background, RenderSettings};

/// The state of a 3D document, or `None` for textures and galleries, which
/// `slerp render` shows as their thumbnails.
pub fn capture(doc: &Document, look: &Look) -> Option<ViewState> {
    if matches!(
        doc.content,
        Content::Texture(_)
            | Content::Gallery(_)
            | Content::Canvas(_)
            | Content::Lut(_)
            | Content::Palette(_)
            | Content::Dicom(_)
            | Content::Volume(_)
            | Content::Pages(_)
            | Content::Cad(_)
    ) {
        return None;
    }
    let view = doc.scene()?;
    let mut state = ViewState { part: doc.part.clone(), ..ViewState::default() };
    match view.cameras.mode {
        Mode::Orbit => {
            let o = &view.cameras.orbit;
            state.orbit = Some(OrbitState {
                yaw: round(o.yaw.to_degrees(), 3),
                pitch: round(o.pitch.to_degrees(), 3),
                distance: Some(round(o.distance, 5)),
                fov: round(o.fov_y.to_degrees(), 3),
                ortho: o.ortho,
                target: Some(o.target.to_array().map(|v| round(v, 5))),
            });
        }
        Mode::Fly => {
            let f = &view.cameras.fly;
            state.fly = Some(FlyState {
                position: f.position.to_array().map(|v| round(v, 5)),
                yaw: round(f.yaw.to_degrees(), 3),
                pitch: round(f.pitch.to_degrees(), 3),
                fov: round(f.fov_y.to_degrees(), 3),
            });
        }
    }
    let scene = &view.scene;
    if let Some(c) = view.player.clip.and_then(|c| scene.animations.get(c)) {
        state.clip = Some(c.name.clone());
        state.time = round(view.player.time, 4);
        state.in_place = !view.player.root_motion;
    }
    // Morph weights by name: whatever the pose shows, plus explicit
    // settings to zero that override a clip.
    for (ni, node) in scene.nodes.iter().enumerate() {
        let Some(mesh) = node.mesh.and_then(|m| scene.meshes.get(m)) else { continue };
        for (ti, name) in mesh.target_names.iter().enumerate() {
            let w = view.pose.weights.get(ni).and_then(|w| w.get(ti)).copied().unwrap_or(0.0);
            if w != 0.0 {
                state.morphs.entry(name.clone()).or_insert(round(w, 4));
            }
        }
    }
    for (name, &w) in &view.morphs.named {
        state.morphs.entry(name.clone()).or_insert(round(w, 4));
    }
    state.hidden = scene
        .nodes
        .iter()
        .zip(&view.initial_visible)
        .filter(|(n, was)| **was && !n.visible)
        .map(|(n, _)| n.name.clone())
        .collect();
    state.exposure = look.exposure;
    state.background = Some(slerp_core::background_name(look.background()));
    state.tonemap = look.tonemap.map(|t| slerp_core::tonemap_name(t).into());
    state.environment = look.environment.clone();
    state.environment_rotation = look.rotation;
    state.environment_intensity = (look.intensity != 1.0).then_some(look.intensity);
    state.key_light = (look.key_light != 1.0).then_some(look.key_light);
    if view.scene.fill > 0.0 {
        state.fill = look.fills.get(&view.scene.info.format).map(|f| round(*f, 3));
    }
    state.shadows = (!look.shadows).then_some(false);
    if look.debug != slerp_render::DebugChannel::None {
        state.debug = Some(slerp_core::debug_name(look.debug).into());
    }
    let o = &look.overlays;
    (state.wireframe, state.normals, state.tangents, state.bounds) = (o.wireframe, o.normals, o.tangents, o.bounds);
    (state.skeleton, state.grid, state.markers) = (o.skeleton, o.grid, o.markers);
    if let Content::Material(m) = &doc.content {
        state.material = Some(m.state.clone());
    }
    if view.scene.splats.is_some() && look.splat_scale != 1.0 {
        state.splat_scale = Some(round(look.splat_scale, 3));
    }
    state.ambient_occlusion = (look.ao != RenderSettings::thumbnail().ao).then_some(look.ao);
    state.ao_strength = (look.ao_strength != 1.0).then_some(round(look.ao_strength, 3));
    if crate::panels::has_points(&view.scene) {
        state.point_size = (look.point_size != 1.0).then_some(round(look.point_size, 3));
        state.point_world_size = look.point_world_size;
    }
    Some(state)
}

fn round(v: f32, places: i32) -> f32 {
    let k = 10f32.powi(places);
    (v * k).round() / k
}

/// Shows `state` in the open document: camera, clip and time (paused),
/// morphs, hidden nodes, lighting and overlays.
pub fn apply(state: &ViewState, doc: &mut Document, look: &mut Look, gfx: &mut Gfx, waker: &Waker, aspect: f32) {
    // 1. Lighting and display, which outlive the document.
    look.exposure = state.exposure;
    if let Some(bg) = state.background.as_deref().and_then(slerp_core::parse_background) {
        match bg {
            Background::Environment { blur } => {
                look.backdrop = Backdrop::Environment;
                look.blur = blur;
            }
            Background::Color(c) => {
                look.backdrop = Backdrop::Color;
                look.color = [c[0], c[1], c[2]];
            }
            // The window has nothing behind it to show through.
            Background::Transparent => {}
        }
    }
    if let Some(t) = state.tonemap.as_deref().and_then(slerp_core::parse_tonemap) {
        look.tonemap = Some(t);
    }
    look.debug = state.debug.as_deref().and_then(slerp_core::parse_debug).unwrap_or_default();
    look.rotation = state.environment_rotation;
    look.intensity = state.environment_intensity.unwrap_or(1.0);
    look.key_light = state.key_light.unwrap_or(1.0);
    look.shadows = state.shadows.unwrap_or(true);
    look.set_environment(state.environment.clone(), waker);
    let o = &mut look.overlays;
    (o.wireframe, o.normals, o.tangents, o.bounds) = (state.wireframe, state.normals, state.tangents, state.bounds);
    (o.skeleton, o.grid, o.markers) = (state.skeleton, state.grid, state.markers);
    look.splat_scale = state.splat_scale.unwrap_or(1.0);
    look.ao = state.ambient_occlusion.unwrap_or(RenderSettings::thumbnail().ao);
    look.ao_strength = state.ao_strength.unwrap_or(1.0);
    look.point_size = state.point_size.unwrap_or(1.0);
    look.point_world_size = state.point_world_size;
    if let (Some(f), Some(view)) = (state.fill, doc.scene()) {
        look.fills.insert(view.scene.info.format.clone(), f);
    }

    // 2. A material's shape.
    if let (Content::Material(m), Some(ms)) = (&mut doc.content, &state.material) {
        m.state = ms.clone();
        m.rebuild(gfx, aspect);
    }

    // 3. The scene: hidden nodes, clip, morphs, camera.
    let Some(view) = doc.scene_mut() else { return };
    for name in &state.hidden {
        let found: Vec<usize> =
            view.scene.nodes.iter().enumerate().filter(|(_, n)| &n.name == name).map(|(i, _)| i).collect();
        for i in found {
            view.set_visible(i, false);
        }
    }
    if let Some(name) = &state.clip {
        let clip = view.scene.animations.iter().position(|a| &a.name == name);
        view.player = crate::anim::Player::new(&view.scene);
        view.player.clip = clip;
        view.player.time = state.time;
        view.player.playing = false;
        view.player.root_motion = !state.in_place;
        view.player.update_hold(&view.scene);
    }
    for (name, &w) in &state.morphs {
        view.morphs.named.insert(name.clone(), w);
    }
    view.pose_dirty = true;
    view.refresh_pose(gfx);
    let bounds = view.frame_bounds();
    if let Some(f) = &state.fly {
        view.cameras.fly.position = Vec3::from(f.position);
        view.cameras.fly.yaw = f.yaw.to_radians();
        view.cameras.fly.pitch = f.pitch.to_radians();
        view.cameras.fly.fov_y = f.fov.to_radians();
        view.cameras.mode = Mode::Fly;
    } else if let Some(o) = &state.orbit {
        let cam = &mut view.cameras.orbit;
        cam.yaw = o.yaw.to_radians();
        cam.pitch = o.pitch.to_radians();
        cam.fov_y = o.fov.to_radians();
        cam.ortho = o.ortho;
        cam.frame(&bounds, aspect);
        if let Some(d) = o.distance {
            cam.distance = d;
        }
        if let Some(t) = o.target {
            cam.target = Vec3::from(t);
        }
        view.cameras.mode = Mode::Orbit;
    }
}
