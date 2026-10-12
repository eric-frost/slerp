//! Source displacements: a brush face with four corners becomes a grid of
//! 2^power + 1 vertices a side, each moved off the flat face along its
//! own direction, as VBSP builds them (`CCoreDispInfo::GenerateDispSurf`):
//!
//! - The face's corners, wound as the compiler winds faces (clockwise seen
//!   from the front), start at the one nearest `startposition`.
//! - Vertex (row `i`, column `j`) lies `i` steps from corner 0 towards
//!   corner 1, and `j` steps across towards the edge from corner 3 to 2;
//!   the file's `rowN` lists hold row N's columns.
//! - Its position is that flat point plus `elevation` along the face
//!   normal, plus its offset, plus its normal times its distance.
//!
//! Texture coordinates come from the flat points, so textures don't
//! stretch over hills.  Alphas (0 to 255) blend a second texture in; the
//! renderer has one texture per material, so the blend is drawn as a
//! second, see-through copy of the grid lifted a hair above the first.

use super::brush::TexVecs;
use super::level::Surface;
use slerp_scene::glam::{DVec3, Vec2, Vec3, Vec4};

/// How far the blended second layer floats above the first, in map units
/// (about 1.3 cm), enough for depth tests to keep them apart at a
/// level's distances.
const LAYER_LIFT: f64 = 0.5;

/// A side's `dispinfo` block.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct DispInfo {
    /// 2, 3 or 4.
    pub power: u32,
    pub start: DVec3,
    pub elevation: f64,
    /// One per vertex, row by row.
    pub normals: Vec<DVec3>,
    pub distances: Vec<f64>,
    pub offsets: Vec<DVec3>,
    /// 0 to 255; empty when the file has none.
    pub alphas: Vec<f32>,
}

impl DispInfo {
    /// Vertices along a side.
    pub fn side(&self) -> usize {
        (1usize << self.power.clamp(1, 4)) + 1
    }
}

/// The displaced grid of the face with corners `face` (clockwise from the
/// front, as `brush::build` makes them) and outward `normal`, using
/// `material`; and when `blend` is given, the second layer with its
/// material, see-through where the alphas are low.
pub(crate) fn build(
    info: &DispInfo,
    face: &[DVec3],
    normal: DVec3,
    vecs: &TexVecs,
    size: (f64, f64),
    material: usize,
    blend: Option<usize>,
) -> Option<(Surface, Option<Surface>)> {
    if face.len() != 4 || !(2..=4).contains(&info.power) {
        return None;
    }
    let n = info.side();
    let count = n * n;
    if info.normals.len() != count || info.distances.len() != count || info.offsets.len() != count {
        return None;
    }

    // 1. Corners from the one nearest the start position.
    let start = (0..4)
        .min_by(|&a, &b| face[a].distance_squared(info.start).total_cmp(&face[b].distance_squared(info.start)))?;
    let p: Vec<DVec3> = (0..4).map(|k| face[(start + k) % 4]).collect();

    // 2. Flat and displaced positions.
    let step = 1.0 / (n - 1) as f64;
    let mut flat = Vec::with_capacity(count);
    let mut positions = Vec::with_capacity(count);
    for i in 0..n {
        let a = p[0] + (p[1] - p[0]) * (i as f64 * step);
        let b = p[3] + (p[2] - p[3]) * (i as f64 * step);
        for j in 0..n {
            let f = a + (b - a) * (j as f64 * step);
            let k = i * n + j;
            flat.push(f);
            positions.push(f + normal * info.elevation + info.offsets[k] + info.normals[k] * info.distances[k]);
        }
    }
    if positions.iter().any(|v| !v.is_finite() || v.abs().max_element() > super::brush::MAX_WORLD) {
        return None;
    }
    let uvs: Vec<Vec2> = flat
        .iter()
        .map(|&f| {
            let st = vecs.at(f);
            Vec2::new((st.x / size.0) as f32, (st.y / size.1) as f32)
        })
        .collect();

    // 3. Triangles in VBSP's alternating pattern, turned to face along the
    //    face's normal.
    let mut indices: Vec<u32> = Vec::with_capacity((n - 1) * (n - 1) * 6);
    for i in 0..n - 1 {
        for j in 0..n - 1 {
            let k = (i * n + j) as u32;
            let w = n as u32;
            if (k % 2) == 1 {
                indices.extend_from_slice(&[k, k + w, k + 1, k + 1, k + w, k + w + 1]);
            } else {
                indices.extend_from_slice(&[k, k + w, k + w + 1, k, k + w + 1, k + 1]);
            }
        }
    }
    let first = (flat[indices[1] as usize] - flat[indices[0] as usize])
        .cross(flat[indices[2] as usize] - flat[indices[0] as usize]);
    if first.dot(normal) < 0.0 {
        for t in indices.chunks_exact_mut(3) {
            t.swap(1, 2);
        }
    }

    // 4. Smooth normals: each vertex averages the triangles around it.
    let mut sums = vec![DVec3::ZERO; count];
    for t in indices.chunks_exact(3) {
        let [a, b, c] = [t[0], t[1], t[2]].map(|i| positions[i as usize]);
        let fnormal = (b - a).cross(c - a);
        for &i in t {
            sums[i as usize] += fnormal;
        }
    }
    let normals: Vec<Vec3> = sums
        .iter()
        .map(|s| if s.length_squared() > 1e-18 { s.normalize().as_vec3() } else { normal.as_vec3() })
        .collect();

    let base = Surface {
        material,
        positions: positions.clone(),
        uvs: uvs.clone(),
        normals: normals.clone(),
        colors: Vec::new(),
        indices: indices.clone(),
    };

    // 5. The blended layer: only the triangles that show any of it.
    let layer = blend.filter(|_| info.alphas.len() == count && info.alphas.iter().any(|&a| a > 0.5)).map(|m| {
        let alpha = |i: u32| (info.alphas[i as usize] / 255.0).clamp(0.0, 1.0);
        let kept: Vec<u32> =
            indices.chunks_exact(3).filter(|t| t.iter().any(|&i| alpha(i) > 0.002)).flatten().copied().collect();
        Surface {
            material: m,
            positions: positions.iter().zip(&normals).map(|(p, n)| *p + n.as_dvec3() * LAYER_LIFT).collect(),
            uvs,
            normals,
            colors: (0..count as u32).map(|i| Vec4::new(1.0, 1.0, 1.0, alpha(i))).collect(),
            indices: kept,
        }
    });
    Some((base, layer.filter(|l| !l.indices.is_empty())))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 512x512 floor at z = 0 with its corners clockwise from above, as
    /// a brush's top face is wound.
    fn floor() -> Vec<DVec3> {
        vec![
            DVec3::new(-256.0, 256.0, 0.0),
            DVec3::new(256.0, 256.0, 0.0),
            DVec3::new(256.0, -256.0, 0.0),
            DVec3::new(-256.0, -256.0, 0.0),
        ]
    }

    fn info(power: u32) -> DispInfo {
        let n = (1usize << power) + 1;
        DispInfo {
            power,
            start: DVec3::new(-256.0, -256.0, 0.0),
            elevation: 0.0,
            normals: vec![DVec3::Z; n * n],
            distances: vec![0.0; n * n],
            offsets: vec![DVec3::ZERO; n * n],
            alphas: Vec::new(),
        }
    }

    #[test]
    fn grids_start_at_the_start_position_and_follow_rows() {
        let mut d = info(2);
        // Raise row 1, column 3 by 32 units.
        d.distances[5 + 3] = 32.0;
        let vecs = TexVecs::axes(DVec3::X, -DVec3::Y, [0.0, 0.0], [1.0, 1.0]);
        let (s, layer) = build(&d, &floor(), DVec3::Z, &vecs, (128.0, 128.0), 0, None).unwrap();
        assert!(layer.is_none());
        assert_eq!(s.positions.len(), 25);
        // Corner 0 is the start; row 0 runs to the next corner clockwise
        // (towards +Y, as the floor is wound), columns across to +X.
        assert_eq!(s.positions[0], DVec3::new(-256.0, -256.0, 0.0));
        assert_eq!(s.positions[4], DVec3::new(256.0, -256.0, 0.0), "column 4 of row 0");
        assert_eq!(s.positions[20], DVec3::new(-256.0, 256.0, 0.0), "row 4, column 0");
        assert_eq!(s.positions[5 + 3], DVec3::new(128.0, -128.0, 32.0));
        // Texture coordinates come from the flat face.
        assert_eq!(s.uvs[5 + 3], Vec2::new(1.0, 1.0));
        // Every triangle faces up, and the raised vertex's normal leans
        // away from nothing in particular but stays upward.
        for t in s.indices.chunks(3) {
            let [a, b, c] = [t[0], t[1], t[2]].map(|i| s.positions[i as usize]);
            assert!((b - a).cross(c - a).z > 0.0);
        }
        assert!(s.normals.iter().all(|n| n.z > 0.0));
        assert_eq!(s.indices.len(), 4 * 4 * 6);
    }

    #[test]
    fn powers_and_elevation() {
        for power in 2..=4 {
            let mut d = info(power);
            d.elevation = 8.0;
            let (s, _) = build(
                &d,
                &floor(),
                DVec3::Z,
                &TexVecs::axes(DVec3::X, DVec3::Y, [0.0; 2], [1.0; 2]),
                (1.0, 1.0),
                0,
                None,
            )
            .unwrap();
            let n = d.side();
            assert_eq!(s.positions.len(), n * n);
            assert!(s.positions.iter().all(|p| p.z == 8.0));
        }
        let mut bad = info(2);
        bad.power = 7;
        assert!(
            build(
                &bad,
                &floor(),
                DVec3::Z,
                &TexVecs::axes(DVec3::X, DVec3::Y, [0.0; 2], [1.0; 2]),
                (1.0, 1.0),
                0,
                None
            )
            .is_none()
        );
        let mut short = info(3);
        short.normals.pop();
        assert!(
            build(
                &short,
                &floor(),
                DVec3::Z,
                &TexVecs::axes(DVec3::X, DVec3::Y, [0.0; 2], [1.0; 2]),
                (1.0, 1.0),
                0,
                None
            )
            .is_none()
        );
        assert!(
            build(
                &info(2),
                &floor()[..3],
                DVec3::Z,
                &TexVecs::axes(DVec3::X, DVec3::Y, [0.0; 2], [1.0; 2]),
                (1.0, 1.0),
                0,
                None
            )
            .is_none()
        );
    }

    #[test]
    fn blends_make_a_lifted_see_through_layer() {
        let mut d = info(2);
        d.alphas = vec![0.0; 25];
        d.alphas[24] = 255.0;
        let vecs = TexVecs::axes(DVec3::X, DVec3::Y, [0.0; 2], [1.0; 2]);
        let (base, layer) = build(&d, &floor(), DVec3::Z, &vecs, (1.0, 1.0), 0, Some(1)).unwrap();
        let layer = layer.unwrap();
        assert_eq!(layer.material, 1);
        assert_eq!(layer.colors[24].w, 1.0);
        assert_eq!(layer.colors[0].w, 0.0);
        assert!(layer.indices.len() < base.indices.len(), "only triangles touching the blend");
        assert!((layer.positions[0].z - LAYER_LIFT).abs() < 1e-9);
        // No alphas at all: no layer.
        let (_, none) = build(&info(2), &floor(), DVec3::Z, &vecs, (1.0, 1.0), 0, Some(1)).unwrap();
        assert!(none.is_none());
    }
}
