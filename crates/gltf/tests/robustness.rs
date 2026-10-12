//! Malformed input must never panic, hang or exhaust memory: truncated
//! files, flipped bytes, and JSON whose numbers are replaced with hostile
//! values one at a time.  Each load may fail; it must only fail cleanly.

use serde_json::Value;
use slerp_scene::{Limits, LoadError};
use std::path::{Path, PathBuf};

/// A path in a checkout of chalk (a game whose files some tests read) in the
/// folder beside this repository's; tests skip the files that aren't there.
macro_rules! chalk {
    ($path:literal) => {
        concat!(env!("CARGO_MANIFEST_DIR"), "/../../../chalk/", $path)
    };
}

fn asset(rel: &str) -> Option<PathBuf> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../local/assets");
    let name = Path::new(rel).file_name().unwrap_or_default();
    let found = [
        root.join("gltf/khronos").join(rel),
        root.join("gltf-extra").join(rel),
        root.join(rel),
        root.join("vrm/anim").join(name),
        PathBuf::from(rel),
    ]
    .into_iter()
    .find(|p| p.is_file());
    if found.is_none() {
        eprintln!("skipped: {rel} isn't downloaded");
    }
    found
}

/// A small deterministic generator, so failures can be reproduced.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}

/// Loads with clips (as the viewer does) and without (as thumbnails do).
/// Textures are kept small: decoding them fully is the textures crate's
/// business, and would only slow these tests down.
fn try_load(path: &Path, bytes: &[u8]) {
    let viewer = Limits { max_texture_size: Some(64), ..Limits::viewer() };
    for limits in [viewer, Limits::thumbnail()] {
        let _ = slerp_gltf::load(path, bytes, &limits);
    }
}

/// Splits a GLB into its JSON and the rest, or returns `None` for JSON
/// files.
fn split_glb(bytes: &[u8]) -> Option<(Value, Vec<u8>)> {
    if !bytes.starts_with(b"glTF") {
        return None;
    }
    let len = u32::from_le_bytes(bytes[12..16].try_into().unwrap()) as usize;
    let json = serde_json::from_slice(&bytes[20..20 + len]).unwrap();
    Some((json, bytes[20 + len..].to_vec()))
}

fn join_glb(json: &Value, rest: &[u8]) -> Vec<u8> {
    let mut text = serde_json::to_vec(json).unwrap();
    while !text.len().is_multiple_of(4) {
        text.push(b' ');
    }
    let total = 12 + 8 + text.len() + rest.len();
    let mut out = Vec::with_capacity(total);
    out.extend(b"glTF");
    out.extend(2u32.to_le_bytes());
    out.extend((total as u32).to_le_bytes());
    out.extend((text.len() as u32).to_le_bytes());
    out.extend(0x4E4F534Au32.to_le_bytes());
    out.extend(text);
    out.extend(rest);
    out
}

/// Paths to every number in a JSON tree, skipping data URIs and extras.
fn number_paths(v: &Value, path: &mut Vec<Value>, out: &mut Vec<Vec<Value>>) {
    match v {
        Value::Number(_) => out.push(path.clone()),
        Value::Array(a) => {
            for (i, x) in a.iter().enumerate() {
                path.push(Value::from(i));
                number_paths(x, path, out);
                path.pop();
            }
        }
        Value::Object(o) => {
            for (k, x) in o {
                if k == "extras" {
                    continue;
                }
                path.push(Value::from(k.clone()));
                number_paths(x, path, out);
                path.pop();
            }
        }
        _ => {}
    }
}

fn at<'a>(v: &'a mut Value, path: &[Value]) -> &'a mut Value {
    path.iter().fold(v, |v, key| match key {
        Value::Number(i) => &mut v[i.as_u64().unwrap() as usize],
        Value::String(k) => &mut v[k.as_str()],
        _ => unreachable!(),
    })
}

const HOSTILE: [f64; 9] = [-1.0, 0.0, 1.0, 3.0, 255.0, 65535.0, 4294967295.0, 1e18, 0.5];

/// Replaces numbers in the JSON one at a time with hostile values.  With
/// `sample`, only that many randomly chosen numbers are tried.
fn mutate_numbers(rel: &str, sample: Option<usize>) {
    let Some(path) = asset(rel) else { return };
    let bytes = std::fs::read(&path).unwrap();
    let (json, rest) = match split_glb(&bytes) {
        Some((j, r)) => (j, Some(r)),
        None => (serde_json::from_slice(&bytes).unwrap(), None),
    };
    let mut paths = Vec::new();
    number_paths(&json, &mut Vec::new(), &mut paths);
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let chosen: Vec<&Vec<Value>> = match sample {
        Some(n) => (0..n).map(|_| &paths[rng.below(paths.len())]).collect(),
        None => paths.iter().collect(),
    };
    for p in chosen {
        for value in HOSTILE {
            let mut mutated = json.clone();
            *at(&mut mutated, p) = serde_json::Number::from_f64(value).map(Value::Number).unwrap_or(Value::Null);
            let bytes = match &rest {
                Some(r) => join_glb(&mutated, r),
                None => serde_json::to_vec(&mutated).unwrap(),
            };
            try_load(&path, &bytes);
        }
    }
}

#[test]
fn hostile_numbers_in_small_files() {
    for rel in [
        "Box/glTF-Embedded/Box.gltf",
        "SimpleSkin/glTF-Embedded/SimpleSkin.gltf",
        "SimpleSparseAccessor/glTF-Embedded/SimpleSparseAccessor.gltf",
        "SimpleMorph/glTF-Embedded/SimpleMorph.gltf",
        "MeshPrimitiveModes/glTF-Embedded/MeshPrimitiveModes.gltf",
        "AnimatedMorphCube/glTF-Binary/AnimatedMorphCube.glb",
        "SimpleInstancing/glTF-Binary/SimpleInstancing.glb",
    ] {
        mutate_numbers(rel, None);
    }
}

#[test]
fn hostile_numbers_in_compressed_files() {
    mutate_numbers("MeshoptCubeTest/glTF-Meshopt/MeshoptCubeTest.gltf", Some(300));
    mutate_numbers(chalk!("html/demos/humans/tate-tee-crowd.glb"), Some(150));
    mutate_numbers(chalk!("html/demos/humans/tate-tee-lod3.glb"), Some(40));
    mutate_numbers("vrm/test.vrma", Some(200));
}

#[test]
fn truncated_files() {
    for (rel, steps) in [
        ("Box/glTF-Binary/Box.glb", usize::MAX),
        ("AnimatedMorphCube/glTF-Binary/AnimatedMorphCube.glb", 400),
        (chalk!("html/demos/humans/tate-tee-crowd.glb"), 300),
        (chalk!("html/demos/humans/tate-tee-lod3.glb"), 150),
        ("vrm/EmissionMigration_v0.106.0.vrm", 150),
        ("Box/glTF-Embedded/Box.gltf", usize::MAX),
        ("draco-extra/google-draco/Box/glTF_Binary/Box_Draco.glb", usize::MAX),
        ("gltf/three/kira.glb", 300),
    ] {
        let Some(path) = asset(rel) else { continue };
        let bytes = std::fs::read(&path).unwrap();
        let step = (bytes.len() / steps.min(bytes.len())).max(1);
        for len in (0..bytes.len()).step_by(step) {
            try_load(&path, &bytes[..len]);
        }
    }
}

#[test]
fn flipped_bytes() {
    let mut rng = Rng(0x2545_F491_4F6C_DD1D);
    for (rel, rounds) in [
        ("Box/glTF-Binary/Box.glb", 2000),
        (chalk!("html/demos/humans/tate-tee-crowd.glb"), 400),
        (chalk!("html/demos/humans/tate-tee-lod3.glb"), 150),
        ("MeshoptCubeTest/glTF-Meshopt/MeshoptCubeTest.gltf", 300),
        ("draco-extra/google-draco/Box/glTF_Binary/Box_Draco.glb", 3000),
        ("gltf/three/kira.glb", 300),
    ] {
        let Some(path) = asset(rel) else { continue };
        let bytes = std::fs::read(&path).unwrap();
        // Flip bytes past the JSON as well as in it, so compressed data gets
        // corrupted too.
        for _ in 0..rounds {
            let mut copy = bytes.clone();
            for _ in 0..1 + rng.below(8) {
                let i = rng.below(copy.len());
                copy[i] ^= 1 << rng.below(8);
            }
            try_load(&path, &copy);
        }
    }
}

#[test]
fn corrupt_meshopt_streams() {
    // Overwrite chalk's compressed vertex and index data with noise, keeping
    // the JSON intact, so the C++ decoder sees garbage of the right size.
    let Some(path) = asset(chalk!("html/demos/humans/tate-tee-crowd.glb")) else { return };
    let bytes = std::fs::read(&path).unwrap();
    let json_len = u32::from_le_bytes(bytes[12..16].try_into().unwrap()) as usize;
    let bin_start = 20 + json_len + 8;
    let mut rng = Rng(42);
    for _ in 0..200 {
        let mut copy = bytes.clone();
        let start = bin_start + rng.below(copy.len() - bin_start);
        let end = (start + rng.below(4096)).min(copy.len());
        for b in &mut copy[start..end] {
            *b = rng.next() as u8;
        }
        try_load(&path, &copy);
    }
}

#[test]
fn hostile_buffers_are_refused() {
    // A tiny file claiming an enormous zero-filled accessor.
    let json = r#"{"asset":{"version":"2.0"},"scenes":[{"nodes":[0]}],"nodes":[{"mesh":0}],
        "meshes":[{"primitives":[{"attributes":{"POSITION":0}}]}],
        "accessors":[{"componentType":5126,"count":4000000000,"type":"VEC3"}]}"#;
    let r = slerp_gltf::load(Path::new("x.gltf"), json.as_bytes(), &Limits::viewer());
    assert!(matches!(r, Err(LoadError::TooLarge(_))), "{:?}", r.err());

    // Too many triangles for a thumbnail, refused before decoding.
    let json = r#"{"asset":{"version":"2.0"},"scenes":[{"nodes":[0]}],"nodes":[{"mesh":0}],
        "meshes":[{"primitives":[{"attributes":{"POSITION":0},"indices":1}]}],
        "accessors":[{"componentType":5126,"count":3,"type":"VEC3"},
                     {"componentType":5125,"count":3000000000,"type":"SCALAR","bufferView":0}],
        "bufferViews":[{"buffer":0,"byteLength":12}],"buffers":[{"byteLength":12}]}"#;
    let r = slerp_gltf::load(Path::new("x.gltf"), json.as_bytes(), &Limits::thumbnail());
    assert!(matches!(r, Err(LoadError::TooLarge(_))), "{:?}", r.err());

    // Files over the size limit.
    let mut limits = Limits::thumbnail();
    limits.max_file_bytes = 10;
    assert!(slerp_gltf::load(Path::new("x.glb"), b"glTF\x02\0\0\0\0\0\0\0\0\0\0\0", &limits).is_err());

    // Not glTF at all.
    assert!(matches!(slerp_gltf::load(Path::new("x.gltf"), b"{\"a\":1}", &Limits::viewer()), Err(LoadError::NotMine)));
    assert!(slerp_gltf::load(Path::new("x.gltf"), b"{\"asset\":{\"version\":\"1.0\"}}", &Limits::viewer()).is_err());
}

#[test]
fn cyclic_hierarchy() {
    let json = r#"{"asset":{"version":"2.0"},"scenes":[{"nodes":[0, 0, 7]}],
        "nodes":[{"children":[1]},{"children":[0, 1, 2]},{"mesh":0, "skin":0}],
        "skins":[{"joints":[0, 1, 5]}],
        "meshes":[{"primitives":[{"attributes":{"POSITION":0}}]}],
        "accessors":[{"componentType":5126,"count":3,"type":"VEC3"}]}"#;
    let r = slerp_gltf::load(Path::new("x.gltf"), json.as_bytes(), &Limits::viewer());
    let Ok(slerp_scene::Asset::Scene(s)) = r else { panic!("{:?}", r.err()) };
    assert_eq!(s.nodes.len(), 3);
    assert!(s.skins.is_empty(), "a skin naming a missing node is dropped");
    assert!(!s.info.warnings.is_empty());
}

/// Plain Draco files cut short and with bytes flipped, of each encoding:
/// edgebreaker and sequential meshes of an old and a current bitstream,
/// and a kd-tree point cloud.
#[test]
fn damaged_drc_files() {
    let mut rng = Rng(7);
    for rel in [
        "draco-extra/google-draco/test_nm.obj.edgebreaker.0.9.1.drc",
        "draco-extra/google-draco/test_nm.obj.sequential.cl3.2.2.drc",
        "draco-extra/google-draco/car.drc",
        "draco-extra/google-draco/pc_kd_color.drc",
    ] {
        let Some(path) = asset(rel) else { continue };
        let bytes = std::fs::read(&path).unwrap();
        for len in (0..bytes.len()).step_by((bytes.len() / 500).max(1)) {
            let _ = slerp_gltf::load_drc(&path, &bytes[..len], &Limits::viewer());
        }
        for _ in 0..1000 {
            let mut copy = bytes.clone();
            for _ in 0..1 + rng.below(4) {
                let i = rng.below(copy.len());
                copy[i] ^= 1 << rng.below(8);
            }
            let _ = slerp_gltf::load_drc(&path, &copy, &Limits::viewer());
        }
    }
}
