//! Turns a Maya graph into a scene: the DAG's transforms become nodes,
//! mesh shapes become meshes with their shading groups' materials.
//!
//! A mesh shape's geometry is what the file stores for it; failing that,
//! the builder walks up its construction history through nodes that don't
//! change the topology (group parts, tweaks, deformers in their rest pose)
//! to a stored mesh (usually the hidden "Orig" shape) or to a primitive's
//! creation node, which is rebuilt.  Meshes whose history needs Maya to
//! evaluate (extrusions, bevels, booleans) are listed and left out.

use crate::graph::{ComponentKind, Components, Graph, Value};
use crate::material::Materials;
use crate::mesh::{MeshData, to_poly};
use crate::prims;
use glam::{Mat3, Mat4, Quat, Vec3};
use slerp_polymesh::textures::TextureSet;
use slerp_polymesh::{PolyMesh, Wants, corner_normals, primitives};
use slerp_scene::{Limits, LoadError, Mesh, Node as SceneNode, Scene, Transform};
use std::collections::HashMap;
use std::path::Path;

/// Most scene nodes made: instancing loops and absurd hierarchies stop
/// here.
const MAX_NODES: usize = 1_000_000;
/// Deepest DAG nesting followed.
const MAX_DEPTH: usize = 512;
/// Most history nodes walked up from one mesh.
const MAX_HISTORY: usize = 64;

/// Node types that don't change a mesh's topology, so their input's
/// geometry is a fair stand-in for their output (deformers show the rest
/// pose).
const PASS_THROUGH: &[&str] = &[
    "groupParts",
    "polyTweak",
    "tweak",
    "skinCluster",
    "blendShape",
    "cluster",
    "ffd",
    "nonLinear",
    "wire",
    "sculpt",
    "softMod",
    "deltaMush",
    "tension",
    "polyColorPerVertex",
    "polyTweakUV",
    "polySoftEdge",
    "polyNormal",
    "polyNormalPerVertex",
    "polyMapCut",
    "polyMapSew",
    "polyAutoProj",
    "polyPlanarProj",
    "polyCylProj",
    "polySphProj",
    "polyMergeUV",
    "polyLayoutUV",
    "transformGeometry",
    "polyMoveVertex",
    "polyMoveUV",
    "proximityWrap",
    "jiggle",
];

/// What a mesh shape's geometry turned out to be.
enum Geometry {
    /// Stored points, with the skin clusters to pose them through (the
    /// one nearest the stored mesh first).
    Stored(MeshData, Vec<usize>),
    Primitive(PolyMesh, String),
    /// Only history Slerp can't evaluate: the node types in the way.
    History(Vec<String>),
    None,
}

/// Counts for the inspector.
#[derive(Default)]
pub struct Report {
    pub meshes: usize,
    pub primitives: Vec<String>,
    /// Shapes shown before their deformers or edits ("pSphereShape1:
    /// blendShape").
    pub rest_pose: Vec<String>,
    /// Skinned shapes posed as their joints stand.
    pub posed: Vec<String>,
    /// Shapes left out: name and the history in the way.
    pub history_only: Vec<String>,
    pub broken_faces: usize,
    pub other_shapes: HashMap<String, usize>,
    pub materials: usize,
}

pub fn scene(g: &Graph, path: Option<&Path>, limits: &Limits) -> Result<(Scene, Report), LoadError> {
    let mut b = Builder {
        g,
        limits,
        scene: Scene::default(),
        report: Report::default(),
        materials: Materials::new(g, TextureSet::new(path, limits)),
        meshes: HashMap::new(),
        children: vec![Vec::new(); g.nodes.len()],
        triangles: 0,
        worlds: HashMap::new(),
        time: crate::anim::current_time(g),
    };
    for (i, n) in g.nodes.iter().enumerate() {
        for &p in &n.parents {
            if p < g.nodes.len() && p != i {
                b.children[p].push(i);
            }
        }
    }
    // 1. The DAG from its roots: transforms without parents.
    let mut stack = Vec::new();
    for (i, n) in g.nodes.iter().enumerate() {
        if n.parents.is_empty()
            && is_transform(g, &b.children, i)
            && let Some(s) = b.visit(i, &mut stack)?
        {
            b.scene.roots.push(s);
        }
    }
    // 2. Units and up axis on one root.
    let metres = if g.metres.is_finite() && g.metres > 0.0 { g.metres as f32 } else { 0.01 };
    let mut root = Transform { scale: Vec3::splat(metres), ..Transform::IDENTITY };
    if g.z_up {
        root.rotation = Quat::from_rotation_x(-std::f32::consts::FRAC_PI_2);
    }
    if !b.scene.roots.is_empty() {
        b.scene.wrap_roots(root);
    }
    let mut scene = b.scene;
    scene.materials = std::mem::take(&mut b.materials.list);
    scene.textures = std::mem::take(&mut b.materials.textures.textures);
    scene.info.warnings.append(&mut b.materials.textures.warnings);
    b.report.materials = scene.materials.len();
    Ok((scene, b.report))
}

/// Whether node `i` is a DAG transform: a transform-like type, or a node
/// other nodes are parented under.
fn is_transform(g: &Graph, children: &[Vec<usize>], i: usize) -> bool {
    matches!(g.nodes[i].kind.as_str(), "transform" | "joint" | "lodGroup" | "ikHandle" | "XFRM" | "JOIN")
        || !children[i].is_empty()
}

struct Builder<'a> {
    g: &'a Graph,
    limits: &'a Limits,
    scene: Scene,
    report: Report,
    materials: Materials<'a>,
    /// Scene mesh of each Maya mesh shape, `None` when it couldn't be built.
    meshes: HashMap<usize, Option<usize>>,
    children: Vec<Vec<usize>>,
    triangles: u64,
    /// World matrices already worked out.
    worlds: HashMap<usize, Mat4>,
    /// The scene's current time, which animated channels are read at.
    time: Option<f64>,
}

impl Builder<'_> {
    /// Makes the scene node for transform `t` and everything under it;
    /// `None` when nothing under it draws.  `stack` holds the transforms
    /// being visited, so a parenting loop ends.
    fn visit(&mut self, t: usize, stack: &mut Vec<usize>) -> Result<Option<usize>, LoadError> {
        if stack.contains(&t) || stack.len() > MAX_DEPTH || self.scene.nodes.len() >= MAX_NODES {
            return Ok(None);
        }
        stack.push(t);
        let g = self.g;
        let node = &g.nodes[t];
        let mut s = SceneNode::new(node.name.clone());
        s.transform = Transform::from_matrix(self.local_matrix(t));
        s.visible = shown(g, t);
        let index = self.scene.nodes.len();
        self.scene.nodes.push(s);
        let kids = self.children[t].clone();
        let mut children = Vec::new();
        let mut meshes = Vec::new();
        for c in kids {
            if is_transform(g, &self.children, c) {
                if let Some(child) = self.visit(c, stack)? {
                    children.push(child);
                }
                continue;
            }
            let shape = &g.nodes[c];
            if shape.kind == "mesh" || shape.kind == "DMSH" {
                if shape.num("io").is_some_and(|v| v != 0.0) {
                    continue;
                }
                if let Some(m) = self.mesh(c)? {
                    meshes.push((m, shown(g, c)));
                }
            } else if !shape.kind.is_empty() {
                *self.report.other_shapes.entry(shape.kind.clone()).or_default() += 1;
            }
        }
        stack.pop();
        // One mesh rides on the transform's node; more get child nodes.
        match meshes[..] {
            [] => {}
            [(m, visible)] => {
                self.scene.nodes[index].mesh = Some(m);
                self.scene.nodes[index].visible &= visible;
            }
            _ => {
                for (m, visible) in meshes {
                    let mut n = SceneNode::new(self.scene.meshes[m].name.clone());
                    n.mesh = Some(m);
                    n.visible = visible;
                    self.scene.nodes.push(n);
                    children.push(self.scene.nodes.len() - 1);
                }
            }
        }
        if children.is_empty() && self.scene.nodes[index].mesh.is_none() {
            // Nothing to draw under it: drop the node (it is the last one
            // pushed, since its children would have been pushed after it).
            if self.scene.nodes.len() == index + 1 {
                self.scene.nodes.pop();
            }
            return Ok(None);
        }
        self.scene.nodes[index].children = children;
        Ok(Some(index))
    }

    /// A transform's matrix as Maya composes it: scale and shear about the
    /// scale pivot, rotation (in its order, after the rotate axis) about
    /// the rotate pivot, then translation; joints add their orientation.
    fn local_matrix(&self, t: usize) -> Mat4 {
        let g = self.g;
        let n = &g.nodes[t];
        let angle = |v: f64| if g.degrees { (v as f32).to_radians() } else { v as f32 };
        // Stored values, or the value of the curve animating a channel.
        let channels = |k: &str, parts: [&str; 3], d: f64| {
            let mut v = n.vec3(k, parts, [d; 3]);
            for (o, p) in v.iter_mut().zip(parts) {
                if let Some(x) = crate::anim::driven(g, t, p, self.time) {
                    *o = x;
                }
            }
            v
        };
        let v3 = |k: &str, parts: [&str; 3], d: f64| {
            let v = channels(k, parts, d);
            Vec3::new(v[0] as f32, v[1] as f32, v[2] as f32)
        };
        let a3 = |k: &str, parts: [&str; 3]| {
            let v = channels(k, parts, 0.0);
            Vec3::new(angle(v[0]), angle(v[1]), angle(v[2]))
        };
        let tr = v3("t", ["tx", "ty", "tz"], 0.0);
        let r = a3("r", ["rx", "ry", "rz"]);
        let s = v3("s", ["sx", "sy", "sz"], 1.0);
        let sh = v3("sh", ["shxy", "shxz", "shyz"], 0.0);
        let rp = v3("rp", ["rpx", "rpy", "rpz"], 0.0);
        let rpt = v3("rpt", ["rptx", "rpty", "rptz"], 0.0);
        let sp = v3("sp", ["spx", "spy", "spz"], 0.0);
        let spt = v3("spt", ["sptx", "spty", "sptz"], 0.0);
        let ra = a3("ra", ["rax", "ray", "raz"]);
        let order = n.num("ro").unwrap_or(0.0) as i32;
        let rot = rotation(r, order);
        let shear = Mat4::from_mat3(Mat3::from_cols(Vec3::X, Vec3::new(sh.x, 1.0, 0.0), Vec3::new(sh.y, sh.z, 1.0)));
        let m = if n.kind == "joint" || n.kind == "JOIN" {
            let jo = a3("jo", ["jox", "joy", "joz"]);
            Mat4::from_translation(tr)
                * Mat4::from_quat(rotation(jo, 0))
                * Mat4::from_quat(rot)
                * Mat4::from_quat(rotation(ra, 0))
                * Mat4::from_scale(s)
        } else {
            Mat4::from_translation(tr + rpt + rp)
                * Mat4::from_quat(rot * rotation(ra, 0))
                * Mat4::from_translation(-rp + spt + sp)
                * shear
                * Mat4::from_scale(s)
                * Mat4::from_translation(-sp)
        };
        if m.is_finite() { m } else { Mat4::IDENTITY }
    }

    /// The scene mesh of Maya mesh shape `shape`, built once.
    fn mesh(&mut self, shape: usize) -> Result<Option<usize>, LoadError> {
        if let Some(&m) = self.meshes.get(&shape) {
            return Ok(m);
        }
        let built = self.build_mesh(shape)?;
        self.meshes.insert(shape, built);
        Ok(built)
    }

    fn build_mesh(&mut self, shape: usize) -> Result<Option<usize>, LoadError> {
        let g = self.g;
        let node = &g.nodes[shape];
        // 1. Geometry, and the material groups of its faces.
        let (groups, whole) = self.assignments(shape);
        let slot_of = |f: usize| -> u32 {
            for (k, (list, _)) in groups.iter().enumerate() {
                if covers(list, f as u32) {
                    return k as u32 + 1;
                }
            }
            0
        };
        let current_uv = node.text("cuvs");
        let colors = node.num("dcol").is_some_and(|v| v != 0.0);
        let (mut poly, uv) = match self.geometry(shape) {
            Geometry::Stored(mut data, skins) => {
                if !skins.is_empty() {
                    self.pose(shape, &skins, &mut data);
                }
                let tris = data.triangle_estimate() as u64;
                self.check_triangles(tris)?;
                let c = to_poly(&data, &slot_of, current_uv, colors);
                self.report.broken_faces += c.broken;
                (c.poly, c.current_uv)
            }
            Geometry::Primitive(p, kind) => {
                self.check_triangles(p.triangle_count() as u64)?;
                self.report.primitives.push(format!("{} ({kind})", node.name));
                // A primitive's faces aren't numbered as Maya numbers them,
                // so per-face assignments go to the first group.
                let mut p = p;
                let slot = if whole.is_some() || groups.is_empty() { 0 } else { 1 };
                p.face_material.iter_mut().for_each(|m| *m = slot);
                (p, Some(0))
            }
            Geometry::History(chain) => {
                self.report.history_only.push(format!("{} ({})", node.name, chain.join(", ")));
                return Ok(None);
            }
            Geometry::None => return Ok(None),
        };
        poly.validate();
        if poly.face_count() == 0 {
            return Ok(None);
        }
        // 2. Materials per slot: 0 is the whole-shape group (or Maya's
        //    default), then each per-face group.
        let mut slot_material = vec![self.materials.for_group(whole)];
        for (_, sg) in &groups {
            slot_material.push(self.materials.for_group(Some(*sg)));
        }
        // With Display Colors on, Maya shows the vertex colours in place
        // of the material's colour (unless the channel is "None").
        if !poly.colors.is_empty() && node.text("dcc").is_none_or(|c| c != "None") {
            for m in &mut slot_material {
                *m = self.materials.vertex_colored(*m);
            }
        }
        // 3. Triangles.
        let normals = corner_normals(&poly);
        let color = (!poly.colors.is_empty()).then_some(0);
        let prims = primitives(&poly, &normals, &|_| Wants { uv: [uv, None], color });
        let mut mesh = Mesh { name: node.name.clone(), ..Default::default() };
        for (slot, mut p) in prims {
            p.material = slot_material.get(slot).copied();
            mesh.primitives.push(p);
        }
        if mesh.primitives.is_empty() {
            return Ok(None);
        }
        self.report.meshes += 1;
        self.scene.meshes.push(mesh);
        Ok(Some(self.scene.meshes.len() - 1))
    }

    /// Poses a skinned mesh's points as its joints stand.  Only in files
    /// whose values are centimetres, Maya's internal unit, which the
    /// matrices are kept in; elsewhere the bind pose stays.
    fn pose(&mut self, shape: usize, skins: &[usize], data: &mut MeshData) {
        let g = self.g;
        if (g.metres - 0.01).abs() > 1e-9 {
            self.report.rest_pose.push(format!("{}: skinCluster (bind pose)", g.nodes[shape].name));
            return;
        }
        let mesh_world = g.nodes[shape].parents.first().and_then(|&p| self.world(p, 0)).unwrap_or(Mat4::IDENTITY);
        let bind = data.clone();
        for &skin in skins {
            // Joints that constraints or expressions move have no stored
            // pose: keep the bind pose then.
            let constrained =
                g.connections.iter().filter(|c| c.dst == skin && c.dst_attr.starts_with("ma[")).any(|c| {
                    let mut j = Some(c.src);
                    let mut steps = 0;
                    while let Some(n) = j {
                        if crate::anim::driven_otherwise(
                            g,
                            n,
                            &["t", "tx", "ty", "tz", "r", "rx", "ry", "rz", "s", "sx", "sy", "sz", "jo"],
                        ) {
                            return true;
                        }
                        steps += 1;
                        j = g.nodes[n].parents.first().copied().filter(|_| steps < MAX_DEPTH);
                    }
                    false
                });
            let mut world = |n: usize| self.world(n, 0);
            if constrained
                || !crate::skin::apply(g, skin, &mut world, mesh_world, data)
                || !crate::skin::plausible(&bind, data)
            {
                *data = bind;
                self.report.rest_pose.push(format!("{}: skinCluster (bind pose)", g.nodes[shape].name));
                return;
            }
        }
        self.report.posed.push(g.nodes[shape].name.clone());
    }

    /// A DAG node's world matrix, in the file's units.
    fn world(&mut self, n: usize, depth: usize) -> Option<Mat4> {
        if depth > MAX_DEPTH || n >= self.g.nodes.len() {
            return None;
        }
        if let Some(&m) = self.worlds.get(&n) {
            return Some(m);
        }
        let local = self.local_matrix(n);
        let m = match self.g.nodes[n].parents.first() {
            Some(&p) if p != n => self.world(p, depth + 1)? * local,
            _ => local,
        };
        self.worlds.insert(n, m);
        Some(m)
    }

    fn check_triangles(&mut self, more: u64) -> Result<(), LoadError> {
        self.triangles = self.triangles.saturating_add(more);
        if self.triangles > self.limits.max_triangles {
            return Err(LoadError::TooLarge(format!("more than {} triangles", self.limits.max_triangles)));
        }
        Ok(())
    }

    /// Finds a mesh shape's geometry: stored in it, or up its history.
    fn geometry(&mut self, shape: usize) -> Geometry {
        let g = self.g;
        if let Some(data) = MeshData::from_node(&g.nodes[shape]) {
            return Geometry::Stored(data, Vec::new());
        }
        let mut passed: Vec<String> = Vec::new();
        let mut tweaks: Vec<usize> = Vec::new();
        let mut skins: Vec<usize> = Vec::new();
        let mut node = shape;
        let mut seen = vec![shape];
        for _ in 0..MAX_HISTORY {
            // The connection feeding this node's geometry.
            let input = ["i", "ip", "ig", "ip[0].ig", "inputPolymesh", "inputGeometry"]
                .iter()
                .find_map(|a| g.connections.iter().find(|c| c.dst == node && c.dst_attr == *a))
                .or_else(|| {
                    g.connections
                        .iter()
                        .find(|c| c.dst == node && c.dst_attr.starts_with("ip[") && c.dst_attr.ends_with(".ig"))
                });
            let Some(c) = input else {
                return if passed.is_empty() { Geometry::None } else { Geometry::History(passed) };
            };
            let src = c.src;
            if seen.contains(&src) {
                return Geometry::History(passed);
            }
            seen.push(src);
            let kind = g.nodes[src].kind.as_str();
            if kind == "mesh" || kind == "DMSH" {
                if let Some(mut data) = MeshData::from_node(&g.nodes[src]) {
                    apply_tweaks(g, &tweaks, &mut data);
                    let others: Vec<String> =
                        dedup(&passed).into_iter().filter(|k| k != "skinCluster" || skins.is_empty()).collect();
                    if !others.is_empty() {
                        self.report.rest_pose.push(format!("{}: {}", g.nodes[shape].name, others.join(", ")));
                    }
                    skins.reverse();
                    return Geometry::Stored(data, skins);
                }
                node = src;
                continue;
            }
            if let Some(p) = prims::build(&g.nodes[src]) {
                if !passed.is_empty() {
                    self.report.rest_pose.push(format!("{}: {}", g.nodes[shape].name, dedup(&passed).join(", ")));
                }
                return Geometry::Primitive(p, kind.to_string());
            }
            if PASS_THROUGH.contains(&kind) {
                if kind == "polyTweak" {
                    tweaks.push(src);
                }
                if kind == "skinCluster" {
                    skins.push(src);
                }
                if kind != "groupParts" {
                    passed.push(kind.to_string());
                }
                node = src;
                continue;
            }
            passed.push(kind.to_string());
            return Geometry::History(dedup(&passed));
        }
        Geometry::History(passed)
    }

    /// The shading groups of a mesh shape's first instance: per face group
    /// (component lists and the group's shading group), and the one for
    /// the whole shape.
    fn assignments(&self, shape: usize) -> (Vec<(Vec<Components>, usize)>, Option<usize>) {
        let g = self.g;
        let mut whole = None;
        let mut groups = Vec::new();
        for c in
            g.connections.iter().filter(|c| c.src == shape && (c.dst_attr == "dsm" || c.dst_attr.starts_with("dsm[")))
        {
            let sg = c.dst;
            let a = c.src_attr.as_str();
            if a == "iog" || a == "iog[0]" {
                whole.get_or_insert(sg);
            } else if let Some(rest) = a.strip_prefix("iog[0].og[").or_else(|| a.strip_prefix("iog.og[")) {
                let Some(k) = rest.strip_suffix(']') else { continue };
                if let Some(list) = self.group_faces(shape, k) {
                    groups.push((list, sg));
                }
            }
        }
        (groups, whole)
    }

    /// The faces of object group `k` of a shape: its own component list,
    /// or the one of the groupParts its group id goes through.
    fn group_faces(&self, shape: usize, k: &str) -> Option<Vec<Components>> {
        let g = self.g;
        if let Some(Value::Components(list)) = g.nodes[shape].value(&format!("iog[0].og[{k}].gcl")) {
            return Some(list.clone());
        }
        let gid = g.input(shape, &format!("iog[0].og[{k}].gid"))?.src;
        g.outputs(gid, "id").find_map(|c| {
            let parts = &g.nodes[c.dst];
            (parts.kind == "groupParts" || parts.kind == "GRPP").then(|| match parts.value("ic") {
                Some(Value::Components(list)) => Some(list.clone()),
                _ => None,
            })?
        })
    }
}

/// Whether a DAG node is shown: its own visibility, and that of the
/// display layer that overrides its drawing.
fn shown(g: &Graph, n: usize) -> bool {
    let own = g.nodes[n].num("v").is_none_or(|v| v != 0.0);
    let layer =
        g.input(n, "do").map(|c| c.src).filter(|&l| matches!(g.nodes[l].kind.as_str(), "displayLayer" | "DSPL"));
    own && layer.is_none_or(|l| g.nodes[l].num("v").is_none_or(|v| v != 0.0))
}

/// Adds the per-vertex offsets of `polyTweak` nodes.
fn apply_tweaks(g: &Graph, tweaks: &[usize], data: &mut MeshData) {
    for &t in tweaks {
        if let Some(tk) = g.nodes[t].dense("tk", 3, crate::mesh::MAX_ELEMENTS) {
            for (p, d) in data.positions.iter_mut().zip(tk.chunks_exact(3)) {
                let d = Vec3::new(d[0] as f32, d[1] as f32, d[2] as f32);
                if d.is_finite() {
                    *p += d;
                }
            }
        }
    }
}

fn dedup(list: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for s in list {
        if !out.contains(s) {
            out.push(s.clone());
        }
    }
    out
}

/// Whether a component list covers face `f`.
fn covers(list: &[Components], f: u32) -> bool {
    list.iter().filter(|c| c.kind == ComponentKind::Face).any(|c| {
        c.ranges.iter().any(|r| match r {
            None => true,
            Some((a, b)) => (*a..=*b).contains(&f),
        })
    })
}

/// A rotation from Euler angles in one of Maya's six orders (0 xyz, 1 yzx,
/// 2 zxy, 3 xzy, 4 yxz, 5 zyx), the first axis applied first.
fn rotation(r: Vec3, order: i32) -> Quat {
    let (x, y, z) = (Quat::from_rotation_x(r.x), Quat::from_rotation_y(r.y), Quat::from_rotation_z(r.z));
    match order {
        1 => x * z * y,
        2 => y * x * z,
        3 => y * z * x,
        4 => z * x * y,
        5 => x * y * z,
        _ => z * y * x,
    }
}
