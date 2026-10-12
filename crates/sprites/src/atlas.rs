//! Sprite-sheet descriptors: the text or JSON files that say where each
//! sprite sits in a sheet picture.  The picture the descriptor names is
//! shown with every sprite's rectangle outlined, and the sprites are listed
//! (name and rectangle) in the inspector and as toggleable layers.
//!
//! - libGDX and Spine `.atlas` (text, old and new syntax, several pages).
//! - TexturePacker's `.tpsheet`: the older text form Unity reads, and the
//!   JSON form Godot and newer Unity use.
//! - TexturePacker and Aseprite JSON (`frames` as a hash or an array, with
//!   `meta.image`), and TexturePacker's multi-sheet `textures` form, when
//!   the file is named to the loader directly or sniffed by content.
//!
//! Only the first page whose picture can be read is drawn; the others are
//! listed.  A missing picture is drawn as a blank sheet of the stated size
//! with a warning, so the rectangles are still visible.

use crate::pixels;
use serde_json::Value;
use slerp_scene::{Canvas, CanvasLayer, FrameClip, Image8, Info, LoadError};
use std::path::{Path, PathBuf};

/// The most sprites listed and outlined.
const MAX_REGIONS: usize = 4096;

/// The most sprites named in the inspector's detail list.
const MAX_DETAILS: usize = 200;

/// Pixels of a sheet picture at most (the textures crate also caps).
const MAX_SHEET_PIXELS: u64 = 1 << 26;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Region {
    pub name: String,
    pub x: i64,
    pub y: i64,
    pub w: i64,
    pub h: i64,
}

#[derive(Clone, Debug, Default)]
pub struct Page {
    pub image: String,
    /// The size the descriptor gives for the sheet, if any.
    pub size: Option<(i64, i64)>,
    pub regions: Vec<Region>,
    /// Rectangles count their `y` up from the bottom of the sheet, as
    /// TexturePacker's Unity text format does.
    pub y_up: bool,
}

const IMAGE_EXTENSIONS: &[&str] = &["png", "jpg", "jpeg", "webp", "tga", "bmp", "ktx", "ktx2", "dds", "pcx", "gif"];

fn is_image_name(s: &str) -> bool {
    let s = s.trim().to_ascii_lowercase();
    s.rsplit_once('.').is_some_and(|(stem, ext)| !stem.is_empty() && IMAGE_EXTENSIONS.contains(&ext))
}

//region Recognising

/// Whether a file is a sprite-sheet descriptor, by extension and content.
pub fn sniff(ext: &str, text: &str) -> bool {
    let t = text.trim_start_matches('\u{feff}').trim_start();
    match ext {
        "atlas" => libgdx_head(t),
        "tpsheet" => t.starts_with('#') || t.starts_with(':') || t.starts_with('{'),
        // TexturePacker and Aseprite JSON is claimed by content only: the
        // sheet's name and a frame list.
        "json" => t.starts_with('{') && t.contains("\"frames\"") && t.contains("\"meta\"") && t.contains("\"image\""),
        _ => false,
    }
}

/// libGDX atlases start with a picture name (possibly after blank lines) and
/// continue with `key: value` lines.
fn libgdx_head(t: &str) -> bool {
    let mut lines = t.lines().map(str::trim).filter(|l| !l.is_empty());
    let Some(first) = lines.next() else { return false };
    if first.contains(':') || !is_image_name(first) {
        return false;
    }
    lines.next().is_some_and(|l| l.contains(':'))
}

//endregion

//region Parsing

pub fn parse(ext: &str, text: &str) -> Result<Vec<Page>, LoadError> {
    let t = text.trim_start_matches('\u{feff}').trim_start();
    let pages = if t.starts_with('{') {
        parse_json(t)?
    } else if t.starts_with('#') || t.starts_with(':') {
        parse_tpsheet(t)
    } else if ext == "atlas" || libgdx_head(t) {
        parse_libgdx(t)
    } else {
        return Err(LoadError::invalid("not a sprite-sheet descriptor"));
    };
    let total: usize = pages.iter().map(|p| p.regions.len()).sum();
    if pages.is_empty() || total == 0 {
        return Err(LoadError::invalid("the descriptor lists no sprites"));
    }
    Ok(pages)
}

/// Two integers from `"a, b"`.
fn pair(v: &str) -> Option<(i64, i64)> {
    let mut it = v.split(',').map(|s| s.trim().parse::<i64>());
    Some((it.next()?.ok()?, it.next()?.ok()?))
}

fn parse_libgdx(t: &str) -> Vec<Page> {
    let mut pages: Vec<Page> = Vec::new();
    let mut new_page = true;
    // The region being read: key-value lines fill its rectangle.
    let mut region: Option<(String, [i64; 4], bool, i64)> = None;
    let finish = |region: &mut Option<(String, [i64; 4], bool, i64)>, pages: &mut Vec<Page>| {
        if let Some((name, [x, y, w, h], rotated, index)) = region.take()
            && let Some(p) = pages.last_mut()
            && p.regions.len() < MAX_REGIONS
        {
            // Rotated regions are stored turned a quarter, so their
            // rectangle on the sheet is the size the other way round.
            let (w, h) = if rotated { (h, w) } else { (w, h) };
            // Animation frames share a name and differ by their index.
            let name = if index >= 0 { format!("{name} [{index}]") } else { name };
            p.regions.push(Region { name, x, y, w, h });
        }
    };
    for raw in t.lines() {
        let line = raw.trim();
        if line.is_empty() {
            finish(&mut region, &mut pages);
            new_page = true;
            continue;
        }
        if let Some((key, value)) = line.split_once(':') {
            let value = value.trim();
            match (key.trim(), pages.last_mut(), region.as_mut()) {
                ("size", Some(p), None) => p.size = pair(value),
                ("xy", _, Some(r)) => {
                    if let Some((x, y)) = pair(value) {
                        (r.1[0], r.1[1]) = (x, y);
                    }
                }
                ("size", _, Some(r)) => {
                    if let Some((w, h)) = pair(value) {
                        (r.1[2], r.1[3]) = (w, h);
                    }
                }
                ("bounds", _, Some(r)) => {
                    let n: Vec<i64> = value.split(',').filter_map(|s| s.trim().parse().ok()).collect();
                    if n.len() == 4 {
                        r.1 = [n[0], n[1], n[2], n[3]];
                    }
                }
                ("rotate", _, Some(r)) => r.2 = value == "true" || value == "90" || value == "270",
                ("index", _, Some(r)) => r.3 = value.parse().unwrap_or(-1),
                _ => {}
            }
            continue;
        }
        if new_page {
            pages.push(Page { image: line.to_string(), ..Default::default() });
            new_page = false;
        } else {
            finish(&mut region, &mut pages);
            region = Some((line.to_string(), [0; 4], false, -1));
        }
    }
    finish(&mut region, &mut pages);
    pages
}

/// TexturePacker's text form for Unity: `:key=value` lines and one
/// `name;x;y;w;h;...` line per sprite.
fn parse_tpsheet(t: &str) -> Vec<Page> {
    let mut page = Page { y_up: true, ..Default::default() };
    for line in t.lines().map(str::trim) {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(meta) = line.strip_prefix(':') {
            match meta.split_once('=') {
                Some(("texture", v)) => page.image = v.trim().to_string(),
                Some(("size", v)) => {
                    page.size =
                        v.split_once('x').and_then(|(w, h)| Some((w.trim().parse().ok()?, h.trim().parse().ok()?)))
                }
                _ => {}
            }
            continue;
        }
        let f: Vec<&str> = line.split(';').collect();
        if f.len() >= 5 && page.regions.len() < MAX_REGIONS {
            let n: Vec<Option<i64>> = f[1..5].iter().map(|s| s.trim().parse().ok()).collect();
            if let [Some(x), Some(y), Some(w), Some(h)] = n[..] {
                page.regions.push(Region { name: f[0].to_string(), x, y, w, h });
            }
        }
    }
    vec![page]
}

fn rect(v: &Value) -> Option<(i64, i64, i64, i64)> {
    let g = |k: &str| v.get(k).and_then(Value::as_f64).map(|n| n.round() as i64);
    Some((g("x")?, g("y")?, g("w")?, g("h")?))
}

fn json_size(v: Option<&Value>) -> Option<(i64, i64)> {
    let v = v?;
    Some((v.get("w")?.as_f64()? as i64, v.get("h")?.as_f64()? as i64))
}

/// A sprite list in any of the JSON shapes: `frames` as a hash or an array,
/// `sprites` with `region`, entries with `frame`.
fn json_regions(list: &Value) -> Vec<Region> {
    let one = |name: &str, e: &Value| -> Option<Region> {
        let (x, y, w, h) = rect(e.get("frame").or_else(|| e.get("region"))?)?;
        let name = e.get("filename").and_then(Value::as_str).unwrap_or(name);
        // A rotated sprite's frame gives the size it has upright, while it
        // lies on its side in the sheet.
        let (w, h) = if e.get("rotated").and_then(Value::as_bool) == Some(true) { (h, w) } else { (w, h) };
        Some(Region { name: name.to_string(), x, y, w, h })
    };
    let mut out = Vec::new();
    match list {
        Value::Object(map) => out.extend(map.iter().take(MAX_REGIONS).filter_map(|(k, e)| one(k, e))),
        Value::Array(items) => {
            out.extend(items.iter().take(MAX_REGIONS).enumerate().filter_map(|(i, e)| one(&format!("sprite {i}"), e)))
        }
        _ => {}
    }
    out
}

fn parse_json(t: &str) -> Result<Vec<Page>, LoadError> {
    let root: Value = serde_json::from_str(t).map_err(|e| LoadError::invalid(format!("JSON: {e}")))?;
    let mut pages = Vec::new();
    if let Some(list) = root.get("textures").and_then(Value::as_array) {
        for tex in list.iter().take(64) {
            let image = tex.get("image").and_then(Value::as_str).unwrap_or("").to_string();
            let regions = json_regions(tex.get("sprites").or_else(|| tex.get("frames")).unwrap_or(&Value::Null));
            pages.push(Page { image, size: json_size(tex.get("size")), regions, y_up: false });
        }
    } else if let Some(frames) = root.get("frames") {
        let meta = root.get("meta");
        let image = meta.and_then(|m| m.get("image")).and_then(Value::as_str).unwrap_or("").to_string();
        pages.push(Page {
            image,
            size: json_size(meta.and_then(|m| m.get("size"))),
            regions: json_regions(frames),
            y_up: false,
        });
    }
    Ok(pages)
}

//endregion

//region The canvas

/// A sheet picture with its sprites' rectangles.
pub struct Atlas {
    info: Info,
    sheet: Image8,
    regions: Vec<Region>,
    layers: Vec<CanvasLayer>,
}

const OUTLINES: [[u8; 3]; 8] = [
    [255, 64, 64],
    [64, 255, 64],
    [80, 140, 255],
    [255, 220, 40],
    [255, 80, 255],
    [40, 240, 240],
    [255, 150, 40],
    [160, 255, 120],
];

/// Loads the descriptor at `path` (already read as `text`), and the sheet
/// picture it names from beside it when `read_sheet` allows.
pub fn load(path: &Path, ext: &str, text: &str, read_sheet: bool) -> Result<Atlas, LoadError> {
    let pages = parse(ext, text)?;
    let dir = path.parent().filter(|d| !d.as_os_str().is_empty()).unwrap_or(Path::new("."));
    let mut info = Info { format: "Sprite sheet descriptor".into(), ..Default::default() };
    let mut chosen: Option<(usize, Image8, PathBuf)> = None;
    let mut first_missing = None;
    for (i, page) in pages.iter().enumerate() {
        let file = dir.join(page.image.replace('\\', "/"));
        if page.image.is_empty() || page.image.contains("..") {
            continue;
        }
        info.neighbours.push(file.clone());
        if !read_sheet {
            first_missing.get_or_insert(i);
            continue;
        }
        match pixels::read_image(&file, MAX_SHEET_PIXELS) {
            Ok(img) => {
                chosen = Some((i, img, file));
                break;
            }
            Err(e) => {
                info.warnings.push(format!("couldn't read {}: {e}", page.image));
                first_missing.get_or_insert(i);
            }
        }
    }
    let (index, sheet) = match chosen {
        Some((i, img, _)) => (i, img),
        None => {
            // Draw the rectangles on an empty sheet of the stated size, or
            // just big enough for them.
            let i = first_missing.unwrap_or(0);
            let p = &pages[i];
            let (w, h) = p.size.unwrap_or_else(|| {
                (
                    p.regions.iter().map(|r| r.x + r.w).max().unwrap_or(1),
                    p.regions.iter().map(|r| r.y + r.h).max().unwrap_or(1),
                )
            });
            let (w, h) = (w.clamp(1, 4096) as u32, h.clamp(1, 4096) as u32);
            (i, pixels::blank(w, h))
        }
    };
    let page = &pages[index];
    let mut regions = page.regions.clone();
    if page.y_up {
        for r in &mut regions {
            r.y = sheet.height as i64 - r.y - r.h;
        }
    }
    info.details.push(("Sheet picture".into(), page.image.clone()));
    info.details.push(("Sheet size".into(), format!("{} × {}", sheet.width, sheet.height)));
    if pages.len() > 1 {
        info.details.push(("Pages".into(), format!("{} (page {} shown)", pages.len(), index + 1)));
        for (i, p) in pages.iter().enumerate().filter(|&(i, _)| i != index).take(16) {
            info.details.push((format!("Page {}", i + 1), format!("{} ({} sprites)", p.image, p.regions.len())));
        }
    }
    info.details.push(("Sprites".into(), regions.len().to_string()));
    for r in regions.iter().take(MAX_DETAILS) {
        info.details.push((r.name.clone(), format!("{}, {}  {} × {}", r.x, r.y, r.w, r.h)));
    }
    if regions.len() > MAX_DETAILS {
        info.details.push(("…".into(), format!("{} more sprites", regions.len() - MAX_DETAILS)));
    }
    let mut layers =
        vec![CanvasLayer { name: "Sheet".into(), visible: true, depth: 0, group: false, note: page.image.clone() }];
    for r in &regions {
        layers.push(CanvasLayer {
            name: r.name.clone(),
            visible: true,
            depth: 0,
            group: false,
            note: format!("{}, {}  {} × {}", r.x, r.y, r.w, r.h),
        });
    }
    Ok(Atlas { info, sheet, regions, layers })
}

impl Canvas for Atlas {
    fn info(&self) -> &Info {
        &self.info
    }

    fn size(&self) -> (u32, u32) {
        (self.sheet.width, self.sheet.height)
    }

    fn layers(&self) -> &[CanvasLayer] {
        &self.layers
    }

    fn frames(&self) -> &[f32] {
        &[0.1]
    }

    fn clips(&self) -> &[FrameClip] {
        &[]
    }

    fn compose(&self, _frame: usize, shown: &[bool], scale: f32) -> Image8 {
        let scale = scale.clamp(0.001, 1.0);
        let w = ((self.sheet.width as f32 * scale).round() as u32).max(1);
        let h = ((self.sheet.height as f32 * scale).round() as u32).max(1);
        let mut out = pixels::resize(&self.sheet, w, h);
        // Under the sheet goes a checkerboard, so transparent sprites show
        // their shape on any background.
        let cell = 8u32;
        for y in 0..h {
            for x in 0..w {
                let i = (y as usize * w as usize + x as usize) * 4;
                let a = out.pixels[i + 3] as u32;
                if a == 255 {
                    continue;
                }
                let base = if ((x / cell) + (y / cell)).is_multiple_of(2) { 0x58u32 } else { 0x70 };
                for c in 0..3 {
                    out.pixels[i + c] = ((out.pixels[i + c] as u32 * a + base * (255 - a) + 127) / 255) as u8;
                }
                out.pixels[i + 3] = 255;
            }
        }
        if shown.first() == Some(&false) {
            out.pixels.chunks_exact_mut(4).for_each(|p| p.copy_from_slice(&[0x58, 0x58, 0x58, 255]));
        }
        for (i, r) in self.regions.iter().enumerate() {
            if shown.get(i + 1) == Some(&false) {
                continue;
            }
            let colour = OUTLINES[i % OUTLINES.len()];
            let (x0, y0) = ((r.x as f32 * scale).round() as i64, (r.y as f32 * scale).round() as i64);
            let (x1, y1) =
                (((r.x + r.w) as f32 * scale).round() as i64 - 1, ((r.y + r.h) as f32 * scale).round() as i64 - 1);
            let (x1, y1) = (x1.max(x0), y1.max(y0));
            // A damaged rectangle can reach far past the sheet; keep the
            // loops to the picture.
            let (x0, x1) = (x0.clamp(-1, w as i64), x1.clamp(-1, w as i64));
            let (y0, y1) = (y0.clamp(-1, h as i64), y1.clamp(-1, h as i64));
            let mut put = |x: i64, y: i64| {
                if x >= 0 && y >= 0 && (x as u32) < w && (y as u32) < h {
                    let i = (y as usize * w as usize + x as usize) * 4;
                    out.pixels[i..i + 3].copy_from_slice(&colour);
                }
            };
            for x in x0..=x1 {
                put(x, y0);
                put(x, y1);
            }
            for y in y0..=y1 {
                put(x0, y);
                put(x1, y);
            }
        }
        out
    }
}

//endregion

#[cfg(test)]
mod tests {
    use super::*;

    const GDX_OLD: &str = "\npage1.png\nsize: 128,64\nformat: RGBA8888\nfilter: Nearest,Nearest\nrepeat: none\nhero\n  rotate: false\n  xy: 2, 4\n  size: 16, 24\n  orig: 16, 24\n  offset: 0, 0\n  index: -1\nspin\n  rotate: true\n  xy: 40, 0\n  size: 10, 20\n  orig: 10, 20\n  offset: 0, 0\n  index: -1\n\npage2.png\nsize: 32,32\nfilter: Linear,Linear\ncoin\nbounds: 1,2,3,4\n";

    #[test]
    fn libgdx_old_and_new_syntax_with_two_pages() {
        let pages = parse("atlas", GDX_OLD).unwrap();
        assert_eq!(pages.len(), 2);
        assert_eq!(pages[0].image, "page1.png");
        assert_eq!(pages[0].size, Some((128, 64)));
        assert_eq!(pages[0].regions[0], Region { name: "hero".into(), x: 2, y: 4, w: 16, h: 24 });
        // Rotated regions occupy the turned rectangle.
        assert_eq!(pages[0].regions[1], Region { name: "spin".into(), x: 40, y: 0, w: 20, h: 10 });
        assert_eq!(pages[1].regions[0], Region { name: "coin".into(), x: 1, y: 2, w: 3, h: 4 });
        let frames = parse("atlas", "a.png\nsize: 8,8\nwalk\n  xy: 0, 0\n  size: 2, 2\n  index: 0\nwalk\n  xy: 2, 0\n  size: 2, 2\n  index: 1\nidle\n  xy: 4, 0\n  size: 2, 2\n  index: -1\n").unwrap();
        let names: Vec<&str> = frames[0].regions.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, ["walk [0]", "walk [1]", "idle"]);
        assert!(sniff("atlas", GDX_OLD));
        assert!(!sniff("atlas", "{\"a\":1}"));
        assert!(!sniff("atlas", "hello world\nthis is not it"));
    }

    #[test]
    fn unity_text_tpsheet() {
        let t = "# Sprite sheet data for Unity.\n:format=40300\n:texture=sheet.png\n:size=256x128\n:pivotpoints=enabled\n\nrun_0;0;0;32;32;0.5;0.5;0;0;0;0\nrun_1;32;0;32;32;0.5;0.5;0;0;0;0\n";
        let pages = parse("tpsheet", t).unwrap();
        assert_eq!(pages[0].image, "sheet.png");
        assert_eq!(pages[0].size, Some((256, 128)));
        assert_eq!(pages[0].regions.len(), 2);
        assert_eq!(pages[0].regions[1].x, 32);
        assert!(sniff("tpsheet", t));
        // Unity counts y up from the bottom: the sprite at y 224 of a
        // 128-high sheet lies off the top, one at y 96 is the top row.
        let a = load(
            Path::new("/nonexistent/s.tpsheet"),
            "tpsheet",
            ":texture=s.png\n:size=64x128\na;0;96;32;32;0;0\n",
            true,
        )
        .unwrap();
        assert_eq!(a.regions[0].y, 0);
    }

    #[test]
    fn json_shapes() {
        let rotated =
            r#"{"frames":{"r":{"frame":{"x":0,"y":0,"w":10,"h":20},"rotated":true}},"meta":{"image":"s.png"}}"#;
        let p = parse("json", rotated).unwrap();
        assert_eq!((p[0].regions[0].w, p[0].regions[0].h), (20, 10));
        let godot = r#"{"textures":[{"image":"a.png","size":{"w":64,"h":64},"sprites":[{"filename":"s1","region":{"x":0,"y":0,"w":8,"h":8}}]}],"meta":{}}"#;
        let p = parse("tpsheet", godot).unwrap();
        assert_eq!((p[0].image.as_str(), p[0].regions[0].w), ("a.png", 8));
        let hash = r#"{"frames":{"a.png":{"frame":{"x":1,"y":2,"w":3,"h":4}},"b.png":{"frame":{"x":5,"y":6,"w":7,"h":8}}},"meta":{"image":"s.png","size":{"w":16,"h":16}}}"#;
        let p = parse("json", hash).unwrap();
        assert_eq!(p[0].regions.len(), 2);
        assert_eq!(p[0].size, Some((16, 16)));
        assert!(sniff("json", hash));
        let array = r#"{"frames":[{"filename":"x","frame":{"x":0,"y":0,"w":2,"h":2}}],"meta":{"image":"s.png"}}"#;
        assert_eq!(parse("json", array).unwrap()[0].regions[0].name, "x");
        // Ordinary JSON is not an atlas.
        assert!(!sniff("json", r#"{"name":"x","frames":3}"#));
        assert!(parse("json", r#"{"a":1}"#).is_err());
    }

    #[test]
    fn missing_sheet_draws_blank_with_outlines() {
        let a = load(Path::new("/nonexistent/x.atlas"), "atlas", GDX_OLD, true).unwrap();
        assert_eq!(a.size(), (128, 64));
        assert!(a.info().warnings.iter().any(|w| w.contains("page1.png")));
        assert_eq!(a.layers().len(), 3);
        let shown = vec![true; 3];
        let img = a.compose(0, &shown, 1.0);
        // The top edge of "hero" at (2,4) is outlined in the first colour.
        let i = (4 * 128 + 2) * 4;
        assert_eq!(&img.pixels[i..i + 3], &OUTLINES[0]);
        // Hiding the sprite removes its outline.
        let hidden = a.compose(0, &[true, false, true], 1.0);
        assert_ne!(&hidden.pixels[i..i + 3], &OUTLINES[0]);
        assert!(a.info().details.iter().any(|(k, v)| k == "hero" && v.contains("16 × 24")));
    }

    #[test]
    fn hostile_files() {
        let mut big = String::from("a.png\nsize: 1,1\n");
        for i in 0..10_000 {
            big.push_str(&format!("r{i}\nbounds: 0,0,1,1\n"));
        }
        let pages = parse("atlas", &big).unwrap();
        assert_eq!(pages[0].regions.len(), MAX_REGIONS);
        let a = load(Path::new("x.atlas"), "atlas", &big, false).unwrap();
        assert_eq!(a.layers().len(), MAX_REGIONS + 1);
        let tiny = parse("atlas", "a.png\nsize: 99999999,99999999\nr\nbounds: 0,0,999999999999,1\n").unwrap();
        let a = load(
            Path::new("x.atlas"),
            "atlas",
            "a.png\nsize: 99999999,99999999\nr\nbounds: 0,0,999999999999,1\n",
            false,
        )
        .unwrap();
        assert!(a.size().0 <= 4096 && a.size().1 <= 4096);
        let _ = (tiny, a.compose(0, &[], 1.0));
        assert!(parse("atlas", "").is_err());
    }
}
