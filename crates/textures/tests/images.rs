//! Ordinary images against Pillow's decodes of the same files (see
//! `data/make-images.py`).

use slerp_textures::{ColorSpace, DecodeOptions, Format, Pixels, decode, decode_as, sniff};
use std::path::Path;

fn data(name: &str) -> Vec<u8> {
    std::fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data").join(name)).unwrap()
}

/// Decodes `name` and checks every channel is within `tolerance` of
/// Pillow's result.
fn check(name: &str, format: Format, tolerance: u8) {
    let bytes = data(name);
    let tex = decode_as(format, &bytes, &DecodeOptions::full()).unwrap_or_else(|e| panic!("{name}: {e}"));
    assert_eq!((tex.width(), tex.height()), (23, 17), "{name}");
    assert_eq!(tex.color_space, ColorSpace::Unspecified);
    let Pixels::Rgba8(ours) = tex.base() else { panic!("{name}: not 8-bit") };
    let theirs = data(&format!("{name}.rgba"));
    assert_eq!(ours.len(), theirs.len(), "{name}");
    // Pillow ignores the fourth byte of a plain 32-bit BMP; browsers, and
    // this reader, use it as alpha when any texel sets it.
    let skip_alpha = name == "rgba32.bmp";
    let worst = ours
        .iter()
        .zip(&theirs)
        .enumerate()
        .filter(|(i, _)| !skip_alpha || i % 4 != 3)
        .map(|(_, (a, b))| a.abs_diff(*b))
        .max();
    let worst = worst.unwrap();
    assert!(worst <= tolerance, "{name}: off by up to {worst}");
}

#[test]
fn png() {
    for name in ["rgba.png", "pal.png", "grey-alpha.png", "interlaced.png"] {
        check(name, Format::Png, 0);
    }
}

/// 16-bit greyscale PNGs are data (heightmaps): all 16 bits are kept as
/// linear floats and tagged so thumbnails stretch them to their range.
#[test]
fn png_grey16_is_data() {
    let tex = decode_as(Format::Png, &data("grey16.png"), &DecodeOptions::full()).unwrap();
    assert_eq!((tex.width(), tex.height()), (23, 17));
    assert_eq!(tex.color_space, ColorSpace::Linear);
    assert!(tex.source.format.ends_with("(data)"), "{}", tex.source.format);
    let Pixels::Rgba32F(ours) = tex.base() else { panic!("not float") };
    // Pillow's 8-bit reference keeps the high byte.
    let theirs = data("grey16.png.rgba");
    for (a, b) in ours.iter().zip(&theirs).step_by(4) {
        assert!((a * 255.0 - *b as f32).abs() <= 1.0, "{a} vs {b}");
    }
    let thumb = slerp_textures::thumbnail(&tex, 64);
    let (lo, hi) = thumb.pixels.chunks_exact(4).fold((255, 0), |(lo, hi), p| (p[0].min(lo), p[0].max(hi)));
    assert_eq!((lo, hi), (0, 255), "thumbnails stretch data to the full range");
}

#[test]
fn jpeg() {
    // Pillow uses libjpeg-turbo, whose IDCT and chroma upsampling round
    // differently.
    check("rgb.jpg", Format::Jpeg, 3);
    check("grey.jpg", Format::Jpeg, 3);
    check("rgb420.jpg", Format::Jpeg, 12);
}

#[test]
fn jpeg_reduced_scale() {
    let bytes = data("rgb.jpg");
    // 23 px wide: 1/2 scale gives 12, which still covers a 10 px request.
    let opts = DecodeOptions { min_size: Some(10), max_size: None, all_mips: false, first_layer_only: true };
    let tex = decode(&bytes, &opts).unwrap();
    assert_eq!((tex.width(), tex.height()), (12, 9));
    assert_eq!((tex.source.width, tex.source.height), (23, 17));
}

#[test]
fn webp() {
    check("rgba-lossless.webp", Format::Webp, 0);
    check("rgb-lossy.webp", Format::Webp, 4);
    // Animated files keep their first frame.
    check("anim.webp", Format::Webp, 0);
    // Stills shrink while decoding when asked for less.
    let tex = decode(&data("rgb-lossy.webp"), &DecodeOptions::preview(10)).unwrap();
    assert_eq!((tex.width(), tex.height(), tex.source.width), (10, 7, 23));
}

#[test]
fn bmp() {
    for name in ["rgb24.bmp", "rgba32.bmp", "pal8.bmp", "mono1.bmp"] {
        check(name, Format::Bmp, 0);
    }
}

#[test]
fn bmp_rle8() {
    // 4x2, bottom row first: a run of 4 of colour 1, then an absolute run
    // of 0 1 2 (padded to a word), a run of 1 of colour 2, end of bitmap.
    let pixels: Vec<u8> = vec![4, 1, 0, 0, 0, 3, 0, 1, 2, 0, 1, 2, 0, 1];
    let palette: [[u8; 4]; 3] = [[0, 0, 0, 0], [0, 0, 255, 0], [255, 0, 0, 0]];
    let offset = 14 + 40 + 12;
    let mut f = b"BM".to_vec();
    f.extend((offset as u32 + pixels.len() as u32).to_le_bytes());
    f.extend([0; 4]);
    f.extend((offset as u32).to_le_bytes());
    f.extend(40u32.to_le_bytes());
    f.extend(4i32.to_le_bytes());
    f.extend(2i32.to_le_bytes());
    f.extend(1u16.to_le_bytes());
    f.extend(8u16.to_le_bytes());
    f.extend(1u32.to_le_bytes());
    f.extend([0; 12]);
    f.extend(3u32.to_le_bytes());
    f.extend([0; 4]);
    f.extend(palette.iter().flatten());
    f.extend(pixels);
    assert_eq!(sniff(&f), Some(Format::Bmp));
    let tex = decode(&f, &DecodeOptions::full()).unwrap();
    let Pixels::Rgba8(v) = tex.base() else { panic!() };
    let red = [255, 0, 0, 255];
    let blue = [0, 0, 255, 255];
    let black = [0, 0, 0, 255];
    let expect: Vec<u8> = [black, red, blue, blue, red, red, red, red].concat();
    assert_eq!(v, &expect);
}

#[test]
fn tga() {
    for name in ["rgb.tga", "rgba-rle.tga", "grey.tga", "pal.tga"] {
        check(name, Format::Tga, 0);
    }
}

#[test]
fn pcx() {
    for name in ["rgb.pcx", "pal8.pcx", "mono.pcx"] {
        check(name, Format::Pcx, 0);
    }
}

#[test]
fn sniffing() {
    for (name, format) in [
        ("rgba.png", Format::Png),
        ("rgb.jpg", Format::Jpeg),
        ("rgb-lossy.webp", Format::Webp),
        ("rgb24.bmp", Format::Bmp),
        ("rgb.pcx", Format::Pcx),
        ("rgb.tga", Format::Tga),
    ] {
        assert_eq!(sniff(&data(name)[..32]), Some(format), "{name}");
    }
    assert_eq!(sniff(b"#?RADIANCE\nFORMAT=32-bit_rle_rgbe\n"), Some(Format::Hdr));
    assert_eq!(sniff(b"\xabKTX 20\xbb\r\n\x1a\n"), Some(Format::Ktx2));
    assert_eq!(sniff(b"\xabKTX 11\xbb\r\n\x1a\n"), Some(Format::Ktx1));
    assert_eq!(sniff(b"DDS |\0\0\0"), Some(Format::Dds));
    assert_eq!(sniff(&[0x13, 0xab, 0xa1, 0x5c, 4, 4, 1]), Some(Format::Astc));
    assert_eq!(sniff(b"hello world, this is text"), None);
    assert_eq!(sniff(b""), None);
}

#[test]
fn max_size_downscales() {
    let bytes = data("rgba.png");
    let opts = DecodeOptions { min_size: None, max_size: Some(8), all_mips: false, first_layer_only: true };
    let tex = decode(&bytes, &opts).unwrap();
    assert_eq!((tex.width(), tex.height()), (8, 6));
    assert_eq!((tex.source.width, tex.source.height), (23, 17));
}
