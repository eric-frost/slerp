//! Bounds-checked reading of the binary formats (PMX, PMD, VMD, IQM, PSK
//! and PSA) and the small text helpers the text formats (MD5, IQE, SMD)
//! share.  Every read is checked against the bytes actually present, and
//! every count a file declares is checked against the bytes left before
//! anything is allocated for it, so a damaged header can't claim gigabytes.

use slerp_scene::LoadError;
use slerp_scene::glam::{Quat, Vec2, Vec3, Vec4};

pub type Result<T> = std::result::Result<T, LoadError>;

/// The error for a file that ends before the structure being read does.
pub fn truncated(what: &str) -> LoadError {
    LoadError::invalid(format!("the file ends inside {what}"))
}

/*┌─────────────────╮
| Binary cursor   |
└─────────────────╯*/
//region Binary cursor

/// A position in a little-endian byte slice.
#[derive(Clone)]
pub struct Cursor<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Cursor { data, pos: 0 }
    }

    /// A cursor over `data` starting at `pos`, or an error when `pos` is
    /// past the end.
    pub fn at(data: &'a [u8], pos: usize, what: &str) -> Result<Self> {
        if pos > data.len() {
            return Err(truncated(what));
        }
        Ok(Cursor { data, pos })
    }

    pub fn pos(&self) -> usize {
        self.pos
    }

    pub fn remaining(&self) -> usize {
        self.data.len() - self.pos
    }

    pub fn is_empty(&self) -> bool {
        self.remaining() == 0
    }

    pub fn bytes(&mut self, n: usize, what: &str) -> Result<&'a [u8]> {
        let end = self.pos.checked_add(n).filter(|&e| e <= self.data.len()).ok_or_else(|| truncated(what))?;
        let out = &self.data[self.pos..end];
        self.pos = end;
        Ok(out)
    }

    pub fn skip(&mut self, n: usize, what: &str) -> Result<()> {
        self.bytes(n, what).map(|_| ())
    }

    fn array<const N: usize>(&mut self, what: &str) -> Result<[u8; N]> {
        let b = self.bytes(N, what)?;
        let mut out = [0u8; N];
        out.copy_from_slice(b);
        Ok(out)
    }

    pub fn u8(&mut self, what: &str) -> Result<u8> {
        Ok(self.array::<1>(what)?[0])
    }

    pub fn i8(&mut self, what: &str) -> Result<i8> {
        Ok(self.array::<1>(what)?[0] as i8)
    }

    pub fn u16(&mut self, what: &str) -> Result<u16> {
        Ok(u16::from_le_bytes(self.array(what)?))
    }

    pub fn i16(&mut self, what: &str) -> Result<i16> {
        Ok(i16::from_le_bytes(self.array(what)?))
    }

    pub fn u32(&mut self, what: &str) -> Result<u32> {
        Ok(u32::from_le_bytes(self.array(what)?))
    }

    pub fn i32(&mut self, what: &str) -> Result<i32> {
        Ok(i32::from_le_bytes(self.array(what)?))
    }

    /// A float, with NaN and infinity replaced by 0 so that one bad value
    /// can't poison bounds, normals or the camera.
    pub fn f32(&mut self, what: &str) -> Result<f32> {
        let v = f32::from_le_bytes(self.array(what)?);
        Ok(if v.is_finite() { v } else { 0.0 })
    }

    pub fn vec2(&mut self, what: &str) -> Result<Vec2> {
        Ok(Vec2::new(self.f32(what)?, self.f32(what)?))
    }

    pub fn vec3(&mut self, what: &str) -> Result<Vec3> {
        Ok(Vec3::new(self.f32(what)?, self.f32(what)?, self.f32(what)?))
    }

    pub fn vec4(&mut self, what: &str) -> Result<Vec4> {
        Ok(Vec4::new(self.f32(what)?, self.f32(what)?, self.f32(what)?, self.f32(what)?))
    }

    /// A quaternion stored as x, y, z, w.
    pub fn quat(&mut self, what: &str) -> Result<Quat> {
        let v = self.vec4(what)?;
        Ok(Quat::from_vec4(v))
    }

    /// A count of records of at least `record` bytes each, checked against
    /// the bytes left so that nothing is allocated for records the file
    /// doesn't hold.
    pub fn count(&self, count: i64, record: usize, what: &str) -> Result<usize> {
        if count < 0 {
            return Err(LoadError::invalid(format!("negative {what} count")));
        }
        let count = usize::try_from(count).map_err(|_| LoadError::invalid(format!("{what} count out of range")))?;
        if count.checked_mul(record.max(1)).is_none_or(|n| n > self.remaining()) {
            return Err(LoadError::invalid(format!("{count} {what} don't fit in the file")));
        }
        Ok(count)
    }
}

//endregion

/*┌─────────────────╮
| Text            |
└─────────────────╯*/
//region Text

/// Fixed-size, zero-padded name fields (PSK, PSA, PMD, VMD) as a string:
/// up to the first zero byte, decoded by `decode`.
pub fn fixed_name(bytes: &[u8], decode: impl Fn(&[u8]) -> String) -> String {
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    decode(&bytes[..end]).trim().to_string()
}

/// Windows-1252 (Latin-1 for the printable range), which Unreal's tools
/// write names in.
pub fn latin1(bytes: &[u8]) -> String {
    bytes.iter().map(|&b| b as char).collect()
}

/// Shift-JIS, which MikuMikuDance's PMD models and VMD motions use for
/// their Japanese names.
pub fn shift_jis(bytes: &[u8]) -> String {
    let (text, _, _) = encoding_rs::SHIFT_JIS.decode(bytes);
    text.into_owned()
}

/// Splits a text file into lines, accepting `\n`, `\r\n` and lone `\r`.
pub fn lines(text: &str) -> impl Iterator<Item = &str> {
    text.split(['\n', '\r']).filter(|l| !l.is_empty())
}

/// The file as text: UTF-8 when it is, else Latin-1, so a stray byte in a
/// comment doesn't refuse the file.
pub fn text(bytes: &[u8]) -> std::borrow::Cow<'_, str> {
    let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
    match std::str::from_utf8(bytes) {
        Ok(s) => std::borrow::Cow::Borrowed(s),
        Err(_) => std::borrow::Cow::Owned(latin1(bytes)),
    }
}

/// Splits a line into words, keeping quoted strings (without their quotes)
/// as single words, and dropping a `//` comment.  Unterminated quotes run
/// to the end of the line.
pub fn words(line: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut rest = line.trim_start();
    while !rest.is_empty() {
        if rest.starts_with("//") {
            break;
        }
        if let Some(inner) = rest.strip_prefix('"') {
            let end = inner.find('"').unwrap_or(inner.len());
            out.push(&inner[..end]);
            rest = inner.get(end + 1..).unwrap_or("").trim_start();
            continue;
        }
        let end = rest.find(|c: char| c.is_whitespace()).unwrap_or(rest.len());
        out.push(&rest[..end]);
        rest = rest[end..].trim_start();
    }
    out
}

/// A float written in a text format; non-finite values count as missing.
pub fn float(word: &str) -> Option<f32> {
    word.parse::<f32>().ok().filter(|v| v.is_finite())
}

/// An integer written in a text format, which some exporters write with a
/// trailing `.0`.
pub fn int(word: &str) -> Option<i64> {
    word.parse::<i64>().ok().or_else(|| float(word).filter(|v| v.fract() == 0.0 && v.abs() < 1e15).map(|v| v as i64))
}

//endregion

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_are_checked_against_the_bytes_left() {
        let data = [0u8; 16];
        let c = Cursor::new(&data);
        assert_eq!(c.count(4, 4, "x").unwrap(), 4);
        assert!(c.count(5, 4, "x").is_err());
        assert!(c.count(-1, 4, "x").is_err());
        assert!(c.count(i64::MAX, 8, "x").is_err());
        assert!(Cursor::at(&data, 17, "x").is_err());
    }

    #[test]
    fn floats_are_finite() {
        let mut data = Vec::new();
        data.extend_from_slice(&f32::NAN.to_le_bytes());
        data.extend_from_slice(&f32::INFINITY.to_le_bytes());
        data.extend_from_slice(&1.5f32.to_le_bytes());
        let mut c = Cursor::new(&data);
        assert_eq!(c.vec3("v").unwrap(), Vec3::new(0.0, 0.0, 1.5));
        assert!(c.u8("end").is_err());
    }

    #[test]
    fn words_keep_quoted_strings() {
        assert_eq!(words("  \"a b\" 1 2 // comment"), ["a b", "1", "2"]);
        assert_eq!(words("shader \"models/x\""), ["shader", "models/x"]);
        assert_eq!(words("\"unterminated"), ["unterminated"]);
        assert_eq!(words("tri 0 1 2"), ["tri", "0", "1", "2"]);
        assert!(words("// nothing").is_empty());
        assert_eq!(int("3.0"), Some(3));
        assert_eq!(int("x"), None);
    }

    #[test]
    fn names() {
        assert_eq!(fixed_name(b"Bip01\0junk", latin1), "Bip01");
        // "センター" (centre) in Shift-JIS.
        assert_eq!(fixed_name(b"\x83\x5a\x83\x93\x83\x5e\x81\x5b\0\0", shift_jis), "センター");
    }
}
