//! The watcher re-applies the theme when `kdeglobals` changes.

use std::time::{Duration, Instant};

use slerp_theme::{Environment, ThemeWatcher, apply, detect_with};

#[test]
fn switching_kdeglobals_from_light_to_dark_restyles_egui() {
    let dir = std::env::temp_dir().join(format!("slerp-theme-watch-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let globals = dir.join("kdeglobals");
    std::fs::write(&globals, "[Colors:Window]\nBackgroundNormal=239,240,241\nForegroundNormal=35,38,41\n").unwrap();
    let env = Environment {
        config_home: dir.clone(),
        config_dirs: Vec::new(),
        data_home: dir.join("data"),
        data_dirs: Vec::new(),
        current_desktop: vec!["KDE".into()],
        session_type: "wayland".into(),
        forced_desktop: None,
        use_portal: false,
        resolve_fonts: false,
    };

    let ctx = egui::Context::default();
    let theme = detect_with(&env);
    assert!(!theme.dark);
    apply(&ctx, &theme);
    let mut watcher = ThemeWatcher::with_environment(&ctx, theme, env);
    assert!(!watcher.poll(&ctx), "nothing changed yet");

    // 1. Change the scheme.  Make sure the modification time moves on even
    //    on file systems with coarse timestamps.
    std::thread::sleep(Duration::from_millis(1100));
    std::fs::write(&globals, "[Colors:Window]\nBackgroundNormal=32,35,38\nForegroundNormal=252,252,252\n").unwrap();

    // 2. The background thread checks twice a second.
    let start = Instant::now();
    let mut changed = false;
    while start.elapsed() < Duration::from_secs(5) {
        if watcher.poll(&ctx) {
            changed = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(changed, "the watcher didn't notice the change");
    assert!(watcher.theme().dark);
    assert_eq!(ctx.global_style().visuals.panel_fill, egui::Color32::from_rgb(32, 35, 38));
    assert!(!watcher.poll(&ctx), "one change is reported once");
    let _ = std::fs::remove_dir_all(&dir);
}
