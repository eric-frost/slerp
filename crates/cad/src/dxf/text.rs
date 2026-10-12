//! Laying out TEXT and MTEXT as strokes of the built-in font.  The result is
//! in the text's own plane, with the entity's insertion point and rotation
//! applied, for the caller to move into the world.

use super::entities::{MText, Text};
use super::font;
use super::tables::Style;
use glam::{DVec2, DVec3};

/// The depth of a descender below the baseline, as a fraction of the text
/// height, for bottom-aligned text.
const DESCENT: f64 = 0.33;
/// MTEXT puts lines this many text heights apart at line spacing 1.
const LINE_PITCH: f64 = 5.0 / 3.0;

/// Replaces the `%%` codes TEXT uses for special characters: `%%c`
/// (diameter), `%%d` (degrees), `%%p` (plus-minus), `%%%` (a percent sign)
/// and `%%nnn` (the character with that code).  The underline, overline
/// and strike-through switches are dropped.
pub fn percent_codes(s: &str) -> String {
    if !s.contains("%%") {
        return s.to_string();
    }
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '%' && chars.get(i + 1) == Some(&'%') {
            match chars.get(i + 2).map(|c| c.to_ascii_lowercase()) {
                Some('c') => out.push('\u{2300}'),
                Some('d') => out.push('\u{B0}'),
                Some('p') => out.push('\u{B1}'),
                Some('%') => out.push('%'),
                Some('u') | Some('o') | Some('k') => {}
                Some(d) if d.is_ascii_digit() => {
                    let digits: String = chars[i + 2..].iter().take(3).take_while(|c| c.is_ascii_digit()).collect();
                    let code: u32 = digits.parse().unwrap_or(0);
                    if let Some(c) = char::from_u32(code).filter(|c| !c.is_control()) {
                        out.push(c);
                    }
                    i += digits.len() - 1;
                }
                _ => {
                    i += 2;
                    continue;
                }
            }
            i += 3;
        } else {
            out.push(chars[i]);
            i += 1;
        }
    }
    out
}

/// How wide `s` is at text height 1.
pub fn width_of(s: &str) -> f64 {
    s.chars().map(font::advance).sum()
}

/// Draws `s` with its baseline starting at the origin, height `h`, stretched
/// by `wf` and slanted by `oblique` (radians), calling `put` with each
/// stroke in that frame.
fn draw_line(s: &str, h: f64, wf: f64, oblique: f64, mut put: impl FnMut(Vec<DVec2>)) {
    let slant = oblique.tan().clamp(-4.0, 4.0);
    let mut cursor = 0.0;
    for c in s.chars() {
        let x0 = cursor;
        let adv = font::glyph(c, |stroke| {
            put(stroke.iter().map(|p| DVec2::new((x0 + p.x + p.y * slant) * h * wf, p.y * h)).collect());
        });
        cursor += adv;
    }
}

/// Turns a local frame (x along the baseline, y up) into the plane: mirrors,
/// rotates and moves to `at`.
fn place(strokes: Vec<Vec<DVec2>>, at: DVec2, rotation: f64, mirror_x: bool, mirror_y: bool) -> Vec<Vec<DVec2>> {
    let (s, c) = rotation.sin_cos();
    strokes
        .into_iter()
        .map(|st| {
            st.into_iter()
                .map(|p| {
                    let p = DVec2::new(if mirror_x { -p.x } else { p.x }, if mirror_y { -p.y } else { p.y });
                    at + DVec2::new(p.x * c - p.y * s, p.x * s + p.y * c)
                })
                .collect()
        })
        .collect()
}

/// The strokes of a TEXT, ATTRIB or constant ATTDEF, in the entity's plane.
pub fn text_strokes(t: &Text, style: Style) -> Vec<Vec<DVec2>> {
    let s = percent_codes(&t.text);
    if s.trim().is_empty() {
        return Vec::new();
    }
    let mut h = if t.height > 0.0 { t.height } else { style.height };
    if h <= 0.0 || !h.is_finite() {
        return Vec::new();
    }
    let mut wf = if t.x_scale > 0.0 { t.x_scale } else { 1.0 } * if style.width > 0.0 { style.width } else { 1.0 };
    let oblique = t.oblique + style.oblique;
    let natural = width_of(&s) * h * wf;
    let mut rotation = t.rotation;
    let (halign, valign) = (t.halign, t.valign);
    let mut at = DVec2::new(t.p.x, t.p.y);
    let mut dx = 0.0;
    if halign == 0 && valign == 0 {
        // Left-aligned on the baseline: the insertion point is the first.
    } else if matches!(halign, 3 | 5) {
        // Aligned and fitted text run between the two points.
        let v = DVec2::new(t.p2.x - t.p.x, t.p2.y - t.p.y);
        if v.length() > 0.0 && natural > 0.0 {
            rotation = v.to_angle();
            let k = v.length() / natural;
            wf *= k;
            if halign == 3 {
                h *= k;
            }
        }
    } else {
        at = DVec2::new(t.p2.x, t.p2.y);
        dx = match halign {
            1 | 4 => -natural / 2.0,
            2 => -natural,
            _ => 0.0,
        };
    }
    // Where the baseline sits relative to the anchor.
    let valign = if halign == 4 { 2 } else { valign };
    let dy = match valign {
        1 => DESCENT * h,
        2 => -0.5 * h,
        3 => -h,
        _ => 0.0,
    };
    let mut strokes = Vec::new();
    draw_line(&s, h, wf, oblique, |st| strokes.push(st.into_iter().map(|p| p + DVec2::new(dx, dy)).collect()));
    place(strokes, at, rotation, t.gen_flags & 2 != 0, t.gen_flags & 4 != 0)
}

//region MTEXT

/// The text of an MTEXT without its formatting codes, one string per line.
/// Handled: `\P` (new paragraph), `\~` (non-breaking space), `\\`, `\{`,
/// `\}`, stacked fractions (`\Sa/b;` becomes "a/b"), and codes that take
/// arguments up to a semicolon (fonts, heights, colours, widths, tracking,
/// slant, alignment, paragraph settings), which are dropped.
pub fn mtext_lines(raw: &str) -> Vec<String> {
    let text = percent_codes(raw);
    let chars: Vec<char> = text.chars().collect();
    let mut lines = vec![String::new()];
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        match c {
            '\\' => {
                let Some(&code) = chars.get(i + 1) else { break };
                i += 2;
                match code {
                    'P' | 'p' if code == 'P' => lines.push(String::new()),
                    'N' => lines.push(String::new()),
                    '~' => lines.last_mut().unwrap().push(' '),
                    '\\' | '{' | '}' => lines.last_mut().unwrap().push(code),
                    'L' | 'l' | 'O' | 'o' | 'K' | 'k' => {}
                    'S' => {
                        let end = chars[i..].iter().position(|&c| c == ';').map_or(chars.len(), |p| i + p);
                        let stack: String = chars[i..end].iter().collect();
                        let text = stack.replace(['^', '#'], "/").replace("\\/", "/");
                        lines.last_mut().unwrap().push_str(&text);
                        i = (end + 1).min(chars.len());
                    }
                    'U' if chars.get(i) == Some(&'+') => {
                        // \U+XXXX when the decoder left one.
                        i += 1;
                    }
                    'f' | 'F' | 'H' | 'W' | 'C' | 'c' | 'T' | 'Q' | 'A' | 'p' | 'h' | 'M' | 'X' => {
                        // Arguments up to the semicolon.
                        let end = chars[i..].iter().position(|&c| c == ';').map_or(chars.len(), |p| i + p);
                        i = (end + 1).min(chars.len());
                    }
                    other => lines.last_mut().unwrap().push(other),
                }
            }
            '{' | '}' => i += 1,
            '\n' => {
                lines.push(String::new());
                i += 1;
            }
            '\r' => i += 1,
            _ => {
                lines.last_mut().unwrap().push(c);
                i += 1;
            }
        }
    }
    lines
}

/// Breaks a line at spaces so that no piece is wider than `width` (text of
/// height `h` stretched by `wf`), keeping words whole.
fn wrap(line: &str, width: f64, h: f64, wf: f64) -> Vec<String> {
    if width <= 0.0 || width_of(line) * h * wf <= width {
        return vec![line.to_string()];
    }
    let mut out = Vec::new();
    let mut current = String::new();
    for word in line.split(' ') {
        let candidate = if current.is_empty() { word.to_string() } else { format!("{current} {word}") };
        if !current.is_empty() && width_of(&candidate) * h * wf > width {
            out.push(std::mem::take(&mut current));
            current = word.to_string();
        } else {
            current = candidate;
        }
    }
    out.push(current);
    out
}

/// The strokes of an MTEXT in its plane (x along its direction, y up),
/// placed at its insertion point and rotated.
pub fn mtext_strokes(t: &MText, style: Style) -> Vec<Vec<DVec2>> {
    let mut h = if t.height > 0.0 { t.height } else { style.height };
    if h <= 0.0 || !h.is_finite() {
        h = 1.0;
    }
    let wf = if style.width > 0.0 { style.width } else { 1.0 };
    let mut lines: Vec<String> = Vec::new();
    for l in mtext_lines(&t.text) {
        lines.extend(wrap(&l, t.width, h, wf));
    }
    if lines.iter().all(|l| l.trim().is_empty()) {
        return Vec::new();
    }
    // Very long texts are shown in part.
    lines.truncate(2000);
    let widths: Vec<f64> = lines.iter().map(|l| width_of(l) * h * wf).collect();
    let box_w = if t.width > 0.0 { t.width } else { widths.iter().copied().fold(0.0, f64::max) };
    let pitch = LINE_PITCH * h * if t.spacing > 0.0 { t.spacing } else { 1.0 };
    let box_h = h + pitch * (lines.len() - 1) as f64;
    // Attachment points 1 to 9: rows top, middle, bottom; columns left,
    // centre, right.
    let attach = t.attach.clamp(1, 9) - 1;
    let (row, col) = (attach / 3, attach % 3);
    let x0 = match col {
        0 => 0.0,
        1 => -box_w / 2.0,
        _ => -box_w,
    };
    let top = match row {
        0 => 0.0,
        1 => box_h / 2.0,
        _ => box_h,
    };
    let mut strokes = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        // Text is aligned within the box as it is attached to it.
        let lx = x0
            + match col {
                0 => 0.0,
                1 => (box_w - widths[i]) / 2.0,
                _ => box_w - widths[i],
            };
        let base = top - h - pitch * i as f64;
        draw_line(line, h, wf, style.oblique, |st| {
            strokes.push(st.into_iter().map(|p| p + DVec2::new(lx, base)).collect());
        });
    }
    let rotation = match t.x_dir {
        Some(d) => d.y.atan2(d.x),
        None => t.rotation.to_radians(),
    };
    place(strokes, DVec2::new(t.p.x, t.p.y), rotation, false, false)
}

//endregion

/// The corner points of a rectangle outline, for placeholders.
#[allow(dead_code)]
pub fn rect(min: DVec3, max: DVec3) -> Vec<DVec2> {
    vec![min.truncate(), DVec2::new(max.x, min.y), max.truncate(), DVec2::new(min.x, max.y), min.truncate()]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bbox(strokes: &[Vec<DVec2>]) -> (DVec2, DVec2) {
        let mut min = DVec2::splat(f64::MAX);
        let mut max = DVec2::splat(f64::MIN);
        for p in strokes.iter().flatten() {
            min = min.min(*p);
            max = max.max(*p);
        }
        (min, max)
    }

    #[test]
    fn percent_codes_become_characters() {
        assert_eq!(percent_codes("%%c25 %%d %%p0.1"), "\u{2300}25 \u{B0} \u{B1}0.1");
        assert_eq!(percent_codes("100%%%"), "100%");
        assert_eq!(percent_codes("%%65bc"), "Abc");
        assert_eq!(percent_codes("%%uunder%%u"), "under");
        assert_eq!(percent_codes("plain"), "plain");
        assert_eq!(percent_codes("end%%"), "end");
    }

    #[test]
    fn mtext_codes_are_stripped() {
        assert_eq!(mtext_lines("one\\Ptwo"), ["one", "two"]);
        assert_eq!(mtext_lines("{\\fArial|b1|i0;bold} text"), ["bold text"]);
        assert_eq!(mtext_lines("\\H2.5x;big\\C1;red"), ["bigred"]);
        assert_eq!(mtext_lines("\\S1/2;"), ["1/2"]);
        assert_eq!(mtext_lines("\\S1^ 2;"), ["1/ 2"]);
        assert_eq!(mtext_lines("a\\~b\\\\c\\{d\\}"), ["a b\\c{d}"]);
        assert_eq!(mtext_lines("\\Lunder\\l \\Oover\\o"), ["under over"]);
        assert_eq!(mtext_lines("\\pxi-3,l3,t3;item"), ["item"]);
        // Unterminated codes don't run off the end.
        assert_eq!(mtext_lines("x\\f"), ["x"]);
        assert_eq!(mtext_lines("x\\"), ["x"]);
        assert_eq!(mtext_lines("x\\Sabc"), ["xabc"]);
        assert_eq!(mtext_lines("").len(), 1);
    }

    #[test]
    fn text_has_the_right_height_and_alignment() {
        let mut t = Text {
            text: "HELLO".into(),
            height: 2.0,
            x_scale: 1.0,
            p: DVec3::new(10.0, 20.0, 0.0),
            ..Default::default()
        };
        let (min, max) = bbox(&text_strokes(&t, Style::default()));
        assert!((min.x - 10.0).abs() < 0.5, "{min:?}");
        assert!((min.y - 20.0).abs() < 0.1 && (max.y - 22.0).abs() < 0.1, "{min:?} {max:?}");
        // Centred on a second point.
        t.halign = 1;
        t.p2 = DVec3::new(50.0, 50.0, 0.0);
        let (min, max) = bbox(&text_strokes(&t, Style::default()));
        assert!(((min.x + max.x) / 2.0 - 50.0).abs() < 0.6, "{min:?} {max:?}");
        // Middle-aligned vertically.
        t.valign = 2;
        let (min, max) = bbox(&text_strokes(&t, Style::default()));
        assert!(((min.y + max.y) / 2.0 - 50.0).abs() < 0.3, "{min:?} {max:?}");
        // Rotated by 90 degrees the text runs up the page.
        t.halign = 0;
        t.valign = 0;
        t.rotation = std::f64::consts::FRAC_PI_2;
        let (min, max) = bbox(&text_strokes(&t, Style::default()));
        assert!(max.y - min.y > max.x - min.x);
    }

    #[test]
    fn aligned_and_fitted_text_span_their_points() {
        let t = Text {
            text: "ABCDEF".into(),
            height: 1.0,
            x_scale: 1.0,
            p: DVec3::new(0.0, 0.0, 0.0),
            p2: DVec3::new(30.0, 0.0, 0.0),
            halign: 5,
            ..Default::default()
        };
        let (min, max) = bbox(&text_strokes(&t, Style::default()));
        assert!(min.x >= -0.01 && max.x <= 30.01 && max.x > 25.0, "{min:?} {max:?}");
        assert!((max.y - 1.0).abs() < 0.1, "fit keeps the height: {max:?}");
        let aligned = Text { halign: 3, ..t };
        let (_, max) = bbox(&text_strokes(&aligned, Style::default()));
        assert!(max.y > 2.0, "aligned scales the height too: {max:?}");
    }

    #[test]
    fn degenerate_text_draws_nothing() {
        let t = Text { text: "x".into(), height: 0.0, ..Default::default() };
        assert!(text_strokes(&t, Style::default()).is_empty());
        let t = Text { text: "   ".into(), height: 1.0, ..Default::default() };
        assert!(text_strokes(&t, Style::default()).is_empty());
        let t = Text { text: "x".into(), height: f64::NAN, ..Default::default() };
        assert!(text_strokes(&t, Style::default()).is_empty());
    }

    #[test]
    fn mtext_wraps_and_attaches() {
        let t = MText {
            text: "alpha beta gamma delta epsilon".into(),
            height: 1.0,
            width: 8.0,
            attach: 1,
            spacing: 1.0,
            ..Default::default()
        };
        let (min, max) = bbox(&mtext_strokes(&t, Style::default()));
        assert!(max.x <= 8.5, "wrapped within its width: {max:?}");
        assert!(max.y <= 0.01 && min.y < -3.0, "hangs down from the insertion point: {min:?} {max:?}");
        // Bottom-right attachment puts the box up and to the left.
        let br = MText { attach: 9, ..t.clone() };
        let (min, max) = bbox(&mtext_strokes(&br, Style::default()));
        assert!(max.x <= 0.01 && min.y >= -0.4, "{min:?} {max:?}");
        // The x direction rotates it.
        let rot = MText { x_dir: Some(DVec3::Y), attach: 1, ..t };
        let (min, max) = bbox(&mtext_strokes(&rot, Style::default()));
        assert!(max.y > 5.0 && max.x > 3.0, "{min:?} {max:?}");
    }

    #[test]
    fn mtext_cannot_blow_up() {
        let t = MText { text: "word ".repeat(100_000), height: 1.0, width: 3.0, ..Default::default() };
        let strokes = mtext_strokes(&t, Style::default());
        assert!(strokes.len() < 100_000, "{}", strokes.len());
    }
}
