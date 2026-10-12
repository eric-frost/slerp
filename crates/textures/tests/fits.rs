//! FITS decoding against astropy and cfitsio.  `data/make-fits.py` writes
//! each case with astropy (tile compression included, by cfitsio, an
//! independent encoder) and the pixels astropy reads back; every one must
//! decode to the same values here.

use slerp_textures::{DecodeOptions, Format, Pixels, Texture, decode_as, decode_file_part, parts_file};
use std::path::{Path, PathBuf};

fn dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/fits")
}

struct Case {
    name: String,
    width: usize,
    height: usize,
    planes: usize,
}

fn cases() -> Vec<Case> {
    std::fs::read_to_string(dir().join("manifest.txt"))
        .unwrap()
        .lines()
        .map(|l| {
            let f: Vec<&str> = l.split(' ').collect();
            Case {
                name: f[0].to_string(),
                width: f[1].parse().unwrap(),
                height: f[2].parse().unwrap(),
                planes: f[3].parse().unwrap(),
            }
        })
        .collect()
}

fn expected(name: &str) -> Vec<f32> {
    let bytes = std::fs::read(dir().join(format!("{name}.f32"))).unwrap();
    bytes.chunks_exact(4).map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]])).collect()
}

fn file_of(case: &Case) -> PathBuf {
    // "two_images_2" is the second picture of two_images.fits.
    let name = case.name.strip_suffix("_2").filter(|n| *n == "two_images").unwrap_or(&case.name);
    dir().join(format!("{name}.fits"))
}

fn full() -> DecodeOptions {
    DecodeOptions::full()
}

/// The pixel at upright (x, y) of surface `s`, channel `c`, and whether it
/// is defined.
fn pixel(tex: &Texture, s: usize, x: usize, y: usize, c: usize) -> (f32, bool) {
    let Pixels::Rgba32F(v) = &tex.levels[0].surfaces[s] else { panic!("not float pixels") };
    let w = tex.levels[0].width as usize;
    let p = &v[(y * w + x) * 4..(y * w + x) * 4 + 4];
    (p[c], p[3] >= 0.5)
}

fn close(a: f32, b: f32) -> bool {
    (a - b).abs() <= 1e-5 * b.abs().max(1.0)
}

fn check(case: &Case, tex: &Texture) {
    let exp = expected(&case.name);
    let (w, h) = (case.width, case.height);
    assert_eq!((tex.width() as usize, tex.height() as usize), (w, h), "{}: size", case.name);
    let rgb = case.planes == 3;
    if rgb {
        assert_eq!(tex.layers, 1, "{}", case.name);
    } else {
        assert_eq!(tex.layers as usize, case.planes, "{}: layers", case.name);
    }
    for s in 0..tex.layers as usize {
        for y in 0..h {
            for x in 0..w {
                // The file's first row is the picture's bottom.
                let file_row = h - 1 - y;
                let channels = if rgb { 3 } else { 1 };
                for c in 0..channels {
                    let plane = if rgb { c } else { s };
                    let want = exp[(plane * h + file_row) * w + x];
                    let (got, defined) = pixel(tex, s, x, y, c);
                    if want.is_nan() {
                        assert!(!defined, "{}: ({x},{y}) should be undefined", case.name);
                    } else {
                        assert!(defined, "{}: ({x},{y}) is undefined, want {want}", case.name);
                        assert!(close(got, want), "{}: ({x},{y}) plane {plane}: got {got}, want {want}", case.name);
                    }
                }
            }
        }
    }
}

#[test]
fn every_case_matches_astropy() {
    let mut count = 0;
    for case in cases() {
        if case.name == "bayer_rggb" {
            continue;
        }
        let path = file_of(&case);
        let tex = if case.name == "two_images_2" {
            let parts = parts_file(&path);
            assert_eq!(parts.len(), 2, "{parts:?}");
            decode_file_part(&path, Some(&parts[1]), &full()).unwrap()
        } else {
            let bytes = std::fs::read(&path).unwrap();
            let tex = decode_as(Format::Fits, &bytes, &full()).unwrap_or_else(|e| panic!("{}: {e}", case.name));
            // The file reader gives the same picture.
            let from_file = decode_file_part(&path, None, &full()).unwrap();
            assert_eq!(from_file.levels[0].surfaces.len(), tex.levels[0].surfaces.len());
            tex
        };
        assert_eq!(tex.source.container, "FITS");
        assert!(tex.source.format.ends_with("(stretch)"), "{}", tex.source.format);
        check(&case, &tex);
        count += 1;
    }
    assert!(count >= 30, "{count}");
}

#[test]
fn files_with_several_images_list_them() {
    let two = parts_file(&dir().join("two_images.fits"));
    assert_eq!(two.len(), 2, "{two:?}");
    assert!(two[0].starts_with("1: SCI 1 (24x16"), "{two:?}");
    assert!(two[1].starts_with("2: ERR 2 (64x40"), "{two:?}");
    // One image, however many units, is no choice.
    assert!(parts_file(&dir().join("ext1_u16.fits")).is_empty());
    assert!(parts_file(&dir().join("u16.fits")).is_empty());
    assert!(parts_file(&dir().join("manifest.txt")).is_empty());
}

#[test]
fn header_cards_are_listed() {
    let bytes = std::fs::read(dir().join("bayer_rggb.fits")).unwrap();
    let tex = decode_as(Format::Fits, &bytes, &full()).unwrap();
    let find = |k: &str| tex.source.header.iter().find(|(key, _)| key == k).map(|(_, v)| v.as_str());
    assert_eq!(find("BAYERPAT"), Some("RGGB"));
    assert_eq!(find("BITPIX"), None, "structural cards stay out");
    assert_eq!(find("BZERO"), Some("32768"));
}

#[test]
fn bayer_mosaics_demosaic_to_their_colour() {
    let bytes = std::fs::read(dir().join("bayer_rggb.fits")).unwrap();
    let tex = decode_as(Format::Fits, &bytes, &full()).unwrap();
    assert!(tex.source.format.contains("RGGB mosaic"), "{}", tex.source.format);
    // Away from the edge a flat red 40000, green 20000, blue 5000 scene.
    for (x, y) in [(10, 10), (11, 10), (10, 11), (30, 20)] {
        let (r, _) = pixel(&tex, 0, x, y, 0);
        let (g, _) = pixel(&tex, 0, x, y, 1);
        let (b, _) = pixel(&tex, 0, x, y, 2);
        assert!(close(r, 40000.0) && close(g, 20000.0) && close(b, 5000.0), "({x},{y}): {r} {g} {b}");
    }
    // A thumbnail takes each cell as one pixel, with the same colours.
    let small = decode_as(Format::Fits, &bytes, &DecodeOptions::preview(16)).unwrap();
    assert!(small.width() <= 32);
    let (r, _) = pixel(&small, 0, 3, 3, 0);
    let (b, _) = pixel(&small, 0, 3, 3, 2);
    assert!(close(r, 40000.0) && close(b, 5000.0), "{r} {b}");
}

#[test]
fn reduced_decodes_average_boxes() {
    let case = cases().into_iter().find(|c| c.name == "u16").unwrap();
    let bytes = std::fs::read(dir().join("u16.fits")).unwrap();
    let small = decode_as(Format::Fits, &bytes, &DecodeOptions::preview(16)).unwrap();
    // 64x40 reduced by 4 (64/16).
    assert_eq!((small.width(), small.height()), (16, 10));
    let exp = expected("u16");
    let (w, h) = (case.width, case.height);
    for (ox, oy) in [(0usize, 0usize), (7, 5), (15, 9)] {
        let mut sum = 0.0;
        for dy in 0..4 {
            for dx in 0..4 {
                let (x, y) = (ox * 4 + dx, oy * 4 + dy);
                sum += exp[(h - 1 - y) * w + x] as f64;
            }
        }
        let (got, _) = pixel(&small, 0, ox, oy, 0);
        assert!(close(got, (sum / 16.0) as f32), "({ox},{oy}): {got} vs {}", sum / 16.0);
    }
    // The compressed path reduces the same way: boxes of the decoded tiles.
    let exp = expected("rice_i16");
    let comp = std::fs::read(dir().join("rice_i16.fits")).unwrap();
    let small_c = decode_as(Format::Fits, &comp, &DecodeOptions::preview(16)).unwrap();
    assert_eq!((small_c.width(), small_c.height()), (16, 10));
    for (ox, oy) in [(0usize, 0usize), (7, 5), (15, 9)] {
        let mut sum = 0.0;
        for dy in 0..4 {
            for dx in 0..4 {
                sum += exp[(h - 1 - (oy * 4 + dy)) * w + ox * 4 + dx] as f64;
            }
        }
        let (got, _) = pixel(&small_c, 0, ox, oy, 0);
        assert!(close(got, (sum / 16.0) as f32), "({ox},{oy}): {got} vs {}", sum / 16.0);
    }
}

#[test]
fn thumbnails_are_stretched() {
    let bytes = std::fs::read(dir().join("u16.fits")).unwrap();
    let tex = decode_as(Format::Fits, &bytes, &DecodeOptions::preview(64)).unwrap();
    let thumb = slerp_textures::thumbnail(&tex, 64);
    let mid = thumb.pixels.iter().step_by(4).map(|&v| v as u32).sum::<u32>() / (thumb.pixels.len() as u32 / 4);
    // Raw values are around 2000-3000 of 65535; the stretch makes them a
    // readable mid grey rather than black.
    assert!((30..160).contains(&mid), "{mid}");
}
