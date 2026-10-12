//! Turning the object records of WMF and EMF into pens, brushes and fonts.
//! The two formats store the same GDI structures with different field
//! widths, and share the flag values.

use crate::Rgb;
use crate::state::*;
use std::rc::Rc;
use tiny_skia::{LineCap, LineJoin};

/// A pen from `PS_*` style flags, a width, a colour and (for
/// `PS_USERSTYLE`) the dash lengths in units of the width.
pub fn pen(style: u32, width: f64, color: Rgb, user_dashes: Vec<f32>) -> Pen {
    let base = style & 0xF;
    let geometric = style & 0xF_0000 == 0x1_0000;
    let cap = match style & 0xF00 {
        0x100 => LineCap::Square,
        0x200 => LineCap::Butt,
        _ => LineCap::Round,
    };
    let join = match style & 0xF000 {
        0x1000 => LineJoin::Bevel,
        0x2000 => LineJoin::Miter,
        _ => LineJoin::Round,
    };
    let dashes = match base {
        1 => Dashes::Dash,
        2 => Dashes::Dot,
        3 => Dashes::DashDot,
        4 => Dashes::DashDotDot,
        7 if !user_dashes.is_empty() => Dashes::User(user_dashes),
        8 => Dashes::Alternate,
        _ => Dashes::Solid,
    };
    Pen { dashes, width: width.max(0.0), color, geometric, cap, join, null: base == 5, inside_frame: base == 6 }
}

/// A brush that needs no bitmap: solid, hollow or hatched.  Pattern brushes
/// are built where the bitmap is.
pub fn simple_brush(style: u32, color: Rgb, hatch: u32) -> Brush {
    match style {
        1 => Brush::Null,
        2 => Brush::Hatch(Rc::new(hatch_tile(hatch)), color),
        _ => Brush::Solid(color),
    }
}

/// The fields of a `LOGFONT` that text drawing uses, however wide they were
/// stored.
pub fn font(
    height: i32,
    escapement: i32,
    weight: i32,
    flags: [u8; 3],
    charset: u8,
    pitch_family: u8,
    face: String,
) -> Font {
    Font {
        height: height as f64,
        escapement: escapement as f64,
        weight: weight.max(0) as u32,
        italic: flags[0] != 0,
        underline: flags[1] != 0,
        strike_out: flags[2] != 0,
        charset,
        pitch_family,
        face,
    }
}

/// The code page a Windows character set number (`lfCharSet`) means, for
/// the sets that aren't Windows-1252, and whether it has double-byte
/// characters.
fn code_page(charset: u8) -> Option<(&'static encoding_rs::Encoding, bool)> {
    use encoding_rs::*;
    Some(match charset {
        128 => (SHIFT_JIS, true),
        129 | 130 => (EUC_KR, true),
        134 => (GBK, true),
        136 => (BIG5, true),
        161 => (WINDOWS_1253, false),
        162 => (WINDOWS_1254, false),
        163 => (WINDOWS_1258, false),
        177 => (WINDOWS_1255, false),
        178 => (WINDOWS_1256, false),
        186 => (WINDOWS_1257, false),
        204 => (WINDOWS_1251, false),
        222 => (WINDOWS_874, false),
        238 => (WINDOWS_1250, false),
        _ => return None,
    })
}

/// Decodes a metafile's 8-bit text in the font's character set, giving each
/// character with the number of bytes it took (`ExtTextOut` spaces its
/// advances by byte).  Unknown sets and the Western ones are Windows-1252.
pub fn decode_ansi_spans(bytes: &[u8], charset: u8) -> Vec<(char, u8)> {
    let mut out = Vec::with_capacity(bytes.len());
    let page = code_page(charset);
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        match page {
            Some((enc, double)) => {
                // Lead bytes of the double-byte sets.
                let lead = double && b >= 0x81 && b != 0xFF && !(charset == 128 && (0xA0..=0xDF).contains(&b));
                let n = if lead && i + 1 < bytes.len() { 2 } else { 1 };
                let (text, _, _) = enc.decode(&bytes[i..i + n]);
                out.push((text.chars().next().unwrap_or('\u{FFFD}'), n as u8));
                i += n;
            }
            None => {
                out.push((
                    match b {
                        0x80..=0x9F => CP1252_HIGH[(b - 0x80) as usize],
                        _ => b as char,
                    },
                    1,
                ));
                i += 1;
            }
        }
    }
    out
}

/// Windows-1252 for the bytes 0x80 to 0x9F, which differ from Latin-1.
const CP1252_HIGH: [char; 32] = [
    '\u{20AC}', '\u{81}', '\u{201A}', '\u{192}', '\u{201E}', '\u{2026}', '\u{2020}', '\u{2021}', '\u{2C6}', '\u{2030}',
    '\u{160}', '\u{2039}', '\u{152}', '\u{8D}', '\u{17D}', '\u{8F}', '\u{90}', '\u{2018}', '\u{2019}', '\u{201C}',
    '\u{201D}', '\u{2022}', '\u{2013}', '\u{2014}', '\u{2DC}', '\u{2122}', '\u{161}', '\u{203A}', '\u{153}', '\u{9D}',
    '\u{17E}', '\u{178}',
];

/// Decodes a metafile's 8-bit text in the font's character set.
pub fn decode_ansi(bytes: &[u8], charset: u8) -> Vec<char> {
    decode_ansi_spans(bytes, charset).into_iter().map(|(c, _)| c).collect()
}

/// Decodes UTF-16 text, replacing unpaired surrogates.
pub fn decode_utf16(units: &[u16]) -> Vec<char> {
    char::decode_utf16(units.iter().copied()).map(|r| r.unwrap_or('\u{FFFD}')).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pen_flags() {
        let p = pen(0x1_2201, 4.0, [1, 2, 3], vec![]);
        assert!(p.geometric && p.dashes == Dashes::Dash);
        assert!(matches!(p.cap, LineCap::Butt) && matches!(p.join, LineJoin::Miter));
        assert!(pen(5, 1.0, [0; 3], vec![]).null);
        assert!(pen(6, 3.0, [0; 3], vec![]).inside_frame);
    }

    #[test]
    fn text_decoding() {
        assert_eq!(decode_ansi(b"A\x80\xE9", 0), vec!['A', '\u{20AC}', '\u{E9}']);
        // Shift-JIS hiragana "a", a half-width katakana byte and ASCII.
        assert_eq!(decode_ansi(&[0x82, 0xA0, 0xB1, b'x'], 128), vec!['\u{3042}', '\u{FF71}', 'x']);
        assert_eq!(decode_ansi(&[0xC4, 0xE3], 134), vec!['\u{4F60}']);
        assert_eq!(decode_ansi(&[0xCF, 0xF0], 204), vec!['\u{41F}', '\u{440}']);
        assert_eq!(decode_ansi_spans(&[0x82, 0xA0, b'x'], 128), vec![('\u{3042}', 2), ('x', 1)]);
        assert_eq!(decode_utf16(&[0x41, 0xD800, 0x42]), vec!['A', '\u{FFFD}', 'B']);
    }
}
