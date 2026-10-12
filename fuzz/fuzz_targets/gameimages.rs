//! Game textures (`crates/gameimages`): XNB with LZX and LZ4, BLP, PVR,
//! PKM, Build ART.
#![no_main]
use libfuzzer_sys::fuzz_target;
use slerp_fuzz::{Target, finish, head, split};
use std::sync::LazyLock;

static T: LazyLock<Target> = LazyLock::new(|| Target { name: "gameimages", exts: vec!["xnb", "blp", "pvr", "pkm", "art"] });

fuzz_target!(init: slerp_fuzz::init(&T), |data: &[u8]| {
    let Some(input) = split(&T, data) else { return };
    let path = input.file();
    let Some(format) = slerp_gameimages::sniff(&path, head(input.body)) else { return };
    if let Ok(asset) = slerp_gameimages::load(format, &path, input.body, &input.limits()) {
        finish(&asset, input.flags.size());
    }
});
