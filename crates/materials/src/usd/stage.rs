//! Composing layers into one prim tree, in a simplified version of USD's
//! strength order: for each prim, its specs in the root layer stack (the
//! root layer and its sublayers), each followed by what it brings in through
//! variant selections, inherits, references and payloads, recursively.
//! The first opinion found for a value wins.
//!
//! Paths authored inside a referenced prim are remapped to where the
//! reference puts it, so material bindings and shader connections in a
//! referenced asset still point at the right prims.

use super::usda::child_path;
use super::{Ctx, Samples, Specifier, Value};
use crate::realize::FileRef;
use slerp_scene::LoadError;
use std::collections::HashMap;
use std::sync::Arc;

/// A stage may compose to at most this many prims and properties, and
/// prims may nest this deep.  References can multiply a small file into a
/// huge stage, legitimately (instancing) or not (a reference to its own
/// ancestor), so these bound the memory a stage can take.
const MAX_PRIMS: usize = 500_000;
const MAX_PROPS: usize = 4_000_000;
const MAX_NESTING: usize = 256;
/// How deep references may nest, and how many specs one prim may compose.
const MAX_ARC_DEPTH: u32 = 32;
const MAX_SOURCES: usize = 512;

/// A composed prim.
#[derive(Debug, Default)]
pub(crate) struct CPrim {
    pub name: String,
    pub path: String,
    /// The parent prim; the pseudo-root is its own parent.
    pub parent: usize,
    pub children: Vec<usize>,
    pub type_name: String,
    /// Defined by a `def` (with every ancestor defined) and not a class.
    pub defined: bool,
    /// Defined by a `def` itself, whatever its ancestors are: a point
    /// instancer's prototypes may sit under an `over`, which keeps them
    /// from being drawn except as instances.
    pub def: bool,
    pub active: bool,
    pub props: Vec<CProp>,
    pub api_schemas: Vec<String>,
    /// The asset info dictionary and the layer that authored it.
    pub asset_info: Option<(Value, usize)>,
    /// A point instancer's ids of instances switched off.
    pub inactive_ids: Vec<i64>,
}

#[derive(Debug, Default)]
pub(crate) struct CProp {
    pub name: String,
    pub type_name: String,
    pub value: Option<Arc<Value>>,
    /// Every time sample, when the strongest opinion is time-sampled and
    /// the layer kept them (see `keeps_samples`).
    pub samples: Option<Arc<Samples>>,
    /// An inbetween blend shape's weight.
    pub weight: Option<f64>,
    /// Connection sources or relationship targets, in stage paths.
    pub targets: Vec<String>,
    pub interpolation: Option<String>,
    pub element_size: Option<i64>,
    pub color_space: Option<String>,
    /// A material binding that overrides those below it.
    pub stronger: bool,
    /// The layer the value came from, which asset paths are relative to.
    pub layer: usize,
}

impl CPrim {
    pub fn prop(&self, name: &str) -> Option<&CProp> {
        self.props.iter().find(|p| p.name == name)
    }

    pub fn value(&self, name: &str) -> Option<&Value> {
        self.prop(name)?.value.as_deref()
    }

    pub fn token(&self, name: &str) -> Option<&str> {
        self.value(name)?.as_str()
    }
}

#[derive(Debug, Default)]
pub(crate) struct Stage {
    /// Index 0 is the pseudo-root.
    pub prims: Vec<CPrim>,
    pub index: HashMap<String, usize>,
    pub up_z: bool,
    pub meters_per_unit: f32,
    pub default_prim: Option<usize>,
    /// Time codes per second, and the time code playback starts at when
    /// the root layer says.
    pub time_codes_per_second: f64,
    pub start_time: Option<f64>,
}

impl Stage {
    pub fn find(&self, path: &str) -> Option<usize> {
        self.index.get(path).copied()
    }

    /// The prims from the root's child down to `prim`, outermost first.
    pub fn ancestry(&self, prim: usize) -> Vec<usize> {
        let mut out = vec![prim];
        let mut i = prim;
        while i != 0 && self.prims[i].parent != 0 && out.len() <= MAX_NESTING {
            i = self.prims[i].parent;
            out.push(i);
        }
        out.reverse();
        out
    }

    /// Whether `prim` is `ancestor` or lies below it.
    pub fn is_within(&self, prim: usize, ancestor: usize) -> bool {
        let mut i = prim;
        for _ in 0..=MAX_NESTING {
            if i == ancestor {
                return true;
            }
            if i == 0 {
                return false;
            }
            i = self.prims[i].parent;
        }
        false
    }

    /// Defined Material prims, depth first.
    pub fn materials(&self) -> Vec<usize> {
        let mut out = Vec::new();
        let mut stack = vec![0];
        while let Some(i) = stack.pop() {
            let p = &self.prims[i];
            if i != 0 && p.defined && p.active && p.type_name == "Material" {
                out.push(i);
            }
            stack.extend(p.children.iter().rev());
        }
        out
    }

    /// The picture an `AssetPreviewsAPI` names on the default prim or a
    /// root prim: `assetInfo:previews:thumbnails:default:defaultImage`.
    pub fn preview_image(&self, ctx: &Ctx) -> Option<(FileRef, String)> {
        let candidates = self.default_prim.into_iter().chain(self.prims[0].children.iter().copied());
        for i in candidates {
            let Some((info, layer)) = &self.prims[i].asset_info else { continue };
            let image = info.get("previews")?.get("thumbnails")?.get("default")?.get("defaultImage")?;
            if let Some(asset) = image.as_str().filter(|a| !a.is_empty()) {
                return Some((ctx.file_ref(*layer, asset), asset.to_string()));
            }
        }
        None
    }
}

/// One spec contributing to a composed prim, and how to remap paths
/// authored in its layer: a path under `from` moves under `to`.
#[derive(Clone, Debug)]
struct Src {
    layer: usize,
    spec: String,
    from: String,
    to: String,
}

impl Src {
    fn map(&self, path: &str) -> String {
        if self.from == self.to {
            return path.to_string();
        }
        match path.strip_prefix(self.from.as_str()) {
            Some(rest) if rest.is_empty() || rest.starts_with('/') || rest.starts_with('.') => {
                format!("{}{rest}", self.to)
            }
            _ => path.to_string(),
        }
    }
}

/// How a node of a prim's composition came in.  Sibling nodes are ordered
/// by kind, which is USD's strength order: local opinions, then inherits,
/// variants, references and payloads.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum ArcKind {
    Local,
    Inherit,
    Variant,
    Reference,
    Payload,
}

/// One node of a prim's composition graph: a spec site and the nodes its
/// arcs bring in.  A prim's sources, strongest first, are the graph's nodes
/// in depth-first order.  A child prim's graph is its parent's with each
/// site moved to the child's path, so arcs on ancestors (a variant on the
/// root prim overriding a referenced model's bindings) keep their strength.
#[derive(Clone, Debug)]
struct PNode {
    src: Src,
    kind: ArcKind,
    children: Vec<PNode>,
    /// Whether this node's own arcs have been added.
    done: bool,
}

/// Bookkeeping while one prim's graph is built.
struct Elaboration<'p> {
    path: &'p str,
    selections: Vec<(String, String)>,
    /// The sites from the graph's root down to the node being built.
    chain: Vec<(usize, String)>,
    nodes: usize,
    warned: &'p mut bool,
}

pub(crate) fn compose(ctx: &mut Ctx) -> Result<Stage, LoadError> {
    // 1. The root layer stack: the root layer and its sublayers, strongest
    // first.
    let stack_layers = ctx.stack(0);
    let meta = |f: &dyn Fn(&super::Layer) -> Option<String>| stack_layers.iter().find_map(|&l| f(&ctx.layers[l].1));
    let up = meta(&|l| l.up_axis.clone());
    let mpu = stack_layers.iter().find_map(|&l| ctx.layers[l].1.meters_per_unit);
    let default_prim = meta(&|l| l.default_prim.clone());
    // Time metadata comes from the root layer alone, as in USD; the frame
    // rate stands in for missing time codes per second.
    let root = &ctx.layers[0].1;
    let tcps = root.time_codes_per_second.or(root.frames_per_second).filter(|t| t.is_finite() && *t > 0.0);
    let start_time = root.start_time.filter(|t| t.is_finite());

    let mut stage = Stage {
        up_z: up.as_deref() == Some("Z"),
        // USD's fallback unit is the centimetre.
        meters_per_unit: mpu.filter(|m| m.is_finite() && *m > 0.0).unwrap_or(0.01) as f32,
        time_codes_per_second: tcps.unwrap_or(24.0),
        start_time,
        ..Default::default()
    };
    stage.prims.push(CPrim { path: "/".into(), defined: true, active: true, ..Default::default() });
    stage.index.insert("/".into(), 0);

    // 2. Walk the tree, composing each prim from its graph.
    let root_graph: Vec<PNode> = stack_layers
        .iter()
        .map(|&l| PNode {
            src: Src { layer: l, spec: "/".into(), from: "/".into(), to: "/".into() },
            kind: ArcKind::Local,
            children: Vec::new(),
            done: true,
        })
        .collect();
    let mut work: Vec<(usize, Vec<PNode>)> = vec![(0, root_graph)];
    let mut warned = false;
    let mut props = 0usize;
    while let Some((idx, mut graph)) = work.pop() {
        // The pseudo-root has no opinions of its own beyond its children.
        if idx != 0 {
            let path = stage.prims[idx].path.clone();
            let mut e =
                Elaboration { path: &path, selections: Vec::new(), chain: Vec::new(), nodes: 0, warned: &mut warned };
            for node in &mut graph {
                elaborate(ctx, node, &mut e, 0);
            }
        }
        let mut sources = Vec::new();
        flatten(&graph, &mut sources);
        if idx != 0 {
            fill(ctx, &mut stage.prims[idx], &sources);
            props += stage.prims[idx].props.len();
            if props > MAX_PROPS {
                return Err(LoadError::TooLarge(format!("more than {MAX_PROPS} properties")));
            }
        }

        // Children: names in order across the sources.
        let mut names: Vec<String> = Vec::new();
        for s in &sources {
            if let Some(spec) = ctx.layers[s.layer].1.prims.get(&s.spec) {
                for c in &spec.children {
                    if valid_name(c) && !names.contains(c) {
                        names.push(c.clone());
                    }
                }
            }
        }
        if stage.prims[idx].path.matches('/').count() >= MAX_NESTING {
            ctx.warn(format!("prims nest deeper than {MAX_NESTING} levels; the rest were skipped"));
            continue;
        }
        let mut children = Vec::with_capacity(names.len());
        let parent_path = stage.prims[idx].path.clone();
        let parent_defined = stage.prims[idx].defined;
        for name in names {
            let child_graph: Vec<PNode> = graph.iter().filter_map(|n| map_child(ctx, n, &name)).collect();
            if stage.prims.len() >= MAX_PRIMS {
                return Err(LoadError::TooLarge(format!("more than {MAX_PRIMS} prims")));
            }
            let path = child_path(&parent_path, &name);
            let child = stage.prims.len();
            stage.prims.push(CPrim {
                name,
                path: path.clone(),
                parent: idx,
                defined: parent_defined,
                active: true,
                ..Default::default()
            });
            stage.index.insert(path, child);
            children.push((child, child_graph));
        }
        stage.prims[idx].children = children.iter().map(|c| c.0).collect();
        work.extend(children.into_iter().rev());
    }

    stage.default_prim = default_prim.and_then(|name| stage.find(&format!("/{}", name.trim_start_matches('/'))));
    Ok(stage)
}

/// Whether a child name is a prim name.  A damaged file can list an empty
/// name, which would make a prim its own child.
fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && !name.contains(['/', '.', '{', '}', '[', ']', ':', '<', '>', '@', '"', '\''])
        && !name.contains(char::is_whitespace)
}

/// A node moved to the child prim `name`, keeping the parts of its subtree
/// that have specs there.
fn map_child(ctx: &Ctx, n: &PNode, name: &str) -> Option<PNode> {
    let spec = child_path(&n.src.spec, name);
    let children: Vec<PNode> = n.children.iter().filter_map(|c| map_child(ctx, c, name)).collect();
    if children.is_empty() && !ctx.layers[n.src.layer].1.prims.contains_key(&spec) {
        return None;
    }
    Some(PNode { src: Src { spec, ..n.src.clone() }, kind: n.kind, children, done: false })
}

fn flatten(nodes: &[PNode], out: &mut Vec<Src>) {
    for n in nodes {
        out.push(n.src.clone());
        flatten(&n.children, out);
    }
}

/// Adds the arcs of each node's spec (variants, inherits, references,
/// payloads) as child nodes, depth first, so a node's variant selection
/// sees the selections of every stronger node.
fn elaborate(ctx: &mut Ctx, node: &mut PNode, e: &mut Elaboration, depth: u32) {
    let path = e.path;
    if depth > MAX_ARC_DEPTH || e.nodes >= MAX_SOURCES {
        if !*e.warned {
            ctx.warn(format!("composition of {path} was cut short"));
            *e.warned = true;
        }
        node.children.clear();
        return;
    }
    e.nodes += 1;
    e.chain.push((node.src.layer, ns_path(&node.src.spec)));
    if !node.done {
        node.done = true;
        let mut added = Vec::new();
        if let Some(spec) = ctx.layers[node.src.layer].1.prims.get(&node.src.spec) {
            // Stronger nodes come first, so the first selection of a set wins.
            for (set, sel) in &spec.variant_selection {
                if !e.selections.iter().any(|(k, _)| k == set) {
                    e.selections.push((set.clone(), sel.clone()));
                }
            }
            let variant_sets = spec.variant_sets.clone();
            let inherits = spec.inherits.clone();
            let refs: Vec<_> = spec.references.iter().map(|r| (r.clone(), ArcKind::Reference)).collect();
            let payloads: Vec<_> = spec.payloads.iter().map(|r| (r.clone(), ArcKind::Payload)).collect();
            let s = &node.src;
            for set in variant_sets {
                let Some((_, sel)) = e.selections.iter().find(|(k, _)| *k == set).cloned() else { continue };
                let spec = format!("{}{{{set}={sel}}}", s.spec);
                if ctx.layers[s.layer].1.prims.contains_key(&spec) {
                    added.push(new_node(Src { spec, ..s.clone() }, ArcKind::Variant));
                }
            }
            for class in inherits {
                if cycles(e, &[s.layer], &class) {
                    ctx.warn(format!("{path}: inherit cycle through {class}"));
                    continue;
                }
                added.push(new_node(
                    Src { layer: s.layer, spec: class.clone(), from: class, to: path.to_string() },
                    ArcKind::Inherit,
                ));
            }
            for (r, kind) in refs.into_iter().chain(payloads) {
                // A reference to another layer brings in its whole layer
                // stack; one within the layer looks in the layer stack the
                // layer belongs to (the root's, when it is one of its
                // layers).
                let layers = if r.asset.is_empty() {
                    let root = ctx.stack(0);
                    if root.contains(&s.layer) { root } else { ctx.stack(s.layer) }
                } else {
                    match ctx.load_layer(s.layer, &r.asset) {
                        Some(l) => ctx.stack(l),
                        None => continue,
                    }
                };
                let prim = if r.prim.is_empty() {
                    layers.iter().find_map(|&l| ctx.layers[l].1.default_prim_path())
                } else {
                    Some(r.prim.clone())
                };
                let Some(prim) = prim else {
                    ctx.warn(format!("reference to {} has no default prim", r.asset));
                    continue;
                };
                if cycles(e, &layers, &prim) {
                    ctx.warn(format!("{path}: reference cycle through {prim}"));
                    continue;
                }
                for layer in layers {
                    added.push(new_node(
                        Src { layer, spec: prim.clone(), from: prim.clone(), to: path.to_string() },
                        kind,
                    ));
                }
            }
        }
        node.children.extend(added);
        // Stable, so arcs from ancestors stay ahead of new ones of a kind.
        node.children.sort_by_key(|c| c.kind);
    }
    for child in &mut node.children {
        elaborate(ctx, child, e, depth + 1);
    }
    e.chain.pop();
}

fn new_node(src: Src, kind: ArcKind) -> PNode {
    PNode { src, kind, children: Vec::new(), done: false }
}

/// Whether an arc to `target` in `layers` would bring a site back into its
/// own subtree: the target is one of the sites (or an ancestor of one) on
/// the way here, which would nest the prim inside itself forever.  USD
/// reports these as composition cycles.
fn cycles(e: &Elaboration, layers: &[usize], target: &str) -> bool {
    let target = ns_path(target);
    e.chain.iter().any(|(l, here)| {
        layers.contains(l) && (target == "/" || *here == target || here.starts_with(&format!("{target}/")))
    })
}

/// A spec path without its variant selections: `/A{v=x}B` is `/A/B`.
pub(crate) fn ns_path(spec: &str) -> String {
    let mut out = String::with_capacity(spec.len());
    let mut depth = 0;
    for c in spec.chars() {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    out.push('/');
                }
            }
            _ if depth == 0 => out.push(c),
            _ => {}
        }
    }
    let out = out.replace("//", "/");
    out.strip_suffix('/').filter(|o| !o.is_empty()).map(str::to_string).unwrap_or(out)
}

/// Fills a composed prim from its sources, strongest first.
fn fill(ctx: &Ctx, prim: &mut CPrim, sources: &[Src]) {
    let mut specifier = None;
    let mut active = None;
    let mut inactive_ids = None;
    let mut by_name: HashMap<String, usize> = HashMap::new();
    // Properties whose binding strength the strongest opinion has set.
    let mut seen_strength = std::collections::HashSet::new();
    for s in sources {
        let Some(spec) = ctx.layers[s.layer].1.prims.get(&s.spec) else { continue };
        if prim.type_name.is_empty() {
            prim.type_name = spec.type_name.clone();
        }
        if specifier.is_none() && spec.specifier != Specifier::Over {
            specifier = Some(spec.specifier);
        }
        if active.is_none() {
            active = spec.active;
        }
        for a in &spec.api_schemas {
            if !prim.api_schemas.contains(a) {
                prim.api_schemas.push(a.clone());
            }
        }
        if prim.asset_info.is_none() {
            prim.asset_info = spec.asset_info.clone().map(|v| (v, s.layer));
        }
        if inactive_ids.is_none() {
            inactive_ids.clone_from(&spec.inactive_ids);
        }
        for p in &spec.props {
            let i = *by_name.entry(p.name.clone()).or_insert_with(|| {
                prim.props.push(CProp { name: p.name.clone(), layer: s.layer, ..Default::default() });
                prim.props.len() - 1
            });
            let c = &mut prim.props[i];
            // The strongest spec with a default or time samples decides
            // both: a stronger default hides weaker animation.  Within a
            // spec, samples win over the default, as they do at any time
            // in USD.
            if c.value.is_none()
                && let Some(v) = p.sample.clone().or_else(|| p.default.clone())
            {
                c.value = Some(v);
                c.samples = p.samples.clone();
                c.layer = s.layer;
            }
            if c.weight.is_none() {
                c.weight = p.weight;
            }
            if c.targets.is_empty() && !p.targets.is_empty() {
                c.targets = p.targets.iter().map(|t| s.map(t)).collect();
            }
            if c.type_name.is_empty() {
                c.type_name = p.type_name.clone();
            }
            if c.interpolation.is_none() {
                c.interpolation = p.interpolation.clone();
            }
            if c.element_size.is_none() {
                c.element_size = p.element_size;
            }
            if c.color_space.is_none() {
                c.color_space = p.color_space.clone();
            }
            if let (Some(st), false) = (p.stronger, seen_strength.contains(&i)) {
                c.stronger = st;
                seen_strength.insert(i);
            }
        }
    }
    prim.def = specifier == Some(Specifier::Def);
    prim.defined = prim.defined && prim.def;
    prim.active = active.unwrap_or(true);
    prim.inactive_ids = inactive_ids.unwrap_or_default();
}
