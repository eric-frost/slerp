//! What is inside a file (`slerp_core::contents`): an archive's items, and
//! each item exported in every format it offers and previewed.
#![no_main]
use libfuzzer_sys::fuzz_target;
use slerp_core::contents::Contents;
use slerp_fuzz::{Target, split};
use std::sync::LazyLock;

static T: LazyLock<Target> = LazyLock::new(|| Target {
    name: "contents",
    exts: vec!["wad", "pk3", "pk7", "pak", "pck", "vpk", "grp", "unitypackage", "zip", "pk4", "glb"],
});

fuzz_target!(init: slerp_fuzz::init(&T), |data: &[u8]| {
    let Some(input) = split(&T, data) else { return };
    let path = input.file();
    let Ok(contents) = Contents::open(&path) else { return };
    let _ = slerp_core::contents::list_text(&contents, input.flags.0 & 8 != 0);
    let _ = contents.maps();
    for item in contents.items().iter().take(24) {
        let _ = contents.preview(item.id);
        for &format in item.formats.iter().take(3) {
            let _ = contents.export(item.id, format);
        }
    }
});
