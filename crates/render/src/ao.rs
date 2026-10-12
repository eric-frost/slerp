//! Screen-space ambient occlusion.
//!
//! Each frame that wants it runs three passes before the main pass:
//!
//! 1. A pre-pass draws the opaque surfaces (and points) once more, without
//!    multisampling, into a depth texture and a texture of view-space
//!    normals (`fs_prepass` in scene.wgsl).
//! 2. `fs_gtao` (ao.wgsl) finds, for every pixel, how much of the sky
//!    around its normal the nearby depth hides: ground-truth ambient
//!    occlusion as Intel's XeGTAO computes it, with three slices of six
//!    steps a side, each pixel of a 4x4 block turned differently.
//!    A short march along the mirror direction says how much of the
//!    environment's reflection escapes nearby geometry.
//! 3. `fs_ao_blur` smooths both over the pixel's own surface and stores
//!    them with the view depth they belong to.
//!
//! The main pass's surfaces look up their pixel (`screen_ao` in
//! lighting.wgsl) and darken the environment's diffuse and specular light,
//! not the key light, whose shadow map already handles it.  Specular light
//! takes the smaller of the occlusion's estimate and the reflection's.  Blended and
//! transmissive surfaces don't take part.  Everything here uses explicit
//! pipeline layouts, so it binds alike on Vulkan, Metal and Direct3D 12.

use crate::Gpu;
use crate::resources::{AO_FORMAT, DEPTH_FORMAT, NORMAL_FORMAT};
use bytemuck::{Pod, Zeroable};
use glam::Mat4;

/// The noisy visibilities (ambient and along the reflection) before the
/// blur.
const RAW_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rg16Float;

/// `AoParams` in ao.wgsl.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub(crate) struct AoParams {
    pub proj: [[f32; 4]; 4],
    pub inv_proj: [[f32; 4]; 4],
    pub light: [f32; 4],
    pub params: [f32; 4],
    pub viewport: [f32; 4],
}

impl AoParams {
    /// The parameters for a frame: `radius` in world units, the camera's
    /// projection, the target's size, and the direction towards the key
    /// light in view space.
    pub fn new(proj: &Mat4, radius: f32, size: (u32, u32), light: glam::Vec3) -> AoParams {
        let (w, h) = (size.0 as f32, size.1 as f32);
        let ortho = proj.z_axis.w == 0.0;
        AoParams {
            proj: proj.to_cols_array_2d(),
            inv_proj: proj.inverse().to_cols_array_2d(),
            light: light.extend(0.0).to_array(),
            // Pixels per view unit at depth 1 (perspective) or anywhere
            // (orthographic), and a cap on the search radius: wider
            // searches only add noise.
            params: [radius, proj.y_axis.y * h * 0.5, w.min(h) * 0.25, if ortho { 1.0 } else { 0.0 }],
            viewport: [w, h, 1.0 / w, 1.0 / h],
        }
    }
}

/// The screen-sized textures, made again when the size changes.
pub(crate) struct AoTargets {
    pub size: (u32, u32),
    pub depth: wgpu::TextureView,
    pub normals: wgpu::TextureView,
    /// What the main pass reads: visibility and view depth.
    pub result: wgpu::TextureView,
    raw: wgpu::TextureView,
    gtao_group: wgpu::BindGroup,
    blur_group: wgpu::BindGroup,
}

pub(crate) struct Ao {
    gtao_layout: wgpu::BindGroupLayout,
    blur_layout: wgpu::BindGroupLayout,
    gtao: wgpu::RenderPipeline,
    blur: wgpu::RenderPipeline,
    params: wgpu::Buffer,
    pub targets: Option<AoTargets>,
}

fn entry(binding: u32, ty: wgpu::BindingType) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry { binding, visibility: wgpu::ShaderStages::FRAGMENT, ty, count: None }
}

fn texture(binding: u32, sample_type: wgpu::TextureSampleType) -> wgpu::BindGroupLayoutEntry {
    entry(
        binding,
        wgpu::BindingType::Texture { sample_type, view_dimension: wgpu::TextureViewDimension::D2, multisampled: false },
    )
}

impl Ao {
    pub fn new(gpu: &Gpu) -> Ao {
        let device = &gpu.device;
        let module = crate::shaders::module(device, "ambient occlusion", crate::shaders::AO);
        let uniform = entry(
            0,
            wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: None,
            },
        );
        let depth = texture(1, wgpu::TextureSampleType::Depth);
        let normals = texture(2, wgpu::TextureSampleType::Float { filterable: false });
        let raw = texture(3, wgpu::TextureSampleType::Float { filterable: false });
        let bgl = |label, entries: &[wgpu::BindGroupLayoutEntry]| {
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor { label: Some(label), entries })
        };
        let gtao_layout = bgl("gtao", &[uniform, depth, normals]);
        let blur_layout = bgl("ao blur", &[uniform, depth, normals, raw]);
        let pipeline = |label, layout: &wgpu::BindGroupLayout, fs, format: wgpu::TextureFormat| {
            let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some(label),
                bind_group_layouts: &[Some(layout)],
                immediate_size: 0,
            });
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(&layout),
                vertex: wgpu::VertexState {
                    module: &module,
                    entry_point: Some("vs_ao"),
                    compilation_options: Default::default(),
                    buffers: &[],
                },
                primitive: Default::default(),
                depth_stencil: None,
                multisample: Default::default(),
                fragment: Some(wgpu::FragmentState {
                    module: &module,
                    entry_point: Some(fs),
                    compilation_options: Default::default(),
                    targets: &[Some(format.into())],
                }),
                multiview_mask: None,
                cache: gpu.pipeline_cache.as_ref(),
            })
        };
        let gtao = pipeline("gtao", &gtao_layout, "fs_gtao", RAW_FORMAT);
        let blur = pipeline("ao blur", &blur_layout, "fs_ao_blur", AO_FORMAT);
        let params = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("ao params"),
            size: std::mem::size_of::<AoParams>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        Ao { gtao_layout, blur_layout, gtao, blur, params, targets: None }
    }

    /// Makes the targets for `size`.  Returns whether they are new, which
    /// means bind groups holding `result` must be made again.
    pub fn ensure_targets(&mut self, gpu: &Gpu, size: (u32, u32)) -> bool {
        if self.targets.as_ref().is_some_and(|t| t.size == size) {
            return false;
        }
        let device = &gpu.device;
        let tex = |format, label| {
            device
                .create_texture(&wgpu::TextureDescriptor {
                    label: Some(label),
                    size: wgpu::Extent3d { width: size.0, height: size.1, depth_or_array_layers: 1 },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format,
                    usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
                    view_formats: &[],
                })
                .create_view(&Default::default())
        };
        let depth = tex(DEPTH_FORMAT, "ao depth");
        let normals = tex(NORMAL_FORMAT, "ao normals");
        let raw = tex(RAW_FORMAT, "ao raw");
        let result = tex(AO_FORMAT, "ao");
        let group = |layout, views: &[&wgpu::TextureView]| {
            let mut entries = vec![wgpu::BindGroupEntry { binding: 0, resource: self.params.as_entire_binding() }];
            for (i, v) in views.iter().enumerate() {
                entries.push(wgpu::BindGroupEntry {
                    binding: 1 + i as u32,
                    resource: wgpu::BindingResource::TextureView(v),
                });
            }
            device.create_bind_group(&wgpu::BindGroupDescriptor { label: Some("ao"), layout, entries: &entries })
        };
        let gtao_group = group(&self.gtao_layout, &[&depth, &normals]);
        let blur_group = group(&self.blur_layout, &[&depth, &normals, &raw]);
        self.targets = Some(AoTargets { size, depth, normals, result, raw, gtao_group, blur_group });
        true
    }

    /// Records the occlusion and blur passes, after the pre-pass has filled
    /// the depth and normal targets.
    pub fn encode(&self, gpu: &Gpu, encoder: &mut wgpu::CommandEncoder, params: &AoParams) {
        let Some(t) = &self.targets else { return };
        gpu.queue.write_buffer(&self.params, 0, bytemuck::bytes_of(params));
        for (target, pipeline, group, label) in
            [(&t.raw, &self.gtao, &t.gtao_group, "gtao"), (&t.result, &self.blur, &t.blur_group, "ao blur")]
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some(label),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::WHITE),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                ..Default::default()
            });
            pass.set_pipeline(pipeline);
            pass.set_bind_group(0, group, &[]);
            pass.draw(0..3, 0..1);
        }
    }
}
