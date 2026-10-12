//! MagicaVoxel `.vox`: every model placed by the file's scene graph, greedy
//! meshed into as few quads as the colours allow, with faces between solid
//! voxels culled.
//!
//! Colours come from the file's palette as vertex colours.  `MATL` chunks
//! turn palette entries into metal, glass or emissive materials, each
//! approximated with the PBR model.  MagicaVoxel is Z-up with no units; the
//! loader turns it Y-up at 1 voxel = 0.1 m, which makes typical characters
//! (20-40 voxels tall) human-sized.
//!
//! A model's pivot is its centre rounded down, `size / 2` in integers, and
//! instance transforms rotate and move the model's voxel cubes about it, as
//! ogt_vox does.

use crate::common::{FastMap, Reader, check_triangles, srgb8_to_linear};
use glam::{Mat3, Quat, Vec3, Vec4};
use slerp_scene::{AlphaMode, Limits, LoadError, Material, Mesh, Node, Primitive, Scene, Topology, Transform};
use std::collections::HashMap;
use std::path::Path;

/// Metres per voxel.
const VOXEL_SIZE: f32 = 0.1;
/// Models larger than this along any axis are clamped; voxel coordinates
/// are bytes, so nothing past 256 can hold voxels anyway.
const MAX_SIDE: u32 = 256;
/// Scene graphs deeper than this are malformed or malicious.
const MAX_DEPTH: usize = 64;

/*┌─────────────────╮
| Chunks          |
└─────────────────╯*/
//region Chunks

struct Model<'a> {
    size: [u32; 3],
    /// (x, y, z, colour index) per voxel.
    voxels: &'a [u8],
}

enum GraphNode {
    Transform { name: String, hidden: bool, child: i32, layer: i32, rotation: Mat3, translation: Vec3 },
    Group { children: Vec<i32> },
    Shape { model: i32 },
}

/// A palette entry's material, from `MATL`.
#[derive(Clone, Copy, Debug, PartialEq)]
enum VoxMaterial {
    Diffuse,
    Metal { metal: f32, rough: f32 },
    Glass { alpha: f32, rough: f32 },
    Emit { strength: f32 },
}

struct VoxFile<'a> {
    version: i32,
    models: Vec<Model<'a>>,
    palette: [[u8; 4]; 256],
    nodes: HashMap<i32, GraphNode>,
    hidden_layers: Vec<i32>,
    materials: HashMap<u8, VoxMaterial>,
    pack: bool,
}

fn read_string(r: &mut Reader) -> Option<String> {
    let n = r.u32()? as usize;
    let b = r.bytes(n)?;
    Some(String::from_utf8_lossy(b).into_owned())
}

fn read_dict(r: &mut Reader) -> Option<HashMap<String, String>> {
    let n = r.u32()? as usize;
    // Each pair takes at least 8 bytes.
    if n > r.remaining() / 8 {
        return None;
    }
    let mut out = HashMap::with_capacity(n);
    for _ in 0..n {
        let k = read_string(r)?;
        let v = read_string(r)?;
        out.insert(k, v);
    }
    Some(out)
}

/// Decodes the packed rotation byte: the column of each row's non-zero
/// entry and its sign.
fn rotation(byte: u8) -> Mat3 {
    let i0 = (byte & 3) as usize;
    let i1 = ((byte >> 2) & 3) as usize;
    if i0 > 2 || i1 > 2 || i0 == i1 {
        return Mat3::IDENTITY;
    }
    let i2 = 3 - i0 - i1;
    let sign = |bit: u8| if byte >> bit & 1 == 1 { -1.0 } else { 1.0 };
    let mut rows = [[0f32; 3]; 3];
    rows[0][i0] = sign(4);
    rows[1][i1] = sign(5);
    rows[2][i2] = sign(6);
    Mat3::from_cols_array_2d(&rows).transpose()
}

fn parse(bytes: &[u8]) -> Result<VoxFile<'_>, LoadError> {
    let mut r = Reader::new(bytes);
    if r.bytes(4) != Some(b"VOX ") {
        return Err(LoadError::NotMine);
    }
    let version = r.i32().ok_or_else(|| LoadError::invalid("VOX header cut short"))?;
    let mut file = VoxFile {
        version,
        models: Vec::new(),
        palette: default_palette(),
        nodes: HashMap::new(),
        hidden_layers: Vec::new(),
        materials: HashMap::new(),
        pack: false,
    };
    // MAIN's content is empty; every other chunk is its child.  Children
    // are read flat to the end of the file, ignoring sizes that lie.
    if r.bytes(4) != Some(b"MAIN") {
        return Err(LoadError::invalid("VOX has no MAIN chunk"));
    }
    let main_content = r.u32().unwrap_or(0) as usize;
    let _children = r.u32();
    r.bytes(main_content.min(r.remaining()));
    let mut pending_size: Option<[u32; 3]> = None;
    while r.remaining() >= 12 {
        let id: [u8; 4] = r.bytes(4).and_then(|b| b.try_into().ok()).unwrap_or([0; 4]);
        let content = r.u32().unwrap_or(0) as usize;
        let children = r.u32().unwrap_or(0) as usize;
        let Some(body) = r.bytes(content) else { break };
        // Nested children only occur under MAIN; skip any others.
        r.bytes(children.min(r.remaining()));
        let mut c = Reader::new(body);
        match &id {
            b"PACK" => file.pack = true,
            b"SIZE" => {
                let s = [c.u32().unwrap_or(0), c.u32().unwrap_or(0), c.u32().unwrap_or(0)];
                pending_size = Some(s.map(|v| v.min(MAX_SIDE)));
            }
            b"XYZI" => {
                let n = c.u32().unwrap_or(0) as usize;
                let voxels = c.bytes(n.min(c.remaining() / 4) * 4).unwrap_or_default();
                if let Some(size) = pending_size.take() {
                    file.models.push(Model { size, voxels });
                }
            }
            b"RGBA" => {
                for i in 0..255 {
                    match c.bytes(4) {
                        Some(p) => file.palette[i + 1] = [p[0], p[1], p[2], p[3]],
                        None => break,
                    }
                }
            }
            b"nTRN" => {
                let (Some(id), Some(attrs)) = (c.i32(), read_dict(&mut c)) else { continue };
                let child = c.i32().unwrap_or(-1);
                let _reserved = c.i32();
                let layer = c.i32().unwrap_or(-1);
                let frames = c.i32().unwrap_or(0);
                let frame = if frames > 0 { read_dict(&mut c).unwrap_or_default() } else { HashMap::new() };
                let rot = frame.get("_r").and_then(|s| s.trim().parse::<u8>().ok()).map_or(Mat3::IDENTITY, rotation);
                let t: Vec<f32> = frame
                    .get("_t")
                    .map(|s| s.split_whitespace().filter_map(|v| v.parse::<i32>().ok()).map(|v| v as f32).collect())
                    .unwrap_or_default();
                let translation = if t.len() == 3 { Vec3::new(t[0], t[1], t[2]) } else { Vec3::ZERO };
                file.nodes.insert(
                    id,
                    GraphNode::Transform {
                        name: attrs.get("_name").cloned().unwrap_or_default(),
                        hidden: attrs.get("_hidden").is_some_and(|v| v == "1"),
                        child,
                        layer,
                        rotation: rot,
                        translation,
                    },
                );
            }
            b"nGRP" => {
                let (Some(id), Some(_)) = (c.i32(), read_dict(&mut c)) else { continue };
                let n = c.u32().unwrap_or(0) as usize;
                let children = (0..n.min(c.remaining() / 4)).filter_map(|_| c.i32()).collect();
                file.nodes.insert(id, GraphNode::Group { children });
            }
            b"nSHP" => {
                let (Some(id), Some(_)) = (c.i32(), read_dict(&mut c)) else { continue };
                let n = c.u32().unwrap_or(0);
                if n > 0
                    && let Some(model) = c.i32()
                {
                    file.nodes.insert(id, GraphNode::Shape { model });
                }
            }
            b"LAYR" => {
                let (Some(id), Some(attrs)) = (c.i32(), read_dict(&mut c)) else { continue };
                if attrs.get("_hidden").is_some_and(|v| v == "1") {
                    file.hidden_layers.push(id);
                }
            }
            b"MATL" => {
                let (Some(id), Some(p)) = (c.i32(), read_dict(&mut c)) else { continue };
                let f = |k: &str| p.get(k).and_then(|v| v.trim().parse::<f32>().ok()).filter(|v| v.is_finite());
                let m = match p.get("_type").map(String::as_str) {
                    Some("_metal") => VoxMaterial::Metal {
                        metal: f("_metal").unwrap_or(1.0).clamp(0.0, 1.0),
                        rough: f("_rough").unwrap_or(0.2).clamp(0.0, 1.0),
                    },
                    Some("_glass") | Some("_blend") => VoxMaterial::Glass {
                        alpha: 1.0 - f("_trans").or(f("_alpha")).unwrap_or(0.5).clamp(0.0, 0.95),
                        rough: f("_rough").unwrap_or(0.1).clamp(0.0, 1.0),
                    },
                    Some("_emit") => VoxMaterial::Emit {
                        strength: f("_emit").unwrap_or(1.0).clamp(0.0, 1.0)
                            * (1.0 + f("_flux").unwrap_or(0.0).clamp(0.0, 4.0)),
                    },
                    _ => VoxMaterial::Diffuse,
                };
                if (1..=255).contains(&id) && m != VoxMaterial::Diffuse {
                    file.materials.insert(id as u8, m);
                }
            }
            _ => {}
        }
    }
    Ok(file)
}

//endregion

/*┌─────────────────╮
| Meshing         |
└─────────────────╯*/
//region Meshing

/// Greedy-meshes one model into primitives keyed by material: index 0
/// collects plain voxels, others hold one special palette entry each.
fn mesh_model(
    model: &Model,
    file: &VoxFile,
    triangles: &mut u64,
    limits: &Limits,
) -> Result<Vec<(u8, Primitive)>, LoadError> {
    let [sx, sy, sz] = model.size.map(|v| v as usize);
    if sx == 0 || sy == 0 || sz == 0 {
        return Ok(Vec::new());
    }
    let dims = [sx, sy, sz];
    let strides = [1, sx, sx * sy];
    let mut grid = vec![0u8; sx * sy * sz];
    for v in model.voxels.chunks_exact(4) {
        let (x, y, z) = (v[0] as usize, v[1] as usize, v[2] as usize);
        if x < sx && y < sy && z < sz {
            grid[x + sx * (y + sy * z)] = v[3];
        }
    }
    let is_glass: [bool; 256] =
        std::array::from_fn(|c| matches!(file.materials.get(&(c as u8)), Some(VoxMaterial::Glass { .. })));
    let glass = |c: u8| is_glass[c as usize];
    let keys: [u8; 256] = std::array::from_fn(|c| if file.materials.contains_key(&(c as u8)) { c as u8 } else { 0 });
    let key = |c: u8| keys[c as usize];
    let pivot = Vec3::new((sx / 2) as f32, (sy / 2) as f32, (sz / 2) as f32);
    let mut prims: FastMap<u8, Primitive> = FastMap::default();

    // Sweep each axis in both directions, slice by slice, building a mask
    // of exposed faces and merging equal neighbours into rectangles.
    for d in 0..3 {
        let (u, v) = ((d + 1) % 3, (d + 2) % 3);
        let mut mask = vec![0u8; dims[u] * dims[v]];
        for positive in [true, false] {
            for slice in 0..dims[d] {
                // The neighbour across the face, by index arithmetic: this
                // loop runs for every cell six times.
                let has_next = if positive { slice + 1 < dims[d] } else { slice > 0 };
                for j in 0..dims[v] {
                    for i in 0..dims[u] {
                        let index = slice * strides[d] + i * strides[u] + j * strides[v];
                        let c = grid[index];
                        let n = match (has_next, positive) {
                            (false, _) => 0,
                            (true, true) => grid[index + strides[d]],
                            (true, false) => grid[index - strides[d]],
                        };
                        // A face shows when the neighbour is empty, or is
                        // glass in front of something that isn't.
                        let shown = c != 0 && (n == 0 || (glass(n) && !glass(c)));
                        mask[i + j * dims[u]] = if shown { c } else { 0 };
                    }
                }
                let plane = slice as f32 + if positive { 1.0 } else { 0.0 };
                for j in 0..dims[v] {
                    let mut i = 0;
                    while i < dims[u] {
                        let c = mask[i + j * dims[u]];
                        if c == 0 {
                            i += 1;
                            continue;
                        }
                        let mut w = 1;
                        while i + w < dims[u] && mask[i + w + j * dims[u]] == c {
                            w += 1;
                        }
                        let mut h = 1;
                        'grow: while j + h < dims[v] {
                            for k in 0..w {
                                if mask[i + k + (j + h) * dims[u]] != c {
                                    break 'grow;
                                }
                            }
                            h += 1;
                        }
                        for jj in 0..h {
                            for k in 0..w {
                                mask[i + k + (j + jj) * dims[u]] = 0;
                            }
                        }
                        *triangles += 2;
                        if *triangles > limits.max_triangles {
                            check_triangles(*triangles, limits)?;
                        }
                        let prim = prims.entry(key(c)).or_default();
                        push_quad(prim, d, u, v, plane, [i, j], [w, h], positive, pivot, color(&file.palette, c));
                        i += w;
                    }
                }
            }
        }
    }
    let mut out: Vec<(u8, Primitive)> = prims.into_iter().collect();
    out.sort_by_key(|(k, _)| *k);
    Ok(out)
}

fn color(palette: &[[u8; 4]; 256], c: u8) -> Vec4 {
    let p = palette[c as usize];
    Vec4::new(srgb8_to_linear(p[0]), srgb8_to_linear(p[1]), srgb8_to_linear(p[2]), p[3] as f32 / 255.0)
}

#[allow(clippy::too_many_arguments)]
fn push_quad(
    prim: &mut Primitive,
    d: usize,
    u: usize,
    v: usize,
    plane: f32,
    at: [usize; 2],
    size: [usize; 2],
    positive: bool,
    pivot: Vec3,
    color: Vec4,
) {
    let corner = |a: usize, b: usize| {
        let mut p = Vec3::ZERO;
        p[d] = plane;
        p[u] = a as f32;
        p[v] = b as f32;
        p - pivot
    };
    let (i, j, w, h) = (at[0], at[1], size[0], size[1]);
    let quad = [corner(i, j), corner(i + w, j), corner(i + w, j + h), corner(i, j + h)];
    let mut normal = Vec3::ZERO;
    normal[d] = if positive { 1.0 } else { -1.0 };
    let base = prim.positions.len() as u32;
    prim.positions.extend(quad);
    prim.normals.extend([normal; 4]);
    prim.colors.extend([color; 4]);
    // The u-v plane's winding faces +d, so negative faces reverse it.
    if positive {
        prim.indices.extend([base, base + 1, base + 2, base, base + 2, base + 3]);
    } else {
        prim.indices.extend([base, base + 2, base + 1, base, base + 3, base + 2]);
    }
}

//endregion

/*┌─────────────────╮
| Scene           |
└─────────────────╯*/
//region Scene

struct Builder<'a, 'b> {
    file: &'b VoxFile<'a>,
    limits: &'b Limits,
    scene: Scene,
    /// Mesh index per model, built on first use.
    meshes: HashMap<i32, Option<usize>>,
    /// Material index per material key.
    materials: HashMap<u8, usize>,
    triangles: u64,
}

impl Builder<'_, '_> {
    fn mesh(&mut self, model: i32) -> Result<Option<usize>, LoadError> {
        if let Some(&m) = self.meshes.get(&model) {
            return Ok(m);
        }
        let Some(m) = usize::try_from(model).ok().and_then(|i| self.file.models.get(i)) else {
            self.meshes.insert(model, None);
            return Ok(None);
        };
        let prims = mesh_model(m, self.file, &mut self.triangles, self.limits)?;
        let mut mesh = Mesh { name: format!("model {model}"), ..Mesh::default() };
        for (key, mut prim) in prims {
            prim.topology = Topology::Triangles;
            prim.material = Some(self.material(key));
            mesh.primitives.push(prim);
        }
        let index = (!mesh.primitives.is_empty()).then(|| {
            self.scene.meshes.push(mesh);
            self.scene.meshes.len() - 1
        });
        self.meshes.insert(model, index);
        Ok(index)
    }

    fn material(&mut self, key: u8) -> usize {
        if let Some(&m) = self.materials.get(&key) {
            return m;
        }
        let mut m = Material { name: "voxel".into(), base_color: Vec4::ONE, roughness: 0.75, ..Material::default() };
        match self.file.materials.get(&key) {
            Some(&VoxMaterial::Metal { metal, rough }) => {
                m.name = format!("metal {key}");
                m.metallic = metal;
                m.roughness = rough;
            }
            Some(&VoxMaterial::Glass { alpha, rough }) => {
                m.name = format!("glass {key}");
                m.base_color.w = alpha;
                m.roughness = rough;
                m.alpha_mode = AlphaMode::Blend;
            }
            Some(&VoxMaterial::Emit { strength }) => {
                m.name = format!("emissive {key}");
                m.emissive = color(&self.file.palette, key).truncate() * strength;
            }
            _ => {}
        }
        self.scene.materials.push(m);
        let index = self.scene.materials.len() - 1;
        self.materials.insert(key, index);
        index
    }

    /// Adds the subtree under a transform node; returns its node index.
    fn visit(&mut self, id: i32, depth: usize, seen: &mut Vec<i32>) -> Result<Option<usize>, LoadError> {
        if depth > MAX_DEPTH || seen.contains(&id) {
            return Ok(None);
        }
        let Some(GraphNode::Transform { name, hidden, child, layer, rotation, translation }) = self.file.nodes.get(&id)
        else {
            return Ok(None);
        };
        if *hidden || self.file.hidden_layers.contains(layer) {
            return Ok(None);
        }
        seen.push(id);
        let mut node = Node::new(name.clone());
        // A rotation byte may hold a reflection, which a quaternion can't:
        // split it off as a negative scale.
        let (rot, scale) = if rotation.determinant() < 0.0 {
            (*rotation * Mat3::from_diagonal(Vec3::new(-1.0, 1.0, 1.0)), Vec3::new(-1.0, 1.0, 1.0))
        } else {
            (*rotation, Vec3::ONE)
        };
        node.transform = Transform { translation: *translation, rotation: Quat::from_mat3(&rot).normalize(), scale };
        let index = self.scene.nodes.len();
        self.scene.nodes.push(node);
        match self.file.nodes.get(child) {
            Some(GraphNode::Group { children }) if !seen.contains(child) => {
                seen.push(*child);
                for &c in children {
                    if let Some(ci) = self.visit(c, depth + 1, seen)? {
                        self.scene.nodes[index].children.push(ci);
                    }
                }
            }
            Some(GraphNode::Shape { model }) => {
                self.scene.nodes[index].mesh = self.mesh(*model)?;
            }
            _ => {}
        }
        Ok(Some(index))
    }
}

pub fn load(_path: &Path, bytes: &[u8], limits: &Limits) -> Result<Scene, LoadError> {
    let file = parse(bytes)?;
    if file.models.is_empty() {
        return Err(LoadError::invalid("VOX holds no models"));
    }
    let mut b = Builder {
        file: &file,
        limits,
        scene: Scene::default(),
        meshes: HashMap::new(),
        materials: HashMap::new(),
        triangles: 0,
    };
    // 1. Place models by the scene graph, rooted at transform node 0.  Old
    //    files have none; there, several models are animation frames, so
    //    only the first is shown.
    let root = if file.nodes.contains_key(&0) { b.visit(0, 0, &mut Vec::new())? } else { None };
    if root.is_none() {
        b.scene.nodes.clear();
        let mut node = Node::new("model 0");
        node.mesh = b.mesh(0)?;
        b.scene.nodes.push(node);
    }
    let mut scene = b.scene;
    scene.compute_roots();
    if scene.meshes.is_empty() {
        return Err(LoadError::invalid("VOX models are empty"));
    }
    // 2. Z-up voxels to Y-up metres.
    scene.wrap_roots(Transform {
        rotation: Quat::from_rotation_x(-std::f32::consts::FRAC_PI_2),
        scale: Vec3::splat(VOXEL_SIZE),
        ..Transform::IDENTITY
    });
    scene.info.format = format!("MagicaVoxel (version {})", file.version);
    let voxels: usize = file.models.iter().map(|m| m.voxels.len() / 4).sum();
    scene.info.details.push(("Models".into(), file.models.len().to_string()));
    scene.info.details.push(("Voxels".into(), voxels.to_string()));
    if file.pack && file.models.len() > 1 && file.nodes.is_empty() {
        scene.info.details.push(("Animation frames".into(), format!("{} (first shown)", file.models.len())));
    }
    Ok(scene)
}

//endregion

/// MagicaVoxel's palette for files without an `RGBA` chunk, as stored in
/// the format description (0xAABBGGRR).
fn default_palette() -> [[u8; 4]; 256] {
    const P: [u32; 256] = [
        0x00000000, 0xffffffff, 0xffccffff, 0xff99ffff, 0xff66ffff, 0xff33ffff, 0xff00ffff, 0xffffccff, 0xffccccff,
        0xff99ccff, 0xff66ccff, 0xff33ccff, 0xff00ccff, 0xffff99ff, 0xffcc99ff, 0xff9999ff, 0xff6699ff, 0xff3399ff,
        0xff0099ff, 0xffff66ff, 0xffcc66ff, 0xff9966ff, 0xff6666ff, 0xff3366ff, 0xff0066ff, 0xffff33ff, 0xffcc33ff,
        0xff9933ff, 0xff6633ff, 0xff3333ff, 0xff0033ff, 0xffff00ff, 0xffcc00ff, 0xff9900ff, 0xff6600ff, 0xff3300ff,
        0xff0000ff, 0xffffffcc, 0xffccffcc, 0xff99ffcc, 0xff66ffcc, 0xff33ffcc, 0xff00ffcc, 0xffffcccc, 0xffcccccc,
        0xff99cccc, 0xff66cccc, 0xff33cccc, 0xff00cccc, 0xffff99cc, 0xffcc99cc, 0xff9999cc, 0xff6699cc, 0xff3399cc,
        0xff0099cc, 0xffff66cc, 0xffcc66cc, 0xff9966cc, 0xff6666cc, 0xff3366cc, 0xff0066cc, 0xffff33cc, 0xffcc33cc,
        0xff9933cc, 0xff6633cc, 0xff3333cc, 0xff0033cc, 0xffff00cc, 0xffcc00cc, 0xff9900cc, 0xff6600cc, 0xff3300cc,
        0xff0000cc, 0xffffff99, 0xffccff99, 0xff99ff99, 0xff66ff99, 0xff33ff99, 0xff00ff99, 0xffffcc99, 0xffcccc99,
        0xff99cc99, 0xff66cc99, 0xff33cc99, 0xff00cc99, 0xffff9999, 0xffcc9999, 0xff999999, 0xff669999, 0xff339999,
        0xff009999, 0xffff6699, 0xffcc6699, 0xff996699, 0xff666699, 0xff336699, 0xff006699, 0xffff3399, 0xffcc3399,
        0xff993399, 0xff663399, 0xff333399, 0xff003399, 0xffff0099, 0xffcc0099, 0xff990099, 0xff660099, 0xff330099,
        0xff000099, 0xffffff66, 0xffccff66, 0xff99ff66, 0xff66ff66, 0xff33ff66, 0xff00ff66, 0xffffcc66, 0xffcccc66,
        0xff99cc66, 0xff66cc66, 0xff33cc66, 0xff00cc66, 0xffff9966, 0xffcc9966, 0xff999966, 0xff669966, 0xff339966,
        0xff009966, 0xffff6666, 0xffcc6666, 0xff996666, 0xff666666, 0xff336666, 0xff006666, 0xffff3366, 0xffcc3366,
        0xff993366, 0xff663366, 0xff333366, 0xff003366, 0xffff0066, 0xffcc0066, 0xff990066, 0xff660066, 0xff330066,
        0xff000066, 0xffffff33, 0xffccff33, 0xff99ff33, 0xff66ff33, 0xff33ff33, 0xff00ff33, 0xffffcc33, 0xffcccc33,
        0xff99cc33, 0xff66cc33, 0xff33cc33, 0xff00cc33, 0xffff9933, 0xffcc9933, 0xff999933, 0xff669933, 0xff339933,
        0xff009933, 0xffff6633, 0xffcc6633, 0xff996633, 0xff666633, 0xff336633, 0xff006633, 0xffff3333, 0xffcc3333,
        0xff993333, 0xff663333, 0xff333333, 0xff003333, 0xffff0033, 0xffcc0033, 0xff990033, 0xff660033, 0xff330033,
        0xff000033, 0xffffff00, 0xffccff00, 0xff99ff00, 0xff66ff00, 0xff33ff00, 0xff00ff00, 0xffffcc00, 0xffcccc00,
        0xff99cc00, 0xff66cc00, 0xff33cc00, 0xff00cc00, 0xffff9900, 0xffcc9900, 0xff999900, 0xff669900, 0xff339900,
        0xff009900, 0xffff6600, 0xffcc6600, 0xff996600, 0xff666600, 0xff336600, 0xff006600, 0xffff3300, 0xffcc3300,
        0xff993300, 0xff663300, 0xff333300, 0xff003300, 0xffff0000, 0xffcc0000, 0xff990000, 0xff660000, 0xff330000,
        0xff0000ee, 0xff0000dd, 0xff0000bb, 0xff0000aa, 0xff000088, 0xff000077, 0xff000055, 0xff000044, 0xff000022,
        0xff000011, 0xff00ee00, 0xff00dd00, 0xff00bb00, 0xff00aa00, 0xff008800, 0xff007700, 0xff005500, 0xff004400,
        0xff002200, 0xff001100, 0xffee0000, 0xffdd0000, 0xffbb0000, 0xffaa0000, 0xff880000, 0xff770000, 0xff550000,
        0xff440000, 0xff220000, 0xff110000, 0xffeeeeee, 0xffdddddd, 0xffbbbbbb, 0xffaaaaaa, 0xff888888, 0xff777777,
        0xff555555, 0xff444444, 0xff222222, 0xff111111,
    ];
    P.map(|c| c.to_le_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chunk(id: &[u8; 4], content: &[u8], children: &[u8]) -> Vec<u8> {
        let mut out = id.to_vec();
        out.extend((content.len() as u32).to_le_bytes());
        out.extend((children.len() as u32).to_le_bytes());
        out.extend(content);
        out.extend(children);
        out
    }

    fn file(models: &[([u32; 3], Vec<[u8; 4]>)], extra: &[u8]) -> Vec<u8> {
        let mut children = Vec::new();
        for (size, voxels) in models {
            children.extend(chunk(b"SIZE", &size.iter().flat_map(|v| v.to_le_bytes()).collect::<Vec<_>>(), &[]));
            let mut xyzi = (voxels.len() as u32).to_le_bytes().to_vec();
            xyzi.extend(voxels.iter().flatten());
            children.extend(chunk(b"XYZI", &xyzi, &[]));
        }
        children.extend(extra);
        let mut out = b"VOX ".to_vec();
        out.extend(150u32.to_le_bytes());
        out.extend(chunk(b"MAIN", &[], &children));
        out
    }

    #[test]
    fn single_voxel_is_a_cube() {
        let bytes = file(&[([1, 1, 1], vec![[0, 0, 0, 75]])], &[]);
        let scene = load(Path::new("a.vox"), &bytes, &Limits::viewer()).unwrap();
        assert_eq!(scene.triangle_count(), 12);
        let p = &scene.meshes[0].primitives[0];
        assert_eq!(p.colors.len(), 24);
        // Default palette entry 75 is 0xff99ff99: R 0x99, G 0xff, B 0x99.
        assert!((p.colors[0].y - 1.0).abs() < 1e-6 && p.colors[0].x < 0.5);
        // Each face's winding agrees with its normal.
        for t in p.indices.chunks(3) {
            let [a, b, c] = [t[0], t[1], t[2]].map(|i| p.positions[i as usize]);
            let n = (b - a).cross(c - a).normalize();
            assert!((n - p.normals[t[0] as usize]).length() < 1e-6);
        }
    }

    #[test]
    fn greedy_meshing_merges_and_culls() {
        // A 4x4x1 slab of one colour is a box: 6 quads, 12 triangles.
        let voxels = (0..16).map(|i| [i % 4, i / 4, 0, 5]).collect();
        let scene = load(Path::new("a.vox"), &file(&[([4, 4, 1], voxels)], &[]), &Limits::viewer()).unwrap();
        assert_eq!(scene.triangle_count(), 12);
        // Two colours side by side split the faces they don't share.
        let scene =
            load(Path::new("a.vox"), &file(&[([2, 1, 1], vec![[0, 0, 0, 5], [1, 0, 0, 6]])], &[]), &Limits::viewer())
                .unwrap();
        assert_eq!(scene.triangle_count(), 20);
    }

    #[test]
    fn scene_graph_places_models() {
        let dict = |pairs: &[(&str, &str)]| {
            let mut out = (pairs.len() as u32).to_le_bytes().to_vec();
            for (k, v) in pairs {
                out.extend((k.len() as u32).to_le_bytes());
                out.extend(k.as_bytes());
                out.extend((v.len() as u32).to_le_bytes());
                out.extend(v.as_bytes());
            }
            out
        };
        let trn = |id: i32, child: i32, frame: &[(&str, &str)]| {
            let mut c = id.to_le_bytes().to_vec();
            c.extend(dict(&[]));
            c.extend(child.to_le_bytes());
            c.extend((-1i32).to_le_bytes());
            c.extend(0i32.to_le_bytes());
            c.extend(1i32.to_le_bytes());
            c.extend(dict(frame));
            chunk(b"nTRN", &c, &[])
        };
        let mut extra = trn(0, 1, &[]);
        let mut grp = 1i32.to_le_bytes().to_vec();
        grp.extend(dict(&[]));
        grp.extend(2u32.to_le_bytes());
        grp.extend(2i32.to_le_bytes());
        grp.extend(4i32.to_le_bytes());
        extra.extend(chunk(b"nGRP", &grp, &[]));
        // A mirrored copy: _r with a negative first row.
        extra.extend(trn(2, 3, &[("_t", "10 0 0")]));
        extra.extend(trn(4, 3, &[("_t", "-10 0 0"), ("_r", "20")]));
        let mut shp = 3i32.to_le_bytes().to_vec();
        shp.extend(dict(&[]));
        shp.extend(1u32.to_le_bytes());
        shp.extend(0i32.to_le_bytes());
        shp.extend(dict(&[]));
        extra.extend(chunk(b"nSHP", &shp, &[]));
        let bytes = file(&[([1, 1, 1], vec![[0, 0, 0, 1]])], &extra);
        let scene = load(Path::new("a.vox"), &bytes, &Limits::viewer()).unwrap();
        assert_eq!(scene.meshes.len(), 1, "instances share the mesh");
        let pose = slerp_scene::Pose::rest(&scene);
        let b = scene.bounds(&pose);
        // x spans -11..11 voxels, 0.1 m each: the mirrored cube lands on
        // the far side of its pivot.
        assert!((b.min.x + 1.1).abs() < 1e-5 && (b.max.x - 1.1).abs() < 1e-5, "{b:?}");
        assert_eq!(rotation(20), Mat3::from_diagonal(Vec3::new(-1.0, 1.0, 1.0)));
    }

    #[test]
    fn survives_truncation_and_garbage() {
        let bytes = file(
            &[([300, 2, 2], vec![[0, 0, 0, 1], [1, 1, 1, 2], [255, 1, 1, 0]])],
            &chunk(b"MATL", &[1, 0, 0, 0, 255, 255, 255, 255], &[]),
        );
        for cut in 0..bytes.len() {
            let _ = load(Path::new("a.vox"), &bytes[..cut], &Limits::viewer());
        }
        let mut lie = bytes.clone();
        lie[20..24].copy_from_slice(&u32::MAX.to_le_bytes());
        let _ = load(Path::new("a.vox"), &lie, &Limits::viewer());
        let limits = Limits { max_triangles: 4, ..Limits::viewer() };
        assert!(matches!(load(Path::new("a.vox"), &bytes, &limits), Err(LoadError::TooLarge(_))));
    }
}
