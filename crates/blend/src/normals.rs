//! Corner normals as Blender computes them, and Blender's custom normals.
//!
//! Each corner's normal is the angle-weighted average of the face normals
//! in its smooth fan: the faces around the vertex reachable without
//! crossing a sharp edge.  Edges are sharp when marked so, when either face
//! is flat-shaded, when they border fewer or more than two faces, or when
//! the two faces wind opposite ways.  Flat faces use their own normal.
//!
//! Custom normals are stored relative to those fans: each fan gets a frame
//! (its normal, a reference edge direction and the angle the fan spans),
//! and each corner stores two angles in that frame as 16-bit integers.
//! Decoding them needs the fans and frames built exactly as Blender builds
//! them, including which corner a fan starts from, so this follows
//! Blender's own traversal order: faces around a vertex in index order,
//! each fan walked first forwards, then backwards.

use crate::poly::{PolyMesh, edge_key};
use glam::Vec3;
use std::f32::consts::TAU;

/// Marks as sharp every edge between two faces that meet at more than
/// `angle` radians, as Auto Smooth, Smooth by Angle and Edge Split do.
pub fn mark_sharp_by_angle(poly: &mut PolyMesh, angle: f32) {
    if angle >= std::f32::consts::PI {
        return;
    }
    let normals = poly.face_normals();
    let cos = angle.cos();
    let mut first: std::collections::HashMap<(u32, u32), usize> = std::collections::HashMap::new();
    let mut sharp = Vec::new();
    for f in 0..poly.face_count() {
        let r = poly.face(f);
        for c in r.clone() {
            let next = if c + 1 == r.end { r.start } else { c + 1 };
            let key = edge_key(poly.corner_verts[c], poly.corner_verts[next]);
            match first.get(&key) {
                Some(&g) if normals[g].dot(normals[f]) < cos => sharp.push(key),
                Some(_) => {}
                None => {
                    first.insert(key, f);
                }
            }
        }
    }
    poly.sharp_edges.extend(sharp);
}

/// The normal of every corner: custom normals when the mesh carries them,
/// otherwise smooth fans.
pub fn corner_normals(poly: &PolyMesh) -> Vec<Vec3> {
    if let Some(n) = &poly.custom_normals
        && n.len() == poly.corner_verts.len()
    {
        let mut n = n.clone();
        let face_normals = poly.face_normals();
        for f in 0..poly.face_count() {
            for c in poly.face(f) {
                n[c] = n[c].try_normalize().unwrap_or(face_normals[f]);
            }
        }
        return n;
    }
    fans(poly, None)
}

/// Decodes custom normals stored as two angles per corner.
pub fn decode_custom(poly: &PolyMesh, encoded: &[[i16; 2]]) -> Vec<Vec3> {
    fans(poly, Some(encoded))
}

/// Per-corner details around one vertex.
#[derive(Clone, Copy)]
struct CornerInfo {
    face: usize,
    corner: usize,
    local_edge_prev: usize,
    local_edge_next: usize,
}

#[derive(Clone, Copy, PartialEq)]
enum EdgeInfo {
    Unset,
    One { local_corner: usize, toward_vert: bool },
    Two(usize, usize),
    Sharp,
}

/// Walks every vertex's fans, writing each corner's fan normal, decoded
/// through the fan's frame when `custom` is given.
fn fans(poly: &PolyMesh, custom: Option<&[[i16; 2]]>) -> Vec<Vec3> {
    let face_normals = poly.face_normals();
    let corners = poly.corner_verts.len();
    let nv = poly.positions.len();
    // Corners default to their face's normal, which is also what a vertex
    // used twice by one face (degenerate) keeps for its second corner.
    let mut out = vec![Vec3::Z; corners];
    let mut corner_face = vec![0usize; corners];
    for f in 0..poly.face_count() {
        for c in poly.face(f) {
            out[c] = face_normals[f];
            corner_face[c] = f;
        }
    }
    // 1. Faces around each vertex, in face order.
    let mut counts = vec![0u32; nv + 1];
    for f in 0..poly.face_count() {
        let r = poly.face(f);
        let mut seen: Vec<u32> = Vec::with_capacity(r.len());
        for c in r {
            let v = poly.corner_verts[c];
            if !seen.contains(&v) {
                seen.push(v);
                counts[v as usize] += 1;
            }
        }
    }
    let mut offsets = vec![0u32; nv + 1];
    for v in 0..nv {
        offsets[v + 1] = offsets[v] + counts[v];
    }
    let mut fill = offsets.clone();
    let mut vert_faces = vec![0u32; offsets[nv] as usize];
    for f in 0..poly.face_count() {
        let r = poly.face(f);
        let mut seen: Vec<u32> = Vec::with_capacity(r.len());
        for c in r {
            let v = poly.corner_verts[c];
            if !seen.contains(&v) {
                seen.push(v);
                vert_faces[fill[v as usize] as usize] = f as u32;
                fill[v as usize] += 1;
            }
        }
    }

    let mut infos: Vec<CornerInfo> = Vec::new();
    let mut other_verts: Vec<u32> = Vec::new();
    let mut edge_infos: Vec<EdgeInfo> = Vec::new();
    let mut edge_dirs: Vec<Vec3> = Vec::new();
    let mut visited: Vec<bool> = Vec::new();
    let mut fan: Vec<usize> = Vec::new();
    for v in 0..nv {
        let faces = &vert_faces[offsets[v] as usize..offsets[v + 1] as usize];
        if faces.is_empty() {
            continue;
        }
        let pos = poly.positions[v];
        // 2. The corners at this vertex and the edges they share, numbered
        //    locally by the vertex at their other end.
        infos.clear();
        other_verts.clear();
        let local = |other: u32, list: &mut Vec<u32>| -> usize {
            match list.iter().position(|&o| o == other) {
                Some(i) => i,
                None => {
                    list.push(other);
                    list.len() - 1
                }
            }
        };
        for &f in faces {
            let r = poly.face(f as usize);
            let Some(corner) = r.clone().find(|&c| poly.corner_verts[c] == v as u32) else { continue };
            let prev = if corner == r.start { r.end - 1 } else { corner - 1 };
            let next = if corner + 1 == r.end { r.start } else { corner + 1 };
            let local_edge_prev = local(poly.corner_verts[prev], &mut other_verts);
            let local_edge_next = local(poly.corner_verts[next], &mut other_verts);
            infos.push(CornerInfo { face: f as usize, corner, local_edge_prev, local_edge_next });
        }
        edge_infos.clear();
        edge_infos.resize(other_verts.len(), EdgeInfo::Unset);
        for (lc, info) in infos.iter().enumerate() {
            if !poly.face_smooth.get(info.face).copied().unwrap_or(false) {
                edge_infos[info.local_edge_prev] = EdgeInfo::Sharp;
                edge_infos[info.local_edge_next] = EdgeInfo::Sharp;
                continue;
            }
            for (le, toward) in [(info.local_edge_prev, true), (info.local_edge_next, false)] {
                let marked = poly.sharp_edges.contains(&edge_key(v as u32, other_verts[le]));
                edge_infos[le] = match edge_infos[le] {
                    EdgeInfo::Unset if marked => EdgeInfo::Sharp,
                    EdgeInfo::Unset => EdgeInfo::One { local_corner: lc, toward_vert: toward },
                    EdgeInfo::One { local_corner, toward_vert } if toward_vert != toward => {
                        EdgeInfo::Two(local_corner, lc)
                    }
                    _ => EdgeInfo::Sharp,
                };
            }
        }
        edge_dirs.clear();
        edge_dirs.extend(other_verts.iter().map(|&o| (poly.positions[o as usize] - pos).normalize_or_zero()));

        // 3. Fans, starting from the first corner not yet in one.
        visited.clear();
        visited.resize(infos.len(), false);
        let mut done = 0;
        let mut start = 0;
        while done < infos.len() {
            fan.clear();
            traverse(&infos, &edge_infos, start, &mut fan);
            let mut normal = if fan.len() == 1 {
                face_normals[infos[fan[0]].face]
            } else {
                let mut n = Vec3::ZERO;
                for &lc in &fan {
                    let i = &infos[lc];
                    let d = edge_dirs[i.local_edge_prev].dot(edge_dirs[i.local_edge_next]).clamp(-1.0, 1.0);
                    n += face_normals[i.face] * d.acos();
                }
                n.normalize_or_zero()
            };
            if let Some(custom) = custom {
                normal = decode_fan(&infos, &edge_dirs, &fan, normal, custom);
            }
            for &lc in &fan {
                out[infos[lc].corner] = normal;
                visited[lc] = true;
            }
            done += fan.len().max(1);
            while start < infos.len() && visited[start] {
                start += 1;
            }
            if start >= infos.len() {
                break;
            }
        }
    }
    out
}

/// Collects the fan containing local corner `start`, in Blender's order.
fn traverse(infos: &[CornerInfo], edges: &[EdgeInfo], start: usize, fan: &mut Vec<usize>) {
    fan.push(start);
    // 1. Forwards across each corner's next edge, then reversed.
    let mut current = start;
    let mut edge = infos[current].local_edge_next;
    let mut cyclic = false;
    while let EdgeInfo::Two(a, b) = edges[edge] {
        current = if current == a { b } else { a };
        if current == start {
            cyclic = true;
            break;
        }
        if fan.len() > infos.len() {
            break;
        }
        fan.push(current);
        edge = infos[current].local_edge_next;
    }
    fan.reverse();
    if cyclic {
        // A closed fan starts at its smallest corner index.
        if let Some(min) = (0..fan.len()).min_by_key(|&i| infos[fan[i]].corner) {
            fan.rotate_left(min);
        }
        return;
    }
    // 2. Backwards across each corner's previous edge.
    let mut current = start;
    let mut edge = infos[current].local_edge_prev;
    while let EdgeInfo::Two(a, b) = edges[edge] {
        current = if current == a { b } else { a };
        if fan.len() > infos.len() {
            break;
        }
        fan.push(current);
        edge = infos[current].local_edge_prev;
    }
}

/// The custom normal of a fan: the fan's frame from its first and last
/// edges, and the corners' stored angles averaged, as Blender averages
/// them.
fn decode_fan(infos: &[CornerInfo], edge_dirs: &[Vec3], fan: &[usize], lnor: Vec3, custom: &[[i16; 2]]) -> Vec3 {
    let first_edge = infos[fan[0]].local_edge_next;
    let last_edge = infos[fan[fan.len() - 1]].local_edge_prev;
    let mut dirs: Vec<Vec3> = Vec::new();
    if fan.len() > 1 {
        dirs.extend(fan.iter().map(|&lc| edge_dirs[infos[lc].local_edge_next]));
        if last_edge != first_edge {
            dirs.push(edge_dirs[last_edge]);
        }
    }
    let Some(space) = Space::define(lnor, edge_dirs[first_edge], edge_dirs[last_edge], &dirs) else {
        return lnor;
    };
    let (mut a, mut b) = (0i32, 0i32);
    for &lc in fan {
        let c = custom.get(infos[lc].corner).copied().unwrap_or([0, 0]);
        a += c[0] as i32;
        b += c[1] as i32;
    }
    let n = fan.len() as i32;
    space.decode([(a / n) as i16, (b / n) as i16])
}

/// A fan's frame for custom normals.
struct Space {
    lnor: Vec3,
    reference: Vec3,
    ortho: Vec3,
    alpha: f32,
    beta: f32,
}

impl Space {
    fn define(lnor: Vec3, vec_ref: Vec3, vec_other: Vec3, edge_dirs: &[Vec3]) -> Option<Space> {
        const THRESHOLD: f32 = 1.0 - 1e-4;
        let dtp_ref = vec_ref.dot(lnor);
        let dtp_other = vec_other.dot(lnor);
        if dtp_ref.abs() >= THRESHOLD || dtp_other.abs() >= THRESHOLD {
            return None;
        }
        let acos = |d: f32| d.clamp(-1.0, 1.0).acos();
        let alpha = if edge_dirs.is_empty() {
            (acos(vec_ref.dot(lnor)) + acos(vec_other.dot(lnor))) / 2.0
        } else {
            edge_dirs.iter().map(|d| acos(d.dot(lnor))).sum::<f32>() / edge_dirs.len() as f32
        };
        let reference = (vec_ref - lnor * dtp_ref).normalize_or_zero();
        let ortho = lnor.cross(reference).normalize_or_zero();
        let other = (vec_other - lnor * dtp_other).normalize_or_zero();
        let dtp = reference.dot(other);
        let beta = if dtp < THRESHOLD {
            let beta = acos(dtp);
            if ortho.dot(other) < 0.0 { TAU - beta } else { beta }
        } else {
            TAU
        };
        Some(Space { lnor, reference, ortho, alpha, beta })
    }

    fn decode(&self, data: [i16; 2]) -> Vec3 {
        if data[0] == 0 || self.alpha == 0.0 || self.beta == 0.0 {
            return self.lnor;
        }
        let alpha_fac = data[0] as f32 / i16::MAX as f32;
        let alpha = (if alpha_fac > 0.0 { self.alpha } else { TAU - self.alpha }) * alpha_fac;
        let beta_fac = data[1] as f32 / i16::MAX as f32;
        let mut n = self.lnor * alpha.cos();
        if beta_fac == 0.0 {
            n += self.reference * alpha.sin();
        } else {
            let beta = (if beta_fac > 0.0 { self.beta } else { TAU - self.beta }) * beta_fac;
            n += self.reference * (alpha.sin() * beta.cos()) + self.ortho * (alpha.sin() * beta.sin());
        }
        n
    }
}
