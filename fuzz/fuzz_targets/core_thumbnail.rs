//! The whole CPU half of a thumbnail (`slerp_core::prepare_thumbnail`):
//! sniffing, the saved picture and its quality check, then the content,
//! for every extension the format table knows.
#![no_main]
use libfuzzer_sys::fuzz_target;
use slerp_fuzz::{Target, note, scene_cpu, split};
use std::sync::LazyLock;

static T: LazyLock<Target> = LazyLock::new(|| Target { name: "core_thumbnail", exts: slerp_fuzz::all_extensions() });

fuzz_target!(init: slerp_fuzz::init(&T), |data: &[u8]| {
    let Some(input) = split(&T, data) else { return };
    let path = input.file();
    let result = slerp_core::prepare_thumbnail(&path, input.flags.size());
    note(data, &result);
    if let Ok(slerp_core::Prepared::Render(scene)) = result {
        scene_cpu(&scene);
    }
});
