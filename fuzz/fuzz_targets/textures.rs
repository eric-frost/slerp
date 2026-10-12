//! Textures and science pictures (`crates/textures`): KTX2, KTX, DDS, ASTC,
//! Radiance HDR, PNG, JPEG, WebP stills, TGA, BMP, PCX, FITS, XISF, DPX,
//! Cineon, PAM, farbfeld, JPEG XR; from memory and streamed from a file.
#![no_main]
use libfuzzer_sys::fuzz_target;
use slerp_fuzz::{Target, finish_texture, split};
use slerp_textures::DecodeOptions;
use std::sync::LazyLock;

static T: LazyLock<Target> = LazyLock::new(|| Target {
    name: "textures",
    exts: vec![
        "ktx2", "ktx", "dds", "astc", "hdr", "png", "jpg", "webp", "tga", "bmp", "pcx", "fits", "fit", "fz", "xisf",
        "dpx", "cin", "pam", "ff", "jxr", "basis",
    ],
});

fuzz_target!(init: slerp_fuzz::init(&T), |data: &[u8]| {
    let Some(input) = split(&T, data) else { return };
    let size = input.flags.size();
    let opts = if input.flags.0 & 8 != 0 { DecodeOptions { max_size: Some(1024), ..DecodeOptions::full() } } else { DecodeOptions::preview(size) };
    if input.flags.0 & 16 != 0 {
        let path = input.file();
        if let Ok(tex) = slerp_textures::decode_file(&path, &opts) {
            finish_texture(&tex, size);
        }
        return;
    }
    let format = slerp_textures::sniff(input.body).or_else(|| match input.ext {
        "tga" => Some(slerp_textures::Format::Tga),
        _ => None,
    });
    let Some(format) = format else { return };
    if let Ok(tex) = slerp_textures::decode_as(format, input.body, &opts) {
        finish_texture(&tex, size);
        if slerp_textures::is_compressed(&tex) {
            let _ = slerp_textures::decompress(&tex);
        }
    }
});
