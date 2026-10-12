//! Ordinary images: PNG, JPEG and WebP through their crates, and small
//! readers of our own for TGA, BMP and PCX, the formats game files embed or
//! sit next to (Quake 2 skins are PCX, Quake 3 skins TGA).

mod bmp;
mod pcx;
mod tga;

pub(crate) use bmp::plausible as bmp_plausible;
pub(crate) use pcx::plausible as pcx_plausible;
pub(crate) use tga::plausible as tga_plausible;

use crate::resize::{reduce_factor, to_rgba8};
use crate::util::{MAX_DECODED_BYTES, check_size, invalid};
use crate::{ColorSpace, DecodeOptions, Format, Texture, TextureError};
use rayon::prelude::*;
use std::io::Cursor;

/// An 8-bit picture as a reader produced it, before it becomes a `Texture`.
pub(crate) struct Picture {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
    /// The pixel format as the file names it, for `SourceInfo`.
    pub format: String,
    /// The size stored in the file, when a smaller image was decoded.
    pub file_size: (u32, u32),
}

impl Picture {
    pub fn new(width: u32, height: u32, rgba: Vec<u8>, format: String) -> Self {
        Picture { width, height, rgba, format, file_size: (width, height) }
    }
}

pub(crate) fn decode(format: Format, bytes: &[u8], opts: &DecodeOptions) -> Result<Texture, TextureError> {
    let (container, pic) = match format {
        Format::Png => return decode_png(bytes, opts),
        Format::Jpeg => ("JPEG", decode_jpeg(bytes, opts)?),
        Format::Webp => ("WebP", decode_webp(bytes, opts)?),
        Format::Tga => ("TGA", tga::decode(bytes)?),
        Format::Bmp => ("BMP", bmp::decode(bytes, opts)?),
        Format::Pcx => ("PCX", pcx::decode(bytes)?),
        _ => unreachable!("not an ordinary image format"),
    };
    Ok(finish(container, pic, bytes.len()))
}

fn finish(container: &str, pic: Picture, file_bytes: usize) -> Texture {
    let mut tex = Texture::from_rgba8(pic.width, pic.height, pic.rgba, ColorSpace::Unspecified);
    tex.source.container = container.into();
    tex.source.format = pic.format;
    tex.source.file_bytes = file_bytes as u64;
    (tex.source.width, tex.source.height) = pic.file_size;
    tex
}

/// Expands rows of `channels`-byte pixels (grey, grey and alpha, RGB or
/// RGBA) to RGBA.
pub(crate) fn expand_to_rgba(src: &[u8], channels: usize) -> Vec<u8> {
    match channels {
        1 => src.iter().flat_map(|&v| [v, v, v, 255]).collect(),
        2 => src.chunks_exact(2).flat_map(|p| [p[0], p[0], p[0], p[1]]).collect(),
        3 => src.chunks_exact(3).flat_map(|p| [p[0], p[1], p[2], 255]).collect(),
        _ => src.to_vec(),
    }
}

/*┌─────────────────╮
| PNG             |
└─────────────────╯*/
//region PNG

fn png_err(e: png::DecodingError) -> TextureError {
    match e {
        png::DecodingError::IoError(e) => invalid(format!("PNG: {e}")),
        png::DecodingError::LimitsExceeded => TextureError::TooLarge("PNG".into()),
        e => invalid(format!("PNG: {e}")),
    }
}

fn decode_png(bytes: &[u8], opts: &DecodeOptions) -> Result<Texture, TextureError> {
    let limits = png::Limits { bytes: MAX_DECODED_BYTES as usize };
    let mut dec = png::Decoder::new_with_limits(Cursor::new(bytes), limits);
    dec.set_transformations(png::Transformations::normalize_to_color8());
    let mut reader = dec.read_info().map_err(png_err)?;
    let info = reader.info();
    if info.bit_depth == png::BitDepth::Sixteen
        && matches!(info.color_type, png::ColorType::Grayscale | png::ColorType::GrayscaleAlpha)
    {
        return decode_png_grey16(bytes, opts);
    }
    let (w, h, interlaced) = (info.width, info.height, info.interlaced);
    let source_format = format!("{:?} {}-bit", info.color_type, info.bit_depth as u8);
    check_size(w, h, 1, 4)?;
    let channels = reader.output_color_type().0.samples();

    // 1. Large images shrink while their rows arrive, so an 8K PNG never
    //    needs its full-size RGBA buffer for a thumbnail.  Interlaced ones
    //    arrive in passes and are decoded whole.
    let factor = match opts.min_size.or(opts.max_size) {
        Some(min) if !interlaced => reduce_factor(w.max(h), min),
        _ => 1,
    };
    let pic = if factor >= 2 {
        // Rows are read here while earlier bands of `factor` rows are
        // averaged on the thread pool.
        let (wu, f) = (w as usize, factor as usize);
        let (ow, oh) = (wu.div_ceil(f), (h as usize).div_ceil(f));
        let stride = wu * channels;
        let mut out = vec![0f32; ow * oh * 4];
        let mut result = Ok(());
        rayon::scope(|s| {
            let mut rows_out = out.chunks_mut(ow * 4);
            let mut band = Vec::with_capacity(stride * f);
            for y in 1..=h as usize {
                match reader.next_row() {
                    Ok(Some(row)) if row.data().len() >= stride => band.extend_from_slice(&row.data()[..stride]),
                    Ok(_) => {
                        result = Err(invalid("PNG: short image"));
                        break;
                    }
                    Err(e) => {
                        result = Err(png_err(e));
                        break;
                    }
                }
                if y % f == 0 || y == h as usize {
                    let (Some(dst), b) =
                        (rows_out.next(), std::mem::replace(&mut band, Vec::with_capacity(stride * f)))
                    else {
                        break;
                    };
                    s.spawn(move |_| reduce_band(&b, wu, channels, f, dst));
                }
            }
        });
        result?;
        Picture {
            width: ow as u32,
            height: oh as u32,
            rgba: to_rgba8(&out, true),
            format: source_format,
            file_size: (w, h),
        }
    } else {
        // 2. Otherwise decode the first frame whole.
        let size = reader.output_buffer_size().ok_or_else(|| TextureError::TooLarge("PNG".into()))?;
        let mut buf = vec![0u8; size];
        let frame = reader.next_frame(&mut buf).map_err(png_err)?;
        buf.truncate(frame.buffer_size());
        Picture::new(frame.width, frame.height, expand_to_rgba(&buf, channels), source_format)
    };
    Ok(finish("PNG", pic, bytes.len()))
}

/// Decodes a 16-bit greyscale PNG, which in games is nearly always data (a
/// heightmap, a mask) rather than a picture, keeping all 16 bits as floats
/// tagged as data, so thumbnails stretch them to their own range.  Large
/// images are box-averaged down to what `opts` asks for.
fn decode_png_grey16(bytes: &[u8], opts: &DecodeOptions) -> Result<Texture, TextureError> {
    let limits = png::Limits { bytes: MAX_DECODED_BYTES as usize };
    let mut dec = png::Decoder::new_with_limits(Cursor::new(bytes), limits);
    dec.set_transformations(png::Transformations::EXPAND);
    let mut reader = dec.read_info().map_err(png_err)?;
    let (w, h) = (reader.info().width, reader.info().height);
    check_size(w, h, 1, 16)?;
    let samples = reader.output_color_type().0.samples();
    let size = reader.output_buffer_size().ok_or_else(|| TextureError::TooLarge("PNG".into()))?;
    let mut buf = vec![0u8; size];
    let frame = reader.next_frame(&mut buf).map_err(png_err)?;
    let stride = frame.line_size;
    let factor = match opts.min_size.or(opts.max_size) {
        Some(min) => reduce_factor(w.max(h), min) as usize,
        None => 1,
    };
    let (wu, hu) = (w as usize, h as usize);
    let (ow, oh) = (wu.div_ceil(factor), hu.div_ceil(factor));
    let mut out = vec![0f32; ow * oh * 4];
    out.par_chunks_mut(ow * 4).enumerate().for_each(|(oy, row)| {
        let (y0, y1) = (oy * factor, ((oy + 1) * factor).min(hu));
        for (ox, o) in row.chunks_exact_mut(4).enumerate() {
            let (x0, x1) = (ox * factor, ((ox + 1) * factor).min(wu));
            let (mut sum, mut alpha) = (0f64, 0f64);
            for y in y0..y1 {
                let line = &buf[y * stride..];
                for x in x0..x1 {
                    let at = x * samples * 2;
                    sum += u16::from_be_bytes([line[at], line[at + 1]]) as f64;
                    alpha +=
                        if samples == 2 { u16::from_be_bytes([line[at + 2], line[at + 3]]) as f64 } else { 65535.0 };
                }
            }
            let n = ((y1 - y0) * (x1 - x0)).max(1) as f64 * 65535.0;
            let v = (sum / n) as f32;
            o.copy_from_slice(&[v, v, v, (alpha / n) as f32]);
        }
    });
    let mut tex = Texture::from_rgba32f(ow as u32, oh as u32, out);
    tex.source.container = "PNG".into();
    tex.source.format = format!("Grayscale 16-bit{}", crate::formats::channel_tag(crate::thumb::Channels::Data));
    tex.source.file_bytes = bytes.len() as u64;
    (tex.source.width, tex.source.height) = (w, h);
    Ok(tex)
}

/// Averages a band of 8-bit rows (`channels` per texel, sRGB) into one row
/// of linear RGBA, in boxes of `factor` texels.
fn reduce_band(band: &[u8], w: usize, channels: usize, factor: usize, out: &mut [f32]) {
    let lut = crate::color::srgb_to_linear_table();
    let rows = band.len() / (w * channels);
    for (ox, o) in out.chunks_exact_mut(4).enumerate() {
        let (x0, x1) = (ox * factor, ((ox + 1) * factor).min(w));
        let mut acc = [0f32; 4];
        for row in band.chunks_exact(w * channels) {
            for p in row[x0 * channels..x1 * channels].chunks_exact(channels) {
                let (c, a) = match channels {
                    1 => ([p[0]; 3], 255),
                    2 => ([p[0]; 3], p[1]),
                    3 => ([p[0], p[1], p[2]], 255),
                    _ => ([p[0], p[1], p[2]], p[3]),
                };
                acc[0] += lut[c[0] as usize];
                acc[1] += lut[c[1] as usize];
                acc[2] += lut[c[2] as usize];
                acc[3] += a as f32;
            }
        }
        let inv = 1.0 / ((x1 - x0) * rows.max(1)) as f32;
        o.copy_from_slice(&[acc[0] * inv, acc[1] * inv, acc[2] * inv, acc[3] * inv / 255.0]);
    }
}

//endregion

/*┌─────────────────╮
| JPEG            |
└─────────────────╯*/
//region JPEG

fn jpeg_err(e: jpeg_decoder::Error) -> TextureError {
    invalid(format!("JPEG: {e}"))
}

/// Decodes a JPEG, at 1/2, 1/4 or 1/8 scale when that still leaves the
/// longer side at least `min_size` (or `max_size`): the reduced IDCT skips
/// most of the work for large photos.
fn decode_jpeg(bytes: &[u8], opts: &DecodeOptions) -> Result<Picture, TextureError> {
    let mut d = jpeg_decoder::Decoder::new(Cursor::new(bytes));
    d.set_max_decoding_buffer_size(MAX_DECODED_BYTES as usize);
    d.read_info().map_err(jpeg_err)?;
    let info = d.info().ok_or_else(|| invalid("JPEG: no frame"))?;
    let (w, h) = (info.width as u32, info.height as u32);
    check_size(w, h, 1, 4)?;
    let (mut ow, mut oh) = (w, h);
    if let Some(min) = opts.min_size.or(opts.max_size) {
        let den = [8u32, 4, 2].into_iter().find(|&den| w.max(h).div_ceil(den) >= min);
        if let Some(den) = den {
            let (sw, sh) = d.scale(w.div_ceil(den) as u16, h.div_ceil(den) as u16).map_err(jpeg_err)?;
            (ow, oh) = (sw as u32, sh as u32);
        }
    }
    let data = d.decode().map_err(jpeg_err)?;
    let n = ow as usize * oh as usize;
    use jpeg_decoder::PixelFormat as P;
    let (rgba, name) = match info.pixel_format {
        P::L8 => (expand_to_rgba(data.get(..n).ok_or_else(|| invalid("JPEG: short"))?, 1), "Grey 8-bit"),
        P::L16 => {
            let grey: Vec<u8> = data.chunks_exact(2).take(n).map(|p| p[0]).collect();
            (expand_to_rgba(&grey, 1), "Grey 16-bit")
        }
        P::RGB24 => (expand_to_rgba(data.get(..n * 3).ok_or_else(|| invalid("JPEG: short"))?, 3), "RGB"),
        P::CMYK32 => {
            let rgba = data
                .chunks_exact(4)
                .take(n)
                .flat_map(|p| {
                    let k = 255 - p[3] as u32;
                    let ch = |c: u8| ((255 - c as u32) * k / 255) as u8;
                    [ch(p[0]), ch(p[1]), ch(p[2]), 255]
                })
                .collect();
            (rgba, "CMYK")
        }
    };
    if rgba.len() != n * 4 {
        return Err(invalid("JPEG: short image"));
    }
    Ok(Picture { width: ow, height: oh, rgba, format: name.into(), file_size: (w, h) })
}

//endregion

/*┌─────────────────╮
| WebP            |
└─────────────────╯*/
//region WebP

/// Decodes a WebP: stills through libwebp, scaled while decoding when a
/// smaller size is asked for, and animations' first frame through
/// image-webp, since libwebp's plain decoder reads stills only.
fn decode_webp(bytes: &[u8], opts: &DecodeOptions) -> Result<Picture, TextureError> {
    use libwebp_sys as webp;
    // 1. What the file holds.
    let mut features: webp::WebPBitstreamFeatures = unsafe { std::mem::zeroed() };
    // SAFETY: libwebp reads at most `bytes.len()` bytes and fills `features`.
    let status = unsafe { webp::WebPGetFeatures(bytes.as_ptr(), bytes.len(), &mut features) };
    if status != webp::VP8StatusCode::VP8_STATUS_OK {
        return Err(invalid(format!("WebP: {status:?}")));
    }
    if features.has_animation != 0 {
        return decode_animated_webp(bytes);
    }
    let (w, h) = (features.width as u32, features.height as u32);
    check_size(w, h, 1, 4)?;
    let (ow, oh) = match opts.min_size.or(opts.max_size) {
        Some(t) => crate::resize::fit(w, h, t),
        None => (w, h),
    };

    // 2. Decode into our own buffer, letting libwebp shrink rows as they
    //    come out of the decoder.
    let mut buf = vec![0u8; ow as usize * oh as usize * 4];
    let mut config: webp::WebPDecoderConfig = unsafe { std::mem::zeroed() };
    // SAFETY: the config is plain data that this call initialises.
    if !unsafe { webp::WebPInitDecoderConfig(&mut config) } {
        return Err(invalid("WebP: decoder version mismatch"));
    }
    config.options.use_threads = 1;
    if (ow, oh) != (w, h) {
        config.options.use_scaling = 1;
        config.options.scaled_width = ow as i32;
        config.options.scaled_height = oh as i32;
    }
    config.output.colorspace = webp::WEBP_CSP_MODE::MODE_RGBA;
    config.output.is_external_memory = 1;
    config.output.u.RGBA = webp::WebPRGBABuffer { rgba: buf.as_mut_ptr(), stride: ow as i32 * 4, size: buf.len() };
    // SAFETY: the output buffer is ours, sized and strided for `ow` x `oh`
    // RGBA, and outlives the call; libwebp checks it against the config.
    let status = unsafe { webp::WebPDecode(bytes.as_ptr(), bytes.len(), &mut config) };
    // SAFETY: frees nothing of ours (external memory), only libwebp's own.
    unsafe { webp::WebPFreeDecBuffer(&mut config.output) };
    if status != webp::VP8StatusCode::VP8_STATUS_OK {
        return Err(invalid(format!("WebP: {status:?}")));
    }
    if features.has_alpha == 0 {
        buf.chunks_exact_mut(4).for_each(|p| p[3] = 255);
    }
    let name = match features.format {
        1 => "lossy",
        2 => "lossless",
        _ => "mixed",
    };
    Ok(Picture { width: ow, height: oh, rgba: buf, format: name.into(), file_size: (w, h) })
}

fn decode_animated_webp(bytes: &[u8]) -> Result<Picture, TextureError> {
    let err = |e: image_webp::DecodingError| invalid(format!("WebP: {e}"));
    let mut d = image_webp::WebPDecoder::new(Cursor::new(bytes)).map_err(err)?;
    d.set_memory_limit(MAX_DECODED_BYTES as usize);
    let (w, h) = d.dimensions();
    check_size(w, h, 1, 4)?;
    let alpha = d.has_alpha();
    let size = d.output_buffer_size().ok_or_else(|| TextureError::TooLarge("WebP".into()))?;
    let mut buf = vec![0u8; size];
    d.read_image(&mut buf).map_err(err)?;
    Ok(Picture::new(w, h, expand_to_rgba(&buf, if alpha { 4 } else { 3 }), "animated".into()))
}

//endregion
