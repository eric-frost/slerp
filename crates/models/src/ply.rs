//! PLY meshes and point clouds, ASCII and binary of either byte order.
//!
//! PLY describes its own layout in a header: elements (vertices, faces,
//! edges, anything else) with typed properties.  The loader reads vertex
//! positions, normals, texture coordinates and colours under their common
//! names, faces of any size (triangulated), per-corner texture coordinates
//! as MeshLab writes them, edges and triangle strips.  A file with vertices
//! and nothing to connect them is a point cloud.  PLY has no units or up
//! axis, and scanners write it Y-up, so coordinates stay as stored.
//!
//! PLY files holding Gaussian splats are left to the splat loader.

use crate::common::{FastMap, Reader, TextureSet, check_triangles, find, parse_f32, srgb_to_linear, tokens, trim};
use glam::{Vec2, Vec3, Vec4};
use slerp_scene::{Info, Limits, LoadError, Material, Mesh, Node, Primitive, Scene, TexRef, Topology};
use std::path::Path;

/// True when a PLY header describes Gaussian splats rather than a mesh.
pub fn is_splat_header(head: &[u8]) -> bool {
    let header = &head[..find(head, b"end_header").unwrap_or(head.len())];
    let has = |name: &[u8]| {
        header.split(|&b| b == b'\n').any(|line| {
            let mut t = tokens(line);
            t.next() == Some(b"property") && t.last() == Some(name)
        })
    };
    has(b"f_dc_0") || (has(b"opacity") && has(b"scale_0") && has(b"rot_0"))
}

/*┌─────────────────╮
| Header          |
└─────────────────╯*/
//region Header

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Encoding {
    Ascii,
    Little,
    Big,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Ty {
    I8,
    U8,
    I16,
    U16,
    I32,
    U32,
    F32,
    F64,
}

impl Ty {
    fn parse(s: &[u8]) -> Option<Ty> {
        Some(match s {
            b"char" | b"int8" => Ty::I8,
            b"uchar" | b"uint8" => Ty::U8,
            b"short" | b"int16" => Ty::I16,
            b"ushort" | b"uint16" => Ty::U16,
            b"int" | b"int32" => Ty::I32,
            b"uint" | b"uint32" => Ty::U32,
            b"float" | b"float32" => Ty::F32,
            b"double" | b"float64" => Ty::F64,
            _ => return None,
        })
    }

    fn size(self) -> usize {
        match self {
            Ty::I8 | Ty::U8 => 1,
            Ty::I16 | Ty::U16 => 2,
            Ty::I32 | Ty::U32 | Ty::F32 => 4,
            Ty::F64 => 8,
        }
    }

    /// The divisor that maps an integer colour channel to 0..1.
    fn color_scale(self) -> f32 {
        match self {
            Ty::U8 | Ty::I8 => 255.0,
            Ty::U16 | Ty::I16 => 65535.0,
            Ty::U32 | Ty::I32 => u32::MAX as f32,
            Ty::F32 | Ty::F64 => 1.0,
        }
    }
}

#[derive(Clone, Debug)]
struct Property {
    name: String,
    ty: Ty,
    /// The type of the item count, for list properties.
    list: Option<Ty>,
}

#[derive(Clone, Debug)]
struct Element {
    name: String,
    count: usize,
    props: Vec<Property>,
}

struct Header {
    encoding: Encoding,
    elements: Vec<Element>,
    comments: Vec<String>,
    /// Offset of the first data byte.
    data: usize,
}

fn parse_header(bytes: &[u8]) -> Result<Header, LoadError> {
    let end = find(&bytes[..bytes.len().min(1 << 20)], b"end_header")
        .ok_or_else(|| LoadError::invalid("PLY header has no end_header"))?;
    // Data starts after the end_header line's newline (LF or CRLF).
    let mut data = end + b"end_header".len();
    while data < bytes.len() && bytes[data] != b'\n' {
        data += 1;
    }
    data = (data + 1).min(bytes.len());
    let mut header = Header { encoding: Encoding::Ascii, elements: Vec::new(), comments: Vec::new(), data };
    let mut saw_format = false;
    for line in bytes[..end].split(|&b| b == b'\n').skip(1) {
        let mut t = tokens(line);
        match t.next() {
            Some(b"format") => {
                header.encoding = match t.next() {
                    Some(b"ascii") => Encoding::Ascii,
                    Some(b"binary_little_endian") => Encoding::Little,
                    Some(b"binary_big_endian") => Encoding::Big,
                    _ => return Err(LoadError::invalid("unknown PLY format")),
                };
                saw_format = true;
            }
            Some(b"comment") | Some(b"obj_info") => {
                let text = trim(line);
                let rest = text.splitn(2, |b| b.is_ascii_whitespace()).nth(1).unwrap_or_default();
                header.comments.push(String::from_utf8_lossy(trim(rest)).into_owned());
            }
            Some(b"element") => {
                let name = String::from_utf8_lossy(t.next().unwrap_or_default()).into_owned();
                let count = t
                    .next()
                    .and_then(crate::common::parse_i64)
                    .filter(|&c| c >= 0)
                    .ok_or_else(|| LoadError::invalid("bad PLY element count"))?;
                header.elements.push(Element {
                    name,
                    count: usize::try_from(count).unwrap_or(usize::MAX),
                    props: Vec::new(),
                });
            }
            Some(b"property") => {
                let Some(el) = header.elements.last_mut() else { continue };
                let first = t.next().unwrap_or_default();
                let prop = if first == b"list" {
                    let count_ty = t.next().and_then(Ty::parse);
                    let item_ty = t.next().and_then(Ty::parse);
                    let name = t.next().unwrap_or_default();
                    match (count_ty, item_ty) {
                        (Some(c), Some(i)) => {
                            Property { name: String::from_utf8_lossy(name).into_owned(), ty: i, list: Some(c) }
                        }
                        _ => return Err(LoadError::invalid("bad PLY list property")),
                    }
                } else {
                    let ty = Ty::parse(first).ok_or_else(|| LoadError::invalid("unknown PLY property type"))?;
                    let name = t.next().unwrap_or_default();
                    Property { name: String::from_utf8_lossy(name).into_owned(), ty, list: None }
                };
                el.props.push(prop);
            }
            _ => {}
        }
    }
    if !saw_format {
        return Err(LoadError::invalid("PLY header has no format line"));
    }
    Ok(header)
}

//endregion

/*┌─────────────────╮
| Values          |
└─────────────────╯*/
//region Values

/// Reads property values in file order, from ASCII tokens or binary.
struct Values<'a> {
    encoding: Encoding,
    reader: Reader<'a>,
}

impl Values<'_> {
    fn scalar(&mut self, ty: Ty) -> Option<f64> {
        if self.encoding == Encoding::Ascii {
            return self.ascii();
        }
        let r = &mut self.reader;
        Some(match ty {
            Ty::I8 => r.u8()? as i8 as f64,
            Ty::U8 => r.u8()? as f64,
            Ty::I16 => r.u16()? as i16 as f64,
            Ty::U16 => r.u16()? as f64,
            Ty::I32 => r.i32()? as f64,
            Ty::U32 => r.u32()? as f64,
            Ty::F32 => r.f32()? as f64,
            Ty::F64 => r.f64()?,
        })
    }

    fn ascii(&mut self) -> Option<f64> {
        let r = &mut self.reader;
        while r.pos < r.data.len() && r.data[r.pos].is_ascii_whitespace() {
            r.pos += 1;
        }
        let start = r.pos;
        while r.pos < r.data.len() && !r.data[r.pos].is_ascii_whitespace() {
            r.pos += 1;
        }
        if start == r.pos {
            return None;
        }
        parse_f32(&r.data[start..r.pos]).map(|v| v as f64)
    }

    /// The smallest number of bytes one value can take, to bound counts the
    /// header claims by the data actually present.
    fn min_size(&self, ty: Ty) -> usize {
        if self.encoding == Encoding::Ascii { 2 } else { ty.size() }
    }

    fn remaining(&self) -> usize {
        self.reader.remaining()
    }
}

//endregion

/*┌─────────────────╮
| Loading         |
└─────────────────╯*/
//region Loading

/// What the vertex element's properties mean, by index into its list.
#[derive(Default)]
struct VertexLayout {
    position: [Option<usize>; 3],
    normal: [Option<usize>; 3],
    uv: [Option<usize>; 2],
    color: [Option<usize>; 4],
    color_scale: f32,
}

impl VertexLayout {
    fn new(el: &Element) -> Self {
        let find = |names: &[&str]| el.props.iter().position(|p| p.list.is_none() && names.contains(&p.name.as_str()));
        let mut l = VertexLayout {
            position: [find(&["x"]), find(&["y"]), find(&["z"])],
            normal: [find(&["nx", "normal_x"]), find(&["ny", "normal_y"]), find(&["nz", "normal_z"])],
            uv: [find(&["s", "u", "texture_u", "texture_s", "tx"]), find(&["t", "v", "texture_v", "texture_t", "ty"])],
            color: [
                find(&["red", "r", "diffuse_red"]),
                find(&["green", "g", "diffuse_green"]),
                find(&["blue", "b", "diffuse_blue"]),
                find(&["alpha", "a", "diffuse_alpha"]),
            ],
            color_scale: 1.0,
        };
        if let Some(i) = l.color[0] {
            l.color_scale = el.props[i].ty.color_scale();
        }
        l
    }
}

pub fn load(path: &Path, bytes: &[u8], limits: &Limits) -> Result<Scene, LoadError> {
    if is_splat_header(&bytes[..bytes.len().min(64 << 10)]) {
        return Err(LoadError::NotMine);
    }
    let header = parse_header(bytes)?;
    let mut scene = Scene::default();
    scene.info.format = format!(
        "PLY ({})",
        match header.encoding {
            Encoding::Ascii => "ASCII",
            Encoding::Little => "binary little-endian",
            Encoding::Big => "binary big-endian",
        }
    );
    let mut values = Values {
        encoding: header.encoding,
        reader: Reader { data: bytes, pos: header.data, big_endian: header.encoding == Encoding::Big },
    };

    // 1. Faces in the header bound the triangle count from below, so huge
    //    meshes are refused before anything is read.
    let face_count: u64 =
        header.elements.iter().filter(|e| e.name == "face" || e.name == "tristrips").map(|e| e.count as u64).sum();
    check_triangles(face_count, limits)?;

    // 2. Read every element in order, keeping vertices, faces and edges.
    let mut prim = Primitive::default();
    let mut has_normals = false;
    let mut polygons: Vec<u32> = Vec::new();
    let mut polygon_sizes: Vec<u32> = Vec::new();
    let mut corner_uvs: Vec<Vec2> = Vec::new();
    let mut edges: Vec<u32> = Vec::new();
    let mut triangles: u64 = 0;
    let mut truncated = false;
    'elements: for el in &header.elements {
        // A row takes at least this many bytes, which caps believable counts.
        let row_min: usize = el.props.iter().map(|p| values.min_size(p.list.unwrap_or(p.ty))).sum::<usize>().max(1);
        let believable = (values.remaining() / row_min).min(el.count);
        match el.name.as_str() {
            "vertex" => {
                let layout = VertexLayout::new(el);
                if layout.position.iter().any(Option::is_none) {
                    return Err(LoadError::invalid("PLY vertices have no x, y, z"));
                }
                has_normals = layout.normal.iter().all(Option::is_some);
                let has_uv = layout.uv.iter().all(Option::is_some);
                let has_color = layout.color[..3].iter().all(Option::is_some);
                prim.positions.reserve_exact(believable);
                let mut row = vec![0f64; el.props.len()];
                for _ in 0..el.count {
                    if !read_row(&mut values, el, &mut row) {
                        truncated = true;
                        break 'elements;
                    }
                    let get = |i: Option<usize>| i.map_or(0.0, |i| row[i] as f32);
                    prim.positions.push(Vec3::new(
                        get(layout.position[0]),
                        get(layout.position[1]),
                        get(layout.position[2]),
                    ));
                    if has_normals {
                        prim.normals.push(Vec3::new(
                            get(layout.normal[0]),
                            get(layout.normal[1]),
                            get(layout.normal[2]),
                        ));
                    }
                    if has_uv {
                        prim.uv0.push(Vec2::new(get(layout.uv[0]), 1.0 - get(layout.uv[1])));
                    }
                    if has_color {
                        let s = layout.color_scale;
                        let c = |i: Option<usize>| srgb_to_linear(get(i) / s);
                        let a = layout.color[3].map_or(1.0, |i| (row[i] as f32 / s).clamp(0.0, 1.0));
                        prim.colors.push(Vec4::new(c(layout.color[0]), c(layout.color[1]), c(layout.color[2]), a));
                    }
                }
            }
            "face" | "tristrips" => {
                let strips = el.name == "tristrips";
                let Some(index_prop) = el
                    .props
                    .iter()
                    .position(|p| p.list.is_some() && matches!(p.name.as_str(), "vertex_indices" | "vertex_index"))
                else {
                    skip_element(&mut values, el, &mut truncated);
                    continue;
                };
                let uv_prop = el.props.iter().position(|p| p.list.is_some() && p.name == "texcoord");
                let mut list = Vec::new();
                for _ in 0..el.count {
                    for (pi, p) in el.props.iter().enumerate() {
                        let ok = match p.list {
                            Some(count_ty) => {
                                list.clear();
                                read_list(&mut values, count_ty, p.ty, &mut list)
                            }
                            None => values.scalar(p.ty).is_some(),
                        };
                        if !ok {
                            truncated = true;
                            break 'elements;
                        }
                        if pi == index_prop {
                            if strips {
                                triangles += push_strip(&list, &mut polygons, &mut polygon_sizes);
                            } else if list.len() >= 3 {
                                polygon_sizes.push(list.len() as u32);
                                polygons.extend(list.iter().map(|&v| if v >= 0.0 { v as u32 } else { u32::MAX }));
                                triangles += list.len() as u64 - 2;
                            }
                        } else if Some(pi) == uv_prop && !strips {
                            corner_uvs.extend(list.chunks_exact(2).map(|c| Vec2::new(c[0] as f32, 1.0 - c[1] as f32)));
                        }
                    }
                    check_triangles(triangles, limits)?;
                }
            }
            "edge" => {
                let a = el.props.iter().position(|p| p.name == "vertex1");
                let b = el.props.iter().position(|p| p.name == "vertex2");
                let mut row = vec![0f64; el.props.len()];
                for _ in 0..el.count {
                    if !read_row(&mut values, el, &mut row) {
                        truncated = true;
                        break 'elements;
                    }
                    if let (Some(a), Some(b)) = (a, b) {
                        edges.extend([row[a].max(0.0) as u32, row[b].max(0.0) as u32]);
                    }
                }
            }
            _ => skip_element(&mut values, el, &mut truncated),
        }
        if truncated {
            break;
        }
    }
    if truncated {
        scene.info.warnings.push("file ends early; loaded what was there".into());
    }
    let vertex_count = prim.positions.len();
    if vertex_count == 0 {
        return Err(LoadError::invalid("PLY has no vertices"));
    }

    // 3. Comments: MeshLab names the texture as "TextureFile <name>".
    let mut textures = TextureSet::new(path, limits);
    let mut material = Material { name: "ply".into(), ..Material::default() };
    for c in &header.comments {
        if let Some(name) = c.strip_prefix("TextureFile ").or_else(|| c.strip_prefix("texturefile "))
            && let Some(t) = textures.file(name.trim())
        {
            material.base_color = Vec4::ONE;
            material.base_color_texture = Some(TexRef::new(t));
        }
    }
    let generator = header.comments.iter().find(|c| {
        let l = c.to_ascii_lowercase();
        l.contains("generated by") || l.contains("created by") || l.contains("exported") || l.starts_with("vcglib")
    });
    scene.info.generator = generator.cloned();

    // 4. Assemble: triangles, lines, or a point cloud.
    let mut primitives = Vec::new();
    if !edges.is_empty() {
        let mut lines = Primitive { topology: Topology::Lines, normals: Vec::new(), ..prim.clone() };
        lines.indices = edges
            .chunks_exact(2)
            .filter(|e| e.iter().all(|&v| (v as usize) < vertex_count))
            .flatten()
            .copied()
            .collect();
        primitives.push(lines);
    }
    if !polygon_sizes.is_empty() {
        let mut tri = triangulate_faces(prim, &polygons, &polygon_sizes, &corner_uvs, &mut scene.info);
        if !has_normals {
            tri.ensure_normals();
        }
        primitives.insert(0, tri);
    } else if primitives.is_empty() {
        let mut points = prim;
        points.topology = Topology::Points;
        points.indices = (0..vertex_count as u32).collect();
        primitives.push(points);
        scene.info.details.push(("Points".into(), vertex_count.to_string()));
    }
    let any_textured = material.base_color_texture.is_some();
    for p in &mut primitives {
        p.material = Some(0);
        if any_textured && p.uv0.is_empty() {
            scene.info.warnings.push("texture named but the mesh has no texture coordinates".into());
        }
    }
    scene.materials.push(material);
    let (tex, warnings) = (textures.textures, textures.warnings);
    scene.textures = tex;
    scene.info.warnings.extend(warnings);
    let name = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let cloud = primitives.len() == 1 && primitives[0].topology == Topology::Points;
    scene.meshes.push(Mesh { name: name.clone(), primitives, target_names: Vec::new() });
    let mut node = Node::new(name);
    node.mesh = Some(0);
    scene.nodes.push(node);
    scene.roots.push(0);
    // Point clouds are thinned and recentred like the other point formats,
    // and drawn unlit, since points have no surface to light.
    if cloud {
        crate::points::tidy_primitive(&mut scene, 0, limits);
        scene.materials[0].unlit = true;
    }
    Ok(scene)
}

/// Reads one row of scalar properties; list properties are read and dropped.
fn read_row(values: &mut Values, el: &Element, row: &mut [f64]) -> bool {
    let mut scratch = Vec::new();
    for (i, p) in el.props.iter().enumerate() {
        match p.list {
            Some(count_ty) => {
                scratch.clear();
                if !read_list(values, count_ty, p.ty, &mut scratch) {
                    return false;
                }
            }
            None => match values.scalar(p.ty) {
                Some(v) => row[i] = v,
                None => return false,
            },
        }
    }
    true
}

fn read_list(values: &mut Values, count_ty: Ty, item_ty: Ty, out: &mut Vec<f64>) -> bool {
    let Some(n) = values.scalar(count_ty) else { return false };
    if !(0.0..=1e9).contains(&n) {
        return false;
    }
    let n = n as usize;
    if n.saturating_mul(values.min_size(item_ty)) > values.remaining() {
        return false;
    }
    for _ in 0..n {
        match values.scalar(item_ty) {
            Some(v) => out.push(v),
            None => return false,
        }
    }
    true
}

fn skip_element(values: &mut Values, el: &Element, truncated: &mut bool) {
    let fixed = el.props.iter().all(|p| p.list.is_none());
    if fixed && values.encoding != Encoding::Ascii {
        let row: usize = el.props.iter().map(|p| p.ty.size()).sum();
        let skip = row.saturating_mul(el.count);
        if skip > values.remaining() {
            *truncated = true;
        }
        values.reader.pos = values.reader.pos.saturating_add(skip).min(values.reader.data.len());
        return;
    }
    let mut row = vec![0f64; el.props.len()];
    for _ in 0..el.count {
        if !read_row(values, el, &mut row) {
            *truncated = true;
            return;
        }
    }
}

/// Turns a triangle strip with -1 restarts into separate triangles.
fn push_strip(list: &[f64], polygons: &mut Vec<u32>, sizes: &mut Vec<u32>) -> u64 {
    let mut count = 0;
    for strip in list.split(|&v| v < 0.0) {
        for i in 2..strip.len() {
            let (a, b, c) = (strip[i - 2] as u32, strip[i - 1] as u32, strip[i] as u32);
            let tri = if i % 2 == 0 { [a, b, c] } else { [b, a, c] };
            if a != b && b != c && a != c {
                polygons.extend(tri);
                sizes.push(3);
                count += 1;
            }
        }
    }
    count
}

/// Triangulates the faces.  Per-corner texture coordinates split vertices
/// that need different ones on different faces.
fn triangulate_faces(
    src: Primitive,
    polygons: &[u32],
    sizes: &[u32],
    corner_uvs: &[Vec2],
    info: &mut Info,
) -> Primitive {
    let n = src.positions.len();
    let use_corner_uvs = corner_uvs.len() == polygons.len();
    let mut indices = Vec::with_capacity(polygons.len());
    let mut split = Primitive::default();
    let mut remap: FastMap<(u32, u32, u32), u32> = FastMap::default();
    let mut tris = Vec::new();
    let mut points = Vec::new();
    let mut skipped = 0usize;
    let mut start = 0usize;
    for &size in sizes {
        let face = &polygons[start..start + size as usize];
        let corner0 = start;
        start += size as usize;
        if face.iter().any(|&v| v as usize >= n) {
            skipped += 1;
            continue;
        }
        points.clear();
        points.extend(face.iter().map(|&v| src.positions[v as usize]));
        tris.clear();
        crate::common::triangulate(&points, &mut tris);
        for tri in &tris {
            for &k in tri {
                let v = face[k as usize];
                if !use_corner_uvs {
                    indices.push(v);
                    continue;
                }
                let uv = corner_uvs[corner0 + k as usize];
                let key = (v, uv.x.to_bits(), uv.y.to_bits());
                let index = *remap.entry(key).or_insert_with(|| {
                    let i = split.positions.len() as u32;
                    split.positions.push(src.positions[v as usize]);
                    if !src.normals.is_empty() {
                        split.normals.push(src.normals[v as usize]);
                    }
                    if !src.colors.is_empty() {
                        split.colors.push(src.colors[v as usize]);
                    }
                    split.uv0.push(uv);
                    i
                });
                indices.push(index);
            }
        }
    }
    if skipped > 0 {
        info.warnings.push(format!("{skipped} faces refer to missing vertices"));
    }
    let mut out = if use_corner_uvs { split } else { src };
    out.indices = indices;
    out.topology = Topology::Triangles;
    out
}

//endregion

#[cfg(test)]
mod tests {
    use super::*;

    const CUBE_ASCII: &[u8] = b"ply\nformat ascii 1.0\ncomment made by hand\nelement vertex 8\nproperty float x\nproperty float y\nproperty float z\n\
property uchar red\nproperty uchar green\nproperty uchar blue\nelement face 6\nproperty list uchar int vertex_indices\nend_header\n\
0 0 0 255 0 0\n0 0 1 255 0 0\n0 1 1 255 0 0\n0 1 0 255 0 0\n1 0 0 0 0 255\n1 0 1 0 0 255\n1 1 1 0 0 255\n1 1 0 0 0 255\n\
4 0 1 2 3\n4 7 6 5 4\n4 0 4 5 1\n4 1 5 6 2\n4 2 6 7 3\n4 3 7 4 0\n";

    fn binary(big: bool) -> Vec<u8> {
        let mut out = format!(
            "ply\nformat binary_{}_endian 1.0\nelement vertex 3\nproperty float x\nproperty float y\nproperty float z\nproperty float nx\nproperty float ny\nproperty float nz\nelement face 1\nproperty list uchar uint vertex_indices\nproperty list uchar float texcoord\nend_header\r\n",
            if big { "big" } else { "little" }
        )
        .into_bytes();
        let f = |v: f32| if big { v.to_be_bytes() } else { v.to_le_bytes() };
        let u = |v: u32| if big { v.to_be_bytes() } else { v.to_le_bytes() };
        for p in [[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]] {
            for c in p {
                out.extend(f(c));
            }
            for c in [0.0, 0.0, 1.0] {
                out.extend(f(c));
            }
        }
        out.push(3);
        for i in 0..3 {
            out.extend(u(i));
        }
        out.push(6);
        for c in [0.0, 0.0, 1.0, 0.0, 0.0, 1.0] {
            out.extend(f(c));
        }
        out
    }

    #[test]
    fn reads_ascii_cube_with_colours() {
        let scene = load(Path::new("cube.ply"), CUBE_ASCII, &Limits::viewer()).unwrap();
        assert_eq!(scene.triangle_count(), 12);
        let p = &scene.meshes[0].primitives[0];
        assert_eq!(p.colors[0], Vec4::new(1.0, 0.0, 0.0, 1.0));
        assert_eq!(p.normals.len(), 8);
        assert!(scene.info.warnings.is_empty(), "{:?}", scene.info.warnings);
    }

    #[test]
    fn reads_binary_both_endians_with_corner_uvs() {
        for big in [false, true] {
            let scene = load(Path::new("t.ply"), &binary(big), &Limits::viewer()).unwrap();
            let p = &scene.meshes[0].primitives[0];
            assert_eq!(scene.triangle_count(), 1);
            assert_eq!(p.positions[1], Vec3::X);
            assert_eq!(p.normals[0], Vec3::Z);
            assert_eq!(p.uv0[1], Vec2::new(1.0, 1.0));
            assert_eq!(p.uv0[2], Vec2::new(0.0, 0.0));
        }
    }

    #[test]
    fn point_clouds_and_splats() {
        let cloud = b"ply\nformat ascii 1.0\nelement vertex 2\nproperty double x\nproperty double y\nproperty double z\nend_header\n1 2 3\n4 5 6\n";
        let scene = load(Path::new("c.ply"), cloud, &Limits::viewer()).unwrap();
        assert_eq!(scene.meshes[0].primitives[0].topology, Topology::Points);
        assert_eq!(scene.meshes[0].primitives[0].indices, vec![0, 1]);
        let splat = b"ply\nformat binary_little_endian 1.0\nelement vertex 1\nproperty float x\nproperty float f_dc_0\nend_header\n";
        assert!(matches!(load(Path::new("s.ply"), splat, &Limits::viewer()), Err(LoadError::NotMine)));
    }

    #[test]
    fn survives_truncation_and_lies() {
        let data = binary(false);
        for cut in 0..data.len() {
            let _ = load(Path::new("t.ply"), &data[..cut], &Limits::viewer());
        }
        for cut in 0..CUBE_ASCII.len() {
            let _ = load(Path::new("t.ply"), &CUBE_ASCII[..cut], &Limits::viewer());
        }
        let lie = b"ply\nformat binary_little_endian 1.0\nelement vertex 4000000000\nproperty float x\nproperty float y\nproperty float z\nelement face 1\nproperty list uint uint vertex_indices\nend_header\n\0\0\0\0\0\0\0\0\0\0\0\0\xff\xff\xff\xff";
        let scene = load(Path::new("t.ply"), lie, &Limits::viewer()).unwrap();
        assert_eq!(scene.vertex_count(), 1);
        let mut corrupt = CUBE_ASCII.to_vec();
        corrupt[200] = b'9';
        corrupt[230] = b'-';
        let _ = load(Path::new("t.ply"), &corrupt, &Limits::viewer());
        let limits = Limits { max_triangles: 5, ..Limits::viewer() };
        assert!(matches!(load(Path::new("t.ply"), CUBE_ASCII, &limits), Err(LoadError::TooLarge(_))));
    }
}
