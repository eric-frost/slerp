//! Shapes the engines generate from a few numbers: Godot's primitive
//! meshes and CSG shapes, Unity's built-in meshes.  Every shape is built
//! with counter-clockwise front faces, outward normals and texture
//! coordinates laid out as the engine lays them out closely enough for
//! tiling textures to look right.

use slerp_scene::Primitive;
use slerp_scene::glam::{Vec2, Vec3};
use std::f32::consts::{PI, TAU};

/// The most segments a generated round shape gets, whatever the file asks:
/// enough for any picture, and a cap on what a hostile number can cost.
const MAX_SEGMENTS: u32 = 256;

fn segments(n: i64, min: u32) -> u32 {
    (n.clamp(min as i64, MAX_SEGMENTS as i64)) as u32
}

/// Appends one flat quad with corners in counter-clockwise order seen from
/// the front.
fn quad(p: &mut Primitive, corners: [Vec3; 4], uvs: [Vec2; 4]) {
    let n = (corners[1] - corners[0]).cross(corners[2] - corners[0]).normalize_or_zero();
    let base = p.positions.len() as u32;
    for (c, uv) in corners.iter().zip(uvs) {
        p.positions.push(*c);
        p.normals.push(n);
        p.uv0.push(uv);
    }
    p.indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
}

/// A box of `size` centred on the origin, each face with the whole texture.
pub(crate) fn cuboid(size: Vec3) -> Primitive {
    let h = size * 0.5;
    let mut p = Primitive::default();
    let uv = [Vec2::new(0.0, 1.0), Vec2::new(1.0, 1.0), Vec2::new(1.0, 0.0), Vec2::new(0.0, 0.0)];
    let v = |x: f32, y: f32, z: f32| Vec3::new(x * h.x, y * h.y, z * h.z);
    // +X, -X, +Y, -Y, +Z, -Z; corners bottom-left, bottom-right,
    // top-right, top-left as seen from outside.
    quad(&mut p, [v(1., -1., 1.), v(1., -1., -1.), v(1., 1., -1.), v(1., 1., 1.)], uv);
    quad(&mut p, [v(-1., -1., -1.), v(-1., -1., 1.), v(-1., 1., 1.), v(-1., 1., -1.)], uv);
    quad(&mut p, [v(-1., 1., 1.), v(1., 1., 1.), v(1., 1., -1.), v(-1., 1., -1.)], uv);
    quad(&mut p, [v(-1., -1., -1.), v(1., -1., -1.), v(1., -1., 1.), v(-1., -1., 1.)], uv);
    quad(&mut p, [v(-1., -1., 1.), v(1., -1., 1.), v(1., 1., 1.), v(-1., 1., 1.)], uv);
    quad(&mut p, [v(1., -1., -1.), v(-1., -1., -1.), v(-1., 1., -1.), v(1., 1., -1.)], uv);
    p
}

/// A plane `size` wide facing `normal` (one of the axes), centred on
/// `offset`, split into `divisions` squares per side.
pub(crate) fn plane(size: Vec2, normal: Vec3, offset: Vec3, divisions: u32) -> Primitive {
    let n = divisions.clamp(1, MAX_SEGMENTS);
    // Axes in the plane: u to the right and v up, as seen from the front.
    let (u_axis, v_axis) = if normal.y.abs() > 0.5 {
        (Vec3::X, Vec3::NEG_Z * normal.y.signum())
    } else if normal.z.abs() > 0.5 {
        (Vec3::X * normal.z.signum(), Vec3::Y)
    } else {
        (Vec3::NEG_Z * normal.x.signum(), Vec3::Y)
    };
    let mut p = Primitive::default();
    for j in 0..=n {
        for i in 0..=n {
            let (s, t) = (i as f32 / n as f32, j as f32 / n as f32);
            p.positions.push(offset + u_axis * (s - 0.5) * size.x + v_axis * (t - 0.5) * size.y);
            p.normals.push(normal);
            p.uv0.push(Vec2::new(s, 1.0 - t));
        }
    }
    let row = n + 1;
    for j in 0..n {
        for i in 0..n {
            let a = j * row + i;
            p.indices.extend_from_slice(&[a, a + 1, a + row + 1, a, a + row + 1, a + row]);
        }
    }
    p
}

/// A sphere of `radius`, squashed or stretched to `height`, optionally only
/// its upper half (whose flat side stays open, as Godot's is).
pub(crate) fn sphere(radius: f32, height: f32, radial: i64, rings: i64, hemisphere: bool) -> Primitive {
    let radial = segments(radial, 3);
    let rings = segments(rings, 2);
    let half_height = height * 0.5;
    let mut p = Primitive::default();
    for r in 0..=rings {
        let v = r as f32 / rings as f32;
        let theta = if hemisphere { v * PI * 0.5 } else { v * PI };
        let (y, ring) = (theta.cos(), theta.sin());
        for s in 0..=radial {
            let u = s as f32 / radial as f32;
            let phi = u * TAU;
            let n = Vec3::new(-phi.sin() * ring, y, -phi.cos() * ring);
            p.positions.push(Vec3::new(
                n.x * radius,
                n.y * if hemisphere { height } else { half_height },
                n.z * radius,
            ));
            p.normals.push(n.normalize_or_zero());
            p.uv0.push(Vec2::new(u, v));
        }
    }
    grid_indices(&mut p, 0, radial, rings);
    p
}

/// Triangles for a grid of `(columns + 1) × (rows + 1)` vertices laid out
/// row by row from `base`, where each row runs anticlockwise around +Y seen
/// from above and rows go downwards.
fn grid_indices(p: &mut Primitive, base: u32, columns: u32, rows: u32) {
    let row = columns + 1;
    for r in 0..rows {
        for s in 0..columns {
            let a = base + r * row + s;
            let b = a + row;
            p.indices.extend_from_slice(&[a, b, a + 1, a + 1, b, b + 1]);
        }
    }
}

/// A capsule `height` tall in all along +Y (two half spheres of `radius`
/// and the cylinder between them).
pub(crate) fn capsule(radius: f32, height: f32, radial: i64, rings: i64) -> Primitive {
    let radial = segments(radial, 3);
    let rings = segments(rings, 1).max(2);
    let mid = (height * 0.5 - radius).max(0.0);
    let mut p = Primitive::default();
    // Latitude rings from the top pole to the bottom one; each hemisphere
    // gets `rings` steps, and its equator ring is duplicated so the
    // cylinder between them has its own straight band.
    let mut lat: Vec<(f32, f32)> = Vec::new();
    for r in 0..=rings {
        lat.push(((r as f32 / rings as f32) * PI * 0.5, mid));
    }
    for r in 0..=rings {
        lat.push((PI * 0.5 + (r as f32 / rings as f32) * PI * 0.5, -mid));
    }
    let total = height.max(2.0 * radius).max(1e-6);
    for &(theta, offset) in &lat {
        let (y, ring) = (theta.cos(), theta.sin());
        let py = y * radius + offset;
        for s in 0..=radial {
            let u = s as f32 / radial as f32;
            let phi = u * TAU;
            let n = Vec3::new(-phi.sin() * ring, y, -phi.cos() * ring);
            p.positions.push(Vec3::new(n.x * radius, py, n.z * radius));
            p.normals.push(n.normalize_or_zero());
            p.uv0.push(Vec2::new(u, 0.5 - py / total));
        }
    }
    grid_indices(&mut p, 0, radial, lat.len() as u32 - 1);
    p
}

/// A cylinder or cone `height` tall along +Y, centred on the origin.
pub(crate) fn cylinder(top: f32, bottom: f32, height: f32, radial: i64, caps: (bool, bool), smooth: bool) -> Primitive {
    let radial = segments(radial, 3);
    let h = height * 0.5;
    let mut p = Primitive::default();
    // 1. The side: smooth shares normals around, faceted gives each side
    //    its own two columns of vertices.
    let slope = (bottom - top) / height.max(1e-6);
    let around = |phi: f32| Vec3::new(-phi.sin(), 0.0, -phi.cos());
    if smooth {
        for (y, r, v) in [(h, top, 0.0), (-h, bottom, 1.0)] {
            for s in 0..=radial {
                let u = s as f32 / radial as f32;
                let d = around(u * TAU);
                p.positions.push(d * r + Vec3::Y * y);
                p.normals.push((d + Vec3::Y * slope).normalize_or_zero());
                p.uv0.push(Vec2::new(u, v * 0.5));
            }
        }
        grid_indices(&mut p, 0, radial, 1);
    } else {
        for s in 0..radial {
            let (u0, u1) = (s as f32 / radial as f32, (s + 1) as f32 / radial as f32);
            let (d0, d1) = (around(u0 * TAU), around(u1 * TAU));
            quad(
                &mut p,
                [d0 * bottom - Vec3::Y * h, d1 * bottom - Vec3::Y * h, d1 * top + Vec3::Y * h, d0 * top + Vec3::Y * h],
                [Vec2::new(u0, 0.5), Vec2::new(u1, 0.5), Vec2::new(u1, 0.0), Vec2::new(u0, 0.0)],
            );
        }
    }
    // 2. Caps, as fans.
    for (on, y, r, up) in [(caps.0, h, top, true), (caps.1, -h, bottom, false)] {
        if !on || r <= 0.0 {
            continue;
        }
        let n = if up { Vec3::Y } else { Vec3::NEG_Y };
        let centre = p.positions.len() as u32;
        p.positions.push(Vec3::Y * y);
        p.normals.push(n);
        p.uv0.push(Vec2::new(0.25 + if up { 0.0 } else { 0.5 }, 0.75));
        for s in 0..=radial {
            let phi = s as f32 / radial as f32 * TAU;
            let d = around(phi);
            p.positions.push(d * r + Vec3::Y * y);
            p.normals.push(n);
            p.uv0.push(Vec2::new(0.25 + d.x * 0.25 + if up { 0.0 } else { 0.5 }, 0.75 + d.z * 0.25));
        }
        for s in 0..radial {
            let (a, b) = (centre + 1 + s, centre + 2 + s);
            // The side runs anticlockwise seen from above.
            if up {
                p.indices.extend_from_slice(&[centre, a, b]);
            } else {
                p.indices.extend_from_slice(&[centre, b, a]);
            }
        }
    }
    p
}

/// A torus around +Y: its tube runs between `inner` and `outer` radius.
pub(crate) fn torus(inner: f32, outer: f32, rings: i64, ring_segments: i64) -> Primitive {
    let rings = segments(rings, 3);
    let sides = segments(ring_segments, 3);
    let centre = (inner + outer) * 0.5;
    let tube = (outer - inner).abs() * 0.5;
    let mut p = Primitive::default();
    for r in 0..=sides {
        let v = r as f32 / sides as f32;
        // From the top of the tube outwards, down and back in.
        let a = v * TAU;
        let (dy, dr) = (a.cos(), a.sin());
        for s in 0..=rings {
            let u = s as f32 / rings as f32;
            let phi = u * TAU;
            let d = Vec3::new(-phi.sin(), 0.0, -phi.cos());
            let n = d * dr + Vec3::Y * dy;
            p.positions.push(d * (centre + tube * dr) + Vec3::Y * (tube * dy));
            p.normals.push(n.normalize_or_zero());
            p.uv0.push(Vec2::new(u, v));
        }
    }
    grid_indices(&mut p, 0, rings, sides);
    p
}

/// Godot's prism: a triangle `size.x` wide and `size.y` tall whose apex
/// sits `left_to_right` of the way across, extruded `size.z` along Z.
pub(crate) fn prism(left_to_right: f32, size: Vec3) -> Primitive {
    let h = size * 0.5;
    let apex_x = -h.x + size.x * left_to_right.clamp(0.0, 1.0);
    let (bl, br, top) = (Vec3::new(-h.x, -h.y, 0.0), Vec3::new(h.x, -h.y, 0.0), Vec3::new(apex_x, h.y, 0.0));
    let (f, b) = (Vec3::Z * h.z, Vec3::NEG_Z * h.z);
    let mut p = Primitive::default();
    let tri = |p: &mut Primitive, c: [Vec3; 3], uv: [Vec2; 3]| {
        let n = (c[1] - c[0]).cross(c[2] - c[0]).normalize_or_zero();
        let base = p.positions.len() as u32;
        for (c, uv) in c.iter().zip(uv) {
            p.positions.push(*c);
            p.normals.push(n);
            p.uv0.push(uv);
        }
        p.indices.extend_from_slice(&[base, base + 1, base + 2]);
    };
    let t = left_to_right.clamp(0.0, 1.0);
    tri(&mut p, [bl + f, br + f, top + f], [Vec2::new(0.0, 1.0), Vec2::new(1.0, 1.0), Vec2::new(t, 0.0)]);
    tri(&mut p, [br + b, bl + b, top + b], [Vec2::new(0.0, 1.0), Vec2::new(1.0, 1.0), Vec2::new(1.0 - t, 0.0)]);
    let uv = [Vec2::new(0.0, 1.0), Vec2::new(1.0, 1.0), Vec2::new(1.0, 0.0), Vec2::new(0.0, 0.0)];
    quad(&mut p, [bl + b, br + b, br + f, bl + f], uv);
    quad(&mut p, [bl + f, top + f, top + b, bl + b], uv);
    quad(&mut p, [br + b, top + b, top + f, br + f], uv);
    p
}

/// A polygon in the XY plane extruded from z = 0 to z = -`depth`, closed
/// at both ends, as Godot's CSGPolygon3D in depth mode.
pub(crate) fn extrude(polygon: &[Vec2], depth: f32) -> Option<Primitive> {
    let caps = triangulate(polygon)?;
    let ccw = signed_area(polygon) > 0.0;
    let mut p = Primitive::default();
    let n = polygon.len();
    let (min, max) = bounds2(polygon);
    let extent = (max - min).max(Vec2::splat(1e-6));
    // 1. Front (+Z) and back caps.
    for (z, normal) in [(0.0, Vec3::Z), (-depth, Vec3::NEG_Z)] {
        let base = p.positions.len() as u32;
        for q in polygon {
            p.positions.push(Vec3::new(q.x, q.y, z));
            p.normals.push(normal);
            p.uv0.push(Vec2::new((q.x - min.x) / extent.x, 1.0 - (q.y - min.y) / extent.y));
        }
        for t in caps.chunks_exact(3) {
            // The front wants triangles anticlockwise seen from +Z, the
            // back clockwise; earcut promises neither.
            let (a, b, c) = (t[0], t[1], t[2]);
            let area = (polygon[b] - polygon[a]).perp_dot(polygon[c] - polygon[a]);
            let (b, c) = if (area > 0.0) == (normal.z > 0.0) { (b, c) } else { (c, b) };
            p.indices.extend_from_slice(&[base + a as u32, base + b as u32, base + c as u32]);
        }
    }
    // 2. The sides.
    let perimeter: f32 = (0..n).map(|i| polygon[i].distance(polygon[(i + 1) % n])).sum::<f32>().max(1e-6);
    let mut along = 0.0;
    for i in 0..n {
        let (a, b) = (polygon[i], polygon[(i + 1) % n]);
        let (a, b, u0, u1) = if ccw { (a, b, along, along) } else { (b, a, along, along) };
        let len = a.distance(b);
        let (u0, u1) = (u0 / perimeter, (u1 + len) / perimeter);
        along += len;
        quad(
            &mut p,
            [
                Vec3::new(a.x, a.y, 0.0),
                Vec3::new(a.x, a.y, -depth),
                Vec3::new(b.x, b.y, -depth),
                Vec3::new(b.x, b.y, 0.0),
            ],
            [Vec2::new(u0, 0.0), Vec2::new(u0, 1.0), Vec2::new(u1, 1.0), Vec2::new(u1, 0.0)],
        );
    }
    Some(p)
}

/// A polygon in the XY plane (x away from the axis) turned `degrees` about
/// +Y in `sides` steps, as Godot's CSGPolygon3D in spin mode.
pub(crate) fn spin(polygon: &[Vec2], degrees: f32, sides: i64) -> Option<Primitive> {
    if polygon.len() < 3 {
        return None;
    }
    let sides = segments(sides, 3);
    let total = degrees.clamp(1.0, 360.0).to_radians();
    let n = polygon.len();
    // Edges in polygon order face away from the polygon's inside when it
    // runs anticlockwise on the positive side of the axis.
    let mean_x = polygon.iter().map(|q| q.x).sum::<f32>() / n as f32;
    let keep = (signed_area(polygon) > 0.0) == (mean_x >= 0.0);
    let mut p = Primitive::default();
    for s in 0..sides {
        let (a0, a1) = (total * s as f32 / sides as f32, total * (s + 1) as f32 / sides as f32);
        let at = |q: Vec2, a: f32| Vec3::new(q.x * a.cos(), q.y, -q.x * a.sin());
        for i in 0..n {
            let (q0, q1) = (polygon[i], polygon[(i + 1) % n]);
            let (q0, q1) = if keep { (q0, q1) } else { (q1, q0) };
            let (u0, u1) = (s as f32 / sides as f32, (s + 1) as f32 / sides as f32);
            quad(
                &mut p,
                [at(q0, a0), at(q0, a1), at(q1, a1), at(q1, a0)],
                [Vec2::new(u0, 1.0), Vec2::new(u1, 1.0), Vec2::new(u1, 0.0), Vec2::new(u0, 0.0)],
            );
        }
    }
    Some(p)
}

/// Triangulates a simple polygon, as index triples into it.
fn triangulate(polygon: &[Vec2]) -> Option<Vec<usize>> {
    // Ear clipping takes time by the square of the points or worse (a ring
    // of repeated points takes seconds at a few thousand).
    if polygon.len() < 3 || polygon.len() > 4096 || polygon.iter().any(|q| !q.is_finite()) {
        return None;
    }
    let flat: Vec<[f64; 2]> = polygon.iter().map(|q| [q.x as f64, q.y as f64]).collect();
    let mut out: Vec<usize> = Vec::new();
    let mut ear = earcut::Earcut::new();
    ear.earcut(flat.iter().copied(), &[], &mut out);
    (!out.is_empty()).then_some(out)
}

fn signed_area(polygon: &[Vec2]) -> f32 {
    let n = polygon.len();
    (0..n).map(|i| polygon[i].perp_dot(polygon[(i + 1) % n])).sum::<f32>() * 0.5
}

fn bounds2(points: &[Vec2]) -> (Vec2, Vec2) {
    points
        .iter()
        .fold((Vec2::splat(f32::INFINITY), Vec2::splat(f32::NEG_INFINITY)), |(lo, hi), q| (lo.min(*q), hi.max(*q)))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Found by fuzzing (Blender's copy of the bug): a polygon of a few
    /// repeated points costs ear clipping time by the square of its points
    /// or worse.  Past the cap it is refused at once.
    #[test]
    fn huge_degenerate_polygon_is_refused_quickly() {
        let polygon: Vec<Vec2> = (0..50_000).map(|i| Vec2::new((i % 3) as f32, ((i * 7) % 3) as f32)).collect();
        let started = std::time::Instant::now();
        assert!(triangulate(&polygon).is_none());
        assert!(started.elapsed().as_secs_f32() < 5.0, "{:?}", started.elapsed());
    }

    /// Every generated shape must wind its faces anticlockwise seen from
    /// outside: each triangle's normal agrees with its vertices' normals,
    /// which the generators compute as the outward direction.
    fn outward(p: &Primitive) -> bool {
        assert_eq!(p.normals.len(), p.positions.len());
        p.indices.chunks_exact(3).all(|t| {
            let [a, b, c] = [t[0], t[1], t[2]].map(|i| p.positions[i as usize]);
            let n = (b - a).cross(c - a);
            let vn: Vec3 = [t[0], t[1], t[2]].iter().map(|&i| p.normals[i as usize]).sum();
            let ok = n.length() < 1e-6 || n.dot(vn) > 0.0;
            if !ok {
                eprintln!("inward triangle {a} {b} {c} normals {vn}");
            }
            ok
        })
    }

    #[test]
    fn generated_shapes_face_outwards() {
        assert!(outward(&cuboid(Vec3::new(1.0, 2.0, 3.0))));
        assert!(outward(&sphere(0.5, 1.0, 16, 8, false)));
        assert!(outward(&capsule(0.5, 2.0, 16, 4)));
        assert!(outward(&cylinder(0.5, 0.5, 2.0, 12, (true, true), true)));
        assert!(outward(&cylinder(0.0, 0.5, 2.0, 12, (true, true), false)));
        assert!(outward(&torus(0.5, 1.0, 16, 8)));
        assert!(outward(&prism(0.5, Vec3::ONE)));
        let square = [Vec2::new(0.0, 0.0), Vec2::new(1.0, 0.0), Vec2::new(1.0, 1.0), Vec2::new(0.0, 1.0)];
        let mut e = extrude(&square, 1.0).unwrap();
        assert!(outward(&e));
        let reversed: Vec<Vec2> = square.iter().rev().copied().collect();
        e = extrude(&reversed, 1.0).unwrap();
        assert!(outward(&e));
        let p = plane(Vec2::new(2.0, 2.0), Vec3::Y, Vec3::ZERO, 2);
        let t = &p.indices[..3];
        let [a, b, c] = [t[0], t[1], t[2]].map(|i| p.positions[i as usize]);
        assert!((b - a).cross(c - a).y > 0.0);
    }

    #[test]
    fn hostile_counts_are_capped() {
        let s = sphere(1.0, 2.0, i64::MAX, i64::MIN, false);
        assert!(s.positions.len() <= ((MAX_SEGMENTS + 1) * (MAX_SEGMENTS + 1)) as usize);
        assert!(extrude(&[Vec2::ZERO, Vec2::ONE], 1.0).is_none());
    }
}
