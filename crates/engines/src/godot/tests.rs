//! Godot scenes built in temporary projects, damaged input, and the sample
//! projects when they have been downloaded.

use super::*;
use slerp_scene::glam::Quat;
use slerp_scene::{Asset, Limits, SceneKind};

/// A temporary Godot project with the given files, removed on drop.
struct Project(PathBuf);

impl Project {
    fn new(name: &str, files: &[(&str, &[u8])]) -> Project {
        let dir = std::env::temp_dir().join(format!("slerp-godot-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("project.godot"), b"config_version=5\n").unwrap();
        for (path, bytes) in files {
            let p = dir.join(path);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, bytes).unwrap();
        }
        Project(dir)
    }

    fn load(&self, file: &str, limits: &Limits) -> Result<Scene, LoadError> {
        let path = self.0.join(file);
        let bytes = std::fs::read(&path).unwrap();
        let format = crate::sniff(&path, &bytes[..bytes.len().min(4096)]).expect("not recognised");
        match crate::load(format, &path, &bytes, limits)? {
            Asset::Scene(s) => Ok(s),
            _ => panic!("not a scene"),
        }
    }
}

impl Drop for Project {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn node<'a>(s: &'a Scene, name: &str) -> &'a slerp_scene::Node {
    s.nodes.iter().find(|n| n.name == name).unwrap_or_else(|| panic!("no node {name}"))
}

const PROP: &str = r#"[gd_scene format=3]

[sub_resource type="StandardMaterial3D" id="red"]
albedo_color = Color(1, 0, 0, 1)

[sub_resource type="BoxMesh" id="box"]
material = SubResource("red")

[node name="Prop" type="Node3D"]

[node name="Body" type="MeshInstance3D" parent="."]
mesh = SubResource("box")
"#;

const LEVEL: &str = r#"[gd_scene load_steps=4 format=3]

[ext_resource type="PackedScene" path="res://props/prop.tscn" id="1_prop"]

[sub_resource type="StandardMaterial3D" id="blue"]
albedo_color = Color(0, 0, 1, 1)

[sub_resource type="SphereMesh" id="ball"]
radius = 2.0
height = 4.0

[sub_resource type="BoxMesh" id="tile"]
size = Vector3(2, 0.2, 2)

[sub_resource type="MeshLibrary" id="lib"]
item/0/name = "tile"
item/0/mesh = SubResource("tile")
item/0/mesh_transform = Transform3D(1, 0, 0, 0, 1, 0, 0, 0, 1, 0, 0, 0)

[node name="Level" type="Node3D"]

[node name="Ball" type="MeshInstance3D" parent="."]
transform = Transform3D(1, 0, 0, 0, 1, 0, 0, 0, 1, 0, 5, 0)
mesh = SubResource("ball")
surface_material_override/0 = SubResource("blue")

[node name="Prop1" parent="." instance=ExtResource("1_prop")]
transform = Transform3D(1, 0, 0, 0, 1, 0, 0, 0, 1, 10, 0, 0)

[node name="Body" parent="Prop1" index="0"]
visible = false

[node name="Prop2" parent="." instance=ExtResource("1_prop")]
transform = Transform3D(1, 0, 0, 0, 1, 0, 0, 0, 1, -10, 0, 0)

[node name="Union" type="CSGBox3D" parent="."]
size = Vector3(4, 4, 4)

[node name="Hole" type="CSGSphere3D" parent="Union"]
operation = 2

[node name="Grid" type="GridMap" parent="."]
mesh_library = SubResource("lib")
data = {
"cells": PackedInt32Array(0, 0, 0, 1, 0, 0, 2, 0, 1048576)
}

[node name="Sun" type="DirectionalLight3D" parent="."]
light_color = Color(1, 0.9, 0.8, 1)

[node name="UI" type="Control" parent="."]

[node name="Label3DInUI" type="MeshInstance3D" parent="UI"]
mesh = SubResource("ball")
"#;

#[test]
fn builds_a_level_with_instances_csg_and_grid_map() {
    let p = Project::new("level", &[("props/prop.tscn", PROP.as_bytes()), ("level.tscn", LEVEL.as_bytes())]);
    let s = p.load("level.tscn", &Limits::viewer()).unwrap();
    // 1. The instanced prop appears twice, sharing one mesh; the first's
    //    body is hidden by an override.
    let bodies: Vec<&slerp_scene::Node> = s.nodes.iter().filter(|n| n.name == "Body").collect();
    assert_eq!(bodies.len(), 2);
    assert_eq!(bodies[0].mesh, bodies[1].mesh);
    assert_eq!(bodies.iter().filter(|n| n.visible).count(), 1);
    assert_eq!(node(&s, "Prop1").transform.translation, Vec3::new(10.0, 0.0, 0.0));
    // 2. The ball wears the override's blue, the prop the box's red.
    let ball = &s.meshes[node(&s, "Ball").mesh.unwrap()];
    let blue = &s.materials[ball.primitives[0].material.unwrap()];
    assert!(blue.base_color.z > 0.9 && blue.base_color.x < 0.1, "{:?}", blue.base_color);
    let body = &s.meshes[bodies[0].mesh.unwrap()];
    assert!(s.materials[body.primitives[0].material.unwrap()].base_color.x > 0.9);
    // 3. CSG: the box is drawn, the subtracted sphere left out with a
    //    warning.
    assert!(node(&s, "Union").mesh.is_some());
    assert!(!node(&s, "Hole").visible);
    assert!(s.info.warnings.iter().any(|w| w.contains("subtraction")));
    // 4. Three grid cells, the third rotated (orientation 16 is a quarter
    //    turn about Y), at cell size 2 and centred.
    let cells: Vec<&slerp_scene::Node> = s.nodes.iter().filter(|n| n.name == "cell 0").collect();
    assert_eq!(cells.len(), 3);
    assert_eq!(cells[0].transform.translation, Vec3::new(1.0, 1.0, 1.0));
    assert_eq!(cells[1].transform.translation, Vec3::new(3.0, 1.0, 1.0));
    assert!((cells[2].transform.rotation.angle_between(Quat::IDENTITY) - std::f32::consts::FRAC_PI_2).abs() < 1e-4);
    // 5. The light is a marker, the UI subtree is left out.
    assert!(s.markers.iter().any(|m| m.label.contains("Sun")));
    assert!(!s.nodes.iter().any(|n| n.name == "Label3DInUI"));
    // Under 20 m across (the hidden body doesn't count): an object.
    assert_eq!(s.kind, SceneKind::Object);
    assert!(s.info.neighbours.iter().any(|n| n.ends_with("prop.tscn")));
}

#[test]
fn instancing_itself_is_refused() {
    let looping = r#"[gd_scene format=3]
[ext_resource type="PackedScene" path="res://a.tscn" id="1"]
[node name="A" type="Node3D"]
[node name="Again" parent="." instance=ExtResource("1")]
[node name="Box" type="CSGBox3D" parent="."]
"#;
    let p = Project::new("loop", &[("a.tscn", looping.as_bytes())]);
    let s = p.load("a.tscn", &Limits::viewer()).unwrap();
    assert!(s.info.warnings.iter().any(|w| w.contains("inside itself")));
    assert!(node(&s, "Box").mesh.is_some());
}

#[test]
fn shows_collision_shapes_when_nothing_else_draws() {
    let text = r#"[gd_scene format=3]
[sub_resource type="BoxShape3D" id="b"]
size = Vector3(2, 1, 2)
[node name="Root" type="Node3D"]
[node name="Body" type="StaticBody3D" parent="."]
[node name="Shape" type="CollisionShape3D" parent="Body"]
shape = SubResource("b")
"#;
    let p = Project::new("shapes", &[("s.tscn", text.as_bytes())]);
    let s = p.load("s.tscn", &Limits::viewer()).unwrap();
    assert!(node(&s, "Shape").mesh.is_some());
}

#[test]
fn reads_compressed_binary_resources() {
    // The hand-made binary resource of `binary::tests`, compressed the way
    // Godot does: zstd blocks behind an `RSCC` header.
    // Offsets count from the start of the decompressed data, which has no
    // magic, so the plain file's (which count it) move back by 4.
    let mut plain = binary::tests::sample();
    let at = binary::tests::OFFSET_AT;
    let offset = u64::from_le_bytes(plain[at..at + 8].try_into().unwrap()) - 4;
    plain[at..at + 8].copy_from_slice(&offset.to_le_bytes());
    let body = &plain[4..];
    let block = 4096u32;
    let mut out = b"RSCC".to_vec();
    for v in [2u32, block, body.len() as u32] {
        out.extend_from_slice(&v.to_le_bytes());
    }
    let chunks: Vec<Vec<u8>> = body.chunks(block as usize).map(|c| zstd::bulk::compress(c, 3).unwrap()).collect();
    let blocks = body.len() / block as usize + 1;
    for i in 0..blocks {
        out.extend_from_slice(&(chunks.get(i).map_or(0, |c| c.len()) as u32).to_le_bytes());
    }
    for c in &chunks {
        out.extend_from_slice(c);
    }
    let f = binary::parse(&out, 1 << 20).unwrap();
    assert_eq!(f.kind, "ArrayMesh");
    assert_eq!(f.main_prop("data").and_then(Value::bytes), Some(&[7u8, 8, 9][..]));
}

#[test]
fn damaged_scenes_never_panic() {
    let p = Project::new("damaged", &[("props/prop.tscn", PROP.as_bytes())]);
    let path = p.0.join("level.tscn");
    let bytes = LEVEL.as_bytes();
    for cut in (0..bytes.len()).step_by(3) {
        let _ = crate::load(crate::EngineFormat::GodotText, &path, &bytes[..cut], &Limits::thumbnail());
    }
    let mut seed = 7u32;
    for _ in 0..300 {
        let mut bad = bytes.to_vec();
        for _ in 0..4 {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let i = (seed as usize >> 8) % bad.len();
            bad[i] = b"[]{}()=,\"0123456789-e./"[(seed >> 4) as usize % 23];
        }
        let _ = crate::load(crate::EngineFormat::GodotText, &path, &bad, &Limits::thumbnail());
    }
    // A grid map claiming a huge cell count in few bytes is bounded by its
    // data, and a sphere asking for absurd segment counts by the cap.
    let huge = LEVEL.replace("radius = 2.0", "radius = 2.0\nradial_segments = 2000000000\nrings = 2000000000");
    let s = crate::load(crate::EngineFormat::GodotText, &path, huge.as_bytes(), &Limits::viewer()).unwrap();
    let Asset::Scene(s) = s else { panic!() };
    assert!(s.vertex_count() < 1_000_000);
}

/// The demo projects, when downloaded: every scene loads, and the numbers
/// of a few are pinned.
#[test]
fn sample_projects() {
    let Some(extra) = crate::tests::extra() else { return };
    let demos = extra.join("godot-demo-projects/3d");
    if !demos.is_dir() {
        return;
    }
    let load = |rel: &str, limits: &Limits| -> Scene {
        let path = demos.join(rel);
        let bytes = std::fs::read(&path).unwrap();
        let format = crate::sniff(&path, &bytes[..bytes.len().min(4096)]).unwrap();
        let t = std::time::Instant::now();
        let Asset::Scene(s) = crate::load(format, &path, &bytes, limits).unwrap() else { panic!() };
        eprintln!("{rel}: {:.1} ms, {} nodes", t.elapsed().as_secs_f64() * 1000.0, s.nodes.len());
        s
    };
    // The platformer's level is a binary GridMap of binary mesh resources.
    let grid = load("platformer/stage/grid_map.scn", &Limits::thumbnail());
    assert_eq!(grid.kind, SceneKind::Level);
    assert!(grid.nodes.len() > 2000);
    // Truck town places glTF models with material overrides; its 2 km
    // ground is cut down for thumbnails.
    let town = load("truck_town/town/town_scene.tscn", &Limits::thumbnail());
    assert_eq!(town.kind, SceneKind::Level);
    let b = town.bounds(&slerp_scene::Pose::rest(&town));
    assert!(b.size().x < 1000.0, "{:?}", b);
    let csg = load("csg/csg.tscn", &Limits::viewer());
    assert!(csg.triangle_count() > 1000);
}
