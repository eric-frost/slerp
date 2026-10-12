//! GPU selection and the renderer shared by Slerp's thumbnails and its
//! viewer.  It draws the scene model of `slerp-scene` with wgpu on
//! Vulkan (Linux), Direct3D 12 (Windows) or Metal (macOS), and never
//! depends on a loader.
//!
//! # Starting a GPU
//!
//! [`open_gpu`] picks a GPU by [`GpuPolicy`] (`SLERP_GPU` overrides the
//! caller's choice): `Auto` is the GPU the desktop renders on (the Wayland
//! compositor's `zwp_linux_dmabuf_v1` main device, else the boot display),
//! `Integrated` and `Discrete` are preferences that fall back to any GPU,
//! `Software` is lavapipe, and `Named` matches part of an adapter's name.
//! Only the chosen GPU's Vulkan driver is loaded, by pinning
//! `VK_DRIVER_FILES` while the instance is created, which saves the cost of
//! starting every driver on hybrid machines, and Mesa's device-select layer
//! is switched off then too (`NODEVICE_SELECT`).  [`open_gpu_with`] lets a
//! program with threads in C libraries make those changes on a thread of
//! its choosing.  If that fails, all drivers load; if no GPU works,
//! lavapipe.  The [`select`] module holds the pure
//! logic (sysfs, driver manifests, plans) and its tests.  On Windows and
//! macOS the system's graphics API knows its GPUs, so the policy becomes
//! wgpu's power preference, and `Software` is Direct3D's WARP renderer
//! (Windows only).
//!
//! A window must render on the desktop's GPU: on a laptop whose compositor
//! runs on the NVIDIA card, a wgpu window on the AMD iGPU crashes because
//! KWin can't import its buffers.  So the viewer uses `Auto`; thumbnails may use
//! `Integrated`, which starts in about 40 ms against 250 ms or more.
//!
//! [`Gpu`] holds the instance, adapter, device and queue, so the viewer's
//! egui can share the device (egui-wgpu's existing-device setup).  It also
//! holds a wgpu pipeline cache (Vulkan only) kept in
//! `$XDG_CACHE_HOME/slerp/` (`%LOCALAPPDATA%\Slerp\cache` on
//! Windows, `~/Library/Caches/Slerp` on macOS).  Mesa
//! drivers keep their own shader cache, which does most of the work on
//! RADV (a first frame of 125 ms with nothing cached, about 20 ms after);
//! on NVIDIA the pipeline cache saves another 20 to 40 ms per cold start.
//!
//! # Rendering
//!
//! ```no_run
//! # use slerp_render::*;
//! # use slerp_scene::{OrbitCamera, Scene};
//! # let scene = Scene::default();
//! let gpu = open_gpu(&GpuPolicy::Integrated).unwrap();
//! let mut renderer = Renderer::with_config(&gpu, wgpu::TextureFormat::Rgba8UnormSrgb, RendererConfig::thumbnail());
//! let gpu_scene = renderer.prepare(&gpu, &scene); // uploads, posed at rest
//! let view = OrbitCamera::framing(&gpu_scene.bounds, 1.0).view(1.0);
//! let image = renderer.render_image(&gpu, &gpu_scene, &view, &RenderSettings::thumbnail(), 256, 256);
//! ```
//!
//! - [`Renderer::prepare`] uploads geometry, materials and textures once.
//!   [`Renderer::update_pose`] uploads joint matrices, morph weights, node
//!   transforms and node visibility for a [`slerp_scene::Pose`] whose
//!   world matrices are current; call it when the pose or a visibility
//!   toggle changes.
//! - [`Renderer::render`] draws into any view of a texture in the
//!   renderer's target format, sRGB or not (non-sRGB targets get encoded
//!   in the shader).  [`Renderer::render_image`] renders offscreen and reads
//!   back an [`Image8`] with straight alpha.
//! - [`GpuScene::bounds`] bounds what is visible in the current pose and
//!   leaves out Quake sky surfaces, since maps can sit inside a sky box far
//!   larger than the play area; use it to frame cameras.  Sky surfaces face
//!   into the map like every Quake face, so back-face culling lets a view
//!   from outside see in, as in map editors.
//! - Thumbnails render in [`THUMBNAIL_FORMAT`]; create a thumbnail
//!   renderer with it so `render_image` shares its pipelines.
//!   [`Background::Color`] takes colours as displayed (sRGB, like hex
//!   colours).  [`RenderSettings::shadows`] is the key light's shadow on the
//!   scene and [`RenderSettings::ground_shadow`] the soft shadow on an
//!   invisible ground (never for levels).
//! - Nothing needs continuous frames: shadow maps are redrawn only when the
//!   pose, light or bounds change, so a viewer can redraw only on input.
//! - Empty scenes, and scenes with only a skeleton, render (as nothing, or
//!   as bones with the skeleton overlay).
//! - [`Renderer::set_environment`] lights the scene with an equirectangular
//!   panorama or a cubemap instead of the built-in studio.
//! - [`Renderer::upload_texture`] and [`Renderer::draw_texture`] are the
//!   viewer's texture mode: one mip, layer or face, a channel mask,
//!   exposure and tone mapping for HDR, pan and zoom with hard texel edges
//!   when magnified, and a checkerboard behind alpha.
//!
//! # Shading
//!
//! Metallic-roughness PBR ported from the Khronos glTF Sample Renderer
//! (Apache-2.0) to WGSL: every field of [`slerp_scene::Material`],
//! including texture transforms and UV sets, alpha mask and blend (blended
//! draws sorted back to front), double-sided and unlit materials,
//! KHR_materials_specular, _ior, _clearcoat, _transmission and _volume,
//! emissive, occlusion, and normal maps with the mesh's tangents or,
//! without them, tangents from screen-space derivatives.
//!
//! Transmissive surfaces (glass) see the opaque scene behind them, which a
//! pass of its own draws first into a texture with blurred mips: refracted
//! by their volume's thickness, blurred by roughness, absorbed by Beer's
//! law, as the Khronos renderer does it (`transmission.rs`).
//!
//! [`RenderSettings::ao`] adds screen-space ambient occlusion (GTAO, as
//! Intel's XeGTAO computes it, with a march along the mirror direction for
//! reflections), which darkens the environment's light in creases and
//! where things touch (`ao.rs`).  It costs 0.4 to 2 ms a thumbnail on an
//! integrated GPU and about half a millisecond a 1080p frame on a discrete
//! one.  Base colour and emissive textures read as
//! sRGB and the rest as linear, from one upload with two views.  Vertex
//! colours multiply the base colour.  Height maps displace material balls
//! along their normals in the vertex shader, so a new height scale or
//! texture transform needs only `Renderer::update_material`, a rewrite of
//! the material's uniform.
//!
//! Game extras: a lightmap (through its own UV set, normally 1) replaces
//! scene lighting; its values are treated as display-encoded light
//! multiplied by `lightmap_scale` before decoding, which reproduces how
//! Quake multiplies texture and light in display space, overbright
//! included.  `Effect::Warp` wobbles UVs like Quake's liquids (unlit), and
//! `Effect::Sky` scrolls the two halves of the texture as Quake's sky
//! layers, the left (front) half transparent where black.
//! `Effect::PanoramaSky` wraps the texture around the viewer as Doom's sky,
//! and `Effect::Billboard` turns sprites about +Y to face the camera.
//!
//! Points draw as round dots sized from their spacing, with soft rims
//! through the multisample coverage mask (`points.rs`);
//! [`RenderSettings::point_size`] and [`RenderSettings::point_world_size`]
//! change it.
//!
//! Lighting is image-based (a radiance cube prefiltered for roughness on
//! the GPU, nine spherical-harmonic irradiance coefficients and a split-sum
//! BRDF table) plus one shadow-casting key light, placed on the brightest
//! part of the environment.  The default environment is a procedural photo
//! studio generated on the GPU, so nothing large ships with the program.
//! Under objects (not levels), a soft contact shadow is made from a depth
//! map seen from below the ground.  Khronos PBR Neutral tone mapping by
//! default, ACES or none on request, and 4x multisampling.
//!
//! Vertex data is pulled from storage buffers rather than bound as vertex
//! buffers, and skinning reads joint matrices from a storage buffer, so
//! any joint count works and one pipeline serves every attribute layout,
//! which keeps cold starts short.  Morph targets live in the same buffers;
//! each draw lists only its active targets and weights, so meshes with
//! hundreds of targets (Quake vertex animation stored as targets) cost
//! what their weighted targets cost.  Skinned meshes ignore their node's
//! transform, as glTF says, and nodes whose world matrix mirrors (negative
//! determinant) draw with the opposite winding.
//!
//! Draws of the same primitive are instanced: every per-draw fact (node
//! transform, skin, active morph targets) lives in the draw's record, so
//! one call draws every visible copy, reading its records through a list
//! of draw indices.  A forest of 100,000 instanced trees is a handful of
//! calls per pass rather than 200,000.  Groups and copies the camera can't
//! see are culled by their boxes on the CPU, except where the vertex
//! shader moves vertices past them (skins, morphs, points, billboards,
//! displacement); blended and transmissive copies stay one call each, to
//! be sorted back to front.  See `instancing.rs`.
//!
//! # Gaussian splats
//!
//! `Scene::splats` draw as screen-space ellipses from each splat's
//! projected 3D covariance, with a Gaussian falloff, coloured for the
//! viewing direction by spherical harmonics up to degree 3.  A compute pass
//! projects them, they are sorted back to front, and they blend with
//! premultiplied alpha into a half-float layer in the photos' display
//! encoding (what 3D Gaussian splatting trains against), which the main pass
//! lays over the background, ground and grid.
//! [`RendererConfig::gpu_splat_sort`] picks a GPU radix sort whenever the
//! camera has moved (the viewer) or one CPU sort per frame (thumbnails),
//! and [`RenderSettings::splat_scale`] resizes them.  [`GpuScene::bounds`]
//! covers the dense part of a cloud, so stray splats far away don't shrink
//! the framing.  See `splats.rs`.
//!
//! # Tracing
//!
//! `SLERP_TRACE=1` prints how long uploads, lighting and each frame's
//! steps take, waiting for the GPU after each.

mod ao;
mod gpu;
mod ibl;
mod instancing;
mod overlays;
mod points;
mod renderer;
mod resources;
mod scene_gpu;
pub mod select;
mod shaders;
mod splats;
mod texview;
mod trace;
mod transmission;
#[cfg(target_os = "linux")]
mod wayland;

pub use gpu::{ChangeHere, EnvChanges, EnvKeeper, Gpu, GpuError, apply_env, backend_name, open_gpu, open_gpu_with};
pub use renderer::{Background, DebugChannel, Overlays, RenderSettings, Renderer, RendererConfig, Tonemap};
pub use resources::GpuTexture;
pub use scene_gpu::{DEFAULT_BINDING_BYTES, GpuScene, PackedGeometry, pack_geometry};
pub use select::GpuPolicy;
pub use slerp_scene::{Image8, View};
pub use texview::TextureDrawParams;
pub use wgpu;

/// The format `render_image` renders in before reading back, and the one to
/// create a thumbnail renderer with so its pipelines are shared.
pub const THUMBNAIL_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;

#[cfg(test)]
mod instancing_tests;
#[cfg(test)]
mod shadow_tests;

/// Opens a GPU for a unit test.  Tests run on several threads, and opening
/// a GPU sets `VK_DRIVER_FILES` for a moment, so they take turns.
#[cfg(test)]
pub(crate) fn test_gpu(policy: GpuPolicy) -> Option<Gpu> {
    static OPENING: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _turn = OPENING.lock().unwrap_or_else(|e| e.into_inner());
    open_gpu(&policy).ok()
}

#[cfg(test)]
mod send_tests {
    fn send<T: Send>() {}

    /// The helper renders on one thread while others decode, so the
    /// renderer and scenes must move between threads.
    #[test]
    fn renderer_is_send() {
        send::<crate::Renderer>();
        send::<crate::GpuScene>();
        send::<crate::Gpu>();
        send::<crate::GpuTexture>();
    }
}
