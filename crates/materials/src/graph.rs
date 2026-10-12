//! A small shader-graph model that both MaterialX documents and USD shading
//! networks are read into, and the mapping from their surface shaders
//! (OpenPBR, Autodesk Standard Surface, glTF PBR, UsdPreviewSurface) onto
//! glTF's metallic-roughness material.
//!
//! Only what a real-time preview can use is evaluated: a shader input is
//! either a constant, or a texture reached through a chain of nodes that
//! pass it on (normal maps, channel extraction, conversions, multiplication
//! by a constant).  Anything else, such as a procedural noise, counts as
//! unconnected, so the input falls back to its constant default.

use crate::realize::{Channel, FileRef, MatDesc, TexInput, uv_transform_from_gl};
use slerp_scene::glam::{Mat3, Vec2, Vec3};
use slerp_scene::{AlphaMode, ColorSpace, Filter, Sampler, Wrap};

/// How deep a chain of nodes may go before it counts as a cycle.
const MAX_DEPTH: u32 = 32;

/*┌─────────────────╮
| Model           |
└─────────────────╯*/
//region Model

#[derive(Clone, Debug)]
pub(crate) enum GValue {
    Num(Vec<f32>),
    Str(String),
    File { file: FileRef, label: String },
}

/// A connection to an output of another node.
#[derive(Clone, Debug)]
pub(crate) struct Link {
    pub node: usize,
    pub output: String,
}

#[derive(Clone, Debug)]
pub(crate) struct GInput {
    pub name: String,
    pub value: Option<GValue>,
    pub link: Option<Link>,
    /// The colour space the document names for this input (MaterialX's
    /// `colorspace`); USD says it with a `sourceColorSpace` input instead.
    pub color_space: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct GNode {
    pub name: String,
    /// The node's kind without type suffixes, such as "open_pbr_surface",
    /// "tiledimage" or "UsdUVTexture".
    pub category: String,
    /// The output type, such as "color3" or "float"; empty when unknown.
    pub ty: String,
    pub inputs: Vec<GInput>,
}

#[derive(Debug, Default)]
pub(crate) struct Graph {
    pub nodes: Vec<GNode>,
}

/// Node categories this module understands, longest first so that a
/// MaterialX node definition name such as `ND_tiledimage_color3` or
/// `ND_open_pbr_surface_surfaceshader` finds its category by prefix.
const CATEGORIES: &[&str] = &[
    "UsdPrimvarReader",
    "UsdPreviewSurface",
    "disney_principled",
    "open_pbr_surface",
    "standard_surface",
    "gltf_colorimage",
    "surfacematerial",
    "UsdTransform2d",
    "gltf_normalmap",
    "geompropvalue",
    "surface_unlit",
    "displacement",
    "hextiledimage",
    "UsdUVTexture",
    "tiledimage",
    "gltf_image",
    "normalmap",
    "separate2",
    "separate3",
    "separate4",
    "constant",
    "multiply",
    "texcoord",
    "gltf_pbr",
    "extract",
    "convert",
    "swizzle",
    "place2d",
    "image",
    "add",
    "dot",
    "mix",
];

/// The category of a node named by a MaterialX node definition (`ND_...`)
/// or a USD shader id.
pub(crate) fn category_of(id: &str) -> String {
    let bare = id.strip_prefix("ND_").unwrap_or(id);
    for cat in CATEGORIES {
        if let Some(rest) = bare.strip_prefix(cat)
            && (rest.is_empty() || rest.starts_with('_'))
        {
            return cat.to_string();
        }
    }
    bare.to_string()
}

fn is_image(cat: &str) -> bool {
    matches!(
        cat,
        "image" | "tiledimage" | "hextiledimage" | "gltf_image" | "gltf_colorimage" | "gltf_normalmap" | "UsdUVTexture"
    )
}

/// Whether a category is a surface shader this module can map.
pub(crate) fn is_surface(cat: &str) -> bool {
    matches!(
        cat,
        "open_pbr_surface"
            | "standard_surface"
            | "gltf_pbr"
            | "UsdPreviewSurface"
            | "surface_unlit"
            | "disney_principled"
    )
}

impl Graph {
    pub fn input(&self, node: usize, name: &str) -> Option<&GInput> {
        self.nodes.get(node)?.inputs.iter().find(|i| i.name == name)
    }

    /// An input's numbers when it holds them directly or through a chain
    /// of nodes that only pass a constant on.
    fn num(&self, node: usize, name: &str) -> Option<Vec<f32>> {
        match self.src(node, name, 0) {
            Src::Const(v) if !v.is_empty() => Some(v),
            _ => None,
        }
    }

    fn num1(&self, node: usize, name: &str, default: f32) -> f32 {
        self.num(node, name).map_or(default, |v| v[0])
    }

    fn vec2(&self, node: usize, name: &str, default: Vec2) -> Vec2 {
        match self.num(node, name) {
            Some(v) if v.len() >= 2 => Vec2::new(v[0], v[1]),
            Some(v) => Vec2::splat(v[0]),
            None => default,
        }
    }

    fn string(&self, node: usize, name: &str) -> Option<&str> {
        match &self.input(node, name)?.value {
            Some(GValue::Str(s)) => Some(s),
            _ => None,
        }
    }
}

//endregion

/*┌─────────────────╮
| Tracing         |
└─────────────────╯*/
//region Tracing

/// What an input turned out to be.
#[derive(Clone, Debug)]
enum Src {
    Const(Vec<f32>),
    Tex(Hit),
    /// Unconnected, or connected to something we can't evaluate.
    None,
}

/// A texture reached from an input.
#[derive(Clone, Debug)]
struct Hit {
    /// The image node.
    image: usize,
    channel: Channel,
    /// Multiplies the texture, per channel.
    factor: [f32; 4],
    /// From `normalmap` nodes along the way.
    normal_scale: f32,
}

impl Graph {
    fn src(&self, node: usize, name: &str, depth: u32) -> Src {
        let Some(input) = self.input(node, name) else { return Src::None };
        if let Some(link) = &input.link {
            return self.trace(link, depth + 1);
        }
        match &input.value {
            Some(GValue::Num(v)) => Src::Const(v.clone()),
            _ => Src::None,
        }
    }

    fn trace(&self, link: &Link, depth: u32) -> Src {
        let Some(n) = self.nodes.get(link.node) else { return Src::None };
        if depth > MAX_DEPTH {
            return Src::None;
        }
        let cat = n.category.as_str();
        if is_image(cat) {
            return Src::Tex(Hit {
                image: link.node,
                channel: image_channel(n, &link.output),
                factor: self.image_factor(link.node),
                normal_scale: 1.0,
            });
        }
        match cat {
            "normalmap" => match self.src(link.node, "in", depth) {
                Src::Tex(mut hit) => {
                    hit.normal_scale *= self.num1(link.node, "scale", 1.0);
                    Src::Tex(hit)
                }
                other => other,
            },
            "constant" => self.src(link.node, "value", depth),
            "dot" | "convert" => self.src(link.node, "in", depth),
            "extract" => {
                let index = self.num1(link.node, "index", 0.0) as usize;
                pick_channel(self.src(link.node, "in", depth), index)
            }
            "separate2" | "separate3" | "separate4" => {
                let index = match link.output.chars().last() {
                    Some('g' | 'y') => 1,
                    Some('b' | 'z') => 2,
                    Some('a' | 'w') => 3,
                    _ => 0,
                };
                pick_channel(self.src(link.node, "in", depth), index)
            }
            "swizzle" => {
                let channels = self.string(link.node, "channels").unwrap_or("r");
                let index = match channels.chars().next() {
                    Some('g' | 'y') => 1,
                    Some('b' | 'z') => 2,
                    Some('a' | 'w') => 3,
                    _ => 0,
                };
                if channels.len() == 1 { pick_channel(self.src(link.node, "in", depth), index) } else { Src::None }
            }
            "multiply" => {
                let a = self.src(link.node, "in1", depth);
                let b = self.src(link.node, "in2", depth);
                match (a, b) {
                    (Src::Tex(hit), Src::Const(c)) | (Src::Const(c), Src::Tex(hit)) => {
                        let mut hit = hit;
                        for (i, f) in hit.factor.iter_mut().enumerate() {
                            *f *= c.get(i).or(c.first()).copied().unwrap_or(1.0);
                        }
                        Src::Tex(hit)
                    }
                    (Src::Const(a), Src::Const(b)) => {
                        let len = a.len().max(b.len());
                        let get = |v: &[f32], i: usize| v.get(i).or(v.first()).copied().unwrap_or(1.0);
                        Src::Const((0..len).map(|i| get(&a, i) * get(&b, i)).collect())
                    }
                    (Src::Tex(hit), Src::None) | (Src::None, Src::Tex(hit)) => Src::Tex(hit),
                    _ => Src::None,
                }
            }
            "mix" => {
                let fg = self.src(link.node, "fg", depth);
                let bg = self.src(link.node, "bg", depth);
                match self.num(link.node, "mix") {
                    Some(m) if m[0] >= 0.5 => fg,
                    Some(_) => bg,
                    None if matches!(fg, Src::Tex(_)) => fg,
                    None => bg,
                }
            }
            _ => Src::None,
        }
    }

    /// The texture's own multiplier: UsdUVTexture's `scale`, glTF image
    /// nodes' `factor`.
    fn image_factor(&self, node: usize) -> [f32; 4] {
        let cat = self.nodes[node].category.as_str();
        let name = match cat {
            "UsdUVTexture" => "scale",
            "gltf_image" | "gltf_normalmap" => "factor",
            "gltf_colorimage" => "color",
            _ => return [1.0; 4],
        };
        match self.num(node, name) {
            Some(v) if v.len() >= 4 => [v[0], v[1], v[2], v[3]],
            Some(v) if v.len() == 3 => [v[0], v[1], v[2], 1.0],
            Some(v) => [v[0]; 4],
            None => [1.0; 4],
        }
    }

    /// The texture-coordinate transform (in OpenGL's convention) and UV set
    /// an image node reads with.
    fn image_uv(&self, node: usize) -> (Mat3, u8) {
        let cat = self.nodes[node].category.as_str();
        let coord_input = if cat == "UsdUVTexture" { "st" } else { "texcoord" };
        let (mut m, set) = match self.input(node, coord_input).and_then(|i| i.link.as_ref()) {
            Some(link) => self.uv_chain(link, 0),
            None => (Mat3::IDENTITY, 0),
        };
        match cat {
            "tiledimage" => {
                let tiling = self.vec2(node, "uvtiling", Vec2::ONE);
                let offset = self.vec2(node, "uvoffset", Vec2::ZERO);
                m = Mat3::from_translation(-offset) * Mat3::from_scale(tiling) * m;
            }
            "gltf_image" | "gltf_colorimage" | "gltf_normalmap" => m = self.place2d(node, Vec2::new(0.0, 1.0)) * m,
            _ => {}
        }
        (m, set)
    }

    fn uv_chain(&self, link: &Link, depth: u32) -> (Mat3, u8) {
        let Some(n) = self.nodes.get(link.node) else { return (Mat3::IDENTITY, 0) };
        if depth > MAX_DEPTH {
            return (Mat3::IDENTITY, 0);
        }
        let inner = |name: &str| match self.input(link.node, name).and_then(|i| i.link.as_ref()) {
            Some(l) => self.uv_chain(l, depth + 1),
            None => (Mat3::IDENTITY, 0),
        };
        match n.category.as_str() {
            "UsdTransform2d" => {
                let (m, set) = inner("in");
                let scale = self.vec2(link.node, "scale", Vec2::ONE);
                let rotation = self.num1(link.node, "rotation", 0.0).to_radians();
                let translation = self.vec2(link.node, "translation", Vec2::ZERO);
                let t = Mat3::from_translation(translation) * Mat3::from_angle(rotation) * Mat3::from_scale(scale);
                (t * m, set)
            }
            "place2d" => {
                let (m, set) = inner("texcoord");
                (self.place2d(link.node, Vec2::ZERO) * m, set)
            }
            "multiply" | "add" => {
                for (coord, other) in [("in1", "in2"), ("in2", "in1")] {
                    let Some(l) = self.input(link.node, coord).and_then(|i| i.link.as_ref()) else { continue };
                    let (m, set) = self.uv_chain(l, depth + 1);
                    let v = self.vec2(link.node, other, if n.category == "add" { Vec2::ZERO } else { Vec2::ONE });
                    let t = if n.category == "add" { Mat3::from_translation(v) } else { Mat3::from_scale(v) };
                    return (t * m, set);
                }
                (Mat3::IDENTITY, 0)
            }
            "texcoord" => (Mat3::IDENTITY, self.num1(link.node, "index", 0.0).clamp(0.0, 1.0) as u8),
            _ => (Mat3::IDENTITY, 0),
        }
    }

    /// MaterialX's `place2d`: subtract the pivot, divide by scale, rotate
    /// clockwise by `rotate` degrees, subtract the offset, add the pivot back.
    fn place2d(&self, node: usize, default_pivot: Vec2) -> Mat3 {
        let pivot = self.vec2(node, "pivot", default_pivot);
        let scale = self.vec2(node, "scale", Vec2::ONE);
        let rotate = self.num1(node, "rotate", 0.0).to_radians();
        let offset = self.vec2(node, "offset", Vec2::ZERO);
        let inv_scale = Vec2::new(recip(scale.x), recip(scale.y));
        let srt = Mat3::from_angle(-rotate) * Mat3::from_scale(inv_scale);
        if self.num1(node, "operationorder", 0.0) as i32 == 1 {
            Mat3::from_translation(pivot) * srt * Mat3::from_translation(-offset - pivot)
        } else {
            Mat3::from_translation(pivot - offset) * srt * Mat3::from_translation(-pivot)
        }
    }

    fn tex_input(&self, hit: &Hit) -> Option<TexInput> {
        let n = &self.nodes[hit.image];
        let file_input = self.input(hit.image, "file")?;
        let Some(GValue::File { file, label }) = &file_input.value else { return None };
        let mut t = TexInput::new(file.clone(), label.clone(), hit.channel);
        t.color_space = if n.category == "UsdUVTexture" {
            match self.string(hit.image, "sourceColorSpace") {
                Some("sRGB") => ColorSpace::Srgb,
                Some("raw") => ColorSpace::Linear,
                _ => file_input.color_space.as_deref().map_or(ColorSpace::Unspecified, color_space),
            }
        } else if n.ty.starts_with("color") {
            file_input.color_space.as_deref().map_or(ColorSpace::Unspecified, color_space)
        } else {
            ColorSpace::Unspecified
        };
        let (m, set) = self.image_uv(hit.image);
        t.transform = uv_transform_from_gl(m);
        t.uv_set = set;
        t.sampler = self.sampler(hit.image);
        let fallback_name = if n.category == "UsdUVTexture" { "fallback" } else { "default" };
        t.fallback = self.num(hit.image, fallback_name).map(|v| match v.len() {
            0 => [1.0; 4],
            1 | 2 => [v[0], v[0], v[0], 1.0],
            3 => [v[0], v[1], v[2], 1.0],
            _ => [v[0], v[1], v[2], v[3]],
        });
        Some(t)
    }

    fn sampler(&self, node: usize) -> Sampler {
        let mut s = Sampler::default();
        let usd = self.nodes[node].category == "UsdUVTexture";
        let wrap = |name: &str| -> Wrap {
            match self.string(node, name) {
                Some("clamp" | "black" | "constant") => Wrap::ClampToEdge,
                Some("mirror") => Wrap::MirroredRepeat,
                _ => Wrap::Repeat,
            }
        };
        s.wrap_u = wrap(if usd { "wrapS" } else { "uaddressmode" });
        s.wrap_v = wrap(if usd { "wrapT" } else { "vaddressmode" });
        if self.string(node, "filtertype") == Some("closest") {
            s.mag = Filter::Nearest;
            s.min = Filter::Nearest;
        }
        s
    }
}

fn recip(x: f32) -> f32 {
    if x.abs() > 1e-8 { 1.0 / x } else { 1.0 }
}

fn image_channel(n: &GNode, output: &str) -> Channel {
    match output {
        "r" => Channel::R,
        "g" => Channel::G,
        "b" => Channel::B,
        "a" | "outa" => Channel::A,
        "rgb" | "rgba" | "outcolor" => Channel::Rgb,
        _ if n.ty == "float" => Channel::R,
        _ => Channel::Rgb,
    }
}

fn pick_channel(src: Src, index: usize) -> Src {
    match src {
        Src::Tex(mut hit) => {
            hit.channel = match (hit.channel, index) {
                (Channel::Rgb, 1) => Channel::G,
                (Channel::Rgb, 2) => Channel::B,
                (Channel::Rgb, 3) => Channel::A,
                (Channel::Rgb, _) => Channel::R,
                (other, _) => other,
            };
            let f = hit.factor[index.min(3)];
            hit.factor = [f; 4];
            Src::Tex(hit)
        }
        Src::Const(v) => v.get(index).map_or(Src::None, |&x| Src::Const(vec![x])),
        Src::None => Src::None,
    }
}

/// Maps a MaterialX or USD colour space name.
pub(crate) fn color_space(name: &str) -> ColorSpace {
    let lower = name.to_ascii_lowercase();
    if lower.contains("srgb") && !lower.starts_with("lin") || lower.starts_with("g22") || lower.contains("gamma") {
        ColorSpace::Srgb
    } else if lower.starts_with("lin") || lower.contains("raw") || lower.contains("acescg") || lower == "data" {
        ColorSpace::Linear
    } else {
        ColorSpace::Unspecified
    }
}

//endregion

/*┌─────────────────╮
| Mapping         |
└─────────────────╯*/
//region Mapping

impl Graph {
    fn color(&self, node: usize, name: &str, default: Vec3) -> (Vec3, Option<TexInput>) {
        match self.src(node, name, 0) {
            Src::Const(v) => (vec3(&v, default), None),
            Src::Tex(hit) => match self.tex_input(&hit) {
                Some(t) => (Vec3::new(hit.factor[0], hit.factor[1], hit.factor[2]), Some(t)),
                None => (default, None),
            },
            Src::None => (default, None),
        }
    }

    fn scalar(&self, node: usize, name: &str, default: f32) -> (f32, Option<TexInput>) {
        match self.src(node, name, 0) {
            Src::Const(v) => (v.first().copied().unwrap_or(default), None),
            Src::Tex(hit) => match self.tex_input(&hit) {
                Some(mut t) => {
                    if t.channel == Channel::Rgb {
                        t.channel = Channel::R;
                    }
                    (hit.factor[t.channel.index()], Some(t))
                }
                None => (default, None),
            },
            Src::None => (default, None),
        }
    }

    fn normal(&self, node: usize, name: &str) -> Option<(TexInput, f32)> {
        let Src::Tex(hit) = self.src(node, name, 0) else { return None };
        let mut t = self.tex_input(&hit)?;
        t.channel = Channel::Rgb;
        let mut scale = hit.normal_scale;
        if self.nodes[hit.image].category == "UsdUVTexture" {
            // USD decodes normals as texel * scale + bias; (2, -1) is the
            // usual decode, so other scales strengthen or weaken the map.
            let s = hit.factor[0].abs();
            if s > 0.0 {
                scale *= s / 2.0;
            }
        }
        Some((t, scale))
    }

    /// Maps a surface shader, and optionally a MaterialX `displacement`
    /// node, onto a material.  `unit` is metres per scene unit, for
    /// displacement distances.
    pub fn map(&self, name: &str, surface: usize, displacement: Option<usize>, unit: f32) -> MatDesc {
        let cat = self.nodes[surface].category.clone();
        let mut d = MatDesc::new(name, &cat);
        match cat.as_str() {
            "UsdPreviewSurface" => self.usd_preview(&mut d, surface, unit),
            "standard_surface" => self.standard_surface(&mut d, surface),
            "open_pbr_surface" => self.open_pbr(&mut d, surface),
            "gltf_pbr" => self.gltf_pbr(&mut d, surface),
            "surface_unlit" => self.unlit(&mut d, surface),
            "disney_principled" => self.disney(&mut d, surface),
            _ => {}
        }
        if let Some(disp) = displacement.filter(|&n| self.nodes.get(n).is_some_and(|n| n.category == "displacement")) {
            let (scale, tex) = self.scalar(disp, "displacement", 0.0);
            if let Some(t) = tex {
                d.material.height_scale = scale * self.num1(disp, "scale", 1.0) * unit;
                d.height = Some(t);
            }
        }
        d
    }

    fn opacity(&self, d: &mut MatDesc, node: usize, name: &str, default: f32) {
        let (o, tex) = match self.src(node, name, 0) {
            // Standard Surface's opacity is a colour: use its mean.
            Src::Const(v) if v.len() >= 3 => ((v[0] + v[1] + v[2]) / 3.0, None),
            _ => self.scalar(node, name, default),
        };
        d.material.base_color.w = o;
        if tex.is_some() || o < 1.0 {
            d.material.alpha_mode = AlphaMode::Blend;
        }
        d.opacity = tex;
    }

    /// Our glTF model has no transmission, so see-through materials are
    /// drawn as partly transparent, tinted by their transmission colour
    /// (`color`, when the shading model has one) rather than their base
    /// colour, which a glass usually sets to black.
    fn transmission(&self, d: &mut MatDesc, node: usize, name: &str, color: Option<&str>) {
        let t = self.num1(node, name, 0.0).clamp(0.0, 1.0);
        if t > 0.0 {
            if let Some(c) = color {
                let tint = self.num(node, c).map_or(Vec3::ONE, |v| vec3(&v, Vec3::ONE));
                let base = d.material.base_color.truncate().lerp(tint, t);
                d.material.base_color = base.extend(d.material.base_color.w);
            }
            d.material.base_color.w *= 1.0 - 0.8 * t;
            d.material.alpha_mode = AlphaMode::Blend;
        }
    }

    fn usd_preview(&self, d: &mut MatDesc, n: usize, unit: f32) {
        let m = &mut d.material;
        let (base, tex) = self.color(n, "diffuseColor", Vec3::splat(0.18));
        m.base_color = base.extend(1.0);
        d.base_color = tex;
        let (emissive, tex) = self.color(n, "emissiveColor", Vec3::ZERO);
        d.material.emissive = emissive;
        d.emissive = tex;
        if self.num1(n, "useSpecularWorkflow", 0.0) != 0.0 {
            // Specular workflow: approximated as a dielectric tinted by the
            // specular colour.
            d.material.metallic = 0.0;
            if let Some(v) = self.num(n, "specularColor") {
                let c = vec3(&v, Vec3::ZERO);
                d.material.specular_color = (c / 0.04).min(Vec3::ONE);
            }
        } else {
            let (metallic, tex) = self.scalar(n, "metallic", 0.0);
            d.material.metallic = metallic;
            d.metallic = tex;
        }
        let (roughness, tex) = self.scalar(n, "roughness", 0.5);
        d.material.roughness = roughness;
        d.roughness = tex;
        d.material.clearcoat = self.num1(n, "clearcoat", 0.0);
        d.material.clearcoat_roughness = self.num1(n, "clearcoatRoughness", 0.01);
        d.material.ior = self.num1(n, "ior", 1.5);
        self.opacity(d, n, "opacity", 1.0);
        let threshold = self.num1(n, "opacityThreshold", 0.0);
        if threshold > 0.0 {
            d.material.alpha_mode = AlphaMode::Mask(threshold);
        }
        if let Some((t, s)) = self.normal(n, "normal") {
            d.normal = Some(t);
            d.material.normal_scale = s;
        }
        let (occlusion, tex) = self.scalar(n, "occlusion", 1.0);
        if tex.is_some() {
            d.material.occlusion_strength = occlusion.clamp(0.0, 1.0);
            d.occlusion = tex;
        }
        let (height, tex) = self.scalar(n, "displacement", 0.0);
        if tex.is_some() {
            d.material.height_scale = height * unit;
            d.height = tex;
        }
    }

    fn standard_surface(&self, d: &mut MatDesc, n: usize) {
        let weight = self.num1(n, "base", 1.0);
        let (base, tex) = self.color(n, "base_color", Vec3::splat(0.8));
        d.material.base_color = (base * weight).extend(1.0);
        d.base_color = tex;
        let (metallic, tex) = self.scalar(n, "metalness", 0.0);
        d.material.metallic = metallic;
        d.metallic = tex;
        let (roughness, tex) = self.scalar(n, "specular_roughness", 0.2);
        d.material.roughness = roughness;
        d.roughness = tex;
        d.material.specular = self.num1(n, "specular", 1.0);
        d.material.specular_color = self.num(n, "specular_color").map_or(Vec3::ONE, |v| vec3(&v, Vec3::ONE));
        d.material.ior = self.num1(n, "specular_IOR", 1.5);
        d.material.clearcoat = self.num1(n, "coat", 0.0);
        d.material.clearcoat_roughness = self.num1(n, "coat_roughness", 0.1);
        let strength = self.num1(n, "emission", 0.0);
        let (color, tex) = self.color(n, "emission_color", Vec3::ONE);
        if strength > 0.0 {
            d.material.emissive = color * strength;
            d.emissive = tex;
        }
        self.opacity(d, n, "opacity", 1.0);
        self.transmission(d, n, "transmission", Some("transmission_color"));
        d.material.double_sided = self.num1(n, "thin_walled", 0.0) != 0.0;
        if let Some((t, s)) = self.normal(n, "normal") {
            d.normal = Some(t);
            d.material.normal_scale = s;
        }
    }

    fn open_pbr(&self, d: &mut MatDesc, n: usize) {
        let weight = self.num1(n, "base_weight", 1.0);
        let (base, tex) = self.color(n, "base_color", Vec3::splat(0.8));
        d.material.base_color = (base * weight).extend(1.0);
        d.base_color = tex;
        let (metallic, tex) = self.scalar(n, "base_metalness", 0.0);
        d.material.metallic = metallic;
        d.metallic = tex;
        let (roughness, tex) = self.scalar(n, "specular_roughness", 0.3);
        d.material.roughness = roughness;
        d.roughness = tex;
        d.material.specular = self.num1(n, "specular_weight", 1.0);
        d.material.specular_color = self.num(n, "specular_color").map_or(Vec3::ONE, |v| vec3(&v, Vec3::ONE));
        d.material.ior = self.num1(n, "specular_ior", 1.5);
        d.material.clearcoat = self.num1(n, "coat_weight", 0.0);
        d.material.clearcoat_roughness = self.num1(n, "coat_roughness", 0.0);
        // Emission is in nits; 1000 nits is taken as full white.
        let luminance = self.num1(n, "emission_luminance", 0.0);
        let (color, tex) = self.color(n, "emission_color", Vec3::ONE);
        if luminance > 0.0 {
            d.material.emissive = color * (luminance / 1000.0);
            d.emissive = tex;
        }
        self.opacity(d, n, "geometry_opacity", 1.0);
        self.transmission(d, n, "transmission_weight", Some("transmission_color"));
        d.material.double_sided = self.num1(n, "geometry_thin_walled", 0.0) != 0.0;
        if let Some((t, s)) = self.normal(n, "geometry_normal") {
            d.normal = Some(t);
            d.material.normal_scale = s;
        }
    }

    fn gltf_pbr(&self, d: &mut MatDesc, n: usize) {
        let (base, tex) = self.color(n, "base_color", Vec3::ONE);
        d.material.base_color = base.extend(1.0);
        d.base_color = tex;
        let (metallic, tex) = self.scalar(n, "metallic", 1.0);
        d.material.metallic = metallic;
        d.metallic = tex;
        let (roughness, tex) = self.scalar(n, "roughness", 1.0);
        d.material.roughness = roughness;
        d.roughness = tex;
        let (occlusion, tex) = self.scalar(n, "occlusion", 1.0);
        if tex.is_some() {
            d.material.occlusion_strength = occlusion.clamp(0.0, 1.0);
            d.occlusion = tex;
        }
        let strength = self.num1(n, "emissive_strength", 1.0);
        let (emissive, tex) = self.color(n, "emissive", Vec3::ZERO);
        d.material.emissive = emissive * strength;
        d.emissive = tex;
        d.material.specular = self.num1(n, "specular", 1.0);
        d.material.specular_color = self.num(n, "specular_color").map_or(Vec3::ONE, |v| vec3(&v, Vec3::ONE));
        d.material.ior = self.num1(n, "ior", 1.5);
        d.material.clearcoat = self.num1(n, "clearcoat", 0.0);
        d.material.clearcoat_roughness = self.num1(n, "clearcoat_roughness", 0.0);
        let (alpha, tex) = self.scalar(n, "alpha", 1.0);
        d.material.base_color.w = alpha;
        d.opacity = tex;
        d.material.alpha_mode = match self.num1(n, "alpha_mode", 0.0) as i32 {
            1 => AlphaMode::Mask(self.num1(n, "alpha_cutoff", 0.5)),
            2 => AlphaMode::Blend,
            _ => AlphaMode::Opaque,
        };
        self.transmission(d, n, "transmission", None);
        if let Some((t, s)) = self.normal(n, "normal") {
            d.normal = Some(t);
            d.material.normal_scale = s;
        }
    }

    fn unlit(&self, d: &mut MatDesc, n: usize) {
        let strength = self.num1(n, "emission", 1.0);
        let (color, tex) = self.color(n, "emission_color", Vec3::ONE);
        d.material.base_color = (color * strength).extend(1.0);
        d.base_color = tex;
        d.material.unlit = true;
        self.opacity(d, n, "opacity", 1.0);
    }

    fn disney(&self, d: &mut MatDesc, n: usize) {
        let (base, tex) = self.color(n, "baseColor", Vec3::new(0.16, 0.16, 0.16));
        d.material.base_color = base.extend(1.0);
        d.base_color = tex;
        let (metallic, tex) = self.scalar(n, "metallic", 0.0);
        d.material.metallic = metallic;
        d.metallic = tex;
        let (roughness, tex) = self.scalar(n, "roughness", 0.5);
        d.material.roughness = roughness;
        d.roughness = tex;
        d.material.clearcoat = self.num1(n, "clearcoat", 0.0);
        d.material.clearcoat_roughness = 1.0 - self.num1(n, "clearcoatGloss", 1.0);
        self.transmission(d, n, "specTrans", None);
    }
}

fn vec3(v: &[f32], default: Vec3) -> Vec3 {
    match v.len() {
        0 => default,
        1 | 2 => Vec3::splat(v[0]),
        _ => Vec3::new(v[0], v[1], v[2]),
    }
}

//endregion
