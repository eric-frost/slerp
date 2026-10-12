//! XNA and MonoGame compiled content (`.xnb`): the texture inside a
//! `Texture2D` or a `SpriteFont`.
//!
//! An XNB file is a ten-byte header (`XNB`, a platform letter, a version,
//! flags and the file size), optionally followed by the size of the data
//! once decompressed, and then the data, which is LZX-compressed (flag
//! `0x80`, XNA and MonoGame) or LZ4-compressed (flag `0x40`, MonoGame 3.8).
//! The data starts with the names of the readers that parse each type
//! stored, then the objects; the first object is the asset.  Anything but a
//! texture or a sprite font is reported as "not an image" without further
//! reading.
//!
//! Texture surface formats handled: Color (RGBA, premultiplied alpha is
//! undone when the pixels look premultiplied), Bgr565, Bgra5551, Bgra4444,
//! Dxt1, Dxt3, Dxt5, Alpha8, Bgr32 and Bgra32.  Only the largest level is
//! decoded.  Xbox 360 files (platform `x`) are big-endian and not read.

use crate::common::{Block, Cursor, check_size, decode_blocks, texture};
use lzxd::{Lzxd, WindowSize};
use slerp_scene::{LoadError, Texture};

/// The most decompressed data read: a 16384 x 16384 RGBA texture is 1 GB,
/// and nothing here wants more than a quarter of that.
const MAX_DECOMPRESSED: usize = 256 << 20;

pub fn sniff(head: &[u8]) -> bool {
    head.len() >= 6 && &head[..3] == b"XNB" && head[3].is_ascii_alphabetic() && (3..=6).contains(&head[4])
}

struct Header {
    platform: u8,
    version: u8,
    flags: u8,
    size: u32,
    /// Size of the data after decompression, when compressed.
    decompressed: Option<u32>,
    data_at: usize,
}

fn header(bytes: &[u8]) -> Result<Header, LoadError> {
    if !sniff(bytes) {
        return Err(LoadError::invalid("not an XNB file"));
    }
    let mut c = Cursor::new(bytes);
    c.take(3)?;
    let (platform, version, flags) = (c.u8()?, c.u8()?, c.u8()?);
    let size = c.u32()?;
    let compressed = flags & 0xc0 != 0;
    let decompressed = if compressed { Some(c.u32()?) } else { None };
    Ok(Header { platform, version, flags, size, decompressed, data_at: c.pos })
}

/// Decompresses XNA's LZX stream: a series of blocks, each with a small
/// header (`0xFF`, a big-endian frame size and block size for unusual
/// frames, otherwise just a big-endian block size for a 32 KB frame),
/// holding one frame of the window.  Stops once `need` bytes exist, so a
/// thumbnail never unpacks the mip chain.
fn lzx(data: &[u8], total: usize, need: usize) -> Result<Vec<u8>, LoadError> {
    let mut lz = Lzxd::new(WindowSize::KB64);
    let mut out: Vec<u8> = Vec::with_capacity(need.min(total).min(1 << 24));
    let mut pos = 0usize;
    while out.len() < need.min(total) && pos < data.len() {
        let (frame, block) = if data[pos] == 0xff {
            let h = data.get(pos + 1..pos + 5).ok_or_else(|| LoadError::invalid("LZX block header is cut off"))?;
            pos += 5;
            (u16::from_be_bytes([h[0], h[1]]) as usize, u16::from_be_bytes([h[2], h[3]]) as usize)
        } else {
            let h = data.get(pos..pos + 2).ok_or_else(|| LoadError::invalid("LZX block header is cut off"))?;
            pos += 2;
            (0x8000, u16::from_be_bytes([h[0], h[1]]) as usize)
        };
        if block == 0 || frame == 0 {
            break;
        }
        let chunk = data.get(pos..pos + block).ok_or_else(|| LoadError::invalid("LZX block is cut off"))?;
        pos += block;
        let frame = frame.min(total - out.len());
        let bytes = lz.decompress_next(chunk, frame).map_err(|e| LoadError::invalid(format!("LZX: {e}")))?;
        out.extend_from_slice(bytes);
    }
    Ok(out)
}

/// A raw LZ4 block (no frame header), as MonoGame writes: sequences of a
/// token, literals and a back-reference.  Output stops at `total`.
fn lz4(data: &[u8], total: usize) -> Result<Vec<u8>, LoadError> {
    let bad = || LoadError::invalid("LZ4 data is damaged");
    let mut out: Vec<u8> = Vec::with_capacity(total.min(1 << 24));
    let mut i = 0usize;
    while i < data.len() && out.len() < total {
        let token = data[i];
        i += 1;
        let mut lit = (token >> 4) as usize;
        if lit == 15 {
            loop {
                let b = *data.get(i).ok_or_else(bad)?;
                i += 1;
                lit = lit.saturating_add(b as usize);
                if b != 255 {
                    break;
                }
            }
        }
        let end = i.checked_add(lit).filter(|&e| e <= data.len()).ok_or_else(bad)?;
        let take = lit.min(total - out.len());
        out.extend_from_slice(&data[i..i + take]);
        i = end;
        if i >= data.len() || out.len() >= total {
            break;
        }
        let offset = u16::from_le_bytes([*data.get(i).ok_or_else(bad)?, *data.get(i + 1).ok_or_else(bad)?]) as usize;
        i += 2;
        if offset == 0 || offset > out.len() {
            return Err(bad());
        }
        let mut len = (token & 15) as usize;
        if len == 15 {
            loop {
                let b = *data.get(i).ok_or_else(bad)?;
                i += 1;
                len = len.saturating_add(b as usize);
                if b != 255 {
                    break;
                }
            }
        }
        len = len.saturating_add(4).min(total - out.len());
        for _ in 0..len {
            let b = out[out.len() - offset];
            out.push(b);
        }
    }
    Ok(out)
}

/// A 7-bit encoded integer (low groups first), as .NET writes lengths.
fn read7(c: &mut Cursor) -> Result<usize, LoadError> {
    let mut v = 0usize;
    for shift in (0..35).step_by(7) {
        let b = c.u8()?;
        v |= ((b & 0x7f) as usize) << shift;
        if b & 0x80 == 0 {
            return Ok(v);
        }
    }
    Err(LoadError::invalid("bad length in XNB"))
}

fn read_string(c: &mut Cursor) -> Result<String, LoadError> {
    let n = read7(c)?;
    if n > 1024 {
        return Err(LoadError::invalid("XNB name is too long"));
    }
    Ok(String::from_utf8_lossy(c.take(n)?).into_owned())
}

/// "Microsoft.Xna.Framework.Content.Texture2DReader, Microsoft..." as
/// "Texture2D".
fn short_name(reader: &str) -> String {
    let full = reader.split(',').next().unwrap_or(reader);
    let last = full.rsplit('.').next().unwrap_or(full);
    last.strip_suffix("Reader").unwrap_or(last).to_string()
}

/// Reads the texture and describes what the file held.
pub fn decode(bytes: &[u8]) -> Result<Texture, LoadError> {
    let h = header(bytes)?;
    if h.platform == b'x' {
        return Err(LoadError::Unsupported("Xbox 360 XNB files (big-endian data)".into()));
    }
    let body = bytes.get(h.data_at..).unwrap_or(&[]);
    let declared = h.size as usize;
    let body = &body[..body.len().min(declared.saturating_sub(h.data_at))];
    // Readers, then the asset header and the texture header are in the first
    // few kilobytes; the first mip follows.  Decompress that far first.
    let (data, how) = match h.decompressed {
        Some(total) => {
            let total = total as usize;
            if total > MAX_DECOMPRESSED {
                return Err(LoadError::TooLarge(format!("XNB data of {total} bytes")));
            }
            if h.flags & 0x80 != 0 {
                (Lazy::Lzx { data: body, total }, "LZX")
            } else {
                (Lazy::Lz4 { data: body, total }, "LZ4")
            }
        }
        None => (Lazy::Plain(body), "uncompressed"),
    };

    // 1. The reader table.
    let head = data.first(4096)?;
    let mut c = Cursor::new(&head);
    let count = read7(&mut c)?;
    if count == 0 || count > 256 {
        return Err(LoadError::invalid("XNB has no type readers"));
    }
    let mut readers = Vec::with_capacity(count);
    for _ in 0..count {
        readers.push(read_string(&mut c)?);
        c.i32()?;
    }
    let _shared = read7(&mut c)?;
    let type_id = read7(&mut c)?;
    if type_id == 0 || type_id > readers.len() {
        return Err(LoadError::invalid("XNB asset has no type"));
    }
    let name = short_name(&readers[type_id - 1]);

    // 2. A SpriteFont starts with its texture, as an object with a type id
    //    of its own.
    let mut container = format!("XNB {name}");
    match name.as_str() {
        "Texture2D" => {}
        "SpriteFont" => {
            let id = read7(&mut c)?;
            if id == 0 || id > readers.len() || short_name(&readers[id - 1]) != "Texture2D" {
                return Err(LoadError::Unsupported("a SpriteFont without a Texture2D".into()));
            }
            container = "XNB SpriteFont texture".into();
        }
        other => {
            return Err(LoadError::Unsupported(format!("XNB content of type {other} is not an image")));
        }
    }

    // 3. The texture header, then the first level.
    let format = c.i32()?;
    let (w, ht, levels) = (c.i32()?, c.i32()?, c.i32()?);
    if w <= 0 || ht <= 0 || levels <= 0 {
        return Err(LoadError::invalid("XNB texture has no size"));
    }
    let (w, ht) = (w as u32, ht as u32);
    check_size(w, ht)?;
    let size = c.u32()? as usize;
    let surface =
        Surface::from_id(format).ok_or_else(|| LoadError::Unsupported(format!("XNB surface format {format}")))?;
    let expected = surface.level_bytes(w, ht);
    if size < expected {
        return Err(LoadError::invalid(format!("XNB level has {size} bytes, {expected} needed")));
    }
    let start = c.pos;
    let level = data.range(start, expected)?;
    let (mut rgba, note) = surface.to_rgba(&level, w, ht);

    // 4. XNA premultiplies alpha when it builds textures; undo it when the
    //    pixels look premultiplied (no channel above alpha), so semi-
    //    transparent edges keep their colour.
    let mut unpremultiplied = false;
    if matches!(surface, Surface::Color | Surface::Bgra32 | Surface::Bgra4444 | Surface::Bgra5551)
        && rgba.chunks_exact(4).all(|p| p[0] <= p[3] && p[1] <= p[3] && p[2] <= p[3])
        && rgba.chunks_exact(4).any(|p| p[3] != 0 && p[3] != 255)
    {
        for p in rgba.chunks_exact_mut(4) {
            if p[3] != 0 && p[3] != 255 {
                let a = p[3] as u32;
                for c in &mut p[..3] {
                    *c = ((*c as u32 * 255 + a / 2) / a).min(255) as u8;
                }
            }
        }
        unpremultiplied = true;
    }
    let platform = match h.platform {
        b'w' => "Windows",
        b'm' => "Windows Phone",
        b'i' => "iOS",
        b'a' => "Android",
        b'd' => "DesktopGL",
        b'X' | b'M' => "macOS",
        b'P' => "PlayStation 4",
        b'W' => "Windows Store",
        b'n' => "Native Client",
        b'r' => "Raspberry Pi",
        _ => "other platform",
    };
    let mut detail = format!("{} {w}x{ht}, {platform}, version {}, {how}", surface.name(), h.version);
    if levels > 1 {
        detail.push_str(&format!(", {levels} mip levels (largest shown)"));
    }
    if unpremultiplied {
        detail.push_str(", premultiplied alpha undone");
    }
    if let Some(n) = note {
        detail.push_str(&format!(", {n}"));
    }
    Ok(texture(w, ht, rgba, &container, &detail))
}

/// The decompressed data, produced only as far as it is read.
enum Lazy<'a> {
    Plain(&'a [u8]),
    Lzx { data: &'a [u8], total: usize },
    Lz4 { data: &'a [u8], total: usize },
}

impl Lazy<'_> {
    /// The first `n` bytes (fewer if the data is shorter).
    fn first(&self, n: usize) -> Result<Vec<u8>, LoadError> {
        match self {
            Lazy::Plain(d) => Ok(d[..d.len().min(n)].to_vec()),
            Lazy::Lzx { data, total } => lzx(data, *total, n),
            Lazy::Lz4 { data, total } => lz4(data, (*total).min(n)),
        }
    }

    /// `len` bytes starting at `start`, or an error when the data ends first.
    fn range(&self, start: usize, len: usize) -> Result<Vec<u8>, LoadError> {
        let end = start
            .checked_add(len)
            .filter(|&e| e <= MAX_DECOMPRESSED)
            .ok_or_else(|| LoadError::invalid("XNB level is too large"))?;
        let all = self.first(end)?;
        all.get(start..end).map(<[u8]>::to_vec).ok_or_else(|| LoadError::invalid("XNB texture data is cut off"))
    }
}

/// XNA's `SurfaceFormat` values (MonoGame keeps them and adds more).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Surface {
    Color,
    Bgr565,
    Bgra5551,
    Bgra4444,
    Dxt1,
    Dxt3,
    Dxt5,
    Alpha8,
    Bgr32,
    Bgra32,
}

impl Surface {
    fn from_id(id: i32) -> Option<Surface> {
        Some(match id {
            0 => Surface::Color,
            1 => Surface::Bgr565,
            2 => Surface::Bgra5551,
            3 => Surface::Bgra4444,
            4 | 70 => Surface::Dxt1,
            5 | 71 => Surface::Dxt3,
            6 | 72 => Surface::Dxt5,
            12 => Surface::Alpha8,
            20 => Surface::Bgr32,
            21 => Surface::Bgra32,
            _ => return None,
        })
    }

    fn name(self) -> &'static str {
        match self {
            Surface::Color => "Color (RGBA8)",
            Surface::Bgr565 => "Bgr565",
            Surface::Bgra5551 => "Bgra5551",
            Surface::Bgra4444 => "Bgra4444",
            Surface::Dxt1 => "DXT1",
            Surface::Dxt3 => "DXT3",
            Surface::Dxt5 => "DXT5",
            Surface::Alpha8 => "Alpha8",
            Surface::Bgr32 => "Bgr32",
            Surface::Bgra32 => "Bgra32",
        }
    }

    fn level_bytes(self, w: u32, h: u32) -> usize {
        let px = w as usize * h as usize;
        match self {
            Surface::Color | Surface::Bgr32 | Surface::Bgra32 => px * 4,
            Surface::Bgr565 | Surface::Bgra5551 | Surface::Bgra4444 => px * 2,
            Surface::Alpha8 => px,
            Surface::Dxt1 => Block::Dxt1.level_bytes(w, h),
            Surface::Dxt3 => Block::Dxt3.level_bytes(w, h),
            Surface::Dxt5 => Block::Dxt5.level_bytes(w, h),
        }
    }

    fn to_rgba(self, d: &[u8], w: u32, h: u32) -> (Vec<u8>, Option<&'static str>) {
        let px = w as usize * h as usize;
        let scale5 = |v: u16| ((v << 3) | (v >> 2)) as u8;
        let scale6 = |v: u16| ((v << 2) | (v >> 4)) as u8;
        let scale4 = |v: u16| (v * 17) as u8;
        let words = || d.chunks_exact(2).map(|b| u16::from_le_bytes([b[0], b[1]]));
        let rgba: Vec<u8> = match self {
            Surface::Color => d[..px * 4].to_vec(),
            Surface::Bgr32 => d.chunks_exact(4).flat_map(|p| [p[2], p[1], p[0], 255]).collect(),
            Surface::Bgra32 => d.chunks_exact(4).flat_map(|p| [p[2], p[1], p[0], p[3]]).collect(),
            Surface::Bgr565 => {
                words().flat_map(|v| [scale5(v >> 11), scale6((v >> 5) & 63), scale5(v & 31), 255]).collect()
            }
            Surface::Bgra5551 => words()
                .flat_map(|v| {
                    [
                        scale5((v >> 10) & 31),
                        scale5((v >> 5) & 31),
                        scale5(v & 31),
                        if v & 0x8000 != 0 { 255 } else { 0 },
                    ]
                })
                .collect(),
            Surface::Bgra4444 => words()
                .flat_map(|v| [scale4((v >> 8) & 15), scale4((v >> 4) & 15), scale4(v & 15), scale4(v >> 12)])
                .collect(),
            Surface::Alpha8 => d.iter().flat_map(|&a| [255, 255, 255, a]).collect(),
            Surface::Dxt1 => decode_blocks(Block::Dxt1, d, w, h),
            Surface::Dxt3 => decode_blocks(Block::Dxt3, d, w, h),
            Surface::Dxt5 => decode_blocks(Block::Dxt5, d, w, h),
        };
        let note = (self == Surface::Alpha8).then_some("alpha shown as white with transparency");
        (rgba, note)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use slerp_scene::textures::Pixels;

    fn s7(n: usize) -> Vec<u8> {
        let mut v = Vec::new();
        let mut n = n;
        loop {
            let b = (n & 0x7f) as u8;
            n >>= 7;
            if n == 0 {
                v.push(b);
                return v;
            }
            v.push(b | 0x80);
        }
    }

    fn string(s: &str) -> Vec<u8> {
        let mut v = s.len().pipe(s7);
        v.extend(s.as_bytes());
        v
    }

    trait Pipe: Sized {
        fn pipe<R>(self, f: impl FnOnce(Self) -> R) -> R {
            f(self)
        }
    }
    impl Pipe for usize {}

    /// The body of an XNB: readers, one asset.
    fn body(reader: &str, format: i32, w: i32, h: i32, level: &[u8], font: bool) -> Vec<u8> {
        let mut b = Vec::new();
        let readers = if font { vec![reader, "Microsoft.Xna.Framework.Content.Texture2DReader"] } else { vec![reader] };
        b.extend(s7(readers.len()));
        for r in &readers {
            b.extend(string(r));
            b.extend(0i32.to_le_bytes());
        }
        b.extend(s7(0)); // shared resources
        b.extend(s7(1)); // the asset's type
        if font {
            b.extend(s7(2)); // the font's texture
        }
        b.extend(format.to_le_bytes());
        b.extend(w.to_le_bytes());
        b.extend(h.to_le_bytes());
        b.extend(1i32.to_le_bytes());
        b.extend((level.len() as u32).to_le_bytes());
        b.extend(level);
        b
    }

    const TEX: &str = "Microsoft.Xna.Framework.Content.Texture2DReader, Microsoft.Xna.Framework.Graphics, Version=4.0.0.0, Culture=neutral, PublicKeyToken=842cf8be1de50553";

    fn file(flags: u8, version: u8, payload: &[u8], decompressed: Option<usize>) -> Vec<u8> {
        let mut f = b"XNBw".to_vec();
        f.push(version);
        f.push(flags);
        let extra = if decompressed.is_some() { 4 } else { 0 };
        f.extend(((10 + extra + payload.len()) as u32).to_le_bytes());
        if let Some(d) = decompressed {
            f.extend((d as u32).to_le_bytes());
        }
        f.extend(payload);
        f
    }

    fn pixels(t: &Texture) -> &[u8] {
        let Pixels::Rgba8(v) = t.base() else { panic!() };
        v
    }

    #[test]
    fn uncompressed_color_texture() {
        let level: Vec<u8> = (0..16).map(|i| if i % 4 == 3 { 255 } else { i }).collect(); // 2x2 pixels
        let f = file(0, 4, &body(TEX, 0, 2, 2, &level, false), None);
        assert!(sniff(&f));
        let t = decode(&f).unwrap();
        assert_eq!((t.width(), t.height()), (2, 2));
        assert_eq!(&pixels(&t)[..4], &[0, 1, 2, 255]);
        assert!(t.source.format.contains("Color") && t.source.container.contains("Texture2D"));
    }

    #[test]
    fn premultiplied_alpha_is_undone_when_it_looks_premultiplied() {
        // One pixel at half alpha with colour 100: premultiplied from 200.
        let level = [100, 50, 0, 128];
        let f = file(0, 4, &body(TEX, 0, 1, 1, &level, false), None);
        let t = decode(&f).unwrap();
        assert_eq!(pixels(&t), &[199, 100, 0, 128]);
        // A channel above alpha means the pixel was not premultiplied.
        let level = [250, 50, 0, 128];
        let f = file(0, 4, &body(TEX, 0, 1, 1, &level, false), None);
        assert_eq!(pixels(&decode(&f).unwrap()), &[250, 50, 0, 128]);
    }

    #[test]
    fn packed_pixel_formats() {
        // Bgr565 pure red, Bgra5551 green with alpha, Bgra4444 blue.
        let f = file(0, 4, &body(TEX, 1, 1, 1, &0xf800u16.to_le_bytes(), false), None);
        assert_eq!(pixels(&decode(&f).unwrap()), &[255, 0, 0, 255]);
        let f = file(0, 4, &body(TEX, 2, 1, 1, &(0x8000u16 | 31 << 5).to_le_bytes(), false), None);
        assert_eq!(pixels(&decode(&f).unwrap()), &[0, 255, 0, 255]);
        let f = file(0, 4, &body(TEX, 3, 1, 1, &0xf00fu16.to_le_bytes(), false), None);
        assert_eq!(pixels(&decode(&f).unwrap()), &[0, 0, 255, 255]);
        let f = file(0, 4, &body(TEX, 12, 2, 1, &[0, 255], false), None);
        assert_eq!(pixels(&decode(&f).unwrap())[3], 0);
    }

    #[test]
    fn dxt_surfaces() {
        let block = [0x00, 0xf8, 0x00, 0x00, 0, 0, 0, 0];
        let f = file(0, 4, &body(TEX, 4, 4, 4, &block, false), None);
        assert_eq!(&pixels(&decode(&f).unwrap())[..4], &[255, 0, 0, 255]);
        let mut dxt5 = vec![255u8, 255, 0, 0, 0, 0, 0, 0];
        dxt5.extend(block);
        let f = file(0, 4, &body(TEX, 6, 4, 4, &dxt5, false), None);
        assert_eq!(&pixels(&decode(&f).unwrap())[..4], &[255, 0, 0, 255]);
    }

    #[test]
    fn sprite_font_shows_its_texture() {
        let f =
            file(0, 4, &body("Microsoft.Xna.Framework.Content.SpriteFontReader", 0, 1, 1, &[1, 2, 3, 255], true), None);
        let t = decode(&f).unwrap();
        assert_eq!(pixels(&t), &[1, 2, 3, 255]);
        assert!(t.source.container.contains("SpriteFont"));
    }

    #[test]
    fn other_content_is_not_an_image() {
        let f = file(0, 4, &body("Microsoft.Xna.Framework.Content.SoundEffectReader", 0, 1, 1, &[0; 4], false), None);
        let e = decode(&f).unwrap_err().to_string();
        assert!(e.contains("SoundEffect") && e.contains("not an image"), "{e}");
    }

    /// LZ4 as MonoGame writes it, here only literals plus one match.
    fn lz4_pack(data: &[u8]) -> Vec<u8> {
        // Sequence 1: all but the last 4 bytes literal; sequence 2 copies
        // the last 4 bytes from the first 4 (offset = len - 4... simple:
        // use one literal run, which is a legal final sequence).
        let mut out = Vec::new();
        let lit = data.len();
        if lit < 15 {
            out.push((lit as u8) << 4);
        } else {
            out.push(15 << 4);
            let mut rest = lit - 15;
            while rest >= 255 {
                out.push(255);
                rest -= 255;
            }
            out.push(rest as u8);
        }
        out.extend(data);
        out
    }

    #[test]
    fn lz4_compressed_files() {
        let level: Vec<u8> = (0..64).map(|i| if i % 4 == 3 { 255 } else { (i * 3) as u8 }).collect(); // 4x4
        let raw = body(TEX, 0, 4, 4, &level, false);
        let packed = lz4_pack(&raw);
        let f = file(0x40, 5, &packed, Some(raw.len()));
        let t = decode(&f).unwrap();
        assert_eq!(&pixels(&t)[..4], &[0, 3, 6, 255]);
        assert!(t.source.format.contains("LZ4"));
    }

    #[test]
    fn lz4_matches_and_bombs() {
        // "abcd" then a match of length 8 at offset 4: "abcdabcdabcd".
        let data = [0x44, b'a', b'b', b'c', b'd', 4, 0];
        assert_eq!(lz4(&data, 100).unwrap(), b"abcdabcdabcd");
        assert_eq!(lz4(&data, 6).unwrap(), b"abcdab");
        // Offset past the start, and zero offset.
        assert!(lz4(&[0x14, b'a', 9, 0], 100).is_err());
        assert!(lz4(&[0x14, b'a', 0, 0], 100).is_err());
        // A literal run longer than the data.
        assert!(lz4(&[0xf0, 255, 255, 255], 100).is_err());
    }

    #[test]
    fn compression_bombs_stay_bounded() {
        // LZ4 claiming 200 MB from a handful of bytes: stops at the data.
        let f = file(0x40, 5, &[0xf0, 0xff, 0xff, 0xff, 1, 2, 3], Some(200 << 20));
        assert!(decode(&f).is_err());
        // A match that repeats one byte for as long as it is allowed to.
        let bomb = [0x1f, b'a', 1, 0, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x00];
        let out = lz4(&bomb, 1 << 20).unwrap();
        assert!(out.len() > 2000 && out.iter().all(|&b| b == b'a'));
        assert_eq!(lz4(&bomb, 100).unwrap().len(), 100);
        // LZX with a block header that promises more than the stream has,
        // and a declared size at the limit.
        let f = file(0x80, 5, &[0xff, 0x7f, 0xff, 0x7f, 0xff, 1, 2, 3], Some(MAX_DECOMPRESSED));
        assert!(decode(&f).is_err());
        let f = file(0x80, 5, &[0, 0, 0, 0], Some(MAX_DECOMPRESSED + 1));
        assert!(matches!(decode(&f), Err(LoadError::TooLarge(_))));
    }

    #[test]
    fn absurd_headers_fail_before_allocating() {
        let f = file(0, 4, &body(TEX, 0, 100_000, 100_000, &[0; 16], false), None);
        assert!(decode(&f).is_err());
        let f = file(0x80, 4, &[1, 2, 3], Some(usize::MAX >> 40));
        assert!(decode(&f).is_err());
        let f = file(0, 4, &body(TEX, 0, 2, 2, &[0; 4], false), None);
        assert!(decode(&f).is_err());
        let f = file(0, 4, &body(TEX, 99, 2, 2, &[0; 16], false), None);
        assert!(decode(&f).is_err());
    }

    #[test]
    fn truncations_never_panic() {
        let level: Vec<u8> = (0..64).collect();
        let f = file(0, 4, &body(TEX, 0, 4, 4, &level, false), None);
        for len in 0..f.len() {
            let _ = decode(&f[..len]);
        }
        let mut seed = 7u32;
        for _ in 0..300 {
            let mut g = f.clone();
            seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
            let at = (seed >> 8) as usize % g.len();
            g[at] = (seed >> 24) as u8;
            let _ = decode(&g);
            let compressed = file(0x80, 4, &g[10..], Some(g.len()));
            let _ = decode(&compressed);
        }
    }
}
