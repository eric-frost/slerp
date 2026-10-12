//! Materials: Blender's shader node trees boiled down to the scene's
//! metallic-roughness material.
//!
//! The surface shader is found from the active Material Output node.  A
//! Principled BSDF gives base colour, metallic, roughness, IOR, alpha,
//! normal, emission, transmission, coat and specular; Diffuse, Glossy,
//! Glass, Emission, Transparent and Mix shaders map to the nearest such
//! material.  Each input is traced back through reroutes, node groups,
//! colour adjustments and channel separators to a constant, an Image
//! Texture (with its UV map and Mapping node) or a colour attribute.
//! Materials without nodes, and every material before Cycles, use their
//! viewport colour, and Blender Internal's image texture slots.

use crate::file::BlendFile;
use crate::image;
use crate::view::Obj;
use glam::{Vec2, Vec3, Vec4};
use slerp_scene::textures::{ColorSpace, Pixels, Texture};
use slerp_scene::{AlphaMode, Filter, Limits, Material, Sampler, TexRef, UvTransform, Wrap};
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

/*┌─────────────────╮
| Node graphs     |
└─────────────────╯*/
//region Node graphs

/// A socket: a node's input or output.
#[derive(Clone)]
pub struct Socket {
    pub name: String,
    pub identifier: String,
    /// Its own value when nothing is linked: 1 to 4 floats.
    pub value: Vec<f32>,
    pub hidden: bool,
    /// Blender's socket type: 0 float, 1 vector, 2 colour, 3 shader.
    pub kind: i64,
}

pub struct Node<'a> {
    pub obj: Obj<'a>,
    pub idname: String,
    pub name: String,
    pub inputs: Vec<Socket>,
    pub outputs: Vec<Socket>,
    pub muted: bool,
}

/// A link from a node's output to another node's input.
#[derive(Clone, Copy)]
pub struct Link {
    pub from: (usize, usize),
    pub to: (usize, usize),
}

/// A node tree with its links resolved to indices.
pub struct Graph<'a> {
    pub nodes: Vec<Node<'a>>,
    pub links: Vec<Link>,
}

impl<'a> Graph<'a> {
    pub fn read(tree: &Obj<'a>) -> Graph<'a> {
        let f = tree.f;
        let mut nodes = Vec::new();
        let mut node_of: HashMap<u64, usize> = HashMap::new();
        let mut sock_of: HashMap<u64, (usize, usize, bool)> = HashMap::new();
        for n in tree.list("nodes").into_iter().take(10_000) {
            let i = nodes.len();
            node_of.insert(f.block(n.block).old + (n.at - f.block(n.block).start) as u64, i);
            let mut sockets = |list: &str, input: bool| -> Vec<Socket> {
                n.list(list)
                    .into_iter()
                    .take(1000)
                    .enumerate()
                    .map(|(k, s)| {
                        sock_of.insert(f.block(s.block).old + (s.at - f.block(s.block).start) as u64, (i, k, input));
                        let value = socket_value(&s);
                        let flag = s.int(&["flag"]).unwrap_or(0);
                        Socket {
                            name: s.string("name"),
                            identifier: s.string("identifier"),
                            value,
                            hidden: flag & (1 << 3) != 0,
                            kind: s.int(&["type"]).unwrap_or(3),
                        }
                    })
                    .collect()
            };
            let inputs = sockets("inputs", true);
            let outputs = sockets("outputs", false);
            let flag = n.int(&["flag"]).unwrap_or(0);
            nodes.push(Node {
                obj: n,
                idname: n.string("idname"),
                name: n.string("name"),
                inputs,
                outputs,
                muted: flag & (1 << 9) != 0,
            });
        }
        let mut links = Vec::new();
        for l in tree.list("links").into_iter().take(20_000) {
            if l.int(&["flag"]).unwrap_or(0) & (1 << 4) != 0 {
                continue;
            }
            let (Some(&(fnode, fs, false)), Some(&(tnode, ts, true))) =
                (sock_of.get(&l.ptr("fromsock")), sock_of.get(&l.ptr("tosock")))
            else {
                continue;
            };
            links.push(Link { from: (fnode, fs), to: (tnode, ts) });
        }
        let _ = node_of;
        Graph { nodes, links }
    }

    /// The output linked into input `k` of node `n`.
    pub fn source(&self, n: usize, k: usize) -> Option<(usize, usize)> {
        self.links.iter().find(|l| l.to == (n, k)).map(|l| l.from)
    }

    /// Input index of node `n` called `names[..]` (by name or identifier).
    pub fn input(&self, n: usize, names: &[&str]) -> Option<usize> {
        let node = &self.nodes[n];
        names.iter().find_map(|want| {
            node.inputs
                .iter()
                .position(|s| !s.hidden && (s.identifier == *want || s.name == *want))
                .or_else(|| node.inputs.iter().position(|s| s.identifier == *want || s.name == *want))
        })
    }
}

//endregion

/// A socket's own value.  Its block is usually typed (a float, vector or
/// colour value structure), but some versions wrote node group sockets'
/// values as untyped data, read then by the socket's type: a float or
/// vector after an int subtype, a colour at the start.
fn socket_value(s: &Obj) -> Vec<f32> {
    let Some(v) = s.deref("default_value") else { return Vec::new() };
    if let Some(fl) = v.field("value") {
        let n = (fl.elems as usize).min(4);
        return (0..n).filter_map(|j| v.float_at("value", j)).collect();
    }
    let Some(raw) = s.raw("default_value") else { return Vec::new() };
    let rd = s.f.endian();
    let floats = |at: usize, n: usize| -> Vec<f32> {
        raw.get(at..at + 4 * n).map(|b| b.chunks_exact(4).map(|c| rd.f32(c)).collect()).unwrap_or_default()
    };
    match s.int(&["type"]) {
        Some(0) => floats(4, 1),
        Some(1) => floats(4, 3),
        Some(2) => floats(0, 4),
        _ => Vec::new(),
    }
}

/*┌─────────────────╮
| Values          |
└─────────────────╯*/
//region Values

/// What a shader input comes from.
#[derive(Clone, Debug)]
pub enum Value {
    Const(Vec4),
    /// An image texture: its Image data-block, which channel (0 red to 3
    /// alpha, 4 colour or luminance), and how it is mapped.
    Image {
        image: u64,
        channel: usize,
        map: Mapping,
    },
    /// A colour attribute ("" for the mesh's default one).
    Attribute(String),
}

#[derive(Clone, Debug, Default)]
pub struct Mapping {
    /// A named UV map, or the default.
    pub uv: Option<String>,
    pub transform: Option<UvTransform>,
    pub wrap: Option<Wrap>,
    pub nearest: bool,
}

/// A frame of the node-group stack: the graph, and the group node in the
/// graph above that led here.
#[derive(Clone, Copy)]
struct Frame<'g, 'a> {
    graph: &'g Graph<'a>,
    group_node: Option<usize>,
}

/// Traces node trees back from a shader's inputs.
struct Tracer<'a> {
    f: &'a BlendFile,
    /// Node groups read so far, by node tree address.
    groups: HashMap<u64, Graph<'a>>,
}

impl<'a> Tracer<'a> {
    fn group(&mut self, tree: u64) -> Option<&Graph<'a>> {
        if !self.groups.contains_key(&tree) {
            let t = self.f.resolve(tree, self.f.sdna.struct_named("bNodeTree"))?;
            let g = Graph::read(&t);
            self.groups.insert(tree, g);
        }
        self.groups.get(&tree)
    }
}

/// The value at input `k` of node `n` of the innermost graph in `stack`.
fn trace_input(t: &mut Tracer, stack: &[Frame], n: usize, k: usize, depth: usize) -> Value {
    let g = stack.last().unwrap().graph;
    let own = || {
        let v = &g.nodes[n].inputs.get(k).map(|s| s.value.clone()).unwrap_or_default();
        Value::Const(match v.len() {
            0 => Vec4::ONE,
            1 => Vec4::new(v[0], v[0], v[0], 1.0),
            2 => Vec4::new(v[0], v[1], 0.0, 1.0),
            3 => Vec4::new(v[0], v[1], v[2], 1.0),
            _ => Vec4::new(v[0], v[1], v[2], v[3]),
        })
    };
    if depth > 32 {
        return own();
    }
    match g.source(n, k) {
        Some((from, out)) => trace_output(t, stack, from, out, depth + 1).unwrap_or_else(own),
        None => own(),
    }
}

/// The value leaving output `out` of node `n`, or None when it isn't
/// something a metallic-roughness material can show.
fn trace_output(t: &mut Tracer, stack: &[Frame], n: usize, out: usize, depth: usize) -> Option<Value> {
    if depth > 32 {
        return None;
    }
    let g = stack.last().unwrap().graph;
    let node = &g.nodes[n];
    let out_name = node.outputs.get(out).map(|s| s.name.as_str()).unwrap_or("");
    // Nodes that pass their first colour or value input through, roughly:
    // muted nodes, reroutes, and colour adjustments that keep the texture's
    // pattern.
    let through = |t: &mut Tracer, names: &[&str]| g.input(n, names).map(|k| trace_input(t, stack, n, k, depth));
    let id = node.idname.as_str();
    if node.muted {
        return through(t, &["Color", "Fac", "Value", "Input", "Image"]).or(Some(Value::Const(Vec4::ONE)));
    }
    match id {
        "NodeReroute" => through(t, &["Input"]),
        "ShaderNodeTexImage" => {
            let image = node.obj.ptr("id");
            if image == 0 {
                return None;
            }
            let channel = if out_name == "Alpha" { 3 } else { 4 };
            let mut map = Mapping::default();
            if let Some(st) = node.obj.deref("storage") {
                map.wrap = match st.int(&["extension"]) {
                    Some(1) | Some(2) => Some(Wrap::ClampToEdge),
                    Some(3) => Some(Wrap::MirroredRepeat),
                    _ => None,
                };
                map.nearest = st.int(&["interpolation"]) == Some(1);
            }
            if let Some(k) = g.input(n, &["Vector"])
                && let Some((from, fo)) = g.source(n, k)
            {
                vector_mapping(stack, from, fo, &mut map, depth + 1);
            }
            Some(Value::Image { image, channel, map })
        }
        "ShaderNodeRGB" | "ShaderNodeValue" => {
            let v = node.outputs.get(out).map(|s| s.value.clone()).unwrap_or_default();
            Some(Value::Const(match v.len() {
                1 => Vec4::new(v[0], v[0], v[0], 1.0),
                n if n >= 3 => Vec4::new(v[0], v[1], v[2], v.get(3).copied().unwrap_or(1.0)),
                _ => Vec4::ONE,
            }))
        }
        "ShaderNodeVertexColor" => {
            let name = node.obj.deref("storage").map(|s| s.string("layer_name")).unwrap_or_default();
            Some(Value::Attribute(name))
        }
        "ShaderNodeAttribute" => {
            let name = node.obj.deref("storage").map(|s| s.string("name")).unwrap_or_default();
            Some(Value::Attribute(name))
        }
        "ShaderNodeSeparateRGB" | "ShaderNodeSeparateColor" | "ShaderNodeSeparateXYZ" => {
            let channel = match out_name {
                "R" | "Red" | "X" | "Hue" => 0,
                "G" | "Green" | "Y" | "Saturation" => 1,
                _ => 2,
            };
            match through(t, &["Image", "Color", "Vector"])? {
                Value::Image { image, map, .. } => Some(Value::Image { image, channel, map }),
                Value::Const(c) => Some(Value::Const(Vec4::splat(c[channel]))),
                v => Some(v),
            }
        }
        "ShaderNodeMixRGB" | "ShaderNodeMix" => {
            // Mix nodes: take a textured or attribute input if there is one
            // (the usual "texture times tint"), else the constants mixed.
            let inputs: Vec<usize> = node
                .inputs
                .iter()
                .enumerate()
                .filter(|(_, s)| !s.hidden && matches!(s.name.as_str(), "Color1" | "Color2" | "A" | "B"))
                .map(|(k, _)| k)
                .collect();
            let vals: Vec<Value> = inputs.iter().map(|&k| trace_input(t, stack, n, k, depth)).collect();
            if let Some(v) = vals.iter().find(|v| !matches!(v, Value::Const(_))) {
                return Some(v.clone());
            }
            let fac = g.input(n, &["Fac", "Factor_Float", "Factor"]).map(|k| trace_input(t, stack, n, k, depth));
            let fac = match fac {
                Some(Value::Const(c)) => c.x.clamp(0.0, 1.0),
                _ => 0.5,
            };
            match (vals.first(), vals.get(1)) {
                (Some(Value::Const(a)), Some(Value::Const(b))) => Some(Value::Const(a.lerp(*b, fac))),
                (Some(v), _) => Some(v.clone()),
                _ => None,
            }
        }
        "ShaderNodeHueSaturation"
        | "ShaderNodeGamma"
        | "ShaderNodeBrightContrast"
        | "ShaderNodeInvert"
        | "ShaderNodeRGBCurve"
        | "ShaderNodeValToRGB"
        | "ShaderNodeMapRange"
        | "ShaderNodeMath"
        | "ShaderNodeClamp"
        | "ShaderNodeRGBToBW"
        | "ShaderNodeVectorMath"
        | "ShaderNodeCombineColor"
        | "ShaderNodeCombineRGB"
        | "ShaderNodeColorRamp" => through(t, &["Color", "Fac", "Value", "Image", "Vector", "Red", "R"]),
        "ShaderNodeNormalMap" | "ShaderNodeBump" => through(t, &["Color", "Height"]),
        "NodeGroupInput" => {
            // Leave the group: the matching input of the group node above.
            let frame = stack.last().unwrap();
            let gn = frame.group_node?;
            let outer = &stack[..stack.len() - 1];
            let og = outer.last()?.graph;
            let sock = node.outputs.get(out)?;
            let k = og.nodes[gn].inputs.iter().position(|s| s.identifier == sock.identifier || s.name == sock.name)?;
            Some(trace_input(t, outer, gn, k, depth))
        }
        "ShaderNodeGroup" => {
            // Enter the group: the matching input of its Group Output node.
            // The group's graph is taken out of the cache while it is in
            // use, which also stops a group that contains itself.
            let tree = node.obj.ptr("id");
            t.group(tree)?;
            let inner = t.groups.remove(&tree)?;
            let sock = node.outputs.get(out)?.clone();
            let result = (|| {
                let on = inner.nodes.iter().position(|nd| nd.idname == "NodeGroupOutput")?;
                let k = inner.nodes[on]
                    .inputs
                    .iter()
                    .position(|s| s.identifier == sock.identifier || s.name == sock.name)?;
                let mut inner_stack = stack.to_vec();
                inner_stack.push(Frame { graph: &inner, group_node: Some(n) });
                Some(trace_input(t, &inner_stack, on, k, depth))
            })();
            t.groups.insert(tree, inner);
            result
        }
        _ => None,
    }
}

/// Follows a texture's Vector input: UV Map nodes name the UV map,
/// Mapping nodes give a transform, Texture Coordinate's UV output is the
/// default.
fn vector_mapping(stack: &[Frame], n: usize, out: usize, map: &mut Mapping, depth: usize) {
    if depth > 32 {
        return;
    }
    let g = stack.last().unwrap().graph;
    let node = &g.nodes[n];
    match node.idname.as_str() {
        "ShaderNodeUVMap" => {
            let name = node.obj.deref("storage").map(|s| s.string("uv_map")).unwrap_or_default();
            if !name.is_empty() {
                map.uv = Some(name);
            }
        }
        "ShaderNodeMapping" => {
            // Since 2.81 the location, rotation and scale are inputs; before
            // they were in the node's TexMapping.
            let get = |names: &[&str], default: Vec3| -> Vec3 {
                match g.input(n, names).map(|k| (k, g.source(n, k))) {
                    Some((k, None)) => {
                        let v = &node.inputs[k].value;
                        if v.len() >= 3 { Vec3::new(v[0], v[1], v[2]) } else { default }
                    }
                    _ => default,
                }
            };
            let (loc, rot, scale) = if node.inputs.len() > 1 {
                (get(&["Location"], Vec3::ZERO), get(&["Rotation"], Vec3::ZERO), get(&["Scale"], Vec3::ONE))
            } else if let Some(st) = node.obj.deref("storage") {
                let v = |name: &str, d: Vec3| st.floats::<3>(&[name]).map(Vec3::from).unwrap_or(d);
                (v("loc", Vec3::ZERO), v("rot", Vec3::ZERO), v("size", Vec3::ONE))
            } else {
                (Vec3::ZERO, Vec3::ZERO, Vec3::ONE)
            };
            // Blender's v runs up, the scene's down: u' = s u + t in Blender
            // becomes v' = s v + (1 - s - t) after flipping both.
            map.transform = Some(UvTransform {
                offset: Vec2::new(loc.x, 1.0 - scale.y - loc.y),
                rotation: -rot.z,
                scale: Vec2::new(scale.x, scale.y),
            });
            if let Some(k) = g.input(n, &["Vector"])
                && let Some((from, fo)) = g.source(n, k)
            {
                vector_mapping(stack, from, fo, map, depth + 1);
            }
        }
        "NodeReroute" => {
            if let Some((from, fo)) = g.source(n, 0) {
                vector_mapping(stack, from, fo, map, depth + 1);
            }
        }
        _ => {
            let _ = out;
        }
    }
}

//endregion

/*┌─────────────────╮
| Shaders         |
└─────────────────╯*/
//region Shaders

/// A material's inputs, before textures are loaded.
#[derive(Clone, Debug)]
pub struct Shading {
    pub base: Value,
    pub metallic: Value,
    pub roughness: Value,
    pub alpha: Value,
    /// A tangent-space normal map and its strength.
    pub normal: Option<(Value, f32)>,
    pub emission: Value,
    pub emission_strength: f32,
    pub ior: f32,
    pub transmission: f32,
    pub coat: f32,
    pub coat_roughness: f32,
    pub specular: f32,
    /// Drawn without lighting (a colour plugged straight into Surface).
    pub unlit: bool,
}

impl Default for Shading {
    fn default() -> Self {
        Shading {
            base: Value::Const(Vec4::new(0.8, 0.8, 0.8, 1.0)),
            metallic: Value::Const(Vec4::ZERO),
            roughness: Value::Const(Vec4::splat(0.5)),
            alpha: Value::Const(Vec4::ONE),
            normal: None,
            emission: Value::Const(Vec4::ZERO),
            emission_strength: 1.0,
            ior: 1.5,
            transmission: 0.0,
            coat: 0.0,
            coat_roughness: 0.03,
            specular: 0.5,
            unlit: false,
        }
    }
}

/// Reads the surface shading of material `ma` from its node tree, or None
/// when it has no usable node tree.
fn node_shading(t: &mut Tracer, ma: &Obj) -> Option<Shading> {
    let uses_nodes = ma.int(&["use_nodes"]).unwrap_or(1) != 0 || t.f.header.version >= 500;
    if !uses_nodes {
        return None;
    }
    let tree = ma.deref("nodetree")?;
    let graph = Graph::read(&tree);
    // 1. The active Material Output node, preferring one for all render
    //    engines or EEVEE.
    let outputs: Vec<usize> =
        (0..graph.nodes.len()).filter(|&i| graph.nodes[i].idname == "ShaderNodeOutputMaterial").collect();
    let score = |i: usize| {
        let n = &graph.nodes[i].obj;
        let active = n.int(&["flag"]).unwrap_or(0) & (1 << 6) != 0;
        let target = n.int(&["custom1"]).unwrap_or(0);
        (active as i32) * 2 + (target != 2) as i32
    };
    let out = outputs.iter().copied().max_by_key(|&i| score(i))?;
    let surface = graph.input(out, &["Surface"])?;
    let (shader, socket) = graph.source(out, surface)?;
    let stack = [Frame { graph: &graph, group_node: None }];
    let mut s = Shading::default();
    if graph.nodes[shader].outputs.get(socket).is_some_and(|o| o.kind != 3) {
        // A colour or value plugged straight into Surface shows as that
        // colour, unlit.
        s.base = trace_output(t, &stack, shader, socket, 0).unwrap_or(Value::Const(Vec4::ONE));
        s.unlit = true;
        return Some(s);
    }
    shader_into(t, &stack, shader, &mut s, 0);
    Some(s)
}

/// Fills `s` from shader node `n`.
fn shader_into(t: &mut Tracer, stack: &[Frame], n: usize, s: &mut Shading, depth: usize) {
    if depth > 16 {
        return;
    }
    let g = stack.last().unwrap().graph;
    let node = &g.nodes[n];
    let input = |t: &mut Tracer, names: &[&str]| g.input(n, names).map(|k| trace_input(t, stack, n, k, depth));
    let scalar = |t: &mut Tracer, names: &[&str], default: f32| match input(t, names) {
        Some(Value::Const(c)) => c.x,
        Some(_) => default,
        None => default,
    };
    match node.idname.as_str() {
        "ShaderNodeBsdfPrincipled" => {
            if let Some(v) = input(t, &["Base Color"]) {
                s.base = v;
            }
            if let Some(v) = input(t, &["Metallic"]) {
                s.metallic = v;
            }
            if let Some(v) = input(t, &["Roughness"]) {
                s.roughness = v;
            }
            if let Some(v) = input(t, &["Alpha"]) {
                s.alpha = v;
            }
            s.normal = normal_input(t, stack, n, depth);
            if let Some(v) = input(t, &["Emission Color", "Emission"]) {
                s.emission = v;
            }
            s.emission_strength = scalar(t, &["Emission Strength"], 1.0);
            s.ior = scalar(t, &["IOR"], 1.5);
            s.transmission = scalar(t, &["Transmission Weight", "Transmission"], 0.0);
            s.coat = scalar(t, &["Coat Weight", "Clearcoat"], 0.0);
            s.coat_roughness = scalar(t, &["Coat Roughness", "Clearcoat Roughness"], 0.03);
            s.specular = scalar(t, &["Specular IOR Level", "Specular"], 0.5);
        }
        "ShaderNodeBsdfDiffuse" => {
            if let Some(v) = input(t, &["Color"]) {
                s.base = v;
            }
            s.roughness = Value::Const(Vec4::ONE);
            s.normal = normal_input(t, stack, n, depth);
        }
        "ShaderNodeBsdfGlossy" | "ShaderNodeBsdfAnisotropic" | "ShaderNodeBsdfMetallic" => {
            if let Some(v) = input(t, &["Color", "Base Color"]) {
                s.base = v;
            }
            s.metallic = Value::Const(Vec4::ONE);
            if let Some(v) = input(t, &["Roughness"]) {
                s.roughness = v;
            }
            s.normal = normal_input(t, stack, n, depth);
        }
        "ShaderNodeBsdfGlass" | "ShaderNodeBsdfRefraction" => {
            if let Some(v) = input(t, &["Color"]) {
                s.base = v;
            }
            if let Some(v) = input(t, &["Roughness"]) {
                s.roughness = v;
            }
            s.ior = scalar(t, &["IOR"], 1.45);
            s.transmission = 1.0;
        }
        "ShaderNodeEmission" => {
            s.base = Value::Const(Vec4::new(0.0, 0.0, 0.0, 1.0));
            if let Some(v) = input(t, &["Color"]) {
                s.emission = v;
            }
            s.emission_strength = scalar(t, &["Strength"], 1.0);
        }
        "ShaderNodeBsdfTransparent" => s.alpha = Value::Const(Vec4::ZERO),
        "ShaderNodeEeveeSpecular" => {
            if let Some(v) = input(t, &["Base Color"]) {
                s.base = v;
            }
            if let Some(v) = input(t, &["Roughness"]) {
                s.roughness = v;
            }
            if let Some(v) = input(t, &["Emissive Color"]) {
                s.emission = v;
            }
            s.normal = normal_input(t, stack, n, depth);
        }
        "ShaderNodeMixShader" | "ShaderNodeAddShader" => {
            // A mix with Transparent BSDF is alpha; otherwise the first
            // shader that isn't transparent wins.
            let shaders: Vec<(usize, usize)> = node
                .inputs
                .iter()
                .enumerate()
                .filter(|(_, sk)| sk.name == "Shader")
                .filter_map(|(k, _)| g.source(n, k).map(|src| (k, src.0)))
                .collect();
            let transparent = |i: usize| g.nodes[i].idname == "ShaderNodeBsdfTransparent";
            let fac = match input(t, &["Fac"]) {
                Some(Value::Const(c)) => Some(c.x),
                _ => None,
            };
            let fac_value = g.input(n, &["Fac"]).map(|k| trace_input(t, stack, n, k, depth));
            match shaders.as_slice() {
                [(k1, a), (_, b)] if node.idname == "ShaderNodeMixShader" && (transparent(*a) || transparent(*b)) => {
                    let opaque = if transparent(*a) { *b } else { *a };
                    shader_into(t, stack, opaque, s, depth + 1);
                    // The second shader shows by Fac: transparent first means
                    // alpha = fac, transparent second means 1 - fac.
                    let _ = k1;
                    s.alpha = match (fac, fac_value) {
                        (Some(f), _) => Value::Const(Vec4::splat(if transparent(*a) { f } else { 1.0 - f })),
                        (None, Some(v)) if transparent(*a) => v,
                        _ => Value::Const(Vec4::splat(0.5)),
                    };
                }
                _ => {
                    if let Some(&(_, first)) = shaders.iter().find(|(_, i)| !transparent(*i)) {
                        shader_into(t, stack, first, s, depth + 1);
                    }
                }
            }
        }
        "NodeReroute" => {
            if let Some((from, _)) = g.source(n, 0) {
                shader_into(t, stack, from, s, depth + 1);
            }
        }
        "ShaderNodeGroup" => {
            let tree = node.obj.ptr("id");
            if t.group(tree).is_none() {
                return;
            }
            let Some(g2) = t.groups.remove(&tree) else { return };
            if let Some(on) = g2.nodes.iter().position(|nd| nd.idname == "NodeGroupOutput")
                && let Some(k) =
                    g2.nodes[on].inputs.iter().position(|sk| sk.name == "Shader" || sk.name == "BSDF").or(Some(0))
                && let Some((inner, _)) = g2.source(on, k)
            {
                let mut inner_stack = stack.to_vec();
                inner_stack.push(Frame { graph: &g2, group_node: Some(n) });
                shader_into(t, &inner_stack, inner, s, depth + 1);
            }
            t.groups.insert(tree, g2);
        }
        _ => {}
    }
}

/// A shader's Normal input when it comes from a tangent-space Normal Map
/// node with an image.
fn normal_input(t: &mut Tracer, stack: &[Frame], n: usize, depth: usize) -> Option<(Value, f32)> {
    let g = stack.last().unwrap().graph;
    let k = g.input(n, &["Normal"])?;
    let (mut from, _) = g.source(n, k)?;
    // Through reroutes to the Normal Map node.
    for _ in 0..8 {
        if g.nodes[from].idname != "NodeReroute" {
            break;
        }
        from = g.source(from, 0)?.0;
    }
    let node = &g.nodes[from];
    if node.idname != "ShaderNodeNormalMap" {
        return None;
    }
    // Only tangent space (custom1 0) maps onto the scene's normal maps.
    if node.obj.int(&["custom1"]).unwrap_or(0) != 0 {
        return None;
    }
    let strength = match g.input(from, &["Strength"]).map(|k| trace_input(t, stack, from, k, depth)) {
        Some(Value::Const(c)) => c.x,
        _ => 1.0,
    };
    let color = g.input(from, &["Color"]).map(|k| trace_input(t, stack, from, k, depth))?;
    matches!(color, Value::Image { .. }).then_some((color, strength))
}

//endregion

/*┌─────────────────╮
| Building        |
└─────────────────╯*/
//region Building

/// A finished material and what its primitives must carry.
#[derive(Clone)]
pub struct Built {
    pub material: Material,
    /// UV maps for the primitive's first and second UV sets, by name
    /// (None for the mesh's default).
    pub uv: [Option<String>; 2],
    /// Uses a second UV set at all.
    pub uv1: bool,
    /// The colour attribute the material multiplies in, by name ("" for
    /// the default).
    pub color: Option<String>,
}

/// Builds materials and their textures, sharing decoded images.
pub struct Materials<'a> {
    f: &'a BlendFile,
    tracer: Tracer<'a>,
    limits: Limits,
    dir: Option<std::path::PathBuf>,
    /// Decoded images by Image address; None when the image failed.
    images: HashMap<u64, Option<Arc<Texture>>>,
    /// Scene textures by (image, packing) so materials share them.
    textures: HashMap<(u64, u64, u8), usize>,
    pub out_textures: Vec<Arc<Texture>>,
    pub warnings: Vec<String>,
    built: HashMap<u64, Built>,
}

impl<'a> Materials<'a> {
    pub fn new(f: &'a BlendFile, limits: &Limits, dir: Option<&Path>) -> Materials<'a> {
        Materials {
            f,
            tracer: Tracer { f, groups: HashMap::new() },
            limits: limits.clone(),
            dir: dir.map(Path::to_path_buf),
            images: HashMap::new(),
            textures: HashMap::new(),
            out_textures: Vec::new(),
            warnings: Vec::new(),
            built: HashMap::new(),
        }
    }

    /// The material for Material data-block `ma` (an old address), built
    /// once.
    pub fn get(&mut self, ma: u64) -> Option<Built> {
        if let Some(b) = self.built.get(&ma) {
            return Some(b.clone());
        }
        let obj = self.f.resolve(ma, self.f.sdna.struct_named("Material"))?;
        if obj.type_name() != "Material" {
            return None;
        }
        let built = self.build(&obj);
        self.built.insert(ma, built.clone());
        Some(built)
    }

    fn build(&mut self, ma: &Obj<'a>) -> Built {
        let name = ma.id_name();
        let viewport = Vec4::new(
            ma.float(&["r"]).unwrap_or(0.8),
            ma.float(&["g"]).unwrap_or(0.8),
            ma.float(&["b"]).unwrap_or(0.8),
            ma.float(&["a", "alpha"]).unwrap_or(1.0),
        );
        let shading = node_shading(&mut self.tracer, ma).unwrap_or_else(|| {
            let mut s = Shading {
                base: Value::Const(viewport),
                metallic: Value::Const(Vec4::splat(ma.float(&["metallic"]).unwrap_or(0.0))),
                roughness: Value::Const(Vec4::splat(ma.float(&["roughness"]).unwrap_or(0.5))),
                ..Default::default()
            };
            if let Some(tex) = internal_texture(ma) {
                s.base = tex;
            }
            s
        });
        let mut m = Material { name: name.clone(), ..Default::default() };
        let mut uv: [Option<String>; 2] = [None, None];
        let mut uv1 = false;
        let mut color = None;
        // Which UV set a texture's UV map lands in: the first map named, or
        // the default, takes set 0; a different one takes set 1.
        let mut uv_set = |map: &Mapping| -> u8 {
            let want = map.uv.clone();
            if uv[0].is_none() && !uv1 && want.is_some() {
                uv[0] = want;
                return 0;
            }
            if want == uv[0] {
                return 0;
            }
            uv[1] = want;
            uv1 = true;
            1
        };
        // 1. Base colour, with alpha from the same image when linked.
        match &shading.base {
            Value::Const(c) => m.base_color = Vec4::new(c.x, c.y, c.z, 1.0),
            Value::Image { image, map, .. } => {
                m.base_color = Vec4::ONE;
                m.base_color_texture = self.texture(*image, 4, map, &mut uv_set, ColorSpace::Srgb);
            }
            Value::Attribute(a) => {
                m.base_color = Vec4::ONE;
                color = Some(a.clone());
            }
        }
        // 2. Alpha.
        let blend_method = ma.int(&["blend_method"]).unwrap_or(0);
        let threshold = ma.float(&["alpha_threshold"]).unwrap_or(0.5);
        let alpha_mode = |used: bool| match (used, blend_method) {
            (false, _) => AlphaMode::Opaque,
            (true, 3) => AlphaMode::Mask(threshold),
            _ => AlphaMode::Blend,
        };
        match &shading.alpha {
            Value::Const(c) if c.x < 0.999 => {
                m.base_color.w = c.x.clamp(0.0, 1.0);
                m.alpha_mode = alpha_mode(true);
            }
            Value::Image { image, channel: 3, .. } if matches!(&shading.base, Value::Image { image: b, .. } if b == image) =>
            {
                m.alpha_mode = alpha_mode(true);
            }
            _ => {}
        }
        // 3. Metallic and roughness, packed as glTF wants (roughness in
        //    green, metallic in blue).
        let (metal_c, rough_c) = (constant(&shading.metallic, 0.0), constant(&shading.roughness, 0.5));
        m.metallic = metal_c.unwrap_or(1.0).clamp(0.0, 1.0);
        m.roughness = rough_c.unwrap_or(1.0).clamp(0.0, 1.0);
        let metal_img = image_of(&shading.metallic);
        let rough_img = image_of(&shading.roughness);
        if metal_img.is_some() || rough_img.is_some() {
            let map = metal_img.or(rough_img).map(|(_, _, m)| m.clone()).unwrap_or_default();
            m.metallic_roughness_texture = self.packed_mr(rough_img, metal_img, &map, &mut uv_set);
            if m.metallic_roughness_texture.is_none() {
                m.metallic = metal_c.unwrap_or(0.0);
                m.roughness = rough_c.unwrap_or(0.5);
            }
        }
        // 4. Normal map.
        if let Some((Value::Image { image, map, .. }, strength)) = &shading.normal {
            m.normal_texture = self.texture(*image, 4, map, &mut uv_set, ColorSpace::Linear);
            m.normal_scale = *strength;
        }
        // 5. Emission.
        match &shading.emission {
            Value::Const(c) => m.emissive = Vec3::new(c.x, c.y, c.z) * shading.emission_strength,
            Value::Image { image, map, .. } if shading.emission_strength > 0.0 => {
                m.emissive = Vec3::splat(shading.emission_strength);
                m.emissive_texture = self.texture(*image, 4, map, &mut uv_set, ColorSpace::Srgb);
            }
            _ => {}
        }
        // 6. The rest.
        m.ior = shading.ior.clamp(1.0, 3.0);
        m.transmission = shading.transmission.clamp(0.0, 1.0);
        m.clearcoat = shading.coat.clamp(0.0, 1.0);
        m.clearcoat_roughness = shading.coat_roughness.clamp(0.0, 1.0);
        // Blender's specular 0.5 is F0 = 4%, glTF's factor 1.
        m.specular = (shading.specular * 2.0).clamp(0.0, 1.0);
        m.unlit = shading.unlit;
        let cull = ma.int(&["blend_flag"]).unwrap_or(0) & (1 << 2) != 0;
        m.double_sided = !cull;
        Built { material: m, uv, uv1, color }
    }

    /// A texture reference to channel `channel` of image `image`.
    fn texture(
        &mut self,
        image: u64,
        channel: usize,
        map: &Mapping,
        uv_set: &mut dyn FnMut(&Mapping) -> u8,
        space: ColorSpace,
    ) -> Option<TexRef> {
        let tex = self.image(image)?;
        let key = (image, 0, channel as u8);
        let index = match self.textures.get(&key) {
            Some(&i) => i,
            None => {
                let mut t = (*tex).clone();
                if t.color_space == ColorSpace::Unspecified {
                    t.color_space = space;
                }
                self.out_textures.push(Arc::new(t));
                let i = self.out_textures.len() - 1;
                self.textures.insert(key, i);
                i
            }
        };
        Some(self.tex_ref(index, map, uv_set))
    }

    fn tex_ref(&self, index: usize, map: &Mapping, uv_set: &mut dyn FnMut(&Mapping) -> u8) -> TexRef {
        let mut r = TexRef::new(index);
        r.uv_set = uv_set(map);
        r.transform = map.transform;
        let wrap = map.wrap.unwrap_or(Wrap::Repeat);
        let filter = if map.nearest { Filter::Nearest } else { Filter::Linear };
        r.sampler = Sampler { wrap_u: wrap, wrap_v: wrap, mag: filter, min: filter, mipmaps: true };
        r
    }

    /// One texture holding roughness in green and metallic in blue, from
    /// one image or two.
    fn packed_mr(
        &mut self,
        rough: Option<(u64, usize, &Mapping)>,
        metal: Option<(u64, usize, &Mapping)>,
        map: &Mapping,
        uv_set: &mut dyn FnMut(&Mapping) -> u8,
    ) -> Option<TexRef> {
        let key = (rough.map_or(0, |r| r.0 ^ (r.1 as u64) << 60), metal.map_or(0, |m| m.0 ^ (m.1 as u64) << 60), 255);
        if let Some(&i) = self.textures.get(&key) {
            return Some(self.tex_ref(i, map, uv_set));
        }
        let rt = rough.and_then(|(i, c, _)| Some((self.image(i)?, c)));
        let mt = metal.and_then(|(i, c, _)| Some((self.image(i)?, c)));
        if rt.is_none() && mt.is_none() {
            return None;
        }
        let size = |t: &Option<(Arc<Texture>, usize)>| t.as_ref().map_or((0, 0), |(t, _)| (t.width(), t.height()));
        let ((rw, rh), (mw, mh)) = (size(&rt), size(&mt));
        let (w, h) = (rw.max(mw).max(1), rh.max(mh).max(1));
        let mut px = vec![255u8; (w * h * 4) as usize];
        for y in 0..h {
            for x in 0..w {
                let i = ((y * w + x) * 4) as usize;
                let at = |t: &(Arc<Texture>, usize)| {
                    let (tw, th) = (t.0.width(), t.0.height());
                    let v = image::sample(&t.0, x * tw / w, y * th / h, if t.1 == 4 { 0 } else { t.1 });
                    (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8
                };
                px[i] = 255;
                if let Some(r) = &rt {
                    px[i + 1] = at(r);
                }
                if let Some(m) = &mt {
                    px[i + 2] = at(m);
                }
            }
        }
        let tex = Texture::from_rgba8(w, h, px, ColorSpace::Linear);
        self.out_textures.push(Arc::new(tex));
        let i = self.out_textures.len() - 1;
        self.textures.insert(key, i);
        Some(self.tex_ref(i, map, uv_set))
    }

    /// The decoded pixels of Image data-block `image`, decoded once.
    fn image(&mut self, image: u64) -> Option<Arc<Texture>> {
        if let Some(t) = self.images.get(&image) {
            return t.clone();
        }
        let tex =
            self.f.resolve(image, self.f.sdna.struct_named("Image")).filter(|o| o.type_name() == "Image").and_then(
                |im| match image::load(&im, self.dir.as_deref(), &self.limits) {
                    Ok(t) => Some(Arc::new(t)),
                    Err(e) => {
                        self.warnings.push(e);
                        None
                    }
                },
            );
        self.images.insert(image, tex.clone());
        tex
    }
}

fn constant(v: &Value, _default: f32) -> Option<f32> {
    match v {
        Value::Const(c) => Some(c.x),
        _ => None,
    }
}

fn image_of(v: &Value) -> Option<(u64, usize, &Mapping)> {
    match v {
        Value::Image { image, channel, map } => Some((*image, *channel, map)),
        _ => None,
    }
}

/// The colour texture of a Blender Internal material (before 2.8): the
/// first enabled image texture slot that maps to colour.
fn internal_texture(ma: &Obj) -> Option<Value> {
    let n = ma.field("mtex").map_or(0, |f| f.elems as usize);
    for i in 0..n {
        let Some(mtex) = ma.follow(ma.ptr_at("mtex", i), Some("MTex")) else { continue };
        if mtex.int(&["mapto"]).unwrap_or(0) & 1 == 0 {
            continue;
        }
        let Some(tex) = mtex.deref("tex") else { continue };
        if tex.int(&["type"]) != Some(8) {
            continue;
        }
        let image = tex.ptr("ima");
        if image == 0 {
            continue;
        }
        let uv = mtex.string("uvname");
        let map = Mapping { uv: (!uv.is_empty()).then_some(uv), ..Default::default() };
        return Some(Value::Image { image, channel: 4, map });
    }
    None
}

//endregion

/// Whether a texture's pixels are floats.
#[allow(dead_code)]
fn is_float(t: &Texture) -> bool {
    matches!(t.levels[0].surfaces.first(), Some(Pixels::Rgba32F(_)))
}
