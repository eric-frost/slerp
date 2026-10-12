//! Evaluating a scene at a moment: node transforms from animation clips,
//! world matrices, joint matrices for skinning, and morph weights.

use crate::{Animation, Channel, Interpolation, Property, Scene, Transform};
use glam::{Mat4, Quat, Vec3};

/// Every node's transform and morph weights at one moment.  Start from
/// `Pose::rest`, optionally `sample` a clip, then call `update_world`.
#[derive(Clone, Debug)]
pub struct Pose {
    pub local: Vec<Transform>,
    pub weights: Vec<Vec<f32>>,
    /// Filled by `update_world`.
    pub world: Vec<Mat4>,
}

impl Pose {
    /// The scene as stored, with world matrices computed.
    pub fn rest(scene: &Scene) -> Pose {
        let mut pose = Pose {
            local: scene.nodes.iter().map(|n| n.transform).collect(),
            weights: scene.nodes.iter().map(|n| default_weights(scene, n)).collect(),
            world: vec![Mat4::IDENTITY; scene.nodes.len()],
        };
        pose.update_world(scene);
        pose
    }

    /// Resets to the rest pose without reallocating.
    pub fn reset(&mut self, scene: &Scene) {
        for (i, n) in scene.nodes.iter().enumerate() {
            self.local[i] = n.transform;
            self.weights[i].clone_from(&n.weights);
            let count = morph_count(scene, n);
            self.weights[i].resize(count, 0.0);
        }
    }

    /// Applies clip `anim` at `time` seconds (clamped to the clip) over the
    /// current local transforms.  Call `update_world` afterwards.
    pub fn sample(&mut self, scene: &Scene, anim: usize, time: f32) {
        let anim: &Animation = &scene.animations[anim];
        let time = time.clamp(0.0, anim.duration.max(0.0));
        for ch in &anim.channels {
            if ch.node >= self.local.len() || ch.times.is_empty() {
                continue;
            }
            match ch.property {
                Property::Translation => {
                    if let Some(v) = sample_vec(ch, time, 3) {
                        self.local[ch.node].translation = Vec3::new(v[0], v[1], v[2]);
                    }
                }
                Property::Scale => {
                    if let Some(v) = sample_vec(ch, time, 3) {
                        self.local[ch.node].scale = Vec3::new(v[0], v[1], v[2]);
                    }
                }
                Property::Rotation => {
                    if let Some(q) = sample_rotation(ch, time) {
                        self.local[ch.node].rotation = q;
                    }
                }
                Property::Weights => {
                    let count = self.weights[ch.node].len();
                    if count == 0 {
                        continue;
                    }
                    if let Some(v) = sample_vec(ch, time, count) {
                        self.weights[ch.node].copy_from_slice(&v[..count]);
                    }
                }
            }
        }
    }

    /// Blends towards `other` by `t` (0 keeps this pose, 1 takes `other`),
    /// for crossfading between clips.
    pub fn blend(&mut self, other: &Pose, t: f32) {
        for (a, b) in self.local.iter_mut().zip(&other.local) {
            *a = a.lerp(b, t);
        }
        for (a, b) in self.weights.iter_mut().zip(&other.weights) {
            for (x, y) in a.iter_mut().zip(b) {
                *x += (y - *x) * t;
            }
        }
    }

    /// Recomputes world matrices from local transforms.
    pub fn update_world(&mut self, scene: &Scene) {
        let mut stack: Vec<(usize, Mat4)> = scene.roots.iter().rev().map(|&r| (r, Mat4::IDENTITY)).collect();
        let mut seen = vec![false; scene.nodes.len()];
        while let Some((node, parent)) = stack.pop() {
            // A malformed file can make a node its own ancestor.
            if node >= scene.nodes.len() || seen[node] {
                continue;
            }
            seen[node] = true;
            let world = parent * self.local[node].matrix();
            self.world[node] = world;
            for &c in scene.nodes[node].children.iter().rev() {
                stack.push((c, world));
            }
        }
    }

    /// World-space skinning matrices for `skin`: joint world matrix times
    /// inverse bind matrix.
    pub fn joint_matrices(&self, scene: &Scene, skin: usize) -> Vec<Mat4> {
        let skin = &scene.skins[skin];
        skin.joints
            .iter()
            .enumerate()
            .map(|(i, &j)| {
                let world = self.world.get(j).copied().unwrap_or(Mat4::IDENTITY);
                world * skin.inverse_bind.get(i).copied().unwrap_or(Mat4::IDENTITY)
            })
            .collect()
    }
}

fn morph_count(scene: &Scene, node: &crate::Node) -> usize {
    node.mesh
        .and_then(|m| scene.meshes.get(m))
        .map(|m| m.primitives.iter().map(|p| p.targets.len()).max().unwrap_or(0))
        .unwrap_or(0)
}

fn default_weights(scene: &Scene, node: &crate::Node) -> Vec<f32> {
    let mut w = node.weights.clone();
    w.resize(morph_count(scene, node), 0.0);
    w
}

/// Finds the keys around `time`: (index of the earlier key, blend factor
/// 0..1 towards the next, seconds between them).
fn locate(times: &[f32], time: f32) -> (usize, f32, f32) {
    if times.len() == 1 || time <= times[0] {
        return (0, 0.0, 0.0);
    }
    let last = times.len() - 1;
    if time >= times[last] {
        return (last, 0.0, 0.0);
    }
    let next = times.partition_point(|&t| t <= time).clamp(1, last);
    let prev = next - 1;
    let span = times[next] - times[prev];
    let t = if span > 0.0 { (time - times[prev]) / span } else { 0.0 };
    (prev, t, span)
}

/// Samples a channel of `n` floats per key.
fn sample_vec(ch: &Channel, time: f32, n: usize) -> Option<Vec<f32>> {
    let keys = ch.times.len();
    let stride = if ch.interpolation == Interpolation::CubicSpline { n * 3 } else { n };
    if ch.values.len() < keys * stride {
        return None;
    }
    let (i, t, span) = locate(&ch.times, time);
    let value = |k: usize, part: usize| -> &[f32] {
        let base = k * stride + part * n;
        &ch.values[base..base + n]
    };
    let at_end = i + 1 >= keys;
    Some(match ch.interpolation {
        Interpolation::Step => value(i, 0).to_vec(),
        Interpolation::Linear => {
            if at_end || t == 0.0 {
                value(i, 0).to_vec()
            } else {
                let (a, b) = (value(i, 0), value(i + 1, 0));
                a.iter().zip(b).map(|(a, b)| a + (b - a) * t).collect()
            }
        }
        Interpolation::CubicSpline => {
            if at_end || t == 0.0 {
                value(i, 1).to_vec()
            } else {
                let (p0, m0) = (value(i, 1), value(i, 2));
                let (p1, m1) = (value(i + 1, 1), value(i + 1, 0));
                (0..n).map(|c| hermite(p0[c], m0[c] * span, p1[c], m1[c] * span, t)).collect()
            }
        }
    })
}

fn sample_rotation(ch: &Channel, time: f32) -> Option<Quat> {
    let q = |v: &[f32]| Quat::from_xyzw(v[0], v[1], v[2], v[3]);
    if ch.interpolation == Interpolation::Linear {
        let keys = ch.times.len();
        if ch.values.len() < keys * 4 {
            return None;
        }
        let (i, t, _) = locate(&ch.times, time);
        let a = q(&ch.values[i * 4..i * 4 + 4]).normalize();
        if i + 1 >= keys || t == 0.0 {
            return Some(a);
        }
        let b = q(&ch.values[(i + 1) * 4..(i + 1) * 4 + 4]).normalize();
        return Some(a.slerp(b, t));
    }
    let v = sample_vec(ch, time, 4)?;
    let r = q(&v);
    Some(if r.length_squared() > 0.0 { r.normalize() } else { Quat::IDENTITY })
}

fn hermite(p0: f32, m0: f32, p1: f32, m1: f32, t: f32) -> f32 {
    let t2 = t * t;
    let t3 = t2 * t;
    (2.0 * t3 - 3.0 * t2 + 1.0) * p0 + (t3 - 2.0 * t2 + t) * m0 + (-2.0 * t3 + 3.0 * t2) * p1 + (t3 - t2) * m1
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Animation, Node};

    fn one_node_scene(ch: Channel) -> Scene {
        let mut scene = Scene::default();
        scene.nodes.push(Node::new("a"));
        scene.roots.push(0);
        scene.animations.push(Animation { name: "clip".into(), duration: 2.0, channels: vec![ch] });
        scene
    }

    #[test]
    fn linear_translation_halfway() {
        let scene = one_node_scene(Channel {
            node: 0,
            property: Property::Translation,
            interpolation: Interpolation::Linear,
            times: vec![0.0, 2.0],
            values: vec![0.0, 0.0, 0.0, 2.0, 4.0, 6.0],
        });
        let mut pose = Pose::rest(&scene);
        pose.sample(&scene, 0, 1.0);
        pose.update_world(&scene);
        assert_eq!(pose.local[0].translation, Vec3::new(1.0, 2.0, 3.0));
        assert_eq!(pose.world[0].w_axis.truncate(), Vec3::new(1.0, 2.0, 3.0));
    }

    #[test]
    fn step_holds_value() {
        let scene = one_node_scene(Channel {
            node: 0,
            property: Property::Scale,
            interpolation: Interpolation::Step,
            times: vec![0.0, 1.0],
            values: vec![1.0, 1.0, 1.0, 3.0, 3.0, 3.0],
        });
        let mut pose = Pose::rest(&scene);
        pose.sample(&scene, 0, 0.99);
        assert_eq!(pose.local[0].scale, Vec3::ONE);
        pose.sample(&scene, 0, 1.5);
        assert_eq!(pose.local[0].scale, Vec3::splat(3.0));
    }

    #[test]
    fn cubic_spline_passes_through_keys() {
        // Keys at 0 and 1 with zero tangents: value at t=0.5 is the midpoint.
        let scene = one_node_scene(Channel {
            node: 0,
            property: Property::Translation,
            interpolation: Interpolation::CubicSpline,
            times: vec![0.0, 1.0],
            values: vec![0.0, 0.0, 0.0, 1.0, 1.0, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 3.0, 3.0, 3.0, 0.0, 0.0, 0.0],
        });
        let mut pose = Pose::rest(&scene);
        pose.sample(&scene, 0, 0.5);
        assert!((pose.local[0].translation - Vec3::splat(2.0)).length() < 1e-5);
    }
}
