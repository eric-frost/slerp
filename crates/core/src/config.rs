//! User settings from `~/.config/slerp/config.jsonc`
//! (`%APPDATA%\Slerp\config.jsonc` on Windows,
//! `~/Library/Application Support/Slerp/config.jsonc` on macOS), and
//! where the programs keep their other files.

use serde::Deserialize;
use std::path::PathBuf;

/// Settings, all optional in the file.  Unknown keys are ignored so a config
/// synced from a newer version still loads.
#[derive(Clone, Debug, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Config {
    /// GPU for thumbnails: "auto" (the desktop's GPU), "integrated",
    /// "discrete", "software", or part of an adapter name.
    pub gpu: String,
    /// GPU for the viewer, which draws a window and so defaults to the
    /// desktop's GPU.
    pub viewer_gpu: String,
    /// Seconds the helper stays loaded after its last request.
    pub helper_idle_seconds: u64,
    /// Animation files offered in the viewer's "Add animations" menu.
    pub animation_libraries: Vec<PathBuf>,
    /// Seconds the viewer stays loaded, without a window, after its last
    /// window closes, so the next `slerp view` opens at once; 0 turns it
    /// off.
    pub viewer_keep_alive_seconds: u64,
    /// Whether the thumbnail helper, when it starts, also starts a viewer
    /// that stays loaded without a window for as long as the helper runs,
    /// so the first file opened from the file manager opens at once
    /// (Linux).
    pub viewer_with_helper: bool,
    /// Volumes (NIfTI, NRRD, TIFF stacks, HDF5 arrays) up to this many
    /// megabytes are loaded into memory by the viewer so scrolling through
    /// slices and switching planes is instant; bigger ones are read from
    /// their file a slice at a time.
    pub volume_memory_mb: u64,
    /// Animated pictures (GIF, APNG, WebP, AVIF) keep up to this many
    /// megabytes of decoded frames in memory, so looping and scrubbing cost
    /// no decoding; longer ones decode frames again as they play.
    pub image_memory_mb: u64,
    /// Where Minecraft schematics take their block textures from: empty
    /// for a resource pack beside the schematic or an installed game's
    /// jar, "none" for Slerp's own colours only, or the path of a client
    /// jar, resource pack zip or unpacked pack.  The textures are read when
    /// a schematic loads and never copied.
    pub minecraft_textures: String,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            gpu: "auto".into(),
            viewer_gpu: "auto".into(),
            helper_idle_seconds: 120,
            animation_libraries: Vec::new(),
            viewer_keep_alive_seconds: 30,
            viewer_with_helper: true,
            volume_memory_mb: 1024,
            image_memory_mb: 512,
            minecraft_textures: String::new(),
        }
    }
}

/// The folder of settings: `$XDG_CONFIG_HOME/slerp`, else
/// `~/.config/slerp`.
#[cfg(not(any(windows, target_os = "macos")))]
pub fn config_dir() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("slerp"))
}

/// The folder of things remembered between runs, which are history rather
/// than settings: `$XDG_STATE_HOME/slerp`, else
/// `~/.local/state/slerp`.
#[cfg(not(any(windows, target_os = "macos")))]
pub fn state_dir() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_STATE_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/state")))?;
    Some(base.join("slerp"))
}

/// The folder of settings: `%APPDATA%\Slerp`, which roams with the
/// user's profile.
#[cfg(windows)]
pub fn config_dir() -> Option<PathBuf> {
    Some(dirs::config_dir()?.join("Slerp"))
}

/// The folder of things remembered between runs: `%LOCALAPPDATA%\Slerp`,
/// since recent files name paths on this machine.
#[cfg(windows)]
pub fn state_dir() -> Option<PathBuf> {
    Some(dirs::data_local_dir()?.join("Slerp"))
}

/// The folder of settings: `~/Library/Application Support/Slerp`.
#[cfg(target_os = "macos")]
pub fn config_dir() -> Option<PathBuf> {
    Some(std::env::home_dir()?.join("Library/Application Support/Slerp"))
}

/// The folder of things remembered between runs, the same as
/// [`config_dir`], as macOS apps keep both in Application Support.
#[cfg(target_os = "macos")]
pub fn state_dir() -> Option<PathBuf> {
    config_dir()
}

impl Config {
    /// `config.jsonc` in [`config_dir`].
    pub fn path() -> Option<PathBuf> {
        Some(config_dir()?.join("config.jsonc"))
    }

    /// Reads the config file, falling back to defaults when it's missing or
    /// broken (a thumbnailer must never fail over a typo in settings).  The
    /// `SLERP_GPU` environment variable overrides `gpu`, and
    /// `SLERP_VIEWER_KEEP_ALIVE` overrides `viewerKeepAliveSeconds` (for
    /// timing cold starts), and `SLERP_MINECRAFT_TEXTURES`
    /// `minecraftTextures`.
    pub fn load() -> Config {
        let mut config = Config::path()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|text| Config::parse(&text).ok())
            .unwrap_or_default();
        if let Ok(gpu) = std::env::var("SLERP_GPU")
            && !gpu.is_empty()
        {
            config.gpu = gpu;
        }
        if let Some(mb) = std::env::var("SLERP_VOLUME_MEMORY_MB").ok().and_then(|s| s.trim().parse().ok()) {
            config.volume_memory_mb = mb;
        }
        if let Some(mb) = std::env::var("SLERP_IMAGE_MEMORY_MB").ok().and_then(|s| s.trim().parse().ok()) {
            config.image_memory_mb = mb;
        }
        if let Ok(source) = std::env::var("SLERP_MINECRAFT_TEXTURES") {
            config.minecraft_textures = source;
        }
        if let Some(seconds) = std::env::var("SLERP_VIEWER_KEEP_ALIVE").ok().and_then(|s| s.trim().parse().ok()) {
            config.viewer_keep_alive_seconds = seconds;
        }
        config
    }

    /// `minecraftTextures` as the schematic loader takes it.
    pub fn minecraft_textures(&self) -> slerp_minecraft::TextureSource {
        match self.minecraft_textures.trim() {
            "" | "auto" => slerp_minecraft::TextureSource::Auto,
            "none" | "off" => slerp_minecraft::TextureSource::Off,
            p => slerp_minecraft::TextureSource::Path(PathBuf::from(p)),
        }
    }

    pub fn parse(text: &str) -> Result<Config, serde_json::Error> {
        serde_json::from_str(&strip_jsonc(text))
    }

    /// Sets one number in the config file, keeping everything else in it as
    /// it is, comments included: the value of `key` is replaced where the
    /// key is set, or the key is added at the top.  The file is created
    /// when there is none.
    pub fn save_number(key: &str, value: u64) -> std::io::Result<()> {
        Config::save_value(key, &value.to_string())
    }

    /// Sets one switch in the config file, as [`Config::save_number`] does.
    pub fn save_bool(key: &str, value: bool) -> std::io::Result<()> {
        Config::save_value(key, if value { "true" } else { "false" })
    }

    fn save_value(key: &str, value: &str) -> std::io::Result<()> {
        let path = Config::path().ok_or_else(|| std::io::Error::other("no config folder"))?;
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(e) => return Err(e),
        };
        let edited = set_value(&text, key, value);
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        crate::write_atomic(&path, edited.as_bytes())
    }
}

/// `text` (JSONC) with the number of `key` replaced by `value`, or the key
/// added after the opening brace when it isn't set.  Comments, strings and
/// layout are left alone.
pub fn set_number(text: &str, key: &str, value: u64) -> String {
    set_value(text, key, &value.to_string())
}

/// [`set_number`] for any value written as one word of JSON: a number,
/// `true` or `false`.
pub fn set_value(text: &str, key: &str, value: &str) -> String {
    // 1. Find `"key"` followed by a colon, outside comments and strings.
    let quoted = format!("\"{key}\"");
    let bytes = text.as_bytes();
    let mut i = 0;
    let mut in_string = false;
    let mut first_brace = None;
    while i < bytes.len() {
        let c = bytes[i];
        if in_string {
            match c {
                b'\\' => i += 1,
                b'"' => in_string = false,
                _ => {}
            }
            i += 1;
            continue;
        }
        match c {
            b'/' if bytes.get(i + 1) == Some(&b'/') => {
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
                continue;
            }
            b'/' if bytes.get(i + 1) == Some(&b'*') => {
                i += 2;
                while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                    i += 1;
                }
                i += 2;
                continue;
            }
            b'{' if first_brace.is_none() => first_brace = Some(i),
            b'"' if text[i..].starts_with(&quoted) => {
                // 2. The value after the colon: replace the number there.
                let after = i + quoted.len();
                let rest = &text[after..];
                let colon = rest.len() - rest.trim_start().len();
                if rest.trim_start().starts_with(':') {
                    let start = after + colon + 1;
                    let value_start = start + (text[start..].len() - text[start..].trim_start().len());
                    let end = text[value_start..]
                        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '+'))
                        .map_or(text.len(), |n| value_start + n);
                    return format!("{}{value}{}", &text[..value_start], &text[end..]);
                }
                in_string = true;
            }
            b'"' => in_string = true,
            _ => {}
        }
        i += 1;
    }
    // 3. Not set: add it first in the object, or make the file.
    match first_brace {
        Some(b) => format!("{}\n  \"{key}\": {value},{}", &text[..=b], &text[b + 1..]),
        None => format!("{{\n  \"{key}\": {value}\n}}\n"),
    }
}

/// Turns JSONC into JSON: removes `//` and `/* */` comments outside strings
/// and trailing commas before `}` or `]`.
pub fn strip_jsonc(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    let mut in_string = false;
    while let Some(c) = chars.next() {
        if in_string {
            out.push(c);
            if c == '\\' {
                if let Some(n) = chars.next() {
                    out.push(n);
                }
            } else if c == '"' {
                in_string = false;
            }
            continue;
        }
        match c {
            '"' => {
                in_string = true;
                out.push(c);
            }
            '/' if chars.peek() == Some(&'/') => {
                for n in chars.by_ref() {
                    if n == '\n' {
                        out.push('\n');
                        break;
                    }
                }
            }
            '/' if chars.peek() == Some(&'*') => {
                chars.next();
                let mut prev = ' ';
                for n in chars.by_ref() {
                    if prev == '*' && n == '/' {
                        break;
                    }
                    prev = n;
                }
            }
            _ => out.push(c),
        }
    }
    remove_trailing_commas(&out)
}

fn remove_trailing_commas(json: &str) -> String {
    let bytes: Vec<char> = json.chars().collect();
    let mut out = String::with_capacity(json.len());
    let mut in_string = false;
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        if in_string {
            if c == '\\' && i + 1 < bytes.len() {
                out.push(c);
                out.push(bytes[i + 1]);
                i += 2;
                continue;
            }
            if c == '"' {
                in_string = false;
            }
        } else if c == '"' {
            in_string = true;
        } else if c == ',' {
            let next = bytes[i + 1..].iter().find(|c| !c.is_whitespace());
            if matches!(next, Some('}') | Some(']')) {
                i += 1;
                continue;
            }
        }
        out.push(c);
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_jsonc_with_comments_and_trailing_commas() {
        let text = r#"{
            // the iGPU starts fastest
            "gpu": "integrated", /* inline */
            "url": "http://example.com/a",
            "animationLibraries": ["/a/b.glb",],
        }"#;
        let c = Config::parse(text).unwrap();
        assert_eq!(c.gpu, "integrated");
        assert_eq!(c.viewer_gpu, "auto");
        assert_eq!(c.animation_libraries, vec![PathBuf::from("/a/b.glb")]);
        assert_eq!(c.viewer_keep_alive_seconds, 30);
    }

    #[test]
    fn sets_one_number_and_keeps_the_rest() {
        let text = "{\n  // \"viewerKeepAliveSeconds\": 5 in a comment\n  \"gpu\": \"integrated\",\n  \"viewerKeepAliveSeconds\": 30, // seconds\n}\n";
        let edited = set_number(text, "viewerKeepAliveSeconds", 0);
        assert!(edited.contains("\"viewerKeepAliveSeconds\": 0, // seconds"), "{edited}");
        assert!(edited.contains("5 in a comment"));
        assert_eq!(Config::parse(&edited).unwrap().viewer_keep_alive_seconds, 0);
        // Missing: added, still valid.
        let added = set_number("{ \"gpu\": \"auto\" }", "viewerKeepAliveSeconds", 120);
        let c = Config::parse(&added).unwrap();
        assert_eq!((c.viewer_keep_alive_seconds, c.gpu.as_str()), (120, "auto"));
        // No file at all.
        assert_eq!(Config::parse(&set_number("", "viewerKeepAliveSeconds", 7)).unwrap().viewer_keep_alive_seconds, 7);
        // A string value that mentions the key isn't touched.
        let s = set_number("{\"x\": \"\\\"viewerKeepAliveSeconds\\\": 1\"}", "viewerKeepAliveSeconds", 9);
        assert_eq!(Config::parse(&s).unwrap().viewer_keep_alive_seconds, 9);
    }

    #[test]
    fn sets_a_switch_either_way() {
        let text = "{\n  \"viewerWithHelper\": true, // starts with the helper\n  \"gpu\": \"auto\"\n}\n";
        let off = set_value(text, "viewerWithHelper", "false");
        assert!(off.contains("\"viewerWithHelper\": false, // starts"), "{off}");
        assert!(!Config::parse(&off).unwrap().viewer_with_helper);
        let on = set_value(&off, "viewerWithHelper", "true");
        assert!(Config::parse(&on).unwrap().viewer_with_helper);
        // On by default, and added when missing.
        assert!(Config::parse("{}").unwrap().viewer_with_helper);
        assert!(!Config::parse(&set_value("{}", "viewerWithHelper", "false")).unwrap().viewer_with_helper);
    }
}
