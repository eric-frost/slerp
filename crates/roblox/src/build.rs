//! Turns the instances of a place or model into a scene.
//!
//! Parts are drawn where their `CFrame` puts them in the world (a part's
//! position never depends on its parents), so models and folders are nodes
//! only to group what is inside them.  The parts of one model are merged
//! into one mesh with a primitive per material, coloured per vertex: a
//! place has tens of thousands of parts, and a node each would cost a draw
//! each.  Invisible parts (Transparency 1) and negative parts go to one
//! hidden node.  Everything is built in studs under a root node that scales
//! studs to metres.

use crate::csg::{self, Physics};
use crate::dom::{CFrame, Dom};
use crate::palette::{self, Look};
use crate::shapes::{self, Shape, Unit};
use crate::terrain;
use crate::{Budget, binary};
use glam::{Mat3, Vec3, Vec4};
use slerp_scene::{AlphaMode, Info, Limits, LoadError, Material, Mesh, Node, Primitive, Scene, SceneKind, Spawn};
use std::collections::HashMap;

/// Metres per stud, Roblox's own conversion (a stud is 28 cm).
pub(crate) const STUD: f32 = 0.28;
/// Models and folders nest deeper than this only in damaged or hostile
/// files; deeper groups merge into the one at this depth.
const MAX_DEPTH: usize = 64;
/// Unions inside unions' source parts are followed this far.
const MAX_UNION_NESTING: u32 = 4;

pub(crate) fn build(dom: &Dom, limits: &Limits, budget: &mut Budget, encoding: &str) -> Result<Scene, LoadError> {
    let mut b = Builder::new(dom, limits);
    // 1. What is drawn: a place's Workspace, or every root of a model.
    let workspace = dom.roots.iter().copied().find(|&r| dom.class(r) == "Workspace");
    let tops: Vec<usize> = match workspace {
        Some(w) => vec![w],
        None => dom.roots.clone(),
    };
    // 2. The groups that have something to draw inside them, and for
    //    thumbnails the part of the world worth framing.
    let wanted = wanted(dom);
    if limits.max_texture_size.is_some() {
        b.keep = framing_box(dom, &tops);
    }
    b.detail = hull_detail(dom, limits);
    // 3. Walk the tree, without recursion.
    let mut stack: Vec<(usize, usize, usize)> = tops.iter().rev().map(|&t| (t, 0, 0)).collect();
    while let Some((i, node, depth)) = stack.pop() {
        let Some(inst) = dom.instances.get(i) else { continue };
        let class = dom.class(i);
        if class == "Terrain" {
            b.terrain(i, node, budget)?;
            continue;
        }
        if is_part(dom, i) {
            b.part(i, node, &CFrame::IDENTITY, Vec3::ONE, 0, budget)?;
            for &c in inst.children.iter().rev() {
                stack.push((c, node, depth));
            }
            continue;
        }
        if class == "SpawnLocation" || class == "Camera" {
            continue;
        }
        if !wanted[i] {
            continue;
        }
        let here = if depth < MAX_DEPTH { b.group(i, node) } else { node };
        for &c in inst.children.iter().rev() {
            stack.push((c, here, depth + 1));
        }
    }
    // 4. Where a level's player starts, and the finishing touches.
    let place = workspace.is_some();
    if place {
        b.spawn(workspace);
    }
    b.finish(place, encoding)
}

/// Whether an instance draws as a part: it has a size and a position, as
/// every class derived from `BasePart` has (blocks, wedges, seats, spawn
/// locations, mesh parts, unions, ...).
fn is_part(dom: &Dom, i: usize) -> bool {
    let p = &dom.instances[i].props;
    p.size.is_some() && p.cframe.is_some() && dom.class(i) != "Terrain"
}

/// For thumbnails: the box around the bulk of the parts below `tops`, when
/// a few lie far outside it.  Places often keep something far from the
/// world (a menu backdrop thousands of studs away, storage boxes, a
/// lobby), and framing them all shrinks the place to dots.  The box is the
/// middle half of the part centres on each axis (the 25th to 75th
/// percentile), widened by one and a half times its size (at least 100
/// studs) on every side, the usual rule for outliers; parts beyond it are
/// left out of the thumbnail when they are under a third of all.
fn framing_box(dom: &Dom, tops: &[usize]) -> Option<(Vec3, Vec3)> {
    let mut centres = Vec::new();
    let mut stack: Vec<usize> = tops.to_vec();
    while let Some(i) = stack.pop() {
        let inst = &dom.instances[i];
        if is_part(dom, i)
            && let Some(c) = inst.props.cframe.filter(|c| c.position.is_finite())
            && inst.props.transparency.unwrap_or(0.0) < 0.999
        {
            centres.push(c.position);
        }
        stack.extend(inst.children.iter().copied());
    }
    if centres.len() < 10 {
        return None;
    }
    let (mut lo, mut hi) = (Vec3::ZERO, Vec3::ZERO);
    for axis in 0..3 {
        let mut v: Vec<f32> = centres.iter().map(|c| c[axis]).collect();
        v.sort_unstable_by(f32::total_cmp);
        let (a, b) = (v[v.len() / 4], v[v.len() * 3 / 4]);
        let widen = (1.5 * (b - a)).max(100.0);
        lo[axis] = a - widen;
        hi[axis] = b + widen;
    }
    let outside = centres.iter().filter(|c| !(c.cmpge(lo).all() && c.cmple(hi).all())).count();
    (outside > 0 && outside * 3 < centres.len()).then_some((lo, hi))
}

/// The finest hull detail whose triangles fit the budget: a modern place
/// can hold fifteen thousand mesh parts with thousands of hull triangles
/// each, sixty million in all, which no thumbnail and few viewers want.
/// Counting reads only the blobs' counts.  Thumbnails afford a million
/// triangles of hulls and never draw them in full; the viewer eight
/// million.
fn hull_detail(dom: &Dom, limits: &Limits) -> csg::Detail {
    let thumbnail = limits.max_texture_size.is_some();
    let budget = if thumbnail { 1_000_000 } else { 8_000_000 };
    let (mut tris, mut pieces) = (0usize, 0usize);
    for inst in &dom.instances {
        let e = inst.props.extra();
        if let Some(b) = e.physics.as_ref().or(e.physics_old.as_ref()) {
            let (t, p) = csg::physics_size(dom.blob(b));
            tris += t;
            pieces += p;
        }
    }
    let coarse = tris.min(pieces * 40);
    if !thumbnail && tris <= budget {
        csg::Detail::Full
    } else if coarse <= budget {
        csg::Detail::Coarse
    } else if pieces * 12 <= budget {
        csg::Detail::PieceBoxes
    } else {
        csg::Detail::PartBoxes
    }
}

/// Marks every instance with a part or terrain somewhere inside it.
fn wanted(dom: &Dom) -> Vec<bool> {
    let mut wanted = vec![false; dom.instances.len()];
    for i in 0..dom.instances.len() {
        if !(is_part(dom, i) || dom.class(i) == "Terrain") {
            continue;
        }
        let mut at = Some(i);
        while let Some(a) = at {
            if wanted[a] {
                break;
            }
            wanted[a] = true;
            at = dom.instances[a].parent;
        }
    }
    wanted
}

/// What a part's geometry came from, for the inspector.
#[derive(Default)]
struct Counts {
    parts: usize,
    hidden: usize,
    /// Mesh parts by how they were drawn.
    mesh_hulls: usize,
    mesh_boxes: usize,
    /// Unions by how they were drawn.
    union_mesh: usize,
    union_children: usize,
    union_hulls: usize,
    union_boxes: usize,
    /// Special meshes whose shape is a file on Roblox's servers.
    file_meshes: usize,
    /// Decals and textures, whose images are on Roblox's servers.
    images: usize,
    damaged: usize,
}

/// The primitives of one node, by material.
#[derive(Default)]
struct Group {
    prims: Vec<Primitive>,
    by_material: HashMap<usize, usize>,
}

/// A material bucket: rounded roughness and metalness, unlit, blended.
type MaterialKey = (u8, u8, bool, bool);

struct Builder<'a> {
    dom: &'a Dom,
    limits: &'a Limits,
    scene: Scene,
    materials: HashMap<MaterialKey, usize>,
    groups: HashMap<usize, Group>,
    hidden: Option<usize>,
    counts: Counts,
    triangles: u64,
    capped: bool,
    terrain: Option<String>,
    warnings: Vec<String>,
    /// Spawn locations' frames and sizes, for the player's start.
    spawns: Vec<(CFrame, Vec3)>,
    /// Baseplates set aside for thumbnails, drawn at the end cut down to
    /// the rest of the place.
    grounds: Vec<Ground>,
    /// The terrain's box in studs, which the rest of the place includes.
    terrain_bounds: Option<(Vec3, Vec3)>,
    /// For thumbnails, the box parts must be centred in to be drawn, when
    /// a few lie far from the rest.
    keep: Option<(Vec3, Vec3)>,
    /// How much hull detail the place can afford.
    detail: csg::Detail,
}

/// A flat, wide, axis-aligned block: a baseplate, perhaps.
struct Ground {
    target: usize,
    /// Its box in studs.
    lo: Vec3,
    hi: Vec3,
    paint: Paint,
}

impl<'a> Builder<'a> {
    fn new(dom: &'a Dom, limits: &'a Limits) -> Builder<'a> {
        let mut scene = Scene::default();
        let mut root = Node::new("root");
        root.transform.scale = Vec3::splat(STUD);
        scene.nodes.push(root);
        scene.roots = vec![0];
        Builder {
            dom,
            limits,
            scene,
            materials: HashMap::new(),
            groups: HashMap::new(),
            hidden: None,
            counts: Counts::default(),
            triangles: 0,
            capped: false,
            terrain: None,
            warnings: dom.warnings.clone(),
            spawns: Vec::new(),
            grounds: Vec::new(),
            terrain_bounds: None,
            keep: None,
            detail: csg::Detail::Full,
        }
    }

    /// A node for a model, folder or other container, under `parent`.
    fn group(&mut self, i: usize, parent: usize) -> usize {
        let inst = &self.dom.instances[i];
        let name = if inst.name.is_empty() { self.dom.class(i).to_owned() } else { inst.name.clone() };
        self.add_node(Node::new(name), parent)
    }

    fn add_node(&mut self, node: Node, parent: usize) -> usize {
        self.scene.nodes.push(node);
        let index = self.scene.nodes.len() - 1;
        self.scene.nodes[parent].children.push(index);
        index
    }

    /// The hidden node invisible and negative parts go to.
    fn hidden_node(&mut self) -> usize {
        if let Some(h) = self.hidden {
            return h;
        }
        let mut node = Node::new("Invisible parts");
        node.visible = false;
        let h = self.add_node(node, 0);
        self.hidden = Some(h);
        h
    }

    /*┌─────────────────╮
    | Parts           |
    └─────────────────╯*/
    //region Parts

    /// Draws part `i` into `node`'s mesh.  `frame` and `stretch` place a
    /// union's source parts, which are stored in the union's own frame at
    /// the size it was made at, where the union now is; `nesting` counts
    /// unions inside unions.
    fn part(
        &mut self,
        i: usize,
        node: usize,
        frame: &CFrame,
        stretch: Vec3,
        nesting: u32,
        budget: &mut Budget,
    ) -> Result<(), LoadError> {
        let dom = self.dom;
        let inst = &dom.instances[i];
        let p = &inst.props;
        let class = dom.class(i);
        let (Some(cframe), Some(size)) = (p.cframe, p.size) else { return Ok(()) };
        if !cframe.is_finite() || !size.is_finite() {
            self.counts.damaged += 1;
            return Ok(());
        }
        if nesting == 0 {
            if let Some((lo, hi)) = self.keep
                && !(cframe.position.cmpge(lo).all() && cframe.position.cmple(hi).all())
            {
                return Ok(());
            }
            self.counts.parts += 1;
        }
        let size = size.abs().min(Vec3::splat(1e5));
        // A part turned within a stretched union stretches along its own
        // axes by about as much (parts can't shear).
        let size = size * (cframe.rotation.transpose().abs() * stretch);
        let cframe = CFrame {
            position: frame.rotation * (cframe.position * stretch) + frame.position,
            rotation: frame.rotation * cframe.rotation,
        };
        if class == "SpawnLocation" && nesting == 0 {
            self.spawns.push((cframe, size));
        }
        // 1. Colour and look.
        let transparency = p.transparency.unwrap_or(0.0).clamp(0.0, 1.0);
        let color = p.color.or_else(|| p.brick_color.and_then(palette::brick_color)).unwrap_or(palette::DEFAULT_COLOR);
        let mut look = palette::look(p.material.unwrap_or(256));
        let reflect = p.reflectance.unwrap_or(0.0).clamp(0.0, 1.0);
        if reflect > 0.0 {
            look.roughness += (0.05 - look.roughness) * reflect;
            look.metallic = look.metallic.max(reflect);
        }
        let hidden = transparency >= 0.999 || class == "NegateOperation";
        if hidden {
            self.counts.hidden += 1;
        }
        let target = if hidden { self.hidden_node() } else { node };
        let paint = Paint { color: palette::linear(color).extend(1.0 - transparency), look };
        // 2. Images on the part are on Roblox's servers.
        for &c in &inst.children {
            if matches!(dom.class(c), "Decal" | "Texture") {
                self.counts.images += 1;
            }
        }
        // 3. The shape: a mesh child reshapes any part; then the class, and
        //    a part's own Shape.
        let mesh_child = inst
            .children
            .iter()
            .copied()
            .find(|&c| matches!(dom.class(c), "SpecialMesh" | "BlockMesh" | "CylinderMesh" | "FileMesh"));
        if let Some(m) = mesh_child {
            self.mesh_child(m, &cframe, size, &paint, target);
            return Ok(());
        }
        match class {
            "WedgePart" => self.unit(target, Shape::Wedge, &cframe, size, Vec3::ZERO, &paint),
            "CornerWedgePart" => self.unit(target, Shape::CornerWedge, &cframe, size, Vec3::ZERO, &paint),
            "TrussPart" => {
                let unit = shapes::truss(size, p.extra().style.unwrap_or(0));
                self.append(target, &unit, &cframe, Vec3::ONE, Vec3::ZERO, &paint);
            }
            "MeshPart" => self.mesh_part(i, &cframe, size, &paint, target),
            "UnionOperation" | "IntersectOperation" | "PartOperation" | "NegateOperation" => {
                self.union(i, &cframe, size, &paint, target, nesting, budget)?;
            }
            _ => {
                let shape = match p.shape {
                    Some(0) => Shape::Ball,
                    Some(2) => Shape::CylinderX,
                    Some(3) => Shape::Wedge,
                    Some(4) => Shape::CornerWedge,
                    _ => Shape::Block,
                };
                let dims = match shape {
                    // A ball part is always round, as wide as its smallest
                    // side; a cylinder part as wide as the smaller of its
                    // other two.
                    Shape::Ball => Vec3::splat(size.min_element()),
                    Shape::CylinderX => Vec3::new(size.x, size.y.min(size.z), size.y.min(size.z)),
                    _ => size,
                };
                if shape == Shape::Block
                    && nesting == 0
                    && self.limits.max_texture_size.is_some()
                    && let Some((lo, hi)) = ground_box(&cframe, size)
                {
                    self.grounds.push(Ground { target, lo, hi, paint });
                    return Ok(());
                }
                self.unit(target, shape, &cframe, dims, Vec3::ZERO, &paint);
            }
        }
        Ok(())
    }

    /// A part reshaped by a `SpecialMesh`, `BlockMesh` or `CylinderMesh`,
    /// whose `Scale` multiplies the part's size and whose `Offset` moves it
    /// in the part's own frame.
    fn mesh_child(&mut self, m: usize, cframe: &CFrame, size: Vec3, paint: &Paint, target: usize) {
        let e = self.dom.instances[m].props.extra();
        let scale = e.scale.filter(|s| s.is_finite()).unwrap_or(Vec3::ONE).abs().min(Vec3::splat(1e4));
        let offset =
            e.offset.filter(|s| s.is_finite()).unwrap_or(Vec3::ZERO).clamp(Vec3::splat(-1e5), Vec3::splat(1e5));
        let dims = size * scale;
        let mut paint = *paint;
        if let Some(tint) = e.vertex_color.filter(|v| v.is_finite() && *v != Vec3::ONE) {
            paint.color = (paint.color.truncate() * tint.clamp(Vec3::ZERO, Vec3::splat(4.0))).extend(paint.color.w);
        }
        let (shape, dims) = match self.dom.class(m) {
            "BlockMesh" => (Shape::Block, dims),
            "CylinderMesh" => {
                let d = dims.x.min(dims.z);
                (Shape::CylinderY, Vec3::new(d, dims.y, d))
            }
            "SpecialMesh" => match e.mesh_type.unwrap_or(5) {
                0 => (Shape::Head, Vec3::new(dims.x * 0.5, dims.y, dims.z)),
                3 => (Shape::Ball, dims),
                4 => (Shape::CylinderX, dims),
                2 | 7 | 9 | 10 => (Shape::Wedge, dims),
                11 => (Shape::CornerWedge, dims),
                8 => (Shape::Pyramid, dims),
                1 | 6 => (Shape::Block, dims),
                // A file mesh: its triangles are on Roblox's servers.  The
                // part's own box stands in for it.
                _ => {
                    self.counts.file_meshes += 1;
                    (Shape::Block, size)
                }
            },
            // A plain FileMesh: the same.
            _ => {
                self.counts.file_meshes += 1;
                (Shape::Block, size)
            }
        };
        self.unit(target, shape, cframe, dims, offset, &paint);
    }

    /// A mesh part: its mesh is on Roblox's servers, so it is drawn by its
    /// collision hulls when the file keeps them, else as its box.
    fn mesh_part(&mut self, i: usize, cframe: &CFrame, size: Vec3, paint: &Paint, target: usize) {
        let e = self.dom.instances[i].props.extra();
        let initial = e.initial_size.filter(|s| s.is_finite() && s.min_element() > 0.0);
        if let Some(Physics::Hulls(mesh)) = self.physics(i) {
            let fit = self.fit(&mesh, initial, size);
            self.append_mesh(target, &mesh, cframe, fit, paint);
            self.counts.mesh_hulls += 1;
            return;
        }
        self.counts.mesh_boxes += 1;
        self.unit(target, Shape::Block, cframe, size, Vec3::ZERO, paint);
    }

    /// A union: its triangles when the file keeps them, else its source
    /// parts when it subtracts nothing, else its collision hulls, else its
    /// source parts with the subtracted ones left out, else its box.
    #[allow(clippy::too_many_arguments)]
    fn union(
        &mut self,
        i: usize,
        cframe: &CFrame,
        size: Vec3,
        paint: &Paint,
        target: usize,
        nesting: u32,
        budget: &mut Budget,
    ) -> Result<(), LoadError> {
        let dom = self.dom;
        let e = dom.instances[i].props.extra();
        let initial = e.initial_size.filter(|s| s.is_finite() && s.min_element() > 0.0);
        let count = nesting == 0;
        // 1. Stored triangles.
        if let Some(mut mesh) = e.mesh_data.as_ref().and_then(|b| csg::union_mesh(dom.blob(b)).ok()) {
            // The stored colours are the source parts'; the union's own
            // colour covers them when it says so.
            if e.use_part_color.unwrap_or(false) {
                mesh.colors.clear();
            }
            let fit = self.fit(&mesh, initial, size);
            self.append_mesh(target, &mesh, cframe, fit, paint);
            if count {
                self.counts.union_mesh += 1;
            }
            return Ok(());
        }
        // 2. The source parts.
        let children = match (&e.child_data, nesting < MAX_UNION_NESTING) {
            (Some(b), true) => binary::read(dom.blob(b), budget).ok(),
            _ => None,
        };
        let subtracts = children
            .as_ref()
            .is_some_and(|c| c.instances.iter().enumerate().any(|(k, _)| c.class(k) == "NegateOperation"));
        if let Some(children) = children.as_ref().filter(|_| !subtracts) {
            self.union_children(
                children,
                e.use_part_color.unwrap_or(false),
                cframe,
                size,
                initial,
                paint,
                target,
                nesting,
                budget,
            )?;
            if count {
                self.counts.union_children += 1;
            }
            return Ok(());
        }
        // 3. Collision hulls.
        match self.physics(i) {
            Some(Physics::Hulls(mesh)) => {
                let fit = self.fit(&mesh, initial, size);
                self.append_mesh(target, &mesh, cframe, fit, paint);
                if count {
                    self.counts.union_hulls += 1;
                }
                return Ok(());
            }
            Some(Physics::Block) | None => {}
        }
        // 4. The source parts, without what they subtract.
        if let Some(children) = children.as_ref() {
            self.union_children(
                children,
                e.use_part_color.unwrap_or(false),
                cframe,
                size,
                initial,
                paint,
                target,
                nesting,
                budget,
            )?;
            if count {
                self.counts.union_children += 1;
            }
            return Ok(());
        }
        if count {
            self.counts.union_boxes += 1;
        }
        self.unit(target, Shape::Block, cframe, size, Vec3::ZERO, paint);
        Ok(())
    }

    /// Draws a union's source parts (a model inside it) where the union is
    /// now: the parts are stored in the union's own frame at the size it
    /// was made at, so they are moved by its present frame and stretched by
    /// how it was resized since.
    #[allow(clippy::too_many_arguments)]
    fn union_children(
        &mut self,
        children: &Dom,
        use_part_color: bool,
        cframe: &CFrame,
        size: Vec3,
        initial: Option<Vec3>,
        paint: &Paint,
        target: usize,
        nesting: u32,
        budget: &mut Budget,
    ) -> Result<(), LoadError> {
        let stretch = initial.map_or(Vec3::ONE, |i| (size / i).clamp(Vec3::splat(1e-3), Vec3::splat(1e3)));
        let mut sub = Builder::new(children, self.limits);
        sub.materials = std::mem::take(&mut self.materials);
        sub.scene.materials = std::mem::take(&mut self.scene.materials);
        sub.triangles = self.triangles;
        for k in 0..children.instances.len() {
            if !is_part(children, k) || children.class(k) == "NegateOperation" {
                continue;
            }
            sub.part(k, 0, cframe, stretch, nesting + 1, budget)?;
        }
        // The source parts take the union's colour when it says so.
        for (_, group) in sub.groups.drain() {
            for mut prim in group.prims {
                if use_part_color {
                    prim.colors.iter_mut().for_each(|c| *c = paint.color);
                }
                let material = prim.material.unwrap_or(0);
                self.push_prim(target, material, prim);
            }
        }
        self.materials = sub.materials;
        self.scene.materials = sub.scene.materials;
        self.triangles = sub.triangles;
        self.capped |= sub.capped;
        Ok(())
    }

    /// How to stretch a stored mesh to the part's size: by the size against
    /// the size the mesh was made at, or against the mesh's own box.
    fn fit(&self, mesh: &csg::Mesh, initial: Option<Vec3>, size: Vec3) -> Vec3 {
        let reference = initial.unwrap_or_else(|| {
            let (mut lo, mut hi) = (Vec3::splat(f32::INFINITY), Vec3::splat(f32::NEG_INFINITY));
            for &p in &mesh.positions {
                lo = lo.min(p);
                hi = hi.max(p);
            }
            (hi - lo).max(Vec3::splat(1e-4))
        });
        (size / reference.max(Vec3::splat(1e-4))).clamp(Vec3::splat(1e-4), Vec3::splat(1e4))
    }

    /// A part's collision geometry, from either property; for thumbnails
    /// a coarse copy, since a place can hold thousands of mesh parts with
    /// hundreds of hull triangles each.
    fn physics(&self, i: usize) -> Option<Physics> {
        let e = self.dom.instances[i].props.extra();
        let found = [&e.physics, &e.physics_old]
            .into_iter()
            .flatten()
            .find_map(|b| csg::physics(self.dom.blob(b), 64 << 20).ok());
        match (found, self.detail) {
            (Some(Physics::Hulls(m)), csg::Detail::Coarse) => Some(Physics::Hulls(m.coarse())),
            (Some(Physics::Hulls(m)), csg::Detail::PieceBoxes) => Some(Physics::Hulls(m.piece_boxes())),
            (Some(Physics::Hulls(_)), csg::Detail::PartBoxes) => Some(Physics::Block),
            (other, _) => other,
        }
    }

    //endregion

    /*┌─────────────────╮
    | Geometry        |
    └─────────────────╯*/
    //region Geometry

    fn unit(&mut self, target: usize, shape: Shape, cframe: &CFrame, dims: Vec3, offset: Vec3, paint: &Paint) {
        self.append(target, shapes::unit(shape), cframe, dims, offset, paint);
    }

    /// Adds `unit`, stretched by `dims` and moved by `offset` in the part's
    /// frame, to `target`'s mesh.
    fn append(&mut self, target: usize, unit: &Unit, cframe: &CFrame, dims: Vec3, offset: Vec3, paint: &Paint) {
        let normal = cframe.rotation * Mat3::from_diagonal(Vec3::ONE / dims.max(Vec3::splat(1e-6)));
        let positions = unit.positions.iter().map(|&p| cframe.rotation * (p * dims + offset) + cframe.position);
        let normals = unit.normals.iter().map(|&n| (normal * n).normalize_or(Vec3::Y));
        self.add(target, positions, Some(normals), None, &unit.indices, paint);
    }

    /// Adds a stored mesh, stretched by `fit`, to `target`'s mesh, in its
    /// own colours when it has them.
    fn append_mesh(&mut self, target: usize, mesh: &csg::Mesh, cframe: &CFrame, fit: Vec3, paint: &Paint) {
        let positions = mesh.positions.iter().map(|&p| cframe.rotation * (p * fit) + cframe.position);
        let colors: Vec<Vec4> =
            mesh.colors.iter().map(|c| palette::linear([c[0], c[1], c[2]]).extend(paint.color.w)).collect();
        self.add(
            target,
            positions,
            None::<std::iter::Empty<Vec3>>,
            Some(&colors).filter(|c| !c.is_empty()),
            &mesh.indices,
            paint,
        );
    }

    /// Appends triangles to the primitive of `target` that has the paint's
    /// material, within the triangle limit, in the paint's colour or
    /// `colors` per vertex.  Without normals, each triangle gets its own
    /// corners and a face normal: stored meshes are faceted.
    fn add(
        &mut self,
        target: usize,
        positions: impl Iterator<Item = Vec3>,
        normals: Option<impl Iterator<Item = Vec3>>,
        colors: Option<&Vec<Vec4>>,
        indices: &[u32],
        paint: &Paint,
    ) {
        let triangles = (indices.len() / 3) as u64;
        if self.triangles + triangles > self.limits.max_triangles {
            self.capped = true;
            return;
        }
        self.triangles += triangles;
        let material = self.material(&paint.look, paint.color.w < 0.999);
        let positions: Vec<Vec3> = positions.collect();
        let color = |i: u32| colors.and_then(|c| c.get(i as usize)).copied().unwrap_or(paint.color);
        let mut prim = Primitive { material: Some(material), ..Default::default() };
        match normals {
            Some(normals) => {
                prim.normals = normals.collect();
                prim.colors = (0..positions.len() as u32).map(color).collect();
                prim.positions = positions;
                prim.indices = indices.to_vec();
            }
            None => {
                for t in indices.chunks_exact(3) {
                    let [a, b, c] = [t[0], t[1], t[2]].map(|i| positions.get(i as usize).copied().unwrap_or_default());
                    let n = (b - a).cross(c - a).normalize_or(Vec3::Y);
                    let base = prim.positions.len() as u32;
                    prim.positions.extend_from_slice(&[a, b, c]);
                    prim.normals.extend_from_slice(&[n, n, n]);
                    prim.colors.extend(t.iter().map(|&i| color(i)));
                    prim.indices.extend_from_slice(&[base, base + 1, base + 2]);
                }
            }
        }
        self.push_prim(target, material, prim);
    }

    /// Merges `prim` into `target`'s primitive for `material`.
    fn push_prim(&mut self, target: usize, material: usize, prim: Primitive) {
        let group = self.groups.entry(target).or_default();
        let slot = *group.by_material.entry(material).or_insert_with(|| {
            group.prims.push(Primitive { material: Some(material), ..Default::default() });
            group.prims.len() - 1
        });
        let out = &mut group.prims[slot];
        let base = out.positions.len() as u32;
        out.positions.extend_from_slice(&prim.positions);
        out.normals.extend_from_slice(&prim.normals);
        out.colors.extend_from_slice(&prim.colors);
        out.indices.extend(prim.indices.iter().map(|&i| i + base));
    }

    /// The material for a look, shared by every part with the same one.
    fn material(&mut self, look: &Look, blend: bool) -> usize {
        let q = |v: f32| (v.clamp(0.0, 1.0) * 20.0).round() as u8;
        let key = (q(look.roughness), q(look.metallic), look.unlit, blend);
        if let Some(&m) = self.materials.get(&key) {
            return m;
        }
        let material = Material {
            name: format!(
                "{}{}",
                if look.unlit { "glowing " } else { "" },
                if blend { "see-through" } else { "solid" }
            ),
            base_color: Vec4::ONE,
            roughness: key.0 as f32 / 20.0,
            metallic: key.1 as f32 / 20.0,
            unlit: look.unlit,
            alpha_mode: if blend { AlphaMode::Blend } else { AlphaMode::Opaque },
            ..Default::default()
        };
        self.scene.materials.push(material);
        let m = self.scene.materials.len() - 1;
        self.materials.insert(key, m);
        m
    }

    //endregion

    /*┌─────────────────╮
    | Terrain         |
    └─────────────────╯*/
    //region Terrain

    fn terrain(&mut self, i: usize, node: usize, budget: &mut Budget) -> Result<(), LoadError> {
        let e = self.dom.instances[i].props.extra();
        let Some(grid) = e.smooth_grid.as_ref() else { return Ok(()) };
        let mut facts = terrain::Facts::default();
        let grid = match terrain::decode(self.dom.blob(grid), budget, &mut facts) {
            Ok(Some(g)) => g,
            Ok(None) => return Ok(()),
            Err(LoadError::TooLarge(_)) => {
                self.warnings.push("The terrain is larger than Slerp draws; it is left out.".into());
                return Ok(());
            }
            Err(err) => {
                self.warnings.push(format!("The terrain could not be read ({err}); it is not drawn."));
                return Ok(());
            }
        };
        let colors = palette::terrain_colors(e.material_colors.as_ref().map(|b| self.dom.blob(b)));
        // Thumbnails draw a coarser surface than the viewer.
        let wanted = if self.limits.max_texture_size.is_some() { 400_000 } else { 6_000_000 };
        let left = self.limits.max_triangles.saturating_sub(self.triangles);
        let (solid, water, scale) = terrain::mesh(grid, wanted.min(left));
        // 1. The facts.
        let mut materials: Vec<(usize, f64)> =
            facts.materials.iter().copied().enumerate().filter(|m| m.1 > 0.0).collect();
        materials.sort_by(|a, b| b.1.total_cmp(&a.1));
        let names: Vec<&str> = materials.iter().take(6).map(|m| palette::TERRAIN_MATERIALS[m.0].0).collect();
        let mut text = format!("{} chunks; {}", facts.chunks, names.join(", "));
        if facts.water > 0 {
            text.push_str(", water");
        }
        if scale > 1 {
            text.push_str(&format!("; drawn {scale} times coarser"));
        }
        self.terrain = Some(text);
        // 2. The meshes: solid ground coloured by material, and water.
        let mut node_ = Node::new(self.dom.instances[i].name.clone().max(String::from("Terrain")));
        let mut prims = Vec::new();
        for (surface, is_water) in [(solid, false), (water, true)] {
            if surface.indices.is_empty() {
                continue;
            }
            let triangles = (surface.indices.len() / 3) as u64;
            if self.triangles + triangles > self.limits.max_triangles {
                self.capped = true;
                continue;
            }
            self.triangles += triangles;
            let colors: Vec<Vec4> = if is_water {
                let water = e.water_color.filter(|c| c.is_finite()).map_or(palette::TERRAIN_MATERIALS[1].1, |c| {
                    c.clamp(Vec3::ZERO, Vec3::ONE).to_array().map(|v| (v * 255.0) as u8)
                });
                vec![palette::linear(water).extend(0.7); surface.positions.len()]
            } else {
                surface.slots.iter().map(|&s| palette::linear(terrain::color(&colors, s)).extend(1.0)).collect()
            };
            let look =
                Look { roughness: if is_water { 0.05 } else { 0.9 }, metallic: 0.0, unlit: false, glassy: false };
            let material = self.material(&look, is_water);
            prims.push(Primitive {
                positions: surface.positions,
                normals: surface.normals,
                colors,
                indices: surface.indices,
                material: Some(material),
                ..Default::default()
            });
        }
        if prims.is_empty() {
            return Ok(());
        }
        let (mut lo, mut hi) = (Vec3::splat(f32::INFINITY), Vec3::splat(f32::NEG_INFINITY));
        for p in prims.iter().flat_map(|p| &p.positions) {
            lo = lo.min(*p);
            hi = hi.max(*p);
        }
        self.terrain_bounds = Some((lo, hi));
        self.scene.meshes.push(Mesh { name: "Terrain".into(), primitives: prims, target_names: vec![] });
        node_.mesh = Some(self.scene.meshes.len() - 1);
        self.add_node(node_, node);
        Ok(())
    }

    //endregion

    /*┌─────────────────╮
    | Finishing       |
    └─────────────────╯*/
    //region Finishing

    /// Draws the baseplates set aside, each cut to the rest of the place
    /// with a margin when it is over six times as wide, so a thumbnail
    /// framing the whole doesn't shrink a building on a 2,048-stud plate
    /// to a dot.
    fn place_grounds(&mut self) {
        let grounds = std::mem::take(&mut self.grounds);
        if grounds.is_empty() {
            return;
        }
        // 1. The box around everything else.
        let (mut lo, mut hi) =
            self.terrain_bounds.unwrap_or((Vec3::splat(f32::INFINITY), Vec3::splat(f32::NEG_INFINITY)));
        for (&node, group) in &self.groups {
            if Some(node) == self.hidden {
                continue;
            }
            for p in group.prims.iter().flat_map(|p| &p.positions) {
                lo = lo.min(*p);
                hi = hi.max(*p);
            }
        }
        let content = (lo.is_finite() && hi.is_finite()).then_some((lo, hi));
        let wide = |lo: Vec3, hi: Vec3| (hi.x - lo.x).max(hi.z - lo.z);
        // 2. Each plate, cut when it dwarfs the rest.
        for g in grounds {
            let (mut glo, mut ghi) = (g.lo, g.hi);
            if let Some((clo, chi)) = content
                && wide(glo, ghi) > 6.0 * wide(clo, chi).max(1.0)
            {
                let margin = (chi - clo) * 0.25 + Vec3::splat(8.0);
                for a in [0, 2] {
                    glo[a] = glo[a].max(clo[a] - margin[a]);
                    ghi[a] = ghi[a].min(chi[a] + margin[a]);
                }
                if glo.x >= ghi.x || glo.z >= ghi.z {
                    continue;
                }
                // A cut plate keeps its top: a 16-stud-thick slab under a
                // small building would look like a pedestal.
                glo.y = glo.y.max(ghi.y - 0.05 * (ghi.x - glo.x).max(ghi.z - glo.z));
            }
            let frame = CFrame { position: (glo + ghi) * 0.5, rotation: Mat3::IDENTITY };
            self.unit(g.target, Shape::Block, &frame, ghi - glo, Vec3::ZERO, &g.paint);
        }
    }

    /// The player's start: the first spawn location, on top of it and
    /// facing where it faces; else the place's saved camera when it looks
    /// roughly level.
    fn spawn(&mut self, workspace: Option<usize>) {
        if let Some((cf, size)) = self.spawns.first() {
            // A character's eyes are about 4.5 studs above its feet.
            let top = cf.position + cf.rotation * Vec3::new(0.0, size.y * 0.5, 0.0);
            let eye = top + Vec3::Y * 4.5;
            let look = cf.rotation * Vec3::NEG_Z;
            let yaw = if look.x.abs() + look.z.abs() > 1e-3 { (-look.x).atan2(-look.z) } else { 0.0 };
            self.scene.spawn = Some(Spawn { position: eye * STUD, yaw });
            return;
        }
        let dom = self.dom;
        let Some(w) = workspace else { return };
        let camera = dom.instances[w].children.iter().copied().find(|&c| dom.class(c) == "Camera");
        if let Some(cf) = camera.and_then(|c| dom.instances[c].props.cframe).filter(|c| c.is_finite()) {
            let forward = (cf.rotation * Vec3::NEG_Z).normalize_or_zero();
            let pitch = forward.y.clamp(-1.0, 1.0).asin();
            if forward != Vec3::ZERO && pitch.abs() < 50f32.to_radians() {
                self.scene.spawn = Some(Spawn { position: cf.position * STUD, yaw: (-forward.x).atan2(-forward.z) });
            }
        }
    }

    fn finish(mut self, place: bool, encoding: &str) -> Result<Scene, LoadError> {
        // 1. Thumbnails frame what was built, not a baseplate far larger.
        self.place_grounds();
        // 2. Meshes for the groups, in node order.
        let mut groups: Vec<(usize, Group)> = self.groups.drain().collect();
        groups.sort_by_key(|g| g.0);
        for (node, group) in groups {
            let prims: Vec<Primitive> = group.prims.into_iter().filter(|p| !p.indices.is_empty()).collect();
            if prims.is_empty() {
                continue;
            }
            let name = self.scene.nodes[node].name.clone();
            self.scene.meshes.push(Mesh { name, primitives: prims, target_names: vec![] });
            let mesh = self.scene.meshes.len() - 1;
            if self.scene.nodes[node].mesh.is_none() {
                self.scene.nodes[node].mesh = Some(mesh);
            } else {
                let mut child = Node::new(self.scene.nodes[node].name.clone());
                child.mesh = Some(mesh);
                self.add_node(child, node);
            }
        }
        if self.scene.meshes.is_empty() {
            return Err(LoadError::invalid(if place {
                "the place has no parts or terrain to draw"
            } else {
                "the model has no parts to draw"
            }));
        }
        self.scene.kind = if place { SceneKind::Level } else { SceneKind::Object };
        // 3. The inspector's facts and notes.
        let info = self.info(place, encoding);
        self.scene.info = info;
        Ok(self.scene)
    }

    fn info(&mut self, place: bool, encoding: &str) -> Info {
        let dom = self.dom;
        let c = &self.counts;
        let mut details = Vec::new();
        let mut by_class: HashMap<&str, usize> = HashMap::new();
        for i in 0..dom.instances.len() {
            *by_class.entry(dom.class(i)).or_default() += 1;
        }
        let mut classes: Vec<(&str, usize)> = by_class.into_iter().collect();
        classes.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
        details.push((
            "Instances".to_string(),
            format!("{} of {} classes", thousands(dom.instances.len()), classes.len()),
        ));
        let top: Vec<String> = classes.iter().take(12).map(|(k, n)| format!("{k} {}", thousands(*n))).collect();
        let more = classes.len().saturating_sub(12);
        details.push((
            "By class".to_string(),
            format!("{}{}", top.join(", "), if more > 0 { format!(", and {more} more") } else { String::new() }),
        ));
        details.push(("Parts".to_string(), thousands(c.parts)));
        if c.hidden > 0 {
            details.push((
                "Invisible parts".to_string(),
                format!("{} (hidden; Transparency 1 or negative parts)", thousands(c.hidden)),
            ));
        }
        let unions = c.union_mesh + c.union_children + c.union_hulls + c.union_boxes;
        if unions > 0 {
            let mut how = Vec::new();
            for (n, what) in [
                (c.union_mesh, "from their stored triangles"),
                (c.union_children, "from their source parts"),
                (c.union_hulls, "by their collision hulls"),
                (c.union_boxes, "as boxes"),
            ] {
                if n > 0 {
                    how.push(format!("{} {what}", thousands(n)));
                }
            }
            details.push(("Unions".to_string(), how.join(", ")));
        }
        let meshes = c.mesh_hulls + c.mesh_boxes;
        if meshes > 0 {
            details.push((
                "Mesh parts".to_string(),
                format!("{} by their collision hulls, {} as boxes", thousands(c.mesh_hulls), thousands(c.mesh_boxes)),
            ));
        }
        if c.mesh_hulls + c.union_hulls > 0 && self.detail != csg::Detail::Full {
            let how = match self.detail {
                csg::Detail::Coarse => "simplified, to keep the triangle count down",
                csg::Detail::PieceBoxes => "each convex piece as its box, to keep the triangle count down",
                _ => "as boxes, to keep the triangle count down",
            };
            details.push(("Collision hulls".to_string(), how.to_string()));
        }
        if c.file_meshes > 0 {
            details
                .push(("File meshes".to_string(), format!("{} drawn as their parts' boxes", thousands(c.file_meshes))));
        }
        if c.images > 0 {
            details.push(("Decals and textures".to_string(), format!("{} (not drawn)", thousands(c.images))));
        }
        if let Some(t) = &self.terrain {
            details.push(("Terrain".to_string(), t.clone()));
        }
        // Notes: what needs Roblox's servers.
        let mut warnings = std::mem::take(&mut self.warnings);
        let server_meshes = c.mesh_hulls + c.mesh_boxes;
        if server_meshes > 0 {
            warnings.push(match (c.mesh_hulls, c.mesh_boxes) {
                (0, b) => format!("{} mesh parts need Roblox's servers; shown as boxes.", thousands(b)),
                (h, 0) => format!("{} mesh parts need Roblox's servers; shown by their collision hulls.", thousands(h)),
                (h, b) => format!(
                    "{} mesh parts need Roblox's servers; {} shown by their collision hulls, {} as boxes.",
                    thousands(h + b),
                    thousands(h),
                    thousands(b)
                ),
            });
        }
        let server_unions = c.union_hulls + c.union_boxes;
        if server_unions > 0 {
            warnings.push(format!(
                "{} unions keep their triangles on Roblox's servers; {} shown by their collision hulls, {} as boxes.",
                thousands(server_unions),
                thousands(c.union_hulls),
                thousands(c.union_boxes)
            ));
        }
        if c.file_meshes > 0 {
            warnings.push(format!(
                "{} special meshes need Roblox's servers; shown as their parts' boxes.",
                thousands(c.file_meshes)
            ));
        }
        if c.images > 0 {
            warnings.push(format!(
                "{} decals and textures show images on Roblox's servers; not drawn.",
                thousands(c.images)
            ));
        }
        if c.damaged > 0 {
            warnings.push(format!("{} parts have no usable position or size and are left out.", thousands(c.damaged)));
        }
        if self.capped {
            warnings.push(format!(
                "The place has more than {} triangles; the rest are left out.",
                thousands(self.limits.max_triangles as usize)
            ));
        }
        Info {
            format: format!("Roblox {} ({encoding})", if place { "place" } else { "model" }),
            warnings,
            details,
            ..Default::default()
        }
    }

    //endregion
}

/// The colour and look of a part.
#[derive(Clone, Copy)]
struct Paint {
    /// Linear RGB and opacity.
    color: Vec4,
    look: Look,
}

/// A number with thousands separators.
fn thousands(n: usize) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// A block's box in studs when it could be a baseplate: flat, at least 100
/// studs across, and square to the world (so cutting its box keeps its
/// shape).
fn ground_box(cframe: &CFrame, size: Vec3) -> Option<(Vec3, Vec3)> {
    let r = cframe.rotation;
    let square = [r.x_axis, r.y_axis, r.z_axis].iter().all(|c| c.abs().max_element() > 0.999);
    let half = r.abs() * (size * 0.5);
    let wide = half.x.max(half.z) * 2.0;
    (square && wide >= 100.0 && half.y * 2.0 <= 0.1 * wide).then_some((cframe.position - half, cframe.position + half))
}
