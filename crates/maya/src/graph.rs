//! Maya's dependency graph as a file stores it: nodes with a type, a name,
//! DAG parents and the attribute values the file sets, plus the
//! connections between attributes.  Both readers (`ma` for MEL text, `mb`
//! for the binary form) fill one of these, and the builder turns it into a
//! scene.
//!
//! Attribute names are kept in Maya's short form ("t", "uvst[0].uvsp"),
//! translating the long names a hand-written file may use.  Array values
//! set piecewise (`setAttr ".vt[0:499]" ...`, then `".vt[500:999]"`) are
//! stored as they come and joined when read through [`Node::dense`].

use std::collections::HashMap;

/// A file's graph.
#[derive(Default)]
pub struct Graph {
    pub nodes: Vec<Node>,
    names: HashMap<String, Vec<usize>>,
    pub connections: Vec<Connection>,
    /// Files the scene references (`file -r`), which aren't loaded.
    pub references: Vec<String>,
    /// Metres per linear unit of the values the file stores.
    pub metres: f64,
    /// Angles in the file are in degrees (Maya ASCII with `currentUnit -a
    /// degree`); binary files store radians.
    pub degrees: bool,
    /// The scene was made with Z up.
    pub z_up: bool,
    /// The Maya version that saved the file, such as "2022".
    pub version: String,
    /// What `fileInfo` says: product, cut and so on.
    pub file_info: Vec<(String, String)>,
    pub warnings: Vec<String>,
}

/// One node.
#[derive(Default)]
pub struct Node {
    /// The node type, such as "transform" or "mesh"; for binary files whose
    /// types aren't known, the four-character code.
    pub kind: String,
    pub name: String,
    /// DAG parents: one for an ordinary node, more for an instanced shape.
    pub parents: Vec<usize>,
    pub attrs: Vec<Attr>,
}

/// One attribute value as the file sets it.
pub struct Attr {
    /// The short name without its last index range: "t", "vt",
    /// "uvst[0].uvsp", "iog[0].og[1].gcl".
    pub key: String,
    /// The element range the value covers, for array attributes set
    /// piecewise.
    pub range: Option<(u32, u32)>,
    pub value: Value,
}

pub enum Value {
    /// Numbers, booleans (as 0 and 1) and matrices.
    Nums(Vec<f64>),
    Text(String),
    Texts(Vec<String>),
    /// A component list (`f[0:5]`, `vtx[3]`): faces, edges or vertices.
    Components(Vec<Components>),
    /// Faces of a mesh (Maya ASCII's "polyFaces" type).
    Faces(Vec<Face>),
    /// A whole mesh as Maya binary files store it.
    Mesh(Box<crate::mesh::MeshData>),
    /// Skin weights as Maya binary files store them: per vertex of the
    /// attribute's range, (influence, weight) pairs.
    Weights(Vec<Vec<(u32, f32)>>),
}

/// A run of components of one kind, each range inclusive.  `None` as a
/// range means all of them (`f[*]`).
#[derive(Clone, Debug, PartialEq)]
pub struct Components {
    pub kind: ComponentKind,
    pub ranges: Vec<Option<(u32, u32)>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ComponentKind {
    Face,
    Edge,
    Vertex,
    Other,
}

/// A face as edges around its outline and holes, each loop with its
/// UV and colour indices per set.
#[derive(Clone, Debug, Default)]
pub struct Face {
    /// The outline first, then holes.
    pub loops: Vec<Loop>,
}

#[derive(Clone, Debug, Default)]
pub struct Loop {
    /// Edge indices; a negative entry `-(e + 1)` runs edge `e` backwards.
    pub edges: Vec<i32>,
    /// Per UV set: the set's index and the UV index of each corner, -1
    /// where a corner has none.
    pub uvs: Vec<(u32, Vec<i32>)>,
    /// Per colour set: the set's index and the colour index of each
    /// corner.
    pub colors: Vec<(u32, Vec<i32>)>,
}

/// `src.src_attr` drives `dst.dst_attr`.
#[derive(Clone, Debug)]
pub struct Connection {
    pub src: usize,
    pub src_attr: String,
    pub dst: usize,
    pub dst_attr: String,
}

/// Long attribute names a file may use, and their short forms.
const LONG_NAMES: &[(&str, &str)] = &[
    ("translate", "t"),
    ("translateX", "tx"),
    ("translateY", "ty"),
    ("translateZ", "tz"),
    ("rotate", "r"),
    ("rotateX", "rx"),
    ("rotateY", "ry"),
    ("rotateZ", "rz"),
    ("scale", "s"),
    ("scaleX", "sx"),
    ("scaleY", "sy"),
    ("scaleZ", "sz"),
    ("shear", "sh"),
    ("rotateOrder", "ro"),
    ("rotatePivot", "rp"),
    ("rotatePivotTranslate", "rpt"),
    ("scalePivot", "sp"),
    ("scalePivotTranslate", "spt"),
    ("rotateAxis", "ra"),
    ("jointOrient", "jo"),
    ("visibility", "v"),
    ("intermediateObject", "io"),
    ("inMesh", "i"),
    ("outMesh", "o"),
    ("worldMesh", "w"),
    ("vrts", "vt"),
    ("pnts", "pt"),
    ("edge", "ed"),
    ("face", "fc"),
    ("normals", "n"),
    ("uvSet", "uvst"),
    ("uvSetName", "uvsn"),
    ("uvSetPoints", "uvsp"),
    ("currentUVSet", "cuvs"),
    ("colorSet", "clst"),
    ("colorName", "clsn"),
    ("colorSetPoints", "clsp"),
    ("instObjGroups", "iog"),
    ("objectGroups", "og"),
    ("objectGrpCompList", "gcl"),
    ("objectGroupId", "gid"),
    ("dagSetMembers", "dsm"),
    ("surfaceShader", "ss"),
    ("outColor", "oc"),
    ("outAlpha", "oa"),
    ("outTransparency", "ot"),
    ("color", "c"),
    ("transparency", "it"),
    ("incandescence", "ic"),
    ("normalCamera", "n"),
    ("fileTextureName", "ftn"),
    ("repeatUV", "re"),
    ("offset", "of"),
    ("groupId", "gi"),
    ("inputComponents", "ic"),
    ("output", "o"),
    ("outputGeometry", "og"),
    ("input", "ip"),
];

/// The short form of one attribute path component (without its index).
fn short(name: &str) -> &str {
    LONG_NAMES.iter().find(|(l, _)| *l == name).map_or(name, |(_, s)| s)
}

/// Canonical form of an attribute path, and the index range of its last
/// component when it has one: ".uvSet[0].uvSetPoints[0:13]" gives
/// ("uvst[0].uvsp", Some((0, 13))).  "[*]" and unparsable ranges give no
/// range.
pub fn canon(path: &str) -> (String, Option<(u32, u32)>) {
    let path = path.trim_start_matches('.');
    let mut out = String::with_capacity(path.len());
    let mut range = None;
    let parts: Vec<&str> = path.split('.').collect();
    for (k, part) in parts.iter().enumerate() {
        if k > 0 {
            out.push('.');
        }
        let (name, index) = match part.find('[') {
            Some(i) => (&part[..i], Some(&part[i..])),
            None => (*part, None),
        };
        out.push_str(short(name));
        if let Some(index) = index {
            if k + 1 == parts.len() {
                range = parse_range(index);
                if range.is_none() && index != "[*]" && index != "[]" {
                    out.push_str(index);
                }
            } else {
                out.push_str(index);
            }
        }
    }
    (out, range)
}

/// "[3]" or "[0:13]".
fn parse_range(index: &str) -> Option<(u32, u32)> {
    let inner = index.strip_prefix('[')?.strip_suffix(']')?;
    match inner.split_once(':') {
        Some((a, b)) => {
            let (a, b) = (a.trim().parse().ok()?, b.trim().parse().ok()?);
            (a <= b).then_some((a, b))
        }
        None => {
            let a = inner.trim().parse().ok()?;
            Some((a, a))
        }
    }
}

/// Parses a component string such as "f[0:534]", "vtx[3]" or "f[*]".
pub fn parse_component(s: &str) -> Option<Components> {
    let open = s.find('[')?;
    let kind = match &s[..open] {
        "f" => ComponentKind::Face,
        "e" => ComponentKind::Edge,
        "vtx" => ComponentKind::Vertex,
        _ => ComponentKind::Other,
    };
    let range = &s[open..];
    let ranges = if range == "[*]" { vec![None] } else { vec![Some(parse_range(range)?)] };
    Some(Components { kind, ranges })
}

impl Node {
    /// The last value set for `key` as a whole (not a piece of an array).
    pub fn value(&self, key: &str) -> Option<&Value> {
        self.attrs.iter().rev().find(|a| a.key == key && a.range.is_none()).map(|a| &a.value)
    }

    pub fn nums(&self, key: &str) -> Option<&[f64]> {
        match self.value(key)? {
            Value::Nums(v) => Some(v),
            _ => None,
        }
    }

    pub fn num(&self, key: &str) -> Option<f64> {
        self.nums(key).and_then(|v| v.first().copied()).filter(|v| v.is_finite())
    }

    pub fn text(&self, key: &str) -> Option<&str> {
        match self.value(key)? {
            Value::Text(t) => Some(t),
            Value::Texts(t) => t.first().map(String::as_str),
            _ => None,
        }
    }

    /// Three numbers for a vector attribute, from the whole value (".t")
    /// or its parts (".tx", ".ty", ".tz"), with `default` for what isn't
    /// set.  `parts` names the parts' keys.
    pub fn vec3(&self, key: &str, parts: [&str; 3], default: [f64; 3]) -> [f64; 3] {
        let mut v = default;
        if let Some(n) = self.nums(key) {
            for (o, x) in v.iter_mut().zip(n) {
                if x.is_finite() {
                    *o = *x;
                }
            }
        }
        for (o, p) in v.iter_mut().zip(parts) {
            if let Some(x) = self.num(p) {
                *o = x;
            }
        }
        v
    }

    /// An array attribute as one list of numbers, `stride` per element,
    /// joined from every piece the file set, with zeros where none set an
    /// element.  At most `max_elements` elements; `None` when nothing set
    /// it.
    pub fn dense(&self, key: &str, stride: usize, max_elements: usize) -> Option<Vec<f64>> {
        let mut out: Vec<f64> = Vec::new();
        let mut any = false;
        // Pieces may leave gaps, but not gaps larger than what they set:
        // a lone element far out mustn't make a tiny file ask for gigabytes.
        let given: usize = self
            .attrs
            .iter()
            .filter(|a| a.key == key)
            .map(|a| match &a.value {
                Value::Nums(v) => v.len() / stride.max(1),
                _ => 0,
            })
            .sum();
        let max_elements = max_elements.min(given.saturating_mul(2).saturating_add(1 << 16));
        for a in self.attrs.iter().filter(|a| a.key == key) {
            let Value::Nums(v) = &a.value else { continue };
            let start = a.range.map_or(0, |r| r.0 as usize);
            let count = match a.range {
                Some((s, e)) => (e as usize - s as usize + 1).min(v.len() / stride.max(1)),
                None => v.len() / stride.max(1),
            };
            if count == 0 && v.is_empty() {
                continue;
            }
            let end = (start + count).min(max_elements);
            if end <= start {
                continue;
            }
            any = true;
            if out.len() < end * stride {
                out.resize(end * stride, 0.0);
            }
            let take = (end - start) * stride;
            out[start * stride..end * stride].copy_from_slice(&v[..take]);
        }
        any.then_some(out)
    }

    /// The indices `i` for which some key starts with `prefix[i]`, such as
    /// the UV sets "uvst[0]", "uvst[1]" for prefix "uvst".
    pub fn indices(&self, prefix: &str) -> Vec<u32> {
        let mut out: Vec<u32> = Vec::new();
        for a in &self.attrs {
            let Some(rest) = a.key.strip_prefix(prefix).and_then(|r| r.strip_prefix('[')) else { continue };
            let Some(end) = rest.find(']') else { continue };
            if let Ok(i) = rest[..end].parse::<u32>()
                && !out.contains(&i)
            {
                out.push(i);
            }
        }
        out.sort_unstable();
        out
    }
}

impl Graph {
    pub fn new() -> Graph {
        Graph { metres: 0.01, degrees: true, ..Default::default() }
    }

    /// The node a name or DAG path refers to: "|group1|pCube1" checks the
    /// parents, a short name gives the first node of that name.  A leading
    /// ':' (the root namespace) is ignored.
    pub fn find(&self, name: &str) -> Option<usize> {
        let name = name.trim_start_matches(':');
        if !name.contains('|') {
            return self.names.get(name).and_then(|v| v.first().copied());
        }
        let parts: Vec<&str> = name.split('|').filter(|p| !p.is_empty()).collect();
        let last = *parts.last()?;
        let candidates = self.names.get(last.trim_start_matches(':'))?;
        candidates
            .iter()
            .copied()
            .find(|&c| {
                // Walk up the first parents, matching the path backwards.
                let mut node = c;
                for want in parts.iter().rev().skip(1) {
                    match self.nodes[node].parents.first() {
                        Some(&p) if self.nodes[p].name == want.trim_start_matches(':') => node = p,
                        _ => return false,
                    }
                }
                true
            })
            .or_else(|| candidates.first().copied())
    }

    /// Adds a node and returns its index.
    pub fn add(&mut self, kind: &str, name: &str, parent: Option<usize>) -> usize {
        let name = name.trim_start_matches(':');
        let short_name = name.rsplit('|').next().unwrap_or(name).to_string();
        self.nodes.push(Node {
            kind: kind.to_string(),
            name: short_name.clone(),
            parents: parent.into_iter().collect(),
            attrs: Vec::new(),
        });
        let i = self.nodes.len() - 1;
        self.names.entry(short_name).or_default().push(i);
        i
    }

    /// The node called `name`, made when the file sets attributes of a
    /// node it never created (Maya's default nodes: ":lambert1",
    /// ":initialShadingGroup", ":time1").
    pub fn find_or_add(&mut self, name: &str) -> usize {
        if let Some(i) = self.find(name) {
            return i;
        }
        let short_name = name.trim_start_matches(':');
        let kind = match short_name {
            "lambert1" => "lambert",
            "initialShadingGroup" | "initialParticleSE" => "shadingEngine",
            "standardSurface1" => "standardSurface",
            _ => "",
        };
        self.add(kind, short_name, None)
    }

    /// Splits "node.attr.path" into the node and the canonical attribute
    /// path.  DAG paths' '|' come before the first '.'.
    pub fn plug(&mut self, plug: &str) -> Option<(usize, String)> {
        let start = plug.rfind('|').map_or(0, |i| i + 1);
        let dot = start + plug[start..].find('.')?;
        let node = self.find_or_add(&plug[..dot]);
        let (key, range) = canon(&plug[dot + 1..]);
        let attr = match range {
            Some((a, b)) if a == b => format!("{key}[{a}]"),
            Some((a, b)) => format!("{key}[{a}:{b}]"),
            None => key,
        };
        Some((node, attr))
    }

    pub fn connect(&mut self, src: &str, dst: &str) {
        if let (Some((s, sa)), Some((d, da))) = (self.plug(src), self.plug(dst)) {
            self.connections.push(Connection { src: s, src_attr: sa, dst: d, dst_attr: da });
        }
    }

    /// Connections into `node` whose attribute is `attr` or lies under it
    /// ("iog" matches "iog[0].og[1]").
    pub fn inputs<'a>(&'a self, node: usize, attr: &'a str) -> impl Iterator<Item = &'a Connection> + 'a {
        self.connections.iter().filter(move |c| c.dst == node && under(&c.dst_attr, attr))
    }

    /// Connections out of `node` whose attribute is `attr` or lies under it.
    pub fn outputs<'a>(&'a self, node: usize, attr: &'a str) -> impl Iterator<Item = &'a Connection> + 'a {
        self.connections.iter().filter(move |c| c.src == node && under(&c.src_attr, attr))
    }

    /// The node driving `node.attr`, when one does.
    pub fn input(&self, node: usize, attr: &str) -> Option<&Connection> {
        self.connections.iter().find(|c| c.dst == node && c.dst_attr == attr)
    }
}

/// Whether attribute path `a` is `prefix` or lies under it.
pub fn under(a: &str, prefix: &str) -> bool {
    a == prefix || (a.starts_with(prefix) && matches!(a.as_bytes().get(prefix.len()), Some(b'.' | b'[')))
}
