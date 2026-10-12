//! What every reader here shares: size checks that run before anything is
//! allocated, a bounded byte cursor, block-compressed texture decoding and
//! the wrapping of a result in a `Texture`.

use slerp_scene::{ColorSpace, LoadError, Texture};

/// The longest side a texture may have.
pub const MAX_SIDE: u32 = 16384;

/// The most texels a decoded texture may have (64 million, 256 MB of RGBA).
pub const MAX_TEXELS: u64 = 1 << 26;

/// Checks a size from a file header before allocating for it.
pub fn check_size(width: u32, height: u32) -> Result<(), LoadError> {
    if width == 0 || height == 0 || width > MAX_SIDE || height > MAX_SIDE || width as u64 * height as u64 > MAX_TEXELS {
        return Err(LoadError::invalid(format!("texture size {width}x{height} is out of range")));
    }
    Ok(())
}

/// Refuses a level whose data is far shorter than its size needs, before the
/// picture for it is allocated: a header can claim 8192 x 8192 texels in a
/// file of 100 bytes.  A file at least an eighth complete (a download that
/// stopped partway) still gives a partial picture.
pub fn expect_data(have: usize, need: usize) -> Result<(), LoadError> {
    if have.saturating_mul(8) < need {
        return Err(LoadError::invalid("the texture data is far shorter than its size needs"));
    }
    Ok(())
}

/// A little-endian reader over a byte slice that fails instead of panicking.
pub struct Cursor<'a> {
    pub bytes: &'a [u8],
    pub pos: usize,
}

impl<'a> Cursor<'a> {
    pub fn new(bytes: &'a [u8]) -> Self {
        Cursor { bytes, pos: 0 }
    }

    pub fn take(&mut self, n: usize) -> Result<&'a [u8], LoadError> {
        let end = self.pos.checked_add(n).filter(|&e| e <= self.bytes.len());
        let end = end.ok_or_else(|| LoadError::invalid("the file is cut off"))?;
        let s = &self.bytes[self.pos..end];
        self.pos = end;
        Ok(s)
    }

    pub fn u8(&mut self) -> Result<u8, LoadError> {
        Ok(self.take(1)?[0])
    }

    pub fn u32(&mut self) -> Result<u32, LoadError> {
        let b = self.take(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    pub fn i32(&mut self) -> Result<i32, LoadError> {
        Ok(self.u32()? as i32)
    }
}

/// An RGBA texture from straight 8-bit pixels.
pub fn texture(width: u32, height: u32, rgba: Vec<u8>, container: &str, detail: &str) -> Texture {
    let mut t = Texture::from_rgba8(width, height, rgba, ColorSpace::Srgb);
    t.source.container = container.into();
    t.source.format = detail.into();
    t
}

/// Block-compressed formats these readers decode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Block {
    Dxt1,
    Dxt3,
    Dxt5,
    Etc1,
    Etc2Rgb,
    Etc2Rgba1,
    Etc2Rgba8,
    /// PVRTC1 at 2 or 4 bits per pixel.
    Pvrtc {
        two_bpp: bool,
    },
}

impl Block {
    pub fn name(self) -> &'static str {
        match self {
            Block::Dxt1 => "DXT1",
            Block::Dxt3 => "DXT3",
            Block::Dxt5 => "DXT5",
            Block::Etc1 => "ETC1",
            Block::Etc2Rgb => "ETC2 RGB",
            Block::Etc2Rgba1 => "ETC2 RGB+A1",
            Block::Etc2Rgba8 => "ETC2 RGBA",
            Block::Pvrtc { two_bpp: true } => "PVRTC 2bpp",
            Block::Pvrtc { two_bpp: false } => "PVRTC 4bpp",
        }
    }

    /// Bytes of one 4x4 block (not meaningful for PVRTC, which codes 4x4 or
    /// 8x4 texels in 8 bytes).
    fn block_bytes(self) -> usize {
        match self {
            Block::Dxt1 | Block::Etc1 | Block::Etc2Rgb | Block::Etc2Rgba1 | Block::Pvrtc { .. } => 8,
            Block::Dxt3 | Block::Dxt5 | Block::Etc2Rgba8 => 16,
        }
    }

    /// The bytes a `width` x `height` level takes.
    pub fn level_bytes(self, width: u32, height: u32) -> usize {
        match self {
            Block::Pvrtc { two_bpp } => {
                // Whole blocks, at least 2 blocks across for 4 bpp (8x8
                // texels minimum) and 16x8 for 2 bpp.
                let (bw, bh) = if two_bpp { (8usize, 4usize) } else { (4, 4) };
                let w = (width as usize).next_power_of_two().max(bw * 2);
                let h = (height as usize).next_power_of_two().max(bh * 2);
                w / bw * (h / bh) * 8
            }
            _ => (width as usize).div_ceil(4) * (height as usize).div_ceil(4) * self.block_bytes(),
        }
    }
}

/// Decodes one level of block-compressed data to straight RGBA.  Missing
/// data (a cut-off file) leaves the rest transparent black.
pub fn decode_blocks(format: Block, data: &[u8], width: u32, height: u32) -> Vec<u8> {
    decode_blocks_ordered(format, data, width, height, false)
}

/// [`decode_blocks`] with the choice of storing the blocks down each column
/// before moving right instead of along each row (some PKM writers do).
pub fn decode_blocks_ordered(format: Block, data: &[u8], width: u32, height: u32, column_major: bool) -> Vec<u8> {
    let (w, h) = (width as usize, height as usize);
    let mut out = vec![0u8; w * h * 4];
    // No blocks to place (and a column or row count of zero to divide by).
    if w == 0 || h == 0 {
        return out;
    }
    if let Block::Pvrtc { two_bpp } = format {
        // PVRTC wants a power-of-two image and decodes it whole.
        let (pw, ph) = (w.next_power_of_two().max(if two_bpp { 16 } else { 8 }), h.next_power_of_two().max(8));
        let mut img = vec![0u32; pw * ph];
        let need = format.level_bytes(width, height);
        let mut padded;
        let src = if data.len() >= need {
            data
        } else {
            padded = data.to_vec();
            padded.resize(need, 0);
            &padded
        };
        if texture2ddecoder::decode_pvrtc(src, pw, ph, &mut img, two_bpp).is_ok() {
            for y in 0..h {
                for x in 0..w {
                    let [b, g, r, a] = img[y * pw + x].to_le_bytes();
                    out[(y * w + x) * 4..(y * w + x) * 4 + 4].copy_from_slice(&[r, g, b, a]);
                }
            }
        }
        return out;
    }
    let bb = format.block_bytes();
    let (blocks_x, blocks_y) = (w.div_ceil(4), h.div_ceil(4));
    let mut tile = [0u8; 64];
    let mut argb = [0u32; 16];
    for (i, block) in data.chunks_exact(bb).enumerate() {
        let (bx, by) = if column_major { (i / blocks_y, i % blocks_y) } else { (i % blocks_x, i / blocks_x) };
        if by >= blocks_y || bx >= blocks_x {
            break;
        }
        match format {
            Block::Dxt1 => bcdec_rs::bc1(block, &mut tile, 16),
            Block::Dxt3 => bcdec_rs::bc2(block, &mut tile, 16),
            Block::Dxt5 => bcdec_rs::bc3(block, &mut tile, 16),
            Block::Etc1 | Block::Etc2Rgb | Block::Etc2Rgba1 | Block::Etc2Rgba8 => {
                match format {
                    Block::Etc1 => texture2ddecoder::decode_etc1_block(block, &mut argb),
                    Block::Etc2Rgb => texture2ddecoder::decode_etc2_rgb_block(block, &mut argb),
                    Block::Etc2Rgba1 => texture2ddecoder::decode_etc2_rgba1_block(block, &mut argb),
                    _ => texture2ddecoder::decode_etc2_rgba8_block(block, &mut argb),
                }
                for (t, &v) in tile.chunks_exact_mut(4).zip(&argb) {
                    let [b, g, r, a] = v.to_le_bytes();
                    t.copy_from_slice(&[r, g, b, a]);
                }
            }
            Block::Pvrtc { .. } => unreachable!(),
        }
        for ty in 0..4 {
            let y = by * 4 + ty;
            if y >= h {
                break;
            }
            for tx in 0..4 {
                let x = bx * 4 + tx;
                if x < w {
                    let s = (ty * 4 + tx) * 4;
                    out[(y * w + x) * 4..(y * w + x) * 4 + 4].copy_from_slice(&tile[s..s + 4]);
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_are_checked() {
        assert!(check_size(0, 5).is_err());
        assert!(check_size(16385, 1).is_err());
        assert!(check_size(16384, 16384).is_err());
        assert!(check_size(4096, 4096).is_ok());
    }

    #[test]
    fn cursor_never_panics() {
        let mut c = Cursor::new(&[1, 2, 3, 4, 5, 6]);
        assert_eq!(c.u32().unwrap(), 0x04030201);
        assert!(c.u32().is_err());
        assert_eq!(c.u8().unwrap(), 5);
        assert!(c.take(usize::MAX).is_err());
    }

    #[test]
    fn dxt1_block_decodes() {
        // Colour 0 = pure red (0xF800), colour 1 = black, all indices 0.
        let block = [0x00, 0xf8, 0x00, 0x00, 0, 0, 0, 0];
        let out = decode_blocks(Block::Dxt1, &block, 4, 4);
        assert_eq!(&out[..4], &[255, 0, 0, 255]);
        assert_eq!(&out[60..64], &[255, 0, 0, 255]);
    }

    #[test]
    fn short_data_leaves_transparent_black() {
        let out = decode_blocks(Block::Dxt5, &[0u8; 5], 8, 8);
        assert_eq!(out.len(), 8 * 8 * 4);
        assert!(out.iter().all(|&b| b == 0));
        let out = decode_blocks(Block::Pvrtc { two_bpp: false }, &[0u8; 3], 8, 8);
        assert_eq!(out.len(), 256);
    }

    #[test]
    fn level_sizes() {
        assert_eq!(Block::Dxt1.level_bytes(8, 8), 32);
        assert_eq!(Block::Dxt5.level_bytes(5, 5), 64);
        assert_eq!(Block::Pvrtc { two_bpp: false }.level_bytes(8, 8), 32);
        assert_eq!(Block::Pvrtc { two_bpp: true }.level_bytes(16, 8), 32);
    }
}
