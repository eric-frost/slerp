//! Photoshop documents (`.psd`, and `.psb` with its 8-byte lengths): the
//! composite picture Photoshop stores after the layers (the "Maximize
//! Compatibility" image), and the layers themselves with their groups, blend
//! modes, opacity, masks and clipping, which [`crate::compose`] flattens when
//! the stored composite is missing or blank, or when the viewer hides a
//! layer.  Every colour mode is read: bitmap, greyscale, indexed, RGB, CMYK
//! (converted without a profile), Lab, duotone (shown as its grey channel)
//! and multichannel (shown as inks), at 1, 8, 16 and 32 bits, raw, PackBits,
//! zip and zip with prediction.
//!
//! The file is kept in memory and layers are decoded when drawn.  Layer
//! styles, adjustment and fill layers, and smart-object transforms aren't
//! applied; the loader says so.

use crate::compose::{Kind, Mask, Mode, Node, Raster, Samples, Source};
use crate::psd_vector::{VectorMask, parse_solid_color, parse_vector_mask};
use std::sync::Arc;

/// Layers and channels a document may have.
const MAX_LAYERS: usize = 1 << 16;
const MAX_CHANNELS: usize = 256;
/// The longest side Photoshop allows in a PSB.
const MAX_SIDE: u32 = 300_000;

/*┌─────────────────╮
| Reading         |
└─────────────────╯*/
//region Reading

struct R<'a> {
    d: &'a [u8],
    at: usize,
}

impl<'a> R<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], String> {
        let end = self.at.checked_add(n).filter(|&e| e <= self.d.len()).ok_or("the file is cut short")?;
        let s = &self.d[self.at..end];
        self.at = end;
        Ok(s)
    }
    fn u8(&mut self) -> Result<u8, String> {
        Ok(self.take(1)?[0])
    }
    fn u16(&mut self) -> Result<u16, String> {
        let b = self.take(2)?;
        Ok(u16::from_be_bytes([b[0], b[1]]))
    }
    fn i16(&mut self) -> Result<i16, String> {
        Ok(self.u16()? as i16)
    }
    fn u32(&mut self) -> Result<u32, String> {
        let b = self.take(4)?;
        Ok(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }
    fn i32(&mut self) -> Result<i32, String> {
        Ok(self.u32()? as i32)
    }
    fn u64(&mut self) -> Result<u64, String> {
        let b = self.take(8)?;
        Ok(u64::from_be_bytes(b.try_into().unwrap_or([0; 8])))
    }
    /// A section length: 4 bytes, or 8 in a PSB where `big`.
    fn len(&mut self, big: bool) -> Result<usize, String> {
        let n = if big { self.u64()? } else { self.u32()? as u64 };
        usize::try_from(n).map_err(|_| "a length is too large".to_string())
    }
    fn skip(&mut self, n: usize) -> Result<(), String> {
        self.take(n).map(|_| ())
    }
}

//endregion

/*┌─────────────────╮
| Structure       |
└─────────────────╯*/
//region Structure

pub struct Psd {
    data: Arc<Vec<u8>>,
    pub psb: bool,
    pub width: u32,
    pub height: u32,
    pub channels: u16,
    pub depth: u16,
    pub mode: u16,
    palette: Vec<[u8; 3]>,
    transparent_index: Option<u8>,
    /// Offset and length of the JPEG thumbnail, and whether it is BGR.
    thumbnail: Option<(usize, usize, bool)>,
    /// Resource 1057's flag: false when the stored composite is a stand-in.
    pub has_real_merged: Option<bool>,
    pub writer: Option<String>,
    pub icc: bool,
    pub layers: Vec<Layer>,
    /// The first extra channel of the composite is its transparency.
    merged_alpha: bool,
    /// Resource 1053's identifier of the first extra channel: 0 is the
    /// merged transparency, saved selections and spot colours have others.
    first_alpha_id: Option<u32>,
    /// Resource 1006's name of the first extra channel.
    first_alpha_name: Option<String>,
    /// EXIF orientation (1 to 8) from resource 1058.
    pub orientation: u8,
    image_data: Option<usize>,
    pub notes: Vec<String>,
}

pub struct Layer {
    pub name: String,
    pub top: i32,
    pub left: i32,
    pub bottom: i32,
    pub right: i32,
    channels: Vec<Chan>,
    pub blend: [u8; 4],
    pub opacity: u8,
    pub clipping: u8,
    pub flags: u8,
    pub mask: Option<MaskRect>,
    /// `lsct`: 1 and 2 open and closed folders, 3 the end of a folder.
    pub section: u32,
    pub section_blend: Option<[u8; 4]>,
    /// Fill opacity (`iOpa`).
    pub fill: u8,
    /// What else the layer is: "text", "smart object", "adjustment"...
    pub kind: Option<&'static str>,
    pub effects: bool,
    /// Channel -2 holds the vector mask Photoshop rendered for older
    /// readers (mask flag bit 3), and channel -3 the user's own mask.
    rendered_vector: bool,
    pub real_mask: Option<MaskRect>,
    /// Mask densities, 0 (no effect) to 1.
    user_density: f32,
    vector_density: f32,
    /// A mask is feathered (blurred), which isn't drawn.
    pub feathered: bool,
    /// The colour of a solid-colour fill (`SoCo`, or a shape's `vscg`).
    pub solid: Option<[f32; 3]>,
    pub vector: Option<VectorMask>,
    /// A shape outline (`vstk`).
    pub stroke: bool,
}

/// One factor of a layer's mask.
enum Factor {
    /// A stored mask channel (-2 or -3) over its rectangle.
    Channel { id: i16, rect: MaskRect, density: f32 },
    /// The vector mask's paths.
    Vector { density: f32 },
}

#[derive(Clone, Copy)]
struct Chan {
    id: i16,
    at: usize,
    len: usize,
}

#[derive(Clone, Copy)]
pub struct MaskRect {
    pub top: i32,
    pub left: i32,
    pub bottom: i32,
    pub right: i32,
    pub default: u8,
    pub disabled: bool,
}

impl Layer {
    pub fn hidden(&self) -> bool {
        self.flags & 2 != 0
    }
    fn width(&self) -> u32 {
        (self.right as i64 - self.left as i64).clamp(0, MAX_SIDE as i64) as u32
    }
    fn height(&self) -> u32 {
        (self.bottom as i64 - self.top as i64).clamp(0, MAX_SIDE as i64) as u32
    }
    fn has_pixels(&self) -> bool {
        self.width() > 0 && self.height() > 0 && self.channels.iter().any(|c| c.id >= 0 && c.len > 2)
    }
}

pub fn is_psd(head: &[u8]) -> bool {
    head.len() >= 26 && head.starts_with(b"8BPS") && matches!(u16::from_be_bytes([head[4], head[5]]), 1 | 2)
}

pub fn mode_name(mode: u16) -> &'static str {
    match mode {
        0 => "bitmap",
        1 => "greyscale",
        2 => "indexed",
        3 => "RGB",
        4 => "CMYK",
        7 => "multichannel",
        8 => "duotone",
        9 => "Lab",
        _ => "unknown",
    }
}

impl Psd {
    pub fn parse(data: Arc<Vec<u8>>) -> Result<Psd, String> {
        let d: &[u8] = &data;
        if !is_psd(d) {
            return Err("not a Photoshop file".into());
        }
        let mut r = R { d, at: 4 };
        let version = r.u16()?;
        let psb = version == 2;
        r.skip(6)?;
        let channels = r.u16()?;
        let height = r.u32()?;
        let width = r.u32()?;
        let depth = r.u16()?;
        let mode = r.u16()?;
        if channels == 0 || channels as usize > MAX_CHANNELS {
            return Err(format!("{channels} channels"));
        }
        let max = if psb { MAX_SIDE } else { 30_000 };
        if width == 0 || height == 0 || width > max || height > max {
            return Err(format!("a document of {width} × {height} pixels"));
        }
        if !matches!(depth, 1 | 8 | 16 | 32) {
            return Err(format!("{depth} bits per channel"));
        }
        if !matches!(mode, 0..=4 | 7..=9) {
            return Err(format!("colour mode {mode}"));
        }
        let mut psd = Psd {
            data: data.clone(),
            psb,
            width,
            height,
            channels,
            depth,
            mode,
            palette: Vec::new(),
            transparent_index: None,
            thumbnail: None,
            has_real_merged: None,
            writer: None,
            icc: false,
            layers: Vec::new(),
            merged_alpha: false,
            first_alpha_id: None,
            first_alpha_name: None,
            orientation: 1,
            image_data: None,
            notes: Vec::new(),
        };
        // 1. Colour mode data: an indexed document's palette.
        let n = r.len(false)?;
        let cm = r.take(n)?;
        if mode == 2 && cm.len() >= 768 {
            psd.palette = (0..256).map(|i| [cm[i], cm[256 + i], cm[512 + i]]).collect();
        }
        // 2. Image resources.
        let n = r.len(false)?;
        let end = r.at + n;
        let res = r.take(n)?;
        psd.resources(res, end - n);
        // 3. Layers.  A damaged layer section still leaves the composite.
        let at = r.at;
        match r.len(psb) {
            Ok(n) => {
                let end = at + if psb { 8 } else { 4 } + n;
                if let Err(e) = psd.layer_section(&mut r, end.min(d.len())) {
                    psd.notes.push(format!("the layers couldn't be read ({e})"));
                    psd.layers.clear();
                }
                if end <= d.len() {
                    psd.image_data = Some(end);
                }
            }
            Err(_) => psd.notes.push("the file ends before its layers".into()),
        }
        Ok(psd)
    }

    fn resources(&mut self, res: &[u8], base: usize) {
        let mut r = R { d: res, at: 0 };
        while r.at + 12 <= res.len() {
            let Ok(sig) = r.take(4) else { break };
            if sig != b"8BIM" {
                break;
            }
            let (Ok(id), Ok(name_len)) = (r.u16(), r.u8()) else { break };
            // A Pascal name padded to an even length, counting its length byte.
            let pad = (name_len as usize + 1) % 2;
            if r.skip(name_len as usize + pad).is_err() {
                break;
            }
            let Ok(size) = r.u32() else { break };
            let start = r.at;
            let Ok(body) = r.take(size as usize) else { break };
            if size % 2 == 1 {
                let _ = r.skip(1);
            }
            match id {
                1033 | 1036 if body.len() > 28 => {
                    self.thumbnail = Some((base + start + 28, body.len() - 28, id == 1033));
                }
                1039 => self.icc = true,
                1058 => self.orientation = exif_orientation(body).unwrap_or(1),
                1006 if !body.is_empty() => {
                    let n = (body[0] as usize).min(body.len() - 1);
                    self.first_alpha_name = Some(String::from_utf8_lossy(&body[1..1 + n]).into_owned());
                }
                1053 if body.len() >= 4 => {
                    self.first_alpha_id = Some(u32::from_be_bytes([body[0], body[1], body[2], body[3]]));
                }
                1047 if body.len() >= 2 => self.transparent_index = Some(body[1]),
                1057 if body.len() >= 5 => {
                    self.has_real_merged = Some(body[4] != 0);
                    let mut s = R { d: body, at: 5 };
                    if let Ok(n) = s.u32()
                        && let Ok(chars) = s.take((n as usize).min(256) * 2)
                    {
                        let units: Vec<u16> = chars.chunks_exact(2).map(|c| u16::from_be_bytes([c[0], c[1]])).collect();
                        let name = String::from_utf16_lossy(&units).trim_end_matches('\0').to_string();
                        if !name.is_empty() {
                            self.writer = Some(name);
                        }
                    }
                }
                _ => {}
            }
        }
    }

    fn layer_section(&mut self, r: &mut R, end: usize) -> Result<(), String> {
        // Layer info, then the global mask, then tagged blocks; 16 and 32-bit
        // documents keep their layers in an `Lr16` or `Lr32` block instead.
        let info_len = r.len(self.psb)?;
        let info_end = r.at.checked_add(info_len).ok_or("bad length")?;
        if info_len > 0 {
            self.layer_info(r, info_end.min(end))?;
        }
        r.at = info_end;
        if r.at + 4 <= end {
            let g = r.u32()? as usize;
            r.at = r.at.saturating_add(g);
        }
        while r.at + 12 <= end {
            let Some((key, start, len)) = self.tagged(r, end) else { break };
            if matches!(&key, b"Lr16" | b"Lr32" | b"Layr") && self.layers.is_empty() && len > 0 {
                let mut inner = R { d: r.d, at: start };
                self.layer_info(&mut inner, start + len)?;
            }
        }
        Ok(())
    }

    /// One tagged block: its key and where its data is, leaving `r` after
    /// it (padding included).
    fn tagged(&self, r: &mut R, end: usize) -> Option<([u8; 4], usize, usize)> {
        // Padding between blocks varies by writer: find the next signature.
        for _ in 0..4 {
            let sig = r.d.get(r.at..r.at + 4)?;
            if sig == b"8BIM" || sig == b"8B64" {
                break;
            }
            r.at += 1;
        }
        let sig = r.take(4).ok()?;
        if sig != b"8BIM" && sig != b"8B64" {
            return None;
        }
        let key: [u8; 4] = r.take(4).ok()?.try_into().ok()?;
        const LONG: [&[u8; 4]; 13] = [
            b"LMsk", b"Lr16", b"Lr32", b"Layr", b"Mt16", b"Mt32", b"Mtrn", b"Alph", b"FMsk", b"lnk2", b"FEid", b"FXid",
            b"PxSD",
        ];
        let big = self.psb && LONG.contains(&&key);
        let len = r.len(big).ok()?;
        let start = r.at;
        let stop = start.checked_add(len)?;
        if stop > end {
            return None;
        }
        r.at = stop + (len % 2);
        Some((key, start, len))
    }

    fn layer_info(&mut self, r: &mut R, end: usize) -> Result<(), String> {
        let count = r.i16()?;
        self.merged_alpha = count < 0;
        let n = count.unsigned_abs() as usize;
        if n > MAX_LAYERS {
            return Err(format!("{n} layers"));
        }
        let mut layers = Vec::with_capacity(n);
        for _ in 0..n {
            layers.push(self.layer_record(r)?);
        }
        // The channels' pixel data follows, layer by layer.
        for l in &mut layers {
            for c in &mut l.channels {
                c.at = r.at;
                r.at = r.at.checked_add(c.len).ok_or("bad channel length")?;
                if r.at > end {
                    return Err("the layer pixel data is cut short".into());
                }
            }
        }
        self.layers = layers;
        Ok(())
    }

    fn layer_record(&self, r: &mut R) -> Result<Layer, String> {
        let (top, left, bottom, right) = (r.i32()?, r.i32()?, r.i32()?, r.i32()?);
        let nch = r.u16()? as usize;
        if nch > MAX_CHANNELS {
            return Err(format!("a layer with {nch} channels"));
        }
        let mut channels = Vec::with_capacity(nch);
        for _ in 0..nch {
            let id = r.i16()?;
            let len = r.len(self.psb)?;
            channels.push(Chan { id, at: 0, len });
        }
        let sig = r.take(4)?;
        if sig != b"8BIM" && sig != b"8B64" {
            return Err("a layer record lacks its signature".into());
        }
        let blend: [u8; 4] = r.take(4)?.try_into().unwrap_or(*b"norm");
        let opacity = r.u8()?;
        let clipping = r.u8()?;
        let flags = r.u8()?;
        r.skip(1)?;
        let extra = r.u32()? as usize;
        let extra_end = r.at.checked_add(extra).ok_or("bad length")?;
        if extra_end > r.d.len() {
            return Err("a layer record is cut short".into());
        }
        let mut layer = Layer {
            name: String::new(),
            top,
            left,
            bottom,
            right,
            channels,
            blend,
            opacity,
            clipping,
            flags,
            mask: None,
            section: 0,
            section_blend: None,
            fill: 255,
            kind: None,
            effects: false,
            rendered_vector: false,
            real_mask: None,
            user_density: 1.0,
            vector_density: 1.0,
            feathered: false,
            solid: None,
            vector: None,
            stroke: false,
        };
        // Mask data.
        let mlen = r.u32()? as usize;
        let mstart = r.at;
        if mlen >= 18 {
            let rec = r.take(mlen)?;
            read_mask_record(rec, &mut layer);
        }
        r.at = mstart.checked_add(mlen).ok_or("bad length")?;
        // Blending ranges.
        let blen = r.u32()? as usize;
        r.skip(blen)?;
        // The name: a Pascal string padded to four bytes.
        let nlen = r.u8()? as usize;
        let name = r.take(nlen)?;
        layer.name = name.iter().map(|&b| b as char).collect();
        let used = 1 + nlen;
        r.skip((4 - used % 4) % 4).ok();
        // Tagged blocks.
        while r.at + 12 <= extra_end {
            let Some((key, start, len)) = self.tagged(r, extra_end) else { break };
            let data = &r.d[start..start + len];
            match &key {
                b"luni" if len >= 4 => {
                    let n = (u32::from_be_bytes([data[0], data[1], data[2], data[3]]) as usize).min((len - 4) / 2);
                    let units: Vec<u16> =
                        data[4..4 + n * 2].chunks_exact(2).map(|c| u16::from_be_bytes([c[0], c[1]])).collect();
                    let s = String::from_utf16_lossy(&units).trim_end_matches('\0').to_string();
                    if !s.is_empty() {
                        layer.name = s;
                    }
                }
                b"lsct" | b"lsdk" if len >= 4 => {
                    layer.section = u32::from_be_bytes([data[0], data[1], data[2], data[3]]);
                    if len >= 12 && &data[4..8] == b"8BIM" {
                        layer.section_blend = data[8..12].try_into().ok();
                    }
                }
                b"iOpa" if len >= 1 => layer.fill = data[0],
                b"lfx2" | b"lrFX" | b"lmfx" => layer.effects = true,
                b"TySh" | b"tySh" => layer.kind = Some("text"),
                b"SoLd" | b"PlLd" | b"SoLE" => layer.kind = Some("smart object"),
                b"SoCo" => {
                    layer.kind = Some("fill layer");
                    layer.solid = parse_solid_color(data);
                }
                b"GdFl" | b"PtFl" => layer.kind = Some("fill layer"),
                // A shape's fill: a key naming the kind, then its descriptor.
                b"vscg" if len >= 4 => {
                    layer.kind = Some("fill layer");
                    if &data[..4] == b"SoCo" {
                        layer.solid = parse_solid_color(&data[4..]);
                    }
                }
                b"vmsk" | b"vsms" => layer.vector = parse_vector_mask(data, self.width, self.height),
                b"vstk" => layer.stroke = true,
                b"levl" | b"curv" | b"brit" | b"blnc" | b"hue " | b"hue2" | b"selc" | b"mixr" | b"grdm" | b"phfl"
                | b"expA" | b"vibA" | b"blwh" | b"nvrt" | b"post" | b"thrs" | b"clrL" => {
                    layer.kind = Some("adjustment layer")
                }
                _ => {}
            }
        }
        r.at = extra_end;
        Ok(layer)
    }
}

/// A layer's mask record: the rectangle, default and flags of channel -2,
/// then (in 36 bytes or more) those of the real user mask, channel -3,
/// and the density and feather parameters when flag bit 4 says so.  Most
/// writers put the parameters after the real-mask fields; some leave the
/// real fields out, which only the record's length tells.
fn read_mask_record(rec: &[u8], layer: &mut Layer) {
    let i32at = |at: usize| rec.get(at..at + 4).map_or(0, |b| i32::from_be_bytes([b[0], b[1], b[2], b[3]]));
    let rect = |at: usize, default: u8, disabled: bool| MaskRect {
        top: i32at(at),
        left: i32at(at + 4),
        bottom: i32at(at + 8),
        right: i32at(at + 12),
        default,
        disabled,
    };
    let (default, flags) = (rec[16], rec[17]);
    layer.mask = Some(rect(0, default, flags & 2 != 0));
    layer.rendered_vector = flags & 8 != 0;
    let params_len = |at: usize| {
        rec.get(at).map(|&f| {
            1 + (f & 1 != 0) as usize + 8 * (f & 2 != 0) as usize + (f & 4 != 0) as usize + 8 * (f & 8 != 0) as usize
        })
    };
    let (mut real_at, mut params_at) = (None, None);
    if flags & 0x10 != 0 {
        match params_len(18) {
            Some(p) if 18 + p <= rec.len() && rec.len() < 36 + p => params_at = Some(18),
            _ => (real_at, params_at) = (Some(18), Some(36)),
        }
    } else if rec.len() >= 36 {
        real_at = Some(18);
    }
    if let Some(at) = real_at.filter(|at| at + 18 <= rec.len()) {
        layer.real_mask = Some(rect(at + 2, rec[at + 1], rec[at] & 2 != 0));
    }
    if let Some(at) = params_at
        && let Some(&f) = rec.get(at)
    {
        let mut i = at + 1;
        let density = |on: bool, i: &mut usize| {
            let d = if on { rec.get(*i).map_or(1.0, |&v| v as f32 / 255.0) } else { 1.0 };
            *i += on as usize;
            d
        };
        layer.user_density = density(f & 1 != 0, &mut i);
        let feather = |on: bool, i: &mut usize| {
            let v = if on {
                rec.get(*i..*i + 8).map_or(0.0, |b| f64::from_be_bytes(b.try_into().unwrap_or([0; 8])))
            } else {
                0.0
            };
            *i += 8 * on as usize;
            v > 0.0
        };
        layer.feathered |= feather(f & 2 != 0, &mut i);
        layer.vector_density = density(f & 4 != 0, &mut i);
        layer.feathered |= feather(f & 8 != 0, &mut i);
    }
}

//endregion

/*┌─────────────────╮
| Pixels          |
└─────────────────╯*/
//region Pixels

/// One decoded channel.
enum Plane {
    U8(Vec<u8>),
    U16(Vec<u16>),
    F32(Vec<f32>),
}

impl Plane {
    #[inline]
    fn get(&self, i: usize) -> f32 {
        match self {
            Plane::U8(v) => v.get(i).map_or(0.0, |&x| x as f32 / 255.0),
            Plane::U16(v) => v.get(i).map_or(0.0, |&x| x as f32 / 65535.0),
            Plane::F32(v) => v.get(i).copied().unwrap_or(0.0),
        }
    }
}

/// PackBits: a count byte below 128 copies that many plus one bytes, one
/// above repeats the next byte 257 minus it times, 128 does nothing.
fn unpackbits(src: &[u8], out: &mut Vec<u8>, want: usize) {
    let mut i = 0;
    let stop = out.len() + want;
    while i < src.len() && out.len() < stop {
        let n = src[i] as i8;
        i += 1;
        if n >= 0 {
            let len = n as usize + 1;
            let end = (i + len).min(src.len());
            out.extend_from_slice(&src[i..end]);
            i += len;
        } else if n != -128 {
            let Some(&v) = src.get(i) else { break };
            out.extend(std::iter::repeat_n(v, (1 - n as i32) as usize));
            i += 1;
        }
    }
    out.resize(stop, 0);
}

impl Psd {
    fn bytes(&self) -> usize {
        match self.depth {
            16 => 2,
            32 => 4,
            _ => 1,
        }
    }

    /// Bytes of one row of a channel `w` pixels wide.
    fn row_bytes(&self, w: usize) -> usize {
        if self.depth == 1 { w.div_ceil(8) } else { w * self.bytes() }
    }

    /// Unpacks `rows` rows of one channel stored with `compression` from
    /// `src` (after the compression word), keeping every `step`th row.
    fn unpack(&self, compression: u16, src: &[u8], w: usize, h: usize, step: usize) -> Result<Vec<u8>, String> {
        let rb = self.row_bytes(w);
        let kept = h.div_ceil(step);
        match compression {
            0 => {
                let mut out = Vec::with_capacity(kept * rb);
                for y in (0..h).step_by(step) {
                    match src.get(y * rb..y * rb + rb) {
                        Some(row) => out.extend_from_slice(row),
                        None => out.resize(out.len() + rb, 0),
                    }
                }
                Ok(out)
            }
            1 => {
                let cw = if self.psb { 4 } else { 2 };
                let counts = src.get(..h * cw).ok_or("the row lengths are cut short")?;
                let len = |y: usize| {
                    let c = &counts[y * cw..y * cw + cw];
                    if cw == 2 {
                        u16::from_be_bytes([c[0], c[1]]) as usize
                    } else {
                        u32::from_be_bytes([c[0], c[1], c[2], c[3]]) as usize
                    }
                };
                let mut starts = Vec::with_capacity(h);
                let mut at = h * cw;
                for y in 0..h {
                    starts.push(at);
                    at = at.saturating_add(len(y));
                }
                let mut out = Vec::with_capacity(kept * rb);
                for y in (0..h).step_by(step) {
                    let s = starts[y].min(src.len());
                    let e = s.saturating_add(len(y)).min(src.len());
                    unpackbits(&src[s..e], &mut out, rb);
                }
                Ok(out)
            }
            2 | 3 => {
                let total = rb.checked_mul(h).ok_or("too large")?;
                let mut out = miniz_oxide::inflate::decompress_to_vec_zlib_with_limit(src, total)
                    .or_else(|e| if e.output.is_empty() { Err(e) } else { Ok(e.output) })
                    .map_err(|_| "damaged zip data".to_string())?;
                out.resize(total, 0);
                if compression == 3 {
                    self.unpredict(&mut out, w, h);
                }
                if step == 1 {
                    return Ok(out);
                }
                Ok((0..h).step_by(step).flat_map(|y| out[y * rb..y * rb + rb].to_vec()).collect())
            }
            _ => Err(format!("compression {compression}")),
        }
    }

    /// Undoes zip prediction: each row holds differences from the sample
    /// to its left; 32-bit rows also split each float's bytes into four
    /// runs.
    fn unpredict(&self, data: &mut [u8], w: usize, h: usize) {
        let rb = self.row_bytes(w);
        for row in data.chunks_exact_mut(rb).take(h) {
            match self.depth {
                16 => {
                    let mut prev = 0u16;
                    for c in row.chunks_exact_mut(2) {
                        let v = u16::from_be_bytes([c[0], c[1]]).wrapping_add(prev);
                        c.copy_from_slice(&v.to_be_bytes());
                        prev = v;
                    }
                }
                32 => {
                    for i in 1..row.len() {
                        row[i] = row[i].wrapping_add(row[i - 1]);
                    }
                    let planar = row.to_vec();
                    for x in 0..w {
                        for b in 0..4 {
                            row[x * 4 + b] = planar[b * w + x];
                        }
                    }
                }
                _ => {
                    for i in 1..row.len() {
                        row[i] = row[i].wrapping_add(row[i - 1]);
                    }
                }
            }
        }
    }

    /// Unpacked bytes as a plane of samples, keeping every `step`th column.
    fn plane(&self, bytes: &[u8], w: usize, rows: usize, step: usize) -> Plane {
        let rb = self.row_bytes(w);
        let cols = w.div_ceil(step);
        match self.depth {
            1 => Plane::U8(
                (0..rows)
                    .flat_map(|y| (0..cols).map(move |x| (y, x * step)))
                    .map(|(y, x)| {
                        let b = bytes.get(y * rb + x / 8).copied().unwrap_or(0);
                        if b >> (7 - x % 8) & 1 == 1 { 0 } else { 255 }
                    })
                    .collect(),
            ),
            16 => Plane::U16(
                (0..rows)
                    .flat_map(|y| (0..cols).map(move |x| y * rb + x * step * 2))
                    .map(|i| bytes.get(i..i + 2).map_or(0, |b| u16::from_be_bytes([b[0], b[1]])))
                    .collect(),
            ),
            32 => Plane::F32(
                (0..rows)
                    .flat_map(|y| (0..cols).map(move |x| y * rb + x * step * 4))
                    .map(|i| bytes.get(i..i + 4).map_or(0.0, |b| f32::from_be_bytes([b[0], b[1], b[2], b[3]])))
                    .collect(),
            ),
            _ => Plane::U8(
                (0..rows)
                    .flat_map(|y| (0..cols).map(move |x| y * rb + x * step))
                    .map(|i| bytes.get(i).copied().unwrap_or(0))
                    .collect(),
            ),
        }
    }

    /// Colour planes and alpha to RGBA in the document's sample size (8 and
    /// 16-bit values as stored, gamma-encoded; 32-bit linear floats).
    fn rgba(&self, planes: &[Option<Plane>], alpha: Option<&Plane>, n: usize) -> Samples {
        let ch = |k: usize, i: usize| planes.get(k).and_then(|p| p.as_ref()).map_or(0.0, |p| p.get(i));
        let px = |i: usize| -> [f32; 4] {
            let a = alpha.map_or(1.0, |p| p.get(i));
            let rgb = match self.mode {
                // Bitmap, greyscale and duotone: one grey channel.
                0 | 1 | 8 => [ch(0, i); 3],
                2 => {
                    let idx = (ch(0, i) * 255.0 + 0.5) as usize;
                    let c = self.palette.get(idx).copied().unwrap_or([0; 3]);
                    if self.transparent_index == Some(idx as u8) {
                        return [0.0; 4];
                    }
                    [c[0] as f32 / 255.0, c[1] as f32 / 255.0, c[2] as f32 / 255.0]
                }
                // CMYK is stored inverted: 1 is no ink.
                4 => {
                    let k = ch(3, i);
                    [ch(0, i) * k, ch(1, i) * k, ch(2, i) * k]
                }
                // Inks: each channel darkens like a process colour.
                7 => {
                    let count = planes.iter().filter(|p| p.is_some()).count();
                    let ink = |k: usize| if k < count { ch(k, i) } else { 1.0 };
                    if count == 1 { [ch(0, i); 3] } else { [ink(0) * ink(3), ink(1) * ink(3), ink(2) * ink(3)] }
                }
                9 => lab_to_srgb(ch(0, i), ch(1, i), ch(2, i)),
                _ => [ch(0, i), ch(1, i), ch(2, i)],
            };
            [rgb[0], rgb[1], rgb[2], a]
        };
        match self.depth {
            32 => Samples::F32((0..n).flat_map(px).collect()),
            16 => {
                Samples::U16((0..n).flat_map(|i| px(i).map(|v| (v.clamp(0.0, 1.0) * 65535.0 + 0.5) as u16)).collect())
            }
            _ => Samples::U8((0..n).flat_map(|i| px(i).map(|v| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8)).collect()),
        }
    }

    /// The colour channels a mode stores, in order.
    fn colour_channels(&self) -> usize {
        match self.mode {
            0 | 1 | 2 | 8 => 1,
            4 => 4,
            7 => (self.channels as usize).min(4),
            _ => 3,
        }
    }

    /// The stored composite, every `step`th pixel each way.  `None` when the
    /// file has none.
    pub fn merged(&self, step: usize) -> Result<(u32, u32, Samples), String> {
        let at = self.image_data.ok_or("the file has no composite image")?;
        let d: &[u8] = &self.data;
        let mut r = R { d, at };
        let compression = r.u16()?;
        let (w, h) = (self.width as usize, self.height as usize);
        let step = step.max(1);
        let colours = self.colour_channels();
        // A negative layer count says the first extra channel is the
        // transparency, but 16 and 32-bit files lose that sign in their
        // `Lr16` and `Lr32` blocks.  Then the channel's identifier says it
        // (0 is the transparency), or failing that its name; an extra
        // channel nothing describes is taken for transparency.
        let alpha = self.merged_alpha
            || match (self.first_alpha_id, &self.first_alpha_name) {
                (Some(id), _) => id == 0,
                (None, Some(name)) => name == "Transparency",
                (None, None) => true,
            };
        let wanted = colours + usize::from(alpha && (self.channels as usize) > colours);
        let wanted = wanted.min(self.channels as usize);
        let rb = self.row_bytes(w);
        let rows = h.div_ceil(step);
        let mut planes = Vec::new();
        match compression {
            1 => {
                // Row lengths for every channel come first.
                let cw = if self.psb { 4 } else { 2 };
                let total_rows = h * self.channels as usize;
                let counts = r.take(total_rows * cw)?.to_vec();
                let mut at = r.at;
                for c in 0..self.channels as usize {
                    let mut lens = Vec::with_capacity(h);
                    let mut size = 0usize;
                    for y in 0..h {
                        let k = &counts[(c * h + y) * cw..(c * h + y) * cw + cw];
                        let n = if cw == 2 {
                            u16::from_be_bytes([k[0], k[1]]) as usize
                        } else {
                            u32::from_be_bytes([k[0], k[1], k[2], k[3]]) as usize
                        };
                        lens.push(n);
                        size = size.saturating_add(n);
                    }
                    if c < wanted {
                        // Rebuild this channel's table in front of its data.
                        let mut src = Vec::with_capacity(h * cw);
                        for n in &lens {
                            if cw == 2 {
                                src.extend_from_slice(&(*n as u16).to_be_bytes());
                            } else {
                                src.extend_from_slice(&(*n as u32).to_be_bytes());
                            }
                        }
                        let end = at.saturating_add(size).min(d.len());
                        src.extend_from_slice(&d[at.min(d.len())..end]);
                        let bytes = self.unpack(1, &src, w, h, step)?;
                        planes.push(Some(self.plane(&bytes, w, rows, step)));
                    }
                    at = at.saturating_add(size);
                }
            }
            0 => {
                for c in 0..wanted {
                    let start = r.at.saturating_add(c * rb * h).min(d.len());
                    let bytes = self.unpack(0, &d[start..], w, h, step)?;
                    planes.push(Some(self.plane(&bytes, w, rows, step)));
                }
            }
            2 | 3 => {
                // One zip stream holds every channel, one after another.
                let total = (rb * h).checked_mul(self.channels as usize).ok_or("too large")?;
                let mut all = miniz_oxide::inflate::decompress_to_vec_zlib_with_limit(&d[r.at..], total)
                    .or_else(|e| if e.output.is_empty() { Err(e) } else { Ok(e.output) })
                    .map_err(|_| "damaged zip data".to_string())?;
                all.resize(total, 0);
                for c in 0..wanted {
                    let mut bytes = all[c * rb * h..(c + 1) * rb * h].to_vec();
                    if compression == 3 {
                        self.unpredict(&mut bytes, w, h);
                    }
                    let bytes: Vec<u8> =
                        (0..h).step_by(step).flat_map(|y| bytes[y * rb..y * rb + rb].to_vec()).collect();
                    planes.push(Some(self.plane(&bytes, w, rows, step)));
                }
            }
            c => return Err(format!("compression {c}")),
        }
        let cols = w.div_ceil(step);
        let alpha = if wanted > colours { planes.pop().flatten() } else { None };
        let mut samples = self.rgba(&planes, alpha.as_ref(), cols * rows);
        if alpha.is_some() {
            unmatte(&mut samples);
        }
        Ok((cols as u32, rows as u32, samples))
    }

    /// Whether the stored composite is only a stand-in: Photoshop says so in
    /// resource 1057 when "Maximize Compatibility" was off, and other
    /// writers leave it entirely transparent although layers have pixels.
    pub fn merged_is_blank(&self, s: &Samples) -> bool {
        if self.has_real_merged == Some(false) {
            return true;
        }
        if !self.layers.iter().any(|l| l.has_pixels() && !l.hidden()) {
            return false;
        }
        match s {
            Samples::U8(v) => v.chunks_exact(4).all(|p| p[3] == 0),
            Samples::U16(v) => v.chunks_exact(4).all(|p| p[3] == 0),
            Samples::F32(v) => v.chunks_exact(4).all(|p| p[3] <= 0.0),
            _ => false,
        }
    }

    /// The JPEG thumbnail Photoshop stores among its resources.
    pub fn thumbnail(&self) -> Option<slerp_scene::Texture> {
        let (at, len, bgr) = self.thumbnail?;
        let jpeg = self.data.get(at..at + len)?;
        let opts = slerp_scene::textures::DecodeOptions::full();
        let mut tex = slerp_scene::textures::decode_as(slerp_scene::textures::Format::Jpeg, jpeg, &opts).ok()?;
        if bgr
            && let Some(slerp_scene::textures::Pixels::Rgba8(px)) =
                tex.levels.get_mut(0).and_then(|l| l.surfaces.get_mut(0))
        {
            px.chunks_exact_mut(4).for_each(|p| p.swap(0, 2));
        }
        Some(tex)
    }

    /// The enabled parts of layer `l`'s mask.  Channel -2 is the user's mask,
    /// or the vector mask as Photoshop rendered it (then -3 is the user's);
    /// a vector mask without a rendered copy is drawn from its paths.
    fn mask_factors(&self, l: &Layer) -> Vec<Factor> {
        let has = |id: i16| l.channels.iter().any(|c| c.id == id);
        let vector_on = l.vector.as_ref().is_some_and(|v| !v.disabled);
        let mut out = Vec::new();
        if let Some(m) = l.mask.filter(|_| has(-2)) {
            if l.rendered_vector {
                if !m.disabled && (vector_on || l.vector.is_none()) {
                    out.push(Factor::Channel { id: -2, rect: m, density: l.vector_density });
                }
                if let Some(real) = l.real_mask.filter(|r| !r.disabled && has(-3)) {
                    out.push(Factor::Channel { id: -3, rect: real, density: l.user_density });
                }
                return out;
            }
            if !m.disabled {
                out.push(Factor::Channel { id: -2, rect: m, density: l.user_density });
            }
        }
        if vector_on {
            out.push(Factor::Vector { density: l.vector_density });
        }
        out
    }

    /// A mask factor's rectangle (left, top, right, bottom) and its value
    /// outside it.
    fn factor_area(&self, f: &Factor, l: &Layer) -> ([i32; 4], f32) {
        let dense = |m: f32, d: f32| 1.0 - d * (1.0 - m);
        match f {
            Factor::Channel { rect, density, .. } => {
                ([rect.left, rect.top, rect.right, rect.bottom], dense(rect.default as f32 / 255.0, *density))
            }
            Factor::Vector { density } => {
                let v = l.vector.as_ref();
                let b = v.and_then(|v| v.bounds()).map_or([0; 4], |(x0, y0, x1, y1)| [x0, y0, x1, y1]);
                (b, dense(v.map_or(1.0, |v| v.outside()), *density))
            }
        }
    }

    /// Where layer `l`'s mask goes and its value outside its rectangle:
    /// a single stored channel as it is, several parts multiplied over the
    /// union of their rectangles within the canvas.
    fn mask_plan(&self, l: &Layer) -> Option<(i32, i32, f32)> {
        let factors = self.mask_factors(l);
        let areas: Vec<([i32; 4], f32)> = factors.iter().map(|f| self.factor_area(f, l)).collect();
        let outside = areas.iter().map(|a| a.1).product();
        match factors.as_slice() {
            [] => None,
            [Factor::Channel { rect, density, .. }] if *density >= 1.0 => Some((rect.left, rect.top, outside)),
            _ => {
                let r = self.mask_union(&areas);
                Some((r[0], r[1], outside))
            }
        }
    }

    /// The union of the factors' rectangles, clipped to the canvas.
    fn mask_union(&self, areas: &[([i32; 4], f32)]) -> [i32; 4] {
        let mut u = [i32::MAX, i32::MAX, i32::MIN, i32::MIN];
        for (r, _) in areas.iter().filter(|(r, _)| r[2] > r[0] && r[3] > r[1]) {
            u = [u[0].min(r[0]), u[1].min(r[1]), u[2].max(r[2]), u[3].max(r[3])];
        }
        let c = [u[0].max(0), u[1].max(0), u[2].min(self.width as i32), u[3].min(self.height as i32)];
        if c[2] > c[0] && c[3] > c[1] { c } else { [0; 4] }
    }

    /// Where a fill layer without stored pixels gets painted: within its
    /// vector mask's paths when the mask hides everything else, else the
    /// whole canvas.  None for layers that aren't solid fills.
    fn fill_rect(&self, l: &Layer) -> Option<(i32, i32, u32, u32)> {
        l.solid?;
        let (w, h) = (self.width as i32, self.height as i32);
        let r = match l.vector.as_ref().filter(|v| !v.disabled) {
            Some(v) if v.outside() == 0.0 => {
                let (x0, y0, x1, y1) = v.bounds()?;
                [x0.max(0), y0.max(0), x1.min(w), y1.min(h)]
            }
            _ => [0, 0, w, h],
        };
        (r[2] > r[0] && r[3] > r[1]).then(|| (r[0], r[1], (r[2] - r[0]) as u32, (r[3] - r[1]) as u32))
    }

    /// A stored mask channel (-2 or -3) over `rect`, as stored.
    fn mask_channel(&self, l: &Layer, id: i16, rect: &MaskRect) -> Result<Raster, String> {
        let w = (rect.right as i64 - rect.left as i64).clamp(0, MAX_SIDE as i64) as usize;
        let h = (rect.bottom as i64 - rect.top as i64).clamp(0, MAX_SIDE as i64) as usize;
        let c = l.channels.iter().find(|c| c.id == id).ok_or("no mask channel")?;
        let src = self.data.get(c.at..c.at.saturating_add(c.len)).ok_or("mask data out of the file")?;
        if w == 0 || h == 0 || src.len() <= 2 {
            return Ok(Raster { width: 0, height: 0, channels: 1, samples: Samples::U8(Vec::new()) });
        }
        if (w as u64) * (h as u64) > 1 << 28 {
            return Err(format!("a mask of {w} × {h} pixels is too large"));
        }
        let compression = u16::from_be_bytes([src[0], src[1]]);
        // Masks are 8-bit in 8-bit documents and follow the depth otherwise.
        let bytes = self.unpack(compression, &src[2..], w, h, 1)?;
        let samples = match self.plane(&bytes, w, h, 1) {
            Plane::U8(v) => Samples::U8(v),
            Plane::U16(v) => Samples::U16(v),
            Plane::F32(v) => Samples::F32(v),
        };
        Ok(Raster { width: w as u32, height: h as u32, channels: 1, samples })
    }

    /// The layer tree for the compositor, bottom first, and notes on what
    /// it can't show.
    pub fn tree(&self, notes: &mut Vec<String>) -> Vec<Node> {
        let mut stack: Vec<Vec<Node>> = vec![Vec::new()];
        let (mut adjustments, mut effects, mut approximated) = (0, 0, Vec::new());
        let (mut strokes, mut feathered) = (0, 0);
        for (i, l) in self.layers.iter().enumerate() {
            if l.section == 3 {
                stack.push(Vec::new());
                continue;
            }
            let key = if matches!(l.section, 1 | 2) { l.section_blend.unwrap_or(l.blend) } else { l.blend };
            let mode = blend_mode(&key);
            if mode.approximate() || mode == Mode::Unknown {
                approximated.push(String::from_utf8_lossy(&key).trim().to_string());
            }
            if l.effects {
                effects += 1;
            }
            let mask = self.mask_plan(l).map(|(x, y, outside)| Mask { id: i, x, y, outside });
            if l.feathered && mask.is_some() {
                feathered += 1;
            }
            let base = Node {
                name: l.name.clone(),
                visible: !l.hidden(),
                opacity: l.opacity as f32 / 255.0,
                mode,
                gimp: Default::default(),
                clip: l.clipping == 1,
                mask,
                kind: Kind::Empty,
                slot: 0,
            };
            if matches!(l.section, 1 | 2) && stack.len() > 1 {
                let children = stack.pop().unwrap_or_default();
                let group = Node { kind: Kind::Group { children, isolated: mode != Mode::PassThrough }, ..base };
                if let Some(s) = stack.last_mut() {
                    s.push(group);
                }
                continue;
            }
            let kind = if l.has_pixels() {
                Kind::Raster { id: i, x: l.left, y: l.top }
            } else if let Some((x, y, _, _)) = self.fill_rect(l) {
                strokes += l.stroke as usize;
                Kind::Raster { id: i, x, y }
            } else {
                if matches!(l.kind, Some("adjustment layer") | Some("fill layer")) {
                    adjustments += 1;
                }
                Kind::Empty
            };
            let opacity = base.opacity * l.fill as f32 / 255.0;
            if let Some(s) = stack.last_mut() {
                s.push(Node { kind, opacity, ..base });
            }
        }
        // Folders left open by a damaged file still count as groups.
        while stack.len() > 1 {
            let children = stack.pop().unwrap_or_default();
            let group = Node {
                name: "group".into(),
                visible: true,
                opacity: 1.0,
                mode: Mode::PassThrough,
                gimp: Default::default(),
                clip: false,
                mask: None,
                kind: Kind::Group { children, isolated: false },
                slot: 0,
            };
            if let Some(s) = stack.last_mut() {
                s.push(group);
            }
        }
        if adjustments > 0 {
            notes.push(format!("{adjustments} adjustment or fill layers aren't applied"));
        }
        if effects > 0 {
            notes.push(format!("layer styles (shadows, strokes, glows) of {effects} layers aren't drawn"));
        }
        if strokes > 0 {
            notes.push(format!("the outlines of {strokes} shapes aren't drawn"));
        }
        if feathered > 0 {
            notes.push(format!("{feathered} feathered masks are drawn with sharp edges"));
        }
        approximated.sort();
        approximated.dedup();
        if !approximated.is_empty() {
            notes.push(format!("blend modes drawn as normal: {}", approximated.join(", ")));
        }
        stack.pop().unwrap_or_default()
    }
}

/// The orientation tag (0x0112) of an EXIF block's first directory.
fn exif_orientation(t: &[u8]) -> Option<u8> {
    let le = match t.get(..4)? {
        b"II*\0" => true,
        b"MM\0*" => false,
        _ => return None,
    };
    let u16at = |i: usize| {
        t.get(i..i + 2).map(|b| if le { u16::from_le_bytes([b[0], b[1]]) } else { u16::from_be_bytes([b[0], b[1]]) })
    };
    let u32at = |i: usize| {
        t.get(i..i + 4).map(|b| {
            if le { u32::from_le_bytes([b[0], b[1], b[2], b[3]]) } else { u32::from_be_bytes([b[0], b[1], b[2], b[3]]) }
        })
    };
    let ifd = u32at(4)? as usize;
    let n = u16at(ifd)? as usize;
    for k in 0..n.min(512) {
        let e = ifd + 2 + k * 12;
        if u16at(e)? == 0x0112 {
            let v = u16at(e + 8)?;
            return (1..=8).contains(&v).then_some(v as u8);
        }
    }
    None
}

/// Photoshop stores a transparent composite matted against white (each
/// colour is `c a + (1 - a)`); this takes the white back out.
fn unmatte(s: &mut Samples) {
    fn fix(px: &mut [f32; 4]) {
        let a = px[3];
        if a > 0.0 && a < 1.0 {
            for c in px.iter_mut().take(3) {
                *c = ((*c - (1.0 - a)) / a).clamp(0.0, 1.0);
            }
        }
    }
    match s {
        Samples::U8(v) => v.chunks_exact_mut(4).for_each(|p| {
            let mut f = [p[0], p[1], p[2], p[3]].map(|x| x as f32 / 255.0);
            fix(&mut f);
            for (o, x) in p.iter_mut().zip(f) {
                *o = (x * 255.0 + 0.5) as u8;
            }
        }),
        Samples::U16(v) => v.chunks_exact_mut(4).for_each(|p| {
            let mut f = [p[0], p[1], p[2], p[3]].map(|x| x as f32 / 65535.0);
            fix(&mut f);
            for (o, x) in p.iter_mut().zip(f) {
                *o = (x * 65535.0 + 0.5) as u16;
            }
        }),
        Samples::F32(v) => v.chunks_exact_mut(4).for_each(|p| {
            let mut f = [p[0], p[1], p[2], p[3]];
            // 32-bit composites are matted in linear light; values above 1
            // stay.
            let a = f[3];
            if a > 0.0 && a < 1.0 {
                for c in f.iter_mut().take(3) {
                    *c = ((*c - (1.0 - a)) / a).max(0.0);
                }
            }
            p.copy_from_slice(&f);
        }),
        _ => {}
    }
}

/// Photoshop's blend mode keys.
pub fn blend_mode(key: &[u8; 4]) -> Mode {
    match key {
        b"pass" => Mode::PassThrough,
        b"norm" => Mode::Normal,
        b"diss" => Mode::Dissolve,
        b"dark" => Mode::Darken,
        b"mul " => Mode::Multiply,
        b"idiv" => Mode::ColorBurn,
        b"lbrn" => Mode::LinearBurn,
        b"dkCl" => Mode::DarkerColor,
        b"lite" => Mode::Lighten,
        b"scrn" => Mode::Screen,
        b"div " => Mode::ColorDodge,
        b"lddg" => Mode::Addition,
        b"lgCl" => Mode::LighterColor,
        b"over" => Mode::Overlay,
        b"sLit" => Mode::SoftLightPhotoshop,
        b"hLit" => Mode::HardLight,
        b"vLit" => Mode::VividLight,
        b"lLit" => Mode::LinearLight,
        b"pLit" => Mode::PinLight,
        b"hMix" => Mode::HardMix,
        b"diff" => Mode::Difference,
        b"smud" => Mode::Exclusion,
        b"fsub" => Mode::Subtract,
        b"fdiv" => Mode::Divide,
        b"hue " => Mode::Hue,
        b"sat " => Mode::Saturation,
        b"colr" => Mode::Color,
        b"lum " => Mode::Luminosity,
        _ => Mode::Unknown,
    }
}

/// Lab samples (0 to 1 as stored) to gamma-encoded sRGB.
fn lab_to_srgb(l: f32, a: f32, b: f32) -> [f32; 3] {
    let lin = crate::compose::lab_to_linear([l * 100.0, a * 255.0 - 128.0, b * 255.0 - 128.0]);
    lin.map(crate::compose::srgb_encode)
}

impl Source for Psd {
    fn layer(&self, id: usize) -> Result<Raster, String> {
        let l = self.layers.get(id).ok_or("no such layer")?;
        if !l.has_pixels()
            && let (Some((_, _, w, h)), Some(rgb)) = (self.fill_rect(l), l.solid)
        {
            return solid_raster(rgb, w, h, self.depth);
        }
        let (w, h) = (l.width() as usize, l.height() as usize);
        if (w as u64) * (h as u64) > 1 << 28 {
            return Err(format!("a layer of {w} × {h} pixels is too large"));
        }
        let colours = self.colour_channels();
        let mut planes: Vec<Option<Plane>> = (0..colours).map(|_| None).collect();
        let mut alpha = None;
        for c in &l.channels {
            let slot = if c.id == -1 {
                None
            } else if c.id >= 0 && (c.id as usize) < colours {
                Some(c.id as usize)
            } else {
                continue;
            };
            let src = self.data.get(c.at..c.at.saturating_add(c.len)).ok_or("channel data out of the file")?;
            if src.len() < 2 {
                continue;
            }
            let compression = u16::from_be_bytes([src[0], src[1]]);
            let bytes = self.unpack(compression, &src[2..], w, h, 1)?;
            let plane = self.plane(&bytes, w, h, 1);
            match slot {
                Some(s) => planes[s] = Some(plane),
                None => alpha = Some(plane),
            }
        }
        Ok(Raster {
            width: w as u32,
            height: h as u32,
            channels: 4,
            samples: self.rgba(&planes, alpha.as_ref(), w * h),
        })
    }

    fn mask(&self, id: usize) -> Result<Raster, String> {
        let l = self.layers.get(id).ok_or("no such layer")?;
        let factors = self.mask_factors(l);
        if let [Factor::Channel { id, rect, density }] = factors.as_slice()
            && *density >= 1.0
        {
            return self.mask_channel(l, *id, rect);
        }
        // Several parts, or one with a density: multiply them over the union.
        let areas: Vec<([i32; 4], f32)> = factors.iter().map(|f| self.factor_area(f, l)).collect();
        let u = self.mask_union(&areas);
        let (w, h) = ((u[2] - u[0]) as usize, (u[3] - u[1]) as usize);
        if (w as u64) * (h as u64) > 1 << 28 {
            return Err(format!("a mask of {w} × {h} pixels is too large"));
        }
        let mut out = vec![1f32; w * h];
        for (f, (_, outside)) in factors.iter().zip(&areas) {
            match f {
                Factor::Channel { id, rect, density } => {
                    let r = self.mask_channel(l, *id, rect)?;
                    let (rw, rh) = (r.width as i64, r.height as i64);
                    for y in 0..h {
                        for x in 0..w {
                            let (mx, my) = ((u[0] - rect.left) as i64 + x as i64, (u[1] - rect.top) as i64 + y as i64);
                            let v = if mx >= 0 && my >= 0 && mx < rw && my < rh {
                                1.0 - density * (1.0 - r.samples.get((my * rw + mx) as usize))
                            } else {
                                *outside
                            };
                            out[y * w + x] *= v;
                        }
                    }
                }
                Factor::Vector { density } => {
                    if let Some(v) = &l.vector {
                        for (o, c) in out.iter_mut().zip(v.coverage(u[0], u[1], w, h)) {
                            *o *= 1.0 - density * (1.0 - c);
                        }
                    }
                }
            }
        }
        let samples = if self.depth == 8 {
            Samples::U8(out.iter().map(|v| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8).collect())
        } else {
            Samples::U16(out.iter().map(|v| (v.clamp(0.0, 1.0) * 65535.0 + 0.5) as u16).collect())
        };
        Ok(Raster { width: w as u32, height: h as u32, channels: 1, samples })
    }
}

/// A solid colour (gamma-encoded RGB, 0 to 1) as an opaque RGBA layer in
/// the document's sample size; 32-bit documents hold linear light.
fn solid_raster(rgb: [f32; 3], w: u32, h: u32, depth: u16) -> Result<Raster, String> {
    let n = w as usize * h as usize;
    if n > 1 << 28 {
        return Err(format!("a fill of {w} × {h} pixels is too large"));
    }
    let samples = match depth {
        8 => Samples::U8([rgb[0], rgb[1], rgb[2], 1.0].map(|v| (v * 255.0 + 0.5) as u8).repeat(n)),
        32 => Samples::F32(
            [
                crate::compose::srgb_decode(rgb[0]),
                crate::compose::srgb_decode(rgb[1]),
                crate::compose::srgb_decode(rgb[2]),
                1.0,
            ]
            .repeat(n),
        ),
        _ => Samples::U16([rgb[0], rgb[1], rgb[2], 1.0].map(|v| (v * 65535.0 + 0.5) as u16).repeat(n)),
    };
    Ok(Raster { width: w, height: h, channels: 4, samples })
}

//endregion

#[cfg(test)]
pub mod tests {
    use super::*;

    /// (name, rect, RGBA pixels, blend key, opacity, flags).
    pub type TestLayer<'a> = (&'a str, [i32; 4], Vec<[u8; 4]>, [u8; 4], u8, u8);

    /// Writes a small RGB(A) document: `layers` are (name, rect, RGBA
    /// pixels, blend key, opacity, flags), stored with RLE; the composite
    /// is raw.
    pub fn write(w: u32, h: u32, layers: &[TestLayer], merged: Option<&[[u8; 4]]>) -> Vec<u8> {
        let mut f = b"8BPS".to_vec();
        f.extend_from_slice(&1u16.to_be_bytes());
        f.extend_from_slice(&[0; 6]);
        f.extend_from_slice(&4u16.to_be_bytes());
        f.extend_from_slice(&h.to_be_bytes());
        f.extend_from_slice(&w.to_be_bytes());
        f.extend_from_slice(&8u16.to_be_bytes());
        f.extend_from_slice(&3u16.to_be_bytes());
        f.extend_from_slice(&0u32.to_be_bytes());
        f.extend_from_slice(&0u32.to_be_bytes());
        // Layer info.
        let mut info = Vec::new();
        info.extend_from_slice(&(-(layers.len() as i16)).to_be_bytes());
        let mut pixel_data = Vec::new();
        for (name, rect, px, key, opacity, flags) in layers {
            for v in rect {
                info.extend_from_slice(&v.to_be_bytes());
            }
            let (lw, lh) = ((rect[3] - rect[1]) as usize, (rect[2] - rect[0]) as usize);
            info.extend_from_slice(&4u16.to_be_bytes());
            let mut chans = Vec::new();
            for (id, c) in [(-1i16, 3usize), (0, 0), (1, 1), (2, 2)] {
                let mut data = 1u16.to_be_bytes().to_vec();
                let rows: Vec<Vec<u8>> = (0..lh)
                    .map(|y| {
                        let row: Vec<u8> = (0..lw).map(|x| px[y * lw + x][c]).collect();
                        let mut packed = Vec::new();
                        for chunk in row.chunks(128) {
                            packed.push((chunk.len() - 1) as u8);
                            packed.extend_from_slice(chunk);
                        }
                        packed
                    })
                    .collect();
                for r in &rows {
                    data.extend_from_slice(&(r.len() as u16).to_be_bytes());
                }
                rows.iter().for_each(|r| data.extend_from_slice(r));
                chans.push((id, data));
            }
            for (id, data) in &chans {
                info.extend_from_slice(&id.to_be_bytes());
                info.extend_from_slice(&(data.len() as u32).to_be_bytes());
            }
            info.extend_from_slice(b"8BIM");
            info.extend_from_slice(key);
            info.extend_from_slice(&[*opacity, 0, *flags, 0]);
            let mut extra = Vec::new();
            extra.extend_from_slice(&0u32.to_be_bytes());
            extra.extend_from_slice(&0u32.to_be_bytes());
            let mut pname = vec![name.len() as u8];
            pname.extend_from_slice(name.as_bytes());
            while !pname.len().is_multiple_of(4) {
                pname.push(0);
            }
            extra.extend(pname);
            info.extend_from_slice(&(extra.len() as u32).to_be_bytes());
            info.extend(extra);
            chans.into_iter().for_each(|(_, d)| pixel_data.extend(d));
        }
        info.extend(pixel_data);
        if info.len() % 2 == 1 {
            info.push(0);
        }
        let mut section = (info.len() as u32).to_be_bytes().to_vec();
        section.extend(info);
        section.extend_from_slice(&0u32.to_be_bytes());
        f.extend_from_slice(&(section.len() as u32).to_be_bytes());
        f.extend(section);
        // The composite, raw, planar: R, G, B, A.
        f.extend_from_slice(&0u16.to_be_bytes());
        let blank = vec![[255u8; 4]; (w * h) as usize];
        let m = merged.unwrap_or(&blank);
        for c in [0, 1, 2, 3] {
            f.extend(m.iter().map(|p| p[c]));
        }
        f
    }

    #[test]
    fn layers_and_composite() {
        let red = vec![[255, 0, 0, 255]; 16];
        let blue = vec![[0, 0, 255, 128]; 4];
        let merged = vec![[10, 20, 30, 255]; 16];
        let f = write(
            4,
            4,
            &[("bg", [0, 0, 4, 4], red, *b"norm", 255, 0), ("Blue", [1, 1, 3, 3], blue, *b"mul ", 255, 0)],
            Some(&merged),
        );
        let psd = Psd::parse(Arc::new(f)).unwrap();
        assert_eq!((psd.width, psd.height, psd.layers.len()), (4, 4, 2));
        assert_eq!(psd.layers[1].name, "Blue");
        let (w, h, s) = psd.merged(1).unwrap();
        assert_eq!((w, h), (4, 4));
        let Samples::U8(v) = &s else { panic!() };
        assert_eq!(&v[0..4], &[10, 20, 30, 255]);
        let l = psd.layer(1).unwrap();
        assert_eq!((l.width, l.height), (2, 2));
        let Samples::U8(v) = &l.samples else { panic!() };
        assert_eq!(&v[0..4], &[0, 0, 255, 128]);
        let mut notes = Vec::new();
        let tree = psd.tree(&mut notes);
        assert_eq!(tree.len(), 2);
        assert_eq!(tree[1].mode, Mode::Multiply);
        // Every other pixel each way.
        let (w, h, _) = psd.merged(2).unwrap();
        assert_eq!((w, h), (2, 2));
    }

    #[test]
    fn packbits_and_prediction() {
        let mut out = Vec::new();
        unpackbits(&[2, 1, 2, 3, 0xfd, 9, 0x80], &mut out, 7);
        assert_eq!(out, [1, 2, 3, 9, 9, 9, 9]);
        let mut out = Vec::new();
        unpackbits(&[5, 1], &mut out, 4);
        assert_eq!(out, [1, 0, 0, 0]);
    }

    #[test]
    fn damaged_documents_never_panic() {
        let red = vec![[255, 0, 0, 255]; 64];
        let f = write(
            8,
            8,
            &[("a", [0, 0, 8, 8], red.clone(), *b"norm", 255, 0), ("b", [2, 2, 10, 10], red, *b"scrn", 99, 2)],
            None,
        );
        for cut in (0..f.len()).step_by(3) {
            if let Ok(psd) = Psd::parse(Arc::new(f[..cut].to_vec())) {
                let _ = psd.merged(1);
                for i in 0..psd.layers.len() {
                    let _ = psd.layer(i);
                }
            }
        }
        let mut seed = 7u32;
        for _ in 0..500 {
            let mut bad = f.clone();
            for _ in 0..3 {
                seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12345);
                let i = (seed >> 8) as usize % bad.len();
                bad[i] = (seed >> 3) as u8;
            }
            if let Ok(psd) = Psd::parse(Arc::new(bad)) {
                let _ = psd.merged(1);
                for i in 0..psd.layers.len().min(4) {
                    let _ = psd.layer(i);
                    let _ = psd.mask(i);
                }
                let _ = psd.tree(&mut Vec::new());
            }
        }
    }
}
