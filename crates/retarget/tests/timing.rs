//! How long retargeting takes, in release mode:
//! `cargo test --release -p slerp-retarget --test timing -- --ignored --nocapture`.
//! Both ways of adding clips bake the version with foot IK and the one
//! without, so switching foot IK off saves only the sorting out.

mod common;

use common::*;
use slerp_retarget::{Options, Strategy, add_animations_with, strategy};
use slerp_scene::Scene;
use std::path::Path;
use std::time::Instant;

/// Adds `library`'s first `clips` clips to `target` five times, with foot
/// IK and without, and prints the best times.
fn time(target: &Path, library: &Path, clips: usize, expect: Strategy) {
    for foot_ik in [true, false] {
        time_with(target, library, clips, expect, Options { foot_ik });
    }
}

fn time_with(target: &Path, library: &Path, clips: usize, expect: Strategy, options: Options) {
    let (Some(scene), Some(mut library)) = (load(target), load(library)) else { return };
    library.animations.truncate(clips);
    assert_eq!(strategy(&scene, &library).unwrap(), expect);
    let joints = scene.skins.iter().map(|s| s.joints.len()).max().unwrap_or(0);
    let keys: usize = library.animations.iter().flat_map(|a| &a.channels).map(|c| c.times.len()).sum();
    let mut best = f64::MAX;
    for _ in 0..5 {
        let mut copy = Scene {
            nodes: scene.nodes.clone(),
            roots: scene.roots.clone(),
            skins: scene.skins.clone(),
            ..Scene::default()
        };
        copy.humanoid = scene.humanoid.clone();
        let start = Instant::now();
        add_animations_with(&mut copy, &library, options).unwrap();
        best = best.min(start.elapsed().as_secs_f64() * 1000.0);
    }
    println!(
        "{} clips ({keys} keys) from {} onto {} ({joints} joints, {expect:?}, foot IK {}): {best:.1} ms",
        library.animations.len(),
        library.name,
        target.file_name().unwrap().to_string_lossy(),
        if options.foot_ik { "on" } else { "off" }
    );
}

#[test]
#[ignore]
fn timing() {
    time(&metahuman(0), &ual1_glb(), 32, Strategy::Humanoid);
    time(&metahuman(0), &ual1_fbx(), 32, Strategy::Humanoid);
    time(&metahuman(3), &metahuman(0), 32, Strategy::SameNames);
    time(&assets().join("vrm/AvatarSample_B.vrm"), &metahuman(0), 32, Strategy::Humanoid);
}
