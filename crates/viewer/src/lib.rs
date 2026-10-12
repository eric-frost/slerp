//! Slerp's viewer: an egui window showing models, levels, textures,
//! materials and texture packs, drawn by the same renderer as thumbnails
//! on the same GPU device as the window.
//!
//! # Modes
//!
//! The mode follows what the file holds:
//!
//! - **3D scenes** open in an orbit camera around the object; levels (Quake
//!   and Half-Life maps) open in the first-person fly camera at the
//!   player's spawn point.  Panels offer the animation clips, morph
//!   targets (with an ARKit face panel and VRM expressions), the scene
//!   tree, materials, statistics, overlays and lighting.
//! - **Textures** show flat with pan and zoom, mip, layer, face and channel
//!   pickers and HDR exposure.  Panoramas and cubemaps can be looked around
//!   in, or used to light the next model.
//!   Linear data (FITS and XISF astronomy frames) shows through
//!   a display stretch: the panel's Stretch section picks the curve (Linear,
//!   Auto, Asinh, Log), moves the black and white points and, for a file
//!   with several planes, picks the plane; the curve is remembered per
//!   format, and the inspector lists the file's header cards (`stretch_view`).
//! - **Materials** show on a sphere, cube or plane with tiling and
//!   displacement sliders, which rewrite the material's uniform on the GPU
//!   every frame of a drag (`MaterialView::retune`) rather than rebuilding
//!   the shape, and the file's own preview picture when it has one.
//! - **Galleries** (Quake WADs, Doom WADs' title screen and automaps) show
//!   a grid of named pictures; clicking one opens it as a texture.
//! - **Sprites and maps** (Aseprite, Tiled) show like textures, with a
//!   layer list and, for sprites, Aseprite's tags played as clips.
//! - **Colour tables** (`.cube`, `.3dl`) show a picture split before and
//!   after grading; drag the split, pick a test picture, or drop a photo.
//! - **Archives** (Doom WADs, PK3 and PK7, Quake PAK and WAD2, Half-Life WAD3,
//!   Godot PCK, Valve VPK, Build GRP) open on what they always showed (a
//!   WAD's first map, a PAK's pictures) with a Contents panel:
//!   a tree of the entries (a WAD's namespaces and maps as folders), a
//!   search box, and the selected entry in the main view, converted where
//!   the game's format isn't a picture file (`contents_view`).  The panel
//!   extracts the selection to a folder, copies it for pasting in a file
//!   manager (Ctrl+C), and offers "Open in SLADE" or "Open in VPKEdit" when
//!   one is installed.  Archives are never written.
//! - **Pieces**: any other file that holds several things (a model's nodes,
//!   collections, meshes, materials, textures and clips, a MetaHuman's
//!   levels of detail, a texture's mips and faces and a cube map as one
//!   panorama, a sprite's layers, frames and tags, a PDF's pages) gets the
//!   same panel.  Clicking a piece shows it in the file that is open (a
//!   node, collection or mesh alone and framed, a clip playing, a blend
//!   shape at full weight, a mip or face picked, a layer alone), a texture
//!   opens as a picture while the model waits aside, and hovering a piece
//!   outlines it in the 3D view.  Rows have small pictures, meshes, nodes
//!   and collections drawn on the GPU in the background
//!   (`contents_thumbs`).  Pieces save as their original bytes when the
//!   file stored them, else as PNG, OpenEXR or GLB.
//! - **Drawings** (flat DXF files) show on light or dark paper with pan and
//!   zoom and a layer list; they are drawn again at every zoom, so lines
//!   stay crisp (`cad_view`).  Drawings with depth and Rhino models are 3D
//!   scenes with a node per layer.
//! - **Documents** (PDF and PDF-compatible Illustrator files) show one page
//!   or artboard at a time on grey, drawn again by the PDF renderer at every
//!   zoom so text stays sharp; Page Up, Page Down and the arrows step
//!   through the pages, which are the file's parts (`pages_view`).  Text on
//!   a page can be selected by dragging across it, double-clicking a word
//!   or Ctrl+A, and copied with Ctrl+C, the right-click menu or the panel's
//!   Copy Page Text button.  EPS and PostScript files are pictures: the
//!   preview they store, or Ghostscript's drawing when it is installed.
//!
//! # Panels
//!
//! The view fills the window but for the side panels, which are tabs:
//! Controls (camera, display, lighting, and whatever the file offers, such
//! as clips, parts, a material or a stretch), Inspector (what the file is
//! and holds) and Contents (its pieces, while it has any).  They start in
//! one tab bar on the left with Controls in front.  A tab drags by its
//! title to the side of the view or of another panel, or into another tab
//! bar, and the borders between them drag to resize; the view itself has no
//! tab and stays in the window (`dock`).  View ▸ Reset Panel Layout puts
//! them back, and F9 hides them all.
//!
//! # Keyboard
//!
//! KDE's standard shortcuts where they exist:
//!
//! | Keys | Action |
//! |---|---|
//! | Ctrl+O | Open a file |
//! | Ctrl+Q | Quit |
//! | Ctrl+W | Close the file |
//! | F5 | Reload the file |
//! | Page Up, Page Down, Alt+Left, Alt+Right | Previous or next file in the folder |
//! | Left, Right | Previous or next file; previous or next picture of an open gallery picture |
//! | F11, Ctrl+Shift+F | Full screen (Esc leaves it) |
//! | Ctrl+M | Show or hide the menu bar |
//! | F9 | Show or hide the side panels |
//! | Ctrl+Shift+S | Save a screenshot at any size |
//! | Ctrl+Shift+C | Copy the view state (JSONC for `slerp render --state`) |
//! | Ctrl+A | Select a document page's text |
//! | Ctrl+C | Copy the selected text, or the Contents panel's piece |
//! | F1 | Keyboard shortcuts |
//! | F6 | Move keyboard focus from the view to the panels |
//! | Space | Play or pause |
//! | `[`, `]` | Previous or next animation clip |
//! | `,`, `.` | Step the paused animation back or forward a frame |
//! | Tab | Switch between the orbit and first-person cameras |
//! | F | Frame the model (3D) or fit the picture (textures) |
//! | Home | Reset the view to how the file opened |
//! | 1, 3, 7 | Front, right side and top views; with Ctrl, back, left side and bottom |
//! | 5 | Orthographic or perspective |
//! | G | Ground grid |
//! | Ctrl+Plus, Ctrl+Minus, Ctrl+0 | Zoom in, out, actual size (textures) |
//! | W, A, S, D, Q, E | Fly forward, left, back, right, down, up; Shift goes faster |
//! | Esc | Clear a text selection; release the mouse in first person; leave a panorama, a gallery picture or full screen; then, with View ▸ Esc Closes the Window, close dialogs and the window |
//! | Backspace | Back to the gallery |
//! | Mouse back button | Leave a gallery picture or panorama, else the previous file |
//! | Mouse forward button | Next file in the folder |
//!
//! The mouse's back and forward buttons work wherever the pointer is.
//!
//! F frames rather than going full screen, because framing is what 3D
//! viewers (Blender, Unreal, Unity) use it for; F11 and Ctrl+Shift+F are
//! KDE's full screen keys.
//!
//! The keys without Ctrl (the arrows, Page Up and Page Down, Space and the
//! letters) belong to whatever has the keyboard focus.  The view has it at
//! start and whenever it is clicked, and then they work as listed.  A panel
//! control takes the focus when it is clicked (or reached with F6, Tab and
//! Shift+Tab), shows it in the desktop's focus colour, and uses those keys
//! itself, as Qt's controls do (`slerp_theme::widgets`): the arrows step a
//! combo box such as the Doom map picker without opening it, step a
//! slider, move a list's or radio group's selection, and Space toggles a
//! check box.  The mouse wheel over a combo box steps it too.  Escape, or a
//! click in the view, gives the keys back to the view.
//!
//! In the orbit camera, drag to turn, Shift-drag or drag with the middle or
//! right button to pan, scroll to move closer, and double-click a surface
//! to orbit around that point.  In first person, click the view to capture
//! the mouse for looking around (Esc releases it), or drag with the right
//! button.
//!
//! # Watch mode
//!
//! The open file and the files its loader read beside it (a glTF's `.bin`
//! and textures, an OBJ's `.mtl`, a map's WADs) are watched for changes on
//! disk (`watch`).  Once they have been quiet for 300 ms, since exporters
//! write in pieces, a bar over the view says "This file changed on disk."
//! with Reload and Always Reload.  Reloading, by the bar or F5, keeps the
//! view: camera, clip and time, morphs, hidden nodes, a texture's mip,
//! layer, channels, pan and zoom, a sprite's frame and layers (`keep`).
//! With View ▸ Always Reload, changes reload by themselves with a brief
//! "Reloaded" note.  A reload that fails (a half-written file) keeps what
//! was shown with a "Couldn't reload" note and tries again at the next
//! change.
//!
//! # Dragging items out
//!
//! Items of the Contents panel (the entries of a WAD, PK3 or PAK, the
//! pieces of a model, texture or sprite) can be dragged out of the window into a file manager or
//! any program that takes files, and land there as ordinary files
//! (`dragout`).  winit has no call for starting a drag out of a window, so
//! each platform uses its own: Wayland's data device on winit's own
//! connection (X11 sessions are not supported and say so), OLE's
//! `DoDragDrop` on Windows, and file promises on macOS.  Files are made only
//! when a drop target asks for them.  `examples/dragout_demo.rs` is a small
//! window for trying it by hand, and `tools/test-dragout.sh` drives it
//! without a person inside a nested sway or KWin.
//!
//! # Remembered between runs
//!
//! `saved.rs` keeps, in `$XDG_STATE_HOME/slerp/viewer.json`: recent
//! lighting and animation libraries, the last folder, whether the side
//! panels are shown and where they are docked, the View menu's Always
//! Reload and Esc Closes the Window, which panel sections are open
//! (`sections`), and the display stretch last used for each linear-data
//! format (FITS, XISF).
//!
//! # Starting quickly
//!
//! Starting a GPU device takes 150 to 800 ms, most of it in the driver, so
//! it starts first, on a thread of its own, and everything else happens
//! meanwhile: the file loads on a thread from the moment the process
//! starts, the desktop theme is read on another, and the window opens at
//! once, in the theme's colours with the view where the last window had it
//! (`shell`, `placeholder`).  When the device is ready, egui draws into the
//! window; the GPU thread then compiles the renderer's pipelines and builds
//! its lighting, and the window's first frames, which lay out the panels,
//! don't wait for those.  Only the chosen GPU's Vulkan driver loads, without
//! Mesa's device-select layer (`slerp_render::open_gpu_with`).  A loaded model's geometry is packed for the GPU on
//! the loading thread; its textures go up over the frames after the first
//! (`SceneView::streaming`), showing their small mips until then, and Basis
//! Universal textures stay compressed on the GPU (BC1, BC3, BC5, BC7 or
//! ASTC; `slerp_textures::decode_for_gpu`), a quarter or less of their
//! decoded size.  A MetaHuman's read files and refitted skin weights come
//! from a cache on disk after the first open (`slerp_dna`).
//!
//! The warm viewer goes further: after its last window closes, the process
//! stays loaded, without a window, for `viewerKeepAliveSeconds` (30 unless
//! the config file or Settings ▸ Stay Loaded After Closing says otherwise),
//! and a later `slerp view` hands it the file, so the window opens in a few
//! tens of milliseconds with the GPU, pipelines and lighting ready (see
//! `warm`).  On Linux the thumbnail helper starts one too, which waits
//! while the helper runs (`viewerWithHelper`, Settings ▸ Get Ready While
//! Browsing), so the first file opened from a file manager showing Slerp's
//! thumbnails opens as quickly.
//!
//! # Rendering only when needed
//!
//! The 3D view draws into an offscreen texture that egui shows as an
//! image.  A frame is drawn only when something it depends on changed
//! (camera, pose, settings, size, lighting), and the window asks for new
//! frames only while something moves: a playing clip, a crossfade, an eased
//! camera move, flying, or loading.  An idle window uses no GPU time.
//!
//! # Environment variables for testing
//!
//! - `SLERP_SCREENSHOT=<file.png>` saves the whole window as soon as
//!   the file is loaded and drawn, then quits.
//! - `SLERP_STATE_OUT=<file.jsonc>` writes the view state shown when
//!   that screenshot is taken, as "Copy View State" would.
//! - `SLERP_VIEW_STATE=<file.jsonc>` opens the file showing a saved
//!   view state; with `SLERP_ADD_ANIMATIONS`, once the clips are added,
//!   so it can name one of them.
//! - `SLERP_BENCH=<seconds>` plays the scene (its clip, or a turning
//!   camera) without waiting for the display, prints frame statistics and
//!   quits.
//! - `SLERP_INPUT="Tab drag:120,0 Space ctrl+shift+C"` feeds scripted
//!   keys, drags, clicks and scrolls to the window once the file has
//!   loaded, before any screenshot; the steps are listed in `script.rs`.
//! - `SLERP_ADD_ANIMATIONS=<file>` adds an animation library's clips
//!   once the file is shown, as "Add Animations from File" does.
//! - `SLERP_SAVE_GLB=<file.glb>` saves the first scene shown there, as
//!   File > Save as GLB does, without the file dialog.
//! - `SLERP_QUIT_AFTER=<seconds>` quits that long after the file is
//!   shown, for measuring an idle window.
//! - `SLERP_DEBUG_REPAINT=1` prints why egui drew each window frame,
//!   for tracking down frames an idle window shouldn't draw.
//! - `SLERP_TIMING=1` prints start-up times and, at exit, how many
//!   frames were drawn.
//! - `SLERP_WINDOW_SIZE=<width>x<height>` sets the window's size.
//! - `SLERP_GPU_DELAY_MS=<ms>` starts the GPU that much later, to see the
//!   placeholder window on systems whose GPU is usually ready before the
//!   window opens (Direct3D 12 often is).
//! - `SLERP_FILE_TYPES=1` opens Settings > File Types at start;
//!   `SLERP_FILE_TYPES=<text>` opens it searching for that text.
//! - `SLERP_NO_VSYNC=1` presents frames without waiting for the display,
//!   for timing windows the compositor doesn't show (behind
//!   `hide-windows`), whose frames it would otherwise hold back.
//! - `SLERP_NO_HANDOFF=1` makes `slerp view` open its own viewer even when
//!   a warm one is listening, and `SLERP_VIEWER_KEEP_ALIVE=<seconds>`
//!   overrides `viewerKeepAliveSeconds` (0 exits with the window).  A warm
//!   viewer reads these hooks from the `slerp view` command that asked for
//!   each window, not from its own environment.

mod anim;
mod app;
mod cad_view;
mod camera;
mod canvas_view;
mod dicom_view;
mod dna;
mod dock;
mod document;
pub mod dragout;
mod env;
mod file_types;
mod gfx;
mod keep;
mod loader;
mod look;
mod lut_view;
#[cfg(target_os = "macos")]
mod macos;
mod pages_view;
mod palette_view;
mod panels;
mod paths_view;
mod placeholder;
mod saved;
mod scene_view;
mod script;
mod sections;
mod shell;
mod slice_view;
mod stretch_view;
mod texture_view;
mod viewstate;
mod volume_view;
mod warm;
mod watch;

use gfx::{Environment, Kept, Waiting};
use saved::Saved;
use shell::{GpuSource, Opening, Parts, Shell, UserEvent, Wake};
use slerp_render::Renderer;
use slerp_scene::textures::GpuBlocks;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc;
use std::time::{Duration, Instant};
use winit::event_loop::EventLoop;
use winit::platform::run_on_demand::EventLoopExtRunOnDemand;

pub use env::hooks_of_this_process;
#[cfg(unix)]
pub use warm::hand_off;

#[derive(Debug, thiserror::Error)]
pub enum ViewerError {
    #[error("no GPU for the viewer: {0}")]
    Gpu(String),
    #[error("couldn't open the window: {0}")]
    Window(String),
}

/// A text that names keys, as this desktop names them.  Shortcuts written
/// with Ctrl use egui's `Modifiers::COMMAND`, which on macOS is the Command
/// key, so there they read "Cmd".
pub(crate) fn keys(text: &str) -> std::borrow::Cow<'_, str> {
    if cfg!(target_os = "macos") { text.replace("Ctrl", "Cmd").into() } else { text.into() }
}

/// Opens the viewer, showing `file` if given.  `class` replaces the window
/// class and Wayland app id `slerp`, for tests behind `hide-windows`.
///
/// After the window closes, the process stays loaded for
/// `viewerKeepAliveSeconds` (see `warm`): without a window, holding its
/// GPU, compiled pipelines and lighting, and opening a window at once for
/// each `slerp view` command that [`hand_off`] sends it, one at a time.
pub fn run(file: Option<PathBuf>, class: Option<String>) -> Result<(), ViewerError> {
    let started = Instant::now();
    warm::return_freed_memory();
    let timing = env::is_set("SLERP_TIMING");
    // Finder passes files as Apple Events rather than arguments.
    #[cfg(target_os = "macos")]
    macos::listen();
    let config = slerp_core::Config::load();
    let policy = slerp_core::parse_policy(&config.viewer_gpu);
    let at = move || started.elapsed().as_secs_f64() * 1000.0;

    // 1. The GPU first, on a thread of its own, since its driver takes
    //    longest (150 to 800 ms): the device, which the window's painter
    //    needs, then the renderer with its pipelines compiled and the
    //    studio lighting built.  It must be the GPU the desktop composites
    //    on: a window rendered on another GPU is copied across every frame,
    //    and with KWin on an NVIDIA card, a wgpu window on an AMD iGPU
    //    crashes outright.  Starting it changes the environment for a
    //    moment (pinning the Vulkan driver), which this thread does for it
    //    between its calls into the window system's libraries, since those
    //    read the environment without a lock (`shell::env_channel`).
    let blocks = likely_gpu_blocks(&policy);
    let wake = Wake::default();
    let (mut keeper, env_requests) = shell::env_channel(wake.clone());
    let (gpu_tx, gpu_rx) = mpsc::channel();
    let (renderer_tx, renderer_rx) = mpsc::channel();
    let gpu_wake = wake.clone();
    std::thread::Builder::new()
        .name("slerp-gpu".into())
        .spawn(move || {
            // Testing hook: a GPU that is ready only after the window has
            // opened, for seeing the placeholder where it usually is first.
            if let Some(ms) = env::var("SLERP_GPU_DELAY_MS").and_then(|v| v.parse().ok()) {
                std::thread::sleep(Duration::from_millis(ms));
            }
            match slerp_render::open_gpu_with(&policy, &mut keeper) {
                Ok(gpu) => {
                    let gpu = Arc::new(gpu);
                    if timing {
                        eprintln!("slerp view: GPU {} ({}) ready at {:.0} ms", gpu.info.name, gpu.chosen_by, at());
                    }
                    let _ = gpu_tx.send(Ok(gpu.clone()));
                    gpu_wake.wake();
                    let start = Instant::now();
                    let mut renderer = Renderer::new(&gpu, gfx::TARGET_FORMAT);
                    renderer.warm_up(&gpu);
                    if timing {
                        eprintln!(
                            "slerp view: renderer warmed up at {:.0} ms ({:.0} ms)",
                            at(),
                            start.elapsed().as_secs_f64() * 1000.0
                        );
                    }
                    let _ = renderer_tx.send(renderer);
                    // The window may be holding a loaded file for it.
                    gpu_wake.wake();
                }
                Err(e) => {
                    let _ = gpu_tx.send(Err(e.to_string()));
                    gpu_wake.wake();
                }
            }
        })
        .map_err(|e| ViewerError::Gpu(e.to_string()))?;

    // 2. The file and the desktop theme, each on a thread of its own: CPU
    //    work that overlaps with starting the GPU.  Models' textures stay in
    //    the block formats this GPU most likely samples; the renderer
    //    decodes them if it turns out not to.  Neither thread reads the
    //    environment except through Rust's own (locked) accessors, so the
    //    changes the GPU thread asks for are safe meanwhile.
    let waker = loader::Waker::default();
    let file = file.map(|f| std::path::absolute(&f).unwrap_or(f));
    let mut loader = loader::Loader::new(waker.clone(), blocks);
    if let Some(f) = &file {
        loader.request(f.clone());
    }
    let theme = detect_theme(wake.clone())?;
    let saved = Saved::load();

    // 3. The connection to the window system, while the GPU's driver
    //    loads.
    let mut event_loop =
        EventLoop::<UserEvent>::with_user_event().build().map_err(|e| ViewerError::Window(e.to_string()))?;
    wake.connect(event_loop.create_proxy());
    if timing {
        eprintln!("slerp view: window system connected at {:.0} ms", at());
    }

    // 4. This command's window, then, as a warm viewer, one for each
    //    command handed over.
    let (home_tx, home_rx) = mpsc::channel::<Kept>();
    let window = Window {
        file,
        class,
        started,
        timing,
        config,
        waker,
        loader,
        theme,
        saved,
        gpu: GpuSource::Coming(gpu_rx),
        renderer: RendererSource::Coming(renderer_rx),
        environment: Environment::Studio,
        env: Some(env_requests),
    };
    open_window(&mut event_loop, window, home_tx)?;
    let Ok(kept) = home_rx.try_recv() else { return Ok(()) };
    stay_warm(&mut event_loop, kept, &wake, None, timing)
}

/// Starts a viewer without a window that stays loaded for as long as the
/// process `helper` runs, and at least `viewerKeepAliveSeconds`: the
/// thumbnail helper starts one when it starts (`viewerWithHelper`), so the
/// first file opened from the file manager opens at once.  Does nothing
/// when another viewer is already waiting.
#[cfg(target_os = "linux")]
pub fn stay_ready(helper: u32) -> Result<(), ViewerError> {
    // 1. One viewer waits at a time; don't start a GPU for nothing.
    if slerp_helper::ViewerListener::bind().is_err() {
        return Ok(());
    }
    warm::return_freed_memory();
    let timing = env::is_set("SLERP_TIMING");
    let config = slerp_core::Config::load();
    let policy = slerp_core::parse_policy(&config.viewer_gpu);
    // 2. The GPU and the renderer on this thread, before any other thread
    //    exists, so the environment can change without care.
    let gpu = Arc::new(slerp_render::open_gpu(&policy).map_err(|e| ViewerError::Gpu(e.to_string()))?);
    let mut renderer = Renderer::new(&gpu, gfx::TARGET_FORMAT);
    renderer.warm_up(&gpu);
    renderer.trim();
    // 3. The window system, for the windows to come.
    let mut event_loop =
        EventLoop::<UserEvent>::with_user_event().build().map_err(|e| ViewerError::Window(e.to_string()))?;
    let wake = Wake::default();
    wake.connect(event_loop.create_proxy());
    let kept = Kept { gpu, renderer, environment: Environment::Studio };
    stay_warm(&mut event_loop, kept, &wake, Some(helper), timing)
}

/// The warm viewer: waits for `slerp view` commands (see `warm::wait`) and
/// opens a window for each, with the GPU and renderer the last one left,
/// until no more come.
fn stay_warm(
    event_loop: &mut EventLoop<UserEvent>,
    mut kept: Kept,
    wake: &Wake,
    helper: Option<u32>,
    timing: bool,
) -> Result<(), ViewerError> {
    let (home_tx, home_rx) = mpsc::channel::<Kept>();
    loop {
        // wgpu frees what the closed window dropped (its targets, egui's
        // textures) only when the device is next polled, and the blocks of
        // GPU memory they emptied go back to the driver then.
        let _ = kept.gpu.device.poll(slerp_render::wgpu::PollType::wait_indefinitely());
        if timing {
            report_gpu_memory(&kept.gpu);
        }
        let keep = Duration::from_secs(slerp_core::Config::load().viewer_keep_alive_seconds);
        let Some(request) = warm::wait(event_loop, keep, helper) else { return Ok(()) };
        open_window(event_loop, Window::for_request(request, kept, wake.clone())?, home_tx.clone())?;
        let Ok(k) = home_rx.try_recv() else { return Ok(()) };
        kept = k;
    }
}

/// Prints what a warm viewer keeps on the GPU between windows: the memory
/// its allocator holds, how much of it is in use, and the largest uses.
/// The driver's own memory for the device comes on top.
fn report_gpu_memory(gpu: &slerp_render::Gpu) {
    let Some(report) = gpu.device.generate_allocator_report() else { return };
    let mb = |b: u64| b as f64 / (1024.0 * 1024.0);
    eprintln!(
        "slerp view: GPU memory between windows: {:.1} MB in use of {:.1} MB in blocks of {:?} MB",
        mb(report.total_allocated_bytes),
        mb(report.total_reserved_bytes),
        report.blocks.iter().map(|b| mb(b.size).round() as u64).collect::<Vec<_>>()
    );
    let mut uses = report.allocations;
    uses.sort_by_key(|a| std::cmp::Reverse(a.size));
    for a in uses.iter().take(8) {
        eprintln!("slerp view:   {:.1} MB {}", mb(a.size), a.name);
    }
}

/// The block formats the viewer's GPU most likely samples, before it is
/// known: BC on desktops and Apple silicon, ASTC on other ARM machines, and
/// none on the software renderer, which would decode blocks at every read.
fn likely_gpu_blocks(policy: &slerp_render::GpuPolicy) -> GpuBlocks {
    if *policy == slerp_render::GpuPolicy::Software {
        return GpuBlocks::default();
    }
    let arm = cfg!(target_arch = "aarch64") && !cfg!(target_os = "macos");
    GpuBlocks { bc: !arm, astc: arm }
}

/// Reads the desktop theme on a thread of its own (about 9 ms, mostly
/// `fc-match`), and wakes the window's event loop when it is done.
fn detect_theme(wake: Wake) -> Result<mpsc::Receiver<slerp_theme::DesktopTheme>, ViewerError> {
    let (tx, rx) = mpsc::channel();
    std::thread::Builder::new()
        .name("slerp-theme".into())
        .spawn(move || {
            let _ = tx.send(slerp_theme::detect());
            wake.wake();
        })
        .map_err(|e| ViewerError::Window(e.to_string()))?;
    Ok(rx)
}

/// The renderer a window draws with: one a warm viewer kept, or the one the
/// GPU thread is still making.
enum RendererSource {
    Ready(Box<Renderer>),
    Coming(mpsc::Receiver<Renderer>),
}

/// Everything a window starts from.
struct Window {
    file: Option<PathBuf>,
    class: Option<String>,
    /// When the command asked for the window, for timings.
    started: Instant,
    timing: bool,
    config: slerp_core::Config,
    waker: loader::Waker,
    loader: loader::Loader,
    theme: mpsc::Receiver<slerp_theme::DesktopTheme>,
    saved: Saved,
    gpu: GpuSource,
    renderer: RendererSource,
    environment: Environment,
    /// The GPU thread's changes to the environment, for the first window.
    env: Option<shell::EnvRequests>,
}

impl Window {
    /// A window a warm viewer opens for a `slerp view` command, with the
    /// GPU and renderer the last window left.
    fn for_request(request: warm::Request, kept: Kept, wake: Wake) -> Result<Window, ViewerError> {
        let started = Instant::now();
        env::set_window_env(request.env);
        let timing = env::is_set("SLERP_TIMING");
        if timing {
            eprintln!("slerp view: request for {:?} taken by the warm viewer", request.file);
        }
        let config = slerp_core::Config::load();
        let waker = loader::Waker::default();
        let mut loader = loader::Loader::new(waker.clone(), kept.gpu.gpu_blocks());
        if let Some(f) = &request.file {
            loader.request(f.clone());
        }
        Ok(Window {
            file: request.file,
            class: request.class,
            started,
            timing,
            config,
            waker,
            loader,
            theme: detect_theme(wake)?,
            saved: Saved::load(),
            gpu: GpuSource::Ready(kept.gpu),
            renderer: RendererSource::Ready(Box::new(kept.renderer)),
            environment: kept.environment,
            env: None,
        })
    }
}

/// Opens a window and runs it until it closes.  The renderer comes back
/// through `home` when the window's viewer is dropped.
fn open_window(
    event_loop: &mut EventLoop<UserEvent>,
    window: Window,
    home: mpsc::Sender<Kept>,
) -> Result<(), ViewerError> {
    let Window { file, class, started, timing, config, waker, loader, theme, saved, gpu, renderer, environment, env } =
        window;
    let app_id = class.unwrap_or_else(|| "slerp".into());
    let size = env::var("SLERP_WINDOW_SIZE")
        .and_then(|s| {
            let (w, h) = s.split_once('x')?;
            Some([w.trim().parse::<f32>().ok()?, h.trim().parse::<f32>().ok()?])
        })
        .unwrap_or([1400.0, 900.0]);
    let title = file
        .as_ref()
        .and_then(|f| f.file_name())
        .map_or_else(|| "Slerp".to_string(), |n| format!("{} — Slerp", n.to_string_lossy()));
    let builder = egui::ViewportBuilder::default()
        .with_title(title)
        .with_app_id(app_id)
        .with_inner_size(size)
        .with_min_inner_size([480.0, 320.0])
        .with_drag_and_drop(true)
        // KWin draws the title bar (server-side decorations), in the
        // user's own window decoration theme.
        .with_decorations(true);
    // Timing runs behind hide-windows, whose hidden windows the compositor
    // paces as it likes, and the benchmark measures frames, so both present
    // without waiting for the display.
    let surface = if env::is_set("SLERP_NO_VSYNC") || env::is_set("SLERP_BENCH") {
        egui_wgpu::SurfaceConfig {
            present_mode: slerp_render::wgpu::PresentMode::AutoNoVsync,
            desired_maximum_frame_latency: Some(2),
        }
    } else {
        egui_wgpu::SurfaceConfig::HIGH_THROUGHPUT
    };
    let view_insets = saved.view_insets;
    let mut start = Some((renderer, environment, loader, saved, home));
    let make: shell::MakeViewer<'_> = Box::new(move |parts: Parts<'_>| {
        let Parts { ctx, render_state, gpu, theme } = parts;
        let (renderer, environment, mut loader, saved, home) = start.take().expect("made once");
        slerp_theme::apply(ctx, &theme);
        let watcher = slerp_theme::ThemeWatcher::new(ctx, theme);
        let _ = waker.set(ctx.clone());
        #[cfg(target_os = "macos")]
        macos::set_context(ctx);
        // Later files load for this GPU exactly.
        loader.set_gpu_blocks(gpu.gpu_blocks());
        let renderer = match renderer {
            RendererSource::Ready(r) => Waiting::ready(*r),
            RendererSource::Coming(rx) => Waiting::later(rx, gpu.clone()),
        };
        let gfx = gfx::Gfx::new(gpu, renderer, environment, render_state.renderer.clone(), Some(home));
        app::Viewer::new(app::Start { gfx, theme: watcher, config, waker, loader, file, started, timing, saved })
    });
    let opening = Opening { builder, gpu, theme, env, view_insets, surface, make, started, timing };
    let mut shell = Shell::new(opening, event_loop.create_proxy());
    let result = event_loop.run_app_on_demand(&mut shell);
    // The viewer and the window went when it closed (`Shell::close`), and
    // the renderer is on its way home.
    let ended = shell.finish();
    result.map_err(|e| ViewerError::Window(e.to_string()))?;
    ended
}
