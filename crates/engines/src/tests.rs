//! Tests through the crate's public entry points on sample projects.

use super::*;
use std::path::PathBuf;

/// The folder of downloaded sample projects, when present.
pub(crate) fn extra() -> Option<PathBuf> {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../local/assets/engines-extra");
    dir.is_dir().then_some(dir)
}

#[test]
fn sniffs_engine_files() {
    let p = Path::new;
    assert_eq!(sniff(p("a.tscn"), b"[gd_scene load_steps=2 format=3]\n"), Some(EngineFormat::GodotText));
    assert_eq!(sniff(p("a.tres"), b"[gd_resource type=\"ArrayMesh\" format=3]\n"), Some(EngineFormat::GodotText));
    assert_eq!(sniff(p("a.tres"), b"[gd_resource type=\"StandardMaterial3D\" format=3]\n"), None);
    assert_eq!(
        sniff(p("a.unity"), b"%YAML 1.1\n%TAG !u! tag:unity3d.com,2011:\n--- !u!1 &1\n"),
        Some(EngineFormat::UnityYaml)
    );
    assert_eq!(sniff(p("a.yaml"), b"%YAML 1.1\n%TAG !u! tag:unity3d.com,2011:\n"), None);
    assert_eq!(sniff(p("a.txt"), b"hello"), None);
}

/// Prints the node tree of the file named by `SLERP_DUMP`, for debugging:
/// `SLERP_DUMP=path cargo test -p slerp-engines dump -- --ignored --nocapture`.
#[test]
#[ignore]
fn dump() {
    let Some(path) = std::env::var_os("SLERP_DUMP").map(PathBuf::from) else { return };
    let bytes = std::fs::read(&path).unwrap();
    let limits = if std::env::var_os("SLERP_THUMB").is_some() { Limits::thumbnail() } else { Limits::viewer() };
    let head = &bytes[..bytes.len().min(4096)];
    let asset = match sniff(&path, head) {
        Some(format) => load(format, &path, &bytes, &limits).unwrap(),
        None => slerp_models::load(slerp_models::sniff(&path, head).unwrap(), &path, &bytes, &limits).unwrap(),
    };
    let Asset::Scene(scene) = asset else { panic!() };
    let pose = slerp_scene::Pose::rest(&scene);
    fn walk(s: &slerp_scene::Scene, pose: &slerp_scene::Pose, n: usize, depth: usize) {
        let node = &s.nodes[n];
        let p = pose.world[n].w_axis.truncate();
        let mesh = node.mesh.map(|m| {
            let mesh = &s.meshes[m];
            let mats: Vec<String> = mesh
                .primitives
                .iter()
                .map(|p| p.material.map(|i| s.materials[i].name.clone()).unwrap_or("-".into()))
                .collect();
            let mut b = slerp_scene::Aabb::EMPTY;
            mesh.primitives.iter().flat_map(|p| &p.positions).for_each(|p| b.add(pose.world[n].transform_point3(*p)));
            format!(
                " mesh '{}' {} tris [{}] world {:.1}..{:.1} scale {:.3}",
                mesh.name,
                mesh.primitives.iter().map(|p| p.triangle_count()).sum::<usize>(),
                mats.join(","),
                b.min,
                b.max,
                node.transform.scale
            )
        });
        let t = &node.transform;
        println!(
            "{}{} @ {:.2} {:.2} {:.2} (local t {:.2} r {:.3} s {:.2}){}{}",
            "  ".repeat(depth),
            node.name,
            p.x,
            p.y,
            p.z,
            t.translation,
            t.rotation,
            t.scale,
            if node.visible { "" } else { " (hidden)" },
            mesh.unwrap_or_default()
        );
        if depth < 12 {
            for &c in &node.children {
                walk(s, pose, c, depth + 1);
            }
        }
    }
    for &r in &scene.roots {
        walk(&scene, &pose, r, 0);
    }
    for w in &scene.info.warnings {
        println!("warning: {w}");
    }
}
