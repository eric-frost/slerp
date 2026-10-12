//! AutoCAD DXF (text and binary) and Rhino `.3dm` (`crates/cad`): the
//! saved picture, the thumbnail (drawn flat or as a scene) and the viewer's
//! opening.
#![no_main]
use libfuzzer_sys::fuzz_target;
use slerp_fuzz::{Target, finish_texture, head, scene_cpu, split};
use std::sync::LazyLock;

static T: LazyLock<Target> = LazyLock::new(|| Target { name: "cad", exts: vec!["dxf", "3dm"] });

fuzz_target!(init: slerp_fuzz::init(&T), |data: &[u8]| {
    let Some(input) = split(&T, data) else { return };
    let path = input.file();
    let Some(format) = slerp_cad::sniff(&path, head(input.body)) else { return };
    let limits = input.limits();
    let size = input.flags.size();
    if let Ok(Some(tex)) = slerp_cad::saved_picture(format, &path, &limits) {
        finish_texture(&tex, size);
    }
    match slerp_cad::thumbnail(format, &path, size, &limits) {
        Ok(slerp_cad::Thumb::Picture(tex)) => finish_texture(&tex, size),
        Ok(slerp_cad::Thumb::Scene(scene)) => scene_cpu(&scene),
        _ => {}
    }
    if input.flags.0 & 8 != 0
        && let Ok(asset) = slerp_cad::load_path(format, &path, &limits)
    {
        slerp_fuzz::finish(&asset, size);
    }
});
