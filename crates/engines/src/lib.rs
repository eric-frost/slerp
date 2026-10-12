//! Game-engine scene files: Godot's text and binary scenes (`.tscn`,
//! `.escn`, `.scn`) and resources holding meshes (`ArrayMesh`, primitive
//! meshes, `MeshLibrary`), and Unity's text-serialized scenes and prefabs
//! (`.unity`, `.prefab`).
//!
//! An engine scene is mostly references: nodes place models, meshes and
//! materials that live in other files of the project.  So the loaders here
//! find the project around the file (the folder holding `project.godot`,
//! or Unity's `Assets/` and `ProjectSettings/`), resolve the references
//! (`res://` paths; Unity GUIDs through the project's `.meta` files) and
//! load what they point to with the other loader crates: glTF through
//! `slerp-gltf`, FBX and OBJ through `slerp-models`, and Godot materials
//! through `slerp-materials`.  A model placed many times is loaded once and
//! its meshes are shared by every placement.
//!
//! Geometry the engines generate themselves is generated here: Godot's
//! primitive meshes and CSG shapes, GridMap cells, and Unity's built-in
//! cube, sphere, capsule, cylinder, plane and quad.  What can't be shown is
//! left out with a warning in `Info::warnings`: CSG subtraction and
//! intersection (drawn as if the subtracted shape weren't there), Unity
//! terrain, custom shaders (approximated from their parameter names).
//!
//! Everything here runs on files a user merely browses past, including
//! downloads, so nothing trusts the file: binary readers check every count
//! against the bytes present, recursion through nested scenes and prefabs
//! is bounded and cycle-checked, and the number of nodes and triangles
//! placed is capped.

mod builder;
mod godot;
mod shapes;
mod unity;

use slerp_scene::{Asset, Limits, LoadError};
use std::path::Path;

/// The file formats this crate reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EngineFormat {
    /// A Godot text scene or resource (`[gd_scene ...]`, or a
    /// `[gd_resource ...]` holding a mesh, mesh library or packed scene).
    GodotText,
    /// A Godot binary resource (`RSRC`, or `RSCC` when compressed): a
    /// `.scn` scene, a `.res` or `.mesh` mesh.
    GodotBinary,
    /// A Unity scene or prefab serialized as text (YAML).
    UnityYaml,
    /// A Unity scene or prefab serialized in Unity's binary format, which
    /// is recognised only to be refused with a clear message.
    UnityBinary,
}

impl EngineFormat {
    pub fn name(self) -> &'static str {
        match self {
            EngineFormat::GodotText => "Godot text scene",
            EngineFormat::GodotBinary => "Godot binary scene",
            EngineFormat::UnityYaml => "Unity scene",
            EngineFormat::UnityBinary => "Unity binary scene",
        }
    }
}

/// Godot resource types this crate shows.  Materials stay with
/// `slerp-materials`; other resources (scripts' data, themes, tile sets)
/// aren't pictures of anything.
const GODOT_RESOURCE_TYPES: &[&str] = &[
    "PackedScene",
    "ArrayMesh",
    "Mesh",
    "MeshLibrary",
    "BoxMesh",
    "CubeMesh",
    "SphereMesh",
    "CapsuleMesh",
    "CylinderMesh",
    "PlaneMesh",
    "QuadMesh",
    "PrismMesh",
    "TorusMesh",
];

/// Recognises a format from the file's name and first bytes.  `head` should
/// hold the first few kilobytes of the file, or all of it when shorter.
pub fn sniff(path: &Path, head: &[u8]) -> Option<EngineFormat> {
    let ext = path.extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase()).unwrap_or_default();
    let text = head.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(head);
    // 1. Godot: text scenes always start with their header.  Text
    //    resources are claimed only for the types above, so material
    //    `.tres` files still reach the materials crate.
    if text.starts_with(b"[gd_scene") {
        return Some(EngineFormat::GodotText);
    }
    if text.starts_with(b"[gd_resource") {
        let line_end = text.iter().position(|&b| b == b'\n').unwrap_or(text.len());
        let kind = godot::text::header_type(&text[..line_end])?;
        return GODOT_RESOURCE_TYPES.contains(&kind.as_str()).then_some(EngineFormat::GodotText);
    }
    if head.starts_with(b"RSRC") || head.starts_with(b"RSCC") {
        // Compressed files hide their type, so they go by extension.
        if head.starts_with(b"RSCC") {
            return matches!(ext.as_str(), "scn" | "res" | "mesh" | "meshlib").then_some(EngineFormat::GodotBinary);
        }
        let kind = godot::binary::header_type(head)?;
        return GODOT_RESOURCE_TYPES.contains(&kind.as_str()).then_some(EngineFormat::GodotBinary);
    }
    // 2. Unity: YAML with Unity's tag directive, or the binary
    //    serialization by extension.
    if text.starts_with(b"%YAML") && contains(&text[..text.len().min(256)], b"tag:unity3d.com") {
        return matches!(ext.as_str(), "unity" | "prefab").then_some(EngineFormat::UnityYaml);
    }
    if matches!(ext.as_str(), "unity" | "prefab") && unity::looks_binary(head) {
        return Some(EngineFormat::UnityBinary);
    }
    None
}

/// Loads a file of a format `sniff` recognised.  `bytes` is the whole file;
/// `path` locates the project and the files the scene refers to, which are
/// read only when `limits.neighbours` allows.  Scenes become
/// `Asset::Scene`, large ones as `SceneKind::Level`.
pub fn load(format: EngineFormat, path: &Path, bytes: &[u8], limits: &Limits) -> Result<Asset, LoadError> {
    if bytes.len() as u64 > limits.max_file_bytes {
        return Err(LoadError::TooLarge(format!("{} bytes", bytes.len())));
    }
    // The readers are written not to panic; this is a last line of defence
    // so a bug in one of them, or in a loader they call, costs one
    // thumbnail rather than the helper process.
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        slerp_scene::collect_neighbours(|| match format {
            EngineFormat::GodotText | EngineFormat::GodotBinary => godot::load(format, path, bytes, limits),
            EngineFormat::UnityYaml => unity::load(path, bytes, limits),
            EngineFormat::UnityBinary => Err(LoadError::Unsupported(
                "Unity scene saved in binary form; Slerp reads scenes saved as text \
                 (Project Settings, Editor, Asset Serialization: Force Text)"
                    .into(),
            )),
        })
    }));
    match result {
        Ok((Ok(mut scene), neighbours)) => {
            for n in neighbours {
                if !scene.info.neighbours.contains(&n) {
                    scene.info.neighbours.push(n);
                }
            }
            Ok(Asset::Scene(scene))
        }
        Ok((Err(e), _)) => Err(e),
        Err(_) => Err(LoadError::invalid(format!("{} loader failed unexpectedly", format.name()))),
    }
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

#[cfg(test)]
mod tests;
