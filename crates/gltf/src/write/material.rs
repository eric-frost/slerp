//! Materials, textures, samplers and images.
//!
//! The scene's material is glTF's metallic-roughness model with the
//! extensions the loader reads, so most of it maps one to one.  What
//! doesn't: a Quake lightmap becomes the occlusion map (glTF has no
//! lightmaps, and an occlusion map darkens the same places), skies and
//! sprites become unlit, and fog and height maps are left out.

use super::WriteError;
use super::Writer;
use super::bin::{num, nums};
use super::image::{self, Encoded};
use super::mesh::MaterialKey;
use rayon::prelude::*;
use serde_json::{Map, Value, json};
use slerp_scene::glam::{Vec3, Vec4};
use slerp_scene::{AlphaMode, Effect, Filter, Material, Sampler, Scene, TexRef, Wrap};
use std::collections::HashMap;

/// Which texture a slot holds, and how glTF names it.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Slot {
    BaseColor,
    MetallicRoughness,
    Normal,
    Occlusion,
    Emissive,
    Transmission,
    Thickness,
}

impl Slot {
    /// Base colour and emissive pictures are sRGB; the rest are data.
    fn is_color(self) -> bool {
        matches!(self, Slot::BaseColor | Slot::Emissive)
    }
}

/// The textures a material writes, by slot.  A lightmap fills the occlusion
/// slot when that is free.
fn slots(m: &Material) -> Vec<(Slot, TexRef)> {
    let occlusion = m.occlusion_texture.or(m.lightmap_texture);
    [
        (Slot::BaseColor, m.base_color_texture),
        (Slot::MetallicRoughness, m.metallic_roughness_texture),
        (Slot::Normal, m.normal_texture),
        (Slot::Occlusion, occlusion),
        (Slot::Emissive, m.emissive_texture),
        (Slot::Transmission, m.transmission_texture.filter(|_| m.transmission > 0.0)),
        (Slot::Thickness, m.thickness_texture.filter(|_| m.transmission > 0.0)),
    ]
    .into_iter()
    .filter_map(|(s, t)| t.map(|t| (s, t)))
    .collect()
}

/// The UV sets a material's textures read.
pub fn uv_sets(scene: &Scene, key: MaterialKey) -> [bool; 2] {
    let mut sets = [false; 2];
    if let MaterialKey::Scene(i) = key {
        for (_, t) in slots(&scene.materials[i]) {
            if t.texture < scene.textures.len() {
                sets[(t.uv_set as usize).min(1)] = true;
            }
        }
    }
    sets
}

/// Writes the materials `keys` name and the textures they use.  Returns
/// each key's material index in the file.
pub fn write(w: &mut Writer, keys: &[MaterialKey]) -> Result<Vec<usize>, WriteError> {
    let scene = w.scene;

    // 1. The pictures, and whether each holds colours.
    let mut colour: HashMap<usize, bool> = HashMap::new();
    for &key in keys {
        if let MaterialKey::Scene(i) = key {
            for (slot, t) in slots(&scene.materials[i]) {
                if t.texture < scene.textures.len() {
                    *colour.entry(t.texture).or_default() |= slot.is_color();
                }
            }
        }
    }
    let mut pictures: Vec<(usize, bool)> = colour.into_iter().collect();
    pictures.sort_unstable();

    // 2. Encoded in parallel: PNG compression is the slowest step of a
    //    conversion.
    let originals = w.options.original_images;
    let encoded: Vec<(usize, Result<Encoded, String>)> =
        pictures.par_iter().map(|&(t, colour)| (t, image::encode(&scene.textures[t], colour, originals))).collect();
    let mut images: HashMap<usize, (usize, Option<&'static str>)> = HashMap::new();
    for (t, result) in encoded {
        match result {
            Ok(e) => {
                if let Some(warning) = e.warning {
                    w.warn(warning);
                }
                let view = w.bin.view(&e.bytes, None)?;
                let mut image = Map::new();
                image.insert("bufferView".into(), view.into());
                image.insert("mimeType".into(), e.mime.into());
                let name = &scene.textures[t].source.name;
                if !name.is_empty() {
                    image.insert("name".into(), name.clone().into());
                }
                w.images.push(Value::Object(image));
                images.insert(t, (w.images.len() - 1, e.extension));
                if let Some(ext) = e.extension {
                    w.extension(ext, true);
                }
            }
            Err(e) => w.warn(format!("texture {t} couldn't be saved: {e}")),
        }
    }

    // 3. The materials.
    let mut textures = Textures { images, of: HashMap::new(), samplers: HashMap::new() };
    let mut notes = Notes::default();
    let mut out = Vec::with_capacity(keys.len());
    for &key in keys {
        let json = match key {
            MaterialKey::Scene(i) => material(w, &scene.materials[i], &mut textures, &mut notes),
            MaterialKey::Default => {
                let m = Material { name: "default".into(), ..Material::default() };
                material(w, &m, &mut textures, &mut notes)
            }
            MaterialKey::Points => {
                w.extension("KHR_materials_unlit", false);
                json!({
                    "name": "points",
                    "pbrMetallicRoughness": { "metallicFactor": 0 },
                    "extensions": { "KHR_materials_unlit": {} },
                })
            }
        };
        w.materials.push(json);
        out.push(w.materials.len() - 1);
    }
    notes.report(w);
    Ok(out)
}

/// Things worth one warning each, however many materials have them.
#[derive(Default)]
struct Notes {
    lightmaps: bool,
    lightmaps_lost: bool,
    skies: bool,
    fog: bool,
    height: bool,
}

impl Notes {
    fn report(&self, w: &mut Writer) {
        if self.lightmaps {
            w.warn("lightmaps were saved as occlusion maps, since glTF has no lightmaps");
        }
        if self.lightmaps_lost {
            w.warn("lightmaps of materials that already have an occlusion map were left out");
        }
        if self.skies {
            w.warn("skies were saved as plain unlit surfaces");
        }
        if self.fog {
            w.warn("fog was left out, since glTF has none");
        }
        if self.height {
            w.warn("height maps were left out, since glTF has none");
        }
    }
}

/*┌─────────────────╮
| Materials       |
└─────────────────╯*/
//region Materials

fn material(w: &mut Writer, m: &Material, tex: &mut Textures, notes: &mut Notes) -> Value {
    let mut o = Map::new();
    if !m.name.is_empty() {
        o.insert("name".into(), m.name.clone().into());
    }
    let mut ext = Map::new();
    let info = |w: &mut Writer, tex: &mut Textures, r: Option<TexRef>| r.and_then(|r| tex.info(w, &r));

    // 1. Metallic-roughness.
    let mut pbr = Map::new();
    let base = clean4(m.base_color, Vec4::ONE).clamp(Vec4::ZERO, Vec4::ONE);
    if base != Vec4::ONE {
        pbr.insert("baseColorFactor".into(), nums(&base.to_array()));
    }
    if let Some(t) = info(w, tex, m.base_color_texture) {
        pbr.insert("baseColorTexture".into(), t);
    }
    let metallic = unit(m.metallic, 1.0);
    if metallic != 1.0 {
        pbr.insert("metallicFactor".into(), num(metallic));
    }
    let roughness = unit(m.roughness, 1.0);
    if roughness != 1.0 {
        pbr.insert("roughnessFactor".into(), num(roughness));
    }
    if let Some(t) = info(w, tex, m.metallic_roughness_texture) {
        pbr.insert("metallicRoughnessTexture".into(), t);
    }
    if !pbr.is_empty() {
        o.insert("pbrMetallicRoughness".into(), pbr.into());
    }

    // 2. Normal, occlusion (or the lightmap) and emissive.
    if let Some(mut t) = info(w, tex, m.normal_texture) {
        if m.normal_scale.is_finite() && m.normal_scale != 1.0 {
            t["scale"] = num(m.normal_scale);
        }
        o.insert("normalTexture".into(), t);
    }
    let lightmap = m.occlusion_texture.is_none() && m.lightmap_texture.is_some();
    notes.lightmaps_lost |= m.occlusion_texture.is_some() && m.lightmap_texture.is_some();
    let occlusion = if lightmap { m.lightmap_texture } else { m.occlusion_texture };
    if let Some(mut t) = info(w, tex, occlusion) {
        let strength = if lightmap { 1.0 } else { unit(m.occlusion_strength, 1.0) };
        if strength != 1.0 {
            t["strength"] = num(strength);
        }
        if lightmap {
            notes.lightmaps = true;
            let scale = if m.lightmap_scale.is_finite() { m.lightmap_scale } else { 1.0 };
            o.insert("extras".into(), json!({ "lightmap": { "scale": num(scale) } }));
        }
        o.insert("occlusionTexture".into(), t);
    }
    let emissive = clean3(m.emissive, Vec3::ZERO).max(Vec3::ZERO);
    let strength = emissive.max_element();
    if strength > 0.0 {
        let factor = if strength > 1.0 { emissive / strength } else { emissive };
        o.insert("emissiveFactor".into(), nums(&factor.to_array()));
        if strength > 1.0 {
            ext.insert("KHR_materials_emissive_strength".into(), json!({ "emissiveStrength": num(strength) }));
        }
    }
    if let Some(t) = info(w, tex, m.emissive_texture) {
        o.insert("emissiveTexture".into(), t);
    }

    // 3. Alpha and sides.
    match m.alpha_mode {
        AlphaMode::Opaque => {}
        AlphaMode::Mask(cutoff) => {
            o.insert("alphaMode".into(), "MASK".into());
            let cutoff = if cutoff.is_finite() { cutoff.max(0.0) } else { 0.5 };
            if cutoff != 0.5 {
                o.insert("alphaCutoff".into(), num(cutoff));
            }
        }
        AlphaMode::Blend => {
            o.insert("alphaMode".into(), "BLEND".into());
        }
    }
    let billboard = m.effect == Effect::Billboard;
    if m.double_sided || billboard {
        o.insert("doubleSided".into(), true.into());
    }

    // 4. Extensions.
    notes.skies |= m.effect.is_sky();
    notes.fog |= m.fog.is_some();
    notes.height |= m.height_texture.is_some() && m.height_scale != 0.0;
    if m.unlit || billboard || m.effect.is_sky() {
        ext.insert("KHR_materials_unlit".into(), json!({}));
    }
    let specular = finite(m.specular, 1.0).max(0.0);
    let specular_color = clean3(m.specular_color, Vec3::ONE).max(Vec3::ZERO);
    if specular != 1.0 || specular_color != Vec3::ONE {
        let mut s = Map::new();
        if specular != 1.0 {
            s.insert("specularFactor".into(), num(specular));
        }
        if specular_color != Vec3::ONE {
            s.insert("specularColorFactor".into(), nums(&specular_color.to_array()));
        }
        ext.insert("KHR_materials_specular".into(), s.into());
    }
    let ior = finite(m.ior, 1.5);
    if ior != 1.5 && (ior >= 1.0 || ior == 0.0) {
        ext.insert("KHR_materials_ior".into(), json!({ "ior": num(ior) }));
    }
    let clearcoat = unit(m.clearcoat, 0.0);
    if clearcoat > 0.0 {
        let mut c = Map::new();
        c.insert("clearcoatFactor".into(), num(clearcoat));
        let r = unit(m.clearcoat_roughness, 0.0);
        if r > 0.0 {
            c.insert("clearcoatRoughnessFactor".into(), num(r));
        }
        ext.insert("KHR_materials_clearcoat".into(), c.into());
    }
    let transmission = unit(m.transmission, 0.0);
    if transmission > 0.0 {
        let mut t = Map::new();
        t.insert("transmissionFactor".into(), num(transmission));
        if let Some(info) = info(w, tex, m.transmission_texture) {
            t.insert("transmissionTexture".into(), info);
        }
        ext.insert("KHR_materials_transmission".into(), t.into());
        let mut v = Map::new();
        let thickness = finite(m.thickness, 0.0).max(0.0);
        if thickness > 0.0 {
            v.insert("thicknessFactor".into(), num(thickness));
        }
        if let Some(info) = info(w, tex, m.thickness_texture) {
            v.insert("thicknessTexture".into(), info);
        }
        if m.attenuation_distance.is_finite() && m.attenuation_distance > 0.0 {
            v.insert("attenuationDistance".into(), num(m.attenuation_distance));
        }
        let color = clean3(m.attenuation_color, Vec3::ONE).clamp(Vec3::ZERO, Vec3::ONE);
        if color != Vec3::ONE {
            v.insert("attenuationColor".into(), nums(&color.to_array()));
        }
        if !v.is_empty() {
            ext.insert("KHR_materials_volume".into(), v.into());
        }
    }
    // An unlit surface ignores every other lighting extension, and
    // validators warn about the pair.
    if ext.contains_key("KHR_materials_unlit") {
        ext.retain(|name, _| name == "KHR_materials_unlit");
    }
    for name in ext.keys() {
        w.extension(extension_name(name), false);
    }
    if !ext.is_empty() {
        o.insert("extensions".into(), ext.into());
    }
    Value::Object(o)
}

/// The static name of a material extension written above.
fn extension_name(name: &str) -> &'static str {
    const NAMES: [&str; 8] = [
        "KHR_materials_clearcoat",
        "KHR_materials_emissive_strength",
        "KHR_materials_ior",
        "KHR_materials_specular",
        "KHR_materials_transmission",
        "KHR_materials_unlit",
        "KHR_materials_volume",
        "KHR_texture_transform",
    ];
    NAMES.into_iter().find(|n| *n == name).unwrap_or("KHR_materials_unlit")
}

fn finite(v: f32, default: f32) -> f32 {
    if v.is_finite() { v } else { default }
}

fn unit(v: f32, default: f32) -> f32 {
    finite(v, default).clamp(0.0, 1.0)
}

fn clean3(v: Vec3, default: Vec3) -> Vec3 {
    if v.is_finite() { v } else { default }
}

fn clean4(v: Vec4, default: Vec4) -> Vec4 {
    if v.is_finite() { v } else { default }
}

//endregion

/*┌─────────────────╮
| Textures        |
└─────────────────╯*/
//region Textures

/// glTF textures are an image with a sampler; Slerp's texture references
/// carry their own sampler, so each pair used becomes one texture.
struct Textures {
    /// Each written scene texture's image, and the extension it needs.
    images: HashMap<usize, (usize, Option<&'static str>)>,
    of: HashMap<(usize, SamplerKey), usize>,
    samplers: HashMap<SamplerKey, usize>,
}

type SamplerKey = (u8, u8, u8, u8, bool);

fn sampler_key(s: &Sampler) -> SamplerKey {
    let wrap = |w: Wrap| w as u8;
    (wrap(s.wrap_u), wrap(s.wrap_v), s.mag as u8, s.min as u8, s.mipmaps)
}

impl Textures {
    /// The texture-info JSON for a reference, or `None` when its picture
    /// wasn't written.
    fn info(&mut self, w: &mut Writer, r: &TexRef) -> Option<Value> {
        let &(image, extension) = self.images.get(&r.texture)?;
        let key = sampler_key(&r.sampler);
        let index = match self.of.get(&(r.texture, key)) {
            Some(&i) => i,
            None => {
                let mut t = Map::new();
                if r.sampler != Sampler::default() {
                    let s = match self.samplers.get(&key) {
                        Some(&s) => s,
                        None => {
                            w.samplers.push(sampler(&r.sampler));
                            self.samplers.insert(key, w.samplers.len() - 1);
                            w.samplers.len() - 1
                        }
                    };
                    t.insert("sampler".into(), s.into());
                }
                match extension {
                    Some(ext) => {
                        let mut e = Map::new();
                        e.insert(ext.into(), json!({ "source": image }));
                        t.insert("extensions".into(), e.into());
                    }
                    None => {
                        t.insert("source".into(), image.into());
                    }
                }
                w.textures.push(Value::Object(t));
                self.of.insert((r.texture, key), w.textures.len() - 1);
                w.textures.len() - 1
            }
        };
        let mut info = Map::new();
        info.insert("index".into(), index.into());
        if r.uv_set == 1 {
            info.insert("texCoord".into(), 1.into());
        }
        if let Some(tr) = r.transform.filter(|t| t.offset.is_finite() && t.scale.is_finite() && t.rotation.is_finite())
        {
            let mut x = Map::new();
            if tr.offset != slerp_scene::glam::Vec2::ZERO {
                x.insert("offset".into(), nums(&tr.offset.to_array()));
            }
            if tr.rotation != 0.0 {
                x.insert("rotation".into(), num(tr.rotation));
            }
            if tr.scale != slerp_scene::glam::Vec2::ONE {
                x.insert("scale".into(), nums(&tr.scale.to_array()));
            }
            if !x.is_empty() {
                w.extension("KHR_texture_transform", false);
                info.insert("extensions".into(), json!({ "KHR_texture_transform": x }));
            }
        }
        Some(Value::Object(info))
    }
}

fn sampler(s: &Sampler) -> Value {
    let wrap = |w: Wrap| match w {
        Wrap::Repeat => 10497,
        Wrap::MirroredRepeat => 33648,
        Wrap::ClampToEdge => 33071,
    };
    let min = match (s.min, s.mipmaps) {
        (Filter::Nearest, false) => 9728,
        (Filter::Linear, false) => 9729,
        (Filter::Nearest, true) => 9984,
        (Filter::Linear, true) => 9987,
    };
    let mag = if s.mag == Filter::Nearest { 9728 } else { 9729 };
    json!({ "magFilter": mag, "minFilter": min, "wrapS": wrap(s.wrap_u), "wrapT": wrap(s.wrap_v) })
}

//endregion
