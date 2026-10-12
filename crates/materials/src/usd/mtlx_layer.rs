//! MaterialX documents referenced as USD layers, laid out the way USD's own
//! MaterialX plugin presents them: each material at
//! `/MaterialX/Materials/<name>`, with a Shader prim per node its surface
//! and displacement use, connected as in the document.  The shading code
//! then reads them like any MaterialX network stored in USD.

use super::{Layer, PrimSpec, Property, Specifier, Value};
use crate::graph::{GValue, Graph};
use crate::realize::FileRef;
use slerp_scene::LoadError;
use std::path::Path;
use std::sync::Arc;

pub(crate) fn layer(path: &Path, bytes: &[u8], neighbours: bool) -> Result<Layer, LoadError> {
    let doc = crate::mtlx::read(path, bytes, neighbours)?;
    let mut layer = Layer {
        format: format!("MaterialX {}", doc.version),
        default_prim: Some("MaterialX".into()),
        ..Default::default()
    };
    let scope = |children: Vec<String>| PrimSpec {
        specifier: Specifier::Def,
        type_name: "Scope".into(),
        children,
        ..Default::default()
    };
    layer.prims.insert("/".into(), PrimSpec { children: vec!["MaterialX".into()], ..Default::default() });
    layer.prims.insert("/MaterialX".into(), scope(vec!["Materials".into()]));
    let mut names = Vec::new();
    for (name, surface, displacement) in &doc.materials {
        let name = prim_name(name);
        if names.contains(&name) {
            continue;
        }
        let mat_path = format!("/MaterialX/Materials/{name}");
        let mut mat = PrimSpec { specifier: Specifier::Def, type_name: "Material".into(), ..Default::default() };

        // 1. The nodes the material uses, following connections.
        let mut used: Vec<usize> = Vec::new();
        let mut pending: Vec<usize> = surface.iter().chain(displacement).copied().collect();
        while let Some(n) = pending.pop() {
            if used.contains(&n) || used.len() >= 4096 {
                continue;
            }
            used.push(n);
            pending.extend(doc.graph.nodes[n].inputs.iter().filter_map(|i| i.link.as_ref().map(|l| l.node)));
        }

        // 2. A Shader prim for each.
        for &n in &used {
            let shader = shader(&doc.graph, n, &mat_path);
            mat.children.push(format!("n{n}"));
            layer.prims.insert(format!("{mat_path}/n{n}"), shader);
        }
        for (output, node) in [("outputs:mtlx:surface", surface), ("outputs:mtlx:displacement", displacement)] {
            if let Some(n) = node {
                mat.props.push(Property {
                    name: output.into(),
                    type_name: "token".into(),
                    targets: vec![format!("{mat_path}/n{n}.outputs:out")],
                    ..Default::default()
                });
            }
        }
        layer.prims.insert(mat_path, mat);
        names.push(name);
    }
    layer.prims.insert("/MaterialX/Materials".into(), scope(names));
    Ok(layer)
}

fn shader(graph: &Graph, n: usize, mat_path: &str) -> PrimSpec {
    let node = &graph.nodes[n];
    let mut spec = PrimSpec { specifier: Specifier::Def, type_name: "Shader".into(), ..Default::default() };
    let token = |name: &str, v: &str| Property {
        name: name.into(),
        type_name: "token".into(),
        default: Some(Arc::new(Value::Str(v.into()))),
        ..Default::default()
    };
    spec.props.push(token("info:id", &node.category));
    let output_type = match node.ty.as_str() {
        "color3" => "color3f",
        "color4" => "color4f",
        "vector2" => "float2",
        "vector3" => "float3",
        "vector4" => "float4",
        "float" => "float",
        _ => "token",
    };
    spec.props.push(Property { name: "outputs:out".into(), type_name: output_type.into(), ..Default::default() });
    for input in &node.inputs {
        let value = input.value.as_ref().map(|v| {
            Arc::new(match v {
                GValue::Num(x) if x.len() == 1 => Value::Float(x[0] as f64),
                GValue::Num(x) => Value::Tuple(x.iter().map(|&f| f as f64).collect()),
                GValue::Str(s) => Value::Str(s.clone()),
                // MaterialX resolved the path; an absolute path stays put
                // when the shading code resolves it against this layer.
                GValue::File { file: FileRef::Disk(p), .. } => Value::Asset(p.to_string_lossy().into_owned()),
                GValue::File { label, .. } => Value::Asset(label.clone()),
            })
        });
        let targets = input.link.iter().map(|l| format!("{mat_path}/n{}.outputs:{}", l.node, l.output)).collect();
        spec.props.push(Property {
            name: format!("inputs:{}", input.name),
            default: value,
            targets,
            color_space: input.color_space.clone(),
            ..Default::default()
        });
    }
    spec
}

/// A material name usable as a prim name.
fn prim_name(name: &str) -> String {
    let mut out: String = name.chars().map(|c| if c.is_ascii_alphanumeric() || c == '_' { c } else { '_' }).collect();
    if out.is_empty() || out.starts_with(|c: char| c.is_ascii_digit()) {
        out.insert(0, '_');
    }
    out
}
