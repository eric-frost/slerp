//! The `slerp` command: thumbnailer, resident helper, viewer and test
//! renderer in one binary.

mod associate;

use clap::{Parser, Subcommand};
use slerp_core::{Config, Prepared, ThumbRenderer};
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{Duration, Instant};

#[derive(Parser)]
#[command(name = "slerp", version, about = "Thumbnails and a viewer for game and 3D asset files")]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
    /// A file to open in the viewer when no command is given.
    file: Option<PathBuf>,
}

#[derive(Subcommand)]
enum Command {
    /// Writes a PNG thumbnail no larger than SIZE pixels (run by file managers).
    Thumbnail { size: u32, input: PathBuf, output: PathBuf },
    /// Runs the resident helper that keeps the GPU loaded between thumbnails.
    Helper,
    /// Opens the viewer.
    View {
        file: Option<PathBuf>,
        /// Window class (X11) or app id (Wayland), for testing.
        #[arg(long)]
        class: Option<String>,
        /// Stays loaded without a window while the process with this id
        /// runs, to open the next file at once (the thumbnail helper starts
        /// one so).
        #[arg(long, hide = true)]
        warm_for: Option<u32>,
    },
    /// Renders one frame offscreen to a PNG, optionally replaying a viewer
    /// state (camera, clip and time, morphs, overlays) from a JSONC file.
    /// Without a state, a file that carries a picture of itself gives that
    /// picture, as its thumbnail does.
    Render {
        input: PathBuf,
        output: PathBuf,
        #[arg(long, default_value_t = 512)]
        size: u32,
        #[arg(long)]
        state: Option<PathBuf>,
        /// Draw the file's content even when it carries a saved picture.
        #[arg(long)]
        content: bool,
    },
    /// Prints what a file holds.
    Info {
        file: PathBuf,
        /// One part of a file that holds several, such as a WAD's map.
        #[arg(long)]
        part: Option<String>,
    },
    /// Converts a 3D file Slerp opens (a model, level, Blender, Maya or 3ds
    /// Max scene, MetaHuman DNA, a Doom map named with --part) to GLB.
    /// Pictures keep their original files where glTF can hold them.
    Convert {
        input: PathBuf,
        output: PathBuf,
        /// One part of a file that holds several, such as a WAD's map.
        #[arg(long)]
        part: Option<String>,
        /// Move the scene so the centre of its box is at the origin.
        #[arg(long)]
        centre: bool,
        /// Write every picture as PNG rather than embedding original files.
        #[arg(long)]
        png: bool,
    },
    /// Lists or extracts what is inside a file: the lumps of a Doom WAD, the
    /// files of a PK3, PK7, Quake PAK, Godot PCK, Valve VPK or Build GRP;
    /// the nodes, collections, meshes, materials, textures and clips of a
    /// model; the mip levels, layers and faces of a texture, and a cube map
    /// as one panorama; the layers, frames and tags of a sprite; the pages,
    /// layers or levels of other files.
    Contents {
        #[command(subcommand)]
        action: ContentsAction,
    },
    /// Chooses which file types get Slerp's thumbnails and open in Slerp by
    /// default, and makes the desktop follow (`--list` shows them all).
    Associate(associate::AssociateArgs),
    /// Prints the table of formats Slerp handles, or writes the files
    /// generated from it.
    Formats(associate::FormatsArgs),
    /// Shows Slerp thumbnails in Explorer and offers the viewer in
    /// "Open with", for the current user (no administrator rights needed),
    /// following the choices of `slerp associate`.
    #[cfg(windows)]
    Register,
    /// Undoes `register`.
    #[cfg(windows)]
    Unregister,
}

#[derive(Subcommand)]
enum ContentsAction {
    /// Prints the items inside FILE as a tree with their sizes and kinds.
    List {
        file: PathBuf,
        /// One tab-separated line per item (path, size, kind, formats), for
        /// scripts.
        #[arg(long)]
        flat: bool,
    },
    /// Writes items from FILE as ordinary files under --out: Doom pictures,
    /// flats and sprites as PNG (with their offsets), sounds as WAV, music
    /// as MIDI, pictures embedded in a model exactly as stored, other
    /// pictures as PNG (or EXR when their values go beyond 0 to 1), meshes,
    /// nodes, collections, materials and clips as GLB, and everything else
    /// as stored.
    /// Existing files are kept unless --force is given, and nothing is
    /// written outside --out.
    Extract {
        file: PathBuf,
        /// Items to extract: paths, names, folders or globs (`*.png`,
        /// `Sprites/TROO*`, `**/THINGS`), compared without regard to case.
        /// All of them when none are named.
        items: Vec<String>,
        /// The folder to write into (created if missing).
        #[arg(long, short)]
        out: PathBuf,
        /// auto (each item's own choice: converted pictures, sounds and
        /// music, original embedded pictures, meshes as GLB), raw, png, exr,
        /// wav, mid, wad (a map as a WAD of its lumps) or glb.
        #[arg(long, default_value = "auto")]
        format: String,
        /// GLB files of nodes and meshes: move them to the origin instead of
        /// keeping their place in the scene.
        #[arg(long)]
        centre: bool,
        /// GLB files of clips: the skeleton alone, without the meshes it
        /// moves.
        #[arg(long)]
        skeleton_only: bool,
        /// Replace files that exist.
        #[arg(long)]
        force: bool,
        /// Also write the empty marker lumps (S_START and the like).
        #[arg(long)]
        markers: bool,
        /// Stop after writing this many bytes (default: a thousand times the
        /// archive's size, at least 1 GiB).
        #[arg(long)]
        max_total: Option<u64>,
    },
}

/// Lets a command that prints for a pipe end quietly when the reader stops
/// early (`slerp formats | head`), as command-line tools do, instead of
/// panicking on the closed pipe.  Only printing commands use it: the helper
/// and the viewer keep Rust's default, where a closed socket is an error to
/// handle, not a signal that ends the process.
fn exit_quietly_on_closed_pipe() {
    // SAFETY: restoring a signal's default disposition before any threads
    // start is sound.
    #[cfg(unix)]
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let result = match cli.command {
        Some(Command::Thumbnail { size, input, output }) => thumbnail(size, &input, &output),
        Some(Command::Helper) => helper(),
        Some(Command::View { warm_for: Some(helper), .. }) => stay_ready(helper),
        Some(Command::View { file, class, .. }) => view(file, class),
        Some(Command::Render { input, output, size, state, content }) => {
            render(&input, &output, size, state.as_deref(), content)
        }
        Some(Command::Info { file, part }) => {
            exit_quietly_on_closed_pipe();
            info(&file, part.as_deref())
        }
        Some(Command::Convert { input, output, part, centre, png }) => {
            convert(&input, &output, part.as_deref(), centre, png)
        }
        Some(Command::Contents { action }) => {
            exit_quietly_on_closed_pipe();
            contents(action)
        }
        Some(Command::Associate(args)) => {
            exit_quietly_on_closed_pipe();
            associate::associate(args)
        }
        Some(Command::Formats(args)) => {
            exit_quietly_on_closed_pipe();
            associate::formats(args)
        }
        #[cfg(windows)]
        Some(Command::Register) => shell_registration(true),
        #[cfg(windows)]
        Some(Command::Unregister) => shell_registration(false),
        None => view(cli.file, None),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("slerp: {e}");
            ExitCode::FAILURE
        }
    }
}

/// `slerp contents list|extract`: the work is `slerp_core::contents`'s.
fn contents(action: ContentsAction) -> Result<(), String> {
    use slerp_core::contents::{self, ExtractOptions, Format};
    match action {
        ContentsAction::List { file, flat } => {
            use std::io::Write;
            let text = contents::list_file(&file, flat).map_err(|e| format!("{}: {e}", file.display()))?;
            // A reader that stops early (`| head`) isn't an error.
            let _ = std::io::stdout().lock().write_all(text.as_bytes());
            Ok(())
        }
        ContentsAction::Extract { file, items, out, format, force, markers, max_total, centre, skeleton_only } => {
            let format = Format::parse(&format)
                .ok_or_else(|| format!("unknown format {format:?}: use auto, raw, png, exr, wav, mid, wad or glb"))?;
            let options =
                ExtractOptions { format, force, max_total, markers, centre, skeleton_only, ..Default::default() };
            let report = contents::extract_file(&file, &items, &out, &options)
                .map_err(|e| format!("{}: {e}", file.display()))?;
            for (item, why) in &report.skipped {
                eprintln!("slerp: not written: {item}: {why}");
            }
            println!("Wrote {} files ({} bytes) to {}", report.written.len(), report.bytes, out.display());
            if report.skipped.is_empty() {
                Ok(())
            } else {
                Err(format!("{} items were not written", report.skipped.len()))
            }
        }
    }
}

/// Opens the viewer: in a viewer that stayed loaded after its last window
/// closed, when one is listening (which takes a few milliseconds), or else
/// in this process.
#[cfg(feature = "viewer")]
fn view(file: Option<PathBuf>, class: Option<String>) -> Result<(), String> {
    #[cfg(unix)]
    if slerp_viewer::hand_off(file.as_deref(), class.as_deref()) {
        return Ok(());
    }
    #[cfg(windows)]
    release_own_console();
    slerp_viewer::run(file, class).map_err(|e| e.to_string())
}

/// Lets go of a console no shell shares, which the viewer doesn't need.
/// Explorer starts the viewer through `slerpw.exe`, which runs this
/// program without a console window; started any other way outside a
/// terminal, Windows opens one, which this closes after it has flashed up.
/// A console shared with a shell (the program was typed at a prompt) is
/// kept, so errors still show there.
#[cfg(all(windows, feature = "viewer"))]
fn release_own_console() {
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetConsoleProcessList(list: *mut u32, count: u32) -> u32;
        fn FreeConsole() -> i32;
    }
    let mut ids = [0u32; 2];
    // SAFETY: ids is valid for writes of two process ids.
    if unsafe { GetConsoleProcessList(ids.as_mut_ptr(), ids.len() as u32) } == 1 {
        // SAFETY: no preconditions; standard streams just stop going anywhere.
        unsafe { FreeConsole() };
    }
}

#[cfg(not(feature = "viewer"))]
fn view(_file: Option<PathBuf>, _class: Option<String>) -> Result<(), String> {
    Err("this build has no viewer".into())
}

/// `slerp view --warm-for HELPER`: a viewer without a window that waits
/// while the thumbnail helper runs (see `start_ready_viewer`).
#[cfg(all(target_os = "linux", feature = "viewer"))]
fn stay_ready(helper: u32) -> Result<(), String> {
    slerp_viewer::stay_ready(helper).map_err(|e| e.to_string())
}

#[cfg(not(all(target_os = "linux", feature = "viewer")))]
fn stay_ready(_helper: u32) -> Result<(), String> {
    Ok(())
}

/// Starts a viewer that stays loaded without a window while this helper
/// runs (`viewerWithHelper`): a file manager showing Slerp's thumbnails is a
/// sign that one of the files may be opened next, which then opens in a few
/// tens of milliseconds instead of a few hundred.  Only where there is a
/// display to open windows on, which GNOME's thumbnail sandbox has not.
#[cfg(all(target_os = "linux", feature = "viewer"))]
fn start_ready_viewer() {
    let has_display = ["WAYLAND_DISPLAY", "DISPLAY"].iter().any(|v| std::env::var_os(v).is_some_and(|d| !d.is_empty()));
    if !has_display {
        return;
    }
    let Ok(exe) = std::env::current_exe() else { return };
    let mut cmd = std::process::Command::new(exe);
    cmd.args(["view", "--warm-for", &std::process::id().to_string()])
        .env_remove("VK_DRIVER_FILES")
        .env_remove("VK_ICD_FILENAMES");
    let _ = slerp_helper::spawn_detached(&mut cmd);
}

/// The file manager's entry point.  2D formats are decoded right here; 3D
/// formats go to the resident helper, and are rendered in this process when
/// the helper isn't running (it is started for the next file) or can't be
/// reached, as inside GNOME's thumbnail sandbox.
fn thumbnail(size: u32, input: &std::path::Path, output: &std::path::Path) -> Result<(), String> {
    let size = size.clamp(16, 2048);
    let kind = slerp_core::sniff_file(input).map_err(|e| e.to_string())?.ok_or("not a supported format")?;
    if kind.usually_3d() {
        let req = slerp_helper::Request {
            path: std::path::absolute(input).map_err(|e| e.to_string())?,
            size,
            // Dolphin on KDE Frameworks 6.24 gives up after 2 s, counted from
            // before this process started.
            deadline: Instant::now() + Duration::from_millis(1700),
            frames: 0,
        };
        let start = || {
            let mut cmd = std::process::Command::new(std::env::current_exe()?);
            // The helper outlives this process and serves every later file,
            // so it shouldn't inherit settings meant for this one run.
            cmd.arg("helper").env_remove("VK_DRIVER_FILES").env_remove("VK_ICD_FILENAMES");
            slerp_helper::spawn_detached(&mut cmd)
        };
        match slerp_helper::request(&req, start) {
            Ok(slerp_helper::Reply::Png(png)) => {
                return slerp_core::write_atomic(output, &png).map_err(|e| e.to_string());
            }
            Ok(slerp_helper::Reply::NoThumbnail) => return Err("no thumbnail for this file".into()),
            Err(slerp_helper::ClientError::TimedOut) => return Err("timed out".into()),
            // Not running yet, unreachable, or failed: render here instead.
            Ok(_) | Err(_) => {}
        }
    }
    let image = match slerp_core::prepare_thumbnail(input, size).map_err(|e| e.to_string())? {
        Prepared::Done(image) => image,
        Prepared::Render(scene) => {
            let config = Config::load();
            let mut renderer = ThumbRenderer::new(&config.gpu);
            renderer.render_thumbnail(&scene, size).map_err(|e| e.to_string())?
        }
    };
    slerp_core::write_atomic(output, &slerp_core::encode_png(&image)).map_err(|e| e.to_string())
}

/// Serves thumbnail requests until idle.  Loading runs in parallel on the
/// helper's threads; only drawing takes the GPU lock.
#[cfg(not(windows))]
fn helper() -> Result<(), String> {
    // Take the per-user lock before anything costly: when several
    // thumbnailers race to start a helper, the losers exit here at once.
    let config = Config::load();
    let opts =
        slerp_helper::ServeOptions { idle: Duration::from_secs(config.helper_idle_seconds), ..Default::default() };
    let server = match slerp_helper::Server::bind(opts) {
        Ok(server) => server,
        Err(slerp_helper::ServeError::AlreadyRunning) => return Ok(()),
        Err(e) => return Err(e.to_string()),
    };
    #[cfg(all(target_os = "linux", feature = "viewer"))]
    if config.viewer_with_helper {
        start_ready_viewer();
    }
    let renderer = std::sync::Mutex::new(ThumbRenderer::new(&config.gpu));
    let handler = move |req: &slerp_helper::Request| -> slerp_helper::Reply {
        if req.frames > 0 {
            return picture(&renderer, req);
        }
        let prepared = match slerp_core::prepare_thumbnail(&req.path, req.size) {
            Ok(p) => p,
            Err(slerp_core::ThumbError::Unknown) => return slerp_helper::Reply::NoThumbnail,
            Err(e) => return slerp_helper::Reply::Failed(e.to_string()),
        };
        let image = match prepared {
            Prepared::Done(image) => image,
            Prepared::Render(scene) => {
                let mut r = renderer.lock().unwrap_or_else(|p| p.into_inner());
                match r.render_thumbnail(&scene, req.size) {
                    Ok(image) => image,
                    Err(e) => return slerp_helper::Reply::Failed(e.to_string()),
                }
            }
        };
        slerp_helper::Reply::Png(slerp_core::encode_png(&image))
    };
    server.run(handler).map(|_| ()).map_err(|e| e.to_string())
}

/// Answers the Qt image plugin, which asks with `frames > 0` and shows
/// what it gets in Dolphin's Information panel, Gwenview and any other Qt
/// program: a texture as one raw frame at up to the requested size, alpha
/// kept, or a model, level or material as a turntable of `frames` frames.
#[cfg(not(windows))]
fn picture(renderer: &std::sync::Mutex<ThumbRenderer>, req: &slerp_helper::Request) -> slerp_helper::Reply {
    use slerp_helper::Reply;
    let images = match slerp_core::prepare_picture(&req.path, req.size) {
        Ok(Prepared::Done(image)) => vec![image],
        Ok(Prepared::Render(scene)) => {
            let mut r = renderer.lock().unwrap_or_else(|p| p.into_inner());
            match r.turntable(&scene, req.size, req.frames) {
                Ok(frames) => frames,
                Err(slerp_core::ThumbError::Empty) => return Reply::NoThumbnail,
                Err(e) => return Reply::Failed(e.to_string()),
            }
        }
        Err(slerp_core::ThumbError::Unknown | slerp_core::ThumbError::Empty) => return Reply::NoThumbnail,
        Err(e) => return Reply::Failed(e.to_string()),
    };
    let (width, height) = (images[0].width, images[0].height);
    Reply::Frames { width, height, rgba: images.into_iter().map(|image| image.pixels).collect() }
}

fn render(
    input: &std::path::Path,
    output: &std::path::Path,
    size: u32,
    state: Option<&std::path::Path>,
    content: bool,
) -> Result<(), String> {
    // Without a viewer state to replay, the picture a file carries of itself
    // wins, as in its thumbnail.
    if state.is_none() && !content {
        let kind = slerp_core::sniff_file(input).map_err(|e| e.to_string())?.ok_or("not a supported format")?;
        if let Some(saved) = slerp_core::saved_picture(kind, input, size)
            && let Prepared::Done(image) = saved.thumbnail(size)
        {
            return slerp_core::write_atomic(output, &slerp_core::encode_png(&image)).map_err(|e| e.to_string());
        }
    }
    let state = match state {
        Some(p) => slerp_core::ViewState::load(p).map_err(|e| e.to_string())?,
        None => slerp_core::ViewState::default(),
    };
    let config = Config::load();
    let mut renderer = ThumbRenderer::new(&config.gpu);
    let image = renderer.render_file(input, size, &state).map_err(|e| e.to_string())?;
    slerp_core::write_atomic(output, &slerp_core::encode_png(&image)).map_err(|e| e.to_string())
}

/// Windows' thumbnail helper, which serves the thumbnail handler inside
/// Explorer over a named pipe.  The handler starts it on demand; the code
/// lives in `slerp_shell.dll`, which owns both ends of the pipe.
#[cfg(windows)]
fn helper() -> Result<(), String> {
    let serve = shell_dll_entry(b"SlerpServe\0")?;
    // SAFETY: SlerpServe takes no arguments and returns an exit code.
    match unsafe { serve() } {
        0 => Ok(()),
        code => Err(format!("the thumbnail helper stopped with code {code}")),
    }
}

/// An entry point of `slerp_shell.dll` beside this program: all of
/// them take no arguments and return a 32-bit status.  The DLL stays
/// loaded until the program exits.
#[cfg(windows)]
fn shell_dll_entry(name: &[u8]) -> Result<unsafe extern "system" fn() -> i32, String> {
    use std::os::windows::ffi::OsStrExt;
    type Entry = unsafe extern "system" fn() -> i32;
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn LoadLibraryW(name: *const u16) -> *mut std::ffi::c_void;
        fn GetProcAddress(module: *mut std::ffi::c_void, name: *const u8) -> Option<Entry>;
    }
    let dll = std::env::current_exe().map_err(|e| e.to_string())?.with_file_name("slerp_shell.dll");
    if !dll.is_file() {
        return Err(format!("{} is missing; it comes with slerp.exe", dll.display()));
    }
    let wide: Vec<u16> = dll.as_os_str().encode_wide().chain([0]).collect();
    // SAFETY: a NUL-terminated path.
    let module = unsafe { LoadLibraryW(wide.as_ptr()) };
    if module.is_null() {
        return Err(format!("can't load {}: {}", dll.display(), std::io::Error::last_os_error()));
    }
    // SAFETY: valid module and NUL-terminated name.
    unsafe { GetProcAddress(module, name.as_ptr()) }.ok_or_else(|| format!("{} isn't Slerp's", dll.display()))
}

/// Registers or unregisters `slerp_shell.dll` beside this program, as
/// `regsvr32` would, but for the current user only.  The DLL does the work,
/// so the thumbnail handler is always registered by its own code.
#[cfg(windows)]
fn shell_registration(register: bool) -> Result<(), String> {
    let entry = shell_dll_entry(if register { b"DllRegisterServer\0" } else { b"DllUnregisterServer\0" })?;
    // SAFETY: both entry points take no arguments and return an HRESULT.
    let hr = unsafe { entry() };
    if hr < 0 {
        return Err(format!("{:#010x}: {}", hr, std::io::Error::from_raw_os_error(hr)));
    }
    if register {
        println!("Explorer now shows Slerp thumbnails and offers Slerp in \"Open with\" for this user.");
        println!("Run it again if this folder moves.  Undo with: slerp unregister");
    } else {
        println!("Removed Slerp's Explorer thumbnails and \"Open with\" entry for this user.");
    }
    Ok(())
}

fn info(file: &std::path::Path, part: Option<&str>) -> Result<(), String> {
    let kind = slerp_core::sniff_file(file).map_err(|e| e.to_string())?.ok_or("not a supported format")?;
    println!("kind: {kind:?}");
    if let slerp_core::Kind::Archive(_) = kind {
        // What the archive holds, which needs no pictures.
        let c = slerp_core::contents::Contents::open(file).map_err(|e| e.to_string())?;
        println!("archive: {} ({} items)", c.label(), c.items().len());
        let mut kinds: Vec<(&str, usize)> =
            slerp_core::contents::count_kinds(c.items()).into_iter().map(|(k, n)| (k.label(), n)).collect();
        kinds.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
        println!("kinds: {}", kinds.iter().map(|(k, n)| format!("{n} {k}")).collect::<Vec<_>>().join(", "));
        for w in c.warnings() {
            println!("warning: {w}");
        }
        return Ok(());
    }
    if let slerp_core::Kind::Palette(_) = kind {
        let palette = slerp_core::load_palette(file).map_err(|e| e.to_string())?;
        print!("{}", palette.describe(500));
        return Ok(());
    }
    if let slerp_core::Kind::Dicom = kind {
        // The facts the viewer lists, which never include a patient's name,
        // ID or birth date.
        let dicom = slerp_core::dicom::Dicom::open(file).map_err(|e| e.to_string())?;
        for (name, value) in dicom.facts() {
            println!("{name}: {value}");
        }
        for warning in &dicom.parsed.warnings {
            println!("warning: {warning}");
        }
        return Ok(());
    }
    if let slerp_core::Kind::Volume(f) = kind {
        // The header's facts and the volume's shape; no data is read.
        let start = std::time::Instant::now();
        let opened = slerp_core::volumes::open(f, file, part, &slerp_core::volumes::OpenOptions::thumbnail())
            .map_err(|e| e.to_string())?;
        println!("opened in {:.1} ms", start.elapsed().as_secs_f64() * 1000.0);
        if !opened.parts.is_empty() {
            println!("parts: {}", opened.parts.join(" | "));
        }
        match opened.content {
            slerp_core::volumes::Content::Volume(v) => {
                println!("format: {}", v.format);
                println!("shape: {:?} {}", v.shape(), v.dtype.name());
                for a in &v.axes {
                    let dir = a.dir.map(|d| format!(" towards {}", d.letter())).unwrap_or_default();
                    let size = if a.spacing > 0.0 { format!(" {} {}", a.spacing, v.unit) } else { String::new() };
                    println!("axis {} ({:?}): {}{size}{dir}", a.name, a.role, a.len);
                }
                if let Some(why) = &v.no_picture {
                    println!("no picture: {why}");
                }
                for (k, val) in &v.facts {
                    println!("{k}: {val}");
                }
                for w in &v.warnings {
                    println!("warning: {w}");
                }
            }
            slerp_core::volumes::Content::Picture(p) => {
                println!("picture: {} x {}", p.texture.width(), p.texture.height());
                for (k, val) in &p.facts {
                    println!("{k}: {val}");
                }
            }
        }
        return Ok(());
    }
    let parts = slerp_core::parts(file);
    if !parts.is_empty() {
        println!("parts: {}", parts.join(" "));
    }
    let start = std::time::Instant::now();
    let limits = slerp_core::scene::Limits::viewer();
    let asset = match part {
        Some(p) => slerp_core::load_part(file, &limits, p),
        None => slerp_core::load(file, &limits),
    }
    .map_err(|e| e.to_string())?;
    println!("loaded in {:.1} ms", start.elapsed().as_secs_f64() * 1000.0);
    print!("{}", slerp_core::describe(&asset));
    Ok(())
}

/// `slerp convert`: loads the file as the viewer does and writes the scene
/// with `slerp_core::gltf::write_glb`.
fn convert(
    input: &std::path::Path,
    output: &std::path::Path,
    part: Option<&str>,
    centre: bool,
    png: bool,
) -> Result<(), String> {
    use slerp_core::gltf::{GlbOptions, Placement, Selection, write_glb};
    let start = Instant::now();
    let limits = slerp_core::scene::Limits::viewer();
    let asset = match part {
        Some(p) => slerp_core::load_part(input, &limits, p),
        None => slerp_core::load(input, &limits),
    }
    .map_err(|e| e.to_string())?;
    let slerp_core::scene::Asset::Scene(scene) = asset else {
        return Err(format!("{} isn't a 3D scene, so there is nothing to write as GLB", input.display()));
    };
    let loaded = start.elapsed();
    let options = GlbOptions {
        placement: if centre { Placement::Centred } else { Placement::InPlace },
        original_images: !png,
        ..GlbOptions::default()
    };
    let start = Instant::now();
    let glb = write_glb(&scene, &Selection::Scene, &options).map_err(|e| e.to_string())?;
    std::fs::write(output, &glb.bytes).map_err(|e| format!("{}: {e}", output.display()))?;
    for w in scene.info.warnings.iter().chain(&glb.warnings) {
        eprintln!("warning: {w}");
    }
    println!(
        "{}: {} KB, loaded in {:.0} ms, written in {:.0} ms",
        output.display(),
        glb.bytes.len().div_ceil(1024),
        loaded.as_secs_f64() * 1000.0,
        start.elapsed().as_secs_f64() * 1000.0
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    /// The Qt image plugin (`qt/`) speaks the helper's protocol from its own
    /// C++ copy of the client, so a change to the protocol must reach it too.
    #[test]
    fn qt_plugin_speaks_the_helpers_protocol() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("qt");
        let header = std::fs::read_to_string(root.join("client.h")).unwrap();
        let client = std::fs::read_to_string(root.join("client.cpp")).unwrap();
        let version = format!("constexpr uint8_t kProtocolVersion = {};", slerp_helper::PROTOCOL_VERSION);
        assert!(header.contains(&version), "qt/client.h must say: {version}");
        let size = format!("constexpr uint32_t kMaxSize = {};", slerp_helper::MAX_SIZE);
        let frames = format!("constexpr uint32_t kMaxFrames = {};", slerp_helper::MAX_FRAMES);
        assert!(client.contains(&size) && client.contains(&frames), "qt/client.cpp must say: {size} {frames}");
    }
}
