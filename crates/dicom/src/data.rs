//! The data set model: elements with their value representation (VR) and
//! value, nested sequences, and typed accessors that decode values in the
//! data set's byte order.

use crate::dict;

/// A tag as `group << 16 | element`.
pub type Tag = u32;

pub const fn tag(group: u16, element: u16) -> Tag {
    (group as u32) << 16 | element as u32
}

/// Tags this crate reads.  Names follow the standard's keywords.
pub mod tags {
    use super::{Tag, tag};
    pub const TRANSFER_SYNTAX: Tag = tag(0x0002, 0x0010);
    pub const MEDIA_STORAGE_SOP_CLASS: Tag = tag(0x0002, 0x0002);
    pub const SPECIFIC_CHARACTER_SET: Tag = tag(0x0008, 0x0005);
    pub const IMAGE_TYPE: Tag = tag(0x0008, 0x0008);
    pub const SOP_CLASS: Tag = tag(0x0008, 0x0016);
    pub const STUDY_DATE: Tag = tag(0x0008, 0x0020);
    pub const MODALITY: Tag = tag(0x0008, 0x0060);
    pub const MANUFACTURER: Tag = tag(0x0008, 0x0070);
    pub const STUDY_DESCRIPTION: Tag = tag(0x0008, 0x1030);
    pub const SERIES_DESCRIPTION: Tag = tag(0x0008, 0x103E);
    pub const MANUFACTURER_MODEL: Tag = tag(0x0008, 0x1090);
    pub const PATIENT_NAME: Tag = tag(0x0010, 0x0010);
    pub const PATIENT_ID: Tag = tag(0x0010, 0x0020);
    pub const BODY_PART: Tag = tag(0x0018, 0x0015);
    pub const SLICE_THICKNESS: Tag = tag(0x0018, 0x0050);
    pub const FRAME_TIME: Tag = tag(0x0018, 0x1063);
    pub const FRAME_TIME_VECTOR: Tag = tag(0x0018, 0x1065);
    pub const CINE_RATE: Tag = tag(0x0018, 0x0040);
    pub const RECOMMENDED_DISPLAY_FRAME_RATE: Tag = tag(0x0008, 0x2144);
    pub const IMAGER_PIXEL_SPACING: Tag = tag(0x0018, 0x1164);
    pub const STUDY_INSTANCE_UID: Tag = tag(0x0020, 0x000D);
    pub const SERIES_INSTANCE_UID: Tag = tag(0x0020, 0x000E);
    pub const STUDY_ID: Tag = tag(0x0020, 0x0010);
    pub const SERIES_NUMBER: Tag = tag(0x0020, 0x0011);
    pub const INSTANCE_NUMBER: Tag = tag(0x0020, 0x0013);
    pub const IMAGE_POSITION: Tag = tag(0x0020, 0x0032);
    pub const IMAGE_ORIENTATION: Tag = tag(0x0020, 0x0037);
    pub const SLICE_LOCATION: Tag = tag(0x0020, 0x1041);
    pub const SAMPLES_PER_PIXEL: Tag = tag(0x0028, 0x0002);
    pub const PHOTOMETRIC: Tag = tag(0x0028, 0x0004);
    pub const PLANAR_CONFIGURATION: Tag = tag(0x0028, 0x0006);
    pub const NUMBER_OF_FRAMES: Tag = tag(0x0028, 0x0008);
    pub const ROWS: Tag = tag(0x0028, 0x0010);
    pub const COLUMNS: Tag = tag(0x0028, 0x0011);
    pub const PIXEL_SPACING: Tag = tag(0x0028, 0x0030);
    pub const PIXEL_ASPECT_RATIO: Tag = tag(0x0028, 0x0034);
    pub const BITS_ALLOCATED: Tag = tag(0x0028, 0x0100);
    pub const BITS_STORED: Tag = tag(0x0028, 0x0101);
    pub const HIGH_BIT: Tag = tag(0x0028, 0x0102);
    pub const PIXEL_REPRESENTATION: Tag = tag(0x0028, 0x0103);
    pub const PIXEL_PADDING_VALUE: Tag = tag(0x0028, 0x0120);
    pub const PIXEL_PADDING_RANGE_LIMIT: Tag = tag(0x0028, 0x0121);
    pub const WINDOW_CENTER: Tag = tag(0x0028, 0x1050);
    pub const WINDOW_WIDTH: Tag = tag(0x0028, 0x1051);
    pub const RESCALE_INTERCEPT: Tag = tag(0x0028, 0x1052);
    pub const RESCALE_SLOPE: Tag = tag(0x0028, 0x1053);
    pub const RESCALE_TYPE: Tag = tag(0x0028, 0x1054);
    pub const WINDOW_EXPLANATION: Tag = tag(0x0028, 0x1055);
    pub const VOI_LUT_FUNCTION: Tag = tag(0x0028, 0x1056);
    pub const RED_PALETTE_DESCRIPTOR: Tag = tag(0x0028, 0x1101);
    pub const GREEN_PALETTE_DESCRIPTOR: Tag = tag(0x0028, 0x1102);
    pub const BLUE_PALETTE_DESCRIPTOR: Tag = tag(0x0028, 0x1103);
    pub const RED_PALETTE_DATA: Tag = tag(0x0028, 0x1201);
    pub const GREEN_PALETTE_DATA: Tag = tag(0x0028, 0x1202);
    pub const BLUE_PALETTE_DATA: Tag = tag(0x0028, 0x1203);
    pub const RED_SEGMENTED_PALETTE: Tag = tag(0x0028, 0x1221);
    pub const GREEN_SEGMENTED_PALETTE: Tag = tag(0x0028, 0x1222);
    pub const BLUE_SEGMENTED_PALETTE: Tag = tag(0x0028, 0x1223);
    pub const MODALITY_LUT_SEQUENCE: Tag = tag(0x0028, 0x3000);
    pub const LUT_DESCRIPTOR: Tag = tag(0x0028, 0x3002);
    pub const LUT_DATA: Tag = tag(0x0028, 0x3006);
    pub const VOI_LUT_SEQUENCE: Tag = tag(0x0028, 0x3010);
    pub const PRESENTATION_LUT_SHAPE: Tag = tag(0x2050, 0x0020);
    pub const SHARED_FUNCTIONAL_GROUPS: Tag = tag(0x5200, 0x9229);
    pub const PER_FRAME_FUNCTIONAL_GROUPS: Tag = tag(0x5200, 0x9230);
    pub const PIXEL_MEASURES_SEQUENCE: Tag = tag(0x0028, 0x9110);
    pub const PLANE_POSITION_SEQUENCE: Tag = tag(0x0020, 0x9113);
    pub const PLANE_ORIENTATION_SEQUENCE: Tag = tag(0x0020, 0x9116);
    pub const FRAME_VOI_LUT_SEQUENCE: Tag = tag(0x0028, 0x9132);
    pub const PIXEL_VALUE_TRANSFORMATION_SEQUENCE: Tag = tag(0x0028, 0x9145);
    pub const EXTENDED_OFFSET_TABLE: Tag = tag(0x7FE0, 0x0001);
    pub const EXTENDED_OFFSET_TABLE_LENGTHS: Tag = tag(0x7FE0, 0x0002);
    pub const FLOAT_PIXEL_DATA: Tag = tag(0x7FE0, 0x0008);
    pub const DOUBLE_PIXEL_DATA: Tag = tag(0x7FE0, 0x0009);
    pub const PIXEL_DATA: Tag = tag(0x7FE0, 0x0010);
    pub const ITEM: Tag = tag(0xFFFE, 0xE000);
    pub const ITEM_DELIMITATION: Tag = tag(0xFFFE, 0xE00D);
    pub const SEQUENCE_DELIMITATION: Tag = tag(0xFFFE, 0xE0DD);
    // DICOMDIR.
    pub const FIRST_ROOT_RECORD: Tag = tag(0x0004, 0x1200);
    pub const DIRECTORY_RECORD_SEQUENCE: Tag = tag(0x0004, 0x1220);
    pub const NEXT_RECORD: Tag = tag(0x0004, 0x1400);
    pub const LOWER_LEVEL_RECORD: Tag = tag(0x0004, 0x1420);
    pub const RECORD_TYPE: Tag = tag(0x0004, 0x1430);
    pub const REFERENCED_FILE_ID: Tag = tag(0x0004, 0x1500);
}

/// A value representation: the two letters that say how a value is
/// encoded.  Item tags have none, shown as two spaces.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Vr(pub [u8; 2]);

impl Vr {
    pub const AE: Vr = Vr(*b"AE");
    pub const AS: Vr = Vr(*b"AS");
    pub const AT: Vr = Vr(*b"AT");
    pub const CS: Vr = Vr(*b"CS");
    pub const DA: Vr = Vr(*b"DA");
    pub const DS: Vr = Vr(*b"DS");
    pub const DT: Vr = Vr(*b"DT");
    pub const FD: Vr = Vr(*b"FD");
    pub const FL: Vr = Vr(*b"FL");
    pub const IS: Vr = Vr(*b"IS");
    pub const LO: Vr = Vr(*b"LO");
    pub const LT: Vr = Vr(*b"LT");
    pub const OB: Vr = Vr(*b"OB");
    pub const OD: Vr = Vr(*b"OD");
    pub const OF: Vr = Vr(*b"OF");
    pub const OL: Vr = Vr(*b"OL");
    pub const OV: Vr = Vr(*b"OV");
    pub const OW: Vr = Vr(*b"OW");
    pub const PN: Vr = Vr(*b"PN");
    pub const SH: Vr = Vr(*b"SH");
    pub const SL: Vr = Vr(*b"SL");
    pub const SQ: Vr = Vr(*b"SQ");
    pub const SS: Vr = Vr(*b"SS");
    pub const ST: Vr = Vr(*b"ST");
    pub const SV: Vr = Vr(*b"SV");
    pub const TM: Vr = Vr(*b"TM");
    pub const UC: Vr = Vr(*b"UC");
    pub const UI: Vr = Vr(*b"UI");
    pub const UL: Vr = Vr(*b"UL");
    pub const UN: Vr = Vr(*b"UN");
    pub const UR: Vr = Vr(*b"UR");
    pub const US: Vr = Vr(*b"US");
    pub const UT: Vr = Vr(*b"UT");
    pub const UV: Vr = Vr(*b"UV");
    pub const NONE: Vr = Vr(*b"  ");

    const ALL: [Vr; 34] = [
        Vr::AE,
        Vr::AS,
        Vr::AT,
        Vr::CS,
        Vr::DA,
        Vr::DS,
        Vr::DT,
        Vr::FD,
        Vr::FL,
        Vr::IS,
        Vr::LO,
        Vr::LT,
        Vr::OB,
        Vr::OD,
        Vr::OF,
        Vr::OL,
        Vr::OV,
        Vr::OW,
        Vr::PN,
        Vr::SH,
        Vr::SL,
        Vr::SQ,
        Vr::SS,
        Vr::ST,
        Vr::SV,
        Vr::TM,
        Vr::UC,
        Vr::UI,
        Vr::UL,
        Vr::UN,
        Vr::UR,
        Vr::US,
        Vr::UT,
        Vr::UV,
    ];

    /// True for the 34 value representations the standard defines.
    pub fn is_known(self) -> bool {
        Vr::ALL.contains(&self)
    }

    /// True when explicit VR encoding gives this VR a 4-byte length after
    /// two reserved bytes, rather than a 2-byte length.
    pub fn long_length(self) -> bool {
        matches!(self.0, [b'O', _] | [b'U', b'C' | b'N' | b'R' | b'T' | b'V'] | [b'S', b'Q' | b'V'])
    }

    pub fn as_str(&self) -> &str {
        std::str::from_utf8(&self.0).unwrap_or("??")
    }

    /// True for VRs whose values are text.
    pub fn is_text(self) -> bool {
        matches!(
            self,
            Vr::AE
                | Vr::AS
                | Vr::CS
                | Vr::DA
                | Vr::DS
                | Vr::DT
                | Vr::IS
                | Vr::LO
                | Vr::LT
                | Vr::PN
                | Vr::SH
                | Vr::ST
                | Vr::TM
                | Vr::UC
                | Vr::UI
                | Vr::UR
                | Vr::UT
        )
    }
}

/// Where a value lies in the file when it wasn't read into memory: pixel
/// data, and other large binary values, which are read only when needed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Span {
    pub offset: u64,
    pub len: u64,
}

#[derive(Clone, Debug)]
pub enum Value {
    /// The value's bytes, in the data set's byte order.
    Bytes(Vec<u8>),
    /// A value left in the file.
    Deferred(Span),
    /// A sequence's items.
    Sequence(Vec<DataSet>),
    /// Encapsulated (compressed) pixel data: the items after the basic
    /// offset table, which is the first span.
    Fragments(Vec<Span>),
}

#[derive(Clone, Debug)]
pub struct Element {
    pub tag: Tag,
    pub vr: Vr,
    pub value: Value,
}

impl Element {
    /// The number of bytes the value takes in the file, for display.
    pub fn len(&self) -> u64 {
        match &self.value {
            Value::Bytes(b) => b.len() as u64,
            Value::Deferred(s) => s.len,
            Value::Sequence(_) => 0,
            Value::Fragments(f) => f.iter().map(|s| s.len).sum(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0 && !matches!(&self.value, Value::Sequence(items) if !items.is_empty())
    }
}

/// A data set (the whole file, or one item of a sequence), its elements
/// sorted by tag.
#[derive(Clone, Debug, Default)]
pub struct DataSet {
    pub elements: Vec<Element>,
    /// Values are big-endian (Explicit VR Big Endian files).
    pub big_endian: bool,
    /// Where the item's tag sits in the file, for DICOMDIR records, which
    /// point at each other by file offset.  0 for the top level.
    pub offset: u64,
}

impl DataSet {
    /// Sorts the elements by tag and drops repeated tags (keeping the
    /// first), so lookups can search.
    pub(crate) fn finish(&mut self) {
        self.elements.sort_by_key(|e| e.tag);
        self.elements.dedup_by_key(|e| e.tag);
    }

    pub fn get(&self, tag: Tag) -> Option<&Element> {
        self.elements.binary_search_by_key(&tag, |e| e.tag).ok().map(|i| &self.elements[i])
    }

    pub fn contains(&self, tag: Tag) -> bool {
        self.get(tag).is_some()
    }

    /// The bytes of a value held in memory.
    pub fn bytes(&self, tag: Tag) -> Option<&[u8]> {
        match &self.get(tag)?.value {
            Value::Bytes(b) => Some(b),
            _ => None,
        }
    }

    /// A text value as a whole, with padding (spaces and NULs) trimmed.
    pub fn string(&self, tag: Tag) -> Option<String> {
        let s = decode_text(self.bytes(tag)?);
        let s = s.trim_matches(|c: char| c == ' ' || c == '\0').to_string();
        (!s.is_empty()).then_some(s)
    }

    /// The values of a multi-valued text element (separated by `\`),
    /// trimmed.
    pub fn strings(&self, tag: Tag) -> Vec<String> {
        let Some(b) = self.bytes(tag) else { return Vec::new() };
        decode_text(b).split('\\').map(|s| s.trim_matches(|c: char| c == ' ' || c == '\0').to_string()).collect()
    }

    /// The numbers of an element, whatever its VR: binary integers and
    /// floats in the data set's byte order, or decimal and integer strings.
    pub fn numbers(&self, tag: Tag) -> Vec<f64> {
        let Some(e) = self.get(tag) else { return Vec::new() };
        let Value::Bytes(b) = &e.value else { return Vec::new() };
        numbers(e.vr, b, self.big_endian)
    }

    pub fn float(&self, tag: Tag) -> Option<f64> {
        self.numbers(tag).into_iter().next().filter(|v| v.is_finite())
    }

    pub fn int(&self, tag: Tag) -> Option<i64> {
        let v = self.float(tag)?;
        (v.abs() < 9.0e15).then_some(v as i64)
    }

    /// A small unsigned integer such as Rows or Bits Allocated.
    pub fn u32(&self, tag: Tag) -> Option<u32> {
        self.int(tag).and_then(|v| u32::try_from(v).ok())
    }

    /// The items of a sequence (empty when absent or not a sequence).
    pub fn items(&self, tag: Tag) -> &[DataSet] {
        match self.get(tag).map(|e| &e.value) {
            Some(Value::Sequence(items)) => items,
            _ => &[],
        }
    }

    /// 16-bit words of a binary value (OW, US, SS), in the data set's byte
    /// order.
    pub fn words(&self, tag: Tag) -> Option<Vec<u16>> {
        let b = self.bytes(tag)?;
        Some(words(b, self.big_endian))
    }
}

pub(crate) fn words(b: &[u8], big_endian: bool) -> Vec<u16> {
    b.chunks_exact(2)
        .map(|c| if big_endian { u16::from_be_bytes([c[0], c[1]]) } else { u16::from_le_bytes([c[0], c[1]]) })
        .collect()
}

/// Decodes the numbers in a value of any numeric or numeric-text VR.
pub fn numbers(vr: Vr, b: &[u8], big: bool) -> Vec<f64> {
    macro_rules! binary {
        ($t:ty, $n:expr) => {
            b.chunks_exact($n)
                .map(|c| {
                    let a: [u8; $n] = c.try_into().unwrap_or([0; $n]);
                    (if big { <$t>::from_be_bytes(a) } else { <$t>::from_le_bytes(a) }) as f64
                })
                .collect()
        };
    }
    match vr {
        Vr::US => binary!(u16, 2),
        Vr::SS => binary!(i16, 2),
        Vr::UL => binary!(u32, 4),
        Vr::SL => binary!(i32, 4),
        Vr::UV => binary!(u64, 8),
        Vr::SV => binary!(i64, 8),
        Vr::FL | Vr::OF => binary!(f32, 4),
        Vr::FD | Vr::OD => binary!(f64, 8),
        Vr::DS | Vr::IS => decode_text(b)
            .split('\\')
            .filter_map(|s| s.trim_matches(|c: char| c == ' ' || c == '\0').parse::<f64>().ok())
            .collect(),
        // Implicit VR files give private and unknown elements no VR; a
        // value that reads as decimal text is taken as such.
        _ if b.iter().all(|&c| c.is_ascii_digit() || b" +-.eE\\\0".contains(&c)) && !b.is_empty() => {
            numbers(Vr::DS, b, big)
        }
        _ => Vec::new(),
    }
}

/// Text from a value.  Most files are ASCII or Latin-1 (ISO_IR 100), the
/// rest UTF-8 (ISO_IR 192); valid UTF-8 is taken as such, anything else as
/// Latin-1, which never fails.  Other character sets (ISO 2022 Japanese
/// and Korean) show as Latin-1.
pub fn decode_text(b: &[u8]) -> String {
    match std::str::from_utf8(b) {
        Ok(s) => s.to_string(),
        Err(_) => b.iter().map(|&c| c as char).collect(),
    }
}

/// The dictionary's name for a tag, or a description of a private one.
pub fn tag_name(t: Tag) -> String {
    if let Some((_, name)) = dict::lookup(t) {
        return name.to_string();
    }
    let (group, element) = ((t >> 16) as u16, t as u16);
    if group & 1 == 1 {
        if element == 0 {
            return "Private Group Length".into();
        }
        if (0x0010..=0x00FF).contains(&element) {
            return "Private Creator".into();
        }
        return "Private tag".into();
    }
    if element == 0 {
        return "Group Length".into();
    }
    "Unknown tag".into()
}

/// A tag as `(gggg,eeee)`.
pub fn tag_text(t: Tag) -> String {
    format!("({:04X},{:04X})", t >> 16, t & 0xFFFF)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lengths_in_explicit_vr() {
        // Four-byte lengths after two reserved bytes for the binary and
        // unlimited-text VRs; two-byte lengths for the rest.
        for vr in
            [Vr::OB, Vr::OW, Vr::OF, Vr::OD, Vr::OL, Vr::OV, Vr::SQ, Vr::UC, Vr::UN, Vr::UR, Vr::UT, Vr::SV, Vr::UV]
        {
            assert!(vr.long_length(), "{}", vr.as_str());
        }
        for vr in [
            Vr::AE,
            Vr::CS,
            Vr::DS,
            Vr::IS,
            Vr::LO,
            Vr::LT,
            Vr::PN,
            Vr::SH,
            Vr::SL,
            Vr::SS,
            Vr::ST,
            Vr::UI,
            Vr::UL,
            Vr::US,
            Vr::FL,
            Vr::FD,
            Vr::AT,
        ] {
            assert!(!vr.long_length(), "{}", vr.as_str());
        }
        assert!(Vr(*b"CS").is_known() && !Vr(*b"ab").is_known() && !Vr(*b"\0\0").is_known());
    }

    #[test]
    fn numbers_in_both_byte_orders() {
        assert_eq!(numbers(Vr::US, &[0x01, 0x02], false), [513.0]);
        assert_eq!(numbers(Vr::US, &[0x01, 0x02], true), [258.0]);
        assert_eq!(numbers(Vr::SS, &[0xFF, 0xFF, 0x00, 0x80], false), [-1.0, -32768.0]);
        assert_eq!(numbers(Vr::SL, &[0xFF, 0xFF, 0xFF, 0xFE], true), [-2.0]);
        assert_eq!(numbers(Vr::UL, &[0, 0, 0, 1], true), [1.0]);
        assert_eq!(numbers(Vr::FL, &1.5f32.to_be_bytes(), true), [1.5]);
        assert_eq!(numbers(Vr::FD, &(-2.25f64).to_le_bytes(), false), [-2.25]);
        // A truncated last value is dropped, not misread.
        assert_eq!(numbers(Vr::US, &[1, 0, 2], false), [1.0]);
        // Decimal and integer strings, several values, padding.
        assert_eq!(numbers(Vr::DS, b"1.5\\-2e1 ", false), [1.5, -20.0]);
        assert_eq!(numbers(Vr::IS, b"-7\0", false), [-7.0]);
        assert!(numbers(Vr::DS, b"abc", false).is_empty());
        // An unknown VR whose bytes read as a number is taken as one.
        assert_eq!(numbers(Vr::UN, b"0.5\\0.5", false), [0.5, 0.5]);
        assert!(numbers(Vr::UN, &[0, 1, 2], false).is_empty());
    }

    #[test]
    fn text_in_latin_1_and_utf_8() {
        assert_eq!(decode_text(b"Smith"), "Smith");
        assert_eq!(decode_text("M\u{fc}ller".as_bytes()), "M\u{fc}ller");
        // Latin-1 bytes that aren't valid UTF-8.
        assert_eq!(decode_text(&[b'M', 0xFC, b'l']), "M\u{fc}l");
    }

    #[test]
    fn names_of_tags() {
        assert_eq!(tag_name(tag(0x0028, 0x0030)), "Pixel Spacing");
        assert_eq!(tag_name(tag(0x0029, 0x0010)), "Private Creator");
        assert_eq!(tag_name(tag(0x0029, 0x1010)), "Private tag");
        assert_eq!(tag_name(tag(0x0029, 0x0000)), "Private Group Length");
        assert_eq!(tag_name(tag(0x0099, 0x0000)), "Private Group Length");
        assert_eq!(tag_text(tag(0x7FE0, 0x0010)), "(7FE0,0010)");
    }

    #[test]
    fn data_set_lookups() {
        let el = |t: Tag, vr: Vr, b: &[u8]| Element { tag: t, vr, value: Value::Bytes(b.to_vec()) };
        let mut ds = DataSet {
            elements: vec![
                el(tags::ROWS, Vr::US, &[0x00, 0x02]),
                el(tags::MODALITY, Vr::CS, b"CT"),
                el(tags::PIXEL_SPACING, Vr::DS, b"0.5\\0.25"),
            ],
            ..Default::default()
        };
        ds.finish();
        assert_eq!(ds.u32(tags::ROWS), Some(512));
        assert_eq!(ds.string(tags::MODALITY).as_deref(), Some("CT"));
        assert_eq!(ds.numbers(tags::PIXEL_SPACING), [0.5, 0.25]);
        assert_eq!(ds.strings(tags::PIXEL_SPACING), ["0.5", "0.25"]);
        assert!(ds.get(tags::COLUMNS).is_none() && ds.u32(tags::COLUMNS).is_none());
        // Negative numbers aren't sizes.
        let neg = DataSet { elements: vec![el(tags::ROWS, Vr::SS, &[0xFF, 0xFF])], ..Default::default() };
        assert_eq!(neg.int(tags::ROWS), Some(-1));
        assert_eq!(neg.u32(tags::ROWS), None);
    }
}
