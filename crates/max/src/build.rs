//! Turns a 3ds Max file's objects into a scene.
//!
//! Each `Node` object becomes a scene node: its name, its parent (chunk
//! 0x0960), its transform controller (reference 0), the object it shows
//! (reference 1) and its material (reference 3).  A node's own transform
//! comes from its Position/Rotation/Scale controller; the object sits at
//! the node's object offset (chunks 0x096A-0x096C), which children don't
//! inherit, so it hangs on a child node of its own when there is one.
//!
//! An object is a base object (Editable Mesh, Editable Poly, a primitive)
//! or a derived object: its modifiers (top of the stack first) and then
//! its base.  TurboSmooth and MeshSmooth are applied; other modifiers are
//! listed and the object is shown without them (deformers such as Skin
//! show the rest pose).
//!
//! 3ds Max quaternions turn the other way from the usual convention, so
//! stored rotations are conjugated; Euler angles are in radians and turn
//! as usual.

use crate::file::{MaxFile, controller_values, param_float, super_class};
use crate::material::Materials;
use crate::{mesh, prims};
use glam::{Quat, Vec3};
use slerp_polymesh::textures::TextureSet;
use slerp_polymesh::{PolyMesh, Smoothing, SubdivideOptions, Wants, corner_normals, primitives, subdivide};
use slerp_scene::{Limits, LoadError, Mesh, Node as SceneNode, Scene, Transform};
use std::collections::HashMap;
use std::path::Path;

const NODE: (u32, u32) = (1, 0);
const EDITABLE_MESH: (u32, u32) = (0xE44F_10B3, 0);
const EDITABLE_POLY: (u32, u32) = (0x1BF8_338D, 0x192F_6098);
const POSITION_XYZ: (u32, u32) = (0x118F_7E02, 0xFFEE_238A);
const EULER_XYZ: (u32, u32) = (0x2012, 0);
const SCALE_XYZ: (u32, u32) = (0x118F_7C01, 0xFEEE_238B);
const POSITION_LIST: (u32, u32) = (0x4B4B_1002, 0);
const ROTATION_LIST: (u32, u32) = (0x4B4B_1003, 0);
const SCALE_LIST: (u32, u32) = (0x4B4B_1004, 0);

/// The node flag of hidden nodes, as files from Max 2010 on show it.
const HIDDEN: u32 = 0x400;

/// Most scene nodes and deepest hierarchy followed.
const MAX_NODES: usize = 1_000_000;
const MAX_DEPTH: usize = 512;
/// Most subdivision levels applied, whatever the modifier says.
const MAX_LEVELS: u32 = 4;

/// What the inspector reports.
#[derive(Default)]
pub struct Report {
    pub nodes: usize,
    pub meshes: usize,
    pub primitives: Vec<String>,
    pub smoothed: Vec<String>,
    /// Modifiers not applied, by name, with counts.
    pub skipped_modifiers: HashMap<String, usize>,
    /// Objects drawn as nothing, by type, with counts.
    pub not_drawn: HashMap<String, usize>,
    pub materials: usize,
    pub unknown_materials: Vec<String>,
}

pub fn scene(file: &MaxFile, path: Option<&Path>, limits: &Limits, metres: f32) -> Result<(Scene, Report), LoadError> {
    let mut b = Builder {
        file,
        limits,
        scene: Scene::default(),
        report: Report::default(),
        materials: Materials::new(file, TextureSet::new(path, limits)),
        meshes: HashMap::new(),
        triangles: 0,
    };
    // 1. Nodes and their children, in file order.
    let n = file.objects.len();
    let mut children: Vec<Vec<usize>> = vec![Vec::new(); n];
    let mut roots = Vec::new();
    for i in 0..n {
        if file.class_id(i) != Some(NODE) {
            continue;
        }
        b.report.nodes += 1;
        let parent = file.objects[i].child_data(0x0960).and_then(|d| crate::chunks::i32_at(d, 0));
        match parent {
            Some(p) if p >= 0 && (p as usize) < n && p as usize != i && file.class_id(p as usize) == Some(NODE) => {
                children[p as usize].push(i);
            }
            _ => roots.push(i),
        }
    }
    // 2. The hierarchy from the roots.
    let mut stack = Vec::new();
    for r in roots {
        if let Some(s) = b.visit(r, &children, &mut stack)? {
            b.scene.roots.push(s);
        }
    }
    // 3. Max's system units to metres, and Z up to Y up.
    if !b.scene.roots.is_empty() {
        b.scene.wrap_roots(Transform {
            rotation: Quat::from_rotation_x(-std::f32::consts::FRAC_PI_2),
            scale: Vec3::splat(metres),
            ..Transform::IDENTITY
        });
    }
    let mut scene = b.scene;
    scene.materials = std::mem::take(&mut b.materials.list);
    scene.textures = std::mem::take(&mut b.materials.textures.textures);
    scene.info.warnings.append(&mut b.materials.textures.warnings);
    b.report.materials = scene.materials.len();
    b.report.unknown_materials = std::mem::take(&mut b.materials.unknown);
    Ok((scene, b.report))
}

/// An object, its material, and the wire colour it is drawn in when it
/// has none.
type MeshKey = (usize, Option<usize>, Option<[u8; 3]>);

struct Builder<'f, 'a> {
    file: &'f MaxFile<'a>,
    limits: &'f Limits,
    scene: Scene,
    report: Report,
    materials: Materials<'f, 'a>,
    /// Scene mesh of each object as drawn, built once.
    meshes: HashMap<MeshKey, Option<usize>>,
    triangles: u64,
}

impl Builder<'_, '_> {
    fn visit(
        &mut self,
        node: usize,
        children: &[Vec<usize>],
        stack: &mut Vec<usize>,
    ) -> Result<Option<usize>, LoadError> {
        if stack.contains(&node) || stack.len() > MAX_DEPTH || self.scene.nodes.len() >= MAX_NODES {
            return Ok(None);
        }
        stack.push(node);
        let file = self.file;
        let name = file.label(node).unwrap_or_default();
        let refs = file.refs(node);
        let mut s = SceneNode::new(name.clone());
        s.transform = refs.first().copied().flatten().map(|c| self.prs(c)).unwrap_or(Transform::IDENTITY);
        // The node's flags follow its parent in 0x0960; 0x400 is set on
        // hidden nodes.
        let flags = file.objects[node].child_data(0x0960).and_then(|d| crate::chunks::u32_at(d, 4)).unwrap_or(0);
        s.visible = flags & HIDDEN == 0;
        let index = self.scene.nodes.len();
        self.scene.nodes.push(s);
        let mut kids = Vec::new();
        // The object, at the node's object offset.
        if let Some(obj) = refs.get(1).copied().flatten() {
            let mtl = refs.get(3).copied().flatten();
            // Without a material, Max draws the object in its wire colour
            // (0x0974, red in the low byte).
            let wire = file.objects[node]
                .child_data(0x0974)
                .and_then(|d| crate::chunks::u32_at(d, 0))
                .map(|c| [c as u8, (c >> 8) as u8, (c >> 16) as u8]);
            if let Some(m) = self.mesh(obj, mtl, wire, &name)? {
                let offset = self.offset(node);
                if offset == Transform::IDENTITY {
                    self.scene.nodes[index].mesh = Some(m);
                } else {
                    let mut o = SceneNode::new(format!("{name} (object offset)"));
                    o.transform = offset;
                    o.mesh = Some(m);
                    self.scene.nodes.push(o);
                    kids.push(self.scene.nodes.len() - 1);
                }
            }
        }
        for &c in &children[node] {
            if let Some(k) = self.visit(c, children, stack)? {
                kids.push(k);
            }
        }
        stack.pop();
        if kids.is_empty() && self.scene.nodes[index].mesh.is_none() {
            if self.scene.nodes.len() == index + 1 {
                self.scene.nodes.pop();
            }
            return Ok(None);
        }
        self.scene.nodes[index].children = kids;
        Ok(Some(index))
    }

    /// The node's object offset: position, rotation, and scale.
    fn offset(&self, node: usize) -> Transform {
        let c = &self.file.objects[node];
        let v = |id: u16, n: usize| {
            c.child_data(id).and_then(|d| crate::chunks::floats(d, 0, n)).filter(|v| v.iter().all(|x| x.is_finite()))
        };
        let mut t = Transform::IDENTITY;
        if let Some(p) = v(0x096A, 3) {
            t.translation = Vec3::new(p[0], p[1], p[2]);
        }
        if let Some(q) = v(0x096B, 4) {
            t.rotation = max_quat(&q);
        }
        if let Some(s) = v(0x096C, 3) {
            t.scale = Vec3::new(s[0], s[1], s[2]);
        }
        t
    }

    /*┌─────────────────╮
    | Transforms      |
    └─────────────────╯*/
    //region Transforms

    /// A node's transform from its Position/Rotation/Scale controller (or
    /// any controller whose references are position, rotation and scale).
    fn prs(&self, ctrl: usize) -> Transform {
        let file = self.file;
        let refs = file.refs(ctrl);
        let get = |k: usize| refs.get(k).copied().flatten();
        let t = Transform {
            translation: get(0).and_then(|c| self.position(c, 0)).unwrap_or(Vec3::ZERO),
            rotation: get(1).and_then(|c| self.rotation(c, 0)).unwrap_or(Quat::IDENTITY),
            scale: get(2).and_then(|c| self.scale(c, 0)).unwrap_or(Vec3::ONE),
        };
        let ok = t.translation.is_finite() && t.rotation.is_finite() && t.scale.is_finite();
        if ok { t } else { Transform::IDENTITY }
    }

    fn floats3(&self, c: usize) -> Option<Vec3> {
        let file = self.file;
        let refs = file.refs(c);
        let f = |k: usize| {
            refs.get(k).copied().flatten().and_then(|r| controller_values(file, r)).and_then(|v| v.first().copied())
        };
        Some(Vec3::new(f(0).unwrap_or(0.0), f(1).unwrap_or(0.0), f(2).unwrap_or(0.0)))
    }

    fn position(&self, c: usize, depth: u32) -> Option<Vec3> {
        let file = self.file;
        match file.class_id(c)? {
            POSITION_XYZ => self.floats3(c),
            POSITION_LIST if depth < 8 => Some(
                file.refs(c)
                    .into_iter()
                    .flatten()
                    .filter(|&r| file.super_id(r) == Some(super_class::POSITION_CONTROL))
                    .filter_map(|r| self.position(r, depth + 1))
                    .sum(),
            ),
            _ => controller_values(file, c).filter(|v| v.len() >= 3).map(|v| Vec3::new(v[0], v[1], v[2])),
        }
    }

    fn rotation(&self, c: usize, depth: u32) -> Option<Quat> {
        let file = self.file;
        match file.class_id(c)? {
            EULER_XYZ => {
                let a = self.floats3(c)?;
                // Max's orders 0-5 are XYZ, XZY, YZX, YXZ, ZXY, ZYX: the
                // first axis turns first.
                let order = file.objects[c].child_data(0x1003).and_then(|d| crate::chunks::i32_at(d, 0)).unwrap_or(0);
                let (x, y, z) = (Quat::from_rotation_x(a.x), Quat::from_rotation_y(a.y), Quat::from_rotation_z(a.z));
                Some(match order {
                    1 => y * z * x,
                    2 => x * z * y,
                    3 => z * x * y,
                    4 => y * x * z,
                    5 => x * y * z,
                    _ => z * y * x,
                })
            }
            ROTATION_LIST if depth < 8 => Some(
                file.refs(c)
                    .into_iter()
                    .flatten()
                    .filter(|&r| file.super_id(r) == Some(super_class::ROTATION_CONTROL))
                    .filter_map(|r| self.rotation(r, depth + 1))
                    .fold(Quat::IDENTITY, |acc, q| q * acc),
            ),
            _ => controller_values(file, c).filter(|v| v.len() >= 4).map(|v| max_quat(&v)),
        }
    }

    fn scale(&self, c: usize, depth: u32) -> Option<Vec3> {
        let file = self.file;
        match file.class_id(c)? {
            SCALE_XYZ => {
                let refs = file.refs(c);
                let f = |k: usize| {
                    refs.get(k)
                        .copied()
                        .flatten()
                        .and_then(|r| controller_values(file, r))
                        .and_then(|v| v.first().copied())
                };
                Some(Vec3::new(f(0).unwrap_or(1.0), f(1).unwrap_or(1.0), f(2).unwrap_or(1.0)))
            }
            SCALE_LIST if depth < 8 => Some(
                file.refs(c)
                    .into_iter()
                    .flatten()
                    .filter(|&r| file.super_id(r) == Some(super_class::SCALE_CONTROL))
                    .filter_map(|r| self.scale(r, depth + 1))
                    .product(),
            ),
            _ => controller_values(file, c).filter(|v| v.len() >= 3).map(|v| Vec3::new(v[0], v[1], v[2])),
        }
    }

    //endregion

    /*┌─────────────────╮
    | Objects         |
    └─────────────────╯*/
    //region Objects

    /// The scene mesh of object `obj` drawn with material `mtl`.
    fn mesh(
        &mut self,
        obj: usize,
        mtl: Option<usize>,
        wire: Option<[u8; 3]>,
        node_name: &str,
    ) -> Result<Option<usize>, LoadError> {
        let wire = if mtl.is_none() { wire } else { None };
        if let Some(&m) = self.meshes.get(&(obj, mtl, wire)) {
            return Ok(m);
        }
        let built = match self.evaluate(obj, 0, node_name)? {
            Some(poly) => self.add_mesh(poly, mtl, wire, node_name)?,
            None => None,
        };
        self.meshes.insert((obj, mtl, wire), built);
        Ok(built)
    }

    /// An object's geometry: a base object, or a derived object's base
    /// with the modifiers Slerp applies.
    fn evaluate(&mut self, obj: usize, depth: u32, node_name: &str) -> Result<Option<PolyMesh>, LoadError> {
        let file = self.file;
        if depth > 16 {
            return Ok(None);
        }
        if file.is_derived(obj) {
            // Modifiers first (top of the stack first), the base last.
            let refs: Vec<usize> = file.refs(obj).into_iter().flatten().collect();
            let Some((&base, modifiers)) = refs.split_last() else { return Ok(None) };
            let Some(mut poly) = self.evaluate(base, depth + 1, node_name)? else { return Ok(None) };
            for &m in modifiers.iter().rev() {
                self.modify(m, &mut poly, node_name)?;
            }
            return Ok(Some(poly));
        }
        let class = file.class_id(obj);
        let chunk = &file.objects[obj];
        let poly = match class {
            Some(EDITABLE_MESH) => mesh::tri_object(chunk),
            Some(EDITABLE_POLY) => mesh::poly_object(chunk),
            _ => match prims::build(file, obj) {
                Some((p, kind)) => {
                    self.report.primitives.push(format!("{node_name} ({kind})"));
                    Some(p)
                }
                None => {
                    // Lights, cameras, helpers, shapes, and geometry types
                    // Slerp can't rebuild.
                    *self.report.not_drawn.entry(file.name_of(obj).to_string()).or_default() += 1;
                    None
                }
            },
        };
        let Some(mut poly) = poly else { return Ok(None) };
        poly.validate();
        self.check_triangles(poly.triangle_count() as u64)?;
        Ok(Some(poly))
    }

    /// Applies one modifier when Slerp can, or notes it.
    fn modify(&mut self, m: usize, poly: &mut PolyMesh, node_name: &str) -> Result<(), LoadError> {
        let file = self.file;
        let name = file.name_of(m).to_string();
        if matches!(name.as_str(), "TurboSmooth" | "MeshSmooth" | "OpenSubdiv") {
            // Iterations: the first integer parameter, 1 when unreadable.
            let levels = file
                .refs(m)
                .into_iter()
                .flatten()
                .find(|&r| matches!(file.name_of(r), "ParamBlock2" | "ParamBlock"))
                .and_then(|pb| param_float(file, pb, 0))
                .filter(|v| (0.0..=10.0).contains(v))
                .map_or(1, |v| v as u32)
                .min(MAX_LEVELS);
            let after = (poly.triangle_count() as u64).saturating_mul(4u64.pow(levels));
            self.check_triangles(after.saturating_sub(poly.triangle_count() as u64))?;
            if levels > 0 {
                poly.validate();
                subdivide(
                    poly,
                    levels,
                    SubdivideOptions { simple: false, keep_corners: false, use_creases: false, limit: false },
                );
                // The result is smooth, as TurboSmooth's "smooth result"
                // makes it.
                poly.custom_normals = None;
                poly.smoothing = Smoothing::Groups;
                poly.face_groups.iter_mut().for_each(|g| *g = 1);
                self.report.smoothed.push(format!("{node_name} ({name} x{levels})"));
            }
            return Ok(());
        }
        *self.report.skipped_modifiers.entry(name).or_default() += 1;
        Ok(())
    }

    fn check_triangles(&mut self, more: u64) -> Result<(), LoadError> {
        self.triangles = self.triangles.saturating_add(more);
        if self.triangles > self.limits.max_triangles {
            return Err(LoadError::TooLarge(format!("more than {} triangles", self.limits.max_triangles)));
        }
        Ok(())
    }

    /// Turns geometry into a scene mesh with the node's material.
    fn add_mesh(
        &mut self,
        poly: PolyMesh,
        mtl: Option<usize>,
        wire: Option<[u8; 3]>,
        name: &str,
    ) -> Result<Option<usize>, LoadError> {
        if poly.face_count() == 0 {
            return Ok(None);
        }
        let slot: Box<dyn Fn(u32) -> usize> = match wire {
            Some(rgb) => {
                let m = self.materials.wire(rgb);
                Box::new(move |_| m)
            }
            None => self.materials.slots(mtl),
        };
        let normals = corner_normals(&poly);
        let uv = (!poly.uvs.is_empty()).then_some(0);
        let prims = primitives(&poly, &normals, &|_| Wants { uv: [uv, None], color: None });
        let mut mesh = Mesh { name: name.to_string(), ..Default::default() };
        for (id, mut p) in prims {
            p.material = Some(slot(id as u32));
            mesh.primitives.push(p);
        }
        if mesh.primitives.is_empty() {
            return Ok(None);
        }
        self.report.meshes += 1;
        self.scene.meshes.push(mesh);
        Ok(Some(self.scene.meshes.len() - 1))
    }

    //endregion
}

/// A 3ds Max quaternion (x, y, z, w), which turns the opposite way from
/// the usual convention, as a rotation.
fn max_quat(v: &[f32]) -> Quat {
    let q = Quat::from_xyzw(v[0], v[1], v[2], v[3]);
    q.try_normalize().map_or(Quat::IDENTITY, |q| q.conjugate())
}

trait TryNormalize: Sized {
    fn try_normalize(self) -> Option<Self>;
}

impl TryNormalize for Quat {
    fn try_normalize(self) -> Option<Quat> {
        let len = self.length();
        (len.is_finite() && len > 1e-12).then(|| self / len)
    }
}
