//! Text as documents store it: PDF text strings (UTF-16 with a byte-order
//! mark, UTF-8 with one, or PDFDocEncoding) and PostScript's comment
//! values.

/// PDFDocEncoding's characters from 0x18 to 0x1F and 0x80 to 0x9F, where it
/// differs from Latin-1 (PDF 32000-1, annex D).
const DOC_LOW: [char; 8] = ['˘', 'ˇ', 'ˆ', '˙', '˝', '˛', '˚', '˜'];
const DOC_HIGH: [char; 32] = [
    '•', '†', '‡', '…', '—', '–', 'ƒ', '⁄', '‹', '›', '−', '‰', '„', '“', '”', '‘', '’', '‚', '™', 'ﬁ', 'ﬂ', 'Ł', 'Œ',
    'Š', 'Ÿ', 'Ž', 'ı', 'ł', 'œ', 'š', 'ž', '\u{fffd}',
];

/// Decodes a PDF text string, the way titles and authors are stored.
pub fn pdf_text(bytes: &[u8]) -> String {
    let text = if let Some(rest) = bytes.strip_prefix(&[0xFE, 0xFF]) {
        let units: Vec<u16> = rest.chunks_exact(2).map(|c| u16::from_be_bytes([c[0], c[1]])).collect();
        String::from_utf16_lossy(&units)
    } else if let Some(rest) = bytes.strip_prefix(&[0xFF, 0xFE]) {
        // Not allowed, but some writers do it.
        let units: Vec<u16> = rest.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        String::from_utf16_lossy(&units)
    } else if let Some(rest) = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        String::from_utf8_lossy(rest).into_owned()
    } else {
        bytes
            .iter()
            .map(|&b| match b {
                0x18..=0x1F => DOC_LOW[(b - 0x18) as usize],
                0x80..=0x9F => DOC_HIGH[(b - 0x80) as usize],
                _ => b as char,
            })
            .collect()
    };
    tidy(&text)
}

/// Text fit for one line of the inspector: control characters gone (some
/// files end their strings with NULs), spaces collapsed, length bounded.
pub fn tidy(text: &str) -> String {
    let mut out = String::new();
    for word in text.split(|c: char| c.is_whitespace() || c.is_control()).filter(|w| !w.is_empty()) {
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(word);
        if out.chars().count() > 300 {
            let cut: String = out.chars().take(300).collect();
            return format!("{cut}…");
        }
    }
    out
}

/// A PDF date as people read it: "2024-03-05 14:20 +01:00".
pub fn pdf_date(d: &hayro::hayro_syntax::object::DateTime) -> String {
    let zone = if d.utc_offset_hour == 0 && d.utc_offset_minute == 0 {
        String::new()
    } else {
        let sign = if d.utc_offset_hour < 0 { '-' } else { '+' };
        format!(" {sign}{:02}:{:02}", d.utc_offset_hour.unsigned_abs(), d.utc_offset_minute)
    };
    format!("{:04}-{:02}-{:02} {:02}:{:02}{zone}", d.year, d.month, d.day, d.hour, d.minute)
}

/// A PostScript comment's value: `(text)` strings lose their parentheses
/// and backslash escapes; anything else is kept as it stands.  Old files
/// are Latin-1 or Mac Roman more often than UTF-8.
pub fn ps_text(raw: &[u8]) -> String {
    let raw = trim_bytes(raw);
    let inner = match raw.strip_prefix(b"(").and_then(|r| r.strip_suffix(b")")) {
        Some(inner) => {
            let mut out = Vec::with_capacity(inner.len());
            let mut it = inner.iter().copied().peekable();
            while let Some(b) = it.next() {
                if b != b'\\' {
                    out.push(b);
                    continue;
                }
                match it.next() {
                    Some(b'n') => out.push(b' '),
                    Some(b'r' | b't') => out.push(b' '),
                    Some(d @ b'0'..=b'7') => {
                        let mut v = (d - b'0') as u32;
                        for _ in 0..2 {
                            match it.peek() {
                                Some(&n @ b'0'..=b'7') => {
                                    v = v * 8 + (n - b'0') as u32;
                                    it.next();
                                }
                                _ => break,
                            }
                        }
                        out.push(v as u8);
                    }
                    Some(other) => out.push(other),
                    None => {}
                }
            }
            out
        }
        None => raw.to_vec(),
    };
    let text = match std::str::from_utf8(&inner) {
        Ok(s) => s.to_string(),
        Err(_) => inner.iter().map(|&b| b as char).collect(),
    };
    tidy(&text)
}

pub fn trim_bytes(b: &[u8]) -> &[u8] {
    let start = b.iter().position(|c| !c.is_ascii_whitespace()).unwrap_or(b.len());
    let end = b.iter().rposition(|c| !c.is_ascii_whitespace()).map_or(start, |e| e + 1);
    &b[start..end.max(start)]
}
