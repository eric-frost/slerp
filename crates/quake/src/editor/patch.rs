//! Quake 3's Bezier patches (`patchDef2`): a grid of control points, odd
//! in both directions, read as 3x3 pieces of biquadratic Bezier surface
//! that share their edge rows.  Each piece is cut into a fixed number of
//! steps per side, a coarse version of what Quake 3 does adaptively.

use super::level::Surface;
use super::mapfile::MapPatch;
use slerp_scene::glam::{DVec3, Vec2, Vec3};

/// Quadratic Bezier weights at `t`.
fn weights(t: f64) -> [f64; 3] {
    [(1.0 - t) * (1.0 - t), 2.0 * t * (1.0 - t), t * t]
}

/// The patch as triangles in map space, with texture coordinates from its
/// control points.  `steps` segments per piece side.
pub(crate) fn tessellate(p: &MapPatch, steps: usize, material: usize) -> Option<Surface> {
    let (rows, cols) = (p.rows, p.columns);
    if rows < 3 || cols < 3 || rows % 2 == 0 || cols % 2 == 0 || p.points.len() != rows * cols {
        return None;
    }
    let steps = steps.max(1);
    let (pieces_r, pieces_c) = ((rows - 1) / 2, (cols - 1) / 2);
    let (vr, vc) = (pieces_r * steps + 1, pieces_c * steps + 1);
    let ctrl = |r: usize, c: usize| &p.points[r * cols + c];

    // 1. Evaluate every vertex of the shared grid: which piece it falls in
    //    and where.
    let locate = |i: usize, pieces: usize| {
        let piece = (i / steps).min(pieces - 1);
        (piece * 2, (i - piece * steps) as f64 / steps as f64)
    };
    let mut positions = Vec::with_capacity(vr * vc);
    let mut uvs = Vec::with_capacity(vr * vc);
    for i in 0..vr {
        let (r0, tr) = locate(i, pieces_r);
        let wr = weights(tr);
        for j in 0..vc {
            let (c0, tc) = locate(j, pieces_c);
            let wc = weights(tc);
            let mut v = [0.0; 5];
            for (a, wa) in wr.iter().enumerate() {
                for (b, wb) in wc.iter().enumerate() {
                    let q = ctrl(r0 + a, c0 + b);
                    for k in 0..5 {
                        v[k] += q[k] * wa * wb;
                    }
                }
            }
            positions.push(DVec3::new(v[0], v[1], v[2]));
            uvs.push(Vec2::new(v[3] as f32, v[4] as f32));
        }
    }
    if positions.iter().any(|v| !v.is_finite()) {
        return None;
    }

    // 2. Normals from the grid's neighbours, and two triangles per cell.
    let at = |i: usize, j: usize| positions[i * vc + j];
    let mut normals = Vec::with_capacity(vr * vc);
    for i in 0..vr {
        for j in 0..vc {
            let du = at((i + 1).min(vr - 1), j) - at(i.saturating_sub(1), j);
            let dv = at(i, (j + 1).min(vc - 1)) - at(i, j.saturating_sub(1));
            let n = dv.cross(du);
            normals.push(if n.length_squared() > 1e-12 { n.normalize().as_vec3() } else { Vec3::Z });
        }
    }
    let mut indices = Vec::with_capacity((vr - 1) * (vc - 1) * 6);
    for i in 0..vr - 1 {
        for j in 0..vc - 1 {
            let k = (i * vc + j) as u32;
            let (right, down) = (k + 1, k + vc as u32);
            indices.extend_from_slice(&[k, right, down, right, down + 1, down]);
        }
    }
    // Make the triangles face along the normals computed above.
    if let Some(t) = indices.get(..3) {
        let [a, b, c] = [t[0], t[1], t[2]].map(|i| positions[i as usize]);
        let n = (b - a).cross(c - a);
        if n.dot(normals[t[0] as usize].as_dvec3()) < 0.0 {
            for tri in indices.chunks_exact_mut(3) {
                tri.swap(1, 2);
            }
        }
    }
    Some(Surface { material, positions, uvs, normals, colors: Vec::new(), indices })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bump() -> MapPatch {
        let mut points = Vec::new();
        for r in 0..3 {
            for c in 0..3 {
                let z = if r == 1 && c == 1 { 64.0 } else { 0.0 };
                points.push([r as f64 * 64.0, c as f64 * 64.0, z, c as f64 / 2.0, r as f64 / 2.0]);
            }
        }
        MapPatch { texture: "t".into(), rows: 3, columns: 3, points }
    }

    #[test]
    fn a_bump_rises_to_a_quarter_of_its_control_point() {
        let s = tessellate(&bump(), 4, 0).unwrap();
        assert_eq!(s.positions.len(), 25);
        assert_eq!(s.indices.len(), 4 * 4 * 6);
        // The centre of a biquadratic patch is at 1/4 of the middle
        // control point's height (weights 1/2 * 1/2).
        assert!((s.positions[12].z - 16.0).abs() < 1e-9);
        assert_eq!(s.uvs[12], Vec2::new(0.5, 0.5));
        // Corners are interpolated exactly.
        assert_eq!(s.positions[0], DVec3::ZERO);
        assert_eq!(s.positions[24], DVec3::new(128.0, 128.0, 0.0));
        // Triangles agree with the normals.
        for t in s.indices.chunks(3) {
            let [a, b, c] = [t[0], t[1], t[2]].map(|i| s.positions[i as usize]);
            assert!((b - a).cross(c - a).dot(s.normals[t[0] as usize].as_dvec3()) > 0.0);
        }
    }

    #[test]
    fn bad_grids_are_refused() {
        let mut p = bump();
        p.points.pop();
        assert!(tessellate(&p, 4, 0).is_none());
        let mut p = bump();
        p.points[4][0] = f64::NAN;
        assert!(tessellate(&p, 4, 0).is_none());
        let mut p = bump();
        p.rows = 2;
        assert!(tessellate(&p, 4, 0).is_none());
    }

    #[test]
    fn pieces_share_their_edges() {
        // A 5x3 grid is two pieces along the rows.
        let mut points = Vec::new();
        for r in 0..5 {
            for c in 0..3 {
                points.push([r as f64 * 32.0, c as f64 * 32.0, 0.0, 0.0, 0.0]);
            }
        }
        let s = tessellate(&MapPatch { texture: "t".into(), rows: 5, columns: 3, points }, 2, 0).unwrap();
        assert_eq!(s.positions.len(), 5 * 3);
        assert_eq!(s.positions[2 * 3], DVec3::new(64.0, 0.0, 0.0));
    }
}
