//! Aseprite sprites (`.aseprite`, `.ase`), read from the published file
//! format (aseprite/docs/ase-file-specs.md): RGBA, greyscale and indexed
//! colour, layers and groups with their blend modes and opacity, linked
//! cels, cel z-index, tilemap layers and their tilesets, palettes and tags.
//!
//! Cel pixels stay compressed in the file's bytes until a frame is
//! composed, so a thumbnail of a long animation decodes only its first
//! frame.  Every size in the file is checked before anything is allocated.

use crate::blend::{Blend, Px};
use crate::pixels::MAX_PIXELS;
use slerp_scene::{Canvas, CanvasLayer, FrameClip, Image8, Info, Limits, LoadError, PlayDirection};
use std::ops::Range;

/*┌─────────────────╮
| The sprite      |
└─────────────────╯*/
//region The sprite

const HEADER_MAGIC: u16 = 0xA5E0;
const FRAME_MAGIC: u16 = 0xF1FA;

/// Recognises the file by its header: the magic number at byte 4 and, when
/// the file holds frames, the first frame's magic number at byte 132.
pub fn sniff(head: &[u8]) -> bool {
    let u16_at = |at: usize| head.get(at..at + 2).map(|b| u16::from_le_bytes([b[0], b[1]]));
    if u16_at(4) != Some(HEADER_MAGIC) {
        return false;
    }
    match u16_at(6) {
        Some(0) => true,
        Some(_) => u16_at(132).is_none_or(|m| m == FRAME_MAGIC),
        None => false,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Depth {
    Rgba,
    Grey,
    /// One palette index per pixel; `transparent` is see-through on every
    /// layer but the background.
    Indexed {
        transparent: u8,
    },
}

impl Depth {
    fn bytes(self) -> usize {
        match self {
            Depth::Rgba => 4,
            Depth::Grey => 2,
            Depth::Indexed { .. } => 1,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LayerKind {
    Image,
    Group,
    Tilemap { tileset: u32 },
}

#[derive(Clone, Debug)]
struct Layer {
    kind: LayerKind,
    /// How many groups it sits inside.
    depth: u32,
    visible: bool,
    background: bool,
    reference: bool,
    blend: Blend,
    opacity: u8,
    parent: Option<usize>,
}

/// Where a cel's pixels are.
#[derive(Clone, Debug)]
enum CelData {
    /// `width` x `height` pixels, zlib-compressed or not.
    Image { width: u32, height: u32, compressed: bool, bytes: Range<usize> },
    /// The same layer's cel in another frame.
    Link(usize),
    /// A grid of tile references, zlib-compressed.
    Tiles { width: u32, height: u32, bits: u16, id_mask: u32, flips: [u32; 3], bytes: Range<usize> },
}

#[derive(Clone, Debug)]
struct Cel {
    layer: usize,
    x: i32,
    y: i32,
    opacity: u8,
    z: i32,
    data: CelData,
}

#[derive(Clone, Debug)]
struct Tileset {
    id: u32,
    count: u32,
    tile_width: u32,
    tile_height: u32,
    /// The tiles stacked in one column, zlib-compressed, when the file
    /// holds them (rather than naming another file).
    image: Option<Range<usize>>,
}

pub struct Sprite {
    info: Info,
    width: u32,
    height: u32,
    depth: Depth,
    palette: Vec<Px>,
    layers: Vec<Layer>,
    listed: Vec<CanvasLayer>,
    /// The layers inside each group, and at the end the top level's.
    children: Vec<Vec<usize>>,
    /// Cels of each frame, sorted by layer.
    frames: Vec<Vec<Cel>>,
    durations: Vec<f32>,
    clips: Vec<FrameClip>,
    tilesets: Vec<Tileset>,
    /// Layer opacity applies (the header says so; very old files left it
    /// unset and used 255).
    layer_opacity: bool,
    /// Groups blend their contents as one layer, with their own mode and
    /// opacity (Aseprite 1.3).
    group_blending: bool,
    bytes: Vec<u8>,
}

//endregion

/*┌─────────────────╮
| Reading         |
└─────────────────╯*/
//region Reading

struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
    end: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<Range<usize>, LoadError> {
        let end = self.pos.checked_add(n).filter(|&e| e <= self.end).ok_or_else(truncated)?;
        let r = self.pos..end;
        self.pos = end;
        Ok(r)
    }

    fn bytes(&mut self, n: usize) -> Result<&'a [u8], LoadError> {
        let r = self.take(n)?;
        Ok(&self.data[r])
    }

    fn u8(&mut self) -> Result<u8, LoadError> {
        Ok(self.bytes(1)?[0])
    }

    fn u16(&mut self) -> Result<u16, LoadError> {
        let b = self.bytes(2)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }

    fn i16(&mut self) -> Result<i16, LoadError> {
        Ok(self.u16()? as i16)
    }

    fn u32(&mut self) -> Result<u32, LoadError> {
        let b = self.bytes(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn string(&mut self) -> Result<String, LoadError> {
        let n = self.u16()? as usize;
        Ok(String::from_utf8_lossy(self.bytes(n)?).into_owned())
    }

    /// The rest of the current chunk.
    fn rest(&mut self) -> Range<usize> {
        let r = self.pos..self.end;
        self.pos = self.end;
        r
    }
}

fn truncated() -> LoadError {
    LoadError::invalid("Aseprite file is truncated")
}

/// Reads a sprite.  Cel pixels are only located here; they are decoded
/// when a frame is composed.
pub fn load(bytes: &[u8], limits: &Limits) -> Result<Sprite, LoadError> {
    if bytes.len() as u64 > limits.max_file_bytes {
        return Err(LoadError::TooLarge(format!("{} bytes", bytes.len())));
    }
    let mut r = Reader { data: bytes, pos: 0, end: bytes.len() };

    // 1. The 128-byte header.
    let _file_size = r.u32()?;
    if r.u16()? != HEADER_MAGIC {
        return Err(LoadError::NotMine);
    }
    let frame_count = r.u16()? as usize;
    let width = r.u16()? as u32;
    let height = r.u16()? as u32;
    let depth_bits = r.u16()?;
    let flags = r.u32()?;
    let speed = r.u16()?;
    r.take(8)?;
    let transparent = r.u8()?;
    r.take(3)?;
    let _colors = r.u16()?;
    let (pixel_w, pixel_h) = (r.u8()?, r.u8()?);
    let (_grid_x, _grid_y, grid_w, grid_h) = (r.i16()?, r.i16()?, r.u16()?, r.u16()?);
    r.take(84)?;
    let depth = match depth_bits {
        32 => Depth::Rgba,
        16 => Depth::Grey,
        8 => Depth::Indexed { transparent },
        d => return Err(LoadError::Unsupported(format!("Aseprite colour depth {d}"))),
    };
    if width == 0 || height == 0 {
        return Err(LoadError::invalid("Aseprite sprite has no pixels"));
    }

    let mut sprite = Sprite {
        info: Info::default(),
        width,
        height,
        depth,
        palette: (0..256).map(|i| [i as u8, i as u8, i as u8, 255]).collect(),
        layers: Vec::new(),
        listed: Vec::new(),
        children: Vec::new(),
        frames: Vec::new(),
        durations: Vec::new(),
        clips: Vec::new(),
        tilesets: Vec::new(),
        layer_opacity: flags & 1 != 0,
        group_blending: flags & 2 != 0,
        bytes: Vec::new(),
    };
    let mut new_palette = false;
    let mut warnings = Vec::new();
    // The latest group at each level of nesting, for finding parents.
    let mut groups = Vec::new();

    // 2. Frames and their chunks.
    for frame in 0..frame_count {
        let start = r.pos;
        let header = (|| -> Result<_, LoadError> {
            let size = r.u32()? as usize;
            let magic = r.u16()?;
            let old_chunks = r.u16()? as u32;
            let duration = r.u16()?;
            r.take(2)?;
            Ok((size, magic, old_chunks, duration, r.u32()?))
        })();
        let (size, _, old_chunks, duration, new_chunks) = match header {
            Ok(h) if h.1 == FRAME_MAGIC && h.0 >= 16 => h,
            Ok(_) => {
                warnings.push(format!("frame {} is damaged; later frames are left out", frame + 1));
                break;
            }
            Err(_) => {
                warnings.push(format!("the file ends after {frame} of {frame_count} frames"));
                break;
            }
        };
        let end = start.saturating_add(size).min(bytes.len());
        let chunks = if new_chunks == 0 { old_chunks } else { new_chunks };
        let ms = if duration > 0 {
            duration
        } else if speed > 0 {
            speed
        } else {
            100
        };
        sprite.durations.push(ms as f32 / 1000.0);
        let mut cels = Vec::new();
        r.end = end;
        for _ in 0..chunks {
            if r.pos + 6 > end {
                break;
            }
            let chunk_start = r.pos;
            let chunk_size = r.u32()? as usize;
            let kind = r.u16()?;
            let chunk_end = chunk_start.saturating_add(chunk_size).min(end);
            if chunk_size < 6 {
                break;
            }
            let mut c = Reader { data: bytes, pos: r.pos, end: chunk_end };
            let result = match kind {
                0x2004 if frame == 0 => read_layer(&mut c, &mut sprite, &mut groups),
                0x2005 => read_cel(&mut c, sprite.layers.len()).map(|cel| cels.extend(cel)),
                0x2018 => read_tags(&mut c, &mut sprite.clips),
                0x2019 => read_palette(&mut c, &mut sprite.palette).map(|_| new_palette = true),
                0x0004 | 0x0011 if !new_palette => read_old_palette(&mut c, &mut sprite.palette, kind == 0x0011),
                0x2023 => read_tileset(&mut c, &mut sprite.tilesets),
                _ => Ok(()),
            };
            if let Err(e) = result {
                warnings.push(format!("frame {}: chunk {kind:#06x} skipped: {e}", frame + 1));
            }
            r.pos = chunk_end;
        }
        cels.sort_by_key(|c| c.layer);
        sprite.frames.push(cels);
        r.pos = end;
        r.end = bytes.len();
    }
    if sprite.frames.is_empty() {
        return Err(LoadError::invalid("Aseprite file holds no frames"));
    }

    // 3. Tags that name frames the file has, and what the inspector shows.
    let n = sprite.frames.len();
    sprite.clips.retain(|c| c.from <= c.to && c.from < n);
    for c in &mut sprite.clips {
        c.to = c.to.min(n - 1);
    }
    let names = std::mem::take(&mut sprite.listed);
    sprite.listed = sprite
        .layers
        .iter()
        .zip(names)
        .map(|(l, named)| CanvasLayer {
            name: named.name,
            visible: l.visible && !l.reference,
            depth: l.depth,
            group: l.kind == LayerKind::Group,
            note: layer_note(l, sprite.layer_opacity, sprite.group_blending),
        })
        .collect();
    sprite.children = vec![Vec::new(); sprite.layers.len() + 1];
    for (i, l) in sprite.layers.iter().enumerate() {
        sprite.children[l.parent.unwrap_or(sprite.layers.len())].push(i);
    }
    sprite.info = Info {
        format: format!(
            "Aseprite sprite ({})",
            match depth {
                Depth::Rgba => "RGBA",
                Depth::Grey => "greyscale",
                Depth::Indexed { .. } => "indexed",
            }
        ),
        generator: None,
        warnings,
        details: vec![
            ("Canvas".into(), format!("{width} × {height}")),
            ("Frames".into(), n.to_string()),
            ("Layers".into(), sprite.layers.len().to_string()),
            ("Tags".into(), sprite.clips.len().to_string()),
        ],
        neighbours: Vec::new(),
    };
    if pixel_w > 0 && pixel_h > 0 && pixel_w != pixel_h {
        sprite.info.details.push(("Pixel ratio".into(), format!("{pixel_w}:{pixel_h}")));
    }
    if grid_w > 0 && grid_h > 0 && (grid_w, grid_h) != (16, 16) {
        sprite.info.details.push(("Grid".into(), format!("{grid_w} × {grid_h}")));
    }
    if !sprite.tilesets.is_empty() {
        sprite.info.details.push(("Tilesets".into(), sprite.tilesets.len().to_string()));
    }
    sprite.bytes = bytes.to_vec();
    Ok(sprite)
}

/// Groups nest at most this deep; deeper layers join the deepest group,
/// so composing never recurses without bound.
const MAX_NESTING: usize = 32;

fn layer_note(l: &Layer, opacity_valid: bool, group_blending: bool) -> String {
    // Groups blend and fade only in files that say so; elsewhere their
    // mode and opacity fields mean nothing.
    let blends = l.kind != LayerKind::Group || group_blending;
    let mut parts = Vec::new();
    match l.kind {
        LayerKind::Group => parts.push("group".to_string()),
        LayerKind::Tilemap { .. } => parts.push("tilemap".to_string()),
        LayerKind::Image => {}
    }
    if l.reference {
        parts.push("reference".into());
    }
    if l.background {
        parts.push("background".into());
    }
    if blends && l.blend != Blend::Normal {
        parts.push(l.blend.name().into());
    }
    if blends && opacity_valid && l.opacity < 255 {
        parts.push(format!("{:.0} %", l.opacity as f32 / 2.55));
    }
    parts.join(", ")
}

fn read_layer(c: &mut Reader, sprite: &mut Sprite, groups: &mut Vec<usize>) -> Result<(), LoadError> {
    let flags = c.u16()?;
    let kind = c.u16()?;
    let child_level = c.u16()? as usize;
    c.take(4)?;
    let blend = Blend::from_aseprite(c.u16()?);
    let opacity = c.u8()?;
    c.take(3)?;
    let name = c.string()?;
    let kind = match kind {
        1 => LayerKind::Group,
        2 => LayerKind::Tilemap { tileset: c.u32()? },
        _ => LayerKind::Image,
    };
    // The parent is the latest group one level up.
    let level = child_level.min(MAX_NESTING);
    let parent = if level == 0 { None } else { groups.get(level - 1).or(groups.last()).copied() };
    let depth = parent.map_or(0, |p| sprite.layers[p].depth + 1);
    if kind == LayerKind::Group {
        groups.truncate(depth as usize);
        groups.push(sprite.layers.len());
    }
    sprite.layers.push(Layer {
        kind,
        depth,
        visible: flags & 1 != 0,
        background: flags & 8 != 0,
        reference: flags & 64 != 0,
        blend,
        opacity,
        parent,
    });
    sprite.listed.push(CanvasLayer { name, ..Default::default() });
    Ok(())
}

fn read_cel(c: &mut Reader, layers: usize) -> Result<Option<Cel>, LoadError> {
    let layer = c.u16()? as usize;
    let x = c.i16()? as i32;
    let y = c.i16()? as i32;
    let opacity = c.u8()?;
    let kind = c.u16()?;
    let z = c.i16()? as i32;
    c.take(5)?;
    if layer >= layers {
        return Err(LoadError::invalid(format!("cel on layer {layer}, which doesn't exist")));
    }
    let data = match kind {
        0 | 2 => {
            let width = c.u16()? as u32;
            let height = c.u16()? as u32;
            CelData::Image { width, height, compressed: kind == 2, bytes: c.rest() }
        }
        1 => CelData::Link(c.u16()? as usize),
        3 => {
            let width = c.u16()? as u32;
            let height = c.u16()? as u32;
            let bits = c.u16()?;
            let id_mask = c.u32()?;
            let flips = [c.u32()?, c.u32()?, c.u32()?];
            c.take(10)?;
            CelData::Tiles { width, height, bits, id_mask, flips, bytes: c.rest() }
        }
        k => return Err(LoadError::Unsupported(format!("cel type {k}"))),
    };
    Ok(Some(Cel { layer, x, y, opacity, z, data }))
}

fn read_tags(c: &mut Reader, clips: &mut Vec<FrameClip>) -> Result<(), LoadError> {
    let count = c.u16()?;
    c.take(8)?;
    for _ in 0..count {
        let from = c.u16()? as usize;
        let to = c.u16()? as usize;
        let direction = match c.u8()? {
            1 => PlayDirection::Reverse,
            2 => PlayDirection::PingPong,
            3 => PlayDirection::PingPongReverse,
            _ => PlayDirection::Forward,
        };
        let repeat = c.u16()? as u32;
        c.take(6 + 3 + 1)?;
        let name = c.string()?;
        clips.push(FrameClip { name, from, to, direction, repeat });
    }
    Ok(())
}

fn read_palette(c: &mut Reader, palette: &mut [Px]) -> Result<(), LoadError> {
    let _size = c.u32()?;
    let first = c.u32()?;
    let last = c.u32()?;
    c.take(8)?;
    if last < first {
        return Ok(());
    }
    for i in first..=last {
        let flags = c.u16()?;
        let p = [c.u8()?, c.u8()?, c.u8()?, c.u8()?];
        if flags & 1 != 0 {
            c.string()?;
        }
        if let Some(slot) = palette.get_mut(i as usize) {
            *slot = p;
        }
    }
    Ok(())
}

/// The palette chunks of files from before Aseprite 1.2, with 8-bit or
/// 6-bit (`six`) channels.
fn read_old_palette(c: &mut Reader, palette: &mut [Px], six: bool) -> Result<(), LoadError> {
    let packets = c.u16()?;
    let mut index = 0usize;
    for _ in 0..packets {
        index += c.u8()? as usize;
        let n = match c.u8()? {
            0 => 256,
            n => n as usize,
        };
        for _ in 0..n {
            let mut rgb = [c.u8()?, c.u8()?, c.u8()?];
            if six {
                rgb = rgb.map(|v| ((v.min(63) as u32 * 255 + 31) / 63) as u8);
            }
            if let Some(slot) = palette.get_mut(index) {
                *slot = [rgb[0], rgb[1], rgb[2], 255];
            }
            index += 1;
        }
    }
    Ok(())
}

fn read_tileset(c: &mut Reader, tilesets: &mut Vec<Tileset>) -> Result<(), LoadError> {
    let id = c.u32()?;
    let flags = c.u32()?;
    let count = c.u32()?;
    let tile_width = c.u16()? as u32;
    let tile_height = c.u16()? as u32;
    let _base = c.i16()?;
    c.take(14)?;
    let _name = c.string()?;
    if flags & 1 != 0 {
        c.take(8)?;
    }
    let image = if flags & 2 != 0 {
        let len = c.u32()? as usize;
        Some(c.take(len)?)
    } else {
        None
    };
    tilesets.push(Tileset { id, count, tile_width, tile_height, image });
    Ok(())
}

//endregion

/*┌─────────────────╮
| Composing       |
└─────────────────╯*/
//region Composing

/// Decoded pixels of one cel or tileset in the sprite's colour mode.
struct Decoded {
    width: u32,
    height: u32,
    data: Vec<u8>,
}

impl Sprite {
    /// The cel of `layer` in `frame`, following links to the cel that holds
    /// the pixels.  The returned cel keeps its own position and opacity.
    fn cel(&self, frame: usize, layer: usize) -> Option<(&Cel, &CelData)> {
        let find = |f: usize| {
            let cels = self.frames.get(f)?;
            cels.get(cels.partition_point(|c| c.layer < layer)).filter(|c| c.layer == layer)
        };
        let own = find(frame)?;
        let mut data = &own.data;
        for _ in 0..8 {
            match data {
                CelData::Link(f) => data = &find(*f)?.data,
                _ => return Some((own, data)),
            }
        }
        None
    }

    /// Inflates `bytes` (or copies them) into exactly `len` bytes, or
    /// `None` when the file holds less: a damaged cel is left out rather
    /// than padded, so a corrupt size can't make Slerp fill gigabytes.
    fn inflate(&self, bytes: &Range<usize>, compressed: bool, len: usize) -> Option<Vec<u8>> {
        let src = self.bytes.get(bytes.clone()).unwrap_or(&[]);
        let out = if compressed {
            match miniz_oxide::inflate::decompress_to_vec_zlib_with_limit(src, len) {
                Ok(v) => v,
                Err(e) => e.output,
            }
        } else {
            src[..src.len().min(len)].to_vec()
        };
        (out.len() == len).then_some(out)
    }

    fn decode_image(&self, width: u32, height: u32, bytes: &Range<usize>, compressed: bool) -> Option<Decoded> {
        let pixels = width as u64 * height as u64;
        if pixels == 0 || pixels > MAX_PIXELS {
            return None;
        }
        let data = self.inflate(bytes, compressed, pixels as usize * self.depth.bytes())?;
        Some(Decoded { width, height, data })
    }

    /// The pixel at `i` of decoded data as RGBA.
    #[inline]
    fn color(&self, data: &[u8], i: usize, background: bool) -> Px {
        match self.depth {
            Depth::Rgba => [data[i * 4], data[i * 4 + 1], data[i * 4 + 2], data[i * 4 + 3]],
            Depth::Grey => [data[i * 2], data[i * 2], data[i * 2], data[i * 2 + 1]],
            Depth::Indexed { transparent } => {
                let index = data[i];
                if index == transparent && !background { [0, 0, 0, 0] } else { self.palette[index as usize] }
            }
        }
    }

    /// Whether each layer shows, given the caller's choices and every
    /// group above it.
    fn shown(&self, choices: &[bool]) -> Vec<bool> {
        let mut shown = Vec::with_capacity(self.layers.len());
        for (i, l) in self.layers.iter().enumerate() {
            let own = choices.get(i).copied().unwrap_or(l.visible && !l.reference);
            let parent = l.parent.is_none_or(|p| p < i && shown[p]);
            shown.push(own && parent);
        }
        shown
    }

    /// Composes the layers whose parent is `parent` into `target`, which
    /// covers the canvas at `scale`.
    fn compose_into(&self, target: &mut Target, frame: usize, parent: Option<usize>, shown: &[bool]) {
        // 1. This level's layers, ordered by layer index plus each cel's
        //    z-index; on a tie the higher z-index goes on top.
        let level = &self.children[parent.unwrap_or(self.layers.len())];
        let mut items: Vec<(i64, i32, usize)> = level
            .iter()
            .copied()
            .filter(|&i| shown[i])
            .map(|i| {
                let z = self.cel(frame, i).map_or(0, |(c, _)| c.z);
                (i as i64 + z as i64, z, i)
            })
            .collect();
        items.sort();
        // 2. Draw them.
        for (_, _, i) in items {
            let layer = &self.layers[i];
            let layer_opacity = if self.layer_opacity { layer.opacity } else { 255 };
            match layer.kind {
                LayerKind::Group if self.group_blending && (layer.blend != Blend::Normal || layer_opacity < 255) => {
                    let mut inner = Target::new(target.width, target.height, target.scale);
                    self.compose_into(&mut inner, frame, Some(i), shown);
                    target.blend_target(&inner, layer.blend, layer_opacity);
                }
                LayerKind::Group => self.compose_into(target, frame, Some(i), shown),
                LayerKind::Image | LayerKind::Tilemap { .. } => {
                    let Some((cel, data)) = self.cel(frame, i) else { continue };
                    let opacity = crate::blend::mul_un8(cel.opacity as i32, layer_opacity as i32) as u8;
                    self.draw_cel(target, cel, data, layer, opacity);
                }
            }
        }
    }

    fn draw_cel(&self, target: &mut Target, cel: &Cel, data: &CelData, layer: &Layer, opacity: u8) {
        match data {
            CelData::Image { width, height, compressed, bytes } => {
                let Some(img) = self.decode_image(*width, *height, bytes, *compressed) else { return };
                target.blend_fn(cel.x, cel.y, img.width, img.height, layer.blend, opacity, |x, y| {
                    self.color(&img.data, (y * img.width + x) as usize, layer.background)
                });
            }
            CelData::Tiles { width, height, bits, id_mask, flips, bytes } => {
                let LayerKind::Tilemap { tileset } = layer.kind else { return };
                let Some(set) = self.tilesets.iter().find(|t| t.id == tileset) else { return };
                let (tw, th) = (set.tile_width, set.tile_height);
                let Some(image) = &set.image else { return };
                let Some(tiles) = self.decode_image(tw, th.saturating_mul(set.count), image, true) else { return };
                if *bits != 32 || tw == 0 || th == 0 || *width as u64 * *height as u64 > MAX_PIXELS {
                    return;
                }
                let Some(refs) = self.inflate(bytes, true, *width as usize * *height as usize * 4) else { return };
                let (pw, ph) = (width.saturating_mul(tw), height.saturating_mul(th));
                target.blend_fn(cel.x, cel.y, pw, ph, layer.blend, opacity, |x, y| {
                    let cell = ((y / th) * width + x / tw) as usize * 4;
                    let value = u32::from_le_bytes([refs[cell], refs[cell + 1], refs[cell + 2], refs[cell + 3]]);
                    let id = value & id_mask;
                    if id == 0 || id >= set.count {
                        return [0, 0, 0, 0];
                    }
                    let (u, v) = flip(x % tw, y % th, tw, th, value, flips);
                    if u >= tw || v >= th {
                        return [0, 0, 0, 0];
                    }
                    let i = ((id * th + v) * tw + u) as usize;
                    self.color(&tiles.data, i, false)
                });
            }
            CelData::Link(_) => {}
        }
    }
}

/// Where a pixel of a flipped tile comes from: the flags flip diagonally
/// (swapping x and y) first, then horizontally, then vertically.
fn flip(x: u32, y: u32, w: u32, h: u32, value: u32, [fx, fy, fd]: &[u32; 3]) -> (u32, u32) {
    let (mut u, mut v) = (x, y);
    if value & fy != 0 && *fy != 0 {
        v = h - 1 - v;
    }
    if value & fx != 0 && *fx != 0 {
        u = w - 1 - u;
    }
    if value & fd != 0 && *fd != 0 {
        (u, v) = (v, u);
    }
    (u, v)
}

/// A picture of the canvas at a scale, being composed.
struct Target {
    width: u32,
    height: u32,
    scale: f32,
    pixels: Vec<u8>,
    /// The canvas column and row each target column and row shows.
    xs: Vec<i32>,
    ys: Vec<i32>,
}

impl Target {
    fn new(width: u32, height: u32, scale: f32) -> Target {
        let at = |i: u32| ((i as f32 + 0.5) / scale).floor() as i32;
        Target {
            width,
            height,
            scale,
            pixels: vec![0; width as usize * height as usize * 4],
            xs: (0..width).map(at).collect(),
            ys: (0..height).map(at).collect(),
        }
    }

    /// Blends a `w` x `h` source placed at `x0`, `y0` on the canvas, whose
    /// pixels `sample` gives.
    #[allow(clippy::too_many_arguments)]
    fn blend_fn(
        &mut self,
        x0: i32,
        y0: i32,
        w: u32,
        h: u32,
        blend: Blend,
        opacity: u8,
        sample: impl Fn(u32, u32) -> Px,
    ) {
        let (x1, y1) = (x0 as i64 + w as i64, y0 as i64 + h as i64);
        let cols: Vec<(usize, u32)> = (0..self.width as usize)
            .filter(|&tx| (x0 as i64..x1).contains(&(self.xs[tx] as i64)))
            .map(|tx| (tx, (self.xs[tx] - x0) as u32))
            .collect();
        for ty in 0..self.height as usize {
            let cy = self.ys[ty] as i64;
            if cy < y0 as i64 || cy >= y1 {
                continue;
            }
            let sy = (cy - y0 as i64) as u32;
            let row = ty * self.width as usize;
            for &(tx, sx) in &cols {
                let s = sample(sx, sy);
                if s[3] == 0 {
                    continue;
                }
                let i = (row + tx) * 4;
                let b = [self.pixels[i], self.pixels[i + 1], self.pixels[i + 2], self.pixels[i + 3]];
                self.pixels[i..i + 4].copy_from_slice(&blend.apply(b, s, opacity));
            }
        }
    }

    /// Blends another target of the same size, as a group does.
    fn blend_target(&mut self, src: &Target, blend: Blend, opacity: u8) {
        for (d, s) in self.pixels.chunks_exact_mut(4).zip(src.pixels.chunks_exact(4)) {
            if s[3] == 0 {
                continue;
            }
            let out = blend.apply([d[0], d[1], d[2], d[3]], [s[0], s[1], s[2], s[3]], opacity);
            d.copy_from_slice(&out);
        }
    }
}

impl Canvas for Sprite {
    fn info(&self) -> &Info {
        &self.info
    }

    fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    fn layers(&self) -> &[CanvasLayer] {
        &self.listed
    }

    fn frames(&self) -> &[f32] {
        &self.durations
    }

    fn clips(&self) -> &[FrameClip] {
        &self.clips
    }

    fn compose(&self, frame: usize, shown: &[bool], scale: f32) -> Image8 {
        let (w, h) = (self.width as u64, self.height as u64);
        let fits = (MAX_PIXELS as f64 / (w * h) as f64).sqrt() as f32;
        let scale = if scale.is_finite() && scale > 0.0 { scale.min(1.0).min(fits) } else { 1.0f32.min(fits) };
        let tw = ((w as f32 * scale).round() as u32).max(1);
        let th = ((h as f32 * scale).round() as u32).max(1);
        let mut target = Target::new(tw, th, scale);
        let frame = frame.min(self.frames.len() - 1);
        self.compose_into(&mut target, frame, None, &self.shown(shown));
        Image8 { width: tw, height: th, pixels: target.pixels }
    }
}

//endregion
