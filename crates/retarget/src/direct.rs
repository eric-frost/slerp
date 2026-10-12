//! Playing clips on a skeleton whose joints have the same names as the
//! clips' own, after normalising the names (`mixamorig:Hips` meets
//! `Hips`).
//!
//! Rotation and scale channels are copied as they are, which is right when
//! the rest poses agree; `Matching::compatible` checks that they do, and
//! skeletons that differ are retargeted as humanoids instead when they can
//! be.
//!
//! Translation channels are where skeletons of different sizes part
//! company: copying them would stretch the target's bones to the source's
//! lengths.  So each one is re-expressed relative to the target's rest
//! pose: a channel that only holds its joint at the source's rest offset
//! (as exporters write for every joint) is dropped, leaving the target's
//! own offset; real motion is added to the target's rest offset, scaled by
//! the ratio of the two bones' lengths, or for a joint that sits on its
//! parent (a root at the origin) by the ratio of the skeletons' sizes.
//! For identical skeletons this copies the channel unchanged.

use crate::names::normalise;
use crate::skeleton::{Skeleton, position};
use slerp_scene::glam::Vec3;
use slerp_scene::{Animation, Channel, Interpolation, Mesh, Property};
use std::collections::HashMap;

/// The most two matched joints' rest poses may differ, in radians, for
/// clips to play by name.  Exporters round-trip rotations to well under a
/// degree; a T-pose against an A-pose differs by 30 or more.
const MAX_REST_DIFFERENCE: f32 = 0.1;

/// The share of the clips' joints that must match for clips to play by
/// name.
pub const MIN_SHARE: f32 = 0.6;

/// Library nodes matched to scene nodes by name.
pub struct Matching<'a> {
    src: &'a Skeleton<'a>,
    dst: &'a Skeleton<'a>,
    /// Scene node for each library node, for the nodes the clips move.
    map: Vec<Option<usize>>,
    /// Library nodes with transform channels, and how many found a match.
    moved: usize,
    matched: usize,
    /// Library nodes with morph weight channels, and how many matched.
    morphed: usize,
    morphs_matched: usize,
    /// For each matched joint, the angle by which its rest rotation or
    /// offset from its parent differs between the skeletons, sorted.
    differences: Vec<f32>,
    /// The matched joints' extent in the scene over that in the library.
    size_ratio: f32,
    /// The library skeleton's extent, for tolerances.
    src_size: f32,
}

impl<'a> Matching<'a> {
    pub fn new(src: &'a Skeleton<'a>, dst: &'a Skeleton<'a>) -> Self {
        // 1. The library nodes clips move.
        let count = src.scene.nodes.len();
        let (mut moves, mut morphs) = (vec![false; count], vec![false; count]);
        for ch in src.scene.animations.iter().flat_map(|a| &a.channels) {
            if ch.node < count {
                match ch.property {
                    Property::Weights => morphs[ch.node] = true,
                    _ => moves[ch.node] = true,
                }
            }
        }

        // 2. Scene nodes by normalised name.
        let mut by_name: HashMap<String, Vec<usize>> = HashMap::new();
        for (i, node) in dst.scene.nodes.iter().enumerate() {
            by_name.entry(normalise(&node.name)).or_default().push(i);
        }

        // 3. Each moved node's namesake.  Repeated names (a MetaHuman's
        //    `head` joint and `head` mesh) go to joints for transforms and
        //    to meshes for morph weights.
        let has_morphs = |m: Option<usize>, scene: &slerp_scene::Scene| {
            m.and_then(|m| scene.meshes.get(m)).is_some_and(|m| morph_count(m) > 0)
        };
        let mut map = vec![None; count];
        for i in (0..count).filter(|&i| moves[i] || morphs[i]) {
            let key = normalise(&src.scene.nodes[i].name);
            let Some(candidates) = by_name.get(&key).filter(|_| !key.is_empty()) else { continue };
            let score = |d: usize| {
                let node = &dst.scene.nodes[d];
                if moves[i] {
                    if dst.joint[d] {
                        2
                    } else if node.mesh.is_none() {
                        1
                    } else {
                        0
                    }
                } else if has_morphs(node.mesh, dst.scene) {
                    1
                } else {
                    0
                }
            };
            // The first of the best.
            map[i] = candidates.iter().copied().rev().max_by_key(|&d| score(d));
        }
        let moved = moves.iter().filter(|&&m| m).count();
        let matched = (0..count).filter(|&i| moves[i] && map[i].is_some()).count();
        let morphed = morphs.iter().filter(|&&m| m).count();
        let morphs_matched = (0..count).filter(|&i| morphs[i] && map[i].is_some()).count();

        // 4. How the matched joints' rest poses differ, and the skeletons'
        //    sizes.
        let pairs: Vec<(usize, usize)> = (0..count).filter(|&i| moves[i]).filter_map(|i| Some((i, map[i]?))).collect();
        let longest = |skel: &Skeleton, side: fn(&(usize, usize)) -> usize| {
            pairs.iter().map(|p| skel.scene.nodes[side(p)].transform.translation.length()).fold(0.0, f32::max)
        };
        let (src_long, dst_long) = (longest(src, |p| p.0), longest(dst, |p| p.1));
        let mut differences: Vec<f32> = pairs
            .iter()
            .map(|&(s, d)| {
                let (a, b) = (&src.scene.nodes[s].transform, &dst.scene.nodes[d].transform);
                let mut angle = a.rotation.normalize().angle_between(b.rotation.normalize());
                // Offsets shorter than 1% of the longest are noise.
                if a.translation.length() > 0.01 * src_long && b.translation.length() > 0.01 * dst_long {
                    angle = angle.max(a.translation.angle_between(b.translation));
                }
                if angle.is_finite() { angle } else { std::f32::consts::PI }
            })
            .collect();
        differences.sort_by(f32::total_cmp);
        let extent = |skel: &Skeleton, side: fn(&(usize, usize)) -> usize| {
            let (mut lo, mut hi) = (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN));
            for p in &pairs {
                let v = position(&skel.rest.world[side(p)]);
                if v.is_finite() {
                    lo = lo.min(v);
                    hi = hi.max(v);
                }
            }
            if hi.cmpge(lo).all() { (hi - lo).length() } else { 0.0 }
        };
        let (src_size, dst_size) = (extent(src, |p| p.0), extent(dst, |p| p.1));
        let size_ratio = if src_size > 1e-9 && dst_size > 1e-9 { dst_size / src_size } else { 1.0 };
        Matching { src, dst, map, moved, matched, morphed, morphs_matched, differences, size_ratio, src_size }
    }

    /// True when enough of the clips' joints have namesakes: 60% of the
    /// joints they move, or of the meshes they morph when they move none.
    pub fn enough(&self) -> bool {
        if self.moved > 0 {
            self.matched as f32 >= MIN_SHARE * self.moved as f32
        } else {
            self.morphs_matched > 0 && self.morphs_matched as f32 >= MIN_SHARE * self.morphed as f32
        }
    }

    /// True when the matched joints' rest poses agree, so local rotations
    /// mean the same on both skeletons.  One joint in twenty may differ
    /// (an end joint, a prop), but no more.
    pub fn compatible(&self) -> bool {
        match self.differences.len() {
            0 => true,
            n => self.differences[(n * 19 / 20).min(n - 1)] <= MAX_REST_DIFFERENCE,
        }
    }

    /// The library's clip `anim` on the scene's nodes.
    pub fn bake(&self, anim: usize) -> Animation {
        let clip = &self.src.scene.animations[anim];
        let mut channels = Vec::with_capacity(clip.channels.len());
        for ch in &clip.channels {
            let Some(node) = self.map.get(ch.node).copied().flatten() else { continue };
            let moved = match ch.property {
                Property::Rotation | Property::Scale => Some(Channel { node, ..ch.clone() }),
                Property::Translation => self.translation(ch, node),
                Property::Weights => self.weights(ch, node),
            };
            channels.extend(moved);
        }
        Animation { name: clip.name.clone(), duration: clip.duration.max(0.0), channels }
    }

    /// A translation channel re-expressed relative to the target's rest
    /// offset, or None when it only holds the source's rest offset.
    fn translation(&self, ch: &Channel, node: usize) -> Option<Channel> {
        let (src, dst) = (self.src, self.dst);
        let s_rest = src.scene.nodes[ch.node].transform.translation;
        let t_rest = dst.scene.nodes[node].transform.translation;
        let cubic = ch.interpolation == Interpolation::CubicSpline;
        let stride = if cubic { 9 } else { 3 };
        let keys = ch.times.len();
        if keys == 0 || ch.values.len() < keys.checked_mul(stride)? {
            return None;
        }
        let values = &ch.values[..keys * stride];
        // Sizes in the nodes' own units, which their parents may scale.
        let (s_scale, t_scale) = (src.parent_scale(ch.node), dst.parent_scale(node));
        let src_local = self.src_size / s_scale;
        let value = |k: usize| {
            let at = k * stride + if cubic { 3 } else { 0 };
            Vec3::from_slice(&values[at..at + 3])
        };
        let tolerance = 1e-4 * src_local.max(1e-6);
        if (0..keys).all(|k| value(k).distance(s_rest) <= tolerance) {
            return None;
        }
        let ratio = if s_rest.length() > 1e-3 * src_local && t_rest.length() > 1e-9 {
            t_rest.length() / s_rest.length()
        } else {
            self.size_ratio * s_scale / t_scale
        };
        let mut out = Vec::with_capacity(values.len());
        for (i, v) in values.chunks_exact(3).enumerate() {
            let v = Vec3::from_slice(v);
            // Cubic splines store in-tangent, value, out-tangent per key;
            // tangents are rates, so they only scale.
            let tangent = cubic && i % 3 != 1;
            let w = if tangent { v * ratio } else { t_rest + (v - s_rest) * ratio };
            out.extend_from_slice(&w.to_array());
        }
        Some(Channel { node, values: out, ..ch.clone() })
    }

    /// A morph weight channel for the scene node's mesh: copied when the
    /// meshes have the same number of targets, matched by target name
    /// when both name them, otherwise dropped.
    fn weights(&self, ch: &Channel, node: usize) -> Option<Channel> {
        let dst_mesh = self.dst.scene.nodes[node].mesh.and_then(|m| self.dst.scene.meshes.get(m))?;
        let dst_count = morph_count(dst_mesh);
        let parts = if ch.interpolation == Interpolation::CubicSpline { 3 } else { 1 };
        let keys = ch.times.len();
        if keys == 0 || dst_count == 0 {
            return None;
        }
        let src_count = ch.values.len() / (keys * parts);
        if src_count == 0 {
            return None;
        }
        let src_names = self.src.scene.nodes[ch.node]
            .mesh
            .and_then(|m| self.src.scene.meshes.get(m))
            .map(|m| m.target_names.as_slice())
            .unwrap_or(&[]);
        let dst_names = &dst_mesh.target_names;
        let named = src_names.len() == src_count && dst_names.len() == dst_count;
        if src_count == dst_count && (!named || src_names == dst_names.as_slice()) {
            return Some(Channel { node, ..ch.clone() });
        }
        if !named {
            return None;
        }
        let from: Vec<Option<usize>> = dst_names.iter().map(|n| src_names.iter().position(|s| s == n)).collect();
        if from.iter().all(Option::is_none) {
            return None;
        }
        let mut values = Vec::with_capacity(keys * parts * dst_count);
        for group in ch.values.chunks_exact(src_count).take(keys * parts) {
            values.extend(from.iter().map(|f| f.map_or(0.0, |s| group[s])));
        }
        Some(Channel { node, values, ..ch.clone() })
    }
}

fn morph_count(mesh: &Mesh) -> usize {
    mesh.primitives.iter().map(|p| p.targets.len()).max().unwrap_or(0)
}
