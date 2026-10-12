//! Roblox's binary format (`.rbxl`, `.rbxm`, and the models inside a
//! union's `ChildData`), following rbx-dom's specification
//! (<https://dom.rojo.space/binary>, MIT).
//!
//! A file is a 32-byte header and a series of chunks, each LZ4 or Zstandard
//! compressed or stored: `INST` lists the instances of one class, `PROP` one
//! property of every instance of a class, `PRNT` the parent of every
//! instance, `SSTR` the strings properties share.  Values of one property sit
//! together and the bytes of numbers are interleaved (all first bytes, then
//! all second bytes, ...), which compresses well.
//!
//! Every count and length in the file is checked against the bytes that are
//! really there before anything is allocated for it, and the decompressed
//! size of all chunks together is held to a budget, so a damaged or hostile
//! file is refused without ever claiming memory it doesn't fill.
//! (rbx-dom's own reader trusts the header's counts and aborts the process
//! on an absurd one.)  Only the chunks of properties Slerp draws with are
//! decoded; the rest are skipped after their header.

use crate::Budget;
use crate::dom::{CFrame, Dom, Instance, Key, Props, Value};
use glam::{Mat3, Vec3};
use slerp_scene::LoadError;
use std::borrow::Cow;
use std::collections::HashMap;

/// The first 14 bytes of every binary file.
pub(crate) const MAGIC: &[u8] = b"<roblox!\x89\xff\x0d\x0a\x1a\x0a";

/// The most bytes a single name kept for the scene: names are shown in the
/// inspector, and a megabyte-long one is either damage or mischief.
const MAX_NAME: usize = 256;

pub(crate) fn read(bytes: &[u8], budget: &mut Budget) -> Result<Dom, LoadError> {
    // 1. The header: magic, signature, version 0, two counts, eight zero
    //    bytes.  The counts are only hints and are never trusted for sizes.
    if bytes.len() < 32 || !bytes.starts_with(MAGIC) {
        return Err(LoadError::invalid("not a binary Roblox file"));
    }
    let version = u16::from_le_bytes([bytes[14], bytes[15]]);
    if version != 0 {
        return Err(LoadError::Unsupported(format!("binary Roblox format version {version}")));
    }
    let mut reader = Reader {
        dom: Dom::default(),
        types: HashMap::new(),
        referents: HashMap::new(),
        class_names: HashMap::new(),
        prnt: false,
    };
    let mut at = 32usize;
    let mut ended = false;
    // 2. The chunks, until `END`.
    while at < bytes.len() {
        let header = bytes.get(at..at + 16).ok_or_else(|| LoadError::invalid("truncated chunk header"))?;
        let name: [u8; 4] = header[0..4].try_into().unwrap_or_default();
        let compressed = u32::from_le_bytes(header[4..8].try_into().unwrap_or_default()) as usize;
        let length = u32::from_le_bytes(header[8..12].try_into().unwrap_or_default()) as usize;
        at += 16;
        let stored = if compressed == 0 { length } else { compressed };
        let body = bytes.get(at..at.saturating_add(stored)).ok_or_else(|| LoadError::invalid("truncated chunk"))?;
        at += stored;
        if &name == b"END\0" {
            ended = true;
            break;
        }
        // Chunks Slerp has no use for are skipped without decompressing.
        if !matches!(&name, b"INST" | b"PROP" | b"PRNT" | b"SSTR") {
            continue;
        }
        let data = decompress(body, compressed, length, budget)?;
        let mut c = Cursor { data: &data, at: 0 };
        match &name {
            b"INST" => reader.inst(&mut c, budget)?,
            b"PROP" => reader.prop(&mut c)?,
            b"PRNT" => reader.prnt(&mut c)?,
            b"SSTR" => reader.sstr(&mut c)?,
            _ => {}
        }
    }
    let mut dom = reader.dom;
    if !ended {
        dom.warnings.push("the file ends without its END chunk; it may be cut short".into());
    }
    if !reader.prnt && dom.instances.len() > 1 {
        dom.warnings.push("the file has no parent chunk; every instance is shown at the top".into());
    }
    dom.link();
    Ok(dom)
}

/// A chunk's data, decompressed when it was compressed, within the budget.
fn decompress<'a>(
    body: &'a [u8],
    compressed: usize,
    length: usize,
    budget: &mut Budget,
) -> Result<Cow<'a, [u8]>, LoadError> {
    if compressed == 0 {
        return Ok(Cow::Borrowed(body));
    }
    budget.take_bytes(length as u64)?;
    let out = if body.starts_with(&[0x28, 0xb5, 0x2f, 0xfd]) {
        zstd::bulk::decompress(body, length).map_err(|e| LoadError::invalid(format!("Zstandard chunk: {e}")))?
    } else {
        lz4_flex::block::decompress(body, length).map_err(|e| LoadError::invalid(format!("LZ4 chunk: {e}")))?
    };
    if out.len() != length {
        return Err(LoadError::invalid("a chunk decompressed to the wrong size"));
    }
    Ok(Cow::Owned(out))
}

/// The state of one file being read.
struct Reader {
    dom: Dom,
    /// The instances of each class id from `INST` chunks, in file order.
    types: HashMap<u32, Vec<usize>>,
    /// Instance index by the file's referent number.
    referents: HashMap<i32, usize>,
    class_names: HashMap<String, u32>,
    prnt: bool,
}

impl Reader {
    /// `INST`: a class id, its name, a service flag and the referents of
    /// its instances.
    fn inst(&mut self, c: &mut Cursor, budget: &mut Budget) -> Result<(), LoadError> {
        let id = c.u32()?;
        let class = c.string()?;
        let _service = c.u8()?;
        let count = c.u32()? as usize;
        if self.types.contains_key(&id) {
            return Err(LoadError::invalid(format!("class id {id} is defined twice")));
        }
        let refs = c.referents(count)?;
        budget.take_instances(count)?;
        let class = String::from_utf8_lossy(class);
        let class = self.dom.class_index(&class, &mut self.class_names);
        let mut list = Vec::with_capacity(count);
        for r in refs {
            let index = self.dom.instances.len();
            if self.referents.insert(r, index).is_some() {
                return Err(LoadError::invalid(format!("referent {r} is used twice")));
            }
            self.dom.instances.push(Instance {
                class,
                name: String::new(),
                parent: None,
                children: Vec::new(),
                props: Props::default(),
            });
            list.push(index);
        }
        self.types.insert(id, list);
        Ok(())
    }

    /// `PROP`: one property of every instance of a class.
    fn prop(&mut self, c: &mut Cursor) -> Result<(), LoadError> {
        let id = c.u32()?;
        let name = c.string()?;
        // A chunk that ends after the name has no type: Roblox skips it.
        let Ok(ty) = c.u8() else { return Ok(()) };
        let Some(list) = self.types.get(&id) else {
            return Err(LoadError::invalid(format!("a property names class id {id}, which isn't defined")));
        };
        let n = list.len();
        if name == b"Name" {
            if ty != 0x01 {
                return Ok(());
            }
            for &i in list {
                let s = c.bytes_string()?;
                let s = &s[..s.len().min(MAX_NAME)];
                self.dom.instances[i].name = String::from_utf8_lossy(s).into_owned();
            }
            return Ok(());
        }
        let Some(key) = std::str::from_utf8(name).ok().and_then(Key::from_name) else { return Ok(()) };
        let values = values(c, ty, n)?;
        let Some(values) = values else { return Ok(()) };
        for (&i, v) in list.iter().zip(values) {
            self.dom.instances[i].props.set(key, v);
        }
        Ok(())
    }

    /// `PRNT`: child and parent referents, pairwise.
    fn prnt(&mut self, c: &mut Cursor) -> Result<(), LoadError> {
        let version = c.u8()?;
        if version != 0 {
            return Err(LoadError::Unsupported(format!("parent chunk version {version}")));
        }
        let count = c.u32()? as usize;
        let children = c.referents(count)?;
        let parents = c.referents(count)?;
        self.prnt = true;
        for (child, parent) in children.into_iter().zip(parents) {
            let Some(&child) = self.referents.get(&child) else { continue };
            self.dom.order.push(child);
            if parent == -1 {
                if !self.dom.roots.contains(&child) {
                    self.dom.roots.push(child);
                }
                continue;
            }
            // A parent the file never defined leaves the instance at the top.
            self.dom.instances[child].parent = self.referents.get(&parent).copied();
        }
        Ok(())
    }

    /// `SSTR`: the shared strings, each after an MD5 hash nothing checks.
    fn sstr(&mut self, c: &mut Cursor) -> Result<(), LoadError> {
        let version = c.u32()?;
        if version != 0 {
            return Err(LoadError::Unsupported(format!("shared string chunk version {version}")));
        }
        let count = c.u32()? as usize;
        // Each entry takes at least 20 bytes, which bounds the count.
        if count > c.remaining() / 20 {
            return Err(LoadError::invalid("more shared strings than the chunk holds"));
        }
        for _ in 0..count {
            c.take(16)?;
            let s = c.bytes_string()?;
            self.dom.shared.push(s.to_vec());
        }
        Ok(())
    }
}

/// Decodes `n` values of type `ty`, or `None` for types Slerp doesn't use.
fn values(c: &mut Cursor, ty: u8, n: usize) -> Result<Option<Vec<Value>>, LoadError> {
    Ok(Some(match ty {
        // String: length and bytes each.
        0x01 => {
            let mut out = Vec::with_capacity(n.min(c.remaining() / 4));
            for _ in 0..n {
                out.push(Value::Bytes(c.bytes_string()?.to_vec()));
            }
            out
        }
        // Bool: a byte each.
        0x02 => c.take(n)?.iter().map(|&b| Value::Bool(b != 0)).collect(),
        // Int32: interleaved, zigzag.
        0x03 => c.interleaved::<4>(n)?.map(|b| Value::Int(unzigzag(u32::from_be_bytes(b)) as i64)).collect(),
        // Float32 in Roblox's rotated layout.
        0x04 => c.interleaved::<4>(n)?.map(|b| Value::Float(rbx_float(b))).collect(),
        // Float64, plain little-endian.
        0x05 => c
            .take(n.checked_mul(8).ok_or_else(overflow)?)?
            .chunks_exact(8)
            .map(|b| Value::Float(f64::from_le_bytes(b.try_into().unwrap_or_default()) as f32))
            .collect(),
        // BrickColor and Enum: interleaved big-endian u32.
        0x0b | 0x12 => c.interleaved::<4>(n)?.map(|b| Value::Int(u32::from_be_bytes(b) as i64)).collect(),
        // Color3: three float arrays.
        0x0c => vectors(c, n)?.into_iter().map(Value::Color3).collect(),
        // Vector3: three float arrays.
        0x0e => vectors(c, n)?.into_iter().map(Value::Vector3).collect(),
        // CFrame: rotation ids (and matrices), then positions.
        0x10 => cframes(c, n)?.into_iter().map(Value::CFrame).collect(),
        // Color3uint8: all reds, all greens, all blues.
        0x1a => {
            let rgb = c.take(n.checked_mul(3).ok_or_else(overflow)?)?;
            (0..n).map(|i| Value::Color3u8([rgb[i], rgb[n + i], rgb[2 * n + i]])).collect()
        }
        // SharedString: interleaved indices into the SSTR chunk.
        0x1c => c.interleaved::<4>(n)?.map(|b| Value::Shared(u32::from_be_bytes(b))).collect(),
        // Content: a source type each, then the URIs of those that are URIs.
        0x22 => {
            let kinds: Vec<i32> = c.interleaved::<4>(n)?.map(|b| unzigzag(u32::from_be_bytes(b))).collect();
            let uri_count = c.u32()? as usize;
            if uri_count > c.remaining() / 4 {
                return Err(LoadError::invalid("more content URIs than the chunk holds"));
            }
            let mut uris = Vec::with_capacity(uri_count);
            for _ in 0..uri_count {
                uris.push(String::from_utf8_lossy(c.bytes_string()?).into_owned());
            }
            let mut uris = uris.into_iter();
            kinds
                .into_iter()
                .map(|k| match k {
                    1 => Value::Text(uris.next().unwrap_or_default()),
                    _ => Value::Text(String::new()),
                })
                .collect()
        }
        _ => return Ok(None),
    }))
}

fn overflow() -> LoadError {
    LoadError::invalid("a count overflows")
}

/// `n` Vector3 or Color3 values: three interleaved float arrays.
fn vectors(c: &mut Cursor, n: usize) -> Result<Vec<Vec3>, LoadError> {
    let x: Vec<f32> = c.interleaved::<4>(n)?.map(rbx_float).collect();
    let y: Vec<f32> = c.interleaved::<4>(n)?.map(rbx_float).collect();
    let z: Vec<f32> = c.interleaved::<4>(n)?.map(rbx_float).collect();
    Ok((0..n).map(|i| Vec3::new(x[i], y[i], z[i])).collect())
}

/// `n` CFrames: per value a rotation id (and nine floats when it is 0),
/// then the positions as a Vector3 array.
fn cframes(c: &mut Cursor, n: usize) -> Result<Vec<CFrame>, LoadError> {
    if n > c.remaining() {
        return Err(LoadError::invalid("more CFrames than the chunk holds"));
    }
    let mut rotations = Vec::with_capacity(n);
    for _ in 0..n {
        let id = c.u8()?;
        let m = if id == 0 {
            let f = c.take(36)?;
            let r: [f32; 9] =
                std::array::from_fn(|i| f32::from_le_bytes(f[i * 4..i * 4 + 4].try_into().unwrap_or_default()));
            Mat3::from_cols(Vec3::new(r[0], r[3], r[6]), Vec3::new(r[1], r[4], r[7]), Vec3::new(r[2], r[5], r[8]))
        } else {
            basic_rotation(id).unwrap_or(Mat3::IDENTITY)
        };
        rotations.push(m);
    }
    let positions = vectors(c, n)?;
    Ok(rotations.into_iter().zip(positions).map(|(rotation, position)| CFrame { position, rotation }).collect())
}

/// One of the 24 axis-aligned rotations a CFrame stores as an id: `id - 1`
/// is `6 * a + b`, where `a` and `b` are the normal ids (right, top, back,
/// left, bottom, front) of the rotation's first and second columns.
pub(crate) fn basic_rotation(id: u8) -> Option<Mat3> {
    let k = id.checked_sub(1)?;
    let (a, b) = (k / 6, k % 6);
    if a >= 6 || a % 3 == b % 3 {
        return None;
    }
    let axis = |n: u8| {
        let v = [Vec3::X, Vec3::Y, Vec3::Z][(n % 3) as usize];
        if n >= 3 { -v } else { v }
    };
    let (x, y) = (axis(a), axis(b));
    Some(Mat3::from_cols(x, y, x.cross(y)))
}

/// Roblox's float layout moves the sign bit to the bottom.
fn rbx_float(b: [u8; 4]) -> f32 {
    f32::from_bits(u32::from_be_bytes(b).rotate_right(1))
}

fn unzigzag(x: u32) -> i32 {
    ((x >> 1) as i32) ^ -((x & 1) as i32)
}

/// Reads a chunk's bytes, every read checked.
struct Cursor<'a> {
    data: &'a [u8],
    at: usize,
}

impl<'a> Cursor<'a> {
    fn remaining(&self) -> usize {
        self.data.len() - self.at
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8], LoadError> {
        let end = self
            .at
            .checked_add(n)
            .filter(|&e| e <= self.data.len())
            .ok_or_else(|| LoadError::invalid("a chunk is shorter than its contents"))?;
        let s = &self.data[self.at..end];
        self.at = end;
        Ok(s)
    }

    fn u8(&mut self) -> Result<u8, LoadError> {
        Ok(self.take(1)?[0])
    }

    fn u32(&mut self) -> Result<u32, LoadError> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap_or_default()))
    }

    /// A length-prefixed string's bytes.
    fn bytes_string(&mut self) -> Result<&'a [u8], LoadError> {
        let n = self.u32()? as usize;
        self.take(n)
    }

    fn string(&mut self) -> Result<&'a [u8], LoadError> {
        self.bytes_string()
    }

    /// `n` interleaved values of `N` bytes each.
    fn interleaved<const N: usize>(&mut self, n: usize) -> Result<impl Iterator<Item = [u8; N]> + 'a, LoadError> {
        let bytes = self.take(n.checked_mul(N).ok_or_else(overflow)?)?;
        Ok((0..n).map(move |i| std::array::from_fn(|k| bytes[k * n + i])))
    }

    /// `n` referents: interleaved zigzag numbers, each added to the one
    /// before.
    fn referents(&mut self, n: usize) -> Result<Vec<i32>, LoadError> {
        let mut last = 0i32;
        Ok(self
            .interleaved::<4>(n)?
            .map(|b| {
                last = last.wrapping_add(unzigzag(u32::from_be_bytes(b)));
                last
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The ids rbx-dom's specification lists, checked against the angles it
    /// gives (applied Y, then X, then Z).
    #[test]
    fn basic_rotations_match_the_specification() {
        let table: [(u8, [f32; 3]); 24] = [
            (0x02, [0., 0., 0.]),
            (0x03, [90., 0., 0.]),
            (0x05, [0., 180., 180.]),
            (0x06, [-90., 0., 0.]),
            (0x07, [0., 180., 90.]),
            (0x09, [0., 90., 90.]),
            (0x0a, [0., 0., 90.]),
            (0x0c, [0., -90., 90.]),
            (0x0d, [-90., -90., 0.]),
            (0x0e, [0., -90., 0.]),
            (0x10, [90., -90., 0.]),
            (0x11, [0., 90., 180.]),
            (0x14, [0., 180., 0.]),
            (0x15, [-90., -180., 0.]),
            (0x17, [0., 0., 180.]),
            (0x18, [90., 180., 0.]),
            (0x19, [0., 0., -90.]),
            (0x1b, [0., -90., -90.]),
            (0x1c, [0., -180., -90.]),
            (0x1e, [0., 90., -90.]),
            (0x1f, [90., 90., 0.]),
            (0x20, [0., 90., 0.]),
            (0x22, [-90., 90., 0.]),
            (0x23, [0., -90., 180.]),
        ];
        for (id, [x, y, z]) in table {
            let r = |d: f32| d.to_radians();
            let expected = Mat3::from_rotation_y(r(y)) * Mat3::from_rotation_x(r(x)) * Mat3::from_rotation_z(r(z));
            let got = basic_rotation(id).unwrap_or_else(|| panic!("id {id:#x}"));
            assert!(got.abs_diff_eq(expected, 1e-5), "id {id:#x}: {got} vs {expected}");
        }
        assert!(basic_rotation(0).is_none() && basic_rotation(1).is_none() && basic_rotation(37).is_none());
    }

    #[test]
    fn roblox_floats_and_zigzag() {
        assert_eq!(rbx_float([0x7c, 0x40, 0x00, 0x01]), -0.15625);
        assert_eq!(unzigzag(0), 0);
        assert_eq!(unzigzag(1), -1);
        assert_eq!(unzigzag(2), 1);
        assert_eq!(unzigzag(u32::MAX), i32::MIN);
    }
}
