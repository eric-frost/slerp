//! OpenEXR (`crates/openexr`): the header and parts, and the default
//! layer decoded, through the `exr` crate and Slerp's own luminance-chroma
//! reader.
#![no_main]
use libfuzzer_sys::fuzz_target;
use slerp_fuzz::{Target, split};
use std::sync::LazyLock;

static T: LazyLock<Target> = LazyLock::new(|| Target { name: "openexr", exts: vec!["exr"] });

fuzz_target!(init: slerp_fuzz::init(&T), |data: &[u8]| {
    let Some(input) = split(&T, data) else { return };
    let _ = slerp_openexr::open(std::io::Cursor::new(input.body));
    let _ = slerp_openexr::decode_default(input.body, Some(input.flags.size()), 1 << 24);
});
