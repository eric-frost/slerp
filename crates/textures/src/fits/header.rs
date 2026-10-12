//! FITS headers: blocks of 2880 bytes holding 80-character cards, ended by
//! an `END` card.

use crate::TextureError;
use crate::util::invalid;
use std::io::{Read, Seek, SeekFrom};

pub(crate) const BLOCK: u64 = 2880;

/// Header blocks read before giving up: 4096 blocks are 47,000 cards, far
/// more than any real file has.
const MAX_BLOCKS: u64 = 4096;

/// A card's value.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Value {
    Str(String),
    Bool(bool),
    Int(i64),
    Float(f64),
    /// An empty or unreadable value field.
    None,
}

impl Value {
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Value::Int(i) => Some(*i as f64),
            Value::Float(f) => Some(*f),
            _ => None,
        }
    }

    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Value::Int(i) => Some(*i),
            Value::Float(f) if f.fract() == 0.0 && f.abs() < 9e18 => Some(*f as i64),
            _ => None,
        }
    }

    /// The value as the inspector shows it.
    pub fn display(&self) -> String {
        match self {
            Value::Str(s) => s.clone(),
            Value::Bool(true) => "T".into(),
            Value::Bool(false) => "F".into(),
            Value::Int(i) => i.to_string(),
            Value::Float(f) => {
                if f.abs() >= 1e15 || (f.abs() < 1e-4 && *f != 0.0) {
                    format!("{f:e}")
                } else {
                    format!("{f}")
                }
            }
            Value::None => String::new(),
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct Card {
    pub key: String,
    pub value: Value,
    /// The text of COMMENT and HISTORY cards, or the comment after a value.
    pub comment: String,
}

/// The cards of one header.
#[derive(Clone, Debug, Default)]
pub(crate) struct Header {
    pub cards: Vec<Card>,
}

impl Header {
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.cards.iter().find(|c| c.key == key).map(|c| &c.value)
    }

    pub fn int(&self, key: &str) -> Option<i64> {
        self.get(key).and_then(Value::as_i64)
    }

    pub fn float(&self, key: &str) -> Option<f64> {
        self.get(key).and_then(Value::as_f64)
    }

    pub fn string(&self, key: &str) -> Option<&str> {
        match self.get(key) {
            Some(Value::Str(s)) => Some(s.as_str()),
            _ => None,
        }
    }

    pub fn flag(&self, key: &str) -> Option<bool> {
        match self.get(key) {
            Some(Value::Bool(b)) => Some(*b),
            _ => None,
        }
    }

    /// Keyword `{prefix}{n}` as an integer, for NAXISn, ZNAXISn and the like.
    pub fn indexed(&self, prefix: &str, n: usize) -> Option<i64> {
        self.int(&format!("{prefix}{n}"))
    }
}

/// Reads the header starting at `start`.  Returns it and the offset of the
/// byte after its last block, where the data begins.
pub(crate) fn read<R: Read + Seek + ?Sized>(
    r: &mut R,
    start: u64,
    file_len: u64,
) -> Result<(Header, u64), TextureError> {
    r.seek(SeekFrom::Start(start))?;
    let mut header = Header::default();
    let mut block = vec![0u8; BLOCK as usize];
    for n in 0..MAX_BLOCKS {
        if start + (n + 1) * BLOCK > file_len {
            return Err(invalid("FITS: header runs past the end of the file"));
        }
        r.read_exact(&mut block)?;
        if n == 0 && !block.is_ascii() {
            return Err(invalid("FITS: header is not text"));
        }
        for raw in block.chunks_exact(80) {
            if raw.starts_with(b"END     ") {
                return Ok((header, start + (n + 1) * BLOCK));
            }
            if let Some(card) = parse_card(raw) {
                merge(&mut header, card);
            }
        }
    }
    Err(invalid("FITS: header has no END card"))
}

/// Adds a card, joining CONTINUE cards onto the long string before them.
fn merge(header: &mut Header, card: Card) {
    if card.key == "CONTINUE"
        && let Some(last) = header.cards.last_mut()
        && let Value::Str(s) = &mut last.value
        && s.ends_with('&')
    {
        s.pop();
        if let Value::Str(more) = &card.value {
            s.push_str(more);
        }
        return;
    }
    header.cards.push(card);
}

fn parse_card(raw: &[u8]) -> Option<Card> {
    let text = String::from_utf8_lossy(raw);
    let key = text.get(..8)?.trim_end().to_string();
    if key.is_empty() && text.trim().is_empty() {
        return None;
    }
    if key == "COMMENT" || key == "HISTORY" || key.is_empty() {
        let comment = text.get(8..).unwrap_or("").trim_end().to_string();
        return Some(Card { key, value: Value::None, comment: comment.trim_start().to_string() });
    }
    if key == "CONTINUE" {
        let rest = text.get(8..).unwrap_or("").trim_start();
        let (value, comment) = parse_value(rest);
        return Some(Card { key, value, comment });
    }
    // HIERARCH cards carry a long keyword before the equals sign.
    if key == "HIERARCH" {
        let rest = text.get(8..)?;
        let (name, value) = rest.split_once('=')?;
        let (value, comment) = parse_value(value.trim_start());
        return Some(Card { key: name.trim().to_string(), value, comment });
    }
    if text.get(8..10) != Some("= ") {
        // A keyword without a value indicator: shown as a comment.
        return Some(Card { key, value: Value::None, comment: text.get(8..).unwrap_or("").trim().to_string() });
    }
    let (value, comment) = parse_value(text.get(10..).unwrap_or("").trim_start());
    Some(Card { key, value, comment })
}

/// Splits `value / comment`.
fn parse_value(s: &str) -> (Value, String) {
    if let Some(rest) = s.strip_prefix('\'') {
        // A string: '' is a quote inside it, trailing blanks don't count.
        let mut out = String::new();
        let mut chars = rest.char_indices().peekable();
        let mut end = rest.len();
        while let Some((i, c)) = chars.next() {
            if c == '\'' {
                if let Some(&(_, '\'')) = chars.peek() {
                    out.push('\'');
                    chars.next();
                } else {
                    end = i + 1;
                    break;
                }
            } else {
                out.push(c);
            }
        }
        let comment = rest[end..].trim_start().strip_prefix('/').unwrap_or("").trim().to_string();
        return (Value::Str(out.trim_end().to_string()), comment);
    }
    let (v, comment) = match s.split_once('/') {
        Some((v, c)) => (v.trim(), c.trim().to_string()),
        None => (s.trim(), String::new()),
    };
    let value = match v {
        "" => Value::None,
        "T" => Value::Bool(true),
        "F" => Value::Bool(false),
        _ => {
            if let Ok(i) = v.parse::<i64>() {
                Value::Int(i)
            } else {
                // Fortran writes exponents with D.
                match v.replace(['D', 'd'], "E").parse::<f64>() {
                    Ok(f) => Value::Float(f),
                    Err(_) => Value::None,
                }
            }
        }
    };
    (value, comment)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn card(s: &str) -> Vec<u8> {
        let mut v = s.as_bytes().to_vec();
        v.resize(80, b' ');
        v
    }

    #[test]
    fn values() {
        let c = parse_card(&card("BITPIX  =                  -32 / bits per pixel")).unwrap();
        assert_eq!(
            (c.key.as_str(), c.value.clone(), c.comment.as_str()),
            ("BITPIX", Value::Int(-32), "bits per pixel")
        );
        let c = parse_card(&card("OBJECT  = 'M 31    '           / it''s big")).unwrap();
        assert_eq!(c.value, Value::Str("M 31".into()));
        assert_eq!(c.comment, "it''s big");
        let c = parse_card(&card("TELESCOP= 'Joe''s scope'")).unwrap();
        assert_eq!(c.value, Value::Str("Joe's scope".into()));
        let c = parse_card(&card("BZERO   =              32768.0")).unwrap();
        assert_eq!(c.value.as_i64(), Some(32768));
        let c = parse_card(&card("EXPTIME = 1.5D+02")).unwrap();
        assert_eq!(c.value.as_f64(), Some(150.0));
        let c = parse_card(&card("SIMPLE  =                    T")).unwrap();
        assert_eq!(c.value, Value::Bool(true));
        let c = parse_card(&card("HIERARCH ESO DET CHIP1 ID = 'CCD-44'")).unwrap();
        assert_eq!(c.key, "ESO DET CHIP1 ID");
        let c = parse_card(&card("COMMENT   hello there")).unwrap();
        assert_eq!(c.comment, "hello there");
        // A slash inside a string is not a comment.
        let c = parse_card(&card("FILTER  = 'H/alpha' / narrowband")).unwrap();
        assert_eq!(c.value, Value::Str("H/alpha".into()));
        assert_eq!(c.comment, "narrowband");
    }

    #[test]
    fn continued_strings_join() {
        let mut h = Header::default();
        merge(&mut h, parse_card(&card("LONG    = 'first part&'")).unwrap());
        merge(&mut h, parse_card(&card("CONTINUE  ' and the rest'")).unwrap());
        assert_eq!(h.string("LONG"), Some("first part and the rest"));
    }

    #[test]
    fn reads_blocks_until_end() {
        let mut data = Vec::new();
        data.extend(card("SIMPLE  =                    T"));
        data.extend(card("NAXIS   =                    0"));
        data.extend(card("END"));
        data.resize(2880 * 2, b' ');
        let len = data.len() as u64;
        let (h, next) = read(&mut std::io::Cursor::new(data), 0, len).unwrap();
        assert_eq!(h.int("NAXIS"), Some(0));
        assert_eq!(next, 2880);
    }

    #[test]
    fn rejects_binary_and_unterminated_headers() {
        let data = vec![0xffu8; 2880];
        assert!(read(&mut std::io::Cursor::new(data), 0, 2880).is_err());
        let data = vec![b' '; 2880 * 3];
        assert!(read(&mut std::io::Cursor::new(data), 0, 2880 * 3).is_err());
        assert!(read(&mut std::io::Cursor::new(vec![b' '; 100]), 0, 100).is_err());
    }
}
