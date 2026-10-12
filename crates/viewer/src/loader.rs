//! Loading files on a background thread, so a large model or texture
//! doesn't freeze the window, and listing the supported files of a folder
//! in natural order for stepping through them.

use crate::texture_view::gallery_thumbs;
use slerp_core::Kind;
use slerp_scene::textures::GpuBlocks;
use slerp_scene::{Asset, Image8, Limits};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, TryRecvError, channel};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

/// Wakes the window from other threads once it exists.  Work can start
/// before the window does (the first file loads while the GPU starts), so
/// the context is filled in later.
pub type Waker = Arc<OnceLock<egui::Context>>;

/// A result coming from another thread.
pub type Task<T> = Receiver<Result<T, String>>;

pub fn wake(waker: &Waker) {
    if let Some(ctx) = waker.get() {
        ctx.request_repaint();
    }
}

/// Runs `f` on its own thread and hands back its result through the
/// returned receiver, waking the window when done.  A panic becomes an
/// error message.
pub fn spawn<T: Send + 'static>(
    waker: &Waker,
    name: &str,
    f: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Task<T> {
    let (tx, rx) = channel();
    let waker = waker.clone();
    let spawned = std::thread::Builder::new().name(name.into()).spawn(move || {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f))
            .unwrap_or_else(|p| Err(format!("internal error: {}", panic_message(&p))));
        let _ = tx.send(result);
        wake(&waker);
    });
    if let Err(e) = spawned {
        let (tx, rx) = channel();
        let _ = tx.send(Err(format!("couldn't start a thread: {e}")));
        return rx;
    }
    rx
}

pub fn panic_message(p: &Box<dyn std::any::Any + Send>) -> String {
    p.downcast_ref::<&str>()
        .map(|s| s.to_string())
        .or_else(|| p.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "a panic".into())
}

/// Polls a task's receiver: `Some` once it finished (or its thread died).
pub fn poll<T>(rx: &Task<T>) -> Option<Result<T, String>> {
    match rx.try_recv() {
        Ok(r) => Some(r),
        Err(TryRecvError::Empty) => None,
        Err(TryRecvError::Disconnected) => Some(Err("the task stopped unexpectedly".into())),
    }
}

/*┌─────────────────╮
| File loading    |
└─────────────────╯*/
//region File loading

/// A file read into memory and ready to upload.
pub struct Loaded {
    pub path: PathBuf,
    pub asset: Asset,
    pub file_bytes: u64,
    /// Gallery thumbnails, made on the loading thread.
    pub thumbs: Vec<Option<Image8>>,
    /// Sizes of the file's parts, such as a GLB's JSON, images and
    /// geometry.
    pub parts: Vec<(String, u64)>,
    pub elapsed: Duration,
    /// A MetaHuman DNA file's rigs, which drive the scene (see `dna`).
    pub dna: Option<Box<slerp_dna::Character>>,
    /// A sprite's or map's first frame, composed on the loading thread.
    pub first_frame: Option<Image8>,
    /// The file's parts when it holds several shown one at a time (a Doom
    /// WAD's maps), and the one loaded; `None` when the whole file is.
    pub part_names: Vec<String>,
    pub part: Option<String>,
    /// A scene's geometry, packed for the GPU here rather than on the
    /// window's thread.
    pub geometry: Option<slerp_render::PackedGeometry>,
    /// The format, as sniffed.
    pub kind: Kind,
    /// A colour palette's swatches with their names and original values;
    /// `asset` is then a picture of them.
    pub palette: Option<Arc<slerp_core::palette::Palette>>,
    /// A G-code toolpath or GPS track, which the scene view redraws as its
    /// controls change (see `paths_view`).
    pub paths: Option<crate::paths_view::PathsState>,
    /// A DICOM file with its decoded frame, which the view re-windows (see
    /// `dicom_view`); `asset` is then its picture.
    pub dicom: Option<Box<crate::dicom_view::Opened>>,
    /// A volume with its first plane cut (see `volume_view`); `asset` is then
    /// that plane's picture.
    pub volume: Option<Box<crate::volume_view::Opened>>,
    /// Files beside this one that were read (a NIfTI pair's other half, an
    /// MHD's raw data), which watch mode watches too.
    pub neighbours: Vec<PathBuf>,
    /// A flat DXF drawing, which the view draws again at every zoom (see
    /// `cad_view`); `first_frame` is its picture, and `asset` a layered
    /// picture of it.
    pub cad: Option<Arc<slerp_core::cad::FlatDrawing>>,
    /// The listing of a game archive, which the Contents panel shows.
    pub contents: Option<Arc<slerp_core::contents::Contents>>,
    /// A PDF kept open to draw its pages at any zoom (see `pages_view`);
    /// `asset` is then the picture of the page opened.
    pub pages: Option<Box<crate::pages_view::Opened>>,
    /// The size of the picture the file carries of itself, when its
    /// thumbnails show that rather than Slerp's drawing of the content (see
    /// `slerp_core::saved_picture`).
    pub saved_picture: Option<(u32, u32)>,
}

/// Which part of a file to load, for files that hold several (see
/// `slerp_core::parts`).
#[derive(Clone, Debug, Default, PartialEq)]
pub enum Part {
    /// The first part, or the whole file when it has no parts.
    #[default]
    First,
    /// The part with this name.
    Named(String),
    /// The whole file as `slerp_core::load` gives it: a WAD's gallery of
    /// title screen and automaps.
    Whole,
}

/// Loads one file at a time.  Asking for another while one loads queues
/// it, replacing anything queued before, so stepping quickly through a
/// folder of large files only loads the last one asked for.
pub struct Loader {
    waker: Waker,
    current: Option<(PathBuf, Instant, Task<Loaded>)>,
    queued: Option<(PathBuf, Part)>,
    pub stage: Arc<Mutex<&'static str>>,
    /// The block formats the window's GPU samples, which models' textures
    /// stay in (see `slerp_textures::decode_for_gpu`).
    blocks: GpuBlocks,
}

impl Loader {
    /// `blocks` may be a guess when the first file starts loading before
    /// the GPU is up; the renderer decodes blocks the GPU can't sample.
    pub fn new(waker: Waker, blocks: GpuBlocks) -> Loader {
        Loader { waker, current: None, queued: None, stage: Arc::new(Mutex::new("")), blocks }
    }

    /// The block formats for files loaded from now on: the GPU's, once
    /// it is known.
    pub fn set_gpu_blocks(&mut self, blocks: GpuBlocks) {
        self.blocks = blocks;
    }

    pub fn request(&mut self, path: PathBuf) {
        self.request_part(path, Part::First);
    }

    /// Loads one part of a file that holds several, or the whole of it.
    pub fn request_part(&mut self, path: PathBuf, part: Part) {
        if self.current.is_some() {
            self.queued = Some((path, part));
        } else {
            self.start(path, part);
        }
    }

    fn start(&mut self, path: PathBuf, part: Part) {
        let stage = self.stage.clone();
        let p = path.clone();
        let blocks = self.blocks;
        let rx = spawn(&self.waker, "slerp-load", move || load(&p, part, blocks, &stage));
        self.current = Some((path, Instant::now(), rx));
    }

    /// The file loading and how long it has taken, for the progress shown.
    pub fn busy(&self) -> Option<(&Path, Duration)> {
        let shown = self.queued.as_ref().map(|q| q.0.as_path()).or(self.current.as_ref().map(|c| c.0.as_path()))?;
        Some((shown, self.current.as_ref().map_or(Duration::ZERO, |c| c.1.elapsed())))
    }

    pub fn stage(&self) -> &'static str {
        self.stage.lock().map(|s| *s).unwrap_or("")
    }

    /// A finished load, unless a newer request superseded it (then that
    /// one starts).
    pub fn poll(&mut self) -> Option<(PathBuf, Result<Loaded, String>)> {
        let (_, _, rx) = self.current.as_ref()?;
        let result = poll(rx)?;
        let (path, _, _) = self.current.take()?;
        if let Some((next, part)) = self.queued.take() {
            self.start(next, part);
            return None;
        }
        Some((path, result))
    }
}

fn set_stage(stage: &Mutex<&'static str>, s: &'static str) {
    if let Ok(mut g) = stage.lock() {
        *g = s;
    }
}

/// Reads and decodes a file with the viewer's limits (everything at full
/// size, animations included).  A file with parts opens on `part`; when
/// its first part fails to load, the whole file shows instead.
fn load(path: &Path, part: Part, blocks: GpuBlocks, stage: &Mutex<&'static str>) -> Result<Loaded, String> {
    let start = Instant::now();
    let frame_bytes = slerp_core::Config::load().image_memory_mb.saturating_mul(1 << 20);
    let limits = Limits { gpu_blocks: blocks, frame_bytes, ..Limits::viewer() };
    set_stage(stage, "Reading");
    let kind = slerp_core::sniff_file(path)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "Slerp doesn't recognise this file's format".to_string())?;
    let file_bytes = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    set_stage(stage, "Decoding");
    // Whether thumbnails show a picture saved in the file, found beside the
    // load so it costs no time.
    let saved = slerp_core::shows_saved_picture(kind).then(|| {
        let owned = path.to_path_buf();
        std::thread::spawn(move || slerp_core::saved_picture_size(kind, &owned))
    });
    let saved_picture = move || saved.and_then(|h| h.join().ok().flatten());
    if let Kind::Volume(f) = kind {
        // Volumes and slides run to gigabytes: they open without reading it
        // all, and the view cuts planes on demand.
        let memory = slerp_core::Config::load().volume_memory_mb.saturating_mul(1 << 20);
        let part = match &part {
            Part::Named(name) => Some(name.as_str()),
            _ => None,
        };
        let o = crate::volume_view::open(f, path, part, memory)?;
        return Ok(Loaded {
            path: path.to_path_buf(),
            asset: o.asset,
            file_bytes,
            thumbs: Vec::new(),
            parts: vec![("Whole file".into(), file_bytes)],
            elapsed: start.elapsed(),
            dna: None,
            first_frame: None,
            part_names: o.parts,
            part: o.part,
            geometry: None,
            kind,
            palette: None,
            paths: None,
            dicom: None,
            volume: o.opened,
            neighbours: o.neighbours,
            cad: None,
            contents: None,
            pages: None,
            saved_picture: None,
        });
    }
    if let Some(pages) = slerp_core::pages::open(kind, path) {
        // A PDF stays open in its renderer's process, so the view can draw
        // its pages again as it zooms; the pages are its parts.
        let part = match &part {
            Part::Named(name) => Some(name.as_str()),
            _ => None,
        };
        let (opened, first) = crate::pages_view::open(pages?, part)?;
        let names = opened.names();
        let part = names.get(opened.page).cloned();
        let asset = Asset::Image(crate::pages_view::image_texture(first));
        // A document of several pages lists them in the Contents panel.
        let contents =
            slerp_core::contents::Contents::of_loaded(kind, path, &asset, &names, part.as_deref()).map(Arc::new);
        return Ok(Loaded {
            path: path.to_path_buf(),
            asset,
            file_bytes,
            thumbs: Vec::new(),
            parts: vec![("Whole file".into(), file_bytes)],
            elapsed: start.elapsed(),
            dna: None,
            first_frame: None,
            part_names: names,
            part,
            geometry: None,
            kind,
            palette: None,
            paths: None,
            dicom: None,
            volume: None,
            neighbours: Vec::new(),
            cad: None,
            contents,
            pages: Some(Box::new(opened)),
            saved_picture: saved_picture(),
        });
    }
    let mut dna = None;
    let mut palette = None;
    let mut paths = None;
    let mut dicom = None;
    let mut cad = None;
    let part_names = slerp_core::parts(path);
    let mut part = match part {
        Part::First => part_names.first().cloned(),
        Part::Named(name) => Some(name),
        Part::Whole => None,
    };
    let first = part_names.first() == part.as_ref();
    let mut asset = None;
    if let Some(name) = &part {
        match slerp_core::load_part(path, &limits, name) {
            Ok(a) => asset = Some(a),
            Err(e) if !first => return Err(e.to_string()),
            Err(e) => {
                eprintln!("slerp view: {name}: {e}");
                part = None;
            }
        }
    }
    let asset = if let Some(a) = asset {
        a
    } else if kind == Kind::Dna {
        let (scene, character) = crate::dna::open(path)?;
        dna = Some(character);
        Asset::Scene(scene)
    } else if let Kind::Palette(_) = kind {
        let p = slerp_core::load_palette(path).map_err(|e| e.to_string())?;
        let picture = slerp_core::palette::picture(&p, 128);
        palette = Some(Arc::new(p));
        Asset::Image(Arc::new(picture))
    } else if let Kind::Path(f) = kind {
        let opened = slerp_core::paths::open(f, path, &limits).map_err(|e| e.to_string())?;
        paths = Some(crate::paths_view::PathsState::new(opened.data));
        Asset::Scene(opened.scene)
    } else if kind == Kind::Dicom {
        let (picture, opened) = crate::dicom_view::open(path)?;
        dicom = Some(opened);
        picture
    } else if let Kind::Cad(f) = kind {
        match slerp_core::cad::open(f, path, &limits).map_err(|e| e.to_string())? {
            slerp_core::cad::Opened::Flat(d) => {
                cad = Some(d.clone());
                Asset::Canvas(d)
            }
            slerp_core::cad::Opened::Model(scene) => Asset::Scene(*scene),
            slerp_core::cad::Opened::Picture(picture) => Asset::Image(picture),
        }
    } else if let Kind::Archive(_) = kind {
        // A game archive with no pictures to show still opens, on its
        // contents.
        slerp_core::load_kind(kind, path, &limits).unwrap_or(Asset::Gallery(Vec::new()))
    } else {
        slerp_core::load_kind(kind, path, &limits).map_err(|e| e.to_string())?
    };
    // What is inside the file, listed from what was just loaded (an
    // archive from its own directory), for the Contents panel.
    let contents =
        slerp_core::contents::Contents::of_loaded(kind, path, &asset, &part_names, part.as_deref()).map(|mut c| {
            if let Some(character) = &dna {
                c.add_character(character);
            }
            Arc::new(c)
        });
    let thumbs = match &asset {
        Asset::Gallery(items) => {
            set_stage(stage, "Making thumbnails");
            let mut thumbs = gallery_thumbs(items, 192);
            // An icon's transparency stays transparent, rather than turning black.
            if matches!(kind, Kind::Image(_)) {
                for (thumb, item) in thumbs.iter_mut().zip(items) {
                    *thumb = thumb.take().map(|img| slerp_core::with_alpha(&item.texture, img));
                }
            }
            thumbs
        }
        _ => Vec::new(),
    };
    let first_frame = match &asset {
        Asset::Canvas(_) if cad.is_some() => {
            set_stage(stage, "Drawing");
            cad.as_deref().map(crate::cad_view::first_picture)
        }
        Asset::Canvas(canvas) => {
            set_stage(stage, "Composing");
            let shown: Vec<bool> = canvas.layers().iter().map(|l| l.visible).collect();
            Some(canvas.compose(0, &shown, crate::canvas_view::viewer_scale(&**canvas)))
        }
        _ => None,
    };
    let parts = file_parts(path, kind, file_bytes);
    let geometry = match &asset {
        Asset::Scene(scene) => Some(slerp_render::pack_geometry(scene, slerp_render::DEFAULT_BINDING_BYTES)),
        _ => None,
    };
    set_stage(stage, "Uploading");
    Ok(Loaded {
        path: path.to_path_buf(),
        asset,
        file_bytes,
        thumbs,
        parts,
        elapsed: start.elapsed(),
        dna,
        first_frame,
        part_names,
        part,
        geometry,
        kind,
        palette,
        paths,
        dicom,
        volume: None,
        neighbours: Vec::new(),
        cad,
        contents,
        pages: None,
        saved_picture: saved_picture(),
    })
}

/// What a file's bytes are spent on.  For glTF: the JSON, the images and
/// the rest of the binary data (geometry and animation).  Other formats
/// report their size as a whole.
fn file_parts(path: &Path, kind: Kind, bytes: u64) -> Vec<(String, u64)> {
    if kind == Kind::Gltf
        && let Some(parts) = gltf_parts(path)
    {
        return parts;
    }
    vec![("Whole file".into(), bytes)]
}

fn gltf_parts(path: &Path) -> Option<Vec<(String, u64)>> {
    use std::io::Read;
    let mut file = std::fs::File::open(path).ok()?;
    let mut head = [0u8; 20];
    file.read_exact(&mut head).ok()?;
    let le = |b: &[u8]| u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as u64;
    let (json, bin) = if &head[0..4] == b"glTF" {
        let json_len = le(&head[12..16]);
        // The JSON chunk is read whole; a GLB's JSON is small next to its
        // binary chunk, but cap it against hostile headers.
        if json_len > 256 << 20 {
            return None;
        }
        let mut json = vec![0u8; json_len as usize];
        file.read_exact(&mut json).ok()?;
        let total = le(&head[8..12]);
        (json, total.saturating_sub(20 + json_len))
    } else {
        (std::fs::read(path).ok()?, 0)
    };
    let root: serde_json::Value = serde_json::from_slice(&json).ok()?;
    let views = root.get("bufferViews").and_then(|v| v.as_array());
    let view_len = |i: u64| {
        views.and_then(|v| v.get(i as usize)).and_then(|v| v.get("byteLength")).and_then(|l| l.as_u64()).unwrap_or(0)
    };
    let mut images_in_bin = 0u64;
    let mut external = 0u64;
    let dir = path.parent().unwrap_or(Path::new("."));
    let mut seen = std::collections::HashSet::new();
    for img in root.get("images").and_then(|v| v.as_array()).into_iter().flatten() {
        if let Some(v) = img.get("bufferView").and_then(|v| v.as_u64()) {
            images_in_bin += view_len(v);
        } else if let Some(uri) = img.get("uri").and_then(|u| u.as_str())
            && !uri.starts_with("data:")
            && seen.insert(uri.to_string())
        {
            external += std::fs::metadata(dir.join(uri)).map(|m| m.len()).unwrap_or(0);
        }
    }
    let mut buffers = 0u64;
    for b in root.get("buffers").and_then(|v| v.as_array()).into_iter().flatten() {
        if let Some(uri) = b.get("uri").and_then(|u| u.as_str())
            && !uri.starts_with("data:")
        {
            buffers += std::fs::metadata(dir.join(uri)).map(|m| m.len()).unwrap_or(0);
        }
    }
    let mut parts = vec![("JSON".to_string(), json.len() as u64)];
    if images_in_bin > 0 {
        parts.push(("Embedded images".into(), images_in_bin));
    }
    if bin > 0 {
        parts.push(("Geometry and animation".into(), bin.saturating_sub(images_in_bin)));
    }
    if buffers > 0 {
        parts.push(("External buffers".into(), buffers));
    }
    if external > 0 {
        parts.push(("External images".into(), external));
    }
    Some(parts)
}

//endregion

/*┌─────────────────╮
| Folders         |
└─────────────────╯*/
//region Folders

/// File name extensions Slerp opens, from the format table
/// (`slerp_formats`): the formats' own extensions and those whose content
/// decides (`.map`, `.pal`, `.nc`).  Stepping through a folder skips
/// everything else without reading it; candidates are then confirmed by
/// their content.  Some have two parts (`nii.gz`, `ome.tif`).
pub fn extensions() -> &'static [&'static str] {
    static LIST: std::sync::OnceLock<Vec<&'static str>> = std::sync::OnceLock::new();
    LIST.get_or_init(slerp_formats::viewer_extensions)
}

pub fn has_supported_extension(path: &Path) -> bool {
    path.file_name().and_then(|n| n.to_str()).is_some_and(slerp_formats::viewer_steps_to)
}

/// The supported files in `dir`, in natural order ("map2" before
/// "map10"), hidden files left out.
pub fn list_folder(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut files: Vec<PathBuf> = entries
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().map(|t| t.is_file() || t.is_symlink()).unwrap_or(false))
        .map(|e| e.path())
        .filter(|p| has_supported_extension(p))
        .filter(|p| !p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with('.')))
        .collect();
    files.sort_by(|a, b| natural_cmp(&file_name(a), &file_name(b)));
    files
}

fn file_name(p: &Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

/// Compares names the way people count: runs of digits by their value,
/// everything else case-insensitively, ties broken by the plain text.
pub fn natural_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    let (mut x, mut y) = (a.chars().peekable(), b.chars().peekable());
    loop {
        match (x.peek().copied(), y.peek().copied()) {
            (None, None) => return a.cmp(b),
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(c), Some(d)) if c.is_ascii_digit() && d.is_ascii_digit() => {
                let take = |it: &mut std::iter::Peekable<std::str::Chars>| {
                    let mut s = String::new();
                    while let Some(&c) = it.peek() {
                        if !c.is_ascii_digit() {
                            break;
                        }
                        s.push(c);
                        it.next();
                    }
                    s
                };
                let (m, n) = (take(&mut x), take(&mut y));
                let (mt, nt) = (m.trim_start_matches('0'), n.trim_start_matches('0'));
                let ord = mt.len().cmp(&nt.len()).then_with(|| mt.cmp(nt));
                if ord != Ordering::Equal {
                    return ord;
                }
            }
            (Some(c), Some(d)) => {
                let ord = c.to_lowercase().cmp(d.to_lowercase());
                if ord != Ordering::Equal {
                    return ord;
                }
                x.next();
                y.next();
            }
        }
    }
}

/// The file `steps` places from `current` in its folder that Slerp can
/// open, wrapping around.  Files whose content isn't a supported format
/// are skipped.
pub fn step_in_folder(current: &Path, steps: isize) -> Option<PathBuf> {
    let dir = current.parent()?;
    let files = list_folder(if dir.as_os_str().is_empty() { Path::new(".") } else { dir });
    if files.is_empty() {
        return None;
    }
    let n = files.len() as isize;
    let here = files.iter().position(|f| f.file_name() == current.file_name());
    let mut i = match here {
        Some(i) => i as isize,
        // The current file is gone or unsupported: start beside where it
        // would sort.
        None => {
            files.iter().position(|f| natural_cmp(&file_name(f), &file_name(current)).is_gt()).map_or(n, |i| i as isize)
                - if steps > 0 { 1 } else { 0 }
        }
    };
    let dir = steps.signum();
    for _ in 0..n {
        i = (i + dir).rem_euclid(n);
        let candidate = &files[i as usize];
        if candidate.file_name() == current.file_name() {
            return None;
        }
        if matches!(slerp_core::sniff_file(candidate), Ok(Some(_))) {
            return Some(candidate.clone());
        }
    }
    None
}

/// Where `path` sits among its folder's supported files: (1-based index,
/// count), for the status bar.
pub fn position_in_folder(path: &Path) -> Option<(usize, usize)> {
    let files = list_folder(path.parent()?);
    let i = files.iter().position(|f| f.file_name() == path.file_name())?;
    Some((i + 1, files.len()))
}

//endregion

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn natural_order() {
        let mut v = vec!["map10.bsp", "Map2.bsp", "map1.bsp", "map01b.bsp", "a.glb", "B.glb"];
        v.sort_by(|a, b| natural_cmp(a, b));
        assert_eq!(v, vec!["a.glb", "B.glb", "map1.bsp", "map01b.bsp", "Map2.bsp", "map10.bsp"]);
        assert_eq!(natural_cmp("x007", "x7"), "x007".cmp("x7"));
    }

    #[test]
    fn steps_through_supported_files() {
        let dir = std::env::temp_dir().join(format!("slerp-viewer-step-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // Radiance HDR files are recognised by their first line.
        let hdr = b"#?RADIANCE\nFORMAT=32-bit_rle_rgbe\n\n-Y 1 +X 1\n\x80\x80\x80\x80";
        for name in ["b2.hdr", "b10.hdr", "a.hdr", "notes.txt"] {
            std::fs::write(dir.join(name), hdr).unwrap();
        }
        // Named like a texture but not one: skipped by content.
        std::fs::write(dir.join("b3.hdr"), b"not an image").unwrap();
        let next = step_in_folder(&dir.join("b2.hdr"), 1).unwrap();
        assert_eq!(next.file_name().unwrap(), "b10.hdr");
        let wrapped = step_in_folder(&dir.join("b10.hdr"), 1).unwrap();
        assert_eq!(wrapped.file_name().unwrap(), "a.hdr");
        let back = step_in_folder(&dir.join("a.hdr"), -1).unwrap();
        assert_eq!(back.file_name().unwrap(), "b10.hdr");
        assert_eq!(position_in_folder(&dir.join("b10.hdr")), Some((4, 4)));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A Doom WAD opens on its first map in 3D; another map or the whole
    /// file's gallery can be asked for by name.
    #[test]
    fn wads_open_on_their_first_map() {
        let wad = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../local/assets/doom/freedoom/freedoom2.wad");
        if !wad.exists() {
            eprintln!("skipping: {} is missing", wad.display());
            return;
        }
        let stage = Mutex::new("");
        let first = load(&wad, Part::First, GpuBlocks::default(), &stage).unwrap();
        assert_eq!(first.part.as_deref(), Some("MAP01"));
        assert_eq!(first.part_names.len(), 32);
        let Asset::Scene(scene) = &first.asset else { panic!("MAP01 should be a scene") };
        assert_eq!(scene.kind, slerp_scene::SceneKind::Level);
        let named = load(&wad, Part::Named("MAP07".into()), GpuBlocks::default(), &stage).unwrap();
        assert_eq!(named.part.as_deref(), Some("MAP07"));
        let whole = load(&wad, Part::Whole, GpuBlocks::default(), &stage).unwrap();
        assert!(whole.part.is_none());
        assert!(matches!(whole.asset, Asset::Gallery(_)));
        assert!(load(&wad, Part::Named("MAP99".into()), GpuBlocks::default(), &stage).is_err());
    }
}
