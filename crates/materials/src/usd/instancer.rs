//! Point instancers: each instance places one of the instancer's
//! prototypes by a position, an orientation and a scale.  Each prototype is
//! built once, as a template of meshes and their transforms relative to the
//! prototype's parent, and each instance becomes a node (or a node and one
//! child per mesh) that shares those meshes rather than copying them.
//!
//! Instances are drawn one by one, so a stage may place at most
//! `MAX_INSTANCES` of them (fewer for thumbnails), within budgets of nodes
//! and of triangles drawn.  Beyond that an instancer is thinned evenly,
//! keeping every n-th instance, with a warning.  Velocities and
//! accelerations are ignored: the instances stand where they are at the
//! first sample.  Nested instancers and instancers inside prototypes
//! expand inside the template; a prototype that contains its own instancer
//! is refused as a cycle.

use super::Value;
use super::geom::Builder;
use slerp_scene::glam::{Mat4, Quat, Vec3};
use slerp_scene::{LoadError, Node, Transform};
use std::collections::HashSet;

/// The most instances a stage's point instancers may place in the viewer,
/// and in thumbnails, where a small picture can't tell the difference.
const MAX_INSTANCES: usize = 100_000;
const MAX_INSTANCES_THUMB: usize = 10_000;
/// The most nodes instancing may add, and the most triangles the copies may
/// draw between them (in the viewer and in thumbnails).
const MAX_NODES: usize = 400_000;
const MAX_NODES_THUMB: usize = 40_000;
const MAX_TRIANGLES: u64 = 100_000_000;
const MAX_TRIANGLES_THUMB: u64 = 10_000_000;
/// How deep instancers may nest inside each other's prototypes.
const MAX_NESTING: usize = 8;

/// What instancing may still add to the scene, and what it placed.
pub(super) struct Budget {
    instances: usize,
    nodes: usize,
    triangles: u64,
    /// Instances placed, and instances the files asked for.
    placed: usize,
    wanted: usize,
    instancers: usize,
}

impl Budget {
    pub fn new(limits: &slerp_scene::Limits) -> Budget {
        let thumb = limits.max_texture_size.is_some();
        Budget {
            instances: if thumb { MAX_INSTANCES_THUMB } else { MAX_INSTANCES },
            nodes: if thumb { MAX_NODES_THUMB } else { MAX_NODES },
            triangles: if thumb { MAX_TRIANGLES_THUMB } else { MAX_TRIANGLES },
            placed: 0,
            wanted: 0,
            instancers: 0,
        }
    }

    /// Facts for the inspector.
    pub fn details(&self) -> Option<(String, String)> {
        if self.instancers == 0 {
            return None;
        }
        let shown = if self.placed < self.wanted {
            format!("{} of {} shown", self.placed, self.wanted)
        } else {
            format!("{}", self.placed)
        };
        Some(("Instances".into(), format!("{shown}, from {} point instancers", self.instancers)))
    }
}

/// One mesh of a prototype, with its transform relative to the prototype's
/// parent (so including the prototype root's own transform, as USD does).
struct Part {
    mesh: usize,
    matrix: Mat4,
    weights: Vec<f32>,
    name: String,
}

/// A prototype built once: its parts, and what each copy costs.
struct Template {
    name: String,
    parts: Vec<Part>,
    triangles: u64,
    /// Instances inside it (from instancers in the prototype), placed and
    /// asked for, which each copy repeats.
    inner_placed: usize,
    inner_wanted: usize,
}

impl Template {
    /// Nodes one instance takes at most: an instance node and a child per
    /// part.  Usually it is a node per part, when the instance's transform
    /// composes with each part's exactly.
    fn nodes(&self) -> usize {
        if self.parts.len() == 1 { 1 } else { 1 + self.parts.len() }
    }
}

/// Expands the point instancer `pi` into instance nodes under its node
/// `node`.
pub(super) fn expand(b: &mut Builder, pi: usize, node: usize) -> Result<(), LoadError> {
    let stage = b.stage;
    let p = &stage.prims[pi];
    if b.expanding.contains(&pi) || b.expanding.len() >= MAX_NESTING {
        b.ctx.warn(format!("{}: point instancers nest in a cycle or too deeply; skipped", p.path));
        return Ok(());
    }

    // 1. The instances: prototype indices, positions, orientations (as
    //    floats when authored, else halves), scales, and which are hidden.
    let (Some(Value::Ints(proto_indices)), Some(Value::Floats(positions, 3))) =
        (p.value("protoIndices"), p.value("positions"))
    else {
        b.ctx.warn(format!("{}: point instancer without protoIndices or positions", p.path));
        return Ok(());
    };
    let count = proto_indices.len();
    if positions.len() / 3 != count {
        b.ctx.warn(format!("{}: {} positions for {count} instances", p.path, positions.len() / 3));
    }
    let count = count.min(positions.len() / 3);
    let sized = |name: &str, width: usize| match p.value(name) {
        Some(Value::Floats(v, w)) if *w == width && v.len() == count * width && count > 0 => Some(v.as_slice()),
        _ => None,
    };
    let orientations = sized("orientationsf", 4).or_else(|| sized("orientations", 4));
    let scales = sized("scales", 3);
    let ids = match p.value("ids") {
        Some(Value::Ints(ids)) if ids.len() == count => Some(ids),
        _ => None,
    };
    let mut hidden: HashSet<i64> = p.inactive_ids.iter().copied().collect();
    if let Some(Value::Ints(inv)) = p.value("invisibleIds") {
        hidden.extend(inv.iter().map(|&i| i as i64));
    }

    // 2. The prototypes, each built once as a template.  A prototype that
    //    holds its own instancer would nest forever.
    let targets = p.prop("prototypes").map_or(&[][..], |q| q.targets.as_slice());
    b.expanding.push(pi);
    let mut templates: Vec<Option<Template>> = Vec::with_capacity(targets.len());
    let used: HashSet<i32> = proto_indices[..count].iter().copied().collect();
    for (k, t) in targets.iter().enumerate() {
        let proto = stage.find(t);
        let template = match proto {
            _ if !used.contains(&(k as i32)) => None,
            Some(proto) if stage.is_within(pi, proto) => {
                b.ctx.warn(format!("{}: prototype {t} contains its own instancer; skipped", p.path));
                None
            }
            Some(proto) => build_template(b, proto)?,
            None => {
                b.ctx.warn(format!("{}: prototype {t} not found", p.path));
                None
            }
        };
        templates.push(template);
    }
    b.expanding.pop();

    // 3. The instances to place: visible ones whose prototype was built,
    //    thinned evenly when they would exceed what is left of the budget.
    let visible: Vec<usize> = (0..count)
        .filter(|&i| {
            let id = ids.map_or(i as i64, |ids| ids[i] as i64);
            let template = usize::try_from(proto_indices[i]).ok().and_then(|k| templates.get(k)?.as_ref());
            !hidden.contains(&id) && template.is_some_and(|t| !t.parts.is_empty())
        })
        .collect();
    let template_of = |i: usize| templates[proto_indices[i] as usize].as_ref().expect("filtered above");
    let sum = |f: &dyn Fn(&Template) -> f64| visible.iter().map(|&i| f(template_of(i))).sum::<f64>();
    let (instances, nodes) = (sum(&|t| 1.0 + t.inner_placed as f64), sum(&|t| t.nodes() as f64));
    let triangles = sum(&|t| t.triangles as f64);
    let budget = &mut b.budget;
    let mut fraction = 1.0f64;
    for (have, want) in
        [(budget.instances as f64, instances), (budget.nodes as f64, nodes), (budget.triangles as f64, triangles)]
    {
        if want > have {
            fraction = fraction.min(have / want);
        }
    }
    let keep = ((visible.len() as f64 * fraction) as usize).min(visible.len());
    let kept: Vec<usize> = (0..keep).map(|k| visible[k * visible.len() / keep]).collect();
    budget.instancers += 1;
    budget.wanted += visible.iter().map(|&i| 1 + template_of(i).inner_wanted).sum::<usize>();
    let placed: usize = kept.iter().map(|&i| 1 + template_of(i).inner_placed).sum();
    budget.placed += placed;
    budget.instances = budget.instances.saturating_sub(placed);
    if keep < visible.len() {
        b.ctx.warn(format!(
            "{}: {keep} of {} instances shown, thinned evenly to stay within Slerp's limits",
            p.path,
            visible.len()
        ));
    }

    // 4. A node per instance (and its parts), sharing the prototypes' meshes.
    for i in kept {
        let t = template_of(i);
        let translation = Vec3::from_slice(&positions[i * 3..i * 3 + 3]);
        let rotation = orientations.map_or(Quat::IDENTITY, |o| {
            let q = Quat::from_xyzw(o[i * 4 + 1], o[i * 4 + 2], o[i * 4 + 3], o[i * 4]);
            if q.is_finite() && q.length_squared() > 1e-12 { q.normalize() } else { Quat::IDENTITY }
        });
        let scale = scales.map_or(Vec3::ONE, |s| Vec3::from_slice(&s[i * 3..i * 3 + 3]));
        let at = Transform { translation, rotation, scale };
        if !(translation.is_finite() && scale.is_finite()) {
            continue;
        }
        let name = |part: &Part| {
            if t.parts.len() == 1 { format!("{} {i}", t.name) } else { format!("{}/{} {i}", t.name, part.name) }
        };
        let add = |b: &mut Builder, node: Node, parent: usize| {
            let ni = b.scene.nodes.len();
            b.scene.nodes.push(node);
            b.scene.nodes[parent].children.push(ni);
            b.budget.nodes = b.budget.nodes.saturating_sub(1);
            ni
        };
        match composed(&at, t) {
            // A node per part, straight under the instancer.
            Some(transforms) => {
                for (part, transform) in t.parts.iter().zip(transforms) {
                    let mut n = Node::new(name(part));
                    (n.transform, n.mesh, n.weights) = (transform, Some(part.mesh), part.weights.clone());
                    add(b, n, node);
                }
            }
            // An instance node, and a child per part.
            None => {
                let mut instance = Node::new(format!("{} {i}", t.name));
                instance.transform = at;
                let ni = add(b, instance, node);
                for part in &t.parts {
                    let mut n = Node::new(part.name.clone());
                    n.transform = Transform::from_matrix(part.matrix);
                    (n.mesh, n.weights) = (Some(part.mesh), part.weights.clone());
                    add(b, n, ni);
                }
            }
        }
        b.budget.triangles = b.budget.triangles.saturating_sub(t.triangles);
    }
    Ok(())
}

/// Each part's transform with the instance's applied, when composing them
/// loses nothing (no shear, which a non-uniform scale on a turned part
/// would make), so that every part can be a node of its own.
fn composed(at: &Transform, t: &Template) -> Option<Vec<Transform>> {
    t.parts
        .iter()
        .map(|part| {
            if part.matrix == Mat4::IDENTITY {
                return Some(*at);
            }
            let m = at.matrix() * part.matrix;
            let x = Transform::from_matrix(m);
            let size = m.to_cols_array().iter().fold(0.0f32, |a, v| a.max(v.abs())).max(1e-6);
            x.matrix().abs_diff_eq(m, size * 1e-4).then_some(x)
        })
        .collect()
}

/// Builds prototype `proto` with the scene builder, flattens its nodes into
/// parts and takes the nodes away again, keeping the meshes.  What
/// instancers inside it place is charged to the budget per copy, by the
/// caller, rather than once here.
fn build_template(b: &mut Builder, proto: usize) -> Result<Option<Template>, LoadError> {
    let before = b.scene.nodes.len();
    let inherited = b.inherited_at(proto);
    let budget = (b.budget.instances, b.budget.nodes, b.budget.triangles, b.budget.placed, b.budget.wanted);
    b.prototype_depth += 1;
    let root = b.visit(proto, inherited);
    b.prototype_depth -= 1;
    let (inner_placed, inner_wanted) = (b.budget.placed - budget.3, b.budget.wanted - budget.4);
    (b.budget.instances, b.budget.nodes, b.budget.triangles, b.budget.placed, b.budget.wanted) = budget;
    let Some(root) = root? else { return Ok(None) };
    let mut parts = Vec::new();
    let mut stack = vec![(root, Mat4::IDENTITY)];
    while let Some((n, parent)) = stack.pop() {
        let node = &b.scene.nodes[n];
        let m = parent * node.transform.matrix();
        if let Some(mesh) = node.mesh {
            parts.push(Part { mesh, matrix: m, weights: node.weights.clone(), name: node.name.clone() });
        }
        stack.extend(node.children.iter().map(|&c| (c, m)));
    }
    parts.reverse();
    b.scene.nodes.truncate(before);
    let triangles = parts
        .iter()
        .map(|p| b.scene.meshes[p.mesh].primitives.iter().map(|q| q.triangle_count() as u64).sum::<u64>())
        .sum();
    let name = b.stage.prims[proto].name.clone();
    Ok(Some(Template { name, parts, triangles, inner_placed, inner_wanted }))
}
