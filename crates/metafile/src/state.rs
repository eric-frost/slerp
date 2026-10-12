//! What a Windows device context remembers between drawing records: the
//! coordinate mapping, the selected pen, brush and font, colours and modes,
//! and the clip.  `SaveDC` and `RestoreDC` copy it whole.

use crate::Rgb;
use crate::affine::Affine;
use std::rc::Rc;
use tiny_skia::{LineCap, LineJoin, Mask};

/*┌───────────────────╮
| Constants           |
└───────────────────╯*/
//region Constants

pub const MM_TEXT: u32 = 1;
pub const MM_LOMETRIC: u32 = 2;
pub const MM_HIMETRIC: u32 = 3;
pub const MM_LOENGLISH: u32 = 4;
pub const MM_HIENGLISH: u32 = 5;
pub const MM_TWIPS: u32 = 6;
pub const MM_ISOTROPIC: u32 = 7;
pub const MM_ANISOTROPIC: u32 = 8;

pub const TA_UPDATECP: u32 = 1;
pub const TA_RIGHT: u32 = 2;
pub const TA_CENTER: u32 = 6;
pub const TA_BOTTOM: u32 = 8;
pub const TA_BASELINE: u32 = 24;

/// Colour references with a flag in the top byte name a palette entry
/// (`PALETTEINDEX`) rather than a colour.
pub const PALETTE_INDEX: u32 = 0x0100_0000;

//endregion

/*┌───────────────────╮
| Pens and brushes    |
└───────────────────╯*/
//region Pens and brushes

/// How a pen's line is broken up.
#[derive(Clone, Debug, PartialEq)]
pub enum Dashes {
    Solid,
    Dash,
    Dot,
    DashDot,
    DashDotDot,
    /// `PS_USERSTYLE`: lengths in units of the pen width, on then off.
    User(Vec<f32>),
    /// `PS_ALTERNATE`: every other device pixel.
    Alternate,
}

#[derive(Clone, Debug)]
pub struct Pen {
    pub dashes: Dashes,
    /// In logical units; zero is a one-pixel cosmetic line.
    pub width: f64,
    pub color: Rgb,
    /// Geometric pens obey their end cap and join; cosmetic ones draw
    /// plain thin lines.
    pub geometric: bool,
    pub cap: LineCap,
    pub join: LineJoin,
    /// True for `PS_NULL`: draws nothing.
    pub null: bool,
    /// `PS_INSIDEFRAME`: shapes draw the line inside their outline.
    pub inside_frame: bool,
}

impl Pen {
    pub fn solid(color: Rgb) -> Pen {
        Pen {
            dashes: Dashes::Solid,
            width: 0.0,
            color,
            geometric: false,
            cap: LineCap::Round,
            join: LineJoin::Round,
            null: false,
            inside_frame: false,
        }
    }

    pub fn none() -> Pen {
        Pen { null: true, ..Pen::solid([0, 0, 0]) }
    }
}

/// A tiling of two colours from a small bitmap: hatch brushes and
/// monochrome pattern brushes.
#[derive(Clone, Debug)]
pub struct MonoTile {
    pub width: usize,
    pub height: usize,
    /// One flag per pixel, true where the foreground shows.
    pub on: Vec<bool>,
}

#[derive(Clone, Debug)]
pub enum Brush {
    Null,
    Solid(Rgb),
    /// A hatch drawn in its colour; the gaps show the background colour
    /// when the background mode is opaque.
    Hatch(Rc<MonoTile>, Rgb),
    /// A monochrome pattern: set bits take the text colour, clear bits the
    /// background colour.
    Mono(Rc<MonoTile>),
    /// A colour pattern from a bitmap.
    Pattern(Rc<tiny_skia::Pixmap>),
}

/// The classic 8x8 hatch patterns (`HS_HORIZONTAL` to `HS_DIAGCROSS`).
pub fn hatch_tile(style: u32) -> MonoTile {
    let mut on = vec![false; 64];
    for y in 0..8usize {
        for x in 0..8usize {
            on[y * 8 + x] = match style {
                0 => y == 4,
                1 => x == 4,
                // Downward `\`, upward `/`, crossed, and both diagonals.
                2 => x == y,
                3 => x + y == 7,
                4 => x == 4 || y == 4,
                _ => x == y || x + y == 7,
            };
        }
    }
    MonoTile { width: 8, height: 8, on }
}

//endregion

/*┌───────────────────╮
| Fonts               |
└───────────────────╯*/
//region Fonts

#[derive(Clone, Debug, Default)]
pub struct Font {
    /// Negative: the character height (em size); positive: the cell height
    /// (ascent plus descent); zero: a default size.  Logical units.
    pub height: f64,
    /// Rotation of the baseline in tenths of a degree, counter-clockwise.
    pub escapement: f64,
    pub weight: u32,
    pub italic: bool,
    pub underline: bool,
    pub strike_out: bool,
    pub charset: u8,
    pub pitch_family: u8,
    pub face: String,
}

//endregion

/*┌───────────────────╮
| Device context      |
└───────────────────╯*/
//region Device context

#[derive(Clone)]
pub struct Dc {
    pub map_mode: u32,
    pub win_org: (f64, f64),
    pub win_ext: (f64, f64),
    pub vp_org: (f64, f64),
    pub vp_ext: (f64, f64),
    /// EMF's world transform: logical (page) space to the space the
    /// mapping mode maps to the device.
    pub world: Affine,
    /// Logical coordinates straight to canvas pixels; kept up to date by
    /// `Gdi::update_transform`.
    pub total: Affine,
    pub pen: Rc<Pen>,
    pub brush: Rc<Brush>,
    pub font: Rc<Font>,
    pub palette: Rc<Vec<Rgb>>,
    pub text_color: Rgb,
    /// Opacity of text, 1 unless a GDI+ string asks for less.
    pub text_alpha: f32,
    pub bk_color: Rgb,
    pub bk_opaque: bool,
    pub rop2: u32,
    pub winding: bool,
    pub stretch_mode: u32,
    pub text_align: u32,
    pub text_extra: f64,
    pub break_extra: f64,
    pub break_count: f64,
    /// Current position, in logical coordinates.
    pub cur: (f64, f64),
    pub arc_ccw: bool,
    pub miter_limit: f32,
    pub brush_org: (f64, f64),
    pub clip: Option<Rc<Mask>>,
    /// True when the layout runs right to left (`SetLayout`).
    pub rtl: bool,
}

impl Dc {
    pub fn new() -> Dc {
        Dc {
            map_mode: MM_TEXT,
            win_org: (0.0, 0.0),
            win_ext: (1.0, 1.0),
            vp_org: (0.0, 0.0),
            vp_ext: (1.0, 1.0),
            world: Affine::IDENTITY,
            total: Affine::IDENTITY,
            pen: Rc::new(Pen::solid([0, 0, 0])),
            brush: Rc::new(Brush::Solid([255, 255, 255])),
            font: Rc::new(Font::default()),
            palette: Rc::new(Vec::new()),
            text_color: [0, 0, 0],
            text_alpha: 1.0,
            bk_color: [255, 255, 255],
            bk_opaque: true,
            rop2: 13,
            winding: false,
            stretch_mode: 1,
            text_align: 0,
            text_extra: 0.0,
            break_extra: 0.0,
            break_count: 0.0,
            cur: (0.0, 0.0),
            arc_ccw: true,
            miter_limit: 10.0,
            brush_org: (0.0, 0.0),
            clip: None,
            rtl: false,
        }
    }
}

//endregion

/// The 20 colours of the default palette, which `PALETTEINDEX` references
/// reach when the context has no palette of its own.
pub const SYSTEM_PALETTE: [Rgb; 20] = [
    [0, 0, 0],
    [128, 0, 0],
    [0, 128, 0],
    [128, 128, 0],
    [0, 0, 128],
    [128, 0, 128],
    [0, 128, 128],
    [192, 192, 192],
    [192, 220, 192],
    [166, 202, 240],
    [255, 251, 240],
    [160, 160, 164],
    [128, 128, 128],
    [255, 0, 0],
    [0, 255, 0],
    [255, 255, 0],
    [0, 0, 255],
    [255, 0, 255],
    [0, 255, 255],
    [255, 255, 255],
];
