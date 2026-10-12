//! Foot IK after a humanoid retarget, so planted feet stay planted on a
//! character whose legs differ from the clip's.
//!
//! The rotation transfer gives every bone the source's turn away from its
//! rest pose, and the hips the source's travel scaled by hip height.  When
//! the target's thighs and shins are proportioned differently, the same
//! angles put the feet somewhere else than the scaled travel expects, so a
//! foot that stood still in the source slides, floats or sinks.  This pass
//! corrects the legs afterwards:
//!
//! 1. Contacts are found on the source clip, per foot, for the ankle and the
//!    toes: moments the joint is near its lowest height in the clip and
//!    nearly still across the ground, with hysteresis, a minimum length and
//!    short gaps joined, so contacts don't flicker.  The ground moves under
//!    a clip made in place (the planted foot slides back at walking speed),
//!    so its speed is taken as that of whichever joint is on the ground.
//! 2. While a foot is held, its goal is where the source's joint is, scaled
//!    by the ratio of hip heights (the ratio the hips' travel is scaled by)
//!    about the target's own rest stance, and pinned: averaged over the
//!    moments the joint is down, then held.  A pin stands still under a clip
//!    with root motion, and lets go if the source's foot slides off it;
//!    under a clip made in place, whose treadmill changes speed over the
//!    stride, it moves across with the source's joint and only its height is
//!    held.  The pin starts on the joint that comes down first (the heel,
//!    usually) and passes to the toes as the heel lifts or the foot rolls
//!    onto them, handed over where the foot is at that moment so it never
//!    jumps.  Neither joint of a held foot goes below where the source has
//!    it while down: the ground's height is kept.
//! 3. When a held foot is out of the leg's reach, the hips come down just
//!    enough, smoothed over a few frames so they don't bob.
//! 4. A two-bone solver turns the thigh and shin so the ankle reaches its
//!    goal, keeping the knee's hinge: the leg's plane swings as little as
//!    it can, and the knee bends the way it already bends (forward when the
//!    leg is straight).  The foot keeps its world rotation from the
//!    transfer, which matches the source's pitch against the ground (flat
//!    while the source's foot is flat).  Toes, spine, arms and head keep
//!    their rotations; only the hips' height can move them.
//! 5. The correction fades in before each contact and out after it over
//!    `BLEND` seconds, so swinging legs keep the transferred motion, and at
//!    every moment an ankle or toe the transfer or the lowered hips put
//!    below the lowest the source ever has it is lifted back.

use slerp_scene::Transform;
use slerp_scene::glam::{Mat4, Quat, Vec3};

use crate::skeleton::{Skeleton, position, rotation};

/*┌─────────────────╮
| Tuning          |
└─────────────────╯*/
//region Tuning

/// A joint enters a contact within this height of its lowest point in the
/// clip, in source hip heights (about 3.5 cm on an adult).
const ENTER_HEIGHT: f32 = 0.035;
/// And stays in it while within this height.
const EXIT_HEIGHT: f32 = 0.06;
/// The lowest point is the height a tenth of the clip is at or below, so
/// a frame dipping through the floor doesn't lift every contact off it.
const LOW: f32 = 0.1;
/// A joint enters a contact moving across the ground slower than this, in
/// source hip heights a second.  A swinging foot moves at 2 to 6; a
/// planted one rocks at up to 0.4 as the foot rolls, more in a run.
/// Height is checked on its own, so only movement across counts.
const ENTER_SPEED: f32 = 0.5;
/// And stays in it while slower than this.
const EXIT_SPEED: f32 = 0.8;
/// A planted joint is down, at the bottom of its contact, within this
/// height of the lowest it gets in that contact (about a centimetre): a
/// heel risen further has lifted, and the pin passes to the toes.
const LIFT: f32 = 0.012;
/// A pin lets go once the source's joint has moved this far across the
/// ground from where it was when pinned (about 3 cm): the source's foot
/// is sliding off, and holding the target's would overstretch its leg.
const RELEASE: f32 = 0.03;
/// A clip whose planted feet slide back faster than this, in source hip
/// heights a second, was made in place.
const IN_PLACE: f32 = 0.05;
/// Under a clip made in place, the speed tests allow this share of the
/// ground's speed: a stride's speed changes as a foot lands and pushes
/// off, and the planted foot shows it.
const SPEED_SHARE: f32 = 0.15;
/// Contacts shorter than this, in seconds, are noise.
const MIN_CONTACT: f32 = 0.06;
/// Gaps shorter than this between two contacts of a joint that stays low
/// are joined.
const MIN_GAP: f32 = 0.06;
/// Seconds over which the correction fades in before a contact and out
/// after it.
const BLEND: f32 = 0.12;
/// The hips' drop is smoothed over this many seconds either side.
const SMOOTH: f32 = 0.1;
/// The most the hips may come down, in target hip heights; beyond, the leg
/// stays short of its goal.
const MAX_DROP: f32 = 0.2;
/// A leg reaches this share of its length when the hips come down, so it
/// isn't locked straight.
const REACH: f32 = 0.998;
/// Clips are sampled at least this often when foot IK applies, so the
/// pinned feet hold between keys.
pub const MAX_STEP: f32 = 1.0 / 30.0;

//endregion

/*┌─────────────────╮
| Plan            |
└─────────────────╯*/
//region Plan

/// The legs of a humanoid retarget, worked out once and applied to every
/// clip.
pub struct Feet {
    legs: Vec<Leg>,
    /// Index into `Plan::bones` of the hips, whose local rotation keys the
    /// hips node's.
    hips: usize,
    /// The source's hip height: the unit of the contact thresholds.
    unit: f32,
    /// Target hip height over the source's: how much the feet's motion is
    /// scaled, as the hips' travel is.
    scale: f32,
    /// The most the hips may come down.
    max_drop: f32,
    /// The target's forward direction in the hips' frame at rest, for a knee
    /// bending from a straight leg.
    forward: Vec3,
}

/// One leg.
struct Leg {
    /// Indices into `Plan::bones`.
    upper: usize,
    lower: usize,
    foot: usize,
    /// The source's ankle and toes (the ankle again when either skeleton
    /// has no toes).
    src: [usize; 2],
    toes: bool,
    /// Their rest positions, turned to face the target's way.
    src_rest: [Vec3; 2],
    /// The target's ankle and toes (the ankle again without toes), which
    /// tests measure, and where they are when the source is at rest:
    /// across the ground as the reference pose has them, so the stance is
    /// the source's, and at their rest height, on the ground.
    #[cfg(test)]
    dst: [usize; 2],
    dst_rest: [Vec3; 2],
    /// The target's nodes from a root down to the toes (or the foot).
    path: Vec<Link>,
    /// Places in `path` of the upper leg, lower leg, foot and toes, and of
    /// the hips when the leg hangs below them.
    at: [usize; 4],
    hips_at: Option<usize>,
}

/// A node on a leg's path, and where its local transform comes from at a
/// moment.
struct Link {
    rest: Transform,
    source: Source,
}

#[derive(Clone, Copy)]
enum Source {
    /// No clip moves it.
    Rest,
    /// Its rotation is a shared bone's (an index into `Plan::bones`).
    Bone(usize),
    /// The hips: the bone's rotation and the clip's travel.
    Hips,
}

/// What `Feet::new` needs from the humanoid plan.
pub struct Setup<'a> {
    pub src: &'a Skeleton<'a>,
    pub dst: &'a Skeleton<'a>,
    /// Each shared bone's VRM name, source node and target node, in
    /// `Plan::bones` order.
    pub pairs: &'a [(String, usize, usize)],
    pub yaw: Quat,
    pub scale: f32,
    pub src_height: Option<f32>,
    pub dst_height: Option<f32>,
    /// The target's forward direction and its hips' world rotation in the
    /// reference pose.
    pub forward: Option<Vec3>,
    pub hips_rotation: Quat,
    /// The target posed like the source at rest (see `humanoid`).
    pub reference: &'a slerp_scene::Pose,
}

impl Feet {
    /// The legs both skeletons have, or None when neither leg is complete
    /// on both (upper leg, lower leg and foot) or the hip heights are
    /// unknown.
    pub fn new(s: Setup) -> Option<Feet> {
        let find = |bone: &str| s.pairs.iter().position(|(b, ..)| b == bone);
        let hips = find("hips")?;
        let unit = s.src_height?;
        let dst_height = s.dst_height?;
        if !(unit > 0.0 && dst_height > 0.0 && s.scale.is_finite() && s.scale > 0.0) {
            return None;
        }
        let mut legs = Vec::new();
        for side in ["left", "right"] {
            let bone = |part: &str| find(&format!("{side}{part}"));
            let (Some(upper), Some(lower), Some(foot)) = (bone("UpperLeg"), bone("LowerLeg"), bone("Foot")) else {
                continue;
            };
            let toes = bone("Toes");
            let end = s.pairs[toes.unwrap_or(foot)].2;
            let path = path_to(s.dst, end);
            let place = |bone: usize| path.iter().position(|&n| n == s.pairs[bone].2);
            let (Some(pu), Some(pl), Some(pf)) = (place(upper), place(lower), place(foot)) else { continue };
            let pt = toes.and_then(place).unwrap_or(pf);
            // The chain must run hips-wards to toes in that order.
            if !(pu < pl && pl < pf && pf <= pt) {
                continue;
            }
            let hips_node = s.pairs[hips].2;
            let links = path
                .iter()
                .map(|&n| Link {
                    rest: s.dst.scene.nodes[n].transform,
                    source: if n == hips_node {
                        Source::Hips
                    } else {
                        s.pairs.iter().position(|p| p.2 == n).map_or(Source::Rest, Source::Bone)
                    },
                })
                .collect();
            let src_ankle = s.pairs[foot].1;
            let src_toes = toes.map_or(src_ankle, |t| s.pairs[t].1);
            let dst_toes = toes.map_or(s.pairs[foot].2, |t| s.pairs[t].2);
            legs.push(Leg {
                upper,
                lower,
                foot,
                src: [src_ankle, src_toes],
                toes: toes.is_some(),
                src_rest: [s.yaw * s.src.rest_position(src_ankle), s.yaw * s.src.rest_position(src_toes)],
                #[cfg(test)]
                dst: [s.pairs[foot].2, dst_toes],
                dst_rest: [s.pairs[foot].2, dst_toes].map(|n| {
                    let across = position(&s.reference.world[n]);
                    Vec3::new(across.x, s.dst.rest_position(n).y, across.z)
                }),
                at: [pu, pl, pf, pt],
                hips_at: path.iter().position(|&n| n == hips_node).filter(|&h| h < pu),
                path: links,
            });
        }
        if legs.is_empty() {
            return None;
        }
        let forward = s.forward.unwrap_or(Vec3::Z);
        Some(Feet {
            legs,
            hips,
            unit,
            scale: s.scale,
            max_drop: MAX_DROP * dst_height,
            forward: (s.hips_rotation.inverse() * forward).normalize_or(Vec3::Z),
        })
    }

    /// Records the watched source joints' world positions, turned to face
    /// the target's way, from the source's posed world matrices.
    pub fn watch(&self, world: &[Mat4], yaw: Quat, out: &mut Vec<Vec3>) {
        for leg in &self.legs {
            for &n in &leg.src {
                out.push(world.get(n).map_or(Vec3::ZERO, |m| yaw * position(m)));
            }
        }
    }
}

/// The nodes from a root down to `node`, root first, guarding against
/// cycles.
fn path_to(skel: &Skeleton, node: usize) -> Vec<usize> {
    let mut path = vec![node];
    let mut n = node;
    while let Some(p) = skel.parents[n] {
        if path.contains(&p) || path.len() > skel.parents.len() {
            break;
        }
        path.push(p);
        n = p;
    }
    path.reverse();
    path
}

//endregion

/*┌─────────────────╮
| Applying        |
└─────────────────╯*/
//region Applying

/// A leg at one moment, posed by the clip as baked so far.
#[derive(Clone, Copy, Default)]
struct Posed {
    /// The hip socket, knee, ankle and toes.
    joints: [Vec3; 4],
    /// The world rotations of the upper leg, lower leg and foot.
    turns: [Quat; 3],
    /// The world rotations of the nodes those three hang from.
    parents: [Quat; 3],
    /// The hips joint's world position and the inverse of its parent's
    /// world matrix, when the leg hangs below the hips.
    hips: Option<(Vec3, Mat4)>,
    /// The hips' world rotation.
    hips_turn: Quat,
}

impl Feet {
    /// Applies foot IK to a baked clip: `rotations` holds each shared bone's
    /// local rotation keys and `travel` the hips' local translation keys at
    /// `times`, and `watched` the source joints `watch` recorded for each
    /// moment.  Rewrites the legs' rotations and the hips' height; returns
    /// whether anything changed.
    pub fn apply(&self, times: &[f32], watched: &[Vec3], rotations: &mut [Vec<f32>], travel: &mut [f32]) -> bool {
        let n = times.len();
        let per = self.legs.len() * 2;
        if n < 3
            || watched.len() != n * per
            || travel.len() != n * 3
            || rotations.iter().any(|r| r.len() != n * 4)
            || !times.iter().all(|t| t.is_finite())
        {
            return false;
        }
        // 1. The legs as the transfer posed them.
        let posed: Vec<Vec<Posed>> =
            self.legs.iter().map(|leg| (0..n).map(|k| self.pose(leg, k, rotations, travel)).collect()).collect();
        let src = |leg: usize, joint: usize, k: usize| watched[k * per + leg * 2 + joint];

        // 2. The ground's speed under the source, then each joint's
        //    contacts.
        let tracks: Vec<[Vec<Vec3>; 2]> =
            (0..self.legs.len()).map(|l| [0, 1].map(|j| (0..n).map(|k| src(l, j, k)).collect())).collect();
        let joints: Vec<&[Vec3]> = self
            .legs
            .iter()
            .zip(&tracks)
            .flat_map(|(leg, t)| t[..if leg.toes { 2 } else { 1 }].iter().map(|v| v.as_slice()))
            .collect();
        let rests: Vec<f32> =
            self.legs.iter().flat_map(|leg| leg.src_rest[..if leg.toes { 2 } else { 1 }].iter().map(|p| p.y)).collect();
        let ground = ground_velocity(times, &joints, &rests, self.unit);
        let in_place = ground != Vec3::ZERO;

        // 3. Each leg's goal for the ankle and how strongly it applies, at
        //    every moment, and the floor under its ankle and toes: the
        //    lowest the scaled source puts them.
        let mut goals: Vec<Vec<(Vec3, f32)>> = Vec::with_capacity(self.legs.len());
        let mut floors: Vec<[f32; 2]> = Vec::with_capacity(self.legs.len());
        for (l, leg) in self.legs.iter().enumerate() {
            let watch = Watch::new(times, &tracks[l], leg.toes, ground, self.unit);
            let raw = |j: usize, k: usize| leg.dst_rest[j] + (src(l, j, k) - leg.src_rest[j]) * self.scale;
            goals.push(plan_goals(times, &watch, &posed[l], raw, in_place, self.unit * self.scale));
            floors.push([0, 1].map(|j| (0..n).map(|k| raw(j, k).y).fold(f32::MAX, f32::min)));
        }

        // 4. How far the hips come down so pinned feet are in reach.
        let mut drop = vec![0.0f32; n];
        for (l, leg) in self.legs.iter().enumerate() {
            if leg.hips_at.is_none() {
                continue;
            }
            for k in 0..n {
                let (goal, w) = goals[l][k];
                if w == 0.0 {
                    continue;
                }
                let p = &posed[l][k];
                let target = p.joints[2] + (goal - p.joints[2]) * w;
                let length = (p.joints[1] - p.joints[0]).length() + (p.joints[2] - p.joints[1]).length();
                drop[k] = drop[k].max(needed_drop(p.joints[0], target, REACH * length));
            }
        }
        for d in &mut drop {
            *d = d.min(self.max_drop);
        }
        let drop = smooth_drop(times, &drop);

        // 5. The hips' new height, then each leg solved.
        if let Some(hips) = self.legs.iter().zip(&posed).find_map(|(l, p)| l.hips_at.map(|_| p)) {
            for k in 0..n {
                if drop[k] <= 0.0 {
                    continue;
                }
                if let Some((joint, to_local)) = hips[k].hips {
                    let local = to_local.transform_point3(joint - Vec3::Y * drop[k]);
                    if local.is_finite() {
                        travel[k * 3..k * 3 + 3].copy_from_slice(&local.to_array());
                    }
                }
            }
        }
        let mut changed = false;
        for (l, leg) in self.legs.iter().enumerate() {
            let lowered = leg.hips_at.is_some();
            for k in 0..n {
                let (goal, w) = goals[l][k];
                let mut p = posed[l][k];
                if lowered {
                    for j in &mut p.joints {
                        *j -= Vec3::Y * drop[k];
                    }
                }
                // The free foot comes down with the hips; a pinned one
                // stays, and a fading one is in between, which matches the
                // pin at the contact's edge and the free foot where the
                // weight reaches nothing.  Then no joint goes through the
                // floor: not a swinging foot the hips brought down, nor one
                // the transfer put there.
                let free = p.joints[2];
                let mut target = free + (goal - free) * w;
                let toes = target.y + p.joints[3].y - p.joints[2].y;
                let lift = (floors[l][0] - target.y).max(if leg.toes { floors[l][1] - toes } else { 0.0 });
                if lift > 1e-5 * self.unit {
                    target.y += lift;
                } else if w == 0.0 {
                    continue;
                }
                changed = true;
                let pole = p.hips_turn * self.forward;
                let [hip, knee, ankle, _] = p.joints;
                let (upper, lower) = two_bone(hip, knee, ankle, target, pole, p.turns[0], p.turns[1]);
                let lower_parent = (upper * p.turns[0].inverse()) * p.parents[1];
                let foot_parent = (lower * p.turns[1].inverse()) * p.parents[2];
                let locals = [
                    (leg.upper, p.parents[0].inverse() * upper),
                    (leg.lower, lower_parent.inverse() * lower),
                    (leg.foot, foot_parent.inverse() * p.turns[2]),
                ];
                for (bone, q) in locals {
                    let q = q.normalize();
                    if q.is_finite() {
                        rotations[bone][k * 4..k * 4 + 4].copy_from_slice(&q.to_array());
                    }
                }
            }
            for bone in [leg.upper, leg.lower, leg.foot] {
                same_hemisphere(&mut rotations[bone]);
            }
        }
        changed || drop.iter().any(|&d| d > 0.0)
    }

    /// Poses one leg at moment `k` from the baked keys.
    fn pose(&self, leg: &Leg, k: usize, rotations: &[Vec<f32>], travel: &[f32]) -> Posed {
        let key = |bone: usize| Quat::from_slice(&rotations[bone][k * 4..k * 4 + 4]);
        let mut world = Mat4::IDENTITY;
        let mut mats = Vec::with_capacity(leg.path.len());
        for link in &leg.path {
            let mut t = link.rest;
            match link.source {
                Source::Rest => {}
                Source::Bone(b) => t.rotation = key(b),
                Source::Hips => {
                    t.rotation = key(self.hips);
                    t.translation = Vec3::from_slice(&travel[k * 3..k * 3 + 3]);
                }
            }
            world *= t.matrix();
            mats.push(world);
        }
        let at = |i: usize| mats[i];
        let parent = |i: usize| if i == 0 { Quat::IDENTITY } else { rotation(&mats[i - 1]) };
        let [pu, pl, pf, pt] = leg.at;
        let hips = leg.hips_at.map(|h| {
            let up = if h == 0 { Mat4::IDENTITY } else { mats[h - 1] };
            (position(&at(h)), up.inverse())
        });
        Posed {
            joints: [pu, pl, pf, pt].map(|i| position(&at(i))),
            turns: [pu, pl, pf].map(|i| rotation(&at(i))),
            parents: [pu, pl, pf].map(parent),
            hips: hips.filter(|(p, m)| p.is_finite() && m.is_finite()),
            hips_turn: leg.hips_at.map_or(Quat::IDENTITY, |h| rotation(&at(h))),
        }
    }
}

/// Keeps neighbouring rotation keys on the same side of the sphere, so
/// interpolation takes the short way.
fn same_hemisphere(keys: &mut [f32]) {
    for k in 1..keys.len() / 4 {
        let (before, after) = keys.split_at_mut(k * 4);
        let prev = Quat::from_slice(&before[(k - 1) * 4..]);
        let q = Quat::from_slice(&after[..4]);
        if prev.dot(q) < 0.0 {
            after[..4].copy_from_slice(&(-q).to_array());
        }
    }
}

//endregion

/*┌─────────────────╮
| Contacts        |
└─────────────────╯*/
//region Contacts

/// One leg's source joints over a clip: per joint (ankle, toes), whether
/// it is planted, and whether it is down at the bottom of that contact.
pub struct Watch {
    pub planted: [Vec<bool>; 2],
    pub down: [Vec<bool>; 2],
}

impl Watch {
    /// Finds the contacts of a leg's ankle and toes (`track`, positions per
    /// moment), with the ground moving at `ground` and lengths in `unit`s.
    /// Without toes, only the ankle plants.
    pub fn new(times: &[f32], track: &[Vec<Vec3>; 2], toes: bool, ground: Vec3, unit: f32) -> Watch {
        let slack = SPEED_SHARE * ground.length() / unit;
        let planted = [0, 1].map(|j| {
            if j == 1 && !toes {
                vec![false; times.len()]
            } else {
                contacts(times, &track[j], &speeds(times, &track[j], ground), unit, slack)
            }
        });
        let down = [0, 1].map(|j| bottoms(&track[j], &planted[j], unit));
        Watch { planted, down }
    }
}

/// Each moment's speed of a joint across the ground (which moves at
/// `ground`), from its neighbours on either side.
fn speeds(times: &[f32], track: &[Vec3], ground: Vec3) -> Vec<f32> {
    let n = times.len().min(track.len());
    let at = |k: usize| track[k] - ground * times[k];
    (0..n)
        .map(|k| {
            let (a, b) = (k.saturating_sub(1), (k + 1).min(n - 1));
            let dt = times[b] - times[a];
            let d = at(b) - at(a);
            let v = Vec3::new(d.x, 0.0, d.z).length() / dt;
            if dt > 1e-6 && v.is_finite() { v } else { 0.0 }
        })
        .collect()
}

/// The moments a planted joint is down: within `LIFT` of the lowest it
/// gets in that contact (the height a quarter of the contact is at or
/// below, so a one-frame dip doesn't decide).
pub fn bottoms(track: &[Vec3], planted: &[bool], unit: f32) -> Vec<bool> {
    let mut down = vec![false; planted.len()];
    for (start, end) in runs(planted) {
        let low = quantile(track[start..=end].iter().map(|p| p.y), 0.25);
        for k in start..=end {
            down[k] = track[k].y - low < LIFT * unit;
        }
    }
    down
}

/// The frames where a joint is planted: near its lowest height in the clip
/// (`LOW`) and nearly still against the ground (with `slack` more speed allowed,
/// in units a second), with hysteresis, short contacts dropped and short
/// gaps joined.
pub fn contacts(times: &[f32], track: &[Vec3], speed: &[f32], unit: f32, slack: f32) -> Vec<bool> {
    let n = times.len().min(track.len()).min(speed.len());
    let lowest = quantile(track[..n].iter().map(|p| p.y), LOW);
    let height = |k: usize| track[k].y - lowest;
    let mut planted = vec![false; n];
    let mut on = false;
    for k in 0..n {
        let (h, v) = (height(k) / unit, speed[k] / unit);
        on = if on { h < EXIT_HEIGHT && v < EXIT_SPEED + slack } else { h < ENTER_HEIGHT && v < ENTER_SPEED + slack };
        planted[k] = on;
    }
    // Short gaps where the joint stays low are one contact.
    for (start, end) in runs(&planted.iter().map(|p| !p).collect::<Vec<_>>()) {
        if start > 0
            && end + 1 < n
            && times[end + 1] - times[start - 1] <= MIN_GAP + 1e-4
            && (start..=end).all(|k| height(k) / unit < EXIT_HEIGHT)
        {
            planted[start..=end].fill(true);
        }
    }
    // Contacts too short to be steps are noise.  Each frame counts for the
    // time halfway to its neighbours, and a contact at either end of the
    // clip may be cut short by it, so it only needs two frames.
    let half = |a: usize, b: usize| (times[b] - times[a]).max(0.0) / 2.0;
    for (start, end) in runs(&planted) {
        let edge = start == 0 || end + 1 == n;
        let span = times[end] - times[start] + half(start.saturating_sub(1), start) + half(end, (end + 1).min(n - 1));
        if !(span + 1e-4 >= MIN_CONTACT || edge && end > start) {
            planted[start..=end].fill(false);
        }
    }
    planted
}

/// The value a `share` of the finite `values` are at or below, or
/// `f32::MAX` when there are none: a lowest point that a frame or two
/// dipping through the floor doesn't decide.
fn quantile(values: impl Iterator<Item = f32>, share: f32) -> f32 {
    let mut v: Vec<f32> = values.filter(|y| y.is_finite()).collect();
    v.sort_by(f32::total_cmp);
    let at = ((v.len().saturating_sub(1)) as f32 * share) as usize;
    v.get(at).copied().unwrap_or(f32::MAX)
}

/// The stretches of `true`, as first and last index.
fn runs(flags: &[bool]) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut start = None;
    for (k, &f) in flags.iter().enumerate() {
        match (f, start) {
            (true, None) => start = Some(k),
            (false, Some(s)) => {
                out.push((s, k - 1));
                start = None;
            }
            _ => {}
        }
    }
    if let Some(s) = start {
        out.push((s, flags.len() - 1));
    }
    out
}

/// How fast the ground moves under the source, flat: the median velocity
/// of the joint on the ground (an ankle, or toes for a runner up on them)
/// over the moments one is.  Zero for clips with root motion, where
/// planted feet stand still; the walking speed backwards for clips made in
/// place, where they slide back.  `joints` are every ankle and toe track
/// and `rest` their heights at rest, standing on the ground: the joint
/// nearest its rest height is the one on the ground.  For a clip whose
/// ground is elsewhere, each joint's lowest stands in for its rest height.
pub fn ground_velocity(times: &[f32], joints: &[&[Vec3]], rest: &[f32], unit: f32) -> Vec3 {
    let n = times.len();
    if n < 3 || joints.is_empty() || joints.iter().any(|a| a.len() < n) || rest.len() != joints.len() {
        return Vec3::ZERO;
    }
    let near = |base: &[f32]| {
        (0..n).filter(|&k| (0..joints.len()).any(|j| joints[j][k].y - base[j] < ENTER_HEIGHT * unit)).count()
    };
    let lowest: Vec<f32> = joints.iter().map(|a| quantile(a[..n].iter().map(|p| p.y), LOW)).collect();
    let lowest = if near(rest) * 10 >= n { rest.to_vec() } else { lowest };
    let (mut xs, mut zs) = (Vec::new(), Vec::new());
    for k in 0..n {
        let Some((joint, h)) =
            joints.iter().enumerate().map(|(j, a)| (j, a[k].y - lowest[j])).min_by(|a, b| a.1.total_cmp(&b.1))
        else {
            continue;
        };
        // The joint must be on the ground on both sides too, or the speed
        // straddles a landing or lift-off: a sprint's stance is five
        // frames, two of them edges.
        let (a, b) = (k.saturating_sub(1), (k + 1).min(n - 1));
        let grounded = |h: f32| h < ENTER_HEIGHT * unit;
        if !grounded(h) || [a, b].iter().any(|&i| !grounded(joints[joint][i].y - lowest[joint])) {
            continue;
        }
        let dt = times[b] - times[a];
        let v = (joints[joint][b] - joints[joint][a]) / dt;
        if dt > 1e-6 && v.is_finite() {
            xs.push(v.x);
            zs.push(v.z);
        }
    }
    let median = |v: &mut Vec<f32>| {
        v.sort_by(f32::total_cmp);
        v.get(v.len() / 2).copied().unwrap_or(0.0)
    };
    let g = Vec3::new(median(&mut xs), 0.0, median(&mut zs));
    if g.length() < IN_PLACE * unit { Vec3::ZERO } else { g }
}

//endregion

/*┌─────────────────╮
| Goals           |
└─────────────────╯*/
//region Goals

/// Which joint a planted foot is pinned by.
#[derive(Clone, Copy, PartialEq)]
enum Pin {
    Ankle,
    Toes,
}

/// The ankle's goal at each moment and the weight it applies with: 1 while
/// held, fading to 0 within `BLEND` seconds of a contact.  `raw(j, k)` is
/// where the scaled source puts joint `j` (ankle, toes) at moment `k`, and
/// `unit` is the target's hip height.
///
/// A foot is held from when either joint plants until the last moment one
/// is down, so it lets go as the source's toes lift rather than when the
/// contact's looser test gives up.  Before the pinned joint first comes
/// down (a heel about to strike), the goal follows the source's height to
/// the pin.  A pin stands still, unless the clip was made `in_place`:
/// then the ground slides back under it, at a speed that changes over the
/// stride, and the pin moves across with the scaled source's joint.
fn plan_goals(
    times: &[f32],
    watch: &Watch,
    posed: &[Posed],
    raw: impl Fn(usize, usize) -> Vec3,
    in_place: bool,
    unit: f32,
) -> Vec<(Vec3, f32)> {
    let n = times.len().min(posed.len());
    // 1. The moments a pin holds: each joint's contacts, up to the last
    //    moment it is down.
    let mut held = vec![false; n];
    for j in 0..2 {
        for (start, end) in runs(&watch.planted[j]) {
            if let Some(last) = (start..=end).rev().find(|&k| watch.down[j][k]) {
                held[start..=last].fill(true);
            }
        }
    }
    // From the ankle to the toes as the transfer turns the foot, which IK
    // keeps.
    let reach = |k: usize| posed[k].joints[3] - posed[k].joints[2];
    let flat = |v: Vec3| Vec3::new(v.x, 0.0, v.z);
    let mut goal = vec![(Vec3::ZERO, 0.0f32); n];

    // 2. Pins for each stretch a foot is held.
    let lowest = |j: usize| (0..n).map(|k| raw(j, k).y).fold(f32::MAX, f32::min);
    let lowest = [lowest(0), lowest(1)];
    for (start, end) in runs(&held) {
        let Some(first) = (start..=end).find(|&k| watch.down[0][k] || watch.down[1][k]) else { continue };
        // Each joint's floor: its height while down in this contact, or
        // its lowest in the clip when it never comes down here.
        let floor = [0, 1].map(|j| {
            let down: Vec<usize> = (start..=end).filter(|&k| watch.down[j][k]).collect();
            if down.is_empty() { lowest[j] } else { down.iter().map(|&k| raw(j, k).y).sum::<f32>() / down.len() as f32 }
        });
        // The first joint down, pinned where it is over its first stretch
        // down.  A pin is a place and, for clips made in place, the offset
        // across from the source's joint it moves with.
        let joint = if watch.down[0][first] { 0 } else { 1 };
        let stretch: Vec<usize> = (first..=end).take_while(|&k| watch.down[joint][k]).collect();
        let place = stretch.iter().map(|&k| raw(joint, k)).sum::<Vec3>() / stretch.len() as f32;
        let offset = stretch.iter().map(|&k| flat(place - raw(joint, k))).sum::<Vec3>() / stretch.len() as f32;
        let (mut pin, mut place, mut offset) = (if joint == 0 { Pin::Ankle } else { Pin::Toes }, place, offset);
        // Where the pinned joint's source was when the pin was set: how far
        // it has moved since is how far the source's foot has slid off.
        let mut set_at = flat(place - offset);
        let mut heel_was_down = false;
        for (k, slot) in goal.iter_mut().enumerate().take(end + 1).skip(start) {
            let (ankle, toes) = ((watch.planted[0][k], watch.down[0][k]), (watch.planted[1][k], watch.down[1][k]));
            let j = if pin == Pin::Ankle { 0 } else { 1 };
            // The pinned joint now, then the ankle where that puts it, and
            // how far the ankle must rise to keep the other joint off its
            // floor: the toes as the foot pitches down about a pinned heel,
            // the heel as it pitches up about pinned toes.
            let pinned = |j: usize, place: Vec3, offset: Vec3| {
                if in_place { flat(raw(j, k)) + offset + Vec3::Y * place.y } else { place }
            };
            let ankle_at = |pin: Pin, p: Vec3| if pin == Pin::Ankle { p } else { p - reach(k) };
            let rise = |a: Vec3, j: usize| (floor[j] - if j == 0 { a.y } else { a.y + reach(k).y }).max(0.0);
            // Hand the pin over where the foot is now, so it doesn't jump:
            // to the toes when the heel lifts or the toes start holding it
            // up (the foot rolls over them), back to the ankle when the toes
            // lift with the heel down or the heel holds them up.
            let roll = 0.25 * LIFT * unit;
            let a = ankle_at(pin, pinned(j, place, offset));
            let to_toes = pin == Pin::Ankle && toes.0 && ((heel_was_down && !ankle.1) || (toes.1 && rise(a, 1) > roll));
            let to_ankle = pin == Pin::Toes && ankle.0 && ((ankle.1 && !toes.1) || rise(a, 0) > roll);
            if to_toes || to_ankle {
                let (next, j) = if to_toes { (Pin::Toes, 1) } else { (Pin::Ankle, 0) };
                place = a + Vec3::Y * rise(a, j) + if to_toes { reach(k) } else { Vec3::ZERO };
                offset = flat(place - raw(j, k));
                set_at = flat(raw(j, k));
                pin = next;
            }
            heel_was_down |= pin == Pin::Ankle && ankle.1;
            let j = if pin == Pin::Ankle { 0 } else { 1 };
            // A still pin lets go once the source's joint slides off it.
            if !in_place && k > first && (flat(raw(j, k)) - set_at).length() > RELEASE * unit {
                break;
            }
            let mut a = ankle_at(pin, pinned(j, place, offset));
            // Before the pinned joint first comes down, the foot follows the
            // source's height down to the pin.
            if k < first {
                let above = raw(joint, k).y - (if joint == 0 { a.y } else { a.y + reach(k).y });
                a.y += above.max(0.0);
            }
            a.y += rise(a, 0).max(if toes.0 || toes.1 { rise(a, 1) } else { 0.0 });
            *slot = (a, 1.0);
        }
    }

    // 3. Fading in and out: the correction at a contact's edge carried
    //    along the free foot's path with a falling weight.  Pins that let
    //    go early leave the rest of their contact to fade.
    let held: Vec<bool> = goal.iter().map(|g| g.1 > 0.0).collect();
    let intervals = runs(&held);
    let correction = |k: usize| goal[k].0 - posed[k].joints[2];
    let mut fades = vec![(Vec3::ZERO, 0.0f32); n];
    for k in (0..n).filter(|&k| !held[k]) {
        let before = intervals.iter().rev().find(|&&(_, e)| e < k).map(|&(_, e)| e);
        let after = intervals.iter().find(|&&(s, _)| s > k).map(|&(s, _)| s);
        let weight = |edge: usize| ease(1.0 - (times[k] - times[edge]).abs() / BLEND);
        let (wb, wa) = (before.map_or(0.0, weight), after.map_or(0.0, weight));
        let w = wb.max(wa);
        if w <= 0.0 {
            continue;
        }
        let fix = match (before, after) {
            (Some(b), Some(a)) if wb > 0.0 && wa > 0.0 => {
                let f = ease((times[k] - times[b]) / (times[a] - times[b]).max(1e-6));
                correction(b).lerp(correction(a), f)
            }
            (Some(b), _) if wb > 0.0 => correction(b),
            (_, Some(a)) => correction(a),
            _ => continue,
        };
        fades[k] = (posed[k].joints[2] + fix, w);
    }
    for k in (0..n).filter(|&k| !held[k]) {
        goal[k] = fades[k];
    }
    // Non-finite input (a broken clip) leaves the leg alone.
    for g in &mut goal {
        if !g.0.is_finite() {
            *g = (Vec3::ZERO, 0.0);
        }
    }
    goal
}

/// Smoothstep from 0 to 1 over `x` in [0, 1].
fn ease(x: f32) -> f32 {
    let x = x.clamp(0.0, 1.0);
    x * x * (3.0 - 2.0 * x)
}

/// How far a hip socket at `hip` must come straight down for a leg of
/// length `reach` to touch `goal`: 0 when it already does.
pub fn needed_drop(hip: Vec3, goal: Vec3, reach: f32) -> f32 {
    let d = hip - goal;
    if !d.is_finite() || d.length() <= reach || d.y <= 0.0 {
        return 0.0;
    }
    let across = d.x * d.x + d.z * d.z;
    let drop = if across < reach * reach { d.y - (reach * reach - across).sqrt() } else { d.y };
    drop.max(0.0)
}

/// The hips' drop smoothed so it never falls short of what each moment
/// needs: the largest within `SMOOTH` seconds, then averaged over the same
/// window.
fn smooth_drop(times: &[f32], drop: &[f32]) -> Vec<f32> {
    let n = drop.len();
    let window = |k: usize| {
        let lo = (0..=k).rev().take_while(|&i| times[k] - times[i] <= SMOOTH).last().unwrap_or(k);
        let hi = (k..n).take_while(|&i| times[i] - times[k] <= SMOOTH).last().unwrap_or(k);
        lo..=hi
    };
    let widest: Vec<f32> = (0..n).map(|k| window(k).map(|i| drop[i]).fold(0.0, f32::max)).collect();
    (0..n)
        .map(|k| {
            let w = window(k);
            let count = w.clone().count() as f32;
            w.map(|i| widest[i]).sum::<f32>() / count
        })
        .collect()
}

//endregion

/*┌─────────────────╮
| Solver          |
└─────────────────╯*/
//region Solver

/// Two-bone IK for a leg: turns the thigh and shin (world rotations
/// `upper` and `lower`, joints at `hip`, `knee` and `ankle`) so the ankle
/// reaches `goal`, or the nearest point the leg's length allows.  Returns
/// their new world rotations.
///
/// The leg's plane swings by the smallest turn that takes the line from
/// hip to ankle onto the line to the goal, which keeps the knee's hinge
/// and twist; within the plane the knee bends to the angle the distance
/// needs, towards the side it bends to now, or towards `pole` when the leg
/// is straight.  Bad input (zero-length bones, non-finite positions)
/// returns the rotations unchanged.
pub fn two_bone(hip: Vec3, knee: Vec3, ankle: Vec3, goal: Vec3, pole: Vec3, upper: Quat, lower: Quat) -> (Quat, Quat) {
    let (a, b) = ((knee - hip).length(), (ankle - knee).length());
    if !(a > 1e-9 && b > 1e-9 && [hip, knee, ankle, goal, pole].iter().all(|v| v.is_finite())) {
        return (upper, lower);
    }
    // 1. The line from the hip to the ankle now and to the goal.
    let thigh = (knee - hip) / a;
    let x0 = (ankle - hip).try_normalize().unwrap_or(thigh);
    let x1 = (goal - hip).try_normalize().unwrap_or(x0);
    let eps = 1e-4 * (a + b);
    let d = (goal - hip).length().clamp((a - b).abs() + eps, a + b - eps);
    // 2. The side the knee bends to: as now, nudged towards the pole so a
    //    straight leg bends forward.
    let across = |v: Vec3, x: Vec3| v - x * v.dot(x);
    let y0 = (across(knee - hip, x0) + across(pole, x0).normalize_or_zero() * (0.02 * a))
        .try_normalize()
        .unwrap_or_else(|| x0.any_orthonormal_vector());
    // 3. Swing the plane, then bend within it.
    let swing = Quat::from_rotation_arc(x0, x1);
    let y1 = across(swing * y0, x1).try_normalize().unwrap_or_else(|| x1.any_orthonormal_vector());
    let cos = ((a * a + d * d - b * b) / (2.0 * a * d)).clamp(-1.0, 1.0);
    let sin = (1.0 - cos * cos).max(0.0).sqrt();
    let knee1 = hip + (x1 * cos + y1 * sin) * a;
    let ankle1 = hip + x1 * d;
    let thigh_turn = Quat::from_rotation_arc((swing * thigh).normalize(), (knee1 - hip) / a) * swing;
    let shin0 = (thigh_turn * (ankle - knee)).normalize();
    let shin1 = (ankle1 - knee1).normalize();
    let upper1 = (thigh_turn * upper).normalize();
    let lower1 = (Quat::from_rotation_arc(shin0, shin1) * thigh_turn * lower).normalize();
    if upper1.is_finite() && lower1.is_finite() { (upper1, lower1) } else { (upper, lower) }
}

//endregion

#[cfg(test)]
mod tests;
