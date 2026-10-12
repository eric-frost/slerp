//! 3ds Max materials as PBR materials.
//!
//! - Standard: its shader object (Blinn, Phong, ...) holds ambient, diffuse
//!   and specular colours, self-illumination, specular level and
//!   glossiness; its extended block the opacity; its `Texmaps` object the
//!   maps by channel (1 diffuse, 5 self-illumination, 6 opacity, 8 bump).
//! - Physical Material: base weight and colour, roughness (or glossiness),
//!   metalness, transparency, emission, and their maps.
//! - Multi/Sub-Object: sub-materials picked by the faces' material ids.
//! - Bitmap maps give their picture (a file name, or an asset id that the
//!   file's asset list resolves) and the tiling of their coordinates;
//!   Normal Bump maps give a normal map.  Other maps are followed through
//!   their first map input when they have one, else ignored.

use crate::file::{MaxFile, param_color, param_float, param_ref, pb2_block, pb2_params};
use glam::{Vec2, Vec3, Vec4};
use slerp_polymesh::textures::{ChannelSource, TextureSet};
use slerp_scene::{AlphaMode, ColorSpace, Material, TexRef, UvTransform};
use std::collections::HashMap;
use std::sync::Arc;

const STANDARD: (u32, u32) = (0x2, 0);
const MULTI: (u32, u32) = (0x200, 0);
const PHYSICAL: (u32, u32) = (0x3D6B_1CEC, 0xDEAD_C001);
const BITMAP: (u32, u32) = (0x240, 0);

pub struct Materials<'f, 'a> {
    file: &'f MaxFile<'a>,
    pub textures: TextureSet<'f>,
    pub list: Vec<Material>,
    by_object: HashMap<Option<usize>, usize>,
    by_wire: HashMap<[u8; 3], usize>,
    /// Material types seen that are read only roughly or not at all.
    pub unknown: Vec<String>,
}

impl<'f, 'a> Materials<'f, 'a> {
    pub fn new(file: &'f MaxFile<'a>, textures: TextureSet<'f>) -> Self {
        Materials {
            file,
            textures,
            list: Vec::new(),
            by_object: HashMap::new(),
            by_wire: HashMap::new(),
            unknown: Vec::new(),
        }
    }

    /// The sub-materials faces use, by face material id: `slots(mtl)` maps
    /// a material id to a scene material.
    pub fn slots(&mut self, mtl: Option<usize>) -> Box<dyn Fn(u32) -> usize> {
        let file = self.file;
        match mtl.filter(|&m| file.class_id(m) == Some(MULTI)) {
            Some(multi) => {
                let (ids, subs) = self.multi(multi);
                let scene: Vec<usize> = subs.iter().map(|&s| self.scene_material(s)).collect();
                let fallback = self.scene_material(None);
                Box::new(move |id: u32| -> usize {
                    if scene.is_empty() {
                        return fallback;
                    }
                    let k = ids.iter().position(|&i| i == id as i32).unwrap_or(id as usize % scene.len());
                    scene.get(k).copied().unwrap_or(fallback)
                })
            }
            None => {
                let m = self.scene_material(mtl);
                Box::new(move |_| m)
            }
        }
    }

    /// A Multi/Sub-Object's material ids and sub-materials, in order.
    fn multi(&self, multi: usize) -> (Vec<i32>, Vec<Option<usize>>) {
        let file = self.file;
        let refs = file.refs(multi);
        let subs: Vec<Option<usize>> = refs.iter().skip(1).copied().collect();
        let mut ids: Vec<i32> = Vec::new();
        if let Some(pb) = refs.first().copied().flatten() {
            // The block's integer table holds the ids (its first table of
            // integers that matches the sub-material count).
            for p in pb2_params(file, pb) {
                if p.ty == 0x801 {
                    let t = p.table_ints();
                    if t.len() == subs.len() {
                        ids = t;
                        break;
                    }
                }
            }
        }
        if ids.len() != subs.len() {
            ids = (0..subs.len() as i32).collect();
        }
        (ids, subs)
    }

    /// The material of an object without one: Max draws it in the node's
    /// wire colour.
    pub fn wire(&mut self, rgb: [u8; 3]) -> usize {
        if let Some(&i) = self.by_wire.get(&rgb) {
            return i;
        }
        let c = Vec3::from(rgb.map(|v| v as f32 / 255.0));
        self.list.push(Material {
            name: format!("Wire colour #{:02x}{:02x}{:02x}", rgb[0], rgb[1], rgb[2]),
            base_color: linear(c).extend(1.0),
            roughness: 0.6,
            ..Default::default()
        });
        let i = self.list.len() - 1;
        self.by_wire.insert(rgb, i);
        i
    }

    /// The scene material of Max material `m` (`None`: Max's default grey).
    pub fn scene_material(&mut self, m: Option<usize>) -> usize {
        if let Some(&i) = self.by_object.get(&m) {
            return i;
        }
        let mat = match m {
            Some(m) => self.convert(m),
            None => Material {
                name: "Default".into(),
                base_color: Vec4::new(0.6, 0.6, 0.6, 1.0),
                roughness: 0.7,
                ..Default::default()
            },
        };
        self.list.push(mat);
        let i = self.list.len() - 1;
        self.by_object.insert(m, i);
        i
    }

    fn convert(&mut self, m: usize) -> Material {
        let file = self.file;
        let name = file.label(m).unwrap_or_else(|| file.name_of(m).to_string());
        let mut mat = Material { name, roughness: 0.6, ..Default::default() };
        match file.class_id(m) {
            Some(STANDARD) => self.standard(m, &mut mat),
            Some(PHYSICAL) => self.physical(m, &mut mat),
            Some(MULTI) => {
                // A Multi/Sub used where one material is wanted: its first.
                if let Some(Some(first)) = self.multi(m).1.first().copied() {
                    let i = self.scene_material(Some(first));
                    return self.list[i].clone();
                }
            }
            _ => {
                let kind = file.name_of(m).to_string();
                if !self.unknown.contains(&kind) {
                    self.unknown.push(kind);
                }
                self.generic(m, &mut mat);
            }
        }
        mat
    }

    /// The Standard material.
    fn standard(&mut self, m: usize, mat: &mut Material) {
        let file = self.file;
        let refs = file.refs(m);
        // 1. The shader's colours and highlights.
        let shader_pb = refs.get(2).copied().flatten().and_then(|s| file.reference(s, 0));
        let color = |id| shader_pb.and_then(|pb| param_color(file, pb, id));
        let num = |id| shader_pb.and_then(|pb| param_float(file, pb, id));
        let diffuse = linear(Vec3::from(color(1).unwrap_or([0.5; 3])));
        mat.base_color = diffuse.extend(1.0);
        let gloss = num(10).unwrap_or(0.1).clamp(0.0, 1.0);
        let level = num(9).unwrap_or(0.0).max(0.0);
        mat.roughness = (1.0 - gloss).clamp(0.05, 1.0).sqrt();
        mat.specular = level.min(1.0);
        let self_illum = num(7).unwrap_or(0.0).clamp(0.0, 1.0);
        if num(6).unwrap_or(0.0) != 0.0 {
            mat.emissive = linear(Vec3::from(color(8).unwrap_or([0.0; 3])));
        } else {
            mat.emissive = diffuse * self_illum;
        }
        // 2. Opacity from the extended block.
        let extended =
            refs.iter().flatten().copied().find(|&r| file.name_of(r) == "ParamBlock2" && pb2_block(file, r) == Some(1));
        let opacity = extended.and_then(|pb| param_float(file, pb, 1)).unwrap_or(1.0).clamp(0.0, 1.0);
        if opacity < 0.999 {
            mat.base_color.w = opacity;
            mat.alpha_mode = AlphaMode::Blend;
        }
        // 3. Maps, by the Standard material's channel numbers.
        let texmaps = refs.get(1).copied().flatten();
        let map = |channel: usize| texmaps.and_then(|t| file.reference(t, channel * 2 + 1));
        if let Some(t) = map(1).and_then(|t| self.texture(t, false)) {
            mat.base_color = Vec4::new(1.0, 1.0, 1.0, mat.base_color.w);
            mat.base_color_texture = Some(t);
        }
        if let Some(t) = map(5).and_then(|t| self.texture(t, false)) {
            mat.emissive = Vec3::ONE;
            mat.emissive_texture = Some(t);
        }
        if let Some(mask) = map(6).and_then(|t| self.texture(t, true)) {
            self.apply_cutout(mat, mask);
        }
        if let Some(n) = map(8).and_then(|t| self.normal_map(t)) {
            mat.normal_texture = Some(n);
        }
    }

    /// The Physical Material: parameters by their ids in its main block.
    fn physical(&mut self, m: usize, mat: &mut Material) {
        let file = self.file;
        let Some(pb) = file.reference(m, 0) else { return };
        let num = |id| param_float(file, pb, id);
        let weight = num(1).unwrap_or(1.0);
        let color = param_color(file, pb, 2).map(|c| linear(Vec3::from(c))).unwrap_or(Vec3::splat(0.2));
        mat.base_color = (color * weight).extend(1.0);
        let rough = num(4).unwrap_or(0.0).clamp(0.0, 1.0);
        // "Roughness" may be inverted (glossiness).
        mat.roughness = if num(5).unwrap_or(0.0) != 0.0 { 1.0 - rough } else { rough };
        mat.metallic = num(6).unwrap_or(0.0).clamp(0.0, 1.0);
        let transparency = num(30).unwrap_or(0.0).clamp(0.0, 1.0);
        if transparency > 0.0 {
            mat.transmission = transparency;
            mat.ior = num(33).unwrap_or(1.5).clamp(1.0, 3.0);
            mat.thickness = if num(34).unwrap_or(0.0) != 0.0 { 0.0 } else { num(32).unwrap_or(0.0).max(0.0) };
        }
        let emission = num(50).unwrap_or(0.0).max(0.0);
        if emission > 0.0 {
            let c = param_color(file, pb, 51).map(|c| linear(Vec3::from(c))).unwrap_or(Vec3::ONE);
            mat.emissive = c * emission * (num(52).unwrap_or(1500.0) / 1500.0).clamp(0.0, 100.0);
        }
        mat.clearcoat = num(60).unwrap_or(0.0).clamp(0.0, 1.0);
        mat.clearcoat_roughness = num(62).unwrap_or(0.0).clamp(0.0, 1.0);
        // Maps: base colour 101, roughness 104, metalness 105, transparency
        // 109, emission colour 117, bump 130, cutout 133.
        if let Some(t) = param_ref(file, pb, 101).and_then(|t| self.texture(t, false)) {
            mat.base_color = Vec4::new(weight, weight, weight, 1.0);
            mat.base_color_texture = Some(t);
        }
        let rough_map = param_ref(file, pb, 104).and_then(|t| self.texture(t, true));
        let metal_map = param_ref(file, pb, 105).and_then(|t| self.texture(t, true));
        if rough_map.is_some() || metal_map.is_some() {
            let invert = num(5).unwrap_or(0.0) != 0.0;
            let source =
                |t: Option<TexRef>, invert| t.map(|t| ChannelSource { texture: t.texture, channel: 0, invert });
            if let Some(packed) = self.textures.pack_metal_rough(source(rough_map, invert), source(metal_map, false)) {
                let transform = rough_map.or(metal_map).and_then(|t| t.transform);
                mat.metallic_roughness_texture = Some(TexRef { transform, ..TexRef::new(packed) });
                if rough_map.is_some() {
                    mat.roughness = 1.0;
                }
                if metal_map.is_some() {
                    mat.metallic = 1.0;
                }
            }
        }
        if let Some(t) = param_ref(file, pb, 117).and_then(|t| self.texture(t, false)) {
            if mat.emissive == Vec3::ZERO {
                mat.emissive = Vec3::ONE;
            }
            mat.emissive_texture = Some(t);
        }
        if let Some(n) = param_ref(file, pb, 130).and_then(|t| self.normal_map(t)) {
            mat.normal_texture = Some(n);
        }
        if let Some(mask) = param_ref(file, pb, 133).and_then(|t| self.texture(t, true)) {
            self.apply_cutout(mat, mask);
        }
    }

    /// Materials of other types: the first colour and map of their main
    /// block, which for most renderers' materials is the diffuse or base
    /// colour.
    fn generic(&mut self, m: usize, mat: &mut Material) {
        let file = self.file;
        let Some(pb) = file.refs(m).into_iter().flatten().find(|&r| file.name_of(r) == "ParamBlock2") else { return };
        if let Some(c) = pb2_params(file, pb).iter().find_map(|p| if matches!(p.ty, 2 | 23) { p.color() } else { None })
        {
            mat.base_color = linear(Vec3::from(c)).extend(1.0);
        }
        let first_map = pb2_params(file, pb).iter().find(|p| p.ty == 15).map(|p| p.id);
        if let Some(t) = first_map.and_then(|id| param_ref(file, pb, id)).and_then(|t| self.texture(t, false)) {
            mat.base_color = Vec4::ONE;
            mat.base_color_texture = Some(t);
        }
    }

    /// Puts a cutout or opacity mask into the base colour's alpha.
    fn apply_cutout(&mut self, mat: &mut Material, mask: TexRef) {
        match mat.base_color_texture {
            Some(base) => {
                if let Some(merged) = self.textures.merge_alpha(base.texture, mask.texture) {
                    mat.base_color_texture = Some(TexRef { texture: merged, ..base });
                }
            }
            // Without a picture to cut, the mask's own alpha (or nothing)
            // is all there is.
            None if self.textures.get(mask.texture).is_some_and(|t| t.has_alpha) => mat.base_color_texture = Some(mask),
            None => return,
        }
        mat.alpha_mode = AlphaMode::Mask(0.5);
    }

    /// A texture map's picture as a texture reference, following simple
    /// wrappers (a map's first map input) a few levels deep.
    fn texture(&mut self, map: usize, data: bool) -> Option<TexRef> {
        let file = self.file;
        let mut node = map;
        for _ in 0..4 {
            // Bitmaps, and OSL maps that look a picture up (their file
            // name is a parameter like any other).
            if file.class_id(node) == Some(BITMAP) || file.name_of(node) == "OSL Map" {
                return self.bitmap(node, data);
            }
            // Wrappers such as Map Output Selector or Color Correction:
            // their first reference that is a map.
            node = file
                .refs(node)
                .into_iter()
                .flatten()
                .find(|&r| file.super_id(r) == Some(crate::file::super_class::TEXMAP))?;
        }
        None
    }

    fn bitmap(&mut self, map: usize, data: bool) -> Option<TexRef> {
        let file = self.file;
        let refs = file.refs(map);
        let name = refs
            .iter()
            .flatten()
            .filter(|&&r| file.name_of(r) == "ParamBlock2")
            .flat_map(|&pb| file_names(file, pb))
            .find(|n| is_picture(n))?;
        let t = self.textures.file(&name)?;
        if data
            && let Some(tex) = self.textures.textures.get(t)
            && tex.color_space != ColorSpace::Linear
        {
            let mut copy = (**tex).clone();
            copy.color_space = ColorSpace::Linear;
            self.textures.textures[t] = Arc::new(copy);
        }
        let mut r = TexRef::new(t);
        // Tiling and offset from the map's coordinates (its first
        // reference, an old parameter block: offsets 0 and 1, tiling 2
        // and 3).
        if let Some(pb) = refs.first().copied().flatten().and_then(|uv| file.reference(uv, 0)) {
            let f = |id: u16, d: f32| param_float(file, pb, id).filter(|v| v.is_finite()).unwrap_or(d);
            let (off, tile) = (Vec2::new(f(0, 0.0), f(1, 0.0)), Vec2::new(f(2, 1.0), f(3, 1.0)));
            if (tile - Vec2::ONE).abs().max_element() > 1e-6 || off.abs().max_element() > 1e-6 {
                // Max tiles about the map's centre; the scene flips v.
                let offset = Vec2::new(0.5 - 0.5 * tile.x + off.x, 0.5 - 0.5 * tile.y - off.y);
                r.transform = Some(UvTransform { offset, rotation: 0.0, scale: tile });
            }
        }
        Some(r)
    }

    /// A normal map behind a bump slot: a Normal Bump map's own map, or a
    /// bitmap that looks like a tangent-space normal map.
    fn normal_map(&mut self, map: usize) -> Option<TexRef> {
        let file = self.file;
        if file.name_of(map) == "Normal Bump" {
            let inner = file
                .refs(map)
                .into_iter()
                .flatten()
                .find(|&r| file.super_id(r) == Some(crate::file::super_class::TEXMAP))?;
            return self.texture(inner, true);
        }
        let t = self.texture(map, true)?;
        let tex = self.textures.get(t.texture)?;
        looks_like_normal_map(tex).then_some(t)
    }
}

/// A colour as Max shows it (display values) in linear light.
fn linear(c: Vec3) -> Vec3 {
    let f = |v: f32| if v <= 0.04045 { v / 12.92 } else { ((v.max(0.0) + 0.055) / 1.055).powf(2.4) };
    Vec3::new(f(c.x), f(c.y), f(c.z))
}

/// The file names a parameter block holds: inline in a file name
/// parameter, as a name (0x1230) or an asset id (0x1260 > 0x0002, or
/// 0x000F > 0x0002) in the chunk after a bitmap or file name parameter.
fn file_names(file: &MaxFile, pb: usize) -> Vec<String> {
    let Some(chunk) = file.objects.get(pb) else { return Vec::new() };
    let kids = chunk.children();
    let mut out = Vec::new();
    let asset = |c: &crate::chunks::Chunk| -> Option<String> {
        let id: [u8; 16] = c.child_data(0x0002)?.get(..16)?.try_into().ok()?;
        file.assets.get(&id).cloned()
    };
    for (k, c) in kids.iter().enumerate() {
        if !matches!(c.id, 0x100E | 0x000A) {
            continue;
        }
        let Some(d) = c.data() else { continue };
        let ty = crate::chunks::u16_at(d, 2).unwrap_or(0) & !0x800;
        if ty != 9 && ty != 16 {
            continue;
        }
        // An inline name where the value goes (offset 15, or 11 in older
        // files): its length in bytes with the closing zero, then UTF-16.
        for start in [15usize, 11] {
            if let Some(n) = crate::chunks::u32_at(d, start).map(|n| n as usize)
                && n >= 2
                && start + 4 + n == d.len()
            {
                let name = crate::chunks::utf16(&d[start + 4..]);
                if !name.is_empty() {
                    out.push(name);
                }
                break;
            }
        }
        if let Some(next) = kids.get(k + 1) {
            if let Some(n) =
                next.find(0x1230).and_then(|c| c.data()).map(crate::chunks::utf16).filter(|n| !n.is_empty())
            {
                out.push(n);
            }
            if let Some(n) =
                next.find(0x1260).and_then(asset).or_else(|| (next.id == 0x000F).then(|| asset(next)).flatten())
            {
                out.push(n);
            }
        }
    }
    out
}

/// Whether a file name is a picture's (OSL maps also name shader files).
fn is_picture(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    !(lower.ends_with(".osl") || lower.ends_with(".oso") || lower.is_empty())
}

/// Whether a picture plugged into a bump slot is really a tangent-space
/// normal map: mostly blue, with red and green centred on one half.
fn looks_like_normal_map(tex: &slerp_scene::Texture) -> bool {
    let Some((w, h, px)) = slerp_polymesh::textures::rgba8(tex) else { return false };
    let count = (w as usize * h as usize).max(1);
    let step = (count / 4096).max(1);
    let (mut r, mut g, mut b, mut n) = (0.0f64, 0.0f64, 0.0f64, 0.0f64);
    for i in (0..count).step_by(step) {
        let p = &px[i * 4..i * 4 + 4];
        r += p[0] as f64;
        g += p[1] as f64;
        b += p[2] as f64;
        n += 1.0;
    }
    let (r, g, b) = (r / n / 255.0, g / n / 255.0, b / n / 255.0);
    b > 0.65 && (r - 0.5).abs() < 0.2 && (g - 0.5).abs() < 0.2
}
