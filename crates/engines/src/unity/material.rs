//! Unity materials (`.mat` files and Material objects in scenes) mapped to
//! Slerp's metallic-roughness model.  The built-in pipeline's Standard
//! shader, URP's Lit and Simple Lit and HDRP's Lit are read by their
//! property names; other shaders (Shader Graph, custom ones) are guessed
//! from theirs.
//!
//! Unity packs material data differently from glTF: smoothness rather than
//! roughness, in the alpha of the metallic map (or of the albedo), and
//! HDRP's mask map holds metallic, occlusion and smoothness together.  Such
//! textures are repacked into the channels glTF uses, once per combination.

use super::yaml::Yaml;
use slerp_scene::glam::{Vec2, Vec3, Vec4};
use slerp_scene::textures::{self, ColorSpace, DecodeOptions, Pixels, Texture};
use slerp_scene::{AlphaMode, Filter, Limits, Material, Sampler, TexRef, UvTransform, Wrap};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Shaders known by GUID: URP's and HDRP's unlit ones.
const UNLIT_SHADERS: &[&str] = &["650dd9526735d5b46b79224bc6e94025", "c4edd00ff2db5b24391a4fcb1762e459"];

/// Shaders whose property names this module knows: Unity's built-in ones
/// (Standard and the legacy shaders), URP's Lit, Simple Lit, Baked Lit and
/// Unlit, HDRP's Lit and Unlit.
const KNOWN_SHADERS: &[&str] = &[
    "0000000000000000f000000000000000",
    "933532a4fcc9baf4fa0491de14d08ed7",
    "8d2bb70cbf9db8d4da26e15b26e74248",
    "0ca6dca7396eb48e5849247ffd444914",
    "650dd9526735d5b46b79224bc6e94025",
    "6e4ae4064600d784cac1e41a9e6f2e59",
    "c4edd00ff2db5b24391a4fcb1762e459",
];

/// A texture slot as a material file names it.
pub(crate) struct TexEnv {
    pub guid: Option<String>,
    pub scale: Vec2,
    pub offset: Vec2,
}

/// A material file's properties, by name.
#[derive(Default)]
pub(crate) struct MatProps {
    pub name: String,
    pub shader_guid: String,
    pub shader_id: i64,
    pub keywords: Vec<String>,
    pub textures: Vec<(String, TexEnv)>,
    pub floats: Vec<(String, f32)>,
    pub colors: Vec<(String, Vec4)>,
}

impl MatProps {
    /// Reads the properties of a Material object.
    pub fn read(doc: &Yaml) -> MatProps {
        let mut p = MatProps {
            name: doc.get("m_Name").str().unwrap_or("").to_string(),
            shader_guid: doc.at("m_Shader.guid").str().unwrap_or("").to_string(),
            shader_id: doc.at("m_Shader.fileID").i64().unwrap_or(0),
            ..Default::default()
        };
        p.keywords = doc.get("m_ShaderKeywords").str().unwrap_or("").split_whitespace().map(str::to_string).collect();
        for k in doc.get("m_ValidKeywords").seq() {
            if let Some(k) = k.str() {
                p.keywords.push(k.to_string());
            }
        }
        let saved = doc.get("m_SavedProperties");
        // Each entry is `- _Name: value` (serialization 3) or
        // `- first: {name: _Name} second: value` (older).
        let entries = |list: &str| -> Vec<(String, Yaml)> {
            saved
                .get(list)
                .seq()
                .iter()
                .filter_map(|item| {
                    if let Some(name) = item.at("first.name").str() {
                        return Some((name.to_string(), item.get("second").clone()));
                    }
                    item.map().first().map(|(k, v)| (k.clone(), v.clone()))
                })
                .collect()
        };
        for (name, v) in entries("m_TexEnvs") {
            let guid = v.at("m_Texture.guid").str().map(str::to_string).filter(|g| !g.bytes().all(|b| b == b'0'));
            let file_id = v.at("m_Texture.fileID").i64().unwrap_or(0);
            let xy = |key: &str, d: f32| {
                Vec2::new(v.at(&format!("{key}.x")).f32().unwrap_or(d), v.at(&format!("{key}.y")).f32().unwrap_or(d))
            };
            p.textures.push((
                name,
                TexEnv { guid: guid.filter(|_| file_id != 0), scale: xy("m_Scale", 1.0), offset: xy("m_Offset", 0.0) },
            ));
        }
        for list in ["m_Floats", "m_Ints"] {
            for (name, v) in entries(list) {
                if let Some(f) = v.f32() {
                    p.floats.push((name, f));
                }
            }
        }
        for (name, v) in entries("m_Colors") {
            let c = |k: &str| v.get(k).f32().unwrap_or(1.0);
            p.colors.push((name, Vec4::new(c("r"), c("g"), c("b"), c("a"))));
        }
        p
    }

    fn float(&self, name: &str) -> Option<f32> {
        self.floats.iter().rev().find(|(n, _)| n == name).map(|(_, v)| *v)
    }

    fn color(&self, name: &str) -> Option<Vec4> {
        self.colors.iter().rev().find(|(n, _)| n == name).map(|(_, v)| *v)
    }

    fn texture(&self, name: &str) -> Option<&TexEnv> {
        self.textures.iter().find(|(n, t)| n == name && t.guid.is_some()).map(|(_, t)| t)
    }

    fn has_keyword(&self, k: &str) -> bool {
        self.keywords.iter().any(|x| x == k)
    }

    /// The first texture or colour whose name contains one of `words` and
    /// none of `not`, for shaders with their own property names.
    fn guess_texture(&self, words: &[&str], not: &[&str]) -> Option<&TexEnv> {
        self.textures
            .iter()
            .filter(|(_, t)| t.guid.is_some())
            .find(|(n, _)| {
                let n = n.to_ascii_lowercase();
                words.iter().any(|w| n.contains(w)) && !not.iter().any(|w| n.contains(w))
            })
            .map(|(_, t)| t)
    }

    /// A colour by name for other shaders: those whose alpha is zero are
    /// masks or highlights, not the surface's colour.
    fn guess_color(&self, words: &[&str]) -> Option<Vec4> {
        self.colors
            .iter()
            .filter(|(_, c)| c.w > 0.0)
            .find(|(n, _)| {
                let n = n.to_ascii_lowercase();
                words.iter().any(|w| n.contains(w)) && !n.contains("emiss") && !n.contains("spec")
            })
            .map(|(_, c)| *c)
    }
}

fn srgb_to_linear(c: f32) -> f32 {
    if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
}

fn linear_rgb(c: Vec4) -> Vec3 {
    Vec3::new(srgb_to_linear(c.x), srgb_to_linear(c.y), srgb_to_linear(c.z))
}

/// How a repacked texture is made.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum Pack {
    /// The file as it is, in the given colour space.
    Plain(PathBuf, bool),
    /// glTF's metallic-roughness texture: metallic in blue from `metal`'s
    /// red (or 1 without), roughness in green from 1 - smoothness, read
    /// from the alpha of `smooth`, scaled; the remaps are stored as
    /// integers in thousandths so the key can be hashed.
    MetalRough { metal: Option<PathBuf>, smooth: Option<PathBuf>, remap: [i32; 4] },
    /// Occlusion moved from green to red.
    Occlusion(PathBuf),
}

/// Textures decoded and repacked for one load, shared by every material.
#[derive(Default)]
pub(crate) struct TextureCache {
    decoded: HashMap<PathBuf, Option<Arc<Texture>>>,
    packed: HashMap<Pack, Option<usize>>,
    pub warnings: Vec<String>,
}

/// What building a material needs from the scene loader.
pub(crate) trait Assets {
    /// The file a GUID names.
    fn asset(&mut self, guid: &str) -> Option<PathBuf>;
    /// Adds a texture to the output scene.
    fn add_texture(&mut self, texture: Arc<Texture>) -> usize;
    fn limits(&self) -> &Limits;
    fn texture_meta(&mut self, path: &Path) -> Option<std::rc::Rc<Yaml>>;
}

impl TextureCache {
    fn decode(&mut self, path: &Path, limits: &Limits) -> Option<Arc<Texture>> {
        if let Some(t) = self.decoded.get(path) {
            return t.clone();
        }
        slerp_scene::record_neighbour(path);
        let opts = match limits.max_texture_size {
            Some(n) => DecodeOptions::preview(n),
            None => DecodeOptions::full(),
        };
        let tiff = path.extension().is_some_and(|e| e.eq_ignore_ascii_case("tif") || e.eq_ignore_ascii_case("tiff"));
        let result = if !limits.neighbours {
            Err("neighbouring files are off".to_string())
        } else {
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                if tiff {
                    decode_tiff(path, limits.max_texture_size)
                } else {
                    textures::decode_file(path, &opts).map_err(|e| e.to_string())
                }
            }))
            .unwrap_or_else(|_| Err("the decoder failed".into()))
        };
        let t = match result {
            Ok(t) => Some(Arc::new(t)),
            Err(e) => {
                let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                if self.warnings.len() < 20 {
                    self.warnings.push(format!("texture {name}: {e}"));
                }
                None
            }
        };
        self.decoded.insert(path.to_path_buf(), t.clone());
        t
    }

    fn get(&mut self, pack: Pack, out: &mut dyn Assets) -> Option<usize> {
        if let Some(&t) = self.packed.get(&pack) {
            return t;
        }
        let limits = out.limits().clone();
        let made: Option<Texture> = match &pack {
            Pack::Plain(path, srgb) => self.decode(path, &limits).map(|t| {
                let mut t = (*t).clone();
                t.color_space = if *srgb { ColorSpace::Srgb } else { ColorSpace::Linear };
                t
            }),
            Pack::MetalRough { metal, smooth, remap } => {
                let m = metal.as_ref().and_then(|p| self.decode(p, &limits));
                let s = smooth.as_ref().and_then(|p| self.decode(p, &limits));
                let r = remap.map(|v| v as f32 / 1000.0);
                combine(m.as_deref(), s.as_deref(), |metal, smooth| {
                    let metallic = r[0] + (r[1] - r[0]) * metal[0];
                    let smoothness = r[2] + (r[3] - r[2]) * smooth[3];
                    [255.0, (1.0 - smoothness) * 255.0, metallic * 255.0, 255.0]
                })
            }
            Pack::Occlusion(path) => {
                let t = self.decode(path, &limits);
                combine(t.as_deref(), None, |a, _| [a[1] * 255.0, a[1] * 255.0, a[1] * 255.0, 255.0])
            }
        };
        let index = made.map(|t| out.add_texture(Arc::new(t)));
        self.packed.insert(pack, index);
        index
    }
}

/// Reads a TIFF picture (8 or 16 bits, grey or colour, with or without
/// alpha), scaled down to `max_size` when given.
fn decode_tiff(path: &Path, max_size: Option<u32>) -> Result<Texture, String> {
    use tiff::ColorType;
    use tiff::decoder::{Decoder, DecodingResult};
    let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let mut dec = Decoder::new(std::io::BufReader::new(file)).map_err(|e| e.to_string())?;
    let (w, h) = dec.dimensions().map_err(|e| e.to_string())?;
    let channels = match dec.colortype().map_err(|e| e.to_string())? {
        ColorType::Gray(_) => 1,
        ColorType::GrayA(_) => 2,
        ColorType::RGB(_) => 3,
        ColorType::RGBA(_) => 4,
        other => return Err(format!("TIFF colour type {other:?} isn't read")),
    };
    let values: Vec<u8> = match dec.read_image().map_err(|e| e.to_string())? {
        DecodingResult::U8(v) => v,
        DecodingResult::U16(v) => v.iter().map(|x| (x >> 8) as u8).collect(),
        DecodingResult::F32(v) => v.iter().map(|x| (x.clamp(0.0, 1.0) * 255.0) as u8).collect(),
        _ => return Err("TIFF sample format isn't read".into()),
    };
    let n = (w as usize).checked_mul(h as usize).ok_or("TIFF too large")?;
    if w == 0 || h == 0 || values.len() < n * channels {
        return Err("TIFF data cut short".into());
    }
    // Nearest-texel downscale to the size asked for.
    let scale = max_size.map_or(1.0, |m| (m as f32 / w.max(h) as f32).min(1.0));
    let (ow, oh) = (((w as f32 * scale) as u32).max(1), ((h as f32 * scale) as u32).max(1));
    let mut out = Vec::with_capacity((ow * oh * 4) as usize);
    for y in 0..oh {
        let sy = (y as u64 * h as u64 / oh as u64) as usize;
        for x in 0..ow {
            let sx = (x as u64 * w as u64 / ow as u64) as usize;
            let p = &values[(sy * w as usize + sx) * channels..][..channels];
            out.extend_from_slice(&match channels {
                1 => [p[0], p[0], p[0], 255],
                2 => [p[0], p[0], p[0], p[1]],
                3 => [p[0], p[1], p[2], 255],
                _ => [p[0], p[1], p[2], p[3]],
            });
        }
    }
    Ok(Texture::from_rgba8(ow, oh, out, ColorSpace::Unspecified))
}

/// The first level of a texture as 8-bit RGBA.
fn rgba8(t: &Texture) -> Option<(u32, u32, Vec<u8>)> {
    let t = if textures::is_compressed(t) { textures::decompress(t) } else { t.clone() };
    let level = t.levels.first()?;
    let px = match level.surfaces.first()? {
        Pixels::Rgba8(v) => v.clone(),
        Pixels::Rgba32F(v) => v.iter().map(|f| (f.clamp(0.0, 1.0) * 255.0 + 0.5) as u8).collect(),
        Pixels::Blocks(..) => return None,
    };
    (px.len() == (level.width * level.height * 4) as usize).then_some((level.width, level.height, px))
}

/// Builds a texture from up to two others texel by texel, `f` getting
/// their RGBA as 0 to 1 (the second sampled at the first's resolution) and
/// returning 0 to 255.
fn combine(a: Option<&Texture>, b: Option<&Texture>, f: impl Fn([f32; 4], [f32; 4]) -> [f32; 4]) -> Option<Texture> {
    let a = a.and_then(rgba8);
    let b = b.and_then(rgba8);
    let (w, h) = match (&a, &b) {
        (Some(a), _) => (a.0, a.1),
        (None, Some(b)) => (b.0, b.1),
        _ => return None,
    };
    let sample = |img: &Option<(u32, u32, Vec<u8>)>, x: u32, y: u32| -> [f32; 4] {
        match img {
            Some((iw, ih, px)) => {
                let (sx, sy) =
                    ((x as u64 * *iw as u64 / w as u64) as usize, (y as u64 * *ih as u64 / h as u64) as usize);
                let i = (sy * *iw as usize + sx) * 4;
                [px[i] as f32 / 255.0, px[i + 1] as f32 / 255.0, px[i + 2] as f32 / 255.0, px[i + 3] as f32 / 255.0]
            }
            None => [1.0; 4],
        }
    };
    let mut out = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            let v = f(sample(&a, x, y), sample(&b, x, y));
            out.extend(v.iter().map(|c| c.clamp(0.0, 255.0) as u8));
        }
    }
    Some(Texture::from_rgba8(w, h, out, ColorSpace::Linear))
}

/// The sampler a texture's import settings ask for.
fn sampler(meta: Option<&Yaml>) -> Sampler {
    let mut s = Sampler::default();
    let Some(meta) = meta else { return s };
    let ti = meta.get("TextureImporter");
    let settings = ti.get("textureSettings");
    let wrap = |v: Option<i64>| match v {
        Some(1) => Wrap::ClampToEdge,
        Some(2) | Some(3) => Wrap::MirroredRepeat,
        _ => Wrap::Repeat,
    };
    let both = settings.get("wrapMode").i64();
    s.wrap_u = wrap(settings.get("wrapU").i64().filter(|&v| v >= 0).or(both));
    s.wrap_v = wrap(settings.get("wrapV").i64().filter(|&v| v >= 0).or(both));
    if settings.get("filterMode").i64() == Some(0) {
        s.mag = Filter::Nearest;
        s.min = Filter::Nearest;
    }
    s
}

/// Builds the material described by `p`, decoding its textures into
/// `out`.  `cache` is shared by the whole load.
pub(crate) fn build(p: &MatProps, cache: &mut TextureCache, out: &mut dyn Assets) -> Material {
    let mut m = Material { name: p.name.clone(), ..Default::default() };
    let hdrp =
        p.texture("_BaseColorMap").is_some() || p.texture("_MaskMap").is_some() || p.float("_SurfaceType").is_some();
    let urp =
        !hdrp && (p.texture("_BaseMap").is_some() || p.color("_BaseColor").is_some() || p.float("_Surface").is_some());
    let known = p.shader_guid.is_empty() || KNOWN_SHADERS.contains(&p.shader_guid.as_str());
    let custom = !known || (!hdrp && !urp && p.texture("_MainTex").is_none() && p.color("_Color").is_none());

    // 1. Base colour.  Other shaders' colours with zero alpha are passed
    //    over for the same reason as in `guess_color`.
    let named = |n: &str| p.color(n).filter(|c| known || c.w > 0.0);
    let base_color = named("_BaseColor")
        .or_else(|| named("_Color"))
        .or_else(|| if custom { p.guess_color(&["albedo", "base", "main", "color", "tint", "diffuse"]) } else { None });
    if let Some(c) = base_color {
        m.base_color = linear_rgb(c).extend(c.w.clamp(0.0, 1.0));
    } else {
        m.base_color = Vec4::ONE;
    }
    let base =
        p.texture("_BaseColorMap").or_else(|| p.texture("_BaseMap")).or_else(|| p.texture("_MainTex")).or_else(|| {
            if custom {
                p.guess_texture(
                    &["albedo", "base", "main", "diffuse", "color", "tex"],
                    &["normal", "bump", "mask", "emiss", "metal", "rough", "occl", "height", "detail"],
                )
            } else {
                None
            }
        });
    let transform = base.and_then(|t| uv_transform(t.scale, t.offset));
    let tex_ref =
        |cache: &mut TextureCache, out: &mut dyn Assets, env: Option<&TexEnv>, srgb: bool| -> Option<TexRef> {
            let env = env?;
            let path = out.asset(env.guid.as_deref()?)?;
            let meta = out.texture_meta(&path);
            let index = cache.get(Pack::Plain(path, srgb), out)?;
            Some(TexRef { texture: index, uv_set: 0, transform, sampler: sampler(meta.as_deref()) })
        };
    let base_path = base.and_then(|t| t.guid.as_deref()).and_then(|g| out.asset(g));
    m.base_color_texture = tex_ref(cache, out, base, true);

    // 2. Metal and roughness.
    let mut metallic = p.float("_Metallic").unwrap_or(0.0);
    let mut smoothness = p.float("_Smoothness").or_else(|| p.float("_Glossiness")).unwrap_or(0.5);
    let mask = p.texture("_MaskMap");
    let metal_map = p.texture("_MetallicGlossMap");
    let albedo_alpha = p.float("_SmoothnessTextureChannel").unwrap_or(0.0) >= 0.5;
    let packed = if hdrp && mask.is_some() {
        let r = |k: &str, d: f32| p.float(k).unwrap_or(d);
        let remap = [
            r("_MetallicRemapMin", 0.0),
            r("_MetallicRemapMax", 1.0),
            r("_SmoothnessRemapMin", 0.0),
            r("_SmoothnessRemapMax", 1.0),
        ];
        let path = mask.and_then(|t| t.guid.as_deref()).and_then(|g| out.asset(g));
        // The mask map's green is occlusion.
        if let Some(p) = &path {
            m.occlusion_texture = cache.get(Pack::Occlusion(p.clone()), out).map(|i| TexRef {
                texture: i,
                uv_set: 0,
                transform,
                sampler: Sampler::default(),
            });
        }
        path.map(|p| {
            (
                Pack::MetalRough { metal: Some(p.clone()), smooth: Some(p), remap: remap.map(|v| (v * 1000.0) as i32) },
                1.0,
                1.0,
            )
        })
    } else if metal_map.is_some() || albedo_alpha {
        // A metallic map replaces the metallic value; smoothness comes from
        // its alpha (or the albedo's) times the smoothness value.
        let metal = metal_map.and_then(|t| t.guid.as_deref()).and_then(|g| out.asset(g));
        let scale = if metal.is_some() {
            p.float("_GlossMapScale").or_else(|| p.float("_Smoothness")).unwrap_or(1.0)
        } else {
            smoothness
        };
        let smooth = if albedo_alpha { base_path.clone() } else { metal.clone() };
        let metal_factor = if metal.is_some() { 1.0 } else { metallic };
        Some((Pack::MetalRough { metal, smooth, remap: [0, 1000, 0, (scale * 1000.0) as i32] }, metal_factor, 1.0))
    } else {
        None
    };
    if let Some((pack, metal_factor, rough_factor)) = packed
        && let Some(i) = cache.get(pack, out)
    {
        m.metallic_roughness_texture = Some(TexRef { texture: i, uv_set: 0, transform, sampler: Sampler::default() });
        metallic = metal_factor;
        smoothness = 1.0 - rough_factor;
    }
    m.metallic = metallic.clamp(0.0, 1.0);
    m.roughness = (1.0 - smoothness).clamp(0.0, 1.0);

    // 3. Normals, occlusion, emission.
    let normal = p
        .texture("_NormalMap")
        .or_else(|| p.texture("_BumpMap"))
        .or_else(|| if custom { p.guess_texture(&["normal", "bump"], &["detail"]) } else { None });
    m.normal_texture = tex_ref(cache, out, normal, false);
    m.normal_scale = p.float("_NormalScale").or_else(|| p.float("_BumpScale")).unwrap_or(1.0);
    if m.occlusion_texture.is_none() {
        m.occlusion_texture = tex_ref(cache, out, p.texture("_OcclusionMap"), false);
        m.occlusion_strength = p.float("_OcclusionStrength").unwrap_or(1.0);
    }
    if hdrp {
        if let Some(c) = p.color("_EmissiveColor") {
            m.emissive = c.truncate().max(Vec3::ZERO);
        }
        m.emissive_texture = tex_ref(cache, out, p.texture("_EmissiveColorMap"), true);
    } else if p.has_keyword("_EMISSION")
        || p.texture("_EmissionMap").is_some() && p.float("_EmissionEnabled").unwrap_or(0.0) > 0.0
    {
        let c = p.color("_EmissionColor").unwrap_or(Vec4::ZERO);
        m.emissive = if c.truncate().max_element() > 1.0 { c.truncate() } else { linear_rgb(c) };
        m.emissive_texture = tex_ref(cache, out, p.texture("_EmissionMap"), true);
    }
    if m.emissive_texture.is_some() && m.emissive == Vec3::ZERO {
        m.emissive_texture = None;
    }

    // 4. Transparency, culling, lighting.
    let cutoff = p.float("_Cutoff").or_else(|| p.float("_AlphaCutoff")).unwrap_or(0.5);
    m.alpha_mode = if hdrp {
        if p.float("_AlphaCutoffEnable").unwrap_or(0.0) > 0.5 {
            AlphaMode::Mask(cutoff)
        } else if p.float("_SurfaceType").unwrap_or(0.0) > 0.5 {
            AlphaMode::Blend
        } else {
            AlphaMode::Opaque
        }
    } else if urp || p.float("_Surface").is_some() {
        if p.float("_AlphaClip").unwrap_or(0.0) > 0.5 {
            AlphaMode::Mask(cutoff)
        } else if p.float("_Surface").unwrap_or(0.0) > 0.5 {
            AlphaMode::Blend
        } else {
            AlphaMode::Opaque
        }
    } else {
        match p.float("_Mode").unwrap_or(0.0) as i32 {
            1 => AlphaMode::Mask(cutoff),
            2 | 3 => AlphaMode::Blend,
            _ if p.has_keyword("_ALPHATEST_ON") => AlphaMode::Mask(cutoff),
            _ => AlphaMode::Opaque,
        }
    };
    m.double_sided = p.float("_DoubleSidedEnable").unwrap_or(0.0) > 0.5 || p.float("_Cull").is_some_and(|c| c < 0.5);
    let builtin_unlit = p.shader_guid == "0000000000000000f000000000000000" && (10750..=10755).contains(&p.shader_id);
    m.unlit = UNLIT_SHADERS.contains(&p.shader_guid.as_str()) || builtin_unlit;
    m
}

/// A Unity texture transform (scale and offset with V up from the bottom)
/// in glTF's terms (V down from the top), or None when it is the identity.
fn uv_transform(scale: Vec2, offset: Vec2) -> Option<UvTransform> {
    if !scale.is_finite() || !offset.is_finite() || (scale == Vec2::ONE && offset == Vec2::ZERO) {
        return None;
    }
    Some(UvTransform { offset: Vec2::new(offset.x, 1.0 - scale.y - offset.y), rotation: 0.0, scale })
}

#[cfg(test)]
mod tests {
    use super::super::yaml;
    use super::*;

    #[test]
    fn reads_standard_and_urp_properties() {
        let text = "Material:\n  m_Name: Red\n  m_Shader: {fileID: 4800000, guid: 933532a4fcc9baf4fa0491de14d08ed7, type: 3}\n  m_ShaderKeywords: _EMISSION\n  m_SavedProperties:\n    serializedVersion: 3\n    m_TexEnvs:\n    - _BaseMap:\n        m_Texture: {fileID: 0}\n        m_Scale: {x: 2, y: 2}\n        m_Offset: {x: 0, y: 0}\n    m_Floats:\n    - _Metallic: 0.25\n    - _Smoothness: 0.75\n    - _Surface: 1\n    m_Colors:\n    - _BaseColor: {r: 1, g: 0, b: 0, a: 0.5}\n    - _EmissionColor: {r: 2, g: 0, b: 0, a: 1}\n";
        let doc = yaml::parse_doc(text).unwrap();
        let p = MatProps::read(&doc);
        struct NoFiles(Limits);
        impl Assets for NoFiles {
            fn asset(&mut self, _: &str) -> Option<PathBuf> {
                None
            }
            fn add_texture(&mut self, _: Arc<Texture>) -> usize {
                0
            }
            fn limits(&self) -> &Limits {
                &self.0
            }
            fn texture_meta(&mut self, _: &Path) -> Option<std::rc::Rc<Yaml>> {
                None
            }
        }
        let m = build(&p, &mut TextureCache::default(), &mut NoFiles(Limits::thumbnail()));
        assert_eq!(m.name, "Red");
        assert_eq!(m.base_color, Vec4::new(1.0, 0.0, 0.0, 0.5));
        assert_eq!((m.metallic, m.roughness), (0.25, 0.25));
        assert_eq!(m.alpha_mode, AlphaMode::Blend);
        assert_eq!(m.emissive, Vec3::new(2.0, 0.0, 0.0));
        let t = uv_transform(Vec2::splat(2.0), Vec2::ZERO).unwrap();
        assert_eq!(t.offset, Vec2::new(0.0, -1.0));
    }
}
