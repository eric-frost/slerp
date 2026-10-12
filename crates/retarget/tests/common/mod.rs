//! Loading the real characters and animation libraries the tests use.
//! They live outside the repository (chalk's files and the downloaded test
//! assets), so a test whose files are missing says so and passes.

#![allow(dead_code)]

use slerp_scene::glam::Vec3;
use slerp_scene::{Asset, Limits, Pose, Scene};
use std::path::{Path, PathBuf};

/// A path in a checkout of chalk (a game whose files some tests read) in the
/// folder beside this repository's; tests skip the files that aren't there.
macro_rules! chalk {
    ($path:literal) => {
        concat!(env!("CARGO_MANIFEST_DIR"), "/../../../chalk/", $path)
    };
}

pub const CHALK_HUMANS: &str = chalk!("html/demos/humans");
pub const UAL: &str = chalk!("tools/humans/vendor/ual");

pub fn assets() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../local/assets")
}

pub fn ual1_glb() -> PathBuf {
    Path::new(UAL).join("ual1/Universal Animation Library[Standard]/Unreal-Godot/UAL1_Standard.glb")
}

pub fn ual2_glb() -> PathBuf {
    Path::new(UAL).join("ual2/Universal Animation Library 2[Standard]/Unreal-Godot/UAL2_Standard.glb")
}

pub fn ual1_fbx() -> PathBuf {
    Path::new(UAL).join("ual1/Universal Animation Library[Standard]/Unity/UAL1_Standard.fbx")
}

pub fn metahuman(lod: u32) -> PathBuf {
    Path::new(CHALK_HUMANS).join(format!("tate-tee-lod{lod}.glb"))
}

/// Loads a glTF, GLB, VRM, VRMA or FBX file with everything the viewer
/// loads, or None (with a note) when it isn't there.
pub fn load(path: &Path) -> Option<Scene> {
    let Ok(bytes) = std::fs::read(path) else {
        eprintln!("skipped: {} is missing", path.display());
        return None;
    };
    let limits = Limits::viewer();
    let head = &bytes[..bytes.len().min(64)];
    let asset = match slerp_models::sniff(path, head) {
        Some(format) => slerp_models::load(format, path, &bytes, &limits),
        None => slerp_gltf::load(path, &bytes, &limits),
    };
    match asset {
        Ok(Asset::Scene(s)) => Some(s),
        Ok(_) => panic!("{} isn't a scene", path.display()),
        Err(e) => panic!("{} didn't load: {e}", path.display()),
    }
}

pub fn clip(scene: &Scene, name: &str) -> usize {
    scene.animations.iter().position(|a| a.name == name).unwrap_or_else(|| panic!("no clip {name}"))
}

pub fn node(scene: &Scene, bone: &str) -> usize {
    let h = scene.humanoid.clone().or_else(|| slerp_retarget::recognise_humanoid(scene)).expect("a humanoid");
    h.node(bone).unwrap_or_else(|| panic!("no {bone}"))
}

/// The scene posed by clip `anim` at `time`.
pub fn posed(scene: &Scene, anim: usize, time: f32) -> Pose {
    let mut pose = Pose::rest(scene);
    pose.sample(scene, anim, time);
    pose.update_world(scene);
    pose
}

pub fn pos(pose: &Pose, node: usize) -> Vec3 {
    pose.world[node].w_axis.truncate()
}

/// Nodes that are joints of a skin.
pub fn joints(scene: &Scene) -> Vec<usize> {
    let mut out: Vec<usize> = scene.skins.iter().flat_map(|s| s.joints.iter().copied()).collect();
    out.sort_unstable();
    out.dedup();
    out
}
