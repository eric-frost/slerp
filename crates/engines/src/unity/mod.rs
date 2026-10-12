//! Unity scenes (`.unity`) and prefabs (`.prefab`) saved as text.
//!
//! A Unity scene file is a flat list of objects (GameObjects, their
//! Transforms and components, prefab instances), each a YAML document with
//! a file id; objects refer to each other by file id, and to other assets
//! by GUID and file id.  Loading:
//!
//! 1. `scene::Ctx::tree` turns a file into a tree of `UNode`s: one per
//!    GameObject, its Transform, mesh, materials, light or camera.  Prefab
//!    instances bring a copy of their prefab's tree (built once per prefab,
//!    recursively for nested prefabs) with the instance's modifications
//!    applied; instances of model files (FBX, OBJ) become nodes that place
//!    the model.
//! 2. `scene::Ctx::emit` converts the tree to Slerp's right-handed space
//!    (Unity is left-handed: Z is mirrored) and adds the meshes: Unity's
//!    built-in primitives, meshes serialized in YAML (ProBuilder's), and
//!    meshes inside model files, found through the model's `.meta` file.
//!
//! Binary-serialized scenes and terrain are refused or left out with a
//! message.

pub(crate) mod material;
mod mesh;
mod project;
mod scene;
mod xxh64;
pub(crate) mod yaml;

use slerp_scene::{Limits, LoadError, Scene};
use std::path::Path;

/// True for the start of a scene or prefab in Unity's binary
/// serialization: a header of big-endian sizes and a format version, then
/// the Unity version as text.
pub(crate) fn looks_binary(head: &[u8]) -> bool {
    if head.len() < 48 || head.starts_with(b"%YAML") {
        return false;
    }
    let version = u32::from_be_bytes([head[8], head[9], head[10], head[11]]);
    (9..=40).contains(&version) && head.windows(2).take(64).any(|w| w[0].is_ascii_digit() && w[1] == b'.')
}

pub(crate) fn load(path: &Path, bytes: &[u8], limits: &Limits) -> Result<Scene, LoadError> {
    scene::load(path, bytes, limits)
}

#[cfg(test)]
mod tests;
