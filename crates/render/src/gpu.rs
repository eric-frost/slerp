//! Starting a GPU device per the policy, plus the on-disk pipeline cache.
//! On Linux only the one Vulkan driver needed is loaded, when it can tell
//! which; on Windows and macOS the system's graphics API (Direct3D 12,
//! Metal) already knows its GPUs, and the policy becomes wgpu's power
//! preference.

#[cfg(target_os = "linux")]
use crate::select::{self, Attempt, Machine};
use crate::select::{GpuClass, GpuPolicy, Want};
#[cfg(target_os = "linux")]
use std::path::Path;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

/// A started GPU: everything the renderer and the viewer's egui need to
/// share one device.
pub struct Gpu {
    pub instance: wgpu::Instance,
    pub adapter: wgpu::Adapter,
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    pub info: wgpu::AdapterInfo,
    /// How the GPU was chosen, such as "the integrated GPU (amdgpu)", for
    /// logs and the viewer's inspector.
    pub chosen_by: String,
    /// Time from `open_gpu` being called to the device being ready.
    pub startup: Duration,
    /// Compiled pipelines kept between runs, when the driver supports it.
    pub pipeline_cache: Option<wgpu::PipelineCache>,
    cache_path: Option<PathBuf>,
    cache_saved_len: AtomicUsize,
}

#[derive(Debug, thiserror::Error)]
pub enum GpuError {
    #[cfg_attr(target_os = "linux", error("no usable GPU or Vulkan driver: {0}"))]
    #[cfg_attr(not(target_os = "linux"), error("no usable GPU: {0}"))]
    NoAdapter(String),
}

/// The graphics API's name, for reasons and the viewer's inspector.
pub fn backend_name(backend: wgpu::Backend) -> &'static str {
    match backend {
        wgpu::Backend::Vulkan => "Vulkan",
        wgpu::Backend::Metal => "Metal",
        wgpu::Backend::Dx12 => "Direct3D 12",
        wgpu::Backend::Gl => "OpenGL",
        wgpu::Backend::BrowserWebGpu => "WebGPU",
        wgpu::Backend::Noop => "no backend",
    }
}

/// Picks and starts the GPU per `policy`, which `SLERP_GPU` overrides.
///
/// On Linux, Vulkan:
///
/// 1. The desktop's GPU (for `Auto`, and as the fallback for the others) is
///    the Wayland compositor's `zwp_linux_dmabuf_v1` main device, else the
///    boot display.
/// 2. Only that GPU's Vulkan driver is loaded, by setting `VK_DRIVER_FILES`
///    to its manifest before creating the instance; the variable is
///    restored straight after, so child processes don't inherit it.
/// 3. If that fails, every driver is loaded and the best adapter used; if
///    no GPU works, lavapipe.
///
/// When `VK_DRIVER_FILES` or `VK_ICD_FILENAMES` is already set, the caller
/// has chosen the driver and only the policy's preference among its
/// adapters applies.
///
/// Changing the environment is only sound while no other thread reads it,
/// so call this early, before starting threads that do, or use
/// [`open_gpu_with`].
///
/// On Windows (Direct3D 12, else Vulkan) and macOS (Metal), `Auto` is the
/// system's default adapter, `Integrated` and `Discrete` ask for the
/// low-power or high-performance one (falling back to any), `Software` is
/// Direct3D's WARP renderer on Windows and an error on macOS, which has
/// none, and `Named` matches part of an adapter's name, else `Auto`.
/// `WGPU_BACKEND` picks other backends.
pub fn open_gpu(policy: &GpuPolicy) -> Result<Gpu, GpuError> {
    open_gpu_with(policy, &mut ChangeHere)
}

/// [`open_gpu`], with the environment changed through `keeper` (see
/// [`EnvKeeper`]), so the calling thread can run while other threads call
/// libraries that read the environment.
pub fn open_gpu_with(policy: &GpuPolicy, keeper: &mut dyn EnvKeeper) -> Result<Gpu, GpuError> {
    let start = Instant::now();
    let policy = match std::env::var("SLERP_GPU") {
        Ok(s) if !s.trim().is_empty() => GpuPolicy::parse(&s),
        _ => policy.clone(),
    };
    #[cfg(target_os = "linux")]
    return open_vulkan(policy, start, keeper);
    #[cfg(not(target_os = "linux"))]
    {
        let _ = keeper;
        open_system(policy, start)
    }
}

/// Variables `open_gpu` sets for the moment a Vulkan instance is made, each
/// with its value, or `None` to remove it: the one driver to load
/// (`VK_DRIVER_FILES`) and Mesa's device-select layer switched off
/// (`NODEVICE_SELECT`).  Afterwards it puts back what was there.
pub type EnvChanges = Vec<(&'static str, Option<std::ffi::OsString>)>;

/// Makes [`open_gpu_with`]'s changes to the process environment.
///
/// Changing the environment is only sound while no other thread reads it.
/// Rust's own accessors take a lock, but libraries written in C read it
/// without one, so a program whose other threads may be inside such
/// libraries passes a keeper that makes the changes on the one thread that
/// calls them, between its calls, and returns once they are made.  The
/// viewer does that: its main thread runs the window system's libraries
/// while the GPU starts on a thread of its own.
pub trait EnvKeeper {
    fn change(&mut self, changes: EnvChanges);
}

/// Makes the changes on the calling thread, as [`open_gpu`] does.
pub struct ChangeHere;

impl EnvKeeper for ChangeHere {
    fn change(&mut self, changes: EnvChanges) {
        apply_env(changes);
    }
}

/// Makes `changes` to the environment on this thread.  Only sound while no
/// other thread reads the environment except through Rust's own accessors
/// (see [`EnvKeeper`]).
pub fn apply_env(changes: EnvChanges) {
    for (name, value) in changes {
        // SAFETY: the caller guarantees that no other thread reads the
        // environment meanwhile except through Rust's locked accessors.
        unsafe {
            match value {
                Some(v) => std::env::set_var(name, v),
                None => std::env::remove_var(name),
            }
        }
    }
}

/*┌─────────────────╮
| Linux           |
└─────────────────╯*/
//region Linux

/// [`open_gpu`] on Linux: the desktop's GPU found through sysfs and the
/// Wayland compositor, with only its Vulkan driver loaded.
#[cfg(target_os = "linux")]
fn open_vulkan(policy: GpuPolicy, start: Instant, keeper: &mut dyn EnvKeeper) -> Result<Gpu, GpuError> {
    let mut tried = Vec::new();

    // 1. The caller pinned a driver: respect it.
    if std::env::var_os("VK_DRIVER_FILES").is_some() || std::env::var_os("VK_ICD_FILENAMES").is_some() {
        let want = match &policy {
            GpuPolicy::Software => Want::Cpu,
            GpuPolicy::Named(n) => Want::Name(n.to_lowercase()),
            GpuPolicy::Integrated => Want::Kind(GpuClass::Integrated),
            GpuPolicy::Discrete => Want::Kind(GpuClass::Discrete),
            GpuPolicy::Auto => Want::Any,
        };
        for want in [want, Want::Any, Want::Cpu] {
            let a = Attempt { drivers: Vec::new(), want, reason: "the drivers in VK_DRIVER_FILES".into() };
            if let Some(gpu) = try_attempt(&a, start, &mut tried, keeper) {
                return Ok(gpu);
            }
        }
        return Err(GpuError::NoAdapter(tried.join("; ")));
    }

    // 2. Look at the machine.  The Wayland question costs a round trip, so
    //    it is only asked once an attempt needs the desktop's GPU.
    let env = |k: &str| std::env::var(k).ok();
    let mut machine = Machine {
        gpus: select::scan_drm(Path::new("/sys/class/drm")),
        icds: select::scan_icds(&select::icd_search_dirs(&env)),
        wayland_main: None,
    };
    let needs_desktop = |a: &Attempt| !matches!(a.want, Want::Kind(_) | Want::Cpu);
    let mut asked_wayland = false;
    let mut attempts = select::plan(&policy, &machine);
    let mut i = 0;
    while i < attempts.len() {
        if !asked_wayland && needs_desktop(&attempts[i]) && !machine.gpus.is_empty() {
            asked_wayland = true;
            machine.wayland_main = crate::wayland::main_device(Duration::from_millis(250));
            if machine.wayland_main.is_some() {
                // Re-plan; keep going from the first attempt that needs the
                // desktop GPU.
                attempts = select::plan(&policy, &machine);
                i = attempts.iter().position(needs_desktop).unwrap_or(attempts.len());
                continue;
            }
        }
        if let Some(gpu) = try_attempt(&attempts[i], start, &mut tried, keeper) {
            return Ok(gpu);
        }
        i += 1;
    }
    Err(GpuError::NoAdapter(if tried.is_empty() { "no Vulkan drivers installed".into() } else { tried.join("; ") }))
}

/// Creates an instance with `attempt`'s drivers and takes the adapter it
/// wants.  Failures are noted in `tried`.
#[cfg(target_os = "linux")]
fn try_attempt(attempt: &Attempt, start: Instant, tried: &mut Vec<String>, keeper: &mut dyn EnvKeeper) -> Option<Gpu> {
    // 1. Pin the drivers for the moment the instance is created, and
    //    switch off Mesa's device-select layer, which loads into every
    //    Vulkan program to put the default GPU first.  Here the GPU is
    //    chosen by its PCI id, so the layer only costs time (about 10 ms
    //    on a laptop with AMD and NVIDIA GPUs).  A variable the user set stays as it is.  The loader
    //    reads its environment in vkCreateInstance, which `Instance::new`
    //    calls, and not afterwards.
    let mut set: EnvChanges = Vec::new();
    if !attempt.drivers.is_empty() {
        set.push(("VK_DRIVER_FILES", Some(std::env::join_paths(&attempt.drivers).ok()?)));
    }
    if std::env::var_os("NODEVICE_SELECT").is_none() {
        set.push(("NODEVICE_SELECT", Some("1".into())));
    }
    let restore: EnvChanges = set.iter().map(|(name, _)| (*name, std::env::var_os(name))).collect();
    keeper.change(set);
    let instance = new_instance(wgpu::Backends::VULKAN);
    keeper.change(restore);

    crate::trace::note("Vulkan instance", start);
    // 2. Choose among its adapters.
    let adapters = block_on(instance.enumerate_adapters(wgpu::Backends::VULKAN));
    crate::trace::note("adapters", start);
    let Some(adapter) = choose(adapters, &attempt.want) else {
        tried.push(format!("{}: no matching adapter", attempt.reason));
        return None;
    };
    start_device(instance, adapter, attempt.reason.clone(), start, tried)
}

//endregion

/*┌─────────────────╮
| Windows, macOS  |
└─────────────────╯*/
//region Windows, macOS

/// The backends to try in order: Direct3D 12 and then Vulkan on Windows,
/// Metal on macOS, or whatever `WGPU_BACKEND` names.
#[cfg(not(target_os = "linux"))]
fn system_backends() -> Vec<wgpu::Backends> {
    if let Some(backends) = wgpu::Backends::from_env() {
        return vec![backends];
    }
    if cfg!(windows) { vec![wgpu::Backends::DX12, wgpu::Backends::VULKAN] } else { vec![wgpu::Backends::METAL] }
}

/// [`open_gpu`] on Windows and macOS, where the system's graphics API
/// already knows which GPU drives the display and which saves power, so
/// the policy maps onto wgpu's adapter request.  Later backends are only
/// started when earlier ones have no adapter.
#[cfg(not(target_os = "linux"))]
fn open_system(policy: GpuPolicy, start: Instant) -> Result<Gpu, GpuError> {
    if policy == GpuPolicy::Software && cfg!(target_os = "macos") {
        return Err(GpuError::NoAdapter("macOS has no software renderer; Metal needs a GPU".into()));
    }
    let mut tried = Vec::new();
    for backends in system_backends() {
        let instance = new_instance(backends);
        let request = |power_preference, force_fallback_adapter| {
            let options =
                wgpu::RequestAdapterOptions { power_preference, force_fallback_adapter, compatible_surface: None };
            block_on(instance.request_adapter(&options)).ok()
        };
        let (adapter, why) = match &policy {
            GpuPolicy::Auto => (request(wgpu::PowerPreference::None, false), "the system's default GPU"),
            GpuPolicy::Integrated => (request(wgpu::PowerPreference::LowPower, false), "the low-power GPU"),
            GpuPolicy::Discrete => (request(wgpu::PowerPreference::HighPerformance, false), "the high-performance GPU"),
            GpuPolicy::Software => (request(wgpu::PowerPreference::None, true), "the software renderer"),
            GpuPolicy::Named(name) => {
                let named = choose(block_on(instance.enumerate_adapters(backends)), &Want::Name(name.to_lowercase()));
                match named {
                    Some(adapter) => (Some(adapter), "the adapter named like that"),
                    None => (
                        request(wgpu::PowerPreference::None, false),
                        "no adapter named like that, so the system's default GPU",
                    ),
                }
            }
        };
        let Some(adapter) = adapter else {
            tried.push(format!("{backends:?}: no adapter for {policy:?}"));
            continue;
        };
        let reason = format!("{why} ({})", backend_name(adapter.get_info().backend));
        if let Some(gpu) = start_device(instance, adapter, reason, start, &mut tried) {
            return Ok(gpu);
        }
    }
    Err(GpuError::NoAdapter(tried.join("; ")))
}

//endregion

/// Starts the device on `adapter` with the limits large scenes need, and
/// loads the pipeline cache.  Failures are noted in `tried`.
fn start_device(
    instance: wgpu::Instance,
    adapter: wgpu::Adapter,
    reason: String,
    start: Instant,
    tried: &mut Vec<String>,
) -> Option<Gpu> {
    // 1. The device.
    let info = adapter.get_info();
    let al = adapter.limits();
    let limits = wgpu::Limits {
        max_buffer_size: al.max_buffer_size,
        max_storage_buffer_binding_size: al.max_storage_buffer_binding_size,
        max_texture_dimension_2d: al.max_texture_dimension_2d,
        max_texture_dimension_3d: al.max_texture_dimension_3d,
        max_texture_array_layers: al.max_texture_array_layers,
        ..wgpu::Limits::default().using_resolution(al.clone()).using_alignment(al.clone())
    };
    // Block-compressed textures upload as they are where the GPU samples
    // them (see `Gpu::gpu_blocks`).
    let wanted = wgpu::Features::PIPELINE_CACHE
        | wgpu::Features::TEXTURE_COMPRESSION_BC
        | wgpu::Features::TEXTURE_COMPRESSION_ASTC;
    let features = adapter.features() & wanted;
    // wgpu's allocator takes GPU memory in blocks and keeps the last block
    // of each memory type even when it is empty.  With wgpu's `Performance`
    // hint the blocks start at 128 MB (64 MB for memory the CPU writes), so
    // a few small textures and buffers kept between windows held 450 to
    // 520 MB in a warm viewer and in the resident helper.  `MemoryUsage`
    // starts at 8 MB (4 MB) and doubles up to 64 MB (32 MB) as more are
    // needed; textures bigger than a block get memory of their own, which
    // goes back to the driver when they do.
    let desc = wgpu::DeviceDescriptor {
        label: Some("slerp"),
        required_features: features,
        required_limits: limits,
        experimental_features: wgpu::ExperimentalFeatures::disabled(),
        memory_hints: wgpu::MemoryHints::MemoryUsage,
        trace: wgpu::Trace::Off,
    };
    let (device, queue) = match block_on(adapter.request_device(&desc)) {
        Ok(dq) => dq,
        Err(e) => {
            tried.push(format!("{reason} ({}): {e}", info.name));
            return None;
        }
    };
    crate::trace::note("device", start);

    // 2. The pipeline cache.
    let (pipeline_cache, cache_path, loaded) = if features.contains(wgpu::Features::PIPELINE_CACHE) {
        load_pipeline_cache(&device, &info)
    } else {
        (None, None, 0)
    };
    Some(Gpu {
        instance,
        adapter,
        device,
        queue,
        info,
        chosen_by: reason,
        startup: start.elapsed(),
        pipeline_cache,
        cache_path,
        cache_saved_len: AtomicUsize::new(loaded),
    })
}

fn new_instance(backends: wgpu::Backends) -> wgpu::Instance {
    wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends,
        // No validation layers unless WGPU_VALIDATION asks: they cost
        // startup time, and wgpu validates the API itself.
        flags: wgpu::InstanceFlags::empty().with_env(),
        // Direct3D's shader compiler can be chosen with WGPU_DX12_COMPILER.
        #[cfg(windows)]
        backend_options: wgpu::BackendOptions::from_env_or_default(),
        ..wgpu::InstanceDescriptor::new_without_display_handle()
    })
}

fn rank(t: wgpu::DeviceType) -> u8 {
    match t {
        wgpu::DeviceType::DiscreteGpu => 0,
        wgpu::DeviceType::IntegratedGpu => 1,
        wgpu::DeviceType::VirtualGpu => 2,
        wgpu::DeviceType::Other => 3,
        wgpu::DeviceType::Cpu => 4,
    }
}

fn choose(adapters: Vec<wgpu::Adapter>, want: &Want) -> Option<wgpu::Adapter> {
    let infos: Vec<wgpu::AdapterInfo> = adapters.iter().map(|a| a.get_info()).collect();
    let best_hardware = || {
        (0..adapters.len())
            .filter(|&i| infos[i].device_type != wgpu::DeviceType::Cpu)
            .min_by_key(|&i| rank(infos[i].device_type))
    };
    let index = match want {
        Want::Any => best_hardware(),
        Want::Pci { vendor, device } => (0..adapters.len())
            .find(|&i| infos[i].vendor == *vendor && infos[i].device == *device)
            .or_else(best_hardware),
        Want::Kind(class) => {
            let t = match class {
                GpuClass::Integrated => wgpu::DeviceType::IntegratedGpu,
                GpuClass::Discrete => wgpu::DeviceType::DiscreteGpu,
                GpuClass::Unknown => wgpu::DeviceType::Other,
            };
            (0..adapters.len()).find(|&i| infos[i].device_type == t)
        }
        Want::Name(n) => (0..adapters.len()).find(|&i| infos[i].name.to_lowercase().contains(n.as_str())),
        Want::Cpu => (0..adapters.len()).find(|&i| infos[i].device_type == wgpu::DeviceType::Cpu),
    }?;
    adapters.into_iter().nth(index)
}

/// Runs a future that completes without waiting, as wgpu's native futures
/// do, without an async runtime.
pub(crate) fn block_on<F: std::future::Future>(fut: F) -> F::Output {
    let mut fut = std::pin::pin!(fut);
    let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
    loop {
        if let std::task::Poll::Ready(v) = fut.as_mut().poll(&mut cx) {
            return v;
        }
        std::thread::yield_now();
    }
}

/*┌─────────────────╮
| Pipeline cache  |
└─────────────────╯*/
//region Pipeline cache

/// `$XDG_CACHE_HOME/slerp`, else `~/.cache/slerp`.
#[cfg(not(any(windows, target_os = "macos")))]
fn cache_dir() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").filter(|s| !s.is_empty()).map(|h| PathBuf::from(h).join(".cache")))?;
    Some(base.join("slerp"))
}

/// `%LOCALAPPDATA%\Slerp\cache`, which doesn't roam with the profile.
#[cfg(windows)]
fn cache_dir() -> Option<PathBuf> {
    Some(dirs::cache_dir()?.join("Slerp").join("cache"))
}

/// `~/Library/Caches/Slerp`.
#[cfg(target_os = "macos")]
fn cache_dir() -> Option<PathBuf> {
    Some(std::env::home_dir()?.join("Library/Caches/Slerp"))
}

fn load_pipeline_cache(
    device: &wgpu::Device,
    info: &wgpu::AdapterInfo,
) -> (Option<wgpu::PipelineCache>, Option<PathBuf>, usize) {
    let Some(key) = wgpu::util::pipeline_cache_key(info) else { return (None, None, 0) };
    // The key names vendor and device; the driver name keeps RADV's and
    // AMDVLK's caches (or lavapipe builds) apart.
    let driver: String = info.driver.chars().filter(|c| c.is_ascii_alphanumeric()).collect();
    let path = cache_dir().map(|d| d.join(format!("{key}_{driver}.bin")));
    let data = path.as_ref().and_then(|p| std::fs::read(p).ok());
    // SAFETY: the data was produced by `PipelineCache::get_data` for an
    // adapter with the same key (the file name), and `fallback` makes wgpu
    // start empty if it doesn't validate.
    let cache = unsafe {
        device.create_pipeline_cache(&wgpu::PipelineCacheDescriptor {
            label: Some("slerp pipelines"),
            data: data.as_deref(),
            fallback: true,
        })
    };
    let loaded = data.map_or(0, |d| d.len());
    (Some(cache), path, loaded)
}

impl Gpu {
    /// Writes the pipeline cache to disk if it has grown since it was loaded
    /// or last saved.  The renderer calls this after creating pipelines.
    pub fn save_pipeline_cache(&self) {
        let (Some(cache), Some(path)) = (&self.pipeline_cache, &self.cache_path) else { return };
        let Some(data) = cache.get_data() else { return };
        if data.len() == self.cache_saved_len.load(Ordering::Relaxed) {
            return;
        }
        self.cache_saved_len.store(data.len(), Ordering::Relaxed);
        // Write a temporary file and rename it, so a concurrent reader never
        // sees half a cache.  Two renderers on one GPU (the viewer's, and
        // the one drawing its Contents panel's row pictures) may save at
        // once from two threads, so each save gets a name of its own.
        static SAVES: AtomicUsize = AtomicUsize::new(0);
        let Some(dir) = path.parent() else { return };
        if std::fs::create_dir_all(dir).is_err() {
            return;
        }
        let n = SAVES.fetch_add(1, Ordering::Relaxed);
        let name = path.file_name().unwrap_or_default().to_string_lossy();
        let tmp = dir.join(format!(".{}.{n}.{name}", std::process::id()));
        if std::fs::write(&tmp, &data).is_ok() && std::fs::rename(&tmp, path).is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
    }

    /// Wraps a device someone else created, such as the viewer's window
    /// toolkit, so the renderer can share it.
    pub fn from_parts(
        instance: wgpu::Instance,
        adapter: wgpu::Adapter,
        device: wgpu::Device,
        queue: wgpu::Queue,
    ) -> Gpu {
        let info = adapter.get_info();
        Gpu {
            instance,
            adapter,
            device,
            queue,
            info,
            chosen_by: "the caller".into(),
            startup: Duration::ZERO,
            pipeline_cache: None,
            cache_path: None,
            cache_saved_len: AtomicUsize::new(0),
        }
    }

    /// Whether this is lavapipe or another CPU driver, where the renderer
    /// keeps image-based lighting smaller.
    pub fn is_cpu(&self) -> bool {
        self.info.device_type == wgpu::DeviceType::Cpu
    }

    /// The block-compressed formats to decode textures to for this GPU
    /// (`slerp_textures::decode_for_gpu`).  None on CPU drivers, which
    /// would decode the blocks again at every texture read.
    pub fn gpu_blocks(&self) -> slerp_scene::textures::GpuBlocks {
        let f = self.device.features();
        slerp_scene::textures::GpuBlocks {
            bc: !self.is_cpu() && f.contains(wgpu::Features::TEXTURE_COMPRESSION_BC),
            astc: !self.is_cpu() && f.contains(wgpu::Features::TEXTURE_COMPRESSION_ASTC),
        }
    }
}

//endregion
