//! Unity scenes in a temporary project, damaged input, and the sample
//! projects when they have been downloaded.

use slerp_scene::glam::Vec3;
use slerp_scene::{Asset, Limits, Pose, Scene, SceneKind};
use std::path::{Path, PathBuf};

const HEADER: &str = "%YAML 1.1\n%TAG !u! tag:unity3d.com,2011:\n";

/// A red Standard material.
const RED: &str = "--- !u!21 &2100000\nMaterial:\n  m_Name: Red\n  m_Shader: {fileID: 46, guid: 0000000000000000f000000000000000, type: 0}\n  m_SavedProperties:\n    serializedVersion: 3\n    m_TexEnvs: []\n    m_Floats:\n    - _Glossiness: 0.25\n    m_Colors:\n    - _Color: {r: 1, g: 0, b: 0, a: 1}\n";

/// A crate (a built-in cube wearing Red) with a lid (a built-in quad).
const CRATE: &str = "--- !u!1 &100\nGameObject:\n  m_Component:\n  - component: {fileID: 400}\n  - component: {fileID: 3300}\n  - component: {fileID: 2300}\n  m_Name: Crate\n  m_IsActive: 1\n--- !u!4 &400\nTransform:\n  m_GameObject: {fileID: 100}\n  m_LocalRotation: {x: 0, y: 0, z: 0, w: 1}\n  m_LocalPosition: {x: 0, y: 0.5, z: 0}\n  m_LocalScale: {x: 1, y: 1, z: 1}\n  m_Children:\n  - {fileID: 401}\n  m_Father: {fileID: 0}\n--- !u!33 &3300\nMeshFilter:\n  m_GameObject: {fileID: 100}\n  m_Mesh: {fileID: 10202, guid: 0000000000000000e000000000000000, type: 0}\n--- !u!23 &2300\nMeshRenderer:\n  m_GameObject: {fileID: 100}\n  m_Enabled: 1\n  m_Materials:\n  - {fileID: 2100000, guid: aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa, type: 2}\n--- !u!1 &101\nGameObject:\n  m_Name: Lid\n  m_IsActive: 1\n--- !u!4 &401\nTransform:\n  m_GameObject: {fileID: 101}\n  m_LocalRotation: {x: 0, y: 0, z: 0, w: 1}\n  m_LocalPosition: {x: 0, y: 0.5, z: 0}\n  m_LocalScale: {x: 1, y: 1, z: 1}\n  m_Children: []\n  m_Father: {fileID: 400}\n--- !u!33 &3301\nMeshFilter:\n  m_GameObject: {fileID: 101}\n  m_Mesh: {fileID: 10210, guid: 0000000000000000e000000000000000, type: 0}\n--- !u!23 &2301\nMeshRenderer:\n  m_GameObject: {fileID: 101}\n  m_Enabled: 1\n  m_Materials: []\n";

/// Two crates, one moved, renamed and with its lid switched off; a sticker
/// added to it through a stripped Transform; a main camera.
const LEVEL: &str = "--- !u!1001 &5000\nPrefabInstance:\n  m_Modification:\n    m_TransformParent: {fileID: 0}\n    m_Modifications:\n    - target: {fileID: 400, guid: bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb, type: 3}\n      propertyPath: m_LocalPosition.x\n      value: 5\n      objectReference: {fileID: 0}\n    - target: {fileID: 100, guid: bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb, type: 3}\n      propertyPath: m_Name\n      value: Crate A\n      objectReference: {fileID: 0}\n    - target: {fileID: 101, guid: bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb, type: 3}\n      propertyPath: m_IsActive\n      value: 0\n      objectReference: {fileID: 0}\n    m_RemovedComponents: []\n  m_SourcePrefab: {fileID: 100100000, guid: bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb, type: 3}\n--- !u!4 &6000 stripped\nTransform:\n  m_CorrespondingSourceObject: {fileID: 400, guid: bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb, type: 3}\n  m_PrefabInstance: {fileID: 5000}\n--- !u!1001 &5100\nPrefabInstance:\n  m_Modification:\n    m_TransformParent: {fileID: 0}\n    m_Modifications:\n    - target: {fileID: 400, guid: bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb, type: 3}\n      propertyPath: m_LocalPosition.z\n      value: 3\n      objectReference: {fileID: 0}\n  m_SourcePrefab: {fileID: 100100000, guid: bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb, type: 3}\n--- !u!1 &7000\nGameObject:\n  m_Name: Sticker\n  m_IsActive: 1\n--- !u!4 &7001\nTransform:\n  m_GameObject: {fileID: 7000}\n  m_LocalRotation: {x: 0, y: 0, z: 0, w: 1}\n  m_LocalPosition: {x: 0, y: 1, z: 0}\n  m_LocalScale: {x: 1, y: 1, z: 1}\n  m_Children: []\n  m_Father: {fileID: 6000}\n--- !u!33 &7002\nMeshFilter:\n  m_GameObject: {fileID: 7000}\n  m_Mesh: {fileID: 10207, guid: 0000000000000000e000000000000000, type: 0}\n--- !u!23 &7003\nMeshRenderer:\n  m_GameObject: {fileID: 7000}\n  m_Enabled: 1\n  m_Materials:\n  - {fileID: 10303, guid: 0000000000000000f000000000000000, type: 0}\n--- !u!1 &8000\nGameObject:\n  m_Name: Main Camera\n  m_TagString: MainCamera\n  m_IsActive: 1\n--- !u!4 &8001\nTransform:\n  m_GameObject: {fileID: 8000}\n  m_LocalRotation: {x: 0, y: 0, z: 0, w: 1}\n  m_LocalPosition: {x: 0, y: 2, z: -10}\n  m_LocalScale: {x: 1, y: 1, z: 1}\n  m_Children: []\n  m_Father: {fileID: 0}\n--- !u!20 &8002\nCamera:\n  m_GameObject: {fileID: 8000}\n  m_Enabled: 1\n";

struct Project(PathBuf);

impl Project {
    /// A project of its own for each test, since tests run in parallel.
    fn new(name: &str) -> Project {
        let dir = std::env::temp_dir().join(format!("slerp-unity-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for d in ["Assets/Materials", "Assets/Prefabs", "Assets/Scenes", "ProjectSettings"] {
            std::fs::create_dir_all(dir.join(d)).unwrap();
        }
        let write = |p: &str, text: &str| std::fs::write(dir.join(p), text).unwrap();
        write("Assets/Materials/Red.mat", &format!("{HEADER}{RED}"));
        write("Assets/Materials/Red.mat.meta", "fileFormatVersion: 2\nguid: aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\n");
        write("Assets/Prefabs/Crate.prefab", &format!("{HEADER}{CRATE}"));
        write("Assets/Prefabs/Crate.prefab.meta", "fileFormatVersion: 2\nguid: bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb\n");
        write("Assets/Scenes/Level.unity", &format!("{HEADER}{LEVEL}"));
        Project(dir)
    }

    fn scene(&self) -> PathBuf {
        self.0.join("Assets/Scenes/Level.unity")
    }
}

impl Drop for Project {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn load(path: &Path, limits: &Limits) -> Scene {
    let bytes = std::fs::read(path).unwrap();
    let format = crate::sniff(path, &bytes[..bytes.len().min(4096)]).expect("not recognised");
    match crate::load(format, path, &bytes, limits).unwrap() {
        Asset::Scene(s) => s,
        _ => panic!("not a scene"),
    }
}

#[test]
fn expands_prefab_instances_with_their_changes() {
    let p = Project::new("instances");
    let s = load(&p.scene(), &Limits::viewer());
    let pose = Pose::rest(&s);
    let find = |name: &str| s.nodes.iter().position(|n| n.name == name).unwrap_or_else(|| panic!("no {name}"));
    // 1. The changed instance: moved, renamed, its lid hidden; Unity's
    //    +Z becomes -Z.
    let a = find("Crate A");
    assert_eq!(s.nodes[a].transform.translation, Vec3::new(5.0, 0.5, 0.0));
    let lids: Vec<&slerp_scene::Node> = s.nodes.iter().filter(|n| n.name == "Lid").collect();
    assert_eq!(lids.len(), 2);
    assert_eq!(lids.iter().filter(|n| n.visible).count(), 1);
    let b = find("Crate");
    assert_eq!(s.nodes[b].transform.translation, Vec3::new(0.0, 0.5, -3.0));
    // 2. The crate wears Red; the sticker hangs from the instance's root
    //    through the stripped Transform.
    let red = &s.materials[s.meshes[s.nodes[a].mesh.unwrap()].primitives[0].material.unwrap()];
    assert_eq!(red.name, "Red");
    assert!(red.base_color.x > 0.99 && red.base_color.y < 0.01);
    assert!((red.roughness - 0.75).abs() < 1e-5);
    let sticker = find("Sticker");
    assert!(s.nodes[a].children.contains(&sticker));
    assert!((pose.world[sticker].w_axis.truncate() - Vec3::new(5.0, 1.5, 0.0)).length() < 1e-5);
    // 3. Both crates share one cube mesh; the camera is a marker.
    assert_eq!(s.nodes[a].mesh, s.nodes[b].mesh);
    assert!(s.markers.iter().any(|m| m.label.contains("Main Camera")));
    assert_eq!(s.kind, SceneKind::Object);
}

#[test]
fn damaged_scenes_never_panic() {
    let p = Project::new("damaged");

    // A prefab that contains itself, written before anything is loaded so
    // the project's GUID index has it.
    let looping = format!(
        "{HEADER}--- !u!1001 &1\nPrefabInstance:\n  m_Modification:\n    m_TransformParent: {{fileID: 0}}\n    m_Modifications: []\n  m_SourcePrefab: {{fileID: 100100000, guid: cccccccccccccccccccccccccccccccc, type: 3}}\n{CRATE}"
    );
    std::fs::write(p.0.join("Assets/Prefabs/Loop.prefab"), &looping).unwrap();
    std::fs::write(p.0.join("Assets/Prefabs/Loop.prefab.meta"), "guid: cccccccccccccccccccccccccccccccc\n").unwrap();
    let path = p.scene();

    let text = format!("{HEADER}{LEVEL}");
    let bytes = text.as_bytes();
    for cut in (0..bytes.len()).step_by(5) {
        let _ = crate::load(crate::EngineFormat::UnityYaml, &path, &bytes[..cut], &Limits::thumbnail());
    }
    // These mutations once included `{x: ]}`, which made the reader loop
    // for ever.
    let mut seed = 11u32;
    for _ in 0..300 {
        let mut bad = bytes.to_vec();
        for _ in 0..4 {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let i = (seed as usize >> 8) % bad.len();
            bad[i] = b"{}[]:,- \n0123456789&!"[(seed >> 4) as usize % 21];
        }
        let _ = crate::load(crate::EngineFormat::UnityYaml, &path, &bad, &Limits::thumbnail());
    }
    // It is refused, not followed.
    let s = load(&p.0.join("Assets/Prefabs/Loop.prefab"), &Limits::viewer());
    assert!(s.info.warnings.iter().any(|w| w.contains("contains itself")), "{:?}", s.info.warnings);
}

/// The sample projects, when downloaded: sizes that catch unit and
/// handedness mistakes in model meshes.
#[test]
fn sample_projects() {
    let Some(extra) = crate::tests::extra() else { return };
    let examples = extra.join("ml-agents/Project/Assets/ML-Agents/Examples");
    if !examples.is_dir() {
        return;
    }
    let size = |rel: &str| -> Vec3 {
        let t = std::time::Instant::now();
        let s = load(&examples.join(rel), &Limits::viewer());
        eprintln!("{rel}: {:.1} ms, {} nodes", t.elapsed().as_secs_f64() * 1000.0, s.nodes.len());
        s.bounds(&Pose::rest(&s)).size()
    };
    // FoodCollector's arena: FBX meshes whose nodes, not vertices, carry
    // the centimetre scale; 102 m walls.
    let food = size("FoodCollector/Prefabs/FoodCollectorArea.prefab");
    assert!((food.x - 102.0).abs() < 1.0, "{food}");
    // Sorter's ring: a Blender FBX in centimetres, scaled 22x in the
    // prefab; about 45 m across.
    let sorter = size("Sorter/Prefabs/Area.prefab");
    assert!(sorter.x > 40.0 && sorter.x < 50.0, "{sorter}");
    let scene = load(&examples.join("3DBall/Scenes/3DBall.unity"), &Limits::thumbnail());
    assert_eq!(scene.kind, SceneKind::Level);
    assert_eq!(scene.nodes.iter().filter(|n| n.name == "Ball").count(), 12);
}
