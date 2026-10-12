//! Text palettes: GIMP's (also Inkscape's and Aseprite's), Paint Shop
//! Pro's JASC format and plain lists of hex colours.

use super::{Color, Palette, PaletteFormat, Swatch};
use slerp_scene::LoadError;

/// At most this many unreadable lines are reported, so a binary file
/// misnamed `.gpl` doesn't make thousands of warnings.
const MAX_WARNINGS: usize = 5;

/// The text after a UTF-8 byte-order mark, if any.
fn without_bom(b: &[u8]) -> &[u8] {
    b.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(b)
}

pub fn is_gpl(head: &[u8]) -> bool {
    without_bom(head).starts_with(b"GIMP Palette")
}

pub fn is_jasc(head: &[u8]) -> bool {
    without_bom(head).starts_with(b"JASC-PAL")
}

fn rgb8(v: [u8; 3]) -> Color {
    Color::Rgb(v.map(|c| c as f32 / 255.0))
}

fn warn(p: &mut Palette, line: usize, text: &str) {
    if p.warnings.len() < MAX_WARNINGS {
        p.warnings.push(format!("line {line} isn't a colour: {}", text.chars().take(40).collect::<String>()));
    }
}

/// Splits the next whitespace-separated word off `s`.
fn word(s: &str) -> Option<(&str, &str)> {
    let s = s.trim_start();
    if s.is_empty() {
        return None;
    }
    let end = s.find(char::is_whitespace).unwrap_or(s.len());
    Some((&s[..end], &s[end..]))
}

/// GIMP palettes: `GIMP Palette`, optional `Name:`, `Columns:` and (from
/// Aseprite) `Channels: RGBA` lines, `#` comments, then one colour per line
/// as decimal red, green and blue (and alpha with RGBA channels) followed
/// by an optional name.
pub fn gpl(b: &[u8]) -> Result<Palette, LoadError> {
    let mut p = Palette::new(PaletteFormat::Gpl);
    let text = String::from_utf8_lossy(without_bom(b));
    let mut alpha = false;
    for (n, line) in text.lines().enumerate().skip(1) {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(v) = line.strip_prefix("Name:") {
            p.name = v.trim().to_string();
            continue;
        }
        if let Some(v) = line.strip_prefix("Columns:") {
            p.columns = v.trim().parse().ok().filter(|&c| c > 0);
            continue;
        }
        if let Some(v) = line.strip_prefix("Channels:") {
            alpha = v.trim().eq_ignore_ascii_case("RGBA");
            continue;
        }
        let mut rest = line;
        let mut values = [255u8; 4];
        let mut ok = true;
        for v in values.iter_mut().take(if alpha { 4 } else { 3 }) {
            match word(rest).and_then(|(w, r)| Some((w.parse::<u8>().ok()?, r))) {
                Some((x, r)) => {
                    *v = x;
                    rest = r;
                }
                None => ok = false,
            }
        }
        if !ok {
            warn(&mut p, n + 1, line);
            continue;
        }
        let mut s = Swatch::new(rest.trim(), rgb8([values[0], values[1], values[2]]));
        s.alpha = values[3] as f32 / 255.0;
        if !p.push(s) {
            break;
        }
    }
    Ok(p)
}

/// JASC palettes: `JASC-PAL`, a version (`0100`), a count, then that many
/// lines of decimal red, green and blue.
pub fn jasc(b: &[u8]) -> Result<Palette, LoadError> {
    let mut p = Palette::new(PaletteFormat::JascPal);
    let text = String::from_utf8_lossy(without_bom(b));
    let mut lines = text.lines().map(str::trim).enumerate().skip(1);
    let _version = lines.next();
    let count: usize = lines.next().and_then(|(_, l)| l.parse().ok()).unwrap_or(usize::MAX);
    for (n, line) in lines.filter(|(_, l)| !l.is_empty()).take(count) {
        let v: Vec<u8> = line.split_whitespace().take(3).filter_map(|w| w.parse().ok()).collect();
        match v[..] {
            [r, g, b] => {
                if !p.push(Swatch::new(String::new(), rgb8([r, g, b]))) {
                    break;
                }
            }
            _ => warn(&mut p, n + 1, line),
        }
    }
    if count != usize::MAX && p.len() < count {
        p.warnings.push(format!("{count} colours announced, {} found", p.len()));
    }
    Ok(p)
}

/// One colour of a hex list: six hex digits, with or without `#`.  Eight
/// digits (with alpha) aren't taken: memory images for Verilog's
/// `$readmemh`, also named `.hex`, are lines of eight hex digits.
fn hex_color(line: &str) -> Option<[u8; 3]> {
    let digits = line.strip_prefix('#').unwrap_or(line);
    if digits.len() != 6 || !digits.bytes().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let byte = |i: usize| u8::from_str_radix(&digits[i..i + 2], 16).ok();
    Some([byte(0)?, byte(2)?, byte(4)?])
}

/// A hex list has only colour lines and blank lines.  The start of a file
/// is all that is seen, so a last line without its line break is let off.
/// Intel HEX firmware (lines starting with `:`) never passes.
pub fn is_hex_list(head: &[u8]) -> bool {
    let Ok(text) = std::str::from_utf8(without_bom(head)) else {
        // A multi-byte character cut by the end of the head would be
        // invalid; a hex list has none anyway.
        return false;
    };
    let mut lines: Vec<&str> = text.split('\n').collect();
    if !text.ends_with('\n') && lines.len() > 1 && head.len() >= 4096 {
        lines.pop();
    }
    let mut colours = 0;
    for line in lines {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if hex_color(line).is_none() {
            return false;
        }
        colours += 1;
    }
    colours > 0
}

/// A list of hex colours, one per line, as Lospec offers palettes.
pub fn hex(b: &[u8]) -> Result<Palette, LoadError> {
    let mut p = Palette::new(PaletteFormat::Hex);
    let text = String::from_utf8_lossy(without_bom(b));
    for (n, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        match hex_color(line) {
            Some(c) => {
                if !p.push(Swatch::new(String::new(), rgb8(c))) {
                    break;
                }
            }
            None => warn(&mut p, n + 1, line),
        }
    }
    Ok(p)
}
