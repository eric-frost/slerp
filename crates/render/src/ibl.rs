//! Image-based lighting built on the GPU: the procedural studio or a user's
//! HDR becomes a radiance cube prefiltered for roughness, nine spherical-
//! harmonic irradiance coefficients and a key light direction; plus the
//! split-sum BRDF table, made once per renderer on the CPU.

use crate::resources::{MipGen, Surfaces, upload_texture};
use bytemuck::{Pod, Zeroable};
use glam::Vec3;
use slerp_scene::Texture;
use slerp_scene::textures::{Pixels, TextureKind};
use std::collections::HashMap;
use wgpu::util::DeviceExt;

const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;

/// The studio's key softbox, as in `studio()` in ibl.wgsl, and its light.
pub(crate) fn studio_key_light() -> (Vec3, Vec3) {
    (Vec3::new(-0.24, 0.71, 0.66).normalize(), Vec3::new(1.0, 0.97, 0.93) * 2.0)
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Params {
    a: [u32; 4],
    b: [f32; 4],
}

/// Lighting from one environment.
pub(crate) struct Environment {
    pub radiance_view: wgpu::TextureView,
    /// The source cube at full resolution, for unblurred backgrounds.
    pub sharp_view: wgpu::TextureView,
    /// Levels in the radiance cube; roughness r reads level r * (count - 1).
    pub mip_count: u32,
    pub sh: wgpu::Buffer,
    /// Towards the key light, in the environment's own frame.
    pub key_dir: Vec3,
    pub key_color: Vec3,
}

/// Where an environment comes from.
pub(crate) enum Source<'a> {
    Studio,
    /// A panorama or a cubemap, HDR or not.
    Texture(&'a Texture),
}

/// Sizes and sample counts; thumbnails use small ones.
#[derive(Clone, Copy, Debug)]
pub(crate) struct IblSizes {
    /// Face size of the cube the source is first drawn into.
    pub source: u32,
    /// Face size of the radiance cube's sharpest level.
    pub radiance: u32,
    /// GGX samples per texel when prefiltering.
    pub samples: u32,
}

pub(crate) struct Ibl {
    module: wgpu::ShaderModule,
    pipelines: HashMap<&'static str, wgpu::ComputePipeline>,
    sampler_cube: wgpu::Sampler,
    sampler_equirect: wgpu::Sampler,
}

/// A resource bound at a binding number for one dispatch.
enum Res<'a> {
    View(&'a wgpu::TextureView),
    Sampler(&'a wgpu::Sampler),
    Buffer(&'a wgpu::Buffer),
}

impl Ibl {
    pub fn new(device: &wgpu::Device) -> Ibl {
        let linear = |u, v| {
            device.create_sampler(&wgpu::SamplerDescriptor {
                label: Some("ibl"),
                address_mode_u: u,
                address_mode_v: v,
                mag_filter: wgpu::FilterMode::Linear,
                min_filter: wgpu::FilterMode::Linear,
                mipmap_filter: wgpu::MipmapFilterMode::Linear,
                ..Default::default()
            })
        };
        Ibl {
            module: crate::shaders::module(device, "ibl", crate::shaders::IBL),
            pipelines: HashMap::new(),
            sampler_cube: linear(wgpu::AddressMode::ClampToEdge, wgpu::AddressMode::ClampToEdge),
            sampler_equirect: linear(wgpu::AddressMode::Repeat, wgpu::AddressMode::ClampToEdge),
        }
    }

    fn pipeline(&mut self, gpu: &crate::Gpu, entry: &'static str) -> wgpu::ComputePipeline {
        let module = &self.module;
        self.pipelines
            .entry(entry)
            .or_insert_with(|| {
                let step = crate::trace::Step::with("compile", entry);
                let pipeline = gpu.device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                    label: Some(entry),
                    layout: None,
                    module,
                    entry_point: Some(entry),
                    // `cs_sh` writes all of its workgroup memory before
                    // reading any, so WebGPU's zeroing is wasted, and naga's
                    // HLSL for it makes Direct3D's FXC compiler take 23 s on
                    // that shader (and WARP 33 s to run it) instead of 0.2 s.
                    compilation_options: wgpu::PipelineCompilationOptions {
                        zero_initialize_workgroup_memory: false,
                        ..Default::default()
                    },
                    cache: gpu.pipeline_cache.as_ref(),
                });
                step.done(&gpu.device);
                pipeline
            })
            .clone()
    }

    /// Records one dispatch of `entry` over `groups`, binding `params` at 0
    /// and `res` at their binding numbers.  When tracing, it is submitted
    /// and timed on its own instead, which keeps the order since `build`
    /// records nothing but dispatches.
    #[allow(clippy::too_many_arguments)]
    fn dispatch(
        &mut self,
        gpu: &crate::Gpu,
        encoder: &mut wgpu::CommandEncoder,
        entry: &'static str,
        params: Params,
        res: &[(u32, Res)],
        groups: (u32, u32, u32),
    ) {
        let pipeline = self.pipeline(gpu, entry);
        let buffer = gpu.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("ibl params"),
            contents: bytemuck::bytes_of(&params),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let mut entries = vec![wgpu::BindGroupEntry { binding: 0, resource: buffer.as_entire_binding() }];
        for (binding, r) in res {
            entries.push(wgpu::BindGroupEntry {
                binding: *binding,
                resource: match r {
                    Res::View(v) => wgpu::BindingResource::TextureView(v),
                    Res::Sampler(s) => wgpu::BindingResource::Sampler(s),
                    Res::Buffer(b) => b.as_entire_binding(),
                },
            });
        }
        let bind = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some(entry),
            layout: &pipeline.get_bind_group_layout(0),
            entries: &entries,
        });
        let record = |encoder: &mut wgpu::CommandEncoder| {
            let mut pass =
                encoder.begin_compute_pass(&wgpu::ComputePassDescriptor { label: Some(entry), timestamp_writes: None });
            pass.set_pipeline(&pipeline);
            pass.set_bind_group(0, &bind, &[]);
            pass.dispatch_workgroups(groups.0, groups.1, groups.2);
        };
        if crate::trace::enabled() {
            let step = crate::trace::Step::with("run", entry);
            let mut own = gpu.device.create_command_encoder(&Default::default());
            record(&mut own);
            gpu.queue.submit([own.finish()]);
            step.done(&gpu.device);
        } else {
            record(encoder);
        }
    }

    /// Builds the lighting for `source`.
    pub fn build(&mut self, gpu: &crate::Gpu, mipgen: &mut MipGen, source: Source, sizes: IblSizes) -> Environment {
        let device = &gpu.device;
        let mut encoder = device.create_command_encoder(&Default::default());

        // 1. Draw the source into a cube with a full mip chain.
        let src_size = sizes.source.max(16);
        let src_mips = 32 - src_size.leading_zeros();
        let src = cube_texture(device, src_size, src_mips, "environment source");
        let groups = |size: u32| (size.div_ceil(8), size.div_ceil(8), 6);
        let level_view = |t: &wgpu::Texture, mip: u32| {
            t.create_view(&wgpu::TextureViewDescriptor {
                dimension: Some(wgpu::TextureViewDimension::D2Array),
                base_mip_level: mip,
                mip_level_count: Some(1),
                ..Default::default()
            })
        };
        let dst0 = level_view(&src, 0);
        let (key_dir, key_color) = match source {
            Source::Studio => {
                let p = Params { a: [src_size, 0, 0, 0], b: [0.0; 4] };
                self.dispatch(gpu, &mut encoder, "cs_studio", p, &[(1, Res::View(&dst0))], groups(src_size));
                studio_key_light()
            }
            Source::Texture(tex) => {
                let key = estimate_key_light(tex);
                // Shrink large panoramas first: nothing past four texels
                // per cube texel is visible.
                let up = upload_texture(gpu, mipgen, tex, Surfaces::All, true, Some(src_size * 4));
                match up {
                    Ok(up) if tex.kind == TextureKind::Cube && up.layers >= 6 => {
                        let cube = up.texture.create_view(&wgpu::TextureViewDescriptor {
                            dimension: Some(wgpu::TextureViewDimension::Cube),
                            array_layer_count: Some(6),
                            format: up.view_srgb.as_ref().map(|_| wgpu::TextureFormat::Rgba8UnormSrgb),
                            ..Default::default()
                        });
                        let lod = (up.width as f32 / src_size as f32).log2().max(0.0);
                        let p = Params { a: [src_size, 0, 0, 0], b: [0.0, lod, 0.0, 0.0] };
                        let res = [
                            (1, Res::View(&dst0)),
                            (3, Res::View(&cube)),
                            (4, Res::Sampler(&self.sampler_cube.clone())),
                        ];
                        self.dispatch(gpu, &mut encoder, "cs_from_cube", p, &res, groups(src_size));
                    }
                    Ok(up) => {
                        let pano = up.texture.create_view(&wgpu::TextureViewDescriptor {
                            dimension: Some(wgpu::TextureViewDimension::D2),
                            array_layer_count: Some(1),
                            format: up.view_srgb.as_ref().map(|_| wgpu::TextureFormat::Rgba8UnormSrgb),
                            ..Default::default()
                        });
                        let lod = (up.width as f32 / (4.0 * src_size as f32)).log2().max(0.0);
                        let p = Params { a: [src_size, 0, 0, 0], b: [0.0, lod, 0.0, 0.0] };
                        let sampler = self.sampler_equirect.clone();
                        let res = [(1, Res::View(&dst0)), (2, Res::View(&pano)), (4, Res::Sampler(&sampler))];
                        self.dispatch(gpu, &mut encoder, "cs_equirect", p, &res, groups(src_size));
                    }
                    Err(_) => {
                        let p = Params { a: [src_size, 0, 0, 0], b: [0.0; 4] };
                        self.dispatch(gpu, &mut encoder, "cs_studio", p, &[(1, Res::View(&dst0))], groups(src_size));
                    }
                }
                key
            }
        };
        for mip in 1..src_mips {
            let size = (src_size >> mip).max(1);
            let from = level_view(&src, mip - 1);
            let to = level_view(&src, mip);
            let p = Params { a: [size, 0, 0, 0], b: [0.0; 4] };
            self.dispatch(
                gpu,
                &mut encoder,
                "cs_downsample",
                p,
                &[(1, Res::View(&to)), (5, Res::View(&from))],
                groups(size),
            );
        }

        // 2. Prefilter the radiance cube, one roughness per level, down to
        //    4x4 faces.
        let size = sizes.radiance.clamp(16, src_size);
        let mip_count = (32 - size.leading_zeros()).saturating_sub(2).max(1);
        let radiance = cube_texture(device, size, mip_count, "radiance");
        let src_cube = src.create_view(&wgpu::TextureViewDescriptor {
            dimension: Some(wgpu::TextureViewDimension::Cube),
            ..Default::default()
        });
        let sampler = self.sampler_cube.clone();
        for mip in 0..mip_count {
            let s = (size >> mip).max(1);
            let roughness = if mip_count > 1 { mip as f32 / (mip_count - 1) as f32 } else { 0.0 };
            let p = Params { a: [s, sizes.samples, 0, 0], b: [roughness, 0.0, src_size as f32, src_mips as f32] };
            let to = level_view(&radiance, mip);
            let res = [(1, Res::View(&to)), (3, Res::View(&src_cube)), (4, Res::Sampler(&sampler))];
            self.dispatch(gpu, &mut encoder, "cs_prefilter", p, &res, groups(s));
        }

        // 3. Irradiance from the 16x16 level.
        let sh_mip = src_mips.saturating_sub(5).min(src_mips - 1);
        let sh_size = (src_size >> sh_mip).max(1);
        let sh = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("sh"),
            size: 9 * 16,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::UNIFORM,
            mapped_at_creation: false,
        });
        let from = level_view(&src, sh_mip);
        let p = Params { a: [sh_size, 0, 0, 0], b: [0.0; 4] };
        self.dispatch(gpu, &mut encoder, "cs_sh", p, &[(5, Res::View(&from)), (6, Res::Buffer(&sh))], (1, 1, 1));
        gpu.queue.submit([encoder.finish()]);

        let radiance_view = radiance.create_view(&wgpu::TextureViewDescriptor {
            dimension: Some(wgpu::TextureViewDimension::Cube),
            ..Default::default()
        });
        let sharp_view = src_cube;
        Environment { radiance_view, sharp_view, mip_count, sh, key_dir, key_color }
    }
}

/// The split-sum BRDF table: the scale (red) and bias (green) applied to
/// F0, by N.V across and perceptual roughness down, integrated with GGX
/// importance sampling and height-correlated Smith visibility as in the
/// Khronos sample renderer.  It is small and the same for every scene, and
/// computing it here costs about as much as one GPU dispatch would on a
/// real GPU, while lavapipe would spend 50 ms on that dispatch.
pub(crate) fn brdf_lut(gpu: &crate::Gpu, size: u32, samples: u32) -> wgpu::TextureView {
    let mut data: Vec<half::f16> = Vec::with_capacity((size * size * 2) as usize);
    let points: Vec<(f32, f32)> =
        (0..samples).map(|i| (i as f32 / samples as f32, i.reverse_bits() as f32 * 2.328_306_4e-10)).collect();
    for y in 0..size {
        let roughness = (y as f32 + 0.5) / size as f32;
        let a = roughness * roughness;
        for x in 0..size {
            let n_dot_v = (x as f32 + 0.5) / size as f32;
            let v = Vec3::new((1.0 - n_dot_v * n_dot_v).sqrt(), 0.0, n_dot_v);
            let (mut sa, mut sb) = (0.0f32, 0.0f32);
            for &(u1, u2) in &points {
                let phi = std::f32::consts::TAU * u1;
                let cos_t = ((1.0 - u2) / (1.0 + (a * a - 1.0) * u2)).sqrt();
                let sin_t = (1.0 - cos_t * cos_t).max(0.0).sqrt();
                let h = Vec3::new(sin_t * phi.cos(), sin_t * phi.sin(), cos_t);
                let l = 2.0 * v.dot(h) * h - v;
                let n_dot_l = l.z.clamp(0.0, 1.0);
                let n_dot_h = h.z.clamp(0.0, 1.0);
                let v_dot_h = v.dot(h).clamp(0.0, 1.0);
                if n_dot_l > 0.0 {
                    let a2 = a * a;
                    let ggxv = n_dot_l * (n_dot_v * n_dot_v * (1.0 - a2) + a2).sqrt();
                    let ggxl = n_dot_v * (n_dot_l * n_dot_l * (1.0 - a2) + a2).sqrt();
                    let vis = if ggxv + ggxl > 0.0 { 0.5 / (ggxv + ggxl) } else { 0.0 };
                    let v_pdf = vis * v_dot_h * n_dot_l / n_dot_h.max(1e-6);
                    let fc = (1.0 - v_dot_h).powi(5);
                    sa += (1.0 - fc) * v_pdf;
                    sb += fc * v_pdf;
                }
            }
            data.push(half::f16::from_f32(4.0 * sa / samples as f32));
            data.push(half::f16::from_f32(4.0 * sb / samples as f32));
        }
    }
    let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("brdf lut"),
        size: wgpu::Extent3d { width: size, height: size, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rg16Float,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    gpu.queue.write_texture(
        texture.as_image_copy(),
        bytemuck::cast_slice(&data),
        wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(size * 4), rows_per_image: None },
        wgpu::Extent3d { width: size, height: size, depth_or_array_layers: 1 },
    );
    texture.create_view(&Default::default())
}

fn cube_texture(device: &wgpu::Device, size: u32, mips: u32, label: &str) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d { width: size, height: size, depth_or_array_layers: 6 },
        mip_level_count: mips,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: FORMAT,
        usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    })
}

/// Finds the brightest part of an environment (a sun, a window, a
/// softbox) on the CPU and turns it into a key light: its direction, and a
/// colour that is part of the light it contributes, since the radiance cube
/// already carries the rest.
fn estimate_key_light(tex: &Texture) -> (Vec3, Vec3) {
    let fallback = (Vec3::new(-0.3, 0.8, 0.5).normalize(), Vec3::splat(0.4));
    let Some(level) = tex.levels.first() else { return fallback };
    let (w, h) = (level.width as usize, level.height as usize);
    if w == 0 || h == 0 {
        return fallback;
    }
    let faces = if tex.kind == TextureKind::Cube { 6.min(level.surfaces.len()) } else { 1 };
    // Sample about 64K texels in total, weighted by solid angle.
    let step = ((w * h * faces) as f64 / 65536.0).sqrt().ceil().max(1.0) as usize;
    let mut samples: Vec<(Vec3, f32, f32)> = Vec::new();
    for face in 0..faces {
        let Some(p) = level.surfaces.get(face) else { continue };
        for y in (0..h).step_by(step) {
            for x in (0..w).step_by(step) {
                let i = (y * w + x) * 4;
                let rgb = match p {
                    Pixels::Rgba32F(v) => v.get(i..i + 3).map(|c| Vec3::new(c[0], c[1], c[2])),
                    Pixels::Rgba8(v) => v.get(i..i + 3).map(|c| {
                        let f = |b: u8| (b as f32 / 255.0).powf(2.2);
                        Vec3::new(f(c[0]), f(c[1]), f(c[2]))
                    }),
                    // Lighting pictures are decoded to texels, never kept in
                    // GPU blocks.
                    Pixels::Blocks(..) => None,
                };
                let Some(rgb) = rgb.filter(|c| c.is_finite()) else { continue };
                let u = (x as f32 + 0.5) / w as f32;
                let v = (y as f32 + 0.5) / h as f32;
                let (dir, weight) = if tex.kind == TextureKind::Cube {
                    let d = cube_dir(face, Vec3::new(u * 2.0 - 1.0, v * 2.0 - 1.0, 0.0));
                    (d.normalize(), 1.0 / d.length().powi(3))
                } else {
                    let phi = (u - 0.5) * std::f32::consts::TAU;
                    let theta = v * std::f32::consts::PI;
                    (Vec3::new(theta.sin() * phi.cos(), theta.cos(), theta.sin() * phi.sin()), theta.sin())
                };
                samples.push((dir, rgb.dot(Vec3::new(0.2126, 0.7152, 0.0722)).max(0.0), weight));
            }
        }
    }
    let total: f32 = samples.iter().map(|s| s.1 * s.2).sum();
    let max = samples.iter().map(|s| s.1).fold(0.0, f32::max);
    if total <= 0.0 || samples.is_empty() {
        return fallback;
    }
    let mean = total / samples.iter().map(|s| s.2).sum::<f32>().max(1e-6);
    // The bright region: everything within half the peak.
    let mut dir = Vec3::ZERO;
    let mut energy = 0.0;
    for (d, l, w) in &samples {
        if *l >= max * 0.5 {
            dir += *d * *l * *w;
            energy += l * w;
        }
    }
    let Some(dir) = dir.try_normalize() else { return fallback };
    // A sharp peak (sun, lamp) gets a stronger key light than an overcast
    // sky, where it mostly places the shadow.
    let peak = (max / mean.max(1e-6)).clamp(1.0, 1000.0);
    let strength = (energy / total).clamp(0.0, 1.0) * mean * peak.ln().max(0.5) * 2.0;
    let dir = if dir.y < 0.05 { (dir + Vec3::Y * 0.3).normalize() } else { dir };
    (dir, Vec3::splat(strength.clamp(0.1, 4.0)))
}

fn cube_dir(face: usize, uv: Vec3) -> Vec3 {
    let (u, v) = (uv.x, uv.y);
    match face {
        0 => Vec3::new(1.0, -v, -u),
        1 => Vec3::new(-1.0, -v, u),
        2 => Vec3::new(u, 1.0, v),
        3 => Vec3::new(u, -1.0, -v),
        4 => Vec3::new(u, -v, 1.0),
        _ => Vec3::new(-u, -v, -1.0),
    }
}
