//! COLLADA `.dae`, versions 1.4 and 1.5, read with a small reader of our own
//! on roxmltree.
//!
//! Covered: the node hierarchy with `matrix`, `translate`, `rotate`, `scale`
//! and `lookat` transforms, instanced nodes, `triangles`, `polylist`,
//! `polygons`, `trifans`, `tristrips`, `lines` and `linestrips` with any
//! number of inputs and offsets, materials bound through
//! `instance_material` to `profile_COMMON` effects (colours or textures for
//! diffuse, emission and transparency, Phong shininess, bump maps from the
//! common `extra` techniques), images by `init_from` or embedded hex, skin
//! controllers (bind shape matrix, inverse bind matrices, any joint count),
//! morph controllers, and animations baked per node to linear keyframes and
//! split by `animation_clip` when the file has them.  `up_axis` and `unit`
//! put the scene Y-up in metres.

use crate::common::{
    FastMap, TextureSet, check_triangles, flipped_uv_transform, linear_color, parse_f32, parse_i64, srgb_color, tokens,
    triangulate,
};
use glam::{Mat3, Mat4, Quat, Vec2, Vec3, Vec4};
use roxmltree::Node as X;
use slerp_scene::{
    AlphaMode, Animation, Channel, Interpolation, Limits, LoadError, Material, Mesh, MorphTarget, Node, Primitive,
    Property, Scene, Skin, TexRef, Topology, Transform,
};
use std::collections::HashMap;
use std::path::Path;
use std::rc::Rc;

/// Deeper node trees and `instance_node` chains are malformed or malicious.
const MAX_DEPTH: usize = 256;
/// Instanced nodes can multiply; the scene stops growing past this many.
const MAX_NODES: usize = 1 << 20;
/// Baked animation keys per node are capped at this many.
const MAX_KEYS: usize = 100_000;

/*┌─────────────────╮
| XML helpers     |
└─────────────────╯*/
//region XML helpers

fn children<'a, 'i>(node: X<'a, 'i>, name: &'static str) -> impl Iterator<Item = X<'a, 'i>> {
    node.children().filter(move |c| c.is_element() && c.tag_name().name() == name)
}

fn child<'a, 'i>(node: X<'a, 'i>, name: &'static str) -> Option<X<'a, 'i>> {
    children(node, name).next()
}

fn floats(node: Option<X>) -> Vec<f32> {
    node.and_then(|n| n.text())
        .map(|t| tokens(t.as_bytes()).map(|v| number(v).unwrap_or(0.0)).collect())
        .unwrap_or_default()
}

/// A number, also accepting a decimal comma: some exporters write
/// "0,01" under a German or French locale, and COLLADA separates values
/// with whitespace only, so the comma can't mean anything else.
fn number(token: &[u8]) -> Option<f32> {
    parse_f32(token).or_else(|| {
        let fixed: Vec<u8> = token.iter().map(|&b| if b == b',' { b'.' } else { b }).collect();
        parse_f32(&fixed)
    })
}

fn ints(node: Option<X>) -> Vec<i64> {
    node.and_then(|n| n.text())
        .map(|t| tokens(t.as_bytes()).map(|v| parse_i64(v).unwrap_or(-1)).collect())
        .unwrap_or_default()
}

/// The id a URL such as `#geom-1` points to.
fn url(s: &str) -> &str {
    s.strip_prefix('#').unwrap_or(s)
}

/// A row-major 4x4 matrix from 16 floats.
fn matrix(v: &[f32]) -> Mat4 {
    if v.len() < 16 {
        return Mat4::IDENTITY;
    }
    let m = Mat4::from_cols_slice(&v[..16]).transpose();
    if m.is_finite() { m } else { Mat4::IDENTITY }
}

//endregion

/*┌─────────────────╮
| Sources         |
└─────────────────╯*/
//region Sources

/// A `<source>`: an array of floats or names read through its accessor.
#[derive(Default)]
struct Source {
    floats: Vec<f32>,
    names: Vec<String>,
    stride: usize,
    offset: usize,
    count: usize,
}

impl Source {
    fn read(node: X) -> Source {
        let mut s = Source { stride: 1, ..Source::default() };
        if let Some(a) = child(node, "float_array") {
            s.floats = floats(Some(a));
        } else if let Some(a) =
            child(node, "Name_array").or_else(|| child(node, "IDREF_array")).or_else(|| child(node, "SIDREF_array"))
        {
            s.names = a.text().map(|t| t.split_whitespace().map(str::to_string).collect()).unwrap_or_default();
        } else if let Some(a) = child(node, "int_array") {
            s.floats = ints(Some(a)).into_iter().map(|v| v as f32).collect();
        }
        let len = s.floats.len().max(s.names.len());
        let accessor = child(node, "technique_common").and_then(|t| child(t, "accessor"));
        s.count = len;
        if let Some(a) = accessor {
            s.stride =
                a.attribute("stride").and_then(|v| v.parse().ok()).filter(|&v: &usize| v > 0 && v <= 64).unwrap_or(1);
            s.offset = a.attribute("offset").and_then(|v| v.parse().ok()).unwrap_or(0);
            let stated: usize = a.attribute("count").and_then(|v| v.parse().ok()).unwrap_or(usize::MAX);
            s.count = stated.min(len.saturating_sub(s.offset) / s.stride);
        }
        s
    }

    fn get(&self, i: usize, component: usize) -> f32 {
        if i >= self.count || component >= self.stride {
            return 0.0;
        }
        self.floats.get(self.offset + i * self.stride + component).copied().unwrap_or(0.0)
    }

    fn vec3(&self, i: usize) -> Vec3 {
        Vec3::new(self.get(i, 0), self.get(i, 1), self.get(i, 2))
    }

    fn mat4(&self, i: usize) -> Mat4 {
        let start = self.offset + i * self.stride;
        self.floats.get(start..start + 16).map_or(Mat4::IDENTITY, matrix)
    }
}

//endregion

/*┌─────────────────╮
| Geometry        |
└─────────────────╯*/
//region Geometry

/// One primitive element of a geometry before materials are bound.
#[derive(Clone)]
struct GeomPrim {
    symbol: String,
    prim: Primitive,
    /// The POSITION index each vertex came from, for skins and morphs.
    source: Vec<u32>,
}

/// A face of a primitive element, as corner numbers.
enum Face {
    /// `n` consecutive corners from `start`.
    Run(usize, usize),
    Tri([usize; 3]),
}

struct Input {
    semantic: String,
    source: Rc<Source>,
    offset: usize,
    set: u32,
}

struct Reader<'a, 'i> {
    ids: HashMap<&'a str, X<'a, 'i>>,
    sources: HashMap<String, Rc<Source>>,
    geometries: HashMap<String, Rc<Vec<GeomPrim>>>,
    limits: &'a Limits,
    triangles: u64,
    warnings: Vec<String>,
}

impl<'a, 'i> Reader<'a, 'i> {
    fn source(&mut self, id: &str) -> Rc<Source> {
        let id = url(id);
        if let Some(s) = self.sources.get(id) {
            return s.clone();
        }
        let s = Rc::new(
            self.ids.get(id).filter(|n| n.tag_name().name() == "source").map(|n| Source::read(*n)).unwrap_or_default(),
        );
        self.sources.insert(id.to_string(), s.clone());
        s
    }

    /// Inputs of a primitive element, with `VERTEX` expanded into the
    /// `<vertices>` element's own inputs at the same offset.
    fn inputs(&mut self, prim: X, vertices: Option<X>) -> Vec<Input> {
        let mut out = Vec::new();
        for i in children(prim, "input") {
            let semantic = i.attribute("semantic").unwrap_or_default().to_string();
            let offset = i.attribute("offset").and_then(|v| v.parse().ok()).unwrap_or(0).min(64);
            let set = i.attribute("set").and_then(|v| v.parse().ok()).unwrap_or(0);
            if semantic == "VERTEX" {
                let target = i.attribute("source").map(url).and_then(|id| self.ids.get(id).copied()).or(vertices);
                if let Some(v) = target {
                    for vi in children(v, "input") {
                        let semantic = vi.attribute("semantic").unwrap_or_default().to_string();
                        let source = self.source(vi.attribute("source").unwrap_or_default());
                        out.push(Input { semantic, source, offset, set: 0 });
                    }
                }
            } else {
                let source = self.source(i.attribute("source").unwrap_or_default());
                out.push(Input { semantic, source, offset, set });
            }
        }
        out
    }

    /// Reads a `<geometry>`'s mesh into primitives, once per geometry.
    fn geometry(&mut self, id: &str) -> Result<Rc<Vec<GeomPrim>>, LoadError> {
        if let Some(g) = self.geometries.get(id) {
            return Ok(g.clone());
        }
        let mut out = Vec::new();
        if let Some(mesh) = self.ids.get(id).and_then(|g| child(*g, "mesh")) {
            let vertices = child(mesh, "vertices");
            for p in mesh.children().filter(|c| c.is_element()) {
                let kind = p.tag_name().name();
                if !matches!(
                    kind,
                    "triangles" | "polylist" | "polygons" | "trifans" | "tristrips" | "lines" | "linestrips"
                ) {
                    continue;
                }
                let inputs = self.inputs(p, vertices);
                if let Some(gp) = self.primitive(p, kind, &inputs)? {
                    out.push(gp);
                }
            }
        }
        let g = Rc::new(out);
        self.geometries.insert(id.to_string(), g.clone());
        Ok(g)
    }

    fn primitive(&mut self, p: X, kind: &str, inputs: &[Input]) -> Result<Option<GeomPrim>, LoadError> {
        let Some(position) = inputs.iter().find(|i| i.semantic == "POSITION") else { return Ok(None) };
        let stride = inputs.iter().map(|i| i.offset).max().unwrap_or(0) + 1;
        let normal = inputs.iter().find(|i| i.semantic == "NORMAL");
        let mut uv_sets: Vec<&Input> = inputs.iter().filter(|i| i.semantic == "TEXCOORD").collect();
        uv_sets.sort_by_key(|i| i.set);
        let color = inputs.iter().find(|i| i.semantic == "COLOR");

        // 1. Faces as corner numbers into the index array: runs of
        //    consecutive corners, or explicit triangles from strips and fans.
        let mut indices: Vec<i64> = Vec::new();
        let mut faces: Vec<Face> = Vec::new();
        let topology = if matches!(kind, "lines" | "linestrips") { Topology::Lines } else { Topology::Triangles };
        match kind {
            "triangles" | "lines" => {
                indices = ints(child(p, "p"));
                let n = if kind == "lines" { 2 } else { 3 };
                let corners = indices.len() / stride;
                faces.extend((0..corners / n).map(|i| Face::Run(i * n, n)));
            }
            "polylist" => {
                indices = ints(child(p, "p"));
                let mut at = 0usize;
                for count in ints(child(p, "vcount")) {
                    let count = count.clamp(0, 1 << 20) as usize;
                    faces.push(Face::Run(at, count));
                    at = at.saturating_add(count);
                }
            }
            _ => {
                // polygons, trifans, tristrips, linestrips: one <p> each.
                for el in children(p, "p") {
                    let part = ints(Some(el));
                    let start = indices.len() / stride;
                    let corners = part.len() / stride;
                    indices.extend(&part[..corners * stride]);
                    match kind {
                        "tristrips" => faces.extend((2..corners).map(|i| {
                            let c = start + i;
                            Face::Tri(if i % 2 == 0 { [c - 2, c - 1, c] } else { [c - 1, c - 2, c] })
                        })),
                        "trifans" => faces.extend((2..corners).map(|i| Face::Tri([start, start + i - 1, start + i]))),
                        "linestrips" => faces.extend((1..corners).map(|i| Face::Run(start + i - 1, 2))),
                        _ => faces.push(Face::Run(start, corners)),
                    }
                }
            }
        }
        let total_corners = indices.len() / stride;
        let tri_estimate: u64 = faces
            .iter()
            .map(|f| match *f {
                Face::Run(_, n) => n.saturating_sub(2) as u64,
                Face::Tri(_) => 1,
            })
            .sum();
        if topology == Topology::Triangles {
            self.triangles += tri_estimate;
            check_triangles(self.triangles, self.limits)?;
        }

        // 2. Deduplicate corners into vertices.
        let index_of = |corner: usize, input: &Input| -> Option<u32> {
            let v = *indices.get(corner * stride + input.offset)?;
            (v >= 0 && (v as usize) < input.source.count).then_some(v as u32)
        };
        let mut gp = GeomPrim {
            symbol: p.attribute("material").unwrap_or_default().to_string(),
            prim: Primitive { topology, ..Default::default() },
            source: Vec::new(),
        };
        let mut remap: FastMap<[u32; 5], u32> = FastMap::default();
        let none = u32::MAX;
        let mut vertex = |corner: usize, gp: &mut GeomPrim| -> Option<u32> {
            let pi = index_of(corner, position)?;
            let key = [
                pi,
                normal.and_then(|n| index_of(corner, n)).unwrap_or(none),
                uv_sets.first().and_then(|u| index_of(corner, u)).unwrap_or(none),
                uv_sets.get(1).and_then(|u| index_of(corner, u)).unwrap_or(none),
                color.and_then(|c| index_of(corner, c)).unwrap_or(none),
            ];
            let next = gp.source.len() as u32;
            let i = *remap.entry(key).or_insert(next);
            if i == next {
                let prim = &mut gp.prim;
                prim.positions.push(position.source.vec3(pi as usize));
                if let Some(n) = normal {
                    prim.normals.push(if key[1] == none { Vec3::Y } else { n.source.vec3(key[1] as usize) });
                }
                for (k, slot) in [(0usize, &mut prim.uv0), (1usize, &mut prim.uv1)] {
                    if let Some(u) = uv_sets.get(k) {
                        let idx = key[2 + k];
                        let uv = if idx == none {
                            Vec2::ZERO
                        } else {
                            Vec2::new(u.source.get(idx as usize, 0), 1.0 - u.source.get(idx as usize, 1))
                        };
                        slot.push(uv);
                    }
                }
                if let Some(c) = color {
                    let idx = key[4] as usize;
                    let a = if c.source.stride >= 4 { c.source.get(idx, 3) } else { 1.0 };
                    prim.colors.push(if key[4] == none {
                        Vec4::ONE
                    } else {
                        srgb_color(c.source.get(idx, 0), c.source.get(idx, 1), c.source.get(idx, 2), a)
                    });
                }
                gp.source.push(pi);
            }
            Some(i)
        };

        // 3. Triangulate.
        let mut tris = Vec::new();
        let mut pts = Vec::new();
        let mut corner_vertices = Vec::new();
        for face in &faces {
            let corners: Vec<usize> = match *face {
                Face::Run(start, n) => (start..start.saturating_add(n)).collect(),
                Face::Tri(t) => t.to_vec(),
            };
            if corners.iter().any(|&c| c >= total_corners) {
                continue;
            }
            corner_vertices.clear();
            for &c in &corners {
                corner_vertices.push(vertex(c, &mut gp));
            }
            let Some(vs) = corner_vertices.iter().copied().collect::<Option<Vec<u32>>>() else { continue };
            if topology == Topology::Lines {
                for pair in vs.windows(2) {
                    gp.prim.indices.extend(pair);
                }
                continue;
            }
            if vs.len() < 3 {
                continue;
            }
            pts.clear();
            pts.extend(vs.iter().map(|&v| gp.prim.positions[v as usize]));
            tris.clear();
            triangulate(&pts, &mut tris);
            for t in &tris {
                gp.prim.indices.extend(t.map(|k| vs[k as usize]));
            }
        }
        Ok((!gp.prim.indices.is_empty()).then_some(gp))
    }
}

//endregion

/*┌─────────────────╮
| Materials       |
└─────────────────╯*/
//region Materials

/// One `profile_COMMON` parameter: a colour, a float, or a texture.
enum Param {
    Color(Vec4),
    Float(f32),
    Texture { image: String, texcoord: String },
}

fn param(node: Option<X>) -> Option<Param> {
    let node = node?;
    if let Some(c) = child(node, "color") {
        let v = floats(Some(c));
        return Some(Param::Color(Vec4::new(
            v.first().copied().unwrap_or(0.0),
            v.get(1).copied().unwrap_or(0.0),
            v.get(2).copied().unwrap_or(0.0),
            v.get(3).copied().unwrap_or(1.0),
        )));
    }
    if let Some(f) = child(node, "float") {
        return floats(Some(f)).first().map(|&v| Param::Float(v));
    }
    child(node, "texture").map(|t| Param::Texture {
        image: t.attribute("texture").unwrap_or_default().to_string(),
        texcoord: t.attribute("texcoord").unwrap_or_default().to_string(),
    })
}

struct MaterialBuilder<'r, 'a, 'i, 't> {
    reader: &'r Reader<'a, 'i>,
    textures: &'r mut TextureSet<'t>,
}

impl MaterialBuilder<'_, '_, '_, '_> {
    /// Follows a texture reference (a sampler's sid, a surface's sid, or an
    /// image id) to an image, and loads it.
    fn image(&mut self, effect: X, name: &str) -> Option<usize> {
        let mut name = name.to_string();
        for _ in 0..4 {
            if let Some(image) = self.reader.ids.get(name.as_str()).filter(|n| n.tag_name().name() == "image") {
                return self.load_image(*image);
            }
            let newparam = effect
                .descendants()
                .find(|n| n.tag_name().name() == "newparam" && n.attribute("sid") == Some(name.as_str()))?;
            let target = newparam.descendants().find_map(|n| match n.tag_name().name() {
                "source" => n.text().map(|t| t.trim().to_string()),
                "init_from" if n.parent().is_some_and(|p| p.tag_name().name() == "surface") => {
                    n.text().map(|t| t.trim().to_string())
                }
                "instance_image" => n.attribute("url").map(|u| url(u).to_string()),
                _ => None,
            })?;
            name = target;
        }
        None
    }

    fn load_image(&mut self, image: X) -> Option<usize> {
        let init = child(image, "init_from");
        // COLLADA 1.5 wraps the path in <ref>; 1.4 holds it directly.
        let path =
            init.and_then(|i| child(i, "ref").and_then(|r| r.text()).or(i.text())).map(str::trim).unwrap_or_default();
        if !path.is_empty() {
            return self.textures.file(path);
        }
        // Embedded pixels: 1.4's <data> or 1.5's <init_from><hex>.
        let hex = child(image, "data").or_else(|| init.and_then(|i| child(i, "hex")))?.text()?;
        let bytes: Vec<u8> = hex
            .as_bytes()
            .iter()
            .filter(|b| b.is_ascii_hexdigit())
            .collect::<Vec<_>>()
            .chunks_exact(2)
            .map(|p| ((*p[0] as char).to_digit(16).unwrap_or(0) * 16 + (*p[1] as char).to_digit(16).unwrap_or(0)) as u8)
            .collect();
        self.textures.embedded(image.attribute("id").unwrap_or("image"), &bytes)
    }

    fn tex_ref(&mut self, effect: X, p: &Param, uv1_name: Option<&str>) -> Option<TexRef> {
        let Param::Texture { image, texcoord } = p else { return None };
        let index = self.image(effect, image)?;
        let mut r = TexRef::new(index);
        if uv1_name.is_some_and(|n| n == texcoord) {
            r.uv_set = 1;
        }
        // Maya and 3ds Max write repeat and offset in an <extra> technique.
        let extra = effect
            .descendants()
            .find(|n| n.tag_name().name() == "texture" && n.attribute("texture") == Some(image.as_str()));
        if let Some(t) = extra {
            let get = |name: &str| {
                t.descendants()
                    .find(|n| n.tag_name().name() == name)
                    .and_then(|n| n.text())
                    .and_then(|s| s.trim().parse::<f32>().ok())
            };
            let repeat = Vec2::new(get("repeatU").unwrap_or(1.0), get("repeatV").unwrap_or(1.0));
            let offset = Vec2::new(get("offsetU").unwrap_or(0.0), get("offsetV").unwrap_or(0.0));
            r.transform = flipped_uv_transform(Mat3::from_translation(offset) * Mat3::from_scale(repeat));
        }
        Some(r)
    }

    fn material(&mut self, material: X, uv1_name: Option<&str>) -> Material {
        let mut out = Material {
            name: material.attribute("name").or(material.attribute("id")).unwrap_or_default().to_string(),
            ..Material::default()
        };
        let Some(effect) = child(material, "instance_effect")
            .and_then(|e| e.attribute("url"))
            .and_then(|u| self.reader.ids.get(url(u)).copied())
        else {
            return out;
        };
        let Some(profile) = child(effect, "profile_COMMON") else { return out };
        let Some(technique) = child(profile, "technique") else { return out };
        let Some(shader) =
            technique.children().find(|c| matches!(c.tag_name().name(), "phong" | "blinn" | "lambert" | "constant"))
        else {
            return out;
        };
        let get = |name: &'static str| param(child(shader, name));

        // 1. Colour.
        match get("diffuse") {
            Some(Param::Color(c)) => out.base_color = linear_color(c.x, c.y, c.z, 1.0),
            Some(p @ Param::Texture { .. }) => {
                out.base_color_texture = self.tex_ref(effect, &p, uv1_name);
                if out.base_color_texture.is_some() {
                    out.base_color = Vec4::ONE;
                }
            }
            _ => {}
        }
        let kind = shader.tag_name().name();
        if kind == "constant" {
            out.unlit = true;
            if let Some(Param::Color(c)) = get("emission") {
                out.base_color = linear_color(c.x, c.y, c.z, 1.0);
            }
        }

        // 2. Shininess to roughness: Blinn's exponent n maps to a GGX alpha
        //    of sqrt(2 / (n + 2)), and roughness is its square root.  Some
        //    exporters write 0..1, taken as a fraction of 128.
        if matches!(kind, "phong" | "blinn") {
            if let Some(Param::Float(s)) = get("shininess") {
                let n = if s <= 1.0 { s * 128.0 } else { s }.clamp(0.0, 10_000.0);
                out.roughness = (2.0 / (n + 2.0)).powf(0.25).clamp(0.05, 1.0);
            }
            if let Some(Param::Color(c)) = get("specular") {
                out.specular = (c.truncate().max_element() * 2.0).clamp(0.0, 1.0);
            }
        } else {
            out.roughness = 0.9;
        }

        // 3. Emission.
        if kind != "constant" {
            match get("emission") {
                Some(Param::Color(c)) => out.emissive = linear_color(c.x, c.y, c.z, 1.0).truncate(),
                Some(p @ Param::Texture { .. }) => {
                    out.emissive_texture = self.tex_ref(effect, &p, uv1_name);
                    if out.emissive_texture.is_some() {
                        out.emissive = Vec3::ONE;
                    }
                }
                _ => {}
            }
        }

        // 4. Transparency, by the `opaque` mode.  A result that would make
        //    the material invisible is an exporter's inverted value.
        let transparent = child(shader, "transparent");
        let mode = transparent.and_then(|t| t.attribute("opaque")).unwrap_or("A_ONE");
        let factor = match get("transparency") {
            Some(Param::Float(f)) => f,
            _ => 1.0,
        };
        let alpha = match param(transparent) {
            Some(Param::Color(c)) => match mode {
                "RGB_ZERO" => 1.0 - c.truncate().dot(Vec3::splat(1.0 / 3.0)) * factor,
                "RGB_ONE" => c.truncate().dot(Vec3::splat(1.0 / 3.0)) * factor,
                "A_ZERO" => 1.0 - c.w * factor,
                _ => c.w * factor,
            },
            Some(p @ Param::Texture { .. }) => {
                if let (Some(mask), Some(base)) = (self.tex_ref(effect, &p, uv1_name), out.base_color_texture.as_mut())
                    && let Some(merged) = self.textures.merge_alpha(base.texture, mask.texture)
                {
                    base.texture = merged;
                    out.alpha_mode = AlphaMode::Mask(0.5);
                    out.double_sided = true;
                }
                1.0
            }
            _ => match get("transparency") {
                Some(Param::Float(f)) if transparent.is_none() => f,
                _ => 1.0,
            },
        };
        let alpha = if alpha.is_finite() && alpha >= 0.01 { alpha.clamp(0.0, 1.0) } else { 1.0 };
        out.base_color.w = alpha;
        if alpha < 0.999 && out.alpha_mode == AlphaMode::Opaque {
            out.alpha_mode = AlphaMode::Blend;
        }

        // 5. Bump maps and double-sidedness live in vendor <extra>s.
        let bump = effect.descendants().find(|n| n.tag_name().name() == "bump").and_then(|b| param(Some(b)));
        if let Some(p) = bump
            && let Some(r) = self.tex_ref(effect, &p, uv1_name)
        {
            let bump_type =
                effect.descendants().find(|n| n.tag_name().name() == "bump").and_then(|b| b.attribute("bumptype"));
            let normal = bump_type.is_some_and(|t| t.eq_ignore_ascii_case("NORMALMAP"))
                || self.textures.get(r.texture).is_some_and(crate::common::looks_like_normal_map);
            if normal {
                out.normal_texture = Some(r);
            }
        }
        let double = effect
            .descendants()
            .chain(material.descendants())
            .any(|n| n.tag_name().name() == "double_sided" && n.text().map(str::trim) == Some("1"));
        out.double_sided |= double;
        out
    }
}

//endregion

/*┌─────────────────╮
| Scene           |
└─────────────────╯*/
//region Scene

#[derive(Clone, Copy, PartialEq, Eq)]
enum XformKind {
    Matrix,
    Translate,
    Rotate,
    Scale,
    Lookat,
}

#[derive(Clone)]
struct Xform {
    kind: XformKind,
    sid: String,
    values: Vec<f32>,
}

fn compose(xforms: &[Xform]) -> Mat4 {
    let mut m = Mat4::IDENTITY;
    for x in xforms {
        let v = &x.values;
        let at = |i: usize| v.get(i).copied().unwrap_or(0.0);
        let step = match x.kind {
            XformKind::Matrix => matrix(v),
            XformKind::Translate => Mat4::from_translation(Vec3::new(at(0), at(1), at(2))),
            XformKind::Scale => Mat4::from_scale(Vec3::new(at(0), at(1), at(2))),
            XformKind::Rotate => {
                let axis = Vec3::new(at(0), at(1), at(2));
                match axis.try_normalize() {
                    Some(a) => Mat4::from_axis_angle(a, at(3).to_radians()),
                    None => Mat4::IDENTITY,
                }
            }
            XformKind::Lookat => {
                let (eye, target, up) =
                    (Vec3::new(at(0), at(1), at(2)), Vec3::new(at(3), at(4), at(5)), Vec3::new(at(6), at(7), at(8)));
                // The node looks from eye to target: -Z forward, +Y up.
                let forward = (target - eye).normalize_or_zero();
                let right = forward.cross(up).normalize_or_zero();
                if forward == Vec3::ZERO || right == Vec3::ZERO {
                    Mat4::IDENTITY
                } else {
                    let up = right.cross(forward);
                    Mat4::from_cols(right.extend(0.0), up.extend(0.0), (-forward).extend(0.0), eye.extend(1.0))
                }
            }
        };
        if step.is_finite() {
            m *= step;
        }
    }
    m
}

/// Where a COLLADA node landed in the scene, for animation targets and
/// skin joints.
struct Placed {
    index: usize,
    id: String,
    sid: String,
    name: String,
    xforms: Vec<Xform>,
}

/// An `instance_controller` waiting for the whole node tree: its node
/// index, the controller, its skeleton root ids and material bindings.
type PendingController<'a, 'i> = (usize, X<'a, 'i>, Vec<String>, Vec<(String, String)>);

/// A morph controller's primitives, target names and default weights.
type Morphed = (Vec<GeomPrim>, Vec<String>, Vec<f32>);

struct Builder<'r, 'a, 'i, 't> {
    reader: Reader<'a, 'i>,
    textures: TextureSet<'t>,
    scene: Scene,
    placed: Vec<Placed>,
    materials: HashMap<(String, Option<String>), usize>,
    meshes: HashMap<String, usize>,
    /// Controllers met while walking nodes, finished once every joint
    /// node exists.
    pending_skins: Vec<PendingController<'a, 'i>>,
    _marker: std::marker::PhantomData<&'r ()>,
}

impl<'a, 'i> Builder<'_, 'a, 'i, '_> {
    /// Adds a node and its subtree.  `path` holds the ids being expanded,
    /// so a node that instances its own ancestor stops there.
    fn node(&mut self, node: X<'a, 'i>, depth: usize, path: &mut Vec<&'a str>) -> Result<Option<usize>, LoadError> {
        if depth > MAX_DEPTH || self.scene.nodes.len() >= MAX_NODES {
            return Ok(None);
        }
        let id = node.attribute("id").unwrap_or_default();
        if !id.is_empty() && path.contains(&id) {
            return Ok(None);
        }
        path.push(id);
        let result = self.node_inner(node, depth, path);
        path.pop();
        result
    }

    fn node_inner(
        &mut self,
        node: X<'a, 'i>,
        depth: usize,
        path: &mut Vec<&'a str>,
    ) -> Result<Option<usize>, LoadError> {
        let name = node.attribute("name").or(node.attribute("id")).unwrap_or_default();
        let mut xforms = Vec::new();
        for c in node.children().filter(|c| c.is_element()) {
            let kind = match c.tag_name().name() {
                "matrix" => XformKind::Matrix,
                "translate" => XformKind::Translate,
                "rotate" => XformKind::Rotate,
                "scale" => XformKind::Scale,
                "lookat" => XformKind::Lookat,
                _ => continue,
            };
            xforms.push(Xform {
                kind,
                sid: c.attribute("sid").unwrap_or_default().to_string(),
                values: floats(Some(c)),
            });
        }
        let mut n = Node::new(name);
        n.transform = Transform::from_matrix(compose(&xforms));
        let index = self.scene.nodes.len();
        self.scene.nodes.push(n);
        self.placed.push(Placed {
            index,
            id: node.attribute("id").unwrap_or_default().to_string(),
            sid: node.attribute("sid").unwrap_or_default().to_string(),
            name: name.to_string(),
            xforms,
        });
        for c in node.children().filter(|c| c.is_element()) {
            match c.tag_name().name() {
                "node" => {
                    if let Some(ci) = self.node(c, depth + 1, path)? {
                        self.scene.nodes[index].children.push(ci);
                    }
                }
                "instance_node" => {
                    let target = c.attribute("url").and_then(|u| self.reader.ids.get(url(u)).copied());
                    if let Some(t) = target.filter(|t| t.tag_name().name() == "node")
                        && let Some(ci) = self.node(t, depth + 1, path)?
                    {
                        self.scene.nodes[index].children.push(ci);
                    }
                }
                "instance_geometry" => {
                    let Some(geom) = c.attribute("url").map(url) else { continue };
                    let bindings = bindings(c);
                    if let Some(mesh) = self.geometry_mesh(geom, &bindings)? {
                        self.attach(index, mesh, None);
                    }
                }
                "instance_controller" => {
                    let Some(ctrl) = c.attribute("url").and_then(|u| self.reader.ids.get(url(u)).copied()) else {
                        continue;
                    };
                    let skeletons =
                        children(c, "skeleton").filter_map(|s| s.text()).map(|t| url(t.trim()).to_string()).collect();
                    self.pending_skins.push((index, ctrl, skeletons, bindings(c)));
                }
                _ => {}
            }
        }
        Ok(Some(index))
    }

    /// Puts a mesh on a node, or on a new child when it already has one.
    fn attach(&mut self, node: usize, mesh: usize, skin: Option<usize>) -> usize {
        let target = if self.scene.nodes[node].mesh.is_some() {
            let mut child = Node::new(format!("{} mesh", self.scene.nodes[node].name));
            child.mesh = Some(mesh);
            self.scene.nodes.push(child);
            let ci = self.scene.nodes.len() - 1;
            self.scene.nodes[node].children.push(ci);
            ci
        } else {
            self.scene.nodes[node].mesh = Some(mesh);
            node
        };
        self.scene.nodes[target].skin = skin;
        target
    }

    fn material_index(&mut self, target: &str, uv1_name: Option<String>) -> Option<usize> {
        let key = (target.to_string(), uv1_name.clone());
        if let Some(&m) = self.materials.get(&key) {
            return Some(m);
        }
        let node = self.reader.ids.get(target).copied().filter(|n| n.tag_name().name() == "material")?;
        let material =
            MaterialBuilder { reader: &self.reader, textures: &mut self.textures }.material(node, uv1_name.as_deref());
        self.scene.materials.push(material);
        let index = self.scene.materials.len() - 1;
        self.materials.insert(key, index);
        Some(index)
    }

    /// Binds materials to a geometry's primitives.
    fn bind(&mut self, prims: &[GeomPrim], bindings: &[(String, String)]) -> Vec<Primitive> {
        prims
            .iter()
            .map(|gp| {
                let mut prim = gp.prim.clone();
                let target = bindings
                    .iter()
                    .find(|(s, _)| *s == gp.symbol)
                    .map(|(_, t)| t.clone())
                    .unwrap_or_else(|| gp.symbol.clone());
                prim.material = self.material_index(&target, None);
                prim
            })
            .collect()
    }

    fn geometry_mesh(&mut self, geom: &str, bindings: &[(String, String)]) -> Result<Option<usize>, LoadError> {
        let key = format!("{geom}|{bindings:?}");
        if let Some(&m) = self.meshes.get(&key) {
            return Ok(Some(m));
        }
        let prims = self.reader.geometry(geom)?;
        if prims.is_empty() {
            return Ok(None);
        }
        let primitives = self.bind(&prims, bindings);
        let name = self.reader.ids.get(geom).and_then(|g| g.attribute("name")).unwrap_or(geom).to_string();
        self.scene.meshes.push(Mesh { name, primitives, target_names: Vec::new() });
        let index = self.scene.meshes.len() - 1;
        self.meshes.insert(key, index);
        Ok(Some(index))
    }

    /// Builds a morph controller's mesh: the base geometry plus one target
    /// per morph, as position offsets per vertex.
    fn morph_prims(&mut self, morph: X<'a, 'i>) -> Result<Morphed, LoadError> {
        let base_id = morph.attribute("source").map(url).unwrap_or_default().to_string();
        let mut prims: Vec<GeomPrim> = self.reader.geometry(&base_id)?.as_ref().clone();
        let relative = morph.attribute("method") == Some("RELATIVE");
        let targets_el = child(morph, "targets");
        let input = |semantic: &str| {
            targets_el
                .and_then(|t| children(t, "input").find(|i| i.attribute("semantic") == Some(semantic)))
                .and_then(|i| i.attribute("source"))
        };
        let (Some(target_src), weight_src) = (input("MORPH_TARGET"), input("MORPH_WEIGHT")) else {
            return Ok((prims, Vec::new(), Vec::new()));
        };
        let targets = self.reader.source(target_src);
        let weights = weight_src.map(|w| self.reader.source(w));
        let base_positions = self.positions_of(&base_id);
        let mut names = Vec::new();
        let mut defaults = Vec::new();
        for (ti, target) in targets.names.iter().enumerate() {
            let positions = self.positions_of(url(target));
            names
                .push(self.reader.ids.get(url(target)).and_then(|g| g.attribute("name")).unwrap_or(target).to_string());
            defaults.push(weights.as_ref().map_or(0.0, |w| w.get(ti, 0)));
            for gp in &mut prims {
                let deltas = gp
                    .source
                    .iter()
                    .map(|&pi| {
                        let t = positions.as_ref().map_or(Vec3::ZERO, |p| p.vec3(pi as usize));
                        let b = base_positions.as_ref().map_or(Vec3::ZERO, |p| p.vec3(pi as usize));
                        if relative { t } else { t - b }
                    })
                    .collect();
                gp.prim.targets.push(MorphTarget { positions: deltas, normals: Vec::new() });
            }
        }
        Ok((prims, names, defaults))
    }

    /// The POSITION source of a geometry.
    fn positions_of(&mut self, geom: &str) -> Option<Rc<Source>> {
        let mesh = self.reader.ids.get(geom).and_then(|g| child(*g, "mesh"))?;
        let vertices = child(mesh, "vertices")?;
        let input = children(vertices, "input").find(|i| i.attribute("semantic") == Some("POSITION"))?;
        Some(self.reader.source(input.attribute("source")?))
    }

    /// Finds a joint node by the name a skin uses: a sid under the
    /// skeleton roots, then any node's sid, id or name.
    fn find_joint(&self, name: &str, roots: &[usize]) -> Option<usize> {
        let under_root = |p: &&Placed| roots.is_empty() || roots.iter().any(|&r| self.is_descendant(p.index, r));
        self.placed
            .iter()
            .filter(under_root)
            .find(|p| p.sid == name)
            .or_else(|| self.placed.iter().find(|p| p.sid == name))
            .or_else(|| self.placed.iter().find(|p| p.id == name))
            .or_else(|| self.placed.iter().find(|p| p.name == name))
            .map(|p| p.index)
    }

    fn is_descendant(&self, node: usize, root: usize) -> bool {
        let mut stack = vec![root];
        let mut steps = 0;
        while let Some(n) = stack.pop() {
            steps += 1;
            if n == node {
                return true;
            }
            if steps > self.scene.nodes.len() {
                break;
            }
            stack.extend(&self.scene.nodes[n].children);
        }
        false
    }

    /// Finishes an `instance_controller`: a skin, a morph, or a skin of a
    /// morph.
    fn controller(
        &mut self,
        node: usize,
        ctrl: X<'a, 'i>,
        skeletons: &[String],
        bindings: &[(String, String)],
    ) -> Result<(), LoadError> {
        let skin_el = child(ctrl, "skin");
        let morph_el = child(ctrl, "morph").or_else(|| {
            let source = skin_el?.attribute("source").map(url)?;
            self.reader.ids.get(source).and_then(|c| child(*c, "morph"))
        });
        // 1. Geometry, through a morph when there is one.
        let (mut prims, target_names, defaults) = match morph_el {
            Some(m) => self.morph_prims(m)?,
            None => {
                let geom = skin_el.and_then(|s| s.attribute("source")).map(url).unwrap_or_default();
                (self.reader.geometry(geom)?.as_ref().clone(), Vec::new(), Vec::new())
            }
        };
        if prims.is_empty() {
            return Ok(());
        }
        // 2. Skin: bake the bind shape matrix into the vertices and add
        //    joint weights per vertex.
        let mut skin = None;
        if let Some(s) = skin_el {
            let bind_shape = matrix(&floats(child(s, "bind_shape_matrix")));
            let normal_matrix = Mat3::from_mat4(bind_shape).inverse().transpose();
            let joints_el = child(s, "joints");
            let joint_input = |semantic: &str| {
                joints_el
                    .and_then(|j| children(j, "input").find(|i| i.attribute("semantic") == Some(semantic)))
                    .and_then(|i| i.attribute("source"))
            };
            let names = joint_input("JOINT").map(|j| self.reader.source(j)).unwrap_or_default();
            let inv_bind = joint_input("INV_BIND_MATRIX").map(|j| self.reader.source(j)).unwrap_or_default();
            let roots: Vec<usize> =
                skeletons.iter().filter_map(|s| self.placed.iter().find(|p| p.id == *s).map(|p| p.index)).collect();
            let mut joints = Vec::with_capacity(names.names.len());
            let mut missing = 0;
            for name in &names.names {
                joints.push(self.find_joint(name, &roots).unwrap_or_else(|| {
                    missing += 1;
                    node
                }));
            }
            if missing > 0 {
                self.reader.warnings.push(format!("{missing} skin joints not found"));
            }
            let per_vertex = vertex_weights(&mut self.reader, s);
            for gp in &mut prims {
                for p in &mut gp.prim.positions {
                    *p = bind_shape.transform_point3(*p);
                }
                for n in &mut gp.prim.normals {
                    *n = (normal_matrix * *n).normalize_or_zero();
                }
                for &pi in &gp.source {
                    let (j, w) = per_vertex.get(pi as usize).copied().unwrap_or(([0; 4], Vec4::ZERO));
                    gp.prim.joints.push(j);
                    gp.prim.weights.push(w);
                }
            }
            let inverse_bind =
                (0..joints.len()).map(|i| if i < inv_bind.count { inv_bind.mat4(i) } else { Mat4::IDENTITY }).collect();
            self.scene.skins.push(Skin {
                name: ctrl.attribute("name").or(ctrl.attribute("id")).unwrap_or_default().to_string(),
                joints,
                inverse_bind,
                skeleton: roots.first().copied(),
            });
            skin = Some(self.scene.skins.len() - 1);
        }
        let primitives = self.bind(&prims, bindings);
        let name = ctrl.attribute("name").or(ctrl.attribute("id")).unwrap_or_default().to_string();
        self.scene.meshes.push(Mesh { name, primitives, target_names });
        let mesh = self.scene.meshes.len() - 1;
        let target = self.attach(node, mesh, skin);
        self.scene.nodes[target].weights = defaults;
        Ok(())
    }
}

/// `instance_material` bindings: (symbol, material id).
fn bindings(instance: X) -> Vec<(String, String)> {
    instance
        .descendants()
        .filter(|n| n.tag_name().name() == "instance_material")
        .filter_map(|m| Some((m.attribute("symbol")?.to_string(), url(m.attribute("target")?).to_string())))
        .collect()
}

/// Up to four strongest joint weights for every position, normalised.
fn vertex_weights(reader: &mut Reader, skin: X) -> Vec<([u16; 4], Vec4)> {
    let Some(vw) = child(skin, "vertex_weights") else { return Vec::new() };
    let mut joint_offset = 0;
    let mut weight_offset = 1;
    let mut weights = Rc::new(Source::default());
    let mut stride = 1;
    for i in children(vw, "input") {
        let offset = i.attribute("offset").and_then(|v| v.parse().ok()).unwrap_or(0usize).min(64);
        stride = stride.max(offset + 1);
        match i.attribute("semantic") {
            Some("JOINT") => joint_offset = offset,
            Some("WEIGHT") => {
                weight_offset = offset;
                weights = reader.source(i.attribute("source").unwrap_or_default());
            }
            _ => {}
        }
    }
    let counts = ints(child(vw, "vcount"));
    let v = ints(child(vw, "v"));
    let mut out = Vec::with_capacity(counts.len());
    let mut at = 0usize;
    for &count in &counts {
        let count = count.clamp(0, 1 << 16) as usize;
        let mut best: [(u16, f32); 4] = [(0, 0.0); 4];
        for k in 0..count {
            let base = (at + k) * stride;
            let (Some(&j), Some(&w)) = (v.get(base + joint_offset), v.get(base + weight_offset)) else { break };
            let (Ok(j), weight) = (u16::try_from(j), weights.get(w.max(0) as usize, 0)) else { continue };
            if weight > best[3].1 {
                best[3] = (j, weight);
                best.sort_by(|a, b| b.1.total_cmp(&a.1));
            }
        }
        at += count;
        let total: f32 = best.iter().map(|b| b.1).sum();
        out.push(if total > 0.0 {
            (best.map(|b| b.0), Vec4::from_array(best.map(|b| b.1 / total)))
        } else {
            ([0; 4], Vec4::ZERO)
        });
    }
    out
}

//endregion

/*┌─────────────────╮
| Animation       |
└─────────────────╯*/
//region Animation

/// One animated value: keys for some or all components of a transform
/// element.
struct Track {
    node_id: String,
    sid: String,
    /// The first component written, and how many.
    component: usize,
    width: usize,
    times: Vec<f32>,
    values: Vec<f32>,
    step: bool,
    /// The `<animation>` it came from, for splitting into clips.
    animation: String,
}

fn member_component(member: &str) -> Option<usize> {
    Some(match member.to_ascii_uppercase().as_str() {
        "X" | "S" | "R" | "U" => 0,
        "Y" | "T" | "G" | "V" => 1,
        "Z" | "P" | "B" => 2,
        "ANGLE" | "W" | "A" => 3,
        _ => return None,
    })
}

fn collect_tracks(reader: &mut Reader, animation: X, root_id: &str, out: &mut Vec<Track>, depth: usize) {
    if depth > MAX_DEPTH {
        return;
    }
    let id = animation.attribute("id").unwrap_or(root_id).to_string();
    let root = if root_id.is_empty() { id.clone() } else { root_id.to_string() };
    for c in animation.children().filter(|c| c.is_element()) {
        match c.tag_name().name() {
            "animation" => collect_tracks(reader, c, &root, out, depth + 1),
            "channel" => {
                let (Some(sampler), Some(target)) = (c.attribute("source"), c.attribute("target")) else { continue };
                let Some(sampler) = reader.ids.get(url(sampler)).copied() else { continue };
                let input = |semantic: &str| {
                    children(sampler, "input")
                        .find(|i| i.attribute("semantic") == Some(semantic))
                        .and_then(|i| i.attribute("source"))
                };
                let (Some(inp), Some(outp)) = (input("INPUT"), input("OUTPUT")) else { continue };
                let times = reader.source(inp);
                let values = reader.source(outp);
                let step = input("INTERPOLATION")
                    .map(|s| reader.source(s))
                    .is_some_and(|s| !s.names.is_empty() && s.names.iter().all(|n| n == "STEP"));
                // "node/sid", "node/sid.ANGLE", "node/sid(3)" or "node/sid(0)(3)".
                let Some((node_id, rest)) = target.split_once('/') else { continue };
                let (sid, member) = match rest.find(['.', '(']) {
                    Some(i) => (&rest[..i], &rest[i..]),
                    None => (rest, ""),
                };
                let (component, width) = if let Some(m) = member.strip_prefix('.') {
                    match member_component(m) {
                        Some(c) => (c, 1),
                        None => continue,
                    }
                } else if member.starts_with('(') {
                    let idx: Vec<usize> = member.split(['(', ')']).filter_map(|s| s.parse().ok()).collect();
                    match idx.as_slice() {
                        [i] => (*i, 1),
                        // COLLADA addresses matrices as (column)(row).
                        [col, row] => (row * 4 + col, 1),
                        _ => continue,
                    }
                } else {
                    (0, values.stride)
                };
                let n = times.count.min(values.count);
                out.push(Track {
                    node_id: node_id.to_string(),
                    sid: sid.to_string(),
                    component,
                    width,
                    times: (0..n).map(|i| times.get(i, 0)).collect(),
                    values: (0..n)
                        .flat_map(|i| (0..width).map(move |k| (i, k)))
                        .map(|(i, k)| values.get(i, k))
                        .collect(),
                    step,
                    animation: root.clone(),
                });
            }
            _ => {}
        }
    }
}

fn sample_track(t: &Track, time: f32, out: &mut [f32]) {
    let n = t.times.len();
    if n == 0 {
        return;
    }
    let i = t.times.partition_point(|&k| k <= time);
    let w = t.width;
    let value = |k: usize, c: usize| t.values.get(k * w + c).copied().unwrap_or(0.0);
    for c in 0..w {
        let v = if i == 0 {
            value(0, c)
        } else if i >= n || t.step {
            value(i - 1, c)
        } else {
            let (a, b) = (t.times[i - 1], t.times[i]);
            let f = if b > a { (time - a) / (b - a) } else { 0.0 };
            value(i - 1, c) + (value(i, c) - value(i - 1, c)) * f
        };
        if let Some(slot) = out.get_mut(t.component + c) {
            *slot = v;
        }
    }
}

/// Bakes the tracks into one clip: per animated node, its transform
/// elements are evaluated at every key time and decomposed.
fn bake(placed: &[Placed], tracks: &[&Track], name: &str, window: Option<(f32, f32)>) -> Option<Animation> {
    let mut anim = Animation { name: name.to_string(), duration: 0.0, channels: Vec::new() };
    for p in placed {
        let mine: Vec<&&Track> =
            tracks.iter().filter(|t| t.node_id == p.id && p.xforms.iter().any(|x| x.sid == t.sid)).collect();
        if mine.is_empty() {
            continue;
        }
        let mut times: Vec<f32> = mine.iter().flat_map(|t| t.times.iter().copied()).filter(|t| t.is_finite()).collect();
        times.sort_by(f32::total_cmp);
        times.dedup_by(|a, b| (*a - *b).abs() < 1e-6);
        if let Some((start, end)) = window {
            times.retain(|&t| t >= start - 1e-6 && t <= end + 1e-6);
            if times.first().is_none_or(|&t| t > start) {
                times.insert(0, start);
            }
        }
        times.truncate(MAX_KEYS);
        let (mut tv, mut rv, mut sv) = (Vec::new(), Vec::new(), Vec::new());
        let mut previous = Quat::IDENTITY;
        for &time in &times {
            let mut xforms = p.xforms.clone();
            for t in &mine {
                if let Some(x) = xforms.iter_mut().find(|x| x.sid == t.sid) {
                    let needed = (t.component + t.width).max(x.values.len());
                    x.values.resize(needed.min(16), 0.0);
                    sample_track(t, time, &mut x.values);
                }
            }
            let (s, mut r, tr) = compose(&xforms).to_scale_rotation_translation();
            if r.dot(previous) < 0.0 {
                r = -r;
            }
            previous = r;
            tv.extend(tr.to_array());
            rv.extend(r.to_array());
            sv.extend(s.to_array());
        }
        let shift = window.map_or(0.0, |w| w.0);
        let times: Vec<f32> = times.iter().map(|t| t - shift).collect();
        for (property, values) in [(Property::Translation, tv), (Property::Rotation, rv), (Property::Scale, sv)] {
            anim.channels.push(Channel {
                node: p.index,
                property,
                interpolation: Interpolation::Linear,
                times: times.clone(),
                values,
            });
        }
    }
    anim.duration = anim.channels.iter().filter_map(|c| c.times.last()).fold(0.0f32, |a, &b| a.max(b));
    (!anim.channels.is_empty()).then_some(anim)
}

//endregion

pub fn load(path: &Path, bytes: &[u8], limits: &Limits) -> Result<Scene, LoadError> {
    // 1. Parse the XML.  COLLADA is UTF-8 in practice; anything else is
    //    read lossily rather than refused.
    let bytes = crate::common::utf8_text(bytes);
    let text = String::from_utf8_lossy(&bytes);
    let options = roxmltree::ParsingOptions { allow_dtd: true, ..Default::default() };
    let doc = roxmltree::Document::parse_with_options(&text, options)
        .map_err(|e| LoadError::invalid(format!("COLLADA XML: {e}")))?;
    let root = doc.root_element();
    if root.tag_name().name() != "COLLADA" {
        return Err(LoadError::NotMine);
    }
    let mut ids = HashMap::new();
    for n in doc.descendants().filter(|n| n.is_element()) {
        if let Some(id) = n.attribute("id") {
            ids.entry(id).or_insert(n);
        }
    }
    let reader =
        Reader { ids, sources: HashMap::new(), geometries: HashMap::new(), limits, triangles: 0, warnings: Vec::new() };

    // 2. Refuse oversized files from the stated primitive counts.
    let stated: u64 = doc
        .descendants()
        .filter(|n| matches!(n.tag_name().name(), "triangles" | "polylist" | "polygons" | "trifans" | "tristrips"))
        .filter_map(|n| n.attribute("count").and_then(|c| c.parse::<u64>().ok()))
        .sum();
    check_triangles(stated, limits)?;

    let mut b = Builder {
        reader,
        textures: TextureSet::new(path, limits),
        scene: Scene::default(),
        placed: Vec::new(),
        materials: HashMap::new(),
        meshes: HashMap::new(),
        pending_skins: Vec::new(),
        _marker: std::marker::PhantomData,
    };
    let asset = child(root, "asset");
    b.scene.info.format = format!("COLLADA {}", root.attribute("version").unwrap_or(""));
    b.scene.info.generator = asset
        .and_then(|a| a.descendants().find(|n| n.tag_name().name() == "authoring_tool"))
        .and_then(|n| n.text())
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty());

    // 3. The visual scene the file instances, or its first one.
    let scene_url =
        child(root, "scene").and_then(|s| child(s, "instance_visual_scene")).and_then(|i| i.attribute("url")).map(url);
    let visual = scene_url
        .and_then(|u| b.reader.ids.get(u).copied())
        .or_else(|| child(root, "library_visual_scenes").and_then(|l| child(l, "visual_scene")));
    let Some(visual) = visual else { return Err(LoadError::invalid("COLLADA has no visual scene")) };
    for n in children(visual, "node") {
        b.node(n, 0, &mut Vec::new())?;
    }
    for (node, ctrl, skeletons, binds) in std::mem::take(&mut b.pending_skins) {
        b.controller(node, ctrl, &skeletons, &binds)?;
    }
    b.scene.compute_roots();
    if b.scene.meshes.is_empty() {
        return Err(LoadError::invalid("COLLADA scene holds no meshes"));
    }

    // 4. Animations, split by clip when the file has clips.
    if limits.animations {
        let mut tracks = Vec::new();
        for lib in children(root, "library_animations") {
            for a in children(lib, "animation") {
                collect_tracks(&mut b.reader, a, "", &mut tracks, 0);
            }
        }
        let clips: Vec<X> =
            children(root, "library_animation_clips").flat_map(|l| children(l, "animation_clip")).collect();
        if clips.is_empty() {
            let all: Vec<&Track> = tracks.iter().collect();
            b.scene.animations.extend(bake(&b.placed, &all, "animation", None));
        }
        for clip in clips {
            let wanted: Vec<&str> =
                children(clip, "instance_animation").filter_map(|i| i.attribute("url")).map(url).collect();
            let mine: Vec<&Track> = tracks.iter().filter(|t| wanted.contains(&t.animation.as_str())).collect();
            let f = |name: &str| clip.attribute(name).and_then(|v| v.parse::<f32>().ok());
            let window = match (f("start"), f("end")) {
                (Some(s), Some(e)) if e > s => Some((s, e)),
                _ => None,
            };
            let name = clip.attribute("name").or(clip.attribute("id")).unwrap_or("clip");
            b.scene.animations.extend(bake(&b.placed, &mine, name, window));
        }
    }

    // 5. Up axis and unit.
    let up = asset.and_then(|a| child(a, "up_axis")).and_then(|u| u.text()).map(str::trim).unwrap_or("Y_UP");
    let unit = asset
        .and_then(|a| child(a, "unit"))
        .and_then(|u| u.attribute("meter"))
        .and_then(|m| number(m.trim().as_bytes()))
        .filter(|m| m.is_finite() && *m > 0.0)
        .unwrap_or(1.0);
    let rotation = match up {
        "Z_UP" => Quat::from_rotation_x(-std::f32::consts::FRAC_PI_2),
        "X_UP" => Quat::from_rotation_z(std::f32::consts::FRAC_PI_2),
        _ => Quat::IDENTITY,
    };
    if rotation != Quat::IDENTITY || unit != 1.0 {
        b.scene.wrap_roots(Transform { rotation, scale: Vec3::splat(unit), ..Transform::IDENTITY });
    }
    b.scene.info.details.push(("Up axis".into(), up.trim_end_matches("_UP").into()));
    b.scene.info.details.push(("Unit".into(), format!("{unit} m")));
    let mut scene = b.scene;
    scene.info.warnings.extend(b.reader.warnings);
    scene.info.warnings.extend(b.textures.warnings);
    scene.textures = b.textures.textures;
    for mesh in &mut scene.meshes {
        for p in &mut mesh.primitives {
            p.ensure_normals();
        }
    }
    Ok(scene)
}

#[cfg(test)]
mod tests {
    use super::*;

    const QUAD: &str = r##"<?xml version="1.0" encoding="utf-8"?>
<COLLADA xmlns="http://www.collada.org/2005/11/COLLADASchema" version="1.4.1">
  <asset><contributor><authoring_tool>Hand</authoring_tool></contributor><unit meter="0.01"/><up_axis>Z_UP</up_axis></asset>
  <library_effects>
    <effect id="red-fx"><profile_COMMON><technique sid="common"><phong>
      <diffuse><color>1 0 0 1</color></diffuse><shininess><float>50</float></shininess>
    </phong></technique></profile_COMMON></effect>
  </library_effects>
  <library_materials><material id="red" name="Red"><instance_effect url="#red-fx"/></material></library_materials>
  <library_geometries>
    <geometry id="quad" name="Quad"><mesh>
      <source id="pos"><float_array id="pos-a" count="12">0 0 0 1 0 0 1 1 0 0 1 0</float_array>
        <technique_common><accessor source="#pos-a" count="4" stride="3"><param name="X" type="float"/><param name="Y" type="float"/><param name="Z" type="float"/></accessor></technique_common></source>
      <source id="uv"><float_array id="uv-a" count="8">0 0 1 0 1 1 0 1</float_array>
        <technique_common><accessor source="#uv-a" count="4" stride="2"><param name="S" type="float"/><param name="T" type="float"/></accessor></technique_common></source>
      <vertices id="verts"><input semantic="POSITION" source="#pos"/></vertices>
      <polylist count="1" material="mat">
        <input semantic="VERTEX" source="#verts" offset="0"/><input semantic="TEXCOORD" source="#uv" offset="1" set="0"/>
        <vcount>4</vcount><p>0 0 1 1 2 2 3 3</p>
      </polylist>
    </mesh></geometry>
  </library_geometries>
  <library_animations>
    <animation id="spin">
      <source id="t"><float_array id="t-a" count="2">0 1</float_array><technique_common><accessor source="#t-a" count="2"><param name="TIME" type="float"/></accessor></technique_common></source>
      <source id="a"><float_array id="a-a" count="2">0 90</float_array><technique_common><accessor source="#a-a" count="2"><param name="ANGLE" type="float"/></accessor></technique_common></source>
      <sampler id="s"><input semantic="INPUT" source="#t"/><input semantic="OUTPUT" source="#a"/></sampler>
      <channel source="#s" target="n1/rotZ.ANGLE"/>
    </animation>
  </library_animations>
  <library_visual_scenes><visual_scene id="vs">
    <node id="n1" name="Spinner"><translate sid="loc">0 0 5</translate><rotate sid="rotZ">0 0 1 0</rotate>
      <instance_geometry url="#quad"><bind_material><technique_common><instance_material symbol="mat" target="#red"/></technique_common></bind_material></instance_geometry>
    </node>
  </visual_scene></library_visual_scenes>
  <scene><instance_visual_scene url="#vs"/></scene>
</COLLADA>"##;

    #[test]
    fn loads_quad_with_material_units_and_animation() {
        let scene = load(Path::new("/nonexistent/q.dae"), QUAD.as_bytes(), &Limits::viewer()).unwrap();
        assert_eq!(scene.info.generator.as_deref(), Some("Hand"));
        assert_eq!(scene.triangle_count(), 2);
        let m = &scene.materials[0];
        assert_eq!(m.name, "Red");
        assert_eq!(m.base_color, Vec4::new(1.0, 0.0, 0.0, 1.0));
        let p = &scene.meshes[0].primitives[0];
        assert_eq!(p.uv0[0], Vec2::new(0.0, 1.0));
        assert_eq!(p.normals[0], Vec3::Z);
        // Z-up centimetres: 5 units up becomes 5 cm along +Y.
        let pose = slerp_scene::Pose::rest(&scene);
        let b = scene.bounds(&pose);
        assert!((b.min.y - 0.05).abs() < 1e-5, "{b:?}");
        // The rotation track bakes to a quarter turn about Z at t = 1.
        let anim = &scene.animations[0];
        assert_eq!(anim.duration, 1.0);
        let rot = anim.channels.iter().find(|c| c.property == Property::Rotation).unwrap();
        let q = Quat::from_slice(&rot.values[4..8]);
        assert!(q.dot(Quat::from_rotation_z(std::f32::consts::FRAC_PI_2)).abs() > 0.999_99, "{q}");
    }

    #[test]
    fn survives_truncation_and_garbage() {
        let bytes = QUAD.as_bytes();
        for cut in (0..bytes.len()).step_by(13) {
            let _ = load(Path::new("q.dae"), &bytes[..cut], &Limits::viewer());
        }
        let broken = QUAD
            .replace("0 0 1 1 2 2 3 3", "0 0 1 99 -5 2 3")
            .replace("count=\"4\" stride=\"3\"", "count=\"999\" stride=\"0\"");
        let _ = load(Path::new("q.dae"), broken.as_bytes(), &Limits::viewer());
        let looped = QUAD.replace("</node>", "<instance_node url=\"#n1\"/></node>");
        assert!(load(Path::new("q.dae"), looped.as_bytes(), &Limits::viewer()).is_ok());
        let limits = Limits { max_triangles: 0, ..Limits::viewer() };
        assert!(matches!(load(Path::new("q.dae"), bytes, &limits), Err(LoadError::TooLarge(_))));
    }
}
