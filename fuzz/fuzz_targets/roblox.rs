//! Roblox places and models, binary and XML (`crates/roblox`).
#![no_main]
use libfuzzer_sys::fuzz_target;
use slerp_fuzz::{Target, head, scene_cpu, split};
use std::sync::LazyLock;

static T: LazyLock<Target> = LazyLock::new(|| Target { name: "roblox", exts: vec!["rbxl", "rbxm", "rbxlx", "rbxmx"] });

fuzz_target!(init: slerp_fuzz::init(&T), |data: &[u8]| {
    let Some(input) = split(&T, data) else { return };
    let path = input.file();
    let Some(format) = slerp_roblox::sniff(&path, head(input.body)) else { return };
    if let Ok(scene) = slerp_roblox::load(format, &path, input.body, &input.limits()) {
        scene_cpu(&scene);
    }
});
