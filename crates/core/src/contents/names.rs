//! File names from archives are untrusted text: they may climb out of the
//! output folder (`..`), start at the root (`/etc/passwd`, `C:\Windows`),
//! use characters or device names a file system refuses (`CON`, `a:b`), end
//! in a dot or space, or run to thousands of bytes.  [`safe_component`] makes
//! one name safe on Linux, Windows and macOS at once, and
//! [`safe_relative_path`] makes a whole path out of safe names, so that
//! joining it to a folder can never leave the folder.  The glob matcher
//! selects items by name.

use std::path::PathBuf;

/// The longest file name written, in bytes (file systems allow 255; the rest
/// is for a `~2` suffix).
const MAX_NAME_BYTES: usize = 200;
/// The deepest folder nesting kept; deeper parts are joined into one name.
const MAX_DEPTH: usize = 32;

/// Windows' reserved device names, which can't be file names with or
/// without an extension.
const RESERVED: &[&str] = &[
    "CON", "PRN", "AUX", "NUL", "CONIN$", "CONOUT$", "COM0", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7",
    "COM8", "COM9", "LPT0", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// One path component made safe: never empty, never `.` or `..`, none of
/// `< > : " / \ | ? *` or control characters, no trailing dot or space, not
/// a Windows device name, at most 200 bytes (the extension is kept).
pub fn safe_component(name: &str) -> String {
    let mut s: String = name
        .chars()
        .map(|c| match c {
            '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect();
    // Windows drops trailing dots and spaces, so "a." and "a" would clash,
    // and leading spaces make trouble in shells.
    let trimmed = s.trim_matches(' ').trim_end_matches(['.', ' ']);
    s = trimmed.to_string();
    if s.is_empty() || s.chars().all(|c| c == '.') {
        return "_".to_string();
    }
    let stem = s.split('.').next().unwrap_or("").trim_end_matches(' ').to_ascii_uppercase();
    if RESERVED.contains(&stem.as_str()) {
        s.insert(0, '_');
    }
    shorten(&s, MAX_NAME_BYTES)
}

/// Cuts a name to `max` bytes at a character boundary, keeping a short
/// extension.
fn shorten(name: &str, max: usize) -> String {
    if name.len() <= max {
        return name.to_string();
    }
    let (stem, ext) = match name.rsplit_once('.') {
        Some((s, e)) if !s.is_empty() && e.len() <= 16 => (s, &name[s.len()..]),
        _ => (name, ""),
    };
    let room = max.saturating_sub(ext.len());
    let mut end = room.min(stem.len());
    while !stem.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}{ext}", &stem[..end])
}

/// A relative path from an archive's path: split at `/` and `\`, every part
/// made safe, empty and `.` parts dropped.  The result is never absolute and
/// has no `..`, so it stays inside any folder it is joined to.  Paths nested
/// deeper than 32 folders keep the first 31 and join the rest into the last
/// name.
pub fn safe_relative_path(path: &str) -> PathBuf {
    let mut parts: Vec<String> =
        path.split(['/', '\\']).filter(|p| !p.is_empty() && *p != ".").map(safe_component).collect();
    if parts.len() > MAX_DEPTH {
        let tail = parts.split_off(MAX_DEPTH - 1).join("_");
        parts.push(shorten(&tail, MAX_NAME_BYTES));
    }
    if parts.is_empty() {
        parts.push("_".into());
    }
    parts.iter().collect()
}

/*┌─────────────────╮
| Globs           |
└─────────────────╯*/
//region Globs

enum Token {
    Literal(char),
    /// `?`: one character other than `/`.
    One,
    /// `*`: any run of characters other than `/`.
    Star,
    /// `**`: any run of characters.
    Globstar,
    Class(Vec<(char, char)>, bool),
}

fn tokens(pattern: &str) -> Vec<Token> {
    let chars: Vec<char> = pattern.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            '*' if chars.get(i + 1) == Some(&'*') => {
                while chars.get(i) == Some(&'*') {
                    i += 1;
                }
                out.push(Token::Globstar);
                continue;
            }
            '*' => out.push(Token::Star),
            '?' => out.push(Token::One),
            '[' => {
                // A class: [abc], [a-z], [!abc].  Without a closing bracket
                // the bracket is a letter.
                let mut j = i + 1;
                let negated = matches!(chars.get(j), Some('!' | '^'));
                if negated {
                    j += 1;
                }
                let start = j;
                let mut ranges = Vec::new();
                while j < chars.len() && (chars[j] != ']' || j == start) {
                    if chars.get(j + 1) == Some(&'-') && chars.get(j + 2).is_some_and(|&c| c != ']') {
                        ranges.push((chars[j].to_ascii_lowercase(), chars[j + 2].to_ascii_lowercase()));
                        j += 3;
                    } else {
                        let c = chars[j].to_ascii_lowercase();
                        ranges.push((c, c));
                        j += 1;
                    }
                }
                if j < chars.len() {
                    out.push(Token::Class(ranges, negated));
                    i = j + 1;
                    continue;
                }
                out.push(Token::Literal('['));
            }
            c => out.push(Token::Literal(c.to_ascii_lowercase())),
        }
        i += 1;
    }
    out
}

/// Whether `text` matches `pattern`, without regard to case (Doom names are
/// compared that way).  `*` matches within a path part, `**` across parts,
/// `?` one character, `[a-z]` and `[!a-z]` classes.
pub fn glob_match(pattern: &str, text: &str) -> bool {
    let tokens = tokens(pattern);
    let text: Vec<char> = text.chars().map(|c| c.to_ascii_lowercase()).collect();
    // reach[j]: the first j tokens can match the text so far.
    let mut reach = vec![false; tokens.len() + 1];
    reach[0] = true;
    // Leading globstars and stars match the empty text.
    let settle = |reach: &mut Vec<bool>| {
        for (j, t) in tokens.iter().enumerate() {
            if matches!(t, Token::Star | Token::Globstar) && reach[j] {
                reach[j + 1] = true;
            }
        }
    };
    settle(&mut reach);
    for &c in &text {
        let mut next = vec![false; tokens.len() + 1];
        for (j, t) in tokens.iter().enumerate() {
            if !reach[j] {
                // A star that already matched can go on matching.
                continue;
            }
            match t {
                Token::Literal(l) if *l == c => next[j + 1] = true,
                Token::One if c != '/' => next[j + 1] = true,
                Token::Class(ranges, negated) if c != '/' => {
                    let inside = ranges.iter().any(|&(a, b)| a <= c && c <= b);
                    if inside != *negated {
                        next[j + 1] = true;
                    }
                }
                _ => {}
            }
        }
        for (j, t) in tokens.iter().enumerate() {
            // A star consumes the character and stays where it is.
            if reach[j + 1] {
                match t {
                    Token::Star if c != '/' => next[j + 1] = true,
                    Token::Globstar => next[j + 1] = true,
                    _ => {}
                }
            }
        }
        settle(&mut next);
        reach = next;
    }
    reach[tokens.len()]
}

//endregion
