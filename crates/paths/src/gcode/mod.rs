//! G-code: the instructions 3D printers, CNC mills and laser cutters run.
//!
//! - `thumbs`: the preview pictures slicers embed in text G-code comments
//!   (PrusaSlicer, OrcaSlicer, Bambu Studio, Cura's thumbnail script,
//!   Creality, Snapmaker Luban, MKS and Weedo printers).
//! - `bgcode`: Prusa's binary G-code, its blocks, compression (deflate
//!   and heatshrink) and MeatPack encoding, and its thumbnail blocks.
//! - `parse`: a streaming interpreter that turns G-code into a
//!   [`Toolpath`]: every position the tool reached and how it got there.
//! - `mesh`: the toolpath as extrusions with real width and height,
//!   coloured by feature type as slicers show them.

mod bgcode;
mod flat;
mod heatshrink;
mod meatpack;
mod mesh;
mod parse;
mod qoi;
mod sample;
mod thumbs;

#[cfg(test)]
mod tests;

pub use bgcode::{BgcodeSink, MAGIC as BGCODE_MAGIC, read_bgcode};
pub use flat::{cut_drawing, is_flat};
pub use mesh::{Target, ToolpathLook, toolpath_scene};
pub use parse::{Keep, Parser, is_layer_marker, parse_meta};
pub use sample::{SAMPLE_FILE_BYTES, has_layer_markers, read_sampled};
pub use thumbs::{Embedded, Scanner, best, decode_picture, embedded_thumbnails};

use glam::Vec4;

/*┌─────────────────╮
| Features        |
└─────────────────╯*/
//region Features

/// What an extrusion is for, from the slicer's comments (`;TYPE:` in
/// PrusaSlicer, OrcaSlicer and Cura, `; FEATURE:` in Bambu Studio,
/// `; feature` in Simplify3D).  `Cut` is a CNC mill's or laser's cutting
/// move, in files without extrusion.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
#[repr(u8)]
pub enum Feature {
    OuterWall,
    InnerWall,
    Overhang,
    SparseInfill,
    SolidInfill,
    TopSurface,
    BottomSurface,
    Bridge,
    GapFill,
    Ironing,
    Skirt,
    Support,
    SupportInterface,
    WipeTower,
    Custom,
    #[default]
    Other,
    Cut,
}

impl Feature {
    pub const COUNT: usize = 17;
    pub const ALL: [Feature; Feature::COUNT] = [
        Feature::OuterWall,
        Feature::InnerWall,
        Feature::Overhang,
        Feature::SparseInfill,
        Feature::SolidInfill,
        Feature::TopSurface,
        Feature::BottomSurface,
        Feature::Bridge,
        Feature::GapFill,
        Feature::Ironing,
        Feature::Skirt,
        Feature::Support,
        Feature::SupportInterface,
        Feature::WipeTower,
        Feature::Custom,
        Feature::Other,
        Feature::Cut,
    ];

    pub fn index(self) -> usize {
        self as usize
    }

    pub fn from_index(i: u8) -> Feature {
        Feature::ALL.get(i as usize).copied().unwrap_or(Feature::Other)
    }

    pub fn name(self) -> &'static str {
        match self {
            Feature::OuterWall => "Outer wall",
            Feature::InnerWall => "Inner wall",
            Feature::Overhang => "Overhang wall",
            Feature::SparseInfill => "Sparse infill",
            Feature::SolidInfill => "Solid infill",
            Feature::TopSurface => "Top surface",
            Feature::BottomSurface => "Bottom surface",
            Feature::Bridge => "Bridge",
            Feature::GapFill => "Gap fill",
            Feature::Ironing => "Ironing",
            Feature::Skirt => "Skirt and brim",
            Feature::Support => "Support",
            Feature::SupportInterface => "Support interface",
            Feature::WipeTower => "Wipe tower",
            Feature::Custom => "Custom",
            Feature::Other => "Extrusion",
            Feature::Cut => "Cut",
        }
    }

    /// The colour slicers show the feature in, as sRGB bytes: PrusaSlicer's
    /// palette, which OrcaSlicer and Bambu Studio share.
    pub fn srgb(self) -> [u8; 3] {
        match self {
            Feature::OuterWall => [0xff, 0x7d, 0x38],
            Feature::InnerWall => [0xff, 0xe6, 0x4d],
            Feature::Overhang => [0x1f, 0x1f, 0xff],
            Feature::SparseInfill => [0xb0, 0x30, 0x29],
            Feature::SolidInfill => [0x96, 0x54, 0xcc],
            Feature::TopSurface => [0xf0, 0x40, 0x40],
            Feature::BottomSurface => [0x66, 0x6a, 0xa8],
            Feature::Bridge => [0x4d, 0x80, 0xba],
            Feature::GapFill => [0xff, 0xff, 0xff],
            Feature::Ironing => [0xff, 0x8c, 0x69],
            Feature::Skirt => [0x00, 0x87, 0x6e],
            Feature::Support => [0x00, 0xff, 0x00],
            Feature::SupportInterface => [0x00, 0x80, 0x00],
            Feature::WipeTower => [0xb3, 0xe3, 0xab],
            Feature::Custom => [0x5e, 0xd1, 0x94],
            Feature::Other => [0xe0, 0xa0, 0x40],
            Feature::Cut => [0x3d, 0x8b, 0xd6],
        }
    }

    /// The colour as linear RGBA, for vertex colours.
    pub fn linear(self) -> Vec4 {
        let [r, g, b] = self.srgb();
        Vec4::new(srgb_to_linear(r), srgb_to_linear(g), srgb_to_linear(b), 1.0)
    }

    /// Recognises a slicer's name for a feature, case and spacing aside:
    /// "External perimeter", "Outer wall", "WALL-OUTER", "outer perimeter"
    /// and so on.  Order matters: "Overhang perimeter" isn't a perimeter,
    /// "Top solid infill" isn't solid infill.
    pub fn from_name(name: &str) -> Feature {
        let n = name.trim().to_ascii_lowercase().replace(['-', '_'], " ");
        let has = |s: &str| n.contains(s);
        if has("overhang") {
            Feature::Overhang
        } else if has("external perimeter") || has("outer wall") || has("wall outer") || has("outer perimeter") {
            Feature::OuterWall
        } else if has("support") && (has("interface") || has("transition") || has("dense")) {
            Feature::SupportInterface
        } else if has("support") {
            Feature::Support
        } else if has("skirt") || has("brim") {
            Feature::Skirt
        } else if has("wipe tower") || has("prime tower") || has("prime pillar") || has("ooze shield") {
            Feature::WipeTower
        } else if has("ironing") {
            Feature::Ironing
        } else if has("top") {
            Feature::TopSurface
        } else if has("bottom") {
            Feature::BottomSurface
        } else if has("bridge") {
            Feature::Bridge
        } else if has("gap") {
            Feature::GapFill
        } else if has("perimeter") || has("wall") || has("shell") {
            Feature::InnerWall
        } else if has("solid") || has("skin") {
            Feature::SolidInfill
        } else if has("infill") || has("fill") {
            Feature::SparseInfill
        } else if has("custom") || has("purge") || has("start") || has("end") || has("prime") {
            Feature::Custom
        } else {
            Feature::Other
        }
    }
}

pub fn srgb_to_linear(c: u8) -> f32 {
    let c = c as f32 / 255.0;
    if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
}

//endregion

/*┌─────────────────╮
| Toolpath        |
└─────────────────╯*/
//region Toolpath

/// How the tool reached a point.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
#[repr(u8)]
pub enum MoveKind {
    /// No move arrives here: the path starts again (after a skipped move,
    /// a `G92` that redefined the position, or at the very start).
    #[default]
    Jump,
    /// A rapid move (`G0`) or a printer's move without extrusion.
    Travel,
    /// A printer's move that extruded.
    Extrude,
    /// A feed move (`G1`, `G2`, `G3`) without extrusion: a cut in a file for
    /// a mill or laser, a travel move in a printer's.
    Feed,
}

/// One move, 8 bytes, beside the point it ends at.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Move {
    pub kind: MoveKind,
    pub feature: Feature,
    /// The tool (extruder) in use.
    pub tool: u8,
    /// Width in micrometres from the slicer's comments (`;WIDTH:`), or 0
    /// when only the extruded amount tells it.
    pub width: u16,
    /// Millimetres of filament per millimetre moved, in units of 1e-5:
    /// the cross-section the extrusion has once the filament's own area is
    /// known (it is often given only at the end of the file).
    pub e_per_mm: u16,
}

/// The moves between two layer changes.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Layer {
    /// The nozzle height of the layer's extrusions, in millimetres.
    pub z: f32,
    /// The layer's thickness in millimetres.
    pub height: f32,
    /// The index of its first point in `Toolpath::points`.
    pub start: u32,
    /// Whether anything was extruded (or cut) in it.
    pub extruded: bool,
    /// Its height from the slicer's comments, 0 when unknown.
    pub height_hint: f32,
}

/// What the slicer says about the print in its comments or metadata.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Meta {
    pub generator: Option<String>,
    /// Estimated print time in seconds.
    pub print_time: Option<f64>,
    pub filament_mm: Option<f64>,
    pub filament_g: Option<f64>,
    pub filament_cm3: Option<f64>,
    pub material: Option<String>,
    pub printer: Option<String>,
    pub layer_height: Option<f64>,
    pub nozzle: Option<f64>,
    pub filament_diameter: Option<f64>,
    pub flavor: Option<String>,
}

/// A G-code file's moves: the positions the tool reached, in
/// millimetres in the machine's coordinates (X right, Y away from the
/// front, Z up), and how it reached each.  Arcs are already divided into
/// short moves.
#[derive(Clone, Debug, Default)]
pub struct Toolpath {
    pub points: Vec<[f32; 3]>,
    /// One per point.
    pub moves: Vec<Move>,
    pub layers: Vec<Layer>,
    pub meta: Meta,
    /// No move extruded, so feed moves are cuts (a mill's or laser's
    /// file).
    pub cnc: bool,
    /// Extruded (or cut) length per feature, in millimetres.
    pub feature_length: [f64; Feature::COUNT],
    /// Bounds of the extrusions (or cuts), in millimetres; min > max when
    /// there are none.
    pub min: [f32; 3],
    pub max: [f32; 3],
    /// Total length of travel moves, in millimetres.
    pub travel_length: f64,
    /// Number of tool changes.
    pub tool_changes: u32,
    /// The lines read and the moves stored, for the inspector.
    pub lines: u64,
    pub warnings: Vec<String>,
    /// The format, such as "G-code" or "Binary G-code (heatshrink, MeatPack)".
    pub format: String,
    /// Only every this many layers' moves were kept (a thumbnail of a very
    /// large file); 1 normally.
    pub thinned: u32,
    /// Parts of the file were skipped (see `sample`): the layers are a
    /// sample, spread over the print, and the totals are of the sample.
    pub sampled: bool,
}

impl Toolpath {
    /// Features present, in display order.
    pub fn features(&self) -> Vec<Feature> {
        Feature::ALL.iter().copied().filter(|f| self.feature_length[f.index()] > 0.0).collect()
    }

    pub fn has_feature(&self, f: Feature) -> bool {
        self.feature_length[f.index()] > 0.0
    }

    /// The width of a move in millimetres, from the slicer's comment or
    /// from how much it extruded, which for a bead of height `h` with
    /// rounded sides (Slic3r's model) is area / h + h (1 - π/4).
    pub fn width_of(&self, m: &Move, h: f32) -> f32 {
        if m.width > 0 {
            return m.width as f32 / 1000.0;
        }
        let nozzle = self.meta.nozzle.unwrap_or(0.4) as f32;
        if m.e_per_mm == 0 {
            return nozzle;
        }
        let d = self.filament_diameter() as f32;
        let area = m.e_per_mm as f32 * 1e-5 * std::f32::consts::PI * d * d / 4.0;
        let h = h.max(0.01);
        let w = area / h + h * (1.0 - std::f32::consts::FRAC_PI_4);
        // A wrong filament diameter (2.85 mm filament read as 1.75) or a
        // wipe would give absurd widths; slicers stay near the nozzle.
        if w.is_finite() && w > nozzle * 0.25 && w < nozzle * 4.0 { w } else { nozzle }
    }

    /// The filament diameter: the file's, else 2.85 mm for Ultimaker's
    /// Griffin flavour, else 1.75 mm.
    pub fn filament_diameter(&self) -> f64 {
        if let Some(d) = self.meta.filament_diameter.filter(|d| *d > 0.5 && *d < 5.0) {
            return d;
        }
        if self.meta.flavor.as_deref().is_some_and(|f| f.eq_ignore_ascii_case("griffin")) { 2.85 } else { 1.75 }
    }

    /// The range of points in layer `i`.
    pub fn layer_range(&self, i: usize) -> std::ops::Range<usize> {
        let start = self.layers.get(i).map_or(self.points.len(), |l| l.start as usize);
        let end = self.layers.get(i + 1).map_or(self.points.len(), |l| l.start as usize);
        start.min(self.points.len())..end.min(self.points.len()).max(start.min(self.points.len()))
    }

    /// Whether the bounds hold anything.
    pub fn is_empty(&self) -> bool {
        self.min[0] > self.max[0]
    }

    /// The facts the inspector lists.
    pub fn details(&self) -> Vec<(String, String)> {
        let mut d = Vec::new();
        let m = &self.meta;
        if let Some(t) = m.print_time {
            d.push(("Print time".into(), crate::text::format_duration(t)));
        }
        let mut filament = Vec::new();
        if let Some(mm) = m.filament_mm {
            filament.push(format!("{:.2} m", mm / 1000.0));
        }
        if let Some(g) = m.filament_g {
            filament.push(format!("{g:.1} g"));
        } else if let Some(c) = m.filament_cm3 {
            filament.push(format!("{c:.2} cm³"));
        }
        if !filament.is_empty() {
            d.push(("Filament".into(), filament.join(", ")));
        }
        if let Some(s) = &m.material {
            d.push(("Material".into(), s.clone()));
        }
        if let Some(s) = &m.printer {
            d.push(("Printer".into(), s.clone()));
        }
        if !self.cnc && !self.sampled {
            d.push(("Layers".into(), self.layers.len().to_string()));
            if let Some(h) = m.layer_height {
                d.push(("Layer height".into(), format!("{h:.2} mm")));
            }
            if let Some(n) = m.nozzle {
                d.push(("Nozzle".into(), format!("{n:.2} mm")));
            }
        }
        if !self.is_empty() {
            let s: Vec<f32> = (0..3).map(|i| self.max[i] - self.min[i]).collect();
            d.push(("Size".into(), format!("{:.1} × {:.1} × {:.1} mm", s[0], s[1], s[2])));
        }
        // The totals of a sample would mislead.
        if !self.sampled {
            let drawn: f64 = self.feature_length.iter().sum();
            let what = if self.cnc { "Cut length" } else { "Extruded path" };
            d.push((what.into(), format!("{:.1} m", drawn / 1000.0)));
            d.push(("Travel".into(), format!("{:.1} m", self.travel_length / 1000.0)));
            if self.tool_changes > 0 {
                d.push(("Tool changes".into(), self.tool_changes.to_string()));
            }
        }
        if let Some(f) = &m.flavor {
            d.push(("Firmware flavour".into(), f.clone()));
        }
        if !self.sampled {
            d.push(("Moves".into(), self.points.len().to_string()));
        }
        d
    }
}

//endregion
