//! Roblox's XML format (`.rbxlx`, `.rbxmx`, and `.rbxl` files from before
//! 2013), following rbx-dom's specification
//! (<https://dom.rojo.space/xml>, MIT).
//!
//! `<roblox>` holds nested `<Item class="Part">` elements, each with a
//! `<Properties>` element whose children are one property each: the element
//! names the type (`Vector3`, `CoordinateFrame`, `token`, ...) and its `name`
//! attribute the property.  Shared strings come last, in `<SharedStrings>`,
//! named by a key the properties refer to.
//!
//! The file is read as a stream of events with an explicit stack, so a
//! place of any size never sits in memory as a document tree and deep
//! nesting can't overflow the call stack (rbx-dom's reader recurses per
//! level and overflows a 2 MB thread stack on a 300 KB file).  Text kept for
//! a property is capped, and binary strings count against the budget.

use crate::Budget;
use crate::dom::{CFrame, Dom, Instance, Key, Props, Value};
use base64::Engine;
use glam::Vec3;
use quick_xml::events::{BytesStart, Event};
use slerp_scene::LoadError;
use std::collections::HashMap;

/// The most text kept for a property that isn't a binary string or a
/// name: numbers and asset URLs are far shorter.
const MAX_TEXT: usize = 4096;
/// The most bytes of a name kept.
const MAX_NAME: usize = 256;

/// Recognises the `<roblox` root element, after an optional byte order
/// mark, XML declaration and comments.
pub(crate) fn sniff(head: &[u8]) -> bool {
    let mut s = head.strip_prefix(b"\xef\xbb\xbf").unwrap_or(head);
    loop {
        s = s.trim_ascii_start();
        if s.starts_with(b"<?") {
            let Some(end) = find(s, b"?>") else { return false };
            s = &s[end + 2..];
        } else if s.starts_with(b"<!--") {
            let Some(end) = find(s, b"-->") else { return false };
            s = &s[end + 3..];
        } else {
            break;
        }
    }
    s.starts_with(b"<roblox") && s.get(7).is_some_and(|&c| c.is_ascii_whitespace() || c == b'>')
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

/// A property element being read: its type (the element's name), its
/// property name, and the text of the element and of its children
/// (`<X>`, `<R00>`, `<url>`, ...).
struct Prop {
    tag: Vec<u8>,
    name: String,
    /// Nesting below the property element.
    depth: u32,
    text: String,
    /// The child element whose text is being read, and the children read.
    field: Vec<u8>,
    field_text: String,
    fields: Vec<(Vec<u8>, String)>,
    /// Whether the text may grow past `MAX_TEXT` (binary strings).
    long: bool,
}

pub(crate) fn read(bytes: &[u8], budget: &mut Budget) -> Result<Dom, LoadError> {
    let mut reader = quick_xml::Reader::from_reader(bytes);
    reader.config_mut().check_end_names = false;
    let mut dom = Dom::default();
    let mut classes: HashMap<String, u32> = HashMap::new();
    // Shared strings by key, numbered in order of first mention.
    let mut shared_keys: HashMap<String, u32> = HashMap::new();
    let mut items: Vec<usize> = Vec::new();
    let mut in_props = false;
    let mut prop: Option<Prop> = None;
    // The shared string definition being read: its key and text.
    let mut shared: Option<(String, String)> = None;
    let mut root = false;
    loop {
        let event = match reader.read_event() {
            Ok(Event::Eof) => break,
            Ok(e) => e,
            Err(e) => {
                if dom.instances.is_empty() {
                    return Err(LoadError::invalid(format!("XML: {e}")));
                }
                dom.warnings.push(format!("the XML breaks off ({e}); what came before is shown"));
                break;
            }
        };
        match event {
            Event::Start(e) | Event::Empty(e) if !root => {
                if e.local_name().as_ref() != b"roblox" {
                    return Err(LoadError::NotMine);
                }
                root = true;
            }
            Event::Start(e) => {
                if let Some(p) = prop.as_mut() {
                    p.depth += 1;
                    p.field = e.local_name().as_ref().to_vec();
                    p.field_text.clear();
                    continue;
                }
                match e.local_name().as_ref() {
                    b"Item" => {
                        let index = new_item(&mut dom, &mut classes, &e, items.last().copied(), budget)?;
                        items.push(index);
                        in_props = false;
                    }
                    b"Properties" if !items.is_empty() => in_props = true,
                    b"SharedString" if !in_props => {
                        let key = attr(&e, b"md5").unwrap_or_default();
                        shared = Some((key, String::new()));
                    }
                    tag if in_props => prop = Some(new_prop(tag, &e)),
                    _ => {}
                }
            }
            Event::Empty(e) => {
                if let Some(p) = prop.as_mut() {
                    // `<null></null>` written short, or an empty child.
                    p.fields.push((e.local_name().as_ref().to_vec(), String::new()));
                    continue;
                }
                match e.local_name().as_ref() {
                    b"Item" => {
                        new_item(&mut dom, &mut classes, &e, items.last().copied(), budget)?;
                    }
                    tag if in_props => {
                        let p = new_prop(tag, &e);
                        if let Some(&i) = items.last() {
                            finish(&mut dom, i, p, &mut shared_keys, budget)?;
                        }
                    }
                    _ => {}
                }
            }
            Event::End(e) => {
                if let Some(p) = prop.as_mut() {
                    if p.depth > 0 {
                        p.depth -= 1;
                        let text = std::mem::take(&mut p.field_text);
                        let field = std::mem::take(&mut p.field);
                        p.fields.push((field, text));
                        continue;
                    }
                    if let (Some(p), Some(&i)) = (prop.take(), items.last()) {
                        finish(&mut dom, i, p, &mut shared_keys, budget)?;
                    }
                    continue;
                }
                match e.local_name().as_ref() {
                    b"Item" => {
                        items.pop();
                        in_props = false;
                    }
                    b"Properties" => in_props = false,
                    b"SharedString" => {
                        if let Some((key, text)) = shared.take() {
                            let index = shared_index(&mut dom, &mut shared_keys, key.trim());
                            let data = decode_base64(&text);
                            budget.take_bytes(data.len() as u64)?;
                            dom.shared[index as usize] = data;
                        }
                    }
                    _ => {}
                }
            }
            Event::Text(t) => push_text(&mut prop, &mut shared, &String::from_utf8_lossy(t.as_ref()), budget)?,
            Event::CData(t) => push_text(&mut prop, &mut shared, &String::from_utf8_lossy(t.as_ref()), budget)?,
            Event::GeneralRef(r) => {
                if let Some(c) = entity(&String::from_utf8_lossy(r.as_ref())) {
                    push_text(&mut prop, &mut shared, c.encode_utf8(&mut [0; 4]), budget)?;
                }
            }
            _ => {}
        }
    }
    if !root {
        return Err(LoadError::NotMine);
    }
    dom.link();
    Ok(dom)
}

/// Adds the instance an `<Item>` element starts.
fn new_item(
    dom: &mut Dom,
    classes: &mut HashMap<String, u32>,
    e: &BytesStart,
    parent: Option<usize>,
    budget: &mut Budget,
) -> Result<usize, LoadError> {
    budget.take_instances(1)?;
    let class = attr(e, b"class").unwrap_or_default();
    let class = dom.class_index(&class, classes);
    dom.instances.push(Instance { class, name: String::new(), parent, children: Vec::new(), props: Props::default() });
    let index = dom.instances.len() - 1;
    if parent.is_none() {
        dom.roots.push(index);
    }
    Ok(index)
}

fn new_prop(tag: &[u8], e: &BytesStart) -> Prop {
    Prop {
        tag: tag.to_vec(),
        name: attr(e, b"name").unwrap_or_default(),
        depth: 0,
        text: String::new(),
        field: Vec::new(),
        field_text: String::new(),
        fields: Vec::new(),
        long: matches!(tag, b"BinaryString"),
    }
}

/// Adds text to the property or shared string being read.  Long text
/// counts against the budget; short text is capped.
fn push_text(
    prop: &mut Option<Prop>,
    shared: &mut Option<(String, String)>,
    text: &str,
    budget: &mut Budget,
) -> Result<(), LoadError> {
    if let Some(p) = prop.as_mut() {
        let wanted = p.name == "Name" || Key::from_name(&p.name).is_some();
        if !wanted {
            return Ok(());
        }
        let target = if p.depth > 0 { &mut p.field_text } else { &mut p.text };
        if p.long {
            budget.take_bytes(text.len() as u64)?;
        } else if target.len() + text.len() > MAX_TEXT {
            return Ok(());
        }
        target.push_str(text);
    } else if let Some((_, s)) = shared.as_mut() {
        budget.take_bytes(text.len() as u64)?;
        s.push_str(text);
    }
    Ok(())
}

/// Turns a finished property element into a value on its instance.
fn finish(
    dom: &mut Dom,
    instance: usize,
    p: Prop,
    shared_keys: &mut HashMap<String, u32>,
    budget: &mut Budget,
) -> Result<(), LoadError> {
    let _ = budget;
    if p.name == "Name" {
        let mut name = p.text;
        if name.len() > MAX_NAME {
            let mut end = MAX_NAME;
            while !name.is_char_boundary(end) {
                end -= 1;
            }
            name.truncate(end);
        }
        dom.instances[instance].name = name;
        return Ok(());
    }
    let Some(key) = Key::from_name(&p.name) else { return Ok(()) };
    let field = |name: &str| p.fields.iter().find(|(f, _)| f == name.as_bytes()).map(|(_, t)| t.as_str());
    let num = |name: &str| field(name).and_then(parse_float).unwrap_or(0.0);
    let value = match p.tag.as_slice() {
        b"string" | b"ProtectedString" => Value::Text(p.text),
        b"bool" => Value::Bool(p.text.trim().eq_ignore_ascii_case("true")),
        b"int" | b"int64" | b"token" | b"BrickColor" => match p.text.trim().parse::<i64>() {
            Ok(v) => Value::Int(v),
            Err(_) => return Ok(()),
        },
        b"float" | b"double" => match parse_float(&p.text) {
            Some(v) => Value::Float(v),
            None => return Ok(()),
        },
        b"Vector3" => Value::Vector3(Vec3::new(num("X"), num("Y"), num("Z"))),
        b"CoordinateFrame" | b"CFrame" => {
            let names = ["X", "Y", "Z", "R00", "R01", "R02", "R10", "R11", "R12", "R20", "R21", "R22"];
            let identity = [0., 0., 0., 1., 0., 0., 0., 1., 0., 0., 0., 1.];
            let c: [f32; 12] = std::array::from_fn(|i| field(names[i]).and_then(parse_float).unwrap_or(identity[i]));
            Value::CFrame(CFrame::from_components(c))
        }
        b"Color3" if field("R").is_some() => Value::Color3(Vec3::new(num("R"), num("G"), num("B"))),
        // Before Color3 had components, it was a packed number.
        b"Color3" | b"Color3uint8" => match p.text.trim().parse::<u32>() {
            Ok(v) => Value::Color3u8([(v >> 16) as u8, (v >> 8) as u8, v as u8]),
            Err(_) => return Ok(()),
        },
        b"Content" | b"ContentId" => Value::Text(field("url").or(field("uri")).unwrap_or("").trim().to_owned()),
        b"BinaryString" => Value::Bytes(decode_base64(&p.text)),
        b"SharedString" => Value::Shared(shared_index(dom, shared_keys, p.text.trim())),
        _ => return Ok(()),
    };
    dom.instances[instance].props.set(key, value);
    Ok(())
}

/// The index of the shared string with this key, reserving a slot for it
/// until its definition at the end of the file is read.
fn shared_index(dom: &mut Dom, keys: &mut HashMap<String, u32>, key: &str) -> u32 {
    if let Some(&i) = keys.get(key) {
        return i;
    }
    let i = dom.shared.len() as u32;
    dom.shared.push(Vec::new());
    keys.insert(key.to_owned(), i);
    i
}

/// Base64 with line breaks and other whitespace in it, as Roblox writes
/// long values; undecodable text gives nothing.
fn decode_base64(text: &str) -> Vec<u8> {
    let clean: Vec<u8> = text.bytes().filter(|b| !b.is_ascii_whitespace()).collect();
    base64::engine::general_purpose::STANDARD.decode(&clean).unwrap_or_default()
}

/// A float as Roblox writes them, `INF` and `NAN` included.
fn parse_float(s: &str) -> Option<f32> {
    let s = s.trim();
    match s {
        "INF" | "+INF" => Some(f32::INFINITY),
        "-INF" => Some(f32::NEG_INFINITY),
        "NAN" => Some(f32::NAN),
        _ => s.parse::<f32>().ok(),
    }
}

fn attr(e: &BytesStart, name: &[u8]) -> Option<String> {
    let a = e.attributes().with_checks(false).flatten().find(|a| a.key.local_name().as_ref() == name)?;
    let raw = String::from_utf8_lossy(&a.value).into_owned();
    Some(if raw.contains('&') { unescape(&raw) } else { raw })
}

/// Replaces XML's predefined entities and character references.
fn unescape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find('&') {
        out.push_str(&rest[..at]);
        rest = &rest[at..];
        let Some(end) = rest.find(';').filter(|&e| e <= 12) else {
            out.push('&');
            rest = &rest[1..];
            continue;
        };
        match entity(&rest[1..end]) {
            Some(c) => out.push(c),
            None => out.push_str(&rest[..=end]),
        }
        rest = &rest[end + 1..];
    }
    out.push_str(rest);
    out
}

/// The character an entity name (without `&` and `;`) stands for.
fn entity(name: &str) -> Option<char> {
    match name {
        "amp" => Some('&'),
        "lt" => Some('<'),
        "gt" => Some('>'),
        "quot" => Some('"'),
        "apos" => Some('\''),
        _ => {
            let n = name.strip_prefix('#')?;
            let code = match n.strip_prefix('x').or_else(|| n.strip_prefix('X')) {
                Some(hex) => u32::from_str_radix(hex, 16).ok()?,
                None => n.parse().ok()?,
            };
            char::from_u32(code)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sniffs_the_root_element() {
        assert!(sniff(b"<roblox version=\"4\">"));
        assert!(sniff(b"\xef\xbb\xbf<?xml version=\"1.0\"?>\n<!-- x -->\n<roblox xmlns:xmime=\"a\">"));
        assert!(sniff(b"<roblox>"));
        assert!(!sniff(b"<roblox!\x89\xff"));
        assert!(!sniff(b"<robloxian>"));
        assert!(!sniff(b"<html><roblox>"));
    }

    #[test]
    fn floats_as_roblox_writes_them() {
        assert_eq!(parse_float(" 0.15625 "), Some(0.15625));
        assert_eq!(parse_float("-INF"), Some(f32::NEG_INFINITY));
        assert_eq!(parse_float("13e37"), Some(13e37));
        assert!(parse_float("NAN").is_some_and(f32::is_nan));
        assert_eq!(parse_float("x"), None);
    }
}
