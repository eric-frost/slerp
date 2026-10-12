//! A Source model's materials: each texture name is looked up as a `.vmt`
//! in the model's material folders (`$cdmaterials`, such as
//! `models/props_c17/`), then by its own path, in the game's `materials/`
//! folders and VPK packs as a map's materials are (see `editor::vmt`).
//! The base texture and normal map are `.vtf` files found the same way.

use crate::editor::vmt::{self, Library, SourceMaterial};
use rayon::prelude::*;
use slerp_scene::glam::{Vec3, Vec4};
use slerp_scene::textures::{ColorSpace, Pixels, Texture};
use slerp_scene::{AlphaMode, Limits, Material, Scene, TexRef};
use std::path::Path;
use std::sync::Arc;

/// What was found for one of the model's texture names.
pub struct Found {
    /// The name as the model stores it.
    pub name: String,
    /// The material's path below `materials/`, when its `.vmt` was found.
    pub path: Option<String>,
    pub vmt: Option<SourceMaterial>,
    pub base: Option<Arc<Texture>>,
    pub bump: Option<Arc<Texture>>,
}

/// A material path as Source writes them, made comparable: forward
/// slashes, lower case, no leading slash, no `.vmt`.
fn clean(path: &str) -> String {
    let p = path.trim().replace('\\', "/").to_ascii_lowercase();
    let p = p.trim_start_matches('/');
    p.strip_suffix(".vmt").unwrap_or(p).replace("//", "/")
}

/// Finds every texture's material and reads its textures (on this thread,
/// so the files are recorded for watching), then decodes the textures in
/// parallel.  `max` caps texture sizes; normal maps are only read when
/// `normals`.
pub fn find(
    model: &Path,
    names: &[String],
    folders: &[String],
    limits: &Limits,
    normals: bool,
) -> (Vec<Found>, Library) {
    let lib = Library::new(model, limits);
    struct Read {
        name: String,
        path: Option<String>,
        vmt: Option<SourceMaterial>,
        base: Option<Vec<u8>>,
        bump: Option<Vec<u8>>,
    }
    let read: Vec<Read> = names
        .iter()
        .map(|name| {
            let mut tried: Vec<String> = folders.iter().map(|f| clean(&format!("{}/{name}", f.trim()))).collect();
            tried.push(clean(name));
            let found = if lib.is_empty() {
                None
            } else {
                tried.iter().find_map(|p| vmt::material(&lib, p).map(|m| (p.clone(), m)))
            };
            let (path, vmt) = match found {
                Some((p, m)) => (Some(p), Some(m)),
                None => (None, None),
            };
            let tex = |t: &Option<String>| t.as_ref().and_then(|t| lib.read(&format!("materials/{t}.vtf")));
            let (base, bump) = match &vmt {
                Some(m) => (tex(&m.base), if normals { tex(&m.bump) } else { None }),
                None => (None, None),
            };
            Read { name: name.clone(), path, vmt, base, bump }
        })
        .collect();
    let max = limits.max_texture_size;
    let found = read
        .into_par_iter()
        .map(|r| Found {
            base: r.base.as_ref().and_then(|b| crate::vtf::decode(b, max).ok()).map(Arc::new),
            bump: r.bump.as_ref().and_then(|b| crate::vtf::decode(b, max).ok()).map(direct3d_normals).map(Arc::new),
            name: r.name,
            path: r.path,
            vmt: r.vmt,
        })
        .collect();
    (found, lib)
}

/// Turns a Direct3D normal map (green pointing down the texture) into the
/// OpenGL convention the renderer uses, and marks it as data.
fn direct3d_normals(mut t: Texture) -> Texture {
    for level in &mut t.levels {
        for surface in &mut level.surfaces {
            if let Pixels::Rgba8(px) = surface {
                for p in px.chunks_exact_mut(4) {
                    p[1] = 255 - p[1];
                }
            }
        }
    }
    t.color_space = ColorSpace::Linear;
    t
}

/// Adds a scene material (and its textures) for what was found.
pub fn add(scene: &mut Scene, found: &Found) -> usize {
    let mut texture = |t: &Arc<Texture>| {
        scene.textures.push(t.clone());
        TexRef::new(scene.textures.len() - 1)
    };
    let mut m = Material {
        name: found.path.clone().unwrap_or_else(|| found.name.clone()),
        base_color: Vec4::ONE,
        roughness: 0.7,
        specular: 0.5,
        ..Default::default()
    };
    match &found.base {
        Some(t) => m.base_color_texture = Some(texture(t)),
        // Eyes draw their iris from a projection this loader doesn't
        // make; a missing texture is a light grey.
        None => m.base_color = Vec4::new(0.7, 0.7, 0.7, 1.0),
    }
    if let Some(n) = &found.bump {
        m.normal_texture = Some(texture(n));
    }
    if let Some(v) = &found.vmt {
        m.unlit = v.unlit();
        m.double_sided = v.nocull;
        if let Some(cut) = v.alphatest {
            m.alpha_mode = AlphaMode::Mask(cut);
            m.double_sided = true;
        }
        if v.translucent && !v.selfillum {
            m.alpha_mode = AlphaMode::Blend;
        }
        if v.additive {
            // No additive blending in the scene model: drawn unlit, with
            // brightness standing in for opacity where the texture has no
            // alpha of its own.
            m.unlit = true;
        }
        if v.alpha < 1.0 {
            m.alpha_mode = AlphaMode::Blend;
            m.base_color.w = v.alpha;
        }
        if v.selfillum
            && let Some(glow) = found.base.as_deref().and_then(vmt::self_illumination)
        {
            m.emissive = Vec3::ONE;
            m.emissive_texture = Some(texture(&Arc::new(glow)));
        }
    }
    scene.materials.push(m);
    scene.materials.len() - 1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_are_cleaned() {
        assert_eq!(clean("Models\\Props_C17\\/Furniture01A"), "models/props_c17/furniture01a");
        assert_eq!(clean("\\models\\x.vmt"), "models/x");
    }

    #[test]
    fn normals_flip_green() {
        let t = direct3d_normals(Texture::from_rgba8(1, 1, vec![128, 0, 255, 255], ColorSpace::Srgb));
        let Pixels::Rgba8(p) = t.base() else { panic!() };
        assert_eq!(p[..], [128, 255, 255, 255]);
        assert_eq!(t.color_space, ColorSpace::Linear);
    }
}
