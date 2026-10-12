//! Saving scene pieces as GLB files, through the GLB writer of
//! `slerp_gltf`.  This is the one place that knows the writer's interface:
//! the rest of `contents` says what to save as a [`Selection`].

use super::{ContentsError, Result};
use slerp_gltf as writer;
use slerp_scene::Scene;

/// What part of a scene to save.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Selection {
    /// Everything.
    Scene,
    /// A node with everything beneath it.
    Node(usize),
    /// Several nodes, each with everything beneath it: a collection.
    Nodes(Vec<usize>),
    /// One mesh, as the node that uses it first places it.
    Mesh(usize),
    /// A material, worn by a ball.
    Material(usize),
    /// A skin's joints: the joint hierarchy from its root.
    Skeleton(usize),
    /// A clip with the meshes it moves, or with the skeleton alone.
    Animation { clip: usize, skeleton_only: bool },
}

/// How to save it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct GlbOptions {
    /// Move what is saved to the origin instead of keeping its place in the
    /// scene.
    pub centre: bool,
}

/// The GLB file of `selection`.
pub(crate) fn write(scene: &Scene, selection: &Selection, options: GlbOptions) -> Result<Vec<u8>> {
    let what = match selection {
        Selection::Scene => writer::Selection::Scene,
        Selection::Node(i) => writer::Selection::Node(*i),
        Selection::Nodes(nodes) => writer::Selection::Nodes(nodes.clone()),
        Selection::Mesh(i) => writer::Selection::Mesh(*i),
        Selection::Animation { clip, skeleton_only } => {
            writer::Selection::Animation { index: *clip, skeleton_only: *skeleton_only }
        }
        Selection::Skeleton(s) => {
            let root = skeleton_root(scene, *s)
                .ok_or_else(|| ContentsError::Unsupported("the skeleton has no joints".into()))?;
            writer::Selection::Node(root)
        }
        Selection::Material(m) => {
            let ball = material_ball(scene, *m)?;
            return write(&ball, &Selection::Scene, options);
        }
    };
    let placement = if options.centre { writer::Placement::Centred } else { writer::Placement::InPlace };
    let opts = writer::GlbOptions { placement, ..Default::default() };
    writer::write_glb(scene, &what, &opts).map(|glb| glb.bytes).map_err(|e| ContentsError::Convert(e.to_string()))
}

/// The node a skin's joint hierarchy hangs from: the skin's own skeleton
/// root, else the first joint whose parent isn't a joint.
fn skeleton_root(scene: &Scene, skin: usize) -> Option<usize> {
    let skin = scene.skins.get(skin)?;
    if let Some(root) = skin.skeleton.filter(|&r| r < scene.nodes.len()) {
        return Some(root);
    }
    let parents = scene.parents();
    let joints: std::collections::HashSet<usize> = skin.joints.iter().copied().collect();
    skin.joints
        .iter()
        .copied()
        .filter(|&j| j < scene.nodes.len())
        .find(|&j| parents.get(j).copied().flatten().is_none_or(|p| !joints.contains(&p)))
}

/// A ball wearing material `m` of `scene`, with the scene's textures.
fn material_ball(scene: &Scene, m: usize) -> Result<Scene> {
    let material = scene.materials.get(m).ok_or(ContentsError::NoSuchItem)?;
    let asset = slerp_scene::MaterialAsset {
        name: material.name.clone(),
        material: material.clone(),
        textures: scene.textures.clone(),
        preview: None,
        info: Default::default(),
    };
    Ok(Scene::material_ball(&asset, slerp_scene::Shape::Sphere, 64))
}
