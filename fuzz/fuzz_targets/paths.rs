//! Toolpaths and tracks (`crates/paths`): G-code with slicer pictures,
//! Prusa binary G-code, GPX, TCX, FIT, KML/KMZ, GeoJSON.
#![no_main]
use libfuzzer_sys::fuzz_target;
use slerp_fuzz::{Target, finish_texture, head, scene_cpu, split};
use std::sync::LazyLock;

static T: LazyLock<Target> = LazyLock::new(|| Target {
    name: "paths",
    exts: vec!["gcode", "gco", "bgcode", "gpx", "tcx", "fit", "kml", "kmz", "geojson"],
});

fuzz_target!(init: slerp_fuzz::init(&T), |data: &[u8]| {
    let Some(input) = split(&T, data) else { return };
    let path = input.file();
    let Some(format) = slerp_paths::sniff(&path, head(input.body)) else { return };
    let size = input.flags.size();
    if let Ok(Some(tex)) = slerp_paths::saved_picture(format, &path) {
        finish_texture(&tex, size);
    }
    if let Ok(slerp_paths::Thumb::Scene(scene)) = slerp_paths::thumbnail(format, &path, size) {
        scene_cpu(&scene);
    }
    if input.flags.0 & 8 != 0
        && let Ok(opened) = slerp_paths::open(format, &path, &input.limits())
    {
        scene_cpu(&opened.scene);
    }
});
