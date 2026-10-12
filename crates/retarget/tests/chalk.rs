//! Retargeting the Universal Animation Library onto chalk's MetaHuman, and
//! comparing the result with the same clips chalk's own pipeline
//! (`tools/humans/retarget.py`, written independently in Python) baked
//! into the character.
//!
//! Made with chalk's choices, the retarget must reproduce chalk's bakes to
//! a fraction of a millimetre, which checks the shared method.  Slerp's
//! own choices depart from chalk's in four ways (see `Method` in
//! `humanoid.rs`; foot IK is the fourth), and their differences are
//! measured and reported.

mod common;

use common::*;
use slerp_retarget::{RetargetError, Strategy, add_animations, add_animations_as_chalk, strategy};
use slerp_scene::Scene;
use std::path::Path;

/// Clips chalk plays slower than the library does (its `RATES`).
fn rate(name: &str) -> f32 {
    if name == "Idle_Loop" { 0.55 } else { 1.0 }
}

/// World-space joint position errors, in centimetres, between chalk's
/// baked clip and ours over every skin joint from the hips down at eleven
/// moments: (mean, max, joint with the max).
fn compare(scene: &Scene, baked: usize, ours: usize, rate: f32) -> (f32, f32, String) {
    let hips = node(scene, "hips");
    let below = |mut j: usize| loop {
        if j == hips {
            return true;
        }
        match scene.nodes.iter().position(|n| n.children.contains(&j)) {
            Some(p) => j = p,
            None => return false,
        }
    };
    let joints: Vec<usize> = joints(scene).into_iter().filter(|&j| below(j)).collect();
    let duration = scene.animations[ours].duration;
    let (mut sum, mut count, mut worst, mut worst_joint) = (0.0f32, 0usize, 0.0f32, String::new());
    for k in 0..=10 {
        let t = duration * k as f32 / 10.0;
        let (a, b) = (posed(scene, baked, t / rate), posed(scene, ours, t));
        for &j in &joints {
            let e = (pos(&a, j) - pos(&b, j)).length() * 100.0;
            sum += e;
            count += 1;
            if e > worst {
                worst = e;
                worst_joint = format!("{} at {t:.2}s", scene.nodes[j].name);
            }
        }
    }
    (sum / count as f32, worst, worst_joint)
}

/// Each clip chalk baked, paired with ours from the library: (name,
/// chalk's, ours).  Ours are renamed "Walk_Loop (2)"; an armature prefix
/// ("Armature|Walk_Loop") is tolerated, though the FBX loader drops it.
fn pairs(scene: &Scene, baked: &[String]) -> Vec<(String, usize, usize)> {
    baked
        .iter()
        .filter_map(|name| {
            let ours = scene.animations.iter().position(|a| {
                let plain = a.name.strip_suffix(" (2)").unwrap_or(&a.name);
                plain.rsplit('|').next() == Some(name.as_str()) && a.name != *name
            })?;
            Some((name.clone(), clip(scene, name), ours))
        })
        .collect()
}

type Add = fn(&mut Scene, &Scene) -> Result<Vec<String>, RetargetError>;

/// Retargets `library` onto the MetaHuman with `add` and compares every
/// clip chalk also baked.  Returns the mean of the clips' mean errors and
/// the largest error.
fn run(lod: u32, library: &Path, add: Add, label: &str) -> Option<(f32, f32)> {
    let (mut scene, library) = (load(&metahuman(lod))?, load(library)?);
    let baked: Vec<String> = scene.animations.iter().map(|a| a.name.clone()).collect();
    assert_eq!(strategy(&scene, &library).unwrap(), Strategy::Humanoid);
    assert_eq!(add(&mut scene, &library).unwrap().len(), library.animations.len());
    let pairs = pairs(&scene, &baked);
    assert!(!pairs.is_empty(), "no clips in common");
    let (mut sum, mut worst) = (0.0, 0.0f32);
    for (name, baked, ours) in &pairs {
        let (mean, max, at) = compare(&scene, *baked, *ours, rate(name));
        println!("lod{lod} {label:<8} {name:<24} mean {mean:5.2} cm, max {max:5.2} cm ({at})");
        sum += mean;
        worst = worst.max(max);
    }
    let mean = sum / pairs.len() as f32;
    println!("lod{lod} {label} summary: {} clips, mean {mean:.3} cm, max {worst:.2} cm", pairs.len());
    Some((mean, worst))
}

/// Chalk's method reproduces chalk's bakes.  Slerp's differs by what
/// its departures explain: a centimetre or two.
fn check(lod: u32, library: &Path, chalk_max: f32) {
    let Some((mean, max)) = run(lod, library, add_animations_as_chalk, "chalk") else { return };
    assert!(mean < 0.05 && max < chalk_max, "chalk's method: mean {mean} cm, max {max} cm");
    let (ours, _) = run(lod, library, add_animations, "ours").unwrap();
    assert!(ours < 3.0, "Slerp's method: mean {ours} cm");
}

#[test]
fn ual1_glb_like_chalk() {
    check(3, &ual1_glb(), 0.1);
}

#[test]
fn ual2_glb_like_chalk() {
    check(3, &ual2_glb(), 0.1);
}

/// The FBX holds the same clips as the GLB chalk used, except that its
/// `Roll` starts in a slightly different pose (the left wrist 2.7 cm away
/// at 0 s, the same from 0.05 s on), hence the looser maximum.
#[test]
fn ual1_fbx_like_chalk() {
    check(3, &ual1_fbx(), 5.0);
}

/// The full-detail body, with all 342 joints (twist, corrective and finger
/// joints the lower levels drop).
#[test]
fn ual1_glb_lod0_like_chalk() {
    check(0, &ual1_glb(), 0.1);
}
