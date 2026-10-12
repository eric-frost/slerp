//! Editing `[Section]` and `key=value` files in place: freedesktop's
//! `mimeapps.list`, KDE's `dolphinrc`, `.thumbnailer` and `.desktop`
//! files.  Only the lines that change are touched, so comments, blank lines,
//! order, spacing and line endings of everything else stay byte for byte,
//! and an edit undone leaves the file as it was.

/// A parsed file.  Keys before the first section belong to the section
/// named `""` (KDE's default group).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ini {
    lines: Vec<String>,
    /// Whether the text ended with a line break.
    final_newline: bool,
    /// The line break the file uses.
    newline: &'static str,
}

/// What a line is.
enum Line<'a> {
    Section(&'a str),
    Entry { key: &'a str, value: &'a str },
    Other,
}

fn classify(line: &str) -> Line<'_> {
    let t = line.trim();
    if t.starts_with('[') && t.ends_with(']') && t.len() >= 2 {
        return Line::Section(&t[1..t.len() - 1]);
    }
    if t.starts_with('#') || t.starts_with(';') || t.is_empty() {
        return Line::Other;
    }
    match t.split_once('=') {
        Some((k, v)) => Line::Entry { key: k.trim(), value: v.trim_start() },
        None => Line::Other,
    }
}

impl Ini {
    pub fn parse(text: &str) -> Ini {
        let newline = if text.contains("\r\n") { "\r\n" } else { "\n" };
        let final_newline = text.ends_with('\n');
        let body = text.strip_suffix(newline).or_else(|| text.strip_suffix('\n')).unwrap_or(text);
        let lines = if text.is_empty() { Vec::new() } else { body.split(newline).map(str::to_owned).collect() };
        Ini { lines, final_newline: final_newline || text.is_empty(), newline }
    }

    pub fn to_text(&self) -> String {
        let mut out = self.lines.join(self.newline);
        if self.final_newline && !self.lines.is_empty() {
            out.push_str(self.newline);
        }
        out
    }

    /// The line ranges of every occurrence of `section`: from its header
    /// (or the file's start, for `""`) to the next header.
    fn spans(&self, section: &str) -> Vec<(Option<usize>, usize, usize)> {
        let mut out = Vec::new();
        let mut current: Option<(Option<usize>, &str, usize)> = Some((None, "", 0));
        for (i, line) in self.lines.iter().enumerate() {
            if let Line::Section(name) = classify(line) {
                if let Some((header, n, start)) = current.take()
                    && n == section
                {
                    out.push((header, start, i));
                }
                current = Some((Some(i), name, i + 1));
            }
        }
        if let Some((header, n, start)) = current
            && n == section
        {
            out.push((header, start, self.lines.len()));
        }
        out
    }

    /// The line of `key` in `section`, the first if it repeats.
    fn find(&self, section: &str, key: &str) -> Option<usize> {
        self.spans(section).into_iter().find_map(|(_, start, end)| {
            (start..end).find(|&i| matches!(classify(&self.lines[i]), Line::Entry { key: k, .. } if k == key))
        })
    }

    pub fn has_section(&self, section: &str) -> bool {
        section.is_empty() || self.spans(section).iter().any(|(h, _, _)| h.is_some())
    }

    pub fn get(&self, section: &str, key: &str) -> Option<String> {
        let i = self.find(section, key)?;
        match classify(&self.lines[i]) {
            Line::Entry { value, .. } => Some(value.trim_end().to_owned()),
            _ => None,
        }
    }

    /// Every section's name, in order, with `""` first for entries before
    /// the first header.
    pub fn sections(&self) -> Vec<String> {
        let mut out = vec![String::new()];
        for line in &self.lines {
            if let Line::Section(name) = classify(line)
                && !out.iter().any(|s| s == name)
            {
                out.push(name.to_owned());
            }
        }
        out
    }

    /// The keys of `section`, in order.
    pub fn keys(&self, section: &str) -> Vec<String> {
        let mut out = Vec::new();
        for (_, start, end) in self.spans(section) {
            for line in &self.lines[start..end] {
                if let Line::Entry { key, .. } = classify(line)
                    && !out.iter().any(|k| k == key)
                {
                    out.push(key.to_owned());
                }
            }
        }
        out
    }

    /// Sets `key` in `section`: the value of its line is replaced, keeping
    /// the key and its spacing; a new key goes after the section's last
    /// entry; a new section goes at the end, after a blank line.
    pub fn set(&mut self, section: &str, key: &str, value: &str) {
        if let Some(i) = self.find(section, key) {
            let line = &self.lines[i];
            let eq = line.find('=').unwrap_or(line.len());
            let after = &line[eq + 1..];
            let space = &after[..after.len() - after.trim_start().len()];
            self.lines[i] = format!("{}={space}{value}", &line[..eq]);
            return;
        }
        let entry = format!("{key}={value}");
        if let Some((_, start, end)) =
            self.spans(section).into_iter().find(|(h, _, _)| h.is_some() || section.is_empty())
        {
            let last = (start..end).rev().find(|&i| matches!(classify(&self.lines[i]), Line::Entry { .. }));
            let at = match last {
                Some(i) => i + 1,
                // An empty section: right under its header.
                None => start,
            };
            self.lines.insert(at, entry);
            return;
        }
        if self.lines.last().is_some_and(|l| !l.trim().is_empty()) {
            self.lines.push(String::new());
        }
        self.lines.push(format!("[{section}]"));
        self.lines.push(entry);
        self.final_newline = true;
    }

    /// Removes `key` from `section`; true when it was there.
    pub fn remove(&mut self, section: &str, key: &str) -> bool {
        match self.find(section, key) {
            Some(i) => {
                self.lines.remove(i);
                true
            }
            None => false,
        }
    }

    /// Removes `section` when it has no entries left (comments count as
    /// nothing), with the blank line before its header.
    pub fn remove_section_if_empty(&mut self, section: &str) {
        if section.is_empty() {
            return;
        }
        let spans = self.spans(section);
        for (header, start, end) in spans.into_iter().rev() {
            let Some(h) = header else { continue };
            if (start..end).any(|i| matches!(classify(&self.lines[i]), Line::Entry { .. })) {
                continue;
            }
            let from = if h > 0 && self.lines[h - 1].trim().is_empty() { h - 1 } else { h };
            // Keep blank lines that separate it from the next section.
            let to = (start..end).rev().find(|&i| !self.lines[i].trim().is_empty()).map_or(start, |i| i + 1);
            self.lines.drain(from..to.max(h + 1));
        }
    }
}

/// The items of a list value split at `sep` (`;` in freedesktop files,
/// `,` in KDE's), without the empty item a trailing separator leaves.
pub fn list(value: &str, sep: char) -> Vec<String> {
    value.split(sep).map(str::trim).filter(|s| !s.is_empty()).map(str::to_owned).collect()
}

/// `items` joined into a list value; freedesktop lists end with the
/// separator, KDE's don't.
pub fn join(items: &[String], sep: char, trailing: bool) -> String {
    let mut out = items.join(&sep.to_string());
    if trailing && !items.is_empty() {
        out.push(sep);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const DOLPHIN: &str = "MenuBar=Disabled\n\n[General]\nShowFullPath=true\n\n[PreviewSettings]\nPlugins=a,b\n\n[VersionControl]\nenabledPlugins=Git\n";

    #[test]
    fn untouched_text_round_trips() {
        for text in [DOLPHIN, "", "no newline", "# c\r\n[A]\r\nk = v\r\n", "\n\n[X]\n"] {
            assert_eq!(Ini::parse(text).to_text(), text, "{text:?}");
        }
    }

    #[test]
    fn edits_touch_only_their_lines_and_undo_exactly() {
        let mut ini = Ini::parse(DOLPHIN);
        assert_eq!(ini.get("PreviewSettings", "Plugins").as_deref(), Some("a,b"));
        assert_eq!(ini.get("", "MenuBar").as_deref(), Some("Disabled"));
        ini.set("PreviewSettings", "Plugins", "a,b,slerp");
        assert_eq!(ini.to_text(), DOLPHIN.replace("a,b", "a,b,slerp"));
        ini.set("PreviewSettings", "Plugins", "a,b");
        assert_eq!(ini.to_text(), DOLPHIN);
        // A new key, then a new section, both undone.
        ini.set("General", "New", "1");
        ini.set("Default Applications", "model/gltf-binary", "slerp.desktop;");
        assert!(ini.to_text().contains("ShowFullPath=true\nNew=1\n\n[PreviewSettings]"));
        assert!(
            ini.to_text().ends_with("enabledPlugins=Git\n\n[Default Applications]\nmodel/gltf-binary=slerp.desktop;\n")
        );
        ini.remove("General", "New");
        ini.remove("Default Applications", "model/gltf-binary");
        ini.remove_section_if_empty("Default Applications");
        assert_eq!(ini.to_text(), DOLPHIN);
        // Spacing and line endings are kept.
        let mut crlf = Ini::parse("[A]\r\nk = v\r\n");
        crlf.set("A", "k", "w");
        crlf.set("A", "j", "x");
        assert_eq!(crlf.to_text(), "[A]\r\nk = w\r\nj=x\r\n");
        // A file that didn't exist gets one section.
        let mut new = Ini::parse("");
        new.set("Default Applications", "a/b", "c;");
        assert_eq!(new.to_text(), "[Default Applications]\na/b=c;\n");
    }

    #[test]
    fn lists() {
        assert_eq!(list("a.desktop;b.desktop;", ';'), ["a.desktop", "b.desktop"]);
        assert_eq!(join(&list("a,b", ','), ',', false), "a,b");
        assert_eq!(join(&["x".into()], ';', true), "x;");
    }
}
