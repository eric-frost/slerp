//! OpenEXR pictures for Slerp's loaders: what a file holds ([`open`]: its
//! parts, the layers of channels in each, windows, compression, mip levels)
//! and one layer of one part decoded to linear RGBA floats ([`decode`]).
//!
//! An EXR file is one or more *parts* (separate images, such as the views of
//! a stereo render or the passes a compositor wrote), and each part holds
//! any number of named channels.  Channels whose names share a prefix up to
//! the last dot form a *layer* (`diffuse.R`, `diffuse.G`, `diffuse.B`), and
//! the channels without a prefix are the part's main layer.  A layer becomes
//! a picture by naming its channels: red, green, blue and alpha; luminance
//! and chroma (`Y`, `RY`, `BY`, as OpenEXR's own RGBA interface writes
//! them); or anything else, which is data (depth, normals, motion) shown as
//! it is.
//!
//! The `exr` crate does the decoding, except for the luminance-chroma files
//! whose chroma channels are stored at reduced resolution, which it can't
//! read: their scan lines are inflated here when they are uncompressed, RLE
//! or zip compressed.  Deep files (several samples per pixel) are refused
//! with a message, since a picture of them means flattening them first.
//!
//! Files are hostile until proven otherwise: sizes are checked against
//! [`Options::max_pixels`] before anything is allocated, every block is
//! clipped to the picture it fills, and panics in the decoder become
//! errors.

use exr::block::chunk::CompressedBlock;
use exr::block::lines::LineIndex;
use exr::block::reader::{ChunksReader, Reader};
use exr::block::{BlockIndex, UncompressedBlock};
use exr::compression::Compression;
use exr::math::RoundingMode;
use exr::meta::attribute::{IntegerBounds, LevelMode, SampleType};
use exr::meta::header::Header;
use exr::meta::{BlockDescription, compute_level_count, compute_level_size};
use std::io::{Read, Seek};

/// The most pixels a decoded level may have by default: 64 million, a
/// gigabyte of RGBA floats.
pub const MAX_PIXELS: u64 = 1 << 26;
/// The most blocks a file may list, and channels a part may have, so the
/// offset tables and channel lists of a hostile header stay small.
const MAX_CHUNKS: usize = 1 << 24;
const MAX_CHANNELS: usize = 4096;

/*┌─────────────────╮
| What a file has |
└─────────────────╯*/
//region What a file has

/// A file's structure, read from its headers alone.
#[derive(Clone, Debug)]
pub struct Exr {
    pub parts: Vec<Part>,
}

/// One part: a picture of its own, with its own size and channels.
#[derive(Clone, Debug)]
pub struct Part {
    /// The part's name (multi-part files name every part).
    pub name: Option<String>,
    /// The view of a stereo or multi-view render.
    pub view: Option<String>,
    pub layers: Vec<Layer>,
    /// The pixels stored, and the frame they belong in.
    pub data_window: Window,
    pub display_window: Window,
    pub compression: String,
    pub tiles: Option<Tiles>,
    /// Deep data: several samples per pixel, which is refused.
    pub deep: bool,
    /// What the header says besides the layout, for the inspector.
    pub facts: Vec<(String, String)>,
    /// The small 8-bit RGBA picture a writer may store in the header
    /// (`preview` attribute): width, height, pixels.
    pub preview: Option<(u32, u32, Vec<u8>)>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Window {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

impl Window {
    fn of(b: IntegerBounds) -> Window {
        Window {
            x: b.position.x(),
            y: b.position.y(),
            width: b.size.width().min(u32::MAX as usize) as u32,
            height: b.size.height().min(u32::MAX as usize) as u32,
        }
    }

    fn pixels(self) -> u64 {
        self.width as u64 * self.height as u64
    }

    fn text(self) -> String {
        format!("{} × {} at ({}, {})", self.width, self.height, self.x, self.y)
    }
}

/// How a tiled part is cut up, and whether it stores smaller copies.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Tiles {
    pub width: u32,
    pub height: u32,
    /// Mip levels (each half the last), when the part has them.
    pub mip_levels: u32,
    /// Rip maps halve width and height separately; only the levels that
    /// keep the shape are used.
    pub rip: bool,
}

/// Channels sharing a name prefix.
#[derive(Clone, Debug)]
pub struct Layer {
    /// The prefix without its final dot; empty for the part's main layer.
    pub name: String,
    pub channels: Vec<Channel>,
}

#[derive(Clone, Debug)]
pub struct Channel {
    /// The full name, as stored.
    pub name: String,
    /// The name after the layer's prefix (`R`, `Z`, `u`).
    pub short: String,
    pub sample: Sample,
    /// Stored at one sample per this many pixels across and down (2 for the
    /// chroma channels of luminance-chroma files).
    pub sampling: (u32, u32),
    /// The channel's position in the part's channel list.
    index: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sample {
    Half,
    Float,
    Uint,
}

impl Sample {
    pub fn name(self) -> &'static str {
        match self {
            Sample::Half => "half",
            Sample::Float => "float",
            Sample::Uint => "uint",
        }
    }
}

/// What a layer's channels mean, by their names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Meaning {
    /// Red, green and blue (some may be missing), maybe alpha.
    Colour,
    /// Luminance alone (a grey picture), maybe alpha.
    Luminance,
    /// Luminance with two chroma channels, as OpenEXR's RGBA interface
    /// stores colour pictures to save space.
    LuminanceChroma,
    /// Anything else: depth, normals, motion, object IDs.
    Data,
}

impl Layer {
    /// Indices into `channels` for red, green, blue and alpha, and what they
    /// mean.  Luminance-chroma layers give Y, RY and BY in the first three.
    fn mapping(&self) -> ([Option<usize>; 4], Meaning) {
        let find =
            |names: &[&str]| self.channels.iter().position(|c| names.iter().any(|n| c.short.eq_ignore_ascii_case(n)));
        let a = find(&["A", "alpha"]);
        let (r, g, b) = (find(&["R", "red"]), find(&["G", "green"]), find(&["B", "blue"]));
        if r.is_some() || g.is_some() || b.is_some() {
            return ([r, g, b, a], Meaning::Colour);
        }
        let (y, ry, by) = (find(&["Y"]), find(&["RY"]), find(&["BY"]));
        if let Some(y) = y {
            if ry.is_some() && by.is_some() {
                return ([Some(y), ry, by, a], Meaning::LuminanceChroma);
            }
            return ([Some(y), Some(y), Some(y), a], Meaning::Luminance);
        }
        // Data: one channel shows grey, two or more as red, green, blue.
        let others: Vec<usize> = (0..self.channels.len()).filter(|&i| Some(i) != a).collect();
        match others.as_slice() {
            // Alpha alone (a matte) is shown as grey.
            [] => ([a, a, a, None], Meaning::Data),
            [one] => ([Some(*one), Some(*one), Some(*one), a], Meaning::Data),
            [x, y] => ([Some(*x), Some(*y), None, a], Meaning::Data),
            [x, y, z, ..] => ([Some(*x), Some(*y), Some(*z), a], Meaning::Data),
        }
    }

    pub fn meaning(&self) -> Meaning {
        self.mapping().1
    }

    /// The channels' short names joined ("RGBA", "Z", "u v").
    pub fn channel_text(&self) -> String {
        let all_single = self.channels.iter().all(|c| c.short.chars().count() == 1);
        let names: Vec<&str> = self.channels.iter().map(|c| c.short.as_str()).collect();
        if all_single && self.meaning() == Meaning::Colour {
            // Red, green, blue, alpha in that order, as people say it.
            let mut sorted = names.clone();
            let rank = |n: &str| "RGBA".find(&n.to_ascii_uppercase()).unwrap_or(9);
            sorted.sort_by_key(|n| rank(n));
            return sorted.concat();
        }
        if all_single { names.concat() } else { names.join(" ") }
    }
}

impl Exr {
    /// Every layer of every part as one list, for a picker: (part, layer,
    /// label).  Labels are unique.
    pub fn choices(&self) -> Vec<(usize, usize, String)> {
        let mut out: Vec<(usize, usize, String)> = Vec::new();
        let many = self.parts.len() > 1;
        for (pi, part) in self.parts.iter().enumerate() {
            for (li, layer) in part.layers.iter().enumerate() {
                let channels = layer.channel_text();
                let layer_label = if layer.name.is_empty() { channels } else { format!("{} ({channels})", layer.name) };
                let mut label = if many {
                    let part_name = part.name.clone().unwrap_or_else(|| format!("Part {}", pi + 1));
                    format!("{part_name}: {layer_label}")
                } else {
                    layer_label
                };
                let mut n = 2;
                while out.iter().any(|(_, _, l)| *l == label) {
                    label = format!("{label} {n}");
                    n += 1;
                }
                out.push((pi, li, label));
            }
        }
        out
    }

    /// The layer shown first: the first colour layer (the unprefixed one
    /// before named ones), else the first luminance layer, else the first
    /// layer of all.
    pub fn default_choice(&self) -> Option<(usize, usize)> {
        let all: Vec<(usize, usize)> =
            self.parts.iter().enumerate().flat_map(|(p, part)| (0..part.layers.len()).map(move |l| (p, l))).collect();
        let meaning = |&(p, l): &(usize, usize)| self.parts[p].layers[l].meaning();
        let unnamed = |&(p, l): &(usize, usize)| self.parts[p].layers[l].name.is_empty();
        let deep = |&(p, _): &(usize, usize)| self.parts[p].deep;
        let colour = |c: &(usize, usize)| matches!(meaning(c), Meaning::Colour | Meaning::LuminanceChroma);
        all.iter()
            .find(|c| !deep(c) && colour(c) && unnamed(c))
            .or_else(|| all.iter().find(|c| !deep(c) && colour(c)))
            .or_else(|| all.iter().find(|c| !deep(c) && meaning(c) == Meaning::Luminance))
            .or_else(|| all.iter().find(|c| !deep(c)))
            .or(all.first())
            .copied()
    }
}

/// Reads a file's headers.
pub fn open<R: Read + Seek>(mut source: R) -> Result<Exr, String> {
    guarded(|| {
        let reader = match Reader::read_from_buffered(&mut source, false) {
            Ok(r) => r,
            Err(e) if e.to_string().contains("subsampling") => {
                let bytes = subsampled::read_all(&mut source)?;
                let file = subsampled::parse(&bytes)?;
                return Ok(Exr { parts: vec![part_of(&file.header)?] });
            }
            Err(e) => return Err(e.to_string()),
        };
        let parts = reader.headers().iter().map(part_of).collect::<Result<Vec<_>, String>>()?;
        if parts.is_empty() {
            return Err("the file has no parts".into());
        }
        Ok(Exr { parts })
    })
}

fn part_of(h: &Header) -> Result<Part, String> {
    if h.channels.list.len() > MAX_CHANNELS {
        return Err(format!("{} channels", h.channels.list.len()));
    }
    // 1. Group channels into layers by the prefix before their last dot,
    //    keeping the order in which each layer first appears.
    let mut layers: Vec<Layer> = Vec::new();
    for (index, c) in h.channels.list.iter().enumerate() {
        let name = c.name.to_string();
        let (prefix, short) = match name.rfind('.') {
            Some(i) if i + 1 < name.len() => (name[..i].to_string(), name[i + 1..].to_string()),
            _ => (String::new(), name.clone()),
        };
        let sample = match c.sample_type {
            SampleType::F16 => Sample::Half,
            SampleType::F32 => Sample::Float,
            SampleType::U32 => Sample::Uint,
        };
        let sampling = (c.sampling.x().min(u32::MAX as usize) as u32, c.sampling.y().min(u32::MAX as usize) as u32);
        let channel = Channel { name, short, sample, sampling, index };
        match layers.iter_mut().find(|l| l.name == prefix) {
            Some(l) => l.channels.push(channel),
            None => layers.push(Layer { name: prefix, channels: vec![channel] }),
        }
    }
    // The main layer comes first.
    layers.sort_by_key(|l| !l.name.is_empty());
    // 2. Layout.
    let tiles = match h.blocks {
        BlockDescription::ScanLines => None,
        BlockDescription::Tiles(t) => {
            let full = h.layer_size.width().max(h.layer_size.height());
            let levels = match t.level_mode {
                LevelMode::Singular => 1,
                LevelMode::MipMap | LevelMode::RipMap => compute_level_count(t.rounding_mode, full),
            };
            Some(Tiles {
                width: t.tile_size.width() as u32,
                height: t.tile_size.height() as u32,
                mip_levels: levels as u32,
                rip: t.level_mode == LevelMode::RipMap,
            })
        }
    };
    let data_window = Window::of(h.data_window());
    let display_window = Window::of(h.shared_attributes.display_window);
    // 3. The facts worth listing.
    let mut facts = Vec::new();
    let o = &h.own_attributes;
    let text = |t: &Option<exr::meta::attribute::Text>| t.as_ref().map(|t| t.to_string());
    for (key, value) in [
        ("Software", text(&o.software_name)),
        ("Owner", text(&o.owner)),
        ("Comments", text(&o.comments)),
        ("Captured", text(&o.capture_date)),
    ] {
        if let Some(v) = value.filter(|v| !v.trim().is_empty()) {
            facts.push((key.to_string(), v));
        }
    }
    if let Some(views) = &o.multi_view_names {
        facts.push(("Views".into(), views.iter().map(|v| v.to_string()).collect::<Vec<_>>().join(", ")));
    }
    if let Some(c) = &h.shared_attributes.chromaticities {
        facts.push((
            "Chromaticities".into(),
            format!(
                "red {:.3} {:.3}, green {:.3} {:.3}, blue {:.3} {:.3}, white {:.4} {:.4}",
                c.red.x(),
                c.red.y(),
                c.green.x(),
                c.green.y(),
                c.blue.x(),
                c.blue.y(),
                c.white.x(),
                c.white.y()
            ),
        ));
    }
    if (h.shared_attributes.pixel_aspect - 1.0).abs() > 1e-4 {
        facts.push(("Pixel aspect".into(), format!("{}", h.shared_attributes.pixel_aspect)));
    }
    if let Some(e) = o.exposure {
        facts.push(("Exposure".into(), format!("{e}")));
    }
    if let Some(env) = &o.environment_map {
        facts.push(("Environment map".into(), format!("{env:?}").to_lowercase()));
    }
    let preview = o.preview.as_ref().and_then(|p| {
        let (w, h) = (p.size.width(), p.size.height());
        (w > 0 && h > 0 && w <= 4096 && h <= 4096 && p.pixel_data.len() == w * h * 4)
            .then(|| (w as u32, h as u32, p.pixel_data.iter().map(|&v| v as u8).collect()))
    });
    if let Some((w, h, _)) = &preview {
        facts.push(("Stored preview".into(), format!("{w} × {h}")));
    }
    Ok(Part {
        preview,
        name: text(&o.layer_name),
        view: text(&o.view_name),
        layers,
        data_window,
        display_window,
        compression: compression_name(h.compression),
        tiles,
        deep: h.deep,
        facts,
    })
}

fn compression_name(c: Compression) -> String {
    match c {
        Compression::Uncompressed => "none".into(),
        Compression::RLE => "RLE".into(),
        Compression::ZIP1 => "ZIPS (zip, one line per block)".into(),
        Compression::ZIP16 => "ZIP".into(),
        Compression::PIZ => "PIZ (wavelet)".into(),
        Compression::PXR24 => "PXR24".into(),
        Compression::B44 => "B44".into(),
        Compression::B44A => "B44A".into(),
        Compression::DWAA(_) => "DWAA".into(),
        Compression::DWAB(_) => "DWAB".into(),
        other => format!("{other}"),
    }
}

impl Part {
    /// The inspector's lines for this part.
    pub fn describe(&self) -> Vec<(String, String)> {
        let mut out = Vec::new();
        if let Some(n) = &self.name {
            out.push(("Part".to_string(), n.clone()));
        }
        if let Some(v) = &self.view {
            out.push(("View".into(), v.clone()));
        }
        out.push(("Compression".into(), self.compression.clone()));
        out.push((
            "Storage".into(),
            match self.tiles {
                None => "scan lines".into(),
                Some(t) if t.mip_levels > 1 => format!(
                    "tiles of {} × {}, {} {} levels",
                    t.width,
                    t.height,
                    t.mip_levels,
                    if t.rip { "rip-map" } else { "mip-map" }
                ),
                Some(t) => format!("tiles of {} × {}", t.width, t.height),
            },
        ));
        out.push(("Data window".into(), self.data_window.text()));
        if self.display_window != self.data_window {
            out.push(("Display window".into(), self.display_window.text()));
        }
        for l in &self.layers {
            let channels: Vec<String> = l
                .channels
                .iter()
                .map(|c| {
                    let sub = if c.sampling != (1, 1) {
                        format!(" every {}×{}", c.sampling.0, c.sampling.1)
                    } else {
                        String::new()
                    };
                    format!("{} ({}{sub})", c.short, c.sample.name())
                })
                .collect();
            let name = if l.name.is_empty() { "Channels".to_string() } else { format!("Layer {}", l.name) };
            out.push((name, channels.join(", ")));
        }
        if self.deep {
            out.push(("Deep data".into(), "yes (several samples per pixel)".into()));
        }
        out.extend(self.facts.iter().cloned());
        out
    }
}

//endregion

/*┌─────────────────╮
| Decoding        |
└─────────────────╯*/
//region Decoding

/// How much to decode.
#[derive(Clone, Copy, Debug)]
pub struct Options {
    /// Refuse levels with more pixels than this.
    pub max_pixels: u64,
    /// Shrink the picture until its longer side is at most this, starting
    /// from the smallest stored mip level that is still at least as large.
    pub max_side: Option<u32>,
    /// Keep the file's own smaller mip levels too (when nothing is shrunk
    /// and the data window fills the display window).
    pub mips: bool,
    /// Place the stored pixels in the display window; otherwise the
    /// picture is the data window alone.
    pub place: bool,
    /// Convert colour from the primaries the file's `chromaticities` name
    /// (ACES, Rec. 2020, XYZ) to sRGB's, so wide-gamut renders don't look
    /// washed out.
    pub to_srgb: bool,
}

impl Default for Options {
    fn default() -> Options {
        Options { max_pixels: MAX_PIXELS, max_side: None, mips: false, place: true, to_srgb: true }
    }
}

/// A decoded layer.
#[derive(Clone, Debug)]
pub struct Picture {
    /// The largest level first; each is linear RGBA, rows from the top.
    pub levels: Vec<Level>,
    pub has_alpha: bool,
    pub meaning: Meaning,
    /// Things the decoder worked around.
    pub notes: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct Level {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<f32>,
}

/// Decodes layer `layer` of part `part` (indices into [`Exr::parts`] and
/// [`Part::layers`]) as [`open`] listed them.
pub fn decode<R: Read + Seek>(source: R, part: usize, layer: usize, opts: &Options) -> Result<Picture, String> {
    guarded(|| decode_inner(source, part, layer, opts))
}

/// Decodes the layer a viewer shows first (see [`Exr::default_choice`]),
/// shrunk to `max_side`: the Blender loader's packed EXR textures.
pub fn decode_default(bytes: &[u8], max_side: Option<u32>, max_pixels: u64) -> Result<Picture, String> {
    let exr = open(std::io::Cursor::new(bytes))?;
    let (p, l) = exr.default_choice().ok_or("the file has no channels")?;
    let opts = Options { max_pixels, max_side, ..Options::default() };
    decode(std::io::Cursor::new(bytes), p, l, &opts)
}

fn guarded<T>(f: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(f))
        .unwrap_or_else(|_| Err("the OpenEXR decoder failed on damaged data".into()))
}

fn decode_inner<R: Read + Seek>(mut source: R, part: usize, layer: usize, opts: &Options) -> Result<Picture, String> {
    // 1. Headers, and the part and layer asked for.  Files with chroma
    //    subsampling, which the `exr` crate refuses, go their own way.
    let reader = match Reader::read_from_buffered(&mut source, false) {
        Ok(r) => r,
        Err(e) if e.to_string().contains("subsampling") => {
            let bytes = subsampled::read_all(&mut source)?;
            let file = subsampled::parse(&bytes)?;
            return subsampled::decode(&bytes, &file, layer, opts);
        }
        Err(e) if e.to_string().contains("deep data") => {
            return Err("deep OpenEXR data (several samples per pixel) can't be shown as a picture".into());
        }
        Err(e) => return Err(e.to_string()),
    };
    let headers = reader.headers();
    let chunks: usize = headers.iter().map(|h| h.chunk_count).fold(0usize, |a, b| a.saturating_add(b));
    if chunks > MAX_CHUNKS {
        return Err(format!("the file lists {chunks} blocks"));
    }
    let header = headers.get(part).ok_or("no such part")?.clone();
    let info = part_of(&header)?;
    if info.deep {
        return Err("deep OpenEXR data (several samples per pixel) can't be shown as a picture".into());
    }
    let lay = info.layers.get(layer).ok_or("no such layer")?;
    let (map, meaning) = lay.mapping();
    let wanted: Vec<Option<usize>> = map.iter().map(|m| m.map(|i| lay.channels[i].index)).collect();
    let mut notes = Vec::new();

    // 2. The level to decode: the smallest stored mip level at least
    //    `max_side` on its longer side, else the full picture.
    let data = header.data_window();
    let (full_w, full_h) = (data.size.width(), data.size.height());
    let (round, level_count) = match header.blocks {
        BlockDescription::Tiles(t) if t.level_mode != LevelMode::Singular => {
            (t.rounding_mode, compute_level_count(t.rounding_mode, full_w.max(full_h)))
        }
        _ => (RoundingMode::Down, 1),
    };
    let size_of = |l: usize| (compute_level_size(round, full_w, l), compute_level_size(round, full_h, l));
    let mut first = 0;
    if let Some(max) = opts.max_side {
        while first + 1 < level_count {
            let (w, h) = size_of(first + 1);
            if w.max(h) < max as usize {
                break;
            }
            first += 1;
        }
    }
    let keep_mips = opts.mips && opts.max_side.is_none() && level_count > 1 && first == 0;
    let last = if keep_mips { level_count - 1 } else { first };
    let mut levels = Vec::new();
    for l in first..=last {
        let (w, h) = size_of(l);
        if (w as u64).saturating_mul(h as u64) > opts.max_pixels || w == 0 || h == 0 {
            return Err(format!("a picture of {w} × {h} pixels is too large"));
        }
        let mut rgba = vec![0f32; w * h * 4];
        if map[3].is_none() {
            rgba.chunks_exact_mut(4).for_each(|p| p[3] = 1.0);
        }
        levels.push(Level { width: w as u32, height: h as u32, rgba });
    }

    // 3. Fill the levels from the blocks of this part and those levels.
    let chunk_reader = reader
        .filter_chunks(false, |_, tile, block| {
            block.layer == part
                && tile.level_index.x() == tile.level_index.y()
                && (first..=last).contains(&tile.level_index.x())
        })
        .map_err(|e| e.to_string())?;
    {
        let mut chunk_reader = chunk_reader;
        let block_bytes = header.max_block_byte_size();
        let mut failed = None;
        while let Some(chunk) = chunk_reader.read_next_chunk() {
            let block = chunk.and_then(|chunk| {
                let packed = match &chunk.compressed_block {
                    CompressedBlock::ScanLine(b) => b.compressed_pixels_le.as_slice(),
                    CompressedBlock::Tile(b) => b.compressed_pixels_le.as_slice(),
                    _ => &[],
                };
                if !dwa_counts_fit(header.compression, packed, block_bytes) {
                    return Err(exr::error::Error::Invalid("a DWA block claims more data than it can hold".into()));
                }
                UncompressedBlock::decompress_chunk(chunk, chunk_reader.meta_data(), false)
            });
            match block {
                Ok(block) => {
                    let li = block.index.level.x() - first;
                    if let Some(level) = levels.get_mut(li) {
                        fill(level, &header, block.index, &block.data, &wanted);
                    }
                }
                // A damaged block leaves a hole; the rest still shows.
                Err(e) => {
                    failed.get_or_insert(e.to_string());
                }
            }
        }
        if let Some(e) = failed {
            if e.contains("unimplemented") || e.contains("unsupported") {
                return Err(format!("{} compression: {e}", info.compression));
            }
            notes.push(format!("some blocks couldn't be read ({e})"));
        }
    }

    finish(levels, &header, map[3].is_some(), meaning, first == 0, opts, notes)
}

/// Whether a DWAA or DWAB block's leading counters (eleven little-endian
/// u64s) ask for no more than the block can hold, `block_bytes` uncompressed.
/// `exr` allocates by the uncompressed sizes and coefficient counts before
/// it checks them, so one damaged counter asked for petabytes: an
/// allocation failure, which aborts the process rather than panicking.
/// Other compressions, and blocks stored whole, always pass.
pub(crate) fn dwa_counts_fit(compression: Compression, packed: &[u8], block_bytes: usize) -> bool {
    if !matches!(compression, Compression::DWAA(_) | Compression::DWAB(_)) || packed.len() >= block_bytes {
        return true;
    }
    let limit = (block_bytes as u64).saturating_mul(2).saturating_add(4096);
    let counter = |i: usize| packed.get(i * 8..i * 8 + 8).map(|b| u64::from_le_bytes(b.try_into().unwrap_or([0; 8])));
    // Unknown section and RLE sizes uncompressed, raw RLE size, AC and DC
    // coefficient counts.
    [1, 5, 6, 7, 8].iter().all(|&i| counter(i).is_none_or(|c| c <= limit))
}

/// Turns decoded levels into the picture: luminance and chroma to RGB, the
/// data window placed in the display window, and the size asked for.
fn finish(
    mut levels: Vec<Level>,
    header: &Header,
    has_alpha: bool,
    meaning: Meaning,
    full_size: bool,
    opts: &Options,
    mut notes: Vec<String>,
) -> Result<Picture, String> {
    let data = header.data_window();
    // 4. Luminance and chroma to RGB.
    if meaning == Meaning::LuminanceChroma {
        let weights = luminance_weights(header);
        for level in &mut levels {
            for p in level.rgba.chunks_exact_mut(4) {
                let (y, ry, by) = (p[0], p[1], p[2]);
                let r = (ry + 1.0) * y;
                let b = (by + 1.0) * y;
                let g = (y - r * weights[0] - b * weights[2]) / weights[1];
                (p[0], p[1], p[2]) = (r, g, b);
            }
        }
    }

    // 5. Colour in other primaries to sRGB's.
    if opts.to_srgb
        && matches!(meaning, Meaning::Colour | Meaning::LuminanceChroma)
        && let Some(m) = header.shared_attributes.chromaticities.as_ref().and_then(to_srgb_matrix)
    {
        for level in &mut levels {
            for p in level.rgba.chunks_exact_mut(4) {
                let (r, g, b) = (p[0], p[1], p[2]);
                for (k, row) in m.iter().enumerate() {
                    p[k] = row[0] * r + row[1] * g + row[2] * b;
                }
            }
        }
        notes.push("colour converted from the file's primaries to sRGB".into());
    }

    // 6. Place the data window in the display window, when the two differ
    //    and the display window is a sensible size.
    if full_size && opts.place {
        let display = Window::of(header.shared_attributes.display_window);
        let stored = Window::of(data);
        if display != stored {
            let overlaps = stored.x < display.x.saturating_add(display.width as i32)
                && display.x < stored.x.saturating_add(stored.width as i32)
                && stored.y < display.y.saturating_add(display.height as i32)
                && display.y < stored.y.saturating_add(stored.height as i32);
            if overlaps && display.pixels() <= opts.max_pixels && display.width > 0 && display.height > 0 {
                let placed = place(&levels[0], stored, display);
                levels = vec![placed];
                let inside = stored.x >= display.x
                    && stored.y >= display.y
                    && stored.x as i64 + stored.width as i64 <= display.x as i64 + display.width as i64
                    && stored.y as i64 + stored.height as i64 <= display.y as i64 + display.height as i64;
                notes.push(if inside {
                    "the stored pixels cover part of the display window; the rest is transparent".to_string()
                } else {
                    "stored pixels outside the display window are cut off".to_string()
                });
            } else {
                notes.push("the data window lies outside the display window; showing the data window".into());
                levels.truncate(1);
            }
        }
    }

    // 7. Shrink to the size asked for.
    if let Some(max) = opts.max_side {
        let l = levels.swap_remove(0);
        levels = vec![shrink(l, max)];
    }
    Ok(Picture { levels, has_alpha, meaning, notes })
}

/// The weights of red, green and blue in luminance, from the file's
/// chromaticities (Rec. 709, OpenEXR's default, when it has none).
fn luminance_weights(h: &Header) -> [f32; 3] {
    let Some(c) = &h.shared_attributes.chromaticities else { return [0.2126, 0.7152, 0.0722] };
    // The Y row of the RGB to XYZ matrix the primaries and white point give.
    let xyz = |x: f32, y: f32| [x / y, 1.0, (1.0 - x - y) / y];
    let (r, g, b, w) = (
        xyz(c.red.x(), c.red.y()),
        xyz(c.green.x(), c.green.y()),
        xyz(c.blue.x(), c.blue.y()),
        xyz(c.white.x(), c.white.y()),
    );
    let m = [[r[0], g[0], b[0]], [r[1], g[1], b[1]], [r[2], g[2], b[2]]];
    let Some(s) = solve3(m, w) else { return [0.2126, 0.7152, 0.0722] };
    let weights = [s[0], s[1], s[2]];
    if weights.iter().all(|v| v.is_finite() && *v > 0.0) { weights } else { [0.2126, 0.7152, 0.0722] }
}

/// The matrix from linear RGB in `c`'s primaries to linear sRGB, adapting
/// the white point with Bradford's method; `None` when `c` is sRGB's own
/// (or nonsense).
fn to_srgb_matrix(c: &exr::meta::attribute::Chromaticities) -> Option<[[f32; 3]; 3]> {
    let p = [c.red.x(), c.red.y(), c.green.x(), c.green.y(), c.blue.x(), c.blue.y(), c.white.x(), c.white.y()];
    let srgb = [0.64, 0.33, 0.30, 0.60, 0.15, 0.06, 0.3127, 0.3290];
    if p.iter().zip(srgb).all(|(a, b)| (a - b).abs() < 2e-3) || p.iter().any(|v| !v.is_finite()) {
        return None;
    }
    let to_xyz = rgb_to_xyz(&p)?;
    let from_xyz = invert(rgb_to_xyz(&srgb)?)?;
    // Bradford adaptation from the file's white to D65.
    let xyz = |x: f32, y: f32| [x / y, 1.0, (1.0 - x - y) / y];
    let bradford = [[0.8951, 0.2664, -0.1614], [-0.7502, 1.7135, 0.0367], [0.0389, -0.0685, 1.0296]];
    let (src, dst) = (mul_v(&bradford, xyz(p[6], p[7])), mul_v(&bradford, xyz(srgb[6], srgb[7])));
    let scale = [[dst[0] / src[0], 0.0, 0.0], [0.0, dst[1] / src[1], 0.0], [0.0, 0.0, dst[2] / src[2]]];
    let adapt = mul(&invert(bradford)?, &mul(&scale, &bradford));
    let m = mul(&from_xyz, &mul(&adapt, &to_xyz));
    m.iter().flatten().all(|v| v.is_finite()).then_some(m)
}

/// RGB to XYZ for primaries and white `[rx, ry, gx, gy, bx, by, wx, wy]`.
fn rgb_to_xyz(p: &[f32; 8]) -> Option<[[f32; 3]; 3]> {
    let col = |x: f32, y: f32| [x / y, 1.0, (1.0 - x - y) / y];
    let (r, g, b, w) = (col(p[0], p[1]), col(p[2], p[3]), col(p[4], p[5]), col(p[6], p[7]));
    let m = [[r[0], g[0], b[0]], [r[1], g[1], b[1]], [r[2], g[2], b[2]]];
    let s = solve3(m, w)?;
    Some([0, 1, 2].map(|i| [m[i][0] * s[0], m[i][1] * s[1], m[i][2] * s[2]]))
}

fn mul(a: &[[f32; 3]; 3], b: &[[f32; 3]; 3]) -> [[f32; 3]; 3] {
    [0, 1, 2].map(|i| [0, 1, 2].map(|j| a[i][0] * b[0][j] + a[i][1] * b[1][j] + a[i][2] * b[2][j]))
}

fn mul_v(a: &[[f32; 3]; 3], v: [f32; 3]) -> [f32; 3] {
    [0, 1, 2].map(|i| a[i][0] * v[0] + a[i][1] * v[1] + a[i][2] * v[2])
}

fn invert(m: [[f32; 3]; 3]) -> Option<[[f32; 3]; 3]> {
    let cols: Vec<[f32; 3]> = (0..3).map(|j| solve3(m, [0, 1, 2].map(|i| (i == j) as u8 as f32))).collect::<Option<_>>()?;
    Some([0, 1, 2].map(|i| [cols[0][i], cols[1][i], cols[2][i]]))
}

/// Solves `m x = v` by Cramer's rule.
fn solve3(m: [[f32; 3]; 3], v: [f32; 3]) -> Option<[f32; 3]> {
    let det = |m: [[f32; 3]; 3]| {
        m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1]) - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
            + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0])
    };
    let d = det(m);
    if d.abs() < 1e-12 {
        return None;
    }
    let mut out = [0f32; 3];
    for (c, o) in out.iter_mut().enumerate() {
        let mut mc = m;
        for r in 0..3 {
            mc[r][c] = v[r];
        }
        *o = det(mc) / d;
    }
    Some(out)
}

/// Copies the wanted channels of one decompressed block into a level.
/// `wanted` holds the part's channel indices for red, green, blue and alpha.
fn fill(level: &mut Level, header: &Header, index: BlockIndex, data: &[u8], wanted: &[Option<usize>]) {
    let (w, h) = (level.width as usize, level.height as usize);
    for (range, line) in LineIndex::lines_in_block(index, &header.channels) {
        let slots: Vec<usize> = (0..4).filter(|&s| wanted[s] == Some(line.channel)).collect();
        if slots.is_empty() {
            continue;
        }
        let (x0, y) = (line.position.x(), line.position.y());
        if y >= h || x0 >= w {
            continue;
        }
        let Some(bytes) = data.get(range) else { continue };
        let sample = header.channels.list[line.channel].sample_type;
        let count = line.sample_count.min(w - x0);
        let row = &mut level.rgba[(y * w + x0) * 4..(y * w + x0 + count) * 4];
        for (i, px) in row.chunks_exact_mut(4).enumerate() {
            let v = read_sample(bytes, i, sample);
            for &s in &slots {
                px[s] = v;
            }
        }
    }
}

/// Sample `i` of a run of native-endian samples, as a float.
fn read_sample(bytes: &[u8], i: usize, sample: SampleType) -> f32 {
    match sample {
        SampleType::F16 => {
            bytes.get(i * 2..i * 2 + 2).map_or(0.0, |b| half::f16::from_bits(u16::from_ne_bytes([b[0], b[1]])).to_f32())
        }
        SampleType::F32 => bytes.get(i * 4..i * 4 + 4).map_or(0.0, |b| f32::from_ne_bytes([b[0], b[1], b[2], b[3]])),
        SampleType::U32 => {
            bytes.get(i * 4..i * 4 + 4).map_or(0.0, |b| u32::from_ne_bytes([b[0], b[1], b[2], b[3]]) as f32)
        }
    }
}

/// The data window's pixels placed where they belong in the display
/// window.  The rest is transparent, also in pictures without alpha, so
/// one can see where the stored pixels end.
fn place(level: &Level, stored: Window, display: Window) -> Level {
    let (dw, dh) = (display.width as usize, display.height as usize);
    let mut rgba = vec![0f32; dw * dh * 4];
    let (sw, sh) = (level.width as i64, level.height as i64);
    for sy in 0..sh {
        let dy = stored.y as i64 + sy - display.y as i64;
        if dy < 0 || dy >= dh as i64 {
            continue;
        }
        let sx0 = (display.x as i64 - stored.x as i64).max(0);
        let sx1 = (display.x as i64 + dw as i64 - stored.x as i64).min(sw);
        if sx0 >= sx1 {
            continue;
        }
        let dx0 = (stored.x as i64 + sx0 - display.x as i64) as usize;
        let n = (sx1 - sx0) as usize;
        let src = ((sy * sw + sx0) * 4) as usize;
        let dst = (dy as usize * dw + dx0) * 4;
        rgba[dst..dst + n * 4].copy_from_slice(&level.rgba[src..src + n * 4]);
    }
    Level { width: display.width, height: display.height, rgba }
}

/// Halves (by averaging 2 × 2 blocks) until the longer side is at most
/// `max`, then scales the rest of the way by nearest samples.
pub fn shrink(mut l: Level, max: u32) -> Level {
    let max = max.max(1);
    while l.width.max(l.height) >= max * 2 && l.width > 1 && l.height > 1 {
        let (w, nw, nh) = (l.width as usize, (l.width / 2) as usize, (l.height / 2) as usize);
        let mut out = vec![0f32; nw * nh * 4];
        for y in 0..nh {
            for x in 0..nw {
                for c in 0..4 {
                    let at = |xx: usize, yy: usize| l.rgba[(yy * w + xx) * 4 + c];
                    out[(y * nw + x) * 4 + c] = 0.25
                        * (at(2 * x, 2 * y) + at(2 * x + 1, 2 * y) + at(2 * x, 2 * y + 1) + at(2 * x + 1, 2 * y + 1));
                }
            }
        }
        l = Level { width: nw as u32, height: nh as u32, rgba: out };
    }
    if l.width.max(l.height) > max {
        let scale = max as f64 / l.width.max(l.height) as f64;
        let nw = ((l.width as f64 * scale).round() as u32).max(1);
        let nh = ((l.height as f64 * scale).round() as u32).max(1);
        let mut out = Vec::with_capacity((nw * nh * 4) as usize);
        for y in 0..nh {
            let sy = ((y as f64 + 0.5) / scale) as usize;
            for x in 0..nw {
                let sx = ((x as f64 + 0.5) / scale) as usize;
                let i = (sy.min(l.height as usize - 1) * l.width as usize + sx.min(l.width as usize - 1)) * 4;
                out.extend_from_slice(&l.rgba[i..i + 4]);
            }
        }
        l = Level { width: nw, height: nh, rgba: out };
    }
    l
}

//endregion

mod subsampled;

#[cfg(test)]
mod tests;
