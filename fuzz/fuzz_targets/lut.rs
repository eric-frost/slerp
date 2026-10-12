//! Colour lookup tables (`crates/lut`): `.cube` and `.3dl`.
#![no_main]
use libfuzzer_sys::fuzz_target;
use slerp_fuzz::{Target, finish, head, split};
use std::sync::LazyLock;

static T: LazyLock<Target> = LazyLock::new(|| Target { name: "lut", exts: vec!["cube", "3dl"] });

fuzz_target!(init: slerp_fuzz::init(&T), |data: &[u8]| {
    let Some(input) = split(&T, data) else { return };
    let path = input.file();
    let Some(format) = slerp_lut::sniff(&path, head(input.body)) else { return };
    if let Ok(asset) = slerp_lut::load(format, &path, input.body, &input.limits()) {
        finish(&asset, input.flags.size());
    }
});
