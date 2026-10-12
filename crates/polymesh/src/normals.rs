//! A normal for every face corner, from the mesh's smoothing.
//!
//! Each corner's normal is the average of the normals of the faces it
//! blends with, weighted by the angle each face spans at the vertex (so a
//! vertex shared by one large and many thin triangles doesn't lean towards
//! the thin ones).  Which faces blend depends on `Smoothing`: with hard
//! edges, those reachable around the vertex without crossing a hard, open
//! or non-manifold edge; with smoothing groups, those at the vertex that
//! share a group bit.

use crate::{PolyMesh, Smoothing, edge_key};
use glam::Vec3;
use std::collections::HashMap;

/// The normal of every corner: the file's own where it stores one, the
/// smoothing's elsewhere.
pub fn corner_normals(poly: &PolyMesh) -> Vec<Vec3> {
    let face_normals = poly.face_normals();
    let mut out = match poly.smoothing {
        Smoothing::Flat => flat(poly, &face_normals),
        Smoothing::HardEdges => by_edges(poly, &face_normals),
        Smoothing::Groups => by_groups(poly, &face_normals),
    };
    if let Some(custom) = &poly.custom_normals {
        for (o, &c) in out.iter_mut().zip(custom) {
            if let Some(n) = c.try_normalize() {
                *o = n;
            }
        }
    }
    out
}

fn flat(poly: &PolyMesh, face_normals: &[Vec3]) -> Vec<Vec3> {
    let mut out = vec![Vec3::Z; poly.corner_verts.len()];
    for f in 0..poly.face_count() {
        for c in poly.face(f) {
            out[c] = face_normals[f];
        }
    }
    out
}

/// The angle face `f` spans at corner `c`, for weighting.
fn corner_angle(poly: &PolyMesh, f: usize, c: usize) -> f32 {
    let r = poly.face(f);
    let prev = if c == r.start { r.end - 1 } else { c - 1 };
    let next = if c + 1 == r.end { r.start } else { c + 1 };
    let p = poly.positions[poly.corner_verts[c] as usize];
    let a = (poly.positions[poly.corner_verts[prev] as usize] - p).normalize_or_zero();
    let b = (poly.positions[poly.corner_verts[next] as usize] - p).normalize_or_zero();
    a.dot(b).clamp(-1.0, 1.0).acos()
}

/// Faces blend across soft manifold edges: the corners at either end of
/// such an edge, one in each face, join one fan.
fn by_edges(poly: &PolyMesh, face_normals: &[Vec3]) -> Vec<Vec3> {
    let corners = poly.corner_verts.len();
    let mut corner_face = vec![0usize; corners];
    for f in 0..poly.face_count() {
        for c in poly.face(f) {
            corner_face[c] = f;
        }
    }
    // 1. Each edge's uses: the corner it starts from in each face, and the
    //    direction it runs there.
    let mut uses: HashMap<(u32, u32), Vec<(usize, bool)>> = HashMap::with_capacity(corners);
    for f in 0..poly.face_count() {
        let r = poly.face(f);
        for c in r.clone() {
            let next = if c + 1 == r.end { r.start } else { c + 1 };
            let (a, b) = (poly.corner_verts[c], poly.corner_verts[next]);
            uses.entry(edge_key(a, b)).or_default().push((c, a < b));
        }
    }
    // 2. Join the corners across every soft edge used by exactly two faces
    //    winding opposite ways.
    let mut parent: Vec<u32> = (0..corners as u32).collect();
    fn find(parent: &mut [u32], mut x: u32) -> u32 {
        while parent[x as usize] != x {
            parent[x as usize] = parent[parent[x as usize] as usize];
            x = parent[x as usize];
        }
        x
    }
    let next_corner = |c: usize| {
        let f = corner_face[c];
        let r = poly.face(f);
        if c + 1 == r.end { r.start } else { c + 1 }
    };
    for (key, list) in &uses {
        let [(c0, d0), (c1, d1)] = list[..] else { continue };
        if d0 == d1 || poly.hard_edges.contains(key) || corner_face[c0] == corner_face[c1] {
            continue;
        }
        // c0 runs a->b, c1 runs b->a: c0's start meets c1's end and the
        // other way round.
        for (x, y) in [(c0, next_corner(c1)), (next_corner(c0), c1)] {
            let (rx, ry) = (find(&mut parent, x as u32), find(&mut parent, y as u32));
            if rx != ry {
                parent[rx as usize] = ry;
            }
        }
    }
    // 3. Sum each fan's weighted face normals.
    let mut sums = vec![Vec3::ZERO; corners];
    for (c, &f) in corner_face.iter().enumerate() {
        let root = find(&mut parent, c as u32) as usize;
        sums[root] += face_normals[f] * corner_angle(poly, f, c);
    }
    (0..corners)
        .map(|c| {
            let root = find(&mut parent, c as u32) as usize;
            sums[root].try_normalize().unwrap_or(face_normals[corner_face[c]])
        })
        .collect()
}

/// Faces at a vertex blend when their smoothing groups share a bit.
fn by_groups(poly: &PolyMesh, face_normals: &[Vec3]) -> Vec<Vec3> {
    let corners = poly.corner_verts.len();
    let mut out = flat(poly, face_normals);
    // 1. Per vertex and smoothing mask, the weighted sum of face normals.
    let mut sums: HashMap<(u32, u32), Vec3> = HashMap::new();
    for f in 0..poly.face_count() {
        let groups = poly.face_groups.get(f).copied().unwrap_or(0);
        if groups == 0 {
            continue;
        }
        for c in poly.face(f) {
            *sums.entry((poly.corner_verts[c], groups)).or_insert(Vec3::ZERO) +=
                face_normals[f] * corner_angle(poly, f, c);
        }
    }
    // 2. The masks at each vertex, so a corner adds up those overlapping
    //    its own.
    let mut masks: HashMap<u32, Vec<(u32, Vec3)>> = HashMap::new();
    for (&(v, m), &n) in &sums {
        masks.entry(v).or_default().push((m, n));
    }
    let mut cache: HashMap<(u32, u32), Vec3> = HashMap::new();
    for f in 0..poly.face_count() {
        let groups = poly.face_groups.get(f).copied().unwrap_or(0);
        if groups == 0 {
            continue;
        }
        for c in poly.face(f) {
            let v = poly.corner_verts[c];
            let n = *cache.entry((v, groups)).or_insert_with(|| {
                masks
                    .get(&v)
                    .map(|list| list.iter().filter(|(m, _)| m & groups != 0).map(|(_, n)| *n).sum::<Vec3>())
                    .unwrap_or(Vec3::ZERO)
            });
            if c < corners {
                out[c] = n.try_normalize().unwrap_or(face_normals[f]);
            }
        }
    }
    out
}
