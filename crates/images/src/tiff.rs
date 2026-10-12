//! TIFF files that are pictures: scans, photographs and graphics in colour,
//! palette, CMYK, YCbCr or 8-bit grey, with an alpha channel or without,
//! at 8 or 16 bits or in floating point.  The file format itself (strips,
//! tiles, LZW, Deflate, PackBits, Zstandard, JPEG, JPEG 2000, BigTIFF) is
//! `slerp-tiff`'s.
//!
//! Stacks of microscope slices, OME and ImageJ files, whole-slide images and
//! grey data deeper than 8 bits (measurements, height maps, 16-bit
//! scientific cameras) stay with `slerp-volumes`, whose window and level
//! controls suit them; [`probe`] tells the two apart from the directories
//! alone.

use crate::color::describe_icc;
use crate::exif::Meta;
use slerp_scene::textures::{ColorSpace, Texture};
use slerp_tiff::raster::{Options, Raster, Sample, compression_name, photometric_name};
use slerp_tiff::{Page, Tiff, tag};
use std::path::Path;

/// The tags the picture needs beyond what `slerp-tiff` reads.
const ORIENTATION: u16 = 274;
const ICC_PROFILE: u16 = 34675;

/// Camera raw formats built on TIFF, which Slerp doesn't develop (their first
/// page is a small preview, not the picture).
const RAW_EXTENSIONS: [&str; 18] = [
    "dng", "cr2", "nef", "nrw", "arw", "srf", "sr2", "orf", "rw2", "pef", "srw", "3fr", "iiq", "erf", "mef", "mos",
    "mrw", "kdc",
];

pub fn is_tiff(head: &[u8]) -> bool {
    matches!(head.get(..4), Some(b"II*\0" | b"MM\0*" | b"II+\0" | b"MM\0+"))
}

/// Whether a page is the kind of picture this module shows.
fn ordinary(p: &Page) -> bool {
    let looks_scientific = p.description.starts_with("ImageJ=")
        || (p.description.trim_start().starts_with('<') && p.description.contains("<OME"))
        || p.ifd.get(tag::NDPI_FORMAT_FLAG).is_some();
    if looks_scientific {
        return false;
    }
    let bits = p.bits.first().copied().unwrap_or(8);
    let integer = matches!(p.sample_format, 1 | 4);
    match p.photometric {
        // Grey and bilevel: pictures up to 8 bits; deeper is data.
        0 | 1 => bits <= 8 && integer,
        // RGB at any depth, floating point included.
        2 => p.samples >= 3,
        3 => bits <= 16,
        5 => bits == 8 && p.samples >= 4,
        6 => true,
        _ => false,
    }
}

/// Whether the file at `path` is a TIFF picture: one page (the others, if
/// any, are reduced-size copies of it) of an ordinary kind.
pub fn probe(path: &Path) -> bool {
    let ext = path.extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
    if RAW_EXTENSIONS.contains(&ext.as_str()) {
        return false;
    }
    let Ok(tiff) = Tiff::open(path) else { return false };
    let Ok(offsets) = tiff.ifd_offsets(17) else { return false };
    if offsets.len() > 16 {
        return false;
    }
    let Ok(first) = tiff.page(offsets[0]) else { return false };
    let only_copies = offsets[1..].iter().all(|&o| tiff.page(o).is_ok_and(|p| is_reduced_copy(&first, &p)));
    only_copies && ordinary(&first)
}

/// Whether `p` is a smaller copy of the picture `first` (a camera's thumbnail,
/// an overview): marked as reduced resolution, or at most a quarter of the
/// pixels at the same shape.  Cameras often forget the mark.
fn is_reduced_copy(first: &Page, p: &Page) -> bool {
    if p.subfile_type & 1 == 1 {
        return true;
    }
    let shape = |a: &Page| a.width as f64 / a.height as f64;
    p.pixels().saturating_mul(4) <= first.pixels() && (shape(p) / shape(first) - 1.0).abs() < 0.05
}

/// What opening the file gave.
pub struct Picture {
    pub texture: Texture,
}

/// Reads the first page as a picture.
pub fn load(path: &Path, min_long_side: usize) -> Result<Picture, String> {
    let tiff = Tiff::open(path)?;
    let offsets = tiff.ifd_offsets(16)?;
    let page = tiff.page(offsets[0])?;
    let opts = Options { min_long_side, max_pixels: 1 << 27 };
    let raster = page.decode(&tiff, &opts)?;
    let mut header = facts(&page, &raster);
    let mut meta = Meta { orientation: 1, ..Meta::default() };
    if let Some(e) = page.ifd.get(ORIENTATION)
        && let Ok(v) = tiff.ints(e, 1)
        && let Some(&o) = v.first()
        && (1..=8).contains(&o)
    {
        meta.orientation = o as u8;
    }
    if let Some(e) = page.ifd.get(ICC_PROFILE)
        && let Ok(b) = tiff.bytes(e)
    {
        meta.icc = Some(b);
    }
    let mut texture = to_texture(&page, &raster)?;
    if let Some(w) = &raster.warning {
        header.push(("Warning".into(), w.clone()));
    }
    let container = if tiff.big { "BigTIFF" } else { "TIFF" };
    texture.source.container = container.into();
    texture.source.header = header;
    crate::photo::apply(&mut texture, &meta);
    // `apply` leaves float pictures alone for colour; say what the profile is.
    if let (Some(icc), true) = (&meta.icc, texture.is_hdr()) {
        texture
            .source
            .header
            .push(("Colour profile".into(), format!("{}, not applied to floating point", describe_icc(icc))));
    }
    Ok(Picture { texture })
}

fn facts(p: &Page, r: &Raster) -> Vec<(String, String)> {
    let mut f = vec![
        ("Compression".to_string(), compression_name(p.compression)),
        ("Photometric".into(), photometric_name(p.photometric).to_string()),
        ("Samples per pixel".into(), p.samples.to_string()),
        ("Bits per sample".into(), p.bits.first().copied().unwrap_or(8).to_string()),
    ];
    if p.sample_format == 3 {
        f.push(("Sample type".into(), "floating point".into()));
    }
    if p.is_tiled() {
        f.push(("Layout".into(), format!("tiles of {} × {}", p.tile.map_or(0, |t| t.0), p.tile.map_or(0, |t| t.1))));
    } else {
        f.push(("Layout".into(), format!("strips of {} rows", p.rows_per_strip)));
    }
    if p.planar == 2 {
        f.push(("Planar configuration".into(), "separate planes".into()));
    }
    if let Some((x, y, unit)) = p.resolution {
        let unit = match unit {
            2 => "per inch",
            3 => "per centimetre",
            _ => "",
        };
        f.push(("Resolution".into(), format!("{x:.0} × {y:.0} {unit}").trim().to_string()));
    }
    if !p.software.is_empty() {
        f.push(("Software".into(), p.software.clone()));
    }
    let desc = p.description.trim();
    if !desc.is_empty() {
        f.push(("Description".into(), desc.chars().take(300).collect()));
    }
    if r.reduction > 1 {
        f.push(("Decoded at".into(), format!("1/{} size", r.reduction)));
    }
    f
}

/// A page's pixels as a texture.
fn to_texture(p: &Page, r: &Raster) -> Result<Texture, String> {
    let (w, h) = (r.width, r.height);
    let samples = r.samples;
    let n = w * h;
    let bits = p.bits.first().copied().unwrap_or(8);
    if r.bytes.len() != n * samples * r.sample.size() {
        return Err("the decoded page has an unexpected size".into());
    }
    // A sample as a number from 0 to 1 (integers) or as it is (floats).
    let value = |i: usize| -> f32 { sample_value(r.sample, &r.bytes, i) };
    let format_name = |kind: &str| format!("{kind} {bits}-bit");
    // Alpha: ExtraSamples says whether it is premultiplied (1), straight
    // (2) or unspecified (0, shown opaque).
    let alpha = match p.extra_samples.first() {
        Some(1) => Some(true),
        Some(2) => Some(false),
        _ => None,
    };
    if samples == 1 {
        // Grey.  Integers up to 8 bits are a picture; anything deeper is data.
        if r.sample == Sample::U8 {
            // A grey page with alpha keeps it (ExtraSamples says whether it is premultiplied).
            let ga = r.alpha.as_deref().filter(|a| a.len() == n && alpha.is_some());
            let px: Vec<u8> = r
                .bytes
                .iter()
                .enumerate()
                .flat_map(|(i, &v)| {
                    let g = if r.invert { 255 - v } else { v };
                    let a = ga.map_or(255, |a| a[i]);
                    // Associated alpha has the grey multiplied in.
                    let g = if a > 0 && a < 255 && ga.is_some() && alpha == Some(true) {
                        (g as u32 * 255 / a as u32).min(255) as u8
                    } else {
                        g
                    };
                    [g, g, g, a]
                })
                .collect();
            let mut t = Texture::from_rgba8(w as u32, h as u32, px, ColorSpace::Srgb);
            t.source.format = format_name("Grayscale");
            return Ok(t);
        }
        let mut px = Vec::with_capacity(n * 4);
        for i in 0..n {
            let mut v = value(i);
            if r.invert {
                v = 1.0 - v;
            }
            px.extend([v, v, v, 1.0]);
        }
        let mut t = Texture::from_rgba32f(w as u32, h as u32, px);
        t.source.format = format!("Grayscale {bits}-bit (data)");
        return Ok(t);
    }
    if samples < 3 {
        return Err(format!("{samples} samples per pixel aren't a picture"));
    }
    let has_alpha = samples >= 4 && alpha.is_some();
    if r.sample.is_float() {
        let mut px = Vec::with_capacity(n * 4);
        for i in 0..n {
            let c = [value(i * samples), value(i * samples + 1), value(i * samples + 2)];
            let a = if has_alpha { value(i * samples + 3).clamp(0.0, 1.0) } else { 1.0 };
            let k = if alpha == Some(true) && a > 0.0 { 1.0 / a } else { 1.0 };
            px.extend([c[0] * k, c[1] * k, c[2] * k, a]);
        }
        let mut t = Texture::from_rgba32f(w as u32, h as u32, px);
        t.source.format = format_name("RGB float");
        return Ok(t);
    }
    // Integers of any size become 8-bit sRGB.
    let to8 = |v: f32| -> u8 { (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8 };
    let mut px = Vec::with_capacity(n * 4);
    for i in 0..n {
        let c = [value(i * samples), value(i * samples + 1), value(i * samples + 2)];
        let a = if has_alpha { value(i * samples + 3).clamp(0.0, 1.0) } else { 1.0 };
        let k = if alpha == Some(true) && a > 0.0 { 1.0 / a } else { 1.0 };
        px.extend([to8(c[0] * k), to8(c[1] * k), to8(c[2] * k), to8(a)]);
    }
    let kind = match p.photometric {
        3 => "Palette",
        5 => "CMYK",
        6 => "YCbCr",
        _ => "RGB",
    };
    let mut t = Texture::from_rgba8(w as u32, h as u32, px, ColorSpace::Srgb);
    t.source.format = if p.photometric == 3 {
        "Palette".into()
    } else {
        format!("{kind}{} {bits}-bit", if has_alpha { "A" } else { "" })
    };
    Ok(t)
}

/// Sample `i` of little-endian data: an integer scaled to 0 to 1, or a float
/// as it is.
fn sample_value(s: Sample, b: &[u8], i: usize) -> f32 {
    let at = i * s.size();
    match s {
        Sample::U8 => b[at] as f32 / 255.0,
        Sample::I8 => (b[at] as i8 as f32 + 128.0) / 255.0,
        Sample::U16 => u16::from_le_bytes([b[at], b[at + 1]]) as f32 / 65535.0,
        Sample::I16 => (i16::from_le_bytes([b[at], b[at + 1]]) as f32 + 32768.0) / 65535.0,
        Sample::U32 => u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]]) as f32 / u32::MAX as f32,
        Sample::I32 => {
            (i32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]]) as f32 + 2147483648.0) / u32::MAX as f32
        }
        Sample::U64 => u64::from_le_bytes(b[at..at + 8].try_into().unwrap_or([0; 8])) as f32 / u64::MAX as f32,
        Sample::I64 => {
            (i64::from_le_bytes(b[at..at + 8].try_into().unwrap_or([0; 8])) as f64 / u64::MAX as f64 + 0.5) as f32
        }
        Sample::F16 => half(u16::from_le_bytes([b[at], b[at + 1]])),
        Sample::F32 => f32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]]),
        Sample::F64 => f64::from_le_bytes(b[at..at + 8].try_into().unwrap_or([0; 8])) as f32,
    }
}

fn half(h: u16) -> f32 {
    let sign = ((h as u32) & 0x8000) << 16;
    let exp = ((h >> 10) & 0x1f) as u32;
    let man = (h & 0x3ff) as u32;
    let bits = match exp {
        0 if man == 0 => sign,
        0 => {
            let shift = man.leading_zeros() - 21;
            sign | ((113 - shift) << 23) | (((man << shift) & 0x3ff) << 13)
        }
        31 => sign | 0x7f80_0000 | (man << 13),
        _ => sign | ((exp + 112) << 23) | (man << 13),
    };
    f32::from_bits(bits)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn halves_and_integers() {
        assert_eq!(half(0x3c00), 1.0);
        assert_eq!(half(0xc000), -2.0);
        assert_eq!(sample_value(Sample::U8, &[255], 0), 1.0);
        assert_eq!(sample_value(Sample::U16, &[0xff, 0xff], 0), 1.0);
        assert_eq!(sample_value(Sample::F32, &1.5f32.to_le_bytes(), 0), 1.5);
        assert!((sample_value(Sample::I16, &i16::MIN.to_le_bytes(), 0)).abs() < 1e-6);
    }

    #[test]
    fn the_signature() {
        assert!(is_tiff(b"II*\0\x08\0\0\0"));
        assert!(is_tiff(b"MM\0+\0\x08\0\0"));
        assert!(!is_tiff(b"GIF89a"));
        assert!(!is_tiff(b"II"));
    }

    #[test]
    fn a_file_that_is_not_a_tiff_is_not_a_picture() {
        let dir = std::env::temp_dir().join(format!("slerp-images-tiff-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("x.tif");
        std::fs::write(&p, b"II*\0\x08\0\0\0\0\0\0\0\0\0").unwrap();
        assert!(!probe(&p));
        std::fs::write(&p, b"not a tiff").unwrap();
        assert!(!probe(&p));
        assert!(!probe(&dir.join("missing.tif")));
        std::fs::remove_dir_all(&dir).ok();
    }
}
