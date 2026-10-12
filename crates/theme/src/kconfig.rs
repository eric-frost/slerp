//! A reader for KDE's INI dialect (KConfig), which `kdeglobals`, colour
//! scheme files and icon themes' `index.theme` all use.
//!
//! KConfig differs from plain INI in ways that matter here: group headers can
//! nest (`[Colors:Header][Inactive]`), keys can carry flags (`key[$e]` expands
//! environment variables, `[$i]` locks a key) or a locale (`Name[de]`),
//! values use backslash escapes, and a setting is the result of cascading
//! several files, user over system.  See KConfig's `kconfigini.cpp` for the
//! reference behaviour.

use std::collections::HashMap;
use std::path::Path;

/// Files larger than this are not configuration files, so they are skipped
/// rather than read into memory.
const MAX_FILE_BYTES: u64 = 4 << 20;

/// Parsed KConfig data: groups of key/value pairs.  Nested group names are
/// stored the way they are written, `Colors:Header][Inactive`.
#[derive(Clone, Debug, Default)]
pub struct KConfig {
    groups: HashMap<String, HashMap<String, String>>,
}

impl KConfig {
    /// Parses KConfig text.  Malformed lines are skipped, as KConfig itself
    /// does, so this never fails.
    pub fn parse(text: &str) -> KConfig {
        let mut config = KConfig::default();
        config.merge_text(text);
        config
    }

    /// Reads and parses a file, or returns `None` when it is missing,
    /// unreadable or implausibly large.
    pub fn load(path: &Path) -> Option<KConfig> {
        let mut config = KConfig::default();
        config.merge_file(path).then_some(config)
    }

    /// Reads several files as one cascade: later files override earlier
    /// ones, the way a user's `~/.config/kdeglobals` overrides
    /// `/etc/xdg/kdeglobals`.  Missing files are skipped.
    pub fn cascade<P: AsRef<Path>>(paths: &[P]) -> KConfig {
        let mut config = KConfig::default();
        for path in paths {
            config.merge_file(path.as_ref());
        }
        config
    }

    /// Overlays a file on this configuration.  Returns whether it was read.
    pub fn merge_file(&mut self, path: &Path) -> bool {
        let Ok(meta) = std::fs::metadata(path) else { return false };
        if !meta.is_file() || meta.len() > MAX_FILE_BYTES {
            return false;
        }
        let Ok(bytes) = std::fs::read(path) else { return false };
        self.merge_text(&String::from_utf8_lossy(&bytes));
        true
    }

    /// Overlays parsed text on this configuration.
    pub fn merge_text(&mut self, text: &str) {
        // Entries before the first header belong to KConfig's "<default>"
        // group, which nothing here reads, but keep them for completeness.
        let mut group = String::from("<default>");
        for raw in text.lines() {
            let line = raw.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if line.starts_with('[') {
                if let Some(name) = parse_group_header(line) {
                    group = name;
                }
                continue;
            }
            let Some((raw_key, raw_value)) = line.split_once('=') else { continue };
            let Some((key, expand)) = parse_key(raw_key.trim()) else { continue };
            let mut value = unescape(raw_value.trim());
            if expand {
                value = expand_env(&value);
            }
            self.groups.entry(group.clone()).or_default().insert(key, value);
        }
    }

    /// The raw string value of `key` in `group`.
    pub fn get(&self, group: &str, key: &str) -> Option<&str> {
        self.groups.get(group)?.get(key).map(String::as_str)
    }

    /// Whether the group exists, even if empty of known keys.
    pub fn has_group(&self, group: &str) -> bool {
        self.groups.contains_key(group)
    }

    /// The names of all groups.
    pub fn group_names(&self) -> impl Iterator<Item = &str> {
        self.groups.keys().map(String::as_str)
    }

    /// A boolean the way KConfig reads one: `true`, `on`, `yes` or a
    /// non-zero number.
    pub fn get_bool(&self, group: &str, key: &str) -> Option<bool> {
        let value = self.get(group, key)?.trim().to_ascii_lowercase();
        match value.as_str() {
            "true" | "on" | "yes" => Some(true),
            "false" | "off" | "no" | "" => Some(false),
            other => other.parse::<f64>().ok().map(|n| n != 0.0),
        }
    }

    pub fn get_f32(&self, group: &str, key: &str) -> Option<f32> {
        self.get(group, key)?.trim().parse::<f32>().ok().filter(|v| v.is_finite())
    }

    pub fn get_u32(&self, group: &str, key: &str) -> Option<u32> {
        self.get(group, key)?.trim().parse::<u32>().ok()
    }

    /// A comma-separated list, as KConfig writes `QStringList`s.  Escaped
    /// commas (`\,`) stay inside their item.
    pub fn get_list(&self, group: &str, key: &str) -> Vec<String> {
        let Some(value) = self.get(group, key) else { return Vec::new() };
        split_list(value)
    }
}

/// Splits a KConfig list value on unescaped commas, dropping empty items.
pub fn split_list(value: &str) -> Vec<String> {
    let mut items = Vec::new();
    let mut current = String::new();
    let mut chars = value.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                if let Some(next) = chars.next() {
                    current.push(next);
                }
            }
            ',' => {
                let item = current.trim().to_owned();
                if !item.is_empty() {
                    items.push(item);
                }
                current.clear();
            }
            _ => current.push(c),
        }
    }
    let item = current.trim().to_owned();
    if !item.is_empty() {
        items.push(item);
    }
    items
}

/// Parses `[A][B]`, returning `A][B`.  Bracketed flags such as `[$i]` are
/// dropped.  Returns `None` for a header with no name.
fn parse_group_header(line: &str) -> Option<String> {
    let mut parts = Vec::new();
    let mut rest = line;
    while let Some(start) = rest.find('[') {
        let after = &rest[start + 1..];
        let end = after.find(']')?;
        let part = &after[..end];
        if !part.starts_with('$') && !part.is_empty() {
            parts.push(unescape(part));
        }
        rest = &after[end + 1..];
    }
    if parts.is_empty() { None } else { Some(parts.join("][")) }
}

/// Splits `key[$e][de]` into the bare key and whether `$e` asks for
/// environment expansion.  Localised keys (`Name[de]`) are skipped, since
/// the theme only needs the untranslated values.
fn parse_key(raw: &str) -> Option<(String, bool)> {
    let (name, mut suffix) = match raw.find('[') {
        Some(i) => (&raw[..i], &raw[i..]),
        None => (raw, ""),
    };
    let name = name.trim();
    if name.is_empty() {
        return None;
    }
    let mut expand = false;
    while let Some(after) = suffix.strip_prefix('[') {
        let end = after.find(']')?;
        let part = &after[..end];
        if let Some(flags) = part.strip_prefix('$') {
            expand |= flags.contains('e');
        } else {
            return None;
        }
        suffix = &after[end + 1..];
    }
    Some((name.to_owned(), expand))
}

/// Undoes KConfig's escapes: `\s`, `\t`, `\n`, `\r`, `\\`, `\;`, `\,` and
/// `\xNN`.
fn unescape(value: &str) -> String {
    if !value.contains('\\') {
        return value.to_owned();
    }
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('s') => out.push(' '),
            Some('t') => out.push('\t'),
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some('x') => {
                let hex: String = (0..2).filter_map(|_| chars.next_if(|c| c.is_ascii_hexdigit())).collect();
                match u8::from_str_radix(&hex, 16) {
                    Ok(byte) if byte.is_ascii() => out.push(byte as char),
                    _ => out.push('?'),
                }
            }
            // `\\`, `\;`, `\,` and anything unknown keep the character.
            Some(other) => out.push(other),
            None => out.push('\\'),
        }
    }
    out
}

/// Expands `$VAR` and `${VAR}` in a `[$e]` value, as KConfig does.  Shell
/// substitutions (`$(command)`) are left alone: running commands to read a
/// colour would be a surprising side effect.
fn expand_env(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut rest = value;
    while let Some(i) = rest.find('$') {
        out.push_str(&rest[..i]);
        let after = &rest[i + 1..];
        let (name, consumed) = if let Some(braced) = after.strip_prefix('{') {
            match braced.find('}') {
                Some(end) => (&braced[..end], end + 2),
                None => ("", 0),
            }
        } else {
            let end = after.find(|c: char| !(c.is_ascii_alphanumeric() || c == '_')).unwrap_or(after.len());
            (&after[..end], end)
        };
        if name.is_empty() {
            out.push('$');
            rest = after;
            continue;
        }
        out.push_str(&std::env::var(name).unwrap_or_default());
        rest = &after[consumed..];
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn groups_keys_and_cascade_order() {
        let mut config = KConfig::parse("[General]\nfont=A,10\n\n# comment\n[Colors:Header][Inactive]\nX=1\n");
        config.merge_text("[General]\nfont=B,11\n");
        assert_eq!(config.get("General", "font"), Some("B,11"));
        assert_eq!(config.get("Colors:Header][Inactive", "X"), Some("1"));
        assert!(config.get("Colors:Header", "X").is_none());
    }

    #[test]
    fn flags_locales_and_escapes() {
        // SAFETY: tests in this module are the only readers of this variable.
        unsafe { std::env::set_var("SLERP_KCONFIG_TEST", "/x") };
        let config = KConfig::parse(
            "[G]\nName=Plain\nName[de]=Deutsch\npath[$e]=${SLERP_KCONFIG_TEST}/y\nlocked[$i]=1\nsp=\\sa\\tb\\\\\n",
        );
        assert_eq!(config.get("G", "Name"), Some("Plain"));
        assert_eq!(config.get("G", "path"), Some("/x/y"));
        assert_eq!(config.get("G", "locked"), Some("1"));
        assert_eq!(config.get("G", "sp"), Some(" a\tb\\"));
    }

    #[test]
    fn garbage_never_panics() {
        for text in
            ["[", "]", "[[]]", "=", "[$i]\n=x", "a[=b", "a[$e]=$", "a[$e]=${", "k=\\x", "k=\\xZZ", "\u{0}\u{ffff}"]
        {
            let _ = KConfig::parse(text);
        }
        assert_eq!(split_list("a,b\\,c,,d"), vec!["a", "b,c", "d"]);
    }
}
