//! TIFF files: classic, BigTIFF and Hamamatsu's NDPI variant, read by
//! seeking so a gigabyte file costs only the directories and chunks that are
//! asked for.
//!
//! This crate has no dependencies of Slerp's own, so any crate can use it.
//! The root module reads the structure (directories, tags, the table of
//! strips or tiles); [`codec`] decompresses chunks (none, LZW, Deflate,
//! PackBits, Zstandard, JPEG, JPEG 2000, with the horizontal and floating
//! point predictors) and [`raster`] assembles a page from them: bit depths
//! from 1 to 64, integer and floating point samples, interleaved or planar,
//! strips or tiles, palettes, CMYK, and reduced-size decoding of JPEG.
//!
//! The simplest way in is [`page_count`] and [`decode_page`], which read one
//! page of a file into a [`Raster`].  Slerp's volume crate builds stacks,
//! OME-TIFF hyperstacks and whole-slide pyramids on the rest.

pub mod codec;
pub mod raster;

pub use raster::{Options, Raster, Sample};

use std::path::Path;

fn read_exact_at(file: &File, mut buf: &mut [u8], mut offset: u64) -> std::io::Result<()> {
    while !buf.is_empty() {
        #[cfg(unix)]
        let n = std::os::unix::fs::FileExt::read_at(file, buf, offset)?;
        #[cfg(windows)]
        let n = std::os::windows::fs::FileExt::seek_read(file, buf, offset)?;
        if n == 0 {
            return Err(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "the file ends early"));
        }
        buf = &mut buf[n..];
        offset += n as u64;
    }
    Ok(())
}
use std::collections::HashSet;
use std::fs::File;
use std::sync::Arc;

/// Most pages a file may have.
pub const MAX_PAGES: usize = 1 << 19;
/// Largest tag value read into memory.
const MAX_TAG_BYTES: u64 = 64 << 20;

/// Tag numbers this reader uses.
pub mod tag {
    pub const SUBFILE_TYPE: u16 = 254;
    pub const WIDTH: u16 = 256;
    pub const HEIGHT: u16 = 257;
    pub const BITS: u16 = 258;
    pub const COMPRESSION: u16 = 259;
    pub const PHOTOMETRIC: u16 = 262;
    pub const DESCRIPTION: u16 = 270;
    pub const STRIP_OFFSETS: u16 = 273;
    pub const SAMPLES: u16 = 277;
    pub const ROWS_PER_STRIP: u16 = 278;
    pub const STRIP_COUNTS: u16 = 279;
    pub const X_RESOLUTION: u16 = 282;
    pub const Y_RESOLUTION: u16 = 283;
    pub const PLANAR: u16 = 284;
    pub const FILL_ORDER: u16 = 266;
    pub const T4_OPTIONS: u16 = 292;
    pub const YCBCR_COEFFICIENTS: u16 = 529;
    pub const REFERENCE_BLACK_WHITE: u16 = 532;
    pub const RESOLUTION_UNIT: u16 = 296;
    pub const SOFTWARE: u16 = 305;
    pub const PREDICTOR: u16 = 317;
    pub const COLOR_MAP: u16 = 320;
    pub const TILE_WIDTH: u16 = 322;
    pub const TILE_HEIGHT: u16 = 323;
    pub const TILE_OFFSETS: u16 = 324;
    pub const TILE_COUNTS: u16 = 325;
    pub const SUB_IFDS: u16 = 330;
    pub const EXTRA_SAMPLES: u16 = 338;
    pub const SAMPLE_FORMAT: u16 = 339;
    pub const JPEG_TABLES: u16 = 347;
    pub const YCBCR_SUBSAMPLING: u16 = 530;
    pub const NDPI_FORMAT_FLAG: u16 = 65420;
    pub const NDPI_SOURCE_LENS: u16 = 65421;
}

/// One tag of a directory: where its value is.
#[derive(Clone, Debug)]
pub struct Entry {
    pub tag: u16,
    pub typ: u16,
    pub count: u64,
    /// The value itself when it fits in the entry, else the offset of it.
    raw: [u8; 8],
    inline: bool,
}

/// An image file directory.
#[derive(Clone, Debug)]
pub struct Ifd {
    pub offset: u64,
    pub entries: Vec<Entry>,
    pub next: u64,
}

/// An open TIFF file.
pub struct Tiff {
    pub file: Arc<File>,
    pub len: u64,
    pub big: bool,
    pub le: bool,
    pub first_ifd: u64,
}

fn type_size(t: u16) -> u64 {
    match t {
        1 | 2 | 6 | 7 => 1,
        3 | 8 => 2,
        4 | 9 | 11 | 13 => 4,
        5 | 10 | 12 | 16 | 17 | 18 => 8,
        _ => 0,
    }
}

impl Tiff {
    /// Checks the header and finds the first directory.
    pub fn open(path: &Path) -> Result<Tiff, String> {
        let file = File::open(path).map_err(|e| e.to_string())?;
        let len = file.metadata().map_err(|e| e.to_string())?.len();
        let mut h = [0u8; 16];
        let n = len.min(16) as usize;
        read_exact_at(&file, &mut h[..n], 0).map_err(|e| e.to_string())?;
        Tiff::from_header(Arc::new(file), len, &h[..n])
    }

    pub fn from_header(file: Arc<File>, len: u64, h: &[u8]) -> Result<Tiff, String> {
        let le = match h.get(..2) {
            Some(b"II") => true,
            Some(b"MM") => false,
            _ => return Err("this isn't a TIFF file".into()),
        };
        let u16_ = |o: usize| {
            h.get(o..o + 2)
                .map(|b| if le { u16::from_le_bytes([b[0], b[1]]) } else { u16::from_be_bytes([b[0], b[1]]) })
        };
        match u16_(2) {
            Some(42) => {
                let b = h.get(4..8).ok_or("the file ends inside the header")?;
                let first = if le {
                    u32::from_le_bytes(b.try_into().unwrap())
                } else {
                    u32::from_be_bytes(b.try_into().unwrap())
                };
                Ok(Tiff { file, len, big: false, le, first_ifd: first as u64 })
            }
            Some(43) => {
                if u16_(4) != Some(8) || u16_(6) != Some(0) {
                    return Err("an unknown BigTIFF variant".into());
                }
                let b = h.get(8..16).ok_or("the file ends inside the header")?;
                let first = if le {
                    u64::from_le_bytes(b.try_into().unwrap())
                } else {
                    u64::from_be_bytes(b.try_into().unwrap())
                };
                Ok(Tiff { file, len, big: true, le, first_ifd: first })
            }
            _ => Err("this isn't a TIFF file".into()),
        }
    }

    fn rd16(&self, b: &[u8]) -> u16 {
        let a = [b[0], b[1]];
        if self.le { u16::from_le_bytes(a) } else { u16::from_be_bytes(a) }
    }

    fn rd32(&self, b: &[u8]) -> u32 {
        let a = [b[0], b[1], b[2], b[3]];
        if self.le { u32::from_le_bytes(a) } else { u32::from_be_bytes(a) }
    }

    fn rd64(&self, b: &[u8]) -> u64 {
        let a: [u8; 8] = b[..8].try_into().unwrap_or([0; 8]);
        if self.le { u64::from_le_bytes(a) } else { u64::from_be_bytes(a) }
    }

    /// Reads the directory at `offset`.
    pub fn ifd(&self, offset: u64) -> Result<Ifd, String> {
        let (count_size, entry_size, next_size) = if self.big { (8u64, 20u64, 8u64) } else { (2, 12, 4) };
        if offset == 0 || offset.checked_add(count_size).is_none_or(|e| e > self.len) {
            return Err("a directory lies outside the file".into());
        }
        let mut cb = [0u8; 8];
        read_exact_at(&self.file, &mut cb[..count_size as usize], offset).map_err(|e| e.to_string())?;
        let n = if self.big { self.rd64(&cb) } else { self.rd16(&cb) as u64 };
        if n > 4096 {
            return Err("a directory claims an absurd number of tags".into());
        }
        let body = n * entry_size + next_size;
        if offset + count_size + body > self.len {
            return Err("a directory runs past the end of the file".into());
        }
        let mut buf = vec![0u8; body as usize];
        read_exact_at(&self.file, &mut buf, offset + count_size).map_err(|e| e.to_string())?;
        let mut entries = Vec::with_capacity(n as usize);
        for i in 0..n as usize {
            let e = &buf[i * entry_size as usize..(i + 1) * entry_size as usize];
            let (count, vs) =
                if self.big { (self.rd64(&e[4..12]), &e[12..20]) } else { (self.rd32(&e[4..8]) as u64, &e[8..12]) };
            let mut raw = [0u8; 8];
            raw[..vs.len()].copy_from_slice(vs);
            let typ = self.rd16(&e[2..4]);
            let inline = count.checked_mul(type_size(typ)).is_some_and(|s| s <= vs.len() as u64);
            entries.push(Entry { tag: self.rd16(&e[0..2]), typ, count, raw, inline });
        }
        let nb = &buf[(n * entry_size) as usize..];
        let next = if self.big { self.rd64(nb) } else { self.rd32(nb) as u64 };
        Ok(Ifd { offset, entries, next })
    }

    /// The offsets of every directory in the main chain, by following the
    /// links; stops at a repeat (a loop) or `limit`.
    pub fn ifd_offsets(&self, limit: usize) -> Result<Vec<u64>, String> {
        let mut out = Vec::new();
        let mut seen = HashSet::new();
        let mut at = self.first_ifd;
        let (count_size, entry_size, next_size) = if self.big { (8u64, 20u64, 8u64) } else { (2, 12, 4) };
        while at != 0 && out.len() < limit.min(MAX_PAGES) && seen.insert(at) {
            // A link may hold any number; past the end ends the chain.
            if at.checked_add(count_size).is_none_or(|e| e > self.len) {
                if out.is_empty() {
                    return Err("the first directory is outside the file".into());
                }
                break;
            }
            let mut cb = [0u8; 8];
            read_exact_at(&self.file, &mut cb[..count_size as usize], at).map_err(|e| e.to_string())?;
            let n = if self.big { self.rd64(&cb) } else { self.rd16(&cb) as u64 };
            if n > 4096 {
                if out.is_empty() {
                    return Err("a directory claims an absurd number of tags".into());
                }
                break;
            }
            let np = at + count_size + n * entry_size;
            if np + next_size > self.len {
                // A file cut off inside a directory keeps the pages before it.
                if out.is_empty() {
                    return Err("the first directory runs past the end of the file".into());
                }
                break;
            }
            let mut nb = [0u8; 8];
            read_exact_at(&self.file, &mut nb[..next_size as usize], np).map_err(|e| e.to_string())?;
            out.push(at);
            at = if self.big { self.rd64(&nb) } else { self.rd32(&nb) as u64 };
        }
        if out.is_empty() {
            return Err("the file has no directories".into());
        }
        Ok(out)
    }

    /// The raw bytes of an entry's value.
    pub fn bytes(&self, e: &Entry) -> Result<Vec<u8>, String> {
        let size = e.count.checked_mul(type_size(e.typ)).ok_or("a tag's size overflows")?;
        if size > MAX_TAG_BYTES {
            return Err("a tag holds an absurd amount of data".into());
        }
        if e.inline {
            return Ok(e.raw[..size as usize].to_vec());
        }
        let off = if self.big { self.rd64(&e.raw) } else { self.rd32(&e.raw) as u64 };
        if off.checked_add(size).is_none_or(|end| end > self.len) {
            return Err("a tag's value lies outside the file".into());
        }
        let mut b = vec![0u8; size as usize];
        read_exact_at(&self.file, &mut b, off).map_err(|e| e.to_string())?;
        Ok(b)
    }

    /// An entry's integers (any integer type), at most `max` of them.
    pub fn ints(&self, e: &Entry, max: usize) -> Result<Vec<u64>, String> {
        if e.count as usize > max {
            return Err(format!("tag {} holds {} values, more than expected", e.tag, e.count));
        }
        let b = self.bytes(e)?;
        let sz = type_size(e.typ) as usize;
        Ok(match e.typ {
            1 | 2 | 6 | 7 => b.iter().map(|&v| v as u64).collect(),
            3 | 8 => b.chunks_exact(2).map(|c| self.rd16(c) as u64).collect(),
            4 | 9 | 13 => b.chunks_exact(4).map(|c| self.rd32(c) as u64).collect(),
            16..=18 => b.chunks_exact(8).map(|c| self.rd64(c)).collect(),
            5 | 10 => b.chunks_exact(8).map(|c| self.rd32(&c[..4]) as u64).collect(),
            _ => return Err(format!("tag {} has a type of {sz} bytes that isn't a number", e.tag)),
        })
    }

    /// An entry's numbers as floats (rationals divided).
    pub fn floats(&self, e: &Entry) -> Vec<f64> {
        let Ok(b) = self.bytes(e) else { return Vec::new() };
        match e.typ {
            5 => b.chunks_exact(8).map(|c| self.rd32(&c[..4]) as f64 / (self.rd32(&c[4..]) as f64).max(1.0)).collect(),
            10 => b
                .chunks_exact(8)
                .map(|c| self.rd32(&c[..4]) as i32 as f64 / (self.rd32(&c[4..]) as i32 as f64).max(1.0))
                .collect(),
            11 => b.chunks_exact(4).map(|c| f32::from_bits(self.rd32(c)) as f64).collect(),
            12 => b.chunks_exact(8).map(|c| f64::from_bits(self.rd64(c))).collect(),
            _ => self.ints(e, 1 << 20).map(|v| v.into_iter().map(|x| x as f64).collect()).unwrap_or_default(),
        }
    }

    pub fn text(&self, e: &Entry) -> String {
        let b = self.bytes(e).unwrap_or_default();
        let end = b.iter().position(|&c| c == 0).unwrap_or(b.len());
        String::from_utf8_lossy(&b[..end]).into_owned()
    }
}

impl Ifd {
    pub fn get(&self, tag: u16) -> Option<&Entry> {
        self.entries.iter().find(|e| e.tag == tag)
    }
}

/// What a directory says about its image, without the strip or tile tables.
#[derive(Clone, Debug)]
pub struct Page {
    pub ifd: Ifd,
    pub width: u64,
    pub height: u64,
    pub bits: Vec<u16>,
    pub samples: usize,
    pub photometric: u16,
    pub compression: u16,
    pub predictor: u16,
    pub planar: u16,
    pub sample_format: u16,
    pub tile: Option<(u64, u64)>,
    pub rows_per_strip: u64,
    pub subfile_type: u32,
    pub extra_samples: Vec<u16>,
    pub description: String,
    pub software: String,
    /// Pixels per unit and the unit (1 none, 2 inch, 3 centimetre).
    pub resolution: Option<(f64, f64, u16)>,
    pub ycbcr_subsampling: (u16, u16),
    /// The luma weights of red, green and blue (0.299, 0.587, 0.114 unless the
    /// file says) and the reference black and white points of Y, Cb and Cr,
    /// for YCbCr pages.
    pub ycbcr_coefficients: [f64; 3],
    pub reference_black_white: [f64; 6],
    /// 2 when the bits of every byte are stored in reverse order.
    pub fill_order: u16,
    /// Group 3 fax options: bit 0 two-dimensional coding, bit 2 fill bits.
    pub t4_options: u32,
    /// Added to every strip and tile offset: ImageJ stores later pages of a
    /// hyperstack after the first page's data with no directory of their own.
    pub offset_shift: u64,
}

impl Tiff {
    /// Reads the directory at `offset` as a page.
    pub fn page(&self, offset: u64) -> Result<Page, String> {
        let ifd = self.ifd(offset)?;
        self.page_of(ifd)
    }

    pub fn page_of(&self, ifd: Ifd) -> Result<Page, String> {
        let num =
            |t: u16| -> Option<u64> { ifd.get(t).and_then(|e| self.ints(e, 1).ok()).and_then(|v| v.first().copied()) };
        let width = num(tag::WIDTH).ok_or("a page has no width")?;
        let height = num(tag::HEIGHT).ok_or("a page has no height")?;
        if width == 0 || height == 0 || width > 1 << 31 || height > 1 << 31 {
            return Err("a page has an absurd size".into());
        }
        let samples = num(tag::SAMPLES).unwrap_or(1) as usize;
        if samples == 0 || samples > 64 {
            return Err(format!("{samples} samples per pixel isn't supported"));
        }
        let bits: Vec<u16> = match ifd.get(tag::BITS) {
            Some(e) => self.ints(e, 64)?.into_iter().map(|b| b as u16).collect(),
            None => vec![1],
        };
        let tile = match (num(tag::TILE_WIDTH), num(tag::TILE_HEIGHT)) {
            (Some(w), Some(h)) if w > 0 && h > 0 && w <= 1 << 20 && h <= 1 << 20 => Some((w, h)),
            _ => None,
        };
        let compression = num(tag::COMPRESSION).unwrap_or(1) as u16;
        let photometric = num(tag::PHOTOMETRIC).unwrap_or(1) as u16;
        let res = match (ifd.get(tag::X_RESOLUTION), ifd.get(tag::Y_RESOLUTION)) {
            (Some(x), Some(y)) => {
                let (fx, fy) = (self.floats(x), self.floats(y));
                match (fx.first(), fy.first()) {
                    (Some(&a), Some(&b)) if a > 0.0 && b > 0.0 => {
                        Some((a, b, num(tag::RESOLUTION_UNIT).unwrap_or(2) as u16))
                    }
                    _ => None,
                }
            }
            _ => None,
        };
        let sub = ifd.get(tag::YCBCR_SUBSAMPLING).and_then(|e| self.ints(e, 2).ok()).filter(|v| v.len() == 2);
        Ok(Page {
            width,
            height,
            bits,
            samples,
            photometric,
            compression,
            predictor: num(tag::PREDICTOR).unwrap_or(1) as u16,
            planar: num(tag::PLANAR).unwrap_or(1) as u16,
            sample_format: ifd
                .get(tag::SAMPLE_FORMAT)
                .and_then(|e| self.ints(e, 64).ok())
                .and_then(|v| v.first().copied())
                .unwrap_or(1) as u16,
            tile,
            rows_per_strip: num(tag::ROWS_PER_STRIP).unwrap_or(height).min(height).max(1),
            subfile_type: num(tag::SUBFILE_TYPE).unwrap_or(0) as u32,
            extra_samples: ifd
                .get(tag::EXTRA_SAMPLES)
                .and_then(|e| self.ints(e, 64).ok())
                .unwrap_or_default()
                .into_iter()
                .map(|v| v as u16)
                .collect(),
            description: ifd.get(tag::DESCRIPTION).map(|e| self.text(e)).unwrap_or_default(),
            software: ifd.get(tag::SOFTWARE).map(|e| self.text(e)).unwrap_or_default(),
            resolution: res,
            ycbcr_subsampling: sub.map_or((2, 2), |v| (v[0] as u16, v[1] as u16)),
            ycbcr_coefficients: ifd
                .get(tag::YCBCR_COEFFICIENTS)
                .map(|e| self.floats(e))
                .filter(|v| v.len() == 3 && v.iter().all(|c| c.is_finite() && *c > 0.0))
                .map_or([0.299, 0.587, 0.114], |v| [v[0], v[1], v[2]]),
            reference_black_white: ifd
                .get(tag::REFERENCE_BLACK_WHITE)
                .map(|e| self.floats(e))
                .filter(|v| v.len() == 6 && v.iter().all(|c| c.is_finite()))
                .map_or([0.0, 255.0, 128.0, 255.0, 128.0, 255.0], |v| [v[0], v[1], v[2], v[3], v[4], v[5]]),
            fill_order: num(tag::FILL_ORDER).unwrap_or(1) as u16,
            t4_options: num(tag::T4_OPTIONS).unwrap_or(0) as u32,
            offset_shift: 0,
            ifd,
        })
    }
}

impl Page {
    pub fn is_tiled(&self) -> bool {
        self.tile.is_some()
    }

    /// Tiles or strips across and down, and the planes stored apart.
    pub fn chunk_grid(&self) -> (u64, u64, u64) {
        let planes = if self.planar == 2 { self.samples as u64 } else { 1 };
        match self.tile {
            Some((tw, th)) => (self.width.div_ceil(tw), self.height.div_ceil(th), planes),
            None => (1, self.height.div_ceil(self.rows_per_strip), planes),
        }
    }

    pub fn chunk_count(&self) -> u64 {
        let (a, d, p) = self.chunk_grid();
        a.saturating_mul(d).saturating_mul(p)
    }

    /// The offsets and byte counts of the strips or tiles.
    pub fn chunk_table(&self, t: &Tiff) -> Result<(Vec<u64>, Vec<u64>), String> {
        let (otag, ctag) = if self.is_tiled() {
            (tag::TILE_OFFSETS, tag::TILE_COUNTS)
        } else {
            (tag::STRIP_OFFSETS, tag::STRIP_COUNTS)
        };
        let n = self.chunk_count();
        if n == 0 || n > 1 << 24 {
            return Err("the page has an absurd number of strips or tiles".into());
        }
        let offs = t.ints(self.ifd.get(otag).ok_or("the page has no strip or tile offsets")?, n as usize)?;
        let counts = match self.ifd.get(ctag) {
            Some(e) => t.ints(e, n as usize)?,
            // A single uncompressed strip may omit its length.
            None if offs.len() == 1 && self.compression == 1 => vec![self.raw_bytes()],
            None => return Err("the page has no strip or tile byte counts".into()),
        };
        if offs.len() as u64 != n || counts.len() as u64 != n {
            return Err(format!("the page has {} offsets but {} strips or tiles", offs.len(), n));
        }
        let offs = offs.into_iter().map(|o| o.saturating_add(self.offset_shift)).collect();
        Ok((offs, counts))
    }

    /// The bytes an uncompressed page takes.
    pub fn raw_bytes(&self) -> u64 {
        let bps = self.bits.first().copied().unwrap_or(8) as u64;
        let row = (self.width * self.samples as u64 * bps).div_ceil(8);
        row.saturating_mul(self.height)
    }

    pub fn pixels(&self) -> u64 {
        self.width.saturating_mul(self.height)
    }
}

/// The number of pages (directories in the main chain) of a TIFF file.
pub fn page_count(path: &Path) -> Result<usize, String> {
    Tiff::open(path)?.ifd_offsets(MAX_PAGES).map(|o| o.len())
}

/// Decodes page `page` (counting from 0) of a TIFF file.
pub fn decode_page(path: &Path, page: usize, options: &Options) -> Result<Raster, String> {
    let tiff = Tiff::open(path)?;
    let offsets = tiff.ifd_offsets(MAX_PAGES)?;
    let offset = *offsets.get(page).ok_or_else(|| format!("the file has {} pages, not {}", offsets.len(), page + 1))?;
    tiff.page(offset)?.decode(&tiff, options)
}
