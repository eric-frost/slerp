//! Game-engine scenes (`crates/engines`): Godot text and binary scenes,
//! Unity scenes and prefabs saved as text.
#![no_main]
use libfuzzer_sys::fuzz_target;
use slerp_fuzz::{Target, finish, head, split};
use std::sync::LazyLock;

static T: LazyLock<Target> = LazyLock::new(|| Target { name: "engines", exts: vec!["tscn", "escn", "scn", "unity", "prefab"] });

fuzz_target!(init: slerp_fuzz::init(&T), |data: &[u8]| {
    let Some(input) = split(&T, data) else { return };
    let path = input.file();
    let Some(format) = slerp_engines::sniff(&path, head(input.body)) else { return };
    if let Ok(asset) = slerp_engines::load(format, &path, input.body, &input.limits()) {
        finish(&asset, input.flags.size());
    }
});
