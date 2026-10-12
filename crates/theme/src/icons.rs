//! Freedesktop icon themes: finding `document-open` in the user's theme at
//! the right size, following `Inherits` chains down to `hicolor`, as the
//! Icon Theme Specification describes.  Icons from themes that follow the
//! colour scheme (Breeze) are recoloured the way KIconLoader does it, by
//! replacing their embedded style sheet.

use std::borrow::Cow;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use egui::ImageSource;
use egui::load::Bytes;

use crate::color::Rgba;
use crate::kconfig::KConfig;

/// SVG and PNG icons larger than this are not icons; skip them.
const MAX_ICON_BYTES: u64 = 2 << 20;

/// Theme inheritance deeper than this is a loop or a broken theme.
const MAX_INHERIT_DEPTH: usize = 8;

/// How a theme directory's icons may be scaled, from `index.theme`.
#[derive(Clone, Copy, Debug, PartialEq)]
enum DirType {
    Fixed,
    Scalable { min: u32, max: u32 },
    Threshold { threshold: u32 },
}

#[derive(Clone, Debug)]
struct ThemeDir {
    /// The subdirectory, such as `actions/22`.
    path: String,
    size: u32,
    scale: u32,
    kind: DirType,
}

/// One parsed icon theme.
#[derive(Clone, Debug)]
pub struct IconTheme {
    pub name: String,
    /// Where the theme's directories live, one per search path that has it.
    roots: Vec<PathBuf>,
    dirs: Vec<ThemeDir>,
    pub inherits: Vec<String>,
    /// `FollowsColorScheme=true`: KDE recolours the icons' style sheets.
    pub follows_color_scheme: bool,
}

impl IconTheme {
    /// Finds and parses the theme called `name` in `search_paths`.
    pub fn load(name: &str, search_paths: &[PathBuf]) -> Option<IconTheme> {
        if name.is_empty() || name.contains('/') || name == ".." {
            return None;
        }
        let roots: Vec<PathBuf> = search_paths.iter().map(|p| p.join(name)).filter(|p| p.is_dir()).collect();
        let index = roots.iter().find_map(|root| KConfig::load(&root.join("index.theme")))?;
        let group = "Icon Theme";
        let mut dirs = Vec::new();
        let listed = index.get_list(group, "Directories").into_iter().chain(index.get_list(group, "ScaledDirectories"));
        for path in listed {
            let Some(size) = index.get_u32(&path, "Size").filter(|s| *s > 0) else { continue };
            let scale = index.get_u32(&path, "Scale").unwrap_or(1).max(1);
            let kind = match index.get(&path, "Type").map(str::trim) {
                Some("Fixed") => DirType::Fixed,
                Some("Scalable") => DirType::Scalable {
                    min: index.get_u32(&path, "MinSize").unwrap_or(size),
                    max: index.get_u32(&path, "MaxSize").unwrap_or(size),
                },
                _ => DirType::Threshold { threshold: index.get_u32(&path, "Threshold").unwrap_or(2) },
            };
            dirs.push(ThemeDir { path, size, scale, kind });
        }
        Some(IconTheme {
            name: name.to_owned(),
            roots,
            dirs,
            inherits: index.get_list(group, "Inherits"),
            follows_color_scheme: index.get_bool(group, "FollowsColorScheme").unwrap_or(false),
        })
    }

    /// The spec's `LookupIcon`: an exact size match first, otherwise the
    /// file in the directory whose size is closest.
    fn lookup(&self, name: &str, size: u32, scale: u32) -> Option<PathBuf> {
        let mut best: Option<(u32, PathBuf)> = None;
        for dir in &self.dirs {
            let distance = dir.distance(size, scale);
            if best.as_ref().is_some_and(|(d, _)| distance >= *d) {
                continue;
            }
            for root in &self.roots {
                if let Some(path) = find_file(&root.join(&dir.path), name) {
                    if distance == 0 {
                        return Some(path);
                    }
                    best = Some((distance, path));
                    break;
                }
            }
        }
        best.map(|(_, path)| path)
    }
}

impl ThemeDir {
    /// The spec's `DirectorySizeDistance`; zero means the directory
    /// matches (`DirectoryMatchesSize`).
    fn distance(&self, size: u32, scale: u32) -> u32 {
        let wanted = size * scale;
        let (min, max) = match self.kind {
            DirType::Fixed => (self.size, self.size),
            DirType::Scalable { min, max } => (min, max),
            DirType::Threshold { threshold } => {
                (self.size.saturating_sub(threshold), self.size.saturating_add(threshold))
            }
        };
        let distance =
            if wanted < min * self.scale { min * self.scale - wanted } else { wanted.saturating_sub(max * self.scale) };
        // Among matching directories, prefer the one drawn for this size,
        // and the right scale, so Breeze's 22 px icons beat its 16@2x ones.
        if distance == 0 && (self.size != size || self.scale != scale) { 1 } else { distance * 2 }
    }
}

/// Looks for `name` with the extensions egui can draw.  SVG comes first:
/// it stays sharp at any scale factor.
fn find_file(dir: &Path, name: &str) -> Option<PathBuf> {
    ["svg", "png"].iter().map(|ext| dir.join(format!("{name}.{ext}"))).find(|p| p.is_file())
}

/// The base directories icon themes live in, in the spec's order:
/// `$HOME/.icons`, `$XDG_DATA_HOME/icons`, then `$XDG_DATA_DIRS/icons`.
pub fn search_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();
    let home = std::env::var_os("HOME").map(PathBuf::from);
    if let Some(home) = &home {
        paths.push(home.join(".icons"));
    }
    match std::env::var_os("XDG_DATA_HOME").filter(|v| !v.is_empty()) {
        Some(data) => paths.push(PathBuf::from(data).join("icons")),
        None => {
            if let Some(home) = &home {
                paths.push(home.join(".local/share/icons"));
            }
        }
    }
    let dirs = std::env::var("XDG_DATA_DIRS").ok().filter(|v| !v.is_empty());
    for dir in dirs.as_deref().unwrap_or("/usr/local/share:/usr/share").split(':').filter(|d| !d.is_empty()) {
        paths.push(PathBuf::from(dir).join("icons"));
    }
    paths
}

/// An icon theme with its whole inheritance chain resolved, ready for
/// lookups.
#[derive(Clone, Debug)]
pub struct IconLookup {
    /// The themes to search in order: the chosen theme, its ancestors
    /// depth-first, then `hicolor`.
    themes: Vec<IconTheme>,
    /// The spec's last resort, plain files in `/usr/share/pixmaps`.
    pixmaps: Vec<PathBuf>,
}

impl IconLookup {
    pub fn new(theme: &str, search_paths: &[PathBuf]) -> IconLookup {
        let mut themes: Vec<IconTheme> = Vec::new();
        let mut pending = vec![(theme.to_owned(), 0usize)];
        while let Some((name, depth)) = pending.pop() {
            if depth > MAX_INHERIT_DEPTH || themes.iter().any(|t| t.name == name) {
                continue;
            }
            let Some(loaded) = IconTheme::load(&name, search_paths) else { continue };
            // Depth-first in the listed order: push parents reversed.
            for parent in loaded.inherits.iter().rev() {
                pending.push((parent.clone(), depth + 1));
            }
            themes.push(loaded);
        }
        if !themes.iter().any(|t| t.name == "hicolor")
            && let Some(hicolor) = IconTheme::load("hicolor", search_paths)
        {
            themes.push(hicolor);
        }
        let pixmaps = search_paths
            .iter()
            .filter_map(|p| p.parent().map(|share| share.join("pixmaps")))
            .filter(|p| p.is_dir())
            .collect();
        IconLookup { themes, pixmaps }
    }

    /// The name of the theme lookups start from, if it was found.
    pub fn theme_name(&self) -> Option<&str> {
        self.themes.first().map(|t| t.name.as_str())
    }

    /// Finds the file for `name` at `size` logical pixels.  When nothing
    /// matches, the name is shortened at its last dash and tried again, so
    /// `media-playback-start-symbolic` can fall back to
    /// `media-playback-start` and then `media-playback`, as the spec
    /// suggests.  Returns the file and whether its theme follows the colour
    /// scheme.
    pub fn find(&self, name: &str, size: u32, scale: u32) -> Option<(PathBuf, bool)> {
        if name.is_empty() || name.contains('/') || name.contains("..") {
            return None;
        }
        let size = size.clamp(1, 1024);
        let scale = scale.clamp(1, 8);
        let mut candidate = name;
        loop {
            for theme in &self.themes {
                if let Some(path) = theme.lookup(candidate, size, scale) {
                    return Some((path, theme.follows_color_scheme));
                }
            }
            if let Some(path) = self.pixmaps.iter().find_map(|dir| find_file(dir, candidate)) {
                return Some((path, false));
            }
            match candidate.rfind('-') {
                Some(i) if i > 0 => candidate = &candidate[..i],
                _ => return None,
            }
        }
    }
}

/// The colours KIconLoader writes into recolourable SVG icons (see
/// kiconthemes' `KIconColors::stylesheet`).
#[derive(Clone, Debug, PartialEq)]
pub struct IconColors {
    pub text: Rgba,
    pub background: Rgba,
    pub highlight: Rgba,
    pub highlighted_text: Rgba,
    pub positive: Rgba,
    pub neutral: Rgba,
    pub negative: Rgba,
    pub accent: Rgba,
}

impl IconColors {
    /// The style sheet for an icon in its normal state, or for one drawn
    /// on a selection (`selected`), where text and highlight swap.
    pub fn stylesheet(&self, selected: bool) -> String {
        let (text, background, highlight, highlighted_text) = if selected {
            (self.highlighted_text, self.highlight, self.highlighted_text, self.highlight)
        } else {
            (self.text, self.background, self.highlight, self.highlighted_text)
        };
        let status = |c: Rgba| if selected { self.highlighted_text } else { c };
        let accent = if selected { self.accent.mix(self.highlighted_text, 0.15) } else { self.accent };
        format!(
            ".ColorScheme-Text {{ color:{}; }} .ColorScheme-Background {{ color:{}; }} \
             .ColorScheme-Highlight {{ color:{}; }} .ColorScheme-HighlightedText {{ color:{}; }} \
             .ColorScheme-PositiveText {{ color:{}; }} .ColorScheme-NeutralText {{ color:{}; }} \
             .ColorScheme-NegativeText {{ color:{}; }} .ColorScheme-Accent {{ color:{}; }}",
            text.to_hex(),
            background.to_hex(),
            highlight.to_hex(),
            highlighted_text.to_hex(),
            status(self.positive).to_hex(),
            status(self.neutral).to_hex(),
            status(self.negative).to_hex(),
            accent.to_hex()
        )
    }
}

/// Replaces the contents of the `<style id="current-color-scheme">` element,
/// as KIconLoader does before rendering.  Returns `None` when the SVG has no
/// such element and so doesn't follow the colour scheme.
pub fn recolor_svg(svg: &str, stylesheet: &str) -> Option<String> {
    let id = svg.find("current-color-scheme")?;
    let open_start = svg[..id].rfind("<style")?;
    let open_end = id + svg[id..].find('>')?;
    // A self-closing <style/> has nothing to replace.
    if svg[open_start..open_end].ends_with('/') {
        return None;
    }
    let close = open_end + svg[open_end..].find("</style>")?;
    let mut out = String::with_capacity(svg.len() + stylesheet.len());
    out.push_str(&svg[..=open_end]);
    out.push_str(stylesheet);
    out.push_str(&svg[close..]);
    Some(out)
}

/*┌─────────────────╮
| Global lookup   |
└─────────────────╯*/
//region Global lookup

/// The icon state `apply()` configures and `icon()` reads.  It is global so
/// the viewer can ask for an icon anywhere without passing the theme
/// around.
struct IconState {
    /// The theme name asked for, which may not be installed.
    requested_theme: String,
    lookup: IconLookup,
    colors: IconColors,
    /// Bumped on every theme change, so new icons get new URIs and egui
    /// doesn't serve stale textures.
    generation: u64,
    cache: HashMap<(String, u32, bool), Option<ImageSource<'static>>>,
}

static STATE: Mutex<Option<IconState>> = Mutex::new(None);

/// Points icon lookups at a theme and its colours.  Returns the URIs of
/// icons handed out under the previous configuration, which egui should
/// forget, or nothing when the configuration didn't change.
pub(crate) fn configure(theme_name: &str, colors: IconColors) -> Vec<String> {
    let mut guard = STATE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(state) = guard.as_ref()
        && state.requested_theme == theme_name
        && state.colors == colors
    {
        return Vec::new();
    }
    let stale = guard.as_ref().map_or_else(Vec::new, |state| {
        state.cache.values().flatten().filter_map(|source| source.uri().map(str::to_owned)).collect()
    });
    let generation = guard.as_ref().map_or(0, |s| s.generation + 1);
    *guard = Some(IconState {
        requested_theme: theme_name.to_owned(),
        lookup: IconLookup::new(theme_name, &search_paths()),
        colors,
        generation,
        cache: HashMap::new(),
    });
    stale
}

pub(crate) fn is_configured() -> bool {
    STATE.lock().unwrap_or_else(|e| e.into_inner()).is_some()
}

/// Looks up an icon through the configured theme.  See [`crate::icon`].
pub(crate) fn icon(name: &str, size: u32, selected: bool) -> Option<ImageSource<'static>> {
    let mut guard = STATE.lock().unwrap_or_else(|e| e.into_inner());
    let state = guard.as_mut()?;
    let key = (name.to_owned(), size, selected);
    if let Some(cached) = state.cache.get(&key) {
        return cached.clone();
    }
    let source = load_icon(state, name, size, selected);
    state.cache.insert(key, source.clone());
    source
}

fn load_icon(state: &IconState, name: &str, size: u32, selected: bool) -> Option<ImageSource<'static>> {
    let Some((path, follows)) = state.lookup.find(name, size, 1) else {
        return bundled_icon(state, name, size, selected);
    };
    if std::fs::metadata(&path).ok()?.len() > MAX_ICON_BYTES {
        return None;
    }
    let bytes = std::fs::read(&path).ok()?;
    let is_svg = path.extension().is_some_and(|e| e.eq_ignore_ascii_case("svg"));
    let bytes: Arc<[u8]> = if is_svg && follows {
        let text = String::from_utf8_lossy(&bytes);
        match recolor_svg(&text, &state.colors.stylesheet(selected)) {
            Some(recolored) => Arc::from(recolored.into_bytes()),
            None => Arc::from(bytes),
        }
    } else {
        Arc::from(bytes)
    };
    // egui picks a loader by the URI's extension, and caches decoded images
    // by URI, so the URI carries the generation, state and real extension.
    let ext = if is_svg { "svg" } else { "png" };
    let uri = format!(
        "bytes://slerp-theme/{}/{}/{}{}@{}.{}",
        state.generation,
        state.lookup.theme_name().unwrap_or("none"),
        name,
        if selected { "-selected" } else { "" },
        size,
        ext
    );
    Some(ImageSource::Bytes { uri: Cow::Owned(uri), bytes: Bytes::Shared(bytes) })
}

/// The built-in icon for `name`, when no installed theme has one: always
/// the case on Windows and macOS.  It takes the colour Breeze icons get, so
/// it follows light and dark mode and selection highlights the same way.
fn bundled_icon(state: &IconState, name: &str, size: u32, selected: bool) -> Option<ImageSource<'static>> {
    let color = if selected { state.colors.highlighted_text } else { state.colors.text };
    let svg = crate::bundled::svg(name, &color.to_hex())?;
    let uri = format!(
        "bytes://slerp-theme/{}/bundled/{}{}@{}.svg",
        state.generation,
        name,
        if selected { "-selected" } else { "" },
        size
    );
    Some(ImageSource::Bytes { uri: Cow::Owned(uri), bytes: Bytes::Shared(Arc::from(svg.into_bytes())) })
}

//endregion

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recolours_kde_style_sheets() {
        let svg = r#"<svg><defs><style type="text/css" id="current-color-scheme">.ColorScheme-Text { color: #fcfcfc; } </style></defs><path class="ColorScheme-Text"/></svg>"#;
        let out = recolor_svg(svg, ".ColorScheme-Text { color:#232629; }").unwrap();
        assert!(out.contains("#232629") && !out.contains("#fcfcfc"));
        assert!(out.ends_with("<path class=\"ColorScheme-Text\"/></svg>"));
        assert_eq!(recolor_svg("<svg/>", "x"), None);
        assert_eq!(recolor_svg("<svg><style id=\"current-color-scheme\"/></svg>", "x"), None);
        assert_eq!(recolor_svg("<svg>current-color-scheme</svg>", "x"), None);
    }

    #[test]
    fn size_distance_prefers_exact_directories() {
        let fixed = |size, scale| ThemeDir { path: String::new(), size, scale, kind: DirType::Fixed };
        assert_eq!(fixed(22, 1).distance(22, 1), 0);
        assert_eq!(fixed(16, 1).distance(22, 1), 12);
        assert!(fixed(22, 1).distance(22, 1) < fixed(11, 2).distance(22, 1));
        let scalable =
            ThemeDir { path: String::new(), size: 48, scale: 1, kind: DirType::Scalable { min: 8, max: 512 } };
        assert_eq!(scalable.distance(22, 1), 1);
    }
}
