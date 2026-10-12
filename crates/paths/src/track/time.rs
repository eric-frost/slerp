//! Times as GPX, TCX and KML write them: ISO 8601 / RFC 3339
//! (`2024-05-01T10:20:30Z`, `2024-05-01T12:20:30.250+02:00`).

/// Seconds since 1970 (UTC), or `None` when `s` isn't such a time.  A
/// time without a zone counts as UTC.
pub fn parse_iso8601(s: &str) -> Option<f64> {
    let b = s.trim().as_bytes();
    let num = |from: usize, len: usize| -> Option<i64> {
        let d = b.get(from..from + len)?;
        if !d.iter().all(u8::is_ascii_digit) {
            return None;
        }
        Some(d.iter().fold(0i64, |v, c| v * 10 + (c - b'0') as i64))
    };
    let year = num(0, 4)?;
    if b.get(4) != Some(&b'-') || b.get(7) != Some(&b'-') {
        return None;
    }
    let (month, day) = (num(5, 2)?, num(8, 2)?);
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    let mut t = days_from_civil(year, month, day) as f64 * 86400.0;
    let mut i = 10;
    if matches!(b.get(i), Some(b'T' | b't' | b' ')) {
        let (h, m) = (num(i + 1, 2)?, num(i + 4, 2)?);
        if b.get(i + 3) != Some(&b':') || h > 24 || m > 59 {
            return None;
        }
        t += (h * 3600 + m * 60) as f64;
        i += 6;
        if b.get(i) == Some(&b':') {
            let s = num(i + 1, 2)?;
            t += s as f64;
            i += 3;
            if matches!(b.get(i), Some(b'.' | b',')) {
                i += 1;
                let mut scale = 0.1;
                while let Some(d) = b.get(i).filter(|d| d.is_ascii_digit()) {
                    t += (d - b'0') as f64 * scale;
                    scale *= 0.1;
                    i += 1;
                }
            }
        }
        if let Some(b'+' | b'-') = b.get(i) {
            let sign = if b[i] == b'-' { -1.0 } else { 1.0 };
            let hh = num(i + 1, 2)?;
            let mm = if b.get(i + 3) == Some(&b':') { num(i + 4, 2) } else { num(i + 3, 2) }.unwrap_or(0);
            t -= sign * (hh * 3600 + mm * 60) as f64;
        }
    }
    Some(t)
}

/// Days from 1970-01-01 to a date in the proleptic Gregorian calendar
/// (Howard Hinnant's algorithm).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

#[cfg(test)]
mod tests {
    use super::parse_iso8601;

    #[test]
    fn reads_the_usual_forms() {
        assert_eq!(parse_iso8601("1970-01-01T00:00:00Z"), Some(0.0));
        assert_eq!(parse_iso8601("2024-05-01T10:20:30Z"), Some(1714558830.0));
        assert_eq!(parse_iso8601("2024-05-01T12:20:30.25+02:00"), Some(1714558830.25));
        assert_eq!(parse_iso8601("2024-05-01T05:20:30-0500"), Some(1714558830.0));
        assert_eq!(parse_iso8601("2024-05-01"), Some(1714521600.0));
        assert_eq!(parse_iso8601("2000-02-29T00:00:00"), Some(951782400.0));
        assert_eq!(parse_iso8601("yesterday"), None);
        assert_eq!(parse_iso8601("2024-13-01T00:00:00Z"), None);
    }
}
