//! XISF (Extensible Image Serialization Format), PixInsight's image file.
//!
//! A monolithic XISF file is the signature `XISF0100`, the length of an
//! XML header, the header, and then the data blocks the header points to.
//! The header describes each image (size, sample type, colour space,
//! storage order, where its block is and how it is compressed), the FITS
//! keywords and properties that came with it, and the colour filter array
//! of mosaiced raw frames.
//!
//! Read here: the first image (the others are the file's parts) in Gray and
//! RGB colour spaces, planar or normal pixel storage, 8 to 64-bit integer
//! and 32 or 64-bit float samples in either byte order, attached, embedded
//! and inline blocks, and the standard codecs zlib, lz4, lz4hc and zstd
//! with or without byte shuffling, with or without subblocks.  Integer
//! samples are scaled to 0..1, as XISF defines their range; the picture is
//! stretched for display like any linear data (see [`crate::stretch`]).
//! Files whose blocks live in other files (distributed units) are refused
//! with a message.

use crate::bayer::{self, BayerReducer, Pattern};
use crate::formats::channel_tag;
use crate::reduce::{Fill, Raster, Reducer, factor_for, rows_per_read};
use crate::thumb::Channels;
use crate::util::{MAX_DECODED_BYTES, Stream, invalid, read_fully, too_large, unsupported};
use crate::{ColorSpace, DecodeOptions, Level, Pixels, SourceInfo, Texture, TextureError, TextureKind};
use rayon::prelude::*;
use roxmltree::{Document, Node};
use std::io::SeekFrom;

pub(crate) const SIGNATURE: &[u8] = b"XISF0100";

/// The largest XML header read; real ones are a few kilobytes, with an
/// embedded thumbnail some hundreds.
const MAX_HEADER_BYTES: u64 = 64 << 20;

/// Most pictures (header cards and properties) listed in the inspector.
const MAX_LISTED: usize = 600;

pub(crate) fn sniff(head: &[u8]) -> bool {
    head.starts_with(SIGNATURE)
}

/*┌─────────────────╮
| Header          |
└─────────────────╯*/
//region Header

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Sample {
    U8,
    U16,
    U32,
    U64,
    F32,
    F64,
}

impl Sample {
    fn parse(s: &str) -> Option<Sample> {
        Some(match s {
            "UInt8" => Sample::U8,
            "UInt16" => Sample::U16,
            "UInt32" => Sample::U32,
            "UInt64" => Sample::U64,
            "Float32" => Sample::F32,
            "Float64" => Sample::F64,
            _ => return None,
        })
    }

    fn bytes(self) -> usize {
        match self {
            Sample::U8 => 1,
            Sample::U16 => 2,
            Sample::U32 | Sample::F32 => 4,
            Sample::U64 | Sample::F64 => 8,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Sample::U8 => "8-bit",
            Sample::U16 => "16-bit",
            Sample::U32 => "32-bit integer",
            Sample::U64 => "64-bit integer",
            Sample::F32 => "32-bit float",
            Sample::F64 => "64-bit float",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Codec {
    Zlib,
    Lz4,
    Zstd,
}

/// How a block is compressed: the codec, whether bytes are shuffled first,
/// the uncompressed size, the size of one item for shuffling, and the
/// independent subblocks (compressed, uncompressed) when listed.
#[derive(Clone, Debug)]
struct Compression {
    codec: Codec,
    shuffle: bool,
    size: u64,
    item: u64,
    subblocks: Vec<(u64, u64)>,
}

/// Where a block's bytes are.
#[derive(Clone, Debug)]
enum Location {
    Attached { position: u64, size: u64 },
    Inline(Vec<u8>),
}

struct Image<'a, 'b> {
    node: Node<'a, 'b>,
    width: u32,
    height: u32,
    channels: u32,
    sample: Sample,
    big_endian: bool,
    planar: bool,
    rgb: bool,
    location: Location,
    compression: Option<Compression>,
    cfa: Option<Pattern>,
    orientation: String,
}

fn attr<'a>(n: Node<'a, '_>, name: &str) -> Option<&'a str> {
    n.attribute(name)
}

fn parse_u64(s: &str) -> Result<u64, TextureError> {
    s.trim().parse().map_err(|_| invalid(format!("XISF: bad number {s:?}")))
}

fn parse_compression(text: &str, subblocks: Option<&str>) -> Result<Compression, TextureError> {
    let mut parts = text.split(':');
    let name = parts.next().unwrap_or("");
    let (codec, shuffle) = match name {
        "zlib" => (Codec::Zlib, false),
        "zlib+sh" => (Codec::Zlib, true),
        "lz4" | "lz4hc" => (Codec::Lz4, false),
        "lz4+sh" | "lz4hc+sh" => (Codec::Lz4, true),
        "zstd" => (Codec::Zstd, false),
        "zstd+sh" => (Codec::Zstd, true),
        other => return Err(unsupported(format!("XISF compression {other:?}"))),
    };
    let size = parse_u64(parts.next().ok_or_else(|| invalid("XISF: compression without a size"))?)?;
    // Shuffled codecs name the size of one item, which shuffling needs.
    let item = match (parts.next(), shuffle) {
        (Some(i), _) => parse_u64(i)?,
        (None, true) => return Err(invalid("XISF: shuffled data without an item size")),
        (None, false) => 1,
    };
    if shuffle && item == 0 {
        return Err(invalid("XISF: shuffled data with an item size of 0"));
    }
    let mut list = Vec::new();
    if let Some(sb) = subblocks {
        for part in sb.split(':').take(1 << 20) {
            let (c, u) = part.split_once(',').ok_or_else(|| invalid("XISF: bad subblocks"))?;
            list.push((parse_u64(c)?, parse_u64(u)?));
        }
    }
    Ok(Compression { codec, shuffle, size, item, subblocks: list })
}

fn parse_location(node: Node, header_len: u64) -> Result<Location, TextureError> {
    let _ = header_len;
    let loc = attr(node, "location").ok_or_else(|| invalid("XISF: image without a location"))?;
    if let Some(rest) = loc.strip_prefix("attachment:") {
        let (pos, size) = rest.split_once(':').ok_or_else(|| invalid("XISF: bad attachment"))?;
        return Ok(Location::Attached { position: parse_u64(pos)?, size: parse_u64(size)? });
    }
    let encoding_of = |n: Node| attr(n, "encoding").map(str::to_string);
    let (text, encoding) = if let Some(enc) = loc.strip_prefix("inline:") {
        (node.text().unwrap_or("").to_string(), enc.to_string())
    } else if loc == "embedded" {
        let data = node.children().find(|c| c.has_tag_name("Data")).ok_or_else(|| invalid("XISF: no Data element"))?;
        (data.text().unwrap_or("").to_string(), encoding_of(data).unwrap_or_default())
    } else {
        return Err(unsupported("XISF data stored outside the file (a distributed unit)"));
    };
    let clean: String = text.chars().filter(|c| !c.is_whitespace()).collect();
    let bytes = match encoding.as_str() {
        "base64" => {
            use base64::Engine;
            base64::engine::general_purpose::STANDARD
                .decode(clean.as_bytes())
                .map_err(|_| invalid("XISF: bad base64 data"))?
        }
        "hex" => {
            if !clean.len().is_multiple_of(2) {
                return Err(invalid("XISF: odd hex data"));
            }
            (0..clean.len() / 2)
                .map(|i| u8::from_str_radix(&clean[2 * i..2 * i + 2], 16).map_err(|_| invalid("XISF: bad hex data")))
                .collect::<Result<Vec<u8>, _>>()?
        }
        other => return Err(unsupported(format!("XISF encoding {other:?}"))),
    };
    Ok(Location::Inline(bytes))
}

fn images<'a, 'b>(doc: &'a Document<'b>, header_len: u64) -> Vec<Result<Image<'a, 'b>, TextureError>> {
    doc.root_element()
        .children()
        .filter(|n| n.has_tag_name("Image"))
        .take(4096)
        .map(|node| parse_image(node, header_len))
        .collect()
}

fn parse_image<'a, 'b>(node: Node<'a, 'b>, header_len: u64) -> Result<Image<'a, 'b>, TextureError> {
    let geometry = attr(node, "geometry").ok_or_else(|| invalid("XISF: image without a geometry"))?;
    let dims: Vec<u64> = geometry.split(':').map(parse_u64).collect::<Result<_, _>>()?;
    if dims.len() != 3 {
        return Err(unsupported(format!("XISF image geometry {geometry:?}")));
    }
    let (width, height, channels) = (dims[0], dims[1], dims[2]);
    if width == 0 || height == 0 || channels == 0 || width > 1 << 20 || height > 1 << 20 || channels > 1 << 12 {
        return Err(invalid(format!("XISF: geometry {geometry}")));
    }
    let sample_name = attr(node, "sampleFormat").unwrap_or("UInt8");
    let sample = Sample::parse(sample_name).ok_or_else(|| unsupported(format!("XISF samples {sample_name}")))?;
    let rgb = match attr(node, "colorSpace").unwrap_or("Gray") {
        "Gray" => false,
        "RGB" => true,
        other => return Err(unsupported(format!("XISF colour space {other}"))),
    };
    if rgb && channels < 3 {
        return Err(invalid("XISF: RGB image with fewer than three channels"));
    }
    let compression = match attr(node, "compression") {
        Some(c) => Some(parse_compression(c, attr(node, "subblocks"))?),
        None => None,
    };
    // For embedded data the compression is on the Data element.
    let compression = match compression {
        Some(c) => Some(c),
        None => node
            .children()
            .find(|c| c.has_tag_name("Data"))
            .and_then(|d| attr(d, "compression").map(|c| parse_compression(c, attr(d, "subblocks"))))
            .transpose()?,
    };
    let cfa = node.children().find(|c| c.has_tag_name("ColorFilterArray")).and_then(|c| {
        let (w, h) = (attr(c, "width")?.trim(), attr(c, "height")?.trim());
        (w == "2" && h == "2").then_some(())?;
        Pattern::parse(attr(c, "pattern")?, 0, 0)
    });
    Ok(Image {
        node,
        width: width as u32,
        height: height as u32,
        channels: channels as u32,
        sample,
        big_endian: attr(node, "byteOrder") == Some("big"),
        planar: attr(node, "pixelStorage") != Some("Normal"),
        rgb,
        location: parse_location(node, header_len)?,
        compression,
        cfa: if rgb || channels != 1 { None } else { cfa },
        orientation: attr(node, "orientation").unwrap_or("0").to_string(),
    })
}

//endregion

/*┌─────────────────╮
| Parts           |
└─────────────────╯*/
//region Parts

/// The file's header text.
fn read_header(r: &mut dyn Stream, len: u64) -> Result<(String, u64), TextureError> {
    let mut head = [0u8; 16];
    r.seek(SeekFrom::Start(0))?;
    if read_fully(r, &mut head)? < 16 || &head[..8] != SIGNATURE {
        return Err(invalid("XISF: bad signature"));
    }
    let header_len = u32::from_le_bytes([head[8], head[9], head[10], head[11]]) as u64;
    if header_len == 0 || header_len > MAX_HEADER_BYTES || 16 + header_len > len {
        return Err(invalid("XISF: bad header length"));
    }
    let mut xml = vec![0u8; header_len as usize];
    r.read_exact(&mut xml)?;
    let text = String::from_utf8(xml).map_err(|_| invalid("XISF: header is not UTF-8"))?;
    Ok((text, header_len))
}

fn parse_doc(text: &str) -> Result<Document<'_>, TextureError> {
    let opts = roxmltree::ParsingOptions { nodes_limit: 1 << 20, ..Default::default() };
    Document::parse_with_options(text, opts).map_err(|e| invalid(format!("XISF: bad header: {e}")))
}

/// Names the images of a file that holds several; empty for one.
pub(crate) fn parts(r: &mut dyn Stream, len: u64) -> Vec<String> {
    let Ok((text, header_len)) = read_header(r, len) else { return Vec::new() };
    let Ok(doc) = parse_doc(&text) else { return Vec::new() };
    let list = images(&doc, header_len);
    if list.iter().filter(|i| i.is_ok()).count() < 2 {
        return Vec::new();
    }
    list.iter()
        .enumerate()
        .filter_map(|(k, img)| {
            let img = img.as_ref().ok()?;
            let id = attr(img.node, "id").unwrap_or("image");
            Some(format!("{k}: {id} ({}x{})", img.width, img.height))
        })
        .collect()
}

//endregion

/*┌─────────────────╮
| Decoding        |
└─────────────────╯*/
//region Decoding

/// Decodes the first image, or the one a part name from [`parts`] picks.
pub(crate) fn decode(
    r: &mut dyn Stream,
    len: u64,
    opts: &DecodeOptions,
    part: Option<&str>,
) -> Result<Texture, TextureError> {
    let (text, header_len) = read_header(r, len)?;
    let doc = parse_doc(&text)?;
    let mut list = images(&doc, header_len);
    let index = match part.and_then(|p| p.split(':').next()).and_then(|n| n.trim().parse::<usize>().ok()) {
        Some(i) => i,
        None => list.iter().position(|i| i.is_ok()).unwrap_or(0),
    };
    if index >= list.len() {
        return Err(unsupported("XISF file holds no image"));
    }
    let img = list.swap_remove(index)?;

    // 1. Sizes.
    let (w, h, c) = (img.width as u64, img.height as u64, img.channels as u64);
    let sample_bytes = img.sample.bytes() as u64;
    let plane_bytes = w * h * sample_bytes;
    let total = plane_bytes.checked_mul(c).ok_or_else(|| too_large("XISF image"))?;
    if let Some(comp) = &img.compression {
        if comp.size != total {
            return Err(invalid("XISF: uncompressed size differs from the geometry"));
        }
        if total > MAX_DECODED_BYTES {
            return Err(too_large(format!("{} MiB of samples", total >> 20)));
        }
    }

    // 2. Where the samples come from: streamed from the file when stored
    //    plainly, otherwise decompressed (or decoded) into memory.
    let source = match (&img.location, &img.compression) {
        (Location::Attached { position, size }, None) => {
            // A file cut short still shows its rows so far.
            if *size < total {
                return Err(invalid("XISF: block smaller than the image"));
            }
            Source::File { start: *position }
        }
        (location, compression) => {
            let raw: Vec<u8> = match location {
                Location::Attached { position, size } => {
                    if position.saturating_add(*size) > len {
                        return Err(invalid("XISF: block runs past the end of the file"));
                    }
                    if *size > MAX_DECODED_BYTES {
                        return Err(too_large("XISF block"));
                    }
                    r.seek(SeekFrom::Start(*position))?;
                    let mut buf = vec![0u8; *size as usize];
                    r.read_exact(&mut buf)?;
                    buf
                }
                Location::Inline(bytes) => bytes.clone(),
            };
            let bytes = match compression {
                Some(comp) => decompress(&raw, comp)?,
                None => raw,
            };
            if (bytes.len() as u64) < total {
                return Err(invalid("XISF: block smaller than the image"));
            }
            Source::Memory(bytes)
        }
    };

    // 3. What to produce: a raster per output surface.
    let mut factor = factor_for(img.width, img.height, opts);
    while img.width.div_ceil(factor) as u64 * img.height.div_ceil(factor) as u64 * 16 > MAX_DECODED_BYTES / 2 {
        factor += 1;
    }
    let (ow, oh) = (img.width.div_ceil(factor), img.height.div_ceil(factor));
    let mut outputs: Vec<(Raster, Vec<(usize, Fill)>)> = Vec::new();
    if img.rgb {
        outputs.push((Raster::new(ow, oh), vec![(0, Fill::Red), (1, Fill::Green), (2, Fill::Blue)]));
    } else {
        let wanted = if opts.first_layer_only { 1 } else { img.channels as usize };
        let fit = (MAX_DECODED_BYTES / 2 / (ow as u64 * oh as u64 * 16)).max(1) as usize;
        for ch in 0..wanted.min(fit).min(1 << 12) {
            outputs.push((Raster::new(ow, oh), vec![(ch, Fill::Grey)]));
        }
    }

    // 4. Read the samples.
    let layout = Layout {
        w: img.width as usize,
        h: img.height as usize,
        c: img.channels as usize,
        sample: img.sample,
        big: img.big_endian,
        planar: img.planar,
    };
    if let Some(pattern) = img.cfa {
        let mut mosaic_out = outputs.remove(0).0;
        read_mosaic(r, &source, &layout, factor, pattern, &mut mosaic_out)?;
        outputs.insert(0, (mosaic_out, vec![(0, Fill::Grey)]));
    } else {
        read_channels(r, &source, &layout, factor, &mut outputs)?;
    }

    // 5. The texture.
    let mut rasters: Vec<Raster> = outputs.into_iter().map(|(ra, _)| ra).collect();
    for ra in &mut rasters {
        orient(ra, &img.orientation);
    }
    let (tw, th) = (rasters[0].width, rasters[0].height);
    let has_alpha = rasters.iter().any(|ra| ra.rgba.chunks_exact(4).any(|p| p[3] < 1.0));
    let layers = rasters.len() as u32;
    let mut format = img.sample.name().to_string();
    if img.rgb {
        format.push_str(", RGB");
    } else if let Some(p) = img.cfa {
        format.push_str(&format!(", {} mosaic", p.name()));
    }
    if let Some(comp) = &img.compression {
        format.push_str(&format!(
            ", {}{}",
            match comp.codec {
                Codec::Zlib => "zlib",
                Codec::Lz4 => "lz4",
                Codec::Zstd => "zstd",
            },
            if comp.shuffle { "+shuffle" } else { "" }
        ));
    }
    format.push_str(channel_tag(Channels::Stretch));
    Ok(Texture {
        kind: TextureKind::D2,
        layers,
        faces: 1,
        levels: vec![Level {
            width: tw,
            height: th,
            depth: 1,
            surfaces: rasters.into_iter().map(|ra| Pixels::Rgba32F(ra.rgba)).collect(),
        }],
        color_space: ColorSpace::Linear,
        has_alpha,
        source: SourceInfo {
            container: "XISF".into(),
            format,
            supercompression: img.compression.as_ref().map(|c| format!("{:?}", c.codec).to_lowercase()),
            width: img.width,
            height: img.height,
            depth: 1,
            layers: if img.rgb { 1 } else { img.channels },
            faces: 1,
            mip_count: 1,
            file_bytes: len,
            header: listed(&doc, &img),
            ..Default::default()
        },
    })
}

/// The header cards and properties the inspector lists.
fn listed(doc: &Document, img: &Image) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    if let Some(t) = attr(img.node, "imageType") {
        out.push(("imageType".into(), t.into()));
    }
    let scopes = [img.node, doc.root_element()];
    for scope in scopes {
        for n in scope.children().filter(|n| n.is_element()) {
            if out.len() >= MAX_LISTED {
                return out;
            }
            if n.has_tag_name("FITSKeyword") {
                let name = attr(n, "name").unwrap_or("");
                let mut value = attr(n, "value").unwrap_or("").trim().trim_matches('\'').trim().to_string();
                if let Some(c) = attr(n, "comment").filter(|c| !c.is_empty()) {
                    if value.is_empty() {
                        value = c.to_string();
                    } else {
                        value.push_str(" / ");
                        value.push_str(c);
                    }
                }
                out.push((name.to_string(), value));
            } else if n.has_tag_name("Property") {
                let id = attr(n, "id").unwrap_or("");
                // Only properties small enough to read as text.
                let value = attr(n, "value").map(str::to_string).or_else(|| n.text().map(|t| t.trim().to_string()));
                if let Some(v) = value.filter(|v| !v.is_empty() && v.len() < 200 && attr(n, "location").is_none()) {
                    out.push((id.to_string(), v));
                }
            }
        }
    }
    out
}

//endregion

/*┌─────────────────╮
| Blocks          |
└─────────────────╯*/
//region Blocks

fn unshuffle(data: &[u8], item: usize) -> Vec<u8> {
    if item <= 1 || data.len() < item {
        return data.to_vec();
    }
    let n = data.len() / item;
    let mut out = vec![0u8; data.len()];
    for b in 0..item {
        for i in 0..n {
            out[i * item + b] = data[b * n + i];
        }
    }
    // Bytes past the last whole item are stored as they are.
    out[n * item..].copy_from_slice(&data[n * item..]);
    out
}

/// The best ratio each codec can reach (zlib 1032 to 1, lz4 255 to 1, zstd's
/// run-length blocks 32768 to 1), so a few bytes can't claim gigabytes.
fn max_ratio(codec: Codec) -> usize {
    match codec {
        Codec::Zlib => 1100,
        Codec::Lz4 => 260,
        Codec::Zstd => 40000,
    }
}

fn inflate_one(codec: Codec, data: &[u8], size: usize) -> Result<Vec<u8>, TextureError> {
    if size > 1 << 24 && size / max_ratio(codec) > data.len() {
        return Err(invalid("XISF: compressed data too small for its claimed size"));
    }
    let out = match codec {
        Codec::Zlib => miniz_oxide::inflate::decompress_to_vec_zlib_with_limit(data, size)
            .map_err(|e| invalid(format!("XISF zlib: {e:?}")))?,
        Codec::Lz4 => lz4_flex::block::decompress(data, size).map_err(|e| invalid(format!("XISF lz4: {e}")))?,
        Codec::Zstd => zstd::bulk::decompress(data, size).map_err(|e| invalid(format!("XISF zstd: {e}")))?,
    };
    Ok(out)
}

/// Decompresses a block, then undoes the byte shuffling over the whole of it.
fn decompress(raw: &[u8], comp: &Compression) -> Result<Vec<u8>, TextureError> {
    let size = usize::try_from(comp.size).map_err(|_| too_large("XISF block"))?;
    let mut out: Vec<u8> = Vec::new();
    if comp.subblocks.is_empty() {
        out = inflate_one(comp.codec, raw, size)?;
    } else {
        let mut at = 0usize;
        for &(c, u) in &comp.subblocks {
            let (c, u) = (c as usize, u as usize);
            let end = at
                .checked_add(c)
                .filter(|&e| e <= raw.len())
                .ok_or_else(|| invalid("XISF: subblocks run past the data"))?;
            if out.len().saturating_add(u) > size {
                return Err(invalid("XISF: subblocks are larger than the block"));
            }
            out.extend(inflate_one(comp.codec, &raw[at..end], u)?);
            at = end;
        }
    }
    if out.len() != size {
        return Err(invalid("XISF: decompressed to the wrong size"));
    }
    Ok(if comp.shuffle { unshuffle(&out, comp.item as usize) } else { out })
}

enum Source {
    /// Stored plainly in the file, starting at this byte.
    File {
        start: u64,
    },
    Memory(Vec<u8>),
}

//endregion

/*┌─────────────────╮
| Samples         |
└─────────────────╯*/
//region Samples

struct Layout {
    w: usize,
    h: usize,
    c: usize,
    sample: Sample,
    big: bool,
    planar: bool,
}

/// Converts raw samples to floats: integers scaled to 0..1, floats as they
/// are, non-finite values undefined.
fn convert(raw: &[u8], sample: Sample, big: bool, out: &mut [f32]) {
    macro_rules! each {
        ($n:expr, $ty:ty, $f:expr) => {
            out.par_chunks_mut(1 << 16).zip(raw.par_chunks((1 << 16) * $n)).for_each(|(o, r)| {
                for (o, b) in o.iter_mut().zip(r.chunks_exact($n)) {
                    let arr: [u8; $n] = b.try_into().expect("exact chunk");
                    let v = if big { <$ty>::from_be_bytes(arr) } else { <$ty>::from_le_bytes(arr) };
                    *o = $f(v);
                }
            })
        };
    }
    match sample {
        Sample::U8 => out.iter_mut().zip(raw).for_each(|(o, &b)| *o = b as f32 * (1.0 / 255.0)),
        Sample::U16 => each!(2, u16, |v: u16| v as f32 * (1.0 / 65535.0)),
        Sample::U32 => each!(4, u32, |v: u32| (v as f64 / 4294967295.0) as f32),
        Sample::U64 => each!(8, u64, |v: u64| (v as f64 / 18446744073709551615.0) as f32),
        Sample::F32 => each!(4, f32, |v: f32| if v.is_finite() { v } else { f32::NAN }),
        Sample::F64 => each!(8, f64, |v: f64| if v.is_finite() { v as f32 } else { f32::NAN }),
    }
}

/// Hands `consume` the raw bytes of rows `0..rows` of a region starting
/// `offset` bytes into the samples, `row_bytes` each, in pieces of at most
/// `piece_rows` rows.  A file that ends early stops the reading quietly.
fn for_each_piece(
    r: &mut dyn Stream,
    source: &Source,
    offset: u64,
    row_bytes: usize,
    rows: usize,
    piece_rows: usize,
    mut consume: impl FnMut(&[u8], usize),
) -> Result<(), TextureError> {
    let piece_rows = piece_rows.clamp(1, rows.max(1));
    match source {
        Source::Memory(bytes) => {
            let mut row = 0;
            while row < rows {
                let n = piece_rows.min(rows - row);
                let start = offset as usize + row * row_bytes;
                let Some(piece) = bytes.get(start..start + n * row_bytes) else { break };
                consume(piece, n);
                row += n;
            }
        }
        Source::File { start } => {
            r.seek(SeekFrom::Start(start + offset))?;
            let mut buf = vec![0u8; row_bytes * piece_rows];
            let mut row = 0;
            while row < rows {
                let n = piece_rows.min(rows - row);
                let got = read_fully(r, &mut buf[..n * row_bytes])?;
                let whole = got / row_bytes;
                if whole > 0 {
                    consume(&buf[..whole * row_bytes], whole);
                }
                if got < n * row_bytes {
                    break;
                }
                row += n;
            }
        }
    }
    Ok(())
}

/// Reads the channels outputs need into their rasters, shrunk by `factor`.
fn read_channels(
    r: &mut dyn Stream,
    source: &Source,
    l: &Layout,
    factor: u32,
    outputs: &mut [(Raster, Vec<(usize, Fill)>)],
) -> Result<(), TextureError> {
    let sz = l.sample.bytes();
    let plane_bytes = (l.w * l.h * sz) as u64;
    // Every (output, channel, fill) the file must feed.
    let jobs: Vec<(usize, usize, Fill)> =
        outputs.iter().enumerate().flat_map(|(o, (_, parts))| parts.iter().map(move |&(c, f)| (o, c, f))).collect();
    if l.planar {
        for (o, channel, fill) in jobs {
            let mut red = Reducer::new(l.w as u32, l.h as u32, factor, false);
            let row_bytes = l.w * sz;
            let piece = rows_per_read(row_bytes, red.factor());
            let mut values: Vec<f32> = Vec::new();
            for_each_piece(r, source, plane_bytes * channel as u64, row_bytes, l.h, piece, |raw, n| {
                values.resize(n * l.w, 0.0);
                convert(raw, l.sample, l.big, &mut values);
                red.push_rows(&values, n, &mut outputs[o].0, fill);
            })?;
            red.finish(&mut outputs[o].0);
        }
    } else {
        let mut reds: Vec<Reducer> = jobs.iter().map(|_| Reducer::new(l.w as u32, l.h as u32, factor, false)).collect();
        let row_bytes = l.w * l.c * sz;
        let piece = rows_per_read(row_bytes, reds[0].factor());
        let mut all: Vec<f32> = Vec::new();
        let mut one: Vec<f32> = Vec::new();
        for_each_piece(r, source, 0, row_bytes, l.h, piece, |raw, n| {
            all.resize(n * l.w * l.c, 0.0);
            convert(raw, l.sample, l.big, &mut all);
            for (k, &(o, channel, fill)) in jobs.iter().enumerate() {
                one.clear();
                one.extend(all.iter().skip(channel).step_by(l.c).copied());
                reds[k].push_rows(&one, n, &mut outputs[o].0, fill);
            }
        })?;
        for (k, &(o, _, _)) in jobs.iter().enumerate() {
            reds[k].finish(&mut outputs[o].0);
        }
    }
    Ok(())
}

/// A mosaic (one channel with a colour filter array) into an RGB raster.
fn read_mosaic(
    r: &mut dyn Stream,
    source: &Source,
    l: &Layout,
    factor: u32,
    pattern: Pattern,
    out: &mut Raster,
) -> Result<(), TextureError> {
    let sz = l.sample.bytes();
    let row_bytes = l.w * sz;
    let mut values: Vec<f32> = Vec::new();
    if factor >= 2 {
        let mut red = BayerReducer::new(l.w as u32, l.h as u32, factor, false, pattern);
        let piece = rows_per_read(row_bytes, red.factor());
        for_each_piece(r, source, 0, row_bytes, l.h, piece, |raw, n| {
            values.resize(n * l.w, 0.0);
            convert(raw, l.sample, l.big, &mut values);
            red.push_rows(&values, n, out);
        })?;
        red.finish(out);
    } else {
        let mut mosaic: Vec<f32> = Vec::with_capacity(l.w * l.h);
        for_each_piece(r, source, 0, row_bytes, l.h, rows_per_read(row_bytes, 256), |raw, n| {
            values.resize(n * l.w, 0.0);
            convert(raw, l.sample, l.big, &mut values);
            mosaic.extend_from_slice(&values);
        })?;
        mosaic.resize(l.w * l.h, f32::NAN);
        bayer::demosaic(&mosaic, l.w, l.h, &pattern, out);
    }
    Ok(())
}

/// Applies the image's `orientation` attribute: a flip and/or a quarter
/// turn.
fn orient(ra: &mut Raster, orientation: &str) {
    let (flip, turn) = match orientation.trim() {
        "flip" => (true, 0),
        "90" => (false, 90),
        "90;flip" => (true, 90),
        "-90" => (false, -90),
        "-90;flip" => (true, -90),
        "180" => (false, 180),
        "180;flip" => (true, 180),
        _ => return,
    };
    let (w, h) = (ra.width as usize, ra.height as usize);
    let (nw, nh) = if turn == 90 || turn == -90 { (h, w) } else { (w, h) };
    let mut out = vec![0f32; ra.rgba.len()];
    for y in 0..h {
        for x in 0..w {
            // Rotate first (counter-clockwise for positive angles), then flip
            // the result horizontally.
            let (mut nx, ny) = match turn {
                90 => (y, w - 1 - x),
                -90 => (h - 1 - y, x),
                180 => (w - 1 - x, h - 1 - y),
                _ => (x, y),
            };
            if flip {
                nx = nw - 1 - nx;
            }
            out[(ny * nw + nx) * 4..(ny * nw + nx) * 4 + 4]
                .copy_from_slice(&ra.rgba[(y * w + x) * 4..(y * w + x) * 4 + 4]);
        }
    }
    ra.rgba = out;
    ra.width = nw as u32;
    ra.height = nh as u32;
    debug_assert_eq!(nw * nh * 4, ra.rgba.len());
}

//endregion

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shuffling_round_trips() {
        let data: Vec<u8> = (0..22).collect();
        // Shuffle by hand with item size 4: five whole items and 2 spare bytes.
        let n = 5;
        let mut shuffled = vec![0u8; 22];
        for i in 0..n {
            for b in 0..4 {
                shuffled[b * n + i] = data[i * 4 + b];
            }
        }
        shuffled[20..].copy_from_slice(&data[20..]);
        assert_eq!(unshuffle(&shuffled, 4), data);
        assert_eq!(unshuffle(&data, 1), data);
        assert_eq!(unshuffle(&data[..3], 4), &data[..3]);
    }

    #[test]
    fn compression_attributes() {
        let c = parse_compression("zlib+sh:1024:2", None).unwrap();
        assert_eq!((c.codec, c.shuffle, c.size, c.item), (Codec::Zlib, true, 1024, 2));
        let c = parse_compression("lz4hc:99", Some("10,20:30,40")).unwrap();
        assert_eq!((c.codec, c.shuffle, c.subblocks.clone()), (Codec::Lz4, false, vec![(10, 20), (30, 40)]));
        assert!(parse_compression("zstd+sh:10", None).is_err(), "shuffling needs an item size");
        assert!(parse_compression("bzip2:10", None).is_err());
        assert!(parse_compression("zlib", None).is_err());
    }

    #[test]
    fn orientation_turns() {
        let mut ra = Raster { width: 3, height: 2, rgba: (0..6).flat_map(|i| [i as f32, 0.0, 0.0, 1.0]).collect() };
        orient(&mut ra, "flip");
        assert_eq!(ra.rgba.iter().step_by(4).copied().collect::<Vec<_>>(), vec![2.0, 1.0, 0.0, 5.0, 4.0, 3.0]);
        let mut ra = Raster { width: 3, height: 2, rgba: (0..6).flat_map(|i| [i as f32, 0.0, 0.0, 1.0]).collect() };
        orient(&mut ra, "180");
        assert_eq!(ra.rgba.iter().step_by(4).copied().collect::<Vec<_>>(), vec![5.0, 4.0, 3.0, 2.0, 1.0, 0.0]);
        let mut ra = Raster { width: 3, height: 2, rgba: (0..6).flat_map(|i| [i as f32, 0.0, 0.0, 1.0]).collect() };
        orient(&mut ra, "90");
        assert_eq!((ra.width, ra.height), (2, 3));
        // Counter-clockwise: the top right pixel (2) goes to the top left.
        assert_eq!(ra.rgba.iter().step_by(4).copied().collect::<Vec<_>>(), vec![2.0, 5.0, 1.0, 4.0, 0.0, 3.0]);
    }
}
