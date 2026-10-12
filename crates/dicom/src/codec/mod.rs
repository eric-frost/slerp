//! Decoders for compressed (encapsulated) pixel data.  Each gives the
//! samples as stored, before DICOM's masking, sign and rescale, which the
//! pixel module applies the same way for every codec.

pub mod j2k;
pub mod jpeg;
pub mod jpegls;
pub mod rle;

use crate::image::ImageInfo;
use crate::transfer::Codec;

/// A decoded frame's samples, interleaved when there are several per
/// pixel.  Integer samples are bit patterns of `precision` bits; signed
/// data is sign-extended later.
pub enum Samples {
    U8(Vec<u8>),
    U16(Vec<u16>),
    U32(Vec<u32>),
    F32(Vec<f32>),
}

impl Samples {
    pub fn len(&self) -> usize {
        match self {
            Samples::U8(v) => v.len(),
            Samples::U16(v) => v.len(),
            Samples::U32(v) => v.len(),
            Samples::F32(v) => v.len(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Sample `i` as an integer bit pattern.
    #[inline]
    pub fn get(&self, i: usize) -> u32 {
        match self {
            Samples::U8(v) => v[i] as u32,
            Samples::U16(v) => v[i] as u32,
            Samples::U32(v) => v[i],
            Samples::F32(v) => v[i] as i32 as u32,
        }
    }
}

pub struct Decoded {
    pub width: u32,
    pub height: u32,
    pub comps: u32,
    /// Bits per sample the codec decoded.
    pub precision: u32,
    pub data: Samples,
    /// Colour samples are RGB already, whatever the photometric
    /// interpretation says: DCT JPEG was converted from YCbCr, JPEG 2000's
    /// component transform undone.
    pub rgb: bool,
}

/// Recognises a compressed frame by its first bytes, for transfer
/// syntaxes this crate doesn't know or files that name the wrong one.
pub fn sniff(data: &[u8]) -> Codec {
    if data.len() >= 4 && data[0] == 0xFF && data[1] == 0xD8 {
        // The first frame marker tells JPEG-LS (F7) from JPEG.
        let mut pos = 2;
        while pos + 4 <= data.len() && pos < 1 << 16 {
            if data[pos] != 0xFF {
                pos += 1;
                continue;
            }
            match data[pos + 1] {
                0xF7 => return Codec::JpegLs,
                0xC3 | 0xC7 => return Codec::JpegLossless,
                0xC0..=0xC2 | 0xC5 | 0xC6 => return Codec::Jpeg,
                0xFF => pos += 1,
                _ => pos += 2 + u16::from_be_bytes([data[pos + 2], data[pos + 3]]) as usize,
            }
        }
        return Codec::Jpeg;
    }
    if data.starts_with(&[0xFF, 0x4F, 0xFF, 0x51]) || data.starts_with(&[0, 0, 0, 0x0C, b'j', b'P']) {
        return Codec::Jpeg2000;
    }
    if data.len() >= 64 {
        let segments = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
        let first = u32::from_le_bytes([data[4], data[5], data[6], data[7]]);
        if (1..=15).contains(&segments) && first == 64 {
            return Codec::Rle;
        }
    }
    Codec::Unknown
}

/// Decodes one compressed frame.  Panics in the codecs (third-party ones
/// included) become errors.
pub fn decode(codec: Codec, data: &[u8], info: &ImageInfo, max_size: Option<u32>) -> Result<Decoded, String> {
    // The bytes decide when they disagree with the transfer syntax: files
    // that say JPEG-LS but hold JPEG exist.
    let sniffed = sniff(data);
    let codec = match (codec, sniffed) {
        (_, Codec::Unknown) => codec,
        (Codec::Rle, _) => Codec::Rle,
        (Codec::HtJpeg2000, _) => Codec::HtJpeg2000,
        (_, s) => s,
    };
    let photometric = info.photometric.clone();
    let run = move || match codec {
        Codec::Jpeg | Codec::JpegLossless => jpeg::decode(data, &photometric, max_size),
        Codec::JpegLs => jpegls::decode(data),
        Codec::Jpeg2000 => j2k::decode(data, max_size),
        Codec::HtJpeg2000 => Err("high-throughput JPEG 2000 (HTJ2K) isn't supported".into()),
        Codec::Rle => rle::decode(data, info.rows, info.columns, info.samples, info.bits_allocated.div_ceil(8)),
        Codec::Video => Err("video-compressed DICOM (MPEG, H.264, HEVC) isn't supported".into()),
        Codec::JpegXl => Err("JPEG XL DICOM isn't supported yet".into()),
        Codec::Native | Codec::EncapsulatedNative | Codec::DeflatedFrames | Codec::Unknown => {
            Err("unknown pixel data compression".into())
        }
    };
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(run))
        .unwrap_or_else(|_| Err(format!("the {codec:?} decoder failed on this frame")))
}
