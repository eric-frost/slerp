//! Colour palettes (`slerp_previews::palette`): ASE, ACO, ACT, GPL, KPL,
//! JASC and RIFF PAL, hex lists, Procreate swatches.
#![no_main]
use libfuzzer_sys::fuzz_target;
use slerp_fuzz::{Target, head, split};
use slerp_previews::palette;
use std::sync::LazyLock;

static T: LazyLock<Target> = LazyLock::new(|| Target {
    name: "palette",
    exts: vec!["ase", "aco", "act", "gpl", "kpl", "pal", "hex", "swatches", "txt"],
});

fuzz_target!(init: slerp_fuzz::init(&T), |data: &[u8]| {
    let Some(input) = split(&T, data) else { return };
    let path = input.file();
    let Some(format) = palette::sniff(&path, head(input.body)) else { return };
    if let Ok(p) = palette::read_path(format, &path, &input.limits()) {
        palette::thumbnail(&p, input.flags.size());
        palette::picture(&p, 512);
    }
});
