//! KTX 2.0: Khronos' container for GPU textures.  Plain Vulkan formats are
//! decoded here, level by level, after undoing zstd or zlib
//! supercompression; Basis Universal payloads (BasisLZ/ETC1S, UASTC, XUASTC
//! and the HDR codecs, all stored with format VK_FORMAT_UNDEFINED) go to
//! `basis`.

use crate::assemble::{Layout, assemble};
use crate::blocks::decode_surface;
use crate::formats::{Codec, PixelFormat, Plain, Yuv, channel_tag, vk_format};
use crate::util::{Reader, invalid, slice, unsupported};
use crate::{ColorSpace, DecodeOptions, GpuBlocks, Pixels, SourceInfo, Texture, TextureError, TextureKind};
use std::borrow::Cow;

/// The header fields that matter, plus the sections they point to.
pub(crate) struct Header<'a> {
    pub vk_format: u32,
    pub width: u32,
    pub height: u32,
    pub depth: u32,
    pub layers: u32,
    pub faces: u32,
    pub levels: u32,
    pub scheme: u32,
    pub dfd: &'a [u8],
    pub kvd: Vec<(&'a str, &'a [u8])>,
    /// Byte offset, byte length and uncompressed length of each level.
    pub index: Vec<(u64, u64, u64)>,
}

impl<'a> Header<'a> {
    pub fn parse(bytes: &'a [u8]) -> Result<Self, TextureError> {
        let mut r = Reader::at(bytes, 12);
        let vk_format = r.u32()?;
        let _type_size = r.u32()?;
        let width = r.u32()?;
        let height = r.u32()?.max(1);
        let depth = r.u32()?;
        let layers = r.u32()?;
        let faces = r.u32()?;
        let levels = r.u32()?;
        let scheme = r.u32()?;
        let (dfd_off, dfd_len) = (r.u32()?, r.u32()?);
        let (kvd_off, kvd_len) = (r.u32()?, r.u32()?);
        let (_sgd_off, _sgd_len) = (r.u64()?, r.u64()?);
        crate::util::check_layout(width, height, depth.max(1), layers.max(1))?;
        if faces != 1 && faces != 6 {
            return Err(invalid(format!("KTX2: {faces} faces")));
        }
        if faces == 6 && (width != height || depth > 0) {
            return Err(invalid("KTX2: cubemap faces must be square and 2D"));
        }
        // A level count of 0 asks the loader to generate mips; one is stored.
        let stored = levels.max(1);
        if stored > 32 {
            return Err(invalid(format!("KTX2: {levels} levels")));
        }
        let mut index = Vec::with_capacity(stored as usize);
        for _ in 0..stored {
            index.push((r.u64()?, r.u64()?, r.u64()?));
        }
        let dfd = slice(bytes, dfd_off as u64, dfd_len as u64, "KTX2 data format descriptor")?;
        let kvd_bytes = slice(bytes, kvd_off as u64, kvd_len as u64, "KTX2 key/value data")?;
        if kvd_off % 4 != 0 || !kvd_padding_fits(kvd_bytes) {
            return Err(invalid("KTX2: key/value data overruns its section"));
        }
        let kvd = parse_kvd(kvd_bytes);
        Ok(Header { vk_format, width, height, depth, layers, faces, levels: stored, scheme, dfd, kvd, index })
    }

    pub fn value(&self, key: &str) -> Option<&'a [u8]> {
        self.kvd.iter().find(|(k, _)| *k == key).map(|(_, v)| *v)
    }

    /// A text value without its terminating NUL.
    pub fn text(&self, key: &str) -> Option<&'a str> {
        self.value(key).and_then(|v| std::str::from_utf8(v.split(|&b| b == 0).next().unwrap_or(v)).ok())
    }

    /// The transfer function the data format descriptor names: sRGB is 2.
    pub fn dfd_srgb(&self) -> bool {
        self.dfd.get(14) == Some(&2)
    }

    /// The DFD's colour model, which names the Basis codec.
    pub fn dfd_model(&self) -> u8 {
        self.dfd.get(12).copied().unwrap_or(0)
    }

    /// The bit offset and length of the DFD sample with this channel id.
    pub fn dfd_sample(&self, channel: u8) -> Option<(u8, u8)> {
        let block_size = u16::from_le_bytes(self.dfd.get(10..12)?.try_into().ok()?) as usize;
        (0..block_size.saturating_sub(24) / 16).find_map(|i| {
            let s = self.dfd.get(4 + 24 + i * 16..4 + 24 + i * 16 + 4)?;
            (s[3] & 15 == channel)
                .then(|| (u16::from_le_bytes([s[0], s[1]]).min(31) as u8, s[2].saturating_add(1).min(32)))
        })
    }

    /// The channel ids of the DFD's samples (low four bits of each sample's
    /// channel type).
    pub fn dfd_channels(&self) -> Vec<u8> {
        let block_size = self.dfd.get(10..12).map(|b| u16::from_le_bytes([b[0], b[1]]) as usize).unwrap_or(0);
        let samples = block_size.saturating_sub(24) / 16;
        (0..samples).filter_map(|i| self.dfd.get(4 + 24 + i * 16 + 3).map(|c| c & 15)).collect()
    }
}

/// Whether every entry's padding stays inside the section, as the
/// specification requires.  The Basis transcoder walks the section the same
/// way and reads past its end otherwise.
fn kvd_padding_fits(data: &[u8]) -> bool {
    let mut p = 0usize;
    while data.len() - p > 4 {
        let len = u32::from_le_bytes([data[p], data[p + 1], data[p + 2], data[p + 3]]) as usize;
        p += 4;
        if len < 2 || data.len() - p < len {
            return true;
        }
        p += len;
        p += (4 - (p & 3)) & 3;
        if p > data.len() {
            return false;
        }
    }
    true
}

/// Splits the key/value section into pairs.  Malformed entries end the list
/// rather than failing the file, since nothing in it is essential.
fn parse_kvd(data: &[u8]) -> Vec<(&str, &[u8])> {
    let mut out = Vec::new();
    let mut r = Reader::new(data);
    while let Ok(len) = r.u32() {
        let Ok(entry) = r.bytes(len as usize) else { break };
        let Some(nul) = entry.iter().position(|&b| b == 0) else { break };
        let Ok(key) = std::str::from_utf8(&entry[..nul]) else { break };
        out.push((key, &entry[nul + 1..]));
        if r.skip((4 - len as usize % 4) % 4).is_err() {
            break;
        }
    }
    out
}

/// Reads `KTXorientation`, whose letters say which way x, y and z run:
/// "ru" means rows go bottom to top.
pub(crate) fn orientation(value: Option<&str>) -> (bool, bool) {
    let v = value.unwrap_or("rd").as_bytes();
    (v.first() == Some(&b'l'), v.get(1) == Some(&b'u'))
}

/// Decodes a KTX2 file, keeping Basis Universal textures in blocks of a
/// format `gpu` samples where possible (see `basis`).
pub(crate) fn decode(bytes: &[u8], opts: &DecodeOptions, gpu: GpuBlocks) -> Result<Texture, TextureError> {
    let h = Header::parse(bytes)?;
    let kind = if h.faces == 6 {
        TextureKind::Cube
    } else if h.depth > 0 {
        TextureKind::D3
    } else {
        TextureKind::D2
    };
    let (flip_x, flip_y) = orientation(h.text("KTXorientation"));
    let supercompression = match h.scheme {
        0 => None,
        1 => Some("BasisLZ".to_string()),
        2 => Some("zstd".to_string()),
        3 => Some("zlib".to_string()),
        4..=6 => Some("Basis intermediate".to_string()),
        n => Some(format!("scheme {n}")),
    };
    let source =
        SourceInfo { container: "KTX2".into(), supercompression, file_bytes: bytes.len() as u64, ..Default::default() };
    let mut layout = Layout {
        kind,
        width: h.width,
        height: h.height,
        depth: h.depth.max(1),
        layers: h.layers.max(1),
        faces: h.faces,
        levels: h.levels,
        color_space: ColorSpace::Linear,
        flip_y,
        flip_x,
        alpha: true,
        source,
    };
    if h.vk_format == 0 {
        return crate::basis::decode_ktx2(bytes, &h, layout, opts, gpu);
    }
    let mut fmt = vk_format(h.vk_format).ok_or_else(|| unsupported(format!("KTX2 VkFormat {}", h.vk_format)))?;
    if let Codec::Plain(Plain::Depth { bytes, .. }) = fmt.codec
        && let Some((shift, bits)) = h.dfd_sample(14)
    {
        fmt.codec = Codec::Plain(Plain::Depth { bytes, shift, bits });
    }
    // Vulkan's 4:2:2 formats are RGB with shared red and blue unless the
    // descriptor's colour model says YUV (KHR_DF_MODEL_YUVSDA), as video
    // files do: then green is luma, blue Cb and red Cr.  The matrix isn't
    // recorded, so they decode as DDS video formats do (BT.601 studio
    // range); 10 and 12-bit samples sit at the top of 16 bits, which reads
    // the same as 16-bit.
    if let Codec::Pairs422 { bytes, g_first } = fmt.codec
        && h.dfd_model() == 2
    {
        fmt.codec = Codec::Yuv(match (bytes, g_first) {
            (1, true) => Yuv::Yuy2,
            (1, false) => Yuv::Uyvy,
            (_, true) => Yuv::Y216,
            (_, false) => Yuv::Uyvy16,
        });
    }
    if h.scheme == 1 || h.scheme > 3 {
        return Err(invalid(format!("KTX2: supercompression scheme {} with a Vulkan format", h.scheme)));
    }
    // Basis Universal's codecs whose blocks are plain ASTC say so in the
    // descriptor's colour model.
    let basis = match h.dfd_model() {
        167 => " (UASTC HDR 4x4)",
        162 if h.value("KTXwriter").is_some_and(|w| w.starts_with(b"basisu")) => " (Basis ASTC)",
        _ => "",
    };
    let ycbcr = if matches!(fmt.codec, Codec::Yuv(_)) { " (YCbCr)" } else { "" };
    layout.source.format = format!("{}{basis}{ycbcr}{}", fmt.name, channel_tag(fmt.channels));
    layout.color_space = if fmt.srgb { ColorSpace::Srgb } else { ColorSpace::Linear };
    layout.alpha = fmt.codec.has_alpha(false);
    let swizzle = Swizzle::parse(h.text("KTXswizzle"));
    let tex = assemble(layout, opts, |level, layers| {
        let surfaces = decode_level(bytes, &h, &fmt, level, layers)?;
        Ok(surfaces.into_iter().map(|s| swizzle.apply(s)).collect())
    })?;
    Ok(tex)
}

/// Decodes one level's surfaces for the first `layers` layers.
fn decode_level(
    bytes: &[u8],
    h: &Header,
    fmt: &PixelFormat,
    level: u32,
    layers: u32,
) -> Result<Vec<Pixels>, TextureError> {
    let (w, ht) = (crate::util::mip_dim(h.width, level), crate::util::mip_dim(h.height, level));
    let d = if h.depth > 0 { crate::util::mip_dim(h.depth, level) } else { 1 };
    let (gx, gy) = fmt.codec.grid(w, ht);
    let slices = d.div_ceil(fmt.codec.block_depth());
    let image = (gx as u64 * gy as u64)
        .checked_mul(slices as u64 * fmt.codec.block_bytes() as u64)
        .ok_or_else(|| invalid("KTX2: level too large"))?;
    let all = image * h.layers.max(1) as u64 * h.faces as u64;
    let data = level_data(bytes, h, level, all)?;
    let mut out = Vec::new();
    for layer in 0..layers {
        for face in 0..h.faces {
            let at = (layer as u64 * h.faces as u64 + face as u64) * image;
            let s = slice(&data, at, image, "KTX2 level")?;
            out.push(decode_surface(fmt, w, ht, d, s, None)?);
        }
    }
    Ok(out)
}

/// A level's bytes with any zstd or zlib supercompression undone.  `expect`
/// is the size the level must have, which also caps decompression.
pub(crate) fn level_data<'a>(
    bytes: &'a [u8],
    h: &Header,
    level: u32,
    expect: u64,
) -> Result<Cow<'a, [u8]>, TextureError> {
    let (off, len, raw_len) = h.index[level as usize];
    let packed = slice(bytes, off, len, "KTX2 level")?;
    crate::util::check_bytes(expect)?;
    match h.scheme {
        0 => Ok(Cow::Borrowed(packed)),
        2 | 3 => {
            if raw_len != expect {
                return Err(invalid(format!("KTX2: level {level} unpacks to {raw_len} bytes, expected {expect}")));
            }
            let out = if h.scheme == 2 { unzstd(packed, expect as usize)? } else { inflate(packed, expect as usize)? };
            if out.len() as u64 != expect {
                return Err(invalid(format!("KTX2: level {level} unpacked to {} bytes, expected {expect}", out.len())));
            }
            Ok(Cow::Owned(out))
        }
        n => Err(unsupported(format!("KTX2 supercompression scheme {n}"))),
    }
}

/// Inflates a zstd stream, never past `max` bytes.
pub(crate) fn unzstd(data: &[u8], max: usize) -> Result<Vec<u8>, TextureError> {
    let mut out = vec![0u8; max];
    let n = zstd::bulk::decompress_to_buffer(data, &mut out[..]).map_err(|e| invalid(format!("zstd: {e}")))?;
    out.truncate(n);
    Ok(out)
}

fn inflate(data: &[u8], max: usize) -> Result<Vec<u8>, TextureError> {
    miniz_oxide::inflate::decompress_to_vec_zlib_with_limit(data, max).map_err(|e| invalid(format!("zlib: {e:?}")))
}

/// `KTXswizzle`: where each output channel comes from, for formats such as
/// R8 that stand in for luminance (`rrr1`).
pub(crate) struct Swizzle([u8; 4]);

impl Swizzle {
    pub fn parse(v: Option<&str>) -> Self {
        let mut s = [0, 1, 2, 3];
        if let Some(v) = v
            && v.len() >= 4
        {
            for (i, c) in v.bytes().take(4).enumerate() {
                s[i] = match c {
                    b'r' => 0,
                    b'g' => 1,
                    b'b' => 2,
                    b'a' => 3,
                    b'0' => 4,
                    b'1' => 5,
                    _ => return Swizzle([0, 1, 2, 3]),
                };
            }
        }
        Swizzle(s)
    }

    pub fn is_identity(&self) -> bool {
        self.0 == [0, 1, 2, 3]
    }

    pub fn apply(&self, p: Pixels) -> Pixels {
        if self.is_identity() {
            return p;
        }
        let s = self.0;
        match p {
            Pixels::Rgba8(mut v) => {
                for t in v.chunks_exact_mut(4) {
                    let src = [t[0], t[1], t[2], t[3], 0, 255];
                    for c in 0..4 {
                        t[c] = src[s[c] as usize];
                    }
                }
                Pixels::Rgba8(v)
            }
            Pixels::Rgba32F(mut v) => {
                for t in v.chunks_exact_mut(4) {
                    let src = [t[0], t[1], t[2], t[3], 0.0, 1.0];
                    for c in 0..4 {
                        t[c] = src[s[c] as usize];
                    }
                }
                Pixels::Rgba32F(v)
            }
            blocks @ Pixels::Blocks(..) => blocks,
        }
    }
}
