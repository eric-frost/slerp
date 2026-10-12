//! The writer's tests: scenes built in code, written, read back with
//! Slerp's own loader and compared; repairs of broken data; selections and
//! placement.  `tests/write_samples.rs` runs the sample files through the
//! Khronos validator.

use super::*;
use slerp_scene::glam::{Mat4, Quat, Vec2, Vec3, Vec4};
use slerp_scene::textures::{Original, Texture};
use slerp_scene::{
    Aabb, AlphaMode, Animation, Asset, Channel, Effect, Interpolation, Limits, Marker, Material, Mesh, MorphTarget,
    Node, Pose, Primitive, Property, Sampler, Skin, Splats, TexRef, Topology, UvTransform, Wrap,
};
use std::path::Path;
use std::sync::Arc;

/*┌─────────────────╮
| Helpers         |
└─────────────────╯*/
//region Helpers

fn reload(glb: &Glb) -> Scene {
    match crate::load(Path::new("/nonexistent/written.glb"), &glb.bytes, &Limits::viewer()) {
        Ok(Asset::Scene(s)) => s,
        Ok(_) => panic!("not a scene"),
        Err(e) => panic!("the written file doesn't load: {e}"),
    }
}

fn write(scene: &Scene, what: &Selection) -> Glb {
    write_glb(scene, what, &GlbOptions::default()).expect("written")
}

/// The JSON chunk of a GLB.
fn json_of(glb: &Glb) -> serde_json::Value {
    let len = u32::from_le_bytes(glb.bytes[12..16].try_into().unwrap()) as usize;
    serde_json::from_slice(&glb.bytes[20..20 + len]).unwrap()
}

/// A quad in the XY plane, 2 by 1, with every attribute.
fn quad(material: Option<usize>) -> Primitive {
    Primitive {
        positions: vec![
            Vec3::new(-1.0, 0.0, 0.0),
            Vec3::new(1.0, 0.0, 0.0),
            Vec3::new(1.0, 1.0, 0.0),
            Vec3::new(-1.0, 1.0, 0.0),
        ],
        normals: vec![Vec3::Z; 4],
        tangents: vec![Vec4::new(1.0, 0.0, 0.0, -1.0); 4],
        uv0: vec![Vec2::new(0.0, 1.0), Vec2::new(1.0, 1.0), Vec2::new(1.0, 0.0), Vec2::new(0.0, 0.0)],
        uv1: vec![Vec2::splat(0.25); 4],
        colors: vec![Vec4::new(1.0, 0.5, 0.25, 1.0); 4],
        indices: vec![0, 1, 2, 0, 2, 3],
        material,
        ..Default::default()
    }
}

fn texture(width: u32, height: u32) -> Arc<Texture> {
    let pixels = (0..width * height).flat_map(|i| [(i * 40) as u8, 128, 255 - (i * 20) as u8, 255]).collect();
    Arc::new(Texture::from_rgba8(width, height, pixels, slerp_scene::ColorSpace::Unspecified))
}

/// A material with every field the writer maps away from its default.
fn rich_material() -> Material {
    let mut transformed = TexRef::new(0);
    transformed.transform =
        Some(UvTransform { offset: Vec2::new(0.5, 0.25), rotation: 0.5, scale: Vec2::new(2.0, 3.0) });
    transformed.sampler = Sampler { wrap_u: Wrap::ClampToEdge, wrap_v: Wrap::MirroredRepeat, ..Sampler::default() };
    let mut second_set = TexRef::new(0);
    second_set.uv_set = 1;
    Material {
        name: "rich".into(),
        base_color: Vec4::new(0.5, 0.25, 0.125, 0.75),
        base_color_texture: Some(transformed),
        metallic: 0.25,
        roughness: 0.75,
        metallic_roughness_texture: Some(TexRef::new(0)),
        normal_texture: Some(TexRef::new(0)),
        normal_scale: 0.5,
        occlusion_texture: Some(second_set),
        occlusion_strength: 0.5,
        emissive: Vec3::new(4.0, 2.0, 0.0),
        emissive_texture: Some(TexRef::new(0)),
        alpha_mode: AlphaMode::Mask(0.25),
        double_sided: true,
        specular: 0.5,
        specular_color: Vec3::new(1.0, 0.5, 1.0),
        ior: 1.25,
        clearcoat: 0.5,
        clearcoat_roughness: 0.25,
        transmission: 0.5,
        thickness: 0.125,
        attenuation_color: Vec3::new(1.0, 0.5, 0.5),
        attenuation_distance: 2.0,
        ..Material::default()
    }
}

/// A scene with a hierarchy, a skinned and morphed mesh, a static mesh,
/// a textured material and a clip on every property.
fn rich_scene() -> Scene {
    let mut scene = Scene { name: "rich".into(), ..Default::default() };
    scene.textures.push(texture(4, 4));
    scene.materials.push(rich_material());
    let mut skinned = quad(Some(0));
    skinned.joints = vec![[0, 1, 0, 0]; 4];
    skinned.weights = vec![Vec4::new(0.5, 0.5, 0.0, 0.0); 4];
    skinned.targets = vec![
        MorphTarget { positions: vec![Vec3::Y; 4], normals: vec![Vec3::ZERO; 4] },
        MorphTarget { positions: vec![Vec3::X; 4], normals: Vec::new() },
    ];
    scene.meshes.push(Mesh {
        name: "skinned".into(),
        primitives: vec![skinned],
        target_names: vec!["up".into(), "right".into()],
    });
    scene.meshes.push(Mesh { name: "plain".into(), primitives: vec![quad(None)], target_names: Vec::new() });

    // 0 root (moved up 1) > 1 hip > 2 knee; 3 body (skinned mesh 0); 4 prop (mesh 1) under the knee.
    let mut root = Node::new("root");
    root.transform.translation = Vec3::new(0.0, 1.0, 0.0);
    root.children = vec![1, 3];
    let mut hip = Node::new("hip");
    hip.children = vec![2];
    let mut knee = Node::new("knee");
    knee.transform.translation = Vec3::new(0.0, -0.5, 0.0);
    knee.children = vec![4];
    let mut body = Node::new("body");
    body.mesh = Some(0);
    body.skin = Some(0);
    body.weights = vec![0.25, 0.0];
    let mut prop = Node::new("prop");
    prop.mesh = Some(1);
    prop.transform.translation = Vec3::new(3.0, 0.0, 0.0);
    scene.nodes = vec![root, hip, knee, body, prop];
    scene.roots = vec![0];
    scene.skins.push(Skin {
        name: "skin".into(),
        joints: vec![1, 2],
        inverse_bind: vec![
            Mat4::from_translation(Vec3::new(0.0, -1.0, 0.0)),
            Mat4::from_translation(Vec3::new(0.0, -0.5, 0.0)),
        ],
        skeleton: Some(1),
    });
    let quarter = Quat::from_rotation_z(0.5);
    scene.animations.push(Animation {
        name: "wave".into(),
        duration: 2.0,
        channels: vec![
            Channel {
                node: 2,
                property: Property::Rotation,
                interpolation: Interpolation::Linear,
                times: vec![0.0, 1.0, 2.0],
                values: [Quat::IDENTITY, quarter, Quat::IDENTITY].iter().flat_map(|q| q.to_array()).collect(),
            },
            Channel {
                node: 1,
                property: Property::Translation,
                interpolation: Interpolation::Step,
                times: vec![0.0, 1.0],
                values: vec![0.0, 0.0, 0.0, 0.0, 0.5, 0.0],
            },
            Channel {
                node: 4,
                property: Property::Scale,
                interpolation: Interpolation::CubicSpline,
                times: vec![0.0, 2.0],
                values: [[0.0; 3], [1.0; 3], [0.0; 3], [0.0; 3], [2.0; 3], [0.0; 3]].concat(),
            },
            Channel {
                node: 3,
                property: Property::Weights,
                interpolation: Interpolation::Linear,
                times: vec![0.0, 1.0],
                values: vec![0.0, 0.0, 1.0, 0.5],
            },
        ],
    });
    scene
}

fn bounds(scene: &Scene) -> Aabb {
    scene.bounds(&Pose::rest(scene))
}

fn close(a: Vec3, b: Vec3) -> bool {
    (a - b).abs().max_element() < 1e-4
}

//endregion

/*┌─────────────────╮
| Round trips     |
└─────────────────╯*/
//region Round trips

#[test]
fn a_rich_scene_survives_a_round_trip() {
    let scene = rich_scene();
    let glb = write(&scene, &Selection::Scene);
    assert!(glb.warnings.is_empty(), "{:?}", glb.warnings);
    let back = reload(&glb);

    // 1. Counts and the drawn result.
    assert_eq!(back.meshes.len(), 2);
    assert_eq!(back.triangle_count(), scene.triangle_count());
    assert_eq!(back.skins.len(), 1);
    assert_eq!(back.skins[0].joints.len(), 2);
    assert_eq!(back.animations.len(), 1);
    assert_eq!(back.animations[0].channels.len(), 4);
    assert!((back.animations[0].duration - 2.0).abs() < 1e-6);
    let (a, b) = (bounds(&scene), bounds(&back));
    assert!(close(a.min, b.min) && close(a.max, b.max), "{a:?} vs {b:?}");

    // 2. Every attribute and the morph targets with their names.
    let skinned = back.meshes.iter().find(|m| m.name == "skinned").expect("skinned mesh");
    let p = &skinned.primitives[0];
    assert_eq!((p.normals.len(), p.tangents.len(), p.uv0.len(), p.uv1.len(), p.colors.len()), (4, 4, 4, 4, 4));
    assert_eq!(p.tangents[0].w, -1.0);
    assert_eq!(p.joints.len(), 4);
    assert!((p.weights[0] - Vec4::new(0.5, 0.5, 0.0, 0.0)).abs().max_element() < 1e-6);
    assert_eq!(skinned.target_names, ["up", "right"]);
    assert_eq!(p.targets.len(), 2);
    assert_eq!(p.targets[1].positions[0], Vec3::X);
    let body = back.nodes.iter().find(|n| n.mesh.is_some() && n.skin.is_some()).expect("skinned node");
    assert_eq!(body.weights, [0.25, 0.0]);

    // 3. The material, field by field.
    let m = back.materials.iter().find(|m| m.name == "rich").expect("material");
    let want = rich_material();
    assert_eq!(m.base_color, want.base_color);
    assert_eq!((m.metallic, m.roughness, m.normal_scale, m.occlusion_strength), (0.25, 0.75, 0.5, 0.5));
    assert!(close(m.emissive, want.emissive), "{:?}", m.emissive);
    assert_eq!(m.alpha_mode, AlphaMode::Mask(0.25));
    assert!(m.double_sided);
    assert_eq!((m.specular, m.specular_color, m.ior), (0.5, want.specular_color, 1.25));
    assert_eq!((m.clearcoat, m.clearcoat_roughness, m.transmission, m.thickness), (0.5, 0.25, 0.5, 0.125));
    assert_eq!((m.attenuation_color, m.attenuation_distance), (want.attenuation_color, 2.0));
    let base = m.base_color_texture.expect("base colour texture");
    assert_eq!(base.transform, want.base_color_texture.unwrap().transform);
    assert_eq!(base.sampler, want.base_color_texture.unwrap().sampler);
    assert_eq!(m.occlusion_texture.expect("occlusion").uv_set, 1);
    let grey = back.materials.iter().find(|m| m.name == "default").expect("Slerp's grey default");
    assert_eq!(grey.base_color, Material::default().base_color);

    // 4. The picture, decoded again.
    assert_eq!(back.textures.len(), 1);
    assert_eq!(back.textures[0].base().texel_count(), 16);
}

#[test]
fn clips_keep_their_interpolation_and_values() {
    let scene = rich_scene();
    let back = reload(&write(&scene, &Selection::Scene));
    let clip = &back.animations[0];
    let by = |p: Property| clip.channels.iter().find(|c| c.property == p).expect("channel");
    assert_eq!(by(Property::Translation).interpolation, Interpolation::Step);
    assert_eq!(by(Property::Scale).interpolation, Interpolation::CubicSpline);
    assert_eq!(by(Property::Scale).values.len(), 18);
    assert_eq!(by(Property::Weights).values, [0.0, 0.0, 1.0, 0.5]);
    let rotation = by(Property::Rotation);
    assert_eq!(rotation.times, [0.0, 1.0, 2.0]);
    let q = Quat::from_slice(&rotation.values[4..8]);
    assert!(q.abs_diff_eq(Quat::from_rotation_z(0.5), 1e-6));
}

#[test]
fn originals_are_embedded_unchanged() {
    // A real PNG, as a loader would keep it.
    let mut png = Vec::new();
    {
        let mut e = png::Encoder::new(&mut png, 2, 1);
        e.set_color(png::ColorType::Rgba);
        let mut w = e.write_header().unwrap();
        w.write_image_data(&[255, 0, 0, 255, 0, 255, 0, 128]).unwrap();
    }
    let mut tex = Texture::from_rgba8(2, 1, vec![255, 0, 0, 255, 0, 255, 0, 128], slerp_scene::ColorSpace::Unspecified);
    tex.source.name = "kept.png".into();
    tex.source.original = Some(Original::Memory(png.as_slice().into()));
    let mut scene = Scene::default();
    scene.textures.push(Arc::new(tex));
    scene.materials.push(Material { base_color_texture: Some(TexRef::new(0)), ..Material::default() });
    scene.add_mesh_node(Mesh { primitives: vec![quad(Some(0))], ..Default::default() });
    let glb = write(&scene, &Selection::Scene);
    assert!(glb.bytes.windows(png.len()).any(|w| w == png.as_slice()), "the original PNG is in the file");
    assert_eq!(json_of(&glb)["images"][0]["name"], "kept.png");
    let back = reload(&glb);
    assert_eq!(back.textures[0].source.name, "kept.png");

    // Without originals, the same picture is encoded again.
    let options = GlbOptions { original_images: false, ..GlbOptions::default() };
    let again = write_glb(&scene, &Selection::Scene, &options).unwrap();
    assert!(!again.bytes.windows(png.len()).any(|w| w == png.as_slice()));
    assert_eq!(reload(&again).textures[0].base().texel_count(), 2);
}

#[test]
fn floating_point_pictures_become_eight_bit() {
    let mut scene = Scene::default();
    scene.textures.push(Arc::new(Texture::from_rgba32f(2, 1, vec![0.5, 2.0, 0.0, 1.0, 0.0, 0.0, 0.25, 0.5])));
    scene.materials.push(Material { base_color_texture: Some(TexRef::new(0)), ..Material::default() });
    scene.add_mesh_node(Mesh { primitives: vec![quad(Some(0))], ..Default::default() });
    let glb = write(&scene, &Selection::Scene);
    assert!(glb.warnings.iter().any(|w| w.contains("8-bit")), "{:?}", glb.warnings);
    let back = reload(&glb);
    let slerp_scene::textures::Pixels::Rgba8(px) = back.textures[0].base() else { panic!("8-bit") };
    // Linear 0.5 in sRGB is 188; 2.0 clips to 255; alpha 0.5 is 128.
    assert_eq!(&px[..4], &[188, 255, 0, 255]);
    assert_eq!(px[7], 128);
}

//endregion

/*┌─────────────────╮
| Repairs         |
└─────────────────╯*/
//region Repairs

#[test]
fn broken_geometry_is_repaired_or_left_out() {
    let mut scene = Scene::default();
    let mut bad = quad(None);
    bad.positions[1] = Vec3::NAN;
    bad.normals[2] = Vec3::ZERO;
    bad.tangents[0] = Vec4::new(f32::NAN, 0.0, 0.0, 0.0);
    bad.uv1.truncate(2);
    bad.indices.extend([0, 1, 99]);
    let empty = Primitive::default();
    let lines = Primitive {
        positions: vec![Vec3::ZERO, Vec3::X],
        indices: vec![0, 1, 1],
        topology: Topology::Lines,
        ..Default::default()
    };
    scene.add_mesh_node(Mesh { name: "broken".into(), primitives: vec![bad, empty, lines], ..Default::default() });
    scene.add_mesh_node(Mesh { name: "nothing".into(), primitives: vec![Primitive::default()], ..Default::default() });
    let glb = write(&scene, &Selection::Scene);
    assert!(glb.warnings.len() >= 3, "{:?}", glb.warnings);
    let back = reload(&glb);
    assert_eq!(back.meshes.len(), 1, "a mesh with nothing to draw is left out");
    let prims = &back.meshes[0].primitives;
    assert_eq!(prims.len(), 2);
    assert_eq!(prims[0].indices.len(), 6);
    assert!(prims[0].positions.iter().all(|p| p.is_finite()));
    assert!(prims[0].normals.iter().all(|n| (n.length() - 1.0).abs() < 1e-5));
    assert!(prims[0].uv1.is_empty());
    assert_eq!(prims[1].topology, Topology::Lines);
    assert_eq!(prims[1].indices, [0, 1]);
    assert_eq!(back.nodes.len(), 2, "the empty mesh's node stays");
}

#[test]
fn skin_weights_and_joints_are_made_valid() {
    let mut scene = rich_scene();
    let p = &mut scene.meshes[0].primitives[0];
    p.joints = vec![[1, 1, 0, 0], [0, 7, 0, 0], [0, 0, 0, 0], [1, 0, 0, 0]];
    p.weights = vec![
        Vec4::new(0.25, 0.25, 0.0, 0.0),
        Vec4::new(1.0, 1.0, 0.0, 0.0),
        Vec4::ZERO,
        Vec4::new(f32::NAN, -1.0, 0.0, 0.0),
    ];
    // A joint named twice, and one that doesn't exist.
    scene.skins[0].joints = vec![1, 2, 2, 999];
    scene.skins[0].inverse_bind = vec![Mat4::IDENTITY, Mat4::from_cols_array(&[f32::NAN; 16])];
    let back = reload(&write(&scene, &Selection::Scene));
    let p = &back.meshes.iter().find(|m| m.name == "skinned").unwrap().primitives[0];
    for (j, w) in p.joints.iter().zip(&p.weights) {
        assert!((w.x + w.y + w.z + w.w - 1.0).abs() < 1e-6, "{w:?}");
        for k in 0..4 {
            assert!(w[k] > 0.0 || j[k] == 0, "joint {j:?} with weight {w:?}");
        }
    }
    assert_eq!(p.joints[0], [1, 0, 0, 0], "a joint named twice is merged");
    assert_eq!(p.weights[0], Vec4::X);
    assert_eq!(back.skins[0].joints.len(), 4, "every joint has a node of its own");
    let mut unique = back.skins[0].joints.clone();
    unique.sort_unstable();
    unique.dedup();
    assert_eq!(unique.len(), 4);
}

#[test]
fn joints_get_a_common_root() {
    // Two root bones, as MikuMikuDance models have.
    let mut scene = rich_scene();
    scene.nodes[0].children = vec![3];
    scene.nodes[1].children.clear();
    scene.roots = vec![0, 1, 2];
    let back = reload(&write(&scene, &Selection::Scene));
    let parents = back.parents();
    let top = |mut x: usize| {
        while let Some(p) = parents[x] {
            x = p;
        }
        x
    };
    let joints = &back.skins[0].joints;
    assert!(joints.iter().all(|&j| top(j) == top(joints[0])));
    let (a, b) = (bounds(&scene), bounds(&back));
    assert!(close(a.min, b.min) && close(a.max, b.max), "{a:?} vs {b:?}");
}

#[test]
fn keyframes_are_made_valid() {
    let mut scene = rich_scene();
    let clip = &mut scene.animations[0];
    clip.channels[0].times = vec![0.0, 0.0, -1.0];
    clip.channels[2].times = vec![1.0];
    clip.channels[2].values.truncate(9);
    clip.channels[1].values = vec![f32::NAN; 6];
    clip.channels.push(clip.channels[1].clone());
    let glb = write(&scene, &Selection::Scene);
    let back = reload(&glb);
    let clip = &back.animations[0];
    let rotation = clip.channels.iter().find(|c| c.property == Property::Rotation).unwrap();
    assert_eq!(rotation.times, [0.0]);
    let scale = clip.channels.iter().find(|c| c.property == Property::Scale).unwrap();
    assert_eq!(scale.interpolation, Interpolation::Linear, "a one-key spline holds still");
    assert_eq!(scale.values, [1.0, 1.0, 1.0]);
    assert_eq!(clip.channels.iter().filter(|c| c.property == Property::Translation).count(), 1);
    assert!(glb.warnings.iter().any(|w| w.contains("keyframes")), "{:?}", glb.warnings);
}

#[test]
fn odd_scenes_never_fail() {
    // Cycles, children past the end, roots past the end, NaN transforms.
    let mut scene = Scene::default();
    let mut a = Node::new("a");
    a.children = vec![1, 7];
    a.transform.translation = Vec3::NAN;
    let mut b = Node::new("b");
    b.children = vec![0];
    b.mesh = Some(5);
    b.skin = Some(3);
    scene.nodes = vec![a, b];
    scene.roots = vec![0, 9];
    scene.animations.push(Animation {
        name: "x".into(),
        duration: 1.0,
        channels: vec![Channel {
            node: 40,
            property: Property::Weights,
            interpolation: Interpolation::Linear,
            times: vec![0.0],
            values: vec![1.0],
        }],
    });
    for what in [
        Selection::Scene,
        Selection::Node(1),
        Selection::Nodes(vec![1, 0, 1]),
        Selection::Animation { index: 0, skeleton_only: false },
        Selection::Animation { index: 0, skeleton_only: true },
    ] {
        for placement in [Placement::InPlace, Placement::Centred] {
            let glb = write_glb(&scene, &what, &GlbOptions { placement, ..GlbOptions::default() }).unwrap();
            reload(&glb);
        }
    }
    let empty = write(&Scene::default(), &Selection::Scene);
    assert!(reload(&empty).nodes.is_empty());
    assert!(matches!(write_glb(&scene, &Selection::Mesh(0), &GlbOptions::default()), Err(WriteError::Selection(_))));
    assert!(matches!(write_glb(&scene, &Selection::Node(2), &GlbOptions::default()), Err(WriteError::Selection(_))));
}

//endregion

/*┌─────────────────╮
| Selections      |
└─────────────────╯*/
//region Selections

#[test]
fn a_node_keeps_its_place_and_brings_its_skeleton() {
    let scene = rich_scene();
    // The prop, under root (up 1) > hip > knee (down 0.5), right 3.
    // A clip scales it, so its parents become one stand-in node, since
    // the clip's values are relative to them.
    let glb = write(&scene, &Selection::Node(4));
    let back = reload(&glb);
    assert_eq!(back.nodes.len(), 2);
    let prop = back.nodes.iter().position(|n| n.name == "prop").unwrap();
    assert!(close(Pose::rest(&back).world[prop].w_axis.truncate(), Vec3::new(3.0, 0.5, 0.0)));
    assert_eq!(back.animations.len(), 1, "the clip's channel on the prop stays");
    // Without clips its parents fold into it.
    let still = GlbOptions { animations: false, ..GlbOptions::default() };
    let back = reload(&write_glb(&scene, &Selection::Node(4), &still).unwrap());
    assert_eq!(back.nodes.len(), 1);
    assert!(close(back.nodes[0].transform.translation, Vec3::new(3.0, 0.5, 0.0)));

    // The skinned body brings its joints, which keep their world places.
    let back = reload(&write(&scene, &Selection::Node(3)));
    assert_eq!(back.skins.len(), 1);
    let (a, b) = (bounds(&scene), bounds(&back));
    let mut body_only = Aabb::EMPTY;
    let pose = Pose::rest(&scene);
    let joints = pose.joint_matrices(&scene, 0);
    for p in &scene.meshes[0].primitives[0].positions {
        body_only.add((joints[0] * 0.5 + joints[1] * 0.5).transform_point3(*p));
    }
    assert!(close(b.min, body_only.min) && close(b.max, body_only.max), "{b:?} vs {body_only:?} (scene {a:?})");
    assert_eq!(back.animations[0].channels.len(), 3, "the prop's channel is gone");
}

#[test]
fn several_nodes_are_written_side_by_side() {
    let scene = rich_scene();
    let still = GlbOptions { animations: false, ..GlbOptions::default() };
    // The skinned body and the prop, each in its place, with the joints the
    // body needs.
    let back = reload(&write_glb(&scene, &Selection::Nodes(vec![3, 4]), &still).unwrap());
    assert_eq!((back.meshes.len(), back.skins.len()), (2, 1));
    let prop = back.nodes.iter().position(|n| n.name == "prop").unwrap();
    assert!(close(Pose::rest(&back).world[prop].w_axis.truncate(), Vec3::new(3.0, 0.5, 0.0)));
    // A node under another one chosen is written once, under it.
    let back = reload(&write_glb(&scene, &Selection::Nodes(vec![2, 4]), &still).unwrap());
    assert_eq!(back.nodes.iter().filter(|n| n.name == "prop").count(), 1);
    assert_eq!(back.roots.len(), 1);
    for bad in [vec![], vec![4, 9]] {
        assert!(matches!(write_glb(&scene, &Selection::Nodes(bad), &still), Err(WriteError::Selection(_))));
    }
}

#[test]
fn a_mesh_alone_has_no_skin_or_clips() {
    let scene = rich_scene();
    let glb = write(&scene, &Selection::Mesh(0));
    let back = reload(&glb);
    assert_eq!((back.nodes.len(), back.meshes.len(), back.skins.len(), back.animations.len()), (1, 1, 0, 0));
    assert!(back.meshes[0].primitives[0].joints.is_empty());
    assert_eq!(back.meshes[0].target_names, ["up", "right"]);
    // Where its joints put it: the bind matrices undo the joints' places.
    let b = bounds(&back);
    assert!(close(b.min, Vec3::new(-1.0, 0.0, 0.0)) && close(b.max, Vec3::new(1.0, 1.0, 0.0)), "{b:?}");
}

#[test]
fn an_animation_brings_what_it_moves() {
    let scene = rich_scene();
    let back = reload(&write(&scene, &Selection::Animation { index: 0, skeleton_only: false }));
    assert_eq!(back.meshes.len(), 2, "the skinned body and the scaled prop");
    assert_eq!(back.animations[0].channels.len(), 4);

    let back = reload(&write(&scene, &Selection::Animation { index: 0, skeleton_only: true }));
    assert!(back.meshes.is_empty());
    assert!(back.nodes.iter().any(|n| n.name == "knee"));
    assert_eq!(back.animations[0].channels.len(), 3, "no mesh, so no weights");
}

#[test]
fn centring_moves_the_box_to_the_origin() {
    let scene = rich_scene();
    for what in [
        Selection::Scene,
        Selection::Node(4),
        Selection::Mesh(1),
        Selection::Animation { index: 0, skeleton_only: false },
    ] {
        let options = GlbOptions { placement: Placement::Centred, ..GlbOptions::default() };
        let back = reload(&write_glb(&scene, &what, &options).unwrap());
        let c = bounds(&back).center();
        assert!(c.length() < 1e-4, "{what:?}: centre {c:?}");
    }
}

#[test]
fn hidden_nodes_round_trip() {
    let mut scene = rich_scene();
    scene.nodes[4].visible = false;
    // A hidden mesh with a visible child: the child must stay visible.
    scene.nodes[0].mesh = Some(1);
    scene.nodes[0].visible = false;
    let glb = write(&scene, &Selection::Scene);
    assert!(json_of(&glb)["extensionsUsed"].as_array().unwrap().iter().any(|e| e == "KHR_node_visibility"));
    let back = reload(&glb);
    let hidden: Vec<&str> = back.nodes.iter().filter(|n| !n.visible).map(|n| n.name.as_str()).collect();
    assert_eq!(hidden, ["root", "prop"]);
    assert!(back.nodes.iter().filter(|n| n.name == "hip" || n.name == "knee").all(|n| n.visible));

    let without = write_glb(&scene, &Selection::Scene, &GlbOptions { hidden: false, ..GlbOptions::default() }).unwrap();
    let back = reload(&without);
    assert!(back.nodes.iter().all(|n| n.visible));
    assert_eq!(back.meshes.len(), 1, "only the skinned body is left");
}

//endregion

/*┌─────────────────╮
| Approximations  |
└─────────────────╯*/
//region Approximations

#[test]
fn levels_keep_markers_lightmaps_and_sprites() {
    let mut scene = Scene::default();
    scene.textures.push(texture(2, 2));
    let mut lightmap = TexRef::new(0);
    lightmap.uv_set = 1;
    scene.materials.push(Material { lightmap_texture: Some(lightmap), lightmap_scale: 2.0, ..Material::default() });
    scene.materials.push(Material {
        effect: Effect::Billboard,
        base_color_texture: Some(TexRef::new(0)),
        ..Material::default()
    });
    scene.add_mesh_node(Mesh { primitives: vec![quad(Some(0))], ..Default::default() });
    // A sprite: four corners at its anchor, offsets in the second UV set.
    let sprite = Primitive {
        positions: vec![Vec3::new(5.0, 0.0, 5.0); 4],
        uv0: vec![Vec2::ZERO; 4],
        uv1: vec![Vec2::new(-0.5, 0.0), Vec2::new(0.5, 0.0), Vec2::new(0.5, 1.0), Vec2::new(-0.5, 1.0)],
        indices: vec![0, 1, 2, 0, 2, 3],
        material: Some(1),
        ..Default::default()
    };
    scene.add_mesh_node(Mesh { primitives: vec![sprite], ..Default::default() });
    // Doom's sky, a kilometre-high curtain drawn at infinity: left out.
    scene.materials.push(Material { effect: Effect::PanoramaSky, ..Material::default() });
    let mut sky = quad(Some(2));
    sky.positions.iter_mut().for_each(|p| p.y *= 1000.0);
    scene.meshes[1].primitives.push(sky);
    scene.markers.push(Marker { label: "info_player_start".into(), position: Vec3::ONE, color: Vec4::ONE });
    scene.splats = Some(Splats {
        positions: vec![Vec3::ZERO, Vec3::X],
        scales: vec![Vec3::ONE; 2],
        rotations: vec![Quat::IDENTITY; 2],
        colors: vec![Vec4::new(1.0, 0.5, 0.0, 1.0); 2],
        ..Default::default()
    });
    let glb = write(&scene, &Selection::Scene);
    for word in ["occlusion", "crossed", "points", "sky"] {
        assert!(glb.warnings.iter().any(|w| w.contains(word)), "{word}: {:?}", glb.warnings);
    }
    let back = reload(&glb);
    let lit = &back.materials[back.meshes[0].primitives[0].material.unwrap()];
    assert_eq!(lit.occlusion_texture.map(|t| t.uv_set), Some(1));
    assert_eq!(back.meshes[1].primitives.len(), 1, "no sky");
    let sprite = &back.meshes[1].primitives[0];
    assert_eq!((sprite.positions.len(), sprite.indices.len()), (8, 12));
    assert!(back.materials[sprite.material.unwrap()].unlit);
    assert!(back.nodes.iter().any(|n| n.name == "info_player_start" && n.transform.translation == Vec3::ONE));
    let points = back.meshes.iter().flat_map(|m| &m.primitives).find(|p| p.topology == Topology::Points).unwrap();
    assert_eq!(points.positions.len(), 2);
    assert!((points.colors[0].y - 0.214).abs() < 0.01, "sRGB 0.5 is linear 0.214");
}

//endregion
