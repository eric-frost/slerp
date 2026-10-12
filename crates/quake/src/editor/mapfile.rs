//! The text of Quake-family `.map` files: entities of quoted key/value
//! pairs holding brushes and patches.  One reader covers the dialects
//! editors write:
//!
//! - Quake's standard faces: three points, a texture, then shift, rotation
//!   and scale (`( p ) ( p ) ( p ) TEX 0 0 0 1 1`).
//! - Valve 220 (Half-Life, and TrenchBroom's "Valve" format for any game):
//!   explicit texture axes (`TEX [ ux uy uz shift ] [ vx vy vz shift ] rot
//!   sx sy`).
//! - Quake 2 and 3: three surface numbers (contents, flags, value) after
//!   either kind of face.
//! - Quake 3's Radiant extras: brush primitives (`brushDef`, a texture
//!   matrix per face), Doom 3's `brushDef3` (plane equations), and Bezier
//!   patches (`patchDef2`, `patchDef3`).
//!
//! TrenchBroom's `// Game:` and `// Format:` comments are read as hints.
//! Damaged brushes are skipped and counted rather than failing the file.

use super::brush::{Plane, TexVecs};
use crate::entities::Entity;
use slerp_scene::glam::DVec3;

/// The most entries in one patch's control grid along either side.
/// Quake 3 stops at 32; editors allow a few more.
const MAX_PATCH_SIDE: usize = 129;

/*┌─────────────────╮
| Model           |
└─────────────────╯*/
//region Model

pub(crate) struct MapFile {
    pub entities: Vec<MapEntity>,
    /// TrenchBroom's `// Game:` comment, such as "Quake" or "Half-Life".
    pub game: Option<String>,
    /// TrenchBroom's `// Format:` comment, such as "Valve" or "Quake2".
    pub format: Option<String>,
    /// Brushes and patches that couldn't be read.
    pub bad_brushes: usize,
    pub bad_patches: usize,
    /// True when any face had Quake 2 or 3 surface numbers.
    pub surface_flags: bool,
    /// True when any face used Valve's explicit texture axes.
    pub valve_axes: bool,
}

#[derive(Default)]
pub(crate) struct MapEntity {
    pub keys: Entity,
    pub brushes: Vec<MapBrush>,
    pub patches: Vec<MapPatch>,
}

pub(crate) struct MapBrush {
    pub sides: Vec<MapSide>,
}

pub(crate) struct MapSide {
    pub plane: Plane,
    pub texture: String,
    pub tex: TexSpec,
    /// Quake 2 and 3's contents, surface flags and value.
    pub flags: Option<[i64; 3]>,
}

/// How a face's texture is placed, as the file says it.
#[derive(Clone, Copy, Debug)]
pub(crate) enum TexSpec {
    Standard { shift: [f64; 2], rotation: f64, scale: [f64; 2] },
    Valve { u: DVec3, v: DVec3, shift: [f64; 2], scale: [f64; 2] },
    Matrix([[f64; 3]; 2]),
}

impl TexSpec {
    pub fn vecs(&self, normal: DVec3) -> TexVecs {
        match *self {
            TexSpec::Standard { shift, rotation, scale } => TexVecs::standard(normal, shift, rotation, scale),
            TexSpec::Valve { u, v, shift, scale } => TexVecs::axes(u, v, shift, scale),
            TexSpec::Matrix(m) => TexVecs::matrix(normal, m),
        }
    }
}

/// A Bezier patch's control grid: `rows` lines of `columns` points, each
/// a position and a texture coordinate in texture widths.
pub(crate) struct MapPatch {
    pub texture: String,
    pub rows: usize,
    pub columns: usize,
    pub points: Vec<[f64; 5]>,
}

//endregion

/*┌─────────────────╮
| Tokens          |
└─────────────────╯*/
//region Tokens

#[derive(Clone, Debug, PartialEq)]
enum Tok<'a> {
    Open,
    Close,
    /// `(`, `)`, `[` or `]`.
    Punct(u8),
    /// A quoted string, without its quotes.
    Quoted(&'a str),
    Word(&'a str),
}

struct Lexer<'a> {
    s: &'a [u8],
    at: usize,
    peeked: Option<Option<Tok<'a>>>,
    /// How many braces are open among the tokens taken so far.
    depth: usize,
}

impl<'a> Lexer<'a> {
    fn new(s: &'a [u8]) -> Self {
        Lexer { s, at: 0, peeked: None, depth: 0 }
    }

    fn text(&self, start: usize, end: usize) -> &'a str {
        // Map text is ASCII in practice; a stray byte costs only its token.
        std::str::from_utf8(&self.s[start..end]).unwrap_or("")
    }

    fn peek(&mut self) -> Option<&Tok<'a>> {
        if self.peeked.is_none() {
            self.peeked = Some(self.scan());
        }
        self.peeked.as_ref().and_then(Option::as_ref)
    }

    fn next(&mut self) -> Option<Tok<'a>> {
        let t = match self.peeked.take() {
            Some(t) => t,
            None => self.scan(),
        };
        match t {
            Some(Tok::Open) => self.depth += 1,
            Some(Tok::Close) => self.depth = self.depth.saturating_sub(1),
            _ => {}
        }
        t
    }

    /// The next token, skipping whitespace and `//` comments.  Braces count
    /// only on their own, since texture names may start with `{`.
    fn scan(&mut self) -> Option<Tok<'a>> {
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
        let c = *s.get(self.at)?;
        let lone = |i: usize| s.get(i).is_none_or(|b| b.is_ascii_whitespace() || *b == b'"');
        match c {
            b'{' | b'}' if lone(self.at + 1) => {
                self.at += 1;
                Some(if c == b'{' { Tok::Open } else { Tok::Close })
            }
            b'(' | b')' | b'[' | b']' => {
                self.at += 1;
                Some(Tok::Punct(c))
            }
            b'"' => {
                let start = self.at + 1;
                let len = s[start..].iter().position(|&b| b == b'"').unwrap_or(s.len() - start);
                self.at = (start + len + 1).min(s.len());
                Some(Tok::Quoted(self.text(start, start + len)))
            }
            _ => {
                let start = self.at;
                while self.at < s.len() && !s[self.at].is_ascii_whitespace() && !b"\"()[]".contains(&s[self.at]) {
                    self.at += 1;
                }
                Some(Tok::Word(self.text(start, self.at)))
            }
        }
    }

    fn number(&mut self) -> Option<f64> {
        match self.next()? {
            Tok::Word(w) => w.parse::<f64>().ok().filter(|v| v.is_finite()),
            _ => None,
        }
    }

    fn expect(&mut self, c: u8) -> Option<()> {
        (self.next()? == Tok::Punct(c)).then_some(())
    }

    /// `( a b c )`.
    fn triple(&mut self) -> Option<DVec3> {
        self.expect(b'(')?;
        let v = DVec3::new(self.number()?, self.number()?, self.number()?);
        self.expect(b')')?;
        Some(v)
    }

    /// A texture name, quoted or not.
    fn name(&mut self) -> Option<String> {
        match self.next()? {
            Tok::Word(w) | Tok::Quoted(w) => Some(w.to_string()),
            _ => None,
        }
    }

    /// Numbers up to the next brace or parenthesis, which ends a face.
    fn trailing_numbers(&mut self) -> Vec<f64> {
        let mut out = Vec::new();
        while let Some(Tok::Word(w)) = self.peek() {
            let Ok(v) = w.parse::<f64>() else { break };
            self.next();
            out.push(if v.is_finite() { v } else { 0.0 });
            if out.len() > 16 {
                break;
            }
        }
        out
    }

    /// Skips tokens until fewer than `depth` braces are open: just past
    /// the `}` of the block that was opened at that depth.
    fn close_to(&mut self, depth: usize) {
        while self.depth >= depth && self.next().is_some() {}
    }
}

//endregion

/*┌─────────────────╮
| Parsing         |
└─────────────────╯*/
//region Parsing

/// Reads a whole map.  Text the reader doesn't understand between entities
/// is skipped; a file with no entities is still a (empty) map.
pub(crate) fn parse(bytes: &[u8]) -> MapFile {
    let bytes = bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(bytes);
    let mut map = MapFile {
        entities: Vec::new(),
        game: None,
        format: None,
        bad_brushes: 0,
        bad_patches: 0,
        surface_flags: false,
        valve_axes: false,
    };
    for line in bytes.split(|&b| b == b'\n').take(16) {
        let line = String::from_utf8_lossy(line);
        let line = line.trim();
        if let Some(v) = line.strip_prefix("// Game:") {
            map.game = Some(v.trim().to_string());
        } else if let Some(v) = line.strip_prefix("// Format:") {
            map.format = Some(v.trim().to_string());
        }
    }

    let mut lex = Lexer::new(bytes);
    while let Some(t) = lex.next() {
        if t == Tok::Open {
            let e = entity(&mut lex, &mut map);
            map.entities.push(e);
        }
    }
    map
}

fn entity<'a>(lex: &mut Lexer<'a>, map: &mut MapFile) -> MapEntity {
    let mut e = MapEntity::default();
    loop {
        match lex.next() {
            None | Some(Tok::Close) => return e,
            Some(Tok::Open) => brush(lex, map, &mut e),
            Some(Tok::Quoted(k) | Tok::Word(k)) => {
                // A key without a value is dropped.
                let value = match lex.peek() {
                    Some(Tok::Quoted(_) | Tok::Word(_)) => lex.next(),
                    _ => None,
                };
                if let Some(Tok::Quoted(v) | Tok::Word(v)) = value {
                    e.keys.pairs.push((k.to_string(), v.to_string()));
                }
            }
            Some(Tok::Punct(_)) => {}
        }
    }
}

/// One `{ ... }` inside an entity, whose `{` has been read: a brush in
/// any dialect, or a patch.  A damaged one is skipped to its `}`.
fn brush<'a>(lex: &mut Lexer<'a>, map: &mut MapFile, e: &mut MapEntity) {
    let depth = lex.depth;
    let word = match lex.peek() {
        Some(Tok::Word(w)) => Some(w.to_ascii_lowercase()),
        _ => None,
    };
    match word.as_deref() {
        Some(w @ ("patchdef2" | "patchdef3" | "patchdef2ws")) => {
            let def3 = w == "patchdef3";
            lex.next();
            match patch(lex, def3) {
                Some(p) => e.patches.push(p),
                None => map.bad_patches += 1,
            }
        }
        Some(w @ ("brushdef" | "brushdef3")) => {
            let def3 = w == "brushdef3";
            lex.next();
            match primitive_brush(lex, def3, map) {
                Some(b) => e.brushes.push(b),
                None => map.bad_brushes += 1,
            }
        }
        _ => {
            let mut sides = Vec::new();
            loop {
                match lex.peek() {
                    Some(Tok::Close) | None => break,
                    Some(Tok::Punct(b'(')) => match side(lex, map) {
                        Some(s) => sides.extend(s),
                        None => {
                            map.bad_brushes += 1;
                            lex.close_to(depth);
                            return;
                        }
                    },
                    _ => {
                        map.bad_brushes += 1;
                        lex.close_to(depth);
                        return;
                    }
                }
            }
            e.brushes.push(MapBrush { sides });
        }
    }
    lex.close_to(depth);
}

/// The faces of a `brushDef` or `brushDef3` block, after its word.
fn primitive_brush(lex: &mut Lexer, def3: bool, map: &mut MapFile) -> Option<MapBrush> {
    if lex.next()? != Tok::Open {
        return None;
    }
    let mut sides = Vec::new();
    loop {
        match lex.peek()? {
            Tok::Close => {
                lex.next();
                return Some(MapBrush { sides });
            }
            Tok::Punct(b'(') => sides.extend(primitive_side(lex, def3, map)?),
            _ => return None,
        }
    }
}

/// One face of a classic brush, in any of the dialects.  `Some(None)` is a
/// face whose points are in a line, which qbsp also drops.
fn side(lex: &mut Lexer, map: &mut MapFile) -> Option<Option<MapSide>> {
    let p = [lex.triple()?, lex.triple()?, lex.triple()?];
    let texture = lex.name()?;
    let tex = if lex.peek() == Some(&Tok::Punct(b'[')) {
        let mut axis = || -> Option<(DVec3, f64)> {
            lex.expect(b'[')?;
            let v = DVec3::new(lex.number()?, lex.number()?, lex.number()?);
            let shift = lex.number()?;
            lex.expect(b']')?;
            Some((v, shift))
        };
        let (u, su) = axis()?;
        let (v, sv) = axis()?;
        let rest = lex.trailing_numbers();
        map.valve_axes = true;
        if rest.len() >= 6 {
            map.surface_flags = true;
        }
        let scale = [rest.get(1).copied().unwrap_or(1.0), rest.get(2).copied().unwrap_or(1.0)];
        let flags = (rest.len() >= 6).then(|| [rest[3] as i64, rest[4] as i64, rest[5] as i64]);
        (TexSpec::Valve { u, v, shift: [su, sv], scale }, flags)
    } else {
        let rest = lex.trailing_numbers();
        if rest.len() < 5 {
            return None;
        }
        if rest.len() >= 8 {
            map.surface_flags = true;
        }
        let flags = (rest.len() >= 8).then(|| [rest[5] as i64, rest[6] as i64, rest[7] as i64]);
        (TexSpec::Standard { shift: [rest[0], rest[1]], rotation: rest[2], scale: [rest[3], rest[4]] }, flags)
    };
    Some(Plane::from_points(p).map(|plane| MapSide { plane, texture, tex: tex.0, flags: tex.1 }))
}

/// A face of a `brushDef` (three points) or `brushDef3` (a plane equation)
/// with its texture matrix: `( ( a b c ) ( d e f ) ) TEX contents flags
/// value`.
fn primitive_side(lex: &mut Lexer, def3: bool, map: &mut MapFile) -> Option<Option<MapSide>> {
    let plane = if def3 {
        lex.expect(b'(')?;
        let n = DVec3::new(lex.number()?, lex.number()?, lex.number()?);
        let d = lex.number()?;
        lex.expect(b')')?;
        // Doom 3 writes `a b c d` with `ax + by + cz + d = 0`.
        Plane::from_equation(n, -d)
    } else {
        Plane::from_points([lex.triple()?, lex.triple()?, lex.triple()?])
    };
    lex.expect(b'(')?;
    let r0 = lex.triple()?;
    let r1 = lex.triple()?;
    lex.expect(b')')?;
    let texture = lex.name()?;
    let rest = lex.trailing_numbers();
    if rest.len() >= 3 {
        map.surface_flags = true;
    }
    let flags = (rest.len() >= 3).then(|| [rest[0] as i64, rest[1] as i64, rest[2] as i64]);
    let m = [[r0.x, r0.y, r0.z], [r1.x, r1.y, r1.z]];
    Some(plane.map(|plane| MapSide { plane, texture, tex: TexSpec::Matrix(m), flags }))
}

/// A patch after its `patchDef2` or `patchDef3` word: `{ TEX ( w h ... )
/// ( ( ( x y z u v ) ... ) ... ) }`.  Doom 3's `patchDef3` adds two
/// subdivision counts to the size, which are ignored.
fn patch(lex: &mut Lexer, def3: bool) -> Option<MapPatch> {
    if lex.next()? != Tok::Open {
        return None;
    }
    let texture = lex.name()?;
    lex.expect(b'(')?;
    let rows = lex.number()?;
    let columns = lex.number()?;
    // patchDef2 adds contents, flags and value; patchDef3 two subdivision
    // counts before them.  None matter here.
    let _ = (lex.trailing_numbers(), def3);
    lex.expect(b')')?;
    let side_ok = |v: f64| v >= 3.0 && v <= MAX_PATCH_SIDE as f64 && v.fract() == 0.0 && (v as usize) % 2 == 1;
    if !side_ok(rows) || !side_ok(columns) {
        return None;
    }
    let (rows, columns) = (rows as usize, columns as usize);
    let mut points = Vec::with_capacity(rows * columns);
    lex.expect(b'(')?;
    for _ in 0..rows {
        lex.expect(b'(')?;
        for _ in 0..columns {
            lex.expect(b'(')?;
            let mut p = [0.0; 5];
            for v in &mut p {
                *v = lex.number()?;
            }
            lex.expect(b')')?;
            points.push(p);
        }
        lex.expect(b')')?;
    }
    lex.expect(b')')?;
    Some(MapPatch { texture, rows, columns, points })
}

//endregion

/*┌─────────────────╮
| Sniffing        |
└─────────────────╯*/
//region Sniffing

/// Whether the start of a file is a Quake-family map: TrenchBroom's
/// `// Game:` line, or (after other comments) an entity block whose quoted
/// key/value pairs include `"classname"`.  Pairs are separated by
/// whitespace only, so JSON (`{"version":3,...}`, as in JavaScript source
/// maps) never matches, nor do the many other formats that use `.map`.
pub(crate) fn sniff(head: &[u8]) -> bool {
    let head = head.strip_prefix(b"\xef\xbb\xbf").unwrap_or(head);
    if head.starts_with(b"// Game:") {
        return true;
    }
    let mut at = 0;
    // 1. Comments and blank lines, and Doom 3's "Version 2" line.
    loop {
        while at < head.len() && head[at].is_ascii_whitespace() {
            at += 1;
        }
        if head[at..].starts_with(b"//") || head[at..].starts_with(b"Version ") {
            match head[at..].iter().position(|&b| b == b'\n') {
                Some(n) => at += n + 1,
                None => return false,
            }
            continue;
        }
        break;
    }
    // 2. The first entity's opening brace on its own.
    if head.get(at) != Some(&b'{') || !head.get(at + 1).is_some_and(u8::is_ascii_whitespace) {
        return false;
    }
    at += 1;
    // 3. Quoted pairs (and comments) until "classname".
    let quoted = |at: &mut usize| -> Option<&[u8]> {
        if head.get(*at) != Some(&b'"') {
            return None;
        }
        let start = *at + 1;
        let len = head.get(start..)?.iter().position(|&b| b == b'"' || b == b'\n')?;
        if head[start + len] != b'"' {
            return None;
        }
        *at = start + len + 1;
        Some(&head[start..start + len])
    };
    for _ in 0..64 {
        while at < head.len() && head[at].is_ascii_whitespace() {
            at += 1;
        }
        if head[at..].starts_with(b"//") {
            match head[at..].iter().position(|&b| b == b'\n') {
                Some(n) => at += n + 1,
                None => return false,
            }
            continue;
        }
        let Some(key) = quoted(&mut at) else { return false };
        // Only spaces or tabs between a key and its value.
        let gap = head[at..].iter().take_while(|&&b| b == b' ' || b == b'\t').count();
        if gap == 0 {
            return false;
        }
        at += gap;
        if quoted(&mut at).is_none() {
            return false;
        }
        if key.eq_ignore_ascii_case(b"classname") {
            return true;
        }
    }
    false
}

//endregion

#[cfg(test)]
mod tests {
    use super::*;

    const STANDARD: &str = "// Game: Quake\n// Format: Standard\n// entity 0\n{\n\"classname\" \"worldspawn\"\n\"wad\" \"gfx/base.wad\"\n// brush 0\n{\n( -64 -64 -16 ) ( -64 -63 -16 ) ( -64 -64 -15 ) {fence 0 0 0 1 1\n( -64 -64 -16 ) ( -64 -64 -15 ) ( -63 -64 -16 ) *water1 16 8 90 0.5 2\n( -64 -64 -16 ) ( -63 -64 -16 ) ( -64 -63 -16 ) sky4 0 0 0 1 1\n( 64 64 16 ) ( 64 65 16 ) ( 65 64 16 ) +0lever 0 0 0 1 1\n( 64 64 16 ) ( 65 64 16 ) ( 64 64 17 ) wall 0 0 0 1 1\n( 64 64 16 ) ( 64 64 17 ) ( 64 65 16 ) wall 0 0 0 1 1\n}\n}\n{\n\"classname\" \"info_player_start\"\n\"origin\" \"0 0 24\"\n}\n";

    #[test]
    fn reads_standard_faces() {
        let m = parse(STANDARD.as_bytes());
        assert_eq!(m.game.as_deref(), Some("Quake"));
        assert_eq!(m.format.as_deref(), Some("Standard"));
        assert_eq!(m.entities.len(), 2);
        let b = &m.entities[0].brushes[0];
        assert_eq!(b.sides.len(), 6);
        let names: Vec<&str> = b.sides.iter().map(|s| s.texture.as_str()).collect();
        assert_eq!(names, ["{fence", "*water1", "sky4", "+0lever", "wall", "wall"]);
        let TexSpec::Standard { shift, rotation, scale } = b.sides[1].tex else { panic!() };
        assert_eq!((shift, rotation, scale), ([16.0, 8.0], 90.0, [0.5, 2.0]));
        assert!(b.sides[1].flags.is_none());
        assert_eq!(m.entities[1].keys.classname(), "info_player_start");
        assert!(!m.valve_axes && !m.surface_flags && m.bad_brushes == 0);
    }

    #[test]
    fn reads_valve_and_quake2_faces() {
        let text = "{\n\"classname\" \"worldspawn\"\n\"mapversion\" \"220\"\n{\n( 0 0 0 ) ( 0 1 0 ) ( 1 0 0 ) e1u1/floor [ 1 0 0 16 ] [ 0 -1 0 -8 ] 0 0.5 0.25 0 512 7\n( 0 0 0 ) ( 0 1 0 ) ( 1 0 0 ) \"tex with space\" [ 1 0 0 0 ] [ 0 -1 0 0 ] 0 1 1\n( 0 0 0 ) ( 0 1 0 ) ( 1 0 0 ) q2wall 0 0 0 1 1 1 2 3\n}\n}";
        let m = parse(text.as_bytes());
        let s = &m.entities[0].brushes[0].sides;
        assert_eq!(s.len(), 3);
        let TexSpec::Valve { u, v, shift, scale } = s[0].tex else { panic!() };
        assert_eq!((u, v, shift, scale), (DVec3::X, -DVec3::Y, [16.0, -8.0], [0.5, 0.25]));
        assert_eq!(s[0].flags, Some([0, 512, 7]));
        assert_eq!(s[1].texture, "tex with space");
        assert_eq!(s[2].flags, Some([1, 2, 3]));
        assert!(m.valve_axes && m.surface_flags);
    }

    #[test]
    fn reads_brush_primitives_and_patches() {
        let text = "// entity 0\n{\n\"classname\" \"worldspawn\"\n// brush 0\n{\nbrushDef\n{\n( 0 0 64 ) ( 1 0 64 ) ( 0 1 64 ) ( ( 0.0078125 0 0 ) ( 0 0.0078125 0 ) ) common/caulk 0 0 0\n}\n}\n// brush 1\n{\nbrushDef3\n{\n( 0 0 1 -64 ) ( ( 0.0078125 0 0 ) ( 0 0.0078125 0 ) ) \"textures/base/floor\" 0 0 0\n}\n}\n{\npatchDef2\n{\nbase_wall/metal\n( 3 3 0 0 0 )\n(\n( ( -64 -64 0 0 0 ) ( -64 0 0 0 0.5 ) ( -64 64 0 0 1 ) )\n( ( 0 -64 32 0.5 0 ) ( 0 0 32 0.5 0.5 ) ( 0 64 32 0.5 1 ) )\n( ( 64 -64 0 1 0 ) ( 64 0 0 1 0.5 ) ( 64 64 0 1 1 ) )\n)\n}\n}\n{\npatchDef2\n{\nbad\n( 4 3 0 0 0 )\n(\n)\n}\n}\n}\n";
        let m = parse(text.as_bytes());
        let e = &m.entities[0];
        assert_eq!(e.brushes.len(), 2);
        assert!(matches!(e.brushes[0].sides[0].tex, TexSpec::Matrix(_)));
        assert_eq!(e.brushes[0].sides[0].flags, Some([0, 0, 0]));
        let p = e.brushes[1].sides[0].plane;
        assert_eq!((p.normal, p.dist), (DVec3::Z, 64.0));
        assert_eq!(e.brushes[1].sides[0].texture, "textures/base/floor");
        assert_eq!(e.patches.len(), 1);
        assert_eq!((e.patches[0].rows, e.patches[0].columns), (3, 3));
        assert_eq!(e.patches[0].points[4], [0.0, 0.0, 32.0, 0.5, 0.5]);
        assert_eq!(m.bad_patches, 1, "an even-sized patch");
        assert_eq!(m.entities.len(), 1);
    }

    #[test]
    fn damaged_text_never_panics() {
        let mut all = vec![STANDARD.as_bytes().to_vec()];
        all.extend(crate::testutil::mutations(STANDARD.as_bytes()));
        for cut in 0..STANDARD.len() {
            all.push(STANDARD.as_bytes()[..cut].to_vec());
        }
        all.push(b"{{{{{{{{{{{{{{{{".to_vec());
        all.push(b"}}}} ( ( ( [ [ ] \"".to_vec());
        all.push(b"{ \"a\" { ( 1e400 0 0 ) ( 0 0 0 ) ( 0 0 nan ) x 0 0 0 1 1 } }".to_vec());
        for bytes in all {
            let m = parse(&bytes);
            for e in &m.entities {
                for b in &e.brushes {
                    for s in &b.sides {
                        assert!(s.plane.normal.is_finite() && s.plane.dist.is_finite());
                    }
                }
            }
        }
    }

    #[test]
    fn sniffs_maps_but_not_lookalikes() {
        assert!(sniff(STANDARD.as_bytes()));
        assert!(sniff(b"{\n\"sounds\" \"6\"\n\"classname\" \"worldspawn\"\n\"wad\" \"gfx/base.wad\"\n{\n"));
        assert!(sniff(b"\xef\xbb\xbf// entity 0\r\n{\r\n\"classname\" \"worldspawn\"\r\n"));
        assert!(sniff(b"Version 2\n// entity 0\n{\n\"classname\" \"worldspawn\"\n"));
        assert!(sniff(b"// Game: Half-Life\n// Format: Valve\n"));
        // JavaScript and CSS source maps, Doxygen image maps, linker maps.
        assert!(!sniff(br#"{"version":3,"file":"furo.js","mappings":";iCAAA,MAQ","sources":["classname"]}"#));
        assert!(!sniff(b"{\n  \"classname\": \"worldspawn\"\n}"));
        assert!(!sniff(b"<map id=\"lib/strings.c\" name=\"lib/strings.c\">\n<area shape=\"rect\"/>"));
        assert!(!sniff(b"Archive member included to satisfy reference by file (symbol)\n\n"));
        assert!(!sniff(b"{\n\"key\" \"value\"\n}\n"), "no classname");
        assert!(!sniff(b"{\n\"classname\""), "cut short");
        assert!(!sniff(b""));
    }
}
