//! Content sniffing for every format (`slerp_core::sniff`), with every
//! extension the format table knows, and the format each kind maps to.
#![no_main]
use libfuzzer_sys::fuzz_target;
use slerp_fuzz::{Target, split};
use std::path::Path;
use std::sync::LazyLock;

static T: LazyLock<Target> = LazyLock::new(|| Target { name: "core_sniff", exts: slerp_fuzz::all_extensions() });

fuzz_target!(init: slerp_fuzz::init(&T), |data: &[u8]| {
    let Some(input) = split(&T, data) else { return };
    let name = format!("in.{}", input.ext);
    let path = Path::new(&name);
    if let Some(kind) = slerp_core::sniff(path, input.body) {
        let _ = kind.format(path);
        let _ = kind.usually_3d();
    }
});
