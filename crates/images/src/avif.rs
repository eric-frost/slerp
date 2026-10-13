//! AVIF: AV1 pictures in an ISO base media file.  Stills are items (one
//! picture, or a grid of tiles stitched together, with an optional second
//! item for transparency); animations are an image sequence track (with
//! the alpha plane in a second track).  8, 10 and 12-bit pictures in
//! 4:0:0, 4:2:0, 4:2:2 and 4:4:4 are read, with the colour description the
//! file gives (CICP code points or an ICC profile) and the rotation,
//! mirroring and cropping its properties ask for.
//!
//! The container is [`crate::isobmff`]; the AV1 decoding itself is behind
//! [`crate::av1`], and conversion to RGB is [`crate::yuv`].

use crate::animation::{Animation, Blend, Dispose, MAX_CANVAS_PIXELS, Patch, PatchSource, Rect, delay_seconds};
use crate::av1;
use crate::color::{Conversion, describe_icc, primaries_name, tone_map, transfer_name};
use crate::isobmff::{Colour, Heif, Property, Track, ratio};
use crate::orient::orient;
use crate::yuv::{Cicp, Rgba, Yuv, to_rgb};
use slerp_isolate::{Job, Opened, Worker};
use slerp_scene::{ColorSpace, Texture};
use std::sync::Arc;

/// The AV1 decoding, which runs isolated (see `slerp-isolate`): rav1d is
/// Rust, but its only interface is dav1d's C one, through which a panic on
/// a damaged file aborts the whole process.  Both take the file's bytes.
const DECODER: &str = "the AV1 decoder";
pub(crate) static STILL_JOB: Job = Job::once("avif.still", DECODER, still_job);
pub(crate) static SEQUENCE_JOB: Job = Job::serving("avif.sequence", DECODER, sequence_job);

/// The most pixels of a still picture opened.
pub const MAX_PIXELS: u64 = 160 << 20;

/// What opening a still gave.
pub struct Still {
    pub texture: Texture,
}

/// Which codec a HEIF file's primary picture uses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Codec {
    Av1,
    /// HEVC and anything else libheif decodes.
    Other,
}

/// The codec of the primary item of a file, looking through grids and
/// derived images to their tiles.
pub fn codec_of(h: &Heif) -> Option<Codec> {
    let mut id = h.primary?;
    for _ in 0..4 {
        let kind = h.item(id)?.kind;
        match &kind {
            b"av01" => return Some(Codec::Av1),
            b"grid" | b"iden" | b"iovl" => id = *h.refs_from(b"dimg", id).first()?,
            _ => return Some(Codec::Other),
        }
    }
    None
}

/*┌─────────────────╮
| Stills          |
└─────────────────╯*/
//region Stills

/// How an image item decodes: its AV1 configuration and data, or a grid.
fn item_yuv(h: &Heif, file: &[u8], id: u32, depth: usize, budget: &mut u64) -> Result<Yuv, String> {
    if depth > 3 {
        return Err("derived images are nested too deeply".into());
    }
    let item = h.item(id).ok_or("a picture refers to an item that isn't there")?;
    match &item.kind {
        b"av01" => {
            let config = h
                .properties_of(id)
                .into_iter()
                .find_map(|p| match p {
                    Property::Av1c { obus, .. } => Some(obus.clone()),
                    _ => None,
                })
                .unwrap_or_default();
            let data = h.item_data(file, id)?;
            let (w, hh) = h
                .properties_of(id)
                .into_iter()
                .find_map(|p| match p {
                    Property::Ispe { width, height } => Some((*width as u64, *height as u64)),
                    _ => None,
                })
                .unwrap_or((0, 0));
            let px = w * hh;
            if px > MAX_PIXELS {
                return Err(format!("{w} × {hh} pixels is more than is opened ({} megapixels)", MAX_PIXELS >> 20));
            }
            *budget = budget.checked_sub(px).ok_or("the picture's tiles are more than is opened")?;
            let operating_point = h
                .properties_of(id)
                .into_iter()
                .find_map(|p| match p {
                    Property::A1op(i) => Some(*i),
                    _ => None,
                })
                .unwrap_or(0);
            av1::decode_still(&config, &data, MAX_PIXELS, operating_point)
        }
        b"grid" => grid_yuv(h, file, id, depth, budget, false),
        b"iden" => {
            let src = *h.refs_from(b"dimg", id).first().ok_or("an identity image has no source")?;
            item_yuv(h, file, src, depth + 1, budget)
        }
        other => Err(format!("{} items aren't AV1", String::from_utf8_lossy(other))),
    }
}

/// A grid item's picture: its tiles side by side.  With `alpha_tiles` the
/// opacity of the grid instead, from the alpha item of every tile, for
/// files whose colour is a grid but whose alpha is attached to the tiles
/// rather than to the grid.
fn grid_yuv(h: &Heif, file: &[u8], id: u32, depth: usize, budget: &mut u64, alpha_tiles: bool) -> Result<Yuv, String> {
    let d = h.item_data(file, id)?;
    let (rows, cols, ow, oh) = parse_grid(&d)?;
    let tiles = h.refs_from(b"dimg", id);
    if tiles.len() != rows * cols {
        return Err(format!("a {cols} by {rows} grid has {} tiles", tiles.len()));
    }
    if ow as u64 * oh as u64 > MAX_PIXELS {
        return Err(format!("{ow} × {oh} pixels is more than is opened"));
    }
    let mut out: Option<Yuv> = None;
    let (mut x, mut y) = (0u32, 0u32);
    let (mut tw0, mut th0) = (0u32, 0u32);
    for (i, tile) in tiles.iter().enumerate() {
        let source = if alpha_tiles { h.alpha_of(*tile).ok_or("a tile has no alpha")? } else { *tile };
        let t = item_yuv(h, file, source, depth + 1, budget)?;
        if i == 0 {
            (tw0, th0) = (t.width, t.height);
            out = Some(Yuv::blank(ow, oh, t.bit_depth, t.layout, t.cicp).ok_or("the grid is too large")?);
        }
        let target = out.as_mut().ok_or("a grid has no tiles")?;
        // Every tile has the first one's size; the last ones may hang over the edge.
        if (t.width, t.height) != (tw0, th0) {
            return Err("the tiles of a grid differ in size".into());
        }
        target.blit(&t, x, y)?;
        x += tw0;
        if (i + 1) % cols == 0 {
            x = 0;
            y += th0;
        }
    }
    out.ok_or_else(|| "a grid has no tiles".to_string())
}

/// A grid item's descriptor: rows, columns and the output size.
fn parse_grid(d: &[u8]) -> Result<(usize, usize, u32, u32), String> {
    let bad = || "a grid descriptor is cut short".to_string();
    let flags = *d.get(1).ok_or_else(bad)?;
    let rows = *d.get(2).ok_or_else(bad)? as usize + 1;
    let cols = *d.get(3).ok_or_else(bad)? as usize + 1;
    let (w, h) = if flags & 1 != 0 {
        let b = d.get(4..12).ok_or_else(bad)?;
        (
            u32::from_be_bytes(b[..4].try_into().unwrap_or([0; 4])),
            u32::from_be_bytes(b[4..].try_into().unwrap_or([0; 4])),
        )
    } else {
        let b = d.get(4..8).ok_or_else(bad)?;
        (u16::from_be_bytes([b[0], b[1]]) as u32, u16::from_be_bytes([b[2], b[3]]) as u32)
    };
    if w == 0 || h == 0 {
        return Err("a grid has no size".into());
    }
    Ok((rows, cols, w, h))
}

/// Whether `id` is a grid of tiles that each have an alpha item of their
/// own (never one item serving two tiles, which libavif and libheif both
/// treat as no alpha at all), with none for the grid as a whole.
fn tiles_have_alpha(h: &Heif, id: u32) -> bool {
    let is_grid = h.item(id).is_some_and(|i| &i.kind == b"grid");
    let alphas: Vec<Option<u32>> = h.refs_from(b"dimg", id).iter().map(|&t| h.alpha_of(t)).collect();
    let mut seen = std::collections::HashSet::new();
    is_grid && !alphas.is_empty() && alphas.iter().all(|a| a.is_some_and(|a| seen.insert(a)))
}

/// Opens the primary picture of an AVIF file as a texture.
pub fn load_still(h: &Heif, file: &[u8]) -> Result<Still, String> {
    let id = h.primary.ok_or("the file has no primary picture")?;
    // The AV1 decoding runs in a child process, which sends back the planes.
    let pixels = h
        .properties_of(id)
        .into_iter()
        .find_map(|p| match p {
            Property::Ispe { width, height } => Some(*width as u64 * *height as u64),
            _ => None,
        })
        .unwrap_or(MAX_PIXELS)
        .min(MAX_PIXELS);
    let wire = slerp_isolate::run(&STILL_JOB, file, limits_for(pixels))?;
    let (pic, alpha_pic) = decode_pair(&wire)?;
    let premultiplied = h.alpha_is_premultiplied(id);
    let props: Vec<Property> = h.properties_of(id).into_iter().cloned().collect();
    let mut tex = render(pic, alpha_pic, premultiplied, &props)?;
    tex.source.container = "AVIF".into();
    Ok(Still { texture: tex })
}

/// The child's side of [`load_still`]: the primary picture and its alpha,
/// decoded from the file.
fn still_job(file: &[u8]) -> Result<Vec<u8>, String> {
    let h = Heif::parse(file)?;
    let id = h.primary.ok_or("the file has no primary picture")?;
    let mut budget = MAX_PIXELS;
    let pic = item_yuv(&h, file, id, 0, &mut budget)?;
    // Transparency: a second image item (or grid) of one channel.
    let alpha = match h.alpha_of(id) {
        Some(a) => Some(item_yuv(&h, file, a, 0, &mut budget)?),
        None if tiles_have_alpha(&h, id) => Some(grid_yuv(&h, file, id, 0, &mut budget, true)?),
        None => None,
    };
    Ok(encode_pair(&pic, alpha.as_ref()))
}

/// Converts a decoded picture (with its alpha) to a texture, and applies
/// the item's properties.
fn render(mut pic: Yuv, alpha: Option<Yuv>, premultiplied: bool, props: &[Property]) -> Result<Texture, String> {
    let mut header: Vec<(String, String)> = Vec::new();
    // The container's colour description wins over the stream's.
    let mut icc: Option<&Vec<u8>> = None;
    for p in props {
        match p {
            Property::Colr(Colour::Nclx { primaries, transfer, matrix, full_range }) => {
                pic.cicp = Cicp {
                    primaries: *primaries as u8,
                    transfer: *transfer as u8,
                    matrix: *matrix as u8,
                    full_range: *full_range,
                };
            }
            Property::Colr(Colour::Icc(profile)) => icc = Some(profile),
            _ => {}
        }
    }
    let (w, h) = (pic.width, pic.height);
    let layout = pic.layout;
    let depth = pic.bit_depth;
    let cicp = pic.cicp.resolved();
    header.push(("Bit depth".into(), format!("{depth} bits per channel")));
    header.push(("Chroma".into(), layout.name().into()));
    header.push(("Range".into(), if cicp.full_range { "full".into() } else { "limited (studio)".to_string() }));
    header.push(("Matrix".into(), matrix_name(cicp.matrix)));
    let opacity = alpha.as_ref().map(|a| (a.opacity(), a.bit_depth));
    let alpha_plane = opacity.as_ref().map(|(o, d)| (&o[..], *d));
    // An ICC profile that is a plain matrix and curve profile replaces the code points.
    let icc_conversion = icc.and_then(|p| Conversion::from_icc(p));
    if let (Some(p), None) = (icc, &icc_conversion) {
        header.push((
            "Colour profile".into(),
            format!("{}, not applied (shown through the code points below)", describe_icc(p)),
        ));
    }
    let converted = if icc_conversion.is_some() {
        // Decode the YUV as sRGB-coded colour, then let the profile convert it.
        let mut plain = pic.clone();
        plain.cicp = Cicp { primaries: 1, transfer: 13, matrix: cicp.matrix, full_range: cicp.full_range };
        to_rgb(&plain, alpha_plane, premultiplied)?
    } else {
        to_rgb(&pic, alpha_plane, premultiplied)?
    };
    let mut pixels = converted.pixels;
    if let (Some(c), Rgba::U8(px)) = (&icc_conversion, &mut pixels) {
        if c.is_srgb() {
            header.push(("Colour profile".into(), format!("{}, already sRGB", c.name)));
        } else {
            c.convert_rgba8(px);
            header.push(("Colour profile".into(), format!("{}, converted to sRGB", c.name)));
        }
    } else {
        header.push((
            "Colour".into(),
            format!(
                "{} primaries, {} transfer ({})",
                primaries_name(cicp.primaries),
                transfer_name(cicp.transfer),
                converted.note
            ),
        ));
    }
    // Transformations, in the order the file lists them.
    let (mut w, mut h) = (w as usize, h as usize);
    for p in props {
        match p {
            Property::Clap { width, height, h_off, v_off } => {
                if let Some((x0, y0, nw, nh)) = clap_rect(w, h, *width, *height, *h_off, *v_off)
                    && (nw, nh) != (w, h)
                {
                    pixels = crop(pixels, w, h, x0, y0, nw, nh);
                    header.push(("Cropped".into(), format!("to {nw} × {nh} (clean aperture)")));
                    (w, h) = (nw, nh);
                }
            }
            Property::Irot(a) if *a != 0 => {
                // Quarter turns anti-clockwise: EXIF 8 is one, 3 two, 6 three.
                let o = match a {
                    1 => 8,
                    2 => 3,
                    _ => 6,
                };
                let (px, nw, nh) = apply_orient(pixels, w, h, o);
                pixels = px;
                (w, h) = (nw, nh);
                header.push(("Rotation".into(), format!("{} degrees anti-clockwise, applied", *a as u32 * 90)));
            }
            Property::Imir(axis) => {
                // As libavif and libheif read it: axis 0 swaps top and bottom, axis 1 left and right.
                let o = if *axis == 0 { 4 } else { 2 };
                let (px, nw, nh) = apply_orient(pixels, w, h, o);
                pixels = px;
                (w, h) = (nw, nh);
                header.push((
                    "Mirror".into(),
                    if *axis == 0 { "left to right, applied".into() } else { "top to bottom, applied".to_string() },
                ));
            }
            _ => {}
        }
    }
    let (w32, h32) = (w as u32, h as u32);
    let mut tex = match pixels {
        Rgba::U8(px) => Texture::from_rgba8(w32, h32, px, ColorSpace::Srgb),
        Rgba::F32(px) => Texture::from_rgba32f(w32, h32, px),
    };
    tex.source.format = format!("{} {depth}-bit{}", layout.name(), if alpha.is_some() { " with alpha" } else { "" });
    tex.source.header = header;
    Ok(tex)
}

/// The rectangle of a `clap` box on a `w` by `h` picture: its width and
/// height, centred on the picture and moved by the offsets (fractions).
fn clap_rect(
    w: usize,
    h: usize,
    width: (u32, u32),
    height: (u32, u32),
    h_off: (i32, u32),
    v_off: (i32, u32),
) -> Option<(usize, usize, usize, usize)> {
    let (cw, ch) = (ratio(width.0, width.1), ratio(height.0, height.1));
    let hx = h_off.0 as f64 / h_off.1.max(1) as f64;
    let vy = v_off.0 as f64 / v_off.1.max(1) as f64;
    let x0 = (hx + (w as f64 - cw) / 2.0).round().max(0.0) as usize;
    let y0 = (vy + (h as f64 - ch) / 2.0).round().max(0.0) as usize;
    let nw = (cw.round() as usize).min(w.saturating_sub(x0));
    let nh = (ch.round() as usize).min(h.saturating_sub(y0));
    (nw > 0 && nh > 0 && x0 < w && y0 < h).then_some((x0, y0, nw, nh))
}

fn matrix_name(m: u8) -> String {
    match m {
        0 => "identity (GBR)".into(),
        1 => "BT.709".into(),
        5 | 6 => "BT.601".into(),
        9 => "BT.2020 non-constant".into(),
        10 => "BT.2020 constant".into(),
        2 => "unspecified (BT.601)".into(),
        n => format!("code {n}"),
    }
}

fn apply_orient(p: Rgba, w: usize, h: usize, o: u8) -> (Rgba, usize, usize) {
    match p {
        Rgba::U8(v) => {
            let (v, w, h) = orient(&v, w, h, 4, o);
            (Rgba::U8(v), w, h)
        }
        Rgba::F32(v) => {
            let (v, w, h) = orient(&v, w, h, 4, o);
            (Rgba::F32(v), w, h)
        }
    }
}

fn crop(p: Rgba, w: usize, _h: usize, x0: usize, y0: usize, nw: usize, nh: usize) -> Rgba {
    fn go<T: Copy>(v: &[T], w: usize, x0: usize, y0: usize, nw: usize, nh: usize) -> Vec<T> {
        let mut out = Vec::with_capacity(nw * nh * 4);
        for y in y0..y0 + nh {
            out.extend_from_slice(&v[(y * w + x0) * 4..(y * w + x0 + nw) * 4]);
        }
        out
    }
    match p {
        Rgba::U8(v) => Rgba::U8(go(&v, w, x0, y0, nw, nh)),
        Rgba::F32(v) => Rgba::F32(go(&v, w, x0, y0, nw, nh)),
    }
}

//endregion

/*┌─────────────────╮
| Sequences       |
└─────────────────╯*/
//region Sequences

/// The decoding side of an AVIF image sequence: AV1 samples are decoded in
/// order (a decoder can't jump), so asking for an earlier frame starts again
/// from the nearest sync sample.  It runs in an isolated worker
/// ([`SEQUENCE_JOB`]), since a damaged sample can make rav1d abort.
struct SequenceDecoder {
    file: Arc<Vec<u8>>,
    colour: Track,
    alpha: Option<Track>,
    decoder: Option<av1::Stream>,
    alpha_decoder: Option<av1::Stream>,
    /// The next sample the decoders will be given.
    next: usize,
}

impl SequenceDecoder {
    fn config_of(t: &Track) -> Vec<u8> {
        match &t.config {
            Some(Property::Av1c { obus, .. }) => obus.clone(),
            _ => Vec::new(),
        }
    }

    fn sample<'a>(file: &'a [u8], t: &Track, i: usize) -> Result<&'a [u8], String> {
        let &(off, size) = t.samples.get(i).ok_or("no such sample")?;
        let (off, size) = (usize::try_from(off).map_err(|_| "a sample lies outside the file")?, size as usize);
        file.get(off..off.checked_add(size).ok_or("a sample lies outside the file")?)
            .ok_or_else(|| "a sample lies outside the file".to_string())
    }

    /// Decodes samples up to and including `i`, from `self.next` on, and
    /// returns the picture of sample `i`.
    fn decode_to(&mut self, i: usize) -> Result<(Yuv, Option<Yuv>), String> {
        let file = self.file.clone();
        loop {
            let at = self.next;
            let data = Self::sample(&file, &self.colour, at)?;
            let dec = self.decoder.as_mut().ok_or("the decoder isn't started")?;
            let pic = dec.decode(data)?;
            let alpha = match (&self.alpha, self.alpha_decoder.as_mut()) {
                (Some(t), Some(d)) => d.decode(Self::sample(&file, t, at)?)?,
                _ => None,
            };
            self.next += 1;
            if at == i {
                return pic.map(|p| (p, alpha)).ok_or_else(|| "the decoder gave no picture for this frame".to_string());
            }
        }
    }

    fn restart(&mut self, from: usize) -> Result<(), String> {
        self.decoder = Some(av1::Stream::new(&Self::config_of(&self.colour), MAX_CANVAS_PIXELS, 0)?);
        self.alpha_decoder = match &self.alpha {
            Some(t) => Some(av1::Stream::new(&Self::config_of(t), MAX_CANVAS_PIXELS, 0)?),
            None => None,
        };
        self.next = from;
        Ok(())
    }

    /// The picture (and alpha) of frame `index`.
    fn frame(&mut self, index: usize) -> Result<(Yuv, Option<Yuv>), String> {
        if self.decoder.is_none() || index < self.next.saturating_sub(1) || index > self.next + 64 || index < self.next
        {
            // From the last sync sample at or before the frame.
            let from = if self.colour.sync.is_empty() {
                0
            } else {
                self.colour.sync.iter().rev().find(|&&s| s as usize <= index).copied().unwrap_or(0) as usize
            };
            self.restart(from)?;
        }
        self.decode_to(index)
    }

    /// Serves one request of the child process: a frame number in, the
    /// encoded picture and alpha out.
    fn serve(&mut self, request: &[u8]) -> Result<Vec<u8>, String> {
        let index = u32::from_le_bytes(request.try_into().map_err(|_| "a bad request")?) as usize;
        let (pic, alpha) = self.frame(index)?;
        Ok(encode_pair(&pic, alpha.as_ref()))
    }
}

/// The worker's side of a [`SequenceSource`]: reads the file's picture
/// track, then decodes each frame asked for.
fn sequence_job(file: &[u8]) -> Result<Opened, String> {
    let h = Heif::parse(file)?;
    let (colour, alpha) = h.picture_track().ok_or("the file has no picture track")?;
    let mut decoder = SequenceDecoder {
        file: Arc::new(file.to_vec()),
        colour: colour.clone(),
        alpha: alpha.cloned(),
        decoder: None,
        alpha_decoder: None,
        next: 0,
    };
    Ok((Box::new(move |request: &[u8]| decoder.serve(request)), Vec::new()))
}

/// A picture and its alpha as bytes, for passing between processes.
fn encode_pair(pic: &Yuv, alpha: Option<&Yuv>) -> Vec<u8> {
    let mut out = Vec::with_capacity(pic.y.len() * 6);
    pic.encode(&mut out);
    out.push(alpha.is_some() as u8);
    if let Some(a) = alpha {
        a.encode(&mut out);
    }
    out
}

fn decode_pair(bytes: &[u8]) -> Result<(Yuv, Option<Yuv>), String> {
    let bad = || "the decoder process sent a damaged picture".to_string();
    let mut rest = bytes;
    let pic = Yuv::decode(&mut rest).ok_or_else(bad)?;
    let (&flag, mut rest) = rest.split_first().ok_or_else(bad)?;
    let alpha = if flag == 1 { Some(Yuv::decode(&mut rest).ok_or_else(bad)?) } else { None };
    Ok((pic, alpha))
}

/// What the isolated decoder of pictures of `pixels` pixels may use: four
/// planes of 16-bit samples.
fn limits_for(pixels: u64) -> slerp_isolate::Limits {
    crate::limits_for_pixels(pixels.saturating_mul(8).min(usize::MAX as u64 / 2) as usize)
}

/// An AVIF image sequence as animation frames, decoded by a child process
/// that keeps its decoder between frames.
struct SequenceSource {
    /// The file, which a new child process starts from.
    file: Arc<Vec<u8>>,
    worker: Option<Worker>,
    cicp: Cicp,
    size: (u32, u32),
}

impl PatchSource for SequenceSource {
    fn patch(&mut self, index: usize) -> Result<Patch, String> {
        if !self.worker.as_ref().is_some_and(|w| w.alive()) {
            let limits = limits_for(self.size.0 as u64 * self.size.1 as u64);
            self.worker = Some(Worker::start(&SEQUENCE_JOB, &self.file, limits)?.0);
        }
        let worker = self.worker.as_mut().ok_or("the decoder process isn't running")?;
        let (mut pic, alpha) = decode_pair(&worker.ask(&(index as u32).to_le_bytes())?)?;
        if self.cicp != Cicp::UNSPECIFIED {
            pic.cicp = self.cicp;
        }
        let opacity = alpha.as_ref().map(|a| (a.opacity(), a.bit_depth));
        let converted = to_rgb(&pic, opacity.as_ref().map(|(o, d)| (&o[..], *d)), false)?;
        // Animations are 8-bit sRGB: HDR frames are tone mapped.
        let rgba = match converted.pixels {
            Rgba::U8(v) => v,
            Rgba::F32(v) => v
                .chunks_exact(4)
                .flat_map(|p| {
                    let c = tone_map([p[0], p[1], p[2]]);
                    [
                        crate::color::encode_srgb8(c[0]),
                        crate::color::encode_srgb8(c[1]),
                        crate::color::encode_srgb8(c[2]),
                        (p[3].clamp(0.0, 1.0) * 255.0 + 0.5) as u8,
                    ]
                })
                .collect(),
        };
        let (w, h) = (pic.width, pic.height);
        if (w, h) != self.size {
            return Err("frames of different sizes aren't supported".into());
        }
        Ok(Patch { rect: Rect { x: 0, y: 0, w, h }, rgba, blend: Blend::Replace, dispose: Dispose::None })
    }
}

/// Opens an AVIF image sequence as an animation, or `None` when the file
/// has no picture track.
pub fn load_sequence(h: &Heif, file: Vec<u8>, info: slerp_scene::Info) -> Option<Result<Animation, String>> {
    let colour = h.picture_track()?.0.clone();
    Some((|| {
        if colour.samples.is_empty() {
            return Err("the sequence has no frames".to_string());
        }
        if colour.samples.len() > crate::animation::MAX_FRAMES {
            return Err("the sequence has too many frames".to_string());
        }
        let (w, hh) = (colour.width, colour.height);
        if w == 0 || hh == 0 || w as u64 * hh as u64 > MAX_CANVAS_PIXELS {
            return Err(format!("{w} × {hh} pixels is more than animations show"));
        }
        let timescale = colour.timescale.max(1) as f32;
        let delays: Vec<f32> =
            colour.deltas.iter().map(|&d| delay_seconds((d as f32 / timescale * 1000.0).round() as u32)).collect();
        let plays = match colour.edit {
            Some((_, true)) | None => None,
            Some((segment, false)) => {
                let movie = h.timescale.max(1) as f64;
                let media = (colour.duration as f64 / colour.timescale.max(1) as f64).max(1e-6);
                let plays = (segment as f64 / movie / media).round().max(1.0);
                // A segment as long as the media plays once.
                Some(plays as u32)
            }
        };
        let cicp = match &colour.colour {
            Some(Colour::Nclx { primaries, transfer, matrix, full_range }) => Cicp {
                primaries: *primaries as u8,
                transfer: *transfer as u8,
                matrix: *matrix as u8,
                full_range: *full_range,
            },
            _ => Cicp::UNSPECIFIED,
        };
        let source = SequenceSource { file: Arc::new(file), worker: None, cicp, size: (w, hh) };
        Ok(Animation { info, size: (w, hh), delays, plays, source: Box::new(source) })
    })())
}

//endregion

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grid_descriptors() {
        // Version 0, 16-bit fields: 2 rows by 3 columns, 300 x 200.
        let mut d = vec![0, 0, 1, 2];
        d.extend(300u16.to_be_bytes());
        d.extend(200u16.to_be_bytes());
        assert_eq!(parse_grid(&d).unwrap(), (2, 3, 300, 200));
        // 32-bit fields.
        let mut d = vec![0, 1, 0, 0];
        d.extend(70000u32.to_be_bytes());
        d.extend(50000u32.to_be_bytes());
        assert_eq!(parse_grid(&d).unwrap(), (1, 1, 70000, 50000));
        assert!(parse_grid(&[0, 0, 1]).is_err());
        assert!(parse_grid(&[0, 0, 0, 0, 0, 0, 0, 0]).is_err());
    }

    #[test]
    fn cropping() {
        let px = Rgba::U8((0..16u8).flat_map(|i| [i, 0, 0, 255]).collect());
        let Rgba::U8(v) = crop(px, 4, 4, 1, 1, 2, 2) else { panic!() };
        assert_eq!(v.chunks_exact(4).map(|p| p[0]).collect::<Vec<_>>(), [5, 6, 9, 10]);
    }
}
