//! Tests across the formats: sniffing, damaged files of every kind, and the
//! sample files in `local/assets` when they have been fetched.

use super::*;
use std::path::PathBuf;

#[test]
fn sniffs_by_signature() {
    let p = Path::new("x");
    assert_eq!(sniff(p, &mmd::tests::pmx(true)), Some(RiggedFormat::Pmx));
    assert_eq!(sniff(p, &mmd::tests::pmd()), Some(RiggedFormat::Pmd));
    assert_eq!(sniff(p, b"Vocaloid Motion Data 0002\0\0\0\0\0"), Some(RiggedFormat::Vmd));
    assert_eq!(sniff(p, b"INTERQUAKEMODEL\0\x02\0\0\0"), Some(RiggedFormat::Iqm));
    assert_eq!(sniff(p, b"# Inter-Quake Export\njoint"), Some(RiggedFormat::Iqe));
    assert_eq!(sniff(p, b"ACTRHEAD\0\0\0\0"), Some(RiggedFormat::Psk));
    assert_eq!(sniff(p, b"ANIMHEAD\0\0\0\0"), Some(RiggedFormat::Psa));
    assert_eq!(sniff(p, b"MD5Version 10\ncommandline \"\"\nnumJoints 1\nnumMeshes 1\n"), Some(RiggedFormat::Md5Mesh));
    assert_eq!(sniff(p, b"MD5Version 10\ncommandline \"\"\nnumFrames 1\n"), Some(RiggedFormat::Md5Anim));
    assert_eq!(sniff(p, b"version 1\nnodes\n0 \"a\" -1\nend\n"), Some(RiggedFormat::Smd));
    // Not ours: a PMX of an unknown version, a Mega Drive ROM, text.
    assert_eq!(sniff(p, b"PMX \0\0\x80\x40"), None);
    let mut rom = vec![0u8; 512];
    rom[256..268].copy_from_slice(b"SEGA GENESIS");
    assert_eq!(sniff(Path::new("game.smd"), &rom), None);
    assert_eq!(sniff(Path::new("a.smd"), b"version 2\nnodes\n"), None);
    assert_eq!(sniff(Path::new("a.psa"), b"Adobe Photoshop Album"), None);
    assert_eq!(sniff(p, b""), None);
}

/// Every prefix of a valid file, and the file with each byte changed,
/// loads or fails without panicking.
fn survives_damage(format: RiggedFormat, good: &[u8]) {
    let limits = Limits { neighbours: false, ..Limits::viewer() };
    for len in 0..good.len() {
        let _ = load(format, Path::new("damaged"), &good[..len], &limits);
    }
    for i in 0..good.len() {
        for flip in [0xFF, 0x80, 0x01] {
            let mut bad = good.to_vec();
            bad[i] ^= flip;
            let _ = load(format, Path::new("damaged"), &bad, &limits);
        }
    }
}

#[test]
fn damaged_files_never_panic() {
    // `load` catches panics, so this checks the readers report errors
    // rather than relying on that: a panic would print and fail here.
    let hook = std::panic::take_hook();
    let panicked = std::sync::Arc::new(std::sync::Mutex::new(None::<String>));
    let first = panicked.clone();
    std::panic::set_hook(Box::new(move |info| {
        first.lock().unwrap().get_or_insert_with(|| info.to_string());
    }));
    survives_damage(RiggedFormat::Psk, &psk::tests::triangle_psk());
    survives_damage(RiggedFormat::Iqm, &iqm::tests::triangle_iqm());
    survives_damage(RiggedFormat::Pmx, &mmd::tests::pmx(false));
    survives_damage(RiggedFormat::Pmd, &mmd::tests::pmd());
    survives_damage(RiggedFormat::Md5Mesh, md5::tests::CUBE_MESH.as_bytes());
    survives_damage(RiggedFormat::Smd, smd::tests::TRIANGLE.as_bytes());
    survives_damage(RiggedFormat::Iqe, iqe::tests::TRIANGLE.as_bytes());
    let motion = mmd::vmd::tests::write(
        "model",
        &[("a", 0, slerp_scene::glam::Vec3::ZERO, slerp_scene::glam::Quat::IDENTITY, [127, 0, 0, 127])],
        &[("b", 3, 1.0)],
    );
    survives_damage(RiggedFormat::Vmd, &motion);
    std::panic::set_hook(hook);
    let first = panicked.lock().unwrap().take();
    assert!(first.is_none(), "a reader panicked: {first:?}");
}

#[test]
fn absurd_counts_are_refused_quickly() {
    let limits = Limits::viewer();
    // A PMX header claiming two billion vertices.
    let mut pmx = b"PMX ".to_vec();
    pmx.extend(2.0f32.to_le_bytes());
    pmx.extend([8, 1, 0, 4, 4, 4, 4, 4, 4]);
    pmx.extend([0u8; 16]);
    pmx.extend(i32::MAX.to_le_bytes());
    assert!(load(RiggedFormat::Pmx, Path::new("a.pmx"), &pmx, &limits).is_err());
    // An MD5 mesh claiming a billion vertices in a few bytes.
    let md5 = b"MD5Version 10\nnumJoints 1\nnumMeshes 1\njoints {\n\"a\" -1 ( 0 0 0 ) ( 0 0 0 )\n}\nmesh {\nnumverts 1000000000\n}\n";
    assert!(load(RiggedFormat::Md5Mesh, Path::new("a.md5mesh"), md5, &limits).is_err());
    // An SMD whose nodes loop: each other's parent.
    let smd = b"version 1\nnodes\n0 \"a\" 1\n1 \"b\" 0\nend\nskeleton\ntime 0\n0 0 0 0 0 0 0\n1 0 0 1 0 0 0\nend\n";
    let Asset::Scene(scene) = load(RiggedFormat::Smd, Path::new("a.smd"), smd, &limits).unwrap() else { panic!() };
    assert_eq!(scene.roots.len(), 1);
    assert!(scene.info.warnings.iter().any(|w| w.contains("loop")));
}

/*┌─────────────────╮
| Sample files    |
└─────────────────╯*/
//region Sample files

fn assets() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../local/assets")
}

/// Every file under `dir` with one of `extensions`.
fn files(dir: &Path, extensions: &[&str]) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else { continue };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| extensions.iter().any(|x| e.eq_ignore_ascii_case(x)))
            {
                out.push(p);
            }
        }
    }
    out.sort();
    out
}

/// What some of the samples must load as: triangles, bones, clips and
/// textures at least.
const EXPECTED: &[(&str, usize, usize, usize, usize)] = &[
    ("md5/bob/Bob.md5mesh", 1000, 33, 1, 5),
    ("md5/bob/Bob.md5anim", 0, 0, 1, 0),
    ("iqm/mrfixit-sdk/mrfixit.iqm", 2988, 75, 1, 2),
    ("pmd/three-mmd/miku/miku_v2.pmd", 22961, 140, 0, 1),
    ("pmd/mmdagent/mei/mei.pmd", 14000, 140, 0, 10),
    ("pmx/gene/Gene_light.pmx", 47000, 224, 0, 10),
    ("pmx/appearance-miku/Appearance Miku.pmx", 37000, 130, 0, 5),
    ("psk/darklight-io-scene-psk-psa/Shrek.psk", 2000, 64, 135, 0),
    ("psk/darklight-io-scene-psk-psa/Shrek.psa", 0, 0, 135, 0),
    ("psk/darklight-io-scene-psk-psa/SK_1033_1033001.pskx", 138_000, 288, 0, 0),
    ("smd/assimp-wuson-holygrail/holy_grailref.smd", 896, 1, 0, 1),
];

/// Loads every sample of the formats: files in `invalid` and `malformed`
/// folders may fail (but never panic), every other one must load, the
/// animation-only ones with clips, and the samples in `EXPECTED` with at
/// least what it lists.  Skipped when the samples haven't been fetched.
#[test]
fn sample_files_load() {
    let root = assets().join("rigged-extra");
    let samples = files(&root, &["pmx", "pmd", "vmd", "md5mesh", "md5anim", "iqm", "iqe", "smd", "psk", "pskx", "psa"]);
    if samples.is_empty() {
        eprintln!("no samples in {}", root.display());
        return;
    }
    let mut failures = Vec::new();
    for path in &samples {
        // With `/` between folders on every system, as `EXPECTED` names them.
        let name = path.strip_prefix(&root).unwrap_or(path).to_string_lossy().replace('\\', "/");
        let damaged = name.contains("invalid/") || name.contains("malformed/");
        let bytes = std::fs::read(path).unwrap();
        let Some(format) = sniff(path, &bytes[..bytes.len().min(4096)]) else {
            if !damaged {
                failures.push(format!("{name}: not recognised"));
            }
            continue;
        };
        let started = std::time::Instant::now();
        let scene = match load(format, path, &bytes, &Limits::viewer()) {
            Ok(Asset::Scene(scene)) => scene,
            Ok(_) => {
                failures.push(format!("{name}: not a scene"));
                continue;
            }
            Err(e) => {
                if !damaged {
                    failures.push(format!("{name}: {e}"));
                }
                continue;
            }
        };
        let (triangles, bones) = (scene.triangle_count(), scene.skins.first().map_or(0, |s| s.joints.len()));
        let (clips, textures) = (scene.animations.len(), scene.textures.len());
        eprintln!(
            "{name}: {triangles} triangles, {bones} bones, {clips} clips, {textures} textures, {} warnings, {} ms",
            scene.info.warnings.len(),
            started.elapsed().as_millis()
        );
        if matches!(format, RiggedFormat::Md5Anim | RiggedFormat::Psa) && clips == 0 {
            failures.push(format!("{name}: no clips"));
        }
        if let Some(&(_, t, b, c, x)) = EXPECTED.iter().find(|e| e.0 == name)
            && (triangles < t || bones < b || clips < c || textures < x)
        {
            failures.push(format!("{name}: {triangles} triangles, {bones} bones, {clips} clips, {textures} textures"));
        }
        for a in &scene.animations {
            for ch in &a.channels {
                assert!(ch.node < scene.nodes.len());
                assert!(ch.values.iter().all(|v| v.is_finite()), "{name}: {}", a.name);
            }
        }
        for p in scene.meshes.iter().flat_map(|m| &m.primitives) {
            assert!(p.positions.iter().all(|v| v.is_finite()), "{name}");
            assert!(p.indices.iter().all(|&i| (i as usize) < p.positions.len()), "{name}");
            if p.is_skinned() {
                let joints = scene.skins[0].joints.len();
                assert!(p.joints.iter().flatten().all(|&j| (j as usize) < joints), "{name}");
            }
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

//endregion

/// Prints each sample model's signed volume (positive when triangles face
/// outwards), its bounds and where its triangles' normals point on
/// average, to check winding and axes.  Run by hand.
#[test]
#[ignore]
fn winding_and_axes() {
    let root = assets().join("rigged-extra");
    let names = [
        "md5/assimp/SimpleCube.md5mesh",
        "md5/bob/Bob.md5mesh",
        "iqm/mrfixit-sdk/mrfixit.iqm",
        "pmd/three-mmd/miku/miku_v2.pmd",
        "pmx/gene/Gene_light.pmx",
        "pmx/maya-mmd-tools-units/for_unit_test/test_1bone_cube.pmx",
        "psk/darklight-io-scene-psk-psa/Shrek.psk",
        "psk/darklight-suzanne/Suzanne.psk",
        "psk/darklight-io-scene-psk-psa/Bat.psk",
        "psk/darklight-io-scene-psk-psa/CS_Sarge_S0_Skelmesh.pskx",
        "psk/darklight-io-scene-psk-psa/SK_1033_1033001.pskx",
        "psk/darklight-io-scene-psk-psa/WEP_BroadSword_SKEL.psk",
        "psk/darklight-psk-psa-py/psk/Grunt_Preview_LOD0.psk",
        "psk/darklight-psk-psa-py/psk/player_body.psk",
        "psk/darklight-psk-psa-py/psk/carlos_head_carlos.psk",
        "psk/pyffi/examplemesh.psk",
        "smd/assimp-wuson-holygrail/holy_grailref.smd",
        "smd/assimp-wuson-holygrail/WusonSMD.smd",
    ];
    for name in names {
        let path = root.join(name);
        let Ok(bytes) = std::fs::read(&path) else { continue };
        let format = sniff(&path, &bytes).unwrap();
        let Ok(Asset::Scene(scene)) = load(format, &path, &bytes, &Limits::thumbnail()) else { continue };
        let mut volume = 0.0f64;
        let mut lo = slerp_scene::glam::Vec3::splat(f32::MAX);
        let mut hi = -lo;
        let mut front = 0.0f64;
        for p in scene.meshes.iter().flat_map(|m| &m.primitives) {
            for t in p.indices.chunks_exact(3) {
                let [a, b, c] = [0, 1, 2].map(|k| p.positions[t[k] as usize]);
                volume += a.dot(b.cross(c)) as f64 / 6.0;
                // Area-weighted normal along +Z: positive when more surface
                // faces +Z (the camera) than away.
                let n = (b - a).cross(c - a);
                if (a + b + c).z / 3.0 > 0.0 {
                    front += n.z as f64;
                }
            }
            for v in &p.positions {
                lo = lo.min(*v);
                hi = hi.max(*v);
            }
        }
        eprintln!(
            "{name}: volume {volume:.4}, size {:.3?}, centre {:.3?}, front-half normals along +Z {front:.4}",
            hi - lo,
            (hi + lo) * 0.5
        );
    }
}

/// MikuMikuDance's usual case: a model with its motions.  Beside the
/// model, a motion plays as a clip that moves bones and the face's morphs;
/// added through the viewer (the retargeting crate), it plays by bone name.
// Links the files into a scratch folder, which Windows allows only with
// developer mode or administrator rights.
#[cfg(unix)]
#[test]
fn gene_plays_its_motions() {
    let root = assets().join("rigged-extra");
    let (model, motion) = (root.join("pmx/gene/Gene_light.pmx"), root.join("vmd/gene/motion/01_happy.vmd"));
    if !model.is_file() || !motion.is_file() {
        eprintln!("skipped: Gene isn't downloaded");
        return;
    }
    // 1. Beside the model, through links in a scratch folder.
    let dir = std::env::temp_dir().join(format!("slerp-rigged-gene-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let link = |from: &Path, name: &str| std::os::unix::fs::symlink(from, dir.join(name)).unwrap();
    link(&model, "Gene_light.pmx");
    link(&model.parent().unwrap().join("light"), "light");
    link(&motion, "01_happy.vmd");
    let path = dir.join("Gene_light.pmx");
    let Ok(Asset::Scene(scene)) = load(RiggedFormat::Pmx, &path, &std::fs::read(&path).unwrap(), &Limits::viewer())
    else {
        panic!("Gene didn't load")
    };
    std::fs::remove_dir_all(&dir).unwrap();
    assert_eq!(scene.animations.len(), 1);
    let clip = &scene.animations[0];
    assert!(clip.duration > 0.5, "{}", clip.duration);
    let rotations = clip.channels.iter().filter(|c| c.property == slerp_scene::Property::Rotation).count();
    assert!(rotations > 5, "{rotations} bones turn");
    let weights = clip.channels.iter().find(|c| c.property == slerp_scene::Property::Weights).expect("face morphs");
    assert!(weights.values.iter().any(|&w| w > 0.5), "the expression shows");
    assert!(scene.textures.len() >= 10);

    // 2. Added to the model as a library.
    let bytes = std::fs::read(&motion).unwrap();
    let Ok(Asset::Scene(library)) = load(RiggedFormat::Vmd, &motion, &bytes, &Limits::viewer()) else { panic!() };
    let mut target = scene;
    target.animations.clear();
    assert_eq!(slerp_retarget::strategy(&target, &library).unwrap(), slerp_retarget::Strategy::SameNames);
    let added = slerp_retarget::add_animations(&mut target, &library).unwrap();
    assert_eq!(added, ["01_happy"]);
    let bones_moved =
        target.animations[0].channels.iter().filter(|c| c.property != slerp_scene::Property::Weights).count();
    assert!(bones_moved > 5, "{bones_moved}");
    // The motion names the model it was made for, as the model names
    // itself, so the face's morphs come along.
    assert!(target.animations[0].channels.iter().any(|c| c.property == slerp_scene::Property::Weights));
}

/// Prints which way Bob's face points: the face mesh's centre against the
/// head joint's.  Run by hand.
#[test]
#[ignore]
fn bob_faces() {
    let path = assets().join("rigged-extra/md5/bob/Bob.md5mesh");
    let Ok(bytes) = std::fs::read(&path) else { return };
    let Ok(Asset::Scene(s)) = load(RiggedFormat::Md5Mesh, &path, &bytes, &Limits::viewer()) else { panic!() };
    let pose = slerp_scene::Pose::rest(&s);
    let head = s.nodes.iter().position(|n| n.name == "head").unwrap();
    let h = pose.world[head].w_axis.truncate();
    for m in &s.meshes {
        let p = &m.primitives[0];
        let c = p.positions.iter().copied().sum::<slerp_scene::glam::Vec3>() / p.positions.len() as f32;
        eprintln!("{}: centre {c:.3?}, from head {:.3?}", m.name, c - h);
    }
}

/// How far each clip's first frame puts the bones from the bind pose, as a
/// share of the skeleton's size: small for idles and walks when the
/// rotation conventions are right, large when a convention is wrong.
/// Run by hand.
#[test]
#[ignore]
fn clips_start_near_the_bind_pose() {
    let root = assets().join("rigged-extra");
    for name in [
        "psk/darklight-io-scene-psk-psa/Shrek.psk",
        "psk/darklight-io-scene-psk-psa/WEP_BroadSword_SKEL.psk",
        "psk/pyffi/examplemesh.psk",
        "md5/bob/Bob.md5mesh",
        "iqm/mrfixit-sdk/mrfixit.iqm",
    ] {
        let path = root.join(name);
        let Ok(bytes) = std::fs::read(&path) else { continue };
        let format = sniff(&path, &bytes).unwrap();
        let Ok(Asset::Scene(s)) = load(format, &path, &bytes, &Limits::viewer()) else { panic!("{name}") };
        let rest = slerp_scene::Pose::rest(&s);
        let joints = &s.skins[0].joints;
        let pos = |p: &slerp_scene::Pose, j: usize| p.world[j].w_axis.truncate();
        let mut lo = slerp_scene::glam::Vec3::splat(f32::MAX);
        let mut hi = -lo;
        for &j in joints {
            lo = lo.min(pos(&rest, j));
            hi = hi.max(pos(&rest, j));
        }
        let size = (hi - lo).length().max(1e-6);
        let mut report = Vec::new();
        for (a, clip) in s.animations.iter().enumerate().take(6) {
            let mut pose = slerp_scene::Pose::rest(&s);
            pose.sample(&s, a, 0.0);
            pose.update_world(&s);
            let mean =
                joints.iter().map(|&j| (pos(&pose, j) - pos(&rest, j)).length()).sum::<f32>() / joints.len() as f32;
            report.push(format!("{} {:.2}", clip.name, mean / size));
        }
        eprintln!("{name}: {}", report.join(", "));
    }
}
