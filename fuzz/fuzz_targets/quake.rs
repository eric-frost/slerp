//! Quake, Half-Life, Doom and Source files (`crates/quake`): BSP, MDL, MD2,
//! MD3, WADs, PK3/PK7, sprites, maps, VMF, `.uasset`, each Doom map and
//! Source skin as a part.
#![no_main]
use libfuzzer_sys::fuzz_target;
use slerp_fuzz::{Target, finish, head, split};
use std::sync::LazyLock;

static T: LazyLock<Target> = LazyLock::new(|| Target {
    name: "quake",
    exts: vec!["bsp", "mdl", "md2", "md3", "wad", "pk3", "pk7", "spr", "sp2", "map", "vmf", "uasset", "lmp"],
});

fuzz_target!(init: slerp_fuzz::init(&T), |data: &[u8]| {
    let Some(input) = split(&T, data) else { return };
    let path = input.file();
    let Some(format) = slerp_quake::sniff(&path, head(input.body)) else { return };
    let limits = input.limits();
    let size = input.flags.size();
    if let Ok(asset) = slerp_quake::load_path(format, &path, &limits) {
        finish(&asset, size);
    }
    // One part (a Doom map, a Source skin), picked by the flags' high bits.
    let parts = slerp_quake::parts_path(format, &path);
    if let Some(part) = parts.get((input.flags.0 >> 4) as usize % parts.len().max(1))
        && let Ok(asset) = slerp_quake::load_part_path(format, &path, &limits, part)
    {
        finish(&asset, size);
    }
});
