//! Netpbm's arbitrary map (PAM, `P7`) and the farbfeld format.
//!
//! PAM is Netpbm's own successor to PBM, PGM and PPM: a short text header
//! (`WIDTH`, `HEIGHT`, `DEPTH`, `MAXVAL`, `TUPLTYPE`, `ENDHDR`) and then rows
//! of samples, one byte each or, above a maximum value of 255, two bytes
//! big-endian.  Its depth makes it carry an alpha channel, which PPM can't.
//! Farbfeld is suckless.org's deliberately trivial format: a magic number,
//! the size, and 16-bit big-endian RGBA, in sRGB and not premultiplied.
//!
//! Both decode to 8-bit RGBA, except 16-bit greyscale, which is far more
//! often data (a depth map) than a photograph, and so decodes as float
//! data stretched from its lowest to its highest value, like 16-bit PNGs.

use crate::formats::channel_tag;
use crate::thumb::Channels;
use crate::util::{check_size, invalid};
use crate::{ColorSpace, Texture, TextureError};

const FARBFELD_MAGIC: &[u8] = b"farbfeld";

/// `P7` and a line break: what separates PAM from PNM's other flavours.
pub(crate) fn sniff_pam(head: &[u8]) -> bool {
    head.starts_with(b"P7\n") || head.starts_with(b"P7\r\n")
}

pub(crate) fn sniff_farbfeld(head: &[u8]) -> bool {
    head.starts_with(FARBFELD_MAGIC)
}

/*┌─────────────────╮
| PAM             |
└─────────────────╯*/
//region PAM

/// The fields of a PAM header.
#[derive(Debug, Default, PartialEq)]
struct Header {
    width: u32,
    height: u32,
    depth: u32,
    maxval: u32,
    tuple_type: String,
    /// Where the samples start.
    data: usize,
}

fn parse_header(bytes: &[u8]) -> Result<Header, TextureError> {
    let mut h = Header::default();
    let mut at = 0usize;
    let mut first = true;
    // The header is at most a few hundred bytes; comments make it longer.
    while at < bytes.len().min(1 << 16) {
        let end = bytes[at..].iter().position(|&b| b == b'\n').ok_or_else(|| invalid("PAM: no ENDHDR"))?;
        let line = String::from_utf8_lossy(&bytes[at..at + end]).trim().to_string();
        at += end + 1;
        if first {
            first = false;
            if line != "P7" {
                return Err(invalid("PAM: no P7"));
            }
            continue;
        }
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (key, value) = line.split_once(char::is_whitespace).map_or((line.as_str(), ""), |(k, v)| (k, v.trim()));
        let number = || value.parse::<u32>().map_err(|_| invalid(format!("PAM: bad {key} {value:?}")));
        match key {
            "WIDTH" => h.width = number()?,
            "HEIGHT" => h.height = number()?,
            "DEPTH" => h.depth = number()?,
            "MAXVAL" => h.maxval = number()?,
            // Several lines add up, as the Netpbm documentation says.
            "TUPLTYPE" => {
                if !h.tuple_type.is_empty() {
                    h.tuple_type.push(' ');
                }
                h.tuple_type.push_str(value);
            }
            "ENDHDR" => {
                h.data = at;
                return Ok(h);
            }
            _ => {}
        }
    }
    Err(invalid("PAM: no ENDHDR"))
}

/// Decodes a PAM file.
pub(crate) fn decode_pam(bytes: &[u8]) -> Result<Texture, TextureError> {
    let h = parse_header(bytes)?;
    if h.width == 0 || h.height == 0 || h.depth == 0 || h.maxval == 0 || h.maxval > 65535 || h.depth > 16 {
        return Err(invalid(format!("PAM: {}x{} depth {} maxval {}", h.width, h.height, h.depth, h.maxval)));
    }
    let n = check_size(h.width, h.height, 1, 16)?;
    let wide = h.maxval > 255;
    let sample_bytes = if wide { 2 } else { 1 };
    let row_bytes = (h.width as u64 * h.depth as u64 * sample_bytes as u64) as usize;
    let need = row_bytes.checked_mul(h.height as usize).ok_or_else(|| invalid("PAM: huge image"))?;
    let data = &bytes[h.data..];
    // A cut-off file shows the rows it has, but not when the header claims
    // far more than the file holds.
    if need / 8 > data.len() {
        return Err(invalid("PAM: the file holds far less data than its header declares"));
    }
    let have = data.len().min(need);
    let data = &data[..have];

    let tuple = h.tuple_type.to_ascii_uppercase();
    // What the channels mean.
    let (gray, alpha, cmyk) = match tuple.as_str() {
        "BLACKANDWHITE" | "GRAYSCALE" => (true, false, false),
        "BLACKANDWHITE_ALPHA" | "GRAYSCALE_ALPHA" => (true, true, false),
        "RGB" => (false, false, false),
        "RGB_ALPHA" => (false, true, false),
        "CMYK" => (false, false, true),
        "CMYK_ALPHA" => (false, true, true),
        // No or unknown tuple type: go by the depth.
        _ => match h.depth {
            1 => (true, false, false),
            2 => (true, true, false),
            3 => (false, false, false),
            _ => (false, h.depth == 4, false),
        },
    };
    let depth = h.depth as usize;
    let colour_channels = if gray {
        1
    } else if cmyk {
        4
    } else {
        3
    };
    let needed = colour_channels + alpha as usize;
    if depth < needed {
        return Err(invalid(format!("PAM: tuple type {tuple} needs depth {needed}, not {depth}")));
    }

    let sample = |row: &[u8], at: usize| -> u32 {
        if wide { u16::from_be_bytes([row[at * 2], row[at * 2 + 1]]) as u32 } else { row[at] as u32 }
    };

    // 16-bit greyscale without alpha is measurement data.
    let grey16 = gray && !alpha && wide;
    let mut tex;
    if grey16 {
        let mut pixels = vec![0f32; n * 4];
        let scale = 1.0 / h.maxval as f32;
        for y in 0..h.height as usize {
            let Some(row) = data.get(y * row_bytes..(y + 1) * row_bytes) else {
                for p in pixels[y * h.width as usize * 4..].chunks_exact_mut(4) {
                    p[3] = 0.0;
                }
                break;
            };
            for x in 0..h.width as usize {
                let v = sample(row, x * depth) as f32 * scale;
                pixels[(y * h.width as usize + x) * 4..][..4].copy_from_slice(&[v, v, v, 1.0]);
            }
        }
        tex = Texture::from_rgba32f(h.width, h.height, pixels);
        tex.source.format = format!("{} 16-bit{}", describe_tuple(&tuple, h.depth), channel_tag(Channels::Data));
    } else {
        let to8 = |v: u32| -> u8 { ((v * 255 + h.maxval / 2) / h.maxval) as u8 };
        let mut rgba = vec![0u8; n * 4];
        for y in 0..h.height as usize {
            let Some(row) = data.get(y * row_bytes..(y + 1) * row_bytes) else { break };
            for x in 0..h.width as usize {
                let at = x * depth;
                let px = &mut rgba[(y * h.width as usize + x) * 4..][..4];
                if gray {
                    let g = to8(sample(row, at));
                    px[..3].fill(g);
                } else if cmyk {
                    let k = 1.0 - sample(row, at + 3) as f32 / h.maxval as f32;
                    for (c, out) in px.iter_mut().take(3).enumerate() {
                        let ink = 1.0 - sample(row, at + c) as f32 / h.maxval as f32;
                        *out = ((ink * k).clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
                    }
                } else {
                    for (c, out) in px.iter_mut().take(3).enumerate() {
                        *out = to8(sample(row, at + c));
                    }
                }
                px[3] = if alpha { to8(sample(row, at + colour_channels)) } else { 255 };
            }
        }
        tex = Texture::from_rgba8(h.width, h.height, rgba, ColorSpace::Unspecified);
        tex.source.format = format!("{} {}-bit", describe_tuple(&tuple, h.depth), if wide { 16 } else { 8 });
    }
    tex.source.container = "PAM".into();
    tex.source.file_bytes = bytes.len() as u64;
    tex.source.header = vec![
        ("WIDTH".into(), h.width.to_string()),
        ("HEIGHT".into(), h.height.to_string()),
        ("DEPTH".into(), h.depth.to_string()),
        ("MAXVAL".into(), h.maxval.to_string()),
        ("TUPLTYPE".into(), tuple),
    ];
    Ok(tex)
}

fn describe_tuple(tuple: &str, depth: u32) -> String {
    if tuple.is_empty() { format!("{depth} channels") } else { tuple.to_string() }
}

//endregion

/*┌─────────────────╮
| Farbfeld        |
└─────────────────╯*/
//region Farbfeld

/// Decodes a farbfeld file.
pub(crate) fn decode_farbfeld(bytes: &[u8]) -> Result<Texture, TextureError> {
    if bytes.len() < 16 || !sniff_farbfeld(bytes) {
        return Err(invalid("farbfeld: bad header"));
    }
    let width = u32::from_be_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]);
    let height = u32::from_be_bytes([bytes[12], bytes[13], bytes[14], bytes[15]]);
    let n = check_size(width, height, 1, 8)?;
    let data = &bytes[16..];
    // At least an eighth of the 8 bytes per pixel must be there.
    if n > data.len() {
        return Err(invalid("farbfeld: the file holds far less data than its header declares"));
    }
    let mut rgba = vec![0u8; n * 4];
    let mut has_alpha = false;
    for (px, s) in rgba.chunks_exact_mut(4).zip(data.chunks_exact(8)) {
        for c in 0..4 {
            let v = u16::from_be_bytes([s[c * 2], s[c * 2 + 1]]) as u32;
            // 16 bits to 8, rounded.
            px[c] = ((v * 255 + 32767) / 65535) as u8;
        }
        has_alpha |= px[3] != 255;
    }
    // A short file leaves the rest transparent.
    let have = data.len() / 8;
    for px in rgba.chunks_exact_mut(4).skip(have) {
        px[3] = 0;
    }
    if have < n {
        has_alpha = true;
    }
    let mut tex = Texture::from_rgba8(width, height, rgba, ColorSpace::Srgb);
    tex.has_alpha = has_alpha;
    tex.source.container = "farbfeld".into();
    tex.source.format = "RGBA 16-bit".into();
    tex.source.file_bytes = bytes.len() as u64;
    Ok(tex)
}

//endregion

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Pixels;

    fn pam(header: &str, data: &[u8]) -> Vec<u8> {
        let mut v = header.as_bytes().to_vec();
        v.extend(data);
        v
    }

    fn rgba8(t: &Texture) -> &[u8] {
        let Pixels::Rgba8(v) = t.base() else { panic!("not 8-bit") };
        v
    }

    #[test]
    fn rgb_alpha() {
        let f =
            pam("P7\nWIDTH 2\nHEIGHT 1\nDEPTH 4\nMAXVAL 255\nTUPLTYPE RGB_ALPHA\nENDHDR\n", &[1, 2, 3, 4, 5, 6, 7, 8]);
        let t = decode_pam(&f).unwrap();
        assert_eq!(rgba8(&t), &[1, 2, 3, 4, 5, 6, 7, 8]);
        assert!(t.has_alpha);
        assert_eq!(t.source.container, "PAM");
    }

    #[test]
    fn grayscale_scales_to_the_maximum() {
        let f = pam("P7\n# a comment\nWIDTH 3\nHEIGHT 1\nDEPTH 1\nMAXVAL 3\nTUPLTYPE GRAYSCALE\nENDHDR\n", &[0, 1, 3]);
        let t = decode_pam(&f).unwrap();
        assert_eq!(rgba8(&t), &[0, 0, 0, 255, 85, 85, 85, 255, 255, 255, 255, 255]);
    }

    #[test]
    fn black_and_white_has_one_for_white() {
        let f = pam("P7\nWIDTH 2\nHEIGHT 1\nDEPTH 1\nMAXVAL 1\nTUPLTYPE BLACKANDWHITE\nENDHDR\n", &[0, 1]);
        assert_eq!(rgba8(&decode_pam(&f).unwrap()), &[0, 0, 0, 255, 255, 255, 255, 255]);
    }

    #[test]
    fn sixteen_bit_colour_drops_to_eight_bits() {
        let f = pam(
            "P7\nWIDTH 1\nHEIGHT 1\nDEPTH 3\nMAXVAL 65535\nTUPLTYPE RGB\nENDHDR\n",
            &[0xff, 0xff, 0x80, 0x00, 0, 0],
        );
        assert_eq!(rgba8(&decode_pam(&f).unwrap()), &[255, 128, 0, 255]);
    }

    #[test]
    fn sixteen_bit_grey_is_data() {
        let f = pam("P7\nWIDTH 2\nHEIGHT 1\nDEPTH 1\nMAXVAL 65535\nTUPLTYPE GRAYSCALE\nENDHDR\n", &[0, 0, 0xff, 0xff]);
        let t = decode_pam(&f).unwrap();
        assert!(t.source.format.ends_with("(data)"), "{}", t.source.format);
        let Pixels::Rgba32F(v) = t.base() else { panic!() };
        assert_eq!((v[0], v[4]), (0.0, 1.0));
    }

    #[test]
    fn cmyk_and_untyped_depths() {
        let f = pam("P7\nWIDTH 1\nHEIGHT 1\nDEPTH 4\nMAXVAL 255\nTUPLTYPE CMYK\nENDHDR\n", &[0, 0, 0, 0]);
        assert_eq!(rgba8(&decode_pam(&f).unwrap()), &[255, 255, 255, 255]);
        let f = pam("P7\nWIDTH 1\nHEIGHT 1\nDEPTH 2\nMAXVAL 255\nENDHDR\n", &[9, 200]);
        assert_eq!(rgba8(&decode_pam(&f).unwrap()), &[9, 9, 9, 200]);
        // Tuple type lines add up.
        let h =
            parse_header(b"P7\nWIDTH 1\nHEIGHT 1\nDEPTH 1\nMAXVAL 1\nTUPLTYPE BLACKANDWHITE\nTUPLTYPE ALPHA\nENDHDR\n")
                .unwrap();
        assert_eq!(h.tuple_type, "BLACKANDWHITE ALPHA");
    }

    #[test]
    fn short_files_and_bad_headers_are_errors_or_partial() {
        let f = pam("P7\nWIDTH 2\nHEIGHT 2\nDEPTH 3\nMAXVAL 255\nTUPLTYPE RGB\nENDHDR\n", &[1, 2, 3, 4, 5, 6]);
        let t = decode_pam(&f).unwrap();
        assert_eq!(&rgba8(&t)[..4], &[1, 2, 3, 255]);
        assert_eq!(rgba8(&t)[8..12], [0, 0, 0, 0]);
        assert!(t.has_alpha);
        assert!(decode_pam(b"P7\nWIDTH 2\n").is_err());
        assert!(decode_pam(b"P7\nWIDTH 0\nHEIGHT 1\nDEPTH 1\nMAXVAL 1\nENDHDR\n").is_err());
        assert!(decode_pam(b"P7\nWIDTH 99999999\nHEIGHT 99999999\nDEPTH 1\nMAXVAL 255\nENDHDR\n").is_err());
        assert!(decode_pam(b"P7\nWIDTH 1\nHEIGHT 1\nDEPTH 1\nMAXVAL 255\nTUPLTYPE RGB\nENDHDR\n").is_err());
        assert!(decode_pam(b"P6\nWIDTH 1\n").is_err());
    }

    #[test]
    fn farbfeld() {
        let mut f = b"farbfeld".to_vec();
        f.extend([0, 0, 0, 2, 0, 0, 0, 1]);
        f.extend([0xff, 0xff, 0x80, 0x00, 0x00, 0x00, 0xff, 0xff]);
        f.extend([0x12, 0x34, 0x12, 0x34, 0x12, 0x34, 0x80, 0x00]);
        let t = decode_farbfeld(&f).unwrap();
        assert_eq!(rgba8(&t), &[255, 128, 0, 255, 18, 18, 18, 128]);
        assert!(t.has_alpha);
        assert!(decode_farbfeld(&f[..20]).unwrap().has_alpha);
        assert!(decode_farbfeld(&f[..10]).is_err());
        let mut huge = b"farbfeld".to_vec();
        huge.extend([0xff; 8]);
        assert!(decode_farbfeld(&huge).is_err());
    }
}
