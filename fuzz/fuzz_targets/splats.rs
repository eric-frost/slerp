//! Gaussian splats (`crates/splats`): 3DGS and compressed PLY, `.splat`,
//! SPZ, SOG; streamed from a file and from memory.
#![no_main]
use libfuzzer_sys::fuzz_target;
use slerp_fuzz::{Target, head, scene_cpu, split};
use std::sync::LazyLock;

static T: LazyLock<Target> =
    LazyLock::new(|| Target { name: "splats", exts: vec!["ply", "splat", "spz", "sog", "compressed.ply"] });

fuzz_target!(init: slerp_fuzz::init(&T), |data: &[u8]| {
    let Some(input) = split(&T, data) else { return };
    let path = input.file();
    let Some(format) = slerp_splats::sniff(&path, head(input.body)) else { return };
    let limits = slerp_scene::Limits { max_splats: 50_000, ..input.limits() };
    let scene = if input.flags.0 & 8 != 0 {
        slerp_splats::load_bytes(format, input.body, &limits)
    } else {
        slerp_splats::load(format, &path, &limits)
    };
    if let Ok(scene) = scene {
        scene_cpu(&scene);
    }
});
