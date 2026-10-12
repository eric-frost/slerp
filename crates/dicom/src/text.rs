//! Data sets as text for the inspector: each element's tag, name, VR and
//! value, sequences as nested rows, and which elements identify a person.

use crate::data::{DataSet, Element, Tag, Value, Vr, decode_text, numbers, tag, tag_name, tag_text};

/// One element as the inspector shows it.
#[derive(Clone, Debug)]
pub struct Row {
    pub tag: Tag,
    /// `(gggg,eeee)`.
    pub tag_text: String,
    pub name: String,
    pub vr: String,
    pub value: String,
    /// The element names or identifies a person or the place that cared
    /// for them, so it is hidden unless the user asks.
    pub identifying: bool,
    /// A sequence's items, each a list of rows.
    pub items: Vec<Vec<Row>>,
}

/// Elements hidden behind "Show patient details": identifiers of the
/// patient and of the people and places involved, after the Basic
/// Application Level Confidentiality Profile (PS3.15 E.1).  Every person
/// name (PN) is hidden as well, wherever it is.
const IDENTIFYING: &[Tag] = &[
    tag(0x0008, 0x0050), // Accession Number
    tag(0x0008, 0x0080), // Institution Name
    tag(0x0008, 0x0081), // Institution Address
    tag(0x0008, 0x0082), // Institution Code Sequence
    tag(0x0008, 0x0090), // Referring Physician's Name
    tag(0x0008, 0x0092), // Referring Physician's Address
    tag(0x0008, 0x0094), // Referring Physician's Telephone Numbers
    tag(0x0008, 0x0096), // Referring Physician Identification Sequence
    tag(0x0008, 0x1010), // Station Name
    tag(0x0008, 0x1040), // Institutional Department Name
    tag(0x0008, 0x1048), // Physician(s) of Record
    tag(0x0008, 0x1050), // Performing Physician's Name
    tag(0x0008, 0x1060), // Name of Physician(s) Reading Study
    tag(0x0008, 0x1070), // Operators' Name
    tag(0x0010, 0x0010), // Patient's Name
    tag(0x0010, 0x0020), // Patient ID
    tag(0x0010, 0x0021), // Issuer of Patient ID
    tag(0x0010, 0x0030), // Patient's Birth Date
    tag(0x0010, 0x0032), // Patient's Birth Time
    tag(0x0010, 0x0050), // Patient's Insurance Plan Code Sequence
    tag(0x0010, 0x1000), // Other Patient IDs
    tag(0x0010, 0x1001), // Other Patient Names
    tag(0x0010, 0x1002), // Other Patient IDs Sequence
    tag(0x0010, 0x1005), // Patient's Birth Name
    tag(0x0010, 0x1040), // Patient's Address
    tag(0x0010, 0x1060), // Patient's Mother's Birth Name
    tag(0x0010, 0x1080), // Military Rank
    tag(0x0010, 0x1090), // Medical Record Locator
    tag(0x0010, 0x2000), // Medical Alerts
    tag(0x0010, 0x2110), // Allergies
    tag(0x0010, 0x2154), // Patient's Telephone Numbers
    tag(0x0010, 0x2155), // Patient's Telecom Information
    tag(0x0010, 0x21B0), // Additional Patient History
    tag(0x0010, 0x4000), // Patient Comments
    tag(0x0020, 0x0010), // Study ID
    tag(0x0020, 0x4000), // Image Comments
    tag(0x0032, 0x1032), // Requesting Physician
    tag(0x0038, 0x0010), // Admission ID
    tag(0x0038, 0x0400), // Patient's Institution Residence
    tag(0x0040, 0x0006), // Scheduled Performing Physician's Name
    tag(0x0040, 0x1010), // Names of Intended Recipients of Results
    tag(0x0040, 0xA123), // Person Name
];

/// Whether an element's value may identify a person: a listed element,
/// any person name, or the value of a private element, which vendors fill
/// with patient and exam identifiers the standard doesn't describe.
/// (Private creators, which only name the vendor, stay visible.)
pub fn is_identifying(t: Tag, vr: Vr) -> bool {
    let (group, element) = ((t >> 16) as u16, t as u16);
    let private_value = group & 1 == 1 && element > 0x00FF;
    vr == Vr::PN || private_value || IDENTIFYING.binary_search(&t).is_ok()
}

/// Values longer than this are cut, with an ellipsis.
const MAX_CHARS: usize = 400;
/// Binary values show at most this many numbers.
const MAX_NUMBERS: usize = 32;

/// The rows of a data set, sequences nested.
pub fn rows(ds: &DataSet) -> Vec<Row> {
    rows_at(ds, 0)
}

fn rows_at(ds: &DataSet, depth: u32) -> Vec<Row> {
    ds.elements.iter().map(|e| row(e, ds.big_endian, depth)).collect()
}

fn row(e: &Element, big: bool, depth: u32) -> Row {
    let items = match &e.value {
        Value::Sequence(items) if depth < 32 => items.iter().map(|i| rows_at(i, depth + 1)).collect(),
        _ => Vec::new(),
    };
    Row {
        tag: e.tag,
        tag_text: tag_text(e.tag),
        name: tag_name(e.tag),
        vr: e.vr.as_str().to_string(),
        value: value_text(e, big),
        identifying: is_identifying(e.tag, e.vr),
        items,
    }
}

/// An element's value as text.
pub fn value_text(e: &Element, big: bool) -> String {
    let bytes = match &e.value {
        Value::Sequence(items) => return format!("{} item{}", items.len(), if items.len() == 1 { "" } else { "s" }),
        Value::Deferred(span) => return format!("{} bytes", group_digits(span.len)),
        Value::Fragments(f) => {
            let total: u64 = f.iter().map(|s| s.len).sum();
            return format!("compressed, {} fragments, {} bytes", f.len().saturating_sub(1), group_digits(total));
        }
        Value::Bytes(b) => b,
    };
    let mut s = match e.vr {
        Vr::UI => {
            let uid = decode_text(bytes).trim_matches(|c: char| c == '\0' || c == ' ').to_string();
            match crate::dict::uid_name(&uid) {
                Some(name) => format!("{uid} ({name})"),
                None => uid,
            }
        }
        vr if vr.is_text() => decode_text(bytes).trim_end_matches(['\0', ' ']).replace(['\r', '\n'], " "),
        Vr::AT => bytes
            .chunks_exact(4)
            .take(MAX_NUMBERS)
            .map(|c| {
                let (g, el) = if big {
                    (u16::from_be_bytes([c[0], c[1]]), u16::from_be_bytes([c[2], c[3]]))
                } else {
                    (u16::from_le_bytes([c[0], c[1]]), u16::from_le_bytes([c[2], c[3]]))
                };
                tag_text(tag(g, el))
            })
            .collect::<Vec<_>>()
            .join("\\"),
        Vr::US | Vr::SS | Vr::UL | Vr::SL | Vr::UV | Vr::SV | Vr::FL | Vr::FD => {
            let v = numbers(e.vr, bytes, big);
            let shown: Vec<String> = v.iter().take(MAX_NUMBERS).map(|x| number_text(*x)).collect();
            let more = if v.len() > MAX_NUMBERS { format!("\\... ({} values)", v.len()) } else { String::new() };
            shown.join("\\") + &more
        }
        _ => {
            // OB, OW, UN and the rest: text when it reads as text, else the
            // size and first bytes.
            let printable = !bytes.is_empty()
                && bytes.len() <= 1024
                && bytes.iter().all(|&c| c == 0 || c == b'\t' || c == b'\r' || c == b'\n' || (32..127).contains(&c));
            if printable {
                decode_text(bytes).trim_end_matches(['\0', ' ']).to_string()
            } else {
                let head: Vec<String> = bytes.iter().take(16).map(|b| format!("{b:02X}")).collect();
                let dots = if bytes.len() > 16 { " ..." } else { "" };
                format!("{} bytes: {}{dots}", group_digits(bytes.len() as u64), head.join(" "))
            }
        }
    };
    if s.chars().count() > MAX_CHARS {
        s = s.chars().take(MAX_CHARS).collect::<String>() + "...";
    }
    s
}

fn number_text(x: f64) -> String {
    if x.fract() == 0.0 && x.abs() < 1e15 { format!("{}", x as i64) } else { format!("{x}") }
}

/// A count with thin groups of three digits: 12 345 678.
pub fn group_digits(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(' ');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identifying_tags_are_sorted_and_found() {
        assert!(IDENTIFYING.windows(2).all(|w| w[0] < w[1]));
        assert!(is_identifying(tag(0x0010, 0x0010), Vr::PN));
        assert!(is_identifying(tag(0x0010, 0x0030), Vr::DA));
        assert!(is_identifying(tag(0x0008, 0x0080), Vr::LO));
        // Any person name, even in a private or unknown element.
        assert!(is_identifying(tag(0x0029, 0x1010), Vr::PN));
        assert!(!is_identifying(tag(0x0008, 0x0060), Vr::CS));
        // Private values are hidden, private creators are not.
        assert!(is_identifying(tag(0x0009, 0x1001), Vr::LO));
        assert!(is_identifying(tag(0x0029, 0x1010), Vr::SQ));
        assert!(!is_identifying(tag(0x0009, 0x0010), Vr::LO));
        // Free-text history and the sequence of other IDs.
        assert!(is_identifying(tag(0x0010, 0x21B0), Vr::LT));
        assert!(is_identifying(tag(0x0010, 0x1002), Vr::SQ));
    }

    #[test]
    fn values() {
        let e = |vr: Vr, b: &[u8]| Element { tag: tag(0x0028, 0x0010), vr, value: Value::Bytes(b.to_vec()) };
        assert_eq!(value_text(&e(Vr::US, &[0, 2]), false), "512");
        assert_eq!(value_text(&e(Vr::US, &[0, 2]), true), "2");
        assert_eq!(value_text(&e(Vr::DS, b"0.5\\0.5 "), false), "0.5\\0.5");
        assert_eq!(
            value_text(&e(Vr::UI, b"1.2.840.10008.1.2\0"), false),
            "1.2.840.10008.1.2 (Implicit VR Little Endian)"
        );
        assert_eq!(value_text(&e(Vr::OB, &[0, 1, 0xFF]), false), "3 bytes: 00 01 FF");
        assert_eq!(value_text(&e(Vr::AT, &[0x28, 0, 0x10, 0]), false), "(0028,0010)");
        assert_eq!(group_digits(1234567), "1 234 567");
    }
}
