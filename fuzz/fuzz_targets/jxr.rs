//! JPEG XR (`crates/jxr`): jxrlib (C) in a forked child and the Rust that
//! reads its answer; the child's faults are kept apart.
#![no_main]
use libfuzzer_sys::fuzz_target;
use slerp_fuzz::{Target, isolated, split};
use std::sync::LazyLock;

static T: LazyLock<Target> = LazyLock::new(|| Target { name: "jxr", exts: vec!["jxr", "wdp", "hdp"] });

fuzz_target!(init: slerp_fuzz::init(&T), |data: &[u8]| {
    let Some(input) = split(&T, data) else { return };
    let min = if input.flags.0 & 8 != 0 { 0 } else { input.flags.size() };
    if let Err(e) = slerp_jxr::decode(input.body, min, 64 << 20) {
        isolated(data, &e.to_string());
    }
});
