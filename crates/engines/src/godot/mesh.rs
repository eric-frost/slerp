//! Godot's meshes: `ArrayMesh` surfaces in every layout Godot has saved
//! (Godot 4.2's split streams with optional 16-bit compression, 4.0's
//! interleaved streams, Godot 3's packed and compressed arrays, and plain
//! arrays as the Blender exporter writes them), and the primitive meshes
//! (box, sphere, capsule, cylinder, plane, quad, prism, torus) with each
//! Godot version's defaults.
//!
//! Godot's front faces wind clockwise, so triangles are reversed here into
//! the counter-clockwise order the rest of Slerp uses.  Vertex layouts
//! follow `RenderingServer::_get_array_from_surface` and
//! `_fix_array_compatibility` in Godot's sources.

use super::variant::{Value, prop};
use crate::shapes;
use slerp_scene::glam::{Mat3, Vec2, Vec3, Vec4};
use slerp_scene::{Primitive, Topology};

/// One surface of a mesh: its geometry and the material it names.
pub(crate) struct Surface {
    pub prim: Primitive,
    pub material: Option<Value>,
}

/// The surfaces of an `ArrayMesh` resource.  Surfaces that can't be read
/// are skipped and counted in the second value.
pub(crate) fn array_mesh(props: &[(String, Value)], max_vertices: usize) -> (Vec<Surface>, usize) {
    let mut out = Vec::new();
    let mut failed = 0;
    // 1. Godot 4: one `_surfaces` array of dictionaries.
    if let Some(surfaces) = prop(props, "_surfaces").and_then(Value::array) {
        for s in surfaces {
            match surface4(s, max_vertices) {
                Some(prim) => out.push(Surface { prim, material: s.get("material").cloned() }),
                None => failed += 1,
            }
        }
        return (out, failed);
    }
    // 2. Godot 3: `surfaces/0`, `surfaces/1`... in order.
    let mut numbered: Vec<(usize, &Value)> = props
        .iter()
        .filter_map(|(k, v)| k.strip_prefix("surfaces/").and_then(|n| n.parse().ok()).map(|n: usize| (n, v)))
        .collect();
    numbered.sort_by_key(|(n, _)| *n);
    for (_, s) in numbered {
        let prim = if s.get("arrays").is_some() { surface_arrays(s, max_vertices) } else { surface3(s, max_vertices) };
        match prim {
            Some(prim) => out.push(Surface { prim, material: s.get("material").cloned() }),
            None => failed += 1,
        }
    }
    (out, failed)
}

/*┌─────────────────╮
| Godot 4         |
└─────────────────╯*/
//region Godot 4

const F_VERTEX: u64 = 1;
const F_NORMAL: u64 = 1 << 1;
const F_TANGENT: u64 = 1 << 2;
const F_COLOR: u64 = 1 << 3;
const F_UV: u64 = 1 << 4;
const F_UV2: u64 = 1 << 5;
const F_CUSTOM0: u64 = 1 << 6;
const F_INDEX: u64 = 1 << 12;
const F_2D: u64 = 1 << 25;
const F_COMPRESS: u64 = 1 << 29;
const VERSION_SHIFT: u64 = 35;

fn le_u16(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes([*b.get(at)?, *b.get(at + 1)?]))
}

fn le_u32(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

fn le_f32(b: &[u8], at: usize) -> Option<f32> {
    le_u32(b, at).map(f32::from_bits)
}

fn oct_decode(x: f32, y: f32) -> Vec3 {
    let f = Vec2::new(x * 2.0 - 1.0, y * 2.0 - 1.0);
    let mut n = Vec3::new(f.x, f.y, 1.0 - f.x.abs() - f.y.abs());
    let t = (-n.z).clamp(0.0, 1.0);
    n.x += if n.x >= 0.0 { -t } else { t };
    n.y += if n.y >= 0.0 { -t } else { t };
    n.normalize_or(Vec3::Y)
}

fn oct_tangent_decode(x: f32, y: f32) -> Vec4 {
    let y = y * 2.0 - 1.0;
    let sign = if y >= 0.0 { 1.0 } else { -1.0 };
    oct_decode(x, y.abs()).extend(sign)
}

fn oct16(v: u32) -> (f32, f32) {
    ((v & 0xFFFF) as f32 / 65535.0, (v >> 16) as f32 / 65535.0)
}

/// Bytes per element of a custom channel's format.
fn custom_size(format: u64, channel: u64) -> usize {
    match (format >> (13 + 3 * channel)) & 7 {
        0..=2 => 4,
        3 => 8,
        4 => 4,
        5 => 8,
        6 => 12,
        _ => 16,
    }
}

fn surface4(s: &Value, max_vertices: usize) -> Option<Primitive> {
    let format = s.get("format")?.int()? as u64;
    let count = usize::try_from(s.get("vertex_count")?.int()?).ok()?;
    if count == 0 || count > max_vertices || format & F_VERTEX == 0 || format & F_2D != 0 {
        return None;
    }
    let vertex = s.get("vertex_data").and_then(Value::bytes).unwrap_or(&[]);
    let attrib = s.get("attribute_data").and_then(Value::bytes).unwrap_or(&[]);
    let compressed = format & F_COMPRESS != 0;
    let version = (format >> VERSION_SHIFT) & 0xFF;

    // 1. Element sizes and offsets as Godot lays them out.
    let pos_size = if compressed { 8 } else { 12 };
    let normal_size = if format & F_NORMAL != 0 { 4 } else { 0 };
    let tangent_size = if format & F_TANGENT != 0 && !compressed { 4 } else { 0 };
    let nt_size = normal_size + tangent_size;
    // Version 1 (Godot 4.0 and 4.1) interleaves position, normal and
    // tangent; version 2 keeps every position first.
    let interleaved = version == 0;
    let stride = vertex.len() / count;
    let pos_at = |i: usize| if interleaved { i * stride } else { i * pos_size };
    let nt_at = |i: usize| if interleaved { i * stride + pos_size } else { count * pos_size + i * nt_size };
    if version == 0 && vertex.len() < count * (pos_size + nt_size) {
        return None;
    }
    let mut a_ofs = 0usize;
    let mut take = |flag: u64, size: usize| -> Option<usize> {
        (format & flag != 0).then(|| {
            let o = a_ofs;
            a_ofs += size;
            o
        })
    };
    let color_at = take(F_COLOR, 4);
    let uv_at = take(F_UV, if compressed { 4 } else { 8 });
    let uv2_at = take(F_UV2, if compressed { 4 } else { 8 });
    for c in 0..4 {
        take(F_CUSTOM0 << c, custom_size(format, c));
    }
    let a_stride = a_ofs;

    // 2. Vertices.
    let aabb = s.get("aabb").and_then(|v| v.math(&["AABB"])).filter(|v| v.len() >= 6);
    let (origin, size) = match aabb {
        Some(v) => (Vec3::new(v[0] as f32, v[1] as f32, v[2] as f32), Vec3::new(v[3] as f32, v[4] as f32, v[5] as f32)),
        None => (Vec3::ZERO, Vec3::ONE),
    };
    let uv_scale = s.get("uv_scale").and_then(|v| v.math(&["Vector4"])).filter(|v| v.len() >= 4);
    let uv_scale = uv_scale.map_or(Vec4::ZERO, |v| Vec4::new(v[0] as f32, v[1] as f32, v[2] as f32, v[3] as f32));
    // The data must hold every vertex before anything is set aside for
    // them.
    if vertex.len() < count.saturating_mul(pos_size + nt_size) || attrib.len() < count.saturating_mul(a_stride) {
        return None;
    }
    let mut p = Primitive::default();
    p.positions.reserve(count);
    for i in 0..count {
        let at = pos_at(i);
        if compressed {
            let q = |k: usize| le_u16(vertex, at + 2 * k).map(|v| v as f32 / 65535.0);
            let unit = Vec3::new(q(0)?, q(1)?, q(2)?);
            p.positions.push(origin + unit * size);
            if normal_size > 0 {
                // Normal and tangent travel as an axis and angle.
                let (ax, ay) = oct16(le_u32(vertex, nt_at(i))?);
                let axis = oct_decode(ax, ay);
                let angle = q(3)?;
                let sign = if angle > 0.5 { 1.0 } else { -1.0 };
                let m = Mat3::from_axis_angle(axis, (angle * 2.0 - 1.0).abs() * std::f32::consts::PI);
                // Godot's basis rows: tangent, binormal, normal.
                p.normals.push(Vec3::new(m.x_axis.z, m.y_axis.z, m.z_axis.z).normalize_or(Vec3::Y));
                p.tangents.push(Vec3::new(m.x_axis.x, m.y_axis.x, m.z_axis.x).normalize_or(Vec3::X).extend(sign));
            }
        } else {
            p.positions.push(Vec3::new(le_f32(vertex, at)?, le_f32(vertex, at + 4)?, le_f32(vertex, at + 8)?));
            if normal_size > 0 {
                let (x, y) = oct16(le_u32(vertex, nt_at(i))?);
                p.normals.push(oct_decode(x, y));
            }
            if tangent_size > 0 {
                let (x, y) = oct16(le_u32(vertex, nt_at(i) + normal_size)?);
                p.tangents.push(oct_tangent_decode(x, y));
            }
        }
        if a_stride > 0 {
            let base = i * a_stride;
            if let Some(o) = color_at {
                let c = attrib.get(base + o..base + o + 4)?;
                p.colors.push(Vec4::new(c[0] as f32, c[1] as f32, c[2] as f32, c[3] as f32) / 255.0);
            }
            for (slot, scale, out) in [
                (uv_at, uv_scale.truncate().truncate(), &mut p.uv0),
                (uv2_at, Vec2::new(uv_scale.z, uv_scale.w), &mut p.uv1),
            ] {
                let Some(o) = slot else { continue };
                let uv = if compressed {
                    let v = Vec2::new(le_u16(attrib, base + o)? as f32, le_u16(attrib, base + o + 2)? as f32) / 65535.0;
                    if scale != Vec2::ZERO { (v - 0.5) * scale } else { v }
                } else {
                    Vec2::new(le_f32(attrib, base + o)?, le_f32(attrib, base + o + 4)?)
                };
                out.push(uv);
            }
        }
    }
    if !p.positions.iter().all(|v| v.is_finite()) {
        return None;
    }

    // 3. Indices and the primitive type.
    let indices = if format & F_INDEX != 0 {
        let data = s.get("index_data").and_then(Value::bytes).unwrap_or(&[]);
        let n = usize::try_from(s.get("index_count").and_then(Value::int).unwrap_or(0)).ok()?;
        read_indices(data, n)?
    } else {
        (0..count as u32).collect()
    };
    finish(p, indices, s.get("primitive").and_then(Value::int).unwrap_or(3), false)
}

/// Indices of 16 or 32 bits, whichever the data's length says.
fn read_indices(data: &[u8], n: usize) -> Option<Vec<u32>> {
    if n == 0 {
        return Some(Vec::new());
    }
    let size = data.len() / n;
    match size {
        2 | 3 => Some((0..n).filter_map(|i| le_u16(data, i * 2)).map(u32::from).collect()),
        s if s >= 4 => Some((0..n).filter_map(|i| le_u32(data, i * 4)).collect()),
        _ => None,
    }
}

/// Turns Godot's primitive type into ours, reverses triangles to
/// counter-clockwise and drops indices out of range.  `old` selects Godot
/// 3's numbering of primitive types.
fn finish(mut p: Primitive, mut indices: Vec<u32>, primitive: i64, old: bool) -> Option<Primitive> {
    let n = p.positions.len() as u32;
    // Godot 3 numbered line loops and fans too; Godot reads them as lines
    // and strips.
    let kind = if old { [0, 1, 2, 1, 3, 4, 4].get(primitive as usize).copied()? } else { primitive };
    match kind {
        0 => p.topology = Topology::Points,
        1 => p.topology = Topology::Lines,
        2 => {
            indices = indices.windows(2).flat_map(|w| [w[0], w[1]]).collect();
            p.topology = Topology::Lines;
        }
        3 => {}
        4 => {
            indices = indices
                .windows(3)
                .enumerate()
                .flat_map(|(i, w)| if i % 2 == 0 { [w[0], w[1], w[2]] } else { [w[1], w[0], w[2]] })
                .collect();
        }
        _ => return None,
    }
    if p.topology == Topology::Triangles {
        indices.truncate(indices.len() / 3 * 3);
        for t in indices.chunks_exact_mut(3) {
            t.swap(1, 2);
        }
        let mut kept = Vec::with_capacity(indices.len());
        for t in indices.chunks_exact(3) {
            if t.iter().all(|&i| i < n) {
                kept.extend_from_slice(t);
            }
        }
        indices = kept;
    } else {
        indices.retain(|&i| i < n);
    }
    p.indices = indices;
    Some(p)
}

//endregion

/*┌─────────────────╮
| Godot 3         |
└─────────────────╯*/
//region Godot 3

const O_VERTEX: u64 = 1;
const O_NORMAL: u64 = 1 << 1;
const O_TANGENT: u64 = 1 << 2;
const O_COLOR: u64 = 1 << 3;
const O_UV: u64 = 1 << 4;
const O_UV2: u64 = 1 << 5;
const O_BONES: u64 = 1 << 6;
const O_WEIGHTS: u64 = 1 << 7;
const O_INDEX: u64 = 1 << 8;
const C_VERTEX: u64 = 1 << 9;
const C_NORMAL: u64 = 1 << 10;
const C_TANGENT: u64 = 1 << 11;
const C_COLOR: u64 = 1 << 12;
const C_UV: u64 = 1 << 13;
const C_UV2: u64 = 1 << 14;
const C_WEIGHTS: u64 = 1 << 16;
const O_2D: u64 = 1 << 18;
const O_16BIT_BONES: u64 = 1 << 19;
const O_OCTAHEDRAL: u64 = 1 << 21;

fn half(b: &[u8], at: usize) -> Option<f32> {
    let h = le_u16(b, at)?;
    let sign = if h & 0x8000 != 0 { -1.0 } else { 1.0 };
    let exp = ((h >> 10) & 0x1F) as i32;
    let man = (h & 0x3FF) as f32;
    Some(
        sign * match exp {
            0 => man * 2f32.powi(-24),
            31 => {
                if man == 0.0 {
                    f32::INFINITY
                } else {
                    f32::NAN
                }
            }
            _ => (1.0 + man / 1024.0) * 2f32.powi(exp - 15),
        },
    )
}

fn snorm8(b: &[u8], at: usize) -> Option<f32> {
    b.get(at).map(|&v| (v as i8 as f32 / 127.0).max(-1.0))
}

fn snorm16(b: &[u8], at: usize) -> Option<f32> {
    le_u16(b, at).map(|v| (v as i16 as f32 / 32767.0).max(-1.0))
}

/// Godot 3's packed `array_data`: one interleaved stream whose element
/// sizes depend on the compression flags.
fn surface3(s: &Value, max_vertices: usize) -> Option<Primitive> {
    let format = s.get("format")?.int()? as u64;
    let count = usize::try_from(s.get("vertex_count")?.int()?).ok()?;
    let data = s.get("array_data").and_then(Value::bytes)?;
    if count == 0 || count > max_vertices || format & O_VERTEX == 0 || format & O_2D != 0 {
        return None;
    }
    let stride = data.len() / count;
    let oct = format & O_OCTAHEDRAL != 0;
    let mut p = Primitive::default();
    let mut ofs = 0usize;
    // 1. Walk the attributes in order, each read for every vertex.
    let has = |f: u64| format & f != 0;
    let at = |i: usize, o: usize| i * stride + o;
    if has(O_VERTEX) {
        for i in 0..count {
            let v = if has(C_VERTEX) {
                Vec3::new(half(data, at(i, ofs))?, half(data, at(i, ofs) + 2)?, half(data, at(i, ofs) + 4)?)
            } else {
                Vec3::new(le_f32(data, at(i, ofs))?, le_f32(data, at(i, ofs) + 4)?, le_f32(data, at(i, ofs) + 8)?)
            };
            p.positions.push(v);
        }
        ofs += if has(C_VERTEX) { 8 } else { 12 };
    }
    if has(O_NORMAL) {
        let small = oct && has(C_NORMAL) && has(O_TANGENT) && has(C_TANGENT);
        for i in 0..count {
            let o = at(i, ofs);
            let n = if oct {
                let (x, y) = if small {
                    (snorm8(data, o)?, snorm8(data, o + 1)?)
                } else {
                    (snorm16(data, o)?, snorm16(data, o + 2)?)
                };
                oct_decode(x * 0.5 + 0.5, y * 0.5 + 0.5)
            } else if has(C_NORMAL) {
                Vec3::new(snorm8(data, o)?, snorm8(data, o + 1)?, snorm8(data, o + 2)?).normalize_or(Vec3::Y)
            } else {
                Vec3::new(le_f32(data, o)?, le_f32(data, o + 4)?, le_f32(data, o + 8)?)
            };
            p.normals.push(n);
        }
        ofs += match (oct, has(C_NORMAL)) {
            (true, _) if has(C_NORMAL) && has(O_TANGENT) && has(C_TANGENT) => 2,
            (true, _) => 4,
            (false, true) => 4,
            (false, false) => 12,
        };
    }
    if has(O_TANGENT) {
        for i in 0..count {
            let o = at(i, ofs);
            let t = if oct {
                let (x, y) = if has(C_TANGENT) {
                    (snorm8(data, o)?, snorm8(data, o + 1)?)
                } else {
                    (snorm16(data, o)?, snorm16(data, o + 2)?)
                };
                oct_tangent_decode(x * 0.5 + 0.5, y * 0.5 + 0.5)
            } else if has(C_TANGENT) {
                Vec4::new(snorm8(data, o)?, snorm8(data, o + 1)?, snorm8(data, o + 2)?, snorm8(data, o + 3)?)
            } else {
                Vec4::new(le_f32(data, o)?, le_f32(data, o + 4)?, le_f32(data, o + 8)?, le_f32(data, o + 12)?)
            };
            p.tangents.push(Vec4::new(t.x, t.y, t.z, if t.w < 0.0 { -1.0 } else { 1.0 }));
        }
        ofs += match (oct, has(C_TANGENT)) {
            (true, true) => 2,
            (true, false) => 4,
            (false, true) => 4,
            (false, false) => 16,
        };
    }
    if has(O_COLOR) {
        for i in 0..count {
            let o = at(i, ofs);
            let c = if has(C_COLOR) {
                let c = data.get(o..o + 4)?;
                Vec4::new(c[0] as f32, c[1] as f32, c[2] as f32, c[3] as f32) / 255.0
            } else {
                Vec4::new(le_f32(data, o)?, le_f32(data, o + 4)?, le_f32(data, o + 8)?, le_f32(data, o + 12)?)
            };
            p.colors.push(c);
        }
        ofs += if has(C_COLOR) { 4 } else { 16 };
    }
    for (flag, compress, slot) in [(O_UV, C_UV, 0), (O_UV2, C_UV2, 1)] {
        if !has(flag) {
            continue;
        }
        for i in 0..count {
            let o = at(i, ofs);
            let uv = if has(compress) {
                Vec2::new(half(data, o)?, half(data, o + 2)?)
            } else {
                Vec2::new(le_f32(data, o)?, le_f32(data, o + 4)?)
            };
            if slot == 0 { p.uv0.push(uv) } else { p.uv1.push(uv) }
        }
        ofs += if has(compress) { 4 } else { 8 };
    }
    // Bones and weights are skipped: Godot scenes draw in their rest pose.
    let _ = (has(O_BONES) && has(O_16BIT_BONES), has(O_WEIGHTS) && has(C_WEIGHTS));
    if !p.positions.iter().all(|v| v.is_finite()) {
        return None;
    }
    let indices = if has(O_INDEX) {
        let idx = s.get("array_index_data").and_then(Value::bytes).unwrap_or(&[]);
        let n = usize::try_from(s.get("index_count").and_then(Value::int).unwrap_or(0)).ok()?;
        read_indices(idx, n)?
    } else {
        (0..count as u32).collect()
    };
    finish(p, indices, s.get("primitive").and_then(Value::int).unwrap_or(4), true)
}

/// A surface given as plain arrays (Godot 2, and the Blender exporter's
/// `.escn` files): Godot 3's nine arrays, or Godot 4's thirteen.
fn surface_arrays(s: &Value, max_vertices: usize) -> Option<Primitive> {
    let arrays = s.get("arrays")?.array()?;
    let (uv2, index) = if arrays.len() >= 13 { (5, 12) } else { (5, 8) };
    let floats = |i: usize| arrays.get(i).and_then(Value::floats).unwrap_or(&[]);
    let positions: Vec<Vec3> = floats(0).chunks_exact(3).map(|c| Vec3::new(c[0], c[1], c[2])).collect();
    if positions.is_empty() || positions.len() > max_vertices || !positions.iter().all(|v| v.is_finite()) {
        return None;
    }
    let n = positions.len();
    let mut p = Primitive { positions, ..Default::default() };
    let normals: Vec<Vec3> = floats(1).chunks_exact(3).map(|c| Vec3::new(c[0], c[1], c[2])).collect();
    if normals.len() == n {
        p.normals = normals;
    }
    let tangents: Vec<Vec4> =
        floats(2).chunks_exact(4).map(|c| Vec4::new(c[0], c[1], c[2], if c[3] < 0.0 { -1.0 } else { 1.0 })).collect();
    if tangents.len() == n {
        p.tangents = tangents;
    }
    let colors: Vec<Vec4> = floats(3).chunks_exact(4).map(|c| Vec4::new(c[0], c[1], c[2], c[3])).collect();
    if colors.len() == n {
        p.colors = colors;
    }
    for (i, out) in [(4, &mut p.uv0), (uv2, &mut p.uv1)] {
        let uv: Vec<Vec2> = floats(i).chunks_exact(2).map(|c| Vec2::new(c[0], c[1])).collect();
        if uv.len() == n {
            *out = uv;
        }
    }
    let indices: Vec<u32> = match arrays.get(index) {
        Some(Value::Ints(v)) => v.iter().filter_map(|&i| u32::try_from(i).ok()).collect(),
        _ => (0..n as u32).collect(),
    };
    let primitive = s.get("primitive").and_then(Value::int).unwrap_or(4);
    // Godot 4 surfaces given this way number primitives the new way.
    finish(p, indices, primitive, arrays.len() < 13)
}

//endregion

/*┌─────────────────╮
| Primitive meshes |
└─────────────────╯*/
//region Primitive meshes

/// A primitive mesh resource of class `kind` with its properties, in Godot
/// 3's sizes when `godot3`.  None for classes that aren't primitive meshes
/// or generate nothing (text and trails).
pub(crate) fn primitive_mesh(kind: &str, props: &[(String, Value)], godot3: bool) -> Option<Primitive> {
    let num = |k: &str, d: f32| prop(props, k).and_then(Value::num).map_or(d, |v| v as f32);
    let int = |k: &str, d: i64| prop(props, k).and_then(Value::int).unwrap_or(d);
    let vec3 = |k: &str, d: Vec3| prop(props, k).and_then(Value::vec3).unwrap_or(d);
    let vec2 = |k: &str, d: Vec2| prop(props, k).and_then(Value::vec2).unwrap_or(d);
    let g3 = |a: f32, b: f32| if godot3 { a } else { b };
    let mut p = match kind {
        "BoxMesh" | "CubeMesh" => shapes::cuboid(vec3("size", Vec3::splat(g3(2.0, 1.0)))),
        "SphereMesh" => {
            let r = num("radius", g3(1.0, 0.5));
            shapes::sphere(
                r,
                num("height", 2.0 * r),
                int("radial_segments", 64),
                int("rings", 32),
                prop(props, "is_hemisphere").is_some_and(Value::truthy),
            )
        }
        "CapsuleMesh" => {
            if godot3 {
                // Godot 3's capsule lies along Z and counts its height
                // between the two half spheres.
                let r = num("radius", 1.0);
                let mut c =
                    shapes::capsule(r, num("mid_height", 1.0) + 2.0 * r, int("radial_segments", 64), int("rings", 8));
                for v in c.positions.iter_mut().chain(c.normals.iter_mut()) {
                    *v = Vec3::new(v.x, -v.z, v.y);
                }
                c
            } else {
                shapes::capsule(num("radius", 0.5), num("height", 2.0), int("radial_segments", 64), int("rings", 8))
            }
        }
        "CylinderMesh" => shapes::cylinder(
            num("top_radius", g3(1.0, 0.5)),
            num("bottom_radius", g3(1.0, 0.5)),
            num("height", 2.0),
            int("radial_segments", 64),
            (prop(props, "cap_top").is_none_or(Value::truthy), prop(props, "cap_bottom").is_none_or(Value::truthy)),
            true,
        ),
        "PlaneMesh" | "QuadMesh" => {
            let quad = kind == "QuadMesh";
            let size = vec2("size", if quad { Vec2::ONE } else { Vec2::splat(2.0) });
            let orientation = if quad && godot3 { 2 } else { int("orientation", if quad { 2 } else { 1 }) };
            let normal = match orientation {
                0 => Vec3::X,
                2 => Vec3::Z,
                _ => Vec3::Y,
            };
            let divisions = int("subdivide_w", 0).max(int("subdivide_d", 0)).clamp(0, 255) as u32 + 1;
            shapes::plane(size, normal, vec3("center_offset", Vec3::ZERO), divisions)
        }
        "PrismMesh" => shapes::prism(num("left_to_right", 0.5), vec3("size", Vec3::splat(g3(2.0, 1.0)))),
        "TorusMesh" => shapes::torus(
            num("inner_radius", 0.5),
            num("outer_radius", 1.0),
            int("rings", 64),
            int("ring_segments", 32),
        ),
        _ => return None,
    };
    if prop(props, "flip_faces").is_some_and(Value::truthy) {
        for t in p.indices.chunks_exact_mut(3) {
            t.swap(1, 2);
        }
        for n in &mut p.normals {
            *n = -*n;
        }
    }
    Some(p)
}

//endregion

#[cfg(test)]
mod tests {
    use super::*;

    fn dict(pairs: Vec<(&str, Value)>) -> Value {
        Value::Dict(pairs.into_iter().map(|(k, v)| (Value::Str(k.into()), v)).collect())
    }

    /// A Godot 4.2 surface: one triangle with uncompressed positions,
    /// octahedral normals and UVs, as the editor saves it.
    #[test]
    fn reads_a_godot4_surface() {
        let mut vertex = Vec::new();
        for v in [[0.0f32, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]] {
            for c in v {
                vertex.extend_from_slice(&c.to_le_bytes());
            }
        }
        // +Z normals: octahedral (0.5, 0.5).
        for _ in 0..3 {
            vertex.extend_from_slice(&(32768u16).to_le_bytes());
            vertex.extend_from_slice(&(32768u16).to_le_bytes());
        }
        let mut attrib = Vec::new();
        for uv in [[0.0f32, 0.0], [1.0, 0.0], [0.0, 1.0]] {
            for c in uv {
                attrib.extend_from_slice(&c.to_le_bytes());
            }
        }
        // Godot winds clockwise: 0, 2, 1 seen from +Z.
        let index: Vec<u8> = [0u16, 2, 1].iter().flat_map(|i| i.to_le_bytes()).collect();
        let s = dict(vec![
            ("format", Value::Int((F_VERTEX | F_NORMAL | F_UV | F_INDEX | (1 << VERSION_SHIFT)) as i64)),
            ("primitive", Value::Int(3)),
            ("vertex_count", Value::Int(3)),
            ("vertex_data", Value::Bytes(vertex)),
            ("attribute_data", Value::Bytes(attrib)),
            ("index_count", Value::Int(3)),
            ("index_data", Value::Bytes(index)),
        ]);
        let p = surface4(&s, 1000).unwrap();
        assert_eq!(p.positions[1], Vec3::X);
        assert!((p.normals[0] - Vec3::Z).length() < 1e-3);
        assert_eq!(p.uv0[2], Vec2::Y);
        let [a, b, c] = [p.indices[0], p.indices[1], p.indices[2]].map(|i| p.positions[i as usize]);
        assert!((b - a).cross(c - a).z > 0.0, "reversed to counter-clockwise");
    }

    #[test]
    fn hostile_surfaces_are_refused() {
        let s = dict(vec![
            ("format", Value::Int((F_VERTEX | F_INDEX) as i64)),
            ("vertex_count", Value::Int(1 << 40)),
            ("vertex_data", Value::Bytes(vec![0; 12])),
        ]);
        assert!(surface4(&s, 1 << 20).is_none());
        let s = dict(vec![
            ("format", Value::Int((F_VERTEX | F_INDEX | (1 << VERSION_SHIFT)) as i64)),
            ("vertex_count", Value::Int(10)),
            ("vertex_data", Value::Bytes(vec![0; 12])),
        ]);
        assert!(surface4(&s, 1 << 20).is_none());
        assert!(primitive_mesh("SphereMesh", &[("radial_segments".into(), Value::Int(i64::MAX))], false).is_some());
    }
}
