//! Tests of the Blockbench reader: hand-written projects whose geometry,
//! UVs and animation are worked out by hand, and the real projects in
//! `local/assets/editors-extra/blockbench/` (sources and licences in its
//! SOURCES.md; skipped when it isn't there).

use super::*;
use slerp_scene::Pose;
use std::path::PathBuf;

fn scene_of(json: &str) -> Scene {
    match load(Path::new("/nonexistent/test.bbmodel"), json.as_bytes(), &Limits::viewer()).unwrap() {
        Loaded::Scene(s) => *s,
        Loaded::Picture(_) => panic!("a picture"),
    }
}

/// World-space positions and UVs of every vertex, at rest.
fn world_vertices(scene: &Scene) -> Vec<(Vec3, Vec2)> {
    let pose = Pose::rest(scene);
    let mut out = Vec::new();
    for (ni, node) in scene.nodes.iter().enumerate() {
        let Some(mi) = node.mesh else { continue };
        let m = pose.world[ni];
        for p in &scene.meshes[mi].primitives {
            for (pos, uv) in p.positions.iter().zip(&p.uv0) {
                out.push((m.transform_point3(*pos), *uv));
            }
        }
    }
    out
}

fn close(a: Vec3, b: Vec3) -> bool {
    (a - b).length() < 1e-4
}

const CUBE: &str = r#"{
  "meta": { "format_version": "4.10", "model_format": "free", "box_uv": false },
  "resolution": { "width": 16, "height": 16 },
  "elements": [ {
    "name": "cube", "type": "cube", "uuid": "c1", "from": [0, 0, 0], "to": [16, 16, 16], "origin": [0, 0, 0],
    "faces": {
      "north": { "uv": [0, 0, 16, 16], "texture": 0 }, "south": { "uv": [0, 0, 16, 16], "texture": 0 },
      "east": { "uv": [0, 0, 16, 16], "texture": 0 }, "west": { "uv": [0, 0, 16, 16], "texture": 0 },
      "up": { "uv": [0, 0, 8, 8], "texture": 0, "rotation": 90 }, "down": { "uv": [0, 0, 16, 16], "texture": null }
    }
  } ],
  "outliner": [ "c1" ],
  "textures": []
}"#;

#[test]
fn cubes_are_metres_with_blockbench_uvs() {
    let scene = scene_of(CUBE);
    // Five faces drawn (the bottom has no texture), two triangles each.
    assert_eq!(scene.triangle_count(), 10);
    let v = world_vertices(&scene);
    let max = v.iter().fold(Vec3::splat(-1e9), |m, (p, _)| m.max(*p));
    let min = v.iter().fold(Vec3::splat(1e9), |m, (p, _)| m.min(*p));
    assert!(close(min, Vec3::ZERO) && close(max, Vec3::ONE), "{min} {max}");
    // The north face (z = 0) maps u from +x to -x, as seen from outside,
    // and v from the top down.
    let north: Vec<&(Vec3, Vec2)> = v.iter().filter(|(p, _)| p.z.abs() < 1e-5 && p.y > 0.99 && p.x > 0.99).collect();
    assert!(north.iter().any(|(_, uv)| (*uv - Vec2::new(0.0, 0.0)).length() < 1e-5), "{north:?}");
    // The top face's UV is turned a quarter: its (0, 0) corner moves.
    let up: Vec<Vec2> = v
        .iter()
        .filter(|(p, uv)| {
            (p.y - 1.0).abs() < 1e-5 && uv.x <= 0.5 && uv.y <= 0.5 && (p.x - 0.0).abs() < 1e-5 && p.z.abs() < 1e-5
        })
        .map(|(_, uv)| *uv)
        .collect();
    assert!(!up.is_empty());
}

#[test]
fn groups_pivot_about_their_origin() {
    // A 2x2x2 cube at x = 8..10 inside a group turned 90 degrees about Y
    // around (8, 0, 0): x' = 8 + z and z' = 8 - x.
    let json = r#"{
      "meta": { "format_version": "4.10", "model_format": "free" },
      "elements": [ { "type": "cube", "uuid": "c", "from": [8, 0, 0], "to": [10, 2, 2], "origin": [8, 0, 0],
                      "faces": { "north": { "uv": [0, 0, 2, 2] } } } ],
      "outliner": [ { "name": "g", "uuid": "g", "origin": [8, 0, 0], "rotation": [0, 90, 0], "children": ["c"] } ]
    }"#;
    let scene = scene_of(json);
    let v = world_vertices(&scene);
    let max = v.iter().fold(Vec3::splat(-1e9), |m, (p, _)| m.max(*p));
    let min = v.iter().fold(Vec3::splat(1e9), |m, (p, _)| m.min(*p));
    // Only the north face (z = 0) is drawn: turned, it stands at x = 8
    // pixels, reaching from z = -2 to 0.
    let u = 1.0 / 16.0;
    assert!(close(min, Vec3::new(8.0 * u, 0.0, -2.0 * u)), "{min}");
    assert!(close(max, Vec3::new(8.0 * u, 2.0 * u, 0.0)), "{max}");
}

#[test]
fn box_uv_unfolds_minecraft_style() {
    // A 4x4x4 box at UV offset (8, 0): the north face sits at
    // (8 + d, d) .. (8 + d + w, d + h) = (12, 4) .. (16, 8), pulled in by
    // 1/64 pixel; the texture is 32 wide and 16 high.
    let json = r#"{
      "meta": { "format_version": "4.10", "model_format": "modded_entity", "box_uv": true },
      "resolution": { "width": 32, "height": 16 },
      "elements": [ { "type": "cube", "uuid": "c", "from": [0, 0, 0], "to": [4, 4, 4], "uv_offset": [8, 0],
                      "faces": { "north": { "uv": [0, 0, 0, 0], "texture": 0 } } } ]
    }"#;
    let scene = scene_of(json);
    let uvs: Vec<Vec2> = world_vertices(&scene).into_iter().map(|(_, uv)| uv).collect();
    let (lo, hi) = (12.0 + 1.0 / 64.0, 16.0 - 1.0 / 64.0);
    for uv in &uvs {
        let (u, v) = (uv.x * 32.0, uv.y * 16.0);
        assert!((u - lo).abs() < 1e-3 || (u - hi).abs() < 1e-3, "{u}");
        assert!((v - (4.0 + 1.0 / 64.0)).abs() < 1e-3 || (v - (8.0 - 1.0 / 64.0)).abs() < 1e-3, "{v}");
    }
}

#[test]
fn mesh_quads_are_put_in_order() {
    // The corners of a unit square listed crosswise: two triangles that
    // together cover the square, not a bow tie.
    let json = r#"{
      "meta": { "format_version": "4.10", "model_format": "free" },
      "elements": [ { "type": "mesh", "uuid": "m", "origin": [0, 0, 0],
        "vertices": { "a": [0, 0, 0], "b": [16, 16, 0], "c": [16, 0, 0], "d": [0, 16, 0] },
        "faces": { "f": { "vertices": ["a", "b", "c", "d"], "uv": { "a": [0, 0], "b": [1, 1], "c": [1, 0], "d": [0, 1] } } } } ]
    }"#;
    let scene = scene_of(json);
    let p = &scene.meshes[0].primitives[0];
    let area: f32 = p
        .indices
        .chunks_exact(3)
        .map(|t| {
            let [a, b, c] = [0, 1, 2].map(|i| p.positions[t[i] as usize]);
            (b - a).cross(c - a).length() / 2.0
        })
        .sum();
    assert!((area - 1.0).abs() < 1e-4, "area {area}");
}

const ANIMATED: &str = r#"{
  "meta": { "format_version": "5.0", "model_format": "free" },
  "variable_placeholders": "speed = 90",
  "elements": [ { "type": "cube", "uuid": "c", "from": [0, 0, 0], "to": [1, 1, 1], "faces": { "north": { "uv": [0, 0, 1, 1] } } } ],
  "outliner": [ { "name": "bone", "uuid": "b", "origin": [0, 0, 0], "children": ["c"] } ],
  "animations": [
    { "name": "turn", "length": 1, "loop": "once", "animators": { "b": { "name": "bone", "type": "bone", "keyframes": [
      { "channel": "rotation", "time": 0, "interpolation": "linear", "data_points": [ { "x": 0, "y": 0, "z": 0 } ] },
      { "channel": "rotation", "time": 1, "interpolation": "linear", "data_points": [ { "x": 0, "y": "90", "z": 0 } ] },
      { "channel": "position", "time": 0, "interpolation": "step", "data_points": [ { "x": 0, "y": 0, "z": 0 } ] },
      { "channel": "position", "time": 0.5, "interpolation": "step", "data_points": [ { "x": 0, "y": 16, "z": 0 } ] }
    ] } } },
    { "name": "wave", "length": 0, "loop": "loop", "animators": { "b": { "name": "bone", "type": "bone", "keyframes": [
      { "channel": "rotation", "time": 0, "data_points": [ { "x": "math.sin(query.anim_time * speed) * 10", "y": 0, "z": 0 } ] }
    ] } } }
  ]
}"#;

fn channel(clip: &Animation, property: Property) -> &Channel {
    clip.channels.iter().find(|c| c.property == property).unwrap()
}

/// A channel's value at a time, by its baked linear keys.
fn at(c: &Channel, t: f32, n: usize) -> Vec<f32> {
    let i = c.times.iter().position(|&x| (x - t).abs() < 1e-4).unwrap_or_else(|| panic!("no key at {t}"));
    c.values[i * n..i * n + n].to_vec()
}

#[test]
fn animations_bake_blockbench_interpolation() {
    let scene = scene_of(ANIMATED);
    let turn = scene.animations.iter().find(|a| a.name == "turn").unwrap();
    assert_eq!(turn.duration, 1.0);
    let rot = channel(turn, Property::Rotation);
    let q = at(rot, 0.5, 4);
    let expect = Quat::from_rotation_y(45f32.to_radians());
    assert!(Quat::from_slice(&q).dot(expect).abs() > 0.9999, "{q:?}");
    // Step keys hold until the next one: up 1 m from half a second.
    let pos = channel(turn, Property::Translation);
    assert!(at(pos, 0.4, 3)[1].abs() < 1e-5);
    assert!((at(pos, 0.5, 3)[1] - 1.0).abs() < 1e-5);
    // Molang through a project variable, played for two seconds since the
    // clip has no length of its own: sin(1 s * 90) * 10 = 10 degrees.
    let wave = scene.animations.iter().find(|a| a.name == "wave").unwrap();
    assert_eq!(wave.duration, 2.0);
    let q = at(channel(wave, Property::Rotation), 1.0, 4);
    assert!(Quat::from_slice(&q).dot(Quat::from_rotation_x(10f32.to_radians())).abs() > 0.9999, "{q:?}");
    assert!(scene.info.warnings.is_empty(), "{:?}", scene.info.warnings);
}

#[test]
fn molang_arithmetic() {
    let vars: Vars = vec![("variable.a".into(), "2 * query.anim_time".into())];
    assert_eq!(molang("1 + 2 * 3", 0.0, &vars), Some(7.0));
    assert_eq!(molang("-(4 - 6) / 2", 0.0, &vars), Some(1.0));
    assert_eq!(molang("math.cos(180)", 0.0, &vars), Some(-1.0));
    assert_eq!(molang("v.a + q.anim_time;", 3.0, &vars), Some(9.0));
    assert_eq!(molang("Math.clamp(5, 0, 2)", 0.0, &vars), Some(2.0));
    assert_eq!(molang("variable.unknown", 0.0, &vars), None);
    assert_eq!(molang("1 ? 2 : 3", 0.0, &vars), None);
    assert_eq!(molang("((((((", 0.0, &vars), None);
    // Deep nesting is refused, not a stack overflow.
    assert_eq!(molang(&"-".repeat(100_000), 0.0, &vars), None);
    assert_eq!(molang(&"(".repeat(100_000), 0.0, &vars), None);
    // A variable defined through itself stops.
    let looped: Vars = vec![("variable.x".into(), "v.x + 1".into())];
    assert!(molang("v.x", 0.0, &looped).is_some());
}

#[test]
fn old_files_negate_animation_x_and_y() {
    // Before 5.0, files stored rotation X and Y (and position X) negated.
    let json = ANIMATED.replace("\"format_version\": \"5.0\"", "\"format_version\": \"4.10\"");
    let scene = scene_of(&json);
    let turn = scene.animations.iter().find(|a| a.name == "turn").unwrap();
    let q = at(channel(turn, Property::Rotation), 1.0, 4);
    assert!(Quat::from_slice(&q).dot(Quat::from_rotation_y(-90f32.to_radians())).abs() > 0.9999, "{q:?}");
}

fn samples() -> Option<PathBuf> {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../local/assets/editors-extra/blockbench");
    if p.exists() {
        Some(p)
    } else {
        eprintln!("skipping: {} is missing", p.display());
        None
    }
}

#[test]
fn every_sample_loads() {
    let Some(dir) = samples() else { return };
    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "bbmodel"))
        .collect();
    files.sort();
    assert!(files.len() >= 20);
    for path in files {
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(crate::sniff(&path, &bytes[..bytes.len().min(4096)]), Some(crate::ModelFormat::Blockbench));
        let start = std::time::Instant::now();
        let asset = crate::load(crate::ModelFormat::Blockbench, &path, &bytes, &Limits::viewer())
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        let ms = start.elapsed().as_secs_f64() * 1000.0;
        match asset {
            slerp_scene::Asset::Scene(s) => {
                eprintln!(
                    "{name}: {} triangles, {} textures, {} clips, {:.1} ms, warnings {:?}",
                    s.triangle_count(),
                    s.textures.len(),
                    s.animations.len(),
                    ms,
                    s.info.warnings
                );
                let expect_empty = name.contains("texture-mesh");
                assert_eq!(s.triangle_count() == 0, expect_empty, "{name}");
                // Every clip's channels hold whole keys.
                for a in &s.animations {
                    for c in &a.channels {
                        let n = if c.property == Property::Rotation { 4 } else { 3 };
                        assert_eq!(c.values.len(), c.times.len() * n, "{name} {}", a.name);
                        assert!(c.values.iter().all(|v| v.is_finite()), "{name} {}", a.name);
                    }
                }
                if name.contains("armature") {
                    assert_eq!(s.skins.len(), 1, "{name}");
                    let p = s.meshes.iter().flat_map(|m| &m.primitives).find(|p| p.is_skinned()).unwrap();
                    for w in &p.weights {
                        assert!((w.x + w.y + w.z + w.w - 1.0).abs() < 1e-4);
                    }
                }
                if name.contains("skin-steve") {
                    assert_eq!(s.triangle_count(), 144, "the player model, both layers");
                }
            }
            slerp_scene::Asset::Image(t) => {
                assert!(name.contains("image"), "{name}");
                assert_eq!((t.width(), t.height()), (16, 16));
            }
            _ => panic!("{name}: unexpected asset"),
        }
    }
}

#[test]
fn damaged_files_fail_cleanly() {
    let mut inputs: Vec<Vec<u8>> = vec![CUBE.as_bytes().to_vec(), ANIMATED.as_bytes().to_vec()];
    if let Some(dir) = samples() {
        for name in [
            "physbone-avatar-free-mixed-uv-mesh-v4.10.bbmodel",
            "foxfabric-vest-armature-anim-v5.0.bbmodel",
            "bettermodel-knight-anim-v5.0.bbmodel",
        ] {
            inputs.push(std::fs::read(dir.join(name)).unwrap());
        }
    }
    let limits = Limits::viewer();
    let path = Path::new("/nonexistent/x.bbmodel");
    for bytes in &inputs {
        for cut in (0..bytes.len()).step_by((bytes.len() / 89).max(1)) {
            let _ = load(path, &bytes[..cut], &limits);
        }
        // Numbers swapped for wild values that still parse.
        let text = String::from_utf8_lossy(bytes);
        for (round, wild) in ["-1", "1e30", "-1e30", "4294967296", "null", "\"\"", "[]", "{}"].iter().enumerate() {
            let mut seed = 0x9E37_79B9u32.wrapping_mul(round as u32 + 7);
            let mut out = String::with_capacity(text.len());
            let mut chars = text.chars().peekable();
            while let Some(c) = chars.next() {
                if c.is_ascii_digit() {
                    while chars.peek().is_some_and(|d| d.is_ascii_digit() || *d == '.') {
                        chars.next();
                    }
                    seed ^= seed << 13;
                    seed ^= seed >> 17;
                    seed ^= seed << 5;
                    out.push_str(if seed.is_multiple_of(5) { wild } else { "2" });
                } else {
                    out.push(c);
                }
            }
            let _ = load(path, out.as_bytes(), &limits);
        }
    }
}
