//! Tests on synthetic bytes, and on real files from `local/assets/
//! blend-extra` when they are there (each such test returns early without
//! them).  `tools/make-test-scenes.py` makes the `generated` files and
//! `tools/blender-reference.py` Blender's own counts for them.

use crate::file::{Packing, parse_header};
use slerp_scene::{Limits, Pose, Scene};
use std::io::Write;
use std::path::{Path, PathBuf};

fn assets() -> Option<PathBuf> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../local/assets/blend-extra");
    dir.is_dir().then_some(dir)
}

fn load(path: &Path) -> Scene {
    let bytes = std::fs::read(path).unwrap();
    crate::load(path, &bytes, &Limits::viewer()).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

/// Triangles of visible nodes.
fn visible_triangles(s: &Scene) -> usize {
    s.nodes
        .iter()
        .filter(|n| n.visible)
        .filter_map(|n| n.mesh)
        .map(|m| s.meshes[m].primitives.iter().map(|p| p.triangle_count()).sum::<usize>())
        .sum()
}

#[test]
fn sniffs_plain_gzip_and_zstd_files() {
    let plain = b"BLENDER-v405REND".to_vec();
    assert!(crate::sniff(Path::new("x.bin"), &plain));
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    gz.write_all(&plain).unwrap();
    let gz = gz.finish().unwrap();
    assert!(crate::sniff(Path::new("x"), &gz));
    let zst = [0x28, 0xb5, 0x2f, 0xfd, 0, 0];
    assert!(crate::sniff(Path::new("scene.blend"), &zst));
    assert!(crate::sniff(Path::new("scene.blend1"), &zst));
    assert!(!crate::sniff(Path::new("scene.zst"), &zst));
    assert!(!crate::sniff(Path::new("x.blend"), b"PK\x03\x04"));
    // A gzip file that isn't a .blend.
    let mut other = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    other.write_all(b"#VRML V2.0 utf8").unwrap();
    assert!(!crate::sniff(Path::new("x.wrz"), &other.finish().unwrap()));
}

#[test]
fn parses_all_three_header_layouts() {
    let h = parse_header(b"BLENDER_V247").unwrap();
    assert_eq!((h.pointer_size, h.big_endian, h.version, h.format, h.len), (4, true, 247, 0, 12));
    let h = parse_header(b"BLENDER-v405").unwrap();
    assert_eq!((h.pointer_size, h.big_endian, h.version, h.format, h.len), (8, false, 405, 0, 12));
    let h = parse_header(b"BLENDER17-01v0501").unwrap();
    assert_eq!((h.pointer_size, h.big_endian, h.version, h.format, h.len), (8, false, 501, 1, 17));
    assert!(parse_header(b"BLENDER17-02v0600").is_err());
    assert!(parse_header(b"BLENDER?v400").is_err());
    assert!(parse_header(b"NOTBLEND").is_err());
    assert_eq!(Packing::of(b"BLENDER"), Some(Packing::Plain));
}

/// A file Blender saved with every modifier Slerp applies: each object's
/// triangle count matches Blender's own evaluation.
#[test]
fn modifiers_match_blender() {
    let Some(dir) = assets() else { return };
    let path = dir.join("generated/modifiers.blend");
    let Ok(json) = std::fs::read_to_string(dir.join("generated/modifiers.json")) else { return };
    let json: serde_json::Value = serde_json::from_str(&json).unwrap();
    for file in ["modifiers.blend", "modifiers-plain.blend"] {
        let s = load(&path.with_file_name(file));
        assert!(s.info.warnings.is_empty(), "{:?}", s.info.warnings);
        for o in json["objects"].as_array().unwrap() {
            let name = o["name"].as_str().unwrap();
            let want = o["triangles"].as_u64().unwrap() as usize;
            let node = s.nodes.iter().find(|n| n.name == name).unwrap_or_else(|| panic!("no node {name}"));
            let got: usize = s.meshes[node.mesh.unwrap()].primitives.iter().map(|p| p.triangle_count()).sum();
            assert_eq!(got, want, "{name} in {file}");
        }
    }
}

/// Collections, instances, hidden objects and parenting.
#[test]
fn scene_visibility_instances_and_parents() {
    let Some(dir) = assets() else { return };
    let path = dir.join("generated/scene.blend");
    if !path.exists() {
        return;
    }
    let s = load(&path);
    let shown = |name: &str| s.nodes.iter().filter(|n| n.name == name).map(|n| n.visible).collect::<Vec<_>>();
    assert_eq!(shown("HiddenRender"), [false]);
    assert_eq!(shown("HiddenViewport"), [false]);
    assert_eq!(shown("HiddenEye"), [false]);
    assert_eq!(shown("Wire"), [true]);
    // The asset collection is excluded from the view layer but instanced
    // three times.
    assert_eq!(shown("AssetCube"), [true, true, true]);
    // World positions, Y up: Blender's (x, y, z) is (x, z, -y).
    let pose = Pose::rest(&s);
    let at = |name: &str| {
        let i = s.nodes.iter().position(|n| n.name == name).unwrap();
        pose.world[i].w_axis.truncate()
    };
    assert!(at("Child").distance(glam::Vec3::new(3.0, 1.0, -8.0)) < 1e-4, "{}", at("Child"));
    assert!(at("Parent").distance(glam::Vec3::new(0.0, 0.0, -8.0)) < 1e-4);
    // The instances sit where their empties are, less the collection's
    // instance offset of 2 up.
    let cubes: Vec<glam::Vec3> = (0..s.nodes.len())
        .filter(|&i| s.nodes[i].name == "AssetCube")
        .map(|i| pose.world[i].w_axis.truncate())
        .collect();
    for (k, c) in cubes.iter().enumerate() {
        assert!(c.distance(glam::Vec3::new(k as f32 * 2.0, 0.0, -4.0)) < 1e-4, "{c}");
    }
}

/// Files from across Blender's history: 2.4's big-endian 32-bit files,
/// gzip from 2.x, Zstandard from 3.x, and Blender 5's large block headers.
#[test]
fn reads_every_file_generation() {
    let Some(dir) = assets() else { return };
    let cases = [
        ("demos/test249/test249/sculpt.blend", 2048, "Blender 2.47"),
        ("gltf-blender-io/01_cube.blend", 12, "Blender 2.78"),
        ("gltf-blender-io/02_suzanne.blend", 3936, "Blender 2.78"),
        ("demos/skeleton-arm-xray.blend", 15990, "Blender 5.1 (Zstandard)"),
        ("polyhaven/Barrel_01/blend/Barrel_01_1k.blend", 2682, "Blender 2.93 (gzip)"),
        // Collections linked from C.blend beside it, instanced 36 times.
        ("gltf-blender-io/21_scene.blend", 17452, "Blender 3.3"),
        // A 2.66 file whose Subdivision Surface predates its boundary and
        // limit-surface settings.
        ("demos/AtvBuggy/buggy2.1.blend", 25418, "Blender 2.66"),
    ];
    for (file, tris, format) in cases {
        let path = dir.join(file);
        if !path.exists() {
            continue;
        }
        let s = load(&path);
        assert_eq!(visible_triangles(&s), tris, "{file}");
        assert_eq!(s.info.format, format, "{file}");
    }
}

#[test]
fn reads_stored_previews() {
    let Some(dir) = assets() else { return };
    for (file, size) in [
        ("polyhaven/vintage_flashlight/blend/vintage_flashlight_1k.blend", (128, 68)),
        ("polyhaven/Barrel_01/blend/Barrel_01_1k.blend", (128, 128)),
        ("demos/blender-4.0-splash.blend", (128, 128)),
        ("gltf-blender-io/01_cube.blend", (128, 128)),
    ] {
        let path = dir.join(file);
        if !path.exists() {
            continue;
        }
        let p = crate::preview_path(&path).unwrap().unwrap();
        assert_eq!((p.width(), p.height()), size, "{file}");
    }
    // Files saved without one (background saves, and Blender before 2.5).
    let path = dir.join("generated/modifiers-plain.blend");
    if path.exists() {
        assert!(crate::preview_path(&path).unwrap().is_none());
    }
}

/// Principled BSDF with image textures beside the file: colour, packed
/// roughness and metallic, and a normal map.
#[test]
fn materials_find_their_textures() {
    let Some(dir) = assets() else { return };
    let path = dir.join("polyhaven/Barrel_01/blend/Barrel_01_1k.blend");
    if !path.exists() {
        return;
    }
    let s = load(&path);
    let m = &s.materials[0];
    assert!(m.base_color_texture.is_some());
    assert!(m.metallic_roughness_texture.is_some());
    assert!(m.normal_texture.is_some());
    assert!(s.info.neighbours.iter().any(|p| p.to_string_lossy().contains("diff")));
    // Thumbnails skip neighbouring files when told to.
    let bytes = std::fs::read(&path).unwrap();
    let limits = Limits { neighbours: false, ..Limits::thumbnail() };
    let s = crate::load(&path, &bytes, &limits).unwrap();
    assert!(s.materials[0].base_color_texture.is_none());
}

/// Custom normals decode to what Blender's own glTF export carries.
#[test]
fn custom_normals_match_blender() {
    let Some(dir) = assets() else { return };
    let path = dir.join("generated/modifiers.blend");
    if !path.exists() {
        return;
    }
    let s = load(&path);
    // Every corner of "CustomNormals" was set to straight up (Blender's
    // +Z, the scene's +Y).
    // Normals are in the object's own (Z-up) space.  Blender itself decodes
    // them to within a fraction of a degree of what was set.
    let node = s.nodes.iter().find(|n| n.name == "CustomNormals").unwrap();
    let (mut sum, mut count) = (0.0, 0);
    for p in &s.meshes[node.mesh.unwrap()].primitives {
        for n in &p.normals {
            let angle = n.dot(glam::Vec3::Z).clamp(-1.0, 1.0).acos().to_degrees();
            assert!(angle < 1.0, "{n}");
            sum += angle;
            count += 1;
        }
    }
    assert!(sum / (count as f32) < 0.1, "mean {}", sum / count as f32);
}

/// Cut and corrupted files load or fail cleanly: never a panic, a hang or
/// a huge allocation.
#[test]
fn damaged_files_never_panic() {
    let Some(dir) = assets() else { return };
    let path = dir.join("generated/modifiers-plain.blend");
    let Ok(good) = std::fs::read(&path) else { return };
    let limits = Limits::thumbnail();
    let try_load = |bytes: &[u8]| {
        let _ = crate::load(&path, bytes, &limits);
        let _ = crate::preview(bytes);
    };
    // 1. Cut at many lengths, including inside the header.
    for k in 0..60 {
        let len = good.len() * k / 60;
        try_load(&good[..len]);
    }
    // 2. Bytes overwritten at pseudo-random places, a few at a time, and
    //    runs of 0xff (huge counts and sizes).
    let mut seed = 0x2545f4914f6cdd1du64;
    let mut next = || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    for round in 0..150 {
        let mut bad = good.clone();
        for _ in 0..1 + round % 8 {
            let at = (next() as usize) % bad.len();
            let run = if round % 3 == 0 { 4 } else { 1 };
            for b in bad.iter_mut().skip(at).take(run) {
                *b = if round % 3 == 0 { 0xff } else { next() as u8 };
            }
        }
        try_load(&bad);
    }
    // 3. A compressed file cut short.
    let zst = std::fs::read(dir.join("generated/modifiers.blend")).unwrap_or_default();
    for k in 1..10 {
        try_load(&zst[..zst.len() * k / 10]);
    }
}

/// The Armature modifier poses a skinned mesh as it was saved: the bounds
/// match Blender's own evaluation.
#[test]
fn armature_pose_matches_blender() {
    let Some(dir) = assets() else { return };
    let path = dir.join("gltf-blender-io/03_skinned_cylinder.blend");
    let Ok(json) = std::fs::read_to_string(dir.join("ref/gltf-blender-io_03_skinned_cylinder.json")) else { return };
    let json: serde_json::Value = serde_json::from_str(&json).unwrap();
    let s = load(&path);
    let pose = Pose::rest(&s);
    for o in json["objects"].as_array().unwrap() {
        let name = o["name"].as_str().unwrap();
        let Some(i) = s.nodes.iter().position(|n| n.name == name && n.mesh.is_some()) else { continue };
        let (mut lo, mut hi) = (glam::Vec3::splat(f32::MAX), glam::Vec3::splat(f32::MIN));
        for p in &s.meshes[s.nodes[i].mesh.unwrap()].primitives {
            for &v in &p.positions {
                let w = pose.world[i].transform_point3(v);
                lo = lo.min(w);
                hi = hi.max(w);
            }
        }
        let v = |k: &str, i: usize| o[k][i].as_f64().unwrap() as f32;
        // Blender's Z up: its (x, y, z) is the scene's (x, z, -y).
        let (blo, bhi) = (
            glam::Vec3::new(v("min", 0), v("min", 2), -v("max", 1)),
            glam::Vec3::new(v("max", 0), v("max", 2), -v("min", 1)),
        );
        assert!(lo.distance(blo) < 1e-3 && hi.distance(bhi) < 1e-3, "{name}: {lo} {hi} vs {blo} {bhi}");
    }
}

/// Found by fuzzing: n-gons of a few repeated points cost ear clipping time
/// by the square of their corners or worse, so a small file of large ones
/// held a thumbnail up for minutes.  Faces past the per-face cap or the
/// mesh's budget are fanned instead.
#[test]
fn degenerate_ngons_triangulate_quickly() {
    use crate::poly::PolyMesh;
    let mut poly = PolyMesh {
        positions: (0..9).map(|i| glam::Vec3::new((i % 3) as f32, (i / 3) as f32, 0.0)).collect(),
        face_starts: vec![0],
        ..Default::default()
    };
    let mut seed = 7u32;
    for n in [60_000usize, 4_000, 4_000, 4_000, 4_000, 4_000, 4_000, 4_000, 4_000] {
        for _ in 0..n {
            seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12_345);
            poly.corner_verts.push((seed >> 16) % 9);
        }
        poly.face_starts.push(poly.corner_verts.len() as u32);
    }
    let started = std::time::Instant::now();
    let prims = crate::triangulate::primitives(&poly, &[], &|_| Default::default());
    let triangles: usize = prims.iter().map(|(_, p)| p.indices.len() / 3).sum();
    assert_eq!(triangles, 60_000 - 2 + 8 * (4_000 - 2));
    assert!(started.elapsed().as_secs_f32() < 5.0, "{:?}", started.elapsed());
}

/// Collections nest, and list the objects in them as nodes; the scene's
/// master collection isn't one of them, and excluded collections are left
/// out with their objects.
#[test]
fn collections_list_their_objects() {
    let Some(dir) = assets() else { return };
    let path = dir.join("demos/race_spaceship.blend");
    if !path.exists() {
        return;
    }
    let s = load(&path);
    let named = |name: &str| s.collections.iter().position(|c| c.name == name).unwrap();
    let children: std::collections::HashSet<usize> =
        s.collections.iter().flat_map(|c| c.children.iter().copied()).collect();
    let top: Vec<&str> =
        (0..s.collections.len()).filter(|i| !children.contains(i)).map(|i| s.collections[i].name.as_str()).collect();
    assert_eq!(top, ["Set", "Probes", "Ship", "Camera"]);
    let set = &s.collections[named("Set")];
    assert_eq!(set.children, [named("Set Lights"), named("Set Geometry")]);
    assert!(set.nodes.is_empty());
    assert_eq!(s.collections[named("Ship Geometry")].nodes.len(), 32);
    assert_eq!(
        s.collections[named("Camera")].nodes.iter().map(|&n| s.nodes[n].name.as_str()).collect::<Vec<_>>(),
        ["Camera"]
    );
    assert!(s.collections.iter().all(|c| c.nodes.iter().all(|&n| n < s.nodes.len())));
    // The asset collection here is excluded from the view layer.
    let generated = dir.join("generated/scene.blend");
    if generated.exists() {
        assert!(load(&generated).collections.is_empty());
    }
}
