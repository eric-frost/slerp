use super::*;
use crate::testutil::{self, asset};
use slerp_scene::textures::Pixels;

fn rgba(t: &Texture) -> &[u8] {
    match t.base() {
        Pixels::Rgba8(p) => p,
        _ => &[],
    }
}

fn load_asset(rel: &str, limits: &Limits) -> Option<Result<Asset>> {
    let p = asset(rel)?;
    let bytes = std::fs::read(&p).unwrap();
    Some(load(&p, &bytes, limits))
}

fn scene(rel: &str, limits: &Limits) -> Option<Scene> {
    match load_asset(rel, limits)? {
        Ok(Asset::Scene(s)) => Some(s),
        Ok(_) => panic!("{rel} is not a scene"),
        Err(e) => panic!("{rel}: {e}"),
    }
}

#[test]
fn models_with_companion_files() {
    let Some(s) = scene("quake-extra/hlmdl/man.mdl", &Limits::viewer()) else { return };
    eprintln!(
        "man: {} tris, {} textures, clips {:?}, warnings {:?}",
        s.triangle_count(),
        s.textures.len(),
        s.animations.iter().map(|a| &a.name).collect::<Vec<_>>(),
        s.info.warnings
    );
    assert_eq!(s.textures.len(), 5, "textures come from manT.mdl");
    assert_eq!(s.animations.iter().map(|a| a.name.as_str()).collect::<Vec<_>>(), ["reference", "walk", "arms_up"]);
    assert_eq!(s.skins[0].joints.len(), 8);
    assert!(s.info.warnings.is_empty(), "{:?}", s.info.warnings);
    // Hidden alternative submodels exist for the second body part.
    assert!(
        s.nodes.iter().any(|n| n.mesh.is_some() && !n.visible)
            || s.nodes.iter().filter(|n| n.mesh.is_some()).count() == 2
    );

    let alone = scene("quake-extra/hlmdl/man.mdl", &Limits { neighbours: false, ..Limits::viewer() }).unwrap();
    assert!(alone.textures.is_empty() && !alone.info.warnings.is_empty());
    let thumb = scene("quake-extra/hlmdl/man.mdl", &Limits::thumbnail()).unwrap();
    assert!(thumb.animations.is_empty());

    let Some(Ok(Asset::Gallery(g))) = load_asset("quake-extra/hlmdl/manT.mdl", &Limits::viewer()) else {
        panic!("manT.mdl is a gallery")
    };
    assert_eq!(g.len(), 5);
}

#[test]
fn winding_matches_stored_normals() {
    for name in ["man", "chrome_sphere", "multiple_roots", "alpha_test"] {
        let Some(s) = scene(&format!("quake-extra/hlmdl/{name}.mdl"), &Limits::viewer()) else { return };
        let (mut agree, mut total) = (0, 0);
        for prim in s.meshes.iter().flat_map(|m| &m.primitives) {
            for t in prim.indices.chunks_exact(3) {
                let p = [0, 1, 2].map(|k| prim.positions[t[k] as usize]);
                let n = (p[1] - p[0]).cross(p[2] - p[0]);
                let stored = prim.normals[t[0] as usize] + prim.normals[t[1] as usize] + prim.normals[t[2] as usize];
                if n.length() > 1e-9 {
                    total += 1;
                    agree += (n.dot(stored) > 0.0) as usize;
                }
            }
        }
        eprintln!("{name}: {agree} of {total} triangles agree with their normals");
        assert!(agree * 10 > total * 9, "{name}: {agree} of {total}");
    }
}

#[test]
fn sequences_decode_and_move_bones() {
    let Some(s) = scene("quake-extra/hlmdl/chrome_sphere.mdl", &Limits::viewer()) else { return };
    let idle = &s.animations[0];
    assert!((idle.duration - 220.0 / 25.0).abs() < 1e-4);
    let mut pose = slerp_scene::Pose::rest(&s);
    let rest = pose.local.clone();
    pose.sample(&s, 0, idle.duration / 2.0);
    assert!(pose.local.iter().zip(&rest).any(|(a, b)| a != b), "the idle sequence moves something");

    let Some(man) = scene("quake-extra/hlmdl/man.mdl", &Limits::viewer()) else { return };
    let walk = man.animations.iter().position(|a| a.name == "walk").unwrap();
    let rest = man.bounds(&slerp_scene::Pose::rest(&man));
    let mut pose = slerp_scene::Pose::rest(&man);
    pose.sample(&man, walk, 0.3);
    pose.update_world(&man);
    let b = man.bounds(&pose);
    // The test man is ten units tall; walking keeps him about that size and
    // upright (Y up).
    let (r, w) = (rest.size(), b.size());
    assert!(r.y > r.x && r.y > r.z, "{r:?}");
    assert!((w.y / r.y - 1.0).abs() < 0.3, "{w:?} vs {r:?}");
}

#[test]
fn texture_flags() {
    let Some(s) = scene("quake-extra/hlmdl/alpha_test.mdl", &Limits::viewer()) else { return };
    assert!(matches!(s.materials[0].alpha_mode, AlphaMode::Mask(_)));
    assert!(rgba(&s.textures[0]).chunks_exact(4).any(|p| p[3] == 0));
    let Some(s) = scene("quake-extra/hlmdl/blend_additive.mdl", &Limits::viewer()) else { return };
    assert!(s.materials.iter().all(|m| m.alpha_mode == AlphaMode::Blend && m.unlit));
}

#[test]
fn odd_test_models_load() {
    for name in [
        "duplicate_bodyparts",
        "duplicate_sequences",
        "duplicate_submodels",
        "multiple_roots",
        "sequence_transitions",
        "unnamed_bodyparts",
        "unnamed_bones",
        "unnamed_sequences",
        "duplicate_sequence_groups/duplicate_sequence_groups",
        "unnamed_sequence_groups/unnamed_sequence_groups",
    ] {
        let Some(s) = scene(&format!("quake-extra/hlmdl/{name}.mdl"), &Limits::viewer()) else { return };
        let names: Vec<&str> = s.animations.iter().map(|a| a.name.as_str()).collect();
        let mut unique = names.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(unique.len(), names.len(), "{name}: clip names are unique");
        assert!(s.triangle_count() > 0);
    }
}

#[test]
fn corrupt_models_never_panic() {
    for name in ["man", "chrome_sphere"] {
        let Some(p) = asset(&format!("quake-extra/hlmdl/{name}.mdl")) else { return };
        let bytes = std::fs::read(&p).unwrap();
        for (i, m) in testutil::mutations(&bytes).iter().enumerate() {
            let r = std::panic::catch_unwind(|| load(&p, m, &Limits::viewer()).map(|_| ()));
            assert!(r.is_ok(), "{name} mutation {i} panicked");
        }
    }
}

#[test]
fn debug_render() {
    let Some(s) = scene("quake-extra/hlmdl/man.mdl", &Limits::viewer()) else { return };
    let img = testutil::render(&s, Vec3::new(-1.0, -0.3, -1.0), 512, true);
    testutil::write_png(Path::new("/tmp/slerp-quake/hl-man.png"), 512, 512, &img);
    let Some(s) = scene("quake-extra/hlmdl/chrome_sphere.mdl", &Limits::viewer()) else { return };
    let img = testutil::render(&s, Vec3::new(-1.0, -0.3, -1.0), 512, true);
    testutil::write_png(Path::new("/tmp/slerp-quake/hl-sphere.png"), 512, 512, &img);
}
