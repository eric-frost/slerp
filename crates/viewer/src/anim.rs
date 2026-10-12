//! Posing a scene: the clip player (time, speed, loop, crossfades, root
//! motion), the Foot IK switch for retargeted clips, morph target
//! overrides, VRM expressions, and the ARKit face shapes grouped by area
//! with expression presets.

use slerp_core::InPlace;
use slerp_scene::{Animation, Pose, Scene};
use std::collections::{BTreeMap, HashMap};

/*┌─────────────────╮
| Clip player     |
└─────────────────╯*/
//region Clip player

/// Seconds a switch between clips blends over.
const CROSSFADE: f32 = 0.25;

/// Plays one clip at a time, crossfading from the previous one when the
/// clip changes.  `None` is the rest pose.
pub struct Player {
    pub clip: Option<usize>,
    pub time: f32,
    pub speed: f32,
    pub playing: bool,
    pub looping: bool,
    /// Lets the clip move the model around; off holds the motion root in
    /// place so walks and runs stay on the spot.
    pub root_motion: bool,
    hold: Option<InPlace>,
    fade: Option<Fade>,
}

/// The clip being faded out.
struct Fade {
    clip: Option<usize>,
    time: f32,
    hold: Option<InPlace>,
    elapsed: f32,
}

impl Player {
    /// A player for `scene` that starts on its idle clip, or the first one.
    pub fn new(scene: &Scene) -> Player {
        let clip = default_clip(scene);
        Player {
            clip,
            time: 0.0,
            speed: 1.0,
            playing: clip.is_some(),
            looping: true,
            root_motion: true,
            hold: None,
            fade: None,
        }
    }

    pub fn duration(&self, scene: &Scene) -> f32 {
        self.clip.and_then(|c| scene.animations.get(c)).map_or(0.0, |a| a.duration.max(0.0))
    }

    /// Switches clips, crossfading from the current pose.
    pub fn select(&mut self, scene: &Scene, clip: Option<usize>) {
        let clip = clip.filter(|&c| c < scene.animations.len());
        if clip == self.clip {
            return;
        }
        self.fade = Some(Fade { clip: self.clip, time: self.time, hold: self.hold.take(), elapsed: 0.0 });
        self.clip = clip;
        self.time = 0.0;
        self.update_hold(scene);
        if clip.is_some() {
            self.playing = true;
        }
    }

    /// Recomputes which node is held in place, after `root_motion` or the
    /// clip changed.
    pub fn update_hold(&mut self, scene: &Scene) {
        self.hold = match (self.root_motion, self.clip) {
            (false, Some(c)) => InPlace::new(scene, c),
            _ => None,
        };
    }

    /// Whether the pose changes over time: playing, or still crossfading.
    pub fn is_animating(&self) -> bool {
        (self.playing && self.clip.is_some()) || self.fade.is_some()
    }

    /// Moves time on by `dt` seconds.  Returns whether the pose changed.
    pub fn advance(&mut self, scene: &Scene, dt: f32) -> bool {
        let mut changed = false;
        if let Some(fade) = &mut self.fade {
            fade.elapsed += dt;
            fade.time += dt * self.speed;
            if let Some(d) = fade.clip.and_then(|c| scene.animations.get(c)).map(|a| a.duration)
                && d > 0.0
            {
                fade.time = fade.time.rem_euclid(d);
            }
            if fade.elapsed >= CROSSFADE {
                self.fade = None;
            }
            changed = true;
        }
        if self.playing && self.clip.is_some() {
            let duration = self.duration(scene);
            let mut t = self.time + dt * self.speed;
            if duration <= 0.0 {
                t = 0.0;
            } else if self.looping {
                t = t.rem_euclid(duration);
            } else if t >= duration || t < 0.0 {
                t = t.clamp(0.0, duration);
                self.playing = false;
            }
            changed |= t != self.time;
            self.time = t;
        }
        changed
    }

    /// Sets `pose` to the current clip at the current time, blended with
    /// the clip being faded out.  `scratch` is reused between frames.
    /// World matrices are left for the caller to update.
    pub fn sample(&self, scene: &Scene, pose: &mut Pose, scratch: &mut Pose) {
        pose.reset(scene);
        if let Some(c) = self.clip {
            pose.sample(scene, c, self.time);
            if let Some(hold) = &self.hold {
                hold.apply(scene, pose);
            }
        }
        if let Some(fade) = &self.fade {
            scratch.reset(scene);
            if let Some(c) = fade.clip {
                scratch.sample(scene, c, fade.time);
                if let Some(hold) = &fade.hold {
                    hold.apply(scene, scratch);
                }
            }
            let t = (fade.elapsed / CROSSFADE).clamp(0.0, 1.0);
            let eased = t * t * (3.0 - 2.0 * t);
            pose.blend(scratch, 1.0 - eased);
        }
    }
}

/// The clip a character starts with: an idle one if the file has it, so
/// the first thing seen is the character standing naturally.
fn default_clip(scene: &Scene) -> Option<usize> {
    pick_clip(scene, 0)
}

/// The most natural clip from `first` on: an idle loop, else anything but
/// a reference pose (libraries often start with a T-pose clip), else the
/// first.
pub fn pick_clip(scene: &Scene, first: usize) -> Option<usize> {
    let names: Vec<(usize, String)> =
        scene.animations.iter().enumerate().skip(first).map(|(i, a)| (i, a.name.to_ascii_lowercase())).collect();
    let find = |f: &dyn Fn(&str) -> bool| names.iter().find(|(_, n)| f(n)).map(|(i, _)| *i);
    find(&|n| n == "idle" || n == "idle_loop")
        .or_else(|| find(&|n| n.contains("idle")))
        .or_else(|| find(&|n| !n.contains("pose")))
        .or_else(|| names.first().map(|(i, _)| *i))
}

/// Foot IK for the clips retargeted onto a scene from other characters
/// (see `slerp_retarget::add_animations_with`): whether it is on, and each
/// such clip's other version, so switching swaps clips instead of
/// retargeting again.
#[derive(Clone)]
pub struct FootIk {
    pub on: bool,
    others: Vec<Animation>,
}

impl Default for FootIk {
    fn default() -> Self {
        FootIk { on: true, others: Vec::new() }
    }
}

impl FootIk {
    /// Keeps the other versions of clips just added.
    pub fn add(&mut self, others: Vec<Animation>) {
        self.others.extend(others);
    }

    /// Switches foot IK for the retargeted clips among `scene`'s clips from
    /// `first` on (the ones added from libraries).
    pub fn set(&mut self, on: bool, scene: &mut Scene, first: usize) {
        if on == self.on {
            return;
        }
        self.on = on;
        let Some(added) = scene.animations.get_mut(first..) else { return };
        for other in &mut self.others {
            if let Some(clip) = added.iter_mut().find(|a| a.name == other.name) {
                std::mem::swap(clip, other);
            }
        }
    }
}

//endregion

/*┌─────────────────╮
| Morph targets   |
└─────────────────╯*/
//region Morph targets

/// Weights the user set, laid over whatever the clip says.  Targets with
/// names are set by name on every mesh that has them (a MetaHuman's head,
/// teeth, eyes and brows all carry the same 52 face shapes); unnamed ones
/// by node and index.
#[derive(Clone, Debug, Default)]
pub struct Morphs {
    pub named: BTreeMap<String, f32>,
    pub indexed: HashMap<(usize, usize), f32>,
    /// VRM expression weights, by index into `Scene::expressions`.
    pub expressions: Vec<f32>,
}

impl Morphs {
    pub fn new(scene: &Scene) -> Morphs {
        Morphs { expressions: vec![0.0; scene.expressions.len()], ..Default::default() }
    }

    /// Applies expressions (added to the clip's weights) and then the
    /// overrides (which replace them).
    pub fn apply(&self, scene: &Scene, pose: &mut Pose) {
        for (expr, &w) in scene.expressions.iter().zip(&self.expressions) {
            if w == 0.0 {
                continue;
            }
            for b in &expr.binds {
                if let Some(slot) = pose.weights.get_mut(b.node).and_then(|ws| ws.get_mut(b.target)) {
                    *slot = (*slot + w * b.weight).clamp(0.0, 1.0);
                }
            }
        }
        if !self.named.is_empty() {
            for (ni, node) in scene.nodes.iter().enumerate() {
                let Some(mesh) = node.mesh.and_then(|m| scene.meshes.get(m)) else { continue };
                for (ti, name) in mesh.target_names.iter().enumerate() {
                    if let (Some(&w), Some(slot)) =
                        (self.named.get(name), pose.weights.get_mut(ni).and_then(|ws| ws.get_mut(ti)))
                    {
                        *slot = w;
                    }
                }
            }
        }
        for (&(node, target), &w) in &self.indexed {
            if let Some(slot) = pose.weights.get_mut(node).and_then(|ws| ws.get_mut(target)) {
                *slot = w;
            }
        }
    }
}

/// Every morph target in a scene, for the sliders: names shared across
/// meshes once, and unnamed targets per node.
pub struct MorphList {
    /// Unique target names, in the order first met.
    pub names: Vec<String>,
    /// For each name, a node and index to read its current weight from.
    pub sources: Vec<(usize, usize)>,
    /// Nodes with unnamed targets: node, its name and the target count.
    pub unnamed: Vec<(usize, String, usize)>,
}

impl MorphList {
    pub fn new(scene: &Scene) -> MorphList {
        let mut list = MorphList { names: Vec::new(), sources: Vec::new(), unnamed: Vec::new() };
        let mut seen = std::collections::HashSet::new();
        for (ni, node) in scene.nodes.iter().enumerate() {
            let Some(mesh) = node.mesh.and_then(|m| scene.meshes.get(m)) else { continue };
            let count = mesh.primitives.iter().map(|p| p.targets.len()).max().unwrap_or(0);
            if count == 0 {
                continue;
            }
            if mesh.target_names.len() >= count {
                for (ti, name) in mesh.target_names.iter().take(count).enumerate() {
                    if seen.insert(name.clone()) {
                        list.names.push(name.clone());
                        list.sources.push((ni, ti));
                    }
                }
            } else {
                list.unnamed.push((ni, node.name.clone(), count));
            }
        }
        list
    }
}

//endregion

/*┌─────────────────╮
| ARKit face      |
└─────────────────╯*/
//region ARKit face

/// Apple's 52 ARKit face shapes by area of the face, the set chalk's
/// MetaHuman heads carry as morph targets.
pub const ARKIT_GROUPS: &[(&str, &[&str])] = &[
    ("Brows", &["browDownLeft", "browDownRight", "browInnerUp", "browOuterUpLeft", "browOuterUpRight"]),
    (
        "Eyes",
        &[
            "eyeBlinkLeft",
            "eyeBlinkRight",
            "eyeLookDownLeft",
            "eyeLookDownRight",
            "eyeLookInLeft",
            "eyeLookInRight",
            "eyeLookOutLeft",
            "eyeLookOutRight",
            "eyeLookUpLeft",
            "eyeLookUpRight",
            "eyeSquintLeft",
            "eyeSquintRight",
            "eyeWideLeft",
            "eyeWideRight",
        ],
    ),
    ("Cheeks", &["cheekPuff", "cheekSquintLeft", "cheekSquintRight"]),
    ("Nose", &["noseSneerLeft", "noseSneerRight"]),
    ("Jaw", &["jawForward", "jawLeft", "jawRight", "jawOpen"]),
    (
        "Mouth",
        &[
            "mouthClose",
            "mouthFunnel",
            "mouthPucker",
            "mouthLeft",
            "mouthRight",
            "mouthSmileLeft",
            "mouthSmileRight",
            "mouthFrownLeft",
            "mouthFrownRight",
            "mouthDimpleLeft",
            "mouthDimpleRight",
            "mouthStretchLeft",
            "mouthStretchRight",
            "mouthRollLower",
            "mouthRollUpper",
            "mouthShrugLower",
            "mouthShrugUpper",
            "mouthPressLeft",
            "mouthPressRight",
            "mouthLowerDownLeft",
            "mouthLowerDownRight",
            "mouthUpperUpLeft",
            "mouthUpperUpRight",
        ],
    ),
    ("Tongue", &["tongueOut"]),
];

/// Expressions made of ARKit shapes, for one-click faces.
pub const ARKIT_PRESETS: &[(&str, &[(&str, f32)])] = &[
    ("Neutral", &[]),
    (
        "Smile",
        &[
            ("mouthSmileLeft", 0.8),
            ("mouthSmileRight", 0.8),
            ("cheekSquintLeft", 0.4),
            ("cheekSquintRight", 0.4),
            ("eyeSquintLeft", 0.25),
            ("eyeSquintRight", 0.25),
        ],
    ),
    (
        "Laugh",
        &[
            ("mouthSmileLeft", 1.0),
            ("mouthSmileRight", 1.0),
            ("jawOpen", 0.45),
            ("cheekSquintLeft", 0.7),
            ("cheekSquintRight", 0.7),
            ("eyeSquintLeft", 0.6),
            ("eyeSquintRight", 0.6),
            ("browInnerUp", 0.2),
        ],
    ),
    (
        "Sad",
        &[
            ("mouthFrownLeft", 0.7),
            ("mouthFrownRight", 0.7),
            ("browInnerUp", 0.8),
            ("mouthShrugLower", 0.3),
            ("eyeLookDownLeft", 0.2),
            ("eyeLookDownRight", 0.2),
        ],
    ),
    (
        "Angry",
        &[
            ("browDownLeft", 0.9),
            ("browDownRight", 0.9),
            ("noseSneerLeft", 0.5),
            ("noseSneerRight", 0.5),
            ("mouthPressLeft", 0.5),
            ("mouthPressRight", 0.5),
            ("eyeSquintLeft", 0.4),
            ("eyeSquintRight", 0.4),
        ],
    ),
    (
        "Surprised",
        &[
            ("browInnerUp", 0.9),
            ("browOuterUpLeft", 0.8),
            ("browOuterUpRight", 0.8),
            ("eyeWideLeft", 0.7),
            ("eyeWideRight", 0.7),
            ("jawOpen", 0.35),
            ("mouthFunnel", 0.3),
        ],
    ),
    (
        "Disgust",
        &[
            ("noseSneerLeft", 0.8),
            ("noseSneerRight", 0.6),
            ("mouthUpperUpLeft", 0.6),
            ("mouthUpperUpRight", 0.4),
            ("browDownLeft", 0.5),
            ("eyeSquintLeft", 0.5),
        ],
    ),
    ("Blink", &[("eyeBlinkLeft", 1.0), ("eyeBlinkRight", 1.0)]),
    ("Wink", &[("eyeBlinkLeft", 1.0), ("cheekSquintLeft", 0.4), ("mouthSmileLeft", 0.4)]),
    ("Aah", &[("jawOpen", 0.7), ("mouthLowerDownLeft", 0.3), ("mouthLowerDownRight", 0.3)]),
    ("Ooh", &[("mouthFunnel", 0.7), ("mouthPucker", 0.5), ("jawOpen", 0.2)]),
    ("Kiss", &[("mouthPucker", 1.0), ("mouthFunnel", 0.2)]),
    ("Puff", &[("cheekPuff", 1.0), ("mouthClose", 0.2)]),
    ("Tongue", &[("jawOpen", 0.4), ("tongueOut", 1.0)]),
    ("Sceptical", &[("browOuterUpLeft", 0.9), ("browDownRight", 0.5), ("mouthLeft", 0.3), ("mouthPressRight", 0.3)]),
];

/// Whether the scene's targets are the ARKit set: most of the 52 names
/// appear.  Matching ignores case, since exporters differ.
pub fn has_arkit(list: &MorphList) -> bool {
    let names: std::collections::HashSet<String> = list.names.iter().map(|n| n.to_ascii_lowercase()).collect();
    let found =
        ARKIT_GROUPS.iter().flat_map(|(_, g)| g.iter()).filter(|n| names.contains(&n.to_ascii_lowercase())).count();
    found >= 40
}

/// The file's own spelling of an ARKit name, which may differ in case.
pub fn arkit_name<'a>(list: &'a MorphList, name: &str) -> Option<&'a str> {
    list.names.iter().find(|n| n.eq_ignore_ascii_case(name)).map(String::as_str)
}

pub fn is_arkit_name(name: &str) -> bool {
    ARKIT_GROUPS.iter().flat_map(|(_, g)| g.iter()).any(|n| n.eq_ignore_ascii_case(name))
}

//endregion

/*┌─────────────────╮
| VRM expressions |
└─────────────────╯*/
//region VRM expressions

/// VRM's preset expressions by group, in VRM 1.0 and 0.x spellings.
pub const VRM_GROUPS: &[(&str, &[&str])] = &[
    ("Emotions", &["neutral", "happy", "joy", "fun", "relaxed", "angry", "sad", "sorrow", "surprised"]),
    ("Mouth", &["aa", "a", "ih", "i", "ou", "u", "ee", "e", "oh", "o"]),
    ("Blink", &["blink", "blinkLeft", "blink_l", "blinkRight", "blink_r"]),
    ("Gaze", &["lookUp", "lookup", "lookDown", "lookdown", "lookLeft", "lookleft", "lookRight", "lookright"]),
];

/// Which group an expression belongs in: its preset name's, else "Custom".
pub fn vrm_group(preset: Option<&str>, name: &str) -> &'static str {
    let key = preset.unwrap_or(name);
    VRM_GROUPS.iter().find(|(_, names)| names.iter().any(|n| n.eq_ignore_ascii_case(key))).map_or("Custom", |(g, _)| g)
}

//endregion

#[cfg(test)]
mod tests {
    use super::*;
    use slerp_scene::{Animation, Channel, Interpolation, Mesh, MorphTarget, Node, Primitive, Property};

    fn scene_with_clips() -> Scene {
        let mut scene = Scene::default();
        scene.nodes.push(Node::new("a"));
        scene.roots.push(0);
        for (name, x) in [("Walk", 2.0), ("Idle_Loop", 0.0)] {
            scene.animations.push(Animation {
                name: name.into(),
                duration: 1.0,
                channels: vec![Channel {
                    node: 0,
                    property: Property::Translation,
                    interpolation: Interpolation::Linear,
                    times: vec![0.0, 1.0],
                    values: vec![0.0, 0.0, 0.0, x, 0.0, 0.0],
                }],
            });
        }
        scene
    }

    #[test]
    fn starts_on_idle_and_loops() {
        let scene = scene_with_clips();
        let mut p = Player::new(&scene);
        assert_eq!(p.clip, Some(1));
        assert!(p.playing);
        p.advance(&scene, 1.25);
        assert!((p.time - 0.25).abs() < 1e-5);
        p.looping = false;
        p.advance(&scene, 2.0);
        assert_eq!(p.time, 1.0);
        assert!(!p.playing);
    }

    #[test]
    fn crossfade_blends_then_settles() {
        let scene = scene_with_clips();
        let mut p = Player::new(&scene);
        p.playing = false;
        p.select(&scene, Some(0));
        p.playing = false;
        p.time = 0.5;
        let (mut pose, mut scratch) = (Pose::rest(&scene), Pose::rest(&scene));
        p.sample(&scene, &mut pose, &mut scratch);
        // At the start of the fade the old clip (at x = 0) still shows.
        assert!(pose.local[0].translation.x.abs() < 1e-5);
        p.advance(&scene, 1.0);
        assert!(!p.is_animating());
        p.sample(&scene, &mut pose, &mut scratch);
        assert!((pose.local[0].translation.x - 1.0).abs() < 1e-5);
    }

    #[test]
    fn named_overrides_reach_every_mesh() {
        let mut scene = Scene::default();
        for name in ["head", "teeth"] {
            let prim = Primitive {
                positions: vec![glam::Vec3::ZERO],
                indices: vec![0],
                targets: vec![MorphTarget::default(), MorphTarget::default()],
                ..Default::default()
            };
            let mesh =
                Mesh { name: name.into(), primitives: vec![prim], target_names: vec!["jawOpen".into(), "x".into()] };
            scene.add_mesh_node(mesh);
        }
        let list = MorphList::new(&scene);
        assert_eq!(list.names, vec!["jawOpen".to_string(), "x".to_string()]);
        let mut morphs = Morphs::new(&scene);
        morphs.named.insert("jawOpen".into(), 0.5);
        let mut pose = Pose::rest(&scene);
        morphs.apply(&scene, &mut pose);
        assert_eq!(pose.weights[0], vec![0.5, 0.0]);
        assert_eq!(pose.weights[1], vec![0.5, 0.0]);
    }

    #[test]
    fn foot_ik_swaps_retargeted_clips() {
        let mut scene = scene_with_clips();
        // "Idle_Loop" came from a library; its version with foot IK moves.
        let mut other = scene.animations[1].clone();
        other.channels[0].values[3] = 0.5;
        let mut feet = FootIk { on: false, others: Vec::new() };
        feet.add(vec![other]);
        let x = |scene: &Scene, clip: usize| scene.animations[clip].channels[0].values[3];
        feet.set(true, &mut scene, 1);
        assert_eq!((x(&scene, 0), x(&scene, 1)), (2.0, 0.5), "the file's own clip stays");
        feet.set(true, &mut scene, 1);
        assert_eq!(x(&scene, 1), 0.5, "switching on twice swaps once");
        feet.set(false, &mut scene, 1);
        assert_eq!(x(&scene, 1), 0.0);
    }

    #[test]
    fn arkit_tables_hold_the_52_shapes() {
        let all: Vec<&str> = ARKIT_GROUPS.iter().flat_map(|(_, g)| g.iter().copied()).collect();
        assert_eq!(all.len(), 52);
        for (_, shapes) in ARKIT_PRESETS {
            for (name, _) in *shapes {
                assert!(is_arkit_name(name), "{name}");
            }
        }
    }
}
