//! BBS and demoscene "ANSI art" (`.ans`, and any file whose end carries a
//! SAUCE record): text in code page 437 with ANSI escape sequences for
//! colours and cursor movement, drawn on a character grid and rendered
//! with an 8x16 VGA-style font.
//!
//! The font in `cp437.bin` comes from Unscii 1.1 (<http://viznut.fi/unscii/>),
//! whose own glyphs are in the Public Domain; see `font/make-font.ts` for
//! how it was cut down to the 256 characters of code page 437.
//!
//! Handled: SGR colours (the 16 VGA colours, bright via bold, 256-colour and
//! 24-bit forms, reverse), cursor movement and positioning, save and
//! restore, clearing the screen and lines, tabs, and the bright-background
//! "iCE colours" mode that SAUCE's flags select (without it the blink
//! attribute is drawn as a plain background).  Width comes from SAUCE,
//! else 80 columns.

use crate::pic::{Pic, Rgb, check_size};
use slerp_scene::LoadError;

static FONT: &[u8; 4096] = include_bytes!("cp437.bin");

/// The 16 colours of a VGA text screen.
pub const VGA: [Rgb; 16] = [
    [0x00, 0x00, 0x00],
    [0x00, 0x00, 0xaa],
    [0x00, 0xaa, 0x00],
    [0x00, 0xaa, 0xaa],
    [0xaa, 0x00, 0x00],
    [0xaa, 0x00, 0xaa],
    [0xaa, 0x55, 0x00],
    [0xaa, 0xaa, 0xaa],
    [0x55, 0x55, 0x55],
    [0x55, 0x55, 0xff],
    [0x55, 0xff, 0x55],
    [0x55, 0xff, 0xff],
    [0xff, 0x55, 0x55],
    [0xff, 0x55, 0xff],
    [0xff, 0xff, 0x55],
    [0xff, 0xff, 0xff],
];

/// SGR colour numbers (0 black, 1 red, 2 green, 3 yellow, 4 blue, 5 magenta,
/// 6 cyan, 7 white) to VGA palette entries.
const ANSI_TO_VGA: [u8; 8] = [0, 4, 2, 6, 1, 5, 3, 7];

/// The most rows of cells kept, so a stray "move to row 30000" cannot ask
/// for gigabytes.
const MAX_ROWS: usize = 8192;

/// The most rows drawn for the viewer: GPU textures are at most 8192 pixels
/// on a side.
pub const MAX_PICTURE_ROWS: usize = 512;

/// Files larger than this are not read; real ANSI art is a few hundred
/// kilobytes at most.
pub const MAX_BYTES: u64 = 32 << 20;

//region SAUCE

/// The 128-byte Standard Architecture for Universal Comment Extensions
/// record that ANSI artists append to their files.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Sauce {
    pub title: String,
    pub author: String,
    pub group: String,
    /// `CCYYMMDD`, as stored.
    pub date: String,
    pub data_type: u8,
    pub file_type: u8,
    pub info: [u16; 4],
    pub flags: u8,
    pub font: String,
    pub comments: Vec<String>,
    /// Bytes of the file before the SAUCE block (and its comments and the
    /// EOF marker).
    pub data_len: usize,
}

impl Sauce {
    /// Non-blink mode: the blink attribute selects a bright background.
    pub fn ice_colours(&self) -> bool {
        self.flags & 1 != 0
    }

    /// The date as `CCYY-MM-DD` when it looks like one.
    pub fn date_text(&self) -> String {
        let d = &self.date;
        if d.len() == 8 && d.bytes().all(|b| b.is_ascii_digit()) && d != "00000000" {
            format!("{}-{}-{}", &d[..4], &d[4..6], &d[6..])
        } else {
            String::new()
        }
    }
}

fn text(b: &[u8]) -> String {
    let end = b.iter().rposition(|&c| c != b' ' && c != 0).map_or(0, |i| i + 1);
    cp437_to_string(&b[..end])
}

/// Code page 437 text as Unicode, for the inspector.
pub fn cp437_to_string(b: &[u8]) -> String {
    const HIGH: &str = "ÇüéâäàåçêëèïîìÄÅÉæÆôöòûùÿÖÜ¢£¥₧ƒáíóúñÑªº¿⌐¬½¼¡«»░▒▓│┤╡╢╖╕╣║╗╝╜╛┐└┴┬├─┼╞╟╚╔╩╦╠═╬╧╨╤╥╙╘╒╓╫╪┘┌█▄▌▐▀αßΓπΣσµτΦΘΩδ∞φε∩≡±≥≤⌠⌡÷≈°∙·√ⁿ²■\u{a0}";
    let high: Vec<char> = HIGH.chars().collect();
    b.iter()
        .map(|&c| match c {
            0x20..=0x7e => c as char,
            0x80..=0xff => high[(c - 0x80) as usize],
            _ => ' ',
        })
        .collect()
}

/// Reads the SAUCE record at the end of `bytes`, if there is one.
pub fn sauce(bytes: &[u8]) -> Option<Sauce> {
    let n = bytes.len();
    let r = bytes.get(n.checked_sub(128)?..)?;
    if &r[..7] != b"SAUCE00" {
        return None;
    }
    let comment_lines = r[104] as usize;
    let mut data_len = n - 128;
    let mut comments = Vec::new();
    let block = 5 + comment_lines * 64;
    if comment_lines > 0 && data_len >= block && &bytes[data_len - block..data_len - block + 5] == b"COMNT" {
        let start = data_len - block + 5;
        comments = bytes[start..data_len].chunks(64).map(text).collect();
        data_len -= block;
    }
    if data_len > 0 && bytes[data_len - 1] == 0x1a {
        data_len -= 1;
    }
    Some(Sauce {
        title: text(&r[7..42]),
        author: text(&r[42..62]),
        group: text(&r[62..82]),
        date: text(&r[82..90]),
        data_type: r[94],
        file_type: r[95],
        info: [
            u16::from_le_bytes([r[96], r[97]]),
            u16::from_le_bytes([r[98], r[99]]),
            u16::from_le_bytes([r[100], r[101]]),
            u16::from_le_bytes([r[102], r[103]]),
        ],
        flags: r[105],
        font: text(&r[106..128]),
        comments,
        data_len,
    })
}

/// Whether a SAUCE record describes character data this module draws
/// (ASCII, ANSI and ANSiMation text).
pub fn is_text_sauce(s: &Sauce) -> bool {
    s.data_type == 1 && s.file_type <= 2
}

//endregion

//region The grid

#[derive(Clone, Copy)]
struct Cell {
    ch: u8,
    fg: Rgb,
    bg: Rgb,
}

/// The screen as characters: what the terminal in the artist's BBS would
/// have shown.
pub struct Grid {
    pub cols: usize,
    pub rows: usize,
    cells: Vec<Cell>,
}

#[derive(Clone, Copy)]
struct Pen {
    /// Foreground and background as chosen by SGR, before bold and blink.
    fg: Colour,
    bg: Colour,
    bold: bool,
    blink: bool,
    reverse: bool,
}

#[derive(Clone, Copy)]
enum Colour {
    /// A VGA palette entry from 0 to 7, which bold and blink may brighten.
    Basic(u8),
    /// Any other colour, taken as it is.
    Fixed(Rgb),
}

impl Pen {
    const DEFAULT: Pen = Pen { fg: Colour::Basic(7), bg: Colour::Basic(0), bold: false, blink: false, reverse: false };

    fn resolve(&self, ice: bool) -> (Rgb, Rgb) {
        let fg = match self.fg {
            Colour::Basic(i) => VGA[(i | if self.bold { 8 } else { 0 }) as usize],
            Colour::Fixed(c) => c,
        };
        let bg = match self.bg {
            Colour::Basic(i) => VGA[(i | if self.blink && ice { 8 } else { 0 }) as usize],
            Colour::Fixed(c) => c,
        };
        if self.reverse { (bg, fg) } else { (fg, bg) }
    }
}

/// The xterm 256-colour palette entry `n`.
fn xterm(n: u32) -> Rgb {
    match n {
        0..=7 => VGA[ANSI_TO_VGA[n as usize] as usize],
        8..=15 => VGA[ANSI_TO_VGA[n as usize - 8] as usize + 8],
        16..=231 => {
            let n = n - 16;
            let level = |v: u32| if v == 0 { 0 } else { (55 + 40 * v) as u8 };
            [level(n / 36), level(n / 6 % 6), level(n % 6)]
        }
        _ => {
            let g = (8 + 10 * (n.min(255) - 232)) as u8;
            [g, g, g]
        }
    }
}

/// Interprets `data` (the file without its SAUCE record) on a grid `cols`
/// wide.
pub fn interpret(data: &[u8], cols: usize, ice: bool) -> Grid {
    let cols = cols.clamp(1, 256);
    let blank = Cell { ch: b' ', fg: VGA[7], bg: VGA[0] };
    let mut cells: Vec<Cell> = Vec::new();
    let mut used_rows = 0usize;
    let (mut x, mut y) = (0usize, 0usize);
    let mut saved = (0usize, 0usize);
    let mut pen = Pen::DEFAULT;
    // Files written on Unix end lines with a bare line feed, which a DOS
    // terminal would show as a staircase; treat it as a new line then.
    let lf_returns = !data.contains(&b'\r');
    let mut i = 0;

    let ensure = |cells: &mut Vec<Cell>, y: usize| {
        let need = (y + 1) * cols;
        if cells.len() < need {
            cells.resize(need, blank);
        }
    };

    while i < data.len() {
        let c = data[i];
        i += 1;
        match c {
            0x1a => break,
            0x00 | 0x07 => {}
            0x08 => x = x.saturating_sub(1),
            0x09 => x = ((x / 8 + 1) * 8).min(cols - 1),
            0x0a => {
                y = (y + 1).min(MAX_ROWS - 1);
                if lf_returns {
                    x = 0;
                }
            }
            0x0c => {}
            0x0d => x = 0,
            0x1b if data.get(i) == Some(&b'[') => {
                i += 1;
                // Parameters: digits, separated by ';' or ':', after an
                // optional private-use marker.
                let mut params: Vec<u32> = Vec::new();
                let mut cur: Option<u32> = None;
                let mut private = false;
                while let Some(&b) = data.get(i) {
                    i += 1;
                    match b {
                        b'0'..=b'9' => {
                            cur =
                                Some(cur.unwrap_or(0).saturating_mul(10).saturating_add((b - b'0') as u32).min(100_000))
                        }
                        b';' | b':' => {
                            if params.len() < 32 {
                                params.push(cur.take().unwrap_or(0));
                            }
                            cur = None;
                        }
                        b'?' | b'=' | b'<' | b'>' | b' ' => private = true,
                        0x40..=0x7e => {
                            if cur.is_some() || !params.is_empty() {
                                params.push(cur.take().unwrap_or(0));
                            }
                            if !private {
                                csi(
                                    b,
                                    &params,
                                    &mut Csi {
                                        cols,
                                        x: &mut x,
                                        y: &mut y,
                                        saved: &mut saved,
                                        pen: &mut pen,
                                        cells: &mut cells,
                                        used_rows: &mut used_rows,
                                        blank,
                                    },
                                );
                            } else if b == b'm' {
                                // Not an SGR after all.
                            }
                            break;
                        }
                        _ => break,
                    }
                }
            }
            0x1b => {}
            ch => {
                ensure(&mut cells, y);
                let (fg, bg) = pen.resolve(ice);
                cells[y * cols + x] = Cell { ch, fg, bg };
                used_rows = used_rows.max(y + 1);
                x += 1;
                if x >= cols {
                    x = 0;
                    y = (y + 1).min(MAX_ROWS - 1);
                }
            }
        }
    }
    let rows = used_rows.max(1);
    cells.resize(rows * cols, blank);
    cells.truncate(rows * cols);
    Grid { cols, rows, cells }
}

struct Csi<'a> {
    cols: usize,
    x: &'a mut usize,
    y: &'a mut usize,
    saved: &'a mut (usize, usize),
    pen: &'a mut Pen,
    cells: &'a mut Vec<Cell>,
    used_rows: &'a mut usize,
    blank: Cell,
}

/// Runs one control sequence ending in `fin`.
fn csi(fin: u8, p: &[u32], s: &mut Csi) {
    let n = |i: usize, default: u32| p.get(i).copied().filter(|&v| v > 0).unwrap_or(default) as usize;
    let cols = s.cols;
    match fin {
        b'A' => *s.y = s.y.saturating_sub(n(0, 1)),
        b'B' => *s.y = (*s.y + n(0, 1)).min(MAX_ROWS - 1),
        b'C' => *s.x = (*s.x + n(0, 1)).min(cols - 1),
        b'D' => *s.x = s.x.saturating_sub(n(0, 1)),
        b'E' => {
            *s.y = (*s.y + n(0, 1)).min(MAX_ROWS - 1);
            *s.x = 0;
        }
        b'F' => {
            *s.y = s.y.saturating_sub(n(0, 1));
            *s.x = 0;
        }
        b'G' => *s.x = (n(0, 1) - 1).min(cols - 1),
        b'H' | b'f' => {
            *s.y = (n(0, 1) - 1).min(MAX_ROWS - 1);
            *s.x = (n(1, 1) - 1).min(cols - 1);
        }
        b's' => *s.saved = (*s.x, *s.y),
        b'u' => (*s.x, *s.y) = *s.saved,
        b'J' => {
            // Only "erase everything" matters for finished art; partial
            // erases clear from or to the cursor.
            let mode = p.first().copied().unwrap_or(0);
            let here = *s.y * cols + *s.x;
            match mode {
                0 => {
                    let end = s.cells.len();
                    s.cells[here.min(end)..].fill(s.blank);
                }
                1 => {
                    let end = (here + 1).min(s.cells.len());
                    s.cells[..end].fill(s.blank);
                }
                _ => {
                    s.cells.clear();
                    *s.used_rows = 0;
                    *s.x = 0;
                    *s.y = 0;
                }
            }
        }
        b'K' => {
            let row = *s.y * cols;
            if row < s.cells.len() {
                let (a, b) = match p.first().copied().unwrap_or(0) {
                    0 => (*s.x, cols),
                    1 => (0, *s.x + 1),
                    _ => (0, cols),
                };
                s.cells[row + a.min(cols)..row + b.min(cols)].fill(s.blank);
            }
        }
        b'm' => sgr(p, s.pen),
        b't' if p.len() == 4 && p[0] <= 1 => {
            // PabloDraw's 24-bit colour: ESC [ 0|1 ; r ; g ; b t.
            let c = Colour::Fixed([p[1].min(255) as u8, p[2].min(255) as u8, p[3].min(255) as u8]);
            if p[0] == 0 { s.pen.bg = c } else { s.pen.fg = c }
        }
        _ => {}
    }
}

fn sgr(p: &[u32], pen: &mut Pen) {
    if p.is_empty() {
        *pen = Pen::DEFAULT;
        return;
    }
    let mut i = 0;
    while i < p.len() {
        let v = p[i];
        match v {
            0 => *pen = Pen::DEFAULT,
            1 => pen.bold = true,
            5 | 6 => pen.blink = true,
            7 => pen.reverse = true,
            22 => pen.bold = false,
            25 => pen.blink = false,
            27 => pen.reverse = false,
            30..=37 => pen.fg = Colour::Basic(ANSI_TO_VGA[(v - 30) as usize]),
            39 => pen.fg = Colour::Basic(7),
            40..=47 => pen.bg = Colour::Basic(ANSI_TO_VGA[(v - 40) as usize]),
            49 => pen.bg = Colour::Basic(0),
            90..=97 => pen.fg = Colour::Fixed(VGA[ANSI_TO_VGA[(v - 90) as usize] as usize + 8]),
            100..=107 => pen.bg = Colour::Fixed(VGA[ANSI_TO_VGA[(v - 100) as usize] as usize + 8]),
            38 | 48 => {
                let colour = match p.get(i + 1) {
                    Some(5) => {
                        i += 2;
                        p.get(i).map(|&n| Colour::Fixed(xterm(n)))
                    }
                    Some(2) => {
                        let c = p
                            .get(i + 2..i + 5)
                            .map(|c| Colour::Fixed([c[0].min(255) as u8, c[1].min(255) as u8, c[2].min(255) as u8]));
                        i += 4;
                        c
                    }
                    _ => None,
                };
                if let Some(c) = colour {
                    if v == 38 { pen.fg = c } else { pen.bg = c }
                }
            }
            _ => {}
        }
        i += 1;
    }
}

//endregion

//region Drawing

impl Grid {
    /// Draws the first `max_rows` rows with the 8x16 font.
    pub fn render(&self, max_rows: usize) -> Pic {
        let rows = self.rows.min(max_rows).max(1);
        let (w, h) = (self.cols as u32 * 8, rows as u32 * 16);
        let mut pic = Pic::new(w, h);
        for row in 0..rows {
            for col in 0..self.cols {
                let cell = self.cells[row * self.cols + col];
                let glyph = &FONT[cell.ch as usize * 16..cell.ch as usize * 16 + 16];
                for (gy, &bits) in glyph.iter().enumerate() {
                    let y = row * 16 + gy;
                    for gx in 0..8usize {
                        let on = bits & (0x80 >> gx) != 0;
                        let c = if on { cell.fg } else { cell.bg };
                        let i = (y * w as usize + col * 8 + gx) * 4;
                        pic.rgba[i..i + 3].copy_from_slice(&c);
                    }
                }
            }
        }
        pic
    }
}

/// Reads an ANSI file and draws it.  `top_square` limits the picture to its
/// top part, as wide as it is tall, for thumbnails of very long files;
/// otherwise at most [`MAX_PICTURE_ROWS`] rows are drawn.
pub fn decode(bytes: &[u8], top_square: bool) -> Result<(Pic, String), LoadError> {
    let meta = sauce(bytes).filter(is_text_sauce);
    let data = &bytes[..meta.as_ref().map_or(bytes.len(), |s| s.data_len.min(bytes.len()))];
    let cols = meta.as_ref().map_or(80, |s| if s.info[0] == 0 { 80 } else { s.info[0] as usize });
    let ice = meta.as_ref().is_some_and(Sauce::ice_colours);
    let grid = interpret(data, cols, ice);
    // A thumbnail shows the whole picture while it is not much taller than
    // wide, otherwise its top square; both fit one 8x16 cell row per 16
    // pixels.
    let limit = if top_square && grid.rows * 16 > grid.cols * 8 * 2 { grid.cols * 8 / 16 } else { MAX_PICTURE_ROWS };
    check_size(grid.cols as u32 * 8, grid.rows.min(limit) as u32 * 16)?;
    let shown = grid.rows.min(limit);
    let pic = grid.render(limit);
    let mut detail = format!("{}x{} characters (code page 437, 8x16 font", grid.cols, grid.rows);
    if ice {
        detail.push_str(", iCE colours");
    }
    detail.push(')');
    if shown < grid.rows {
        detail.push_str(&format!(", showing the first {shown} lines"));
    }
    if let Some(s) = &meta {
        for (k, v) in [("title", &s.title), ("author", &s.author), ("group", &s.group)] {
            if !v.is_empty() {
                detail.push_str(&format!("; {k}: {v}"));
            }
        }
        let date = s.date_text();
        if !date.is_empty() {
            detail.push_str(&format!("; date: {date}"));
        }
        if !s.font.is_empty() {
            detail.push_str(&format!("; font: {}", s.font));
        }
        for c in s.comments.iter().filter(|c| !c.is_empty()).take(8) {
            detail.push_str(&format!("; comment: {c}"));
        }
    }
    Ok((pic, detail))
}

//endregion

#[cfg(test)]
mod tests {
    use super::*;

    fn grid(s: &str) -> Grid {
        interpret(s.as_bytes(), 80, false)
    }

    fn cell(g: &Grid, x: usize, y: usize) -> Cell {
        g.cells[y * g.cols + x]
    }

    #[test]
    fn plain_text_wraps_at_the_width() {
        let g = interpret(b"abcdef", 4, false);
        assert_eq!((g.cols, g.rows), (4, 2));
        assert_eq!(cell(&g, 0, 1).ch, b'e');
    }

    #[test]
    fn colours_bold_and_reset() {
        // Red text (SGR 31 is VGA colour 4), then bold makes it light red.
        let g = grid("\x1b[31mR\x1b[1mB\x1b[0mN\x1b[44mx");
        assert_eq!(cell(&g, 0, 0).fg, VGA[4]);
        assert_eq!(cell(&g, 1, 0).fg, VGA[12]);
        assert_eq!(cell(&g, 2, 0).fg, VGA[7]);
        assert_eq!(cell(&g, 3, 0).bg, VGA[1]);
    }

    #[test]
    fn extended_colours() {
        let g = grid("\x1b[38;5;196ma\x1b[38;2;1;2;3;48;5;232mb\x1b[1;255;0;0tc");
        assert_eq!(cell(&g, 0, 0).fg, [255, 0, 0]);
        assert_eq!(cell(&g, 1, 0).fg, [1, 2, 3]);
        assert_eq!(cell(&g, 1, 0).bg, [8, 8, 8]);
        assert_eq!(cell(&g, 2, 0).fg, [255, 0, 0]);
    }

    #[test]
    fn cursor_movement_positions_text() {
        let g = grid("\x1b[3;5Hx\x1b[2Dy\x1b[1Az\x1b[10Gq");
        assert_eq!(cell(&g, 4, 2).ch, b'x');
        // After x the cursor is at column 6 (index 5); two back is index 3.
        assert_eq!(cell(&g, 3, 2).ch, b'y');
        assert_eq!(cell(&g, 4, 1).ch, b'z');
        assert_eq!(cell(&g, 9, 1).ch, b'q');
        assert_eq!(g.rows, 3);
    }

    #[test]
    fn save_restore_and_clear() {
        let g = grid("ab\x1b[sc\x1b[ud\x1b[2Jhi");
        assert_eq!(cell(&g, 0, 0).ch, b'h');
        assert_eq!(cell(&g, 1, 0).ch, b'i');
        let g = grid("ab\x1b[sc\x1b[ud");
        assert_eq!(cell(&g, 2, 0).ch, b'd');
    }

    #[test]
    fn blink_is_a_bright_background_only_with_ice() {
        let off = interpret(b"\x1b[5;44mx", 80, false);
        assert_eq!(cell(&off, 0, 0).bg, VGA[1]);
        let on = interpret(b"\x1b[5;44mx", 80, true);
        assert_eq!(cell(&on, 0, 0).bg, VGA[9]);
    }

    #[test]
    fn line_feed_alone_returns_the_cursor_in_unix_files() {
        let g = grid("ab\ncd");
        assert_eq!(cell(&g, 0, 1).ch, b'c');
        let g = grid("ab\r\ncd");
        assert_eq!(cell(&g, 0, 1).ch, b'c');
        // With carriage returns present, a bare line feed keeps the column.
        let g = grid("ab\r\ncd\nef");
        assert_eq!(cell(&g, 2, 2).ch, b'e');
    }

    #[test]
    fn eof_marker_stops_the_text() {
        let g = grid("ab\x1acd");
        assert_eq!(g.rows, 1);
        assert_eq!(cell(&g, 2, 0).ch, b' ');
    }

    fn sauce_record(title: &str, author: &str, group: &str, width: u16, flags: u8) -> Vec<u8> {
        let mut r = b"SAUCE00".to_vec();
        let pad = |s: &str, n: usize| {
            let mut v = s.as_bytes().to_vec();
            v.resize(n, b' ');
            v
        };
        r.extend(pad(title, 35));
        r.extend(pad(author, 20));
        r.extend(pad(group, 20));
        r.extend(b"19960405");
        r.extend(0u32.to_le_bytes());
        r.extend([1, 1]);
        r.extend(width.to_le_bytes());
        r.extend(25u16.to_le_bytes());
        r.extend([0u8; 4]);
        r.push(0);
        r.push(flags);
        r.extend([0u8; 22]);
        assert_eq!(r.len(), 128);
        r
    }

    #[test]
    fn sauce_fields_and_width() {
        let mut f = b"\x1b[1;33mHello".to_vec();
        f.push(0x1a);
        f.extend(sauce_record("My Art", "someone", "SOMEGROUP", 40, 1));
        let s = sauce(&f).unwrap();
        assert_eq!((s.title.as_str(), s.author.as_str(), s.group.as_str()), ("My Art", "someone", "SOMEGROUP"));
        assert_eq!(s.date_text(), "1996-04-05");
        assert_eq!(s.info[0], 40);
        assert!(s.ice_colours());
        assert_eq!(s.data_len, f.len() - 129);
        let (pic, detail) = decode(&f, false).unwrap();
        assert_eq!((pic.width, pic.height), (40 * 8, 16));
        assert!(detail.contains("40x1") && detail.contains("title: My Art") && detail.contains("1996-04-05"));
    }

    #[test]
    fn comments_are_read() {
        let mut f = b"hi".to_vec();
        f.push(0x1a);
        f.extend(b"COMNT");
        let mut line = b"first comment".to_vec();
        line.resize(64, b' ');
        f.extend(line);
        let mut rec = sauce_record("t", "a", "g", 80, 0);
        rec[104] = 1;
        f.extend(rec);
        let s = sauce(&f).unwrap();
        assert_eq!(s.comments, vec!["first comment".to_string()]);
        assert_eq!(s.data_len, 2);
    }

    #[test]
    fn glyphs_are_drawn_in_cells() {
        let (pic, _) = decode(b"\xdb", false).unwrap();
        // A full block in light grey on black fills its 8x16 cell.
        assert_eq!((pic.width, pic.height), (640, 16));
        assert_eq!(pic.get(0, 0), VGA[7]);
        assert_eq!(pic.get(7, 15), VGA[7]);
        assert_eq!(pic.get(8, 0), VGA[0]);
        // A space is all background.
        let (pic, _) = decode(b"\x1b[44m ", false).unwrap();
        assert_eq!(pic.get(3, 3), VGA[1]);
    }

    #[test]
    fn tall_files_give_the_top_square_for_thumbnails() {
        let mut f = Vec::new();
        for _ in 0..200 {
            f.extend(b"line\r\n");
        }
        let (full, _) = decode(&f, false).unwrap();
        assert_eq!(full.height, 200 * 16);
        let (top, detail) = decode(&f, true).unwrap();
        assert_eq!((top.width, top.height), (640, 640));
        assert!(detail.contains("first 40 lines"));
        // An 80x25 screen is shown whole.
        let (whole, _) = decode(&f[..6 * 25], true).unwrap();
        assert_eq!(whole.height, 25 * 16);
    }

    #[test]
    fn hostile_input_stays_small() {
        let g = grid("\x1b[32000;32000Hx\x1b[99999999999999999999Bx");
        assert!(g.rows <= MAX_ROWS);
        let mut junk = Vec::new();
        for i in 0..100_000u32 {
            junk.push((i.wrapping_mul(2654435761) >> 24) as u8);
        }
        let _ = decode(&junk, false);
        let mut many = b"\x1b[".to_vec();
        many.extend(std::iter::repeat_n(b'1', 100_000));
        many.push(b'm');
        let _ = decode(&many, false);
        let mut params = b"\x1b[".to_vec();
        for _ in 0..10_000 {
            params.extend(b"38;5;1;");
        }
        params.push(b'm');
        let _ = decode(&params, false);
    }
}
