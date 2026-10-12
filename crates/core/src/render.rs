//! The GPU half of thumbnails, and replaying a viewer state offscreen for
//! `slerp render`.

use crate::ThumbError;
use glam::{Vec2, Vec3};
use serde::{Deserialize, Serialize};
use slerp_render::{Background, DebugChannel, Gpu, GpuPolicy, RenderSettings, Renderer, RendererConfig, Tonemap};
use slerp_scene::LoadError;
use slerp_scene::{
    Aabb, FlyCamera, Image8, MaterialAsset, OrbitCamera, Pose, Property, Scene, SceneKind, Shape, UvTransform, View,
};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Turns a config value such as "auto", "integrated" or "RTX" into a policy.
pub fn parse_policy(value: &str) -> GpuPolicy {
    match value.trim().to_ascii_lowercase().as_str() {
        "" | "auto" => GpuPolicy::Auto,
        "integrated" => GpuPolicy::Integrated,
        "discrete" => GpuPolicy::Discrete,
        "software" | "cpu" | "lavapipe" => GpuPolicy::Software,
        _ => GpuPolicy::Named(value.trim().to_string()),
    }
}

/// Owns the GPU for thumbnails, starting it on first use, so files that
/// need no GPU never pay for it; or shares one already open
/// ([`ThumbRenderer::with_gpu`]).
pub struct ThumbRenderer {
    policy: GpuPolicy,
    ctx: Option<(Arc<Gpu>, Renderer)>,
    failed: Option<String>,
    /// The picture lighting the scene, when a replayed state asked for one
    /// instead of the studio.
    environment: Option<PathBuf>,
}

impl ThumbRenderer {
    pub fn new(policy: &str) -> Self {
        ThumbRenderer { policy: parse_policy(policy), ctx: None, failed: None, environment: None }
    }

    /// A thumbnail renderer on a GPU that is already open, such as the
    /// viewer's, for small pictures drawn beside the window's own work
    /// (the Contents panel's rows).  Making it compiles pipelines and
    /// draws the studio lighting, so make it off the window's thread.
    pub fn with_gpu(gpu: Arc<Gpu>) -> Self {
        let renderer = Renderer::with_config(&gpu, slerp_render::THUMBNAIL_FORMAT, RendererConfig::thumbnail());
        ThumbRenderer { policy: GpuPolicy::Auto, ctx: Some((gpu, renderer)), failed: None, environment: None }
    }

    fn context(&mut self) -> Result<&mut (Arc<Gpu>, Renderer), ThumbError> {
        if let Some(e) = &self.failed {
            return Err(ThumbError::NoGpu(e.clone()));
        }
        if self.ctx.is_none() {
            match slerp_render::open_gpu(&self.policy) {
                Ok(gpu) => {
                    let renderer =
                        Renderer::with_config(&gpu, slerp_render::THUMBNAIL_FORMAT, RendererConfig::thumbnail());
                    self.ctx = Some((Arc::new(gpu), renderer));
                }
                Err(e) => {
                    self.failed = Some(e.to_string());
                    return Err(ThumbError::NoGpu(e.to_string()));
                }
            }
        }
        Ok(self.ctx.as_mut().expect("just created"))
    }

    /// Draws a scene's thumbnail: rest pose, an automatic camera fitted to
    /// the bounds, transparent background.
    pub fn render_thumbnail(&mut self, scene: &Scene, size: u32) -> Result<Image8, ThumbError> {
        self.render_state(scene, &ViewState::default(), size, size)
    }

    /// Loads a file with the viewer's limits and draws it as `state` says.
    pub fn render_file(&mut self, path: &Path, size: u32, state: &ViewState) -> Result<Image8, ThumbError> {
        let asset = match &state.part {
            Some(part) => crate::load_part(path, &slerp_scene::Limits::viewer(), part)?,
            None => crate::load(path, &slerp_scene::Limits::viewer())?,
        };
        let kind = crate::sniff_file(path).ok().flatten().ok_or(ThumbError::Unknown)?;
        // A material state asks for the material on a shape even when the
        // file has a preview picture.
        let mut scene = match (asset, &state.material) {
            (slerp_scene::Asset::Material(mat), Some(look)) => material_scene(&mat, look),
            (asset, _) => match crate::prepare_asset(kind, asset, size)? {
                crate::Prepared::Done(image) => return Ok(image),
                crate::Prepared::Render(scene) => *scene,
            },
        };
        state.apply_scene(&mut scene);
        self.set_environment(state.environment.as_deref())?;
        self.render_state(&scene, state, size, size)
    }

    /// Lights later renders with the picture at `path` (an HDR panorama,
    /// any image or a cubemap), or with the built-in studio for `None`.
    pub fn set_environment(&mut self, path: Option<&Path>) -> Result<(), ThumbError> {
        if self.environment.as_deref() == path {
            return Ok(());
        }
        let texture = match path {
            Some(p) => Some(slerp_textures::decode_file(p, &environment_decode_options()).map_err(LoadError::from)?),
            None => None,
        };
        let (gpu, renderer) = self.context()?;
        match &texture {
            Some(t) => renderer.set_environment(gpu, t),
            None => renderer.set_studio_environment(gpu),
        }
        self.environment = path.map(Path::to_path_buf);
        Ok(())
    }

    /// Draws `scene` posed and framed as `state` says.  Framing uses the
    /// renderer's bounds of what is visible in the pose, which leave out
    /// Quake skies (chalk's maps sit inside a 670 m sky box).
    pub fn render_state(
        &mut self,
        scene: &Scene,
        state: &ViewState,
        width: u32,
        height: u32,
    ) -> Result<Image8, ThumbError> {
        let mut pose = Pose::rest(scene);
        state.apply_pose(scene, &mut pose);
        pose.update_world(scene);
        let settings = state.settings(scene);
        let aspect = width as f32 / height as f32;
        let (gpu, renderer) = self.context()?;
        let mut gpu_scene = renderer.prepare(gpu, scene);
        renderer.update_pose(gpu, &mut gpu_scene, scene, &pose);
        let bounds = gpu_scene.bounds;
        if bounds.is_empty() {
            return Err(ThumbError::Empty);
        }
        let view = state.view(scene, &bounds, aspect);
        Ok(renderer.render_image(gpu, &gpu_scene, &view, &settings, width, height))
    }

    /// Draws `frames` square views of `scene` in its rest pose, `size`
    /// pixels each, with the camera circling once around the vertical axis
    /// through the centre of the bounds: the turntable the Qt image plugin
    /// plays in Dolphin's Information panel and Gwenview.
    ///
    /// Frame 0 is the thumbnail's own view, and every frame is lit and
    /// framed the way the thumbnail is.  The lighting turns with the camera,
    /// as on a photographer's turntable, so each frame looks like the
    /// thumbnail of the model turned by that angle rather than a camera
    /// walking into its shadow side.  The distance fits the bounding sphere,
    /// which doesn't change as the model turns, so nothing grows or shrinks.
    /// Seen from above, the model turns anticlockwise.
    pub fn turntable(&mut self, scene: &Scene, size: u32, frames: u32) -> Result<Vec<Image8>, ThumbError> {
        let size = size.max(1);
        let state = ViewState::default();
        let mut pose = Pose::rest(scene);
        pose.update_world(scene);
        let base = state.orbit(scene);
        let (gpu, renderer) = self.context()?;
        let mut gpu_scene = renderer.prepare(gpu, scene);
        renderer.update_pose(gpu, &mut gpu_scene, scene, &pose);
        let bounds = gpu_scene.bounds;
        if bounds.is_empty() {
            return Err(ThumbError::Empty);
        }
        let mut images = Vec::with_capacity(frames as usize);
        for i in 0..frames.max(1) {
            // Turning the model one way is the camera and the lights circling
            // it the other way.
            let turn = 360.0 * i as f32 / frames.max(1) as f32;
            let frame = ViewState {
                orbit: Some(OrbitState { yaw: base.yaw - turn, ..base.clone() }),
                environment_rotation: -turn,
                ..ViewState::default()
            };
            let view = frame.view(scene, &bounds, 1.0);
            images.push(renderer.render_image(gpu, &gpu_scene, &view, &frame.settings(scene), size, size));
        }
        Ok(images)
    }
}

/// A viewer state to replay offscreen, read from JSONC.  Every field is
/// optional; the defaults give the thumbnail view.  The viewer's "Copy View
/// State" writes one, so a view seen in the viewer can be rendered again by
/// `slerp render --state` for reference images.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ViewState {
    /// Which part of a file that holds several to show, such as a Doom
    /// WAD's map (see `crate::parts`); missing shows the whole file, as
    /// thumbnails do.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub part: Option<String>,
    /// Orbit camera angles in degrees and distance in metres; a missing
    /// distance frames the bounds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub orbit: Option<OrbitState>,
    /// First-person camera; overrides `orbit`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fly: Option<FlyState>,
    /// Animation clip name and time in seconds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub clip: Option<String>,
    pub time: f32,
    /// Keeps the clip's root motion from moving the model, as the viewer
    /// does with "Root motion" turned off.
    #[serde(skip_serializing_if = "is_false")]
    pub in_place: bool,
    /// Morph target weights by target name.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub morphs: BTreeMap<String, f32>,
    /// Names of nodes to hide, with everything under them.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub hidden: Vec<String>,
    /// Exposure in stops.
    pub exposure: f32,
    /// "transparent" (the default), "#rrggbb", or "environment" for the
    /// lighting environment, optionally blurred: "environment:0.6".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub background: Option<String>,
    /// "pbrNeutral" (the default), "aces" or "none".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tonemap: Option<String>,
    /// An HDR or other picture to light the scene with instead of the
    /// built-in studio.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub environment: Option<PathBuf>,
    /// Turns the lighting environment around the vertical axis, in degrees.
    pub environment_rotation: f32,
    /// Multiplies the environment's light; missing means 1.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub environment_intensity: Option<f32>,
    /// The key light's strength relative to the environment; missing
    /// means 1, and 0 turns it and its shadow off.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key_light: Option<f32>,
    /// The fill light for scenes without light of their own (level-editor
    /// maps; see `Scene::fill`); missing means the scene's own default.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fill: Option<f32>,
    /// The key light's shadow; missing means on.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shadows: Option<bool>,
    /// One material channel instead of the lit picture: "baseColor",
    /// "normal", "metallic", "roughness", "occlusion", "emissive" or
    /// "uvChecker".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub debug: Option<String>,
    pub wireframe: bool,
    pub normals: bool,
    pub tangents: bool,
    pub bounds: bool,
    pub skeleton: bool,
    pub grid: bool,
    pub markers: bool,
    /// For material files: the shape and its texture tiling, drawn instead
    /// of the file's own preview picture.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub material: Option<MaterialState>,
    /// Multiplies the size of Gaussian splats; missing means 1.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub splat_scale: Option<f32>,
    /// Screen-space ambient occlusion; missing means what thumbnails do.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ambient_occlusion: Option<bool>,
    /// How strongly ambient occlusion darkens; missing means 1.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ao_strength: Option<f32>,
    /// Multiplies the size of points' dots; missing means 1, the size at
    /// which neighbouring dots meet.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub point_size: Option<f32>,
    /// Points keep their size in the world rather than on screen.
    #[serde(skip_serializing_if = "is_false")]
    pub point_world_size: bool,
}

fn is_false(b: &bool) -> bool {
    !*b
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, rename_all = "camelCase")]
pub struct OrbitState {
    pub yaw: f32,
    pub pitch: f32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub distance: Option<f32>,
    pub fov: f32,
    pub ortho: bool,
    /// The point the camera circles; a missing target is the centre of the
    /// bounds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<[f32; 3]>,
}

impl Default for OrbitState {
    fn default() -> Self {
        OrbitState { yaw: 30.0, pitch: 20.0, distance: None, fov: 35.0, ortho: false, target: None }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, rename_all = "camelCase")]
pub struct FlyState {
    pub position: [f32; 3],
    pub yaw: f32,
    pub pitch: f32,
    pub fov: f32,
}

impl Default for FlyState {
    fn default() -> Self {
        FlyState { position: [0.0; 3], yaw: 0.0, pitch: 0.0, fov: 75.0 }
    }
}

/// How a material file is shown: on which shape, with how many texture
/// repeats, and how far its height map pushes the surface out.
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(default, rename_all = "camelCase")]
pub struct MaterialState {
    /// "sphere", "cube" or "plane".
    pub shape: String,
    /// Texture repeats across the shape, multiplying the material's own.
    pub tiling: f32,
    /// Displacement in metres at full height; missing means the file's own,
    /// limited to a few centimetres.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub displacement: Option<f32>,
}

impl Default for MaterialState {
    fn default() -> Self {
        MaterialState { shape: "sphere".into(), tiling: 1.0, displacement: None }
    }
}

impl MaterialState {
    pub fn shape(&self) -> Shape {
        match self.shape.to_ascii_lowercase().as_str() {
            "cube" => Shape::Cube,
            "plane" => Shape::Plane,
            _ => Shape::Sphere,
        }
    }

    pub fn shape_name(shape: Shape) -> &'static str {
        match shape {
            Shape::Sphere => "sphere",
            Shape::Cube => "cube",
            Shape::Plane => "plane",
        }
    }
}

/// The largest displacement a material file gets by default.  Material
/// files give it in their own units: ambientCG's say 1.0, which on a 1 m
/// ball would turn it into a hedgehog, while a few centimetres reads as
/// relief.
pub const DEFAULT_MAX_DISPLACEMENT: f32 = 0.03;

/// A material file's material on a generated shape, as the viewer's
/// material mode and `MaterialState` replays draw it, with the state's
/// tiling and displacement applied (see `tune_material`).
pub fn material_scene(asset: &MaterialAsset, state: &MaterialState) -> Scene {
    let shape = state.shape();
    let has_height = asset.material.height_texture.is_some();
    // Displacement needs enough vertices to show the height map's detail.
    let detail = match (shape, has_height) {
        (Shape::Cube, _) => 1,
        (_, true) => 320,
        (_, false) => 128,
    };
    let mut scene = Scene::material_ball(asset, shape, detail);
    for m in &mut scene.materials {
        tune_material(m, state);
    }
    scene
}

/// Applies `state`'s tiling and displacement to `m`, a material as its file
/// gave it: the displacement becomes its height scale (the file's own is
/// limited to `DEFAULT_MAX_DISPLACEMENT`), and tiling scales every
/// texture's UVs on top of the material's own texture transform.  Both
/// only change values the renderer keeps in the material's uniform, so the
/// viewer applies them to a fresh copy while a slider moves and rewrites
/// that uniform (`Renderer::update_material`) instead of rebuilding the
/// shape.
pub fn tune_material(m: &mut slerp_scene::Material, state: &MaterialState) {
    let tiling = if state.tiling.is_finite() && state.tiling > 0.0 { state.tiling } else { 1.0 };
    m.height_scale = match state.displacement {
        Some(d) if d.is_finite() => d.max(0.0),
        _ => m.height_scale.clamp(0.0, DEFAULT_MAX_DISPLACEMENT),
    };
    if tiling != 1.0 {
        let slots = [
            &mut m.base_color_texture,
            &mut m.metallic_roughness_texture,
            &mut m.normal_texture,
            &mut m.occlusion_texture,
            &mut m.emissive_texture,
            &mut m.height_texture,
        ];
        for r in slots.into_iter().flatten() {
            // A uniform scale commutes with the transform's rotation, so
            // scaling after it scales its offset and scale alike.
            let t = r.transform.unwrap_or(UvTransform { offset: Vec2::ZERO, rotation: 0.0, scale: Vec2::ONE });
            r.transform =
                Some(UvTransform { offset: t.offset * tiling, rotation: t.rotation, scale: t.scale * tiling });
        }
    }
}

/// The topmost node a clip moves, whose horizontal travel is the clip's
/// root motion, with the chain of its ancestors.  Holding it in place keeps
/// a walking character on the spot.
#[derive(Clone, Debug)]
pub struct InPlace {
    node: usize,
    /// Ancestors of `node`, outermost first.
    chain: Vec<usize>,
}

impl InPlace {
    /// Finds the motion root of clip `clip`: of the nodes it translates,
    /// the one closest to the scene's roots.
    pub fn new(scene: &Scene, clip: usize) -> Option<InPlace> {
        let anim = scene.animations.get(clip)?;
        let parents = scene.parents();
        let ancestors = |mut n: usize| {
            let mut chain = Vec::new();
            while let Some(p) = parents.get(n).copied().flatten() {
                // A malformed file can make a node its own ancestor.
                if chain.len() > parents.len() {
                    break;
                }
                chain.push(p);
                n = p;
            }
            chain.reverse();
            chain
        };
        anim.channels
            .iter()
            .filter(|c| c.property == Property::Translation && c.node < scene.nodes.len())
            .map(|c| (c.node, ancestors(c.node)))
            .min_by_key(|(node, chain)| (chain.len(), *node))
            .map(|(node, chain)| InPlace { node, chain })
    }

    /// The node held in place.
    pub fn node(&self) -> usize {
        self.node
    }

    /// Puts the motion root back over its rest position, horizontally, in
    /// world space, so it works whatever way the skeleton's axes point.
    /// Call after sampling the clip and before `update_world`.
    pub fn apply(&self, scene: &Scene, pose: &mut Pose) {
        let Some(rest) = scene.nodes.get(self.node).map(|n| n.transform.translation) else { return };
        let parent =
            self.chain.iter().filter_map(|&a| pose.local.get(a)).fold(glam::Mat4::IDENTITY, |m, t| m * t.matrix());
        if parent.determinant().abs() < 1e-12 {
            return;
        }
        let Some(local) = pose.local.get_mut(self.node) else { return };
        let mut world = parent.transform_point3(local.translation);
        let home = parent.transform_point3(rest);
        world.x = home.x;
        world.z = home.z;
        local.translation = parent.inverse().transform_point3(world);
    }
}

/// The near and far planes of a first-person camera in a scene with these
/// bounds, shared by the viewer and replays so both clip alike.
pub fn fly_clip_planes(bounds: &Aabb) -> (f32, f32) {
    let radius = if bounds.is_empty() { 1.0 } else { bounds.radius() };
    ((radius * 0.0005).clamp(0.002, 0.05), radius.max(10.0) * 4.0)
}

/// Reads a tone mapping name as `ViewState::tonemap` spells it.
pub fn parse_tonemap(name: &str) -> Option<Tonemap> {
    match name.to_ascii_lowercase().as_str() {
        "pbrneutral" | "neutral" | "khronos" => Some(Tonemap::PbrNeutral),
        "aces" => Some(Tonemap::Aces),
        "none" | "clamp" => Some(Tonemap::None),
        _ => None,
    }
}

/// The fill light to draw `scene` with: its own default, or `chosen` when
/// somebody picked a value.  Scenes with authored light (`Scene::fill` of 0)
/// never get one.
pub fn fill_light(scene: &Scene, chosen: Option<f32>) -> f32 {
    if scene.fill > 0.0 { chosen.unwrap_or(scene.fill).max(0.0) } else { 0.0 }
}

/// The tone mapping a scene looks right with when nobody chose one.
/// Scenes whose light is baked into what they store, so that every drawn
/// material is unlit, lightmapped, a sky or a liquid (Quake and Doom
/// levels, point cloud scans, unlit glTF), hold colours meant for the
/// screen as they are: PBR Neutral's toe would darken and saturate them,
/// so they get none.  Everything lit by Slerp's own lights gets PBR
/// Neutral.
pub fn default_tonemap(scene: &Scene) -> Tonemap {
    let mut drawn = scene.meshes.iter().flat_map(|m| &m.primitives).peekable();
    if drawn.peek().is_none() {
        return Tonemap::PbrNeutral;
    }
    let baked = |p: &slerp_scene::Primitive| {
        p.material
            .and_then(|m| scene.materials.get(m))
            .is_some_and(|m| m.unlit || m.lightmap_texture.is_some() || m.effect != slerp_scene::Effect::None)
    };
    if drawn.all(baked) { Tonemap::None } else { Tonemap::PbrNeutral }
}

pub fn tonemap_name(t: Tonemap) -> &'static str {
    match t {
        Tonemap::PbrNeutral => "pbrNeutral",
        Tonemap::Aces => "aces",
        Tonemap::None => "none",
    }
}

/// Reads a debug channel name as `ViewState::debug` spells it.
pub fn parse_debug(name: &str) -> Option<DebugChannel> {
    Some(match name.to_ascii_lowercase().as_str() {
        "none" | "lit" => DebugChannel::None,
        "basecolor" | "base" => DebugChannel::BaseColor,
        "normal" | "normals" => DebugChannel::Normal,
        "metallic" => DebugChannel::Metallic,
        "roughness" => DebugChannel::Roughness,
        "occlusion" => DebugChannel::Occlusion,
        "emissive" => DebugChannel::Emissive,
        "uvchecker" | "uv" => DebugChannel::UvChecker,
        _ => return None,
    })
}

pub fn debug_name(d: DebugChannel) -> &'static str {
    match d {
        DebugChannel::None => "none",
        DebugChannel::BaseColor => "baseColor",
        DebugChannel::Normal => "normal",
        DebugChannel::Metallic => "metallic",
        DebugChannel::Roughness => "roughness",
        DebugChannel::Occlusion => "occlusion",
        DebugChannel::Emissive => "emissive",
        DebugChannel::UvChecker => "uvChecker",
    }
}

/// Reads a background as `ViewState::background` spells it.
pub fn parse_background(s: &str) -> Option<Background> {
    let s = s.trim();
    if s.eq_ignore_ascii_case("transparent") {
        return Some(Background::Transparent);
    }
    if let Some(rest) = s.strip_prefix("environment") {
        let blur = rest.strip_prefix(':').and_then(|b| b.trim().parse::<f32>().ok()).unwrap_or(0.6);
        return Some(Background::Environment { blur: blur.clamp(0.0, 1.0) });
    }
    parse_hex_color(s).map(Background::Color)
}

pub fn background_name(b: Background) -> String {
    match b {
        Background::Transparent => "transparent".into(),
        Background::Environment { blur } => format!("environment:{}", (blur * 100.0).round() / 100.0),
        Background::Color(c) => {
            let byte = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
            format!("#{:02x}{:02x}{:02x}", byte(c[0]), byte(c[1]), byte(c[2]))
        }
    }
}

impl ViewState {
    pub fn load(path: &Path) -> Result<ViewState, String> {
        let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        Self::parse(&text).map_err(|e| format!("{}: {e}", path.display()))
    }

    pub fn parse(text: &str) -> Result<ViewState, String> {
        serde_json::from_str(&crate::config::strip_jsonc(text)).map_err(|e| e.to_string())
    }

    /// The state as JSONC, with a comment saying how to replay it.
    pub fn to_jsonc(&self, file: Option<&Path>) -> String {
        let json = serde_json::to_string_pretty(self).unwrap_or_else(|_| "{}".into());
        let input = file.map_or_else(|| "FILE".into(), |p| shell_quote(&p.to_string_lossy()));
        format!(
            "// Slerp view state.  Replay it with:\n//   slerp render {input} out.png --state state.jsonc\n{json}\n"
        )
    }

    /// Hides the nodes `hidden` names.
    pub fn apply_scene(&self, scene: &mut Scene) {
        if self.hidden.is_empty() {
            return;
        }
        for node in &mut scene.nodes {
            if self.hidden.iter().any(|h| h == &node.name) {
                node.visible = false;
            }
        }
    }

    fn apply_pose(&self, scene: &Scene, pose: &mut Pose) {
        if let Some(name) = &self.clip
            && let Some(i) = scene.animations.iter().position(|a| &a.name == name)
        {
            pose.sample(scene, i, self.time);
            if self.in_place
                && let Some(hold) = InPlace::new(scene, i)
            {
                hold.apply(scene, pose);
            }
        }
        if self.morphs.is_empty() {
            return;
        }
        for (ni, node) in scene.nodes.iter().enumerate() {
            let Some(mesh) = node.mesh.and_then(|m| scene.meshes.get(m)) else { continue };
            for (ti, name) in mesh.target_names.iter().enumerate() {
                if let (Some(&w), Some(slot)) = (self.morphs.get(name), pose.weights[ni].get_mut(ti)) {
                    *slot = w;
                }
            }
        }
    }

    fn view(&self, scene: &Scene, bounds: &Aabb, aspect: f32) -> View {
        if let Some(fly) = &self.fly {
            let (near, far) = fly_clip_planes(bounds);
            let cam = FlyCamera {
                position: Vec3::from(fly.position),
                yaw: fly.yaw.to_radians(),
                pitch: fly.pitch.to_radians(),
                fov_y: fly.fov.to_radians(),
                near,
                far,
            };
            return cam.view(aspect);
        }
        let orbit = self.orbit(scene);
        let mut cam = OrbitCamera {
            target: Vec3::ZERO,
            yaw: orbit.yaw.to_radians(),
            pitch: orbit.pitch.to_radians(),
            distance: 1.0,
            fov_y: orbit.fov.to_radians(),
            ortho: orbit.ortho,
        };
        cam.frame(bounds, aspect);
        if let Some(d) = orbit.distance {
            cam.distance = d;
        }
        if let Some(t) = orbit.target {
            cam.target = Vec3::from(t);
        }
        let mut view = cam.view(aspect);
        // Gaussian splat captures surround their subject with foliage,
        // walls or a shell of floaters, which can stand between it and the
        // framing camera.  When the camera frames itself, splats nearer
        // than the framed sphere are left out, so the subject shows.
        if scene.splats.is_some() && orbit.distance.is_none() && !cam.ortho && !bounds.is_empty() {
            let near = cam.distance - bounds.radius() * 1.05;
            if near > view.near {
                view.proj = glam::camera::rh::proj::directx::perspective(cam.fov_y, aspect, near, view.far);
                view.near = near;
            }
        }
        view
    }

    /// The orbit the camera takes: the state's own, or the default for the
    /// scene's kind.
    fn orbit(&self, scene: &Scene) -> OrbitState {
        let mut orbit = self.orbit.clone().unwrap_or_else(|| default_orbit(scene));
        if self.orbit.is_none() && scene.kind == SceneKind::MaterialBall {
            orbit.pitch = 15.0;
        }
        orbit
    }

    fn settings(&self, scene: &Scene) -> RenderSettings {
        let mut s = RenderSettings::thumbnail();
        s.exposure = self.exposure;
        if let Some(bg) = self.background.as_deref().and_then(parse_background) {
            s.background = bg;
        }
        s.tonemap = self.tonemap.as_deref().and_then(parse_tonemap).unwrap_or_else(|| default_tonemap(scene));
        if let Some(d) = self.debug.as_deref().and_then(parse_debug) {
            s.debug = d;
        }
        s.environment_rotation = self.environment_rotation.to_radians();
        s.environment_intensity = self.environment_intensity.unwrap_or(1.0);
        s.key_light = self.key_light.unwrap_or(1.0);
        s.fill = fill_light(scene, self.fill);
        s.shadows = self.shadows.unwrap_or(true);
        s.overlays.wireframe = self.wireframe;
        s.overlays.normals = self.normals;
        s.overlays.tangents = self.tangents;
        s.overlays.bounds = self.bounds;
        s.overlays.skeleton = self.skeleton;
        s.overlays.grid = self.grid;
        s.overlays.markers = self.markers;
        s.splat_scale = self.splat_scale.unwrap_or(1.0);
        s.ao = self.ambient_occlusion.unwrap_or(s.ao);
        s.ao_strength = self.ao_strength.unwrap_or(1.0);
        s.point_size = self.point_size.unwrap_or(1.0);
        s.point_world_size = self.point_world_size;
        // Levels have no single object to ground, so no contact shadow.
        s.ground_shadow = scene.kind == SceneKind::Object;
        s
    }
}

/// How lighting environments are decoded: the first layer, no larger than
/// 2048 pixels, which is far more than the prefiltered lighting keeps.
pub fn environment_decode_options() -> slerp_textures::DecodeOptions {
    slerp_textures::DecodeOptions { min_size: None, max_size: Some(2048), all_mips: false, first_layer_only: true }
}

/// Quotes a path for a shell command line when it needs it.
fn shell_quote(s: &str) -> String {
    if !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || "/._-+,:@".contains(c)) {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}

/// Objects get a three-quarter view from slightly above; levels a higher
/// view that shows their layout.
fn default_orbit(scene: &Scene) -> OrbitState {
    // A MetaHuman head is framed as a portrait, in a square picture.
    if let Some(p) = slerp_dna::portrait(scene) {
        let half = (OrbitState::default().fov * 0.5).to_radians();
        return OrbitState {
            yaw: p.yaw,
            pitch: p.pitch,
            distance: Some(p.radius / half.sin()),
            target: Some(p.target.to_array()),
            ..Default::default()
        };
    }
    match scene.kind {
        SceneKind::Level => OrbitState { yaw: 35.0, pitch: 45.0, ..Default::default() },
        _ => OrbitState::default(),
    }
}

fn parse_hex_color(s: &str) -> Option<[f32; 4]> {
    let hex = s.strip_prefix('#')?;
    if hex.len() != 6 {
        return None;
    }
    let v = u32::from_str_radix(hex, 16).ok()?;
    let c = |shift: u32| ((v >> shift) & 255) as f32 / 255.0;
    Some([c(16), c(8), c(0), 1.0])
}

#[cfg(test)]
mod view_state_tests {
    use super::*;
    use slerp_scene::{Animation, Channel, Interpolation, Material, Node, TexRef, Transform};

    #[test]
    fn round_trips_through_jsonc() {
        let mut state = ViewState {
            orbit: Some(OrbitState { yaw: 12.5, target: Some([1.0, 2.0, 3.0]), ..Default::default() }),
            clip: Some("Walk_Loop".into()),
            time: 0.5,
            in_place: true,
            tonemap: Some("aces".into()),
            background: Some(background_name(Background::Environment { blur: 0.25 })),
            environment: Some(PathBuf::from("/tmp/sky one.hdr")),
            hidden: vec!["eyeshell".into()],
            skeleton: true,
            material: Some(MaterialState { shape: "cube".into(), tiling: 2.0, displacement: Some(0.01) }),
            ambient_occlusion: Some(false),
            ao_strength: Some(1.5),
            point_size: Some(2.0),
            point_world_size: true,
            ..Default::default()
        };
        state.morphs.insert("jawOpen".into(), 0.75);
        let text = state.to_jsonc(Some(Path::new("/a b/c.glb")));
        assert!(text.starts_with("// Slerp view state"));
        assert!(text.contains("'/a b/c.glb'"));
        let back = ViewState::parse(&text).unwrap();
        assert_eq!(back.orbit.as_ref().unwrap().target, Some([1.0, 2.0, 3.0]));
        assert_eq!(back.clip.as_deref(), Some("Walk_Loop"));
        assert!(back.in_place && back.skeleton && !back.wireframe);
        assert_eq!(back.morphs.get("jawOpen"), Some(&0.75));
        assert_eq!(back.material, state.material);
        let s = back.settings(&Scene::default());
        assert_eq!(s.tonemap, Tonemap::Aces);
        assert_eq!(s.background, Background::Environment { blur: 0.25 });
        assert!(s.overlays.skeleton);
        assert!(!s.ao && s.ao_strength == 1.5);
        assert!(s.point_world_size && s.point_size == 2.0);
        let plain = ViewState::default().settings(&Scene::default());
        assert_eq!(plain.ao, RenderSettings::thumbnail().ao, "no setting means what thumbnails do");
    }

    #[test]
    fn old_states_still_parse() {
        let state =
            ViewState::parse(r##"{ "orbit": { "yaw": 10 }, "background": "#ff8000", "grid": true, }"##).unwrap();
        let s = state.settings(&Scene::default());
        assert_eq!(s.background, Background::Color([1.0, 128.0 / 255.0, 0.0, 1.0]));
        assert!(s.overlays.grid);
        assert_eq!(state.orbit.unwrap().pitch, 20.0);
    }

    /// Scenes whose light is baked into what they store get no tone
    /// mapping; anything lit by Slerp's lights, or a scene with nothing
    /// drawn, keeps PBR Neutral.
    #[test]
    fn baked_light_scenes_skip_tone_mapping() {
        let with = |materials: Vec<Material>| {
            let mut scene = Scene::default();
            let mut prims = Vec::new();
            for i in 0..materials.len() {
                prims.push(slerp_scene::Primitive { material: Some(i), ..Shape::Cube.primitive(1) });
            }
            scene.materials = materials;
            scene.add_mesh_node(slerp_scene::Mesh { name: "m".into(), primitives: prims, target_names: vec![] });
            default_tonemap(&scene)
        };
        let unlit = Material { unlit: true, ..Default::default() };
        let lightmapped = Material { lightmap_texture: Some(TexRef::new(0)), ..Default::default() };
        let sky = Material { effect: slerp_scene::Effect::PanoramaSky, ..Default::default() };
        assert_eq!(with(vec![unlit.clone(), lightmapped.clone(), sky]), Tonemap::None);
        assert_eq!(with(vec![unlit, lightmapped, Material::default()]), Tonemap::PbrNeutral);
        assert_eq!(default_tonemap(&Scene::default()), Tonemap::PbrNeutral);
        let state = ViewState { tonemap: Some("aces".into()), ..Default::default() };
        let mut level = Scene::default();
        level.materials.push(Material { unlit: true, ..Default::default() });
        level.add_mesh_node(slerp_scene::Mesh {
            name: "m".into(),
            primitives: vec![slerp_scene::Primitive { material: Some(0), ..Shape::Cube.primitive(1) }],
            target_names: vec![],
        });
        assert_eq!(ViewState::default().settings(&level).tonemap, Tonemap::None);
        assert_eq!(state.settings(&level).tonemap, Tonemap::Aces, "a chosen tone mapping stays");
    }

    #[test]
    fn names_parse_back() {
        for t in [Tonemap::PbrNeutral, Tonemap::Aces, Tonemap::None] {
            assert_eq!(parse_tonemap(tonemap_name(t)), Some(t));
        }
        for d in [DebugChannel::None, DebugChannel::Normal, DebugChannel::UvChecker, DebugChannel::Emissive] {
            assert_eq!(parse_debug(debug_name(d)), Some(d));
        }
        assert_eq!(parse_background("#000000"), Some(Background::Color([0.0, 0.0, 0.0, 1.0])));
        assert_eq!(parse_background("nonsense"), None);
    }

    /// A root node turned 90 degrees about Y with a child whose clip walks
    /// along its local X, which is world -Z.
    fn walking_scene() -> Scene {
        let mut scene = Scene::default();
        let mut root = Node::new("root");
        root.transform =
            Transform { rotation: glam::Quat::from_rotation_y(std::f32::consts::FRAC_PI_2), ..Transform::IDENTITY };
        root.children = vec![1];
        let mut hips = Node::new("hips");
        hips.transform.translation = Vec3::new(0.0, 1.0, 0.0);
        scene.nodes = vec![root, hips];
        scene.roots = vec![0];
        scene.animations.push(Animation {
            name: "walk".into(),
            duration: 1.0,
            channels: vec![Channel {
                node: 1,
                property: Property::Translation,
                interpolation: Interpolation::Linear,
                times: vec![0.0, 1.0],
                values: vec![0.0, 1.0, 0.0, 2.0, 1.1, 0.0],
            }],
        });
        scene
    }

    #[test]
    fn in_place_keeps_height_and_drops_travel() {
        let scene = walking_scene();
        let hold = InPlace::new(&scene, 0).unwrap();
        assert_eq!(hold.node(), 1);
        let mut pose = Pose::rest(&scene);
        pose.sample(&scene, 0, 1.0);
        hold.apply(&scene, &mut pose);
        pose.update_world(&scene);
        let p = pose.world[1].w_axis.truncate();
        assert!((p - Vec3::new(0.0, 1.1, 0.0)).length() < 1e-5, "{p}");
    }

    /// Mean absolute difference between two pictures, per channel value.
    fn difference(a: &Image8, b: &Image8) -> f64 {
        assert_eq!((a.width, a.height), (b.width, b.height));
        let sum: u64 = a.pixels.iter().zip(&b.pixels).map(|(x, y)| x.abs_diff(*y) as u64).sum();
        sum as f64 / a.pixels.len() as f64
    }

    /// Each turntable frame is the thumbnail of the model turned by that
    /// frame's angle: the lights turn with the camera, and the framing
    /// stays put.  A box twice as wide as it is tall and half as deep shows
    /// its narrow side after a quarter turn, anticlockwise from above.
    /// Rendered on lavapipe, so it runs without a GPU.
    #[test]
    fn turntable_frames_are_thumbnails_of_the_model_turned() {
        let mut scene = Scene::default();
        let shape = Shape::Cube.primitive(1);
        scene.add_mesh_node(slerp_scene::Mesh { name: "box".into(), primitives: vec![shape], target_names: vec![] });
        scene.nodes[0].transform.scale = Vec3::new(2.0, 1.0, 0.5);
        let mut renderer = ThumbRenderer::new("software");
        let frames = match renderer.turntable(&scene, 96, 4) {
            Ok(frames) => frames,
            Err(ThumbError::NoGpu(e)) => return eprintln!("skipped, no lavapipe: {e}"),
            Err(e) => panic!("{e}"),
        };
        assert_eq!(frames.len(), 4);
        assert!(frames.iter().all(|f| (f.width, f.height) == (96, 96)));
        let thumbnail = renderer.render_thumbnail(&scene, 96).unwrap();
        assert!(difference(&frames[0], &thumbnail) < 0.01, "frame 0 is the thumbnail");
        scene.nodes[0].transform.rotation = glam::Quat::from_rotation_y(std::f32::consts::FRAC_PI_2);
        let turned = renderer.render_thumbnail(&scene, 96).unwrap();
        let same = difference(&frames[1], &turned);
        let other = difference(&frames[0], &frames[1]);
        assert!(same < 1.0, "a quarter turn is the model turned: {same}");
        assert!(other > 5.0 * same.max(0.2), "a quarter turn shows another side: {other} vs {same}");
    }

    #[test]
    fn material_tiling_scales_every_texture() {
        let material = Material {
            base_color_texture: Some(TexRef::new(0)),
            normal_texture: Some(TexRef {
                transform: Some(UvTransform { offset: Vec2::new(0.5, 0.0), rotation: 0.3, scale: Vec2::splat(2.0) }),
                ..TexRef::new(0)
            }),
            height_scale: 1.0,
            ..Default::default()
        };
        let asset =
            MaterialAsset { name: "m".into(), material, textures: vec![], preview: None, info: Default::default() };
        let scene = material_scene(&asset, &MaterialState { tiling: 3.0, ..Default::default() });
        let m = &scene.materials[0];
        assert_eq!(m.base_color_texture.unwrap().transform.unwrap().scale, Vec2::splat(3.0));
        let n = m.normal_texture.unwrap().transform.unwrap();
        assert_eq!((n.offset, n.rotation, n.scale), (Vec2::new(1.5, 0.0), 0.3, Vec2::splat(6.0)));
        assert_eq!(m.height_scale, DEFAULT_MAX_DISPLACEMENT);
        let flat = material_scene(
            &asset,
            &MaterialState { shape: "plane".into(), displacement: Some(0.0), ..Default::default() },
        );
        assert_eq!(flat.materials[0].height_scale, 0.0);
    }

    /// Only scenes without light of their own get a fill light: they start
    /// with their own default, and a chosen value replaces it.
    #[test]
    fn fill_light_is_only_for_scenes_without_light() {
        let authored = Scene::default();
        let editor = Scene { fill: 0.5, ..Default::default() };
        assert_eq!(fill_light(&authored, None), 0.0);
        assert_eq!(fill_light(&authored, Some(1.5)), 0.0);
        assert_eq!(fill_light(&editor, None), 0.5);
        assert_eq!(fill_light(&editor, Some(1.5)), 1.5);
        assert_eq!(fill_light(&editor, Some(-1.0)), 0.0);
        let state = ViewState { fill: Some(0.25), ..Default::default() };
        assert_eq!(state.settings(&editor).fill, 0.25);
        assert_eq!(state.settings(&authored).fill, 0.0);
        assert_eq!(ViewState::default().settings(&editor).fill, 0.5);
    }
}
