//! Gaussian splats on the GPU.
//!
//! Each frame that shows splats runs four steps:
//!
//! 1. A compute pass projects every splat (`splat_project.wgsl`): its
//!    screen-space ellipse from the 3D covariance, its colour for the
//!    viewing direction from spherical harmonics, and a depth key.
//! 2. The splats are put in back-to-front order.  The viewer re-sorts on
//!    the GPU whenever the camera has moved (`splat_sort.wgsl`, a radix sort
//!    that needs no subgroup operations or forward-progress guarantees, so
//!    it runs alike on Vulkan, Metal, Direct3D 12 and lavapipe).
//!    Thumbnails sort once on the CPU, which costs a few milliseconds and
//!    no extra pipelines in a cold process.  `RendererConfig::
//!    gpu_splat_sort` chooses.
//! 3. A render pass draws one quad per splat, in that order, into a layer of
//!    its own (`splat_draw.wgsl`), blending with premultiplied alpha in the
//!    photos' display encoding, which is what 3D Gaussian splatting trains
//!    against.  The layer is half-float, so thousands of faint splats don't
//!    band.
//! 4. The main pass lays the layer over the background, ground and grid
//!    (`splat_composite.wgsl`), decoded to linear light, before
//!    transparent surfaces and overlays.
//!
//! Splats don't test against the depth of meshes: no format puts both in
//! one file.  They cast no shadows and get no ground shadow; captures bring
//! their own ground.

use crate::Gpu;
use crate::renderer::{RenderSettings, Renderer, buffer_init};
use crate::resources::Layouts;
use crate::scene_gpu::GpuScene;
use bytemuck::{Pod, Zeroable};
use glam::{Mat3, Mat4, Vec3};
use slerp_scene::{Aabb, Splats, View};
use std::collections::HashMap;

/// The splats' own layer: half floats with alpha.
const LAYER_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;
/// Keys per workgroup tile of the sort, as `splat_sort.wgsl` has it.
const TILE: u32 = 4096;
/// 24-bit keys, 8 bits per pass.
const PASSES: usize = 3;
/// Words per splat in the base buffer.
const BASE_WORDS: usize = 12;
/// Bytes per splat of `Projected` in the shaders.
const PROJECTED_BYTES: u64 = 24;
/// More than any capture holds; keeps sort dispatches within limits.
const MAX_SPLATS: usize = 64 << 20;

/*┌─────────────────╮
| Data            |
└─────────────────╯*/
//region Data

/// `Params` in `splat_project.wgsl` and `splat_draw.wgsl`.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Params {
    view: [[f32; 4]; 4],
    proj: [[f32; 4]; 4],
    camera: [f32; 4],
    forward: [f32; 4],
    viewport: [f32; 4],
    misc: [f32; 4],
    counts: [u32; 4],
}

/// `SortParams` in `splat_sort.wgsl`.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct SortParams {
    count: u32,
    shift: u32,
    tiles: u32,
    pad: u32,
}

/// A scene's splats on the GPU.
pub(crate) struct GpuSplats {
    pub count: u32,
    pub sh_degree: u8,
    sh_words: u32,
    antialiased: bool,
    /// The dense part of the cloud (`Splats::dense_bounds`), for framing.
    pub bounds: Aabb,
    /// GPU memory held.
    pub bytes: u64,
    params: wgpu::Buffer,
    project_group: wgpu::BindGroup,
    draw_group: wgpu::BindGroup,
    /// Splat indices in drawing order, written by the CPU sort or the GPU
    /// sort's last pass.
    order: wgpu::Buffer,
    /// The GPU sort's bind group for each pass, ping-ponging between two
    /// pairs of key and value buffers and ending in `order`.
    sort_groups: Option<[wgpu::BindGroup; PASSES]>,
    tiles: u32,
    /// Centres for the CPU sort, when the GPU doesn't sort.
    positions: Vec<Vec3>,
}

/// Pipelines and the layer, made when the first splats are prepared.
impl SplatPipelines {
    /// Drops the layer sized to the last target, and the sort order of the
    /// last scene.
    pub(crate) fn trim(&mut self) {
        self.layer = None;
        self.sorted = None;
    }
}

pub(crate) struct SplatPipelines {
    project_layout: wgpu::BindGroupLayout,
    sort_layout: wgpu::BindGroupLayout,
    draw_layout: wgpu::BindGroupLayout,
    composite_layout: wgpu::BindGroupLayout,
    project: Option<wgpu::ComputePipeline>,
    sort: Option<[wgpu::ComputePipeline; 3]>,
    draw: Option<wgpu::RenderPipeline>,
    composite_module: Option<wgpu::ShaderModule>,
    composite: HashMap<(wgpu::TextureFormat, u32), wgpu::RenderPipeline>,
    layer: Option<((u32, u32), wgpu::BindGroup, wgpu::TextureView)>,
    /// The scene id and view matrix the order was last sorted for.
    sorted: Option<(u64, [u32; 16])>,
}

fn storage(binding: u32, vis: wgpu::ShaderStages, read_only: bool) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: vis,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Storage { read_only },
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    }
}

fn uniform(binding: u32, vis: wgpu::ShaderStages) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: vis,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Uniform,
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    }
}

impl SplatPipelines {
    fn new(device: &wgpu::Device) -> SplatPipelines {
        use wgpu::ShaderStages as S;
        let bgl = |label: &str, entries: &[wgpu::BindGroupLayoutEntry]| {
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor { label: Some(label), entries })
        };
        let c = S::COMPUTE;
        SplatPipelines {
            project_layout: bgl(
                "splat project",
                &[
                    uniform(0, c),
                    storage(1, c, true),
                    storage(2, c, true),
                    storage(3, c, false),
                    storage(4, c, false),
                    storage(5, c, false),
                ],
            ),
            sort_layout: bgl(
                "splat sort",
                &[
                    uniform(0, c),
                    storage(1, c, true),
                    storage(2, c, true),
                    storage(3, c, false),
                    storage(4, c, false),
                    storage(5, c, false),
                    storage(6, c, false),
                ],
            ),
            draw_layout: bgl(
                "splat draw",
                &[uniform(0, S::VERTEX), storage(1, S::VERTEX, true), storage(2, S::VERTEX, true)],
            ),
            composite_layout: bgl(
                "splat composite",
                &[wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: S::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                }],
            ),
            project: None,
            sort: None,
            draw: None,
            composite_module: None,
            composite: HashMap::new(),
            layer: None,
            sorted: None,
        }
    }
}

//endregion

/*┌─────────────────╮
| Sorting         |
└─────────────────╯*/
//region Sorting

/// A 24-bit key that sorts farthest first, as `depth_key` in
/// `splat_project.wgsl` makes it.
fn depth_key(depth: f32) -> u32 {
    let b = depth.to_bits();
    let ordered = if b & 0x8000_0000 != 0 { !b } else { b | 0x8000_0000 };
    !ordered >> 8
}

/// Splat indices ordered farthest from the camera first, by depth along
/// the view direction: three stable passes of 8 bits over the same keys
/// the GPU sort uses.
pub(crate) fn back_to_front(positions: &[Vec3], view: &Mat4) -> Vec<u32> {
    let row = view.row(2);
    let n = positions.len();
    let mut keys: Vec<u32> =
        positions.iter().map(|p| depth_key(-(row.x * p.x + row.y * p.y + row.z * p.z + row.w))).collect();
    let mut values: Vec<u32> = (0..n as u32).collect();
    let mut keys2 = vec![0u32; n];
    let mut values2 = vec![0u32; n];
    for pass in 0..PASSES {
        let shift = pass * 8;
        let mut starts = [0usize; 256];
        for &k in &keys {
            starts[((k >> shift) & 255) as usize] += 1;
        }
        let mut sum = 0;
        for s in starts.iter_mut() {
            let count = *s;
            *s = sum;
            sum += count;
        }
        for (&k, &v) in keys.iter().zip(&values) {
            let d = ((k >> shift) & 255) as usize;
            keys2[starts[d]] = k;
            values2[starts[d]] = v;
            starts[d] += 1;
        }
        std::mem::swap(&mut keys, &mut keys2);
        std::mem::swap(&mut values, &mut values2);
    }
    values
}

//endregion

/*┌─────────────────╮
| Upload          |
└─────────────────╯*/
//region Upload

/// Covariance xx, xy, xz, yy, yz, zz of a splat: R S Sᵀ Rᵀ.
fn covariance(scale: Vec3, rotation: glam::Quat) -> [f32; 6] {
    let m = Mat3::from_quat(rotation.normalize()) * Mat3::from_diagonal(scale);
    let s = m * m.transpose();
    [s.x_axis.x, s.y_axis.x, s.z_axis.x, s.y_axis.y, s.z_axis.y, s.z_axis.z]
}

fn half_bits(v: f32) -> u32 {
    half::f16::from_f32(v).to_bits() as u32
}

/// A buffer of `items` records of `width` 32-bit words, each filled by
/// `fill(item, words)`.  Records are built 65,536 at a time and copied into
/// mapped memory, so a large cloud never exists twice in full.
fn filled_buffer(
    device: &wgpu::Device,
    label: &str,
    items: usize,
    width: usize,
    usage: wgpu::BufferUsages,
    fill: impl Fn(usize, &mut [u32]),
) -> wgpu::Buffer {
    let words = items * width;
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some(label),
        size: (words.max(4) * 4) as u64,
        usage,
        mapped_at_creation: true,
    });
    if width > 0 {
        let mut mapped = buffer.slice(..).get_mapped_range_mut();
        let mut chunk: Vec<u32> = Vec::new();
        for first in (0..items).step_by(65_536) {
            let n = (items - first).min(65_536);
            chunk.clear();
            chunk.resize(n * width, 0);
            for (j, out) in chunk.chunks_exact_mut(width).enumerate() {
                fill(first + j, out);
            }
            let at = first * width * 4;
            mapped.slice(at..at + chunk.len() * 4).copy_from_slice(bytemuck::cast_slice(&chunk));
        }
    }
    buffer.unmap();
    buffer
}

fn entry(binding: u32, buffer: &wgpu::Buffer) -> wgpu::BindGroupEntry<'_> {
    wgpu::BindGroupEntry { binding, resource: buffer.as_entire_binding() }
}

fn plain_buffer(device: &wgpu::Device, label: &str, bytes: u64, usage: wgpu::BufferUsages) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some(label),
        size: bytes.max(16).next_multiple_of(4),
        usage,
        mapped_at_creation: false,
    })
}

impl Renderer {
    /// Uploads splats.  Clouds larger than the device can bind are thinned
    /// evenly, with a warning.
    pub(crate) fn prepare_splats(&mut self, gpu: &Gpu, s: &Splats, warnings: &mut Vec<String>) -> Option<GpuSplats> {
        let device = &gpu.device;
        let pipes = self.splat.get_or_insert_with(|| SplatPipelines::new(device));

        // 1. What to upload: every splat whose vectors agree, unless the
        //    device's largest buffer binding can't hold them.
        let total = s.positions.len().min(s.scales.len()).min(s.rotations.len()).min(s.colors.len());
        let per_channel = Splats::sh_coefficients(s.sh_degree);
        let (degree, per_channel) =
            if s.sh.len() >= total * per_channel * 3 { (s.sh_degree.min(3), per_channel) } else { (0, 0) };
        let sh_words = (per_channel * 3).div_ceil(2);
        let limits = device.limits();
        let largest = limits.max_storage_buffer_binding_size.min(limits.max_buffer_size);
        let widest = (BASE_WORDS as u64 * 4).max(sh_words as u64 * 4).max(PROJECTED_BYTES);
        let fit = (largest / widest) as usize;
        let n = total.min(fit).min(MAX_SPLATS);
        if n == 0 {
            return None;
        }
        let step = total.div_ceil(n);
        let picked: Vec<usize> = (0..total).step_by(step).collect();
        let n = picked.len();
        if step > 1 {
            warnings.push(format!("drawing every {step}th of {total} splats: the GPU's buffers can't hold them all"));
        }

        // 2. Centre, opacity, covariance and base colour; harmonics as
        //    half-float pairs.
        let storage = wgpu::BufferUsages::STORAGE;
        let base = filled_buffer(device, "splat base", n, BASE_WORDS, storage, |j, out| {
            let i = picked[j];
            let p = s.positions[i];
            let c = s.colors[i];
            let cov = covariance(s.scales[i], s.rotations[i]);
            out[0] = p.x.to_bits();
            out[1] = p.y.to_bits();
            out[2] = p.z.to_bits();
            out[3] = c.w.clamp(0.0, 1.0).to_bits();
            for (o, v) in out[4..10].iter_mut().zip(cov) {
                *o = if v.is_finite() { v.to_bits() } else { 0 };
            }
            out[10] = half_bits(c.x) | half_bits(c.y) << 16;
            out[11] = half_bits(c.z);
        });
        let halves = per_channel * 3;
        let sh = filled_buffer(device, "splat harmonics", n, sh_words, storage, |j, out| {
            let i = picked[j];
            let src = &s.sh[i * halves..(i + 1) * halves];
            for (j, o) in out.iter_mut().enumerate() {
                let lo = src.get(2 * j).copied().unwrap_or(0) as u32;
                let hi = src.get(2 * j + 1).copied().unwrap_or(0) as u32;
                *o = lo | hi << 16;
            }
        });

        // 3. What each frame writes: projections, the order, and for the
        //    GPU sort its keys, values and counts.
        let projected = plain_buffer(device, "splat projected", n as u64 * PROJECTED_BYTES, storage);
        // Copyable, so tests can read the GPU sort back.
        let order = plain_buffer(
            device,
            "splat order",
            n as u64 * 4,
            storage | wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::COPY_SRC,
        );
        let params = plain_buffer(
            device,
            "splat params",
            std::mem::size_of::<Params>() as u64,
            wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        );
        let gpu_sort = self.config.gpu_splat_sort;
        let tiles = (n as u32).div_ceil(TILE);
        let key_bytes = if gpu_sort { n as u64 * 4 } else { 4 };
        let keys_a = plain_buffer(device, "splat keys", key_bytes, storage);
        let values_a = plain_buffer(device, "splat values", key_bytes, storage);
        let mut bytes = base.size() + sh.size() + projected.size() + order.size() + keys_a.size() * 2;
        let project_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("splat project"),
            layout: &pipes.project_layout,
            entries: &[
                entry(0, &params),
                entry(1, &base),
                entry(2, &sh),
                entry(3, &projected),
                entry(4, &keys_a),
                entry(5, &values_a),
            ],
        });
        let draw_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("splat draw"),
            layout: &pipes.draw_layout,
            entries: &[entry(0, &params), entry(1, &projected), entry(2, &order)],
        });
        let sort_groups = gpu_sort.then(|| {
            let keys_b = plain_buffer(device, "splat keys", key_bytes, storage);
            let counts = plain_buffer(device, "splat counts", 256 * tiles as u64 * 4, storage);
            let totals = plain_buffer(device, "splat totals", 256 * 4, storage);
            bytes += keys_b.size() + counts.size() + totals.size();
            let a = (&keys_a, &values_a);
            let b = (&keys_b, &order);
            let ends = [(a, b), (b, a), (a, b)];
            std::array::from_fn(|pass| {
                let ((keys_in, values_in), (keys_out, values_out)) = ends[pass];
                let params = SortParams { count: n as u32, shift: pass as u32 * 8, tiles, pad: 0 };
                let params =
                    buffer_init(device, "splat sort", bytemuck::bytes_of(&params), wgpu::BufferUsages::UNIFORM);
                device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("splat sort"),
                    layout: &pipes.sort_layout,
                    entries: &[
                        entry(0, &params),
                        entry(1, keys_in),
                        entry(2, values_in),
                        entry(3, keys_out),
                        entry(4, values_out),
                        entry(5, &counts),
                        entry(6, &totals),
                    ],
                })
            })
        });
        Some(GpuSplats {
            count: n as u32,
            sh_degree: degree,
            sh_words: sh_words as u32,
            antialiased: s.antialiased,
            bounds: s.dense_bounds(),
            bytes,
            params,
            project_group,
            draw_group,
            order,
            sort_groups,
            tiles,
            positions: if gpu_sort { Vec::new() } else { picked.iter().map(|&i| s.positions[i]).collect() },
        })
    }
}

//endregion

/*┌─────────────────╮
| Drawing         |
└─────────────────╯*/
//region Drawing

impl SplatPipelines {
    fn compute(
        gpu: &Gpu,
        layout: &wgpu::BindGroupLayout,
        module: &wgpu::ShaderModule,
        entry: &str,
    ) -> wgpu::ComputePipeline {
        let layout = gpu.device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some(entry),
            bind_group_layouts: &[Some(layout)],
            immediate_size: 0,
        });
        gpu.device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some(entry),
            layout: Some(&layout),
            module,
            entry_point: Some(entry),
            // Every kernel writes its workgroup memory before reading it,
            // so zeroing it (which Direct3D's compiler handles slowly) is
            // wasted.
            compilation_options: wgpu::PipelineCompilationOptions {
                zero_initialize_workgroup_memory: false,
                ..Default::default()
            },
            cache: gpu.pipeline_cache.as_ref(),
        })
    }

    /// Makes what drawing needs; returns whether anything was made, so the
    /// pipeline cache gets saved.
    fn ensure(&mut self, gpu: &Gpu, gpu_sort: bool) -> bool {
        let mut made = false;
        if self.project.is_none() {
            let module = crate::shaders::module(&gpu.device, "splat project", crate::shaders::SPLAT_PROJECT);
            self.project = Some(Self::compute(gpu, &self.project_layout, &module, "cs_project"));
            made = true;
        }
        if gpu_sort && self.sort.is_none() {
            let module = crate::shaders::module(&gpu.device, "splat sort", crate::shaders::SPLAT_SORT);
            let make = |entry| Self::compute(gpu, &self.sort_layout, &module, entry);
            self.sort = Some([make("cs_count"), make("cs_scan"), make("cs_scatter")]);
            made = true;
        }
        if self.draw.is_none() {
            let module = crate::shaders::module(&gpu.device, "splat draw", crate::shaders::SPLAT_DRAW);
            let layout = gpu.device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("splat draw"),
                bind_group_layouts: &[Some(&self.draw_layout)],
                immediate_size: 0,
            });
            self.draw = Some(gpu.device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("splat draw"),
                layout: Some(&layout),
                vertex: wgpu::VertexState {
                    module: &module,
                    entry_point: Some("vs_splat"),
                    compilation_options: Default::default(),
                    buffers: &[],
                },
                primitive: wgpu::PrimitiveState {
                    topology: wgpu::PrimitiveTopology::TriangleStrip,
                    ..Default::default()
                },
                depth_stencil: None,
                multisample: Default::default(),
                fragment: Some(wgpu::FragmentState {
                    module: &module,
                    entry_point: Some("fs_splat"),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: LAYER_FORMAT,
                        blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                multiview_mask: None,
                cache: gpu.pipeline_cache.as_ref(),
            }));
            made = true;
        }
        made
    }

    /// The pipeline that lays the layer over a main pass of `format` and
    /// `samples`.  Its layout is the frame group and the layer's, with no
    /// empty slots (see `Layouts::backdrop` for why that matters on Metal
    /// and Direct3D).
    fn composite(
        &mut self,
        gpu: &Gpu,
        layouts: &Layouts,
        format: wgpu::TextureFormat,
        samples: u32,
    ) -> (wgpu::RenderPipeline, bool) {
        if let Some(p) = self.composite.get(&(format, samples)) {
            return (p.clone(), false);
        }
        let module = self
            .composite_module
            .get_or_insert_with(|| {
                crate::shaders::module(&gpu.device, "splat composite", crate::shaders::SPLAT_COMPOSITE)
            })
            .clone();
        let layout = gpu.device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("splat composite"),
            bind_group_layouts: &[Some(&layouts.frame), Some(&self.composite_layout)],
            immediate_size: 0,
        });
        let pipeline = gpu.device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("splat composite"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &module,
                entry_point: Some("vs_splat_composite"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            primitive: Default::default(),
            depth_stencil: Some(wgpu::DepthStencilState {
                format: crate::resources::DEPTH_FORMAT,
                depth_write_enabled: Some(false),
                depth_compare: Some(wgpu::CompareFunction::Always),
                stencil: Default::default(),
                bias: Default::default(),
            }),
            multisample: wgpu::MultisampleState { count: samples, ..Default::default() },
            fragment: Some(wgpu::FragmentState {
                module: &module,
                entry_point: Some("fs_splat_composite"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: gpu.pipeline_cache.as_ref(),
        });
        self.composite.insert((format, samples), pipeline.clone());
        (pipeline, true)
    }

    /// The layer for a target of `size`, and the composite's bind group
    /// reading it.
    fn layer(&mut self, gpu: &Gpu, size: (u32, u32)) -> (wgpu::TextureView, wgpu::BindGroup) {
        if let Some((s, group, view)) = &self.layer
            && *s == size
        {
            return (view.clone(), group.clone());
        }
        let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("splat layer"),
            size: wgpu::Extent3d { width: size.0, height: size.1, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: LAYER_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let view = texture.create_view(&Default::default());
        let group = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("splat layer"),
            layout: &self.composite_layout,
            entries: &[wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&view) }],
        });
        self.layer = Some((size, group.clone(), view.clone()));
        (view, group)
    }
}

impl Renderer {
    /// Projects, sorts and draws a scene's splats into their layer, and
    /// returns what the main pass needs to lay it over the target: the
    /// composite pipeline and the layer's bind group.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn encode_splats(
        &mut self,
        gpu: &Gpu,
        encoder: &mut wgpu::CommandEncoder,
        scene: &GpuScene,
        view: &View,
        settings: &RenderSettings,
        size: (u32, u32),
        format: wgpu::TextureFormat,
        samples: u32,
    ) -> Option<(wgpu::RenderPipeline, wgpu::BindGroup)> {
        let s = scene.splats.as_ref()?;
        let gpu_sort = s.sort_groups.is_some();
        let layouts = &self.layouts;
        let pipes = self.splat.as_mut()?;
        let step = crate::trace::Step::new("splats");
        let mut made = pipes.ensure(gpu, gpu_sort);
        let (composite, new) = pipes.composite(gpu, layouts, format, samples);
        made |= new;
        let (layer_view, layer_group) = pipes.layer(gpu, size);

        // 1. Frame constants.  Sort again only when the camera moved.
        let ortho = view.proj.z_axis.w == 0.0;
        let view_bits = view.view.to_cols_array().map(f32::to_bits);
        let sort_now = pipes.sorted != Some((scene.id, view_bits));
        pipes.sorted = Some((scene.id, view_bits));
        let (w, h) = (size.0 as f32, size.1 as f32);
        let forward = -view.view.row(2).truncate().normalize_or_zero();
        let flags = s.antialiased as u32 | if gpu_sort && sort_now { 2 } else { 0 };
        let scale = if settings.splat_scale.is_finite() { settings.splat_scale.clamp(0.01, 100.0) } else { 1.0 };
        let params = Params {
            view: view.view.to_cols_array_2d(),
            proj: view.proj.to_cols_array_2d(),
            camera: view.position.extend(if ortho { 1.0 } else { 0.0 }).to_array(),
            forward: forward.extend(0.0).to_array(),
            viewport: [w, h, view.proj.x_axis.x * w * 0.5, view.proj.y_axis.y * h * 0.5],
            misc: [scale, view.near, 1.0 / view.proj.x_axis.x, 1.0 / view.proj.y_axis.y],
            counts: [s.count, s.sh_degree as u32, s.sh_words, flags],
        };
        gpu.queue.write_buffer(&s.params, 0, bytemuck::bytes_of(&params));
        if !gpu_sort && sort_now {
            let order = back_to_front(&s.positions, &view.view);
            gpu.queue.write_buffer(&s.order, 0, bytemuck::cast_slice(&order));
        }

        // 2. Project, then sort on the GPU.
        {
            let mut pass = encoder
                .begin_compute_pass(&wgpu::ComputePassDescriptor { label: Some("splats"), timestamp_writes: None });
            pass.set_pipeline(pipes.project.as_ref()?);
            pass.set_bind_group(0, &s.project_group, &[]);
            let groups = s.count.div_ceil(256);
            let x = groups.min(65535);
            pass.dispatch_workgroups(x, groups.div_ceil(x), 1);
            if let (true, Some(sort), Some(sort_groups)) = (sort_now, &pipes.sort, &s.sort_groups) {
                for group in sort_groups {
                    pass.set_bind_group(0, group, &[]);
                    pass.set_pipeline(&sort[0]);
                    pass.dispatch_workgroups(s.tiles, 1, 1);
                    pass.set_pipeline(&sort[1]);
                    pass.dispatch_workgroups(256, 1, 1);
                    pass.set_pipeline(&sort[2]);
                    pass.dispatch_workgroups(s.tiles, 1, 1);
                }
            }
        }

        // 3. The splats, back to front, into their layer.
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("splats"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &layer_view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                ..Default::default()
            });
            pass.set_pipeline(pipes.draw.as_ref()?);
            pass.set_bind_group(0, &s.draw_group, &[]);
            pass.draw(0..4, 0..s.count);
        }
        if made {
            self.pipelines_made = true;
        }
        step.done(&gpu.device);
        Some((composite, layer_group))
    }
}

//endregion

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_order_far_to_near() {
        let depths = [-3.0f32, -0.5, 0.0, 0.25, 1.0, 2.0, 100.0, 1e6];
        for pair in depths.windows(2) {
            assert!(depth_key(pair[0]) > depth_key(pair[1]), "{pair:?}");
        }
        assert!(depth_key(f32::MAX) < 1 << 24);
    }

    #[test]
    fn cpu_sort_is_back_to_front() {
        let view = glam::camera::rh::view::look_at_mat4(Vec3::new(0.0, 0.0, 5.0), Vec3::ZERO, Vec3::Y);
        let positions: Vec<Vec3> =
            (0..5000).map(|i| Vec3::new((i % 7) as f32, (i % 3) as f32, ((i * 7919) % 1000) as f32 * -0.01)).collect();
        let order = back_to_front(&positions, &view);
        let depth = |i: u32| -(view * positions[i as usize].extend(1.0)).z;
        for pair in order.windows(2) {
            assert!(depth(pair[0]) >= depth(pair[1]));
        }
        let mut seen = order.clone();
        seen.sort_unstable();
        assert!(seen.iter().enumerate().all(|(i, &v)| v == i as u32));
    }

    #[test]
    fn covariance_of_a_turned_splat() {
        let q = glam::Quat::from_rotation_z(std::f32::consts::FRAC_PI_2);
        let c = covariance(Vec3::new(2.0, 1.0, 0.5), q);
        // The long axis now points along y.
        assert!((c[0] - 1.0).abs() < 1e-5 && (c[3] - 4.0).abs() < 1e-5 && (c[5] - 0.25).abs() < 1e-5, "{c:?}");
        assert!(c[1].abs() < 1e-5);
    }
}
