//! USD geometry turned into a `Scene`: meshes (with their primvars and
//! GeomSubsets), the simple shapes (cube, sphere, cylinder, cone, capsule,
//! plane), transforms from xformOps, bound materials, and the stage's up
//! axis and unit.  Skinning, blend shapes and clips are `skel`'s; point
//! instancers are `instancer`'s.  Curves and points are not read yet.

use super::instancer;
use super::skel::{self, Deform};
use super::stage::{CPrim, Stage};
use super::{Ctx, Value, shade};
use crate::realize::{self, MatDesc};
use slerp_scene::glam::{DMat4, DQuat, DVec3, Mat4, Quat, Vec2, Vec3, Vec4};
use slerp_scene::{LoadError, Mesh, Node, Primitive, Scene, Shape, Transform};
use std::collections::HashMap;
use std::f32::consts::{FRAC_PI_2, PI, TAU};

const GPRIMS: &[&str] = &["Mesh", "Cube", "Sphere", "Cylinder", "Cone", "Capsule", "Plane"];

/// Prim types that are never drawn, so traversal doesn't make nodes for
/// them or look inside.
const NOT_DRAWN: &[&str] = &[
    "Material",
    "Shader",
    "NodeGraph",
    "GeomSubset",
    "SkelAnimation",
    "BlendShape",
    "Camera",
    "DistantLight",
    "DomeLight",
    "SphereLight",
    "RectLight",
    "DiskLight",
    "CylinderLight",
    "GeometryLight",
    "PhysicsScene",
    "RenderSettings",
    "RenderProduct",
    "RenderVar",
];

/// Whether the stage has anything to draw.  A point instancer counts, as
/// its prototypes may sit under an `over` that keeps them from being drawn
/// on their own, and so does a skeleton, as a file of skeletons and their
/// animations is an animation library for other characters.
pub(crate) fn has_geometry(stage: &Stage) -> bool {
    const ALSO: &[&str] = &["PointInstancer", "Skeleton"];
    let drawn = |t: &str| GPRIMS.contains(&t) || ALSO.contains(&t);
    stage.prims.iter().any(|p| p.defined && p.active && drawn(&p.type_name))
}

pub(crate) fn build(ctx: &mut Ctx, stage: &Stage, name: String) -> Result<Scene, LoadError> {
    let has_render = stage.prims.iter().any(|p| p.token("purpose") == Some("render"));
    let budget = instancer::Budget::new(ctx.limits);
    let mut b = Builder {
        ctx,
        stage,
        scene: Scene { name, ..Default::default() },
        materials: HashMap::new(),
        descs: Vec::new(),
        display_material: None,
        mesh_cache: HashMap::new(),
        triangles: 0,
        skipped: HashMap::new(),
        has_render,
        rig: skel::Rig::default(),
        budget,
        prototype_depth: 0,
        expanding: Vec::new(),
    };

    // 1. Walk the prims, depth first, making a node for each drawable one.
    for &c in &stage.prims[0].children {
        if let Some(root) = b.visit(c, Inherited::default())? {
            b.scene.roots.push(root);
        }
    }

    // 2. Skeletons, skins, blend shapes and clips.
    skel::finish(&mut b)?;

    // 3. Materials and textures.
    let mut warnings = std::mem::take(&mut b.ctx.warnings);
    let descs: Vec<&MatDesc> = b.descs.iter().collect();
    let realized = realize::realize(&descs, b.ctx.package.as_ref(), b.ctx.limits, false, &mut warnings);
    let mut scene = b.scene;
    scene.materials = realized.materials;
    scene.textures = realized.textures;
    scene.ensure_normals();

    // 4. Units and up axis: metres, Y up.
    let rotation = if stage.up_z { Quat::from_rotation_x(-FRAC_PI_2) } else { Quat::IDENTITY };
    let scale = Vec3::splat(stage.meters_per_unit);
    if rotation != Quat::IDENTITY || stage.meters_per_unit != 1.0 {
        scene.wrap_roots(Transform { translation: Vec3::ZERO, rotation, scale });
    }

    // 5. Facts for the inspector.
    let info = &mut scene.info;
    info.details.push(("Up axis".into(), if stage.up_z { "Z" } else { "Y" }.into()));
    info.details.push(("Metres per unit".into(), format!("{}", stage.meters_per_unit)));
    info.details.push(("Prims".into(), format!("{}", stage.prims.len() - 1)));
    info.details.push(("Layers".into(), format!("{}", b.ctx.layers.len())));
    for d in &b.descs {
        info.details.push((format!("Material {}", d.material.name), d.model.clone()));
    }
    info.details.append(&mut b.rig.details);
    info.details.extend(b.budget.details());
    let mut skipped: Vec<_> = b.skipped.into_iter().collect();
    skipped.sort();
    for (kind, n) in skipped {
        warnings.push(format!("{n} {kind} prim(s) not drawn"));
    }
    info.warnings = warnings;
    Ok(scene)
}

/// What a prim inherits from its ancestors: its purpose, its material
/// binding (with whether it is stronger than descendants'), and its
/// UsdSkel binding properties.
#[derive(Clone, Copy, Debug)]
pub(super) struct Inherited<'s> {
    pub purpose: &'s str,
    pub binding: Option<(usize, bool)>,
    pub skel: skel::Binding,
}

impl Default for Inherited<'_> {
    fn default() -> Self {
        Inherited { purpose: "default", binding: None, skel: skel::Binding::default() }
    }
}

/// Identifies a mesh already built, so prims referencing the same asset
/// share it: the address of the points and indices arrays, and materials.
type MeshKey = (usize, usize, Option<usize>, Vec<(usize, usize)>);

pub(super) struct Builder<'c, 'a> {
    pub ctx: &'c mut Ctx<'a>,
    pub stage: &'c Stage,
    pub scene: Scene,
    /// Material prim to its index in `descs` and its UV primvar name.
    materials: HashMap<usize, (usize, Option<String>)>,
    descs: Vec<MatDesc>,
    /// The plain white material of meshes coloured by `displayColor`, as
    /// an index into `descs`, once one needs it.
    display_material: Option<usize>,
    mesh_cache: HashMap<MeshKey, usize>,
    triangles: u64,
    skipped: HashMap<String, usize>,
    /// Whether any prim has the render purpose, which hides proxies.
    has_render: bool,
    /// Skeletons, skinned and morphed nodes, and animated transforms
    /// found so far.
    pub rig: skel::Rig,
    /// What point instancers may still add.
    pub budget: instancer::Budget,
    /// Above zero while a point instancer's prototype is being built:
    /// prototypes are drawn in their rest pose, without skins or clips.
    pub prototype_depth: u32,
    /// The point instancers being expanded, outermost first, to catch
    /// prototypes that contain their own instancer.
    pub expanding: Vec<usize>,
}

impl<'c> Builder<'c, '_> {
    /// Makes nodes for `root` and the drawable prims below it, depth
    /// first, and returns the root's node (not yet attached to anything),
    /// or None when the prim isn't drawn.
    pub(super) fn visit(&mut self, root: usize, inherited: Inherited<'c>) -> Result<Option<usize>, LoadError> {
        let stage = self.stage;
        let mut root_node = None;
        let mut stack: Vec<(usize, Option<usize>, Inherited<'c>)> = vec![(root, None, inherited)];
        while let Some((pi, parent, inh)) = stack.pop() {
            let p = &stage.prims[pi];
            if !p.def || !p.active || NOT_DRAWN.contains(&p.type_name.as_str()) {
                continue;
            }
            if p.token("visibility") == Some("invisible") {
                continue;
            }
            let purpose = p.token("purpose").unwrap_or(inh.purpose);
            if purpose == "guide" || (purpose == "proxy" && self.has_render) {
                continue;
            }
            // An ancestor's binding marked stronger than its descendants' wins.
            let binding = match inh.binding {
                Some((_, true)) => inh.binding,
                _ => self.binding(p).or(inh.binding),
            };
            let inh = Inherited { purpose, binding, skel: inh.skel.at(stage, pi) };
            let mut node = Node::new(p.name.clone());
            node.transform = self.xform(p);
            let ni = self.scene.nodes.len();
            // Prototypes are drawn in their rest pose.
            let posed = self.prototype_depth == 0;
            match p.type_name.as_str() {
                "Mesh" => {
                    let points = match p.value("points") {
                        Some(Value::Floats(v, 3)) => v.len() / 3,
                        _ => 0,
                    };
                    let d = if posed { skel::deform(self, pi, &inh.skel, points) } else { None };
                    node.mesh = self.mesh(pi, binding, d.as_ref())?;
                    if let (Some(d), Some(_)) = (d, node.mesh) {
                        self.rig.add_deformed(ni, d);
                    }
                }
                "Cube" | "Sphere" | "Cylinder" | "Cone" | "Capsule" | "Plane" => {
                    let d = if posed { skel::deform(self, pi, &inh.skel, 0) } else { None };
                    node.mesh = self.shape(pi, binding, d.as_ref())?;
                    if let (Some(d), Some(_)) = (d, node.mesh) {
                        self.rig.add_deformed(ni, d);
                    }
                }
                "Points" | "BasisCurves" | "NurbsCurves" | "NurbsPatch" | "HermiteCurves" | "TetMesh" => {
                    *self.skipped.entry(p.type_name.clone()).or_default() += 1;
                }
                _ => {}
            }
            if posed && skel::animated_xform(p) {
                self.rig.xforms.push((ni, pi));
            }
            self.scene.nodes.push(node);
            match parent {
                Some(pn) => self.scene.nodes[pn].children.push(ni),
                None => root_node = Some(ni),
            }
            // A skeleton's children are joints rather than drawn prims, and
            // a point instancer's are its prototypes, drawn only as
            // instances.
            match p.type_name.as_str() {
                "Skeleton" => {
                    if posed {
                        skel::add_skeleton(self, pi, ni);
                    }
                }
                "PointInstancer" => instancer::expand(self, pi, ni)?,
                _ => {
                    for &c in p.children.iter().rev() {
                        stack.push((c, Some(ni), inh));
                    }
                }
            }
        }
        Ok(root_node)
    }

    /// What prototype `prim` inherits from its ancestors when built away
    /// from where it sits: material bindings only.  Visibility and purpose
    /// count from the prototype down, as in UsdImaging (prototypes are
    /// often kept under an invisible scope).
    pub(super) fn inherited_at(&self, prim: usize) -> Inherited<'c> {
        let stage = self.stage;
        let mut inh = Inherited::default();
        let ancestry = stage.ancestry(prim);
        for &a in &ancestry[..ancestry.len().saturating_sub(1)] {
            let p = &stage.prims[a];
            inh.binding = match inh.binding {
                Some((_, true)) => inh.binding,
                _ => self.binding(p).or(inh.binding),
            };
        }
        inh
    }

    /// The local-to-world matrix of a prim from its own and its ancestors'
    /// xformOps, for things built away from their place in the hierarchy.
    pub(super) fn world_matrix(&mut self, prim: usize) -> Mat4 {
        let stage = self.stage;
        let mut m = Mat4::IDENTITY;
        for a in stage.ancestry(prim) {
            m *= self.xform(&stage.prims[a]).matrix();
        }
        m
    }
}

/// How a primvar's values map onto a mesh.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Interp {
    Constant,
    Uniform,
    Vertex,
    FaceVarying,
}

struct Primvar<'v> {
    data: &'v [f32],
    width: usize,
    interp: Interp,
    indices: Option<&'v [i32]>,
}

impl Primvar<'_> {
    /// The element a corner reads: by face, point or corner, then through
    /// the primvar's indices.
    fn element(&self, face: usize, point: usize, corner: usize) -> Option<usize> {
        let e = match self.interp {
            Interp::Constant => 0,
            Interp::Uniform => face,
            Interp::Vertex => point,
            Interp::FaceVarying => corner,
        };
        let e = match self.indices {
            Some(ix) => usize::try_from(*ix.get(e)?).ok()?,
            None => e,
        };
        (e < self.data.len() / self.width).then_some(e)
    }

    fn get(&self, e: usize) -> &[f32] {
        &self.data[e * self.width..(e + 1) * self.width]
    }
}

/// Counts of a mesh's points, faces and face corners, which tell which
/// interpolation a primvar of a given length has when its metadata is
/// missing or wrong.
struct Counts {
    points: usize,
    faces: usize,
    corners: usize,
}

fn primvar<'v>(p: &'v CPrim, name: &str, width: usize, default: Interp, n: &Counts) -> Option<Primvar<'v>> {
    let prop = p.prop(name)?;
    let Value::Floats(data, w) = prop.value.as_deref()? else { return None };
    if *w != width || data.is_empty() {
        return None;
    }
    let indices = match p.value(&format!("{name}:indices")) {
        Some(Value::Ints(ix)) if !ix.is_empty() => Some(ix.as_slice()),
        _ => None,
    };
    let len = indices.map_or(data.len() / width, <[i32]>::len);
    let authored = match prop.interpolation.as_deref() {
        Some("constant") => Some(Interp::Constant),
        Some("uniform") => Some(Interp::Uniform),
        Some("vertex" | "varying") => Some(Interp::Vertex),
        Some("faceVarying") => Some(Interp::FaceVarying),
        _ => None,
    };
    let fits = |i: Interp| match i {
        Interp::Constant => len >= 1,
        Interp::Uniform => len >= n.faces,
        Interp::Vertex => len >= n.points,
        Interp::FaceVarying => len >= n.corners,
    };
    let interp = [authored, Some(default)].into_iter().flatten().find(|&i| fits(i)).or_else(|| {
        [Interp::FaceVarying, Interp::Vertex, Interp::Uniform].into_iter().find(|&i| fits(i) && len >= 1)
    })?;
    Some(Primvar { data, width, interp, indices })
}

/// The address of a property's value, to recognise the same array shared
/// by several prims.
fn value_id(p: &CPrim, name: &str) -> usize {
    p.prop(name).and_then(|q| q.value.as_ref()).map_or(0, |v| std::sync::Arc::as_ptr(v) as usize)
}

impl Builder<'_, '_> {
    /// The Material a prim binds, preferring the real-time preview binding,
    /// and whether the binding is stronger than its descendants'.
    fn binding(&self, p: &CPrim) -> Option<(usize, bool)> {
        for rel in ["material:binding:preview", "material:binding"] {
            let Some(q) = p.prop(rel) else { continue };
            if let Some(t) = q.targets.first()
                && let Some(m) = self.stage.find(t).filter(|&m| self.stage.prims[m].type_name == "Material")
            {
                return Some((m, q.stronger));
            }
        }
        None
    }

    fn material(&mut self, prim: usize) -> (usize, Option<String>) {
        if let Some(m) = self.materials.get(&prim) {
            return m.clone();
        }
        let (desc, uv) = shade::material_desc(self.ctx, self.stage, prim);
        self.descs.push(desc);
        let entry = (self.descs.len() - 1, uv);
        self.materials.insert(prim, entry.clone());
        entry
    }

    /// The material for meshes without one that carry `displayColor`.
    fn display_material(&mut self, double_sided: bool) -> usize {
        let i = *self.display_material.get_or_insert_with(|| {
            let mut d = MatDesc::new("displayColor", "displayColor");
            d.material.base_color = Vec4::ONE;
            self.descs.push(d);
            self.descs.len() - 1
        });
        self.descs[i].material.double_sided |= double_sided;
        i
    }

    fn count_triangles(&mut self, n: u64) -> Result<(), LoadError> {
        self.triangles = self.triangles.saturating_add(n);
        if self.triangles > self.ctx.limits.max_triangles {
            return Err(LoadError::TooLarge(format!("more than {} triangles", self.ctx.limits.max_triangles)));
        }
        Ok(())
    }

    /// The prim's local transform from its xformOps, applied in
    /// `xformOpOrder`: the first op is outermost.
    pub(super) fn xform(&mut self, p: &CPrim) -> Transform {
        let Some(Value::Strs(order)) = p.value("xformOpOrder") else { return Transform::IDENTITY };
        if order.iter().any(|op| op == "!resetXformStack!") {
            self.ctx.warn("!resetXformStack! is ignored".into());
        }
        op_stack(order, |name| p.value(name).cloned())
    }

    /// The material index for a primitive, and whether vertex colours
    /// should carry `displayColor` instead.
    fn primitive_material(&mut self, binding: Option<usize>, double_sided: bool) -> (Option<usize>, Option<String>) {
        match binding {
            Some(m) => {
                let (i, uv) = self.material(m);
                if double_sided {
                    self.descs[i].material.double_sided = true;
                }
                (Some(i), uv)
            }
            None => (None, None),
        }
    }

    /// A Mesh prim's mesh, shared with other prims that hold the same
    /// arrays and materials unless `deform` moves its points.
    fn mesh(
        &mut self,
        pi: usize,
        bound: Option<(usize, bool)>,
        deform: Option<&Deform>,
    ) -> Result<Option<usize>, LoadError> {
        let binding = bound.map(|b| b.0);
        let stage = self.stage;
        let p = &stage.prims[pi];
        let (Some(Value::Floats(points, 3)), Some(Value::Ints(counts)), Some(Value::Ints(fvi))) =
            (p.value("points"), p.value("faceVertexCounts"), p.value("faceVertexIndices"))
        else {
            return Ok(None);
        };

        // 1. Face starts, skipping what the index array can't cover.
        let mut starts = Vec::with_capacity(counts.len());
        let mut corners = 0usize;
        let mut tris = 0u64;
        for &c in counts {
            let c = c.max(0) as usize;
            if corners + c > fvi.len() {
                break;
            }
            starts.push(corners);
            corners += c;
            tris += c.saturating_sub(2) as u64;
        }
        self.count_triangles(tris)?;
        let n = Counts { points: points.len() / 3, faces: starts.len(), corners };

        // 2. Materials: the mesh's own and its GeomSubsets'.
        let double_sided = p.value("doubleSided").and_then(Value::as_f64).is_some_and(|v| v != 0.0);
        let (material, uv_name) = self.primitive_material(binding, double_sided);
        let mut subsets: Vec<(&[i32], usize, usize)> = Vec::new();
        for &c in &p.children {
            let s = &stage.prims[c];
            if s.type_name != "GeomSubset" || s.token("elementType").is_some_and(|t| t != "face") {
                continue;
            }
            // A binding stronger than descendants also covers the subsets.
            if bound.is_some_and(|b| b.1) {
                break;
            }
            let (Some(Value::Ints(ix)), Some((m, _))) = (s.value("indices"), self.binding(s)) else { continue };
            let (Some(mi), _) = self.primitive_material(Some(m), double_sided) else { continue };
            subsets.push((ix, mi, c));
        }
        let key: MeshKey = (
            value_id(p, "points"),
            value_id(p, "faceVertexIndices"),
            material,
            subsets.iter().map(|s| (value_id(&stage.prims[s.2], "indices"), s.1)).collect(),
        );
        if deform.is_none()
            && let Some(&m) = self.mesh_cache.get(&key)
        {
            return Ok(Some(m));
        }

        // 3. Primvars.
        let uv_name = uv_name.unwrap_or_else(|| {
            if p.prop("primvars:st").is_some() {
                return "st".into();
            }
            p.props
                .iter()
                .find(|q| {
                    q.name.starts_with("primvars:")
                        && q.type_name.starts_with("texCoord2")
                        && !q.name.ends_with(":indices")
                })
                .map_or("st".into(), |q| q.name["primvars:".len()..].to_string())
        });
        let uv = primvar(p, &format!("primvars:{uv_name}"), 2, Interp::FaceVarying, &n);
        let normals = primvar(p, "primvars:normals", 3, Interp::Vertex, &n)
            .or_else(|| primvar(p, "normals", 3, Interp::Vertex, &n));
        let color =
            if material.is_none() { primvar(p, "primvars:displayColor", 3, Interp::Constant, &n) } else { None };
        let opacity =
            if material.is_none() { primvar(p, "primvars:displayOpacity", 1, Interp::Constant, &n) } else { None };
        let display_index = if color.is_some() { Some(self.display_material(double_sided)) } else { None };
        let left_handed = p.token("orientation") == Some("leftHanded");

        // 4. One primitive per material group.
        let mut group = vec![0usize; n.faces];
        for (gi, (ix, _, _)) in subsets.iter().enumerate() {
            for &f in *ix {
                if let Some(g) = usize::try_from(f).ok().and_then(|f| group.get_mut(f)) {
                    *g = gi + 1;
                }
            }
        }
        let attrs = Attrs { points, fvi, counts, starts: &starts, uv, normals, color, opacity, left_handed };
        let mut mesh = Mesh { name: p.name.clone(), ..Default::default() };
        for g in 0..=subsets.len() {
            let faces = (0..n.faces).filter(|&f| group[f] == g);
            let (mut prim, points) = attrs.primitive(faces, n.points);
            if prim.indices.is_empty() {
                continue;
            }
            if let Some(d) = deform {
                d.apply(&mut prim, &points);
            }
            prim.material = if g == 0 { material.or(display_index) } else { Some(subsets[g - 1].1) };
            mesh.primitives.push(prim);
        }
        if let Some(d) = deform {
            mesh.target_names = d.target_names.clone();
        }
        self.scene.meshes.push(mesh);
        let index = self.scene.meshes.len() - 1;
        if deform.is_none() {
            self.mesh_cache.insert(key, index);
        }
        Ok(Some(index))
    }

    /// The implicit shapes, built about the origin along their axis.  A
    /// rigid `deform` binds every vertex to the same joints.
    fn shape(
        &mut self,
        pi: usize,
        bound: Option<(usize, bool)>,
        deform: Option<&Deform>,
    ) -> Result<Option<usize>, LoadError> {
        let binding = bound.map(|b| b.0);
        let p = &self.stage.prims[pi];
        let num = |name: &str, default: f64| p.value(name).and_then(Value::as_f64).unwrap_or(default) as f32;
        let (mut prim, oriented) = match p.type_name.as_str() {
            "Sphere" => (scaled(Shape::Sphere.primitive(48), Vec3::splat(num("radius", 1.0) * 2.0)), false),
            "Cube" => (scaled(Shape::Cube.primitive(1), Vec3::splat(num("size", 2.0))), false),
            "Plane" => {
                let size = Vec3::new(num("width", 2.0), 1.0, num("length", 2.0));
                (scaled(Shape::Plane.primitive(1), size), true)
            }
            "Cylinder" | "Cone" => {
                let (r, h) = (num("radius", 1.0), num("height", 2.0) / 2.0);
                let top = if p.type_name == "Cone" { 0.0 } else { r };
                (lathe(&[&[(0.0, -h), (r, -h)], &[(r, -h), (top, h)], &[(top, h), (0.0, h)]]), true)
            }
            _ => {
                let (r, h) = (num("radius", 0.5), num("height", 1.0) / 2.0);
                let mut profile = Vec::new();
                for i in 0..=8 {
                    let a = i as f32 / 8.0 * FRAC_PI_2;
                    profile.push((r * a.sin(), -h - r * a.cos()));
                }
                for i in 0..=8 {
                    let a = i as f32 / 8.0 * FRAC_PI_2;
                    profile.push((r * a.cos(), h + r * a.sin()));
                }
                (lathe(&[&profile]), true)
            }
        };
        if oriented {
            let q = match p.token("axis").unwrap_or("Z") {
                "X" => Quat::from_rotation_z(-FRAC_PI_2),
                "Y" => Quat::IDENTITY,
                _ => Quat::from_rotation_x(FRAC_PI_2),
            };
            prim.positions.iter_mut().for_each(|v| *v = q * *v);
            prim.normals.iter_mut().for_each(|v| *v = q * *v);
            prim.tangents.clear();
        }
        self.count_triangles(prim.triangle_count() as u64)?;
        let double_sided =
            p.type_name == "Plane" || p.value("doubleSided").and_then(Value::as_f64).is_some_and(|v| v != 0.0);
        let (material, _) = self.primitive_material(binding, double_sided);
        prim.material = material;
        if material.is_none() {
            let color = match p.value("primvars:displayColor") {
                Some(Value::Floats(c, 3)) if c.len() >= 3 => Some(Vec3::new(c[0], c[1], c[2])),
                _ => None,
            };
            if let Some(c) = color {
                prim.colors = vec![c.extend(1.0); prim.positions.len()];
                prim.material = Some(self.display_material(double_sided));
            }
        }
        if let Some(d) = deform {
            let points = vec![0; prim.positions.len()];
            d.apply(&mut prim, &points);
        }
        let name = p.name.clone();
        self.scene.meshes.push(Mesh { name, primitives: vec![prim], target_names: Vec::new() });
        Ok(Some(self.scene.meshes.len() - 1))
    }
}

/// What `Attrs::primitive` reads from a mesh.
struct Attrs<'v> {
    points: &'v [f32],
    fvi: &'v [i32],
    counts: &'v [i32],
    starts: &'v [usize],
    uv: Option<Primvar<'v>>,
    normals: Option<Primvar<'v>>,
    color: Option<Primvar<'v>>,
    opacity: Option<Primvar<'v>>,
    left_handed: bool,
}

impl Attrs<'_> {
    /// Triangulates `faces` as fans, sharing a vertex wherever every
    /// attribute agrees.  Also returns the point each vertex came from.
    fn primitive(&self, faces: impl Iterator<Item = usize>, point_count: usize) -> (Primitive, Vec<u32>) {
        let mut p = Primitive::default();
        let mut points: Vec<u32> = Vec::new();
        let vars = [&self.normals, &self.uv, &self.color, &self.opacity];
        let per_point =
            vars.iter().all(|v| v.as_ref().is_none_or(|v| matches!(v.interp, Interp::Vertex | Interp::Constant)));
        let mut by_point = if per_point { vec![u32::MAX; point_count] } else { Vec::new() };
        let mut by_key: HashMap<[u32; 5], u32> = HashMap::new();
        let mut corner_ids: Vec<u32> = Vec::new();
        for f in faces {
            let start = self.starts[f];
            let count = self.counts[f].max(0) as usize;
            if count < 3 {
                continue;
            }
            let ix = &self.fvi[start..start + count];
            if ix.iter().any(|&i| i < 0 || i as usize >= point_count) {
                continue;
            }
            corner_ids.clear();
            for (k, &pt) in ix.iter().enumerate() {
                let pt = pt as usize;
                let corner = start + k;
                let el = |v: &Option<Primvar>| v.as_ref().and_then(|v| v.element(f, pt, corner));
                let els = [el(&self.normals), el(&self.uv), el(&self.color), el(&self.opacity)];
                let existing = if per_point {
                    Some(by_point[pt]).filter(|&v| v != u32::MAX)
                } else {
                    let key = [pt as u32, id(els[0]), id(els[1]), id(els[2]), id(els[3])];
                    by_key.get(&key).copied()
                };
                let v = match existing {
                    Some(v) => v,
                    None => {
                        let v = p.positions.len() as u32;
                        self.push_vertex(&mut p, pt, els);
                        points.push(pt as u32);
                        if per_point {
                            by_point[pt] = v;
                        } else {
                            by_key.insert([pt as u32, id(els[0]), id(els[1]), id(els[2]), id(els[3])], v);
                        }
                        v
                    }
                };
                corner_ids.push(v);
            }
            for k in 1..count - 1 {
                let (a, b, c) = (corner_ids[0], corner_ids[k], corner_ids[k + 1]);
                if self.left_handed {
                    p.indices.extend_from_slice(&[a, c, b]);
                } else {
                    p.indices.extend_from_slice(&[a, b, c]);
                }
            }
        }
        (p, points)
    }

    fn push_vertex(&self, p: &mut Primitive, pt: usize, els: [Option<usize>; 4]) {
        let q = &self.points[pt * 3..pt * 3 + 3];
        p.positions.push(Vec3::new(q[0], q[1], q[2]));
        if let Some(n) = &self.normals {
            let v = els[0].map_or(Vec3::Y, |e| Vec3::from_slice(n.get(e)));
            p.normals.push(v.try_normalize().unwrap_or(Vec3::Y));
        }
        if let Some(uv) = &self.uv {
            // USD's t grows upwards from the bottom of the image; glTF's v
            // grows downwards from the top.
            let v = els[1].map_or(Vec2::ZERO, |e| Vec2::from_slice(uv.get(e)));
            p.uv0.push(Vec2::new(v.x, 1.0 - v.y));
        }
        if self.color.is_some() || self.opacity.is_some() {
            let c = match (&self.color, els[2]) {
                (Some(c), Some(e)) => Vec3::from_slice(c.get(e)),
                _ => Vec3::ONE,
            };
            let a = match (&self.opacity, els[3]) {
                (Some(o), Some(e)) => o.get(e)[0],
                _ => 1.0,
            };
            p.colors.push(c.extend(a));
        }
    }
}

fn id(e: Option<usize>) -> u32 {
    e.map_or(u32::MAX, |e| e as u32)
}

/// The transform of an xformOp stack, applied in `order` (the first op is
/// outermost), with `value` giving each op's value.
pub(super) fn op_stack(order: &[String], value: impl Fn(&str) -> Option<Value>) -> Transform {
    let mut m = DMat4::IDENTITY;
    for op in order {
        let (invert, name) = match op.strip_prefix("!invert!") {
            Some(n) => (true, n),
            None => (false, op.as_str()),
        };
        let Some(op_m) = value(name).and_then(|v| op_matrix(name, &v)) else { continue };
        m *= if invert { op_m.inverse() } else { op_m };
    }
    let m = m.as_mat4();
    if !m.is_finite() {
        return Transform::IDENTITY;
    }
    Transform::from_matrix(m)
}

/// The matrix of one xformOp, named like `xformOp:rotateXYZ:pivot`.
fn op_matrix(name: &str, v: &Value) -> Option<DMat4> {
    let kind = name.strip_prefix("xformOp:")?.split(':').next()?;
    let t: Vec<f64> = match v {
        Value::Tuple(t) => t.clone(),
        Value::Float(f) => vec![*f],
        Value::Int(i) => vec![*i as f64],
        Value::Floats(f, _) => f.iter().map(|&x| x as f64).collect(),
        _ => return None,
    };
    let v3 = || match t.len() {
        0 => None,
        1 | 2 => Some(DVec3::splat(t[0])),
        _ => Some(DVec3::new(t[0], t[1], t[2])),
    };
    let axis = |c: char, a: f64| match c {
        'X' => DMat4::from_rotation_x(a.to_radians()),
        'Y' => DMat4::from_rotation_y(a.to_radians()),
        _ => DMat4::from_rotation_z(a.to_radians()),
    };
    Some(match kind {
        "translate" => DMat4::from_translation(v3()?),
        "scale" => DMat4::from_scale(v3()?),
        "rotateX" | "rotateY" | "rotateZ" => axis(kind.chars().last()?, *t.first()?),
        "orient" if t.len() >= 4 => {
            let q = DQuat::from_xyzw(t[1], t[2], t[3], t[0]);
            DMat4::from_quat(if q.length_squared() > 0.0 { q.normalize() } else { DQuat::IDENTITY })
        }
        "transform" if t.len() >= 16 => DMat4::from_cols_slice(&t[..16]),
        // rotateXYZ rotates about X first, so X is innermost.
        k if k.starts_with("rotate") && k.len() == 9 => {
            let r = v3()?;
            let mut m = DMat4::IDENTITY;
            for (c, a) in k[6..].chars().zip([r.x, r.y, r.z]) {
                m = axis(c, a) * m;
            }
            m
        }
        _ => return None,
    })
}

fn scaled(mut p: Primitive, s: Vec3) -> Primitive {
    p.positions.iter_mut().for_each(|v| *v *= s);
    p.normals.iter_mut().for_each(|n| *n = (*n / s).normalize_or_zero());
    p
}

/// Revolves profile strips (radius, height) about +Y.  Separate strips get
/// separate vertices, so caps meet sides at a hard edge.
fn lathe(strips: &[&[(f32, f32)]]) -> Primitive {
    const SEGMENTS: u32 = 32;
    let mut p = Primitive::default();
    for strip in strips {
        let base = p.positions.len() as u32;
        for &(r, y) in strip.iter() {
            for s in 0..=SEGMENTS {
                let a = s as f32 / SEGMENTS as f32 * TAU;
                p.positions.push(Vec3::new(r * a.cos(), y, r * a.sin()));
                p.uv0.push(Vec2::new(s as f32 / SEGMENTS as f32, 0.5 - y.atan() / PI));
            }
        }
        let row = SEGMENTS + 1;
        for i in 0..strip.len().saturating_sub(1) as u32 {
            for s in 0..SEGMENTS {
                let a = base + i * row + s;
                let b = a + row;
                p.indices.extend_from_slice(&[a, b, a + 1, a + 1, b, b + 1]);
            }
        }
    }
    p.ensure_normals();
    p
}
