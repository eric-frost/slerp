//! Game archives (`crates/archive`): Doom WADs, PK3/PK7, PAK, WAD2/WAD3,
//! Godot PCK, VPK, GRP and Unity packages, listed and every entry read.
#![no_main]
use libfuzzer_sys::fuzz_target;
use slerp_fuzz::{Target, split};
use std::sync::LazyLock;
use std::time::Duration;

static T: LazyLock<Target> = LazyLock::new(|| Target {
    name: "archive",
    exts: vec!["wad", "pk3", "pk7", "pak", "pck", "vpk", "grp", "unitypackage", "zip", "pk4"],
});

fuzz_target!(init: slerp_fuzz::init(&T), |data: &[u8]| {
    let Some(input) = split(&T, data) else { return };
    let path = input.file();
    if input.flags.0 & 8 != 0 {
        let _ = slerp_archive::unity::pictures(&path, 4, Duration::from_secs(2));
    }
    let Ok(archive) = slerp_archive::Archive::open(&path) else { return };
    let _ = archive.groups();
    for (i, entry) in archive.entries().iter().enumerate().take(64) {
        let _ = archive.find(&entry.path);
        if i % 2 == 0 {
            let _ = archive.read(entry);
        } else {
            let _ = archive.read_head(entry, 64);
        }
    }
    let wanted: Vec<usize> = (0..archive.entries().len().min(16)).collect();
    let _ = archive.read_many(&wanted);
});
