//! XISF decoding: files written by the `xisf` Python package (codecs with
//! and without byte shuffling) and by hand (storage orders, subblocks,
//! embedded data, mosaics, orientations), against the pixels they were
//! written from (`data/make-xisf.py`), plus the real files from other
//! programs in the downloaded assets when present.

use slerp_textures::{DecodeOptions, Format, Pixels, Texture, decode_as, decode_file, parts_file};
use std::path::{Path, PathBuf};

fn dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/xisf")
}

fn expected(name: &str) -> Vec<f32> {
    let bytes = std::fs::read(dir().join(format!("{name}.f32"))).unwrap();
    bytes.chunks_exact(4).map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]])).collect()
}

fn px(tex: &Texture, x: usize, y: usize) -> [f32; 4] {
    let Pixels::Rgba32F(v) = &tex.levels[0].surfaces[0] else { panic!("not float pixels") };
    let w = tex.levels[0].width as usize;
    v[(y * w + x) * 4..(y * w + x) * 4 + 4].try_into().unwrap()
}

#[test]
fn every_case_matches_what_was_written() {
    let manifest = std::fs::read_to_string(dir().join("manifest.txt")).unwrap();
    let mut count = 0;
    for line in manifest.lines() {
        let f: Vec<&str> = line.split(' ').collect();
        let (name, w, h, c): (&str, usize, usize, usize) =
            (f[0], f[1].parse().unwrap(), f[2].parse().unwrap(), f[3].parse().unwrap());
        if name == "cfa_rggb" {
            continue;
        }
        let bytes = std::fs::read(dir().join(format!("{name}.xisf"))).unwrap();
        let tex = decode_as(Format::Xisf, &bytes, &DecodeOptions::full()).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(tex.source.container, "XISF");
        assert!(tex.source.format.ends_with("(stretch)"), "{}", tex.source.format);
        assert_eq!((tex.width() as usize, tex.height() as usize), (w, h), "{name}");
        let exp = expected(name);
        for y in 0..h {
            for x in 0..w {
                let p = px(&tex, x, y);
                for ch in 0..3 {
                    let want = exp[(y * w + x) * c + ch.min(c - 1)];
                    if want.is_nan() {
                        assert!(p[3] < 0.5, "{name} ({x},{y}) should be undefined");
                    } else {
                        assert!(p[3] >= 0.5, "{name} ({x},{y}) is undefined");
                        assert!(
                            (p[ch] - want).abs() <= 1e-6 + 1e-6 * want.abs(),
                            "{name} ({x},{y}) ch {ch}: {} vs {want}",
                            p[ch]
                        );
                    }
                }
            }
        }
        count += 1;
    }
    assert!(count >= 17, "{count}");
}

#[test]
fn codecs_are_named() {
    let bytes = std::fs::read(dir().join("u16_zstd_sh.xisf")).unwrap();
    let tex = decode_as(Format::Xisf, &bytes, &DecodeOptions::full()).unwrap();
    assert!(tex.source.format.contains("zstd+shuffle"), "{}", tex.source.format);
    assert_eq!(tex.source.supercompression.as_deref(), Some("zstd"));
    let find = |k: &str| tex.source.header.iter().find(|(key, _)| key == k).map(|(_, v)| v.clone());
    assert_eq!(find("OBJECT").as_deref(), Some("M 31 / target"));
    assert_eq!(find("EXPTIME").as_deref(), Some("120"));
}

#[test]
fn mosaics_become_colour() {
    let bytes = std::fs::read(dir().join("cfa_rggb.xisf")).unwrap();
    let tex = decode_as(Format::Xisf, &bytes, &DecodeOptions::full()).unwrap();
    assert!(tex.source.format.contains("RGGB mosaic"), "{}", tex.source.format);
    let p = px(&tex, 11, 9);
    assert!(
        (p[0] - 50000.0 / 65535.0).abs() < 1e-4
            && (p[1] - 30000.0 / 65535.0).abs() < 1e-4
            && (p[2] - 8000.0 / 65535.0).abs() < 1e-4,
        "{p:?}"
    );
    let small = decode_as(Format::Xisf, &bytes, &DecodeOptions::preview(10)).unwrap();
    assert_eq!((small.width(), small.height()), (10, 6));
    let p = px(&small, 4, 4);
    assert!((p[0] - 50000.0 / 65535.0).abs() < 1e-4 && (p[2] - 8000.0 / 65535.0).abs() < 1e-4, "{p:?}");
}

#[test]
fn reduced_decodes_average_boxes() {
    let bytes = std::fs::read(dir().join("u16_le_normal_rgb.xisf")).unwrap();
    let full = decode_as(Format::Xisf, &bytes, &DecodeOptions::full()).unwrap();
    // 40x24 reduced by 4 (40/10).
    let small = decode_as(Format::Xisf, &bytes, &DecodeOptions::preview(10)).unwrap();
    assert_eq!((small.width(), small.height()), (10, 6));
    let mut sum = [0.0f64; 3];
    for dy in 0..4 {
        for dx in 0..4 {
            let p = px(&full, 4 + dx, 8 + dy);
            for c in 0..3 {
                sum[c] += p[c] as f64;
            }
        }
    }
    let got = px(&small, 1, 2);
    for c in 0..3 {
        assert!((got[c] as f64 - sum[c] / 16.0).abs() < 1e-5, "{c}: {} vs {}", got[c], sum[c] / 16.0);
    }
}

#[test]
fn files_from_other_programs() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../local/assets/science-extra/xisf");
    let Ok(entries) = std::fs::read_dir(&root) else { return };
    let mut checked = 0;
    for e in entries.flatten() {
        let path = e.path();
        if path.extension().and_then(|s| s.to_str()) != Some("xisf") {
            continue;
        }
        let tex = decode_file(&path, &DecodeOptions::preview(64)).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        let thumb = slerp_textures::thumbnail(&tex, 64);
        // Flat test files (one dark value such as 42 of 65535, or all
        // undefined) are rightly black: a flat picture shows its own value.
        let flat = match tex.base() {
            Pixels::Rgba32F(v) => {
                let mut defined = v.chunks_exact(4).filter(|p| p[3] >= 0.5);
                let first = defined.next().map(|p| [p[0], p[1], p[2]]);
                first.is_none_or(|f| defined.all(|p| (0..3).all(|c| (p[c] - f[c]).abs() <= 1e-5 * f[c].abs())))
            }
            _ => false,
        };
        // One generated file holds random bit patterns read as floats
        // (values from 1e-29 to 1e29 and NaNs), which no stretch can show.
        let noise = path.to_string_lossy().contains("Float32_Gray_lz4");
        assert!(flat || noise || thumb.pixels.iter().step_by(4).any(|&v| v > 20), "{} is black", path.display());
        assert!(parts_file(&path).is_empty());
        checked += 1;
    }
    eprintln!("checked {checked} downloaded XISF files");
}
