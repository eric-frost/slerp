//! System fonts for metafile text, found by looking for well-known files:
//! Arial, Times New Roman and Courier New where Windows or Microsoft's fonts
//! are installed, the metrically compatible Liberation fonts, DejaVu, Noto
//! and so on elsewhere.  A metafile names a font by family, and what matters
//! for a thumbnail is serif, sans or monospace, bold and italic, so the name
//! is reduced to those and the first installed candidate wins.  There is no
//! directory scan and no font database: a few dozen `stat` calls, and one
//! file read when text is actually drawn.  When nothing is found, text falls
//! back to the built-in stroke font.

use crate::state::Font;
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use tiny_skia::{Path, PathBuilder};

/// The largest font file read (a CJK collection is about 20 MB).
const MAX_FONT_BYTES: u64 = 48 << 20;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum Class {
    Sans,
    Serif,
    Mono,
    /// Office's default fonts have free look-alikes with the same metrics
    /// (Carlito, Caladea), which keep laid-out text the right width.
    Calibri,
    Cambria,
}

/// Which family class a face name means.
fn class_of(face: &str, pitch_family: u8) -> Class {
    let f = face.to_ascii_lowercase();
    let has = |words: &[&str]| words.iter().any(|w| f.contains(w));
    if has(&["calibri", "carlito"]) {
        Class::Calibri
    } else if has(&["cambria", "caladea"]) {
        Class::Cambria
    } else if has(&["courier", "mono", "consolas", "lucida console", "terminal", "fixed", "typewriter", "andale"]) {
        Class::Mono
    } else if has(&["sans", "arial", "helvetica", "verdana", "tahoma", "calibri", "segoe", "gothic", "trebuchet"]) {
        Class::Sans
    } else if has(&[
        "times",
        "serif",
        "georgia",
        "garamond",
        "palatino",
        "cambria",
        "book antiqua",
        "century",
        "roman",
        "minion",
        "bookman",
    ]) {
        Class::Serif
    } else if pitch_family & 3 == 1 {
        // FIXED_PITCH in the pitch bits means monospace.
        Class::Mono
    } else if pitch_family & 0xF0 == 0x10 {
        // FF_ROMAN in the family bits means serif.
        Class::Serif
    } else {
        Class::Sans
    }
}

/// Candidate files for each class and style (regular, bold, italic, bold
/// italic), best first: Microsoft's, then Liberation, DejaVu, Noto, Free.
fn candidates(class: Class, style: usize) -> Vec<PathBuf> {
    // The look-alikes for Office's fonts come first, then the plain class.
    let (own, class) = match class {
        Class::Calibri => (Some(("Calibri", "calibri", "Carlito", ["", "b", "i", "z"])), Class::Sans),
        Class::Cambria => (Some(("Cambria", "cambria", "Caladea", ["", "b", "i", "z"])), Class::Serif),
        other => (None, other),
    };
    let mut out: Vec<PathBuf> = Vec::new();
    if let Some((ms, win, free, suffixes)) = own {
        let style_names = ["Regular", "Bold", "Italic", "BoldItalic"];
        out.push(PathBuf::from(format!("C:\\Windows\\Fonts\\{win}{}.ttf", suffixes[style])));
        out.push(PathBuf::from(format!(
            "/usr/share/fonts/truetype/msttcorefonts/{ms}{}.ttf",
            ["", "_Bold", "_Italic", "_Bold_Italic"][style]
        )));
        for dir in [
            "/usr/share/fonts/truetype/crosextra",
            "/usr/share/fonts/crosextra",
            "/usr/share/fonts/truetype/carlito",
            "/usr/share/fonts/truetype/caladea",
        ] {
            out.push(PathBuf::from(dir).join(format!("{free}-{}.ttf", style_names[style])));
        }
    }
    let (ms, lib, dejavu, noto, free): (&[&str], &str, &str, &str, &str) = match class {
        Class::Sans | Class::Calibri => (&["Arial", "arial"], "LiberationSans", "DejaVuSans", "NotoSans", "FreeSans"),
        Class::Serif | Class::Cambria => {
            (&["Times_New_Roman", "times"], "LiberationSerif", "DejaVuSerif", "NotoSerif", "FreeSerif")
        }
        Class::Mono => (&["Courier_New", "cour"], "LiberationMono", "DejaVuSansMono", "NotoSansMono", "FreeMono"),
    };
    let suffix = |names: [&str; 4]| names[style].to_string();
    let mut push = |dir: &str, name: String| out.push(PathBuf::from(dir).join(name));
    // Microsoft's core fonts: Ubuntu's installer, Windows, macOS.
    let ms_linux = format!("{}{}.ttf", ms[0], suffix(["", "_Bold", "_Italic", "_Bold_Italic"]));
    push("/usr/share/fonts/truetype/msttcorefonts", ms_linux.clone());
    push("/usr/share/fonts/truetype/msttcorefonts", ms_linux.to_ascii_lowercase());
    let win = format!("{}{}.ttf", ms[1], suffix(["", "bd", "i", "bi"]));
    push("C:\\Windows\\Fonts", win);
    let mac_name = ms[0].replace('_', " ");
    push(
        "/System/Library/Fonts/Supplemental",
        format!("{mac_name}{}.ttf", suffix(["", " Bold", " Italic", " Bold Italic"])),
    );
    // Liberation (metric-compatible with the above).
    let l = format!("{lib}-{}.ttf", suffix(["Regular", "Bold", "Italic", "BoldItalic"]));
    for dir in [
        "/usr/share/fonts/truetype/liberation",
        "/usr/share/fonts/truetype/liberation2",
        "/usr/share/fonts/liberation",
        "/usr/share/fonts/liberation-fonts",
        "/usr/share/fonts/TTF",
    ] {
        push(dir, l.clone());
    }
    let d = format!("{dejavu}{}.ttf", suffix(["", "-Bold", "-Oblique", "-BoldOblique"]));
    let d_serif = format!("{dejavu}{}.ttf", suffix(["", "-Bold", "-Italic", "-BoldItalic"]));
    for dir in ["/usr/share/fonts/truetype/dejavu", "/usr/share/fonts/dejavu", "/usr/share/fonts/TTF"] {
        push(dir, d.clone());
        push(dir, d_serif.clone());
    }
    let n = format!("{noto}-{}.ttf", suffix(["Regular", "Bold", "Italic", "BoldItalic"]));
    for dir in ["/usr/share/fonts/truetype/noto", "/usr/share/fonts/noto", "/usr/share/fonts/opentype/noto"] {
        push(dir, n.clone());
    }
    let f = format!("{free}{}.ttf", suffix(["", "Bold", "Oblique", "BoldOblique"]));
    push("/usr/share/fonts/truetype/freefont", f);
    out
}

/// Fonts that cover many scripts, tried for characters the first choice
/// lacks.
fn fallbacks() -> Vec<PathBuf> {
    [
        "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
        "/usr/share/fonts/dejavu/DejaVuSans.ttf",
        "/usr/share/fonts/TTF/DejaVuSans.ttf",
        "/usr/share/fonts/truetype/noto/NotoSans-Regular.ttf",
        "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
        "/usr/share/fonts/truetype/droid/DroidSansFallbackFull.ttf",
        "/usr/share/fonts/truetype/wqy/wqy-microhei.ttc",
        "/usr/share/fonts/opentype/ipafont-gothic/ipag.ttf",
        "/usr/share/fonts/truetype/freefont/FreeSans.ttf",
        "/usr/share/fonts/opentype/unifont/unifont.otf",
        "C:\\Windows\\Fonts\\arialuni.ttf",
        "C:\\Windows\\Fonts\\msyh.ttc",
        "C:\\Windows\\Fonts\\malgun.ttf",
        "/System/Library/Fonts/Supplemental/Arial Unicode.ttf",
        "/System/Library/Fonts/PingFang.ttc",
    ]
    .iter()
    .map(PathBuf::from)
    .collect()
}

struct Loaded {
    id: usize,
    data: Vec<u8>,
}

/// A font chosen for one `LOGFONT`: the face itself and fallbacks.
pub struct Resolved {
    chain: Vec<Rc<Loaded>>,
    /// True when the face really is bold, so no thickening is needed.
    pub bold_real: bool,
    /// True when the face really is italic, so no slant is added.
    pub italic_real: bool,
}

type Glyph = Option<(Option<Path>, f64)>;

/// A font file found for a class and style, and whether it is really bold
/// and really italic.
type Choice = (Rc<Loaded>, bool, bool);

#[derive(Default)]
pub struct SystemFonts {
    files: HashMap<PathBuf, Option<Rc<Loaded>>>,
    chosen: HashMap<(Class, usize), Option<Choice>>,
    /// The fallback fonts opened so far, and how far down `fallbacks()` the
    /// search has gone: files are opened only for characters that need them.
    fallback_open: Vec<Rc<Loaded>>,
    fallback_next: usize,
    /// Characters no font has, so a second lookup costs nothing.
    missing: std::collections::HashSet<char>,
    glyphs: HashMap<(usize, char), Rc<Glyph>>,
    next_id: usize,
}

impl SystemFonts {
    fn load(&mut self, path: &PathBuf) -> Option<Rc<Loaded>> {
        if let Some(f) = self.files.get(path) {
            return f.clone();
        }
        let loaded = (|| {
            let meta = std::fs::metadata(path).ok()?;
            if !meta.is_file() || meta.len() > MAX_FONT_BYTES {
                return None;
            }
            let data = std::fs::read(path).ok()?;
            ttf_parser::Face::parse(&data, 0).ok()?;
            Some(data)
        })();
        let loaded = loaded.map(|data| {
            self.next_id += 1;
            Rc::new(Loaded { id: self.next_id, data })
        });
        self.files.insert(path.clone(), loaded.clone());
        loaded
    }

    /// The font for a `LOGFONT`, or `None` when no system font was found.
    pub fn resolve(&mut self, font: &Font) -> Option<Resolved> {
        // For testing the built-in font on a machine that has others.
        if std::env::var_os("SLERP_METAFILE_NO_SYSTEM_FONTS").is_some() {
            return None;
        }
        let class = class_of(&font.face, font.pitch_family);
        let bold = font.weight >= 600;
        let italic = font.italic;
        // Prefer the exact style, then bold or italic alone, then regular.
        let wanted = [(bold, italic), (bold, false), (false, italic), (false, false)];
        let mut found: Option<Choice> = None;
        for (b, i) in wanted {
            let style = usize::from(b) + 2 * usize::from(i);
            let key = (class, style);
            let entry = match self.chosen.get(&key) {
                Some(e) => e.clone(),
                None => {
                    let e = candidates(class, style).iter().find_map(|p| self.load(p)).map(|l| (l, b, i));
                    self.chosen.insert(key, e.clone());
                    e
                }
            };
            if entry.is_some() {
                found = entry;
                break;
            }
        }
        // Another class is better than the stroke font.
        if found.is_none() && !matches!(class, Class::Sans | Class::Calibri) {
            return self.resolve(&Font { face: "Arial".into(), pitch_family: 0, ..font.clone() });
        }
        let (primary, b, i) = found?;
        Some(Resolved { chain: vec![primary], bold_real: b || !bold, italic_real: i || !italic })
    }

    /// The outline of `c` in em units (y up) and its advance in ems, from the
    /// first font in the chain that has the character.  The outline is `None`
    /// for characters that draw nothing, such as spaces.
    pub fn glyph(&mut self, r: &Resolved, c: char) -> Glyph {
        for font in &r.chain {
            if let Some(g) = self.lookup(font, c) {
                return Some(g);
            }
        }
        if self.missing.contains(&c) {
            return None;
        }
        // The fallbacks already open, then the next ones in the list.
        let mut at = 0;
        loop {
            while at < self.fallback_open.len() {
                let font = self.fallback_open[at].clone();
                at += 1;
                if let Some(g) = self.lookup(&font, c) {
                    return Some(g);
                }
            }
            let list = fallbacks();
            let Some(path) = list.get(self.fallback_next) else { break };
            self.fallback_next += 1;
            if let Some(l) = self.load(path) {
                self.fallback_open.push(l);
            }
        }
        self.missing.insert(c);
        None
    }

    fn lookup(&mut self, font: &Rc<Loaded>, c: char) -> Glyph {
        let key = (font.id, c);
        if let Some(g) = self.glyphs.get(&key) {
            return (**g).clone();
        }
        let g = read_glyph(font, c);
        self.glyphs.insert(key, Rc::new(g.clone()));
        g
    }
}

fn read_glyph(font: &Loaded, c: char) -> Glyph {
    let face = ttf_parser::Face::parse(&font.data, 0).ok()?;
    let id = face.glyph_index(c)?;
    let upem = face.units_per_em() as f64;
    if upem <= 0.0 {
        return None;
    }
    let advance = face.glyph_hor_advance(id).unwrap_or(0) as f64 / upem;
    let mut b = Builder { pb: PathBuilder::new(), scale: (1.0 / upem) as f32 };
    face.outline_glyph(id, &mut b)?;
    Some((b.pb.finish(), advance))
}

struct Builder {
    pb: PathBuilder,
    scale: f32,
}

impl ttf_parser::OutlineBuilder for Builder {
    fn move_to(&mut self, x: f32, y: f32) {
        self.pb.move_to(x * self.scale, y * self.scale);
    }
    fn line_to(&mut self, x: f32, y: f32) {
        self.pb.line_to(x * self.scale, y * self.scale);
    }
    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        let s = self.scale;
        self.pb.quad_to(x1 * s, y1 * s, x * s, y * s);
    }
    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        let s = self.scale;
        self.pb.cubic_to(x1 * s, y1 * s, x2 * s, y2 * s, x * s, y * s);
    }
    fn close(&mut self) {
        self.pb.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn family_classes() {
        assert_eq!(class_of("Courier New", 0), Class::Mono);
        assert_eq!(class_of("Times New Roman", 0), Class::Serif);
        assert_eq!(class_of("Arial", 0), Class::Sans);
        assert_eq!(class_of("Segoe UI", 0), Class::Sans);
        assert_eq!(class_of("DejaVu Sans Mono", 0), Class::Mono);
        assert_eq!(class_of("Unknown Face", 0), Class::Sans);
    }

    #[test]
    fn candidate_paths_cover_styles() {
        let regular = candidates(Class::Sans, 0);
        let bold_italic = candidates(Class::Sans, 3);
        assert!(regular.iter().any(|p| p.ends_with("LiberationSans-Regular.ttf")));
        assert!(bold_italic.iter().any(|p| p.ends_with("LiberationSans-BoldItalic.ttf")));
        assert!(
            candidates(Class::Mono, 1).iter().any(|p| p.ends_with("cour") || p.to_string_lossy().contains("courbd"))
        );
    }
}
