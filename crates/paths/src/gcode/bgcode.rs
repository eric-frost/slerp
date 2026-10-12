//! Prusa's binary G-code (`.bgcode`), read from its published
//! specification (libbgcode's `doc/specifications.md`; libbgcode itself is
//! AGPL, so none of its code is used).
//!
//! A 10-byte header (`GCDE`, version, checksum type) is followed by
//! blocks, each a header (type, compression, uncompressed size and, when
//! compressed, the compressed size), parameters (an encoding, or a
//! thumbnail's format and size), data, and an optional CRC32 that isn't
//! checked: a damaged block shows up as damaged G-code, which the
//! interpreter shrugs off.  Thumbnails come before the G-code, so a
//! thumbnail only reads the start of the file.

use super::heatshrink;
use super::meatpack::MeatPack;
use super::thumbs::Embedded;
use slerp_scene::LoadError;
use std::io::{BufReader, Read};
use std::path::Path;

/// Binary G-code's magic number.
pub const MAGIC: &[u8; 4] = b"GCDE";
/// The largest block accepted, compressed or not; PrusaSlicer writes
/// blocks of 64 KiB.
const MAX_BLOCK: u32 = 64 << 20;

const FILE_METADATA: u16 = 0;
const GCODE: u16 = 1;
const SLICER_METADATA: u16 = 2;
const PRINTER_METADATA: u16 = 3;
const PRINT_METADATA: u16 = 4;
const THUMBNAIL: u16 = 5;

/// Receives a G-code block's text; returns false to stop reading.
pub type TextSink<'a> = &'a mut dyn FnMut(&[u8]) -> bool;

/// What reading a binary G-code file gives besides its G-code text.
#[derive(Default)]
pub struct BgcodeSink {
    pub thumbnails: Vec<Embedded>,
    /// `key=value` lines from the metadata blocks.
    pub metadata: Vec<Vec<u8>>,
    /// The compressions and encodings the G-code blocks used.
    pub compression: Vec<&'static str>,
    pub warnings: Vec<String>,
}

/// Reads the binary G-code at `path`.  Thumbnails and metadata are
/// collected; G-code blocks are decompressed, decoded and handed to `gcode`
/// as text, one block at a time, until it returns false.  Without `gcode`,
/// reading stops at the first G-code block, after the thumbnails.
pub fn read_bgcode(path: &Path, mut gcode: Option<TextSink>) -> Result<BgcodeSink, LoadError> {
    let file = std::fs::File::open(path)?;
    let len = file.metadata()?.len();
    let mut r = BufReader::with_capacity(256 << 10, file);
    let mut head = [0u8; 10];
    r.read_exact(&mut head).map_err(|_| LoadError::invalid("binary G-code header cut short"))?;
    if &head[0..4] != MAGIC {
        return Err(LoadError::NotMine);
    }
    let version = u32::from_le_bytes([head[4], head[5], head[6], head[7]]);
    let checksum = u16::from_le_bytes([head[8], head[9]]);
    let checksum_bytes = match checksum {
        0 => 0,
        1 => 4,
        n => return Err(LoadError::Unsupported(format!("binary G-code checksum type {n}"))),
    };
    let mut sink = BgcodeSink::default();
    if version != 1 {
        sink.warnings.push(format!("binary G-code version {version}; read as version 1"));
    }
    let mut pos = 10u64;
    let mut meatpack = MeatPack::default();
    let mut text = Vec::new();
    loop {
        // 1. The block header and parameters.
        let mut h = [0u8; 12];
        match read_some(&mut r, &mut h[..8]) {
            0 => break,
            8 => {}
            _ => {
                sink.warnings.push("the file ends inside a block header".into());
                break;
            }
        }
        let kind = u16::from_le_bytes([h[0], h[1]]);
        let compression = u16::from_le_bytes([h[2], h[3]]);
        let size = u32::from_le_bytes([h[4], h[5], h[6], h[7]]);
        let mut stored = size;
        let mut header_len = 8;
        if compression != 0 {
            if read_some(&mut r, &mut h[8..12]) != 4 {
                sink.warnings.push("the file ends inside a block header".into());
                break;
            }
            stored = u32::from_le_bytes([h[8], h[9], h[10], h[11]]);
            header_len = 12;
        }
        let params_len = match kind {
            THUMBNAIL => 6,
            FILE_METADATA | GCODE | SLICER_METADATA | PRINTER_METADATA | PRINT_METADATA => 2,
            _ => {
                sink.warnings.push(format!("unknown block type {kind}; the rest of the file is skipped"));
                break;
            }
        };
        let mut params = [0u8; 6];
        if read_some(&mut r, &mut params[..params_len]) != params_len {
            sink.warnings.push("the file ends inside a block".into());
            break;
        }
        pos += header_len + params_len as u64;
        if size > MAX_BLOCK || stored > MAX_BLOCK || pos + stored as u64 > len {
            sink.warnings.push(format!("a block of {stored} bytes runs past the end of the file"));
            break;
        }
        if kind == GCODE && gcode.is_none() {
            break;
        }

        // 2. The data, decompressed.
        let mut data = vec![0u8; stored as usize];
        if read_some(&mut r, &mut data) != data.len() {
            sink.warnings.push("the file ends inside a block".into());
            break;
        }
        pos += stored as u64 + checksum_bytes;
        if checksum_bytes > 0 && r.seek_relative(checksum_bytes as i64).is_err() {
            break;
        }
        let data = match compression {
            0 => data,
            1 => match inflate(&data, size as usize) {
                Some(d) => d,
                None => {
                    sink.warnings.push("a deflate block doesn't decompress".into());
                    continue;
                }
            },
            2 => heatshrink::decode(&data, 11, 4, size as usize),
            3 => heatshrink::decode(&data, 12, 4, size as usize),
            n => {
                sink.warnings.push(format!("unknown compression {n}; block skipped"));
                continue;
            }
        };
        if kind == GCODE {
            let name = match compression {
                1 => "deflate",
                2 | 3 => "heatshrink",
                _ => "uncompressed",
            };
            if !sink.compression.contains(&name) {
                sink.compression.push(name);
            }
        }

        // 3. What the block holds.
        let param = u16::from_le_bytes([params[0], params[1]]);
        match kind {
            THUMBNAIL => {
                let width = u16::from_le_bytes([params[2], params[3]]) as u32;
                let height = u16::from_le_bytes([params[4], params[5]]) as u32;
                sink.thumbnails.push(Embedded { width, height, data, rgb565: false });
            }
            GCODE => {
                let decoded = match param {
                    0 => data,
                    1 | 2 => {
                        if !sink.compression.contains(&"MeatPack") {
                            sink.compression.push("MeatPack");
                        }
                        text.clear();
                        meatpack.decode(&data, &mut text);
                        std::mem::take(&mut text)
                    }
                    n => {
                        sink.warnings.push(format!("unknown G-code encoding {n}; block skipped"));
                        continue;
                    }
                };
                if let Some(f) = gcode.as_mut()
                    && !f(&decoded)
                {
                    break;
                }
                text = decoded;
            }
            _ => sink.metadata.extend(data.split(|&b| b == b'\n').filter(|l| !l.is_empty()).map(<[u8]>::to_vec)),
        }
    }
    Ok(sink)
}

/// Reads as much of `buf` as the stream has, returning how much that was.
fn read_some(r: &mut impl Read, buf: &mut [u8]) -> usize {
    let mut n = 0;
    while n < buf.len() {
        match r.read(&mut buf[n..]) {
            Ok(0) => break,
            Ok(k) => n += k,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => break,
        }
    }
    n
}

/// Inflates zlib-wrapped or raw deflate data, at most `limit` bytes.
fn inflate(data: &[u8], limit: usize) -> Option<Vec<u8>> {
    use miniz_oxide::inflate::{decompress_to_vec_with_limit, decompress_to_vec_zlib_with_limit};
    let zlib = data.len() >= 2 && data[0] & 0x0f == 8 && u16::from_be_bytes([data[0], data[1]]).is_multiple_of(31);
    let limit = limit.max(1);
    let result =
        if zlib { decompress_to_vec_zlib_with_limit(data, limit) } else { decompress_to_vec_with_limit(data, limit) };
    match result {
        Ok(v) => Some(v),
        // A block that inflates past its declared size keeps its start.
        Err(e) if !e.output.is_empty() => Some(e.output),
        Err(_) => None,
    }
}
