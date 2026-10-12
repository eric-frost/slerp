//! Minecraft's Named Binary Tag format (NBT), as Java Edition writes it:
//! big-endian, usually compressed with gzip (sometimes zlib, sometimes not
//! at all), a tree of typed values under one named compound.
//!
//! The reader keeps arrays as slices of the decompressed bytes and strings
//! borrowed where they are valid UTF-8, so a schematic's block data is
//! never copied.  Nesting is limited to 512 levels as in Minecraft, every
//! length is checked against the bytes left before anything is allocated
//! for it, the number of values kept is capped, and subtrees nobody reads
//! (entities, block entities, biomes) are walked without being kept.

use slerp_scene::LoadError;
use std::borrow::Cow;

type Result<T> = std::result::Result<T, LoadError>;

/// Deeper nesting than Minecraft itself accepts.
const MAX_DEPTH: usize = 512;

/// Values kept for one file.  Lists of small values take about 32 bytes of
/// memory per value, so this keeps a hostile file to a few hundred
/// megabytes; real schematics keep their bulk in arrays, which are slices.
const MAX_TAGS: usize = 6 << 20;

/*┌─────────────────╮
| Decompression   |
└─────────────────╯*/
//region Decompression

/// The NBT bytes of a file: gzip and zlib streams are inflated (to at most
/// `max` bytes, so a small file can't claim gigabytes), anything else is
/// taken as uncompressed.
pub fn decompress(data: &[u8], max: usize) -> Result<Cow<'_, [u8]>> {
    let inflate = |r: std::result::Result<Vec<u8>, miniz_oxide::inflate::DecompressError>| match r {
        Ok(v) => Ok(Cow::Owned(v)),
        Err(e) if e.status == miniz_oxide::inflate::TINFLStatus::HasMoreOutput => {
            Err(LoadError::TooLarge(format!("more than {} MB of NBT data", max >> 20)))
        }
        Err(e) => Err(LoadError::invalid(format!("the compressed data is damaged ({:?})", e.status))),
    };
    if data.starts_with(&[0x1f, 0x8b]) {
        let start = gzip_payload(data)?;
        return inflate(miniz_oxide::inflate::decompress_to_vec_with_limit(&data[start..], max));
    }
    if is_zlib(data) {
        return inflate(miniz_oxide::inflate::decompress_to_vec_zlib_with_limit(data, max));
    }
    Ok(Cow::Borrowed(data))
}

/// Whether the bytes start with a zlib header (deflate, a valid check sum).
pub fn is_zlib(data: &[u8]) -> bool {
    data.len() >= 2
        && data[0] & 0x0f == 8
        && data[0] >> 4 <= 7
        && (u16::from(data[0]) << 8 | u16::from(data[1])) % 31 == 0
}

/// Where the deflate data of a gzip member starts, after its header and
/// optional fields.
fn gzip_payload(d: &[u8]) -> Result<usize> {
    let bad = || LoadError::invalid("damaged gzip header");
    if d.len() < 10 || d[2] != 8 {
        return Err(bad());
    }
    let flags = d[3];
    let mut at = 10usize;
    if flags & 4 != 0 {
        let len = u16::from_le_bytes([*d.get(at).ok_or_else(bad)?, *d.get(at + 1).ok_or_else(bad)?]) as usize;
        at += 2 + len;
    }
    for flag in [8u8, 16] {
        if flags & flag != 0 {
            let end = d.get(at..).and_then(|r| r.iter().position(|&b| b == 0)).ok_or_else(bad)?;
            at += end + 1;
        }
    }
    if flags & 2 != 0 {
        at += 2;
    }
    if at > d.len() {
        return Err(bad());
    }
    Ok(at)
}

/// The first bytes of the decompressed NBT, as far as `head` (the start of
/// a compressed file) goes; for sniffing.
pub fn decompress_head(head: &[u8], want: usize) -> Vec<u8> {
    use miniz_oxide::inflate::core::{DecompressorOxide, decompress, inflate_flags};
    let (input, flags) = if head.starts_with(&[0x1f, 0x8b]) {
        match gzip_payload(head) {
            Ok(at) => (&head[at..], 0),
            Err(_) => return Vec::new(),
        }
    } else if is_zlib(head) {
        (head, inflate_flags::TINFL_FLAG_PARSE_ZLIB_HEADER)
    } else {
        return head[..head.len().min(want)].to_vec();
    };
    let mut out = vec![0u8; want];
    let mut state = DecompressorOxide::new();
    let flags =
        flags | inflate_flags::TINFL_FLAG_HAS_MORE_INPUT | inflate_flags::TINFL_FLAG_USING_NON_WRAPPING_OUTPUT_BUF;
    let (_, _, written) = decompress(&mut state, input, &mut out, 0, flags);
    out.truncate(written);
    out
}

//endregion

/*┌─────────────────╮
| Values          |
└─────────────────╯*/
//region Values

#[derive(Debug, Clone, PartialEq)]
pub enum Tag<'a> {
    Byte(i8),
    Short(i16),
    Int(i32),
    Long(i64),
    Float(f32),
    Double(f64),
    ByteArray(&'a [u8]),
    String(Cow<'a, str>),
    List(Vec<Tag<'a>>),
    Compound(Compound<'a>),
    /// Big-endian 32-bit integers.
    IntArray(&'a [u8]),
    /// Big-endian 64-bit integers.
    LongArray(&'a [u8]),
}

/// A compound's entries in file order.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Compound<'a>(pub Vec<(Cow<'a, str>, Tag<'a>)>);

impl<'a> Compound<'a> {
    pub fn get(&self, name: &str) -> Option<&Tag<'a>> {
        self.0.iter().find(|(k, _)| k == name).map(|(_, v)| v)
    }

    pub fn compound(&self, name: &str) -> Option<&Compound<'a>> {
        self.get(name)?.as_compound()
    }

    pub fn list(&self, name: &str) -> Option<&[Tag<'a>]> {
        match self.get(name)? {
            Tag::List(l) => Some(l),
            _ => None,
        }
    }

    pub fn int(&self, name: &str) -> Option<i64> {
        self.get(name)?.as_int()
    }

    pub fn str(&self, name: &str) -> Option<&str> {
        match self.get(name)? {
            Tag::String(s) => Some(s),
            _ => None,
        }
    }

    pub fn bytes(&self, name: &str) -> Option<&'a [u8]> {
        match self.get(name)? {
            Tag::ByteArray(b) => Some(b),
            _ => None,
        }
    }

    pub fn has(&self, name: &str) -> bool {
        self.get(name).is_some()
    }
}

impl<'a> Tag<'a> {
    /// Any integer value, widened.
    pub fn as_int(&self) -> Option<i64> {
        match *self {
            Tag::Byte(v) => Some(v as i64),
            Tag::Short(v) => Some(v as i64),
            Tag::Int(v) => Some(v as i64),
            Tag::Long(v) => Some(v),
            _ => None,
        }
    }

    pub fn as_compound(&self) -> Option<&Compound<'a>> {
        match self {
            Tag::Compound(c) => Some(c),
            _ => None,
        }
    }

    /// Three integers stored as a list or an int array, as positions and
    /// sizes are.
    pub fn as_ivec3(&self) -> Option<[i64; 3]> {
        match self {
            Tag::List(l) if l.len() == 3 => Some([l[0].as_int()?, l[1].as_int()?, l[2].as_int()?]),
            Tag::IntArray(b) if b.len() == 12 => Some([0, 1, 2].map(|i| int_at(b, i) as i64)),
            _ => None,
        }
    }
}

/// Element `i` of a big-endian int array's bytes.
pub fn int_at(b: &[u8], i: usize) -> i32 {
    i32::from_be_bytes(b[4 * i..4 * i + 4].try_into().unwrap_or([0; 4]))
}

/// Element `i` of a big-endian long array's bytes.
pub fn long_at(b: &[u8], i: usize) -> u64 {
    u64::from_be_bytes(b[8 * i..8 * i + 8].try_into().unwrap_or([0; 8]))
}

//endregion

/*┌─────────────────╮
| Parsing         |
└─────────────────╯*/
//region Parsing

/// Parses an NBT file's root compound and its name.  Entries named in
/// `skip` (at any depth) are read past without being kept.
pub fn parse<'a>(data: &'a [u8], skip: &[&str]) -> Result<(Cow<'a, str>, Compound<'a>)> {
    let mut p = Parser { data, at: 0, tags: 0, skip };
    if p.u8()? != 10 {
        return Err(LoadError::invalid("NBT data doesn't start with a compound"));
    }
    let name = p.string()?;
    let root = p.compound(0)?;
    Ok((name, root))
}

/// The names of the root compound's entries, as far as `data` goes (it
/// may be the start of a file), and whether the compound ended within it.
/// Nothing is kept, so this is cheap on any file.
pub fn root_keys(data: &[u8]) -> (Vec<String>, bool) {
    let mut p = Parser { data, at: 0, tags: 0, skip: &[] };
    let mut keys = Vec::new();
    let start = p.u8().ok() == Some(10) && p.string().is_ok();
    if !start {
        return (keys, false);
    }
    loop {
        let Ok(kind) = p.u8() else { return (keys, false) };
        if kind == 0 {
            return (keys, true);
        }
        let Ok(name) = p.string() else { return (keys, false) };
        keys.push(name.into_owned());
        if p.skip_value(kind, 1).is_err() || keys.len() > 4096 {
            return (keys, false);
        }
    }
}

/// The int array at `outer.inner` (a compound entry of the root, and an
/// entry in it), found without keeping anything else: `Ok(None)` when the
/// data says it isn't there, `Err(())` when the data (the start of a file)
/// ends before telling.
pub fn root_int_array<'a>(data: &'a [u8], outer: &str, inner: &str) -> std::result::Result<Option<&'a [u8]>, ()> {
    let mut p = Parser { data, at: 0, tags: 0, skip: &[] };
    let e = |_| ();
    if p.u8().map_err(e)? != 10 {
        return Ok(None);
    }
    p.string().map_err(e)?;
    loop {
        let kind = p.u8().map_err(e)?;
        if kind == 0 {
            return Ok(None);
        }
        let name = p.string().map_err(e)?;
        if kind != 10 || name != outer {
            p.skip_value(kind, 1).map_err(e)?;
            continue;
        }
        loop {
            let kind = p.u8().map_err(e)?;
            if kind == 0 {
                return Ok(None);
            }
            let name = p.string().map_err(e)?;
            if kind == 11 && name == inner {
                let n = p.length(4).map_err(e)?;
                return p.take(n * 4).map(Some).map_err(e);
            }
            p.skip_value(kind, 2).map_err(e)?;
        }
    }
}

struct Parser<'a, 's> {
    data: &'a [u8],
    at: usize,
    tags: usize,
    skip: &'s [&'s str],
}

/// The fewest bytes one value of each tag type takes, so a list's length
/// can be checked against what is left before it is read.
fn min_size(kind: u8) -> usize {
    match kind {
        1 => 1,
        2 => 2,
        3 | 5 => 4,
        4 | 6 => 8,
        7 | 11 | 12 => 4,
        8 => 2,
        9 => 5,
        10 => 1,
        _ => 0,
    }
}

impl<'a> Parser<'a, '_> {
    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = self.at.checked_add(n).filter(|&e| e <= self.data.len());
        let end = end.ok_or_else(|| LoadError::invalid("NBT data ends early"))?;
        let s = &self.data[self.at..end];
        self.at = end;
        Ok(s)
    }

    fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }

    fn u16(&mut self) -> Result<u16> {
        Ok(u16::from_be_bytes(self.take(2)?.try_into().unwrap_or([0; 2])))
    }

    fn i32(&mut self) -> Result<i32> {
        Ok(i32::from_be_bytes(self.take(4)?.try_into().unwrap_or([0; 4])))
    }

    fn i64(&mut self) -> Result<i64> {
        Ok(i64::from_be_bytes(self.take(8)?.try_into().unwrap_or([0; 8])))
    }

    fn string(&mut self) -> Result<Cow<'a, str>> {
        let n = self.u16()? as usize;
        Ok(String::from_utf8_lossy(self.take(n)?))
    }

    /// A length that must fit in what is left at `min` bytes per element.
    fn length(&mut self, min: usize) -> Result<usize> {
        let n = self.i32()?;
        let n = usize::try_from(n).map_err(|_| LoadError::invalid("negative NBT length"))?;
        if n > 0 && min == 0 {
            return Err(LoadError::invalid("an NBT list of nothing claims elements"));
        }
        if n.saturating_mul(min) > self.data.len() - self.at {
            return Err(LoadError::invalid("an NBT length runs past the end"));
        }
        Ok(n)
    }

    fn count(&mut self) -> Result<()> {
        self.tags += 1;
        if self.tags > MAX_TAGS {
            return Err(LoadError::TooLarge("more NBT values than a schematic needs".into()));
        }
        Ok(())
    }

    fn compound(&mut self, depth: usize) -> Result<Compound<'a>> {
        if depth > MAX_DEPTH {
            return Err(LoadError::invalid("NBT nested too deeply"));
        }
        let mut entries = Vec::new();
        loop {
            let kind = self.u8()?;
            if kind == 0 {
                return Ok(Compound(entries));
            }
            let name = self.string()?;
            if self.skip.iter().any(|s| *s == name) {
                self.skip_value(kind, depth + 1)?;
                continue;
            }
            let value = self.value(kind, depth + 1)?;
            entries.push((name, value));
        }
    }

    fn value(&mut self, kind: u8, depth: usize) -> Result<Tag<'a>> {
        if depth > MAX_DEPTH {
            return Err(LoadError::invalid("NBT nested too deeply"));
        }
        self.count()?;
        Ok(match kind {
            1 => Tag::Byte(self.u8()? as i8),
            2 => Tag::Short(self.u16()? as i16),
            3 => Tag::Int(self.i32()?),
            4 => Tag::Long(self.i64()?),
            5 => Tag::Float(f32::from_bits(self.i32()? as u32)),
            6 => Tag::Double(f64::from_bits(self.i64()? as u64)),
            7 => {
                let n = self.length(1)?;
                Tag::ByteArray(self.take(n)?)
            }
            8 => Tag::String(self.string()?),
            9 => {
                let element = self.u8()?;
                if element > 12 {
                    return Err(LoadError::invalid(format!("NBT list of unknown type {element}")));
                }
                let n = self.length(min_size(element))?;
                let mut list = Vec::with_capacity(n.min(1 << 16));
                for _ in 0..n {
                    list.push(self.value(element, depth + 1)?);
                }
                Tag::List(list)
            }
            10 => Tag::Compound(self.compound(depth)?),
            11 => {
                let n = self.length(4)?;
                Tag::IntArray(self.take(n * 4)?)
            }
            12 => {
                let n = self.length(8)?;
                Tag::LongArray(self.take(n * 8)?)
            }
            _ => return Err(LoadError::invalid(format!("unknown NBT tag type {kind}"))),
        })
    }

    /// Reads past a value without keeping it.
    fn skip_value(&mut self, kind: u8, depth: usize) -> Result<()> {
        if depth > MAX_DEPTH {
            return Err(LoadError::invalid("NBT nested too deeply"));
        }
        match kind {
            1..=6 => {
                self.take(min_size(kind))?;
            }
            7 => {
                let n = self.length(1)?;
                self.take(n)?;
            }
            8 => {
                let n = self.u16()? as usize;
                self.take(n)?;
            }
            9 => {
                let element = self.u8()?;
                if element > 12 {
                    return Err(LoadError::invalid(format!("NBT list of unknown type {element}")));
                }
                let n = self.length(min_size(element))?;
                match element {
                    1..=6 => {
                        self.take(n * min_size(element))?;
                    }
                    _ => {
                        for _ in 0..n {
                            self.skip_value(element, depth + 1)?;
                        }
                    }
                }
            }
            10 => loop {
                let k = self.u8()?;
                if k == 0 {
                    break;
                }
                let n = self.u16()? as usize;
                self.take(n)?;
                self.skip_value(k, depth + 1)?;
            },
            11 => {
                let n = self.length(4)?;
                self.take(n * 4)?;
            }
            12 => {
                let n = self.length(8)?;
                self.take(n * 8)?;
            }
            _ => return Err(LoadError::invalid(format!("unknown NBT tag type {kind}"))),
        }
        Ok(())
    }
}

//endregion

/*┌─────────────────╮
| Writing         |
└─────────────────╯*/
//region Writing

/// Writes NBT, for the tests and the sample generator's checks.
#[cfg(test)]
pub mod write {
    use super::*;

    pub fn root(name: &str, c: &Compound) -> Vec<u8> {
        let mut out = vec![10];
        string(&mut out, name);
        compound(&mut out, c);
        out
    }

    fn string(out: &mut Vec<u8>, s: &str) {
        out.extend_from_slice(&(s.len() as u16).to_be_bytes());
        out.extend_from_slice(s.as_bytes());
    }

    fn compound(out: &mut Vec<u8>, c: &Compound) {
        for (k, v) in &c.0 {
            out.push(kind(v));
            string(out, k);
            value(out, v);
        }
        out.push(0);
    }

    pub fn kind(v: &Tag) -> u8 {
        match v {
            Tag::Byte(_) => 1,
            Tag::Short(_) => 2,
            Tag::Int(_) => 3,
            Tag::Long(_) => 4,
            Tag::Float(_) => 5,
            Tag::Double(_) => 6,
            Tag::ByteArray(_) => 7,
            Tag::String(_) => 8,
            Tag::List(_) => 9,
            Tag::Compound(_) => 10,
            Tag::IntArray(_) => 11,
            Tag::LongArray(_) => 12,
        }
    }

    fn value(out: &mut Vec<u8>, v: &Tag) {
        match v {
            Tag::Byte(b) => out.push(*b as u8),
            Tag::Short(s) => out.extend_from_slice(&s.to_be_bytes()),
            Tag::Int(i) => out.extend_from_slice(&i.to_be_bytes()),
            Tag::Long(l) => out.extend_from_slice(&l.to_be_bytes()),
            Tag::Float(f) => out.extend_from_slice(&f.to_be_bytes()),
            Tag::Double(d) => out.extend_from_slice(&d.to_be_bytes()),
            Tag::ByteArray(b) => {
                out.extend_from_slice(&(b.len() as i32).to_be_bytes());
                out.extend_from_slice(b);
            }
            Tag::String(s) => string(out, s),
            Tag::List(l) => {
                out.push(l.first().map_or(0, kind));
                out.extend_from_slice(&(l.len() as i32).to_be_bytes());
                l.iter().for_each(|e| value(out, e));
            }
            Tag::Compound(c) => compound(out, c),
            Tag::IntArray(b) => {
                out.extend_from_slice(&((b.len() / 4) as i32).to_be_bytes());
                out.extend_from_slice(b);
            }
            Tag::LongArray(b) => {
                out.extend_from_slice(&((b.len() / 8) as i32).to_be_bytes());
                out.extend_from_slice(b);
            }
        }
    }

    /// Gzip with stored deflate blocks (the reader doesn't check the CRC).
    pub fn gzip(data: &[u8]) -> Vec<u8> {
        let mut out = vec![0x1f, 0x8b, 8, 0, 0, 0, 0, 0, 0, 255];
        out.extend_from_slice(&miniz_oxide::deflate::compress_to_vec(data, 1));
        out.extend_from_slice(&[0; 4]);
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out
    }
}

//endregion

#[cfg(test)]
mod tests {
    use super::write::*;
    use super::*;

    fn sample<'a>(bytes: &'a [u8], ints: &'a [u8], longs: &'a [u8]) -> Compound<'a> {
        Compound(vec![
            ("b".into(), Tag::Byte(-3)),
            ("s".into(), Tag::Short(-300)),
            ("i".into(), Tag::Int(70000)),
            ("l".into(), Tag::Long(-1 << 40)),
            ("f".into(), Tag::Float(1.5)),
            ("d".into(), Tag::Double(-2.25)),
            ("ba".into(), Tag::ByteArray(bytes)),
            ("str".into(), Tag::String("héllo".into())),
            ("list".into(), Tag::List(vec![Tag::Int(1), Tag::Int(2)])),
            ("nested".into(), Tag::Compound(Compound(vec![("x".into(), Tag::String("y".into()))]))),
            ("ia".into(), Tag::IntArray(ints)),
            ("la".into(), Tag::LongArray(longs)),
            ("empty".into(), Tag::List(Vec::new())),
            ("Entities".into(), Tag::List(vec![Tag::Compound(Compound(vec![("id".into(), Tag::Int(5))]))])),
        ])
    }

    #[test]
    fn every_tag_type_round_trips() {
        let ints: Vec<u8> = [1i32, -2, 3].iter().flat_map(|v| v.to_be_bytes()).collect();
        let longs: Vec<u8> = [u64::MAX, 7].iter().flat_map(|v| v.to_be_bytes()).collect();
        let c = sample(&[1, 2, 255], &ints, &longs);
        let raw = root("Schematic", &c);
        let (name, back) = parse(&raw, &[]).unwrap();
        assert_eq!(name, "Schematic");
        assert_eq!(back, c);
        assert_eq!(back.str("str"), Some("héllo"));
        assert_eq!(back.int("s"), Some(-300));
        let Some(Tag::LongArray(l)) = back.get("la") else { panic!() };
        assert_eq!((long_at(l, 0), long_at(l, 1)), (u64::MAX, 7));
        assert_eq!(back.get("ia").unwrap().as_ivec3(), Some([1, -2, 3]));
        // Skipped entries are read past and left out.
        let (_, skipped) = parse(&raw, &["Entities", "nested"]).unwrap();
        assert!(!skipped.has("Entities") && !skipped.has("nested") && skipped.has("la"));
        // Compressed forms.
        for packed in [gzip(&raw), miniz_oxide::deflate::compress_to_vec_zlib(&raw, 6)] {
            let data = decompress(&packed, 1 << 20).unwrap();
            assert_eq!(parse(&data, &[]).unwrap().1, c);
            assert_eq!(decompress_head(&packed, 12), raw[..12]);
        }
    }

    #[test]
    fn hostile_data_fails_cleanly() {
        // A list of compounds claiming two billion elements.
        let mut bomb = vec![10, 0, 0, 9, 0, 1, b'x', 10];
        bomb.extend_from_slice(&i32::MAX.to_be_bytes());
        assert!(parse(&bomb, &[]).is_err());
        // A list of nothing that claims elements.
        let mut empty = vec![10, 0, 0, 9, 0, 1, b'x', 0];
        empty.extend_from_slice(&5i32.to_be_bytes());
        assert!(parse(&empty, &[]).is_err());
        // Nesting past 512 levels, kept and skipped.
        let mut deep = vec![10, 0, 0];
        for _ in 0..600 {
            deep.extend_from_slice(&[10, 0, 1, b'a']);
        }
        assert!(parse(&deep, &[]).is_err());
        assert!(parse(&deep, &["a"]).is_err());
        let mut deep_lists = vec![10, 0, 0, 9, 0, 1, b'a'];
        for _ in 0..600 {
            deep_lists.extend_from_slice(&[9, 0, 0, 0, 1]);
        }
        assert!(parse(&deep_lists, &[]).is_err());
        // Negative and oversized array lengths.
        for n in [-1i32, i32::MAX] {
            let mut arr = vec![10, 0, 0, 12, 0, 1, b'x'];
            arr.extend_from_slice(&n.to_be_bytes());
            assert!(parse(&arr, &[]).is_err());
        }
        // A decompression bomb: 64 MB of zeros in a few kilobytes.
        let zeros = vec![0u8; 64 << 20];
        let packed = gzip(&zeros);
        assert!(packed.len() < 1 << 20);
        assert!(matches!(decompress(&packed, 16 << 20), Err(LoadError::TooLarge(_))));
        // Truncations and corruptions of a real tree never panic.
        let raw = root("", &sample(&[1, 2], &[0; 12], &[0; 16]));
        for cut in 0..raw.len() {
            let _ = parse(&raw[..cut], &[]);
        }
        let gz = gzip(&raw);
        for i in 0..gz.len() {
            let mut bad = gz.clone();
            bad[i] ^= 0x5a;
            if let Ok(d) = decompress(&bad, 1 << 20) {
                let _ = parse(&d, &[]);
            }
            let _ = decompress_head(&bad, 64);
        }
    }
}
