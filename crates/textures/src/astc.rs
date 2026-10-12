//! Raw `.astc` files as ARM's astcenc writes them: a 16-byte header giving
//! the block footprint and image size, then the blocks.  The header doesn't
//! say whether the data is sRGB or HDR; HDR is found from the blocks.  3D
//! footprints (a block depth above 1) make volume textures, stored a slice
//! of blocks at a time.

use crate::assemble::{Layout, assemble};
use crate::blocks::decode_astc;
use crate::formats::{ASTC_3D_BLOCKS, ASTC_BLOCKS, Codec, PixelFormat};
use crate::thumb::Channels;
use crate::util::{invalid, slice};
use crate::{ColorSpace, DecodeOptions, SourceInfo, Texture, TextureError, TextureKind};

pub(crate) fn decode(bytes: &[u8], opts: &DecodeOptions) -> Result<Texture, TextureError> {
    let h = bytes.get(..16).ok_or_else(|| invalid("ASTC: truncated header"))?;
    let (bw, bh, bd) = (h[4], h[5], h[6]);
    let dim = |at: usize| u32::from_le_bytes([h[at], h[at + 1], h[at + 2], 0]);
    let (w, ht, d) = (dim(7), dim(10), dim(13).max(1));
    let name: &'static str = if bd > 1 {
        let i = ASTC_3D_BLOCKS.iter().position(|&b| b == (bw, bh, bd));
        ASTC_3D_NAMES[i.ok_or_else(|| invalid(format!("ASTC: bad block size {bw}x{bh}x{bd}")))?]
    } else {
        let i = ASTC_BLOCKS.iter().position(|&b| b == (bw, bh));
        ASTC_NAMES[i.ok_or_else(|| invalid(format!("ASTC: bad block size {bw}x{bh}")))?]
    };
    crate::util::check_layout(w, ht, d, 1)?;
    let bd = bd.max(1);
    let codec = Codec::Astc { w: bw, h: bh, d: bd, hdr: false };
    let fmt = PixelFormat { name, codec, srgb: false, channels: Channels::Rgba };
    let layout = Layout {
        kind: if d > 1 { TextureKind::D3 } else { TextureKind::D2 },
        width: w,
        height: ht,
        depth: d,
        layers: 1,
        faces: 1,
        levels: 1,
        color_space: ColorSpace::Unspecified,
        flip_y: false,
        flip_x: false,
        alpha: true,
        source: SourceInfo {
            container: "ASTC".into(),
            format: name.into(),
            file_bytes: bytes.len() as u64,
            ..Default::default()
        },
    };
    let row = w.div_ceil(bw as u32) as usize * 16;
    let size = row as u64 * ht.div_ceil(bh as u32) as u64 * d.div_ceil(bd as u32) as u64;
    let data = slice(bytes, 16, size, "ASTC blocks")?;
    let mut tex = assemble(layout, opts, |_, _| Ok(vec![decode_astc(&fmt, w, ht, d, data, row)?]))?;
    if tex.is_hdr() {
        tex.color_space = ColorSpace::Linear;
        tex.source.format = format!("{name} HDR");
    }
    Ok(tex)
}

const ASTC_3D_NAMES: [&str; 10] = [
    "ASTC 3x3x3",
    "ASTC 4x3x3",
    "ASTC 4x4x3",
    "ASTC 4x4x4",
    "ASTC 5x4x4",
    "ASTC 5x5x4",
    "ASTC 5x5x5",
    "ASTC 6x5x5",
    "ASTC 6x6x5",
    "ASTC 6x6x6",
];

const ASTC_NAMES: [&str; 14] = [
    "ASTC 4x4",
    "ASTC 5x4",
    "ASTC 5x5",
    "ASTC 6x5",
    "ASTC 6x6",
    "ASTC 8x5",
    "ASTC 8x6",
    "ASTC 8x8",
    "ASTC 10x5",
    "ASTC 10x6",
    "ASTC 10x8",
    "ASTC 10x10",
    "ASTC 12x10",
    "ASTC 12x12",
];
