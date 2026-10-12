//! The group-code stream every DXF file is made of: pairs of a number (the
//! group code, which says what the value means) and a value.  Text files
//! put each on a line of its own; binary files store the code as a short
//! integer and the value in the type the code implies.
//!
//! The reader never fails on bad input.  A code that isn't a number is
//! skipped a line at a time; running out of file ends the stream.  Values
//! stay as the bytes they were read from and are converted when asked, which
//! keeps parsing a 50 MB file of coordinates fast: only the numbers that
//! are used get parsed.

/// The sentinel that opens a binary DXF file, as AutoCAD writes it.
#[cfg(test)]
pub const BINARY_SENTINEL: &[u8; 22] = b"AutoCAD Binary DXF\r\n\x1a\0";

/// Where the pairs of a binary DXF file start, or `None` for a file that
/// isn't one.  Some writers leave the carriage return out of the sentinel.
pub fn binary_start(data: &[u8]) -> Option<usize> {
    let rest = data.strip_prefix(b"AutoCAD Binary DXF")?;
    let skip = rest.iter().take(4).position(|&b| b == 0x1a).map(|p| p + 2)?;
    // Everything between the name and the Ctrl-Z is line ending.
    rest[..skip - 2].iter().all(|b| matches!(b, b'\r' | b'\n')).then_some(18 + skip)
}

/// A value as it sits in the file.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Value<'a> {
    /// A line of a text file: a string or a number not yet read.
    Text(&'a [u8]),
    Int(i64),
    Float(f64),
    /// Binary chunks (group codes 310 to 319 and 1004).
    Bin(&'a [u8]),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Pair<'a> {
    pub code: i32,
    pub value: Value<'a>,
}

impl<'a> Pair<'a> {
    pub fn f64(&self) -> f64 {
        match self.value {
            Value::Text(t) => parse_f64(t),
            Value::Int(i) => i as f64,
            Value::Float(f) => f,
            Value::Bin(_) => 0.0,
        }
    }

    pub fn i64(&self) -> i64 {
        match self.value {
            Value::Text(t) => parse_i64(t),
            Value::Int(i) => i,
            Value::Float(f) => f as i64,
            Value::Bin(_) => 0,
        }
    }

    pub fn i32(&self) -> i32 {
        self.i64().clamp(i32::MIN as i64, i32::MAX as i64) as i32
    }

    /// The raw bytes of a string value (without the line ending).
    pub fn bytes(&self) -> &'a [u8] {
        match self.value {
            Value::Text(t) | Value::Bin(t) => t,
            _ => &[],
        }
    }

    /// True for a pair that starts something: code 0 with this name.
    pub fn is(&self, code: i32, name: &str) -> bool {
        self.code == code && self.bytes().trim_ascii() == name.as_bytes()
    }
}

/// Reads pairs from the bytes of a whole file.
pub struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
    mode: Mode,
    peeked: Option<Pair<'a>>,
    /// Lines that weren't group codes, skipped on the way.
    pub skipped_lines: u32,
}

#[derive(Clone, Copy)]
enum Mode {
    /// Text, with the byte that ends a line (a few very old files end lines
    /// with a lone carriage return).
    Text(u8),
    /// Binary; R12 files have one-byte group codes.
    Binary { short_codes: bool },
}

/// Too many unreadable lines in a row mean the rest isn't DXF.
const MAX_SKIPPED_LINES: u32 = 64;

impl<'a> Reader<'a> {
    pub fn new(data: &'a [u8]) -> Reader<'a> {
        if let Some(start) = binary_start(data) {
            let short_codes = binary_is_r12(data, start);
            return Reader { data, pos: start, mode: Mode::Binary { short_codes }, peeked: None, skipped_lines: 0 };
        }
        // A byte order mark, then lines ended by \n, \r\n or (rarely) \r.
        let start = if data.starts_with(&[0xEF, 0xBB, 0xBF]) { 3 } else { 0 };
        let head = &data[start..data.len().min(start + 4096)];
        let eol =
            if memchr::memchr(b'\n', head).is_none() && memchr::memchr(b'\r', head).is_some() { b'\r' } else { b'\n' };
        Reader { data, pos: start, mode: Mode::Text(eol), peeked: None, skipped_lines: 0 }
    }

    /// A reader of the same encoding as `self` (text with its line ending,
    /// or binary with its code width) over `data` from `pos`, for reading
    /// one section found by searching, without the rest of the file.
    pub fn like<'b>(&self, data: &'b [u8], pos: usize) -> Reader<'b> {
        Reader { data, pos: pos.min(data.len()), mode: self.mode, peeked: None, skipped_lines: 0 }
    }

    pub fn is_binary(&self) -> bool {
        matches!(self.mode, Mode::Binary { .. })
    }

    pub fn next(&mut self) -> Option<Pair<'a>> {
        if let Some(p) = self.peeked.take() {
            return Some(p);
        }
        self.read()
    }

    pub fn peek(&mut self) -> Option<&Pair<'a>> {
        if self.peeked.is_none() {
            self.peeked = self.read();
        }
        self.peeked.as_ref()
    }

    fn read(&mut self) -> Option<Pair<'a>> {
        loop {
            let pair = match self.mode {
                Mode::Text(eol) => self.read_text(eol)?,
                Mode::Binary { short_codes } => self.read_binary(short_codes)?,
            };
            // 999 is a comment.
            if pair.code != 999 {
                return Some(pair);
            }
        }
    }

    fn line(&mut self, eol: u8) -> Option<&'a [u8]> {
        if self.pos >= self.data.len() {
            return None;
        }
        let rest = &self.data[self.pos..];
        let (line, used) = match memchr::memchr(eol, rest) {
            Some(i) => (&rest[..i], i + 1),
            None => (rest, rest.len()),
        };
        self.pos += used;
        Some(if eol == b'\n' { line.strip_suffix(b"\r").unwrap_or(line) } else { line })
    }

    fn read_text(&mut self, eol: u8) -> Option<Pair<'a>> {
        let mut consecutive = 0;
        loop {
            let code_line = self.line(eol)?;
            if let Some(code) = parse_code(code_line) {
                let value = self.line(eol)?;
                return Some(Pair { code, value: Value::Text(value) });
            }
            // Not a group code: the pairs are out of step, so skip a line
            // and try again, but not for ever.
            consecutive += 1;
            self.skipped_lines += 1;
            if consecutive > MAX_SKIPPED_LINES {
                self.pos = self.data.len();
                return None;
            }
        }
    }

    fn read_binary(&mut self, short_codes: bool) -> Option<Pair<'a>> {
        let d = self.data;
        let mut i = self.pos;
        let code = if short_codes {
            let c = *d.get(i)?;
            if c == 255 {
                let v = u16::from_le_bytes([*d.get(i + 1)?, *d.get(i + 2)?]);
                i += 3;
                v as i32
            } else {
                i += 1;
                c as i32
            }
        } else {
            let v = u16::from_le_bytes([*d.get(i)?, *d.get(i + 1)?]);
            i += 2;
            v as i32
        };
        let value = match binary_type(code) {
            BinType::Chunk => {
                let n = *d.get(i)? as usize;
                let chunk = d.get(i + 1..i + 1 + n)?;
                i += 1 + n;
                Value::Bin(chunk)
            }
            BinType::I16 => {
                let v = i16::from_le_bytes(d.get(i..i + 2)?.try_into().ok()?);
                i += 2;
                Value::Int(v as i64)
            }
            BinType::I32 => {
                let v = i32::from_le_bytes(d.get(i..i + 4)?.try_into().ok()?);
                i += 4;
                Value::Int(v as i64)
            }
            BinType::I64 => {
                let v = i64::from_le_bytes(d.get(i..i + 8)?.try_into().ok()?);
                i += 8;
                Value::Int(v)
            }
            BinType::F64 => {
                let v = f64::from_le_bytes(d.get(i..i + 8)?.try_into().ok()?);
                i += 8;
                Value::Float(v)
            }
            BinType::Byte => {
                let v = *d.get(i)?;
                i += 1;
                Value::Int(v as i64)
            }
            BinType::Str => {
                let rest = d.get(i..)?;
                let n = memchr::memchr(0, rest).unwrap_or(rest.len());
                let s = &rest[..n];
                i += (n + 1).min(rest.len());
                Value::Text(s)
            }
        };
        self.pos = i;
        Some(Pair { code, value })
    }
}

/// R12 and older binary files use one-byte group codes.  Every file starts
/// with the pair `0 SECTION`: the code is `00` (R12) or `00 00` (R13 and
/// later), followed by the text.  Failing that, the version in the header
/// decides.
fn binary_is_r12(data: &[u8], start: usize) -> bool {
    match (data.get(start), data.get(start + 1)) {
        (Some(0), Some(0)) => return false,
        (Some(0), Some(c)) if c.is_ascii_graphic() => return true,
        _ => {}
    }
    let head = &data[..data.len().min(1024)];
    let Some(at) = head.windows(8).position(|w| w == b"$ACADVER") else { return true };
    // After the name's terminating zero comes the code of the value (one
    // or two bytes), then the version string.
    let mut i = at + 9;
    if head.get(i) == Some(&1) && head.get(i + 1) == Some(&0) {
        i += 2;
    } else if head.get(i) == Some(&1) {
        i += 1;
    }
    match head.get(i..i + 6) {
        Some(v) => v <= b"AC1009".as_slice(),
        None => false,
    }
}

#[derive(Clone, Copy)]
enum BinType {
    Str,
    I16,
    I32,
    I64,
    F64,
    Byte,
    Chunk,
}

/// The type a group code's value has in a binary file (the ranges of the
/// DXF reference).
fn binary_type(code: i32) -> BinType {
    match code {
        310..=319 | 1004 => BinType::Chunk,
        10..=59 | 110..=149 | 210..=239 | 460..=469 | 1010..=1059 => BinType::F64,
        60..=79 | 170..=179 | 270..=289 | 370..=389 | 400..=409 | 1060..=1070 => BinType::I16,
        90..=99 | 420..=459 | 1071 => BinType::I32,
        160..=169 => BinType::I64,
        290..=299 => BinType::Byte,
        _ => BinType::Str,
    }
}

/// A group code: whitespace around a possibly negative integer.
fn parse_code(line: &[u8]) -> Option<i32> {
    let s = line.trim_ascii();
    if s.is_empty() || s.len() > 6 {
        return None;
    }
    let (neg, digits) = match s[0] {
        b'-' => (true, &s[1..]),
        b'+' => (false, &s[1..]),
        _ => (false, s),
    };
    if digits.is_empty() {
        return None;
    }
    let mut v: i32 = 0;
    for &c in digits {
        if !c.is_ascii_digit() {
            return None;
        }
        v = v * 10 + (c - b'0') as i32;
    }
    Some(if neg { -v } else { v })
}

pub fn parse_i64(s: &[u8]) -> i64 {
    let s = s.trim_ascii();
    let (neg, digits) = match s.first() {
        Some(b'-') => (true, &s[1..]),
        Some(b'+') => (false, &s[1..]),
        _ => (false, s),
    };
    let mut v: i64 = 0;
    for &c in digits {
        if c.is_ascii_digit() {
            v = v.saturating_mul(10).saturating_add((c - b'0') as i64);
        } else if c == b'.' || c == b'e' || c == b'E' {
            // A real where an integer was expected.
            return parse_f64(s) as i64;
        } else {
            break;
        }
    }
    if neg { -v } else { v }
}

/// Powers of ten that a double holds exactly.
const POW10: [f64; 23] = [
    1e0, 1e1, 1e2, 1e3, 1e4, 1e5, 1e6, 1e7, 1e8, 1e9, 1e10, 1e11, 1e12, 1e13, 1e14, 1e15, 1e16, 1e17, 1e18, 1e19, 1e20,
    1e21, 1e22,
];

/// Reads a real number; anything unreadable is 0.  Plain decimals with up
/// to 15 digits are done here with exact arithmetic (a mantissa below 2^53
/// divided or multiplied by an exactly representable power of ten rounds
/// once, so the answer is the correctly rounded one), which is several
/// times faster than the general routine; everything else goes to it.
pub fn parse_f64(s: &[u8]) -> f64 {
    let s = s.trim_ascii();
    let Some(&first) = s.first() else { return 0.0 };
    let mut i = 0;
    let neg = first == b'-';
    if first == b'-' || first == b'+' {
        i = 1;
    }
    let mut mant: u64 = 0;
    let mut digits = 0;
    let mut frac = 0i32;
    let mut seen_dot = false;
    while let Some(&c) = s.get(i) {
        match c {
            b'0'..=b'9' => {
                if digits < 15 {
                    mant = mant * 10 + (c - b'0') as u64;
                    if mant != 0 {
                        digits += 1;
                    }
                    if seen_dot {
                        frac += 1;
                    }
                } else {
                    return slow(s);
                }
            }
            b'.' if !seen_dot => seen_dot = true,
            _ => break,
        }
        i += 1;
    }
    let mut exp = 0i32;
    if let Some(&c) = s.get(i) {
        if matches!(c, b'e' | b'E' | b'd' | b'D') {
            let rest = &s[i + 1..];
            if rest.is_empty() || rest.len() > 5 {
                return slow(s);
            }
            let (eneg, ed) = match rest[0] {
                b'-' => (true, &rest[1..]),
                b'+' => (false, &rest[1..]),
                _ => (false, rest),
            };
            if ed.is_empty() || !ed.iter().all(u8::is_ascii_digit) {
                return slow(s);
            }
            let e: i32 = ed.iter().fold(0, |a, &c| a * 10 + (c - b'0') as i32);
            exp = if eneg { -e } else { e };
        } else {
            // Trailing junk: take what parsed, as C's atof would.
            if i == 0 || (i == 1 && (neg || first == b'+')) {
                return 0.0;
            }
        }
    }
    let e10 = exp - frac;
    let v = if (0..=22).contains(&e10) {
        mant as f64 * POW10[e10 as usize]
    } else if (-22..0).contains(&e10) {
        mant as f64 / POW10[(-e10) as usize]
    } else {
        return slow(s);
    };
    if neg { -v } else { v }
}

fn slow(s: &[u8]) -> f64 {
    let Ok(t) = std::str::from_utf8(s) else { return 0.0 };
    let t = t.replace(['d', 'D'], "e");
    match t.parse::<f64>() {
        Ok(v) if v.is_finite() => v,
        _ => 0.0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pairs(data: &[u8]) -> Vec<(i32, String)> {
        let mut r = Reader::new(data);
        let mut out = Vec::new();
        while let Some(p) = r.next() {
            out.push((p.code, String::from_utf8_lossy(p.bytes()).into_owned()));
        }
        out
    }

    #[test]
    fn text_pairs_in_every_line_ending() {
        let want = vec![(0, "SECTION".to_string()), (2, "HEADER".to_string())];
        assert_eq!(pairs(b"  0\nSECTION\n  2\nHEADER\n"), want);
        assert_eq!(pairs(b"  0\r\nSECTION\r\n  2\r\nHEADER\r\n"), want);
        assert_eq!(pairs(b"  0\rSECTION\r  2\rHEADER\r"), want);
        assert_eq!(pairs(b"0\nSECTION\n2\nHEADER"), want);
        assert_eq!(pairs(b"\xEF\xBB\xBF0\nSECTION\n2\nHEADER\n"), want);
    }

    #[test]
    fn comments_are_skipped_and_stray_lines_resync() {
        assert_eq!(pairs(b"999\nmade by hand\n0\nEOF\n"), vec![(0, "EOF".to_string())]);
        // A stray line shifts the pairs; the reader skips it.
        let got = pairs(b"0\nSECTION\nstray\n2\nHEADER\n");
        assert_eq!(got, vec![(0, "SECTION".to_string()), (2, "HEADER".to_string())]);
        // Garbage ends the stream rather than looping.
        let junk = "x\n".repeat(1000);
        assert!(pairs(junk.as_bytes()).is_empty());
        // A pair cut off at the end of the file is dropped.
        assert_eq!(pairs(b"0\nSECTION\n2\n"), vec![(0, "SECTION".to_string())]);
    }

    #[test]
    fn numbers() {
        for (s, v) in [
            ("1.5", 1.5),
            ("  -2.25 ", -2.25),
            ("+3", 3.0),
            ("0.1", 0.1),
            (".5", 0.5),
            ("5.", 5.0),
            ("1e3", 1000.0),
            ("1.5E-3", 0.0015),
            ("2.5D2", 250.0),
            ("123456789.123456", 123456789.123456),
            ("0.30000000000000004", 0.30000000000000004),
            ("1234567890123456789", 1234567890123456789.0),
            ("1e300", 1e300),
            ("-0", 0.0),
            ("", 0.0),
            ("abc", 0.0),
            ("1e", 0.0),
            ("NaN", 0.0),
            ("1e999", 0.0),
        ] {
            let got = parse_f64(s.as_bytes());
            assert!((got - v).abs() <= v.abs() * 1e-15, "{s:?}: {got} != {v}");
        }
        assert_eq!(parse_i64(b" 42 "), 42);
        assert_eq!(parse_i64(b"-7"), -7);
        assert_eq!(parse_i64(b"3.0"), 3);
        assert_eq!(parse_i64(b"99999999999999999999999"), i64::MAX);
    }

    /// The fast path must agree bit for bit with the standard routine on the
    /// shapes of numbers CAD programs write.
    #[test]
    fn fast_parse_is_exact() {
        let mut x: u64 = 12345;
        for _ in 0..20000 {
            x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            let int = (x >> 40) % 100000;
            let frac = (x >> 8) % 1000000;
            let s = format!("{}{}.{:06}", if x & 1 == 0 { "-" } else { "" }, int, frac);
            assert_eq!(parse_f64(s.as_bytes()), s.parse::<f64>().unwrap(), "{s}");
        }
    }

    #[test]
    fn binary_pairs() {
        let mut d = BINARY_SENTINEL.to_vec();
        // R13 style: two-byte codes.
        d.extend([0, 0]);
        d.extend(b"LINE\0");
        d.extend([10, 0]);
        d.extend(1.5f64.to_le_bytes());
        d.extend([70, 0]);
        d.extend(7i16.to_le_bytes());
        d.extend([90, 0]);
        d.extend(100000i32.to_le_bytes());
        d.extend([0x36, 0x01]); // 310 binary chunk
        d.extend([2, 0xAB, 0xCD]);
        d.extend([0x22, 0x01]); // 290 byte
        d.extend([1]);
        let mut r = Reader::new(&d);
        assert!(r.is_binary());
        let p = r.next().unwrap();
        assert!(p.is(0, "LINE"));
        assert_eq!(r.next().unwrap().f64(), 1.5);
        assert_eq!(r.next().unwrap().i32(), 7);
        assert_eq!(r.next().unwrap().i32(), 100000);
        assert_eq!(r.next().unwrap().bytes(), &[0xAB, 0xCD]);
        assert_eq!(r.next().unwrap().i32(), 1);
        assert!(r.next().is_none());
    }

    #[test]
    fn binary_r12_has_short_codes() {
        let mut d = BINARY_SENTINEL.to_vec();
        d.extend([0]);
        d.extend(b"SECTION\0");
        d.extend([2]);
        d.extend(b"HEADER\0");
        d.extend([9]);
        d.extend(b"$ACADVER\0");
        d.extend([1]);
        d.extend(b"AC1009\0");
        d.extend([255, 0xE8, 0x03]); // extended code 1000
        d.extend(b"xdata\0");
        let mut r = Reader::new(&d);
        let mut codes = vec![];
        while let Some(p) = r.next() {
            codes.push(p.code);
        }
        assert_eq!(codes, vec![0, 2, 9, 1, 1000]);
    }

    #[test]
    fn truncated_binary_stops() {
        let mut d = BINARY_SENTINEL.to_vec();
        d.extend([10, 0, 1, 2, 3]);
        let mut r = Reader::new(&d);
        assert!(r.next().is_none());
        // Every prefix of a valid stream reads without panicking.
        let mut full = BINARY_SENTINEL.to_vec();
        full.extend([0, 0]);
        full.extend(b"LINE\0");
        full.extend([10, 0]);
        full.extend(1.5f64.to_le_bytes());
        for n in 0..full.len() {
            let mut r = Reader::new(&full[..n]);
            while r.next().is_some() {}
        }
    }
}
