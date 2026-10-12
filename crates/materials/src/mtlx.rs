//! MaterialX documents (`.mtlx`): their first material, read into the shared
//! shader graph and mapped onto a glTF-style material.
//!
//! Connections follow the MaterialX rules: `nodename` names a node in the
//! same node graph (or the document), `nodegraph` plus `output` names an
//! output of a node graph, and `interfacename` inside a node graph takes the
//! graph's own input.  File names get the nearest `fileprefix` and are
//! relative to the document.  Version 1.37's `shaderref` materials are read
//! too.

use crate::graph::{GInput, GNode, GValue, Graph, Link, category_of, is_surface};
use crate::realize::{FileRef, MatDesc};
use roxmltree::{Document, Node, ParsingOptions};
use slerp_scene::{Asset, Info, Limits, LoadError};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Elements at document or node-graph level that aren't shading nodes.
const NOT_NODES: &[&str] = &[
    "nodedef",
    "typedef",
    "implementation",
    "look",
    "lookgroup",
    "materialassign",
    "collection",
    "geominfo",
    "geompropdef",
    "propertyset",
    "propertysetassign",
    "variantset",
    "variantassign",
    "backdrop",
    "unitdef",
    "unittypedef",
    "targetdef",
    "attributedef",
    "include",
    "token",
    "input",
    "output",
    "parameter",
    "material",
];

pub(crate) fn load(path: &Path, bytes: &[u8], limits: &Limits) -> Result<Asset, LoadError> {
    let parsed = parse(path, bytes, limits.neighbours)?;
    let mut info = Info { format: format!("MaterialX {}", parsed.version), ..Default::default() };
    if parsed.material_names.len() > 1 {
        info.details.push(("Materials".into(), parsed.material_names.join(", ")));
    }
    info.warnings.extend(parsed.warnings);
    Ok(crate::realize::material_asset(path, None, parsed.desc, info, None, limits))
}

pub(crate) struct Parsed {
    pub version: String,
    pub desc: MatDesc,
    pub material_names: Vec<String>,
    pub warnings: Vec<String>,
}

/*┌─────────────────╮
| Reading         |
└─────────────────╯*/
//region Reading

/// An input as written, before its connection is looked up.
struct RawInput<'a> {
    /// The folder of the document it came from, which file names are
    /// relative to.
    dir: &'a Path,
    name: &'a str,
    ty: &'a str,
    value: Option<&'a str>,
    nodename: Option<&'a str>,
    nodegraph: Option<&'a str>,
    output: Option<&'a str>,
    interfacename: Option<&'a str>,
    colorspace: Option<&'a str>,
    fileprefix: &'a str,
}

struct RawNode<'a> {
    /// The node graph holding it, or `None` at document level.
    scope: Option<&'a str>,
    name: &'a str,
    /// For a version 1.37 `shaderref`, the material it belongs to.
    legacy_material: Option<&'a str>,
    category: String,
    ty: &'a str,
    inputs: Vec<RawInput<'a>>,
}

/// A node graph's interface: its inputs, and its outputs as
/// (name, nodename, output) triples.
#[derive(Default)]
struct RawGraph<'a> {
    inputs: Vec<RawInput<'a>>,
    outputs: Vec<(&'a str, Option<&'a str>, Option<&'a str>)>,
}

/// Where a document-level `<output>` points: its `nodename`, `nodegraph`
/// and `output` attributes.
type OutputTarget<'a> = (Option<&'a str>, Option<&'a str>, Option<&'a str>);

struct Reader<'a> {
    nodes: Vec<RawNode<'a>>,
    index: HashMap<(Option<&'a str>, &'a str), usize>,
    graphs: HashMap<&'a str, RawGraph<'a>>,
    /// Document-level `<output>` elements.
    outputs: HashMap<&'a str, OutputTarget<'a>>,
}

/// A document read into the shared graph: its materials as (name, surface
/// shader node, displacement node), the one to show first.
pub(crate) struct Doc {
    pub version: String,
    pub graph: Graph,
    pub materials: Vec<(String, Option<usize>, Option<usize>)>,
    pub warnings: Vec<String>,
}

pub(crate) fn parse(path: &Path, bytes: &[u8], neighbours: bool) -> Result<Parsed, LoadError> {
    let Doc { version, graph, materials, mut warnings } = read(path, bytes, neighbours)?;
    let Some((name, surface, displacement)) = materials.first().cloned() else {
        return Err(LoadError::Unsupported("MaterialX document without a material".into()));
    };
    let desc = match surface {
        Some(s) if is_surface(&graph.nodes[s].category) => graph.map(&name, s, displacement, 1.0),
        Some(s) => {
            warnings.push(format!("unsupported shading model {}", graph.nodes[s].category));
            MatDesc::new(&name, &graph.nodes[s].category)
        }
        None => {
            warnings.push("material without a surface shader".into());
            MatDesc::new(&name, "none")
        }
    };
    Ok(Parsed { version, desc, material_names: materials.into_iter().map(|m| m.0).collect(), warnings })
}

/// Reads a document into the shared graph.
pub(crate) fn read(path: &Path, bytes: &[u8], neighbours: bool) -> Result<Doc, LoadError> {
    let text =
        std::str::from_utf8(crate::strip_bom(bytes)).map_err(|_| LoadError::invalid("MaterialX is not UTF-8"))?;
    let doc =
        Document::parse_with_options(text, options()).map_err(|e| LoadError::invalid(format!("MaterialX XML: {e}")))?;
    if doc.root_element().tag_name().name() != "materialx" {
        return Err(LoadError::NotMine);
    }
    let dir = path.parent().map(Path::to_path_buf).unwrap_or_default();

    // 1. Documents pulled in with `xi:include`, when other files may be read:
    // their nodes and materials join this document's.
    let mut warnings = Vec::new();
    let included = if neighbours { includes(&doc, &dir, &mut warnings) } else { Vec::new() };
    let included_docs: Vec<(&Path, Document)> = included
        .iter()
        .filter_map(|(d, t)| Some((d.as_path(), Document::parse_with_options(t, options()).ok()?)))
        .collect();

    // 2. Gather nodes, node graphs and outputs, this document's first.
    let mut r = Reader { nodes: Vec::new(), index: HashMap::new(), graphs: HashMap::new(), outputs: HashMap::new() };
    r.gather(doc.root_element(), &dir);
    for (d, doc) in &included_docs {
        if doc.root_element().tag_name().name() == "materialx" {
            r.gather(doc.root_element(), d);
        }
    }
    let root = doc.root_element();

    // 3. Resolve connections into the shared graph.
    let mut graph = Graph::default();
    for node in &r.nodes {
        let inputs = node
            .inputs
            .iter()
            .map(|inp| {
                let (value, link) = r.resolve(node.scope, inp, 0);
                GInput { name: inp.name.to_string(), value, link, color_space: inp.colorspace.map(str::to_string) }
            })
            .collect();
        graph.nodes.push(GNode {
            name: node.name.to_string(),
            category: node.category.clone(),
            ty: node.ty.to_string(),
            inputs,
        });
    }

    // 4. Find the materials; the first is the one to show.
    let mut materials: Vec<(String, Option<usize>, Option<usize>)> = Vec::new();
    for (i, n) in graph.nodes.iter().enumerate() {
        match n.category.as_str() {
            "surfacematerial" => {
                let link = |name: &str| graph.input(i, name).and_then(|inp| inp.link.as_ref()).map(|l| l.node);
                materials.push((n.name.clone(), link("surfaceshader"), link("displacementshader")));
            }
            _ => {
                if let Some(m) = r.nodes[i].legacy_material {
                    materials.push((m.to_string(), Some(i), None));
                }
            }
        }
    }
    if materials.is_empty()
        && let Some(i) = graph.nodes.iter().position(|n| is_surface(&n.category))
    {
        materials.push((graph.nodes[i].name.clone(), Some(i), None));
    }
    // A look in this document says which material its main geometry wears.
    let assigned = root
        .children()
        .filter(|c| c.tag_name().name() == "look")
        .flat_map(|look| look.children().filter(|c| c.tag_name().name() == "materialassign"))
        .find_map(|a| a.attribute("material"));
    if let Some(i) = assigned.and_then(|name| materials.iter().position(|m| m.0 == name)) {
        let m = materials.remove(i);
        materials.insert(0, m);
    }
    let version = root.attribute("version").unwrap_or("1.38").to_string();
    Ok(Doc { version, graph, materials, warnings })
}

fn raw_input<'a>(el: Node<'a, 'a>, dir: &'a Path, prefix: &'a str, cs: Option<&'a str>) -> RawInput<'a> {
    RawInput {
        dir,
        name: el.attribute("name").unwrap_or(""),
        ty: el.attribute("type").unwrap_or(""),
        value: el.attribute("value"),
        nodename: el.attribute("nodename"),
        nodegraph: el.attribute("nodegraph"),
        output: el.attribute("output"),
        interfacename: el.attribute("interfacename"),
        colorspace: el.attribute("colorspace").or(cs),
        fileprefix: el.attribute("fileprefix").unwrap_or(prefix),
    }
}

impl<'a> Reader<'a> {
    /// Adds a document's nodes, node graphs and outputs.
    fn gather(&mut self, root: Node<'a, 'a>, dir: &'a Path) {
        let doc_prefix = root.attribute("fileprefix").unwrap_or("");
        let doc_cs = root.attribute("colorspace");
        for el in root.children().filter(Node::is_element) {
            let tag = el.tag_name().name();
            match tag {
                "nodegraph" => {
                    // A graph with a `nodedef` implements a node type; it
                    // isn't part of any material here.
                    if el.has_attribute("nodedef") {
                        continue;
                    }
                    let Some(gname) = el.attribute("name") else { continue };
                    let prefix = el.attribute("fileprefix").unwrap_or(doc_prefix);
                    let cs = el.attribute("colorspace").or(doc_cs);
                    let mut graph = RawGraph::default();
                    for child in el.children().filter(Node::is_element) {
                        match child.tag_name().name() {
                            "input" | "parameter" => graph.inputs.push(raw_input(child, dir, prefix, cs)),
                            "output" => {
                                if let Some(name) = child.attribute("name") {
                                    graph.outputs.push((name, child.attribute("nodename"), child.attribute("output")));
                                }
                            }
                            t if NOT_NODES.contains(&t) => {}
                            _ => self.add_node(Some(gname), child, dir, prefix, cs),
                        }
                    }
                    self.graphs.insert(gname, graph);
                }
                "output" => {
                    if let Some(name) = el.attribute("name") {
                        let target = (el.attribute("nodename"), el.attribute("nodegraph"), el.attribute("output"));
                        self.outputs.insert(name, target);
                    }
                }
                "material" => self.add_legacy_material(el, dir, doc_prefix, doc_cs),
                t if NOT_NODES.contains(&t) || el.tag_name().namespace().is_some() => {}
                _ => self.add_node(None, el, dir, doc_prefix, doc_cs),
            }
        }
    }

    fn add_node(
        &mut self,
        scope: Option<&'a str>,
        el: Node<'a, 'a>,
        dir: &'a Path,
        prefix: &'a str,
        cs: Option<&'a str>,
    ) {
        let Some(name) = el.attribute("name") else { return };
        let prefix = el.attribute("fileprefix").unwrap_or(prefix);
        let cs = el.attribute("colorspace").or(cs);
        let category = el.tag_name().name().to_string();
        let inputs = el
            .children()
            .filter(|c| matches!(c.tag_name().name(), "input" | "parameter"))
            .map(|c| raw_input(c, dir, prefix, cs))
            .collect();
        let node =
            RawNode { scope, name, legacy_material: None, category, ty: el.attribute("type").unwrap_or(""), inputs };
        self.index.insert((scope, name), self.nodes.len());
        self.nodes.push(node);
    }

    /// MaterialX 1.37 and older: `<material>` holding a `<shaderref>` whose
    /// `node` attribute is the shader and whose `bindinput`s are its inputs.
    fn add_legacy_material(&mut self, el: Node<'a, 'a>, dir: &'a Path, prefix: &'a str, cs: Option<&'a str>) {
        let Some(mat_name) = el.attribute("name") else { return };
        for sr in el.children().filter(|c| c.tag_name().name() == "shaderref") {
            let Some(shader) = sr.attribute("node") else { continue };
            let inputs = sr
                .children()
                .filter(|c| matches!(c.tag_name().name(), "bindinput" | "bindparam"))
                .map(|c| raw_input(c, dir, prefix, cs))
                .collect();
            let name = sr.attribute("name").unwrap_or(mat_name);
            let node = RawNode {
                scope: None,
                name,
                legacy_material: Some(mat_name),
                category: category_of(shader),
                ty: "surfaceshader",
                inputs,
            };
            self.nodes.push(node);
            return;
        }
    }

    fn lookup(&self, scope: Option<&'a str>, name: &str) -> Option<usize> {
        self.index.get(&(scope, name)).or_else(|| self.index.get(&(None, name))).copied()
    }

    /// An input's value or connection, following interface inputs and node
    /// graph outputs.
    fn resolve(&self, scope: Option<&'a str>, inp: &RawInput<'a>, depth: u32) -> (Option<GValue>, Option<Link>) {
        if depth > 16 {
            return (None, None);
        }
        if let (Some(iface), Some(g)) = (inp.interfacename, scope)
            && let Some(gi) = self.graphs.get(g).and_then(|graph| graph.inputs.iter().find(|i| i.name == iface))
        {
            return self.resolve(None, gi, depth + 1);
        }
        let output = inp.output.unwrap_or("out").to_string();
        if let Some(nn) = inp.nodename {
            if let Some(node) = self.lookup(scope, nn) {
                return (None, Some(Link { node, output }));
            }
            return (None, None);
        }
        if let Some(ng) = inp.nodegraph {
            return (None, self.graph_output(ng, inp.output));
        }
        if let Some(out) = inp.output.and_then(|o| self.outputs.get(o)) {
            return match out {
                (Some(nn), _, o) => {
                    (None, self.lookup(None, nn).map(|node| Link { node, output: o.unwrap_or("out").to_string() }))
                }
                (None, Some(ng), o) => (None, self.graph_output(ng, *o)),
                _ => (None, None),
            };
        }
        (inp.value.and_then(|v| self.value(inp, v)), None)
    }

    fn graph_output(&self, ng: &str, output: Option<&str>) -> Option<Link> {
        let graph = self.graphs.get(ng)?;
        let out = match output {
            Some(o) => graph.outputs.iter().find(|x| x.0 == o)?,
            None if graph.outputs.len() == 1 => &graph.outputs[0],
            None => return None,
        };
        let (gname, _) = self.graphs.get_key_value(ng)?;
        let node = self.lookup(Some(gname), out.1?)?;
        Some(Link { node, output: out.2.unwrap_or("out").to_string() })
    }

    fn value(&self, inp: &RawInput, v: &str) -> Option<GValue> {
        match inp.ty {
            "filename" => {
                let label = format!("{}{}", inp.fileprefix, v);
                let path = resolve_file(inp.dir, &label);
                Some(GValue::File { file: FileRef::Disk(path), label })
            }
            "string" => Some(GValue::Str(v.to_string())),
            "boolean" => Some(GValue::Num(vec![if v.trim() == "true" { 1.0 } else { 0.0 }])),
            _ => {
                let nums: Vec<f32> = v.split(',').filter_map(|s| s.trim().parse().ok()).collect();
                if nums.is_empty() { Some(GValue::Str(v.to_string())) } else { Some(GValue::Num(nums)) }
            }
        }
    }
}

fn options<'a>() -> ParsingOptions<'a> {
    ParsingOptions { allow_dtd: true, nodes_limit: 4_000_000, ..Default::default() }
}

/// The documents `xi:include` elements name, recursively, as (folder,
/// text) pairs.  At most 16 documents of 16 MB each are read.
fn includes(doc: &Document, dir: &Path, warnings: &mut Vec<String>) -> Vec<(PathBuf, String)> {
    let mut out: Vec<(PathBuf, String)> = Vec::new();
    let mut seen: Vec<PathBuf> = Vec::new();
    let mut pending: Vec<(PathBuf, String)> = include_hrefs(doc).into_iter().map(|h| (dir.to_path_buf(), h)).collect();
    while let Some((from, href)) = pending.pop() {
        let path = resolve_file(&from, &href);
        let key = path.canonicalize().unwrap_or_else(|_| path.clone());
        if seen.contains(&key) || out.len() >= 16 {
            continue;
        }
        seen.push(key);
        slerp_scene::record_neighbour(&path);
        let text = match std::fs::metadata(&path) {
            Ok(m) if m.len() <= 16 << 20 => std::fs::read(&path).ok().and_then(|b| String::from_utf8(b).ok()),
            _ => None,
        };
        let Some(text) = text else {
            warnings.push(format!("include {href} not read"));
            continue;
        };
        let sub_dir = path.parent().map(Path::to_path_buf).unwrap_or_default();
        if let Ok(sub) = Document::parse_with_options(&text, options()) {
            pending.extend(include_hrefs(&sub).into_iter().map(|h| (sub_dir.clone(), h)));
        }
        out.push((sub_dir, text));
    }
    out
}

fn include_hrefs(doc: &Document) -> Vec<String> {
    doc.root_element()
        .children()
        .filter(|c| c.tag_name().name() == "include")
        .filter_map(|c| c.attribute("href").map(str::to_string))
        .collect()
}

/// A texture path relative to the document, with UDIM tokens pointing at
/// the first tile.
fn resolve_file(dir: &Path, name: &str) -> PathBuf {
    let name = name.replace("<UDIM>", "1001").replace("<UVTILE>", "u1_v1").replace('\\', "/");
    let p = Path::new(&name);
    if p.is_absolute() { p.to_path_buf() } else { dir.join(p) }
}

//endregion
