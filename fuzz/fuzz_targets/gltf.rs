//! glTF, GLB, VRM and Draco (`crates/gltf`): the scene and VRM's portrait.
#![no_main]
use libfuzzer_sys::fuzz_target;
use slerp_fuzz::{Target, finish, finish_texture, head, split};
use std::sync::LazyLock;

static T: LazyLock<Target> = LazyLock::new(|| Target { name: "gltf", exts: vec!["glb", "gltf", "vrm", "vrma", "drc"] });

fuzz_target!(init: slerp_fuzz::init(&T), |data: &[u8]| {
    let Some(input) = split(&T, data) else { return };
    let path = input.file();
    let head = head(input.body);
    let limits = input.limits();
    let size = input.flags.size();
    if slerp_gltf::sniff(&path, head) {
        if let Ok(Some(tex)) = slerp_gltf::saved_picture(&path, &limits) {
            finish_texture(&tex, size);
        }
        if let Ok(asset) = slerp_gltf::load(&path, input.body, &limits) {
            finish(&asset, size);
        }
    } else if slerp_gltf::sniff_drc(head)
        && let Ok(asset) = slerp_gltf::load_drc(&path, input.body, &limits)
    {
        finish(&asset, size);
    }
});
