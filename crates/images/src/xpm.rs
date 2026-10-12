//! XPM, the X PixMap: a C array of strings holding the size, a colour table
//! keyed by one or more characters per pixel, and the rows of pixel
//! characters.  XPM3 (the C form) and XPM2 (the same strings as plain lines
//! after `! XPM2`) are read.  Colours are `#rgb` hexadecimal in 1 to 4 digits
//! per channel, X11 colour names, or `None` for transparency.

use crate::x11_colors::COLORS;
use std::collections::HashMap;

pub struct Xpm {
    pub width: u32,
    pub height: u32,
    pub colors: usize,
    pub chars_per_pixel: usize,
    pub hotspot: Option<(u32, u32)>,
    pub rgba: Vec<u8>,
    pub warnings: Vec<String>,
}

const MAX_CHARS_PER_PIXEL: usize = 8;
const MAX_COLORS: usize = 1 << 20;

pub fn is_xpm(head: &[u8]) -> bool {
    let text = String::from_utf8_lossy(&head[..head.len().min(256)]);
    let t = text.trim_start_matches('\u{feff}').trim_start();
    t.starts_with("/* XPM */") || t.starts_with("! XPM2")
}

pub fn decode(d: &[u8], max_pixels: u64) -> Result<Xpm, String> {
    let text = String::from_utf8_lossy(d);
    let xpm2 = text.trim_start().starts_with("! XPM2");
    let mut lines: Box<dyn Iterator<Item = String>> = if xpm2 {
        Box::new(text.lines().skip_while(|l| !l.trim_start().starts_with("! XPM2")).skip(1).map(str::to_string))
    } else {
        Box::new(strings(&text).into_iter())
    };
    let mut warnings = Vec::new();
    // 1. "width height ncolors cpp [x_hot y_hot] [XPMEXT]".
    let values = lines.next().ok_or("the file has no XPM values line")?;
    let nums: Vec<u64> = values.split_whitespace().map_while(|w| w.parse().ok()).collect();
    let [w, h, ncolors, cpp] = nums.get(..4).ok_or("the XPM values line is incomplete")?.try_into().unwrap_or([0; 4]);
    if w == 0 || h == 0 || w.saturating_mul(h) > max_pixels {
        return Err(format!("a picture of {w} × {h} pixels is not allowed"));
    }
    if cpp == 0 || cpp as usize > MAX_CHARS_PER_PIXEL || ncolors == 0 || ncolors as usize > MAX_COLORS {
        return Err(format!("{ncolors} colours of {cpp} characters is not a plausible XPM"));
    }
    let hotspot = (nums.len() >= 6).then(|| (nums[4] as u32, nums[5] as u32));
    let (w, h, cpp) = (w as usize, h as usize, cpp as usize);
    // 2. The colour table.
    let mut table: HashMap<Vec<u8>, [u8; 4]> = HashMap::new();
    let mut unknown = Vec::new();
    for _ in 0..ncolors {
        let Some(line) = lines.next() else {
            warnings.push("the colour table is cut short".into());
            break;
        };
        let bytes = line.as_bytes();
        if bytes.len() < cpp {
            continue;
        }
        let key = bytes[..cpp].to_vec();
        let rest = String::from_utf8_lossy(&bytes[cpp..]).into_owned();
        let value = color_of(&rest);
        let rgba = match value {
            Some(Color::Rgba(c)) => c,
            Some(Color::Unknown(name)) => {
                unknown.push(name);
                [0, 0, 0, 255]
            }
            None => [0, 0, 0, 255],
        };
        table.insert(key, rgba);
    }
    if !unknown.is_empty() {
        unknown.truncate(5);
        warnings.push(format!("unknown colour names, shown black: {}", unknown.join(", ")));
    }
    // 3. The rows.
    let mut rgba = vec![0u8; w * h * 4];
    let mut short = 0;
    for y in 0..h {
        let Some(line) = lines.next() else {
            short = h - y;
            break;
        };
        let bytes = line.as_bytes();
        for x in 0..w {
            let Some(key) = bytes.get(x * cpp..x * cpp + cpp) else { break };
            if let Some(c) = table.get(key) {
                rgba[(y * w + x) * 4..(y * w + x) * 4 + 4].copy_from_slice(c);
            }
        }
    }
    if short > 0 {
        warnings.push(format!("the file is cut short: {short} rows are missing"));
    }
    Ok(Xpm { width: w as u32, height: h as u32, colors: table.len(), chars_per_pixel: cpp, hotspot, rgba, warnings })
}

/// The C string literals of an XPM3 file, in order, outside comments.
fn strings(text: &str) -> Vec<String> {
    let b = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'/' if b.get(i + 1) == Some(&b'*') => {
                i = text[i + 2..].find("*/").map_or(b.len(), |e| i + 2 + e + 2);
            }
            b'/' if b.get(i + 1) == Some(&b'/') => {
                i = text[i..].find('\n').map_or(b.len(), |e| i + e);
            }
            b'"' => {
                let mut s = Vec::new();
                i += 1;
                while i < b.len() && b[i] != b'"' {
                    if b[i] == b'\\' && i + 1 < b.len() {
                        i += 1;
                    }
                    s.push(b[i]);
                    i += 1;
                }
                i += 1;
                out.push(String::from_utf8_lossy(&s).into_owned());
            }
            _ => i += 1,
        }
    }
    out
}

enum Color {
    Rgba([u8; 4]),
    Unknown(String),
}

/// The colour of a colour-table entry: "c" (colour) wins over "g" (grey),
/// "g4" (four-level grey) and "m" (monochrome); "s" names a symbol.
fn color_of(rest: &str) -> Option<Color> {
    let words: Vec<&str> = rest.split_whitespace().collect();
    let mut found: Vec<(&str, String)> = Vec::new();
    let mut i = 0;
    while i < words.len() {
        let key = words[i];
        if matches!(key, "c" | "m" | "g" | "g4" | "s") {
            let mut j = i + 1;
            while j < words.len() && !matches!(words[j], "c" | "m" | "g" | "g4" | "s") {
                j += 1;
            }
            found.push((key, words[i + 1..j].join(" ")));
            i = j;
        } else {
            i += 1;
        }
    }
    let value = ["c", "g", "g4", "m"].iter().find_map(|k| found.iter().find(|(f, v)| f == k && !v.is_empty()))?;
    Some(parse_color(&value.1))
}

fn parse_color(v: &str) -> Color {
    let v = v.trim();
    if v.eq_ignore_ascii_case("none") || v.eq_ignore_ascii_case("transparent") {
        return Color::Rgba([0, 0, 0, 0]);
    }
    if let Some(hex) = v.strip_prefix('#') {
        let n = hex.len();
        if n % 3 == 0 && (3..=12).contains(&n) && hex.bytes().all(|c| c.is_ascii_hexdigit()) {
            let digits = n / 3;
            let max = (1u32 << (4 * digits)) - 1;
            let ch = |k: usize| {
                let v = u32::from_str_radix(&hex[k * digits..(k + 1) * digits], 16).unwrap_or(0);
                ((v * 255 + max / 2) / max) as u8
            };
            return Color::Rgba([ch(0), ch(1), ch(2), 255]);
        }
    }
    let key: String = v.chars().filter(|c| !c.is_whitespace()).collect::<String>().to_ascii_lowercase();
    match COLORS.binary_search_by(|(name, _)| name.cmp(&key.as_str())) {
        Ok(i) => {
            let [r, g, b] = COLORS[i].1;
            Color::Rgba([r, g, b, 255])
        }
        Err(_) => Color::Unknown(v.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"/* XPM */
static char *test[] = {
/* columns rows colors chars-per-pixel */
"4 3 4 2 1 2",
"   c None",
".. c #FF0000",
"XX c dark slate gray s background",
"oo m white c #00000000ffff",
/* pixels */
"  ..XXoo",
"oo  ..XX",
"XXoo  .\"",
};
"#;

    #[test]
    fn reads_colours_names_and_transparency() {
        assert!(is_xpm(SAMPLE.as_bytes()));
        let x = decode(SAMPLE.as_bytes(), u64::MAX).unwrap();
        assert_eq!((x.width, x.height, x.colors, x.chars_per_pixel, x.hotspot), (4, 3, 4, 2, Some((1, 2))));
        let px = |i: usize| &x.rgba[i * 4..i * 4 + 4];
        assert_eq!(px(0), [0, 0, 0, 0]);
        assert_eq!(px(1), [255, 0, 0, 255]);
        assert_eq!(px(2), [47, 79, 79, 255]);
        assert_eq!(px(3), [0, 0, 255, 255]);
        // An unknown key (`.` followed by a quote) leaves the pixel clear.
        assert_eq!(px(11), [0, 0, 0, 0]);
        assert!(x.warnings.is_empty(), "{:?}", x.warnings);
    }

    #[test]
    fn xpm2_is_plain_lines() {
        let text = "! XPM2\n2 1 2 1\na c #000\nb c grey50\nab\n";
        let x = decode(text.as_bytes(), u64::MAX).unwrap();
        assert_eq!(&x.rgba[..], &[0, 0, 0, 255, 127, 127, 127, 255]);
    }

    #[test]
    fn damaged_files_are_bounded() {
        for cut in 0..SAMPLE.len() {
            let _ = decode(&SAMPLE.as_bytes()[..cut], u64::MAX);
        }
        assert!(decode(b"/* XPM */\n\"100000 100000 1 1\",\n\"a c red\"", 1 << 26).is_err());
        assert!(decode(b"/* XPM */\n\"1 1 9999999 1\"", u64::MAX).is_err());
        assert!(decode(b"/* XPM */\n\"1 1 1 99\"", u64::MAX).is_err());
        let x = decode(b"/* XPM */\n\"2 2 1 1\",\n\"a c chartreuse-ish\",\n\"aa\"", u64::MAX).unwrap();
        assert_eq!(x.warnings.len(), 2);
    }
}
