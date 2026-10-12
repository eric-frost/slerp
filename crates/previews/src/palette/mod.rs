//! Colour palettes: swatch files from Adobe's apps, GIMP, Inkscape, Krita,
//! Paint Shop Pro, Windows, Lospec and Procreate, read into one model of
//! named swatches in groups.  Thumbnails draw the swatches as a grid; the
//! viewer lists them with their names and values.
//!
//! Each swatch keeps the colour as the file stores it (RGB, HSB, CMYK, Lab
//! or grey), so the viewer can show the original numbers, and converts it
//! to sRGB for display.  CMYK and Lab convert with the plain textbook
//! formulas rather than through colour profiles, so printed colours are
//! approximate.
//!
//! - **Adobe Swatch Exchange** (`.ase`): big-endian blocks of colours and
//!   groups; RGB, CMYK, Lab and grey; global, spot and normal colours.
//! - **Photoshop swatches** (`.aco`): version 1 colours, usually followed
//!   by the same colours again with names as version 2.
//! - **Adobe colour tables** (`.act`): 256 RGB triples, optionally with a
//!   count and a transparent index.
//! - **GIMP and Inkscape palettes** (`.gpl`), **JASC palettes** (`.pal`,
//!   text) and **Lospec hex lists** (`.hex`).
//! - **RIFF palettes** (`.pal`, binary, from Windows).
//! - **Krita palettes** (`.kpl`, a zip holding `colorset.xml`) and
//!   **Procreate swatches** (`.swatches`, a zip holding `Swatches.json`).
//!
//! `.pal` and `.hex` name unrelated files too (raw game palettes, Intel
//! HEX firmware), so those are recognised by content alone.

mod binary;
mod text;
mod zipped;

#[cfg(test)]
mod tests;

use crate::source::Source;
use slerp_scene::textures::{ColorSpace, Image8, Texture};
use slerp_scene::{Limits, LoadError};
use std::path::Path;

/// The most swatches read from one file.  The largest real palettes (Pantone
/// libraries) hold a few thousand.
pub const MAX_SWATCHES: usize = 65536;

/// Palette files are small; anything bigger isn't one.
const MAX_FILE: u64 = 32 << 20;

/// The palette formats read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PaletteFormat {
    /// Adobe Swatch Exchange (`.ase`).
    Ase,
    /// Photoshop swatches (`.aco`).
    Aco,
    /// Adobe colour table (`.act`).
    Act,
    /// GIMP or Inkscape palette (`.gpl`).
    Gpl,
    /// Krita palette (`.kpl`).
    Kpl,
    /// Paint Shop Pro's text palette (`.pal`).
    JascPal,
    /// Microsoft's RIFF palette (`.pal`).
    RiffPal,
    /// A list of hex colours, as Lospec offers them (`.hex`).
    Hex,
    /// Procreate swatches (`.swatches`).
    ProcreateSwatches,
}

impl PaletteFormat {
    pub fn name(self) -> &'static str {
        match self {
            PaletteFormat::Ase => "Adobe Swatch Exchange",
            PaletteFormat::Aco => "Photoshop swatches",
            PaletteFormat::Act => "Adobe colour table",
            PaletteFormat::Gpl => "GIMP palette",
            PaletteFormat::Kpl => "Krita palette",
            PaletteFormat::JascPal => "JASC palette",
            PaletteFormat::RiffPal => "RIFF palette",
            PaletteFormat::Hex => "Hex colour list",
            PaletteFormat::ProcreateSwatches => "Procreate swatches",
        }
    }
}

/*┌─────────────────╮
| Model           |
└─────────────────╯*/
//region Model

/// A colour as the file stores it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Color {
    /// Red, green and blue from 0 to 1, sRGB.
    Rgb([f32; 3]),
    /// Hue in degrees, saturation and brightness from 0 to 1.
    Hsb([f32; 3]),
    /// Cyan, magenta, yellow and black ink from 0 to 1.
    Cmyk([f32; 4]),
    /// CIE L* from 0 to 100, a* and b* around 0 (D50 white, as Adobe's
    /// apps use).
    Lab([f32; 3]),
    /// Grey from 0 (black) to 1 (white).
    Gray(f32),
}

/// How a design app treats a swatch: an ordinary one, a global colour
/// whose changes update every use, or a spot colour printed as its own
/// ink.  Only Adobe Swatch Exchange and Krita files say.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum SwatchKind {
    #[default]
    Normal,
    Global,
    Spot,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Swatch {
    /// The swatch's name; empty when the file gives none.
    pub name: String,
    pub color: Color,
    /// Opacity from 0 to 1.  Only Procreate, some GIMP palettes (written by
    /// Aseprite) and an Adobe colour table's transparent entry store one.
    pub alpha: f32,
    pub kind: SwatchKind,
}

impl Swatch {
    pub fn new(name: impl Into<String>, color: Color) -> Swatch {
        Swatch { name: name.into(), color, alpha: 1.0, kind: SwatchKind::Normal }
    }

    /// The colour in 8-bit sRGB with alpha, for display.
    pub fn rgba8(&self) -> [u8; 4] {
        let [r, g, b] = to_srgb(self.color);
        let byte = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
        [byte(r), byte(g), byte(b), byte(self.alpha)]
    }

    /// `#RRGGBB`, as people paste colours.
    pub fn hex(&self) -> String {
        let [r, g, b, _] = self.rgba8();
        format!("#{r:02X}{g:02X}{b:02X}")
    }

    /// The colour in its own model's numbers, for colours not stored as RGB
    /// (whose hex says it all): "C 0% M 45% Y 100% K 0%".
    pub fn values(&self) -> Option<String> {
        let pct = |v: f32| format!("{}%", (v * 100.0).round() as i32);
        Some(match self.color {
            Color::Rgb(_) => return None,
            Color::Hsb([h, s, b]) => format!("H {}° S {} B {}", h.round() as i32, pct(s), pct(b)),
            Color::Cmyk([c, m, y, k]) => format!("C {} M {} Y {} K {}", pct(c), pct(m), pct(y), pct(k)),
            Color::Lab([l, a, b]) => format!("L {l:.1} a {a:.1} b {b:.1}"),
            // Ink coverage, as Adobe's apps show grey swatches.
            Color::Gray(g) => format!("K {}", pct(1.0 - g)),
        })
    }

    /// The colour model's name: "RGB", "CMYK", ...
    pub fn model(&self) -> &'static str {
        match self.color {
            Color::Rgb(_) => "RGB",
            Color::Hsb(_) => "HSB",
            Color::Cmyk(_) => "CMYK",
            Color::Lab(_) => "Lab",
            Color::Gray(_) => "Grey",
        }
    }
}

/// Swatches under one heading.  Formats without groups have one, unnamed.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Group {
    pub name: String,
    pub swatches: Vec<Swatch>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Palette {
    pub format: PaletteFormat,
    /// The palette's own name, when the file has one.
    pub name: String,
    /// How many columns the palette's app lays it out in, when it says.
    pub columns: Option<u32>,
    /// Never empty; empty groups are dropped, except a lone one.
    pub groups: Vec<Group>,
    /// Problems that didn't stop reading, such as skipped entries.
    pub warnings: Vec<String>,
}

impl Palette {
    fn new(format: PaletteFormat) -> Palette {
        Palette { format, name: String::new(), columns: None, groups: Vec::new(), warnings: Vec::new() }
    }

    /// How many swatches there are in all.
    pub fn len(&self) -> usize {
        self.groups.iter().map(|g| g.swatches.len()).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn swatches(&self) -> impl Iterator<Item = &Swatch> {
        self.groups.iter().flat_map(|g| &g.swatches)
    }

    /// Facts for `slerp info` and the viewer's inspector.
    pub fn facts(&self) -> Vec<(String, String)> {
        let mut out = vec![("Format".to_string(), self.format.name().to_string())];
        if !self.name.is_empty() {
            out.push(("Name".into(), self.name.clone()));
        }
        out.push(("Colours".into(), self.len().to_string()));
        let named_groups = self.groups.iter().filter(|g| !g.name.is_empty()).count();
        if named_groups > 0 {
            out.push(("Groups".into(), named_groups.to_string()));
        }
        let mut models: Vec<&str> = Vec::new();
        for s in self.swatches() {
            if !models.contains(&s.model()) {
                models.push(s.model());
            }
        }
        out.push(("Colour models".into(), models.join(", ")));
        if self.swatches().any(|s| matches!(s.color, Color::Cmyk(_) | Color::Lab(_))) {
            out.push(("Shown as".into(), "sRGB, CMYK and Lab converted without colour profiles (approximate)".into()));
        }
        for (kind, label) in [(SwatchKind::Spot, "Spot colours"), (SwatchKind::Global, "Global colours")] {
            let n = self.swatches().filter(|s| s.kind == kind).count();
            if n > 0 {
                out.push((label.into(), n.to_string()));
            }
        }
        if let Some(c) = self.columns {
            out.push(("Columns".into(), c.to_string()));
        }
        out
    }

    /// A plain-text listing for `slerp info`: the facts, then each colour's
    /// hex value, name and original numbers, group by group, up to `limit`
    /// colours.
    pub fn describe(&self, limit: usize) -> String {
        use std::fmt::Write;
        let mut out = String::new();
        for (k, v) in self.facts() {
            let _ = writeln!(out, "{k}: {v}");
        }
        for w in &self.warnings {
            let _ = writeln!(out, "warning: {w}");
        }
        let mut shown = 0;
        'groups: for g in &self.groups {
            if !g.name.is_empty() {
                let _ = writeln!(out, "group {}:", g.name);
            }
            for s in &g.swatches {
                if shown == limit {
                    let _ = writeln!(out, "  ... {} more", self.len() - shown);
                    break 'groups;
                }
                shown += 1;
                let name = if s.name.is_empty() { String::new() } else { format!("  {}", s.name) };
                let values = s.values().map(|v| format!("  ({v})")).unwrap_or_default();
                let alpha = if s.alpha < 1.0 { format!("  alpha {:.0}%", s.alpha * 100.0) } else { String::new() };
                let kind = match s.kind {
                    SwatchKind::Spot => "  spot",
                    SwatchKind::Global => "  global",
                    SwatchKind::Normal => "",
                };
                let _ = writeln!(out, "  {}{name}{values}{alpha}{kind}", s.hex());
            }
        }
        out
    }

    /// Adds a swatch to the last group, starting an unnamed one if there is
    /// none.  Returns false once the cap is reached.
    fn push(&mut self, swatch: Swatch) -> bool {
        if self.len() >= MAX_SWATCHES {
            if !self.warnings.iter().any(|w| w.starts_with("only the first")) {
                self.warnings.push(format!("only the first {MAX_SWATCHES} colours are shown"));
            }
            return false;
        }
        if self.groups.is_empty() {
            self.groups.push(Group::default());
        }
        self.groups.last_mut().expect("added above").swatches.push(swatch);
        true
    }

    /// Starts a named group.
    fn begin_group(&mut self, name: String) {
        self.groups.push(Group { name, swatches: Vec::new() });
    }

    /// Drops empty groups and refuses a palette without colours.
    fn finish(mut self) -> Result<Palette, LoadError> {
        self.groups.retain(|g| !g.swatches.is_empty());
        if self.groups.is_empty() {
            return Err(LoadError::Unsupported(format!("{} without colours", self.format.name())));
        }
        Ok(self)
    }
}

//endregion

/*┌─────────────────╮
| Colour models   |
└─────────────────╯*/
//region Colour models

/// Converts any stored colour to sRGB (0 to 1, possibly out of range).
pub fn to_srgb(c: Color) -> [f32; 3] {
    match c {
        Color::Rgb(rgb) => rgb,
        Color::Gray(g) => [g; 3],
        Color::Cmyk([c, m, y, k]) => [(1.0 - c) * (1.0 - k), (1.0 - m) * (1.0 - k), (1.0 - y) * (1.0 - k)],
        Color::Hsb([h, s, v]) => hsb_to_rgb(h, s, v),
        Color::Lab(lab) => lab_to_srgb(lab),
    }
}

fn hsb_to_rgb(h: f32, s: f32, v: f32) -> [f32; 3] {
    let h = h.rem_euclid(360.0) / 60.0;
    let c = v * s;
    let x = c * (1.0 - (h % 2.0 - 1.0).abs());
    let (r, g, b) = match h as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = v - c;
    [r + m, g + m, b + m]
}

/// CIE Lab (D50) to sRGB: Lab to XYZ, then Bradford-adapted to D65 and
/// sRGB's primaries in one matrix (Lindbloom's), then sRGB's curve.
fn lab_to_srgb([l, a, b]: [f32; 3]) -> [f32; 3] {
    let fy = (l + 16.0) / 116.0;
    let (fx, fz) = (fy + a / 500.0, fy - b / 200.0);
    let inv = |t: f32| if t > 6.0 / 29.0 { t * t * t } else { 3.0 * (6.0f32 / 29.0).powi(2) * (t - 4.0 / 29.0) };
    let (x, y, z) = (0.96422 * inv(fx), inv(fy), 0.82521 * inv(fz));
    let lin = [
        3.133_856 * x - 1.616_867 * y - 0.490_614_6 * z,
        -0.978_768_4 * x + 1.916_142 * y + 0.033_454 * z,
        0.071_945_3 * x - 0.228_991_4 * y + 1.405_243 * z,
    ];
    lin.map(|v| {
        let v = v.clamp(0.0, 1.0);
        if v <= 0.0031308 { 12.92 * v } else { 1.055 * v.powf(1.0 / 2.4) - 0.055 }
    })
}

//endregion

/*┌─────────────────╮
| Reading         |
└─────────────────╯*/
//region Reading

/// Recognises a palette from the file's name and first bytes.  `head`
/// holds the whole file when it is shorter than what was read, which is how
/// an Adobe colour table (no magic, a fixed size) is told apart.
pub fn sniff(path: &Path, head: &[u8]) -> Option<PaletteFormat> {
    let ext = path.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase).unwrap_or_default();
    if head.starts_with(b"ASEF") {
        return Some(PaletteFormat::Ase);
    }
    if head.starts_with(b"RIFF") && head.get(8..12) == Some(b"PAL ") {
        return Some(PaletteFormat::RiffPal);
    }
    if text::is_jasc(head) {
        return Some(PaletteFormat::JascPal);
    }
    if text::is_gpl(head) {
        return Some(PaletteFormat::Gpl);
    }
    match ext.as_str() {
        "aco" if binary::looks_like_aco(head) => Some(PaletteFormat::Aco),
        "act" if binary::looks_like_act(head) => Some(PaletteFormat::Act),
        "hex" if text::is_hex_list(head) => Some(PaletteFormat::Hex),
        "kpl" if crate::zip::is_zip(head) => Some(PaletteFormat::Kpl),
        "swatches" if crate::zip::is_zip(head) => Some(PaletteFormat::ProcreateSwatches),
        _ => None,
    }
}

/// Reads the palette in the file at `path`.
pub fn read_path(format: PaletteFormat, path: &Path, limits: &Limits) -> Result<Palette, LoadError> {
    let mut src = Source::open(path)?;
    read_source(format, &mut src, limits)
}

/// Reads a palette from a file already opened (or bytes, for tests).
pub fn read_source(format: PaletteFormat, src: &mut Source, limits: &Limits) -> Result<Palette, LoadError> {
    if src.len() > limits.max_file_bytes.min(MAX_FILE) {
        return Err(LoadError::TooLarge(format!("a {}-byte palette", src.len())));
    }
    let palette = match format {
        PaletteFormat::Kpl => zipped::kpl(src)?,
        PaletteFormat::ProcreateSwatches => zipped::procreate(src)?,
        _ => {
            let bytes = src.head(src.len())?;
            match format {
                PaletteFormat::Ase => binary::ase(&bytes)?,
                PaletteFormat::Aco => binary::aco(&bytes)?,
                PaletteFormat::Act => binary::act(&bytes)?,
                PaletteFormat::RiffPal => binary::riff(&bytes)?,
                PaletteFormat::Gpl => text::gpl(&bytes)?,
                PaletteFormat::JascPal => text::jasc(&bytes)?,
                PaletteFormat::Hex => text::hex(&bytes)?,
                PaletteFormat::Kpl | PaletteFormat::ProcreateSwatches => unreachable!(),
            }
        }
    };
    palette.finish()
}

//endregion

/*┌─────────────────╮
| Thumbnails      |
└─────────────────╯*/
//region Thumbnails

/// Where each swatch goes in a grid of square cells: groups start on new
/// rows with a wider gap between them.
pub struct Layout {
    pub columns: u32,
    /// The cell's side and the gap between cells, in pixels.
    pub cell: f32,
    pub gap: f32,
    /// The extra gap between groups.
    pub group_gap: f32,
    /// Each swatch's cell: (group, index in group, x, y) of its top-left
    /// corner, relative to the grid's.
    pub cells: Vec<(usize, usize, f32, f32)>,
    pub width: f32,
    pub height: f32,
}

/// Lays the palette out to fill a `width` x `height` area with cells as
/// large as possible.
pub fn layout(p: &Palette, width: f32, height: f32) -> Layout {
    // Every swatch in order, and the lengths of the runs that start new
    // rows: the groups, or one run when there are so many groups that
    // separating them would only add gaps.
    let index: Vec<(usize, usize)> =
        p.groups.iter().enumerate().flat_map(|(gi, g)| (0..g.swatches.len()).map(move |i| (gi, i))).collect();
    let runs: Vec<usize> = if p.groups.len() <= 256 {
        p.groups.iter().map(|g| g.swatches.len()).filter(|&c| c > 0).collect()
    } else {
        vec![index.len()]
    };
    let n = index.len().max(1);
    let gaps = runs.len().saturating_sub(1) as f32;
    // 1. Try every column count and keep the one giving the largest cells.
    //    A cell's pitch is its side plus a gap of an eighth of it; runs are
    //    a further half pitch apart.
    let mut best = (1u32, 0.0f32);
    for cols in 1..=(n as u32).min(4096) {
        let rows: usize = runs.iter().map(|&c| c.div_ceil(cols as usize)).sum();
        let pitch = (width / (cols as f32 - 0.125)).min(height / (rows.max(1) as f32 - 0.125 + gaps * 0.5));
        if pitch > best.1 {
            best = (cols, pitch);
        }
    }
    let (columns, pitch) = best;
    // 2. Tiny cells lose their gaps, so they stay visible at all.
    let gap = if pitch >= 6.0 { (pitch / 8.0).max(1.0).floor() } else { 0.0 };
    let cell = (pitch - gap).max(1.0).floor();
    let pitch = cell + gap;
    let group_gap = if gap > 0.0 { (pitch / 2.0).floor() } else { 0.0 };
    // 3. The cells, row by row.
    let mut cells = Vec::with_capacity(index.len());
    let mut y = 0.0f32;
    let mut next = index.iter();
    for (ri, &count) in runs.iter().enumerate() {
        if ri > 0 {
            y += group_gap;
        }
        for (i, &(gi, si)) in next.by_ref().take(count).enumerate() {
            let (c, r) = ((i as u32 % columns) as f32, (i as u32 / columns) as f32);
            cells.push((gi, si, c * pitch, y + r * pitch));
        }
        y += count.div_ceil(columns as usize) as f32 * pitch;
    }
    let used_cols = (n as u32).min(columns) as f32;
    Layout {
        columns,
        cell,
        gap,
        group_gap,
        cells,
        width: (used_cols * pitch - gap).max(0.0),
        height: (y - gap).max(0.0),
    }
}

/// A grid of the palette's swatches, `size` pixels square, without text.
/// Light swatches get a faint outline so they show on white backgrounds,
/// and transparent ones a checkerboard.
pub fn thumbnail(p: &Palette, size: u32) -> Image8 {
    let size = size.clamp(8, 2048);
    let mut out = Image8::new(size, size);
    let margin = (size as f32 / 32.0).floor();
    let room = size as f32 - 2.0 * margin;
    let lay = layout(p, room, room);
    let x0 = (margin + (room - lay.width) / 2.0).floor().max(0.0) as i64;
    let y0 = (margin + (room - lay.height) / 2.0).floor().max(0.0) as i64;
    let cell = lay.cell as i64;
    let outline = cell >= 8;
    for &(gi, i, cx, cy) in &lay.cells {
        let [r, g, b, a] = p.groups[gi].swatches[i].rgba8();
        let (left, top) = (x0 + cx as i64, y0 + cy as i64);
        for y in top.max(0)..(top + cell).min(size as i64) {
            for x in left.max(0)..(left + cell).min(size as i64) {
                let (dx, dy) = (x - left, y - top);
                let edge = outline && (dx == 0 || dy == 0 || dx == cell - 1 || dy == cell - 1);
                let mut px = [r, g, b, 255];
                if a < 255 {
                    // Transparent swatches over a checkerboard.
                    let check = if ((dx * 4 / cell.max(1)) + (dy * 4 / cell.max(1))) % 2 == 0 { 255u32 } else { 204 };
                    let mix = |c: u8| ((c as u32 * a as u32 + check * (255 - a as u32)) / 255) as u8;
                    px = [mix(r), mix(g), mix(b), 255];
                }
                if edge {
                    // A darker rim, which only light colours really show.
                    for c in &mut px[..3] {
                        *c = (*c as u32 * 7 / 8) as u8;
                    }
                }
                let o = ((y as u32 * size + x as u32) * 4) as usize;
                out.pixels[o..o + 4].copy_from_slice(&px);
            }
        }
    }
    out
}

/// The palette as a picture, for code that wants every file as an image
/// (`slerp info`, the Qt plugin).
pub fn picture(p: &Palette, size: u32) -> Texture {
    let img = thumbnail(p, size);
    let mut t = Texture::from_rgba8(img.width, img.height, img.pixels, ColorSpace::Srgb);
    t.source.container = p.format.name().to_string();
    t.source.format = format!("{} colours", p.len());
    t
}

//endregion

/// Reads a big-endian UTF-16 string of `units` code units at `at`, without
/// a trailing NUL.
fn utf16_be(bytes: &[u8], at: usize, units: usize) -> Option<String> {
    let raw = bytes.get(at..at.checked_add(units.checked_mul(2)?)?)?;
    let chars: Vec<u16> = raw.chunks_exact(2).map(|c| u16::from_be_bytes([c[0], c[1]])).collect();
    let end = chars.iter().position(|&c| c == 0).unwrap_or(chars.len());
    Some(String::from_utf16_lossy(&chars[..end]))
}
