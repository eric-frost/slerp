//! OpenRaster (`.ora`), the exchange format of MyPaint, Krita, GIMP, Pinta
//! and others, and Krita's own `.kra`: both are zip files whose first entry
//! names their type.
//!
//! An OpenRaster file holds `stack.xml` (the tree of stacks and layers,
//! each layer a PNG inside the zip), `mergedimage.png` (the flattened
//! picture, required since OpenRaster 0.0.5) and `Thumbnails/thumbnail.png`.
//! The merged picture is shown when present; otherwise the stack is
//! composed here with the `composite-op` blend modes of the W3C compositing
//! specification, and failing that the thumbnail is shown.  Krita files are
//! read for their merged picture (`mergedimage.png`, else `preview.png`)
//! and the layer list in `maindoc.xml`; their own layer tiles aren't
//! decoded.

use crate::compose::{Kind, Mode, Node, Raster, Samples, Source};
use slerp_archive::Archive;

/// The most layers listed or composed.
const MAX_NODES: usize = 1 << 14;
const MAX_DEPTH: u32 = 64;
/// The largest PNG decoded, in bytes of pixels.
const MAX_PNG_BYTES: usize = 1 << 30;

/// Which of the two zip formats the first entry names, by the stored
/// `mimetype` member a zip starts with.
pub fn zip_kind(head: &[u8]) -> Option<&'static str> {
    if !head.starts_with(b"PK\x03\x04") || head.len() < 38 || &head[30..38] != b"mimetype" {
        return None;
    }
    let extra = u16::from_le_bytes([head[28], head[29]]) as usize;
    let body = head.get(38 + extra..)?;
    if body.starts_with(b"image/openraster") {
        Some("ora")
    } else if body.starts_with(b"application/x-krita") {
        Some("kra")
    } else {
        None
    }
}

/// A PNG to RGBA, 8 or 16 bits as stored, with its size.  Given `max_side`
/// (a thumbnail's), a picture more than twice that size is read at every
/// `step`th pixel each way, keeping about twice the side the thumbnail
/// needs: a 5 KB file can hold a flat 4096 x 4096 merged picture, which
/// takes 64 MB in full.  Rows are converted as they are inflated, so only
/// the result is held (interlaced pictures, rare in these files, are
/// inflated whole first).
pub fn png_raster(bytes: &[u8], max_side: Option<u32>) -> Result<Raster, String> {
    let limits = png::Limits { bytes: MAX_PNG_BYTES };
    let mut dec = png::Decoder::new_with_limits(std::io::Cursor::new(bytes), limits);
    dec.set_transformations(png::Transformations::EXPAND);
    // Damaged checksums are common in hand-made files and harmless here:
    // the data is checked for size as it is decoded.
    dec.ignore_checksums(true);
    let mut reader = dec.read_info().map_err(|e| e.to_string())?;
    let (w, h, interlaced) = (reader.info().width, reader.info().height, reader.info().interlaced);
    let step = max_side.map_or(1, |max| (w.max(h) / (2 * max.max(1))).max(1));
    let (out_w, out_h) = (w.div_ceil(step) as usize, h.div_ceil(step) as usize);
    let (color, depth) = reader.output_color_type();
    let ch = color.samples();
    let wide = depth == png::BitDepth::Sixteen;
    let mut out = vec![0u16; out_w.checked_mul(out_h).and_then(|n| n.checked_mul(4)).ok_or("the picture is too large")?];
    // 1. One decoded row into the result's row `oy`: grey, grey and alpha,
    //    RGB or RGBA to RGBA.
    let max = if wide { 65535 } else { 255 };
    let mut put = |row: &[u8], oy: usize| {
        let sample = |i: usize| -> u16 {
            if wide {
                row.get(i * 2..i * 2 + 2).map_or(0, |b| u16::from_be_bytes([b[0], b[1]]))
            } else {
                row.get(i).copied().unwrap_or(0) as u16
            }
        };
        for ox in 0..out_w {
            let at = ox * step as usize * ch;
            let s = |c: usize| sample(at + c);
            let px = match ch {
                1 => [s(0), s(0), s(0), max],
                2 => [s(0), s(0), s(0), s(1)],
                3 => [s(0), s(1), s(2), max],
                _ => [s(0), s(1), s(2), s(3)],
            };
            out[(oy * out_w + ox) * 4..][..4].copy_from_slice(&px);
        }
    };
    // 2. Every `step`th row.
    if interlaced {
        let mut buf = vec![0u8; reader.output_buffer_size().ok_or("the picture is too large")?];
        let frame = reader.next_frame(&mut buf).map_err(|e| e.to_string())?;
        for oy in 0..out_h {
            let y = oy * step as usize;
            put(buf.get(y * frame.line_size..(y + 1) * frame.line_size).unwrap_or(&[]), oy);
        }
    } else {
        let mut y = 0usize;
        while let Some(row) = reader.next_row().map_err(|e| e.to_string())? {
            if y % step as usize == 0 && y / (step as usize) < out_h {
                put(row.data(), y / step as usize);
            }
            y += 1;
        }
    }
    let samples = if wide { Samples::U16(out) } else { Samples::U8(out.into_iter().map(|v| v as u8).collect()) };
    Ok(Raster { width: out_w as u32, height: out_h as u32, channels: 4, samples })
}

/*┌─────────────────╮
| OpenRaster      |
└─────────────────╯*/
//region OpenRaster

pub struct Ora {
    archive: Archive,
    pub width: u32,
    pub height: u32,
    pub version: String,
    /// Each raster layer's path in the zip, by its id.
    sources: Vec<String>,
    pub tree: Vec<Node>,
    pub notes: Vec<String>,
}

impl Ora {
    pub fn open(archive: Archive) -> Result<Ora, String> {
        let stack = read_entry(&archive, "stack.xml")?;
        let text = String::from_utf8_lossy(&stack);
        let options = roxmltree::ParsingOptions { allow_dtd: true, nodes_limit: 1 << 20, ..Default::default() };
        let doc = roxmltree::Document::parse_with_options(&text, options).map_err(|e| format!("stack.xml: {e}"))?;
        let image = doc.root_element();
        if !image.has_tag_name("image") {
            return Err("stack.xml has no image element".into());
        }
        let num = |n: roxmltree::Node, a: &str| n.attribute(a).and_then(|v| v.trim().parse::<f64>().ok());
        let width = num(image, "w").unwrap_or(0.0).clamp(0.0, 1e6) as u32;
        let height = num(image, "h").unwrap_or(0.0).clamp(0.0, 1e6) as u32;
        let mut ora = Ora {
            archive,
            width,
            height,
            version: image.attribute("version").unwrap_or("").to_string(),
            sources: Vec::new(),
            tree: Vec::new(),
            notes: Vec::new(),
        };
        let Some(root) = image.children().find(|c| c.has_tag_name("stack")) else {
            return Err("stack.xml has no stack".into());
        };
        let mut unknown = Vec::new();
        let mut count = 0;
        ora.tree = ora.stack(root, 0, &mut unknown, &mut count);
        unknown.sort();
        unknown.dedup();
        if !unknown.is_empty() {
            ora.notes.push(format!("blend modes drawn as normal: {}", unknown.join(", ")));
        }
        Ok(ora)
    }

    /// The children of a stack, bottom first (the file lists them top
    /// first).  Stacks have no position since OpenRaster 0.0.6 (and no
    /// writer set one), so only layers' `x` and `y` count.  Stacks nested
    /// deeper than [`MAX_DEPTH`] are flattened into their parent.
    fn stack(&mut self, el: roxmltree::Node, depth: u32, unknown: &mut Vec<String>, count: &mut usize) -> Vec<Node> {
        let mut out = Vec::new();
        let kids: Vec<roxmltree::Node> = el.children().filter(|c| c.is_element()).collect();
        for c in kids.into_iter().rev() {
            if *count >= MAX_NODES {
                break;
            }
            *count += 1;
            if c.has_tag_name("stack") && depth >= MAX_DEPTH {
                out.extend(self.stack(c, depth, unknown, count));
                continue;
            }
            let int = |a: &str| {
                c.attribute(a).and_then(|v| v.trim().parse::<f64>().ok()).unwrap_or(0.0).clamp(-1e6, 1e6) as i32
            };
            let (cx, cy) = (int("x"), int("y"));
            let opacity =
                c.attribute("opacity").and_then(|v| v.trim().parse::<f32>().ok()).unwrap_or(1.0).clamp(0.0, 1.0);
            let visible = c.attribute("visibility").is_none_or(|v| v.trim() != "hidden");
            let op = c.attribute("composite-op").unwrap_or("svg:src-over").trim();
            let mut mode = ora_mode(op);
            if mode == Mode::Unknown {
                unknown.push(op.to_string());
            }
            let name = c.attribute("name").unwrap_or("").to_string();
            let kind = if c.has_tag_name("stack") {
                // "isolate" makes a group of its own, blended with its
                // composite-op.  "auto" (the default, which MyPaint writes
                // for pass-through groups) ignores the composite-op and puts
                // the children straight onto what is below, unless the
                // stack is translucent, which needs a group to fade.
                // A Porter-Duff operator (`src-atop` for clipping) only makes
                // sense on the stack's own result, so it isolates too.
                let porter_duff =
                    matches!(mode, Mode::SrcAtop | Mode::DstIn | Mode::DstOut | Mode::DstAtop | Mode::Xor);
                let isolate = c.attribute("isolation").is_some_and(|v| v.trim() == "isolate") || porter_duff;
                if !isolate {
                    mode = Mode::Normal;
                }
                Kind::Group { children: self.stack(c, depth + 1, unknown, count), isolated: isolate || opacity < 1.0 }
            } else if c.has_tag_name("layer") {
                match c.attribute("src") {
                    Some(src) if src.to_ascii_lowercase().ends_with(".png") => {
                        self.sources.push(src.to_string());
                        Kind::Raster { id: self.sources.len() - 1, x: cx, y: cy }
                    }
                    Some(src) => {
                        self.notes.push(format!("layer \"{name}\" is {src}, which isn't a PNG and isn't drawn"));
                        Kind::Empty
                    }
                    None => Kind::Empty,
                }
            } else {
                continue;
            };
            let mode = if matches!(kind, Kind::Group { isolated: false, .. }) { Mode::PassThrough } else { mode };
            out.push(Node {
                name,
                visible,
                opacity,
                mode,
                gimp: Default::default(),
                clip: false,
                mask: None,
                kind,
                slot: 0,
            });
        }
        out
    }

    /// The merged picture, reduced for a thumbnail of `max_side` (see
    /// [`png_raster`]).
    pub fn merged(&self, max_side: Option<u32>) -> Option<Result<Raster, String>> {
        find(&self.archive, "mergedimage.png").map(|bytes| bytes.and_then(|b| png_raster(&b, max_side)))
    }

    pub fn thumbnail(&self, max_side: Option<u32>) -> Option<Result<Raster, String>> {
        find(&self.archive, "Thumbnails/thumbnail.png").map(|bytes| bytes.and_then(|b| png_raster(&b, max_side)))
    }
}

impl Source for Ora {
    fn layer(&self, id: usize) -> Result<Raster, String> {
        let path = self.sources.get(id).ok_or("no such layer")?;
        png_raster(&read_entry(&self.archive, path)?, None)
    }

    fn mask(&self, _id: usize) -> Result<Raster, String> {
        Err("OpenRaster has no masks".into())
    }
}

/// OpenRaster's composite operators (the W3C compositing specification's
/// names), and the Krita and GIMP names their exporters use for the rest.
pub fn ora_mode(op: &str) -> Mode {
    let name = op.rsplit(':').next().unwrap_or(op).replace(['_', ' '], "-").to_ascii_lowercase();
    match name.as_str() {
        "src-over" | "normal" => Mode::Normal,
        "multiply" => Mode::Multiply,
        "screen" => Mode::Screen,
        "overlay" => Mode::Overlay,
        "darken" => Mode::Darken,
        "lighten" => Mode::Lighten,
        "color-dodge" | "dodge" => Mode::ColorDodge,
        "color-burn" | "burn" => Mode::ColorBurn,
        "hard-light" => Mode::HardLight,
        "soft-light" | "soft-light-svg" => Mode::SoftLight,
        "difference" => Mode::Difference,
        "exclusion" => Mode::Exclusion,
        "color" => Mode::Color,
        "luminosity" => Mode::Luminosity,
        "hue" => Mode::Hue,
        "saturation" => Mode::Saturation,
        "plus" => Mode::Plus,
        "dst-in" => Mode::DstIn,
        "dst-out" => Mode::DstOut,
        "src-atop" => Mode::SrcAtop,
        "dst-atop" => Mode::DstAtop,
        "xor" => Mode::Xor,
        "add" | "addition" | "linear-dodge" => Mode::Addition,
        "subtract" => Mode::Subtract,
        "divide" => Mode::Divide,
        "linear-burn" => Mode::LinearBurn,
        "vivid-light" => Mode::VividLight,
        "linear-light" => Mode::LinearLight,
        "pin-light" => Mode::PinLight,
        "hard-mix" => Mode::HardMix,
        "darker-color" => Mode::DarkerColor,
        "lighter-color" => Mode::LighterColor,
        "dissolve" => Mode::Dissolve,
        "erase" => Mode::DstOut,
        "pass-through" => Mode::PassThrough,
        _ => Mode::Unknown,
    }
}

//endregion

/*┌─────────────────╮
| Krita           |
└─────────────────╯*/
//region Krita

pub struct Kra {
    archive: Archive,
    pub width: u32,
    pub height: u32,
    pub color_space: String,
    /// (depth, name, kind, visible, composite op, opacity 0 to 255).
    pub layers: Vec<(u32, String, String, bool, String, u32)>,
    pub notes: Vec<String>,
}

impl Kra {
    pub fn open(archive: Archive) -> Result<Kra, String> {
        let mut kra =
            Kra { archive, width: 0, height: 0, color_space: String::new(), layers: Vec::new(), notes: Vec::new() };
        match read_entry(&kra.archive, "maindoc.xml") {
            Ok(xml) => {
                let text = String::from_utf8_lossy(&xml);
                let options = roxmltree::ParsingOptions { allow_dtd: true, nodes_limit: 1 << 20, ..Default::default() };
                match roxmltree::Document::parse_with_options(&text, options) {
                    Ok(doc) => {
                        if let Some(image) = doc.descendants().find(|n| n.has_tag_name("IMAGE")) {
                            let num = |a: &str| image.attribute(a).and_then(|v| v.parse::<u32>().ok()).unwrap_or(0);
                            kra.width = num("width");
                            kra.height = num("height");
                            kra.color_space = image.attribute("colorspacename").unwrap_or("").to_string();
                            if let Some(layers) = image.children().find(|n| n.has_tag_name("layers")) {
                                kra.list(layers, 0);
                            }
                        }
                    }
                    Err(e) => kra.notes.push(format!("maindoc.xml couldn't be read ({e})")),
                }
            }
            Err(e) => kra.notes.push(format!("no layer list ({e})")),
        }
        Ok(kra)
    }

    fn list(&mut self, el: roxmltree::Node, depth: u32) {
        if depth > MAX_DEPTH {
            return;
        }
        for l in el.children().filter(|c| c.has_tag_name("layer") || c.has_tag_name("mask")) {
            if self.layers.len() >= MAX_NODES {
                return;
            }
            let attr = |a: &str| l.attribute(a).unwrap_or("").to_string();
            let kind = if l.has_tag_name("mask") { attr("nodetype") + " mask" } else { attr("nodetype") };
            self.layers.push((
                depth,
                attr("name"),
                kind,
                attr("visible") != "0",
                attr("compositeop"),
                l.attribute("opacity").and_then(|v| v.parse().ok()).unwrap_or(255),
            ));
            for c in l.children().filter(|c| c.has_tag_name("layers") || c.has_tag_name("masks")) {
                self.list(c, depth + 1);
            }
        }
    }

    /// The merged picture, reduced for a thumbnail of `max_side` (see
    /// [`png_raster`]).
    pub fn merged(&self, max_side: Option<u32>) -> Option<Result<Raster, String>> {
        find(&self.archive, "mergedimage.png").map(|bytes| bytes.and_then(|b| png_raster(&b, max_side)))
    }

    pub fn preview(&self, max_side: Option<u32>) -> Option<Result<Raster, String>> {
        find(&self.archive, "preview.png").map(|bytes| bytes.and_then(|b| png_raster(&b, max_side)))
    }
}

//endregion

fn find(archive: &Archive, path: &str) -> Option<Result<Vec<u8>, String>> {
    let i = *archive.find(path).last()?;
    let e = archive.entry(i)?;
    Some(archive.read(e).map_err(|e| e.to_string()))
}

fn read_entry(archive: &Archive, path: &str) -> Result<Vec<u8>, String> {
    find(archive, path).unwrap_or_else(|| Err(format!("{path} is missing")))
}

#[cfg(test)]
pub mod tests {
    use super::*;

    /// A stored (uncompressed) zip of `files`, `mimetype` first.
    pub fn zip(files: &[(&str, &[u8])]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut central = Vec::new();
        for (name, data) in files {
            let at = out.len() as u32;
            let crc = crc32(data);
            let mut head = Vec::new();
            head.extend_from_slice(&[20, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
            head.extend_from_slice(&crc.to_le_bytes());
            head.extend_from_slice(&(data.len() as u32).to_le_bytes());
            head.extend_from_slice(&(data.len() as u32).to_le_bytes());
            head.extend_from_slice(&(name.len() as u16).to_le_bytes());
            head.extend_from_slice(&0u16.to_le_bytes());
            out.extend_from_slice(b"PK\x03\x04");
            out.extend_from_slice(&head);
            out.extend_from_slice(name.as_bytes());
            out.extend_from_slice(data);
            central.extend_from_slice(b"PK\x01\x02\x14\x00");
            central.extend_from_slice(&head);
            // Comment length, disk, internal and external attributes.
            central.extend_from_slice(&[0; 10]);
            central.extend_from_slice(&at.to_le_bytes());
            central.extend_from_slice(name.as_bytes());
        }
        let cd_at = out.len() as u32;
        out.extend_from_slice(&central);
        out.extend_from_slice(b"PK\x05\x06\0\0\0\0");
        out.extend_from_slice(&(files.len() as u16).to_le_bytes());
        out.extend_from_slice(&(files.len() as u16).to_le_bytes());
        out.extend_from_slice(&(central.len() as u32).to_le_bytes());
        out.extend_from_slice(&cd_at.to_le_bytes());
        out.extend_from_slice(&[0, 0]);
        out
    }

    fn crc32(data: &[u8]) -> u32 {
        let mut c = !0u32;
        for &b in data {
            c ^= b as u32;
            for _ in 0..8 {
                c = if c & 1 != 0 { 0xEDB8_8320 ^ (c >> 1) } else { c >> 1 };
            }
        }
        !c
    }

    pub fn png(w: u32, h: u32, rgba: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut enc = png::Encoder::new(&mut out, w, h);
        enc.set_color(png::ColorType::Rgba);
        enc.write_header().unwrap().write_image_data(rgba).unwrap();
        out
    }

    /// A PNG of `w` x `h` with `ch` samples per pixel of `depth`, its
    /// samples given big-endian as the file stores them.
    fn png_of(w: u32, h: u32, color: png::ColorType, depth: png::BitDepth, data: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut enc = png::Encoder::new(&mut out, w, h);
        enc.set_color(color);
        enc.set_depth(depth);
        enc.write_header().unwrap().write_image_data(data).unwrap();
        out
    }

    /// A picture decoded for a thumbnail is every `step`th pixel of the full
    /// one, for 8-bit RGB and 16-bit grey with alpha, and a big flat merged
    /// picture (a few kilobytes of file) comes out at twice the thumbnail.
    #[test]
    fn reduced_pictures_are_every_nth_pixel() {
        let (w, h) = (301u32, 170u32);
        let rgb: Vec<u8> = (0..w * h).flat_map(|i| [(i % w) as u8, (i / w) as u8, (i * 7) as u8]).collect();
        let grey: Vec<u8> = (0..w * h).flat_map(|i| [(i >> 8) as u8, i as u8, 0xff, (i % 3) as u8]).collect();
        for file in [
            png_of(w, h, png::ColorType::Rgb, png::BitDepth::Eight, &rgb),
            png_of(w, h, png::ColorType::GrayscaleAlpha, png::BitDepth::Sixteen, &grey),
        ] {
            let full = png_raster(&file, None).unwrap();
            assert_eq!((full.width, full.height), (w, h));
            // 301 / (2 x 64) gives every 2nd pixel.
            let small = png_raster(&file, Some(64)).unwrap();
            assert_eq!((small.width, small.height), (151, 85));
            for (oy, ox) in [(0, 0), (1, 1), (42, 150), (84, 7)] {
                for c in 0..4 {
                    let at = |r: &Raster, x: usize, y: usize| r.samples.get((y * r.width as usize + x) * 4 + c);
                    assert_eq!(at(&small, ox, oy), at(&full, ox * 2, oy * 2), "({ox}, {oy}) channel {c}");
                }
            }
        }
        let flat = png(2048, 2048, &vec![90u8; 2048 * 2048 * 4]);
        assert!(flat.len() < 64 << 10, "{} bytes", flat.len());
        let kra = zip(&[("mimetype", b"application/x-krita"), ("mergedimage.png", &flat)]);
        let archive = Archive::from_bytes(kra).unwrap();
        let merged = png_raster(&read_entry(&archive, "mergedimage.png").unwrap(), Some(256)).unwrap();
        assert_eq!((merged.width, merged.height), (512, 512));
    }

    #[test]
    fn stacks_offsets_and_modes() {
        let stack = br#"<?xml version="1.0"?><image w="4" h="4" version="0.0.5"><stack>
            <layer name="top" src="data/b.png" x="1" y="2" opacity="0.5" composite-op="svg:multiply"/>
            <stack name="inner" x="1" visibility="hidden" isolation="isolate"><layer name="c" src="data/a.png"/></stack>
            <layer name="bottom" src="data/a.png" composite-op="krita:linear_burn"/>
            <layer name="vector" src="data/v.svg"/>
        </stack></image>"#;
        let a = png(4, 4, &[200; 64]);
        let f = zip(&[("mimetype", b"image/openraster"), ("stack.xml", stack), ("data/a.png", &a), ("data/b.png", &a)]);
        assert_eq!(zip_kind(&f), Some("ora"));
        let ora = Ora::open(Archive::from_bytes(f).unwrap()).unwrap();
        assert_eq!((ora.width, ora.height), (4, 4));
        let names: Vec<&str> = ora.tree.iter().map(|n| n.name.as_str()).collect();
        assert_eq!(names, ["vector", "bottom", "inner", "top"]);
        assert_eq!(ora.tree[1].mode, Mode::LinearBurn);
        assert!(!ora.tree[2].visible);
        let Kind::Group { children, isolated } = &ora.tree[2].kind else { panic!() };
        assert!(*isolated);
        assert!(matches!(children[0].kind, Kind::Raster { x: 0, y: 0, .. }));
        assert!(matches!(ora.tree[3].kind, Kind::Raster { x: 1, y: 2, .. }));
        assert_eq!(ora.notes.len(), 1);
        assert!(ora.merged(None).is_none());
        assert_eq!(ora.layer(0).unwrap().width, 4);
    }

    #[test]
    fn krita_layer_list() {
        let doc = br#"<!DOCTYPE DOC><DOC><IMAGE width="10" height="20" colorspacename="RGBA"><layers>
            <layer name="Group" nodetype="grouplayer" visible="1" opacity="255" compositeop="normal"><layers>
            <layer name="Paint" nodetype="paintlayer" visible="0" opacity="128" compositeop="multiply"/></layers></layer>
            </layers></IMAGE></DOC>"#;
        let p = png(2, 2, &[1; 16]);
        let f = zip(&[("mimetype", b"application/x-krita"), ("maindoc.xml", doc), ("preview.png", &p)]);
        assert_eq!(zip_kind(&f), Some("kra"));
        let kra = Kra::open(Archive::from_bytes(f).unwrap()).unwrap();
        assert_eq!((kra.width, kra.height), (10, 20));
        assert_eq!(kra.layers.len(), 2);
        assert_eq!(kra.layers[1].0, 1);
        assert!(!kra.layers[1].3);
        assert!(kra.merged(None).is_none() && kra.preview(None).is_some());
    }
}
