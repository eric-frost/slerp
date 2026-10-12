//! Skies for `Effect::PanoramaSky`: a cylinder around the viewer whose u
//! counts turns anticlockwise from east (+X) and whose v, through the
//! texture transform, is the tangent of the elevation.
//!
//! Sky boxes (Quake 2, Half-Life, source ports and Source) are six
//! pictures named `rt`, `bk`, `lf`, `ft`, `up` and `dn`, laid on a cube as
//! Quake 2's `R_DrawSkyBox` lays them, and resampled onto the cylinder
//! here.  Without one, a plain gradient stands in.

use rayon::prelude::*;
use slerp_scene::glam::{DVec3, Vec2};
use slerp_scene::textures::{ColorSpace, Pixels, Texture};
use slerp_scene::{Filter, Sampler, TexRef, UvTransform, Wrap};

/// How far up and down the cylinder reaches, as the tangent of the
/// elevation (about 68 degrees).  Beyond that its edge rows continue.
const REACH: f64 = 2.5;

/// Sky box face names in Quake 2's order: +X, +Y, -X, -Y, +Z, -Z.
pub(crate) const SUFFIXES: [&str; 6] = ["rt", "bk", "lf", "ft", "up", "dn"];

/// How the panorama texture is sampled.
pub(crate) fn sky_ref(texture: usize) -> TexRef {
    TexRef {
        texture,
        uv_set: 0,
        transform: Some(UvTransform {
            offset: Vec2::new(0.0, 0.5),
            rotation: 0.0,
            scale: Vec2::new(1.0, (-0.5 / REACH) as f32),
        }),
        sampler: Sampler {
            wrap_u: Wrap::Repeat,
            wrap_v: Wrap::ClampToEdge,
            mag: Filter::Linear,
            min: Filter::Linear,
            mipmaps: true,
        },
    }
}

/// A sky from light blue overhead to a pale horizon, and grey below it.
pub(crate) fn gradient() -> Texture {
    let (w, h) = (4u32, 128u32);
    let mut rgba = Vec::with_capacity((w * h * 4) as usize);
    for row in 0..h {
        let t = REACH * (1.0 - 2.0 * (row as f64 + 0.5) / h as f64);
        let c: [f64; 3] = if t >= 0.0 {
            let k = (t / REACH).powf(0.5);
            let (top, horizon) = ([70.0, 120.0, 200.0], [200.0, 220.0, 235.0]);
            [0, 1, 2].map(|i| horizon[i] + (top[i] - horizon[i]) * k)
        } else {
            let k = (-t / REACH).min(1.0);
            let (horizon, low) = ([170.0, 175.0, 180.0], [110.0, 112.0, 115.0]);
            [0, 1, 2].map(|i| horizon[i] + (low[i] - horizon[i]) * k)
        };
        for _ in 0..w {
            rgba.extend_from_slice(&[c[0] as u8, c[1] as u8, c[2] as u8, 255]);
        }
    }
    let mut t = Texture::from_rgba8(w, h, rgba, ColorSpace::Srgb);
    t.source.container = "sky gradient".into();
    t
}

/// A face's texels as 8-bit RGBA, whatever it decoded to.
fn rgba8(t: &Texture) -> Option<(usize, usize, Vec<u8>)> {
    let (w, h) = (t.width() as usize, t.height() as usize);
    let data = match t.base() {
        Pixels::Rgba8(v) => v.clone(),
        Pixels::Rgba32F(v) => v.iter().map(|&f| (f.clamp(0.0, 1.0).powf(1.0 / 2.2) * 255.0 + 0.5) as u8).collect(),
        Pixels::Blocks(..) => return None,
    };
    (data.len() >= w * h * 4 && w > 0 && h > 0).then_some((w, h, data))
}

/// Resamples a sky box (faces in `SUFFIXES` order) onto the cylinder, at
/// most `max` texels wide.
pub(crate) fn panorama(faces: &[Texture; 6], max: u32) -> Option<Texture> {
    let faces: Vec<(usize, usize, Vec<u8>)> = faces.iter().map(rgba8).collect::<Option<_>>()?;
    let side = faces[0].0.max(faces[0].1) as u32;
    let w = (side * 4).clamp(64, max.clamp(64, 4096));
    let h = w / 2;
    let sample = |face: usize, s: f64, t: f64| -> [u8; 4] {
        let (fw, fh, data) = &faces[face];
        // Bilinear, clamped to the face.
        let x = (s * *fw as f64 - 0.5).clamp(0.0, *fw as f64 - 1.0);
        let y = (t * *fh as f64 - 0.5).clamp(0.0, *fh as f64 - 1.0);
        let (x0, y0) = (x.floor() as usize, y.floor() as usize);
        let (x1, y1) = ((x0 + 1).min(fw - 1), (y0 + 1).min(fh - 1));
        let (fx, fy) = (x - x0 as f64, y - y0 as f64);
        let at = |xx: usize, yy: usize, c: usize| data[(yy * fw + xx) * 4 + c] as f64;
        let mut out = [255u8; 4];
        for (c, o) in out.iter_mut().enumerate().take(3) {
            let top = at(x0, y0, c) * (1.0 - fx) + at(x1, y0, c) * fx;
            let bottom = at(x0, y1, c) * (1.0 - fx) + at(x1, y1, c) * fx;
            *o = (top * (1.0 - fy) + bottom * fy).round() as u8;
        }
        out
    };
    let mut rgba = vec![0u8; w as usize * h as usize * 4];
    rgba.par_chunks_mut(w as usize * 4).enumerate().for_each(|(row, line)| {
        let up = REACH * (1.0 - 2.0 * (row as f64 + 0.5) / h as f64);
        for (col, out) in line.chunks_exact_mut(4).enumerate() {
            let phi = (col as f64 + 0.5) / w as f64 * std::f64::consts::TAU;
            let d = DVec3::new(phi.cos(), phi.sin(), up);
            // Quake 2's `vec_to_st`: the face of the largest component,
            // and where on it.
            let a = d.abs();
            let (face, s, t) = if a.x >= a.y && a.x >= a.z {
                if d.x > 0.0 { (0, -d.y / d.x, d.z / d.x) } else { (2, -d.y / d.x, -d.z / d.x) }
            } else if a.y >= a.z {
                if d.y > 0.0 { (1, d.x / d.y, d.z / d.y) } else { (3, d.x / d.y, -d.z / d.y) }
            } else if d.z > 0.0 {
                (4, -d.y / d.z, -d.x / d.z)
            } else {
                (5, d.y / d.z, -d.x / d.z)
            };
            // Texture rows run downwards.
            out.copy_from_slice(&sample(face, (s + 1.0) * 0.5, 1.0 - (t + 1.0) * 0.5));
        }
    });
    let mut t = Texture::from_rgba8(w, h, rgba, ColorSpace::Srgb);
    t.source.container = "sky box".into();
    Some(t)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid(c: [u8; 3]) -> Texture {
        Texture::from_rgba8(8, 8, [c[0], c[1], c[2], 255].repeat(64), ColorSpace::Srgb)
    }

    /// Each face lands in its direction: rt east, bk north, lf west, ft
    /// south, up overhead, dn below.
    #[test]
    fn faces_land_where_quake_2_puts_them() {
        let colors = [[255, 0, 0], [0, 255, 0], [0, 0, 255], [255, 255, 0], [255, 255, 255], [0, 0, 0]];
        let faces = colors.map(solid);
        let p = panorama(&faces, 256).unwrap();
        let r = sky_ref(0).transform.unwrap();
        let Pixels::Rgba8(px) = p.base() else { panic!() };
        let at = |turns: f64, tan: f64| {
            let v = r.offset.y as f64 + r.scale.y as f64 * tan;
            let x = ((turns * p.width() as f64) as usize).min(p.width() as usize - 1);
            let y = ((v * p.height() as f64) as usize).min(p.height() as usize - 1);
            let i = (y * p.width() as usize + x) * 4;
            [px[i], px[i + 1], px[i + 2]]
        };
        assert_eq!(at(0.01, 0.0), colors[0], "east");
        assert_eq!(at(0.25, 0.0), colors[1], "north");
        assert_eq!(at(0.5, 0.0), colors[2], "west");
        assert_eq!(at(0.75, 0.0), colors[3], "south");
        assert_eq!(at(0.1, 2.4), colors[4], "up");
        assert_eq!(at(0.1, -2.4), colors[5], "down");
        assert!(gradient().width() > 0);
    }
}
