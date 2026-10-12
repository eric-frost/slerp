//! A thin, streaming layer over quick-xml for the XML track formats: the
//! handler sees each element's start (with its path of local names, such
//! as `gpx/trk/trkseg/trkpt`, namespace prefixes dropped) and its end with
//! the text it held.  Broken XML ends the walk where it breaks, keeping
//! what was read.

use quick_xml::events::{BytesStart, Event};
use std::io::BufRead;

/// The most points a reader keeps from one file.  A day of recording at one
/// fix a second is 86,000; a file with millions is damaged or hostile, and
/// each point costs 56 bytes before it is thinned.
pub const MAX_POINTS: usize = 8_000_000;

/// The most text one element may hold (a KML line's coordinates can run
/// to megabytes).
const MAX_TEXT: usize = 256 << 20;

pub trait Handler {
    /// An element opened.  `path` ends with its own name.
    fn start(&mut self, path: &[u8], e: &BytesStart);
    /// An element closed, with its text (trimmed only of the outside
    /// whitespace by the handler, if it wants).
    fn end(&mut self, path: &[u8], text: &str);
}

/// Walks the document, returning a warning if it was cut short.
pub fn walk<R: BufRead>(reader: R, h: &mut impl Handler) -> Option<String> {
    let mut reader = quick_xml::Reader::from_reader(reader);
    reader.config_mut().check_end_names = false;
    let mut buf = Vec::new();
    let mut path: Vec<u8> = Vec::new();
    let mut lengths: Vec<usize> = Vec::new();
    let mut text = String::new();
    let mut warning = None;
    loop {
        buf.clear();
        let event = match reader.read_event_into(&mut buf) {
            Ok(Event::Eof) => break,
            Ok(e) => e,
            Err(e) => {
                warning = Some(format!("XML error: {e}"));
                break;
            }
        };
        match event {
            Event::Start(e) => {
                open(&mut path, &mut lengths, &e);
                text.clear();
                h.start(&path, &e);
            }
            Event::Empty(e) => {
                open(&mut path, &mut lengths, &e);
                text.clear();
                h.start(&path, &e);
                h.end(&path, "");
                close(&mut path, &mut lengths);
            }
            Event::End(_) => {
                h.end(&path, &text);
                text.clear();
                close(&mut path, &mut lengths);
            }
            Event::Text(t) => push_text(&mut text, &String::from_utf8_lossy(t.as_ref())),
            Event::CData(t) => push_text(&mut text, &String::from_utf8_lossy(t.as_ref())),
            Event::GeneralRef(r) => {
                if let Some(c) = entity(&String::from_utf8_lossy(r.as_ref())) {
                    let mut tmp = [0u8; 4];
                    push_text(&mut text, c.encode_utf8(&mut tmp));
                }
            }
            _ => {}
        }
    }
    warning
}

fn push_text(text: &mut String, s: &str) {
    if text.len() + s.len() <= MAX_TEXT {
        text.push_str(s);
    }
}

fn open(path: &mut Vec<u8>, lengths: &mut Vec<usize>, e: &BytesStart) {
    lengths.push(path.len());
    if !path.is_empty() {
        path.push(b'/');
    }
    path.extend_from_slice(e.local_name().as_ref());
}

fn close(path: &mut Vec<u8>, lengths: &mut Vec<usize>) {
    let len = lengths.pop().unwrap_or(0);
    path.truncate(len);
}

/// The character an entity reference stands for.
fn entity(name: &str) -> Option<char> {
    match name {
        "amp" => Some('&'),
        "lt" => Some('<'),
        "gt" => Some('>'),
        "quot" => Some('"'),
        "apos" => Some('\''),
        _ => {
            let n = name.strip_prefix('#')?;
            let code = match n.strip_prefix(['x', 'X']) {
                Some(hex) => u32::from_str_radix(hex, 16).ok()?,
                None => n.parse().ok()?,
            };
            char::from_u32(code)
        }
    }
}

/// The last element name in a path.
pub fn leaf(path: &[u8]) -> &[u8] {
    match path.iter().rposition(|&b| b == b'/') {
        Some(i) => &path[i + 1..],
        None => path,
    }
}

/// The element above the last in a path.
pub fn parent(path: &[u8]) -> &[u8] {
    match path.iter().rposition(|&b| b == b'/') {
        Some(i) => leaf(&path[..i]),
        None => b"",
    }
}

/// Whether the path contains an element of this name.
pub fn within(path: &[u8], name: &[u8]) -> bool {
    path.split(|&b| b == b'/').any(|p| p == name)
}

/// An attribute's value by local name, entities resolved.
pub fn attr(e: &BytesStart, name: &[u8]) -> Option<String> {
    for a in e.attributes().with_checks(false).flatten() {
        if a.key.local_name().as_ref() == name {
            return Some(unescape(&String::from_utf8_lossy(&a.value)));
        }
    }
    None
}

/// Resolves `&amp;` and the other entity references in attribute text.
fn unescape(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        match rest.find(';').filter(|&j| j <= 12).and_then(|j| entity(&rest[1..j]).map(|c| (c, j))) {
            Some((c, j)) => {
                out.push(c);
                rest = &rest[j + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// A number from element text or an attribute.
pub fn number(s: &str) -> Option<f64> {
    let v: f64 = s.trim().parse().ok()?;
    v.is_finite().then_some(v)
}
