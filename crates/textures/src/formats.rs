//! Pixel formats: how a container's texels are stored, named once here so
//! KTX2 (Vulkan formats), KTX1 (OpenGL formats) and DDS (DXGI formats and
//! legacy pixel-format masks) share one set of decoders.

use crate::thumb::Channels;

/// The numeric type of a whole-byte component.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Num {
    Unorm,
    Snorm,
    Uint,
    Sint,
    Float,
    /// Signed fixed point with 5 fractional bits (NVIDIA's S10_5).
    Fixed5,
}

/// Where one stored component goes in the decoded RGBA texel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Dest {
    R,
    G,
    B,
    A,
    /// Luminance: copied to red, green and blue.
    L,
    /// Padding or a channel Slerp doesn't show (stencil).
    X,
}

/// Uncompressed texel layouts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Plain {
    /// `n` components of `bytes` bytes each, little-endian, in memory order.
    Array { n: u8, bytes: u8, num: Num, dest: [Dest; 4] },
    /// Unsigned normalised bit fields in a little-endian word of `bytes`
    /// bytes: `(shift, bits)` for red, green, blue and alpha, where zero
    /// bits means the channel is absent.
    Packed { bytes: u8, fields: [(u8, u8); 4] },
    /// Three small unsigned floats: red 11 bits from bit 0, green 11 from
    /// bit 11, blue 10 from bit 22.
    B10G11R11,
    /// Shared-exponent RGB.
    E5B9G9R9,
    /// Depth in a texel of `bytes` bytes: `bits` bits from bit `shift`,
    /// unsigned normalised or (for 32 bits) a float.  Shown as grey; any
    /// stencil beside it is ignored.
    Depth { bytes: u8, shift: u8, bits: u8 },
    /// DXGI's R10G10B10_XR_BIAS_A2: 10-bit fields where 384 means 0 and
    /// 894 means 1, so values reach beyond 0..1.
    XrBias,
}

impl Plain {
    pub fn texel_bytes(&self) -> usize {
        match *self {
            Plain::Array { n, bytes, .. } => n as usize * bytes as usize,
            Plain::Packed { bytes, .. } | Plain::Depth { bytes, .. } => bytes as usize,
            _ => 4,
        }
    }
}

/// How texels are stored: block compression schemes, or plain texels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Codec {
    /// BC1; with `alpha` false the punch-through alpha is ignored.
    Bc1 {
        alpha: bool,
    },
    Bc2,
    Bc3,
    Bc4 {
        signed: bool,
    },
    Bc5 {
        signed: bool,
    },
    Bc6h {
        signed: bool,
    },
    Bc7,
    Etc1,
    Etc2Rgb,
    Etc2Rgba1,
    Etc2Rgba,
    EacR {
        signed: bool,
    },
    EacRg {
        signed: bool,
    },
    /// ASTC with this block footprint (`d` is 1 for 2D blocks).  `hdr` is
    /// set when the format promises HDR (Vulkan's SFLOAT formats); otherwise
    /// blocks are checked.
    Astc {
        w: u8,
        h: u8,
        d: u8,
        hdr: bool,
    },
    /// PVRTC version 1; 2 or 4 bits per pixel.
    Pvrtc {
        bpp: u8,
    },
    AtcRgb,
    AtcRgba,
    /// 4:2:2 pairs of texels sharing red and blue, with components of
    /// `bytes` bytes in the order G0 B G1 R (`g_first`) or B G0 R G1.
    Pairs422 {
        bytes: u8,
        g_first: bool,
    },
    /// Video formats that store luma and chroma (see `yuv`).
    Yuv(Yuv),
    Plain(Plain),
}

/// Video formats from DXGI 1.2 and Direct3D 9, stored as luma and chroma
/// rather than RGB.  The block shape `Codec` gives them describes their
/// rows: planar layouts count the chroma plane's rows after the luma rows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Yuv {
    /// 4:4:4, 8-bit V U Y A.
    Ayuv,
    /// 4:4:4, 10-bit U Y V and 2-bit alpha in a 32-bit word.
    Y410,
    /// 4:4:4, 16-bit U Y V A.
    Y416,
    /// 4:2:2 pairs: Y0 U Y1 V, 8-bit.
    Yuy2,
    /// 4:2:2 pairs: U Y0 V Y1, 8-bit.
    Uyvy,
    /// 4:2:2 pairs: Y0 U Y1 V, 10 bits at the top of 16.
    Y210,
    /// 4:2:2 pairs: Y0 U Y1 V, 16-bit.
    Y216,
    /// 4:2:2 pairs: U Y0 V Y1, 16-bit (or 10 and 12 bits at the top of
    /// 16): Vulkan's `B16G16R16G16_422` and its packed kin, which DXGI lacks.
    Uyvy16,
    /// 4:2:0: an 8-bit luma plane, then interleaved U V at half size.
    Nv12,
    /// NV12 with 10 bits at the top of 16.
    P010,
    /// NV12 with 16-bit samples.
    P016,
    /// 4:1:1: an 8-bit luma plane, then U V pairs at quarter width.
    Nv11,
}

impl Codec {
    /// The block footprint in texels; plain formats are 1x1.
    pub fn block(&self) -> (u32, u32) {
        match *self {
            Codec::Astc { w, h, .. } => (w as u32, h as u32),
            Codec::Pvrtc { bpp: 2 } => (8, 4),
            Codec::Pairs422 { .. } => (2, 1),
            Codec::Yuv(Yuv::Ayuv | Yuv::Y410 | Yuv::Y416) => (1, 1),
            Codec::Yuv(Yuv::Nv11) => (4, 1),
            Codec::Yuv(_) => (2, 1),
            Codec::Plain(_) => (1, 1),
            _ => (4, 4),
        }
    }

    /// The block depth in texels: more than 1 only for 3D ASTC.
    pub fn block_depth(&self) -> u32 {
        match *self {
            Codec::Astc { d, .. } => d.max(1) as u32,
            _ => 1,
        }
    }

    /// How many blocks across and down a `w` x `h` image is stored as.
    /// PVRTC1 always stores at least two by two blocks, because its texels
    /// blend colours from the neighbouring blocks.  Planar video formats
    /// count the rows of both planes: 4:2:0 chroma adds half as many rows,
    /// and NV11's chroma as many again (half filled, as Direct3D sizes it).
    pub fn grid(&self, w: u32, h: u32) -> (u32, u32) {
        let (bw, bh) = self.block();
        match self {
            Codec::Pvrtc { .. } => (w.div_ceil(bw).max(2), h.div_ceil(bh).max(2)),
            Codec::Yuv(Yuv::Nv12 | Yuv::P010 | Yuv::P016) => (w.div_ceil(bw), h + h.div_ceil(2)),
            Codec::Yuv(Yuv::Nv11) => (w.div_ceil(bw), 2 * h),
            _ => (w.div_ceil(bw), h.div_ceil(bh)),
        }
    }

    pub fn block_bytes(&self) -> usize {
        match self {
            Codec::Bc1 { .. } | Codec::Bc4 { .. } | Codec::Etc1 | Codec::Etc2Rgb | Codec::Etc2Rgba1 => 8,
            Codec::EacR { .. } | Codec::AtcRgb | Codec::Pvrtc { .. } => 8,
            Codec::Plain(p) => p.texel_bytes(),
            Codec::Pairs422 { bytes, .. } => 4 * *bytes as usize,
            Codec::Yuv(Yuv::Nv12) => 2,
            Codec::Yuv(Yuv::Ayuv | Yuv::Y410 | Yuv::Yuy2 | Yuv::Uyvy | Yuv::P010 | Yuv::P016 | Yuv::Nv11) => 4,
            Codec::Yuv(Yuv::Y416 | Yuv::Y210 | Yuv::Y216 | Yuv::Uyvy16) => 8,
            _ => 16,
        }
    }

    /// Whether the decoded texels are floats rather than bytes.  ASTC
    /// decides per texture, from its blocks.
    pub fn float_output(&self) -> bool {
        matches!(
            self,
            Codec::Bc6h { .. }
                | Codec::Astc { hdr: true, .. }
                | Codec::Plain(
                    Plain::Array { num: Num::Float | Num::Fixed5, .. }
                        | Plain::B10G11R11
                        | Plain::E5B9G9R9
                        | Plain::Depth { bits: 32, .. }
                        | Plain::XrBias
                )
        )
    }

    /// Whether the format can store alpha below 1.
    pub fn has_alpha(&self, dest_alpha: bool) -> bool {
        match self {
            Codec::Bc1 { alpha } => *alpha,
            Codec::Bc2 | Codec::Bc3 | Codec::Bc7 | Codec::Etc2Rgba1 | Codec::Etc2Rgba | Codec::Astc { .. } => true,
            Codec::Pvrtc { .. } | Codec::AtcRgba => true,
            Codec::Yuv(y) => matches!(y, Yuv::Ayuv | Yuv::Y410 | Yuv::Y416),
            Codec::Plain(Plain::Array { dest, .. }) => dest.contains(&Dest::A),
            Codec::Plain(Plain::Packed { fields, .. }) => fields[3].1 > 0,
            _ => dest_alpha,
        }
    }
}

/// A format as a container names it, with how to decode it.
#[derive(Clone, Copy, Debug)]
pub(crate) struct PixelFormat {
    pub name: &'static str,
    pub codec: Codec,
    pub srgb: bool,
    pub channels: Channels,
}

/*┌─────────────────╮
| Shorthands      |
└─────────────────╯*/
//region Shorthands

use Dest::{A, B, G, L, R, X};

pub(crate) const fn fmt(name: &'static str, codec: Codec, srgb: bool, channels: Channels) -> PixelFormat {
    PixelFormat { name, codec, srgb, channels }
}

pub(crate) const fn arr(n: u8, bytes: u8, num: Num, dest: [Dest; 4]) -> Codec {
    Codec::Plain(Plain::Array { n, bytes, num, dest })
}

pub(crate) const fn depth(bytes: u8, shift: u8, bits: u8) -> Codec {
    Codec::Plain(Plain::Depth { bytes, shift, bits })
}

pub(crate) const fn packed(bytes: u8, fields: [(u8, u8); 4]) -> Codec {
    Codec::Plain(Plain::Packed { bytes, fields })
}

/// The channel count implied by where components go.
pub(crate) const fn channels_of_dest(dest: [Dest; 4], n: u8) -> Channels {
    if n == 1 {
        match dest[0] {
            R => Channels::R,
            A => Channels::A,
            _ => Channels::Rgba,
        }
    } else if n == 2 && matches!(dest[0], R) && matches!(dest[1], G) {
        Channels::Rg
    } else {
        Channels::Rgba
    }
}

const RGBA: [Dest; 4] = [R, G, B, A];
const BGRA: [Dest; 4] = [B, G, R, A];

//endregion

/*┌─────────────────╮
| Vulkan formats  |
└─────────────────╯*/
//region Vulkan formats

/// The variants Vulkan lists for each whole-byte format, in its enum order.
const VARIANTS: [(&str, Num, bool); 7] = [
    ("UNORM", Num::Unorm, false),
    ("SNORM", Num::Snorm, false),
    ("USCALED", Num::Uint, false),
    ("SSCALED", Num::Sint, false),
    ("UINT", Num::Uint, false),
    ("SINT", Num::Sint, false),
    ("SRGB", Num::Unorm, true),
];

/// ASTC footprints in Vulkan's enum order.
pub(crate) const ASTC_BLOCKS: [(u8, u8); 14] = [
    (4, 4),
    (5, 4),
    (5, 5),
    (6, 5),
    (6, 6),
    (8, 5),
    (8, 6),
    (8, 8),
    (10, 5),
    (10, 6),
    (10, 8),
    (10, 10),
    (12, 10),
    (12, 12),
];

/// 3D ASTC footprints in Vulkan's (and OpenGL's) enum order.
pub(crate) const ASTC_3D_BLOCKS: [(u8, u8, u8); 10] =
    [(3, 3, 3), (4, 3, 3), (4, 4, 3), (4, 4, 4), (5, 4, 4), (5, 5, 4), (5, 5, 5), (6, 5, 5), (6, 6, 5), (6, 6, 6)];

/// Looks up a `VkFormat` value as KTX2 stores it.  Names leave off the
/// `VK_FORMAT_` prefix.
pub(crate) fn vk_format(v: u32) -> Option<PixelFormat> {
    let f = |name, codec, srgb, ch| Some(fmt(name, codec, srgb, ch));
    // Whole-byte formats come in runs of variants; name them from tables.
    let run = |base: u32, names: &'static [&'static str], n: u8, bytes: u8, dest: [Dest; 4]| -> Option<PixelFormat> {
        let i = v.checked_sub(base)? as usize;
        let name = *names.get(i)?;
        let (num, srgb) = if bytes == 1 {
            (VARIANTS[i].1, VARIANTS[i].2)
        } else {
            // 16-bit runs end in SFLOAT instead of SRGB; 32- and 64-bit runs
            // are only UINT, SINT, SFLOAT.
            match names.len() {
                7 => (if i == 6 { Num::Float } else { VARIANTS[i].1 }, false),
                _ => ([Num::Uint, Num::Sint, Num::Float][i], false),
            }
        };
        Some(fmt(name, arr(n, bytes, num, dest), srgb, channels_of_dest(dest, n)))
    };
    let ch = Channels::Rgba;
    match v {
        1 => f("R4G4_UNORM_PACK8", packed(1, [(4, 4), (0, 4), (0, 0), (0, 0)]), false, Channels::Rg),
        2 => f("R4G4B4A4_UNORM_PACK16", packed(2, [(12, 4), (8, 4), (4, 4), (0, 4)]), false, ch),
        3 => f("B4G4R4A4_UNORM_PACK16", packed(2, [(4, 4), (8, 4), (12, 4), (0, 4)]), false, ch),
        4 => f("R5G6B5_UNORM_PACK16", packed(2, [(11, 5), (5, 6), (0, 5), (0, 0)]), false, ch),
        5 => f("B5G6R5_UNORM_PACK16", packed(2, [(0, 5), (5, 6), (11, 5), (0, 0)]), false, ch),
        6 => f("R5G5B5A1_UNORM_PACK16", packed(2, [(11, 5), (6, 5), (1, 5), (0, 1)]), false, ch),
        7 => f("B5G5R5A1_UNORM_PACK16", packed(2, [(1, 5), (6, 5), (11, 5), (0, 1)]), false, ch),
        8 => f("A1R5G5B5_UNORM_PACK16", packed(2, [(10, 5), (5, 5), (0, 5), (15, 1)]), false, ch),
        9..=15 => run(9, &R8, 1, 1, [R, X, X, X]),
        16..=22 => run(16, &RG8, 2, 1, [R, G, X, X]),
        23..=29 => run(23, &RGB8, 3, 1, [R, G, B, X]),
        30..=36 => run(30, &BGR8, 3, 1, [B, G, R, X]),
        37..=43 => run(37, &RGBA8, 4, 1, RGBA),
        44..=50 => run(44, &BGRA8, 4, 1, BGRA),
        // A8B8G8R8_*_PACK32 is R8G8B8A8 in little-endian memory.
        51..=57 => run(51, &ABGR8, 4, 1, RGBA),
        58..=63 => f(A2R10[(v - 58) as usize], packed(4, [(20, 10), (10, 10), (0, 10), (30, 2)]), false, ch),
        64..=69 => f(A2B10[(v - 64) as usize], packed(4, [(0, 10), (10, 10), (20, 10), (30, 2)]), false, ch),
        70..=76 => run(70, &R16, 1, 2, [R, X, X, X]),
        77..=83 => run(77, &RG16, 2, 2, [R, G, X, X]),
        84..=90 => run(84, &RGB16, 3, 2, [R, G, B, X]),
        91..=97 => run(91, &RGBA16, 4, 2, RGBA),
        98..=100 => run(98, &R32, 1, 4, [R, X, X, X]),
        101..=103 => run(101, &RG32, 2, 4, [R, G, X, X]),
        104..=106 => run(104, &RGB32, 3, 4, [R, G, B, X]),
        107..=109 => run(107, &RGBA32, 4, 4, RGBA),
        110..=112 => run(110, &R64, 1, 8, [R, X, X, X]),
        113..=115 => run(113, &RG64, 2, 8, [R, G, X, X]),
        116..=118 => run(116, &RGB64, 3, 8, [R, G, B, X]),
        119..=121 => run(119, &RGBA64, 4, 8, RGBA),
        122 => f("B10G11R11_UFLOAT_PACK32", Codec::Plain(Plain::B10G11R11), false, ch),
        123 => f("E5B9G9R9_UFLOAT_PACK32", Codec::Plain(Plain::E5B9G9R9), false, ch),
        124 => f("D16_UNORM", arr(1, 2, Num::Unorm, [L, X, X, X]), false, ch),
        125 => f("X8_D24_UNORM_PACK32", depth(4, 0, 24), false, ch),
        126 => f("D32_SFLOAT", arr(1, 4, Num::Float, [L, X, X, X]), false, ch),
        127 => f("S8_UINT", arr(1, 1, Num::Uint, [L, X, X, X]), false, ch),
        // Combined depth and stencil, as KTX2 lays them out; the container
        // corrects the depth bits from its data format descriptor.
        128 => f("D16_UNORM_S8_UINT", depth(4, 0, 16), false, ch),
        129 => f("D24_UNORM_S8_UINT", depth(4, 8, 24), false, ch),
        130 => f("D32_SFLOAT_S8_UINT", depth(8, 0, 32), false, ch),
        131 => f("BC1_RGB_UNORM_BLOCK", Codec::Bc1 { alpha: false }, false, ch),
        132 => f("BC1_RGB_SRGB_BLOCK", Codec::Bc1 { alpha: false }, true, ch),
        133 => f("BC1_RGBA_UNORM_BLOCK", Codec::Bc1 { alpha: true }, false, ch),
        134 => f("BC1_RGBA_SRGB_BLOCK", Codec::Bc1 { alpha: true }, true, ch),
        135 => f("BC2_UNORM_BLOCK", Codec::Bc2, false, ch),
        136 => f("BC2_SRGB_BLOCK", Codec::Bc2, true, ch),
        137 => f("BC3_UNORM_BLOCK", Codec::Bc3, false, ch),
        138 => f("BC3_SRGB_BLOCK", Codec::Bc3, true, ch),
        139 => f("BC4_UNORM_BLOCK", Codec::Bc4 { signed: false }, false, Channels::R),
        140 => f("BC4_SNORM_BLOCK", Codec::Bc4 { signed: true }, false, Channels::R),
        141 => f("BC5_UNORM_BLOCK", Codec::Bc5 { signed: false }, false, Channels::Rg),
        142 => f("BC5_SNORM_BLOCK", Codec::Bc5 { signed: true }, false, Channels::Rg),
        143 => f("BC6H_UFLOAT_BLOCK", Codec::Bc6h { signed: false }, false, ch),
        144 => f("BC6H_SFLOAT_BLOCK", Codec::Bc6h { signed: true }, false, ch),
        145 => f("BC7_UNORM_BLOCK", Codec::Bc7, false, ch),
        146 => f("BC7_SRGB_BLOCK", Codec::Bc7, true, ch),
        147 => f("ETC2_R8G8B8_UNORM_BLOCK", Codec::Etc2Rgb, false, ch),
        148 => f("ETC2_R8G8B8_SRGB_BLOCK", Codec::Etc2Rgb, true, ch),
        149 => f("ETC2_R8G8B8A1_UNORM_BLOCK", Codec::Etc2Rgba1, false, ch),
        150 => f("ETC2_R8G8B8A1_SRGB_BLOCK", Codec::Etc2Rgba1, true, ch),
        151 => f("ETC2_R8G8B8A8_UNORM_BLOCK", Codec::Etc2Rgba, false, ch),
        152 => f("ETC2_R8G8B8A8_SRGB_BLOCK", Codec::Etc2Rgba, true, ch),
        153 => f("EAC_R11_UNORM_BLOCK", Codec::EacR { signed: false }, false, Channels::R),
        154 => f("EAC_R11_SNORM_BLOCK", Codec::EacR { signed: true }, false, Channels::R),
        155 => f("EAC_R11G11_UNORM_BLOCK", Codec::EacRg { signed: false }, false, Channels::Rg),
        156 => f("EAC_R11G11_SNORM_BLOCK", Codec::EacRg { signed: true }, false, Channels::Rg),
        157..=184 => {
            let i = (v - 157) as usize;
            let (w, h) = ASTC_BLOCKS[i / 2];
            f(ASTC_LDR_NAMES[i], Codec::Astc { w, h, d: 1, hdr: false }, i % 2 == 1, ch)
        }
        1000066000..=1000066013 => {
            let i = (v - 1000066000) as usize;
            let (w, h) = ASTC_BLOCKS[i];
            f(ASTC_HDR_NAMES[i], Codec::Astc { w, h, d: 1, hdr: true }, false, ch)
        }
        // VK_EXT_texture_compression_astc_3d: UNORM, SRGB and SFLOAT for
        // each footprint.
        1000288000..=1000288029 => {
            let i = (v - 1000288000) as usize;
            let (w, h, d) = ASTC_3D_BLOCKS[i / 3];
            let codec = Codec::Astc { w, h, d, hdr: i % 3 == 2 };
            f(ASTC_3D_NAMES[i / 3][i % 3], codec, i % 3 == 1, ch)
        }
        1000054000..=1000054007 => {
            let i = (v - 1000054000) as usize;
            // PVRTC2 uses a different block layout that no decoder here reads.
            if i % 4 >= 2 {
                return None;
            }
            f(PVRTC_NAMES[i], Codec::Pvrtc { bpp: if i.is_multiple_of(2) { 2 } else { 4 } }, i >= 4, ch)
        }
        1000156000 => f("G8B8G8R8_422_UNORM", Codec::Pairs422 { bytes: 1, g_first: true }, false, ch),
        1000156001 => f("B8G8R8G8_422_UNORM", Codec::Pairs422 { bytes: 1, g_first: false }, false, ch),
        1000156010 => {
            f("G10X6B10X6G10X6R10X6_422_UNORM_4PACK16", Codec::Pairs422 { bytes: 2, g_first: true }, false, ch)
        }
        1000156011 => {
            f("B10X6G10X6R10X6G10X6_422_UNORM_4PACK16", Codec::Pairs422 { bytes: 2, g_first: false }, false, ch)
        }
        1000156020 => {
            f("G12X4B12X4G12X4R12X4_422_UNORM_4PACK16", Codec::Pairs422 { bytes: 2, g_first: true }, false, ch)
        }
        1000156021 => {
            f("B12X4G12X4R12X4G12X4_422_UNORM_4PACK16", Codec::Pairs422 { bytes: 2, g_first: false }, false, ch)
        }
        1000156027 => f("G16B16G16R16_422_UNORM", Codec::Pairs422 { bytes: 2, g_first: true }, false, ch),
        1000156028 => f("B16G16R16G16_422_UNORM", Codec::Pairs422 { bytes: 2, g_first: false }, false, ch),
        // The 10- and 12-bit formats keep their bits at the top of 16, so
        // they read as 16-bit values.
        1000156007 => f("R10X6_UNORM_PACK16", arr(1, 2, Num::Unorm, [R, X, X, X]), false, Channels::R),
        1000156008 => f("R10X6G10X6_UNORM_2PACK16", arr(2, 2, Num::Unorm, [R, G, X, X]), false, Channels::Rg),
        1000156009 => f("R10X6G10X6B10X6A10X6_UNORM_4PACK16", arr(4, 2, Num::Unorm, RGBA), false, ch),
        1000156017 => f("R12X4_UNORM_PACK16", arr(1, 2, Num::Unorm, [R, X, X, X]), false, Channels::R),
        1000156018 => f("R12X4G12X4_UNORM_2PACK16", arr(2, 2, Num::Unorm, [R, G, X, X]), false, Channels::Rg),
        1000156019 => f("R12X4G12X4B12X4A12X4_UNORM_4PACK16", arr(4, 2, Num::Unorm, RGBA), false, ch),
        1000464000 => f("R16G16_S10_5_NV", arr(2, 2, Num::Fixed5, [R, G, X, X]), false, Channels::Rg),
        1000340000 => f("A4R4G4B4_UNORM_PACK16", packed(2, [(8, 4), (4, 4), (0, 4), (12, 4)]), false, ch),
        1000340001 => f("A4B4G4R4_UNORM_PACK16", packed(2, [(0, 4), (4, 4), (8, 4), (12, 4)]), false, ch),
        1000470000 => f("A1B5G5R5_UNORM_PACK16", packed(2, [(0, 5), (5, 5), (10, 5), (15, 1)]), false, ch),
        1000470001 => f("A8_UNORM", arr(1, 1, Num::Unorm, [A, X, X, X]), false, Channels::A),
        _ => None,
    }
}

const R8: [&str; 7] = ["R8_UNORM", "R8_SNORM", "R8_USCALED", "R8_SSCALED", "R8_UINT", "R8_SINT", "R8_SRGB"];
const RG8: [&str; 7] =
    ["R8G8_UNORM", "R8G8_SNORM", "R8G8_USCALED", "R8G8_SSCALED", "R8G8_UINT", "R8G8_SINT", "R8G8_SRGB"];
const RGB8: [&str; 7] =
    ["R8G8B8_UNORM", "R8G8B8_SNORM", "R8G8B8_USCALED", "R8G8B8_SSCALED", "R8G8B8_UINT", "R8G8B8_SINT", "R8G8B8_SRGB"];
const BGR8: [&str; 7] =
    ["B8G8R8_UNORM", "B8G8R8_SNORM", "B8G8R8_USCALED", "B8G8R8_SSCALED", "B8G8R8_UINT", "B8G8R8_SINT", "B8G8R8_SRGB"];
const RGBA8: [&str; 7] = [
    "R8G8B8A8_UNORM",
    "R8G8B8A8_SNORM",
    "R8G8B8A8_USCALED",
    "R8G8B8A8_SSCALED",
    "R8G8B8A8_UINT",
    "R8G8B8A8_SINT",
    "R8G8B8A8_SRGB",
];
const BGRA8: [&str; 7] = [
    "B8G8R8A8_UNORM",
    "B8G8R8A8_SNORM",
    "B8G8R8A8_USCALED",
    "B8G8R8A8_SSCALED",
    "B8G8R8A8_UINT",
    "B8G8R8A8_SINT",
    "B8G8R8A8_SRGB",
];
const ABGR8: [&str; 7] = [
    "A8B8G8R8_UNORM_PACK32",
    "A8B8G8R8_SNORM_PACK32",
    "A8B8G8R8_USCALED_PACK32",
    "A8B8G8R8_SSCALED_PACK32",
    "A8B8G8R8_UINT_PACK32",
    "A8B8G8R8_SINT_PACK32",
    "A8B8G8R8_SRGB_PACK32",
];
const A2R10: [&str; 6] = [
    "A2R10G10B10_UNORM_PACK32",
    "A2R10G10B10_SNORM_PACK32",
    "A2R10G10B10_USCALED_PACK32",
    "A2R10G10B10_SSCALED_PACK32",
    "A2R10G10B10_UINT_PACK32",
    "A2R10G10B10_SINT_PACK32",
];
const A2B10: [&str; 6] = [
    "A2B10G10R10_UNORM_PACK32",
    "A2B10G10R10_SNORM_PACK32",
    "A2B10G10R10_USCALED_PACK32",
    "A2B10G10R10_SSCALED_PACK32",
    "A2B10G10R10_UINT_PACK32",
    "A2B10G10R10_SINT_PACK32",
];
const R16: [&str; 7] = ["R16_UNORM", "R16_SNORM", "R16_USCALED", "R16_SSCALED", "R16_UINT", "R16_SINT", "R16_SFLOAT"];
const RG16: [&str; 7] =
    ["R16G16_UNORM", "R16G16_SNORM", "R16G16_USCALED", "R16G16_SSCALED", "R16G16_UINT", "R16G16_SINT", "R16G16_SFLOAT"];
const RGB16: [&str; 7] = [
    "R16G16B16_UNORM",
    "R16G16B16_SNORM",
    "R16G16B16_USCALED",
    "R16G16B16_SSCALED",
    "R16G16B16_UINT",
    "R16G16B16_SINT",
    "R16G16B16_SFLOAT",
];
const RGBA16: [&str; 7] = [
    "R16G16B16A16_UNORM",
    "R16G16B16A16_SNORM",
    "R16G16B16A16_USCALED",
    "R16G16B16A16_SSCALED",
    "R16G16B16A16_UINT",
    "R16G16B16A16_SINT",
    "R16G16B16A16_SFLOAT",
];
const R32: [&str; 3] = ["R32_UINT", "R32_SINT", "R32_SFLOAT"];
const RG32: [&str; 3] = ["R32G32_UINT", "R32G32_SINT", "R32G32_SFLOAT"];
const RGB32: [&str; 3] = ["R32G32B32_UINT", "R32G32B32_SINT", "R32G32B32_SFLOAT"];
const RGBA32: [&str; 3] = ["R32G32B32A32_UINT", "R32G32B32A32_SINT", "R32G32B32A32_SFLOAT"];
const R64: [&str; 3] = ["R64_UINT", "R64_SINT", "R64_SFLOAT"];
const RG64: [&str; 3] = ["R64G64_UINT", "R64G64_SINT", "R64G64_SFLOAT"];
const RGB64: [&str; 3] = ["R64G64B64_UINT", "R64G64B64_SINT", "R64G64B64_SFLOAT"];
const RGBA64: [&str; 3] = ["R64G64B64A64_UINT", "R64G64B64A64_SINT", "R64G64B64A64_SFLOAT"];
const ASTC_LDR_NAMES: [&str; 28] = [
    "ASTC_4x4_UNORM_BLOCK",
    "ASTC_4x4_SRGB_BLOCK",
    "ASTC_5x4_UNORM_BLOCK",
    "ASTC_5x4_SRGB_BLOCK",
    "ASTC_5x5_UNORM_BLOCK",
    "ASTC_5x5_SRGB_BLOCK",
    "ASTC_6x5_UNORM_BLOCK",
    "ASTC_6x5_SRGB_BLOCK",
    "ASTC_6x6_UNORM_BLOCK",
    "ASTC_6x6_SRGB_BLOCK",
    "ASTC_8x5_UNORM_BLOCK",
    "ASTC_8x5_SRGB_BLOCK",
    "ASTC_8x6_UNORM_BLOCK",
    "ASTC_8x6_SRGB_BLOCK",
    "ASTC_8x8_UNORM_BLOCK",
    "ASTC_8x8_SRGB_BLOCK",
    "ASTC_10x5_UNORM_BLOCK",
    "ASTC_10x5_SRGB_BLOCK",
    "ASTC_10x6_UNORM_BLOCK",
    "ASTC_10x6_SRGB_BLOCK",
    "ASTC_10x8_UNORM_BLOCK",
    "ASTC_10x8_SRGB_BLOCK",
    "ASTC_10x10_UNORM_BLOCK",
    "ASTC_10x10_SRGB_BLOCK",
    "ASTC_12x10_UNORM_BLOCK",
    "ASTC_12x10_SRGB_BLOCK",
    "ASTC_12x12_UNORM_BLOCK",
    "ASTC_12x12_SRGB_BLOCK",
];
const ASTC_HDR_NAMES: [&str; 14] = [
    "ASTC_4x4_SFLOAT_BLOCK",
    "ASTC_5x4_SFLOAT_BLOCK",
    "ASTC_5x5_SFLOAT_BLOCK",
    "ASTC_6x5_SFLOAT_BLOCK",
    "ASTC_6x6_SFLOAT_BLOCK",
    "ASTC_8x5_SFLOAT_BLOCK",
    "ASTC_8x6_SFLOAT_BLOCK",
    "ASTC_8x8_SFLOAT_BLOCK",
    "ASTC_10x5_SFLOAT_BLOCK",
    "ASTC_10x6_SFLOAT_BLOCK",
    "ASTC_10x8_SFLOAT_BLOCK",
    "ASTC_10x10_SFLOAT_BLOCK",
    "ASTC_12x10_SFLOAT_BLOCK",
    "ASTC_12x12_SFLOAT_BLOCK",
];
const ASTC_3D_NAMES: [[&str; 3]; 10] = [
    ["ASTC_3x3x3_UNORM_BLOCK_EXT", "ASTC_3x3x3_SRGB_BLOCK_EXT", "ASTC_3x3x3_SFLOAT_BLOCK_EXT"],
    ["ASTC_4x3x3_UNORM_BLOCK_EXT", "ASTC_4x3x3_SRGB_BLOCK_EXT", "ASTC_4x3x3_SFLOAT_BLOCK_EXT"],
    ["ASTC_4x4x3_UNORM_BLOCK_EXT", "ASTC_4x4x3_SRGB_BLOCK_EXT", "ASTC_4x4x3_SFLOAT_BLOCK_EXT"],
    ["ASTC_4x4x4_UNORM_BLOCK_EXT", "ASTC_4x4x4_SRGB_BLOCK_EXT", "ASTC_4x4x4_SFLOAT_BLOCK_EXT"],
    ["ASTC_5x4x4_UNORM_BLOCK_EXT", "ASTC_5x4x4_SRGB_BLOCK_EXT", "ASTC_5x4x4_SFLOAT_BLOCK_EXT"],
    ["ASTC_5x5x4_UNORM_BLOCK_EXT", "ASTC_5x5x4_SRGB_BLOCK_EXT", "ASTC_5x5x4_SFLOAT_BLOCK_EXT"],
    ["ASTC_5x5x5_UNORM_BLOCK_EXT", "ASTC_5x5x5_SRGB_BLOCK_EXT", "ASTC_5x5x5_SFLOAT_BLOCK_EXT"],
    ["ASTC_6x5x5_UNORM_BLOCK_EXT", "ASTC_6x5x5_SRGB_BLOCK_EXT", "ASTC_6x5x5_SFLOAT_BLOCK_EXT"],
    ["ASTC_6x6x5_UNORM_BLOCK_EXT", "ASTC_6x6x5_SRGB_BLOCK_EXT", "ASTC_6x6x5_SFLOAT_BLOCK_EXT"],
    ["ASTC_6x6x6_UNORM_BLOCK_EXT", "ASTC_6x6x6_SRGB_BLOCK_EXT", "ASTC_6x6x6_SFLOAT_BLOCK_EXT"],
];
const PVRTC_NAMES: [&str; 8] = [
    "PVRTC1_2BPP_UNORM_BLOCK_IMG",
    "PVRTC1_4BPP_UNORM_BLOCK_IMG",
    "PVRTC2_2BPP_UNORM_BLOCK_IMG",
    "PVRTC2_4BPP_UNORM_BLOCK_IMG",
    "PVRTC1_2BPP_SRGB_BLOCK_IMG",
    "PVRTC1_4BPP_SRGB_BLOCK_IMG",
    "PVRTC2_2BPP_SRGB_BLOCK_IMG",
    "PVRTC2_4BPP_SRGB_BLOCK_IMG",
];

//endregion

/// The suffix that records a format's channels in `SourceInfo::format`,
/// where `thumbnail` reads it back.
pub(crate) fn channel_tag(c: Channels) -> &'static str {
    match c {
        Channels::R => " (R)",
        Channels::Rg => " (RG)",
        Channels::A => " (A)",
        Channels::Data => " (data)",
        Channels::Stretch => " (stretch)",
        Channels::Rgba => "",
    }
}

/// The channels a `SourceInfo::format` name records, from its suffix.
pub(crate) fn channels_by_name(name: &str) -> Option<Channels> {
    [Channels::R, Channels::Rg, Channels::A, Channels::Data, Channels::Stretch]
        .into_iter()
        .find(|&c| name.ends_with(channel_tag(c)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vk_names_line_up() {
        assert_eq!(vk_format(37).unwrap().name, "R8G8B8A8_UNORM");
        assert!(vk_format(43).unwrap().srgb);
        assert_eq!(vk_format(76).unwrap().name, "R16_SFLOAT");
        assert_eq!(vk_format(76).unwrap().codec, arr(1, 2, Num::Float, [R, X, X, X]));
        assert_eq!(vk_format(109).unwrap().name, "R32G32B32A32_SFLOAT");
        assert_eq!(vk_format(146).unwrap().name, "BC7_SRGB_BLOCK");
        assert_eq!(vk_format(184).unwrap().name, "ASTC_12x12_SRGB_BLOCK");
        assert_eq!(vk_format(165).unwrap().codec, Codec::Astc { w: 6, h: 6, d: 1, hdr: false });
        assert_eq!(vk_format(1000066004).unwrap().codec, Codec::Astc { w: 6, h: 6, d: 1, hdr: true });
        assert_eq!(vk_format(1000288000).unwrap().name, "ASTC_3x3x3_UNORM_BLOCK_EXT");
        assert_eq!(vk_format(1000288010).unwrap().codec, Codec::Astc { w: 4, h: 4, d: 4, hdr: false });
        assert!(vk_format(1000288013).unwrap().srgb);
        assert_eq!(vk_format(1000288029).unwrap().codec, Codec::Astc { w: 6, h: 6, d: 6, hdr: true });
        assert!(vk_format(1000288030).is_none());
        assert_eq!(vk_format(141).unwrap().channels, Channels::Rg);
        assert_eq!(vk_format(9).unwrap().channels, Channels::R);
        assert_eq!(vk_format(30).unwrap().channels, Channels::Rgba);
        assert!(vk_format(0).is_none());
        assert!(vk_format(185).is_none());
    }
}
