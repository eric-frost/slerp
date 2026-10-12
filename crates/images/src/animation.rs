//! Animated pictures (GIF, APNG, animated WebP and the animated sequences of
//! AVIF and JPEG XL) as a [`Canvas`] the viewer already knows how to play.
//!
//! The formats store most frames as a patch: a rectangle of pixels with an
//! offset, a way to blend it onto what is there and a way to dispose of it
//! before the next frame.  A [`Compositor`] replays the patches onto a
//! canvas the size of the picture.  Showing frame 7 means replaying
//! frames 0 to 7, so [`AnimatedImage`] keeps checkpoints of the canvas along
//! the way and the frames themselves while they fit a memory budget; stepping
//! back or scrubbing starts from the nearest checkpoint instead of the
//! beginning, and playing forwards never decodes a frame twice.

use crate::scale::{box_shrink, scaled_size};
use slerp_scene::{Canvas, CanvasLayer, FrameClip, Image8, Info};
use std::sync::{Arc, Mutex};

/// Animation canvases are limited to this many pixels, which is also what
/// the viewer's canvas view will show at full size (a texture is at most
/// 8192 pixels across).  A hostile file can claim a logical screen of
/// 65535 by 65535; no honest animation is that big.
pub const MAX_CANVAS_PIXELS: u64 = 32 << 20;

/// The most frames of one animation that are indexed.  An animation of a
/// hundred thousand frames is already hours long.
pub const MAX_FRAMES: usize = 100_000;

/// The shortest time browsers show a frame: delays of 10 ms or less (what
/// GIF calls 0 or 1 centisecond) play at 100 ms, because early animation
/// tools wrote 0 for "as fast as you can" and nobody wants that literally.
pub const MIN_DELAY_MS: u32 = 100;

/// The time a frame shows, in seconds, from the delay the file gives in
/// milliseconds.
pub fn delay_seconds(ms: u32) -> f32 {
    (if ms <= 10 { MIN_DELAY_MS } else { ms }) as f32 / 1000.0
}

/*┌─────────────────╮
| Patches         |
└─────────────────╯*/
//region Patches

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

/// What happens to a frame's rectangle before the next frame is drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dispose {
    /// Left as it is.
    None,
    /// Cleared to transparent.
    Background,
    /// Put back as it was before the frame.
    Previous,
}

/// How a patch's pixels combine with the canvas.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Blend {
    /// The patch replaces what is under it.
    Replace,
    /// GIF: pixels that are not fully transparent replace, the others leave
    /// the canvas alone.
    Binary,
    /// APNG: straight alpha "over".
    Over,
    /// WebP: the integer version of "over" libwebp uses, so frames match
    /// what other decoders show.
    WebpOver,
}

/// One frame's pixels and how to put them on the canvas.
pub struct Patch {
    pub rect: Rect,
    /// Straight-alpha RGBA, `rect.w * rect.h` pixels.
    pub rgba: Vec<u8>,
    pub blend: Blend,
    pub dispose: Dispose,
}

/// Where an animation's patches come from.  Sources decode a frame when
/// asked, in any order.
pub trait PatchSource: Send {
    fn patch(&mut self, index: usize) -> Result<Patch, String>;
}

//endregion

/*┌─────────────────╮
| Compositor      |
└─────────────────╯*/
//region Compositor

/// The canvas an animation is replayed onto.
#[derive(Clone)]
pub struct Compositor {
    w: u32,
    h: u32,
    canvas: Vec<u8>,
    /// The canvas as it was before a frame that will be disposed of by
    /// restoring it.
    saved: Option<Vec<u8>>,
    /// The rectangle and disposal of the frame drawn last.
    pending: Option<(Rect, Dispose)>,
}

impl Compositor {
    pub fn new(w: u32, h: u32) -> Compositor {
        Compositor { w, h, canvas: vec![0; w as usize * h as usize * 4], saved: None, pending: None }
    }

    pub fn pixels(&self) -> &[u8] {
        &self.canvas
    }

    /// Disposes of the frame drawn before, then draws `patch`.  A patch
    /// that reaches outside the canvas is clipped to it.
    pub fn apply(&mut self, patch: &Patch) {
        // 1. The frame before leaves.
        let mut cleared = None;
        if let Some((rect, how)) = self.pending.take() {
            match how {
                Dispose::None => {}
                Dispose::Background => {
                    self.clear(rect);
                    cleared = Some(rect);
                }
                Dispose::Previous => {
                    if let Some(prev) = self.saved.take() {
                        self.canvas = prev;
                    }
                }
            }
        }
        // 2. This frame will be undone: remember what is under it.  A frame
        //    that covers the canvas and replaces it would leave nothing to
        //    restore but a blank canvas, which is what restoring a first
        //    frame means anyway.
        if patch.dispose == Dispose::Previous {
            match &mut self.saved {
                Some(s) => s.clone_from(&self.canvas),
                None => self.saved = Some(self.canvas.clone()),
            }
        } else {
            self.saved = None;
        }
        // 3. Draw it.
        self.draw(patch, cleared);
        self.pending = Some((patch.rect, patch.dispose));
    }

    fn clear(&mut self, r: Rect) {
        let (x1, y1) = ((r.x + r.w).min(self.w), (r.y + r.h).min(self.h));
        for y in r.y.min(self.h)..y1 {
            let a = (y as usize * self.w as usize + r.x.min(self.w) as usize) * 4;
            let b = (y as usize * self.w as usize + x1 as usize) * 4;
            self.canvas[a..b].fill(0);
        }
    }

    /// Draws a patch.  `cleared` is the rectangle just emptied by disposing of
    /// the frame before: libwebp copies WebP pixels there as they are, where
    /// elsewhere it blends them (and rounds).
    fn draw(&mut self, patch: &Patch, cleared: Option<Rect>) {
        let r = patch.rect;
        if r.w == 0 || r.h == 0 || patch.rgba.len() < r.w as usize * r.h as usize * 4 {
            return;
        }
        let w = r.w.min(self.w.saturating_sub(r.x)) as usize;
        let rows = r.h.min(self.h.saturating_sub(r.y)) as usize;
        if w == 0 || rows == 0 {
            return;
        }
        if patch.blend == Blend::Replace && w == self.w as usize && rows == self.h as usize && r.w == self.w {
            let n = self.canvas.len();
            self.canvas.copy_from_slice(&patch.rgba[..n]);
            return;
        }
        for row in 0..rows {
            let src = &patch.rgba[row * r.w as usize * 4..][..w * 4];
            let at = ((r.y as usize + row) * self.w as usize + r.x as usize) * 4;
            let dst = &mut self.canvas[at..at + w * 4];
            match patch.blend {
                Blend::Replace => dst.copy_from_slice(src),
                Blend::Binary => {
                    for (d, s) in dst.chunks_exact_mut(4).zip(src.chunks_exact(4)) {
                        if s[3] != 0 {
                            d.copy_from_slice(s);
                        }
                    }
                }
                Blend::Over => {
                    for (d, s) in dst.chunks_exact_mut(4).zip(src.chunks_exact(4)) {
                        over(d, s);
                    }
                }
                Blend::WebpOver => {
                    let y = r.y + row as u32;
                    for (i, (d, s)) in dst.chunks_exact_mut(4).zip(src.chunks_exact(4)).enumerate() {
                        let x = r.x + i as u32;
                        let fresh = cleared.is_some_and(|c| x >= c.x && x < c.x + c.w && y >= c.y && y < c.y + c.h);
                        if fresh {
                            d.copy_from_slice(s);
                        } else {
                            webp_over(d, s);
                        }
                    }
                }
            }
        }
    }
}

/// Straight-alpha source-over, as the APNG specification writes it.
fn over(d: &mut [u8], s: &[u8]) {
    match s[3] {
        0 => {}
        255 => d.copy_from_slice(s),
        sa => {
            let (sa, da) = (sa as u32, d[3] as u32);
            let t = da * (255 - sa);
            let a = sa * 255 + t;
            for i in 0..3 {
                d[i] = ((s[i] as u32 * sa * 255 + d[i] as u32 * t + a / 2) / a) as u8;
            }
            d[3] = a.div_ceil(255).min(255) as u8;
        }
    }
}

/// libwebp's `BlendPixelNonPremult`.
fn webp_over(d: &mut [u8], s: &[u8]) {
    let sa = s[3] as u32;
    if sa == 0 {
        return;
    }
    // libwebp blends only translucent pixels; an opaque one is copied.
    if sa == 255 {
        d.copy_from_slice(s);
        return;
    }
    let da = d[3] as u32;
    let dst_factor = (da * (256 - sa)) >> 8;
    let blend_a = (sa + dst_factor).min(255);
    let scale = (1u32 << 24) / blend_a.max(1);
    for i in 0..3 {
        d[i] = (((s[i] as u32 * sa + d[i] as u32 * dst_factor) * scale) >> 24).min(255) as u8;
    }
    d[3] = blend_a as u8;
}

//endregion

/*┌─────────────────╮
| Animated image  |
└─────────────────╯*/
//region Animated image

/// Everything an [`AnimatedImage`] needs from a format's reader.
pub struct Animation {
    pub info: Info,
    pub size: (u32, u32),
    /// How long each frame shows, in seconds.
    pub delays: Vec<f32>,
    /// How many times the animation plays; `None` for ever.
    pub plays: Option<u32>,
    pub source: Box<dyn PatchSource>,
}

struct Inner {
    source: Box<dyn PatchSource>,
    comp: Compositor,
    /// The frame on the canvas now.
    shown: Option<usize>,
    /// The first frame that would not decode; later frames show the last
    /// good one.
    broken: Option<usize>,
    checkpoints: Vec<(usize, Compositor)>,
    interval: usize,
    max_checkpoints: usize,
}

#[derive(Default)]
struct Kept {
    frames: Vec<Option<Arc<Vec<u8>>>>,
    bytes: usize,
}

/// An animation as a [`Canvas`]: one layer-less picture whose frames are
/// decoded as they are asked for.
pub struct AnimatedImage {
    info: Info,
    size: (u32, u32),
    delays: Vec<f32>,
    plays: Option<u32>,
    inner: Mutex<Inner>,
    kept: Mutex<Kept>,
    /// Bytes of finished frames kept for quick access.
    budget: usize,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl AnimatedImage {
    /// `budget` is how many bytes of finished frames may be kept; beyond it
    /// frames are decoded again when they are asked for.
    pub fn new(a: Animation, budget: usize) -> AnimatedImage {
        let (w, h) = a.size;
        let n = a.delays.len().max(1);
        let canvas_bytes = (w as usize * h as usize * 4).max(1);
        // A quarter of the budget goes to checkpoints, each holding up to
        // two canvases.
        let max_checkpoints = (budget / 4 / (2 * canvas_bytes)).min(64);
        let interval = if max_checkpoints == 0 { usize::MAX } else { n.div_ceil(max_checkpoints).max(2) };
        AnimatedImage {
            info: a.info,
            size: a.size,
            delays: a.delays,
            plays: a.plays,
            inner: Mutex::new(Inner {
                source: a.source,
                comp: Compositor::new(w, h),
                shown: None,
                broken: None,
                checkpoints: Vec::new(),
                interval,
                max_checkpoints,
            }),
            kept: Mutex::new(Kept { frames: vec![None; n], bytes: 0 }),
            budget,
        }
    }

    /// The pixels of frame `i` at full size.
    pub fn frame_pixels(&self, i: usize) -> Arc<Vec<u8>> {
        let i = i.min(self.delays.len().saturating_sub(1));
        if let Some(f) = lock(&self.kept).frames.get(i).cloned().flatten() {
            return f;
        }
        let mut inner = lock(&self.inner);
        // Another thread may have finished it while this one waited.
        if let Some(f) = lock(&self.kept).frames.get(i).cloned().flatten() {
            return f;
        }
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| inner.render(i)));
        if result.is_err() {
            // A decoder that panicked has left the canvas in an unknown state.
            inner.reset();
        }
        let pixels = Arc::new(inner.comp.pixels().to_vec());
        drop(inner);
        let mut kept = lock(&self.kept);
        if kept.bytes + pixels.len() <= self.budget
            && let Some(slot) = kept.frames.get_mut(i)
        {
            *slot = Some(pixels.clone());
            kept.bytes += pixels.len();
        }
        pixels
    }
}

impl Inner {
    fn reset(&mut self) {
        self.comp = Compositor::new(self.comp.w, self.comp.h);
        self.shown = None;
    }

    /// Replays patches until frame `i` is on the canvas, or the last frame
    /// that decodes.
    fn render(&mut self, i: usize) {
        if self.shown == Some(i) {
            return;
        }
        let stop = i.min(self.broken.map_or(usize::MAX, |b| b.saturating_sub(1)));
        // 1. Where to start: after the frame on the canvas if that is
        //    behind us, but a checkpoint nearer to the target is better.
        let mut from = match self.shown {
            Some(s) if s < stop => Some(s),
            Some(s) if s == stop => return,
            _ => None,
        };
        if let Some((at, cp)) = self.checkpoints.iter().rev().find(|(at, _)| *at <= stop)
            && from.is_none_or(|f| *at > f)
        {
            self.comp = cp.clone();
            from = Some(*at);
        }
        if from.is_none() {
            self.reset();
        }
        // 2. Replay.
        let mut k = from.map_or(0, |f| f + 1);
        while k <= stop {
            match self.source.patch(k) {
                Ok(patch) => {
                    self.comp.apply(&patch);
                    self.shown = Some(k);
                }
                Err(e) => {
                    eprintln!("slerp: animation frame {}: {e}", k + 1);
                    self.broken = Some(k);
                    break;
                }
            }
            if k > 0
                && k % self.interval == 0
                && self.checkpoints.len() < self.max_checkpoints
                && !self.checkpoints.iter().any(|(at, _)| *at == k)
            {
                self.checkpoints.push((k, self.comp.clone()));
                self.checkpoints.sort_by_key(|(at, _)| *at);
            }
            k += 1;
        }
    }
}

impl Canvas for AnimatedImage {
    fn info(&self) -> &Info {
        &self.info
    }

    fn size(&self) -> (u32, u32) {
        self.size
    }

    fn layers(&self) -> &[CanvasLayer] {
        &[]
    }

    fn frames(&self) -> &[f32] {
        &self.delays
    }

    fn clips(&self) -> &[FrameClip] {
        &[]
    }

    fn loops(&self) -> Option<u32> {
        self.plays
    }

    fn compose(&self, frame: usize, _shown: &[bool], scale: f32) -> Image8 {
        let (w, h) = self.size;
        let pixels = self.frame_pixels(frame);
        let (ow, oh) = scaled_size(w, h, scale);
        if (ow, oh) == (w, h) {
            Image8 { width: w, height: h, pixels: pixels.to_vec() }
        } else {
            Image8 { width: ow, height: oh, pixels: box_shrink(&pixels, w, h, ow, oh) }
        }
    }
}

//endregion

#[cfg(test)]
mod tests {
    use super::*;

    fn solid(w: u32, h: u32, c: [u8; 4]) -> Vec<u8> {
        c.repeat((w * h) as usize)
    }

    fn patch(rect: Rect, c: [u8; 4], blend: Blend, dispose: Dispose) -> Patch {
        Patch { rect, rgba: solid(rect.w, rect.h, c), blend, dispose }
    }

    const FULL: Rect = Rect { x: 0, y: 0, w: 4, h: 4 };
    const DOT: Rect = Rect { x: 1, y: 1, w: 2, h: 2 };

    fn at(c: &Compositor, x: usize, y: usize) -> [u8; 4] {
        let i = (y * 4 + x) * 4;
        c.pixels()[i..i + 4].try_into().unwrap()
    }

    #[test]
    fn dispose_none_background_previous() {
        // None: the dot stays under the next frame's transparent hole.
        let mut c = Compositor::new(4, 4);
        c.apply(&patch(FULL, [0, 0, 255, 255], Blend::Replace, Dispose::None));
        c.apply(&patch(DOT, [255, 0, 0, 255], Blend::Binary, Dispose::None));
        c.apply(&patch(Rect { x: 0, y: 0, w: 1, h: 1 }, [0, 255, 0, 255], Blend::Binary, Dispose::None));
        assert_eq!(at(&c, 1, 1), [255, 0, 0, 255]);
        assert_eq!(at(&c, 0, 0), [0, 255, 0, 255]);
        assert_eq!(at(&c, 3, 3), [0, 0, 255, 255]);

        // Background: the dot's rectangle is cleared when the next frame comes.
        let mut c = Compositor::new(4, 4);
        c.apply(&patch(FULL, [0, 0, 255, 255], Blend::Replace, Dispose::None));
        c.apply(&patch(DOT, [255, 0, 0, 255], Blend::Binary, Dispose::Background));
        assert_eq!(at(&c, 1, 1), [255, 0, 0, 255]);
        c.apply(&patch(Rect { x: 0, y: 0, w: 1, h: 1 }, [0, 255, 0, 255], Blend::Binary, Dispose::None));
        assert_eq!(at(&c, 1, 1), [0, 0, 0, 0]);
        assert_eq!(at(&c, 2, 2), [0, 0, 0, 0]);
        assert_eq!(at(&c, 3, 3), [0, 0, 255, 255]);

        // Previous: what was under the dot comes back.
        let mut c = Compositor::new(4, 4);
        c.apply(&patch(FULL, [0, 0, 255, 255], Blend::Replace, Dispose::None));
        c.apply(&patch(DOT, [255, 0, 0, 255], Blend::Binary, Dispose::Previous));
        assert_eq!(at(&c, 2, 2), [255, 0, 0, 255]);
        c.apply(&patch(Rect { x: 0, y: 0, w: 1, h: 1 }, [0, 255, 0, 255], Blend::Binary, Dispose::None));
        assert_eq!(at(&c, 2, 2), [0, 0, 255, 255]);
    }

    #[test]
    fn restoring_the_previous_canvas_of_a_first_frame_is_blank() {
        let mut c = Compositor::new(4, 4);
        c.apply(&patch(FULL, [9, 9, 9, 255], Blend::Replace, Dispose::Previous));
        c.apply(&patch(DOT, [1, 1, 1, 255], Blend::Replace, Dispose::None));
        assert_eq!(at(&c, 0, 0), [0, 0, 0, 0]);
        assert_eq!(at(&c, 1, 1), [1, 1, 1, 255]);
    }

    #[test]
    fn blending() {
        let mut c = Compositor::new(4, 4);
        c.apply(&patch(FULL, [0, 0, 255, 255], Blend::Replace, Dispose::None));
        // Half-transparent red over opaque blue, by APNG's formula.
        c.apply(&patch(DOT, [255, 0, 0, 128], Blend::Over, Dispose::None));
        let p = at(&c, 1, 1);
        assert_eq!(p[3], 255);
        assert!((p[0] as i32 - 128).abs() <= 1 && (p[2] as i32 - 127).abs() <= 1, "{p:?}");
        // Binary blending ignores a transparent pixel and takes any other whole.
        let mut c = Compositor::new(4, 4);
        c.apply(&patch(FULL, [0, 0, 255, 255], Blend::Replace, Dispose::None));
        c.apply(&patch(DOT, [255, 0, 0, 0], Blend::Binary, Dispose::None));
        assert_eq!(at(&c, 1, 1), [0, 0, 255, 255]);
        // Replace puts transparency in.
        c.apply(&patch(DOT, [255, 0, 0, 0], Blend::Replace, Dispose::None));
        assert_eq!(at(&c, 1, 1), [255, 0, 0, 0]);
        // WebP's formula over a transparent canvas gives the source back.
        let mut c = Compositor::new(4, 4);
        c.apply(&patch(DOT, [10, 20, 30, 77], Blend::WebpOver, Dispose::None));
        let p = at(&c, 1, 1);
        assert_eq!(p[3], 77);
        assert!((p[0] as i32 - 10).abs() <= 1 && (p[1] as i32 - 20).abs() <= 1, "{p:?}");
    }

    #[test]
    fn patches_outside_the_canvas_are_clipped() {
        let mut c = Compositor::new(4, 4);
        c.apply(&patch(Rect { x: 3, y: 3, w: 5, h: 5 }, [5, 5, 5, 255], Blend::Replace, Dispose::None));
        assert_eq!(at(&c, 3, 3), [5, 5, 5, 255]);
        c.apply(&patch(Rect { x: 9, y: 9, w: 2, h: 2 }, [6, 6, 6, 255], Blend::Replace, Dispose::Background));
        c.apply(&patch(Rect { x: 0, y: 0, w: 0, h: 0 }, [6, 6, 6, 255], Blend::Replace, Dispose::None));
        // A patch with too few pixels is ignored, not read past.
        let mut short = patch(FULL, [7, 7, 7, 255], Blend::Replace, Dispose::None);
        short.rgba.truncate(10);
        c.apply(&short);
        assert_eq!(at(&c, 3, 3), [5, 5, 5, 255]);
    }

    #[test]
    fn delays_follow_browsers() {
        assert_eq!(delay_seconds(0), 0.1);
        assert_eq!(delay_seconds(10), 0.1);
        assert_eq!(delay_seconds(20), 0.02);
        assert_eq!(delay_seconds(500), 0.5);
    }

    /// Frame i is a canvas whose every pixel has red = i, so the order in
    /// which frames were decoded shows.
    struct Counting {
        decoded: Arc<Mutex<Vec<usize>>>,
        fail_at: Option<usize>,
    }

    impl PatchSource for Counting {
        fn patch(&mut self, index: usize) -> Result<Patch, String> {
            if self.fail_at == Some(index) {
                return Err("broken".into());
            }
            self.decoded.lock().unwrap().push(index);
            Ok(patch(FULL, [index as u8, 0, 0, 255], Blend::Replace, Dispose::None))
        }
    }

    fn animation(n: usize, fail_at: Option<usize>, budget: usize) -> (AnimatedImage, Arc<Mutex<Vec<usize>>>) {
        let decoded = Arc::new(Mutex::new(Vec::new()));
        let a = Animation {
            info: Info::default(),
            size: (4, 4),
            delays: vec![0.1; n],
            plays: Some(3),
            source: Box::new(Counting { decoded: decoded.clone(), fail_at }),
        };
        (AnimatedImage::new(a, budget), decoded)
    }

    fn red(a: &AnimatedImage, i: usize) -> u8 {
        a.compose(i, &[], 1.0).pixels[0]
    }

    #[test]
    fn playing_forwards_decodes_each_frame_once() {
        let (a, decoded) = animation(10, None, 1 << 20);
        for i in 0..10 {
            assert_eq!(red(&a, i), i as u8);
        }
        // A second pass, and jumps, come from the kept frames.
        for i in [3, 0, 9, 5, 5] {
            assert_eq!(red(&a, i), i as u8);
        }
        assert_eq!(*decoded.lock().unwrap(), (0..10).collect::<Vec<_>>());
        assert_eq!(a.loops(), Some(3));
        assert_eq!(a.frames().len(), 10);
        assert!(a.layers().is_empty() && a.clips().is_empty());
    }

    #[test]
    fn without_a_budget_frames_are_decoded_again_from_checkpoints() {
        // Room for checkpoints but no kept frames: 4x4 canvas is 64 bytes.
        let (a, decoded) = animation(100, None, 64 * 1000);
        {
            let mut k = lock(&a.kept);
            k.bytes = a.budget;
        }
        for i in 0..100 {
            assert_eq!(red(&a, i), i as u8);
        }
        let first_pass = decoded.lock().unwrap().len();
        assert_eq!(first_pass, 100);
        // Jumping back goes through a checkpoint, not frame 0 again.
        assert_eq!(red(&a, 60), 60);
        let again = decoded.lock().unwrap().len() - first_pass;
        assert!(again < 60, "decoded {again} frames to get to frame 60");
        assert_eq!(red(&a, 0), 0);
    }

    #[test]
    fn a_broken_frame_shows_the_last_good_one() {
        let (a, _) = animation(6, Some(3), 1 << 20);
        assert_eq!(red(&a, 2), 2);
        assert_eq!(red(&a, 3), 2);
        assert_eq!(red(&a, 5), 2);
        assert_eq!(red(&a, 1), 1);
        // Even asking for the broken frame first.
        let (b, _) = animation(6, Some(0), 1 << 20);
        assert_eq!(b.compose(4, &[], 1.0).pixels[3], 0);
    }

    #[test]
    fn scaled_composition() {
        let (a, _) = animation(2, None, 1 << 20);
        let img = a.compose(1, &[], 0.5);
        assert_eq!((img.width, img.height), (2, 2));
        assert_eq!(img.pixels.len(), 16);
        assert_eq!(&img.pixels[..4], &[1, 0, 0, 255]);
        // Frames beyond the end clamp to the last.
        assert_eq!(red(&a, 99), 1);
    }
}
