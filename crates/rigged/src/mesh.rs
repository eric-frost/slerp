//! Splitting one vertex array into a primitive per material, keeping in
//! each only the vertices its triangles use, as the PMX, PMD, PSK and IQM
//! loaders need: their files hold one vertex array for the whole model.

use slerp_scene::glam::Vec3;
use slerp_scene::{MorphTarget, Primitive, Topology};

/// A copy of `all` with only the triangles `indices` (into `all`'s
/// vertices) and the vertices they use, in their original order.  Returns
/// it with the new index of each old vertex (`u32::MAX` for those left
/// out), for carrying morph targets over.  Indices past the vertices drop
/// their triangle.
pub fn gather(all: &Primitive, indices: &[u32]) -> (Primitive, Vec<u32>) {
    let n = all.positions.len();
    let mut new_of_old = vec![u32::MAX; n];
    let valid = |tri: &&[u32]| tri.iter().all(|&i| (i as usize) < n);
    for tri in indices.chunks_exact(3).filter(valid) {
        for &i in tri {
            new_of_old[i as usize] = 0;
        }
    }
    let keep: Vec<u32> = (0..n as u32).filter(|&i| new_of_old[i as usize] == 0).collect();
    for (new, &old) in keep.iter().enumerate() {
        new_of_old[old as usize] = new as u32;
    }
    let mut out = Primitive { topology: Topology::Triangles, material: all.material, ..Default::default() };
    out.indices = indices.chunks_exact(3).filter(valid).flatten().map(|&i| new_of_old[i as usize]).collect();
    fn pick<T: Copy>(src: &[T], keep: &[u32]) -> Vec<T> {
        if src.is_empty() { Vec::new() } else { keep.iter().map(|&i| src[i as usize]).collect() }
    }
    out.positions = pick(&all.positions, &keep);
    out.normals = pick(&all.normals, &keep);
    out.tangents = pick(&all.tangents, &keep);
    out.uv0 = pick(&all.uv0, &keep);
    out.uv1 = pick(&all.uv1, &keep);
    out.colors = pick(&all.colors, &keep);
    out.joints = pick(&all.joints, &keep);
    out.weights = pick(&all.weights, &keep);
    (out, new_of_old)
}

/// A morph target of a gathered primitive from sparse offsets of the whole
/// vertex array.  Empty (which the renderer takes as "moves nothing") when
/// none of the offsets touch the primitive, so a face's shapes cost
/// nothing on the body's primitives.
pub fn sparse_target(offsets: &[(u32, Vec3)], new_of_old: &[u32], len: usize) -> MorphTarget {
    let mut positions: Vec<Vec3> = Vec::new();
    for &(old, d) in offsets {
        let Some(&new) = new_of_old.get(old as usize) else { continue };
        if new == u32::MAX || !d.is_finite() || d == Vec3::ZERO {
            continue;
        }
        if positions.is_empty() {
            positions = vec![Vec3::ZERO; len];
        }
        positions[new as usize] += d;
    }
    MorphTarget { positions, normals: Vec::new() }
}

/// Swaps the second and third corner of every triangle.
pub fn flip(indices: &mut [u32]) {
    for tri in indices.chunks_exact_mut(3) {
        tri.swap(1, 2);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use slerp_scene::glam::Vec2;

    #[test]
    fn gathers_used_vertices_and_targets() {
        let all = Primitive {
            positions: (0..6).map(|i| Vec3::splat(i as f32)).collect(),
            uv0: (0..6).map(|i| Vec2::splat(i as f32)).collect(),
            ..Default::default()
        };
        let (p, map) = gather(&all, &[4, 2, 5, 2, 4, 9, 0]);
        assert_eq!(p.indices, vec![1, 0, 2]);
        assert_eq!(p.positions, vec![Vec3::splat(2.0), Vec3::splat(4.0), Vec3::splat(5.0)]);
        assert_eq!(p.uv0[2], Vec2::splat(5.0));
        assert_eq!(map[4], 1);
        assert_eq!(map[0], u32::MAX);
        let t = sparse_target(&[(5, Vec3::X), (0, Vec3::Y), (77, Vec3::Z)], &map, p.positions.len());
        assert_eq!(t.positions, vec![Vec3::ZERO, Vec3::ZERO, Vec3::X]);
        assert!(sparse_target(&[(0, Vec3::Y)], &map, 3).positions.is_empty());
        let mut i = vec![0, 1, 2];
        flip(&mut i);
        assert_eq!(i, vec![0, 2, 1]);
    }
}
