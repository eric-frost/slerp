//! XBM, the X BitMap: C source with `#define name_width` and
//! `name_height` (and maybe a hot spot) and an array of bytes (X11) or
//! 16-bit words (X10), each row padded to whole bytes or words, bits least
//! significant first.  Set bits are the foreground, drawn black on white as
//! X shows bitmaps by default.

pub struct Xbm {
    pub width: u32,
    pub height: u32,
    pub x10: bool,
    pub hotspot: Option<(u32, u32)>,
    pub rgba: Vec<u8>,
    pub warnings: Vec<String>,
}

/// An XBM starts with its width's `#define`, after comments at most, and
/// declares its `_bits` array soon after.
pub fn is_xbm(head: &[u8]) -> bool {
    let text = String::from_utf8_lossy(&head[..head.len().min(1024)]);
    let mut rest = text.trim_start();
    while let Some(after) = rest.strip_prefix("/*") {
        rest = after.find("*/").map_or("", |e| &after[e + 2..]).trim_start();
    }
    let Some(def) = rest.strip_prefix("#define") else { return false };
    let mut words = def.split_whitespace();
    let name_ok = words.next().is_some_and(|n| n.ends_with("_width") || n == "width");
    // The bits array follows the few defines, so a C header that merely
    // starts with a `_width` define isn't taken for a bitmap.
    name_ok && words.next().is_some_and(|v| v.parse::<u32>().is_ok()) && text.contains("_bits")
}

pub fn decode(d: &[u8], max_pixels: u64) -> Result<Xbm, String> {
    let text = String::from_utf8_lossy(d);
    let define = |suffix: &str| -> Option<u64> {
        text.lines().find_map(|l| {
            let mut w = l.split_whitespace();
            (w.next()? == "#define").then_some(())?;
            let name = w.next()?;
            (name == suffix.trim_start_matches('_') || name.ends_with(suffix)).then_some(())?;
            w.next()?.parse().ok()
        })
    };
    let (w, h) = (define("_width").ok_or("no width")?, define("_height").ok_or("no height")?);
    if w == 0 || h == 0 || w.saturating_mul(h) > max_pixels {
        return Err(format!("a bitmap of {w} × {h} pixels is not allowed"));
    }
    let hotspot = define("_x_hot").zip(define("_y_hot")).map(|(x, y)| (x as u32, y as u32));
    let open = text.find('{').ok_or("no bitmap data")?;
    let x10 = text[..open].contains("short");
    let (w, h) = (w as usize, h as usize);
    let unit = if x10 { 16 } else { 8 };
    let per_row = w.div_ceil(unit);
    let body = &text[open + 1..];
    let body = &body[..body.find('}').unwrap_or(body.len())];
    let mut values = Vec::with_capacity(per_row * h);
    for v in body.split(',') {
        if values.len() >= per_row * h {
            break;
        }
        let v = v.trim();
        if v.is_empty() {
            continue;
        }
        let parsed = match v.strip_prefix("0x").or_else(|| v.strip_prefix("0X")) {
            Some(hex) => u32::from_str_radix(hex, 16).ok(),
            None => v.parse::<u32>().ok(),
        };
        values.push(parsed.unwrap_or(0));
    }
    let mut warnings = Vec::new();
    if values.len() < per_row * h {
        warnings.push(format!("the bitmap is cut short: {} of {} values", values.len(), per_row * h));
    }
    let mut rgba = vec![255u8; w * h * 4];
    for y in 0..h {
        for x in 0..w {
            let v = values.get(y * per_row + x / unit).copied().unwrap_or(0);
            if v >> (x % unit) & 1 == 1 {
                rgba[(y * w + x) * 4..(y * w + x) * 4 + 3].copy_from_slice(&[0, 0, 0]);
            }
        }
    }
    Ok(Xbm { width: w as u32, height: h as u32, x10, hotspot, rgba, warnings })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn x11_and_x10_bitmaps() {
        let x11 = b"/* made by hand */\n#define dot_width 10\n#define dot_height 2\n#define dot_x_hot 1\n#define dot_y_hot 0\nstatic unsigned char dot_bits[] = {\n   0x01, 0x02, 0xff, 0x03 };\n";
        assert!(is_xbm(x11));
        let b = decode(x11, u64::MAX).unwrap();
        assert_eq!((b.width, b.height, b.x10, b.hotspot), (10, 2, false, Some((1, 0))));
        let black = |x: usize, y: usize| b.rgba[(y * 10 + x) * 4] == 0;
        assert!(black(0, 0) && !black(1, 0) && black(9, 0) && !black(8, 0));
        assert!((0..10).all(|x| black(x, 1)));
        let x10 = b"#define w_width 17\n#define w_height 1\nstatic short w_bits[] = {0x8001, 0x0001};";
        let b = decode(x10, u64::MAX).unwrap();
        assert!(b.x10);
        let black = |x: usize| b.rgba[x * 4] == 0;
        assert!(black(0) && black(15) && black(16) && !black(1));
    }

    #[test]
    fn damaged_files_are_bounded() {
        let text = b"#define a_width 8\n#define a_height 4\nstatic char a_bits[] = { 0x01, 0x02 ";
        let b = decode(text, u64::MAX).unwrap();
        assert_eq!(b.warnings.len(), 1);
        assert!(decode(b"#define a_width 99999\n#define a_height 99999\n{}", 1 << 26).is_err());
        assert!(!is_xbm(b"#include <stdio.h>\n"));
        assert!(!is_xbm(b"#define VERSION 3\n"));
        assert!(!is_xbm(b"#define BOX_width 3\nint f(void);\n"));
    }
}
