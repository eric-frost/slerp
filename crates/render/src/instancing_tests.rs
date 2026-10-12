//! Instanced drawing checked on lavapipe: copies of one mesh drawn as
//! instances, culled or not, must look exactly like copies that each have
//! a mesh of their own and are drawn one by one.

use crate::instancing::{FrameRuns, Frustum};
use crate::{
    Background, GpuPolicy, GpuScene, Image8, Overlays, RenderSettings, Renderer, RendererConfig, THUMBNAIL_FORMAT,
};
use glam::{Quat, Vec3, Vec4};
use slerp_scene::*;
use std::sync::Arc;

#[path = "../tests/scenes/mod.rs"]
mod scenes;

/// A scene of repeats: a 7 x 7 grid of one box (some mirrored, one hidden,
/// one under a hidden parent), a masked leaf card and a blended pane placed
/// three times each, and two skinned, morphed tubes sharing one mesh, each
/// with its own skeleton and morph weight.  With `shared` false every node
/// gets its own copy of its mesh, so nothing repeats.
fn repeats(shared: bool) -> Scene {
    let mut scene = Scene::default();
    let mut leaf = Vec::new();
    for y in 0..32i32 {
        for x in 0..32i32 {
            let inside = (x - 16).pow(2) + (y - 16).pow(2) < 150;
            leaf.extend_from_slice(&[40, 160, 60, if inside { 255 } else { 0 }]);
        }
    }
    scene.textures.push(Arc::new(Texture::from_rgba8(32, 32, leaf, ColorSpace::Srgb)));
    let mut leaf = Material { alpha_mode: AlphaMode::Mask(0.5), double_sided: true, ..Default::default() };
    leaf.base_color_texture = Some(TexRef::new(0));
    scene.materials = vec![
        Material { base_color: Vec4::new(0.8, 0.3, 0.1, 1.0), roughness: 0.5, ..Default::default() },
        leaf,
        Material {
            base_color: Vec4::new(0.3, 0.5, 1.0, 0.4),
            alpha_mode: AlphaMode::Blend,
            double_sided: true,
            ..Default::default()
        },
    ];
    let mesh = |prim: Primitive, material: usize| Mesh {
        name: "m".into(),
        primitives: vec![Primitive { material: Some(material), ..prim }],
        target_names: vec![],
    };
    scene.meshes = vec![mesh(Shape::Cube.primitive(1), 0), mesh(Shape::Plane.primitive(1), 1)];
    scene.meshes.push(mesh(Shape::Plane.primitive(1), 2));
    let place = |scene: &mut Scene, mesh: usize, t: Transform| {
        let mut node = Node::new("copy");
        node.mesh = Some(mesh);
        node.transform = t;
        scene.nodes.push(node);
        scene.nodes.len() - 1
    };

    // 1. The boxes, every fifth mirrored.
    for i in 0..7 {
        for j in 0..7 {
            let k = i * 7 + j;
            let sx = if k % 5 == 0 { -0.8 } else { 0.8 };
            let t = Transform {
                translation: Vec3::new((i as f32 - 3.0) * 1.3, 0.4, (j as f32 - 3.0) * 1.3),
                rotation: Quat::from_rotation_y(k as f32 * 0.37),
                scale: Vec3::new(sx, 0.8, 0.8),
            };
            let n = place(&mut scene, 0, t);
            scene.roots.push(n);
            if k == 17 {
                scene.nodes[n].visible = false;
            }
        }
    }
    let mut parent = Node::new("hidden parent");
    parent.visible = false;
    scene.nodes.push(parent);
    let p = scene.nodes.len() - 1;
    scene.roots.push(p);
    let c = place(&mut scene, 0, Transform { translation: Vec3::new(0.0, 2.0, 0.0), ..Transform::IDENTITY });
    scene.nodes[p].children.push(c);

    // 2. Leaves and panes, standing up above the boxes.
    for (mesh, z) in [(1, -1.0f32), (2, 1.5)] {
        for x in [-2.5f32, 0.0, 2.5] {
            let t = Transform {
                translation: Vec3::new(x, 1.6, z + x * 0.2),
                rotation: Quat::from_rotation_x(std::f32::consts::FRAC_PI_2),
                scale: Vec3::splat(1.6),
            };
            let n = place(&mut scene, mesh, t);
            scene.roots.push(n);
        }
    }

    // 3. Two skinned tubes on one mesh, each with its skeleton.
    let tube = scenes::skinned();
    let material = scene.materials.len();
    scene.materials.push(tube.materials[0].clone());
    let mut tube_mesh = tube.meshes[0].clone();
    tube_mesh.primitives[0].material = Some(material);
    scene.meshes.push(tube_mesh);
    let tube_mesh = scene.meshes.len() - 1;
    for (k, (x, bend, weight)) in [(-5.5f32, -0.7f32, 1.0f32), (5.5, 0.9, 0.3)].into_iter().enumerate() {
        let base = scene.nodes.len();
        let mut skinned = Node::new("tube");
        skinned.mesh = Some(tube_mesh);
        skinned.skin = Some(k);
        skinned.weights = vec![weight];
        let mut root = Node::new("root");
        root.transform = Transform { translation: Vec3::new(x, 0.0, 0.0), ..Transform::IDENTITY };
        root.children = vec![base + 2];
        let mut upper = Node::new("upper");
        upper.transform = Transform {
            translation: Vec3::new(0.0, 1.0, 0.0),
            rotation: Quat::from_rotation_z(bend),
            scale: Vec3::ONE,
        };
        scene.nodes.extend([skinned, root, upper]);
        scene.roots.extend([base, base + 1]);
        let mut skin = tube.skins[0].clone();
        skin.joints = vec![base + 1, base + 2];
        skin.skeleton = Some(base + 1);
        scene.skins.push(skin);
    }

    // 4. Without sharing, every node after the first to show a mesh gets
    //    a copy of it.
    if !shared {
        let mut used = vec![false; scene.meshes.len()];
        for i in 0..scene.nodes.len() {
            let Some(m) = scene.nodes[i].mesh else { continue };
            if std::mem::replace(&mut used[m], true) {
                scene.meshes.push(scene.meshes[m].clone());
                scene.nodes[i].mesh = Some(scene.meshes.len() - 1);
            }
        }
    }
    scene
}

/// The largest difference in any channel, and how many pixels differ by
/// more than 2.
fn differences(a: &Image8, b: &Image8) -> (u8, usize) {
    let mut worst = 0;
    let mut bad = 0;
    for (x, y) in a.pixels.chunks_exact(4).zip(b.pixels.chunks_exact(4)) {
        let d = x.iter().zip(y).map(|(p, q)| p.abs_diff(*q)).max().unwrap_or(0);
        worst = worst.max(d);
        bad += (d > 2) as usize;
    }
    (worst, bad)
}

/// Draws instances seen through `view`, as the main pass would.
fn drawn(gs: &GpuScene, view: &View) -> (u32, usize) {
    let mut runs = FrameRuns::default();
    runs.build(gs, &view.view, Frustum::new(&view.view_proj()).as_ref());
    let all = runs.opaque.iter().chain(&runs.transmissive).chain(&runs.blended);
    (all.clone().map(|r| r.count).sum(), all.count())
}

/// Copies drawn as instances, with and without culling, look like the same
/// copies drawn one by one, from outside, close up (the grid crosses the
/// frustum's edges) and from inside (half of it behind the camera), with
/// and without the wireframe and normal overlays.
#[test]
fn instanced_copies_draw_like_separate_meshes() {
    if !std::path::Path::new("/usr/share/vulkan/icd.d/lvp_icd.json").exists() {
        eprintln!("lavapipe isn't installed; skipped");
        return;
    }
    let Some(gpu) = crate::test_gpu(GpuPolicy::Software) else { return };
    let mut renderer = Renderer::with_config(&gpu, THUMBNAIL_FORMAT, RendererConfig::thumbnail());
    let (shared, separate) = (repeats(true), repeats(false));
    let gs_shared = renderer.prepare(&gpu, &shared);
    let mut gs_unculled = renderer.prepare(&gpu, &shared);
    let gs_separate = renderer.prepare(&gpu, &separate);

    // 1. Grouping: 50 boxes, 3 leaves and 3 panes and 2 tubes make 4
    //    groups; apart, every draw is a group of its own.
    assert_eq!(gs_shared.draws.len(), 58);
    assert_eq!(gs_shared.groups.len(), 4);
    assert_eq!(gs_separate.groups.len(), gs_separate.draws.len());
    // The hidden box and the box under the hidden parent aren't listed.
    assert_eq!(gs_shared.groups[0].visible(), 48);
    assert_eq!(gs_shared.groups[0].mirrored, 10);
    for g in &mut gs_unculled.groups {
        g.rigid = false;
    }

    // 2. Views.
    let mut far = OrbitCamera::framing(&gs_shared.bounds, 1.0);
    let outside = far.view(1.0);
    far.distance *= 0.3;
    let close = far.view(1.0);
    let inside = FlyCamera {
        position: Vec3::new(-1.0, 1.2, 0.3),
        yaw: 1.2,
        pitch: -0.3,
        fov_y: 70f32.to_radians(),
        near: 0.05,
        far: 100.0,
    }
    .view(1.0);
    let grey = RenderSettings { background: Background::Color([0.2, 0.2, 0.21, 1.0]), ..RenderSettings::thumbnail() };
    let lines = RenderSettings { overlays: Overlays { wireframe: true, normals: true, ..Overlays::default() }, ..grey };
    let mut failures = Vec::new();
    for (name, view) in [("outside", &outside), ("close", &close), ("inside", &inside)] {
        let (shown, calls) = drawn(&gs_shared, view);
        let (shown_unculled, _) = drawn(&gs_unculled, view);
        let (_, separate_calls) = drawn(&gs_separate, view);
        eprintln!("{name}: {shown} of {shown_unculled} instances in {calls} calls; {separate_calls} calls apart");
        assert!(calls < separate_calls, "{name}: instancing saved no calls");
        if name != "outside" {
            assert!(shown < shown_unculled, "{name}: nothing was culled");
        }
        for (settings, overlays) in [(&grey, false), (&lines, true)] {
            let image = |gs: &GpuScene, r: &mut Renderer| r.render_image(&gpu, gs, view, settings, 192, 192);
            let a = image(&gs_shared, &mut renderer);
            let b = image(&gs_separate, &mut renderer);
            let c = image(&gs_unculled, &mut renderer);
            for (what, other) in [("apart", &b), ("unculled", &c)] {
                let (worst, bad) = differences(&a, other);
                // Overlay lines blend, and instancing changes the order
                // in which overlapping lines of different copies land.
                let allowed = if overlays && what == "apart" { 192 * 192 / 200 } else { 0 };
                if bad > allowed {
                    failures.push(format!("{name}, overlays {overlays}, {what}: {bad} pixels differ (worst {worst})"));
                }
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
