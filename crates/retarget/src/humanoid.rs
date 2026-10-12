//! Humanoid retargeting, for skeletons whose joints are named differently
//! or whose rest poses differ (an A-pose MetaHuman playing clips made on a
//! T-pose mannequin).
//!
//! Copying local rotations would carry the rest poses' differences into
//! every frame, so the transfer happens in world space:
//!
//! 1. Both skeletons are read as humanoids, in VRM 1.0's bone names, and
//!    the source is turned about the vertical to face the target's way.
//! 2. The target is posed into the source's rest pose: each shared bone is
//!    turned so it points at its child as the source's does, and the upper
//!    chest and hands also match their sideways axis.  This is the
//!    reference pose.  The hips stay upright as at rest, since rigs place
//!    the hips joint too differently for the line from it to the spine to
//!    mean the same on both.  Shoulders keep their own rest angle, since longer
//!    collarbones that start further forward (a MetaHuman's) would
//!    otherwise throw the shoulders back.  Feet stay flat as at rest and
//!    only turn about the vertical to point their toes the source's way:
//!    ankle heights and foot lengths differ, so matching the slope from
//!    ankle to toes would tip a steeper foot's toes off the ground.
//! 3. At each sampled moment every shared target bone makes the same
//!    world-space turn away from the reference pose that its source bone
//!    makes away from its rest pose.  Bones with no counterpart keep their
//!    rest pose relative to their parent.
//! 4. The hips move by the source hips' motion, scaled by the ratio of hip
//!    heights, measured at the hip sockets (the upper legs' joints): legs
//!    set the stride, and where rigs put the hips joint itself varies (8 cm
//!    above the sockets on some VRM avatars, at them on Unreal's).
//! 5. Foot IK corrects the legs where the target's proportions put planted
//!    feet elsewhere than the scaled travel expects (see `feet`).  Clips
//!    are then baked at least 30 times a second so the feet hold between
//!    keys, and both versions come out, so a viewer can switch it.
//!
//! This is the method of chalk's `tools/humans/retarget.py`, which bakes
//! the Universal Animation Library onto MetaHumans, generalised from
//! Unreal's joint names to any recognised humanoid, with four departures
//! described at `Method`: hips kept upright, feet kept flat, the hip
//! sockets rather than the hips joint carrying the travel, and foot IK.  Both face
//! skeletons by the line across the hips; a foot's toe line would carry a
//! character's toe-out into every clip.

use crate::feet::{self, Feet};
use crate::names::{CORE, MIN_CORE, all_bones};
use crate::skeleton::{Skeleton, densify, position, rotation, sample_times};
use slerp_scene::glam::{Mat4, Quat, Vec3};
use slerp_scene::{Animation, Channel, Humanoid, Interpolation, Property, Scene};
use std::collections::HashMap;

/// A retarget from one humanoid to another, worked out once and used for
/// every clip.
pub struct Plan<'a> {
    src: &'a Skeleton<'a>,
    bones: Vec<Bone>,
    /// Turns the source about the vertical to face the target's way.
    yaw: Quat,
    hips: Hips,
    /// The legs, for foot IK after the transfer, when the method asks for
    /// it and both skeletons have them.
    feet: Option<Feet>,
}

/// A clip baked for the target: as the rotation transfer makes it, and
/// with foot IK when the plan has feet and IK changed something.
pub struct Baked {
    pub plain: Animation,
    pub feet: Option<Animation>,
}

/// A bone both skeletons have.
struct Bone {
    src: usize,
    dst: usize,
    /// Takes the source bone's world rotation at a moment to the target
    /// bone's: the inverse of the source's turned rest rotation, then the
    /// target's reference rotation.
    key: Quat,
    /// The nearest shared bone above this one, as an index into `bones`.
    parent: Option<usize>,
    /// The rotation from that bone (or from the world, when there is none)
    /// to this bone's parent, through the rest pose of the nodes between,
    /// which no clip moves.
    offset: Quat,
}

/// The hips carry the clip's travel.  What follows the source is the
/// point between the hip sockets, where the legs hang, rather than the
/// hips joint itself: rigs put that joint anywhere from the sockets to 8 cm
/// above them, and as the pelvis tilts, a joint that high moves differently
/// from the sockets, lifting the feet off the ground.
struct Hips {
    /// The source nodes whose average position is followed: the upper
    /// legs, or the hips when either skeleton lacks them.
    src: Vec<usize>,
    /// Where that point is at rest.
    src_rest: Vec3,
    /// Where the target's matching point is in the reference pose.
    place: Vec3,
    /// From the target hips joint to that point, in the hips' own frame.
    offset: Vec3,
    /// Index into `Plan::bones` of the hips.
    bone: usize,
    /// Target hip height over source hip height.
    scale: f32,
    /// From world space to the space of the target hips' parent.
    to_local: Mat4,
    dst: usize,
}

/// The choices where this retarget departs from chalk's `retarget.py`.
/// Each departure fixed a character that came out wrong; chalk's choices
/// remain so tests can check the shared method against chalk's own bakes.
#[derive(Clone, Copy, Debug)]
pub struct Method {
    /// Turn the hips in the reference pose to point at the spine, as chalk
    /// does, rather than keep them upright.  Rigs put the hips joint
    /// anywhere from the hip sockets to the waist, so that line is no
    /// anatomical axis: on Alicia it tilted the pelvis 32 degrees and
    /// lifted the feet 1.3 cm off the ground.
    pub aim_hips: bool,
    /// Tip the feet to the source's slope from ankle to toes, as chalk
    /// does, rather than keep them flat; a steeper foot's toes then float.
    pub aim_feet: bool,
    /// Move the hips joint by the source's and measure hip height there,
    /// as chalk does, rather than at the hip sockets.
    pub hips_joint: bool,
    /// Correct the legs afterwards so planted feet stay planted (see
    /// `feet`); chalk doesn't.
    pub foot_ik: bool,
}

impl Method {
    pub const SLERP: Method = Method { aim_hips: false, aim_feet: false, hips_joint: false, foot_ik: true };
    pub const CHALK: Method = Method { aim_hips: true, aim_feet: true, hips_joint: true, foot_ik: false };
}

/// The skeleton's humanoid bones: the ones its file declares (VRM), or
/// those recognised from its joint names.
pub fn humanoid_of(scene: &Scene) -> Option<Humanoid> {
    scene.humanoid.clone().filter(|h| !h.bones.is_empty()).or_else(|| crate::names::recognise(scene))
}

impl<'a> Plan<'a> {
    /// Plans a retarget, or None unless both skeletons are humanoids that
    /// share the hips and at least 60% of VRM's required bones.
    pub fn new(src: &'a Skeleton<'a>, dst: &Skeleton, method: Method) -> Option<Plan<'a>> {
        let (sh, th) = (humanoid_of(src.scene)?, humanoid_of(dst.scene)?);

        // 1. The bones both have, each node once, in the target's order
        //    (parents first).
        let usable = |skel: &Skeleton, h: &Humanoid, bone: &str| {
            h.node(bone).filter(|&n| n < skel.scene.nodes.len() && skel.rank[n] != usize::MAX)
        };
        let mut pairs: Vec<(String, usize, usize)> = Vec::new();
        for bone in all_bones() {
            if let (Some(s), Some(d)) = (usable(src, &sh, &bone), usable(dst, &th, &bone))
                && !pairs.iter().any(|&(_, ps, pd)| ps == s || pd == d)
            {
                pairs.push((bone, s, d));
            }
        }
        let core = CORE.iter().filter(|c| pairs.iter().any(|(b, ..)| b == *c)).count();
        if core < MIN_CORE || !pairs.iter().any(|(b, ..)| b == "hips") {
            return None;
        }
        pairs.sort_by_key(|&(_, _, d)| dst.rank[d]);
        let index: HashMap<&str, usize> = pairs.iter().enumerate().map(|(i, (b, ..))| (b.as_str(), i)).collect();

        // 2. Facing: the turn about the vertical from the source's forward
        //    to the target's.
        let yaw = match (facing(src, &sh), facing(dst, &th)) {
            (Some(a), Some(b)) => Quat::from_rotation_y(b.x.atan2(b.z) - a.x.atan2(a.z)),
            _ => Quat::IDENTITY,
        };
        let src_pos: Vec<Vec3> = pairs.iter().map(|&(_, s, _)| yaw * src.rest_position(s)).collect();
        let src_rot: Vec<Quat> = pairs.iter().map(|&(_, s, _)| (yaw * src.rest_rotation(s)).normalize()).collect();

        // 3. The reference pose.
        let mut pose = dst.rest.clone();
        let top = ["upperChest", "chest", "spine"].into_iter().find(|b| index.contains_key(b));
        let set_world = |pose: &mut slerp_scene::Pose, d: usize, world: Quat| {
            let parent = dst.parents[d].map_or(Quat::IDENTITY, |p| rotation(&pose.world[p]));
            pose.local[d].rotation = (parent.inverse() * world).normalize();
            pose.update_world(dst.scene);
        };
        for (i, (bone, _, d)) in pairs.iter().enumerate() {
            if (bone == "hips" && !method.aim_hips) || bone.ends_with("Shoulder") {
                continue;
            }
            if let Some((side, "Foot")) = split_side(bone).filter(|_| !method.aim_feet) {
                let flat = |v: Vec3| Vec3::new(v.x, 0.0, v.z).normalize_or_zero();
                let mut world = dst.rest_rotation(*d);
                if let Some(&toes) = index.get(format!("{side}Toes").as_str()) {
                    let want = flat(src_pos[toes] - src_pos[i]);
                    let have = flat(dst.rest_position(pairs[toes].2) - dst.rest_position(*d));
                    if want != Vec3::ZERO && have != Vec3::ZERO {
                        world = Quat::from_rotation_y(want.x.atan2(want.z) - have.x.atan2(have.z)) * world;
                    }
                }
                set_world(&mut pose, *d, world.normalize());
                continue;
            }
            let Some(aim) = aims(bone).iter().find_map(|a| index.get(a.as_str()).copied()) else { continue };
            let here = |p: &slerp_scene::Pose, i: usize| position(&p.world[pairs[i].2]);
            let want = (src_pos[aim] - src_pos[i]).normalize_or_zero();
            let have = (here(&pose, aim) - here(&pose, i)).normalize_or_zero();
            if want == Vec3::ZERO || have == Vec3::ZERO {
                continue;
            }
            let mut turn = Quat::from_rotation_arc(have, want);
            let side = sides(bone, top)
                .into_iter()
                .find_map(|(x, y)| Some((*index.get(x.as_str())?, *index.get(y.as_str())?)));
            if let Some((x, y)) = side {
                let flat = |v: Vec3| (v - want * v.dot(want)).normalize_or_zero();
                let ws = flat(src_pos[x] - src_pos[y]);
                let wt = flat(turn * (here(&pose, x) - here(&pose, y)));
                if ws != Vec3::ZERO && wt != Vec3::ZERO {
                    let angle = want.dot(wt.cross(ws)).atan2(wt.dot(ws));
                    turn = Quat::from_axis_angle(want, angle) * turn;
                }
            }
            let world = (turn * rotation(&pose.world[*d])).normalize();
            set_world(&mut pose, *d, world);
        }

        // 4. Each bone's link to the nearest shared bone above it.
        let by_node: HashMap<usize, usize> = pairs.iter().enumerate().map(|(i, &(_, _, d))| (d, i)).collect();
        let mut bones = Vec::with_capacity(pairs.len());
        for (i, &(_, s, d)) in pairs.iter().enumerate() {
            let (mut parent, mut offset, mut n) = (None, Quat::IDENTITY, d);
            for _ in 0..dst.parents.len() {
                let Some(p) = dst.parents[n] else { break };
                if let Some(&j) = by_node.get(&p) {
                    parent = Some(j);
                    break;
                }
                offset = pose.local[p].rotation * offset;
                n = p;
            }
            if parent.is_none() {
                offset = dst.parents[d].map_or(Quat::IDENTITY, |p| rotation(&pose.world[p]));
            }
            let key = (src_rot[i].inverse() * rotation(&pose.world[d])).normalize();
            bones.push(Bone { src: s, dst: d, key, parent, offset: offset.normalize() });
        }

        // 5. The hips, which also carry the clip's travel.
        let bone = index["hips"];
        let (_, s, d) = pairs[bone];
        let legs = |h: &Humanoid, skel: &Skeleton| -> Option<Vec<usize>> {
            ["leftUpperLeg", "rightUpperLeg"].iter().map(|b| usable(skel, h, b)).collect()
        };
        let (src_anchor, dst_anchor) = match (legs(&sh, src), legs(&th, dst)) {
            (Some(a), Some(b)) if !method.hips_joint => (a, b),
            _ => (vec![s], vec![d]),
        };
        let mean = |p: &slerp_scene::Pose, nodes: &[usize]| {
            nodes.iter().map(|&n| position(&p.world[n])).sum::<Vec3>() / nodes.len() as f32
        };
        let src_rest = mean(&src.rest, &src_anchor);
        let place = mean(&pose, &dst_anchor);
        let offset = rotation(&pose.world[d]).inverse() * (place - position(&pose.world[d]));
        let heights = (hip_height(src, &sh, method.hips_joint), hip_height(dst, &th, method.hips_joint));
        let scale = match heights {
            (Some(a), Some(b)) => b / a,
            _ => 1.0,
        };
        let to_local = dst.parents[d].map_or(Mat4::IDENTITY, |p| pose.world[p].inverse());
        let to_local = if to_local.is_finite() { to_local } else { Mat4::IDENTITY };
        let hips = Hips { src: src_anchor, src_rest, place, offset, bone, scale, to_local, dst: d };

        // 6. The legs, for foot IK.
        let feet = method
            .foot_ik
            .then(|| {
                Feet::new(feet::Setup {
                    src,
                    dst,
                    pairs: &pairs,
                    yaw,
                    scale,
                    src_height: heights.0,
                    dst_height: heights.1,
                    forward: facing(dst, &th),
                    hips_rotation: rotation(&pose.world[d]),
                    reference: &pose,
                })
            })
            .flatten();
        Some(Plan { src, bones, yaw, hips, feet })
    }

    /// The legs foot IK corrects, for tests that measure it.
    #[cfg(test)]
    pub fn feet(&self) -> Option<&Feet> {
        self.feet.as_ref()
    }

    #[cfg(test)]
    pub fn yaw(&self) -> Quat {
        self.yaw
    }

    /// The source's clip `anim`, baked for the target into linear channels
    /// at the clip's own key times (30 a second for cubic splines, and at
    /// least that often for foot IK).
    pub fn bake(&self, anim: usize) -> Baked {
        let scene = self.src.scene;
        let clip = &scene.animations[anim];
        let (mut times, interpolation) = sample_times(clip);
        if self.feet.is_some() && interpolation == Interpolation::Linear {
            times = densify(&times, feet::MAX_STEP);
        }
        let mut rotations: Vec<Vec<f32>> = (0..self.bones.len()).map(|_| Vec::with_capacity(times.len() * 4)).collect();
        let mut travel: Vec<f32> = Vec::with_capacity(times.len() * 3);
        let mut watched: Vec<Vec3> = Vec::new();
        let mut world = vec![Quat::IDENTITY; self.bones.len()];
        let mut pose = self.src.rest.clone();
        let h = &self.hips;
        for &t in &times {
            // 1. The source at this moment.
            pose.reset(scene);
            pose.sample(scene, anim, t);
            pose.update_world(scene);
            if let Some(feet) = &self.feet {
                feet.watch(&pose.world, self.yaw, &mut watched);
            }
            // 2. Each target bone's world rotation, then its local one under
            //    its parent's.
            for (i, b) in self.bones.iter().enumerate() {
                let w = (self.yaw * rotation(&pose.world[b.src]) * b.key).normalize();
                world[i] = w;
                let parent = b.parent.map_or(Quat::IDENTITY, |p| world[p]) * b.offset;
                let mut q = (parent.inverse() * w).normalize();
                let keys = &mut rotations[i];
                // Neighbouring keys on the same side of the sphere, so
                // interpolation takes the short way.
                if let Some(prev) = keys.len().checked_sub(4).map(|k| Quat::from_slice(&keys[k..]))
                    && prev.dot(q) < 0.0
                {
                    q = -q;
                }
                keys.extend_from_slice(&q.to_array());
            }
            // 3. The hips' travel: the point between the sockets follows
            //    the source's, and the joint goes where that puts it.
            let anchor = h.src.iter().map(|&n| position(&pose.world[n])).sum::<Vec3>() / h.src.len() as f32;
            let moved = self.yaw * (anchor - h.src_rest) * h.scale;
            let joint = h.place + moved - world[h.bone] * h.offset;
            let local = h.to_local.transform_point3(joint);
            let local = if local.is_finite() { local } else { h.to_local.transform_point3(h.place - h.offset) };
            travel.extend_from_slice(&local.to_array());
        }
        // 4. Foot IK on a copy, so both versions come out.
        let feet = self.feet.as_ref().and_then(|feet| {
            let (mut rotations, mut travel) = (rotations.clone(), travel.clone());
            let changed = feet.apply(&times, &watched, &mut rotations, &mut travel);
            changed.then(|| self.clip(clip, &times, interpolation, rotations, travel))
        });
        Baked { plain: self.clip(clip, &times, interpolation, rotations, travel), feet }
    }

    /// A clip from baked rotation and travel keys.
    fn clip(
        &self,
        clip: &Animation,
        times: &[f32],
        interpolation: Interpolation,
        rotations: Vec<Vec<f32>>,
        travel: Vec<f32>,
    ) -> Animation {
        let mut channels: Vec<Channel> = self
            .bones
            .iter()
            .zip(rotations)
            .map(|(b, values)| Channel {
                node: b.dst,
                property: Property::Rotation,
                interpolation,
                times: times.to_vec(),
                values,
            })
            .collect();
        channels.push(Channel {
            node: self.hips.dst,
            property: Property::Translation,
            interpolation,
            times: times.to_vec(),
            values: travel,
        });
        Animation { name: clip.name.clone(), duration: clip.duration.max(0.0), channels }
    }
}

/// The skeleton's forward direction in the rest pose, flat on the ground:
/// across from its right side to its left, turned a quarter about the
/// vertical.  A character facing +Z has its left at +X.
pub fn facing(skel: &Skeleton, h: &Humanoid) -> Option<Vec3> {
    let pos = |b: &str| h.node(b).filter(|&n| n < skel.scene.nodes.len()).map(|n| skel.rest_position(n));
    let pairs = [
        ("leftUpperLeg", "rightUpperLeg"),
        ("leftUpperArm", "rightUpperArm"),
        ("leftShoulder", "rightShoulder"),
        ("leftFoot", "rightFoot"),
        ("leftHand", "rightHand"),
    ];
    pairs.into_iter().find_map(|(l, r)| {
        let f = (pos(l)? - pos(r)?).cross(Vec3::Y);
        let f = Vec3::new(f.x, 0.0, f.z).normalize_or_zero();
        (f != Vec3::ZERO).then_some(f)
    })
}

/// How high the hip sockets (the upper legs' joints, or the hips joint
/// when there are no legs or `joint` asks for it) are above the floor at
/// rest.  Characters stand on y = 0 almost always; when the feet are
/// clearly elsewhere, the lowest foot or toe joint stands in for the floor.
fn hip_height(skel: &Skeleton, h: &Humanoid, joint: bool) -> Option<f32> {
    let at = |b: &str| h.node(b).filter(|&n| n < skel.scene.nodes.len()).map(|n| skel.rest_position(n));
    let sockets: Vec<Vec3> =
        if joint { Vec::new() } else { ["leftUpperLeg", "rightUpperLeg"].iter().filter_map(|b| at(b)).collect() };
    let hips = if sockets.is_empty() {
        at("hips")?.y
    } else {
        sockets.iter().map(|p| p.y).sum::<f32>() / sockets.len() as f32
    };
    let lowest =
        ["leftFoot", "rightFoot", "leftToes", "rightToes"].iter().filter_map(|b| at(b)).map(|p| p.y).reduce(f32::min);
    let floor = match lowest {
        Some(y) if y.abs() > 0.3 * (hips - y).abs() => y,
        _ => 0.0,
    };
    let height = hips - floor;
    (height.is_finite() && height > 1e-6).then_some(height)
}

/// The bones a bone may point at in the reference pose, nearest first; it
/// points at the first both skeletons have.
fn aims(bone: &str) -> Vec<String> {
    const TORSO: [&str; 6] = ["hips", "spine", "chest", "upperChest", "neck", "head"];
    if let Some(i) = TORSO.iter().position(|b| *b == bone) {
        return TORSO[i + 1..].iter().map(|b| b.to_string()).collect();
    }
    let Some((side, part)) = split_side(bone) else { return Vec::new() };
    let next: Vec<String> = match part {
        "UpperArm" => vec!["LowerArm".into()],
        "LowerArm" => vec!["Hand".into()],
        "Hand" => ["MiddleProximal", "IndexProximal", "RingProximal", "LittleProximal"].map(String::from).into(),
        "UpperLeg" => vec!["LowerLeg".into()],
        "LowerLeg" => vec!["Foot".into()],
        "Foot" => vec!["Toes".into()],
        "ThumbMetacarpal" => vec!["ThumbProximal".into()],
        "ThumbProximal" => vec!["ThumbDistal".into()],
        _ => match (part.strip_suffix("Proximal"), part.strip_suffix("Intermediate")) {
            (Some(finger), _) => vec![format!("{finger}Intermediate")],
            (_, Some(finger)) => vec![format!("{finger}Distal")],
            _ => Vec::new(),
        },
    };
    next.into_iter().map(|n| format!("{side}{n}")).collect()
}

/// Pairs of bones, left then right, whose direction also fixes how a bone
/// turns about the line to its child: across the hips, across the top of
/// the chest (`top`), and across the palm.
fn sides(bone: &str, top: Option<&str>) -> Vec<(String, String)> {
    let both = |l: &str, r: &str| (l.to_string(), r.to_string());
    if bone == "hips" {
        return vec![both("leftUpperLeg", "rightUpperLeg")];
    }
    if Some(bone) == top {
        return vec![both("leftShoulder", "rightShoulder"), both("leftUpperArm", "rightUpperArm")];
    }
    match split_side(bone) {
        Some((side, "Hand")) => [("Index", "Little"), ("Index", "Ring"), ("Middle", "Little")]
            .iter()
            .map(|(a, b)| (format!("{side}{a}Proximal"), format!("{side}{b}Proximal")))
            .collect(),
        _ => Vec::new(),
    }
}

fn split_side(bone: &str) -> Option<(&'static str, &str)> {
    if let Some(rest) = bone.strip_prefix("left") {
        Some(("left", rest))
    } else {
        bone.strip_prefix("right").map(|rest| ("right", rest))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aim_chains() {
        assert_eq!(aims("hips"), ["spine", "chest", "upperChest", "neck", "head"]);
        assert!(aims("head").is_empty());
        assert_eq!(aims("leftHand")[0], "leftMiddleProximal");
        assert_eq!(aims("rightIndexProximal"), ["rightIndexIntermediate"]);
        assert_eq!(aims("rightLittleIntermediate"), ["rightLittleDistal"]);
        assert_eq!(aims("leftThumbMetacarpal"), ["leftThumbProximal"]);
        assert_eq!(aims("leftThumbProximal"), ["leftThumbDistal"]);
        assert!(aims("leftThumbDistal").is_empty());
        assert!(aims("leftToes").is_empty());
        assert_eq!(sides("chest", Some("chest")).len(), 2);
        assert!(sides("chest", Some("upperChest")).is_empty());
        assert_eq!(sides("rightHand", None)[0], ("rightIndexProximal".into(), "rightLittleProximal".into()));
    }
}
