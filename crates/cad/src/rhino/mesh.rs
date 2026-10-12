//! Rhino meshes (`ON_Mesh`), as they are stored in a file: faces of four
//! vertex indices (a triangle repeats its last), then compressed arrays of
//! vertices, normals, texture coordinates, curvatures and colours.
//! Files from before Rhino 2 stored the arrays uncompressed (chunk version
//! 1); everything after is version 3.

use super::chunks::{ANONYMOUS_CHUNK, Archive, Cursor};
use super::preview::read_compressed;

/// The most vertices or faces one mesh may claim: far beyond what Rhino
/// makes, but a size that can't be used to exhaust memory by itself.
const MAX_COUNT: usize = 200_000_000;

/// What a thumbnail and the viewer need of a mesh.
#[derive(Clone, Debug, Default)]
pub struct MeshData {
    pub positions: Vec<[f32; 3]>,
    pub normals: Vec<[f32; 3]>,
    /// Per vertex, as Rhino stores colours: red in the lowest byte.
    pub colors: Vec<u32>,
    /// Triangles, three indices each.
    pub indices: Vec<u32>,
}

/// Reads the data of an `ON_Mesh` (the body of its class data chunk).
pub fn read(a: &Archive, data: &[u8]) -> Option<MeshData> {
    let mut cur = Cursor::new(data);
    let (major, _minor) = cur.version()?;
    if major != 1 && major != 3 {
        return None;
    }
    let vcount = usize::try_from(cur.i32()?).ok().filter(|&n| n <= MAX_COUNT)?;
    let fcount = usize::try_from(cur.i32()?).ok().filter(|&n| n <= MAX_COUNT)?;
    // Texture and surface domains and scales, then the bounding boxes of
    // the vertices, normals and texture coordinates, then "closed".
    cur.skip(4 * 16 + 16 + 24 + 24 + 16 + 4)?;
    // Mesh parameters: a flag and, if set, a chunk.
    if cur.u8()? != 0 {
        skip_chunk(a, &mut cur)?;
    }
    // Curvature statistics for four kinds of curvature.
    for _ in 0..4 {
        if cur.u8()? != 0 {
            skip_chunk(a, &mut cur)?;
        }
    }
    // The faces, as bytes, shorts or ints.
    let width = match cur.i32()? {
        1 => 1,
        2 => 2,
        4 => 4,
        _ => return None,
    };
    let face_bytes = fcount.checked_mul(4 * width)?;
    if face_bytes > cur.remaining() {
        return None;
    }
    let raw_faces = cur.bytes(face_bytes)?;
    let index = |i: usize| -> u32 {
        match width {
            1 => raw_faces[i] as u32,
            2 => u16::from_le_bytes([raw_faces[i * 2], raw_faces[i * 2 + 1]]) as u32,
            _ => u32::from_le_bytes(raw_faces[i * 4..i * 4 + 4].try_into().unwrap()),
        }
    };
    let mut mesh = MeshData::default();
    if major == 1 {
        // Uncompressed arrays, each with its count.
        mesh.positions = read_floats(&mut cur, 3, vcount)?.chunks_exact(3).map(|c| [c[0], c[1], c[2]]).collect();
        let normals = read_floats(&mut cur, 3, vcount)?;
        mesh.normals = normals.chunks_exact(3).map(|c| [c[0], c[1], c[2]]).collect();
        // Texture coordinates and curvatures aren't used; skip them to
        // reach the colours.
        let _ = read_floats(&mut cur, 2, vcount)?;
        let _ = read_floats(&mut cur, 4, vcount)?;
        let n = cur.i32()?;
        if n > 0 && n as usize == vcount {
            mesh.colors = (0..n as usize).map(|_| cur.u32()).collect::<Option<_>>()?;
        }
    } else {
        // Five compressed arrays, each with a size (0 if absent).
        mesh.positions = read_array(a, &mut cur, vcount, 12)?
            .map(|b| b.chunks_exact(12).map(|c| [f32_at(c, 0), f32_at(c, 4), f32_at(c, 8)]).collect())
            .unwrap_or_default();
        mesh.normals = read_array(a, &mut cur, vcount, 12)?
            .map(|b| b.chunks_exact(12).map(|c| [f32_at(c, 0), f32_at(c, 4), f32_at(c, 8)]).collect())
            .unwrap_or_default();
        let _texture = read_array(a, &mut cur, vcount, 8)?;
        let _curvature = read_array(a, &mut cur, vcount, 16)?;
        mesh.colors = read_array(a, &mut cur, vcount, 4)?
            .map(|b| b.chunks_exact(4).map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect())
            .unwrap_or_default();
    }
    let n = mesh.positions.len();
    if n == 0 {
        return None;
    }
    mesh.indices.reserve(fcount * 3);
    for f in 0..fcount {
        let v: [u32; 4] = [index(f * 4), index(f * 4 + 1), index(f * 4 + 2), index(f * 4 + 3)];
        if v.iter().any(|&i| i as usize >= n) {
            continue;
        }
        // A triangle repeats its third vertex.
        mesh.indices.extend_from_slice(&[v[0], v[1], v[2]]);
        if v[2] != v[3] {
            mesh.indices.extend_from_slice(&[v[0], v[2], v[3]]);
        }
    }
    (!mesh.indices.is_empty()).then_some(mesh)
}

fn f32_at(b: &[u8], at: usize) -> f32 {
    let v = f32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]]);
    // Garbage becomes zero rather than spreading NaNs.
    if v.is_finite() { v } else { 0.0 }
}

/// Skips the chunk at the cursor.
fn skip_chunk(a: &Archive, cur: &mut Cursor) -> Option<()> {
    let sub = Archive { data: cur.data, version: a.version };
    let c = sub.chunk_at(cur.pos, cur.data.len())?;
    cur.pos = c.next;
    Some(())
}

/// One compressed array: its size in bytes, which must be `count * stride`
/// (or zero for an array the mesh lacks), then the buffer.
fn read_array<'a>(a: &Archive, cur: &mut Cursor<'a>, count: usize, stride: usize) -> Option<Option<Vec<u8>>> {
    let size = cur.u32()? as usize;
    if size == 0 {
        return Some(None);
    }
    if size != count.checked_mul(stride)? {
        return None;
    }
    read_compressed(a, cur, size).map(Some)
}

/// An uncompressed array of floats: a count, then `per` floats each.
fn read_floats(cur: &mut Cursor, per: usize, vcount: usize) -> Option<Vec<f32>> {
    let n = cur.i32()?;
    if n <= 0 {
        return Some(Vec::new());
    }
    let n = n as usize;
    if n != vcount {
        // Arrays that don't match the vertices are dropped.
        cur.skip(n.checked_mul(per * 4)?)?;
        return Some(Vec::new());
    }
    let raw = cur.bytes(n.checked_mul(per * 4)?)?;
    Some(raw.chunks_exact(4).map(|c| f32_at(c, 0)).collect())
}

#[allow(dead_code)]
const _: u32 = ANONYMOUS_CHUNK;
