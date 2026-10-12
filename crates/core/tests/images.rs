//! Ordinary image formats through the core: what each file is taken for,
//! which TIFFs are pictures and which are volumes, and that animations,
//! icons and stills load and make thumbnails.

use slerp_core::{Kind, Prepared, load, prepare_thumbnail, sniff_file};
use slerp_images::ImageFormat;
use slerp_scene::{Asset, Limits};
use std::path::{Path, PathBuf};

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("slerp-core-images-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn write(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
    let p = dir.join(name);
    std::fs::write(&p, bytes).unwrap();
    p
}

fn kind(p: &Path) -> Option<Kind> {
    sniff_file(p).unwrap()
}

/// A GIF of `frames` frames, each a 2 x 2 picture of one palette colour.
fn gif(frames: u8) -> Vec<u8> {
    let mut g = b"GIF89a".to_vec();
    g.extend([2, 0, 2, 0, 0x80 | 1, 0, 0]);
    g.extend([0, 0, 0, 255, 0, 0, 0, 255, 0, 0, 0, 255]);
    g.extend([0x21, 0xff, 11]);
    g.extend(b"NETSCAPE2.0");
    g.extend([3, 1, 0, 0, 0]);
    for i in 0..frames {
        g.extend([0x21, 0xf9, 4, 0, 10, 0, 0, 0]);
        g.extend([0x2c, 0, 0, 0, 0, 2, 0, 2, 0, 0]);
        // Min code size 2, one sub-block: clear, four literals of colour `1 + i % 3`, end.
        let c = 1 + i % 3;
        let codes = [4u8, c, c, c, c, 5];
        let mut bits = 0u32;
        let mut n = 0;
        let mut out = Vec::new();
        for code in codes {
            bits |= (code as u32) << n;
            n += 3;
            while n >= 8 {
                out.push(bits as u8);
                bits >>= 8;
                n -= 8;
            }
        }
        if n > 0 {
            out.push(bits as u8);
        }
        g.push(2);
        g.push(out.len() as u8);
        g.extend(out);
        g.push(0);
    }
    g.push(0x3b);
    g
}

/// A classic little-endian TIFF with one uncompressed page.
fn tiff(
    w: u32,
    h: u32,
    bits: u16,
    samples: u16,
    photometric: u16,
    data_len: usize,
    description: Option<&str>,
) -> Vec<u8> {
    let mut entries: Vec<(u16, u16, u32, u32)> = vec![
        (256, 4, 1, w),
        (257, 4, 1, h),
        (259, 3, 1, 1),
        (262, 3, 1, photometric as u32),
        (277, 3, 1, samples as u32),
        (278, 4, 1, h),
    ];
    let count = entries.len() + 3 + description.is_some() as usize;
    let ifd_len = 2 + count * 12 + 4;
    let mut extra = Vec::new();
    let extra_at = 8 + ifd_len;
    // Bits per sample: inline for one or two samples, else an array after the directory.
    if samples <= 2 {
        entries.push((258, 3, samples as u32, bits as u32 | ((bits as u32) << 16)));
    } else {
        entries.push((258, 3, samples as u32, (extra_at + extra.len()) as u32));
        for _ in 0..samples {
            extra.extend(bits.to_le_bytes());
        }
        if extra.len() % 2 == 1 {
            extra.push(0);
        }
    }
    if let Some(d) = description {
        let mut text = d.as_bytes().to_vec();
        text.push(0);
        entries.push((270, 2, text.len() as u32, (extra_at + extra.len()) as u32));
        extra.extend(text);
    }
    let data_at = extra_at + extra.len();
    entries.push((273, 4, 1, data_at as u32));
    entries.push((279, 4, 1, data_len as u32));
    entries.sort_by_key(|e| e.0);
    let mut t = b"II*\0".to_vec();
    t.extend(8u32.to_le_bytes());
    t.extend((entries.len() as u16).to_le_bytes());
    for (tag, typ, n, v) in &entries {
        t.extend(tag.to_le_bytes());
        t.extend(typ.to_le_bytes());
        t.extend(n.to_le_bytes());
        t.extend(v.to_le_bytes());
    }
    t.extend(0u32.to_le_bytes());
    t.extend(&extra);
    t.extend(vec![128u8; data_len]);
    t
}

#[test]
fn ordinary_formats_are_recognised_by_content() {
    let dir = scratch("sniff");
    assert_eq!(kind(&write(&dir, "a.gif", &gif(2))), Some(Kind::Image(ImageFormat::Gif)));
    // By content, not by name.
    assert_eq!(kind(&write(&dir, "picture.dat", &gif(2))), Some(Kind::Image(ImageFormat::Gif)));
    assert_eq!(kind(&write(&dir, "a.pgm", b"P5\n2 2\n255\n\x01\x02\x03\x04")), Some(Kind::Image(ImageFormat::Pnm)));
    // PAM has its own reader in the texture crate and is not taken for a Netpbm picture.
    assert!(matches!(
        kind(&write(&dir, "a.pam", b"P7\nWIDTH 1\nHEIGHT 1\nDEPTH 1\nMAXVAL 255\nTUPLTYPE GRAYSCALE\nENDHDR\n\x05")),
        Some(Kind::Texture(_))
    ));
    let mut ico = vec![0, 0, 1, 0, 1, 0, 2, 2, 0, 0, 1, 0, 24, 0];
    let dib: Vec<u8> = {
        let mut d = Vec::new();
        d.extend(40u32.to_le_bytes());
        d.extend(2u32.to_le_bytes());
        d.extend(4u32.to_le_bytes());
        d.extend([1, 0, 24, 0]);
        d.extend([0u8; 24]);
        d.extend(vec![200u8; 16]);
        d.extend(vec![0u8; 8]);
        d
    };
    ico.extend((dib.len() as u32).to_le_bytes());
    ico.extend(22u32.to_le_bytes());
    ico.extend(&dib);
    assert_eq!(kind(&write(&dir, "a.ico", &ico)), Some(Kind::Image(ImageFormat::Ico)));
    // JPEG 2000's signature box, AVIF's and HEIC's brands.
    assert_eq!(
        kind(&write(&dir, "a.jp2", b"\x00\x00\x00\x0cjP  \r\n\x87\n\0\0\0\x14ftypjp2 \0\0\0\0jp2 ")),
        Some(Kind::Image(ImageFormat::Jp2))
    );
    assert_eq!(
        kind(&write(&dir, "a.avif", b"\0\0\0\x14ftypavif\0\0\0\0avifmif1")),
        Some(Kind::Image(ImageFormat::Avif))
    );
    assert_eq!(
        kind(&write(&dir, "a.heic", b"\0\0\0\x18ftypheic\0\0\0\0mif1heic")),
        Some(Kind::Image(ImageFormat::Heif))
    );
    // A plain PNG and a JPEG stay with the texture crate.
    let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
    png.extend([0, 0, 0, 13]);
    png.extend(b"IHDR");
    png.extend([0u8; 13]);
    png.extend([0, 0, 0, 0, 0, 0, 0, 0]);
    png.extend(b"IDAT");
    assert!(matches!(kind(&write(&dir, "plain.png", &png)), Some(Kind::Texture(_))));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn tiff_pictures_and_tiff_data_go_different_ways() {
    let dir = scratch("tiff");
    // An 8-bit colour page and a grey one are pictures.
    assert_eq!(kind(&write(&dir, "photo.tif", &tiff(2, 2, 8, 3, 2, 12, None))), Some(Kind::Image(ImageFormat::Tiff)));
    assert_eq!(kind(&write(&dir, "grey.tif", &tiff(2, 2, 8, 1, 1, 4, None))), Some(Kind::Image(ImageFormat::Tiff)));
    // 16-bit colour is a picture too; 16-bit grey is data for the volume viewer.
    assert_eq!(kind(&write(&dir, "deep.tif", &tiff(2, 2, 16, 3, 2, 24, None))), Some(Kind::Image(ImageFormat::Tiff)));
    assert!(matches!(kind(&write(&dir, "data.tif", &tiff(2, 2, 16, 1, 1, 8, None))), Some(Kind::Volume(_))));
    // Scientific files stay volumes whatever their pixels.
    let ij = tiff(2, 2, 8, 1, 1, 4, Some("ImageJ=1.53\nimages=1\n"));
    assert!(matches!(kind(&write(&dir, "stack.tif", &ij)), Some(Kind::Volume(_))));
    // Camera raw files are neither.
    assert_eq!(kind(&write(&dir, "shot.dng", &tiff(2, 2, 8, 3, 2, 12, None))), None);
    // The picture loads, upright and as 8-bit colour.
    let Asset::Image(t) = load(&dir.join("photo.tif"), &Limits::viewer()).unwrap() else { panic!("not a picture") };
    assert_eq!((t.width(), t.height()), (2, 2));
    assert_eq!(t.source.container, "TIFF");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn an_animated_gif_is_a_canvas_with_a_thumbnail() {
    let dir = scratch("gif");
    let p = write(&dir, "anim.gif", &gif(5));
    let Asset::Canvas(c) = load(&p, &Limits::viewer()).unwrap() else { panic!("not an animation") };
    assert_eq!(c.frames().len(), 5);
    assert_eq!(c.size(), (2, 2));
    assert_eq!(c.loops(), None);
    // Frame i shows palette colour 1 + i % 3 (red, green, blue).
    let px = |i: usize| c.compose(i, &[], 1.0).pixels[..4].to_vec();
    assert_eq!(px(0), [255, 0, 0, 255]);
    assert_eq!(px(1), [0, 255, 0, 255]);
    assert_eq!(px(2), [0, 0, 255, 255]);
    assert_eq!(px(4), [0, 255, 0, 255]);
    // Thumbnails take the first frame, decoding nothing else.
    let Ok(Prepared::Done(img)) = prepare_thumbnail(&p, 64) else { panic!("no thumbnail") };
    assert!(img.width > 0 && img.height > 0);
    // A GIF of one frame is a picture.
    let one = write(&dir, "still.gif", &gif(1));
    assert!(matches!(load(&one, &Limits::viewer()).unwrap(), Asset::Image(_)));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn damaged_files_fail_cleanly() {
    let dir = scratch("damaged");
    let good = gif(3);
    for cut in (0..good.len()).step_by(3) {
        let p = write(&dir, "cut.gif", &good[..cut]);
        // Whatever it is taken for, loading and thumbnailing never panic.
        let _ = load(&p, &Limits::viewer());
        let _ = prepare_thumbnail(&p, 32);
    }
    let t = tiff(2, 2, 8, 3, 2, 12, None);
    for cut in 0..t.len() {
        let p = write(&dir, "cut.tif", &t[..cut]);
        let _ = load(&p, &Limits::viewer());
    }
    std::fs::remove_dir_all(&dir).ok();
}

/// The sample files, when `local/assets/images-extra` exists: everything the
/// sniffer takes for an ordinary image opens, thumbnails and describes.
#[test]
fn sample_files_open_through_the_core() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../local/assets/images-extra");
    if !root.is_dir() {
        return;
    }
    let (mut opened, mut refused) = (0, Vec::new());
    for folder in ["gif", "apng", "webp", "ico", "pnm", "tiff", "jp2", "heic"] {
        let Ok(rd) = std::fs::read_dir(root.join(folder)) else { continue };
        let mut files: Vec<PathBuf> = rd.filter_map(|e| e.ok()).map(|e| e.path()).filter(|p| p.is_file()).collect();
        files.sort();
        for p in files.into_iter().step_by(4) {
            let Ok(Some(Kind::Image(_))) = sniff_file(&p) else { continue };
            match load(&p, &Limits::viewer()) {
                Ok(asset) => {
                    opened += 1;
                    let text = slerp_core::describe(&asset);
                    assert!(!text.is_empty());
                    let _ = prepare_thumbnail(&p, 64);
                }
                Err(e) => refused.push(format!("{}: {e}", p.file_name().unwrap().to_string_lossy())),
            }
        }
    }
    assert!(opened > 50, "{opened}");
    // Files the readers decline (a compression nobody here reads, a damaged file) say so.
    assert!(refused.iter().all(|r| !r.contains("internal decoder error")), "{refused:?}");
}
