//! GPU-side data layouts shared with the WGSL, bind group layouts, samplers,
//! texture upload and mipmap generation.

use bytemuck::{Pod, Zeroable};
use slerp_scene::textures::{BlockFormat, Pixels, TextureKind};
use slerp_scene::{Filter, Sampler, Texture, Wrap};
use std::collections::HashMap;

/*┌─────────────────╮
| Shared structs  |
└─────────────────╯*/
//region Shared structs

/// `Frame` in common.wgsl.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub(crate) struct FrameUniform {
    pub view_proj: [[f32; 4]; 4],
    pub view: [[f32; 4]; 4],
    pub inv_view_proj: [[f32; 4]; 4],
    pub light_view_proj: [[f32; 4]; 4],
    pub contact_view_proj: [[f32; 4]; 4],
    pub camera: [f32; 4],
    pub camera_forward: [f32; 4],
    pub light_dir: [f32; 4],
    pub light_color: [f32; 4],
    pub env: [f32; 4],
    pub tone: [f32; 4],
    pub ground: [f32; 4],
    pub ground_center: [f32; 4],
    pub viewport: [f32; 4],
    pub misc: [f32; 4],
    pub background: [f32; 4],
    pub shadow: [f32; 4],
    pub ao: [f32; 4],
    pub points: [f32; 4],
    pub proj: [f32; 4],
    pub transmission: [f32; 4],
}

/// `Draw` in common.wgsl.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub(crate) struct DrawUniform {
    pub model: [[f32; 4]; 4],
    pub normal: [[f32; 4]; 4],
    pub attrs0: [u32; 4],
    pub attrs1: [u32; 4],
    pub morph: [u32; 4],
    pub misc: [u32; 4],
}

/// `Material` in scene.wgsl.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub(crate) struct MaterialUniform {
    pub base_color: [f32; 4],
    pub emissive: [f32; 4],
    pub specular: [f32; 4],
    pub pbr: [f32; 4],
    pub extra: [f32; 4],
    pub extra2: [f32; 4],
    pub flags: [u32; 4],
    pub transmission: [f32; 4],
    pub attenuation: [f32; 4],
    /// Distance fog: linear RGB and density per metre (0 for none).
    pub fog: [f32; 4],
    /// Brightening near the camera: factor at the camera, distance where
    /// it ends (0 for none), 1 when the material is shaded as Doom does
    /// (`Material::fog` is set), 0.
    pub fog_near: [f32; 4],
    pub slots: [[f32; 8]; SLOTS],
}

/// Texture slots of a material: base colour, metallic-roughness, normal,
/// occlusion, emissive, lightmap, height, transmission and thickness.
pub(crate) const SLOTS: usize = 9;

/// `TexParams` in texview.wgsl.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub(crate) struct TexViewUniform {
    pub xform: [f32; 4],
    pub tex: [f32; 4],
    pub color: [f32; 4],
    pub mask: [f32; 4],
    pub flags: [u32; 4],
    pub background: [f32; 4],
}

/// A vertex of the lines the CPU builds for overlays.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub(crate) struct LineVertex {
    pub pos: [f32; 3],
    pub color: [f32; 4],
}

pub(crate) const NONE: u32 = u32::MAX;
pub(crate) const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;
pub(crate) const CONTACT_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::R16Float;
/// The ambient occlusion pre-pass's view-space normals.
pub(crate) const NORMAL_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;
/// Ambient occlusion, the view depth it belongs to, and the visibility of
/// reflections.
pub(crate) const AO_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;
/// The opaque scene in linear light, which transmissive surfaces see
/// through.
pub(crate) const TRANSMISSION_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;

//endregion

/*┌─────────────────╮
| Layouts         |
└─────────────────╯*/
//region Layouts

pub(crate) struct Layouts {
    pub frame: wgpu::BindGroupLayout,
    pub material: wgpu::BindGroupLayout,
    pub geometry: wgpu::BindGroupLayout,
    pub lighting: wgpu::BindGroupLayout,
    pub texview: wgpu::BindGroupLayout,
    /// No bindings: fills the material and geometry slots of `backdrop`.
    pub empty: wgpu::BindGroupLayout,
    /// Frame, material, geometry and lighting: the main pass.
    pub scene: wgpu::PipelineLayout,
    /// Without lighting, for the passes that render the shadow maps and
    /// the ambient occlusion pre-pass.
    pub depth: wgpu::PipelineLayout,
    /// Frame and lighting, for the background, ground, grid and overlay
    /// lines, which draw between scene draws in the same pass.  Slots 1
    /// and 2 hold empty groups rather than nothing.  With nothing there,
    /// wgpu 29 treats switching from `scene` to this layout as keeping the
    /// lighting group bound, and drops binding the same group again as
    /// redundant.  That is harmless on Vulkan, which binds groups by
    /// number, but Metal and Direct3D 12 number resources across all groups
    /// of a layout, and the lighting textures sit after the material's in
    /// `scene`: the ground then sampled a material texture where it meant
    /// the contact map, and drew a solid disc.  Empty groups make the
    /// switch a change at slot 1, which binds everything after it again.
    pub backdrop: wgpu::PipelineLayout,
    pub texview_pipeline: wgpu::PipelineLayout,
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

fn storage(binding: u32, vis: wgpu::ShaderStages) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: vis,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Storage { read_only: true },
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    }
}

fn texture(binding: u32, vis: wgpu::ShaderStages, dim: wgpu::TextureViewDimension) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: vis,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: true },
            view_dimension: dim,
            multisampled: false,
        },
        count: None,
    }
}

fn sampler(binding: u32, vis: wgpu::ShaderStages, ty: wgpu::SamplerBindingType) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry { binding, visibility: vis, ty: wgpu::BindingType::Sampler(ty), count: None }
}

impl Layouts {
    pub fn new(device: &wgpu::Device) -> Layouts {
        use wgpu::ShaderStages as S;
        let vf = S::VERTEX | S::FRAGMENT;
        let bgl = |label: &str, entries: &[wgpu::BindGroupLayoutEntry]| {
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor { label: Some(label), entries })
        };
        let frame = bgl(
            "frame",
            &[uniform(0, vf), storage(1, vf), storage(2, S::VERTEX), storage(3, S::VERTEX), storage(4, S::VERTEX)],
        );
        // Textures at 1..=SLOTS, their samplers after them.  With the
        // lighting group's seven, that is 16 textures in the fragment
        // stage: WebGPU's default limit, which every backend meets.
        let mut entries = vec![uniform(0, vf)];
        for i in 0..SLOTS as u32 {
            entries.push(texture(1 + i, vf, wgpu::TextureViewDimension::D2));
        }
        for i in 0..SLOTS as u32 {
            entries.push(sampler(1 + SLOTS as u32 + i, vf, wgpu::SamplerBindingType::Filtering));
        }
        let material = bgl("material", &entries);
        let geometry = bgl("geometry", &[storage(0, S::VERTEX)]);
        let lighting = bgl(
            "lighting",
            &[
                texture(0, S::FRAGMENT, wgpu::TextureViewDimension::Cube),
                sampler(1, S::FRAGMENT, wgpu::SamplerBindingType::Filtering),
                texture(2, S::FRAGMENT, wgpu::TextureViewDimension::D2),
                uniform(3, S::FRAGMENT),
                wgpu::BindGroupLayoutEntry {
                    binding: 4,
                    visibility: S::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Depth,
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                sampler(5, S::FRAGMENT, wgpu::SamplerBindingType::Comparison),
                texture(6, S::FRAGMENT, wgpu::TextureViewDimension::D2),
                sampler(7, S::FRAGMENT, wgpu::SamplerBindingType::Filtering),
                texture(8, S::FRAGMENT, wgpu::TextureViewDimension::Cube),
                texture(9, S::FRAGMENT, wgpu::TextureViewDimension::D2),
                texture(10, S::FRAGMENT, wgpu::TextureViewDimension::D2),
            ],
        );
        let texview = bgl(
            "texview",
            &[
                uniform(0, S::FRAGMENT),
                texture(1, S::FRAGMENT, wgpu::TextureViewDimension::D2Array),
                sampler(2, S::FRAGMENT, wgpu::SamplerBindingType::Filtering),
                sampler(3, S::FRAGMENT, wgpu::SamplerBindingType::Filtering),
            ],
        );
        let pl = |label: &str, groups: &[Option<&wgpu::BindGroupLayout>]| {
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some(label),
                bind_group_layouts: groups,
                immediate_size: 0,
            })
        };
        let scene = pl("scene", &[Some(&frame), Some(&material), Some(&geometry), Some(&lighting)]);
        let depth = pl("depth", &[Some(&frame), Some(&material), Some(&geometry)]);
        let empty = bgl("empty", &[]);
        let backdrop = pl("backdrop", &[Some(&frame), Some(&empty), Some(&empty), Some(&lighting)]);
        let texview_pipeline = pl("texview", &[Some(&texview)]);
        Layouts { frame, material, geometry, lighting, texview, empty, scene, depth, backdrop, texview_pipeline }
    }
}

//endregion

/*┌─────────────────╮
| Samplers        |
└─────────────────╯*/
//region Samplers

/// Samplers made on demand from scene sampler descriptions and kept, since
/// every material slot names one and most are alike.
#[derive(Default)]
pub(crate) struct Samplers {
    made: Vec<(Sampler, wgpu::Sampler)>,
}

impl Samplers {
    pub fn get(&mut self, device: &wgpu::Device, s: &Sampler) -> wgpu::Sampler {
        if let Some((_, made)) = self.made.iter().find(|(k, _)| k == s) {
            return made.clone();
        }
        let wrap = |w: Wrap| match w {
            Wrap::Repeat => wgpu::AddressMode::Repeat,
            Wrap::MirroredRepeat => wgpu::AddressMode::MirrorRepeat,
            Wrap::ClampToEdge => wgpu::AddressMode::ClampToEdge,
        };
        let filter = |f: Filter| match f {
            Filter::Nearest => wgpu::FilterMode::Nearest,
            Filter::Linear => wgpu::FilterMode::Linear,
        };
        let all_linear = s.mag == Filter::Linear && s.min == Filter::Linear && s.mipmaps;
        let made = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("material"),
            address_mode_u: wrap(s.wrap_u),
            address_mode_v: wrap(s.wrap_v),
            address_mode_w: wgpu::AddressMode::Repeat,
            mag_filter: filter(s.mag),
            min_filter: filter(s.min),
            mipmap_filter: if s.mipmaps && s.min == Filter::Linear {
                wgpu::MipmapFilterMode::Linear
            } else {
                wgpu::MipmapFilterMode::Nearest
            },
            lod_max_clamp: if s.mipmaps { 32.0 } else { 0.0 },
            anisotropy_clamp: if all_linear { 8 } else { 1 },
            ..Default::default()
        });
        self.made.push((*s, made.clone()));
        made
    }
}

pub(crate) fn clamp_sampler(device: &wgpu::Device, filter: wgpu::FilterMode) -> wgpu::Sampler {
    device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some("clamp"),
        mag_filter: filter,
        min_filter: filter,
        mipmap_filter: match filter {
            wgpu::FilterMode::Linear => wgpu::MipmapFilterMode::Linear,
            wgpu::FilterMode::Nearest => wgpu::MipmapFilterMode::Nearest,
        },
        ..Default::default()
    })
}

//endregion

/*┌─────────────────╮
| Textures        |
└─────────────────╯*/
//region Textures

/// A texture on the GPU as a 2D array (one layer for material textures;
/// every layer, face or slice for the texture viewer).
pub struct GpuTexture {
    pub(crate) texture: wgpu::Texture,
    /// The data as stored: unorm for 8-bit textures, float for HDR.
    pub(crate) view_raw: wgpu::TextureView,
    /// The same 8-bit data decoded from sRGB when sampled.
    pub(crate) view_srgb: Option<wgpu::TextureView>,
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) layers: u32,
    pub(crate) mips: u32,
    /// Mip levels the file stored, before any were generated.
    pub(crate) file_mips: u32,
    pub(crate) hdr: bool,
    pub(crate) bytes: u64,
    /// 6 when layers hold cube faces.
    pub(crate) faces: u32,
    /// Whether the texture viewer reads it through the sRGB view.
    pub(crate) color: bool,
}

impl GpuTexture {
    /// A view of one layer with all mips, for material slots.
    pub(crate) fn view(&self, srgb: bool) -> wgpu::TextureView {
        let format = match (srgb_twin(self.texture.format()), &self.view_srgb, srgb) {
            (Some(twin), Some(_), true) => twin,
            _ => self.texture.format(),
        };
        self.texture.create_view(&wgpu::TextureViewDescriptor {
            format: Some(format),
            dimension: Some(wgpu::TextureViewDimension::D2),
            array_layer_count: Some(1),
            ..Default::default()
        })
    }
}

/// Which surfaces of a texture to upload.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Surfaces {
    /// Layer 0, face 0, first slice: material textures.
    First,
    /// Every layer and face, and every slice of a 3D texture, as layers.
    All,
}

/// The sRGB format that reads `format`'s data decoded from sRGB, for
/// colour slots, when there is one.
fn srgb_twin(format: wgpu::TextureFormat) -> Option<wgpu::TextureFormat> {
    use wgpu::TextureFormat as F;
    match format {
        F::Rgba8Unorm => Some(F::Rgba8UnormSrgb),
        F::Bc1RgbaUnorm => Some(F::Bc1RgbaUnormSrgb),
        F::Bc3RgbaUnorm => Some(F::Bc3RgbaUnormSrgb),
        F::Bc7RgbaUnorm => Some(F::Bc7RgbaUnormSrgb),
        F::Astc { block, channel: wgpu::AstcChannel::Unorm } => {
            Some(F::Astc { block, channel: wgpu::AstcChannel::UnormSrgb })
        }
        _ => None,
    }
}

/// The GPU format of `blocks`, if the device can sample it.
fn block_format(gpu: &crate::Gpu, blocks: BlockFormat) -> Option<wgpu::TextureFormat> {
    let features = gpu.device.features();
    let bc = features.contains(wgpu::Features::TEXTURE_COMPRESSION_BC);
    match blocks {
        BlockFormat::Bc1 if bc => Some(wgpu::TextureFormat::Bc1RgbaUnorm),
        BlockFormat::Bc3 if bc => Some(wgpu::TextureFormat::Bc3RgbaUnorm),
        BlockFormat::Bc5 if bc => Some(wgpu::TextureFormat::Bc5RgUnorm),
        BlockFormat::Bc7 if bc => Some(wgpu::TextureFormat::Bc7RgbaUnorm),
        BlockFormat::Astc4x4 if features.contains(wgpu::Features::TEXTURE_COMPRESSION_ASTC) => {
            Some(wgpu::TextureFormat::Astc { block: wgpu::AstcBlock::B4x4, channel: wgpu::AstcChannel::Unorm })
        }
        _ => None,
    }
}

/// Uploads `tex`, generating mipmaps when it has a single level, after
/// shrinking it to fit the device and `max_size`.  `srgb_mips` filters
/// generated mips in linear light, for colour data.  Textures kept in GPU
/// blocks upload as they are when the device samples their format, and
/// are decoded on the CPU first when it doesn't.
pub(crate) fn upload_texture(
    gpu: &crate::Gpu,
    mipgen: &mut MipGen,
    tex: &Texture,
    surfaces: Surfaces,
    srgb_mips: bool,
    max_size: Option<u32>,
) -> Result<GpuTexture, String> {
    if slerp_scene::textures::is_compressed(tex) {
        if surfaces == Surfaces::First
            && let Some(Pixels::Blocks(blocks, _)) = tex.levels.first().and_then(|l| l.surfaces.first())
            && let Some(format) = block_format(gpu, *blocks)
            && let Some(t) = upload_blocks(gpu, tex, format, max_size, 0)
        {
            return Ok(t);
        }
        let texels = slerp_scene::textures::decompress(tex);
        return upload_texture(gpu, mipgen, &texels, surfaces, srgb_mips, max_size);
    }
    let device = &gpu.device;
    let limits = device.limits();
    let max_dim = limits.max_texture_dimension_2d.min(max_size.unwrap_or(u32::MAX)).max(1);
    let hdr = tex.is_hdr();

    // 1. Collect usable levels: consistent sizes, complete data.
    let w0 = tex.levels.first().map(|l| l.width).unwrap_or(0);
    let h0 = tex.levels.first().map(|l| l.height).unwrap_or(0);
    if w0 == 0 || h0 == 0 {
        return Err("empty texture".into());
    }
    let slices = if tex.kind == TextureKind::D3 { tex.levels[0].depth.max(1) } else { 1 };
    let layer_count = match (surfaces, tex.kind) {
        (Surfaces::First, _) => 1,
        (Surfaces::All, TextureKind::D3) => slices,
        (Surfaces::All, _) => (tex.layers.max(1) * tex.faces.max(1)).min(tex.levels[0].surfaces.len() as u32),
    }
    .clamp(1, limits.max_texture_array_layers);

    // Pick the first level that fits the device; shrink on the CPU if none.
    let mut first = 0;
    while first + 1 < tex.levels.len() && (tex.levels[first].width > max_dim || tex.levels[first].height > max_dim) {
        first += 1;
    }
    let mut levels: Vec<(u32, u32, Vec<Pixels>)> = Vec::new();
    for (i, level) in tex.levels.iter().enumerate().skip(first) {
        let (w, h) = (level.width, level.height);
        if let Some((pw, ph, _)) = levels.last()
            && (w != (pw / 2).max(1) || h != (ph / 2).max(1))
        {
            break;
        }
        if i > first && w == 0 {
            break;
        }
        let mut layers = Vec::new();
        for layer in 0..layer_count {
            let Some(p) = surface(tex, i, layer, surfaces) else { break };
            layers.push(p);
        }
        if layers.len() as u32 != layer_count {
            break;
        }
        levels.push((w, h, layers));
    }
    if levels.is_empty() {
        return Err("texture data doesn't match its size".into());
    }
    while levels[0].0 > max_dim || levels[0].1 > max_dim {
        let (w, h, layers) = levels.remove(0);
        let layers = layers.iter().map(|p| halve(p, w, h)).collect();
        levels = vec![((w / 2).max(1), (h / 2).max(1), layers)];
    }
    let (width, height) = (levels[0].0, levels[0].1);
    let file_mips = levels.len() as u32;
    let full_chain = 32 - width.max(height).leading_zeros();
    let mips = if file_mips == 1 { full_chain } else { file_mips };

    // 2. Create and fill.
    let format = if hdr { wgpu::TextureFormat::Rgba16Float } else { wgpu::TextureFormat::Rgba8Unorm };
    let view_formats: &[wgpu::TextureFormat] = if hdr { &[] } else { &[wgpu::TextureFormat::Rgba8UnormSrgb] };
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("texture"),
        size: wgpu::Extent3d { width, height, depth_or_array_layers: layer_count },
        mip_level_count: mips,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::TEXTURE_BINDING
            | wgpu::TextureUsages::COPY_DST
            | wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats,
    });
    let mut bytes = 0u64;
    for (level, (w, h, layers)) in levels.iter().enumerate() {
        for (layer, p) in layers.iter().enumerate() {
            let data: std::borrow::Cow<[u8]> = match p {
                Pixels::Rgba8(v) => v.as_slice().into(),
                Pixels::Rgba32F(v) => to_f16_bytes(v).into(),
                // Decoded above.
                Pixels::Blocks(..) => continue,
            };
            let bpp = if hdr { 8 } else { 4 };
            let wanted = (*w as usize) * (*h as usize) * bpp;
            if data.len() < wanted {
                continue;
            }
            gpu.queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &texture,
                    mip_level: level as u32,
                    origin: wgpu::Origin3d { x: 0, y: 0, z: layer as u32 },
                    aspect: wgpu::TextureAspect::All,
                },
                &data[..wanted],
                wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(w * bpp as u32), rows_per_image: None },
                wgpu::Extent3d { width: *w, height: *h, depth_or_array_layers: 1 },
            );
            bytes += wanted as u64;
        }
    }
    if file_mips == 1 && mips > 1 {
        let gen_format = if hdr {
            wgpu::TextureFormat::Rgba16Float
        } else if srgb_mips {
            wgpu::TextureFormat::Rgba8UnormSrgb
        } else {
            wgpu::TextureFormat::Rgba8Unorm
        };
        mipgen.generate(gpu, &texture, gen_format, layer_count, mips);
        bytes += bytes / 3;
    }
    let view_raw = texture.create_view(&wgpu::TextureViewDescriptor {
        dimension: Some(wgpu::TextureViewDimension::D2Array),
        ..Default::default()
    });
    let view_srgb = (!hdr).then(|| {
        texture.create_view(&wgpu::TextureViewDescriptor {
            format: Some(wgpu::TextureFormat::Rgba8UnormSrgb),
            dimension: Some(wgpu::TextureViewDimension::D2Array),
            ..Default::default()
        })
    });
    Ok(GpuTexture {
        texture,
        view_raw,
        view_srgb,
        width,
        height,
        layers: layer_count,
        mips,
        file_mips,
        hdr,
        bytes,
        faces: 1,
        color: srgb_mips,
    })
}

/// Uploads a texture kept in GPU blocks as `format`: from the first level
/// at or after `start` that fits the device and `max_size` in whole
/// blocks, down to the smallest.  Its mips are the file's, since blocks
/// can't be rendered into.  `None` when no level is usable, so the caller
/// decodes it instead.
fn upload_blocks(
    gpu: &crate::Gpu,
    tex: &Texture,
    format: wgpu::TextureFormat,
    max_size: Option<u32>,
    start: usize,
) -> Option<GpuTexture> {
    let device = &gpu.device;
    let max_dim = device.limits().max_texture_dimension_2d.min(max_size.unwrap_or(u32::MAX)).max(1);
    let block_bytes = format.block_copy_size(None).unwrap_or(16);
    // 1. The first level that fits, whose size (as the texture's largest)
    //    must be whole blocks, then every level after it that halves.
    let first = (start..tex.levels.len()).find(|&i| {
        let l = &tex.levels[i];
        l.width <= max_dim
            && l.height <= max_dim
            && l.width.is_multiple_of(4)
            && l.height.is_multiple_of(4)
            && l.width > 0
    })?;
    let mut levels: Vec<(u32, u32, &[u8])> = Vec::new();
    for l in &tex.levels[first..] {
        if let Some((pw, ph, _)) = levels.last()
            && (l.width != (pw / 2).max(1) || l.height != (ph / 2).max(1))
        {
            break;
        }
        let Some(Pixels::Blocks(_, data)) = l.surfaces.first() else { break };
        let size = l.width.div_ceil(4) as usize * l.height.div_ceil(4) as usize * block_bytes as usize;
        if data.len() < size {
            break;
        }
        levels.push((l.width, l.height, &data[..size]));
    }
    let (width, height, _) = *levels.first()?;
    let mips = levels.len() as u32;
    // 2. Create and fill, each level in whole blocks.
    let twin = srgb_twin(format);
    let view_formats: Vec<wgpu::TextureFormat> = twin.into_iter().collect();
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("texture (blocks)"),
        size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
        mip_level_count: mips,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &view_formats,
    });
    let mut bytes = 0u64;
    for (level, (w, h, data)) in levels.iter().enumerate() {
        let (bx, by) = (w.div_ceil(4), h.div_ceil(4));
        gpu.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: level as u32,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            data,
            wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(bx * block_bytes), rows_per_image: Some(by) },
            // Small levels are copied as the whole blocks that hold them.
            wgpu::Extent3d { width: bx * 4, height: by * 4, depth_or_array_layers: 1 },
        );
        bytes += data.len() as u64;
    }
    let view_raw = texture.create_view(&wgpu::TextureViewDescriptor {
        dimension: Some(wgpu::TextureViewDimension::D2Array),
        ..Default::default()
    });
    let view_srgb = twin.map(|f| {
        texture.create_view(&wgpu::TextureViewDescriptor {
            format: Some(f),
            dimension: Some(wgpu::TextureViewDimension::D2Array),
            ..Default::default()
        })
    });
    Some(GpuTexture {
        texture,
        view_raw,
        view_srgb,
        width,
        height,
        layers: 1,
        mips,
        file_mips: mips,
        hdr: false,
        bytes,
        faces: 1,
        color: twin.is_some(),
    })
}

/// A small stand-in for `tex` while the whole of it waits to be uploaded:
/// its stored mip levels no larger than `side`, which cost next to nothing
/// to upload.  `None` for textures without such levels (a PNG's single
/// level), which show their material's plain colour until then.
pub(crate) fn upload_preview(gpu: &crate::Gpu, tex: &Texture, side: u32) -> Option<GpuTexture> {
    let start = tex.levels.iter().position(|l| l.width.max(l.height) <= side).filter(|&i| i > 0)?;
    match tex.levels[start].surfaces.first()? {
        Pixels::Blocks(blocks, _) => upload_blocks(gpu, tex, block_format(gpu, *blocks)?, None, start),
        _ => {
            let mut small = tex.clone();
            small.levels.drain(..start);
            upload_texture(gpu, &mut MipGen::new(), &small, Surfaces::First, false, None).ok()
        }
    }
}

/// The pixels of one upload layer of level `level`, checked for size.
fn surface(tex: &Texture, level: usize, layer: u32, surfaces: Surfaces) -> Option<Pixels> {
    let l = tex.levels.get(level)?;
    let texels = (l.width as usize).checked_mul(l.height as usize)?;
    if tex.kind == TextureKind::D3 && surfaces == Surfaces::All {
        // One slice of the first volume.
        let p = l.surfaces.first()?;
        let start = texels.checked_mul(layer as usize)? * 4;
        let end = start + texels * 4;
        return match p {
            Pixels::Rgba8(v) => v.get(start..end).map(|s| Pixels::Rgba8(s.to_vec())),
            Pixels::Rgba32F(v) => v.get(start..end).map(|s| Pixels::Rgba32F(s.to_vec())),
            Pixels::Blocks(..) => None,
        };
    }
    let p = l.surfaces.get(layer as usize)?;
    if p.texel_count() < texels {
        return None;
    }
    match p {
        Pixels::Rgba8(v) => Some(Pixels::Rgba8(v[..texels * 4].to_vec())),
        Pixels::Rgba32F(v) => Some(Pixels::Rgba32F(v[..texels * 4].to_vec())),
        Pixels::Blocks(..) => None,
    }
}

/// A 2x2 box-filtered copy, for textures larger than the device allows.
fn halve(p: &Pixels, w: u32, h: u32) -> Pixels {
    let (nw, nh) = ((w / 2).max(1) as usize, (h / 2).max(1) as usize);
    let (w, h) = (w as usize, h as usize);
    let at = |x: usize, y: usize, c: usize| (y.min(h - 1) * w + x.min(w - 1)) * 4 + c;
    match p {
        Pixels::Rgba8(v) => {
            let mut out = vec![0u8; nw * nh * 4];
            for y in 0..nh {
                for x in 0..nw {
                    for c in 0..4 {
                        let s = v[at(2 * x, 2 * y, c)] as u32
                            + v[at(2 * x + 1, 2 * y, c)] as u32
                            + v[at(2 * x, 2 * y + 1, c)] as u32
                            + v[at(2 * x + 1, 2 * y + 1, c)] as u32;
                        out[(y * nw + x) * 4 + c] = ((s + 2) / 4) as u8;
                    }
                }
            }
            Pixels::Rgba8(out)
        }
        Pixels::Rgba32F(v) => {
            let mut out = vec![0f32; nw * nh * 4];
            for y in 0..nh {
                for x in 0..nw {
                    for c in 0..4 {
                        let s = v[at(2 * x, 2 * y, c)]
                            + v[at(2 * x + 1, 2 * y, c)]
                            + v[at(2 * x, 2 * y + 1, c)]
                            + v[at(2 * x + 1, 2 * y + 1, c)];
                        out[(y * nw + x) * 4 + c] = s * 0.25;
                    }
                }
            }
            Pixels::Rgba32F(out)
        }
        // Decoded before anything is halved.
        Pixels::Blocks(..) => Pixels::Rgba8(vec![0; nw * nh * 4]),
    }
}

pub(crate) fn to_f16_bytes(v: &[f32]) -> Vec<u8> {
    use half::slice::HalfFloatSliceExt;
    // Clamp to what f16 holds, so very bright suns don't turn infinite.
    let clamped: Vec<f32> = v.iter().map(|x| if x.is_finite() { x.clamp(-65000.0, 65000.0) } else { 0.0 }).collect();
    let mut out = vec![half::f16::ZERO; v.len()];
    out.as_mut_slice().convert_from_f32_slice(&clamped);
    bytemuck::cast_slice(&out).to_vec()
}

/// A 1x1 texture of one colour, standing in for absent material slots.
pub(crate) fn solid_texture(gpu: &crate::Gpu, rgba: [u8; 4]) -> wgpu::TextureView {
    let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("solid"),
        size: wgpu::Extent3d { width: 1, height: 1, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    gpu.queue.write_texture(
        texture.as_image_copy(),
        &rgba,
        wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(4), rows_per_image: None },
        wgpu::Extent3d { width: 1, height: 1, depth_or_array_layers: 1 },
    );
    texture.create_view(&Default::default())
}

//endregion

/*┌─────────────────╮
| Mip generation  |
└─────────────────╯*/
//region Mip generation

/// Renders each mip level from the one above with a bilinear tap, which
/// averages 2x2 texels.  Rendering (rather than a compute shader) works for
/// sRGB formats, so colour textures are filtered in linear light.
pub(crate) struct MipGen {
    module: Option<wgpu::ShaderModule>,
    layout: Option<(wgpu::BindGroupLayout, wgpu::PipelineLayout)>,
    pipelines: HashMap<wgpu::TextureFormat, wgpu::RenderPipeline>,
    sampler: Option<wgpu::Sampler>,
}

impl MipGen {
    pub fn new() -> MipGen {
        MipGen { module: None, layout: None, pipelines: HashMap::new(), sampler: None }
    }

    fn pipeline(&mut self, gpu: &crate::Gpu, format: wgpu::TextureFormat) -> wgpu::RenderPipeline {
        let device = &gpu.device;
        let module =
            self.module.get_or_insert_with(|| crate::shaders::module(device, "mipgen", crate::shaders::MIPGEN));
        let (_, pl) = self.layout.get_or_insert_with(|| {
            let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("mipgen"),
                entries: &[
                    texture(0, wgpu::ShaderStages::FRAGMENT, wgpu::TextureViewDimension::D2),
                    sampler(1, wgpu::ShaderStages::FRAGMENT, wgpu::SamplerBindingType::Filtering),
                ],
            });
            let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("mipgen"),
                bind_group_layouts: &[Some(&bgl)],
                immediate_size: 0,
            });
            (bgl, pl)
        });
        self.pipelines
            .entry(format)
            .or_insert_with(|| {
                device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                    label: Some("mipgen"),
                    layout: Some(pl),
                    vertex: wgpu::VertexState {
                        module,
                        entry_point: Some("vs_mip"),
                        compilation_options: Default::default(),
                        buffers: &[],
                    },
                    primitive: Default::default(),
                    depth_stencil: None,
                    multisample: Default::default(),
                    fragment: Some(wgpu::FragmentState {
                        module,
                        entry_point: Some("fs_mip"),
                        compilation_options: Default::default(),
                        targets: &[Some(format.into())],
                    }),
                    multiview_mask: None,
                    cache: gpu.pipeline_cache.as_ref(),
                })
            })
            .clone()
    }

    /// Fills mips 1.. of every layer of `texture`, viewing it as `format`.
    pub fn generate(
        &mut self,
        gpu: &crate::Gpu,
        texture: &wgpu::Texture,
        format: wgpu::TextureFormat,
        layers: u32,
        mips: u32,
    ) {
        let mut encoder = gpu.device.create_command_encoder(&Default::default());
        self.encode(gpu, &mut encoder, texture, format, layers, mips);
        gpu.queue.submit([encoder.finish()]);
    }

    /// Records what `generate` does into `encoder`, for textures drawn in
    /// the same frame.
    pub fn encode(
        &mut self,
        gpu: &crate::Gpu,
        encoder: &mut wgpu::CommandEncoder,
        texture: &wgpu::Texture,
        format: wgpu::TextureFormat,
        layers: u32,
        mips: u32,
    ) {
        let pipeline = self.pipeline(gpu, format);
        let device = &gpu.device;
        let sampler = self.sampler.get_or_insert_with(|| clamp_sampler(device, wgpu::FilterMode::Linear)).clone();
        let bgl = &self.layout.as_ref().unwrap().0;
        for layer in 0..layers {
            for mip in 1..mips {
                let view = |m: u32| {
                    texture.create_view(&wgpu::TextureViewDescriptor {
                        format: Some(format),
                        dimension: Some(wgpu::TextureViewDimension::D2),
                        base_mip_level: m,
                        mip_level_count: Some(1),
                        base_array_layer: layer,
                        array_layer_count: Some(1),
                        ..Default::default()
                    })
                };
                let src = view(mip - 1);
                let dst = view(mip);
                let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("mipgen"),
                    layout: bgl,
                    entries: &[
                        wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&src) },
                        wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&sampler) },
                    ],
                });
                let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("mipgen"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &dst,
                        depth_slice: None,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    ..Default::default()
                });
                pass.set_pipeline(&pipeline);
                pass.set_bind_group(0, &bind, &[]);
                pass.draw(0..3, 0..1);
            }
        }
    }
}

//endregion
