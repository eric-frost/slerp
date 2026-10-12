//! The text on a page, and what selecting it means: each glyph's box and
//! the characters it stands for, grouped into lines, with the rules a PDF
//! reader follows for the glyph under the pointer, a selection's ends, a
//! word, and the plain text a selection copies as.
//!
//! Positions are in the page's units (PDF points) from its top left with y
//! going down, rotation applied, as the viewer measures pages.  The glyphs
//! are read in the renderer's child process (`pdf.rs`), in the order the
//! page draws them, which for nearly every document is reading order; what
//! is here is arithmetic on what comes back.
//!
//! A selection runs between two carets, a caret being a place between
//! glyphs counted in that order: caret 0 is before the first glyph, caret
//! `len()` after the last.  The glyphs between the carets are selected.
//!
//! Text may run in any of four directions on the page as shown: across
//! from the left, or turned a quarter, a half or three quarters (a turned
//! page, a label up the side of a chart).  Each glyph carries its
//! direction, and lines, words and carets are worked out in a frame turned
//! with it, where the text runs along `u` and lines follow one another
//! along `v`, so turned text groups as level text does.

use std::ops::Range;

/// The most glyphs read from one page.  A page of small print has a few
/// thousand; the cap only bounds hostile files, whose further glyphs are
/// left out.
pub const MAX_GLYPHS: usize = 200_000;
/// The most bytes of text one glyph stands for: a ligature's letters, or
/// the phrase a font maps a symbol to.  Longer mappings are cut.
const MAX_GLYPH_TEXT: usize = 32;
/// A gap between neighbouring glyphs wider than this many font sizes
/// separates words.  Kerning and the tracking of headings stay well under
/// it; PDFs often leave out the space characters and only move the next
/// word along, so this is the only sign of a word break.
const SPACE_GAP: f32 = 0.12;
/// A gap wider than this many font sizes splits a line into separate
/// stretches (table cells, columns side by side), which are hit and
/// highlighted on their own, so the empty paper between them pans.
const SPAN_GAP: f32 = 1.5;

/// Which way a glyph's text runs on the page as shown, a quarter turn at a
/// time clockwise from left to right.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Direction {
    #[default]
    Right,
    Down,
    Left,
    Up,
}

impl Direction {
    /// The direction nearest a baseline vector on the page (y going down).
    pub fn of(dx: f64, dy: f64) -> Direction {
        match (dx.abs() >= dy.abs(), dx >= 0.0, dy >= 0.0) {
            (true, true, _) => Direction::Right,
            (true, false, _) => Direction::Left,
            (false, _, true) => Direction::Down,
            (false, _, false) => Direction::Up,
        }
    }

    /// A page point in this direction's frame: `u` along the text, `v`
    /// across it, towards the following lines.
    fn point(self, p: [f32; 2]) -> [f32; 2] {
        match self {
            Direction::Right => [p[0], p[1]],
            Direction::Down => [p[1], -p[0]],
            Direction::Left => [-p[0], -p[1]],
            Direction::Up => [-p[1], p[0]],
        }
    }

    /// A page rectangle in this direction's frame, as `[u0, v0, u1, v1]`.
    fn rect(self, r: [f32; 4]) -> [f32; 4] {
        let (a, b) = (self.point([r[0], r[1]]), self.point([r[2], r[3]]));
        [a[0].min(b[0]), a[1].min(b[1]), a[0].max(b[0]), a[1].max(b[1])]
    }

    fn code(self) -> u8 {
        self as u8
    }

    fn from_code(c: u8) -> Option<Direction> {
        [Direction::Right, Direction::Down, Direction::Left, Direction::Up].get(c as usize).copied()
    }
}

/// One glyph: its box on the page, which way it runs, and where its text
/// is.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Glyph {
    /// Left, top, right and bottom on the page.
    rect: [f32; 4],
    dir: Direction,
    /// Its text, a byte range of [`PageText::text`].
    start: u32,
    end: u32,
}

impl Glyph {
    /// The box in the glyph's own frame (see [`Direction::rect`]).
    fn frame(&self) -> [f32; 4] {
        self.dir.rect(self.rect)
    }
}

/// A stretch of glyphs on one line with no wide gap inside: the unit the
/// pointer hits and a highlight covers.
#[derive(Clone, Debug, PartialEq)]
struct Span {
    glyphs: Range<usize>,
    rect: [f32; 4],
    dir: Direction,
}

/// The text on one page.  Empty for pages without text: scans, drawings,
/// lettering turned into outlines.
#[derive(Clone, Debug, Default)]
pub struct PageText {
    text: String,
    glyphs: Vec<Glyph>,
    /// The first glyph of each line, in order.
    line_starts: Vec<usize>,
    spans: Vec<Span>,
    truncated: bool,
}

impl PageText {
    /// A page's text from its glyphs' boxes (left, top, right, bottom),
    /// directions and text, in the order the page draws them.
    pub fn new<'s>(glyphs: impl IntoIterator<Item = ([f32; 4], Direction, &'s str)>) -> PageText {
        let mut b = Builder::default();
        for (rect, dir, text) in glyphs {
            if !b.push(rect, dir, text) {
                break;
            }
        }
        b.build()
    }

    pub fn len(&self) -> usize {
        self.glyphs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.glyphs.is_empty()
    }

    /// Whether the page had more than [`MAX_GLYPHS`] glyphs, and the rest
    /// were left out.
    pub fn truncated(&self) -> bool {
        self.truncated
    }

    fn text_of(&self, i: usize) -> &str {
        let g = &self.glyphs[i];
        &self.text[g.start as usize..g.end as usize]
    }

    /// Whether `p` is over text, where the pointer shows the text cursor
    /// and a drag selects.  Between the words of a line counts; the paper
    /// between columns doesn't.
    pub fn on_text(&self, p: [f32; 2]) -> bool {
        self.spans.iter().any(|s| contains(s.rect, p))
    }

    /// The glyph at `p`: the one under it, or, between two glyphs of a
    /// line, the nearer one.  `None` off text.
    pub fn glyph_at(&self, p: [f32; 2]) -> Option<usize> {
        let span = self.spans.iter().find(|s| contains(s.rect, p))?;
        let u = span.dir.point(p)[0];
        let off = |i: &usize| {
            let f = self.glyphs[*i].frame();
            (f[0] - u).max(u - f[2]).max(0.0)
        };
        span.glyphs.clone().min_by(|a, b| off(a).total_cmp(&off(b)))
    }

    /// The caret nearest `p`, where a selection dragged to `p` ends.  The
    /// stretch is the nearest across the lines first (the line at the
    /// pointer's height, for level text), then along them, so a pointer
    /// past the end of a line ends the selection there rather than on a
    /// longer line nearby.  On it, the caret goes before the first glyph
    /// whose middle is past `p`.  `None` when the page has no text.
    pub fn caret_at(&self, p: [f32; 2]) -> Option<usize> {
        let distance = |s: &Span| {
            let (f, q) = (s.dir.rect(s.rect), s.dir.point(p));
            let across = (f[1] - q[1]).max(q[1] - f[3]).max(0.0);
            let along = (f[0] - q[0]).max(q[0] - f[2]).max(0.0);
            (across, along)
        };
        let nearest = |a: &&Span, b: &&Span| {
            let (a, b) = (distance(a), distance(b));
            a.0.total_cmp(&b.0).then(a.1.total_cmp(&b.1))
        };
        let span = self.spans.iter().min_by(nearest)?;
        let u = span.dir.point(p)[0];
        let before = span.glyphs.clone().find(|&i| {
            let f = self.glyphs[i].frame();
            u < (f[0] + f[2]) / 2.0
        });
        Some(before.unwrap_or(span.glyphs.end))
    }

    /// The word at `p`, as a range of glyphs: letters and digits that touch
    /// on one line.  A punctuation mark or space is a word of its own.
    pub fn word_at(&self, p: [f32; 2]) -> Option<Range<usize>> {
        let i = self.glyph_at(p)?;
        if !self.is_word(i) {
            return Some(i..i + 1);
        }
        let span = &self.spans[self.span_of(i)];
        let mut start = i;
        while start > span.glyphs.start && self.is_word(start - 1) && !self.spaced(start - 1, start) {
            start -= 1;
        }
        let mut end = i + 1;
        while end < span.glyphs.end && self.is_word(end) && !self.spaced(end - 1, end) {
            end += 1;
        }
        Some(start..end)
    }

    /// The boxes to highlight for the glyphs in `range`: one per stretch of
    /// a line, as tall as the line (as wide, for turned text), so a
    /// selection reads as bars rather than glyph by glyph.
    pub fn selection_rects(&self, range: Range<usize>) -> Vec<[f32; 4]> {
        let range = range.start.min(self.len())..range.end.min(self.len());
        if range.is_empty() {
            return Vec::new();
        }
        let first = self.span_of(range.start);
        let mut rects = Vec::new();
        for span in self.spans[first..].iter().take_while(|s| s.glyphs.start < range.end) {
            let (s, e) = (span.glyphs.start.max(range.start), span.glyphs.end.min(range.end));
            if s >= e {
                continue;
            }
            // Spaces at the ends of the selected part don't widen the bar,
            // as the copied text leaves them out.
            let (s, e) = match (s..e).position(|i| !self.is_space(i)) {
                Some(first) => (s + first, e - (s..e).rev().position(|i| !self.is_space(i)).unwrap_or(0)),
                None => (s, e),
            };
            let r = self.glyphs[s..e].iter().fold(self.glyphs[s].rect, |r, g| union(r, g.rect));
            rects.push(match span.dir {
                Direction::Right | Direction::Left => [r[0], span.rect[1], r[2], span.rect[3]],
                Direction::Down | Direction::Up => [span.rect[0], r[1], span.rect[2], r[3]],
            });
        }
        rects
    }

    /// The glyphs in `range` as plain text: a line break between lines, a
    /// space between glyphs far enough apart to be separate words, and the
    /// spaces at either end of each line left out.  Indentation set by
    /// position can't be told from text set apart by position, so neither
    /// is kept; spaces a page draws at a line's ends (Illustrator centres
    /// lines that way) are dropped to match.
    pub fn copy_text(&self, range: Range<usize>) -> String {
        let range = range.start.min(self.len())..range.end.min(self.len());
        let mut lines = vec![String::new()];
        for i in range.clone() {
            let line = lines.last_mut().expect("never empty");
            if i > range.start {
                if self.starts_line(i) {
                    lines.push(String::new());
                } else if self.spaced(i - 1, i)
                    && !line.ends_with(char::is_whitespace)
                    && !self.text_of(i).starts_with(char::is_whitespace)
                {
                    line.push(' ');
                }
            }
            lines.last_mut().expect("never empty").push_str(self.text_of(i));
        }
        lines.iter().map(|l| l.trim()).collect::<Vec<_>>().join("\n")
    }

    /// The whole page's text, as [`PageText::copy_text`] gives it.
    pub fn all_text(&self) -> String {
        self.copy_text(0..self.len())
    }

    /// Whether glyphs `a` and `b`, neighbours on a line, are far enough
    /// apart to be separate words.
    fn spaced(&self, a: usize, b: usize) -> bool {
        let (fa, fb) = (self.glyphs[a].frame(), self.glyphs[b].frame());
        fb[0] - fa[2] > SPACE_GAP * (fa[3] - fa[1]).max(fb[3] - fb[1])
    }

    fn is_space(&self, i: usize) -> bool {
        self.text_of(i).chars().all(char::is_whitespace)
    }

    /// Letters, digits and the marks inside words (an apostrophe, an
    /// underscore).
    fn is_word(&self, i: usize) -> bool {
        let t = self.text_of(i);
        !t.is_empty() && t.chars().all(|c| c.is_alphanumeric() || matches!(c, '_' | '\'' | '\u{2019}'))
    }

    fn starts_line(&self, i: usize) -> bool {
        self.line_starts.binary_search(&i).is_ok()
    }

    /// The stretch holding glyph `i`.
    fn span_of(&self, i: usize) -> usize {
        self.spans.partition_point(|s| s.glyphs.end <= i).min(self.spans.len().saturating_sub(1))
    }

    /// Groups the glyphs into lines, and lines into stretches, in each
    /// glyph's frame.  A glyph stays on the line when it runs the same way
    /// and shares most of its height with the glyph before or with the
    /// line's first glyph (so the numerator and denominator of a fraction,
    /// and sub- and superscripts, stay on their line), and doesn't step
    /// back by more than its size; it starts a new stretch after a wide gap.
    fn group(&mut self) {
        self.line_starts.clear();
        self.spans.clear();
        let shares = |a: [f32; 4], b: [f32; 4]| {
            let overlap = a[3].min(b[3]) - a[1].max(b[1]);
            overlap >= 0.5 * (a[3] - a[1]).min(b[3] - b[1])
        };
        let mut first = 0;
        for i in 0..self.glyphs.len() {
            let g = self.glyphs[i];
            let joins = i > 0 && {
                let before = self.glyphs[i - 1];
                let (p, r) = (before.frame(), g.frame());
                let size = (p[3] - p[1]).max(r[3] - r[1]);
                let level = shares(p, r) || shares(self.glyphs[first].frame(), r);
                let same_line = before.dir == g.dir && level && r[0] >= p[0] - size;
                if !same_line {
                    self.line_starts.push(i);
                    first = i;
                }
                same_line && r[0] - p[2] <= SPAN_GAP * size
            };
            if i == 0 {
                self.line_starts.push(0);
            }
            match self.spans.last_mut() {
                Some(s) if joins => {
                    s.glyphs.end = i + 1;
                    s.rect = union(s.rect, g.rect);
                }
                _ => self.spans.push(Span { glyphs: i..i + 1, rect: g.rect, dir: g.dir }),
            }
        }
        // A stretch's box is its visible glyphs', without spaces at its
        // ends, so the text cursor shows over text rather than blank paper.
        for span in &mut self.spans {
            let glyphs = &self.glyphs[span.glyphs.clone()];
            let ink =
                glyphs.iter().filter(|g| !self.text[g.start as usize..g.end as usize].chars().all(char::is_whitespace));
            if let Some(r) = ink.map(|g| g.rect).reduce(union) {
                span.rect = r;
            }
        }
    }

    /// Reads an answer from [`Builder::encode`]; `None` when it is damaged.
    pub(crate) fn decode(b: &[u8]) -> Option<PageText> {
        let count = u32::from_le_bytes(b.get(0..4)?.try_into().ok()?) as usize;
        if count > MAX_GLYPHS {
            return None;
        }
        let truncated = *b.get(4)? != 0;
        let records = b.get(5..5 + count * RECORD)?;
        let text = std::str::from_utf8(b.get(5 + count * RECORD..)?).ok()?;
        let mut glyphs = Vec::with_capacity(count);
        let mut at = 0usize;
        for r in records.chunks_exact(RECORD) {
            let f = |o: usize| f32::from_le_bytes(r[o..o + 4].try_into().expect("four bytes"));
            let rect = [f(0), f(4), f(8), f(12)];
            let dir = Direction::from_code(r[16])?;
            let end = at + r[17] as usize;
            if !sane(rect) || end == at || end > text.len() || !text.is_char_boundary(end) {
                return None;
            }
            glyphs.push(Glyph { rect, dir, start: at as u32, end: end as u32 });
            at = end;
        }
        if at != text.len() {
            return None;
        }
        let mut page = PageText { text: text.to_string(), glyphs, truncated, ..PageText::default() };
        page.group();
        Some(page)
    }
}

/// The bytes of one glyph in an answer: its box, direction and text length.
const RECORD: usize = 18;

/// Gathers a page's glyphs in the order the page draws them.
#[derive(Default)]
pub(crate) struct Builder {
    text: String,
    glyphs: Vec<Glyph>,
    truncated: bool,
}

impl Builder {
    /// Adds a glyph.  Its text is cleaned up for copying (see [`clean`]);
    /// a glyph with no text left, or with a box that isn't one, is left
    /// out.  Returns false once the page is full, when the caller stops.
    pub(crate) fn push(&mut self, rect: [f32; 4], dir: Direction, text: &str) -> bool {
        if self.glyphs.len() >= MAX_GLYPHS {
            self.truncated = true;
            return false;
        }
        let start = self.text.len();
        if sane(rect) {
            clean(text, &mut self.text);
        }
        if self.text.len() > start {
            self.glyphs.push(Glyph { rect, dir, start: start as u32, end: self.text.len() as u32 });
        }
        true
    }

    pub(crate) fn build(self) -> PageText {
        let mut page =
            PageText { text: self.text, glyphs: self.glyphs, truncated: self.truncated, ..PageText::default() };
        page.group();
        page
    }

    /// The glyphs as bytes, for the answer from the renderer's child: the
    /// count, whether the page was cut short, each glyph's box, direction
    /// and text length, then all the text.
    pub(crate) fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(5 + self.glyphs.len() * RECORD + self.text.len());
        out.extend((self.glyphs.len() as u32).to_le_bytes());
        out.push(self.truncated as u8);
        for g in &self.glyphs {
            for v in g.rect {
                out.extend(v.to_le_bytes());
            }
            out.push(g.dir.code());
            out.push((g.end - g.start) as u8);
        }
        out.extend(self.text.as_bytes());
        out
    }
}

/// Appends a glyph's text as people expect to paste it: the Latin
/// ligatures as their letters, a no-break space as a space, a soft hyphen
/// (which a PDF only contains where it is drawn) as a hyphen, and control
/// characters left out.  At most [`MAX_GLYPH_TEXT`] bytes.
fn clean(text: &str, out: &mut String) {
    let start = out.len();
    for c in text.chars() {
        let s: &str = match c {
            '\u{FB00}' => "ff",
            '\u{FB01}' => "fi",
            '\u{FB02}' => "fl",
            '\u{FB03}' => "ffi",
            '\u{FB04}' => "ffl",
            '\u{FB05}' | '\u{FB06}' => "st",
            '\u{00A0}' => " ",
            '\u{00AD}' => "-",
            c if c.is_control() => "",
            c => {
                if out.len() + c.len_utf8() - start > MAX_GLYPH_TEXT {
                    break;
                }
                out.push(c);
                continue;
            }
        };
        if out.len() + s.len() - start > MAX_GLYPH_TEXT {
            break;
        }
        out.push_str(s);
    }
}

/// Whether a rectangle is one: finite, with some height (a width of zero is
/// a combining accent's).
fn sane(r: [f32; 4]) -> bool {
    r.iter().all(|v| v.is_finite()) && r[2] >= r[0] && r[3] > r[1]
}

fn contains(r: [f32; 4], p: [f32; 2]) -> bool {
    p[0] >= r[0] && p[0] <= r[2] && p[1] >= r[1] && p[1] <= r[3]
}

fn union(a: [f32; 4], b: [f32; 4]) -> [f32; 4] {
    [a[0].min(b[0]), a[1].min(b[1]), a[2].max(b[2]), a[3].max(b[3])]
}
