//! The science and film formats through core: recognised by content under
//! any name, FITS files with several images as parts, and thumbnails that
//! need no GPU.

use slerp_core::scene::{Asset, Limits};
use slerp_core::textures::Format as TextureFormat;
use slerp_core::{Kind, Prepared, load_part, parts, prepare_thumbnail, sniff_file};
use std::path::{Path, PathBuf};

fn data(name: &str) -> PathBuf {
    let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("../textures/tests/data");
    if name.ends_with(".jxr") {
        return Path::new(env!("CARGO_MANIFEST_DIR")).join("../jxr/tests/data/files").join(name);
    }
    let sub = match name.rsplit('.').next().unwrap() {
        "fits" => "fits",
        "xisf" => "xisf",
        _ => "film",
    };
    base.join(sub).join(name)
}

#[test]
fn recognised_by_content() {
    for (name, format) in [
        ("u16.fits", TextureFormat::Fits),
        ("rice_i16.fits", TextureFormat::Fits),
        ("u16_zstd_sh.xisf", TextureFormat::Xisf),
        ("dpx_10_be_a.dpx", TextureFormat::Dpx),
        ("dpx_10_le_a.dpx", TextureFormat::Dpx),
        ("cin_10_rgb.cin", TextureFormat::Cineon),
        ("pam_rgba8.pam", TextureFormat::Pam),
        ("farbfeld.ff", TextureFormat::Farbfeld),
        ("rgb24.jxr", TextureFormat::Jxr),
    ] {
        let kind = sniff_file(&data(name)).unwrap();
        assert_eq!(kind, Some(Kind::Texture(format)), "{name}");
        assert!(!Kind::Texture(format).usually_3d());
        // And under another name.
        let tmp = std::env::temp_dir().join(format!("slerp-science-sniff-{}-{name}", std::process::id()));
        std::fs::copy(data(name), &tmp).unwrap();
        let renamed = tmp.with_extension("");
        std::fs::rename(&tmp, &renamed).unwrap();
        assert_eq!(sniff_file(&renamed).unwrap(), Some(Kind::Texture(format)), "{name} renamed");
        std::fs::remove_file(&renamed).unwrap();
    }
}

/// FITS astronomy images and Garmin activity files share the extension
/// `.fit`; only the content tells them apart.
#[test]
fn fit_extension_is_told_apart_by_content() {
    let dir = std::env::temp_dir().join(format!("slerp-science-fit-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let sky = dir.join("sky.fit");
    std::fs::copy(data("u16.fits"), &sky).unwrap();
    assert_eq!(sniff_file(&sky).unwrap(), Some(Kind::Texture(TextureFormat::Fits)));
    // A Garmin FIT header: size 14, protocol, profile, no records, ".FIT".
    let garmin = dir.join("activity.fit");
    std::fs::write(&garmin, b"\x0e\x20\x54\x08\0\0\0\0.FIT\0\0\0\0").unwrap();
    assert!(!matches!(sniff_file(&garmin).unwrap(), Some(Kind::Texture(_))), "a Garmin file is not a picture");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn images_in_one_fits_file_are_parts() {
    let path = data("two_images.fits");
    let names = parts(&path);
    assert_eq!(names.len(), 2, "{names:?}");
    let Asset::Image(second) = load_part(&path, &Limits::viewer(), &names[1]).unwrap() else { panic!("not a picture") };
    assert_eq!((second.width(), second.height()), (64, 40));
    assert!(parts(&data("u16.fits")).is_empty());
    assert!(parts(&data("u16_zstd_sh.xisf")).is_empty());
    assert!(parts(&data("dpx_10_be_a.dpx")).is_empty());
}

#[test]
fn thumbnails_need_no_gpu_and_are_not_black() {
    for name in [
        "u16.fits",
        "rice_f32_dither1.fits",
        "rgb_f32.fits",
        "u16_zlib_sh.xisf",
        "dpx_10_be_a.dpx",
        "dpx_16_linear.dpx",
        "cin_10_rgb.cin",
        "pam_rgba8.pam",
        "farbfeld.ff",
        "rgba_half.jxr",
    ] {
        let Prepared::Done(img) = prepare_thumbnail(&data(name), 64).unwrap_or_else(|e| panic!("{name}: {e}")) else {
            panic!("{name}: needs the GPU")
        };
        assert!(img.width <= 64 && img.height <= 64 && img.width > 0, "{name}");
        let bright = img.pixels.chunks_exact(4).filter(|p| p[0] > 24 || p[1] > 24 || p[2] > 24).count();
        assert!(bright > 20, "{name} is black ({bright} bright pixels)");
    }
}
