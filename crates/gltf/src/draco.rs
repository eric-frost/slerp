//! Draco geometry compression: primitives compressed with
//! `KHR_draco_mesh_compression` in glTF and GLB files, and plain `.drc`
//! files.  Decoding is `draco-core`'s, a pure-Rust port of Google's
//! decoder covering every bitstream version and method (sequential and
//! edgebreaker connectivity, every prediction scheme, quantization,
//! octahedral normals, generic attributes), so no C++ is linked.
//!
//! A Draco stream states how many points and faces it holds, and constant
//! attributes compress to almost nothing, so a few bytes can legitimately
//! decode to millions of points.  The decoder is therefore given the load's
//! remaining budget as its ceiling, and what it produced is charged to the
//! budget afterwards; `draco-core` also refuses counts no data backs.

use crate::Ctx;
use crate::accessor::Accessors;
use crate::json::DracoJ;
use draco_core::{
    DataType, DecodeLimits, DecoderBuffer, FaceIndex, GeometryAttributeType, MeshDecoder, PointAttribute,
};
use slerp_scene::glam::{Vec2, Vec3, Vec4};
use slerp_scene::{Info, Limits, LoadError, Mesh, Primitive, Scene, Topology};
use std::path::Path;

/// Whether the bytes start a Draco stream.
pub fn sniff(head: &[u8]) -> bool {
    head.starts_with(b"DRACO")
}

/// Decodes a Draco stream within the load's budget.  A panic in the
/// decoder, which promises none, is caught and reported as a damaged
/// stream, since glTF primitives decode on worker threads.
fn decode(data: &[u8], ctx: &Ctx) -> Result<draco_core::Mesh, LoadError> {
    let budget = ctx.remaining();
    let limits =
        DecodeLimits::default().with_max_decoded_bytes(budget).with_max_points(budget / 12).with_max_faces(budget / 12);
    let result = std::panic::catch_unwind(|| {
        let mut buffer = DecoderBuffer::new(data).with_limits(limits);
        let mut mesh = draco_core::Mesh::new();
        MeshDecoder::new().decode(&mut buffer, &mut mesh).map(|()| mesh)
    });
    let mesh = match result {
        Ok(Ok(mesh)) => mesh,
        Ok(Err(e)) if matches!(e.kind(), draco_core::ErrorKind::LimitExceeded) => {
            return Err(LoadError::TooLarge(format!("Draco geometry: {e}")));
        }
        Ok(Err(e)) => return Err(LoadError::invalid(format!("Draco geometry: {e}"))),
        Err(_) => return Err(LoadError::invalid("Draco geometry: the decoder failed")),
    };
    // What the decoder holds now: each attribute's values, and the faces.
    let mut bytes = mesh.num_faces() as u64 * 12;
    for i in 0..mesh.num_attributes() {
        let a = mesh.attribute(i);
        bytes = bytes.saturating_add(a.size() as u64 * a.num_components() as u64 * a.data_type().byte_length() as u64);
    }
    ctx.reserve(bytes)?;
    Ok(mesh)
}

/// Reads an attribute as `N` floats per point, completing values with
/// fewer components from `pad`, and mapping integers to 0..1 or -1..1 when
/// `normalized`.
fn read<const N: usize>(attr: &PointAttribute, points: usize, pad: [f32; N], normalized: bool) -> Vec<[f32; N]> {
    let have = (attr.num_components() as usize).min(N);
    let raw = attr.read_f32s(points, N);
    let scale = match (normalized, attr.data_type()) {
        (true, DataType::Int8) => 1.0 / 127.0,
        (true, DataType::Uint8) => 1.0 / 255.0,
        (true, DataType::Int16) => 1.0 / 32767.0,
        (true, DataType::Uint16) => 1.0 / 65535.0,
        _ => 1.0,
    };
    raw.chunks_exact(N)
        .map(|v| {
            let mut out = pad;
            for k in 0..have {
                let x = v[k] * scale;
                out[k] = if !x.is_finite() {
                    0.0
                } else if scale != 1.0 {
                    x.max(-1.0)
                } else {
                    x
                };
            }
            out
        })
        .collect()
}

/// Every face's corners, as indices of points.
fn indices(mesh: &draco_core::Mesh) -> Vec<u32> {
    (0..mesh.num_faces()).flat_map(|f| mesh.face(FaceIndex(f as u32)).map(|p| p.0)).collect()
}

/*┌─────────────────╮
| glTF            |
└─────────────────╯*/
//region glTF

/// A primitive's Draco geometry, decoded, with the glTF attributes it
/// holds by name.
pub struct Decoded {
    mesh: draco_core::Mesh,
    attributes: Vec<(String, u32)>,
}

impl Decoded {
    pub fn new(ext: &DracoJ, acc: &Accessors, ctx: &Ctx) -> Result<Decoded, LoadError> {
        let view = acc
            .views
            .get(ext.buffer_view)
            .and_then(|v| v.as_ref())
            .ok_or_else(|| LoadError::invalid(format!("Draco buffer view {} has no data", ext.buffer_view)))?;
        let mesh = decode(view.bytes(), ctx)?;
        let attributes = ext.attributes.iter().map(|(k, &v)| (k.clone(), v)).collect();
        Ok(Decoded { mesh, attributes })
    }

    pub fn points(&self) -> usize {
        self.mesh.num_points()
    }

    /// The glTF attribute `name` from the Draco geometry, or `None` when
    /// the extension doesn't hold it (it is then read from its accessor).
    pub fn read<const N: usize>(
        &self,
        name: &str,
        pad: [f32; N],
        normalized: bool,
    ) -> Option<Result<Vec<[f32; N]>, LoadError>> {
        let &(_, id) = self.attributes.iter().find(|(n, _)| n == name)?;
        Some(match self.mesh.attribute_by_unique_id(id) {
            Some(a) => Ok(read(a, self.points(), pad, normalized || a.normalized())),
            None => Err(LoadError::invalid(format!("the Draco geometry has no attribute {id} for {name}"))),
        })
    }

    pub fn indices(&self) -> Vec<u32> {
        indices(&self.mesh)
    }
}

//endregion

/*┌─────────────────╮
| .drc files      |
└─────────────────╯*/
//region .drc files

/// Loads a plain `.drc` file: a mesh, or a point cloud when it has no
/// faces.  Draco keeps no units or axes; files usually come from OBJ or PLY
/// and are taken as Y up.  Colours are taken as display values, as those
/// formats store them.
pub fn load_drc(path: &Path, bytes: &[u8], limits: &Limits) -> Result<Scene, LoadError> {
    if bytes.len() as u64 > limits.max_file_bytes {
        return Err(LoadError::TooLarge(format!("{} bytes", bytes.len())));
    }
    if !sniff(bytes) {
        return Err(LoadError::NotMine);
    }
    let ctx = Ctx::new(bytes.len() as u64, 0);
    let mesh = decode(bytes, &ctx)?;
    let n = mesh.num_points();
    let named = |t: GeometryAttributeType| mesh.named_attribute(t);
    let Some(position) = named(GeometryAttributeType::Position) else {
        return Err(LoadError::invalid("the Draco file has no positions"));
    };
    let mut prim = Primitive {
        positions: read(position, n, [0.0; 3], false).into_iter().map(Vec3::from_array).collect(),
        ..Default::default()
    };
    if let Some(a) = named(GeometryAttributeType::Normal) {
        prim.normals = read(a, n, [0.0; 3], a.normalized())
            .into_iter()
            .map(|v| Vec3::from_array(v).normalize_or(Vec3::Y))
            .collect();
    }
    if let Some(a) = named(GeometryAttributeType::TexCoord) {
        prim.uv0 = read(a, n, [0.0; 2], a.normalized()).into_iter().map(|v| Vec2::new(v[0], 1.0 - v[1])).collect();
    }
    if let Some(a) = named(GeometryAttributeType::Color) {
        let normalized = a.normalized() || matches!(a.data_type(), DataType::Uint8 | DataType::Uint16);
        prim.colors = read(a, n, [0.0, 0.0, 0.0, 1.0], normalized)
            .into_iter()
            .map(|c| Vec4::new(linear(c[0]), linear(c[1]), linear(c[2]), c[3].clamp(0.0, 1.0)))
            .collect();
    }
    let faces = mesh.num_faces();
    if faces as u64 > limits.max_triangles {
        return Err(LoadError::TooLarge(format!("{faces} triangles")));
    }
    if faces > 0 {
        prim.indices = indices(&mesh);
        if prim.indices.iter().any(|&i| i as usize >= n) {
            return Err(LoadError::invalid("a Draco face points past the last vertex"));
        }
        prim.ensure_normals();
    } else {
        prim.topology = Topology::Points;
        prim.indices = (0..n as u32).collect();
    }
    let mut scene = Scene {
        name: path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default(),
        ..Default::default()
    };
    scene.add_mesh_node(Mesh { name: scene.name.clone(), primitives: vec![prim], target_names: Vec::new() });
    let generic = (0..mesh.num_attributes())
        .filter(|&i| mesh.attribute(i).attribute_type() == GeometryAttributeType::Generic)
        .count();
    scene.info = Info {
        format: if faces > 0 { "Draco mesh".into() } else { "Draco point cloud".into() },
        details: vec![
            (
                "Version".into(),
                format!("{}.{}", bytes.get(5).copied().unwrap_or(0), bytes.get(6).copied().unwrap_or(0)),
            ),
            ("Points".into(), n.to_string()),
        ],
        ..Default::default()
    };
    if generic > 0 {
        scene.info.details.push(("Other attributes".into(), generic.to_string()));
    }
    Ok(scene)
}

fn linear(c: f32) -> f32 {
    let c = c.clamp(0.0, 1.0);
    if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
}

//endregion
