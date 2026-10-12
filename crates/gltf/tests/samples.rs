//! Loads Khronos sample models, VRM samples and chalk's game files, and
//! checks what comes out against what their JSON says.  Files that aren't
//! on this machine are skipped with a note, so the tests pass without the
//! downloaded corpus.

use slerp_scene::glam::{Vec3, Vec4};
use slerp_scene::{AlphaMode, Asset, Interpolation, Limits, Pose, Property, Scene, Topology, Wrap};
use std::path::{Path, PathBuf};

/// A path in a checkout of chalk (a game whose files some tests read) in the
/// folder beside this repository's; tests skip the files that aren't there.
macro_rules! chalk {
    ($path:literal) => {
        concat!(env!("CARGO_MANIFEST_DIR"), "/../../../chalk/", $path)
    };
}

/// Finds a test file in the shared corpus (`local/assets/gltf/khronos/`,
/// `local/assets/vrm/`), this crate's own downloads
/// (`local/assets/gltf-extra/`), or at an absolute path.
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

fn load_with(rel: &str, limits: &Limits) -> Option<Scene> {
    let path = asset(rel)?;
    let bytes = std::fs::read(&path).unwrap();
    match slerp_gltf::load(&path, &bytes, limits) {
        Ok(Asset::Scene(s)) => Some(s),
        Ok(_) => panic!("{rel}: not a scene"),
        Err(e) => panic!("{rel}: {e}"),
    }
}

fn load(rel: &str) -> Option<Scene> {
    load_with(rel, &Limits::viewer())
}

/// Warnings other than images the textures crate can't decode yet.
fn real_warnings(scene: &Scene) -> Vec<&String> {
    scene.info.warnings.iter().filter(|w| !w.contains("couldn't be decoded")).collect()
}

fn rest_size(scene: &Scene) -> Vec3 {
    scene.bounds(&Pose::rest(scene)).size()
}

fn close(a: Vec3, b: Vec3, tolerance: f32) -> bool {
    (a - b).abs().max_element() <= tolerance
}

/*┌─────────────────╮
| Core glTF       |
└─────────────────╯*/
//region Core glTF

#[test]
fn box_in_every_container() {
    for rel in [
        "Box/glTF-Binary/Box.glb",
        "Box/glTF/Box.gltf",
        "Box/glTF-Embedded/Box.gltf",
        "BoxInterleaved/glTF-Binary/BoxInterleaved.glb",
    ] {
        let Some(s) = load(rel) else { continue };
        assert_eq!((s.triangle_count(), s.vertex_count()), (12, 24), "{rel}");
        assert!(close(rest_size(&s), Vec3::ONE, 1e-5), "{rel}");
        assert!(real_warnings(&s).is_empty(), "{rel}: {:?}", s.info.warnings);
        assert!(s.meshes[0].primitives[0].normals.len() == 24);
    }
    if let Some(s) = load("Box/glTF-Binary/Box.glb") {
        assert_eq!(s.info.format, "glTF 2.0 (binary)");
        assert_eq!(s.info.generator.as_deref(), Some("COLLADA2GLTF"));
    }
}

#[test]
fn external_files_with_escaped_names() {
    for rel in ["Box With Spaces/glTF/Box With Spaces.gltf", "Unicode❤♻Test/glTF/Unicode❤♻Test.gltf"] {
        let Some(s) = load(rel) else { continue };
        assert!(s.triangle_count() > 0, "{rel}");
        assert!(real_warnings(&s).is_empty(), "{rel}: {:?}", s.info.warnings);
    }
}

#[test]
fn neighbours_off_keeps_geometry_from_data_uris_only() {
    let mut limits = Limits::viewer();
    limits.neighbours = false;
    // External .bin: no geometry, but a warning rather than a failure.
    if let Some(s) = load_with("Box/glTF/Box.gltf", &limits) {
        assert_eq!(s.triangle_count(), 0);
        assert!(!s.info.warnings.is_empty());
    }
    if let Some(s) = load_with("Box/glTF-Embedded/Box.gltf", &limits) {
        assert_eq!(s.triangle_count(), 12);
    }
}

#[test]
fn damaged_helmet_and_spheres() {
    if let Some(s) = load("DamagedHelmet/glTF-Binary/DamagedHelmet.glb") {
        assert_eq!(s.triangle_count(), 15452);
        let p = &s.meshes[0].primitives[0];
        assert_eq!((p.normals.len(), p.uv0.len()), (p.positions.len(), p.positions.len()));
        if !s.textures.is_empty() {
            let m = &s.materials[0];
            assert!(m.base_color_texture.is_some() && m.normal_texture.is_some() && m.emissive_texture.is_some());
            assert!(m.occlusion_texture.is_some() && m.metallic_roughness_texture.is_some());
        }
    }
    if let Some(s) = load("MetalRoughSpheres/glTF-Binary/MetalRoughSpheres.glb") {
        assert_eq!(s.triangle_count(), 501776);
    }
}

#[test]
fn sparse_accessor() {
    let Some(s) = load("SimpleSparseAccessor/glTF-Embedded/SimpleSparseAccessor.gltf") else { return };
    let p = &s.meshes[0].primitives[0].positions;
    assert_eq!(p.len(), 14);
    assert_eq!(p[8], Vec3::new(1.0, 2.0, 0.0));
    assert_eq!(p[10], Vec3::new(3.0, 3.0, 0.0));
    assert_eq!(p[12], Vec3::new(5.0, 4.0, 0.0));
    assert_eq!(p[9], Vec3::new(2.0, 1.0, 0.0));
}

#[test]
fn primitive_modes() {
    let Some(s) = load("MeshPrimitiveModes/glTF-Embedded/MeshPrimitiveModes.gltf") else { return };
    let prims: Vec<_> = s.meshes.iter().map(|m| &m.primitives[0]).collect();
    // Points, lines, line loop, line strip, triangles, strip, fan.
    let expected = [
        (Topology::Points, 7),
        (Topology::Lines, 12),
        (Topology::Lines, 14),
        (Topology::Lines, 12),
        (Topology::Triangles, 18),
        (Topology::Triangles, 12),
        (Topology::Triangles, 18),
    ];
    for (p, (topology, count)) in prims.iter().zip(expected) {
        assert_eq!((p.topology, p.indices.len()), (topology, count));
    }
}

#[test]
fn unindexed_and_multiple_scenes() {
    if let Some(s) = load("TriangleWithoutIndices/glTF-Embedded/TriangleWithoutIndices.gltf") {
        assert_eq!(s.meshes[0].primitives[0].indices, vec![0, 1, 2]);
    }
    if let Some(s) = load("MultipleScenes/glTF-Embedded/MultipleScenes.gltf") {
        // The file's default scene is the second, which holds one of its two
        // nodes.
        assert_eq!(s.nodes.len(), 1);
        assert!(s.info.details.iter().any(|(k, _)| k == "Scenes"));
    }
}

#[test]
fn instancing_and_lights() {
    if let Some(s) = load("SimpleInstancing/glTF-Binary/SimpleInstancing.glb") {
        let drawn = s.nodes.iter().filter(|n| n.mesh.is_some()).count();
        assert_eq!(drawn, 125);
    }
    if let Some(s) = load("LightsPunctualLamp/glTF-Binary/LightsPunctualLamp.glb") {
        assert!(s.info.details.iter().any(|(k, v)| k == "Lights" && v.contains("point")));
    }
}

#[test]
fn awkward_hierarchies() {
    for rel in [
        "RecursiveSkeletons/glTF-Binary/RecursiveSkeletons.glb",
        "NegativeScaleTest/glTF-Binary/NegativeScaleTest.glb",
        "NodeVisibilityTest/glTF-Binary/NodeVisibilityTest.glb",
        "Cameras/glTF-Embedded/Cameras.gltf",
    ] {
        let Some(s) = load(rel) else { continue };
        assert!(s.triangle_count() > 0, "{rel}");
        let parents = s.parents();
        assert!(s.roots.iter().all(|&r| parents[r].is_none()), "{rel}");
    }
}

//endregion

/*┌─────────────────╮
| Materials       |
└─────────────────╯*/
//region Materials

#[test]
fn alpha_modes() {
    let Some(s) = load("AlphaBlendModeTest/glTF-Binary/AlphaBlendModeTest.glb") else { return };
    let modes: Vec<AlphaMode> = s.materials.iter().map(|m| m.alpha_mode).collect();
    assert!(modes.contains(&AlphaMode::Blend));
    assert!(modes.contains(&AlphaMode::Opaque));
    assert!(modes.iter().any(|m| matches!(m, AlphaMode::Mask(c) if (*c - 0.25).abs() < 1e-6)));
    assert!(modes.iter().any(|m| matches!(m, AlphaMode::Mask(c) if (*c - 0.5).abs() < 1e-6)));
}

#[test]
fn texture_transforms() {
    let Some(s) = load("TextureTransformTest/glTF/TextureTransformTest.gltf") else { return };
    if s.textures.is_empty() {
        eprintln!("skipped: textures don't decode yet");
        return;
    }
    let tr = |name: &str| s.materials.iter().find(|m| m.name == name).unwrap().base_color_texture.unwrap().transform;
    assert_eq!(tr("Offset U").unwrap().offset.x, 0.5);
    assert!((tr("Rotation").unwrap().rotation - std::f32::consts::FRAC_PI_8).abs() < 1e-6);
    assert_eq!(tr("Scale").unwrap().scale.x, 1.5);
    let all = tr("All").unwrap();
    assert_eq!((all.offset.y, all.rotation, all.scale.y), (-0.1, 0.3, 1.5));
    assert!(tr("Correct").is_none());
}

/// Follows TextureTransformTest's rotation check on the CPU: the arrow
/// texture's tip, seen through the rotated UVs, must land on the quad's
/// "Correct" marker, not its "Error" marker (rotation the wrong way) or
/// "Not Supported" marker (no rotation).
#[test]
fn texture_rotation_points_at_the_correct_marker() {
    use slerp_scene::glam::{Vec2, Vec3Swizzles};
    use slerp_scene::textures::Pixels;
    let Some(s) = load("TextureTransformTest/glTF/TextureTransformTest.gltf") else { return };
    let quad = s.nodes.iter().position(|n| n.name == "Rotation").unwrap();
    let material = &s.materials[s.meshes[s.nodes[quad].mesh.unwrap()].primitives[0].material.unwrap()];
    let Some(tex) = material.base_color_texture else {
        eprintln!("skipped: textures don't decode yet");
        return;
    };
    // 1. The arrow points to the image's lower right, so its tip is the
    // dark texel furthest that way.
    let image = &s.textures[tex.texture];
    let (w, h) = (image.width() as usize, image.height() as usize);
    let Pixels::Rgba8(px) = image.base() else { panic!("expected 8-bit") };
    let tip = (0..w * h)
        .filter(|&i| px[i * 4] < 64 && px[i * 4 + 3] > 128)
        .max_by_key(|&i| i % w + i / w)
        .map(|i| Vec2::new((i % w) as f32 + 0.5, (i / w) as f32 + 0.5) / Vec2::new(w as f32, h as f32))
        .unwrap();
    let p = &s.meshes[s.nodes[quad].mesh.unwrap()].primitives[0];
    let marker_hit = |transform: slerp_scene::UvTransform| -> &str {
        // 2. The quad's UV at which that texel shows.
        let uv = transform.matrix().inverse().transform_point2(tip);
        // 3. Where that UV is on the quad, from the quad's own vertices,
        // which map UV to position affinely.
        let duv = [p.uv0[1] - p.uv0[0], p.uv0[2] - p.uv0[0]];
        let dpos = [p.positions[1] - p.positions[0], p.positions[2] - p.positions[0]];
        let m = slerp_scene::glam::Mat2::from_cols(duv[0], duv[1]).inverse() * (uv - p.uv0[0]);
        let at = p.positions[0] + dpos[0] * m.x + dpos[1] * m.y;
        // 4. The nearest marker.
        let d = |n: usize| (s.nodes[n].transform.translation.xy() - at.xy()).length();
        let nearest = s.nodes[quad].children.iter().min_by(|&&x, &&y| d(x).total_cmp(&d(y))).unwrap();
        &s.nodes[*nearest].name
    };
    let ours = tex.transform.unwrap();
    assert_eq!(marker_hit(ours), "Rotation - Correct");
    // The check itself tells the directions apart.
    assert_eq!(marker_hit(slerp_scene::UvTransform { rotation: -ours.rotation, ..ours }), "Rotation - Error");
    assert_eq!(marker_hit(slerp_scene::UvTransform { rotation: 0.0, ..ours }), "Rotation - Not Supported");
}

#[test]
fn material_extensions() {
    if let Some(s) = load("EmissiveStrengthTest/glTF-Binary/EmissiveStrengthTest.glb") {
        assert!(s.materials.iter().any(|m| m.emissive.max_element() > 1.0));
    }
    if let Some(s) = load("UnlitTest/glTF-Binary/UnlitTest.glb") {
        assert!(s.materials.iter().all(|m| m.unlit));
    }
    if let Some(s) = load("ClearCoatTest/glTF-Binary/ClearCoatTest.glb") {
        assert!(s.materials.iter().any(|m| m.clearcoat > 0.0 && m.clearcoat_roughness > 0.0));
    }
    if let Some(s) = load("SpecularTest/glTF-Binary/SpecularTest.glb") {
        assert!(s.materials.iter().any(|m| m.specular < 1.0));
    }
    if let Some(s) = load("IORTestGrid/glTF-Binary/IORTestGrid.glb") {
        assert!(s.materials.iter().any(|m| m.ior != 1.5));
    }
    if let Some(s) = load("SpecGlossVsMetalRough/glTF-Binary/SpecGlossVsMetalRough.glb") {
        assert!(s.materials.iter().any(|m| m.metallic == 0.0 && m.roughness < 1.0));
    }
}

/// KHR_materials_transmission and KHR_materials_volume: factors, textures,
/// and attenuation, with the defaults where the file leaves them out.
#[test]
fn transmission_and_volume() {
    if let Some(s) = load("DragonAttenuation/glTF-Binary/DragonAttenuation.glb") {
        let m = s.materials.iter().find(|m| m.name == "Dragon with Attenuation").expect("the dragon");
        assert_eq!(m.transmission, 1.0);
        assert!((m.thickness - 2.27).abs() < 1e-6 && m.thickness_texture.is_some());
        assert!((m.attenuation_distance - 0.155).abs() < 1e-6);
        assert!((m.attenuation_color - Vec3::new(0.921, 0.64, 0.064)).length() < 1e-6);
        // The file's second dragon material, which no mesh uses, may be
        // left out; when present it has no attenuation.
        if let Some(plain) = s.materials.iter().find(|m| m.name == "Dragon with Surface Coloring Only") {
            assert_eq!((plain.attenuation_distance, plain.attenuation_color), (f32::INFINITY, Vec3::ONE));
        }
        assert!(s.materials.iter().filter(|m| m.transmission == 0.0).all(|m| m.thickness == 0.0));
    }
    if let Some(s) = load("TransmissionTest/glTF-Binary/TransmissionTest.glb") {
        let textured = s.materials.iter().filter(|m| m.transmission_texture.is_some()).count();
        assert!(textured >= 3, "{textured} materials with a transmission texture");
        assert!(s.materials.iter().all(|m| m.thickness == 0.0), "no volume: thin walls");
    }
}

#[test]
fn samplers() {
    let Some(s) = load("TextureSettingsTest/glTF-Binary/TextureSettingsTest.glb") else { return };
    if s.textures.is_empty() {
        eprintln!("skipped: textures don't decode yet");
        return;
    }
    let wraps: Vec<(Wrap, Wrap)> =
        s.materials.iter().filter_map(|m| m.base_color_texture).map(|t| (t.sampler.wrap_u, t.sampler.wrap_v)).collect();
    for pair in [
        (Wrap::Repeat, Wrap::ClampToEdge),
        (Wrap::ClampToEdge, Wrap::Repeat),
        (Wrap::Repeat, Wrap::MirroredRepeat),
        (Wrap::MirroredRepeat, Wrap::Repeat),
    ] {
        assert!(wraps.contains(&pair), "{pair:?}");
    }
}

#[test]
fn default_material() {
    let Some(s) = load("TriangleWithoutIndices/glTF-Embedded/TriangleWithoutIndices.gltf") else { return };
    let m = &s.materials[s.meshes[0].primitives[0].material.unwrap()];
    assert_eq!((m.metallic, m.roughness, m.base_color.x), (1.0, 1.0, 1.0));
}

//endregion

/*┌─────────────────╮
| Animation       |
└─────────────────╯*/
//region Animation

#[test]
fn morph_targets() {
    if let Some(s) = load("AnimatedMorphCube/glTF-Binary/AnimatedMorphCube.glb") {
        let p = &s.meshes[0].primitives[0];
        assert_eq!(p.targets.len(), 2);
        let ch = &s.animations[0].channels[0];
        assert_eq!(ch.property, Property::Weights);
        assert_eq!(ch.values.len(), ch.times.len() * 2);
        // The quantized copy has the same shape.
        if let Some(q) = load("AnimatedMorphCube/glTF-Quantized/AnimatedMorphCube.gltf") {
            assert_eq!(q.meshes[0].primitives[0].targets.len(), 2);
            assert!(close(rest_size(&q), rest_size(&s), 1e-3));
        }
    }
    if let Some(s) = load("MorphPrimitivesTest/glTF-Binary/MorphPrimitivesTest.glb") {
        assert_eq!(s.meshes[0].primitives.len(), 2);
        assert!(s.meshes[0].primitives.iter().all(|p| p.targets.len() == 1));
    }
    if let Some(s) = load("MorphStressTest/glTF-Binary/MorphStressTest.glb") {
        assert_eq!(s.meshes[0].primitives[0].targets.len(), 8);
        assert_eq!(s.animations.len(), 3);
    }
}

#[test]
fn skins() {
    for (rel, joints, channels) in [
        ("SimpleSkin/glTF-Embedded/SimpleSkin.gltf", 2, 1),
        ("RiggedSimple/glTF-Binary/RiggedSimple.glb", 2, 3),
        ("RiggedFigure/glTF-Binary/RiggedFigure.glb", 19, 57),
        ("CesiumMan/glTF-Binary/CesiumMan.glb", 19, 57),
    ] {
        let Some(s) = load(rel) else { continue };
        assert_eq!(s.skins[0].joints.len(), joints, "{rel}");
        assert_eq!(s.skins[0].inverse_bind.len(), joints, "{rel}");
        assert_eq!(s.animations[0].channels.len(), channels, "{rel}");
        // The clip moves the mesh.
        let mut pose = Pose::rest(&s);
        let rest = s.bounds(&pose);
        pose.sample(&s, 0, s.animations[0].duration * 0.3);
        pose.update_world(&s);
        let moved = s.bounds(&pose);
        assert!(!close(rest.min, moved.min, 1e-4) || !close(rest.max, moved.max, 1e-4), "{rel}");
    }
    if let Some(s) = load("Fox/glTF-Binary/Fox.glb") {
        let names: Vec<&str> = s.animations.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(names, ["Survey", "Walk", "Run"]);
        assert_eq!(s.skins[0].joints.len(), 24);
    }
}

#[test]
fn interpolation_modes() {
    let Some(s) = load("InterpolationTest/glTF-Binary/InterpolationTest.glb") else { return };
    assert_eq!(s.animations.len(), 9);
    for a in &s.animations {
        let ch = &a.channels[0];
        let n = match ch.property {
            Property::Rotation => 4,
            _ => 3,
        };
        let expected = match ch.interpolation {
            Interpolation::CubicSpline => 3 * n * ch.times.len(),
            _ => n * ch.times.len(),
        };
        assert_eq!(ch.values.len(), expected, "{}", a.name);
        let kind = if a.name.starts_with("Step") {
            Interpolation::Step
        } else if a.name.starts_with("Cubic") {
            Interpolation::CubicSpline
        } else {
            Interpolation::Linear
        };
        assert_eq!(ch.interpolation, kind, "{}", a.name);
    }
}

#[test]
fn thumbnail_limits_skip_clips() {
    let Some(s) = load_with("CesiumMan/glTF-Binary/CesiumMan.glb", &Limits::thumbnail()) else { return };
    assert!(s.animations.is_empty());
    assert_eq!(s.skins[0].joints.len(), 19);
}

//endregion

/*┌─────────────────╮
| Compression     |
└─────────────────╯*/
//region Compression

#[test]
fn quantized_matches_plain() {
    let (Some(q), Some(p)) = (load("Duck/glTF-Quantized/Duck.gltf"), load("Duck/glTF-Binary/Duck.glb")) else { return };
    assert_eq!(q.triangle_count(), p.triangle_count());
    assert!(close(rest_size(&q), rest_size(&p), 1e-3), "{:?} vs {:?}", rest_size(&q), rest_size(&p));
    let n = &q.meshes[0].primitives[0].normals;
    assert!(n.iter().all(|v| (v.length() - 1.0).abs() < 1e-4));
    for rel in ["Avocado/glTF-Quantized/Avocado.gltf", "Lantern/glTF-Quantized/Lantern.gltf"] {
        let Some(s) = load(rel) else { continue };
        assert!(s.triangle_count() > 0 && real_warnings(&s).is_empty(), "{rel}: {:?}", s.info.warnings);
    }
}

#[test]
fn meshopt_matches_plain() {
    let plain = load("BrainStem/glTF-Binary/BrainStem.glb");
    for rel in ["BrainStem/glTF-Meshopt/BrainStem.gltf", "BrainStem/glTF-Meshopt-EXT/BrainStem.gltf"] {
        let Some(s) = load(rel) else { continue };
        assert_eq!(s.triangle_count(), 61666, "{rel}");
        assert_eq!(s.skins[0].joints.len(), 18, "{rel}");
        assert!(real_warnings(&s).is_empty(), "{rel}: {:?}", s.info.warnings);
        if let Some(p) = &plain {
            assert!(close(rest_size(&s), rest_size(p), 1e-3), "{rel}");
        }
    }
    if let Some(s) = load("MeshoptCubeTest/glTF-Meshopt/MeshoptCubeTest.gltf") {
        assert_eq!(s.meshes.len(), 35);
        assert!(real_warnings(&s).is_empty(), "{:?}", s.info.warnings);
        // Every cube is a unit-ish cube whatever filter its data used.
        for (i, m) in s.meshes.iter().enumerate() {
            let p = &m.primitives[0];
            assert!(p.positions.iter().all(|v| v.abs().max_element() <= 1.01), "mesh {i}");
            assert!(p.normals.iter().all(|v| (v.length() - 1.0).abs() < 0.02), "mesh {i}");
            // The COLOR filter's output is normalized colour.
            assert!(p.colors.iter().all(|c| c.min_element() >= 0.0 && c.max_element() <= 1.0), "mesh {i}");
        }
        assert!(s.meshes.iter().any(|m| !m.primitives[0].colors.is_empty()));
        // The QUATERNION filter's output is unit rotations.
        let rotations = s.animations[0].channels.iter().filter(|c| c.property == Property::Rotation);
        for ch in rotations {
            for q in ch.values.chunks(4) {
                let len = (q[0] * q[0] + q[1] * q[1] + q[2] * q[2] + q[3] * q[3]).sqrt();
                assert!((len - 1.0).abs() < 0.01, "{q:?}");
            }
        }
    }
}

/// The Draco variants of sample models against their plain ones: the same
/// triangles, skins, clips and morph targets, and the same bounds within
/// Draco's quantization (a little under 1% of a model's size at the
/// exporters' usual 14 bits).
#[test]
fn draco_matches_plain() {
    for (draco, plain) in [
        ("Box/glTF-Draco/Box.gltf", "Box/glTF-Binary/Box.glb"),
        ("Duck/glTF-Draco/Duck.gltf", "Duck/glTF-Binary/Duck.glb"),
        ("Avocado/glTF-Draco/Avocado.gltf", "Avocado/glTF-Binary/Avocado.glb"),
        ("Lantern/glTF-Draco/Lantern.gltf", "Lantern/glTF-Binary/Lantern.glb"),
        ("BrainStem/glTF-Draco/BrainStem.gltf", "BrainStem/glTF-Binary/BrainStem.glb"),
        ("CesiumMan/glTF-Draco/CesiumMan.gltf", "CesiumMan/glTF-Binary/CesiumMan.glb"),
        ("RiggedSimple/glTF-Draco/RiggedSimple.gltf", "RiggedSimple/glTF-Binary/RiggedSimple.glb"),
        ("RiggedFigure/glTF-Draco/RiggedFigure.gltf", "RiggedFigure/glTF-Binary/RiggedFigure.glb"),
        (
            "MorphPrimitivesTest/glTF-Draco/MorphPrimitivesTest.gltf",
            "MorphPrimitivesTest/glTF-Binary/MorphPrimitivesTest.glb",
        ),
        ("CarConcept/glTF-KTX-BasisU-Draco/CarConcept.gltf", "CarConcept/glTF-Binary/CarConcept.glb"),
        (
            "draco-extra/khronos/CesiumMilkTruck/glTF-Draco/CesiumMilkTruck.gltf",
            "draco-extra/khronos/CesiumMilkTruck/glTF-Binary/CesiumMilkTruck.glb",
        ),
        (
            "draco-extra/khronos/SunglassesKhronos/glTF-Draco/SunglassesKhronos.gltf",
            "draco-extra/khronos/SunglassesKhronos/glTF-Binary/SunglassesKhronos.glb",
        ),
        (
            "draco-extra/khronos-virtualcity/VirtualCity/glTF-Draco/VirtualCity.gltf",
            "draco-extra/khronos-virtualcity/VirtualCity/glTF-Binary/VirtualCity.glb",
        ),
        ("draco-extra/google-draco/Box/glTF_Binary/Box_Draco.glb", "draco-extra/google-draco/Box/glTF_Binary/Box.glb"),
        (
            "draco-extra/google-draco/BoxMetaDraco/glTF/BoxMetaDraco.gltf",
            "draco-extra/google-draco/BoxMeta/glTF/BoxMeta.gltf",
        ),
    ] {
        let (Some(d), Some(p)) = (load(draco), load(plain)) else { continue };
        assert!(real_warnings(&d).is_empty(), "{draco}: {:?}", d.info.warnings);
        assert_eq!(d.triangle_count(), p.triangle_count(), "{draco}");
        assert_eq!(d.meshes.len(), p.meshes.len(), "{draco}");
        assert_eq!(
            d.skins.iter().map(|s| s.joints.len()).sum::<usize>(),
            p.skins.iter().map(|s| s.joints.len()).sum::<usize>(),
            "{draco}"
        );
        assert_eq!(d.animations.len(), p.animations.len(), "{draco}");
        let targets = |s: &Scene| s.meshes.iter().flat_map(|m| &m.primitives).map(|p| p.targets.len()).sum::<usize>();
        assert_eq!(targets(&d), targets(&p), "{draco}");
        let (a, b) = (rest_size(&d), rest_size(&p));
        assert!(close(a, b, 0.01 * b.max_element()), "{draco}: {a:?} vs {b:?}");
        for prim in d.meshes.iter().flat_map(|m| &m.primitives) {
            assert!(prim.normals.iter().all(|n| (n.length() - 1.0).abs() < 1e-3), "{draco}");
            assert!(prim.uv0.iter().all(|t| t.is_finite()), "{draco}");
            assert!(
                prim.weights
                    .iter()
                    .all(|w| w.min_element() >= 0.0 && (w.element_sum() - 1.0).abs() < 0.02 || *w == Vec4::ZERO),
                "{draco}"
            );
            for t in &prim.targets {
                assert!(t.positions.is_empty() || t.positions.len() == prim.positions.len(), "{draco}");
            }
        }
    }
}

/// GLB files with Draco inside, as three.js's examples and Sketchfab
/// exports ship them: skinned, instanced and textured primitives.
#[test]
fn draco_glbs() {
    for (rel, triangles) in [
        ("gltf/three/LittlestTokyo.glb", 141_802),
        ("gltf/three/ferrari.glb", 358_788),
        ("gltf/three/kira.glb", 51_601),
        ("draco-extra/three/nemetona.glb", 0),
        ("draco-extra/three/bath_day.glb", 0),
        ("draco-extra/three/pool.glb", 22_280),
        ("draco-extra/three/IridescentDishWithOlives.glb", 0),
        ("draco-extra/three/CarbonFrameBike.glb", 115_083),
        ("draco-extra/three/venice_mask.glb", 295_600),
        ("draco-extra/three/rolex.glb", 108_240),
        ("draco-extra/three/gears.glb", 21_696),
        ("draco-extra/three/ShaderBall2.glb", 13_388),
    ] {
        let Some(s) = load(rel) else { continue };
        let missing_images = |w: &&String| !w.contains("couldn't be decoded") && !w.contains("image");
        let warnings: Vec<_> = s.info.warnings.iter().filter(missing_images).collect();
        assert!(warnings.is_empty(), "{rel}: {warnings:?}");
        if triangles > 0 {
            // Instancing multiplies drawn triangles; the file's own count is
            // the least there can be.
            assert!(s.triangle_count() >= triangles, "{rel}: {}", s.triangle_count());
        } else {
            assert!(s.triangle_count() > 0, "{rel}");
        }
    }
}

/// Plain `.drc` files of every bitstream version from 1.1 to 2.3: meshes
/// keep their faces, point clouds become points, and the `test_nm` sphere
/// decodes alike from every version and method.
#[test]
fn drc_files() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../local/assets/draco-extra");
    let Ok(dir) = std::fs::read_dir(root.join("google-draco")) else {
        eprintln!("skipped: draco-extra isn't downloaded");
        return;
    };
    let mut files: Vec<PathBuf> =
        dir.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e == "drc")).collect();
    files.extend(
        ["three/bunny.drc", "three/bunny_bitstream1.1.drc", "google-draco-bunny/bunny_gltf.drc"].map(|f| root.join(f)),
    );
    files.sort();
    let mut sphere: Option<Vec3> = None;
    for path in files.iter().filter(|p| p.is_file()) {
        let bytes = std::fs::read(path).unwrap();
        let name = path.file_name().unwrap().to_string_lossy();
        assert!(slerp_gltf::sniff_drc(&bytes), "{name}");
        let Ok(Asset::Scene(s)) = slerp_gltf::load_drc(path, &bytes, &Limits::viewer()) else { panic!("{name}") };
        let p = &s.meshes[0].primitives[0];
        let faces = bytes[7] == 1;
        assert_eq!(p.topology == Topology::Triangles, faces, "{name}");
        assert!(!p.positions.is_empty() && p.positions.iter().all(|v| v.is_finite()), "{name}");
        if name.starts_with("test_nm") {
            assert_eq!(s.triangle_count(), 170, "{name}");
            let size = rest_size(&s);
            match sphere {
                None => sphere = Some(size),
                Some(first) => assert!(close(size, first, 0.01 * first.max_element()), "{name}: {size:?} vs {first:?}"),
            }
        }
        if name.starts_with("bunny") {
            assert_eq!(s.triangle_count(), 69_451, "{name}");
        }
        if name.starts_with("pc_") {
            assert_eq!((p.positions.len(), p.colors.len()), (7733, 7733), "{name}");
        }
    }
}

//endregion

/*┌─────────────────╮
| chalk           |
└─────────────────╯*/
//region chalk

const HUMANS: &str = chalk!("html/demos/humans");

/// A posed human should stand between 1.4 and 2.1 m tall and be under 2 m
/// wide in every direction.
fn human_sized(size: Vec3) -> bool {
    (1.4..2.1).contains(&size.y) && size.x < 2.0 && size.z < 2.0 && size.min_element() > 0.2
}

#[test]
fn chalk_metahuman_lod1() {
    let Some(s) = load(&format!("{HUMANS}/tate-tee-lod1.glb")) else { return };
    assert_eq!(s.triangle_count(), 129388);
    assert_eq!(s.skins.len(), 15);
    assert!(s.skins.iter().all(|k| k.joints.len() == 198));
    assert_eq!(s.animations.len(), 32);
    let head = s.meshes.iter().find(|m| m.name == "head").unwrap();
    assert_eq!(head.target_names.len(), 52);
    assert_eq!(head.target_names[0], "browDownLeft");
    assert_eq!(head.primitives[0].targets.len(), 52);
    assert!(real_warnings(&s).is_empty(), "{:?}", s.info.warnings);

    let mut pose = Pose::rest(&s);
    let rest = s.bounds(&pose).size();
    assert!(human_sized(rest), "rest {rest:?}");
    let walk = s.animations.iter().position(|a| a.name == "Walk_Loop").unwrap();
    pose.sample(&s, walk, s.animations[walk].duration * 0.5);
    pose.update_world(&s);
    let mid = s.bounds(&pose).size();
    assert!(human_sized(mid), "mid-walk {mid:?}");
}

#[test]
fn chalk_metahuman_lods() {
    if let Some(s) = load(&format!("{HUMANS}/tate-tee-lod0.glb")) {
        assert_eq!(s.triangle_count(), 177474);
        assert!(s.skins.iter().all(|k| k.joints.len() == 342));
        assert!(human_sized(rest_size(&s)));
    }
    if let Some(s) = load_with(&format!("{HUMANS}/tate-tee-lod3.glb"), &Limits::thumbnail()) {
        assert_eq!(s.triangle_count(), 11930);
        assert!(s.animations.is_empty());
        assert!(human_sized(rest_size(&s)));
    }
}

#[test]
fn chalk_props_and_kits() {
    if let Some(s) = load(chalk!("html/chalk/builder.chalk.glb")) {
        // Three scenes of decreasing detail; the first is shown.
        assert_eq!(s.triangle_count(), 79552);
        assert!(s.info.details.iter().any(|(k, v)| k == "Scenes" && v.contains("lod0")));
    }
    if let Some(s) = load(chalk!("local/kits/kit/rocks.glb")) {
        assert_eq!(s.triangle_count(), 36000);
    }
    for (file, triangles) in [
        ("0af9b1465e8e46fa92e93f8bcad0a705", 168974),
        ("80e8c3ff182240518f5b9dd34bc541bc", 6386),
        ("c51440cab8694de780b7456f281f10f1", 6718),
    ] {
        let Some(s) = load(&format!("{}{file}.glb", chalk!("local/shirts/"))) else { continue };
        assert_eq!(s.triangle_count(), triangles, "{file}");
    }
}

//endregion

/*┌─────────────────╮
| VRM             |
└─────────────────╯*/
//region VRM

/// World-space x of a humanoid bone in the rest pose.
fn bone_x(s: &Scene, bone: &str) -> f32 {
    let node = s.humanoid.as_ref().unwrap().node(bone).unwrap();
    Pose::rest(s).world[node].w_axis.x
}

#[test]
fn vrm0_avatar() {
    let Some(s) = load("vrm/AliciaSolid_vrm-0.51.vrm") else { return };
    assert_eq!(s.info.format, "VRM 0.x");
    let h = s.humanoid.as_ref().unwrap();
    assert_eq!(h.bones.len(), 55);
    assert!(h.node("leftThumbMetacarpal").is_some() && h.node("leftThumbIntermediate").is_none());
    // Turned to face +Z, so the avatar's left is +X.
    assert!(bone_x(&s, "leftHand") > 0.3 && bone_x(&s, "rightHand") < -0.3);
    let joy = s.expressions.iter().find(|e| e.name == "Joy").unwrap();
    assert_eq!(joy.preset.as_deref(), Some("happy"));
    assert!(!joy.binds.is_empty() && joy.binds.iter().all(|b| b.weight > 0.0 && b.weight <= 1.0));
    assert!(s.expressions.iter().any(|e| e.preset.as_deref() == Some("aa")));
    assert!(s.materials.iter().all(|m| !m.unlit && m.metallic == 0.0));
    assert!(s.info.details.iter().any(|(k, v)| k == "Title" && v == "Alicia Solid"));
}

#[test]
fn vrm1_avatars() {
    for rel in ["vrm/Seed-san.vrm", "vrm/VRM1_Constraint_Twist_Sample.vrm"] {
        let Some(s) = load(rel) else { continue };
        assert_eq!(s.info.format, "VRM 1.0", "{rel}");
        assert!(s.humanoid.as_ref().unwrap().bones.len() >= 51, "{rel}");
        assert!(bone_x(&s, "leftHand") > 0.3, "{rel}");
        let names: Vec<&str> = s.expressions.iter().filter_map(|e| e.preset.as_deref()).collect();
        for preset in ["happy", "aa", "blink", "neutral"] {
            assert!(names.contains(&preset), "{rel}: {preset}");
        }
        let blink = s.expressions.iter().find(|e| e.name == "blink").unwrap();
        assert!(!blink.binds.is_empty(), "{rel}");
        // MToon materials are drawn lit, as a dielectric.
        assert!(s.materials.iter().any(|m| !m.unlit && m.metallic == 0.0), "{rel}");
    }
}

#[test]
fn vrm_animation() {
    let Some(s) = load("vrm/test.vrma") else { return };
    assert_eq!(s.info.format, "VRM animation");
    assert!(s.humanoid.as_ref().unwrap().node("hips").is_some());
    assert_eq!(s.animations.len(), 1);
    assert!(s.animations[0].duration > 0.0);
}

//endregion
