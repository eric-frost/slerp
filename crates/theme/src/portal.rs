//! Appearance settings outside KDE: the XDG desktop portal's
//! `org.freedesktop.appearance` namespace (light or dark, accent colour),
//! GNOME's interface settings, which the GNOME and GTK portals expose too,
//! and GTK's `settings.ini` files as the fallback without a portal.

use std::path::Path;

use crate::color::Rgba;
use crate::kconfig::KConfig;

/// What the desktop says about appearance, from whichever source answered.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Appearance {
    /// `Some(true)` for a dark preference, `Some(false)` for light, `None`
    /// for no preference.
    pub dark: Option<bool>,
    pub accent: Option<Rgba>,
    /// The portal's `contrast` setting asks for high contrast.
    pub high_contrast: bool,
    /// GNOME's `accent-color` name (`blue`, `teal`, ...), when set.
    pub gnome_accent_name: Option<String>,
    /// GNOME or GTK font, as a Pango description.
    pub font: Option<String>,
    pub monospace_font: Option<String>,
    pub icon_theme: Option<String>,
    pub gtk_theme: Option<String>,
    /// GNOME's `text-scaling-factor`.
    pub text_scale: Option<f32>,
}

/// Reads `settings.ini` from `$XDG_CONFIG_HOME/gtk-4.0` and `gtk-3.0`, the
/// files GTK desktops without a portal (and GTK itself) use.
pub fn read_gtk_settings(config_home: &Path) -> Appearance {
    let mut out = Appearance::default();
    for version in ["gtk-4.0", "gtk-3.0"] {
        let Some(ini) = KConfig::load(&config_home.join(version).join("settings.ini")) else { continue };
        let get = |key: &str| ini.get("Settings", key).map(|v| v.trim().trim_matches('"').to_owned());
        out.font = out.font.or_else(|| get("gtk-font-name"));
        out.icon_theme = out.icon_theme.or_else(|| get("gtk-icon-theme-name"));
        out.gtk_theme = out.gtk_theme.or_else(|| get("gtk-theme-name"));
        if out.dark.is_none() {
            out.dark = ini.get_bool("Settings", "gtk-application-prefer-dark-theme").filter(|d| *d);
        }
    }
    // Themes named "-dark" are dark even without the preference flag.
    if out.dark.is_none() && out.gtk_theme.as_deref().is_some_and(|t| t.to_ascii_lowercase().ends_with("-dark")) {
        out.dark = Some(true);
    }
    out
}

/// GNOME's named accent colours (GNOME 47+), as libadwaita draws them.
#[cfg_attr(not(all(feature = "portal", unix, not(target_vendor = "apple"))), allow(dead_code))]
pub fn gnome_accent(name: &str) -> Option<Rgba> {
    Some(match name {
        "blue" => Rgba::rgb8(0x35, 0x84, 0xe4),
        "teal" => Rgba::rgb8(0x21, 0x90, 0xa4),
        "green" => Rgba::rgb8(0x3a, 0x94, 0x4a),
        "yellow" => Rgba::rgb8(0xc8, 0x88, 0x00),
        "orange" => Rgba::rgb8(0xed, 0x5b, 0x00),
        "red" => Rgba::rgb8(0xe6, 0x2d, 0x42),
        "pink" => Rgba::rgb8(0xd5, 0x61, 0x99),
        "purple" => Rgba::rgb8(0x91, 0x41, 0xac),
        "slate" => Rgba::rgb8(0x6f, 0x83, 0x96),
        _ => return None,
    })
}

#[cfg(all(feature = "portal", unix, not(target_vendor = "apple")))]
pub use dbus::{read_portal, watch_portal};

#[cfg(not(all(feature = "portal", unix, not(target_vendor = "apple"))))]
pub fn read_portal() -> Option<Appearance> {
    None
}

#[cfg(all(feature = "portal", unix, not(target_vendor = "apple")))]
mod dbus {
    use std::collections::HashMap;
    use std::sync::mpsc;
    use std::time::Duration;

    use zbus::zvariant::{OwnedValue, Value};

    use super::{Appearance, gnome_accent};
    use crate::color::Rgba;

    /// The whole read, including connecting to the bus, must finish within
    /// this, so a missing or wedged portal never delays the viewer's start.
    const TIMEOUT: Duration = Duration::from_millis(400);

    const DESTINATION: &str = "org.freedesktop.portal.Desktop";
    const PATH: &str = "/org/freedesktop/portal/desktop";
    const INTERFACE: &str = "org.freedesktop.portal.Settings";

    /// Reads the portal's appearance and GNOME interface settings, or
    /// returns `None` when there is no session bus or portal, or it doesn't
    /// answer in time.
    pub fn read_portal() -> Option<Appearance> {
        let (send, receive) = mpsc::channel();
        // The worker may outlive the timeout; it then finishes on its own
        // and its answer is dropped.
        std::thread::Builder::new()
            .name("theme-portal".into())
            .spawn(move || {
                let _ = send.send(read_blocking());
            })
            .ok()?;
        receive.recv_timeout(TIMEOUT).ok().flatten()
    }

    fn read_blocking() -> Option<Appearance> {
        let connection = zbus::blocking::connection::Builder::session().ok()?.method_timeout(TIMEOUT).build().ok()?;
        let reply = connection
            .call_method(
                Some(DESTINATION),
                PATH,
                Some(INTERFACE),
                "ReadAll",
                &(vec!["org.freedesktop.appearance", "org.gnome.desktop.interface"],),
            )
            .ok()?;
        let all: HashMap<String, HashMap<String, OwnedValue>> = reply.body().deserialize().ok()?;
        let mut out = Appearance::default();
        if let Some(appearance) = all.get("org.freedesktop.appearance") {
            for (key, value) in appearance {
                apply_setting(&mut out, "org.freedesktop.appearance", key, value);
            }
        }
        if let Some(gnome) = all.get("org.gnome.desktop.interface") {
            for (key, value) in gnome {
                apply_setting(&mut out, "org.gnome.desktop.interface", key, value);
            }
        }
        Some(out)
    }

    fn apply_setting(out: &mut Appearance, namespace: &str, key: &str, value: &Value<'_>) {
        // Values sometimes arrive wrapped in an extra variant.
        let value = match value {
            Value::Value(inner) => inner.as_ref(),
            other => other,
        };
        match (namespace, key) {
            ("org.freedesktop.appearance", "color-scheme") => {
                if let Ok(scheme) = u32::try_from(value) {
                    out.dark = match scheme {
                        1 => Some(true),
                        2 => Some(false),
                        _ => out.dark,
                    };
                }
            }
            ("org.freedesktop.appearance", "accent-color") => {
                if let Ok((r, g, b)) = <(f64, f64, f64)>::try_from(value.try_clone().unwrap_or(Value::U8(0)))
                    && [r, g, b].iter().all(|c| (0.0..=1.0).contains(c))
                {
                    out.accent = Some(Rgba::from_f32(r as f32, g as f32, b as f32, 1.0));
                }
            }
            ("org.freedesktop.appearance", "contrast") => {
                out.high_contrast = u32::try_from(value).is_ok_and(|c| c == 1);
            }
            ("org.gnome.desktop.interface", key) => {
                let text = <&str>::try_from(value).ok().map(str::to_owned);
                match key {
                    "color-scheme" if out.dark.is_none() => {
                        out.dark = match text.as_deref() {
                            Some("prefer-dark") => Some(true),
                            Some("prefer-light") => Some(false),
                            _ => None,
                        };
                    }
                    "accent-color" => {
                        if out.accent.is_none() {
                            out.accent = text.as_deref().and_then(gnome_accent);
                        }
                        out.gnome_accent_name = text;
                    }
                    "font-name" => out.font = text,
                    "monospace-font-name" => out.monospace_font = text,
                    "icon-theme" => out.icon_theme = text,
                    "gtk-theme" => out.gtk_theme = text,
                    "text-scaling-factor" => {
                        out.text_scale = f64::try_from(value)
                            .ok()
                            .map(|v| v as f32)
                            .filter(|v| v.is_finite() && *v > 0.25 && *v < 4.0);
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    /// Calls `on_change` whenever the portal reports a changed appearance
    /// or GNOME interface setting, from a background thread, until
    /// `on_change` returns `false`.  Returns `false` when there is no bus to
    /// listen on.
    pub fn watch_portal(mut on_change: impl FnMut() -> bool + Send + 'static) -> bool {
        let (send, receive) = mpsc::channel();
        let spawned = std::thread::Builder::new().name("theme-portal-watch".into()).spawn(move || {
            let Some(connection) = zbus::blocking::Connection::session().ok() else {
                let _ = send.send(false);
                return;
            };
            let rule = zbus::MatchRule::builder()
                .msg_type(zbus::message::Type::Signal)
                .interface(INTERFACE)
                .and_then(|r| r.member("SettingChanged"))
                .and_then(|r| r.path(PATH))
                .map(|r| r.build());
            let Ok(rule) = rule else {
                let _ = send.send(false);
                return;
            };
            let Ok(messages) = zbus::blocking::MessageIterator::for_match_rule(rule, &connection, Some(16)) else {
                let _ = send.send(false);
                return;
            };
            let _ = send.send(true);
            for message in messages {
                let Ok(message) = message else { continue };
                let relevant = message
                    .body()
                    .deserialize::<(String, String, OwnedValue)>()
                    .is_ok_and(|(ns, _, _)| ns == "org.freedesktop.appearance" || ns == "org.gnome.desktop.interface");
                if relevant && !on_change() {
                    return;
                }
            }
        });
        spawned.is_ok() && receive.recv_timeout(TIMEOUT).unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gtk_settings_files() {
        let dir = std::env::temp_dir().join(format!("slerp-theme-gtk-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("gtk-3.0")).unwrap();
        std::fs::write(
            dir.join("gtk-3.0/settings.ini"),
            "[Settings]\ngtk-theme-name=Adwaita-dark\ngtk-icon-theme-name=Papirus\ngtk-font-name=Cantarell 11\n",
        )
        .unwrap();
        let settings = read_gtk_settings(&dir);
        assert_eq!(settings.dark, Some(true));
        assert_eq!(settings.icon_theme.as_deref(), Some("Papirus"));
        assert_eq!(settings.font.as_deref(), Some("Cantarell 11"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn accent_names() {
        assert_eq!(gnome_accent("blue"), Some(Rgba::rgb8(0x35, 0x84, 0xe4)));
        assert_eq!(gnome_accent("chartreuse"), None);
    }
}
