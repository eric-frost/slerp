//! GIMP's own format, XCF, from GIMP 1 to GIMP 3 (file versions 0 to 26 and
//! beyond, with 64-bit offsets from version 11): the layer tree with
//! groups, offsets, opacity, visibility, masks and every layer mode, the
//! image precision (8, 16 and 32-bit integers, half and full floats, linear
//! or gamma-encoded) and the indexed, greyscale and RGB modes.  Tiles of 64 ×
//! 64 pixels are stored raw, run-length coded per byte or zlib compressed.
//!
//! GIMP doesn't store a flattened picture, so the layers are always
//! composed (by [`crate::compose`] with GIMP's rules); layers are decoded
//! from the file kept in memory when they are drawn.  Non-destructive
//! filters of GIMP 3 aren't applied.

use crate::compose::{Composite, GimpOverrides, Kind, Legacy, Mask, Mode, Node, Raster, Samples, Source, Space};
use std::sync::Arc;

const MAX_LAYERS: usize = 1 << 16;
/// GIMP's own limit on image sides.
const MAX_SIDE: u32 = 524_288;
const TILE: usize = 64;

/*┌─────────────────╮
| Reading         |
└─────────────────╯*/
//region Reading

struct R<'a> {
    d: &'a [u8],
    at: usize,
    wide: bool,
}

impl<'a> R<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], String> {
        let end = self.at.checked_add(n).filter(|&e| e <= self.d.len()).ok_or("the file is cut short")?;
        let s = &self.d[self.at..end];
        self.at = end;
        Ok(s)
    }
    fn u32(&mut self) -> Result<u32, String> {
        let b = self.take(4)?;
        Ok(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }
    fn i32(&mut self) -> Result<i32, String> {
        Ok(self.u32()? as i32)
    }
    fn f32(&mut self) -> Result<f32, String> {
        Ok(f32::from_bits(self.u32()?))
    }
    /// A file offset: 4 bytes, or 8 from version 11.
    fn offset(&mut self) -> Result<usize, String> {
        let v = if self.wide {
            let b = self.take(8)?;
            u64::from_be_bytes(b.try_into().unwrap_or([0; 8]))
        } else {
            self.u32()? as u64
        };
        usize::try_from(v).map_err(|_| "an offset is too large".to_string())
    }
    fn string(&mut self) -> Result<String, String> {
        let n = self.u32()? as usize;
        if n > 1 << 20 {
            return Err("a string is too long".into());
        }
        let b = self.take(n)?;
        Ok(String::from_utf8_lossy(b.strip_suffix(&[0]).unwrap_or(b)).into_owned())
    }
}

//endregion

/*┌─────────────────╮
| Structure       |
└─────────────────╯*/
//region Structure

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Component {
    U8,
    U16,
    U32,
    Half,
    Float,
    Double,
}

impl Component {
    fn bytes(self) -> usize {
        match self {
            Component::U8 => 1,
            Component::U16 | Component::Half => 2,
            Component::U32 | Component::Float => 4,
            Component::Double => 8,
        }
    }
}

pub struct Xcf {
    data: Arc<Vec<u8>>,
    pub version: u32,
    pub width: u32,
    pub height: u32,
    /// 0 RGB, 1 greyscale, 2 indexed.
    pub base: u32,
    pub component: Component,
    /// Linear light rather than gamma-encoded.
    pub linear: bool,
    compression: u8,
    colormap: Vec<[u8; 3]>,
    /// In file order: top first, each group before its contents.
    pub layers: Vec<XLayer>,
    pub notes: Vec<String>,
}

pub struct XLayer {
    pub name: String,
    pub width: u32,
    pub height: u32,
    /// GIMP's drawable type: RGB, RGBA, GRAY, GRAYA, INDEXED, INDEXEDA.
    pub kind: u32,
    pub opacity: f32,
    pub visible: bool,
    pub mode: u32,
    pub overrides: GimpOverrides,
    pub x: i32,
    pub y: i32,
    pub group: bool,
    pub path: Vec<u32>,
    hierarchy: usize,
    mask: Option<(u32, u32, usize)>,
    apply_mask: bool,
    pub floating: bool,
    pub text: bool,
    pub filters: bool,
}

pub fn is_xcf(head: &[u8]) -> bool {
    head.starts_with(b"gimp xcf ")
}

impl Xcf {
    pub fn parse(data: Arc<Vec<u8>>) -> Result<Xcf, String> {
        let d: &[u8] = &data;
        if d.len() < 26 || !is_xcf(d) {
            return Err("not a GIMP file".into());
        }
        // 1. "gimp xcf file" (version 0) or "gimp xcf v011".
        let version = if &d[9..13] == b"file" {
            0
        } else if d[9] == b'v' && d[13] == 0 {
            std::str::from_utf8(&d[10..13]).ok().and_then(|v| v.parse().ok()).ok_or("an unknown XCF version")?
        } else {
            return Err("an unknown XCF version".into());
        };
        let mut r = R { d, at: 14, wide: version >= 11 };
        let width = r.u32()?;
        let height = r.u32()?;
        let base = r.u32()?;
        if base > 2 {
            return Err("an unknown image type".into());
        }
        if width == 0 || height == 0 || width > MAX_SIDE || height > MAX_SIDE {
            return Err(format!("an image of {width} × {height} pixels"));
        }
        let (component, linear) = if version >= 4 { precision(version, r.u32()?)? } else { (Component::U8, false) };
        let mut x = Xcf {
            data: data.clone(),
            version,
            width,
            height,
            base,
            component,
            linear,
            compression: 0,
            colormap: Vec::new(),
            layers: Vec::new(),
            notes: Vec::new(),
        };
        // 2. Image properties.
        loop {
            let kind = r.u32()?;
            let size = r.u32()? as usize;
            if kind == 0 {
                break;
            }
            let start = r.at;
            match kind {
                // The colour map.
                1 => {
                    let n = (r.u32()? as usize).min(256);
                    let c = r.take(n * 3)?;
                    x.colormap = c.chunks_exact(3).map(|p| [p[0], p[1], p[2]]).collect();
                }
                17 => x.compression = r.take(1)?[0],
                // A user unit, whose size field some GIMP 3 versions wrote
                // wrong: read its fields instead.
                24 => {
                    r.f32()?;
                    r.u32()?;
                    for _ in 0..if version < 21 { 5 } else { 3 } {
                        r.string()?;
                    }
                    continue;
                }
                _ => {}
            }
            r.at = start.checked_add(size).filter(|&e| e <= d.len()).ok_or("an image property runs past the end")?;
        }
        // 3. The layers: offsets until a zero.
        let mut offsets = Vec::new();
        loop {
            let o = r.offset()?;
            if o == 0 {
                break;
            }
            if offsets.len() >= MAX_LAYERS {
                return Err("too many layers".into());
            }
            offsets.push(o);
        }
        for o in offsets {
            match x.read_layer(o) {
                Ok(l) => x.layers.push(l),
                Err(e) => x.notes.push(format!("a layer couldn't be read ({e})")),
            }
        }
        Ok(x)
    }

    fn read_layer(&self, at: usize) -> Result<XLayer, String> {
        let mut r = R { d: &self.data, at, wide: self.version >= 11 };
        let width = r.u32()?;
        let height = r.u32()?;
        let kind = r.u32()?;
        let name = r.string()?;
        if kind > 5 {
            return Err(format!("layer type {kind}"));
        }
        let mut l = XLayer {
            name,
            width,
            height,
            kind,
            opacity: 1.0,
            visible: true,
            mode: 0,
            overrides: GimpOverrides::default(),
            x: 0,
            y: 0,
            group: false,
            path: Vec::new(),
            hierarchy: 0,
            mask: None,
            apply_mask: true,
            floating: false,
            text: false,
            filters: false,
        };
        // Properties.
        loop {
            let kind = r.u32()?;
            let size = r.u32()? as usize;
            if kind == 0 {
                break;
            }
            let start = r.at;
            let end =
                start.checked_add(size).filter(|&e| e <= r.d.len()).ok_or("a layer property runs past the end")?;
            let mut p = R { d: &r.d[..end], at: start, wide: r.wide };
            match kind {
                5 => l.floating = true,
                6 => l.opacity = (p.u32()?.min(255)) as f32 / 255.0,
                33 => l.opacity = p.f32()?.clamp(0.0, 1.0),
                7 => {
                    let m = p.u32()?;
                    // GIMP reads the old broken overlay as soft light.
                    l.mode = if m == 5 { 19 } else { m };
                }
                8 => l.visible = p.u32()? != 0,
                11 => l.apply_mask = p.u32()? != 0,
                15 => {
                    l.x = p.i32()?;
                    l.y = p.i32()?;
                }
                26 => l.text = true,
                29 | 31 => l.group = true,
                30 => {
                    while p.at + 4 <= end && l.path.len() < 64 {
                        l.path.push(p.u32()?);
                    }
                }
                35 => l.overrides.composite = composite_of(p.i32()?),
                36 => l.overrides.composite_space = space_of(p.i32()?),
                37 => l.overrides.blend_space = space_of(p.i32()?),
                44..=46 => l.filters = true,
                _ => {}
            }
            r.at = end;
        }
        // Pixels: the hierarchy, the mask, and in newer files the filters.
        let hierarchy = r.offset()?;
        let mask = r.offset()?;
        if self.version >= 20 && r.offset().is_ok_and(|o| o != 0) {
            l.filters = true;
        }
        l.hierarchy = hierarchy;
        if mask != 0 {
            let mut m = R { d: &self.data, at: mask, wide: r.wide };
            let (mw, mh) = (m.u32()?, m.u32()?);
            m.string()?;
            loop {
                let kind = m.u32()?;
                let size = m.u32()? as usize;
                if kind == 0 {
                    break;
                }
                m.at = m.at.checked_add(size).filter(|&e| e <= m.d.len()).ok_or("a mask property runs past the end")?;
            }
            l.mask = Some((mw, mh, m.offset()?));
        }
        Ok(l)
    }

    /// The tree for the compositor, bottom first.
    pub fn tree(&self, notes: &mut Vec<String>) -> Vec<Node> {
        // 1. Each layer's parent, from its item path (index of every
        //    ancestor, then its own); old files have no groups.
        #[derive(Default)]
        struct Slot {
            node: Option<Node>,
            children: Vec<usize>,
        }
        let mut slots: Vec<Slot> = Vec::new();
        let mut top: Vec<usize> = Vec::new();
        let mut by_path: std::collections::HashMap<Vec<u32>, usize> = std::collections::HashMap::new();
        let (mut filters, mut floating, mut approximated) = (0, 0, Vec::new());
        for (i, l) in self.layers.iter().enumerate() {
            if l.floating {
                floating += 1;
                continue;
            }
            if l.filters {
                filters += 1;
            }
            let mode = gimp_mode(l.mode);
            if mode.approximate() {
                approximated.push(mode.name());
            }
            let kind = if l.group {
                Kind::Group { children: Vec::new(), isolated: mode != Mode::PassThrough }
            } else if l.width > 0 && l.height > 0 && l.hierarchy != 0 {
                Kind::Raster { id: i, x: l.x, y: l.y }
            } else {
                Kind::Empty
            };
            let mask = l.mask.filter(|_| l.apply_mask).map(|_| Mask { id: i, x: l.x, y: l.y, outside: 0.0 });
            let node = Node {
                name: l.name.clone(),
                visible: l.visible,
                opacity: l.opacity,
                mode,
                gimp: l.overrides,
                clip: false,
                mask,
                kind,
                slot: 0,
            };
            let index = slots.len();
            slots.push(Slot { node: Some(node), children: Vec::new() });
            // Top-level items carry no path: theirs is their place in the
            // top-level list, which their children's paths start with.
            let path = if l.path.is_empty() { vec![top.len() as u32] } else { l.path.clone() };
            let parent = if path.len() > 1 { by_path.get(&path[..path.len() - 1]).copied() } else { None };
            match parent {
                Some(p) => slots[p].children.push(index),
                None => top.push(index),
            }
            by_path.insert(path, index);
        }
        // 2. Build bottom first: the file lists each container top first.
        fn build(slots: &mut [Slot], list: &[usize]) -> Vec<Node> {
            let mut out = Vec::new();
            for &i in list.iter().rev() {
                let children = std::mem::take(&mut slots[i].children);
                let Some(mut node) = slots[i].node.take() else { continue };
                if let Kind::Group { children: c, .. } = &mut node.kind {
                    *c = build(slots, &children);
                }
                out.push(node);
            }
            out
        }
        if filters > 0 {
            notes.push(format!("non-destructive filters on {filters} layers aren't applied"));
        }
        if floating > 0 {
            notes.push("a floating selection isn't shown".into());
        }
        approximated.sort();
        approximated.dedup();
        if !approximated.is_empty() {
            notes.push(format!("layer modes approximated as normal: {}", approximated.join(", ")));
        }
        build(&mut slots, &top)
    }
}

/// The image precision field, which changed meaning twice.
fn precision(version: u32, p: u32) -> Result<(Component, bool), String> {
    use Component::*;
    let v = match version {
        4 => match p {
            0 => (U8, false),
            1 => (U16, false),
            2 => (U32, true),
            3 => (Half, true),
            4 => (Float, true),
            _ => return Err(format!("precision {p}")),
        },
        5 | 6 => match p {
            100 => (U8, true),
            150 => (U8, false),
            200 => (U16, true),
            250 => (U16, false),
            300 => (U32, true),
            350 => (U32, false),
            400 => (Half, true),
            450 => (Half, false),
            500 => (Float, true),
            550 => (Float, false),
            _ => return Err(format!("precision {p}")),
        },
        _ => {
            let comp = match p / 100 {
                1 => U8,
                2 => U16,
                3 => U32,
                5 => Half,
                6 => Float,
                7 => Double,
                _ => return Err(format!("precision {p}")),
            };
            (comp, p.is_multiple_of(100))
        }
    };
    Ok(v)
}

/// A stored space: negative values are GIMP's "auto" (the mode's default).
fn space_of(v: i32) -> Option<Space> {
    match v {
        1 => Some(Space::Linear),
        2 | 4 => Some(Space::Perceptual),
        3 => Some(Space::Lab),
        _ => None,
    }
}

fn composite_of(v: i32) -> Option<Composite> {
    match v {
        1 => Some(Composite::Union),
        2 => Some(Composite::ClipToBackdrop),
        3 => Some(Composite::ClipToLayer),
        4 => Some(Composite::Intersection),
        _ => None,
    }
}

/// GIMP's layer mode numbers.
pub fn gimp_mode(m: u32) -> Mode {
    use Legacy as L;
    match m {
        0 => Mode::Legacy(L::Normal),
        1 => Mode::Dissolve,
        // Modes for painting only: GIMP draws a layer with one as normal.
        2 | 22 | 29 | 62..=64 => Mode::Normal,
        3 => Mode::Legacy(L::Multiply),
        4 => Mode::Legacy(L::Screen),
        5 | 19 => Mode::Legacy(L::SoftLight),
        6 => Mode::Legacy(L::Difference),
        7 => Mode::Legacy(L::Addition),
        8 => Mode::Legacy(L::Subtract),
        9 => Mode::Legacy(L::Darken),
        10 => Mode::Legacy(L::Lighten),
        11 => Mode::Legacy(L::HsvHue),
        12 => Mode::Legacy(L::HsvSaturation),
        13 => Mode::Legacy(L::HslColor),
        14 => Mode::Legacy(L::HsvValue),
        15 => Mode::Legacy(L::Divide),
        16 => Mode::Legacy(L::Dodge),
        17 => Mode::Legacy(L::Burn),
        18 => Mode::Legacy(L::HardLight),
        20 => Mode::Legacy(L::GrainExtract),
        21 => Mode::Legacy(L::GrainMerge),
        23 => Mode::Overlay,
        24 => Mode::LchHue,
        25 => Mode::LchChroma,
        26 => Mode::LchColor,
        27 => Mode::LchLightness,
        28 => Mode::Normal,
        30 => Mode::Multiply,
        31 => Mode::Screen,
        32 => Mode::Difference,
        33 => Mode::Addition,
        34 => Mode::Subtract,
        35 => Mode::Darken,
        36 => Mode::Lighten,
        37 => Mode::HsvHue,
        38 => Mode::HsvSaturation,
        39 => Mode::HslColor,
        40 => Mode::HsvValue,
        41 => Mode::Divide,
        42 => Mode::ColorDodge,
        43 => Mode::ColorBurn,
        44 => Mode::HardLight,
        45 => Mode::SoftLight,
        46 => Mode::GrainExtract,
        47 => Mode::GrainMerge,
        48 => Mode::VividLight,
        49 => Mode::PinLight,
        50 => Mode::LinearLight,
        51 => Mode::HardMix,
        52 => Mode::Exclusion,
        53 => Mode::LinearBurn,
        54 => Mode::LumaDarken,
        55 => Mode::LumaLighten,
        56 => Mode::Luminance,
        57 => Mode::ColorErase,
        58 => Mode::Erase,
        59 => Mode::Merge,
        60 => Mode::Split,
        61 => Mode::PassThrough,
        // GIMP falls back to the legacy normal mode for numbers it doesn't
        // know.
        _ => Mode::Legacy(L::Normal),
    }
}

//endregion

/*┌─────────────────╮
| Pixels          |
└─────────────────╯*/
//region Pixels

impl Xcf {
    /// The pixels of a hierarchy: its first level's tiles, as interleaved
    /// big-endian components (little-endian before version 12).
    fn hierarchy(&self, at: usize, w: u32, h: u32, bpp: usize) -> Result<Vec<u8>, String> {
        let d: &[u8] = &self.data;
        let mut r = R { d, at, wide: self.version >= 11 };
        let (hw, hh, hbpp) = (r.u32()?, r.u32()?, r.u32()? as usize);
        if (hw, hh, hbpp) != (w, h, bpp) {
            return Err(format!("pixel data of {hw} × {hh} × {hbpp} where {w} × {h} × {bpp} was expected"));
        }
        let level = r.offset()?;
        let mut r = R { d, at: level, wide: r.wide };
        let (lw, lh) = (r.u32()? as usize, r.u32()? as usize);
        if (lw, lh) != (w as usize, h as usize) {
            return Err("the level's size differs from the layer's".into());
        }
        let (w, h) = (w as usize, h as usize);
        let total = w.checked_mul(h).and_then(|n| n.checked_mul(bpp)).filter(|&n| n <= 1 << 31).ok_or("too large")?;
        let mut out = vec![0u8; total];
        let (cols, rows) = (w.div_ceil(TILE), h.div_ceil(TILE));
        let mut offset = r.offset()?;
        let max_len = TILE * TILE * bpp * 3 / 2;
        for t in 0..cols * rows {
            if offset == 0 {
                break;
            }
            let next = r.offset()?;
            let len = if next == 0 || next < offset { max_len } else { (next - offset).min(max_len) };
            let src = &d[offset.min(d.len())..offset.saturating_add(len).min(d.len())];
            let (tx, ty) = (t % cols * TILE, t / cols * TILE);
            let (tw, th) = (TILE.min(w - tx), TILE.min(h - ty));
            let tile = match self.compression {
                0 => src.get(..tw * th * bpp).ok_or("a tile is cut short")?.to_vec(),
                1 => unrle(src, tw * th, bpp)?,
                2 => miniz_oxide::inflate::decompress_to_vec_zlib_with_limit(src, tw * th * bpp)
                    .map_err(|_| "a damaged zlib tile".to_string())?,
                c => return Err(format!("compression {c}")),
            };
            if tile.len() < tw * th * bpp {
                return Err("a tile is cut short".into());
            }
            for y in 0..th {
                let dst = ((ty + y) * w + tx) * bpp;
                out[dst..dst + tw * bpp].copy_from_slice(&tile[y * tw * bpp..(y + 1) * tw * bpp]);
            }
            offset = next;
        }
        Ok(out)
    }

    fn sample(&self, bytes: &[u8], i: usize) -> f32 {
        let n = self.component.bytes();
        let b = &bytes[i * n..i * n + n];
        let le = self.version < 12;
        let u16v = || if le { u16::from_le_bytes([b[0], b[1]]) } else { u16::from_be_bytes([b[0], b[1]]) };
        let u32v = || {
            if le { u32::from_le_bytes([b[0], b[1], b[2], b[3]]) } else { u32::from_be_bytes([b[0], b[1], b[2], b[3]]) }
        };
        match self.component {
            Component::U8 => b[0] as f32 / 255.0,
            Component::U16 => u16v() as f32 / 65535.0,
            Component::U32 => (u32v() as f64 / u32::MAX as f64) as f32,
            Component::Half => half_to_f32(u16v()),
            Component::Float => f32::from_bits(u32v()),
            Component::Double => {
                let a: [u8; 8] = b.try_into().unwrap_or([0; 8]);
                (if le { f64::from_le_bytes(a) } else { f64::from_be_bytes(a) }) as f32
            }
        }
    }
}

/// GIMP's tile run-length code, one byte plane after another: a byte of
/// 128 or more starts 256 minus it literal bytes, less a run of it plus one
/// copies; a count of 128 is followed by a 16-bit count.
fn unrle(src: &[u8], pixels: usize, bpp: usize) -> Result<Vec<u8>, String> {
    let mut out = vec![0u8; pixels * bpp];
    let mut i = 0;
    let bad = || "a damaged run-length tile".to_string();
    for plane in 0..bpp {
        let mut n = 0;
        while n < pixels {
            let v = *src.get(i).ok_or_else(bad)? as usize;
            i += 1;
            if v >= 128 {
                let mut len = 256 - v;
                if len == 128 {
                    len = (*src.get(i).ok_or_else(bad)? as usize) << 8 | *src.get(i + 1).ok_or_else(bad)? as usize;
                    i += 2;
                }
                if n + len > pixels {
                    return Err(bad());
                }
                let lit = src.get(i..i + len).ok_or_else(bad)?;
                for (k, &b) in lit.iter().enumerate() {
                    out[(n + k) * bpp + plane] = b;
                }
                i += len;
                n += len;
            } else {
                let mut len = v + 1;
                if len == 128 {
                    len = (*src.get(i).ok_or_else(bad)? as usize) << 8 | *src.get(i + 1).ok_or_else(bad)? as usize;
                    i += 2;
                }
                if n + len > pixels {
                    return Err(bad());
                }
                let b = *src.get(i).ok_or_else(bad)?;
                i += 1;
                for k in 0..len {
                    out[(n + k) * bpp + plane] = b;
                }
                n += len;
            }
        }
    }
    Ok(out)
}

fn half_to_f32(h: u16) -> f32 {
    let sign = if h & 0x8000 != 0 { -1.0 } else { 1.0 };
    let exp = (h >> 10 & 0x1f) as i32;
    let man = (h & 0x3ff) as f32;
    sign * match exp {
        0 => man * 2f32.powi(-24),
        31 => {
            if man == 0.0 {
                f32::INFINITY
            } else {
                f32::NAN
            }
        }
        _ => (1.0 + man / 1024.0) * 2f32.powi(exp - 15),
    }
}

impl Source for Xcf {
    fn layer(&self, id: usize) -> Result<Raster, String> {
        let l = self.layers.get(id).ok_or("no such layer")?;
        let comps = [3, 4, 1, 2, 1, 2][l.kind as usize];
        let indexed = l.kind >= 4;
        let n = if indexed { 1 } else { self.component.bytes() };
        let bpp = if indexed { comps } else { comps * n };
        let bytes = self.hierarchy(l.hierarchy, l.width, l.height, bpp)?;
        let px = l.width as usize * l.height as usize;
        let has_alpha = comps == 2 || comps == 4;
        if indexed {
            let mut out = Vec::with_capacity(px * 4);
            for p in 0..px {
                let c = self.colormap.get(bytes[p * comps] as usize).copied().unwrap_or([0; 3]);
                let a = if has_alpha { bytes[p * comps + 1] } else { 255 };
                out.extend_from_slice(&[c[0], c[1], c[2], a]);
            }
            return Ok(Raster { width: l.width, height: l.height, channels: 4, samples: Samples::Srgb8(out) });
        }
        // Grey replicates into red, green and blue.
        let at = |p: usize, c: usize| -> usize {
            match (comps, c) {
                (1 | 2, 0..=2) => p * comps,
                (2, _) => p * comps + 1,
                (3, 3) => usize::MAX,
                _ => p * comps + c,
            }
        };
        let samples = match self.component {
            Component::U8 => {
                let v: Vec<u8> = (0..px * 4)
                    .map(|i| {
                        let j = at(i / 4, i % 4);
                        if j == usize::MAX || (!has_alpha && i % 4 == 3) { 255 } else { bytes[j] }
                    })
                    .collect();
                if self.linear { Samples::U8(v) } else { Samples::Srgb8(v) }
            }
            Component::U16 => {
                let v: Vec<u16> = (0..px * 4)
                    .map(|i| {
                        let j = at(i / 4, i % 4);
                        if j == usize::MAX || (!has_alpha && i % 4 == 3) {
                            65535
                        } else {
                            (self.sample(&bytes, j) * 65535.0 + 0.5) as u16
                        }
                    })
                    .collect();
                if self.linear { Samples::U16(v) } else { Samples::Srgb16(v) }
            }
            _ => Samples::F32(
                (0..px * 4)
                    .map(|i| {
                        let j = at(i / 4, i % 4);
                        if j == usize::MAX || (!has_alpha && i % 4 == 3) {
                            return 1.0;
                        }
                        let v = self.sample(&bytes, j);
                        if self.linear || i % 4 == 3 { v } else { crate::compose::srgb_decode(v) }
                    })
                    .collect(),
            ),
        };
        Ok(Raster { width: l.width, height: l.height, channels: 4, samples })
    }

    fn mask(&self, id: usize) -> Result<Raster, String> {
        let l = self.layers.get(id).ok_or("no such layer")?;
        let (w, h, at) = l.mask.ok_or("no mask")?;
        // Masks are stored as linear values of the image's component type.
        let bytes = self.hierarchy(at, w, h, self.component.bytes())?;
        let n = w as usize * h as usize;
        let samples = match self.component {
            Component::U8 => Samples::U8(bytes),
            _ => Samples::F32((0..n).map(|i| self.sample(&bytes, i)).collect()),
        };
        Ok(Raster { width: w, height: h, channels: 1, samples })
    }
}

//endregion

#[cfg(test)]
pub mod tests {
    use super::*;

    fn prop(out: &mut Vec<u8>, kind: u32, data: &[u8]) {
        out.extend_from_slice(&kind.to_be_bytes());
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        out.extend_from_slice(data);
    }

    fn string(out: &mut Vec<u8>, s: &str) {
        out.extend_from_slice(&(s.len() as u32 + 1).to_be_bytes());
        out.extend_from_slice(s.as_bytes());
        out.push(0);
    }

    /// Writes an 8-bit perceptual RGBA XCF (version 11, 64-bit offsets)
    /// with RLE tiles: `layers` top first as (name, w, h, x, y, mode,
    /// opacity, RGBA pixels).
    /// (name, width, height, x, y, mode, opacity, RGBA pixels).
    pub type TestLayer<'a> = (&'a str, u32, u32, i32, i32, u32, u32, Vec<[u8; 4]>);

    pub fn write(w: u32, h: u32, layers: &[TestLayer]) -> Vec<u8> {
        let mut f = b"gimp xcf v011\0".to_vec();
        for v in [w, h, 0, 150] {
            f.extend_from_slice(&v.to_be_bytes());
        }
        prop(&mut f, 17, &[1]);
        prop(&mut f, 0, &[]);
        let table = f.len();
        f.extend(vec![0u8; (layers.len() + 2) * 8]);
        let mut offsets = Vec::new();
        for (name, lw, lh, x, y, mode, opacity, px) in layers {
            offsets.push(f.len() as u64);
            for v in [*lw, *lh, 1] {
                f.extend_from_slice(&v.to_be_bytes());
            }
            string(&mut f, name);
            prop(&mut f, 6, &opacity.to_be_bytes());
            prop(&mut f, 7, &mode.to_be_bytes());
            prop(&mut f, 8, &1u32.to_be_bytes());
            let mut off = x.to_be_bytes().to_vec();
            off.extend_from_slice(&y.to_be_bytes());
            prop(&mut f, 15, &off);
            prop(&mut f, 0, &[]);
            let hier_at = f.len() + 16;
            f.extend_from_slice(&(hier_at as u64).to_be_bytes());
            f.extend_from_slice(&0u64.to_be_bytes());
            // Hierarchy, one level, tiles.
            for v in [*lw, *lh, 4] {
                f.extend_from_slice(&v.to_be_bytes());
            }
            let level_at = f.len() + 8 + 8;
            f.extend_from_slice(&(level_at as u64).to_be_bytes());
            f.extend_from_slice(&0u64.to_be_bytes());
            f.extend_from_slice(&lw.to_be_bytes());
            f.extend_from_slice(&lh.to_be_bytes());
            let (cols, rows) = ((*lw as usize).div_ceil(TILE), (*lh as usize).div_ceil(TILE));
            let ntiles = cols * rows;
            let tile_table = f.len();
            f.extend(vec![0u8; (ntiles + 1) * 8]);
            for t in 0..ntiles {
                let at = f.len() as u64;
                f[tile_table + t * 8..tile_table + t * 8 + 8].copy_from_slice(&at.to_be_bytes());
                let (tx, ty) = (t % cols * TILE, t / cols * TILE);
                let (tw, th) = (TILE.min(*lw as usize - tx), TILE.min(*lh as usize - ty));
                for c in 0..4usize {
                    let vals: Vec<u8> = (0..th)
                        .flat_map(|yy| (0..tw).map(move |xx| (yy, xx)))
                        .map(|(yy, xx)| px[(ty + yy) * *lw as usize + tx + xx].get(c).copied().unwrap_or(0))
                        .collect();
                    for chunk in vals.chunks(127) {
                        f.push((256 - chunk.len()) as u8);
                        f.extend_from_slice(chunk);
                    }
                }
            }
        }
        for (i, o) in offsets.iter().enumerate() {
            f[table + i * 8..table + i * 8 + 8].copy_from_slice(&o.to_be_bytes());
        }
        f
    }

    #[test]
    fn layers_tiles_and_tree() {
        let red = vec![[255, 0, 0, 255]; 100 * 70];
        let blue = vec![[0, 0, 255, 255]; 10 * 10];
        let f = write(100, 70, &[("top", 10, 10, 5, 5, 30, 255, blue), ("bottom", 100, 70, 0, 0, 28, 255, red)]);
        let x = Xcf::parse(Arc::new(f)).unwrap();
        assert_eq!((x.version, x.width, x.height, x.layers.len()), (11, 100, 70, 2));
        assert!(!x.linear && x.component == Component::U8);
        let l = x.layer(1).unwrap();
        assert_eq!((l.width, l.height), (100, 70));
        let Samples::Srgb8(v) = &l.samples else { panic!() };
        assert_eq!(&v[(69 * 100 + 99) * 4..], &[255, 0, 0, 255]);
        let tree = x.tree(&mut Vec::new());
        assert_eq!(tree.iter().map(|n| n.name.as_str()).collect::<Vec<_>>(), ["bottom", "top"]);
        assert_eq!(tree[1].mode, Mode::Multiply);
    }

    #[test]
    fn damaged_files_never_panic() {
        let px = vec![[9, 8, 7, 200]; 70 * 70];
        let f = write(70, 70, &[("a", 70, 70, 0, 0, 28, 128, px.clone()), ("b", 70, 70, 3, 3, 0, 255, px)]);
        for cut in (0..f.len()).step_by(11) {
            if let Ok(x) = Xcf::parse(Arc::new(f[..cut].to_vec())) {
                for i in 0..x.layers.len() {
                    let _ = x.layer(i);
                }
            }
        }
        let mut seed = 99u32;
        for _ in 0..400 {
            let mut bad = f.clone();
            for _ in 0..3 {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                let i = (seed >> 8) as usize % bad.len();
                bad[i] = (seed >> 2) as u8;
            }
            if let Ok(x) = Xcf::parse(Arc::new(bad)) {
                for i in 0..x.layers.len().min(3) {
                    let _ = x.layer(i);
                    let _ = x.mask(i);
                }
                let _ = x.tree(&mut Vec::new());
            }
        }
        assert_eq!(half_to_f32(0x3c00), 1.0);
        assert_eq!(half_to_f32(0xc000), -2.0);
    }
}
