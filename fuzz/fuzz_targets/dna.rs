//! MetaHuman DNA (`crates/dna`): the header and section index checked in
//! Rust, then openriglogic (C++) in a forked child, whose faults are kept
//! apart.
#![no_main]
use libfuzzer_sys::fuzz_target;
use slerp_fuzz::{Target, head, note, scene_cpu, split};
use std::sync::LazyLock;

static T: LazyLock<Target> = LazyLock::new(|| Target { name: "dna", exts: vec!["dna"] });

fuzz_target!(init: slerp_fuzz::init(&T), |data: &[u8]| {
    let Some(input) = split(&T, data) else { return };
    if !slerp_dna::sniff(head(input.body)) {
        return;
    }
    let path = input.file();
    let result = slerp_dna::load(&path, &input.limits());
    note(data, &result);
    if let Ok(scene) = result {
        scene_cpu(&scene);
    }
});
