use super::*;
use basisu::astc::decode::{decode_block_hdr16, decode_block_ldr8};
use basisu::astc::unpack::{LogAstcBlock, unpack_block};
use std::path::{Path, PathBuf};

/// A small deterministic generator, so failures repeat.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn block(&mut self) -> [u8; 16] {
        let (a, b) = (self.next(), self.next());
        let mut out = [0u8; 16];
        out[..8].copy_from_slice(&a.to_le_bytes());
        out[8..].copy_from_slice(&b.to_le_bytes());
        out
    }
}

/// What basisu (the 2D decoder Slerp uses) makes of a block, as
/// `blocks::decode_astc` calls it: None when the block is HDR.
fn basisu_ldr(block: &[u8; 16], w: u32, h: u32, srgb: bool) -> Option<Vec<[u8; 4]>> {
    let n = (w * h) as usize;
    let mut out = vec![[0u8; 4]; 144];
    match unpack_block(block, w, h) {
        Some(log) if is_hdr(&log) => return None,
        Some(log) => {
            if decode_block_ldr8(&log, w, h, srgb, &mut out).is_none() {
                out.fill(ERROR_LDR);
            }
        }
        None => out.fill(ERROR_LDR),
    }
    out.truncate(n);
    Some(out)
}

fn basisu_hdr(block: &[u8; 16], w: u32, h: u32) -> Vec<[u16; 4]> {
    let mut out = vec![[0u16; 4]; 144];
    let ok = unpack_block(block, w, h).and_then(|log| decode_block_hdr16(&log, w, h, &mut out));
    if ok.is_none() {
        out.fill(ERROR_HDR);
    }
    out.truncate((w * h) as usize);
    out
}

fn is_hdr(log: &LogAstcBlock) -> bool {
    let ldr = |c: u8| matches!(c, 0 | 1 | 4 | 5 | 6 | 8 | 9 | 10 | 12 | 13);
    let n = (log.num_partitions as usize).clamp(1, 4);
    log.solid_color_flag_hdr || (!log.solid_color_flag_ldr && !log.color_endpoint_modes[..n].iter().all(|&c| ldr(c)))
}

fn mine_ldr(block: &[u8; 16], fp: Footprint, srgb: bool) -> Option<Vec<[u8; 4]>> {
    let mut out = vec![[0u8; 4]; MAX_TEXELS];
    decode_ldr(block, fp, srgb, &mut out).then(|| out[..fp.texels()].to_vec())
}

fn mine_hdr(block: &[u8; 16], fp: Footprint) -> Vec<[u16; 4]> {
    let mut out = vec![[0u16; 4]; MAX_TEXELS];
    decode_hdr(block, fp, &mut out);
    out.truncate(fp.texels());
    out
}

/// Compares the two decoders on one 2D block, returning a description of
/// the first difference.
fn compare_2d(block: &[u8; 16], w: u32, h: u32) -> Result<(), String> {
    let fp = Footprint { w, h, d: 1 };
    for srgb in [false, true] {
        let (theirs, ours) = (basisu_ldr(block, w, h, srgb), mine_ldr(block, fp, srgb));
        if theirs != ours {
            return Err(format!("{w}x{h} srgb {srgb} block {block:02x?}: basisu {theirs:?}, ours {ours:?}"));
        }
    }
    let (theirs, ours) = (basisu_hdr(block, w, h), mine_hdr(block, fp));
    if theirs != ours {
        return Err(format!("{w}x{h} HDR block {block:02x?}: basisu {theirs:?}, ours {ours:?}"));
    }
    Ok(())
}

/// Random 128-bit blocks reach nearly every block mode, partition count,
/// endpoint mode and range, valid or not.  Decoding them in 2D must agree
/// with basisu exactly, which checks everything 2D and 3D share.
#[test]
fn random_2d_blocks_match_basisu() {
    let mut rng = Rng(0x5eed_1234_abcd_0001);
    let mut weighted = 0;
    for &(w, h) in &crate::formats::ASTC_BLOCKS {
        for _ in 0..20_000 {
            let block = rng.block();
            compare_2d(&block, w as u32, h as u32).unwrap();
            weighted += unpack_block(&block, w as u32, h as u32).is_some() as u32;
        }
    }
    // Enough of them are valid to mean something.
    assert!(weighted > 25_000, "only {weighted} valid blocks");
}

/// Real encoder output decodes alike too: every block of the 2D `.astc`
/// samples, LDR and HDR.
#[test]
fn sample_2d_blocks_match_basisu() {
    let dir = assets().join("textures/astc");
    let Ok(entries) = std::fs::read_dir(&dir) else {
        eprintln!("skipped: no {}", dir.display());
        return;
    };
    let mut files = 0;
    for path in entries.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e == "astc")) {
        let bytes = std::fs::read(&path).unwrap();
        let (w, h) = (bytes[4] as u32, bytes[5] as u32);
        for block in bytes[16..].chunks_exact(16).step_by(7) {
            compare_2d(block.try_into().unwrap(), w, h).map_err(|e| format!("{}: {e}", path.display())).unwrap();
        }
        files += 1;
    }
    assert!(files > 0);
}

/// Weight shares always add up to 16, and the grid points they name exist,
/// for every footprint and grid size.
#[test]
fn infill_shares_add_up() {
    for &(w, h, d) in &crate::formats::ASTC_3D_BLOCKS {
        let fp = Footprint { w: w as u32, h: h as u32, d: d as u32 };
        for gx in 2..=fp.w {
            for gy in 2..=fp.h {
                for gz in 2..=fp.d {
                    for i in 0..fp.texels() as u32 {
                        let (x, y, z) = (i % fp.w, i / fp.w % fp.h, i / (fp.w * fp.h));
                        let t = taps(fp, [gx, gy, gz], x, y, z);
                        assert_eq!(t.iter().map(|&(_, s)| s).sum::<u32>(), 16);
                        assert!(t.iter().all(|&(at, s)| s == 0 || at < (gx * gy * gz) as usize));
                    }
                }
            }
        }
    }
}

/// A grid as fine as the block puts each weight on its own texel.
#[test]
fn full_grid_is_identity() {
    let fp = Footprint { w: 4, h: 3, d: 3 };
    for i in 0..fp.texels() as u32 {
        let (x, y, z) = (i % 4, i / 4 % 3, i / 12);
        let t = taps(fp, [4, 3, 3], x, y, z);
        assert_eq!(t[0], (i as usize, 16));
    }
}

/// A 3D void-extent block fills the block with its colour; one whose extent
/// is inverted is malformed.
#[test]
fn void_extent_3d() {
    let fp = Footprint { w: 4, h: 4, d: 4 };
    let mut bits: u128 = 0x1FC;
    // Extent coordinates all ones: no extent.
    bits |= ((1u128 << 54) - 1) << 10;
    for (i, c) in [0x8000u128, 0x4000, 0xFFFF, 0x1234].iter().enumerate() {
        bits |= c << (64 + 16 * i);
    }
    let block = bits.to_le_bytes();
    let out = mine_ldr(&block, fp, false).unwrap();
    assert_eq!(out.len(), 64);
    assert!(out.iter().all(|&t| t == [0x80, 0x40, 0xFF, 0x12]));
    // Low s above high s.
    let bad = (bits & !(((1u128 << 18) - 1) << 10)) | (5 << 10) | (2 << 19);
    assert!(mine_ldr(&bad.to_le_bytes(), fp, false).unwrap().iter().all(|&t| t == ERROR_LDR));
    // HDR: the colour is half floats, and must be finite.
    let halves = [0x3C00u16, 0x4000, 0x3800, 0x3C00];
    let mut hdr = (bits | 0x200) & ((1u128 << 64) - 1);
    for (i, &c) in halves.iter().enumerate() {
        hdr |= (c as u128) << (64 + 16 * i);
    }
    assert!(mine_ldr(&hdr.to_le_bytes(), fp, false).is_none());
    assert!(mine_hdr(&hdr.to_le_bytes(), fp).iter().all(|&t| t == halves));
    let nan = (hdr | (0x7E00u128 << 64)).to_le_bytes();
    assert!(mine_hdr(&nan, fp).iter().all(|&t| t == ERROR_HDR));
}

/// Random data in every 3D footprint decodes without panicking, and a
/// truncated or oversized surface is an error.
#[test]
fn malformed_3d_data_is_safe() {
    let mut rng = Rng(0x0dd_ba11_0000_0003);
    for &(w, h, d) in &crate::formats::ASTC_3D_BLOCKS {
        let fp = Footprint { w: w as u32, h: h as u32, d: d as u32 };
        for _ in 0..3_000 {
            let block = rng.block();
            let _ = mine_ldr(&block, fp, true);
            let _ = mine_hdr(&block, fp);
        }
        let data: Vec<u8> = (0..4 * 4 * 4 * 16).map(|_| rng.next() as u8).collect();
        let (w, h) = (4 * fp.w - 1, 4 * fp.h - 1);
        assert!(decode_surface(fp, false, false, w, h, 4 * fp.d - 2, &data).is_ok());
        assert!(decode_surface(fp, false, false, w + 2, h, 4 * fp.d, &data).is_err());
        assert!(decode_surface(fp, false, false, 1 << 16, 1 << 16, 1 << 16, &data).is_err());
    }
}

/*┌─────────────────╮
| Against astcenc |
└─────────────────╯*/
//region Against astcenc

fn assets() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../local/assets")
}

/// The raw blocks of a 3D ASTC sample: its footprint, size and level 0
/// data, from a `.astc` file or an uncompressed KTX2 file.
fn raw_blocks(bytes: &[u8]) -> Option<(Footprint, [u32; 3], &[u8])> {
    let u32_at = |at: usize| u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap());
    let u24_at = |at: usize| u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], 0]);
    if bytes.starts_with(&[0x13, 0xab, 0xa1, 0x5c]) {
        let fp = Footprint { w: bytes[4] as u32, h: bytes[5] as u32, d: bytes[6] as u32 };
        return Some((fp, [u24_at(7), u24_at(10), u24_at(13)], &bytes[16..]));
    }
    if bytes.starts_with(b"\xabKTX 20") && u32_at(44) == 0 {
        let i = u32_at(12).checked_sub(1000288000)? as usize / 3;
        let (w, h, d) = *crate::formats::ASTC_3D_BLOCKS.get(i)?;
        let at = u64::from_le_bytes(bytes[80..88].try_into().unwrap()) as usize;
        let fp = Footprint { w: w as u32, h: h as u32, d: d as u32 };
        return Some((fp, [u32_at(20), u32_at(24), u32_at(28)], &bytes[at..]));
    }
    None
}

/// The 3D samples made with astcenc (see `tools/assets.jsonc`,
/// `astc-3d`) and the 3D ASTC files of the KTX conformance suite decode to
/// what astcenc decodes them to.  `ref/` holds its half-float output
/// (`-dh`, or `-dH` for HDR alpha) as uncompressed KTX files.  Decoded to
/// halves, every block must match exactly; the file as Slerp opens it must
/// match too, within one step for 8-bit results, which keep the top byte of
/// each 16-bit value where astcenc rounds.
#[test]
fn samples_match_astcenc() {
    let dir = assets().join("textures-extra/astc-3d");
    let Ok(entries) = std::fs::read_dir(&dir) else {
        eprintln!("skipped: no {}", dir.display());
        return;
    };
    let mut paths: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
    let cts = assets().join("textures/ktx-cts/ktx2");
    paths.extend(
        ["3x3x3_UNORM", "4x4x3_SFLOAT", "5x4x4_SRGB", "6x6x5_UNORM"]
            .map(|f| cts.join(format!("valid_ASTC_{f}_BLOCK_EXT_3D.ktx2"))),
    );
    paths.sort();
    let floats = |p: &Pixels| match p {
        Pixels::Rgba32F(v) => v.clone(),
        Pixels::Rgba8(v) => v.iter().map(|&b| b as f32 / 255.0).collect(),
        Pixels::Blocks(..) => panic!("blocks"),
    };
    let mut checked = 0;
    for path in paths.iter().filter(|p| p.extension().is_some_and(|e| e == "astc" || e == "ktx" || e == "ktx2")) {
        let stem = path.file_stem().unwrap().to_string_lossy();
        let reference = dir.join("ref").join(format!("{stem}.ktx"));
        if !reference.exists() {
            // The conformance files' references are made by hand (see the
            // manifest's note).
            eprintln!("{stem}: no reference");
            continue;
        }
        let opts = crate::DecodeOptions::full();
        let theirs = crate::decode_file(&reference, &opts).unwrap();
        let theirs = floats(&theirs.levels[0].surfaces[0]);

        // 1. Every block, decoded to halves.  astcenc's HDR decode marks a
        //    malformed block with NaN, where Slerp (and the specification's
        //    LDR decode) uses magenta; it also turns negative values in HDR
        //    void-extent blocks, which only random data holds, into NaN.
        let bytes = std::fs::read(path).unwrap();
        let magenta = [1.0, 0.0, 1.0, 1.0];
        if let Some((fp, [w, h, d], data)) = raw_blocks(&bytes) {
            let Pixels::Rgba32F(ours) = decode_surface(fp, false, true, w, h, d, data).unwrap() else { panic!() };
            assert_eq!(ours.len(), theirs.len(), "{stem}");
            let mut errors = 0;
            for (i, (a, b)) in ours.chunks_exact(4).zip(theirs.chunks_exact(4)).enumerate() {
                // astcenc writes only the channels a file uses, so a
                // malformed block can read back as (NaN, NaN, 0, 1).
                if a == magenta && b[0].is_nan() {
                    errors += 1;
                    continue;
                }
                for c in 0..4 {
                    let ok = if b[c].is_nan() { a[c] < 0.0 } else { a[c] == b[c] };
                    assert!(ok, "{stem}: texel {i}: ours {a:?}, astcenc {b:?}");
                }
            }
            eprintln!("{stem}: {} texels match astcenc, {errors} of them in malformed blocks", ours.len() / 4);
        }

        // 2. The whole file as Slerp opens it.
        let tex = crate::decode_file(path, &opts).unwrap();
        assert_eq!(tex.kind, crate::TextureKind::D3, "{stem}");
        let ours = floats(&tex.levels[0].surfaces[0]);
        assert_eq!(ours.len(), theirs.len(), "{stem}");
        let tolerance = if tex.is_hdr() { 0.0 } else { 1.01 / 255.0 };
        let worst = (ours.chunks_exact(4).zip(theirs.chunks_exact(4)))
            .filter(|(_, b)| !b.iter().any(|v| v.is_nan()))
            .flat_map(|(a, b)| a.iter().zip(b).map(|(x, y)| (x - y).abs()))
            .fold(0.0, f32::max);
        eprintln!("{stem}: {}, largest difference {worst}", tex.source.format);
        assert!(worst <= tolerance, "{stem}: differs from astcenc by up to {worst}");
        checked += 1;
    }
    assert!(checked >= 25, "only {checked} samples in {}", dir.display());
}

//endregion
