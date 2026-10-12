//! Retargeting real libraries (the Universal Animation Library, VRM
//! animations, Mixamo clips) onto real characters (chalk's MetaHuman, VRM
//! 0.x and 1.0 avatars, Mixamo characters), checking what a viewer would
//! show: feet stay on the ground during walks, hands hang clear of the
//! hips at idle, and no bone stretches.

mod common;

use common::*;
use slerp_retarget::{RetargetError, Strategy, add_animations, recognise_humanoid, strategy};
use slerp_scene::glam::Vec3;
use slerp_scene::{Pose, Scene};
use std::path::PathBuf;

/// What a posed clip looks like, measured over a whole clip.
#[derive(Debug)]
struct Measure {
    /// The highest the lower foot gets above its rest height, as a share
    /// of hip height: near 0 when a foot is always planted.
    planted: f32,
    /// The deepest a foot or toe joint sinks below its rest height, as a
    /// share of hip height.
    sink: f32,
    /// The closest a wrist comes to the body's centre line, sideways, as a
    /// multiple of the distance from the hips to a hip joint.
    hands: f32,
    /// The largest change in any bone's length, relative to its length.
    stretch: f32,
}

fn measure(scene: &Scene, anim: usize) -> Measure {
    let rest = Pose::rest(scene);
    let bone = |b: &str| node(scene, b);
    let (hips, lleg, rleg) = (bone("hips"), bone("leftUpperLeg"), bone("rightUpperLeg"));
    let hip_height = pos(&rest, hips).y;
    let half_width = (pos(&rest, lleg) - pos(&rest, rleg)).length() / 2.0;
    let feet: Vec<usize> = ["leftFoot", "rightFoot", "leftToes", "rightToes"]
        .iter()
        .filter_map(|b| scene.humanoid.clone().or_else(|| recognise_humanoid(scene)).and_then(|h| h.node(b)))
        .collect();
    let hands = [bone("leftHand"), bone("rightHand")];
    // Every joint below the hips, with its parent; the hips' own offset
    // from its parent is the travel, which may change.
    let parents = {
        let mut p = vec![None; scene.nodes.len()];
        for (i, n) in scene.nodes.iter().enumerate() {
            for &c in &n.children {
                p[c] = Some(i);
            }
        }
        p
    };
    let below_hips = |mut j: usize| {
        while let Some(p) = parents[j] {
            if p == hips {
                return true;
            }
            j = p;
        }
        false
    };
    let bones: Vec<(usize, usize)> =
        joints(scene).into_iter().filter(|&j| below_hips(j)).filter_map(|j| Some((j, parents[j]?))).collect();
    let length = |p: &Pose, (j, q): (usize, usize)| (pos(p, j) - pos(p, q)).length();

    let duration = scene.animations[anim].duration;
    let mut m = Measure { planted: 0.0, sink: 0.0, hands: f32::MAX, stretch: 0.0 };
    for k in 0..=40 {
        let pose = posed(scene, anim, duration * k as f32 / 40.0);
        let rise: Vec<f32> = feet.iter().map(|&f| (pos(&pose, f).y - pos(&rest, f).y) / hip_height).collect();
        m.planted = m.planted.max(rise.iter().copied().fold(f32::MAX, f32::min));
        m.sink = m.sink.min(rise.iter().copied().fold(0.0, f32::min));
        let centre = pos(&pose, hips);
        let across = (pos(&pose, lleg) - pos(&pose, rleg)).normalize();
        for h in hands {
            m.hands = m.hands.min((pos(&pose, h) - centre).dot(across).abs() / half_width);
        }
        for &b in &bones {
            let (now, then) = (length(&pose, b), length(&rest, b));
            if then > 1e-4 {
                m.stretch = m.stretch.max((now - then).abs() / then);
            }
        }
    }
    m
}

fn vrm(name: &str) -> PathBuf {
    assets().join("vrm").join(name)
}

fn three(name: &str) -> PathBuf {
    assets().join("gltf/three").join(name)
}

/// Retargets `library` onto `target`, checks the strategy, and measures the
/// added clip that was called `clip` in the library.
fn retarget(target: &std::path::Path, library: &std::path::Path, expect: Strategy, clip: &str) -> Option<Measure> {
    let (mut scene, library) = (load(target)?, load(library)?);
    assert_eq!(strategy(&scene, &library).unwrap(), expect);
    let before = scene.animations.len();
    let added = add_animations(&mut scene, &library).unwrap();
    assert_eq!(added.len(), library.animations.len());
    let index = (before..scene.animations.len())
        .find(|&i| library.animations[i - before].name == clip)
        .unwrap_or_else(|| panic!("no clip {clip}"));
    let m = measure(&scene, index);
    println!(
        "{:<40} {clip:<22} on {:<34} planted {:.3} sink {:.3} hands {:.2} stretch {:.1e}",
        library.name,
        target.file_name().unwrap().to_string_lossy(),
        m.planted,
        m.sink,
        m.hands,
        m.stretch
    );
    assert!(m.stretch < 1e-3, "a bone stretched by {}", m.stretch);
    Some(m)
}

/// A walk keeps a foot on the ground (within 5% of hip height, about 5 cm
/// on an adult) and never pushes one through it.
fn walks(m: Option<Measure>) {
    let Some(m) = m else { return };
    assert!(m.planted < 0.05, "both feet off the ground by {} hip heights", m.planted);
    assert!(m.sink > -0.05, "a foot sank {} hip heights", m.sink);
}

/// At idle the wrists hang outside the hip joints.
fn idles(m: Option<Measure>) {
    let Some(m) = m else { return };
    assert!(m.hands > 1.2, "a wrist came within {} hip half-widths of the centre line", m.hands);
    assert!(m.planted < 0.05 && m.sink > -0.05, "feet: {m:?}");
}

#[test]
fn ual_on_metahuman() {
    walks(retarget(&metahuman(3), &ual1_glb(), Strategy::Humanoid, "Walk_Loop"));
    idles(retarget(&metahuman(3), &ual1_glb(), Strategy::Humanoid, "Idle_Loop"));
}

#[test]
fn ual_on_vrm() {
    for avatar in ["AvatarSample_A.vrm", "AvatarSample_B.vrm", "Seed-san.vrm", "AliciaSolid_vrm-0.51.vrm"] {
        walks(retarget(&vrm(avatar), &ual1_glb(), Strategy::Humanoid, "Walk_Loop"));
        idles(retarget(&vrm(avatar), &ual1_glb(), Strategy::Humanoid, "Idle_Loop"));
    }
}

#[test]
fn ual_fbx_on_vrm() {
    walks(retarget(&vrm("AvatarSample_B.vrm"), &ual1_fbx(), Strategy::Humanoid, "Walk_Loop"));
}

#[test]
fn vrma_on_vrm_and_metahuman() {
    let anim = assets().join("vrm/anim");
    for target in [vrm("AvatarSample_A.vrm"), vrm("AvatarSample_B.vrm"), vrm("Seed-san.vrm"), metahuman(3)] {
        idles(retarget(&target, &anim.join("idle_loop.vrma"), Strategy::Humanoid, "animation"));
        retarget(&target, &anim.join("VRMA_02.vrma"), Strategy::Humanoid, "animation");
    }
}

#[test]
fn mixamo() {
    for target in [three("Soldier.glb"), three("Michelle.glb"), three("readyplayer.me.glb"), metahuman(3)] {
        walks(retarget(&target, &three("Xbot.glb"), Strategy::Humanoid, "walk"));
        idles(retarget(&target, &three("Xbot.glb"), Strategy::Humanoid, "idle"));
    }
    walks(retarget(&vrm("AvatarSample_B.vrm"), &three("Soldier.glb"), Strategy::Humanoid, "Walk"));
    walks(retarget(&three("Xbot.glb"), &ual1_glb(), Strategy::Humanoid, "Walk_Loop"));
    // The same character's clips play as they are.
    walks(retarget(&three("Xbot.glb"), &three("Xbot.glb"), Strategy::SameNames, "walk"));
}

/// Recognising the skeletons of VRM avatars from their joint names alone
/// gives the bone maps their files declare.
#[test]
fn recognition_agrees_with_vrm_files() {
    let files = [
        "AliciaSolid_vrm-0.51.vrm",
        "AvatarSample_A.vrm",
        "AvatarSample_B.vrm",
        "EmissionMigration_v0.106.0.vrm",
        "Seed-san.vrm",
        "VRM1_Constraint_Twist_Sample.vrm",
        "VRMC_materials_mtoon_UV_Animation_Test.vrm",
        "anim/VRMA_02.vrma",
        "anim/test.vrma",
    ];
    for file in files {
        let Some(scene) = load(&vrm(file)) else { continue };
        let declared = scene.humanoid.clone().expect("VRM files declare their bones");
        let found = recognise_humanoid(&scene).expect("a humanoid");
        // No bone placed differently, and at most one the names don't
        // reveal (Alicia's jaw is "mouth", Seed-san's head "head 1").
        let mut missing = Vec::new();
        for (bone, node) in &declared.bones {
            match found.node(bone) {
                Some(n) => assert_eq!(n, *node, "{file}: {bone}"),
                None => missing.push(bone.clone()),
            }
        }
        println!(
            "{file}: {} of {} bones, missing {missing:?}",
            declared.bones.len() - missing.len(),
            declared.bones.len()
        );
        assert!(missing.len() <= 1, "{file}: missing {missing:?}");
    }
    // And for files without a declared map, the skeletons are recognised.
    for (path, bones) in
        [(metahuman(3), 34), (ual1_glb(), 52), (three("Xbot.glb"), 54), (three("readyplayer.me.glb"), 54)]
    {
        let Some(scene) = load(&path) else { continue };
        assert_eq!(recognise_humanoid(&scene).map(|h| h.bones.len()), Some(bones), "{}", path.display());
    }
}

#[test]
fn errors() {
    let mannequin = PathBuf::from(UAL)
        .join("ual2/Universal Animation Library 2[Standard]/Female Mannequin/Unreal-Godot/Mannequin_F.glb");
    let (Some(mut scene), Some(no_clips), Some(horse)) =
        (load(&metahuman(3)), load(&mannequin), load(&three("Horse.glb")))
    else {
        return;
    };
    let before = scene.animations.len();
    assert!(matches!(add_animations(&mut scene, &no_clips), Err(RetargetError::NoClips)));
    assert!(matches!(add_animations(&mut scene, &horse), Err(RetargetError::NoMatch)));
    assert_eq!(scene.animations.len(), before);
    // The mannequin with the library's clips plays them.
    let (Some(mut mannequin), Some(library)) = (load(&mannequin), load(&ual2_glb())) else { return };
    add_animations(&mut mannequin, &library).unwrap();
    let walk = mannequin.animations.iter().position(|a| a.name == "Zombie_Walk_Fwd_Loop").unwrap();
    let m = measure(&mannequin, walk);
    assert!(m.stretch < 1e-3 && m.planted < 0.05, "{m:?}");
    let _ = Vec3::ZERO;
}
