//! UsdSkel: skeletons become joint nodes, skinned prims get Slerp skins,
//! blend shapes become morph targets, and the SkelAnimation each skeleton
//! is bound to (with any animated transforms) becomes a clip.
//!
//! USD writes matrices row by row for row vectors (`p' = p * M`); read
//! column by column into glam, the same numbers are the same transforms for
//! column vectors.  UsdSkel moves a point to
//! `p * geomBind * inverse(bind[j]) * skelSpace[j] * skelToWorld`, summed
//! over the point's joints by weight, while Slerp's skinning computes
//! `jointWorld[j] * inverseBind[j] * p`.  So the geom bind transform is
//! baked into the points, joint nodes hang under the Skeleton prim's node
//! (making `jointWorld = skelToWorld * skelSpace`), and a skin's inverse
//! bind matrices are the inverses of the skeleton's bind transforms.
//!
//! Binding properties (`skel:skeleton`, `skel:joints`, the joint index and
//! weight primvars, the geom bind transform and the blend shapes) are
//! inherited down the namespace, as UsdSkel's own cache does; a skeleton's
//! `skel:animationSource` may be authored on it or an ancestor.  Unlike
//! USD, a binding works outside a SkelRoot too, and a skeleton without rest
//! transforms takes them from its bind transforms.
//!
//! Joints, blend shapes and transforms start in the pose of the first time
//! sample (USD viewers open there), which is also what thumbnails draw.

use super::Value;
use super::geom::{Builder, op_stack};
use super::stage::{CPrim, Stage};
use slerp_scene::glam::{Mat3, Mat4, Quat, Vec3, Vec4};
use slerp_scene::{Animation, Channel, Interpolation, MorphTarget, Node, Primitive, Property, Skin, Transform};
use std::collections::{HashMap, HashSet};

/// The most joints a skeleton may have: vertices name joints with 16 bits,
/// and the largest real rigs (film faces, MetaHumans) have a few thousand.
const MAX_JOINTS: usize = 16_384;
/// The most influences per point a file may claim.  Slerp keeps the four
/// strongest of them.
const MAX_ELEMENT_SIZE: usize = 256;
/// The most morph targets one mesh may have, counting each inbetween shape,
/// and the most point offsets they may hold together (12 bytes each).
const MAX_TARGETS: usize = 1024;
const MAX_TARGET_POINTS: usize = 32 << 20;
/// The most keyframe values all of a stage's clips may hold together.
const MAX_KEY_FLOATS: usize = 64 << 20;
/// The most distinct times an animated transform is sampled at.
const MAX_XFORM_KEYS: usize = 100_000;

/*┌─────────────────╮
| Bindings        |
└─────────────────╯*/
//region Bindings

/// The UsdSkel binding a prim sees.  Each field is the prim that authored
/// the property nearest above (or at) it, except `skeleton`, which is the
/// Skeleton prim its `skel:skeleton` relationship targets.
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct Binding {
    skeleton: Option<usize>,
    joints: Option<usize>,
    indices: Option<usize>,
    weights: Option<usize>,
    geom_bind: Option<usize>,
    shapes: Option<usize>,
}

impl Binding {
    /// The binding `pi` sees: its own authored properties over its
    /// ancestors'.
    pub fn at(self, stage: &Stage, pi: usize) -> Binding {
        let p = &stage.prims[pi];
        let mut b = self;
        if let Some(t) = p.prop("skel:skeleton").and_then(|q| q.targets.first()) {
            b.skeleton = stage.find(t).filter(|&s| stage.prims[s].type_name == "Skeleton");
        }
        let authored = |name: &str| p.prop(name).is_some_and(|q| q.value.is_some());
        let set = |field: &mut Option<usize>, name: &str| {
            if authored(name) {
                *field = Some(pi);
            }
        };
        set(&mut b.joints, "skel:joints");
        set(&mut b.indices, "primvars:skel:jointIndices");
        set(&mut b.weights, "primvars:skel:jointWeights");
        set(&mut b.geom_bind, "primvars:skel:geomBindTransform");
        set(&mut b.shapes, "skel:blendShapes");
        b
    }
}

/// The SkelAnimation driving a skeleton: the `skel:animationSource` of the
/// skeleton or of its nearest ancestor that has one.
fn animation_of(stage: &Stage, skeleton: usize) -> Option<usize> {
    for a in stage.ancestry(skeleton).into_iter().rev() {
        if let Some(t) = stage.prims[a].prop("skel:animationSource").and_then(|q| q.targets.first()) {
            return stage.find(t).filter(|&x| stage.prims[x].type_name == "SkelAnimation");
        }
    }
    None
}

/// A joint path as written in `joints`, without stray slashes.
fn joint_path(s: &str) -> &str {
    s.trim_matches('/')
}

/// A skeleton's joint hierarchy: joint paths such as `Hips/Spine`, each
/// joint's parent (the joint whose path is its path less the last part),
/// and the index of each path.
struct Topology {
    joints: Vec<String>,
    parents: Vec<Option<usize>>,
    index: HashMap<String, usize>,
    /// The `joints` array's length as authored, before any cut.
    authored: usize,
}

impl Topology {
    fn read(p: &CPrim) -> Topology {
        let all = match p.value("joints") {
            Some(Value::Strs(j)) => j.as_slice(),
            _ => &[],
        };
        let joints: Vec<String> = all.iter().take(MAX_JOINTS).map(|j| joint_path(j).to_string()).collect();
        let mut index = HashMap::with_capacity(joints.len());
        for (i, j) in joints.iter().enumerate() {
            index.entry(j.clone()).or_insert(i);
        }
        // A parent path is always shorter, so the hierarchy has no cycles.
        let parents =
            joints.iter().map(|j| j.rsplit_once('/').and_then(|(parent, _)| index.get(parent).copied())).collect();
        Topology { joints, parents, index, authored: all.len() }
    }

    /// Skeleton joint indices for a list of joint paths (a SkelAnimation's
    /// or a mesh's `skel:joints`); None for paths the skeleton lacks.
    fn lookup(&self, paths: &[String]) -> Vec<Option<usize>> {
        paths.iter().map(|p| self.index.get(joint_path(p)).copied()).collect()
    }
}

/// The text values of a token array property.
fn strs<'p>(p: &'p CPrim, name: &str) -> Option<&'p [String]> {
    match p.value(name) {
        Some(Value::Strs(s)) => Some(s),
        _ => None,
    }
}

/// A numeric array property of `width` numbers per element.
fn floats<'p>(p: &'p CPrim, name: &str, width: usize) -> Option<&'p [f32]> {
    match p.value(name) {
        Some(Value::Floats(f, w)) if *w == width => Some(f),
        _ => None,
    }
}

/// `n` matrices from an array property holding exactly `authored` of them.
fn matrices(p: &CPrim, name: &str, authored: usize, n: usize) -> Option<Vec<Mat4>> {
    let f = floats(p, name, 16).filter(|f| f.len() == authored * 16)?;
    let m: Vec<Mat4> = f.chunks_exact(16).take(n).map(Mat4::from_cols_slice).collect();
    m.iter().all(|m| m.is_finite()).then_some(m)
}

/// One matrix-valued property, such as `primvars:skel:geomBindTransform`.
fn matrix(p: &CPrim, name: &str) -> Option<Mat4> {
    let m = match p.value(name)? {
        Value::Tuple(t) if t.len() == 16 => Mat4::from_cols_array(&std::array::from_fn(|i| t[i] as f32)),
        Value::Floats(f, _) if f.len() == 16 => Mat4::from_cols_slice(f),
        _ => return None,
    };
    m.is_finite().then_some(m)
}

/// A USD quaternion as stored here (real part first) as glam's.
fn quat(v: &[f32]) -> Quat {
    let q = Quat::from_xyzw(v[1], v[2], v[3], v[0]);
    if q.is_finite() && q.length_squared() > 1e-12 { q.normalize() } else { Quat::IDENTITY }
}

/// An inverse bind matrix, or identity for a bind transform that has none.
fn invert(m: Mat4) -> Mat4 {
    let inv = m.inverse();
    if m.determinant() != 0.0 && inv.is_finite() { inv } else { Mat4::IDENTITY }
}

//endregion

/*┌─────────────────╮
| Skeletons       |
└─────────────────╯*/
//region Skeletons

/// A skeleton made into nodes.
struct Skeleton {
    /// The Skeleton prim's node, which the joints hang under.
    root: usize,
    /// One node per joint.
    nodes: Vec<usize>,
    /// Skeleton-space bind transforms, per joint.
    bind: Vec<Mat4>,
    /// Local rest transforms, per joint.
    rest: Vec<Transform>,
    anim: Option<usize>,
}

/// What UsdSkel found while the stage was walked, finished by `finish`.
#[derive(Default)]
pub(super) struct Rig {
    topologies: HashMap<usize, Topology>,
    skeletons: HashMap<usize, Skeleton>,
    deformed: Vec<Deformed>,
    /// Nodes whose transforms are animated, and their prims.
    pub xforms: Vec<(usize, usize)>,
    /// Facts for the inspector.
    pub details: Vec<(String, String)>,
}

impl Rig {
    fn topology(&mut self, stage: &Stage, skeleton: usize) -> &Topology {
        self.topologies.entry(skeleton).or_insert_with(|| Topology::read(&stage.prims[skeleton]))
    }

    /// Remembers what `finish` must do for a node whose mesh `d` deformed.
    pub fn add_deformed(&mut self, node: usize, d: Deform) {
        let targets = d.target_names.len();
        self.deformed.push(Deformed { node, skin: d.skin, skeleton: d.skeleton, shapes: d.shapes, targets });
    }
}

/// Makes joint nodes under the Skeleton prim `pi`'s node.  Each joint
/// starts where the skeleton's animation has it at its first sample, or at
/// its rest transform.
pub(super) fn add_skeleton(b: &mut Builder, pi: usize, node: usize) {
    if b.rig.skeletons.contains_key(&pi) {
        return;
    }
    let stage = b.stage;
    let p = &stage.prims[pi];
    let topo = b.rig.topology(stage, pi);
    let n = topo.joints.len();
    let (parents, authored) = (topo.parents.clone(), topo.authored);
    if authored > n {
        b.ctx.warn(format!("{}: only the first {MAX_JOINTS} of {authored} joints were read", p.path));
    }

    // 1. Bind and rest transforms, each derived from the other when
    //    missing (USD itself refuses a skeleton without rest transforms).
    let bind = matrices(p, "bindTransforms", authored, n);
    let rest = matrices(p, "restTransforms", authored, n);
    let (bind, rest) = match (bind, rest) {
        (Some(bind), Some(rest)) => (bind, rest),
        (Some(bind), None) => {
            let rest = (0..n).map(|j| parents[j].map_or(bind[j], |pj| invert(bind[pj]) * bind[j])).collect();
            (bind, rest)
        }
        (None, rest) => {
            let rest = rest.unwrap_or_else(|| vec![Mat4::IDENTITY; n]);
            let mut bind = rest.clone();
            for j in 0..n {
                // Parents usually come first; a parent listed later is
                // still at rest here, which only matters for odd files.
                if let Some(pj) = parents[j].filter(|&pj| pj < j) {
                    bind[j] = bind[pj] * rest[j];
                }
            }
            (bind, rest)
        }
    };
    let rest: Vec<Transform> = rest.into_iter().map(Transform::from_matrix).collect();

    // 2. The starting pose: the animation's first values where it has them.
    let anim = animation_of(stage, pi);
    let mut local = rest.clone();
    if let Some(a) = anim {
        let topo = b.rig.topology(stage, pi);
        first_pose(&stage.prims[a], topo, &mut local);
    }

    // 3. One node per joint, under its parent joint or the skeleton.
    let names = strs(p, "jointNames").filter(|j| j.len() == authored);
    let topo = b.rig.topology(stage, pi);
    let first = b.scene.nodes.len();
    for j in 0..n {
        let name = match names {
            Some(names) => names[j].clone(),
            None => topo.joints[j].rsplit('/').next().unwrap_or_default().to_string(),
        };
        let mut joint = Node::new(name);
        joint.transform = local[j];
        b.scene.nodes.push(joint);
    }
    for (j, parent) in parents.iter().enumerate() {
        let parent = parent.map_or(node, |pj| first + pj);
        b.scene.nodes[parent].children.push(first + j);
    }
    let nodes = (first..first + n).collect();
    b.rig.skeletons.insert(pi, Skeleton { root: node, nodes, bind, rest, anim });
}

/// Builds a skeleton that the walk never reached (it sits under an
/// invisible scope, say) as a new root node placed where the prim is.
fn add_detached_skeleton(b: &mut Builder, pi: usize) {
    let mut node = Node::new(b.stage.prims[pi].name.clone());
    node.transform = Transform::from_matrix(b.world_matrix(pi));
    let ni = b.scene.nodes.len();
    b.scene.nodes.push(node);
    b.scene.roots.push(ni);
    add_skeleton(b, pi, ni);
}

/// The SkelAnimation's joint pairs: (index in the animation, skeleton
/// joint), for the joints the skeleton has.
fn anim_joints(anim: &CPrim, topo: &Topology) -> Vec<(usize, usize)> {
    let paths = strs(anim, "joints").unwrap_or_default();
    topo.lookup(paths).into_iter().enumerate().filter_map(|(a, j)| Some((a, j?))).collect()
}

/// Overwrites the joints a SkelAnimation moves with its values at the
/// first sample (or its default).  Arrays whose length doesn't match the
/// animation's joints are ignored, as USD ignores them.
fn first_pose(anim: &CPrim, topo: &Topology, local: &mut [Transform]) {
    let count = strs(anim, "joints").map_or(0, <[String]>::len);
    let t = floats(anim, "translations", 3).filter(|v| v.len() == count * 3);
    let r = floats(anim, "rotations", 4).filter(|v| v.len() == count * 4);
    let s = floats(anim, "scales", 3).filter(|v| v.len() == count * 3);
    for (a, j) in anim_joints(anim, topo) {
        if let Some(t) = t {
            local[j].translation = Vec3::from_slice(&t[a * 3..a * 3 + 3]);
        }
        if let Some(r) = r {
            local[j].rotation = quat(&r[a * 4..a * 4 + 4]);
        }
        if let Some(s) = s {
            local[j].scale = Vec3::from_slice(&s[a * 3..a * 3 + 3]);
        }
    }
}

//endregion

/*┌─────────────────╮
| Deformation     |
└─────────────────╯*/
//region Deformation

/// Up to four joints per point (indices into the mesh's joint order) and
/// their weights, which sum to one.
pub(super) enum Influences {
    /// The same joints for every point (constant interpolation).
    Rigid([u16; 4], Vec4),
    PerPoint(Vec<[u16; 4]>, Vec<Vec4>),
}

/// A skin to make once the skeleton's nodes exist: the skeleton and, for
/// each joint the mesh's influences name, the skeleton joint it is (None
/// when the skeleton lacks it; no weight names those).
pub(super) struct SkinRequest {
    skeleton: usize,
    joints: Vec<Option<usize>>,
}

/// A blend shape's position and normal offsets for every point of a mesh.
pub(super) struct PointTarget {
    offsets: Vec<Vec3>,
    normals: Vec<Vec3>,
}

/// One of a mesh's blend shapes: its name in `skel:blendShapes`, and the
/// shape weights at which each of its morph targets is fully applied.  The
/// first stop is the undeformed mesh at weight 0; inbetween shapes add stops
/// besides the primary shape's at weight 1.  Between two stops the mesh
/// blends linearly from one to the next, and beyond the ends it carries on
/// along the nearest pair, as UsdSkel does.
pub(super) struct ShapeMap {
    name: String,
    stops: Vec<(f32, Option<usize>)>,
}

impl ShapeMap {
    /// Adds this shape's morph target weights at shape weight `w` to `out`.
    fn weights(&self, w: f32, out: &mut [f32]) {
        let s = &self.stops;
        if s.len() < 2 || !w.is_finite() {
            return;
        }
        let i = s.partition_point(|stop| stop.0 <= w).clamp(1, s.len() - 1) - 1;
        let ((w0, a), (w1, b)) = (s[i], s[i + 1]);
        let t = (w - w0) / (w1 - w0);
        for (target, f) in [(a, 1.0 - t), (b, t)] {
            if let Some(slot) = target.and_then(|k| out.get_mut(k)) {
                *slot += f;
            }
        }
    }
}

/// How a gprim's points move: worked out while the gprim is built, applied
/// to each primitive by `apply`.
#[derive(Default)]
pub(super) struct Deform {
    /// Moves the points into the skeleton's bind space; identity for prims
    /// that are only morphed.
    geom_bind: Mat4,
    influences: Option<Influences>,
    skin: Option<SkinRequest>,
    /// The skeleton bound, whose animation drives the blend shapes.
    skeleton: Option<usize>,
    targets: Vec<PointTarget>,
    pub target_names: Vec<String>,
    shapes: Vec<ShapeMap>,
    /// For thumbnails: the blend shapes' offsets at the first sample, added
    /// to the points instead of becoming morph targets.
    baked: Vec<Vec3>,
}

/// A node whose mesh was deformed, as `finish` needs it.
struct Deformed {
    node: usize,
    skin: Option<SkinRequest>,
    skeleton: Option<usize>,
    shapes: Vec<ShapeMap>,
    /// Morph targets of the mesh.
    targets: usize,
}

/// How the gprim `pi`, of `points` points (0 for implicit shapes), is
/// skinned and morphed, or None when it is neither.
pub(super) fn deform(b: &mut Builder, pi: usize, bind: &Binding, points: usize) -> Option<Deform> {
    let mut d = Deform { geom_bind: Mat4::IDENTITY, skeleton: bind.skeleton, ..Default::default() };
    // 1. Joint influences, for a prim bound to a skeleton with joints.
    if let (Some(skeleton), Some(_), Some(_)) = (bind.skeleton, bind.indices, bind.weights) {
        match influences(b, bind, skeleton, points) {
            Ok(Some((influences, joints))) => {
                let geom_bind =
                    bind.geom_bind.and_then(|g| matrix(&b.stage.prims[g], "primvars:skel:geomBindTransform"));
                d.geom_bind = geom_bind.unwrap_or(Mat4::IDENTITY);
                d.influences = Some(influences);
                d.skin = Some(SkinRequest { skeleton, joints });
            }
            Ok(None) => {}
            Err(msg) => b.ctx.warn(format!("{}: {msg}; drawn unskinned", b.stage.prims[pi].path)),
        }
    }
    // 2. Blend shapes.
    if let Some(shapes) = bind.shapes.filter(|_| points > 0) {
        blend_shapes(b, pi, shapes, points, &mut d);
    }
    (d.skin.is_some() || !d.targets.is_empty() || !d.baked.is_empty()).then_some(d)
}

type Influenced = (Influences, Vec<Option<usize>>);

/// The joints and weights of each point, with the skeleton joint each of
/// the mesh's joints is.  Ok(None) for a skeleton without joints (one that
/// only drives blend shapes).
fn influences(b: &mut Builder, bind: &Binding, skeleton: usize, points: usize) -> Result<Option<Influenced>, String> {
    let stage = b.stage;
    let topo = b.rig.topology(stage, skeleton);
    if topo.joints.is_empty() {
        return Ok(None);
    }

    // 1. The mesh's joint order, as skeleton joints.
    let joints: Vec<Option<usize>> = match bind.joints.and_then(|j| strs(&stage.prims[j], "skel:joints")) {
        Some(paths) => topo.lookup(&paths[..paths.len().min(MAX_JOINTS)]),
        None => (0..topo.joints.len()).map(Some).collect(),
    };

    // 2. Indices and weights, with their element size and interpolation.
    let (Some(ip), Some(wp)) = (bind.indices, bind.weights) else { return Ok(None) };
    let iprop = stage.prims[ip].prop("primvars:skel:jointIndices").ok_or("no joint indices")?;
    let wprop = stage.prims[wp].prop("primvars:skel:jointWeights").ok_or("no joint weights")?;
    let (Some(Value::Ints(ix)), Some(Value::Floats(wt, 1))) = (iprop.value.as_deref(), wprop.value.as_deref()) else {
        return Err("joint indices or weights of the wrong type".into());
    };
    let size = iprop.element_size.unwrap_or(1);
    if wprop.element_size.unwrap_or(1) != size || !(1..=MAX_ELEMENT_SIZE as i64).contains(&size) {
        return Err(format!("joint weights' element size {size} is unusable"));
    }
    let size = size as usize;
    let pick = |e: usize| -> ([u16; 4], Vec4) {
        let mut best = [(0u16, 0.0f32); 4];
        for k in e * size..(e + 1) * size {
            let (j, w) = (ix[k], wt[k]);
            let named = usize::try_from(j).ok().and_then(|j| joints.get(j).copied().flatten()).is_some();
            if !(named && w > 0.0 && w.is_finite()) {
                continue;
            }
            // Keep the four strongest, strongest first.
            if let Some(at) = best.iter().position(|b| w > b.1) {
                best.copy_within(at..3, at + 1);
                best[at] = (j as u16, w);
            }
        }
        let sum: f32 = best.iter().map(|b| b.1).sum();
        let scale = if sum > 0.0 { 1.0 / sum } else { 0.0 };
        (best.map(|b| b.0), Vec4::from_array(best.map(|b| b.1 * scale)))
    };
    let influences = if iprop.interpolation.as_deref() == Some("constant") {
        if ix.len() < size || wt.len() < size {
            return Err("fewer joint weights than the element size".into());
        }
        let (j, w) = pick(0);
        Influences::Rigid(j, w)
    } else if points == 0 {
        return Err("per-point joint weights on an implicit shape".into());
    } else {
        let need = points.checked_mul(size).ok_or("too many joint weights")?;
        if ix.len() < need || wt.len() < need {
            return Err(format!("{} joint weights for {points} points of {size}", ix.len().min(wt.len())));
        }
        let (j, w) = (0..points).map(pick).unzip();
        Influences::PerPoint(j, w)
    };
    Ok(Some((influences, joints)))
}

/// Reads the blend shapes `skel:blendShapes` and `skel:blendShapeTargets`
/// on prim `sp` name into morph targets (or, for thumbnails, into offsets
/// baked in at the first sample).
fn blend_shapes(b: &mut Builder, pi: usize, sp: usize, points: usize, d: &mut Deform) {
    let stage = b.stage;
    let p = &stage.prims[sp];
    let names = strs(p, "skel:blendShapes").unwrap_or_default();
    let targets = p.prop("skel:blendShapeTargets").map_or(&[][..], |q| q.targets.as_slice());
    if names.len() != targets.len() {
        b.ctx.warn(format!(
            "{}: {} blend shape names for {} targets",
            stage.prims[pi].path,
            names.len(),
            targets.len()
        ));
    }
    let mut full = false;
    for (name, target) in names.iter().zip(targets) {
        let Some(shape) = stage.find(target).filter(|&s| stage.prims[s].type_name == "BlendShape") else {
            b.ctx.warn(format!("{}: blend shape {target} not found", stage.prims[pi].path));
            continue;
        };
        let shape = &stage.prims[shape];
        let indices = match shape.value("pointIndices") {
            Some(Value::Ints(ix)) => Some(ix.as_slice()),
            _ => None,
        };

        // 1. The primary shape at weight 1 and any inbetweens at theirs.
        let mut subs: Vec<SubShape> = Vec::new();
        if let Some(offsets) = floats(shape, "offsets", 3) {
            let normals = floats(shape, "normalOffsets", 3);
            subs.push(SubShape { weight: 1.0, name: name.clone(), offsets, normals });
        }
        for q in &shape.props {
            let Some(between) = q.name.strip_prefix("inbetweens:").filter(|n| !n.contains(':')) else { continue };
            let (Some(w), Some(Value::Floats(offsets, 3))) = (q.weight, q.value.as_deref()) else { continue };
            let weight = w as f32;
            if weight.is_finite() && weight != 0.0 && weight != 1.0 {
                let normals = floats(shape, &format!("{}:normalOffsets", q.name), 3);
                subs.push(SubShape { weight, name: format!("{name} {between}"), offsets, normals });
            }
        }

        // 2. A dense morph target for each, within the caps.
        let mut stops = vec![(0.0f32, None)];
        for sub in subs {
            if d.targets.len() >= MAX_TARGETS || (d.targets.len() + 1).saturating_mul(points) > MAX_TARGET_POINTS {
                full = true;
                break;
            }
            let normals = sub.normals.map(|n| scatter(n, indices, points)).unwrap_or_default();
            stops.push((sub.weight, Some(d.targets.len())));
            d.targets.push(PointTarget { offsets: scatter(sub.offsets, indices, points), normals });
            d.target_names.push(sub.name);
        }
        stops.sort_by(|a, b| a.0.total_cmp(&b.0));
        stops.dedup_by(|a, b| a.0 == b.0);
        d.shapes.push(ShapeMap { name: name.clone(), stops });
    }
    if full {
        b.ctx.warn(format!("{}: too many blend shapes; the rest were skipped", stage.prims[pi].path));
    }

    // 3. Thumbnails don't animate: add the first sample's offsets to the
    //    points instead.
    if !b.ctx.limits.animations {
        let anim = d.skeleton.and_then(|s| animation_of(stage, s));
        let weights = start_weights(stage, anim, &d.shapes, d.targets.len());
        let mut baked = vec![Vec3::ZERO; points];
        for (t, &w) in d.targets.iter().zip(&weights) {
            if w != 0.0 {
                baked.iter_mut().zip(&t.offsets).for_each(|(b, o)| *b += *o * w);
            }
        }
        if weights.iter().any(|&w| w != 0.0) {
            d.baked = baked;
        }
        d.targets.clear();
        d.target_names.clear();
        d.shapes.clear();
    }
}

/// A blend shape's primary shape or one of its inbetweens, as read.
struct SubShape<'p> {
    /// The shape weight at which it is fully applied.
    weight: f32,
    name: String,
    offsets: &'p [f32],
    normals: Option<&'p [f32]>,
}

/// Offsets for every point, from offsets for the points `indices` names
/// (or for every point, in order, when there are none).
fn scatter(offsets: &[f32], indices: Option<&[i32]>, points: usize) -> Vec<Vec3> {
    let mut dense = vec![Vec3::ZERO; points];
    let vectors = offsets.chunks_exact(3).map(Vec3::from_slice);
    match indices {
        Some(ix) => {
            for (&i, v) in ix.iter().zip(vectors) {
                if let Some(slot) = usize::try_from(i).ok().and_then(|i| dense.get_mut(i)) {
                    *slot = v;
                }
            }
        }
        None => dense.iter_mut().zip(vectors).for_each(|(slot, v)| *slot = v),
    }
    dense
}

/// Morph target weights at the start of a SkelAnimation (its first sample
/// or default): each of the mesh's blend shapes takes the animation's
/// weight for its name, or 0.
fn start_weights(stage: &Stage, anim: Option<usize>, shapes: &[ShapeMap], targets: usize) -> Vec<f32> {
    let mut out = vec![0.0; targets];
    let Some(anim) = anim.map(|a| &stage.prims[a]) else { return out };
    let (Some(names), Some(weights)) = (strs(anim, "blendShapes"), floats(anim, "blendShapeWeights", 1)) else {
        return out;
    };
    for s in shapes {
        if let Some(w) = names.iter().position(|n| *n == s.name).and_then(|i| weights.get(i)) {
            s.weights(*w, &mut out);
        }
    }
    out
}

impl Deform {
    /// Applies the deformation to one primitive built from the gprim's
    /// points, `points` giving the point each vertex came from: morph
    /// targets or baked offsets, then (for skinned prims) the bind transform
    /// and the joints and weights.
    pub fn apply(&self, prim: &mut Primitive, points: &[u32]) {
        let at = |v: usize| points.get(v).map_or(0, |&p| p as usize);
        let n = prim.positions.len();
        for (v, p) in prim.positions.iter_mut().enumerate() {
            if let Some(o) = self.baked.get(at(v)) {
                *p += *o;
            }
        }
        let linear = Mat3::from_mat4(self.geom_bind);
        let normal = if linear.determinant() != 0.0 { linear.inverse().transpose() } else { Mat3::IDENTITY };
        for t in &self.targets {
            let offset = |v: usize| t.offsets.get(at(v)).copied().unwrap_or(Vec3::ZERO);
            let positions = (0..n).map(|v| linear * offset(v)).collect();
            let normals = if t.normals.is_empty() {
                Vec::new()
            } else {
                (0..n).map(|v| normal * t.normals.get(at(v)).copied().unwrap_or(Vec3::ZERO)).collect()
            };
            prim.targets.push(MorphTarget { positions, normals });
        }
        let Some(influences) = self.influences.as_ref().filter(|_| self.skin.is_some()) else { return };
        for p in &mut prim.positions {
            *p = self.geom_bind.transform_point3(*p);
        }
        for nm in &mut prim.normals {
            *nm = (normal * *nm).try_normalize().unwrap_or(Vec3::Y);
        }
        // The renderer derives tangents again from the moved positions.
        prim.tangents.clear();
        match influences {
            Influences::Rigid(j, w) => {
                prim.joints = vec![*j; n];
                prim.weights = vec![*w; n];
            }
            Influences::PerPoint(js, ws) => {
                prim.joints = (0..n).map(|v| js.get(at(v)).copied().unwrap_or_default()).collect();
                prim.weights = (0..n).map(|v| ws.get(at(v)).copied().unwrap_or(Vec4::ZERO)).collect();
            }
        }
    }
}

//endregion

/*┌─────────────────╮
| Finishing       |
└─────────────────╯*/
//region Finishing

/// After the walk: makes the skins, sets blend shape weights at the first
/// sample, builds the clips and notes what was found.
pub(super) fn finish(b: &mut Builder) -> Result<(), slerp_scene::LoadError> {
    let stage = b.stage;
    let deformed = std::mem::take(&mut b.rig.deformed);

    // 1. Skins, one per skeleton and joint order.
    let mut skins: HashMap<(usize, Vec<Option<usize>>), usize> = HashMap::new();
    let mut skinned = 0;
    for d in &deformed {
        let Some(req) = &d.skin else { continue };
        if !b.rig.skeletons.contains_key(&req.skeleton) {
            add_detached_skeleton(b, req.skeleton);
        }
        let skel = &b.rig.skeletons[&req.skeleton];
        let key = (req.skeleton, req.joints.clone());
        let next = b.scene.skins.len();
        let si = *skins.entry(key).or_insert(next);
        if si == next {
            let node = |j: &Option<usize>| j.and_then(|j| skel.nodes.get(j).copied()).unwrap_or(skel.root);
            let inverse = |j: &Option<usize>| j.and_then(|j| skel.bind.get(j).copied()).map_or(Mat4::IDENTITY, invert);
            b.scene.skins.push(Skin {
                name: stage.prims[req.skeleton].name.clone(),
                joints: req.joints.iter().map(node).collect(),
                inverse_bind: req.joints.iter().map(inverse).collect(),
                skeleton: Some(skel.root),
            });
        }
        b.scene.nodes[d.node].skin = Some(si);
        skinned += 1;
    }

    // 2. Blend shape weights at the first sample.
    let mut morphed = 0;
    for d in deformed.iter().filter(|d| d.targets > 0) {
        let anim = d.skeleton.and_then(|s| animation_of(stage, s));
        b.scene.nodes[d.node].weights = start_weights(stage, anim, &d.shapes, d.targets);
        morphed += 1;
    }

    // 3. Clips.
    if b.ctx.limits.animations {
        clips(b, &deformed);
    }

    // 4. Facts for the inspector.
    let joints: usize = b.rig.skeletons.values().map(|s| s.nodes.len()).sum();
    if !b.rig.skeletons.is_empty() {
        let n = b.rig.skeletons.len();
        b.rig.details.push(("Skeletons".into(), format!("{n} ({joints} joints)")));
    }
    if skinned > 0 {
        b.rig.details.push(("Skinned prims".into(), format!("{skinned} ({} skins)", b.scene.skins.len())));
    }
    if morphed > 0 {
        let targets: usize = deformed.iter().map(|d| d.targets).sum();
        b.rig.details.push(("Blend shapes".into(), format!("{targets} on {morphed} prims")));
    }
    Ok(())
}

/// Converts time codes to clip seconds.
#[derive(Clone, Copy)]
struct Clock {
    start: f64,
    per_second: f64,
}

impl Clock {
    fn seconds(self, t: f64) -> f32 {
        ((t - self.start) / self.per_second) as f32
    }
}

/// Channels being gathered for one clip, within the stage's budget of
/// keyframe values.
struct ClipBuilder<'b> {
    clock: Clock,
    channels: Vec<Channel>,
    budget: &'b mut usize,
    cut: bool,
}

impl ClipBuilder<'_> {
    /// Adds a channel unless the budget is spent.
    fn push(&mut self, ch: Channel) {
        let cost = ch.times.len() + ch.values.len();
        if cost > *self.budget {
            self.cut = true;
            return;
        }
        *self.budget -= cost;
        self.channels.push(ch);
    }

    fn finish(self, name: String, warnings: &mut Vec<String>) -> Option<Animation> {
        if self.cut {
            warnings.push(format!("clip {name}: too much animation; some joints don't move"));
        }
        if self.channels.is_empty() {
            return None;
        }
        let duration = self.channels.iter().filter_map(|c| c.times.last()).fold(0.0f32, |a, &t| a.max(t));
        Some(Animation { name, duration, channels: self.channels })
    }
}

/// The time of the first sample of any of `attrs` on `p`.
fn first_time(p: &CPrim, attrs: &[&str]) -> Option<f64> {
    attrs.iter().filter_map(|a| p.prop(a)?.samples.as_ref()?.times.first().copied()).reduce(f64::min)
}

/// Builds the stage's clip (every skeleton's bound SkelAnimation, the blend
/// shape weights it drives and the animated transforms, played together)
/// and one clip for each SkelAnimation no skeleton is bound to, played on
/// the skeleton sharing the most joints with it.
fn clips(b: &mut Builder, deformed: &[Deformed]) {
    let stage = b.stage;
    let mut budget = MAX_KEY_FLOATS;
    let mut warnings = Vec::new();
    let mut skeletons: Vec<usize> = b.rig.skeletons.keys().copied().collect();
    skeletons.sort_unstable();
    const JOINT_ATTRS: [&str; 4] = ["translations", "rotations", "scales", "blendShapeWeights"];

    // 1. The stage's clip, starting at the stage's start time or else the
    //    earliest sample.
    let bound: Vec<(usize, usize)> = skeletons.iter().filter_map(|&s| Some((s, b.rig.skeletons[&s].anim?))).collect();
    let mut start = stage.start_time;
    if start.is_none() {
        let anims = bound.iter().filter_map(|&(_, a)| first_time(&stage.prims[a], &JOINT_ATTRS));
        let xforms = b.rig.xforms.iter().filter_map(|&(_, p)| xform_times(&stage.prims[p]).first().copied());
        start = anims.chain(xforms).reduce(f64::min);
    }
    let clock = Clock { start: start.unwrap_or(0.0), per_second: stage.time_codes_per_second };
    let mut clip = ClipBuilder { clock, channels: Vec::new(), budget: &mut budget, cut: false };
    for &(s, a) in &bound {
        joint_channels(&mut clip, b, s, a, true);
        shape_channels(&mut clip, stage, deformed, s, a, true);
    }
    for &(node, p) in &b.rig.xforms {
        xform_channels(&mut clip, &stage.prims[p], node);
    }
    let mut anims: Vec<usize> = bound.iter().map(|&(_, a)| a).collect();
    anims.sort_unstable();
    anims.dedup();
    let name = match anims.as_slice() {
        [a] => stage.prims[*a].name.clone(),
        _ => "Animation".to_string(),
    };
    if let Some(c) = clip.finish(name, &mut warnings) {
        b.rig.details.push(("Animation".into(), clip_fact(&c, stage.time_codes_per_second)));
        b.scene.animations.push(c);
    }

    // 2. SkelAnimations no skeleton plays, as clips of their own.  They
    //    set every joint, as the pose they start from is another clip's.
    let used: HashSet<usize> = anims.into_iter().collect();
    for (a, p) in stage.prims.iter().enumerate() {
        if p.type_name != "SkelAnimation" || !p.defined || !p.active || used.contains(&a) {
            continue;
        }
        let best = skeletons
            .iter()
            .map(|&s| (s, anim_joints(p, b.rig.topology(stage, s)).len()))
            .filter(|&(_, n)| n > 0)
            .max_by_key(|&(_, n)| n);
        let Some((s, _)) = best else { continue };
        let start = stage.start_time.or_else(|| first_time(p, &JOINT_ATTRS)).unwrap_or(0.0);
        let clock = Clock { start, per_second: stage.time_codes_per_second };
        let mut clip = ClipBuilder { clock, channels: Vec::new(), budget: &mut budget, cut: false };
        joint_channels(&mut clip, b, s, a, false);
        shape_channels(&mut clip, stage, deformed, s, a, false);
        if let Some(c) = clip.finish(p.name.clone(), &mut warnings) {
            b.scene.animations.push(c);
        }
    }
    for w in warnings {
        b.ctx.warn(w);
    }
    if b.scene.animations.len() > 1 {
        b.rig.details.push(("Clips".into(), format!("{}", b.scene.animations.len())));
    }
}

/// "2.40 s at 30 time codes per second".
fn clip_fact(c: &Animation, per_second: f64) -> String {
    format!("{:.2} s at {per_second} time codes per second", c.duration)
}

/// Channels for the joints SkelAnimation `a` moves on skeleton `s`.  For
/// the stage's clip (`main`), joints whose values never change are left to
/// their nodes, which start at those values; another clip sets every joint
/// of the skeleton, the ones it doesn't move to their rest transforms.
fn joint_channels(clip: &mut ClipBuilder, b: &mut Builder, s: usize, a: usize, main: bool) {
    let stage = b.stage;
    let anim = &stage.prims[a];
    let pairs = anim_joints(anim, b.rig.topology(stage, s));
    let skel = &b.rig.skeletons[&s];
    let count = strs(anim, "joints").map_or(0, <[String]>::len);
    let pairs: Vec<(usize, usize, usize)> =
        pairs.into_iter().filter_map(|(ai, j)| Some((ai, j, *skel.nodes.get(j)?))).collect();
    for (attr, property, width) in [
        ("translations", Property::Translation, 3),
        ("rotations", Property::Rotation, 4),
        ("scales", Property::Scale, 3),
    ] {
        let keys = keyframes(anim, attr, width, count, clip.clock);
        let mut moved = vec![false; skel.nodes.len()];
        for &(ai, j, node) in &pairs {
            let Some(keys) = &keys else { continue };
            let at = ai * width;
            let first = &keys[0].1[at..at + width];
            let constant = keys.iter().all(|k| k.1[at..at + width] == *first);
            moved[j] = true;
            if main && (constant || keys.len() < 2) {
                continue;
            }
            let keys: &[(f32, &[f32])] = if constant { &keys[..1] } else { keys };
            let mut values = Vec::with_capacity(keys.len() * width);
            let mut prev = Quat::IDENTITY;
            for (_, v) in keys {
                let v = &v[at..at + width];
                if property == Property::Rotation {
                    // Keep neighbouring keys in one hemisphere, so blending
                    // between them takes the short way round.
                    let q = quat(v);
                    let q = if prev.dot(q) < 0.0 { -q } else { q };
                    prev = q;
                    values.extend_from_slice(&q.to_array());
                } else {
                    values.extend_from_slice(v);
                }
            }
            let times = keys.iter().map(|k| k.0).collect();
            clip.push(Channel { node, property, interpolation: Interpolation::Linear, times, values });
        }
        if main {
            continue;
        }
        // Joints this clip doesn't move hold their rest values.
        for (j, &node) in skel.nodes.iter().enumerate() {
            if moved[j] {
                continue;
            }
            let r = &skel.rest[j];
            let values = match property {
                Property::Translation => r.translation.to_array().to_vec(),
                Property::Rotation => r.rotation.to_array().to_vec(),
                _ => r.scale.to_array().to_vec(),
            };
            clip.push(Channel { node, property, interpolation: Interpolation::Step, times: vec![0.0], values });
        }
    }
}

/// The samples of a SkelAnimation array attribute that hold `width`
/// numbers for each of the animation's `count` joints, as (seconds,
/// numbers); None when there are none.
fn keyframes<'p>(p: &'p CPrim, attr: &str, width: usize, count: usize, clock: Clock) -> Option<Vec<(f32, &'p [f32])>> {
    let samples = p.prop(attr)?.samples.as_deref()?;
    let keys: Vec<(f32, &[f32])> = samples
        .times
        .iter()
        .zip(&samples.values)
        .filter_map(|(&t, v)| match &**v {
            Value::Floats(f, w) if *w == width && f.len() == count * width => Some((clock.seconds(t), f.as_slice())),
            _ => None,
        })
        .collect();
    (!keys.is_empty()).then_some(keys)
}

/// Morph target weight channels for the prims bound to skeleton `s`, from
/// SkelAnimation `a`'s blend shape weights.
fn shape_channels(clip: &mut ClipBuilder, stage: &Stage, deformed: &[Deformed], s: usize, a: usize, main: bool) {
    let anim = &stage.prims[a];
    let Some(names) = strs(anim, "blendShapes") else { return };
    let Some(keys) = keyframes(anim, "blendShapeWeights", 1, names.len(), clip.clock) else { return };
    for d in deformed.iter().filter(|d| d.skeleton == Some(s) && d.targets > 0) {
        let slots: Vec<Option<usize>> = d.shapes.iter().map(|m| names.iter().position(|n| *n == m.name)).collect();
        let mut values = Vec::with_capacity(keys.len() * d.targets);
        for (_, weights) in &keys {
            let mut out = vec![0.0; d.targets];
            for (m, slot) in d.shapes.iter().zip(&slots) {
                if let Some(&w) = slot.and_then(|i| weights.get(i)) {
                    m.weights(w, &mut out);
                }
            }
            values.extend_from_slice(&out);
        }
        let first = &values[..d.targets];
        let constant = values.chunks_exact(d.targets).all(|c| c == first);
        if main && constant {
            continue;
        }
        let (times, values) =
            if constant { (vec![keys[0].0], first.to_vec()) } else { (keys.iter().map(|k| k.0).collect(), values) };
        let ch =
            Channel { node: d.node, property: Property::Weights, interpolation: Interpolation::Linear, times, values };
        clip.push(ch);
    }
}

//endregion

/*┌─────────────────╮
| Transforms      |
└─────────────────╯*/
//region Transforms

/// The ops of a prim's `xformOpOrder`, without `!invert!`.
fn ops(p: &CPrim) -> impl Iterator<Item = &str> {
    let order = match p.value("xformOpOrder") {
        Some(Value::Strs(o)) => o.as_slice(),
        _ => &[],
    };
    order.iter().map(|op| op.strip_prefix("!invert!").unwrap_or(op))
}

/// Whether any of the prim's transform operations has samples at two or
/// more times.
pub(super) fn animated_xform(p: &CPrim) -> bool {
    ops(p).any(|op| p.prop(op).and_then(|q| q.samples.as_ref()).is_some_and(|s| s.times.len() > 1))
}

/// Every time any of the prim's transform operations has a sample at, in
/// order, up to a cap.
fn xform_times(p: &CPrim) -> Vec<f64> {
    let mut times: Vec<f64> =
        ops(p).filter_map(|op| p.prop(op)?.samples.as_ref()).flat_map(|s| s.times.iter().copied()).collect();
    times.sort_by(f64::total_cmp);
    times.dedup();
    times.truncate(MAX_XFORM_KEYS);
    times
}

/// An attribute's value at time `t`: its samples blended linearly (held
/// before the first and after the last), or its only value.
fn value_at(p: &CPrim, name: &str, t: f64) -> Option<Value> {
    let q = p.prop(name)?;
    let Some(s) = q.samples.as_ref().filter(|s| !s.times.is_empty()) else { return q.value.as_deref().cloned() };
    let next = s.times.partition_point(|&x| x <= t);
    if next == 0 || next == s.times.len() {
        return Some((*s.values[next.saturating_sub(1)]).clone());
    }
    let (t0, t1) = (s.times[next - 1], s.times[next]);
    let f = if t1 > t0 { (t - t0) / (t1 - t0) } else { 0.0 };
    let (a, b) = (&*s.values[next - 1], &*s.values[next]);
    Some(match (a, b) {
        (Value::Float(x), Value::Float(y)) => Value::Float(x + (y - x) * f),
        (Value::Tuple(x), Value::Tuple(y)) if x.len() == y.len() => {
            Value::Tuple(x.iter().zip(y).map(|(x, y)| x + (y - x) * f).collect())
        }
        _ => a.clone(),
    })
}

/// Translation, rotation and scale channels for a node whose prim's
/// transform operations are time-sampled, keyed at every sample time of any
/// of them.  Channels that never change are left out.
fn xform_channels(clip: &mut ClipBuilder, p: &CPrim, node: usize) {
    let Some(Value::Strs(order)) = p.value("xformOpOrder") else { return };
    let times = xform_times(p);
    let poses: Vec<Transform> = times.iter().map(|&t| op_stack(order, |name| value_at(p, name, t))).collect();
    let seconds: Vec<f32> = times.iter().map(|&t| clip.clock.seconds(t)).collect();
    let mut prev = Quat::IDENTITY;
    let rotations: Vec<Quat> = poses
        .iter()
        .map(|x| {
            let q = if prev.dot(x.rotation) < 0.0 { -x.rotation } else { x.rotation };
            prev = q;
            q
        })
        .collect();
    let parts: [(Property, Vec<f32>); 3] = [
        (Property::Translation, poses.iter().flat_map(|x| x.translation.to_array()).collect()),
        (Property::Rotation, rotations.iter().flat_map(|q| q.to_array()).collect()),
        (Property::Scale, poses.iter().flat_map(|x| x.scale.to_array()).collect()),
    ];
    for (property, values) in parts {
        let width = values.len() / times.len().max(1);
        if width == 0 || values.chunks_exact(width).all(|c| c == &values[..width]) {
            continue;
        }
        let interpolation = Interpolation::Linear;
        clip.push(Channel { node, property, interpolation, times: seconds.clone(), values });
    }
}

//endregion
