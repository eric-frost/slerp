//! Maya ASCII and binary scenes (`crates/maya`).
#![no_main]
use libfuzzer_sys::fuzz_target;
use slerp_fuzz::{Target, head, scene_cpu, split};
use std::sync::LazyLock;

static T: LazyLock<Target> = LazyLock::new(|| Target { name: "maya", exts: vec!["ma", "mb"] });

fuzz_target!(init: slerp_fuzz::init(&T), |data: &[u8]| {
    let Some(input) = split(&T, data) else { return };
    if slerp_maya::sniff(head(input.body)).is_none() {
        return;
    }
    let path = input.file();
    if let Ok(scene) = slerp_maya::load(input.body, Some(&path), &input.limits()) {
        scene_cpu(&scene);
    }
});
