//! Rigged models and animations (`crates/rigged`): PMX, PMD, VMD, MD5,
//! IQM, IQE, SMD, PSK/PSA.
#![no_main]
use libfuzzer_sys::fuzz_target;
use slerp_fuzz::{Target, finish, head, split};
use std::sync::LazyLock;

static T: LazyLock<Target> = LazyLock::new(|| Target { name: "rigged", exts: vec!["pmx", "pmd", "vmd", "md5mesh", "md5anim", "iqm", "iqe", "smd", "psk", "pskx", "psa"] });

fuzz_target!(init: slerp_fuzz::init(&T), |data: &[u8]| {
    let Some(input) = split(&T, data) else { return };
    let path = input.file();
    let Some(format) = slerp_rigged::sniff(&path, head(input.body)) else { return };
    if let Ok(asset) = slerp_rigged::load(format, &path, input.body, &input.limits()) {
        finish(&asset, input.flags.size());
    }
});
