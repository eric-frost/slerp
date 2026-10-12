//! OFF, Geomview's Object File Format, the plain polygon mesh format of
//! geometry research (Princeton Shape Benchmark, ModelNet, CGAL), in its
//! text and binary forms.
//!
//! The header keyword says what each vertex line holds after its position:
//! `[ST][C][N][4][n]OFF`, where `N` adds a normal, `C` a colour, `ST` a
//! texture coordinate, `4` a homogeneous fourth coordinate and `n` a
//! dimension given on the next line.  Faces are a corner count, the corner
//! indices and an optional colour to the end of the line.  Colours are 0-1
//! floats or 0-255 integers; a single number is a colour-map index, which
//! needs Geomview's map and is ignored.
//!
//! OFF has no units, up axis or materials.  Files are left as they are (Y
//! up, 1 unit = 1 m), with normals smoothed across shallow edges.

use crate::common::{Reader, check_triangles, parse_f32, parse_i64, srgb_color, tokens};
use crate::polymesh::{Attr, Binding, PolyMesh};
use glam::{Vec2, Vec3, Vec4};
use slerp_scene::{Limits, LoadError, Material, Mesh, Node, Scene};
use std::path::Path;

/// What the header keyword declares.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Header {
    tex: bool,
    color: bool,
    normal: bool,
    homogeneous: bool,
    dimension: bool,
}

/// Splits `[ST][C][N][4][n]OFF` into its flags and whatever followed the
/// keyword on the same token (ModelNet writes "OFF490 518 0").
fn keyword(token: &[u8]) -> Option<(Header, &[u8])> {
    let mut h = Header::default();
    let mut t = token;
    for (prefix, flag) in [
        (&b"ST"[..], &mut h.tex),
        (b"C", &mut h.color),
        (b"N", &mut h.normal),
        (b"4", &mut h.homogeneous),
        (b"n", &mut h.dimension),
    ] {
        if let Some(rest) = t.strip_prefix(prefix) {
            *flag = true;
            t = rest;
        }
    }
    Some((h, t.strip_prefix(b"OFF")?))
}

/// True when `head` starts (after comments and blank lines) with an OFF
/// keyword followed by counts or `BINARY`.
pub fn looks_like_off(head: &[u8]) -> bool {
    let mut words = lines(head).flat_map(tokens);
    let Some(first) = words.next() else { return false };
    let Some((h, rest)) = keyword(first) else { return false };
    if rest.is_empty() {
        let next: Vec<&[u8]> = words.take(if h.dimension { 3 } else { 2 }).collect();
        next.first() == Some(&&b"BINARY"[..]) || (next.len() >= 2 && next.iter().all(|t| parse_i64(t).is_some()))
    } else {
        parse_i64(rest).is_some()
    }
}

/// Lines with `#` comments removed, blank ones skipped.
fn lines(bytes: &[u8]) -> impl Iterator<Item = &[u8]> {
    bytes
        .split(|&b| b == b'\n' || b == b'\r')
        .map(|l| l.iter().position(|&b| b == b'#').map_or(l, |c| &l[..c]))
        .filter(|l| l.iter().any(|b| !b.is_ascii_whitespace()))
}

pub fn load(_path: &Path, bytes: &[u8], limits: &Limits) -> Result<Scene, LoadError> {
    let mut lines = lines(bytes);
    // 1. The keyword, and counts that may share its line.
    let first = lines.next().ok_or_else(|| LoadError::invalid("empty OFF file"))?;
    let mut words = tokens(first);
    let (header, rest) = words.next().and_then(keyword).ok_or(LoadError::NotMine)?;
    let mut pending: Vec<&[u8]> = Vec::new();
    if !rest.is_empty() {
        pending.push(rest);
    }
    pending.extend(words);
    if pending.first() == Some(&&b"BINARY"[..]) {
        let at = crate::common::find(bytes, b"BINARY").unwrap_or(0) + 6;
        let body = bytes.get(at..).unwrap_or_default();
        // The binary data starts after the rest of the keyword's line.
        let body = body.iter().position(|&b| b == b'\n').map_or(body, |nl| &body[nl + 1..]);
        return finish(read_binary(header, body, limits)?, header, "OFF (binary)");
    }
    let mut numbers = |count: usize, lines: &mut dyn Iterator<Item = &[u8]>| -> Vec<i64> {
        let mut out: Vec<i64> = pending.drain(..).filter_map(parse_i64).collect();
        while out.len() < count {
            let Some(line) = lines.next() else { break };
            out.extend(tokens(line).map(|t| parse_i64(t).unwrap_or(-1)));
        }
        out
    };
    let mut dim = 3usize;
    if header.dimension {
        let d = numbers(1, &mut lines);
        dim = d
            .first()
            .copied()
            .filter(|&d| (1..=16).contains(&d))
            .ok_or_else(|| LoadError::invalid("bad nOFF dimension"))? as usize;
    }
    let counts = numbers(2, &mut lines);
    let (nv, nf) = match counts[..] {
        [nv, nf, ..] if nv >= 0 && nf >= 0 => (nv as usize, nf as usize),
        _ => return Err(LoadError::invalid("OFF header has no vertex and face counts")),
    };
    // Every vertex and face takes a line of at least a few bytes.
    if nv > bytes.len() / 2 || nf > bytes.len() / 2 {
        return Err(LoadError::invalid(format!("OFF counts {nv} vertices and {nf} faces, more than the file holds")));
    }
    let width = dim + header.homogeneous as usize;

    // 2. Vertices, one per line.
    let mut mesh = PolyMesh::default();
    mesh.positions.reserve(nv);
    let mut normals = Vec::new();
    let mut colors = Vec::new();
    let mut uvs = Vec::new();
    let mut values: Vec<f32> = Vec::new();
    let mut color_tokens: Vec<&[u8]> = Vec::new();
    for _ in 0..nv {
        let Some(line) = lines.next() else { break };
        let words: Vec<&[u8]> = tokens(line).collect();
        values.clear();
        values.extend(words.iter().map(|t| parse_f32(t).unwrap_or(f32::NAN)));
        let get = |i: usize| values.get(i).copied().unwrap_or(0.0);
        let mut p = Vec3::new(get(0), if dim > 1 { get(1) } else { 0.0 }, if dim > 2 { get(2) } else { 0.0 });
        if header.homogeneous {
            let w = get(dim);
            if w != 0.0 && w.is_finite() {
                p /= w;
            }
        }
        mesh.positions.push(p);
        let mut at = width;
        if header.normal {
            normals.push(Vec3::new(get(at), get(at + 1), get(at + 2)));
            at += 3;
        }
        let tail = if header.tex { 2 } else { 0 };
        if header.color {
            let n = words.len().saturating_sub(at + tail).min(4);
            color_tokens.clear();
            color_tokens.extend_from_slice(&words[at.min(words.len())..(at + n).min(words.len())]);
            colors.push(color(&color_tokens).unwrap_or(Vec4::ONE));
            at += n;
        }
        if header.tex {
            uvs.push(Vec2::new(get(at), 1.0 - get(at + 1)));
        }
    }

    // 3. Faces: a corner count, the corners, then an optional colour.
    let mut face_colors: Vec<Vec4> = Vec::new();
    let mut any_face_color = false;
    let mut corners: Vec<u32> = Vec::new();
    for _ in 0..nf {
        let Some(line) = lines.next() else { break };
        let words: Vec<&[u8]> = tokens(line).collect();
        let Some(n) = words.first().and_then(|t| parse_i64(t)).filter(|&n| n >= 0) else { continue };
        let n = n as usize;
        corners.clear();
        corners.extend(
            words.iter().skip(1).take(n).map(|t| parse_i64(t).and_then(|v| u32::try_from(v).ok()).unwrap_or(u32::MAX)),
        );
        if corners.len() < n {
            continue;
        }
        check_triangles(mesh.triangle_count() + n.saturating_sub(2) as u64, limits)?;
        mesh.add_face(&corners);
        let c = color(&words[(1 + n).min(words.len())..]);
        any_face_color |= c.is_some();
        face_colors.push(c.unwrap_or(Material::default().base_color));
    }
    if mesh.face_sizes.is_empty() {
        return Err(LoadError::invalid("OFF file holds no faces"));
    }
    if !normals.is_empty() {
        mesh.normals = Some(Attr::new(normals, Binding::Vertex));
    }
    if !uvs.is_empty() {
        mesh.uvs = Some(Attr::new(uvs, Binding::Vertex));
    }
    if !colors.is_empty() {
        mesh.colors = Some(Attr::new(colors, Binding::Vertex));
    } else if any_face_color {
        mesh.colors = Some(Attr::new(face_colors, Binding::Face));
    }
    let format = match (header.color, header.normal, header.tex) {
        (false, false, false) => "OFF".to_string(),
        _ => format!(
            "{}{}{}OFF",
            if header.tex { "ST" } else { "" },
            if header.color { "C" } else { "" },
            if header.normal { "N" } else { "" }
        ),
    };
    finish(mesh, header, &format)
}

/// A colour from 3 or 4 numbers: integers are 0-255, decimals 0-1.  One
/// number is a colour-map index, which isn't supported.
fn color(words: &[&[u8]]) -> Option<Vec4> {
    if words.len() < 3 {
        return None;
    }
    let words = &words[..words.len().min(4)];
    let integers = words.iter().all(|t| parse_i64(t).is_some());
    let v: Vec<f32> = words.iter().map(|t| parse_f32(t).unwrap_or(0.0)).collect();
    let scale = if integers || v.iter().any(|&c| c > 1.0) { 1.0 / 255.0 } else { 1.0 };
    let a = v.get(3).map_or(1.0, |&a| a * scale);
    Some(srgb_color(v[0] * scale, v[1] * scale, v[2] * scale, a))
}

/// Geomview's binary OFF: big-endian counts, then each vertex's floats,
/// then per face a corner count, its corners, a colour count and that many
/// floats.
fn read_binary(header: Header, body: &[u8], limits: &Limits) -> Result<PolyMesh, LoadError> {
    let mut r = Reader::new(body);
    r.big_endian = true;
    let mut dim = 3usize;
    if header.dimension {
        dim = r.u32().filter(|&d| (1..=16).contains(&d)).ok_or_else(|| LoadError::invalid("bad nOFF dimension"))?
            as usize;
    }
    let (nv, nf) = match (r.u32(), r.u32(), r.u32()) {
        (Some(nv), Some(nf), Some(_)) => (nv as usize, nf as usize),
        _ => return Err(LoadError::invalid("truncated binary OFF header")),
    };
    let per_vertex = dim
        + header.homogeneous as usize
        + if header.normal { 3 } else { 0 }
        + if header.color { 4 } else { 0 }
        + if header.tex { 2 } else { 0 };
    if nv.saturating_mul(per_vertex * 4) > r.remaining() || nf.saturating_mul(8) > r.remaining() {
        return Err(LoadError::invalid("binary OFF counts exceed the file"));
    }
    let mut mesh = PolyMesh::default();
    let (mut colors, mut normals, mut uvs) = (Vec::new(), Vec::new(), Vec::new());
    for _ in 0..nv {
        let v: Vec<f32> = (0..per_vertex).map(|_| r.f32().unwrap_or(f32::NAN)).collect();
        let mut p = Vec3::new(v[0], if dim > 1 { v[1] } else { 0.0 }, if dim > 2 { v[2] } else { 0.0 });
        let mut at = dim;
        if header.homogeneous {
            if v[at] != 0.0 && v[at].is_finite() {
                p /= v[at];
            }
            at += 1;
        }
        mesh.positions.push(p);
        if header.normal {
            normals.push(Vec3::new(v[at], v[at + 1], v[at + 2]));
            at += 3;
        }
        if header.color {
            colors.push(srgb_color(v[at], v[at + 1], v[at + 2], v[at + 3]));
            at += 4;
        }
        if header.tex {
            uvs.push(Vec2::new(v[at], 1.0 - v[at + 1]));
        }
    }
    let mut face_colors = Vec::new();
    let mut corners = Vec::new();
    for _ in 0..nf {
        let Some(n) = r.u32() else { break };
        let n = n as usize;
        if n.saturating_mul(4) > r.remaining() {
            break;
        }
        corners.clear();
        corners.extend((0..n).map(|_| r.u32().unwrap_or(u32::MAX)));
        let nc = r.u32().unwrap_or(0) as usize;
        let c: Vec<f32> = (0..nc.min(4)).map(|_| r.f32().unwrap_or(0.0)).collect();
        for _ in 4..nc.max(4) {
            r.f32();
        }
        check_triangles(mesh.triangle_count() + n.saturating_sub(2) as u64, limits)?;
        mesh.add_face(&corners);
        face_colors.push(if c.len() >= 3 {
            srgb_color(c[0], c[1], c[2], c.get(3).copied().unwrap_or(1.0))
        } else {
            Material::default().base_color
        });
    }
    if !normals.is_empty() {
        mesh.normals = Some(Attr::new(normals, Binding::Vertex));
    }
    if !uvs.is_empty() {
        mesh.uvs = Some(Attr::new(uvs, Binding::Vertex));
    }
    if !colors.is_empty() {
        mesh.colors = Some(Attr::new(colors, Binding::Vertex));
    } else if face_colors.iter().any(|&c| c != Material::default().base_color) {
        mesh.colors = Some(Attr::new(face_colors, Binding::Face));
    }
    Ok(mesh)
}

/// Turns the mesh into a one-node scene.
fn finish(mesh: PolyMesh, _header: Header, format: &str) -> Result<Scene, LoadError> {
    let mut scene = Scene::default();
    scene.info.format = format.to_string();
    let mut warnings = Vec::new();
    let prims = mesh.build(&mut warnings);
    if prims.is_empty() {
        return Err(LoadError::invalid("OFF file holds no usable faces"));
    }
    let colored = mesh.colors.is_some();
    scene.materials.push(Material {
        name: "OFF".into(),
        base_color: if colored { Vec4::ONE } else { Material::default().base_color },
        double_sided: true,
        ..Material::default()
    });
    let primitives = prims
        .into_iter()
        .map(|(_, mut p)| {
            p.material = Some(0);
            p
        })
        .collect();
    scene.meshes.push(Mesh { name: String::new(), primitives, target_names: Vec::new() });
    let mut node = Node::new("");
    node.mesh = Some(0);
    scene.nodes.push(node);
    scene.roots.push(0);
    scene.info.warnings = warnings;
    scene.info.details.push(("Units".into(), "unknown (1 unit = 1 m)".into()));
    Ok(scene)
}

#[cfg(test)]
mod tests {
    use super::*;

    const CUBE: &str = "OFF\n# a cube\n8 6 12\n0 0 0\n1 0 0\n0 1 0\n1 1 0\n0 0 1\n1 0 1\n0 1 1\n1 1 1\n\
        4 0 2 3 1\n4 4 5 7 6\n4 0 1 5 4\n4 2 6 7 3\n4 0 4 6 2 255 0 0\n4 1 3 7 5\n";

    #[test]
    fn reads_variants() {
        assert!(looks_like_off(CUBE.as_bytes()));
        assert!(looks_like_off(b"OFF490 518 0\n"));
        assert!(looks_like_off(b"STCNOFF\n3 1 0\n"));
        assert!(!looks_like_off(b"OFFICE party\n"));
        let s = load(Path::new("c.off"), CUBE.as_bytes(), &Limits::viewer()).unwrap();
        assert_eq!(s.triangle_count(), 12);
        let p = &s.meshes[0].primitives[0];
        // One face is red, the rest default grey.
        assert!(p.colors.iter().any(|c| c.x > 0.9 && c.y < 0.1));
        assert_eq!(s.info.format, "OFF");

        // Colours per vertex (0-255 integers with alpha), normals and UVs.
        let coff = "STCNOFF\n3 1 0\n0 0 0 0 0 1 255 0 0 255 0 0\n1 0 0 0 0 1 0 255 0 255 1 0\n0 1 0 0 0 1 0 0 255 255 0 1\n3 0 1 2\n";
        let s = load(Path::new("c.off"), coff.as_bytes(), &Limits::viewer()).unwrap();
        let p = &s.meshes[0].primitives[0];
        assert_eq!(s.info.format, "STCNOFF");
        assert_eq!(p.colors[1], Vec4::new(0.0, 1.0, 0.0, 1.0));
        assert_eq!(p.normals[0], Vec3::Z);
        assert_eq!(p.uv0[2], Vec2::new(0.0, 0.0));

        // Homogeneous coordinates and counts on the keyword line.
        let s =
            load(Path::new("c.off"), b"4OFF 3 1 0\n0 0 0 2\n2 0 0 2\n0 2 0 2\n3 0 1 2\n", &Limits::viewer()).unwrap();
        assert_eq!(s.meshes[0].primitives[0].positions[1], Vec3::X);

        // A concave polygon triangulates into four triangles.
        let l = "OFF\n6 1 0\n0 0 0\n2 0 0\n2 1 0\n1 1 0\n1 2 0\n0 2 0\n6 0 1 2 3 4 5\n";
        let s = load(Path::new("l.off"), l.as_bytes(), &Limits::viewer()).unwrap();
        assert_eq!(s.triangle_count(), 4);
    }

    #[test]
    fn reads_binary() {
        let mut b = b"OFF BINARY\n".to_vec();
        for v in [3u32, 1, 0] {
            b.extend_from_slice(&v.to_be_bytes());
        }
        for f in [0f32, 0., 0., 1., 0., 0., 0., 1., 0.] {
            b.extend_from_slice(&f.to_be_bytes());
        }
        for v in [3u32, 0, 1, 2, 0] {
            b.extend_from_slice(&v.to_be_bytes());
        }
        let s = load(Path::new("b.off"), &b, &Limits::viewer()).unwrap();
        assert_eq!(s.triangle_count(), 1);
        for cut in 0..b.len() {
            let _ = load(Path::new("b.off"), &b[..cut], &Limits::viewer());
        }
    }

    #[test]
    fn survives_truncation_and_garbage() {
        for cut in 0..CUBE.len() {
            let _ = load(Path::new("c.off"), &CUBE.as_bytes()[..cut], &Limits::viewer());
        }
        for bad in [
            "OFF\n3 4\n0.1 0.2 0.3\n0.2 0.3 0.4\n0.3 0.4 0.5\n0\n2\r20\n4\n",
            "OFF\n99999999 1 0\n",
            "OFF\n3 1 0\n0 0 0\n1 0 0\n0 1 0\n4294967295 0 1 2\n",
        ] {
            let _ = load(Path::new("c.off"), bad.as_bytes(), &Limits::viewer());
        }
        let limits = Limits { max_triangles: 5, ..Limits::viewer() };
        assert!(matches!(load(Path::new("c.off"), CUBE.as_bytes(), &limits), Err(LoadError::TooLarge(_))));
    }
}
