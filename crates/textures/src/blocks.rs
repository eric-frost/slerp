//! Decodes one surface of any `PixelFormat` to RGBA: block-compressed
//! formats through bcdec_rs (BC1-7, BC6H), texture2ddecoder (ETC, EAC, ATC,
//! PVRTC) and basisu's ASTC decoder (LDR and HDR), and uncompressed texels
//! here.  Rows of blocks decode in parallel.

use crate::color::{half_to_f32, rgb9e5, small_ufloat};
use crate::formats::{Codec, Dest, Num, PixelFormat, Plain};
use crate::util::{check_size, invalid};
use crate::{Pixels, TextureError};
use rayon::prelude::*;

/// Largest block footprint (ASTC 12x12).
const MAX_TEXELS: usize = 144;

/// Below this many texels a surface decodes on the calling thread, since
/// handing small work to the pool costs more than it saves.
const PARALLEL_TEXELS: usize = 1 << 16;

/// Decodes a surface of GPU blocks kept by `decode_for_gpu` to RGBA.
pub(crate) fn decode_gpu_blocks(
    format: crate::BlockFormat,
    w: u32,
    h: u32,
    data: &[u8],
    srgb: bool,
) -> Result<Pixels, TextureError> {
    use crate::thumb::Channels;
    let fmt = match format {
        crate::BlockFormat::Bc1 => {
            PixelFormat { name: "BC1", codec: Codec::Bc1 { alpha: false }, srgb, channels: Channels::Rgba }
        }
        crate::BlockFormat::Bc3 => PixelFormat { name: "BC3", codec: Codec::Bc3, srgb, channels: Channels::Rgba },
        crate::BlockFormat::Bc7 => PixelFormat { name: "BC7", codec: Codec::Bc7, srgb, channels: Channels::Rgba },
        crate::BlockFormat::Bc5 => {
            PixelFormat { name: "BC5", codec: Codec::Bc5 { signed: false }, srgb: false, channels: Channels::Rg }
        }
        crate::BlockFormat::Astc4x4 => PixelFormat {
            name: "ASTC",
            codec: Codec::Astc { w: 4, h: 4, d: 1, hdr: false },
            srgb,
            channels: Channels::Rgba,
        },
    };
    match format {
        crate::BlockFormat::Astc4x4 => decode_astc(&fmt, w, h, 1, data, w.div_ceil(4) as usize * 16),
        _ => decode_surface(&fmt, w, h, 1, data, None),
    }
}

/// Decodes `depth` slices of `w` x `h` texels.  `row_pitch` is the distance
/// between rows of blocks when it isn't tight (KTX1 pads rows to four
/// bytes).
pub(crate) fn decode_surface(
    fmt: &PixelFormat,
    w: u32,
    h: u32,
    depth: u32,
    data: &[u8],
    row_pitch: Option<usize>,
) -> Result<Pixels, TextureError> {
    let codec = fmt.codec;
    let (bw, bh) = codec.block();
    let (bx, by) = codec.grid(w, h);
    let (bx, by) = (bx as usize, by as usize);
    let row = row_pitch.unwrap_or(bx * codec.block_bytes()).max(bx * codec.block_bytes());
    let slice = row * by;
    if let Codec::Pvrtc { bpp } = codec {
        return decode_pvrtc(w, h, depth, data, bpp == 2);
    }
    // 3D ASTC blocks hold several slices each.
    let block_slices = depth.div_ceil(codec.block_depth()) as usize;
    let need = slice.checked_mul(block_slices).ok_or_else(|| invalid("surface too large"))?;
    if data.len() < need {
        return Err(invalid(format!("{}: {} bytes of data for a level that needs {need}", fmt.name, data.len())));
    }
    let float = codec.float_output();
    check_size(w, h, depth, if float { 16 } else { 4 })?;
    let texels = w as usize * h as usize;
    let out_slice = texels * 4;

    if let Codec::Astc { .. } = codec {
        return decode_astc(fmt, w, h, depth, data, row);
    }
    if let Codec::Yuv(layout) = codec {
        return Ok(crate::yuv::decode(layout, w, h, depth, data, row, slice));
    }
    if float {
        let mut out = vec![0f32; out_slice * depth as usize];
        for (z, dst) in out.chunks_exact_mut(out_slice).enumerate() {
            let src = &data[z * slice..(z + 1) * slice];
            if let Codec::Plain(p) = codec {
                let tb = p.texel_bytes();
                dst.par_chunks_mut(w as usize * 4).enumerate().for_each(|(y, d)| {
                    for (x, o) in d.chunks_exact_mut(4).enumerate() {
                        let at = y * row + x * tb;
                        o.copy_from_slice(&plain_f32(&p, &src[at..at + tb]));
                    }
                });
                continue;
            }
            each_block_row(dst, w, bh, texels, |by_i, rows| {
                let mut tmp = [[0f32; 4]; MAX_TEXELS];
                for bx_i in 0..bx {
                    let at = by_i * row + bx_i * codec.block_bytes();
                    decode_block_f32(&codec, &src[at..at + codec.block_bytes()], &mut tmp);
                    copy_block(&tmp, bw, bh, bx_i, w, rows);
                }
            });
        }
        Ok(Pixels::Rgba32F(out))
    } else {
        let mut out = vec![0u8; out_slice * depth as usize];
        for (z, dst) in out.chunks_exact_mut(out_slice).enumerate() {
            let src = &data[z * slice..(z + 1) * slice];
            if let Codec::Plain(p) = codec {
                decode_plain_u8(&p, src, w, row, dst);
                continue;
            }
            each_block_row(dst, w, bh, texels, |by_i, rows| {
                let mut tmp = [[0u8; 4]; MAX_TEXELS];
                for bx_i in 0..bx {
                    let at = by_i * row + bx_i * codec.block_bytes();
                    decode_block_u8(&codec, &src[at..at + codec.block_bytes()], &mut tmp);
                    copy_block(&tmp, bw, bh, bx_i, w, rows);
                }
            });
        }
        Ok(Pixels::Rgba8(out))
    }
}

/// Runs `f(block_row, output_rows)` for every row of blocks, in parallel for
/// large surfaces.  `output_rows` is the slice of the output those blocks
/// cover (shorter for the last row when the height isn't a multiple of the
/// block height).
fn each_block_row<T: Send>(dst: &mut [T], w: u32, bh: u32, texels: usize, f: impl Fn(usize, &mut [T]) + Sync) {
    let chunk = w as usize * bh as usize * 4;
    if texels >= PARALLEL_TEXELS {
        dst.par_chunks_mut(chunk).enumerate().for_each(|(i, rows)| f(i, rows));
    } else {
        dst.chunks_mut(chunk).enumerate().for_each(|(i, rows)| f(i, rows));
    }
}

/// Copies the visible part of a decoded block into its rows of the output.
#[inline]
fn copy_block<T: Copy>(tmp: &[[T; 4]], bw: u32, bh: u32, bx: usize, w: u32, rows: &mut [T]) {
    let x0 = bx * bw as usize;
    let cols = (bw as usize).min(w as usize - x0);
    let rows_here = rows.len() / (w as usize * 4);
    for y in 0..(bh as usize).min(rows_here) {
        let dst = &mut rows[(y * w as usize + x0) * 4..(y * w as usize + x0 + cols) * 4];
        for (x, d) in dst.chunks_exact_mut(4).enumerate() {
            d.copy_from_slice(&tmp[y * bw as usize + x]);
        }
    }
}

/*┌─────────────────╮
| Compressed      |
└─────────────────╯*/
//region Compressed

fn unpack_argb(src: &[u32], out: &mut [[u8; 4]]) {
    // texture2ddecoder packs texels as little-endian BGRA.
    for (o, &v) in out.iter_mut().zip(src) {
        let [b, g, r, a] = v.to_le_bytes();
        *o = [r, g, b, a];
    }
}

fn decode_block_u8(codec: &Codec, b: &[u8], out: &mut [[u8; 4]; MAX_TEXELS]) {
    let mut raw = [0u8; 64];
    let mut argb = [0u32; 16];
    let unorm = |v: f32| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
    let snorm = |v: f32| ((v.clamp(-1.0, 1.0) * 0.5 + 0.5) * 255.0 + 0.5) as u8;
    match *codec {
        Codec::Bc1 { alpha } => {
            bcdec_rs::bc1(b, &mut raw, 16);
            copy_raw(&raw, out);
            if !alpha {
                out[..16].iter_mut().for_each(|p| p[3] = 255);
            }
        }
        Codec::Bc2 => {
            bcdec_rs::bc2(b, &mut raw, 16);
            copy_raw(&raw, out);
        }
        Codec::Bc3 => {
            bcdec_rs::bc3(b, &mut raw, 16);
            copy_raw(&raw, out);
        }
        Codec::Bc7 => {
            bcdec_rs::bc7(b, &mut raw, 16);
            copy_raw(&raw, out);
        }
        Codec::Bc4 { signed } => {
            let mut f = [0f32; 16];
            bcdec_rs::bc4_float(b, &mut f, 4, signed);
            for (o, v) in out.iter_mut().zip(f) {
                let r = if signed { snorm(v) } else { unorm(v) };
                *o = [r, 0, 0, 255];
            }
        }
        Codec::Bc5 { signed } => {
            let mut f = [0f32; 32];
            bcdec_rs::bc5_float(b, &mut f, 8, signed);
            for (o, v) in out.iter_mut().zip(f.chunks_exact(2)) {
                *o = if signed { [snorm(v[0]), snorm(v[1]), 0, 255] } else { [unorm(v[0]), unorm(v[1]), 0, 255] };
            }
        }
        Codec::Etc1 => {
            texture2ddecoder::decode_etc1_block(b, &mut argb);
            unpack_argb(&argb, out);
        }
        Codec::Etc2Rgb => {
            texture2ddecoder::decode_etc2_rgb_block(b, &mut argb);
            unpack_argb(&argb, out);
        }
        Codec::Etc2Rgba1 => {
            texture2ddecoder::decode_etc2_rgba1_block(b, &mut argb);
            unpack_argb(&argb, out);
        }
        Codec::Etc2Rgba => {
            texture2ddecoder::decode_etc2_rgba8_block(b, &mut argb);
            unpack_argb(&argb, out);
        }
        Codec::EacR { signed } => {
            if signed {
                texture2ddecoder::decode_eacr_signed_block(b, &mut argb);
            } else {
                texture2ddecoder::decode_eacr_block(b, &mut argb);
            }
            unpack_argb(&argb, out);
            out[..16].iter_mut().for_each(|p| *p = [p[0], 0, 0, 255]);
        }
        Codec::EacRg { signed } => {
            if signed {
                texture2ddecoder::decode_eacrg_signed_block(b, &mut argb);
            } else {
                texture2ddecoder::decode_eacrg_block(b, &mut argb);
            }
            unpack_argb(&argb, out);
            out[..16].iter_mut().for_each(|p| *p = [p[0], p[1], 0, 255]);
        }
        Codec::AtcRgb => {
            texture2ddecoder::decode_atc_rgb4_block(b, &mut argb);
            unpack_argb(&argb, out);
        }
        Codec::AtcRgba => {
            texture2ddecoder::decode_atc_rgba8_block(b, &mut argb);
            unpack_argb(&argb, out);
        }
        Codec::Pairs422 { bytes, g_first } => {
            let c = |i: usize| if bytes == 1 { b[i] } else { b[i * 2 + 1] };
            let (g0, bl, g1, r) = if g_first { (c(0), c(1), c(2), c(3)) } else { (c(1), c(0), c(3), c(2)) };
            out[0] = [r, g0, bl, 255];
            out[1] = [r, g1, bl, 255];
        }
        // Handled by their own paths.
        Codec::Bc6h { .. } | Codec::Astc { .. } | Codec::Pvrtc { .. } | Codec::Yuv(_) | Codec::Plain(_) => {}
    }
}

fn copy_raw(raw: &[u8; 64], out: &mut [[u8; 4]]) {
    for (o, p) in out.iter_mut().zip(raw.chunks_exact(4)) {
        *o = [p[0], p[1], p[2], p[3]];
    }
}

/// Decodes a block to floats.  BC6H is the only block format that needs
/// them; ASTC HDR and plain float texels have their own paths.
fn decode_block_f32(codec: &Codec, b: &[u8], out: &mut [[f32; 4]; MAX_TEXELS]) {
    if let Codec::Bc6h { signed } = *codec {
        let mut f = [0f32; 48];
        bcdec_rs::bc6h_float(b, &mut f, 12, signed);
        for (o, p) in out.iter_mut().zip(f.chunks_exact(3)) {
            *o = [p[0], p[1], p[2], 1.0];
        }
    }
}

/// PVRTC blocks borrow colours from their neighbours, so the decoder works
/// on whole images whose block counts are powers of two.
fn decode_pvrtc(w: u32, h: u32, depth: u32, data: &[u8], two_bpp: bool) -> Result<Pixels, TextureError> {
    let bw = if two_bpp { 8 } else { 4 };
    let pw = w.div_ceil(bw).max(2).next_power_of_two() * bw;
    let ph = h.div_ceil(4).max(2).next_power_of_two() * 4;
    let slice = (pw / bw * ph / 4) as usize * 8;
    check_size(pw, ph, depth, 4)?;
    if data.len() < slice * depth as usize {
        return Err(invalid("PVRTC: truncated level"));
    }
    let mut out = Vec::with_capacity(w as usize * h as usize * 4 * depth as usize);
    let mut img = vec![0u32; pw as usize * ph as usize];
    for z in 0..depth as usize {
        let src = &data[z * slice..(z + 1) * slice];
        texture2ddecoder::decode_pvrtc(src, pw as usize, ph as usize, &mut img, two_bpp)
            .map_err(|e| invalid(format!("PVRTC: {e}")))?;
        for row in img.chunks_exact(pw as usize).take(h as usize) {
            for &v in &row[..w as usize] {
                let [b, g, r, a] = v.to_le_bytes();
                out.extend_from_slice(&[r, g, b, a]);
            }
        }
    }
    Ok(Pixels::Rgba8(out))
}

//endregion

/*┌─────────────────╮
| ASTC            |
└─────────────────╯*/
//region ASTC

/// The colour ASTC decoders return for a malformed block.
const ASTC_ERROR: [u8; 4] = [255, 0, 255, 255];

/// Whether a block uses an HDR endpoint mode or is an HDR solid colour.
fn astc_is_hdr(log: &basisu::astc::unpack::LogAstcBlock) -> bool {
    let ldr = |c: u8| matches!(c, 0 | 1 | 4 | 5 | 6 | 8 | 9 | 10 | 12 | 13);
    let n = (log.num_partitions as usize).clamp(1, 4);
    log.solid_color_flag_hdr || (!log.solid_color_flag_ldr && !log.color_endpoint_modes[..n].iter().all(|&c| ldr(c)))
}

/// Decodes ASTC.  A file says whether it holds HDR only through Vulkan's
/// SFLOAT formats; raw `.astc` files and UNORM formats can hold HDR blocks
/// too, so an LDR decode that meets one starts over in HDR.  3D footprints
/// go to `astc3d`, since basisu reads only 2D blocks.
pub(crate) fn decode_astc(
    fmt: &PixelFormat,
    w: u32,
    h: u32,
    depth: u32,
    data: &[u8],
    row: usize,
) -> Result<Pixels, TextureError> {
    let Codec::Astc { w: bw, h: bh, d: bd, hdr } = fmt.codec else { unreachable!() };
    if bd > 1 {
        let fp = crate::astc3d::Footprint { w: bw as u32, h: bh as u32, d: bd as u32 };
        return crate::astc3d::decode_surface(fp, fmt.srgb, hdr, w, h, depth, data);
    }
    let (bw, bh) = (bw as u32, bh as u32);
    let bx = w.div_ceil(bw) as usize;
    let slice = row * h.div_ceil(bh) as usize;
    let texels = w as usize * h as usize;
    if !hdr {
        check_size(w, h, depth, 4)?;
        let mut out = vec![0u8; texels * 4 * depth as usize];
        let found_hdr = std::sync::atomic::AtomicBool::new(false);
        for (z, dst) in out.chunks_exact_mut(texels * 4).enumerate() {
            let src = &data[z * slice..(z + 1) * slice];
            each_block_row(dst, w, bh, texels, |by_i, rows| {
                let mut tmp = [[0u8; 4]; MAX_TEXELS];
                for bx_i in 0..bx {
                    if found_hdr.load(std::sync::atomic::Ordering::Relaxed) {
                        return;
                    }
                    let at = by_i * row + bx_i * 16;
                    let block: &[u8; 16] = src[at..at + 16].try_into().unwrap();
                    match basisu::astc::unpack::unpack_block(block, bw, bh) {
                        Some(log) if astc_is_hdr(&log) => {
                            found_hdr.store(true, std::sync::atomic::Ordering::Relaxed);
                            return;
                        }
                        Some(log) => {
                            if basisu::astc::decode::decode_block_ldr8(&log, bw, bh, fmt.srgb, &mut tmp).is_none() {
                                tmp.fill(ASTC_ERROR);
                            }
                        }
                        None => tmp.fill(ASTC_ERROR),
                    }
                    copy_block(&tmp, bw, bh, bx_i, w, rows);
                }
            });
        }
        if !found_hdr.into_inner() {
            return Ok(Pixels::Rgba8(out));
        }
    }
    check_size(w, h, depth, 16)?;
    let mut out = vec![0f32; texels * 4 * depth as usize];
    for (z, dst) in out.chunks_exact_mut(texels * 4).enumerate() {
        let src = &data[z * slice..(z + 1) * slice];
        each_block_row(dst, w, bh, texels, |by_i, rows| {
            let mut halves = [[0u16; 4]; MAX_TEXELS];
            let mut tmp = [[0f32; 4]; MAX_TEXELS];
            for bx_i in 0..bx {
                let at = by_i * row + bx_i * 16;
                let block: &[u8; 16] = src[at..at + 16].try_into().unwrap();
                let ok = basisu::astc::unpack::unpack_block(block, bw, bh)
                    .and_then(|log| basisu::astc::decode::decode_block_hdr16(&log, bw, bh, &mut halves));
                if ok.is_some() {
                    for (t, hv) in tmp.iter_mut().zip(&halves).take((bw * bh) as usize) {
                        *t = [half_to_f32(hv[0]), half_to_f32(hv[1]), half_to_f32(hv[2]), half_to_f32(hv[3])];
                    }
                } else {
                    tmp.fill([1.0, 0.0, 1.0, 1.0]);
                }
                copy_block(&tmp, bw, bh, bx_i, w, rows);
            }
        });
    }
    Ok(Pixels::Rgba32F(out))
}

/// Decodes UASTC LDR 4x4 blocks, Basis Universal's high-quality codec,
/// which is built on ASTC but packs its blocks differently.
pub(crate) fn decode_uastc(w: u32, h: u32, data: &[u8], srgb: bool) -> Result<Pixels, TextureError> {
    let bx = w.div_ceil(4) as usize;
    let texels = check_size(w, h, 1, 4)?;
    if data.len() < bx * h.div_ceil(4) as usize * 16 {
        return Err(invalid("UASTC: truncated level"));
    }
    let mut out = vec![0u8; texels * 4];
    each_block_row(&mut out, w, 4, texels, |by_i, rows| {
        let mut tmp = [[0u8; 4]; MAX_TEXELS];
        for bx_i in 0..bx {
            let at = (by_i * bx + bx_i) * 16;
            let block: &[u8; 16] = data[at..at + 16].try_into().unwrap();
            match basisu::uastc::unpack::unpack_uastc(block, srgb) {
                Some(px) => {
                    for (t, p) in tmp.iter_mut().zip(px.iter()) {
                        *t = p.c;
                    }
                }
                None => tmp[..16].fill(ASTC_ERROR),
            }
            copy_block(&tmp, 4, 4, bx_i, w, rows);
        }
    });
    Ok(Pixels::Rgba8(out))
}

//endregion

/*┌─────────────────╮
| Uncompressed    |
└─────────────────╯*/
//region Uncompressed

/// Decodes rows of plain texels to 8-bit RGBA, with fast paths for the
/// common byte layouts.
fn decode_plain_u8(p: &Plain, src: &[u8], w: u32, row: usize, dst: &mut [u8]) {
    let tb = p.texel_bytes();
    let w = w as usize;
    let rows = dst.par_chunks_mut(w * 4).enumerate();
    match *p {
        Plain::Array { n: 4, bytes: 1, num: Num::Unorm, dest: [Dest::R, Dest::G, Dest::B, Dest::A] } => {
            rows.for_each(|(y, d)| d.copy_from_slice(&src[y * row..y * row + w * 4]));
        }
        Plain::Array { n: 4, bytes: 1, num: Num::Unorm, dest: [Dest::B, Dest::G, Dest::R, Dest::A] } => {
            rows.for_each(|(y, d)| {
                for (o, s) in d.chunks_exact_mut(4).zip(src[y * row..].chunks_exact(4)) {
                    o.copy_from_slice(&[s[2], s[1], s[0], s[3]]);
                }
            });
        }
        _ => rows.for_each(|(y, d)| {
            for (x, o) in d.chunks_exact_mut(4).enumerate() {
                let at = y * row + x * tb;
                o.copy_from_slice(&plain_u8(p, &src[at..at + tb]));
            }
        }),
    }
}

/// Reads component `i` of `bytes` bytes as a value in 0..1 (or -1..1 for
/// signed types), or as a float.
fn component(t: &[u8], i: usize, bytes: u8, num: Num) -> f32 {
    let s = &t[i * bytes as usize..(i + 1) * bytes as usize];
    match (bytes, num) {
        (1, Num::Unorm | Num::Uint) => s[0] as f32 / 255.0,
        (1, _) => (s[0] as i8 as f32 / 127.0).max(-1.0),
        (2, Num::Float) => half_to_f32(u16::from_le_bytes([s[0], s[1]])),
        (2, Num::Fixed5) => i16::from_le_bytes([s[0], s[1]]) as f32 / 32.0,
        (2, Num::Unorm | Num::Uint) => u16::from_le_bytes([s[0], s[1]]) as f32 / 65535.0,
        (2, _) => (i16::from_le_bytes([s[0], s[1]]) as f32 / 32767.0).max(-1.0),
        (4, Num::Float) => f32::from_le_bytes([s[0], s[1], s[2], s[3]]),
        (4, Num::Unorm | Num::Uint) => u32::from_le_bytes([s[0], s[1], s[2], s[3]]) as f32 / u32::MAX as f32,
        (4, _) => (i32::from_le_bytes([s[0], s[1], s[2], s[3]]) as f32 / i32::MAX as f32).max(-1.0),
        (8, Num::Float) => f64::from_le_bytes(s.try_into().unwrap()) as f32,
        (8, Num::Unorm | Num::Uint) => u64::from_le_bytes(s.try_into().unwrap()) as f32 / u64::MAX as f32,
        _ => (i64::from_le_bytes(s.try_into().unwrap()) as f32 / i64::MAX as f32).max(-1.0),
    }
}

/// One plain texel as linear floats, for formats whose values exceed 0..1.
fn plain_f32(p: &Plain, t: &[u8]) -> [f32; 4] {
    match *p {
        Plain::Array { n, bytes, num, dest } => {
            let mut o = [0.0, 0.0, 0.0, 1.0];
            for (i, d) in dest.iter().enumerate().take(n as usize) {
                let v = component(t, i, bytes, num);
                match d {
                    Dest::R => o[0] = v,
                    Dest::G => o[1] = v,
                    Dest::B => o[2] = v,
                    Dest::A => o[3] = v,
                    Dest::L => o[..3].fill(v),
                    Dest::X => {}
                }
            }
            o
        }
        Plain::B10G11R11 => {
            let v = u32::from_le_bytes([t[0], t[1], t[2], t[3]]);
            [small_ufloat(v & 0x7ff, 6), small_ufloat((v >> 11) & 0x7ff, 6), small_ufloat(v >> 22, 5), 1.0]
        }
        Plain::E5B9G9R9 => {
            let c = rgb9e5(u32::from_le_bytes([t[0], t[1], t[2], t[3]]));
            [c[0], c[1], c[2], 1.0]
        }
        Plain::XrBias => {
            let v = u32::from_le_bytes([t[0], t[1], t[2], t[3]]);
            let c = |s: u32| (((v >> s) & 1023) as f32 - 384.0) / 510.0;
            [c(0), c(10), c(20), (v >> 30) as f32 / 3.0]
        }
        Plain::Depth { shift, bits, .. } => {
            let v = u32::from_le_bytes([t[0], t[1], t[2], t[3]]);
            let d = if bits == 32 {
                f32::from_bits(v)
            } else {
                ((v >> shift) & ((1u32 << bits) - 1)) as f32 / ((1u32 << bits) - 1) as f32
            };
            [d, d, d, 1.0]
        }
        _ => {
            let b = plain_u8(p, t);
            [b[0] as f32 / 255.0, b[1] as f32 / 255.0, b[2] as f32 / 255.0, b[3] as f32 / 255.0]
        }
    }
}

/// One plain texel as 8-bit RGBA.  Signed values map -1..1 to 0..255, so a
/// signed normal map looks like an unsigned one.
fn plain_u8(p: &Plain, t: &[u8]) -> [u8; 4] {
    let to8 = |v: f32, num: Num| {
        let v = if matches!(num, Num::Snorm | Num::Sint) { v * 0.5 + 0.5 } else { v };
        (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8
    };
    match *p {
        Plain::Array { n, bytes, num, dest } => {
            let mut o = [0, 0, 0, 255];
            for (i, d) in dest.iter().enumerate().take(n as usize) {
                let v = if bytes == 1 && matches!(num, Num::Unorm | Num::Uint) {
                    t[i]
                } else {
                    to8(component(t, i, bytes, num), num)
                };
                match d {
                    Dest::R => o[0] = v,
                    Dest::G => o[1] = v,
                    Dest::B => o[2] = v,
                    Dest::A => o[3] = v,
                    Dest::L => o[..3].fill(v),
                    Dest::X => {}
                }
            }
            o
        }
        Plain::Packed { bytes, fields } => {
            let mut v = 0u32;
            for (i, b) in t.iter().take(bytes as usize).enumerate() {
                v |= (*b as u32) << (8 * i);
            }
            let mut o = [0, 0, 0, 255];
            for (c, &(shift, bits)) in fields.iter().enumerate() {
                if bits > 0 {
                    let max = (1u32 << bits) - 1;
                    let x = (v >> shift) & max;
                    o[c] = ((x * 255 + max / 2) / max) as u8;
                }
            }
            o
        }
        Plain::Depth { .. } => {
            let d = to8(plain_f32(p, t)[0], Num::Unorm);
            [d, d, d, 255]
        }
        Plain::B10G11R11 | Plain::E5B9G9R9 | Plain::XrBias => {
            let f = plain_f32(p, t);
            [to8(f[0], Num::Unorm), to8(f[1], Num::Unorm), to8(f[2], Num::Unorm), 255]
        }
    }
}

//endregion
