//! The viewer's picture formats (`crates/images`): GIF, APNG, animated
//! WebP, ICO/CUR, PNM, TIFF, JPEG 2000, JPEG XL, AVIF, HEIC, QOI, ICNS, SGI,
//! XPM, XBM, OpenEXR; with the saved pictures (EXR previews).  AVIF and HEIC
//! decode in a forked child, whose faults are kept apart.
#![no_main]
use libfuzzer_sys::fuzz_target;
use slerp_fuzz::{Target, finish, finish_texture, head, note, split};
use std::sync::LazyLock;

static T: LazyLock<Target> = LazyLock::new(|| Target {
    name: "images",
    exts: vec![
        "gif", "apng", "webp", "ico", "cur", "pbm", "pgm", "ppm", "pnm", "tif", "jp2", "j2k", "jxl", "avif", "heic",
        "qoi", "icns", "sgi", "rgb", "xpm", "xbm", "exr", "png",
    ],
});

fuzz_target!(init: slerp_fuzz::init(&T), |data: &[u8]| {
    let Some(input) = split(&T, data) else { return };
    let path = input.file();
    let Some(format) = slerp_images::sniff(&path, head(input.body)) else { return };
    let size = input.flags.size();
    if let Ok(Some(tex)) = slerp_images::saved_picture(format, &path) {
        finish_texture(&tex, size);
    }
    let result = slerp_images::load(format, &path, &input.limits());
    note(data, &result);
    if let Ok(asset) = result {
        finish(&asset, size);
    }
});
