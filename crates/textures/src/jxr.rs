//! JPEG XR through `slerp-jxr`, which runs Microsoft's decoder in a child
//! process.  8-bit pictures come out as sRGB 8-bit RGBA; half, fixed-point
//! and float ones (Windows HDR screenshots) as linear float RGBA, which the
//! viewer's exposure control and the HDR thumbnail path treat like any
//! other HDR picture.

use crate::formats::channel_tag;
use crate::thumb::Channels;
use crate::util::{MAX_DECODED_BYTES, too_large};
use crate::{ColorSpace, DecodeOptions, Texture, TextureError};
use slerp_jxr::{JxrError, Rgba};

pub(crate) fn sniff(head: &[u8]) -> bool {
    slerp_jxr::sniff(head)
}

pub(crate) fn decode(bytes: &[u8], opts: &DecodeOptions) -> Result<Texture, TextureError> {
    let min = opts.min_size.or(opts.max_size).unwrap_or(0);
    // The decoder's converted pixels are at most 16 bytes each; widening
    // grey 8-bit to RGBA multiplies by four.
    let d = slerp_jxr::decode(bytes, min, (MAX_DECODED_BYTES / 4) as usize).map_err(|e| match e {
        JxrError::Unsupported(m) => TextureError::Unsupported(m),
        JxrError::Invalid(m) => TextureError::Invalid(m),
        JxrError::TooLarge(m) => too_large(m),
    })?;
    let (w, h) = (d.width, d.height);
    let float = d.layout.is_float();
    let mut tex = match d.to_rgba() {
        Rgba::U8(v) => Texture::from_rgba8(w, h, v, ColorSpace::Srgb),
        Rgba::F32(v) => Texture::from_rgba32f(w, h, v),
    };
    tex.source.container = "JPEG XR".into();
    tex.source.format = d.native.clone() + if float { "" } else { channel_tag(Channels::Rgba) };
    tex.source.width = d.full_width;
    tex.source.height = d.full_height;
    tex.source.file_bytes = bytes.len() as u64;
    tex.source.header.push(("Pixel format".into(), d.native));
    tex.source.header.push(("Size".into(), format!("{} x {}", d.full_width, d.full_height)));
    tex.source.header.push(("Alpha".into(), if d.has_alpha { "yes" } else { "no" }.into()));
    if let Some((x, y)) = d.dpi {
        tex.source.header.push(("Resolution".into(), format!("{x:.0} x {y:.0} dpi")));
    }
    if (w, h) != (d.full_width, d.full_height) {
        tex.source.header.push(("Decoded at".into(), format!("{w} x {h}")));
    }
    Ok(tex)
}
