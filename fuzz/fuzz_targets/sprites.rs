//! Sprites and maps (`crates/sprites`): Aseprite, Tiled, LDtk, Piskel, Pyxel
//! Edit, sprite-sheet descriptors.
#![no_main]
use libfuzzer_sys::fuzz_target;
use slerp_fuzz::{Target, finish, head, split};
use std::sync::LazyLock;

static T: LazyLock<Target> = LazyLock::new(|| Target { name: "sprites", exts: vec!["aseprite", "ase", "tmx", "tmj", "ldtk", "ldtkl", "piskel", "pyxel", "atlas", "tpsheet", "json"] });

fuzz_target!(init: slerp_fuzz::init(&T), |data: &[u8]| {
    let Some(input) = split(&T, data) else { return };
    let path = input.file();
    let Some(format) = slerp_sprites::sniff(&path, head(input.body)) else { return };
    let limits = input.limits();
    if let Ok(asset) = slerp_sprites::load(format, &path, input.body, &limits) {
        finish(&asset, input.flags.size());
    }
    // One part (an LDtk level), picked by the flags' high bits.
    let parts = slerp_sprites::parts(format, &path);
    if let Some(part) = parts.get((input.flags.0 >> 4) as usize % parts.len().max(1))
        && let Ok(asset) = slerp_sprites::load_part(format, &path, &limits, part)
    {
        finish(&asset, input.flags.size());
    }
});
