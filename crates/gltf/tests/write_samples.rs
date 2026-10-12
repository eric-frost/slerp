//! Writes the glTF samples on this machine as GLB, reads each back with
//! Slerp's loader and compares what is drawn, then checks every written
//! file with the Khronos glTF Validator when it is installed (see
//! `tools/validate-gltf.ts`).  Files that aren't downloaded are skipped.

use slerp_gltf::{GlbOptions, Selection, write_glb};
use slerp_scene::{Asset, Limits, Pose, Scene};
use std::path::{Path, PathBuf};
use std::process::Command;

/// A path in a checkout of chalk (a game whose files some tests read) in the
/// folder beside this repository's; tests skip the files that aren't there.
macro_rules! chalk {
    ($path:literal) => {
        concat!(env!("CARGO_MANIFEST_DIR"), "/../../../chalk/", $path)
    };
}

fn assets() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../local/assets")
}

/// One file per Khronos model (the GLB, else the JSON), the VRM, three.js
/// and Poly Haven samples, and chalk's characters.
fn samples() -> Vec<PathBuf> {
    let mut out = Vec::new();
    let pick = |dir: &Path, ext: &str| -> Vec<PathBuf> {
        let Ok(rd) = std::fs::read_dir(dir) else { return Vec::new() };
        let mut v: Vec<PathBuf> =
            rd.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e == ext)).collect();
        v.sort();
        v
    };
    if let Ok(rd) = std::fs::read_dir(assets().join("gltf/khronos")) {
        let mut models: Vec<PathBuf> = rd.flatten().map(|e| e.path()).collect();
        models.sort();
        for m in models {
            let glb = pick(&m.join("glTF-Binary"), "glb");
            let gltf = pick(&m.join("glTF"), "gltf");
            out.extend(glb.into_iter().chain(gltf).take(1));
        }
    }
    out.extend(pick(&assets().join("vrm"), "vrm"));
    out.extend(pick(&assets().join("gltf/three"), "glb"));
    if let Ok(rd) = std::fs::read_dir(assets().join("gltf/polyhaven")) {
        for m in rd.flatten() {
            out.extend(pick(&m.path(), "gltf"));
        }
    }
    out.extend(pick(Path::new(chalk!("html/chalk")), "glb"));
    out.push(PathBuf::from(chalk!("html/demos/humans/tate-tee-lod3.glb")));
    out.retain(|p| p.is_file());
    out
}

fn load(path: &Path) -> Scene {
    let bytes = std::fs::read(path).unwrap();
    match slerp_gltf::load(path, &bytes, &Limits::viewer()) {
        Ok(Asset::Scene(s)) => s,
        Ok(_) => panic!("{}: not a scene", path.display()),
        Err(e) => panic!("{}: {e}", path.display()),
    }
}

/// Triangles drawn: each visible node's mesh, counted once per node.
fn drawn_triangles(s: &Scene) -> usize {
    s.nodes
        .iter()
        .filter(|n| n.visible)
        .filter_map(|n| n.mesh)
        .map(|m| s.meshes[m].primitives.iter().map(|p| p.triangle_count()).sum::<usize>())
        .sum()
}

/// The materials drawn, as factors that survive writing exactly.
fn drawn_materials(s: &Scene) -> Vec<String> {
    let mut v: Vec<String> = s
        .nodes
        .iter()
        .filter_map(|n| n.mesh)
        .flat_map(|m| &s.meshes[m].primitives)
        .filter_map(|p| p.material)
        .map(|m| {
            let m = &s.materials[m];
            format!("{:?} {} {} {:?} {}", m.base_color, m.metallic, m.roughness, m.alpha_mode, m.unlit)
        })
        .collect();
    v.sort();
    v.dedup();
    v
}

#[test]
fn samples_round_trip_and_validate() {
    let samples = samples();
    if samples.is_empty() {
        eprintln!("skipped: no samples downloaded");
        return;
    }
    let out = std::env::temp_dir().join(format!("slerp-write-samples-{}", std::process::id()));
    std::fs::create_dir_all(&out).unwrap();
    let mut written = Vec::new();
    for (i, path) in samples.iter().enumerate() {
        let scene = load(path);
        let glb = write_glb(&scene, &Selection::Scene, &GlbOptions::default()).unwrap();
        let file = out.join(format!("{i:03}.glb"));
        std::fs::write(&file, &glb.bytes).unwrap();
        let back = load(&file);
        let name = path.display();

        // What is drawn matches: triangles, materials, the box in the
        // rest pose, clips and their lengths.
        assert_eq!(drawn_triangles(&back), drawn_triangles(&scene), "{name}: triangles");
        assert_eq!(drawn_materials(&back), drawn_materials(&scene), "{name}: materials");
        let (a, b) = (scene.bounds(&Pose::rest(&scene)), back.bounds(&Pose::rest(&back)));
        let tolerance = 1e-3 * a.size().max_element().max(1e-3);
        assert!(
            (a.min - b.min).abs().max_element() < tolerance && (a.max - b.max).abs().max_element() < tolerance,
            "{name}: bounds {a:?} vs {b:?}"
        );
        assert_eq!(back.animations.len(), scene.animations.len(), "{name}: clips");
        for (x, y) in scene.animations.iter().zip(&back.animations) {
            assert!(
                (x.duration - y.duration).abs() < 1e-4,
                "{name}: {} lasts {} not {}",
                x.name,
                y.duration,
                x.duration
            );
        }
        assert_eq!(back.skins.is_empty(), scene.skins.is_empty(), "{name}: skins");
        written.push(file);
    }

    // The validator, when it is installed.
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tools/validate-gltf.ts");
    let validator = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../local/tools/gltf-validator/node_modules/gltf-validator/index.js");
    if !validator.is_file() {
        eprintln!("skipped validation: the validator isn't installed (see tools/validate-gltf.ts)");
    } else {
        let run = Command::new("deno")
            .args(["run", "--allow-read", "--allow-env"])
            .arg(&script)
            .arg("--json")
            .args(&written)
            .output();
        match run {
            Ok(o) => {
                let text = String::from_utf8_lossy(&o.stdout);
                let failures: Vec<&str> = text.lines().filter(|l| !l.contains("\"errors\":0,")).collect();
                assert!(failures.is_empty(), "validator errors:\n{}", failures.join("\n"));
                assert_eq!(text.lines().count(), written.len(), "every file validated");
            }
            Err(e) => eprintln!("skipped validation: deno didn't run: {e}"),
        }
    }
    std::fs::remove_dir_all(&out).ok();
}

/// Loading keeps where each picture's encoded bytes are: a range of the GLB,
/// the neighbouring file, or (only when asked) a copy of a data URI's.
#[test]
fn pictures_keep_their_originals() {
    use slerp_scene::textures::Original;
    let dir = assets().join("gltf/khronos/BoxTextured");
    let png = dir.join("glTF/CesiumLogoFlat.png");
    if !png.is_file() {
        eprintln!("skipped: BoxTextured isn't downloaded");
        return;
    }
    let expected = std::fs::read(&png).unwrap();
    let texture = |file: &str, limits: &Limits| {
        let path = dir.join(file);
        let bytes = std::fs::read(&path).unwrap();
        let Ok(Asset::Scene(s)) = slerp_gltf::load(&path, &bytes, limits) else { panic!("{file}") };
        s.textures[0].clone()
    };
    for file in ["glTF-Binary/BoxTextured.glb", "glTF/BoxTextured.gltf", "glTF-Embedded/BoxTextured.gltf"] {
        let t = texture(file, &Limits::viewer());
        let original = t.source.original.as_ref().unwrap_or_else(|| panic!("{file}: no original"));
        assert_eq!(&*original.read().unwrap(), expected.as_slice(), "{file}");
        assert_eq!(original.format().map(|f| f.mime_type()), Some("image/png"));
        let kind = if file.contains("Embedded") { "memory" } else { "file" };
        assert_eq!(matches!(original, Original::Memory(_)), kind == "memory", "{file}");
    }
    // Thumbnails copy nothing, but file ranges cost nothing and stay.
    assert!(texture("glTF-Embedded/BoxTextured.gltf", &Limits::thumbnail()).source.original.is_none());
    assert!(texture("glTF-Binary/BoxTextured.glb", &Limits::thumbnail()).source.original.is_some());
    assert_eq!(texture("glTF/BoxTextured.gltf", &Limits::viewer()).source.name, "CesiumLogoFlat.png");
}
