//! How the 3D view looks: lighting (the studio or any HDR file), exposure,
//! tone mapping, background, shadows, inspection overlays and debug
//! channels.  These carry over from file to file.

use crate::gfx::Environment;
use crate::loader::{Task, Waker, poll, spawn};
use slerp_render::{Background, DebugChannel, Overlays, RenderSettings, Tonemap};
use slerp_scene::{Scene, SceneKind, Texture};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Backdrop {
    /// The lighting environment, blurred.
    Environment,
    /// A flat colour.
    Color,
}

pub struct Look {
    /// The HDR or picture lighting the scene; `None` is the built-in studio.
    pub environment: Option<PathBuf>,
    /// The decoded picture for `environment`, once loaded.
    texture: Option<Arc<Texture>>,
    loading: Option<(PathBuf, Task<Texture>)>,
    pub exposure: f32,
    /// The tone mapping the user chose; until then each scene gets its
    /// own default (none for levels with baked light, PBR Neutral for the
    /// rest).
    pub tonemap: Option<Tonemap>,
    pub backdrop: Backdrop,
    pub blur: f32,
    /// sRGB colour for `Backdrop::Color`.
    pub color: [f32; 3],
    /// Degrees.
    pub rotation: f32,
    pub intensity: f32,
    pub key_light: f32,
    pub shadows: bool,
    pub ground_shadow: bool,
    pub msaa: bool,
    pub overlays: Overlays,
    pub debug: DebugChannel,
    /// Plays Quake's liquids and skies.  Off by default so a level at rest
    /// costs nothing to show.
    pub animate_effects: bool,
    /// Multiplies the size of Gaussian splats.
    pub splat_scale: f32,
    /// Screen-space ambient occlusion, and how strongly it darkens.
    pub ao: bool,
    pub ao_strength: f32,
    /// Multiplies the size of points' dots.
    pub point_size: f32,
    /// Points keep their size in the world instead of on screen.
    pub point_world_size: bool,
    /// The fill light the user chose for each format whose files carry no
    /// light of their own (`Scene::fill`), by format name; formats not
    /// listed use the scene's default.  Remembered between runs.
    pub fills: BTreeMap<String, f32>,
}

impl Default for Look {
    fn default() -> Self {
        Look {
            environment: None,
            texture: None,
            loading: None,
            exposure: 0.0,
            tonemap: None,
            backdrop: Backdrop::Environment,
            blur: 0.6,
            color: [0.24, 0.25, 0.27],
            rotation: 0.0,
            intensity: 1.0,
            key_light: 1.0,
            shadows: true,
            ground_shadow: true,
            msaa: true,
            overlays: Overlays::default(),
            debug: DebugChannel::None,
            animate_effects: false,
            splat_scale: 1.0,
            ao: true,
            ao_strength: 1.0,
            point_size: 1.0,
            point_world_size: false,
            fills: BTreeMap::new(),
        }
    }
}

impl Look {
    /// Switches the lighting to the picture at `path` (or the studio),
    /// decoding it in the background.
    pub fn set_environment(&mut self, path: Option<PathBuf>, waker: &Waker) {
        if path == self.environment && (self.texture.is_some() || path.is_none() || self.loading.is_some()) {
            return;
        }
        self.environment = path.clone();
        self.texture = None;
        self.loading = path.map(|p| {
            let q = p.clone();
            let rx = spawn(waker, "slerp-hdr", move || {
                slerp_scene::textures::decode_file(&q, &slerp_core::environment_decode_options())
                    .map_err(|e| e.to_string())
            });
            (p, rx)
        });
    }

    /// Picks up a finished decode.  Returns an error message when the
    /// picture couldn't be read, after going back to the studio.
    pub fn poll(&mut self) -> Option<Result<(), String>> {
        let (path, rx) = self.loading.as_ref()?;
        let result = poll(rx)?;
        let path = path.clone();
        self.loading = None;
        if self.environment.as_ref() != Some(&path) {
            return None;
        }
        Some(match result {
            Ok(t) => {
                self.texture = Some(Arc::new(t));
                Ok(())
            }
            Err(e) => {
                self.environment = None;
                Err(format!("{}: {e}", path.display()))
            }
        })
    }

    pub fn is_loading(&self) -> bool {
        self.loading.is_some()
    }

    /// The lighting the renderer should have now: the studio until a
    /// chosen picture has decoded.
    pub fn wanted_environment(&self) -> Environment {
        match &self.texture {
            Some(t) => Environment::Picture(t.clone()),
            None => Environment::Studio,
        }
    }

    /// The fill light `scene` is drawn with: the one chosen for its format,
    /// else its own default, and none for scenes with light of their own.
    pub fn fill(&self, scene: &Scene) -> f32 {
        slerp_core::fill_light(scene, self.fills.get(&scene.info.format).copied())
    }

    pub fn background(&self) -> Background {
        match self.backdrop {
            Backdrop::Environment => Background::Environment { blur: self.blur },
            Backdrop::Color => Background::Color([self.color[0], self.color[1], self.color[2], 1.0]),
        }
    }

    /// Render settings for `scene`, at `time` seconds of effect animation.
    pub fn settings(&self, scene: &Scene, time: f32) -> RenderSettings {
        let kind = scene.kind;
        RenderSettings {
            background: self.background(),
            exposure: self.exposure,
            tonemap: self.tonemap.unwrap_or_else(|| slerp_core::default_tonemap(scene)),
            shadows: self.shadows,
            // Only objects stand on an invisible ground: levels have their
            // own floors, and replays of material balls have none.
            ground_shadow: self.ground_shadow && kind == SceneKind::Object,
            msaa: self.msaa,
            overlays: self.overlays,
            environment_rotation: self.rotation.to_radians(),
            environment_intensity: self.intensity,
            key_light: self.key_light,
            time,
            debug: self.debug,
            splat_scale: self.splat_scale,
            ao: self.ao,
            ao_strength: self.ao_strength,
            ao_radius: RenderSettings::viewer().ao_radius,
            point_size: self.point_size,
            point_world_size: self.point_world_size,
            fill: self.fill(scene),
        }
    }
}
