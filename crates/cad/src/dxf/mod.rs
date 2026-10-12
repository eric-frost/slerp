//! Reading DXF, AutoCAD's drawing exchange format: text and binary files of
//! every version from R10 to the current one.
//!
//! A DXF file is a flat list of group-code pairs (see `pairs`) organised
//! into sections: HEADER (settings), TABLES (layers, text styles), BLOCKS
//! (reusable drawings that INSERT entities place), and ENTITIES (the
//! drawing itself).  The reader takes the sections in order, expands block
//! references as it meets them, and ends with a `Drawing`.

mod aci;
mod curves;
mod emit;
mod encoding;
mod entities;
mod font;
mod pairs;
mod tables;
#[cfg(test)]
mod tests;
mod text;
pub mod thumbnail;

use crate::drawing::Drawing;
use emit::{Block, Builder, Ctx};
use encoding::Decoder;
use entities::{Color, Entity, Env, Kind, Parsed};
use glam::DVec3;
use pairs::{Pair, Reader};
use slerp_scene::{Info, LoadError};
use std::rc::Rc;

/// The largest file read: text DXF runs to tens or hundreds of megabytes,
/// and a drawing is held in memory several times over.
pub const MAX_BYTES: u64 = 768 << 20;

/// True when `head` (the first kilobytes of a file) starts like a DXF file:
/// the binary sentinel, or the pair `0 SECTION` (after any comments)
/// followed by `2` and a section name.
pub fn sniff(head: &[u8]) -> bool {
    if pairs::binary_start(head).is_some() {
        return true;
    }
    let mut r = Reader::new(head);
    let Some(a) = r.next() else { return false };
    if a.is(0, "SECTION") {
        let Some(b) = r.next() else { return false };
        return b.code == 2 && is_section_name(b.bytes());
    }
    // Fragments from simple writers leave out the SECTION line (and some
    // write `0 ENTITIES` where `2 ENTITIES` belongs).
    matches!(a.code, 0 | 2) && is_section_name(a.bytes()) && r.next().is_some_and(|b| b.code == 0)
}

fn is_section_name(name: &[u8]) -> bool {
    matches!(
        name.trim_ascii().to_ascii_uppercase().as_slice(),
        b"HEADER" | b"CLASSES" | b"TABLES" | b"BLOCKS" | b"ENTITIES" | b"OBJECTS" | b"THUMBNAILIMAGE"
    )
}

/// Loads the drawing in `data`.  `budget` is how many points it may hold.
pub fn load(data: &[u8], budget: usize) -> Result<Drawing, LoadError> {
    if data.len() as u64 > MAX_BYTES {
        return Err(LoadError::TooLarge(format!("{} bytes of DXF", data.len())));
    }
    if !sniff(&data[..data.len().min(4096)]) {
        return Err(LoadError::NotMine);
    }
    let mut parser = Parser::new(data, budget);
    parser.run();
    parser.finish()
}

/// AutoCAD release names by the version string in the header.
fn version_name(v: &str) -> &'static str {
    match v {
        "AC1006" => "R10",
        "AC1009" => "R11/R12",
        "AC1012" => "R13",
        "AC1014" => "R14",
        "AC1015" => "2000",
        "AC1018" => "2004",
        "AC1021" => "2007",
        "AC1024" => "2010",
        "AC1027" => "2013",
        "AC1032" => "2018",
        _ => "",
    }
}

fn units_name(code: i32) -> &'static str {
    match code {
        1 => "inches",
        2 => "feet",
        3 => "miles",
        4 => "millimetres",
        5 => "centimetres",
        6 => "metres",
        7 => "kilometres",
        8 => "microinches",
        9 => "mils",
        10 => "yards",
        11 => "angstroms",
        12 => "nanometres",
        13 => "micrometres",
        14 => "decimetres",
        15 => "decametres",
        16 => "hectometres",
        17 => "gigametres",
        18 => "astronomical units",
        19 => "light years",
        20 => "parsecs",
        _ => "",
    }
}

/// What the header says that matters here.
#[derive(Default)]
struct Header {
    version: String,
    codepage: String,
    units: i32,
    ext_min: Option<DVec3>,
    ext_max: Option<DVec3>,
}

struct Parser<'a> {
    r: Reader<'a>,
    b: Builder,
    header: Header,
    dec: Decoder,
    /// Block references in model space, drawn once every block is known.
    deferred: Vec<Entity>,
    buf: Vec<Pair<'a>>,
    entities: usize,
    saw_section: bool,
}

/// What `read_entity` found.
enum Item {
    Entity(Entity),
    /// A kind not drawn; already counted.
    Skipped,
    /// The end of the run: the name that ended it (ENDSEC, ENDBLK, ...), or
    /// nothing at the end of the file.
    End(Vec<u8>),
}

impl<'a> Parser<'a> {
    fn new(data: &'a [u8], budget: usize) -> Parser<'a> {
        Parser {
            r: Reader::new(data),
            b: Builder::new(budget),
            header: Header::default(),
            dec: Decoder::Western,
            deferred: Vec::new(),
            buf: Vec::new(),
            entities: 0,
            saw_section: false,
        }
    }

    fn run(&mut self) {
        // A fragment may start with the section's name, or with entities.
        if let Some(first) = self.r.peek().copied()
            && !first.is(0, "SECTION")
            && matches!(first.code, 0 | 2)
            && is_section_name(first.bytes())
        {
            self.r.next();
            self.saw_section = true;
            self.section(first.bytes());
        }
        while let Some(p) = self.r.next() {
            if p.is(0, "EOF") {
                break;
            }
            if !p.is(0, "SECTION") {
                continue;
            }
            let Some(n) = self.r.next() else { break };
            if n.code != 2 {
                continue;
            }
            self.saw_section = true;
            self.section(n.bytes());
            if self.b.over_budget {
                break;
            }
        }
        // Block references last: a file may list its blocks after its
        // entities, and this way both orders work.
        let deferred = std::mem::take(&mut self.deferred);
        for e in &deferred {
            self.b.emit(e, &Ctx::ROOT);
        }
    }

    fn section(&mut self, name: &[u8]) {
        match name.trim_ascii().to_ascii_uppercase().as_slice() {
            b"HEADER" => self.header_section(),
            b"TABLES" => self.tables_section(),
            b"BLOCKS" => self.blocks_section(),
            b"ENTITIES" => self.entities_section(),
            _ => self.skip_section(),
        }
    }

    fn finish(mut self) -> Result<Drawing, LoadError> {
        if !self.saw_section {
            return Err(LoadError::invalid("no DXF sections found"));
        }
        let mut d = self.b.finish();
        let h = &self.header;
        let version = match version_name(&h.version) {
            "" if h.version.is_empty() => "unknown version".to_string(),
            "" => h.version.clone(),
            name => format!("AutoCAD {name}"),
        };
        d.info = Info {
            format: format!("DXF ({version}, {})", if self.r.is_binary() { "binary" } else { "text" }),
            generator: None,
            warnings: std::mem::take(&mut self.b.warnings),
            details: Vec::new(),
            neighbours: Vec::new(),
        };
        if !h.version.is_empty() {
            d.info.details.push(("Version".into(), format!("{} ({})", version, h.version)));
        }
        let units = units_name(h.units);
        if !units.is_empty() {
            d.info.details.push(("Units".into(), units.into()));
        }
        d.info.details.push(("Entities read".into(), self.entities.to_string()));
        d.info.details.push(("Layers".into(), d.layers.len().to_string()));
        // Files that never recomputed their extents hold a single point or junk, which says nothing.
        let sane = |v: DVec3| v.x.is_finite() && v.y.is_finite() && v.x.abs() < 1e15 && v.y.abs() < 1e15;
        if let (Some(a), Some(b)) = (h.ext_min, h.ext_max)
            && sane(a)
            && sane(b)
            && (b.x > a.x || b.y > a.y)
        {
            d.info
                .details
                .push(("Header extents".into(), format!("({:.6}, {:.6}) to ({:.6}, {:.6})", a.x, a.y, b.x, b.y)));
        }
        if d.is_empty() {
            d.info.warnings.push("The drawing has nothing in model space to draw.".into());
        }
        if self.r.skipped_lines > 0 {
            d.info.warnings.push(format!("{} unreadable lines were skipped.", self.r.skipped_lines));
        }
        Ok(d)
    }

    fn env(&mut self) -> Env<'_> {
        Env { dec: self.dec, layers: &mut self.b.layers }
    }

    //region Sections

    fn skip_section(&mut self) {
        while let Some(p) = self.r.next() {
            if p.is(0, "ENDSEC") || p.is(0, "EOF") {
                return;
            }
        }
    }

    fn header_section(&mut self) {
        let mut var: Vec<u8> = Vec::new();
        let mut h = Header::default();
        while let Some(p) = self.r.next() {
            if p.is(0, "ENDSEC") || p.is(0, "EOF") {
                break;
            }
            if p.code == 9 {
                var = p.bytes().trim_ascii().to_ascii_uppercase();
                continue;
            }
            match (var.as_slice(), p.code) {
                (b"$ACADVER", 1) => h.version = String::from_utf8_lossy(p.bytes().trim_ascii()).into_owned(),
                (b"$DWGCODEPAGE", 3) => h.codepage = String::from_utf8_lossy(p.bytes().trim_ascii()).into_owned(),
                (b"$INSUNITS", 70) => h.units = p.i32(),
                (b"$EXTMIN", 10) => h.ext_min.get_or_insert(DVec3::ZERO).x = p.f64(),
                (b"$EXTMIN", 20) => h.ext_min.get_or_insert(DVec3::ZERO).y = p.f64(),
                (b"$EXTMAX", 10) => h.ext_max.get_or_insert(DVec3::ZERO).x = p.f64(),
                (b"$EXTMAX", 20) => h.ext_max.get_or_insert(DVec3::ZERO).y = p.f64(),
                _ => {}
            }
        }
        // R2007 and later are UTF-8; earlier files use the named code page.
        self.dec = if h.version.as_str() >= "AC1021" && h.version.starts_with("AC") {
            Decoder::Utf8
        } else if h.codepage.is_empty() {
            Decoder::Western
        } else {
            Decoder::from_codepage(&h.codepage)
        };
        self.header = h;
    }

    /// Collects the pairs of one table record (after its `0 NAME` pair).
    fn record(&mut self) {
        self.buf.clear();
        while let Some(q) = self.r.peek() {
            if q.code == 0 {
                break;
            }
            let q = self.r.next().unwrap();
            self.buf.push(q);
        }
    }

    fn tables_section(&mut self) {
        // Inside TABLE: records start with `0 LAYER`, `0 STYLE`, ...
        let mut table: Vec<u8> = Vec::new();
        while let Some(p) = self.r.next() {
            if p.code != 0 {
                continue;
            }
            let name = p.bytes().trim_ascii().to_ascii_uppercase();
            match name.as_slice() {
                b"ENDSEC" | b"EOF" => return,
                b"TABLE" => {
                    // The next pair names the table.
                    if let Some(n) = self.r.peek()
                        && n.code == 2
                    {
                        table = n.bytes().trim_ascii().to_ascii_uppercase();
                    }
                }
                b"ENDTAB" => table.clear(),
                b"LAYER" if table == b"LAYER" => {
                    self.record();
                    self.layer_record();
                }
                b"STYLE" if table == b"STYLE" => {
                    self.record();
                    self.style_record();
                }
                _ => {}
            }
        }
    }

    fn layer_record(&mut self) {
        let (mut name, mut flags, mut aci, mut rgb): (&[u8], i32, i32, Option<u32>) = (b"", 0, 7, None);
        for p in &self.buf {
            match p.code {
                2 => name = p.bytes(),
                70 => flags = p.i32(),
                62 => aci = p.i32(),
                420 => rgb = Some(p.i64() as u32 & 0xFF_FFFF),
                _ => {}
            }
        }
        let id = self.env().layer(name);
        if let Some(l) = self.b.layers.list.get_mut(id as usize) {
            l.frozen = flags & 1 != 0;
            // A negative colour switches the layer off.
            l.off = aci < 0;
            l.color = match rgb {
                Some(c) => Color::True(c),
                None => Color::Aci(aci.unsigned_abs().clamp(1, 255) as u8),
            };
        }
    }

    fn style_record(&mut self) {
        let mut style = tables::Style::default();
        let mut name: &[u8] = b"";
        for p in &self.buf {
            match p.code {
                2 => name = p.bytes(),
                40 => style.height = p.f64(),
                41 => style.width = p.f64(),
                50 => style.oblique = p.f64().to_radians(),
                _ => {}
            }
        }
        self.b.styles.set(name, style);
    }

    fn blocks_section(&mut self) {
        loop {
            // Up to the next BLOCK, or the end of the section.
            let Some(p) = self.r.next() else { return };
            if p.is(0, "ENDSEC") || p.is(0, "EOF") {
                return;
            }
            if !p.is(0, "BLOCK") {
                continue;
            }
            self.record();
            let (mut name, mut alt, mut flags) = (Vec::new(), Vec::new(), 0);
            let mut base = DVec3::ZERO;
            for q in &self.buf {
                if entities::set_axis(&mut base, 10, q.code, q) {
                    continue;
                }
                match q.code {
                    2 => name = q.bytes().to_vec(),
                    3 => alt = q.bytes().to_vec(),
                    70 => flags = q.i32(),
                    _ => {}
                }
            }
            if name.is_empty() {
                name = alt;
            }
            let mut list = Vec::new();
            let done;
            loop {
                match self.read_entity() {
                    Item::Entity(e) => list.push(e),
                    Item::Skipped => {}
                    Item::End(end) => {
                        // ENDBLK is skipped by the search for the next BLOCK;
                        // anything else (a missing ENDBLK, the end of the
                        // section) ends this block as well.
                        done = !end.eq_ignore_ascii_case(b"ENDBLK") && !end.eq_ignore_ascii_case(b"BLOCK");
                        break;
                    }
                }
                if self.b.over_budget {
                    return;
                }
            }
            self.store_block(&name, flags, base, list);
            if done {
                return;
            }
        }
    }

    fn store_block(&mut self, name: &[u8], flags: i32, base: DVec3, entities: Vec<Entity>) {
        let key = emit::block_key(name);
        if key.is_empty() || key.starts_with(b"*PAPER_SPACE") || key == b"*MODEL_SPACE" {
            return;
        }
        // Anonymous dimension blocks (`*D1`) and everything else are
        // looked up by name.  Blocks flagged 4 or 8 are external references.
        self.b.blocks.insert(key, Rc::new(Block { base, entities, xref: flags & 12 != 0 }));
    }

    fn entities_section(&mut self) {
        loop {
            match self.read_entity() {
                Item::Entity(e) => {
                    self.entities += 1;
                    if e.paper {
                        continue;
                    }
                    if matches!(e.kind, Kind::Insert(_) | Kind::Dimension { .. }) {
                        self.deferred.push(e);
                    } else {
                        self.b.emit(&e, &Ctx::ROOT);
                    }
                }
                Item::Skipped => self.entities += 1,
                Item::End(_) => return,
            }
            if self.b.over_budget {
                return;
            }
        }
    }

    //endregion

    /// Reads the next entity, with the vertices of a polyline and the
    /// attributes of a block reference that follow it.
    fn read_entity(&mut self) -> Item {
        loop {
            let Some(p) = self.r.peek() else { return Item::End(Vec::new()) };
            if p.code != 0 {
                // Stray groups between entities.
                self.r.next();
                continue;
            }
            let name = p.bytes().trim_ascii().to_ascii_uppercase();
            match name.as_slice() {
                // The end of the run, left for the caller.
                b"ENDSEC" | b"ENDBLK" | b"EOF" | b"BLOCK" | b"SECTION" | b"ENDTAB" => return Item::End(name),
                // A stray sub-entity or terminator outside its parent.
                b"SEQEND" | b"VERTEX" => {
                    self.r.next();
                    self.record();
                    continue;
                }
                _ => {}
            }
            self.r.next();
            self.record();
            let parsed = {
                let mut env = Env { dec: self.dec, layers: &mut self.b.layers };
                entities::parse(&name, &self.buf, &mut env)
            };
            let Parsed::Entity(mut e) = parsed else {
                // Quietly pass over what is not geometry.
                if !matches!(name.as_slice(), b"VIEWPORT" | b"OLE2FRAME" | b"TOLERANCE" | b"WIPEOUT") {
                    self.b.note_skipped(&name);
                }
                return Item::Skipped;
            };
            match &mut e.kind {
                Kind::Poly(poly) => {
                    while let Some(q) = self.r.peek() {
                        if q.is(0, "VERTEX") {
                            self.r.next();
                            self.record();
                            if poly.verts.len() < 50_000_000 {
                                poly.verts.push(entities::parse_vertex(&self.buf));
                            }
                        } else if q.is(0, "SEQEND") {
                            self.r.next();
                            self.record();
                            break;
                        } else {
                            break;
                        }
                    }
                }
                Kind::Insert(ins) if ins.has_attribs => {
                    while let Some(q) = self.r.peek() {
                        if q.is(0, "ATTRIB") {
                            self.r.next();
                            self.record();
                            let parsed = {
                                let mut env = Env { dec: self.dec, layers: &mut self.b.layers };
                                entities::parse(b"ATTRIB", &self.buf, &mut env)
                            };
                            if let Parsed::Entity(a) = parsed
                                && ins.attribs.len() < 10_000
                            {
                                ins.attribs.push(a);
                            }
                        } else if q.is(0, "SEQEND") {
                            self.r.next();
                            self.record();
                            break;
                        } else {
                            break;
                        }
                    }
                }
                _ => {}
            }
            return Item::Entity(e);
        }
    }
}
