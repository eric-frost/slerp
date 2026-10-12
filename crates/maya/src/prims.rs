//! Maya's polygon primitives rebuilt from their creation nodes' parameters
//! (`polyCube`, `polySphere`, `polyCylinder`, `polyCone`, `polyPlane`,
//! `polyTorus`), for meshes whose shape exists only as that history.  The
//! shapes, sizes and subdivisions match Maya's; the UV layouts are simpler.

use crate::graph::Node;
use glam::{Quat, Vec2, Vec3};
use slerp_polymesh::{Layer, PolyMesh, Smoothing, edge_key};
use std::f32::consts::TAU;

/// Most subdivisions in any direction: Maya's own sliders stop below.
const MAX_DIVISIONS: f64 = 512.0;

/// The primitive a creation node makes, or `None` for other node types.
pub fn build(node: &Node) -> Option<PolyMesh> {
    let num = |k: &str, d: f64| node.num(k).unwrap_or(d);
    let div = |k: &str, d: f64, min: f64| num(k, d).round().clamp(min, MAX_DIVISIONS) as u32;
    let mut poly = match node.kind.as_str() {
        "polyCube" => cube(
            Vec3::new(num("w", 1.0) as f32, num("h", 1.0) as f32, num("d", 1.0) as f32),
            [div("sw", 1.0, 1.0), div("sh", 1.0, 1.0), div("sd", 1.0, 1.0)],
        ),
        "polyPlane" => plane(num("w", 1.0) as f32, num("h", 1.0) as f32, div("sw", 10.0, 1.0), div("sh", 10.0, 1.0)),
        "polySphere" => {
            let r = num("r", 1.0) as f32;
            let rings = div("sh", 20.0, 2.0);
            let profile: Vec<Vec2> = (0..=rings)
                .map(|i| {
                    let a = std::f32::consts::PI * i as f32 / rings as f32;
                    // The poles exactly on the axis, where sin(pi) isn't.
                    let x = if i == 0 || i == rings { 0.0 } else { r * a.sin() };
                    Vec2::new(x, -r * a.cos())
                })
                .collect();
            revolve(&profile, div("sa", 20.0, 3.0), false, &[])
        }
        "polyCylinder" => {
            let (r, h) = (num("r", 1.0) as f32, num("h", 2.0) as f32);
            let caps = div("sc", 1.0, 0.0).max(1);
            let rows = div("sh", 1.0, 1.0);
            let mut profile = Vec::new();
            for i in 0..caps {
                profile.push(Vec2::new(r * i as f32 / caps as f32, -h / 2.0));
            }
            let bottom = profile.len();
            for i in 0..=rows {
                profile.push(Vec2::new(r, -h / 2.0 + h * i as f32 / rows as f32));
            }
            let top = profile.len() - 1;
            for i in (0..caps).rev() {
                profile.push(Vec2::new(r * i as f32 / caps as f32, h / 2.0));
            }
            revolve(&profile, div("sa", 20.0, 3.0), false, &[bottom, top])
        }
        "polyCone" => {
            let (r, h) = (num("r", 1.0) as f32, num("h", 2.0) as f32);
            let caps = div("sc", 1.0, 0.0).max(1);
            let rows = div("sh", 1.0, 1.0);
            let mut profile = Vec::new();
            for i in 0..caps {
                profile.push(Vec2::new(r * i as f32 / caps as f32, -h / 2.0));
            }
            let base = profile.len();
            for i in 0..=rows {
                let t = i as f32 / rows as f32;
                profile.push(Vec2::new(r * (1.0 - t), -h / 2.0 + h * t));
            }
            revolve(&profile, div("sa", 20.0, 3.0), false, &[base])
        }
        "polyTorus" => {
            let (r, sr) = (num("r", 1.0) as f32, num("sr", 0.5) as f32);
            let rings = div("sh", 20.0, 3.0);
            let twist = (num("tw", 0.0) as f32).to_radians();
            let profile: Vec<Vec2> = (0..rings)
                .map(|i| {
                    let a = TAU * i as f32 / rings as f32 + twist;
                    Vec2::new(r + sr * a.cos(), sr * a.sin())
                })
                .collect();
            revolve(&profile, div("sa", 20.0, 3.0), true, &[])
        }
        _ => return None,
    };
    // The primitive's axis (`.ax`, Y by default).
    if let Some(ax) = node.nums("ax").filter(|a| a.len() >= 3) {
        let axis = Vec3::new(ax[0] as f32, ax[1] as f32, ax[2] as f32);
        if let Some(axis) = axis.try_normalize()
            && (axis - Vec3::Y).length() > 1e-6
        {
            poly.transform(glam::Mat4::from_quat(Quat::from_rotation_arc(Vec3::Y, axis)));
        }
    }
    Some(poly)
}

fn new_poly() -> PolyMesh {
    let mut p = PolyMesh::new();
    p.smoothing = Smoothing::HardEdges;
    p.uvs.push(Layer { name: "map1".into(), values: Vec::new() });
    p
}

fn push(p: &mut PolyMesh, verts: &[u32], uvs: &[Vec2]) {
    p.push_face(verts, 0, 0);
    p.uvs[0].values.extend_from_slice(uvs);
}

/// A box centred on the origin, `div` quads along each side, every edge
/// between two sides hard.
fn cube(size: Vec3, div: [u32; 3]) -> PolyMesh {
    let mut p = new_poly();
    // Each side is its own grid; the shared border vertices are merged by
    // position afterwards so the box is closed.
    let half = size / 2.0;
    let sides: [(Vec3, Vec3, Vec3, u32, u32); 6] = [
        (Vec3::Z, Vec3::X, Vec3::Y, div[0], div[1]),
        (-Vec3::Z, -Vec3::X, Vec3::Y, div[0], div[1]),
        (Vec3::X, -Vec3::Z, Vec3::Y, div[2], div[1]),
        (-Vec3::X, Vec3::Z, Vec3::Y, div[2], div[1]),
        (Vec3::Y, Vec3::X, -Vec3::Z, div[0], div[2]),
        (-Vec3::Y, Vec3::X, Vec3::Z, div[0], div[2]),
    ];
    let mut index: std::collections::HashMap<[i64; 3], u32> = std::collections::HashMap::new();
    let mut vertex = |p: &mut PolyMesh, q: Vec3| -> u32 {
        let key = [(q.x * 1e5).round() as i64, (q.y * 1e5).round() as i64, (q.z * 1e5).round() as i64];
        *index.entry(key).or_insert_with(|| {
            p.positions.push(q);
            (p.positions.len() - 1) as u32
        })
    };
    for (normal, u, v, nu, nv) in sides {
        let centre = normal * half;
        let (su, sv) = ((u * half).length(), (v * half).length());
        let at = |i: u32, j: u32| {
            centre + u * su * (2.0 * i as f32 / nu as f32 - 1.0) + v * sv * (2.0 * j as f32 / nv as f32 - 1.0)
        };
        let uv = |i: u32, j: u32| Vec2::new(i as f32 / nu as f32, j as f32 / nv as f32);
        for j in 0..nv {
            for i in 0..nu {
                let quad = [(i, j), (i + 1, j), (i + 1, j + 1), (i, j + 1)];
                let verts: Vec<u32> = quad.iter().map(|&(a, b)| vertex(&mut p, at(a, b))).collect();
                let uvs: Vec<Vec2> = quad.iter().map(|&(a, b)| uv(a, b)).collect();
                push(&mut p, &verts, &uvs);
            }
        }
    }
    // Edges along the box's own edges are hard: those whose two faces
    // point different ways.
    let normals = p.face_normals();
    let mut first: std::collections::HashMap<(u32, u32), usize> = std::collections::HashMap::new();
    for f in 0..p.face_count() {
        let r = p.face(f);
        for c in r.clone() {
            let next = if c + 1 == r.end { r.start } else { c + 1 };
            let key = edge_key(p.corner_verts[c], p.corner_verts[next]);
            match first.get(&key) {
                Some(&g) if normals[g].dot(normals[f]) < 0.5 => {
                    p.hard_edges.insert(key);
                }
                Some(_) => {}
                None => {
                    first.insert(key, f);
                }
            }
        }
    }
    p
}

/// A grid in the XZ plane facing +Y: `w` along X, `h` along Z.
fn plane(w: f32, h: f32, nu: u32, nv: u32) -> PolyMesh {
    let mut p = new_poly();
    for j in 0..=nv {
        for i in 0..=nu {
            p.positions.push(Vec3::new(w * (i as f32 / nu as f32 - 0.5), 0.0, h * (0.5 - j as f32 / nv as f32)));
        }
    }
    let id = |i: u32, j: u32| j * (nu + 1) + i;
    for j in 0..nv {
        for i in 0..nu {
            let quad = [(i, j), (i + 1, j), (i + 1, j + 1), (i, j + 1)];
            let verts: Vec<u32> = quad.iter().map(|&(a, b)| id(a, b)).collect();
            let uvs: Vec<Vec2> =
                quad.iter().map(|&(a, b)| Vec2::new(a as f32 / nu as f32, b as f32 / nv as f32)).collect();
            push(&mut p, &verts, &uvs);
        }
    }
    p
}

/// Turns a profile (radius, height) around the Y axis in `segments` steps.
/// Points on the axis become single vertices (poles).  `closed` joins the
/// last profile point back to the first (a torus).  The rings at the
/// profile indices in `hard` have hard edges (the rims of caps).
fn revolve(profile: &[Vec2], segments: u32, closed: bool, hard: &[usize]) -> PolyMesh {
    let mut p = new_poly();
    let n = profile.len();
    // 1. Vertices: a ring per profile point, or one on the axis.
    let mut ring_start = Vec::with_capacity(n);
    for q in profile {
        ring_start.push(p.positions.len() as u32);
        if q.x.abs() < 1e-7 {
            p.positions.push(Vec3::new(0.0, q.y, 0.0));
        } else {
            for s in 0..segments {
                let a = TAU * s as f32 / segments as f32;
                p.positions.push(Vec3::new(q.x * a.cos(), q.y, -q.x * a.sin()));
            }
        }
    }
    let vert = |i: usize, s: u32| -> u32 {
        if profile[i].x.abs() < 1e-7 { ring_start[i] } else { ring_start[i] + s % segments }
    };
    // 2. Faces between consecutive rings, wound to face outwards.
    let rows = if closed { n } else { n - 1 };
    for i in 0..rows {
        let i2 = (i + 1) % n;
        let v0 = i as f32 / rows as f32;
        let v1 = (i + 1) as f32 / rows as f32;
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
                push(&mut p, &verts, &uvs);
            }
        }
    }
    for &i in hard {
        if i < n && profile[i].x.abs() >= 1e-7 {
            for s in 0..segments {
                p.hard_edges.insert(edge_key(vert(i, s), vert(i, s + 1)));
            }
        }
    }
    // 3. Faces must face out: a closed solid has positive volume then.
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
    p
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{Attr, Value};

    fn node(kind: &str, attrs: &[(&str, f64)]) -> Node {
        Node {
            kind: kind.into(),
            attrs: attrs
                .iter()
                .map(|(k, v)| Attr { key: k.to_string(), range: None, value: Value::Nums(vec![*v]) })
                .collect(),
            ..Default::default()
        }
    }

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
    fn primitives_are_closed_and_face_out() {
        let cube = build(&node("polyCube", &[("w", 2.0), ("h", 3.0), ("d", 4.0), ("sw", 2.0)])).unwrap();
        assert_eq!(cube.positions.len(), 12);
        assert!((volume(&cube) - 24.0).abs() < 1e-3);
        assert_eq!(cube.hard_edges.len(), 16);
        let sphere = build(&node("polySphere", &[("r", 1.0), ("sa", 32.0), ("sh", 32.0)])).unwrap();
        assert_eq!(sphere.positions.len(), 31 * 32 + 2);
        let v = volume(&sphere);
        assert!(v > 4.0 && v < 4.19, "{v}");
        let cyl = build(&node("polyCylinder", &[("r", 1.0), ("h", 2.0), ("sa", 64.0)])).unwrap();
        assert!((volume(&cyl) - std::f32::consts::PI * 2.0).abs() < 0.05);
        let cone = build(&node("polyCone", &[])).unwrap();
        assert!(volume(&cone) > 0.0);
        let torus = build(&node("polyTorus", &[])).unwrap();
        assert!(volume(&torus) > 0.0);
        let plane = build(&node("polyPlane", &[("sw", 2.0), ("sh", 3.0)])).unwrap();
        assert_eq!(plane.face_count(), 6);
        assert!(plane.face_normals().iter().all(|n| n.y > 0.99));
    }

    #[test]
    fn absurd_subdivisions_are_capped() {
        let p = build(&node("polySphere", &[("sa", 1e12), ("sh", 3.0)])).unwrap();
        assert!(p.positions.len() <= 2 * 512 + 2);
    }
}
