//! The reusable entry points on files written by the volume crate's
//! `tools/make-tiff.py` (skipped when they are missing).

use slerp_tiff::{Options, Sample, decode_page, page_count};
use std::path::{Path, PathBuf};

fn dir() -> Option<PathBuf> {
    let d = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../local/assets/volumes/tiff-generated");
    d.join("reference.json").exists().then_some(d)
}

#[test]
fn pages_are_counted_and_decoded_one_at_a_time() {
    let Some(d) = dir() else { return };
    assert_eq!(page_count(&d.join("pages_mixed.tif")).unwrap(), 3);
    let second = decode_page(&d.join("pages_mixed.tif"), 1, &Options::full()).unwrap();
    assert_eq!((second.width, second.height, second.samples), (40, 50, 1));
    let third = decode_page(&d.join("pages_mixed.tif"), 2, &Options::full()).unwrap();
    assert_eq!((third.width, third.height, third.samples), (30, 30, 3));
    assert!(decode_page(&d.join("pages_mixed.tif"), 3, &Options::full()).is_err());
    assert!(page_count(&d.join("reference.json")).is_err());
}

#[test]
fn sixteen_bit_float_bigtiff_and_compressed_pages_decode() {
    let Some(d) = dir() else { return };
    for (name, sample) in [
        ("gray_u16_lzw_pred.tif", Sample::U16),
        ("gray_f32_pred3_deflate.tif", Sample::F32),
        ("gray_f64_pred3_lzw.tif", Sample::F64),
        ("gray_f16.tif", Sample::F16),
        ("bigtiff_u16.tif", Sample::U16),
        ("gray_i16_packbits.tif", Sample::I16),
        ("gray_u32_zstd.tif", Sample::U32),
        ("tiled_lzw_pred_rgb.tif", Sample::U8),
        ("rgb_u16_planar.tif", Sample::U16),
    ] {
        let r = decode_page(&d.join(name), 0, &Options::full()).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(r.sample, sample, "{name}");
        assert_eq!(r.bytes.len(), r.width * r.height * r.samples * sample.size(), "{name}");
    }
}

/// Found by fuzzing: a BigTIFF directory whose link to the next one points
/// near the end of the 64-bit range overflowed the check that it lies in the
/// file.
#[test]
fn next_directory_link_past_the_range_ends_the_chain() {
    let mut f = b"II".to_vec();
    f.extend(43u16.to_le_bytes());
    f.extend(8u16.to_le_bytes());
    f.extend(0u16.to_le_bytes());
    f.extend(16u64.to_le_bytes());
    // One entry (ImageWidth = 1), then the link.
    f.extend(1u64.to_le_bytes());
    f.extend(256u16.to_le_bytes());
    f.extend(4u16.to_le_bytes());
    f.extend(1u64.to_le_bytes());
    f.extend(1u64.to_le_bytes());
    f.extend((u64::MAX - 3).to_le_bytes());
    let path = std::env::temp_dir().join(format!("slerp-tiff-link-{}.tif", std::process::id()));
    std::fs::write(&path, &f).unwrap();
    let pages = page_count(&path);
    std::fs::remove_file(&path).ok();
    assert_eq!(pages, Ok(1));
}
