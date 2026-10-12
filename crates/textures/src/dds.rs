//! DirectDraw Surface files: the legacy header with FourCC codes or channel
//! masks, and the DX10 extension with DXGI formats, array sizes and cubemap
//! flags.  Readers in the wild disagree about many header fields, so this
//! one trusts the format and sizes and forgives the rest (wrong mip counts,
//! zero structure sizes, missing flags).

use crate::assemble::{Layout, assemble};
use crate::blocks::decode_surface;
use crate::formats::{ASTC_BLOCKS, Codec, Dest, Num, PixelFormat, Plain, Yuv, arr, channel_tag, fmt, packed};
use crate::thumb::Channels;
use crate::util::{Reader, invalid, mip_dim, unsupported};
use crate::{ColorSpace, DecodeOptions, Pixels, SourceInfo, Texture, TextureError, TextureKind};

/// How decoded texels need rearranging for formats that store channels
/// somewhere unusual.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Fixup {
    None,
    /// DXT5 normal maps (nvtt's DXT5nm, "BC3n"): X in alpha, Y in green.
    XInAlpha,
    /// "RXGB": DXT5 with red moved to alpha.
    Rxgb,
}

pub(crate) fn decode(bytes: &[u8], opts: &DecodeOptions) -> Result<Texture, TextureError> {
    // 1. The legacy header.
    let mut r = Reader::at(bytes, 4);
    let _size = r.u32()?;
    let _flags = r.u32()?;
    let height = r.u32()?;
    let width = r.u32()?;
    let pitch = r.u32()?;
    let depth = r.u32()?;
    let mips = r.u32()?;
    r.skip(44)?;
    let _pf_size = r.u32()?;
    let pf_flags = r.u32()?;
    let fourcc = r.bytes(4)?;
    let bit_count = r.u32()?;
    let masks = [r.u32()?, r.u32()?, r.u32()?, r.u32()?];
    let _caps = r.u32()?;
    let caps2 = r.u32()?;
    r.skip(12)?;

    // 2. The DX10 extension, or a format from the legacy pixel format.
    let mut kind = TextureKind::D2;
    let mut faces = 1;
    let mut layers = 1;
    let mut fixup = Fixup::None;
    let format;
    if fourcc == b"DX10" {
        let dxgi = r.u32()?;
        let dimension = r.u32()?;
        let misc = r.u32()?;
        layers = r.u32()?.max(1);
        let _misc2 = r.u32()?;
        format = dxgi_format(dxgi).ok_or_else(|| unsupported(format!("DDS DXGI format {dxgi}")))?;
        if misc & 4 != 0 {
            kind = TextureKind::Cube;
            faces = 6;
        } else if dimension == 4 {
            kind = TextureKind::D3;
        }
    } else {
        let (f, fx) = legacy_format(pf_flags, fourcc, bit_count, masks).ok_or_else(|| {
            unsupported(format!("DDS pixel format {:?} / {bit_count} bits", String::from_utf8_lossy(fourcc)))
        })?;
        format = f;
        fixup = fx;
        if caps2 & 0x200 != 0 {
            // Legacy cubemaps may leave faces out; only complete ones are cubes.
            let present = (caps2 >> 10 & 63).count_ones().max(1);
            if present == 6 {
                kind = TextureKind::Cube;
                faces = 6;
            } else {
                layers = present;
            }
        } else if caps2 & 0x20_0000 != 0 && depth > 1 {
            kind = TextureKind::D3;
        }
    }
    let data_start = r.pos;
    let depth = if kind == TextureKind::D3 { depth.max(1) } else { 1 };
    crate::util::check_layout(width, height, depth, layers)?;

    // 3. Sizes.  Mip counts are often wrong, so use the largest count, up to
    //    the claimed one, whose data fits in the file.
    let plain = matches!(format.codec, Codec::Plain(_));
    let tight_row = |w: u32| format.codec.grid(w, 1).0 as u64 * format.codec.block_bytes() as u64;
    // Old writers padded uncompressed rows to four bytes and said so in the
    // pitch field; most don't.
    let padded = plain && pitch as u64 == tight_row(width).div_ceil(4) * 4 && pitch as u64 != tight_row(width);
    let row_bytes = move |w: u32| if padded { tight_row(w).div_ceil(4) * 4 } else { tight_row(w) };
    let level_bytes = |l: u32| {
        let (w, h) = (mip_dim(width, l), mip_dim(height, l));
        let d = if kind == TextureKind::D3 { mip_dim(depth, l) } else { 1 };
        row_bytes(w) * format.codec.grid(w, h).1 as u64 * d.div_ceil(format.codec.block_depth()) as u64
    };
    let full_chain = 32 - width.max(height).max(depth).leading_zeros();
    let available = (bytes.len() - data_start) as u64;
    let surfaces = layers as u64 * faces as u64;
    let mut levels = mips.clamp(1, full_chain);
    let layer_bytes = |n: u32| (0..n).map(level_bytes).sum::<u64>();
    while levels > 1 && layer_bytes(levels).saturating_mul(surfaces) > available {
        levels -= 1;
    }
    if layer_bytes(1) > available {
        return Err(invalid("DDS: truncated image data"));
    }
    // A file too short for its array keeps the layers that fit.
    let layer_size = layer_bytes(levels) * faces as u64;
    let layers = layers.min((available / layer_size.max(1)).max(1) as u32);

    let name = format!("{}{}", format.name, channel_tag(format.channels));
    let layout = Layout {
        kind,
        width,
        height,
        depth,
        layers,
        faces,
        levels,
        color_space: if format.srgb { ColorSpace::Srgb } else { ColorSpace::Linear },
        flip_y: false,
        flip_x: false,
        alpha: format.codec.has_alpha(false) && !matches!(fixup, Fixup::XInAlpha | Fixup::Rxgb),
        source: SourceInfo {
            container: "DDS".into(),
            format: name,
            file_bytes: bytes.len() as u64,
            ..Default::default()
        },
    };

    // 4. Surfaces are stored layer by layer (faces count as layers), each
    //    with its whole mip chain.
    let data = &bytes[data_start..];
    assemble(layout, opts, |level, want| {
        let (w, h) = (mip_dim(width, level), mip_dim(height, level));
        let d = if kind == TextureKind::D3 { mip_dim(depth, level) } else { 1 };
        let skip = layer_bytes(level);
        let size = level_bytes(level);
        let mut out = Vec::new();
        for i in 0..(want * faces) as u64 {
            let s = crate::util::slice(data, i * layer_bytes(levels) + skip, size, "DDS surface")?;
            let p = decode_surface(&format, w, h, d, s, Some(row_bytes(w) as usize))?;
            out.push(apply(p, fixup));
        }
        Ok(out)
    })
}

fn apply(p: Pixels, fixup: Fixup) -> Pixels {
    match (fixup, p) {
        (Fixup::XInAlpha, Pixels::Rgba8(mut v)) => {
            v.chunks_exact_mut(4).for_each(|t| t.copy_from_slice(&[t[3], t[1], 0, 255]));
            Pixels::Rgba8(v)
        }
        (Fixup::Rxgb, Pixels::Rgba8(mut v)) => {
            v.chunks_exact_mut(4).for_each(|t| t.copy_from_slice(&[t[3], t[1], t[2], 255]));
            Pixels::Rgba8(v)
        }
        (_, p) => p,
    }
}

/*┌─────────────────╮
| Legacy formats  |
└─────────────────╯*/
//region Legacy formats

use Dest::{A, B, G, L, R, X};

/// The format a legacy (pre-DX10) pixel format describes: a FourCC code,
/// a Direct3D 9 format number in the FourCC field, or channel masks.
fn legacy_format(flags: u32, fourcc: &[u8], bits: u32, masks: [u32; 4]) -> Option<(PixelFormat, Fixup)> {
    let ch = Channels::Rgba;
    let plain = |f: PixelFormat| Some((f, Fixup::None));
    if flags & 4 != 0 || fourcc != [0; 4] && flags & 0x40 == 0 {
        let normal = flags & 0x8000_0000 != 0;
        return match fourcc {
            b"DXT1" => plain(fmt("DXT1", Codec::Bc1 { alpha: true }, false, ch)),
            b"DXT2" => plain(fmt("DXT2", Codec::Bc2, false, ch)),
            b"DXT3" => plain(fmt("DXT3", Codec::Bc2, false, ch)),
            b"DXT4" => plain(fmt("DXT4", Codec::Bc3, false, ch)),
            b"DXT5" if normal => Some((fmt("DXT5nm", Codec::Bc3, false, Channels::Rg), Fixup::XInAlpha)),
            b"DXT5" => plain(fmt("DXT5", Codec::Bc3, false, ch)),
            b"RXGB" => Some((fmt("RXGB", Codec::Bc3, false, ch), Fixup::Rxgb)),
            b"ATI1" | b"BC4U" => plain(fmt("BC4U", Codec::Bc4 { signed: false }, false, Channels::R)),
            b"BC4S" => plain(fmt("BC4S", Codec::Bc4 { signed: true }, false, Channels::R)),
            b"ATI2" | b"BC5U" => plain(fmt("BC5U", Codec::Bc5 { signed: false }, false, Channels::Rg)),
            b"BC5S" => plain(fmt("BC5S", Codec::Bc5 { signed: true }, false, Channels::Rg)),
            b"BC6H" => plain(fmt("BC6H", Codec::Bc6h { signed: false }, false, ch)),
            b"BC7\0" | b"BC7L" => plain(fmt("BC7", Codec::Bc7, false, ch)),
            b"ETC1" => plain(fmt("ETC1", Codec::Etc1, false, ch)),
            b"ETC2" => plain(fmt("ETC2", Codec::Etc2Rgb, false, ch)),
            b"ETCA" => plain(fmt("ETC2A", Codec::Etc2Rgba, false, ch)),
            b"ATC " => plain(fmt("ATC", Codec::AtcRgb, false, ch)),
            b"ATCI" => plain(fmt("ATCI", Codec::AtcRgba, false, ch)),
            b"YUY2" => plain(fmt("YUY2", Codec::Yuv(Yuv::Yuy2), true, ch)),
            b"UYVY" => plain(fmt("UYVY", Codec::Yuv(Yuv::Uyvy), true, ch)),
            b"RGBG" => plain(fmt("R8G8_B8G8", Codec::Pairs422 { bytes: 1, g_first: false }, false, ch)),
            b"GRGB" => plain(fmt("G8R8_G8B8", Codec::Pairs422 { bytes: 1, g_first: true }, false, ch)),
            _ => d3d9_format(u32::from_le_bytes(fourcc.try_into().ok()?)).map(|f| (f, Fixup::None)),
        };
    }
    // Signed (bump map) masks that line up with whole bytes, such as
    // R8G8_SNORM or R16G16_SNORM written the Direct3D 9 way.
    if flags & 0x8_0000 != 0 {
        let n = masks.iter().take_while(|&&m| m != 0).count();
        let size = masks[0].count_ones();
        let aligned = (size == 8 || size == 16)
            && n * size as usize == bits as usize
            && (0..n).all(|i| masks[i] == (((1u64 << size) - 1) as u32) << (i as u32 * size));
        if aligned {
            let dest = [[R, X, X, X], [R, G, X, X], [R, G, B, X], [R, G, B, A]][n - 1];
            let channels = [Channels::R, Channels::Rg, Channels::Rgba, Channels::Rgba][n - 1];
            let name = match (n, size) {
                (2, 8) => "V8U8",
                (4, 8) => "Q8W8V8U8",
                (2, 16) => "V16U16",
                (4, 16) => "Q16W16V16U16",
                _ => "signed",
            };
            return plain(fmt(name, arr(n as u8, (size / 8) as u8, Num::Snorm, dest), false, channels));
        }
    }
    // Channel masks.  Luminance keeps its mask in the red slot.
    let bytes = match bits {
        8 | 16 | 24 | 32 => (bits / 8) as u8,
        _ => return None,
    };
    let field = |m: u32| -> (u8, u8) {
        if m == 0 {
            (0, 0)
        } else {
            (m.trailing_zeros() as u8, (m >> m.trailing_zeros()).trailing_ones().min(16) as u8)
        }
    };
    let alpha = if flags & 3 != 0 { field(masks[3]) } else { (0, 0) };
    let luminance = flags & 0x2_0000 != 0;
    let alpha_only = flags & 0x2 != 0 && flags & 0x40 == 0 && !luminance;
    let fields = if alpha_only {
        [(0, 0), (0, 0), (0, 0), field(if masks[3] != 0 { masks[3] } else { (1u64 << bits).wrapping_sub(1) as u32 })]
    } else if luminance {
        let l = field(masks[0]);
        [l, l, l, alpha]
    } else {
        [field(masks[0]), field(masks[1]), field(masks[2]), alpha]
    };
    let channels = if alpha_only {
        Channels::A
    } else if !luminance && masks[1] != 0 && masks[2] == 0 {
        Channels::Rg
    } else if !luminance && masks[1] == 0 && masks[2] == 0 {
        Channels::R
    } else {
        Channels::Rgba
    };
    let name = match (luminance, alpha_only, bits) {
        (true, _, _) => "luminance",
        (_, true, _) => "alpha",
        (_, _, 32) => "32-bit RGB",
        (_, _, 24) => "24-bit RGB",
        (_, _, 16) => "16-bit RGB",
        _ => "8-bit RGB",
    };
    plain(fmt(name, packed(bytes, fields), false, channels))
}

/// Direct3D 9 `D3DFORMAT` numbers that DDS writers put in the FourCC field.
fn d3d9_format(v: u32) -> Option<PixelFormat> {
    let ch = Channels::Rgba;
    let f = |name, codec, channels| Some(fmt(name, codec, false, channels));
    match v {
        36 => f("A16B16G16R16", arr(4, 2, Num::Unorm, [R, G, B, A]), ch),
        110 => f("Q16W16V16U16", arr(4, 2, Num::Snorm, [R, G, B, A]), ch),
        111 => f("R16F", arr(1, 2, Num::Float, [R, X, X, X]), Channels::R),
        112 => f("G16R16F", arr(2, 2, Num::Float, [R, G, X, X]), Channels::Rg),
        113 => f("A16B16G16R16F", arr(4, 2, Num::Float, [R, G, B, A]), ch),
        114 => f("R32F", arr(1, 4, Num::Float, [R, X, X, X]), Channels::R),
        115 => f("G32R32F", arr(2, 4, Num::Float, [R, G, X, X]), Channels::Rg),
        116 => f("A32B32G32R32F", arr(4, 4, Num::Float, [R, G, B, A]), ch),
        117 => f("CxV8U8", arr(2, 1, Num::Snorm, [R, G, X, X]), Channels::Rg),
        60 => f("V8U8", arr(2, 1, Num::Snorm, [R, G, X, X]), Channels::Rg),
        63 => f("Q8W8V8U8", arr(4, 1, Num::Snorm, [R, G, B, A]), ch),
        64 => f("V16U16", arr(2, 2, Num::Snorm, [R, G, X, X]), Channels::Rg),
        81 => f("L16", arr(1, 2, Num::Unorm, [L, X, X, X]), ch),
        _ => None,
    }
}

//endregion

/*┌─────────────────╮
| DXGI formats    |
└─────────────────╯*/
//region DXGI formats

/// Looks up a DXGI format.  Typeless formats read as UNORM, the most
/// common way they are viewed.
fn dxgi_format(v: u32) -> Option<PixelFormat> {
    let ch = Channels::Rgba;
    let f = |name, codec, srgb, channels| Some(fmt(name, codec, srgb, channels));
    let (r1, rg, rgb, rgba) = ([R, X, X, X], [R, G, X, X], [R, G, B, X], [R, G, B, A]);
    let c1 = Channels::R;
    let c2 = Channels::Rg;
    match v {
        1 | 2 => f("R32G32B32A32_FLOAT", arr(4, 4, Num::Float, rgba), false, ch),
        3 => f("R32G32B32A32_UINT", arr(4, 4, Num::Uint, rgba), false, ch),
        4 => f("R32G32B32A32_SINT", arr(4, 4, Num::Sint, rgba), false, ch),
        5 | 6 => f("R32G32B32_FLOAT", arr(3, 4, Num::Float, rgb), false, ch),
        7 => f("R32G32B32_UINT", arr(3, 4, Num::Uint, rgb), false, ch),
        8 => f("R32G32B32_SINT", arr(3, 4, Num::Sint, rgb), false, ch),
        9 | 10 => f("R16G16B16A16_FLOAT", arr(4, 2, Num::Float, rgba), false, ch),
        11 => f("R16G16B16A16_UNORM", arr(4, 2, Num::Unorm, rgba), false, ch),
        12 => f("R16G16B16A16_UINT", arr(4, 2, Num::Uint, rgba), false, ch),
        13 => f("R16G16B16A16_SNORM", arr(4, 2, Num::Snorm, rgba), false, ch),
        14 => f("R16G16B16A16_SINT", arr(4, 2, Num::Sint, rgba), false, ch),
        15 | 16 => f("R32G32_FLOAT", arr(2, 4, Num::Float, rg), false, c2),
        17 => f("R32G32_UINT", arr(2, 4, Num::Uint, rg), false, c2),
        18 => f("R32G32_SINT", arr(2, 4, Num::Sint, rg), false, c2),
        19..=22 => f("D32_FLOAT_S8X24_UINT", crate::formats::depth(8, 0, 32), false, ch),
        23 | 24 => f("R10G10B10A2_UNORM", packed(4, [(0, 10), (10, 10), (20, 10), (30, 2)]), false, ch),
        25 => f("R10G10B10A2_UINT", packed(4, [(0, 10), (10, 10), (20, 10), (30, 2)]), false, ch),
        26 => f("R11G11B10_FLOAT", Codec::Plain(Plain::B10G11R11), false, ch),
        27 | 28 => f("R8G8B8A8_UNORM", arr(4, 1, Num::Unorm, rgba), false, ch),
        29 => f("R8G8B8A8_UNORM_SRGB", arr(4, 1, Num::Unorm, rgba), true, ch),
        30 => f("R8G8B8A8_UINT", arr(4, 1, Num::Uint, rgba), false, ch),
        31 => f("R8G8B8A8_SNORM", arr(4, 1, Num::Snorm, rgba), false, ch),
        32 => f("R8G8B8A8_SINT", arr(4, 1, Num::Sint, rgba), false, ch),
        33 | 34 => f("R16G16_FLOAT", arr(2, 2, Num::Float, rg), false, c2),
        35 => f("R16G16_UNORM", arr(2, 2, Num::Unorm, rg), false, c2),
        36 => f("R16G16_UINT", arr(2, 2, Num::Uint, rg), false, c2),
        37 => f("R16G16_SNORM", arr(2, 2, Num::Snorm, rg), false, c2),
        38 => f("R16G16_SINT", arr(2, 2, Num::Sint, rg), false, c2),
        39 | 41 => f("R32_FLOAT", arr(1, 4, Num::Float, r1), false, c1),
        40 => f("D32_FLOAT", arr(1, 4, Num::Float, [L, X, X, X]), false, ch),
        42 => f("R32_UINT", arr(1, 4, Num::Uint, r1), false, c1),
        43 => f("R32_SINT", arr(1, 4, Num::Sint, r1), false, c1),
        44..=47 => f("D24_UNORM_S8_UINT", crate::formats::depth(4, 0, 24), false, ch),
        48 | 49 => f("R8G8_UNORM", arr(2, 1, Num::Unorm, rg), false, c2),
        50 => f("R8G8_UINT", arr(2, 1, Num::Uint, rg), false, c2),
        51 => f("R8G8_SNORM", arr(2, 1, Num::Snorm, rg), false, c2),
        52 => f("R8G8_SINT", arr(2, 1, Num::Sint, rg), false, c2),
        53 | 54 => f("R16_FLOAT", arr(1, 2, Num::Float, r1), false, c1),
        55 => f("D16_UNORM", arr(1, 2, Num::Unorm, [L, X, X, X]), false, ch),
        56 => f("R16_UNORM", arr(1, 2, Num::Unorm, r1), false, c1),
        57 => f("R16_UINT", arr(1, 2, Num::Uint, r1), false, c1),
        58 => f("R16_SNORM", arr(1, 2, Num::Snorm, r1), false, c1),
        59 => f("R16_SINT", arr(1, 2, Num::Sint, r1), false, c1),
        60 | 61 => f("R8_UNORM", arr(1, 1, Num::Unorm, r1), false, c1),
        62 => f("R8_UINT", arr(1, 1, Num::Uint, r1), false, c1),
        63 => f("R8_SNORM", arr(1, 1, Num::Snorm, r1), false, c1),
        64 => f("R8_SINT", arr(1, 1, Num::Sint, r1), false, c1),
        65 => f("A8_UNORM", arr(1, 1, Num::Unorm, [A, X, X, X]), false, Channels::A),
        67 => f("R9G9B9E5_SHAREDEXP", Codec::Plain(Plain::E5B9G9R9), false, ch),
        68 => f("R8G8_B8G8_UNORM", Codec::Pairs422 { bytes: 1, g_first: false }, false, ch),
        69 => f("G8R8_G8B8_UNORM", Codec::Pairs422 { bytes: 1, g_first: true }, false, ch),
        70 | 71 => f("BC1_UNORM", Codec::Bc1 { alpha: true }, false, ch),
        72 => f("BC1_UNORM_SRGB", Codec::Bc1 { alpha: true }, true, ch),
        73 | 74 => f("BC2_UNORM", Codec::Bc2, false, ch),
        75 => f("BC2_UNORM_SRGB", Codec::Bc2, true, ch),
        76 | 77 => f("BC3_UNORM", Codec::Bc3, false, ch),
        78 => f("BC3_UNORM_SRGB", Codec::Bc3, true, ch),
        79 | 80 => f("BC4_UNORM", Codec::Bc4 { signed: false }, false, c1),
        81 => f("BC4_SNORM", Codec::Bc4 { signed: true }, false, c1),
        82 | 83 => f("BC5_UNORM", Codec::Bc5 { signed: false }, false, c2),
        84 => f("BC5_SNORM", Codec::Bc5 { signed: true }, false, c2),
        85 => f("B5G6R5_UNORM", packed(2, [(11, 5), (5, 6), (0, 5), (0, 0)]), false, ch),
        86 => f("B5G5R5A1_UNORM", packed(2, [(10, 5), (5, 5), (0, 5), (15, 1)]), false, ch),
        87 | 90 => f("B8G8R8A8_UNORM", arr(4, 1, Num::Unorm, [B, G, R, A]), false, ch),
        88 | 92 => f("B8G8R8X8_UNORM", arr(4, 1, Num::Unorm, [B, G, R, X]), false, ch),
        89 => f("R10G10B10_XR_BIAS_A2_UNORM", Codec::Plain(Plain::XrBias), false, ch),
        91 => f("B8G8R8A8_UNORM_SRGB", arr(4, 1, Num::Unorm, [B, G, R, A]), true, ch),
        93 => f("B8G8R8X8_UNORM_SRGB", arr(4, 1, Num::Unorm, [B, G, R, X]), true, ch),
        94 | 95 => f("BC6H_UF16", Codec::Bc6h { signed: false }, false, ch),
        96 => f("BC6H_SF16", Codec::Bc6h { signed: true }, false, ch),
        97 | 98 => f("BC7_UNORM", Codec::Bc7, false, ch),
        99 => f("BC7_UNORM_SRGB", Codec::Bc7, true, ch),
        // Video formats.  Their RGB is gamma-encoded like sRGB.
        // 420_OPAQUE's layout is private to drivers; files that hold it
        // can only have it as NV12, the layout its size matches.
        100 => f("AYUV", Codec::Yuv(Yuv::Ayuv), true, ch),
        101 => f("Y410", Codec::Yuv(Yuv::Y410), true, ch),
        102 => f("Y416", Codec::Yuv(Yuv::Y416), true, ch),
        103 => f("NV12", Codec::Yuv(Yuv::Nv12), true, ch),
        104 => f("P010", Codec::Yuv(Yuv::P010), true, ch),
        105 => f("P016", Codec::Yuv(Yuv::P016), true, ch),
        106 => f("420_OPAQUE", Codec::Yuv(Yuv::Nv12), true, ch),
        107 => f("YUY2", Codec::Yuv(Yuv::Yuy2), true, ch),
        108 => f("Y210", Codec::Yuv(Yuv::Y210), true, ch),
        109 => f("Y216", Codec::Yuv(Yuv::Y216), true, ch),
        110 => f("NV11", Codec::Yuv(Yuv::Nv11), true, ch),
        115 => f("B4G4R4A4_UNORM", packed(2, [(8, 4), (4, 4), (0, 4), (12, 4)]), false, ch),
        191 => f("A4B4G4R4_UNORM", packed(2, [(12, 4), (8, 4), (4, 4), (0, 4)]), false, ch),
        // ASTC was given DXGI numbers that never shipped in Direct3D, but
        // some tools write them: typeless, UNORM and sRGB per block size.
        133..=188 => {
            let (i, variant) = ((v - 133) / 4, (v - 133) % 4);
            let (w, h) = ASTC_BLOCKS[i as usize];
            if variant == 3 {
                return None;
            }
            f(DXGI_ASTC[i as usize], Codec::Astc { w, h, d: 1, hdr: false }, variant == 2, ch)
        }
        _ => None,
    }
}

const DXGI_ASTC: [&str; 14] = [
    "ASTC_4X4_UNORM",
    "ASTC_5X4_UNORM",
    "ASTC_5X5_UNORM",
    "ASTC_6X5_UNORM",
    "ASTC_6X6_UNORM",
    "ASTC_8X5_UNORM",
    "ASTC_8X6_UNORM",
    "ASTC_8X8_UNORM",
    "ASTC_10X5_UNORM",
    "ASTC_10X6_UNORM",
    "ASTC_10X8_UNORM",
    "ASTC_10X10_UNORM",
    "ASTC_12X10_UNORM",
    "ASTC_12X12_UNORM",
];

//endregion
