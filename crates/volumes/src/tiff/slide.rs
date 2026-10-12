//! Whole-slide and other pyramidal TIFF files: Aperio `.svs`, Hamamatsu
//! `.ndpi`, Leica `.scn`, Philips and generic tiled pyramids.  A pyramid
//! holds the same picture at several sizes; thumbnails use the smallest level
//! that is big enough, and never the full-resolution one.  The viewer lists
//! levels, the label and the macro picture as parts.

use crate::dtype::{DType, Endian};
use crate::render::{ChannelStyle, ColorMap, Layout, Style, percentile_window, render};
use crate::volume::{ChannelInfo, Slice};
use slerp_scene::textures::{ColorSpace, SourceInfo, Texture};
use slerp_tiff::raster::{Options, Raster};
use slerp_tiff::{Page, Tiff, tag};
use std::sync::Arc;

/// The largest level the viewer decodes whole, in pixels.
pub const MAX_VIEW_PIXELS: u64 = 1 << 26;
/// The longest side the viewer draws, a limit common to GPUs.
pub const MAX_VIEW_SIDE: u64 = 16384;
/// The largest page decoded for a thumbnail, in pixels.
pub const MAX_THUMB_PIXELS: u64 = 1 << 25;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LevelKind {
    Level,
    Thumbnail,
    Label,
    Macro,
    /// An image with another shape that is none of these.
    Extra,
}

/// One size of one picture: a page, or one page per channel.
#[derive(Clone, Debug)]
pub struct SlideLevel {
    pub kind: LevelKind,
    pub width: u64,
    pub height: u64,
    pub pages: Vec<usize>,
}

#[derive(Clone, Debug)]
pub struct SlideImage {
    pub title: String,
    pub levels: Vec<SlideLevel>,
    pub channels: Vec<ChannelInfo>,
    /// The scan itself, rather than an overview.
    pub main: bool,
}

#[derive(Clone, Debug)]
pub struct Slide {
    pub vendor: &'static str,
    pub images: Vec<SlideImage>,
    pub facts: Vec<(String, String)>,
}

/// A part of a slide, named for the viewer's picker.
#[derive(Clone, Debug)]
pub struct SlidePart {
    pub name: String,
    pub image: usize,
    pub level: usize,
    /// Too large to decode whole.
    pub too_large: bool,
}

impl SlideLevel {
    fn pixels(&self) -> u64 {
        self.width.saturating_mul(self.height)
    }

    fn long(&self) -> u64 {
        self.width.max(self.height)
    }
}

/*┌─────────────────╮
| Recognition     |
└─────────────────╯*/
//region Recognition

fn mentions(desc: &str, word: &str) -> bool {
    let d = desc.trim_start().to_ascii_lowercase();
    d.starts_with(word) || d.contains(&format!("\n{word}")) || d.contains(&format!("|{word}"))
}

fn vendor_of(first: &Page) -> &'static str {
    if first.ifd.get(tag::NDPI_FORMAT_FLAG).is_some() {
        "Hamamatsu NDPI"
    } else if first.description.starts_with("Aperio") || first.software.starts_with("Aperio") {
        "Aperio"
    } else if first.description.contains("leica-microsystems.com/scn") {
        "Leica SCN"
    } else if first.description.contains("DPUfsImport") || first.software.contains("Philips") {
        "Philips"
    } else if first.description.starts_with("Scan Size") || first.description.contains("Huron") {
        "Huron"
    } else {
        "pyramidal TIFF"
    }
}

fn level_name(kind: LevelKind, index: usize, extra: usize) -> String {
    match kind {
        LevelKind::Level => format!("Level {index}"),
        LevelKind::Thumbnail => "Thumbnail".into(),
        LevelKind::Label => "Label".into(),
        LevelKind::Macro => "Macro".into(),
        LevelKind::Extra => format!("Image {extra}"),
    }
}

/// Decides whether a file with these pages is a pyramid, and lays it out.
/// `pages` are all of the file's pages (small files only).
pub fn classify(t: &Tiff, pages: &[Page]) -> Result<Option<Slide>, String> {
    let first = &pages[0];
    let vendor = vendor_of(first);
    if vendor == "Leica SCN" {
        return leica(pages).map(Some);
    }
    // 1. Which pages are overviews, and which belong to the pyramid.
    let mut levels: Vec<(usize, LevelKind)> = Vec::new();
    let aspect = first.width as f64 / first.height as f64;
    for (i, p) in pages.iter().enumerate() {
        let lens = p.ifd.get(tag::NDPI_SOURCE_LENS).map(|e| t.floats(e)).and_then(|v| v.first().copied());
        let kind = if mentions(&p.description, "label") {
            LevelKind::Label
        } else if mentions(&p.description, "macro") || lens == Some(-1.0) {
            LevelKind::Macro
        } else if i == 0 || p.description.starts_with("level=") {
            // Philips levels carry tile-padded sizes, so their shapes drift.
            LevelKind::Level
        } else if (p.width as f64 / p.height as f64 / aspect - 1.0).abs() > 0.05 {
            LevelKind::Extra
        } else {
            LevelKind::Level
        };
        levels.push((i, kind));
    }
    let main: Vec<usize> = levels.iter().filter(|(_, k)| *k == LevelKind::Level).map(|(i, _)| *i).collect();
    // 2. A pyramid has a large tiled first page, or several pages that shrink.
    let big = first.pixels() >= 8_000_000 && (first.is_tiled() || vendor == "Hamamatsu NDPI");
    let shrinking = main.len() >= 2
        && main
            .windows(2)
            .all(|w| pages[w[1]].pixels() * 3 / 2 <= pages[w[0]].pixels() || pages[w[1]].subfile_type & 1 == 1)
        && first.is_tiled();
    let vendor_known = vendor != "pyramidal TIFF";
    if !(big || shrinking || (vendor_known && pages.len() >= 2 && first.pixels() >= 1_000_000)) {
        return Ok(None);
    }
    // 3. Levels by size, largest first; a small untiled one after a tiled
    //    page is the file's own thumbnail (Aperio puts it second).
    let mut sorted = main.clone();
    sorted.sort_by_key(|&i| std::cmp::Reverse(pages[i].pixels()));
    let mut out = Vec::new();
    let mut n = 0;
    let mut extras = 0;
    for &i in &sorted {
        let p = &pages[i];
        let thumb = !p.is_tiled() && p.pixels() <= 2_000_000 && i != sorted[0] && vendor == "Aperio";
        let kind = if thumb { LevelKind::Thumbnail } else { LevelKind::Level };
        out.push(SlideLevel { kind, width: p.width, height: p.height, pages: vec![i] });
        if kind == LevelKind::Level {
            n += 1;
        }
    }
    let _ = n;
    for (i, kind) in &levels {
        if matches!(kind, LevelKind::Label | LevelKind::Macro | LevelKind::Extra) {
            if *kind == LevelKind::Extra {
                extras += 1;
            }
            out.push(SlideLevel { kind: *kind, width: pages[*i].width, height: pages[*i].height, pages: vec![*i] });
        }
    }
    let _ = extras;
    let mut facts = vec![("Slide format".to_string(), vendor.to_string())];
    // Aperio and others put the scan's calibration in the description.
    for line in first.description.split(['\n', '|']).map(str::trim) {
        if let Some((k, v)) = line.split_once('=') {
            let k = k.trim();
            if matches!(k, "AppMag" | "MPP" | "Date" | "Time" | "Filename" | "ScanScope ID" | "Left" | "Top") {
                facts.push((k.to_string(), v.trim().to_string()));
            }
        }
    }
    if let Some(e) = first.ifd.get(tag::NDPI_SOURCE_LENS)
        && let Some(v) = t.floats(e).first()
    {
        facts.push(("Objective".into(), format!("{v}x")));
    }
    Ok(Some(Slide {
        vendor,
        images: vec![SlideImage { title: "Slide".into(), levels: out, channels: Vec::new(), main: true }],
        facts,
    }))
}

/// Leica SCN: an XML description lists images with their pyramid levels and
/// channels, each a page.
fn leica(pages: &[Page]) -> Result<Slide, String> {
    let doc = roxmltree::Document::parse_with_options(
        &pages[0].description,
        roxmltree::ParsingOptions { allow_dtd: false, ..Default::default() },
    )
    .map_err(|e| format!("the SCN description is damaged: {e}"))?;
    let named = |n: &roxmltree::Node, name: &str| n.is_element() && n.tag_name().name() == name;
    let collection = doc.descendants().find(|n| named(n, "collection")).ok_or("the SCN file has no collection")?;
    let (cw, ch) = (
        collection.attribute("sizeX").and_then(|v| v.parse::<f64>().ok()).unwrap_or(0.0),
        collection.attribute("sizeY").and_then(|v| v.parse::<f64>().ok()).unwrap_or(0.0),
    );
    let mut images = Vec::new();
    for (n, image) in collection.children().filter(|n| named(n, "image")).enumerate().take(64) {
        let Some(pixels) = image.children().find(|c| named(c, "pixels")) else { continue };
        let mut dims: Vec<(usize, usize, usize, u64, u64)> = Vec::new();
        for d in pixels.children().filter(|c| named(c, "dimension")) {
            let get = |a: &str| -> Option<u64> { d.attribute(a).and_then(|v| v.parse().ok()) };
            if let (Some(ifd), Some(w), Some(h)) = (get("ifd"), get("sizeX"), get("sizeY"))
                && (ifd as usize) < pages.len()
            {
                dims.push((get("r").unwrap_or(0) as usize, get("c").unwrap_or(0) as usize, ifd as usize, w, h));
            }
        }
        if dims.is_empty() {
            continue;
        }
        let nlevels = dims.iter().map(|d| d.0).max().unwrap_or(0) + 1;
        let mut levels = Vec::new();
        for r in 0..nlevels {
            let mut mine: Vec<_> = dims.iter().filter(|d| d.0 == r).collect();
            mine.sort_by_key(|d| d.1);
            if let Some(f) = mine.first() {
                levels.push(SlideLevel {
                    kind: LevelKind::Level,
                    width: f.3,
                    height: f.4,
                    pages: mine.iter().map(|d| d.2).collect(),
                });
            }
        }
        let view = image.children().find(|c| named(c, "view"));
        let (vw, vh) = view
            .map(|v| {
                (
                    v.attribute("sizeX").and_then(|s| s.parse::<f64>().ok()).unwrap_or(0.0),
                    v.attribute("sizeY").and_then(|s| s.parse::<f64>().ok()).unwrap_or(0.0),
                )
            })
            .unwrap_or((0.0, 0.0));
        // An image covering the whole slide is the overview.
        let overview = cw > 0.0 && vw >= cw * 0.95 && vh >= ch * 0.95;
        let mut channels = Vec::new();
        let nch = dims.iter().map(|d| d.1).max().unwrap_or(0) + 1;
        if nch > 1 {
            let settings: Vec<_> = image.descendants().filter(|c| named(c, "channel")).collect();
            for c in 0..nch {
                let s = settings.iter().find(|s| s.attribute("index").and_then(|v| v.parse::<usize>().ok()) == Some(c));
                let color = s
                    .and_then(|s| s.attribute("rgb"))
                    .and_then(|v| u32::from_str_radix(v.trim_start_matches('#'), 16).ok())
                    .map_or(super::stack::default_channel_colors(nch)[c], |v| {
                        [(v >> 16) as u8, (v >> 8) as u8, v as u8]
                    });
                let name = s
                    .and_then(|s| s.attribute("name"))
                    .map(|n| n.split('|').next().unwrap_or(n).to_string())
                    .unwrap_or_else(|| format!("Channel {}", c + 1));
                channels.push(ChannelInfo { name, color });
            }
        }
        let light = image
            .descendants()
            .find(|c| named(c, "illuminationSource"))
            .and_then(|c| c.text())
            .unwrap_or("")
            .to_string();
        let title = format!(
            "Image {}{}{}",
            n + 1,
            if overview { " (overview)" } else { "" },
            if light.is_empty() { String::new() } else { format!(", {light}") }
        );
        images.push(SlideImage { title, levels, channels, main: !overview });
    }
    if images.is_empty() {
        return Err("the SCN file lists no images".into());
    }
    // The scans first, then the overviews.
    images.sort_by_key(|i| !i.main);
    Ok(Slide { vendor: "Leica SCN", images, facts: vec![("Slide format".into(), "Leica SCN".into())] })
}

//endregion

impl Slide {
    /// The parts a viewer lists, the best to show first: displayable levels
    /// largest first, then overviews, then levels too large to decode whole.
    pub fn parts(&self) -> Vec<SlidePart> {
        let multi = self.images.len() > 1;
        let (mut shown, mut big) = (Vec::new(), Vec::new());
        for (ii, im) in self.images.iter().enumerate() {
            let mut n = 0;
            let mut extra = 0;
            for (li, l) in im.levels.iter().enumerate() {
                if l.kind == LevelKind::Extra {
                    extra += 1;
                }
                let base = level_name(l.kind, n, extra);
                if l.kind == LevelKind::Level {
                    n += 1;
                }
                let chans = if l.pages.len() > 1 { format!(", {} channels", l.pages.len()) } else { String::new() };
                let size = format!("{} x {}", l.width, l.height);
                // Textures wider than the GPU's limit can't be drawn either.
                let too_large = l.pixels() > MAX_VIEW_PIXELS || l.long() > MAX_VIEW_SIDE;
                let name = if multi {
                    format!("{} {} ({size}{chans})", im.title, base.to_lowercase())
                } else {
                    format!("{base} ({size}{chans})")
                };
                let part = SlidePart {
                    name: if too_large { format!("{name}, too large to show") } else { name },
                    image: ii,
                    level: li,
                    too_large,
                };
                if too_large { big.push(part) } else { shown.push(part) }
            }
        }
        // Largest displayable first within the scan; overviews after.
        let rank = |p: &SlidePart| -> (bool, std::cmp::Reverse<u64>) {
            let im = &self.images[p.image];
            let l = &im.levels[p.level];
            (!(im.main && matches!(l.kind, LevelKind::Level)), std::cmp::Reverse(l.pixels()))
        };
        shown.sort_by_key(|p| (p.image, rank(p)));
        shown.sort_by_key(|p| !self.images[p.image].main);
        shown.extend(big);
        shown
    }

    /// The level a thumbnail of `size` pixels is made from: the smallest of
    /// the scan's levels whose longer side reaches `size`, else the largest.
    pub fn thumbnail_level(&self, size: u64) -> Option<(usize, usize)> {
        let image = self.images.iter().position(|i| i.main).unwrap_or(0);
        let im = &self.images[image];
        let mut cands: Vec<(usize, &SlideLevel)> = im
            .levels
            .iter()
            .enumerate()
            .filter(|(_, l)| matches!(l.kind, LevelKind::Level | LevelKind::Thumbnail))
            .collect();
        cands.sort_by_key(|(_, l)| l.long());
        let pick = cands.iter().find(|(_, l)| l.long() >= size).or(cands.last())?;
        Some((image, pick.0))
    }
}

/*┌─────────────────╮
| Pictures        |
└─────────────────╯*/
//region Pictures

/// Builds a picture from decoded pages: RGB as it is, a single grey channel
/// windowed, several channels composited in their colours.
pub fn picture(
    rasters: &[Raster],
    channels: &[ChannelInfo],
    title: &str,
    facts: Vec<(String, String)>,
    full: (u64, u64),
) -> Result<Texture, String> {
    let r0 = rasters.first().ok_or("no pixels")?;
    let (w, h) = (r0.width, r0.height);
    if rasters.iter().any(|r| (r.width, r.height) != (w, h)) {
        return Err("a slide's channels have different sizes".into());
    }
    let samples: usize = rasters.iter().map(|r| r.samples).sum();
    let mut data = vec![0f32; w * h * samples];
    let mut at = 0;
    for r in rasters {
        let mut vals = vec![0f32; w * h * r.samples];
        crate::dtype::decode(super::stack::dtype_of(r.sample), Endian::Little, &r.bytes, &mut vals);
        for px in 0..w * h {
            for s in 0..r.samples {
                data[px * samples + at + s] = vals[px * r.samples + s];
            }
        }
        at += r.samples;
    }
    let slice = Slice {
        width: w,
        height: h,
        samples,
        data,
        col_spacing: 0.0,
        row_spacing: 0.0,
        step: 1,
        edges: [None; 4],
        col_axis: 0,
        row_axis: 0,
        fixed: vec![0; 2],
        flip_rows: false,
        flip_cols: false,
        rows: h,
        cols: w,
        projected_axis: None,
    };
    let rgb = rasters.len() == 1 && matches!(r0.samples, 3 | 4);
    let style = if rgb {
        let window = if super::stack::dtype_of(r0.sample) == DType::U8 {
            (0.0, 255.0)
        } else {
            percentile_window(&slice.data, 0.5, 99.5)
        };
        Style { window, layout: Layout::Rgb }
    } else if rasters.len() == 1 && r0.samples == 1 {
        Style {
            window: percentile_window(&slice.data, 1.0, 99.0),
            layout: Layout::Single { map: ColorMap::Gray, invert: r0.invert },
        }
    } else {
        let colors = super::stack::default_channel_colors(samples);
        let chans = (0..samples)
            .map(|c| {
                let vals: Vec<f32> = slice.data.iter().skip(c).step_by(samples).copied().collect();
                let color = channels.get(c).map_or(colors[c], |i| i.color);
                ChannelStyle { color, window: percentile_window(&vals, 1.0, 99.5), visible: true }
            })
            .collect();
        Style { window: (0.0, 1.0), layout: Layout::Channels(chans) }
    };
    let (rgba, alpha) = render(&slice, &style);
    let mut tex = Texture::from_rgba8(w as u32, h as u32, rgba, ColorSpace::Srgb);
    tex.has_alpha = alpha;
    tex.source = SourceInfo {
        container: title.to_string(),
        format: format!("{} samples, {}", samples, super::stack::dtype_of(r0.sample).name()),
        width: full.0 as u32,
        height: full.1 as u32,
        depth: 1,
        layers: 1,
        faces: 1,
        mip_count: 1,
        header: facts,
        ..Default::default()
    };
    Ok(tex)
}

/// Decodes the pages of one level.
pub fn decode_level(t: &Tiff, pages: &[Page], level: &SlideLevel, opts: &Options) -> Result<Vec<Raster>, String> {
    level.pages.iter().map(|&i| pages[i].decode(t, opts)).collect()
}

/// The picture of a slide's level, at most `max_pixels` large.
pub fn level_picture(
    t: &Arc<Tiff>,
    pages: &[Page],
    slide: &Slide,
    image: usize,
    level: usize,
    opts: &Options,
) -> Result<Texture, String> {
    let im = &slide.images[image];
    let l = &im.levels[level];
    let rasters = decode_level(t, pages, l, opts)?;
    let mut facts = slide.facts.clone();
    facts.push(("Level size".into(), format!("{} x {}", l.width, l.height)));
    if let Some(w) = rasters.iter().find_map(|r| r.warning.clone()) {
        facts.push(("Warning".into(), w));
    }
    facts.extend(super::stack::page_facts(&pages[l.pages[0]], t));
    let title = format!("{} slide", slide.vendor);
    let full = pages[im.levels[0].pages[0]].width;
    let _ = full;
    picture(&rasters, &im.channels, &title, facts, (l.width, l.height))
}

//endregion
