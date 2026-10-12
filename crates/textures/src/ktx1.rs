//! KTX 1.1: the older Khronos container, which names formats the OpenGL way
//! (an internal format, plus a format and type for uncompressed data) and
//! pads rows and faces to four bytes.

use crate::assemble::{Layout, assemble};
use crate::blocks::decode_surface;
use crate::formats::{
    ASTC_3D_BLOCKS, ASTC_BLOCKS, Codec, Dest, Num, PixelFormat, Plain, arr, channel_tag, channels_of_dest, fmt, packed,
};
use crate::thumb::Channels;
use crate::util::{Reader, invalid, unsupported};
use crate::{ColorSpace, DecodeOptions, SourceInfo, Texture, TextureError, TextureKind};

/// The 13 header words after the identifier.
struct Header {
    gl_type: u32,
    type_size: u32,
    gl_format: u32,
    internal: u32,
    width: u32,
    height: u32,
    depth: u32,
    layers: u32,
    faces: u32,
    levels: u32,
    kvd_len: u32,
    big_endian: bool,
}

pub(crate) fn decode(bytes: &[u8], opts: &DecodeOptions) -> Result<Texture, TextureError> {
    // 1. The header, in whichever byte order the file was written.
    let mut r = Reader::at(bytes, 12);
    let endian = r.u32()?;
    let big_endian = match endian {
        0x0403_0201 => false,
        0x0102_0304 => true,
        _ => return Err(invalid("KTX1: bad endianness marker")),
    };
    let mut word = || -> Result<u32, TextureError> { r.u32().map(|v| if big_endian { v.swap_bytes() } else { v }) };
    let h = Header {
        gl_type: word()?,
        type_size: word()?,
        gl_format: word()?,
        internal: word()?,
        width: {
            word()?; // glBaseInternalFormat
            word()?
        },
        height: word()?.max(1),
        depth: word()?,
        layers: word()?,
        faces: word()?,
        levels: word()?,
        kvd_len: word()?,
        big_endian,
    };
    if (h.faces != 1 && h.faces != 6) || h.levels > 32 {
        return Err(invalid("KTX1: bad header"));
    }
    crate::util::check_layout(h.width, h.height, h.depth.max(1), h.layers.max(1))?;
    let kvd = crate::util::slice(bytes, 64, h.kvd_len as u64, "KTX1 key/value data")?;
    let orientation = kv_text(kvd, h.big_endian, "KTXorientation");

    // 2. The format.
    let fmt = gl_format(&h).ok_or_else(|| {
        unsupported(format!(
            "KTX1 format 0x{:04x} (format 0x{:04x}, type 0x{:04x})",
            h.internal, h.gl_format, h.gl_type
        ))
    })?;
    let kind = if h.faces == 6 {
        TextureKind::Cube
    } else if h.depth > 0 {
        TextureKind::D3
    } else {
        TextureKind::D2
    };
    // KTX1 writes orientation as "S=r,T=u" (T=u: rows bottom to top).
    let flip_y = orientation.as_deref().is_some_and(|o| o.contains("T=u"));
    let flip_x = orientation.as_deref().is_some_and(|o| o.contains("S=l"));
    let layout = Layout {
        kind,
        width: h.width,
        height: h.height,
        depth: h.depth.max(1),
        layers: h.layers.max(1),
        faces: h.faces,
        levels: h.levels.max(1),
        color_space: if fmt.srgb { ColorSpace::Srgb } else { ColorSpace::Linear },
        flip_y,
        flip_x,
        alpha: fmt.codec.has_alpha(false),
        source: SourceInfo {
            container: "KTX".into(),
            format: format!("{}{}", fmt.name, channel_tag(fmt.channels)),
            file_bytes: bytes.len() as u64,
            ..Default::default()
        },
    };

    // 3. Find where each level starts: levels are stored one after another,
    //    each with its size first.
    let mut starts = Vec::new();
    let mut at = 64 + h.kvd_len as u64;
    for _ in 0..layout.levels {
        let size = Reader::at(bytes, at as usize).u32().map(|v| if h.big_endian { v.swap_bytes() } else { v });
        let Ok(size) = size else { break };
        starts.push((at + 4, size as u64));
        // A plain cubemap's size is per face, and each face is padded.
        let padded = (size as u64).div_ceil(4) * 4;
        at += 4 + if h.faces == 6 && h.layers == 0 { padded * 6 } else { padded };
    }
    if starts.is_empty() {
        return Err(invalid("KTX1: no image data"));
    }
    let blocks = !matches!(fmt.codec, Codec::Plain(_));
    let levels = starts.len() as u32;
    let layout = Layout { levels, ..layout };
    assemble(layout, opts, |level, layers| {
        let (w, ht) = (crate::util::mip_dim(h.width, level), crate::util::mip_dim(h.height, level));
        let d = if h.depth > 0 { crate::util::mip_dim(h.depth, level) } else { 1 };
        let (gx, gy) = fmt.codec.grid(w, ht);
        let row = gx as usize * fmt.codec.block_bytes();
        // Uncompressed rows are padded to four bytes; block rows are not.
        let pitch = if blocks { row } else { row.div_ceil(4) * 4 };
        let face_bytes = pitch as u64 * gy as u64 * d.div_ceil(fmt.codec.block_depth()) as u64;
        let face_stride = face_bytes.div_ceil(4) * 4;
        let (start, _) = starts[level as usize];
        let mut out = Vec::new();
        for i in 0..(layers * h.faces) as u64 {
            let data = crate::util::slice(bytes, start + i * face_stride, face_bytes, "KTX1 level")?;
            let data = if h.big_endian && h.type_size > 1 { swap_words(data, h.type_size) } else { data.to_vec() };
            out.push(decode_surface(&fmt, w, ht, d, &data, Some(pitch))?);
        }
        Ok(out)
    })
}

fn swap_words(data: &[u8], size: u32) -> Vec<u8> {
    let mut v = data.to_vec();
    for w in v.chunks_exact_mut(size as usize) {
        w.reverse();
    }
    v
}

/// A text value from KTX1's key/value data.
fn kv_text(kvd: &[u8], big_endian: bool, key: &str) -> Option<String> {
    let mut r = Reader::new(kvd);
    while let Ok(len) = r.u32() {
        let len = if big_endian { len.swap_bytes() } else { len } as usize;
        let entry = r.bytes(len).ok()?;
        let nul = entry.iter().position(|&b| b == 0)?;
        if &entry[..nul] == key.as_bytes() {
            let v = &entry[nul + 1..];
            return Some(String::from_utf8_lossy(v.split(|&b| b == 0).next().unwrap_or(v)).into_owned());
        }
        r.skip((4 - len % 4) % 4).ok()?;
    }
    None
}

/*┌─────────────────╮
| OpenGL formats  |
└─────────────────╯*/
//region OpenGL formats

use Dest::{A, B, G, L, R, X};

/// The format of a KTX1 file: compressed formats by internal format,
/// uncompressed ones from their format and type.
fn gl_format(h: &Header) -> Option<PixelFormat> {
    if h.gl_type == 0 || h.gl_format == 0 {
        return gl_compressed(h.internal);
    }
    let srgb = matches!(h.internal, 0x8C40..=0x8C47);
    let name = gl_internal_name(h.internal);
    let (n, dest): (u8, [Dest; 4]) = match h.gl_format {
        0x1903 | 0x8D94 => (1, [R, X, X, X]), // RED, RED_INTEGER
        0x8227 | 0x8228 => (2, [R, G, X, X]), // RG, RG_INTEGER
        0x1907 | 0x8D98 => (3, [R, G, B, X]), // RGB, RGB_INTEGER
        0x1908 | 0x8D99 => (4, [R, G, B, A]), // RGBA, RGBA_INTEGER
        0x80E0 | 0x8D9A => (3, [B, G, R, X]), // BGR, BGR_INTEGER
        0x80E1 | 0x8D9B => (4, [B, G, R, A]), // BGRA, BGRA_INTEGER
        0x1909 | 0x1902 => (1, [L, X, X, X]), // LUMINANCE, DEPTH_COMPONENT
        0x190A => (2, [L, A, X, X]),          // LUMINANCE_ALPHA
        0x1906 => (1, [A, X, X, X]),          // ALPHA
        _ => return None,
    };
    let integer = matches!(h.gl_format, 0x8D94..=0x8D9B | 0x8228);
    let codec = match h.gl_type {
        0x1401 => arr(n, 1, if integer { Num::Uint } else { Num::Unorm }, dest),
        0x1400 => arr(n, 1, if integer { Num::Sint } else { Num::Snorm }, dest),
        0x1403 => arr(n, 2, if integer { Num::Uint } else { Num::Unorm }, dest),
        0x1402 => arr(n, 2, if integer { Num::Sint } else { Num::Snorm }, dest),
        0x1405 => arr(n, 4, Num::Uint, dest),
        0x1404 => arr(n, 4, Num::Sint, dest),
        0x140B | 0x8D61 => arr(n, 2, Num::Float, dest),
        0x1406 => arr(n, 4, Num::Float, dest),
        0x8363 => packed(2, [(11, 5), (5, 6), (0, 5), (0, 0)]), // UNSIGNED_SHORT_5_6_5
        0x8033 => packed(2, [(12, 4), (8, 4), (4, 4), (0, 4)]), // UNSIGNED_SHORT_4_4_4_4
        0x8034 => packed(2, [(11, 5), (6, 5), (1, 5), (0, 1)]), // UNSIGNED_SHORT_5_5_5_1
        0x8368 if h.gl_format == 0x80E1 => packed(4, [(20, 10), (10, 10), (0, 10), (30, 2)]),
        0x8368 => packed(4, [(0, 10), (10, 10), (20, 10), (30, 2)]), // UNSIGNED_INT_2_10_10_10_REV
        0x8C3B => Codec::Plain(Plain::B10G11R11),
        0x8C3E => Codec::Plain(Plain::E5B9G9R9),
        _ => return None,
    };
    let channels = match codec {
        Codec::Plain(Plain::Array { .. }) => channels_of_dest(dest, n),
        _ => Channels::Rgba,
    };
    Some(fmt(name, codec, srgb, channels))
}

/// Names of the sized internal formats, for the inspector.
fn gl_internal_name(v: u32) -> &'static str {
    match v {
        0x8229 => "GL_R8",
        0x822B => "GL_RG8",
        0x8051 => "GL_RGB8",
        0x8058 => "GL_RGBA8",
        0x8C41 => "GL_SRGB8",
        0x8C43 => "GL_SRGB8_ALPHA8",
        0x822A => "GL_R16",
        0x822C => "GL_RG16",
        0x8054 => "GL_RGB16",
        0x805B => "GL_RGBA16",
        0x822D => "GL_R16F",
        0x822F => "GL_RG16F",
        0x881B => "GL_RGB16F",
        0x881A => "GL_RGBA16F",
        0x822E => "GL_R32F",
        0x8230 => "GL_RG32F",
        0x8815 => "GL_RGB32F",
        0x8814 => "GL_RGBA32F",
        0x8C3A => "GL_R11F_G11F_B10F",
        0x8C3D => "GL_RGB9_E5",
        0x8D62 => "GL_RGB565",
        0x8056 => "GL_RGBA4",
        0x8057 => "GL_RGB5_A1",
        0x8059 => "GL_RGB10_A2",
        0x8F94 => "GL_R8_SNORM",
        0x8F95 => "GL_RG8_SNORM",
        0x8F97 => "GL_RGBA8_SNORM",
        0x1903 => "GL_RED",
        0x8227 => "GL_RG",
        0x1907 => "GL_RGB",
        0x1908 => "GL_RGBA",
        0x80E1 => "GL_BGRA",
        0x1909 => "GL_LUMINANCE",
        0x190A => "GL_LUMINANCE_ALPHA",
        0x1906 => "GL_ALPHA",
        0x8040 => "GL_LUMINANCE8",
        0x8045 => "GL_LUMINANCE8_ALPHA8",
        0x803C => "GL_ALPHA8",
        _ => "uncompressed",
    }
}

fn gl_compressed(v: u32) -> Option<PixelFormat> {
    let ch = Channels::Rgba;
    let f = |name, codec, srgb, channels| Some(fmt(name, codec, srgb, channels));
    match v {
        0x83F0 => f("GL_COMPRESSED_RGB_S3TC_DXT1", Codec::Bc1 { alpha: false }, false, ch),
        0x83F1 => f("GL_COMPRESSED_RGBA_S3TC_DXT1", Codec::Bc1 { alpha: true }, false, ch),
        0x83F2 => f("GL_COMPRESSED_RGBA_S3TC_DXT3", Codec::Bc2, false, ch),
        0x83F3 => f("GL_COMPRESSED_RGBA_S3TC_DXT5", Codec::Bc3, false, ch),
        0x8C4C => f("GL_COMPRESSED_SRGB_S3TC_DXT1", Codec::Bc1 { alpha: false }, true, ch),
        0x8C4D => f("GL_COMPRESSED_SRGB_ALPHA_S3TC_DXT1", Codec::Bc1 { alpha: true }, true, ch),
        0x8C4E => f("GL_COMPRESSED_SRGB_ALPHA_S3TC_DXT3", Codec::Bc2, true, ch),
        0x8C4F => f("GL_COMPRESSED_SRGB_ALPHA_S3TC_DXT5", Codec::Bc3, true, ch),
        0x8DBB => f("GL_COMPRESSED_RED_RGTC1", Codec::Bc4 { signed: false }, false, Channels::R),
        0x8DBC => f("GL_COMPRESSED_SIGNED_RED_RGTC1", Codec::Bc4 { signed: true }, false, Channels::R),
        0x8DBD => f("GL_COMPRESSED_RG_RGTC2", Codec::Bc5 { signed: false }, false, Channels::Rg),
        0x8DBE => f("GL_COMPRESSED_SIGNED_RG_RGTC2", Codec::Bc5 { signed: true }, false, Channels::Rg),
        0x8E8C => f("GL_COMPRESSED_RGBA_BPTC_UNORM", Codec::Bc7, false, ch),
        0x8E8D => f("GL_COMPRESSED_SRGB_ALPHA_BPTC_UNORM", Codec::Bc7, true, ch),
        0x8E8E => f("GL_COMPRESSED_RGB_BPTC_SIGNED_FLOAT", Codec::Bc6h { signed: true }, false, ch),
        0x8E8F => f("GL_COMPRESSED_RGB_BPTC_UNSIGNED_FLOAT", Codec::Bc6h { signed: false }, false, ch),
        0x8D64 => f("GL_ETC1_RGB8_OES", Codec::Etc1, false, ch),
        0x9274 => f("GL_COMPRESSED_RGB8_ETC2", Codec::Etc2Rgb, false, ch),
        0x9275 => f("GL_COMPRESSED_SRGB8_ETC2", Codec::Etc2Rgb, true, ch),
        0x9276 => f("GL_COMPRESSED_RGB8_PUNCHTHROUGH_ALPHA1_ETC2", Codec::Etc2Rgba1, false, ch),
        0x9277 => f("GL_COMPRESSED_SRGB8_PUNCHTHROUGH_ALPHA1_ETC2", Codec::Etc2Rgba1, true, ch),
        0x9278 => f("GL_COMPRESSED_RGBA8_ETC2_EAC", Codec::Etc2Rgba, false, ch),
        0x9279 => f("GL_COMPRESSED_SRGB8_ALPHA8_ETC2_EAC", Codec::Etc2Rgba, true, ch),
        0x9270 => f("GL_COMPRESSED_R11_EAC", Codec::EacR { signed: false }, false, Channels::R),
        0x9271 => f("GL_COMPRESSED_SIGNED_R11_EAC", Codec::EacR { signed: true }, false, Channels::R),
        0x9272 => f("GL_COMPRESSED_RG11_EAC", Codec::EacRg { signed: false }, false, Channels::Rg),
        0x9273 => f("GL_COMPRESSED_SIGNED_RG11_EAC", Codec::EacRg { signed: true }, false, Channels::Rg),
        0x93B0..=0x93BD => {
            let (w, h) = ASTC_BLOCKS[(v - 0x93B0) as usize];
            f(GL_ASTC[(v - 0x93B0) as usize], Codec::Astc { w, h, d: 1, hdr: false }, false, ch)
        }
        0x93D0..=0x93DD => {
            let (w, h) = ASTC_BLOCKS[(v - 0x93D0) as usize];
            f(GL_ASTC_SRGB[(v - 0x93D0) as usize], Codec::Astc { w, h, d: 1, hdr: false }, true, ch)
        }
        // OES_texture_compression_astc's 3D footprints.
        0x93C0..=0x93C9 => {
            let (w, h, d) = ASTC_3D_BLOCKS[(v - 0x93C0) as usize];
            f(GL_ASTC_3D[(v - 0x93C0) as usize], Codec::Astc { w, h, d, hdr: false }, false, ch)
        }
        0x93E0..=0x93E9 => {
            let (w, h, d) = ASTC_3D_BLOCKS[(v - 0x93E0) as usize];
            f(GL_ASTC_3D_SRGB[(v - 0x93E0) as usize], Codec::Astc { w, h, d, hdr: false }, true, ch)
        }
        0x8C00 => f("GL_COMPRESSED_RGB_PVRTC_4BPPV1_IMG", Codec::Pvrtc { bpp: 4 }, false, ch),
        0x8C01 => f("GL_COMPRESSED_RGB_PVRTC_2BPPV1_IMG", Codec::Pvrtc { bpp: 2 }, false, ch),
        0x8C02 => f("GL_COMPRESSED_RGBA_PVRTC_4BPPV1_IMG", Codec::Pvrtc { bpp: 4 }, false, ch),
        0x8C03 => f("GL_COMPRESSED_RGBA_PVRTC_2BPPV1_IMG", Codec::Pvrtc { bpp: 2 }, false, ch),
        0x8A54 => f("GL_COMPRESSED_SRGB_PVRTC_2BPPV1_EXT", Codec::Pvrtc { bpp: 2 }, true, ch),
        0x8A55 => f("GL_COMPRESSED_SRGB_PVRTC_4BPPV1_EXT", Codec::Pvrtc { bpp: 4 }, true, ch),
        0x8A56 => f("GL_COMPRESSED_SRGB_ALPHA_PVRTC_2BPPV1_EXT", Codec::Pvrtc { bpp: 2 }, true, ch),
        0x8A57 => f("GL_COMPRESSED_SRGB_ALPHA_PVRTC_4BPPV1_EXT", Codec::Pvrtc { bpp: 4 }, true, ch),
        0x8C92 => f("GL_ATC_RGB_AMD", Codec::AtcRgb, false, ch),
        0x87EE => f("GL_ATC_RGBA_INTERPOLATED_ALPHA_AMD", Codec::AtcRgba, false, ch),
        _ => None,
    }
}

const GL_ASTC: [&str; 14] = [
    "GL_COMPRESSED_RGBA_ASTC_4x4",
    "GL_COMPRESSED_RGBA_ASTC_5x4",
    "GL_COMPRESSED_RGBA_ASTC_5x5",
    "GL_COMPRESSED_RGBA_ASTC_6x5",
    "GL_COMPRESSED_RGBA_ASTC_6x6",
    "GL_COMPRESSED_RGBA_ASTC_8x5",
    "GL_COMPRESSED_RGBA_ASTC_8x6",
    "GL_COMPRESSED_RGBA_ASTC_8x8",
    "GL_COMPRESSED_RGBA_ASTC_10x5",
    "GL_COMPRESSED_RGBA_ASTC_10x6",
    "GL_COMPRESSED_RGBA_ASTC_10x8",
    "GL_COMPRESSED_RGBA_ASTC_10x10",
    "GL_COMPRESSED_RGBA_ASTC_12x10",
    "GL_COMPRESSED_RGBA_ASTC_12x12",
];
const GL_ASTC_3D: [&str; 10] = [
    "GL_COMPRESSED_RGBA_ASTC_3x3x3_OES",
    "GL_COMPRESSED_RGBA_ASTC_4x3x3_OES",
    "GL_COMPRESSED_RGBA_ASTC_4x4x3_OES",
    "GL_COMPRESSED_RGBA_ASTC_4x4x4_OES",
    "GL_COMPRESSED_RGBA_ASTC_5x4x4_OES",
    "GL_COMPRESSED_RGBA_ASTC_5x5x4_OES",
    "GL_COMPRESSED_RGBA_ASTC_5x5x5_OES",
    "GL_COMPRESSED_RGBA_ASTC_6x5x5_OES",
    "GL_COMPRESSED_RGBA_ASTC_6x6x5_OES",
    "GL_COMPRESSED_RGBA_ASTC_6x6x6_OES",
];
const GL_ASTC_3D_SRGB: [&str; 10] = [
    "GL_COMPRESSED_SRGB8_ALPHA8_ASTC_3x3x3_OES",
    "GL_COMPRESSED_SRGB8_ALPHA8_ASTC_4x3x3_OES",
    "GL_COMPRESSED_SRGB8_ALPHA8_ASTC_4x4x3_OES",
    "GL_COMPRESSED_SRGB8_ALPHA8_ASTC_4x4x4_OES",
    "GL_COMPRESSED_SRGB8_ALPHA8_ASTC_5x4x4_OES",
    "GL_COMPRESSED_SRGB8_ALPHA8_ASTC_5x5x4_OES",
    "GL_COMPRESSED_SRGB8_ALPHA8_ASTC_5x5x5_OES",
    "GL_COMPRESSED_SRGB8_ALPHA8_ASTC_6x5x5_OES",
    "GL_COMPRESSED_SRGB8_ALPHA8_ASTC_6x6x5_OES",
    "GL_COMPRESSED_SRGB8_ALPHA8_ASTC_6x6x6_OES",
];
const GL_ASTC_SRGB: [&str; 14] = [
    "GL_COMPRESSED_SRGB8_ALPHA8_ASTC_4x4",
    "GL_COMPRESSED_SRGB8_ALPHA8_ASTC_5x4",
    "GL_COMPRESSED_SRGB8_ALPHA8_ASTC_5x5",
    "GL_COMPRESSED_SRGB8_ALPHA8_ASTC_6x5",
    "GL_COMPRESSED_SRGB8_ALPHA8_ASTC_6x6",
    "GL_COMPRESSED_SRGB8_ALPHA8_ASTC_8x5",
    "GL_COMPRESSED_SRGB8_ALPHA8_ASTC_8x6",
    "GL_COMPRESSED_SRGB8_ALPHA8_ASTC_8x8",
    "GL_COMPRESSED_SRGB8_ALPHA8_ASTC_10x5",
    "GL_COMPRESSED_SRGB8_ALPHA8_ASTC_10x6",
    "GL_COMPRESSED_SRGB8_ALPHA8_ASTC_10x8",
    "GL_COMPRESSED_SRGB8_ALPHA8_ASTC_10x10",
    "GL_COMPRESSED_SRGB8_ALPHA8_ASTC_12x10",
    "GL_COMPRESSED_SRGB8_ALPHA8_ASTC_12x12",
];

//endregion
