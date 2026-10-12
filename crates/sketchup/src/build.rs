//! Turning a [`Model`] into a scene the renderer draws, the way SketchUp
//! shows it: two-sided faces with their front and back materials, shading
//! blended across smooth edges, edges drawn as black lines unless soft or
//! hidden, hidden things and things on hidden tags kept but switched off,
//! and each component's mesh built once and placed by nodes.
//!
//! Lengths become metres here (SketchUp's unit is the inch), and the
//! model turns from Z-up to Y-up at its root node.

use crate::model::*;
use crate::triangulate;
use glam::{DMat3, DMat4, DVec3, Vec2, Vec3, Vec4};
use slerp_scene::textures::{DecodeOptions, Pixels, Texture};
use slerp_scene::{
    AlphaMode, Limits, Material as SceneMaterial, Mesh, Node, Primitive, Scene, TexRef, Topology, Transform,
};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

/// Metres per inch.
pub const INCH: f64 = 0.0254;

/// How deep groups and components may nest.
const MAX_NESTING: usize = 64;

/// The most nodes (placed groups and components) a scene gets, for the
/// triangles `limits` allow: about 300,000 for thumbnails and a million in
/// the viewer, since nested components can multiply into billions of
/// placements, empty ones included.
fn max_nodes(limits: &Limits) -> usize {
    (limits.max_triangles / 64).clamp(50_000, 1_000_000) as usize
}

/// How far edges are lifted off their faces, as a fraction of the size of
/// their component, so the depth test draws them over the faces they
/// bound rather than flickering in and out.
const EDGE_LIFT: f64 = 3e-4;

/// SketchUp's texture coordinates run up from the picture's bottom edge;
/// the renderer's run down from its top.
const FLIP_V: bool = true;

/*┌─────────────────╮
| Builder         |
└─────────────────╯*/
//region Builder

/// Which side of a face a material is for.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum Side {
    Front,
    Back,
}

/// A face side's paint: one of the model's materials, or the style's
/// colour for faces without one.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum Paint {
    Material(u32),
    Default(Side),
}

/// What became of the faces, for the warnings.
#[derive(Default)]
struct Tally {
    faces: usize,
    degenerate: usize,
    open_loops: usize,
    non_planar: usize,
}

/// A definition's meshes: what shows, and what is hidden (hidden faces and
/// edges, and those on hidden tags), with the triangles they hold.
#[derive(Clone, Copy, Default)]
struct Built {
    shown: Option<usize>,
    hidden: Option<usize>,
    triangles: u64,
}

struct Builder<'m> {
    model: &'m Model,
    limits: &'m Limits,
    scene: Scene,
    materials: HashMap<(Paint, bool), usize>,
    textures: HashMap<u32, Option<(usize, bool)>>,
    edge_material: Option<usize>,
    meshes: HashMap<(usize, Option<u32>), Built>,
    counted: HashSet<usize>,
    tally: Tally,
    warnings: Vec<String>,
    placed_triangles: u64,
    placements: usize,
    over_budget: bool,
    cycles: usize,
}

/// The scene of `model`, under `limits`.
pub(crate) fn scene(model: &Model, limits: &Limits) -> Scene {
    let mut b = Builder {
        model,
        limits,
        scene: Scene::default(),
        materials: HashMap::new(),
        textures: HashMap::new(),
        edge_material: None,
        meshes: HashMap::new(),
        counted: HashSet::new(),
        tally: Tally::default(),
        warnings: Vec::new(),
        placed_triangles: 0,
        placements: 0,
        over_budget: false,
        cycles: 0,
    };
    // 1. The model's own entities on a root node, then every placement
    //    below it.
    let root = model.definitions.len();
    b.scene.nodes.push(Node::new("SketchUp model"));
    b.attach(root, None, 0, None);
    let mut stack = Vec::new();
    b.place(root, None, 0, &mut stack, None, 0);
    b.scene.compute_roots();
    b.scene.convert_z_up();
    // 2. Problems worth saying.
    b.warn();
    let mut scene = b.scene;
    scene.info.warnings.extend(b.warnings);
    scene.info.details.push(("Placed groups and components".into(), b.placements.to_string()));
    scene
}

//endregion

/*┌─────────────────╮
| Placement       |
└─────────────────╯*/
//region Placement

impl Builder<'_> {
    fn hidden_layer(&self, layer: Option<u32>) -> bool {
        layer.and_then(|l| self.model.layers.get(l as usize)).is_some_and(|l| l.hidden)
    }

    /// Gives node `node` the meshes of definition `def` painted with
    /// `paint`: the shown one as the node's mesh, the hidden one on a
    /// switched-off child.  `bake` is a placement to build into the
    /// vertices, for placements a node's transform can't hold (shear).
    fn attach(&mut self, def: usize, paint: Option<u32>, node: usize, bake: Option<DMat4>) {
        let built = match bake {
            Some(m) => self.build(def, paint, Some(m)),
            None => match self.meshes.get(&(def, paint)) {
                Some(b) => *b,
                None => {
                    let b = self.build(def, paint, None);
                    self.meshes.insert((def, paint), b);
                    b
                }
            },
        };
        if self.placed_triangles.saturating_add(built.triangles) > self.limits.max_triangles {
            self.over_budget = true;
            return;
        }
        self.placed_triangles += built.triangles;
        self.scene.nodes[node].mesh = built.shown;
        if let Some(h) = built.hidden {
            let mut n = Node::new("Hidden geometry");
            n.mesh = Some(h);
            n.visible = false;
            self.scene.nodes.push(n);
            let i = self.scene.nodes.len() - 1;
            self.scene.nodes[node].children.push(i);
        }
    }

    /// Adds a node under `parent` for each group and component placed in
    /// definition `def`, recursively.  `stack` holds the definitions being
    /// placed, which stops components that contain themselves; `bake` is
    /// set inside a sheared placement, whose contents are built in place.
    fn place(
        &mut self,
        def: usize,
        paint: Option<u32>,
        depth: usize,
        stack: &mut Vec<usize>,
        bake: Option<DMat4>,
        parent: usize,
    ) {
        let model = self.model;
        let Some(d) = model.definition(def) else { return };
        for inst in &d.instances {
            if self.scene.nodes.len() >= max_nodes(self.limits) || self.over_budget {
                self.over_budget = true;
                return;
            }
            let child = inst.definition as usize;
            let Some(cd) = model.definitions.get(child) else { continue };
            if stack.contains(&child) || depth + 1 >= MAX_NESTING {
                self.cycles += 1;
                continue;
            }
            let m = placement(&inst.matrix);
            // Faces without a material take the nearest painted group's.
            let paint = inst.material.or(paint);
            let name =
                [&inst.name, &cd.name].into_iter().find(|n| !n.is_empty()).cloned().unwrap_or_else(|| "Group".into());
            let mut node = Node::new(name);
            node.visible = !inst.hidden && !self.hidden_layer(inst.layer);
            let child_bake = match bake {
                Some(outer) => Some(outer * m),
                None => match node_transform(m) {
                    Some(t) => {
                        node.transform = t;
                        None
                    }
                    None => Some(m),
                },
            };
            self.scene.nodes.push(node);
            let ni = self.scene.nodes.len() - 1;
            self.scene.nodes[parent].children.push(ni);
            self.placements += 1;
            stack.push(child);
            self.attach(child, paint, ni, child_bake);
            self.place(child, paint, depth + 1, stack, child_bake, ni);
            stack.pop();
        }
    }
}

/// An instance's placement as a matrix in metres.  SketchUp applies the
/// stored 3 by 3 part row by row (`x' = m0 x + m1 y + m2 z + m9`) and divides
/// by the thirteenth value, a homogeneous scale.
fn placement(m: &[f64; 13]) -> DMat4 {
    let w = if m[12].is_finite() && m[12].abs() > 1e-12 { m[12] } else { 1.0 };
    let linear =
        DMat3::from_cols(DVec3::new(m[0], m[3], m[6]), DVec3::new(m[1], m[4], m[7]), DVec3::new(m[2], m[5], m[8]))
            * (1.0 / w);
    let t = DVec3::new(m[9], m[10], m[11]) * (INCH / w);
    let out = DMat4::from_mat3_translation(linear, t);
    if out.is_finite() { out } else { DMat4::IDENTITY }
}

/// The node transform for a placement, or `None` when translation,
/// rotation and scale can't express it (shear).
fn node_transform(m: DMat4) -> Option<Transform> {
    let (scale, rotation, translation) = m.to_scale_rotation_translation();
    let back = DMat4::from_scale_rotation_translation(scale, rotation, translation);
    let size = m.to_cols_array().iter().fold(1.0f64, |a, &b| a.max(b.abs()));
    if !back.abs_diff_eq(m, 1e-6 * size) || scale.min_element().abs() < 1e-12 {
        return None;
    }
    Some(Transform { translation: translation.as_vec3(), rotation: rotation.as_quat(), scale: scale.as_vec3() })
}

//endregion

/*┌─────────────────╮
| Meshes          |
└─────────────────╯*/
//region Meshes

/// A primitive being filled, with its vertices by what they hold.
#[derive(Default)]
struct Filling {
    prim: Primitive,
    seen: HashMap<(u32, u32, u64), u32>,
}

impl Filling {
    fn vertex(&mut self, key: (u32, u32, u64), p: Vec3, n: Vec3, uv: Option<Vec2>) -> u32 {
        if let Some(&i) = self.seen.get(&key) {
            return i;
        }
        let i = self.prim.positions.len() as u32;
        self.prim.positions.push(p);
        self.prim.normals.push(n);
        if let Some(uv) = uv {
            self.prim.uv0.push(uv);
        }
        self.seen.insert(key, i);
        i
    }
}

/// A face ready for triangles: its rings of vertices and its normal.
struct Prepared {
    rings: Vec<Vec<u32>>,
    normal: DVec3,
}

/// Faces joined by smooth edges share their shading: the faces of each
/// group of faces joined that way, found with a union-find.
fn smooth_groups(d: &Definition, prepared: &[Option<Prepared>]) -> Vec<u32> {
    let n = d.faces.len();
    let mut parent: Vec<u32> = (0..n as u32).collect();
    fn root(parent: &mut [u32], mut i: u32) -> u32 {
        while parent[i as usize] != i {
            parent[i as usize] = parent[parent[i as usize] as usize];
            i = parent[i as usize];
        }
        i
    }
    let mut first_face: HashMap<u32, u32> = HashMap::new();
    for (f, face) in d.faces.iter().enumerate() {
        if prepared[f].is_none() {
            continue;
        }
        for c in face.loops.iter().flatten() {
            if !d.edges.get(c.edge as usize).is_some_and(|e| e.smooth) {
                continue;
            }
            match first_face.get(&c.edge) {
                Some(&g) => {
                    let (a, b) = (root(&mut parent, g), root(&mut parent, f as u32));
                    parent[a as usize] = b;
                }
                None => {
                    first_face.insert(c.edge, f as u32);
                }
            }
        }
    }
    (0..n as u32).map(|i| root(&mut parent, i)).collect()
}

/// A face's rings of vertex indices, following its coedges, and whether
/// every ring closed.
fn rings(d: &Definition, face: &Face) -> (Vec<Vec<u32>>, bool) {
    let mut out = Vec::new();
    let mut closed = true;
    for l in &face.loops {
        let mut ring: Vec<u32> = Vec::with_capacity(l.len());
        let mut last_end = None;
        for c in l {
            let Some(e) = d.edges.get(c.edge as usize) else { continue };
            let (from, to) = if c.reversed { (e.b, e.a) } else { (e.a, e.b) };
            if last_end.is_some_and(|end| end != from) {
                closed = false;
            }
            if ring.last() != Some(&from) {
                ring.push(from);
            }
            last_end = Some(to);
        }
        if let (Some(&first), Some(end)) = (ring.first(), last_end)
            && first != end
        {
            closed = false;
        }
        if ring.len() > 1 && ring.first() == ring.last() {
            ring.pop();
        }
        if ring.len() >= 3 {
            out.push(ring);
        }
    }
    (out, closed)
}

/// The texture coordinates of point `p` (inches, in the definition) on a
/// face with plane axes `xr`, `yr`, as OpenSKP worked them out against
/// the SDK: the point in the face's plane, through the inverse of a
/// positioned texture's matrix, divided by the tile size.
fn face_uv(p: DVec3, xr: DVec3, yr: DVec3, mapping: Option<&Mapping>, tile: (f64, f64)) -> Vec2 {
    let (px, py) = (p.dot(xr), p.dot(yr));
    let (u, v) = match mapping.and_then(|m| invert3(&m.matrix)) {
        Some(inv) => {
            let u = px * inv[0] + py * inv[3] + inv[6];
            let v = px * inv[1] + py * inv[4] + inv[7];
            let q = px * inv[2] + py * inv[5] + inv[8];
            let q = if q.abs() < 1e-12 { 1.0 } else { q };
            (u / q, v / q)
        }
        None => (px, py),
    };
    let (u, v) = (u / tile.0, v / tile.1);
    let v = if FLIP_V { -v } else { v };
    if u.is_finite() && v.is_finite() { Vec2::new(u as f32, v as f32) } else { Vec2::ZERO }
}

/// SketchUp's plane axes for texture coordinates: `Z x n` and `n x` that,
/// with the world axes for faces within a thousandth of horizontal (and
/// turned half way round for faces looking down).
fn uv_axes(n: DVec3) -> (DVec3, DVec3) {
    let c = DVec3::new(-n.y, n.x, 0.0);
    let len = c.length();
    if len < 1e-3 {
        return (if n.z >= 0.0 { DVec3::X } else { DVec3::NEG_X }, DVec3::Y);
    }
    let xr = c / len;
    (xr, n.cross(xr))
}

fn invert3(m: &[f64; 9]) -> Option<[f64; 9]> {
    let [a, b, c, d, e, f, g, h, i] = *m;
    let det = a * (e * i - f * h) - b * (d * i - f * g) + c * (d * h - e * g);
    if det.abs() < 1e-12 || !det.is_finite() {
        return None;
    }
    let k = 1.0 / det;
    Some([
        (e * i - f * h) * k,
        (c * h - b * i) * k,
        (b * f - c * e) * k,
        (f * g - d * i) * k,
        (a * i - c * g) * k,
        (c * d - a * f) * k,
        (d * h - e * g) * k,
        (b * g - a * h) * k,
        (a * e - b * d) * k,
    ])
}

impl Builder<'_> {
    /// Builds definition `def`'s meshes with faces of the default material
    /// painted `paint`, optionally with a placement built in.
    fn build(&mut self, def: usize, paint: Option<u32>, bake: Option<DMat4>) -> Built {
        let model = self.model;
        let Some(d) = model.definition(def) else { return Built::default() };
        let first = self.counted.insert(def);
        let place = |p: DVec3| -> Vec3 {
            let m = p * INCH;
            bake.map_or(m, |b| b.transform_point3(m)).as_vec3()
        };
        let normal_matrix = bake.map(|b| DMat3::from_mat4(b).inverse().transpose());
        let turn = |n: DVec3| -> Vec3 { normal_matrix.map_or(n, |m| m * n).normalize_or_zero().as_vec3() };
        let mirrored = bake.is_some_and(|b| DMat3::from_mat4(b).determinant() < 0.0);

        // 1. Each face's rings, and the checks that catch a misread.
        let mut prepared: Vec<Option<Prepared>> = Vec::with_capacity(d.faces.len());
        for face in &d.faces {
            let (rings, closed) = rings(d, face);
            if first {
                self.tally.faces += 1;
                self.tally.open_loops += usize::from(!closed);
            }
            let Some(outer) = rings.first() else {
                self.tally.degenerate += usize::from(first);
                prepared.push(None);
                continue;
            };
            let points: Vec<DVec3> = outer.iter().map(|&i| d.vertices[i as usize]).collect();
            let newell = triangulate::newell(&points);
            let stored = face.normal.normalize_or_zero();
            let normal = if stored.is_finite() && stored != DVec3::ZERO { stored } else { newell.normalize_or_zero() };
            if first {
                let size = points.iter().map(|p| (*p - points[0]).length()).fold(0.0, f64::max);
                let off = points.iter().map(|p| (*p - points[0]).dot(normal).abs()).fold(0.0, f64::max);
                self.tally.non_planar += usize::from(off > size * 0.01 + 1e-4);
            }
            prepared.push(Some(Prepared { rings, normal }));
        }
        let groups = smooth_groups(d, &prepared);
        // Shading normals per vertex and smoothing group, each face's normal
        // turned to agree with the group's first.
        let mut smooth: HashMap<(u32, u32), DVec3> = HashMap::new();
        for (f, p) in prepared.iter().enumerate() {
            let Some(p) = p else { continue };
            if groups[f] == f as u32
                && !d.faces[f].loops.iter().flatten().any(|c| d.edges.get(c.edge as usize).is_some_and(|e| e.smooth))
            {
                continue;
            }
            for &v in p.rings.iter().flatten() {
                let sum = smooth.entry((v, groups[f])).or_insert(DVec3::ZERO);
                *sum += if sum.dot(p.normal) < 0.0 { -p.normal } else { p.normal };
            }
        }

        // 2. Faces to triangles, each side with its material.
        let mut fills: HashMap<(usize, bool), Filling> = HashMap::new();
        let mut order: Vec<(usize, bool)> = Vec::new();
        let mut tris: Vec<[u32; 3]> = Vec::new();
        let mut triangles = 0u64;
        for (f, face) in d.faces.iter().enumerate() {
            let Some(p) = &prepared[f] else { continue };
            tris.clear();
            if !triangulate::face(&d.vertices, &p.rings, p.normal, &mut tris) {
                self.tally.degenerate += usize::from(first);
                continue;
            }
            let hidden = face.hidden || self.hidden_layer(face.layer);
            let front = face.front.or(paint).map_or(Paint::Default(Side::Front), Paint::Material);
            let back = face.back.or(paint).map_or(Paint::Default(Side::Back), Paint::Material);
            let sides: &[(Side, Paint, bool)] = if front == back {
                &[(Side::Front, front, true)]
            } else {
                &[(Side::Front, front, false), (Side::Back, back, false)]
            };
            for &(side, paint, double) in sides {
                let mat = self.material(paint, double);
                let tile = match paint {
                    Paint::Material(m) => self.textures.get(&m).copied().flatten().and_then(|_| {
                        let t = model.materials.get(m as usize)?.texture.as_ref()?;
                        Some((
                            if t.width.abs() > 1e-9 { t.width } else { 1.0 },
                            if t.height.abs() > 1e-9 { t.height } else { 1.0 },
                        ))
                    }),
                    Paint::Default(_) => None,
                };
                let mapping = face.mapping[if side == Side::Front { 0 } else { 1 }].as_ref();
                // A projected texture runs in the plane across its
                // projection, the face's own texture in the face's plane.
                let (xr, yr) = uv_axes(mapping.and_then(|m| m.projection).unwrap_or(p.normal));
                let key = (mat, hidden);
                let fill = fills.entry(key).or_insert_with(|| {
                    order.push(key);
                    Filling::default()
                });
                let flip = (side == Side::Back) != mirrored;
                let sign = if side == Side::Back { -1.0 } else { 1.0 };
                let shading_id = 2 * if smooth.contains_key(&(p.rings[0][0], groups[f])) {
                    groups[f] + d.faces.len() as u32
                } else {
                    f as u32
                } + (side == Side::Back) as u32;
                for t in &tris {
                    let mut ids = [0u32; 3];
                    for (k, &v) in t.iter().enumerate() {
                        let pos = d.vertices[v as usize];
                        let n = smooth
                            .get(&(v, groups[f]))
                            .map_or(p.normal, |s| if s.dot(p.normal) < 0.0 { -*s } else { *s });
                        let uv = tile.map(|tile| face_uv(pos, xr, yr, mapping, tile));
                        let uv_key = uv.map_or(0, |uv| (uv.x.to_bits() as u64) << 32 | uv.y.to_bits() as u64);
                        ids[k] = fill.vertex((v, shading_id, uv_key), place(pos), turn(n * sign), uv);
                    }
                    if flip {
                        ids.swap(1, 2);
                    }
                    fill.prim.indices.extend(ids);
                }
                triangles += tris.len() as u64;
            }
        }

        // 3. Edges as lines, lifted off the faces they bound.
        let lines = self.lines(d, &prepared, &place);

        // 4. The meshes: shown and hidden.
        let mut parts: [Vec<Primitive>; 2] = [Vec::new(), Vec::new()];
        for key in order {
            if let Some(mut fill) = fills.remove(&key) {
                fill.prim.material = Some(key.0);
                parts[key.1 as usize].push(fill.prim);
            }
        }
        let edge_material = self.edge_material();
        for (hidden, mut prim) in lines.into_iter().enumerate() {
            if !prim.indices.is_empty() {
                prim.material = Some(edge_material);
                parts[hidden].push(prim);
            }
        }
        let name = if def == model.definitions.len() { "Model".to_string() } else { d.name.clone() };
        let [shown, hidden] = parts.map(|primitives| {
            (!primitives.is_empty()).then(|| {
                self.scene.meshes.push(Mesh { name: name.clone(), primitives, target_names: Vec::new() });
                self.scene.meshes.len() - 1
            })
        });
        Built { shown, hidden, triangles }
    }

    /// The definition's edges as two line primitives, shown and hidden.
    /// Soft edges aren't drawn.  Each edge is lifted a little along the
    /// normals of the faces it bounds; an edge with one face (or faces
    /// facing opposite ways) is drawn on both sides.
    fn lines(
        &mut self,
        d: &Definition,
        prepared: &[Option<Prepared>],
        place: &dyn Fn(DVec3) -> Vec3,
    ) -> [Primitive; 2] {
        let mut normals: Vec<(DVec3, u32)> = vec![(DVec3::ZERO, 0); d.edges.len()];
        let mut any: Vec<DVec3> = vec![DVec3::ZERO; d.edges.len()];
        for (f, face) in d.faces.iter().enumerate() {
            let Some(p) = &prepared[f] else { continue };
            for c in face.loops.iter().flatten() {
                if let Some(slot) = normals.get_mut(c.edge as usize) {
                    slot.0 += p.normal;
                    slot.1 += 1;
                    any[c.edge as usize] = p.normal;
                }
            }
        }
        let (mut lo, mut hi) = (DVec3::splat(f64::MAX), DVec3::splat(f64::MIN));
        for v in &d.vertices {
            lo = lo.min(*v);
            hi = hi.max(*v);
        }
        let lift = if lo.x <= hi.x { (hi - lo).length() * EDGE_LIFT } else { 0.0 };
        let mut out = [
            Primitive { topology: Topology::Lines, ..Default::default() },
            Primitive { topology: Topology::Lines, ..Default::default() },
        ];
        for (i, e) in d.edges.iter().enumerate() {
            if e.soft {
                continue;
            }
            let hidden = e.hidden || self.hidden_layer(e.layer);
            let (Some(&a), Some(&b)) = (d.vertices.get(e.a as usize), d.vertices.get(e.b as usize)) else { continue };
            let (sum, count) = normals[i];
            let offsets: Vec<DVec3> = if count == 0 {
                vec![DVec3::ZERO]
            } else if count >= 2 && sum.length() > 0.5 {
                vec![sum.normalize() * lift]
            } else {
                let n = any[i];
                vec![n * lift, -n * lift]
            };
            let prim = &mut out[hidden as usize];
            for o in offsets {
                let k = prim.positions.len() as u32;
                prim.positions.push(place(a + o));
                prim.positions.push(place(b + o));
                prim.indices.extend([k, k + 1]);
            }
        }
        out
    }
}

//endregion

/*┌─────────────────╮
| Materials       |
└─────────────────╯*/
//region Materials

fn linear(c: u8) -> f32 {
    let c = c as f32 / 255.0;
    if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
}

fn linear_rgb(c: [u8; 3]) -> Vec3 {
    Vec3::new(linear(c[0]), linear(c[1]), linear(c[2]))
}

impl Builder<'_> {
    /// The scene material for `paint`, two-sided or not.
    fn material(&mut self, paint: Paint, double: bool) -> usize {
        if let Some(&i) = self.materials.get(&(paint, double)) {
            return i;
        }
        let mut m = SceneMaterial { roughness: 0.9, metallic: 0.0, double_sided: double, ..SceneMaterial::default() };
        match paint {
            Paint::Default(side) => {
                let (name, color) = match side {
                    Side::Front => ("Default front", self.model.style.front),
                    Side::Back => ("Default back", self.model.style.back),
                };
                m.name = name.into();
                m.base_color = linear_rgb(color).extend(1.0);
            }
            Paint::Material(i) => {
                let source = &self.model.materials[i as usize];
                m.name = source.name.clone();
                let texture = self.texture(i);
                m.base_color = match texture {
                    Some(_) => Vec4::new(1.0, 1.0, 1.0, source.opacity),
                    None => linear_rgb(source.color).extend(source.opacity),
                };
                if let Some((t, _)) = texture {
                    m.base_color_texture = Some(TexRef::new(t));
                }
                m.alpha_mode = if source.opacity < 0.999 {
                    AlphaMode::Blend
                } else if texture.is_some_and(|(_, alpha)| alpha) {
                    // Cut-outs: trees, fences and people drawn on flat faces.
                    AlphaMode::Mask(0.5)
                } else {
                    AlphaMode::Opaque
                };
            }
        }
        self.scene.materials.push(m);
        let i = self.scene.materials.len() - 1;
        self.materials.insert((paint, double), i);
        i
    }

    /// The scene texture of material `i` and whether it has transparency,
    /// decoded once; a colourised material's picture is tinted here.
    fn texture(&mut self, i: u32) -> Option<(usize, bool)> {
        if let Some(t) = self.textures.get(&i) {
            return *t;
        }
        let source = &self.model.materials[i as usize];
        let made = source.texture.as_ref().and_then(|t| {
            let opts = match self.limits.max_texture_size {
                Some(n) => DecodeOptions::preview(n),
                None => DecodeOptions::full(),
            };
            match slerp_scene::textures::decode(&t.bytes, &opts) {
                Ok(tex) => Some(tex),
                Err(e) => {
                    self.warnings.push(format!("texture of {}: {e}", source.name));
                    None
                }
            }
        });
        let made = made.map(|mut tex| {
            if source.colorized {
                colorize(&mut tex, source.color);
            }
            let alpha = tex.has_alpha;
            self.scene.textures.push(Arc::new(tex));
            (self.scene.textures.len() - 1, alpha)
        });
        self.textures.insert(i, made);
        made
    }

    fn edge_material(&mut self) -> usize {
        if let Some(i) = self.edge_material {
            return i;
        }
        let m = SceneMaterial {
            name: "Edges".into(),
            base_color: Vec4::new(0.0, 0.0, 0.0, 1.0),
            unlit: true,
            double_sided: true,
            ..SceneMaterial::default()
        };
        self.scene.materials.push(m);
        let i = self.scene.materials.len() - 1;
        self.edge_material = Some(i);
        i
    }
}

/// Tints a picture towards `color` as SketchUp's colourise does: each
/// channel scaled so the picture's average becomes the colour, keeping its
/// detail.
fn colorize(tex: &mut Texture, color: [u8; 3]) {
    let Some(Pixels::Rgba8(px)) = tex.levels.first().and_then(|l| l.surfaces.first()) else { return };
    let n = (px.len() / 4).max(1) as f64;
    let mut avg = [0.0f64; 3];
    for p in px.chunks_exact(4) {
        for c in 0..3 {
            avg[c] += linear(p[c]) as f64;
        }
    }
    let gain: Vec<f32> = (0..3).map(|c| (linear(color[c]) as f64 / (avg[c] / n).max(1e-4)).min(64.0) as f32).collect();
    for level in &mut tex.levels {
        for s in &mut level.surfaces {
            if let Pixels::Rgba8(px) = s {
                for p in px.chunks_exact_mut(4) {
                    for c in 0..3 {
                        let v = (linear(p[c]) * gain[c]).clamp(0.0, 1.0);
                        let srgb = if v <= 0.0031308 { v * 12.92 } else { 1.055 * v.powf(1.0 / 2.4) - 0.055 };
                        p[c] = (srgb * 255.0 + 0.5) as u8;
                    }
                }
            }
        }
    }
}

//endregion

/*┌─────────────────╮
| Warnings        |
└─────────────────╯*/
//region Warnings

impl Builder<'_> {
    fn warn(&mut self) {
        let t = &self.tally;
        // A misread shows as loops that don't close and faces that aren't
        // flat or have no area; SketchUp's own faces are none of these, but
        // real files carry a few slivers, so only a noticeable share is
        // worth a warning (a misread spoils far more).
        let share = |n: usize| n as f64 / t.faces.max(1) as f64;
        let mut facts = Vec::new();
        if t.degenerate > 0 {
            let text = format!("{} of {} faces have no area or no outline and were left out", t.degenerate, t.faces);
            if share(t.degenerate) > 0.02 { self.warnings.push(text) } else { facts.push(text) }
        }
        let suspect = t.open_loops.max(t.non_planar);
        if suspect > 0 {
            let text = format!(
                "{} faces have loops that don't close and {} aren't flat, of {}",
                t.open_loops, t.non_planar, t.faces
            );
            if share(suspect) > 0.02 {
                self.warnings.push(format!("{text}; the file may not have been read correctly"));
            } else {
                facts.push(text);
            }
        }
        if !facts.is_empty() {
            self.scene.info.details.push(("Odd faces".into(), facts.join("; ")));
        }
        if self.cycles > 0 {
            self.warnings
                .push(format!("{} placements contain themselves or nest too deeply and were left out", self.cycles));
        }
        if self.over_budget {
            self.warnings.push(format!(
                "the model is too large to show whole: placements were left out after {} triangles",
                self.placed_triangles
            ));
        }
    }
}

//endregion

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn placements_apply_rows() {
        // A quarter turn about Z as SketchUp stores it, moved 10 inches.
        let m = [0.0, -1.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 10.0, 0.0, 0.0, 1.0];
        let p = placement(&m).transform_point3(DVec3::new(1.0, 0.0, 0.0));
        assert!((p - DVec3::new(10.0 * INCH, 1.0, 0.0)).length() < 1e-9, "{p}");
        assert!(node_transform(placement(&m)).is_some());
        // Shear can't be a node transform.
        let shear = [1.0, 0.5, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0];
        assert!(node_transform(placement(&shear)).is_none());
    }

    #[test]
    fn texture_axes_follow_sketchup() {
        let (x, y) = uv_axes(DVec3::Z);
        assert_eq!((x, y), (DVec3::X, DVec3::Y));
        let (x, _) = uv_axes(DVec3::NEG_Z);
        assert_eq!(x, DVec3::NEG_X);
        let (x, y) = uv_axes(DVec3::NEG_Y);
        assert!((x - DVec3::X).length() < 1e-12 && (y - DVec3::Z).length() < 1e-12, "{x} {y}");
    }
}
