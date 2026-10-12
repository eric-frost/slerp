//! The active scene's objects as the shared scene model: which objects the
//! scene shows, their transforms and hierarchy, collection instances, the
//! collections they are organised in, and a mesh per object with its
//! modifiers applied.
//!
//! Blender 2.8 and later organise objects in collections, shown or hidden
//! per view layer; earlier versions had twenty scene layers.  Objects
//! hidden in the viewport or in renders become hidden nodes, and excluded
//! collections' objects are left out, so the scene looks as it renders.

use crate::file::{BlendFile, GLOB};
use crate::link::Libraries;
use crate::material::Materials;
use crate::mesh::{self, MeshData};
use crate::modifiers;
use crate::normals;
use crate::poly::PolyMesh;
use crate::triangulate::{self, Wants};
use crate::view::Obj;
use glam::{Mat4, Quat, Vec3, Vec4};
use slerp_scene::{Collection, Limits, Marker, Material, Mesh, Node, Scene, Transform};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;
use std::rc::Rc;

/// Instancing deeper than this is cut off (collections may instance
/// themselves).
const MAX_INSTANCE_DEPTH: usize = 8;
/// The most nodes a scene may grow to through instancing.
const MAX_NODES: usize = 200_000;

/// Object types, from DNA_object_types.h.
const OB_EMPTY: i64 = 0;
const OB_MESH: i64 = 1;
const OB_LAMP: i64 = 10;
const OB_CAMERA: i64 = 11;
const OB_ARMATURE: i64 = 25;

/*┌─────────────────╮
| Scene contents  |
└─────────────────╯*/
//region Scene contents

/// An object the scene contains, and whether it shows.
struct Member<'a> {
    ob: Obj<'a>,
    visible: bool,
}

/// A collection of the scene's collection tree: its objects (indices into
/// the members) and the collections inside it (indices into the list).
struct Group {
    name: String,
    members: Vec<usize>,
    children: Vec<usize>,
}

/// The scene to show: the one the file was saved with, else the first.
fn active_scene<'a>(f: &'a BlendFile) -> Option<Obj<'a>> {
    let glob = f.blocks.iter().position(|b| b.code == GLOB).and_then(|i| Obj::of_block(f, i as u32));
    if let Some(sce) = glob.and_then(|g| g.deref("curscene")).filter(|o| o.type_name() == "Scene") {
        return Some(sce);
    }
    let i = f.blocks.iter().position(|b| b.is_id() && b.id_code() == *b"SC")?;
    Obj::of_block(f, i as u32)
}

/// The address of the structure `o` starts at.
pub fn addr(o: &Obj) -> u64 {
    let b = o.f.block(o.block);
    b.old + (o.at - b.start) as u64
}

/// The objects of scene `sce` with their visibility, and the collections
/// they are organised in (none before Blender 2.8).
fn members<'a>(f: &'a BlendFile, libs: &'a Libraries, sce: &Obj<'a>) -> (Vec<Member<'a>>, Vec<Group>) {
    let mut out: Vec<Member> = Vec::new();
    let mut groups: Vec<Group> = Vec::new();
    let mut index: HashMap<(usize, u64), usize> = HashMap::new();
    let mut add = |ob: Obj<'a>, visible: bool, out: &mut Vec<Member<'a>>| -> Option<usize> {
        // Objects linked from libraries come back as their library's own.
        let ob = libs.resolve(&ob).unwrap_or(ob);
        if ob.type_name() != "Object" {
            return None;
        }
        let key = (ob.f as *const BlendFile as usize, addr(&ob));
        let shown = visible && object_shows(&ob);
        match index.get(&key) {
            Some(&i) => {
                out[i].visible |= shown;
                Some(i)
            }
            None => {
                index.insert(key, out.len());
                out.push(Member { ob, visible: shown });
                Some(out.len() - 1)
            }
        }
    };
    if let Some(master) = sce.deref("master_collection") {
        // 1. Blender 2.8 and later: the collection tree, with the active
        //    view layer's exclusions and hidden collections and objects.
        let layers = sce.list("view_layers");
        let glob = f.blocks.iter().position(|b| b.code == GLOB).and_then(|i| Obj::of_block(f, i as u32));
        let current = glob.map_or(0, |g| g.ptr("cur_view_layer"));
        let layer = layers.iter().find(|l| addr(l) == current).or(layers.first()).copied();
        let mut layer_flags: HashMap<u64, i64> = HashMap::new();
        if let Some(l) = &layer {
            let mut stack: Vec<Obj> = l.list("layer_collections");
            let mut steps = 0;
            while let Some(lc) = stack.pop() {
                steps += 1;
                if steps > 100_000 {
                    break;
                }
                layer_flags.insert(lc.ptr("collection"), lc.int(&["flag"]).unwrap_or(0));
                stack.extend(lc.list("layer_collections"));
            }
        }
        let hidden_bases: HashSet<u64> = layer
            .map(|l| {
                l.list("object_bases")
                    .iter()
                    .filter(|b| b.int(&["flag"]).unwrap_or(0) & (1 << 8) != 0)
                    .map(|b| b.ptr("object"))
                    .collect()
            })
            .unwrap_or_default();
        // Each collection walked: its place in `groups` (none for the
        // scene's own master collection and excluded ones).
        let mut walked: HashMap<u64, Option<usize>> = HashMap::new();
        // (collection, shown, the group it sits in, is the master).
        let mut stack: Vec<(Obj, bool, Option<usize>, bool)> = vec![(master, true, None, true)];
        while let Some((c, shown, parent, is_master)) = stack.pop() {
            let key = addr(&c);
            if let Some(&group) = walked.get(&key) {
                // A collection inside several others is walked once and
                // listed in each.
                if let (Some(p), Some(g)) = (parent, group)
                    && !groups[p].children.contains(&g)
                {
                    groups[p].children.push(g);
                }
                continue;
            }
            if walked.len() > 100_000 {
                continue;
            }
            let lflag = layer_flags.get(&key).copied().unwrap_or(0);
            // Excluded collections aren't in the view layer at all.
            if lflag & (1 << 4) != 0 {
                walked.insert(key, None);
                continue;
            }
            let group = (!is_master).then(|| {
                groups.push(Group { name: c.id_name(), members: Vec::new(), children: Vec::new() });
                groups.len() - 1
            });
            walked.insert(key, group);
            if let (Some(p), Some(g)) = (parent, group) {
                groups[p].children.push(g);
            }
            let cflag = c.int(&["flag"]).unwrap_or(0);
            let shown = shown && lflag & (1 << 7) == 0 && cflag & (1 | 1 << 3) == 0;
            for co in c.list("gobject") {
                if let Some(ob) = co.deref("ob") {
                    let base_hidden = hidden_bases.contains(&addr(&ob));
                    let member = add(ob, shown && !base_hidden, &mut out);
                    if let (Some(g), Some(m)) = (group, member) {
                        groups[g].members.push(m);
                    }
                }
            }
            for child in c.list("children").into_iter().rev() {
                if let Some(cc) = child.deref("collection") {
                    stack.push((cc, shown, group, false));
                }
            }
        }
    } else {
        // 2. Before 2.8: the scene's bases, shown on the visible layers.
        let lay = sce.int(&["lay"]).unwrap_or(-1);
        for base in sce.list("base") {
            let Some(ob) = base.deref("object") else { continue };
            let blay = base.int(&["lay"]).or_else(|| ob.int(&["lay"])).unwrap_or(-1);
            add(ob, blay & lay != 0, &mut out);
        }
    }
    (out, groups)
}

/// Whether an object shows by its own settings: not hidden in the viewport
/// or in renders.  Objects the viewport draws as wireframes or boxes still
/// render solid, so they show; helpers such as boolean cutters are hidden
/// from renders too.
fn object_shows(ob: &Obj) -> bool {
    let restrict = ob.int(&["restrictflag", "visibility_flag"]).unwrap_or(0);
    restrict & (1 | 1 << 2) == 0
}

/// The scene's collections with their objects as nodes, each object once
/// per collection.
fn collections(groups: Vec<Group>, node_of: &[Option<usize>]) -> Vec<Collection> {
    groups
        .into_iter()
        .map(|g| {
            let mut seen = HashSet::new();
            let nodes = g.members.iter().filter_map(|&m| node_of.get(m).copied().flatten()).filter(|&n| seen.insert(n));
            Collection { name: g.name, nodes: nodes.collect(), children: g.children }
        })
        .collect()
}

/// The objects of a collection (or a pre-2.8 group), its child collections'
/// included, for instancing.  Collections hidden in renders or the viewport
/// are skipped, as Blender skips them when instancing.
fn collection_objects<'a>(c: &Obj<'a>) -> Vec<Obj<'a>> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    let mut stack = vec![*c];
    while let Some(c) = stack.pop() {
        if !seen.insert(addr(&c)) || seen.len() > 10_000 {
            continue;
        }
        if c.int(&["flag"]).unwrap_or(0) & (1 | 1 << 3) != 0 && seen.len() > 1 {
            continue;
        }
        out.extend(c.list("gobject").iter().filter_map(|co| co.deref("ob")).filter(|o| o.type_name() == "Object"));
        stack.extend(c.list("children").iter().filter_map(|ch| ch.deref("collection")));
    }
    out
}

//endregion

/*┌─────────────────╮
| Transforms      |
└─────────────────╯*/
//region Transforms

/// A 4x4 matrix field, which Blender stores row by row of its column-major
/// matrices: `m[i][j]` is column i, row j, as glam's columns.
fn mat4(o: &Obj, names: &[&str]) -> Option<Mat4> {
    let v = o.floats::<16>(names)?;
    let m = Mat4::from_cols_array(&v);
    m.is_finite().then_some(m)
}

/// An object's own transform, without its parent: location, rotation and
/// scale with their deltas.
fn basis(ob: &Obj) -> Mat4 {
    let v3 = |names: &[&str], d: Vec3| ob.floats::<3>(names).map(Vec3::from).filter(|v| v.is_finite()).unwrap_or(d);
    let loc = v3(&["loc"], Vec3::ZERO) + v3(&["dloc"], Vec3::ZERO);
    // Delta scale multiplies; before Blender 2.6 it was `dsize`, added.
    let delta = if ob.has("dscale") { v3(&["dscale"], Vec3::ONE) } else { v3(&["dsize"], Vec3::ZERO) + Vec3::ONE };
    let scale = v3(&["size", "scale"], Vec3::ONE) * delta;
    let mode = ob.int(&["rotmode"]).unwrap_or(1);
    let rot = |main: &str, delta: bool| -> Quat {
        match mode {
            0 => {
                let q = ob.floats::<4>(&[main]).map(|q| Quat::from_xyzw(q[1], q[2], q[3], q[0]));
                q.filter(|q| q.is_finite() && q.length_squared() > 1e-12).map_or(Quat::IDENTITY, |q| q.normalize())
            }
            -1 => {
                let (axis, angle) = if delta { ("drotAxis", "drotAngle") } else { ("rotAxis", "rotAngle") };
                let a = ob.floats::<3>(&[axis]).map(Vec3::from).unwrap_or(Vec3::Y);
                let t = ob.float(&[angle]).unwrap_or(0.0);
                a.try_normalize().map_or(Quat::IDENTITY, |a| Quat::from_axis_angle(a, t))
            }
            m => euler(ob.floats::<3>(&[main]).map(Vec3::from).unwrap_or(Vec3::ZERO), m),
        }
    };
    let (main, delta) = if mode == 0 { ("quat", "dquat") } else { ("rot", "drot") };
    let r = rot(delta, true) * rot(main, false);
    let r = if r.is_finite() { r } else { Quat::IDENTITY };
    Mat4::from_scale_rotation_translation(scale, r, loc)
}

/// A Blender Euler rotation in rotation mode `mode` (1 XYZ to 6 ZYX), whose
/// first named axis turns first, about the fixed axes.
fn euler(e: Vec3, mode: i64) -> Quat {
    let order: [usize; 3] = match mode {
        2 => [0, 2, 1],
        3 => [1, 0, 2],
        4 => [1, 2, 0],
        5 => [2, 0, 1],
        6 => [2, 1, 0],
        _ => [0, 1, 2],
    };
    let axis = |i: usize| Quat::from_axis_angle([Vec3::X, Vec3::Y, Vec3::Z][i], e[i]);
    axis(order[2]) * axis(order[1]) * axis(order[0])
}

/// An object's world matrix through its parents, without the cache that
/// scene building uses, for modifiers that refer to other objects.
pub fn world_of(ob: &Obj) -> Mat4 {
    Worlds { f: ob.f, done: HashMap::new(), unevaluated: 0 }.get(ob, 0)
}

/// World matrices of objects, computed once each through their parents.
struct Worlds<'a> {
    f: &'a BlendFile,
    done: HashMap<u64, Mat4>,
    /// Objects with constraints and no stored world matrix.
    unevaluated: usize,
}

impl<'a> Worlds<'a> {
    fn get(&mut self, ob: &Obj<'a>, depth: usize) -> Mat4 {
        let key = addr(ob);
        if let Some(m) = self.done.get(&key) {
            return *m;
        }
        // Constraints (Child Of, Track To, ...) aren't evaluated here.
        // Files before Blender 4.2 store each object's evaluated world
        // matrix, which has them applied; later files don't.
        if !ob.list("constraints").is_empty() {
            if let Some(m) = mat4(ob, &["obmat", "object_to_world"]).filter(|m| m.determinant().abs() > 1e-12) {
                self.done.insert(key, m);
                return m;
            }
            self.unevaluated += 1;
        }
        let own = basis(ob);
        let world = match ob.deref("parent").filter(|p| p.type_name() == "Object" && depth < 64) {
            Some(parent) => {
                let pw = self.get(&parent, depth + 1) * self.parent_offset(ob, &parent);
                let inv = mat4(ob, &["parentinv"]).unwrap_or(Mat4::IDENTITY);
                pw * inv * own
            }
            None => own,
        };
        let world = if world.is_finite() { world } else { Mat4::IDENTITY };
        self.done.insert(key, world);
        world
    }

    /// Where a bone parent puts its child, in the parent armature's space:
    /// the bone's tail in its saved pose.
    fn parent_offset(&self, ob: &Obj, parent: &Obj) -> Mat4 {
        if ob.int(&["partype"]).unwrap_or(0) & 0xf != 7 {
            return Mat4::IDENTITY;
        }
        let bone = ob.string("parsubstr");
        let Some(pose) = parent.deref("pose") else { return Mat4::IDENTITY };
        let Some(ch) = pose.list("chanbase").into_iter().find(|c| c.string("name") == bone) else {
            return Mat4::IDENTITY;
        };
        let length = ch.deref("bone").and_then(|b| b.float(&["length"])).unwrap_or(0.0);
        let m = mat4(&ch, &["pose_mat"]).or_else(|| ch.deref("bone").and_then(|b| mat4(&b, &["arm_mat"])));
        let _ = self.f;
        m.unwrap_or(Mat4::IDENTITY) * Mat4::from_translation(Vec3::new(0.0, length, 0.0))
    }
}

//endregion

/*┌─────────────────╮
| Building        |
└─────────────────╯*/
//region Building

struct Builder<'a> {
    f: &'a BlendFile,
    libs: &'a Libraries,
    limits: &'a Limits,
    /// Linked collections already brought in, by (library file, collection
    /// address): the node that holds a copy of them.
    imported: HashMap<(usize, u64), usize>,
    scene: Scene,
    mats: Materials<'a>,
    worlds: Worlds<'a>,
    /// Meshes read, by Mesh data-block address.
    meshes: HashMap<u64, Option<Rc<MeshData>>>,
    /// Scene meshes by (mesh data-block, object when its modifiers or
    /// materials make it unique).
    built: HashMap<(u64, u64), Option<usize>>,
    /// Scene materials by Material data-block address (0 for none).
    materials: HashMap<u64, usize>,
    warnings: Vec<String>,
    skipped: BTreeMap<&'static str, usize>,
    unapplied: BTreeMap<String, usize>,
    applied: BTreeMap<String, usize>,
    triangles: u64,
    over_budget: bool,
}

/// Builds the scene of a parsed file, with the libraries it links.
pub fn build(
    f: &BlendFile,
    libs: &Libraries,
    limits: &Limits,
    dir: Option<&Path>,
) -> Result<Scene, slerp_scene::LoadError> {
    let sce = active_scene(f).ok_or_else(|| slerp_scene::LoadError::invalid("the file has no scene"))?;
    let mut b = Builder::new(f, libs, limits, dir);
    b.warnings.extend(libs.warnings.iter().cloned());
    b.scene.name = sce.id_name();
    let (members, groups) = members(f, libs, &sce);
    // 1. The scene's own objects, then those linked from each library,
    //    which are built from their own file and brought in.  Each
    //    member's node is noted for the collections.
    let mut node_of = vec![None; members.len()];
    let own: Vec<usize> = (0..members.len()).filter(|&i| std::ptr::eq(members[i].ob.f, f)).collect();
    let (roots, nodes) = b.add_members(&own.iter().map(|&i| &members[i]).collect::<Vec<_>>());
    own.iter().zip(nodes).for_each(|(&i, n)| node_of[i] = Some(n));
    b.scene.roots = roots;
    let mut files: Vec<&BlendFile> = Vec::new();
    for m in &members {
        if !std::ptr::eq(m.ob.f, f) && !files.iter().any(|x| std::ptr::eq(*x, m.ob.f)) {
            files.push(m.ob.f);
        }
    }
    for lf in files {
        let group: Vec<usize> = (0..members.len()).filter(|&i| std::ptr::eq(members[i].ob.f, lf)).collect();
        let dir = libs.dir_of(lf).flatten();
        let mut sub = Builder::new(lf, libs, limits, dir);
        sub.triangles = b.triangles;
        let (roots, nodes) = sub.add_members(&group.iter().map(|&i| &members[i]).collect::<Vec<_>>());
        let base = b.import(sub);
        group.iter().zip(nodes).for_each(|(&i, n)| node_of[i] = Some(n + base));
        b.scene.roots.extend(roots.into_iter().map(|r| r + base));
    }
    b.scene.collections = collections(groups, &node_of);
    b.hide_domes();
    // 2. Blender is Z up.
    b.scene.textures.extend(std::mem::take(&mut b.mats.out_textures));
    b.scene.convert_z_up();
    for m in &mut b.scene.markers {
        m.position = Vec3::new(m.position.x, m.position.z, -m.position.y);
    }
    // 3. What the viewer's inspector shows.
    b.finish_info(&sce);
    Ok(b.scene)
}

impl<'a> Builder<'a> {
    fn new(f: &'a BlendFile, libs: &'a Libraries, limits: &'a Limits, dir: Option<&Path>) -> Builder<'a> {
        Builder {
            f,
            libs,
            limits,
            imported: HashMap::new(),
            scene: Scene::default(),
            mats: Materials::new(f, limits, dir),
            worlds: Worlds { f, done: HashMap::new(), unevaluated: 0 },
            meshes: HashMap::new(),
            built: HashMap::new(),
            materials: HashMap::new(),
            warnings: f.warnings.clone(),
            skipped: BTreeMap::new(),
            unapplied: BTreeMap::new(),
            applied: BTreeMap::new(),
            triangles: 0,
            over_budget: false,
        }
    }

    /// Adds a node per object, parented as in Blender when the parent is
    /// among them too, and returns the nodes without a parent and the node
    /// of each object.
    fn add_members(&mut self, members: &[&Member<'a>]) -> (Vec<usize>, Vec<usize>) {
        let mut node_of: HashMap<u64, usize> = HashMap::new();
        for m in members {
            let i = self.object_node(&m.ob, m.visible, 0);
            node_of.insert(addr(&m.ob), i);
        }
        let mut children = HashSet::new();
        for m in members {
            let i = node_of[&addr(&m.ob)];
            let world = self.worlds.get(&m.ob, 0);
            let parent = m.ob.deref("parent").and_then(|p| node_of.get(&addr(&p)).map(|&pi| (p, pi)));
            let local = match &parent {
                Some((p, _)) => self.worlds.get(p, 0).inverse() * world,
                None => world,
            };
            self.scene.nodes[i].transform = Transform::from_matrix(local);
            if let Some((_, pi)) = parent {
                self.scene.nodes[pi].children.push(i);
                children.insert(i);
            }
        }
        let nodes: Vec<usize> = members.iter().map(|m| node_of[&addr(&m.ob)]).collect();
        (nodes.iter().copied().filter(|i| !children.contains(i)).collect(), nodes)
    }

    /// Hides a sky dome: a mesh that encloses everything else visible and
    /// is far larger.  Scenes paint skies on such spheres; seen from
    /// outside, as thumbnails and the viewer's orbit camera see a scene,
    /// the dome is all there is to see.
    fn hide_domes(&mut self) {
        let pose = slerp_scene::Pose::rest(&self.scene);
        let bounds: Vec<Option<(Vec3, Vec3)>> = (0..self.scene.nodes.len())
            .map(|i| {
                let n = &self.scene.nodes[i];
                let m = self.scene.meshes.get(n.mesh?)?;
                let (mut lo, mut hi) = (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN));
                for p in m.primitives.iter().flat_map(|p| &p.positions) {
                    let w = pose.world[i].transform_point3(*p);
                    lo = lo.min(w);
                    hi = hi.max(w);
                }
                (n.visible && lo.x <= hi.x).then_some((lo, hi))
            })
            .collect();
        let shown: Vec<usize> = (0..bounds.len()).filter(|&i| bounds[i].is_some()).collect();
        if shown.len() < 2 {
            return;
        }
        let size = |(lo, hi): (Vec3, Vec3)| (hi - lo).length();
        for &d in &shown {
            let (dlo, dhi) = bounds[d].unwrap();
            let (mut lo, mut hi) = (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN));
            for &o in shown.iter().filter(|&&o| o != d) {
                let (a, b) = bounds[o].unwrap();
                lo = lo.min(a);
                hi = hi.max(b);
            }
            let encloses = dlo.cmple(lo).all() && dhi.cmpge(hi).all();
            // Flat things (ground planes) never count: a dome is big in
            // every direction.
            let round = (dhi - dlo).min_element() > 0.5 * (dhi - dlo).max_element();
            // Painted skies glow rather than reflect light.
            let glows = self.scene.nodes[d].mesh.and_then(|m| self.scene.meshes.get(m)).is_some_and(|m| {
                m.primitives.iter().all(|p| {
                    p.material.and_then(|i| self.scene.materials.get(i)).is_some_and(|mat| {
                        mat.unlit
                            || (mat.base_color.truncate().max_element() < 0.01 && mat.emissive.max_element() > 0.0)
                    })
                })
            });
            let ratio = size((dlo, dhi)) / size((lo, hi)).max(1e-6);
            if encloses && round && (ratio > 8.0 || glows && ratio > 1.2) {
                self.scene.nodes[d].visible = false;
                self.warnings.push(format!("\"{}\", a dome around the scene, is hidden", self.scene.nodes[d].name));
            }
        }
    }

    /// Moves everything `sub` built (from a library file) into this scene,
    /// and returns the index its first node now has.
    fn import(&mut self, mut sub: Builder<'a>) -> usize {
        // Every texture stays in the materials' list until the scene is
        // finished, so the library's go after this builder's own.
        let (nodes, meshes, mats, texs) =
            (self.scene.nodes.len(), self.scene.meshes.len(), self.scene.materials.len(), self.mats.out_textures.len());
        self.mats.out_textures.extend(std::mem::take(&mut sub.mats.out_textures));
        for mut n in sub.scene.nodes {
            n.children.iter_mut().for_each(|c| *c += nodes);
            n.mesh = n.mesh.map(|m| m + meshes);
            self.scene.nodes.push(n);
        }
        for mut m in sub.scene.meshes {
            for p in &mut m.primitives {
                p.material = p.material.map(|i| i + mats);
            }
            self.scene.meshes.push(m);
        }
        for mut m in sub.scene.materials {
            for t in [
                &mut m.base_color_texture,
                &mut m.metallic_roughness_texture,
                &mut m.normal_texture,
                &mut m.occlusion_texture,
                &mut m.emissive_texture,
                &mut m.transmission_texture,
                &mut m.thickness_texture,
                &mut m.lightmap_texture,
                &mut m.height_texture,
            ]
            .into_iter()
            .flatten()
            {
                t.texture += texs;
            }
            self.scene.materials.push(m);
        }
        self.scene.markers.extend(sub.scene.markers);
        self.warnings.extend(sub.warnings);
        for (k, v) in sub.applied {
            *self.applied.entry(k).or_default() += v;
        }
        for (k, v) in sub.unapplied {
            *self.unapplied.entry(k).or_default() += v;
        }
        for (k, v) in sub.skipped {
            *self.skipped.entry(k).or_default() += v;
        }
        self.worlds.unevaluated += sub.worlds.unevaluated;
        self.triangles = self.triangles.max(sub.triangles);
        self.over_budget |= sub.over_budget;
        nodes
    }

    /// A copy of node `i` and everything under it, for another instance of
    /// a collection already brought in.
    fn copy_subtree(&mut self, i: usize) -> usize {
        let mut node = self.scene.nodes[i].clone();
        let children = std::mem::take(&mut node.children);
        self.scene.nodes.push(node);
        let copy = self.scene.nodes.len() - 1;
        for c in children {
            if self.scene.nodes.len() >= MAX_NODES {
                break;
            }
            let cc = self.copy_subtree(c);
            self.scene.nodes[copy].children.push(cc);
        }
        copy
    }

    /// Adds a node for object `ob` (and its collection instance) and
    /// returns its index.  The caller sets its transform.
    fn object_node(&mut self, ob: &Obj<'a>, visible: bool, depth: usize) -> usize {
        let mut node = Node::new(ob.id_name());
        node.visible = visible;
        let ty = ob.int(&["type"]).unwrap_or(-1);
        match ty {
            OB_MESH => node.mesh = self.mesh_for(ob),
            OB_EMPTY | OB_ARMATURE => {}
            OB_LAMP | OB_CAMERA => {
                if visible && depth == 0 {
                    let p = self.worlds.get(ob, 0).w_axis.truncate();
                    let (label, color) = if ty == OB_LAMP {
                        let la = ob.deref("data");
                        let c = la.and_then(|l| Some(Vec3::new(l.float(&["r"])?, l.float(&["g"])?, l.float(&["b"])?)));
                        (format!("light: {}", ob.id_name()), c.unwrap_or(Vec3::ONE).extend(1.0))
                    } else {
                        (format!("camera: {}", ob.id_name()), Vec4::new(0.6, 0.6, 0.6, 1.0))
                    };
                    self.scene.markers.push(Marker { label, position: p, color });
                }
            }
            2 | 3 => *self.skipped.entry("curves and surfaces").or_default() += 1,
            4 => *self.skipped.entry("text").or_default() += 1,
            5 => *self.skipped.entry("metaballs").or_default() += 1,
            26 | 30 => *self.skipped.entry("grease pencil").or_default() += 1,
            27 => *self.skipped.entry("hair curves").or_default() += 1,
            28 => *self.skipped.entry("point clouds").or_default() += 1,
            29 => *self.skipped.entry("volumes").or_default() += 1,
            _ => {}
        }
        if !ob.list("particlesystem").is_empty() {
            *self.skipped.entry("particle systems").or_default() += 1;
        }
        self.scene.nodes.push(node);
        let index = self.scene.nodes.len() - 1;
        // Collection instances hang under the instancing object.
        let transflag = ob.int(&["transflag"]).unwrap_or(0);
        if transflag & (1 << 8) != 0
            && let Some(c) = ob.deref("dup_group").or_else(|| ob.deref("instance_collection"))
        {
            self.instance(index, &c, visible, depth);
        }
        index
    }

    /// Adds the objects of collection `c` under node `parent`, placed
    /// relative to the collection's instance offset.
    fn instance(&mut self, parent: usize, c: &Obj<'a>, visible: bool, depth: usize) {
        if depth >= MAX_INSTANCE_DEPTH {
            return;
        }
        // A collection linked from a library is built from its own file
        // once, and copied for later instances.
        if let Some(real) = self.libs.resolve(c) {
            let key = (real.f as *const BlendFile as usize, addr(&real));
            let holder = match self.imported.get(&key) {
                Some(&h) => self.copy_subtree(h),
                None => {
                    let dir = self.libs.dir_of(real.f).flatten();
                    let mut sub = Builder::new(real.f, self.libs, self.limits, dir);
                    sub.triangles = self.triangles;
                    sub.scene.nodes.push(Node::new(c.id_name()));
                    sub.instance(0, &real, visible, depth);
                    let h = self.import(sub);
                    self.imported.insert(key, h);
                    h
                }
            };
            self.scene.nodes[parent].children.push(holder);
            return;
        }
        if c.type_name() == "ID" || c.sub("id").is_some_and(|id| id.ptr("lib") != 0) && c.list("gobject").is_empty() {
            self.warnings.push(format!("collection \"{}\" is linked from a file that wasn't found", c.id_name()));
            return;
        }
        let offset = c.floats::<3>(&["instance_offset", "dupli_ofs"]).map(Vec3::from).unwrap_or(Vec3::ZERO);
        let shift = Mat4::from_translation(-offset);
        for ob in collection_objects(c) {
            if self.scene.nodes.len() >= MAX_NODES {
                self.warnings.push(format!("instances stopped at {MAX_NODES} nodes"));
                return;
            }
            let shows = visible && object_shows(&ob);
            let i = self.object_node(&ob, shows, depth + 1);
            let world = self.worlds.get(&ob, 0);
            self.scene.nodes[i].transform = Transform::from_matrix(shift * world);
            self.scene.nodes[parent].children.push(i);
        }
    }

    /// The scene mesh for mesh object `ob`, with its modifiers applied and
    /// its material slots resolved.
    fn mesh_for(&mut self, ob: &Obj<'a>) -> Option<usize> {
        let data = ob.ptr("data");
        let unique = !ob.list("modifiers").is_empty() || ob_has_own_materials(ob);
        let key = (data, if unique { addr(ob) } else { 0 });
        if let Some(&m) = self.built.get(&key) {
            return m;
        }
        let built = self.build_mesh(ob, data);
        self.built.insert(key, built);
        built
    }

    fn build_mesh(&mut self, ob: &Obj<'a>, data: u64) -> Option<usize> {
        if self.over_budget {
            return None;
        }
        // 1. The mesh as stored, read once however many objects use it.
        let md = match self.meshes.get(&data) {
            Some(m) => m.clone(),
            None => {
                let me = self.f.resolve(data, self.f.sdna.struct_named("Mesh"));
                let m = match me {
                    Some(me) if me.type_name() == "Mesh" => mesh::read(self.f, &me, &mut self.warnings).map(|mut d| {
                        // Auto Smooth (before Blender 4.1) worked on the
                        // finished mesh, after modifiers, except that custom
                        // normals were stored relative to its fans on the
                        // mesh itself.
                        if let Some(a) = d.auto_smooth
                            && d.encoded_normals.is_some()
                        {
                            normals::mark_sharp_by_angle(&mut d.poly, a);
                            d.auto_smooth = None;
                        }
                        if let Some(enc) = d.encoded_normals.take() {
                            d.poly.custom_normals = Some(normals::decode_custom(&d.poly, &enc));
                        }
                        Rc::new(d)
                    }),
                    Some(other) if other.type_name() == "ID" => {
                        self.warnings.push(format!("object \"{}\" uses a mesh linked from another file", ob.id_name()));
                        None
                    }
                    _ => None,
                };
                self.meshes.insert(data, m.clone());
                m
            }
        }?;
        // 2. Modifiers, on a copy.
        let mut poly: PolyMesh = md.poly.clone();
        let budget = subdivision_budget(self.limits).saturating_sub(self.triangles);
        // Vertex group names lived on the object before Blender 3.0.
        let ob_groups: Vec<String>;
        let groups = if md.groups.is_empty() {
            ob_groups = ob.list("defbase").iter().map(|g| g.string("name")).collect();
            &ob_groups
        } else {
            &md.groups
        };
        let weights = modifiers::Weights { per_vertex: &md.weights, groups };
        let report = modifiers::apply(self.f, ob, &mut poly, budget, &weights);
        for m in report.applied {
            *self.applied.entry(m).or_default() += 1;
        }
        for m in report.skipped {
            *self.unapplied.entry(m).or_default() += 1;
        }
        self.warnings.extend(report.warnings);
        if let Some(a) = md.auto_smooth {
            normals::mark_sharp_by_angle(&mut poly, a);
        }
        let tris = poly.triangle_count() as u64;
        if self.triangles + tris > self.limits.max_triangles {
            self.over_budget = true;
            self.warnings.push(format!("stopped at {} triangles, the most allowed", self.triangles));
            return None;
        }
        self.triangles += tris;
        // 3. Normals, then primitives split by material slot.
        let normals = normals::corner_normals(&poly);
        let slots = material_slots(ob, &md);
        let built: Vec<Option<crate::material::Built>> = slots.iter().map(|&m| self.mats.get(m)).collect();
        let wants = |slot: usize| -> Wants {
            let Some(Some(b)) = built.get(slot) else { return Wants::default() };
            let textured = has_texture(&b.material);
            let find = |name: &Option<String>| -> Option<usize> {
                match name {
                    Some(n) => poly.uvs.iter().position(|l| &l.name == n).or(Some(poly.default_uv)),
                    None => Some(poly.default_uv),
                }
                .filter(|&i| i < poly.uvs.len())
            };
            let uv0 = if textured { find(&b.uv[0]) } else { None };
            let uv1 = if textured && b.uv1 { find(&b.uv[1]) } else { None };
            let color = b.color.as_ref().and_then(|n| {
                poly.colors
                    .iter()
                    .position(|l| !n.is_empty() && &l.name == n)
                    .or_else(|| (!poly.colors.is_empty()).then_some(poly.default_color.min(poly.colors.len() - 1)))
            });
            Wants { uv: [uv0, uv1], color }
        };
        let prims = triangulate::primitives(&poly, &normals, &wants);
        if prims.is_empty() {
            return None;
        }
        let mut mesh = Mesh { name: md_name(ob), ..Default::default() };
        for (slot, mut prim) in prims {
            let ma = slots.get(slot).copied().unwrap_or(0);
            prim.material = Some(self.material_index(ma, built.get(slot).cloned().flatten()));
            mesh.primitives.push(prim);
        }
        self.scene.meshes.push(mesh);
        Some(self.scene.meshes.len() - 1)
    }

    /// The scene material for Material data-block `ma` (0 for an empty
    /// slot), added once.
    fn material_index(&mut self, ma: u64, built: Option<crate::material::Built>) -> usize {
        if let Some(&i) = self.materials.get(&ma) {
            return i;
        }
        let material =
            built.map(|b| b.material).unwrap_or_else(|| Material { name: "default".into(), ..Default::default() });
        self.scene.materials.push(material);
        let i = self.scene.materials.len() - 1;
        self.materials.insert(ma, i);
        i
    }

    fn finish_info(&mut self, sce: &Obj) {
        let h = self.f.header;
        let info = &mut self.scene.info;
        info.format = format!("Blender {}.{}", h.version / 100, h.version % 100);
        if let Some(p) = self.f.packing.name() {
            info.format.push_str(&format!(" ({p})"));
        }
        info.details.push(("scene".into(), sce.id_name()));
        if !self.applied.is_empty() {
            let list: Vec<String> = self.applied.iter().map(|(k, v)| format!("{k} x{v}")).collect();
            info.details.push(("modifiers applied".into(), list.join(", ")));
        }
        if !self.unapplied.is_empty() {
            let list: Vec<String> = self.unapplied.iter().map(|(k, v)| format!("{k} x{v}")).collect();
            self.warnings.push(format!("modifiers not applied: {}", list.join(", ")));
        }
        if self.worlds.unevaluated > 0 {
            self.warnings.push(format!(
                "{} objects have constraints, which aren't evaluated, so they may be out of place",
                self.worlds.unevaluated
            ));
        }
        if !self.skipped.is_empty() {
            let list: Vec<String> = self.skipped.iter().map(|(k, v)| format!("{v} {k}")).collect();
            self.warnings.push(format!("not shown: {}", list.join(", ")));
        }
        self.warnings.extend(std::mem::take(&mut self.mats.warnings));
        let mut seen = HashSet::new();
        self.warnings.retain(|w| seen.insert(w.clone()));
        info.warnings = std::mem::take(&mut self.warnings);
    }
}

/// An object's material slots as Material addresses: the object's own
/// material where its slot says so, the mesh's otherwise.
fn material_slots(ob: &Obj, md: &MeshData) -> Vec<u64> {
    let ob_n = ob.int(&["totcol"]).unwrap_or(0).clamp(0, i16::MAX as i64) as usize;
    let ob_mats = ob.ptr_array("mat", ob_n);
    let bits = ob.raw("matbits").unwrap_or(&[]);
    let n = md.materials.len().max(ob_mats.len());
    (0..n)
        .map(|i| {
            let own = bits.get(i).is_some_and(|&b| b != 0);
            match (own, ob_mats.get(i), md.materials.get(i)) {
                (true, Some(&m), _) if m != 0 => m,
                (_, _, Some(&m)) => m,
                _ => 0,
            }
        })
        .collect()
}

fn ob_has_own_materials(ob: &Obj) -> bool {
    let n = ob.int(&["totcol"]).unwrap_or(0).clamp(0, i16::MAX as i64) as usize;
    ob.raw("matbits").is_some_and(|b| b.iter().take(n).any(|&x| x != 0))
}

fn md_name(ob: &Obj) -> String {
    ob.deref("data").map(|d| d.id_name()).unwrap_or_else(|| ob.id_name())
}

fn has_texture(m: &Material) -> bool {
    m.base_color_texture.is_some()
        || m.metallic_roughness_texture.is_some()
        || m.normal_texture.is_some()
        || m.emissive_texture.is_some()
}

/// Triangles subdivision may create in all: thumbnails get 2 million, the
/// viewer 8 million.
fn subdivision_budget(limits: &Limits) -> u64 {
    (limits.max_triangles / 10).clamp(500_000, 8_000_000)
}

//endregion
