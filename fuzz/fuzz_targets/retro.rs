//! Retro computer pictures (`crates/retro`): Atari ST, C64, Spectrum, CPC,
//! MSX, Apple II, Atari 8-bit, BBC, Amiga IFF, ANSI art, PICO-8, TIC-80.
#![no_main]
use libfuzzer_sys::fuzz_target;
use slerp_fuzz::{Target, finish, head, split};
use std::sync::LazyLock;

static T: LazyLock<Target> = LazyLock::new(|| Target { name: "retro", exts: vec!["pi1", "pi2", "pi3", "pc1", "pc2", "pc3", "neo", "tny", "tn1", "koa", "kla", "art", "dd", "hir", "sc2", "sc5", "sc7", "sc8", "s12", "sr5", "hgr", "dhr", "mic", "gr9", "bbc", "ans", "p8", "tic", "iff", "lbm", "png"] });

fuzz_target!(init: slerp_fuzz::init(&T), |data: &[u8]| {
    let Some(input) = split(&T, data) else { return };
    let path = input.file();
    let Some(format) = slerp_retro::sniff(&path, head(input.body)) else { return };
    if let Ok(asset) = slerp_retro::load(format, &path, input.body, &input.limits()) {
        finish(&asset, input.flags.size());
    }
});
