//! Retargeting between made-up skeletons whose answers are known.

use super::*;
use slerp_scene::glam::{Quat, Vec3};
use slerp_scene::{Channel, Interpolation, Node, Pose, Property, Transform};

/// Builds a skeleton from world positions and world rotations, so tests
/// can give joints arbitrary bone rolls.
pub(crate) struct Rig {
    scene: Scene,
    world: Vec<(Vec3, Quat)>,
}

impl Rig {
    pub(crate) fn new() -> Self {
        Rig { scene: Scene::default(), world: Vec::new() }
    }

    pub(crate) fn joint(&mut self, name: &str, parent: Option<usize>, pos: Vec3, rot: Quat) -> usize {
        let (pp, pr) = parent.map_or((Vec3::ZERO, Quat::IDENTITY), |p| self.world[p]);
        let mut node = Node::new(name);
        node.transform =
            Transform { translation: pr.inverse() * (pos - pp), rotation: pr.inverse() * rot, ..Transform::IDENTITY };
        self.scene.nodes.push(node);
        let i = self.scene.nodes.len() - 1;
        if let Some(p) = parent {
            self.scene.nodes[p].children.push(i);
        }
        self.world.push((pos, rot));
        i
    }

    pub(crate) fn finish(mut self) -> Scene {
        self.scene.compute_roots();
        self.scene
    }
}

/// Joint names in one rig's scheme, by VRM bone.
pub(crate) fn mixamo(bone: &str) -> String {
    let side = |b: &str| b.replace("left", "Left").replace("right", "Right");
    match bone {
        "hips" => "mixamorig:Hips".into(),
        "spine" => "mixamorig:Spine".into(),
        "chest" => "mixamorig:Spine1".into(),
        "neck" => "mixamorig:Neck".into(),
        "head" => "mixamorig:Head".into(),
        b => {
            let b = side(b)
                .replace("UpperArm", "Arm")
                .replace("LowerArm", "ForeArm")
                .replace("UpperLeg", "UpLeg")
                .replace("LowerLeg", "Leg")
                .replace("Toes", "ToeBase")
                .replace("MiddleProximal", "HandMiddle1");
            format!("mixamorig:{b}")
        }
    }
}

pub(crate) fn unreal(bone: &str) -> String {
    let s = if bone.starts_with("left") { "l" } else { "r" };
    let part = bone.trim_start_matches("left").trim_start_matches("right");
    match bone {
        "hips" => "pelvis".into(),
        "spine" => "spine_01".into(),
        "chest" => "spine_02".into(),
        "neck" => "neck_01".into(),
        "head" => "head".into(),
        _ => {
            let p = match part {
                "Shoulder" => "clavicle",
                "UpperArm" => "upperarm",
                "LowerArm" => "lowerarm",
                "Hand" => "hand",
                "MiddleProximal" => "middle_01",
                "UpperLeg" => "thigh",
                "LowerLeg" => "calf",
                "Foot" => "foot",
                "Toes" => "ball",
                _ => unreachable!(),
            };
            format!("{p}_{s}")
        }
    }
}

const BONES: [(&str, Option<&str>); 25] = [
    ("hips", None),
    ("spine", Some("hips")),
    ("chest", Some("spine")),
    ("neck", Some("chest")),
    ("head", Some("neck")),
    ("leftShoulder", Some("chest")),
    ("leftUpperArm", Some("leftShoulder")),
    ("leftLowerArm", Some("leftUpperArm")),
    ("leftHand", Some("leftLowerArm")),
    ("leftMiddleProximal", Some("leftHand")),
    ("rightShoulder", Some("chest")),
    ("rightUpperArm", Some("rightShoulder")),
    ("rightLowerArm", Some("rightUpperArm")),
    ("rightHand", Some("rightLowerArm")),
    ("rightMiddleProximal", Some("rightHand")),
    ("leftUpperLeg", Some("hips")),
    ("leftLowerLeg", Some("leftUpperLeg")),
    ("leftFoot", Some("leftLowerLeg")),
    ("leftToes", Some("leftFoot")),
    ("rightUpperLeg", Some("hips")),
    ("rightLowerLeg", Some("rightUpperLeg")),
    ("rightFoot", Some("rightLowerLeg")),
    ("rightToes", Some("rightFoot")),
    ("root", None),
    ("prop", Some("leftHand")),
];

/// A T-pose (`arm_drop` 0) or A-pose humanoid facing +Z, scaled, turned
/// by `facing` about the vertical, with joint frames rolled by `roll`
/// (a different made-up roll per joint when true).
fn humanoid(name: fn(&str) -> String, scale: f32, arm_drop: f32, facing: f32, roll: bool) -> Scene {
    let mut rig = Rig::new();
    let turn = Quat::from_rotation_y(facing);
    let mut index = std::collections::HashMap::new();
    for (k, (bone, parent)) in BONES.iter().enumerate() {
        if *bone == "root" || *bone == "prop" {
            continue;
        }
        let x = |v: f32| if bone.starts_with("right") { -v } else { v };
        let mut p = match bone.trim_start_matches("left").trim_start_matches("right") {
            "hips" => Vec3::new(0.0, 1.0, 0.0),
            "spine" => Vec3::new(0.0, 1.1, 0.0),
            "chest" => Vec3::new(0.0, 1.25, 0.02),
            "neck" => Vec3::new(0.0, 1.5, 0.0),
            "head" => Vec3::new(0.0, 1.6, 0.02),
            "Shoulder" => Vec3::new(x(0.04), 1.45, 0.0),
            "UpperArm" => Vec3::new(x(0.18), 1.45, 0.0),
            "LowerArm" => Vec3::new(x(0.46), 1.45, 0.0),
            "Hand" => Vec3::new(x(0.72), 1.45, 0.0),
            "MiddleProximal" => Vec3::new(x(0.82), 1.45, 0.0),
            "UpperLeg" => Vec3::new(x(0.1), 0.95, 0.0),
            "LowerLeg" => Vec3::new(x(0.1), 0.5, 0.0),
            "Foot" => Vec3::new(x(0.1), 0.08, 0.0),
            "Toes" => Vec3::new(x(0.1), 0.0, 0.12),
            _ => unreachable!(),
        };
        // An A-pose drops the arms about the shoulder joint.
        if arm_drop != 0.0
            && (bone.contains("Arm") && !bone.contains("Upper") || bone.contains("Hand") || bone.contains("Middle"))
        {
            let pivot = Vec3::new(x(0.18), 1.45, 0.0);
            let drop = Quat::from_rotation_z(if bone.starts_with("left") { -arm_drop } else { arm_drop });
            p = pivot + drop * (p - pivot);
        }
        let rot = if roll {
            Quat::from_euler(slerp_scene::glam::EulerRot::XYZ, k as f32 * 0.7, k as f32 * 1.3, k as f32 * 0.4)
        } else {
            Quat::IDENTITY
        };
        let parent = parent.map(|p| index[p]);
        let i = rig.joint(&name(bone), parent, turn * (p * scale), turn * rot);
        index.insert(*bone, i);
    }
    // A joint no humanoid map knows.
    let prop = rig.joint("prop", Some(index["leftHand"]), turn * Vec3::new(0.9, 1.45, 0.1) * scale, turn);
    let _ = prop;
    rig.finish()
}

/// A clip on a skeleton named by `name`: the left arm drops and bends,
/// the spine bows, the right leg lifts and the hips travel forward.
fn clip(scene: &Scene, name: fn(&str) -> String) -> Animation {
    let node = |b: &str| scene.nodes.iter().position(|n| n.name == name(b)).unwrap();
    let rest = |b: &str| scene.nodes[node(b)].transform;
    let turn = |b: &str, q: Quat| {
        let r = rest(b).rotation;
        let mut values = Vec::new();
        for f in [0.0, 0.5, 1.0] {
            values.extend_from_slice(&(r * Quat::IDENTITY.slerp(q, f)).to_array());
        }
        Channel {
            node: node(b),
            property: Property::Rotation,
            interpolation: Interpolation::Linear,
            times: vec![0.0, 0.5, 1.0],
            values,
        }
    };
    let hips = rest("hips").translation;
    let mut travel = Vec::new();
    for f in [0.0, 0.5, 1.0] {
        travel.extend_from_slice(&(hips + Vec3::new(0.0, -0.1 * f, 0.4 * f)).to_array());
    }
    Animation {
        name: "walk".into(),
        duration: 1.0,
        channels: vec![
            turn("leftUpperArm", Quat::from_rotation_z(-1.2) * Quat::from_rotation_y(0.4)),
            turn("leftLowerArm", Quat::from_rotation_y(0.9)),
            turn("spine", Quat::from_rotation_x(0.3)),
            turn("rightUpperLeg", Quat::from_rotation_x(-0.8)),
            turn("rightLowerLeg", Quat::from_rotation_x(0.9)),
            Channel {
                node: node("hips"),
                property: Property::Translation,
                interpolation: Interpolation::Linear,
                times: vec![0.0, 0.5, 1.0],
                values: travel,
            },
        ],
    }
}

fn world_pos(scene: &Scene, pose: &Pose, name: &str) -> Vec3 {
    let i = scene.nodes.iter().position(|n| n.name == name).unwrap();
    pose.world[i].w_axis.truncate()
}

fn posed(scene: &Scene, anim: usize, t: f32) -> Pose {
    let mut pose = Pose::rest(scene);
    pose.sample(scene, anim, t);
    pose.update_world(scene);
    pose
}

/// The direction from `a` to `b` in the posed scene.
fn dir(scene: &Scene, pose: &Pose, name: fn(&str) -> String, a: &str, b: &str) -> Vec3 {
    (world_pos(scene, pose, &name(b)) - world_pos(scene, pose, &name(a))).normalize()
}

#[test]
fn t_pose_clip_on_rolled_a_pose_facing_away() {
    let mut library = humanoid(mixamo, 1.0, 0.0, 0.0, false);
    library.animations.push(clip(&library, mixamo));
    // Smaller, A-posed, facing -Z, with every joint frame rolled.
    let mut target = humanoid(unreal, 0.8, 0.7, std::f32::consts::PI, true);
    assert_eq!(strategy(&target, &library).unwrap(), Strategy::Humanoid);
    // The transfer alone: foot IK would pin the left foot, which slides
    // forward under the gliding hips.
    let added = add_animations_with(&mut target, &library, Options { foot_ik: false }).unwrap();
    assert_eq!(added.names, ["walk"]);
    assert_eq!(added.others.len(), 1, "the version with foot IK");
    let yaw = Quat::from_rotation_y(std::f32::consts::PI);
    let limbs = [
        ("leftUpperArm", "leftLowerArm"),
        ("leftLowerArm", "leftHand"),
        ("leftHand", "leftMiddleProximal"),
        ("rightUpperArm", "rightLowerArm"),
        ("spine", "chest"),
        ("chest", "neck"),
        ("rightUpperLeg", "rightLowerLeg"),
        ("rightLowerLeg", "rightFoot"),
        ("leftFoot", "leftToes"),
    ];
    for t in [0.0, 0.25, 0.5, 1.0] {
        let (sp, tp) = (posed(&library, 0, t), posed(&target, 0, t));
        for (a, b) in limbs {
            let want = yaw * dir(&library, &sp, mixamo, a, b);
            let got = dir(&target, &tp, unreal, a, b);
            assert!(want.angle_between(got) < 0.01, "{a} at {t}: want {want}, got {got}");
        }
        // Travel scaled by hip height and turned to the target's forward.
        let moved = world_pos(&target, &tp, "pelvis") - world_pos(&target, &Pose::rest(&target), "pelvis");
        let expect = yaw * Vec3::new(0.0, -0.1 * t, 0.4 * t) * 0.8;
        assert!((moved - expect).length() < 1e-4, "travel at {t}: {moved} vs {expect}");
    }
    // Bone lengths stay the target's own.
    let rest = Pose::rest(&target);
    let pose = posed(&target, 0, 0.7);
    for (bone, parent) in BONES.iter().filter_map(|(b, p)| Some((*b, (*p)?))) {
        if bone == "prop" {
            continue;
        }
        let len = |p: &Pose| (world_pos(&target, p, &unreal(bone)) - world_pos(&target, p, &unreal(parent))).length();
        assert!((len(&rest) - len(&pose)).abs() < 1e-5, "{bone}");
    }
}

#[test]
fn same_skeleton_copies_clips() {
    let mut library = humanoid(mixamo, 1.0, 0.0, 0.0, true);
    library.animations.push(clip(&library, mixamo));
    let mut target = humanoid(mixamo, 1.0, 0.0, 0.0, true);
    assert_eq!(strategy(&target, &library).unwrap(), Strategy::SameNames);
    add_animations(&mut target, &library).unwrap();
    let (a, b) = (&library.animations[0], &target.animations[0]);
    assert_eq!(a.channels.len(), b.channels.len());
    for (x, y) in a.channels.iter().zip(&b.channels) {
        assert_eq!(x.node, y.node);
        for (p, q) in x.values.iter().zip(&y.values) {
            assert!((p - q).abs() < 1e-6);
        }
    }
    // A second copy gets a new name.
    assert_eq!(add_animations(&mut target, &library).unwrap(), ["walk (2)"]);
    assert_eq!(add_animations(&mut target, &library).unwrap(), ["walk (3)"]);
}

#[test]
fn same_names_twice_the_size() {
    let mut library = humanoid(unreal, 1.0, 0.0, 0.0, false);
    library.animations.push(clip(&library, unreal));
    let mut target = humanoid(unreal, 2.0, 0.0, 0.0, false);
    assert_eq!(strategy(&target, &library).unwrap(), Strategy::SameNames);
    add_animations(&mut target, &library).unwrap();
    let pose = posed(&target, 0, 1.0);
    let moved = world_pos(&target, &pose, "pelvis") - world_pos(&target, &Pose::rest(&target), "pelvis");
    assert!((moved - Vec3::new(0.0, -0.2, 0.8)).length() < 1e-4, "{moved}");
}

#[test]
fn errors() {
    let library = humanoid(mixamo, 1.0, 0.0, 0.0, false);
    let mut target = humanoid(unreal, 1.0, 0.0, 0.0, false);
    assert!(matches!(add_animations(&mut target, &library), Err(RetargetError::NoClips)));

    // A clip for a skeleton with nothing in common.
    let mut rig = Rig::new();
    let a = rig.joint("Bone", None, Vec3::ZERO, Quat::IDENTITY);
    rig.joint("Bone.001", Some(a), Vec3::Y, Quat::IDENTITY);
    let mut library = rig.finish();
    library.animations.push(Animation {
        name: "wiggle".into(),
        duration: 1.0,
        channels: vec![Channel {
            node: 1,
            property: Property::Rotation,
            interpolation: Interpolation::Step,
            times: vec![0.0],
            values: vec![0.0, 0.0, 0.0, 1.0],
        }],
    });
    assert!(matches!(add_animations(&mut target, &library), Err(RetargetError::NoMatch)));
    assert!(target.animations.is_empty());
}

#[test]
fn malformed_clips_dont_panic() {
    let mut library = humanoid(mixamo, 1.0, 0.0, 0.0, false);
    let mut bad = clip(&library, mixamo);
    bad.channels[0].values.truncate(5);
    bad.channels[1].times.clear();
    bad.channels[2].node = 9999;
    bad.channels[3].values[0] = f32::NAN;
    bad.channels[5].values.truncate(2);
    bad.duration = f32::INFINITY;
    library.animations.push(bad);
    let mut target = humanoid(unreal, 0.9, 0.5, 0.0, true);
    add_animations(&mut target, &library).unwrap();
    let mut same = humanoid(mixamo, 1.0, 0.0, 0.0, false);
    add_animations(&mut same, &library).unwrap();
    let pose = posed(&target, 0, 0.5);
    assert!(pose.world.iter().all(|m| m.is_finite()));
}

/// Scenes a broken or hostile file could produce: humanoid maps naming
/// nodes that don't exist, children out of range, cycles, skins with
/// missing joints, non-finite and degenerate transforms.
#[test]
fn hostile_scenes_dont_panic() {
    let mut library = humanoid(mixamo, 1.0, 0.0, 0.0, false);
    library.animations.push(clip(&library, mixamo));
    let mut target = humanoid(unreal, 1.0, 0.3, 0.0, true);

    // A declared humanoid pointing past the end.
    target.humanoid = Some(Humanoid { bones: vec![("hips".into(), 9999), ("spine".into(), 1)] });
    let _ = add_animations(&mut target, &library);
    target.humanoid = None;

    // Children out of range and a cycle through the root.
    let n = target.nodes.len();
    target.nodes[3].children.push(n + 5);
    target.nodes[4].children.push(0);
    target.skins.push(slerp_scene::Skin { joints: vec![0, 1, n + 7], ..Default::default() });
    let _ = add_animations(&mut target, &library);

    // Zero scale and NaN in the rest pose.
    let mut zero = humanoid(unreal, 1.0, 0.3, 0.0, true);
    zero.nodes[0].transform.scale = Vec3::ZERO;
    zero.nodes[5].transform.rotation = Quat::from_xyzw(f32::NAN, 0.0, 0.0, 1.0);
    zero.nodes[7].transform.translation = Vec3::splat(f32::INFINITY);
    let _ = add_animations(&mut zero, &library);
    let _ = strategy(&zero, &library);

    // A library whose humanoid map is garbage and whose clips are empty.
    let mut odd = humanoid(mixamo, 1.0, 0.0, 0.0, false);
    odd.humanoid = Some(Humanoid { bones: vec![("hips".into(), usize::MAX)] });
    odd.animations.push(Animation { name: String::new(), duration: -1.0, channels: Vec::new() });
    let mut fresh = humanoid(unreal, 1.0, 0.0, 0.0, false);
    assert!(matches!(add_animations(&mut fresh, &odd), Err(RetargetError::NoMatch)));
    let _ = recognise_humanoid(&zero);
}
