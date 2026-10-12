//! The entity lump of Quake and Half-Life maps: plain text blocks of quoted
//! key and value pairs, one block per entity, such as
//! `{ "classname" "light" "origin" "0 64 128" }`.

use slerp_scene::glam::Vec3;

#[derive(Clone, Debug, Default)]
pub struct Entity {
    pub pairs: Vec<(String, String)>,
}

impl Entity {
    /// The value of `key`.  When a key repeats, the last one wins, as in
    /// Quake's own parser.
    pub fn get(&self, key: &str) -> Option<&str> {
        self.pairs.iter().rev().find(|(k, _)| k.eq_ignore_ascii_case(key)).map(|(_, v)| v.as_str())
    }

    pub fn classname(&self) -> &str {
        self.get("classname").unwrap_or("")
    }

    /// Up to three numbers separated by spaces, such as an origin.
    pub fn vec3(&self, key: &str) -> Option<Vec3> {
        let mut it = self.get(key)?.split_whitespace().map(|s| s.parse::<f32>().ok().filter(|v| v.is_finite()));
        Some(Vec3::new(it.next()??, it.next()??, it.next()??))
    }

    pub fn float(&self, key: &str) -> Option<f32> {
        self.get(key)?.trim().parse::<f32>().ok().filter(|v| v.is_finite())
    }
}

/// Parses the whole lump.  Malformed text ends the list early rather than
/// failing, since entities only add markers and the spawn point.
pub fn parse(bytes: &[u8]) -> Vec<Entity> {
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    let text = String::from_utf8_lossy(&bytes[..end]);
    let mut tokens = Tokens { s: text.as_bytes(), at: 0 };
    let mut out = Vec::new();
    while let Some(t) = tokens.next() {
        if t != "{" {
            break;
        }
        let mut e = Entity::default();
        loop {
            match tokens.next() {
                Some(k) if k == "}" => break,
                Some(k) => match tokens.next() {
                    Some(v) if v != "}" => e.pairs.push((k, v)),
                    _ => {
                        out.push(e);
                        return out;
                    }
                },
                None => {
                    out.push(e);
                    return out;
                }
            }
        }
        out.push(e);
    }
    out
}

struct Tokens<'a> {
    s: &'a [u8],
    at: usize,
}

impl Tokens<'_> {
    /// The next brace, quoted string or bare word, skipping whitespace and
    /// `//` comments.
    fn next(&mut self) -> Option<String> {
        loop {
            while self.at < self.s.len() && self.s[self.at].is_ascii_whitespace() {
                self.at += 1;
            }
            if self.s[self.at..].starts_with(b"//") {
                while self.at < self.s.len() && self.s[self.at] != b'\n' {
                    self.at += 1;
                }
                continue;
            }
            break;
        }
        let c = *self.s.get(self.at)?;
        if c == b'{' || c == b'}' {
            self.at += 1;
            return Some((c as char).to_string());
        }
        if c == b'"' {
            let start = self.at + 1;
            let len = self.s[start..].iter().position(|&b| b == b'"').unwrap_or(self.s.len() - start);
            self.at = (start + len + 1).min(self.s.len());
            return Some(String::from_utf8_lossy(&self.s[start..start + len]).into_owned());
        }
        let start = self.at;
        while self.at < self.s.len() && !self.s[self.at].is_ascii_whitespace() && !b"{}\"".contains(&self.s[self.at]) {
            self.at += 1;
        }
        Some(String::from_utf8_lossy(&self.s[start..self.at]).into_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_blocks() {
        let text = b"{\n\"classname\" \"worldspawn\"\n\"message\" \"The Hall\"\n}\n{\n\"classname\" \"info_player_start\"\n\"origin\" \"1 2 3\"\n\"angle\" \"90\"\n}\n\0junk";
        let es = parse(text);
        assert_eq!(es.len(), 2);
        assert_eq!(es[0].get("message"), Some("The Hall"));
        assert_eq!(es[1].vec3("origin"), Some(Vec3::new(1.0, 2.0, 3.0)));
        assert_eq!(es[1].float("angle"), Some(90.0));
    }

    #[test]
    fn survives_garbage() {
        for text in
            [&b"{ \"a\" "[..], b"{ \"unterminated", b"}}}{", b"", b"{ \"origin\" \"1 nan\" }", &[0xff, 0xfe, b'{']]
        {
            let es = parse(text);
            for e in &es {
                assert!(e.vec3("origin").is_none());
            }
        }
    }
}
