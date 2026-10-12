use super::*;
use glam::{Vec2, Vec3};

/// A unit cube of six quads facing out.
fn cube() -> PolyMesh {
    let mut p = PolyMesh::new();
    for z in [0.0, 1.0] {
        for y in [0.0, 1.0] {
            for x in [0.0, 1.0] {
                p.positions.push(Vec3::new(x, y, z));
            }
        }
    }
    for f in [[0, 2, 3, 1], [4, 5, 7, 6], [0, 1, 5, 4], [2, 6, 7, 3], [0, 4, 6, 2], [1, 3, 7, 5]] {
        p.push_face(&f, 0, 1);
    }
    p
}

#[test]
fn cube_faces_point_out() {
    let p = cube();
    let centre = Vec3::splat(0.5);
    for (f, n) in p.face_normals().iter().enumerate() {
        let c = p.positions[p.corner_verts[p.face(f).start] as usize];
        assert!(n.dot(c - centre) > 0.0, "face {f}");
    }
}

#[test]
fn hard_edges_keep_cube_flat_and_soft_ones_round_it() {
    let mut p = cube();
    p.smoothing = Smoothing::HardEdges;
    // Every edge soft: the corner normals point along the diagonals.
    let n = corner_normals(&p);
    assert!((n[0] - Vec3::splat(-1.0).normalize()).length() < 1e-4);
    // Every edge hard: each corner takes its face's normal.
    for f in 0..p.face_count() {
        let r = p.face(f);
        for c in r.clone() {
            let next = if c + 1 == r.end { r.start } else { c + 1 };
            p.hard_edges.insert(edge_key(p.corner_verts[c], p.corner_verts[next]));
        }
    }
    let n = corner_normals(&p);
    let faces = p.face_normals();
    for f in 0..p.face_count() {
        for c in p.face(f) {
            assert!((n[c] - faces[f]).length() < 1e-5);
        }
    }
}

#[test]
fn smoothing_groups_blend_only_shared_bits() {
    let mut p = cube();
    p.smoothing = Smoothing::Groups;
    // Faces in distinct groups: flat.
    p.face_groups = vec![1, 2, 4, 8, 16, 32];
    let n = corner_normals(&p);
    let faces = p.face_normals();
    for f in 0..6 {
        for c in p.face(f) {
            assert!((n[c] - faces[f]).length() < 1e-5);
        }
    }
    // One group: smooth.
    p.face_groups = vec![1; 6];
    let n = corner_normals(&p);
    assert!((n[0] - Vec3::splat(-1.0).normalize()).length() < 1e-4);
}

#[test]
fn subdividing_a_cube_quadruples_its_faces() {
    let mut p = cube();
    p.uvs.push(Layer { name: "map1".into(), values: vec![Vec2::ZERO; 24] });
    subdivide(&mut p, 2, SubdivideOptions { simple: false, keep_corners: false, use_creases: false, limit: false });
    assert_eq!(p.face_count(), 6 * 16);
    assert_eq!(p.uvs[0].values.len(), p.corner_verts.len());
    // Catmull-Clark pulls the corners in.
    assert!(p.positions.iter().all(|q| q.cmpge(Vec3::splat(-1e-4)).all() && q.cmple(Vec3::splat(1.0001)).all()));
    assert!(p.positions.iter().all(|q| *q != Vec3::ZERO));
}

#[test]
fn primitives_split_by_material_and_flip_v() {
    let mut p = cube();
    p.face_material = vec![0, 0, 0, 1, 1, 1];
    p.uvs.push(Layer { name: "map1".into(), values: vec![Vec2::new(0.25, 0.25); 24] });
    let normals = corner_normals(&p);
    let prims = primitives(&p, &normals, &|_| Wants { uv: [Some(0), None], color: None });
    assert_eq!(prims.len(), 2);
    assert_eq!(prims.iter().map(|(_, p)| p.triangle_count()).sum::<usize>(), 12);
    assert_eq!(prims[0].1.uv0[0], Vec2::new(0.25, 0.75));
}

#[test]
fn faces_with_holes_triangulate_around_them() {
    let square =
        [Vec3::new(0.0, 0.0, 0.0), Vec3::new(4.0, 0.0, 0.0), Vec3::new(4.0, 4.0, 0.0), Vec3::new(0.0, 4.0, 0.0)];
    let hole = [Vec3::new(1.0, 1.0, 0.0), Vec3::new(1.0, 3.0, 0.0), Vec3::new(3.0, 3.0, 0.0), Vec3::new(3.0, 1.0, 0.0)];
    let tris = triangulate_with_holes(&square, &[&hole]);
    assert_eq!(tris.len(), 8);
    let all: Vec<Vec3> = square.iter().chain(&hole).copied().collect();
    let area: f32 = tris.iter().map(|t| (all[t[1]] - all[t[0]]).cross(all[t[2]] - all[t[0]]).z * 0.5).sum();
    assert!((area - 12.0).abs() < 1e-4, "{area}");
}

#[test]
fn validate_drops_broken_faces() {
    let mut p = cube();
    p.push_face(&[0, 1], 0, 0);
    p.push_face(&[0, 1, 99], 0, 0);
    assert_eq!(p.validate(), 2);
    assert_eq!(p.face_count(), 6);
}

/// Found by fuzzing (in Blender's copy of this code): n-gons of a few
/// repeated points cost ear clipping time by the square of their corners or
/// worse.  Faces past the per-face cap or the mesh's budget are fanned.
#[test]
fn degenerate_ngons_triangulate_quickly() {
    let mut p = PolyMesh::new();
    p.positions = (0..9).map(|i| Vec3::new((i % 3) as f32, (i / 3) as f32, 0.0)).collect();
    let mut seed = 7u32;
    for n in [60_000usize, 4_000, 4_000, 4_000, 4_000, 4_000, 4_000, 4_000, 4_000] {
        let corners: Vec<u32> = (0..n)
            .map(|_| {
                seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12_345);
                (seed >> 16) % 9
            })
            .collect();
        p.push_face(&corners, 0, 0);
    }
    let started = std::time::Instant::now();
    let prims = primitives(&p, &[], &|_| Wants::default());
    let triangles: usize = prims.iter().map(|(_, p)| p.indices.len() / 3).sum();
    assert_eq!(triangles, 60_000 - 2 + 8 * (4_000 - 2));
    assert!(started.elapsed().as_secs_f32() < 5.0, "{:?}", started.elapsed());
}
