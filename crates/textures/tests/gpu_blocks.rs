//! Basis Universal textures decoded for a GPU stay in BC7, BC5 or ASTC
//! blocks, which decode back to (nearly) the texels the RGBA path gives,
//! and damaged files still never panic.

use slerp_textures::{
    BlockFormat, DecodeOptions, GpuBlocks, Pixels, TextureError, decode, decode_for_gpu, decompress, thumbnail,
};
use std::path::{Path, PathBuf};

fn samples() -> Vec<PathBuf> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../local/assets");
    let mut out = Vec::new();
    let mut dirs = vec![root.join("textures/ktx-software"), root.join("textures/ktx-cts/ktx2_sample")];
    while let Some(dir) = dirs.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                dirs.push(p);
            } else if p.extension().is_some_and(|x| x == "ktx2") {
                out.push(p);
            }
        }
    }
    out.sort();
    out
}

/// Basis files whose level 0 is at most this many texels, to keep the test
/// quick in debug builds.
const MAX_TEXELS: u32 = 1024 * 1024;

fn mean_difference(a: &[u8], b: &[u8], channels: &[usize]) -> f64 {
    let mut sum = 0u64;
    for (p, q) in a.chunks_exact(4).zip(b.chunks_exact(4)) {
        for &c in channels {
            sum += p[c].abs_diff(q[c]) as u64;
        }
    }
    sum as f64 / (a.len() / 4 * channels.len()).max(1) as f64
}

#[test]
fn basis_textures_stay_in_blocks_that_match_their_rgba() {
    let files = samples();
    if files.is_empty() {
        eprintln!("skipping: no KTX2 samples in local/assets");
        return;
    }
    let mut kept = [0usize; 5];
    for path in files {
        let bytes = std::fs::read(&path).unwrap();
        // Basis Universal only (VK_FORMAT_UNDEFINED), small enough.
        if bytes.len() < 48 || bytes[12..16] != [0; 4] {
            continue;
        }
        let w = u32::from_le_bytes(bytes[20..24].try_into().unwrap());
        let h = u32::from_le_bytes(bytes[24..28].try_into().unwrap());
        if w.saturating_mul(h) > MAX_TEXELS {
            continue;
        }
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        let Ok(rgba) = decode(&bytes, &DecodeOptions::full()) else { continue };
        for (gpu, formats) in [
            (
                GpuBlocks { bc: true, astc: false },
                &[BlockFormat::Bc1, BlockFormat::Bc3, BlockFormat::Bc5, BlockFormat::Bc7][..],
            ),
            (GpuBlocks { bc: false, astc: true }, &[BlockFormat::Astc4x4][..]),
        ] {
            let tex = decode_for_gpu(&bytes, &DecodeOptions::full(), gpu).unwrap_or_else(|e| panic!("{name}: {e}"));
            let Pixels::Blocks(format, _) = &tex.levels[0].surfaces[0] else {
                assert!(!slerp_textures::is_compressed(&tex), "{name}: mixed blocks and texels");
                continue;
            };
            assert!(formats.contains(format), "{name}: {format:?} for {gpu:?}");
            kept[match format {
                BlockFormat::Bc1 => 0,
                BlockFormat::Bc3 => 1,
                BlockFormat::Bc5 => 2,
                BlockFormat::Bc7 => 3,
                BlockFormat::Astc4x4 => 4,
            }] += 1;
            // 1. The same levels, sizes, colour space and format name.
            assert_eq!(tex.levels.len(), rgba.levels.len(), "{name}");
            assert_eq!(tex.color_space, rgba.color_space, "{name}");
            assert_eq!(tex.source.format, rgba.source.format, "{name}");
            for (a, b) in tex.levels.iter().zip(&rgba.levels) {
                assert_eq!((a.width, a.height, a.surfaces.len()), (b.width, b.height, b.surfaces.len()), "{name}");
                let Pixels::Blocks(_, data) = &a.surfaces[0] else { panic!("{name}: level not in blocks") };
                assert_eq!(
                    data.len(),
                    (a.width.div_ceil(4) * a.height.div_ceil(4)) as usize * format.block_bytes(),
                    "{name}"
                );
            }
            // 2. Decoded back, close to the RGBA path's texels: a few levels
            //    apart on average, BC5 compared in red and green only.
            let back = decompress(&tex);
            let channels: &[usize] = if *format == BlockFormat::Bc5 { &[0, 1] } else { &[0, 1, 2, 3] };
            for level in [0, back.levels.len() / 2] {
                let (Pixels::Rgba8(a), Pixels::Rgba8(b)) =
                    (&back.levels[level].surfaces[0], &rgba.levels[level].surfaces[0])
                else {
                    panic!("{name}: not 8-bit")
                };
                let diff = mean_difference(a, b, channels);
                assert!(diff < 4.0, "{name} {format:?} level {level}: off by {diff:.2} on average");
            }
            // 3. Thumbnails read blocks too.
            let t = thumbnail(&tex, 64);
            assert!(t.width > 0 && t.width <= 64, "{name}");
        }
    }
    eprintln!("kept as BC1, BC3, BC5, BC7, ASTC: {kept:?}");
    // The samples have no ETC1S with alpha, so BC3 may not come up.
    assert!([0, 2, 3, 4].iter().all(|&i| kept[i] > 0), "every block format came up: {kept:?}");
}

/// What can't stay in blocks decodes to RGBA as before: a thumbnail's one
/// level, and anything without a GPU format.
#[test]
fn partial_requests_decode_to_rgba() {
    let Some(path) =
        samples().into_iter().find(|p| p.file_name().is_some_and(|n| n == "ktx_document_uastc_rdo_4_zstd_5.ktx2"))
    else {
        eprintln!("skipping: sample missing");
        return;
    };
    let bytes = std::fs::read(path).unwrap();
    let bc = GpuBlocks { bc: true, astc: false };
    let tex = decode_for_gpu(&bytes, &DecodeOptions::preview(128), bc).unwrap();
    assert!(!slerp_textures::is_compressed(&tex));
    let tex = decode_for_gpu(&bytes, &DecodeOptions::full(), GpuBlocks::default()).unwrap();
    assert!(!slerp_textures::is_compressed(&tex));
    let tex = decode_for_gpu(&bytes, &DecodeOptions::full(), bc).unwrap();
    assert!(slerp_textures::is_compressed(&tex));
}

/// Cut and scrambled Basis files decoded for a GPU give an error or a
/// texture, never a panic.
#[test]
fn damaged_basis_files_never_panic_on_the_block_path() {
    let wanted = ["ktx_document_blze.ktx2", "ktx_document_uastc_rdo_4_zstd_5.ktx2", "Iron_Bars_001_normal_blze.ktx2"];
    let files: Vec<PathBuf> =
        samples().into_iter().filter(|p| p.file_name().is_some_and(|n| wanted.iter().any(|w| n == *w))).collect();
    if files.is_empty() {
        eprintln!("skipping: samples missing");
        return;
    }
    let gpu = GpuBlocks { bc: true, astc: true };
    let mut seed = 0x9e37_79b9_7f4a_7c15u64;
    for path in files {
        let original = std::fs::read(&path).unwrap();
        let check = |bytes: &[u8], what: &str| match decode_for_gpu(bytes, &DecodeOptions::full(), gpu) {
            Err(TextureError::Invalid(m)) if m.contains("internal decoder error") => panic!("{what}: panicked"),
            Ok(t) => {
                let _ = decompress(&t);
            }
            Err(_) => {}
        };
        for len in (0..original.len()).step_by((original.len() / 60).max(1)) {
            check(&original[..len], &format!("{} cut at {len}", path.display()));
        }
        for i in 0..80 {
            let mut b = original.clone();
            for _ in 0..4 {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                let at = (seed % b.len() as u64) as usize;
                b[at] = (seed >> 32) as u8;
            }
            check(&b, &format!("{} mutation {i}", path.display()));
        }
    }
}
