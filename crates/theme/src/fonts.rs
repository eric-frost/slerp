//! Desktop fonts: parsing the font descriptions KDE and GNOME store, finding
//! the font files through fontconfig (on Windows and macOS, in the system's
//! font folder), and loading them into egui.

use std::path::PathBuf;
#[cfg(not(any(windows, target_os = "macos")))]
use std::process::Command;
use std::sync::Arc;

use egui::epaint::text::{FontTweak, HintingTarget, SmoothHinting, VariationCoords};
use egui::{FontData, FontDefinitions, FontFamily};

/// Fonts larger than this are not loaded (CJK collections run to ~20 MB;
/// anything far beyond that is not a UI font).
const MAX_FONT_BYTES: u64 = 64 << 20;

/// A font as the desktop describes it: family, size and style, before it
/// is resolved to a file.
#[derive(Clone, Debug, PartialEq)]
pub struct FontSpec {
    pub family: String,
    /// Size in typographic points.  Qt and GTK both draw a point as
    /// 96 / 72 logical pixels, so 10 pt is 13.33 egui points.
    pub size_pt: f32,
    /// OpenType weight, 100 (thin) to 900 (black); 400 is regular.
    pub weight: u16,
    pub italic: bool,
}

impl FontSpec {
    pub fn new(family: &str, size_pt: f32) -> FontSpec {
        FontSpec { family: family.to_owned(), size_pt, weight: 400, italic: false }
    }

    /// The size in logical pixels at `dpi` (96 unless the user forces
    /// another font DPI).
    pub fn size_px(&self, dpi: f32) -> f32 {
        self.size_pt * dpi / 72.0
    }

    /// Parses a Qt font string as KDE stores it in `kdeglobals`, such as
    /// `Noto Sans,10,-1,5,400,0,0,0,0,0,0,0,0,0,0,1` (Qt 6) or
    /// `Noto Sans,10,-1,5,50,0,0,0,0,0` (Qt 5).  The fields are family,
    /// point size, pixel size, style hint, weight and italic, then flags
    /// this crate doesn't need.  Qt 5 used its own 0 to 99 weight scale; Qt 6
    /// uses OpenType weights, and writes 16 or more fields.
    pub fn parse_qt(text: &str) -> Option<FontSpec> {
        let fields: Vec<&str> = text.split(',').map(str::trim).collect();
        let family = fields.first().filter(|f| !f.is_empty())?.to_string();
        let number = |i: usize| fields.get(i).and_then(|f| f.parse::<f32>().ok()).filter(|v| v.is_finite());
        let point_size = number(1).filter(|v| *v > 0.0);
        let pixel_size = number(2).filter(|v| *v > 0.0);
        // A string with only a pixel size gives points at 96 DPI.
        let size_pt = point_size.or(pixel_size.map(|px| px * 72.0 / 96.0)).unwrap_or(10.0).clamp(4.0, 72.0);
        let qt6 = fields.len() >= 16;
        let weight = match number(4) {
            Some(w) if qt6 => w.clamp(1.0, 1000.0) as u16,
            Some(w) => qt5_weight_to_opentype(w),
            None => 400,
        };
        let italic = number(5).is_some_and(|v| v != 0.0);
        Some(FontSpec { family, size_pt, weight: weight.clamp(100, 900), italic })
    }

    /// Parses a Pango font description as GTK and GNOME store them, such as
    /// `Cantarell 11`, `Noto Sans Bold Italic 10` or `Adwaita Mono 11`.
    pub fn parse_pango(text: &str) -> Option<FontSpec> {
        let mut words: Vec<&str> = text.split_whitespace().collect();
        let mut size_pt = 10.0;
        if let Some(last) = words.last() {
            let (digits, px) = match last.strip_suffix("px") {
                Some(d) => (d, true),
                None => (*last, false),
            };
            if let Ok(size) = digits.parse::<f32>()
                && size.is_finite()
                && size > 0.0
            {
                size_pt = if px { size * 72.0 / 96.0 } else { size };
                words.pop();
            }
        }
        // Style words come after the family; strip them from the end.
        let mut weight = 400;
        let mut italic = false;
        while let Some(word) = words.last() {
            let lower = word.to_ascii_lowercase();
            let style_weight = match lower.as_str() {
                "thin" | "hairline" => Some(100),
                "ultra-light" | "extra-light" | "ultralight" | "extralight" => Some(200),
                "light" => Some(300),
                "book" | "regular" | "normal" => Some(400),
                "medium" => Some(500),
                "semi-bold" | "demi-bold" | "semibold" | "demibold" => Some(600),
                "bold" => Some(700),
                "ultra-bold" | "extra-bold" | "ultrabold" | "extrabold" => Some(800),
                "heavy" | "black" => Some(900),
                _ => None,
            };
            if let Some(w) = style_weight {
                weight = w;
            } else if lower == "italic" || lower == "oblique" {
                italic = true;
            } else if !matches!(lower.as_str(), "condensed" | "expanded" | "semi-condensed" | "small-caps") {
                break;
            }
            words.pop();
        }
        let family = words.join(" ").trim_end_matches(',').to_owned();
        if family.is_empty() {
            return None;
        }
        Some(FontSpec { family, size_pt: size_pt.clamp(4.0, 72.0), weight, italic })
    }
}

/// Qt 5's weights (`QFont::Normal` = 50, `Bold` = 75) mapped to OpenType's,
/// as Qt 6 converts them when reading an old font string.
fn qt5_weight_to_opentype(weight: f32) -> u16 {
    const TABLE: [(f32, f32); 9] = [
        (0.0, 100.0),
        (12.0, 200.0),
        (25.0, 300.0),
        (50.0, 400.0),
        (57.0, 500.0),
        (63.0, 600.0),
        (75.0, 700.0),
        (81.0, 800.0),
        (87.0, 900.0),
    ];
    interpolate(&TABLE, weight).round() as u16
}

/// OpenType weights mapped to fontconfig's scale (FcWeightFromOpenType).
#[cfg_attr(any(windows, target_os = "macos"), allow(dead_code))]
fn opentype_weight_to_fontconfig(weight: u16) -> u32 {
    const TABLE: [(f32, f32); 11] = [
        (100.0, 0.0),
        (200.0, 40.0),
        (300.0, 50.0),
        (350.0, 55.0),
        (380.0, 75.0),
        (400.0, 80.0),
        (500.0, 100.0),
        (600.0, 180.0),
        (700.0, 200.0),
        (800.0, 205.0),
        (900.0, 210.0),
    ];
    interpolate(&TABLE, weight as f32).round() as u32
}

fn interpolate(table: &[(f32, f32)], x: f32) -> f32 {
    let Some(first) = table.first() else { return x };
    if x <= first.0 {
        return first.1;
    }
    for pair in table.windows(2) {
        let ((x0, y0), (x1, y1)) = (pair[0], pair[1]);
        if x <= x1 {
            return y0 + (y1 - y0) * (x - x0) / (x1 - x0);
        }
    }
    table.last().map_or(x, |last| last.1)
}

/// A font file and the face inside it.
#[derive(Clone, Debug, PartialEq)]
pub struct FontFile {
    pub path: PathBuf,
    /// The face index within a collection (`.ttc`), usually 0.
    pub index: u32,
    /// Coordinates on a variable font's axes, such as `wght` 700 for the
    /// bold face of macOS's San Francisco, which ships as one variable font.
    /// Empty for static fonts, which is what fontconfig returns.
    pub variations: Vec<([u8; 4], f32)>,
}

impl FontFile {
    /// A static font file, or the first face of a collection.
    pub fn new(path: impl Into<PathBuf>) -> FontFile {
        FontFile { path: path.into(), index: 0, variations: Vec::new() }
    }
}

/// Finds the file fontconfig would use for `spec`, the same answer Qt and
/// GTK get, including fontconfig's substitutions when the family is not
/// installed.  Uses `fc-match`, which is part of every fontconfig install
/// and answers in about 10 ms.
#[cfg(not(any(windows, target_os = "macos")))]
pub fn resolve(spec: &FontSpec) -> Option<FontFile> {
    let pattern = format!(
        "{}:weight={}:slant={}",
        escape_fontconfig(&spec.family),
        opentype_weight_to_fontconfig(spec.weight),
        if spec.italic { 100 } else { 0 }
    );
    resolve_pattern(&pattern)
}

/// The file `fc-match` gives for a fontconfig pattern, if egui can read it.
#[cfg(not(any(windows, target_os = "macos")))]
fn resolve_pattern(pattern: &str) -> Option<FontFile> {
    let output = Command::new("fc-match").args(["--format=%{file}\n%{index}\n", pattern]).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8(output.stdout).ok()?;
    let mut lines = text.lines();
    let path = PathBuf::from(lines.next()?.trim());
    let index = lines.next().and_then(|i| i.trim().parse().ok()).unwrap_or(0);
    let lower = path.extension()?.to_str()?.to_ascii_lowercase();
    // egui reads TrueType and OpenType; skip bitmap and Type 1 fonts.
    if !matches!(lower.as_str(), "ttf" | "otf" | "ttc" | "otc") || !path.is_file() {
        return None;
    }
    Some(FontFile { path, index, variations: Vec::new() })
}

/// Finds the file for `spec` among the system fonts Windows and macOS
/// apps use, which have no fontconfig to ask.  Families it doesn't know
/// give `None`, and egui's own fonts stand in.
#[cfg(any(windows, target_os = "macos"))]
pub fn resolve(spec: &FontSpec) -> Option<FontFile> {
    #[cfg(windows)]
    let folder = std::env::var_os("WINDIR").map_or_else(|| PathBuf::from(r"C:\Windows"), PathBuf::from).join("Fonts");
    #[cfg(target_os = "macos")]
    let folder = PathBuf::from("/System/Library/Fonts");
    system_font(spec, &folder).filter(|f| f.path.is_file())
}

/// The file a system font family lives in, inside `folder` (Windows'
/// `Fonts` folder or macOS's `/System/Library/Fonts`).  Pure, so tests on
/// any platform can check the tables.
pub fn system_font(spec: &FontSpec, folder: &std::path::Path) -> Option<FontFile> {
    let bold = spec.weight >= 600;
    let file = |name: &str| FontFile::new(folder.join(name));
    let family = spec.family.to_ascii_lowercase();
    match family.as_str() {
        // Windows: Segoe UI and the monospace fonts that ship with it.
        "segoe ui" | "segoe ui variable" => Some(file(match (bold, spec.italic) {
            (false, false) => "segoeui.ttf",
            (true, false) => "segoeuib.ttf",
            (false, true) => "segoeuii.ttf",
            (true, true) => "segoeuiz.ttf",
        })),
        "consolas" => Some(file(match (bold, spec.italic) {
            (false, false) => "consola.ttf",
            (true, false) => "consolab.ttf",
            (false, true) => "consolai.ttf",
            (true, true) => "consolaz.ttf",
        })),
        // macOS: San Francisco, one variable font per style.  Its default
        // instance is the Display design (optical size 28); text below 20 pt
        // uses the Text design at the axis minimum, as Core Text picks it.
        ".applesystemuifont" | "system-ui" | "sf pro" | "sf pro text" | "sf ns" => {
            let mut f = file(if spec.italic { "SFNSItalic.ttf" } else { "SFNS.ttf" });
            f.variations = vec![(*b"wght", f32::from(spec.weight)), (*b"opsz", spec.size_pt.clamp(17.0, 28.0))];
            Some(f)
        }
        // SF Mono's default instance is its Light weight; its YAXS axis
        // follows the weight in Apple's named instances.
        "sf mono" | "ui-monospace" => {
            let mut f = file(if spec.italic { "SFNSMonoItalic.ttf" } else { "SFNSMono.ttf" });
            let weight = f32::from(spec.weight).clamp(300.0, 900.0);
            f.variations = vec![(*b"wght", weight), (*b"YAXS", weight)];
            Some(f)
        }
        "menlo" => Some(FontFile { index: u32::from(bold) + 2 * u32::from(spec.italic), ..file("Menlo.ttc") }),
        _ => None,
    }
}

/// Escapes the characters fontconfig's pattern syntax treats specially.
#[cfg_attr(any(windows, target_os = "macos"), allow(dead_code))]
fn escape_fontconfig(family: &str) -> String {
    let mut out = String::with_capacity(family.len());
    for c in family.chars() {
        if matches!(c, '\\' | '-' | ':' | ',' | '=') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// How the desktop wants glyph outlines snapped to pixels, from
/// fontconfig's hint styles as KDE stores them (`XftHintStyle`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hinting {
    None,
    Slight,
    Full,
}

/// The fonts a desktop theme uses, with their files when found.
#[derive(Clone, Debug, PartialEq)]
pub struct Fonts {
    /// The general UI font (KDE `font`, GNOME `font-name`).
    pub general: FontSpec,
    /// The fixed-width font (KDE `fixed`, GNOME `monospace-font-name`).
    pub fixed: FontSpec,
    /// The smallest readable size (KDE `smallestReadableFont`), for small
    /// print such as status bars.
    pub small: FontSpec,
    /// Logical DPI fonts are drawn at; 96 unless forced.
    pub dpi: f32,
    pub hinting: Hinting,
    pub general_file: Option<FontFile>,
    /// The bold face of the general font, for headings and emphasis.
    pub bold_file: Option<FontFile>,
    pub fixed_file: Option<FontFile>,
}

impl Fonts {
    /// Looks up the font files.  The lookups run in parallel, so this costs
    /// one `fc-match` run (~10 ms) rather than three.
    pub fn resolve_files(&mut self) {
        let mut bold = self.general.clone();
        bold.weight = 700;
        let specs = [self.general.clone(), bold, self.fixed.clone()];
        let results: Vec<Option<FontFile>> = std::thread::scope(|scope| {
            let handles: Vec<_> = specs.iter().map(|spec| scope.spawn(move || resolve(spec))).collect();
            handles.into_iter().map(|h| h.join().ok().flatten()).collect()
        });
        self.general_file = results[0].clone();
        // fontconfig returns the regular face when no bold exists; only
        // keep a genuinely different file.
        self.bold_file = results[1].clone().filter(|bold| Some(bold) != self.general_file.as_ref());
        self.fixed_file = results[2].clone();
    }

    /// egui font definitions with the desktop fonts first and egui's own
    /// fonts after them, so glyphs the desktop font lacks (emoji, egui's
    /// arrow symbols) still render.
    pub fn font_definitions(&self) -> FontDefinitions {
        let mut defs = FontDefinitions::default();
        let tweak = FontTweak {
            hinting: Some(self.hinting != Hinting::None),
            hinting_target: HintingTarget::Smooth(SmoothHinting {
                // fontconfig's "slight" only snaps vertically, like
                // FreeType's light mode.
                light: self.hinting == Hinting::Slight,
                ..SmoothHinting::default()
            }),
            ..FontTweak::default()
        };
        let fallbacks = defs.families.get(&FontFamily::Proportional).cloned().unwrap_or_default();
        add_font(&mut defs, "desktop-ui", &self.general_file, FontFamily::Proportional, &tweak);
        add_font(&mut defs, "desktop-mono", &self.fixed_file, FontFamily::Monospace, &tweak);
        let bold = FontFamily::Name(BOLD.into());
        let mut bold_list = fallbacks;
        if add_font(&mut defs, "desktop-bold", &self.bold_file, bold.clone(), &tweak) {
            bold_list.insert(0, "desktop-bold".to_owned());
        } else if self.general_file.is_some() {
            // Without a bold face, the bold family is the regular one.
            bold_list.insert(0, "desktop-ui".to_owned());
        }
        defs.families.insert(bold, bold_list);
        // Other scripts' fonts last, once loaded (see `fallbacks`).
        if let Some(fallbacks) = FALLBACKS.get() {
            for (name, data) in fallbacks {
                defs.font_data.insert(name.clone(), data.clone());
                for list in defs.families.values_mut() {
                    list.push(name.clone());
                }
            }
        }
        defs
    }
}

/*┌─────────────────╮
| Other scripts   |
└─────────────────╯*/
//region Other scripts

/// The scripts desktop UI fonts usually lack, as fontconfig language tags,
/// in the order their fonts are tried: Japanese first, since the CJK fonts
/// share most characters and Japanese files (MikuMikuDance models, Japanese
/// file names) are what Slerp meets most; then Chinese and Korean for the
/// characters Japanese fonts lack.  Emoji are left to egui's own
/// monochrome emoji font: colour emoji fonts (Noto Color Emoji, Apple Color
/// Emoji) are bitmaps egui can't draw.
#[cfg(not(any(windows, target_os = "macos")))]
const LANGUAGES: [&str; 7] = ["ja", "zh-cn", "ko", "ar", "he", "th", "hi"];

/// The system fonts that cover the scripts desktop UI fonts usually lack:
/// CJK, Arabic, Hebrew, Thai and Devanagari.  Found through fontconfig
/// (each language's default sans-serif, one `fc-match` each, in parallel),
/// skipping files already listed, since one CJK collection covers several
/// languages.
#[cfg(not(any(windows, target_os = "macos")))]
pub fn fallback_files() -> Vec<FontFile> {
    let found: Vec<Option<FontFile>> = std::thread::scope(|scope| {
        let handles: Vec<_> = LANGUAGES
            .iter()
            .map(|lang| scope.spawn(move || resolve_pattern(&format!("sans\\-serif:lang={lang}"))))
            .collect();
        handles.into_iter().map(|h| h.join().ok().flatten()).collect()
    });
    let mut out: Vec<FontFile> = Vec::new();
    for f in found.into_iter().flatten() {
        if !out.iter().any(|o| o.path == f.path) {
            out.push(f);
        }
    }
    out
}

/// The system fonts that cover the scripts Segoe UI or San Francisco lack,
/// by their file names in the system font folder, the first present for
/// each script.  Arabic and Hebrew are in Segoe UI itself on Windows.
#[cfg(any(windows, target_os = "macos"))]
pub fn fallback_files() -> Vec<FontFile> {
    #[cfg(windows)]
    let folder = std::env::var_os("WINDIR").map_or_else(|| PathBuf::from(r"C:\Windows"), PathBuf::from).join("Fonts");
    #[cfg(target_os = "macos")]
    let folder = PathBuf::from("/System/Library/Fonts");
    fallback_names()
        .iter()
        .filter_map(|names| names.iter().map(|n| folder.join(n)).find(|p| p.is_file()))
        .map(FontFile::new)
        .collect()
}

/// The font files tried for each script on Windows and macOS, in order.
/// Pure, for tests.
#[cfg_attr(not(any(windows, target_os = "macos")), allow(dead_code))]
fn fallback_names() -> &'static [&'static [&'static str]] {
    if cfg!(windows) {
        &[
            // Japanese: Yu Gothic UI's regular face, else Meiryo.
            &["YuGothR.ttc", "YuGothM.ttc", "meiryo.ttc", "msgothic.ttc"],
            // Simplified and traditional Chinese: Microsoft YaHei, JhengHei.
            &["msyh.ttc", "msyh.ttf"],
            &["msjh.ttc", "msjh.ttf"],
            // Korean: Malgun Gothic.
            &["malgun.ttf"],
            // Thai: Leelawadee UI; Devanagari: Nirmala UI.
            &["LeelawUI.ttf", "leelawad.ttf"],
            &["Nirmala.ttc", "Nirmala.ttf"],
        ]
    } else {
        &[
            // Japanese: Hiragino Sans; Chinese: Hiragino Sans GB (PingFang
            // is a downloadable asset, not in the system folder); Korean:
            // Apple SD Gothic Neo.
            &["ヒラギノ角ゴシック W3.ttc", "AquaKana.ttc"],
            &["Hiragino Sans GB.ttc", "STHeiti Light.ttc"],
            &["AppleSDGothicNeo.ttc"],
            // Arabic, Hebrew, Thai and Devanagari.
            &["SFArabic.ttf", "GeezaPro.ttc"],
            &["SFHebrew.ttf", "ArialHB.ttc"],
            &["ThonburiUI.ttc"],
            &["Kohinoor.ttc"],
        ]
    }
}

/// The fallback fonts, once [`load_fallbacks`] has read them.
static FALLBACKS: std::sync::OnceLock<Vec<(String, Arc<FontData>)>> = std::sync::OnceLock::new();

/// Finds and reads the fallback fonts, once per process; later calls
/// return at once.  The files run to 20 MB (a CJK collection), which is
/// why [`crate::apply`] calls this on a thread of its own rather than
/// before the first frame: text in those scripts shows as boxes for the
/// first few frames at most.
pub fn load_fallbacks() -> bool {
    let fonts = FALLBACKS.get_or_init(|| {
        let tweak = FontTweak::default();
        fallback_files()
            .iter()
            .enumerate()
            .filter_map(|(i, f)| Some((format!("fallback-{i}"), Arc::new(read_font(f, tweak.clone())?))))
            .collect()
    });
    !fonts.is_empty()
}

/// Whether the fallback fonts have been read.
pub fn fallbacks_loaded() -> bool {
    FALLBACKS.get().is_some()
}

//endregion

/// Loads `file` as `name` and puts it first in `family`.  Returns whether
/// the font could be read.
fn add_font(
    defs: &mut FontDefinitions,
    name: &str,
    file: &Option<FontFile>,
    family: FontFamily,
    tweak: &FontTweak,
) -> bool {
    let Some(data) = file.as_ref().and_then(|f| read_font(f, tweak.clone())) else { return false };
    defs.font_data.insert(name.to_owned(), Arc::new(data));
    defs.families.entry(family).or_default().insert(0, name.to_owned());
    true
}

/// The egui font family holding the bold face of the general font:
/// `egui::FontFamily::Name("bold".into())`.
pub const BOLD: &str = "bold";

fn read_font(file: &FontFile, mut tweak: FontTweak) -> Option<FontData> {
    let meta = std::fs::metadata(&file.path).ok()?;
    if meta.len() > MAX_FONT_BYTES {
        return None;
    }
    let bytes = std::fs::read(&file.path).ok()?;
    if !file.variations.is_empty() {
        tweak.coords = VariationCoords::new(file.variations.iter().copied());
    }
    let mut data = FontData::from_owned(bytes).tweak(tweak);
    data.index = file.index;
    Some(data)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_font_tables() {
        let folder = std::path::Path::new("/fonts");
        let mut segoe = FontSpec::new("Segoe UI", 10.5);
        assert_eq!(system_font(&segoe, folder), Some(FontFile::new(folder.join("segoeui.ttf"))));
        segoe.weight = 700;
        assert_eq!(system_font(&segoe, folder).unwrap().path, folder.join("segoeuib.ttf"));
        // San Francisco: one variable file, bold by its weight axis, and the
        // Text optical size for UI sizes.
        let mut sf = FontSpec::new(".AppleSystemUIFont", 13.0);
        sf.weight = 700;
        let f = system_font(&sf, folder).unwrap();
        assert_eq!(f.path, folder.join("SFNS.ttf"));
        assert_eq!(f.variations, vec![(*b"wght", 700.0), (*b"opsz", 17.0)]);
        assert_eq!(system_font(&FontSpec::new("Comic Sans", 10.0), folder), None);
    }

    #[test]
    fn qt6_font_strings() {
        let f = FontSpec::parse_qt("Noto Sans,10,-1,5,400,0,0,0,0,0,0,0,0,0,0,1").unwrap();
        assert_eq!(f, FontSpec { family: "Noto Sans".into(), size_pt: 10.0, weight: 400, italic: false });
        assert!((f.size_px(96.0) - 13.333).abs() < 0.01);
        let f = FontSpec::parse_qt("Hack,9.5,-1,5,700,1,0,0,0,0,0,0,0,0,0,1,Bold Italic").unwrap();
        assert_eq!((f.family.as_str(), f.size_pt, f.weight, f.italic), ("Hack", 9.5, 700, true));
    }

    #[test]
    fn qt5_font_strings_use_the_old_weight_scale() {
        let f = FontSpec::parse_qt("Oxygen,8,-1,5,50,0,0,0,0,0").unwrap();
        assert_eq!((f.size_pt, f.weight), (8.0, 400));
        let f = FontSpec::parse_qt("Oxygen,8,-1,5,75,0,0,0,0,0").unwrap();
        assert_eq!(f.weight, 700);
        // Pixel sizes become points at 96 DPI.
        let f = FontSpec::parse_qt("Pix,-1,16,5,50,0,0,0,0,0").unwrap();
        assert_eq!(f.size_pt, 12.0);
    }

    #[test]
    fn bad_qt_font_strings() {
        assert_eq!(FontSpec::parse_qt(""), None);
        assert_eq!(FontSpec::parse_qt(",10"), None);
        let f = FontSpec::parse_qt("Only Family").unwrap();
        assert_eq!((f.size_pt, f.weight), (10.0, 400));
        let f = FontSpec::parse_qt("X,NaN,inf,5,99999,0").unwrap();
        assert_eq!((f.size_pt, f.weight), (10.0, 900));
    }

    #[test]
    fn pango_descriptions() {
        assert_eq!(FontSpec::parse_pango("Cantarell 11"), Some(FontSpec::new("Cantarell", 11.0)));
        let f = FontSpec::parse_pango("Noto Sans Bold Italic 10").unwrap();
        assert_eq!((f.family.as_str(), f.weight, f.italic, f.size_pt), ("Noto Sans", 700, true, 10.0));
        assert_eq!(FontSpec::parse_pango("Adwaita Sans").unwrap().size_pt, 10.0);
        assert_eq!(FontSpec::parse_pango("Bold 12"), None);
        assert_eq!(FontSpec::parse_pango(""), None);
    }

    #[test]
    fn fallback_tables_name_a_font_for_every_script() {
        // Windows and macOS find their fallbacks by file name; each script
        // has at least one candidate, and Japanese comes first.
        let names = fallback_names();
        assert!(names.len() >= 6 && names.iter().all(|n| !n.is_empty()));
    }

    /// Japanese and Chinese text, boxes with the desktop font alone, have
    /// glyphs once the fallbacks are in, on a machine with a CJK font.
    #[cfg(not(any(windows, target_os = "macos")))]
    #[test]
    fn cjk_text_draws_once_fallbacks_load() {
        let listed = Command::new("fc-list").arg(":lang=ja").output().map(|o| o.stdout).unwrap_or_default();
        if listed.is_empty() {
            eprintln!("no Japanese font installed; skipped");
            return;
        }
        assert!(load_fallbacks());
        let mut fonts = Fonts {
            general: FontSpec::new("sans-serif", 10.0),
            fixed: FontSpec::new("monospace", 10.0),
            small: FontSpec::new("sans-serif", 8.0),
            dpi: 96.0,
            hinting: Hinting::Slight,
            general_file: None,
            bold_file: None,
            fixed_file: None,
        };
        fonts.resolve_files();
        let ctx = egui::Context::default();
        ctx.set_fonts(fonts.font_definitions());
        let _ = ctx.run_ui(egui::RawInput::default(), |_| {});
        for family in [FontFamily::Proportional, FontFamily::Monospace, FontFamily::Name(BOLD.into())] {
            let id = egui::FontId::new(14.0, family);
            let missing: String =
                "センター上半身中文한국어".chars().filter(|&c| !ctx.fonts_mut(|f| f.has_glyph(&id, c))).collect();
            assert!(missing.is_empty(), "{id:?}: {missing:?}");
        }
    }

    #[test]
    fn weight_scales() {
        assert_eq!(opentype_weight_to_fontconfig(400), 80);
        assert_eq!(opentype_weight_to_fontconfig(700), 200);
        assert_eq!(opentype_weight_to_fontconfig(50), 0);
        assert_eq!(escape_fontconfig("A-B:C"), "A\\-B\\:C");
    }
}
