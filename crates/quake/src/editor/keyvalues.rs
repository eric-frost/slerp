//! Valve's KeyValues text, the syntax of Hammer's `.vmf` maps and of
//! `.vmt` materials: `key value` pairs and `key { ... }` blocks, keys and
//! values quoted or bare, `//` comments, and platform conditions such as
//! `[$X360]` after an entry (which are dropped).  Keys keep their case;
//! lookups ignore it, as the engine's do.

/// The deepest nesting read.  Real files nest a handful of levels; deeper
/// blocks are skipped so a hostile file can't exhaust the stack.
const MAX_DEPTH: usize = 32;

/// Entries borrow their text from the file, so a large map parses without
/// a string allocation per key and value.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Value<'a> {
    Text(&'a str),
    Block(Vec<Kv<'a>>),
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Kv<'a> {
    pub key: &'a str,
    pub value: Value<'a>,
}

impl<'a> Kv<'a> {
    /// The entries of a block (empty for text).
    pub fn children(&self) -> &[Kv<'a>] {
        match &self.value {
            Value::Block(v) => v,
            Value::Text(_) => &[],
        }
    }

    pub fn text(&self) -> Option<&'a str> {
        match &self.value {
            Value::Text(t) => Some(t),
            Value::Block(_) => None,
        }
    }

    /// The text of the last entry called `key` (keys repeat in the wild,
    /// and the engine keeps the last).
    pub fn get(&self, key: &str) -> Option<&'a str> {
        self.children().iter().rev().find(|k| k.key.eq_ignore_ascii_case(key)).and_then(Kv::text)
    }

    /// Blocks called `key`, in order.
    pub fn blocks<'s>(&'s self, key: &'s str) -> impl Iterator<Item = &'s Kv<'a>> + 's {
        self.children().iter().filter(move |k| k.key.eq_ignore_ascii_case(key) && matches!(k.value, Value::Block(_)))
    }

    pub fn block(&self, key: &str) -> Option<&Kv<'a>> {
        self.children().iter().find(|k| k.key.eq_ignore_ascii_case(key) && matches!(k.value, Value::Block(_)))
    }
}

#[derive(Debug, PartialEq)]
enum Tok<'a> {
    Open,
    Close,
    Text(&'a str),
    /// `[$X360]` and the like.
    Condition,
}

struct Lexer<'a> {
    s: &'a [u8],
    at: usize,
}

impl<'a> Lexer<'a> {
    fn next(&mut self) -> Option<Tok<'a>> {
        let s = self.s;
        loop {
            while self.at < s.len() && s[self.at].is_ascii_whitespace() {
                self.at += 1;
            }
            if s[self.at..].starts_with(b"//") {
                while self.at < s.len() && s[self.at] != b'\n' {
                    self.at += 1;
                }
                continue;
            }
            break;
        }
        let text = |a: usize, b: usize| std::str::from_utf8(&s[a..b]).unwrap_or("");
        match *s.get(self.at)? {
            b'{' => {
                self.at += 1;
                Some(Tok::Open)
            }
            b'}' => {
                self.at += 1;
                Some(Tok::Close)
            }
            b'[' => {
                let len = s[self.at..].iter().position(|&b| b == b']' || b == b'\n').unwrap_or(s.len() - self.at);
                self.at = (self.at + len + 1).min(s.len());
                Some(Tok::Condition)
            }
            b'"' => {
                let start = self.at + 1;
                let len = s[start..].iter().position(|&b| b == b'"').unwrap_or(s.len() - start);
                self.at = (start + len + 1).min(s.len());
                Some(Tok::Text(text(start, start + len)))
            }
            _ => {
                let start = self.at;
                while self.at < s.len() && !s[self.at].is_ascii_whitespace() && !b"{}\"".contains(&s[self.at]) {
                    self.at += 1;
                }
                Some(Tok::Text(text(start, self.at)))
            }
        }
    }
}

/// Parses a whole file into its top-level entries.  Damaged text ends the
/// entry it's in; everything read so far is kept.
pub(crate) fn parse(bytes: &[u8]) -> Vec<Kv<'_>> {
    let bytes = bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(bytes);
    let mut lex = Lexer { s: bytes, at: 0 };
    let mut out = Vec::new();
    block(&mut lex, &mut out, 0, true);
    out
}

/// Reads entries into `out` until the closing brace (or the end, at the
/// top level).
fn block<'a>(lex: &mut Lexer<'a>, out: &mut Vec<Kv<'a>>, depth: usize, top: bool) {
    let mut pending: Option<&'a str> = None;
    while let Some(t) = lex.next() {
        match t {
            Tok::Close => {
                if top {
                    pending = None;
                    continue;
                }
                return;
            }
            Tok::Condition => {}
            Tok::Open => {
                let key = pending.take().unwrap_or("");
                let mut children = Vec::new();
                if depth < MAX_DEPTH {
                    block(lex, &mut children, depth + 1, false);
                } else {
                    skip(lex);
                }
                out.push(Kv { key, value: Value::Block(children) });
            }
            Tok::Text(s) => match pending.take() {
                Some(key) => out.push(Kv { key, value: Value::Text(s) }),
                None => pending = Some(s),
            },
        }
    }
}

/// Skips to just past the brace closing the current block.
fn skip(lex: &mut Lexer) {
    let mut depth = 1usize;
    while let Some(t) = lex.next() {
        match t {
            Tok::Open => depth += 1,
            Tok::Close => {
                depth -= 1;
                if depth == 0 {
                    return;
                }
            }
            _ => {}
        }
    }
}

/// Numbers in text such as `"(0 0 0) (1 0 0) (0 1 0)"`, `"[1 0 0 0] 0.25"`
/// or a displacement row: every run of characters that can make a number.
pub(crate) fn numbers(s: &str) -> impl Iterator<Item = f64> + '_ {
    s.split(|c: char| c.is_ascii_whitespace() || "()[]{},".contains(c))
        .filter(|w| !w.is_empty())
        .map(|w| w.parse::<f64>().ok().filter(|v| v.is_finite()).unwrap_or(0.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_blocks_pairs_comments_and_conditions() {
        let text = br#"
// A material.
"LightmappedGeneric"
{
	"$basetexture" "concrete/floor01"
	$surfaceprop concrete
	"$translucent" 1 [$X360]
	"Proxies"
	{
		AnimatedTexture { animatedtexturevar $basetexture }
	}
	"$BASETEXTURE" "concrete/floor02"
}
"#;
        let kv = parse(text);
        assert_eq!(kv.len(), 1);
        let m = &kv[0];
        assert_eq!(m.key, "LightmappedGeneric");
        assert_eq!(m.get("$basetexture"), Some("concrete/floor02"), "the last one wins, ignoring case");
        assert_eq!(m.get("$surfaceprop"), Some("concrete"));
        assert_eq!(m.get("$translucent"), Some("1"));
        let proxies = m.block("proxies").unwrap();
        assert_eq!(proxies.block("AnimatedTexture").unwrap().get("animatedtexturevar"), Some("$basetexture"));
    }

    #[test]
    fn damaged_text_is_kept_up_to_the_damage() {
        let kv = parse(b"world { \"a\" \"1\" solid { side { \"plane\" \"(0 0 0)");
        assert_eq!(kv[0].get("a"), Some("1"));
        assert_eq!(kv[0].block("solid").unwrap().block("side").unwrap().get("plane"), Some("(0 0 0)"));
        // Stray closing braces and deep nesting.
        assert_eq!(parse(b"}}} a b }").len(), 1);
        let deep = "a {".repeat(10_000);
        let kv = parse(deep.as_bytes());
        assert_eq!(kv.len(), 1);
        let text = b"versioninfo\n{\n\t\"editorversion\" \"400\"\n}\nworld { }";
        for cut in 0..text.len() {
            parse(&text[..cut]);
        }
    }

    #[test]
    fn numbers_in_text() {
        let v: Vec<f64> = numbers("(-512 512 64) (512 512 64) (512 -512 64)").collect();
        assert_eq!(v, [-512.0, 512.0, 64.0, 512.0, 512.0, 64.0, 512.0, -512.0, 64.0]);
        let v: Vec<f64> = numbers("[1 0 0 -16] 0.25").collect();
        assert_eq!(v, [1.0, 0.0, 0.0, -16.0, 0.25]);
        let v: Vec<f64> = numbers("1 nan x 1e999").collect();
        assert_eq!(v, [1.0, 0.0, 0.0, 0.0]);
    }
}
