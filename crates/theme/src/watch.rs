//! Noticing when the user changes their theme, colours, fonts or icons, so
//! the viewer restyles itself without restarting, as KDE apps do.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime};

use crate::{Desktop, DesktopTheme, Environment, apply, detect_with, modified};

/// How often the background thread checks the settings files' times.  Each
/// check is a few `stat` calls.
const INTERVAL: Duration = Duration::from_millis(500);

/// Watches the desktop's settings and re-applies the theme when they
/// change.  A background thread checks the settings files' modification
/// times twice a second (and, outside KDE, listens for the portal's
/// `SettingChanged` signal) and wakes egui when something changed; `poll`
/// then re-reads the theme.  Dropping the watcher stops the thread.
pub struct ThemeWatcher {
    theme: DesktopTheme,
    env: Environment,
    changed: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    files: Arc<std::sync::Mutex<Vec<PathBuf>>>,
}

impl ThemeWatcher {
    /// Starts watching on behalf of `ctx`, given the theme that is
    /// currently applied (the one [`crate::detect`] returned).
    pub fn new(ctx: &egui::Context, theme: DesktopTheme) -> ThemeWatcher {
        Self::with_environment(ctx, theme, Environment::from_env())
    }

    /// As [`ThemeWatcher::new`], reading settings from `env`.
    pub fn with_environment(ctx: &egui::Context, theme: DesktopTheme, env: Environment) -> ThemeWatcher {
        let changed = Arc::new(AtomicBool::new(false));
        let stop = Arc::new(AtomicBool::new(false));
        let files = Arc::new(std::sync::Mutex::new(theme.watched_files.clone()));

        // 1. File times, for KDE and GTK settings files.  Windows and macOS
        //    keep their settings in the registry and the preferences daemon,
        //    so their few values are read and compared instead.
        {
            let (changed, stop, files, ctx) = (changed.clone(), stop.clone(), files.clone(), ctx.clone());
            let stamp: fn() -> u64 = match theme.desktop {
                Desktop::Windows => crate::fluent::settings_stamp,
                Desktop::MacOs => crate::aqua::settings_stamp,
                _ => || 0,
            };
            let _ = std::thread::Builder::new().name("theme-watch".into()).spawn(move || {
                let mut known: Vec<(PathBuf, Option<SystemTime>)> = Vec::new();
                let mut known_stamp = stamp();
                while !stop.load(Ordering::Relaxed) {
                    let current_stamp = stamp();
                    if current_stamp != known_stamp {
                        known_stamp = current_stamp;
                        changed.store(true, Ordering::Relaxed);
                        ctx.request_repaint();
                    }
                    let current = files.lock().map(|f| f.clone()).unwrap_or_default();
                    let times: Vec<(PathBuf, Option<SystemTime>)> =
                        current.into_iter().map(|p| (p.clone(), modified(&p))).collect();
                    // A changed list of files (after a re-read) is a new
                    // baseline, not a change.
                    let same_files = known.len() == times.len() && known.iter().zip(&times).all(|(a, b)| a.0 == b.0);
                    if same_files && known != times {
                        changed.store(true, Ordering::Relaxed);
                        ctx.request_repaint();
                    }
                    known = times;
                    std::thread::sleep(INTERVAL);
                }
            });
        }

        // 2. The portal's change signal, outside KDE (whose settings all live
        //    in files).
        #[cfg(all(feature = "portal", unix, not(target_vendor = "apple")))]
        if matches!(theme.desktop, Desktop::Gnome | Desktop::Other) && env.use_portal {
            let (changed, stop, ctx) = (changed.clone(), stop.clone(), ctx.clone());
            crate::portal::watch_portal(move || {
                changed.store(true, Ordering::Relaxed);
                ctx.request_repaint();
                !stop.load(Ordering::Relaxed)
            });
        }

        ThemeWatcher { theme, env, changed, stop, files }
    }

    /// The theme currently applied.
    pub fn theme(&self) -> &DesktopTheme {
        &self.theme
    }

    /// Call once per frame.  Cheap unless the background thread saw a
    /// change, in which case it re-reads the theme and, if it differs,
    /// applies it and returns `true`.
    pub fn poll(&mut self, ctx: &egui::Context) -> bool {
        settle_cursor_blink(ctx);
        if !self.changed.swap(false, Ordering::Relaxed) {
            return false;
        }
        // Re-read without fontconfig, and only look font files up again if
        // the fonts themselves changed.
        let mut env = self.env.clone();
        env.resolve_fonts = false;
        let mut theme = detect_with(&env);
        let old = &self.theme.fonts;
        let same_fonts = theme.fonts.general == old.general
            && theme.fonts.fixed == old.fixed
            && theme.fonts.small == old.small
            && theme.fonts.dpi == old.dpi
            && theme.fonts.hinting == old.hinting;
        if same_fonts {
            theme.fonts = old.clone();
        } else if self.env.resolve_fonts {
            theme.fonts.resolve_files();
        }
        if let Ok(mut files) = self.files.lock() {
            files.clone_from(&theme.watched_files);
        }
        if theme == self.theme {
            return false;
        }
        apply(ctx, &theme);
        self.theme = theme;
        true
    }
}

impl Drop for ThemeWatcher {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// How long a focused text field's cursor blinks after the last input, as
/// GTK's `gtk-cursor-blink-timeout` does.  egui wakes the window about
/// three times per half-second blink, so a field left focused (the Contents
/// panel's Find box) cost 6 frames a second for ever; after this long the
/// cursor stays lit and the window sleeps until the next input.
const BLINK_SECONDS: f64 = 10.0;

/// Turns the text cursor's blinking off once there has been no input for
/// [`BLINK_SECONDS`], and on again at the next input.  Called every frame.
fn settle_cursor_blink(ctx: &egui::Context) {
    let id = egui::Id::new("slerp-theme-last-input");
    let (now, input) = ctx.input(|i| {
        let pointer = i.pointer.is_moving() || i.pointer.any_down();
        (i.time, pointer || !i.events.is_empty())
    });
    let last = match ctx.data(|d| d.get_temp::<f64>(id)) {
        Some(t) if !input => t,
        _ => {
            ctx.data_mut(|d| d.insert_temp(id, now));
            now
        }
    };
    let blink = now - last < BLINK_SECONDS;
    if ctx.global_style().visuals.text_cursor.blink != blink {
        ctx.all_styles_mut(|s| s.visuals.text_cursor.blink = blink);
    }
}
