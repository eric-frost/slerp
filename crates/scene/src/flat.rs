//! Flat assets that their own crates draw on request: layered and animated
//! 2D pictures (Aseprite sprites, Tiled maps) and colour lookup tables.
//!
//! The shared model only describes what thumbnails and the viewer need to
//! offer, such as the layers to toggle and the clips to play.  How pixels
//! are composed stays with the format's crate, behind these traits, since
//! blend modes, tile layouts and table interpolation mean nothing to
//! anyone else.

use crate::{Image8, Info};

/*┌─────────────────╮
| Canvases        |
└─────────────────╯*/
//region Canvases

/// A 2D picture built from layers and, for sprites, frames: an Aseprite
/// sprite or a Tiled map.
pub trait Canvas: Send + Sync {
    /// The format, facts for the inspector, and problems the loader worked
    /// around (a missing tileset image, say).
    fn info(&self) -> &Info;

    /// Width and height of a composed frame at full size, in pixels.
    fn size(&self) -> (u32, u32);

    /// The layers, bottom first, with each group before the layers inside
    /// it.
    fn layers(&self) -> &[CanvasLayer];

    /// How long each frame shows, in seconds.  A still picture has one
    /// frame.
    fn frames(&self) -> &[f32];

    /// Named runs of frames to play, such as Aseprite's tags.
    fn clips(&self) -> &[FrameClip];

    /// How many times the whole animation plays before it stops, for
    /// pictures that say (GIF, APNG and WebP files do); `None` plays for
    /// ever, as sprites and maps do.
    fn loops(&self) -> Option<u32> {
        None
    }

    /// Draws frame `frame` showing the layers `shown` marks, one entry per
    /// layer (a layer without an entry keeps its own visibility; hiding a
    /// group hides what is inside it), at `scale` times full size (never
    /// more than full size).  The result has straight alpha and is about
    /// `size() * scale`; its own width and height say exactly.
    fn compose(&self, frame: usize, shown: &[bool], scale: f32) -> Image8;
}

#[derive(Clone, Debug, Default)]
pub struct CanvasLayer {
    pub name: String,
    /// Whether the file shows it.
    pub visible: bool,
    /// How many groups it sits inside: 0 at the top level.
    pub depth: u32,
    /// A group of other layers rather than a layer of its own.
    pub group: bool,
    /// A few words on what the layer is, such as "tiles", "objects" or
    /// "multiply, 50 %", for the viewer's layer list.
    pub note: String,
}

/// The order a clip plays its frames in, as Aseprite's tags name them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum PlayDirection {
    #[default]
    Forward,
    Reverse,
    /// Forward, then back again.
    PingPong,
    /// Backward, then forward again.
    PingPongReverse,
}

#[derive(Clone, Debug)]
pub struct FrameClip {
    pub name: String,
    /// The first and last frame, inclusive, with `from <= to`.
    pub from: usize,
    pub to: usize,
    pub direction: PlayDirection,
    /// How many times it plays, each way of a ping-pong counting once; 0
    /// means it loops for ever.
    pub repeat: u32,
}

//endregion

/*┌─────────────────╮
| Lookup tables   |
└─────────────────╯*/
//region Lookup tables

/// A colour transform read from a lookup table file (`.cube`, `.3dl`),
/// which thumbnails and the viewer show applied to photographs.
pub trait ColorLut: Send + Sync {
    /// The format, title, table sizes and input ranges.
    fn info(&self) -> &Info;

    /// Grades RGBA pixels holding sRGB-encoded colour, in place, the way
    /// photo and video editors apply such tables: to the stored values,
    /// not to linear light.  Alpha is left alone.
    fn apply(&self, rgba: &mut [u8]);
}

//endregion
