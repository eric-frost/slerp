//! What to write: the output's nodes, and the scene's meshes, skins and
//! clips they use, worked out from the selection before anything is
//! encoded.
//!
//! The scene's hierarchy is first made into a forest (a node reached twice
//! keeps its first parent; cycles are cut), so every later step can walk
//! it without checks.  The selection then picks a set of scene nodes, which
//! grows by the joints their skinned meshes need.  A picked node whose
//! parent isn't picked becomes a root of the output, with the parents'
//! transform folded into its own, or, when a clip moves it, into a new
//! parent standing for them, since the clip's values are relative to that
//! parent.
//!
//! Some meshes move to nodes of their own ("carriers"), so the output says
//! exactly what Slerp draws: a skinned mesh goes to a new root with no
//! transform, since glTF and Slerp both ignore a skinned mesh node's own
//! transform and glTF validators warn about one; and a hidden mesh on a
//! node with visible children goes to a hidden child, since
//! `KHR_node_visibility` hides a node's whole subtree while Slerp hides
//! only the node's own mesh.

use super::{GlbOptions, Placement, Selection, WriteError};
use slerp_scene::glam::{Mat4, Quat, Vec3};
use slerp_scene::{Aabb, Channel, Property, Scene, Transform};
use std::collections::HashMap;

/// At most this many markers are written as empty nodes.
const MAX_MARKERS: usize = 100_000;

/*┌─────────────────╮
| Plan            |
└─────────────────╯*/
//region Plan

/// A node of the output.
#[derive(Clone, Debug)]
pub struct OutNode {
    pub name: String,
    pub transform: Transform,
    pub children: Vec<usize>,
    pub mesh: Option<MeshUse>,
    pub weights: Vec<f32>,
    pub hidden: bool,
    /// Carries a skinned mesh: a root whose transform glTF ignores, so
    /// centring leaves it alone.
    pub skinned_carrier: bool,
}

impl OutNode {
    fn new(name: String, transform: Transform) -> Self {
        OutNode {
            name,
            transform,
            children: Vec::new(),
            mesh: None,
            weights: Vec::new(),
            hidden: false,
            skinned_carrier: false,
        }
    }
}

/// A node's mesh: an index into `Plan::meshes`, and into `Plan::skins`
/// when it is drawn skinned.
#[derive(Clone, Copy, Debug)]
pub struct MeshUse {
    pub index: usize,
    pub skin: Option<usize>,
}

/// Where a written mesh's geometry comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MeshSource {
    /// `Scene::meshes[i]`.
    Scene(usize),
    /// `Scene::splats`, written as coloured points.
    Splats,
}

/// A mesh to write.
#[derive(Clone, Debug)]
pub struct PlannedMesh {
    pub source: MeshSource,
    /// Written with joints and weights, for a skinned node.
    pub skinned: bool,
    /// Joint indices at or past this are cleared: the fewest joints of the
    /// skins drawing it.
    pub joint_limit: usize,
}

/// A skin to write, with its joints as output nodes.
#[derive(Clone, Debug)]
pub struct PlannedSkin {
    pub name: String,
    pub joints: Vec<usize>,
    pub inverse_bind: Vec<Mat4>,
    pub skeleton: Option<usize>,
}

/// A clip to write: the scene's clip, and its channels that land on output
/// nodes, with the node each now targets.
#[derive(Clone, Debug)]
pub struct PlannedClip<'a> {
    pub name: String,
    pub channels: Vec<(&'a Channel, usize)>,
}

pub struct Plan<'a> {
    pub nodes: Vec<OutNode>,
    pub roots: Vec<usize>,
    pub meshes: Vec<PlannedMesh>,
    pub skins: Vec<PlannedSkin>,
    pub clips: Vec<PlannedClip<'a>>,
    pub warnings: Vec<String>,
}

//endregion

/*┌─────────────────╮
| Forest          |
└─────────────────╯*/
//region Forest

/// The scene's hierarchy as a forest, with rest-pose world matrices.
struct Forest {
    parent: Vec<Option<usize>>,
    children: Vec<Vec<usize>>,
    /// Every node, parents before children, trees in the order the scene
    /// draws them.
    order: Vec<usize>,
    /// Reached from `Scene::roots`, so drawn.
    drawn: Vec<bool>,
    world: Vec<Mat4>,
    local: Vec<Transform>,
}

impl Forest {
    fn new(scene: &Scene) -> Forest {
        let n = scene.nodes.len();
        let local: Vec<Transform> = scene.nodes.iter().map(|node| clean_transform(node.transform)).collect();
        let mut f = Forest {
            parent: vec![None; n],
            children: vec![Vec::new(); n],
            order: Vec::with_capacity(n),
            drawn: vec![false; n],
            world: vec![Mat4::IDENTITY; n],
            local,
        };
        let mut has_parent = vec![false; n];
        for (i, node) in scene.nodes.iter().enumerate() {
            for &c in &node.children {
                if c < n && c != i {
                    has_parent[c] = true;
                }
            }
        }
        let starts: Vec<usize> = scene
            .roots
            .iter()
            .copied()
            .filter(|&r| r < n)
            .chain((0..n).filter(|&i| !has_parent[i]))
            .chain(0..n)
            .collect();
        let roots = scene.roots.iter().filter(|&&r| r < n).count();
        let mut seen = vec![false; n];
        for (k, start) in starts.into_iter().enumerate() {
            if seen[start] {
                continue;
            }
            seen[start] = true;
            let drawn = k < roots;
            let mut stack = vec![start];
            while let Some(node) = stack.pop() {
                f.order.push(node);
                f.drawn[node] = drawn;
                let parent_world = f.parent[node].map_or(Mat4::IDENTITY, |p| f.world[p]);
                f.world[node] = parent_world * f.local[node].matrix();
                for &c in scene.nodes[node].children.iter().rev() {
                    if c < n && !seen[c] {
                        seen[c] = true;
                        f.parent[c] = Some(node);
                        stack.push(c);
                    }
                }
            }
        }
        // Children in the scene's order, now that each has one parent.
        for (i, node) in scene.nodes.iter().enumerate() {
            f.children[i] = node.children.iter().copied().filter(|&c| c < n && f.parent[c] == Some(i)).collect();
        }
        f
    }

    /// `node` and everything under it.
    fn subtree(&self, node: usize, into: &mut [bool]) {
        let mut stack = vec![node];
        while let Some(x) = stack.pop() {
            if !std::mem::replace(&mut into[x], true) {
                stack.extend(&self.children[x]);
            }
        }
    }
}

/// A transform glTF accepts: finite, with a unit rotation.
fn clean_transform(t: Transform) -> Transform {
    let rotation = t.rotation.normalize();
    if t.translation.is_finite() && t.scale.is_finite() && rotation.is_finite() {
        Transform { rotation, ..t }
    } else {
        Transform::IDENTITY
    }
}

/// A matrix as translation, rotation and scale.  Shear is lost.
fn from_matrix(m: Mat4) -> Transform {
    if !m.is_finite() {
        return Transform::IDENTITY;
    }
    clean_transform(Transform::from_matrix(m))
}

fn is_identity(m: &Mat4) -> bool {
    m.abs_diff_eq(Mat4::IDENTITY, 1e-6)
}

//endregion

/*┌─────────────────╮
| Building        |
└─────────────────╯*/
//region Building

pub fn build<'a>(scene: &'a Scene, what: &Selection, options: &GlbOptions) -> Result<Plan<'a>, WriteError> {
    let forest = Forest::new(scene);
    let mut b = Builder {
        scene,
        forest: &forest,
        nodes: Vec::new(),
        roots: Vec::new(),
        meshes: Vec::new(),
        mesh_of: HashMap::new(),
        skin_uses: Vec::new(),
        of_scene: vec![None; scene.nodes.len()],
        carrier: vec![None; scene.nodes.len()],
        warnings: Vec::new(),
    };
    if let Selection::Mesh(m) = *what {
        b.single_mesh(m, options.placement)?;
        return Ok(b.finish(Vec::new()));
    }

    // 1. The scene nodes picked, the ones whose meshes are written, and
    //    the clips.
    let n = scene.nodes.len();
    let mut wanted = vec![false; n];
    let mut mesh_on = vec![false; n];
    let has_mesh = |x: usize| scene.nodes[x].mesh.is_some_and(|m| m < scene.meshes.len());
    let shown = |x: usize| options.hidden || scene.nodes[x].visible;
    let clips: Vec<usize> = match *what {
        Selection::Scene => {
            wanted.clone_from(&forest.drawn);
            if options.animations { (0..scene.animations.len()).collect() } else { Vec::new() }
        }
        Selection::Node(x) => {
            if x >= n {
                return Err(WriteError::Selection(format!("the scene has no node {x}")));
            }
            forest.subtree(x, &mut wanted);
            if options.animations { (0..scene.animations.len()).collect() } else { Vec::new() }
        }
        Selection::Nodes(ref xs) => {
            if let Some(x) = xs.iter().find(|&&x| x >= n) {
                return Err(WriteError::Selection(format!("the scene has no node {x}")));
            }
            if xs.is_empty() {
                return Err(WriteError::Selection("no nodes were chosen".into()));
            }
            xs.iter().for_each(|&x| forest.subtree(x, &mut wanted));
            if options.animations { (0..scene.animations.len()).collect() } else { Vec::new() }
        }
        Selection::Animation { index, skeleton_only } => {
            let Some(clip) = scene.animations.get(index) else {
                return Err(WriteError::Selection(format!("the scene has no animation {index}")));
            };
            let mut moved = vec![false; n];
            let mut morphed = vec![false; n];
            for ch in clip.channels.iter().filter(|c| c.node < n) {
                match ch.property {
                    Property::Weights => morphed[ch.node] = true,
                    _ => forest.subtree(ch.node, &mut moved),
                }
            }
            for x in 0..n {
                let skin_moves =
                    skin_of(scene, x).is_some_and(|s| scene.skins[s].joints.iter().any(|&j| j < n && moved[j]));
                if skeleton_only {
                    wanted[x] |= moved[x];
                    if skin_moves && let Some(s) = skin_of(scene, x) {
                        scene.skins[s].joints.iter().filter(|&&j| j < n).for_each(|&j| wanted[j] = true);
                    }
                } else {
                    wanted[x] |= moved[x];
                    mesh_on[x] = has_mesh(x) && shown(x) && (moved[x] || morphed[x] || skin_moves);
                    wanted[x] |= mesh_on[x];
                }
            }
            vec![index]
        }
        Selection::Mesh(_) => Vec::new(),
    };
    if !matches!(what, Selection::Animation { .. }) {
        for x in 0..n {
            mesh_on[x] = wanted[x] && has_mesh(x) && shown(x);
        }
    }

    // 2. The joints skinned meshes need.
    let mut is_joint = vec![false; n];
    for x in (0..n).filter(|&x| mesh_on[x]) {
        if let Some(s) = skin_of(scene, x)
            && mesh_is_skinned(scene, x)
        {
            for &j in scene.skins[s].joints.iter().filter(|&&j| j < n) {
                wanted[j] = true;
                is_joint[j] = true;
            }
        }
    }

    // 3. Nodes a written clip moves, which keep their parents' frame.
    let mut animated = vec![false; n];
    for &c in &clips {
        for ch in &scene.animations[c].channels {
            if ch.node < n && wanted[ch.node] && ch.property != Property::Weights {
                animated[ch.node] = true;
            }
        }
    }

    // 4. The output nodes, in the scene's order.
    let mut stand_ins: HashMap<usize, usize> = HashMap::new();
    for &x in &forest.order {
        if !wanted[x] {
            continue;
        }
        let parent_out = forest.parent[x].filter(|&p| wanted[p]).and_then(|p| b.of_scene[p]);
        let has_children = forest.children[x].iter().any(|&c| wanted[c]);
        let mut transform = forest.local[x];
        let mut attach = parent_out;
        if parent_out.is_none()
            && let Some(p) = forest.parent[x]
            && !is_identity(&forest.world[p])
        {
            if animated[x] {
                // Clip values are relative to the parent: stand in for it.
                let stand_in = *stand_ins.entry(p).or_insert_with(|| {
                    let name = scene.nodes[p].name.clone();
                    b.push_root(OutNode::new(name, from_matrix(forest.world[p])))
                });
                attach = Some(stand_in);
            } else {
                transform = from_matrix(forest.world[p] * forest.local[x].matrix());
            }
        }
        b.add_node(x, transform, attach, mesh_on[x], has_children, is_joint[x] || animated[x]);
    }

    // 5. A level's markers and a capture's splats, with the whole scene.
    if *what == Selection::Scene {
        b.markers();
        if scene.splats.as_ref().is_some_and(|s| !s.is_empty()) {
            let mut node = OutNode::new("Gaussian splats".into(), Transform::IDENTITY);
            node.mesh = Some(MeshUse { index: b.mesh(MeshSource::Splats, false, usize::MAX), skin: None });
            b.push_root(node);
            b.warnings.push("Gaussian splats were saved as coloured points, which glTF can show".into());
        }
    }

    // 6. Placement.
    if options.placement == Placement::Centred {
        let bounds = b.bounds(&mesh_on);
        let mut moves = vec![false; b.nodes.len()];
        (0..n).filter(|&x| animated[x]).filter_map(|x| b.of_scene[x]).for_each(|o| moves[o] = true);
        let any_animated = b.roots.iter().any(|&r| moves[r]);
        b.centre(&bounds, any_animated);
    }
    let planned_clips = b.clips(&clips, &mesh_on);
    Ok(b.finish(planned_clips))
}

/// The node's skin, when it has a mesh and the skin exists.
fn skin_of(scene: &Scene, x: usize) -> Option<usize> {
    let node = &scene.nodes[x];
    node.mesh.filter(|&m| m < scene.meshes.len())?;
    node.skin.filter(|&s| s < scene.skins.len())
}

/// True when the node draws its mesh skinned: it has a skin, and its mesh
/// has a skinned primitive.
fn mesh_is_skinned(scene: &Scene, x: usize) -> bool {
    skin_of(scene, x).is_some_and(|s| !scene.skins[s].joints.is_empty())
        && scene.nodes[x]
            .mesh
            .and_then(|m| scene.meshes.get(m))
            .is_some_and(|m| m.primitives.iter().any(|p| p.is_skinned()))
}

fn morph_count(scene: &Scene, mesh: usize) -> usize {
    scene.meshes.get(mesh).map_or(0, |m| m.primitives.iter().map(|p| p.targets.len()).max().unwrap_or(0))
}

struct Builder<'a, 'f> {
    scene: &'a Scene,
    forest: &'f Forest,
    nodes: Vec<OutNode>,
    roots: Vec<usize>,
    meshes: Vec<PlannedMesh>,
    mesh_of: HashMap<(MeshSource, bool), usize>,
    /// (output node, scene skin) for each skinned mesh written.
    skin_uses: Vec<(usize, usize)>,
    of_scene: Vec<Option<usize>>,
    /// The output node carrying each scene node's mesh.
    carrier: Vec<Option<usize>>,
    warnings: Vec<String>,
}

impl<'a> Builder<'a, '_> {
    fn push(&mut self, node: OutNode, parent: Option<usize>) -> usize {
        self.nodes.push(node);
        let i = self.nodes.len() - 1;
        match parent {
            Some(p) => self.nodes[p].children.push(i),
            None => self.roots.push(i),
        }
        i
    }

    fn push_root(&mut self, node: OutNode) -> usize {
        self.push(node, None)
    }

    /// The planned mesh for `source` drawn skinned or not, made on first
    /// use.  `joints` is the skin's joint count, which bounds the joint
    /// indices written.
    fn mesh(&mut self, source: MeshSource, skinned: bool, joints: usize) -> usize {
        let index = *self.mesh_of.entry((source, skinned)).or_insert_with(|| {
            self.meshes.push(PlannedMesh { source, skinned, joint_limit: usize::MAX });
            self.meshes.len() - 1
        });
        let m = &mut self.meshes[index];
        m.joint_limit = m.joint_limit.min(joints);
        index
    }

    /// Adds scene node `x` under `parent` (or as a root), with its mesh
    /// when `with_mesh`, and a carrier for the mesh where it needs one.
    /// A skinned mesh's node without children, joint's place or a clip's
    /// motion (`needed`) is left out once its mesh has moved to a carrier.
    fn add_node(
        &mut self,
        x: usize,
        transform: Transform,
        parent: Option<usize>,
        with_mesh: bool,
        has_children: bool,
        needed: bool,
    ) {
        let scene = self.scene;
        let node = &scene.nodes[x];
        let out = OutNode::new(node.name.clone(), transform);
        let Some(mesh) = node.mesh.filter(|&m| with_mesh && m < scene.meshes.len()) else {
            let i = self.push(out, parent);
            self.of_scene[x] = Some(i);
            return;
        };
        let mut weights: Vec<f32> = node.weights.iter().map(|w| if w.is_finite() { *w } else { 0.0 }).collect();
        weights.resize(morph_count(scene, mesh), 0.0);
        let hidden = !node.visible;
        if mesh_is_skinned(scene, x) {
            // A root with no transform carries the skinned mesh.
            let skin = skin_of(scene, x).unwrap_or(0);
            let joints = scene.skins.get(skin).map_or(0, |s| s.joints.len());
            let mut carrier = OutNode::new(node.name.clone(), Transform::IDENTITY);
            carrier.mesh = Some(MeshUse { index: self.mesh(MeshSource::Scene(mesh), true, joints), skin: None });
            carrier.weights = weights;
            carrier.hidden = hidden;
            carrier.skinned_carrier = true;
            let c = self.push_root(carrier);
            self.skin_uses.push((c, skin));
            self.carrier[x] = Some(c);
            if has_children || needed {
                let i = self.push(out, parent);
                self.of_scene[x] = Some(i);
            }
            return;
        }
        let use_ = MeshUse { index: self.mesh(MeshSource::Scene(mesh), false, usize::MAX), skin: None };
        if !hidden || !has_children {
            let mut out = out;
            out.mesh = Some(use_);
            out.weights = weights;
            out.hidden = hidden;
            let i = self.push(out, parent);
            self.of_scene[x] = Some(i);
            self.carrier[x] = Some(i);
        } else {
            // A hidden child carries the mesh, so the children stay visible.
            let i = self.push(out, parent);
            self.of_scene[x] = Some(i);
            let mut carrier = OutNode::new(node.name.clone(), Transform::IDENTITY);
            carrier.mesh = Some(use_);
            carrier.weights = weights;
            carrier.hidden = true;
            let c = self.push(carrier, Some(i));
            self.carrier[x] = Some(c);
        }
    }

    /// `Selection::Mesh`: one node showing mesh `m`.
    fn single_mesh(&mut self, m: usize, placement: Placement) -> Result<(), WriteError> {
        let scene = self.scene;
        if m >= scene.meshes.len() {
            return Err(WriteError::Selection(format!("the scene has no mesh {m}")));
        }
        let first = self.forest.order.iter().copied().find(|&x| scene.nodes[x].mesh == Some(m));
        let mut matrix = Mat4::IDENTITY;
        let mut weights = Vec::new();
        if let Some(x) = first {
            matrix = self.forest.world[x];
            weights = scene.nodes[x].weights.clone();
            // A skinned mesh sits where its joints put it in the rest pose:
            // the skinning matrix of its first weighted vertex.
            if mesh_is_skinned(scene, x)
                && let Some(s) = skin_of(scene, x)
                && let Some(p) = scene.meshes[m].primitives.iter().find(|p| p.is_skinned())
                && let (Some(j), Some(w)) = (p.joints.first(), p.weights.first())
            {
                let k = (0..4).max_by(|&a, &b| w[a].total_cmp(&w[b])).unwrap_or(0);
                let skin = &scene.skins[s];
                if let Some(&joint) = skin.joints.get(j[k] as usize).filter(|&&jn| jn < scene.nodes.len()) {
                    let ibm = skin.inverse_bind.get(j[k] as usize).copied().unwrap_or(Mat4::IDENTITY);
                    matrix = self.forest.world[joint] * ibm;
                }
            }
        }
        weights.resize(morph_count(scene, m), 0.0);
        weights.iter_mut().filter(|w| !w.is_finite()).for_each(|w| *w = 0.0);
        let mut node = OutNode::new(scene.meshes[m].name.clone(), from_matrix(matrix));
        node.mesh = Some(MeshUse { index: self.mesh(MeshSource::Scene(m), false, usize::MAX), skin: None });
        node.weights = weights;
        self.push_root(node);
        if placement == Placement::Centred {
            let mut bounds = Aabb::EMPTY;
            let t = self.nodes[0].transform.matrix();
            scene.meshes[m].primitives.iter().flat_map(|p| &p.positions).for_each(|&p| {
                if p.is_finite() {
                    bounds.add(t.transform_point3(p));
                }
            });
            self.centre(&bounds, false);
        }
        Ok(())
    }

    /// A level's markers and spawn point, as empty nodes in a group.
    fn markers(&mut self) {
        let scene = self.scene;
        if scene.markers.is_empty() && scene.spawn.is_none() {
            return;
        }
        let group = self.push_root(OutNode::new("Markers".into(), Transform::IDENTITY));
        if let Some(spawn) = scene.spawn.filter(|s| s.position.is_finite() && s.yaw.is_finite()) {
            let t = Transform {
                translation: spawn.position,
                rotation: Quat::from_rotation_y(spawn.yaw),
                ..Transform::IDENTITY
            };
            self.push(OutNode::new("spawn".into(), t), Some(group));
        }
        if scene.markers.len() > MAX_MARKERS {
            self.warnings.push(format!("only the first {MAX_MARKERS} of {} markers were saved", scene.markers.len()));
        }
        for m in scene.markers.iter().take(MAX_MARKERS) {
            let position = if m.position.is_finite() { m.position } else { Vec3::ZERO };
            self.push(
                OutNode::new(m.label.clone(), Transform { translation: position, ..Transform::IDENTITY }),
                Some(group),
            );
        }
    }

    /// The box around the meshes written, in the rest pose.
    fn bounds(&self, mesh_on: &[bool]) -> Aabb {
        let scene = self.scene;
        let mut out = Aabb::EMPTY;
        let add = |out: &mut Aabb, p: Vec3| {
            if p.is_finite() {
                out.add(p);
            }
        };
        for x in (0..scene.nodes.len()).filter(|&x| mesh_on[x] && self.carrier[x].is_some()) {
            let Some(mesh) = scene.nodes[x].mesh.and_then(|m| scene.meshes.get(m)) else { continue };
            let joints: Option<Vec<Mat4>> = skin_of(scene, x).filter(|_| mesh_is_skinned(scene, x)).map(|s| {
                let skin = &scene.skins[s];
                skin.joints
                    .iter()
                    .enumerate()
                    .map(|(k, &j)| {
                        let world = self.forest.world.get(j).copied().unwrap_or(Mat4::IDENTITY);
                        world * skin.inverse_bind.get(k).copied().unwrap_or(Mat4::IDENTITY)
                    })
                    .collect()
            });
            for p in &mesh.primitives {
                match &joints {
                    Some(joints) if p.is_skinned() && p.joints.len() == p.positions.len() => {
                        for ((&v, j), w) in p.positions.iter().zip(&p.joints).zip(&p.weights) {
                            let mut m = Mat4::ZERO;
                            for k in 0..4 {
                                if w[k] > 0.0 {
                                    m += joints.get(j[k] as usize).copied().unwrap_or(Mat4::IDENTITY) * w[k];
                                }
                            }
                            add(&mut out, m.transform_point3(v));
                        }
                    }
                    _ => p.positions.iter().for_each(|&v| add(&mut out, self.forest.world[x].transform_point3(v))),
                }
            }
        }
        if let Some(s) = &self.scene.splats {
            s.positions.iter().for_each(|&p| add(&mut out, p));
        }
        // A skeleton alone: the box around its joints.
        if out.is_empty() {
            for (x, o) in self.of_scene.iter().enumerate() {
                if o.is_some() {
                    add(&mut out, self.forest.world[x].w_axis.truncate());
                }
            }
        }
        out
    }

    /// Moves the roots so `bounds` is centred on the origin: by their
    /// translations, or under a new root when a clip moves one of them and
    /// would overwrite its translation.
    fn centre(&mut self, bounds: &Aabb, animated_root: bool) {
        if bounds.is_empty() {
            return;
        }
        let offset = -bounds.center();
        if animated_root {
            let roots = std::mem::take(&mut self.roots);
            let mut node = OutNode::new("centred".into(), Transform { translation: offset, ..Transform::IDENTITY });
            node.children = roots.iter().copied().filter(|&r| !self.nodes[r].skinned_carrier).collect();
            let skinned: Vec<usize> = roots.into_iter().filter(|&r| self.nodes[r].skinned_carrier).collect();
            self.push_root(node);
            self.roots.extend(skinned);
            return;
        }
        for &r in &self.roots {
            let node = &mut self.nodes[r];
            if !node.skinned_carrier {
                node.transform.translation += offset;
            }
        }
    }

    /// The written clips' channels, each aimed at its output node.  Weight
    /// channels go to the node carrying the mesh.  A second channel for the
    /// same node and property is dropped, as glTF requires.
    fn clips(&mut self, clips: &[usize], mesh_on: &[bool]) -> Vec<PlannedClip<'a>> {
        let scene = self.scene;
        let mut out = Vec::new();
        for &c in clips {
            let clip = &scene.animations[c];
            let mut channels = Vec::new();
            let mut seen = std::collections::HashSet::new();
            for ch in &clip.channels {
                let Some(node) = scene.nodes.get(ch.node) else { continue };
                let target = match ch.property {
                    Property::Weights => {
                        let morphs = node.mesh.map_or(0, |m| morph_count(scene, m));
                        if !mesh_on[ch.node] || morphs == 0 {
                            continue;
                        }
                        self.carrier[ch.node]
                    }
                    _ => self.of_scene[ch.node],
                };
                let Some(target) = target else { continue };
                if seen.insert((target, ch.property as u8)) {
                    channels.push((ch, target));
                }
            }
            if !channels.is_empty() {
                out.push(PlannedClip { name: clip.name.clone(), channels });
            }
        }
        out
    }

    /// The skins of the skinned meshes written.  A joint a skin names twice
    /// or that doesn't exist gets a stand-in node, since glTF wants every
    /// joint once and the vertices already name each position.
    fn finish(mut self, clips: Vec<PlannedClip<'a>>) -> Plan<'a> {
        let scene = self.scene;
        let mut skins: Vec<PlannedSkin> = Vec::new();
        let mut skin_of_scene: HashMap<usize, usize> = HashMap::new();
        for (carrier, s) in std::mem::take(&mut self.skin_uses) {
            let index = match skin_of_scene.get(&s) {
                Some(&i) => i,
                None => {
                    let skin = &scene.skins[s];
                    let mut used = std::collections::HashSet::new();
                    let mut joints = Vec::with_capacity(skin.joints.len());
                    for (k, &j) in skin.joints.iter().enumerate() {
                        let out = self.of_scene.get(j).copied().flatten();
                        let joint = match out {
                            Some(o) if used.insert(o) => o,
                            Some(o) => {
                                let name = format!("{} (again)", self.nodes[o].name);
                                self.push(OutNode::new(name, Transform::IDENTITY), Some(o))
                            }
                            None => self.push_root(OutNode::new(format!("joint {k}"), Transform::IDENTITY)),
                        };
                        used.insert(joint);
                        joints.push(joint);
                    }
                    self.join_roots(&joints);
                    let inverse_bind = (0..joints.len())
                        .map(|k| clean_inverse_bind(skin.inverse_bind.get(k).copied().unwrap_or(Mat4::IDENTITY)))
                        .collect();
                    let skeleton = skin.skeleton.and_then(|s| self.of_scene.get(s).copied().flatten()).filter(|&s| {
                        let parents = self.parents();
                        joints.iter().all(|&j| is_ancestor_or_self(&parents, s, j))
                    });
                    skins.push(PlannedSkin { name: skin.name.clone(), joints, inverse_bind, skeleton });
                    skin_of_scene.insert(s, skins.len() - 1);
                    skins.len() - 1
                }
            };
            if let Some(mesh) = &mut self.nodes[carrier].mesh {
                mesh.skin = Some(index);
            }
        }
        Plan { nodes: self.nodes, roots: self.roots, meshes: self.meshes, skins, clips, warnings: self.warnings }
    }

    /// glTF wants a skin's joints under one root.  Skeletons with several
    /// root bones (MikuMikuDance's, whose physics and IK bones hang from
    /// the top) get a new root without a transform above their roots, which
    /// moves nothing.
    fn join_roots(&mut self, joints: &[usize]) {
        let parents = self.parents();
        let top = |mut x: usize| {
            for _ in 0..parents.len() {
                match parents[x] {
                    Some(p) => x = p,
                    None => break,
                }
            }
            x
        };
        let mut tops: Vec<usize> = joints.iter().map(|&j| top(j)).collect();
        tops.sort_unstable();
        tops.dedup();
        if tops.len() < 2 {
            return;
        }
        self.roots.retain(|r| !tops.contains(r));
        let mut root = OutNode::new("skeleton".into(), Transform::IDENTITY);
        root.children = tops;
        self.push_root(root);
    }

    /// Each output node's parent.
    fn parents(&self) -> Vec<Option<usize>> {
        let mut parent = vec![None; self.nodes.len()];
        for (i, n) in self.nodes.iter().enumerate() {
            for &c in &n.children {
                parent[c] = Some(i);
            }
        }
        parent
    }
}

fn is_ancestor_or_self(parents: &[Option<usize>], ancestor: usize, node: usize) -> bool {
    let mut x = Some(node);
    for _ in 0..=parents.len() {
        match x {
            Some(v) if v == ancestor => return true,
            Some(v) => x = parents.get(v).copied().flatten(),
            None => return false,
        }
    }
    false
}

/// An inverse bind matrix glTF accepts: finite, with an affine last row.
fn clean_inverse_bind(m: Mat4) -> Mat4 {
    if !m.is_finite() {
        return Mat4::IDENTITY;
    }
    let mut a = m.to_cols_array();
    a[3] = 0.0;
    a[7] = 0.0;
    a[11] = 0.0;
    a[15] = 1.0;
    Mat4::from_cols_array(&a)
}

//endregion
