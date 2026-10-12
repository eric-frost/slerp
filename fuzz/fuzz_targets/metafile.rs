//! Windows and Macintosh metafiles (`crates/metafile`): WMF, EMF with EMF+,
//! their gzip forms, PICT, replayed on the CPU.
#![no_main]
use libfuzzer_sys::fuzz_target;
use slerp_fuzz::{Target, finish, head, split};
use std::sync::LazyLock;

static T: LazyLock<Target> =
    LazyLock::new(|| Target { name: "metafile", exts: vec!["wmf", "emf", "wmz", "emz", "pict", "pct"] });

fuzz_target!(init: slerp_fuzz::init(&T), |data: &[u8]| {
    let Some(input) = split(&T, data) else { return };
    let path = input.file();
    let Some(format) = slerp_metafile::sniff(&path, head(input.body)) else { return };
    if let Ok(asset) = slerp_metafile::load_path(format, &path, &input.limits()) {
        finish(&asset, input.flags.size());
    }
});
