//! Garmin's FIT activity files, which Garmin, Wahoo, Hammerhead, Coros and
//! most other bike computers and watches record, read from the published
//! FIT protocol.
//!
//! A file is a header (12 or 14 bytes: size, protocol and profile
//! versions, the data's length, `.FIT`) and records.  A definition record
//! tells how a local message type (0-15) lays out its fields (number, size,
//! base type, byte order) for a global message number; data records then
//! carry values in that layout.  A compressed-timestamp header packs the
//! local type and a 5-bit time offset into the record header itself.
//! Developer fields are skipped by their size.  Files may be chained one
//! after another, and are often cut short by a crash: whatever was read
//! stays.
//!
//! What is read: `record` (20) positions in semicircles, altitude or
//! enhanced altitude, speed or enhanced speed, heart rate, cadence, power
//! and time; `session` (18) totals; `lap` (19) count; `file_id` (0) maker
//! and product; `sport` (12).

use super::{DeviceTotals, Line, LineKind, TrackPoint, TrackSet};
use slerp_scene::LoadError;
use std::ops::Range;
use std::rc::Rc;

/// Seconds from 1970 to FIT's epoch, 1989-12-31 00:00 UTC.
const FIT_EPOCH: f64 = 631_065_600.0;
/// Degrees per semicircle: 180 / 2^31.
const SEMICIRCLE: f64 = 180.0 / 2_147_483_648.0;
/// Records kept at most.
const MAX_RECORDS: usize = 8_000_000;

pub fn is_fit(head: &[u8]) -> bool {
    head.len() >= 12 && matches!(head[0], 12 | 14) && &head[8..12] == b".FIT"
}

/// How far past a damaged record the stream is searched for, in bytes.
const RESYNC_SPAN: usize = 4096;
/// How many records in a row must make sense to take a position for the
/// start of one.
const RESYNC_RECORDS: usize = 4;
/// The longest pause, in seconds, between one `record` and the next that
/// still counts as the same recording (two days).
const MAX_TIME_GAP: u32 = 2 * 86400;
/// Damaged spots mended in one file, which keeps the search bounded.
const MAX_RESYNCS: usize = 1000;

#[derive(Clone)]
struct Definition {
    global: u16,
    big_endian: bool,
    /// (field number, offset, size, base type)
    fields: Vec<(u8, usize, usize, u8)>,
    /// Bytes of a data record, developer fields included.
    size: usize,
}

impl Definition {
    /// The timestamp field (253) of a data record laid out like this.
    fn time(&self, bytes: &[u8]) -> Option<u32> {
        let &(_, offset, size, base) = self.fields.iter().find(|f| f.0 == 253)?;
        value(bytes.get(offset..offset + size)?, base, self.big_endian)?.uint().map(|t| t as u32)
    }
}

pub fn parse_fit(data: &[u8]) -> Result<TrackSet, LoadError> {
    if !is_fit(data) {
        return Err(LoadError::NotMine);
    }
    let mut r = Reader::default();
    let mut pos = 0usize;
    let mut files = 0;
    while pos + 12 <= data.len() && is_fit(&data[pos..]) {
        let header = data[pos] as usize;
        let size = u32::from_le_bytes([data[pos + 4], data[pos + 5], data[pos + 6], data[pos + 7]]) as usize;
        let start = pos + header;
        let declared_end = start.saturating_add(size);
        let end = declared_end.min(data.len());
        if declared_end > data.len() {
            r.warnings.push("the file is cut short".into());
        }
        // A zero size is a file whose writer never went back to fill it in.
        let end = if size == 0 { data.len() } else { end };
        if start <= end {
            // Each file of a chain defines its own layouts and keeps its own
            // clock, and is its own line.
            r.defs = Default::default();
            r.last_time = 0;
            r.last_record_time = None;
            r.records(&data[start..end]);
            r.flush();
        }
        files += 1;
        pos = end.saturating_add(2);
        if size == 0 || files > 64 {
            break;
        }
    }
    r.flush();
    let mut set = TrackSet {
        format: "FIT".into(),
        lines: r.lines,
        creator: r.creator,
        sport: r.sport,
        device: r.totals,
        warnings: r.warnings,
        ..Default::default()
    };
    set.device.laps = r.laps;
    Ok(set)
}

#[derive(Default)]
struct Reader {
    defs: [Option<Rc<Definition>>; 16],
    last_time: u32,
    /// The time of the last `record` message, to check a resynchronised
    /// stream against.
    last_record_time: Option<u32>,
    current: Vec<TrackPoint>,
    lines: Vec<Line>,
    records: usize,
    laps: u32,
    totals: DeviceTotals,
    creator: Option<String>,
    sport: Option<String>,
    warnings: Vec<String>,
}

impl Reader {
    /*┌─────────────────╮
    | Records         |
    └─────────────────╯*/
    //region Records

    fn records(&mut self, d: &[u8]) {
        let mut p = 0usize;
        let mut resyncs = 0;
        while p < d.len() {
            match parse_record(&self.defs, d, p) {
                Ok((Parsed::Definition(local, def), next)) => {
                    self.defs[local] = Some(Rc::new(def));
                    p = next;
                }
                Ok((Parsed::Data { local, offset, range }, next)) => {
                    let mut time = None;
                    if let Some(offset) = offset {
                        // A compressed timestamp rolls the last time forward
                        // to the next moment whose low five bits are
                        // `offset`.
                        let base = self.last_time & !0x1f;
                        // Wrapping: a damaged file's last time may be anything.
                        let t = if offset >= (self.last_time & 0x1f) {
                            base.wrapping_add(offset)
                        } else {
                            base.wrapping_add(offset).wrapping_add(0x20)
                        };
                        self.last_time = t;
                        time = Some(t);
                    }
                    let def = self.defs[local].clone().expect("parse_record found it");
                    self.data(&def, &d[range], time);
                    p = next;
                }
                Err(Fault::Short) => {
                    self.warnings.push("the file ends inside a record".into());
                    return;
                }
                Err(Fault::Lost) => {
                    // Damage (a dropped byte, a bad sector) shifts every
                    // header after it.  The records repeat the same few
                    // layouts, so the stream can usually be picked up again
                    // a few bytes on.
                    match self.resync(d, p).filter(|_| resyncs < MAX_RESYNCS) {
                        Some(q) => {
                            if resyncs == 0 {
                                self.warnings.push("damaged records were skipped".into());
                            }
                            resyncs += 1;
                            p = q;
                        }
                        None => {
                            self.warnings.push("a record without a definition; the rest of the file is skipped".into());
                            return;
                        }
                    }
                }
            }
        }
    }

    /// The next position after `from` where several records in a row make
    /// sense: each has a header with a known layout and fits in the data,
    /// and the times of `record` messages go on from the last good one (a
    /// wrong alignment of a regular stream has headers that repeat, but
    /// its times are noise).
    fn resync(&self, d: &[u8], from: usize) -> Option<usize> {
        let last = d.len().min(from + RESYNC_SPAN);
        (from + 1..last).find(|&q| {
            let mut defs = self.defs.clone();
            let mut time = self.last_record_time;
            let (mut p, mut n) = (q, 0);
            while p < d.len() && n < RESYNC_RECORDS {
                match parse_record(&defs, d, p) {
                    Ok((Parsed::Definition(local, def), next)) => {
                        defs[local] = Some(Rc::new(def));
                        p = next;
                    }
                    Ok((Parsed::Data { local, range, .. }, next)) => {
                        let def = defs[local].as_ref().expect("parse_record found it");
                        if def.global == 20
                            && let Some(t) = def.time(&d[range])
                        {
                            if time.is_some_and(|prev| t + 5 < prev || t > prev.saturating_add(MAX_TIME_GAP)) {
                                return false;
                            }
                            time = Some(t);
                        }
                        p = next;
                    }
                    Err(_) => return false,
                }
                n += 1;
            }
            n >= RESYNC_RECORDS || (n > 0 && p >= d.len())
        })
    }

    //endregion

    fn data(&mut self, def: &Definition, bytes: &[u8], header_time: Option<u32>) {
        let field = |num: u8| -> Option<Value> {
            let &(_, offset, size, base) = def.fields.iter().find(|f| f.0 == num)?;
            value(bytes.get(offset..offset + size)?, base, def.big_endian)
        };
        let uint = |num: u8| field(num).and_then(Value::uint);
        let int = |num: u8| field(num).and_then(Value::int);
        let stamp = uint(253).map(|t| t as u32);
        // A record whose time jumps back or years ahead is damaged,
        // whatever its position says.
        if def.global == 20
            && let Some(t) = stamp
            && self.last_record_time.is_some_and(|prev| t.saturating_add(5) < prev || t > prev.saturating_add(MAX_TIME_GAP))
        {
            return;
        }
        if let Some(t) = stamp {
            self.last_time = t;
        }
        match def.global {
            20 => {
                if self.records >= MAX_RECORDS {
                    return;
                }
                self.records += 1;
                let (Some(lat), Some(lon)) = (int(0), int(1)) else { return };
                let mut pt = TrackPoint::new(lat as f64 * SEMICIRCLE, lon as f64 * SEMICIRCLE);
                let time = stamp.or(header_time);
                if let Some(t) = time {
                    pt.time = t as f64 + FIT_EPOCH;
                    self.last_record_time = Some(t);
                }
                // Altitude is metres × 5 with a 500 m offset; `tidy` drops
                // what a damaged record makes of it.
                if let Some(a) = uint(78).or(uint(2)) {
                    pt.ele = (a as f64 / 5.0 - 500.0) as f32;
                }
                if let Some(s) = uint(73).or(uint(6)) {
                    pt.speed = (s as f64 / 1000.0) as f32;
                }
                if let Some(v) = uint(3) {
                    pt.hr = v as f32;
                }
                if let Some(v) = uint(4) {
                    pt.cad = v as f32;
                }
                if let Some(v) = uint(7) {
                    pt.power = v as f32;
                }
                self.current.push(pt);
            }
            18 => {
                let t = &mut self.totals;
                // Several sessions (a multisport file) add up.
                let add = |slot: &mut Option<f64>, v: Option<f64>| {
                    if let Some(v) = v {
                        *slot = Some(slot.unwrap_or(0.0) + v);
                    }
                };
                add(&mut t.elapsed, uint(7).map(|v| v as f64 / 1000.0));
                add(&mut t.timer, uint(8).map(|v| v as f64 / 1000.0));
                add(&mut t.distance, uint(9).map(|v| v as f64 / 100.0));
                add(&mut t.ascent, uint(22).map(|v| v as f64));
                add(&mut t.descent, uint(23).map(|v| v as f64));
                add(&mut t.calories, uint(11).map(|v| v as f64));
                if let Some(s) = uint(5) {
                    self.sport = Some(sport_name(s as u8));
                }
            }
            19 => self.laps += 1,
            12 => {
                if self.sport.is_none()
                    && let Some(s) = uint(0)
                {
                    self.sport = Some(sport_name(s as u8));
                }
            }
            0 => {
                let maker = uint(1).map(|m| manufacturer(m as u16));
                let product = field(8).and_then(Value::text);
                let name = match (maker, product) {
                    (Some(m), Some(p)) => Some(format!("{m} {p}")),
                    (Some(m), None) => Some(m),
                    (None, p) => p,
                };
                if name.is_some() {
                    self.creator = name;
                }
            }
            // A timer stop or a new activity in a chained file: keep lines
            // apart only between files (flush), not at every pause.
            _ => {}
        }
    }

    fn flush(&mut self) {
        if !self.current.is_empty() {
            let points = std::mem::take(&mut self.current);
            self.lines.push(Line { name: String::new(), kind: LineKind::Track, points });
        }
    }
}

/// What one record is.
enum Parsed {
    /// A definition of a local message type.
    Definition(usize, Definition),
    /// A data record of a defined type: where its bytes are, and the
    /// offset of a compressed timestamp header.
    Data { local: usize, offset: Option<u32>, range: Range<usize> },
}

/// Why a record can't be read.
enum Fault {
    /// The data ends inside it.
    Short,
    /// It is data of a type nothing defined.
    Lost,
}

/// Reads the record at `p`, returning it and where the next one starts.
fn parse_record(defs: &[Option<Rc<Definition>>; 16], d: &[u8], p: usize) -> Result<(Parsed, usize), Fault> {
    let h = *d.get(p).ok_or(Fault::Short)?;
    let p = p + 1;
    if h & 0x40 != 0 && h & 0x80 == 0 {
        let local = (h & 0x0f) as usize;
        let developer = h & 0x20 != 0;
        let fixed = d.get(p..p + 5).ok_or(Fault::Short)?;
        // The reserved byte is zero and the architecture 0 or 1; anything
        // else is damage.
        if fixed[0] != 0 || fixed[1] > 1 {
            return Err(Fault::Lost);
        }
        let big_endian = fixed[1] == 1;
        let global = if big_endian {
            u16::from_be_bytes([fixed[2], fixed[3]])
        } else {
            u16::from_le_bytes([fixed[2], fixed[3]])
        };
        let n = fixed[4] as usize;
        let mut p = p + 5;
        let raw = d.get(p..p + n * 3).ok_or(Fault::Short)?;
        p += n * 3;
        let mut fields = Vec::with_capacity(n);
        let mut size = 0usize;
        for f in raw.chunks_exact(3) {
            if !plausible_field(f[1] as usize, f[2]) {
                return Err(Fault::Lost);
            }
            fields.push((f[0], size, f[1] as usize, f[2]));
            size += f[1] as usize;
        }
        if developer {
            let m = *d.get(p).ok_or(Fault::Short)? as usize;
            p += 1;
            let dev = d.get(p..p + m * 3).ok_or(Fault::Short)?;
            p += m * 3;
            size += dev.chunks_exact(3).map(|f| f[1] as usize).sum::<usize>();
        }
        return Ok((Parsed::Definition(local, Definition { global, big_endian, fields, size }), p));
    }
    // A compressed timestamp header holds the local type in bits 5-6 and a
    // time offset in the low five bits.
    let (local, offset) =
        if h & 0x80 != 0 { (((h >> 5) & 3) as usize, Some((h & 0x1f) as u32)) } else { ((h & 0x0f) as usize, None) };
    let def = defs[local].as_ref().ok_or(Fault::Lost)?;
    if p + def.size > d.len() {
        return Err(Fault::Short);
    }
    Ok((Parsed::Data { local, offset, range: p..p + def.size }, p + def.size))
}

/// Whether a field can have this base type: the type number is one FIT
/// defines (0 to 16, with the endian flag in bit 7) and it holds some
/// bytes.  Sizes that aren't whole numbers of values are allowed, since
/// some devices (Coros watches) write them in otherwise good files.
fn plausible_field(size: usize, base: u8) -> bool {
    base & 0x60 == 0 && (base & 0x1f) <= 16 && size > 0
}

enum Value {
    Uint(u64),
    Int(i64),
    Text(String),
    Float(f64),
}

impl Value {
    fn uint(self) -> Option<u64> {
        match self {
            Value::Uint(v) => Some(v),
            Value::Int(v) if v >= 0 => Some(v as u64),
            Value::Float(f) if f >= 0.0 && f.is_finite() => Some(f as u64),
            _ => None,
        }
    }

    fn int(self) -> Option<i64> {
        match self {
            Value::Int(v) => Some(v),
            Value::Uint(v) => i64::try_from(v).ok(),
            Value::Float(f) if f.is_finite() => Some(f as i64),
            _ => None,
        }
    }

    fn text(self) -> Option<String> {
        match self {
            Value::Text(s) if !s.is_empty() => Some(s),
            _ => None,
        }
    }
}

/// Reads a field's first value by its base type, or `None` for FIT's
/// "invalid" value (all ones, the largest positive signed value, or zero
/// for the `z` types) or a size too small for the type.
fn value(b: &[u8], base: u8, big_endian: bool) -> Option<Value> {
    let kind = base & 0x1f;
    let width = match kind {
        0 | 1 | 2 | 7 | 10 | 13 => 1,
        3 | 4 | 11 => 2,
        5 | 6 | 8 | 12 => 4,
        9 | 14 | 15 | 16 => 8,
        _ => return None,
    };
    if kind == 7 {
        let end = b.iter().position(|&c| c == 0).unwrap_or(b.len());
        return Some(Value::Text(String::from_utf8_lossy(&b[..end]).trim().to_string()));
    }
    let raw = b.get(..width)?;
    let mut v: u64 = 0;
    for i in 0..width {
        let byte = if big_endian { raw[i] } else { raw[width - 1 - i] };
        v = v << 8 | byte as u64;
    }
    let bits = width as u32 * 8;
    let all_ones = if bits == 64 { u64::MAX } else { (1u64 << bits) - 1 };
    match kind {
        // Signed: the largest positive value is "invalid".
        1 | 3 | 5 | 14 => {
            if v == all_ones >> 1 {
                return None;
            }
            let shift = 64 - bits;
            Some(Value::Int(((v << shift) as i64) >> shift))
        }
        8 => {
            let f = f32::from_bits(v as u32);
            (v as u32 != u32::MAX && f.is_finite()).then_some(Value::Float(f as f64))
        }
        9 => {
            let f = f64::from_bits(v);
            (v != u64::MAX && f.is_finite()).then_some(Value::Float(f))
        }
        10 | 11 | 12 | 16 => (v != 0).then_some(Value::Uint(v)),
        13 => Some(Value::Uint(v)),
        _ => (v != all_ones).then_some(Value::Uint(v)),
    }
}

fn sport_name(s: u8) -> String {
    let name = match s {
        0 => "Generic",
        1 => "Running",
        2 => "Cycling",
        3 => "Transition",
        4 => "Fitness equipment",
        5 => "Swimming",
        10 => "Training",
        11 => "Walking",
        12 => "Cross-country skiing",
        13 => "Alpine skiing",
        14 => "Snowboarding",
        15 => "Rowing",
        16 => "Mountaineering",
        17 => "Hiking",
        18 => "Multisport",
        19 => "Paddling",
        21 => "E-biking",
        _ => return format!("Sport {s}"),
    };
    name.to_string()
}

/// The makers in FIT's profile that most activity files come from.
fn manufacturer(m: u16) -> String {
    let name = match m {
        1 => "Garmin",
        15 | 13 => "Dynastream",
        23 => "Suunto",
        32 => "Wahoo",
        41 => "Shimano",
        69 => "Stages",
        70 => "Sigma",
        89 => "Tacx",
        107 => "Magene",
        115 => "iGPSPORT",
        123 => "Polar",
        255 => "Development",
        258 => "Lezyne",
        260 => "Zwift",
        265 => "Strava",
        267 => "Bryton",
        289 => "Hammerhead",
        294 => "Coros",
        _ => return format!("Manufacturer {m}"),
    };
    name.to_string()
}
