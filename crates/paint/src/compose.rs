//! The compositor every layered format here shares: a tree of raster layers
//! and groups, each with a blend mode, opacity, mask and offset, flattened
//! onto a transparent canvas at any scale up to full size.
//!
//! Two conventions are implemented, because the applications disagree:
//!
//! - [`Rules::Adobe`]: Photoshop and OpenRaster (whose `composite-op`s are
//!   the W3C compositing specification's, itself modelled on PDF and
//!   Photoshop).  Colours blend and composite as stored, gamma-encoded for 8
//!   and 16-bit files and linear for 32-bit ones; a blend mode mixes in where
//!   the backdrop is opaque and fades to the plain layer where it isn't.
//! - [`Rules::Gimp`]: GIMP 2.10 and 3.  Every mode has a blend space (linear
//!   RGB, gamma-encoded RGB or CIE LCh), a composite space and a composite
//!   mode (union, clip to backdrop, clip to layer, intersection), each of
//!   which a layer may override; the legacy modes of GIMP 2.8 and earlier
//!   use the old arithmetic in gamma-encoded RGB and never add opacity.
//!
//! Pixels are straight-alpha RGBA floats.  The accumulator holds the
//! working space of the rules: stored values for Adobe files, linear light
//! for GIMP's.  Layers are decoded one at a time through a [`Source`], put
//! at the target scale, blended in rows on all cores, and dropped, so memory
//! stays at a few canvases whatever the number of layers.

use rayon::prelude::*;

/*┌─────────────────╮
| Model           |
└─────────────────╯*/
//region Model

/// Decoded layer pixels in the file's own sample size.  Colour layers have
/// four channels (RGBA), masks one.  The `Srgb` kinds hold gamma-encoded
/// colour (alpha stays linear) that is read out as linear light, so GIMP's
/// 8 and 16-bit perceptual layers keep their compact size until composed.
#[derive(Clone, Debug)]
pub enum Samples {
    U8(Vec<u8>),
    U16(Vec<u16>),
    F32(Vec<f32>),
    Srgb8(Vec<u8>),
    Srgb16(Vec<u16>),
}

impl Samples {
    #[inline]
    pub(crate) fn get(&self, i: usize) -> f32 {
        match self {
            Samples::U8(v) => v[i] as f32 * (1.0 / 255.0),
            Samples::U16(v) => v[i] as f32 * (1.0 / 65535.0),
            Samples::F32(v) => v[i],
            Samples::Srgb8(v) if i % 4 == 3 => v[i] as f32 * (1.0 / 255.0),
            Samples::Srgb8(v) => srgb8_table()[v[i] as usize],
            Samples::Srgb16(v) if i % 4 == 3 => v[i] as f32 * (1.0 / 65535.0),
            Samples::Srgb16(v) => srgb16_table()[v[i] as usize],
        }
    }

    pub fn len(&self) -> usize {
        match self {
            Samples::U8(v) | Samples::Srgb8(v) => v.len(),
            Samples::U16(v) | Samples::Srgb16(v) => v.len(),
            Samples::F32(v) => v.len(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// A decoded layer or mask: `channels` (4 or 1) samples per pixel, rows
/// from the top.  Colour values are in the encoding the rules expect as
/// input: as stored for Adobe files, linear light for GIMP files (decoders
/// convert gamma-encoded GIMP data while reading).
#[derive(Clone, Debug)]
pub struct Raster {
    pub width: u32,
    pub height: u32,
    pub channels: u8,
    pub samples: Samples,
}

/// Where the pixels come from: decoders keep the file and decode a layer
/// only when it is drawn.
pub trait Source: Send + Sync {
    /// Colour layer `id`, RGBA.
    fn layer(&self, id: usize) -> Result<Raster, String>;
    /// Mask `id`, one channel.
    fn mask(&self, id: usize) -> Result<Raster, String>;
}

/// A node of the layer tree.
#[derive(Clone, Debug)]
pub struct Node {
    pub name: String,
    pub visible: bool,
    /// 0 to 1.
    pub opacity: f32,
    pub mode: Mode,
    /// GIMP's per-layer overrides of the mode's spaces and compositing.
    pub gimp: GimpOverrides,
    /// Photoshop clipping: this layer shows only where the nearest
    /// unclipped layer below it does.
    pub clip: bool,
    pub mask: Option<Mask>,
    pub kind: Kind,
    /// Index into the flat list the viewer toggles (see [`flatten`]).
    pub slot: usize,
}

#[derive(Clone, Debug)]
pub enum Kind {
    /// Pixels from [`Source::layer`], with their top-left corner at (x, y)
    /// on the canvas.
    Raster { id: usize, x: i32, y: i32 },
    /// Children bottom first.  An isolated group is composed on its own
    /// transparent canvas and then blended as one layer; a pass-through
    /// group puts its children straight onto what is below.
    Group { children: Vec<Node>, isolated: bool },
    /// A layer with nothing to draw (an adjustment layer, an empty layer).
    Empty,
}

#[derive(Clone, Copy, Debug)]
pub struct Mask {
    pub id: usize,
    pub x: i32,
    pub y: i32,
    /// The value outside the mask's rectangle.
    pub outside: f32,
}

/// How a document's colours blend.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rules {
    /// Photoshop and OpenRaster; `linear` when the stored values are
    /// linear light (32-bit Photoshop files).
    Adobe {
        linear: bool,
    },
    Gimp,
}

/// GIMP's layer properties that override a mode's defaults; `None` keeps
/// the mode's own.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GimpOverrides {
    pub blend_space: Option<Space>,
    pub composite_space: Option<Space>,
    pub composite: Option<Composite>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Space {
    Linear,
    /// Gamma-encoded RGB (GIMP's "perceptual" and "non-linear").
    Perceptual,
    Lab,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Composite {
    Union,
    ClipToBackdrop,
    ClipToLayer,
    Intersection,
}

/// Every blend mode of the formats here.  Each format maps its own names
/// onto these; [`Mode::name`] gives the label the viewer lists.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Normal,
    Dissolve,
    Behind,
    Multiply,
    Screen,
    Overlay,
    Darken,
    Lighten,
    ColorDodge,
    ColorBurn,
    HardLight,
    /// The W3C formula, which OpenRaster names.
    SoftLight,
    /// Photoshop's own soft light, which differs slightly from the W3C's.
    SoftLightPhotoshop,
    Difference,
    Exclusion,
    Hue,
    Saturation,
    Color,
    Luminosity,
    LinearBurn,
    /// Linear dodge; "plus" in OpenRaster.
    Addition,
    Subtract,
    Divide,
    VividLight,
    LinearLight,
    PinLight,
    HardMix,
    DarkerColor,
    LighterColor,
    /// Porter-Duff operators OpenRaster allows.
    DstIn,
    DstOut,
    SrcAtop,
    DstAtop,
    Xor,
    /// OpenRaster's `svg:plus`: colours add, weighted by alpha.
    Plus,
    // GIMP's own.
    GrainExtract,
    GrainMerge,
    HsvHue,
    HsvSaturation,
    HslColor,
    HsvValue,
    LchHue,
    LchChroma,
    LchColor,
    LchLightness,
    LumaDarken,
    LumaLighten,
    Luminance,
    Erase,
    AntiErase,
    Merge,
    Split,
    ColorErase,
    Replace,
    PassThrough,
    /// One of GIMP 2.8's modes, kept with its old arithmetic.
    Legacy(Legacy),
    /// A mode this reader doesn't implement, drawn as normal.
    Unknown,
}

/// GIMP 2.8's layer modes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Legacy {
    Normal,
    Behind,
    Multiply,
    Screen,
    Difference,
    Addition,
    Subtract,
    Darken,
    Lighten,
    HsvHue,
    HsvSaturation,
    HslColor,
    HsvValue,
    Divide,
    Dodge,
    Burn,
    HardLight,
    SoftLight,
    GrainExtract,
    GrainMerge,
    ColorErase,
}

impl Mode {
    pub fn name(self) -> &'static str {
        use Mode::*;
        match self {
            Normal => "normal",
            Dissolve => "dissolve",
            Behind => "behind",
            Multiply => "multiply",
            Screen => "screen",
            Overlay => "overlay",
            Darken => "darken",
            Lighten => "lighten",
            ColorDodge => "colour dodge",
            ColorBurn => "colour burn",
            HardLight => "hard light",
            SoftLight | SoftLightPhotoshop => "soft light",
            Difference => "difference",
            Exclusion => "exclusion",
            Hue => "hue",
            Saturation => "saturation",
            Color => "colour",
            Luminosity => "luminosity",
            LinearBurn => "linear burn",
            Addition => "addition",
            Subtract => "subtract",
            Divide => "divide",
            VividLight => "vivid light",
            LinearLight => "linear light",
            PinLight => "pin light",
            HardMix => "hard mix",
            DarkerColor => "darker colour",
            LighterColor => "lighter colour",
            DstIn => "destination in",
            DstOut => "destination out",
            SrcAtop => "source atop",
            DstAtop => "destination atop",
            Xor => "xor",
            Plus => "plus",
            GrainExtract => "grain extract",
            GrainMerge => "grain merge",
            HsvHue => "HSV hue",
            HsvSaturation => "HSV saturation",
            HslColor => "HSL colour",
            HsvValue => "HSV value",
            LchHue => "LCh hue",
            LchChroma => "LCh chroma",
            LchColor => "LCh colour",
            LchLightness => "LCh lightness",
            LumaDarken => "luma darken only",
            LumaLighten => "luma lighten only",
            Luminance => "luminance",
            Erase => "erase",
            AntiErase => "anti-erase",
            Merge => "merge",
            Split => "split",
            ColorErase => "colour erase",
            Replace => "replace",
            PassThrough => "pass through",
            Legacy(l) => match l {
                self::Legacy::Normal => "normal (legacy)",
                self::Legacy::Behind => "behind (legacy)",
                self::Legacy::Multiply => "multiply (legacy)",
                self::Legacy::Screen => "screen (legacy)",
                self::Legacy::Difference => "difference (legacy)",
                self::Legacy::Addition => "addition (legacy)",
                self::Legacy::Subtract => "subtract (legacy)",
                self::Legacy::Darken => "darken only (legacy)",
                self::Legacy::Lighten => "lighten only (legacy)",
                self::Legacy::HsvHue => "hue (legacy)",
                self::Legacy::HsvSaturation => "saturation (legacy)",
                self::Legacy::HslColor => "colour (legacy)",
                self::Legacy::HsvValue => "value (legacy)",
                self::Legacy::Divide => "divide (legacy)",
                self::Legacy::Dodge => "dodge (legacy)",
                self::Legacy::Burn => "burn (legacy)",
                self::Legacy::HardLight => "hard light (legacy)",
                self::Legacy::SoftLight => "soft light (legacy)",
                self::Legacy::GrainExtract => "grain extract (legacy)",
                self::Legacy::GrainMerge => "grain merge (legacy)",
                self::Legacy::ColorErase => "colour erase (legacy)",
            },
            Unknown => "unknown (drawn as normal)",
        }
    }

    /// Whether the compositor only approximates this mode, which the
    /// loader mentions.
    pub fn approximate(self) -> bool {
        matches!(self, Mode::Merge | Mode::Split | Mode::ColorErase | Mode::Legacy(Legacy::ColorErase) | Mode::Unknown)
    }
}

/// The flat list the viewer shows and toggles, bottom first with each
/// group before its children, as `(depth, node)`; it also numbers each
/// node's [`Node::slot`].
pub fn flatten(nodes: &mut [Node]) -> usize {
    fn walk(nodes: &mut [Node], next: &mut usize) {
        for n in nodes.iter_mut() {
            n.slot = *next;
            *next += 1;
            if let Kind::Group { children, .. } = &mut n.kind {
                walk(children, next);
            }
        }
    }
    let mut next = 0;
    walk(nodes, &mut next);
    next
}

/// Visits every node, bottom first, groups before their children, with
/// their depth.
pub fn visit<'a>(nodes: &'a [Node], depth: u32, f: &mut dyn FnMut(&'a Node, u32)) {
    for n in nodes {
        f(n, depth);
        if let Kind::Group { children, .. } = &n.kind {
            visit(children, depth + 1, f);
        }
    }
}

//endregion

/*┌─────────────────╮
| Composing       |
└─────────────────╯*/
//region Composing

/// A canvas of straight RGBA floats.
#[derive(Clone)]
pub struct Canvas {
    pub width: u32,
    pub height: u32,
    pub px: Vec<f32>,
}

impl Canvas {
    pub fn new(width: u32, height: u32) -> Canvas {
        Canvas { width, height, px: vec![0.0; width as usize * height as usize * 4] }
    }
}

/// Flattens the visible layers of `nodes` (`shown` overrides visibility by
/// slot) at `scale` of the `width` × `height` canvas.  Failures to decode a
/// layer are collected in `problems`; the layer is skipped.
#[allow(clippy::too_many_arguments)]
pub fn compose(
    nodes: &[Node],
    source: &dyn Source,
    rules: Rules,
    width: u32,
    height: u32,
    scale: f32,
    shown: &[bool],
    problems: &mut Vec<String>,
) -> Canvas {
    let tw = ((width as f32 * scale).round() as u32).max(1);
    let th = ((height as f32 * scale).round() as u32).max(1);
    let mut ctx = Ctx { source, rules, scale, shown, problems, tw, th };
    let mut canvas = Canvas::new(tw, th);
    ctx.layers(nodes, &mut canvas, &mut true);
    canvas
}

struct Ctx<'a> {
    source: &'a dyn Source,
    rules: Rules,
    scale: f32,
    shown: &'a [bool],
    problems: &'a mut Vec<String>,
    tw: u32,
    th: u32,
}

/// A layer's pixels at the target scale, placed on the target canvas.
struct Placed {
    x: i32,
    y: i32,
    w: u32,
    h: u32,
    channels: usize,
    px: Vec<f32>,
    outside: f32,
}

impl Placed {
    #[inline]
    fn at(&self, x: i32, y: i32, c: usize) -> Option<f32> {
        let (lx, ly) = (x - self.x, y - self.y);
        if lx < 0 || ly < 0 || lx >= self.w as i32 || ly >= self.h as i32 {
            return None;
        }
        Some(self.px[(ly as usize * self.w as usize + lx as usize) * self.channels + c])
    }
}

impl Ctx<'_> {
    fn visible(&self, n: &Node) -> bool {
        self.shown.get(n.slot).copied().unwrap_or(n.visible)
    }

    /// Draws `nodes` onto `target`.  `bottom` stays true while nothing has
    /// been drawn into the stack: GIMP draws the bottom layer of every stack
    /// as it is, whatever its mode, since there is nothing to blend with.
    fn layers(&mut self, nodes: &[Node], target: &mut Canvas, bottom: &mut bool) {
        // The alpha of the last unclipped layer, for the layers clipped to it.
        let mut base: Option<Placed> = None;
        for n in nodes {
            if !n.clip {
                base = None;
            }
            if !self.visible(n) || n.opacity <= 0.0 {
                if !n.clip {
                    // A hidden base hides what is clipped to it.
                    base = Some(Placed { x: 0, y: 0, w: 0, h: 0, channels: 1, px: Vec::new(), outside: 0.0 });
                }
                continue;
            }
            let mask = n.mask.and_then(|m| self.place_mask(m));
            match &n.kind {
                Kind::Empty => {
                    if !n.clip {
                        base = Some(Placed { x: 0, y: 0, w: 0, h: 0, channels: 1, px: Vec::new(), outside: 0.0 });
                    }
                }
                Kind::Raster { id, x, y } => {
                    let Some(mut layer) = self.place_layer(*id, *x, *y, &n.name) else { continue };
                    if n.clip {
                        if let Some(b) = &base {
                            clip_alpha(&mut layer, b);
                        }
                    } else {
                        base = Some(alpha_of(&layer, mask.as_ref()));
                    }
                    blend_onto(target, &layer, mask.as_ref(), &self.effective(n, bottom), self.rules);
                }
                Kind::Group { children, isolated } => {
                    let pass = !isolated || n.mode == Mode::PassThrough;
                    if pass {
                        let before = (n.opacity < 1.0 || mask.is_some()).then(|| target.clone());
                        self.layers(children, target, bottom);
                        if let Some(before) = before {
                            mix_back(target, &before, n.opacity, mask.as_ref());
                        }
                    } else {
                        let mut inner = Canvas::new(self.tw, self.th);
                        self.layers(children, &mut inner, &mut true);
                        let layer =
                            Placed { x: 0, y: 0, w: self.tw, h: self.th, channels: 4, px: inner.px, outside: 0.0 };
                        if !n.clip {
                            base = Some(alpha_of(&layer, mask.as_ref()));
                        }
                        blend_onto(target, &layer, mask.as_ref(), &self.effective(n, bottom), self.rules);
                    }
                }
            }
        }
    }

    /// The node as drawn: under GIMP's rules the first thing drawn into a
    /// stack is drawn in normal mode (GIMP's "last node" of a layer graph).
    fn effective(&self, n: &Node, bottom: &mut bool) -> Node {
        let first = std::mem::replace(bottom, false);
        let mut n = n.clone();
        if first && self.rules == Rules::Gimp && n.mode != Mode::Dissolve {
            n.mode = Mode::Normal;
            n.gimp = GimpOverrides::default();
        }
        if let Kind::Group { children, .. } = &mut n.kind {
            children.clear();
        }
        n
    }

    fn place_layer(&mut self, id: usize, x: i32, y: i32, name: &str) -> Option<Placed> {
        match self.source.layer(id) {
            Ok(r) if r.width > 0 && r.height > 0 && r.channels == 4 => {
                Some(place(&r, x, y, self.scale, 0.0, (self.tw, self.th)))
            }
            Ok(_) => None,
            Err(e) => {
                self.problems.push(format!("layer \"{name}\": {e}"));
                None
            }
        }
    }

    fn place_mask(&mut self, m: Mask) -> Option<Placed> {
        match self.source.mask(m.id) {
            Ok(r) if r.width > 0 && r.height > 0 && r.channels == 1 => {
                Some(place(&r, m.x, m.y, self.scale, m.outside, (self.tw, self.th)))
            }
            // An empty mask is its outside value everywhere.
            Ok(_) => Some(Placed { x: 0, y: 0, w: 0, h: 0, channels: 1, px: Vec::new(), outside: m.outside }),
            Err(e) => {
                self.problems.push(format!("a layer mask: {e}"));
                None
            }
        }
    }
}

/// Puts a raster at the target scale, cut to the target canvas: offsets
/// scale with it, and pixels are averaged (weighted by alpha for colour)
/// over the source area each target pixel covers.
fn place(r: &Raster, x: i32, y: i32, scale: f32, outside: f32, (tw, th): (u32, u32)) -> Placed {
    let ch = r.channels as usize;
    let empty = Placed { x: 0, y: 0, w: 0, h: 0, channels: ch, px: Vec::new(), outside };
    if (scale - 1.0).abs() < 1e-6 {
        let (x0, y0) = (x.max(0), y.max(0));
        let x1 = (x as i64 + r.width as i64).min(tw as i64) as i32;
        let y1 = (y as i64 + r.height as i64).min(th as i64) as i32;
        if x0 >= x1 || y0 >= y1 {
            return empty;
        }
        let (w, h) = ((x1 - x0) as usize, (y1 - y0) as usize);
        let mut px = vec![0f32; w * h * ch];
        px.par_chunks_mut(w * ch).enumerate().for_each(|(row, out)| {
            let sy = (y0 - y) as usize + row;
            let start = (sy * r.width as usize + (x0 - x) as usize) * ch;
            for (i, v) in out.iter_mut().enumerate() {
                *v = r.samples.get(start + i);
            }
        });
        return Placed { x: x0, y: y0, w: w as u32, h: h as u32, channels: ch, px, outside };
    }
    let x0 = ((x as f64 * scale as f64).floor() as i64).max(0) as i32;
    let y0 = ((y as f64 * scale as f64).floor() as i64).max(0) as i32;
    let x1 = (((x as f64 + r.width as f64) * scale as f64).ceil() as i64).min(tw as i64) as i32;
    let y1 = (((y as f64 + r.height as f64) * scale as f64).ceil() as i64).min(th as i64) as i32;
    if x0 >= x1 || y0 >= y1 {
        return empty;
    }
    let (w, h) = ((x1 - x0) as u32, (y1 - y0) as u32);
    let inv = 1.0 / scale as f64;
    let mut px = vec![0f32; w as usize * h as usize * ch];
    px.par_chunks_mut(w as usize * ch).enumerate().for_each(|(ty, row)| {
        // The source rows and columns this target pixel covers.
        let sy0 = (((y0 + ty as i32) as f64 * inv) - y as f64).max(0.0);
        let sy1 = (((y0 + ty as i32 + 1) as f64 * inv) - y as f64).min(r.height as f64);
        for tx in 0..w as usize {
            let sx0 = (((x0 + tx as i32) as f64 * inv) - x as f64).max(0.0);
            let sx1 = (((x0 + tx as i32 + 1) as f64 * inv) - x as f64).min(r.width as f64);
            let out = &mut row[tx * ch..tx * ch + ch];
            if sx1 <= sx0 || sy1 <= sy0 {
                if ch == 1 {
                    out[0] = outside;
                }
                continue;
            }
            let (ya, yb) = (sy0.floor() as usize, (sy1.ceil() as usize).min(r.height as usize));
            let (xa, xb) = (sx0.floor() as usize, (sx1.ceil() as usize).min(r.width as usize));
            let mut acc = [0f64; 4];
            let mut weight = 0f64;
            let covered = (sx1 - sx0) * (sy1 - sy0);
            // The part of the pixel the layer doesn't reach.
            let area = inv * inv;
            for sy in ya..yb {
                let wy = (sy as f64 + 1.0).min(sy1) - (sy as f64).max(sy0);
                for sx in xa..xb {
                    let wx = (sx as f64 + 1.0).min(sx1) - (sx as f64).max(sx0);
                    let wgt = wx * wy;
                    let i = (sy * r.width as usize + sx) * ch;
                    if ch == 4 {
                        let a = r.samples.get(i + 3) as f64 * wgt;
                        for (c, v) in acc.iter_mut().take(3).enumerate() {
                            *v += r.samples.get(i + c) as f64 * a;
                        }
                        acc[3] += a;
                    } else {
                        acc[0] += r.samples.get(i) as f64 * wgt;
                    }
                    weight += wgt;
                }
            }
            if ch == 4 {
                if acc[3] > 0.0 {
                    for c in 0..3 {
                        out[c] = (acc[c] / acc[3]) as f32;
                    }
                }
                out[3] = (acc[3] / area) as f32;
            } else {
                let missing = (area - covered).max(0.0);
                out[0] = ((acc[0] + outside as f64 * missing) / (weight + missing).max(1e-12)) as f32;
            }
        }
    });
    Placed { x: x0, y: y0, w, h, channels: ch, px, outside }
}

/// A layer's alpha after its mask, kept for the layers clipped to it.
fn alpha_of(layer: &Placed, mask: Option<&Placed>) -> Placed {
    let mut px = Vec::with_capacity(layer.w as usize * layer.h as usize);
    for ly in 0..layer.h as i32 {
        for lx in 0..layer.w as i32 {
            let (x, y) = (layer.x + lx, layer.y + ly);
            let a = layer.px[(ly as usize * layer.w as usize + lx as usize) * 4 + 3];
            let m = mask.map_or(1.0, |m| m.at(x, y, 0).unwrap_or(m.outside));
            px.push(a * m);
        }
    }
    Placed { x: layer.x, y: layer.y, w: layer.w, h: layer.h, channels: 1, px, outside: 0.0 }
}

fn clip_alpha(layer: &mut Placed, base: &Placed) {
    for ly in 0..layer.h as i32 {
        for lx in 0..layer.w as i32 {
            let a = base.at(layer.x + lx, layer.y + ly, 0).unwrap_or(0.0);
            layer.px[(ly as usize * layer.w as usize + lx as usize) * 4 + 3] *= a;
        }
    }
}

/// Mixes a pass-through group's result back towards what was below it by
/// the group's opacity and mask.
fn mix_back(target: &mut Canvas, before: &Canvas, opacity: f32, mask: Option<&Placed>) {
    let w = target.width as usize;
    target.px.par_chunks_mut(w * 4).zip(before.px.par_chunks(w * 4)).enumerate().for_each(|(y, (row, old))| {
        for x in 0..w {
            let m = mask.map_or(1.0, |m| m.at(x as i32, y as i32, 0).unwrap_or(m.outside));
            let t = opacity * m;
            for c in 0..4 {
                let i = x * 4 + c;
                row[i] = old[i] + (row[i] - old[i]) * t;
            }
        }
    });
}

/// Blends one placed layer onto the target with the node's mode, opacity
/// and mask.
fn blend_onto(target: &mut Canvas, layer: &Placed, mask: Option<&Placed>, n: &Node, rules: Rules) {
    let (tw, th) = (target.width as i32, target.height as i32);
    // Only the rows and columns the layer covers change, except for the
    // modes that act where the layer is absent too.
    let everywhere = match rules {
        Rules::Adobe { .. } => matches!(n.mode, Mode::DstIn | Mode::DstAtop),
        Rules::Gimp => matches!(gimp_info(n.mode, n.gimp).composite, Composite::Intersection | Composite::ClipToLayer),
    };
    let (ya, yb) = if everywhere { (0, th) } else { (layer.y.max(0), (layer.y + layer.h as i32).min(th)) };
    let (xa, xb) = if everywhere { (0, tw) } else { (layer.x.max(0), (layer.x + layer.w as i32).min(tw)) };
    if ya >= yb || xa >= xb {
        return;
    }
    let w = tw as usize;
    let gimp = gimp_info(n.mode, n.gimp);
    let opacity = n.opacity.clamp(0.0, 1.0);
    let mode = n.mode;
    target.px.par_chunks_mut(w * 4).enumerate().skip(ya as usize).take((yb - ya) as usize).for_each(|(y, row)| {
        for x in xa..xb {
            let i = x as usize * 4;
            let src = match layer.at(x, y as i32, 0) {
                Some(_) => {
                    let j = ((y as i32 - layer.y) as usize * layer.w as usize + (x - layer.x) as usize) * 4;
                    [layer.px[j], layer.px[j + 1], layer.px[j + 2], layer.px[j + 3]]
                }
                None => [0.0; 4],
            };
            let m = mask.map_or(1.0, |m| m.at(x, y as i32, 0).unwrap_or(m.outside)).clamp(0.0, 1.0);
            let dst = [row[i], row[i + 1], row[i + 2], row[i + 3]];
            let out = match rules {
                Rules::Adobe { linear } => adobe(mode, dst, src, opacity * m, linear, x, y as i32),
                Rules::Gimp => gimp_pixel(mode, &gimp, dst, src, opacity, m, x, y as i32),
            };
            row[i..i + 4].copy_from_slice(&out);
        }
    });
}

//endregion

/*┌─────────────────╮
| Adobe and W3C   |
└─────────────────╯*/
//region Adobe and W3C

/// One pixel of a Photoshop or OpenRaster layer over the backdrop: the
/// W3C's "source-over with blending", `cs' = (1 - ab) cs + ab B(cb, cs)`,
/// or a Porter-Duff operator.
#[allow(clippy::too_many_arguments)]
fn adobe(mode: Mode, b: [f32; 4], s: [f32; 4], opacity: f32, linear: bool, x: i32, y: i32) -> [f32; 4] {
    let mut sa = s[3] * opacity;
    let ba = b[3];
    if mode == Mode::Dissolve {
        sa = if dither(x, y) < sa { 1.0 } else { 0.0 };
    }
    let (cb, cs) = ([b[0], b[1], b[2]], [s[0], s[1], s[2]]);
    match mode {
        Mode::DstIn => return [b[0], b[1], b[2], ba * sa],
        Mode::DstOut => return [b[0], b[1], b[2], ba * (1.0 - sa)],
        Mode::SrcAtop => {
            let c = mix3(cb, cs, sa);
            return [c[0], c[1], c[2], ba];
        }
        Mode::DstAtop => {
            let ao = sa;
            if ao <= 0.0 {
                return [0.0; 4];
            }
            let c = [0, 1, 2].map(|k| (cb[k] * ba * sa + cs[k] * sa * (1.0 - ba)) / ao);
            return [c[0], c[1], c[2], ao];
        }
        Mode::Xor => {
            let ao = sa * (1.0 - ba) + ba * (1.0 - sa);
            if ao <= 0.0 {
                return [0.0; 4];
            }
            let c = [0, 1, 2].map(|k| (cs[k] * sa * (1.0 - ba) + cb[k] * ba * (1.0 - sa)) / ao);
            return [c[0], c[1], c[2], ao];
        }
        Mode::Plus => {
            let ao = (sa + ba).min(1.0);
            if ao <= 0.0 {
                return [0.0; 4];
            }
            let c = [0, 1, 2].map(|k| ((cs[k] * sa + cb[k] * ba) / ao).min(if linear { f32::MAX } else { 1.0 }));
            return [c[0], c[1], c[2], ao];
        }
        Mode::Behind => {
            // The layer goes under what is already there.
            return over([cs[0], cs[1], cs[2], sa], b);
        }
        _ => {}
    }
    if sa <= 0.0 {
        return b;
    }
    let blended = if ba > 0.0 { adobe_blend(mode, cb, cs, linear) } else { cs };
    let mixed = [0, 1, 2].map(|k| (1.0 - ba) * cs[k] + ba * blended[k]);
    over([mixed[0], mixed[1], mixed[2], sa], b)
}

/// Straight-alpha source over destination.
#[inline]
fn over(s: [f32; 4], d: [f32; 4]) -> [f32; 4] {
    let ao = s[3] + d[3] * (1.0 - s[3]);
    if ao <= 0.0 {
        return [0.0; 4];
    }
    let k = d[3] * (1.0 - s[3]);
    [(s[0] * s[3] + d[0] * k) / ao, (s[1] * s[3] + d[1] * k) / ao, (s[2] * s[3] + d[2] * k) / ao, ao]
}

#[inline]
fn mix3(a: [f32; 3], b: [f32; 3], t: f32) -> [f32; 3] {
    [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t, a[2] + (b[2] - a[2]) * t]
}

/// A repeatable pseudo-random threshold per pixel, for dissolve.
fn dither(x: i32, y: i32) -> f32 {
    let mut h = (x as u32).wrapping_mul(0x9E37_79B1) ^ (y as u32).wrapping_mul(0x85EB_CA77);
    h ^= h >> 15;
    h = h.wrapping_mul(0x2C1B_3C6D);
    h ^= h >> 12;
    (h & 0xFFFF) as f32 / 65536.0
}

fn adobe_blend(mode: Mode, cb: [f32; 3], cs: [f32; 3], linear: bool) -> [f32; 3] {
    let sep = |f: &dyn Fn(f32, f32) -> f32| [f(cb[0], cs[0]), f(cb[1], cs[1]), f(cb[2], cs[2])];
    let out = match mode {
        Mode::Multiply => sep(&|b, s| b * s),
        Mode::Screen => sep(&|b, s| b + s - b * s),
        Mode::Overlay => sep(&|b, s| hard_light(s, b)),
        Mode::Darken => sep(&f32::min),
        Mode::Lighten => sep(&f32::max),
        Mode::ColorDodge => sep(&|b, s| {
            if b <= 0.0 {
                0.0
            } else if s >= 1.0 {
                1.0
            } else {
                (b / (1.0 - s)).min(1.0)
            }
        }),
        Mode::ColorBurn => sep(&|b, s| {
            if b >= 1.0 {
                1.0
            } else if s <= 0.0 {
                0.0
            } else {
                1.0 - ((1.0 - b) / s).min(1.0)
            }
        }),
        Mode::HardLight => sep(&hard_light),
        Mode::SoftLight => sep(&|b, s| {
            if s <= 0.5 {
                b - (1.0 - 2.0 * s) * b * (1.0 - b)
            } else {
                let d = if b <= 0.25 { ((16.0 * b - 12.0) * b + 4.0) * b } else { b.max(0.0).sqrt() };
                b + (2.0 * s - 1.0) * (d - b)
            }
        }),
        Mode::SoftLightPhotoshop => sep(&|b, s| {
            if s <= 0.5 {
                2.0 * b * s + b * b * (1.0 - 2.0 * s)
            } else {
                2.0 * b * (1.0 - s) + b.max(0.0).sqrt() * (2.0 * s - 1.0)
            }
        }),
        Mode::Difference => sep(&|b, s| (b - s).abs()),
        Mode::Exclusion => sep(&|b, s| b + s - 2.0 * b * s),
        Mode::LinearBurn => sep(&|b, s| (b + s - 1.0).max(0.0)),
        Mode::Addition => sep(&|b, s| b + s),
        Mode::Subtract => sep(&|b, s| (b - s).max(0.0)),
        Mode::Divide => sep(&|b, s| {
            if s <= 0.0 { if b <= 0.0 { 0.0 } else { 1.0 } } else { b / s }
        }),
        Mode::VividLight => sep(&|b, s| {
            if s <= 0.5 {
                if s <= 0.0 { if b >= 1.0 { 1.0 } else { 0.0 } } else { (1.0 - (1.0 - b) / (2.0 * s)).max(0.0) }
            } else if s >= 1.0 {
                if b <= 0.0 { 0.0 } else { 1.0 }
            } else {
                (b / (2.0 * (1.0 - s))).min(1.0)
            }
        }),
        Mode::LinearLight => sep(&|b, s| b + 2.0 * s - 1.0),
        Mode::PinLight => sep(&|b, s| if s <= 0.5 { b.min(2.0 * s) } else { b.max(2.0 * s - 1.0) }),
        Mode::HardMix => sep(&|b, s| if b + s >= 1.0 { 1.0 } else { 0.0 }),
        Mode::DarkerColor => {
            if sum(cs) < sum(cb) {
                cs
            } else {
                cb
            }
        }
        Mode::LighterColor => {
            if sum(cs) > sum(cb) {
                cs
            } else {
                cb
            }
        }
        Mode::Hue => set_lum(set_sat(cs, sat(cb)), lum(cb)),
        Mode::Saturation => set_lum(set_sat(cb, sat(cs)), lum(cb)),
        Mode::Color => set_lum(cs, lum(cb)),
        Mode::Luminosity => set_lum(cb, lum(cs)),
        _ => cs,
    };
    if linear { out } else { out.map(|v| v.clamp(0.0, 1.0)) }
}

fn sum(c: [f32; 3]) -> f32 {
    c[0] + c[1] + c[2]
}

fn hard_light(b: f32, s: f32) -> f32 {
    if s <= 0.5 {
        b * 2.0 * s
    } else {
        let t = 2.0 * s - 1.0;
        b + t - b * t
    }
}

fn lum(c: [f32; 3]) -> f32 {
    0.3 * c[0] + 0.59 * c[1] + 0.11 * c[2]
}

fn clip_color(c: [f32; 3]) -> [f32; 3] {
    let l = lum(c);
    let n = c[0].min(c[1]).min(c[2]);
    let x = c[0].max(c[1]).max(c[2]);
    let mut c = c;
    if n < 0.0 && l - n > 1e-9 {
        c = c.map(|v| l + (v - l) * l / (l - n));
    }
    if x > 1.0 && x - l > 1e-9 {
        c = c.map(|v| l + (v - l) * (1.0 - l) / (x - l));
    }
    c
}

fn set_lum(c: [f32; 3], l: f32) -> [f32; 3] {
    let d = l - lum(c);
    clip_color(c.map(|v| v + d))
}

fn sat(c: [f32; 3]) -> f32 {
    c[0].max(c[1]).max(c[2]) - c[0].min(c[1]).min(c[2])
}

fn set_sat(c: [f32; 3], s: f32) -> [f32; 3] {
    let max = c[0].max(c[1]).max(c[2]);
    let min = c[0].min(c[1]).min(c[2]);
    if max - min <= 1e-9 {
        return [0.0; 3];
    }
    c.map(|v| (v - min) * s / (max - min))
}

//endregion

/*┌─────────────────╮
| GIMP            |
└─────────────────╯*/
//region GIMP

/// A GIMP mode's spaces and compositing, after the layer's overrides.
#[derive(Clone, Copy, Debug)]
pub struct GimpInfo {
    blend: Space,
    space: Space,
    pub composite: Composite,
}

/// The defaults of GIMP's layer mode table (`gimp-layer-modes.c`).
fn gimp_info(mode: Mode, o: GimpOverrides) -> GimpInfo {
    use Mode::*;
    let (blend, space, composite) = match mode {
        Legacy(self::Legacy::Normal) | Legacy(self::Legacy::Behind) => {
            (Space::Perceptual, Space::Perceptual, Composite::Union)
        }
        Legacy(_) => (Space::Perceptual, Space::Perceptual, Composite::ClipToBackdrop),
        Normal | Dissolve | Behind | Merge | PassThrough | Replace | AntiErase | Unknown => {
            (Space::Linear, Space::Linear, Composite::Union)
        }
        Overlay | Screen | Difference | ColorDodge | ColorBurn | HardLight | SoftLight | GrainExtract | GrainMerge
        | VividLight | PinLight | LinearLight | HardMix | Exclusion | LinearBurn => {
            (Space::Perceptual, Space::Linear, Composite::ClipToBackdrop)
        }
        LchHue | LchChroma | LchColor | LchLightness => (Space::Lab, Space::Linear, Composite::ClipToBackdrop),
        HsvHue | HsvSaturation | HslColor | HsvValue => (Space::Perceptual, Space::Linear, Composite::ClipToBackdrop),
        _ => (Space::Linear, Space::Linear, Composite::ClipToBackdrop),
    };
    let legacy = matches!(mode, Legacy(_));
    let immutable_blend = legacy
        || matches!(
            mode,
            LchHue
                | LchChroma
                | LchColor
                | LchLightness
                | HsvHue
                | HsvSaturation
                | HslColor
                | HsvValue
                | Luminance
                | Normal
                | Behind
                | Darken
                | Lighten
                | Erase
                | Merge
                | Split
                | PassThrough
                | Replace
        );
    GimpInfo {
        blend: if immutable_blend { blend } else { o.blend_space.unwrap_or(blend) },
        space: if legacy { space } else { o.composite_space.unwrap_or(space) },
        composite: if legacy || matches!(mode, PassThrough) { composite } else { o.composite.unwrap_or(composite) },
    }
}

/// One pixel of a GIMP layer over the backdrop.  The accumulator and the
/// layer hold linear light; each is moved into the mode's blend space for
/// the blend and into its composite space for compositing.
#[allow(clippy::too_many_arguments)]
fn gimp_pixel(
    mode: Mode,
    info: &GimpInfo,
    d: [f32; 4],
    s: [f32; 4],
    opacity: f32,
    mask: f32,
    x: i32,
    y: i32,
) -> [f32; 4] {
    if let Mode::Legacy(l) = mode {
        return gimp_legacy(l, d, s, opacity * mask);
    }
    match mode {
        Mode::Erase => {
            let a = s[3] * opacity * mask;
            return [d[0], d[1], d[2], d[3] * (1.0 - a)];
        }
        Mode::AntiErase => {
            let a = s[3] * opacity * mask;
            return [d[0], d[1], d[2], d[3] + a * (1.0 - d[3])];
        }
        Mode::Split => {
            let a = s[3] * opacity * mask;
            return [d[0], d[1], d[2], (d[3] - a).max(0.0)];
        }
        Mode::Replace => {
            let t = opacity * mask;
            let a = d[3] + (s[3] - d[3]) * t;
            if a <= 0.0 {
                return [0.0; 4];
            }
            let c = [0, 1, 2].map(|k| (d[k] * d[3] * (1.0 - t) + s[k] * s[3] * t) / a);
            return [c[0], c[1], c[2], a];
        }
        Mode::Behind => {
            let la = s[3] * opacity * mask;
            return over(d, [s[0], s[1], s[2], la]);
        }
        _ => {}
    }
    if mode == Mode::ColorErase {
        return gimp_color_erase(info.composite, d, s, opacity, mask);
    }
    let mut s = s;
    if mode == Mode::Dissolve {
        s[3] = if dither(x, y) < s[3] * opacity * mask { 1.0 } else { 0.0 };
        return gimp_composite(Composite::Union, d, s, s, 1.0, 1.0);
    }
    // 1. The blend, in the blend space.
    let comp = if d[3] != 0.0 && s[3] != 0.0 {
        let (bi, bl) = (to_space(info.blend, [d[0], d[1], d[2]]), to_space(info.blend, [s[0], s[1], s[2]]));
        let c = gimp_blend(mode, bi, bl);
        from_space(info.blend, c)
    } else {
        [s[0], s[1], s[2]]
    };
    // 2. Composite, in the composite space.
    if info.space == Space::Linear {
        gimp_composite(info.composite, d, s, [comp[0], comp[1], comp[2], s[3]], opacity, mask)
    } else {
        let enc = |p: [f32; 4]| {
            let c = to_space(info.space, [p[0], p[1], p[2]]);
            [c[0], c[1], c[2], p[3]]
        };
        let out = gimp_composite(info.composite, enc(d), enc(s), enc([comp[0], comp[1], comp[2], s[3]]), opacity, mask);
        let c = from_space(info.space, [out[0], out[1], out[2]]);
        [c[0], c[1], c[2], out[3]]
    }
}

/// GIMP's colour erase: the layer's colour is taken out of the backdrop,
/// leaving the backdrop more transparent where they agree, composited by
/// GIMP's subtractive rules.
fn gimp_color_erase(composite: Composite, i: [f32; 4], l: [f32; 4], opacity: f32, mask: f32) -> [f32; 4] {
    let mut comp = [0f32; 4];
    if i[3] != 0.0 && l[3] != 0.0 {
        let mut a = 0f32;
        for c in 0..3 {
            let (col, bg) = (i[c].clamp(0.0, 1.0), l[c].clamp(0.0, 1.0));
            if (col - bg).abs() > 1e-6 {
                a = a.max(if col > bg { (col - bg) / (1.0 - bg) } else { (bg - col) / bg });
            }
        }
        if a > 1e-6 {
            for c in 0..3 {
                comp[c] = (i[c] - l[c]) / a + l[c];
            }
        }
        comp[3] = a;
    }
    let la = l[3] * opacity * mask;
    match composite {
        Composite::Union => {
            let na = i[3] + la - (2.0 - comp[3]) * i[3] * la;
            if la == 0.0 || na == 0.0 {
                return [i[0], i[1], i[2], na];
            }
            if i[3] == 0.0 {
                return [l[0], l[1], l[2], na];
            }
            let ratio = i[3] / na;
            let k = 1.0 / i[3] - 1.0;
            let c = [0, 1, 2].map(|b| ratio * (la * (comp[3] * comp[b] + k * l[b] - i[b]) + i[b]));
            [c[0], c[1], c[2], na]
        }
        _ => {
            let ca = comp[3] * la;
            let na = 1.0 - la + ca;
            let rgb = if i[3] == 0.0 || ca == 0.0 {
                [i[0], i[1], i[2]]
            } else {
                let r = ca / na;
                [0, 1, 2].map(|b| comp[b] * r + i[b] * (1.0 - r))
            };
            [rgb[0], rgb[1], rgb[2], na * i[3]]
        }
    }
}

/// GIMP's four ways of laying the blended colour over the backdrop
/// (`gimpoperationlayermode-composite.c`).
fn gimp_composite(mode: Composite, i: [f32; 4], l: [f32; 4], comp: [f32; 4], opacity: f32, mask: f32) -> [f32; 4] {
    match mode {
        Composite::Union => {
            let la = l[3] * opacity * mask;
            let na = la + (1.0 - la) * i[3];
            if la == 0.0 || na == 0.0 {
                return [i[0], i[1], i[2], na];
            }
            if i[3] == 0.0 {
                return [l[0], l[1], l[2], na];
            }
            let r = la / na;
            let c = [0, 1, 2].map(|b| r * (i[3] * (comp[b] - l[b]) + l[b] - i[b]) + i[b]);
            [c[0], c[1], c[2], na]
        }
        Composite::ClipToBackdrop => {
            let la = comp[3] * opacity * mask;
            if i[3] == 0.0 || la == 0.0 {
                return i;
            }
            let c = [0, 1, 2].map(|b| comp[b] * la + i[b] * (1.0 - la));
            [c[0], c[1], c[2], i[3]]
        }
        Composite::ClipToLayer => {
            let la = l[3] * opacity * mask;
            if la == 0.0 {
                return [i[0], i[1], i[2], 0.0];
            }
            if i[3] == 0.0 {
                return [l[0], l[1], l[2], la];
            }
            let c = [0, 1, 2].map(|b| comp[b] * i[3] + l[b] * (1.0 - i[3]));
            [c[0], c[1], c[2], la]
        }
        Composite::Intersection => {
            let na = i[3] * comp[3] * opacity * mask;
            if na == 0.0 {
                return [i[0], i[1], i[2], 0.0];
            }
            [comp[0], comp[1], comp[2], na]
        }
    }
}

fn safe_div(a: f32, b: f32) -> f32 {
    if a.abs() > 1e-6 { (a / b).clamp(-1e6, 1e6) } else { 0.0 }
}

/// GIMP's blend functions (`gimpoperationlayermode-blend.c`), on colours
/// already in the mode's blend space.
fn gimp_blend(mode: Mode, i: [f32; 3], l: [f32; 3]) -> [f32; 3] {
    let sep = |f: &dyn Fn(f32, f32) -> f32| [f(i[0], l[0]), f(i[1], l[1]), f(i[2], l[2])];
    match mode {
        Mode::Addition => sep(&|a, b| a + b),
        Mode::ColorBurn => sep(&|a, b| 1.0 - safe_div(1.0 - a, b)),
        Mode::Darken => sep(&f32::min),
        Mode::Lighten => sep(&f32::max),
        Mode::Difference => sep(&|a, b| (a - b).abs()),
        Mode::Divide => sep(&safe_div),
        Mode::ColorDodge => sep(&|a, b| safe_div(a, 1.0 - b)),
        Mode::Exclusion => sep(&|a, b| 0.5 - 2.0 * (a - 0.5) * (b - 0.5)),
        Mode::GrainExtract => sep(&|a, b| a - b + 0.5),
        Mode::GrainMerge => sep(&|a, b| a + b - 0.5),
        Mode::HardMix => sep(&|a, b| if a + b < 1.0 { 0.0 } else { 1.0 }),
        Mode::HardLight => sep(&|a, b| {
            if b > 0.5 {
                let v = (1.0 - a) * (1.0 - (b - 0.5) * 2.0);
                (1.0 - v).min(1.0)
            } else {
                (a * (b * 2.0)).min(1.0)
            }
        }),
        Mode::LinearBurn => sep(&|a, b| a + b - 1.0),
        Mode::LinearLight => sep(&|a, b| if b <= 0.5 { a + 2.0 * b - 1.0 } else { a + 2.0 * (b - 0.5) }),
        Mode::Multiply => sep(&|a, b| a * b),
        Mode::Overlay => sep(&|a, b| if a < 0.5 { 2.0 * a * b } else { 1.0 - 2.0 * (1.0 - b) * (1.0 - a) }),
        Mode::PinLight => sep(&|a, b| if b > 0.5 { a.max(2.0 * (b - 0.5)) } else { a.min(2.0 * b) }),
        Mode::Screen => sep(&|a, b| 1.0 - (1.0 - a) * (1.0 - b)),
        Mode::SoftLight => sep(&|a, b| {
            let m = a * b;
            let s = 1.0 - (1.0 - a) * (1.0 - b);
            (1.0 - a) * m + a * s
        }),
        Mode::Subtract => sep(&|a, b| a - b),
        Mode::VividLight => sep(&|a, b| {
            if b <= 0.5 { (1.0 - safe_div(1.0 - a, 2.0 * b)).max(0.0) } else { safe_div(a, 2.0 * (1.0 - b)).min(1.0) }
        }),
        Mode::HsvHue => {
            let (smin, smax) = (l[0].min(l[1]).min(l[2]), l[0].max(l[1]).max(l[2]));
            let sd = smax - smin;
            if sd > 1e-6 {
                let (dmin, dmax) = (i[0].min(i[1]).min(i[2]), i[0].max(i[1]).max(i[2]));
                let ds = if dmax != 0.0 { (dmax - dmin) / dmax } else { 0.0 };
                let ratio = ds * dmax / sd;
                let offset = dmax - smax * ratio;
                l.map(|v| v * ratio + offset)
            } else {
                i
            }
        }
        Mode::HsvSaturation => {
            let (dmin, dmax) = (i[0].min(i[1]).min(i[2]), i[0].max(i[1]).max(i[2]));
            let dd = dmax - dmin;
            if dd > 1e-6 {
                let (smin, smax) = (l[0].min(l[1]).min(l[2]), l[0].max(l[1]).max(l[2]));
                let ss = if smax != 0.0 { (smax - smin) / smax } else { 0.0 };
                let ratio = ss * dmax / dd;
                let offset = (1.0 - ratio) * dmax;
                i.map(|v| v * ratio + offset)
            } else {
                [dmax; 3]
            }
        }
        Mode::HsvValue => {
            let dv = i[0].max(i[1]).max(i[2]);
            let sv = l[0].max(l[1]).max(l[2]);
            if dv.abs() > 1e-6 { i.map(|v| v * sv / dv) } else { [sv; 3] }
        }
        Mode::HslColor => {
            let (dmin, dmax) = (i[0].min(i[1]).min(i[2]), i[0].max(i[1]).max(i[2]));
            let mut dl = (dmin + dmax) / 2.0;
            let (smin, smax) = (l[0].min(l[1]).min(l[2]), l[0].max(l[1]).max(l[2]));
            let mut sl = (smin + smax) / 2.0;
            if sl.abs() > 1e-6 && (1.0 - sl).abs() > 1e-6 {
                let (dh, sh) = (dl > 0.5, sl > 0.5);
                dl = dl.min(1.0 - dl);
                sl = sl.min(1.0 - sl);
                let ratio = dl / sl;
                let mut offset = 0.0;
                if dh {
                    offset += 1.0 - 2.0 * dl;
                }
                if sh {
                    offset += 2.0 * dl - ratio;
                }
                l.map(|v| v * ratio + offset)
            } else {
                [dl; 3]
            }
        }
        Mode::LchChroma => {
            let c1 = i[1].hypot(i[2]);
            if c1 > 1e-6 {
                let c2 = l[1].hypot(l[2]);
                [i[0], c2 * i[1] / c1, c2 * i[2] / c1]
            } else {
                i
            }
        }
        Mode::LchColor => [i[0], l[1], l[2]],
        Mode::LchHue => {
            let c2 = l[1].hypot(l[2]);
            if c2 > 1e-6 {
                let c1 = i[1].hypot(i[2]);
                [i[0], c1 * l[1] / c2, c1 * l[2] / c2]
            } else {
                i
            }
        }
        Mode::LchLightness => [l[0], i[1], i[2]],
        Mode::LumaDarken => {
            if luma(i) <= luma(l) {
                i
            } else {
                l
            }
        }
        Mode::LumaLighten => {
            if luma(i) >= luma(l) {
                i
            } else {
                l
            }
        }
        Mode::Luminance => {
            let r = safe_div(luma(l), luma(i));
            i.map(|v| v * r)
        }
        // Merge, colour erase and unknown modes: the layer's colour.
        _ => l,
    }
}

/// Rec. 709 luminance of linear RGB, as babl computes it for sRGB.
fn luma(c: [f32; 3]) -> f32 {
    0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2]
}

/// GIMP 2.8's modes: blended in gamma-encoded RGB, composited by
/// `min(backdrop alpha, layer alpha) × opacity`, keeping the backdrop's
/// alpha (`layer-modes-legacy/`).
fn gimp_legacy(mode: Legacy, d: [f32; 4], s: [f32; 4], opacity: f32) -> [f32; 4] {
    let i = [srgb_encode(d[0]), srgb_encode(d[1]), srgb_encode(d[2])];
    let l = [srgb_encode(s[0]), srgb_encode(s[1]), srgb_encode(s[2])];
    if matches!(mode, Legacy::Normal | Legacy::Behind) {
        let la = s[3] * opacity;
        let (top, bottom) = if mode == Legacy::Normal {
            ([l[0], l[1], l[2], la], [i[0], i[1], i[2], d[3]])
        } else {
            ([i[0], i[1], i[2], d[3]], [l[0], l[1], l[2], la])
        };
        let o = over(top, bottom);
        return [srgb_decode(o[0]), srgb_decode(o[1]), srgb_decode(o[2]), o[3]];
    }
    let ca = d[3].min(s[3]) * opacity;
    let na = d[3] + (1.0 - d[3]) * ca;
    if ca == 0.0 || na == 0.0 {
        return d;
    }
    let half = 128.0 / 255.0;
    let sep = |f: &dyn Fn(f32, f32) -> f32| [f(i[0], l[0]), f(i[1], l[1]), f(i[2], l[2])];
    let comp = match mode {
        Legacy::Multiply => sep(&|a, b| a * b),
        Legacy::Screen => sep(&|a, b| 1.0 - (1.0 - a) * (1.0 - b)),
        Legacy::Difference => sep(&|a, b| (a - b).abs()),
        Legacy::Addition => sep(&|a, b| (a + b).clamp(0.0, 1.0)),
        Legacy::Subtract => sep(&|a, b| (a - b).clamp(0.0, 1.0)),
        Legacy::Darken => sep(&f32::min),
        Legacy::Lighten => sep(&f32::max),
        Legacy::Divide => sep(&|a, b| if b <= 0.0 { if a > 0.0 { 1.0 } else { 0.0 } } else { (a / b).clamp(0.0, 1.0) }),
        Legacy::Dodge => {
            sep(&|a, b| if b >= 1.0 { if a > 0.0 { 1.0 } else { 0.0 } } else { (a / (1.0 - b)).clamp(0.0, 1.0) })
        }
        Legacy::Burn => {
            sep(&|a, b| if b <= 0.0 { if a < 1.0 { 0.0 } else { 1.0 } } else { (1.0 - (1.0 - a) / b).clamp(0.0, 1.0) })
        }
        Legacy::HardLight => sep(&|a, b| {
            if b > half { (1.0 - (1.0 - a) * (1.0 - (b - half) * 2.0)).min(1.0) } else { (a * b * 2.0).min(1.0) }
        }),
        Legacy::SoftLight => sep(&|a, b| {
            let m = a * b;
            let s = 1.0 - (1.0 - a) * (1.0 - b);
            (1.0 - a) * m + a * s
        }),
        Legacy::GrainExtract => sep(&|a, b| (a - b + half).clamp(0.0, 1.0)),
        Legacy::GrainMerge => sep(&|a, b| (a + b - half).clamp(0.0, 1.0)),
        Legacy::HsvHue | Legacy::HsvSaturation | Legacy::HsvValue => {
            let (mut o, ls) = (rgb_to_hsv(i), rgb_to_hsv(l));
            match mode {
                // Hue from a grey layer would paint black red.
                Legacy::HsvHue if ls[1] != 0.0 => o[0] = ls[0],
                Legacy::HsvSaturation => o[1] = ls[1],
                Legacy::HsvValue => o[2] = ls[2],
                _ => {}
            }
            hsv_to_rgb(o)
        }
        Legacy::HslColor => {
            let (o, ls) = (rgb_to_hsl(i), rgb_to_hsl(l));
            hsl_to_rgb([ls[0], ls[1], o[2]])
        }
        Legacy::ColorErase | Legacy::Normal | Legacy::Behind => l,
    };
    let r = ca / na;
    let o = [0, 1, 2].map(|b| comp[b] * r + i[b] * (1.0 - r));
    [srgb_decode(o[0]), srgb_decode(o[1]), srgb_decode(o[2]), d[3]]
}

/// GIMP 2.8's HSV: hue 0 to 1, 0 for greys.
fn rgb_to_hsv(c: [f32; 3]) -> [f32; 3] {
    let max = c[0].max(c[1]).max(c[2]);
    let min = c[0].min(c[1]).min(c[2]);
    let d = max - min;
    if d <= 0.0001 {
        return [0.0, 0.0, max];
    }
    let mut h = if c[0] == max {
        let h = (c[1] - c[2]) / d;
        if h < 0.0 { h + 6.0 } else { h }
    } else if c[1] == max {
        2.0 + (c[2] - c[0]) / d
    } else {
        4.0 + (c[0] - c[1]) / d
    };
    h /= 6.0;
    [h, d / max, max]
}

fn hsv_to_rgb(hsv: [f32; 3]) -> [f32; 3] {
    let [h, s, v] = hsv;
    if s == 0.0 {
        return [v; 3];
    }
    let h = if h == 1.0 { 0.0 } else { h } * 6.0;
    let i = h as i32;
    let f = h - i as f32;
    let (w, q, t) = (v * (1.0 - s), v * (1.0 - s * f), v * (1.0 - s * (1.0 - f)));
    match i {
        0 => [v, t, w],
        1 => [q, v, w],
        2 => [w, v, t],
        3 => [w, q, v],
        4 => [t, w, v],
        _ => [v, w, q],
    }
}

/// GIMP 2.8's HSL: hue 0 to 1, -1 for greys.
fn rgb_to_hsl(c: [f32; 3]) -> [f32; 3] {
    let max = c[0].max(c[1]).max(c[2]);
    let min = c[0].min(c[1]).min(c[2]);
    let l = (max + min) / 2.0;
    if max == min {
        return [-1.0, 0.0, l];
    }
    let s = if l <= 0.5 { (max - min) / (max + min) } else { (max - min) / (2.0 - max - min) };
    let d = max - min;
    let mut h = if c[0] == max {
        (c[1] - c[2]) / d
    } else if c[1] == max {
        2.0 + (c[2] - c[0]) / d
    } else {
        4.0 + (c[0] - c[1]) / d
    } / 6.0;
    if h < 0.0 {
        h += 1.0;
    }
    [h, s, l]
}

fn hsl_to_rgb(hsl: [f32; 3]) -> [f32; 3] {
    let [h, s, l] = hsl;
    if s == 0.0 {
        return [l; 3];
    }
    let m2 = if l <= 0.5 { l * (1.0 + s) } else { l + s - l * s };
    let m1 = 2.0 * l - m2;
    let value = |hue: f32| {
        let hue = if hue > 6.0 {
            hue - 6.0
        } else if hue < 0.0 {
            hue + 6.0
        } else {
            hue
        };
        if hue < 1.0 {
            m1 + (m2 - m1) * hue
        } else if hue < 3.0 {
            m2
        } else if hue < 4.0 {
            m1 + (m2 - m1) * (4.0 - hue)
        } else {
            m1
        }
    };
    [value(h * 6.0 + 2.0), value(h * 6.0), value(h * 6.0 - 2.0)]
}

fn to_space(space: Space, c: [f32; 3]) -> [f32; 3] {
    match space {
        Space::Linear => c,
        Space::Perceptual => c.map(srgb_encode),
        Space::Lab => linear_to_lab(c),
    }
}

fn from_space(space: Space, c: [f32; 3]) -> [f32; 3] {
    match space {
        Space::Linear => c,
        Space::Perceptual => c.map(srgb_decode),
        Space::Lab => lab_to_linear(c),
    }
}

//endregion

/*┌─────────────────╮
| Colour          |
└─────────────────╯*/
//region Colour

fn srgb8_table() -> &'static [f32; 256] {
    static T: std::sync::OnceLock<[f32; 256]> = std::sync::OnceLock::new();
    T.get_or_init(|| std::array::from_fn(|i| srgb_decode(i as f32 / 255.0)))
}

fn srgb16_table() -> &'static [f32] {
    static T: std::sync::OnceLock<Vec<f32>> = std::sync::OnceLock::new();
    T.get_or_init(|| (0..65536).map(|i| srgb_decode(i as f32 / 65535.0)).collect())
}

/// The sRGB transfer curve, continued straight below 0 and along the curve
/// above 1, as babl (GIMP's colour library) does.
pub fn srgb_encode(v: f32) -> f32 {
    if v <= 0.003_130_8 { v * 12.92 } else { 1.055 * v.powf(1.0 / 2.4) - 0.055 }
}

pub fn srgb_decode(v: f32) -> f32 {
    if v <= 0.040_45 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
}

/// Linear sRGB to CIE L*a*b* relative to D50, as babl's "CIE Lab float"
/// (Bradford-adapted sRGB primaries).
pub fn linear_to_lab(c: [f32; 3]) -> [f32; 3] {
    let [r, g, b] = c;
    let x = 0.436_065_7 * r + 0.385_151_5 * g + 0.143_078_5 * b;
    let y = 0.222_493_2 * r + 0.716_887_3 * g + 0.060_619_3 * b;
    let z = 0.013_923_9 * r + 0.097_081_5 * g + 0.714_099_6 * b;
    let f = |t: f32| if t > 216.0 / 24389.0 { t.cbrt() } else { (24389.0 / 27.0 * t + 16.0) / 116.0 };
    let (fx, fy, fz) = (f(x / 0.964_212), f(y), f(z / 0.825_188));
    [116.0 * fy - 16.0, 500.0 * (fx - fy), 200.0 * (fy - fz)]
}

pub fn lab_to_linear(lab: [f32; 3]) -> [f32; 3] {
    let fy = (lab[0] + 16.0) / 116.0;
    let fx = fy + lab[1] / 500.0;
    let fz = fy - lab[2] / 200.0;
    let inv = |f: f32| {
        let t = f * f * f;
        if t > 216.0 / 24389.0 { t } else { (116.0 * f - 16.0) * 27.0 / 24389.0 }
    };
    let (x, y, z) = (inv(fx) * 0.964_212, inv(fy), inv(fz) * 0.825_188);
    [
        3.134_051_3 * x - 1.617_385_2 * y - 0.490_632_5 * z,
        -0.978_795_6 * x + 1.916_254_2 * y + 0.033_442_6 * z,
        0.071_976_7 * x - 0.228_912_4 * y + 1.405_386_8 * z,
    ]
}

/// A canvas to 8-bit sRGB with straight alpha; `linear` when the canvas
/// holds linear light.
pub fn to_image8(c: &Canvas, linear: bool) -> slerp_scene::Image8 {
    let mut pixels = vec![0u8; c.px.len()];
    pixels.par_chunks_mut(4).zip(c.px.par_chunks(4)).for_each(|(o, p)| {
        for k in 0..3 {
            let v = if linear { srgb_encode(p[k]) } else { p[k] };
            o[k] = (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
        }
        o[3] = (p[3].clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
    });
    slerp_scene::Image8 { width: c.width, height: c.height, pixels }
}

//endregion

#[cfg(test)]
mod tests {
    use super::*;

    struct Layers(Vec<Raster>);

    impl Source for Layers {
        fn layer(&self, id: usize) -> Result<Raster, String> {
            self.0.get(id).cloned().ok_or_else(|| "missing".into())
        }
        fn mask(&self, id: usize) -> Result<Raster, String> {
            self.0.get(id).cloned().ok_or_else(|| "missing".into())
        }
    }

    fn solid(w: u32, h: u32, c: [f32; 4]) -> Raster {
        Raster { width: w, height: h, channels: 4, samples: Samples::F32((0..w * h).flat_map(|_| c).collect()) }
    }

    fn node(id: usize, mode: Mode, opacity: f32) -> Node {
        Node {
            name: format!("layer {id}"),
            visible: true,
            opacity,
            mode,
            gimp: GimpOverrides::default(),
            clip: false,
            mask: None,
            kind: Kind::Raster { id, x: 0, y: 0 },
            slot: 0,
        }
    }

    fn run(nodes: &mut [Node], src: &Layers, rules: Rules, scale: f32) -> Canvas {
        flatten(nodes);
        let mut problems = Vec::new();
        let c = compose(nodes, src, rules, 4, 4, scale, &[], &mut problems);
        assert!(problems.is_empty(), "{problems:?}");
        c
    }

    fn px(c: &Canvas, x: usize, y: usize) -> [f32; 4] {
        let i = (y * c.width as usize + x) * 4;
        [c.px[i], c.px[i + 1], c.px[i + 2], c.px[i + 3]]
    }

    fn close(a: [f32; 4], b: [f32; 4]) -> bool {
        a.iter().zip(b).all(|(x, y)| (x - y).abs() < 2e-3)
    }

    #[test]
    fn normal_opacity_and_multiply_follow_the_w3c() {
        let src = Layers(vec![solid(4, 4, [0.8, 0.4, 0.2, 1.0]), solid(4, 4, [0.5, 0.5, 1.0, 1.0])]);
        let rules = Rules::Adobe { linear: false };
        let c = run(&mut [node(0, Mode::Normal, 1.0), node(1, Mode::Normal, 0.5)], &src, rules, 1.0);
        assert!(close(px(&c, 1, 1), [0.65, 0.45, 0.6, 1.0]), "{:?}", px(&c, 1, 1));
        let c = run(&mut [node(0, Mode::Normal, 1.0), node(1, Mode::Multiply, 1.0)], &src, rules, 1.0);
        assert!(close(px(&c, 0, 0), [0.4, 0.2, 0.2, 1.0]));
        // A multiply layer over nothing is just the layer.
        let c = run(&mut [node(1, Mode::Multiply, 1.0)], &src, rules, 1.0);
        assert!(close(px(&c, 0, 0), [0.5, 0.5, 1.0, 1.0]));
    }

    #[test]
    fn gimp_normal_blends_in_linear_light_and_legacy_in_srgb() {
        let src = Layers(vec![solid(4, 4, [0.0, 0.0, 0.0, 1.0]), solid(4, 4, [1.0, 1.0, 1.0, 1.0])]);
        let c = run(&mut [node(0, Mode::Normal, 1.0), node(1, Mode::Normal, 0.5)], &src, Rules::Gimp, 1.0);
        assert!(close(px(&c, 0, 0), [0.5, 0.5, 0.5, 1.0]));
        let c =
            run(&mut [node(0, Mode::Normal, 1.0), node(1, Mode::Legacy(Legacy::Normal), 0.5)], &src, Rules::Gimp, 1.0);
        // Half way in sRGB is about 0.214 linear.
        assert!(close(px(&c, 0, 0), [0.214, 0.214, 0.214, 1.0]), "{:?}", px(&c, 0, 0));
        // The bottom layer of a stack is drawn as it is; above a clear one,
        // clip-to-backdrop modes add no alpha.
        let c = run(&mut [node(1, Mode::Multiply, 1.0)], &src, Rules::Gimp, 1.0);
        assert_eq!(px(&c, 0, 0), [1.0, 1.0, 1.0, 1.0]);
        let src = Layers(vec![solid(4, 4, [0.0; 4]), solid(4, 4, [1.0, 1.0, 1.0, 1.0])]);
        let c = run(&mut [node(0, Mode::Normal, 1.0), node(1, Mode::Multiply, 1.0)], &src, Rules::Gimp, 1.0);
        assert_eq!(px(&c, 0, 0)[3], 0.0);
    }

    #[test]
    fn groups_masks_clipping_and_scale() {
        let mut mask = Raster { width: 2, height: 4, channels: 1, samples: Samples::F32(vec![1.0; 8]) };
        if let Samples::F32(v) = &mut mask.samples {
            v[0] = 0.0;
        }
        let src = Layers(vec![solid(4, 4, [1.0, 0.0, 0.0, 1.0]), solid(2, 2, [0.0, 1.0, 0.0, 1.0]), mask]);
        let rules = Rules::Adobe { linear: false };
        let mut green = node(1, Mode::Normal, 1.0);
        green.kind = Kind::Raster { id: 1, x: 2, y: 2 };
        green.mask = Some(Mask { id: 2, x: 2, y: 0, outside: 0.0 });
        let group = Node { kind: Kind::Group { children: vec![green], isolated: true }, ..node(9, Mode::Normal, 1.0) };
        let mut nodes = vec![node(0, Mode::Normal, 1.0), group];
        let c = run(&mut nodes, &src, rules, 1.0);
        assert!(close(px(&c, 0, 0), [1.0, 0.0, 0.0, 1.0]));
        assert!(close(px(&c, 3, 3), [0.0, 1.0, 0.0, 1.0]));
        // Hidden through `shown`.
        let mut problems = Vec::new();
        let hidden = compose(&nodes, &src, rules, 4, 4, 1.0, &[true, false, true], &mut problems);
        assert!(close(px(&hidden, 3, 3), [1.0, 0.0, 0.0, 1.0]));
        // At half size the green quarter becomes one averaged pixel.
        let half = run(&mut nodes, &src, rules, 0.5);
        assert_eq!((half.width, half.height), (2, 2));
        assert!(close(px(&half, 1, 1), [0.0, 1.0, 0.0, 1.0]), "{:?}", px(&half, 1, 1));
        // A layer clipped to a small base shows only over it.
        let mut clipped = node(0, Mode::Normal, 1.0);
        clipped.clip = true;
        let mut base = node(1, Mode::Normal, 1.0);
        base.kind = Kind::Raster { id: 1, x: 2, y: 2 };
        let c = run(&mut [base, clipped], &src, rules, 1.0);
        assert_eq!(px(&c, 0, 0)[3], 0.0);
        assert!(close(px(&c, 2, 2), [1.0, 0.0, 0.0, 1.0]));
    }

    #[test]
    fn lab_round_trips() {
        for c in [[0.2, 0.5, 0.9], [1.0, 1.0, 1.0], [0.0, 0.0, 0.0], [0.7, 0.1, 0.3]] {
            let back = lab_to_linear(linear_to_lab(c));
            assert!(c.iter().zip(back).all(|(a, b)| (a - b).abs() < 1e-3), "{c:?} {back:?}");
        }
        let white = linear_to_lab([1.0, 1.0, 1.0]);
        assert!((white[0] - 100.0).abs() < 0.05 && white[1].abs() < 0.05 && white[2].abs() < 0.05, "{white:?}");
    }
}
