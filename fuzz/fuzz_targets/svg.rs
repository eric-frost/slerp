//! SVG and SVGZ (`crates/svg`): usvg and resvg draw in a forked child; its
//! faults are kept apart.
#![no_main]
use libfuzzer_sys::fuzz_target;
use slerp_fuzz::{Target, finish_texture, head, note, split};
use std::sync::LazyLock;

static T: LazyLock<Target> = LazyLock::new(|| Target { name: "svg", exts: vec!["svg", "svgz"] });

fuzz_target!(init: slerp_fuzz::init(&T), |data: &[u8]| {
    let Some(input) = split(&T, data) else { return };
    let path = input.file();
    if !slerp_svg::is_svg(&path, head(input.body)) {
        return;
    }
    let _ = slerp_svg::read(&path);
    let result = slerp_svg::picture(&path, &input.limits());
    note(data, &result);
    if let Ok(tex) = result {
        finish_texture(&tex, input.flags.size());
    }
});
