//! Tests against the downloaded test corpus in `local/assets/`: every file
//! loads without panicking, the well-known ones load with the expected
//! contents, and mutated copies (truncated, bytes flipped, numbers mangled)
//! never panic.  Tests skip themselves when the corpus isn't there.

use crate::{ModelFormat, load_scene, sniff};
use slerp_scene::{Limits, Scene};
use std::path::{Path, PathBuf};

fn assets() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../local/assets")
}

/// Corpus folders this crate's formats live in: the fetch tool's per-format
/// folders and this crate's own extra downloads.
const FOLDERS: [&str; 7] = ["models-extra", "fbx", "collada", "obj", "stl", "ply", "vox"];

fn corpus(max_bytes: u64) -> Vec<(PathBuf, ModelFormat)> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else { return };
        let mut entries: Vec<_> = entries.flatten().map(|e| e.path()).collect();
        entries.sort();
        for p in entries {
            if p.is_dir() {
                if p.file_name().is_some_and(|n| n != ".git") {
                    walk(&p, out);
                }
            } else {
                out.push(p);
            }
        }
    }
    let mut files = Vec::new();
    for f in FOLDERS {
        walk(&assets().join(f), &mut files);
    }
    files
        .into_iter()
        .filter(|p| std::fs::metadata(p).is_ok_and(|m| m.len() <= max_bytes))
        .filter_map(|p| {
            let bytes = std::fs::read(&p).ok()?;
            let format = sniff(&p, &bytes[..bytes.len().min(4096)])?;
            Some((p, format))
        })
        .collect()
}

fn load(path: &Path, limits: &Limits) -> Option<Scene> {
    let bytes = std::fs::read(path).ok()?;
    let format = sniff(path, &bytes[..bytes.len().min(4096)])?;
    load_scene(format, path, &bytes, limits).ok()
}

#[test]
fn whole_corpus_loads_without_panicking() {
    let files = corpus(64 << 20);
    if files.is_empty() {
        eprintln!("corpus missing; skipped");
        return;
    }
    let mut loaded = 0;
    for (path, format) in &files {
        let bytes = std::fs::read(path).unwrap();
        // Calls the loaders directly, so a panic fails the test instead of
        // being caught by `load`.
        if load_scene(*format, path, &bytes, &Limits::thumbnail()).is_ok() {
            loaded += 1;
        }
    }
    eprintln!("{loaded} of {} corpus files loaded", files.len());
    assert!(loaded * 2 > files.len(), "most corpus files should load");
}

#[test]
fn known_files_load_as_expected() {
    let root = assets();
    if !root.join("fbx").exists() {
        eprintln!("corpus missing; skipped");
        return;
    }
    let check = |rel: &str, f: &dyn Fn(&Scene)| {
        let path = root.join(rel);
        if path.exists() {
            let scene = load(&path, &Limits::viewer()).unwrap_or_else(|| panic!("{rel} failed to load"));
            f(&scene);
        }
    };
    check("ply/stanford-bunny/bun_zipper.ply", &|s| {
        assert_eq!(s.triangle_count(), 69451);
        assert_eq!(s.vertex_count(), 35947);
    });
    check("collada/assimp/duck.dae", &|s| {
        assert_eq!(s.triangle_count(), 4212);
        assert!(s.materials[0].base_color_texture.is_some(), "duck has its texture");
    });
    check("collada/assimp/library_animation_clips.dae", &|s| {
        assert_eq!(s.skins.len(), 1);
        assert_eq!(s.animations.len(), 2);
    });
    check("collada/assimp/teapots.DAE", &|s| {
        // Written with decimal commas.
        let b = s.bounds(&slerp_scene::Pose::rest(s));
        assert!(b.size().max_element() > 0.1, "{b:?}");
    });
    check("obj/assimp/spider.obj", &|s| {
        assert_eq!(s.triangle_count(), 1368);
        assert_eq!(s.materials.len(), 4);
    });
    check("fbx/assimp/huesitos.fbx", &|s| {
        assert_eq!(s.skins[0].joints.len(), 9);
        assert_eq!(s.animations.len(), 1);
    });
    check("fbx/ufbx/maya_blend_shape_cube_7700_binary.fbx", &|s| {
        assert!(!s.meshes[0].target_names.is_empty(), "blend shapes become morph targets");
    });
    check("collada/assimp/cube_UTF16LE.dae", &|s| assert_eq!(s.triangle_count(), 12));
    check("obj/assimp/box_UTF16BE.obj", &|s| assert_eq!(s.triangle_count(), 12));
    check("vox/ephtracy/vox/character/chr_knight.vox", &|s| {
        assert_eq!(s.triangle_count(), 1012);
    });
    let points = |s: &Scene| s.meshes[0].primitives[0].positions.len();
    check("models-extra/assimp-x/test_cube_compressed.x", &|s| {
        assert_eq!((s.triangle_count(), s.skins.len()), (12, 1));
    });
    check("models-extra/assimp-x-nonbsd/dwarf.x", &|s| {
        assert_eq!((s.triangle_count(), s.skins[0].joints.len(), s.animations.len()), (1896, 40, 1));
    });
    check("models-extra/3mf-samples/core/cube_gears.3mf", &|s| {
        assert_eq!((s.triangle_count(), s.meshes.len()), (25692, 17));
    });
    check("models-extra/assimp-3ds-nonbsd/jeep1.3ds", &|s| {
        assert_eq!(s.triangle_count(), 2032);
        assert!(s.materials.iter().any(|m| m.base_color_texture.is_some()));
    });
    check("models-extra/assimp-off/Wuson.off", &|s| assert_eq!(s.triangle_count(), 3732));
    check("models-extra/assimp-wrl/MotionCaptureROM.WRL", &|s| {
        assert_eq!((s.triangle_count(), s.animations.len()), (2000, 1));
    });
    check("models-extra/assimp-x3d-nonbsd/Chevy/ChevyTahoe.x3d", &|s| assert_eq!(s.triangle_count(), 1106));
    check("models-extra/pointclouds/pdal/laz/autzen_trim.laz", &|s| assert_eq!(points(s), 110_000));
    check("models-extra/pointclouds/e57-crate/bunnyDouble.e57", &|s| assert_eq!(points(s), 30_571));
    check("models-extra/pointclouds/pcl/milk_color.pcd", &|s| assert_eq!(points(s), 13_704));
    check("models-extra/pointclouds/pdal/ptx/1.2-with-color.ptx", &|s| assert_eq!(points(s), 1065));
}

#[test]
fn chalk_animation_library() {
    let path = Path::new(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../chalk/tools/humans/vendor/ual/ual1/Universal Animation Library[Standard]/Unity/UAL1_Standard.fbx"
    ));
    if !path.exists() {
        eprintln!("chalk missing; skipped");
        return;
    }
    let scene = load(path, &Limits::viewer()).unwrap();
    assert_eq!(scene.animations.len(), 42);
    assert_eq!(scene.skins[0].joints.len(), 65);
    // Facing +Z like the same library's glTF export: the left toe is ahead
    // of the left ankle.
    let pose = slerp_scene::Pose::rest(&scene);
    let at = |name: &str| pose.world[scene.nodes.iter().position(|n| n.name == name).unwrap()].w_axis;
    assert!(at("ball_l").z > at("foot_l").z);
    assert!(at("hand_l").x > 0.5);
}

/// A small deterministic random number generator for the mutations.
struct XorShift(u64);

impl XorShift {
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

#[test]
fn mutated_corpus_files_never_panic() {
    let files = corpus(256 << 10);
    if files.is_empty() {
        eprintln!("corpus missing; skipped");
        return;
    }
    let mut rng = XorShift(0x9E37_79B9_7F4A_7C15);
    // A spread of files per format, so one format's many tiny test files
    // don't crowd out the others.
    let mut per_format: Vec<(ModelFormat, usize)> = Vec::new();
    let mut runs = 0;
    for (path, format) in &files {
        let count = match per_format.iter_mut().find(|(f, _)| f == format) {
            Some((_, c)) => c,
            None => {
                per_format.push((*format, 0));
                &mut per_format.last_mut().unwrap().1
            }
        };
        if *count >= 40 {
            continue;
        }
        *count += 1;
        let original = std::fs::read(path).unwrap();
        for round in 0..8 {
            let mut bytes = original.clone();
            match round % 4 {
                0 => bytes.truncate(rng.below(bytes.len())),
                1 => {
                    for _ in 0..1 + rng.below(16) {
                        let i = rng.below(bytes.len());
                        if let Some(b) = bytes.get_mut(i) {
                            *b = rng.next() as u8;
                        }
                    }
                }
                2 => {
                    // Large numbers where text formats expect counts and
                    // indices.
                    for _ in 0..1 + rng.below(8) {
                        let i = rng.below(bytes.len());
                        let junk: &[u8] =
                            [b"4294967295".as_slice(), b"-2147483648", b"1e38", b"nan", b"-1"][rng.below(5)];
                        bytes.splice(i..i, junk.iter().copied());
                    }
                }
                _ => {
                    // Binary formats: overwrite a little-endian u32 with an
                    // extreme value.
                    let i = rng.below(bytes.len().saturating_sub(4));
                    let v = [u32::MAX, 0x7fff_ffff, 0x8000_0000, 65536][rng.below(4)];
                    if let Some(slot) = bytes.get_mut(i..i + 4) {
                        slot.copy_from_slice(&v.to_le_bytes());
                    }
                }
            }
            let _ = load_scene(*format, path, &bytes, &Limits::thumbnail());
            runs += 1;
        }
    }
    eprintln!("{runs} mutated loads across {per_format:?}");
}

/// Point cloud files mutated many more times than the general test does,
/// since their decoders (`laz`, `e57`) are other people's code.
/// `SLERP_FUZZ_ROUNDS` raises the count for a longer run.
#[test]
fn mutated_point_clouds_never_panic_or_hang() {
    let root = assets().join("models-extra/pointclouds");
    let files: Vec<(PathBuf, ModelFormat)> = corpus(256 << 10)
        .into_iter()
        .filter(|(p, f)| p.starts_with(&root) && matches!(f, ModelFormat::Las | ModelFormat::E57 | ModelFormat::Pcd))
        .collect();
    if files.is_empty() {
        eprintln!("point cloud corpus missing; skipped");
        return;
    }
    let rounds: usize = std::env::var("SLERP_FUZZ_ROUNDS").ok().and_then(|v| v.parse().ok()).unwrap_or(48);
    let mut rng = XorShift(0x2545_F491_4F6C_DD1D);
    for (path, format) in &files {
        let original = std::fs::read(path).unwrap();
        for round in 0..rounds {
            let mut bytes = original.clone();
            if round % 3 == 0 {
                bytes.truncate(rng.below(bytes.len()));
            } else {
                for _ in 0..1 + rng.below(8) {
                    let i = rng.below(bytes.len());
                    if let Some(b) = bytes.get_mut(i) {
                        *b = rng.next() as u8;
                    }
                }
            }
            let start = std::time::Instant::now();
            let _ = load_scene(*format, path, &bytes, &Limits::thumbnail());
            assert!(start.elapsed().as_secs() < 5, "{} round {round} took {:?}", path.display(), start.elapsed());
        }
    }
}

/// Clips whose first keys are the rest pose put every node where the rest
/// pose does, which pins down each format's rotation conventions (DirectX's
/// conjugated quaternions, 3DS's relative angle-axis keys, VRML's split
/// transforms).
#[test]
fn clips_start_at_the_rest_pose() {
    let root = assets().join("models-extra");
    for (rel, clip) in [
        ("assimp-x/Testwuson.X", "Wuson_Bind"),
        ("assimp-x/BCN_Epileptic.X", "Epileptisch"),
        ("assimp-3ds/RotatingCube.3DS", "Keyframer"),
        ("assimp-3ds/CameraRollAnimWithChildObject.3ds", "Keyframer"),
        ("assimp-wrl/MotionCaptureROM.WRL", "Bip01-TIMER"),
    ] {
        let path = root.join(rel);
        let Some(s) = load(&path, &Limits::viewer()) else { continue };
        let rest = slerp_scene::Pose::rest(&s);
        let size = s.bounds(&rest).size().max_element().max(1e-6);
        let a = s.animations.iter().position(|a| a.name == clip).unwrap_or_else(|| panic!("{rel}: no clip {clip}"));
        let mut pose = slerp_scene::Pose::rest(&s);
        pose.sample(&s, a, 0.0);
        pose.update_world(&s);
        for (i, (r, p)) in rest.world.iter().zip(&pose.world).enumerate() {
            let moved = (r.w_axis - p.w_axis).truncate().length() / size;
            assert!(moved < 0.01, "{rel}: {} moves {moved} of the model's size", s.nodes[i].name);
            let (_, rq, _) = r.to_scale_rotation_translation();
            let (_, pq, _) = p.to_scale_rotation_translation();
            if !rel.ends_with(".WRL") {
                // VRML's scale orientations shear the bones' matrices,
                // which have no rotation to compare.
                assert!(rq.angle_between(pq) < 0.02, "{rel}: {} turns {}", s.nodes[i].name, rq.angle_between(pq));
            }
        }
    }
}
