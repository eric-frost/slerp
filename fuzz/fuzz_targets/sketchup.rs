//! SketchUp models (`crates/sketchup`) and the picture SketchUp stores in
//! them (`crates/previews`).
#![no_main]
use libfuzzer_sys::fuzz_target;
use slerp_fuzz::{Target, scene_cpu, split};
use std::sync::LazyLock;

static T: LazyLock<Target> = LazyLock::new(|| Target { name: "sketchup", exts: vec!["skp"] });

fuzz_target!(init: slerp_fuzz::init(&T), |data: &[u8]| {
    let Some(input) = split(&T, data) else { return };
    if !slerp_sketchup::sniff(input.body) {
        return;
    }
    let limits = input.limits();
    let _ = slerp_sketchup::header_version(input.body);
    if let Ok(scene) = slerp_sketchup::load(input.body, &limits) {
        scene_cpu(&scene);
    }
    let path = input.file();
    let _ = slerp_previews::read_stored_path(slerp_previews::PreviewFormat::SketchUp, &path, &limits);
});
