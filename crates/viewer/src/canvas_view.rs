//! Sprite, map and animation mode: an Aseprite sprite, a Tiled map or an
//! animated picture (GIF, APNG, animated WebP, AVIF sequence, JPEG XL
//! animation) shown flat with the texture mode's pan and zoom, plus
//! Aseprite's tags played as clips (with their frame durations and
//! directions), a layer list to show or hide layers, and for animated
//! pictures a frame slider, play and pause, speed and a loop switch.
//!
//! Frames are composed on the CPU by the format's crate and uploaded as a
//! texture.  Small sprites compose in microseconds, so frames are composed
//! as they come up and kept while the layers stay the same; large maps
//! compose on a thread, so toggling a layer never freezes the window.

use crate::gfx::{Gfx, next_id};
use crate::loader::{Task, Waker, poll, spawn};
use crate::slice_view::counter_slider;
use crate::texture_view::TextureView;
use slerp_render::GpuTexture;
use slerp_scene::{Canvas, ColorSpace, Image8, PlayDirection, Texture};
use slerp_theme::widgets;
use std::sync::Arc;

/// The largest side a canvas is composed at for the viewer: GPU textures
/// are at most 8192 pixels on a side everywhere, and 32 million pixels
/// keep a map's memory reasonable.
const MAX_SIDE: f32 = 8192.0;
const MAX_PIXELS: f32 = 32.0 * 1024.0 * 1024.0;

/// Compositions above this many pixels happen on a thread.
const ASYNC_PIXELS: f32 = 1024.0 * 1024.0;

/// Frames kept on the GPU at most, in bytes.
const CACHE_BYTES: u64 = 256 << 20;

/// The scale the viewer composes `canvas` at: full size unless that is too
/// big for a texture.
pub fn viewer_scale(canvas: &dyn Canvas) -> f32 {
    let (w, h) = canvas.size();
    let (w, h) = (w.max(1) as f32, h.max(1) as f32);
    (MAX_SIDE / w.max(h)).min((MAX_PIXELS / (w * h)).sqrt()).min(1.0)
}

fn texture_of(img: Image8) -> Arc<Texture> {
    Arc::new(Texture::from_rgba8(img.width, img.height, img.pixels, ColorSpace::Srgb))
}

/*┌─────────────────╮
| Playing         |
└─────────────────╯*/
//region Playing

/// Plays a clip's frames: the order its direction gives, each for its own
/// duration, repeated as many times as the clip says or for ever.
pub struct FramePlayer {
    /// The clip playing, or `None` for every frame in order.
    pub clip: Option<usize>,
    pub playing: bool,
    pub speed: f32,
    /// Loop even when the clip asks for a number of repeats, or the file
    /// (a GIF, say) for a number of plays.
    pub looping: bool,
    /// The sequence is the picture's own number of plays, not one pass:
    /// when it ends and `looping` is off, playing stops.
    finite: bool,
    /// Frames in playing order, and where in it we are.
    sequence: Vec<usize>,
    position: usize,
    /// Seconds into the current frame.
    time: f32,
    /// When time last moved on, while playing.  Playback keeps its own
    /// clock so the window can sleep until the next frame is due.
    clock: Option<std::time::Instant>,
}

impl FramePlayer {
    fn new(canvas: &dyn Canvas) -> FramePlayer {
        let mut p = FramePlayer {
            clip: None,
            playing: false,
            speed: 1.0,
            looping: true,
            finite: false,
            sequence: Vec::new(),
            position: 0,
            time: 0.0,
            clock: None,
        };
        p.select(canvas, None);
        // Animations start playing, as they do in Aseprite's preview.
        p.playing = canvas.frames().len() > 1;
        p
    }

    /// Plays `clip` (or every frame) from its start.
    pub fn select(&mut self, canvas: &dyn Canvas, clip: Option<usize>) {
        let n = canvas.frames().len();
        let clip = clip.filter(|&c| c < canvas.clips().len());
        self.clip = clip;
        // Without a clip: every frame, once, or as many times as the file
        // asks for when looping is off.
        let plays = canvas.loops().filter(|_| !self.looping && n > 1).map(|p| p.clamp(1, 1000) as usize);
        self.finite = clip.is_none() && plays.is_some();
        self.sequence = match clip.map(|c| &canvas.clips()[c]) {
            None => (0..n).collect::<Vec<_>>().repeat(plays.unwrap_or(1)),
            Some(c) => {
                sequence(c.from, c.to.min(n.saturating_sub(1)), c.direction, if self.looping { 0 } else { c.repeat })
            }
        };
        if self.sequence.is_empty() {
            self.sequence.push(0);
        }
        self.position = 0;
        self.time = 0.0;
        self.playing = n > 1;
    }

    /// The frame showing.
    pub fn frame(&self) -> usize {
        self.sequence[self.position.min(self.sequence.len() - 1)]
    }

    /// Moves time on to now.  While playing, returns how long until the
    /// next frame is due.
    fn advance(&mut self, durations: &[f32]) -> Option<std::time::Duration> {
        if !self.playing || self.sequence.len() < 2 {
            self.clock = None;
            return None;
        }
        let now = std::time::Instant::now();
        // After a pause in drawing (a minimised window), carry on rather
        // than racing through the frames missed.
        let dt = self.clock.map_or(0.0, |c| (now - c).as_secs_f32().min(0.25));
        self.clock = Some(now);
        self.time += dt * self.speed;
        // A frame of zero length still shows for a moment, so a broken
        // file can't spin this loop.
        for _ in 0..1000 {
            let d = durations.get(self.frame()).copied().unwrap_or(0.1).max(0.005);
            if self.time < d {
                break;
            }
            self.time -= d;
            if self.position + 1 < self.sequence.len() {
                self.position += 1;
            } else if self.looping || (self.clip.is_none() && !self.finite) {
                self.position = 0;
            } else {
                self.playing = false;
                self.time = 0.0;
                self.clock = None;
                return None;
            }
        }
        let d = durations.get(self.frame()).copied().unwrap_or(0.1).max(0.005);
        Some(std::time::Duration::from_secs_f32(((d - self.time) / self.speed.max(0.01)).max(0.001)))
    }

    /// Carries on where `old` was, after the file was reloaded: the same
    /// clip (by name), place in it, speed and play state.
    pub fn keep_from(&mut self, canvas: &dyn Canvas, old: &FramePlayer, old_canvas: &dyn Canvas) {
        let name = old.clip.and_then(|c| old_canvas.clips().get(c)).map(|c| c.name.as_str());
        let clip = name.and_then(|n| canvas.clips().iter().position(|c| c.name == n));
        self.looping = old.looping;
        self.speed = old.speed;
        self.select(canvas, clip);
        // The same clip, or every frame both times.
        if clip.is_some() == old.clip.is_some() {
            self.position = old.position.min(self.sequence.len() - 1);
            self.time = old.time;
        }
        self.playing = old.playing && self.sequence.len() > 1;
    }

    /// Steps `delta` frames along the sequence and pauses.
    pub fn step(&mut self, delta: isize) {
        let n = self.sequence.len() as isize;
        self.position = (self.position as isize + delta).rem_euclid(n.max(1)) as usize;
        self.time = 0.0;
        self.playing = false;
    }

    /// Shows `frame` (an index into the picture's frames) and pauses: at its
    /// first place in the sequence playing, or, when the clip doesn't have
    /// it, in the sequence of every frame.
    pub fn seek(&mut self, canvas: &dyn Canvas, frame: usize) {
        if !self.sequence.contains(&frame) {
            self.select(canvas, None);
        }
        self.position = self.sequence.iter().position(|&f| f == frame).unwrap_or(0);
        self.time = 0.0;
        self.playing = false;
    }

    /// Plays from the start if a clip with repeats has finished, or
    /// toggles play and pause.
    pub fn play_pause(&mut self) {
        if !self.playing && self.position + 1 >= self.sequence.len() && !self.looping {
            self.position = 0;
            self.playing = true;
        } else {
            self.playing = !self.playing;
        }
    }
}

/// The frames from `from` to `to` in the order `direction` plays them,
/// `repeat` times (each way of a ping-pong counting once), or one cycle to
/// loop when `repeat` is 0.
pub fn sequence(from: usize, to: usize, direction: PlayDirection, repeat: u32) -> Vec<usize> {
    if to < from {
        return vec![from];
    }
    let up: Vec<usize> = (from..=to).collect();
    let down: Vec<usize> = (from..=to).rev().collect();
    let passes = repeat.clamp(1, 1000) as usize;
    match direction {
        PlayDirection::Forward => up.repeat(if repeat == 0 { 1 } else { passes }),
        PlayDirection::Reverse => down.repeat(if repeat == 0 { 1 } else { passes }),
        PlayDirection::PingPong | PlayDirection::PingPongReverse => {
            let first_up = direction == PlayDirection::PingPong;
            if from == to {
                return vec![from];
            }
            if repeat == 0 {
                // One loop: there and back, without doubling either end.
                let (a, b) = if first_up { (&up, &down) } else { (&down, &up) };
                return a.iter().chain(&b[1..b.len() - 1]).copied().collect();
            }
            let mut out = Vec::new();
            for pass in 0..passes {
                let forward = (pass % 2 == 0) == first_up;
                let run = if forward { &up } else { &down };
                // Each pass after the first starts where the last ended.
                out.extend(&run[if pass == 0 { 0 } else { 1 }..]);
            }
            out
        }
    }
}

//endregion

/*┌─────────────────╮
| The view        |
└─────────────────╯*/
//region The view

pub struct CanvasView {
    pub canvas: Arc<dyn Canvas>,
    /// Which layers show, one per layer.
    pub shown: Vec<bool>,
    pub player: FramePlayer,
    /// The composed frame on the GPU, with pan and zoom.
    pub texture: TextureView,
    pub scale: f32,
    /// What `texture` shows: the frame and layer choice.
    showing: (usize, u64),
    /// Composed frames for the current layer choice, other than the one
    /// showing (GPU textures move between here and `texture`).
    cache: Vec<Option<(Arc<Texture>, GpuTexture)>>,
    cached_bytes: u64,
    /// A composition running on a thread, and what it is of.
    pending: Option<((usize, u64), Task<Image8>)>,
    waker: Waker,
}

impl CanvasView {
    /// Shows `canvas`, whose first frame `first` the loading thread already
    /// composed at [`viewer_scale`].
    pub fn new(
        name: String,
        canvas: Arc<dyn Canvas>,
        first: Image8,
        gfx: &mut Gfx,
        waker: Waker,
    ) -> Result<CanvasView, String> {
        let shown: Vec<bool> = canvas.layers().iter().map(|l| l.visible).collect();
        let texture = TextureView::new(name, texture_of(first), gfx)?;
        let player = FramePlayer::new(&*canvas);
        let frames = canvas.frames().len();
        let mut view = CanvasView {
            scale: viewer_scale(&*canvas),
            canvas,
            showing: (0, 0),
            shown,
            player,
            texture,
            cache: (0..frames).map(|_| None).collect(),
            cached_bytes: 0,
            pending: None,
            waker,
        };
        view.showing = (0, view.layer_key());
        Ok(view)
    }

    /// A number standing for the layer choice.
    fn layer_key(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        self.shown.hash(&mut h);
        h.finish()
    }

    /// Forgets composed frames after the layer choice changed.
    pub fn layers_changed(&mut self) {
        for c in &mut self.cache {
            *c = None;
        }
        self.cached_bytes = 0;
    }

    /// Plays on, and puts the frame that should show on screen.  Playback
    /// keeps its own clock and wakes the window when the next frame is due,
    /// and compositions on a thread wake it when done, so this never asks
    /// for frames in between: `_dt` is unused and the result is always
    /// false.
    pub fn update(&mut self, gfx: &mut Gfx, _dt: f32) -> bool {
        if let Some(wait) = self.player.advance(self.canvas.frames())
            && let Some(ctx) = self.waker.get()
        {
            ctx.request_repaint_after(wait);
        }
        let want = (self.player.frame(), self.layer_key());
        // 1. A finished composition from the thread.
        if let Some((key, task)) = &self.pending
            && let Some(result) = poll(task)
        {
            let key = *key;
            self.pending = None;
            if let Ok(img) = result
                && key.1 == want.1
            {
                self.show(key.0, texture_of(img), gfx);
            }
        }
        if want == self.showing {
            return false;
        }
        // 2. A frame composed earlier.
        if let Some((tex, gpu)) = self.cache.get_mut(want.0).and_then(Option::take) {
            self.cached_bytes -= gpu.gpu_bytes();
            self.replace(tex, gpu, want);
            return false;
        }
        // 3. Compose it: here when small, on a thread when large.
        let (w, h) = self.canvas.size();
        let pixels = w as f32 * h as f32 * self.scale * self.scale;
        if pixels <= ASYNC_PIXELS {
            let img = self.canvas.compose(want.0, &self.shown, self.scale);
            self.show(want.0, texture_of(img), gfx);
        } else if self.pending.as_ref().is_none_or(|(k, _)| *k != want) {
            let (canvas, shown, scale) = (self.canvas.clone(), self.shown.clone(), self.scale);
            let task = spawn(&self.waker, "slerp-compose", move || Ok(canvas.compose(want.0, &shown, scale)));
            self.pending = Some((want, task));
        }
        false
    }

    fn show(&mut self, frame: usize, tex: Arc<Texture>, gfx: &mut Gfx) {
        match gfx.renderer.upload_texture(&gfx.gpu, &tex) {
            Ok(gpu) => self.replace(tex, gpu, (frame, self.layer_key())),
            Err(e) => eprintln!("slerp view: {e}"),
        }
    }

    /// Shows another picture of the same size, keeping pan and zoom, and
    /// keeps the one it replaces for later if its layers still apply.
    fn replace(&mut self, tex: Arc<Texture>, gpu: GpuTexture, key: (usize, u64)) {
        let old_tex = std::mem::replace(&mut self.texture.texture, tex);
        let old_gpu = std::mem::replace(&mut self.texture.gpu, gpu);
        self.texture.id = next_id();
        let (old_frame, old_layers) = std::mem::replace(&mut self.showing, key);
        let bytes = old_gpu.gpu_bytes();
        if old_layers == key.1
            && self.cached_bytes + bytes <= CACHE_BYTES
            && let Some(slot) = self.cache.get_mut(old_frame)
            && slot.is_none()
        {
            *slot = Some((old_tex, old_gpu));
            self.cached_bytes += bytes;
        }
    }

    pub fn is_map(&self) -> bool {
        let format = &self.canvas.info().format;
        format.starts_with("Tiled") || format.starts_with("LDtk")
    }

    pub fn summary(&self) -> String {
        let (w, h) = self.canvas.size();
        let n = self.canvas.frames().len();
        if n > 1 { format!("{w} × {h}, {n} frames") } else { format!("{w} × {h}") }
    }
}

//endregion

/*┌─────────────────╮
| Panel           |
└─────────────────╯*/
//region Panel

fn section(ui: &mut egui::Ui, title: &str, open: bool, add: impl FnOnce(&mut egui::Ui)) {
    crate::sections::section(ui, title, title, open, add);
}

/// A button with a theme icon, or `words` where there is no icon theme.
fn icon_button(ui: &mut egui::Ui, icon: &str, words: &str) -> egui::Response {
    match slerp_theme::icon(icon, 16) {
        Some(src) => ui.add(egui::Button::image(egui::Image::new(src).fit_to_exact_size(egui::vec2(16.0, 16.0)))),
        None => ui.button(words),
    }
}

fn direction_word(d: PlayDirection) -> &'static str {
    match d {
        PlayDirection::Forward => "forward",
        PlayDirection::Reverse => "reverse",
        PlayDirection::PingPong => "ping-pong",
        PlayDirection::PingPongReverse => "ping-pong reverse",
    }
}

/// The right-hand panel's controls: clips, frames and layers.
pub fn controls(ui: &mut egui::Ui, v: &mut CanvasView) {
    let canvas = v.canvas.clone();
    let frames = canvas.frames();
    if frames.len() > 1 {
        section(ui, "Animation", true, |ui| {
            ui.horizontal(|ui| {
                let text = if v.player.playing { "Pause" } else { "Play" };
                if ui.button(text).on_hover_text("Space").clicked() {
                    v.player.play_pause();
                }
                if icon_button(ui, "media-skip-backward", "Previous").on_hover_text("Previous frame (,)").clicked() {
                    v.player.step(-1);
                }
                if icon_button(ui, "media-skip-forward", "Next").on_hover_text("Next frame (.)").clicked() {
                    v.player.step(1);
                }
            });
            let frame = v.player.frame();
            let mut shown = frame;
            if counter_slider(ui, "Frame ", &mut shown, frames.len()) {
                v.player.seek(&*canvas, shown);
            }
            let ms = frames.get(frame).copied().unwrap_or(0.0) * 1000.0;
            ui.label(format!("Frame {} of {}, {ms:.0} ms", frame + 1, frames.len()));
            egui::Grid::new("canvas-anim").num_columns(2).spacing([8.0, 4.0]).show(ui, |ui| {
                ui.label("Speed:");
                widgets::slider(
                    ui,
                    egui::Slider::new(&mut v.player.speed, 0.1..=4.0).logarithmic(true).suffix("×").fixed_decimals(2),
                );
                ui.end_row();
            });
            let mut looping = v.player.looping;
            if ui
                .add(widgets::CheckBox::new(&mut looping, "Loop"))
                .on_hover_text(
                    "Loop for ever, even when a tag asks for a number of repeats or the file for a number of plays",
                )
                .changed()
            {
                v.player.looping = looping;
                let clip = v.player.clip;
                v.player.select(&*canvas, clip);
            }
            // Pictures that are only an animation (GIF, APNG, WebP) have no tags.
            if !canvas.clips().is_empty() {
                ui.add_space(4.0);
                ui.label(egui::RichText::new("Tags").text_style(egui::TextStyle::Name("Bold".into())));
                // Row 0 is "All frames", row i + 1 the tag i.
                let mut pick = None;
                let mut list = widgets::List::new(ui, "sprite-tags");
                let all = format!("All frames  (1–{})", frames.len());
                if list.row(ui, 0, v.player.clip.is_none(), all).clicked() {
                    pick = Some(None);
                }
                for (i, c) in canvas.clips().iter().enumerate() {
                    let repeat = if c.repeat > 0 { format!(", ×{}", c.repeat) } else { String::new() };
                    let text =
                        format!("{}  ({}–{}, {}{repeat})", c.name, c.from + 1, c.to + 1, direction_word(c.direction));
                    if list.row(ui, i + 1, v.player.clip == Some(i), text).clicked() {
                        pick = Some(Some(i));
                    }
                }
                if let Some(row) = list.finish(ui) {
                    pick = Some(row.checked_sub(1));
                }
                if let Some(clip) = pick {
                    v.player.select(&*canvas, clip);
                }
            }
        });
    }
    let layers = canvas.layers();
    if !layers.is_empty() {
        section(ui, "Layers", true, |ui| {
            ui.horizontal(|ui| {
                if ui.button("Show All").clicked() {
                    v.shown.iter_mut().for_each(|s| *s = true);
                    v.layers_changed();
                }
                if ui.button("As Saved").clicked() {
                    v.shown = layers.iter().map(|l| l.visible).collect();
                    v.layers_changed();
                }
            });
            let mut changed = false;
            // Top layers first, as Aseprite and Tiled list them.
            egui::ScrollArea::vertical().id_salt("canvas-layers").max_height(420.0).show(ui, |ui| {
                for (i, l) in layers.iter().enumerate().rev() {
                    ui.horizontal(|ui| {
                        ui.add_space(l.depth as f32 * 14.0);
                        let name = if l.name.is_empty() { format!("Layer {}", i + 1) } else { l.name.clone() };
                        changed |= ui.add(widgets::CheckBox::new(&mut v.shown[i], name)).changed();
                        if !l.note.is_empty() {
                            ui.label(egui::RichText::new(&l.note).weak().small());
                        }
                    });
                }
            });
            if changed {
                v.layers_changed();
            }
        });
    }
    section(ui, "View", false, |ui| {
        ui.add(widgets::CheckBox::new(&mut v.texture.params.checkerboard, "Checkerboard behind transparency"));
        if v.scale < 1.0 {
            ui.label(format!("Shown at {:.0} % of full size, the largest a texture holds.", v.scale * 100.0));
        }
    });
}

//endregion

#[cfg(test)]
mod tests {
    use super::*;
    use PlayDirection::*;

    #[test]
    fn clip_orders() {
        assert_eq!(sequence(2, 4, Forward, 0), [2, 3, 4]);
        assert_eq!(sequence(2, 4, Forward, 2), [2, 3, 4, 2, 3, 4]);
        assert_eq!(sequence(2, 4, Reverse, 0), [4, 3, 2]);
        assert_eq!(sequence(0, 3, PingPong, 0), [0, 1, 2, 3, 2, 1]);
        assert_eq!(sequence(0, 3, PingPongReverse, 0), [3, 2, 1, 0, 1, 2]);
        // Three passes of a ping-pong: there, back, there again.
        assert_eq!(sequence(0, 2, PingPong, 3), [0, 1, 2, 1, 0, 1, 2]);
        assert_eq!(sequence(5, 5, PingPong, 0), [5]);
    }
}
