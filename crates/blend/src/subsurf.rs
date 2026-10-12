//! Catmull-Clark subdivision as Blender's Subdivision Surface modifier does
//! it through OpenSubdiv: smooth faces, boundaries smoothed or with their
//! corners kept, edge creases (semi-sharp, losing one unit of sharpness per
//! level), and vertices placed on the limit surface after the last level.
//! UVs and colours are interpolated linearly, Blender's "None" UV
//! smoothing, which differs from its default only inside UV islands.

use crate::poly::{PolyMesh, edge_key};
use glam::{Vec2, Vec3, Vec4};
use std::collections::HashMap;

/// How the modifier is set up.
#[derive(Clone, Copy, Debug)]
pub struct Options {
    /// Plain midpoint subdivision instead of Catmull-Clark.
    pub simple: bool,
    /// Keep the corners of open boundaries sharp.
    pub keep_corners: bool,
    pub use_creases: bool,
    /// Move the final vertices onto the limit surface.
    pub limit: bool,
}

/// An edge of the mesh being subdivided.
struct Edge {
    v: [u32; 2],
    /// Faces using it: two for a manifold edge.
    faces: u32,
    first_face: u32,
    second_face: u32,
    /// OpenSubdiv sharpness: 0 smooth, 10 and more infinitely sharp.
    sharp: f32,
}

/// Subdivides `poly` `levels` times.
pub fn subdivide(poly: &mut PolyMesh, levels: u32, opts: Options) {
    let mut sharpness: HashMap<(u32, u32), f32> = HashMap::new();
    if opts.use_creases {
        // Blender turns a crease of c into an OpenSubdiv sharpness of
        // 10 c squared.
        for (&k, &c) in &poly.creases {
            if c > 0.0 {
                sharpness.insert(k, 10.0 * c * c);
            }
        }
    }
    for level in 0..levels {
        sharpness = one_level(poly, &sharpness, opts);
        if opts.limit && !opts.simple && level + 1 == levels {
            to_limit(poly, &sharpness, opts);
        }
    }
    poly.creases.clear();
}

/// The edges of `poly` and the edge of each corner (from it to the next).
fn edges(poly: &PolyMesh, sharpness: &HashMap<(u32, u32), f32>) -> (Vec<Edge>, Vec<u32>) {
    let mut index: HashMap<(u32, u32), u32> = HashMap::with_capacity(poly.corner_verts.len());
    let mut list: Vec<Edge> = Vec::with_capacity(poly.corner_verts.len() / 2 + 1);
    let mut corner_edge = vec![0u32; poly.corner_verts.len()];
    for f in 0..poly.face_count() {
        let r = poly.face(f);
        for c in r.clone() {
            let next = if c + 1 == r.end { r.start } else { c + 1 };
            let key = edge_key(poly.corner_verts[c], poly.corner_verts[next]);
            let e = *index.entry(key).or_insert_with(|| {
                list.push(Edge {
                    v: [key.0, key.1],
                    faces: 0,
                    first_face: f as u32,
                    second_face: u32::MAX,
                    sharp: sharpness.get(&key).copied().unwrap_or(0.0),
                });
                (list.len() - 1) as u32
            });
            let edge = &mut list[e as usize];
            if edge.faces == 1 {
                edge.second_face = f as u32;
            }
            edge.faces += 1;
            corner_edge[c] = e;
        }
    }
    (list, corner_edge)
}

/// Sharpness of an edge for vertex and edge rules: boundaries and
/// non-manifold edges are infinitely sharp.
fn edge_sharpness(e: &Edge) -> f32 {
    if e.faces != 2 { f32::INFINITY } else { e.sharp }
}

/// One level; returns the child edges' sharpness.
fn one_level(poly: &mut PolyMesh, sharpness: &HashMap<(u32, u32), f32>, opts: Options) -> HashMap<(u32, u32), f32> {
    let (edges, corner_edge) = edges(poly, sharpness);
    let nv = poly.positions.len();
    let nf = poly.face_count();
    let ne = edges.len();
    // 1. Face points.
    let face_points: Vec<Vec3> = (0..nf)
        .map(|f| {
            let r = poly.face(f);
            let n = r.len() as f32;
            r.map(|c| poly.positions[poly.corner_verts[c] as usize]).sum::<Vec3>() / n
        })
        .collect();
    // 2. Edge points: midpoints on sharp and boundary edges, the average
    //    with both face points on smooth ones, blended for semi-sharp.
    let edge_points: Vec<Vec3> = edges
        .iter()
        .map(|e| {
            let (a, b) = (poly.positions[e.v[0] as usize], poly.positions[e.v[1] as usize]);
            let mid = (a + b) * 0.5;
            let s = edge_sharpness(e);
            if opts.simple || s >= 1.0 {
                return mid;
            }
            let smooth = (a + b + face_points[e.first_face as usize] + face_points[e.second_face as usize]) * 0.25;
            if s > 0.0 { smooth.lerp(mid, s) } else { smooth }
        })
        .collect();
    // 3. Vertex points.
    let vertex_points: Vec<Vec3> = if opts.simple {
        poly.positions.clone()
    } else {
        let mut face_sum = vec![Vec3::ZERO; nv];
        let mut face_count = vec![0u32; nv];
        for f in 0..nf {
            for c in poly.face(f) {
                let v = poly.corner_verts[c] as usize;
                face_sum[v] += face_points[f];
                face_count[v] += 1;
            }
        }
        let mut mid_sum = vec![Vec3::ZERO; nv];
        let mut valence = vec![0u32; nv];
        let mut sharp_count = vec![0u32; nv];
        let mut sharp_sum = vec![0f32; nv];
        let mut sharp_neighbours = vec![Vec3::ZERO; nv];
        for e in &edges {
            let (a, b) = (e.v[0] as usize, e.v[1] as usize);
            let mid = (poly.positions[a] + poly.positions[b]) * 0.5;
            let s = edge_sharpness(e);
            for (v, other) in [(a, b), (b, a)] {
                mid_sum[v] += mid;
                valence[v] += 1;
                if s > 0.0 {
                    sharp_count[v] += 1;
                    sharp_sum[v] += s.min(10.0);
                    sharp_neighbours[v] += poly.positions[other];
                }
            }
        }
        (0..nv)
            .map(|v| {
                let p = poly.positions[v];
                let n = valence[v];
                if n == 0 || face_count[v] == 0 {
                    return p;
                }
                let smooth = if n >= 3 {
                    let nf = n as f32;
                    (face_sum[v] / face_count[v] as f32 + 2.0 * mid_sum[v] / nf + (nf - 3.0) * p) / nf
                } else {
                    p
                };
                let boundary_corner = face_count[v] == 1 && n == 2;
                let sharp_result = match sharp_count[v] {
                    0 | 1 => return smooth,
                    2 if boundary_corner && opts.keep_corners => p,
                    2 => (sharp_neighbours[v] + 6.0 * p) / 8.0,
                    _ => p,
                };
                let s = sharp_sum[v] / sharp_count[v] as f32;
                if s >= 1.0 { sharp_result } else { smooth.lerp(sharp_result, s) }
            })
            .collect()
    };
    // 4. The new mesh: vertices, then edge points, then face points; one
    //    quad per original corner.
    let mut positions = Vec::with_capacity(nv + ne + nf);
    positions.extend(vertex_points);
    positions.extend(edge_points);
    positions.extend(face_points);
    let corners = poly.corner_verts.len();
    let mut face_starts = Vec::with_capacity(corners + 1);
    face_starts.push(0u32);
    let mut corner_verts = Vec::with_capacity(corners * 4);
    let mut face_material = Vec::with_capacity(corners);
    let mut face_smooth = Vec::with_capacity(corners);
    let mut uvs: Vec<Vec<Vec2>> = poly.uvs.iter().map(|_| Vec::with_capacity(corners * 4)).collect();
    let mut colors: Vec<Vec<Vec4>> = poly.colors.iter().map(|_| Vec::with_capacity(corners * 4)).collect();
    let mut normals = poly.custom_normals.as_ref().map(|_| Vec::with_capacity(corners * 4));
    for f in 0..nf {
        let r = poly.face(f);
        let n = r.len();
        for (k, c) in r.clone().enumerate() {
            let prev = r.start + (k + n - 1) % n;
            let next = r.start + (k + 1) % n;
            let ev_next = (nv + corner_edge[c] as usize) as u32;
            let ev_prev = (nv + corner_edge[prev] as usize) as u32;
            let fv = (nv + ne + f) as u32;
            corner_verts.extend([poly.corner_verts[c], ev_next, fv, ev_prev]);
            face_starts.push(corner_verts.len() as u32);
            face_material.push(poly.face_material.get(f).copied().unwrap_or(0));
            face_smooth.push(poly.face_smooth.get(f).copied().unwrap_or(true));
            for (out, l) in uvs.iter_mut().zip(&poly.uvs) {
                let v = &l.values;
                let centre = r.clone().map(|i| v[i]).sum::<Vec2>() / n as f32;
                out.extend([v[c], (v[c] + v[next]) * 0.5, centre, (v[c] + v[prev]) * 0.5]);
            }
            for (out, l) in colors.iter_mut().zip(&poly.colors) {
                let v = &l.values;
                let centre = r.clone().map(|i| v[i]).sum::<Vec4>() / n as f32;
                out.extend([v[c], (v[c] + v[next]) * 0.5, centre, (v[c] + v[prev]) * 0.5]);
            }
            if let (Some(out), Some(v)) = (&mut normals, &poly.custom_normals) {
                let centre = r.clone().map(|i| v[i]).sum::<Vec3>();
                out.extend([v[c], v[c] + v[next], centre, v[c] + v[prev]].map(|x| x.normalize_or_zero()));
            }
        }
    }
    // 5. Sharp edges and creases pass to both halves of their edge.
    let mut sharp_edges = std::collections::HashSet::new();
    let mut child_sharpness = HashMap::new();
    for (i, e) in edges.iter().enumerate() {
        let ev = (nv + i) as u32;
        let halves = [edge_key(e.v[0], ev), edge_key(ev, e.v[1])];
        if poly.sharp_edges.contains(&(e.v[0], e.v[1])) {
            sharp_edges.extend(halves);
        }
        if e.sharp > 1.0 && e.faces == 2 {
            for h in halves {
                child_sharpness.insert(h, e.sharp - 1.0);
            }
        }
    }
    poly.positions = positions;
    poly.face_starts = face_starts;
    poly.corner_verts = corner_verts;
    poly.face_material = face_material;
    poly.face_smooth = face_smooth;
    poly.sharp_edges = sharp_edges;
    for (l, v) in poly.uvs.iter_mut().zip(uvs) {
        l.values = v;
    }
    for (l, v) in poly.colors.iter_mut().zip(colors) {
        l.values = v;
    }
    poly.custom_normals = normals;
    child_sharpness
}

/// Moves every vertex of an all-quad mesh to its limit position.
fn to_limit(poly: &mut PolyMesh, sharpness: &HashMap<(u32, u32), f32>, opts: Options) {
    let (edges, _) = edges(poly, sharpness);
    let nv = poly.positions.len();
    let mut edge_sum = vec![Vec3::ZERO; nv];
    let mut valence = vec![0u32; nv];
    let mut sharp_count = vec![0u32; nv];
    let mut sharp_sum = vec![Vec3::ZERO; nv];
    for e in &edges {
        let (a, b) = (e.v[0] as usize, e.v[1] as usize);
        let s = edge_sharpness(e);
        for (v, other) in [(a, b), (b, a)] {
            edge_sum[v] += poly.positions[other];
            valence[v] += 1;
            if s > 0.0 {
                sharp_count[v] += 1;
                sharp_sum[v] += poly.positions[other];
            }
        }
    }
    // The vertex across each quad from each corner.
    let mut diag_sum = vec![Vec3::ZERO; nv];
    let mut face_count = vec![0u32; nv];
    let mut quads_only = vec![true; nv];
    for f in 0..poly.face_count() {
        let r = poly.face(f);
        let n = r.len();
        for (k, c) in r.clone().enumerate() {
            let v = poly.corner_verts[c] as usize;
            face_count[v] += 1;
            if n == 4 {
                diag_sum[v] += poly.positions[poly.corner_verts[r.start + (k + 2) % 4] as usize];
            } else {
                quads_only[v] = false;
            }
        }
    }
    let old = poly.positions.clone();
    for v in 0..nv {
        let p = old[v];
        let n = valence[v];
        if n == 0 || face_count[v] == 0 {
            continue;
        }
        let boundary_corner = face_count[v] == 1 && n == 2;
        poly.positions[v] = match sharp_count[v] {
            0 | 1 if quads_only[v] && n >= 3 && face_count[v] == n => {
                let nf = n as f32;
                (nf * nf * p + 4.0 * edge_sum[v] + diag_sum[v]) / (nf * (nf + 5.0))
            }
            0 | 1 => p,
            2 if boundary_corner && opts.keep_corners => p,
            2 => (sharp_sum[v] + 4.0 * p) / 6.0,
            _ => p,
        };
    }
}
