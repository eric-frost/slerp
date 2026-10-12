//! Truncated and corrupted files must give an error or a picture, never a
//! panic, a hang or a runaway allocation.  Every sample is cut at many
//! lengths and mutated with a fixed-seed generator, then decoded with both
//! the thumbnail and the full options.

mod common;

use common::*;
use slerp_textures::{DecodeOptions, Format, TextureError, decode, decode_as, thumbnail};
use std::path::Path;
use std::time::{Duration, Instant};

/// A small deterministic generator, so failures reproduce.
struct XorShift(u64);

impl XorShift {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}

/// Decodes `bytes` both ways, failing the test on a caught panic or when a
/// decode is slow enough to suggest a hang.
fn survive(name: &str, format: Option<Format>, bytes: &[u8]) {
    for opts in [DecodeOptions::preview(64), DecodeOptions::full()] {
        let start = Instant::now();
        let r = match format {
            Some(f) => decode_as(f, bytes, &opts),
            None => decode(bytes, &opts),
        };
        match r {
            Ok(tex) => {
                let t = thumbnail(&tex, 64);
                assert!(t.width <= 64 && t.height <= 64 && t.width > 0 && t.height > 0, "{name}: bad thumbnail");
            }
            Err(TextureError::Invalid(m)) if m.contains("internal decoder error") => panic!("{name}: decoder panicked"),
            Err(_) => {}
        }
        assert!(start.elapsed() < Duration::from_secs(5), "{name}: took {:?}", start.elapsed());
    }
}

fn torture(name: &str, format: Option<Format>, original: &[u8], seed: u64) {
    // 1. Every truncation for small files, a spread of them for larger.
    let step = (original.len() / 200).max(1);
    for len in (0..original.len()).step_by(step) {
        survive(&format!("{name} cut at {len}"), format, &original[..len]);
    }
    // 2. Random bytes in the header, where sizes and offsets live, and
    //    anywhere in the file.
    let mut rng = XorShift(seed);
    // SLERP_FUZZ_ITERS raises the count for a longer run.
    let iters = std::env::var("SLERP_FUZZ_ITERS").ok().and_then(|v| v.parse().ok()).unwrap_or(300);
    for i in 0..iters {
        let mut b = original.to_vec();
        for _ in 0..1 + rng.below(4) {
            let at = if i % 2 == 0 { rng.below(b.len().min(160)) } else { rng.below(b.len()) };
            if let Some(byte) = b.get_mut(at) {
                *byte = match rng.below(4) {
                    0 => 0,
                    1 => 0xff,
                    _ => rng.next() as u8,
                };
            }
        }
        survive(&format!("{name} mutation {i}"), format, &b);
    }
}

fn data(name: &str) -> Vec<u8> {
    std::fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data").join(name)).unwrap()
}

#[test]
fn ordinary_images() {
    for (i, (name, format)) in [
        ("rgba.png", None),
        ("interlaced.png", None),
        ("rgb420.jpg", None),
        ("rgba-lossless.webp", None),
        ("rgb-lossy.webp", None),
        ("pal8.bmp", None),
        ("rgba32.bmp", None),
        ("rgba-rle.tga", Some(Format::Tga)),
        ("pal.tga", Some(Format::Tga)),
        ("pal8.pcx", None),
        ("rgb.pcx", None),
    ]
    .into_iter()
    .enumerate()
    {
        torture(name, format, &data(name), 1 + i as u64);
    }
}

#[test]
fn ktx2() {
    let mut cube = Ktx2::new(37, 4, 4, vec![7; 4 * 4 * 4 * 6]);
    cube.faces = 6;
    let mut mips = Ktx2::new(131, 8, 8, vec![]);
    mips.levels = vec![([bc1(1, 2, 3), bc1(4, 5, 6), bc1(7, 8, 9), bc1(1, 1, 1)].concat(), 32), (bc1(3, 3, 3), 8)];
    let mut zst = Ktx2::new(97, 4, 2, vec![]);
    zst.scheme = 2;
    zst.levels = vec![(zstd::bulk::compress(&[0x3c; 64], 3).unwrap(), 64)];
    let mut oriented = Ktx2::new(165, 7, 5, [astc_solid([1, 2, 3, 4]), astc_solid_hdr([0x4000; 4])].concat());
    oriented.kv.push(("KTXorientation", b"ru\0"));
    oriented.kv.push(("KTXswizzle", b"bgr1\0"));
    for (i, k) in [cube, mips, zst, oriented].iter().enumerate() {
        torture(&format!("ktx2 #{i}"), None, &k.build(), 100 + i as u64);
    }
}

#[test]
fn ktx1_dds_astc() {
    let rows = [[1u8, 2, 3, 4, 5, 6, 7, 8, 9, 0, 0, 0], [10, 11, 12, 13, 14, 15, 16, 17, 18, 0, 0, 0]].concat();
    torture("ktx1 rgb", None, &ktx1([0x1401, 0x1907, 0x8051, 0x1907], 3, 2, 1, &[], &[rows]), 200);
    let faces: Vec<Vec<u8>> = (0..6u16).map(|i| bc1(i, !i, 0x5555)).collect();
    torture("ktx1 cube", None, &ktx1([0, 0, 0x83f1, 0x1908], 4, 4, 6, &[], &faces), 201);

    let mut f = dds(8, 8, 2, (FOURCC, b"DX10", 0, [0; 4]), 0, Some((98, 2, 0)));
    f.extend(vec![0x55; (4 + 1) * 16 * 2]);
    torture("dds bc7 array", None, &f, 300);
    let mut f = dds(3, 3, 2, (0x41, &[0; 4], 32, [0xff0000, 0xff00, 0xff, 0xff00_0000]), 0xfe00, None);
    f.extend(vec![0x80; (9 + 1) * 4 * 6]);
    torture("dds legacy cube", None, &f, 301);

    let blocks = [astc_solid([9, 9, 9, 9]), astc_solid_hdr([0x3c00; 4])].concat();
    torture("astc", None, &astc_file(5, 4, 1, 9, 4, &blocks), 400);

    // 3D footprints: 3x2x2 blocks of 3x3x3 texels, arbitrary bits (many
    // blocks malformed, some valid), as a raw file and in KTX2.
    let mut rng = XorShift(401);
    let blocks: Vec<u8> = (0..12 * 16).map(|_| rng.next() as u8).collect();
    let mut f = astc_file(3, 3, 3, 7, 5, &blocks);
    f[13] = 4;
    torture("astc 3d", None, &f, 402);
    let mut k = Ktx2::new(1000288000, 7, 5, blocks);
    k.depth = 4;
    torture("ktx2 astc 3d", None, &k.build(), 403);

    // Video formats: NV12 (two planes) with a mip, NV11, Y410, and the
    // legacy UYVY FourCC, odd sizes where the format allows them.
    let texels: Vec<u8> = (0..4096).map(|_| rng.next() as u8).collect();
    let mut f = dds(6, 4, 2, (FOURCC, b"DX10", 0, [0; 4]), 0, Some((103, 1, 0)));
    f.extend(&texels[..6 * 6 + 3 * 2 * 2]);
    torture("dds nv12", None, &f, 404);
    for (i, (dxgi, size)) in [(110u32, 8 * 3 * 2), (101, 5 * 3 * 4), (108, 3 * 8 * 3)].into_iter().enumerate() {
        let mut f = dds(5, 3, 1, (FOURCC, b"DX10", 0, [0; 4]), 0, Some((dxgi, 1, 0)));
        f.extend(&texels[..size]);
        torture(&format!("dds dxgi {dxgi}"), None, &f, 405 + i as u64);
    }
    let mut f = dds(5, 3, 1, (FOURCC, b"UYVY", 0, [0; 4]), 0, None);
    f.extend(&texels[..3 * 4 * 3]);
    torture("dds uyvy", None, &f, 409);
}

#[test]
fn radiance_hdr() {
    let mut f = b"#?RADIANCE\nFORMAT=32-bit_rle_rgbe\nEXPOSURE=1.0\n\n-Y 3 +X 20\n".to_vec();
    for y in 0..3u8 {
        f.extend([2, 2, 0, 20]);
        for c in 0..4u8 {
            f.extend([130, c + y, 10]);
            f.extend((0..10).map(|x| x * 7 + c));
            f.extend([136, 128 + c]);
        }
    }
    assert!(decode(&f, &DecodeOptions::full()).is_ok());
    torture("hdr", None, &f, 500);
}

/// Files the asset fetcher downloads, when present: every one must decode
/// or fail cleanly, including the KTX conformance suite's invalid files.
#[test]
fn downloaded_assets() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../local/assets/textures");
    let dirs = ["ktx-cts/validate", "ktx-cts/ktx", "dds/permissive", "dds/bc fuzz", "dds/other"];
    let mut files = Vec::new();
    for d in dirs {
        let Ok(walk) = walk(&root.join(d)) else { continue };
        files.extend(walk);
    }
    for path in &files {
        let bytes = std::fs::read(path).unwrap();
        survive(&path.display().to_string(), None, &bytes);
    }
    eprintln!("checked {} downloaded files", files.len());
}

/// Small downloaded files in the codecs this crate doesn't decode itself
/// (BasisLZ/ETC1S, UASTC, UASTC HDR 6x6 in `basisu`), mutated like the
/// built ones.  Release builds only: `basisu` relies on wrapping arithmetic
/// in places, which debug builds report as overflow panics.
#[test]
#[cfg_attr(debug_assertions, ignore = "run with --release")]
fn downloaded_basis_files() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../local/assets");
    let files = [
        "textures/ktx-software/ktx2/alpha_simple_blze.ktx2",
        "textures/ktx-software/ktx2/r8g8b8a8_srgb_mip_blze.ktx2",
        "textures/ktx-cts/ktx2_sample/rg_reference_basis.ktx2",
        "textures/ktx-cts/ktx2_sample/rg_reference_uastc.ktx2",
        "textures/ktx-cts/ktx2_sample/luminance_alpha_reference_uastc.ktx2",
        "textures/ktx-cts/ktx2_sample/astc_ldr_6x6_arraytex_7_mipmap.ktx2",
        "textures/ktx-cts/ktx2/valid_R8G8B8A8_SRGB_2D_UASTC_ZLIB_9.ktx2",
        "textures/ktx-cts/ktx2/valid_ASTC_4x4_UNORM_BLOCK_2D_ZSTD_9.ktx2",
        "textures/ktx-cts/ktx2_sample/rgba-mipmap-reference-basis.ktx2",
        "textures/ktx-software/ktx2/Desk_uastc_hdr6x6i.ktx2",
        "textures-extra/basisu-2.50/ldr_6x6i.ktx2",
        "textures-extra/basisu-2.50/ldr_12x12i.ktx2",
        "textures-extra/basisu-2.50/xubc7.ktx2",
    ];
    for (i, f) in files.iter().enumerate() {
        if let Ok(bytes) = std::fs::read(root.join(f)) {
            torture(f, None, &bytes, 600 + i as u64);
        }
    }
}

fn walk(dir: &Path) -> std::io::Result<Vec<std::path::PathBuf>> {
    let mut out = Vec::new();
    for e in std::fs::read_dir(dir)? {
        let p = e?.path();
        if p.is_dir() {
            out.extend(walk(&p)?);
        } else {
            out.push(p);
        }
    }
    out.sort();
    Ok(out)
}
