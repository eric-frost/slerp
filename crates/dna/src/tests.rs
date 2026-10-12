//! Tests on synthetic headers and, when chalk's checkout is present, on
//! chalk's own exported MetaHuman (`tools/humans/characters/tate/Tate`,
//! read only) and the heads chalk bakes from it with openriglogic's Python
//! bindings (`html/demos/humans/tate-tee-lod*.glb`).  Epic's DNA
//! Calibration samples (Ada, Taro) are licensed for Unreal use only and
//! are never used here.

use super::*;
use glam::{Mat4, Vec3};
use std::collections::HashMap;

/// A checkout of chalk in the folder beside this repository's.
const CHALK: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../../chalk");

/// A file of chalk's Tate export, when the checkout is there.
fn tate(file: &str) -> Option<PathBuf> {
    let p = Path::new(CHALK).join("tools/humans/characters/tate/Tate").join(file);
    p.is_file().then_some(p)
}

/// One of chalk's baked heads.
fn baked(file: &str) -> Option<Scene> {
    let p = Path::new(CHALK).join("html/demos/humans").join(file);
    let bytes = std::fs::read(&p).ok()?;
    match slerp_gltf::load(&p, &bytes, &Limits::viewer()).ok()? {
        slerp_scene::Asset::Scene(s) => Some(s),
        _ => None,
    }
}

fn request(lods: &[u16], blend_shapes: bool) -> Request {
    Request { lods: lods.to_vec(), geometry: true, blend_shapes, probe_rig: false }
}

/// Every vertex of the meshes named `name` (a DNA mesh at any level, or a
/// GLB node), posed: morph targets at the pose's weights, then skinned.
fn posed(scene: &Scene, pose: &Pose, name: &str) -> Vec<Vec3> {
    let mut out = Vec::new();
    for (ni, node) in scene.nodes.iter().enumerate() {
        let Some(mesh) = node.mesh.and_then(|m| scene.meshes.get(m)) else { continue };
        if node.name != name && scene::base_name(&mesh.name) != name {
            continue;
        }
        let joints = node.skin.map(|s| pose.joint_matrices(scene, s));
        let weights = &pose.weights[ni];
        for p in &mesh.primitives {
            for (i, &v) in p.positions.iter().enumerate() {
                let mut v = v;
                for (t, target) in p.targets.iter().enumerate() {
                    let w = weights.get(t).copied().unwrap_or(0.0);
                    if w != 0.0 && !target.positions.is_empty() {
                        v += target.positions[i] * w;
                    }
                }
                let v = match (&joints, p.is_skinned()) {
                    (Some(j), true) => {
                        let mut m = Mat4::ZERO;
                        for k in 0..4 {
                            m += j[p.joints[i][k] as usize] * p.weights[i][k];
                        }
                        m.transform_point3(v)
                    }
                    _ => pose.world[ni].transform_point3(v),
                };
                out.push(v);
            }
        }
    }
    out
}

/// Every vertex's texture coordinates, in the order `posed` lists them.
fn uvs(scene: &Scene, name: &str) -> Vec<glam::Vec2> {
    let mut out = Vec::new();
    for node in &scene.nodes {
        let Some(mesh) = node.mesh.and_then(|m| scene.meshes.get(m)) else { continue };
        if node.name == name || scene::base_name(&mesh.name) == name {
            for p in &mesh.primitives {
                out.extend(p.uv0.iter().copied());
            }
        }
    }
    out
}

/// For each point of `a`, the index of the nearest point of `b` and the
/// distance to it, through a grid of 5 mm cells.  Texture coordinates
/// break ties, since the lips' vertices meet at rest: a UV unit counts as
/// 10 cm.
fn nearest(a: &[Vec3], a_uv: &[glam::Vec2], b: &[Vec3], b_uv: &[glam::Vec2]) -> Vec<(usize, f32)> {
    const CELL: f32 = 0.005;
    let key = |p: Vec3| ((p.x / CELL).floor() as i32, (p.y / CELL).floor() as i32, (p.z / CELL).floor() as i32);
    let mut grid: HashMap<(i32, i32, i32), Vec<usize>> = HashMap::new();
    for (i, &p) in b.iter().enumerate() {
        grid.entry(key(p)).or_default().push(i);
    }
    a.iter()
        .enumerate()
        .map(|(ai, &p)| {
            let (x, y, z) = key(p);
            let mut best = (usize::MAX, f32::INFINITY, f32::INFINITY);
            for dx in -1..=1 {
                for dy in -1..=1 {
                    for dz in -1..=1 {
                        for &i in grid.get(&(x + dx, y + dy, z + dz)).into_iter().flatten() {
                            let d = (b[i] - p).length();
                            let score = d + 0.1 * (b_uv[i] - a_uv[ai]).length();
                            if score < best.2 {
                                best = (i, d, score);
                            }
                        }
                    }
                }
            }
            (best.0, best.1)
        })
        .collect()
}

/*┌─────────────────╮
| Headers         |
└─────────────────╯*/
//region Headers

#[test]
fn sniffs_the_signature() {
    assert!(sniff(b"DNA\x00\x02\x00\x05\x00\x00\x00\x09"));
    assert!(sniff(b"DNA\x00\x02\x00\x01"));
    assert!(!sniff(b"DNA\x00\x01\x00\x05"));
    assert!(!sniff(b"DNA\x00\x02\x00\x00"));
    assert!(!sniff(b"\x09\x00\x0eSnapGene"));
    assert!(!sniff(b"DNA"));
}

/// A header with an index entry per (offset, size), padded to `len`
/// bytes.
fn header(sections: &[(u32, u32)], len: usize) -> Vec<u8> {
    let mut v = b"DNA\x00\x02\x00\x05".to_vec();
    v.extend_from_slice(&(sections.len() as u32).to_be_bytes());
    for &(offset, size) in sections {
        v.extend_from_slice(b"desc\x00\x01\x00\x01");
        v.extend_from_slice(&offset.to_be_bytes());
        v.extend_from_slice(&size.to_be_bytes());
    }
    v.resize(len.max(v.len()), 0);
    v
}

#[test]
fn headers_are_checked_against_the_file() {
    let dir = std::env::temp_dir().join(format!("slerp-dna-headers-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join("x.dna");
    let check = |bytes: &[u8]| {
        std::fs::write(&p, bytes).unwrap();
        check_header(&p, u64::MAX)
    };
    assert!(check(&header(&[(27, 10)], 37)).is_ok());
    assert!(matches!(check(&header(&[(27, 11)], 37)), Err(LoadError::Invalid(_))));
    assert!(matches!(check(&header(&[(u32::MAX, u32::MAX)], 64)), Err(LoadError::Invalid(_))));
    assert!(matches!(check(&header(&[(0, 0); 65], 2000)), Err(LoadError::Invalid(_))));
    assert!(matches!(check(&header(&[(27, 10)], 37)[..20]), Err(LoadError::Invalid(_))));
    assert!(matches!(check(b"DNA\x00\x02"), Err(LoadError::NotMine)));
    assert!(matches!(check(b"glTF"), Err(LoadError::NotMine)));
    std::fs::write(&p, header(&[], 100)).unwrap();
    assert!(matches!(check_header(&p, 99), Err(LoadError::TooLarge(_))));
    // Files that pass the checks but hold nothing a scene can be made of
    // fail in the reader, cleanly.
    assert!(open(&p, &Options::thumbnail(128)).is_err());
    std::fs::remove_dir_all(&dir).unwrap();
}

//endregion

/*┌─────────────────╮
| chalk's Tate    |
└─────────────────╯*/
//region chalk's Tate

/// Position counts per level of detail, as chalk's README lists them for
/// Tate (its "Head vertices" and "Body vertices" columns).
#[test]
fn levels_of_detail_match_chalks_counts() {
    let (Some(head), Some(body)) = (tate("head.dna"), tate("body.dna")) else { return };
    let opts = Options::viewer();
    for (path, base, counts) in
        [(&head, "head", [24_049, 11_977, 5_999, 2_548]), (&body, "body", [30_455, 7_622, 3_328, 985])]
    {
        let (d, _) = read(path, &request(&[], false), &opts).unwrap();
        for (lod, want) in counts.into_iter().enumerate() {
            let m = d.lod_meshes[lod].iter().find(|&&m| scene::base_name(&d.mesh_names[m as usize]) == base).unwrap();
            let (_, data) = d.meshes.iter().find(|(i, _)| i == m).unwrap();
            assert_eq!(data.positions.len() / 3, want, "{base} level {lod}");
        }
    }
    let (d, _) = read(&head, &request(&[7], false), &opts).unwrap();
    assert_eq!(d.lod_count, 1, "one level asked for, one read");
    let (d, _) = read(&head, &Request::default(), &opts).unwrap();
    assert_eq!((d.lod_count, d.controls.len(), d.channels.len()), (8, 263, 782));
}

/// The neutral head at levels 0 and 1 against the heads chalk baked from
/// the same file at those levels: every baked vertex has one of ours at
/// its place, within the precision of the GLB's quantised positions.
#[test]
fn neutral_head_matches_chalks_bake() {
    let Some(head) = tate("head.dna") else { return };
    for lod in [0u16, 1] {
        let Some(glb) = baked(&format!("tate-tee-lod{lod}.glb")) else { continue };
        let ours = open(&head, &Options { lod, ..Options::thumbnail(512) }).unwrap().scene;
        let mine = posed(&ours, &Pose::rest(&ours), "head");
        let theirs = posed(&glb, &Pose::rest(&glb), "head");
        let d = nearest(&theirs, &uvs(&glb, "head"), &mine, &uvs(&ours, "head"));
        let max = d.iter().map(|x| x.1).fold(0f32, f32::max);
        let mean = d.iter().map(|x| x.1).sum::<f32>() / d.len() as f32;
        println!(
            "level {lod}: {} baked vertices, {} ours, nearest mean {:.4} mm, max {:.4} mm",
            theirs.len(),
            mine.len(),
            mean * 1e3,
            max * 1e3
        );
        assert!(theirs.len() > mine.len() * 9 / 10, "{} of {}", theirs.len(), mine.len());
        assert!(max < 2e-4, "max {max}");
    }
}

/// Opening the jaw: RigLogic turns the jaw joint and weights the jaw's
/// corrective blend shapes, and the posed head lands where chalk's baked
/// `jawOpen` shape puts it (chalk evaluates the same rig in Python and
/// skins with every influence; Slerp keeps the strongest four).
#[test]
fn jaw_open_moves_the_jaw_and_its_shapes() {
    let Some(head) = tate("head.dna") else { return };
    let mut opts = Options::viewer();
    opts.body = false;
    opts.textures = false;
    let mut c = open(&head, &opts).unwrap();
    for (k, v) in &c.scene.info.details {
        println!("{k}: {v}");
    }
    println!("{:?}", c.timings);
    let rig = c.head_rig.as_mut().unwrap();
    let jaw = rig.control("jawOpen").unwrap();
    rig.values[jaw] = 1.0;
    let mut pose = Pose::rest(&c.scene);
    c.apply(&mut pose).unwrap();
    pose.update_world(&c.scene);
    let rig = c.head_rig.as_ref().unwrap();

    // 1. The jaw joint turns, by many degrees.
    let j = c.main.joint_names.iter().position(|n| n == "FACIAL_C_Jaw").unwrap();
    let out = &rig.joint_outputs()[j * 9..j * 9 + 9];
    let turn = Vec3::new(out[3], out[4], out[5]);
    println!("jaw joint offsets: {out:?}");
    assert!(turn.length() > 10.0, "{turn}");
    let rest = Pose::rest(&c.scene);
    let node = rig.joint_node(j).unwrap();
    let chin_rest = rest.world[node].transform_vector3(Vec3::Z);
    let chin_open = pose.world[node].transform_vector3(Vec3::Z);
    assert!(chin_rest.angle_between(chin_open) > 0.2);

    // 2. Its blend shape channels weigh in.
    let weights = rig.channel_outputs();
    let active: Vec<(&String, f32)> =
        c.main.channels.iter().zip(weights).filter(|(_, w)| **w > 0.05).map(|(n, w)| (n, *w)).collect();
    println!("{} channels active: {:?}", active.len(), &active[..active.len().min(12)]);
    assert!(
        active.iter().any(|(n, w)| n.to_lowercase().contains("jaw") && n.to_lowercase().contains("open") && *w > 0.5)
    );
    assert!(active.len() < 40, "a lone control shouldn't weigh in the whole face");

    // 3. Against chalk's bake.
    let Some(glb) = baked("tate-tee-lod0.glb") else { return };
    let mine_rest = posed(&c.scene, &rest, "head");
    let mine_open = posed(&c.scene, &pose, "head");
    let mut glb_pose = Pose::rest(&glb);
    let head_node = glb.nodes.iter().position(|n| n.name == "head" && n.mesh.is_some()).unwrap();
    let mesh = &glb.meshes[glb.nodes[head_node].mesh.unwrap()];
    let t = mesh.target_names.iter().position(|n| n == "jawOpen").unwrap();
    let theirs_rest = posed(&glb, &glb_pose, "head");
    glb_pose.weights[head_node][t] = 1.0;
    let theirs_open = posed(&glb, &glb_pose, "head");
    let pairs = nearest(&theirs_rest, &uvs(&glb, "head"), &mine_rest, &uvs(&c.scene, "head"));
    let mut errors: Vec<f32> = Vec::new();
    for (i, &(k, _)) in pairs.iter().enumerate() {
        let (a, b) = (theirs_open[i] - theirs_rest[i], mine_open[k] - mine_rest[k]);
        if a.length() > 1e-3 {
            errors.push((a - b).length());
        }
    }
    errors.sort_by(f32::total_cmp);
    let mean = errors.iter().sum::<f32>() / errors.len() as f32;
    let p99 = errors[errors.len() * 99 / 100];
    println!(
        "{} vertices move over 1 mm; difference from chalk's bake: mean {:.3} mm, 99th percentile {:.3} mm, max {:.3} mm",
        errors.len(),
        mean * 1e3,
        p99 * 1e3,
        errors.last().unwrap() * 1e3
    );
    assert!(errors.len() > 2000);
    let max = *errors.last().unwrap();
    assert!(mean < 4e-4 && p99 < 2.5e-3 && max < 5e-3, "mean {mean}, 99th percentile {p99}, max {max}");
}

/// Every ARKit shape's controls exist in a MetaHuman face rig, and a level
/// of detail switch keeps every joint at its node, so clips added at one
/// level play at another, while the coarser level moves by joints alone.
#[test]
fn arkit_controls_and_level_switches() {
    let Some(head) = tate("head.dna") else { return };
    let mut opts = Options::viewer();
    opts.textures = false;
    opts.body = false;
    opts.blend_shapes = false;
    let mut c = open(&head, &opts).unwrap();
    let rig = c.head_rig.as_ref().unwrap();
    for (shape, controls) in ARKIT_CONTROLS {
        for control in *controls {
            assert!(rig.control(control).is_some(), "{shape}: {control}");
        }
    }
    let names: Vec<String> = c.scene.nodes.iter().map(|n| n.name.clone()).collect();
    let joints = c.main.joint_names.len();
    c.set_lod(2).unwrap();
    assert_eq!(c.lod(), 2);
    assert!(c.scene.nodes[..joints].iter().map(|n| &n.name).eq(names[..joints].iter()));
    assert!(c.scene.meshes[0].name.contains("lod2"));
    let rig = c.head_rig.as_mut().unwrap();
    let jaw = rig.control("jawOpen").unwrap();
    rig.values[jaw] = 1.0;
    let mut pose = Pose::rest(&c.scene);
    c.apply(&mut pose).unwrap();
    let node = c.scene.nodes.iter().position(|n| n.name == "FACIAL_C_Jaw").unwrap();
    assert!(pose.local[node].rotation.angle_between(c.scene.nodes[node].transform.rotation) > 0.2);
}

/// A body's correctives read joint rotations relative to the rest pose:
/// standing still asks nothing of them, a bent elbow does.
#[test]
fn body_correctives_follow_the_joints() {
    let Some(body) = tate("body.dna") else { return };
    let mut opts = Options::viewer();
    opts.textures = false;
    opts.blend_shapes = false;
    let mut c = open(&body, &opts).unwrap();
    assert!(c.body_rig.is_some() && c.head_rig.is_some(), "the head beside the body is loaded with it");
    let mut pose = Pose::rest(&c.scene);
    c.apply(&mut pose).unwrap();
    let rest_out = c.body_rig.as_ref().unwrap().joint_outputs().iter().fold(0f32, |m, x| m.max(x.abs()));
    assert!(rest_out < 1e-3, "{rest_out}");
    let elbow = c.scene.nodes.iter().position(|n| n.name == "lowerarm_l").unwrap();
    let mut pose = Pose::rest(&c.scene);
    pose.local[elbow].rotation *= glam::Quat::from_rotation_z(1.2);
    c.apply(&mut pose).unwrap();
    let bent = c.body_rig.as_ref().unwrap().joint_outputs().iter().fold(0f32, |m, x| m.max(x.abs()));
    println!("largest corrective offset for a bent elbow: {bent}");
    assert!(bent > 0.1);
}

/// What a thumbnail gets: the head alone at a coarse level, no helper
/// meshes, textured from the maps beside the file (lashes come with their
/// texture), with no rig or clips.
#[test]
fn thumbnail_scene_is_the_neutral_head() {
    let Some(head) = tate("head.dna") else { return };
    let start = Instant::now();
    let scene = load(&head, &Limits { max_texture_size: Some(256), ..Limits::thumbnail() }).unwrap();
    println!("thumbnail scene in {:.0} ms", start.elapsed().as_secs_f64() * 1e3);
    let names: Vec<String> = scene.meshes.iter().map(|m| scene::base_name(&m.name)).collect();
    for part in ["head", "teeth", "eyeLeft", "eyeRight"] {
        assert!(names.iter().any(|n| n == part), "{part} missing from {names:?}");
    }
    assert!(!names.iter().any(|n| n == "saliva" || n == "eyeshell"), "helper meshes in {names:?}");
    assert!(scene.meshes[0].name.contains("lod3"));
    assert!(scene.textures.len() > 1, "the maps beside the file are used");
    assert!(scene.textures.iter().all(|t| t.width().max(t.height()) <= 256), "textures shrunk to the thumbnail");
    assert!(scene.animations.is_empty() && scene.skins.len() == 1);
    let b = scene.bounds(&Pose::rest(&scene));
    assert!(b.max.y > 1.8 && b.min.y > 1.3, "{b:?}");
}

/// Without the maps folder (as in GNOME's sandbox), the thumbnail is clay
/// with painted eyes.
#[test]
fn thumbnail_without_neighbours_is_clay() {
    let Some(head) = tate("head.dna") else { return };
    let scene = load(&head, &Limits { max_texture_size: Some(256), neighbours: false, ..Limits::thumbnail() }).unwrap();
    assert_eq!(scene.textures.len(), 1, "only the painted eye");
}

/// Damaged copies of the body file: cut short anywhere, or with bytes
/// changed inside its sections.  Each must load or fail, never hang or
/// take the caller down; on Unix a crash of the reader shows as an error.
#[test]
fn damaged_files_fail_cleanly() {
    let Some(body) = tate("body.dna") else { return };
    let bytes = std::fs::read(&body).unwrap();
    let dir = std::env::temp_dir().join(format!("slerp-dna-damaged-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("damaged.dna");
    let opts = Options { timeout: Duration::from_secs(20), ..Options::thumbnail(256) };
    let (mut ok, mut failed, mut crashed) = (0, 0, 0);
    let mut try_file = |data: &[u8]| {
        std::fs::write(&path, data).unwrap();
        match open(&path, &opts) {
            Ok(_) => ok += 1,
            Err(LoadError::Invalid(e)) if e.contains("crashed") => crashed += 1,
            Err(_) => failed += 1,
        }
    };
    // 1. Cut short, from inside the header to just before the end.
    for cut in [0usize, 3, 7, 11, 30, 200, 4096, 100_000, 1_000_000, bytes.len() / 2, bytes.len() - 1] {
        try_file(&bytes[..cut.min(bytes.len())]);
    }
    // 2. Bytes changed, at random but repeatably, after the section index;
    //    every other run only in the first sections, where the counts and
    //    names live.
    let mut seed = 0x2545_f491_4f6c_dd1du64;
    let mut next = || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    for run in 0..120 {
        let mut data = bytes.clone();
        let span = if run % 2 == 0 { 4096 } else { data.len() - 200 };
        for _ in 0..1 + run % 16 {
            let at = 200 + (next() as usize) % span;
            data[at] = next() as u8;
        }
        try_file(&data);
    }
    println!("damaged body files: {ok} loaded, {failed} refused, {crashed} crashed the reader process");
    assert!(failed > 0);
    std::fs::remove_dir_all(&dir).unwrap();
}

//endregion
