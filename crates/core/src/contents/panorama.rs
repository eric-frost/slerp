//! A cube map as one equirectangular panorama: a picture twice as wide as
//! it is high, whose columns go once around the horizon and whose rows run
//! from straight up to straight down, the way HDR skies are shared.
//!
//! Both mappings are the renderer's own, so a panorama made here lights a
//! scene in Slerp exactly as the cube did.  Faces are read in the order
//! +X, -X, +Y, -Y, +Z, -Z with the orientation of Vulkan and OpenGL cube
//! maps, which KTX2 and DDS files store (`cube_dir` in the renderer's
//! `ibl.rs` and `ibl.wgsl`), and the panorama's column `u` and row `v` (0
//! to 1) look along `(sin θ cos φ, cos θ, sin θ sin φ)` with `φ = (u - 0.5)
//! 2π` and `θ = v π`, which is how the renderer reads an HDR sky
//! (`cs_equirect`): the middle column faces +X, and turning right from it
//! faces +Z.

use super::{ContentsError, Result};
use rayon::prelude::*;
use slerp_textures::{Pixels, Texture, TextureKind};

/// The widest panorama made from 8-bit faces.
pub(crate) const MAX_WIDTH: u32 = 8192;
/// The widest made from floating-point faces, whose pixels take four times
/// the memory (4096 x 2048 is 128 MB of floats).
const MAX_WIDTH_FLOAT: u32 = 4096;

/// Whether `t` is a cube map whose layers can be made into panoramas.
pub(crate) fn is_cube(t: &Texture) -> bool {
    t.kind == TextureKind::Cube && t.faces == 6 && t.levels.first().is_some_and(|l| l.width > 0 && l.height > 0)
}

/// The size of the panorama of `t`'s cubes, at most `max_width` wide: four
/// faces around, which keeps the faces' detail at the horizon.
pub(crate) fn size(t: &Texture, max_width: u32) -> (u32, u32) {
    let face = t.levels.first().map_or(1, |l| l.width.max(1));
    let cap = if is_float(t) { MAX_WIDTH_FLOAT } else { MAX_WIDTH };
    let width = face.saturating_mul(4).min(max_width).min(cap).max(4) & !1;
    (width, width / 2)
}

fn is_float(t: &Texture) -> bool {
    t.levels.first().and_then(|l| l.surfaces.first()).is_some_and(Pixels::is_hdr)
}

/// The panorama of array layer `layer` of cube map `t`, at most
/// `max_width` wide, read from the smallest mip level that still has the
/// detail.  Floating-point faces give a floating-point panorama, others an
/// 8-bit one in the faces' colour space.
pub(crate) fn panorama(t: &Texture, layer: u32, max_width: u32) -> Result<Texture> {
    if !is_cube(t) {
        return Err(ContentsError::Unsupported("only a cube map makes a panorama".into()));
    }
    if layer >= t.layers.max(1) {
        return Err(ContentsError::NoSuchItem);
    }
    let (width, height) = size(t, max_width);
    // 1. The smallest level whose four faces around are at least as wide.
    let level = t.levels.iter().rposition(|l| l.width.saturating_mul(4) >= width).unwrap_or(0) as u32;
    let mut faces = Vec::with_capacity(6);
    for f in 0..6 {
        let (w, h, pixels) = super::save::surface(t, level, layer, f)?;
        if pixels.texel_count() < w as usize * h as usize {
            return Err(ContentsError::Convert(format!("face {f} of the cube map is incomplete")));
        }
        faces.push(Face { w, h, pixels });
    }
    let float = faces.iter().all(|f| f.pixels.is_hdr());

    // 2. Every pixel of the panorama, row by row in parallel.
    let row = width as usize * 4;
    let sample = |x: u32, y: u32| -> [f32; 4] {
        let u = (x as f32 + 0.5) / width as f32;
        let v = (y as f32 + 0.5) / height as f32;
        let (face, fu, fv) = face_of(direction(u, v));
        faces[face].bilinear(fu, fv)
    };
    let mut out = if float {
        let mut px = vec![0f32; row * height as usize];
        px.par_chunks_mut(row).enumerate().for_each(|(y, line)| {
            for x in 0..width {
                let c = sample(x, y as u32);
                line[x as usize * 4..x as usize * 4 + 4].copy_from_slice(&c);
            }
        });
        Texture::from_rgba32f(width, height, px)
    } else {
        let mut px = vec![0u8; row * height as usize];
        px.par_chunks_mut(row).enumerate().for_each(|(y, line)| {
            for x in 0..width {
                let c = sample(x, y as u32);
                for (k, value) in c.iter().enumerate() {
                    line[x as usize * 4 + k] = (value * 255.0 + 0.5).clamp(0.0, 255.0) as u8;
                }
            }
        });
        Texture::from_rgba8(width, height, px, t.color_space)
    };
    out.color_space = t.color_space;
    out.source.container = "panorama".into();
    Ok(out)
}

/// The direction panorama point (`u`, `v`) looks along, as the renderer
/// reads skies.
pub(crate) fn direction(u: f32, v: f32) -> glam::Vec3 {
    let phi = (u - 0.5) * std::f32::consts::TAU;
    let theta = v * std::f32::consts::PI;
    glam::Vec3::new(theta.sin() * phi.cos(), theta.cos(), theta.sin() * phi.sin())
}

/// The cube face direction `d` meets, and where on it, from -1 to 1 with
/// `v` downwards: the inverse of the renderer's `cube_dir`.
pub(crate) fn face_of(d: glam::Vec3) -> (usize, f32, f32) {
    let a = d.abs();
    if a.x >= a.y && a.x >= a.z && a.x > 0.0 {
        if d.x > 0.0 { (0, -d.z / a.x, -d.y / a.x) } else { (1, d.z / a.x, -d.y / a.x) }
    } else if a.y >= a.z && a.y > 0.0 {
        if d.y > 0.0 { (2, d.x / a.y, d.z / a.y) } else { (3, d.x / a.y, -d.z / a.y) }
    } else if a.z > 0.0 {
        if d.z > 0.0 { (4, d.x / a.z, -d.y / a.z) } else { (5, -d.x / a.z, -d.y / a.z) }
    } else {
        (0, 0.0, 0.0)
    }
}

/// One face's pixels, decoded.
struct Face {
    w: u32,
    h: u32,
    pixels: Pixels,
}

impl Face {
    /// The face at (`u`, `v`), each from -1 to 1, blended from the four
    /// nearest texels; edges are held, as the GPU's clamp to edge does.
    fn bilinear(&self, u: f32, v: f32) -> [f32; 4] {
        let x = ((u + 1.0) * 0.5 * self.w as f32 - 0.5).clamp(0.0, (self.w - 1) as f32);
        let y = ((v + 1.0) * 0.5 * self.h as f32 - 0.5).clamp(0.0, (self.h - 1) as f32);
        let (x0, y0) = (x.floor() as u32, y.floor() as u32);
        let (x1, y1) = ((x0 + 1).min(self.w - 1), (y0 + 1).min(self.h - 1));
        let (fx, fy) = (x - x0 as f32, y - y0 as f32);
        let (a, b, c, d) = (self.texel(x0, y0), self.texel(x1, y0), self.texel(x0, y1), self.texel(x1, y1));
        std::array::from_fn(|k| {
            let top = a[k] + (b[k] - a[k]) * fx;
            let bottom = c[k] + (d[k] - c[k]) * fx;
            top + (bottom - top) * fy
        })
    }

    fn texel(&self, x: u32, y: u32) -> [f32; 4] {
        let i = (y as usize * self.w as usize + x as usize) * 4;
        match &self.pixels {
            Pixels::Rgba8(v) => std::array::from_fn(|k| v.get(i + k).map_or(0.0, |&b| b as f32 / 255.0)),
            Pixels::Rgba32F(v) => {
                std::array::from_fn(|k| v.get(i + k).copied().filter(|f| f.is_finite()).unwrap_or(0.0))
            }
            Pixels::Blocks(..) => [0.0; 4],
        }
    }
}
