//! Reference-image tests, rendered on lavapipe so they give the same
//! pixels on any machine with Mesa's CPU driver (and in CI).  Each picture
//! is compared with `tests/reference/<name>.png` within a tolerance; a
//! missing reference is written instead, as is every reference when
//! `SLERP_BLESS=1`.  Failed renders are saved next to the reference as
//! `<name>.actual.png` for inspection.
//!
//! The Khronos sample models are read from `local/assets/gltf/khronos`,
//! which `tools/fetch-test-assets.ts` fills; without it those cases are
//! skipped.

mod scenes;

use slerp_render::{
    Background, DebugChannel, Gpu, GpuPolicy, GpuScene, Overlays, RenderSettings, Renderer, RendererConfig,
    TextureDrawParams, Tonemap, open_gpu, wgpu,
};
use slerp_scene::{Asset, Image8, Limits, OrbitCamera, Pose, Scene, View};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

/*┌─────────────────╮
| Harness         |
└─────────────────╯*/
//region Harness

struct Ctx {
    gpu: Gpu,
    renderer: Renderer,
}

/// One lavapipe device and renderer for every test, since tests run on
/// several threads and starting a device sets the environment.
fn ctx() -> Option<std::sync::MutexGuard<'static, Ctx>> {
    static CTX: OnceLock<Option<Mutex<Ctx>>> = OnceLock::new();
    let ctx = CTX.get_or_init(|| {
        if !Path::new("/usr/share/vulkan/icd.d/lvp_icd.json").exists() {
            eprintln!("lavapipe isn't installed; skipping GPU tests");
            return None;
        }
        let gpu = open_gpu(&GpuPolicy::Software).ok()?;
        let renderer = Renderer::with_config(&gpu, wgpu::TextureFormat::Rgba8UnormSrgb, RendererConfig::thumbnail());
        Some(Mutex::new(Ctx { gpu, renderer }))
    });
    ctx.as_ref().map(|m| m.lock().unwrap_or_else(|e| e.into_inner()))
}

fn reference_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/reference")
}

fn assets() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../local/assets")
}

fn write_png(path: &Path, img: &Image8) {
    let file = std::fs::File::create(path).unwrap();
    let mut enc = png::Encoder::new(std::io::BufWriter::new(file), img.width, img.height);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header().unwrap().write_image_data(&img.pixels).unwrap();
}

fn read_png(path: &Path) -> Option<Image8> {
    let decoder = png::Decoder::new(std::io::BufReader::new(std::fs::File::open(path).ok()?));
    let mut reader = decoder.read_info().ok()?;
    let mut buf = vec![0; reader.output_buffer_size()?];
    let info = reader.next_frame(&mut buf).ok()?;
    if info.color_type != png::ColorType::Rgba || info.bit_depth != png::BitDepth::Eight {
        return None;
    }
    buf.truncate(info.buffer_size());
    Some(Image8 { width: info.width, height: info.height, pixels: buf })
}

/// Fails when more than 0.5% of pixels differ by more than 12 levels in any
/// channel, which allows for Mesa and LLVM versions rounding differently
/// but catches anything visible.
fn compare(name: &str, img: &Image8) -> Result<(), String> {
    let path = reference_dir().join(format!("{name}.png"));
    let bless = std::env::var_os("SLERP_BLESS").is_some_and(|v| v == "1");
    let Some(reference) = read_png(&path).filter(|_| !bless) else {
        std::fs::create_dir_all(reference_dir()).unwrap();
        write_png(&path, img);
        eprintln!("wrote reference {}", path.display());
        return Ok(());
    };
    if (reference.width, reference.height) != (img.width, img.height) {
        return Err(format!("{name}: size {}x{} != {}x{}", img.width, img.height, reference.width, reference.height));
    }
    let mut bad = 0usize;
    let mut worst = 0u8;
    for (a, b) in img.pixels.chunks_exact(4).zip(reference.pixels.chunks_exact(4)) {
        let d = a.iter().zip(b).map(|(x, y)| x.abs_diff(*y)).max().unwrap_or(0);
        worst = worst.max(d);
        if d > 12 {
            bad += 1;
        }
    }
    let total = (img.width * img.height) as usize;
    if bad * 200 > total {
        let actual = reference_dir().join(format!("{name}.actual.png"));
        write_png(&actual, img);
        return Err(format!("{name}: {bad} of {total} pixels differ (worst {worst}); see {}", actual.display()));
    }
    Ok(())
}

/// Places the camera for a prepared scene.
type Camera = fn(&GpuScene) -> View;

fn three_quarter(gs: &GpuScene) -> View {
    OrbitCamera::framing(&gs.bounds, 1.0).view(1.0)
}

fn front(gs: &GpuScene) -> View {
    let mut cam = OrbitCamera::framing(&gs.bounds, 1.0);
    cam.yaw = 0.0;
    cam.pitch = 0.0;
    cam.frame(&gs.bounds, 1.0);
    cam.view(1.0)
}

/// The front view framed on a box that includes the Quake test scene's sky
/// quad, which the renderer's bounds leave out.
fn front_with_sky(_: &GpuScene) -> View {
    let b = slerp_scene::Aabb { min: glam::Vec3::new(-1.6, -0.5, 0.0), max: glam::Vec3::new(1.6, 0.5, 0.0) };
    let mut cam = OrbitCamera::framing(&b, 1.0);
    cam.yaw = 0.0;
    cam.pitch = 0.0;
    cam.frame(&b, 1.0);
    cam.view(1.0)
}

/// Standing on the floor of a level, looking up at 25 degrees, turned
/// so the panorama's seam (where the turn count wraps, looking west) is
/// in view.
fn looking_up(_: &GpuScene) -> View {
    let cam = slerp_scene::FlyCamera {
        position: glam::Vec3::Y * 1.0,
        yaw: 80f32.to_radians(),
        pitch: 25f32.to_radians(),
        fov_y: 70f32.to_radians(),
        near: 0.05,
        far: 100.0,
    };
    cam.view(1.0)
}

fn render(ctx: &mut Ctx, scene: &Scene, view: Camera, settings: &RenderSettings) -> Image8 {
    let Ctx { gpu, renderer } = ctx;
    let gs = renderer.prepare(gpu, scene);
    renderer.render_image(gpu, &gs, &view(&gs), settings, 256, 256)
}

/// A Khronos sample from the shared corpus, or one downloaded for these
/// tests into `local/assets/gltf-extra`.
fn load_khronos(name: &str) -> Option<Scene> {
    let path = ["gltf/khronos", "gltf-extra"]
        .iter()
        .map(|d| assets().join(d).join(name))
        .flat_map(|dir| [dir.join(format!("glTF-Binary/{name}.glb")), dir.join(format!("glTF/{name}.gltf"))])
        .find(|p| p.exists())?;
    let bytes = std::fs::read(&path).ok()?;
    match slerp_gltf::load(&path, &bytes, &Limits::thumbnail()) {
        Ok(Asset::Scene(s)) => Some(s),
        _ => None,
    }
}

//endregion

/*┌─────────────────╮
| Reference images|
└─────────────────╯*/
//region Reference images

#[test]
fn generated_scenes() {
    let Some(mut ctx) = ctx() else { return };
    let thumb = RenderSettings::thumbnail();
    let grey = RenderSettings { background: Background::Color([0.2, 0.2, 0.21, 1.0]), ..thumb };
    let cases: Vec<(&str, Scene, Camera, RenderSettings)> = vec![
        ("spheres", scenes::spheres(), front, thumb),
        ("shapes", scenes::shapes(), three_quarter, thumb),
        ("quake", scenes::quake(), front_with_sky, RenderSettings { time: 1.3, ..grey }),
        ("skin", scenes::skinned(), three_quarter, thumb),
        ("alpha", scenes::alpha(), front, grey),
        (
            "overlays",
            scenes::skinned(),
            three_quarter,
            RenderSettings {
                overlays: Overlays {
                    wireframe: true,
                    normals: true,
                    bounds: true,
                    skeleton: true,
                    grid: true,
                    ..Overlays::default()
                },
                ..grey
            },
        ),
        ("debug-normals", scenes::shapes(), three_quarter, RenderSettings { debug: DebugChannel::Normal, ..grey }),
        ("debug-uv", scenes::shapes(), three_quarter, RenderSettings { debug: DebugChannel::UvChecker, ..grey }),
        ("aces", scenes::spheres(), front, RenderSettings { tonemap: Tonemap::Aces, exposure: 0.5, ..grey }),
        ("environment-background", scenes::shapes(), three_quarter, RenderSettings::viewer()),
        ("splats", scenes::splats(), three_quarter, grey),
        ("splats-transparent", scenes::splats(), three_quarter, thumb),
        ("splats-small", scenes::splats(), three_quarter, RenderSettings { splat_scale: 0.4, ..grey }),
        // Without ambient occlusion, as every picture was before it.
        ("shapes-no-ao", scenes::shapes(), three_quarter, RenderSettings { ao: false, ..thumb }),
        ("spheres-no-ao", scenes::spheres(), front, RenderSettings { ao: false, ..thumb }),
        ("glass", scenes::glass(), front, grey),
        ("glass-transparent", scenes::glass(), front, thumb),
        ("glass-environment", scenes::glass(), front, RenderSettings::viewer()),
        ("points", scenes::points(), three_quarter, grey),
        ("points-world", scenes::points(), three_quarter, RenderSettings { point_world_size: true, ..grey }),
        ("points-large", scenes::points(), three_quarter, RenderSettings { point_size: 3.0, ..grey }),
        ("panorama-sky", scenes::panorama_sky(), looking_up, RenderSettings { tonemap: Tonemap::None, ..thumb }),
    ];
    let mut failures = Vec::new();
    for (name, scene, view, settings) in &cases {
        let img = render(&mut ctx, scene, *view, settings);
        if let Err(e) = compare(name, &img) {
            failures.push(e);
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// The viewer sorts splats on the GPU; thumbnails sort on the CPU.  Both
/// must draw the same picture, from several sides (the sort decides which
/// splats cover which).
#[test]
fn splat_sorts_agree() {
    let Some(mut ctx) = ctx() else { return };
    let Ctx { gpu, renderer } = &mut *ctx;
    let config = RendererConfig { gpu_splat_sort: true, ..RendererConfig::thumbnail() };
    let mut gpu_sorting = Renderer::with_config(gpu, wgpu::TextureFormat::Rgba8UnormSrgb, config);
    let scene = scenes::splats();
    let settings =
        RenderSettings { background: Background::Color([0.2, 0.2, 0.21, 1.0]), ..RenderSettings::thumbnail() };
    let cpu = renderer.prepare(gpu, &scene);
    let gs = gpu_sorting.prepare(gpu, &scene);
    for yaw in [0.0f32, 100.0, 200.0, 290.0] {
        let mut cam = OrbitCamera::framing(&cpu.bounds, 1.0);
        cam.yaw = yaw.to_radians();
        let view = cam.view(1.0);
        let a = renderer.render_image(gpu, &cpu, &view, &settings, 128, 128);
        let b = gpu_sorting.render_image(gpu, &gs, &view, &settings, 128, 128);
        let worst = a.pixels.iter().zip(&b.pixels).map(|(x, y)| x.abs_diff(*y)).max().unwrap_or(0);
        assert!(worst <= 2, "yaw {yaw}: GPU and CPU sorts differ by {worst}");
    }
}

#[test]
fn khronos_samples() {
    let Some(mut ctx) = ctx() else { return };
    let thumb = RenderSettings::thumbnail();
    let cases: [(&str, Camera); 12] = [
        ("DamagedHelmet", three_quarter),
        ("MetalRoughSpheres", front),
        ("NormalTangentTest", front),
        ("TextureTransformTest", front),
        ("AlphaBlendModeTest", front),
        ("ClearCoatTest", front),
        ("CesiumMan", three_quarter),
        ("NegativeScaleTest", front),
        ("TransmissionTest", front),
        ("TransmissionRoughnessTest", front),
        ("AttenuationTest", front),
        ("DragonAttenuation", front),
    ];
    let mut failures = Vec::new();
    for (name, view) in cases {
        let Some(scene) = load_khronos(name) else {
            eprintln!("skipping {name}: not in local/assets or the glTF loader failed");
            continue;
        };
        let img = render(&mut ctx, &scene, view, &thumb);
        if let Err(e) = compare(&format!("khronos-{name}"), &img) {
            failures.push(e);
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// The viewer's texture mode: a texture with alpha over the checkerboard,
/// magnified with hard texel edges, its alpha channel alone, a face of a
/// cubemap at a lower mip, and an HDR texture exposed down two stops.
#[test]
fn texture_mode() {
    use slerp_scene::Texture;
    use slerp_scene::textures::{ColorSpace, Level, Pixels, TextureKind};
    let Some(mut ctx) = ctx() else { return };
    let Ctx { gpu, renderer } = &mut *ctx;
    let mut failures = Vec::new();

    // 1. RGBA with an alpha ramp and a coloured grid.
    let mut px = Vec::new();
    for y in 0..32u32 {
        for x in 0..64u32 {
            let grid = x % 8 == 0 || y % 8 == 0;
            px.extend_from_slice(&[(x * 4) as u8, (y * 8) as u8, if grid { 255 } else { 60 }, (x * 4 + 3) as u8]);
        }
    }
    let rgba = Texture::from_rgba8(64, 32, px, ColorSpace::Srgb);
    let t = renderer.upload_texture(gpu, &rgba).unwrap();
    assert_eq!((t.width(), t.height(), t.mips(), t.file_mips()), (64, 32, 7, 1));
    let fit = TextureDrawParams::fit(&t, (128, 128));
    let zoomed = TextureDrawParams { zoom: 12.0, center: [6.0, 6.0], ..fit };
    let alpha = TextureDrawParams { channels: [false, false, false, true], ..fit };
    for (name, p) in [("texture-fit", fit), ("texture-zoom", zoomed), ("texture-alpha", alpha)] {
        let img = renderer.draw_texture_image(gpu, &t, &p, 128, 128);
        if let Err(e) = compare(name, &img) {
            failures.push(e);
        }
    }

    // 2. A cubemap whose faces are flat colours, two mips each.
    let colors = [
        [255u8, 0, 0, 255],
        [0, 255, 0, 255],
        [0, 0, 255, 255],
        [255, 255, 0, 255],
        [0, 255, 255, 255],
        [255, 0, 255, 255],
    ];
    let level = |size: u32| Level {
        width: size,
        height: size,
        depth: 1,
        surfaces: colors.iter().map(|c| Pixels::Rgba8(c.repeat((size * size) as usize))).collect(),
    };
    let mut cube = Texture::from_rgba8(1, 1, vec![0; 4], ColorSpace::Srgb);
    cube.kind = TextureKind::Cube;
    cube.faces = 6;
    cube.levels = vec![level(8), level(4)];
    let t = renderer.upload_texture(gpu, &cube).unwrap();
    assert_eq!((t.layers(), t.mips()), (6, 2));
    let p = TextureDrawParams { face: 4, mip: Some(1), ..TextureDrawParams::fit(&t, (32, 32)) };
    let img = renderer.draw_texture_image(gpu, &t, &p, 32, 32);
    let mid = ((16 * 32 + 16) * 4) as usize;
    assert_eq!(&img.pixels[mid..mid + 4], &[0, 255, 255, 255], "face +Z should be cyan");

    // 3. HDR: a value of 4.0 exposed down two stops shows as 1.0 untouched
    //    by tone mapping when tone mapping is off.
    let hdr = Texture::from_rgba32f(4, 4, [4.0f32, 1.0, 0.25, 1.0].repeat(16));
    let t = renderer.upload_texture(gpu, &hdr).unwrap();
    assert!(t.is_hdr());
    let p = TextureDrawParams { exposure: -2.0, tonemap: Tonemap::None, ..TextureDrawParams::fit(&t, (16, 16)) };
    let img = renderer.draw_texture_image(gpu, &t, &p, 16, 16);
    let mid = ((8 * 16 + 8) * 4) as usize;
    let c = &img.pixels[mid..mid + 4];
    // 1.0, 0.25 and 0.0625 encode as 255, 137 and 70.
    assert!(c[0] == 255 && c[1].abs_diff(137) <= 2 && c[2].abs_diff(70) <= 2 && c[3] == 255, "HDR pixel {c:?}");
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// A real Quake map: lightmaps, liquids and sky from LibreQuake, seen from
/// a high three-quarter view.  Sky surfaces must not count in the bounds,
/// since maps often sit inside a sky box far larger than the play area.
#[test]
fn quake_map() {
    let Some(mut ctx) = ctx() else { return };
    let path = assets().join("quake/librequake/id1/maps/lqdm1.bsp");
    let Ok(bytes) = std::fs::read(&path) else {
        eprintln!("skipping quake_map: {} is missing", path.display());
        return;
    };
    let format = slerp_quake::sniff(&path, &bytes[..bytes.len().min(64)]).expect("a BSP");
    let Ok(Asset::Scene(mut scene)) = slerp_quake::load(format, &path, &bytes, &Limits::thumbnail()) else {
        panic!("lqdm1 didn't load");
    };
    let Ctx { gpu, renderer } = &mut *ctx;
    let gs = renderer.prepare(gpu, &scene);
    let all = scene.bounds(&Pose::rest(&scene));
    assert!(gs.bounds.size().length() <= all.size().length() + 1e-3);
    let mut cam = OrbitCamera::framing(&gs.bounds, 1.0);
    cam.yaw = 35f32.to_radians();
    cam.pitch = 45f32.to_radians();
    cam.frame(&gs.bounds, 1.0);
    let settings = RenderSettings { ground_shadow: false, time: 2.0, ..RenderSettings::thumbnail() };
    let img = renderer.render_image(gpu, &gs, &cam.view(1.0), &settings, 256, 256);
    compare("quake-lqdm1", &img).unwrap();

    // A sky whose texture is missing (its WAD wasn't found) shows the
    // environment instead.
    for m in &mut scene.materials {
        if m.effect == slerp_scene::Effect::Sky {
            m.base_color_texture = None;
        }
    }
    let gs = renderer.prepare(gpu, &scene);
    let img = renderer.render_image(gpu, &gs, &cam.view(1.0), &settings, 256, 256);
    compare("quake-lqdm1-no-sky-texture", &img).unwrap();
}

//endregion

/*┌─────────────────╮
| Behaviour       |
└─────────────────╯*/
//region Behaviour

/// A transparent background stays transparent, and a half-transparent
/// pane over nothing comes out with its own colour at straight alpha, not
/// darkened by premultiplication.
#[test]
fn transparent_background_is_straight_alpha() {
    let Some(mut ctx) = ctx() else { return };
    let mut scene = scenes::alpha();
    // Keep only the glass pane, lit as unlit so its colour is exact.
    scene.nodes[0].visible = false;
    scene.nodes[2].visible = false;
    scene.materials[1].unlit = true;
    scene.materials[1].base_color = glam::Vec4::new(0.2, 0.5, 0.8, 0.5);
    let settings = RenderSettings { shadows: false, tonemap: Tonemap::None, ..RenderSettings::thumbnail() };
    let img = render(&mut ctx, &scene, front, &settings);
    let px = |x: u32, y: u32| {
        let i = ((y * img.width + x) * 4) as usize;
        [img.pixels[i], img.pixels[i + 1], img.pixels[i + 2], img.pixels[i + 3]]
    };
    assert_eq!(px(2, 2), [0, 0, 0, 0], "corner should be transparent");
    let c = px(128, 128);
    // sRGB encodings of 0.2, 0.5 and 0.8 are 124, 188 and 231.
    let want = [124u8, 188, 231, 128];
    for k in 0..4 {
        assert!(c[k].abs_diff(want[k]) <= 3, "centre pixel {c:?}, wanted about {want:?}");
    }
}

/// The viewer hands the renderer egui's texture, which may not be sRGB;
/// the shader then encodes itself, and the picture must match.
#[test]
fn non_srgb_targets_match() {
    let Some(mut ctx) = ctx() else { return };
    let Ctx { gpu, renderer } = &mut *ctx;
    let scene = scenes::shapes();
    let settings =
        RenderSettings { background: Background::Color([0.2, 0.2, 0.21, 1.0]), ..RenderSettings::thumbnail() };
    let gs = renderer.prepare(gpu, &scene);
    let view = three_quarter(&gs);
    let srgb = renderer.render_image(gpu, &gs, &view, &settings, 128, 128);

    let mut unorm = Renderer::with_config(gpu, wgpu::TextureFormat::Bgra8Unorm, RendererConfig::thumbnail());
    let gs2 = unorm.prepare(gpu, &scene);
    let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: None,
        size: wgpu::Extent3d { width: 128, height: 128, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Bgra8Unorm,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    unorm.render(gpu, &gs2, &view, &settings, &texture.create_view(&Default::default()), (128, 128));
    let buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: 128 * 128 * 4,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    encoder.copy_texture_to_buffer(
        texture.as_image_copy(),
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(512), rows_per_image: None },
        },
        wgpu::Extent3d { width: 128, height: 128, depth_or_array_layers: 1 },
    );
    gpu.queue.submit([encoder.finish()]);
    buffer.slice(..).map_async(wgpu::MapMode::Read, |_| {});
    gpu.device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
    let data = buffer.slice(..).get_mapped_range().to_vec();
    let mut bad = 0;
    for (a, b) in srgb.pixels.chunks_exact(4).zip(data.chunks_exact(4)) {
        let b = [b[2], b[1], b[0], b[3]];
        if a.iter().zip(&b).any(|(x, y)| x.abs_diff(*y) > 6) {
            bad += 1;
        }
    }
    // Edges differ: a unorm target resolves multisampling in display
    // space rather than in linear light.
    assert!(bad < 128 * 128 / 25, "{bad} pixels differ between sRGB and unorm targets");
}

/// Malformed geometry draws what it can instead of failing: indices past
/// the vertices, attribute arrays of the wrong length, joints past the
/// skin, NaN positions, and an empty scene.
#[test]
fn malformed_scenes_render() {
    let Some(mut ctx) = ctx() else { return };
    let mut scene = scenes::skinned();
    let prim = &mut scene.meshes[0].primitives[0];
    prim.indices.extend_from_slice(&[0, 1, 999_999]);
    prim.uv0.truncate(3);
    prim.tangents = vec![glam::Vec4::X; 5];
    prim.joints[0] = [60_000, 2, 3, 4];
    prim.positions[1] = glam::Vec3::NAN;
    prim.targets.push(slerp_scene::MorphTarget { positions: vec![glam::Vec3::ONE; 2], normals: vec![] });
    scene.nodes[0].weights = vec![1.0, 5.0, 3.0];
    scene.materials[0].roughness = f32::NAN;
    let settings = RenderSettings::thumbnail();
    let img = render(&mut ctx, &scene, three_quarter, &settings);
    assert!(img.pixels.chunks_exact(4).any(|p| p[3] > 0), "nothing drawn");

    let empty = Scene::default();
    let img = render(&mut ctx, &empty, three_quarter, &settings);
    assert!(img.pixels.chunks_exact(4).all(|p| p[3] == 0));
}

/// A node with a negative determinant mirrors its mesh and flips its
/// winding: a single-sided quad facing +Z, mirrored in Z, faces -Z, so it
/// shows from behind and not from the front.
#[test]
fn mirrored_nodes_flip_winding() {
    let Some(mut ctx) = ctx() else { return };
    let mut scene = scenes::alpha();
    scene.nodes[1].visible = false;
    scene.nodes[2].visible = false;
    scene.nodes[0].transform.scale = glam::Vec3::new(2.0, 2.0, -2.0);
    let settings = RenderSettings { shadows: false, ground_shadow: false, ..RenderSettings::thumbnail() };
    let covered = |img: &Image8| img.pixels.chunks_exact(4).filter(|p| p[3] > 0).count();
    let from_front = render(&mut ctx, &scene, front, &settings);
    let from_behind = render(
        &mut ctx,
        &scene,
        |gs| {
            let mut cam = OrbitCamera::framing(&gs.bounds, 1.0);
            cam.yaw = std::f32::consts::PI;
            cam.pitch = 0.0;
            cam.frame(&gs.bounds, 1.0);
            cam.view(1.0)
        },
        &settings,
    );
    assert_eq!(covered(&from_front), 0, "a mirrored quad showed its back");
    assert!(covered(&from_behind) > 5000, "a mirrored quad vanished from its front");
}

/// A file with a skeleton and animation but no meshes (FBX animation
/// files) renders as an empty picture, or as its bones with the skeleton
/// overlay.
#[test]
fn skeleton_only_scene() {
    let Some(mut ctx) = ctx() else { return };
    let mut scene = scenes::skinned();
    scene.meshes.clear();
    scene.nodes[0].mesh = None;
    let settings = RenderSettings::thumbnail();
    let img = render(&mut ctx, &scene, three_quarter, &settings);
    assert!(img.pixels.chunks_exact(4).all(|p| p[3] == 0));
    let Ctx { gpu, renderer } = &mut *ctx;
    let gs = renderer.prepare(gpu, &scene);
    assert!(gs.bounds.is_empty());
    let mut cam =
        OrbitCamera::framing(&slerp_scene::Aabb { min: glam::Vec3::splat(-1.0), max: glam::Vec3::splat(2.0) }, 1.0);
    cam.frame(&slerp_scene::Aabb { min: glam::Vec3::splat(-1.0), max: glam::Vec3::splat(2.0) }, 1.0);
    let bones = RenderSettings { overlays: Overlays { skeleton: true, ..Overlays::default() }, ..settings };
    let img = renderer.render_image(gpu, &gs, &cam.view(1.0), &bones, 128, 128);
    assert!(img.pixels.chunks_exact(4).any(|p| p[3] > 0), "no bones drawn");
}

/// Animation moves what's drawn: posing the skinned tube changes the
/// picture and the bounds, and its node transform (5 m off) is ignored.
#[test]
fn poses_update_draws() {
    let Some(mut ctx) = ctx() else { return };
    let Ctx { gpu, renderer } = &mut *ctx;
    let scene = scenes::skinned();
    let mut gs = renderer.prepare(gpu, &scene);
    assert!(gs.bounds.max.x < 2.0, "skinned mesh followed its node: {:?}", gs.bounds);
    let view = three_quarter(&gs);
    let settings = RenderSettings::thumbnail();
    let rest = renderer.render_image(gpu, &gs, &view, &settings, 96, 96);
    let mut pose = Pose::rest(&scene);
    pose.local[2].rotation = glam::Quat::from_rotation_z(0.9);
    pose.weights[0] = vec![0.0];
    pose.update_world(&scene);
    renderer.update_pose(gpu, &mut gs, &scene, &pose);
    let bent = renderer.render_image(gpu, &gs, &view, &settings, 96, 96);
    let changed = rest.pixels.chunks_exact(4).zip(bent.pixels.chunks_exact(4)).filter(|(a, b)| a != b).count();
    assert!(changed > 200, "only {changed} pixels changed");
    assert!(gs.bounds.min.x < -0.5, "bounds didn't follow the pose: {:?}", gs.bounds);
}

/// Rewriting a material in place, as the viewer's Tiling and Displacement
/// sliders do every frame of a drag, draws exactly what preparing the
/// changed scene draws, shadows included (they are cached by pose).
#[test]
fn updated_materials_draw_like_prepared_ones() {
    let Some(mut ctx) = ctx() else { return };
    let Ctx { gpu, renderer } = &mut *ctx;
    let mut scene = scenes::displaced();
    let mut gs = renderer.prepare(gpu, &scene);
    let view = three_quarter(&gs);
    let settings = RenderSettings::thumbnail();
    let before = renderer.render_image(gpu, &gs, &view, &settings, 96, 96);
    let m = &mut scene.materials[0];
    m.height_scale = 0.12;
    let scale = glam::Vec2::splat(3.0);
    m.height_texture.as_mut().unwrap().transform =
        Some(slerp_scene::UvTransform { offset: glam::Vec2::ZERO, rotation: 0.0, scale });
    renderer.update_material(gpu, &mut gs, &scene, 0);
    let live = renderer.render_image(gpu, &gs, &view, &settings, 96, 96);
    let fresh = renderer.prepare(gpu, &scene);
    let prepared = renderer.render_image(gpu, &fresh, &view, &settings, 96, 96);
    assert!(live.pixels != before.pixels, "the update changed nothing");
    let differ = live.pixels.chunks_exact(4).zip(prepared.pixels.chunks_exact(4)).filter(|(a, b)| a != b).count();
    assert_eq!(differ, 0, "{differ} pixels differ from a freshly prepared scene");
}

/// Where world point `p` lands in a `size` x `size` picture seen through
/// `view`.
fn pixel_of(view: &View, p: glam::Vec3, size: u32) -> (u32, u32) {
    let c = view.proj * view.view * p.extend(1.0);
    let ndc = c.truncate() / c.w;
    let x = ((ndc.x * 0.5 + 0.5) * size as f32) as u32;
    let y = ((0.5 - ndc.y * 0.5) * size as f32) as u32;
    (x.min(size - 1), y.min(size - 1))
}

/// The mean of a 3x3 block's red, green and blue around a pixel.
fn mean_rgb(img: &Image8, (x, y): (u32, u32)) -> glam::Vec3 {
    let mut sum = glam::Vec3::ZERO;
    for dy in 0..3 {
        for dx in 0..3 {
            let (px, py) =
                ((x + dx).saturating_sub(1).min(img.width - 1), (y + dy).saturating_sub(1).min(img.height - 1));
            let i = ((py * img.width + px) * 4) as usize;
            sum += glam::Vec3::new(img.pixels[i] as f32, img.pixels[i + 1] as f32, img.pixels[i + 2] as f32);
        }
    }
    sum / 9.0
}

/// Ambient occlusion darkens the floor where a cube stands on it, and
/// leaves open floor and the cube's own flat faces alone.  Lit by the
/// environment only, since occlusion doesn't touch the key light.
#[test]
fn ambient_occlusion_darkens_creases_only() {
    use slerp_scene::{Material, Mesh, Shape, Transform};
    let Some(mut ctx) = ctx() else { return };
    let mut scene = Scene::default();
    let mut add = |prim, t: Transform| {
        let n = scene.add_mesh_node(Mesh { name: "m".into(), primitives: vec![prim], target_names: vec![] });
        scene.nodes[n].transform = t;
    };
    add(Shape::Plane.primitive(1), Transform { scale: glam::Vec3::splat(2.0), ..Transform::IDENTITY });
    let cube = Transform { translation: glam::Vec3::Y * 0.25, scale: glam::Vec3::splat(0.5), ..Transform::IDENTITY };
    add(Shape::Cube.primitive(1), cube);
    scene.materials.push(Material::default());
    let Ctx { gpu, renderer } = &mut *ctx;
    let gs = renderer.prepare(gpu, &scene);
    let view = three_quarter(&gs);
    let base = RenderSettings {
        key_light: 0.0,
        ground_shadow: false,
        background: Background::Color([0.0, 0.0, 0.0, 1.0]),
        ..RenderSettings::thumbnail()
    };
    let off = renderer.render_image(gpu, &gs, &view, &RenderSettings { ao: false, ..base }, 192, 192);
    let on = renderer.render_image(gpu, &gs, &view, &RenderSettings { ao: true, ..base }, 192, 192);
    let at = |p: glam::Vec3| {
        let px = pixel_of(&view, p, 192);
        (mean_rgb(&off, px).x, mean_rgb(&on, px).x)
    };
    let (crease_off, crease_on) = at(glam::Vec3::new(0.0, 0.0, 0.27));
    assert!(crease_on < crease_off * 0.92, "the crease should darken: {crease_off} to {crease_on}");
    for p in [glam::Vec3::new(0.0, 0.0, 0.9), glam::Vec3::new(0.0, 0.25, 0.251), glam::Vec3::new(0.0, 0.501, 0.0)] {
        let (a, b) = at(p);
        assert!((a - b).abs() <= 3.0, "open surface at {p} changed: {a} to {b}");
    }
}

/// A clear pane shows the red wall behind it; a volume of blue-absorbing
/// glass makes it dark, as Beer's law says; without transmission the pane
/// is just white.
#[test]
fn transmission_shows_what_is_behind() {
    use slerp_scene::{Material, Mesh, Shape, Transform};
    let Some(mut ctx) = ctx() else { return };
    let wall = |scene: &mut Scene, z: f32, material: usize| {
        let mut quad = Shape::Plane.primitive(1);
        for p in &mut quad.positions {
            *p = glam::Vec3::new(p.x, -p.z, 0.0);
        }
        quad.normals.iter_mut().for_each(|n| *n = glam::Vec3::Z);
        quad.tangents.clear();
        quad.material = Some(material);
        let n = scene.add_mesh_node(Mesh { name: "wall".into(), primitives: vec![quad], target_names: vec![] });
        scene.nodes[n].transform = Transform { translation: glam::Vec3::Z * z, ..Transform::IDENTITY };
    };
    let render = |ctx: &mut Ctx, glass: Material| {
        let mut scene = Scene::default();
        let red = Material { base_color: glam::Vec4::new(0.9, 0.02, 0.02, 1.0), unlit: true, ..Default::default() };
        scene.materials = vec![red, glass];
        wall(&mut scene, -0.5, 0);
        wall(&mut scene, 0.0, 1);
        let Ctx { gpu, renderer } = ctx;
        let gs = renderer.prepare(gpu, &scene);
        let view = front(&gs);
        let settings = RenderSettings { ground_shadow: false, key_light: 0.0, ..RenderSettings::thumbnail() };
        let img = renderer.render_image(gpu, &gs, &view, &settings, 64, 64);
        mean_rgb(&img, (32, 32))
    };
    let white = Material { base_color: glam::Vec4::ONE, roughness: 0.3, double_sided: true, ..Default::default() };
    let opaque = render(&mut ctx, white.clone());
    let clear = render(&mut ctx, Material { transmission: 1.0, ..white.clone() });
    let absorbing = render(
        &mut ctx,
        Material {
            transmission: 1.0,
            thickness: 0.5,
            attenuation_color: glam::Vec3::new(0.05, 0.1, 1.0),
            attenuation_distance: 0.1,
            ..white
        },
    );
    assert!((opaque.x - opaque.y).abs() < 20.0, "an opaque white pane: {opaque}");
    assert!(clear.x > 120.0 && clear.x > clear.y * 3.0, "the wall should show through: {clear}");
    assert!(absorbing.x < clear.x * 0.5, "blue glass should absorb the red: {absorbing} vs {clear}");
}

/// Points draw as round dots: about a disc's worth of pixels, a quarter
/// of them more with twice the size.  On screen they keep their size as
/// the camera comes closer; sized in the world they grow.
#[test]
fn points_are_round_dots() {
    use slerp_scene::{Material, Mesh, Primitive, Topology};
    let Some(mut ctx) = ctx() else { return };
    let mut scene = Scene::default();
    let prim = Primitive {
        positions: vec![glam::Vec3::new(-0.5, 0.0, 0.0), glam::Vec3::new(0.5, 0.0, 0.0)],
        indices: vec![0, 1],
        topology: Topology::Points,
        material: Some(0),
        ..Default::default()
    };
    scene.materials.push(Material { base_color: glam::Vec4::ONE, unlit: true, ..Default::default() });
    scene.add_mesh_node(Mesh { name: "dots".into(), primitives: vec![prim], target_names: vec![] });
    let Ctx { gpu, renderer } = &mut *ctx;
    let gs = renderer.prepare(gpu, &scene);
    // Framing a wider box keeps the dots off the picture's edges.
    let wide = slerp_scene::Aabb { min: glam::Vec3::new(-1.5, -0.5, -0.5), max: glam::Vec3::new(1.5, 0.5, 0.5) };
    let mut cam = OrbitCamera::framing(&wide, 1.0);
    cam.yaw = 0.0;
    cam.pitch = 0.0;
    cam.frame(&wide, 1.0);
    let lit = |img: &Image8| img.pixels.chunks_exact(4).filter(|p| p[0] > 128).count() as f32;
    let settings = RenderSettings {
        background: Background::Color([0.0, 0.0, 0.0, 1.0]),
        ground_shadow: false,
        ao: false,
        point_size: 0.15,
        ..RenderSettings::thumbnail()
    };
    let size = 160;
    let img = renderer.render_image(gpu, &gs, &cam.view(1.0), &settings, size, size);
    let area = lit(&img) / 2.0;
    // The dot's radius from its pixels across the middle row.
    let row = (size / 2 * size * 4) as usize;
    let across = img.pixels[row..row + size as usize * 4].chunks_exact(4).filter(|p| p[0] > 128).count() as f32 / 2.0;
    let disc = std::f32::consts::PI * (across / 2.0).powi(2);
    assert!(across > 4.0, "dots {across} pixels wide");
    assert!((area / disc - 1.0).abs() < 0.2, "a dot covers {area} pixels, a disc of its width {disc}");
    let double =
        renderer.render_image(gpu, &gs, &cam.view(1.0), &RenderSettings { point_size: 0.3, ..settings }, size, size);
    let ratio = lit(&double) / lit(&img);
    assert!((ratio - 4.0).abs() < 0.6, "twice the size covers {ratio} times the pixels");

    let mut near = cam;
    near.distance *= 0.5;
    let world = RenderSettings { point_world_size: true, ..settings };
    let screen_far = lit(&img);
    let screen_near = lit(&renderer.render_image(gpu, &gs, &near.view(1.0), &settings, size, size));
    assert!((screen_near / screen_far - 1.0).abs() < 0.15, "on screen: {screen_far} then {screen_near}");
    let world_far = lit(&renderer.render_image(gpu, &gs, &cam.view(1.0), &world, size, size));
    let world_near = lit(&renderer.render_image(gpu, &gs, &near.view(1.0), &world, size, size));
    assert!(world_near > world_far * 2.5, "in the world: {world_far} then {world_near}");
}

/// A billboard turns to face the camera about +Y: seen from the front and
/// from the side it covers the same pixels, where a plain quad would show
/// its edge.  Its corners are offsets from the anchor, in metres.
#[test]
fn billboards_face_the_camera() {
    use slerp_scene::{Effect, Material, Mesh, Primitive};
    let Some(mut ctx) = ctx() else { return };
    let mut scene = Scene::default();
    let corners =
        [glam::Vec2::new(-0.5, 0.0), glam::Vec2::new(0.5, 0.0), glam::Vec2::new(0.5, 1.0), glam::Vec2::new(-0.5, 1.0)];
    let prim = Primitive {
        positions: vec![glam::Vec3::ZERO; 4],
        normals: vec![glam::Vec3::Y; 4],
        uv0: vec![glam::Vec2::ZERO; 4],
        uv1: corners.to_vec(),
        indices: vec![0, 1, 2, 0, 2, 3],
        material: Some(0),
        ..Default::default()
    };
    scene.materials.push(Material { unlit: true, double_sided: true, effect: Effect::Billboard, ..Default::default() });
    scene.add_mesh_node(Mesh { name: "sprite".into(), primitives: vec![prim], target_names: vec![] });
    let Ctx { gpu, renderer } = &mut *ctx;
    let gs = renderer.prepare(gpu, &scene);
    let target = slerp_scene::Aabb { min: glam::Vec3::new(-1.0, 0.0, -1.0), max: glam::Vec3::new(1.0, 1.0, 1.0) };
    let settings = RenderSettings {
        background: Background::Color([0.0, 0.0, 0.0, 1.0]),
        ground_shadow: false,
        ..RenderSettings::thumbnail()
    };
    let mut covered = Vec::new();
    for yaw in [0f32, 90.0, 210.0] {
        let mut cam = OrbitCamera::framing(&target, 1.0);
        cam.yaw = yaw.to_radians();
        cam.pitch = 0.0;
        cam.frame(&target, 1.0);
        let img = renderer.render_image(gpu, &gs, &cam.view(1.0), &settings, 96, 96);
        covered.push(img.pixels.chunks_exact(4).filter(|p| p[0] > 64).count() as f32);
    }
    assert!(covered[0] > 300.0, "the sprite covers {} pixels", covered[0]);
    for c in &covered[1..] {
        assert!((c / covered[0] - 1.0).abs() < 0.05, "turned views cover {covered:?}");
    }
}

//endregion
