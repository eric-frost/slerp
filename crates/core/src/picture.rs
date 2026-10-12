//! What the Qt image plugin shows: textures as the pictures they are, at up
//! to the size the image viewer asks for and with their alpha, and models,
//! levels and materials as scenes to draw as a turntable.

use crate::{Kind, Prepared, ThumbError, load_kind, prepare_asset, sniff_file, strip_sky};
use slerp_scene::{Asset, Image8, Limits, LoadError, SceneKind};
use slerp_textures::{Pixels, Texture, TextureKind};
use std::path::Path;

/// Loads `path` for an image viewer, for the helper's answer to the Qt
/// plugin.  A picture comes back no larger than `size` on its longer side,
/// which for textures is normally their own size; a scene comes back to be
/// drawn as a turntable with [`crate::ThumbRenderer::turntable`].
///
/// It differs from [`crate::prepare_thumbnail`] in three ways:
///
/// - Textures decode up to `size` rather than a thumbnail's 1024 pixels, so
///   Gwenview opens them at full resolution.
/// - Alpha stays.  Thumbnails drop it, because a mask would hide a
///   texture's colour in a file manager; an image viewer draws a
///   checkerboard behind it instead.
/// - A material file is always drawn on a ball, even when it ships a
///   preview picture, since the point of the plugin is a model that turns.
pub fn prepare_picture(path: &Path, size: u32) -> Result<Prepared, ThumbError> {
    let size = size.max(1);
    let kind = sniff_file(path).map_err(LoadError::Io)?.ok_or(ThumbError::Unknown)?;
    let limits = match kind {
        Kind::Texture(_) => Limits { max_texture_size: Some(size), ..Limits::thumbnail() },
        _ => crate::thumbnail_limits(size),
    };
    let asset = load_kind(kind, path, &limits)?;
    let prepared = match asset {
        Asset::Image(tex) => {
            // A thumbnail enlarges small textures to fill the size asked for;
            // a picture is never larger than the texture (its cross, for
            // cubemaps).
            let own = if tex.kind == TextureKind::Cube { tex.width().saturating_mul(4) } else { tex.width() };
            let size = size.min(own.max(tex.height()).max(1));
            Prepared::Done(with_alpha(&tex, slerp_textures::thumbnail(&tex, size)))
        }
        Asset::Material(mat) if !matches!(kind, Kind::Material(slerp_materials::MaterialFormat::Sbsar)) => {
            // At thumbnail size the loader skips the textures of a material
            // that ships a preview picture, since the thumbnail shows the
            // picture, so the ball needs the file loaded again with them.
            // ambientCG's 1K and 2K materials take 40-140 ms that way.
            let mat = match mat.preview {
                Some(_) => match load_kind(kind, path, &Limits { max_texture_size: None, ..limits })? {
                    Asset::Material(full) => full,
                    _ => mat,
                },
                None => mat,
            };
            Prepared::Render(Box::new(crate::material_scene(&mat, &crate::MaterialState::default())))
        }
        asset => prepare_asset(kind, asset, size)?,
    };
    Ok(match prepared {
        Prepared::Render(mut scene) => {
            if scene.kind == SceneKind::Level {
                strip_sky(&mut scene);
            }
            Prepared::Render(scene)
        }
        done => done,
    })
}

/// Puts a texture's alpha back into its thumbnail, `img`, which shows the
/// colour opaque.  Only plain 2D textures (and the first layer or slice of
/// arrays and volumes) whose format stores colour and alpha get it back:
/// cubemaps are laid out as a cross whose gaps are already transparent,
/// and single-channel formats are shown as grey on purpose.  Shrunk
/// pictures average the alpha of the texels each pixel covers; enlarged
/// ones repeat it, as the colour is.
pub fn with_alpha(tex: &Texture, mut img: Image8) -> Image8 {
    let level = &tex.levels[0];
    let plain_colour = !["(R)", "(RG)", "(A)", "(data)"].iter().any(|tag| tex.source.format.ends_with(tag));
    if !tex.has_alpha || tex.kind == TextureKind::Cube || !plain_colour || level.surfaces.is_empty() {
        return img;
    }
    let (w, h) = (level.width as usize, level.height as usize);
    let (tw, th) = (img.width as usize, img.height as usize);
    let alpha_at = |i: usize| -> f32 {
        match &level.surfaces[0] {
            Pixels::Rgba8(v) => v.get(i * 4 + 3).map_or(1.0, |&a| a as f32 / 255.0),
            Pixels::Rgba32F(v) => v.get(i * 4 + 3).map_or(1.0, |a| a.clamp(0.0, 1.0)),
            // Pictures are decoded with `decode`, which never keeps blocks.
            Pixels::Blocks(..) => 1.0,
        }
    };
    if w == 0 || h == 0 || tw == 0 || th == 0 || img.pixels.len() != tw * th * 4 {
        return img;
    }
    // Full size, the usual case: one texel per pixel.
    if (tw, th) == (w, h) {
        for (i, p) in img.pixels.chunks_exact_mut(4).enumerate() {
            p[3] = (p[3] as f32 * alpha_at(i) + 0.5) as u8;
        }
        return img;
    }
    for y in 0..th {
        // The rows and columns of texels this pixel covers: at least one,
        // so enlarging repeats texels.
        let (y0, y1) = (y * h / th, ((y + 1) * h).div_ceil(th).max(y * h / th + 1).min(h));
        for x in 0..tw {
            let (x0, x1) = (x * w / tw, ((x + 1) * w).div_ceil(tw).max(x * w / tw + 1).min(w));
            let mut sum = 0.0;
            for sy in y0..y1 {
                for sx in x0..x1 {
                    sum += alpha_at(sy * w + sx);
                }
            }
            let a = sum / ((y1 - y0) * (x1 - x0)) as f32;
            let p = &mut img.pixels[(y * tw + x) * 4 + 3];
            *p = (*p as f32 * a + 0.5) as u8;
        }
    }
    img
}

#[cfg(test)]
mod tests {
    use super::*;
    use slerp_textures::ColorSpace;

    fn texture(w: u32, h: u32, alpha: impl Fn(u32, u32) -> u8) -> Texture {
        let mut data = Vec::new();
        for y in 0..h {
            for x in 0..w {
                data.extend_from_slice(&[200, 100, 50, alpha(x, y)]);
            }
        }
        Texture::from_rgba8(w, h, data, ColorSpace::Srgb)
    }

    #[test]
    fn alpha_survives_at_full_size() {
        let tex = texture(4, 2, |x, _| if x < 2 { 0 } else { 255 });
        let img = with_alpha(&tex, slerp_textures::thumbnail(&tex, 4));
        assert_eq!((img.width, img.height), (4, 2));
        let alphas: Vec<u8> = img.pixels.chunks_exact(4).map(|p| p[3]).collect();
        assert_eq!(alphas, [0, 0, 255, 255, 0, 0, 255, 255]);
        // The colour underneath is untouched.
        assert_eq!(&img.pixels[..3], &[200, 100, 50]);
    }

    #[test]
    fn alpha_averages_when_shrunk_and_repeats_when_enlarged() {
        let tex = texture(4, 4, |x, _| if x % 2 == 0 { 0 } else { 255 });
        let small = with_alpha(&tex, slerp_textures::thumbnail(&tex, 2));
        assert!(small.pixels.chunks_exact(4).all(|p| (127..=128).contains(&p[3])), "{:?}", small.pixels);
        let tiny = texture(2, 1, |x, _| if x == 0 { 0 } else { 255 });
        let big = with_alpha(&tiny, slerp_textures::thumbnail(&tiny, 8));
        assert_eq!((big.width, big.height), (8, 4));
        let row: Vec<u8> = big.pixels.chunks_exact(4).take(8).map(|p| p[3]).collect();
        assert_eq!(row, [0, 0, 0, 0, 255, 255, 255, 255]);
    }

    #[test]
    fn opaque_and_grey_textures_are_left_alone() {
        let opaque = texture(2, 2, |_, _| 255);
        let img = slerp_textures::thumbnail(&opaque, 2);
        assert_eq!(with_alpha(&opaque, img.clone()).pixels, img.pixels);
        let mut mask = texture(2, 2, |x, _| x as u8 * 255);
        mask.source.format = "A8_UNORM (A)".into();
        let img = slerp_textures::thumbnail(&mask, 2);
        assert_eq!(with_alpha(&mask, img.clone()).pixels, img.pixels);
    }
}
