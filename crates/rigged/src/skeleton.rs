//! Skeletons, skins and clips as every format here describes them: a list
//! of bones with names, parents and rest transforms, vertices weighted to
//! several bones, and poses sampled frame by frame.  This module turns them
//! into the scene model's nodes, `Skin` and `Animation` clips, so each
//! format only reads its file.
//!
//! Files list bones in any order (a PMX bone may name a parent after it),
//! may name parents that don't exist, and damaged ones make loops; `Rig`
//! sorts that out once, so nothing downstream walks a broken tree.

use crate::read::Result;
use slerp_scene::glam::{Mat4, Quat, Vec3, Vec4};
use slerp_scene::{Animation, Channel, Interpolation, LoadError, Node, Property, Scene, Skin, Transform};

/// More bones than this can't be addressed by a vertex's 16-bit joint
/// indices, and no real skeleton comes near it.
pub const MAX_BONES: usize = 32_768;

/*┌─────────────────╮
| Budget          |
└─────────────────╯*/
//region Budget

/// How much memory a load may decode, as a multiple of the bytes it read,
/// with a floor for small files and a ceiling for all.  Text and packed
/// binary formats expand a few times into floats; a file claiming more
/// than this is damaged or hostile.
pub struct Budget {
    left: u64,
}

impl Budget {
    const RATIO: u64 = 64;
    const MIN: u64 = 256 << 20;
    const MAX: u64 = 8 << 30;

    pub fn new(input_bytes: u64) -> Budget {
        Budget { left: input_bytes.saturating_mul(Self::RATIO).clamp(Self::MIN, Self::MAX) }
    }

    /// Claims `count` items of `size` bytes before allocating them.
    pub fn take(&mut self, count: usize, size: usize, what: &str) -> Result<()> {
        let bytes = (count as u64).saturating_mul(size as u64);
        if bytes > self.left {
            return Err(LoadError::TooLarge(format!("{what} would need {} MB", bytes >> 20)));
        }
        self.left -= bytes;
        Ok(())
    }

    /// Whether `count` items of `size` bytes would still fit, without
    /// claiming them.
    pub fn fits(&self, count: usize, size: usize) -> bool {
        (count as u64).saturating_mul(size as u64) <= self.left
    }
}

//endregion

/*┌─────────────────╮
| Rig             |
└─────────────────╯*/
//region Rig

/// One bone as a file describes it, already in Slerp's coordinates.
#[derive(Clone, Debug)]
pub struct Bone {
    pub name: String,
    /// The parent bone's index; `None` for a root.
    pub parent: Option<usize>,
    /// The rest (bind) transform relative to the parent.
    pub local: Transform,
}

/// A skeleton whose parents are known to form a forest.
#[derive(Clone, Debug, Default)]
pub struct Rig {
    pub bones: Vec<Bone>,
    /// Each bone's rest transform in model space.
    pub world: Vec<Mat4>,
    /// Bone indices with every parent before its children.
    pub order: Vec<usize>,
    /// How many parents were dropped because they didn't exist or closed a
    /// loop.
    pub repaired: usize,
}

impl Rig {
    /// A rig from bones with transforms relative to their parents.
    pub fn from_local(mut bones: Vec<Bone>) -> Result<Rig> {
        check_count(bones.len())?;
        let repaired = repair_parents(&mut bones);
        let order = parents_first(&bones);
        let mut world = vec![Mat4::IDENTITY; bones.len()];
        for &b in &order {
            let local = bones[b].local.matrix();
            world[b] = match bones[b].parent {
                Some(p) => world[p] * local,
                None => local,
            };
        }
        Ok(Rig { bones, world, order, repaired })
    }

    /// A rig from bones whose `local` field holds their model-space rest
    /// transform, as MD5 and PMX store them; each becomes relative to its
    /// parent here.
    pub fn from_world(mut bones: Vec<Bone>) -> Result<Rig> {
        check_count(bones.len())?;
        let repaired = repair_parents(&mut bones);
        let order = parents_first(&bones);
        let world: Vec<Mat4> = bones.iter().map(|b| b.local.matrix()).collect();
        for b in &mut bones {
            if let Some(p) = b.parent {
                b.local = Transform::from_matrix(inverse(world[p]) * b.local.matrix());
            }
        }
        Ok(Rig { bones, world, order, repaired })
    }

    pub fn len(&self) -> usize {
        self.bones.len()
    }

    pub fn is_empty(&self) -> bool {
        self.bones.is_empty()
    }

    /// The index of the first bone called `name`.
    pub fn find(&self, name: &str) -> Option<usize> {
        self.bones.iter().position(|b| b.name == name)
    }

    /// Adds one node per bone, in bone order, with the root bones among
    /// the scene's roots.  Returns the node index of bone 0; bone `i` is
    /// node `base + i`.
    pub fn add_nodes(&self, scene: &mut Scene) -> usize {
        let base = scene.nodes.len();
        for b in &self.bones {
            let mut node = Node::new(b.name.clone());
            node.transform = b.local;
            scene.nodes.push(node);
        }
        for (i, b) in self.bones.iter().enumerate() {
            match b.parent {
                Some(p) => scene.nodes[base + p].children.push(base + i),
                None => scene.roots.push(base + i),
            }
        }
        base
    }

    /// A skin whose joint `i` is bone `i`, bound in the rest pose.
    pub fn skin(&self, base: usize, name: &str) -> Skin {
        Skin {
            name: name.to_string(),
            joints: (0..self.bones.len()).map(|i| base + i).collect(),
            inverse_bind: self.world.iter().map(|&w| inverse(w)).collect(),
            skeleton: None,
        }
    }

    /// Each bone's rest transform, for comparing poses against.
    pub fn rest(&self) -> Vec<Transform> {
        self.bones.iter().map(|b| b.local).collect()
    }
}

fn check_count(n: usize) -> Result<()> {
    if n > MAX_BONES {
        return Err(LoadError::TooLarge(format!("{n} bones")));
    }
    Ok(())
}

/// Drops parents that don't exist, point at the bone itself, or close a
/// loop, so that the bones form a forest.  Returns how many were dropped.
fn repair_parents(bones: &mut [Bone]) -> usize {
    let n = bones.len();
    let mut repaired = 0;
    for (i, bone) in bones.iter_mut().enumerate() {
        if bone.parent.is_some_and(|p| p >= n || p == i) {
            bone.parent = None;
            repaired += 1;
        }
    }
    // Walk up from each bone not yet seen, marking the path; reaching a
    // bone on the current path means a loop, which is cut where the walk
    // re-entered it.  Each bone is walked once, so long chains stay cheap.
    const NEW: u8 = 0;
    const ON_PATH: u8 = 1;
    const DONE: u8 = 2;
    let mut state = vec![NEW; n];
    let mut path = Vec::new();
    for start in 0..n {
        if state[start] != NEW {
            continue;
        }
        let mut at = start;
        loop {
            state[at] = ON_PATH;
            path.push(at);
            match bones[at].parent {
                Some(p) if state[p] == NEW => at = p,
                Some(p) if state[p] == ON_PATH => {
                    bones[at].parent = None;
                    repaired += 1;
                    break;
                }
                _ => break,
            }
        }
        for b in path.drain(..) {
            state[b] = DONE;
        }
    }
    repaired
}

/// Bone indices ordered so that parents come before their children, file
/// order otherwise.
fn parents_first(bones: &[Bone]) -> Vec<usize> {
    let n = bones.len();
    let mut children: Vec<Vec<usize>> = vec![Vec::new(); n];
    let mut roots = Vec::new();
    for (i, b) in bones.iter().enumerate() {
        match b.parent {
            Some(p) => children[p].push(i),
            None => roots.push(i),
        }
    }
    let mut order = Vec::with_capacity(n);
    let mut stack: Vec<usize> = roots.into_iter().rev().collect();
    while let Some(b) = stack.pop() {
        order.push(b);
        stack.extend(children[b].iter().rev());
    }
    order
}

/// The inverse of a rest matrix, or identity for a singular one (a bone
/// scaled to nothing), which would otherwise fill the skin with NaN.
pub fn inverse(m: Mat4) -> Mat4 {
    let det = m.determinant();
    if det.is_finite() && det.abs() > 1e-12 { m.inverse() } else { Mat4::IDENTITY }
}

//endregion

/*┌─────────────────╮
| Weights         |
└─────────────────╯*/
//region Weights

/// The four strongest of a vertex's bone influences, renormalised, as the
/// renderer takes them.  Repeated bones are merged, and influences that are
/// zero, negative or not numbers dropped.  A vertex left with none follows
/// `fallback` fully.
pub fn strongest_four(influences: &mut Vec<(u32, f32)>, fallback: u32) -> ([u16; 4], Vec4) {
    influences.retain(|&(_, w)| w.is_finite() && w > 0.0);
    influences.sort_by_key(|&(j, _)| j);
    influences.dedup_by(|b, a| {
        if a.0 == b.0 {
            a.1 += b.1;
            true
        } else {
            false
        }
    });
    influences.sort_by(|a, b| b.1.total_cmp(&a.1));
    let kept = &influences[..influences.len().min(4)];
    let sum: f32 = kept.iter().map(|k| k.1).sum();
    if kept.is_empty() || sum <= 0.0 {
        return ([fallback.min(u16::MAX as u32) as u16, 0, 0, 0], Vec4::X);
    }
    let mut joints = [0u16; 4];
    let mut weights = [0f32; 4];
    for (i, &(j, w)) in kept.iter().enumerate() {
        joints[i] = j.min(u16::MAX as u32) as u16;
        weights[i] = w / sum;
    }
    (joints, Vec4::from_array(weights))
}

//endregion

/*┌─────────────────╮
| Clips           |
└─────────────────╯*/
//region Clips

/// A clip from poses sampled at a fixed rate: `poses[frame * bones + bone]`
/// is bone `bone`'s transform relative to its parent at `frame`.  `node`
/// gives the scene node of each bone (`None` for bones the scene lacks).
/// Tracks that never move are dropped when they sit at the rest pose, and
/// kept as one key when they hold another pose throughout.
pub fn sampled_clip(
    name: &str,
    fps: f32,
    bones: usize,
    poses: &[Transform],
    rest: &[Transform],
    node: impl Fn(usize) -> Option<usize>,
) -> Animation {
    let fps = if fps.is_finite() && fps > 0.0 { fps } else { 30.0 };
    let frames = if bones == 0 { 0 } else { poses.len() / bones };
    if frames == 0 {
        return Animation { name: name.to_string(), duration: 0.0, channels: Vec::new() };
    }
    let times: Vec<f32> = (0..frames).map(|f| f as f32 / fps).collect();
    let mut channels = Vec::new();
    for b in 0..bones {
        let Some(node) = node(b) else { continue };
        let track = |f: usize| poses[f * bones + b];
        let rest = rest.get(b).copied().unwrap_or(Transform::IDENTITY);
        // 1. Translation.
        let first = track(0).translation;
        if let Some(keys) = track_keys(frames, |f| track(f).translation, first, rest.translation, close3) {
            let values = keys.iter().flat_map(|&f| track(f).translation.to_array()).collect();
            channels.push(channel(node, Property::Translation, &times, &keys, values));
        }
        // 2. Rotation, each key in the hemisphere of the one before so
        //    that interpolation takes the short way.
        let mut rotations: Vec<Quat> = (0..frames).map(|f| track(f).rotation.normalize()).collect();
        for f in 1..rotations.len() {
            if rotations[f].dot(rotations[f - 1]) < 0.0 {
                rotations[f] = -rotations[f];
            }
        }
        if let Some(keys) = track_keys(frames, |f| rotations[f], rotations[0], rest.rotation, close_quat) {
            let values = keys.iter().flat_map(|&f| rotations[f].to_array()).collect();
            channels.push(channel(node, Property::Rotation, &times, &keys, values));
        }
        // 3. Scale.
        let first = track(0).scale;
        if let Some(keys) = track_keys(frames, |f| track(f).scale, first, rest.scale, close3) {
            let values = keys.iter().flat_map(|&f| track(f).scale.to_array()).collect();
            channels.push(channel(node, Property::Scale, &times, &keys, values));
        }
    }
    Animation { name: name.to_string(), duration: times.last().copied().unwrap_or(0.0), channels }
}

/// The frames a track needs: all of them when it moves, the first alone
/// when it holds one value other than the rest pose, none when it stays at
/// rest.
fn track_keys<T: Copy>(
    frames: usize,
    value: impl Fn(usize) -> T,
    first: T,
    rest: T,
    close: fn(T, T) -> bool,
) -> Option<Vec<usize>> {
    if frames == 0 {
        return None;
    }
    if (1..frames).any(|f| !close(value(f), first)) {
        return Some((0..frames).collect());
    }
    (!close(first, rest)).then(|| vec![0])
}

fn channel(node: usize, property: Property, times: &[f32], keys: &[usize], values: Vec<f32>) -> Channel {
    Channel {
        node,
        property,
        interpolation: Interpolation::Linear,
        times: keys.iter().map(|&k| times[k]).collect(),
        values,
    }
}

fn close3(a: Vec3, b: Vec3) -> bool {
    (a - b).abs().max_element() <= 1e-6 * (1.0 + a.abs().max_element().max(b.abs().max_element()))
}

fn close_quat(a: Quat, b: Quat) -> bool {
    a.dot(b).abs() >= 1.0 - 1e-7
}

/// Gives clips unique names, numbering repeats ("walk", "walk 2").
pub fn unique_names(clips: &mut [Animation]) {
    let mut seen: Vec<String> = Vec::with_capacity(clips.len());
    for clip in clips.iter_mut() {
        if clip.name.trim().is_empty() {
            clip.name = "Animation".into();
        }
        let base = clip.name.clone();
        let mut n = 2;
        while seen.contains(&clip.name) {
            clip.name = format!("{base} {n}");
            n += 1;
        }
        seen.push(clip.name.clone());
    }
}

//endregion

#[cfg(test)]
mod tests {
    use super::*;

    fn bone(name: &str, parent: Option<usize>, x: f32) -> Bone {
        Bone {
            name: name.into(),
            parent,
            local: Transform { translation: Vec3::new(x, 0.0, 0.0), ..Transform::IDENTITY },
        }
    }

    #[test]
    fn parents_after_children_and_loops() {
        // Bone 0's parent is bone 2, listed later; 3 and 4 form a loop; 5's
        // parent doesn't exist.
        let bones = vec![
            bone("a", Some(2), 1.0),
            bone("b", Some(0), 1.0),
            bone("root", None, 1.0),
            bone("x", Some(4), 1.0),
            bone("y", Some(3), 1.0),
            bone("z", Some(99), 1.0),
        ];
        let rig = Rig::from_local(bones).unwrap();
        assert_eq!(rig.bones[0].parent, Some(2));
        assert_eq!(rig.bones[5].parent, None);
        // One of the loop's links is cut, the other kept.
        assert!(rig.bones[3].parent.is_none() != rig.bones[4].parent.is_none());
        assert_eq!(rig.repaired, 2);
        // A long chain and a long loop are cheap.
        let mut chain: Vec<Bone> = (0..MAX_BONES).map(|i| bone("c", i.checked_sub(1), 1.0)).collect();
        chain[0].parent = Some(MAX_BONES - 1);
        let long = Rig::from_local(chain).unwrap();
        assert_eq!((long.repaired, long.order.len()), (1, MAX_BONES));
        // Parents come first, and world positions add up along the chain.
        let pos = |i: usize| rig.order.iter().position(|&b| b == i).unwrap();
        assert!(pos(2) < pos(0) && pos(0) < pos(1));
        assert_eq!(rig.world[1].w_axis.truncate(), Vec3::new(3.0, 0.0, 0.0));
        // Every bone is a node, and the roots are the scene's.
        let mut scene = Scene::default();
        let base = rig.add_nodes(&mut scene);
        assert_eq!(scene.nodes.len(), 6);
        assert_eq!(scene.nodes[base + 2].children, vec![base]);
        let skin = rig.skin(base, "s");
        let pose = slerp_scene::Pose::rest(&scene);
        // At rest every joint matrix is the identity.
        for m in pose.joint_matrices(
            &{
                let mut s = scene;
                s.skins.push(skin);
                s
            },
            0,
        ) {
            assert!(m.abs_diff_eq(Mat4::IDENTITY, 1e-5));
        }
    }

    #[test]
    fn world_transforms_become_local() {
        let mut child = bone("c", Some(0), 0.0);
        child.local = Transform {
            translation: Vec3::new(1.0, 2.0, 0.0),
            rotation: Quat::from_rotation_z(1.0),
            ..Transform::IDENTITY
        };
        let parent = Bone {
            name: "p".into(),
            parent: None,
            local: Transform { translation: Vec3::new(1.0, 0.0, 0.0), ..Transform::IDENTITY },
        };
        let rig = Rig::from_world(vec![parent, child]).unwrap();
        assert!((rig.bones[1].local.translation - Vec3::new(0.0, 2.0, 0.0)).length() < 1e-6);
        assert!(rig.bones[1].local.rotation.angle_between(Quat::from_rotation_z(1.0)) < 1e-5);
    }

    #[test]
    fn four_strongest_influences() {
        let mut list = vec![(3, 0.1), (1, 0.4), (3, 0.2), (2, 0.05), (5, 0.1), (6, f32::NAN), (7, -1.0), (8, 0.15)];
        let (j, w) = strongest_four(&mut list, 0);
        assert_eq!(j, [1, 3, 8, 5]);
        assert!((w.element_sum() - 1.0).abs() < 1e-6);
        assert!((w.x - 0.4 / 0.95).abs() < 1e-6);
        let (j, w) = strongest_four(&mut Vec::new(), 7);
        assert_eq!((j, w), ([7, 0, 0, 0], Vec4::X));
    }

    #[test]
    fn clips_keep_only_moving_tracks() {
        let rest = vec![Transform::IDENTITY, Transform::IDENTITY, Transform::IDENTITY];
        let moved = Transform { translation: Vec3::X, ..Transform::IDENTITY };
        let turned = |a: f32| Transform { rotation: Quat::from_rotation_y(a), ..Transform::IDENTITY };
        // Bone 0 stays at rest, bone 1 holds a translation, bone 2 turns.
        let poses = vec![Transform::IDENTITY, moved, turned(0.0), Transform::IDENTITY, moved, turned(1.0)];
        let clip = sampled_clip("c", 10.0, 3, &poses, &rest, |b| Some(b + 10));
        assert_eq!(clip.duration, 0.1);
        assert_eq!(clip.channels.len(), 2);
        assert_eq!((clip.channels[0].node, clip.channels[0].property), (11, Property::Translation));
        assert_eq!(clip.channels[0].times, vec![0.0]);
        assert_eq!((clip.channels[1].node, clip.channels[1].times.len()), (12, 2));
    }

    #[test]
    fn rotations_take_the_short_way() {
        // The second key is stored in the other hemisphere.
        let (a, b) = (Quat::from_rotation_y(0.1), -Quat::from_rotation_y(0.2));
        let poses =
            vec![Transform { rotation: a, ..Transform::IDENTITY }, Transform { rotation: b, ..Transform::IDENTITY }];
        let clip = sampled_clip("c", 30.0, 1, &poses, &[Transform::IDENTITY], Some);
        let v = &clip.channels[0].values;
        assert!(v[3] > 0.0 && v[7] > 0.0, "{v:?}");
    }

    #[test]
    fn repeated_clip_names_are_numbered() {
        let clip = |n: &str| Animation { name: n.into(), ..Default::default() };
        let mut clips = vec![clip("walk"), clip("walk"), clip(""), clip("walk")];
        unique_names(&mut clips);
        let names: Vec<_> = clips.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["walk", "walk 2", "Animation", "walk 3"]);
    }

    #[test]
    fn budget() {
        let mut b = Budget::new(1);
        assert!(b.take(1, Budget::MIN as usize, "x").is_ok());
        assert!(b.take(1, 1, "x").is_err());
        assert!(Budget::new(1).take(usize::MAX, usize::MAX, "x").is_err());
    }
}
