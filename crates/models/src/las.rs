//! LAS lidar point clouds (versions 1.0 to 1.4, point formats 0 to 10) and
//! their LASzip-compressed form, LAZ.
//!
//! The header and point records are read here; LAZ points are decompressed
//! by the `laz` crate, in parallel across its chunks, a batch at a time so
//! memory stays bounded.  Coordinates are stored as integers with a scale and
//! offset per axis, usually in a projected coordinate system in metres, Z
//! up.

use crate::common::Reader;
use crate::points::Cloud;
use glam::DVec3;
use slerp_scene::{Limits, LoadError, Scene};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::Path;

/// Bytes of points decompressed per batch: enough chunks (50,000 points
/// each, typically) to keep every core busy.
const BATCH_BYTES: usize = 48 << 20;

/// True for the `LASF` signature with a 1.x version.
pub fn looks_like_las(head: &[u8]) -> bool {
    head.len() >= 26 && head.starts_with(b"LASF") && head[24] == 1 && head[25] <= 4
}

struct Header {
    minor: u8,
    header_size: usize,
    point_offset: usize,
    vlr_count: u32,
    format: u8,
    compressed: bool,
    record_len: usize,
    count: u64,
    scale: DVec3,
    offset: DVec3,
    software: String,
}

fn header(bytes: &[u8]) -> Result<Header, LoadError> {
    if !looks_like_las(bytes) || bytes.len() < 227 {
        return Err(LoadError::invalid("LAS header is truncated"));
    }
    let mut r = Reader::new(bytes);
    let at = |r: &mut Reader, pos: usize| {
        r.pos = pos;
    };
    at(&mut r, 94);
    let header_size = r.u16().unwrap_or(0) as usize;
    let point_offset = r.u32().unwrap_or(0) as usize;
    let vlr_count = r.u32().unwrap_or(0);
    let format_byte = r.u8().unwrap_or(0);
    let record_len = r.u16().unwrap_or(0) as usize;
    let legacy_count = r.u32().unwrap_or(0) as u64;
    at(&mut r, 131);
    let mut f = [0f64; 6];
    for v in &mut f {
        *v = r.f64().unwrap_or(f64::NAN);
    }
    let minor = bytes[25];
    let mut count = legacy_count;
    if minor >= 4 && header_size >= 255 && bytes.len() >= 255 {
        at(&mut r, 247);
        let long = r.array::<8>().map(u64::from_le_bytes).unwrap_or(0);
        if long > 0 {
            count = long;
        }
    }
    let software: String = bytes[58..90].iter().take_while(|&&c| c != 0).map(|&c| c as char).collect();
    let scale = DVec3::new(f[0], f[1], f[2]);
    if !scale.is_finite() || scale.abs().min_element() == 0.0 || !DVec3::new(f[3], f[4], f[5]).is_finite() {
        return Err(LoadError::invalid("LAS header has an unusable scale or offset"));
    }
    Ok(Header {
        minor,
        header_size,
        point_offset,
        vlr_count,
        format: format_byte & 0x3F,
        compressed: format_byte & 0xC0 != 0,
        record_len,
        count,
        scale,
        offset: DVec3::new(f[3], f[4], f[5]),
        software: software.trim().to_string(),
    })
}

/// Where a point format keeps RGB, and how long its fixed part is.
fn layout(format: u8) -> Option<(usize, Option<usize>)> {
    Some(match format {
        0 => (20, None),
        1 => (28, None),
        2 => (26, Some(20)),
        3 => (34, Some(28)),
        4 => (57, None),
        5 => (63, Some(28)),
        6 => (30, None),
        7 => (36, Some(30)),
        8 => (38, Some(30)),
        9 => (59, None),
        10 => (67, Some(30)),
        _ => return None,
    })
}

/// Decodes one point record into the cloud.
#[inline]
fn add(cloud: &mut Cloud, h: &Header, rgb: Option<usize>, p: &[u8]) {
    let i32_at = |o: usize| i32::from_le_bytes([p[o], p[o + 1], p[o + 2], p[o + 3]]) as f64;
    let u16_at = |o: usize| u16::from_le_bytes([p[o], p[o + 1]]);
    let pos = DVec3::new(i32_at(0), i32_at(4), i32_at(8)) * h.scale + h.offset;
    let color = rgb.map(|o| [u16_at(o), u16_at(o + 2), u16_at(o + 4)]);
    cloud.push(pos, color, Some(u16_at(12)));
}

/// The LASzip description among the variable-length records.
fn laszip_vlr<'a>(bytes: &'a [u8], h: &Header) -> Option<&'a [u8]> {
    let mut at = h.header_size;
    for _ in 0..h.vlr_count.min(65_536) {
        let head = bytes.get(at..at.checked_add(54)?)?;
        let user = &head[2..18];
        let record = u16::from_le_bytes([head[18], head[19]]);
        let len = u16::from_le_bytes([head[20], head[21]]) as usize;
        let data = bytes.get(at + 54..at + 54 + len)?;
        if user.starts_with(b"laszip encoded") && record == 22204 {
            return Some(data);
        }
        at += 54 + len;
    }
    None
}

pub fn load(_path: &Path, bytes: &[u8], limits: &Limits) -> Result<Scene, LoadError> {
    // 1. Header and point layout.
    let h = header(bytes)?;
    let (fixed, rgb) =
        layout(h.format).ok_or_else(|| LoadError::Unsupported(format!("LAS point format {}", h.format)))?;
    if h.record_len < fixed {
        return Err(LoadError::invalid(format!(
            "LAS records of {} bytes are too short for format {}",
            h.record_len, h.format
        )));
    }
    let data =
        bytes.get(h.point_offset..).ok_or_else(|| LoadError::invalid("LAS point data offset is past the end"))?;
    let mut warnings = Vec::new();

    // 2. Points, every one the cloud asks for.
    let cloud = if h.compressed {
        let vlr = laszip_vlr(bytes, &h).ok_or_else(|| LoadError::invalid("LAZ file has no LASzip record"))?;
        read_laz(bytes, data, vlr, &h, rgb, limits, &mut warnings)?
    } else {
        let present = (data.len() / h.record_len) as u64;
        if present < h.count {
            warnings.push(format!("header says {} points, file holds {present}", h.count));
        }
        let n = h.count.min(present);
        let mut cloud = Cloud::new(Some(n), limits);
        for record in data.chunks_exact(h.record_len).take(n as usize) {
            if cloud.wants() {
                add(&mut cloud, &h, rgb, record);
            }
        }
        cloud
    };

    // 3. The scene.
    let format = format!("{} 1.{} (point format {})", if h.compressed { "LAZ" } else { "LAS" }, h.minor, h.format);
    let mut scene = cloud.finish(&format, true)?;
    if !h.software.is_empty() {
        scene.info.generator = Some(h.software.clone());
    }
    scene.info.warnings = warnings;
    Ok(scene)
}

/// Decompresses LAZ points batch by batch.  The chunk table is checked
/// first: `laz` reserves memory for as many chunks as it claims.
fn read_laz(
    bytes: &[u8],
    data: &[u8],
    vlr_bytes: &[u8],
    h: &Header,
    rgb: Option<usize>,
    limits: &Limits,
    warnings: &mut Vec<String>,
) -> Result<Cloud, LoadError> {
    if vlr_bytes.len() < 34 {
        return Err(LoadError::invalid("LASzip record is truncated"));
    }
    // Point-wise compression (LASzip 1.x) has no chunks, but `laz` takes
    // its chunk size of 0 for variable-sized chunks and then panics for the
    // missing table; any fixed size reads it right.
    let compressor = u16::from_le_bytes([vlr_bytes[0], vlr_bytes[1]]);
    let mut vlr_bytes = vlr_bytes.to_vec();
    if compressor == 1 {
        vlr_bytes[12..16].copy_from_slice(&50_000u32.to_le_bytes());
    }
    let vlr = catch_unwind(|| laz::LazVlr::from_buffer(&vlr_bytes))
        .map_err(|_| LoadError::invalid("LASzip record is damaged"))?
        .map_err(|e| LoadError::invalid(format!("LASzip record: {e}")))?;
    // Summed here: `items_size` adds the items' sizes as a u16, which a
    // damaged record overflows (wrapping to a plausible size in release).
    let size: usize = vlr.items().iter().map(|i| i.size() as usize).sum();
    if size < h.record_len.min(20) || size > 1024 {
        return Err(LoadError::invalid("LASzip record size doesn't match the points"));
    }
    // 1. Chunked data starts with the chunk table's offset.  Every chunk
    //    starts with a whole point stored raw, so the table can't list more
    //    chunks than fit between the data and the table.
    let chunked = matches!(compressor, 2 | 3);
    let table_fits = |at: i64| {
        at > h.point_offset as i64 && (at as u64) < bytes.len() as u64 && {
            let chunks = bytes[at as usize..].get(4..8).map(|b| u32::from_le_bytes(b.try_into().unwrap_or_default()));
            let room = (at as u64 - h.point_offset as u64) / size as u64 + 1;
            chunks.is_some_and(|c| c as u64 <= room)
        }
    };
    let offset_at = |b: Option<&[u8]>| b.map(|b| i64::from_le_bytes(b.try_into().unwrap_or_default())).unwrap_or(-1);
    let table_ok = chunked && table_fits(offset_at(data.get(..8)));
    if chunked && !table_ok && vlr.uses_variable_size_chunks() {
        return Err(LoadError::invalid("LAZ chunk table is missing or damaged"));
    }
    // Without a usable offset in front, `laz` takes one from the file's
    // last eight bytes (where writers that can't seek back put it) and
    // reserves memory for as many chunks as the table there claims.
    let trailing = offset_at(bytes.get(bytes.len().saturating_sub(8)..));
    if chunked && !table_ok && trailing > h.point_offset as i64 && !table_fits(trailing) {
        return Err(LoadError::invalid("LAZ chunk table is missing or damaged"));
    }
    // A table the check refused is hidden from `laz` by blanking its offset,
    // which makes it read fixed-size chunks in order without one.
    let source = Patched { bytes, pos: h.point_offset as u64, blank: (chunked && !table_ok).then_some(h.point_offset) };
    // `laz` reserves a buffer as large as the largest chunk's point count,
    // and allocates each layer's stated length (layered chunks) before
    // reading it: both are checked against the data first, since one
    // damaged number otherwise asks for terabytes, and a failed allocation
    // aborts the process.  Compressed points take two bits or more.
    let table = if table_ok { chunk_table(bytes, h.point_offset as u64, &vlr) } else { None };
    let most_points = data.len() as u64 * 4;
    let fixed_too_big = !vlr.uses_variable_size_chunks() && u64::from(vlr.chunk_size()) > most_points.max(1 << 24);
    if fixed_too_big || table.as_ref().is_some_and(|t| t.iter().any(|e| e.point_count > most_points)) {
        return Err(LoadError::invalid("the LAZ chunk table claims more points than the file holds"));
    }
    let n = h.count.min(data.len() as u64 * 4);
    if compressor == 3 && !layers_fit(bytes, h.point_offset as u64, table.as_deref(), &vlr, n) {
        return Err(LoadError::invalid("a LAZ chunk claims more data than the file holds"));
    }

    // 2. Batches of points, decompressed in parallel across chunks when the
    //    table allows.  Compressed points take at least two bits or so
    //    each, so the stated count is capped by the data's size (`n`).
    //
    //    `laz` isn't hardened against damaged data (it can divide by zero or
    //    trip its own assertions), so every call into it is guarded: a panic
    //    ends the decoding with the points read so far.
    let mut cloud = Cloud::new(Some(n), limits);
    let batch = (BATCH_BYTES / size).max(1);
    let mut buffer = vec![0u8; batch.min(n as usize).max(1) * size];
    let mut done = 0u64;
    let made = catch_unwind(AssertUnwindSafe(move || -> Result<Decompress, LoadError> {
        let fail = |e: laz::LasZipError| LoadError::invalid(format!("LAZ: {e}"));
        // Only what is drawn: the fields of the layered 1.4 formats are
        // compressed apart, so the others needn't be decoded at all.
        let wanted = laz::DecompressionSelection::base().decompress_z().decompress_intensity().decompress_rgb();
        Ok(if table_ok {
            let mut d = laz::ParLasZipDecompressor::selective(source, vlr, wanted).map_err(fail)?;
            Box::new(move |out: &mut [u8]| d.decompress_many(out).map_err(|e| e.to_string()))
        } else {
            let mut d = laz::LasZipDecompressor::selective(source, vlr, wanted).map_err(fail)?;
            Box::new(move |out: &mut [u8]| d.decompress_many(out).map_err(|e| e.to_string()))
        })
    }));
    let mut decompress = made.map_err(|_| LoadError::invalid("LAZ: damaged compressed data"))??;
    while done < n {
        let count = ((n - done) as usize).min(batch);
        let out = &mut buffer[..count * size];
        match catch_unwind(AssertUnwindSafe(|| decompress(out))) {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                warnings.push(format!("LAZ data damaged after {done} points: {e}"));
                break;
            }
            Err(_) => {
                warnings.push(format!("LAZ data damaged after {done} points"));
                break;
            }
        }
        for record in out.chunks_exact(size) {
            if cloud.wants() {
                add(&mut cloud, h, rgb, record);
            }
        }
        done += count as u64;
    }
    Ok(cloud)
}

/// Decompresses the next points into a buffer, sequentially or in parallel.
type Decompress<'a> = Box<dyn FnMut(&mut [u8]) -> Result<(), String> + 'a>;

/// The chunk table as `laz` reads it, or `None` when it can't be read.
fn chunk_table(bytes: &[u8], point_offset: u64, vlr: &laz::LazVlr) -> Option<Vec<laz::laszip::ChunkTableEntry>> {
    let src = Patched { bytes, pos: point_offset, blank: None };
    catch_unwind(AssertUnwindSafe(|| laz::laszip::ChunkTable::read_from(src, vlr)))
        .ok()
        .and_then(Result::ok)
        .map(|t| t.as_ref().to_vec())
}

/// Whether the layer lengths stated by every chunk of a layered LAZ file
/// (point formats 6 to 10) fit in the file.  `laz` allocates each layer's
/// stated length before reading it, so one damaged length made a 3 KB file
/// ask for gigabytes.  The chunks are found as `laz` finds them: through
/// the chunk table when it is used, else one after another by their own
/// lengths, as far as the `points` that will be read.  The walk stops there:
/// what follows the last chunk (usually the chunk table itself) isn't a
/// chunk, and reading it as one refused good files.
fn layers_fit(
    bytes: &[u8],
    point_offset: u64,
    table: Option<&[laz::laszip::ChunkTableEntry]>,
    vlr: &laz::LazVlr,
    points: u64,
) -> bool {
    use laz::LazItemType as T;
    let len = bytes.len() as u64;
    // 1. A chunk starts with its first point raw and its point count, then
    //    one length per layer of each item.
    let raw: u64 = vlr.items().iter().map(|i| i.size() as u64).sum();
    let layers: u64 = vlr
        .items()
        .iter()
        .map(|i| match i.item_type() {
            T::Point14 => 9,
            T::RGBNIR14 => 2,
            T::RGB14 | T::WavePacket14 => 1,
            T::Byte14(n) => n as u64,
            _ => 0,
        })
        .sum();
    // 2. The chunks' sizes from the table, when `laz` will use it.
    let sizes: Option<Vec<u64>> = table.map(|t| t.iter().map(|e| e.byte_count).collect());
    // 3. Each chunk's stated lengths against the bytes after them.  Without
    //    a table, a chunk is counted as no more points than it states or
    //    the fixed chunk size allows, so damage can only lengthen the walk.
    let mut at = point_offset.saturating_add(8);
    let mut covered = 0u64;
    for k in 0..1 << 20 {
        let past_table = sizes.as_ref().is_some_and(|s| k >= s.len());
        if past_table || (sizes.is_none() && covered >= points) {
            break;
        }
        if let Some(b) = bytes.get(at.saturating_add(raw) as usize..).and_then(|b| b.get(..4)) {
            let stated = u32::from_le_bytes([b[0], b[1], b[2], b[3]]);
            covered += u64::from(stated.min(vlr.chunk_size()));
        }
        let head = at.saturating_add(raw + 4);
        let Some(end) = head.checked_add(4 * layers).filter(|&e| e <= len) else { break };
        let stated: u64 = bytes[head as usize..end as usize]
            .chunks_exact(4)
            .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as u64)
            .sum();
        if end + stated > len {
            return false;
        }
        let next = match &sizes {
            Some(s) => at.saturating_add(s[k]),
            None => end + stated,
        };
        if next <= at {
            break;
        }
        at = next;
    }
    true
}

/// The file as a seekable stream, with the eight bytes at `blank` read as
/// zeros.
#[derive(Clone)]
struct Patched<'a> {
    bytes: &'a [u8],
    pos: u64,
    blank: Option<usize>,
}

impl std::io::Read for Patched<'_> {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        let start = (self.pos as usize).min(self.bytes.len());
        let n = out.len().min(self.bytes.len() - start);
        out[..n].copy_from_slice(&self.bytes[start..start + n]);
        if let Some(b) = self.blank {
            for (i, byte) in out[..n].iter_mut().enumerate() {
                if (b..b + 8).contains(&(start + i)) {
                    *byte = 0;
                }
            }
        }
        self.pos += n as u64;
        Ok(n)
    }
}

impl std::io::Seek for Patched<'_> {
    fn seek(&mut self, to: std::io::SeekFrom) -> std::io::Result<u64> {
        let pos = match to {
            std::io::SeekFrom::Start(p) => Some(p),
            std::io::SeekFrom::End(d) => (self.bytes.len() as u64).checked_add_signed(d),
            std::io::SeekFrom::Current(d) => self.pos.checked_add_signed(d),
        };
        self.pos = pos.ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "seek before start"))?;
        Ok(self.pos)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A LAS 1.2 file of format 2 points (with RGB) on a 10 m grid far from
    /// the origin.
    pub(crate) fn sample(points: usize) -> Vec<u8> {
        let mut b = vec![0u8; 227];
        b[..4].copy_from_slice(b"LASF");
        b[24] = 1;
        b[25] = 2;
        b[58..63].copy_from_slice(b"slerp");
        b[94..96].copy_from_slice(&227u16.to_le_bytes());
        b[96..100].copy_from_slice(&227u32.to_le_bytes());
        b[104] = 2;
        b[105..107].copy_from_slice(&26u16.to_le_bytes());
        b[107..111].copy_from_slice(&(points as u32).to_le_bytes());
        for (i, v) in [0.01f64, 0.01, 0.01, 500_000.0, 4_000_000.0, 0.0].iter().enumerate() {
            b[131 + i * 8..139 + i * 8].copy_from_slice(&v.to_le_bytes());
        }
        for i in 0..points {
            let mut p = [0u8; 26];
            p[0..4].copy_from_slice(&((i % 10) as i32 * 1000).to_le_bytes());
            p[4..8].copy_from_slice(&((i / 10) as i32 * 1000).to_le_bytes());
            p[8..12].copy_from_slice(&(i as i32).to_le_bytes());
            p[12..14].copy_from_slice(&(i as u16).to_le_bytes());
            p[20..22].copy_from_slice(&65535u16.to_le_bytes());
            b.extend_from_slice(&p);
        }
        b
    }

    #[test]
    fn reads_points_recentred_and_coloured() {
        let bytes = sample(100);
        let s = load(Path::new("a.las"), &bytes, &Limits::viewer()).unwrap();
        assert_eq!(s.info.format, "LAS 1.2 (point format 2)");
        assert_eq!(s.info.generator.as_deref(), Some("slerp"));
        let p = &s.meshes[0].primitives[0];
        assert_eq!(p.positions.len(), 100);
        assert_eq!(p.colors[0], glam::Vec4::new(1.0, 0.0, 0.0, 1.0));
        // A 90 m square, recentred: within 45 m of the origin.
        assert!(p.positions.iter().all(|q| q.abs().max_element() <= 45.01), "{:?}", p.positions[0]);
        assert!(s.info.details.iter().any(|d| d.0 == "Origin offset" && d.1.starts_with("500045.000")));
        let thumb = Limits { max_splats: 10, ..Limits::thumbnail() };
        let s = load(Path::new("a.las"), &bytes, &thumb).unwrap();
        assert_eq!(s.meshes[0].primitives[0].positions.len(), 20);
    }

    /// Found by fuzzing: a layered LAZ chunk stating a layer longer than
    /// the file made `laz` allocate it (2.2 GB for a 33 KB file).  PDAL's
    /// COPC sample from the test corpus, with one layer length raised.
    #[test]
    fn layer_lengths_past_the_file_are_refused() {
        let p = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../local/assets/models-extra/pointclouds/pdal/copc/1.2-with-color.copc.laz");
        let Ok(mut b) = std::fs::read(&p) else { return };
        let h = header(&b).unwrap();
        let vlr = laz::LazVlr::from_buffer(laszip_vlr(&b, &h).unwrap()).unwrap();
        let at = h.point_offset as usize + 8 + vlr.items_size() as usize + 4;
        b[at..at + 4].copy_from_slice(&0x7fff_ffffu32.to_le_bytes());
        let result = load(&p, &b, &Limits::thumbnail());
        assert!(matches!(&result, Err(e) if e.to_string().contains("claims more data")), "{:?}", result.err());
    }

    /// PDAL's Autzen sample (point format 7, 214 chunks): its chunk table
    /// follows the last chunk, and reading the table's bytes as one more
    /// chunk's layer lengths refused the file.
    #[test]
    fn a_chunk_table_after_the_last_chunk_is_not_a_chunk() {
        let p = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../local/assets/models-extra/pointclouds/pdal/big/autzen-classified.laz");
        let Ok(b) = std::fs::read(&p) else { return };
        let h = header(&b).unwrap();
        let vlr = laz::LazVlr::from_buffer(laszip_vlr(&b, &h).unwrap()).unwrap();
        let table = chunk_table(&b, h.point_offset as u64, &vlr).unwrap();
        assert_eq!(table.len(), 214);
        assert!(layers_fit(&b, h.point_offset as u64, Some(&table), &vlr, h.count));
        assert!(layers_fit(&b, h.point_offset as u64, None, &vlr, h.count));
    }

    /// Found by fuzzing: LASzip items whose sizes add up past 65,535
    /// overflowed `laz`'s own u16 sum of them.
    #[test]
    fn item_sizes_past_a_u16_are_refused() {
        let p = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../local/assets/models-extra/pointclouds/pdal/copc/1.2-with-color.copc.laz");
        let Ok(mut b) = std::fs::read(&p) else { return };
        let h = header(&b).unwrap();
        let at = laszip_vlr(&b, &h).unwrap().as_ptr() as usize - b.as_ptr() as usize;
        // The first item's size, after the 34-byte record head and its type.
        b[at + 36..at + 38].copy_from_slice(&u16::MAX.to_le_bytes());
        assert!(load(&p, &b, &Limits::thumbnail()).is_err());
    }

    /// Found by fuzzing: a chunk table claiming more points in a chunk than
    /// the address space holds, for which `laz` reserved a buffer.
    #[test]
    fn chunk_point_counts_past_the_data_are_refused() {
        let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fuzz/laz-chunk-point-count.laz");
        let b = std::fs::read(&p).unwrap();
        let result = load(&p, &b, &Limits::thumbnail());
        assert!(matches!(&result, Err(e) if e.to_string().contains("claims more points")), "{:?}", result.err());
    }

    /// Found by fuzzing: with the chunk table's offset in front refused,
    /// `laz` read one from the last eight bytes and reserved memory for the
    /// four billion chunks the table there claimed.  PDAL's `simple.laz`
    /// from the test corpus, damaged that way.
    #[test]
    fn trailing_chunk_table_claiming_too_much_is_refused() {
        let p = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../local/assets/models-extra/pointclouds/pdal/laz/simple.laz");
        let Ok(mut b) = std::fs::read(&p) else { return };
        let at = header(&b).unwrap().point_offset as usize;
        b[at..at + 8].fill(0);
        b[at + 12..at + 16].copy_from_slice(&u32::MAX.to_le_bytes());
        let end = b.len();
        b[end - 8..].copy_from_slice(&(at as i64 + 8).to_le_bytes());
        let result = load(&p, &b, &Limits::thumbnail());
        assert!(matches!(&result, Err(e) if e.to_string().contains("chunk table")), "{:?}", result.err());
    }

    #[test]
    fn survives_truncation_and_garbage() {
        let bytes = sample(50);
        for cut in 0..bytes.len() {
            let _ = load(Path::new("a.las"), &bytes[..cut], &Limits::viewer());
        }
        for i in 0..240 {
            let mut b = bytes.clone();
            b[i] = 0xFF;
            let _ = load(Path::new("a.las"), &b, &Limits::viewer());
        }
        // A LAZ flag without a LASzip record.
        let mut b = bytes.clone();
        b[104] |= 0x80;
        assert!(load(Path::new("a.laz"), &b, &Limits::viewer()).is_err());
    }
}
