//! Binary model and point-cloud formats of `crates/models`: FBX (and its
//! header thumbnail), 3DS, 3MF (and its package picture), E57, LAS/LAZ,
//! PLY, STL, MagicaVoxel.
#![no_main]
use libfuzzer_sys::fuzz_target;
use slerp_fuzz::{Target, finish, finish_texture, head, split};
use std::sync::LazyLock;

static T: LazyLock<Target> = LazyLock::new(|| Target { name: "models", exts: vec!["fbx", "3ds", "3mf", "e57", "las", "laz", "ply", "stl", "vox"] });

fuzz_target!(init: slerp_fuzz::init(&T), |data: &[u8]| {
    let Some(input) = split(&T, data) else { return };
    let path = input.file();
    let Some(format) = slerp_models::sniff(&path, head(input.body)) else { return };
    let limits = input.limits();
    if let Ok(Some(tex)) = slerp_models::saved_picture(format, &path, &limits) {
        finish_texture(&tex, input.flags.size());
    }
    if let Ok(asset) = slerp_models::load(format, &path, input.body, &limits) {
        finish(&asset, input.flags.size());
    }
});
