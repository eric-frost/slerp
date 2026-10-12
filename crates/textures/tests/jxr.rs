//! JPEG XR through the texture API: sniffing, the picture kinds it gives
//! (8-bit sRGB, linear float for HDR), alpha, the reduced-size path and the
//! facts for the inspector.  The pixel-exact checks are in `slerp-jxr`.

use slerp_textures::{DecodeOptions, Format, Pixels, decode_as, decode_file, sniff, thumbnail};
use std::path::{Path, PathBuf};

fn file(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../jxr/tests/data/files").join(name)
}

fn bytes(name: &str) -> Vec<u8> {
    std::fs::read(file(name)).unwrap()
}

#[test]
fn sniffs_and_decodes() {
    let b = bytes("rgb24.jxr");
    assert_eq!(sniff(&b[..32]), Some(Format::Jxr));
    let tex = decode_as(Format::Jxr, &b, &DecodeOptions::full()).unwrap();
    assert_eq!((tex.width(), tex.height()), (40, 24));
    assert!(matches!(tex.base(), Pixels::Rgba8(_)));
    assert_eq!(tex.source.container, "JPEG XR");
    assert!(!tex.has_alpha);
    let find = |k: &str| tex.source.header.iter().find(|(key, _)| key == k).map(|(_, v)| v.clone());
    assert_eq!(find("Pixel format").as_deref(), Some("8-bit RGB"));
    assert_eq!(find("Alpha").as_deref(), Some("no"));
    let from_file = decode_file(&file("rgb24.jxr"), &DecodeOptions::full()).unwrap();
    assert_eq!(from_file.width(), 40);
}

#[test]
fn hdr_pictures_are_linear_floats_and_alpha_is_kept() {
    let tex = decode_as(Format::Jxr, &bytes("rgba_half.jxr"), &DecodeOptions::full()).unwrap();
    assert!(matches!(tex.base(), Pixels::Rgba32F(_)), "{:?}", tex.base());
    assert!(tex.has_alpha);
    let thumb = thumbnail(&tex, 32);
    assert!(thumb.pixels.chunks_exact(4).any(|p| p[0] > 20));
    let rgba = decode_as(Format::Jxr, &bytes("rgba8_planar.jxr"), &DecodeOptions::full()).unwrap();
    assert!(rgba.has_alpha);
    assert!(matches!(rgba.base(), Pixels::Rgba8(_)));
}

#[test]
fn thumbnails_decode_a_reduced_picture() {
    let b = bytes("rgb24_tiled.jxr");
    let small = decode_as(Format::Jxr, &b, &DecodeOptions::preview(40)).unwrap();
    assert_eq!((small.width(), small.height()), (40, 24));
    assert_eq!((small.source.width, small.source.height), (160, 96), "the file's size is kept");
    let t = thumbnail(&small, 40);
    assert_eq!((t.width, t.height), (40, 24));
}

#[test]
fn files_from_other_programs() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../local/assets/science-extra/jxr");
    let Ok(entries) = std::fs::read_dir(&root) else { return };
    let mut checked = 0;
    for e in entries.flatten() {
        let path = e.path();
        if !matches!(path.extension().and_then(|s| s.to_str()), Some("jxr" | "wdp" | "hdp")) {
            continue;
        }
        let tex = decode_file(&path, &DecodeOptions::preview(64)).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        let thumb = thumbnail(&tex, 64);
        assert!(thumb.pixels.iter().step_by(4).any(|&v| v > 5), "{} is black", path.display());
        checked += 1;
    }
    eprintln!("checked {checked} downloaded JPEG XR files");
}
