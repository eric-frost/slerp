//! Foot IK: the solver and contact detection on made-up data whose answers
//! are known, and measurements of foot slide and floor penetration before
//! and after on real characters and clips.

use super::*;
use crate::humanoid::{Method, Plan};
use slerp_scene::{Asset, Limits, Pose, Scene};
use std::f32::consts::PI;
use std::path::{Path, PathBuf};

/*┌─────────────────╮
| Solver          |
└─────────────────╯*/
//region Solver

/// A leg hanging from (0, 1, 0) with its knee bent forward (+Z), and the
/// world rotations of its bones (identity at rest).
fn bent_leg() -> (Vec3, Vec3, Vec3) {
    (Vec3::new(0.0, 1.0, 0.0), Vec3::new(0.0, 0.55, 0.1), Vec3::new(0.0, 0.08, 0.0))
}

/// Where the knee and ankle go when the bones turn by these world
/// rotations away from `upper0` and `lower0`.
fn moved(
    (hip, knee, ankle): (Vec3, Vec3, Vec3),
    (upper0, lower0): (Quat, Quat),
    (upper1, lower1): (Quat, Quat),
) -> (Vec3, Vec3) {
    let knee1 = hip + upper1 * upper0.inverse() * (knee - hip);
    let ankle1 = knee1 + lower1 * lower0.inverse() * (ankle - knee);
    (knee1, ankle1)
}

#[test]
fn solver_reaches() {
    let leg @ (hip, knee, ankle) = bent_leg();
    let (a, b) = ((knee - hip).length(), (ankle - knee).length());
    let rest = (Quat::from_rotation_x(0.3), Quat::from_rotation_y(-0.2));
    for goal in
        [Vec3::new(0.0, 0.2, 0.1), Vec3::new(0.15, 0.1, -0.2), Vec3::new(-0.1, 0.4, 0.3), Vec3::new(0.0, 0.08, 0.0)]
    {
        let turned = two_bone(hip, knee, ankle, goal, Vec3::Z, rest.0, rest.1);
        let (k, an) = moved(leg, rest, turned);
        assert!(an.distance(goal) < 1e-4, "{goal}: ankle at {an}");
        // Bones keep their lengths, and the knee still bends forward.
        assert!(((k - hip).length() - a).abs() < 1e-4 && ((an - k).length() - b).abs() < 1e-4);
        let line = (an - hip).normalize();
        let off = (k - hip) - line * (k - hip).dot(line);
        assert!(off.z > 0.0, "{goal}: knee went back to {k}");
    }
    // Reaching where the ankle already is changes nothing.
    let same = two_bone(hip, knee, ankle, ankle, Vec3::Z, rest.0, rest.1);
    assert!(same.0.angle_between(rest.0) < 1e-3 && same.1.angle_between(rest.1) < 1e-3);
}

#[test]
fn solver_out_of_reach() {
    let leg @ (hip, knee, ankle) = bent_leg();
    let length = (knee - hip).length() + (ankle - knee).length();
    let rest = (Quat::IDENTITY, Quat::IDENTITY);
    let goal = Vec3::new(0.3, -1.0, 0.2);
    let turned = two_bone(hip, knee, ankle, goal, Vec3::Z, rest.0, rest.1);
    let (_, an) = moved(leg, rest, turned);
    // As near as the straightened leg gets, on the line to the goal.
    assert!((an.distance(hip) - length).abs() < 1e-3 * length, "{an}");
    assert!((an - hip).normalize().angle_between((goal - hip).normalize()) < 1e-3);
}

#[test]
fn solver_bends_towards_pole() {
    // A straight leg asked to shorten bends its knee towards the pole.
    let (hip, knee, ankle) = (Vec3::new(0.0, 1.0, 0.0), Vec3::new(0.0, 0.5, 0.0), Vec3::ZERO);
    for pole in [Vec3::Z, -Vec3::Z, Vec3::X] {
        let turned = two_bone(hip, knee, ankle, Vec3::new(0.0, 0.3, 0.0), pole, Quat::IDENTITY, Quat::IDENTITY);
        let (k, an) = moved((hip, knee, ankle), (Quat::IDENTITY, Quat::IDENTITY), turned);
        assert!(an.distance(Vec3::new(0.0, 0.3, 0.0)) < 1e-4);
        assert!((k - hip).normalize().dot(pole) > 0.5, "pole {pole}: knee at {k}");
    }
    // A bent knee keeps bending its own way, whatever the pole says.
    let leg @ (hip, knee, ankle) = bent_leg();
    let turned = two_bone(hip, knee, ankle, Vec3::new(0.0, 0.3, 0.0), -Vec3::Z, Quat::IDENTITY, Quat::IDENTITY);
    let (k, _) = moved(leg, (Quat::IDENTITY, Quat::IDENTITY), turned);
    assert!(k.z > 0.0, "{k}");
}

#[test]
fn solver_never_nan() {
    let (hip, knee, ankle) = (Vec3::new(0.0, 1.0, 0.0), Vec3::new(0.0, 0.5, 0.0), Vec3::ZERO);
    let id = Quat::IDENTITY;
    let goals = [
        ankle,                       // already at full extension
        Vec3::new(0.0, -1.0, 0.0),   // straight on, beyond reach
        Vec3::new(0.0, 0.0, 0.0001), // a hair off the straight line
        hip,                         // at the hip itself
        Vec3::new(0.0, 2.0, 0.0),    // straight up, folded back on itself
        Vec3::new(1e20, 0.0, 0.0),
    ];
    for goal in goals {
        for pole in [Vec3::Z, Vec3::ZERO, -Vec3::Y] {
            let (u, l) = two_bone(hip, knee, ankle, goal, pole, id, id);
            assert!(u.is_finite() && l.is_finite() && u.is_normalized() && l.is_normalized(), "{goal} {pole}");
        }
    }
    // Degenerate legs and non-finite input leave the rotations as they were.
    let q = Quat::from_rotation_x(0.4);
    assert_eq!(two_bone(hip, hip, ankle, ankle, Vec3::Z, q, q), (q, q));
    assert_eq!(two_bone(hip, knee, ankle, Vec3::NAN, Vec3::Z, q, q), (q, q));
    assert_eq!(two_bone(hip, knee, knee, ankle, Vec3::Z, q, q), (q, q));
}

#[test]
fn hips_drop() {
    let hip = Vec3::new(0.0, 1.0, 0.0);
    assert_eq!(needed_drop(hip, Vec3::new(0.0, 0.1, 0.0), 1.0), 0.0);
    // A goal 0.6 forward needs the hip at height 0.8 for a leg of 1.
    let d = needed_drop(hip, Vec3::new(0.0, 0.0, 0.6), 1.0);
    assert!((d - 0.2).abs() < 1e-5, "{d}");
    assert_eq!(needed_drop(hip, Vec3::new(0.0, 2.5, 0.0), 1.0), 0.0, "goals above never lower the hips");
    assert_eq!(needed_drop(hip, Vec3::NAN, 1.0), 0.0);
    // Smoothing never leaves a moment short of what it needs.
    let times: Vec<f32> = (0..30).map(|i| i as f32 / 30.0).collect();
    let mut raw = vec![0.0; 30];
    raw[10] = 0.05;
    raw[11] = 0.02;
    let smooth = smooth_drop(&times, &raw);
    assert!(smooth.iter().zip(&raw).all(|(s, r)| s + 1e-6 >= *r));
    assert!(smooth[6] > 0.0 && smooth[6] < 0.05 && smooth[25] == 0.0, "{smooth:?}");
}

//endregion

/*┌─────────────────╮
| Contacts        |
└─────────────────╯*/
//region Contacts

/// A foot walking at 1.2 m/s on legs whose hips are 1 m high: down for
/// 0.6 s, then swinging 1.2 m forward in a 12 cm arc for 0.4 s, from
/// `phase` seconds into the cycle, sampled at 30 frames a second.  With
/// `in_place`, the ground slides back under it instead (a treadmill).
/// Returns the times, the track and whether each moment is in stance.
fn walking_foot(seconds: f32, phase: f32, in_place: bool) -> (Vec<f32>, Vec<Vec3>, Vec<bool>) {
    let frames = (seconds * 30.0) as usize + 1;
    let times: Vec<f32> = (0..frames).map(|i| i as f32 / 30.0).collect();
    let (mut track, mut stance) = (Vec::new(), Vec::new());
    for &t in &times {
        let c = t + phase;
        let (cycle, f) = ((c / 1.0).floor(), c.rem_euclid(1.0));
        let (z, y, down) = if f < 0.6 {
            (cycle * 1.2, 0.08, true)
        } else {
            let s = (f - 0.6) / 0.4;
            let ease = s * s * (3.0 - 2.0 * s);
            (cycle * 1.2 + 1.2 * ease, 0.08 + 0.12 * (s * PI).sin(), false)
        };
        let z = if in_place { z - 1.2 * c } else { z };
        track.push(Vec3::new(0.1, y, z));
        stance.push(down);
    }
    (times, track, stance)
}

/// How many frames two flag lists disagree on, and how many runs of
/// `true` the first has.
fn compare(found: &[bool], truth: &[bool]) -> (usize, usize) {
    (found.iter().zip(truth).filter(|(a, b)| a != b).count(), runs(found).len())
}

#[test]
fn contacts_on_synthetic_walks() {
    for in_place in [false, true] {
        let (times, left, truth_l) = walking_foot(3.0, 0.0, in_place);
        let (_, right, truth_r) = walking_foot(3.0, 0.5, in_place);
        let ground = ground_velocity(&times, &[&left, &right], &[0.08, 0.08], 1.0);
        // The ground slides back at walking speed under a walk in place.
        let expect = if in_place { Vec3::new(0.0, 0.0, -1.2) } else { Vec3::ZERO };
        assert!(ground.distance(expect) < 0.01, "in place {in_place}: ground {ground}");
        for (track, truth) in [(&left, &truth_l), (&right, &truth_r)] {
            let watch = Watch::new(&times, &[track.clone(), track.clone()], false, ground, 1.0);
            let (wrong, steps) = compare(&watch.planted[0], truth);
            // One contact per step (3 or 4 in three seconds; a step cut to
            // one frame by the clip's end is none), wrong only in the frame
            // or two either side where the foot lifts slowly.
            let expect = runs(truth).iter().filter(|(a, b)| b > a).count();
            assert!(
                steps == expect,
                "in place {in_place}: {steps} contacts, not {expect}: {:?} vs {:?}",
                runs(&watch.planted[0]),
                runs(truth)
            );
            assert!(wrong <= 4 * steps, "in place {in_place}: {wrong} frames wrong over {steps} steps");
            assert!(watch.planted[1].iter().all(|p| !p), "no toes, no toe contacts");
        }
    }
}

#[test]
fn contacts_dont_flicker() {
    // A planted foot with tracking noise: 4 mm jitter at 30 Hz is 0.24 m/s
    // of speed noise, under the threshold to stay but over half of it.
    let times: Vec<f32> = (0..60).map(|i| i as f32 / 30.0).collect();
    let track: Vec<Vec3> =
        (0..60).map(|i| Vec3::new(if i % 2 == 0 { 0.004 } else { 0.0 }, 0.08 + 0.002 * (i % 3) as f32, 0.0)).collect();
    let speed = speeds(&times, &track, Vec3::ZERO);
    let planted = contacts(&times, &track, &speed, 1.0, 0.0);
    assert_eq!(runs(&planted), vec![(0, 59)]);
    // A brief dip of a swinging foot isn't a step.
    let track: Vec<Vec3> = (0..60).map(|i| Vec3::new(0.0, if i == 30 { 0.0 } else { 0.3 }, i as f32 * 0.04)).collect();
    let speed = speeds(&times, &track, Vec3::ZERO);
    assert!(contacts(&times, &track, &speed, 1.0, 0.0).iter().all(|p| !p));
    // Short gaps are joined: the foot lifts a hair for one frame.
    let mut track: Vec<Vec3> = (0..60).map(|_| Vec3::new(0.0, 0.08, 0.0)).collect();
    track[20].y += 0.04;
    let speed = speeds(&times, &track, Vec3::ZERO);
    assert_eq!(runs(&contacts(&times, &track, &speed, 1.0, 0.0)), vec![(0, 59)]);
}

//endregion

/*┌─────────────────╮
| Made-up walkers |
└─────────────────╯*/
//region Made-up walkers

/// A biped facing +Z standing on y = 0, named by `name`, with thighs and
/// shins of the given lengths, ankles `ankle` above the ground and every
/// joint frame unturned.
fn walker(name: fn(&str) -> String, thigh: f32, shin: f32, ankle: f32) -> Scene {
    let mut rig = crate::tests::Rig::new();
    let socket = ankle + shin + thigh;
    let mut at = std::collections::HashMap::new();
    let mut add = |rig: &mut crate::tests::Rig, bone: &str, parent: Option<&str>, pos: Vec3| {
        let parent = parent.map(|p| at[p]);
        let i = rig.joint(&name(bone), parent, pos, Quat::IDENTITY);
        at.insert(bone.to_string(), i);
    };
    add(&mut rig, "hips", None, Vec3::new(0.0, socket + 0.05, 0.0));
    add(&mut rig, "spine", Some("hips"), Vec3::new(0.0, socket + 0.15, 0.0));
    add(&mut rig, "chest", Some("spine"), Vec3::new(0.0, socket + 0.35, 0.0));
    add(&mut rig, "neck", Some("chest"), Vec3::new(0.0, socket + 0.55, 0.0));
    add(&mut rig, "head", Some("neck"), Vec3::new(0.0, socket + 0.65, 0.0));
    for (side, x) in [("left", 1.0f32), ("right", -1.0)] {
        let b = |part: &str| format!("{side}{part}");
        add(&mut rig, &b("Shoulder"), Some("chest"), Vec3::new(0.05 * x, socket + 0.5, 0.0));
        add(&mut rig, &b("UpperArm"), Some(&b("Shoulder")), Vec3::new(0.18 * x, socket + 0.5, 0.0));
        add(&mut rig, &b("LowerArm"), Some(&b("UpperArm")), Vec3::new(0.45 * x, socket + 0.5, 0.0));
        add(&mut rig, &b("Hand"), Some(&b("LowerArm")), Vec3::new(0.7 * x, socket + 0.5, 0.0));
        add(&mut rig, &b("UpperLeg"), Some("hips"), Vec3::new(0.1 * x, socket, 0.0));
        add(&mut rig, &b("LowerLeg"), Some(&b("UpperLeg")), Vec3::new(0.1 * x, ankle + shin, 0.0));
        add(&mut rig, &b("Foot"), Some(&b("LowerLeg")), Vec3::new(0.1 * x, ankle, 0.0));
        add(&mut rig, &b("Toes"), Some(&b("Foot")), Vec3::new(0.1 * x, 0.02, 0.13));
    }
    rig.finish()
}

/// The walk's gait: a 1.2 s cycle at 0.8 m/s, each foot down for 0.72 s
/// (the left from the cycle's start, the right from its middle) and
/// swinging in a 7 cm arc for the rest.  Returns where the ankle is
/// relative to its rest place, and whether it is down.
fn gait(t: f32, left: bool) -> (Vec3, bool) {
    let (cycle, stride, stance) = (1.2f32, 0.96f32, 0.72f32);
    let offset = if left { 0.0 } else { 0.5 };
    let c = t / cycle + offset;
    let (n, f) = (c.floor(), c.fract() * cycle);
    // A foot lands half a stance's travel ahead of the hips.
    let start = (n - offset) * stride + stride * stance / cycle / 2.0;
    if f < stance {
        return (Vec3::new(0.0, 0.0, start), true);
    }
    let s = (f - stance) / (cycle - stance);
    let ease = s * s * (3.0 - 2.0 * s);
    (Vec3::new(0.0, 0.07 * (s * PI).sin(), start + stride * ease), false)
}

/// A 2.4 s walk with root motion on `walker`'s skeleton: the hips travel
/// at the gait's speed, a little lower than at rest so the legs reach,
/// and two-bone IK puts each flat foot where the gait says, so planted
/// feet stand exactly still.
fn walk(scene: &Scene, name: fn(&str) -> String) -> Animation {
    let node = |b: &str| scene.nodes.iter().position(|n| n.name == name(b)).unwrap();
    let rest = Pose::rest(scene);
    let at = |n: usize| position(&rest.world[n]);
    let times: Vec<f32> = (0..=72).map(|i| i as f32 / 30.0).collect();
    let hips = node("hips");
    let mut travel = Vec::new();
    let mut keys: Vec<(usize, Vec<f32>)> = Vec::new();
    for side in ["left", "right"] {
        for part in ["UpperLeg", "LowerLeg", "Foot"] {
            keys.push((node(&format!("{side}{part}")), Vec::new()));
        }
    }
    for &t in &times {
        let lift = Vec3::new(0.0, -0.06 + 0.01 * (t * 2.0 * PI / 0.6).cos(), 0.8 * t);
        travel.extend_from_slice(&(at(hips) + lift).to_array());
        for (s, side) in ["left", "right"].iter().enumerate() {
            let [u, l, f] = ["UpperLeg", "LowerLeg", "Foot"].map(|p| node(&format!("{side}{p}")));
            let goal = at(f) + gait(t, *side == "left").0;
            let (upper, lower) =
                two_bone(at(u) + lift, at(l) + lift, at(f) + lift, goal, Vec3::Z, Quat::IDENTITY, Quat::IDENTITY);
            for (i, q) in [upper, upper.inverse() * lower, lower.inverse()].into_iter().enumerate() {
                keys[s * 3 + i].1.extend_from_slice(&q.to_array());
            }
        }
    }
    let channel = |node: usize, property: Property, values: Vec<f32>| Channel {
        node,
        property,
        interpolation: slerp_scene::Interpolation::Linear,
        times: times.clone(),
        values,
    };
    let mut channels: Vec<Channel> = keys.into_iter().map(|(n, v)| channel(n, Property::Rotation, v)).collect();
    channels.push(channel(hips, Property::Translation, travel));
    Animation { name: "walk".into(), duration: 2.4, channels }
}

use slerp_scene::{Animation, Channel, Property};

/// The most each ankle moves while the gait has its foot down, over the
/// middle of each stance (a tenth of a second in from either end, where IK
/// fades), in centimetres; and the deepest a toe or ankle goes below its
/// rest height.
fn planted_drift(scene: &Scene, anim: usize, name: fn(&str) -> String) -> (f32, f32) {
    let node = |b: &str| scene.nodes.iter().position(|n| n.name == name(b)).unwrap();
    let rest = Pose::rest(scene);
    let (mut drift, mut sink) = (0.0f32, 0.0f32);
    for side in ["left", "right"] {
        let (ankle, toes) = (node(&format!("{side}Foot")), node(&format!("{side}Toes")));
        let mut start: Option<Vec3> = None;
        for i in 0..=240 {
            let t = i as f32 * 0.01;
            let mut pose = Pose::rest(scene);
            pose.sample(scene, anim, t);
            pose.update_world(scene);
            for j in [ankle, toes] {
                sink = sink.min(position(&pose.world[j]).y - position(&rest.world[j]).y);
            }
            let down = [-0.1, 0.0, 0.1].iter().all(|d| gait((t + d).clamp(0.0, 2.4), side == "left").1);
            let p = position(&pose.world[ankle]);
            match (down, start) {
                (true, None) => start = Some(p),
                (true, Some(s)) => drift = drift.max((p - s).length()),
                (false, _) => start = None,
            }
        }
    }
    (drift * 100.0, -sink * 100.0)
}

#[test]
fn walkers_keep_their_feet() {
    let mut source = walker(crate::tests::mixamo, 0.45, 0.42, 0.08);
    let clip = walk(&source, crate::tests::mixamo);
    source.animations.push(clip);
    let (drift, _) = planted_drift(&source, 0, crate::tests::mixamo);
    // Rotations interpolated between keys 1/30 s apart drift a millimetre.
    assert!(drift < 0.2, "the made-up walk's own feet drift {drift} cm");

    // Long thighs and short shins: the transferred angles put the feet
    // elsewhere, so they slide; foot IK holds them.
    let mut target = walker(crate::tests::unreal, 0.55, 0.3, 0.1);
    let added = crate::add_animations_with(&mut target, &source, crate::Options::default()).unwrap();
    target.animations.extend(added.others);
    let (ik, plain) =
        (planted_drift(&target, 0, crate::tests::unreal), planted_drift(&target, 1, crate::tests::unreal));
    println!(
        "made-up walker: plain transfer drifts {:.2} cm, sinks {:.2} cm; foot IK {:.2} cm, {:.2} cm",
        plain.0, plain.1, ik.0, ik.1
    );
    assert!(plain.0 > 2.0, "the plain transfer should slide: {plain:?}");
    assert!(ik.0 < 0.4 && ik.1 < 0.1, "foot IK: {ik:?}");

    // The same proportions at another size: the transfer is already
    // right, and foot IK changes next to nothing.
    let mut small = walker(crate::tests::unreal, 0.36, 0.336, 0.064);
    let added = crate::add_animations_with(&mut small, &source, crate::Options::default()).unwrap();
    small.animations.extend(added.others);
    let (a, b) = (&small.animations[0], &small.animations[1]);
    for t in [0.3, 0.9, 1.7] {
        let pose = |anim: usize| {
            let mut p = Pose::rest(&small);
            p.sample(&small, anim, t);
            p.update_world(&small);
            p
        };
        let (pa, pb) = (pose(0), pose(1));
        for (i, (x, y)) in pa.world.iter().zip(&pb.world).enumerate() {
            let d = position(x).distance(position(y));
            assert!(d < 0.002, "{} moved {d} at {t} ({}, {})", small.nodes[i].name, a.name, b.name);
        }
    }
}

//endregion

/*┌─────────────────╮
| Real clips      |
└─────────────────╯*/
//region Real clips

/// A path in a checkout of chalk (a game whose files some tests read) in the
/// folder beside this repository's; tests skip the files that aren't there.
macro_rules! chalk {
    ($path:literal) => {
        concat!(env!("CARGO_MANIFEST_DIR"), "/../../../chalk/", $path)
    };
}

const UAL1: &str =
    chalk!("tools/humans/vendor/ual/ual1/Universal Animation Library[Standard]/Unreal-Godot/UAL1_Standard.glb");
const METAHUMAN: &str = chalk!("html/demos/humans/tate-tee-lod3.glb");

fn assets() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../local/assets")
}

/// A character or library, without its meshes, or None (with a note)
/// when the file isn't there.
fn load(path: &Path) -> Option<Scene> {
    let Ok(bytes) = std::fs::read(path) else {
        eprintln!("skipped: {} is missing", path.display());
        return None;
    };
    let limits = Limits::viewer();
    let asset = match slerp_models::sniff(path, &bytes[..bytes.len().min(64)]) {
        Some(format) => slerp_models::load(format, path, &bytes, &limits),
        None => slerp_gltf::load(path, &bytes, &limits),
    };
    let Ok(Asset::Scene(scene)) = asset else { panic!("{} didn't load as a scene", path.display()) };
    let mut bones = Scene {
        nodes: scene.nodes,
        roots: scene.roots,
        skins: scene.skins,
        animations: scene.animations,
        ..Scene::default()
    };
    bones.humanoid = scene.humanoid;
    Some(bones)
}

/// What a clip does to the feet, over the moments a source ankle or toe
/// is down (planted and within a centimetre of the lowest it gets in that
/// contact): how far it slides (the most it strays from where it first
/// came down, in centimetres; mean and worst over contacts), how far it
/// is from the height the scaled source puts it at (mean, cm), and the
/// deepest any ankle or toe sinks below its rest height at any moment
/// (cm).  Under a clip with root motion, sliding is measured against the
/// world; under one made in place, whose planted feet slide back with a
/// treadmill that changes speed over the stride, against the scaled
/// source's own joint.
#[derive(Clone, Copy, Debug, Default)]
struct Feel {
    slide: f32,
    worst: f32,
    height: f32,
    sink: f32,
}

/// One clip's feet: the scaled source's, the plain transfer's and foot
/// IK's, and how many contacts there were.
struct Report {
    source: Feel,
    plain: Feel,
    ik: Feel,
    contacts: usize,
    drop: f32,
}

fn report(target: &Scene, library: &Scene, anim: usize, dump: bool) -> Option<Report> {
    let (src, dst) = (Skeleton::new(library), Skeleton::new(target));
    let plan = Plan::new(&src, &dst, Method::SLERP)?;
    let feet = plan.feet()?;
    let baked = plan.bake(anim);
    let ik = baked.feet.clone().unwrap_or_else(|| baked.plain.clone());
    let times = baked.plain.channels[0].times.clone();
    let n = times.len();

    // 1. The source's watched joints and their contacts.
    let mut pose = Pose::rest(library);
    let mut watched = Vec::new();
    for &t in &times {
        pose.reset(library);
        pose.sample(library, anim, t);
        pose.update_world(library);
        feet.watch(&pose.world, plan.yaw(), &mut watched);
    }
    let per = feet.legs.len() * 2;
    let tracks: Vec<[Vec<Vec3>; 2]> =
        (0..feet.legs.len()).map(|l| [0, 1].map(|j| (0..n).map(|k| watched[k * per + l * 2 + j]).collect())).collect();
    let joints: Vec<&[Vec3]> = feet
        .legs
        .iter()
        .zip(&tracks)
        .flat_map(|(leg, t)| t[..if leg.toes { 2 } else { 1 }].iter().map(|v| v.as_slice()))
        .collect();
    let rests: Vec<f32> =
        feet.legs.iter().flat_map(|leg| leg.src_rest[..if leg.toes { 2 } else { 1 }].iter().map(|p| p.y)).collect();
    let ground = ground_velocity(&times, &joints, &rests, feet.unit);
    let watches: Vec<Watch> =
        feet.legs.iter().zip(&tracks).map(|(leg, t)| Watch::new(&times, t, leg.toes, ground, feet.unit)).collect();

    // 2. The target's ankles and toes in each version, and where the scaled
    //    source puts them.
    let mut scene = target.clone();
    scene.animations = vec![baked.plain, ik];
    let hips = crate::humanoid::humanoid_of(dst.scene).and_then(|h| h.node("hips"));
    let mut pose = Pose::rest(&scene);
    let mut posed = |clip: usize| {
        let mut out: Vec<[Vec<Vec3>; 2]> = feet.legs.iter().map(|_| [Vec::new(), Vec::new()]).collect();
        let mut hip_y = Vec::new();
        for &t in &times {
            pose.reset(&scene);
            pose.sample(&scene, clip, t);
            pose.update_world(&scene);
            for (track, leg) in out.iter_mut().zip(&feet.legs) {
                for (joint, &node) in track.iter_mut().zip(&leg.dst) {
                    joint.push(position(&pose.world[node]));
                }
            }
            hip_y.push(hips.map_or(0.0, |h| position(&pose.world[h]).y));
        }
        (out, hip_y)
    };
    let ((plain, plain_hips), (fixed, fixed_hips)) = (posed(0), posed(1));
    let scaled: Vec<[Vec<Vec3>; 2]> = feet
        .legs
        .iter()
        .zip(&tracks)
        .map(|(leg, t)| {
            [0, 1].map(|j| t[j].iter().map(|p| leg.dst_rest[j] + (*p - leg.src_rest[j]) * feet.scale).collect())
        })
        .collect();

    // 3. The measures.
    let src_ground: Vec<Vec3> = times.iter().map(|t| ground * *t).collect();
    let ground: Vec<Vec3> = src_ground.iter().map(|g| *g * feet.scale).collect();
    let in_place = src_ground[n - 1] != Vec3::ZERO;
    let feel = |track: &[[Vec<Vec3>; 2]]| {
        let (mut slides, mut heights, mut sink) = (Vec::new(), Vec::new(), 0.0f32);
        for (l, leg) in feet.legs.iter().enumerate() {
            for j in 0..if leg.toes { 2 } else { 1 } {
                let at = |k: usize| track[l][j][k] - if in_place { scaled[l][j][k] } else { Vec3::ZERO };
                for (start, end) in runs(&watches[l].planted[j]) {
                    let down: Vec<usize> = (start..=end).filter(|&k| watches[l].down[j][k]).collect();
                    let base = at(down[0]);
                    let far = down
                        .iter()
                        .map(|&k| {
                            let d = at(k) - base;
                            Vec3::new(d.x, 0.0, d.z).length()
                        })
                        .fold(0.0, f32::max);
                    slides.push(far * 100.0);
                    heights.extend(down.iter().map(|&k| (track[l][j][k].y - scaled[l][j][k].y).abs() * 100.0));
                }
                for p in &track[l][j] {
                    sink = sink.min(p.y - leg.dst_rest[j].y);
                }
            }
        }
        let mean = |v: &[f32]| v.iter().sum::<f32>() / v.len().max(1) as f32;
        Feel {
            slide: mean(&slides),
            worst: slides.iter().copied().fold(0.0, f32::max),
            height: mean(&heights),
            sink: -sink * 100.0,
        }
    };
    if dump {
        let l = 0;
        println!("ground at the end {} unit {} scale {}", ground[n - 1], feet.unit, feet.scale);
        println!(
            "   t     A  T  | src.y   plain.y  ik.y   goal.y | plain.z  ik.z  src.z | hips plain ik | toe plain ik goal | toe.z plain ik goal"
        );
        for k in 0..n {
            let w = &watches[l];
            let flag = |j: usize| ["  ", "p ", "pd"][w.planted[j][k] as usize + w.down[j][k] as usize];
            println!(
                "{:6.3} {} {} | {:6.3} {:6.3} {:6.3} {:6.3} | {:6.3} {:6.3} {:6.3} | {:6.3} {:6.3} | {:6.3} {:6.3} {:6.3} | {:6.3} {:6.3} {:6.3}",
                times[k],
                flag(0),
                flag(1),
                tracks[l][0][k].y,
                plain[l][0][k].y,
                fixed[l][0][k].y,
                scaled[l][0][k].y,
                plain[l][0][k].z - ground[k].z,
                fixed[l][0][k].z - ground[k].z,
                scaled[l][0][k].z - ground[k].z,
                plain_hips[k],
                fixed_hips[k],
                plain[l][1][k].y,
                fixed[l][1][k].y,
                scaled[l][1][k].y,
                plain[l][1][k].z - ground[k].z,
                fixed[l][1][k].z - ground[k].z,
                scaled[l][1][k].z - ground[k].z,
            );
        }
    }
    let contacts = watches.iter().map(|w| runs(&w.planted[0]).len() + runs(&w.planted[1]).len()).sum();
    let drop = plain_hips.iter().zip(&fixed_hips).map(|(a, b)| a - b).fold(0.0, f32::max) * 100.0;
    Some(Report { source: feel(&scaled), plain: feel(&plain), ik: feel(&fixed), contacts, drop })
}

/// The library with clip `anim` given root motion, as a clip with root
/// motion has it: a clip made in place has its hips carried forward along
/// the path its planted feet slide back on (the lowest joint's), so they
/// stand still.  None when the clip moves already.
fn with_root_motion(library: &Scene, anim: usize) -> Option<Scene> {
    let (src, dst) = (Skeleton::new(library), Skeleton::new(library));
    let plan = Plan::new(&src, &dst, Method::SLERP)?;
    let feet = plan.feet()?;
    let (times, _) = crate::skeleton::sample_times(&library.animations[anim]);
    let n = times.len();
    let mut pose = Pose::rest(library);
    let mut watched = Vec::new();
    for &t in &times {
        pose.reset(library);
        pose.sample(library, anim, t);
        pose.update_world(library);
        feet.watch(&pose.world, Quat::IDENTITY, &mut watched);
    }
    let per = feet.legs.len() * 2;
    let joints: Vec<Vec<Vec3>> = (0..per).map(|j| (0..n).map(|k| watched[k * per + j]).collect()).collect();
    let refs: Vec<&[Vec3]> = joints.iter().map(|a| a.as_slice()).collect();
    let rests: Vec<f32> = feet.legs.iter().flat_map(|leg| leg.src_rest.iter().map(|p| p.y)).collect();
    let typical = ground_velocity(&times, &refs, &rests, feet.unit);
    if typical == Vec3::ZERO {
        return None;
    }
    // 1. The lowest joint's velocity at each moment it is near its lowest,
    //    the typical one in the air.
    let lowest: Vec<f32> = joints.iter().map(|a| a.iter().map(|p| p.y).fold(f32::MAX, f32::min)).collect();
    let velocity: Vec<Vec3> = (0..n)
        .map(|k| {
            let (j, h) = (0..per).map(|j| (j, joints[j][k].y - lowest[j])).min_by(|a, b| a.1.total_cmp(&b.1)).unwrap();
            let (a, b) = (k.saturating_sub(1), (k + 1).min(n - 1));
            let d = joints[j][b] - joints[j][a];
            let v = Vec3::new(d.x, 0.0, d.z) / (times[b] - times[a]);
            if h < ENTER_HEIGHT * feet.unit && v.is_finite() { v } else { typical }
        })
        .collect();
    // 2. Added up into the ground's path.
    let mut path = vec![Vec3::ZERO; n];
    for k in 1..n {
        path[k] = path[k - 1] + (velocity[k - 1] + velocity[k]) * 0.5 * (times[k] - times[k - 1]);
    }
    let path_at = |t: f32| {
        let k = times.partition_point(|&x| x < t).min(n - 1);
        if k == 0 || times[k] <= t {
            return path[k];
        }
        let f = (t - times[k - 1]) / (times[k] - times[k - 1]);
        path[k - 1].lerp(path[k], f)
    };
    // 3. The hips' translation keys, moved against it in their parent's
    //    frame.
    let hips = crate::humanoid::humanoid_of(library)?.node("hips")?;
    let to_local = src.parents[hips].map_or(Mat4::IDENTITY, |p| src.rest.world[p].inverse());
    let mut out = library.clone();
    let channel = out.animations[anim].channels.iter_mut().find(|c| {
        c.node == hips
            && c.property == slerp_scene::Property::Translation
            && c.interpolation != slerp_scene::Interpolation::CubicSpline
    })?;
    for (t, v) in channel.times.iter().zip(channel.values.chunks_exact_mut(3)) {
        let moved = Vec3::from_slice(v) - to_local.transform_vector3(path_at(*t));
        v.copy_from_slice(&moved.to_array());
    }
    Some(out)
}

/// Measures `clips` from `library` on `target`, as they are and given
/// root motion, prints a line each and returns the reports.
fn measure(target: &Path, library: &Path, clips: &[&str]) -> Vec<Report> {
    let (Some(scene), Some(lib)) = (load(target), load(library)) else { return Vec::new() };
    let dump = std::env::var("FOOTIK_DUMP").ok();
    let mut out = Vec::new();
    for &name in clips {
        let Some(anim) = lib.animations.iter().position(|a| a.name == name) else {
            let names: Vec<&str> = lib.animations.iter().map(|a| a.name.as_str()).collect();
            panic!("no clip {name} in {names:?}")
        };
        let moved = with_root_motion(&lib, anim);
        for (label, lib) in [("", Some(&lib)), (" +root", moved.as_ref())] {
            let Some(lib) = lib else { continue };
            let full = format!("{name}{label}");
            let Some(r) = report(&scene, lib, anim, dump.as_deref() == Some(full.as_str())) else {
                println!("{full}: no foot IK");
                continue;
            };
            let f = |x: Feel| format!("{:5.2} {:5.2} {:5.2} {:5.2}", x.slide, x.worst, x.height, x.sink);
            println!(
                "{:<26} {:<24} contacts {:3}  source {}  plain {}  ik {}  drop {:4.1}",
                target.file_name().unwrap().to_string_lossy(),
                full,
                r.contacts,
                f(r.source),
                f(r.plain),
                f(r.ik),
                r.drop
            );
            out.push(r);
        }
    }
    out
}

const UAL_CLIPS: [&str; 6] =
    ["Walk_Loop", "Jog_Fwd_Loop", "Sprint_Loop", "Idle_Loop", "Crouch_Fwd_Loop", "Walk_Formal_Loop"];

/// Foot IK never slides or sinks more than the worse of the plain transfer
/// and the source, give or take 2 mm.
fn no_worse(reports: Vec<Report>) {
    for r in reports {
        assert!(r.ik.slide <= r.plain.slide.max(r.source.slide) + 0.2, "slide {:?} vs {:?}", r.ik, r.plain);
        assert!(r.ik.sink <= r.plain.sink.max(r.source.sink) + 0.2, "sink {:?} vs {:?}", r.ik, r.plain);
    }
}

#[test]
fn ual_on_metahuman() {
    no_worse(measure(Path::new(METAHUMAN), Path::new(UAL1), &UAL_CLIPS));
}

#[test]
fn ual_on_vrm() {
    for avatar in ["AvatarSample_B.vrm", "AliciaSolid_vrm-0.51.vrm", "Seed-san.vrm"] {
        no_worse(measure(&assets().join("vrm").join(avatar), Path::new(UAL1), &UAL_CLIPS));
    }
}

#[test]
fn mixamo() {
    let three = assets().join("gltf/three");
    for target in ["Soldier.glb", "Michelle.glb", "readyplayer.me.glb"] {
        no_worse(measure(&three.join(target), &three.join("Xbot.glb"), &["walk", "run", "idle"]));
    }
    no_worse(measure(Path::new(METAHUMAN), &three.join("Xbot.glb"), &["walk", "run"]));
    no_worse(measure(&assets().join("vrm/AvatarSample_B.vrm"), &three.join("Soldier.glb"), &["Walk", "Run"]));
}

//endregion
