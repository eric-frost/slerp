//! 3ds Max scenes (`.max`), read without 3ds Max.
//!
//! A `.max` file is an OLE compound document.  Its `Scene` stream holds
//! the scene's objects as a tree of chunks, named by the `ClassDirectory3`
//! stream (see [`file`]).  What is drawn: nodes with their hierarchy and
//! transforms; Editable Mesh and Editable Poly objects with UVs, smoothing
//! groups and stored normals; the standard primitives rebuilt from their
//! parameters; TurboSmooth and MeshSmooth when they sit on such an object;
//! and Standard, Physical and Multi/Sub-Object materials with their bitmap
//! textures, found beside the file.  Other modifiers and object types are
//! listed in the inspector, and the object is shown without them.
//!
//! Blender's `io_scene_max` extension showed this is feasible; this reader
//! was written from the files themselves and the 3ds Max SDK's public
//! documentation, not from that code.
//!
//! Thumbnails keep using the picture Max saved in the file (read by
//! `slerp-previews`); this crate is for the viewer, and for files without
//! one.

pub mod build;
pub mod chunks;
pub mod file;
mod material;
mod mesh;
pub mod ole;
mod prims;

use slerp_scene::{Limits, LoadError, Scene};
use std::collections::HashMap;
use std::path::Path;

/// Whether a file is a 3ds Max scene: an OLE compound document named
/// `.max` (other OLE files, such as old Office documents, look alike).
pub fn sniff(path: &Path, head: &[u8]) -> bool {
    head.starts_with(ole::MAGIC) && path.extension().is_some_and(|e| e.eq_ignore_ascii_case("max"))
}

/// Loads a 3ds Max file as a scene.
pub fn load_path(path: &Path, limits: &Limits) -> Result<Scene, LoadError> {
    let size = std::fs::metadata(path)?.len();
    if size > limits.max_file_bytes {
        return Err(LoadError::TooLarge(format!("{size} bytes")));
    }
    load(&std::fs::read(path)?, Some(path), limits)
}

/// Loads a 3ds Max file held in memory; `path` is where it came from, for
/// finding its textures, which the scene lists as its neighbours.
pub fn load(bytes: &[u8], path: Option<&Path>, limits: &Limits) -> Result<Scene, LoadError> {
    let (result, neighbours) = slerp_scene::collect_neighbours(|| {
        // A panic would mean a bug in the reader; a damaged file mustn't
        // take the viewer or thumbnailer down with it.
        std::panic::catch_unwind(|| load_inner(bytes, path, limits))
            .unwrap_or_else(|_| Err(LoadError::invalid("3ds Max reader failed on this file")))
    });
    let mut scene = result?;
    scene.info.neighbours = neighbours;
    Ok(scene)
}

fn load_inner(bytes: &[u8], path: Option<&Path>, limits: &Limits) -> Result<Scene, LoadError> {
    let ole = ole::Ole::open(bytes)?;
    let cap = (bytes.len() as u64).saturating_mul(64).clamp(256 << 20, 4 << 30);
    // 1. The classes the scene's objects are made of.
    let dir = ["ClassDirectory3", "ClassDirectory2", "ClassDirectory"]
        .iter()
        .find_map(|n| ole.stream(n, cap).ok().flatten())
        .ok_or_else(|| LoadError::invalid("3ds Max file without a class directory"))?;
    let classes = class_directory(&dir);
    // 2. The assets (texture paths) by id.
    let assets = [("FileAssetMetaData3", 3), ("FileAssetMetaData2", 2)]
        .iter()
        .find_map(|&(n, strings)| ole.stream(n, cap).ok().flatten().map(|d| asset_list(&d, strings)))
        .unwrap_or_default();
    // 3. The scene's objects.
    let stream = ole.stream("Scene", cap)?.ok_or_else(|| LoadError::invalid("3ds Max file without a scene"))?;
    let data = chunks::decompress(stream, cap)?;
    let file = file::MaxFile::new(classes, chunks::parse(&data), assets);
    let (unit, metres) = units(&file);
    let (mut scene, report) = build::scene(&file, path, limits, metres)?;
    if scene.meshes.is_empty() {
        let mut list: Vec<String> = report.not_drawn.iter().map(|(k, v)| format!("{k} x{v}")).collect();
        list.sort();
        let why = if list.is_empty() {
            "nothing in this scene is geometry Slerp can rebuild".to_string()
        } else {
            format!("nothing in this scene is geometry Slerp can rebuild ({})", list.join(", "))
        };
        return Err(LoadError::Unsupported(why));
    }
    describe(&file, bytes, &report, unit, &mut scene);
    Ok(scene)
}

/// The classes in a class directory: per entry a container (0x2040) with
/// the DLL index, class id and superclass id (0x2060) and the name
/// (0x2042).
fn class_directory(data: &[u8]) -> Vec<file::Class> {
    chunks::parse(data)
        .iter()
        .map(|c| {
            let mut class = file::Class::default();
            if let Some(h) = c.child_data(0x2060) {
                class.dll = chunks::i32_at(h, 0).unwrap_or(-1);
                class.id = (chunks::u32_at(h, 4).unwrap_or(0), chunks::u32_at(h, 8).unwrap_or(0));
                class.super_id = chunks::u32_at(h, 12).unwrap_or(0);
            }
            if let Some(n) = c.child_data(0x2042) {
                class.name = chunks::utf16(n);
            }
            class
        })
        .collect()
}

/// The asset list: per asset a 16-byte id, then its type and its path
/// and, in the third version, its resolved path: `strings` UTF-16 strings,
/// each led by its length in characters and ended by a zero.
fn asset_list(d: &[u8], strings: usize) -> HashMap<[u8; 16], String> {
    let mut out = HashMap::new();
    let mut at = 0usize;
    let string = |at: &mut usize| -> Option<String> {
        let n = chunks::u32_at(d, *at)? as usize;
        let bytes = d.get(*at + 4..(*at + 4).checked_add(n.checked_mul(2)?)?)?;
        *at += 4 + n * 2 + 2;
        Some(chunks::utf16(bytes))
    };
    while at + 20 <= d.len() && out.len() < 1 << 16 {
        let Ok(id) = <[u8; 16]>::try_from(&d[at..at + 16]) else { break };
        at += 16;
        let mut parts = Vec::with_capacity(strings);
        for _ in 0..strings {
            match string(&mut at) {
                Some(s) => parts.push(s),
                None => return out,
            }
        }
        if let Some(path) = parts.get(1) {
            out.insert(id, path.clone());
        }
    }
    out
}

/// The system unit (0x8520 in the `Scene` object: type and scale) as a
/// name and in metres.
fn units(file: &file::MaxFile) -> (String, f32) {
    let scene_object = (0..file.objects.len()).rev().find(|&i| file.class_id(i) == Some((0x2222, 0)));
    let data = scene_object.and_then(|i| file.objects[i].child_data(0x8520));
    let (kind, scale) = data
        .and_then(|d| Some((chunks::i32_at(d, 0)?, chunks::f32_at(d, 4)?)))
        .filter(|(_, s)| s.is_finite() && *s > 0.0)
        .unwrap_or((0, 1.0));
    let (name, per) = match kind {
        1 => ("feet", 0.3048),
        2 => ("miles", 1609.344),
        3 => ("millimetres", 0.001),
        4 => ("centimetres", 0.01),
        5 => ("metres", 1.0),
        6 => ("kilometres", 1000.0),
        _ => ("inches", 0.0254),
    };
    let label = if scale == 1.0 { name.to_string() } else { format!("{scale} {name}") };
    (label, per * scale)
}

/// The version of Max that saved the file, from the "3ds Max Version"
/// line of the document summary.
fn version(bytes: &[u8]) -> Option<String> {
    let needle: Vec<u8> = "3ds Max Version: ".encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
    let at = bytes.windows(needle.len()).position(|w| w == needle.as_slice())?;
    let rest = &bytes[at + needle.len()..];
    let v = chunks::utf16(&rest[..rest.len().min(32)]);
    let v: String = v.chars().take_while(|c| c.is_ascii_digit() || *c == '.').collect();
    (!v.is_empty()).then_some(v)
}

fn describe(file: &file::MaxFile, bytes: &[u8], r: &build::Report, unit: String, scene: &mut Scene) {
    let info = &mut scene.info;
    info.format = "3ds Max scene".into();
    if let Some(v) = version(bytes) {
        let major: u32 = v.split('.').next().and_then(|m| m.parse().ok()).unwrap_or(0);
        let name = if major >= 10 { format!("3ds Max {}", 1998 + major) } else { format!("3ds max {major}") };
        info.generator = Some(format!("{name} ({v})"));
    }
    let mut d: Vec<(String, String)> = vec![
        ("Units".into(), format!("{unit}, Z up")),
        ("Objects".into(), file.objects.len().to_string()),
        ("Nodes".into(), r.nodes.to_string()),
        ("Meshes".into(), r.meshes.to_string()),
        ("Materials".into(), r.materials.to_string()),
    ];
    if !r.primitives.is_empty() {
        d.push(("Rebuilt primitives".into(), r.primitives.join(", ")));
    }
    if !r.smoothed.is_empty() {
        d.push(("Subdivided".into(), r.smoothed.join(", ")));
    }
    let counted = |m: &HashMap<String, usize>| {
        let mut list: Vec<String> =
            m.iter().map(|(k, v)| if *v == 1 { k.clone() } else { format!("{k} x{v}") }).collect();
        list.sort();
        list.join(", ")
    };
    if !r.skipped_modifiers.is_empty() {
        d.push(("Modifiers not applied".into(), counted(&r.skipped_modifiers)));
    }
    if !r.not_drawn.is_empty() {
        d.push(("Not drawn".into(), counted(&r.not_drawn)));
    }
    if !r.unknown_materials.is_empty() {
        d.push(("Materials read roughly".into(), r.unknown_materials.join(", ")));
    }
    d.append(&mut info.details);
    info.details = d;
}

#[cfg(test)]
mod tests;
