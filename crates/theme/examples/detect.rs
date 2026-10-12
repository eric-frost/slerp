//! Prints what `slerp_theme::detect()` finds and how long it takes, for
//! checking a desktop's settings without opening a window.
//!
//! ```sh
//! cargo run -p slerp-theme --example detect
//! ```

use std::time::Instant;

fn main() {
    // 1. Detection, cold (first fontconfig run) and warm.
    let start = Instant::now();
    let theme = slerp_theme::detect();
    let cold = start.elapsed();
    let start = Instant::now();
    let _ = slerp_theme::detect();
    let warm = start.elapsed();

    let c = &theme.colors;
    let f = &theme.fonts;
    println!("desktop      {:?}, dark {}", theme.desktop, theme.dark);
    println!("scheme       {:?}, accent {:?}", c.name, c.accent.map(|a| a.to_hex()));
    println!("window       bg {} fg {}", c.window.background.to_hex(), c.window.foreground.to_hex());
    println!("view         bg {} fg {}", c.view.background.to_hex(), c.view.foreground.to_hex());
    println!("button       bg {} fg {}", c.button.background.to_hex(), c.button.foreground.to_hex());
    println!("selection    bg {} fg {}", c.selection.background.to_hex(), c.selection.foreground.to_hex());
    println!("focus/hover  {} {}", c.view.focus.to_hex(), c.view.hover.to_hex());
    println!(
        "font         {} {} pt (weight {}) -> {:?}",
        f.general.family, f.general.size_pt, f.general.weight, f.general_file
    );
    println!("bold         {:?}", f.bold_file);
    println!("fixed        {} {} pt -> {:?}", f.fixed.family, f.fixed.size_pt, f.fixed_file);
    println!("small        {} {} pt, dpi {}, hinting {:?}", f.small.family, f.small.size_pt, f.dpi, f.hinting);
    println!("icons        {}", theme.icon_theme);
    println!("animation    x{}", theme.animation_factor);
    println!("watching     {:?}", theme.watched_files);

    // 2. Icon lookups, cold then cached.
    let start = Instant::now();
    let found = slerp_theme::VIEWER_ICONS.iter().filter(|n| slerp_theme::icon(n, 22).is_some()).count();
    let icons_cold = start.elapsed();
    let start = Instant::now();
    let _ = slerp_theme::VIEWER_ICONS.iter().filter(|n| slerp_theme::icon(n, 22).is_some()).count();
    let icons_warm = start.elapsed();
    println!("viewer icons {found}/{} found", slerp_theme::VIEWER_ICONS.len());

    // 3. Applying to a fresh egui context: style plus font loading.
    let ctx = egui::Context::default();
    let start = Instant::now();
    slerp_theme::apply(&ctx, &theme);
    let apply = start.elapsed();
    println!(
        "timings      detect {:.1} ms cold, {:.1} ms warm; {} icons {:.1} ms cold, {:.3} ms cached; apply {:.1} ms",
        ms(cold),
        ms(warm),
        found,
        ms(icons_cold),
        ms(icons_warm),
        ms(apply)
    );
}

fn ms(d: std::time::Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}
