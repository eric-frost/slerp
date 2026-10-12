//! Materials and USD (`crates/materials`): MaterialX, USD text, binary and
//! zipped, Godot `.tres`, `.sbsar` icons.
#![no_main]
use libfuzzer_sys::fuzz_target;
use slerp_fuzz::{Target, finish, head, split};
use std::sync::LazyLock;

static T: LazyLock<Target> = LazyLock::new(|| Target { name: "materials", exts: vec!["mtlx", "usda", "usdc", "usd", "usdz", "tres", "sbsar"] });

fuzz_target!(init: slerp_fuzz::init(&T), |data: &[u8]| {
    let Some(input) = split(&T, data) else { return };
    let path = input.file();
    let Some(format) = slerp_materials::sniff(&path, head(input.body)) else { return };
    if let Ok(asset) = slerp_materials::load(format, &path, input.body, &input.limits()) {
        finish(&asset, input.flags.size());
    }
});
