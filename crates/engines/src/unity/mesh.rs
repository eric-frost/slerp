//! Unity's meshes that live in YAML: the built-in cube, sphere, capsule,
//! cylinder, plane and quad, and `Mesh` objects serialized in a scene,
//! prefab or `.asset` file (ProBuilder's, procedural ones saved by tools)
//! with their vertex streams as hexadecimal text.
//!
//! Everything is converted from Unity's left-handed space to Slerp's
//! right-handed one by mirroring Z.  A camera mirrored along sees the same
//! picture, so Unity's clockwise front faces still look clockwise and are
//! reversed into Slerp's counter-clockwise order.  Texture coordinates flip
//! from Unity's bottom-left origin to the top-left one; with the mirror,
//! that leaves the tangents' handedness as it was.

use super::yaml::Yaml;
use crate::shapes;
use slerp_scene::glam::{Vec2, Vec3, Vec4};
use slerp_scene::{Primitive, Topology};

/// The GUID of Unity's built-in "extra" resources, which hold the
/// primitive meshes.
pub(crate) const BUILTIN_EXTRA: &str = "0000000000000000e000000000000000";

/// A built-in mesh by its file id, in Slerp's space.
pub(crate) fn builtin(file_id: i64) -> Option<(&'static str, Primitive)> {
    Some(match file_id {
        10202 => ("Cube", shapes::cuboid(Vec3::ONE)),
        10206 => ("Cylinder", shapes::cylinder(0.5, 0.5, 2.0, 20, (true, true), true)),
        10207 => ("Sphere", shapes::sphere(0.5, 1.0, 24, 16, false)),
        10208 => ("Capsule", shapes::capsule(0.5, 2.0, 24, 8)),
        10209 => ("Plane", shapes::plane(Vec2::splat(10.0), Vec3::Y, Vec3::ZERO, 10)),
        // Unity's quad faces -Z, towards a camera looking down +Z; mirrored
        // into Slerp's space it faces +Z.
        10210 => ("Quad", shapes::plane(Vec2::ONE, Vec3::Z, Vec3::ZERO, 1)),
        _ => return None,
    })
}

/// Decodes hexadecimal text, as Unity writes binary blobs in YAML.
fn hex(s: &str) -> Option<Vec<u8>> {
    let s = s.trim();
    if !s.len().is_multiple_of(2) {
        return None;
    }
    let digit = |c: u8| -> Option<u8> {
        match c {
            b'0'..=b'9' => Some(c - b'0'),
            b'a'..=b'f' => Some(c - b'a' + 10),
            b'A'..=b'F' => Some(c - b'A' + 10),
            _ => None,
        }
    };
    s.as_bytes().chunks_exact(2).map(|p| Some(digit(p[0])? << 4 | digit(p[1])?)).collect()
}

fn half(h: u16) -> f32 {
    let sign = if h & 0x8000 != 0 { -1.0 } else { 1.0 };
    let exp = ((h >> 10) & 0x1F) as i32;
    let man = (h & 0x3FF) as f32;
    sign * match exp {
        0 => man * 2f32.powi(-24),
        31 => f32::NAN,
        _ => (1.0 + man / 1024.0) * 2f32.powi(exp - 15),
    }
}

/// Reads component `k` of a vertex attribute in Unity's
/// `VertexAttributeFormat` (2019 and later).
fn component(data: &[u8], at: usize, format: i64, k: usize) -> Option<f32> {
    let size = format_size(format)?;
    let o = at + k * size;
    let b = data.get(o..o + size)?;
    Some(match format {
        0 => f32::from_le_bytes(b.try_into().ok()?),
        1 => half(u16::from_le_bytes(b.try_into().ok()?)),
        2 => b[0] as f32 / 255.0,
        3 => (b[0] as i8 as f32 / 127.0).max(-1.0),
        4 => u16::from_le_bytes(b.try_into().ok()?) as f32 / 65535.0,
        5 => (i16::from_le_bytes(b.try_into().ok()?) as f32 / 32767.0).max(-1.0),
        6 => b[0] as f32,
        7 => b[0] as i8 as f32,
        8 => u16::from_le_bytes(b.try_into().ok()?) as f32,
        9 => i16::from_le_bytes(b.try_into().ok()?) as f32,
        10 => u32::from_le_bytes(b.try_into().ok()?) as f32,
        11 => i32::from_le_bytes(b.try_into().ok()?) as f32,
        _ => return None,
    })
}

fn format_size(format: i64) -> Option<usize> {
    Some(match format {
        0 | 10 | 11 => 4,
        1 | 4 | 5 | 8 | 9 => 2,
        2 | 3 | 6 | 7 => 1,
        _ => return None,
    })
}

/// A serialized `Mesh` object, one primitive per sub-mesh, in Slerp's
/// space; None when it can't be read (or is stored compressed).
pub(crate) fn decode(mesh: &Yaml, max_vertices: usize) -> Result<Vec<Primitive>, String> {
    if mesh.get("m_MeshCompression").i64().unwrap_or(0) != 0 {
        return Err("compressed meshes aren't read".into());
    }
    let vd = mesh.get("m_VertexData");
    let count = usize::try_from(vd.get("m_VertexCount").i64().unwrap_or(0)).map_err(|_| "bad vertex count")?;
    if count == 0 {
        return Err("no vertices".into());
    }
    if count > max_vertices {
        return Err("too many vertices".into());
    }
    let data = hex(vd.get("_typelessdata").str().unwrap_or("")).ok_or("bad vertex data")?;
    let channels = vd.get("m_Channels").seq();
    if mesh.get("serializedVersion").i64().unwrap_or(0) < 9 && channels.len() < 14 {
        return Err("mesh from a Unity version too old to read".into());
    }

    // 1. Each stream's stride and start: streams follow one another, each
    //    starting on a 16-byte boundary.
    let ch: Vec<(usize, usize, i64, usize)> = channels
        .iter()
        .map(|c| {
            let get = |k: &str| c.get(k).i64().unwrap_or(0);
            (
                get("stream").clamp(0, 3) as usize,
                get("offset").max(0) as usize,
                get("format"),
                get("dimension").clamp(0, 4) as usize,
            )
        })
        .collect();
    let mut strides = [0usize; 4];
    for &(stream, offset, format, dim) in &ch {
        if dim > 0 {
            let end = offset + format_size(format).ok_or("bad vertex format")? * dim;
            strides[stream] = strides[stream].max(end);
        }
    }
    let mut starts = [0usize; 4];
    let mut at = 0usize;
    for s in 0..4 {
        starts[s] = at;
        let stride = strides[s].div_ceil(4) * 4;
        strides[s] = stride;
        at = (at + stride * count).div_ceil(16) * 16;
    }
    let read = |channel: usize, i: usize| -> Option<Vec4> {
        let &(stream, offset, format, dim) = ch.get(channel)?;
        if dim == 0 {
            return None;
        }
        let base = starts[stream] + i * strides[stream] + offset;
        let mut v = [0.0f32, 0.0, 0.0, 1.0];
        for (k, slot) in v.iter_mut().enumerate().take(dim) {
            *slot = component(&data, base, format, k)?;
        }
        Some(Vec4::from_array(v))
    };

    // 2. Vertices, mirrored.
    let mut base = Primitive::default();
    for i in 0..count {
        let p = read(0, i).ok_or("vertex data cut short")?;
        base.positions.push(Vec3::new(p.x, p.y, -p.z));
        if let Some(n) = read(1, i) {
            base.normals.push(Vec3::new(n.x, n.y, -n.z));
        }
        if let Some(t) = read(2, i) {
            base.tangents.push(Vec4::new(t.x, t.y, -t.z, if t.w < 0.0 { -1.0 } else { 1.0 }));
        }
        if let Some(c) = read(3, i) {
            base.colors.push(c);
        }
        if let Some(uv) = read(4, i) {
            base.uv0.push(Vec2::new(uv.x, 1.0 - uv.y));
        }
        if let Some(uv) = read(5, i) {
            base.uv1.push(Vec2::new(uv.x, 1.0 - uv.y));
        }
    }
    if !base.positions.iter().all(|p| p.is_finite()) {
        return Err("vertex positions aren't numbers".into());
    }
    for v in [&mut base.normals] {
        if v.len() != count {
            v.clear();
        }
    }
    if base.tangents.len() != count {
        base.tangents.clear();
    }

    // 3. Indices, split by sub-mesh.
    let indices = hex(mesh.get("m_IndexBuffer").str().unwrap_or("")).ok_or("bad index data")?;
    let wide = mesh.get("m_IndexFormat").i64().unwrap_or(0) == 1;
    let size = if wide { 4 } else { 2 };
    let mut out = Vec::new();
    for sub in mesh.get("m_SubMeshes").seq() {
        let first = sub.get("firstByte").i64().unwrap_or(0).max(0) as usize;
        let n = sub.get("indexCount").i64().unwrap_or(0).max(0) as usize;
        let base_vertex = sub.get("baseVertex").i64().unwrap_or(0).max(0) as u32;
        let topology = sub.get("topology").i64().unwrap_or(0);
        let mut idx: Vec<u32> = Vec::with_capacity(n.min(indices.len() / size));
        for k in 0..n {
            let o = first + k * size;
            let Some(b) = indices.get(o..o + size) else { break };
            let v = if wide {
                u32::from_le_bytes(b.try_into().unwrap_or([0; 4]))
            } else {
                u16::from_le_bytes([b[0], b[1]]) as u32
            };
            idx.push(v.saturating_add(base_vertex));
        }
        let mut p = Primitive { indices: Vec::new(), ..base.clone() };
        match topology {
            0 => {
                for t in idx.chunks_exact(3).filter(|t| t.iter().all(|&i| (i as usize) < count)) {
                    p.indices.extend_from_slice(&[t[0], t[2], t[1]]);
                }
            }
            // Quads become two triangles each.
            2 => {
                for q in idx.chunks_exact(4).filter(|q| q.iter().all(|&i| (i as usize) < count)) {
                    p.indices.extend_from_slice(&[q[0], q[2], q[1], q[0], q[3], q[2]]);
                }
            }
            3 => {
                p.topology = Topology::Lines;
                p.indices = idx.into_iter().filter(|&i| (i as usize) < count).collect();
            }
            4 => {
                p.topology = Topology::Lines;
                p.indices = idx.windows(2).flat_map(|w| [w[0], w[1]]).filter(|&i| (i as usize) < count).collect();
            }
            5 => {
                p.topology = Topology::Points;
                p.indices = idx.into_iter().filter(|&i| (i as usize) < count).collect();
            }
            _ => continue,
        }
        out.push(p);
    }
    if out.is_empty() {
        return Err("no sub-meshes".into());
    }
    Ok(out)
}

/// The GUID of ProBuilder's `ProBuilderMesh` script, whose component
/// holds a mesh's editable form.
pub(crate) const PROBUILDER_SCRIPT: &str = "8233d90336aea43098adf6dbabd606a2";

/// A ProBuilder mesh rebuilt from its `ProBuilderMesh` component, which is
/// all a prefab keeps of it (ProBuilder makes the Mesh when the prefab
/// loads).  One primitive per sub-mesh, faceted, with the sub-mesh index of
/// each.
pub(crate) fn probuilder(pb: &Yaml, max_vertices: usize) -> Result<Vec<(usize, Primitive)>, String> {
    let v3 = |v: &Yaml| {
        Vec3::new(v.get("x").f32().unwrap_or(0.0), v.get("y").f32().unwrap_or(0.0), -v.get("z").f32().unwrap_or(0.0))
    };
    let positions: Vec<Vec3> = pb.get("m_Positions").seq().iter().map(v3).collect();
    if positions.is_empty() {
        return Err("no ProBuilder vertices".into());
    }
    if positions.len() > max_vertices {
        return Err("too many vertices".into());
    }
    let uvs: Vec<Vec2> = pb
        .get("m_Textures0")
        .seq()
        .iter()
        .map(|v| Vec2::new(v.get("x").f32().unwrap_or(0.0), 1.0 - v.get("y").f32().unwrap_or(0.0)))
        .collect();
    let mut subs: Vec<(usize, Primitive)> = Vec::new();
    for face in pb.get("m_Faces").seq() {
        let Some(bytes) = face.get("m_Indexes").str().and_then(hex) else { continue };
        let sub = face.get("m_SubmeshIndex").i64().unwrap_or(0).clamp(0, 255) as usize;
        let at = match subs.iter().position(|(s, _)| *s == sub) {
            Some(i) => i,
            None => {
                subs.push((sub, Primitive::default()));
                subs.len() - 1
            }
        };
        let p = &mut subs[at].1;
        let idx: Vec<usize> =
            bytes.chunks_exact(4).map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]) as usize).collect();
        for t in idx.chunks_exact(3) {
            if t.iter().any(|&i| i >= positions.len()) || p.positions.len() + 3 > max_vertices {
                continue;
            }
            // Faceted, and reversed from Unity's clockwise order.
            let [a, b, c] = [t[0], t[2], t[1]];
            let n = (positions[b] - positions[a]).cross(positions[c] - positions[a]).normalize_or(Vec3::Y);
            for i in [a, b, c] {
                p.indices.push(p.positions.len() as u32);
                p.positions.push(positions[i]);
                p.normals.push(n);
                if uvs.len() == positions.len() {
                    p.uv0.push(uvs[i]);
                }
            }
        }
    }
    subs.retain(|(_, p)| !p.indices.is_empty());
    if subs.is_empty() {
        return Err("no ProBuilder faces".into());
    }
    subs.sort_by_key(|(s, _)| *s);
    Ok(subs)
}

#[cfg(test)]
mod tests {
    use super::super::yaml;
    use super::*;

    #[test]
    fn rebuilds_a_probuilder_quad() {
        let text = "MonoBehaviour:\n  m_Script: {fileID: 11500000, guid: 8233d90336aea43098adf6dbabd606a2, type: 3}\n  m_Faces:\n  - m_Indexes: 000000000100000002000000010000000300000002000000\n    m_SubmeshIndex: 0\n  m_Positions:\n  - {x: 0, y: 0, z: 0}\n  - {x: 1, y: 0, z: 0}\n  - {x: 0, y: 1, z: 0}\n  - {x: 1, y: 1, z: 0}\n  m_Textures0:\n  - {x: 0, y: 0}\n  - {x: 1, y: 0}\n  - {x: 0, y: 1}\n  - {x: 1, y: 1}\n";
        let doc = yaml::parse_doc(text).unwrap();
        let subs = probuilder(&doc, 1000).unwrap();
        assert_eq!(subs.len(), 1);
        assert_eq!(subs[0].1.triangle_count(), 2);
        assert!(probuilder(&yaml::parse_doc("MonoBehaviour:\n  m_Faces: []\n").unwrap(), 10).is_err());
    }

    /// One triangle: positions and UVs in one stream, 16-bit indices.
    fn triangle() -> String {
        let mut data = String::new();
        for (p, uv) in
            [([0.0f32, 0.0, 0.0], [0.0f32, 0.0]), ([1.0, 0.0, 0.0], [1.0, 0.0]), ([0.0, 1.0, 0.0], [0.0, 1.0])]
        {
            for f in p.iter().chain(uv.iter()) {
                for b in f.to_le_bytes() {
                    data.push_str(&format!("{b:02x}"));
                }
            }
        }
        let empty = "    - stream: 0\n      offset: 0\n      format: 0\n      dimension: 0\n";
        format!(
            "Mesh:\n  serializedVersion: 10\n  m_SubMeshes:\n  - serializedVersion: 2\n    firstByte: 0\n    indexCount: 3\n    topology: 0\n    baseVertex: 0\n  m_IndexFormat: 0\n  m_IndexBuffer: 000001000200\n  m_VertexData:\n    serializedVersion: 3\n    m_VertexCount: 3\n    m_Channels:\n    - stream: 0\n      offset: 0\n      format: 0\n      dimension: 3\n{e}{e}{e}    - stream: 0\n      offset: 12\n      format: 0\n      dimension: 2\n{rest}    m_DataSize: 60\n    _typelessdata: {data}\n",
            e = empty,
            rest = empty.repeat(9),
        )
    }

    #[test]
    fn decodes_a_serialized_mesh() {
        let mesh = yaml::parse_doc(&triangle()).unwrap();
        let prims = decode(&mesh, 1000).unwrap();
        let p = &prims[0];
        assert_eq!(p.positions[1], Vec3::X);
        assert_eq!(p.uv0[2], Vec2::new(0.0, 0.0));
        // The triangle is clockwise seen from Unity's +Z, so that is its
        // front; mirrored, the front faces Slerp's -Z.
        let [a, b, c] = [p.indices[0], p.indices[1], p.indices[2]].map(|i| p.positions[i as usize]);
        assert!((b - a).cross(c - a).z < 0.0);
    }

    #[test]
    fn damaged_meshes_fail_cleanly() {
        let text = triangle();
        for cut in (0..text.len()).step_by(7) {
            if let Ok(m) = yaml::parse_doc(&text[..cut]) {
                let _ = decode(&m, 1000);
            }
        }
        let big = text.replace("m_VertexCount: 3", "m_VertexCount: 99999999999");
        assert!(decode(&yaml::parse_doc(&big).unwrap(), 1 << 20).is_err());
    }
}
