//! Luminance-chroma files whose chroma channels are stored at reduced
//! resolution (OpenEXR's `RgbaOutputFile` with `WRITE_YC`), which the `exr`
//! crate refuses to open.  Their header is read here, and each scan-line
//! block is decompressed by `exr`'s own codecs through a header built for
//! the purpose: the codecs follow each channel's sampling already, and only
//! the expected block size `exr` checks them against has to be told.  B44
//! is the exception (its codec ignores sampling), and is refused.

use super::*;
use exr::math::Vec2;
use exr::meta::attribute::{ChannelDescription, Chromaticities, Text};
use std::io::SeekFrom;

/// The largest such file read into memory.
const MAX_BYTES: u64 = 1 << 30;

pub(super) struct File {
    pub header: Header,
    offsets: Vec<u64>,
}

pub(super) fn read_all<R: Read + Seek>(r: &mut R) -> Result<Vec<u8>, String> {
    r.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
    let mut bytes = Vec::new();
    r.take(MAX_BYTES + 1).read_to_end(&mut bytes).map_err(|e| e.to_string())?;
    if bytes.len() as u64 > MAX_BYTES {
        return Err("the file is too large".into());
    }
    Ok(bytes)
}

struct Cur<'a> {
    d: &'a [u8],
    at: usize,
}

impl<'a> Cur<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], String> {
        let end = self.at.checked_add(n).filter(|&e| e <= self.d.len()).ok_or("the header is cut short")?;
        let s = &self.d[self.at..end];
        self.at = end;
        Ok(s)
    }
    fn i32(&mut self) -> Result<i32, String> {
        let b = self.take(4)?;
        Ok(i32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }
    fn f32(&mut self) -> Result<f32, String> {
        Ok(f32::from_bits(self.i32()? as u32))
    }
    fn name(&mut self) -> Result<&'a str, String> {
        let rest = &self.d[self.at.min(self.d.len())..];
        let n = rest.iter().take(256).position(|&b| b == 0).ok_or("a name is too long")?;
        let s = std::str::from_utf8(&rest[..n]).map_err(|_| "a name isn't text")?;
        self.at += n + 1;
        Ok(s)
    }
}

/// Reads the header and offset table of a single-part scan-line file.
pub(super) fn parse(d: &[u8]) -> Result<File, String> {
    let mut c = Cur { d, at: 0 };
    if c.take(4)? != [0x76, 0x2f, 0x31, 0x01] {
        return Err("not an OpenEXR file".into());
    }
    let flags = c.i32()?;
    if flags & 0x1a00 != 0 {
        return Err("chroma-subsampled files that are tiled, deep or multi-part aren't supported".into());
    }
    let mut channels: Vec<ChannelDescription> = Vec::new();
    let mut compression = Compression::Uncompressed;
    let (mut data, mut display) = (None, None);
    let mut chromaticities = None;
    loop {
        let name = c.name()?;
        if name.is_empty() {
            break;
        }
        let _kind = c.name()?;
        let size = c.i32()?;
        let size = usize::try_from(size).map_err(|_| "a negative attribute size")?;
        let value = c.take(size)?;
        let mut v = Cur { d: value, at: 0 };
        match name {
            "channels" => loop {
                let n = v.name()?;
                if n.is_empty() {
                    break;
                }
                let sample = match v.i32()? {
                    0 => SampleType::U32,
                    1 => SampleType::F16,
                    2 => SampleType::F32,
                    t => return Err(format!("channel type {t}")),
                };
                let linear = v.take(4)?[0] != 0;
                let (sx, sy) = (v.i32()?, v.i32()?);
                if !(1..=64).contains(&sx) || !(1..=64).contains(&sy) {
                    return Err("a bad channel sampling".into());
                }
                let text = Text::new_or_none(n).ok_or("a channel name isn't Latin-1")?;
                let mut ch = ChannelDescription::new(text, sample, linear);
                ch.sampling = Vec2(sx as usize, sy as usize);
                channels.push(ch);
                if channels.len() > MAX_CHANNELS {
                    return Err("too many channels".into());
                }
            },
            "compression" => {
                compression = match value.first().copied().unwrap_or(0) {
                    0 => Compression::Uncompressed,
                    1 => Compression::RLE,
                    2 => Compression::ZIP1,
                    3 => Compression::ZIP16,
                    4 => Compression::PIZ,
                    5 => Compression::PXR24,
                    6 => Compression::B44,
                    7 => Compression::B44A,
                    8 => Compression::DWAA(None),
                    9 => Compression::DWAB(None),
                    m => return Err(format!("compression {m}")),
                }
            }
            "dataWindow" | "displayWindow" => {
                let (x0, y0, x1, y1) = (v.i32()?, v.i32()?, v.i32()?, v.i32()?);
                let w = (x1 as i64 - x0 as i64 + 1).clamp(0, 1 << 24) as usize;
                let h = (y1 as i64 - y0 as i64 + 1).clamp(0, 1 << 24) as usize;
                let b = IntegerBounds::new((x0, y0), (w, h));
                if name == "dataWindow" { data = Some(b) } else { display = Some(b) }
            }
            "chromaticities" => {
                let f: Vec<f32> = (0..8).map(|_| v.f32()).collect::<Result<_, _>>()?;
                chromaticities = Some(Chromaticities {
                    red: Vec2(f[0], f[1]),
                    green: Vec2(f[2], f[3]),
                    blue: Vec2(f[4], f[5]),
                    white: Vec2(f[6], f[7]),
                });
            }
            _ => {}
        }
    }
    let data = data.ok_or("the file has no data window")?;
    if data.size.area() == 0 {
        return Err("an empty data window".into());
    }
    let lines = compression.scan_lines_per_block();
    let count = data.size.height().div_ceil(lines);
    if count > MAX_CHUNKS {
        return Err("too many blocks".into());
    }
    let mut offsets = Vec::with_capacity(count);
    for _ in 0..count {
        let b = c.take(8)?;
        offsets.push(u64::from_le_bytes(b.try_into().unwrap_or([0; 8])));
    }
    let mut header = Header::new(Text::new_or_panic(""), data.size, channels.into());
    header.compression = compression;
    header.blocks = BlockDescription::ScanLines;
    header.own_attributes.layer_name = None;
    header.own_attributes.layer_position = data.position;
    header.shared_attributes.display_window = display.unwrap_or(data);
    header.shared_attributes.chromaticities = chromaticities;
    header.chunk_count = count;
    Ok(File { header, offsets })
}

/// Decodes layer `layer` of a parsed file.
pub(super) fn decode(d: &[u8], file: &File, layer: usize, opts: &Options) -> Result<Picture, String> {
    let header = &file.header;
    let info = part_of(header)?;
    let lay = info.layers.get(layer).ok_or("no such layer")?;
    let (map, meaning) = lay.mapping();
    let wanted: Vec<Option<usize>> = map.iter().map(|m| m.map(|i| lay.channels[i].index)).collect();
    if matches!(header.compression, Compression::B44 | Compression::B44A) {
        return Err("chroma-subsampled pictures compressed with B44 aren't supported".into());
    }
    let data = header.data_window();
    let (x0, y0) = (data.position.x() as i64, data.position.y() as i64);
    let (w, h) = (data.size.width(), data.size.height());
    if (w as u64).saturating_mul(h as u64) > opts.max_pixels {
        return Err(format!("a picture of {w} × {h} pixels is too large"));
    }
    let mut level = Level { width: w as u32, height: h as u32, rgba: vec![0f32; w * h * 4] };
    if map[3].is_none() {
        level.rgba.chunks_exact_mut(4).for_each(|p| p[3] = 1.0);
    }
    let channels = &header.channels.list;
    let bytes_of = |c: usize| if channels[c].sample_type == SampleType::F16 { 2 } else { 4 };
    // Samples of channel `c` on a line: the columns that are multiples of
    // its sampling.
    let count = |c: usize| {
        let s = channels[c].sampling.x() as i64;
        let (a, b) = (x0, x0 + w as i64 - 1);
        (b.div_euclid(s) - (a + s - 1).div_euclid(s) + 1).max(0) as usize
    };
    let lines = header.compression.scan_lines_per_block() as i64;
    let mut notes = Vec::new();
    for &offset in &file.offsets {
        let mut c = Cur { d, at: usize::try_from(offset).unwrap_or(usize::MAX) };
        let (Ok(top), Ok(size)) = (c.i32(), c.i32()) else {
            notes.push("some blocks lie outside the file".to_string());
            continue;
        };
        let Ok(packed) = c.take(size.max(0) as usize) else {
            notes.push("the file is cut short".to_string());
            continue;
        };
        let top = top as i64;
        let rows: Vec<i64> = (top..top + lines).filter(|&y| y >= y0 && y < y0 + h as i64).collect();
        if rows.is_empty() {
            continue;
        }
        // 1. The block's real size, which `exr` checks the codec's output
        //    against as width × lines × bytes per pixel.
        let mut real = 0usize;
        for &y in &rows {
            for ch in 0..channels.len() {
                if y.rem_euclid(channels[ch].sampling.y() as i64) == 0 {
                    real += count(ch) * bytes_of(ch);
                }
            }
        }
        let area = w * rows.len();
        if !real.is_multiple_of(area) {
            return Err("this chroma subsampling isn't supported".into());
        }
        let mut block_header = header.clone();
        block_header.channels.bytes_per_pixel = real / area;
        let rect = IntegerBounds::new((x0 as i32, rows[0] as i32), (w, rows.len()));
        if !crate::dwa_counts_fit(header.compression, packed, real) {
            notes.push("some blocks couldn't be read (a DWA block claims more data than it can hold)".to_string());
            continue;
        }
        let bytes =
            match header.compression.decompress_image_section_from_le(&block_header, packed.to_vec(), rect, false) {
                Ok(b) => b,
                Err(e) => {
                    notes.push(format!("some blocks couldn't be read ({e})"));
                    continue;
                }
            };
        // 2. Spread each wanted channel's samples over the pixels they
        //    stand for.
        let mut at = 0usize;
        for &y in &rows {
            for ch in 0..channels.len() {
                let (sx, sy) = (channels[ch].sampling.x(), channels[ch].sampling.y());
                if y.rem_euclid(sy as i64) != 0 {
                    continue;
                }
                let n = count(ch);
                let len = n * bytes_of(ch);
                let Some(run) = bytes.get(at..at + len) else { break };
                at += len;
                for slot in (0..4).filter(|&s| wanted[s] == Some(ch)) {
                    let first_x = (x0 + sx as i64 - 1).div_euclid(sx as i64) * sx as i64 - x0;
                    for i in 0..n {
                        let v = read_sample(run, i, channels[ch].sample_type);
                        for dy in 0..sy {
                            let row = (y - y0) as usize + dy;
                            if row >= h {
                                break;
                            }
                            for dx in 0..sx {
                                let col = first_x as usize + i * sx + dx;
                                if col < w {
                                    level.rgba[(row * w + col) * 4 + slot] = v;
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    notes.dedup();
    finish(vec![level], header, map[3].is_some(), meaning, true, opts, notes)
}
