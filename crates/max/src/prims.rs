//! 3ds Max's standard primitives rebuilt from their parameters: Box,
//! Sphere, GeoSphere, Cylinder, Cone, Tube, Torus, Pyramid and Plane.  Each
//! keeps its parameters in a parameter block (its first reference),
//! numbered as the Max SDK's samples number them.  Shapes, sizes, pivots
//! and segment counts follow Max; slices, hemispheres and the UV layouts
//! are simpler.  Coordinates are Max's own: Z up.

use crate::file::{MaxFile, param_float};
use glam::{Vec2, Vec3};
use slerp_polymesh::{Layer, PolyMesh, Smoothing};
use std::f32::consts::{PI, TAU};

/// Most segments in any direction: Max's own spinners stop at 200.
const MAX_SEGMENTS: f32 = 512.0;

/// The primitive object `obj` makes, with its type's name, or `None` for
/// objects that aren't primitives Slerp rebuilds.
pub fn build(file: &MaxFile, obj: usize) -> Option<(PolyMesh, &'static str)> {
    let class = file.class(obj)?;
    let kind = match (class.id, class.name.as_str()) {
        ((0x10, 0), _) => "Box",
        ((0x11, 0), _) => "Sphere",
        ((0x12, 0), _) => "Cylinder",
        ((0x20, 0), _) => "Torus",
        ((0xA86C_23DD, 0), _) => "Cone",
        ((0x7B21, 0), _) => "Tube",
        ((0x76BF_318A, 0x4BF3_7B10), _) => "Pyramid",
        ((0x081F_1DFC, 0x7756_6F65), _) => "Plane",
        (_, "GeoSphere") => "GeoSphere",
        _ => return None,
    };
    let pb = file.reference(obj, 0)?;
    let f = |id: u16, d: f32| param_float(file, pb, id).filter(|v| v.is_finite()).unwrap_or(d);
    let seg = |id: u16, d: f32, min: f32| f(id, d).round().clamp(min, MAX_SEGMENTS) as u32;
    let poly = match kind {
        "Box" => {
            boxed(Vec3::new(f(1, 1.0), f(0, 1.0), f(2, 1.0)), [seg(3, 1.0, 1.0), seg(4, 1.0, 1.0), seg(5, 1.0, 1.0)])
        }
        "Sphere" => {
            let r = f(0, 1.0);
            let around = seg(1, 32.0, 4.0);
            let rows = (around / 2).max(2);
            let profile: Vec<Vec2> = (0..=rows)
                .map(|i| {
                    let a = PI * i as f32 / rows as f32;
                    let x = if i == 0 || i == rows { 0.0 } else { r * a.sin() };
                    Vec2::new(x, -r * a.cos())
                })
                .collect();
            let mut p = revolve(&profile, around, false, &[]);
            // "Base to pivot" puts the bottom on the pivot.
            if f(5, 0.0) != 0.0 {
                p.positions.iter_mut().for_each(|q| q.z += r);
            }
            p
        }
        "Cylinder" => {
            let (r, h) = (f(0, 1.0), f(1, 1.0));
            pipe(&[r], r, h, seg(2, 1.0, 1.0), seg(3, 1.0, 1.0), seg(4, 18.0, 3.0))
        }
        "Cone" => {
            let (r1, r2, h) = (f(0, 1.0), f(1, 0.0), f(2, 1.0));
            pipe(&[r1], r2, h, seg(3, 1.0, 1.0), seg(4, 1.0, 1.0), seg(5, 24.0, 3.0))
        }
        "Tube" => {
            let (r1, r2, h) = (f(0, 1.0), f(1, 0.5), f(2, 1.0));
            tube(r1, r2, h, seg(3, 1.0, 1.0), seg(4, 1.0, 1.0), seg(5, 18.0, 3.0))
        }
        "Torus" => {
            let (r1, r2) = (f(0, 1.0), f(1, 0.25));
            let (twist, around, sides) = (f(3, 0.0), seg(4, 24.0, 3.0), seg(5, 12.0, 3.0));
            let profile: Vec<Vec2> = (0..sides)
                .map(|i| {
                    let a = TAU * i as f32 / sides as f32 + twist;
                    Vec2::new(r1 + r2 * a.cos(), r2 * a.sin())
                })
                .collect();
            revolve(&profile, around, true, &[])
        }
        "Pyramid" => pyramid(f(0, 1.0), f(1, 1.0), f(2, 1.0)),
        "Plane" => plane(f(1, 1.0), f(0, 1.0), seg(3, 4.0, 1.0), seg(2, 4.0, 1.0)),
        "GeoSphere" => geosphere(f(0, 1.0), seg(1, 4.0, 1.0).min(128)),
        _ => return None,
    };
    Some((poly, kind))
}

fn new_poly() -> PolyMesh {
    let mut p = PolyMesh::new();
    p.smoothing = Smoothing::Groups;
    p.uvs.push(Layer { name: "Map channel 1".into(), values: Vec::new() });
    p
}

fn push(p: &mut PolyMesh, verts: &[u32], uvs: &[Vec2], groups: u32) {
    p.push_face(verts, 0, groups);
    p.uvs[0].values.extend_from_slice(uvs);
}

/// A box with its base centred on the pivot: `size` is width (X), length
/// (Y) and height (Z), `div` the segments along each.  Each side has a
/// smoothing group of its own, so the sides stay flat.
fn boxed(size: Vec3, div: [u32; 3]) -> PolyMesh {
    let mut p = new_poly();
    let half = Vec3::new(size.x / 2.0, size.y / 2.0, size.z / 2.0);
    let centre = Vec3::new(0.0, 0.0, size.z / 2.0);
    let sides: [(Vec3, Vec3, Vec3, u32, u32); 6] = [
        (Vec3::Z, Vec3::X, Vec3::Y, div[0], div[1]),
        (-Vec3::Z, -Vec3::X, Vec3::Y, div[0], div[1]),
        (Vec3::Y, -Vec3::X, Vec3::Z, div[0], div[2]),
        (-Vec3::Y, Vec3::X, Vec3::Z, div[0], div[2]),
        (Vec3::X, Vec3::Y, Vec3::Z, div[1], div[2]),
        (-Vec3::X, -Vec3::Y, Vec3::Z, div[1], div[2]),
    ];
    let mut index: std::collections::HashMap<[i64; 3], u32> = std::collections::HashMap::new();
    for (k, (n, u, v, nu, nv)) in sides.into_iter().enumerate() {
        let c = centre + n * half;
        let (su, sv) = ((u * half).length(), (v * half).length());
        for j in 0..nv {
            for i in 0..nu {
                let quad = [(i, j), (i + 1, j), (i + 1, j + 1), (i, j + 1)];
                let mut verts = [0u32; 4];
                let mut uvs = [Vec2::ZERO; 4];
                for (q, &(a, b)) in quad.iter().enumerate() {
                    let pos =
                        c + u * su * (2.0 * a as f32 / nu as f32 - 1.0) + v * sv * (2.0 * b as f32 / nv as f32 - 1.0);
                    let key =
                        [(pos.x * 1e4).round() as i64, (pos.y * 1e4).round() as i64, (pos.z * 1e4).round() as i64];
                    verts[q] = *index.entry(key).or_insert_with(|| {
                        p.positions.push(pos);
                        (p.positions.len() - 1) as u32
                    });
                    uvs[q] = Vec2::new(a as f32 / nu as f32, b as f32 / nv as f32);
                }
                push(&mut p, &verts, &uvs, 1 << k);
            }
        }
    }
    fix_winding(&mut p);
    p
}

/// A plane in XY facing +Z, centred on the pivot.
fn plane(w: f32, l: f32, nu: u32, nv: u32) -> PolyMesh {
    let mut p = new_poly();
    for j in 0..=nv {
        for i in 0..=nu {
            p.positions.push(Vec3::new(w * (i as f32 / nu as f32 - 0.5), l * (j as f32 / nv as f32 - 0.5), 0.0));
        }
    }
    let id = |i: u32, j: u32| j * (nu + 1) + i;
    for j in 0..nv {
        for i in 0..nu {
            let quad = [(i, j), (i + 1, j), (i + 1, j + 1), (i, j + 1)];
            let verts: Vec<u32> = quad.iter().map(|&(a, b)| id(a, b)).collect();
            let uvs: Vec<Vec2> =
                quad.iter().map(|&(a, b)| Vec2::new(a as f32 / nu as f32, b as f32 / nv as f32)).collect();
            push(&mut p, &verts, &uvs, 1);
        }
    }
    p
}

/// A cylinder or cone standing on the pivot: bottom radius `bottom[0]`,
/// top radius `top`, `rows` height segments, `caps` cap rings, `sides`
/// around.  The side is one smoothing group, each cap another.
fn pipe(bottom: &[f32], top: f32, h: f32, rows: u32, caps: u32, sides: u32) -> PolyMesh {
    let r1 = bottom[0];
    let mut profile = Vec::new();
    let mut groups = Vec::new();
    for i in 0..caps {
        profile.push(Vec2::new(r1 * i as f32 / caps as f32, 0.0));
        groups.push(2);
    }
    for i in 0..=rows {
        let t = i as f32 / rows as f32;
        profile.push(Vec2::new(r1 + (top - r1) * t, h * t));
        groups.push(if i < rows { 1 } else { 4 });
    }
    if top > 1e-7 {
        for i in (0..caps).rev() {
            profile.push(Vec2::new(top * i as f32 / caps as f32, h));
            groups.push(4);
        }
    }
    revolve(&profile, sides, false, &groups)
}

/// A tube: two pipes joined at their rims.
fn tube(r1: f32, r2: f32, h: f32, rows: u32, caps: u32, sides: u32) -> PolyMesh {
    let (outer, inner) = (r1.max(r2), r1.min(r2));
    let mut profile = Vec::new();
    let mut groups = Vec::new();
    for i in 0..caps {
        profile.push(Vec2::new(inner + (outer - inner) * i as f32 / caps as f32, 0.0));
        groups.push(2);
    }
    for i in 0..=rows {
        profile.push(Vec2::new(outer, h * i as f32 / rows as f32));
        groups.push(if i < rows { 1 } else { 4 });
    }
    for i in 1..caps {
        profile.push(Vec2::new(outer - (outer - inner) * i as f32 / caps as f32, h));
        groups.push(4);
    }
    for i in 0..rows {
        profile.push(Vec2::new(inner, h - h * i as f32 / rows as f32));
        groups.push(8);
    }
    revolve(&profile, sides, true, &groups)
}

/// Turns a profile (radius, height) around the Z axis in `segments` steps.
/// Points on the axis become single vertices (poles); `closed` joins the
/// last point to the first.  `groups` gives the smoothing groups of the
/// band of faces after each profile point (one group for all when empty).
fn revolve(profile: &[Vec2], segments: u32, closed: bool, groups: &[u32]) -> PolyMesh {
    let mut p = new_poly();
    let n = profile.len();
    let mut ring_start = Vec::with_capacity(n);
    for q in profile {
        ring_start.push(p.positions.len() as u32);
        if q.x.abs() < 1e-7 {
            p.positions.push(Vec3::new(0.0, 0.0, q.y));
        } else {
            for s in 0..segments {
                let a = TAU * s as f32 / segments as f32;
                p.positions.push(Vec3::new(q.x * a.cos(), q.x * a.sin(), q.y));
            }
        }
    }
    let vert = |i: usize, s: u32| if profile[i].x.abs() < 1e-7 { ring_start[i] } else { ring_start[i] + s % segments };
    let rows = if closed { n } else { n - 1 };
    for i in 0..rows {
        let i2 = (i + 1) % n;
        let (v0, v1) = (i as f32 / rows as f32, (i + 1) as f32 / rows as f32);
        for s in 0..segments {
            let (u0, u1) = (s as f32 / segments as f32, (s + 1) as f32 / segments as f32);
            let corners = [(i, s, u0, v0), (i, s + 1, u1, v0), (i2, s + 1, u1, v1), (i2, s, u0, v1)];
            let mut verts = Vec::with_capacity(4);
            let mut uvs = Vec::with_capacity(4);
            for (pi, ps, u, v) in corners {
                let id = vert(pi, ps);
                if verts.last() != Some(&id) && verts.first() != Some(&id) {
                    verts.push(id);
                    uvs.push(Vec2::new(u, v));
                }
            }
            if verts.len() >= 3 {
                push(&mut p, &verts, &uvs, groups.get(i).copied().unwrap_or(1));
            }
        }
    }
    fix_winding(&mut p);
    p
}

/// A four-sided pyramid standing on the pivot.
fn pyramid(w: f32, d: f32, h: f32) -> PolyMesh {
    let mut p = new_poly();
    let (x, y) = (w / 2.0, d / 2.0);
    p.positions = vec![
        Vec3::new(-x, -y, 0.0),
        Vec3::new(x, -y, 0.0),
        Vec3::new(x, y, 0.0),
        Vec3::new(-x, y, 0.0),
        Vec3::new(0.0, 0.0, h),
    ];
    let uv = [Vec2::ZERO, Vec2::X, Vec2::ONE];
    push(&mut p, &[3, 2, 1, 0], &[Vec2::ZERO, Vec2::X, Vec2::ONE, Vec2::Y], 1);
    for (k, (a, b)) in [(0u32, 1u32), (1, 2), (2, 3), (3, 0)].into_iter().enumerate() {
        push(&mut p, &[a, b, 4], &uv, 2 << k);
    }
    fix_winding(&mut p);
    p
}

/// An icosahedron with each face cut into `segs` x `segs` triangles,
/// pushed out onto the sphere: the GeoSphere's default base.
fn geosphere(r: f32, segs: u32) -> PolyMesh {
    let mut p = new_poly();
    let t = (1.0 + 5f32.sqrt()) / 2.0;
    let base = [
        Vec3::new(-1.0, t, 0.0),
        Vec3::new(1.0, t, 0.0),
        Vec3::new(-1.0, -t, 0.0),
        Vec3::new(1.0, -t, 0.0),
        Vec3::new(0.0, -1.0, t),
        Vec3::new(0.0, 1.0, t),
        Vec3::new(0.0, -1.0, -t),
        Vec3::new(0.0, 1.0, -t),
        Vec3::new(t, 0.0, -1.0),
        Vec3::new(t, 0.0, 1.0),
        Vec3::new(-t, 0.0, -1.0),
        Vec3::new(-t, 0.0, 1.0),
    ];
    const FACES: [[usize; 3]; 20] = [
        [0, 11, 5],
        [0, 5, 1],
        [0, 1, 7],
        [0, 7, 10],
        [0, 10, 11],
        [1, 5, 9],
        [5, 11, 4],
        [11, 10, 2],
        [10, 7, 6],
        [7, 1, 8],
        [3, 9, 4],
        [3, 4, 2],
        [3, 2, 6],
        [3, 6, 8],
        [3, 8, 9],
        [4, 9, 5],
        [2, 4, 11],
        [6, 2, 10],
        [8, 6, 7],
        [9, 8, 1],
    ];
    let mut index: std::collections::HashMap<[i64; 3], u32> = std::collections::HashMap::new();
    let mut vertex = |p: &mut PolyMesh, q: Vec3| -> u32 {
        let q = q.normalize_or_zero() * r;
        let key = [(q.x * 1e5).round() as i64, (q.y * 1e5).round() as i64, (q.z * 1e5).round() as i64];
        *index.entry(key).or_insert_with(|| {
            p.positions.push(q);
            (p.positions.len() - 1) as u32
        })
    };
    let uv = |q: Vec3| Vec2::new(0.5 + q.y.atan2(q.x) / TAU, 0.5 + q.normalize_or_zero().z.asin() / PI);
    for f in FACES {
        let (a, b, c) = (base[f[0]], base[f[1]], base[f[2]]);
        let at = |i: u32, j: u32| a + (b - a) * (i as f32 / segs as f32) + (c - a) * (j as f32 / segs as f32);
        for i in 0..segs {
            for j in 0..segs - i {
                let tri = [at(i, j), at(i + 1, j), at(i, j + 1)];
                let v: Vec<u32> = tri.iter().map(|&q| vertex(&mut p, q)).collect();
                push(&mut p, &v, &tri.map(uv), 1);
                if i + j + 1 < segs {
                    let tri = [at(i + 1, j), at(i + 1, j + 1), at(i, j + 1)];
                    let v: Vec<u32> = tri.iter().map(|&q| vertex(&mut p, q)).collect();
                    push(&mut p, &v, &tri.map(uv), 1);
                }
            }
        }
    }
    fix_winding(&mut p);
    p
}

/// Faces must face out: a closed solid has positive volume then.
fn fix_winding(p: &mut PolyMesh) {
    let mut volume = 0.0;
    for f in 0..p.face_count() {
        let r = p.face(f);
        let a = p.positions[p.corner_verts[r.start] as usize];
        for c in r.start + 1..r.end - 1 {
            let (b, d) = (p.positions[p.corner_verts[c] as usize], p.positions[p.corner_verts[c + 1] as usize]);
            volume += a.dot(b.cross(d));
        }
    }
    if volume < 0.0 {
        p.flip_faces();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn volume(p: &PolyMesh) -> f32 {
        let mut v = 0.0;
        for f in 0..p.face_count() {
            let r = p.face(f);
            let a = p.positions[p.corner_verts[r.start] as usize];
            for c in r.start + 1..r.end - 1 {
                v += a.dot(p.positions[p.corner_verts[c] as usize].cross(p.positions[p.corner_verts[c + 1] as usize]));
            }
        }
        v / 6.0
    }

    #[test]
    fn shapes_are_closed_and_face_out() {
        let sphere = {
            let r = 2.0;
            let rows = 16;
            let profile: Vec<Vec2> = (0..=rows)
                .map(|i| {
                    let a = PI * i as f32 / rows as f32;
                    let x = if i == 0 || i == rows { 0.0 } else { r * a.sin() };
                    Vec2::new(x, -r * a.cos())
                })
                .collect();
            revolve(&profile, 32, false, &[])
        };
        let v = volume(&sphere);
        assert!(v > 32.0 && v < 33.6, "{v}");
        // Each face's normal points away from the centre.
        for (f, n) in sphere.face_normals().iter().enumerate() {
            let c = sphere.positions[sphere.corner_verts[sphere.face(f).start] as usize];
            assert!(n.dot(c) > 0.0, "face {f}");
        }
        let b = boxed(Vec3::new(2.0, 3.0, 4.0), [1, 2, 1]);
        assert!((volume(&b) - 24.0).abs() < 1e-3);
        let c = pipe(&[1.0], 1.0, 2.0, 1, 1, 64);
        assert!((volume(&c) - PI * 2.0).abs() < 0.05);
        let cone = pipe(&[1.0], 0.0, 3.0, 2, 1, 64);
        assert!((volume(&cone) - PI).abs() < 0.05);
        let t = tube(2.0, 1.0, 1.0, 1, 1, 64);
        assert!((volume(&t) - PI * 3.0).abs() < 0.1, "{}", volume(&t));
        let g = geosphere(1.0, 4);
        assert!(volume(&g) > 4.0 && volume(&g) < 4.19);
        assert!(volume(&pyramid(1.0, 1.0, 3.0)) > 0.99);
    }

    #[test]
    fn diagonals_split_polygons() {
        assert_eq!(crate::mesh::split_for_tests(5, &[0, 2, 0, 3]).map(|v| v.len()), Some(3));
        assert!(crate::mesh::split_for_tests(4, &[0, 1]).is_none());
    }
}
