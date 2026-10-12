//! Choosing which GPU and which Vulkan driver to start, from what the kernel
//! and the installed driver manifests say.  Everything here is pure file
//! reading, so tests run it against a fake `/sys/class/drm` and fake manifest
//! directories; `gpu.rs` turns the resulting plan into a device.
//!
//! The point of the exercise is startup time: the Vulkan loader normally
//! starts every installed driver, which costs about 250 ms on a machine with
//! two GPUs, while a thumbnailer only ever needs one.  Pinning the one driver
//! with `VK_DRIVER_FILES` skips the rest.

use std::fs;
use std::path::{Path, PathBuf};

/*┌─────────────────╮
| Kernel GPUs     |
└─────────────────╯*/
//region Kernel GPUs

/// One GPU as the kernel sees it: a device with a primary (`cardN`) node
/// and usually a render (`renderDN`) node.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DrmGpu {
    /// The device directory in sysfs, resolved, which identifies the GPU.
    pub device_path: PathBuf,
    /// Node names under `/sys/class/drm`, such as `card1` and `renderD128`.
    pub nodes: Vec<String>,
    /// The nodes' device numbers as (major, minor).
    pub dev_numbers: Vec<(u32, u32)>,
    /// The kernel driver's name, such as `amdgpu` or `nvidia`.
    pub driver: String,
    /// PCI vendor and device IDs, which Vulkan reports too; 0 when unknown.
    pub vendor: u32,
    pub device: u32,
    /// The firmware's boot display, which X11 desktops normally run on.
    pub boot_vga: bool,
}

impl DrmGpu {
    /// The PCI address (`0000:06:00.0`) when the GPU is a PCI device.
    pub fn pci_slot(&self) -> Option<&str> {
        let name = self.device_path.file_name()?.to_str()?;
        is_pci_address(name).then_some(name)
    }

    /// Guesses whether this is an integrated or a discrete GPU from what the
    /// kernel shows.  It is only a guess, which `gpu.rs` checks against the
    /// type Vulkan reports, so a wrong guess costs time, not correctness.
    pub fn class(&self) -> GpuClass {
        let parent = self.device_path.parent().and_then(|p| p.file_name()).and_then(|n| n.to_str()).unwrap_or("");
        let slot = self.pci_slot();
        let on_root_bus = slot.is_some_and(|s| s.get(5..7) == Some("00"));
        match self.driver.as_str() {
            "nvidia" | "nouveau" => GpuClass::Discrete,
            // Intel's integrated GPUs always sit at 00:02.0; Arc cards don't.
            "i915" | "xe" => {
                if slot.is_some_and(|s| s.ends_with(":00:02.0")) {
                    GpuClass::Integrated
                } else {
                    GpuClass::Discrete
                }
            }
            // AMD's APUs sit on the root bus (older ones) or behind the
            // internal bridge at 00:08.x (Raven Ridge onwards); add-in and
            // laptop cards sit behind a PCIe port.
            "amdgpu" | "radeon" => {
                if on_root_bus || parent.starts_with("0000:00:08.") {
                    GpuClass::Integrated
                } else {
                    GpuClass::Discrete
                }
            }
            "virtio_gpu" | "vmwgfx" | "qxl" | "bochs-drm" | "cirrus" | "simpledrm" | "vkms" => GpuClass::Unknown,
            // System-on-chip GPUs (msm, panfrost, asahi, v3d, ...) are never
            // PCI devices and are always part of the chip.
            _ if slot.is_none() && !self.driver.is_empty() => GpuClass::Integrated,
            _ => GpuClass::Unknown,
        }
    }

    /// True when `dev` (as the kernel encodes `dev_t`) is one of this GPU's
    /// nodes.
    pub fn has_dev(&self, dev: u64) -> bool {
        self.dev_numbers.contains(&split_dev(dev))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GpuClass {
    Integrated,
    Discrete,
    Unknown,
}

/// Splits a Linux `dev_t` into (major, minor), as glibc's `major()` and
/// `minor()` do.
pub fn split_dev(dev: u64) -> (u32, u32) {
    let major = ((dev >> 8) & 0xfff) | ((dev >> 32) & !0xfff);
    let minor = (dev & 0xff) | ((dev >> 12) & !0xff);
    (major as u32, minor as u32)
}

fn is_pci_address(name: &str) -> bool {
    // dddd:bb:dd.f
    let b = name.as_bytes();
    b.len() == 12 && b[4] == b':' && b[7] == b':' && b[10] == b'.'
}

/// Lists the GPUs under `class_drm` (normally `/sys/class/drm`), one entry
/// per device however many nodes it has.  Missing or unreadable files make
/// fields empty rather than failing: under GNOME's thumbnail sandbox there
/// is no `/sys` at all, and the result is simply empty.
pub fn scan_drm(class_drm: &Path) -> Vec<DrmGpu> {
    let Ok(dir) = fs::read_dir(class_drm) else { return Vec::new() };
    let mut names: Vec<String> =
        dir.flatten().filter_map(|e| e.file_name().into_string().ok()).filter(|n| is_node_name(n)).collect();
    names.sort_by_key(|n| (n.starts_with("render"), node_number(n)));
    let mut gpus: Vec<DrmGpu> = Vec::new();
    for name in names {
        let node = class_drm.join(&name);
        let Ok(device_path) = fs::canonicalize(node.join("device")) else { continue };
        let dev = fs::read_to_string(node.join("dev")).ok().and_then(|s| parse_dev(&s));
        let gpu = match gpus.iter_mut().find(|g| g.device_path == device_path) {
            Some(g) => g,
            None => {
                let device_dir = node.join("device");
                gpus.push(DrmGpu {
                    driver: fs::read_link(device_dir.join("driver"))
                        .ok()
                        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
                        .unwrap_or_default(),
                    vendor: read_hex(&device_dir.join("vendor")),
                    device: read_hex(&device_dir.join("device")),
                    boot_vga: fs::read_to_string(device_dir.join("boot_vga")).is_ok_and(|s| s.trim() == "1"),
                    device_path,
                    nodes: Vec::new(),
                    dev_numbers: Vec::new(),
                });
                gpus.last_mut().unwrap()
            }
        };
        gpu.nodes.push(name);
        gpu.dev_numbers.extend(dev);
    }
    gpus
}

/// `card0`, `renderD128`, but not connectors such as `card1-DP-1`.
fn is_node_name(n: &str) -> bool {
    let digits = n.strip_prefix("renderD").or_else(|| n.strip_prefix("card"));
    digits.is_some_and(|d| !d.is_empty() && d.bytes().all(|b| b.is_ascii_digit()))
}

fn node_number(n: &str) -> u32 {
    n.trim_start_matches(|c: char| !c.is_ascii_digit()).parse().unwrap_or(0)
}

fn parse_dev(s: &str) -> Option<(u32, u32)> {
    let (a, b) = s.trim().split_once(':')?;
    Some((a.parse().ok()?, b.parse().ok()?))
}

fn read_hex(path: &Path) -> u32 {
    fs::read_to_string(path)
        .ok()
        .and_then(|s| u32::from_str_radix(s.trim().trim_start_matches("0x"), 16).ok())
        .unwrap_or(0)
}

//endregion

/*┌─────────────────╮
| Driver manifests|
└─────────────────╯*/
//region Driver manifests

/// A Vulkan driver manifest (`*_icd.json`) and the library it names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IcdManifest {
    pub path: PathBuf,
    /// The manifest's `library_path`, as written.
    pub library: String,
}

impl IcdManifest {
    /// The library's file name without directories or the `.so...` suffix,
    /// such as `libvulkan_radeon` or `libGLX_nvidia`.
    pub fn library_stem(&self) -> &str {
        library_stem(&self.library)
    }
}

pub fn library_stem(library: &str) -> &str {
    let name = library.rsplit('/').next().unwrap_or(library);
    match name.find(".so") {
        Some(i) => &name[..i],
        None => name.strip_suffix(".dll").unwrap_or(name),
    }
}

/// The directories the Vulkan loader searches for driver manifests on
/// Linux, in its order, given an environment lookup.
pub fn icd_search_dirs(env: &dyn Fn(&str) -> Option<String>) -> Vec<PathBuf> {
    let home = env("HOME").filter(|h| !h.is_empty());
    let config_home =
        env("XDG_CONFIG_HOME").filter(|s| !s.is_empty()).or_else(|| home.as_ref().map(|h| format!("{h}/.config")));
    let data_home =
        env("XDG_DATA_HOME").filter(|s| !s.is_empty()).or_else(|| home.as_ref().map(|h| format!("{h}/.local/share")));
    let config_dirs = env("XDG_CONFIG_DIRS").filter(|s| !s.is_empty()).unwrap_or_else(|| "/etc/xdg".into());
    let data_dirs =
        env("XDG_DATA_DIRS").filter(|s| !s.is_empty()).unwrap_or_else(|| "/usr/local/share:/usr/share".into());
    let mut dirs = Vec::new();
    dirs.extend(config_home);
    dirs.extend(config_dirs.split(':').filter(|s| !s.is_empty()).map(String::from));
    dirs.push("/etc".into());
    dirs.extend(data_home);
    dirs.extend(data_dirs.split(':').filter(|s| !s.is_empty()).map(String::from));
    let mut out: Vec<PathBuf> = Vec::new();
    for d in dirs {
        let p = Path::new(&d).join("vulkan/icd.d");
        if !out.contains(&p) {
            out.push(p);
        }
    }
    out
}

/// Reads every manifest in `dirs`.  Unreadable or malformed files are
/// skipped.  When the same file name appears in several directories, the
/// first one wins, as in the loader.
pub fn scan_icds(dirs: &[PathBuf]) -> Vec<IcdManifest> {
    let mut out: Vec<IcdManifest> = Vec::new();
    for dir in dirs {
        let Ok(entries) = fs::read_dir(dir) else { continue };
        let mut paths: Vec<PathBuf> =
            entries.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e == "json")).collect();
        paths.sort();
        for path in paths {
            if out.iter().any(|m| m.path.file_name() == path.file_name()) {
                continue;
            }
            // Manifests are a few hundred bytes; refuse anything silly.
            let Ok(meta) = fs::metadata(&path) else { continue };
            if meta.len() > 64 * 1024 {
                continue;
            }
            let Ok(text) = fs::read_to_string(&path) else { continue };
            if let Some(library) = json_string_field(&text, "library_path") {
                out.push(IcdManifest { path, library });
            }
        }
    }
    out
}

/// The driver libraries that serve GPUs bound to a kernel driver.  Several
/// may apply (Intel's two Mesa drivers split the hardware generations
/// between them; AMD has RADV and AMDVLK); the loader skips those that find
/// no device.
pub fn driver_library_stems(kernel_driver: &str) -> &'static [&'static str] {
    match kernel_driver {
        "amdgpu" => &["libvulkan_radeon", "amdvlk64", "amdvlk"],
        "radeon" => &["libvulkan_radeon"],
        "i915" | "xe" => &["libvulkan_intel", "libvulkan_intel_hasvk"],
        "nvidia" => &["libGLX_nvidia", "libnvidia-vulkan-producer"],
        "nouveau" => &["libvulkan_nouveau"],
        "virtio_gpu" => &["libvulkan_virtio"],
        "msm" => &["libvulkan_freedreno"],
        "panfrost" | "panthor" => &["libvulkan_panfrost"],
        "asahi" => &["libvulkan_asahi"],
        "v3d" | "vc4" => &["libvulkan_broadcom"],
        "powervr" => &["libvulkan_powervr_mesa"],
        "lima" => &[],
        _ => &[],
    }
}

/// The lavapipe (CPU) driver's library.
pub const LAVAPIPE: &str = "libvulkan_lvp";

/// The manifests whose library serves `kernel_driver`.
pub fn manifests_for_driver(icds: &[IcdManifest], kernel_driver: &str) -> Vec<PathBuf> {
    let stems = driver_library_stems(kernel_driver);
    icds.iter().filter(|m| stems.contains(&m.library_stem())).map(|m| m.path.clone()).collect()
}

pub fn lavapipe_manifests(icds: &[IcdManifest]) -> Vec<PathBuf> {
    icds.iter().filter(|m| m.library_stem() == LAVAPIPE).map(|m| m.path.clone()).collect()
}

/// Finds `"key": "value"` in a small JSON document and returns the value
/// with escapes undone.  Driver manifests are flat and tiny, so this is all
/// the JSON reading they need.
fn json_string_field(text: &str, key: &str) -> Option<String> {
    let quoted = format!("\"{key}\"");
    let mut from = 0;
    while let Some(i) = text[from..].find(&quoted) {
        let after = from + i + quoted.len();
        let rest = text[after..].trim_start();
        if let Some(rest) = rest.strip_prefix(':') {
            let rest = rest.trim_start();
            let mut chars = rest.strip_prefix('"')?.chars();
            let mut value = String::new();
            loop {
                match chars.next()? {
                    '"' => return Some(value),
                    '\\' => match chars.next()? {
                        'n' => value.push('\n'),
                        't' => value.push('\t'),
                        'u' => {
                            let hex: String = chars.by_ref().take(4).collect();
                            value.push(u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32)?);
                        }
                        c => value.push(c),
                    },
                    c => value.push(c),
                }
            }
        }
        from = after;
    }
    None
}

//endregion

/*┌─────────────────╮
| Plans           |
└─────────────────╯*/
//region Plans

/// Which GPU to use.  Thumbnails and the viewer each have a setting; the
/// `SLERP_GPU` environment variable overrides both.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub enum GpuPolicy {
    /// The GPU the desktop renders on, which a window must use anyway and
    /// which is already awake.
    #[default]
    Auto,
    /// Prefer an integrated GPU, which starts faster and saves power; falls
    /// back to any GPU, because the setting syncs to machines without one.
    Integrated,
    /// Prefer a discrete GPU; falls back to any GPU.
    Discrete,
    /// lavapipe, Mesa's CPU Vulkan driver.
    Software,
    /// The first adapter whose name contains this text (case-insensitive);
    /// falls back to `Auto`.
    Named(String),
}

impl GpuPolicy {
    /// Reads a setting value: `auto`, `integrated`, `discrete`, `software`,
    /// or anything else as part of an adapter name.
    pub fn parse(s: &str) -> GpuPolicy {
        match s.trim().to_ascii_lowercase().as_str() {
            "" | "auto" | "desktop" => GpuPolicy::Auto,
            "integrated" | "igpu" => GpuPolicy::Integrated,
            "discrete" | "dgpu" => GpuPolicy::Discrete,
            "software" | "cpu" | "lavapipe" => GpuPolicy::Software,
            _ => GpuPolicy::Named(s.trim().to_string()),
        }
    }
}

/// What an attempt accepts from the adapters it finds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Want {
    /// Any hardware adapter.
    Any,
    /// The adapter with these PCI vendor and device IDs, else any.
    Pci { vendor: u32, device: u32 },
    /// Only an adapter of this kind.
    Kind(GpuClass),
    /// Only an adapter whose name contains this (lowercase) text.
    Name(String),
    /// Only a CPU adapter.
    Cpu,
}

/// One way of starting Vulkan, tried in order until one gives an adapter.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Attempt {
    /// Manifests to pin with `VK_DRIVER_FILES`; empty loads every driver.
    pub drivers: Vec<PathBuf>,
    pub want: Want,
    /// Why, for logs and the viewer's inspector.
    pub reason: String,
}

/// What the machine looks like, gathered once by `gpu.rs`.
pub struct Machine {
    pub gpus: Vec<DrmGpu>,
    pub icds: Vec<IcdManifest>,
    /// The device number of the Wayland compositor's main device, when the
    /// policy needed it and the compositor said.
    pub wayland_main: Option<u64>,
}

impl Machine {
    /// The GPU the desktop renders on: the Wayland compositor's main device,
    /// else the boot display, else the only GPU.
    pub fn desktop_gpu(&self) -> Option<(&DrmGpu, &'static str)> {
        if let Some(dev) = self.wayland_main
            && let Some(g) = self.gpus.iter().find(|g| g.has_dev(dev))
        {
            return Some((g, "the Wayland compositor's GPU"));
        }
        if let Some(g) = self.gpus.iter().find(|g| g.boot_vga) {
            return Some((g, "the boot display GPU"));
        }
        match self.gpus.as_slice() {
            [only] => Some((only, "the only GPU")),
            _ => None,
        }
    }
}

/// Orders the ways of starting Vulkan for `policy`.  Pinned attempts come
/// first because they are fast; loading every driver and then lavapipe are
/// the fallbacks.
pub fn plan(policy: &GpuPolicy, m: &Machine) -> Vec<Attempt> {
    let lavapipe = lavapipe_manifests(&m.icds);
    let software = || Attempt { drivers: lavapipe.clone(), want: Want::Cpu, reason: "lavapipe, the CPU driver".into() };
    let mut out = Vec::new();
    match policy {
        GpuPolicy::Software => {
            out.push(software());
            return out;
        }
        GpuPolicy::Named(name) => out.push(Attempt {
            drivers: Vec::new(),
            want: Want::Name(name.to_lowercase()),
            reason: format!("the adapter named like \"{name}\""),
        }),
        GpuPolicy::Integrated | GpuPolicy::Discrete => {
            let class = if *policy == GpuPolicy::Integrated { GpuClass::Integrated } else { GpuClass::Discrete };
            let label = if class == GpuClass::Integrated { "integrated" } else { "discrete" };
            for g in m.gpus.iter().filter(|g| g.class() == class) {
                let drivers = manifests_for_driver(&m.icds, &g.driver);
                if !drivers.is_empty() {
                    out.push(Attempt {
                        drivers,
                        want: Want::Kind(class),
                        reason: format!("the {label} GPU ({})", g.driver),
                    });
                }
            }
        }
        GpuPolicy::Auto => {}
    }
    // No GPU visible at all (a sandbox, a VM without one, CI): go straight
    // to the CPU driver rather than starting every hardware driver for
    // nothing.
    if m.gpus.is_empty() && !lavapipe.is_empty() {
        out.push(software());
    }
    match m.desktop_gpu() {
        Some((g, why)) => {
            let drivers = manifests_for_driver(&m.icds, &g.driver);
            if !drivers.is_empty() {
                out.push(Attempt {
                    drivers,
                    want: Want::Pci { vendor: g.vendor, device: g.device },
                    reason: format!("{why} ({})", g.driver),
                });
            }
            out.push(Attempt {
                drivers: Vec::new(),
                want: Want::Pci { vendor: g.vendor, device: g.device },
                reason: format!("{why} ({}), all drivers loaded", g.driver),
            });
        }
        None => {
            out.push(Attempt { drivers: Vec::new(), want: Want::Any, reason: "any GPU, all drivers loaded".into() })
        }
    }
    if !lavapipe.is_empty() && !out.iter().any(|a| a.want == Want::Cpu) {
        out.push(software());
    }
    out
}

//endregion

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    /// A scratch directory under the system temp dir, removed on drop.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Scratch {
            let p = std::env::temp_dir().join(format!("slerp-render-{name}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&p);
            fs::create_dir_all(&p).unwrap();
            Scratch(p)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    /// Builds a fake sysfs with a GPU at `pci_path` (relative to
    /// `devices/`) bound to `driver`, with the given nodes.
    fn fake_gpu(root: &Path, pci_path: &str, driver: &str, ids: (u32, u32), boot_vga: bool, nodes: &[(&str, &str)]) {
        let dev = root.join("devices").join(pci_path);
        fs::create_dir_all(&dev).unwrap();
        symlink(format!("/sys/bus/pci/drivers/{driver}"), dev.join("driver")).unwrap();
        fs::write(dev.join("vendor"), format!("0x{:04x}\n", ids.0)).unwrap();
        fs::write(dev.join("device"), format!("0x{:04x}\n", ids.1)).unwrap();
        fs::write(dev.join("boot_vga"), if boot_vga { "1\n" } else { "0\n" }).unwrap();
        let drm = root.join("class/drm");
        fs::create_dir_all(&drm).unwrap();
        for (node, devnum) in nodes {
            let n = dev.join("drm").join(node);
            fs::create_dir_all(&n).unwrap();
            fs::write(n.join("dev"), format!("{devnum}\n")).unwrap();
            symlink(&dev, n.join("device")).unwrap();
            symlink(&n, drm.join(node)).unwrap();
            // Connectors live next to nodes and must be ignored.
            let conn = dev.join("drm").join(format!("{node}-DP-1"));
            fs::create_dir_all(&conn).unwrap();
            let _ = symlink(&conn, drm.join(format!("{node}-DP-1")));
        }
    }

    fn tux_like(root: &Path) {
        fake_gpu(
            root,
            "pci0000:00/0000:00:01.1/0000:01:00.0",
            "nvidia",
            (0x10de, 0x2f18),
            false,
            &[("card1", "226:1"), ("renderD128", "226:128")],
        );
        fake_gpu(
            root,
            "pci0000:00/0000:00:08.1/0000:06:00.0",
            "amdgpu",
            (0x1002, 0x13c0),
            true,
            &[("card2", "226:2"), ("renderD129", "226:129")],
        );
    }

    fn fake_icds(dir: &Path, list: &[(&str, &str)]) {
        fs::create_dir_all(dir).unwrap();
        for (file, lib) in list {
            fs::write(
                dir.join(file),
                format!("{{\n  \"file_format_version\": \"1.0.1\",\n  \"ICD\": {{ \"library_path\": \"{lib}\", \"api_version\": \"1.4.0\" }}\n}}"),
            )
            .unwrap();
        }
    }

    fn all_icds(dir: &Path) -> Vec<IcdManifest> {
        fake_icds(
            dir,
            &[
                ("radeon_icd.json", "libvulkan_radeon.so"),
                ("intel_icd.json", "libvulkan_intel.so"),
                ("intel_hasvk_icd.json", "libvulkan_intel_hasvk.so"),
                ("nvidia_icd.json", "libGLX_nvidia.so.0"),
                ("lvp_icd.json", "/usr/lib/x86_64-linux-gnu/libvulkan_lvp.so"),
                ("nouveau_icd.json", "libvulkan_nouveau.so"),
            ],
        );
        scan_icds(&[dir.to_path_buf()])
    }

    fn names(paths: &[PathBuf]) -> Vec<String> {
        paths.iter().map(|p| p.file_name().unwrap().to_string_lossy().into_owned()).collect()
    }

    #[test]
    fn scans_fake_sysfs() {
        let s = Scratch::new("scan");
        tux_like(&s.0);
        let gpus = scan_drm(&s.0.join("class/drm"));
        assert_eq!(gpus.len(), 2);
        let nv = gpus.iter().find(|g| g.driver == "nvidia").unwrap();
        assert_eq!(nv.nodes, vec!["card1", "renderD128"]);
        assert_eq!(nv.dev_numbers, vec![(226, 1), (226, 128)]);
        assert_eq!((nv.vendor, nv.device, nv.boot_vga), (0x10de, 0x2f18, false));
        assert_eq!(nv.pci_slot(), Some("0000:01:00.0"));
        assert_eq!(nv.class(), GpuClass::Discrete);
        let amd = gpus.iter().find(|g| g.driver == "amdgpu").unwrap();
        assert!(amd.boot_vga);
        assert_eq!(amd.class(), GpuClass::Integrated);
    }

    #[test]
    fn missing_sysfs_is_empty() {
        assert!(scan_drm(Path::new("/nonexistent/slerp/drm")).is_empty());
    }

    #[test]
    fn classifies_intel_and_amd() {
        let g = |path: &str, driver: &str| DrmGpu {
            device_path: PathBuf::from(path),
            nodes: vec![],
            dev_numbers: vec![],
            driver: driver.into(),
            vendor: 0,
            device: 0,
            boot_vga: false,
        };
        assert_eq!(g("/sys/devices/pci0000:00/0000:00:02.0", "i915").class(), GpuClass::Integrated);
        assert_eq!(g("/sys/devices/pci0000:00/0000:00:01.0/0000:03:00.0", "xe").class(), GpuClass::Discrete);
        assert_eq!(g("/sys/devices/pci0000:00/0000:00:01.0", "amdgpu").class(), GpuClass::Integrated);
        assert_eq!(g("/sys/devices/pci0000:00/0000:00:03.1/0000:0a:00.0", "amdgpu").class(), GpuClass::Discrete);
        assert_eq!(g("/sys/devices/platform/soc/fd00000.gpu", "panfrost").class(), GpuClass::Integrated);
        assert_eq!(g("/sys/devices/pci0000:00/0000:00:01.0", "virtio_gpu").class(), GpuClass::Unknown);
    }

    #[test]
    fn dev_t_split() {
        // makedev(226, 128) and a large minor.
        assert_eq!(split_dev((226 << 8) | 128), (226, 128));
        let dev = ((300u64 & 0xfff) << 8) | ((5000u64 & 0xff) | ((5000u64 & !0xff) << 12));
        assert_eq!(split_dev(dev), (300, 5000));
    }

    #[test]
    fn maps_drivers_to_manifests() {
        let s = Scratch::new("icd");
        let icds = all_icds(&s.0);
        assert_eq!(names(&manifests_for_driver(&icds, "amdgpu")), vec!["radeon_icd.json"]);
        assert_eq!(names(&manifests_for_driver(&icds, "nvidia")), vec!["nvidia_icd.json"]);
        assert_eq!(names(&manifests_for_driver(&icds, "i915")), vec!["intel_hasvk_icd.json", "intel_icd.json"]);
        assert_eq!(names(&manifests_for_driver(&icds, "nouveau")), vec!["nouveau_icd.json"]);
        assert!(manifests_for_driver(&icds, "mystery").is_empty());
        assert_eq!(names(&lavapipe_manifests(&icds)), vec!["lvp_icd.json"]);
    }

    #[test]
    fn manifest_parsing_is_forgiving() {
        let s = Scratch::new("icd-bad");
        fs::write(s.0.join("broken.json"), "{ \"ICD\": { \"library_path\": ").unwrap();
        fs::write(s.0.join("empty.json"), "").unwrap();
        fs::write(s.0.join("escaped.json"), r#"{"ICD":{"library_path" : "\/opt\/x\/libvulkan_radeon.so"}}"#).unwrap();
        let icds = scan_icds(&[s.0.clone(), PathBuf::from("/nonexistent/icd.d")]);
        assert_eq!(icds.len(), 1);
        assert_eq!(icds[0].library, "/opt/x/libvulkan_radeon.so");
        assert_eq!(icds[0].library_stem(), "libvulkan_radeon");
    }

    #[test]
    fn earlier_directories_win() {
        let s = Scratch::new("icd-order");
        fake_icds(&s.0.join("a"), &[("radeon_icd.json", "/custom/libvulkan_radeon.so")]);
        fake_icds(&s.0.join("b"), &[("radeon_icd.json", "libvulkan_radeon.so")]);
        let icds = scan_icds(&[s.0.join("a"), s.0.join("b")]);
        assert_eq!(icds.len(), 1);
        assert_eq!(icds[0].library, "/custom/libvulkan_radeon.so");
    }

    #[test]
    fn search_dirs_follow_xdg() {
        let env = |k: &str| match k {
            "HOME" => Some("/home/u".to_string()),
            "XDG_DATA_DIRS" => Some("/opt/share:/usr/share".to_string()),
            _ => None,
        };
        let dirs: Vec<String> = icd_search_dirs(&env).iter().map(|p| p.to_string_lossy().into_owned()).collect();
        assert_eq!(
            dirs,
            vec![
                "/home/u/.config/vulkan/icd.d",
                "/etc/xdg/vulkan/icd.d",
                "/etc/vulkan/icd.d",
                "/home/u/.local/share/vulkan/icd.d",
                "/opt/share/vulkan/icd.d",
                "/usr/share/vulkan/icd.d",
            ]
        );
    }

    #[test]
    fn policy_parsing() {
        assert_eq!(GpuPolicy::parse("Integrated"), GpuPolicy::Integrated);
        assert_eq!(GpuPolicy::parse(" auto "), GpuPolicy::Auto);
        assert_eq!(GpuPolicy::parse("software"), GpuPolicy::Software);
        assert_eq!(GpuPolicy::parse("RTX 5070"), GpuPolicy::Named("RTX 5070".into()));
    }

    fn machine(root: &Path, icd_dir: &Path, wayland_main: Option<u64>) -> Machine {
        Machine { gpus: scan_drm(&root.join("class/drm")), icds: all_icds(icd_dir), wayland_main }
    }

    #[test]
    fn auto_prefers_wayland_main_device() {
        let s = Scratch::new("plan-auto");
        tux_like(&s.0);
        // The compositor names the NVIDIA render node, though the AMD GPU is
        // the boot display.
        let m = machine(&s.0, &s.0.join("icd"), Some((226 << 8) | 128));
        let p = plan(&GpuPolicy::Auto, &m);
        assert_eq!(names(&p[0].drivers), vec!["nvidia_icd.json"]);
        assert_eq!(p[0].want, Want::Pci { vendor: 0x10de, device: 0x2f18 });
        assert!(p[1].drivers.is_empty());
        assert_eq!(p.last().unwrap().want, Want::Cpu);
        // Without Wayland, the boot display wins.
        let m = machine(&s.0, &s.0.join("icd2"), None);
        let p = plan(&GpuPolicy::Auto, &m);
        assert_eq!(names(&p[0].drivers), vec!["radeon_icd.json"]);
    }

    #[test]
    fn integrated_and_discrete_fall_back() {
        let s = Scratch::new("plan-pref");
        tux_like(&s.0);
        let m = machine(&s.0, &s.0.join("icd"), Some((226 << 8) | 1));
        let p = plan(&GpuPolicy::Integrated, &m);
        assert_eq!(names(&p[0].drivers), vec!["radeon_icd.json"]);
        assert_eq!(p[0].want, Want::Kind(GpuClass::Integrated));
        // Then the desktop GPU, as with Auto.
        assert_eq!(names(&p[1].drivers), vec!["nvidia_icd.json"]);
        let p = plan(&GpuPolicy::Discrete, &m);
        assert_eq!(names(&p[0].drivers), vec!["nvidia_icd.json"]);

        // A desktop with only a discrete card: "integrated" still finds it.
        let s2 = Scratch::new("plan-pref2");
        fake_gpu(
            &s2.0,
            "pci0000:00/0000:00:01.0/0000:01:00.0",
            "amdgpu",
            (0x1002, 0x744c),
            true,
            &[("card0", "226:0"), ("renderD128", "226:128")],
        );
        let m = machine(&s2.0, &s2.0.join("icd"), None);
        let p = plan(&GpuPolicy::Integrated, &m);
        assert_eq!(names(&p[0].drivers), vec!["radeon_icd.json"]);
        assert_eq!(p[0].want, Want::Pci { vendor: 0x1002, device: 0x744c });
    }

    #[test]
    fn no_gpu_goes_to_lavapipe() {
        let s = Scratch::new("plan-none");
        let m = Machine { gpus: vec![], icds: all_icds(&s.0), wayland_main: None };
        let p = plan(&GpuPolicy::Auto, &m);
        assert_eq!(names(&p[0].drivers), vec!["lvp_icd.json"]);
        assert_eq!(p[0].want, Want::Cpu);
        let p = plan(&GpuPolicy::Software, &m);
        assert_eq!(p.len(), 1);
    }

    #[test]
    fn named_loads_all_drivers_first() {
        let s = Scratch::new("plan-named");
        tux_like(&s.0);
        let m = machine(&s.0, &s.0.join("icd"), None);
        let p = plan(&GpuPolicy::Named("RTX".into()), &m);
        assert!(p[0].drivers.is_empty());
        assert_eq!(p[0].want, Want::Name("rtx".into()));
        assert_eq!(names(&p[1].drivers), vec!["radeon_icd.json"]);
    }
}
