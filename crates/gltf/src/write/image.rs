//! Pictures for the file: the original encoded bytes when the loader kept
//! them and glTF can hold them, else an 8-bit PNG of the decoded pixels.

use slerp_scene::textures::{self, ColorSpace, Format, Level, Pixels, SourceInfo, Texture, TextureKind};
use std::borrow::Cow;

/// An image ready for the file.
pub struct Encoded {
    pub bytes: Vec<u8>,
    pub mime: &'static str,
    /// The extension a texture needs to name this image (KTX2 and WebP
    /// aren't core glTF), which readers must then understand.
    pub extension: Option<&'static str>,
    pub warning: Option<String>,
}

/// Encodes texture `tex`, used in a colour slot (base colour, emissive)
/// when `colour`, whose pictures glTF reads as sRGB.
pub fn encode(tex: &Texture, colour: bool, originals: bool) -> Result<Encoded, String> {
    let mut warning = None;
    if originals && let Some(original) = &tex.source.original {
        match original.read() {
            Ok(bytes) => {
                if let Some((mime, extension)) = embeddable(&bytes) {
                    return Ok(Encoded { bytes: bytes.into_owned(), mime, extension, warning: None });
                }
            }
            Err(e) => {
                warning = Some(format!("a picture's original file couldn't be read ({e}), so it was saved as PNG"))
            }
        }
    }
    let mut encoded = png(tex, colour)?;
    encoded.warning = encoded.warning.or(warning);
    Ok(encoded)
}

/// The MIME type and texture extension of a picture glTF can hold as it
/// is: PNG and JPEG in the core, WebP through `EXT_texture_webp`, and KTX2
/// holding Basis Universal through `KHR_texture_basisu`.
fn embeddable(bytes: &[u8]) -> Option<(&'static str, Option<&'static str>)> {
    match textures::sniff(&bytes[..bytes.len().min(64)])? {
        Format::Png => Some(("image/png", None)),
        Format::Jpeg => Some(("image/jpeg", None)),
        Format::Webp => Some(("image/webp", Some("EXT_texture_webp"))),
        Format::Ktx2 if is_basis_2d(bytes) => Some(("image/ktx2", Some("KHR_texture_basisu"))),
        _ => None,
    }
}

/// True for a KTX2 file `KHR_texture_basisu` allows: a plain 2D texture
/// in Basis Universal, either ETC1S (BasisLZ supercompression) or UASTC
/// (the data format descriptor's colour model 166).
fn is_basis_2d(b: &[u8]) -> bool {
    let u32_at = |at: usize| b.get(at..at + 4).map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]));
    let (Some(vk_format), Some(depth), Some(layers), Some(faces), Some(scheme), Some(dfd)) =
        (u32_at(12), u32_at(28), u32_at(32), u32_at(36), u32_at(44), u32_at(48))
    else {
        return false;
    };
    let colour_model = b.get(dfd as usize + 12).copied();
    vk_format == 0 && depth == 0 && layers <= 1 && faces == 1 && (scheme == 1 || colour_model == Some(166))
}

/*┌─────────────────╮
| PNG             |
└─────────────────╯*/
//region PNG

/// The first surface of the largest level as an 8-bit PNG, without alpha
/// when every texel is opaque.
fn png(tex: &Texture, colour: bool) -> Result<Encoded, String> {
    let level = tex.levels.first().ok_or("it has no pixels")?;
    let (width, height) = (level.width, level.height);
    let texels = width as usize * height as usize;
    let surface = level.surfaces.first().ok_or("it has no pixels")?;
    if texels == 0 {
        return Err("it has no pixels".into());
    }
    let mut warning = None;
    if tex.kind != TextureKind::D2 || tex.layers > 1 {
        warning = Some("cube maps, arrays and 3D textures were saved as their first picture".to_string());
    }
    let rgba: Cow<[u8]> = match surface {
        Pixels::Rgba8(v) => {
            let v = v.get(..texels * 4).ok_or("its pixels are short")?;
            if colour && tex.color_space == ColorSpace::Linear {
                Cow::Owned(v.chunks_exact(4).flat_map(|p| [srgb8(p[0]), srgb8(p[1]), srgb8(p[2]), p[3]]).collect())
            } else {
                Cow::Borrowed(v)
            }
        }
        Pixels::Rgba32F(v) => {
            let v = v.get(..texels * 4).ok_or("its pixels are short")?;
            let clipped = v.chunks_exact(4).any(|p| p[..3].iter().any(|&c| c > 1.0));
            warning = Some(if clipped {
                "floating-point pictures were saved as 8-bit PNG, clipping values above 1".to_string()
            } else {
                "floating-point pictures were saved as 8-bit PNG".to_string()
            });
            let channel = |c: f32| {
                let c = if c.is_finite() { c.clamp(0.0, 1.0) } else { 0.0 };
                if colour { srgb_encode(c) } else { (c * 255.0 + 0.5) as u8 }
            };
            let alpha = |a: f32| (if a.is_finite() { a.clamp(0.0, 1.0) } else { 1.0 } * 255.0 + 0.5) as u8;
            Cow::Owned(
                v.chunks_exact(4).flat_map(|p| [channel(p[0]), channel(p[1]), channel(p[2]), alpha(p[3])]).collect(),
            )
        }
        Pixels::Blocks(..) => {
            let one = Texture {
                kind: TextureKind::D2,
                layers: 1,
                faces: 1,
                levels: vec![Level { width, height, depth: 1, surfaces: vec![surface.clone()] }],
                color_space: tex.color_space,
                has_alpha: tex.has_alpha,
                source: SourceInfo::default(),
            };
            match textures::decompress(&one).levels.into_iter().next().and_then(|l| l.surfaces.into_iter().next()) {
                Some(Pixels::Rgba8(v)) if v.len() >= texels * 4 => {
                    let mut v = v;
                    v.truncate(texels * 4);
                    if colour && tex.color_space == ColorSpace::Linear {
                        v.chunks_exact_mut(4).for_each(|p| (0..3).for_each(|k| p[k] = srgb8(p[k])));
                    }
                    Cow::Owned(v)
                }
                _ => return Err("its compressed blocks couldn't be decoded".into()),
            }
        }
    };
    let opaque = rgba.chunks_exact(4).all(|p| p[3] == 255);
    let (data, color): (Cow<[u8]>, png::ColorType) = if opaque {
        (Cow::Owned(rgba.chunks_exact(4).flat_map(|p| [p[0], p[1], p[2]]).collect()), png::ColorType::Rgb)
    } else {
        (rgba, png::ColorType::Rgba)
    };
    let mut bytes = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut bytes, width, height);
        encoder.set_color(color);
        encoder.set_depth(png::BitDepth::Eight);
        encoder.set_compression(png::Compression::Fast);
        let mut writer = encoder.write_header().map_err(|e| e.to_string())?;
        writer.write_image_data(&data).map_err(|e| e.to_string())?;
        writer.finish().map_err(|e| e.to_string())?;
    }
    Ok(Encoded { bytes, mime: "image/png", extension: None, warning })
}

/// A linear value from 0 to 1 in sRGB's encoding, as a byte.
fn srgb_encode(c: f32) -> u8 {
    let s = if c <= 0.003_130_8 { c * 12.92 } else { 1.055 * c.powf(1.0 / 2.4) - 0.055 };
    (s.clamp(0.0, 1.0) * 255.0 + 0.5) as u8
}

/// A linear byte in sRGB's encoding, through a table: a 4K picture has
/// fifty million of them.
fn srgb8(c: u8) -> u8 {
    static TABLE: std::sync::OnceLock<[u8; 256]> = std::sync::OnceLock::new();
    TABLE.get_or_init(|| std::array::from_fn(|i| srgb_encode(i as f32 / 255.0)))[c as usize]
}

//endregion
