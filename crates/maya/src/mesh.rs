//! Maya meshes as files store them, and their turn into polygon meshes.
//!
//! A mesh is vertices, edges (two vertices and a hard-or-smooth flag) and
//! faces made of edges: each face lists the edges around its outline, then
//! around any holes, an edge run backwards written as `-(e + 1)`.  UVs,
//! colours and normals belong to face corners.  Maya ASCII spells this out
//! in attributes (`.vt`, `.ed`, `.fc`, `.uvst[i].uvsp`, `.n`); Maya binary
//! packs the same data into one blob, which [`parse_blob`] reads.

use crate::graph::{Face, Loop, Node, Value};
use glam::{Vec2, Vec3, Vec4};
use slerp_polymesh::{Layer, PolyMesh, Smoothing, edge_key, triangulate_with_holes};

/// Most vertices, edges or faces read for one mesh: far beyond what Maya
/// itself handles comfortably, and it keeps a damaged count from asking for
/// gigabytes.
pub const MAX_ELEMENTS: usize = 50_000_000;

/// Normals Maya writes for corners without one of their own.
const NO_NORMAL: f32 = 1e19;

#[derive(Clone, Debug, Default)]
pub struct MeshData {
    pub positions: Vec<Vec3>,
    /// Each edge's two vertices.
    pub edges: Vec<[u32; 2]>,
    /// Whether each edge is hard.
    pub hard: Vec<bool>,
    pub faces: Vec<Face>,
    /// UV sets by their index in the file; `None` for indices it skips.
    pub uv_sets: Vec<Option<UvSet>>,
    pub color_sets: Vec<Option<ColorSet>>,
    /// Normals per face corner, in face order; empty when the file stores
    /// none.
    pub normals: Vec<Vec3>,
}

#[derive(Clone, Debug, Default)]
pub struct UvSet {
    pub name: String,
    pub points: Vec<Vec2>,
}

#[derive(Clone, Debug, Default)]
pub struct ColorSet {
    pub name: String,
    pub colors: Vec<Vec4>,
}

impl MeshData {
    /// Reads a mesh node's stored geometry: its binary blob, or its ASCII
    /// attributes.  `None` when the node stores no faces.
    pub fn from_node(node: &Node) -> Option<MeshData> {
        let mut mesh = node
            .attrs
            .iter()
            .rev()
            .find_map(|a| match &a.value {
                Value::Mesh(m) => Some((**m).clone()),
                _ => None,
            })
            .or_else(|| from_attrs(node))?;
        // Tweaks (`.pt`) move vertices from where the history or the stored
        // points put them.
        if let Some(pt) = node.dense("pt", 3, MAX_ELEMENTS) {
            for (p, d) in mesh.positions.iter_mut().zip(pt.chunks_exact(3)) {
                let d = Vec3::new(d[0] as f32, d[1] as f32, d[2] as f32);
                if d.is_finite() {
                    *p += d;
                }
            }
        }
        (!mesh.faces.is_empty()).then_some(mesh)
    }

    pub fn triangle_estimate(&self) -> usize {
        self.faces.iter().map(|f| f.loops.iter().map(|l| l.edges.len()).sum::<usize>().saturating_sub(2)).sum()
    }
}

fn from_attrs(node: &Node) -> Option<MeshData> {
    let vt = node.dense("vt", 3, MAX_ELEMENTS)?;
    let mut mesh = MeshData {
        positions: vt.chunks_exact(3).map(|p| Vec3::new(p[0] as f32, p[1] as f32, p[2] as f32)).collect(),
        ..Default::default()
    };
    if let Some(ed) = node.dense("ed", 3, MAX_ELEMENTS) {
        for e in ed.chunks_exact(3) {
            mesh.edges.push([e[0].max(0.0) as u32, e[1].max(0.0) as u32]);
            mesh.hard.push(e[2] == 0.0);
        }
    }
    // Faces come in pieces (`.fc[0:499]`, `.fc[500:999]`), in order.
    let mut pieces: Vec<(u32, &Vec<Face>)> = node
        .attrs
        .iter()
        .filter(|a| a.key == "fc")
        .filter_map(|a| match &a.value {
            Value::Faces(f) => Some((a.range.map_or(0, |r| r.0), f)),
            _ => None,
        })
        .collect();
    pieces.sort_by_key(|p| p.0);
    for (start, faces) in pieces {
        let start = start as usize;
        // A gap far beyond the faces so far would only be padding.
        if start > MAX_ELEMENTS || start > mesh.faces.len() + (1 << 16) {
            continue;
        }
        if mesh.faces.len() < start {
            mesh.faces.resize(start, Face::default());
        }
        mesh.faces.truncate(start);
        mesh.faces.extend(faces.iter().cloned());
    }
    for i in node.indices("uvst") {
        let points = node.dense(&format!("uvst[{i}].uvsp"), 2, MAX_ELEMENTS).unwrap_or_default();
        let name = node.text(&format!("uvst[{i}].uvsn")).unwrap_or("map1").to_string();
        set_at(
            &mut mesh.uv_sets,
            i as usize,
            UvSet { name, points: points.chunks_exact(2).map(|p| Vec2::new(p[0] as f32, p[1] as f32)).collect() },
        );
    }
    for i in node.indices("clst") {
        let colors = node.dense(&format!("clst[{i}].clsp"), 4, MAX_ELEMENTS).unwrap_or_default();
        let name = node.text(&format!("clst[{i}].clsn")).unwrap_or("colorSet1").to_string();
        set_at(
            &mut mesh.color_sets,
            i as usize,
            ColorSet {
                name,
                colors: colors
                    .chunks_exact(4)
                    .map(|c| Vec4::new(c[0] as f32, c[1] as f32, c[2] as f32, c[3] as f32))
                    .collect(),
            },
        );
    }
    if let Some(n) = node.dense("n", 3, MAX_ELEMENTS) {
        mesh.normals = n.chunks_exact(3).map(|n| Vec3::new(n[0] as f32, n[1] as f32, n[2] as f32)).collect();
    }
    Some(mesh)
}

fn set_at<T>(list: &mut Vec<Option<T>>, i: usize, value: T) {
    if i > 1024 {
        return;
    }
    if list.len() <= i {
        list.resize_with(i + 1, || None);
    }
    list[i] = Some(value);
}

/*┌─────────────────╮
| Binary blob     |
└─────────────────╯*/
//region Binary blob

/// A big-endian reader over a blob that never reads past its end.
struct Reader<'a> {
    b: &'a [u8],
    at: usize,
}

impl Reader<'_> {
    fn u32(&mut self) -> Option<u32> {
        let v = self.b.get(self.at..self.at + 4)?;
        self.at += 4;
        Some(u32::from_be_bytes([v[0], v[1], v[2], v[3]]))
    }

    /// A count of 4-byte values that must fit in what is left.
    fn count(&mut self) -> Option<usize> {
        let n = self.u32()? as usize;
        (n <= (self.b.len() - self.at) / 4 && n <= MAX_ELEMENTS * 3).then_some(n)
    }

    fn f32s(&mut self, n: usize) -> Option<Vec<f32>> {
        let bytes = self.b.get(self.at..self.at + n * 4)?;
        self.at += n * 4;
        Some(bytes.chunks_exact(4).map(|c| f32::from_be_bytes([c[0], c[1], c[2], c[3]])).collect())
    }

    fn u32s(&mut self, n: usize) -> Option<Vec<u32>> {
        let bytes = self.b.get(self.at..self.at + n * 4)?;
        self.at += n * 4;
        Some(bytes.chunks_exact(4).map(|c| u32::from_be_bytes([c[0], c[1], c[2], c[3]])).collect())
    }

    fn cstr(&mut self) -> Option<String> {
        let rest = self.b.get(self.at..)?;
        let end = rest.iter().position(|&c| c == 0)?;
        self.at += end + 1;
        Some(String::from_utf8_lossy(&rest[..end]).into_owned())
    }
}

/// Reads a `MESH` chunk's data after its attribute name and flag byte:
/// vertices; edges with the hard flag in the top bit of the first vertex;
/// faces as edge references (top bit: reversed; the next two bits mark the
/// end of a loop, both the end of the face); normals per corner; then each
/// UV set's name, points and per-corner indices.  What follows (colour sets,
/// creases) isn't read.
pub fn parse_blob(b: &[u8]) -> Option<MeshData> {
    let mut r = Reader { b, at: 0 };
    let mut mesh = MeshData::default();
    // 1. Vertices.
    let n = r.count()?;
    let p = r.f32s(n)?;
    mesh.positions = p.chunks_exact(3).map(|p| Vec3::new(p[0], p[1], p[2])).collect();
    // 2. Edges.
    let n = r.count()?;
    let e = r.u32s(n)?;
    for pair in e.chunks_exact(2) {
        mesh.edges.push([pair[0] & 0x7FFF_FFFF, pair[1] & 0x7FFF_FFFF]);
        mesh.hard.push(pair[0] & 0x8000_0000 != 0);
    }
    // 3. Faces: loops of edge references.
    let n = r.count()?;
    let refs = r.u32s(n)?;
    let mut face = Face::default();
    let mut lp = Loop::default();
    for x in refs {
        let e = (x & 0x1FFF_FFFF) as i32;
        lp.edges.push(if x & 0x8000_0000 != 0 { -(e + 1) } else { e });
        match (x >> 29) & 3 {
            0 => {}
            3 => {
                face.loops.push(std::mem::take(&mut lp));
                mesh.faces.push(std::mem::take(&mut face));
            }
            _ => face.loops.push(std::mem::take(&mut lp)),
        }
    }
    if !lp.edges.is_empty() {
        face.loops.push(lp);
    }
    if !face.loops.is_empty() {
        mesh.faces.push(face);
    }
    // 4. Normals per corner, then a word that follows them.
    let n = r.count()?;
    if n > 0 {
        let v = r.f32s(n)?;
        mesh.normals = v.chunks_exact(3).map(|n| Vec3::new(n[0], n[1], n[2])).collect();
        r.u32()?;
    }
    // 5. UV sets, each with indices for every corner in face order.  A
    //    damaged set leaves the mesh without UVs rather than failing it.
    let Some(sets) = r.u32() else { return Some(mesh) };
    for s in 0..sets.min(64) {
        let read = (|| {
            r.u32()?;
            let name = r.cstr()?;
            let n = r.count()?;
            let pts = r.f32s(n)?;
            let n = r.count()?;
            let idx = r.u32s(n)?;
            Some((name, pts, idx))
        })();
        let Some((name, pts, idx)) = read else { break };
        let set = UvSet { name, points: pts.chunks_exact(2).map(|p| Vec2::new(p[0], p[1])).collect() };
        let mut k = 0;
        for face in &mut mesh.faces {
            for lp in &mut face.loops {
                let n = lp.edges.len();
                let ids: Vec<i32> = (0..n).map(|j| idx.get(k + j).map_or(-1, |&i| i as i32)).collect();
                k += n;
                lp.uvs.push((s, ids));
            }
        }
        set_at(&mut mesh.uv_sets, s as usize, set);
    }
    Some(mesh)
}

//endregion

/*┌─────────────────╮
| To polygons     |
└─────────────────╯*/
//region To polygons

/// What `to_poly` made besides the mesh.
pub struct Converted {
    pub poly: PolyMesh,
    /// The UV layer of the mesh's current UV set, an index into
    /// `poly.uvs`.
    pub current_uv: Option<usize>,
    /// Faces dropped as broken (edges or vertices out of range).
    pub broken: usize,
}

/// Turns stored mesh data into a polygon mesh: faces without holes stay
/// polygons, faces with holes become triangles around them.  `face_material`
/// gives each face's material; `current_uv` names the UV set to draw with;
/// colours are kept only when `colors` is set (Maya shows vertex colours
/// only with Display Colors on).
pub fn to_poly(
    mesh: &MeshData,
    face_material: &dyn Fn(usize) -> u32,
    current_uv: Option<&str>,
    colors: bool,
) -> Converted {
    let mut poly = PolyMesh::new();
    poly.smoothing = Smoothing::HardEdges;
    poly.positions = mesh.positions.clone();
    // 1. Layers: one per UV and colour set present.
    let uv_layers: Vec<usize> = (0..mesh.uv_sets.len()).filter(|&i| mesh.uv_sets[i].is_some()).collect();
    for &i in &uv_layers {
        let name = mesh.uv_sets[i].as_ref().map(|s| s.name.clone()).unwrap_or_default();
        poly.uvs.push(Layer { name, values: Vec::new() });
    }
    let color_layers: Vec<usize> = if colors {
        (0..mesh.color_sets.len()).filter(|&i| mesh.color_sets[i].is_some()).collect()
    } else {
        Vec::new()
    };
    for &i in &color_layers {
        let name = mesh.color_sets[i].as_ref().map(|s| s.name.clone()).unwrap_or_default();
        poly.colors.push(Layer { name, values: Vec::new() });
    }
    let has_normals = !mesh.normals.is_empty();
    if has_normals {
        poly.custom_normals = Some(Vec::new());
    }
    // 2. Hard edges.
    for (e, &[a, b]) in mesh.edges.iter().enumerate() {
        if mesh.hard.get(e).copied().unwrap_or(false) {
            poly.hard_edges.insert(edge_key(a, b));
        }
    }
    // 3. Faces.  A corner carries its vertex, UVs, colour and normal.
    let nv = mesh.positions.len() as u32;
    let mut broken = 0;
    let mut normal_at = 0usize;
    let mut corners: Vec<Corner> = Vec::new();
    let mut loop_starts: Vec<usize> = Vec::new();
    for (f, face) in mesh.faces.iter().enumerate() {
        corners.clear();
        loop_starts.clear();
        let mut ok = true;
        for lp in &face.loops {
            loop_starts.push(corners.len());
            for (j, &e) in lp.edges.iter().enumerate() {
                let (index, backwards) = if e >= 0 { (e as usize, false) } else { ((-(e as i64) - 1) as usize, true) };
                let Some(&[a, b]) = mesh.edges.get(index) else {
                    ok = false;
                    break;
                };
                let v = if backwards { b } else { a };
                ok &= v < nv;
                let uv = uv_layers
                    .iter()
                    .map(|&set| {
                        let points = &mesh.uv_sets[set].as_ref().map_or(&[][..], |s| &s.points[..]);
                        lp.uvs
                            .iter()
                            .find(|(s, _)| *s as usize == set)
                            .and_then(|(_, ids)| ids.get(j))
                            .and_then(|&i| usize::try_from(i).ok())
                            .and_then(|i| points.get(i))
                            .copied()
                            .unwrap_or(Vec2::ZERO)
                    })
                    .collect();
                let color = color_layers
                    .iter()
                    .map(|&set| {
                        let list = &mesh.color_sets[set].as_ref().map_or(&[][..], |s| &s.colors[..]);
                        lp.colors
                            .iter()
                            .find(|(s, _)| *s as usize == set)
                            .and_then(|(_, ids)| ids.get(j))
                            .and_then(|&i| usize::try_from(i).ok())
                            .and_then(|i| list.get(i))
                            .copied()
                            .unwrap_or(Vec4::ONE)
                    })
                    .collect();
                let normal = mesh.normals.get(normal_at).copied().filter(|n| n.is_finite() && n.x.abs() < NO_NORMAL);
                normal_at += 1;
                corners.push(Corner { v, uv, color, normal: normal.unwrap_or(Vec3::ZERO) });
            }
        }
        if !ok || corners.len() < 3 {
            broken += 1;
            continue;
        }
        let material = face_material(f);
        if face.loops.len() == 1 {
            push_corners(&mut poly, corners.iter(), material, has_normals);
            continue;
        }
        // A face with holes: triangles around them.
        let outline: Vec<Vec3> = corners[..loop_starts.get(1).copied().unwrap_or(corners.len())]
            .iter()
            .map(|c| mesh.positions[c.v as usize])
            .collect();
        let holes: Vec<Vec<Vec3>> = (1..loop_starts.len())
            .map(|k| {
                let end = loop_starts.get(k + 1).copied().unwrap_or(corners.len());
                corners[loop_starts[k]..end].iter().map(|c| mesh.positions[c.v as usize]).collect()
            })
            .collect();
        let hole_refs: Vec<&[Vec3]> = holes.iter().map(|h| &h[..]).collect();
        let tris = triangulate_with_holes(&outline, &hole_refs);
        if tris.is_empty() {
            broken += 1;
            continue;
        }
        for t in tris {
            push_corners(&mut poly, t.iter().map(|&k| &corners[k]), material, has_normals);
        }
    }
    let current_uv = current_uv
        .and_then(|name| poly.uvs.iter().position(|l| l.name == name))
        .or_else(|| (!poly.uvs.is_empty()).then_some(0));
    Converted { poly, current_uv, broken }
}

struct Corner {
    v: u32,
    uv: Vec<Vec2>,
    color: Vec<Vec4>,
    normal: Vec3,
}

fn push_corners<'a>(poly: &mut PolyMesh, corners: impl Iterator<Item = &'a Corner>, material: u32, normals: bool) {
    let start = poly.corner_verts.len();
    for c in corners {
        poly.corner_verts.push(c.v);
        for (l, &uv) in poly.uvs.iter_mut().zip(&c.uv) {
            l.values.push(uv);
        }
        for (l, &col) in poly.colors.iter_mut().zip(&c.color) {
            l.values.push(col);
        }
        if normals && let Some(n) = &mut poly.custom_normals {
            n.push(c.normal);
        }
    }
    if poly.corner_verts.len() > start {
        poly.face_starts.push(poly.corner_verts.len() as u32);
        poly.face_material.push(material);
        poly.face_groups.push(0);
    }
}

//endregion
