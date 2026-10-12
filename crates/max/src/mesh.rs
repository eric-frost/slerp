//! Editable Mesh and Editable Poly objects as polygon meshes.
//!
//! Both keep their geometry in chunk 0x08FE.  An Editable Mesh (a
//! `TriObject`) has vertices (0x0914: a count, then x, y, z per vertex),
//! triangles (0x0912: a count, then three vertex indices, the smoothing
//! groups, and flags whose upper half is the material id), and map
//! channels as runs of 0x0959 (the channel: 1 and up are UVs, 0 vertex
//! colours), 0x2394 (map vertices) and 0x2396 (map triangles).
//!
//! An Editable Poly (an `MNMesh`) has vertices (0x0100: a count, then flags
//! and x, y, z), edges (0x010A), faces (0x011A: a count, then per face its
//! degree, its vertices, a flag word saying which optional fields follow:
//! 0x01 smoothing groups, 0x08 the material id, 0x10 face flags, 0x20 the
//! triangulation's diagonals) and map channels as runs of 0x0124 (the
//! channel), 0x0128 (map vertices) and 0x012B (map faces: degree and
//! indices each).
//!
//! Normals the user set by hand (or Max cached) sit in 0x23A0 (meshes) or
//! 0x0300 (polys); they are used when they cover every face, and smoothing
//! groups give the normals otherwise.

use crate::chunks::{Chunk, f32_at, u16_at, u32_at};
use glam::{Vec2, Vec3};
use slerp_polymesh::{Layer, PolyMesh, Smoothing};

/// Most vertices, faces or map entries read for one object.
pub const MAX_ELEMENTS: usize = 50_000_000;

/// A count at the start of `d` that `per` bytes per element must fit.
fn count(d: &[u8], per: usize) -> Option<usize> {
    let n = u32_at(d, 0)? as usize;
    (n <= MAX_ELEMENTS && n.checked_mul(per)? <= d.len().saturating_sub(4)).then_some(n)
}

fn points(d: &[u8], skip: usize) -> Vec<Vec3> {
    let stride = skip + 12;
    let Some(n) = count(d, stride) else { return Vec::new() };
    (0..n)
        .filter_map(|i| {
            let at = 4 + i * stride + skip;
            Some(Vec3::new(f32_at(d, at)?, f32_at(d, at + 4)?, f32_at(d, at + 8)?))
        })
        .map(|p| if p.is_finite() { p } else { Vec3::ZERO })
        .collect()
}

/// An Editable Mesh's geometry.
pub fn tri_object(obj: &Chunk) -> Option<PolyMesh> {
    let g = obj.child(0x08FE)?;
    let mut p = PolyMesh::new();
    p.smoothing = Smoothing::Groups;
    p.positions = points(g.child_data(0x0914)?, 0);
    let faces = g.child_data(0x0912)?;
    let n = count(faces, 20)?;
    for i in 0..n {
        let at = 4 + i * 20;
        let v = [u32_at(faces, at)?, u32_at(faces, at + 4)?, u32_at(faces, at + 8)?];
        let groups = u32_at(faces, at + 12)?;
        let flags = u32_at(faces, at + 16)?;
        p.push_face(&v, flags >> 16, groups);
    }
    // UVs from map channel 1: its own triangles index its own vertices.
    let kids = g.children();
    for (k, c) in kids.iter().enumerate() {
        if c.id != 0x0959 || c.data().and_then(|d| u32_at(d, 0)) != Some(1) {
            continue;
        }
        let verts = kids[k + 1..].iter().take(3).find(|c| c.id == 0x2394).and_then(Chunk::data);
        let tris = kids[k + 1..].iter().take(3).find(|c| c.id == 0x2396).and_then(Chunk::data);
        if let (Some(verts), Some(tris)) = (verts, tris) {
            let uv = points(verts, 0);
            if let Some(m) = count(tris, 12).filter(|&m| m == n) {
                let mut values = Vec::with_capacity(n * 3);
                for i in 0..m {
                    for j in 0..3 {
                        let idx = u32_at(tris, 4 + i * 12 + j * 4).unwrap_or(0) as usize;
                        values.push(uv.get(idx).map_or(Vec2::ZERO, |t| Vec2::new(t.x, t.y)));
                    }
                }
                p.uvs.push(Layer { name: "Map channel 1".into(), values });
            }
        }
        break;
    }
    if let Some(spec) = g.child(0x23A0) {
        p.custom_normals = specified_normals(spec, &p, false);
    }
    Some(p)
}

/// An Editable Poly's geometry.
pub fn poly_object(obj: &Chunk) -> Option<PolyMesh> {
    let g = obj.child(0x08FE)?;
    let mut p = PolyMesh::new();
    p.smoothing = Smoothing::Groups;
    p.positions = points(g.child_data(0x0100)?, 4);
    let faces = g.child_data(0x011A)?;
    let n = u32_at(faces, 0)? as usize;
    let mut at = 4;
    let mut diagonals: Vec<Vec<u32>> = Vec::new();
    let mut verts: Vec<u32> = Vec::new();
    for _ in 0..n.min(MAX_ELEMENTS) {
        let deg = u32_at(faces, at)? as usize;
        if deg > (faces.len() - at) / 4 {
            break;
        }
        at += 4;
        verts.clear();
        for j in 0..deg {
            verts.push(u32_at(faces, at + j * 4)?);
        }
        at += deg * 4;
        let flags = u16_at(faces, at)?;
        at += 2;
        let mut groups = 0;
        let mut material = 0;
        if flags & 0x01 != 0 {
            groups = u32_at(faces, at)?;
            at += 4;
        }
        if flags & 0x08 != 0 {
            material = u16_at(faces, at)? as u32;
            at += 2;
        }
        if flags & 0x10 != 0 {
            at += 4;
        }
        let mut diag = Vec::new();
        if flags & 0x20 != 0 && deg >= 3 {
            for j in 0..2 * (deg - 3) {
                diag.push(u32_at(faces, at + j * 4)?);
            }
            at += 2 * (deg - 3) * 4;
        }
        p.push_face(&verts, material, groups);
        diagonals.push(diag);
    }
    // UVs: map channel 1's faces, one per geometry face.
    let kids = g.children();
    for (k, c) in kids.iter().enumerate() {
        if c.id != 0x0124 || c.data().and_then(|d| u32_at(d, 0)) != Some(1) {
            continue;
        }
        let verts = kids[k + 1..].iter().take(3).find(|c| c.id == 0x0128).and_then(Chunk::data);
        let mfaces = kids[k + 1..].iter().take(3).find(|c| c.id == 0x012B).and_then(Chunk::data);
        if let (Some(verts), Some(mfaces)) = (verts, mfaces) {
            let uv = points(verts, 0);
            let mut values = Vec::with_capacity(p.corner_verts.len());
            let mut at = 0;
            let mut ok = true;
            for f in 0..p.face_count() {
                let deg = p.face(f).len();
                if u32_at(mfaces, at).map(|d| d as usize) != Some(deg) {
                    ok = false;
                    break;
                }
                for j in 0..deg {
                    let idx = u32_at(mfaces, at + 4 + j * 4).unwrap_or(0) as usize;
                    values.push(uv.get(idx).map_or(Vec2::ZERO, |t| Vec2::new(t.x, t.y)));
                }
                at += 4 + deg * 4;
            }
            if ok {
                p.uvs.push(Layer { name: "Map channel 1".into(), values });
            }
        }
        break;
    }
    if let Some(spec) = g.child(0x0300) {
        p.custom_normals = specified_normals(spec, &p, true);
    }
    triangulate_by_diagonals(&mut p, &diagonals);
    Some(p)
}

/// Splits faces along the diagonals Max chose for them, so concave faces
/// draw as Max draws them.  Faces without diagonals stay polygons.
fn triangulate_by_diagonals(p: &mut PolyMesh, diagonals: &[Vec<u32>]) {
    if !diagonals.iter().any(|d| !d.is_empty()) {
        return;
    }
    let mut out = p.empty_like();
    out.positions = std::mem::take(&mut p.positions);
    for f in 0..p.face_count() {
        let r = p.face(f);
        let deg = r.len();
        let pieces = diagonals.get(f).and_then(|d| split(deg, d)).unwrap_or_else(|| vec![(0..deg).collect()]);
        for piece in pieces {
            let idx: Vec<usize> = piece.iter().map(|&k| r.start + k).collect();
            for &c in &idx {
                out.corner_verts.push(p.corner_verts[c]);
            }
            for (o, l) in out.uvs.iter_mut().zip(&p.uvs) {
                o.values.extend(idx.iter().map(|&c| l.values.get(c).copied().unwrap_or(Vec2::ZERO)));
            }
            if let (Some(o), Some(n)) = (&mut out.custom_normals, &p.custom_normals) {
                o.extend(idx.iter().map(|&c| n.get(c).copied().unwrap_or(Vec3::ZERO)));
            }
            out.face_starts.push(out.corner_verts.len() as u32);
            out.face_material.push(p.face_material.get(f).copied().unwrap_or(0));
            out.face_groups.push(p.face_groups.get(f).copied().unwrap_or(0));
        }
    }
    *p = out;
}

/// Cuts a polygon of `deg` corners along `diag` (pairs of corner indices)
/// into triangles, as corner lists; `None` when the diagonals don't make
/// a valid triangulation.
#[cfg(test)]
pub(crate) fn split_for_tests(deg: usize, diag: &[u32]) -> Option<Vec<Vec<usize>>> {
    split(deg, diag)
}

fn split(deg: usize, diag: &[u32]) -> Option<Vec<Vec<usize>>> {
    // Large polygons are left to the general triangulation: splitting
    // costs time by the square of the corners.
    if diag.is_empty() || !(4..=64).contains(&deg) {
        return None;
    }
    let mut polys: Vec<Vec<usize>> = vec![(0..deg).collect()];
    for pair in diag.chunks_exact(2) {
        let (a, b) = (pair[0] as usize, pair[1] as usize);
        let k = polys.iter().position(|poly| poly.contains(&a) && poly.contains(&b))?;
        let poly = polys.swap_remove(k);
        let ia = poly.iter().position(|&v| v == a)?;
        let ib = poly.iter().position(|&v| v == b)?;
        let (lo, hi) = (ia.min(ib), ia.max(ib));
        if hi - lo < 2 || lo + poly.len() - hi < 2 {
            return None;
        }
        let first: Vec<usize> = poly[lo..=hi].to_vec();
        let mut second: Vec<usize> = poly[hi..].to_vec();
        second.extend_from_slice(&poly[..=lo]);
        polys.push(first);
        polys.push(second);
    }
    polys.iter().all(|p| p.len() == 3).then_some(polys)
}

/// Normals stored per face corner: a list of normals (0x0110), then per
/// face its index (0x0124) and the normal of each corner (0x0128 > 0x0200;
/// for polys led by the corner count).  Used only when every face has
/// them.
fn specified_normals(spec: &Chunk, p: &PolyMesh, counted: bool) -> Option<Vec<Vec3>> {
    let list = points(spec.child_data(0x0110)?, 0);
    if list.is_empty() {
        return None;
    }
    let mut out = vec![Vec3::ZERO; p.corner_verts.len()];
    let mut covered = vec![false; p.face_count()];
    let mut face: Option<usize> = None;
    for c in spec.children() {
        match c.id {
            0x0124 => face = c.data().and_then(|d| u32_at(d, 0)).map(|f| f as usize),
            0x0128 => {
                let (Some(f), Some(ids)) = (face, c.child_data(0x0200)) else { continue };
                if f >= p.face_count() {
                    continue;
                }
                let r = p.face(f);
                let skip = if counted { 4 } else { 0 };
                if counted && u32_at(ids, 0).map(|n| n as usize) != Some(r.len()) {
                    continue;
                }
                for (j, corner) in r.clone().enumerate() {
                    let Some(i) = u32_at(ids, skip + j * 4) else { break };
                    out[corner] = list.get(i as usize).copied().unwrap_or(Vec3::ZERO);
                }
                covered[f] = true;
            }
            _ => {}
        }
    }
    covered.iter().all(|&c| c).then_some(out)
}
