//! A canvas made of whole-picture layers: every layer has one full-size
//! picture per frame, an opacity and a blend mode, and frames are composed
//! by laying the visible layers over each other.  Piskel sprites and Pyxel
//! Edit documents are both this simple.

use crate::blend::{Blend, Px};
use crate::pixels;
use slerp_scene::{Canvas, CanvasLayer, FrameClip, Image8, Info};
use std::sync::Arc;

/// How a layer's pixels go over the ones below.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Blend(Blend),
    /// Pyxel Edit's "invert": the backdrop's colour turned negative where
    /// the layer has pixels, as far as the layer is opaque.
    Invert,
}

pub struct StackLayer {
    /// One picture per frame, each the canvas's size; `None` where a frame
    /// of this layer is empty.
    pub frames: Vec<Option<Arc<Image8>>>,
    pub opacity: u8,
    pub mode: Mode,
    /// A group's own entry, which draws nothing; its children hide with it.
    pub group: bool,
    /// The groups it sits in, nearest first, as indices into the layers.
    pub parents: Vec<usize>,
}

/// The canvas.  `layers` and `listed` run in parallel, bottom first.
pub struct Stack {
    pub info: Info,
    pub width: u32,
    pub height: u32,
    pub layers: Vec<StackLayer>,
    pub listed: Vec<CanvasLayer>,
    pub durations: Vec<f32>,
    pub clips: Vec<FrameClip>,
}

impl Canvas for Stack {
    fn info(&self) -> &Info {
        &self.info
    }

    fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    fn layers(&self) -> &[CanvasLayer] {
        &self.listed
    }

    fn frames(&self) -> &[f32] {
        &self.durations
    }

    fn clips(&self) -> &[FrameClip] {
        &self.clips
    }

    fn compose(&self, frame: usize, shown: &[bool], scale: f32) -> Image8 {
        let visible = |i: usize| shown.get(i).copied().unwrap_or(self.listed[i].visible);
        let mut out = pixels::blank(self.width, self.height);
        for (i, layer) in self.layers.iter().enumerate() {
            if layer.group || !visible(i) || layer.parents.iter().any(|&p| !visible(p)) {
                continue;
            }
            let Some(Some(picture)) = layer.frames.get(frame.min(layer.frames.len().saturating_sub(1))) else {
                continue;
            };
            if picture.width != self.width || picture.height != self.height {
                continue;
            }
            for (o, s) in out.pixels.chunks_exact_mut(4).zip(picture.pixels.chunks_exact(4)) {
                if s[3] == 0 {
                    continue;
                }
                let b: Px = [o[0], o[1], o[2], o[3]];
                let src: Px = [s[0], s[1], s[2], s[3]];
                let r = match layer.mode {
                    Mode::Blend(blend) => blend.apply(b, src, layer.opacity),
                    Mode::Invert if b[3] == 0 => b,
                    Mode::Invert => Blend::Normal.apply(b, [255 - b[0], 255 - b[1], 255 - b[2], s[3]], layer.opacity),
                };
                o.copy_from_slice(&r);
            }
        }
        let scale = if scale.is_finite() && scale > 0.0 { scale.min(1.0) } else { 1.0 };
        if scale < 1.0 {
            let w = ((self.width as f32 * scale).round() as u32).max(1);
            let h = ((self.height as f32 * scale).round() as u32).max(1);
            return pixels::resize(&out, w, h);
        }
        out
    }
}
