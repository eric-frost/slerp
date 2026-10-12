//! The 3D Gaussian splatting reference PLY: one `vertex` element whose
//! properties are a splat's centre (`x`, `y`, `z`), band 0 colour
//! (`f_dc_0..2`), higher spherical-harmonic bands (`f_rest_*`, all red
//! coefficients, then green, then blue), opacity before its sigmoid,
//! log scales (`scale_0..2`) and a rotation quaternion (`rot_0..3`, w
//! first).  Files are binary, usually little-endian floats, and read as a
//! stream of records so a gigabyte capture never sits in memory twice.
//!
//! The header parser here is also used to recognise SuperSplat's
//! compressed PLY (`compressed.rs`).

use crate::{Builder, SH_C0, SplatFormat, degree_for, sigmoid};
use glam::{Quat, Vec3};
use slerp_scene::{Info, Limits, LoadError, Scene};
use std::io::{BufReader, Read};
use std::path::Path;

/*┌─────────────────╮
| Header          |
└─────────────────╯*/
//region Header

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Ty {
    I8,
    U8,
    I16,
    U16,
    I32,
    U32,
    F32,
    F64,
}

impl Ty {
    fn parse(s: &str) -> Option<Ty> {
        Some(match s {
            "char" | "int8" => Ty::I8,
            "uchar" | "uint8" => Ty::U8,
            "short" | "int16" => Ty::I16,
            "ushort" | "uint16" => Ty::U16,
            "int" | "int32" => Ty::I32,
            "uint" | "uint32" => Ty::U32,
            "float" | "float32" => Ty::F32,
            "double" | "float64" => Ty::F64,
            _ => return None,
        })
    }

    pub fn size(self) -> usize {
        match self {
            Ty::I8 | Ty::U8 => 1,
            Ty::I16 | Ty::U16 => 2,
            Ty::I32 | Ty::U32 | Ty::F32 => 4,
            Ty::F64 => 8,
        }
    }

    /// Reads one value of this type from the start of `b`, which holds at
    /// least `size()` bytes.
    fn read(self, b: &[u8], big_endian: bool) -> f64 {
        macro_rules! num {
            ($t:ty, $n:expr) => {{
                let mut a = [0u8; $n];
                a.copy_from_slice(&b[..$n]);
                (if big_endian { <$t>::from_be_bytes(a) } else { <$t>::from_le_bytes(a) }) as f64
            }};
        }
        match self {
            Ty::I8 => b[0] as i8 as f64,
            Ty::U8 => b[0] as f64,
            Ty::I16 => num!(i16, 2),
            Ty::U16 => num!(u16, 2),
            Ty::I32 => num!(i32, 4),
            Ty::U32 => num!(u32, 4),
            Ty::F32 => num!(f32, 4),
            Ty::F64 => num!(f64, 8),
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct Property {
    pub name: String,
    pub ty: Ty,
    /// Byte offset within the element's record.
    pub offset: usize,
}

#[derive(Clone, Debug)]
pub(crate) struct Element {
    pub name: String,
    pub count: u64,
    pub props: Vec<Property>,
    /// Bytes per record.
    pub size: usize,
}

impl Element {
    pub fn prop(&self, name: &str) -> Option<&Property> {
        self.props.iter().find(|p| p.name == name)
    }
}

pub(crate) struct Header {
    pub big_endian: bool,
    pub elements: Vec<Element>,
    pub comments: Vec<String>,
    /// Offset of the first data byte.
    pub data: usize,
}

impl Header {
    pub fn element(&self, name: &str) -> Option<&Element> {
        self.elements.iter().find(|e| e.name == name)
    }

    /// The data offset of element `index`, after every element before it.
    /// `None` when that overflows.
    pub fn offset_of(&self, index: usize) -> Option<u64> {
        let mut at = self.data as u64;
        for e in &self.elements[..index] {
            at = at.checked_add(e.count.checked_mul(e.size as u64)?)?;
        }
        Some(at)
    }
}

/// Parses a binary PLY header from the start of `bytes`.  List properties
/// (faces) make a file no splat file, and ASCII PLY is refused: no splat
/// tool writes it.
pub(crate) fn parse_header(bytes: &[u8]) -> Result<Header, LoadError> {
    let limit = bytes.len().min(1 << 20);
    let end = find(&bytes[..limit], b"end_header").ok_or_else(|| LoadError::invalid("PLY header has no end_header"))?;
    let mut data = end + b"end_header".len();
    while data < bytes.len() && bytes[data] != b'\n' {
        data += 1;
    }
    let data = (data + 1).min(bytes.len());
    let text = String::from_utf8_lossy(&bytes[..end]);
    let mut header = Header { big_endian: false, elements: Vec::new(), comments: Vec::new(), data };
    let mut format = None;
    for line in text.lines().skip(1) {
        let mut t = line.split_ascii_whitespace();
        match t.next() {
            Some("format") => format = t.next().map(str::to_string),
            Some("comment") | Some("obj_info") => {
                let rest = line.trim().split_once(char::is_whitespace).map(|(_, r)| r.trim()).unwrap_or("");
                header.comments.push(rest.to_string());
            }
            Some("element") => {
                let name = t.next().unwrap_or("").to_string();
                let count = t
                    .next()
                    .and_then(|c| c.parse::<u64>().ok())
                    .ok_or_else(|| LoadError::invalid("bad PLY element count"))?;
                header.elements.push(Element { name, count, props: Vec::new(), size: 0 });
            }
            Some("property") => {
                let element =
                    header.elements.last_mut().ok_or_else(|| LoadError::invalid("PLY property before any element"))?;
                let ty = t.next().unwrap_or("");
                if ty == "list" {
                    return Err(LoadError::Unsupported("PLY list properties in a splat file".into()));
                }
                let ty = Ty::parse(ty).ok_or_else(|| LoadError::invalid(format!("unknown PLY type {ty}")))?;
                let name = t.next().unwrap_or("").to_string();
                element.props.push(Property { name, ty, offset: element.size });
                element.size += ty.size();
            }
            _ => {}
        }
    }
    header.big_endian = match format.as_deref() {
        Some("binary_little_endian") => false,
        Some("binary_big_endian") => true,
        Some("ascii") => return Err(LoadError::Unsupported("ASCII PLY splats".into())),
        _ => return Err(LoadError::invalid("PLY header has no binary format line")),
    };
    Ok(header)
}

pub(crate) fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

/// Splat PLYs by their header: the reference layout (with `f_dc_0`, or at
/// least `opacity`, `scale_0` and `rot_0`, as the mesh loader also checks)
/// or SuperSplat's chunked one.
pub(crate) fn sniff(head: &[u8]) -> Option<SplatFormat> {
    let header = &head[..find(head, b"end_header").unwrap_or(head.len())];
    let text = String::from_utf8_lossy(header);
    let mut elements: Vec<&str> = Vec::new();
    let mut props: Vec<(&str, &str)> = Vec::new();
    for line in text.lines() {
        let mut t = line.split_ascii_whitespace();
        match t.next() {
            Some("element") => elements.push(t.next().unwrap_or("")),
            Some("property") => {
                if let (Some(&e), Some(name)) = (elements.last(), t.last()) {
                    props.push((e, name));
                }
            }
            _ => {}
        }
    }
    let has = |e: &str, n: &str| props.contains(&(e, n));
    if elements.first() == Some(&"chunk") && has("vertex", "packed_position") && has("vertex", "packed_rotation") {
        return Some(SplatFormat::CompressedPly);
    }
    let splat =
        has("vertex", "f_dc_0") || (has("vertex", "opacity") && has("vertex", "scale_0") && has("vertex", "rot_0"));
    splat.then_some(SplatFormat::Ply)
}

//endregion

/*┌─────────────────╮
| Records         |
└─────────────────╯*/
//region Records

/// Where each splat field sits in a vertex record.
struct Layout {
    pos: [Property; 3],
    dc: Option<[Property; 3]>,
    /// Plain `red`, `green`, `blue` colours, when a file has no `f_dc_*`.
    rgb: Option<[Property; 3]>,
    opacity: Option<Property>,
    scale: [Property; 3],
    rot: [Property; 4],
    /// `f_rest_0..`, in file order: all red coefficients, then green, then
    /// blue.
    rest: Vec<Property>,
    /// Coefficients per channel in the file.
    rest_per_channel: usize,
    big_endian: bool,
    /// Every property is a little-endian float: the common case, read
    /// without the general conversion.
    fast: bool,
}

impl Layout {
    fn new(v: &Element, big_endian: bool, info: &mut Info) -> Result<Layout, LoadError> {
        let get = |n: &str| v.prop(n).cloned();
        let need = |n: &str| get(n).ok_or_else(|| LoadError::invalid(format!("splat PLY has no {n} property")));
        let three = |a: &str, b: &str, c: &str| -> Option<[Property; 3]> { Some([get(a)?, get(b)?, get(c)?]) };
        let pos = [need("x")?, need("y")?, need("z")?];
        let scale = [need("scale_0")?, need("scale_1")?, need("scale_2")?];
        let rot = [need("rot_0")?, need("rot_1")?, need("rot_2")?, need("rot_3")?];
        let mut rest: Vec<Property> = Vec::new();
        while let Some(p) = get(&format!("f_rest_{}", rest.len())) {
            rest.push(p);
        }
        let per_channel = rest.len() / 3;
        let rest_per_channel = match degree_for(per_channel) {
            Some(_) if rest.len().is_multiple_of(3) => per_channel,
            _ => {
                if !rest.is_empty() {
                    info.warnings
                        .push(format!("ignored {} spherical-harmonic coefficients of no known degree", rest.len()));
                }
                rest.clear();
                0
            }
        };
        let fast = !big_endian && v.props.iter().all(|p| p.ty == Ty::F32);
        Ok(Layout {
            pos,
            dc: three("f_dc_0", "f_dc_1", "f_dc_2"),
            rgb: three("red", "green", "blue"),
            opacity: get("opacity"),
            scale,
            rot,
            rest,
            rest_per_channel,
            big_endian,
            fast,
        })
    }

    fn get(&self, rec: &[u8], p: &Property) -> f32 {
        let b = &rec[p.offset..];
        if self.fast { f32::from_le_bytes([b[0], b[1], b[2], b[3]]) } else { p.ty.read(b, self.big_endian) as f32 }
    }

    /// Decodes one record into the builder.
    fn push(&self, rec: &[u8], out: &mut Builder) {
        let g = |p: &Property| self.get(rec, p);
        let position = Vec3::new(g(&self.pos[0]), g(&self.pos[1]), g(&self.pos[2]));
        let scale = Vec3::new(g(&self.scale[0]), g(&self.scale[1]), g(&self.scale[2])).exp();
        let rotation = Quat::from_xyzw(g(&self.rot[1]), g(&self.rot[2]), g(&self.rot[3]), g(&self.rot[0]));
        let rgb = match (&self.dc, &self.rgb) {
            (Some(dc), _) => Vec3::new(g(&dc[0]), g(&dc[1]), g(&dc[2])) * SH_C0 + 0.5,
            (None, Some(c)) => {
                let s = match c[0].ty {
                    Ty::U8 => 255.0,
                    Ty::U16 => 65535.0,
                    _ => 1.0,
                };
                Vec3::new(g(&c[0]), g(&c[1]), g(&c[2])) / s
            }
            _ => Vec3::splat(0.7),
        };
        let opacity = self.opacity.as_ref().map_or(1.0, |p| sigmoid(g(p)));
        let n = self.rest_per_channel;
        out.push(position, scale, rotation, rgb.extend(opacity), |k, c| g(&self.rest[c * n + k]));
    }
}

//endregion

/// Loads a splat PLY from `path`, `size` bytes long.
pub(crate) fn load(path: &Path, size: u64, limits: &Limits) -> Result<Scene, LoadError> {
    load_reader(std::fs::File::open(path)?, size, limits)
}

/// Loads a splat PLY from a reader holding `size` bytes.
pub(crate) fn load_reader(reader: impl Read, size: u64, limits: &Limits) -> Result<Scene, LoadError> {
    let mut reader = BufReader::with_capacity(1 << 20, reader);
    // 1. The header, read in growing pieces until `end_header` shows up.
    let mut head = Vec::new();
    loop {
        let before = head.len();
        reader.by_ref().take(16 << 10).read_to_end(&mut head)?;
        if find(&head, b"end_header").is_some_and(|e| head[e..].contains(&b'\n')) || head.len() == before {
            break;
        }
        if head.len() > 1 << 20 {
            return Err(LoadError::invalid("PLY header longer than 1 MB"));
        }
    }
    let header = parse_header(&head)?;
    let vi = header
        .elements
        .iter()
        .position(|e| e.name == "vertex")
        .ok_or_else(|| LoadError::invalid("PLY has no vertex element"))?;
    let vertex = &header.elements[vi];
    if vertex.size == 0 {
        return Err(LoadError::invalid("PLY vertices have no properties"));
    }
    let mut info = Info { format: SplatFormat::Ply.name().into(), ..Default::default() };
    if let Some(c) = header.comments.iter().find(|c| c.to_ascii_lowercase().contains("generated by")) {
        info.generator = Some(c.trim_start_matches("Generated by").trim().to_string());
    }
    let layout = Layout::new(vertex, header.big_endian, &mut info)?;

    // 2. The records present: a truncated download loads what it has.
    let start = header.offset_of(vi).ok_or_else(|| LoadError::invalid("PLY element sizes overflow"))?;
    let present = size.saturating_sub(start) / vertex.size as u64;
    let count = vertex.count.min(present);
    if count == 0 {
        return Err(LoadError::invalid("PLY has no complete splat records"));
    }
    if count < vertex.count {
        info.warnings.push(format!("the file is cut short: {} of {} splats present", count, vertex.count));
    }
    let degree = degree_for(layout.rest_per_channel).unwrap_or(0);
    let mut out = Builder::new(count, limits, degree, true);

    // 3. Skip to the vertices, then decode them in blocks of records.
    let mut skip = start.saturating_sub(header.data as u64);
    let mut leftover = head.split_off(header.data.min(head.len()));
    let from_head = (skip as usize).min(leftover.len());
    leftover.drain(..from_head);
    skip -= from_head as u64;
    std::io::copy(&mut reader.by_ref().take(skip), &mut std::io::sink())?;
    let mut stream = std::io::Cursor::new(leftover).chain(reader);
    let rec = vertex.size;
    let per_block = ((1usize << 20) / rec).max(1);
    let mut block = vec![0u8; per_block * rec];
    let mut i = 0u64;
    while i < count {
        let n = (per_block as u64).min(count - i) as usize;
        let buf = &mut block[..n * rec];
        if let Err(e) = stream.read_exact(buf) {
            if e.kind() == std::io::ErrorKind::UnexpectedEof {
                break;
            }
            return Err(e.into());
        }
        for (j, r) in buf.chunks_exact(rec).enumerate() {
            if out.keeps(i + j as u64) {
                layout.push(r, &mut out);
            }
        }
        i += n as u64;
    }
    out.finish(info)
}
