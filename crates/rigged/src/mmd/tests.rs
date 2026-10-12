//! Small PMX and PMD files written here, with answers worked out by hand.

use super::vmd::tests::{LINEAR, write as write_vmd};
use super::*;
use slerp_scene::Property;
use slerp_scene::glam::Quat;

/// Writes PMX 2.0 in UTF-8 or UTF-16: a triangle on two bones (a root at
/// the origin and a child 10 units up), one material, a vertex morph that
/// lifts the top corner, a group morph of it at half strength and a bone
/// morph, which isn't shown.
pub fn pmx(utf8: bool) -> Vec<u8> {
    let text = |s: &str| -> Vec<u8> {
        let b: Vec<u8> =
            if utf8 { s.as_bytes().to_vec() } else { s.encode_utf16().flat_map(|u| u.to_le_bytes()).collect() };
        let mut out = (b.len() as i32).to_le_bytes().to_vec();
        out.extend(b);
        out
    };
    let f = |v: &[f32]| v.iter().flat_map(|x| x.to_le_bytes()).collect::<Vec<u8>>();
    let mut out = b"PMX ".to_vec();
    out.extend(2.0f32.to_le_bytes());
    // Globals: encoding, extra UVs, then index widths (vertex 1, texture 1,
    // material 1, bone 2, morph 1, rigid 1).
    out.extend([8, utf8 as u8, 0, 1, 1, 1, 2, 1, 1]);
    for s in ["テスト", "Test", "comment", ""] {
        out.extend(text(s));
    }
    out.extend(3i32.to_le_bytes());
    let corners = [([0.0, 0.0, 0.0], [0.0, 0.0]), ([10.0, 0.0, 0.0], [1.0, 0.0]), ([0.0, 10.0, 0.0], [0.0, 1.0])];
    for (i, (p, uv)) in corners.iter().enumerate() {
        out.extend(f(p));
        out.extend(f(&[0.0, 0.0, -1.0]));
        out.extend(f(uv));
        if i < 2 {
            out.push(0); // BDEF1
            out.extend(0i16.to_le_bytes());
        } else {
            out.push(3); // SDEF: bones 0 and 1, a quarter on bone 0
            out.extend(0i16.to_le_bytes());
            out.extend(1i16.to_le_bytes());
            out.extend(f(&[0.25]));
            out.extend(f(&[0.0; 9]));
        }
        out.extend(f(&[1.0]));
    }
    out.extend(3i32.to_le_bytes());
    out.extend([0u8, 1, 2]);
    out.extend(1i32.to_le_bytes());
    out.extend(text("tex\\skin.png"));
    // One material.
    out.extend(1i32.to_le_bytes());
    out.extend(text("肌"));
    out.extend(text("skin"));
    out.extend(f(&[1.0, 0.5, 0.0, 1.0, 0.1, 0.1, 0.1, 50.0, 0.5, 0.25, 0.0]));
    out.push(0x11); // double-sided, outline
    out.extend(f(&[0.0, 0.0, 0.0, 1.0, 1.0]));
    out.extend([0u8, 0xFF, 0, 1, 3]); // texture 0, no sphere, shared toon 3
    out.extend(text(""));
    out.extend(3i32.to_le_bytes());
    // Two bones; the child names its parent and has an IK block.
    out.extend(2i32.to_le_bytes());
    out.extend(text("センター"));
    out.extend(text("center"));
    out.extend(f(&[0.0, 0.0, 0.0]));
    out.extend((-1i16).to_le_bytes());
    out.extend(0i32.to_le_bytes());
    out.extend(0u16.to_le_bytes());
    out.extend(f(&[0.0, 1.0, 0.0]));
    out.extend(text("頭"));
    out.extend(text("head"));
    out.extend(f(&[0.0, 10.0, 0.0]));
    out.extend(0i16.to_le_bytes());
    out.extend(0i32.to_le_bytes());
    out.extend((0x0001u16 | 0x0020).to_le_bytes());
    out.extend((-1i16).to_le_bytes());
    out.extend(0i16.to_le_bytes());
    out.extend(10i32.to_le_bytes());
    out.extend(f(&[1.0]));
    out.extend(1i32.to_le_bytes());
    out.extend(0i16.to_le_bytes());
    out.push(1);
    out.extend(f(&[0.0; 6]));
    // Morphs: vertex, group, bone.
    out.extend(3i32.to_le_bytes());
    out.extend(text("あ"));
    out.extend(text("a"));
    out.extend([1, 1]);
    out.extend(1i32.to_le_bytes());
    out.push(2);
    out.extend(f(&[0.0, 1.0, 0.0]));
    out.extend(text("笑い"));
    out.extend(text("smile"));
    out.extend([1, 0]);
    out.extend(1i32.to_le_bytes());
    out.push(0);
    out.extend(f(&[0.5]));
    out.extend(text("ボーン"));
    out.extend(text("bone"));
    out.extend([1, 2]);
    out.extend(1i32.to_le_bytes());
    out.extend(1i16.to_le_bytes());
    out.extend(f(&[0.0; 7]));
    out
}

/// The same triangle as PMD.
pub fn pmd() -> Vec<u8> {
    let sjis = |s: &str, n: usize| {
        let (b, _, _) = encoding_rs::SHIFT_JIS.encode(s);
        let mut v = b.into_owned();
        v.resize(n, 0);
        v
    };
    let f = |v: &[f32]| v.iter().flat_map(|x| x.to_le_bytes()).collect::<Vec<u8>>();
    let mut out = b"Pmd".to_vec();
    out.extend(1.0f32.to_le_bytes());
    out.extend(sjis("テスト", 20));
    out.extend(sjis("comment", 256));
    out.extend(3u32.to_le_bytes());
    let corners = [([0.0, 0.0, 0.0], 100u8), ([10.0, 0.0, 0.0], 100), ([0.0, 10.0, 0.0], 25)];
    for (p, w) in corners {
        out.extend(f(&p));
        out.extend(f(&[0.0, 0.0, -1.0, 0.0, 0.0]));
        out.extend(0u16.to_le_bytes());
        out.extend(1u16.to_le_bytes());
        out.extend([w, 0]);
    }
    out.extend(3u32.to_le_bytes());
    for i in [0u16, 1, 2] {
        out.extend(i.to_le_bytes());
    }
    out.extend(1u32.to_le_bytes());
    out.extend(f(&[1.0, 0.5, 0.0, 0.5, 5.0, 0.0, 0.0, 0.0, 0.5, 0.25, 0.0]));
    out.extend([0xFF, 1]);
    out.extend(3u32.to_le_bytes());
    out.extend(sjis("skin.bmp*shine.spa", 20));
    out.extend(2u16.to_le_bytes());
    for (name, parent, y) in [("センター", 0xFFFFu16, 0.0f32), ("頭", 0, 10.0)] {
        out.extend(sjis(name, 20));
        out.extend(parent.to_le_bytes());
        out.extend(0u16.to_le_bytes());
        out.push(0);
        out.extend(0u16.to_le_bytes());
        out.extend(f(&[0.0, y, 0.0]));
    }
    out.extend(0u16.to_le_bytes());
    // Morphs: the base lists vertex 2; "あ" moves it up by 1.
    out.extend(2u16.to_le_bytes());
    out.extend(sjis("base", 20));
    out.extend(1u32.to_le_bytes());
    out.push(0);
    out.extend(2u32.to_le_bytes());
    out.extend(f(&[0.0, 10.0, 0.0]));
    out.extend(sjis("あ", 20));
    out.extend(1u32.to_le_bytes());
    out.push(3);
    out.extend(0u32.to_le_bytes());
    out.extend(f(&[0.0, 1.0, 0.0]));
    out
}

fn no_neighbours() -> Limits {
    Limits { neighbours: false, ..Limits::viewer() }
}

#[test]
fn pmx_triangle() {
    for utf8 in [false, true] {
        let scene = load_pmx(Path::new("t.pmx"), &pmx(utf8), &no_neighbours()).unwrap();
        assert_eq!(scene.info.format, "MikuMikuDance PMX 2.0");
        assert_eq!(scene.meshes[0].name, "テスト");
        let p = &scene.meshes[0].primitives[0];
        // 10 units are 80 cm; MMD's -Z normal (towards the front) becomes
        // +Z; the mirror swaps two corners.
        assert!((p.positions[1] - Vec3::new(0.8, 0.0, 0.0)).length() < 1e-6);
        assert_eq!(p.indices, vec![0, 2, 1]);
        assert!((p.normals[0] - Vec3::Z).length() < 1e-6);
        // The SDEF corner: three quarters on the head, a quarter on the root.
        assert_eq!(p.joints[2][..2], [1, 0]);
        assert!((p.weights[2].x - 0.75).abs() < 1e-6);
        // Morphs: "あ" and the group "笑い" (half of it); the bone morph is
        // counted only.
        assert_eq!(scene.meshes[0].target_names, vec!["あ", "笑い"]);
        assert!((p.targets[0].positions[2] - Vec3::new(0.0, 0.08, 0.0)).length() < 1e-6);
        assert!((p.targets[1].positions[2] - Vec3::new(0.0, 0.04, 0.0)).length() < 1e-6);
        assert!(scene.info.details.iter().any(|(k, v)| k == "Morphs not shown" && v.starts_with('1')));
        let m = &scene.materials[0];
        assert!(m.double_sided);
        assert_eq!(m.name, "肌");
        assert!((m.base_color.y - linear(0.5)).abs() < 1e-6);
        // The head is 80 cm above the root, unrotated.
        assert!((scene.nodes[1].transform.translation - Vec3::new(0.0, 0.8, 0.0)).length() < 1e-6);
        assert_eq!(scene.nodes[1].transform.rotation, Quat::IDENTITY);
        assert!(scene.info.warnings.iter().any(|w| w.contains("SDEF")));
    }
}

#[test]
fn pmd_triangle() {
    let scene = load_pmd(Path::new("t.pmd"), &pmd(), &no_neighbours()).unwrap();
    assert_eq!(scene.info.format, "MikuMikuDance PMD 1.0");
    let p = &scene.meshes[0].primitives[0];
    assert!((p.positions[2] - Vec3::new(0.0, 0.8, 0.0)).length() < 1e-6);
    assert_eq!(p.joints[2][..2], [1, 0]);
    assert!((p.weights[2].x - 0.75).abs() < 1e-6);
    assert_eq!(scene.meshes[0].target_names, vec!["あ"]);
    assert!((p.targets[0].positions[2] - Vec3::new(0.0, 0.08, 0.0)).length() < 1e-6);
    // Translucent PMD materials are drawn from both sides and blended.
    assert!(scene.materials[0].double_sided);
    assert_eq!(scene.materials[0].alpha_mode, AlphaMode::Blend);
    assert_eq!(scene.nodes[1].name, "頭");
}

#[test]
fn motions_beside_a_model_play_on_it() {
    let dir = std::env::temp_dir().join(format!("slerp-rigged-vmd-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let model = dir.join("model.pmx");
    std::fs::write(&model, pmx(false)).unwrap();
    let motion = write_vmd(
        "テスト",
        &[
            ("頭", 0, Vec3::ZERO, Quat::IDENTITY, LINEAR),
            ("頭", 30, Vec3::new(0.0, 0.0, 10.0), Quat::from_rotation_x(0.5), LINEAR),
            ("ないボーン", 0, Vec3::ZERO, Quat::IDENTITY, LINEAR),
        ],
        &[("笑い", 0, 0.0), ("笑い", 30, 1.0), ("ない", 0, 1.0)],
    );
    std::fs::write(dir.join("wave.vmd"), motion).unwrap();
    std::fs::write(dir.join("camera.vmd"), write_vmd("カメラ・照明", &[], &[])).unwrap();
    let loaded = crate::load(crate::RiggedFormat::Pmx, &model, &std::fs::read(&model).unwrap(), &Limits::viewer());
    let Ok(slerp_scene::Asset::Scene(scene)) = loaded else { panic!("not a scene") };
    assert_eq!(scene.animations.len(), 1);
    let clip = &scene.animations[0];
    assert_eq!(clip.name, "wave");
    assert!(scene.info.neighbours.iter().any(|p| p.ends_with("wave.vmd")));
    // The head's translation: its rest place plus the key's offset.
    let t = clip.channels.iter().find(|c| c.node == 1 && c.property == Property::Translation).unwrap();
    assert!((Vec3::from_slice(&t.values[3..6]) - Vec3::new(0.0, 0.8, -0.8)).length() < 1e-6);
    // The morph channel: one value per target, "笑い" second.
    let w = clip.channels.iter().find(|c| c.property == Property::Weights).unwrap();
    assert_eq!(w.node, 2);
    assert_eq!(w.values, vec![0.0, 0.0, 0.0, 1.0]);
    // Thumbnails read no motions.
    let thumb = load_pmx(&model, &std::fs::read(&model).unwrap(), &Limits::thumbnail()).unwrap();
    assert!(thumb.animations.is_empty());
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn damaged_models_fail_cleanly() {
    for good in [pmx(false), pmx(true), pmd()] {
        for len in 0..good.len() {
            let r = std::panic::catch_unwind(|| {
                let _ = load_pmx(Path::new("t.pmx"), &good[..len], &no_neighbours());
                let _ = load_pmd(Path::new("t.pmd"), &good[..len], &no_neighbours());
            });
            assert!(r.is_ok(), "panicked at length {len}");
        }
        // Every byte flipped in turn.
        for i in 0..good.len() {
            let mut bad = good.clone();
            bad[i] ^= 0xFF;
            let r = std::panic::catch_unwind(|| {
                let _ = load_pmx(Path::new("t.pmx"), &bad, &no_neighbours());
                let _ = load_pmd(Path::new("t.pmd"), &bad, &no_neighbours());
            });
            assert!(r.is_ok(), "panicked with byte {i} flipped");
        }
    }
}
