//! Photographs as cameras and phones write them: JPEG, PNG and WebP stills
//! carry an EXIF orientation (the sensor's idea of up, which is why photos
//! show up sideways in viewers that ignore it) and often an ICC colour
//! profile (an iPhone's pictures are Display P3, which looks washed out when
//! shown as if it were sRGB).
//!
//! The texture crate decodes the pixels as stored, which is what a game
//! wants of a texture.  [`refine`] turns a decoded still into the picture a
//! person sees, and notes what it did for the inspector.

use crate::color::{Conversion, describe_icc, primaries_name, transfer_name};
use crate::exif::{self, Meta};
use crate::orient::{self, describe, swaps_axes};
use slerp_scene::Texture;
use slerp_scene::textures::{Format, Pixels, TextureKind};
use std::path::Path;

/// Applies the orientation and colour profile the file at `path` carries to
/// the still `tex` decoded from it.  Files that aren't JPEG, PNG or WebP,
/// data textures and anything unreadable come back as they are.
pub fn refine(path: &Path, format: Format, mut tex: Texture) -> Texture {
    let Ok(mut file) = std::fs::File::open(path) else { return tex };
    let meta = match format {
        Format::Jpeg => exif::read_jpeg(&mut file),
        Format::Png => exif::read_png(&mut file),
        Format::Webp => exif::read_webp(&mut file),
        _ => return tex,
    };
    apply(&mut tex, &meta);
    tex
}

/// Whether the texture is a plain picture that metadata applies to.
fn eligible(tex: &Texture) -> bool {
    tex.kind == TextureKind::D2
        && tex.levels.len() == 1
        && tex.layers <= 1
        && tex.faces <= 1
        && tex.levels[0].surfaces.len() == 1
        && !tex.source.format.ends_with("(data)")
        && !tex.source.format.ends_with("(stretch)")
        && matches!(tex.levels[0].surfaces[0], Pixels::Rgba8(_) | Pixels::Rgba32F(_))
}

/// Converts colour to sRGB as the profile or code points ask, then turns the
/// picture upright, and adds what happened to the texture's header facts.
pub fn apply(tex: &mut Texture, meta: &Meta) {
    if !eligible(tex) {
        return;
    }
    // 1. Colour.
    if let Pixels::Rgba8(px) = &mut tex.levels[0].surfaces[0] {
        let (conversion, source) = match (&meta.icc, meta.cicp) {
            (Some(icc), _) => (Conversion::from_icc(icc), Some(describe_icc(icc))),
            (None, Some(cp)) => (
                Conversion::from_cicp(cp),
                Some(format!("{} primaries, {} transfer (CICP)", primaries_name(cp[0]), transfer_name(cp[1]))),
            ),
            _ => (None, None),
        };
        match (conversion, source) {
            (Some(c), _) if c.is_srgb() => {
                tex.source.header.push(("Colour profile".into(), format!("{}, already sRGB", c.name)));
            }
            (Some(c), _) => {
                c.convert_rgba8(px);
                tex.source.header.push(("Colour profile".into(), format!("{}, converted to sRGB", c.name)));
            }
            (None, Some(s)) => {
                tex.source.header.push(("Colour profile".into(), format!("{s}, not applied (shown as sRGB)")));
            }
            (None, None) => {}
        }
    }
    // 2. Orientation.
    if !meta.upright() {
        orient_texture(tex, meta.orientation);
        tex.source.header.push((
            "Orientation".into(),
            format!("{} (EXIF {}), shown upright", describe(meta.orientation), meta.orientation),
        ));
    }
}

/// Turns the texture's single surface as EXIF orientation `o` asks.
pub fn orient_texture(tex: &mut Texture, o: u8) {
    if !(2..=8).contains(&o) {
        return;
    }
    let level = &mut tex.levels[0];
    let (w, h) = (level.width as usize, level.height as usize);
    let (pixels, nw, nh) = match &level.surfaces[0] {
        Pixels::Rgba8(v) => {
            let (v, nw, nh) = orient::orient(v, w, h, 4, o);
            (Pixels::Rgba8(v), nw, nh)
        }
        Pixels::Rgba32F(v) => {
            let (v, nw, nh) = orient::orient(v, w, h, 4, o);
            (Pixels::Rgba32F(v), nw, nh)
        }
        Pixels::Blocks(..) => return,
    };
    level.surfaces[0] = pixels;
    level.width = nw as u32;
    level.height = nh as u32;
    if swaps_axes(o) {
        std::mem::swap(&mut tex.source.width, &mut tex.source.height);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use slerp_scene::ColorSpace;

    fn tex(w: u32, h: u32) -> Texture {
        let px: Vec<u8> = (0..w * h).flat_map(|i| [i as u8, 0, 0, 255]).collect();
        let mut t = Texture::from_rgba8(w, h, px, ColorSpace::Unspecified);
        t.source.format = "RGB".into();
        t
    }

    #[test]
    fn orientation_turns_the_picture_and_says_so() {
        let mut t = tex(3, 2);
        apply(&mut t, &Meta { orientation: 6, ..Meta::default() });
        assert_eq!((t.width(), t.height()), (2, 3));
        assert_eq!((t.source.width, t.source.height), (2, 3));
        let Pixels::Rgba8(v) = t.base() else { panic!() };
        assert_eq!(v.chunks_exact(4).map(|p| p[0]).collect::<Vec<_>>(), [3, 0, 4, 1, 5, 2]);
        assert!(t.source.header.iter().any(|(k, v)| k == "Orientation" && v.contains("90 degrees clockwise")));
    }

    #[test]
    fn upright_photos_are_untouched() {
        let mut t = tex(3, 2);
        let before = t.clone();
        apply(&mut t, &Meta { orientation: 1, ..Meta::default() });
        assert_eq!(t.width(), before.width());
        assert!(t.source.header.is_empty());
        apply(&mut t, &Meta::default());
        assert!(t.source.header.is_empty());
    }

    #[test]
    fn display_p3_is_converted() {
        let mut t = Texture::from_rgba8(1, 1, vec![255, 0, 0, 255], ColorSpace::Unspecified);
        t.source.format = "RGB".into();
        // Display P3 green is outside sRGB; as a CICP photo it stays a valid pixel.
        apply(&mut t, &Meta { orientation: 1, icc: None, cicp: Some([12, 13, 0, 1]) });
        let Pixels::Rgba8(v) = t.base() else { panic!() };
        assert_eq!(&v[..], [255, 0, 0, 255]);
        assert!(t.source.header[0].1.contains("converted to sRGB"), "{:?}", t.source.header);
        let mut t = Texture::from_rgba8(1, 1, vec![128, 128, 128, 255], ColorSpace::Unspecified);
        apply(&mut t, &Meta { orientation: 1, icc: None, cicp: Some([1, 13, 0, 1]) });
        assert!(t.source.header[0].1.contains("already sRGB"));
    }

    #[test]
    fn data_textures_and_unreadable_profiles_are_left_alone() {
        let mut t = Texture::from_rgba32f(1, 1, vec![0.5, 0.5, 0.5, 1.0]);
        t.source.format = "Grayscale 16-bit (data)".into();
        apply(&mut t, &Meta { orientation: 6, icc: None, cicp: None });
        assert!(t.source.header.is_empty());
        let mut t = tex(2, 1);
        apply(&mut t, &Meta { orientation: 1, icc: Some(vec![0; 200]), cicp: None });
        assert!(t.source.header[0].1.contains("not applied"), "{:?}", t.source.header);
    }
}
