//! JPEG XR decoding against files written by Microsoft's own encoder and
//! the pictures its own decoder makes of them (`data/make-jxr.py`), the
//! reduced-size path, and damaged files: a fault in jxrlib must cost an
//! error, never the test process.

use slerp_jxr::{Rgba, decode};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

fn dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/files")
}

fn read(name: &str) -> Vec<u8> {
    std::fs::read(dir().join(name)).unwrap()
}

fn cases() -> Vec<(String, u32, u32, String)> {
    String::from_utf8(read("manifest.txt"))
        .unwrap()
        .lines()
        .map(|l| {
            let f: Vec<&str> = l.split(' ').collect();
            (f[0].to_string(), f[1].parse().unwrap(), f[2].parse().unwrap(), f[3].to_string())
        })
        .collect()
}

const LIMIT: usize = 256 << 20;

#[test]
fn every_case_matches_the_reference_decoder() {
    let mut count = 0;
    for (name, w, h, kind) in cases() {
        let d = decode(&read(&format!("{name}.jxr")), 0, LIMIT).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!((d.width, d.height), (w, h), "{name}");
        assert_eq!((d.full_width, d.full_height), (w, h), "{name}");
        if kind == "oracle" {
            check_against_oracle(&name, &d);
            count += 1;
            continue;
        }
        match (d.to_rgba(), kind.as_str()) {
            (Rgba::U8(got), "u8") => {
                let want = read(&format!("{name}.rgba"));
                assert_eq!(got.len(), want.len(), "{name}");
                let worst = got.iter().zip(&want).map(|(a, b)| a.abs_diff(*b)).max().unwrap();
                let mismatches = got.iter().zip(&want).filter(|(a, b)| a != b).count();
                assert!(
                    worst <= 1 && mismatches * 100 < got.len(),
                    "{name}: worst {worst}, {mismatches} of {} differ",
                    got.len()
                );
            }
            (Rgba::F32(got), "f32" | "f32lossy") => {
                // Half-float files come back within a unit of the half's last place.
                let loose = if kind == "f32lossy" {
                    0.15
                } else if name.contains("half") {
                    2e-3
                } else {
                    1e-5
                };
                let want: Vec<f32> = read(&format!("{name}.f32"))
                    .chunks_exact(4)
                    .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
                    .collect();
                assert_eq!(got.len(), want.len(), "{name}");
                for (i, (a, b)) in got.iter().zip(&want).enumerate() {
                    assert!((a - b).abs() <= loose * b.abs().max(1.0), "{name} sample {i}: {a} vs {b}");
                }
            }
            (other, kind) => panic!("{name}: expected {kind}, got {other:?}"),
        }
        count += 1;
    }
    assert!(count >= 21, "{count}");
}

/// 128-bit float pictures against an independent decoder (which has no
/// alpha support, so those are only checked for sane values).
fn check_against_oracle(name: &str, d: &slerp_jxr::Decoded) {
    let Rgba::F32(mine) = d.to_rgba() else { panic!("{name}: not float") };
    assert!(mine.iter().all(|v| v.is_finite()), "{name}");
    if d.has_alpha {
        return;
    }
    let other = openreadout_jpegxr::decode(&read(&format!("{name}.jxr")), LIMIT)
        .unwrap_or_else(|e| panic!("{name}: oracle: {e}"));
    let channels = other.channels as usize;
    let theirs: Vec<f32> = other.data.chunks_exact(4).map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]])).collect();
    assert_eq!(theirs.len(), (d.width * d.height) as usize * channels, "{name}");
    for (p, px) in theirs.chunks_exact(channels).enumerate() {
        for c in 0..3 {
            let want = px[c.min(channels - 1)];
            assert!(
                (mine[p * 4 + c] - want).abs() <= 1e-6 * want.abs().max(1.0),
                "{name} pixel {p} channel {c}: {} vs {want}",
                mine[p * 4 + c]
            );
        }
    }
}

#[test]
fn alpha_is_kept() {
    for name in ["rgba8_planar", "rgba8_interleaved", "rgba16", "rgba_float", "rgba_half"] {
        let d = decode(&read(&format!("{name}.jxr")), 0, LIMIT).unwrap();
        assert!(d.has_alpha, "{name}");
        let any_transparent = match d.to_rgba() {
            Rgba::U8(v) => v.chunks_exact(4).any(|p| p[3] < 255),
            Rgba::F32(v) => v.chunks_exact(4).any(|p| p[3] < 0.99),
        };
        assert!(any_transparent, "{name} has no transparent pixel");
    }
    assert!(!decode(&read("rgb24.jxr"), 0, LIMIT).unwrap().has_alpha);
}

#[test]
fn reduced_decodes_are_smaller_and_agree_with_the_whole() {
    let bytes = read("rgb24_tiled.jxr");
    let whole = decode(&bytes, 0, LIMIT).unwrap();
    // 160x96: a 40-pixel minimum allows one halving twice (160 -> 40).
    let small = decode(&bytes, 40, LIMIT).unwrap();
    assert_eq!((small.full_width, small.full_height), (160, 96));
    assert_eq!((small.width, small.height), (40, 24));
    let mean = |d: &slerp_jxr::Decoded| {
        let Rgba::U8(v) = d.to_rgba() else { panic!() };
        let n = v.len() / 4;
        (0..3).map(|c| v.chunks_exact(4).map(|p| p[c] as f64).sum::<f64>() / n as f64).collect::<Vec<_>>()
    };
    for (a, b) in mean(&whole).iter().zip(mean(&small)) {
        assert!((a - b).abs() < 3.0, "{a} vs {b}");
    }
    // A minimum above the size leaves the picture whole.
    let same = decode(&bytes, 4000, LIMIT).unwrap();
    assert_eq!((same.width, same.height), (160, 96));
    // jxrlib divides by zero below 1/16, so a tiny minimum stops there.
    let tiny = decode(&bytes, 1, LIMIT).unwrap();
    assert_eq!((tiny.width, tiny.height), (10, 6));
}

#[test]
fn limits_and_garbage() {
    assert!(decode(&read("rgb24_tiled.jxr"), 0, 1000).is_err(), "output limit");
    assert!(decode(b"II\xbc\x01\x08\0\0\0", 0, LIMIT).is_err());
    assert!(decode(&[], 0, LIMIT).is_err());
}

struct XorShift(u64);

impl XorShift {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
}

/// Cuts and corrupts real files; every attempt must return, in time, with
/// a picture or an error, whatever jxrlib does inside its child process.
#[test]
fn damaged_files_are_survived() {
    let mut crashes = 0;
    let mut errors = 0;
    let mut pictures = 0;
    for (k, name) in
        ["rgb24_tiled", "rgba8_planar", "rgb24_yuv420", "rgba_half", "gray16", "rgb565"].into_iter().enumerate()
    {
        let original = read(&format!("{name}.jxr"));
        let mut rng = XorShift(0x9e37_79b9_7f4a_7c15 ^ (k as u64 + 1));
        let mut attempts: Vec<Vec<u8>> = Vec::new();
        for len in (0..original.len()).step_by((original.len() / 40).max(1)) {
            attempts.push(original[..len].to_vec());
        }
        let iterations: usize = std::env::var("SLERP_FUZZ_ITERS").ok().and_then(|v| v.parse().ok()).unwrap_or(60);
        for i in 0..iterations {
            let mut b = original.clone();
            for _ in 0..1 + (rng.next() % 4) as usize {
                // Half the damage lands in the container directory and the
                // headers, where sizes live.
                let at =
                    if i % 2 == 0 { rng.next() as usize % b.len().min(400) } else { rng.next() as usize % b.len() };
                b[at] = match rng.next() % 4 {
                    0 => 0,
                    1 => 0xff,
                    _ => rng.next() as u8,
                };
            }
            attempts.push(b);
        }
        for (i, bytes) in attempts.iter().enumerate() {
            let start = Instant::now();
            match decode(bytes, 0, LIMIT) {
                Ok(d) => {
                    assert!(d.width > 0 && d.height > 0);
                    pictures += 1;
                }
                Err(e) => {
                    errors += 1;
                    if e.to_string().contains("crashed") {
                        crashes += 1;
                    }
                }
            }
            assert!(start.elapsed() < Duration::from_secs(20), "{name} attempt {i}: took {:?}", start.elapsed());
        }
    }
    eprintln!("{pictures} pictures, {errors} errors, of which {crashes} were contained crashes");
}
