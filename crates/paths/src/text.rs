//! Small pieces for reading text formats quickly and safely: splitting a
//! stream into lines with a cap on their length, numbers without the
//! overhead of `str::parse`, and a forgiving base64 decoder.

use std::io::Read;

/// Splits byte chunks into lines, which may straddle chunks.  A line
/// longer than `max` is cut at `max` bytes and the rest of it dropped, so
/// a file without newlines can't make it grow without bound.  Lines come
/// without their `\n` or `\r\n`.
pub struct Lines {
    carry: Vec<u8>,
    max: usize,
    overflow: bool,
}

impl Lines {
    pub fn new(max: usize) -> Lines {
        Lines { carry: Vec::new(), max: max.max(1), overflow: false }
    }

    /// Feeds a chunk, calling `f` for each line completed in it.  `f`
    /// returns false to stop, which `push` passes on.
    pub fn push(&mut self, mut chunk: &[u8], f: &mut impl FnMut(&[u8]) -> bool) -> bool {
        while !chunk.is_empty() {
            match memchr::memchr(b'\n', chunk) {
                Some(i) => {
                    let part = &chunk[..i];
                    chunk = &chunk[i + 1..];
                    let go = if self.carry.is_empty() && !self.overflow {
                        f(trim_cr(&part[..part.len().min(self.max)]))
                    } else {
                        self.append(part);
                        let go = f(trim_cr(&self.carry));
                        self.carry.clear();
                        self.overflow = false;
                        go
                    };
                    if !go {
                        return false;
                    }
                }
                None => {
                    self.append(chunk);
                    break;
                }
            }
        }
        true
    }

    /// Hands over a last line that had no newline after it.
    pub fn finish(&mut self, f: &mut impl FnMut(&[u8]) -> bool) {
        if !self.carry.is_empty() {
            f(trim_cr(&self.carry));
            self.carry.clear();
        }
        self.overflow = false;
    }

    fn append(&mut self, part: &[u8]) {
        let room = self.max - self.carry.len().min(self.max);
        if part.len() > room {
            self.overflow = true;
        }
        self.carry.extend_from_slice(&part[..part.len().min(room)]);
    }
}

fn trim_cr(s: &[u8]) -> &[u8] {
    s.strip_suffix(b"\r").unwrap_or(s)
}

/// Reads `reader` in chunks of `size` bytes, handing each to `f` until it
/// returns false or the reader ends.  Interrupted reads are retried; other
/// errors end the stream as if the file ended there, since a loader shows
/// what it could read of a damaged file.
pub fn for_chunks(mut reader: impl Read, size: usize, mut f: impl FnMut(&[u8]) -> bool) -> std::io::Result<()> {
    let mut buf = vec![0u8; size.max(1)];
    loop {
        let n = match reader.read(&mut buf) {
            Ok(0) => return Ok(()),
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        if !f(&buf[..n]) {
            return Ok(());
        }
    }
}

pub fn trim(s: &[u8]) -> &[u8] {
    s.trim_ascii()
}

/// Whether `s` starts with `prefix`, ignoring ASCII case.
pub fn starts_with_ci(s: &[u8], prefix: &[u8]) -> bool {
    s.len() >= prefix.len() && s[..prefix.len()].eq_ignore_ascii_case(prefix)
}

/// Whether `s` contains `needle`, ignoring ASCII case.  `needle` must be
/// lower case.
pub fn contains_ci(s: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() {
        return true;
    }
    s.windows(needle.len()).any(|w| w.eq_ignore_ascii_case(needle))
}

/// Powers of ten that f64 holds exactly.
const POW10: [f64; 19] =
    [1e0, 1e1, 1e2, 1e3, 1e4, 1e5, 1e6, 1e7, 1e8, 1e9, 1e10, 1e11, 1e12, 1e13, 1e14, 1e15, 1e16, 1e17, 1e18];

/// Parses a decimal number at the start of `s` the way G-code writes them:
/// an optional sign, digits and a decimal point, no exponent (in G-code
/// without spaces, `X1E2` is an X word and an E word).  Returns the value
/// and the bytes used, or `None` when no digit follows.  Up to 18
/// significant digits are exact to the f64 nearest the decimal; beyond
/// that the extra digits only scale the value.
pub fn parse_number(s: &[u8]) -> Option<(f64, usize)> {
    let mut i = 0;
    let negative = match s.first() {
        Some(b'-') => {
            i = 1;
            true
        }
        Some(b'+') => {
            i = 1;
            false
        }
        _ => false,
    };
    // 1. Digits before and after the point, as one integer while it fits.
    let mut mantissa: u64 = 0;
    let mut kept = 0u32;
    let mut dropped_int = 0i32;
    let mut scale = 0usize;
    let mut any = false;
    while let Some(&c) = s.get(i) {
        let d = c.wrapping_sub(b'0');
        if d > 9 {
            break;
        }
        any = true;
        if kept < 18 {
            mantissa = mantissa * 10 + d as u64;
            kept += (mantissa != 0) as u32;
        } else {
            dropped_int += 1;
        }
        i += 1;
    }
    if s.get(i) == Some(&b'.') {
        i += 1;
        while let Some(&c) = s.get(i) {
            let d = c.wrapping_sub(b'0');
            if d > 9 {
                break;
            }
            any = true;
            if kept < 18 && scale < 18 {
                mantissa = mantissa * 10 + d as u64;
                kept += (mantissa != 0) as u32;
                scale += 1;
            }
            i += 1;
        }
    }
    if !any {
        return None;
    }
    // 2. One division by an exact power of ten rounds once, so 0.6 reads
    //    as the f64 nearest 0.6.
    let mut v = mantissa as f64 / POW10[scale];
    if dropped_int > 0 {
        v *= 10f64.powi(dropped_int);
    }
    Some((if negative { -v } else { v }, i))
}

/// Decodes base64, skipping anything that isn't part of the alphabet
/// (spaces, comment markers, line breaks) and stopping at padding.  Both
/// the standard and the URL-safe alphabets are read.  At most `max` bytes
/// come out.
pub fn base64_decode(input: &[u8], max: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity((input.len() / 4 * 3).min(max));
    let mut acc: u32 = 0;
    let mut bits = 0u32;
    for &c in input {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            b'=' => break,
            _ => continue,
        };
        acc = (acc << 6) | v as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            if out.len() >= max {
                break;
            }
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    out
}

/// Decodes hexadecimal digit pairs, skipping anything else.
pub fn hex_decode(input: &[u8], max: usize) -> Vec<u8> {
    let mut out = Vec::new();
    let mut high: Option<u8> = None;
    for &c in input {
        let v = match c {
            b'0'..=b'9' => c - b'0',
            b'a'..=b'f' => c - b'a' + 10,
            b'A'..=b'F' => c - b'A' + 10,
            _ => continue,
        };
        match high.take() {
            None => high = Some(v),
            Some(h) => {
                if out.len() >= max {
                    break;
                }
                out.push(h << 4 | v);
            }
        }
    }
    out
}

/// Parses a duration as slicers write it: `1d 2h 3m 4s`, `56m 41s`,
/// `1 hours 5 minutes`, `1:02:03`, or plain seconds.
pub fn parse_duration(s: &str) -> Option<f64> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    if let Ok(v) = s.parse::<f64>() {
        return (v.is_finite() && v >= 0.0).then_some(v);
    }
    if s.contains(':') && s.bytes().all(|b| b.is_ascii_digit() || b == b':' || b == b'.') {
        let mut total = 0.0;
        for part in s.split(':') {
            total = total * 60.0 + part.parse::<f64>().ok()?;
        }
        return Some(total);
    }
    let mut total = 0.0;
    let mut any = false;
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        let Some((v, n)) = parse_number(&b[i..]) else {
            i += 1;
            continue;
        };
        i += n;
        while i < b.len() && b[i] == b' ' {
            i += 1;
        }
        let unit = b.get(i).copied().unwrap_or(b's').to_ascii_lowercase();
        let factor = match unit {
            b'd' => 86400.0,
            b'h' => 3600.0,
            b'm' => 60.0,
            b's' => 1.0,
            _ => continue,
        };
        total += v * factor;
        any = true;
    }
    any.then_some(total)
}

/// Formats seconds as `1 h 02 min`, `12 min 05 s` or `42 s`.
pub fn format_duration(s: f64) -> String {
    let s = s.max(0.0).round() as u64;
    let (d, h, m, sec) = (s / 86400, s / 3600 % 24, s / 60 % 60, s % 60);
    if d > 0 {
        format!("{d} d {h} h {m:02} min")
    } else if h > 0 {
        format!("{h} h {m:02} min")
    } else if m > 0 {
        format!("{m} min {sec:02} s")
    } else {
        format!("{sec} s")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_lines_across_chunks_and_caps_them() {
        let mut lines = Lines::new(8);
        let mut got = Vec::new();
        let mut f = |l: &[u8]| {
            got.push(String::from_utf8_lossy(l).into_owned());
            true
        };
        lines.push(b"G1 X1\r\nG1 Y", &mut f);
        lines.push(b"2\n", &mut f);
        lines.push(b"a very long line indeed\nend", &mut f);
        lines.finish(&mut f);
        assert_eq!(got, ["G1 X1", "G1 Y2", "a very l", "end"]);
    }

    #[test]
    fn numbers_as_gcode_writes_them() {
        assert_eq!(parse_number(b"12.5X"), Some((12.5, 4)));
        assert_eq!(parse_number(b"-.25"), Some((-0.25, 4)));
        assert_eq!(parse_number(b"+3"), Some((3.0, 2)));
        assert_eq!(parse_number(b"1E2"), Some((1.0, 1)));
        assert_eq!(parse_number(b"-"), None);
        assert_eq!(parse_number(b"."), None);
        let (v, _) = parse_number(b"123456789012345678901234.5").unwrap();
        assert!((v / 1.2345678901234568e23 - 1.0).abs() < 1e-12);
    }

    #[test]
    fn base64_and_hex() {
        assert_eq!(base64_decode(b"; aGVs\n; bG8=", 100), b"hello");
        assert_eq!(base64_decode(b"aGVsbG8", 3), b"hel");
        assert_eq!(hex_decode(b"0aFf 1", 10), [0x0a, 0xff]);
    }

    #[test]
    fn durations() {
        assert_eq!(parse_duration("1d 2h 3m 4s"), Some(93784.0));
        assert_eq!(parse_duration("56m 41s"), Some(3401.0));
        assert_eq!(parse_duration("1 hours 5 minutes"), Some(3900.0));
        assert_eq!(parse_duration("1234"), Some(1234.0));
        assert_eq!(parse_duration("1:02:03"), Some(3723.0));
        assert_eq!(parse_duration("soon"), None);
        assert_eq!(format_duration(3723.0), "1 h 02 min");
    }
}
