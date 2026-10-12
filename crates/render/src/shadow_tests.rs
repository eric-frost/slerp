//! The ground's contact shadow, checked on the machine's own GPU rather
//! than lavapipe: it once came out as a solid disc on Metal and Direct3D
//! while Vulkan drew it right.

use crate::{GpuPolicy, RenderSettings, Renderer, RendererConfig, THUMBNAIL_FORMAT};
use slerp_scene::{Material, MaterialAsset, OrbitCamera, Scene, Shape};

/// Converts a half-precision float.
fn f16_to_f32(h: u16) -> f32 {
    let sign = if h & 0x8000 != 0 { -1.0 } else { 1.0 };
    let exp = (h >> 10) & 0x1f;
    let frac = (h & 0x3ff) as f32;
    sign * match exp {
        0 => frac * 2f32.powi(-24),
        31 => f32::INFINITY,
        e => (1.0 + frac / 1024.0) * 2f32.powi(e as i32 - 15),
    }
}

/// Reads the contact map back: one coverage value per texel, rows from
/// the top of the texture.
fn read_contact_map(gpu: &crate::Gpu, renderer: &Renderer) -> (u32, Vec<f32>) {
    let size = renderer.contact_map.width();
    let row = (size * 2).div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT) * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("contact readback"),
        size: (row * size) as u64,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    encoder.copy_texture_to_buffer(
        renderer.contact_map.as_image_copy(),
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(row), rows_per_image: None },
        },
        wgpu::Extent3d { width: size, height: size, depth_or_array_layers: 1 },
    );
    gpu.queue.submit([encoder.finish()]);
    let slice = buffer.slice(..);
    slice.map_async(wgpu::MapMode::Read, |_| {});
    let _ = gpu.device.poll(wgpu::PollType::wait_indefinitely());
    let data = slice.get_mapped_range();
    let mut values = Vec::with_capacity((size * size) as usize);
    for y in 0..size as usize {
        for x in 0..size as usize {
            let i = y * row as usize + x * 2;
            values.push(f16_to_f32(u16::from_le_bytes([data[i], data[i + 1]])));
        }
    }
    (size, values)
}

/// A cube resting on the ground covers the middle of the contact map,
/// fully where its bottom touches the ground, and nothing towards the
/// map's corners, which reach past the cube.
#[test]
fn contact_map_covers_only_what_is_under_the_scene() {
    let Some(gpu) = crate::test_gpu(GpuPolicy::Auto) else {
        eprintln!("skipped, no GPU");
        return;
    };
    let mut renderer = Renderer::with_config(&gpu, THUMBNAIL_FORMAT, RendererConfig::thumbnail());
    let asset = MaterialAsset {
        name: "cube".into(),
        material: Material::default(),
        textures: vec![],
        preview: None,
        info: Default::default(),
    };
    let scene = Scene::material_ball(&asset, Shape::Cube, 1);
    let gs = renderer.prepare(&gpu, &scene);
    let view = OrbitCamera::framing(&gs.bounds, 1.0).view(1.0);
    renderer.render_image(&gpu, &gs, &view, &RenderSettings::thumbnail(), 64, 64);

    let (size, map) = read_contact_map(&gpu, &renderer);
    let at = |x: u32, y: u32| map[(y * size + x) as usize];
    let covered = map.iter().filter(|&&v| v > 0.5).count() as f32 / map.len() as f32;
    let (min, max) = map.iter().fold((f32::MAX, f32::MIN), |(a, b), &v| (a.min(v), b.max(v)));
    let corners = [at(1, 1), at(size - 2, 1), at(1, size - 2), at(size - 2, size - 2)];
    let centre = at(size / 2, size / 2);
    eprintln!(
        "{} ({}): contact map {size}x{size}, centre {centre}, corners {corners:?}, min {min}, max {max}, covered {:.0}%",
        gpu.info.name,
        crate::backend_name(gpu.info.backend),
        covered * 100.0
    );
    // The cube is 1 m across on a ground about 1.6 m across.
    assert!(centre > 0.9, "the cube's bottom should cover the centre fully, got {centre}");
    assert!(corners.iter().all(|&c| c == 0.0), "the corners should be empty, got {corners:?}");
    assert!((0.2..0.6).contains(&covered), "the cube should cover about 40% of the map, got {covered}");
}

/// The ground must read the contact map it was given.  A thin pillar marks
/// only a small spot under itself, so with the key light off (no cast
/// shadow) the ground a little way off stays clear.  Metal and Direct3D
/// once read a material texture there instead, because the lighting group
/// wasn't bound again for the ground's pipeline (see `Layouts::backdrop`),
/// which drew a solid disc.
#[test]
fn ground_reads_the_contact_map() {
    let Some(gpu) = crate::test_gpu(GpuPolicy::Auto) else {
        eprintln!("skipped, no GPU");
        return;
    };
    let mut renderer = Renderer::with_config(&gpu, THUMBNAIL_FORMAT, RendererConfig::thumbnail());
    let asset = MaterialAsset {
        name: "pillar".into(),
        material: Material::default(),
        textures: vec![],
        preview: None,
        info: Default::default(),
    };
    let mut scene = Scene::material_ball(&asset, Shape::Cube, 1);
    scene.nodes[0].transform.scale = glam::Vec3::new(0.1, 2.0, 0.1);
    let gs = renderer.prepare(&gpu, &scene);
    let size = 128;
    let view = OrbitCamera::framing(&gs.bounds, 1.0).view(1.0);
    let settings = RenderSettings { key_light: 0.0, ..RenderSettings::thumbnail() };
    let image = renderer.render_image(&gpu, &gs, &view, &settings, size, size);

    // Ground points 0.3 m from the pillar's axis, well inside the ground's
    // unfaded middle, and the pixels they land on.
    let ground = gs.bounds.min.y;
    let view_proj = view.proj * view.view;
    let mut checked = Vec::new();
    for (x, z) in [(0.3, 0.0), (-0.3, 0.0), (0.0, 0.3), (0.0, -0.3)] {
        let clip = view_proj * glam::Vec4::new(x, ground, z, 1.0);
        let ndc = clip.truncate() / clip.w;
        let px = ((ndc.x * 0.5 + 0.5) * size as f32) as u32;
        let py = ((0.5 - ndc.y * 0.5) * size as f32) as u32;
        if px >= size || py >= size {
            continue;
        }
        let alpha = image.pixels[((py * size + px) * 4 + 3) as usize];
        // Skip a point hidden behind the pillar.
        if alpha < 255 {
            checked.push(((x, z), (px, py), alpha));
        }
    }
    eprintln!("{} ({}): ground alpha {checked:?}", gpu.info.name, crate::backend_name(gpu.info.backend));
    assert!(checked.len() >= 2, "too few ground points visible: {checked:?}");
    assert!(checked.iter().all(|&(_, _, a)| a <= 8), "the ground away from the pillar should be clear: {checked:?}");
}

/// With no light at all a lit surface is black, and the fill light shows a
/// fraction of its own colour: a fill of 1 shows the colour itself and a
/// fill of 0.5 half of it, in linear light.  Without tone mapping, so the
/// values are the colour itself.
#[test]
fn fill_light_shows_a_fraction_of_the_surface_colour() {
    let Some(gpu) = crate::test_gpu(GpuPolicy::Auto) else {
        eprintln!("skipped, no GPU");
        return;
    };
    let mut renderer = Renderer::with_config(&gpu, THUMBNAIL_FORMAT, RendererConfig::thumbnail());
    let asset = MaterialAsset {
        name: "cube".into(),
        material: Material::default(),
        textures: vec![],
        preview: None,
        info: Default::default(),
    };
    let scene = Scene::material_ball(&asset, Shape::Cube, 1);
    let gs = renderer.prepare(&gpu, &scene);
    let size = 64;
    let view = OrbitCamera::framing(&gs.bounds, 1.0).view(1.0);
    let unlit = RenderSettings {
        key_light: 0.0,
        environment_intensity: 0.0,
        ao: false,
        shadows: false,
        ground_shadow: false,
        tonemap: crate::Tonemap::None,
        ..RenderSettings::thumbnail()
    };
    let mut centre = |fill: f32| {
        let settings = RenderSettings { fill, ..unlit };
        let image = renderer.render_image(&gpu, &gs, &view, &settings, size, size);
        let i = ((size / 2 * size + size / 2) * 4) as usize;
        image.pixels[i..i + 3].iter().map(|&c| c as u32).sum::<u32>() / 3
    };
    let (none, half, full) = (centre(0.0), centre(0.5), centre(1.0));
    eprintln!("{}: centre brightness {none}, {half}, {full}", gpu.info.name);
    let to_linear = |v: u32| ((v as f32 / 255.0 + 0.055) / 1.055).powf(2.4);
    let to_display = |l: f32| (255.0 * (1.055 * l.powf(1.0 / 2.4) - 0.055)).round();
    assert!(none < 12, "no light and no fill should be black, got {none}");
    assert!(full > 100, "a fill of 1 should show the surface colour, got {full}");
    let expected = to_display(to_linear(full) * 0.5);
    assert!((half as f32 - expected).abs() <= 3.0, "a fill of 0.5 should show {expected}, got {half}");
}
