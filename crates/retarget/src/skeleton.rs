//! What both retargeting strategies need to know about a scene's node
//! tree: parents, an order that visits parents first, the rest pose, and
//! which key times to sample a clip at.

use slerp_scene::glam::{Mat4, Quat, Vec3};
use slerp_scene::{Animation, Interpolation, Pose, Scene, Transform};

/// A scene's node tree and rest pose.
pub struct Skeleton<'a> {
    pub scene: &'a Scene,
    pub parents: Vec<Option<usize>>,
    /// Each node's place in a walk from the roots that visits parents
    /// first, or `usize::MAX` for nodes the walk doesn't reach.
    pub rank: Vec<usize>,
    pub rest: Pose,
    /// True for nodes that are joints of a skin.
    pub joint: Vec<bool>,
}

impl<'a> Skeleton<'a> {
    pub fn new(scene: &'a Scene) -> Self {
        let count = scene.nodes.len();
        // 1. Parents, ignoring children that don't exist.
        let mut parents = vec![None; count];
        for (i, node) in scene.nodes.iter().enumerate() {
            for &c in &node.children {
                if c < count && c != i {
                    parents[c] = Some(i);
                }
            }
        }
        // 2. A depth-first walk from the roots; a malformed file can make a
        //    node its own ancestor, so each node is visited once.
        let mut rank = vec![usize::MAX; count];
        let mut stack: Vec<usize> = scene.roots.iter().rev().copied().filter(|&r| r < count).collect();
        let mut next = 0;
        while let Some(n) = stack.pop() {
            if rank[n] != usize::MAX {
                continue;
            }
            rank[n] = next;
            next += 1;
            stack.extend(scene.nodes[n].children.iter().rev().copied().filter(|&c| c < count && rank[c] == usize::MAX));
        }
        // 3. Skin joints.
        let mut joint = vec![false; count];
        for skin in &scene.skins {
            for &j in &skin.joints {
                if let Some(slot) = joint.get_mut(j) {
                    *slot = true;
                }
            }
        }
        Skeleton { scene, parents, rank, rest: Pose::rest(scene), joint }
    }

    /// The node's world position in the rest pose.
    pub fn rest_position(&self, node: usize) -> Vec3 {
        position(&self.rest.world[node])
    }

    /// The node's world rotation in the rest pose.
    pub fn rest_rotation(&self, node: usize) -> Quat {
        rotation(&self.rest.world[node])
    }

    /// How much the node's parent scales lengths, which turns world
    /// distances into the node's local units.  Assumes uniform scale.
    pub fn parent_scale(&self, node: usize) -> f32 {
        let Some(p) = self.parents[node] else { return 1.0 };
        let s = self.rest.world[p].matrix3_scale();
        if s.is_finite() && s > 1e-12 { s } else { 1.0 }
    }
}

/// The rotation part of a world matrix, without its scale.
pub fn rotation(m: &Mat4) -> Quat {
    let q = Transform::from_matrix(*m).rotation;
    if q.is_finite() && q.length_squared() > 1e-12 { q.normalize() } else { Quat::IDENTITY }
}

pub fn position(m: &Mat4) -> Vec3 {
    m.w_axis.truncate()
}

trait MatrixScale {
    fn matrix3_scale(&self) -> f32;
}

impl MatrixScale for Mat4 {
    /// The average length of the basis vectors.
    fn matrix3_scale(&self) -> f32 {
        (self.x_axis.truncate().length() + self.y_axis.truncate().length() + self.z_axis.truncate().length()) / 3.0
    }
}

/// The most samples a baked clip may have per bone: an hour at 30 frames a
/// second, which keeps a corrupt clip claiming a duration of years from
/// asking for gigabytes.
const MAX_SAMPLES: usize = 30 * 3600;

/// The frame rate cubic-spline clips are sampled at.
pub const FPS: f32 = 30.0;

/// The times to bake a clip at, and how to interpolate between them.
/// Linear and step clips keep their own key times, so nothing is lost;
/// cubic splines curve between keys and are sampled at 30 frames a second.
pub fn sample_times(anim: &Animation) -> (Vec<f32>, Interpolation) {
    let duration = if anim.duration.is_finite() { anim.duration.max(0.0) } else { 0.0 };
    let all_step = !anim.channels.is_empty() && anim.channels.iter().all(|c| c.interpolation == Interpolation::Step);
    let cubic = anim.channels.iter().any(|c| c.interpolation == Interpolation::CubicSpline);
    let frames = ((duration * FPS).ceil() as usize).saturating_add(1);
    if cubic && frames <= MAX_SAMPLES {
        let times = (0..frames).map(|i| (i as f32 / FPS).min(duration)).collect::<Vec<_>>();
        return (dedup(times), Interpolation::Linear);
    }
    let mut times: Vec<f32> =
        anim.channels.iter().flat_map(|c| c.times.iter().copied()).filter(|t| t.is_finite() && *t >= 0.0).collect();
    times.sort_by(f32::total_cmp);
    let mut times = dedup(times);
    if times.len() > MAX_SAMPLES {
        // Thin to the cap, keeping the first and last key.
        let step = times.len() as f64 / MAX_SAMPLES as f64;
        let last = *times.last().unwrap_or(&0.0);
        times = (0..MAX_SAMPLES - 1).map(|i| times[(i as f64 * step) as usize]).chain([last]).collect();
    }
    if times.is_empty() {
        times.push(0.0);
    }
    (times, if all_step { Interpolation::Step } else { Interpolation::Linear })
}

/// `times` with evenly spaced moments added to every gap longer than
/// `step`, so a clip keyed sparsely is still baked often enough for what
/// changes between its keys (feet held in place by IK).  Stays within
/// `MAX_SAMPLES`.
pub fn densify(times: &[f32], step: f32) -> Vec<f32> {
    let (Some(&first), Some(&last)) = (times.first(), times.last()) else { return times.to_vec() };
    let span = last - first;
    let usable = step > 0.0 && span.is_finite() && span / step <= MAX_SAMPLES as f32 - times.len() as f32;
    if !usable {
        return times.to_vec();
    }
    let mut out = Vec::with_capacity(times.len() + (span / step) as usize + 1);
    for pair in times.windows(2) {
        let (a, b) = (pair[0], pair[1]);
        out.push(a);
        let parts = ((b - a) / step).ceil() as usize;
        out.extend((1..parts).map(|i| a + (b - a) * i as f32 / parts as f32));
    }
    out.push(last);
    out
}

/// Drops times within a hundred-thousandth of a second of the one before.
fn dedup(mut times: Vec<f32>) -> Vec<f32> {
    times.dedup_by(|b, a| *b - *a < 1e-5);
    times
}

#[cfg(test)]
mod tests {
    use super::*;
    use slerp_scene::{Channel, Property};

    fn channel(interpolation: Interpolation, times: Vec<f32>) -> Channel {
        let values = vec![0.0; times.len() * 4 * if interpolation == Interpolation::CubicSpline { 3 } else { 1 }];
        Channel { node: 0, property: Property::Rotation, interpolation, times, values }
    }

    #[test]
    fn times_union_of_keys() {
        let anim = Animation {
            name: "a".into(),
            duration: 1.0,
            channels: vec![
                channel(Interpolation::Linear, vec![0.0, 0.5, 1.0]),
                channel(Interpolation::Linear, vec![0.0, 0.25, 0.5000001, f32::NAN]),
            ],
        };
        let (times, interp) = sample_times(&anim);
        assert_eq!(times, vec![0.0, 0.25, 0.5, 1.0]);
        assert_eq!(interp, Interpolation::Linear);
    }

    #[test]
    fn cubic_resampled() {
        let anim = Animation {
            name: "a".into(),
            duration: 1.0,
            channels: vec![channel(Interpolation::CubicSpline, vec![0.0, 1.0])],
        };
        let (times, _) = sample_times(&anim);
        assert_eq!(times.len(), 31);
        assert_eq!(*times.last().unwrap(), 1.0);
        // A corrupt duration falls back to the keys.
        let anim = Animation {
            name: "a".into(),
            duration: 1e12,
            channels: vec![channel(Interpolation::CubicSpline, vec![0.0, 1.0])],
        };
        assert_eq!(sample_times(&anim).0, vec![0.0, 1.0]);
    }

    #[test]
    fn densified() {
        assert_eq!(densify(&[0.0, 0.1], 0.04), vec![0.0, 0.1 / 3.0, 0.2 / 3.0, 0.1]);
        assert_eq!(densify(&[0.0, 0.02, 0.04], 0.04), vec![0.0, 0.02, 0.04]);
        assert_eq!(densify(&[0.5], 0.04), vec![0.5]);
        assert!(densify(&[], 0.04).is_empty());
        // A span that would pass the cap stays as it was.
        assert_eq!(densify(&[0.0, 1e9], 0.04), vec![0.0, 1e9]);
        assert_eq!(densify(&[0.0, 1.0], 0.0), vec![0.0, 1.0]);
    }

    #[test]
    fn step_kept() {
        let anim =
            Animation { name: "a".into(), duration: 1.0, channels: vec![channel(Interpolation::Step, vec![0.0, 1.0])] };
        assert_eq!(sample_times(&anim).1, Interpolation::Step);
        let empty = Animation { name: "a".into(), duration: 0.0, channels: vec![] };
        assert_eq!(sample_times(&empty), (vec![0.0], Interpolation::Linear));
    }
}
