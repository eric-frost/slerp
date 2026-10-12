//! The TIFF reader (`crates/tiff`): every page's decode, at full size or
//! reduced.
#![no_main]
use libfuzzer_sys::fuzz_target;
use slerp_fuzz::{Target, split};
use std::sync::LazyLock;

static T: LazyLock<Target> = LazyLock::new(|| Target { name: "tiff", exts: vec!["tif", "tiff", "btf", "ndpi", "svs"] });

fuzz_target!(init: slerp_fuzz::init(&T), |data: &[u8]| {
    let Some(input) = split(&T, data) else { return };
    let path = input.file();
    let Ok(pages) = slerp_tiff::page_count(&path) else { return };
    let options = slerp_tiff::Options {
        min_long_side: if input.flags.0 & 8 != 0 { 64 } else { 0 },
        max_pixels: 1 << 24,
    };
    for page in 0..pages.min(3) {
        let _ = slerp_tiff::decode_page(&path, page, &options);
    }
});
