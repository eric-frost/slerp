//! The viewer window: menus, tool bar, side panels and status bar around
//! the view, keyboard shortcuts, files (open dialog, drag and drop,
//! stepping through a folder), screenshots, and the testing hooks.

use crate::camera::{Mode, ray};
use crate::document::{Content, Document, Panorama};
use crate::gfx::{FrameKey, Gfx};
use crate::loader::{self, Loader, Task, Waker, poll, spawn};
use crate::look::Look;
use crate::panels;
use crate::saved::Saved;
use crate::scene_view::to_ndc;
use crate::texture_view::TextureView;
use crate::watch::{BarAction, Change, FileWatch, Notice};
use egui::{Key, Modifiers, PointerButton, Sense};
use glam::{Vec2, Vec3};
use slerp_core::{Config, ViewState};
use slerp_render::{Background, RenderSettings};
use slerp_scene::{Asset, Image8, Scene};
use slerp_theme::{ThemeWatcher, widgets};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

// The Contents panel for archives lives in its own file but is a child of
// this module, so it can reach the viewer's private state.
#[path = "contents_view.rs"]
pub(crate) mod contents_view;
// Dragging entries out of that panel into a file manager.
#[path = "contents_drag.rs"]
mod contents_drag;
// File > Save as GLB.
#[path = "save_glb.rs"]
mod save_glb;

/// Everything `run` prepared before the window existed.
pub struct Start {
    pub gfx: Gfx,
    pub theme: ThemeWatcher,
    pub config: Config,
    pub waker: Waker,
    pub loader: Loader,
    pub file: Option<PathBuf>,
    pub started: Instant,
    pub timing: bool,
    /// What the viewer remembers, read before the window opened.
    pub saved: Saved,
}

/// Things the menus, tool bar, panels and keyboard ask for, carried out in
/// one place.
#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    Open,
    Reload,
    Close,
    Quit,
    Step(isize),
    /// One slice along (a volume's slice axis), from the Up and Down keys.
    StepSlice(isize),
    ChooseEnvironment,
    SetEnvironment(Option<PathBuf>),
    /// The fill light slider changed: remember it for the next run.
    SaveFill,
    ChooseLibrary,
    AddLibrary(PathBuf),
    Screenshot,
    CopyViewState,
    Fullscreen,
    TogglePanels,
    /// Puts the side panels back in one tab bar on the right.
    ResetPanels,
    ToggleMenuBar,
    Shortcuts,
    About,
    Frame,
    Home,
    ToggleCamera,
    PlayPause,
    StepClip(isize),
    StepFrame(isize),
    View(f32, f32),
    ToggleOrtho,
    ToggleGrid,
    ZoomIn,
    ZoomOut,
    ActualSize,
    BackToGallery,
    Panorama(bool),
    UseAsLighting,
    /// Left or Right: the previous or next picture of an open gallery
    /// picture, otherwise the previous or next file.
    Arrow(isize),
    /// Backs out of one thing; see `Viewer::escape`.
    Escape,
    FocusPanels,
    /// Opens the file dialog for a photo to grade with a colour table.
    ChoosePhoto,
    /// Grades this photo with the colour table shown.
    UsePhoto(PathBuf),
    /// Reload files that change on disk without asking (View menu).
    SetAlwaysReload(bool),
    /// Esc closes the window once it has nothing else to leave (View
    /// menu).
    SetEscCloses(bool),
    /// Closes the bar or note about the file on disk.
    DismissNotice,
    /// The mouse's back button: leaves a gallery picture or panorama, else
    /// the previous file.
    Back,
    /// Shows another part of the file (a WAD's map in 3D), or the whole
    /// file (its gallery of automaps).
    OpenPart(loader::Part),
    /// How many seconds the viewer stays loaded after its last window
    /// closes (Settings menu; `viewerKeepAliveSeconds` in the config file).
    SetKeepAlive(u64),
    /// Whether the thumbnail helper starts a viewer that stays loaded while
    /// it runs (Settings menu; `viewerWithHelper` in the config file).
    SetWithHelper(bool),
    /// Opens this file (a DICOM series' slice, a DICOMDIR's image).
    OpenFile(PathBuf),
    /// The Contents panel's buttons: extract, copy, open in an editor.
    Contents(contents_view::Do),
    /// Opens Settings > File Types.
    FileTypes,
    /// File > Save as GLB: asks where, then writes the scene.
    SaveGlb,
}

/// What a file dialog was opened for.
enum Purpose {
    Open,
    Environment,
    Library,
    Screenshot(Shot),
    Photo,
    /// The folder the Contents panel extracts into.
    ExtractTo,
    /// The file a scene is saved to as GLB.
    SaveGlb,
}

/// A screenshot to render at a chosen size.
#[derive(Clone, Copy)]
pub struct Shot {
    pub width: u32,
    pub height: u32,
    pub transparent: bool,
}

pub struct ShotDialog {
    pub open: bool,
    pub width: u32,
    pub height: u32,
    pub transparent: bool,
}

/// Panel visibility and transient UI state.
pub struct UiState {
    pub show_panels: bool,
    pub show_menu: bool,
    pub shortcuts: bool,
    pub about: bool,
    pub shot: ShotDialog,
    pub morph_filter: String,
    pub clip_filter: String,
    pub tree_filter: String,
    pub show_all_nodes: bool,
    /// The clip the clip list last showed as selected.
    pub shown_clip: Option<usize>,
    /// The part of the file (a WAD's map) asked for and still loading, which
    /// the map picker shows meanwhile, so stepping through maps with the
    /// keyboard faster than they load goes on from the last one asked for.
    pub requested_part: Option<String>,
    pub thumbs: std::collections::HashMap<(u64, usize), egui::TextureHandle>,
}

/// Behaviour switched on by environment variables, for tests and
/// benchmarks.
struct Hooks {
    screenshot: Option<PathBuf>,
    /// When the window screenshot was last asked for.
    screenshot_asked: Option<Instant>,
    settled: u32,
    view_state: Option<ViewState>,
    bench: Option<Bench>,
    /// The first frame showing the file has been drawn, its textures
    /// perhaps still going up.
    shown: bool,
    /// The first frame showing the file whole has been drawn.
    first_frame: bool,
    script: Option<crate::script::Script>,
    /// Quit this long after the file is shown, for idle measurements.
    quit_after: Option<Duration>,
    ready_at: Option<Instant>,
    /// An animation library to add once the file is shown.
    add_animations: Option<PathBuf>,
    /// A screenshot the script's `shot` step asked for, and when it was
    /// last requested from the window (`None`: not yet).
    shot: Option<(PathBuf, Option<Instant>)>,
}

struct Bench {
    seconds: f32,
    start: Option<Instant>,
    last: Option<Instant>,
    cpu_ms: Vec<f32>,
    interval_ms: Vec<f32>,
}

pub struct Viewer {
    pub gfx: Gfx,
    theme: ThemeWatcher,
    pub config: Config,
    pub saved: Saved,
    pub waker: Waker,
    loader: Loader,
    pub doc: Option<Document>,
    /// The file shown or last asked for, for stepping and reloading.
    current: Option<PathBuf>,
    failed: Option<(PathBuf, String)>,
    notice: Option<(String, Instant, bool)>,
    pub look: Look,
    pub ui: UiState,
    dialog: Option<(Purpose, Task<Option<PathBuf>>)>,
    library: Option<(PathBuf, Task<Scene>)>,
    hooks: Hooks,
    view_id: egui::Id,
    last_frame: Option<Instant>,
    pub view_size: (u32, u32),
    /// Where the view is, in points.
    view_rect: egui::Rect,
    /// The view's distances from the window's edges outside full screen,
    /// saved when the window closes for the next window's placeholder.
    view_insets: Option<crate::placeholder::ViewInsets>,
    position: Option<(usize, usize)>,
    pointer_locked: bool,
    /// A panel control had the keyboard focus at the end of the last frame.
    /// Escape then only takes the focus from it (egui drops it before the
    /// frame), rather than also leaving full screen or closing the window.
    control_focused: bool,
    hover: Option<String>,
    started: Instant,
    timing: bool,
    egui_frames: u64,
    pub actions: Vec<Action>,
    pending_screenshot: Option<Shot>,
    /// Watches the file shown (or loading) and its neighbours for changes.
    watch: FileWatch,
    /// A reload of the file shown is loading; when it lands, the view
    /// carries over.
    reloading: Option<PathBuf>,
    /// The bar or note about the file on disk, over the view.
    file_notice: Option<Notice>,
    /// The Contents panel, while an archive is open (see `contents_view`).
    pub contents: Option<contents_view::State>,
    /// Where the view and the side panels are (see `dock`).
    pub dock: crate::dock::Dock,
    /// A scene being written as GLB.
    glb: Option<save_glb::Saving>,
    /// Settings > File Types, while open.
    file_types: Option<crate::file_types::FileTypes>,
}

impl Viewer {
    pub fn new(start: Start) -> Viewer {
        let saved = start.saved;
        let dock = crate::dock::Dock::new(saved.layout.clone());
        crate::stretch_view::load_saved(&saved);
        let mut look = Look::default();
        look.fills = saved.fills.clone();
        // Lighting carries over between runs, as it does between files.
        if let Some(env) = saved.environment.clone().filter(|p| p.is_file()) {
            look.set_environment(Some(env), &start.waker);
        }
        let hooks = Hooks {
            screenshot: crate::env::var("SLERP_SCREENSHOT").map(PathBuf::from),
            screenshot_asked: None,
            settled: 0,
            view_state: crate::env::var("SLERP_VIEW_STATE").and_then(|p| match ViewState::load(Path::new(&p)) {
                Ok(s) => Some(s),
                Err(e) => {
                    eprintln!("slerp view: {e}");
                    None
                }
            }),
            bench: crate::env::var("SLERP_BENCH").and_then(|s| s.parse::<f32>().ok()).map(|seconds| Bench {
                seconds,
                start: None,
                last: None,
                cpu_ms: Vec::new(),
                interval_ms: Vec::new(),
            }),
            shown: false,
            first_frame: false,
            script: crate::script::Script::from_env(),
            quit_after: crate::env::var("SLERP_QUIT_AFTER")
                .and_then(|s| s.parse::<f32>().ok())
                .map(|s| Duration::from_secs_f32(s.max(0.0))),
            ready_at: None,
            add_animations: crate::env::var("SLERP_ADD_ANIMATIONS").map(PathBuf::from),
            shot: None,
        };
        let mut watch = FileWatch::new(start.waker.clone());
        if let Some(f) = &start.file {
            watch.watch(std::slice::from_ref(f));
        }
        Viewer {
            gfx: start.gfx,
            theme: start.theme,
            config: start.config,
            ui: UiState {
                show_panels: !saved.hide_panels,
                show_menu: true,
                shortcuts: false,
                about: false,
                shot: ShotDialog { open: false, width: 1920, height: 1080, transparent: false },
                morph_filter: String::new(),
                clip_filter: String::new(),
                tree_filter: String::new(),
                show_all_nodes: false,
                shown_clip: None,
                requested_part: None,
                thumbs: Default::default(),
            },
            saved,
            waker: start.waker,
            loader: start.loader,
            doc: None,
            current: start.file,
            failed: None,
            notice: None,
            look,
            dialog: None,
            library: None,
            hooks,
            view_id: egui::Id::new("slerp-view"),
            last_frame: None,
            view_size: (1, 1),
            view_rect: egui::Rect::NOTHING,
            view_insets: None,
            position: None,
            pointer_locked: false,
            control_focused: false,
            hover: None,
            started: start.started,
            timing: start.timing,
            egui_frames: 0,
            actions: Vec::new(),
            pending_screenshot: None,
            watch,
            reloading: None,
            file_notice: None,
            contents: None,
            dock,
            glb: None,
            // `SLERP_FILE_TYPES=1` (or a search) opens the page at start, for
            // tests and screenshots.
            file_types: crate::env::var("SLERP_FILE_TYPES").map(|search| {
                let mut page = crate::file_types::FileTypes::open();
                if search != "1" {
                    page.search = search;
                }
                page
            }),
        }
    }

    fn aspect(&self) -> f32 {
        self.view_size.0 as f32 / self.view_size.1.max(1) as f32
    }

    pub fn notify(&mut self, text: impl Into<String>) {
        self.notice = Some((text.into(), Instant::now(), false));
    }

    pub fn warn(&mut self, text: impl Into<String>) {
        self.notice = Some((text.into(), Instant::now(), true));
    }

    /*┌─────────────────╮
    | Files           |
    └─────────────────╯*/
    //region Files

    fn open(&mut self, path: PathBuf) {
        let path = std::path::absolute(&path).unwrap_or(path);
        self.current = Some(path.clone());
        self.reloading = None;
        self.file_notice = None;
        // Watched from now, so a change made while it loads counts.
        self.watch.watch(std::slice::from_ref(&path));
        self.loader.request(path);
    }

    /// Loads the file shown again, keeping how it is shown (see `keep`).
    /// A file that isn't showing (it failed to open) is simply opened.
    fn reload(&mut self) {
        let Some(path) = self.current.clone() else { return };
        // An entry of an archive (or a card about one) reloads the archive's
        // listing in place and goes on showing the entry.
        if self.contents.is_some()
            && self.doc.as_ref().is_none_or(|d| d.origin.is_some())
            && contents_view::reload(self)
        {
            return;
        }
        if self.doc.as_ref().is_some_and(|d| d.path == path) {
            if matches!(self.file_notice, Some(Notice::Changed | Notice::Failed(_))) {
                self.file_notice = None;
            }
            self.reloading = Some(path.clone());
            let part = self.shown_part();
            self.loader.request_part(path, part);
        } else {
            self.open(path);
        }
    }

    /// Which part of the file is shown, so a reload brings back the same
    /// map of a WAD (or its gallery).
    fn shown_part(&self) -> loader::Part {
        match &self.doc {
            // A PDF's page is chosen in the view itself.
            Some(Document { content: Content::Pages(p), .. }) => loader::Part::Named(p.part_name()),
            Some(Document { part: Some(p), .. }) => loader::Part::Named(p.clone()),
            Some(d) if !d.part_names.is_empty() => loader::Part::Whole,
            _ => loader::Part::First,
        }
    }

    /// Answers files that changed on disk and have since been quiet.
    fn file_changed(&mut self, change: Change) {
        let Some(path) = self.current.clone() else { return };
        let showing = self.doc.as_ref().is_some_and(|d| d.source_path() == path)
            || (self.doc.is_none() && self.contents.as_ref().is_some_and(|c| c.archive == path && c.has_card()));
        if !showing {
            // Nothing of this file is on screen to keep (it is loading, or
            // failed to open, perhaps half-written): just load it again.
            if !change.main_missing && (self.loader.busy().is_some() || self.failed.is_some()) {
                self.open(path);
            }
            return;
        }
        match Notice::for_change(&change, self.saved.always_reload) {
            Some(notice) => self.file_notice = Some(notice),
            None => self.reload(),
        }
    }

    /// The files watch mode watches for `doc`: the file and what the loader
    /// read beside it.
    fn watch_doc(&mut self, doc: &Document) {
        let mut files = vec![doc.source_path().to_path_buf()];
        files.extend(doc.neighbours.iter().cloned());
        self.watch.watch(&files);
    }

    /// Picks up finished loads and other background work.
    fn poll_background(&mut self, ctx: &egui::Context) {
        // 1. The file being loaded.  A reload that fails keeps what was
        //    shown, with a note.
        if let Some((path, result)) = self.loader.poll() {
            let aspect = self.aspect();
            let reload =
                self.reloading.take_if(|p| *p == path).is_some() && self.doc.as_ref().is_some_and(|d| d.path == path);
            self.ui.requested_part = None;
            let made = result.and_then(|loaded| Document::new(loaded, &mut self.gfx, aspect, &self.waker));
            // A temporary file the Contents panel asked for is an entry of
            // the archive, not a file of its own.
            let made = made.map(|mut doc| {
                contents_view::tag(self, &mut doc);
                doc
            });
            match made {
                Ok(doc) if reload => self.install_reload(doc),
                Ok(doc) => self.install(doc, ctx),
                Err(e) if reload => self.file_notice = Some(Notice::Failed(e)),
                Err(e) if contents_view::load_failed(self, &path, &e) => {}
                Err(e) => {
                    self.doc = None;
                    self.failed = Some((path.clone(), e));
                    self.set_title(ctx, &path);
                }
            }
        }
        // 1a. Files changed on disk, and a "Reloaded" note that is done.
        if let Some(change) = self.watch.poll() {
            self.file_changed(change);
        }
        if self.file_notice.as_ref().is_some_and(|n| n.expired(Instant::now())) {
            self.file_notice = None;
        }
        // 2. A lighting picture.
        if let Some(Err(e)) = self.look.poll() {
            self.warn(format!("Couldn't use that lighting: {e}"));
        }
        // 3. File dialogs.
        if let Some((_, rx)) = &self.dialog
            && let Some(result) = poll(rx)
        {
            let (purpose, _) = self.dialog.take().expect("checked above");
            match result {
                Ok(Some(path)) => match purpose {
                    Purpose::Open => self.open(path),
                    Purpose::Environment => self.actions.push(Action::SetEnvironment(Some(path))),
                    Purpose::Library => self.actions.push(Action::AddLibrary(path)),
                    Purpose::Screenshot(shot) => self.save_screenshot(&path, shot),
                    Purpose::Photo => self.actions.push(Action::UsePhoto(path)),
                    Purpose::ExtractTo => contents_view::extract_to(self, path),
                    Purpose::SaveGlb => save_glb::start(self, path),
                },
                Ok(None) => {}
                Err(e) => self.warn(format!("The file dialog failed: {e}")),
            }
        }
        save_glb::poll_save(self);
        // 4. An animation library.
        if let Some((_, rx)) = &self.library
            && let Some(result) = poll(rx)
        {
            let (path, _) = self.library.take().expect("checked above");
            self.add_library(&path, result);
        }
    }

    /// Shows a newly loaded document.
    fn install(&mut self, mut doc: Document, ctx: &egui::Context) {
        if self.timing {
            eprintln!(
                "slerp view: {} loaded in {:.0} ms, uploaded in {:.0} ms",
                doc.name(),
                doc.load_time.as_secs_f64() * 1000.0,
                doc.upload_time.as_secs_f64() * 1000.0
            );
        }
        contents_view::installed(self, &doc);
        match &doc.origin {
            Some(o) => ctx.send_viewport_cmd(egui::ViewportCommand::Title(format!("{} — Slerp", o.title))),
            None => self.set_title(ctx, &doc.path),
        }
        self.watch_doc(&doc);
        self.file_notice = None;
        self.position = if doc.origin.is_some() { None } else { loader::position_in_folder(&doc.path) };
        if let Some(dir) = doc.path.parent().filter(|_| doc.origin.is_none()) {
            self.saved.last_folder = Some(dir.to_path_buf());
        }
        self.failed = None;
        self.hover = None;
        self.ui.thumbs.clear();
        self.release_pointer(ctx);
        // Colour tables keep the picture and split of the one before;
        // palettes their background.
        if let (Some(Document { content: Content::Lut(old), .. }), Content::Lut(new)) = (&self.doc, &mut doc.content) {
            new.carry_over(old);
        }
        if let (Some(Document { content: Content::Palette(old), .. }), Content::Palette(new)) =
            (&self.doc, &mut doc.content)
        {
            new.carry_over(old);
        }
        // A drawing keeps its layers, paper and place while it reloads.
        if let (Some(Document { content: Content::Cad(old), .. }), Content::Cad(new)) = (&self.doc, &mut doc.content) {
            new.carry_over(old);
        }
        // DICOM slices of one series keep the window, inversion and zoom.
        if let (Some(Document { content: Content::Dicom(old), .. }), Content::Dicom(new)) =
            (&self.doc, &mut doc.content)
        {
            new.follow(old);
        }
        // Another part of the same file (a WAD's map) chosen in the panel
        // leaves the keyboard with the control that chose it, so the arrows
        // go on stepping through the maps; a new file gives it to the view.
        let same_file = self.doc.as_ref().is_some_and(|d| d.path == doc.path);
        let keep_focus = (same_file || doc.origin.is_some()) && self.control_focused;
        self.doc = Some(doc);
        // A view state waits for `SLERP_ADD_ANIMATIONS`' clips, which it
        // may name.
        if self.hooks.add_animations.is_none() {
            self.apply_view_state();
        }
        self.gfx.invalidate();
        if !keep_focus {
            ctx.memory_mut(|m| m.request_focus(self.view_id));
        }
    }

    /// Shows a reloaded document the way the one it replaces was shown.
    /// The mouse capture, keyboard focus and window title stay as they are.
    fn install_reload(&mut self, mut doc: Document) {
        if self.timing {
            eprintln!(
                "slerp view: {} reloaded in {:.0} ms, uploaded in {:.0} ms",
                doc.name(),
                doc.load_time.as_secs_f64() * 1000.0,
                doc.upload_time.as_secs_f64() * 1000.0
            );
        }
        let aspect = self.aspect();
        if let Some(old) = &self.doc {
            crate::keep::carry(old, &mut doc, &mut self.gfx, aspect);
        }
        // The Contents panel lists what the file holds now.
        contents_view::installed(self, &doc);
        self.watch_doc(&doc);
        self.position = loader::position_in_folder(&doc.path);
        self.failed = None;
        self.hover = None;
        self.ui.thumbs.clear();
        self.doc = Some(doc);
        self.gfx.invalidate();
        self.file_notice = Some(Notice::Reloaded(Instant::now()));
    }

    fn set_title(&self, ctx: &egui::Context, path: &Path) {
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        ctx.send_viewport_cmd(egui::ViewportCommand::Title(format!("{name} — Slerp")));
    }

    /// Opens the desktop's file dialog on a thread of its own, so the
    /// window keeps drawing while it is open.
    fn file_dialog(&mut self, frame: &crate::shell::Frame, purpose: Purpose) {
        if self.dialog.is_some() {
            return;
        }
        let folder = self
            .current
            .as_ref()
            .and_then(|p| p.parent().map(Path::to_path_buf))
            .or_else(|| self.saved.last_folder.clone());
        let mut dialog = rfd::FileDialog::new();
        if let Some(window) = frame.winit_window() {
            dialog = dialog.set_parent(window);
        }
        let save = matches!(purpose, Purpose::Screenshot(_) | Purpose::SaveGlb);
        match &purpose {
            Purpose::Open => {
                dialog = dialog.set_title("Open").add_filter("Supported files", crate::loader::extensions());
                if let Some(f) = &folder {
                    dialog = dialog.set_directory(f);
                }
            }
            Purpose::Environment => {
                let dir = self.look.environment.as_ref().and_then(|p| p.parent().map(Path::to_path_buf)).or(folder);
                dialog = dialog.set_title("Choose Lighting").add_filter(
                    "HDR and pictures",
                    &["hdr", "pic", "ktx2", "ktx", "dds", "png", "jpg", "jpeg", "webp", "tga"],
                );
                if let Some(d) = dir {
                    dialog = dialog.set_directory(d);
                }
            }
            Purpose::Library => {
                let dir =
                    self.saved.recent_libraries.first().and_then(|p| p.parent().map(Path::to_path_buf)).or(folder);
                dialog = dialog.set_title("Add Animations from File").add_filter(
                    "Animations",
                    &[
                        "glb", "gltf", "fbx", "vrma", "dae", "vmd", "md5anim", "md5mesh", "psa", "psk", "smd", "iqm",
                        "iqe",
                    ],
                );
                if let Some(d) = dir {
                    dialog = dialog.set_directory(d);
                }
            }
            Purpose::Photo => {
                dialog = dialog
                    .set_title("Grade a Photo")
                    .add_filter("Pictures", &["jpg", "jpeg", "png", "webp", "tga", "bmp", "hdr", "ktx2", "dds"]);
                if let Some(d) = folder {
                    dialog = dialog.set_directory(d);
                }
            }
            Purpose::SaveGlb => dialog = save_glb::configure(dialog, self.current.as_deref(), folder),
            Purpose::ExtractTo => {
                dialog = dialog.set_title("Extract To");
                if let Some(d) = folder {
                    dialog = dialog.set_directory(d);
                }
            }
            Purpose::Screenshot(_) => {
                let stem = self
                    .current
                    .as_ref()
                    .and_then(|p| p.file_stem())
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "screenshot".into());
                dialog = dialog
                    .set_title("Save Screenshot")
                    .add_filter("PNG", &["png"])
                    .set_file_name(format!("{stem}.png"));
                if let Some(d) = folder {
                    dialog = dialog.set_directory(d);
                }
            }
        }
        let folder_wanted = matches!(purpose, Purpose::ExtractTo);
        let rx = spawn(&self.waker, "slerp-dialog", move || {
            Ok(if save {
                dialog.save_file()
            } else if folder_wanted {
                dialog.pick_folder()
            } else {
                dialog.pick_file()
            })
        });
        self.dialog = Some((purpose, rx));
    }

    //endregion

    /*┌─────────────────╮
    | Actions         |
    └─────────────────╯*/
    //region Actions

    fn perform(&mut self, action: Action, ctx: &egui::Context, frame: &crate::shell::Frame) {
        let aspect = self.aspect();
        match action {
            Action::Open => self.file_dialog(frame, Purpose::Open),
            Action::SaveGlb => self.file_dialog(frame, Purpose::SaveGlb),
            Action::Reload => self.reload(),
            Action::OpenPart(part) => {
                if let Some(path) = self.current.clone() {
                    self.reloading = None;
                    self.ui.requested_part = match &part {
                        loader::Part::Named(name) => Some(name.clone()),
                        _ => None,
                    };
                    self.loader.request_part(path, part);
                }
            }
            Action::Close => {
                self.contents = None;
                self.doc = None;
                self.failed = None;
                self.current = None;
                self.position = None;
                self.reloading = None;
                self.file_notice = None;
                self.watch.clear();
                ctx.send_viewport_cmd(egui::ViewportCommand::Title("Slerp".into()));
            }
            Action::Quit => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
            Action::OpenFile(path) => self.open(path),
            Action::Contents(what) => contents_view::perform(self, what, frame),
            Action::StepSlice(delta) => {
                if let Some(Document { content: Content::Volume(v), .. }) = &mut self.doc
                    && v.has_picture()
                {
                    v.playing = false;
                    if !v.step_slice(delta) {
                        self.notify(if delta < 0 { "First slice" } else { "Last slice" });
                    }
                }
            }
            Action::Step(delta) => {
                // A document's Page Up and Page Down go through its pages.
                if let Some(Document { content: Content::Pages(p), part, .. }) = &mut self.doc
                    && p.count() > 1
                {
                    if p.step(delta) {
                        *part = Some(p.part_name());
                    } else {
                        self.notify(if delta < 0 { "First page" } else { "Last page" });
                    }
                    return;
                }
                // A volume's Page Up and Page Down go through its slices.
                if let Some(Document { content: Content::Volume(v), .. }) = &mut self.doc
                    && v.has_picture()
                    && v.slice_count() > 1
                {
                    v.playing = false;
                    if !v.step_slice(delta) {
                        self.notify(if delta < 0 { "First slice" } else { "Last slice" });
                    }
                    return;
                }
                // A DICOM image steps through its series in slice order.
                let dicom_step = match &mut self.doc {
                    Some(Document { content: Content::Dicom(d), .. }) => d.step(delta),
                    _ => None,
                };
                if let Some(next) = dicom_step {
                    match next {
                        Some(path) => self.open(path),
                        None => self.notify(if delta < 0 { "First slice" } else { "Last slice" }),
                    }
                    return;
                }
                if let Some(cur) = self.current.clone() {
                    match loader::step_in_folder(&cur, delta) {
                        Some(next) => self.open(next),
                        None => self.notify("No other files Slerp can open in this folder"),
                    }
                }
            }
            Action::ChooseEnvironment => self.file_dialog(frame, Purpose::Environment),
            Action::SetEnvironment(path) => {
                if let Some(p) = &path {
                    self.saved.add_environment(p);
                }
                self.saved.environment = path.clone();
                self.saved.save();
                self.look.set_environment(path, &self.waker);
            }
            Action::SaveFill => {
                self.saved.fills = self.look.fills.clone();
                self.saved.save();
            }
            Action::ChooseLibrary => self.file_dialog(frame, Purpose::Library),
            Action::AddLibrary(path) => self.load_library(path),
            Action::Screenshot => {
                if !self.can_screenshot() {
                    self.notify("Open a picture from the gallery to save it");
                    return;
                }
                let s = &mut self.ui.shot;
                if !s.open {
                    (s.width, s.height) = self.view_size;
                }
                s.open = true;
            }
            Action::CopyViewState => self.copy_view_state(ctx),
            Action::Fullscreen => {
                let full = ctx.input(|i| i.viewport().fullscreen.unwrap_or(false));
                ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(!full));
            }
            Action::TogglePanels => {
                self.ui.show_panels = !self.ui.show_panels;
                self.saved.hide_panels = !self.ui.show_panels;
                self.saved.save();
            }
            Action::ResetPanels => {
                self.dock.reset();
                self.saved.layout = None;
                self.ui.show_panels = true;
                self.saved.hide_panels = false;
                self.saved.save();
            }
            Action::ToggleMenuBar => self.ui.show_menu = !self.ui.show_menu,
            Action::Shortcuts => self.ui.shortcuts = !self.ui.shortcuts,
            Action::About => self.ui.about = true,
            Action::FileTypes => {
                if self.file_types.is_none() {
                    self.file_types = Some(crate::file_types::FileTypes::open());
                }
            }
            Action::Frame => {
                if let Some(Document { content: Content::Pages(p), .. }) = &mut self.doc {
                    p.fit_now();
                } else if let Some(doc) = &mut self.doc {
                    if let Some(view) = doc.scene_mut() {
                        let b = view.frame_bounds();
                        view.cameras.frame(&b, aspect);
                    } else if let Some(t) = doc.texture_mut() {
                        t.fit_now();
                    }
                }
            }
            Action::Home => {
                if let Some(Document { content: Content::Pages(p), .. }) = &mut self.doc {
                    p.fit_now();
                } else if let Some(doc) = &mut self.doc {
                    if let Some(view) = doc.scene_mut() {
                        view.cameras.home();
                    } else if let Some(t) = doc.texture_mut() {
                        t.fit_now();
                    }
                }
            }
            Action::ToggleCamera => {
                if let Some(view) = self.doc.as_mut().and_then(|d| d.scene_mut()) {
                    view.cameras.toggle_mode();
                }
                self.release_pointer(ctx);
            }
            Action::PlayPause => {
                if let Some(Document { content: Content::Volume(v), .. }) = &mut self.doc {
                    v.toggle_play();
                } else if let Some(Document { content: Content::Dicom(d), .. }) = &mut self.doc {
                    d.toggle_play();
                } else if let Some(c) = self.doc.as_mut().and_then(|d| d.canvas_mut()) {
                    c.player.play_pause();
                } else if let Some(view) = self.doc.as_mut().and_then(|d| d.scene_mut()) {
                    if view.scene.animations.is_empty() {
                        if view.has_effects {
                            self.look.animate_effects = !self.look.animate_effects;
                        }
                    } else {
                        let p = &mut view.player;
                        if p.clip.is_none() {
                            p.select(&view.scene, Some(0));
                        } else if !p.playing && !p.looping && p.time >= p.duration(&view.scene) {
                            p.time = 0.0;
                            p.playing = true;
                        } else {
                            p.playing = !p.playing;
                        }
                    }
                }
            }
            Action::StepClip(delta) => {
                if let Some(c) = self.doc.as_mut().and_then(|d| d.canvas_mut()) {
                    // "All frames" first, then the tags.
                    let n = c.canvas.clips().len() as isize + 1;
                    let now = c.player.clip.map_or(0, |i| i as isize + 1);
                    let next = (now + delta).rem_euclid(n);
                    let canvas = c.canvas.clone();
                    c.player.select(&*canvas, (next > 0).then(|| next as usize - 1));
                } else if let Some(view) = self.doc.as_mut().and_then(|d| d.scene_mut()) {
                    let n = view.scene.animations.len() as isize;
                    if n > 0 {
                        let next = view
                            .player
                            .clip
                            .map_or(if delta > 0 { 0 } else { n - 1 }, |c| (c as isize + delta).rem_euclid(n));
                        view.player.select(&view.scene, Some(next as usize));
                    }
                }
            }
            Action::StepFrame(delta) => {
                if let Some(Document { content: Content::Dicom(d), .. }) = &mut self.doc {
                    d.playing = false;
                    let n = d.frames().max(1) as isize;
                    d.show_frame((d.index as isize + delta).rem_euclid(n) as u32);
                } else if let Some(c) = self.doc.as_mut().and_then(|d| d.canvas_mut()) {
                    c.player.step(delta);
                } else if let Some(view) = self.doc.as_mut().and_then(|d| d.scene_mut()) {
                    let d = view.player.duration(&view.scene);
                    if view.player.clip.is_some() && d > 0.0 {
                        view.player.playing = false;
                        view.player.time = (view.player.time + delta as f32 / 30.0).clamp(0.0, d);
                        view.pose_dirty = true;
                    }
                }
            }
            Action::View(yaw, pitch) => {
                if let Some(view) = self.doc.as_mut().and_then(|d| d.scene_mut()) {
                    view.cameras.look_from(yaw, pitch);
                }
            }
            Action::ToggleOrtho => {
                if let Some(view) = self.doc.as_mut().and_then(|d| d.scene_mut()) {
                    let ortho = !view.cameras.orbit.ortho;
                    view.cameras.set_ortho(ortho);
                }
            }
            Action::ToggleGrid => self.look.overlays.grid = !self.look.overlays.grid,
            Action::ZoomIn | Action::ZoomOut => {
                let factor = if action == Action::ZoomIn { 1.25 } else { 0.8 };
                if let Some(Document { content: Content::Pages(p), .. }) = &mut self.doc {
                    p.zoom(factor as f64);
                } else if let Some(doc) = &mut self.doc {
                    if let Some(t) = doc.texture_mut() {
                        t.zoom(factor);
                    } else if let Some(view) = doc.scene_mut() {
                        view.cameras.dolly(if factor > 1.0 { 1.5 } else { -1.5 });
                    }
                }
            }
            Action::ActualSize => {
                if let Some(Document { content: Content::Pages(p), .. }) = &mut self.doc {
                    p.actual_size();
                } else if let Some(t) = self.doc.as_mut().and_then(|d| d.texture_mut()) {
                    t.actual_size();
                }
            }
            Action::BackToGallery => {
                if let Some(Document { content: Content::Gallery(g), .. }) = &mut self.doc {
                    g.open = None;
                }
            }
            Action::Panorama(on) => {
                let gfx = &mut self.gfx;
                if let Some(t) = self.doc.as_mut().and_then(|d| d.texture_mut()) {
                    t.inside = (on && t.is_panorama()).then(|| Panorama::new(gfx));
                }
            }
            Action::UseAsLighting => {
                if let Some(doc) = &self.doc {
                    let path = match &doc.content {
                        Content::Texture(_) => Some(doc.path.clone()),
                        _ => None,
                    };
                    match path {
                        Some(p) => {
                            self.perform(Action::SetEnvironment(Some(p)), ctx, frame);
                            self.notify("This picture now lights the models you open");
                        }
                        None => self.notify("Only a picture file on its own can be used as lighting"),
                    }
                }
            }
            Action::Arrow(d) => {
                if self.gallery_picture_open() {
                    if let Some(Document { content: Content::Gallery(g), .. }) = &mut self.doc
                        && let Err(e) = g.step(d, &mut self.gfx)
                    {
                        self.warn(e);
                    }
                } else {
                    self.perform(Action::Step(d), ctx, frame);
                }
            }
            Action::Escape => self.escape(ctx),
            Action::FocusPanels => ctx.memory_mut(|m| {
                m.surrender_focus(self.view_id);
                m.move_focus(egui::FocusDirection::Next);
            }),
            Action::ChoosePhoto => self.file_dialog(frame, Purpose::Photo),
            Action::UsePhoto(path) => {
                if let Some(Document { content: Content::Lut(lut), .. }) = &mut self.doc
                    && let Err(e) = lut.use_photo(&path)
                {
                    self.warn(format!("Couldn't read {}: {e}", short_name(&path)));
                }
            }
            Action::SetAlwaysReload(on) => {
                self.saved.always_reload = on;
                self.saved.save();
                // Turned on from the bar: what changed reloads now.
                if on && self.file_notice == Some(Notice::Changed) {
                    self.reload();
                }
            }
            Action::SetEscCloses(on) => {
                self.saved.esc_closes = on;
                self.saved.save();
            }
            Action::DismissNotice => self.file_notice = None,
            Action::SetKeepAlive(seconds) => {
                self.config.viewer_keep_alive_seconds = seconds;
                if let Err(e) = Config::save_number("viewerKeepAliveSeconds", seconds) {
                    self.warn(format!("Couldn't save the setting: {e}"));
                }
            }
            Action::SetWithHelper(on) => {
                self.config.viewer_with_helper = on;
                if let Err(e) = Config::save_bool("viewerWithHelper", on) {
                    self.warn(format!("Couldn't save the setting: {e}"));
                }
            }
            Action::Back => {
                let panorama = self.doc.as_ref().and_then(|d| d.texture()).is_some_and(|t| t.inside.is_some());
                if panorama {
                    self.perform(Action::Panorama(false), ctx, frame);
                } else if self.gallery_picture_open() {
                    self.perform(Action::BackToGallery, ctx, frame);
                } else {
                    self.perform(Action::Step(-1), ctx, frame);
                }
            }
        }
    }

    fn can_screenshot(&self) -> bool {
        match &self.doc {
            Some(Document { content: Content::Gallery(g), .. }) => g.open.is_some(),
            Some(_) => true,
            None => false,
        }
    }

    fn release_pointer(&mut self, ctx: &egui::Context) {
        if self.pointer_locked {
            ctx.send_viewport_cmd(egui::ViewportCommand::CursorGrab(egui::CursorGrab::None));
            ctx.send_viewport_cmd(egui::ViewportCommand::CursorVisible(true));
            self.pointer_locked = false;
        }
    }

    fn copy_view_state(&mut self, ctx: &egui::Context) {
        let Some(doc) = &self.doc else { return };
        match crate::viewstate::capture(doc, &self.look) {
            Some(state) => {
                ctx.copy_text(state.to_jsonc(Some(&doc.path)));
                self.notify("View state copied: replay it with slerp render --state");
            }
            None => self.notify("Textures and galleries have no view state to copy"),
        }
    }

    /// Loads an animation library on a thread.
    fn load_library(&mut self, path: PathBuf) {
        if self.doc.as_ref().and_then(|d| d.scene()).is_none() {
            self.notify("Open a model first, then add animations to it");
            return;
        }
        let p = path.clone();
        let rx = spawn(&self.waker, "slerp-library", move || {
            match slerp_core::load(&p, &slerp_scene::Limits::viewer()).map_err(|e| e.to_string())? {
                Asset::Scene(scene) => Ok(scene),
                _ => Err("this file holds no skeleton or animations".into()),
            }
        });
        self.library = Some((path, rx));
    }

    /// Adds a loaded library's clips to the open scene.  Retargeting is
    /// another crate's work and may not be finished, so a panic there is
    /// reported, not fatal.
    fn add_library(&mut self, path: &Path, result: Result<Scene, String>) {
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let library = match result {
            Ok(l) => l,
            Err(e) => return self.warn(format!("Couldn't read {name}: {e}")),
        };
        let Some(view) = self.doc.as_mut().and_then(|d| d.scene_mut()) else { return };
        let before = view.scene.animations.len();
        let options = slerp_retarget::Options { foot_ik: view.foot_ik.on };
        let added = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            slerp_retarget::add_animations_with(&mut view.scene, &library, options)
        }));
        // Whatever happened, the clip list must stay consistent with the
        // player.
        if view.player.clip.is_some_and(|c| c >= view.scene.animations.len()) {
            view.player.clip = None;
        }
        match added {
            Ok(Ok(added)) => {
                let names = added.names;
                view.foot_ik.add(added.others);
                self.saved.add_library(path);
                self.saved.save();
                if let Some(clip) = crate::anim::pick_clip(&view.scene, before) {
                    view.player.select(&view.scene, Some(clip));
                }
                self.notify(format!("Added {} clips from {name}", names.len()));
            }
            Ok(Err(e)) => self.warn(format!("Couldn't use the clips in {name}: {e}")),
            Err(p) => self
                .warn(format!("Animations from other skeletons aren't available yet ({})", loader::panic_message(&p))),
        }
        self.apply_view_state();
    }

    /// Shows the view state `SLERP_VIEW_STATE` asked for, once.
    fn apply_view_state(&mut self) {
        let aspect = self.aspect();
        if let Some(state) = self.hooks.view_state.take()
            && let Some(doc) = &mut self.doc
        {
            crate::viewstate::apply(&state, doc, &mut self.look, &mut self.gfx, &self.waker, aspect);
        }
    }

    /// Renders and writes a screenshot at the chosen size.
    fn save_screenshot(&mut self, path: &Path, shot: Shot) {
        let max = self.gfx.gpu.device.limits().max_texture_dimension_2d.min(8192);
        let (w, h) = (shot.width.clamp(16, max), shot.height.clamp(16, max));
        let Some(doc) = &mut self.doc else { return };
        let image: Option<Image8> = if let Some(view) = doc.scene_mut() {
            let mut settings = self.look.settings(&view.scene, view.effects_time);
            settings.overlays.highlight_joint = None;
            if shot.transparent {
                settings.background = Background::Transparent;
            }
            let v = view.cameras.view(w as f32 / h as f32);
            Some(self.gfx.renderer.render_image(&self.gfx.gpu, &view.gpu, &v, &settings, w, h))
        } else if let Some(t) = doc.texture_mut()
            && let Some(inside) = &t.inside
        {
            let v = inside.camera.view(w as f32 / h as f32);
            let settings = panorama_settings(&t.params);
            Some(self.gfx.renderer.render_image(&self.gfx.gpu, &inside.scene, &v, &settings, w, h))
        } else if let Content::Lut(lut) = &doc.content {
            Some(lut.screenshot(w, h))
        } else if let Content::Palette(p) = &doc.content {
            Some(crate::palette_view::screenshot(p, w, h))
        } else if let Content::Cad(c) = &doc.content {
            Some(crate::cad_view::screenshot(c, w, h))
        } else if let Content::Pages(p) = &doc.content {
            crate::pages_view::screenshot(p, w, h)
        } else if let Some(t) = doc.texture_mut() {
            // Keep what the view shows: the same centre, zoomed so the
            // picture's width in the shot matches the view's.
            let mut params = t.params;
            params.zoom *= w as f32 / self.view_size.0.max(1) as f32;
            Some(self.gfx.renderer.draw_texture_image(&self.gfx.gpu, &t.gpu, &params, w, h))
        } else {
            None
        };
        let Some(image) = image else { return };
        match slerp_core::write_atomic(path, &slerp_core::encode_png(&image)) {
            Ok(()) => self.notify(format!("Saved {} ({w} × {h})", path.display())),
            Err(e) => self.warn(format!("Couldn't save {}: {e}", path.display())),
        }
    }

    //endregion

    /*┌─────────────────╮
    | Keyboard        |
    └─────────────────╯*/
    //region Keyboard

    /// Reads the keyboard.  Keys with Ctrl and function keys work
    /// anywhere; plain keys only while the view has focus (or nothing
    /// does), so they don't fight sliders and text fields.
    fn shortcuts(&mut self, ctx: &egui::Context) {
        let view_keys = ctx.memory(|m| m.focused().is_none() || m.has_focus(self.view_id));
        let typing = ctx.egui_wants_keyboard_input() && !view_keys;
        let ctrl = Modifiers::COMMAND;
        let ctrl_shift = Modifiers::COMMAND | Modifiers::SHIFT;
        let mut actions = Vec::new();
        ctx.input_mut(|i| {
            // Most specific first: consume_key ignores extra Shift and Alt.
            if i.consume_key(ctrl_shift, Key::S) {
                actions.push(Action::Screenshot);
            }
            // egui-winit reports Ctrl+Shift+C, like Ctrl+C, as a copy event
            // without a key event; scripts send the key.
            let copy_with_shift = i.modifiers.shift && i.events.iter().any(|e| matches!(e, egui::Event::Copy));
            if copy_with_shift {
                i.events.retain(|e| !matches!(e, egui::Event::Copy));
            }
            if i.consume_key(ctrl_shift, Key::C) || copy_with_shift {
                actions.push(Action::CopyViewState);
            }
            if i.consume_key(ctrl_shift, Key::F) {
                actions.push(Action::Fullscreen);
            }
            if i.consume_key(ctrl, Key::O) {
                actions.push(Action::Open);
            }
            if i.consume_key(ctrl, Key::Q) {
                actions.push(Action::Quit);
            }
            if i.consume_key(ctrl, Key::W) {
                actions.push(Action::Close);
            }
            if i.consume_key(ctrl, Key::M) {
                actions.push(Action::ToggleMenuBar);
            }
            if i.consume_key(ctrl, Key::Plus) || i.consume_key(ctrl, Key::Equals) {
                actions.push(Action::ZoomIn);
            }
            if i.consume_key(ctrl, Key::Minus) {
                actions.push(Action::ZoomOut);
            }
            if i.consume_key(ctrl, Key::Num0) {
                actions.push(Action::ActualSize);
            }
            for (key, action) in [
                (Key::F1, Action::Shortcuts),
                (Key::F5, Action::Reload),
                (Key::F9, Action::TogglePanels),
                (Key::F11, Action::Fullscreen),
            ] {
                if i.consume_key(Modifiers::NONE, key) {
                    actions.push(action);
                }
            }
            if i.consume_key(Modifiers::ALT, Key::ArrowLeft) {
                actions.push(Action::Step(-1));
            }
            if i.consume_key(Modifiers::ALT, Key::ArrowRight) {
                actions.push(Action::Step(1));
            }
            if i.consume_key(Modifiers::NONE, Key::BrowserBack) {
                actions.push(Action::Step(-1));
            }
            // The mouse's back and forward buttons, wherever the pointer
            // is: they never drag or click anything.
            if i.pointer.button_pressed(PointerButton::Extra1) {
                actions.push(Action::Back);
            }
            if i.pointer.button_pressed(PointerButton::Extra2) {
                actions.push(Action::Step(1));
            }
            // The rest are plain keys, which a focused control (a combo box,
            // slider or list in the panels) uses itself: they apply only
            // while the view has the focus, or nothing has.
            if !view_keys || typing {
                return;
            }
            if i.consume_key(Modifiers::NONE, Key::PageUp) {
                actions.push(Action::Step(-1));
            }
            if i.consume_key(Modifiers::NONE, Key::PageDown) {
                actions.push(Action::Step(1));
            }
            // Views with Ctrl look from the opposite side, as Blender's
            // keypad does.
            for (key, yaw, pitch) in [(Key::Num1, 0.0, 0.0), (Key::Num3, 90.0, 0.0), (Key::Num7, 0.0, 90.0)] {
                if i.consume_key(ctrl, key) {
                    actions.push(Action::View(yaw + 180.0, if pitch > 0.0 { -pitch } else { 0.0 }));
                }
                if i.consume_key(Modifiers::NONE, key) {
                    actions.push(Action::View(yaw, pitch));
                }
            }
            let plain = [
                (Key::Space, Action::PlayPause),
                (Key::Tab, Action::ToggleCamera),
                (Key::F, Action::Frame),
                (Key::Home, Action::Home),
                (Key::Num5, Action::ToggleOrtho),
                (Key::G, Action::ToggleGrid),
                (Key::OpenBracket, Action::StepClip(-1)),
                (Key::CloseBracket, Action::StepClip(1)),
                (Key::Comma, Action::StepFrame(-1)),
                (Key::Period, Action::StepFrame(1)),
                (Key::Backspace, Action::BackToGallery),
                (Key::ArrowLeft, Action::Arrow(-1)),
                (Key::ArrowRight, Action::Arrow(1)),
                (Key::ArrowUp, Action::StepSlice(-1)),
                (Key::ArrowDown, Action::StepSlice(1)),
            ];
            for (key, action) in plain {
                if i.consume_key(Modifiers::NONE, key) {
                    actions.push(action);
                }
            }
            if i.consume_key(Modifiers::NONE, Key::Escape) && !self.control_focused {
                actions.push(Action::Escape);
            }
            if i.consume_key(Modifiers::NONE, Key::F6) {
                actions.push(Action::FocusPanels);
            }
        });
        // Keys the view used must not also move keyboard focus, which egui
        // does with Tab and the arrows at the end of the frame.
        if view_keys && actions.iter().any(|a| matches!(a, Action::ToggleCamera | Action::Arrow(_))) {
            ctx.memory_mut(|m| m.move_focus(egui::FocusDirection::None));
        }
        self.actions.extend(actions);
    }

    fn gallery_picture_open(&self) -> bool {
        matches!(&self.doc, Some(Document { content: Content::Gallery(g), .. }) if g.open.is_some())
    }

    /// Escape leaves one thing at a time: captured mouse, panorama, gallery
    /// picture, full screen.  With "Esc Closes the Window" on, it then
    /// closes an open dialog, and with nothing left to leave the window, as
    /// Gwenview does.
    fn escape(&mut self, ctx: &egui::Context) {
        if self.pointer_locked {
            return self.release_pointer(ctx);
        }
        if let Some(t) = self.doc.as_mut().and_then(|d| d.texture_mut())
            && t.inside.is_some()
        {
            t.inside = None;
            return;
        }
        if self.gallery_picture_open() {
            self.actions.push(Action::BackToGallery);
            return;
        }
        if ctx.input(|i| i.viewport().fullscreen.unwrap_or(false)) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(false));
            return;
        }
        if ctx.any_popup_open() {
            return;
        }
        // The settings page closes with Esc, as Qt's dialogs do.
        if self.file_types.take().is_some() {
            return;
        }
        if !self.saved.esc_closes {
            return;
        }
        let dialogs = [&mut self.ui.shortcuts, &mut self.ui.about, &mut self.ui.shot.open];
        if dialogs.iter().any(|d| **d) {
            dialogs.into_iter().for_each(|d| *d = false);
        } else {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }

    //endregion

    /*┌─────────────────╮
    | The view        |
    └─────────────────╯*/
    //region The view

    /// The central area: the 3D view, texture or gallery, with loading
    /// progress and messages over it.  Returns whether it needs another
    /// frame soon (something is moving).
    pub(crate) fn view_ui(&mut self, ui: &mut egui::Ui, dt: f32) -> bool {
        let rect = ui.available_rect_before_wrap();
        self.view_rect = rect;
        let ctx = ui.ctx().clone();
        let ppp = ctx.pixels_per_point();
        self.view_size = ((rect.width() * ppp).round().max(1.0) as u32, (rect.height() * ppp).round().max(1.0) as u32);
        let grid = matches!(&self.doc, Some(Document { content: Content::Gallery(g), .. }) if g.open.is_none());
        let palette = matches!(&self.doc, Some(Document { content: Content::Palette(_), .. }))
            || matches!(&self.doc, Some(Document { content: Content::Volume(v), .. }) if !v.has_picture());
        let sense = if grid || palette || self.doc.is_none() {
            Sense::focusable_noninteractive()
        } else {
            Sense::click_and_drag()
        };
        let response = ui.interact(rect, self.view_id, sense);
        let label = self.view_label();
        response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Other, true, label.clone()));
        if response.clicked() || response.drag_started() || response.double_clicked() {
            response.request_focus();
        }
        if response.has_focus() {
            ctx.memory_mut(|m| {
                m.set_focus_lock_filter(
                    self.view_id,
                    egui::EventFilter { tab: true, horizontal_arrows: true, vertical_arrows: true, escape: true },
                )
            });
        } else if ctx.memory(|m| m.focused().is_none()) && !ctx.any_popup_open() {
            ctx.memory_mut(|m| m.request_focus(self.view_id));
        }

        let mut moving = false;
        self.hover = None;
        // The labels over the view move below the bar about the file.
        let notice = self.file_notice.clone().filter(|_| self.doc.is_some());
        let top = crate::watch::bar_height(ui, notice.as_ref());
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 0.0, ui.visuals().extreme_bg_color);
        if grid {
            let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect));
            if let Some(Document { content: Content::Gallery(g), .. }) = &mut self.doc
                && let Some(i) = panels::gallery_grid(&mut child, g)
            {
                match g.open_item(i, &mut self.gfx) {
                    // The clicked cell had focus and is gone now; the view
                    // takes it, so the arrow keys step through pictures.
                    Ok(()) => ctx.memory_mut(|m| m.request_focus(self.view_id)),
                    Err(e) => self.warn(e),
                }
            }
        } else if let Some(Document { content: Content::Lut(lut), .. }) = &mut self.doc {
            crate::lut_view::view(ui, rect, &response, lut);
            self.view_overlays(ui, rect, top);
        } else if let Some(Document { content: Content::Palette(p), .. }) = &mut self.doc {
            let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect));
            crate::palette_view::view(&mut child, p);
        } else if let Some(Document { content: Content::Volume(v), .. }) = &self.doc
            && !v.has_picture()
        {
            // An array with no picture: its statistics, and a plot.
            let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect));
            crate::volume_view::stats_view(&mut child, v);
        } else if self.doc.is_some() {
            let id = self.gfx.target(self.view_size);
            moving = self.drive_view(ui, &response, rect, dt);
            painter.image(
                id,
                rect,
                egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                egui::Color32::WHITE,
            );
            self.view_overlays(ui, rect, top);
        } else if let Some(state) = self.contents.as_ref().filter(|s| s.has_card()) {
            // An archive entry that draws no picture: its facts, or its text.
            contents_view::card_view(ui, rect, state);
        } else if self.failed.is_none() && self.loader.busy().is_none() {
            empty_state(ui, rect, &mut self.actions);
        }
        // The file on disk changed, was reloaded, or couldn't be.
        if let Some(notice) = &notice {
            let action = match crate::watch::notice_ui(ui, rect, notice) {
                Some(BarAction::Reload) => Some(Action::Reload),
                Some(BarAction::AlwaysReload) => Some(Action::SetAlwaysReload(true)),
                Some(BarAction::Dismiss) => Some(Action::DismissNotice),
                None => None,
            };
            self.actions.extend(action);
        }
        // Loading progress ticks by itself (the spinner asks for 30 frames
        // a second) rather than counting as movement.
        self.status_overlays(ui, rect);
        moving
    }

    /// Input, animation and drawing for a scene or texture.
    fn drive_view(&mut self, ui: &egui::Ui, response: &egui::Response, rect: egui::Rect, dt: f32) -> bool {
        let ctx = ui.ctx().clone();
        let ppp = ctx.pixels_per_point();
        let size = self.view_size;
        let aspect = self.aspect();
        let view_keys = ctx.memory(|m| m.has_focus(self.view_id)) || self.pointer_locked;
        let input = Input::read(&ctx, response, view_keys);
        let Some(doc) = &mut self.doc else { return false };
        if let Some(view) = doc.scene_mut() {
            // 1. Cameras.
            let mut moving = false;
            match view.cameras.mode {
                Mode::Orbit => {
                    if let Some((button, delta)) = input.drag {
                        if button == PointerButton::Primary && !input.shift {
                            view.cameras.orbit_drag(delta);
                        } else {
                            view.cameras.pan(delta, rect.height());
                        }
                    }
                    if response.hovered() && input.scroll != 0.0 {
                        view.cameras.dolly(input.scroll);
                    }
                    if response.hovered() && input.zoom != 1.0 {
                        view.cameras.dolly(input.zoom.ln() / 0.85f32.recip().ln());
                    }
                    if response.double_clicked()
                        && let Some(pos) = input.pointer
                    {
                        let (o, d) = ray(&view.cameras.view(aspect), to_ndc(rect, pos));
                        match view.pick(o, d) {
                            Some(p) => view.cameras.pivot(p),
                            None => {
                                let b = view.frame_bounds();
                                view.cameras.frame(&b, aspect);
                            }
                        }
                    }
                }
                Mode::Fly => {
                    if response.clicked() && !self.pointer_locked {
                        // Capture the mouse for looking around.
                        ctx.send_viewport_cmd(egui::ViewportCommand::CursorGrab(egui::CursorGrab::Locked));
                        ctx.send_viewport_cmd(egui::ViewportCommand::CursorVisible(false));
                        self.pointer_locked = true;
                    }
                    if self.pointer_locked && input.motion != Vec2::ZERO {
                        view.cameras.look(input.motion);
                    } else if let Some((_, delta)) = input.drag {
                        view.cameras.look(delta * ppp);
                    }
                    if (response.hovered() || self.pointer_locked) && input.scroll != 0.0 {
                        view.cameras.change_speed(input.scroll);
                    }
                    if input.fly != Vec3::ZERO {
                        view.cameras.fly_move(input.fly, dt, if input.shift { 4.0 } else { 1.0 });
                        moving = true;
                    }
                }
            }
            moving |= view.cameras.advance(dt);

            // 2. What the pointer is over.
            let v = view.cameras.view(aspect);
            let mut highlight = None;
            if let (Some(pos), true) = (input.pointer, response.hovered()) {
                if self.look.overlays.skeleton
                    && let Some(j) = view.joint_near(&v, rect, pos, 10.0)
                {
                    highlight = Some(j);
                    self.hover = Some(format!("Joint: {}", view.scene.nodes[j].name));
                } else if self.look.overlays.markers
                    && let Some(m) = view.marker_near(&v, rect, pos, 10.0)
                {
                    let mk = &view.scene.markers[m];
                    self.hover = Some(format!(
                        "{} at {:.2}, {:.2}, {:.2}",
                        mk.label, mk.position.x, mk.position.y, mk.position.z
                    ));
                }
            }

            // A GPS track's point under the pointer, which its profile marks.
            if let Some(text) =
                crate::paths_view::hover(&ctx, view, &v, rect, input.pointer.filter(|_| response.hovered()))
            {
                self.hover = Some(text);
            }

            // 3. Animation and the pose, then the frame.
            moving |= view.update(&mut self.gfx, dt, self.look.animate_effects);
            self.gfx.set_environment(&self.look.wanted_environment());
            let mut settings = self.look.settings(&view.scene, view.effects_time);
            settings.overlays.highlight_joint = highlight;
            let key = FrameKey::Scene {
                scene: view.id,
                pose: view.pose_version,
                textures: view.textures_arrived,
                view: [v.view, v.proj],
                settings,
                size,
                env: self.gfx.env_version,
            };
            self.gfx.draw_scene(key, &view.gpu, &v, &settings);
            return moving || view.cameras.is_easing();
        }
        // PDF pages: pan and zoom, drawn again at each zoom.
        if let Content::Pages(p) = &mut doc.content {
            let (drag, pointer, scroll, zoom) = (input.drag, input.pointer, input.scroll, input.zoom);
            let gfx = &mut self.gfx;
            let hover = &mut self.hover;
            return crate::pages_view::drive(p, gfx, drag, pointer, scroll, zoom, response, rect, ppp, size, hover);
        }
        // Flat drawings: pan and zoom, drawn again at each zoom.
        if let Content::Cad(c) = &mut doc.content {
            let (drag, pointer, scroll, zoom) = (input.drag, input.pointer, input.scroll, input.zoom);
            return crate::cad_view::drive(
                c,
                &mut self.gfx,
                drag,
                pointer,
                scroll,
                zoom,
                response,
                rect,
                ppp,
                size,
                &mut self.hover,
            );
        }
        // Volumes: the wheel turns through slices, the right button moves the window.
        if let Content::Volume(v) = &mut doc.content {
            let ctrl = ctx.input(|i| i.modifiers.command);
            let gfx = &mut self.gfx;
            let (drag, pointer, scroll, zoom) = (input.drag, input.pointer, input.scroll, input.zoom);
            return crate::volume_view::drive(
                v,
                gfx,
                drag,
                pointer,
                scroll,
                zoom,
                ctrl,
                response,
                rect,
                ppp,
                size,
                dt,
                &mut self.hover,
            );
        }
        // DICOM images: dragging with the right button moves the window.
        if let Content::Dicom(d) = &mut doc.content {
            let ctrl = ctx.input(|i| i.modifiers.command);
            let gfx = &mut self.gfx;
            let (drag, pointer, scroll, zoom) = (input.drag, input.pointer, input.scroll, input.zoom);
            return crate::dicom_view::drive(
                d,
                gfx,
                drag,
                pointer,
                scroll,
                zoom,
                ctrl,
                response,
                rect,
                ppp,
                size,
                dt,
                &mut self.hover,
            );
        }
        // Sprites and maps play on, then show like a texture.
        let playing = doc.canvas_mut().is_some_and(|c| c.update(&mut self.gfx, dt));
        if let Some(t) = doc.texture_mut() {
            return drive_texture(t, &mut self.gfx, &input, response, rect, ppp, size, &mut self.hover) || playing;
        }
        playing
    }

    /// Small text over the view: the mode, and what is under the pointer.
    /// `top` is the height of a bar along the view's top edge, to keep
    /// clear of.
    fn view_overlays(&mut self, ui: &egui::Ui, rect: egui::Rect, top: f32) {
        // The Contents panel's hovered item, outlined.
        contents_view::overlay(self, &ui.painter_at(rect), rect);
        let Some(doc) = &self.doc else { return };
        if let Some(view) = doc.scene() {
            crate::paths_view::overlay(&ui.painter_at(rect), rect, view, self.aspect());
        }
        if let Content::Volume(v) = &doc.content {
            crate::volume_view::overlay(&ui.painter_at(rect), rect, v);
        }
        if let Content::Pages(p) = &doc.content {
            crate::pages_view::overlay(&ui.painter_at(rect), rect, p);
        }
        let mut parts = vec![doc.mode_name().to_string()];
        if let Content::Pages(p) = &doc.content {
            parts.extend(p.overlay_parts());
        }
        if let Some(view) = doc.scene() {
            match view.cameras.mode {
                Mode::Orbit => parts.push(if view.cameras.orbit.ortho { "Orthographic" } else { "Perspective" }.into()),
                Mode::Fly => {
                    parts.push(format!("{:.1} m/s", view.cameras.fly_speed));
                    if !self.pointer_locked {
                        parts.push("click to look around, Esc releases".into());
                    }
                }
            }
        } else if let Some(t) = doc.texture()
            && t.inside.is_some()
        {
            parts.push("drag to look around, scroll to zoom, Esc leaves".into());
        } else if let Some(t) = doc.texture() {
            if matches!(doc.content, Content::Gallery(_)) {
                parts.push(t.name.clone());
            }
            parts.push(format!("{:.0} %", t.params.zoom * 100.0 / ui.ctx().pixels_per_point()));
            if let Content::Dicom(d) = &doc.content {
                parts.extend(d.overlay_parts());
            }
            if let Content::Volume(v) = &doc.content {
                parts.extend(v.overlay_parts());
            }
        }
        let text = parts.join("  ·  ");
        let font = egui::TextStyle::Small.resolve(ui.style());
        let painter = ui.painter_at(rect);
        let pos = rect.left_top() + egui::vec2(8.0, 6.0 + top);
        let galley = painter.layout_no_wrap(text, font.clone(), egui::Color32::from_gray(235));
        let bg = egui::Rect::from_min_size(pos - egui::vec2(4.0, 2.0), galley.size() + egui::vec2(8.0, 4.0));
        painter.rect_filled(bg, 3.0, egui::Color32::from_black_alpha(110));
        painter.galley(pos, galley, egui::Color32::from_gray(235));
        if let (Some(text), Some(pointer)) = (&self.hover, ui.ctx().pointer_hover_pos()) {
            let galley = painter.layout_no_wrap(text.clone(), font, egui::Color32::WHITE);
            let at = pointer + egui::vec2(14.0, 10.0);
            let bg = egui::Rect::from_min_size(at - egui::vec2(4.0, 2.0), galley.size() + egui::vec2(8.0, 4.0));
            painter.rect_filled(bg, 3.0, egui::Color32::from_black_alpha(170));
            painter.galley(at, galley, egui::Color32::WHITE);
        }
    }

    /// Loading progress, load errors and dropped-file hints over the view.
    fn status_overlays(&mut self, ui: &mut egui::Ui, rect: egui::Rect) {
        let hovering_files = ui.ctx().input(|i| !i.raw.hovered_files.is_empty());
        let reloading = self.reloading.is_some() && self.doc.is_some();
        if let (Some((path, elapsed)), true) = (self.loader.busy(), reloading) {
            // A reload keeps the old content on screen; a small note says
            // it is coming, once it takes long enough to notice.
            if elapsed > Duration::from_millis(300) {
                let name = short_name(path);
                crate::watch::progress_note(ui, rect, &format!("Reloading {name}…  {:.1} s", elapsed.as_secs_f32()));
            }
        } else if let Some((path, elapsed)) = self.loader.busy() {
            let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
            let stage = self.loader.stage();
            let text = format!("Loading {name} ({})\n{stage}…  {:.1} s", bytes(size), elapsed.as_secs_f32());
            card(ui, rect, |ui| {
                ui.horizontal(|ui| {
                    spinner(ui, 22.0);
                    ui.label(text);
                });
            });
        } else if let Some((path, error)) = &self.failed {
            let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let (name, error) = (name, error.clone());
            card(ui, rect, |ui| {
                ui.label(egui::RichText::new(format!("Couldn't open {name}")).text_style(heading2()));
                ui.label(error);
                ui.label("Page Up and Page Down step to the other files in the folder.");
            });
        }
        if hovering_files {
            let painter = ui.painter_at(rect);
            painter.rect_filled(rect, 0.0, egui::Color32::from_black_alpha(90));
            painter.text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                "Drop to open",
                egui::TextStyle::Heading.resolve(ui.style()),
                egui::Color32::WHITE,
            );
        }
    }

    fn view_label(&self) -> String {
        match &self.doc {
            Some(doc) => format!(
                "{} view of {}.  Tab switches camera, F frames, Space plays, F6 moves to the panels.",
                doc.mode_name(),
                doc.name()
            ),
            None => "Empty view.  Press Ctrl+O to open a file.".into(),
        }
    }

    //endregion

    /*┌─────────────────╮
    | Bars and panels |
    └─────────────────╯*/
    //region Bars and panels

    fn status_bar(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        egui::Panel::bottom("status").frame(widgets::status_bar_frame(&ctx)).show_separator_line(false).show(
            ui,
            |ui| {
                widgets::compact(ui);
                ui.horizontal(|ui| {
                    if let Some((text, when, error)) = &self.notice
                        && when.elapsed() < Duration::from_secs(6)
                    {
                        let color = if *error { ui.visuals().warn_fg_color } else { ui.visuals().text_color() };
                        ui.label(egui::RichText::new(text).color(color));
                        ui.separator();
                        ctx.request_repaint_after(Duration::from_secs(6).saturating_sub(when.elapsed()));
                    }
                    if let Some(doc) = &self.doc {
                        ui.label(doc.name());
                        // A DICOM image counts its place in its series, in
                        // slice order, rather than in the folder's file names.
                        let position = match &doc.content {
                            Content::Dicom(d) => d.slice(),
                            _ => self.position,
                        };
                        if let Some((i, n)) = position {
                            ui.separator();
                            ui.label(format!("{i} of {n}"));
                        }
                        ui.separator();
                        ui.label(panels::summary(doc));
                        ui.separator();
                        ui.label(format!(
                            "loaded in {:.0} ms",
                            (doc.load_time + doc.upload_time).as_secs_f64() * 1000.0
                        ));
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let info = &self.gfx.gpu.info;
                        let api = slerp_render::backend_name(info.backend);
                        ui.label(egui::RichText::new(format!("{api} · {}", info.name)).small().weak());
                        if let (Some(hover), true) =
                            (&self.hover, self.doc.as_ref().is_some_and(|d| d.texture().is_some()))
                        {
                            ui.separator();
                            ui.label(egui::RichText::new(hover).monospace());
                        }
                    });
                });
            },
        );
    }

    fn tool_bar(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        let (is_scene, is_texture, flying, playing) = match &self.doc {
            Some(doc) => (
                doc.scene().is_some(),
                doc.texture().is_some(),
                doc.scene().is_some_and(|s| s.flying()),
                doc.scene().is_some_and(|s| s.player.playing && s.player.clip.is_some()),
            ),
            None => (false, false, false, false),
        };
        let sprite = match &self.doc {
            Some(Document { content: Content::Canvas(c), .. }) if c.canvas.frames().len() > 1 => Some(c.player.playing),
            _ => None,
        };
        let actions = &mut self.actions;
        let grid = self.look.overlays.grid;
        let panels_shown = self.ui.show_panels;
        let full = ctx.input(|i| i.viewport().fullscreen.unwrap_or(false));
        egui::Panel::top("toolbar").frame(widgets::tool_bar_frame(&ctx)).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                let mut tool =
                    |ui: &mut egui::Ui, icon: &str, fallback: &str, tip: &str, checked: bool, action: Action| {
                        if widgets::tool_button(ui, icon, fallback, checked).on_hover_text(tip).clicked() {
                            actions.push(action);
                        }
                    };
                tool(ui, "document-open", "Open", "Open a file (Ctrl+O)", false, Action::Open);
                tool(ui, "camera-photo", "Shot", "Save a screenshot (Ctrl+Shift+S)", false, Action::Screenshot);
                tool(ui, "edit-copy", "State", "Copy the view state (Ctrl+Shift+C)", false, Action::CopyViewState);
                widgets::tool_separator(ui);
                tool(ui, "go-previous", "<", "Previous file in the folder (Page Up)", false, Action::Step(-1));
                tool(ui, "go-next", ">", "Next file in the folder (Page Down)", false, Action::Step(1));
                widgets::tool_separator(ui);
                tool(ui, "zoom-fit-best", "Fit", "Frame the model or fit the picture (F)", false, Action::Frame);
                tool(ui, "zoom-in", "+", "Zoom in (Ctrl+Plus)", false, Action::ZoomIn);
                tool(ui, "zoom-out", "-", "Zoom out (Ctrl+Minus)", false, Action::ZoomOut);
                if is_texture {
                    tool(ui, "zoom-original", "1:1", "Actual size (Ctrl+0)", false, Action::ActualSize);
                }
                if let Some(playing) = sprite {
                    widgets::tool_separator(ui);
                    let (icon, tip) = if playing {
                        ("media-playback-pause", "Pause (Space)")
                    } else {
                        ("media-playback-start", "Play (Space)")
                    };
                    tool(ui, icon, "Play", tip, false, Action::PlayPause);
                }
                if is_scene {
                    widgets::tool_separator(ui);
                    tool(
                        ui,
                        "object-rotate-right",
                        "Orbit",
                        "Switch between orbit and first person (Tab)",
                        flying,
                        Action::ToggleCamera,
                    );
                    tool(ui, "view-grid", "Grid", "Ground grid (G)", grid, Action::ToggleGrid);
                    let (icon, tip) = if playing {
                        ("media-playback-pause", "Pause (Space)")
                    } else {
                        ("media-playback-start", "Play (Space)")
                    };
                    tool(ui, icon, "Play", tip, false, Action::PlayPause);
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let mut tool =
                        |ui: &mut egui::Ui, icon: &str, fallback: &str, tip: &str, checked: bool, action: Action| {
                            if widgets::tool_button(ui, icon, fallback, checked).on_hover_text(tip).clicked() {
                                actions.push(action);
                            }
                        };
                    let (icon, tip) = if full {
                        ("view-restore", "Leave full screen (F11)")
                    } else {
                        ("view-fullscreen", "Full screen (F11)")
                    };
                    tool(ui, icon, "Full", tip, false, Action::Fullscreen);
                    tool(
                        ui,
                        "document-properties",
                        "Panels",
                        "Show or hide the side panels (F9)",
                        panels_shown,
                        Action::TogglePanels,
                    );
                });
            });
        });
    }

    fn menu_bar(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        let has_doc = self.doc.is_some();
        let can_save_glb = save_glb::can_save(self.doc.as_ref());
        let libraries = self.libraries();
        let recent_env = self.saved.recent_environments.clone();
        let current_env = self.look.environment.clone();
        let actions = &mut self.actions;
        let look = &mut self.look;
        let show_panels = self.ui.show_panels;
        let (always_reload, esc_closes) = (self.saved.always_reload, self.saved.esc_closes);
        let keep_alive = self.config.viewer_keep_alive_seconds;
        let with_helper = self.config.viewer_with_helper;
        let scene = self.doc.as_mut().and_then(|d| d.scene_mut());
        egui::Panel::top("menu").frame(widgets::menu_bar_frame(&ctx)).show_separator_line(false).show(ui, |ui| {
            widgets::menu_bar(&ctx).ui(ui, |ui| {
                let item = |ui: &mut egui::Ui, text: &str, keys: &str, enabled: bool| -> bool {
                    ui.add_enabled(enabled, egui::Button::new(text).shortcut_text(keys)).clicked()
                };
                widgets::menu_button(ui, "File", |ui| {
                    if item(ui, "Open…", "Ctrl+O", true) {
                        actions.push(Action::Open);
                    }
                    if item(ui, "Reload", "F5", has_doc) {
                        actions.push(Action::Reload);
                    }
                    if item(ui, "Previous File", "Page Up", has_doc) {
                        actions.push(Action::Step(-1));
                    }
                    if item(ui, "Next File", "Page Down", has_doc) {
                        actions.push(Action::Step(1));
                    }
                    ui.separator();
                    if item(ui, "Save Screenshot…", "Ctrl+Shift+S", has_doc) {
                        actions.push(Action::Screenshot);
                    }
                    if item(ui, "Save as GLB…", "", can_save_glb) {
                        actions.push(Action::SaveGlb);
                    }
                    if item(ui, "Copy View State", "Ctrl+Shift+C", has_doc) {
                        actions.push(Action::CopyViewState);
                    }
                    ui.separator();
                    if item(ui, "Close", "Ctrl+W", has_doc) {
                        actions.push(Action::Close);
                    }
                    if item(ui, "Quit", "Ctrl+Q", true) {
                        actions.push(Action::Quit);
                    }
                });
                widgets::menu_button(ui, "View", |ui| {
                    if item(ui, "Frame", "F", has_doc) {
                        actions.push(Action::Frame);
                    }
                    if item(ui, "Reset View", "Home", has_doc) {
                        actions.push(Action::Home);
                    }
                    if item(ui, "Switch Camera", "Tab", has_doc) {
                        actions.push(Action::ToggleCamera);
                    }
                    for (text, keys, yaw, pitch) in
                        [("Front", "1", 0.0, 0.0), ("Right Side", "3", 90.0, 0.0), ("Top", "7", 0.0, 90.0)]
                    {
                        if item(ui, text, keys, has_doc) {
                            actions.push(Action::View(yaw, pitch));
                        }
                    }
                    if item(ui, "Orthographic", "5", has_doc) {
                        actions.push(Action::ToggleOrtho);
                    }
                    ui.separator();
                    ui.add(widgets::CheckBox::new(&mut look.overlays.grid, "Ground Grid"));
                    ui.add(widgets::CheckBox::new(&mut look.overlays.wireframe, "Wireframe"));
                    ui.add(widgets::CheckBox::new(&mut look.overlays.skeleton, "Skeleton"));
                    ui.add(widgets::CheckBox::new(&mut look.overlays.bounds, "Bounds"));
                    ui.separator();
                    let mut always = always_reload;
                    if ui
                        .add(widgets::CheckBox::new(&mut always, "Always Reload"))
                        .on_hover_text("Reload the file whenever it changes on disk, without asking")
                        .clicked()
                    {
                        actions.push(Action::SetAlwaysReload(always));
                    }
                    let mut esc = esc_closes;
                    if ui
                        .add(widgets::CheckBox::new(&mut esc, "Esc Closes the Window"))
                        .on_hover_text("When Esc has nothing else to leave: full screen, a panorama, a picture")
                        .clicked()
                    {
                        actions.push(Action::SetEscCloses(esc));
                    }
                    ui.separator();
                    let mut panels = show_panels;
                    if ui.add(widgets::CheckBox::new(&mut panels, "Side Panels (F9)")).clicked() {
                        actions.push(Action::TogglePanels);
                    }
                    if item(ui, "Reset Panel Layout", "", true) {
                        actions.push(Action::ResetPanels);
                    }
                    if item(ui, "Hide Menu Bar", "Ctrl+M", true) {
                        actions.push(Action::ToggleMenuBar);
                    }
                    if item(ui, "Full Screen", "F11", true) {
                        actions.push(Action::Fullscreen);
                    }
                });
                widgets::menu_button(ui, "Animation", |ui| {
                    let has_clips = scene.as_ref().is_some_and(|s| !s.scene.animations.is_empty());
                    if item(ui, "Play or Pause", "Space", has_clips) {
                        actions.push(Action::PlayPause);
                    }
                    if item(ui, "Previous Clip", "[", has_clips) {
                        actions.push(Action::StepClip(-1));
                    }
                    if item(ui, "Next Clip", "]", has_clips) {
                        actions.push(Action::StepClip(1));
                    }
                    if item(ui, "Step Back", ",", has_clips) {
                        actions.push(Action::StepFrame(-1));
                    }
                    if item(ui, "Step Forward", ".", has_clips) {
                        actions.push(Action::StepFrame(1));
                    }
                    ui.separator();
                    let has_scene = scene.is_some();
                    if item(ui, "Add Animations from File…", "", has_scene) {
                        actions.push(Action::ChooseLibrary);
                    }
                    if !libraries.is_empty() {
                        ui.add_enabled_ui(has_scene, |ui| {
                            ui.menu_button("Add Animations From", |ui| {
                                for lib in &libraries {
                                    if ui.button(short_name(lib)).on_hover_text(lib.display().to_string()).clicked() {
                                        actions.push(Action::AddLibrary(lib.clone()));
                                    }
                                }
                            });
                        });
                    }
                });
                widgets::menu_button(ui, "Lighting", |ui| {
                    if ui.add(egui::Button::new("Studio").selected(current_env.is_none())).clicked() {
                        actions.push(Action::SetEnvironment(None));
                    }
                    for p in &recent_env {
                        let selected = current_env.as_ref() == Some(p);
                        if ui
                            .add(egui::Button::new(short_name(p)).selected(selected))
                            .on_hover_text(p.display().to_string())
                            .clicked()
                        {
                            actions.push(Action::SetEnvironment(Some(p.clone())));
                        }
                    }
                    ui.separator();
                    if item(ui, "Choose HDR File…", "", true) {
                        actions.push(Action::ChooseEnvironment);
                    }
                });
                widgets::menu_button(ui, "Settings", |ui| {
                    if item(ui, "File Types…", "", true) {
                        actions.push(Action::FileTypes);
                    }
                    ui.separator();
                    keep_alive_menu(ui, keep_alive, actions);
                    let mut with_helper = with_helper;
                    if cfg!(target_os = "linux")
                        && ui
                            .add(widgets::CheckBox::new(&mut with_helper, "Get Ready While Browsing"))
                            .on_hover_text(
                                "While the file manager is showing Slerp's thumbnails, keep Slerp loaded in the \
                                 background, so the first file you open from it opens at once",
                            )
                            .clicked()
                    {
                        actions.push(Action::SetWithHelper(with_helper));
                    }
                });
                widgets::menu_button(ui, "Help", |ui| {
                    if item(ui, "Keyboard Shortcuts", "F1", true) {
                        actions.push(Action::Shortcuts);
                    }
                    if item(ui, "About Slerp", "", true) {
                        actions.push(Action::About);
                    }
                });
            });
        });
    }

    /// Animation libraries to offer: the config's, then recent ones.
    pub fn libraries(&self) -> Vec<PathBuf> {
        let mut out: Vec<PathBuf> = self.config.animation_libraries.clone();
        for p in &self.saved.recent_libraries {
            if !out.contains(p) {
                out.push(p.clone());
            }
        }
        out
    }

    fn dialogs(&mut self, ctx: &egui::Context) {
        if let Some(page) = &mut self.file_types
            && !page.show(ctx)
        {
            self.file_types = None;
        }
        if self.ui.shortcuts {
            let mut open = true;
            egui::Window::new("Keyboard Shortcuts").open(&mut open).collapsible(false).default_width(460.0).show(
                ctx,
                |ui| {
                    egui::ScrollArea::vertical().max_height(520.0).show(ui, |ui| {
                        egui::Grid::new("shortcuts").num_columns(2).striped(true).spacing([16.0, 4.0]).show(ui, |ui| {
                            for (keys, what) in SHORTCUTS {
                                ui.label(egui::RichText::new(*keys).monospace());
                                ui.label(*what);
                                ui.end_row();
                            }
                        });
                    });
                },
            );
            self.ui.shortcuts = open;
        }
        if self.ui.about {
            let mut open = true;
            egui::Window::new("About Slerp").open(&mut open).collapsible(false).resizable(false).show(ctx, |ui| {
                ui.label(egui::RichText::new("Slerp").text_style(heading2()));
                ui.label(format!("Version {}", env!("CARGO_PKG_VERSION")));
                ui.label("Thumbnails and a viewer for game and 3D asset files.");
                ui.label("MIT or Apache-2.0.");
                let info = &self.gfx.gpu.info;
                ui.label(format!(
                    "Drawing with {} ({}), chosen as {}.",
                    info.name, info.driver, self.gfx.gpu.chosen_by
                ));
            });
            self.ui.about = open;
        }
        if self.ui.shot.open {
            let mut open = true;
            let mut save = None;
            let view = self.view_size;
            let s = &mut self.ui.shot;
            egui::Window::new("Save Screenshot").open(&mut open).collapsible(false).resizable(false).show(ctx, |ui| {
                egui::Grid::new("shot").num_columns(2).spacing([8.0, 6.0]).show(ui, |ui| {
                    ui.label("Width:");
                    ui.add(egui::DragValue::new(&mut s.width).range(16..=8192).suffix(" px"));
                    ui.end_row();
                    ui.label("Height:");
                    ui.add(egui::DragValue::new(&mut s.height).range(16..=8192).suffix(" px"));
                    ui.end_row();
                });
                ui.horizontal_wrapped(|ui| {
                    for (text, w, h) in [
                        ("View", view.0, view.1),
                        ("2× view", view.0 * 2, view.1 * 2),
                        ("1080p", 1920, 1080),
                        ("4K", 3840, 2160),
                        ("Square", 2048, 2048),
                    ] {
                        if ui.button(text).clicked() {
                            (s.width, s.height) = (w.min(8192), h.min(8192));
                        }
                    }
                });
                ui.add(widgets::CheckBox::new(&mut s.transparent, "Transparent background (3D)"));
                ui.add_space(4.0);
                if ui.button("Save…").clicked() {
                    save = Some(Shot { width: s.width, height: s.height, transparent: s.transparent });
                }
            });
            self.ui.shot.open = open && save.is_none();
            if let Some(shot) = save {
                self.pending_screenshot = Some(shot);
            }
        }
    }

    //endregion
}

/*┌─────────────────╮
| Frame loop      |
└─────────────────╯*/
//region Frame loop

impl Viewer {
    /// One frame of the window (`shell` calls it, as eframe would call
    /// `App::ui`).
    pub(crate) fn ui(&mut self, ui: &mut egui::Ui, frame: &mut crate::shell::Frame) {
        let frame_start = Instant::now();
        let ctx = ui.ctx().clone();
        if crate::env::is_set("SLERP_DEBUG_REPAINT") {
            let focus = ctx.memory(|m| m.focused());
            let keys: Vec<String> = ctx.input(|i| {
                i.raw
                    .events
                    .iter()
                    .filter_map(|e| match e {
                        egui::Event::Key { key, pressed: true, .. } => Some(format!("{key:?}")),
                        _ => None,
                    })
                    .collect()
            });
            eprintln!(
                "frame {}: focus {} keys {keys:?} causes {:?}",
                self.egui_frames,
                match focus {
                    Some(f) if f == self.view_id => "view".to_string(),
                    Some(f) => format!("{f:?}"),
                    None => "none".into(),
                },
                ctx.repaint_causes()
            );
        }
        if self.egui_frames == 0 {
            crate::sections::load(&ctx, self.saved.sections.clone());
        }

        self.egui_frames += 1;
        // Time since the last frame, but when the window was idle, one
        // frame's worth, so animation doesn't jump after a pause.
        let dt = match self.last_frame {
            Some(t) => t.elapsed().as_secs_f32().clamp(0.0, 0.1),
            None => 1.0 / 60.0,
        };

        // 1. Background work, files dropped in or sent by Finder, the
        //    keyboard.
        self.theme.poll(&ctx);
        self.poll_background(&ctx);
        contents_view::poll(self, &ctx);
        if let Some(path) = ctx.input(|i| i.raw.dropped_files.iter().find_map(|f| f.path.clone())) {
            // A picture dropped on a colour table is graded with it.
            let lut = matches!(&self.doc, Some(Document { content: Content::Lut(_), .. }));
            if lut && matches!(slerp_core::sniff_file(&path), Ok(Some(slerp_core::Kind::Texture(_)))) {
                self.actions.push(Action::UsePhoto(path));
            } else {
                self.open(path);
            }
        }
        #[cfg(target_os = "macos")]
        if let Some(path) = crate::macos::take_pending() {
            self.open(path);
        }
        if ctx.input(|i| i.viewport().focused == Some(false)) {
            self.release_pointer(&ctx);
        }
        // A text selection on a document page takes Ctrl+C and Esc
        // before the app's shortcuts and the Contents panel's copy.
        if let Some(Document { content: Content::Pages(p), .. }) = &mut self.doc {
            let view_keys = ctx.memory(|m| m.focused().is_none() || m.has_focus(self.view_id));
            crate::pages_view::keys(p, &ctx, view_keys);
        }
        self.shortcuts(&ctx);
        contents_view::shortcuts(self, &ctx);
        self.run_actions(&ctx, frame);

        // 3. Layout: bars, panels, then the view in what is left.
        let full = ctx.input(|i| i.viewport().fullscreen.unwrap_or(false));
        if !full && self.ui.show_menu {
            self.menu_bar(ui);
        }
        if !full {
            self.tool_bar(ui);
            self.status_bar(ui);
        }
        // A track's elevation profile, below the view.
        crate::paths_view::profile_panel(ui, self.doc.as_mut());
        let mut moving = false;
        egui::CentralPanel::no_frame().show(ui, |ui| {
            moving = if self.ui.show_panels { crate::dock::show(self, ui, dt) } else { self.view_ui(ui, dt) };
        });
        if self.ui.show_panels {
            contents_drag::run(self, &ctx, frame);
        }
        if !full && self.view_rect.is_positive() {
            let (s, v) = (ctx.content_rect(), self.view_rect);
            self.view_insets =
                Some([v.left() - s.left(), v.top() - s.top(), s.right() - v.right(), s.bottom() - v.bottom()]);
        }
        self.dialogs(&ctx);
        if let Some(shot) = self.pending_screenshot.take() {
            self.file_dialog(frame, Purpose::Screenshot(shot));
        }
        self.run_actions(&ctx, frame);
        if let Some(sections) = crate::sections::take_changes(&ctx) {
            self.saved.sections = sections;
            self.saved.save();
        }

        let view_id = self.view_id;
        self.control_focused = ctx.memory(|m| m.focused().is_some_and(|f| f != view_id));

        // 4. Testing hooks, and the next frame if anything moves.
        moving |= self.hooks(&ctx, frame_start);
        if self.timing && self.egui_frames == 1 {
            // The first frame the GPU paints; the window may have shown a
            // placeholder since "window shown".
            eprintln!("slerp view: first window frame at {:.0} ms", self.started.elapsed().as_secs_f64() * 1000.0);
        }
        let loading = self.loader.busy().is_some() || self.look.is_loading() || self.library.is_some();
        if moving {
            ctx.request_repaint();
            self.last_frame = Some(frame_start);
        } else {
            self.last_frame = None;
            if loading {
                // Progress text ticks; the loader also wakes us when done.
                ctx.request_repaint_after(Duration::from_millis(100));
            }
        }
    }

    /// Adds scripted input and the wheel and drag-out fixes to the
    /// window's input, before the frame runs.
    pub(crate) fn raw_input_hook(&mut self, ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        if self.hooks.first_frame
            && let Some(script) = &mut self.hooks.script
        {
            raw_input.events.extend(script.events(self.view_rect));
            if let Some(path) = script.take_shot() {
                self.hooks.shot = Some((path, None));
            }
            if script.is_done() {
                self.hooks.script = None;
            }
        }
        // The wheel over a combo box steps it rather than scrolling (after
        // the script, whose wheel turns count too).
        widgets::wheel_hook(ctx, raw_input);
        // After a drag out of the window starts, the window system owns the
        // pointer and egui never sees the button go up.
        raw_input.events.extend(crate::dragout::take_pointer_release(ctx));
    }
}

/// Remembers where the view was, for the next window's placeholder, and
/// reports frame counts on exit.
impl Drop for Viewer {
    fn drop(&mut self) {
        contents_view::shutdown();
        contents_drag::closed();
        if let Some(insets) = self.view_insets
            && self.saved.view_insets != Some(insets)
        {
            self.saved.view_insets = Some(insets);
            self.saved.save();
        }
        if self.timing {
            eprintln!("slerp view: {} window frames, {} view frames drawn", self.egui_frames, self.gfx.draws);
        }
    }
}

impl Viewer {
    fn run_actions(&mut self, ctx: &egui::Context, frame: &crate::shell::Frame) {
        while !self.actions.is_empty() {
            let actions = std::mem::take(&mut self.actions);
            for a in actions {
                self.perform(a, ctx, frame);
            }
        }
    }

    /// The `SLERP_SCREENSHOT` and `SLERP_BENCH` hooks, and the
    /// first-frame timing.  Returns whether they need more frames.
    fn hooks(&mut self, ctx: &egui::Context, frame_start: Instant) -> bool {
        // A card about an archive entry that draws nothing counts as shown.
        let card = self.doc.is_none() && self.contents.as_ref().is_some_and(|c| c.has_card());
        let shown = (self.doc.is_some() || card) && self.loader.busy().is_none() && !self.look.is_loading();
        // Textures that go up after the first frame (see
        // `SceneView::streaming`) count for screenshots and scripts.
        let uploading = self.doc.as_ref().and_then(|d| d.scene()).is_some_and(|s| s.textures_pending());
        let ready = shown && !uploading;
        if shown && !self.hooks.shown {
            self.hooks.shown = true;
            if self.timing {
                eprintln!(
                    "slerp view: first frame with the model at {:.0} ms (window frame {})",
                    self.started.elapsed().as_secs_f64() * 1000.0,
                    self.egui_frames
                );
            }
        }
        if ready && !self.hooks.first_frame {
            self.hooks.first_frame = true;
            self.hooks.ready_at = Some(Instant::now());
            if let Some(lib) = self.hooks.add_animations.take() {
                self.actions.push(Action::AddLibrary(lib));
            }
            if self.timing || self.hooks.screenshot.is_some() || self.hooks.bench.is_some() {
                eprintln!(
                    "slerp view: first frame with the file complete at {:.0} ms (window frame {})",
                    self.started.elapsed().as_secs_f64() * 1000.0,
                    self.egui_frames
                );
            }
        }
        let mut more = (self.hooks.script.is_some() || self.library.is_some()) && ready;
        // 1. Screenshots the script takes on its way, told apart from the
        //    final one by the path they carry, and asked for again when a
        //    window that couldn't present dropped the request.
        if let Some((path, asked)) = &mut self.hooks.shot {
            if asked.is_none_or(|at| at.elapsed() >= Duration::from_millis(500)) {
                ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::new(path.clone())));
                *asked = Some(Instant::now());
            }
            more = true;
        }
        let shots: Vec<(PathBuf, std::sync::Arc<egui::ColorImage>)> = ctx.input(|i| {
            i.raw
                .events
                .iter()
                .filter_map(|e| match e {
                    egui::Event::Screenshot { image, user_data, .. } => {
                        Some((user_data.data.as_ref()?.downcast_ref::<PathBuf>()?.clone(), image.clone()))
                    }
                    _ => None,
                })
                .collect()
        });
        for (path, image) in shots {
            if self.hooks.shot.as_ref().is_none_or(|(p, _)| *p != path) {
                // A repeated request's second picture.
                continue;
            }
            self.hooks.shot = None;
            if let Some(script) = &mut self.hooks.script {
                script.shot_taken();
            }
            save_window_image(&path, &image);
            let beside = |ext: &str| {
                let mut p = path.clone().into_os_string();
                p.push(ext);
                PathBuf::from(p)
            };
            if let Some(doc) = &self.doc {
                let what = format!("{}\n{}\n", doc.mode_name(), doc.path.display());
                let _ = slerp_core::write_atomic(&beside(".txt"), what.as_bytes());
                if let Some(state) = crate::viewstate::capture(doc, &self.look) {
                    let _ = slerp_core::write_atomic(&beside(".jsonc"), state.to_jsonc(Some(&doc.path)).as_bytes());
                }
            }
        }
        if let (Some(after), Some(at)) = (self.hooks.quit_after, self.hooks.ready_at) {
            // One wake-up at the end, so the idle time in between stays
            // idle.
            match after.checked_sub(at.elapsed()) {
                Some(left) if !left.is_zero() => ctx.request_repaint_after(left),
                _ => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
            }
        }
        if let Some(path) = self.hooks.screenshot.clone()
            && self.hooks.script.is_none()
            && self.library.is_none()
        {
            let failed = self.failed.is_some() && self.loader.busy().is_none();
            if ready || failed || (self.current.is_none() && self.egui_frames > 3) {
                // 1. A few frames for icons and thumbnails to arrive.
                self.hooks.settled += 1;
                // 2. Ask, and ask again if no picture came: a screenshot
                //    asked for while the window can't present is dropped.
                //    macOS reports a new window occluded for its first
                //    frames, and egui-wgpu skips occluded frames together
                //    with any screenshot asked for them.
                let ask = match self.hooks.screenshot_asked {
                    None => self.hooks.settled >= 4,
                    Some(at) => at.elapsed() >= Duration::from_millis(500),
                };
                if ask {
                    if self.hooks.screenshot_asked.is_none()
                        && let (Some(out), Some(doc)) = (crate::env::var("SLERP_STATE_OUT"), &self.doc)
                        && let Some(state) = crate::viewstate::capture(doc, &self.look)
                    {
                        // The view state shown, for checking replays
                        // against the screenshot.
                        let _ = slerp_core::write_atomic(Path::new(&out), state.to_jsonc(Some(&doc.path)).as_bytes());
                    }
                    ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::default()));
                    self.hooks.screenshot_asked = Some(Instant::now());
                }
                // Frames keep coming until the picture arrives.
                more = true;
            }
            let shot = ctx.input(|i| {
                i.raw.events.iter().find_map(|e| match e {
                    egui::Event::Screenshot { image, user_data, .. } if user_data.data.is_none() => Some(image.clone()),
                    _ => None,
                })
            });
            if let Some(image) = shot {
                save_window_image(&path, &image);
                self.hooks.screenshot = None;
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        }
        if let Some(bench) = &mut self.hooks.bench
            && ready
        {
            let now = Instant::now();
            if bench.start.is_none() {
                bench.start = Some(now);
                // The window presents without waiting for the display
                // (`lib.rs` sets it up so for SLERP_BENCH).
                self.gfx.gpu_times = Some(Vec::new());
                if let Some(view) = self.doc.as_mut().and_then(|d| d.scene_mut())
                    && view.player.clip.is_some()
                {
                    view.player.playing = true;
                }
            } else {
                bench.cpu_ms.push(frame_start.elapsed().as_secs_f32() * 1000.0);
                if let Some(last) = bench.last {
                    bench.interval_ms.push((now - last).as_secs_f32() * 1000.0);
                }
            }
            bench.last = Some(now);
            // Without a clip, turn the camera so every frame draws.
            if let Some(view) = self.doc.as_mut().and_then(|d| d.scene_mut())
                && view.player.clip.is_none()
            {
                view.cameras.orbit_drag(Vec2::new(2.0, 0.0));
            }
            if bench.start.is_some_and(|s| s.elapsed().as_secs_f32() >= bench.seconds) {
                let stats = |v: &mut Vec<f32>| {
                    v.sort_by(f32::total_cmp);
                    let mean = v.iter().sum::<f32>() / v.len().max(1) as f32;
                    let p = |q: f32| {
                        v.get(((v.len() as f32 * q) as usize).min(v.len().saturating_sub(1))).copied().unwrap_or(0.0)
                    };
                    (mean, p(0.5), p(0.95))
                };
                let (cm, c50, c95) = stats(&mut bench.cpu_ms);
                let (im, i50, i95) = stats(&mut bench.interval_ms);
                let mut gpu = self.gfx.gpu_times.take().unwrap_or_default();
                let (gm, g50, g95) = stats(&mut gpu);
                eprintln!(
                    "slerp view: bench {} frames in {:.1} s: {:.0} fps; frame interval mean {im:.2} ms, median {i50:.2}, p95 {i95:.2}; window frame CPU mean {cm:.2} ms, median {c50:.2}, p95 {c95:.2} (3D view drawn with a GPU wait each frame); 3D view draw until GPU done mean {gm:.2} ms, median {g50:.2}, p95 {g95:.2}",
                    bench.interval_ms.len(),
                    bench.seconds,
                    1000.0 / im.max(1e-3),
                );
                self.hooks.bench = None;
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            more = true;
        }
        more
    }
}

//endregion

/*┌─────────────────╮
| Helpers         |
└─────────────────╯*/
//region Helpers

/// What the pointer and keyboard did to the view this frame.
struct Input {
    pointer: Option<egui::Pos2>,
    /// A drag with this button, in points.
    drag: Option<(PointerButton, Vec2)>,
    /// Wheel notches, positive away from the user.
    scroll: f32,
    /// Pinch or Ctrl+wheel zoom factor.
    zoom: f32,
    shift: bool,
    /// Raw mouse movement in pixels, for mouse look with the pointer
    /// captured.
    motion: Vec2,
    /// Fly keys held: x right, y up, z forward.
    fly: Vec3,
}

impl Input {
    /// Reads this frame's input.  The response's drag queries lock the
    /// context themselves, so they come before `ctx.input`.
    fn read(ctx: &egui::Context, response: &egui::Response, keys: bool) -> Input {
        let delta = response.drag_delta();
        let button = [PointerButton::Primary, PointerButton::Middle, PointerButton::Secondary]
            .into_iter()
            .find(|&b| response.dragged_by(b));
        let drag = button.filter(|_| delta != egui::Vec2::ZERO).map(|b| (b, Vec2::new(delta.x, delta.y)));
        ctx.input(|i| Self::read_input(i, drag, keys))
    }

    fn read_input(i: &egui::InputState, drag: Option<(PointerButton, Vec2)>, keys: bool) -> Input {
        let motion = i
            .raw
            .events
            .iter()
            .filter_map(|e| match e {
                egui::Event::MouseMoved(d) => Some(Vec2::new(d.x, d.y)),
                _ => None,
            })
            .sum();
        let held = |k: Key| keys && i.key_down(k) && !i.modifiers.command;
        let axis = |pos: Key, neg: Key| (held(pos) as i32 - held(neg) as i32) as f32;
        Input {
            pointer: i.pointer.hover_pos(),
            drag,
            // About 50 points per notch of a mouse wheel.
            scroll: i.smooth_scroll_delta.y / 50.0,
            zoom: i.zoom_delta(),
            shift: i.modifiers.shift,
            motion,
            fly: Vec3::new(axis(Key::D, Key::A), axis(Key::E, Key::Q), axis(Key::W, Key::S)),
        }
    }
}

/// Pan, zoom and drawing in texture mode, or looking around inside a
/// panorama.  Returns whether it needs another frame.
#[allow(clippy::too_many_arguments)]
fn drive_texture(
    t: &mut TextureView,
    gfx: &mut Gfx,
    input: &Input,
    response: &egui::Response,
    rect: egui::Rect,
    ppp: f32,
    size: (u32, u32),
    hover: &mut Option<String>,
) -> bool {
    t.set_size(size);
    if let Some(inside) = &mut t.inside {
        // Looking around: drag turns, the wheel changes the field of view.
        if let Some((_, d)) = input.drag {
            inside.camera.yaw += d.x * 0.004;
            inside.camera.pitch = (inside.camera.pitch + d.y * 0.004).clamp(-1.55, 1.55);
        }
        if response.hovered() && input.scroll != 0.0 {
            inside.camera.fov_y = (inside.camera.fov_y * 0.9f32.powf(input.scroll)).clamp(0.2, 2.6);
        }
        gfx.set_environment(&crate::gfx::Environment::Picture(t.texture.clone()));
        let settings = panorama_settings(&t.params);
        let aspect = size.0 as f32 / size.1.max(1) as f32;
        let v = inside.camera.view(aspect);
        let key = FrameKey::Scene {
            scene: inside.id,
            pose: 0,
            textures: 0,
            view: [v.view, v.proj],
            settings,
            size,
            env: gfx.env_version,
        };
        gfx.draw_scene(key, &inside.scene, &v, &settings);
        return false;
    }
    if let Some((_, d)) = input.drag {
        t.pan(egui::vec2(d.x, d.y) * ppp);
    }
    let at = |p: egui::Pos2| (p - rect.min) * ppp;
    if response.hovered()
        && let Some(p) = input.pointer
    {
        if input.scroll != 0.0 {
            t.zoom_about(1.2f32.powf(input.scroll), at(p));
        }
        if input.zoom != 1.0 {
            t.zoom_about(input.zoom, at(p));
        }
    }
    if response.double_clicked() {
        if t.fit { t.actual_size() } else { t.fit_now() }
    }
    if let (Some(p), true) = (input.pointer, response.hovered())
        && let Some((x, y, v, hdr)) = t.texel_at(at(p))
    {
        *hover = Some(if hdr {
            format!("{x}, {y}: {:.3} {:.3} {:.3} {:.3}", v[0], v[1], v[2], v[3])
        } else {
            format!("{x}, {y}: {} {} {} {}", v[0] as u8, v[1] as u8, v[2] as u8, v[3] as u8)
        });
    }
    t.draw(gfx);
    false
}

/// Seeing a panorama from the inside: the picture itself as a sharp
/// background, with the texture view's exposure and tone mapping.
fn panorama_settings(params: &slerp_render::TextureDrawParams) -> RenderSettings {
    RenderSettings {
        background: Background::Environment { blur: 0.0 },
        exposure: params.exposure,
        tonemap: params.tonemap,
        shadows: false,
        ground_shadow: false,
        ..RenderSettings::viewer()
    }
}

/// Settings ▸ Stay Loaded After Closing: how long the viewer stays loaded,
/// without a window, after its last window closes, so the next file opens
/// at once (see `warm`).
fn keep_alive_menu(ui: &mut egui::Ui, seconds: u64, actions: &mut Vec<Action>) {
    const CHOICES: [(u64, &str); 5] =
        [(0, "Off"), (10, "10 Seconds"), (30, "30 Seconds"), (120, "2 Minutes"), (600, "10 Minutes")];
    let response = ui.menu_button("Stay Loaded After Closing", |ui| {
        for (s, text) in CHOICES {
            if ui.add(widgets::RadioButton::new(seconds == s, text)).clicked() {
                actions.push(Action::SetKeepAlive(s));
            }
        }
        // A time set in the config file that isn't one of the choices.
        if !CHOICES.iter().any(|(s, _)| *s == seconds) {
            ui.add(widgets::RadioButton::new(true, format!("{seconds} Seconds (config file)")));
        }
    });
    response
        .response
        .on_hover_text("After the last window closes, keep Slerp loaded for this long, so the next file opens at once");
}

/// Writes a screenshot of the window as a PNG, saying so on stderr.
fn save_window_image(path: &Path, image: &egui::ColorImage) {
    let img = Image8 {
        width: image.width() as u32,
        height: image.height() as u32,
        pixels: image.pixels.iter().flat_map(|c| c.to_srgba_unmultiplied()).collect(),
    };
    match slerp_core::write_atomic(path, &slerp_core::encode_png(&img)) {
        Ok(()) => eprintln!("slerp view: saved {}", path.display()),
        Err(e) => eprintln!("slerp view: couldn't save {}: {e}", path.display()),
    }
}

/// A busy indicator that, unlike egui's spinner, doesn't ask for a frame
/// every frame: the window ticks at 30 frames a second while it shows.
pub fn spinner(ui: &mut egui::Ui, size: f32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(size, size), Sense::hover());
    if ui.is_rect_visible(rect) {
        let t = ui.input(|i| i.time) as f32;
        let start = t * 5.0;
        let points: Vec<egui::Pos2> = (0..=24)
            .map(|i| {
                let a = start + i as f32 / 24.0 * 4.0;
                rect.center() + egui::vec2(a.cos(), a.sin()) * (size * 0.4)
            })
            .collect();
        let stroke = egui::Stroke::new(2.5, ui.visuals().strong_text_color());
        ui.painter().add(egui::Shape::line(points, stroke));
    }
    ui.ctx().request_repaint_after(Duration::from_millis(33));
}

/// A framed message in the middle of the view.
fn card(ui: &mut egui::Ui, rect: egui::Rect, add: impl FnOnce(&mut egui::Ui)) {
    let area = egui::Area::new(egui::Id::new("view-card"))
        .order(egui::Order::Foreground)
        .fixed_pos(rect.center())
        .pivot(egui::Align2::CENTER_CENTER)
        .constrain_to(rect);
    area.show(ui.ctx(), |ui| {
        egui::Frame::popup(ui.style()).inner_margin(egui::Margin::same(14)).show(ui, |ui| {
            ui.set_max_width(rect.width() * 0.8);
            add(ui)
        });
    });
}

fn empty_state(ui: &mut egui::Ui, rect: egui::Rect, actions: &mut Vec<Action>) {
    card(ui, rect, |ui| {
        ui.vertical_centered(|ui| {
            ui.label(egui::RichText::new("No file open").text_style(heading2()));
            ui.label("Open a model, level, texture or material, or drop one here.");
            ui.add_space(6.0);
            if ui.button("Open…").clicked() {
                actions.push(Action::Open);
            }
        });
    });
}

/// The theme's second heading style, or egui's heading before the theme
/// is applied.
pub fn heading2() -> egui::TextStyle {
    egui::TextStyle::Name("Heading2".into())
}

pub fn short_name(p: &Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| p.display().to_string())
}

/// A byte count for people: "18.1 MB".
pub fn bytes(n: u64) -> String {
    let n = n as f64;
    if n >= 1e9 {
        format!("{:.2} GB", n / 1e9)
    } else if n >= 1e6 {
        format!("{:.1} MB", n / 1e6)
    } else if n >= 1e3 {
        format!("{:.0} kB", n / 1e3)
    } else {
        format!("{n:.0} bytes")
    }
}

/// The shortcut list shown by F1, matching the crate documentation.
const SHORTCUTS: &[(&str, &str)] = &[
    ("Ctrl+O", "Open a file"),
    ("Ctrl+Q", "Quit"),
    ("Ctrl+W", "Close the file"),
    ("F5", "Reload the file"),
    ("Page Up / Page Down", "Previous or next file in the folder"),
    ("Alt+Left / Alt+Right", "Previous or next file in the folder"),
    ("Left / Right", "Previous or next file, or gallery picture"),
    ("F11, Ctrl+Shift+F", "Full screen"),
    ("Ctrl+M", "Show or hide the menu bar"),
    ("F9", "Show or hide the side panels"),
    ("Ctrl+Shift+S", "Save a screenshot"),
    ("Ctrl+Shift+C", "Copy the view state"),
    ("Ctrl+A", "Select a document page's text"),
    ("Ctrl+C", "Copy the selected text, or the Contents panel's piece"),
    ("F6", "Move keyboard focus to the panels"),
    ("Space", "Play or pause"),
    ("[ / ]", "Previous or next clip"),
    (", / .", "Step a paused clip by a frame"),
    ("Tab", "Orbit or first-person camera"),
    ("F", "Frame the model, fit the picture"),
    ("Home", "Reset the view"),
    ("1 / 3 / 7", "Front, side, top view (Ctrl: opposite)"),
    ("5", "Orthographic or perspective"),
    ("G", "Ground grid"),
    ("Ctrl+Plus / Ctrl+Minus", "Zoom in or out"),
    ("Ctrl+0", "Actual size"),
    ("W A S D", "Fly forward, left, back, right"),
    ("Q / E", "Fly down or up"),
    ("Shift", "Fly faster"),
    (
        "Esc",
        "Clear a text selection, release the mouse, go back, leave full screen; then close, if set in the View menu",
    ),
    ("Backspace", "Back to the gallery"),
    ("Mouse back", "Back to the gallery or texture, else the previous file"),
    ("Mouse forward", "Next file in the folder"),
    ("Drag", "Orbit; Shift, middle or right button pans"),
    ("Wheel", "Move closer; in first person, flying speed"),
    ("Double-click", "Orbit around the point clicked"),
];

//endregion
