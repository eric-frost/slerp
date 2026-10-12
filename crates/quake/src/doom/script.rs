//! The tokenizer of ZDoom's text lumps (TEXTURES, ANIMDEFS, MAPINFO,
//! GLDEFS), after its `sc_man`: quoted strings, bare words and numbers,
//! the symbols `{ } ( ) , = ; :`, and `//` and `/* */` comments.  Bare
//! words stop at whitespace and symbols, so `x=1;` is three tokens.

/// One token.  Words keep their case; callers compare without it.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Tok {
    Word(String),
    /// A quoted string, without its quotes.
    Quoted(String),
    Sym(u8),
}

impl Tok {
    /// The text of a word or a quoted string.
    pub fn text(&self) -> Option<&str> {
        match self {
            Tok::Word(s) | Tok::Quoted(s) => Some(s),
            Tok::Sym(_) => None,
        }
    }

    pub fn is(&self, word: &str) -> bool {
        matches!(self, Tok::Word(w) if w.eq_ignore_ascii_case(word))
    }

    pub fn is_sym(&self, c: u8) -> bool {
        *self == Tok::Sym(c)
    }

    pub fn number(&self) -> Option<f64> {
        self.text().and_then(parse_number)
    }
}

/// A number as ZDoom writes them: decimal, `0x` hexadecimal, a sign.
pub(crate) fn parse_number(s: &str) -> Option<f64> {
    let s = s.trim();
    let (neg, body) = match s.strip_prefix('-') {
        Some(b) => (true, b),
        None => (false, s.strip_prefix('+').unwrap_or(s)),
    };
    let v = if let Some(h) = body.strip_prefix("0x").or_else(|| body.strip_prefix("0X")) {
        u64::from_str_radix(h, 16).ok()? as f64
    } else {
        body.parse::<f64>().ok()?
    };
    let v = if neg { -v } else { v };
    v.is_finite().then_some(v)
}

/// The longest script read: real ones are a few hundred kilobytes.
pub const MAX_SCRIPT_BYTES: usize = 16 << 20;

pub(crate) struct Scanner<'a> {
    s: &'a [u8],
    i: usize,
    /// Where the last token was read from, for `unget`.
    prev: usize,
}

impl<'a> Scanner<'a> {
    pub fn new(text: &'a [u8]) -> Scanner<'a> {
        let s = &text[..text.len().min(MAX_SCRIPT_BYTES)];
        Scanner { s, i: 0, prev: 0 }
    }

    /// Gives the last token back, so the next `next` reads it again.
    pub fn unget(&mut self, _t: Tok) {
        self.i = self.prev;
    }

    /// The next token, or `None` at the end.
    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> Option<Tok> {
        self.prev = self.i;
        let s = self.s;
        loop {
            let c = *s.get(self.i)?;
            if c.is_ascii_whitespace() || c == 0 {
                self.i += 1;
            } else if c == b'/' && s.get(self.i + 1) == Some(&b'/') {
                while self.i < s.len() && s[self.i] != b'\n' {
                    self.i += 1;
                }
            } else if c == b'/' && s.get(self.i + 1) == Some(&b'*') {
                self.i += 2;
                while self.i + 1 < s.len() && !(s[self.i] == b'*' && s[self.i + 1] == b'/') {
                    self.i += 1;
                }
                self.i = (self.i + 2).min(s.len());
            } else {
                break;
            }
        }
        let c = s[self.i];
        if b"{}(),=;:".contains(&c) {
            self.i += 1;
            return Some(Tok::Sym(c));
        }
        if c == b'"' {
            self.i += 1;
            let mut v = Vec::new();
            while self.i < s.len() && s[self.i] != b'"' {
                if s[self.i] == b'\\' && self.i + 1 < s.len() {
                    self.i += 1;
                }
                v.push(s[self.i]);
                self.i += 1;
            }
            self.i = (self.i + 1).min(s.len());
            return Some(Tok::Quoted(String::from_utf8_lossy(&v).into_owned()));
        }
        let start = self.i;
        while self.i < s.len() {
            let c = s[self.i];
            if c.is_ascii_whitespace() || b"{}(),=;:\"".contains(&c) {
                break;
            }
            if c == b'/' && matches!(s.get(self.i + 1), Some(b'/' | b'*')) {
                break;
            }
            self.i += 1;
        }
        Some(Tok::Word(String::from_utf8_lossy(&s[start..self.i]).into_owned()))
    }

    /// The next token if it is the symbol `c`, which is then consumed.
    pub fn eat(&mut self, c: u8) -> bool {
        match self.next() {
            Some(t) if t.is_sym(c) => true,
            Some(t) => {
                self.unget(t);
                false
            }
            None => false,
        }
    }

    /// The next token's text, when it is a word or string.
    pub fn text(&mut self) -> Option<String> {
        match self.next()? {
            Tok::Word(s) | Tok::Quoted(s) => Some(s),
            t => {
                self.unget(t);
                None
            }
        }
    }

    /// The next token as a number, consumed only if it is one.
    pub fn number(&mut self) -> Option<f64> {
        let t = self.next()?;
        match t.number() {
            Some(v) => Some(v),
            None => {
                self.unget(t);
                None
            }
        }
    }

    /// Skips to the end of the block whose `{` was just read.
    pub fn skip_block(&mut self) {
        let mut depth = 1usize;
        while let Some(t) = self.next() {
            if t.is_sym(b'{') {
                depth += 1;
            } else if t.is_sym(b'}') {
                depth -= 1;
                if depth == 0 {
                    return;
                }
            }
        }
    }

    /// Whether a newline separates position `at` from the next token.
    fn newline_before(&self, at: usize) -> bool {
        let mut i = at;
        while i < self.s.len() && self.s[i].is_ascii_whitespace() {
            if self.s[i] == b'\n' {
                return true;
            }
            i += 1;
        }
        false
    }

    /// Whether the next token starts on a new line.
    pub fn at_line_start(&self) -> bool {
        self.newline_before(self.i)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens() {
        let mut s = Scanner::new(b"Texture \"A/B\", 64,-32 // c\n{ XScale 2.0 /* x */ }x=0x10;");
        let mut all = Vec::new();
        while let Some(t) = s.next() {
            all.push(t);
        }
        assert_eq!(
            all,
            [
                Tok::Word("Texture".into()),
                Tok::Quoted("A/B".into()),
                Tok::Sym(b','),
                Tok::Word("64".into()),
                Tok::Sym(b','),
                Tok::Word("-32".into()),
                Tok::Sym(b'{'),
                Tok::Word("XScale".into()),
                Tok::Word("2.0".into()),
                Tok::Sym(b'}'),
                Tok::Word("x".into()),
                Tok::Sym(b'='),
                Tok::Word("0x10".into()),
                Tok::Sym(b';'),
            ]
        );
        assert_eq!(all[12].number(), Some(16.0));
        assert_eq!(parse_number("-1.5"), Some(-1.5));
        assert_eq!(parse_number("nan"), None);
        // Unterminated strings and comments end quietly.
        let mut s = Scanner::new(b"\"abc");
        assert_eq!(s.next(), Some(Tok::Quoted("abc".into())));
        assert_eq!(s.next(), None);
        let mut s = Scanner::new(b"a /* b");
        assert_eq!(s.next(), Some(Tok::Word("a".into())));
        assert_eq!(s.next(), None);
    }
}
