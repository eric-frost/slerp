//! DICOM mode: a medical image shown like a texture (pan, zoom, the value
//! under the pointer), with the brightness window medical viewers have.
//! Dragging with the right mouse button (or Ctrl and the left) moves the
//! window: sideways changes its width (contrast), up and down its centre
//! (brightness).  The side panel picks stored windows, CT presets and the
//! image's own range, inverts, plays cine loops, and steps through the
//! folder's series in slice order.  The inspector lists every tag, with
//! sequences folded, and a DICOMDIR's patients, studies and series.
//!
//! Greyscale frames stay as floating-point modality values (Hounsfield
//! units for CT), so moving the window only re-maps them on the CPU and
//! uploads the result, with no decoding.
//!
//! Testing hooks, like the crate's others: `SLERP_DICOM_WINDOW` picks a
//! window by name (a stored window's explanation, a CT preset such as
//! `Lung`, or `Image range`) or as `centre,width`, and
//! `SLERP_DICOM_FRAME=<n>` shows frame n (from 1) of a multi-frame file.
//!
//! Tags that identify a person (names, IDs, birth dates, addresses,
//! institutions, physicians, accession numbers) are hidden until "Show
//! patient details" is turned on.  The switch is off for every file
//! opened by itself and is never saved, so screenshots don't leak them by
//! accident; it carries over only while stepping through one folder's
//! files, so the next slice doesn't hide what was asked for.  A warm
//! viewer opens each new window's files with it off too.

use crate::app::Action;
use crate::gfx::{Gfx, next_id};
use crate::loader::{Task, Waker, poll, spawn};
use crate::slice_view::{WindowEdit, counter_slider, dragged_window, number, window_controls as window_section};
use crate::texture_view::TextureView;
use egui::{PointerButton, Ui};
use slerp_core::dicom::dicomdir::{self, Record};
use slerp_core::dicom::series::{self, Series};
use slerp_core::dicom::text::Row;
use slerp_core::dicom::{CT_PRESETS, Dicom, Frame, FramePixels, StoredVoi, Voi, Window, tags};
use slerp_scene::Asset;
use slerp_scene::textures::{ColorSpace, Texture};
use slerp_theme::widgets;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Decoded frames kept for playing loops, at most this many bytes.
const CACHE_BYTES: usize = 512 << 20;

/// A DICOM file read on the loading thread.
pub struct Opened {
    dicom: Dicom,
    /// The file the picture comes from: the file itself, or a DICOMDIR's
    /// first image.
    image_path: PathBuf,
    /// A DICOMDIR's own data set and records.
    directory: Option<(Dicom, Vec<Record>)>,
    index: u32,
    frame: Option<Frame>,
    voi: Voi,
    error: Option<String>,
}

/// Reads a DICOM file (for a DICOMDIR, its first image too) and decodes
/// the frame to show: the first of a cine loop, the middle of a volume.
/// The asset is the picture with the stored window.
pub fn open(path: &Path) -> Result<(Asset, Box<Opened>), String> {
    let file = Dicom::open(path).map_err(|e| e.to_string())?;
    let (dicom, image_path, directory) = if file.is_directory() {
        let dir = path.parent().filter(|d| !d.as_os_str().is_empty()).unwrap_or(Path::new("."));
        let records = dicomdir::records(file.data(), dir);
        let image = dicomdir::first_image(&records)
            .and_then(|p| Dicom::open(&p).ok().filter(|d| !d.is_directory()).map(|d| (d, p)));
        match image {
            Some((image, p)) => (image, p, Some((file, records))),
            None => {
                // No image to show: the directory itself, with its records.
                let again = Dicom::open(path).map_err(|e| e.to_string())?;
                (again, path.to_path_buf(), Some((file, records)))
            }
        }
    } else {
        (file, path.to_path_buf(), None)
    };
    let frames = dicom.frame_count();
    let index = if dicom.frame_interval().is_some() { 0 } else { frames / 2 };
    let (frame, error) = match dicom.decode_frame(index, None) {
        Ok(f) => (Some(f), None),
        Err(e) => (None, Some(e.to_string())),
    };
    let voi = frame.as_ref().map_or(Voi::Window(Window::new(0.0, 1.0)), |f| dicom.default_voi(index, f));
    let picture = match &frame {
        Some(f) => dicom.picture(f, &voi, dicom.inverted(), dicom.pixel_aspect(index)),
        None => placeholder(),
    };
    let opened = Opened { dicom, image_path, directory, index, frame, voi, error };
    Ok((Asset::Image(Arc::new(picture)), Box::new(opened)))
}

/// A small dark square for files with nothing to show (reports, plans).
fn placeholder() -> Texture {
    Texture::from_rgba8(64, 64, [40u8, 40, 42, 255].repeat(64 * 64), ColorSpace::Srgb)
}

/*┌─────────────────╮
| View            |
└─────────────────╯*/
//region View

pub struct DicomView {
    /// The picture as shown, with pan and zoom.
    pub texture: TextureView,
    pub dicom: Dicom,
    pub image_path: PathBuf,
    pub directory: Option<(Dicom, Vec<Record>)>,
    /// The frame shown.
    pub index: u32,
    frame: Option<Arc<Frame>>,
    cache: HashMap<u32, Arc<Frame>>,
    cache_bytes: usize,
    pub voi: Voi,
    /// What the window is called in the presets list: a stored window's
    /// explanation, a preset, "Image range" or "Custom".
    pub voi_name: String,
    pub stored: Vec<StoredVoi>,
    pub invert: bool,
    aspect: f64,
    /// The frame's lowest and highest value, which sets how far a drag
    /// moves the window.
    range: (f32, f32),
    pub playing: bool,
    pub fps: f64,
    clock: f64,
    /// Why there is no picture.
    pub error: Option<String>,
    rows: Option<Vec<Row>>,
    pub filter: String,
    /// "Show patient details": off for every file opened by itself.
    pub show_patient: bool,
    pub series: Option<Arc<Vec<Series>>>,
    series_task: Option<Task<Vec<Series>>>,
    /// Wakes the window when the folder's series are found.
    waker: Waker,
    /// The window or frame changed and the picture needs redrawing; done at
    /// most once per frame.
    dirty: bool,
}

fn frame_bytes(f: &Frame) -> usize {
    match &f.pixels {
        FramePixels::Grey(v) => v.len() * 4,
        FramePixels::Rgb(v) => v.len(),
    }
}

fn range_of(frame: Option<&Frame>) -> (f32, f32) {
    match frame.map(|f| &f.pixels) {
        Some(FramePixels::Grey(v)) => slerp_core::dicom::window::value_range(v, None).unwrap_or((0.0, 1.0)),
        _ => (0.0, 255.0),
    }
}

impl DicomView {
    pub fn new(
        name: String,
        picture: Arc<Texture>,
        opened: Opened,
        gfx: &mut Gfx,
        waker: &Waker,
    ) -> Result<Self, String> {
        let Opened { dicom, image_path, directory, index, frame, voi, error } = opened;
        let mut texture = TextureView::new(name, picture, gfx)?;
        // Medical images sit on black, as in every medical viewer.
        texture.params.background = [0.02, 0.02, 0.025];
        let stored = dicom.stored_vois(index);
        let voi_name = match stored.first() {
            Some(s) if s.voi == voi => s.name.clone(),
            _ => "Image range".into(),
        };
        let fps = dicom.frame_interval().map_or(15.0, |s| (1.0 / s).clamp(1.0, 120.0));
        let frame = frame.map(Arc::new);
        let range = range_of(frame.as_deref());
        let mut cache = HashMap::new();
        let mut cache_bytes = 0;
        if let Some(f) = &frame {
            cache_bytes += frame_bytes(f);
            cache.insert(index, f.clone());
        }
        let mut view = DicomView {
            texture,
            invert: dicom.inverted(),
            aspect: dicom.pixel_aspect(index),
            dicom,
            image_path,
            directory,
            index,
            frame,
            cache,
            cache_bytes,
            voi,
            voi_name,
            stored,
            range,
            playing: false,
            fps,
            clock: 0.0,
            error,
            rows: None,
            filter: String::new(),
            show_patient: false,
            series: None,
            series_task: None,
            waker: waker.clone(),
            dirty: false,
        };
        view.testing_hooks();
        Ok(view)
    }

    /// `SLERP_DICOM_WINDOW` and `SLERP_DICOM_FRAME` (see the module docs).
    fn testing_hooks(&mut self) {
        if let Some(n) = crate::env::var("SLERP_DICOM_FRAME").and_then(|v| v.parse::<u32>().ok()) {
            self.show_frame(n.saturating_sub(1));
        }
        let Some(want) = crate::env::var("SLERP_DICOM_WINDOW") else { return };
        let named = self
            .stored
            .iter()
            .map(|s| (s.name.clone(), s.voi.clone()))
            .chain(CT_PRESETS.iter().map(|&(n, c, w)| (n.to_string(), Voi::Window(Window::new(c, w)))))
            .chain([("Image range".to_string(), self.image_range())])
            .find(|(n, _)| n.eq_ignore_ascii_case(want.trim()));
        if let Some((name, voi)) = named {
            self.set_voi(voi, name);
        } else if let Some((c, w)) = want.split_once(',')
            && let (Ok(c), Ok(w)) = (c.trim().parse::<f64>(), w.trim().parse::<f64>())
        {
            self.set_voi(Voi::Window(Window::new(c, w.max(1e-6))), "Custom");
        }
    }

    pub fn frames(&self) -> u32 {
        self.dicom.frame_count()
    }

    pub fn is_grey(&self) -> bool {
        matches!(self.frame.as_deref().map(|f| &f.pixels), Some(FramePixels::Grey(_)))
    }

    /// The series of the folder this file belongs to, once found.
    pub fn my_series(&self) -> Option<&Series> {
        let series = self.series.as_ref()?;
        let me = &self.image_path;
        series.iter().find(|s| s.files.iter().any(|f| series::same_file(f, me)))
    }

    /// Where this file is in its series: (1-based index, count).
    pub fn slice(&self) -> Option<(usize, usize)> {
        let s = self.my_series()?;
        let i = s.files.iter().position(|f| series::same_file(f, &self.image_path))?;
        Some((i + 1, s.files.len()))
    }

    /// Changes the window and redraws.
    pub fn set_voi(&mut self, voi: Voi, name: impl Into<String>) {
        self.voi = voi;
        self.voi_name = name.into();
        self.dirty = true;
    }

    /// The window spanning the frame's own values.
    pub fn image_range(&self) -> Voi {
        match self.frame.as_deref() {
            Some(f) => self.dicom.auto_voi(f),
            None => self.voi.clone(),
        }
    }

    /// Shows frame `i` of a multi-frame file, decoding it unless cached.
    pub fn show_frame(&mut self, i: u32) {
        let n = self.frames();
        if n == 0 {
            return;
        }
        let i = i % n;
        if i == self.index && self.frame.is_some() {
            return;
        }
        let frame = match self.cache.get(&i) {
            Some(f) => f.clone(),
            None => match self.dicom.decode_frame(i, None) {
                Ok(f) => {
                    let f = Arc::new(f);
                    if self.cache_bytes + frame_bytes(&f) > CACHE_BYTES {
                        self.cache.clear();
                        self.cache_bytes = 0;
                    }
                    self.cache_bytes += frame_bytes(&f);
                    self.cache.insert(i, f.clone());
                    f
                }
                Err(e) => {
                    self.error = Some(format!("frame {}: {e}", i + 1));
                    self.playing = false;
                    return;
                }
            },
        };
        self.index = i;
        self.frame = Some(frame);
        self.aspect = self.dicom.pixel_aspect(i);
        self.dirty = true;
    }

    /// Re-maps the frame through the window and uploads it, keeping pan
    /// and zoom.
    fn redraw(&mut self, gfx: &mut Gfx) {
        self.dirty = false;
        let Some(frame) = &self.frame else { return };
        let picture = self.dicom.picture(frame, &self.voi, self.invert, self.aspect);
        match gfx.renderer.upload_texture(&gfx.gpu, &picture) {
            Ok(gpu) => {
                let resized = (gpu.width(), gpu.height()) != (self.texture.gpu.width(), self.texture.gpu.height());
                self.texture.gpu = gpu;
                self.texture.texture = Arc::new(picture);
                self.texture.id = next_id();
                if resized && self.texture.fit {
                    self.texture.fit_now();
                }
            }
            Err(e) => self.error = Some(e),
        }
    }

    /// Plays the loop, collects the folder's series and redraws when the
    /// window changed.  True while something moves.
    pub fn update(&mut self, gfx: &mut Gfx, dt: f32) -> bool {
        // The folder's series are found in the background, once: the next
        // slice of the same folder takes them over (see `follow`).  A
        // DICOMDIR lists its own.
        if self.series.is_none() && self.series_task.is_none() && self.directory.is_none() {
            let dir = self.image_path.parent().map(Path::to_path_buf).unwrap_or_default();
            self.series_task = Some(spawn(&self.waker, "slerp-dicom-series", move || Ok(series::folder_series(&dir))));
        }
        if let Some(task) = &self.series_task
            && let Some(result) = poll(task)
        {
            // A failed scan lists nothing rather than trying again.
            self.series = Some(Arc::new(result.unwrap_or_default()));
            self.series_task = None;
        }
        if self.playing && self.frames() > 1 {
            self.clock += dt as f64;
            let step = 1.0 / self.fps.max(0.1);
            if self.clock >= step {
                let skip = (self.clock / step).floor() as u32;
                self.clock -= skip as f64 * step;
                // Frames that couldn't be shown in time are skipped.
                self.show_frame(self.index + skip.min(self.frames()));
            }
        }
        if self.dirty {
            self.redraw(gfx);
        }
        self.playing && self.frames() > 1
    }

    pub fn toggle_play(&mut self) {
        if self.frames() > 1 {
            self.playing = !self.playing;
            self.clock = 0.0;
        }
    }

    /// Moves the window by a drag of `delta` physical pixels: right widens
    /// it, down raises its centre (darkens), as medical viewers do.
    fn drag_window(&mut self, delta: egui::Vec2) {
        if !self.is_grey() {
            return;
        }
        let w = self.voi.as_window();
        let (center, width) = dragged_window((w.center, w.width), self.range, delta);
        self.set_voi(Voi::Window(Window { center, width, function: w.function }), "Custom");
    }

    /// The text over the view: window, frame and slice.
    pub fn overlay_parts(&self) -> Vec<String> {
        let mut parts = Vec::new();
        if let Some(e) = &self.error {
            parts.push(e.clone());
        }
        if self.is_grey() {
            let w = self.voi.as_window();
            let name = if self.voi_name == "Custom" { String::new() } else { format!(" ({})", self.voi_name) };
            parts.push(format!("C {} W {}{name}", number(w.center), number(w.width)));
        }
        if self.invert != self.dicom.inverted() {
            parts.push("Inverted".into());
        }
        if self.frames() > 1 {
            parts.push(format!("Frame {} of {}", self.index + 1, self.frames()));
        }
        if let Some((i, n)) = self.slice().filter(|s| s.1 > 1) {
            parts.push(format!("Slice {i} of {n}"));
        }
        parts
    }

    /// The modality value under texel (x, y) of the shown picture.
    fn value_at(&self, x: u32, y: u32) -> Option<String> {
        let frame = self.frame.as_deref()?;
        let (tw, th) = (self.texture.texture.width().max(1), self.texture.texture.height().max(1));
        let fx = (x as u64 * frame.width as u64 / tw as u64).min(frame.width as u64 - 1) as usize;
        let fy = (y as u64 * frame.height as u64 / th as u64).min(frame.height as u64 - 1) as usize;
        let i = fy * frame.width as usize + fx;
        Some(match &frame.pixels {
            FramePixels::Grey(v) => {
                let unit = if self.dicom.is_ct() { " HU" } else { "" };
                format!("{fx}, {fy}: {}{unit}", number(*v.get(i)? as f64))
            }
            FramePixels::Rgb(v) => {
                let p = v.get(i * 3..i * 3 + 3)?;
                format!("{fx}, {fy}: {} {} {}", p[0], p[1], p[2])
            }
        })
    }

    /// Takes over the window, inversion, pan and zoom of the file before
    /// when both are in the same series, as stepping through slices
    /// should; and the folder's series, which needn't be found again.
    pub fn follow(&mut self, old: &DicomView) {
        let same_folder = old.image_path.parent() == self.image_path.parent();
        if same_folder && old.series.is_some() {
            self.series = old.series.clone();
        }
        if same_folder {
            self.show_patient = old.show_patient;
            self.filter = old.filter.clone();
        }
        let uid = |d: &Dicom| d.data().string(tags::SERIES_INSTANCE_UID);
        if !same_folder || uid(&old.dicom).is_none() || uid(&old.dicom) != uid(&self.dicom) {
            return;
        }
        // The window stays while scrolling through a series, as in every
        // medical viewer, instead of jumping with each slice's own range.
        if old.is_grey() && self.is_grey() && old.voi != self.voi {
            self.voi = old.voi.clone();
            self.voi_name = old.voi_name.clone();
            self.dirty = true;
        }
        if old.invert != self.invert {
            self.invert = old.invert;
            self.dirty = true;
        }
        if (old.texture.gpu.width(), old.texture.gpu.height()) == (self.texture.gpu.width(), self.texture.gpu.height())
        {
            self.texture.params = old.texture.params;
            self.texture.fit = old.texture.fit;
        }
        self.fps = old.fps;
    }

    /// After a reload: the same frame, window and inversion as before.
    pub fn carry_over(&mut self, old: &DicomView) {
        self.follow(old);
        if old.index < self.frames() && old.index != self.index {
            self.show_frame(old.index);
        }
        if self.is_grey() {
            self.voi = old.voi.clone();
            self.voi_name = old.voi_name.clone();
        }
        self.invert = old.invert;
        self.playing = old.playing;
        self.dirty = true;
    }

    /// The file `delta` slices away in this series; `Some(None)` at either
    /// end, `None` when the file has no series to step through (then the
    /// folder is stepped instead).  A key pressed before the background
    /// scan of the folder finished scans it here instead, which takes
    /// milliseconds: header reads of a few hundred slices.
    pub fn step(&mut self, delta: isize) -> Option<Option<PathBuf>> {
        if self.series.is_none() && self.directory.is_none() {
            let dir = self.image_path.parent().map(Path::to_path_buf).unwrap_or_default();
            self.series = Some(Arc::new(series::folder_series(&dir)));
            self.series_task = None;
        }
        let s = self.my_series().filter(|s| s.files.len() > 1)?;
        let i = s.files.iter().position(|f| series::same_file(f, &self.image_path))? as isize;
        let j = i + delta;
        Some((0..s.files.len() as isize).contains(&j).then(|| s.files[j as usize].clone()))
    }
}

/// Pan, zoom, window/level dragging and drawing.  `drag` is the button and
/// movement in points, as the view reads it; `ctrl` makes a left drag
/// move the window.  True while it needs more frames.
#[allow(clippy::too_many_arguments)]
pub fn drive(
    d: &mut DicomView,
    gfx: &mut Gfx,
    drag: Option<(PointerButton, glam::Vec2)>,
    pointer: Option<egui::Pos2>,
    scroll: f32,
    zoom: f32,
    ctrl: bool,
    response: &egui::Response,
    rect: egui::Rect,
    ppp: f32,
    size: (u32, u32),
    dt: f32,
    hover: &mut Option<String>,
) -> bool {
    let t_size = size;
    if let Some((button, delta)) = drag {
        let delta = egui::vec2(delta.x, delta.y) * ppp;
        if button == PointerButton::Secondary || (button == PointerButton::Primary && ctrl) {
            d.drag_window(delta);
        } else {
            d.texture.pan(delta);
        }
    }
    let moving = d.update(gfx, dt);
    let at = |p: egui::Pos2| (p - rect.min) * ppp;
    let t = &mut d.texture;
    t.set_size(t_size);
    if response.hovered()
        && let Some(p) = pointer
    {
        if scroll != 0.0 {
            t.zoom_about(1.2f32.powf(scroll), at(p));
        }
        if zoom != 1.0 {
            t.zoom_about(zoom, at(p));
        }
    }
    if response.double_clicked() {
        if t.fit { t.actual_size() } else { t.fit_now() }
    }
    let texel = match (pointer, response.hovered()) {
        (Some(p), true) => t.texel_at(at(p)),
        _ => None,
    };
    if let Some((x, y, _, _)) = texel {
        *hover = d.value_at(x, y);
    }
    d.texture.draw(gfx);
    moving
}

//endregion

/*┌─────────────────╮
| Panels          |
└─────────────────╯*/
//region Panels

fn section(ui: &mut Ui, title: &str, open: bool, add: impl FnOnce(&mut Ui)) {
    crate::sections::section(ui, title, title, open, add);
}

/// The "DICOM" section of the right-hand panel: window, inversion, frames
/// and series.
pub fn controls(ui: &mut Ui, d: &mut DicomView, actions: &mut Vec<Action>) {
    if let Some(e) = &d.error {
        ui.label(egui::RichText::new(e).color(ui.visuals().warn_fg_color));
    }
    if d.frame.is_some() {
        section(ui, "Window", true, |ui| window_controls(ui, d));
    }
    if d.frames() > 1 {
        section(ui, "Frames", true, |ui| frame_controls(ui, d));
    }
    if d.directory.is_none() {
        section(ui, "Series", true, |ui| series_controls(ui, d, actions));
    }
}

fn window_controls(ui: &mut Ui, d: &mut DicomView) {
    if d.is_grey() {
        // Every window to pick from: the file's, the CT presets, the
        // image's own range.
        let mut choices: Vec<(String, Voi)> = d.stored.iter().map(|s| (s.name.clone(), s.voi.clone())).collect();
        if d.dicom.is_ct() {
            choices.extend(CT_PRESETS.iter().map(|&(name, c, w)| (name.to_string(), Voi::Window(Window::new(c, w)))));
        }
        choices.push(("Image range".into(), d.image_range()));
        let w = d.voi.as_window();
        match window_section(ui, "dicom", &choices, &d.voi_name, (w.center, w.width), d.range) {
            Some(WindowEdit::Preset(name, voi)) => d.set_voi(voi, name),
            Some(WindowEdit::Custom { center, width }) => {
                d.set_voi(Voi::Window(Window { center, width, function: w.function }), "Custom")
            }
            None => {}
        }
        ui.label(
            egui::RichText::new(crate::keys("Drag with the right mouse button (or Ctrl) to adjust.")).small().weak(),
        );
    }
    let mut invert = d.invert;
    if ui.add(widgets::CheckBox::new(&mut invert, "Invert")).changed() {
        d.invert = invert;
        d.dirty = true;
    }
}

fn frame_controls(ui: &mut Ui, d: &mut DicomView) {
    let n = d.frames();
    let shown = d.index + 1;
    ui.horizontal(|ui| {
        let label = if d.playing { "Pause" } else { "Play" };
        if ui.button(label).clicked() {
            d.toggle_play();
        }
        let mut index = (shown - 1) as usize;
        if counter_slider(ui, "Frame ", &mut index, n as usize) {
            d.playing = false;
            d.show_frame(index as u32);
        }
    });
    egui::Grid::new("dicom-frames").num_columns(2).spacing([8.0, 4.0]).show(ui, |ui| {
        ui.label("Speed:");
        widgets::slider(
            ui,
            egui::Slider::new(&mut d.fps, 1.0..=120.0).logarithmic(true).suffix(" frames/s").max_decimals(1),
        );
        ui.end_row();
    });
    if let Some(s) = d.dicom.frame_interval() {
        ui.label(egui::RichText::new(format!("The file says {:.1} frames/s.", 1.0 / s)).small().weak());
    }
}

fn series_controls(ui: &mut Ui, d: &mut DicomView, actions: &mut Vec<Action>) {
    let Some(all) = d.series.clone() else {
        ui.horizontal(|ui| {
            crate::app::spinner(ui, 14.0);
            ui.label("Reading the folder…");
        });
        return;
    };
    let mine = all.iter().position(|s| s.files.iter().any(|f| series::same_file(f, &d.image_path)));
    if all.len() > 1 {
        let mut pick = mine.unwrap_or(usize::MAX);
        let items = all.iter().enumerate().map(|(i, s)| (i, s.label()));
        widgets::Choice::new("dicom-series", &mut pick, items).width(240.0).show(ui);
        if Some(pick) != mine
            && let Some(s) = all.get(pick)
        {
            // A volume opens in its middle, as thumbnails show it.
            actions.push(Action::OpenFile(s.files[s.files.len() / 2].clone()));
        }
    } else if let Some(s) = all.first() {
        ui.label(s.label());
    }
    if let (Some(m), Some((i, n))) = (mine, d.slice())
        && n > 1
    {
        let mut slice = i - 1;
        if counter_slider(ui, "Slice ", &mut slice, n)
            && let Some(f) = all[m].files.get(slice)
        {
            actions.push(Action::OpenFile(f.clone()));
        }
        ui.label(egui::RichText::new("Page Up and Page Down step through the slices.").small().weak());
    }
}

/// The inspector's DICOM sections: a DICOMDIR's records, and every tag.
pub fn inspector(ui: &mut Ui, d: &mut DicomView, actions: &mut Vec<Action>) {
    let mut show = d.show_patient;
    if let Some((_, records)) = &d.directory {
        let records = records.clone();
        section(ui, "Directory", true, |ui| {
            ui.add(widgets::CheckBox::new(&mut show, "Show patient details"));
            let mut counter = 0;
            for r in &records {
                record_ui(ui, r, show, &mut counter, actions);
            }
        });
    }
    d.show_patient = show;
    section(ui, "Tags", true, |ui| {
        ui.add(widgets::CheckBox::new(&mut show, "Show patient details"));
        d.show_patient = show;
        ui.add(
            widgets::text_field(ui.ctx(), &mut d.filter)
                .hint_text("Find a tag or value")
                .desired_width(ui.available_width()),
        );
        let rows = d.rows.get_or_insert_with(|| {
            let mut rows = slerp_core::dicom::text::rows(d.dicom.meta());
            rows.extend(slerp_core::dicom::text::rows(d.dicom.data()));
            rows
        });
        let filter = d.filter.to_lowercase();
        let mut shown = 0;
        for (i, row) in rows.iter().enumerate() {
            if row_matches(row, &filter, show) {
                row_ui(ui, row, show, &filter, &format!("t{i}"));
                shown += 1;
            }
        }
        if shown == 0 {
            ui.label(egui::RichText::new("No tags match.").weak());
        }
    });
}

fn row_matches(row: &Row, filter: &str, show: bool) -> bool {
    let visible = show || !row.identifying;
    filter.is_empty()
        || row.tag_text.to_lowercase().contains(filter)
        || row.name.to_lowercase().contains(filter)
        || (visible && row.value.to_lowercase().contains(filter))
        || (visible && row.items.iter().flatten().any(|r| row_matches(r, filter, show)))
}

/// One element: tag, name and VR, then the value (hidden for identifying
/// elements unless shown).  Sequences fold their items.
fn row_ui(ui: &mut Ui, row: &Row, show: bool, filter: &str, id: &str) {
    let weak = ui.visuals().weak_text_color();
    let head = format!("{} {} {}", row.tag_text, row.vr, row.name);
    // A hidden element's items, nested values included, stay hidden too.
    let hidden = row.identifying && !show;
    if !row.items.is_empty() && !hidden {
        egui::CollapsingHeader::new(egui::RichText::new(format!("{head} · {}", row.value)).small())
            .id_salt(id)
            .default_open(!filter.is_empty())
            .show(ui, |ui| {
                for (n, item) in row.items.iter().enumerate().take(200) {
                    egui::CollapsingHeader::new(egui::RichText::new(format!("Item {}", n + 1)).small())
                        .id_salt((id, n))
                        .default_open(row.items.len() == 1 || !filter.is_empty())
                        .show(ui, |ui| {
                            for (k, r) in item.iter().enumerate() {
                                if row_matches(r, filter, show) {
                                    row_ui(ui, r, show, filter, &format!("{id}.{n}.{k}"));
                                }
                            }
                        });
                }
                if row.items.len() > 200 {
                    ui.label(egui::RichText::new(format!("… {} more items", row.items.len() - 200)).weak());
                }
            });
        return;
    }
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        ui.label(egui::RichText::new(&row.tag_text).small().monospace().color(weak));
        ui.label(egui::RichText::new(&row.name).small());
        ui.label(egui::RichText::new(&row.vr).small().color(weak));
        if hidden {
            ui.label(egui::RichText::new("hidden").small().italics().color(weak));
        } else if !row.value.is_empty() {
            ui.label(egui::RichText::new(&row.value).small().strong());
        }
    });
}

/// One DICOMDIR record and those below it: patients, studies and series
/// fold; images open when clicked.
fn record_ui(ui: &mut Ui, r: &Record, show: bool, counter: &mut usize, actions: &mut Vec<Action>) {
    *counter += 1;
    let ds = &r.data;
    let get = |g: u16, e: u16| ds.string(slerp_core::dicom::tag(g, e)).unwrap_or_default();
    let title = match r.kind.as_str() {
        "PATIENT" if show => format!("Patient {} ({})", get(0x0010, 0x0010).replace('^', " "), get(0x0010, 0x0020)),
        "PATIENT" => "Patient (details hidden)".to_string(),
        "STUDY" => format!("Study {} {}", get(0x0008, 0x0020), get(0x0008, 0x1030)),
        "SERIES" => format!("Series {} {} {}", get(0x0020, 0x0011), get(0x0008, 0x0060), get(0x0008, 0x103E)),
        "IMAGE" => format!("Image {}", get(0x0020, 0x0013)),
        other => other.to_lowercase(),
    };
    let title = title.split_whitespace().collect::<Vec<_>>().join(" ");
    if r.children.is_empty() {
        match &r.file {
            Some(f) => {
                if ui.link(egui::RichText::new(&title).small()).on_hover_text(f.display().to_string()).clicked() {
                    actions.push(Action::OpenFile(f.clone()));
                }
            }
            None => {
                ui.label(egui::RichText::new(format!("{title} (missing)")).small().weak());
            }
        }
        return;
    }
    let images = dicomdir::count(&r.children, "IMAGE");
    let plural = if images == 1 { "" } else { "s" };
    let label = if images > 0 && r.kind != "IMAGE" { format!("{title} · {images} image{plural}") } else { title };
    egui::CollapsingHeader::new(egui::RichText::new(label).small())
        .id_salt(("dicomdir", *counter))
        .default_open(r.kind == "PATIENT" || r.kind == "STUDY")
        .show(ui, |ui| {
            if r.kind == "SERIES"
                && let Some(first) = dicomdir::first_image(&r.children)
                && ui.small_button("Open the series").clicked()
            {
                actions.push(Action::OpenFile(first));
            }
            for c in r.children.iter().take(500) {
                record_ui(ui, c, show, counter, actions);
            }
        });
}

/// The status bar's summary: size, frames, modality.
pub fn summary(d: &DicomView) -> String {
    let mut s = match &d.dicom.info {
        Some(i) => format!("{} × {}", i.columns, i.rows),
        None => "no image".into(),
    };
    if d.frames() > 1 {
        s.push_str(&format!(", {} frames", d.frames()));
    }
    let m = d.dicom.modality();
    if !m.is_empty() {
        s.push_str(&format!(" {m}"));
    }
    s
}

//endregion

#[cfg(test)]
mod tests {
    use super::*;

    fn row(name: &str, value: &str, identifying: bool, items: Vec<Vec<Row>>) -> Row {
        Row {
            tag: 0,
            tag_text: "(0010,0010)".into(),
            name: name.into(),
            vr: "PN".into(),
            value: value.into(),
            identifying,
            items,
        }
    }

    /// Searching never finds a hidden value, in the element or in the items
    /// below it, so typing a name into the box can't confirm one.
    #[test]
    fn hidden_values_cannot_be_searched() {
        let name = row("Patient's Name", "doe^jane", true, Vec::new());
        assert!(!row_matches(&name, "jane", false));
        assert!(row_matches(&name, "jane", true));
        // The element's own name is always searchable.
        assert!(row_matches(&name, "patient's name", false));
        let nested = row(
            "Other Patient IDs Sequence",
            "1 item",
            true,
            vec![vec![row("Patient ID", "x-4711", true, Vec::new())]],
        );
        assert!(!row_matches(&nested, "4711", false));
        assert!(row_matches(&nested, "4711", true));
        // Ordinary values are found either way.
        let modality = row("Modality", "ct", false, Vec::new());
        assert!(row_matches(&modality, "ct", false));
    }
}
