//! Maya scenes, read without Maya: Maya ASCII (`.ma`, a MEL script) and
//! Maya binary (`.mb`, the same data in IFF chunks).
//!
//! What is drawn: the DAG's transforms (with pivots, rotate order and
//! joint orientation), mesh shapes whose geometry the file stores (points,
//! edges, faces with holes, UV sets, normals, colour sets), meshes whose
//! history is a primitive's creation node (`polyCube`, `polySphere`,
//! `polyCylinder`, `polyCone`, `polyPlane`, `polyTorus`), and their shading
//! groups' materials with file textures.  Meshes behind deformers show
//! their rest pose.  Meshes that exist only as history Maya would have to
//! evaluate are listed in the inspector and left out, as are references to
//! other files, NURBS, curves, cameras and lights.
//!
//! Maya files carry no preview picture, so thumbnails render the scene.

mod anim;
pub mod build;
pub mod graph;
pub mod ma;
mod material;
pub mod mb;
pub mod mesh;
mod prims;
mod skin;

use slerp_scene::{Limits, LoadError, Scene};
use std::path::Path;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MayaFormat {
    Ascii,
    Binary,
}

impl MayaFormat {
    pub fn name(self) -> &'static str {
        match self {
            MayaFormat::Ascii => "Maya ASCII",
            MayaFormat::Binary => "Maya binary",
        }
    }
}

/// Recognises a Maya file by its first bytes: "//Maya ASCII", or an IFF
/// group of type "Maya".
pub fn sniff(head: &[u8]) -> Option<MayaFormat> {
    if ma::sniff(head) {
        Some(MayaFormat::Ascii)
    } else if mb::sniff(head) {
        Some(MayaFormat::Binary)
    } else {
        None
    }
}

/// Loads a Maya file as a scene.
pub fn load_path(path: &Path, limits: &Limits) -> Result<Scene, LoadError> {
    let size = std::fs::metadata(path)?.len();
    if size > limits.max_file_bytes {
        return Err(LoadError::TooLarge(format!("{size} bytes")));
    }
    load(&std::fs::read(path)?, Some(path), limits)
}

/// Loads a Maya file held in memory.  `path` is where it came from, for
/// finding its textures, which the scene lists as its neighbours.
pub fn load(bytes: &[u8], path: Option<&Path>, limits: &Limits) -> Result<Scene, LoadError> {
    let (result, neighbours) = slerp_scene::collect_neighbours(|| load_inner(bytes, path, limits));
    let mut scene = result?;
    scene.info.neighbours = neighbours;
    Ok(scene)
}

fn load_inner(bytes: &[u8], path: Option<&Path>, limits: &Limits) -> Result<Scene, LoadError> {
    let format = sniff(bytes).ok_or(LoadError::NotMine)?;
    // A panic would mean a bug in the readers; a damaged file mustn't take
    // the viewer or thumbnailer down with it.
    let built = std::panic::catch_unwind(|| {
        let mut graph = match format {
            MayaFormat::Ascii => ma::parse(&String::from_utf8_lossy(bytes)),
            MayaFormat::Binary => mb::parse(bytes)?,
        };
        // Maya keeps the up axis in a script node it runs on opening:
        // `upAxis -ax "z"`.
        graph.z_up |= graph.nodes.iter().any(|n| {
            matches!(n.kind.as_str(), "script" | "SCRP")
                && n.text("b").is_some_and(|b| b.contains("upAxis -ax \"z\"") || b.contains("upAxis -ax z"))
        });
        let (scene, report) = build::scene(&graph, path, limits)?;
        Ok::<_, LoadError>((graph, scene, report))
    })
    .unwrap_or_else(|_| Err(LoadError::invalid("Maya reader failed on this file")));
    let (graph, mut scene, report) = built?;
    if scene.meshes.is_empty() {
        let mut why = "no mesh in this file stores its geometry".to_string();
        if !report.history_only.is_empty() {
            why = format!(
                "the meshes here exist only as construction history, which needs Maya to evaluate: {}",
                report.history_only.join("; ")
            );
        } else if !graph.references.is_empty() {
            why = format!(
                "the scene's meshes are in referenced files, which aren't loaded: {}",
                graph.references.join(", ")
            );
        }
        return Err(LoadError::Unsupported(why));
    }
    describe(format, &graph, &report, &mut scene);
    Ok(scene)
}

/// The inspector's facts.
fn describe(format: MayaFormat, g: &graph::Graph, r: &build::Report, scene: &mut Scene) {
    let info = &mut scene.info;
    let version = g.version.trim();
    info.format = if version.is_empty() {
        format!("{} scene", format.name())
    } else {
        format!("{} scene (Maya {version})", format.name())
    };
    info.generator = g.file_info.iter().find(|(k, _)| k == "product").map(|(_, v)| v.clone());
    let mut d: Vec<(String, String)> = Vec::new();
    let unit = [(0.001, "millimetres"), (0.01, "centimetres"), (1.0, "metres"), (0.0254, "inches"), (0.3048, "feet")]
        .iter()
        .find(|(m, _)| *m == g.metres)
        .map_or("other units", |(_, name)| name);
    d.push(("Units".into(), format!("{unit}, {} up", if g.z_up { "Z" } else { "Y" })));
    d.push(("Nodes".into(), g.nodes.len().to_string()));
    d.push(("Meshes".into(), r.meshes.to_string()));
    d.push(("Materials".into(), r.materials.to_string()));
    if !r.primitives.is_empty() {
        d.push(("Rebuilt primitives".into(), r.primitives.join(", ")));
    }
    if !r.posed.is_empty() {
        d.push(("Posed by their skin".into(), r.posed.join(", ")));
    }
    if !r.rest_pose.is_empty() {
        d.push(("Shown before history".into(), r.rest_pose.join("; ")));
    }
    if !r.history_only.is_empty() {
        d.push(("Left out (history only)".into(), r.history_only.join("; ")));
    }
    if !g.references.is_empty() {
        d.push(("References (not loaded)".into(), g.references.join(", ")));
    }
    if !r.other_shapes.is_empty() {
        let mut list: Vec<_> = r.other_shapes.iter().map(|(k, v)| format!("{k} x{v}")).collect();
        list.sort();
        d.push(("Not drawn".into(), list.join(", ")));
    }
    if r.broken_faces > 0 {
        info.warnings
            .push(format!("{} faces referred to edges or vertices that don't exist and were skipped", r.broken_faces));
    }
    info.warnings.extend(g.warnings.iter().cloned());
    d.append(&mut info.details);
    info.details = d;
}

#[cfg(test)]
mod tests;
