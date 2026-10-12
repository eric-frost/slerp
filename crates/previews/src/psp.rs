//! Jasc and Corel Paint Shop Pro images (`.psp`, `.pspimage`), picture
//! frames (`.pspframe`) and picture tubes (`.psptube`, `.tub`), all one
//! format, read as the published Paint Shop Pro file format specifications
//! (versions 5, 6, 7 and 8 from Jasc) describe it.
//!
//! The file is a 32-byte signature, a version (major 3 is Paint Shop Pro 5,
//! 4 is 6, 5 is 7, 6 is 8, and so on to 13 for PaintShop Pro 2020 and
//! later), and then blocks: `~BK\0`, a 16-bit id and a 32-bit length (major
//! 3 adds the length of the block's first chunk), with chunks that start
//! with their own size from major 4 on.  The pictures are in the composite
//! image bank (id 16): a list of attributes (size, depth, compression and
//! whether it is the full composite or a thumbnail), then the pictures in
//! the same order, each a JPEG block (id 18) or a composite image block (id
//! 9) of channel blocks (id 5).  Paint Shop Pro 5 files have a thumbnail
//! block (id 9) of their own instead.
//!
//! Channels are compressed one by one (none, run lengths, or zlib), one
//! byte per pixel and row after row; the specifications say rows are padded
//! to four bytes, which real files don't do, so the stored length decides.
//!
//! Thumbnails take the stored thumbnail when it is big enough, else the
//! full composite; the viewer takes the full composite.  Single-layer
//! documents without transparency often store only the thumbnail, so the
//! viewer then reads the one layer itself.

use crate::source::Source;
use slerp_scene::textures::Texture;
use slerp_scene::{Limits, LoadError};

pub const SIGNATURE: &[u8] = b"Paint Shop Pro Image File\n\x1a\0\0\0\0\0";

/// The largest channel, picture or block read, and the most pixels.
const MAX_BLOCK: u64 = 256 << 20;
const MAX_PIXELS: u64 = 1 << 28;
/// Blocks looked at in one list before giving up.
const MAX_BLOCKS: usize = 4096;

const IMAGE_BLOCK: u16 = 0;
const COLOR_BLOCK: u16 = 2;
const LAYER_START_BLOCK: u16 = 3;
const LAYER_BLOCK: u16 = 4;
const CHANNEL_BLOCK: u16 = 5;
const COMPOSITE_IMAGE_BLOCK: u16 = 9;
const COMPOSITE_BANK_BLOCK: u16 = 16;
const COMPOSITE_ATTRIBUTES_BLOCK: u16 = 17;
const JPEG_BLOCK: u16 = 18;

/// Channel bitmap types that are transparency.
const TRANSPARENCY: [u16; 3] = [1, 6, 9];

pub fn looks_like_psp(head: &[u8]) -> bool {
    head.starts_with(SIGNATURE)
}

fn bad(what: &str) -> LoadError {
    LoadError::invalid(format!("Paint Shop Pro file: {what}"))
}

fn u16_at(b: &[u8], i: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(i..i.checked_add(2)?)?.try_into().ok()?))
}

fn u32_at(b: &[u8], i: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(i..i.checked_add(4)?)?.try_into().ok()?))
}

/*┌─────────────────╮
| Blocks          |
└─────────────────╯*/
//region Blocks

/// A block: its id, where its body starts, how long it is, and (major 3)
/// the length of its first chunk.
#[derive(Clone, Copy, Debug)]
struct Block {
    id: u16,
    body: u64,
    len: u64,
    first_chunk: Option<u64>,
}

impl Block {
    fn end(&self) -> u64 {
        self.body.saturating_add(self.len)
    }
}

struct File<'a> {
    src: &'a mut Source,
    major: u16,
}

impl File<'_> {
    /// The blocks from `at` to `end`.
    fn blocks(&mut self, mut at: u64, end: u64) -> Result<Vec<Block>, LoadError> {
        let header = if self.major <= 3 { 14 } else { 10 };
        let mut out = Vec::new();
        while at.saturating_add(header) <= end && out.len() < MAX_BLOCKS {
            let h = self.src.exact(at, header)?;
            if &h[..4] != b"~BK\0" {
                break;
            }
            let id = u16_at(&h, 4).unwrap_or(u16::MAX);
            let (len, first_chunk) = if self.major <= 3 {
                (u32_at(&h, 10).unwrap_or(0) as u64, Some(u32_at(&h, 6).unwrap_or(0) as u64))
            } else {
                (u32_at(&h, 6).unwrap_or(0) as u64, None)
            };
            let block = Block { id, body: at + header, len, first_chunk };
            if block.end() > end {
                return Err(bad("a block runs past its parent"));
            }
            out.push(block);
            at = block.end();
        }
        Ok(out)
    }

    /// A block's first chunk, and where what follows it starts.
    fn chunk(&mut self, b: &Block) -> Result<(Vec<u8>, u64), LoadError> {
        let size = match b.first_chunk {
            Some(n) => n,
            None => u32_at(&self.src.exact(b.body, 4)?, 0).unwrap_or(0) as u64,
        };
        if size > b.len || size > 1 << 20 {
            return Err(bad("a chunk larger than its block"));
        }
        // From major 4 on, the size counts itself; the fields follow it.
        let skip = if b.first_chunk.is_some() { 0 } else { 4 };
        let bytes = self.src.exact(b.body + skip, size.saturating_sub(skip))?;
        Ok((bytes, b.body + size))
    }
}

/// What a channel holds.
struct Channel {
    bitmap: u16,
    channel: u16,
    data: Vec<u8>,
}

/// The general attributes: size, depth, compression, greyscale, layers.
struct Attributes {
    width: u32,
    height: u32,
    compression: u16,
    bits: u16,
    grey: bool,
    layers: u16,
}

//endregion

/*┌─────────────────╮
| Reading         |
└─────────────────╯*/
//region Reading

pub fn preview(src: &mut Source, limits: &Limits) -> Result<Texture, LoadError> {
    let head = src.head(36)?;
    if !looks_like_psp(&head) || head.len() < 36 {
        return Err(LoadError::NotMine);
    }
    let major = u16_at(&head, 32).unwrap_or(0);
    if !(3..=64).contains(&major) {
        return Err(LoadError::Unsupported(format!("Paint Shop Pro files of format version {major}")));
    }
    let len = src.len();
    let mut file = File { src, major };
    let blocks = file.blocks(36, len)?;
    // 1. The general attributes and the global palette.
    let image = blocks.iter().find(|b| b.id == IMAGE_BLOCK).ok_or_else(|| bad("no image attributes"))?;
    let (a, _) = file.chunk(image)?;
    let attrs = Attributes {
        width: u32_at(&a, 0).unwrap_or(0),
        height: u32_at(&a, 4).unwrap_or(0),
        compression: u16_at(&a, 17).unwrap_or(0),
        bits: u16_at(&a, 19).unwrap_or(0),
        grey: a.get(27).is_some_and(|&g| g != 0),
        layers: u16_at(&a, 36).unwrap_or(0),
    };
    let palette = match blocks.iter().find(|b| b.id == COLOR_BLOCK) {
        Some(b) => Some(read_palette(&mut file, b)?),
        None => None,
    };
    // 2. The stored pictures: the bank's, or Paint Shop Pro 5's thumbnail.
    let mut pictures = Vec::new();
    if let Some(bank) = blocks.iter().find(|b| b.id == COMPOSITE_BANK_BLOCK) {
        pictures = bank_pictures(&mut file, bank)?;
    } else if major <= 3
        && let Some(b) = blocks.iter().find(|b| b.id == COMPOSITE_IMAGE_BLOCK)
    {
        pictures.push(Picture { thumbnail: true, width: 0, height: 0, bits: 0, compression: 0, block: *b });
    }
    // 3. The ones to try, best first: for thumbnails the stored thumbnail
    //    when it is big enough, else the composite; for the viewer the
    //    composite, else the one layer of a single-layer document, else the
    //    thumbnail.  The next is tried when one can't be read (a few
    //    thumbnails are JPEGs strict decoders refuse).
    let full = pictures.iter().find(|p| !p.thumbnail && p.width as u64 * p.height as u64 <= MAX_PIXELS);
    let thumb = pictures.iter().find(|p| p.thumbnail);
    let mut order: Vec<Option<&Picture>> = Vec::new();
    match limits.max_texture_size.map(|n| n / 2) {
        Some(n) => {
            if let Some(t) = thumb.filter(|t| t.width.max(t.height) >= n) {
                order.push(Some(t));
            }
            order.extend([full.map(Some), thumb.map(Some), Some(None)].into_iter().flatten());
        }
        None => order.extend([full.map(Some), Some(None), thumb.map(Some)].into_iter().flatten()),
    }
    let mut error = crate::no_preview("Paint Shop Pro file");
    for choice in order {
        let result = match choice {
            Some(picture) => read_picture(&mut file, picture, palette.as_deref(), limits).map(|mut t| {
                let shown = if picture.thumbnail {
                    "The thumbnail Paint Shop Pro saved in the file"
                } else {
                    "The merged picture Paint Shop Pro saved in the file"
                };
                t.source.header.push(("Shown".into(), shown.into()));
                t
            }),
            // The one layer, which only a single-layer document's composite is.
            None if attrs.layers == 1 => match blocks.iter().find(|b| b.id == LAYER_START_BLOCK) {
                Some(layers) => single_layer(&mut file, layers, &attrs, palette.as_deref(), limits),
                None => continue,
            },
            None => continue,
        };
        match result {
            Ok(t) => return Ok(t),
            Err(e) => error = e,
        }
    }
    Err(error)
}

/// A picture in the composite bank.
struct Picture {
    thumbnail: bool,
    width: u32,
    height: u32,
    bits: u16,
    compression: u16,
    /// The JPEG or composite image block holding it.
    block: Block,
}

fn bank_pictures(file: &mut File, bank: &Block) -> Result<Vec<Picture>, LoadError> {
    let (_, rest) = file.chunk(bank)?;
    let subs = file.blocks(rest, bank.end())?;
    let attrs: Vec<&Block> = subs.iter().filter(|b| b.id == COMPOSITE_ATTRIBUTES_BLOCK).collect();
    let images: Vec<&Block> = subs.iter().filter(|b| b.id == JPEG_BLOCK || b.id == COMPOSITE_IMAGE_BLOCK).collect();
    let mut out = Vec::new();
    for (a, image) in attrs.iter().zip(images) {
        let (c, _) = file.chunk(a)?;
        out.push(Picture {
            width: u32_at(&c, 0).unwrap_or(0),
            height: u32_at(&c, 4).unwrap_or(0),
            bits: u16_at(&c, 8).unwrap_or(0),
            compression: u16_at(&c, 10).unwrap_or(0),
            thumbnail: u16_at(&c, 18) == Some(1),
            block: *image,
        });
    }
    Ok(out)
}

fn read_palette(file: &mut File, b: &Block) -> Result<Vec<[u8; 4]>, LoadError> {
    let (c, rest) = file.chunk(b)?;
    let count = (u32_at(&c, 0).unwrap_or(0) as u64).min(256);
    let entries = file.src.exact(rest, count * 4)?;
    // RGBQUAD: blue, green, red, unused.
    Ok(entries.chunks_exact(4).map(|e| [e[2], e[1], e[0], 255]).collect())
}

/// Reads the channel blocks among `blocks`.
fn read_channels(file: &mut File, blocks: &[Block], limit: u64) -> Result<Vec<(Channel, u32)>, LoadError> {
    let mut out = Vec::new();
    for b in blocks.iter().filter(|b| b.id == CHANNEL_BLOCK) {
        let (c, rest) = file.chunk(b)?;
        let packed = u32_at(&c, 0).unwrap_or(0) as u64;
        let unpacked = u32_at(&c, 4).unwrap_or(0);
        if packed > MAX_BLOCK.min(limit) {
            return Err(LoadError::TooLarge(format!("a {packed}-byte channel")));
        }
        let data = file.src.exact(rest, packed)?;
        out.push((
            Channel { bitmap: u16_at(&c, 8).unwrap_or(0), channel: u16_at(&c, 10).unwrap_or(0), data },
            unpacked,
        ));
    }
    Ok(out)
}

fn read_picture(
    file: &mut File,
    p: &Picture,
    palette: Option<&[[u8; 4]]>,
    limits: &Limits,
) -> Result<Texture, LoadError> {
    let b = p.block;
    if b.id == JPEG_BLOCK {
        let (c, rest) = file.chunk(&b)?;
        let packed = u32_at(&c, 0).unwrap_or(0) as u64;
        if packed > MAX_BLOCK {
            return Err(LoadError::TooLarge("Paint Shop Pro thumbnail".into()));
        }
        let jpeg = file.src.exact(rest, packed)?;
        return crate::decode_image(&jpeg, limits);
    }
    // A composite image block: an information chunk (Paint Shop Pro 5: the
    // thumbnail's attributes), a palette for paletted pictures, channels.
    let (c, rest) = file.chunk(&b)?;
    let (width, height, bits, compression) = if file.major <= 3 {
        (
            u32_at(&c, 0).unwrap_or(0),
            u32_at(&c, 4).unwrap_or(0),
            u16_at(&c, 8).unwrap_or(0),
            u16_at(&c, 10).unwrap_or(0),
        )
    } else {
        (p.width, p.height, p.bits, p.compression)
    };
    let subs = file.blocks(rest, b.end())?;
    let own = match subs.iter().find(|s| s.id == COLOR_BLOCK) {
        Some(s) => Some(read_palette(file, s)?),
        None => None,
    };
    let channels = read_channels(file, &subs, MAX_BLOCK)?;
    let rgba = assemble(width, height, bits, compression, false, own.as_deref().or(palette), &channels)?;
    picture_texture(width, height, rgba, limits)
}

/// The one raster layer of a single-layer document, placed in the image.
fn single_layer(
    file: &mut File,
    bank: &Block,
    attrs: &Attributes,
    palette: Option<&[[u8; 4]]>,
    limits: &Limits,
) -> Result<Texture, LoadError> {
    let start = match bank.first_chunk {
        Some(n) => bank.body + n,
        None => bank.body,
    };
    let layer = *file.blocks(start, bank.end())?.iter().find(|b| b.id == LAYER_BLOCK).ok_or_else(|| bad("no layer"))?;
    let (info, rest) = file.chunk(&layer)?;
    // The name (a counted string), the type, then the image and saved
    // rectangles, opacity and flags.
    // Paint Shop Pro 5 gave the name a fixed 256 bytes.
    let at = if file.major <= 3 { 256 } else { 2 + u16_at(&info, 0).unwrap_or(0) as usize };
    let kind = *info.get(at).ok_or_else(|| bad("a short layer"))?;
    let rect = |i: usize| -> Option<[i32; 4]> { Some([0, 4, 8, 12].map(|k| u32_at(&info, i + k).unwrap_or(0) as i32)) };
    let saved = rect(at + 17).ok_or_else(|| bad("a short layer"))?;
    let opacity = *info.get(at + 33).unwrap_or(&255);
    let flags = *info.get(at + 35).unwrap_or(&1);
    // Paint Shop Pro 5 numbered a normal layer 0, later versions 1.
    let raster = if file.major <= 3 { kind == 0 } else { kind == 1 };
    if !raster || opacity != 255 || flags & 1 == 0 {
        return Err(LoadError::Unsupported("the layer isn't a plain visible raster".into()));
    }
    let (w, h) = ((saved[2] - saved[0]).max(0) as u32, (saved[3] - saved[1]).max(0) as u32);
    // Then (major 4 on) any extension blocks, which newer versions write
    // even for plain layers, the bitmap chunk, and the channels.
    let mut channels_at = rest;
    if file.major > 3 {
        for _ in 0..64 {
            let h = file.src.exact(channels_at, 10)?;
            if &h[..4] != b"~BK\0" {
                break;
            }
            channels_at = channels_at.saturating_add(10 + u32_at(&h, 6).unwrap_or(0) as u64);
        }
        channels_at = channels_at.saturating_add(u32_at(&file.src.exact(channels_at, 4)?, 0).unwrap_or(0) as u64);
    }
    let subs = file.blocks(channels_at, layer.end())?;
    let channels = read_channels(file, &subs, MAX_BLOCK)?;
    let rgba = assemble(w, h, attrs.bits, attrs.compression, attrs.grey, palette, &channels)?;
    // Placed in a canvas the image's size; outside the saved rectangle is
    // transparent.
    let (cw, ch) = (attrs.width, attrs.height);
    if cw as u64 * ch as u64 > MAX_PIXELS || cw == 0 || ch == 0 {
        return Err(bad("an image without a size"));
    }
    let mut canvas = vec![0u8; cw as usize * ch as usize * 4];
    for y in 0..h as i64 {
        let ty = y + saved[1] as i64;
        if !(0..ch as i64).contains(&ty) {
            continue;
        }
        for x in 0..w as i64 {
            let tx = x + saved[0] as i64;
            if !(0..cw as i64).contains(&tx) {
                continue;
            }
            let s = ((y * w as i64 + x) * 4) as usize;
            let t = ((ty * cw as i64 + tx) * 4) as usize;
            canvas[t..t + 4].copy_from_slice(&rgba[s..s + 4]);
        }
    }
    let mut texture = picture_texture(cw, ch, canvas, limits)?;
    texture
        .source
        .header
        .push(("Shown".into(), "The document's one layer (the file holds only a small thumbnail of the whole)".into()));
    Ok(texture)
}

/// RGBA pixels from channels: three colour channels, or one of palette
/// indices or grey, and an optional transparency channel.
fn assemble(
    width: u32,
    height: u32,
    bits: u16,
    compression: u16,
    grey: bool,
    palette: Option<&[[u8; 4]]>,
    channels: &[(Channel, u32)],
) -> Result<Vec<u8>, LoadError> {
    let pixels = width as u64 * height as u64;
    if pixels == 0 || pixels > MAX_PIXELS {
        return Err(bad(&format!("a picture of {width} × {height}")));
    }
    let (w, h) = (width as usize, height as usize);
    let mut rgba = vec![255u8; w * h * 4];
    let mut have_colour = false;
    for (c, _) in channels {
        let transparency = TRANSPARENCY.contains(&c.bitmap);
        // Colour channels hold `bits` per pixel; masks always 8.
        let depth = if transparency || c.channel != 0 { 8 } else { bits.max(1) as usize };
        let row = (w * depth).div_ceil(8);
        let raw = unpack(&c.data, compression, row * h, row.next_multiple_of(4) * h)?;
        // Rows padded to four bytes, as the specification says, or not.
        let stride =
            if raw.len() >= row.next_multiple_of(4) * h && row % 4 != 0 { row.next_multiple_of(4) } else { row };
        let sample = |x: usize, y: usize| -> u8 {
            let r = &raw[(y * stride).min(raw.len())..];
            match depth {
                8 => r.get(x).copied().unwrap_or(0),
                4 => r.get(x / 2).map_or(0, |b| if x.is_multiple_of(2) { b >> 4 } else { b & 15 }),
                1 => r.get(x / 8).map_or(0, |b| (b >> (7 - x % 8)) & 1),
                _ => 0,
            }
        };
        for y in 0..h {
            for x in 0..w {
                let p = &mut rgba[(y * w + x) * 4..(y * w + x) * 4 + 4];
                let v = sample(x, y);
                match (transparency, c.channel) {
                    (true, _) => p[3] = v,
                    (false, 1..=3) => p[c.channel as usize - 1] = v,
                    (false, _) => match palette.filter(|_| !grey || depth < 8) {
                        Some(pal) => {
                            let e = pal.get(v as usize).copied().unwrap_or([0, 0, 0, 255]);
                            p[..3].copy_from_slice(&e[..3]);
                        }
                        None => p[..3].copy_from_slice(&[v, v, v]),
                    },
                }
            }
        }
        have_colour |= !transparency;
    }
    if !have_colour {
        return Err(bad("a picture without colour channels"));
    }
    Ok(rgba)
}

/// A channel's bytes, uncompressed: at most `padded` of them, and at least
/// `exact` (short data is padded with zeros).
fn unpack(data: &[u8], compression: u16, exact: usize, padded: usize) -> Result<Vec<u8>, LoadError> {
    let mut out = match compression {
        0 => data.get(..padded.min(data.len())).unwrap_or_default().to_vec(),
        1 => {
            // Run lengths: a count over 128 repeats the next byte (count -
            // 128) times; otherwise that many bytes follow as they are.
            let mut out = Vec::with_capacity(padded);
            let mut i = 0;
            while i < data.len() && out.len() < padded {
                let n = data[i] as usize;
                i += 1;
                if n > 128 {
                    let v = *data.get(i).unwrap_or(&0);
                    i += 1;
                    out.resize((out.len() + n - 128).min(padded), v);
                } else {
                    let end = (i + n).min(data.len());
                    out.extend_from_slice(&data[i..end]);
                    i = end;
                }
            }
            out.truncate(padded);
            out
        }
        2 => miniz_oxide::inflate::decompress_to_vec_zlib_with_limit(data, padded)
            .or_else(|e| if e.output.len() >= exact { Ok(e.output) } else { Err(e) })
            .map_err(|_| bad("damaged zlib data"))?,
        c => return Err(LoadError::Unsupported(format!("Paint Shop Pro channels compressed with method {c}"))),
    };
    if out.len() < exact {
        out.resize(exact, 0);
    }
    Ok(out)
}

fn picture_texture(width: u32, height: u32, rgba: Vec<u8>, limits: &Limits) -> Result<Texture, LoadError> {
    use slerp_scene::textures::ColorSpace;
    let mut t = Texture::from_rgba8(width, height, rgba, ColorSpace::Srgb);
    if let Some(n) = limits.max_texture_size
        && width.max(height) > n
    {
        let small = slerp_scene::textures::thumbnail(&t, n);
        t = Texture::from_rgba8(small.width, small.height, small.pixels, ColorSpace::Srgb);
    }
    (t.source.width, t.source.height) = (width, height);
    t.source.format = "channels".into();
    Ok(t)
}

//endregion
