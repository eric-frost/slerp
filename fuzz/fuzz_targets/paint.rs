//! Layered paint documents (`crates/paint`): PSD/PSB, XCF, Krita and
//! OpenRaster, the saved picture and the composed layers.
#![no_main]
use libfuzzer_sys::fuzz_target;
use slerp_fuzz::{Target, finish, finish_texture, head, split};
use std::sync::LazyLock;

static T: LazyLock<Target> =
    LazyLock::new(|| Target { name: "paint", exts: vec!["psd", "psb", "xcf", "kra", "ora", "xcf.gz", "xcf.bz2", "krz"] });

fuzz_target!(init: slerp_fuzz::init(&T), |data: &[u8]| {
    let Some(input) = split(&T, data) else { return };
    let path = input.file();
    let Some(format) = slerp_paint::sniff(&path, head(input.body)) else { return };
    let limits = input.limits();
    if let Ok(Some(tex)) = slerp_paint::saved_picture(format, &path, &limits) {
        finish_texture(&tex, input.flags.size());
    }
    if let Ok(asset) = slerp_paint::load(format, &path, &limits) {
        finish(&asset, input.flags.size());
    }
});
