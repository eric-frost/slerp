//! Minecraft schematics (`crates/minecraft`): NBT, the four layouts,
//! meshing, and Litematica's stored preview.
#![no_main]
use libfuzzer_sys::fuzz_target;
use slerp_fuzz::{Target, finish_texture, head, scene_cpu, split};
use std::sync::LazyLock;

static T: LazyLock<Target> =
    LazyLock::new(|| Target { name: "minecraft", exts: vec!["schem", "litematic", "nbt", "schematic"] });

fuzz_target!(init: slerp_fuzz::init(&T), |data: &[u8]| {
    let Some(input) = split(&T, data) else { return };
    let path = input.file();
    let Some(format) = slerp_minecraft::sniff(&path, head(input.body)) else { return };
    if let Some(tex) = slerp_minecraft::stored_preview(format, input.body) {
        finish_texture(&tex, input.flags.size());
    }
    let source = slerp_minecraft::TextureSource::Auto;
    if let Ok(scene) = slerp_minecraft::load(format, &path, input.body, &input.limits(), &source) {
        scene_cpu(&scene);
    }
});
