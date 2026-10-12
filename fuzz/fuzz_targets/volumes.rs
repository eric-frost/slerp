//! Scientific arrays and medical volumes (`crates/volumes`): NumPy, NIfTI,
//! Analyze, MetaImage, NRRD, NetCDF, HDF5 (and MATLAB 7.3), TIFF stacks and
//! slides; the middle plane drawn, each part listed and one loaded.
#![no_main]
use libfuzzer_sys::fuzz_target;
use slerp_fuzz::{Target, finish, head, split};
use std::sync::LazyLock;

static T: LazyLock<Target> = LazyLock::new(|| Target {
    name: "volumes",
    exts: vec![
        "npy", "npz", "nii", "nii.gz", "hdr", "mha", "mhd", "nrrd", "nhdr", "nc", "h5", "mat", "ome.tif", "tif", "svs",
        "scn", "btf",
    ],
});

fuzz_target!(init: slerp_fuzz::init(&T), |data: &[u8]| {
    let Some(input) = split(&T, data) else { return };
    let path = input.file();
    let Some(format) = slerp_volumes::sniff(&path, head(input.body)) else { return };
    let limits = input.limits();
    if let Ok(asset) = slerp_volumes::load_path(format, &path, &limits) {
        finish(&asset, input.flags.size());
    }
    let parts = slerp_volumes::parts_path(format, &path);
    if let Some(part) = parts.get((input.flags.0 >> 4) as usize % parts.len().max(1))
        && let Ok(asset) = slerp_volumes::load_part_path(format, &path, &limits, part)
    {
        finish(&asset, input.flags.size());
    }
});
