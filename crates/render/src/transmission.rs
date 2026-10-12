//! What transmissive surfaces (glass, KHR_materials_transmission) see
//! through.
//!
//! Following the Khronos glTF Sample Renderer, a frame with visible
//! transmissive surfaces first draws everything opaque behind them (the
//! background, opaque surfaces and points, the ground's shadow and the
//! grid) into a texture of its own, in linear light without tone mapping
//! and without multisampling, then fills that texture's mips with
//! successively blurred copies.  The main pass draws transmissive surfaces
//! after the opaque ones; each samples the texture where the ray refracted
//! by its volume leaves it, at a mip that grows with its roughness, and
//! absorbs the light by Beer's law (`fs_main` in scene.wgsl).  Glass behind
//! glass shows only the opaque scene, as in the sample renderer.
//!
//! The texture keeps premultiplied alpha.  Where it is empty (a
//! transparent thumbnail background), glass shows the lighting
//! environment instead, much as the viewer's environment background
//! would: a picture's alpha can't tint what lies behind it, so letting
//! the file manager's background through would lose the glass's colour.

use crate::Gpu;
use crate::resources::{DEPTH_FORMAT, TRANSMISSION_FORMAT};

pub(crate) struct Transmission {
    pub size: (u32, u32),
    pub texture: wgpu::Texture,
    /// Every mip, for sampling.
    pub view: wgpu::TextureView,
    /// Mip 0, for drawing into.
    pub target: wgpu::TextureView,
    pub depth: wgpu::TextureView,
    pub mips: u32,
}

impl Transmission {
    pub fn new(gpu: &Gpu, size: (u32, u32)) -> Transmission {
        let device = &gpu.device;
        let mips = 32 - size.0.max(size.1).max(1).leading_zeros();
        let extent = wgpu::Extent3d { width: size.0, height: size.1, depth_or_array_layers: 1 };
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("transmission"),
            size: extent,
            mip_level_count: mips,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: TRANSMISSION_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let view = texture.create_view(&Default::default());
        let target = texture.create_view(&wgpu::TextureViewDescriptor {
            base_mip_level: 0,
            mip_level_count: Some(1),
            ..Default::default()
        });
        let depth = device
            .create_texture(&wgpu::TextureDescriptor {
                label: Some("transmission depth"),
                size: extent,
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: DEPTH_FORMAT,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                view_formats: &[],
            })
            .create_view(&Default::default());
        Transmission { size, texture, view, target, depth, mips }
    }
}
