//! Volume mode: scientific arrays, medical volumes and microscope stacks
//! (NumPy, NIfTI, MetaImage, NRRD, TIFF stacks and OME-TIFF, NetCDF, HDF5),
//! shown one plane at a time.
//!
//! The volume stays in its file or in memory as `slerp_volumes` holds it; a
//! plane is cut on demand (on a thread when that can take a while) and put
//! through a brightness window and a colour map on the CPU, so changing the
//! window, the colour map or a channel never reads the file again.  The
//! wheel (and Page Up/Down, and the arrow keys Up and Down) steps through the
//! slices, Ctrl and the wheel zooms, the right button (or Ctrl and the left)
//! moves the window as in `dicom_view`, and the left button pans.  Arrays
//! with no picture (a vector, a scalar, five dimensions) show their
//! statistics, and a one-dimensional array its plot.
//!
//! The volume can be cut as axial, coronal or sagittal planes (arrays whose
//! orientation the file doesn't give get "XY", "XZ" and "YZ", and, when
//! even the slice axis is a guess, a switch for it), and a maximum
//! intensity projection runs along the slice axis of volumes small enough
//! to read whole.
//!
//! Testing hooks: `SLERP_VOLUME_PLANE=axial|coronal|sagittal`,
//! `SLERP_VOLUME_SLICE=<n>` (from 1), `SLERP_VOLUME_TIME=<n>` (from 1),
//! `SLERP_VOLUME_WINDOW=<centre>,<width>` and `SLERP_VOLUME_COLORMAP=<name>`.

use crate::app::Action;
use crate::gfx::{Gfx, next_id};
use crate::loader::{Part, Task, Waker, poll, spawn};
use crate::slice_view::{WindowEdit, counter_slider, dragged_window, number, window_controls};
use crate::texture_view::TextureView;
use egui::{PointerButton, Ui};
use slerp_core::volumes::render::{percentile_window, render, square_pixels};
use slerp_core::volumes::{
    AxisMap, ChannelSel, ChannelStyle, ColorMap, Content, Layout, OpenOptions, PlaneKind, Role, Slice, SliceView,
    Stats, Style, Volume, VolumeFormat,
};
use slerp_scene::Asset;
use slerp_scene::textures::{ColorSpace, Texture};
use slerp_theme::widgets;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Planes drawn larger than this (pixels along the longer side) after
/// correcting for voxel shape are limited to it.
const MAX_SIDE: usize = 4096;
/// Volumes at most this big (in samples) offer a maximum intensity projection.
const MIP_LIMIT: u64 = 1 << 28;

/*┌─────────────────╮
| Opening         |
└─────────────────╯*/
//region Opening

/// A volume opened on the loading thread, with its first plane.
pub struct Opened {
    volume: Arc<Volume>,
    map: Option<AxisMap>,
    kind: PlaneKind,
    slice: usize,
    first: Option<Slice>,
    stats: Option<Stats>,
    plot: Option<Vec<(f32, f32)>>,
    window: (f64, f64),
    window_name: String,
    channels: Vec<ChannelStyle>,
    cmap: ColorMap,
}

/// What the loading thread hands back for a volume file.
pub struct Loaded {
    pub asset: Asset,
    pub opened: Option<Box<Opened>>,
    pub parts: Vec<String>,
    pub part: Option<String>,
    pub neighbours: Vec<PathBuf>,
}

fn dark_placeholder() -> Texture {
    Texture::from_rgba8(64, 64, [40u8, 40, 42, 255].repeat(64 * 64), ColorSpace::Srgb)
}

fn bad(e: impl std::fmt::Display) -> String {
    e.to_string()
}

/// Reads a volume file for the viewer: pictures (slide levels) come back
/// finished, volumes with their first plane cut and windowed.
pub fn open(f: VolumeFormat, path: &Path, part: Option<&str>, memory_limit: u64) -> Result<Loaded, String> {
    let o = slerp_core::volumes::open(f, path, part, &OpenOptions::viewer(memory_limit)).map_err(bad)?;
    let (parts, shown, neighbours) = (o.parts, o.part, o.neighbours);
    let volume = match o.content {
        Content::Picture(p) => {
            return Ok(Loaded {
                asset: Asset::Image(Arc::new(p.texture)),
                opened: None,
                parts,
                part: shown,
                neighbours,
            });
        }
        Content::Volume(v) => Arc::new(*v),
    };
    let in_memory = volume.source.as_slice().is_some();
    let stats = if in_memory || volume.no_picture.is_some() { volume.stats().ok() } else { None };
    let plot = (volume.no_picture.is_some() && volume.axes.len() == 1).then(|| plot_data(&volume)).flatten();
    let Some(map) = volume.default_map().filter(|_| volume.no_picture.is_none()) else {
        let opened = Opened {
            volume,
            map: None,
            kind: PlaneKind::Axial,
            slice: 0,
            first: None,
            stats,
            plot,
            window: (0.0, 1.0),
            window_name: String::new(),
            channels: Vec::new(),
            cmap: ColorMap::Gray,
        };
        return Ok(Loaded {
            asset: Asset::Image(Arc::new(dark_placeholder())),
            opened: Some(Box::new(opened)),
            parts,
            part: shown,
            neighbours,
        });
    };
    // The first plane: the middle slice of the first time point.
    let kind = PlaneKind::Axial;
    let slice = volume.slice_count(&map, kind) / 2;
    let view = SliceView { kind, slice, t: 0, neurological: false };
    let first = volume.extract(&map, &view, ChannelSel::All, Some(MAX_SIDE * 2)).map_err(bad)?;
    let auto = slerp_core::volumes::auto_style(&volume, &first);
    // A volume-wide window keeps its brightness while scrolling; a slice's
    // own range is what a thumbnail uses.
    let mut window = auto.window;
    let mut window_name = "Slice 1–99 %".to_string();
    if let Some(s) = &stats
        && !matches!(auto.layout, Layout::Channels(_) | Layout::Rgb)
        && s.p99 > s.p1
    {
        window = (s.p1 as f32, s.p99 as f32);
        window_name = "Volume 1–99 %".into();
    }
    if matches!(auto.layout, Layout::Rgb) {
        window_name = "Full".into();
    }
    let (cmap, channels) = match &auto.layout {
        Layout::Single { map, .. } => (*map, Vec::new()),
        Layout::Channels(c) => (ColorMap::Gray, c.clone()),
        Layout::Rgb => (ColorMap::Gray, Vec::new()),
    };
    let window = ((window.0 as f64 + window.1 as f64) / 2.0, (window.1 as f64 - window.0 as f64).max(1e-9));
    let opened = Opened {
        volume: volume.clone(),
        map: Some(map),
        kind,
        slice,
        first: Some(first),
        stats,
        plot,
        window,
        window_name,
        channels,
        cmap,
    };
    let picture = opened.picture().unwrap_or_else(dark_placeholder);
    Ok(Loaded {
        asset: Asset::Image(Arc::new(picture)),
        opened: Some(Box::new(opened)),
        parts,
        part: shown,
        neighbours,
    })
}

impl Opened {
    fn picture(&self) -> Option<Texture> {
        let slice = self.first.as_ref()?;
        let invert = self.volume.invert;
        Some(draw_slice(&self.volume, slice, self.window, self.cmap, invert, &self.channels))
    }
}

/// Puts a plane through the window and colour map as the viewer shows it.
fn draw_slice(
    volume: &Volume,
    slice: &Slice,
    window: (f64, f64),
    cmap: ColorMap,
    invert: bool,
    channels: &[ChannelStyle],
) -> Texture {
    let style = style_of(slice, window, cmap, invert, channels, !volume.channels.is_empty());
    let (rgba, alpha) = render(slice, &style);
    let (w, h, rgba) = square_pixels(slice.width, slice.height, rgba, slice.col_spacing, slice.row_spacing, MAX_SIDE);
    let mut tex = slerp_core::volumes::texture_of(volume, w, h, rgba, 1);
    tex.has_alpha = alpha;
    tex
}

fn style_of(
    slice: &Slice,
    window: (f64, f64),
    cmap: ColorMap,
    invert: bool,
    channels: &[ChannelStyle],
    has_channels: bool,
) -> Style {
    let lo = (window.0 - window.1 / 2.0) as f32;
    let hi = (window.0 + window.1 / 2.0) as f32;
    let layout = if has_channels && channels.len() == slice.samples {
        Layout::Channels(channels.to_vec())
    } else if slice.samples >= 3 {
        Layout::Rgb
    } else {
        Layout::Single { map: cmap, invert }
    };
    Style { window: (lo, hi), layout }
}

/// A one-dimensional array as up to 2048 (lowest, highest) pairs, one per
/// stretch of it, for a plot.  Long arrays are looked at in 128 samples from
/// the start of each stretch.
fn plot_data(v: &Volume) -> Option<Vec<(f32, f32)>> {
    let n = v.axes.first()?.len as u64;
    let buckets = n.min(2048);
    let per = n / buckets.max(1);
    let mut out = Vec::with_capacity(buckets as usize);
    for b in 0..buckets {
        let take = per.clamp(1, 128) as usize;
        let vals = v.read_values(b * per, take).ok()?;
        let (mn, mx) = vals
            .iter()
            .filter(|x| x.is_finite())
            .fold((f32::INFINITY, f32::NEG_INFINITY), |(a, c), &x| (a.min(x), c.max(x)));
        out.push(if mn <= mx { (mn, mx) } else { (0.0, 0.0) });
    }
    Some(out)
}

/// The status bar's summary: size, type, volumes.
pub fn summary(v: &VolumeView) -> String {
    let shape = v.volume.shape().iter().map(|s| s.to_string()).collect::<Vec<_>>().join(" × ");
    format!("{shape} {}", v.volume.dtype.name())
}

//endregion

/*┌─────────────────╮
| View            |
└─────────────────╯*/
//region View

pub struct VolumeView {
    /// The picture of the plane shown, with pan and zoom.
    pub texture: TextureView,
    pub volume: Arc<Volume>,
    pub map: Option<AxisMap>,
    pub kind: PlaneKind,
    /// The slice shown, counted from 0, and the time point.
    pub slice: usize,
    pub t: usize,
    pub neurological: bool,
    pub mip: bool,
    /// Brightness window: centre and width, and what it is called.
    pub window: (f64, f64),
    pub window_name: String,
    pub cmap: ColorMap,
    pub invert: bool,
    pub channels: Vec<ChannelStyle>,
    stats: Option<Stats>,
    plot: Option<Vec<(f32, f32)>>,
    /// The data's own range, which sets how far a drag moves the window.
    range: (f32, f32),
    current: Option<Arc<Slice>>,
    pending: Option<Task<Slice>>,
    /// A plane was asked for while another was being cut.
    queued: bool,
    dirty_plane: bool,
    dirty_draw: bool,
    pub error: Option<String>,
    pub playing: bool,
    clock: f64,
    waker: Waker,
    /// Wheel movement that hasn't made a whole slice yet.
    scroll: f32,
}

impl VolumeView {
    pub fn new(
        name: String,
        picture: Arc<Texture>,
        opened: Opened,
        gfx: &mut Gfx,
        waker: &Waker,
    ) -> Result<Self, String> {
        let Opened { volume, map, kind, slice, first, stats, plot, window, window_name, channels, cmap } = opened;
        let mut texture = TextureView::new(name, picture, gfx)?;
        // Medical images sit on black.
        texture.params.background = [0.02, 0.02, 0.025];
        let range = stats
            .as_ref()
            .map(|s| (s.min as f32, s.max as f32))
            .or_else(|| {
                first.as_ref().map(|f| {
                    (f.data.iter().cloned().fold(f32::MAX, f32::min), f.data.iter().cloned().fold(f32::MIN, f32::max))
                })
            })
            .unwrap_or((0.0, 1.0));
        let mut view = VolumeView {
            texture,
            invert: volume.invert,
            volume,
            map,
            kind,
            slice,
            t: 0,
            neurological: false,
            mip: false,
            window,
            window_name,
            cmap,
            channels,
            stats,
            plot,
            range,
            current: first.map(Arc::new),
            pending: None,
            queued: false,
            dirty_plane: false,
            dirty_draw: false,
            error: None,
            playing: false,
            clock: 0.0,
            waker: waker.clone(),
            scroll: 0.0,
        };
        view.testing_hooks();
        Ok(view)
    }

    /// `SLERP_VOLUME_*` (see the module docs).
    fn testing_hooks(&mut self) {
        let var = |k: &str| crate::env::var(k);
        if let Some(k) = var("SLERP_VOLUME_PLANE") {
            let kind = match k.to_ascii_lowercase().as_str() {
                "coronal" => Some(PlaneKind::Coronal),
                "sagittal" => Some(PlaneKind::Sagittal),
                "axial" => Some(PlaneKind::Axial),
                _ => None,
            };
            if let Some(kind) = kind {
                self.set_plane(kind);
            }
        }
        if let Some(n) = var("SLERP_VOLUME_SLICE").and_then(|v| v.parse::<usize>().ok()) {
            self.set_slice(n.saturating_sub(1));
        }
        if let Some(n) = var("SLERP_VOLUME_TIME").and_then(|v| v.parse::<usize>().ok()) {
            self.set_time(n.saturating_sub(1));
        }
        if let Some(w) = var("SLERP_VOLUME_WINDOW")
            && let Some((c, wd)) = w.split_once(',')
            && let (Ok(c), Ok(wd)) = (c.trim().parse::<f64>(), wd.trim().parse::<f64>())
        {
            self.set_window((c, wd.max(1e-9)), "Custom");
        }
        if let Some(name) = var("SLERP_VOLUME_COLORMAP")
            && let Some(m) = ColorMap::ALL.iter().find(|m| m.name().eq_ignore_ascii_case(name.trim()))
        {
            self.cmap = *m;
            self.dirty_draw = true;
        }
    }

    /// There is a plane to look at (not a one-dimensional array or the like).
    pub fn has_picture(&self) -> bool {
        self.map.is_some()
    }

    fn oriented(&self) -> bool {
        self.map.as_ref().is_some_and(|m| self.volume.oriented(m))
    }

    pub fn slice_count(&self) -> usize {
        self.map.as_ref().map_or(1, |m| self.volume.slice_count(m, self.kind))
    }

    pub fn time_count(&self) -> usize {
        self.map.as_ref().map_or(1, |m| self.volume.time_count(m))
    }

    fn can_switch_planes(&self) -> bool {
        self.map.as_ref().is_some_and(|m| m.z.is_some() && self.volume.axes[m.z.unwrap_or(0)].len > 0)
    }

    fn view_spec(&self) -> SliceView {
        SliceView { kind: self.kind, slice: self.slice, t: self.t, neurological: self.neurological }
    }

    fn sample_sel(&self) -> ChannelSel {
        ChannelSel::All
    }

    /// Whether a maximum intensity projection can be offered.
    fn can_project(&self) -> bool {
        self.has_picture()
            && self.slice_count() > 1
            && self.volume.samples() <= MIP_LIMIT
            && self.volume.source.as_slice().is_some()
    }

    pub fn set_slice(&mut self, n: usize) {
        let n = n.min(self.slice_count().saturating_sub(1));
        if n != self.slice {
            self.slice = n;
            self.dirty_plane = true;
        }
    }

    pub fn set_time(&mut self, n: usize) {
        let n = n.min(self.time_count().saturating_sub(1));
        if n != self.t {
            self.t = n;
            self.dirty_plane = true;
        }
    }

    pub fn set_plane(&mut self, kind: PlaneKind) {
        if self.kind != kind {
            self.kind = kind;
            self.slice = self.slice_count() / 2;
            self.dirty_plane = true;
            self.texture.fit = true;
        }
    }

    pub fn set_window(&mut self, window: (f64, f64), name: impl Into<String>) {
        self.window = window;
        self.window_name = name.into();
        self.dirty_draw = true;
    }

    /// Steps `delta` slices, or volumes when the plane has only one slice (a
    /// time series of pictures, say); false at either end.
    pub fn step_slice(&mut self, delta: isize) -> bool {
        let by_time = self.slice_count() <= 1 && self.time_count() > 1;
        let (now, n) = if by_time { (self.t, self.time_count()) } else { (self.slice, self.slice_count()) };
        let next = now as isize + delta;
        if next < 0 || next >= n as isize {
            return false;
        }
        if by_time {
            self.t = next as usize;
        } else {
            self.slice = next as usize;
        }
        self.dirty_plane = true;
        true
    }

    pub fn toggle_play(&mut self) {
        if self.slice_count() > 1 || self.time_count() > 1 {
            self.playing = !self.playing;
            self.clock = 0.0;
        }
    }

    /// Cuts the plane (on a thread unless the data is in memory) and shows it,
    /// and plays.  True while something moves or loads.
    pub fn update(&mut self, gfx: &mut Gfx, dt: f32) -> bool {
        if let Some(task) = &self.pending
            && let Some(result) = poll(task)
        {
            self.pending = None;
            match result {
                Ok(s) => {
                    self.current = Some(Arc::new(s));
                    self.error = None;
                    self.dirty_draw = true;
                }
                Err(e) => {
                    self.error = Some(e);
                    self.playing = false;
                }
            }
            if self.queued {
                self.queued = false;
                self.dirty_plane = true;
            }
        }
        if self.playing {
            self.clock += dt as f64;
            let step = 1.0 / 12.0;
            if self.clock >= step {
                self.clock -= step;
                if self.time_count() > 1 {
                    let n = self.time_count();
                    self.set_time((self.t + 1) % n);
                } else {
                    let n = self.slice_count();
                    self.set_slice((self.slice + 1) % n);
                }
            }
        }
        if self.dirty_plane && self.map.is_some() {
            if self.pending.is_some() {
                self.queued = true;
                self.dirty_plane = false;
            } else {
                self.dirty_plane = false;
                self.start_plane();
            }
        }
        if self.dirty_draw {
            self.redraw(gfx);
        }
        self.pending.is_some() || self.playing
    }

    fn start_plane(&mut self) {
        let Some(map) = self.map else { return };
        let (volume, view, sel, mip) = (self.volume.clone(), self.view_spec(), self.sample_sel(), self.mip);
        let cut = move || -> Result<Slice, String> {
            if mip { volume.mip(&map, &view, sel) } else { volume.extract(&map, &view, sel, Some(MAX_SIDE * 2)) }
                .map_err(bad)
        };
        if self.volume.source.as_slice().is_some() && !mip {
            match cut() {
                Ok(s) => {
                    self.current = Some(Arc::new(s));
                    self.error = None;
                    self.dirty_draw = true;
                }
                Err(e) => self.error = Some(e),
            }
        } else {
            self.pending = Some(spawn(&self.waker, "slerp-volume-plane", cut));
        }
    }

    /// Draws the plane again through the window and colour map, keeping pan
    /// and zoom.
    fn redraw(&mut self, gfx: &mut Gfx) {
        self.dirty_draw = false;
        let Some(slice) = self.current.clone() else { return };
        let picture = draw_slice(&self.volume, &slice, self.window, self.cmap, self.invert, &self.channels);
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

    /// The text for the value and place under texel (x, y) of the picture.
    fn readout(&self, x: u32, y: u32) -> Option<String> {
        let slice = self.current.as_deref()?;
        let (tw, th) = (self.texture.texture.width().max(1) as usize, self.texture.texture.height().max(1) as usize);
        let sx = (x as usize * slice.width / tw).min(slice.width - 1);
        let sy = (y as usize * slice.height / th).min(slice.height - 1);
        let idx = slice.voxel(sx, sy);
        let v = &self.volume;
        let place: Vec<String> = v
            .axes
            .iter()
            .enumerate()
            .filter(|(i, a)| {
                !matches!(a.role, Role::Color | Role::Channel) && slice.projected_axis != Some(*i) && a.len > 1
            })
            .map(|(i, a)| {
                let name = match a.role {
                    Role::X => "x",
                    Role::Y => "y",
                    Role::Z => "z",
                    Role::T => "t",
                    _ => "",
                };
                if name.is_empty() { format!("{}", idx[i]) } else { format!("{name} {}", idx[i]) }
            })
            .collect();
        let value = if slice.samples == 1 {
            let x = slice.value(sx, sy, 0)?;
            if x.is_nan() {
                "no data".to_string()
            } else {
                format!(
                    "{}{}",
                    number(x as f64),
                    if v.value_name.is_empty() { String::new() } else { format!(" {}", v.value_name) }
                )
            }
        } else if v.channels.len() == slice.samples {
            (0..slice.samples)
                .map(|s| format!("{} {}", v.channels[s].name, number(slice.value(sx, sy, s).unwrap_or(0.0) as f64)))
                .collect::<Vec<_>>()
                .join(", ")
        } else {
            (0..slice.samples)
                .map(|s| number(slice.value(sx, sy, s).unwrap_or(0.0) as f64))
                .collect::<Vec<_>>()
                .join(" ")
        };
        // Where that is in the body or the specimen, when the file says how big voxels are.
        let mm: Vec<String> = v
            .axes
            .iter()
            .enumerate()
            .filter(|(i, a)| {
                a.spacing > 0.0 && matches!(a.role, Role::X | Role::Y | Role::Z) && slice.projected_axis != Some(*i)
            })
            .map(|(i, a)| format!("{:.1}", idx[i] as f64 * a.spacing))
            .collect();
        let at =
            if mm.is_empty() || v.unit.is_empty() { String::new() } else { format!(" ({} {})", mm.join(", "), v.unit) };
        Some(format!("{}: {value}{at}", place.join(", ")))
    }

    /// The text over the view.
    pub fn overlay_parts(&self) -> Vec<String> {
        let mut parts = Vec::new();
        if let Some(e) = &self.error {
            parts.push(e.clone());
        }
        if !self.has_picture() {
            return parts;
        }
        let names = if self.oriented() { self.kind.name().to_string() } else { unoriented_name(self.kind).to_string() };
        parts.push(if self.mip {
            format!("{names} maximum projection")
        } else if self.slice_count() > 1 {
            format!("{names} {} / {}", self.slice + 1, self.slice_count())
        } else {
            names
        });
        if self.time_count() > 1 {
            parts.push(format!("Volume {} / {}", self.t + 1, self.time_count()));
        }
        if self.pending.is_some() {
            parts.push("reading…".into());
        }
        let multi = self.current.as_ref().is_some_and(|s| s.samples > 1);
        if !multi || self.uses_window() {
            parts.push(format!("C {} W {}", number(self.window.0), number(self.window.1)));
        }
        parts
    }

    /// Whether one window serves the plane (a single value or RGB), rather than
    /// a window for each channel.
    fn uses_window(&self) -> bool {
        !(self.volume.channels.len() > 1
            && self.current.as_ref().is_some_and(|s| s.samples == self.volume.channels.len()))
    }

    fn is_grey_layout(&self) -> bool {
        self.current.as_ref().is_some_and(|s| s.samples == 1)
    }

    /// After a reload: the same plane, window and colours as before.
    pub fn carry_over(&mut self, old: &VolumeView) {
        if self.volume.shape() == old.volume.shape() && self.volume.axes.len() == old.volume.axes.len() {
            self.kind = old.kind;
            self.slice = old.slice.min(self.slice_count().saturating_sub(1));
            self.t = old.t.min(self.time_count().saturating_sub(1));
            self.mip = old.mip;
            self.neurological = old.neurological;
            self.map = old.map.filter(|_| self.map.is_some());
            self.window = old.window;
            self.window_name = old.window_name.clone();
            self.cmap = old.cmap;
            self.invert = old.invert;
            if self.channels.len() == old.channels.len() {
                self.channels = old.channels.clone();
            }
            self.dirty_plane = true;
        }
        self.texture.params = old.texture.params;
        self.texture.fit = old.texture.fit;
        self.playing = old.playing;
    }

    /// Statistics, for the inspector.
    pub fn stats(&self) -> Option<&Stats> {
        self.stats.as_ref()
    }
}

fn unoriented_name(kind: PlaneKind) -> &'static str {
    match kind {
        PlaneKind::Axial => "XY",
        PlaneKind::Coronal => "XZ",
        PlaneKind::Sagittal => "YZ",
    }
}

/// Pan, zoom, scrolling through slices, window dragging and drawing.  `drag`
/// is the button and movement in points; `ctrl` makes a left drag move the
/// window.  True while it needs more frames.
#[allow(clippy::too_many_arguments)]
pub fn drive(
    v: &mut VolumeView,
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
    if let Some((button, delta)) = drag {
        let delta = egui::vec2(delta.x, delta.y) * ppp;
        if button == PointerButton::Secondary || (button == PointerButton::Primary && ctrl) {
            if v.uses_window() {
                let (c, w) = dragged_window(v.window, v.range, delta);
                v.set_window((c, w), "Custom");
            }
        } else {
            v.texture.pan(delta);
        }
    }
    let moving = v.update(gfx, dt);
    let at = |p: egui::Pos2| (p - rect.min) * ppp;
    v.texture.set_size(size);
    if response.hovered() {
        // The wheel turns through slices; Ctrl and the wheel (or a pinch) zooms.
        if scroll != 0.0 {
            v.scroll += scroll;
            let whole = v.scroll.trunc();
            if whole != 0.0 {
                v.scroll -= whole;
                if !v.step_slice(-(whole as isize)) {
                    v.scroll = 0.0;
                }
            }
        }
        if zoom != 1.0
            && let Some(p) = pointer
        {
            v.texture.zoom_about(zoom, at(p));
        }
    }
    if response.double_clicked() {
        if v.texture.fit { v.texture.actual_size() } else { v.texture.fit_now() }
    }
    if let (Some(p), true) = (pointer, response.hovered())
        && let Some((x, y, _, _)) = v.texture.texel_at(at(p))
    {
        *hover = v.readout(x, y);
    }
    v.texture.draw(gfx);
    moving
}

/// The letters at the edges of the picture that say which way is which.
pub fn overlay(painter: &egui::Painter, rect: egui::Rect, v: &VolumeView) {
    let Some(slice) = &v.current else { return };
    let font = egui::FontId::proportional(15.0);
    let color = egui::Color32::from_rgb(255, 220, 120);
    let mid = rect.center();
    let spots = [
        (slice.edges[0], egui::pos2(rect.left() + 12.0, mid.y), egui::Align2::LEFT_CENTER),
        (slice.edges[1], egui::pos2(rect.right() - 12.0, mid.y), egui::Align2::RIGHT_CENTER),
        (slice.edges[2], egui::pos2(mid.x, rect.top() + 34.0), egui::Align2::CENTER_TOP),
        (slice.edges[3], egui::pos2(mid.x, rect.bottom() - 12.0), egui::Align2::CENTER_BOTTOM),
    ];
    for (letter, pos, anchor) in spots {
        if let Some(c) = letter {
            painter.text(pos, anchor, c.to_string(), font.clone(), color);
        }
    }
}

//endregion

/*┌─────────────────╮
| Panels          |
└─────────────────╯*/
//region Panels

fn section(ui: &mut Ui, title: &str, open: bool, add: impl FnOnce(&mut Ui)) {
    crate::sections::section(ui, title, title, open, add);
}

/// A file's parts as a picker: an `.npz`'s arrays, an HDF5 file's datasets, a
/// slide's levels, label and macro picture.
pub fn parts_controls(ui: &mut Ui, names: &[String], current: &str, kind: &str, actions: &mut Vec<Action>) {
    if names.len() < 2 {
        return;
    }
    section(ui, kind, true, |ui| {
        let mut pick = current.to_string();
        let items = names.iter().map(|n| (n.clone(), n.clone()));
        widgets::Choice::new("volume-part", &mut pick, items).width(230.0).show(ui);
        if pick != current {
            actions.push(Action::OpenPart(Part::Named(pick)));
        }
    });
}

/// What a file's parts are called in the panel.
pub fn parts_title(f: VolumeFormat) -> &'static str {
    match f {
        VolumeFormat::Npz => "Arrays",
        VolumeFormat::Hdf5 | VolumeFormat::NetCdf => "Datasets",
        VolumeFormat::Tiff => "Levels and pages",
        _ => "Parts",
    }
}

/// The right-hand panel's sections for a volume.
pub fn controls(ui: &mut Ui, v: &mut VolumeView) {
    if let Some(e) = &v.error {
        ui.label(egui::RichText::new(e).color(ui.visuals().warn_fg_color));
    }
    if !v.has_picture() {
        ui.label(v.volume.no_picture.clone().unwrap_or_default());
        ui.label(
            egui::RichText::new("There's no picture of this array; its statistics are shown instead.").small().weak(),
        );
        return;
    }
    section(ui, "Volume", true, |ui| plane_controls(ui, v));
    if v.uses_window() {
        section(ui, "Window", true, |ui| window_section(ui, v));
    } else {
        section(ui, "Channels", true, |ui| channel_section(ui, v));
    }
}

fn plane_controls(ui: &mut Ui, v: &mut VolumeView) {
    let oriented = v.oriented();
    // Which plane: axial, coronal, sagittal.
    if v.can_switch_planes() {
        ui.horizontal(|ui| {
            let mut kind = v.kind;
            let options: Vec<(PlaneKind, &str)> =
                PlaneKind::ALL.iter().map(|&k| (k, if oriented { k.name() } else { unoriented_name(k) })).collect();
            if widgets::radio_group(ui, &mut kind, &options).changed() {
                v.set_plane(kind);
            }
        });
    }
    // For arrays that don't say which axis holds the slices.
    let choices = v.volume.slice_axis_choices();
    if choices.len() > 1 && v.map.is_some() {
        let current = choices.iter().position(|(_, m)| Some(m.z) == v.map.map(|c| c.z)).unwrap_or(0);
        let mut pick = current;
        ui.horizontal(|ui| {
            ui.label("Slice axis:");
            let items = choices.iter().enumerate().map(|(i, (n, _))| (i, n.clone()));
            widgets::Choice::new("volume-slice-axis", &mut pick, items).width(100.0).show(ui);
        });
        if pick != current {
            v.map = Some(choices[pick].1);
            v.kind = PlaneKind::Axial;
            v.slice = v.slice_count() / 2;
            v.t = 0;
            v.dirty_plane = true;
            v.texture.fit = true;
        }
        ui.label(
            egui::RichText::new("The file doesn't say which axis is the slice axis; this is a guess.").small().weak(),
        );
    }
    let n = v.slice_count();
    if n > 1 {
        let mut s = v.slice;
        if counter_slider(ui, "Slice ", &mut s, n) {
            v.playing = false;
            v.set_slice(s);
        }
    }
    let nt = v.time_count();
    if nt > 1 {
        let mut t = v.t;
        if counter_slider(ui, "Volume ", &mut t, nt) {
            v.playing = false;
            v.set_time(t);
        }
    }
    if n > 1 || nt > 1 {
        if ui.button(if v.playing { "Pause" } else { "Play" }).clicked() {
            v.toggle_play();
        }
        let hint = "The wheel, Page Up and Page Down step through the slices, or the volumes when there is one slice.";
        ui.label(egui::RichText::new(hint).small().weak());
    }
    if oriented {
        let mut neuro = v.neurological;
        if ui.add(widgets::CheckBox::new(&mut neuro, "Left on the left (neurological)")).changed() {
            v.neurological = neuro;
            v.dirty_plane = true;
        }
    }
    if v.can_project() {
        let mut mip = v.mip;
        if ui.add(widgets::CheckBox::new(&mut mip, "Maximum intensity projection")).changed() {
            v.mip = mip;
            v.dirty_plane = true;
        }
    }
}

/// The presets a volume offers for its window.
fn presets(v: &VolumeView) -> Vec<(String, (f64, f64))> {
    let cw = |lo: f64, hi: f64| ((lo + hi) / 2.0, (hi - lo).max(1e-9));
    let mut out = Vec::new();
    if let Some(s) = &v.stats {
        if s.p99 > s.p1 {
            out.push(("Volume 1–99 %".to_string(), cw(s.p1, s.p99)));
        }
        if s.max > s.min {
            out.push(("Full range".to_string(), cw(s.min, s.max)));
        }
    }
    if let Some(slice) = &v.current {
        let (lo, hi) = percentile_window(&slice.data, 1.0, 99.0);
        out.push(("Slice 1–99 %".to_string(), cw(lo as f64, hi as f64)));
        let (lo, hi) = percentile_window(&slice.data, 0.0, 100.0);
        out.push(("Slice range".to_string(), cw(lo as f64, hi as f64)));
    }
    if let Some((lo, hi)) = v.volume.display_range {
        out.push(("File's range".to_string(), cw(lo, hi)));
    }
    if let Some((lo, hi)) = v.volume.dtype.integer_range()
        && v.volume.dtype.size() <= 2
    {
        out.push((format!("Type's range ({})", v.volume.dtype.name()), cw(lo as f64, hi as f64)));
    }
    out
}

fn window_section(ui: &mut Ui, v: &mut VolumeView) {
    let choices = presets(v);
    match window_controls(ui, "volume", &choices, &v.window_name, v.window, v.range) {
        Some(WindowEdit::Preset(name, w)) => v.set_window(w, name),
        Some(WindowEdit::Custom { center, width }) => v.set_window((center, width), "Custom"),
        None => {}
    }
    ui.label(egui::RichText::new(crate::keys("Drag with the right mouse button (or Ctrl) to adjust.")).small().weak());
    if v.is_grey_layout() {
        egui::Grid::new("volume-map").num_columns(2).spacing([8.0, 4.0]).show(ui, |ui| {
            ui.label("Colours:");
            let mut m = v.cmap;
            widgets::Choice::new("volume-colormap", &mut m, ColorMap::ALL.iter().map(|&c| (c, c.name().to_string())))
                .width(170.0)
                .show(ui);
            if m != v.cmap {
                v.cmap = m;
                v.dirty_draw = true;
            }
            ui.end_row();
        });
        let mut invert = v.invert;
        if ui.add(widgets::CheckBox::new(&mut invert, "Invert")).changed() {
            v.invert = invert;
            v.dirty_draw = true;
        }
    }
}

fn channel_section(ui: &mut Ui, v: &mut VolumeView) {
    let names: Vec<String> = v.volume.channels.iter().map(|c| c.name.clone()).collect();
    let mut changed = false;
    for (i, ch) in v.channels.iter_mut().enumerate() {
        ui.horizontal(|ui| {
            changed |= ui
                .add(widgets::CheckBox::new(
                    &mut ch.visible,
                    names.get(i).cloned().unwrap_or_else(|| format!("Channel {}", i + 1)),
                ))
                .changed();
            let mut rgb = ch.color;
            if ui.color_edit_button_srgb(&mut rgb).changed() {
                ch.color = rgb;
                changed = true;
            }
        });
        ui.horizontal(|ui| {
            ui.add_space(24.0);
            let speed = ((ch.window.1 - ch.window.0).abs().max(1.0) / 300.0) as f64;
            let (mut lo, mut hi) = (ch.window.0 as f64, ch.window.1 as f64);
            let a = ui.add(egui::DragValue::new(&mut lo).speed(speed).prefix("from "));
            let b = ui.add(egui::DragValue::new(&mut hi).speed(speed).prefix("to "));
            if a.changed() || b.changed() {
                ch.window = (lo as f32, (hi as f32).max(lo as f32 + 1e-6));
                changed = true;
            }
        });
    }
    if ui.button("Auto brightness").clicked()
        && let Some(slice) = v.current.clone()
    {
        let n = slice.samples;
        for (c, ch) in v.channels.iter_mut().enumerate() {
            let vals: Vec<f32> = slice.data.iter().skip(c).step_by(n).copied().collect();
            ch.window = percentile_window(&vals, 1.0, 99.5);
        }
        changed = true;
    }
    if changed {
        v.dirty_draw = true;
    }
}

/// Statistics for the left-hand panel: what the values are, and their
/// histogram.
pub fn inspector(ui: &mut Ui, v: &VolumeView) {
    let Some(s) = v.stats() else { return };
    section(ui, "Values", true, |ui| {
        egui::Grid::new("volume-stats").num_columns(2).spacing([8.0, 2.0]).show(ui, |ui| {
            let unit = if v.volume.value_name.is_empty() { String::new() } else { format!(" {}", v.volume.value_name) };
            for (k, val) in [
                ("Type", v.volume.dtype.name().to_string()),
                ("Lowest", format!("{}{unit}", number(s.min))),
                ("1st percentile", number(s.p1)),
                ("Median", number(s.median)),
                ("99th percentile", number(s.p99)),
                ("Highest", format!("{}{unit}", number(s.max))),
                ("Mean", number(s.mean)),
                ("Spread (std)", number(s.std)),
            ] {
                ui.label(egui::RichText::new(k).weak());
                ui.label(val);
                ui.end_row();
            }
            if s.nan > 0 {
                ui.label(egui::RichText::new("No data").weak());
                ui.label(format!("{} of {} sampled", s.nan, s.sampled));
                ui.end_row();
            }
        });
        if s.sampled < s.total {
            ui.label(
                egui::RichText::new(format!("From {} of {} values, spread evenly.", s.sampled, s.total)).small().weak(),
            );
        }
        histogram(ui, s, 70.0);
    });
}

/// Bars for the histogram of a statistics sample, log-scaled so rare values
/// still show.
fn histogram(ui: &mut Ui, s: &Stats, height: f32) {
    if s.histogram.is_empty() {
        return;
    }
    let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), height), egui::Sense::hover());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 2.0, ui.visuals().extreme_bg_color);
    let peak = (*s.histogram.iter().max().unwrap_or(&1) as f32 + 1.0).ln();
    let n = s.histogram.len() as f32;
    let color = ui.visuals().selection.bg_fill;
    for (i, &c) in s.histogram.iter().enumerate() {
        let h = ((c as f32 + 1.0).ln() / peak.max(1e-6)) * (height - 4.0);
        let x0 = rect.left() + rect.width() * i as f32 / n;
        let x1 = rect.left() + rect.width() * (i as f32 + 1.0) / n;
        painter.rect_filled(
            egui::Rect::from_min_max(
                egui::pos2(x0 + 0.5, rect.bottom() - 2.0 - h),
                egui::pos2(x1 - 0.5, rect.bottom() - 2.0),
            ),
            0.0,
            color,
        );
    }
}

/// The central area of an array that has no picture: its statistics, and for
/// a one-dimensional array its plot.
pub fn stats_view(ui: &mut Ui, v: &VolumeView) {
    let rect = ui.max_rect();
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 0.0, ui.visuals().extreme_bg_color);
    let text = ui.visuals().text_color();
    let weak = ui.visuals().weak_text_color();
    let shape = v.volume.shape().iter().map(|s| s.to_string()).collect::<Vec<_>>().join(" × ");
    painter.text(
        rect.left_top() + egui::vec2(24.0, 22.0),
        egui::Align2::LEFT_TOP,
        format!("{} of {}", v.volume.dtype.name(), shape),
        egui::FontId::proportional(20.0),
        text,
    );
    painter.text(
        rect.left_top() + egui::vec2(24.0, 50.0),
        egui::Align2::LEFT_TOP,
        v.volume.no_picture.clone().unwrap_or_default(),
        egui::FontId::proportional(13.0),
        weak,
    );
    let area = egui::Rect::from_min_max(
        rect.left_top() + egui::vec2(24.0, 90.0),
        rect.right_bottom() - egui::vec2(24.0, 24.0),
    );
    if area.width() < 50.0 || area.height() < 50.0 {
        return;
    }
    if let Some(plot) = &v.plot {
        let (lo, hi) = plot.iter().fold((f32::MAX, f32::MIN), |(a, b), p| (a.min(p.0), b.max(p.1)));
        let span = (hi - lo).max(1e-9);
        let n = plot.len().max(2) as f32 - 1.0;
        let line = egui::Stroke::new(1.5, ui.visuals().selection.bg_fill);
        let pt = |i: usize, y: f32| {
            egui::pos2(area.left() + area.width() * i as f32 / n, area.bottom() - area.height() * (y - lo) / span)
        };
        for (i, (a, b)) in plot.iter().enumerate() {
            painter.line_segment([pt(i, *a), pt(i, *b)], line);
            if i > 0 {
                painter.line_segment([pt(i - 1, (plot[i - 1].0 + plot[i - 1].1) / 2.0), pt(i, (a + b) / 2.0)], line);
            }
        }
        painter.rect_stroke(area, 0.0, egui::Stroke::new(1.0, weak), egui::StrokeKind::Outside);
        painter.text(
            area.left_top() - egui::vec2(0.0, 2.0),
            egui::Align2::LEFT_BOTTOM,
            number(hi as f64),
            egui::FontId::proportional(12.0),
            weak,
        );
        painter.text(
            area.left_bottom() + egui::vec2(0.0, 2.0),
            egui::Align2::LEFT_TOP,
            number(lo as f64),
            egui::FontId::proportional(12.0),
            weak,
        );
    } else if let Some(s) = &v.stats {
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(area));
        child.label(egui::RichText::new("Distribution of the values").weak());
        histogram(&mut child, s, (area.height() - 30.0).min(240.0));
    }
}

//endregion

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn planes_have_names_whether_or_not_the_file_says_how_they_lie() {
        assert_eq!(unoriented_name(PlaneKind::Axial), "XY");
        assert_eq!(unoriented_name(PlaneKind::Sagittal), "YZ");
        assert_eq!(parts_title(VolumeFormat::Npz), "Arrays");
        assert_eq!(parts_title(VolumeFormat::Hdf5), "Datasets");
    }

    #[test]
    fn a_style_picks_its_layout_from_the_samples() {
        let slice = |samples: usize| Slice {
            width: 1,
            height: 1,
            samples,
            data: vec![0.0; samples],
            col_spacing: 0.0,
            row_spacing: 0.0,
            step: 1,
            edges: [None; 4],
            col_axis: 0,
            row_axis: 0,
            fixed: vec![0; 2],
            flip_rows: false,
            flip_cols: false,
            rows: 1,
            cols: 1,
            projected_axis: None,
        };
        let one = style_of(&slice(1), (50.0, 100.0), ColorMap::Gray, false, &[], false);
        assert!(matches!(one.layout, Layout::Single { .. }));
        assert_eq!(one.window, (0.0, 100.0));
        assert!(matches!(style_of(&slice(3), (0.5, 1.0), ColorMap::Gray, false, &[], false).layout, Layout::Rgb));
        let chans = vec![ChannelStyle { color: [255, 0, 0], window: (0.0, 1.0), visible: true }; 3];
        assert!(matches!(
            style_of(&slice(3), (0.5, 1.0), ColorMap::Gray, false, &chans, true).layout,
            Layout::Channels(_)
        ));
    }
}
