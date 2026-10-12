//! A 3D scene in the viewer: its GPU copy, the current pose (clip, morphs,
//! visibility), the cameras, and what the pointer is over.

use crate::anim::{MorphList, Morphs, Player, has_arkit};
use crate::camera::{Cameras, Mode};
use crate::gfx::{Gfx, next_id};
use glam::{Mat4, Vec2, Vec3};
use slerp_render::GpuScene;
use slerp_scene::{Aabb, Effect, Pose, Scene, SceneKind, Topology, View};

pub struct SceneView {
    /// Distinguishes this scene in frame keys.
    pub id: u64,
    pub scene: Scene,
    pub gpu: GpuScene,
    pub pose: Pose,
    scratch: Pose,
    pub player: Player,
    pub morphs: Morphs,
    pub morph_list: MorphList,
    /// The targets are Apple's ARKit face shapes.
    pub arkit: bool,
    pub cameras: Cameras,
    /// Bumped every time the pose on the GPU changes.
    pub pose_version: u64,
    /// The pose must be recomputed and uploaded before the next frame.
    pub pose_dirty: bool,
    /// Seconds of Quake liquid and sky animation shown.
    pub effects_time: f32,
    /// The scene has liquids or skies that move.
    pub has_effects: bool,
    /// Nodes that are joints of a skin, for the skeleton overlay.
    pub joints: Vec<usize>,
    pub lods: Option<Lods>,
    pub selected_material: Option<usize>,
    /// Each node's visibility as the file had it, so a copied view state
    /// lists only the nodes the viewer hid.
    pub initial_visible: Vec<bool>,
    /// A MetaHuman DNA file's rigs, evaluated into every pose.
    pub dna: Option<Box<crate::dna::DnaState>>,
    /// A G-code toolpath or GPS track and its controls (see `paths_view`).
    pub paths: Option<crate::paths_view::PathsState>,
    /// How many of `scene.animations` came from the file; the rest were
    /// added from animation libraries, and a reload carries them over.
    pub own_clips: usize,
    /// The Foot IK switch for the added clips.
    pub foot_ik: crate::anim::FootIk,
    /// Textures uploaded since the first frame (see `streaming`), so the
    /// view redraws as they arrive.
    pub textures_arrived: u64,
    /// Frames this view has been updated for.
    updates: u64,
}

/// How long a frame may spend uploading textures that arrive after the
/// first one, so the window stays responsive while a large model's
/// textures go up.
const UPLOAD_BUDGET: std::time::Duration = std::time::Duration::from_millis(50);

impl SceneView {
    /// Uploads `scene` and sets up the cameras: an orbit around objects, or
    /// the fly camera at a level's spawn point.
    pub fn new(scene: Scene, gfx: &mut Gfx, aspect: f32) -> SceneView {
        let pitch = if scene.kind == SceneKind::MaterialBall { 15.0 } else { 20.0 };
        Self::with_pitch(scene, gfx, aspect, pitch)
    }

    /// As `new`, but the geometry is drawn before the textures are all up:
    /// they go up over the next frames (see `update`), with their small
    /// mips or their materials' plain colours meanwhile.  For files, where a
    /// large model's textures would otherwise hold up its first frame.
    /// `geometry` is the scene's, packed while it loaded.
    pub fn streaming(
        scene: Scene,
        geometry: Option<slerp_render::PackedGeometry>,
        gfx: &mut Gfx,
        aspect: f32,
    ) -> SceneView {
        let pitch = if scene.kind == SceneKind::MaterialBall { 15.0 } else { 20.0 };
        Self::build(scene, gfx, aspect, pitch, Some(geometry))
    }

    /// As `new`, with the orbit camera starting `pitch` degrees above the
    /// horizon.
    pub fn with_pitch(scene: Scene, gfx: &mut Gfx, aspect: f32, pitch: f32) -> SceneView {
        Self::build(scene, gfx, aspect, pitch, None)
    }

    /// `stream` holds the packed geometry, if any, when textures go up
    /// after the first frame.
    fn build(
        mut scene: Scene,
        gfx: &mut Gfx,
        aspect: f32,
        pitch: f32,
        stream: Option<Option<slerp_render::PackedGeometry>>,
    ) -> SceneView {
        let initial_visible = scene.nodes.iter().map(|n| n.visible).collect();
        let lods = Lods::find(&scene);
        if let Some(l) = &lods {
            l.apply(&mut scene);
        }
        let gpu = match stream {
            Some(Some(geometry)) => gfx.renderer.prepare_packed(&gfx.gpu, &scene, geometry),
            Some(None) => gfx.renderer.prepare_untextured(&gfx.gpu, &scene),
            None => gfx.renderer.prepare(&gfx.gpu, &scene),
        };
        let pose = Pose::rest(&scene);
        let scratch = pose.clone();
        let player = Player::new(&scene);
        let morph_list = MorphList::new(&scene);
        let arkit = has_arkit(&morph_list);
        let bounds = gpu.bounds;
        let level = scene.kind == SceneKind::Level;
        let spawn = scene.spawn.map(|s| (s.position, s.yaw));
        let mut cameras = Cameras::new(bounds, aspect, pitch, spawn, level);
        // A MetaHuman head opens on its face, as its thumbnail shows it,
        // even with the body loaded beside it; F frames the whole figure.
        if let Some(p) = slerp_dna::portrait(&scene) {
            let mut orbit = cameras.orbit;
            orbit.yaw = p.yaw.to_radians();
            orbit.pitch = p.pitch.to_radians();
            orbit.target = p.target;
            orbit.distance = p.radius / (orbit.fov_y * 0.5).sin();
            cameras.set_home_orbit(orbit);
        }
        // A SketchUp model opens at the view it was saved with.
        if let Some(orbit) = slerp_core::saved_view(&scene, &bounds) {
            cameras.set_home_orbit(orbit);
        }
        let mut is_joint = vec![false; scene.nodes.len()];
        for s in &scene.skins {
            for &j in &s.joints {
                if let Some(slot) = is_joint.get_mut(j) {
                    *slot = true;
                }
            }
        }
        let joints = (0..scene.nodes.len()).filter(|&i| is_joint[i]).collect();
        // Effects that move with time: Quake's liquids and sky.
        let has_effects = scene.materials.iter().any(|m| matches!(m.effect, Effect::Warp | Effect::Sky));
        let own_clips = scene.animations.len();
        SceneView {
            id: next_id(),
            morphs: Morphs::new(&scene),
            scene,
            gpu,
            pose,
            scratch,
            player,
            morph_list,
            arkit,
            cameras,
            pose_version: 0,
            pose_dirty: true,
            effects_time: 0.0,
            has_effects,
            joints,
            lods,
            selected_material: None,
            initial_visible,
            dna: None,
            paths: None,
            own_clips,
            foot_ik: Default::default(),
            textures_arrived: 0,
            updates: 0,
        }
    }

    /// Whether textures are still on their way to the GPU.
    pub fn textures_pending(&self) -> bool {
        slerp_render::Renderer::textures_pending(&self.gpu)
    }

    /// Moves animation on by `dt` and uploads the pose if anything changed.
    /// Returns whether the scene is still moving by itself.
    pub fn update(&mut self, gfx: &mut Gfx, dt: f32, animate_effects: bool) -> bool {
        crate::dna::update(self, gfx);
        crate::paths_view::update(self, gfx);
        // Textures still to go up take their share of each frame after the
        // first, which shows the model without them.
        let mut uploading = false;
        if self.textures_pending() {
            uploading = true;
            if self.updates > 0 {
                uploading = gfx.renderer.upload_textures(&gfx.gpu, &mut self.gpu, &self.scene, UPLOAD_BUDGET);
                self.textures_arrived += 1;
            }
        }
        self.updates += 1;
        if self.player.advance(&self.scene, dt) {
            self.pose_dirty = true;
        }
        if animate_effects && self.has_effects {
            self.effects_time += dt;
        }
        if self.pose_dirty {
            self.refresh_pose(gfx);
        }
        uploading || self.player.is_animating() || (animate_effects && self.has_effects)
    }

    /// Recomputes the pose from the clip, morphs and visibility, and
    /// uploads it.
    pub fn refresh_pose(&mut self, gfx: &mut Gfx) {
        self.player.sample(&self.scene, &mut self.pose, &mut self.scratch);
        if let Some(dna) = &mut self.dna {
            dna.apply(&mut self.pose);
        }
        self.morphs.apply(&self.scene, &mut self.pose);
        self.pose.update_world(&self.scene);
        gfx.renderer.update_pose(&gfx.gpu, &mut self.gpu, &self.scene, &self.pose);
        self.pose_version += 1;
        self.pose_dirty = false;
        self.cameras.set_bounds(self.gpu.bounds);
    }

    pub fn set_visible(&mut self, node: usize, visible: bool) {
        if let Some(n) = self.scene.nodes.get_mut(node)
            && n.visible != visible
        {
            n.visible = visible;
            self.pose_dirty = true;
        }
    }

    pub fn set_lod(&mut self, level: Option<u32>) {
        if let Some(l) = &mut self.lods {
            l.current = level;
            l.apply(&mut self.scene);
            self.pose_dirty = true;
        }
    }

    /// Which nodes are drawn: visible themselves and under visible parents.
    pub fn effective_visibility(&self) -> Vec<bool> {
        let n = self.scene.nodes.len();
        let mut out = vec![false; n];
        let mut stack: Vec<usize> = self.scene.roots.clone();
        let mut seen = vec![false; n];
        while let Some(i) = stack.pop() {
            if i >= n || seen[i] {
                continue;
            }
            seen[i] = true;
            if self.scene.nodes[i].visible {
                out[i] = true;
                stack.extend(self.scene.nodes[i].children.iter().copied());
            }
        }
        out
    }

    /// Bounds for framing: what is visible, or the rest bounds when
    /// everything is hidden.
    pub fn frame_bounds(&self) -> Aabb {
        if self.gpu.bounds.is_empty() { self.gpu.rest_bounds } else { self.gpu.bounds }
    }

    /*┌─────────────────╮
    | Picking         |
    └─────────────────╯*/
    //region Picking

    /// The nearest visible surface point along a ray, skinned meshes posed
    /// on the CPU, or the nearest splat on it.  Used to set the orbit pivot
    /// by double-clicking.
    pub fn pick(&self, origin: Vec3, dir: Vec3) -> Option<Vec3> {
        let visible = self.effective_visibility();
        let mut best = f32::INFINITY;
        for (ni, node) in self.scene.nodes.iter().enumerate() {
            let (Some(mi), true) = (node.mesh, visible.get(ni).copied().unwrap_or(false)) else { continue };
            let Some(mesh) = self.scene.meshes.get(mi) else { continue };
            let joints =
                node.skin.filter(|&s| s < self.scene.skins.len()).map(|s| self.pose.joint_matrices(&self.scene, s));
            let world = self.pose.world.get(ni).copied().unwrap_or(Mat4::IDENTITY);
            for prim in &mesh.primitives {
                if prim.topology != Topology::Triangles || prim.positions.is_empty() {
                    continue;
                }
                if self.scene.materials.get(prim.material.unwrap_or(usize::MAX)).is_some_and(|m| m.effect.is_sky()) {
                    continue;
                }
                let posed: Vec<Vec3> = match (&joints, prim.is_skinned()) {
                    (Some(joints), true) => prim
                        .positions
                        .iter()
                        .enumerate()
                        .map(|(i, &p)| {
                            let (j, w) = (prim.joints[i], prim.weights[i]);
                            let mut m = Mat4::ZERO;
                            for k in 0..4 {
                                if w[k] != 0.0 {
                                    m += joints.get(j[k] as usize).copied().unwrap_or(Mat4::IDENTITY) * w[k];
                                }
                            }
                            m.transform_point3(p)
                        })
                        .collect(),
                    _ => prim.positions.iter().map(|&p| world.transform_point3(p)).collect(),
                };
                for tri in prim.indices.chunks_exact(3) {
                    let get = |i: u32| posed.get(i as usize).copied();
                    let (Some(a), Some(b), Some(c)) = (get(tri[0]), get(tri[1]), get(tri[2])) else { continue };
                    if let Some(t) = ray_triangle(origin, dir, a, b, c)
                        && t < best
                    {
                        best = t;
                    }
                }
            }
        }
        // Gaussian splats: the nearest fairly opaque splat whose centre is
        // within two standard deviations of the ray, or a few thousandths
        // of its distance, so small splats can still be hit.
        if let Some(splats) = &self.scene.splats {
            let len2 = dir.length_squared().max(1e-20);
            for (i, &p) in splats.positions.iter().enumerate() {
                if splats.colors.get(i).is_none_or(|c| c.w < 0.3) {
                    continue;
                }
                let to = p - origin;
                let t = to.dot(dir) / len2;
                if t <= 0.0 || t >= best {
                    continue;
                }
                let along = t * len2.sqrt();
                let reach = (splats.scales.get(i).map_or(0.0, |s| s.max_element()) * 2.0).max(along * 0.003);
                if (to - dir * t).length_squared() <= reach * reach {
                    best = t;
                }
            }
        }
        best.is_finite().then(|| origin + dir * best)
    }

    /// The joint whose position is nearest `pointer` (in points, within
    /// `radius`), for naming joints under the mouse in the skeleton overlay.
    pub fn joint_near(&self, view: &View, rect: egui::Rect, pointer: egui::Pos2, radius: f32) -> Option<usize> {
        let vp = view.view_proj();
        let mut best = (radius * radius, None);
        for &j in &self.joints {
            let Some(m) = self.pose.world.get(j) else { continue };
            if let Some(p) = project(&vp, m.w_axis.truncate(), rect) {
                let d = p.distance_sq(pointer);
                if d < best.0 {
                    best = (d, Some(j));
                }
            }
        }
        best.1
    }

    /// The marker nearest `pointer`, as for joints.
    pub fn marker_near(&self, view: &View, rect: egui::Rect, pointer: egui::Pos2, radius: f32) -> Option<usize> {
        let vp = view.view_proj();
        let mut best = (radius * radius, None);
        for (i, m) in self.scene.markers.iter().enumerate() {
            if let Some(p) = project(&vp, m.position, rect) {
                let d = p.distance_sq(pointer);
                if d < best.0 {
                    best = (d, Some(i));
                }
            }
        }
        best.1
    }

    //endregion

    /// The fly camera is in use.
    pub fn flying(&self) -> bool {
        self.cameras.mode == Mode::Fly
    }
}

/// Where a world point lands in `rect`, or `None` when it is behind the
/// camera or outside the depth range.
pub fn project(view_proj: &Mat4, p: Vec3, rect: egui::Rect) -> Option<egui::Pos2> {
    let clip = *view_proj * p.extend(1.0);
    if clip.w <= 1e-6 {
        return None;
    }
    let ndc = clip.truncate() / clip.w;
    if !(0.0..=1.0).contains(&ndc.z) {
        return None;
    }
    Some(egui::pos2(rect.left() + (ndc.x * 0.5 + 0.5) * rect.width(), rect.top() + (0.5 - ndc.y * 0.5) * rect.height()))
}

/// The pointer's position in normalised device coordinates of `rect`.
pub fn to_ndc(rect: egui::Rect, pos: egui::Pos2) -> Vec2 {
    Vec2::new(
        (pos.x - rect.left()) / rect.width().max(1.0) * 2.0 - 1.0,
        1.0 - (pos.y - rect.top()) / rect.height().max(1.0) * 2.0,
    )
}

/// Möller and Trumbore's ray-triangle test, both faces.  Returns the
/// distance along the ray.
fn ray_triangle(o: Vec3, d: Vec3, a: Vec3, b: Vec3, c: Vec3) -> Option<f32> {
    let e1 = b - a;
    let e2 = c - a;
    let p = d.cross(e2);
    let det = e1.dot(p);
    if det.abs() < 1e-12 {
        return None;
    }
    let inv = 1.0 / det;
    let s = o - a;
    let u = s.dot(p) * inv;
    if !(0.0..=1.0).contains(&u) {
        return None;
    }
    let q = s.cross(e1);
    let v = d.dot(q) * inv;
    if v < 0.0 || u + v > 1.0 {
        return None;
    }
    let t = e2.dot(q) * inv;
    (t > 0.0).then_some(t)
}

/*┌─────────────────╮
| Levels of detail|
└─────────────────╯*/
//region Levels of detail

/// Meshes exported once per level of detail and told apart by a name
/// ending in `lod0`, `_LOD1` and so on.  Drawing all of them at once
/// stacks the copies, so only one level shows.
pub struct Lods {
    /// The levels present, ascending.
    pub levels: Vec<u32>,
    /// Each node's level, if its name has one.
    node_level: Vec<Option<u32>>,
    /// The level shown; `None` shows all.
    pub current: Option<u32>,
}

impl Lods {
    pub fn find(scene: &Scene) -> Option<Lods> {
        let node_level: Vec<Option<u32>> =
            scene.nodes.iter().map(|n| if n.mesh.is_some() { lod_suffix(&n.name) } else { None }).collect();
        let mut levels: Vec<u32> = node_level.iter().flatten().copied().collect();
        levels.sort_unstable();
        levels.dedup();
        (levels.len() > 1).then(|| Lods { current: levels.first().copied(), levels, node_level })
    }

    pub fn apply(&self, scene: &mut Scene) {
        for (node, level) in scene.nodes.iter_mut().zip(&self.node_level) {
            if let Some(level) = level {
                node.visible = self.current.is_none_or(|c| c == *level);
            }
        }
    }
}

/// The level of detail a node name ends with: "hip lod1", "Body_LOD2",
/// "rock-lod0" give 1, 2 and 0.
fn lod_suffix(name: &str) -> Option<u32> {
    let lower = name.to_ascii_lowercase();
    let at = lower.rfind("lod")?;
    let digits = &lower[at + 3..];
    if digits.is_empty() || digits.len() > 2 || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let before = lower[..at].chars().last();
    if !matches!(before, None | Some(' ' | '_' | '-' | '.')) {
        return None;
    }
    digits.parse().ok()
}

//endregion

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lod_names() {
        assert_eq!(lod_suffix("hip lod1"), Some(1));
        assert_eq!(lod_suffix("Body_LOD2"), Some(2));
        assert_eq!(lod_suffix("rock-lod0"), Some(0));
        assert_eq!(lod_suffix("lod3"), Some(3));
        assert_eq!(lod_suffix("pilodx"), None);
        assert_eq!(lod_suffix("melod1"), None);
        assert_eq!(lod_suffix("hip lod"), None);
    }

    #[test]
    fn ray_hits_triangle() {
        let t = ray_triangle(Vec3::new(0.2, 0.2, 5.0), Vec3::NEG_Z, Vec3::ZERO, Vec3::X, Vec3::Y);
        assert_eq!(t, Some(5.0));
        assert_eq!(ray_triangle(Vec3::new(2.0, 2.0, 5.0), Vec3::NEG_Z, Vec3::ZERO, Vec3::X, Vec3::Y), None);
    }

    #[test]
    fn projection_lands_in_rect() {
        let view = slerp_scene::OrbitCamera::framing(&Aabb { min: Vec3::splat(-1.0), max: Vec3::ONE }, 1.0).view(1.0);
        let rect = egui::Rect::from_min_size(egui::pos2(10.0, 20.0), egui::vec2(100.0, 100.0));
        let p = project(&view.view_proj(), Vec3::ZERO, rect).unwrap();
        assert!((p - egui::pos2(60.0, 70.0)).length() < 1e-3, "{p:?}");
        let back = to_ndc(rect, p);
        assert!(back.length() < 1e-4);
    }
}
