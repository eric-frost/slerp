//! Basis Universal payloads in KTX2 files with format VK_FORMAT_UNDEFINED,
//! through the pure-Rust `basisu` crate.  UASTC LDR blocks decode straight
//! from the level data in parallel.  ETC1S is transcoded to RGBA, and the
//! codecs with their own entropy coding (XUASTC, UASTC HDR 6x6) to the ASTC
//! blocks they are built on, which `blocks::decode_astc` then decodes in
//! parallel.  (Basis codecs whose blocks are plain ASTC, UASTC HDR 4x4 and
//! raw ASTC, carry an ASTC Vulkan format and never come here.)
//!
//! XUBC7, added in Basis Universal 2.50, is newer than the `basisu` crate,
//! so those files alone go through the upstream C++ transcoder
//! (`basisu_c_sys`), which turns them into BC7 blocks.  Only files whose
//! descriptor names XUBC7 reach it, which keeps other malformed files away
//! from C++ code.

use crate::assemble::{Layout, assemble};
use crate::blocks::{decode_astc, decode_uastc};
use crate::formats::{Codec, PixelFormat, channel_tag};
use crate::ktx2::{Header, level_data};
use crate::thumb::Channels;
use crate::util::{invalid, slice, unsupported};
use crate::{BlockFormat, ColorSpace, DecodeOptions, GpuBlocks, Pixels, Texture, TextureError, TextureKind};
use basisu::{AstcBlock, DecodeFlags, SourceFormat, TargetFormat, Transcoder};

/// The KTX2 data format descriptor's colour models for Basis codecs.
const MODEL_ETC1S: u8 = 163;
const MODEL_UASTC: u8 = 166;
const MODEL_XUBC7: u8 = 170;

fn err(e: basisu::Error) -> TextureError {
    match e {
        basisu::Error::Unsupported { .. } | basisu::Error::UnsupportedFeature(_) => {
            unsupported(format!("Basis: {e:?}"))
        }
        _ => invalid(format!("Basis: {e:?}")),
    }
}

/// What the encoder put in which channel, from the descriptor's sample
/// channel ids.  Two-channel Basis textures (normal maps) keep their second
/// channel in alpha (ETC1S RRR+GGG, UASTC RRRG) or in green (UASTC RG).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Layout2 {
    Rgb,
    Rgba,
    R,
    /// Red in RGB and green in alpha.
    RrrG,
    Rg,
}

fn channel_layout(h: &Header) -> Layout2 {
    let ch = h.dfd_channels();
    match (h.dfd_model(), ch.as_slice()) {
        (MODEL_ETC1S, [3]) => Layout2::R,
        (MODEL_ETC1S, [3, 4]) => Layout2::RrrG,
        (MODEL_ETC1S, [_, 15]) => Layout2::Rgba,
        (MODEL_UASTC, [3]) => Layout2::Rgba,
        (MODEL_UASTC, [4]) => Layout2::R,
        (MODEL_UASTC, [5]) => Layout2::RrrG,
        (MODEL_UASTC, [6]) => Layout2::Rg,
        _ => Layout2::Rgb,
    }
}

/// An LDR ASTC footprint as `(width, height, hdr)`.
fn ldr(b: AstcBlock) -> (u8, u8, bool) {
    let (w, h) = b.dims();
    (w as u8, h as u8, false)
}

fn block_name(b: AstcBlock) -> String {
    let (w, h) = b.dims();
    format!("{w}x{h}")
}

pub(crate) fn decode_ktx2(
    bytes: &[u8],
    h: &Header,
    mut layout: Layout,
    opts: &DecodeOptions,
    gpu: GpuBlocks,
) -> Result<Texture, TextureError> {
    let srgb = h.dfd_srgb();
    let chans = channel_layout(h);
    let suffix = match chans {
        Layout2::R => channel_tag(Channels::R),
        Layout2::Rg | Layout2::RrrG => channel_tag(Channels::Rg),
        _ => "",
    };
    layout.color_space = if srgb { ColorSpace::Srgb } else { ColorSpace::Linear };
    layout.alpha = matches!(chans, Layout2::Rgba);

    if h.dfd_model() == MODEL_XUBC7 {
        layout.source.format = format!("XUBC7{suffix}");
        let keep = gpu.bc && can_keep_blocks(&layout, opts);
        return decode_xubc7(bytes, layout, opts, keep);
    }

    // 0. For a GPU that samples BC7, BC5 or ASTC: blocks of those, which
    //    upload in a quarter of the memory and time of RGBA.  Anything
    //    that goes wrong falls back to RGBA below.
    if let Some(target) = block_target(h, chans, gpu)
        && can_keep_blocks(&layout, opts)
        && let Ok(tex) = decode_blocks(bytes, h, layout.clone(), opts, chans, target, suffix)
    {
        return Ok(tex);
    }

    // 1. UASTC LDR needs no transcoder: its blocks decode straight from the
    //    level data, in parallel, under any of the general supercompression
    //    schemes (the transcoder refuses zlib).
    if h.dfd_model() == MODEL_UASTC && matches!(h.scheme, 0 | 2 | 3) {
        layout.source.format = format!("UASTC LDR 4x4{suffix}");
        let (faces, all_layers) = (layout.faces, layout.layers);
        let (w0, h0) = (layout.width, layout.height);
        return assemble(layout, opts, |level, layers| {
            let (w, ht) = (crate::util::mip_dim(w0, level), crate::util::mip_dim(h0, level));
            let image = w.div_ceil(4) as u64 * ht.div_ceil(4) as u64 * 16;
            let data = level_data(bytes, h, level, image * all_layers as u64 * faces as u64)?;
            (0..(layers * faces) as u64)
                .map(|i| Ok(remap(decode_uastc(w, ht, slice(&data, i * image, image, "KTX2 level")?, srgb)?, chans)))
                .collect()
        });
    }

    // 2. Everything else goes through the transcoder: ETC1S straight to
    //    RGBA, and the codecs with their own entropy coding (XUASTC, UASTC
    //    HDR 6x6) to the ASTC blocks they are built on.
    let t = Transcoder::new(bytes).map_err(err)?;
    let (name, target, astc) = match t.source_format() {
        SourceFormat::Etc1s => ("ETC1S".to_string(), TargetFormat::Rgba32, None),
        SourceFormat::UastcLdr => ("UASTC LDR 4x4".into(), TargetFormat::Astc4x4Rgba, Some((4, 4, false))),
        SourceFormat::AstcLdr(b) => (format!("ASTC LDR {}", block_name(b)), b.passthrough_target(), Some(ldr(b))),
        SourceFormat::XuastcLdr(b) => (format!("XUASTC LDR {}", block_name(b)), b.passthrough_target(), Some(ldr(b))),
        SourceFormat::UastcHdr4x4 => ("UASTC HDR 4x4".into(), TargetFormat::AstcHdr4x4Rgba, Some((4, 4, true))),
        SourceFormat::AstcHdr6x6 => ("ASTC HDR 6x6".into(), TargetFormat::AstcHdr6x6Rgba, Some((6, 6, true))),
        SourceFormat::UastcHdr6x6 => ("UASTC HDR 6x6".into(), TargetFormat::AstcHdr6x6Rgba, Some((6, 6, true))),
        other => return Err(unsupported(format!("Basis codec {other:?}"))),
    };
    let hdr = astc.is_some_and(|a| a.2);
    let video = t.is_video();
    layout.source.format = format!("{name}{suffix}{}", if video { " video" } else { "" });
    if hdr {
        layout.color_space = ColorSpace::Linear;
    }
    layout.alpha &= t.has_alpha();
    if video {
        // Video frames are stored as layers that depend on each other; the
        // first frame stands for the file.
        layout.layers = 1;
    }
    let fmt = astc.map(|(w, h, hdr)| PixelFormat {
        name: "ASTC",
        codec: Codec::Astc { w, h, d: 1, hdr },
        srgb: srgb && !hdr,
        channels: Channels::Rgba,
    });
    let faces = layout.faces;
    assemble(layout, opts, |level, layers| {
        let info = t.image_level_info(level).map_err(err)?;
        let (w, ht) = (info.width, info.height);
        let mut out = Vec::new();
        for layer in 0..layers {
            for face in 0..faces {
                let data = if video {
                    t.transcode_video_frame(&mut basisu::VideoState::new(), level, 0, target, DecodeFlags::NONE)
                } else {
                    t.transcode_image(level, layer, face, target, DecodeFlags::NONE)
                }
                .map_err(err)?;
                let pixels = match &fmt {
                    Some(f) => decode_astc(f, w, ht, 1, &data, info.num_blocks_x as usize * 16)?,
                    None => {
                        if data.len() != w as usize * ht as usize * 4 {
                            return Err(invalid("Basis: short RGBA image"));
                        }
                        Pixels::Rgba8(data)
                    }
                };
                out.push(remap(pixels, chans));
            }
        }
        Ok(out)
    })
}

/*┌─────────────────╮
| GPU blocks      |
└─────────────────╯*/
//region GPU blocks

/// The block format to transcode to for a GPU that samples `gpu`, or
/// `None` to decode to RGBA.  Each codec goes where Basis Universal meant
/// it to: UASTC LDR to BC7, and ETC1S, whose quality is BC1's anyway, to
/// BC1 (BC3 with alpha), which also transcodes several times faster than
/// to BC7.  Two-channel normal maps go to BC5, since BC7 and BC1 would
/// leave their second channel in alpha.  Without BC, ASTC; the other codecs
/// decode to RGBA.  Single-channel textures stay in colour formats, which
/// keep the channel in red, green and blue as RGBA decoding does.
fn block_target(h: &Header, chans: Layout2, gpu: GpuBlocks) -> Option<BlockFormat> {
    let two = matches!(chans, Layout2::Rg | Layout2::RrrG);
    match h.dfd_model() {
        MODEL_ETC1S | MODEL_UASTC if two && gpu.bc => Some(BlockFormat::Bc5),
        MODEL_ETC1S | MODEL_UASTC if two => None,
        MODEL_UASTC if gpu.bc => Some(BlockFormat::Bc7),
        MODEL_ETC1S if gpu.bc && chans == Layout2::Rgba => Some(BlockFormat::Bc3),
        MODEL_ETC1S if gpu.bc => Some(BlockFormat::Bc1),
        MODEL_ETC1S | MODEL_UASTC if gpu.astc => Some(BlockFormat::Astc4x4),
        _ => None,
    }
}

/// Whether a texture can stay in blocks: a plain 2D texture asked for
/// whole, with every mip level stored (blocks can't have mips made for them
/// on the GPU), in whole blocks (as GPUs want the largest level), stored
/// top row first (block data can't be mirrored).
fn can_keep_blocks(layout: &Layout, opts: &DecodeOptions) -> bool {
    let full_chain = 32 - layout.width.max(layout.height).max(1).leading_zeros();
    layout.kind == TextureKind::D2
        && opts.all_mips
        && opts.min_size.is_none()
        && opts.max_size.is_none()
        && layout.levels == full_chain
        && layout.width.is_multiple_of(4)
        && layout.height.is_multiple_of(4)
        && !layout.flip_x
        && !layout.flip_y
}

/// Transcodes every level to `target` blocks.  UASTC blocks transcode one
/// by one, in parallel; ETC1S goes through the transcoder level by level.
fn decode_blocks(
    bytes: &[u8],
    h: &Header,
    mut layout: Layout,
    opts: &DecodeOptions,
    chans: Layout2,
    target: BlockFormat,
    suffix: &str,
) -> Result<Texture, TextureError> {
    use rayon::prelude::*;
    let faces = layout.faces;
    let all_layers = layout.layers;
    let (w0, h0) = (layout.width, layout.height);
    if h.dfd_model() == MODEL_UASTC && matches!(h.scheme, 0 | 2 | 3) {
        layout.source.format = format!("UASTC LDR 4x4{suffix}");
        // Channel 3 holds RRRG's green.
        let green = if chans == Layout2::RrrG { 3 } else { 1 };
        return assemble(layout, opts, |level, layers| {
            let (w, ht) = (crate::util::mip_dim(w0, level), crate::util::mip_dim(h0, level));
            let image = w.div_ceil(4) as u64 * ht.div_ceil(4) as u64 * 16;
            let data = level_data(bytes, h, level, image * all_layers as u64 * faces as u64)?;
            (0..(layers * faces) as u64)
                .map(|i| {
                    let src = slice(&data, i * image, image, "KTX2 level")?;
                    let mut out = vec![0u8; src.len()];
                    let ok = out.par_chunks_mut(16 * 256).zip(src.par_chunks(16 * 256)).all(|(dst, src)| {
                        dst.chunks_exact_mut(16).zip(src.chunks_exact(16)).all(|(d, s)| {
                            let Ok(s) = <&[u8; 16]>::try_from(s) else { return false };
                            let block = match target {
                                BlockFormat::Bc7 => basisu::uastc::bc7::transcode_uastc_to_bc7(s),
                                BlockFormat::Bc5 => basisu::uastc::bc3_bc5::transcode_uastc_to_bc5(s, false, 0, green),
                                BlockFormat::Astc4x4 => basisu::uastc::astc_pack::transcode_uastc_to_astc(s),
                                // `block_target` sends UASTC to the others.
                                BlockFormat::Bc1 | BlockFormat::Bc3 => None,
                            };
                            block.map(|b| d.copy_from_slice(&b)).is_some()
                        })
                    });
                    if !ok {
                        return Err(invalid("UASTC: a block didn't transcode"));
                    }
                    Ok(Pixels::Blocks(target, out))
                })
                .collect()
        });
    }
    let t = Transcoder::new(bytes).map_err(err)?;
    if t.source_format() != SourceFormat::Etc1s || t.is_video() {
        return Err(unsupported("Basis: no blocks for this codec"));
    }
    layout.source.format = format!("ETC1S{suffix}");
    layout.alpha &= t.has_alpha();
    let format = match target {
        BlockFormat::Bc1 => TargetFormat::Bc1Rgb,
        BlockFormat::Bc3 => TargetFormat::Bc3Rgba,
        BlockFormat::Bc5 => TargetFormat::Bc5Rg,
        BlockFormat::Bc7 => TargetFormat::Bc7Rgba,
        BlockFormat::Astc4x4 => TargetFormat::Astc4x4Rgba,
    };
    // Levels are independent, so the larger ones transcode side by side.
    let levels: Vec<Result<Vec<Pixels>, TextureError>> = (0..layout.levels)
        .into_par_iter()
        .map(|level| {
            let layers = if opts.first_layer_only { 1 } else { all_layers };
            let mut out = Vec::new();
            for layer in 0..layers {
                for face in 0..faces {
                    let data = t.transcode_image(level, layer, face, format, DecodeFlags::NONE).map_err(err)?;
                    out.push(Pixels::Blocks(target, data));
                }
            }
            Ok(out)
        })
        .collect();
    let mut levels = levels.into_iter();
    assemble(layout, opts, |_, _| levels.next().unwrap_or_else(|| Err(invalid("Basis: missing level"))))
}

//endregion

/*┌─────────────────╮
| XUBC7           |
└─────────────────╯*/
//region XUBC7

/// An open KTX2 file in the C++ transcoder, closed when dropped.
struct CTranscoder(u64);

impl Drop for CTranscoder {
    fn drop(&mut self) {
        // SAFETY: the handle came from `bt_ktx2_open` and is closed once.
        unsafe { basisu_c_sys::transcoder::bt_ktx2_close(self.0) }
    }
}

/// Decodes XUBC7, whose codec is built on BC7: with `keep`, the BC7 blocks
/// themselves are the result.
fn decode_xubc7(bytes: &[u8], layout: Layout, opts: &DecodeOptions, keep: bool) -> Result<Texture, TextureError> {
    use basisu_c_sys::common::TF_BC7_RGBA;
    use basisu_c_sys::transcoder as bt;
    static INIT: std::sync::Once = std::sync::Once::new();
    // SAFETY: initialises the transcoder's global tables, once.
    INIT.call_once(|| unsafe { bt::bt_init() });
    let len = u32::try_from(bytes.len()).map_err(|_| crate::util::too_large("XUBC7 file over 4 GiB"))?;
    // SAFETY: the transcoder reads `len` bytes from the pointer while the
    // handle is open; `bytes` outlives the handle, which is dropped below.
    let handle = CTranscoder(unsafe { bt::bt_ktx2_open(bytes.as_ptr() as u64, len) });
    if handle.0 == 0 || unsafe { bt::bt_ktx2_start_transcoding(handle.0) }.is_err() {
        return Err(invalid("XUBC7: the transcoder refused the file"));
    }
    let bc7 = PixelFormat {
        name: "BC7",
        codec: Codec::Bc7,
        srgb: layout.color_space == ColorSpace::Srgb,
        channels: Channels::Rgba,
    };
    let faces = layout.faces;
    assemble(layout, opts, |level, layers| {
        let mut out = Vec::new();
        for layer in 0..layers {
            for face in 0..faces {
                // SAFETY: plain queries on an open handle; out-of-range
                // indices return zero.
                let (w, h, blocks) = unsafe {
                    (
                        bt::bt_ktx2_get_level_orig_width(handle.0, level, layer, face),
                        bt::bt_ktx2_get_level_orig_height(handle.0, level, layer, face),
                        bt::bt_ktx2_get_level_total_blocks(handle.0, level, layer, face),
                    )
                };
                if w == 0 || h == 0 || blocks as u64 != w.div_ceil(4) as u64 * h.div_ceil(4) as u64 {
                    return Err(invalid("XUBC7: bad level size"));
                }
                crate::util::check_size(w, h, 1, 4)?;
                let mut data = vec![0u8; blocks as usize * 16];
                // SAFETY: `data` holds exactly `blocks` 16-byte BC7 blocks,
                // which is the size passed; the call writes no more.
                let ok = unsafe {
                    bt::bt_ktx2_transcode_image_level(
                        handle.0,
                        level,
                        layer,
                        face,
                        data.as_mut_ptr() as u64,
                        blocks,
                        TF_BC7_RGBA,
                        0,
                        0,
                        0,
                        -1,
                        -1,
                        0,
                    )
                };
                if ok.is_err() {
                    return Err(invalid("XUBC7: transcoding failed"));
                }
                if keep {
                    out.push(Pixels::Blocks(BlockFormat::Bc7, data));
                } else {
                    out.push(crate::blocks::decode_surface(&bc7, w, h, 1, &data, None)?);
                }
            }
        }
        Ok(out)
    })
}

//endregion

/// Moves a two-channel texture's green out of alpha, so every decoder hands
/// the renderer and thumbnails the same RG layout.
fn remap(p: Pixels, chans: Layout2) -> Pixels {
    match (p, chans) {
        (Pixels::Rgba8(mut v), Layout2::RrrG | Layout2::Rg) => {
            for t in v.chunks_exact_mut(4) {
                let g = if chans == Layout2::RrrG { t[3] } else { t[1] };
                t.copy_from_slice(&[t[0], g, 0, 255]);
            }
            Pixels::Rgba8(v)
        }
        (Pixels::Rgba8(mut v), Layout2::Rgb | Layout2::R) => {
            v.chunks_exact_mut(4).for_each(|t| t[3] = 255);
            Pixels::Rgba8(v)
        }
        (p, _) => p,
    }
}
