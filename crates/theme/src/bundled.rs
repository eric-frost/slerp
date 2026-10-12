//! Icons built into Slerp for desktops without a freedesktop icon theme,
//! which means Windows and macOS: without them the viewer's tool bar shows
//! words.  They are Lucide's line icons (ISC licence, `icons/LICENSE`),
//! drawn in the theme's text colour, and are used only when no installed
//! theme has the icon, so Linux keeps Breeze or whatever the desktop uses.

/// Each icon the viewer asks for by its freedesktop name, with the file in
/// `icons/` that stands in for it.  Files keep Lucide's names, so they can
/// be refreshed from upstream as they are.
const ICONS: &[(&str, &str)] = &[
    ("document-open", include_str!("../icons/folder-open.svg")),
    ("document-save", include_str!("../icons/save.svg")),
    // The viewer's side-panel switch.
    ("document-properties", include_str!("../icons/panel-right.svg")),
    ("edit-copy", include_str!("../icons/copy.svg")),
    ("media-playback-start", include_str!("../icons/play.svg")),
    ("media-playback-pause", include_str!("../icons/pause.svg")),
    ("media-playback-stop", include_str!("../icons/square.svg")),
    ("media-skip-backward", include_str!("../icons/skip-back.svg")),
    ("media-skip-forward", include_str!("../icons/skip-forward.svg")),
    ("zoom-in", include_str!("../icons/zoom-in.svg")),
    ("zoom-out", include_str!("../icons/zoom-out.svg")),
    ("zoom-fit-best", include_str!("../icons/scan.svg")),
    ("zoom-original", include_str!("../icons/actual-size.svg")),
    ("view-fullscreen", include_str!("../icons/expand.svg")),
    ("view-restore", include_str!("../icons/shrink.svg")),
    ("view-grid", include_str!("../icons/grid-3x3.svg")),
    ("view-refresh", include_str!("../icons/refresh-cw.svg")),
    ("camera-photo", include_str!("../icons/camera.svg")),
    ("go-previous", include_str!("../icons/chevron-left.svg")),
    ("go-next", include_str!("../icons/chevron-right.svg")),
    ("go-up", include_str!("../icons/chevron-up.svg")),
    ("go-down", include_str!("../icons/chevron-down.svg")),
    ("help-about", include_str!("../icons/info.svg")),
    ("application-exit", include_str!("../icons/log-out.svg")),
    ("configure", include_str!("../icons/settings.svg")),
    ("edit-find", include_str!("../icons/search.svg")),
    ("object-rotate-left", include_str!("../icons/rotate-ccw.svg")),
    ("object-rotate-right", include_str!("../icons/rotate-cw.svg")),
    ("visibility", include_str!("../icons/eye.svg")),
    ("hint", include_str!("../icons/lightbulb.svg")),
];

/// The built-in SVG for a freedesktop icon name, drawn in `color` (a CSS
/// colour such as `#232629`), or `None` for names it doesn't cover.
pub fn svg(name: &str, color: &str) -> Option<String> {
    let (_, svg) = ICONS.iter().find(|(n, _)| *n == name)?;
    // Lucide strokes in `currentColor`, which resvg would draw black.
    Some(svg.replace("currentColor", color))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn covers_every_viewer_icon_in_the_given_colour() {
        for name in crate::VIEWER_ICONS {
            let svg = svg(name, "#2060c0").unwrap_or_else(|| panic!("no built-in icon for {name}"));
            assert!(!svg.contains("currentColor"), "{name}");
            // Drawn as egui will draw it: some pixels must be solid and in
            // the colour asked for.
            let image = egui_extras::image::load_svg_bytes(svg.as_bytes(), &Default::default())
                .unwrap_or_else(|e| panic!("{name}: {e}"));
            assert_eq!(image.size, [24, 24], "{name}");
            let solid = image.pixels.iter().filter(|p| p.to_srgba_unmultiplied() == [0x20, 0x60, 0xc0, 255]).count();
            assert!(solid >= 10, "{name} has {solid} solid pixels");
        }
        assert_eq!(svg("no-such-icon", "#000000"), None);
    }
}
