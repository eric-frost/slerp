//! Apple icon files (`.icns`): a list of typed elements, each one picture
//! of the icon at one size, in the encodings of three decades of macOS:
//!
//! - PNG or JPEG 2000 files (`ic07` to `ic14`, `icp4` to `icp6`, `ic04`,
//!   `ic05`, `icsb`, `icsB`, `sb24`, `SB24`), the modern kind;
//! - 24-bit colour run-length coded per channel (`is32`, `il32`, `ih32`,
//!   `it32`) with an 8-bit alpha mask in a separate element (`s8mk`,
//!   `l8mk`, `h8mk`, `t8mk`), and the `ARGB` form of the same;
//! - classic Mac OS icons of 1, 4 and 8 bits per pixel with the system
//!   palettes (`ICON`, `ICN#`, `ics#`, `icm#`, `ich#`, `icl4`, `icl8` and
//!   friends), whose `#` elements also carry the 1-bit mask.
//!
//! A dark-mode variant (an `icns` nested in element `0xFDD92FA8`) is read
//! too.  Every picture is listed, largest first.

use crate::ico::decode_png;
use crate::jp2;

pub struct Picture {
    pub width: u32,
    pub height: u32,
    /// Element type and encoding, such as "ic09, PNG" or "il32, RLE".
    pub label: String,
    pub dark: bool,
    pub rgba: Vec<u8>,
}

pub struct Icns {
    pub pictures: Vec<Picture>,
    pub warnings: Vec<String>,
}

const MAX_SIDE: u32 = 4096;
const MAX_ELEMENTS: usize = 1 << 16;

pub fn is_icns(head: &[u8]) -> bool {
    head.len() >= 8 && head.starts_with(b"icns") && u32::from_be_bytes([head[4], head[5], head[6], head[7]]) >= 8
}

/// Size of the colour elements, and of their masks.
fn rle_size(kind: &[u8; 4]) -> Option<u32> {
    Some(match kind {
        b"is32" | b"s8mk" => 16,
        b"il32" | b"l8mk" => 32,
        b"ih32" | b"h8mk" => 48,
        b"it32" | b"t8mk" => 128,
        _ => return None,
    })
}

/// Element types of compressed pictures, with their nominal pixel size.
fn file_size(kind: &[u8; 4]) -> Option<u32> {
    Some(match kind {
        b"icp4" | b"ic04" => 16,
        b"sb24" => 24,
        b"icp5" | b"ic05" | b"ic11" => 32,
        b"icsb" => 18,
        b"icsB" => 36,
        b"SB24" => 48,
        b"icp6" | b"ic12" => 64,
        b"ic07" => 128,
        b"ic08" | b"ic13" => 256,
        b"ic09" | b"ic14" => 512,
        b"ic10" => 1024,
        _ => return None,
    })
}

/// Classic icons: (width, height, bits per pixel), and the element holding
/// their mask.
fn classic(kind: &[u8; 4]) -> Option<(u32, u32, u32, &'static [u8; 4])> {
    Some(match kind {
        b"ICON" => (32, 32, 1, b"    "),
        b"ICN#" => (32, 32, 1, b"ICN#"),
        b"icl4" => (32, 32, 4, b"ICN#"),
        b"icl8" => (32, 32, 8, b"ICN#"),
        b"ics#" => (16, 16, 1, b"ics#"),
        b"ics4" => (16, 16, 4, b"ics#"),
        b"ics8" => (16, 16, 8, b"ics#"),
        b"icm#" => (16, 12, 1, b"icm#"),
        b"icm4" => (16, 12, 4, b"icm#"),
        b"icm8" => (16, 12, 8, b"icm#"),
        b"ich#" => (48, 48, 1, b"ich#"),
        b"ich4" => (48, 48, 4, b"ich#"),
        b"ich8" => (48, 48, 8, b"ich#"),
        _ => return None,
    })
}

pub fn decode(d: &[u8]) -> Result<Icns, String> {
    if !is_icns(d) {
        return Err("not an Apple icon file".into());
    }
    let mut out = Icns { pictures: Vec::new(), warnings: Vec::new() };
    read(d, false, &mut out);
    if out.pictures.is_empty() {
        let why = out.warnings.first().cloned().unwrap_or_else(|| "no pictures".into());
        return Err(format!("the icon file holds no readable picture ({why})"));
    }
    // Largest first; at one size, the richer encodings before the old.
    let rank = |p: &Picture| match p.label.split(", ").nth(1).unwrap_or("") {
        "PNG" | "JPEG 2000" | "ARGB" => 0,
        l if l.starts_with("RLE") || l.starts_with("32-bit") => 1,
        l if l.starts_with("8-bit") => 2,
        l if l.starts_with("4-bit") => 3,
        _ => 4,
    };
    out.pictures.sort_by_key(|p| (p.dark, std::cmp::Reverse(p.width * p.height), rank(p)));
    Ok(out)
}

fn read(d: &[u8], dark: bool, out: &mut Icns) {
    let total = (u32::from_be_bytes([d[4], d[5], d[6], d[7]]) as usize).min(d.len());
    if total < d.len() && !dark {
        out.warnings.push("data after the icon list is ignored".into());
    }
    // 1. The elements.
    let mut elements: Vec<([u8; 4], &[u8])> = Vec::new();
    let mut at = 8;
    while at + 8 <= total && elements.len() < MAX_ELEMENTS {
        let kind: [u8; 4] = d[at..at + 4].try_into().unwrap_or([0; 4]);
        let len = u32::from_be_bytes([d[at + 4], d[at + 5], d[at + 6], d[at + 7]]) as usize;
        if len < 8 || at + len > total {
            out.warnings.push(format!("element {} is cut short", name(&kind)));
            let end = total.min(at.saturating_add(len.max(8)));
            elements.push((kind, &d[at + 8..end.max(at + 8)]));
            break;
        }
        elements.push((kind, &d[at + 8..at + len]));
        at += len;
    }
    let find = |k: &[u8; 4]| elements.iter().find(|(e, _)| e == k).map(|(_, data)| *data);
    // 2. Each picture.
    for &(kind, data) in &elements {
        let picture = if kind == [0xfd, 0xd9, 0x2f, 0xa8] {
            if !dark && is_icns(data) {
                read(data, true, out);
            }
            continue;
        } else if let Some(size) = file_size(&kind) {
            compressed(&kind, data, size)
        } else if let Some(size) = rle_size(&kind) {
            if kind.ends_with(b"mk") {
                continue;
            }
            // The 8-bit mask, else the 1-bit one of the classic icon of that
            // size (the second half of its `#` element).
            let mask_kind = [kind[1], b'8', b'm', b'k'];
            let one_bit = match &kind {
                b"is32" => find(b"ics#"),
                b"il32" => find(b"ICN#"),
                b"ih32" => find(b"ich#"),
                _ => None,
            };
            let mask = match (find(&mask_kind), one_bit) {
                (Some(m), _) => Mask::Bytes(m),
                (None, Some(m)) => Mask::Bits(m),
                _ => Mask::None,
            };
            rle_picture(&kind, data, size, mask)
        } else if let Some((w, h, bits, mask)) = classic(&kind) {
            // A `#` element is the 1-bit icon and its mask; shown only when
            // no deeper icon of that size exists.
            if bits == 1 && kind != *b"ICON" {
                let rle = match &kind {
                    b"ics#" => *b"is32",
                    b"ICN#" => *b"il32",
                    b"ich#" => *b"ih32",
                    _ => *b"    ",
                };
                let deeper = elements
                    .iter()
                    .any(|(e, _)| *e == rle || classic(e).is_some_and(|(_, _, b, m)| b > 1 && *m == kind));
                if deeper {
                    continue;
                }
            }
            let mask = if mask == b"    " { None } else { find(mask) };
            classic_picture(&kind, data, w, h, bits, mask)
        } else {
            continue;
        };
        match picture {
            Ok(mut p) => {
                p.dark = dark;
                if dark {
                    p.label.push_str(", dark");
                }
                out.pictures.push(p);
            }
            Err(e) => out.warnings.push(format!("{}: {e}", name(&kind))),
        }
    }
}

fn name(kind: &[u8; 4]) -> String {
    kind.iter().map(|&b| if b.is_ascii_graphic() || b == b' ' { b as char } else { '?' }).collect()
}

/// A PNG, JPEG 2000 or ARGB element.
fn compressed(kind: &[u8; 4], data: &[u8], nominal: u32) -> Result<Picture, String> {
    let label = |enc: &str| format!("{}, {enc}", name(kind));
    if data.starts_with(b"\x89PNG") {
        let e = decode_png(data)?;
        check(e.width, e.height)?;
        return Ok(Picture { width: e.width, height: e.height, label: label("PNG"), dark: false, rgba: e.rgba });
    }
    if jp2::is_jp2(data) {
        let j = jp2::decode(data)?;
        check(j.width, j.height)?;
        let rgba = match j.pixels {
            jp2::Pixels::Rgba8(px) => px,
            jp2::Pixels::Grey16(px) => px.iter().map(|v| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8).collect(),
        };
        return Ok(Picture { width: j.width, height: j.height, label: label("JPEG 2000"), dark: false, rgba });
    }
    if data.starts_with(b"ARGB") {
        let planes = unpack_rle(&data[4..], (nominal * nominal) as usize, 4)?;
        let n = (nominal * nominal) as usize;
        let rgba = (0..n).flat_map(|i| [planes[n + i], planes[2 * n + i], planes[3 * n + i], planes[i]]).collect();
        return Ok(Picture { width: nominal, height: nominal, label: label("ARGB"), dark: false, rgba });
    }
    // `icp4` and `icp5` may also hold raw run-length RGB.
    if matches!(kind, b"icp4" | b"icp5") {
        return rle_picture(kind, data, nominal, Mask::None);
    }
    Err("unknown encoding".into())
}

fn check(w: u32, h: u32) -> Result<(), String> {
    if w == 0 || h == 0 || w > MAX_SIDE || h > MAX_SIDE {
        return Err(format!("{w} × {h} is not a plausible icon size"));
    }
    Ok(())
}

/// Where a colour icon's transparency comes from.
enum Mask<'a> {
    None,
    /// An 8-bit mask element.
    Bytes(&'a [u8]),
    /// A classic `#` element: the 1-bit icon, then its 1-bit mask.
    Bits(&'a [u8]),
}

/// 24-bit colour, run-length coded one channel after another (or stored
/// as 32-bit pixels when the element is exactly that long), with alpha
/// from the matching mask.
fn rle_picture(kind: &[u8; 4], data: &[u8], size: u32, mask: Mask) -> Result<Picture, String> {
    let n = (size * size) as usize;
    let data = if kind == b"it32" && data.starts_with(&[0, 0, 0, 0]) { &data[4..] } else { data };
    let (rgb, encoding) = if data.len() == n * 4 {
        let rgb: Vec<u8> = (0..3).flat_map(|c| data.chunks_exact(4).map(move |p| p[c + 1])).collect();
        (rgb, "32-bit")
    } else {
        (unpack_rle(data, n, 3)?, "RLE")
    };
    let alpha: Vec<u8> = match mask {
        Mask::Bytes(m) if m.len() >= n => m[..n].to_vec(),
        Mask::Bits(m) if m.len() >= 2 * n / 8 => {
            (0..n).map(|i| if m[n / 8 + i / 8] >> (7 - i % 8) & 1 == 1 { 255 } else { 0 }).collect()
        }
        _ => Vec::new(),
    };
    let rgba =
        (0..n).flat_map(|i| [rgb[i], rgb[n + i], rgb[2 * n + i], alpha.get(i).copied().unwrap_or(255)]).collect();
    let label = format!("{}, {encoding}{}", name(kind), if alpha.is_empty() { "" } else { " with mask" });
    Ok(Picture { width: size, height: size, label, dark: false, rgba })
}

/// Apple's icon run-length code, channel after channel: a byte below 0x80
/// is followed by that many plus one literal bytes, one of 0x80 or above
/// by one byte repeated that many minus 125 times.
fn unpack_rle(data: &[u8], per_channel: usize, channels: usize) -> Result<Vec<u8>, String> {
    let mut out = vec![0u8; per_channel * channels];
    let mut i = 0;
    for c in 0..channels {
        let plane = &mut out[c * per_channel..(c + 1) * per_channel];
        let mut x = 0;
        while x < per_channel {
            let Some(&b) = data.get(i) else {
                return if c == channels - 1 && x > 0 {
                    Ok(out)
                } else {
                    Err("the run-length data is cut short".into())
                };
            };
            i += 1;
            if b < 0x80 {
                let n = b as usize + 1;
                let lit = data.get(i..i + n).ok_or("the run-length data is cut short")?;
                let take = n.min(per_channel - x);
                plane[x..x + take].copy_from_slice(&lit[..take]);
                x += take;
                i += n;
            } else {
                let n = b as usize - 125;
                let &v = data.get(i).ok_or("the run-length data is cut short")?;
                i += 1;
                let take = n.min(per_channel - x);
                plane[x..x + take].fill(v);
                x += take;
            }
        }
    }
    Ok(out)
}

/// A classic Mac OS icon: 1-bit (black on white), 4-bit or 8-bit with the
/// system palettes, and a 1-bit mask (the second half of a `#` element).
fn classic_picture(
    kind: &[u8; 4],
    data: &[u8],
    w: u32,
    h: u32,
    bits: u32,
    mask: Option<&[u8]>,
) -> Result<Picture, String> {
    let n = (w * h) as usize;
    let need = n * bits as usize / 8;
    if data.len() < need {
        return Err("the icon is cut short".into());
    }
    let bit = |src: &[u8], i: usize| src[i / 8] >> (7 - i % 8) & 1 == 1;
    let mask_bits = mask.filter(|m| m.len() >= 2 * n / 8).map(|m| &m[n / 8..2 * n / 8]);
    let mut rgba = Vec::with_capacity(n * 4);
    for i in 0..n {
        let [r, g, b] = match bits {
            1 => {
                if bit(data, i) {
                    [0, 0, 0]
                } else {
                    [255, 255, 255]
                }
            }
            4 => MAC4[(data[i / 2] >> (if i % 2 == 0 { 4 } else { 0 }) & 15) as usize],
            _ => mac8(data[i]),
        };
        let a = match mask_bits {
            Some(m) => {
                if bit(m, i) {
                    255
                } else {
                    0
                }
            }
            None => 255,
        };
        rgba.extend_from_slice(&[r, g, b, a]);
    }
    let label = format!("{}, {bits}-bit{}", name(kind), if mask_bits.is_some() { " with mask" } else { "" });
    Ok(Picture { width: w, height: h, label, dark: false, rgba })
}

/// The classic Mac OS 16-colour palette.
const MAC4: [[u8; 3]; 16] = [
    [0xff, 0xff, 0xff],
    [0xfc, 0xf3, 0x05],
    [0xff, 0x64, 0x02],
    [0xdd, 0x08, 0x06],
    [0xf2, 0x08, 0x84],
    [0x46, 0x00, 0xa5],
    [0x00, 0x00, 0xd4],
    [0x02, 0xab, 0xea],
    [0x1f, 0xb7, 0x14],
    [0x00, 0x64, 0x11],
    [0x56, 0x2c, 0x05],
    [0x90, 0x71, 0x3a],
    [0xc0, 0xc0, 0xc0],
    [0x80, 0x80, 0x80],
    [0x40, 0x40, 0x40],
    [0x00, 0x00, 0x00],
];

/// The classic Mac OS 256-colour system palette: a 6 × 6 × 6 cube from
/// white down without its black (0 to 214), ramps of red, green, blue and
/// grey in the steps the cube skips (215 to 254), then black.
fn mac8(i: u8) -> [u8; 3] {
    const RAMP: [u8; 10] = [0xee, 0xdd, 0xbb, 0xaa, 0x88, 0x77, 0x55, 0x44, 0x22, 0x11];
    let i = i as usize;
    match i {
        0..=214 => {
            let level = |k: usize| (5 - k) as u8 * 0x33;
            [level(i / 36), level(i / 6 % 6), level(i % 6)]
        }
        215..=224 => [RAMP[i - 215], 0, 0],
        225..=234 => [0, RAMP[i - 225], 0],
        235..=244 => [0, 0, RAMP[i - 235]],
        245..=254 => [RAMP[i - 245]; 3],
        _ => [0, 0, 0],
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;

    /// Packs one channel as Apple does: runs of 3 or more as repeats,
    /// everything else as literals.
    pub fn pack_rle(plane: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut i = 0;
        while i < plane.len() {
            let mut run = 1;
            while i + run < plane.len() && plane[i + run] == plane[i] && run < 130 {
                run += 1;
            }
            if run >= 3 {
                out.extend_from_slice(&[(run + 125) as u8, plane[i]]);
                i += run;
            } else {
                let start = i;
                while i < plane.len() && i - start < 128 {
                    if i + 2 < plane.len() && plane[i] == plane[i + 1] && plane[i] == plane[i + 2] {
                        break;
                    }
                    i += 1;
                }
                out.push((i - start - 1) as u8);
                out.extend_from_slice(&plane[start..i]);
            }
        }
        out
    }

    pub fn element(kind: &[u8; 4], data: &[u8]) -> Vec<u8> {
        let mut e = kind.to_vec();
        e.extend_from_slice(&((data.len() + 8) as u32).to_be_bytes());
        e.extend_from_slice(data);
        e
    }

    pub fn file(elements: &[Vec<u8>]) -> Vec<u8> {
        let body: Vec<u8> = elements.concat();
        let mut f = b"icns".to_vec();
        f.extend_from_slice(&((body.len() + 8) as u32).to_be_bytes());
        f.extend(body);
        f
    }

    fn png(w: u32, h: u32, rgba: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut enc = png::Encoder::new(&mut out, w, h);
        enc.set_color(png::ColorType::Rgba);
        enc.write_header().unwrap().write_image_data(rgba).unwrap();
        out
    }

    #[test]
    fn modern_rle_and_classic_pictures() {
        let n = 16 * 16;
        let planes: Vec<Vec<u8>> = vec![
            (0..n).map(|i| (i % 7) as u8 * 30).collect(),
            vec![200; n],
            (0..n).map(|i| (i / 16) as u8 * 16).collect(),
        ];
        let rle: Vec<u8> = planes.iter().flat_map(|p| pack_rle(p)).collect();
        let mask: Vec<u8> = (0..n).map(|i| if i < 16 { 0 } else { 255 }).collect();
        let png32 = png(32, 32, &vec![9; 32 * 32 * 4]);
        // An 8-bit 32 × 32 icon and its 1-bit mask: all colour 5, all
        // opaque but the first row.
        let mut icn = vec![0u8; 128];
        icn.extend((0..128).map(|i| if i < 4 { 0 } else { 0xff }));
        let f = file(&[
            element(b"is32", &rle),
            element(b"s8mk", &mask),
            element(b"ic11", &png32),
            element(b"icl8", &[5; 1024]),
            element(b"ICN#", &icn),
            element(b"TOC ", &[0; 8]),
        ]);
        let icns = decode(&f).unwrap();
        assert!(icns.warnings.is_empty(), "{:?}", icns.warnings);
        let labels: Vec<&str> = icns.pictures.iter().map(|p| p.label.as_str()).collect();
        assert_eq!(labels, ["ic11, PNG", "icl8, 8-bit with mask", "is32, RLE with mask"]);
        let small = &icns.pictures[2];
        assert_eq!(&small.rgba[0..4], &[0, 200, 0, 0]);
        assert_eq!(&small.rgba[(17 * 4)..(17 * 4 + 4)], &[90, 200, 16, 255]);
        let classic = &icns.pictures[1];
        assert_eq!(&classic.rgba[0..4], &[0xff, 0xff, 0x00, 0]);
        assert_eq!(classic.rgba[32 * 4 + 3], 255);
        assert_eq!(mac8(255), [0, 0, 0]);
        assert_eq!(mac8(0), [255, 255, 255]);
        assert_eq!(mac8(214), [0, 0, 0x33]);
    }

    #[test]
    fn damaged_files_are_bounded() {
        let rle: Vec<u8> = [7u8, 8, 9].iter().flat_map(|&v| pack_rle(&[v; 48 * 48])).collect();
        let f = file(&[element(b"ih32", &rle), element(b"ic07", b"\x89PNG garbage")]);
        let icns = decode(&f).unwrap();
        assert_eq!(icns.pictures.len(), 1);
        assert_eq!(icns.warnings.len(), 1);
        for cut in 0..f.len() {
            let _ = decode(&f[..cut]);
        }
        // An element claiming 4 GB.
        let mut bad = f.clone();
        bad[12..16].copy_from_slice(&u32::MAX.to_be_bytes());
        let icns = decode(&bad).unwrap();
        assert!(icns.warnings.iter().any(|w| w.contains("cut short")));
        assert_eq!(&icns.pictures[0].rgba[0..4], &[7, 8, 9, 255]);
    }
}
