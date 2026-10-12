//! Blender files (`crates/blend`): the stored preview and the scene through
//! the file's own SDNA, gzip and Zstandard included.
#![no_main]
use libfuzzer_sys::fuzz_target;
use slerp_fuzz::{Target, finish_texture, head, scene_cpu, split};
use std::sync::LazyLock;

static T: LazyLock<Target> = LazyLock::new(|| Target { name: "blend", exts: vec!["blend", "blend1"] });

fuzz_target!(init: slerp_fuzz::init(&T), |data: &[u8]| {
    let Some(input) = split(&T, data) else { return };
    let path = input.file();
    if !slerp_blend::sniff(&path, head(input.body)) {
        return;
    }
    if let Ok(Some(tex)) = slerp_blend::preview(input.body) {
        finish_texture(&tex, input.flags.size());
    }
    if let Ok(scene) = slerp_blend::load(&path, input.body, &input.limits()) {
        scene_cpu(&scene);
    }
});
