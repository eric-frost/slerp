//! The container AVIF and HEIF share (ISO base media file format): boxes
//! holding a list of items (the pictures and their metadata), the
//! properties each item has (size, colour, bit depth, rotation, mirroring,
//! cropping, which codec) and where each item's bytes are.  AVIF animations
//! (`avis`) use the movie half of the format instead: a track of samples.
//!
//! This reads only what a viewer needs and checks every length against the
//! file, with limits on the number of items, properties and nesting, so a
//! hostile file costs a refusal rather than memory.

use std::collections::HashMap;

const MAX_ITEMS: usize = 1 << 16;
const MAX_PROPERTIES: usize = 4096;
const MAX_SAMPLES: usize = 1 << 20;

/// A box: its type and where its payload lies.
#[derive(Clone, Copy, Debug)]
struct BoxRef {
    kind: [u8; 4],
    /// The payload, after the header.
    start: usize,
    end: usize,
}

/// Boxes in `d[start..end]`, in order.  Boxes cut off by the end of the
/// enclosing range are clipped to it.
fn boxes(d: &[u8], start: usize, end: usize) -> Vec<BoxRef> {
    let mut out = Vec::new();
    let mut at = start;
    while at + 8 <= end && out.len() < 1 << 16 {
        let mut size = u32::from_be_bytes([d[at], d[at + 1], d[at + 2], d[at + 3]]) as usize;
        let kind = [d[at + 4], d[at + 5], d[at + 6], d[at + 7]];
        let mut header = 8;
        if size == 1 {
            let Some(b) = d.get(at + 8..at + 16) else { break };
            let big = u64::from_be_bytes(b.try_into().unwrap_or([0; 8]));
            size = usize::try_from(big).unwrap_or(usize::MAX);
            header = 16;
        } else if size == 0 {
            size = end - at;
        }
        if &kind == b"uuid" {
            header += 16;
        }
        if size < header {
            break;
        }
        let stop = at.saturating_add(size).min(end);
        out.push(BoxRef { kind, start: (at + header).min(stop), end: stop });
        at = stop;
    }
    out
}

/// A big-endian reader over a slice that fails instead of panicking.
struct Cur<'a> {
    d: &'a [u8],
    at: usize,
}

impl<'a> Cur<'a> {
    fn new(d: &'a [u8]) -> Self {
        Cur { d, at: 0 }
    }

    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let s = self.d.get(self.at..self.at.checked_add(n)?)?;
        self.at += n;
        Some(s)
    }

    fn u8(&mut self) -> Option<u8> {
        Some(self.take(1)?[0])
    }

    fn u16(&mut self) -> Option<u16> {
        Some(u16::from_be_bytes(self.take(2)?.try_into().ok()?))
    }

    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_be_bytes(self.take(4)?.try_into().ok()?))
    }

    fn u64(&mut self) -> Option<u64> {
        Some(u64::from_be_bytes(self.take(8)?.try_into().ok()?))
    }

    /// An unsigned integer of `n` bytes (0, 4 or 8, as `iloc` uses).
    fn uint(&mut self, n: usize) -> Option<u64> {
        match n {
            0 => Some(0),
            1 => self.u8().map(u64::from),
            2 => self.u16().map(u64::from),
            4 => self.u32().map(u64::from),
            8 => self.u64(),
            _ => None,
        }
    }

    /// Version and flags of a full box.
    fn full(&mut self) -> Option<(u8, u32)> {
        let v = self.u32()?;
        Some(((v >> 24) as u8, v & 0xff_ffff))
    }

    fn rest(&self) -> &'a [u8] {
        &self.d[self.at.min(self.d.len())..]
    }

    fn cstring(&mut self) -> Option<String> {
        let rest = self.rest();
        let n = rest.iter().position(|&b| b == 0).unwrap_or(rest.len());
        self.at += (n + 1).min(rest.len());
        Some(String::from_utf8_lossy(&rest[..n]).into_owned())
    }
}

/// What a file's brands say it is.
#[derive(Clone, Debug, Default)]
pub struct Brands {
    pub major: [u8; 4],
    pub compatible: Vec<[u8; 4]>,
}

impl Brands {
    pub fn has(&self, b: &[u8; 4]) -> bool {
        &self.major == b || self.compatible.contains(b)
    }
}

#[derive(Clone, Debug)]
pub struct Item {
    pub id: u32,
    pub kind: [u8; 4],
    pub hidden: bool,
    pub content_type: String,
}

#[derive(Clone, Debug)]
struct Location {
    /// 0: in the file, 1: in the `idat` box.
    method: u8,
    base: u64,
    extents: Vec<(u64, u64)>,
}

/// An item property.
#[derive(Clone, Debug, PartialEq)]
pub enum Property {
    Ispe {
        width: u32,
        height: u32,
    },
    Pixi {
        bits: Vec<u8>,
    },
    Colr(Colour),
    /// The codec configuration of an AV1 item: the four fixed bytes and the
    /// configuration OBUs after them.
    Av1c {
        high_bitdepth: bool,
        twelve_bit: bool,
        monochrome: bool,
        subsampling: (bool, bool),
        obus: Vec<u8>,
    },
    /// A configuration the file's codec uses (HEVC's `hvcC`, ...), unread.
    CodecConfig([u8; 4]),
    Auxc {
        urn: String,
    },
    /// Rotation by this many quarter turns anti-clockwise.
    Irot(u8),
    /// 0 swaps top and bottom, 1 swaps left and right, as libavif and
    /// libheif read the box.
    Imir(u8),
    Clap {
        width: (u32, u32),
        height: (u32, u32),
        h_off: (i32, u32),
        v_off: (i32, u32),
    },
    /// The operating point of a scalable AV1 stream the item is meant to be
    /// shown at (`a1op`).
    A1op(u8),
    Other([u8; 4]),
}

#[derive(Clone, Debug, PartialEq)]
pub enum Colour {
    /// Primaries, transfer, matrix coefficients and full-range flag.
    Nclx {
        primaries: u16,
        transfer: u16,
        matrix: u16,
        full_range: bool,
    },
    Icc(Vec<u8>),
}

/// A track of an image sequence.
#[derive(Clone, Debug, Default)]
pub struct Track {
    pub id: u32,
    pub handler: [u8; 4],
    pub timescale: u32,
    /// Duration in the media timescale.
    pub duration: u64,
    /// Where each sample is: offset and size.
    pub samples: Vec<(u64, u32)>,
    /// Each sample's duration in media timescale units.
    pub deltas: Vec<u32>,
    /// Sync samples, from 0; empty means every sample is one.
    pub sync: Vec<u32>,
    pub config: Option<Property>,
    pub colour: Option<Colour>,
    /// The track this one is an alpha plane of.
    pub alpha_of: Option<u32>,
    /// The auxiliary type from the sample entry (`auxi`), when present.
    pub aux_type: String,
    /// The edit list: segment duration (movie timescale) of the first entry
    /// and whether it repeats.
    pub edit: Option<(u64, bool)>,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Default)]
pub struct Heif {
    pub brands: Brands,
    pub items: Vec<Item>,
    pub primary: Option<u32>,
    locations: HashMap<u32, Location>,
    properties: Vec<Property>,
    /// Item id to the 1-based property indices associated with it, in order.
    associations: HashMap<u32, Vec<(u16, bool)>>,
    /// Reference type, from item, to items.
    refs: Vec<([u8; 4], u32, Vec<u32>)>,
    idat: (usize, usize),
    pub tracks: Vec<Track>,
    /// The movie's timescale.
    pub timescale: u32,
    pub warnings: Vec<String>,
}

/// Whether a file's first bytes are an ISO base media file with image
/// brands, and which family: AVIF brands, or HEIF's.
pub fn brands_of(head: &[u8]) -> Option<Brands> {
    if head.len() < 16 || &head[4..8] != b"ftyp" {
        return None;
    }
    let size = u32::from_be_bytes([head[0], head[1], head[2], head[3]]) as usize;
    let end = size.clamp(16, head.len());
    let major = [head[8], head[9], head[10], head[11]];
    let compatible = head[16..end].chunks_exact(4).map(|c| [c[0], c[1], c[2], c[3]]).collect();
    Some(Brands { major, compatible })
}

impl Heif {
    pub fn parse(d: &[u8]) -> Result<Heif, String> {
        let brands = brands_of(d.get(..d.len().min(4096)).unwrap_or(d)).ok_or("this isn't an HEIF or AVIF file")?;
        let mut h = Heif { brands, ..Heif::default() };
        let top = boxes(d, 0, d.len());
        for b in &top {
            match &b.kind {
                b"meta" => h.parse_meta(d, b)?,
                b"moov" => h.parse_moov(d, b),
                _ => {}
            }
        }
        if h.items.is_empty() && h.tracks.is_empty() {
            return Err("the file holds no pictures".into());
        }
        Ok(h)
    }

    /*┌─────────────────╮
    | Items           |
    └─────────────────╯*/
    //region Items

    fn parse_meta(&mut self, d: &[u8], meta: &BoxRef) -> Result<(), String> {
        // `meta` is a full box: four bytes of version and flags first.
        let start = meta.start + 4;
        for b in boxes(d, start.min(meta.end), meta.end) {
            let body = &d[b.start..b.end];
            match &b.kind {
                b"pitm" => {
                    let mut c = Cur::new(body);
                    let (v, _) = c.full().ok_or("a short pitm box")?;
                    self.primary = Some(if v == 0 {
                        c.u16().ok_or("a short pitm box")? as u32
                    } else {
                        c.u32().ok_or("a short pitm box")?
                    });
                }
                b"iinf" => self.parse_iinf(d, &b),
                b"iloc" => self.parse_iloc(body),
                b"iref" => self.parse_iref(d, &b),
                b"iprp" => self.parse_iprp(d, &b),
                b"idat" => self.idat = (b.start, b.end),
                _ => {}
            }
        }
        Ok(())
    }

    fn parse_iinf(&mut self, d: &[u8], iinf: &BoxRef) {
        let mut c = Cur::new(&d[iinf.start..iinf.end]);
        let Some((v, _)) = c.full() else { return };
        if (if v == 0 { c.u16().map(u32::from) } else { c.u32() }).is_none() {
            return;
        }
        let first = iinf.start + c.at;
        for b in boxes(d, first, iinf.end) {
            if &b.kind != b"infe" || self.items.len() >= MAX_ITEMS {
                continue;
            }
            let mut c = Cur::new(&d[b.start..b.end]);
            let Some((v, flags)) = c.full() else { continue };
            if v < 2 {
                continue;
            }
            let id = if v == 2 { c.u16().map(u32::from) } else { c.u32() };
            let (Some(id), Some(_protection), Some(kind)) = (id, c.u16(), c.take(4)) else { continue };
            let kind: [u8; 4] = kind.try_into().unwrap_or([0; 4]);
            let _name = c.cstring();
            let content_type = if &kind == b"mime" { c.cstring().unwrap_or_default() } else { String::new() };
            self.items.push(Item { id, kind, hidden: flags & 1 != 0, content_type });
        }
    }

    fn parse_iloc(&mut self, body: &[u8]) {
        let mut c = Cur::new(body);
        let Some((v, _)) = c.full() else { return };
        let (Some(a), Some(b)) = (c.u8(), c.u8()) else { return };
        let (offset_size, length_size) = ((a >> 4) as usize, (a & 15) as usize);
        let (base_size, index_size) = ((b >> 4) as usize, if v == 1 || v == 2 { (b & 15) as usize } else { 0 });
        let count = if v < 2 { c.u16().map(u32::from) } else { c.u32() };
        let Some(count) = count else { return };
        for _ in 0..count.min(MAX_ITEMS as u32) {
            let id = if v < 2 { c.u16().map(u32::from) } else { c.u32() };
            let method = if v == 1 || v == 2 { c.u16().map(|m| (m & 15) as u8) } else { Some(0) };
            let (Some(id), Some(method), Some(_dref)) = (id, method, c.u16()) else { return };
            let (Some(base), Some(n)) = (c.uint(base_size), c.u16()) else { return };
            let mut extents = Vec::new();
            for _ in 0..n.min(1024) {
                if index_size > 0 && c.uint(index_size).is_none() {
                    return;
                }
                let (Some(off), Some(len)) = (c.uint(offset_size), c.uint(length_size)) else { return };
                extents.push((off, len));
            }
            self.locations.insert(id, Location { method, base, extents });
        }
    }

    fn parse_iref(&mut self, d: &[u8], iref: &BoxRef) {
        let mut c = Cur::new(&d[iref.start..iref.end]);
        let Some((v, _)) = c.full() else { return };
        let first = iref.start + c.at;
        for b in boxes(d, first, iref.end) {
            let mut c = Cur::new(&d[b.start..b.end]);
            let from = if v == 0 { c.u16().map(u32::from) } else { c.u32() };
            let (Some(from), Some(n)) = (from, c.u16()) else { continue };
            let mut to = Vec::new();
            for _ in 0..n.min(4096) {
                match if v == 0 { c.u16().map(u32::from) } else { c.u32() } {
                    Some(t) => to.push(t),
                    None => break,
                }
            }
            if self.refs.len() < 1 << 16 {
                self.refs.push((b.kind, from, to));
            }
        }
    }

    fn parse_iprp(&mut self, d: &[u8], iprp: &BoxRef) {
        for b in boxes(d, iprp.start, iprp.end) {
            match &b.kind {
                b"ipco" => {
                    for p in boxes(d, b.start, b.end).into_iter().take(MAX_PROPERTIES) {
                        self.properties.push(parse_property(&d[p.start..p.end], p.kind));
                    }
                }
                b"ipma" => {
                    let mut c = Cur::new(&d[b.start..b.end]);
                    let Some((v, flags)) = c.full() else { continue };
                    let Some(n) = c.u32() else { continue };
                    for _ in 0..n.min(MAX_ITEMS as u32) {
                        let id = if v < 1 { c.u16().map(u32::from) } else { c.u32() };
                        let (Some(id), Some(count)) = (id, c.u8()) else { break };
                        let list = self.associations.entry(id).or_default();
                        for _ in 0..count {
                            let (essential, index) = if flags & 1 != 0 {
                                let Some(v) = c.u16() else { break };
                                (v & 0x8000 != 0, v & 0x7fff)
                            } else {
                                let Some(v) = c.u8() else { break };
                                (v & 0x80 != 0, (v & 0x7f) as u16)
                            };
                            list.push((index, essential));
                        }
                    }
                }
                _ => {}
            }
        }
    }

    /// The properties of item `id`, in the order the file lists them.
    pub fn properties_of(&self, id: u32) -> Vec<&Property> {
        self.associations
            .get(&id)
            .map(|l| l.iter().filter_map(|&(i, _)| self.properties.get((i as usize).checked_sub(1)?)).collect())
            .unwrap_or_default()
    }

    pub fn item(&self, id: u32) -> Option<&Item> {
        self.items.iter().find(|i| i.id == id)
    }

    /// Items that `kind` references from `from`, in order.
    pub fn refs_from(&self, kind: &[u8; 4], from: u32) -> Vec<u32> {
        self.refs.iter().filter(|(k, f, _)| k == kind && *f == from).flat_map(|(_, _, t)| t.iter().copied()).collect()
    }

    /// Items that reference `to` with `kind`.
    pub fn refs_to(&self, kind: &[u8; 4], to: u32) -> Vec<u32> {
        self.refs.iter().filter(|(k, _, t)| k == kind && t.contains(&to)).map(|(_, f, _)| *f).collect()
    }

    /// A text listing of the file's items, properties and references, for
    /// debugging and the `heif_dump` example.
    pub fn describe(&self) -> String {
        use std::fmt::Write;
        let mut out = String::new();
        let _ = writeln!(
            out,
            "brands: {:?} {:?}",
            String::from_utf8_lossy(&self.brands.major),
            self.brands.compatible.iter().map(|b| String::from_utf8_lossy(b).to_string()).collect::<Vec<_>>()
        );
        let _ = writeln!(out, "primary: {:?}", self.primary);
        for i in &self.items {
            let _ = writeln!(
                out,
                "item {} {} hidden={} {:?}",
                i.id,
                String::from_utf8_lossy(&i.kind),
                i.hidden,
                self.properties_of(i.id)
            );
        }
        for (k, f, t) in &self.refs {
            let _ = writeln!(out, "ref {} {} -> {:?}", String::from_utf8_lossy(k), f, t);
        }
        for t in &self.tracks {
            let _ = writeln!(
                out,
                "track {} handler={} {}x{} samples={} timescale={} alpha_of={:?} aux={:?} edit={:?}",
                t.id,
                String::from_utf8_lossy(&t.handler),
                t.width,
                t.height,
                t.samples.len(),
                t.timescale,
                t.alpha_of,
                t.aux_type,
                t.edit
            );
        }
        out
    }

    /// The item holding the alpha plane of `id`, if there is one.
    pub fn alpha_of(&self, id: u32) -> Option<u32> {
        self.refs_to(b"auxl", id)
            .into_iter()
            .find(|&a| self.properties_of(a).iter().any(|p| matches!(p, Property::Auxc { urn } if is_alpha_urn(urn))))
    }

    /// Whether the alpha plane of `id` is premultiplied into the colour.
    pub fn alpha_is_premultiplied(&self, id: u32) -> bool {
        self.alpha_of(id).is_some_and(|a| self.refs_from(b"prem", a).contains(&id))
    }

    /// An item's bytes: its extents from the file or from the `idat` box.
    pub fn item_data(&self, d: &[u8], id: u32) -> Result<Vec<u8>, String> {
        let loc = self.locations.get(&id).ok_or("an item has no location")?;
        let mut out = Vec::new();
        for &(off, len) in &loc.extents {
            let start = loc.base.checked_add(off).ok_or("an item's location overflows")?;
            let end = start.checked_add(len).ok_or("an item's location overflows")?;
            let (lo, hi) = match loc.method {
                0 => (start, end),
                1 => (
                    (self.idat.0 as u64).checked_add(start).ok_or("an item's location overflows")?,
                    (self.idat.0 as u64).checked_add(end).ok_or("an item's location overflows")?,
                ),
                m => return Err(format!("item data by construction method {m} isn't supported")),
            };
            // A length of 0 means the rest of the file (or box).
            let hi = if len == 0 && loc.method == 0 { d.len() as u64 } else { hi };
            let slice = usize::try_from(lo)
                .ok()
                .zip(usize::try_from(hi).ok())
                .and_then(|(lo, hi)| d.get(lo..hi))
                .ok_or("an item's data lies outside the file")?;
            if out.len() + slice.len() > d.len() {
                return Err("an item's data is larger than the file".into());
            }
            out.extend_from_slice(slice);
        }
        Ok(out)
    }

    //endregion

    /*┌─────────────────╮
    | Sequences       |
    └─────────────────╯*/
    //region Sequences

    fn parse_moov(&mut self, d: &[u8], moov: &BoxRef) {
        for b in boxes(d, moov.start, moov.end) {
            match &b.kind {
                b"mvhd" => {
                    let mut c = Cur::new(&d[b.start..b.end]);
                    if let Some((v, _)) = c.full() {
                        let _ = if v == 1 { c.take(16) } else { c.take(8) };
                        self.timescale = c.u32().unwrap_or(0);
                    }
                }
                b"trak" => {
                    if self.tracks.len() < 16
                        && let Some(t) = parse_track(d, &b)
                    {
                        self.tracks.push(t);
                    }
                }
                _ => {}
            }
        }
    }

    /// The track of colour pictures, and the track of its alpha plane.
    pub fn picture_track(&self) -> Option<(&Track, Option<&Track>)> {
        let colour =
            self.tracks.iter().find(|t| &t.handler == b"pict" && t.alpha_of.is_none() && !t.samples.is_empty())?;
        // Other auxiliary tracks (a depth map) name the colour track the same way.
        let alpha = self.tracks.iter().find(|t| {
            t.alpha_of == Some(colour.id)
                && !t.samples.is_empty()
                && (t.aux_type.is_empty() || is_alpha_urn(&t.aux_type))
        });
        Some((colour, alpha))
    }

    //endregion
}

fn parse_property(b: &[u8], kind: [u8; 4]) -> Property {
    let mut c = Cur::new(b);
    let parsed = (|| -> Option<Property> {
        Some(match &kind {
            b"ispe" => {
                c.full()?;
                Property::Ispe { width: c.u32()?, height: c.u32()? }
            }
            b"pixi" => {
                c.full()?;
                let n = c.u8()? as usize;
                Property::Pixi { bits: c.take(n)?.to_vec() }
            }
            b"colr" => {
                let t = c.take(4)?;
                match t {
                    b"nclx" => Property::Colr(Colour::Nclx {
                        primaries: c.u16()?,
                        transfer: c.u16()?,
                        matrix: c.u16()?,
                        full_range: c.u8()? & 0x80 != 0,
                    }),
                    b"rICC" | b"prof" => Property::Colr(Colour::Icc(c.rest().to_vec())),
                    _ => Property::Other(kind),
                }
            }
            b"av1C" => {
                let b0 = c.u8()?;
                if b0 != 0x81 {
                    return Some(Property::Other(kind));
                }
                let _profile = c.u8()?;
                let flags = c.u8()?;
                let _delay = c.u8()?;
                Property::Av1c {
                    high_bitdepth: flags & 0x40 != 0,
                    twelve_bit: flags & 0x20 != 0,
                    monochrome: flags & 0x10 != 0,
                    subsampling: (flags & 0x08 != 0, flags & 0x04 != 0),
                    obus: c.rest().to_vec(),
                }
            }
            b"hvcC" | b"vvcC" | b"jpgC" | b"uncC" | b"avcC" => Property::CodecConfig(kind),
            b"auxC" => {
                c.full()?;
                Property::Auxc { urn: c.cstring()? }
            }
            b"a1op" => Property::A1op(c.u8()?),
            b"irot" => Property::Irot(c.u8()? & 3),
            b"imir" => Property::Imir(c.u8()? & 1),
            b"clap" => {
                let (wn, wd, hn, hd) = (c.u32()?, c.u32()?, c.u32()?, c.u32()?);
                let (hon, hod, von, vod) = (c.u32()? as i32, c.u32()?, c.u32()? as i32, c.u32()?);
                Property::Clap { width: (wn, wd), height: (hn, hd), h_off: (hon, hod), v_off: (von, vod) }
            }
            _ => Property::Other(kind),
        })
    })();
    parsed.unwrap_or(Property::Other(kind))
}

pub fn is_alpha_urn(urn: &str) -> bool {
    urn.contains("alpha") || urn.ends_with(":auxid:1")
}

fn parse_track(d: &[u8], trak: &BoxRef) -> Option<Track> {
    let mut t = Track::default();
    let mut stsc = Vec::new();
    let mut chunk_offsets = Vec::new();
    let mut sizes: Vec<u32> = Vec::new();
    let mut fixed_size = 0u32;
    let mut sample_count = 0usize;
    let walk = |kind: &[u8; 4], from: &BoxRef| boxes(d, from.start, from.end).into_iter().find(|b| &b.kind == kind);
    for b in boxes(d, trak.start, trak.end) {
        match &b.kind {
            b"tkhd" => {
                let mut c = Cur::new(&d[b.start..b.end]);
                let (v, _) = c.full()?;
                let _ = if v == 1 { c.take(16) } else { c.take(8) };
                t.id = c.u32()?;
                // Width and height are the last two 16.16 numbers.
                let tail = &d[b.start..b.end];
                if tail.len() >= 8 {
                    let n = tail.len();
                    t.width = u32::from_be_bytes(tail[n - 8..n - 4].try_into().ok()?) >> 16;
                    t.height = u32::from_be_bytes(tail[n - 4..].try_into().ok()?) >> 16;
                }
            }
            b"tref" => {
                for r in boxes(d, b.start, b.end) {
                    if &r.kind == b"auxl" && r.end >= r.start + 4 {
                        t.alpha_of = Some(u32::from_be_bytes(d[r.start..r.start + 4].try_into().ok()?));
                    }
                }
            }
            b"edts" => {
                if let Some(elst) = walk(b"elst", &b) {
                    let mut c = Cur::new(&d[elst.start..elst.end]);
                    let (v, flags) = c.full()?;
                    if c.u32()? > 0 {
                        let seg = if v == 1 { c.u64()? } else { c.u32()? as u64 };
                        t.edit = Some((seg, flags & 1 != 0));
                    }
                }
            }
            b"mdia" => {
                for m in boxes(d, b.start, b.end) {
                    match &m.kind {
                        b"mdhd" => {
                            let mut c = Cur::new(&d[m.start..m.end]);
                            let (v, _) = c.full()?;
                            let _ = if v == 1 { c.take(16) } else { c.take(8) };
                            t.timescale = c.u32()?;
                            t.duration = if v == 1 { c.u64()? } else { c.u32()? as u64 };
                        }
                        b"hdlr" => {
                            let mut c = Cur::new(&d[m.start..m.end]);
                            c.full()?;
                            c.u32()?;
                            t.handler = c.take(4)?.try_into().ok()?;
                        }
                        b"minf" => {
                            let stbl = walk(b"stbl", &m)?;
                            for s in boxes(d, stbl.start, stbl.end) {
                                let mut c = Cur::new(&d[s.start..s.end]);
                                match &s.kind {
                                    b"stsd" => {
                                        c.full()?;
                                        c.u32()?;
                                        // The first sample entry: a box with 78
                                        // bytes of visual fields, then children.
                                        let entry = boxes(d, s.start + 8, s.end).into_iter().next()?;
                                        let body = entry.start + 78;
                                        for p in boxes(d, body.min(entry.end), entry.end) {
                                            match &p.kind {
                                                b"av1C" | b"hvcC" | b"colr" => {
                                                    match parse_property(&d[p.start..p.end], p.kind) {
                                                        Property::Colr(c) => t.colour = Some(c),
                                                        cfg => t.config = Some(cfg),
                                                    }
                                                }
                                                b"auxi" => {
                                                    let mut ac = Cur::new(&d[p.start..p.end]);
                                                    ac.full()?;
                                                    t.aux_type = ac.cstring().unwrap_or_default();
                                                }
                                                _ => {}
                                            }
                                        }
                                    }
                                    b"stts" => {
                                        c.full()?;
                                        let n = c.u32()?;
                                        for _ in 0..n.min(1 << 20) {
                                            let (count, delta) = (c.u32()?, c.u32()?);
                                            for _ in 0..count.min(MAX_SAMPLES as u32) {
                                                if t.deltas.len() >= MAX_SAMPLES {
                                                    break;
                                                }
                                                t.deltas.push(delta);
                                            }
                                        }
                                    }
                                    b"stss" => {
                                        c.full()?;
                                        let n = c.u32()?;
                                        for _ in 0..n.min(1 << 20) {
                                            t.sync.push(c.u32()?.saturating_sub(1));
                                        }
                                    }
                                    b"stsz" => {
                                        c.full()?;
                                        fixed_size = c.u32()?;
                                        sample_count = c.u32()? as usize;
                                        if sample_count > MAX_SAMPLES {
                                            return None;
                                        }
                                        if fixed_size == 0 {
                                            for _ in 0..sample_count {
                                                sizes.push(c.u32()?);
                                            }
                                        }
                                    }
                                    b"stsc" => {
                                        c.full()?;
                                        let n = c.u32()?;
                                        for _ in 0..n.min(1 << 20) {
                                            stsc.push((c.u32()?, c.u32()?, c.u32()?));
                                        }
                                    }
                                    b"stco" | b"co64" => {
                                        c.full()?;
                                        let n = c.u32()?;
                                        for _ in 0..n.min(1 << 20) {
                                            chunk_offsets.push(if &s.kind == b"co64" {
                                                c.u64()?
                                            } else {
                                                c.u32()? as u64
                                            });
                                        }
                                    }
                                    _ => {}
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
    // Where each sample is: samples fill chunks as the sample-to-chunk table says.
    let size_of = |i: usize| if fixed_size != 0 { fixed_size } else { sizes.get(i).copied().unwrap_or(0) };
    let mut sample = 0usize;
    for (ci, &off) in chunk_offsets.iter().enumerate() {
        let chunk_no = ci as u32 + 1;
        let per = stsc.iter().rev().find(|(first, _, _)| *first <= chunk_no).map_or(1, |e| e.1) as usize;
        let mut at = off;
        for _ in 0..per {
            if sample >= sample_count {
                break;
            }
            let size = size_of(sample);
            t.samples.push((at, size));
            at = at.checked_add(size as u64)?;
            sample += 1;
        }
    }
    t.deltas.resize(t.samples.len(), t.deltas.last().copied().unwrap_or(0));
    Some(t)
}

/// A rational as the `clap` box stores it, as a float.
pub fn ratio(n: u32, d: u32) -> f64 {
    n as f64 / d.max(1) as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bx(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut b = ((body.len() + 8) as u32).to_be_bytes().to_vec();
        b.extend(kind);
        b.extend(body);
        b
    }

    fn full(kind: &[u8; 4], version: u8, flags: u32, body: &[u8]) -> Vec<u8> {
        let mut b = vec![version];
        b.extend(&flags.to_be_bytes()[1..]);
        b.extend(body);
        bx(kind, &b)
    }

    fn infe(id: u16, kind: &[u8; 4], hidden: bool) -> Vec<u8> {
        let mut b = id.to_be_bytes().to_vec();
        b.extend([0, 0]);
        b.extend(kind);
        b.push(0);
        full(b"infe", 2, hidden as u32, &b)
    }

    /// A still-image file: item 1 is the primary `av01` image, item 2 its
    /// alpha plane, item 3 hidden.  The image data of item 1 is "PIXELS".
    fn sample() -> Vec<u8> {
        let mut ftyp = b"avif".to_vec();
        ftyp.extend([0, 0, 0, 0]);
        ftyp.extend(b"avifmif1");
        let mut out = bx(b"ftyp", &ftyp);
        let hdlr = full(b"hdlr", 0, 0, &[0, 0, 0, 0, b'p', b'i', b'c', b't', 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        let pitm = full(b"pitm", 0, 0, &1u16.to_be_bytes());
        let mut iinf_body = 3u16.to_be_bytes().to_vec();
        iinf_body.extend(infe(1, b"av01", false));
        iinf_body.extend(infe(2, b"av01", false));
        iinf_body.extend(infe(3, b"Exif", true));
        let iinf = full(b"iinf", 0, 0, &iinf_body);
        // iloc version 0: offsets and lengths of 4 bytes, no base offset.
        let mut iloc_body = vec![0x44, 0x00];
        iloc_body.extend(2u16.to_be_bytes());
        let meta_len_guess = 400u32;
        for (id, off, len) in [(1u16, meta_len_guess, 6u32), (2, meta_len_guess + 6, 3)] {
            iloc_body.extend(id.to_be_bytes());
            iloc_body.extend(0u16.to_be_bytes());
            iloc_body.extend(1u16.to_be_bytes());
            iloc_body.extend(off.to_be_bytes());
            iloc_body.extend(len.to_be_bytes());
        }
        let iloc = full(b"iloc", 0, 0, &iloc_body);
        let mut iref_body = Vec::new();
        let mut auxl = 2u16.to_be_bytes().to_vec();
        auxl.extend(1u16.to_be_bytes());
        auxl.extend(1u16.to_be_bytes());
        iref_body.extend(bx(b"auxl", &auxl));
        let iref = full(b"iref", 0, 0, &iref_body);
        // Properties: 1 ispe, 2 colr nclx, 3 irot, 4 auxC alpha, 5 av1C.
        let mut ipco = Vec::new();
        let mut ispe = vec![0, 0, 0, 0];
        ispe.extend(64u32.to_be_bytes());
        ispe.extend(48u32.to_be_bytes());
        ipco.extend(bx(b"ispe", &ispe));
        let mut colr = b"nclx".to_vec();
        colr.extend(12u16.to_be_bytes());
        colr.extend(16u16.to_be_bytes());
        colr.extend(9u16.to_be_bytes());
        colr.push(0x80);
        ipco.extend(bx(b"colr", &colr));
        ipco.extend(bx(b"irot", &[1]));
        let mut auxc = vec![0, 0, 0, 0];
        auxc.extend(b"urn:mpeg:mpegB:cicp:systems:auxiliary:alpha\0");
        ipco.extend(bx(b"auxC", &auxc));
        ipco.extend(bx(b"av1C", &[0x81, 0, 0x4c, 0, 0xaa, 0xbb]));
        let ipco = bx(b"ipco", &ipco);
        let mut ipma_body = 2u32.to_be_bytes().to_vec();
        ipma_body.extend(1u16.to_be_bytes());
        ipma_body.extend([4, 1, 0x82, 3, 0x85]);
        ipma_body.extend(2u16.to_be_bytes());
        ipma_body.extend([2, 4, 5]);
        let ipma = full(b"ipma", 0, 0, &ipma_body);
        let iprp = bx(b"iprp", &[ipco, ipma].concat());
        let meta = full(b"meta", 0, 0, &[hdlr, pitm, iinf, iloc, iref, iprp].concat());
        out.extend(&meta);
        // Pad so that the data starts where iloc says.
        out.resize(meta_len_guess as usize, 0);
        out.extend(b"PIXELSabc");
        out
    }

    #[test]
    fn items_properties_and_data() {
        let d = sample();
        let h = Heif::parse(&d).unwrap();
        assert!(h.brands.has(b"avif") && h.brands.has(b"mif1") && !h.brands.has(b"heic"));
        assert_eq!(h.primary, Some(1));
        assert_eq!(h.items.len(), 3);
        assert!(h.item(3).unwrap().hidden);
        assert_eq!(h.item_data(&d, 1).unwrap(), b"PIXELS");
        assert_eq!(h.item_data(&d, 2).unwrap(), b"abc");
        let props = h.properties_of(1);
        assert_eq!(props[0], &Property::Ispe { width: 64, height: 48 });
        assert!(matches!(
            props[1],
            Property::Colr(Colour::Nclx { primaries: 12, transfer: 16, matrix: 9, full_range: true })
        ));
        assert_eq!(props[2], &Property::Irot(1));
        assert!(matches!(props[3], Property::Av1c { obus, .. } if obus == &[0xaa, 0xbb]));
        assert_eq!(h.alpha_of(1), Some(2));
        assert!(!h.alpha_is_premultiplied(1));
        assert!(h.item_data(&d, 9).is_err());
    }

    #[test]
    fn every_cut_and_flip_is_survived() {
        let d = sample();
        for cut in 0..d.len() {
            if let Ok(h) = Heif::parse(&d[..cut]) {
                for i in &h.items {
                    let _ = h.item_data(&d[..cut], i.id);
                    let _ = h.properties_of(i.id);
                }
            }
        }
        for at in 0..400 {
            let mut bad = d.clone();
            bad[at] ^= 0xff;
            if let Ok(h) = Heif::parse(&bad) {
                for i in &h.items {
                    let _ = h.item_data(&bad, i.id);
                }
            }
        }
        assert!(Heif::parse(b"").is_err());
        assert!(Heif::parse(b"not an image file at all").is_err());
        // A box claiming to be bigger than the file.
        let mut big = bx(b"ftyp", b"avif\0\0\0\0avif");
        big.extend([0xff, 0xff, 0xff, 0xff]);
        big.extend(b"meta");
        assert!(Heif::parse(&big).is_err());
    }

    #[test]
    fn brands() {
        let b = brands_of(b"\0\0\0\x18ftypheic\0\0\0\0mif1heic").unwrap();
        assert!(b.has(b"heic") && b.has(b"mif1"));
        assert!(brands_of(b"\0\0\0\x08ftyp").is_none());
        assert!(brands_of(b"GIF89a......").is_none());
    }
}
