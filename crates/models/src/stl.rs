//! STL, binary and ASCII.  STL stores loose triangles with no shared
//! vertices, so meshes are drawn faceted, which also suits the CAD and
//! 3D-printing parts it mostly holds.
//!
//! STL has no units or up axis.  In practice it comes from CAD and slicers,
//! which are Z-up and work in millimetres, so the loader turns Z-up to Y-up
//! and scales millimetres to metres when the part looks like it was measured
//! in millimetres: when its largest side is more than 10 units.  A 10 m part
//! is rarer than a 10 mm one.  Smaller parts are left at 1 unit = 1 m, which
//! only matters for the viewer's metric grid since thumbnails frame whatever
//! they get.  One exception keeps a file Y-up: see `rests_on_y`.

use crate::common::{Reader, check_triangles, parse_f32, srgb8_to_linear, tokens, trim};
use glam::{Quat, Vec3, Vec4};
use slerp_scene::{Aabb, Info, Limits, LoadError, Mesh, Node, Primitive, Scene, Topology, Transform};
use std::path::Path;

/// Parts whose largest side exceeds this are taken to be in millimetres.
const MILLIMETRE_GUESS: f32 = 10.0;

pub fn load(_path: &Path, bytes: &[u8], limits: &Limits) -> Result<Scene, LoadError> {
    let mut scene = Scene::default();
    let parts = if is_binary(bytes) {
        scene.info.format = "STL (binary)".into();
        vec![read_binary(bytes, limits, &mut scene.info)?]
    } else {
        scene.info.format = "STL (ASCII)".into();
        read_ascii(bytes, limits)?
    };
    let mut bounds = Aabb::EMPTY;
    for (name, prim) in parts {
        if prim.positions.is_empty() {
            continue;
        }
        prim.positions.iter().for_each(|&p| bounds.add(p));
        let mesh_index = scene.meshes.len();
        scene.meshes.push(Mesh { name: name.clone(), primitives: vec![prim], target_names: Vec::new() });
        let mut node = Node::new(name);
        node.mesh = Some(mesh_index);
        scene.nodes.push(node);
        scene.roots.push(scene.nodes.len() - 1);
    }
    if scene.meshes.is_empty() {
        return Err(LoadError::invalid("STL holds no triangles"));
    }
    let millimetres = !bounds.is_empty() && bounds.size().max_element() > MILLIMETRE_GUESS;
    let y_up = rests_on_y(&bounds);
    scene
        .info
        .details
        .push(("Units".into(), if millimetres { "millimetres (guessed)" } else { "metres (guessed)" }.into()));
    scene.info.details.push(("Up axis".into(), if y_up { "Y (guessed)" } else { "Z (guessed)" }.into()));
    scene.wrap_roots(Transform {
        rotation: if y_up { Quat::IDENTITY } else { Quat::from_rotation_x(-std::f32::consts::FRAC_PI_2) },
        scale: Vec3::splat(if millimetres { 0.001 } else { 1.0 }),
        ..Transform::IDENTITY
    });
    Ok(scene)
}

/// The exception to Z-up: a part standing on the Y = 0 plane while
/// reaching below Z = 0 was modelled Y-up (old Blender exporters and game
/// tools write such files).  Parts placed in CAD's first octant touch both
/// planes and stay Z-up.
fn rests_on_y(b: &Aabb) -> bool {
    if b.is_empty() {
        return false;
    }
    let tolerance = b.size().max_element() * 0.01;
    b.min.y.abs() <= tolerance && b.min.z < -tolerance
}

/// Binary when the triangle count in the header matches the file size
/// exactly; many binary files start with "solid" too, so that alone
/// doesn't make one ASCII.
fn is_binary(bytes: &[u8]) -> bool {
    if bytes.len() >= 84 {
        let n = u32::from_le_bytes([bytes[80], bytes[81], bytes[82], bytes[83]]) as u64;
        if 84 + 50 * n == bytes.len() as u64 {
            return true;
        }
    }
    let head = trim(&bytes[..bytes.len().min(512)]);
    !(head.starts_with(b"solid") && crate::common::looks_like_text(head))
}

/// Pushes one flat-shaded triangle, skipping ones with broken coordinates.
fn push_triangle(prim: &mut Primitive, p: [Vec3; 3], color: Option<Vec4>) {
    if !p.iter().all(|v| v.is_finite()) {
        return;
    }
    let n = (p[1] - p[0]).cross(p[2] - p[0]).normalize_or_zero();
    let n = if n == Vec3::ZERO { Vec3::Z } else { n };
    for v in p {
        prim.indices.push(prim.positions.len() as u32);
        prim.positions.push(v);
        prim.normals.push(n);
        if let Some(c) = color {
            prim.colors.push(c);
        }
    }
}

fn read_binary(bytes: &[u8], limits: &Limits, info: &mut Info) -> Result<(String, Primitive), LoadError> {
    if bytes.len() < 84 {
        return Err(LoadError::invalid("STL shorter than its header"));
    }
    let header = &bytes[..80];
    let stated = u32::from_le_bytes([bytes[80], bytes[81], bytes[82], bytes[83]]) as usize;
    let present = (bytes.len() - 84) / 50;
    let count = stated.min(present);
    if stated != present {
        info.warnings.push(format!("header says {stated} triangles, file holds {present}"));
    }
    check_triangles(count as u64, limits)?;
    let text: String = header.iter().map(|&b| b as char).filter(|c| c.is_ascii_graphic() || *c == ' ').collect();
    let text = text.trim();
    if !text.is_empty() {
        info.details.push(("Header".into(), text.to_string()));
    }

    // Colours: Materialise Magics marks its files with "COLOR=" in the
    // header and a per-file colour after it; VisCAM and SolidView set bit
    // 15 of each triangle's attribute word.  The two disagree on channel
    // order and on what bit 15 means.
    let magics = crate::common::find(header, b"COLOR=").map(|at| {
        let c = header.get(at + 6..at + 10).unwrap_or(&[204, 204, 204, 255]);
        Vec4::new(srgb8_to_linear(c[0]), srgb8_to_linear(c[1]), srgb8_to_linear(c[2]), c[3] as f32 / 255.0)
    });
    let face_color = |attr: u16| -> Option<Vec4> {
        let c5 = |v: u16| srgb8_to_linear(((v & 31) * 255 / 31) as u8);
        match magics {
            Some(_) if attr & 0x8000 == 0 => Some(Vec4::new(c5(attr), c5(attr >> 5), c5(attr >> 10), 1.0)),
            Some(default) => Some(default),
            None if attr & 0x8000 != 0 => Some(Vec4::new(c5(attr >> 10), c5(attr >> 5), c5(attr), 1.0)),
            None => None,
        }
    };
    let has_colors = magics.is_some()
        || (0..count).any(|i| u16::from_le_bytes([bytes[84 + i * 50 + 48], bytes[84 + i * 50 + 49]]) & 0x8000 != 0);

    let mut prim = Primitive { topology: Topology::Triangles, ..Default::default() };
    prim.positions.reserve_exact(count * 3);
    prim.normals.reserve_exact(count * 3);
    prim.indices.reserve_exact(count * 3);
    let mut r = Reader::new(&bytes[84..]);
    for _ in 0..count {
        let mut v = [0f32; 12];
        for x in &mut v {
            *x = r.f32().unwrap_or(f32::NAN);
        }
        let attr = r.u16().unwrap_or(0);
        let p = [Vec3::new(v[3], v[4], v[5]), Vec3::new(v[6], v[7], v[8]), Vec3::new(v[9], v[10], v[11])];
        let color = has_colors.then(|| face_color(attr).unwrap_or(Vec4::ONE));
        push_triangle(&mut prim, p, color);
    }
    Ok((String::new(), prim))
}

fn read_ascii(bytes: &[u8], limits: &Limits) -> Result<Vec<(String, Primitive)>, LoadError> {
    let mut parts: Vec<(String, Primitive)> = Vec::new();
    let mut current: Option<(String, Primitive)> = None;
    let mut loop_points: Vec<Vec3> = Vec::new();
    let mut triangles: u64 = 0;
    let mut tris = Vec::new();
    for line in bytes.split(|&b| b == b'\n') {
        let mut t = tokens(line);
        let Some(keyword) = t.next() else { continue };
        match keyword.to_ascii_lowercase().as_slice() {
            b"solid" => {
                if let Some(done) = current.take() {
                    parts.push(done);
                }
                let name = String::from_utf8_lossy(trim(line).get(5..).unwrap_or_default()).trim().to_string();
                current = Some((name, Primitive::default()));
            }
            b"endsolid" => {
                if let Some(done) = current.take() {
                    parts.push(done);
                }
            }
            b"outer" => loop_points.clear(),
            b"vertex" => {
                let mut c = [0f32; 3];
                for x in &mut c {
                    *x = t.next().and_then(parse_f32).unwrap_or(f32::NAN);
                }
                if loop_points.len() < 1024 {
                    loop_points.push(Vec3::from(c));
                }
            }
            b"endloop" => {
                let prim = &mut current.get_or_insert_with(|| (String::new(), Primitive::default())).1;
                tris.clear();
                crate::common::triangulate(&loop_points, &mut tris);
                triangles += tris.len() as u64;
                check_triangles(triangles, limits)?;
                for tri in &tris {
                    push_triangle(prim, tri.map(|i| loop_points[i as usize]), None);
                }
                loop_points.clear();
            }
            _ => {}
        }
    }
    if let Some(done) = current.take() {
        parts.push(done);
    }
    Ok(parts)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn binary(tris: &[[f32; 9]], header: &[u8], attr: u16) -> Vec<u8> {
        let mut out = vec![0u8; 80];
        out[..header.len()].copy_from_slice(header);
        out.extend((tris.len() as u32).to_le_bytes());
        for t in tris {
            out.extend([0u8; 12]);
            for v in t {
                out.extend(v.to_le_bytes());
            }
            out.extend(attr.to_le_bytes());
        }
        out
    }

    #[test]
    fn reads_binary_and_converts_axes() {
        let bytes = binary(&[[0.0, 0.0, 0.0, 20.0, 0.0, 0.0, 0.0, 0.0, 20.0]], b"solid but binary", 0);
        let scene = load(Path::new("a.stl"), &bytes, &Limits::viewer()).unwrap();
        assert_eq!(scene.info.format, "STL (binary)");
        assert_eq!(scene.triangle_count(), 1);
        let p = &scene.meshes[0].primitives[0];
        assert_eq!(p.normals[0], Vec3::NEG_Y);
        assert!(p.colors.is_empty());
        // Z-up millimetres become Y-up metres: +Z maps to +Y at 1/1000 scale.
        let pose = slerp_scene::Pose::rest(&scene);
        let b = scene.bounds(&pose);
        assert!((b.max.y - 0.02).abs() < 1e-6, "{b:?}");
    }

    #[test]
    fn keeps_parts_modelled_y_up() {
        // Standing on y = 0 and reaching below z = 0: left as stored.
        let bytes = binary(&[[-1.0, 0.0, -1.0, 1.0, 0.0, 1.0, 0.0, 2.0, 0.0]], b"", 0);
        let scene = load(Path::new("a.stl"), &bytes, &Limits::viewer()).unwrap();
        let b = scene.bounds(&slerp_scene::Pose::rest(&scene));
        assert!((b.max.y - 2.0).abs() < 1e-6 && b.min.y.abs() < 1e-6, "{b:?}");
    }

    #[test]
    fn reads_viscam_colours() {
        let red = 0x8000 | (31 << 10);
        let bytes = binary(&[[0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0]], b"", red);
        let scene = load(Path::new("a.stl"), &bytes, &Limits::viewer()).unwrap();
        assert_eq!(scene.meshes[0].primitives[0].colors[0], Vec4::new(1.0, 0.0, 0.0, 1.0));
    }

    #[test]
    fn reads_ascii_with_several_solids() {
        let text = b"solid one\nfacet normal 0 0 1\n outer loop\n vertex 0 0 0\n vertex 1 0 0\n vertex 0 1 0\n endloop\nendfacet\nendsolid one\n\
solid two\nfacet normal 0 0 1\n outer loop\n vertex 0 0 0\n vertex 1 0 0\n vertex 1 1 0\n vertex 0 1 0\n endloop\nendfacet\nendsolid\n";
        let scene = load(Path::new("a.stl"), text, &Limits::viewer()).unwrap();
        assert_eq!(scene.info.format, "STL (ASCII)");
        assert_eq!(scene.meshes.len(), 2);
        assert_eq!(scene.meshes[0].name, "one");
        assert_eq!(scene.triangle_count(), 3);
    }

    #[test]
    fn survives_truncation_and_garbage() {
        let bytes = binary(&[[0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0]; 4], b"", 0);
        for cut in 0..bytes.len() {
            let _ = load(Path::new("a.stl"), &bytes[..cut], &Limits::viewer());
        }
        let mut lie = bytes.clone();
        lie[80..84].copy_from_slice(&u32::MAX.to_le_bytes());
        let scene = load(Path::new("a.stl"), &lie, &Limits::viewer()).unwrap();
        assert_eq!(scene.triangle_count(), 4);
        assert!(!scene.info.warnings.is_empty());
        let _ = load(Path::new("a.stl"), b"solid x\nvertex 1 2\nendloop\nendloop\n", &Limits::viewer());
        let limits = Limits { max_triangles: 2, ..Limits::viewer() };
        assert!(matches!(load(Path::new("a.stl"), &bytes, &limits), Err(LoadError::TooLarge(_))));
    }
}
