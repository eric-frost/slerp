//! The GPU side of the window: the renderer shared with thumbnails, the
//! offscreen picture the 3D view and texture mode draw into (shown by egui
//! as an image), and the bookkeeping that redraws it only when something
//! it shows has changed.

use slerp_render::wgpu;
use slerp_render::{Gpu, GpuScene, GpuTexture, RenderSettings, Renderer, TextureDrawParams};
use slerp_scene::{Texture, View};
use std::cell::{OnceCell, RefCell};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender};

/// The renderer draws in sRGB so blending happens in linear light; egui
/// samples its textures as already-encoded bytes, so it is given a plain
/// `Rgba8Unorm` view of the same texture.
pub const TARGET_FORMAT: wgpu::TextureFormat = slerp_render::THUMBNAIL_FORMAT;
const EGUI_VIEW_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

/// Everything a frame of the view depends on.  When it equals what the
/// target already shows, nothing is drawn, so an idle window does no GPU
/// work at all.  `textures` counts textures that arrived after the scene's
/// first frame.
#[derive(Clone, Debug, PartialEq)]
pub enum FrameKey {
    Scene {
        scene: u64,
        pose: u64,
        textures: u64,
        view: [glam::Mat4; 2],
        settings: RenderSettings,
        size: (u32, u32),
        env: u64,
    },
    Texture {
        texture: u64,
        params: TextureDrawParams,
        size: (u32, u32),
    },
}

/// The renderer, which a thread of its own may still be making (compiling
/// pipelines and building the studio lighting) while the window opens.
/// The first use waits for it; the window's first frames, which only lay
/// out the panels, don't use it.  If that thread fails, the renderer is
/// made on the spot.
pub struct Waiting {
    value: OnceCell<Renderer>,
    from: RefCell<Option<(Receiver<Renderer>, Arc<Gpu>)>>,
}

impl Waiting {
    pub fn ready(renderer: Renderer) -> Waiting {
        Waiting { value: OnceCell::from(renderer), from: RefCell::new(None) }
    }

    pub fn later(rx: Receiver<Renderer>, gpu: Arc<Gpu>) -> Waiting {
        Waiting { value: OnceCell::new(), from: RefCell::new(Some((rx, gpu))) }
    }

    /// Whether it has arrived, without waiting.
    pub fn arrived(&self) -> bool {
        if self.value.get().is_some() {
            return true;
        }
        let from = self.from.borrow();
        let Some((rx, _)) = from.as_ref() else { return false };
        match rx.try_recv() {
            Ok(r) => {
                drop(from);
                let _ = self.value.set(r);
                *self.from.borrow_mut() = None;
                true
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => false,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => true,
        }
    }

    fn get(&self) -> &Renderer {
        self.value.get_or_init(|| {
            let (rx, gpu) = self.from.borrow_mut().take().expect("a renderer or its source");
            rx.recv().unwrap_or_else(|_| Renderer::new(&gpu, TARGET_FORMAT))
        })
    }

    /// The renderer, if it ever arrived, for keeping after the window.
    fn take(&mut self) -> Option<Renderer> {
        if !self.arrived() {
            return None;
        }
        self.value.take()
    }
}

impl std::ops::Deref for Waiting {
    type Target = Renderer;
    fn deref(&self) -> &Renderer {
        self.get()
    }
}

impl std::ops::DerefMut for Waiting {
    fn deref_mut(&mut self) -> &mut Renderer {
        self.get();
        self.value.get_mut().expect("just made")
    }
}

/// What outlives a window in a warm viewer: the GPU and the renderer with
/// its pipelines, and the lighting it was left with.
pub struct Kept {
    pub gpu: Arc<Gpu>,
    pub renderer: Renderer,
    pub environment: Environment,
}

/// What lights the scene.
#[derive(Clone)]
pub enum Environment {
    Studio,
    Picture(Arc<Texture>),
}

impl PartialEq for Environment {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Environment::Studio, Environment::Studio) => true,
            (Environment::Picture(a), Environment::Picture(b)) => Arc::ptr_eq(a, b),
            _ => false,
        }
    }
}

struct Target {
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    id: egui::TextureId,
    size: (u32, u32),
}

pub struct Gfx {
    pub gpu: Arc<Gpu>,
    pub renderer: Waiting,
    egui: Arc<egui::mutex::RwLock<egui_wgpu::Renderer>>,
    target: Option<Target>,
    shown: Option<FrameKey>,
    environment: Environment,
    /// Changes whenever the lighting environment is replaced.
    pub env_version: u64,
    /// Frames drawn into the target, for the idle check.
    pub draws: u64,
    /// When set, each scene frame waits for the GPU and records how long
    /// drawing took, in milliseconds.  Only the benchmark turns it on.
    pub gpu_times: Option<Vec<f32>>,
    /// Where the renderer goes when the window closes, for the next window
    /// of a warm viewer.
    home: Option<Sender<Kept>>,
}

impl Gfx {
    /// `renderer` is made elsewhere (see `Waiting`), warmed up: its
    /// pipelines compiled and the studio lighting built.
    /// `environment` is the lighting `renderer` holds.
    pub fn new(
        gpu: Arc<Gpu>,
        renderer: Waiting,
        environment: Environment,
        egui: Arc<egui::mutex::RwLock<egui_wgpu::Renderer>>,
        home: Option<Sender<Kept>>,
    ) -> Gfx {
        Gfx {
            gpu,
            renderer,
            egui,
            target: None,
            shown: None,
            environment,
            env_version: 0,
            draws: 0,
            gpu_times: None,
            home,
        }
    }

    /// Makes the lighting `env`, if it isn't already.
    pub fn set_environment(&mut self, env: &Environment) {
        if &self.environment == env {
            return;
        }
        match env {
            Environment::Studio => self.renderer.set_studio_environment(&self.gpu),
            Environment::Picture(t) => self.renderer.set_environment(&self.gpu, t),
        }
        self.environment = env.clone();
        self.env_version += 1;
    }

    /// The egui texture showing the view, `size` physical pixels large,
    /// made or resized as needed.
    pub fn target(&mut self, size: (u32, u32)) -> egui::TextureId {
        let max = self.gpu.device.limits().max_texture_dimension_2d;
        let size = (size.0.clamp(1, max), size.1.clamp(1, max));
        if let Some(t) = &self.target
            && t.size == size
        {
            return t.id;
        }
        let texture = self.gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("viewer target"),
            size: wgpu::Extent3d { width: size.0, height: size.1, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: TARGET_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[EGUI_VIEW_FORMAT],
        });
        let view = texture.create_view(&Default::default());
        let egui_view =
            texture.create_view(&wgpu::TextureViewDescriptor { format: Some(EGUI_VIEW_FORMAT), ..Default::default() });
        let mut egui = self.egui.write();
        let id = match &self.target {
            Some(old) => {
                egui.update_egui_texture_from_wgpu_texture(
                    &self.gpu.device,
                    &egui_view,
                    wgpu::FilterMode::Linear,
                    old.id,
                );
                old.id
            }
            None => egui.register_native_texture(&self.gpu.device, &egui_view, wgpu::FilterMode::Linear),
        };
        drop(egui);
        if let Some(old) = self.target.take() {
            old.texture.destroy();
        }
        self.target = Some(Target { texture, view, id, size });
        self.shown = None;
        id
    }

    /// Forgets what the target shows, so the next frame draws.
    pub fn invalidate(&mut self) {
        self.shown = None;
    }

    /// Whether drawing `key` would change the picture.
    pub fn needs(&self, key: &FrameKey) -> bool {
        self.shown.as_ref() != Some(key)
    }

    /// Draws a scene into the target unless it already shows `key`.
    /// Returns whether it drew.
    pub fn draw_scene(&mut self, key: FrameKey, scene: &GpuScene, view: &View, settings: &RenderSettings) -> bool {
        let Some(t) = &self.target else { return false };
        if !self.needs(&key) {
            return false;
        }
        let start = std::time::Instant::now();
        self.renderer.render(&self.gpu, scene, view, settings, &t.view, t.size);
        if let Some(times) = &mut self.gpu_times {
            let _ = self.gpu.device.poll(wgpu::PollType::wait_indefinitely());
            times.push(start.elapsed().as_secs_f32() * 1000.0);
        }
        self.shown = Some(key);
        self.draws += 1;
        true
    }

    /// Draws a texture into the target unless it already shows `key`.
    pub fn draw_texture(&mut self, key: FrameKey, texture: &GpuTexture, params: &TextureDrawParams) -> bool {
        let Some(t) = &self.target else { return false };
        if !self.needs(&key) {
            return false;
        }
        self.renderer.draw_texture(&self.gpu, texture, params, &t.view, t.size);
        self.shown = Some(key);
        self.draws += 1;
        true
    }
}

impl Drop for Gfx {
    /// Hands the renderer back for the next window, without the textures
    /// sized for this one, and with the lighting the window had (the next
    /// one starts from the same saved lighting).
    fn drop(&mut self) {
        let Some(home) = self.home.take() else { return };
        if let Some(t) = self.target.take() {
            t.texture.destroy();
        }
        if let Some(mut renderer) = self.renderer.take() {
            renderer.trim();
            let _ = home.send(Kept { gpu: self.gpu.clone(), renderer, environment: self.environment.clone() });
        }
    }
}

/// Unique numbers for scenes and textures shown, so frame keys from two
/// files never compare equal.
pub fn next_id() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}
