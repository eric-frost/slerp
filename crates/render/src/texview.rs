//! The viewer's texture mode: any texture shown flat, one mip, layer or
//! face at a time.

use crate::renderer::{Renderer, Tonemap};
use crate::resources::{GpuTexture, Surfaces, TexViewUniform, upload_texture};
use crate::{Gpu, Image8};
use slerp_scene::Texture;
use slerp_scene::textures::TextureKind;

/// How to show a texture.  Positions are in texels of the largest level.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TextureDrawParams {
    /// The mip level to show; `None` picks one from the zoom.
    pub mip: Option<u32>,
    /// Array layer, or slice of a 3D texture.
    pub layer: u32,
    /// Cube face, in the order +X, -X, +Y, -Y, +Z, -Z.
    pub face: u32,
    /// Which of R, G, B, A to show.  A single colour channel shows as grey;
    /// with alpha off, the texture is opaque.
    pub channels: [bool; 4],
    /// Exposure in stops, for HDR textures.
    pub exposure: f32,
    /// Tone mapping for HDR textures.
    pub tonemap: Tonemap,
    /// The texel shown at the centre of the target.
    pub center: [f32; 2],
    /// Screen pixels per texel.  Above 1, texels show with hard edges.
    pub zoom: f32,
    /// A checkerboard behind transparent texels.
    pub checkerboard: bool,
    /// Colour around the texture, as displayed (sRGB-encoded).
    pub background: [f32; 3],
}

impl TextureDrawParams {
    /// The whole texture, centred and fitted into a `size` target.
    pub fn fit(tex: &GpuTexture, size: (u32, u32)) -> TextureDrawParams {
        let zoom = (size.0 as f32 / tex.width as f32).min(size.1 as f32 / tex.height as f32);
        TextureDrawParams {
            mip: None,
            layer: 0,
            face: 0,
            channels: [true; 4],
            exposure: 0.0,
            tonemap: Tonemap::PbrNeutral,
            center: [tex.width as f32 * 0.5, tex.height as f32 * 0.5],
            zoom: if zoom.is_finite() && zoom > 0.0 { zoom } else { 1.0 },
            checkerboard: true,
            background: [0.46, 0.46, 0.47],
        }
    }
}

pub(crate) struct TexViewState {
    uniform: wgpu::Buffer,
}

impl GpuTexture {
    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    /// Array layers (or 3D slices) times faces.
    pub fn layers(&self) -> u32 {
        self.layers
    }

    /// Mip levels on the GPU, including generated ones.
    pub fn mips(&self) -> u32 {
        self.mips
    }

    /// Mip levels the file stored.
    pub fn file_mips(&self) -> u32 {
        self.file_mips
    }

    pub fn is_hdr(&self) -> bool {
        self.hdr
    }

    /// Bytes of GPU memory, mips included.
    pub fn gpu_bytes(&self) -> u64 {
        self.bytes
    }
}

impl Renderer {
    /// Uploads every layer, face and slice of `tex` for `draw_texture`.
    /// Textures with one level get generated mips for zooming out.
    pub fn upload_texture(&mut self, gpu: &Gpu, tex: &Texture) -> Result<GpuTexture, String> {
        let srgb = tex.color_space != slerp_scene::ColorSpace::Linear;
        let mut t = upload_texture(gpu, &mut self.mipgen, tex, Surfaces::All, srgb, None)?;
        t.faces = if tex.kind == TextureKind::Cube { 6 } else { 1 };
        t.color = srgb;
        Ok(t)
    }

    /// Draws `tex` into `target` (a view of a texture in the renderer's
    /// target format) as `params` say.
    pub fn draw_texture(
        &mut self,
        gpu: &Gpu,
        tex: &GpuTexture,
        params: &TextureDrawParams,
        target: &wgpu::TextureView,
        size: (u32, u32),
    ) {
        let mut encoder = gpu.device.create_command_encoder(&Default::default());
        self.encode_texture(gpu, &mut encoder, tex, params, target, size, self.format);
        gpu.queue.submit([encoder.finish()]);
        self.finish_frame(gpu);
    }

    /// `draw_texture` offscreen, read back with straight alpha.
    pub fn draw_texture_image(
        &mut self,
        gpu: &Gpu,
        tex: &GpuTexture,
        params: &TextureDrawParams,
        width: u32,
        height: u32,
    ) -> Image8 {
        self.offscreen(gpu, width, height, |r, encoder, view, size, format| {
            r.encode_texture(gpu, encoder, tex, params, view, size, format)
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn encode_texture(
        &mut self,
        gpu: &Gpu,
        encoder: &mut wgpu::CommandEncoder,
        tex: &GpuTexture,
        params: &TextureDrawParams,
        target: &wgpu::TextureView,
        size: (u32, u32),
        format: wgpu::TextureFormat,
    ) {
        let pipeline = self.texview_pipeline(gpu, format);
        let state = self.texview_state.get_or_insert_with(|| TexViewState {
            uniform: gpu.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("texview"),
                size: std::mem::size_of::<TexViewUniform>() as u64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }),
        });
        let zoom = if params.zoom.is_finite() && params.zoom > 0.0 { params.zoom } else { 1.0 };
        let max_lod = (tex.mips - 1) as f32;
        let lod = match params.mip {
            Some(m) => (m as f32).min(max_lod),
            None => (1.0 / zoom).log2().clamp(0.0, max_lod),
        };
        let layer = (params.layer * tex.faces + params.face.min(tex.faces - 1)).min(tex.layers - 1);
        let shown: Vec<usize> = (0..3).filter(|&c| params.channels[c]).collect();
        let single = match (shown.as_slice(), params.channels[3]) {
            ([c], false) => *c as u32 + 1,
            ([], true) => 4,
            _ => 0,
        };
        let tonemap = match params.tonemap {
            Tonemap::None => 0.0,
            Tonemap::PbrNeutral => 1.0,
            Tonemap::Aces => 2.0,
        };
        let mask = params.channels.map(|c| if c { 1.0 } else { 0.0 });
        let u = TexViewUniform {
            xform: [
                size.0 as f32 * 0.5 - params.center[0] * zoom,
                size.1 as f32 * 0.5 - params.center[1] * zoom,
                1.0 / zoom,
                0.0,
            ],
            tex: [tex.width as f32, tex.height as f32, lod, layer as f32],
            color: [
                2f32.powf(params.exposure),
                tonemap,
                if format.is_srgb() { 1.0 } else { 0.0 },
                if tex.hdr { 1.0 } else { 0.0 },
            ],
            mask,
            flags: [(zoom >= 1.0) as u32, (params.checkerboard && params.channels[3] && single == 0) as u32, single, 0],
            background: {
                let c = crate::renderer::srgb_to_linear(
                    glam::Vec3::from(params.background).clamp(glam::Vec3::ZERO, glam::Vec3::ONE),
                );
                [c.x, c.y, c.z, 1.0]
            },
        };
        gpu.queue.write_buffer(&state.uniform, 0, bytemuck::bytes_of(&u));
        let view = if tex.color { tex.view_srgb.as_ref().unwrap_or(&tex.view_raw) } else { &tex.view_raw };
        let group = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("texview"),
            layout: &self.layouts.texview,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: state.uniform.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(view) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::Sampler(&self.nearest_clamp) },
                wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::Sampler(&self.linear_clamp) },
            ],
        });
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("texview"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: target,
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
        pass.set_bind_group(0, &group, &[]);
        pass.draw(0..3, 0..1);
    }
}
