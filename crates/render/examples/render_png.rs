//! Renders a test scene to PNG and prints how long each step took.
//!
//! Usage: render_png [--gpu auto|integrated|discrete|software|NAME] [--size N]
//!                   [--frames N] [--bg transparent|env|grey] [--thumb]
//!                   [--overlays wire,normals,tangents,bounds,skeleton,grid,markers]
//!                   [--debug normal|base|metallic|roughness|occlusion|emissive|uv]
//!                   [--key STRENGTH] [--exposure STOPS] [--yaw DEGREES]
//!                   SCENE OUT.png
//!                   [--hdr ENV.hdr] [--pitch DEGREES] [--anim CLIP@SECONDS]
//!                   [--ao on|off] [--viewer-frame WxH]
//! SCENE is a glTF/GLB/VRM or Quake/Half-Life file, or `spheres` (a metallic/roughness grid), `shapes` (a cube, a
//! sphere and a plane), `quake` (a lightmapped wall, a liquid and a sky),
//! `skin` (a bent, skinned and morphed tube), `alpha` (masked and blended
//! layers), `glass` (transmission) or `points` (point clouds).

use slerp_render::wgpu;
use slerp_render::{Background, DebugChannel, GpuPolicy, Overlays, RenderSettings, Renderer, RendererConfig, open_gpu};
use slerp_scene::*;
use std::time::Instant;

#[path = "../tests/scenes/mod.rs"]
mod scenes;
use scenes::*;

fn main() {
    let t0 = Instant::now();
    let mut args = std::env::args().skip(1);
    let mut policy = GpuPolicy::Auto;
    let mut size = 512u32;
    let mut frames = 3u32;
    let mut background = None;
    let mut thumb = false;
    let mut overlays = Overlays::default();
    let mut debug = DebugChannel::None;
    let mut key = 1.0f32;
    let mut exposure = 0.0f32;
    let mut yaw = None;
    let mut pitch = None;
    let mut hdr: Option<String> = None;
    let mut animation: Option<(usize, f32)> = None;
    let mut viewer_frame: Option<(u32, u32)> = None;
    let mut ao: Option<bool> = None;
    let mut positional = Vec::new();
    while let Some(a) = args.next() {
        match a.as_str() {
            "--gpu" => policy = GpuPolicy::parse(&args.next().unwrap_or_default()),
            "--size" => size = args.next().and_then(|s| s.parse().ok()).unwrap_or(512),
            "--frames" => frames = args.next().and_then(|s| s.parse().ok()).unwrap_or(3),
            "--bg" => background = args.next(),
            "--thumb" => thumb = true,
            "--key" => key = args.next().and_then(|s| s.parse().ok()).unwrap_or(1.0),
            "--exposure" => exposure = args.next().and_then(|s| s.parse().ok()).unwrap_or(0.0),
            "--yaw" => yaw = args.next().and_then(|s| s.parse::<f32>().ok()),
            "--pitch" => pitch = args.next().and_then(|s| s.parse::<f32>().ok()),
            "--hdr" => hdr = args.next(),
            "--ao" => ao = args.next().map(|v| v == "on"),
            "--viewer-frame" => {
                viewer_frame = args.next().and_then(|s| {
                    let (w, h) = s.split_once('x')?;
                    Some((w.parse().ok()?, h.parse().ok()?))
                })
            }
            "--anim" => {
                let v = args.next().unwrap_or_default();
                let (a, t) = v.split_once('@').unwrap_or((&v, "0"));
                animation = Some((a.parse().unwrap_or(0), t.parse().unwrap_or(0.0)));
            }
            "--overlays" => {
                for o in args.next().unwrap_or_default().split(',') {
                    match o {
                        "wire" => overlays.wireframe = true,
                        "normals" => overlays.normals = true,
                        "tangents" => overlays.tangents = true,
                        "bounds" => overlays.bounds = true,
                        "skeleton" => overlays.skeleton = true,
                        "grid" => overlays.grid = true,
                        "markers" => overlays.markers = true,
                        _ => {}
                    }
                }
            }
            "--debug" => {
                debug = match args.next().unwrap_or_default().as_str() {
                    "normal" => DebugChannel::Normal,
                    "base" => DebugChannel::BaseColor,
                    "metallic" => DebugChannel::Metallic,
                    "roughness" => DebugChannel::Roughness,
                    "occlusion" => DebugChannel::Occlusion,
                    "emissive" => DebugChannel::Emissive,
                    "uv" => DebugChannel::UvChecker,
                    _ => DebugChannel::None,
                }
            }
            _ => positional.push(a),
        }
    }
    let [scene_name, out] = positional.as_slice() else {
        eprintln!("usage: render_png [options] SCENE OUT.png");
        std::process::exit(2);
    };

    let gpu = open_gpu(&policy).expect("no GPU");
    let t_gpu = t0.elapsed();
    let config = if thumb { RendererConfig::thumbnail() } else { RendererConfig::viewer() };
    let mut renderer = Renderer::with_config(&gpu, wgpu::TextureFormat::Rgba8UnormSrgb, config);
    let t_renderer = t0.elapsed();

    if let Some(path) = &hdr {
        let opts =
            textures::DecodeOptions { min_size: None, max_size: Some(2048), all_mips: false, first_layer_only: true };
        let tex = textures::decode_file(std::path::Path::new(path), &opts).expect("decode HDR");
        renderer.set_environment(&gpu, &tex);
    }
    let t_env = t0.elapsed();
    let (scene, front) = match scene_name.as_str() {
        "ball" => (ball(), false),
        "displaced" => (displaced(), false),
        "spheres" => (spheres(), true),
        "shapes" => (shapes(), false),
        "quake" => (quake(), true),
        "skin" => (skinned(), false),
        "alpha" => (alpha(), true),
        "glass" => (glass(), true),
        "points" => (points(), false),
        path => {
            let bytes = std::fs::read(path).expect("read model");
            let limits = if thumb { Limits::thumbnail() } else { Limits { animations: true, ..Limits::thumbnail() } };
            let p = std::path::Path::new(path);
            let head = &bytes[..bytes.len().min(64)];
            let asset = match slerp_quake::sniff(p, head) {
                Some(format) => slerp_quake::load(format, p, &bytes, &limits),
                None => slerp_gltf::load(p, &bytes, &limits),
            };
            match asset.expect("load model") {
                Asset::Scene(s) => (s, false),
                _ => panic!("not a scene"),
            }
        }
    };
    // chalk's MetaHumans carry eye-occlusion and tear meshes that only its
    // own shaders can draw; as plain materials they are opaque white shells
    // over the eyes, so chalk hides them, and so does this example.
    let mut scene = scene;
    for i in 0..scene.nodes.len() {
        let name = scene.nodes[i].mesh.and_then(|m| scene.meshes.get(m)).map(|m| m.name.clone()).unwrap_or_default();
        if name.starts_with("eyeshell") || name.starts_with("eyeEdge") {
            scene.nodes[i].visible = false;
        }
    }
    let t_scene = t0.elapsed();
    let mut gs = renderer.prepare(&gpu, &scene);
    if let Some((clip, time)) = animation.filter(|(a, _)| *a < scene.animations.len()) {
        let mut pose = Pose::rest(&scene);
        pose.sample(&scene, clip, time);
        pose.update_world(&scene);
        renderer.update_pose(&gpu, &mut gs, &scene, &pose);
    }
    for w in &gs.warnings {
        eprintln!("warning: {w}");
    }
    let t_prepare = t0.elapsed();

    // The renderer's bounds leave out sky surfaces; the Quake test scene's
    // sky quad is part of what it shows, so frame on everything there.
    let frame_bounds = if scene_name == "quake" { scene.bounds(&Pose::rest(&scene)) } else { gs.bounds };
    let mut cam = OrbitCamera::framing(&frame_bounds, 1.0);
    if scene.kind == SceneKind::Level {
        // A high three-quarter view that shows a map's layout.
        cam.yaw = 35f32.to_radians();
        cam.pitch = 45f32.to_radians();
        cam.frame(&gs.bounds, 1.0);
    }
    if front {
        cam.yaw = 0.0;
        cam.pitch = 0.0;
        cam.frame(&frame_bounds, 1.0);
    }
    if yaw.is_some() || pitch.is_some() {
        cam.yaw = yaw.map_or(cam.yaw, f32::to_radians);
        cam.pitch = pitch.map_or(cam.pitch, f32::to_radians);
        cam.frame(&gs.bounds, 1.0);
    }
    let view = cam.view(1.0);
    let mut settings = if thumb { RenderSettings::thumbnail() } else { RenderSettings::viewer() };
    settings.overlays = overlays;
    settings.debug = debug;
    settings.key_light = key;
    settings.exposure = exposure;
    settings.time = 1.3;
    settings.ao = ao.unwrap_or(settings.ao);
    match background.as_deref() {
        Some("transparent") => settings.background = Background::Transparent,
        Some("env") => settings.background = Background::Environment { blur: 0.6 },
        Some("grey") => settings.background = Background::Color([0.18, 0.18, 0.19, 1.0]),
        _ => {}
    }
    let image = renderer.render_image(&gpu, &gs, &view, &settings, size, size);
    let t_first = t0.elapsed();
    let mut warm = Vec::new();
    for i in 1..frames {
        let t = Instant::now();
        settings.time = 1.3 + i as f32 * 0.1;
        let _ = renderer.render_image(&gpu, &gs, &view, &settings, size, size);
        warm.push(t.elapsed().as_secs_f64() * 1000.0);
    }

    // Viewer-style frames: drawn into a window-sized target, no readback,
    // waiting for the GPU so the time is the frame's.
    let mut viewer_ms = Vec::new();
    if let Some((w, h)) = viewer_frame {
        let target = gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: None,
            size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        let target_view = target.create_view(&Default::default());
        let mut vs = RenderSettings::viewer();
        vs.overlays = settings.overlays;
        vs.ao = settings.ao;
        let view = cam.view(w as f32 / h as f32);
        for i in 0..12 {
            let t = Instant::now();
            vs.time = i as f32 * 0.1;
            renderer.render(&gpu, &gs, &view, &vs, &target_view, (w, h));
            let _ = gpu.device.poll(wgpu::PollType::wait_indefinitely());
            if i >= 2 {
                viewer_ms.push(t.elapsed().as_secs_f64() * 1000.0);
            }
        }
        viewer_ms.sort_by(f64::total_cmp);
    }

    let file = std::fs::File::create(out).expect("create output");
    let mut enc = png::Encoder::new(std::io::BufWriter::new(file), image.width, image.height);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header().unwrap().write_image_data(&image.pixels).unwrap();
    let ms = |d: std::time::Duration| d.as_secs_f64() * 1000.0;
    println!(
        "{} ({:?}) via {}: gpu {:.1} ms, renderer {:.1}, env {:.1}, scene {:.1}, prepare {:.1}, first frame {:.1}, total {:.1} ms; warm frames {}",
        gpu.info.name,
        gpu.info.device_type,
        gpu.chosen_by,
        ms(t_gpu),
        ms(t_renderer - t_gpu),
        ms(t_env - t_renderer),
        ms(t_scene - t_env),
        ms(t_prepare - t_scene),
        ms(t_first - t_prepare),
        ms(t0.elapsed()),
        warm.iter().map(|w| format!("{w:.1}")).collect::<Vec<_>>().join(" "),
    );
    if !viewer_ms.is_empty() {
        println!(
            "viewer frames: median {:.2} ms, min {:.2}, max {:.2}",
            viewer_ms[viewer_ms.len() / 2],
            viewer_ms[0],
            viewer_ms[viewer_ms.len() - 1]
        );
    }
}
