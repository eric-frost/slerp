//! 3ds Max scenes (`crates/max`) and their stored preview
//! (`crates/previews`).
#![no_main]
use libfuzzer_sys::fuzz_target;
use slerp_fuzz::{Target, head, scene_cpu, split};
use std::sync::LazyLock;

static T: LazyLock<Target> = LazyLock::new(|| Target { name: "max", exts: vec!["max"] });

fuzz_target!(init: slerp_fuzz::init(&T), |data: &[u8]| {
    let Some(input) = split(&T, data) else { return };
    let path = input.file();
    if !slerp_max::sniff(&path, head(input.body)) {
        return;
    }
    let limits = input.limits();
    if let Ok(scene) = slerp_max::load(input.body, Some(&path), &limits) {
        scene_cpu(&scene);
    }
    let _ = slerp_previews::read_stored_path(slerp_previews::PreviewFormat::Max, &path, &limits);
});
