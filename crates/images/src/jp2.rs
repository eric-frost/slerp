//! JPEG 2000 (`.jp2`, `.jpf`, and bare `.j2k` codestreams), decoded by the
//! pure-Rust `hayro-jpeg2000` that DICOM and TIFF already use.  The decoder
//! gives one plane of floating point samples per component, in the range of
//! the file's bit depth; this turns them into 8-bit RGBA (colour and
//! anything up to 8 bits) or 16-bit data (grey deeper than 8 bits, like 16-bit
//! PNG), and applies an embedded ICC profile.

use crate::color::Conversion;
use hayro_jpeg2000::{ColorSpace, DecodeSettings, DecoderContext, Image};

/// Pictures with more pixels than this are refused: the decoder holds
/// several planes of floats.
const MAX_PIXELS: u64 = 64 << 20;

pub enum Pixels {
    Rgba8(Vec<u8>),
    /// Grey deeper than 8 bits as floats from 0 to 1, with opacity.
    Grey16(Vec<f32>),
}

pub struct Jp2 {
    pub width: u32,
    pub height: u32,
    pub bit_depth: u8,
    pub components: usize,
    pub has_alpha: bool,
    pub space: String,
    pub pixels: Pixels,
    /// What was done with the embedded profile, for the inspector.
    pub profile_note: Option<String>,
}

pub fn is_jp2(head: &[u8]) -> bool {
    head.starts_with(b"\x00\x00\x00\x0cjP  ") || head.starts_with(&[0xff, 0x4f, 0xff, 0x51])
}

pub fn decode(data: &[u8]) -> Result<Jp2, String> {
    let settings = DecodeSettings { resolve_palette_indices: true, strict: false, target_resolution: None };
    let image = Image::new(data, &settings).map_err(|e| format!("JPEG 2000: {e}"))?;
    let (w, h) = (image.width(), image.height());
    if w == 0 || h == 0 {
        return Err("the picture is empty".into());
    }
    if w as u64 * h as u64 > MAX_PIXELS {
        return Err(format!("{w} × {h} pixels is more than is opened ({} megapixels)", MAX_PIXELS >> 20));
    }
    let space = image.color_space().clone();
    let has_alpha = image.has_alpha();
    let mut ctx = DecoderContext::default();
    let decoded = image.decode(&mut ctx).map_err(|e| format!("JPEG 2000: {e}"))?;
    let planes = decoded.components();
    let n = w as usize * h as usize;
    if planes.is_empty() || planes.iter().any(|p| p.samples().len() != n) {
        return Err("components of different sizes aren't supported".into());
    }
    let bits = planes[0].bit_depth().clamp(1, 16);
    let max = ((1u32 << bits) - 1) as f32;
    let colour = space.num_channels() as usize;
    if colour == 0 || planes.len() < colour {
        return Err("fewer components than the colour space needs".into());
    }
    let alpha = (has_alpha && planes.len() > colour).then(|| planes[colour].samples());
    let (name, _) = match &space {
        ColorSpace::Gray => ("grey".to_string(), 1),
        ColorSpace::RGB => ("RGB".to_string(), 3),
        ColorSpace::CMYK => ("CMYK".to_string(), 4),
        ColorSpace::Icc { num_channels, .. } => (format!("ICC profile, {num_channels} channels"), *num_channels),
        ColorSpace::Unknown { num_channels } => (format!("{num_channels} channels"), *num_channels),
    };
    let to8 = |v: f32| -> u8 { (v / max * 255.0 + 0.5).clamp(0.0, 255.0) as u8 };
    let a8 = |i: usize| -> u8 { alpha.map_or(255, |a| to8(a[i])) };
    let mut profile_note = None;
    let pixels = match colour {
        1 if bits > 8 => {
            let g = planes[0].samples();
            let mut out = Vec::with_capacity(n * 4);
            for i in 0..n {
                let v = (g[i] / max).clamp(0.0, 1.0);
                out.extend([v, v, v, alpha.map_or(1.0, |a| (a[i] / max).clamp(0.0, 1.0))]);
            }
            Pixels::Grey16(out)
        }
        1 => {
            let g = planes[0].samples();
            Pixels::Rgba8((0..n).flat_map(|i| [to8(g[i]), to8(g[i]), to8(g[i]), a8(i)]).collect())
        }
        3 => {
            let (r, g, b) = (planes[0].samples(), planes[1].samples(), planes[2].samples());
            let mut out: Vec<u8> = (0..n).flat_map(|i| [to8(r[i]), to8(g[i]), to8(b[i]), a8(i)]).collect();
            if let ColorSpace::Icc { profile, .. } = &space {
                match Conversion::from_icc(profile) {
                    Some(c) if c.is_srgb() => profile_note = Some(format!("{}, already sRGB", c.name)),
                    Some(c) => {
                        c.convert_rgba8(&mut out);
                        profile_note = Some(format!("{}, converted to sRGB", c.name));
                    }
                    None => {
                        profile_note =
                            Some(format!("{}, not applied (shown as sRGB)", crate::color::describe_icc(profile)))
                    }
                }
            }
            Pixels::Rgba8(out)
        }
        4 => {
            let (c, m, y, k) = (planes[0].samples(), planes[1].samples(), planes[2].samples(), planes[3].samples());
            let ink = |v: f32| (max - v).clamp(0.0, max) / max;
            Pixels::Rgba8(
                (0..n)
                    .flat_map(|i| {
                        // Inks: 0 is no ink in JPEG 2000's CMYK.
                        let kk = 1.0 - k[i].clamp(0.0, max) / max;
                        let ch = |v: f32| ((ink(v) * kk) * 255.0 + 0.5) as u8;
                        [ch(c[i]), ch(m[i]), ch(y[i]), 255]
                    })
                    .collect(),
            )
        }
        other => return Err(format!("{other} colour channels aren't supported")),
    };
    Ok(Jp2 {
        width: w,
        height: h,
        bit_depth: bits,
        components: planes.len(),
        has_alpha: alpha.is_some(),
        space: name,
        pixels,
        profile_note,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signatures() {
        assert!(is_jp2(b"\x00\x00\x00\x0cjP  \r\n\x87\n"));
        assert!(is_jp2(&[0xff, 0x4f, 0xff, 0x51, 0, 0]));
        assert!(!is_jp2(b"\x89PNG"));
        assert!(!is_jp2(b""));
    }

    #[test]
    fn garbage_is_an_error() {
        assert!(decode(b"").is_err());
        assert!(decode(b"\x00\x00\x00\x0cjP  \r\n\x87\n\0\0").is_err());
        let mut cs = vec![0xff, 0x4f, 0xff, 0x51];
        cs.extend([0xff; 100]);
        assert!(decode(&cs).is_err());
    }
}
