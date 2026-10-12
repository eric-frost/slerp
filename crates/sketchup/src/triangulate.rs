//! Splitting SketchUp's planar faces (an outer loop and holes) into
//! triangles that face the way the face does.

use glam::{DVec2, DVec3};

/// The largest outline ear clipping is given (convex ones are fanned
/// whatever their size); larger faces are skipped with a warning rather
/// than tying up the loader, since ear clipping takes time by the square of
/// the points or worse (a ring of repeated points takes seconds at a few
/// thousand).
const MAX_POINTS: usize = 4096;

/// Two directions in the plane of a face with `normal`.
pub fn plane_axes(normal: DVec3) -> (DVec3, DVec3) {
    let helper = if normal.x.abs() < 0.9 { DVec3::X } else { DVec3::Y };
    let u = normal.cross(helper).normalize_or_zero();
    (u, normal.cross(u))
}

/// The normal of a ring by Newell's method: robust for concave outlines
/// and slightly non-planar ones.
pub fn newell(points: &[DVec3]) -> DVec3 {
    let mut n = DVec3::ZERO;
    for (i, a) in points.iter().enumerate() {
        let b = points[(i + 1) % points.len()];
        n += DVec3::new((a.y - b.y) * (a.z + b.z), (a.z - b.z) * (a.x + b.x), (a.x - b.x) * (a.y + b.y));
    }
    n
}

fn area2(ring: &[DVec2]) -> f64 {
    (0..ring.len()).map(|i| ring[i].perp_dot(ring[(i + 1) % ring.len()])).sum::<f64>()
}

fn is_convex(ring: &[DVec2]) -> bool {
    let n = ring.len();
    let mut sign = 0.0;
    for i in 0..n {
        let (a, b, c) = (ring[i], ring[(i + 1) % n], ring[(i + 2) % n]);
        let cross = (b - a).perp_dot(c - b);
        if cross.abs() > 1e-12 * (b - a).length() * (c - b).length() {
            if sign == 0.0 {
                sign = cross.signum();
            } else if cross.signum() != sign {
                return false;
            }
        }
    }
    sign != 0.0
}

/// Triangles of the face whose rings (outer loop first, then holes) index
/// `vertices`, wound counter-clockwise around `normal`.  Returns false
/// when nothing could be made of it.
pub fn face(vertices: &[DVec3], rings: &[Vec<u32>], normal: DVec3, out: &mut Vec<[u32; 3]>) -> bool {
    let start = out.len();
    let points = |r: &[u32]| r.iter().map(|&i| vertices[i as usize]).collect::<Vec<_>>();
    let Some(outer) = rings.first().filter(|r| r.len() >= 3) else { return false };
    let normal = if normal.is_finite() && normal.length_squared() > 1e-20 {
        normal.normalize()
    } else {
        newell(&points(outer)).normalize_or_zero()
    };
    if normal == DVec3::ZERO {
        return false;
    }
    // 1. In the face's plane, measured from its first point.
    let (u, v) = plane_axes(normal);
    let origin = vertices[outer[0] as usize];
    let flat = |r: &[u32]| -> Vec<DVec2> {
        r.iter()
            .map(|&i| {
                let d = vertices[i as usize] - origin;
                DVec2::new(d.dot(u), d.dot(v))
            })
            .collect()
    };
    let mut rings2: Vec<(Vec<DVec2>, &Vec<u32>)> =
        rings.iter().filter(|r| r.len() >= 3).map(|r| (flat(r), r)).collect();
    // 2. The outer loop is stored first; should a hole outsize it, the
    //    largest ring is taken as the outline.
    if let Some(big) =
        (0..rings2.len()).max_by(|&a, &b| area2(&rings2[a].0).abs().total_cmp(&area2(&rings2[b].0).abs()))
        && big != 0
        && area2(&rings2[big].0).abs() > area2(&rings2[0].0).abs() * 1.0001
    {
        rings2.swap(0, big);
    }
    let total: usize = rings2.iter().map(|r| r.0.len()).sum();
    if rings2.iter().flat_map(|r| &r.0).any(|p| !p.is_finite()) {
        return false;
    }
    let (outline, ids) = (&rings2[0].0, rings2[0].1);
    // 3. Simple outlines without holes: a fan.
    if rings2.len() == 1 && (ids.len() == 3 || is_convex(outline)) {
        for k in 1..ids.len() - 1 {
            out.push([ids[0], ids[k], ids[k + 1]]);
        }
    } else if total > MAX_POINTS {
        return false;
    } else {
        // 4. Everything else by ear clipping.  earcut asserts on some
        //    malformed input, so a panic gives a fan of the outline.
        let mut holes = Vec::new();
        let mut index: Vec<u32> = Vec::with_capacity(total);
        let mut coords: Vec<[f64; 2]> = Vec::with_capacity(total);
        for (k, (ring, ids)) in rings2.iter().enumerate() {
            if k > 0 {
                holes.push(coords.len() as u32);
            }
            coords.extend(ring.iter().map(|p| [p.x, p.y]));
            index.extend(ids.iter());
        }
        let tris = std::panic::catch_unwind(|| {
            let mut tris: Vec<u32> = Vec::new();
            earcut::Earcut::new().earcut(coords.iter().copied(), &holes, &mut tris);
            tris
        })
        .unwrap_or_default();
        if tris.len() >= 3 && tris.iter().all(|&t| (t as usize) < index.len()) {
            out.extend(
                tris.chunks_exact(3).map(|t| [index[t[0] as usize], index[t[1] as usize], index[t[2] as usize]]),
            );
        } else {
            for k in 1..ids.len() - 1 {
                out.push([ids[0], ids[k], ids[k + 1]]);
            }
        }
    }
    // 5. Wound around the normal; triangles without area dropped.
    let mut k = start;
    while k < out.len() {
        let [a, b, c] = out[k];
        let (pa, pb, pc) = (vertices[a as usize], vertices[b as usize], vertices[c as usize]);
        let cross = (pb - pa).cross(pc - pa);
        let d = cross.dot(normal);
        if !d.is_finite() || cross.length_squared() == 0.0 {
            out.swap_remove(k);
            continue;
        }
        if d < 0.0 {
            out[k] = [a, c, b];
        }
        k += 1;
    }
    out.len() > start
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Found by fuzzing (Blender's copy of the bug): an outline of a few
    /// repeated points costs ear clipping time by the square of its points
    /// or worse.  Past the cap the face is skipped at once.
    #[test]
    fn huge_degenerate_face_is_skipped_quickly() {
        let n = 50_000u32;
        let v: Vec<DVec3> = (0..n).map(|i| DVec3::new((i % 3) as f64, ((i * 7) % 3) as f64, 0.0)).collect();
        let started = std::time::Instant::now();
        let mut out = Vec::new();
        assert!(!face(&v, &[(0..n).collect()], DVec3::Z, &mut out));
        assert!(started.elapsed().as_secs_f32() < 5.0, "{:?}", started.elapsed());
    }

    #[test]
    fn squares_with_holes_and_notches() {
        let v: Vec<DVec3> =
            [(0.0, 0.0), (4.0, 0.0), (4.0, 4.0), (0.0, 4.0), (1.0, 1.0), (1.0, 3.0), (3.0, 3.0), (3.0, 1.0)]
                .iter()
                .map(|&(x, y)| DVec3::new(x, y, 2.0))
                .collect();
        let area = |tris: &[[u32; 3]]| -> f64 {
            tris.iter()
                .map(|t| {
                    let (a, b, c) = (v[t[0] as usize], v[t[1] as usize], v[t[2] as usize]);
                    (b - a).cross(c - a).dot(DVec3::Z) * 0.5
                })
                .sum()
        };
        let mut out = Vec::new();
        assert!(face(&v, &[vec![0, 1, 2, 3], vec![4, 5, 6, 7]], DVec3::Z, &mut out));
        assert!((area(&out) - 12.0).abs() < 1e-9, "{}", area(&out));
        // Facing down: the same triangles wound the other way.
        out.clear();
        assert!(face(&v, &[vec![0, 1, 2, 3]], -DVec3::Z, &mut out));
        assert!((area(&out) + 16.0).abs() < 1e-9);
        // An L shape is concave.
        let l: Vec<DVec3> = [(0.0, 0.0), (2.0, 0.0), (2.0, 1.0), (1.0, 1.0), (1.0, 2.0), (0.0, 2.0)]
            .iter()
            .map(|&(x, y)| DVec3::new(x, 0.0, y))
            .collect();
        out.clear();
        assert!(face(&l, &[vec![0, 1, 2, 3, 4, 5]], DVec3::NEG_Y, &mut out));
        assert_eq!(out.len(), 4);
        // Collinear points give nothing.
        out.clear();
        let line = [DVec3::ZERO, DVec3::X, DVec3::X * 2.0];
        assert!(!face(&line, &[vec![0, 1, 2]], DVec3::Z, &mut out));
    }
}
