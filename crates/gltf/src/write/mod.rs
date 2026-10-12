//! Writing Slerp's scene model as GLB: the whole scene, one node (or
//! several) and what hangs under it, one mesh alone, or one animation clip
//! with what it moves.  This is what File > Save as GLB, `slerp convert` and the
//! Contents panel's exports use, so any 3D file Slerp opens can be passed
//! on to Blender, a game engine or a web viewer.
//!
//! Everything the scene model holds that glTF can say is written: nodes
//! and their hierarchy, meshes of every topology with normals, tangents,
//! two UV sets and vertex colours, metallic-roughness materials with the
//! extensions the loader reads, textures and samplers, skins, named morph
//! targets, and clips with their interpolation.  Pictures keep their
//! original files where the loader kept them and glTF can hold them (PNG,
//! JPEG, Basis Universal KTX2, WebP); everything else is written as 8-bit
//! PNG.  What glTF can't say is approximated and reported in
//! [`Glb::warnings`]: floating-point pictures become 8-bit, Quake lightmaps
//! become occlusion maps, Doom's sprites become crossed quads, Gaussian
//! splats become coloured points.
//!
//! The writer never panics on odd scenes.  Primitives with no vertices,
//! indices past the end, non-finite numbers, unnormalised skin weights and
//! keyframes out of order are repaired or left out with a warning, so the
//! output passes the Khronos glTF Validator.
//!
//! ```no_run
//! # fn f(scene: &slerp_scene::Scene) -> Result<(), slerp_gltf::WriteError> {
//! use slerp_gltf::{GlbOptions, Selection, write_glb};
//! let glb = write_glb(scene, &Selection::Scene, &GlbOptions::default())?;
//! std::fs::write("out.glb", &glb.bytes).ok();
//! # Ok(()) }
//! ```

mod anim;
mod bin;
mod image;
mod material;
mod mesh;
mod plan;
#[cfg(test)]
mod tests;

use bin::Bin;
use serde_json::{Map, Value, json};
use slerp_scene::Scene;
use std::collections::BTreeSet;

/*┌─────────────────╮
| Interface       |
└─────────────────╯*/
//region Interface

/// Which part of a scene to write.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Selection {
    /// Every node, mesh and clip, with a level's markers as empty nodes and
    /// Gaussian splats as coloured points.
    Scene,
    /// One node (an index into `Scene::nodes`) and everything under it,
    /// plus the joints its skinned meshes need, and the channels of the
    /// scene's clips that move what is written.
    Node(usize),
    /// Several nodes, each as [`Selection::Node`] writes it, side by side:
    /// a Blender collection's objects.  A node under another one listed is
    /// written once, in its place under it.
    Nodes(Vec<usize>),
    /// One mesh (an index into `Scene::meshes`) alone: its primitives,
    /// materials, textures and morph targets, without skin or clips.
    Mesh(usize),
    /// One clip (an index into `Scene::animations`) with the nodes it
    /// moves and the meshes they carry or skin.  With `skeleton_only` the
    /// meshes are left out, leaving the joints and the motion.
    Animation { index: usize, skeleton_only: bool },
}

/// Where the written part sits in the new file.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Placement {
    /// Where it sat in the scene: the transforms of parents that aren't
    /// written are folded into the written roots.  A mesh alone sits where
    /// the first node showing it put it.
    #[default]
    InPlace,
    /// Moved so the centre of the box around it, in its rest pose, is at
    /// the origin.
    Centred,
}

/// How to write.
#[derive(Clone, Debug)]
pub struct GlbOptions {
    pub placement: Placement,
    /// Write clips with a `Scene` or `Node` selection.
    pub animations: bool,
    /// Embed pictures' original files where the loader kept them and glTF
    /// can hold them (PNG, JPEG, Basis Universal KTX2, WebP).  When false,
    /// or when there is no original, pictures are written as PNG, which
    /// every glTF reader opens.
    pub original_images: bool,
    /// Write hidden nodes, marked hidden with `KHR_node_visibility`.  When
    /// false their meshes are left out.
    pub hidden: bool,
}

impl Default for GlbOptions {
    fn default() -> Self {
        GlbOptions { placement: Placement::InPlace, animations: true, original_images: true, hidden: true }
    }
}

/// A written GLB file.
pub struct Glb {
    pub bytes: Vec<u8>,
    /// What couldn't be written as it was, in sentences for people.
    pub warnings: Vec<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum WriteError {
    /// The selection names a node, mesh or clip the scene doesn't have.
    #[error("{0}")]
    Selection(String),
    /// The file would pass GLB's limit of 4 GB.
    #[error("too large for a GLB file: {0}")]
    TooLarge(String),
}

/// Writes `what` of `scene` as a GLB file.  Fails only for a selection
/// the scene doesn't have or an output larger than GLB allows; everything
/// else that can't be written is repaired or left out, and said in
/// [`Glb::warnings`].
pub fn write_glb(scene: &Scene, what: &Selection, options: &GlbOptions) -> Result<Glb, WriteError> {
    // 1. What to write: output nodes and the scene's meshes, skins and
    //    clips they use.
    let plan = plan::build(scene, what, options)?;
    let mut w = Writer::new(scene, options);
    for warning in &plan.warnings {
        w.warn(warning.clone());
    }

    // 2. Materials, and the pictures they use, encoded in parallel.
    let prepared = mesh::prepare(&w, &plan);
    let materials = material::write(&mut w, &prepared.materials)?;

    // 3. Geometry, skins, nodes and clips into the binary chunk.
    let meshes = mesh::write(&mut w, &plan, &prepared, &materials)?;
    anim::write_skins(&mut w, &plan)?;
    anim::write_clips(&mut w, &plan, &meshes)?;
    let nodes = write_nodes(&mut w, &plan, &meshes);
    if !scene.expressions.is_empty() || scene.humanoid.is_some() {
        w.warn("VRM's expressions and humanoid bone map aren't part of plain glTF and were left out");
    }

    // 4. The JSON and the container.
    let mut root = Map::new();
    let mut asset = Map::new();
    asset.insert("version".into(), "2.0".into());
    asset.insert("generator".into(), format!("Slerp {}", env!("CARGO_PKG_VERSION")).into());
    if let Some((_, c)) = scene.info.details.iter().find(|(k, _)| k == "Copyright") {
        asset.insert("copyright".into(), c.clone().into());
    }
    root.insert("asset".into(), asset.into());
    let mut scene_json = Map::new();
    if !scene.name.is_empty() {
        scene_json.insert("name".into(), scene.name.clone().into());
    }
    if !plan.roots.is_empty() {
        scene_json.insert("nodes".into(), json!(plan.roots));
    }
    root.insert("scene".into(), 0.into());
    root.insert("scenes".into(), json!([scene_json]));
    w.finish(root, nodes)
}

//endregion

/*┌─────────────────╮
| Writer          |
└─────────────────╯*/
//region Writer

/// More warnings than this are dropped: a broken file can repeat one
/// problem a million times.
const MAX_WARNINGS: usize = 100;

/// The file being written: the binary chunk with its views and accessors,
/// the JSON arrays the parts fill in, extensions, and warnings.
pub(crate) struct Writer<'a> {
    pub scene: &'a Scene,
    pub options: &'a GlbOptions,
    pub bin: Bin,
    pub images: Vec<Value>,
    pub samplers: Vec<Value>,
    pub textures: Vec<Value>,
    pub materials: Vec<Value>,
    pub meshes: Vec<Value>,
    pub skins: Vec<Value>,
    pub animations: Vec<Value>,
    used: BTreeSet<&'static str>,
    required: BTreeSet<&'static str>,
    warnings: Vec<String>,
}

impl<'a> Writer<'a> {
    fn new(scene: &'a Scene, options: &'a GlbOptions) -> Self {
        Writer {
            scene,
            options,
            bin: Bin::default(),
            images: Vec::new(),
            samplers: Vec::new(),
            textures: Vec::new(),
            materials: Vec::new(),
            meshes: Vec::new(),
            skins: Vec::new(),
            animations: Vec::new(),
            used: BTreeSet::new(),
            required: BTreeSet::new(),
            warnings: Vec::new(),
        }
    }

    /// Records a problem worked around.  Repeats are dropped, and so is
    /// everything after the first hundred.
    pub fn warn(&mut self, message: impl Into<String>) {
        let message = message.into();
        if self.warnings.len() < MAX_WARNINGS && !self.warnings.contains(&message) {
            self.warnings.push(message);
        }
    }

    /// Notes an extension the file uses; `required` ones must be understood
    /// to read the file at all.
    pub fn extension(&mut self, name: &'static str, required: bool) {
        self.used.insert(name);
        if required {
            self.required.insert(name);
        }
    }

    /// Assembles the JSON and the GLB container.
    fn finish(mut self, mut root: Map<String, Value>, nodes: Vec<Value>) -> Result<Glb, WriteError> {
        let arrays = [
            ("nodes", nodes),
            ("meshes", std::mem::take(&mut self.meshes)),
            ("materials", std::mem::take(&mut self.materials)),
            ("textures", std::mem::take(&mut self.textures)),
            ("images", std::mem::take(&mut self.images)),
            ("samplers", std::mem::take(&mut self.samplers)),
            ("skins", std::mem::take(&mut self.skins)),
            ("animations", std::mem::take(&mut self.animations)),
            ("accessors", std::mem::take(&mut self.bin.accessors)),
            ("bufferViews", std::mem::take(&mut self.bin.views)),
        ];
        for (name, array) in arrays {
            // glTF forbids empty arrays.
            if !array.is_empty() {
                root.insert(name.into(), Value::Array(array));
            }
        }
        if !self.bin.data.is_empty() {
            root.insert("buffers".into(), json!([{ "byteLength": self.bin.data.len() }]));
        }
        if !self.used.is_empty() {
            root.insert("extensionsUsed".into(), json!(self.used));
        }
        if !self.required.is_empty() {
            root.insert("extensionsRequired".into(), json!(self.required));
        }
        let json = serde_json::to_vec(&Value::Object(root)).unwrap_or_default();
        let bytes = glb(&json, &self.bin.data)?;
        Ok(Glb { bytes, warnings: self.warnings })
    }
}

//endregion

/*┌─────────────────╮
| Nodes and GLB   |
└─────────────────╯*/
//region Nodes and GLB

/// The nodes' JSON.  `meshes` maps each planned mesh to its index in the
/// file, or `None` when nothing of it could be written; a node then keeps
/// its place without the mesh, skin or weights.
fn write_nodes(w: &mut Writer, plan: &plan::Plan, meshes: &[Option<usize>]) -> Vec<Value> {
    use slerp_scene::glam::{Quat, Vec3};
    let mut used_visibility = false;
    let out: Vec<Value> = plan
        .nodes
        .iter()
        .map(|n| {
            let mut o = Map::new();
            if !n.name.is_empty() {
                o.insert("name".into(), n.name.clone().into());
            }
            let t = &n.transform;
            if t.translation != Vec3::ZERO {
                o.insert("translation".into(), bin::nums(&t.translation.to_array()));
            }
            if t.rotation != Quat::IDENTITY {
                o.insert("rotation".into(), bin::nums(&t.rotation.to_array()));
            }
            if t.scale != Vec3::ONE {
                o.insert("scale".into(), bin::nums(&t.scale.to_array()));
            }
            if !n.children.is_empty() {
                o.insert("children".into(), json!(n.children));
            }
            if let Some(m) = n.mesh
                && let Some(index) = meshes.get(m.index).copied().flatten()
            {
                o.insert("mesh".into(), index.into());
                if let Some(s) = m.skin {
                    o.insert("skin".into(), s.into());
                }
                if !n.weights.is_empty() {
                    o.insert("weights".into(), bin::nums(&n.weights));
                }
            }
            if n.hidden {
                used_visibility = true;
                o.insert("extensions".into(), json!({ "KHR_node_visibility": { "visible": false } }));
            }
            Value::Object(o)
        })
        .collect();
    if used_visibility {
        w.extension("KHR_node_visibility", false);
    }
    out
}

/// The GLB container: a 12-byte header, the JSON chunk padded with spaces
/// and the binary chunk padded with zeros, each to 4 bytes.
fn glb(json: &[u8], bin: &[u8]) -> Result<Vec<u8>, WriteError> {
    let json_len = json.len().next_multiple_of(4);
    let bin_len = bin.len().next_multiple_of(4);
    let total = 12 + 8 + json_len + if bin.is_empty() { 0 } else { 8 + bin_len };
    let total32 = u32::try_from(total).map_err(|_| WriteError::TooLarge(format!("{} MB", total >> 20)))?;
    let mut out = Vec::with_capacity(total);
    out.extend_from_slice(b"glTF");
    out.extend_from_slice(&2u32.to_le_bytes());
    out.extend_from_slice(&total32.to_le_bytes());
    out.extend_from_slice(&(json_len as u32).to_le_bytes());
    out.extend_from_slice(b"JSON");
    out.extend_from_slice(json);
    out.resize(20 + json_len, b' ');
    if !bin.is_empty() {
        out.extend_from_slice(&(bin_len as u32).to_le_bytes());
        out.extend_from_slice(b"BIN\0");
        out.extend_from_slice(bin);
        out.resize(total, 0);
    }
    Ok(out)
}

//endregion
