//! Godot's binary resource format (`.scn`, `.res`, `.mesh`), versions 2
//! to 6 of the format (Godot 3 and 4), plain (`RSRC`) or compressed
//! (`RSCC`: blocks of zstd, deflate, gzip or FastLZ).  Read into the same
//! `ResFile` as text resources, with a binary scene's packed node table
//! turned into `[node]`-style entries.
//!
//! Layout, after Godot's `core/io/resource_format_binary.cpp`: a header
//! (endianness, version, the main resource's type), a string table, the
//! external resources, and a table of internal resources with offsets,
//! each a type and a list of properties; the main resource comes last.

use super::variant::{ExtRes, NodeEntry, ResFile, SubRes, Value};
use slerp_scene::LoadError;

const MAX_DEPTH: u32 = 64;
/// The largest decompressed resource accepted.
const MAX_INFLATED: u64 = 1 << 31;

const FLAG_NAMED_SCENE_IDS: u32 = 1;
const FLAG_UIDS: u32 = 2;
const FLAG_REAL_IS_DOUBLE: u32 = 4;
const FLAG_HAS_SCRIPT_CLASS: u32 = 8;

/// The main resource's type, from the start of an uncompressed file.
pub(crate) fn header_type(head: &[u8]) -> Option<String> {
    let mut r = Reader::new(head.strip_prefix(b"RSRC")?);
    r.big = r.u32().ok()? != 0;
    r.u32().ok()?;
    r.u32().ok()?;
    r.u32().ok()?;
    r.u32().ok()?;
    r.string().ok()
}

/// Parses a whole binary resource.  `max_bytes` caps the decompressed
/// size of a compressed one.
pub(crate) fn parse(bytes: &[u8], max_bytes: u64) -> Result<ResFile, LoadError> {
    // Offsets count from the file's start in a plain file, which includes
    // the magic, and from the start of the decompressed data otherwise.
    let inflated;
    let (body, shift) = if let Some(rest) = bytes.strip_prefix(b"RSCC") {
        inflated = decompress(rest, max_bytes.min(MAX_INFLATED))?;
        (&inflated[..], 0)
    } else if let Some(rest) = bytes.strip_prefix(b"RSRC") {
        (rest, 4)
    } else {
        return Err(LoadError::NotMine);
    };
    let mut r = Reader::new(body);

    // 1. Header.
    r.big = r.u32()? != 0;
    let _use_real64 = r.u32()?;
    let major = r.u32()?;
    let _minor = r.u32()?;
    let format = r.u32()?;
    if format > 6 || major > 4 {
        return Err(LoadError::Unsupported(format!("Godot {major} binary resource, format {format}")));
    }
    let kind = r.string()?;
    let _import_metadata = r.u64()?;
    let flags = r.u32()?;
    r.real64 = flags & FLAG_REAL_IS_DOUBLE != 0;
    let named_ids = flags & FLAG_NAMED_SCENE_IDS != 0;
    let uids = flags & FLAG_UIDS != 0;
    let _uid = r.u64()?;
    if flags & FLAG_HAS_SCRIPT_CLASS != 0 {
        r.string()?;
    }
    for _ in 0..11 {
        r.u32()?;
    }
    r.format = format;

    // 2. Tables.
    let count = r.count(4)?;
    let mut strings = Vec::with_capacity(count);
    for _ in 0..count {
        strings.push(r.string()?);
    }
    r.strings = strings;
    let count = r.count(8)?;
    let mut ext = Vec::with_capacity(count);
    for i in 0..count {
        let kind = r.string()?;
        let path = r.string()?;
        if uids {
            r.u64()?;
        }
        ext.push(ExtRes { id: i.to_string(), kind, path });
    }
    let count = r.count(12)?;
    let mut internal: Vec<(String, u64)> = Vec::with_capacity(count);
    for _ in 0..count {
        let path = r.string()?;
        let offset = r.u64()?;
        // "local://ID" in a file, "res://file.scn::ID" in older ones.
        let id = path.strip_prefix("local://").or_else(|| path.rsplit_once("::").map(|(_, id)| id)).unwrap_or(&path);
        internal.push((id.to_string(), offset));
    }
    r.internal_ids = internal.iter().map(|(id, _)| id.clone()).collect();
    r.named_ids = named_ids;

    // 3. Resources: every internal one, the main one last.
    let mut file = ResFile { godot3: major < 4, kind: kind.clone(), ext, ..Default::default() };
    let last = internal.len().saturating_sub(1);
    for (i, (id, offset)) in internal.iter().enumerate() {
        r.pos = usize::try_from(*offset)
            .ok()
            .and_then(|o| o.checked_sub(shift))
            .ok_or_else(|| LoadError::invalid("bad offset"))?;
        let res_kind = r.string()?;
        let n = r.count(8)?;
        let mut props = Vec::with_capacity(n);
        for _ in 0..n {
            let name = r.name()?;
            let value = r.variant(0)?;
            props.push((name, value));
        }
        if i == last {
            file.main = props;
        } else {
            file.subs.push(SubRes { id: id.clone(), kind: res_kind, props });
        }
    }
    if file.kind == "PackedScene" {
        file.nodes = bundled_nodes(file.main_prop("_bundled"))?;
    }
    Ok(file)
}

/// Turns a packed scene's `_bundled` table (see Godot's
/// `SceneState::set_bundled_scene`) into node entries.
fn bundled_nodes(bundled: Option<&Value>) -> Result<Vec<NodeEntry>, LoadError> {
    const NO_PARENT: i64 = 0x7FFF_FFFF;
    const FLAG_ID_IS_PATH: i64 = 1 << 30;
    const TYPE_INSTANTIATED: i64 = 0x7FFF_FFFF;
    const FLAG_MASK: i64 = (1 << 24) - 1;
    const NAME_INDEX_BITS: u32 = 18;
    let b = bundled.ok_or_else(|| LoadError::invalid("scene without nodes"))?;
    let names: Vec<String> = match b.get("names") {
        Some(Value::Strings(s)) => s.clone(),
        Some(Value::Array(a)) => a.iter().map(|v| v.str().unwrap_or("").to_string()).collect(),
        _ => Vec::new(),
    };
    let empty = Vec::new();
    let variants = b.get("variants").and_then(Value::array).unwrap_or(&empty);
    let node_paths: Vec<String> = b
        .get("node_paths")
        .and_then(Value::array)
        .unwrap_or(&empty)
        .iter()
        .map(|v| v.str().unwrap_or(".").into())
        .collect();
    let count = b.get("node_count").and_then(Value::int).unwrap_or(0).max(0) as usize;
    let data = b.get("nodes").and_then(Value::ints).unwrap_or(&[]);
    let name = |i: i64| names.get(i as usize).cloned().unwrap_or_default();

    let mut out: Vec<NodeEntry> = Vec::with_capacity(count.min(data.len() / 6));
    // Each node's path from the root, for its children's `parent`.
    let mut paths: Vec<String> = Vec::new();
    let mut idx = 0usize;
    let mut next = || -> Result<i64, LoadError> {
        let v = *data.get(idx).ok_or_else(|| LoadError::invalid("node table cut short"))?;
        idx += 1;
        Ok(v)
    };
    for _ in 0..count {
        let parent = next()?;
        let _owner = next()?;
        let class = next()?;
        let name_index = next()?;
        let instance = next()?;
        let node_name = name(name_index & ((1 << NAME_INDEX_BITS) - 1));
        let parent_path = if parent == NO_PARENT || parent < 0 {
            None
        } else if parent & FLAG_ID_IS_PATH != 0 {
            Some(node_paths.get((parent & FLAG_MASK) as usize).cloned().unwrap_or_else(|| ".".into()))
        } else {
            Some(paths.get(parent as usize).cloned().ok_or_else(|| LoadError::invalid("node before its parent"))?)
        };
        let path = match parent_path.as_deref() {
            None => ".".to_string(),
            Some(".") => node_name.clone(),
            Some(p) => format!("{p}/{node_name}"),
        };
        let mut entry = NodeEntry {
            name: node_name,
            class: (class != TYPE_INSTANTIATED && class >= 0).then(|| name(class)),
            parent: parent_path,
            instance: (instance >= 0).then(|| variants.get((instance & FLAG_MASK) as usize).cloned()).flatten(),
            props: Vec::new(),
        };
        let props = next()?;
        if props < 0 || props as usize > data.len() {
            return Err(LoadError::invalid("bad property count"));
        }
        for _ in 0..props {
            let key = next()?;
            let value = next()?;
            let key = name(key & ((1 << 30) - 1));
            entry.props.push((key, variants.get(value.max(0) as usize).cloned().unwrap_or(Value::Nil)));
        }
        let groups = next()?;
        if groups < 0 || groups as usize > data.len() {
            return Err(LoadError::invalid("bad group count"));
        }
        for _ in 0..groups {
            next()?;
        }
        paths.push(path);
        out.push(entry);
    }
    Ok(out)
}

/*┌─────────────────╮
| Reading values  |
└─────────────────╯*/
//region Reading values

struct Reader<'a> {
    b: &'a [u8],
    pos: usize,
    big: bool,
    real64: bool,
    format: u32,
    strings: Vec<String>,
    internal_ids: Vec<String>,
    named_ids: bool,
}

fn short() -> LoadError {
    LoadError::invalid("binary resource cut short")
}

impl<'a> Reader<'a> {
    fn new(b: &'a [u8]) -> Self {
        Reader {
            b,
            pos: 0,
            big: false,
            real64: false,
            format: 0,
            strings: Vec::new(),
            internal_ids: Vec::new(),
            named_ids: false,
        }
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8], LoadError> {
        let end = self.pos.checked_add(n).ok_or_else(short)?;
        let s = self.b.get(self.pos..end).ok_or_else(short)?;
        self.pos = end;
        Ok(s)
    }

    fn u16(&mut self) -> Result<u16, LoadError> {
        let s: [u8; 2] = self.take(2)?.try_into().map_err(|_| short())?;
        Ok(if self.big { u16::from_be_bytes(s) } else { u16::from_le_bytes(s) })
    }

    fn u32(&mut self) -> Result<u32, LoadError> {
        let s: [u8; 4] = self.take(4)?.try_into().map_err(|_| short())?;
        Ok(if self.big { u32::from_be_bytes(s) } else { u32::from_le_bytes(s) })
    }

    fn u64(&mut self) -> Result<u64, LoadError> {
        let s: [u8; 8] = self.take(8)?.try_into().map_err(|_| short())?;
        Ok(if self.big { u64::from_be_bytes(s) } else { u64::from_le_bytes(s) })
    }

    fn f32(&mut self) -> Result<f32, LoadError> {
        self.u32().map(f32::from_bits)
    }

    fn f64(&mut self) -> Result<f64, LoadError> {
        self.u64().map(f64::from_bits)
    }

    /// A `real_t`: a float or a double, as the file was saved.
    fn real(&mut self) -> Result<f64, LoadError> {
        if self.real64 { self.f64() } else { self.f32().map(f64::from) }
    }

    fn reals(&mut self, n: usize) -> Result<Vec<f64>, LoadError> {
        (0..n).map(|_| self.real()).collect()
    }

    /// A count of items each taking at least `min_size` bytes, checked
    /// against what is left so a corrupt count can't allocate gigabytes.
    fn count(&mut self, min_size: usize) -> Result<usize, LoadError> {
        let n = self.u32()? as usize;
        let left = self.b.len().saturating_sub(self.pos);
        if n.saturating_mul(min_size.max(1)) > left {
            return Err(LoadError::invalid("count larger than the file"));
        }
        Ok(n)
    }

    /// A length-prefixed UTF-8 string (Godot writes a trailing NUL).
    fn string(&mut self) -> Result<String, LoadError> {
        let n = self.count(1)?;
        let s = self.take(n)?;
        let s = s.split(|&c| c == 0).next().unwrap_or(&[]);
        Ok(String::from_utf8_lossy(s).into_owned())
    }

    /// A property or node-path name: an index into the string table, or
    /// inline when its top bit is set.
    fn name(&mut self) -> Result<String, LoadError> {
        let id = self.u32()?;
        if id & 0x8000_0000 != 0 {
            let len = (id & 0x7FFF_FFFF) as usize;
            let s = self.take(len)?;
            return Ok(String::from_utf8_lossy(s.split(|&c| c == 0).next().unwrap_or(&[])).into_owned());
        }
        self.strings.get(id as usize).cloned().ok_or_else(|| LoadError::invalid("bad string index"))
    }

    fn pad(&mut self, len: usize) -> Result<(), LoadError> {
        let extra = (4 - len % 4) % 4;
        self.take(extra).map(|_| ())
    }

    fn variant(&mut self, depth: u32) -> Result<Value, LoadError> {
        if depth > MAX_DEPTH {
            return Err(LoadError::invalid("values nested too deeply"));
        }
        let kind = self.u32()?;
        let math = |r: &mut Self, name: &str, n: usize| -> Result<Value, LoadError> {
            Ok(Value::Math(name.into(), r.reals(n)?))
        };
        let ints = |r: &mut Self, name: &str, n: usize| -> Result<Value, LoadError> {
            let v: Result<Vec<f64>, LoadError> = (0..n).map(|_| r.u32().map(|x| x as i32 as f64)).collect();
            Ok(Value::Math(name.into(), v?))
        };
        Ok(match kind {
            1 => Value::Nil,
            2 => Value::Bool(self.u32()? != 0),
            3 => Value::Int(self.u32()? as i32 as i64),
            4 => Value::Float(self.real()?),
            5 | 44 => Value::Str(self.string()?),
            10 => math(self, "Vector2", 2)?,
            11 => math(self, "Rect2", 4)?,
            12 => math(self, "Vector3", 3)?,
            13 => math(self, "Plane", 4)?,
            14 => math(self, "Quaternion", 4)?,
            15 => math(self, "AABB", 6)?,
            16 => math(self, "Basis", 9)?,
            17 => math(self, "Transform3D", 12)?,
            18 => math(self, "Transform2D", 6)?,
            20 => {
                let v: Result<Vec<f64>, LoadError> = (0..4).map(|_| self.f32().map(f64::from)).collect();
                Value::Math("Color".into(), v?)
            }
            22 => {
                // A NodePath: names, then subnames (with a property name
                // counted in before format 3).
                let names = self.u16()? as usize;
                let sub = self.u16()?;
                let absolute = sub & 0x8000 != 0;
                let mut subs = (sub & 0x7FFF) as usize;
                if self.format < 3 {
                    subs += 1;
                }
                let mut parts = Vec::new();
                for _ in 0..names {
                    parts.push(self.name()?);
                }
                let mut path = parts.join("/");
                for _ in 0..subs {
                    let s = self.name()?;
                    path.push(':');
                    path.push_str(&s);
                }
                Value::Str(if absolute { format!("/{path}") } else { path })
            }
            23 => {
                self.u32()?;
                Value::Nil
            }
            24 => match self.u32()? {
                0 => Value::Nil,
                1 => {
                    // External resource by type and path, the old way.
                    let _kind = self.string()?;
                    Value::Other(format!("path:{}", self.string()?))
                }
                2 => {
                    let index = self.u32()? as usize;
                    let id = if self.named_ids {
                        self.internal_ids.get(index).cloned().ok_or_else(|| LoadError::invalid("bad resource index"))?
                    } else {
                        index.to_string()
                    };
                    Value::Sub(id)
                }
                3 => Value::Ext(self.u32()?.to_string()),
                _ => return Err(LoadError::invalid("bad object reference")),
            },
            25 => Value::Nil,
            26 => {
                let n = (self.u32()? & 0x7FFF_FFFF) as usize;
                if n.saturating_mul(8) > self.b.len().saturating_sub(self.pos) {
                    return Err(LoadError::invalid("dictionary larger than the file"));
                }
                let mut pairs = Vec::with_capacity(n);
                for _ in 0..n {
                    let k = self.variant(depth + 1)?;
                    let v = self.variant(depth + 1)?;
                    pairs.push((k, v));
                }
                Value::Dict(pairs)
            }
            30 => {
                let n = (self.u32()? & 0x7FFF_FFFF) as usize;
                if n.saturating_mul(4) > self.b.len().saturating_sub(self.pos) {
                    return Err(LoadError::invalid("array larger than the file"));
                }
                let mut items = Vec::with_capacity(n);
                for _ in 0..n {
                    items.push(self.variant(depth + 1)?);
                }
                Value::Array(items)
            }
            31 => {
                let n = self.count(1)?;
                let bytes = self.take(n)?.to_vec();
                self.pad(n)?;
                Value::Bytes(bytes)
            }
            32 => {
                let n = self.count(4)?;
                let v: Result<Vec<i64>, LoadError> = (0..n).map(|_| self.u32().map(|x| x as i32 as i64)).collect();
                Value::Ints(v?)
            }
            48 => {
                let n = self.count(8)?;
                let v: Result<Vec<i64>, LoadError> = (0..n).map(|_| self.u64().map(|x| x as i64)).collect();
                Value::Ints(v?)
            }
            33 => {
                let n = self.count(4)?;
                Value::Floats("PackedFloat32Array".into(), self.f32s(n)?)
            }
            49 => {
                let n = self.count(8)?;
                let v: Result<Vec<f32>, LoadError> = (0..n).map(|_| self.f64().map(|x| x as f32)).collect();
                Value::Floats("PackedFloat32Array".into(), v?)
            }
            34 => {
                let n = self.count(4)?;
                let mut v = Vec::with_capacity(n);
                for _ in 0..n {
                    v.push(self.string()?);
                }
                Value::Strings(v)
            }
            35 | 37 | 53 => {
                let (name, per) = match kind {
                    35 => ("PackedVector3Array", 3),
                    37 => ("PackedVector2Array", 2),
                    _ => ("PackedVector4Array", 4),
                };
                let size = if self.real64 { 8 } else { 4 };
                let n = self.count(per * size)?;
                let v: Result<Vec<f32>, LoadError> = (0..n * per).map(|_| self.real().map(|x| x as f32)).collect();
                Value::Floats(name.into(), v?)
            }
            36 => {
                let n = self.count(16)?;
                Value::Floats("PackedColorArray".into(), self.f32s(n * 4)?)
            }
            40 => Value::Int(self.u64()? as i64),
            41 => Value::Float(self.f64()?),
            42 => Value::Other("Callable".into()),
            43 => Value::Other("Signal".into()),
            45 => ints(self, "Vector2i", 2)?,
            46 => ints(self, "Rect2i", 4)?,
            47 => ints(self, "Vector3i", 3)?,
            50 => math(self, "Vector4", 4)?,
            51 => ints(self, "Vector4i", 4)?,
            52 => math(self, "Projection", 16)?,
            _ => return Err(LoadError::invalid(format!("unknown value type {kind}"))),
        })
    }

    fn f32s(&mut self, n: usize) -> Result<Vec<f32>, LoadError> {
        let bytes = self.take(n.checked_mul(4).ok_or_else(short)?)?;
        let big = self.big;
        Ok(bytes
            .chunks_exact(4)
            .map(|c| {
                let a = [c[0], c[1], c[2], c[3]];
                f32::from_bits(if big { u32::from_be_bytes(a) } else { u32::from_le_bytes(a) })
            })
            .collect())
    }
}

//endregion

/*┌─────────────────╮
| Decompression   |
└─────────────────╯*/
//region Decompression

/// Inflates a compressed resource after its `RSCC` magic: a mode, a block
/// size, the total size, each block's compressed size, then the blocks.
fn decompress(b: &[u8], max_bytes: u64) -> Result<Vec<u8>, LoadError> {
    let mut r = Reader::new(b);
    let mode = r.u32()?;
    let block = r.u32()? as usize;
    let total = r.u32()? as u64;
    // Godot writes 4 KB blocks; a block can't expand beyond what zstd or
    // deflate can claim for its size either, so a short file can't ask
    // for gigabytes.
    if block == 0 || block > 16 << 20 {
        return Err(LoadError::invalid("compressed resource with a bad block size"));
    }
    let plausible = (b.len() as u64).saturating_mul(1024).max(64 << 20);
    if total > max_bytes || total > plausible {
        return Err(LoadError::TooLarge(format!("{total} bytes once decompressed")));
    }
    let total = total as usize;
    let blocks = total / block + 1;
    if blocks.saturating_mul(4) > b.len() {
        return Err(LoadError::invalid("block table larger than the file"));
    }
    let sizes: Vec<usize> = (0..blocks).map(|_| r.u32().map(|s| s as usize)).collect::<Result<_, _>>()?;
    let mut out = Vec::with_capacity(total.min(64 << 20));
    for (i, &csize) in sizes.iter().enumerate() {
        let want = if i + 1 == blocks { total - block * i } else { block };
        let src = r.take(csize)?;
        if want == 0 {
            continue;
        }
        let start = out.len();
        match mode {
            0 => fastlz(src, want, &mut out)?,
            1 => out.extend(
                miniz_oxide::inflate::decompress_to_vec_zlib_with_limit(src, want)
                    .map_err(|_| LoadError::invalid("bad deflate block"))?,
            ),
            2 => {
                let data = zstd::bulk::decompress(src, want).map_err(|_| LoadError::invalid("bad zstd block"))?;
                out.extend(data);
            }
            3 => {
                let raw = gzip_body(src).ok_or_else(|| LoadError::invalid("bad gzip block"))?;
                out.extend(
                    miniz_oxide::inflate::decompress_to_vec_with_limit(raw, want)
                        .map_err(|_| LoadError::invalid("bad gzip block"))?,
                );
            }
            4 => return Err(LoadError::Unsupported("Brotli-compressed Godot resource".into())),
            _ => return Err(LoadError::invalid(format!("unknown compression {mode}"))),
        }
        if out.len() - start != want {
            return Err(LoadError::invalid("compressed block of the wrong size"));
        }
    }
    Ok(out)
}

/// The deflate stream inside a gzip member, past its header.
fn gzip_body(b: &[u8]) -> Option<&[u8]> {
    if b.len() < 18 || b[0] != 0x1F || b[1] != 0x8B || b[2] != 8 {
        return None;
    }
    let flags = b[3];
    let mut pos = 10usize;
    if flags & 4 != 0 {
        let n = u16::from_le_bytes([*b.get(pos)?, *b.get(pos + 1)?]) as usize;
        pos += 2 + n;
    }
    for bit in [8u8, 16] {
        if flags & bit != 0 {
            pos += b.get(pos..)?.iter().position(|&c| c == 0)? + 1;
        }
    }
    if flags & 2 != 0 {
        pos += 2;
    }
    b.get(pos..)
}

/// FastLZ decompression (levels 1 and 2, told apart by the first byte), as
/// Godot 3 compressed resources with it by default.
fn fastlz(src: &[u8], want: usize, out: &mut Vec<u8>) -> Result<(), LoadError> {
    let bad = || LoadError::invalid("bad FastLZ block");
    if src.is_empty() {
        return Err(bad());
    }
    let level = (src[0] >> 5) + 1;
    let start = out.len();
    let mut ip = 0usize;
    let mut ctrl = (src[ip] & 31) as usize;
    ip += 1;
    loop {
        if ctrl >= 32 {
            // A back reference.
            let mut len = (ctrl >> 5) - 1;
            let mut ofs = (ctrl & 31) << 8;
            if len == 6 {
                if level == 1 {
                    len += *src.get(ip).ok_or_else(bad)? as usize;
                    ip += 1;
                } else {
                    loop {
                        let c = *src.get(ip).ok_or_else(bad)? as usize;
                        ip += 1;
                        len += c;
                        if c != 255 {
                            break;
                        }
                    }
                }
            }
            let code = *src.get(ip).ok_or_else(bad)? as usize;
            ip += 1;
            let mut distance = ofs + code + 1;
            if level == 2 && code == 255 && ofs == 31 << 8 {
                let hi = *src.get(ip).ok_or_else(bad)? as usize;
                let lo = *src.get(ip + 1).ok_or_else(bad)? as usize;
                ip += 2;
                ofs = (hi << 8) + lo;
                distance = ofs + 8191 + 1;
            }
            let produced = out.len() - start;
            if distance > produced || produced + len + 3 > want {
                return Err(bad());
            }
            let from = out.len() - distance;
            for k in 0..len + 3 {
                let c = out[from + k];
                out.push(c);
            }
        } else {
            // A literal run.
            let n = ctrl + 1;
            let run = src.get(ip..ip + n).ok_or_else(bad)?;
            if out.len() - start + n > want {
                return Err(bad());
            }
            out.extend_from_slice(run);
            ip += n;
        }
        if ip >= src.len() {
            break;
        }
        ctrl = src[ip] as usize;
        ip += 1;
    }
    Ok(())
}

//endregion

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Where `sample` stores its one internal resource's offset.
    pub(crate) const OFFSET_AT: usize = 146;

    /// A tiny binary resource built by hand: an ArrayMesh-typed main
    /// resource with one property of each common kind.
    pub(crate) fn sample() -> Vec<u8> {
        let mut b = b"RSRC".to_vec();
        let u32 = |b: &mut Vec<u8>, v: u32| b.extend_from_slice(&v.to_le_bytes());
        let s = |b: &mut Vec<u8>, t: &str| {
            b.extend_from_slice(&(t.len() as u32 + 1).to_le_bytes());
            b.extend_from_slice(t.as_bytes());
            b.push(0);
        };
        for v in [0, 0, 4, 2, 5] {
            u32(&mut b, v);
        }
        s(&mut b, "ArrayMesh");
        b.extend_from_slice(&0u64.to_le_bytes());
        u32(&mut b, FLAG_NAMED_SCENE_IDS);
        b.extend_from_slice(&0u64.to_le_bytes());
        for _ in 0..11 {
            u32(&mut b, 0);
        }
        u32(&mut b, 2);
        s(&mut b, "size");
        s(&mut b, "data");
        u32(&mut b, 0);
        u32(&mut b, 1);
        s(&mut b, "local://1");
        let offset_at = b.len();
        assert_eq!(offset_at, OFFSET_AT);
        b.extend_from_slice(&0u64.to_le_bytes());
        let offset = b.len() as u64;
        b[offset_at..offset_at + 8].copy_from_slice(&offset.to_le_bytes());
        s(&mut b, "ArrayMesh");
        u32(&mut b, 2);
        u32(&mut b, 0);
        u32(&mut b, 12);
        for f in [1.0f32, 2.0, 3.0] {
            b.extend_from_slice(&f.to_le_bytes());
        }
        u32(&mut b, 1);
        u32(&mut b, 31);
        u32(&mut b, 3);
        b.extend_from_slice(&[7, 8, 9, 0]);
        b
    }

    #[test]
    fn reads_a_hand_made_resource() {
        let f = parse(&sample(), 1 << 20).unwrap();
        assert_eq!(f.kind, "ArrayMesh");
        assert_eq!(f.main_prop("size").and_then(Value::vec3), Some(slerp_scene::glam::Vec3::new(1.0, 2.0, 3.0)));
        assert_eq!(f.main_prop("data").and_then(Value::bytes), Some(&[7u8, 8, 9][..]));
        assert_eq!(header_type(&sample()).as_deref(), Some("ArrayMesh"));
    }

    #[test]
    fn damaged_resources_fail_cleanly() {
        let good = sample();
        for cut in 0..good.len() {
            let _ = parse(&good[..cut], 1 << 20);
        }
        let mut seed = 0x1234_5678u32;
        for _ in 0..2000 {
            let mut bad = good.clone();
            for _ in 0..3 {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                let i = 4 + (seed as usize >> 8) % (bad.len() - 4);
                bad[i] = (seed >> 3) as u8;
            }
            let _ = parse(&bad, 1 << 20);
        }
        // A compressed header claiming more than allowed.
        let mut rscc = b"RSCC".to_vec();
        for v in [2u32, 4096, u32::MAX] {
            rscc.extend_from_slice(&v.to_le_bytes());
        }
        assert!(parse(&rscc, 1 << 20).is_err());
    }

    #[test]
    fn fastlz_level1_round_trip_by_hand() {
        // Literal run "abc", then a match of 4 at distance 3 ("abca").
        let src = [2u8, b'a', b'b', b'c', 0x40, 2];
        let mut out = Vec::new();
        fastlz(&src, 7, &mut out).unwrap();
        assert_eq!(&out, b"abcabca");
    }
}
