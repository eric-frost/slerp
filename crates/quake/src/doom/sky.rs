//! The sky's picture, laid out for `Effect::PanoramaSky`: a cylinder around
//! the viewer whose u counts turns anticlockwise from east and whose v is
//! the tangent of the elevation, both through the texture transform.
//!
//! Doom's own skies are one picture repeated around the horizon; GZDoom's
//! sky boxes (GLDEFS) are six pictures on the faces of a cube, which are
//! resampled here onto the cylinder.

use super::graphics::{Picture, Tex};
use super::level::STRETCH;
use rayon::prelude::*;
use slerp_scene::glam::Vec2;
use slerp_scene::{Filter, Sampler, UvTransform, Wrap};
use std::sync::Arc;

/// Rows of a Doom sky added above the picture, fading into its top row's
/// average colour.
const SKY_FADE: f32 = 48.0;

/// The widest cylinder made from a sky box, and how far up and down it
/// reaches (as the tangent of the elevation: about 68 degrees).  Above
/// that, its top row's colours continue to the zenith.
const BOX_WIDTH: u32 = 4096;
const BOX_REACH: f64 = 2.5;

pub(crate) struct SkyPicture {
    pub picture: Picture,
    pub transform: UvTransform,
    pub sampler: Sampler,
}

fn sampler() -> Sampler {
    Sampler {
        wrap_u: Wrap::Repeat,
        wrap_v: Wrap::ClampToEdge,
        mag: Filter::Nearest,
        min: Filter::Linear,
        mipmaps: true,
    }
}

/// Doom's sky, wrapped around the player as the original draws it
/// (`r_plane.c`): 1024 texture columns per turn (of the texture's size in
/// the world, for scaled skies), counted anticlockwise from east as Doom's
/// angles are (which shows the picture mirrored left to right), and one
/// texel per row of a 320x200 frame, whose lens is 160 pixels deep, with
/// row 100 on the horizon.  The world here is `STRETCH` times taller than
/// Doom's, so a world slope is that much steeper than Doom's for the same
/// row.  Doom never looked up past the picture's top; here `SKY_FADE` rows
/// above it fade into the top row's average colour, which continues to the
/// zenith, as in GZDoom, rather than smearing the top row upwards.
pub(crate) fn classic(tex: &Tex) -> SkyPicture {
    let p = &tex.picture;
    let (w, h) = (p.width.max(1) as usize, p.height.max(1) as usize);
    let fade = (SKY_FADE as f64 * tex.scale.1).round().max(1.0) as usize;
    let top = &p.rgba[..(w * 4).min(p.rgba.len())];
    let mut average = [0u32; 3];
    for px in top.chunks_exact(4) {
        for c in 0..3 {
            average[c] += px[c] as u32;
        }
    }
    let average = average.map(|v| (v / (top.len() / 4).max(1) as u32) as f32);
    let mut rgba = Vec::with_capacity((h + fade) * w * 4);
    for row in 0..fade {
        let t = row as f32 / fade as f32;
        for px in top.chunks_exact(4) {
            for c in 0..3 {
                rgba.push((average[c] + (px[c] as f32 - average[c]) * t).round() as u8);
            }
            rgba.push(255);
        }
    }
    rgba.extend_from_slice(&p.rgba);
    // In the world's units: a scaled sky's display size.
    let (dw, dh) = (w as f64 / tex.scale.0, (h + fade) as f64 / tex.scale.1);
    let display_fade = fade as f64 / tex.scale.1;
    SkyPicture {
        picture: Picture { width: w as u32, height: (h + fade) as u32, rgba, holes: false },
        transform: UvTransform {
            offset: Vec2::new(0.0, ((100.0 + display_fade) / dh) as f32),
            rotation: 0.0,
            scale: Vec2::new((1024.0 / dw) as f32, (-160.0 / STRETCH as f64 / dh) as f32),
        },
        sampler: sampler(),
    }
}

/// A sky box resampled onto the cylinder.  `faces` are north, east, south,
/// west, top and bottom as GLDEFS lists them (or one picture wrapping all
/// four sides, then top and bottom); each side seen from inside is upright,
/// and the top has north at its top edge unless `flip_top`.  This follows
/// the cube GZDoom's `hw_skydome.cpp` draws.
pub(crate) fn from_box(faces: &[Arc<Picture>], flip_top: bool) -> Option<SkyPicture> {
    if faces.len() != 6 && faces.len() != 3 {
        return None;
    }
    let side = faces[0].width.max(faces[0].height).max(1);
    let per_face = if faces.len() == 6 { side } else { side / 4 };
    let w = (per_face * 4).clamp(64, BOX_WIDTH);
    let h = w / 2;
    let sample = |p: &Picture, u: f64, v: f64| -> [u8; 4] {
        let x = ((u * p.width as f64) as i64).clamp(0, p.width as i64 - 1) as usize;
        let y = ((v * p.height as f64) as i64).clamp(0, p.height as i64 - 1) as usize;
        let i = (y * p.width as usize + x) * 4;
        p.rgba.get(i..i + 4).and_then(|s| s.try_into().ok()).unwrap_or([0, 0, 0, 255])
    };
    let (top, bottom) = if faces.len() == 6 { (&faces[4], &faces[5]) } else { (&faces[1], &faces[2]) };
    // Each column's direction around the horizon, worked out once.
    let dirs: Vec<(f64, f64)> = (0..w)
        .map(|col| {
            let phi = (col as f64 + 0.5) / w as f64 * std::f64::consts::TAU;
            (phi.cos(), phi.sin())
        })
        .collect();
    let mut rgba = vec![0u8; w as usize * h as usize * 4];
    rgba.par_chunks_mut(w as usize * 4).enumerate().for_each(|(row, line)| {
        let t = BOX_REACH * (1.0 - 2.0 * (row as f64 + 0.5) / h as f64);
        for (&(dx, dy), out) in dirs.iter().zip(line.chunks_exact_mut(4)) {
            let dz = t;
            let (ax, ay, az) = (dx.abs(), dy.abs(), dz.abs());
            let px = if az >= ax && az >= ay {
                let u = (1.0 + dx / az) / 2.0;
                if dz > 0.0 {
                    let v = if flip_top { (1.0 + dy / az) / 2.0 } else { (1.0 - dy / az) / 2.0 };
                    sample(top, u, v)
                } else {
                    sample(bottom, u, (1.0 - dy / az) / 2.0)
                }
            } else {
                // The side face, and where on it, left to right as seen
                // from inside.
                let (k, u, major) = if ay >= ax {
                    if dy > 0.0 { (0, (1.0 + dx / ay) / 2.0, ay) } else { (2, (1.0 - dx / ay) / 2.0, ay) }
                } else if dx > 0.0 {
                    (1, (1.0 - dy / ax) / 2.0, ax)
                } else {
                    (3, (1.0 + dy / ax) / 2.0, ax)
                };
                let v = (1.0 - dz / major) / 2.0;
                if faces.len() == 6 { sample(&faces[k], u, v) } else { sample(&faces[0], (k as f64 + u) / 4.0, v) }
            };
            out.copy_from_slice(&[px[0], px[1], px[2], 255]);
        }
    });
    Some(SkyPicture {
        picture: Picture { width: w, height: h, rgba, holes: false },
        transform: UvTransform {
            offset: Vec2::new(0.0, 0.5),
            rotation: 0.0,
            scale: Vec2::new(1.0, (-0.5 / BOX_REACH) as f32),
        },
        sampler: sampler(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid(c: [u8; 3]) -> Arc<Picture> {
        let rgba = [c[0], c[1], c[2], 255].repeat(16 * 16);
        Arc::new(Picture { width: 16, height: 16, rgba, holes: false })
    }

    /// Each face lands where its direction says: east at no turn, north
    /// at a quarter turn anticlockwise, west at a half, south at three
    /// quarters, the top above the reach of the sides.
    #[test]
    fn box_faces_land_in_their_directions() {
        let faces: Vec<Arc<Picture>> =
            [[255, 0, 0], [0, 255, 0], [0, 0, 255], [255, 255, 0], [255, 255, 255], [0, 0, 0]]
                .into_iter()
                .map(solid)
                .collect();
        let s = from_box(&faces, false).unwrap();
        let p = &s.picture;
        let at = |turns: f64, tan: f64| {
            // Through the transform, as the renderer samples.
            let v = s.transform.offset.y as f64 + s.transform.scale.y as f64 * tan;
            let x = ((turns * p.width as f64) as usize).min(p.width as usize - 1);
            let y = ((v * p.height as f64) as usize).min(p.height as usize - 1);
            let i = (y * p.width as usize + x) * 4;
            [p.rgba[i], p.rgba[i + 1], p.rgba[i + 2]]
        };
        assert_eq!(at(0.25, 0.0), [255, 0, 0], "north");
        assert_eq!(at(0.0, 0.0), [0, 255, 0], "east");
        assert_eq!(at(0.75, 0.0), [0, 0, 255], "south");
        assert_eq!(at(0.5, 0.0), [255, 255, 0], "west");
        assert_eq!(at(0.1, 2.0), [255, 255, 255], "top");
        assert_eq!(at(0.1, -2.0), [0, 0, 0], "bottom");
        assert!(from_box(&faces[..2], false).is_none());
    }
}
