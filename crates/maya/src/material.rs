//! Maya shading networks as PBR materials.
//!
//! A shading group (`shadingEngine`) takes its surface from the node
//! connected to its `.ss`.  The surface's type decides which attributes
//! hold base colour, roughness, metalness, emission and opacity: the
//! classic Lambert family (lambert, blinn, phong) is approximated, while
//! standardSurface, aiStandardSurface, openPBRSurface and StingrayPBS map
//! almost directly.  A `file` node connected to a colour gives its picture,
//! a `place2dTexture` behind it the tiling, and a `bump2d` in tangent-space
//! normal mode (or an `aiNormalMap`) a normal map.  Unknown surface types
//! are read by whichever of the common attribute names they have.

use crate::graph::{Graph, under};
use glam::{Vec2, Vec3, Vec4};
use slerp_polymesh::textures::{ChannelSource, TextureSet};
use slerp_scene::{AlphaMode, ColorSpace, Material, TexRef, UvTransform};
use std::collections::HashMap;
use std::sync::Arc;

/// Builds scene materials from shading nodes, each once.
pub struct Materials<'a> {
    g: &'a Graph,
    pub textures: TextureSet<'a>,
    pub list: Vec<Material>,
    by_node: HashMap<Option<usize>, usize>,
    vertex_variants: HashMap<usize, usize>,
}

/// A colour or number input: its constant, and the file texture driving
/// it with the channel read (0 red .. 3 alpha, `None` for colour).
struct Input {
    texture: Option<(usize, Option<u8>)>,
}

impl<'a> Materials<'a> {
    pub fn new(g: &'a Graph, textures: TextureSet<'a>) -> Self {
        Materials { g, textures, list: Vec::new(), by_node: HashMap::new(), vertex_variants: HashMap::new() }
    }

    /// The scene material of shading group `sg` (`None`: Maya's default
    /// grey Lambert).
    pub fn for_group(&mut self, sg: Option<usize>) -> usize {
        let surface = sg.and_then(|sg| self.surface(sg));
        if let Some(&i) = self.by_node.get(&surface) {
            return i;
        }
        let m = match surface {
            Some(s) => self.convert(s),
            None => Material {
                name: "lambert1".into(),
                base_color: Vec4::new(0.4, 0.4, 0.4, 1.0),
                roughness: 0.9,
                double_sided: true,
                ..Default::default()
            },
        };
        self.list.push(m);
        let i = self.list.len() - 1;
        self.by_node.insert(surface, i);
        i
    }

    /// A copy of scene material `m` whose colour comes from the vertex
    /// colours alone (its picture, if any, stays), made once.
    pub fn vertex_colored(&mut self, m: usize) -> usize {
        if let Some(&i) = self.vertex_variants.get(&m) {
            return i;
        }
        let mut copy = self.list[m].clone();
        if copy.base_color_texture.is_none() {
            copy.base_color = Vec4::new(1.0, 1.0, 1.0, copy.base_color.w);
        }
        copy.name = format!("{} (vertex colours)", copy.name);
        self.list.push(copy);
        let i = self.list.len() - 1;
        self.vertex_variants.insert(m, i);
        i
    }

    /// The surface shader of a shading group.  Maya's default group isn't
    /// connected to `lambert1` in files; that default is assumed.
    fn surface(&self, sg: usize) -> Option<usize> {
        let g = self.g;
        let input = ["ss", "surfaceShader", "ai_surface_shader", "aiSurfaceShader"]
            .iter()
            .find_map(|a| g.input(sg, a))
            .map(|c| c.src);
        input.or_else(|| (g.nodes[sg].name == "initialShadingGroup").then(|| g.find("lambert1")).flatten())
    }

    /// The node feeding `node.attr`, or one of its components ("bcr").
    fn feed(&self, node: usize, attrs: &[&str]) -> Option<(usize, String)> {
        let g = self.g;
        for a in attrs {
            if let Some(c) = g.connections.iter().find(|c| c.dst == node && (c.dst_attr == *a || under(&c.dst_attr, a)))
            {
                return Some((c.src, c.src_attr.clone()));
            }
        }
        None
    }

    /// The file texture behind an attribute: directly, or through simple
    /// pass-through nodes (bump, normal map, gamma, reverse are not
    /// followed: they change the meaning).
    fn texture_input(&mut self, node: usize, attrs: &[&str], data: bool) -> Input {
        let Some((src, out)) = self.feed(node, attrs) else { return Input { texture: None } };
        let channel = match out.as_str() {
            "ocr" | "outColorR" => Some(0),
            "ocg" | "outColorG" => Some(1),
            "ocb" | "outColorB" => Some(2),
            "oa" | "outAlpha" => Some(3),
            _ => None,
        };
        let tex = self.file_texture(src, data);
        Input { texture: tex.map(|t| (t, channel)) }
    }

    /// Loads a `file` node's picture, once.
    fn file_texture(&mut self, file: usize, data: bool) -> Option<usize> {
        let g = self.g;
        let node = &g.nodes[file];
        if node.kind != "file" && node.kind != "aiImage" && node.kind != "RTFT" {
            return None;
        }
        let name = node.text("ftn").or_else(|| node.text("filename")).or_else(|| node.text("fileTextureName"))?;
        let t = self.textures.file(name)?;
        // A colour picture whose node says it is raw data, or a data
        // picture, is read without the sRGB curve.
        let raw = node
            .text("cs")
            .is_some_and(|cs| matches!(cs, "Raw" | "raw" | "Utility - Raw" | "scene-linear Rec 709/sRGB" | "linear"));
        if (raw || data)
            && let Some(tex) = self.textures.textures.get(t)
            && tex.color_space != ColorSpace::Linear
        {
            let mut copy = (**tex).clone();
            copy.color_space = ColorSpace::Linear;
            self.textures.textures[t] = Arc::new(copy);
        }
        Some(t)
    }

    /// The UV tiling of the `place2dTexture` behind a file node.
    fn tiling(&self, file: usize) -> Option<UvTransform> {
        let g = self.g;
        let place = g
            .input(file, "uv")
            .or_else(|| g.input(file, "uvCoord"))
            .map(|c| c.src)
            .filter(|&p| g.nodes[p].kind == "place2dTexture");
        let source = place.map(|p| &g.nodes[p]).unwrap_or(&g.nodes[file]);
        let re = source
            .nums("re")
            .filter(|v| v.len() >= 2)
            .map(|v| Vec2::new(v[0] as f32, v[1] as f32))
            .unwrap_or(Vec2::ONE);
        let of = source
            .nums("of")
            .filter(|v| v.len() >= 2)
            .map(|v| Vec2::new(v[0] as f32, v[1] as f32))
            .unwrap_or(Vec2::ZERO);
        if (re - Vec2::ONE).abs().max_element() < 1e-6 && of.abs().max_element() < 1e-6
            || !re.is_finite()
            || !of.is_finite()
        {
            return None;
        }
        // Maya's v runs up and the renderer's down: the scene flips v, so
        // the offset is measured from the other end.
        Some(UvTransform { offset: Vec2::new(of.x, 1.0 - re.y - of.y), rotation: 0.0, scale: re })
    }

    fn tex_ref(&self, texture: usize, file: Option<usize>) -> TexRef {
        let mut r = TexRef::new(texture);
        r.transform = file.and_then(|f| self.tiling(f));
        r
    }

    /// The file node behind an attribute, for its tiling.
    fn file_node(&self, node: usize, attrs: &[&str]) -> Option<usize> {
        self.feed(node, attrs).map(|(s, _)| s)
    }

    fn convert(&mut self, s: usize) -> Material {
        let g = self.g;
        let node = &g.nodes[s];
        let kind = node.kind.as_str();
        let color3 = |keys: &[&str], d: Vec3| -> Vec3 {
            keys.iter()
                .find_map(|k| {
                    node.nums(k).filter(|v| v.len() >= 3).map(|v| Vec3::new(v[0] as f32, v[1] as f32, v[2] as f32))
                })
                .filter(|c| c.is_finite())
                .unwrap_or(d)
        };
        let scalar = |keys: &[&str], d: f32| -> f32 { keys.iter().find_map(|k| node.num(k)).map_or(d, |v| v as f32) };
        let mut m = Material { name: node.name.clone(), double_sided: true, ..Default::default() };
        // Attribute names per family: base colour, its weight, roughness,
        // metalness, emission colour and weight, opacity.
        struct Names {
            color: &'static [&'static str],
            weight: &'static [&'static str],
            rough: &'static [&'static str],
            metal: &'static [&'static str],
            emit_color: &'static [&'static str],
            emit: &'static [&'static str],
            opacity: &'static [&'static str],
            normal: &'static [&'static str],
        }
        let (names, defaults) = match kind {
            "standardSurface" => (
                Names {
                    color: &["bc", "baseColor"],
                    weight: &["b", "base"],
                    rough: &["sr", "specularRoughness"],
                    metal: &["m", "metalness"],
                    emit_color: &["ec", "emissionColor"],
                    emit: &["e", "emission"],
                    opacity: &["op", "opacity"],
                    normal: &["n", "normalCamera"],
                },
                (Vec3::ONE, 0.8, 0.2),
            ),
            "aiStandardSurface" => (
                Names {
                    color: &["base_color", "baseColor"],
                    weight: &["base"],
                    rough: &["specular_roughness", "specularRoughness"],
                    metal: &["metalness"],
                    emit_color: &["emission_color", "emissionColor"],
                    emit: &["emission"],
                    opacity: &["opacity"],
                    normal: &["n", "normalCamera"],
                },
                (Vec3::ONE, 0.8, 0.2),
            ),
            "openPBRSurface" => (
                Names {
                    color: &["bc", "baseColor"],
                    weight: &["b", "baseWeight"],
                    rough: &["sr", "specularRoughness"],
                    metal: &["m", "baseMetalness"],
                    emit_color: &["ec", "emissionColor"],
                    emit: &["elu", "emissionLuminance"],
                    opacity: &["geop", "geometryOpacity"],
                    normal: &["n", "normalCamera", "geon"],
                },
                (Vec3::splat(0.8), 1.0, 0.3),
            ),
            "StingrayPBS" => (
                Names {
                    color: &["base_color"],
                    weight: &[],
                    rough: &["roughness"],
                    metal: &["metallic"],
                    emit_color: &["emissive"],
                    emit: &["emissive_intensity"],
                    opacity: &[],
                    normal: &[],
                },
                (Vec3::ONE, 1.0, 0.33),
            ),
            "surfaceShader" => (
                Names {
                    color: &["oc", "outColor"],
                    weight: &[],
                    rough: &[],
                    metal: &[],
                    emit_color: &[],
                    emit: &[],
                    opacity: &[],
                    normal: &[],
                },
                (Vec3::splat(0.5), 1.0, 1.0),
            ),
            // The Lambert family, and anything else: Maya's "color" times
            // "diffuse".
            _ => (
                Names {
                    color: &["c", "color", "bc", "base_color", "baseColor"],
                    weight: &["dc", "diffuse"],
                    rough: &[],
                    metal: &[],
                    emit_color: &["ic", "incandescence"],
                    emit: &[],
                    opacity: &[],
                    normal: &["n", "normalCamera"],
                },
                (Vec3::splat(0.5), 0.8, 1.0),
            ),
        };
        let lambert = !matches!(
            kind,
            "standardSurface" | "aiStandardSurface" | "openPBRSurface" | "StingrayPBS" | "surfaceShader"
        );
        // 1. Base colour, and its picture.
        let color = color3(names.color, defaults.0);
        let weight = scalar(names.weight, defaults.1);
        let mut base = color * weight;
        let stingray_tex = |flag: &str| kind != "StingrayPBS" || node.num(flag).unwrap_or(0.0) != 0.0;
        let color_attrs: Vec<&str> = if kind == "StingrayPBS" { vec!["TEX_color_map"] } else { names.color.to_vec() };
        if stingray_tex("use_color_map")
            && let Some((t, _)) = self.texture_input(s, &color_attrs, false).texture
        {
            // A picture replaces the colour; the weight still scales it.
            base = Vec3::splat(weight);
            m.base_color_texture = Some(self.tex_ref(t, self.file_node(s, &color_attrs)));
        }
        m.base_color = base.extend(1.0);
        // 2. Roughness and metalness.
        m.roughness = if lambert {
            match kind {
                // Blinn's eccentricity and Phong's cosine power as a
                // roughness, roughly.
                "blinn" => scalar(&["ecc", "eccentricity"], 0.3).clamp(0.0, 1.0).sqrt(),
                "phong" => (2.0 / (scalar(&["cp", "cosinePower"], 20.0).max(2.0) + 2.0)).sqrt(),
                "phongE" => scalar(&["rgh", "roughness"], 0.5),
                _ => 0.9,
            }
        } else {
            scalar(names.rough, defaults.2)
        }
        .clamp(0.0, 1.0);
        m.metallic = scalar(names.metal, 0.0).clamp(0.0, 1.0);
        if !lambert {
            let rough_attrs: Vec<&str> =
                if kind == "StingrayPBS" { vec!["TEX_roughness_map"] } else { names.rough.to_vec() };
            let metal_attrs: Vec<&str> =
                if kind == "StingrayPBS" { vec!["TEX_metallic_map"] } else { names.metal.to_vec() };
            let rough = if stingray_tex("use_roughness_map") {
                self.texture_input(s, &rough_attrs, true).texture
            } else {
                None
            };
            let metal =
                if stingray_tex("use_metallic_map") { self.texture_input(s, &metal_attrs, true).texture } else { None };
            let source = |t: Option<(usize, Option<u8>)>| {
                t.map(|(texture, ch)| ChannelSource { texture, channel: ch.unwrap_or(0), invert: false })
            };
            if (rough.is_some() || metal.is_some())
                && let Some(packed) = self.textures.pack_metal_rough(source(rough), source(metal))
            {
                if rough.is_some() {
                    m.roughness = 1.0;
                }
                if metal.is_some() {
                    m.metallic = 1.0;
                }
                let file = self.file_node(s, &rough_attrs).or_else(|| self.file_node(s, &metal_attrs));
                m.metallic_roughness_texture = Some(self.tex_ref(packed, file));
            }
        }
        // 3. Emission.
        let emit_color = color3(names.emit_color, if lambert { Vec3::ZERO } else { Vec3::ONE });
        let emit = if names.emit.is_empty() { 1.0 } else { scalar(names.emit, 0.0) };
        m.emissive = (emit_color * emit).max(Vec3::ZERO);
        if kind == "openPBRSurface" {
            // Luminance in nits; a few hundred reads as fully lit.
            m.emissive = (emit_color * (emit / 300.0)).max(Vec3::ZERO);
        }
        let emit_attrs: Vec<&str> =
            if kind == "StingrayPBS" { vec!["TEX_emissive_map"] } else { names.emit_color.to_vec() };
        if stingray_tex("use_emissive_map")
            && let Some((t, _)) = self.texture_input(s, &emit_attrs, false).texture
        {
            m.emissive = Vec3::splat(if names.emit.is_empty() {
                1.0
            } else {
                emit.max(if kind == "StingrayPBS" { 1.0 } else { 0.0 })
            });
            m.emissive_texture = Some(self.tex_ref(t, self.file_node(s, &emit_attrs)));
        }
        if kind == "surfaceShader" {
            m.unlit = true;
            m.emissive = Vec3::ZERO;
        }
        // 4. Opacity: Lambert's transparency is its inverse.
        let opacity = if lambert {
            let t = color3(&["it", "transparency"], Vec3::ZERO);
            1.0 - (t.x + t.y + t.z) / 3.0
        } else {
            let o = color3(names.opacity, Vec3::ONE);
            (o.x + o.y + o.z) / 3.0
        };
        let opacity_attrs: Vec<&str> = if lambert { vec!["it", "transparency"] } else { names.opacity.to_vec() };
        let opacity_tex = self.texture_input(s, &opacity_attrs, true).texture;
        if let Some((mask, _)) = opacity_tex {
            // The mask goes into the base picture's alpha (Maya's
            // transparency maps are inverted, so their alpha is used as it
            // is: a file's outTransparency already reads as opacity there).
            match m.base_color_texture {
                Some(base) => {
                    if let Some(merged) = self.textures.merge_alpha(base.texture, mask) {
                        m.base_color_texture = Some(TexRef { texture: merged, ..base });
                    }
                }
                None => m.base_color_texture = Some(self.tex_ref(mask, self.file_node(s, &opacity_attrs))),
            }
            m.alpha_mode = AlphaMode::Mask(0.5);
        } else if opacity < 0.999 {
            m.base_color.w = opacity.clamp(0.0, 1.0);
            m.alpha_mode = AlphaMode::Blend;
        }
        // 5. Normal maps.
        let normal_attrs: Vec<&str> =
            if kind == "StingrayPBS" { vec!["TEX_normal_map"] } else { names.normal.to_vec() };
        if !normal_attrs.is_empty() && (kind != "StingrayPBS" || stingray_tex("use_normal_map")) {
            m.normal_texture = self.normal_map(s, &normal_attrs);
        }
        if kind == "StingrayPBS"
            && stingray_tex("use_ao_map")
            && let Some((t, _)) = self.texture_input(s, &["TEX_ao_map"], true).texture
        {
            m.occlusion_texture = Some(self.tex_ref(t, self.file_node(s, &["TEX_ao_map"])));
        }
        m
    }

    /// A tangent-space normal map behind a surface's normal input: a
    /// `bump2d` in normal-map mode, or an `aiNormalMap`; or a file plugged
    /// in directly (StingrayPBS).
    fn normal_map(&mut self, s: usize, attrs: &[&str]) -> Option<TexRef> {
        let g = self.g;
        let (src, _) = self.feed(s, attrs)?;
        let node = &g.nodes[src];
        let file = match node.kind.as_str() {
            "file" => Some(src),
            "bump2d" if node.num("bi").unwrap_or(0.0) >= 1.0 => self.feed(src, &["bv", "bumpValue"]).map(|(f, _)| f),
            "aiNormalMap" => self.feed(src, &["input", "ip"]).map(|(f, _)| f),
            _ => None,
        }?;
        let t = self.file_texture(file, true)?;
        Some(self.tex_ref(t, Some(file)))
    }
}
