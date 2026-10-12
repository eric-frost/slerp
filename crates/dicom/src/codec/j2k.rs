//! JPEG 2000 through `hayro-jpeg2000`, a memory-safe pure-Rust decoder.
//! It decodes at a reduced resolution for thumbnails (skipping the finest
//! wavelet levels, most of the work), keeps every bit of precision, and
//! applies the codestream's colour transform (DICOM's YBR_RCT and
//! YBR_ICT).  It doesn't apply signedness, which is read here from the
//! codestream's SIZ marker.

use super::{Decoded, Samples};
use crate::image::MAX_FRAME_SAMPLES as MAX_SAMPLES;

/// The marker segments of the main header (after SOC, up to the first
/// tile's SOT): (marker, segment bytes after the length).
fn main_header(data: &[u8]) -> Vec<(u8, &[u8])> {
    let mut out = Vec::new();
    let Some(soc) = data.windows(4).take(4096).position(|w| w == [0xFF, 0x4F, 0xFF, 0x51]) else { return out };
    let mut pos = soc + 2;
    while pos + 4 <= data.len() && data[pos] == 0xFF && data[pos + 1] != 0x90 && out.len() < 256 {
        let len = u16::from_be_bytes([data[pos + 2], data[pos + 3]]) as usize;
        let Some(seg) = data.get(pos + 4..pos + 2 + len.max(2)) else { break };
        out.push((data[pos + 1], seg));
        pos += 2 + len.max(2);
    }
    out
}

/// Components' (precision, signed) from the SIZ marker.
fn siz_components(header: &[(u8, &[u8])]) -> Option<Vec<(u8, bool)>> {
    let siz = header.iter().find(|m| m.0 == 0x51)?.1;
    // Rsiz, then eight 32-bit sizes, then Csiz.
    let n = u16::from_be_bytes([*siz.get(34)?, *siz.get(35)?]) as usize;
    (0..n.min(16)).map(|i| siz.get(36 + 3 * i).map(|&s| ((s & 0x7F) + 1, s & 0x80 != 0))).collect()
}

/// Whether the COD marker turns on the multiple component transform,
/// after which the decoder gives RGB whatever the photometric
/// interpretation says (YBR_RCT, YBR_ICT).
fn uses_mct(header: &[(u8, &[u8])]) -> bool {
    // Scod, progression order, two bytes of layers, then the transform.
    header.iter().find(|m| m.0 == 0x52).and_then(|m| m.1.get(4)).is_some_and(|&t| t & 1 == 1)
}

/// Decodes a JPEG 2000 codestream (or JP2 file, which some writers put in
/// DICOM against the standard).
pub fn decode(data: &[u8], max_size: Option<u32>) -> Result<Decoded, String> {
    use hayro_jpeg2000::{DecodeSettings, DecoderContext, Image};
    // Some writers put bytes before the codestream; start at SOC.
    let start = data.windows(4).take(256).position(|w| w == [0xFF, 0x4F, 0xFF, 0x51] || w == [0, 0, 0, 0x0C]);
    let data = &data[start.unwrap_or(0)..];
    let header = main_header(data);
    let comps = siz_components(&header).ok_or("JPEG 2000 without a SIZ marker")?;
    let mct = uses_mct(&header);
    // A CAP marker in the main header announces Part 15 (high-throughput)
    // block coding, which the decoder doesn't read.
    if header.iter().any(|m| m.0 == 0x50) {
        return Err("high-throughput JPEG 2000 (HTJ2K) isn't supported".into());
    }
    let settings = DecodeSettings {
        resolve_palette_indices: true,
        strict: false,
        target_resolution: max_size.map(|s| (s.max(1), s.max(1))),
    };
    let image = Image::new(data, &settings).map_err(|e| format!("JPEG 2000: {e}"))?;
    let (w, h) = (image.width(), image.height());
    if w == 0 || h == 0 || (w as u64) * (h as u64) * (comps.len().max(1) as u64) > MAX_SAMPLES {
        return Err("JPEG 2000 image too large".into());
    }
    let mut ctx = DecoderContext::default();
    let decoded = image.decode(&mut ctx).map_err(|e| format!("JPEG 2000: {e}"))?;
    let planes = decoded.components();
    let n = planes.len().min(3);
    if n == 0 || n == 2 {
        return Err(format!("JPEG 2000 with {} components", planes.len()));
    }
    let pixels = planes[0].samples().len();
    if pixels != (w as usize) * (h as usize) || planes.iter().take(n).any(|p| p.samples().len() != pixels) {
        return Err("JPEG 2000 components of different sizes".into());
    }
    // The decoder adds 2^(precision-1) to every component, as it should
    // for unsigned ones; signed ones get it taken off again.  Values are
    // kept as the two's complement bit pattern of their precision, which
    // the pixel module sign-extends.
    let precision = planes[0].bit_depth() as u32;
    let mut out = vec![0u32; pixels * n];
    for (c, plane) in planes.iter().take(n).enumerate() {
        let (bits, signed) = comps.get(c).copied().unwrap_or((plane.bit_depth(), false));
        let offset = if signed { (1u32 << (bits.clamp(1, 31) - 1)) as f32 } else { 0.0 };
        let (lo, hi) =
            if signed { (-(1i64 << (bits - 1)), (1i64 << (bits - 1)) - 1) } else { (0, (1i64 << bits.min(32)) - 1) };
        for (i, &v) in plane.samples().iter().enumerate() {
            let v = ((v - offset).round() as i64).clamp(lo, hi);
            out[i * n + c] = v as u32;
        }
    }
    Ok(Decoded { width: w, height: h, comps: n as u32, precision, data: Samples::U32(out), rgb: n == 3 && mct })
}
