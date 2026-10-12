//! Materials, samplers, texture references and image decoding.

use crate::Ctx;
use crate::data::{Neighbours, Origins, ViewData, read_uri, uri_path};
use crate::json::{MaterialJ, Root, SamplerJ, TextureInfoJ};
use rayon::prelude::*;
use slerp_scene::glam::{Vec2, Vec3, Vec4};
use slerp_scene::textures::{DecodeOptions, GpuBlocks, Original, Texture};
use slerp_scene::{AlphaMode, Filter, Material, Sampler, TexRef, UvTransform, Wrap};
use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::Arc;

/*┌─────────────────╮
| Materials       |
└─────────────────╯*/
//region Materials

/// glTF's material for primitives that name none: white, fully metallic
/// and fully rough, as the specification defines it.
pub fn default_material() -> Material {
    Material { name: "default".into(), base_color: Vec4::ONE, metallic: 1.0, roughness: 1.0, ..Material::default() }
}

/// Every texture a material uses, by the file's texture index.
pub fn textures_of(m: &MaterialJ) -> impl Iterator<Item = usize> + '_ {
    let pbr = m.pbr_metallic_roughness.as_ref();
    let ext = &m.extensions;
    [
        pbr.and_then(|p| p.base_color_texture.as_ref()),
        pbr.and_then(|p| p.metallic_roughness_texture.as_ref()),
        m.normal_texture.as_ref(),
        m.occlusion_texture.as_ref(),
        m.emissive_texture.as_ref(),
        ext.spec_gloss.as_ref().and_then(|s| s.diffuse_texture.as_ref()),
        ext.transmission.as_ref().and_then(|t| t.transmission_texture.as_ref()),
        ext.volume.as_ref().and_then(|v| v.thickness_texture.as_ref()),
    ]
    .into_iter()
    .flatten()
    .filter_map(|t| t.index)
}

/// Turns a glTF material into the scene's.  Texture slots whose image
/// didn't load are left empty, so the material draws without them.
pub fn build_material(index: usize, m: &MaterialJ, refs: &TexRefs) -> Material {
    let pbr = m.pbr_metallic_roughness.as_ref();
    let ext = &m.extensions;
    let tex = |info: Option<&TextureInfoJ>| info.and_then(|i| refs.get(i));
    let mut out = Material {
        name: m.name.clone().unwrap_or_else(|| format!("material {index}")),
        base_color: Vec4::from_array(pbr.and_then(|p| p.base_color_factor).unwrap_or([1.0; 4])),
        base_color_texture: tex(pbr.and_then(|p| p.base_color_texture.as_ref())),
        metallic: unit(pbr.and_then(|p| p.metallic_factor), 1.0),
        roughness: unit(pbr.and_then(|p| p.roughness_factor), 1.0),
        metallic_roughness_texture: tex(pbr.and_then(|p| p.metallic_roughness_texture.as_ref())),
        normal_texture: tex(m.normal_texture.as_ref()),
        normal_scale: finite(m.normal_texture.as_ref().and_then(|t| t.scale), 1.0),
        occlusion_texture: tex(m.occlusion_texture.as_ref()),
        occlusion_strength: unit(m.occlusion_texture.as_ref().and_then(|t| t.strength), 1.0),
        emissive: Vec3::from_array(m.emissive_factor.unwrap_or([0.0; 3]))
            * finite(ext.emissive_strength.as_ref().and_then(|e| e.emissive_strength), 1.0).max(0.0),
        emissive_texture: tex(m.emissive_texture.as_ref()),
        alpha_mode: match m.alpha_mode.as_deref() {
            Some("MASK") => AlphaMode::Mask(finite(m.alpha_cutoff, 0.5)),
            Some("BLEND") => AlphaMode::Blend,
            _ => AlphaMode::Opaque,
        },
        double_sided: m.double_sided,
        unlit: ext.unlit.is_some(),
        ..Material::default()
    };
    if let Some(s) = &ext.specular {
        out.specular = finite(s.specular_factor, 1.0).max(0.0);
        out.specular_color = Vec3::from_array(s.specular_color_factor.unwrap_or([1.0; 3]));
    }
    if let Some(ior) = &ext.ior {
        out.ior = finite(ior.ior, 1.5).max(1.0);
    }
    if let Some(c) = &ext.clearcoat {
        out.clearcoat = unit(c.clearcoat_factor, 0.0);
        out.clearcoat_roughness = unit(c.clearcoat_roughness_factor, 0.0);
    }
    if let Some(t) = &ext.transmission {
        out.transmission = unit(t.transmission_factor, 0.0);
        out.transmission_texture = tex(t.transmission_texture.as_ref());
    }
    // KHR_materials_volume only means something on a transmissive
    // material, which it gives a thickness and a colour.
    if let Some(v) = ext.volume.as_ref().filter(|_| ext.transmission.is_some()) {
        out.thickness = finite(v.thickness_factor, 0.0).max(0.0);
        out.thickness_texture = tex(v.thickness_texture.as_ref());
        let color = Vec3::from_array(v.attenuation_color.unwrap_or([1.0; 3]));
        out.attenuation_color = if color.is_finite() { color.clamp(Vec3::ZERO, Vec3::ONE) } else { Vec3::ONE };
        // Absent, zero or negative distances mean no absorption.
        out.attenuation_distance =
            v.attenuation_distance.filter(|d| d.is_finite() && *d > 0.0).unwrap_or(f32::INFINITY);
    }
    // The retired specular-glossiness model, still common in older files,
    // becomes a dielectric with the diffuse colour as base colour.
    if let Some(sg) = &ext.spec_gloss {
        out.base_color = Vec4::from_array(sg.diffuse_factor.unwrap_or([1.0; 4]));
        out.base_color_texture = tex(sg.diffuse_texture.as_ref());
        out.metallic = 0.0;
        out.roughness = 1.0 - unit(sg.glossiness_factor, 1.0);
        out.metallic_roughness_texture = None;
    }
    // VRM's toon material is drawn as a matte dielectric.  Its files also
    // carry KHR_materials_unlit as a fallback for plain glTF viewers, which
    // would look flat, and leave metallic at glTF's default of 1.
    if ext.mtoon.is_some() {
        make_toon(&mut out);
    }
    if !out.base_color.is_finite() {
        out.base_color = Vec4::ONE;
    }
    if !out.emissive.is_finite() {
        out.emissive = Vec3::ZERO;
    }
    out
}

/// Turns a material into the lit stand-in for VRM's MToon.
pub fn make_toon(m: &mut Material) {
    m.unlit = false;
    m.metallic = 0.0;
    m.roughness = 0.9;
    m.metallic_roughness_texture = None;
}

fn finite(v: Option<f32>, default: f32) -> f32 {
    v.filter(|v| v.is_finite()).unwrap_or(default)
}

fn unit(v: Option<f32>, default: f32) -> f32 {
    finite(v, default).clamp(0.0, 1.0)
}

/// Resolves the file's texture references against the images that loaded.
pub struct TexRefs<'a> {
    pub root: &'a Root,
    /// Scene texture index for each of the file's textures.
    pub of_texture: Vec<Option<usize>>,
}

impl TexRefs<'_> {
    pub fn get(&self, info: &TextureInfoJ) -> Option<TexRef> {
        let t = info.index?;
        let mut r = TexRef::new((*self.of_texture.get(t)?)?);
        let transform = info.extensions.transform.as_ref();
        let uv = transform.and_then(|t| t.tex_coord).or(info.tex_coord).unwrap_or(0);
        r.uv_set = if uv <= 1 { uv as u8 } else { 0 };
        r.transform = transform.and_then(|t| {
            let tr = UvTransform {
                offset: Vec2::from_array(t.offset.unwrap_or([0.0; 2])),
                rotation: t.rotation.unwrap_or(0.0),
                scale: Vec2::from_array(t.scale.unwrap_or([1.0; 2])),
            };
            let identity = tr.offset == Vec2::ZERO && tr.rotation == 0.0 && tr.scale == Vec2::ONE;
            let finite = tr.offset.is_finite() && tr.rotation.is_finite() && tr.scale.is_finite();
            (!identity && finite).then_some(tr)
        });
        let sampler = self.root.textures.get(t).and_then(|t| t.sampler).and_then(|s| self.root.samplers.get(s));
        r.sampler = sampler.map(convert_sampler).unwrap_or_default();
        Some(r)
    }

    /// A plain reference to the file's texture `t`, for VRM 0.x's material
    /// properties, which name textures without any of glTF's options.
    pub fn plain(&self, t: usize) -> Option<TexRef> {
        self.get(&TextureInfoJ { index: Some(t), ..Default::default() })
    }
}

fn convert_sampler(s: &SamplerJ) -> Sampler {
    let wrap = |w: Option<u32>| match w {
        Some(33071) => Wrap::ClampToEdge,
        Some(33648) => Wrap::MirroredRepeat,
        _ => Wrap::Repeat,
    };
    let (min, mipmaps) = match s.min_filter {
        Some(9728) => (Filter::Nearest, false),
        Some(9729) => (Filter::Linear, false),
        Some(9984 | 9986) => (Filter::Nearest, true),
        _ => (Filter::Linear, true),
    };
    Sampler {
        wrap_u: wrap(s.wrap_s),
        wrap_v: wrap(s.wrap_t),
        mag: if s.mag_filter == Some(9728) { Filter::Nearest } else { Filter::Linear },
        min,
        mipmaps,
    }
}

//endregion

/*┌─────────────────╮
| Images          |
└─────────────────╯*/
//region Images

/// The images a texture may use, best first: a KTX2 (Basis), WebP or DDS
/// alternative named by an extension, then the core PNG or JPEG source.
pub fn image_candidates(root: &Root, texture: usize) -> Vec<usize> {
    let Some(t) = root.textures.get(texture) else { return Vec::new() };
    let e = &t.extensions;
    let mut out: Vec<usize> = Vec::new();
    let sources = [e.basisu.as_ref(), e.webp.as_ref(), e.dds.as_ref()].into_iter().flatten().filter_map(|s| s.source);
    for i in sources.chain(t.source) {
        if i < root.images.len() && !out.contains(&i) {
            out.push(i);
        }
    }
    out
}

/// Decoded images, and the scene texture each of the file's textures got.
pub struct Decoded {
    pub textures: Vec<Arc<Texture>>,
    pub of_texture: Vec<Option<usize>>,
}

/// Decodes the images that the textures marked in `used` need, in
/// parallel.  An image that fails is replaced by the texture's next
/// candidate, if it has one; a texture with none left is dropped with a
/// warning, so its material draws without it.  Basis textures stay in
/// blocks of the formats in `gpu` where they can.  Each texture records
/// its image's name and where its encoded bytes are (`origins`), so it can
/// be saved or written into another glTF unchanged.
#[allow(clippy::too_many_arguments)]
pub fn decode_images(
    root: &Root,
    used: &[bool],
    views: &[Option<ViewData>],
    neighbours: &Neighbours,
    origins: &Origins,
    opts: &DecodeOptions,
    gpu: GpuBlocks,
    ctx: &Ctx,
) -> Decoded {
    let candidates: Vec<Vec<usize>> =
        (0..root.textures.len()).map(|t| if used[t] { image_candidates(root, t) } else { Vec::new() }).collect();
    let mut results: HashMap<usize, Option<Arc<Texture>>> = HashMap::new();
    let mut chosen: Vec<Option<usize>> = vec![None; root.textures.len()];
    // Each round tries every unresolved texture's next candidate.
    for round in 0..4 {
        let wanted: Vec<usize> = {
            let mut w: Vec<usize> = candidates
                .iter()
                .enumerate()
                .filter(|&(t, _)| chosen[t].is_none())
                .filter_map(|(_, c)| c.get(round).copied())
                .filter(|i| !results.contains_key(i))
                .collect();
            w.sort_unstable();
            w.dedup();
            w
        };
        let decoded: Vec<(usize, Option<Arc<Texture>>)> = wanted
            .par_iter()
            .map(|&i| (i, decode_image(root, i, views, neighbours, origins, opts, gpu, ctx)))
            .collect();
        results.extend(decoded);
        for (t, c) in candidates.iter().enumerate() {
            if chosen[t].is_none()
                && let Some(&image) = c.get(round)
                && results.get(&image).is_some_and(|r| r.is_some())
            {
                chosen[t] = Some(image);
            }
        }
    }
    // Number the images that loaded in file order.
    let mut images: Vec<(usize, Arc<Texture>)> = results.into_iter().filter_map(|(i, r)| r.map(|t| (i, t))).collect();
    images.sort_unstable_by_key(|(i, _)| *i);
    let index_of: HashMap<usize, usize> = images.iter().enumerate().map(|(n, (i, _))| (*i, n)).collect();
    Decoded {
        of_texture: chosen.iter().map(|c| c.and_then(|i| index_of.get(&i).copied())).collect(),
        textures: images.into_iter().map(|(_, t)| t).collect(),
    }
}

#[allow(clippy::too_many_arguments)]
fn decode_image(
    root: &Root,
    index: usize,
    views: &[Option<ViewData>],
    neighbours: &Neighbours,
    origins: &Origins,
    opts: &DecodeOptions,
    gpu: GpuBlocks,
    ctx: &Ctx,
) -> Option<Arc<Texture>> {
    let image = root.images.get(index)?;
    let name = image.name.clone().or_else(|| image.uri.clone().filter(|u| !u.starts_with("data:")));
    let what = match name {
        Some(n) => format!("image {index} ({n})"),
        None => format!("image {index}"),
    };
    let (bytes, original): (Cow<[u8]>, _) = match (&image.buffer_view, &image.uri) {
        (Some(v), _) => match views.get(*v).and_then(|v| v.as_ref()) {
            Some(data) => (Cow::Borrowed(data.bytes()), origins.of_view(root, *v, data)),
            None => {
                ctx.warn(format!("{what} has no data"));
                return None;
            }
        },
        (None, Some(uri)) => {
            let bytes = read_uri(uri, neighbours, ctx, &what)?;
            let original = match uri_path(uri, neighbours) {
                Some(path) => Some(Original::File { path, offset: 0, len: bytes.len() as u64 }),
                None => origins.memory.then(|| Original::Memory(bytes.as_slice().into())),
            };
            (Cow::Owned(bytes), original)
        }
        (None, None) => {
            ctx.warn(format!("{what} has no data"));
            return None;
        }
    };
    let decoded = slerp_scene::textures::decode_for_gpu(&bytes, opts, gpu);
    match decoded {
        Ok(mut texture) => {
            texture.source.name = image_name(image);
            texture.source.original = original;
            Some(Arc::new(texture))
        }
        Err(e) => {
            ctx.warn(format!("{what} couldn't be decoded: {e}"));
            None
        }
    }
}

/// An image's own name, or the file name its URI gives.
fn image_name(image: &crate::json::ImageJ) -> String {
    if let Some(name) = image.name.as_ref().filter(|n| !n.is_empty()) {
        return name.clone();
    }
    let uri = image.uri.as_deref().filter(|u| !u.starts_with("data:")).unwrap_or("");
    let decoded = String::from_utf8_lossy(&crate::data::percent_decode(uri)).replace('\\', "/");
    decoded.rsplit('/').next().unwrap_or("").to_string()
}

//endregion

#[cfg(test)]
mod tests {
    use super::*;
    use slerp_scene::glam::Vec3 as V3;

    /// KHR_texture_transform's matrix as the Khronos sample renderer builds
    /// it.  The README's GLSL lists the rotation's rows (cos, sin) and
    /// (-sin, cos); read as GLSL's column-major constructor it would turn the
    /// other way, but the sample renderer, three.js and the
    /// TextureTransformTest model all follow the row reading.
    fn spec_matrix(offset: Vec2, rotation: f32, scale: Vec2) -> slerp_scene::glam::Mat3 {
        use slerp_scene::glam::Mat3;
        let t = Mat3::from_cols(V3::new(1.0, 0.0, 0.0), V3::new(0.0, 1.0, 0.0), V3::new(offset.x, offset.y, 1.0));
        let (s, c) = rotation.sin_cos();
        let r = Mat3::from_cols(V3::new(c, -s, 0.0), V3::new(s, c, 0.0), V3::new(0.0, 0.0, 1.0));
        let sc = Mat3::from_cols(V3::new(scale.x, 0.0, 0.0), V3::new(0.0, scale.y, 0.0), V3::Z);
        t * r * sc
    }

    #[test]
    fn uv_transform_matches_the_specification() {
        let tr = UvTransform {
            offset: Vec2::new(0.5, -0.25),
            rotation: std::f32::consts::FRAC_PI_8,
            scale: Vec2::new(2.0, 3.0),
        };
        let ours = tr.matrix();
        let spec = spec_matrix(tr.offset, tr.rotation, tr.scale);
        assert!(ours.abs_diff_eq(spec, 1e-6), "{ours:?} vs {spec:?}");
        // A quarter turn maps (1, 0) to (0, -1): the rotation is clockwise
        // in UV space, as the extension's README describes.
        let quarter = UvTransform { offset: Vec2::ZERO, rotation: std::f32::consts::FRAC_PI_2, scale: Vec2::ONE };
        let p = quarter.matrix() * V3::new(1.0, 0.0, 1.0);
        assert!((p - V3::new(0.0, -1.0, 1.0)).length() < 1e-6, "{p:?}");
    }

    #[test]
    fn samplers() {
        let s = convert_sampler(&SamplerJ {
            mag_filter: Some(9728),
            min_filter: Some(9986),
            wrap_s: Some(33071),
            wrap_t: Some(33648),
        });
        assert_eq!(s.mag, Filter::Nearest);
        assert_eq!((s.min, s.mipmaps), (Filter::Nearest, true));
        assert_eq!((s.wrap_u, s.wrap_v), (Wrap::ClampToEdge, Wrap::MirroredRepeat));
        assert_eq!(convert_sampler(&SamplerJ::default()), Sampler::default());
    }
}
