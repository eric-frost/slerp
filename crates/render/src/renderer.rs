//! The renderer: settings, pipelines and the passes that draw a prepared
//! scene into a target.

use crate::ibl::{Environment, Ibl, IblSizes, Source};
use crate::instancing::{FrameRuns, Frustum, Run};
use crate::resources::{
    CONTACT_FORMAT, DEPTH_FORMAT, FrameUniform, Layouts, LineVertex, MipGen, NORMAL_FORMAT, Samplers,
    TRANSMISSION_FORMAT, clamp_sampler, solid_texture,
};
use crate::scene_gpu::GpuScene;
use crate::{Gpu, Image8};
use glam::camera::rh::{proj::directx as proj, view};
use glam::{Mat4, Vec3};
use slerp_scene::{Aabb, Texture, Topology, View};
use std::collections::HashMap;
use wgpu::util::DeviceExt;

/*┌─────────────────╮
| Settings        |
└─────────────────╯*/
//region Settings

/// What is behind the scene.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Background {
    /// Transparent: thumbnails, so they suit light and dark themes.
    Transparent,
    /// A colour as displayed (sRGB-encoded, like a hex colour or egui's
    /// `Color32`), with straight alpha.
    Color([f32; 4]),
    /// The lighting environment itself; `blur` from 0 (sharp) to 1 (as
    /// blurred as the roughest reflections).
    Environment { blur: f32 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Tonemap {
    /// Khronos PBR Neutral: base colours show as authored.
    #[default]
    PbrNeutral,
    /// ACES, Stephen Hill's fit: filmic contrast and desaturated highlights.
    Aces,
    /// Clamp only.
    None,
}

/// A single material channel shown instead of the lit image.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum DebugChannel {
    #[default]
    None,
    BaseColor,
    /// The shading normal (after normal mapping) as colour.
    Normal,
    Metallic,
    Roughness,
    /// The material's occlusion map, combined with ambient occlusion when
    /// that is on.
    Occlusion,
    Emissive,
    /// A checker on the first UV set, showing seams and stretching.
    UvChecker,
}

/// Inspection overlays drawn over the scene.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Overlays {
    pub wireframe: bool,
    pub normals: bool,
    pub tangents: bool,
    /// The posed scene's bounding box.
    pub bounds: bool,
    /// Lines between each joint and its parent, drawn over everything.
    pub skeleton: bool,
    /// A metric grid on the ground.
    pub grid: bool,
    /// The scene's markers (Quake entities, lights) as small crosses.
    pub markers: bool,
    /// A node drawn highlighted in the skeleton overlay, such as the joint
    /// under the mouse.
    pub highlight_joint: Option<usize>,
}

/// How to draw a frame.  `thumbnail()` and `viewer()` are the defaults for
/// the two uses.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RenderSettings {
    pub background: Background,
    /// Exposure in stops: 0 shows the scene as lit, 1 doubles it.
    pub exposure: f32,
    pub tonemap: Tonemap,
    /// The key light's shadow on the scene itself.
    pub shadows: bool,
    /// A soft shadow on an invisible ground under the scene: the contact
    /// shadow plus the key light's shadow there.  Levels never get one,
    /// since they have their own floors.
    pub ground_shadow: bool,
    /// 4x multisampling.
    pub msaa: bool,
    pub overlays: Overlays,
    /// Radians around +Y.
    pub environment_rotation: f32,
    /// Multiplies the environment's light and the key light.
    pub environment_intensity: f32,
    /// The key light's strength relative to the environment's default; 0
    /// turns it off (and with it the key light's shadow).
    pub key_light: f32,
    /// Seconds, for animated effects (Quake's liquids and sky).
    pub time: f32,
    pub debug: DebugChannel,
    /// Multiplies the size of Gaussian splats: below 1 shows the cloud's
    /// structure, 1 is as trained.
    pub splat_scale: f32,
    /// Screen-space ambient occlusion: creases, corners and contact areas
    /// get less of the environment's light.
    pub ao: bool,
    /// How strongly occlusion darkens: 1 is as computed, 2 squares the
    /// light that remains, 0 is none.
    pub ao_strength: f32,
    /// How far away an occluder still counts, as a fraction of the
    /// scene's bounding radius.
    pub ao_radius: f32,
    /// Multiplies the size of points' dots, which by default is just
    /// enough for neighbouring dots to meet.
    pub point_size: f32,
    /// Points keep their size in the world, growing as the camera comes
    /// closer, instead of on screen.
    pub point_world_size: bool,
    /// How much of its own colour each lit surface shows where no light
    /// falls, as a fraction: a stand-in for the light that scenes without
    /// authored lighting (level-editor maps) lack.  0 is none.
    pub fill: f32,
}

impl RenderSettings {
    pub fn thumbnail() -> RenderSettings {
        RenderSettings {
            background: Background::Transparent,
            exposure: 0.0,
            tonemap: Tonemap::PbrNeutral,
            shadows: true,
            ground_shadow: true,
            msaa: true,
            overlays: Overlays::default(),
            environment_rotation: 0.0,
            environment_intensity: 1.0,
            key_light: 1.0,
            time: 0.0,
            debug: DebugChannel::None,
            splat_scale: 1.0,
            ao: true,
            ao_strength: 1.0,
            ao_radius: 0.15,
            point_size: 1.0,
            point_world_size: false,
            fill: 0.0,
        }
    }

    pub fn viewer() -> RenderSettings {
        RenderSettings { background: Background::Environment { blur: 0.6 }, ..RenderSettings::thumbnail() }
    }
}

impl Default for RenderSettings {
    fn default() -> Self {
        RenderSettings::viewer()
    }
}

/// Sizes of the renderer's own textures.  Thumbnails use small ones so a
/// cold start stays fast; the viewer can afford sharper lighting.
#[derive(Clone, Copy, Debug)]
pub struct RendererConfig {
    /// Face size the environment is first drawn at.
    pub environment_size: u32,
    /// Face size of the sharpest level of the prefiltered radiance cube,
    /// which is also what an environment background shows.
    pub radiance_size: u32,
    /// GGX samples per texel when prefiltering.
    pub prefilter_samples: u32,
    pub shadow_map_size: u32,
    /// Resolution of the map the soft ground shadow is made from.
    pub contact_map_size: u32,
    /// Sort Gaussian splats on the GPU, every frame the camera moves, as
    /// the viewer does.  Otherwise they are sorted on the CPU, which suits
    /// one-off frames: no sort pipelines to compile in a cold process.
    pub gpu_splat_sort: bool,
}

impl RendererConfig {
    pub fn viewer() -> RendererConfig {
        RendererConfig {
            environment_size: 512,
            radiance_size: 256,
            prefilter_samples: 128,
            shadow_map_size: 2048,
            contact_map_size: 512,
            gpu_splat_sort: true,
        }
    }

    pub fn thumbnail() -> RendererConfig {
        RendererConfig {
            environment_size: 128,
            radiance_size: 64,
            prefilter_samples: 32,
            shadow_map_size: 1024,
            contact_map_size: 256,
            gpu_splat_sort: false,
        }
    }
}

//endregion

/*┌─────────────────╮
| Pipelines       |
└─────────────────╯*/
//region Pipelines

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Kind {
    Surface {
        topology: u8,
        cull: bool,
        blend: bool,
        mirrored: bool,
    },
    Shadow,
    Contact,
    Background,
    Ground,
    Grid,
    Wire,
    Normals,
    Tangents,
    Lines {
        on_top: bool,
    },
    TexView,
    /// Points as round dots.
    Points,
    /// The ambient occlusion pre-pass: depth and normals.
    Prepass {
        cull: bool,
        mirrored: bool,
    },
    PointsPrepass,
}

/// Module, layout, vertex and fragment entry points, primitive, depth,
/// colour target and vertex buffers of a pipeline.
type PipelineSpec<'a> = (
    &'a wgpu::ShaderModule,
    &'a wgpu::PipelineLayout,
    &'a str,
    Option<&'a str>,
    wgpu::PrimitiveState,
    Option<wgpu::DepthStencilState>,
    Option<wgpu::ColorTargetState>,
    &'a [wgpu::VertexBufferLayout<'a>],
);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct PipeKey {
    kind: Kind,
    format: wgpu::TextureFormat,
    samples: u32,
}

/// The pipeline that draws a primitive of `topology` with `mat`.
fn surface_kind(topology: Topology, mat: &crate::scene_gpu::GpuMaterial, mirrored: bool) -> Kind {
    match topology {
        Topology::Points => Kind::Points,
        Topology::Triangles => Kind::Surface { topology: 0, cull: !mat.double_sided, blend: mat.blend, mirrored },
        Topology::Lines => Kind::Surface { topology: 1, cull: false, blend: mat.blend, mirrored },
    }
}

/// Binds a run's groups and draws its instances: indexed, or for points
/// three vertices per point, which `vs_points` looks up in the index data.
/// Pre-passes have no lighting group.
fn draw_run(
    pass: &mut wgpu::RenderPass,
    scene: &GpuScene,
    run: &Run,
    frame: &wgpu::BindGroup,
    lighting: Option<&wgpu::BindGroup>,
) {
    let prim = &scene.prims[run.prim];
    let chunk = &scene.chunks[prim.chunk];
    pass.set_bind_group(0, frame, &[]);
    pass.set_bind_group(1, &scene.materials[prim.material].group, &[]);
    pass.set_bind_group(2, &chunk.group, &[]);
    if let Some(l) = lighting {
        pass.set_bind_group(3, l, &[]);
    }
    let instances = run.first..run.first + run.count;
    if prim.topology == Topology::Points {
        pass.draw(0..prim.index_count * 3, instances);
    } else {
        pass.set_index_buffer(chunk.buffer.slice(..), wgpu::IndexFormat::Uint32);
        pass.draw_indexed(prim.index_first..prim.index_first + prim.index_count, 0, instances);
    }
}

/// A run with the pipeline that draws it.
type Draw = (Run, wgpu::RenderPipeline);

//endregion

/// Draws prepared scenes.  One renderer serves any number of scenes and
/// targets; a `GpuScene` belongs to the renderer that prepared it.
pub struct Renderer {
    pub(crate) format: wgpu::TextureFormat,
    pub(crate) config: RendererConfig,
    pub(crate) layouts: Layouts,
    scene_module: wgpu::ShaderModule,
    pub(crate) texview_module: Option<wgpu::ShaderModule>,
    pipelines: HashMap<PipeKey, wgpu::RenderPipeline>,
    /// Pipelines were made since the cache was last saved.
    pub(crate) pipelines_made: bool,
    pub(crate) samplers: Samplers,
    pub(crate) mipgen: MipGen,
    ibl: Ibl,
    pub(crate) white: wgpu::TextureView,
    pub(crate) flat_normal: wgpu::TextureView,
    env: Option<Environment>,
    brdf: Option<wgpu::TextureView>,
    /// The main pass's lighting group, and the transmission pass's, which
    /// can't bind the texture it draws into.  Made again when the
    /// environment or a screen-sized texture they hold changes.
    lighting: Option<wgpu::BindGroup>,
    lighting_source: Option<wgpu::BindGroup>,
    lighting_dirty: bool,
    /// Bound where transmission has nothing to show.
    clear_texture: wgpu::TextureView,
    /// Ambient occlusion's pipelines and targets, made when first used.
    ao: Option<crate::ao::Ao>,
    /// The opaque scene for transmissive surfaces, made when first needed.
    transmission: Option<crate::transmission::Transmission>,
    /// Bound in the empty slots of `Layouts::backdrop`.
    empty_group: wgpu::BindGroup,
    shadow_view: wgpu::TextureView,
    /// The texture behind `contact_view`, which tests read back.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) contact_map: wgpu::Texture,
    contact_view: wgpu::TextureView,
    contact_depth: wgpu::TextureView,
    comparison_sampler: wgpu::Sampler,
    pub(crate) linear_clamp: wgpu::Sampler,
    pub(crate) nearest_clamp: wgpu::Sampler,
    /// Main, shadow, contact and transmission passes.
    pub(crate) frame_buffers: [wgpu::Buffer; 4],
    targets: Option<Targets>,
    readback: Option<Readback>,
    lines: Option<(wgpu::Buffer, u64)>,
    /// What the shadow maps hold, to skip redrawing them.
    shadow_key: Option<[u64; 4]>,
    pub(crate) texview_state: Option<crate::texview::TexViewState>,
    /// Gaussian splat pipelines, made with the first splats.
    pub(crate) splat: Option<crate::splats::SplatPipelines>,
}

/// Multisampled colour and depth for one target size.
struct Targets {
    size: (u32, u32),
    samples: u32,
    format: wgpu::TextureFormat,
    color: Option<wgpu::TextureView>,
    depth: wgpu::TextureView,
}

struct Readback {
    size: (u32, u32),
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    buffer: wgpu::Buffer,
    row_bytes: u32,
}

const READBACK_FORMAT: wgpu::TextureFormat = crate::THUMBNAIL_FORMAT;

impl Renderer {
    /// A renderer drawing into textures of `target_format`, with the viewer's
    /// sizes, or the thumbnail sizes on a CPU driver.
    pub fn new(gpu: &Gpu, target_format: wgpu::TextureFormat) -> Renderer {
        let config = if gpu.is_cpu() { RendererConfig::thumbnail() } else { RendererConfig::viewer() };
        Renderer::with_config(gpu, target_format, config)
    }

    pub fn with_config(gpu: &Gpu, target_format: wgpu::TextureFormat, config: RendererConfig) -> Renderer {
        let device = &gpu.device;
        let layouts = Layouts::new(device);
        let scene_module = crate::shaders::module(device, "scene", crate::shaders::SCENE);
        let shadow_size = config.shadow_map_size.clamp(64, device.limits().max_texture_dimension_2d);
        let shadow = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("shadow map"),
            size: wgpu::Extent3d { width: shadow_size, height: shadow_size, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: DEPTH_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let contact_size = config.contact_map_size.clamp(16, 4096);
        let contact = |format, usage, label| {
            device.create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size: wgpu::Extent3d { width: contact_size, height: contact_size, depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage,
                view_formats: &[],
            })
        };
        // Copyable, so tests can read it back.
        let contact_map = contact(
            CONTACT_FORMAT,
            wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_SRC,
            "contact map",
        );
        let contact_view = contact_map.create_view(&Default::default());
        let contact_depth = contact(DEPTH_FORMAT, wgpu::TextureUsages::RENDER_ATTACHMENT, "contact depth")
            .create_view(&Default::default());
        let frame_buffer = |label| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size: std::mem::size_of::<FrameUniform>() as u64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            })
        };
        Renderer {
            format: target_format,
            config,
            scene_module,
            texview_module: None,
            pipelines: HashMap::new(),
            pipelines_made: false,
            samplers: Samplers::default(),
            mipgen: MipGen::new(),
            ibl: Ibl::new(device),
            white: solid_texture(gpu, [255, 255, 255, 255]),
            flat_normal: solid_texture(gpu, [128, 128, 255, 255]),
            env: None,
            brdf: None,
            lighting: None,
            lighting_source: None,
            lighting_dirty: true,
            clear_texture: solid_texture(gpu, [0, 0, 0, 0]),
            ao: None,
            transmission: None,
            empty_group: device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("empty"),
                layout: &layouts.empty,
                entries: &[],
            }),
            shadow_view: shadow.create_view(&Default::default()),
            contact_map,
            contact_view,
            contact_depth,
            comparison_sampler: device.create_sampler(&wgpu::SamplerDescriptor {
                label: Some("shadow"),
                mag_filter: wgpu::FilterMode::Linear,
                min_filter: wgpu::FilterMode::Linear,
                compare: Some(wgpu::CompareFunction::LessEqual),
                ..Default::default()
            }),
            linear_clamp: clamp_sampler(device, wgpu::FilterMode::Linear),
            nearest_clamp: clamp_sampler(device, wgpu::FilterMode::Nearest),
            frame_buffers: [
                frame_buffer("frame"),
                frame_buffer("shadow frame"),
                frame_buffer("contact frame"),
                frame_buffer("transmission frame"),
            ],
            targets: None,
            readback: None,
            lines: None,
            shadow_key: None,
            texview_state: None,
            splat: None,
            layouts,
        }
    }

    pub fn target_format(&self) -> wgpu::TextureFormat {
        self.format
    }

    /// Lets go of what is sized to the last picture drawn (multisampled
    /// colour and depth, the readback buffer, ambient occlusion's and glass's
    /// targets, the splat layer), for a renderer kept while nothing is
    /// shown.  Pipelines and lighting stay; the rest comes back with the next
    /// frame.
    pub fn trim(&mut self) {
        self.targets = None;
        self.readback = None;
        self.lines = None;
        self.transmission = None;
        if let Some(ao) = &mut self.ao {
            ao.targets = None;
        }
        if let Some(s) = &mut self.splat {
            s.trim();
        }
        // The lighting groups hold views of what just went.
        self.lighting = None;
        self.lighting_source = None;
        self.lighting_dirty = true;
        self.shadow_key = None;
    }

    /*┌─────────────────╮
    | Environment     |
    └─────────────────╯*/
    //region Environment

    /// Replaces the image-based lighting with an equirectangular panorama
    /// (HDR or not) or a cubemap.  The key light moves to its brightest
    /// region.
    pub fn set_environment(&mut self, gpu: &Gpu, hdr: &Texture) {
        let sizes = self.ibl_sizes();
        let env = self.ibl.build(gpu, &mut self.mipgen, Source::Texture(hdr), sizes);
        self.install_environment(gpu, env);
    }

    /// Goes back to the built-in studio lighting.
    pub fn set_studio_environment(&mut self, gpu: &Gpu) {
        let step = crate::trace::Step::new("studio environment");
        let sizes = self.ibl_sizes();
        let env = self.ibl.build(gpu, &mut self.mipgen, Source::Studio, sizes);
        self.install_environment(gpu, env);
        step.done(&gpu.device);
    }

    fn ibl_sizes(&self) -> IblSizes {
        IblSizes {
            source: self.config.environment_size.next_power_of_two().clamp(16, 2048),
            radiance: self.config.radiance_size.next_power_of_two().clamp(16, 1024),
            samples: self.config.prefilter_samples.clamp(4, 1024),
        }
    }

    fn install_environment(&mut self, gpu: &Gpu, env: Environment) {
        let step = crate::trace::Step::new("brdf table");
        self.brdf.get_or_insert_with(|| crate::ibl::brdf_lut(gpu, 32, 128));
        step.done(&gpu.device);
        self.env = Some(env);
        self.lighting_dirty = true;
        self.shadow_key = None;
    }

    /// Makes the lighting groups again if what they hold has changed.
    fn ensure_lighting(&mut self, gpu: &Gpu) {
        if !self.lighting_dirty && self.lighting.is_some() {
            return;
        }
        let (Some(env), Some(brdf)) = (&self.env, &self.brdf) else { return };
        let ao = self.ao.as_ref().and_then(|a| a.targets.as_ref()).map_or(&self.white, |t| &t.result);
        let make = |label, transmission: &wgpu::TextureView| {
            let view = wgpu::BindingResource::TextureView;
            gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some(label),
                layout: &self.layouts.lighting,
                entries: &[
                    wgpu::BindGroupEntry { binding: 0, resource: view(&env.radiance_view) },
                    wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&self.linear_clamp) },
                    wgpu::BindGroupEntry { binding: 2, resource: view(brdf) },
                    wgpu::BindGroupEntry { binding: 3, resource: env.sh.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 4, resource: view(&self.shadow_view) },
                    wgpu::BindGroupEntry {
                        binding: 5,
                        resource: wgpu::BindingResource::Sampler(&self.comparison_sampler),
                    },
                    wgpu::BindGroupEntry { binding: 6, resource: view(&self.contact_view) },
                    wgpu::BindGroupEntry { binding: 7, resource: wgpu::BindingResource::Sampler(&self.linear_clamp) },
                    wgpu::BindGroupEntry { binding: 8, resource: view(&env.sharp_view) },
                    wgpu::BindGroupEntry { binding: 9, resource: view(ao) },
                    wgpu::BindGroupEntry { binding: 10, resource: view(transmission) },
                ],
            })
        };
        let transmission = self.transmission.as_ref().map_or(&self.clear_texture, |t| &t.view);
        self.lighting = Some(make("lighting", transmission));
        self.lighting_source = Some(make("lighting behind glass", &self.clear_texture));
        self.lighting_dirty = false;
    }

    fn ensure_environment(&mut self, gpu: &Gpu) {
        if self.env.is_none() {
            self.set_studio_environment(gpu);
        }
    }

    /// The key light in world space for `settings`: direction towards the
    /// light and its colour.
    pub(crate) fn key_light(&self, settings: &RenderSettings) -> (Vec3, Vec3) {
        let (dir, color) = self.env.as_ref().map_or_else(crate::ibl::studio_key_light, |e| (e.key_dir, e.key_color));
        // The shaders rotate world directions into the environment's frame;
        // the light goes the other way.
        let (s, c) = settings.environment_rotation.sin_cos();
        let world = Vec3::new(c * dir.x + s * dir.z, dir.y, -s * dir.x + c * dir.z);
        (world, color * settings.key_light.max(0.0) * settings.environment_intensity.max(0.0))
    }

    //endregion

    /*┌─────────────────╮
    | Pipelines       |
    └─────────────────╯*/
    //region Pipelines

    fn pipeline(&mut self, gpu: &Gpu, kind: Kind, format: wgpu::TextureFormat, samples: u32) -> wgpu::RenderPipeline {
        let key = PipeKey { kind, format, samples };
        if let Some(p) = self.pipelines.get(&key) {
            return p.clone();
        }
        let p = self.make_pipeline(gpu, key);
        self.pipelines_made = true;
        self.pipelines.insert(key, p.clone());
        p
    }

    fn make_pipeline(&mut self, gpu: &Gpu, key: PipeKey) -> wgpu::RenderPipeline {
        let device = &gpu.device;
        let premultiplied = Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING);
        let color =
            |blend| Some(wgpu::ColorTargetState { format: key.format, blend, write_mask: wgpu::ColorWrites::ALL });
        let depth = |write: bool, compare| {
            Some(wgpu::DepthStencilState {
                format: DEPTH_FORMAT,
                depth_write_enabled: Some(write),
                depth_compare: Some(compare),
                stencil: Default::default(),
                bias: Default::default(),
            })
        };
        let ms = wgpu::MultisampleState { count: key.samples, ..Default::default() };
        let line_list = wgpu::PrimitiveState { topology: wgpu::PrimitiveTopology::LineList, ..Default::default() };
        let line_layout = [wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<LineVertex>() as u64,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x4],
        }];
        let texview_module;
        let (module, layout, vs, fs, primitive, depth_state, target, buffers): PipelineSpec = match key.kind {
            Kind::Surface { topology, cull, blend, mirrored } => {
                let topology = match topology {
                    0 => wgpu::PrimitiveTopology::TriangleList,
                    1 => wgpu::PrimitiveTopology::LineList,
                    _ => wgpu::PrimitiveTopology::PointList,
                };
                // Mirrored nodes wind clockwise; flipping which side is the
                // front keeps both culling and `front_facing` right.
                let primitive = wgpu::PrimitiveState {
                    topology,
                    front_face: if mirrored { wgpu::FrontFace::Cw } else { wgpu::FrontFace::Ccw },
                    cull_mode: (cull && topology == wgpu::PrimitiveTopology::TriangleList).then_some(wgpu::Face::Back),
                    ..Default::default()
                };
                let (target, ds) = if blend {
                    (color(premultiplied), depth(false, wgpu::CompareFunction::Less))
                } else {
                    (color(None), depth(true, wgpu::CompareFunction::Less))
                };
                (&self.scene_module, &self.layouts.scene, "vs_main", Some("fs_main"), primitive, ds, target, &[])
            }
            Kind::Shadow | Kind::Contact => {
                let shadow = key.kind == Kind::Shadow;
                let ds = wgpu::DepthStencilState {
                    format: DEPTH_FORMAT,
                    depth_write_enabled: Some(true),
                    depth_compare: Some(wgpu::CompareFunction::Less),
                    stencil: Default::default(),
                    bias: Default::default(),
                };
                let (fs, target) = if shadow {
                    ("fs_shadow", None)
                } else {
                    (
                        "fs_contact",
                        Some(wgpu::ColorTargetState {
                            format: CONTACT_FORMAT,
                            blend: None,
                            write_mask: wgpu::ColorWrites::ALL,
                        }),
                    )
                };
                (
                    &self.scene_module,
                    &self.layouts.depth,
                    "vs_main",
                    Some(fs),
                    Default::default(),
                    Some(ds),
                    target,
                    &[],
                )
            }
            Kind::Background => (
                &self.scene_module,
                &self.layouts.backdrop,
                "vs_fullscreen",
                Some("fs_background"),
                Default::default(),
                depth(false, wgpu::CompareFunction::Always),
                color(None),
                &[],
            ),
            Kind::Ground | Kind::Grid => {
                let (vs, fs) =
                    if key.kind == Kind::Ground { ("vs_ground", "fs_ground") } else { ("vs_grid", "fs_grid") };
                (
                    &self.scene_module,
                    &self.layouts.backdrop,
                    vs,
                    Some(fs),
                    Default::default(),
                    depth(false, wgpu::CompareFunction::LessEqual),
                    color(premultiplied),
                    &[],
                )
            }
            Kind::Wire => (
                &self.scene_module,
                &self.layouts.scene,
                "vs_wire",
                Some("fs_wire"),
                Default::default(),
                depth(false, wgpu::CompareFunction::LessEqual),
                color(premultiplied),
                &[],
            ),
            Kind::Normals | Kind::Tangents => (
                &self.scene_module,
                &self.layouts.scene,
                if key.kind == Kind::Normals { "vs_normals" } else { "vs_tangents" },
                Some("fs_line"),
                line_list,
                depth(false, wgpu::CompareFunction::LessEqual),
                color(premultiplied),
                &[],
            ),
            Kind::Lines { on_top } => (
                &self.scene_module,
                &self.layouts.backdrop,
                "vs_lines",
                Some("fs_line"),
                line_list,
                depth(false, if on_top { wgpu::CompareFunction::Always } else { wgpu::CompareFunction::LessEqual }),
                color(premultiplied),
                &line_layout[..],
            ),
            Kind::Points => (
                &self.scene_module,
                &self.layouts.scene,
                "vs_points",
                Some("fs_points"),
                Default::default(),
                depth(true, wgpu::CompareFunction::Less),
                color(None),
                &[],
            ),
            Kind::Prepass { cull, mirrored } => {
                let primitive = wgpu::PrimitiveState {
                    front_face: if mirrored { wgpu::FrontFace::Cw } else { wgpu::FrontFace::Ccw },
                    cull_mode: cull.then_some(wgpu::Face::Back),
                    ..Default::default()
                };
                let ds = depth(true, wgpu::CompareFunction::Less);
                (
                    &self.scene_module,
                    &self.layouts.depth,
                    "vs_main",
                    Some("fs_prepass"),
                    primitive,
                    ds,
                    color(None),
                    &[],
                )
            }
            Kind::PointsPrepass => (
                &self.scene_module,
                &self.layouts.depth,
                "vs_points",
                Some("fs_points_prepass"),
                Default::default(),
                depth(true, wgpu::CompareFunction::Less),
                color(None),
                &[],
            ),
            Kind::TexView => {
                texview_module = self
                    .texview_module
                    .get_or_insert_with(|| crate::shaders::module(device, "texview", crate::shaders::TEXVIEW))
                    .clone();
                (
                    &texview_module,
                    &self.layouts.texview_pipeline,
                    "vs_texview",
                    Some("fs_texview"),
                    Default::default(),
                    None,
                    color(premultiplied),
                    &[],
                )
            }
        };
        let targets = [target];
        let fragment = fs.map(|fs| wgpu::FragmentState {
            module,
            entry_point: Some(fs),
            compilation_options: Default::default(),
            targets: if targets[0].is_some() { &targets[..] } else { &[] },
        });
        device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some(vs),
            layout: Some(layout),
            vertex: wgpu::VertexState {
                module,
                entry_point: Some(vs),
                compilation_options: Default::default(),
                buffers,
            },
            primitive,
            depth_stencil: depth_state,
            multisample: if matches!(key.kind, Kind::Shadow | Kind::Contact | Kind::TexView) {
                Default::default()
            } else {
                ms
            },
            fragment,
            multiview_mask: None,
            cache: gpu.pipeline_cache.as_ref(),
        })
    }

    /// Creates the pipelines a typical scene needs, so the first frame
    /// doesn't wait for them.  Optional; the helper calls it at startup.
    pub fn warm_up(&mut self, gpu: &Gpu) {
        self.ensure_environment(gpu);
        let samples = 4;
        for format in [self.format, READBACK_FORMAT] {
            for blend in [false, true] {
                self.pipeline(gpu, Kind::Surface { topology: 0, cull: true, blend, mirrored: false }, format, samples);
            }
            self.pipeline(gpu, Kind::Ground, format, samples);
        }
        self.pipeline(gpu, Kind::Shadow, DEPTH_FORMAT, 1);
        self.pipeline(gpu, Kind::Contact, CONTACT_FORMAT, 1);
        if RenderSettings::thumbnail().ao {
            self.pipeline(gpu, Kind::Prepass { cull: true, mirrored: false }, NORMAL_FORMAT, 1);
            self.ao.get_or_insert_with(|| crate::ao::Ao::new(gpu));
        }
        self.save_cache(gpu);
    }

    fn save_cache(&mut self, gpu: &Gpu) {
        if self.pipelines_made {
            let step = crate::trace::Step::new("save pipeline cache");
            self.pipelines_made = false;
            gpu.save_pipeline_cache();
            step.done(&gpu.device);
        }
    }

    pub(crate) fn texview_pipeline(&mut self, gpu: &Gpu, format: wgpu::TextureFormat) -> wgpu::RenderPipeline {
        self.pipeline(gpu, Kind::TexView, format, 1)
    }

    pub(crate) fn finish_frame(&mut self, gpu: &Gpu) {
        self.save_cache(gpu);
    }

    //endregion

    /*┌─────────────────╮
    | Rendering       |
    └─────────────────╯*/
    //region Rendering

    /// Draws `scene` into `target`, a view of a texture in the renderer's
    /// target format, `size` pixels large.  Submits its own commands.
    pub fn render(
        &mut self,
        gpu: &Gpu,
        scene: &GpuScene,
        view: &View,
        settings: &RenderSettings,
        target: &wgpu::TextureView,
        size: (u32, u32),
    ) {
        let mut encoder = gpu.device.create_command_encoder(&Default::default());
        self.encode(gpu, &mut encoder, scene, view, settings, target, size, self.format);
        gpu.queue.submit([encoder.finish()]);
        self.save_cache(gpu);
    }

    /// Renders offscreen and reads the picture back, for thumbnails and
    /// tests.  Alpha is straight (not premultiplied), as PNG wants.
    pub fn render_image(
        &mut self,
        gpu: &Gpu,
        scene: &GpuScene,
        view: &View,
        settings: &RenderSettings,
        width: u32,
        height: u32,
    ) -> Image8 {
        self.offscreen(gpu, width, height, |r, encoder, target, size, format| {
            r.encode(gpu, encoder, scene, view, settings, target, size, format)
        })
    }

    /// Runs `draw` into an offscreen sRGB target and reads it back.
    pub(crate) fn offscreen(
        &mut self,
        gpu: &Gpu,
        width: u32,
        height: u32,
        draw: impl FnOnce(&mut Renderer, &mut wgpu::CommandEncoder, &wgpu::TextureView, (u32, u32), wgpu::TextureFormat),
    ) -> Image8 {
        let max = gpu.device.limits().max_texture_dimension_2d;
        let (width, height) = (width.clamp(1, max), height.clamp(1, max));
        if self.readback.as_ref().is_none_or(|r| r.size != (width, height)) {
            self.readback = Some(make_readback(&gpu.device, width, height));
        }
        let rb = self.readback.take().unwrap();
        let mut encoder = gpu.device.create_command_encoder(&Default::default());
        draw(self, &mut encoder, &rb.view, (width, height), READBACK_FORMAT);
        let step = crate::trace::Step::new("draw and read back");
        encoder.copy_texture_to_buffer(
            rb.texture.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &rb.buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(rb.row_bytes),
                    rows_per_image: None,
                },
            },
            wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
        );
        gpu.queue.submit([encoder.finish()]);
        let image = read_image(gpu, &rb.buffer, width, height, rb.row_bytes);
        step.done(&gpu.device);
        self.readback = Some(rb);
        self.save_cache(gpu);
        image
    }

    fn ensure_targets(&mut self, device: &wgpu::Device, size: (u32, u32), samples: u32, format: wgpu::TextureFormat) {
        if self.targets.as_ref().is_some_and(|t| t.size == size && t.samples == samples && t.format == format) {
            return;
        }
        let tex = |format, label| {
            device
                .create_texture(&wgpu::TextureDescriptor {
                    label: Some(label),
                    size: wgpu::Extent3d { width: size.0, height: size.1, depth_or_array_layers: 1 },
                    mip_level_count: 1,
                    sample_count: samples,
                    dimension: wgpu::TextureDimension::D2,
                    format,
                    usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                    view_formats: &[],
                })
                .create_view(&Default::default())
        };
        self.targets = Some(Targets {
            size,
            samples,
            format,
            color: (samples > 1).then(|| tex(format, "msaa colour")),
            depth: tex(DEPTH_FORMAT, "depth"),
        });
    }

    #[allow(clippy::too_many_arguments)]
    fn encode(
        &mut self,
        gpu: &Gpu,
        encoder: &mut wgpu::CommandEncoder,
        scene: &GpuScene,
        view: &View,
        settings: &RenderSettings,
        target: &wgpu::TextureView,
        size: (u32, u32),
        format: wgpu::TextureFormat,
    ) {
        let step = crate::trace::Step::new("environment and targets");
        self.ensure_environment(gpu);
        let size = (size.0.max(1), size.1.max(1));
        let multisamples = gpu.adapter.get_texture_format_features(format).flags.sample_count_supported(4);
        let samples = if settings.msaa && multisamples { 4 } else { 1 };
        self.ensure_targets(&gpu.device, size, samples, format);
        step.done(&gpu.device);

        let step = crate::trace::Step::new("frame setup");
        // 1. Which draws go where: opaque surfaces and points first, then
        //    transmissive surfaces (which show the opaque ones), then blended
        //    ones back to front, each a run of the instances the camera
        //    sees, whose list goes up now.  Pipelines are made before any
        //    pass borrows the targets.
        let mut runs = FrameRuns::default();
        runs.build(scene, &view.view, Frustum::new(&view.view_proj()).as_ref());
        if !runs.culled.is_empty() {
            let after_listed = (scene.draws.len() * 4) as u64;
            gpu.queue.write_buffer(&scene.instances, after_listed, bytemuck::cast_slice(&runs.culled));
        }
        let mut with_pipelines = |list: &[Run]| -> Vec<Draw> {
            let mut pipeline = |r: &Run| {
                let prim = &scene.prims[r.prim];
                let kind = surface_kind(prim.topology, &scene.materials[prim.material], r.mirrored);
                self.pipeline(gpu, kind, format, samples)
            };
            list.iter().map(|r| (*r, pipeline(r))).collect()
        };
        let opaque = with_pipelines(&runs.opaque);
        let transmissive = with_pipelines(&runs.transmissive);
        let blended = with_pipelines(&runs.blended);
        let plain = settings.debug == DebugChannel::None;
        let ao = settings.ao
            && settings.ao_strength > 0.0
            && runs.occludable
            && (plain || settings.debug == DebugChannel::Occlusion);
        let transmission = !transmissive.is_empty() && plain;

        // 2. Frame constants for the main pass, the two shadow passes and
        //    the transmission pass.
        let frames = self.frame_uniforms(scene, view, settings, size, format, samples, ao, transmission);
        for (buffer, frame) in self.frame_buffers.iter().zip(&frames.uniforms) {
            gpu.queue.write_buffer(buffer, 0, bytemuck::bytes_of(frame));
        }

        // 3. Shadow maps, when something they show changed.
        let lit = frames.light_strength > 0.0 && !scene.draws.is_empty();
        let shadows = settings.shadows && lit;
        let ground_shadow = settings.ground_shadow && scene.ground_shadow && !scene.draws.is_empty();
        let key = [
            scene.id,
            scene.pose_version,
            hash_mat(&frames.uniforms[1].view_proj),
            hash_mat(&frames.uniforms[2].view_proj),
        ];
        if (shadows || ground_shadow) && self.shadow_key != Some(key) {
            self.shadow_key = Some(key);
            self.shadow_passes(gpu, encoder, scene, ground_shadow);
        }

        // 4. CPU-built overlay lines.
        let (lines, on_top) = crate::overlays::lines(scene, settings, frames.line_length);
        if !lines.is_empty() {
            let bytes = (lines.len() * std::mem::size_of::<LineVertex>()) as u64;
            if self.lines.as_ref().is_none_or(|(_, cap)| *cap < bytes) {
                let cap = bytes.next_power_of_two().max(4096);
                let buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("overlay lines"),
                    size: cap,
                    usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                });
                self.lines = Some((buffer, cap));
            }
            gpu.queue.write_buffer(&self.lines.as_ref().unwrap().0, 0, bytemuck::cast_slice(&lines));
        }

        // 5. The other pipelines.
        let background = matches!(settings.background, Background::Environment { .. })
            .then(|| self.pipeline(gpu, Kind::Background, format, samples));
        let ground = ground_shadow.then(|| self.pipeline(gpu, Kind::Ground, format, samples));
        let grid = settings.overlays.grid.then(|| self.pipeline(gpu, Kind::Grid, format, samples));
        let wire = settings.overlays.wireframe.then(|| self.pipeline(gpu, Kind::Wire, format, samples));
        let normals = settings.overlays.normals.then(|| self.pipeline(gpu, Kind::Normals, format, samples));
        let tangents = settings.overlays.tangents.then(|| self.pipeline(gpu, Kind::Tangents, format, samples));
        let line_pipes = (!lines.is_empty()).then(|| {
            (
                self.pipeline(gpu, Kind::Lines { on_top: false }, format, samples),
                self.pipeline(gpu, Kind::Lines { on_top: true }, format, samples),
            )
        });

        // 5a. Gaussian splats, projected, sorted and drawn into their own
        //     layer, which the main pass lays over the background.
        let splats = self.encode_splats(gpu, encoder, scene, view, settings, size, format, samples);

        // 5b. Screen-sized textures for occlusion and transmission, and the
        //     lighting groups that read them.
        if ao {
            let made = self.ao.is_none();
            let pass = self.ao.get_or_insert_with(|| crate::ao::Ao::new(gpu));
            self.lighting_dirty |= pass.ensure_targets(gpu, size);
            self.pipelines_made |= made;
        }
        if transmission && self.transmission.as_ref().is_none_or(|t| t.size != size) {
            self.transmission = Some(crate::transmission::Transmission::new(gpu, size));
            self.lighting_dirty = true;
        }
        self.ensure_lighting(gpu);
        step.done(&gpu.device);

        // 6. Ambient occlusion, then the opaque scene that transmissive
        //    surfaces see through.
        if ao {
            self.encode_ao(gpu, encoder, scene, view, settings, &opaque, size);
        }
        if transmission {
            let clear = frames.uniforms[0].background.map(f64::from);
            let clear = wgpu::Color { r: clear[0], g: clear[1], b: clear[2], a: clear[3] };
            self.encode_transmission(gpu, encoder, scene, settings, &opaque, ground_shadow, clear);
        }

        // 7. The main pass.
        let clear = match settings.background {
            Background::Transparent => wgpu::Color::TRANSPARENT,
            Background::Environment { .. } => wgpu::Color::BLACK,
            Background::Color(c) => {
                let a = c[3].clamp(0.0, 1.0) as f64;
                let rgb = Vec3::new(c[0], c[1], c[2]).clamp(Vec3::ZERO, Vec3::ONE);
                let rgb = if format.is_srgb() { srgb_to_linear(rgb) } else { rgb };
                wgpu::Color { r: rgb.x as f64 * a, g: rgb.y as f64 * a, b: rgb.z as f64 * a, a }
            }
        };
        let targets = self.targets.as_ref().unwrap();
        let (attach, resolve) = match &targets.color {
            Some(msaa) => (msaa, Some(target)),
            None => (target, None),
        };
        let lighting = self.lighting.as_ref().unwrap();
        let frame_group = &scene.frame_groups[0];
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("main"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: attach,
                depth_slice: None,
                resolve_target: resolve,
                ops: wgpu::Operations { load: wgpu::LoadOp::Clear(clear), store: wgpu::StoreOp::Store },
            })],
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: &targets.depth,
                depth_ops: Some(wgpu::Operations { load: wgpu::LoadOp::Clear(1.0), store: wgpu::StoreOp::Discard }),
                stencil_ops: None,
            }),
            ..Default::default()
        });
        let empty = &self.empty_group;
        let backdrop = |pass: &mut wgpu::RenderPass, pipeline: &wgpu::RenderPipeline, vertices: u32| {
            pass.set_pipeline(pipeline);
            pass.set_bind_group(0, frame_group, &[]);
            pass.set_bind_group(1, empty, &[]);
            pass.set_bind_group(2, empty, &[]);
            pass.set_bind_group(3, lighting, &[]);
            pass.draw(0..vertices, 0..1);
        };
        if let Some(p) = &background {
            backdrop(&mut pass, p, 3);
        }
        let draw_surface = |pass: &mut wgpu::RenderPass, (run, pipeline): &Draw| {
            pass.set_pipeline(pipeline);
            draw_run(pass, scene, run, frame_group, Some(lighting));
        };
        for d in &opaque {
            draw_surface(&mut pass, d);
        }
        if let Some(p) = &ground {
            backdrop(&mut pass, p, 6);
        }
        if let Some(p) = &grid {
            backdrop(&mut pass, p, 6);
        }
        if let Some((pipeline, layer)) = &splats {
            pass.set_pipeline(pipeline);
            pass.set_bind_group(0, frame_group, &[]);
            pass.set_bind_group(1, layer, &[]);
            pass.draw(0..3, 0..1);
        }
        for d in transmissive.iter().chain(&blended) {
            draw_surface(&mut pass, d);
        }
        // Overlays that read scene geometry, a call per group.
        for (pipeline, what) in [(&wire, 0), (&normals, 1), (&tangents, 2)] {
            let Some(pipeline) = pipeline else { continue };
            for g in &scene.groups {
                let prim = &scene.prims[g.prim];
                if g.visible() == 0 || (what == 0 && prim.topology != Topology::Triangles) {
                    continue;
                }
                if what == 2 && prim.attrs0[2] == crate::resources::NONE {
                    continue;
                }
                let chunk = &scene.chunks[prim.chunk];
                pass.set_pipeline(pipeline);
                pass.set_bind_group(0, frame_group, &[]);
                pass.set_bind_group(1, &scene.materials[prim.material].group, &[]);
                pass.set_bind_group(2, &chunk.group, &[]);
                pass.set_bind_group(3, lighting, &[]);
                let count = if what == 0 { prim.index_count } else { prim.vertex_count * 2 };
                pass.draw(0..count, g.first..g.first + g.visible());
            }
        }
        if let (Some((depth_tested, top)), Some((buffer, _))) = (&line_pipes, &self.lines) {
            let split = (lines.len() - on_top) as u32;
            pass.set_vertex_buffer(0, buffer.slice(..));
            for (pipeline, range) in [(depth_tested, 0..split), (top, split..lines.len() as u32)] {
                if range.is_empty() {
                    continue;
                }
                pass.set_pipeline(pipeline);
                pass.set_bind_group(0, frame_group, &[]);
                pass.set_bind_group(1, empty, &[]);
                pass.set_bind_group(2, empty, &[]);
                pass.set_bind_group(3, lighting, &[]);
                pass.draw(range, 0..1);
            }
        }
    }

    /// The ambient occlusion pre-pass over the opaque draws, then the
    /// occlusion and blur passes.
    #[allow(clippy::too_many_arguments)]
    fn encode_ao(
        &mut self,
        gpu: &Gpu,
        encoder: &mut wgpu::CommandEncoder,
        scene: &GpuScene,
        view: &View,
        settings: &RenderSettings,
        opaque: &[Draw],
        size: (u32, u32),
    ) {
        let step = crate::trace::Step::new("ambient occlusion");
        let mut draws = Vec::with_capacity(opaque.len());
        for (run, _) in opaque {
            let prim = &scene.prims[run.prim];
            let kind = match prim.topology {
                Topology::Triangles => {
                    Kind::Prepass { cull: !scene.materials[prim.material].double_sided, mirrored: run.mirrored }
                }
                Topology::Points => Kind::PointsPrepass,
                Topology::Lines => continue,
            };
            draws.push((*run, self.pipeline(gpu, kind, NORMAL_FORMAT, 1)));
        }
        let Some(ao) = &self.ao else { return };
        let Some(t) = &ao.targets else { return };
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("ao pre-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &t.normals,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &t.depth,
                    depth_ops: Some(wgpu::Operations { load: wgpu::LoadOp::Clear(1.0), store: wgpu::StoreOp::Store }),
                    stencil_ops: None,
                }),
                ..Default::default()
            });
            for (run, pipeline) in &draws {
                pass.set_pipeline(pipeline);
                draw_run(&mut pass, scene, run, &scene.frame_groups[0], None);
            }
        }
        // The reach follows the scene's size at rest, so it doesn't change
        // as a model animates.
        let bounds = if scene.rest_bounds.is_empty() { scene.bounds } else { scene.rest_bounds };
        let radius = settings.ao_radius.clamp(1e-4, 1.0) * bounds.radius().max(1e-6);
        // Plain point clouds are shaded towards the key light, or towards
        // the camera without one.
        let (dir, color) = self.key_light(settings);
        let light =
            if color.max_element() > 0.0 { view.view.transform_vector3(dir).normalize_or_zero() } else { Vec3::Z };
        ao.encode(gpu, encoder, &crate::ao::AoParams::new(&view.proj, radius, size, light));
        step.done(&gpu.device);
    }

    /// Draws what transmissive surfaces see through into the transmission
    /// texture, in linear light, and fills its mips.
    #[allow(clippy::too_many_arguments)]
    fn encode_transmission(
        &mut self,
        gpu: &Gpu,
        encoder: &mut wgpu::CommandEncoder,
        scene: &GpuScene,
        settings: &RenderSettings,
        opaque: &[Draw],
        ground_shadow: bool,
        clear: wgpu::Color,
    ) {
        let step = crate::trace::Step::new("transmission");
        let format = TRANSMISSION_FORMAT;
        let mut draws = Vec::with_capacity(opaque.len());
        for (run, _) in opaque {
            let prim = &scene.prims[run.prim];
            let kind = surface_kind(prim.topology, &scene.materials[prim.material], run.mirrored);
            draws.push((*run, self.pipeline(gpu, kind, format, 1)));
        }
        let environment = matches!(settings.background, Background::Environment { .. });
        let background = environment.then(|| self.pipeline(gpu, Kind::Background, format, 1));
        let ground = ground_shadow.then(|| self.pipeline(gpu, Kind::Ground, format, 1));
        let grid = settings.overlays.grid.then(|| self.pipeline(gpu, Kind::Grid, format, 1));
        let (Some(t), Some(lighting)) = (&self.transmission, &self.lighting_source) else { return };
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("transmission"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &t.target,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(if environment { wgpu::Color::BLACK } else { clear }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &t.depth,
                    depth_ops: Some(wgpu::Operations { load: wgpu::LoadOp::Clear(1.0), store: wgpu::StoreOp::Discard }),
                    stencil_ops: None,
                }),
                ..Default::default()
            });
            let frame = &scene.frame_groups[3];
            let empty = &self.empty_group;
            let backdrop = |pass: &mut wgpu::RenderPass, pipeline: &wgpu::RenderPipeline, vertices: u32| {
                pass.set_pipeline(pipeline);
                pass.set_bind_group(0, frame, &[]);
                pass.set_bind_group(1, empty, &[]);
                pass.set_bind_group(2, empty, &[]);
                pass.set_bind_group(3, lighting, &[]);
                pass.draw(0..vertices, 0..1);
            };
            if let Some(p) = &background {
                backdrop(&mut pass, p, 3);
            }
            for (run, pipeline) in &draws {
                pass.set_pipeline(pipeline);
                draw_run(&mut pass, scene, run, frame, Some(lighting));
            }
            if let Some(p) = &ground {
                backdrop(&mut pass, p, 6);
            }
            if let Some(p) = &grid {
                backdrop(&mut pass, p, 6);
            }
        }
        self.mipgen.encode(gpu, encoder, &t.texture, format, 1, t.mips);
        step.done(&gpu.device);
    }

    /// The key light's depth map and the contact map seen from below the
    /// ground.
    fn shadow_passes(&mut self, gpu: &Gpu, encoder: &mut wgpu::CommandEncoder, scene: &GpuScene, with_contact: bool) {
        let shadow = self.pipeline(gpu, Kind::Shadow, DEPTH_FORMAT, 1);
        let contact = self.pipeline(gpu, Kind::Contact, CONTACT_FORMAT, 1);
        for (pass_index, pipeline) in [(1usize, &shadow), (2, &contact)] {
            if pass_index == 2 && !with_contact {
                continue;
            }
            let color = (pass_index == 2).then_some(wgpu::RenderPassColorAttachment {
                view: &self.contact_view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                    store: wgpu::StoreOp::Store,
                },
            });
            let colors = [color];
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some(if pass_index == 1 { "shadow" } else { "contact" }),
                color_attachments: if pass_index == 2 { &colors[..] } else { &[] },
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: if pass_index == 1 { &self.shadow_view } else { &self.contact_depth },
                    depth_ops: Some(wgpu::Operations { load: wgpu::LoadOp::Clear(1.0), store: wgpu::StoreOp::Store }),
                    stencil_ops: None,
                }),
                ..Default::default()
            });
            pass.set_pipeline(pipeline);
            pass.set_bind_group(0, &scene.frame_groups[pass_index], &[]);
            // Every visible copy casts, so a group is one call, uncut.
            for g in &scene.groups {
                let prim = &scene.prims[g.prim];
                let mat = &scene.materials[prim.material];
                if g.visible() == 0 || mat.blend || !mat.casts_shadow || prim.topology != Topology::Triangles {
                    continue;
                }
                let chunk = &scene.chunks[prim.chunk];
                pass.set_bind_group(1, &mat.group, &[]);
                pass.set_bind_group(2, &chunk.group, &[]);
                pass.set_index_buffer(chunk.buffer.slice(..), wgpu::IndexFormat::Uint32);
                let instances = g.first..g.first + g.visible();
                pass.draw_indexed(prim.index_first..prim.index_first + prim.index_count, 0, instances);
            }
        }
    }

    //endregion
}

/*┌─────────────────╮
| Frame constants |
└─────────────────╯*/
//region Frame constants

struct Frames {
    /// Main, shadow, contact and transmission passes.
    uniforms: [FrameUniform; 4],
    light_strength: f32,
    line_length: f32,
}

fn cols(m: Mat4) -> [[f32; 4]; 4] {
    m.to_cols_array_2d()
}

fn hash_mat(m: &[[f32; 4]; 4]) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for v in m.iter().flatten() {
        h ^= v.to_bits() as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

pub(crate) fn srgb_to_linear(c: Vec3) -> Vec3 {
    let f = |x: f32| if x <= 0.04045 { x / 12.92 } else { ((x + 0.055) / 1.055).powf(2.4) };
    Vec3::new(f(c.x), f(c.y), f(c.z))
}

impl Renderer {
    /// `ao` and `transmission` say whether this frame runs those passes.
    #[allow(clippy::too_many_arguments)]
    fn frame_uniforms(
        &self,
        scene: &GpuScene,
        view: &View,
        settings: &RenderSettings,
        size: (u32, u32),
        format: wgpu::TextureFormat,
        samples: u32,
        ao: bool,
        transmission: bool,
    ) -> Frames {
        let bounds =
            if scene.bounds.is_empty() { Aabb { min: Vec3::splat(-0.5), max: Vec3::splat(0.5) } } else { scene.bounds };
        let center = bounds.center();
        let radius = bounds.radius().max(1e-3);
        let ortho = view.proj.z_axis.w == 0.0;
        let forward = -view.view.row(2).truncate().normalize_or_zero();
        let view_proj = view.proj * view.view;

        // 1. The key light's shadow map covers the scene's bounding sphere.
        let (light_dir, light_color) = self.key_light(settings);
        let light_strength = if light_color.max_element() > 0.0 { 1.0 } else { 0.0 };
        let up = if light_dir.y.abs() > 0.99 { Vec3::Z } else { Vec3::Y };
        let light_view = view::look_at_mat4(center + light_dir * radius * 2.0, center, up);
        let light_proj = proj::orthographic(-radius, radius, -radius, radius, radius * 0.9, radius * 3.1);
        let light_view_proj = light_proj * light_view;
        let shadow_size = self.config.shadow_map_size.max(64) as f32;
        let texel_world = 2.0 * radius / shadow_size;
        let depth_range = radius * 2.2;

        // 2. The ground, its contact map seen from below, and the grid.  The
        //    ground sits a hair below the lowest point, so a floor that is
        //    part of the scene hides it instead of fighting it for depth.
        let ground_y = scene.ground_height - radius * 0.002;
        let extent = bounds.size();
        let ground_radius = (extent.x.max(extent.z) * 0.5 + radius * 0.35).max(radius * 0.6);
        let ground_center = Vec3::new(center.x, ground_y, center.z);
        let contact_height = (extent.max_element() * 0.3).max(1e-3);
        let contact_view = view::look_to_mat4(ground_center, Vec3::Y, Vec3::NEG_Z);
        let contact_proj = proj::orthographic(
            -ground_radius,
            ground_radius,
            -ground_radius,
            ground_radius,
            -contact_height * 0.01,
            contact_height,
        );
        let contact_view_proj = contact_proj * contact_view;
        let spacing = 10f32.powf(radius.max(1e-3).log10().floor());
        let grid_radius = (radius * 6.0).max(spacing * 10.0);

        // 3. Tone, environment and background.
        let env_mips = self.env.as_ref().map_or(1, |e| e.mip_count) as f32;
        let blur = match settings.background {
            Background::Environment { blur } => blur.clamp(0.0, 1.0) * (env_mips - 1.0),
            _ => 0.0,
        };
        let background = match settings.background {
            Background::Color(c) => {
                let a = c[3].clamp(0.0, 1.0);
                let rgb = srgb_to_linear(Vec3::new(c[0], c[1], c[2]).clamp(Vec3::ZERO, Vec3::ONE)) * a;
                [rgb.x, rgb.y, rgb.z, a]
            }
            _ => [0.0; 4],
        };
        let (rs, rc) = settings.environment_rotation.sin_cos();
        let tonemap = match settings.tonemap {
            Tonemap::None => 0.0,
            Tonemap::PbrNeutral => 1.0,
            Tonemap::Aces => 2.0,
        };
        let debug = match settings.debug {
            DebugChannel::None => 0.0,
            DebugChannel::BaseColor => 1.0,
            DebugChannel::Normal => 2.0,
            DebugChannel::Metallic => 3.0,
            DebugChannel::Roughness => 4.0,
            DebugChannel::Occlusion => 5.0,
            DebugChannel::Emissive => 6.0,
            DebugChannel::UvChecker => 7.0,
        };
        let line_length = radius * 0.04;

        // 4. Points: on screen, a size of 1 makes neighbouring dots meet when
        //    the scene at rest fills the view.
        let rest = if scene.rest_bounds.is_empty() { bounds } else { scene.rest_bounds };
        let fill = size.0.min(size.1) as f32 / (2.0 * rest.radius().max(1e-6));
        let point_size = if settings.point_size.is_finite() { settings.point_size.clamp(0.01, 100.0) } else { 1.0 };
        let world_points = if settings.point_world_size { 1.0 } else { 0.0 };
        let ao_strength =
            if ao && settings.ao_strength.is_finite() { settings.ao_strength.clamp(0.0, 8.0) } else { 0.0 };
        let fill_light = if settings.fill.is_finite() { settings.fill.clamp(0.0, 4.0) } else { 0.0 };
        let largest_mip = (32 - size.0.max(size.1).leading_zeros()).saturating_sub(1) as f32;
        let main = FrameUniform {
            view_proj: cols(view_proj),
            view: cols(view.view),
            inv_view_proj: cols(view_proj.inverse()),
            light_view_proj: cols(light_view_proj),
            contact_view_proj: cols(contact_view_proj),
            camera: view.position.extend(if ortho { 1.0 } else { 0.0 }).to_array(),
            camera_forward: forward.extend(0.0).to_array(),
            light_dir: light_dir.extend(light_strength * 0.9).to_array(),
            light_color: light_color.extend(1.0 / shadow_size).to_array(),
            env: [rc, rs, settings.environment_intensity.max(0.0), env_mips - 1.0],
            tone: [2f32.powf(settings.exposure), tonemap, if format.is_srgb() { 1.0 } else { 0.0 }, blur],
            ground: [ground_y, contact_height, ground_radius, 0.75],
            ground_center: [ground_center.x, ground_center.z, spacing, grid_radius],
            viewport: [size.0 as f32, size.1 as f32, 1.0 / size.0 as f32, 1.0 / size.1 as f32],
            misc: [settings.time, line_length, debug, 0.85],
            background,
            shadow: [
                texel_world * 1.5,
                texel_world * 2.0 / depth_range,
                0.035,
                if settings.shadows { 1.0 } else { 0.0 },
            ],
            ao: [ao_strength, fill_light, 0.0, 0.0],
            points: [point_size, world_points, fill, samples as f32],
            proj: [view.proj.x_axis.x, view.proj.y_axis.y, 0.0, 0.0],
            transmission: [largest_mip, if transmission { 1.0 } else { 0.0 }, 0.0, 0.0],
        };
        let shadow = FrameUniform { view_proj: cols(light_view_proj), ..main };
        let contact = FrameUniform { view_proj: cols(contact_view_proj), ..main };
        // What transmissive surfaces see: scene-referred light, untouched
        // by exposure and tone mapping (they apply both once, to the
        // result), drawn without multisampling.
        let behind = FrameUniform {
            tone: [1.0, 3.0, 1.0, main.tone[3]],
            points: [point_size, world_points, fill, 1.0],
            transmission: [0.0; 4],
            ..main
        };
        Frames { uniforms: [main, shadow, contact, behind], light_strength, line_length }
    }
}

//endregion

/*┌─────────────────╮
| Readback        |
└─────────────────╯*/
//region Readback

fn make_readback(device: &wgpu::Device, width: u32, height: u32) -> Readback {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("readback"),
        size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: READBACK_FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let row_bytes = (width * 4).div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT) * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("readback"),
        size: row_bytes as u64 * height as u64,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let view = texture.create_view(&Default::default());
    Readback { size: (width, height), texture, view, buffer, row_bytes }
}

/// Maps the readback buffer and converts premultiplied, sRGB-encoded
/// pixels to straight alpha.  Dividing must happen in linear light,
/// because premultiplication happened there.
fn read_image(gpu: &Gpu, buffer: &wgpu::Buffer, width: u32, height: u32, row_bytes: u32) -> Image8 {
    let slice = buffer.slice(..);
    slice.map_async(wgpu::MapMode::Read, |_| {});
    let _ = gpu.device.poll(wgpu::PollType::wait_indefinitely());
    let mut image = Image8::new(width, height);
    {
        let data = slice.get_mapped_range();
        for y in 0..height as usize {
            let src = &data[y * row_bytes as usize..y * row_bytes as usize + width as usize * 4];
            image.pixels[y * width as usize * 4..(y + 1) * width as usize * 4].copy_from_slice(src);
        }
    }
    buffer.unmap();
    unpremultiply_srgb(&mut image.pixels);
    image
}

pub(crate) fn unpremultiply_srgb(pixels: &mut [u8]) {
    let decode: Vec<f32> = (0..256)
        .map(|i| {
            let c = i as f32 / 255.0;
            if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
        })
        .collect();
    for px in pixels.chunks_exact_mut(4) {
        let a = px[3];
        if a == 0 {
            px[0] = 0;
            px[1] = 0;
            px[2] = 0;
        } else if a < 255 {
            let inv = 255.0 / a as f32;
            for c in &mut px[..3] {
                let lin = (decode[*c as usize] * inv).min(1.0);
                let enc = if lin <= 0.0031308 { lin * 12.92 } else { 1.055 * lin.powf(1.0 / 2.4) - 0.055 };
                *c = (enc * 255.0 + 0.5) as u8;
            }
        }
    }
}

//endregion

pub(crate) fn buffer_init(device: &wgpu::Device, label: &str, data: &[u8], usage: wgpu::BufferUsages) -> wgpu::Buffer {
    device.create_buffer_init(&wgpu::util::BufferInitDescriptor { label: Some(label), contents: data, usage })
}
