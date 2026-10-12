//! DICOM medical images (`crates/dicom`): the data set, a frame through
//! each transfer syntax's decoder, DICOMDIR.
#![no_main]
use libfuzzer_sys::fuzz_target;
use slerp_fuzz::{Target, finish, head, split};
use std::sync::LazyLock;

static T: LazyLock<Target> = LazyLock::new(|| Target { name: "dicom", exts: vec!["dcm", "dicom", "dicomdir"] });

fuzz_target!(init: slerp_fuzz::init(&T), |data: &[u8]| {
    let Some(input) = split(&T, data) else { return };
    let path = input.file();
    if !slerp_dicom::sniff(&path, head(input.body)) {
        return;
    }
    if let Ok(asset) = slerp_dicom::load_path(&path, &input.limits()) {
        finish(&asset, input.flags.size());
    }
});
