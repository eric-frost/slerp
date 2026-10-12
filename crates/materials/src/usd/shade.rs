//! A USD Material's shading network, read into the shared shader graph and
//! mapped onto a glTF-style material.  Both UsdPreviewSurface networks and
//! MaterialX networks stored in USD (`info:id = "ND_..."`) are understood.
//! Connections may pass through NodeGraph and Material interface inputs and
//! outputs, which either forward to a shader or hold the value themselves.

use super::stage::{CProp, Stage};
use super::{Ctx, Value};
use crate::graph::{GInput, GNode, GValue, Graph, Link, category_of, is_surface};
use crate::realize::MatDesc;
use std::collections::HashMap;

/// How long a chain of connections may get.
const MAX_DEPTH: u32 = 64;

/// The material bound to `mat`, and the texture-coordinate primvar its
/// textures read (from a UsdPrimvarReader), if it names one.
pub(crate) fn material_desc(ctx: &mut Ctx, stage: &Stage, mat: usize) -> (MatDesc, Option<String>) {
    let mut b = Builder { ctx, stage, graph: Graph::default(), nodes: HashMap::new() };
    let prim = &stage.prims[mat];

    // 1. The surface: the universal output, MaterialX's, then any other
    // render context's, preferring one we can map.
    let mut outputs: Vec<&CProp> = Vec::new();
    for name in ["outputs:surface", "outputs:mtlx:surface"] {
        outputs.extend(prim.prop(name));
    }
    let others: Vec<&CProp> = prim
        .props
        .iter()
        .filter(|p| p.name.ends_with(":surface") && !outputs.iter().any(|o| o.name == p.name))
        .collect();
    outputs.extend(others);
    let mut surface = None;
    let mut unmappable = None;
    for out in outputs {
        let Some(Resolved::Link(link)) = out.targets.first().map(|t| b.follow(t, 0)) else { continue };
        if is_surface(&b.graph.nodes[link.node].category) {
            surface = Some(link.node);
            break;
        }
        unmappable.get_or_insert(link.node);
    }

    // 2. Displacement, for MaterialX networks (UsdPreviewSurface has its
    // own input).
    let displacement = ["outputs:mtlx:displacement", "outputs:displacement"].iter().find_map(|name| {
        let target = prim.prop(name)?.targets.first()?;
        match b.follow(target, 0) {
            Resolved::Link(l) => Some(l.node),
            _ => None,
        }
    });

    let desc = match (surface, unmappable) {
        (Some(s), _) => b.graph.map(&prim.name, s, displacement, stage.meters_per_unit),
        (None, Some(n)) => {
            let cat = b.graph.nodes[n].category.clone();
            b.ctx.warn(format!("material {}: unsupported shader {cat}", prim.name));
            MatDesc::new(&prim.name, &cat)
        }
        (None, None) => {
            b.ctx.warn(format!("material {} has no surface shader", prim.name));
            MatDesc::new(&prim.name, "none")
        }
    };
    let uv = b.graph.nodes.iter().find_map(|n| {
        let key = match n.category.as_str() {
            "UsdPrimvarReader" => "varname",
            "geompropvalue" => "geomprop",
            _ => return None,
        };
        match &n.inputs.iter().find(|i| i.name == key)?.value {
            Some(GValue::Str(s)) => Some(s.clone()),
            _ => None,
        }
    });
    (desc, uv)
}

enum Resolved {
    Link(Link),
    Value(GValue),
    None,
}

struct Builder<'c, 'a> {
    ctx: &'c mut Ctx<'a>,
    stage: &'c Stage,
    graph: Graph,
    /// Shader prim to graph node.
    nodes: HashMap<usize, usize>,
}

impl Builder<'_, '_> {
    /// Where a connection to `target` (`/Path/To/Prim.outputs:name`) leads.
    fn follow(&mut self, target: &str, depth: u32) -> Resolved {
        if depth > MAX_DEPTH {
            return Resolved::None;
        }
        let Some((prim_path, prop_name)) = target.rsplit_once('.') else { return Resolved::None };
        let Some(pi) = self.stage.find(prim_path) else { return Resolved::None };
        let p = &self.stage.prims[pi];
        if p.type_name == "Shader" {
            let node = self.node(pi, depth + 1);
            let output = prop_name.strip_prefix("outputs:").unwrap_or(prop_name).to_string();
            return Resolved::Link(Link { node, output });
        }
        // A NodeGraph or Material interface: forwards, or holds a value.
        let Some(prop) = p.prop(prop_name) else { return Resolved::None };
        if let Some(t) = prop.targets.first() {
            return self.follow(&t.clone(), depth + 1);
        }
        match self.convert(prop) {
            Some(v) => Resolved::Value(v),
            None => Resolved::None,
        }
    }

    fn node(&mut self, pi: usize, depth: u32) -> usize {
        if let Some(&n) = self.nodes.get(&pi) {
            return n;
        }
        let p = &self.stage.prims[pi];
        let id = p.token("info:id").unwrap_or(&p.type_name);
        let ty = p
            .props
            .iter()
            .find(|q| q.name.starts_with("outputs:"))
            .map(|q| mtlx_type(&q.type_name))
            .unwrap_or_default();
        let idx = self.graph.nodes.len();
        self.graph.nodes.push(GNode { name: p.name.clone(), category: category_of(id), ty, inputs: Vec::new() });
        self.nodes.insert(pi, idx);
        if depth > MAX_DEPTH {
            return idx;
        }
        let mut inputs = Vec::new();
        for prop in p.props.iter().filter(|q| q.name.starts_with("inputs:")) {
            let name = prop.name["inputs:".len()..].to_string();
            let (value, link) = match prop.targets.first() {
                Some(t) => match self.follow(t, depth + 1) {
                    Resolved::Link(l) => (None, Some(l)),
                    Resolved::Value(v) => (Some(v), None),
                    Resolved::None => (self.convert(prop), None),
                },
                None => (self.convert(prop), None),
            };
            inputs.push(GInput { name, value, link, color_space: prop.color_space.clone() });
        }
        self.graph.nodes[idx].inputs = inputs;
        idx
    }

    fn convert(&self, prop: &CProp) -> Option<GValue> {
        Some(match prop.value.as_deref()? {
            Value::Bool(b) => GValue::Num(vec![*b as i32 as f32]),
            Value::Int(i) => GValue::Num(vec![*i as f32]),
            Value::Float(f) => GValue::Num(vec![*f as f32]),
            Value::Tuple(t) => GValue::Num(t.iter().map(|&x| x as f32).collect()),
            Value::Floats(v, _) => GValue::Num(v.clone()),
            Value::Str(s) => GValue::Str(s.clone()),
            Value::Asset(a) => GValue::File { file: self.ctx.file_ref(prop.layer, a), label: a.clone() },
            _ => return None,
        })
    }
}

/// MaterialX's name for a USD attribute type, as the graph's node types.
fn mtlx_type(usd: &str) -> String {
    // `color3f`, `texCoord2h`: drop the precision letter after the digit.
    let base = match usd.strip_suffix(['f', 'd', 'h']) {
        Some(b) if b.ends_with(|c: char| c.is_ascii_digit()) => b,
        _ => usd,
    };
    match base {
        "color3" => "color3",
        "color4" => "color4",
        "float" | "double" | "half" => "float",
        "float2" | "texCoord2" | "double2" => "vector2",
        "float3" | "normal3" | "vector3" | "point3" | "double3" => "vector3",
        "float4" | "double4" => "vector4",
        "token" => "surfaceshader",
        other => other,
    }
    .to_string()
}
