//! Textures kept in GPU blocks draw like their RGBA twins, and textures
//! uploaded after the first frame (`prepare_untextured`, `upload_textures`)
//! end up drawing exactly as `prepare` draws them.  Rendered on lavapipe,
//! which samples BC7 and BC5 when its Mesa does, and otherwise goes through
//! the renderer's CPU decoding: either way the pictures must match.

use slerp_render::{GpuPolicy, RenderSettings, Renderer, RendererConfig, open_gpu, wgpu};
use slerp_scene::textures::{DecodeOptions, GpuBlocks, decode, decode_for_gpu};
use slerp_scene::{Image8, Material, Mesh, OrbitCamera, Scene, Shape, TexRef, Texture, Transform};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

fn sample(name: &str) -> Option<PathBuf> {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../local/assets/textures/ktx-software/ktx2").join(name);
    p.exists().then_some(p)
}

/// A tilted plane with a base colour map and a normal map.
fn plane(color: Texture, normal: Texture) -> Scene {
    let mut scene = Scene::default();
    let mut prim = Shape::Plane.primitive(4);
    prim.material = Some(0);
    let mut m = Material { roughness: 0.6, ..Default::default() };
    m.base_color_texture = Some(TexRef::new(0));
    m.normal_texture = Some(TexRef::new(1));
    scene.materials.push(m);
    scene.textures = vec![Arc::new(color), Arc::new(normal)];
    let n = scene.add_mesh_node(Mesh { name: "plane".into(), primitives: vec![prim], target_names: vec![] });
    scene.nodes[n].transform = Transform {
        rotation: glam::Quat::from_rotation_x(-0.9) * glam::Quat::from_rotation_y(0.4),
        ..Transform::IDENTITY
    };
    scene
}

fn mean_difference(a: &Image8, b: &Image8) -> f64 {
    let sum: u64 = a.pixels.iter().zip(&b.pixels).map(|(x, y)| x.abs_diff(*y) as u64).sum();
    sum as f64 / a.pixels.len() as f64
}

#[test]
fn block_textures_and_late_uploads_draw_like_rgba() {
    if !Path::new("/usr/share/vulkan/icd.d/lvp_icd.json").exists() {
        eprintln!("lavapipe isn't installed; skipping");
        return;
    }
    let (Some(color), Some(normal)) =
        (sample("ktx_document_uastc_rdo_4_zstd_5.ktx2"), sample("Iron_Bars_001_normal_blze.ktx2"))
    else {
        eprintln!("skipping: KTX-Software samples missing");
        return;
    };
    let (color, normal) = (std::fs::read(color).unwrap(), std::fs::read(normal).unwrap());
    let full = DecodeOptions::full();
    let bc = GpuBlocks { bc: true, astc: false };
    let mut rgba = plane(decode(&color, &full).unwrap(), decode(&normal, &full).unwrap());
    let mut blocks = plane(decode_for_gpu(&color, &full, bc).unwrap(), decode_for_gpu(&normal, &full, bc).unwrap());
    // Scenes with only a few megabytes of textures get them at once; more
    // materials with copies of the textures (out of sight) make enough to
    // go up later.
    for scene in [&mut rgba, &mut blocks] {
        for _ in 0..24 {
            let mut m = scene.materials[0].clone();
            m.base_color_texture = Some(TexRef::new(scene.textures.len()));
            scene.textures.push(Arc::new((*scene.textures[0]).clone()));
            scene.materials.push(m);
        }
    }
    assert!(blocks.textures.iter().all(|t| slerp_scene::textures::is_compressed(t)));

    let Ok(gpu) = open_gpu(&GpuPolicy::Software) else { return };
    let samples_bc = gpu.device.features().contains(wgpu::Features::TEXTURE_COMPRESSION_BC);
    eprintln!("lavapipe samples BC blocks itself: {samples_bc}");
    let mut r = Renderer::with_config(&gpu, wgpu::TextureFormat::Rgba8UnormSrgb, RendererConfig::thumbnail());
    let settings = RenderSettings::thumbnail();
    let draw = |gs: &slerp_render::GpuScene, r: &mut Renderer| {
        let view = OrbitCamera::framing(&gs.bounds, 1.0).view(1.0);
        r.render_image(&gpu, gs, &view, &settings, 256, 256)
    };

    // 1. Blocks draw like the texels they came from.
    let a = r.prepare(&gpu, &rgba);
    let b = r.prepare(&gpu, &blocks);
    let (ia, ib) = (draw(&a, &mut r), draw(&b, &mut r));
    let diff = mean_difference(&ia, &ib);
    assert!(diff < 1.5, "blocks draw {diff:.2} levels away from RGBA on average");

    // 2. Late uploads: something drawable at once, then the same picture.
    let mut c = r.prepare_untextured(&gpu, &blocks);
    assert!(Renderer::textures_pending(&c));
    let early = draw(&c, &mut r);
    assert!(early.pixels.chunks_exact(4).any(|p| p[3] > 0), "the plane draws before its textures");
    let mut rounds = 0;
    while r.upload_textures(&gpu, &mut c, &blocks, Duration::ZERO) {
        rounds += 1;
    }
    // One texture per round with no time to spare: the plane's two and the
    // copies, all but the last before the loop ends.
    assert_eq!(rounds, 2 + 24 - 1, "one texture per round with no time to spare");
    let late = draw(&c, &mut r);
    assert_eq!(late.pixels, ib.pixels, "late uploads draw as prepare does");
    assert_eq!(c.gpu_bytes(), b.gpu_bytes());
}
