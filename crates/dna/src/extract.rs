//! Reading a DNA file with openriglogic and copying out what Slerp uses
//! into plain Rust data, which can cross a process boundary (see
//! `crate::JOBS`).  Nothing here trusts the counts and indices it copies;
//! `scene` checks them all.

use crate::ffi::bridge;
use std::path::Path;

/// openriglogic's data layers (`dna::DataLayer`).
pub(crate) mod layer {
    pub const DEFINITION: u32 = 2 | 1;
    pub const BEHAVIOR: u32 = 4 | DEFINITION;
    pub const GEOMETRY: u32 = 8 | DEFINITION;
    pub const GEOMETRY_WITHOUT_BLEND_SHAPES: u32 = 16 | DEFINITION;
    /// Everything RigLogic evaluates: joints, blend shapes and animated
    /// maps, the RBF solvers and twist and swing setups of a body, and
    /// machine-learned behaviour.
    pub const RIG: u32 = BEHAVIOR | 32 | DEFINITION | 64 | 128 | 256;
}

/// What to read.
#[derive(Clone, Debug, Default)]
pub(crate) struct Request {
    /// Levels of detail to keep, all when empty.
    pub lods: Vec<u16>,
    /// Read meshes at all.
    pub geometry: bool,
    /// Read blend shape targets with the meshes.
    pub blend_shapes: bool,
    /// Build RigLogic and evaluate it once with every control at rest and
    /// once at full strength, so a file that would crash it does so here.
    pub probe_rig: bool,
}

impl Request {
    /// The request as fields of an isolated job's input.
    pub fn pack(&self, mut pack: slerp_isolate::Pack) -> slerp_isolate::Pack {
        pack = pack.u32(self.lods.len() as u32);
        for &lod in &self.lods {
            pack = pack.u32(lod as u32);
        }
        pack.bool(self.geometry).bool(self.blend_shapes).bool(self.probe_rig)
    }

    pub fn unpack(u: &mut slerp_isolate::Unpack) -> Result<Request, String> {
        let n = u.u32()?;
        if n > 64 {
            return Err("a damaged request".into());
        }
        let lods = (0..n).map(|_| u.u32().map(|l| l as u16)).collect::<Result<Vec<_>, _>>()?;
        Ok(Request { lods, geometry: u.bool()?, blend_shapes: u.bool()?, probe_rig: u.bool()? })
    }
}

/// Everything Slerp uses from one DNA file, as stored (DNA units).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DnaData {
    pub name: String,
    /// Levels of detail read.
    pub lod_count: u16,
    /// Translation unit, rotation unit, coordinate system axes, rotation
    /// sequence, face winding (openriglogic's enum values).
    pub conventions: Vec<u32>,
    pub joint_names: Vec<String>,
    /// Parent of each joint; a root is its own parent.
    pub joint_parents: Vec<u16>,
    /// Six floats per joint: translation, then rotation.
    pub neutral_joints: Vec<f32>,
    /// Joint indices per level of detail.
    pub lod_joints: Vec<Vec<u16>>,
    pub controls: Vec<String>,
    pub channels: Vec<String>,
    pub animated_maps: u16,
    pub mesh_names: Vec<String>,
    /// Mesh indices per level of detail.
    pub lod_meshes: Vec<Vec<u16>>,
    /// The meshes read, by index into `mesh_names`.
    pub meshes: Vec<(u16, MeshData)>,
    /// Peak bytes openriglogic held while reading.
    pub peak_bytes: u64,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct MeshData {
    /// xyz per position.
    pub positions: Vec<f32>,
    /// xyz per normal.
    pub normals: Vec<f32>,
    /// uv per texture coordinate.
    pub uvs: Vec<f32>,
    /// Position, texture coordinate and normal index per vertex layout.
    pub layouts: Vec<u32>,
    /// Each face as its corner count followed by its layout indices.
    pub faces: Vec<u32>,
    /// Influence count per position, then the joints and weights in order.
    pub skin_counts: Vec<u32>,
    pub skin_joints: Vec<u16>,
    pub skin_weights: Vec<f32>,
    pub targets: Vec<TargetData>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct TargetData {
    pub channel: u16,
    /// Positions moved, and xyz per moved position.
    pub vertices: Vec<u32>,
    pub deltas: Vec<f32>,
}

/// Memory openriglogic may use for a file of `bytes`: a MetaHuman head
/// needs about 1.3 times its size, so a budget of four times the size plus
/// 64 MB leaves room while stopping files that ask for absurd amounts.
pub(crate) fn budget(bytes: u64) -> u64 {
    bytes.saturating_mul(4).saturating_add(64 << 20)
}

/// Reads `path` as `req` says.
pub(crate) fn extract(path: &Path, req: &Request) -> Result<DnaData, String> {
    let bytes = std::fs::metadata(path).map_err(|e| e.to_string())?.len();
    let mut layers = layer::DEFINITION;
    if req.geometry {
        layers |= if req.blend_shapes { layer::GEOMETRY } else { layer::GEOMETRY_WITHOUT_BLEND_SHAPES };
    }
    if req.probe_rig {
        layers |= layer::RIG;
    }
    let dna = bridge::read_dna(&path_bytes(path)?, layers, &req.lods, budget(bytes)).map_err(|e| e.to_string())?;
    let dna = dna.as_ref().ok_or("openriglogic returned no reader")?;

    // 1. Descriptor and definition.
    let mut out = DnaData {
        name: bridge::dna_name(dna),
        lod_count: bridge::dna_lod_count(dna),
        conventions: bridge::dna_conventions(dna),
        ..Default::default()
    };
    let joints = bridge::joint_count(dna);
    out.joint_names = (0..joints).map(|i| bridge::joint_name(dna, i)).collect();
    out.joint_parents = (0..joints).map(|i| bridge::joint_parent(dna, i)).collect();
    out.neutral_joints = bridge::neutral_joints(dna);
    out.lod_joints = (0..out.lod_count).map(|l| bridge::joints_for_lod(dna, l)).collect();
    out.controls = (0..bridge::raw_control_count(dna)).map(|i| bridge::raw_control_name(dna, i)).collect();
    out.channels = (0..bridge::channel_count(dna)).map(|i| bridge::channel_name(dna, i)).collect();
    out.animated_maps = bridge::animated_map_count(dna);
    out.mesh_names = (0..bridge::mesh_count(dna)).map(|i| bridge::mesh_name(dna, i)).collect();
    out.lod_meshes = (0..out.lod_count).map(|l| bridge::meshes_for_lod(dna, l)).collect();

    // 2. Geometry of every mesh some kept level uses.
    if req.geometry {
        let mut wanted: Vec<u16> = out.lod_meshes.iter().flatten().copied().collect();
        wanted.sort_unstable();
        wanted.dedup();
        for m in wanted.into_iter().filter(|&m| (m as usize) < out.mesh_names.len()) {
            out.meshes.push((m, mesh(dna, m, req.blend_shapes)));
        }
    }

    // 3. Exercise RigLogic, so a file that breaks it breaks here.
    if req.probe_rig {
        let mut rig = bridge::make_rig(dna, budget(bytes)).map_err(|e| e.to_string())?;
        let count = bridge::rig_control_count(&rig) as usize;
        for value in [0.0f32, 1.0] {
            for lod in 0..bridge::rig_lod_count(&rig) {
                bridge::rig_set_lod(rig.pin_mut(), lod);
                bridge::rig_evaluate(rig.pin_mut(), &vec![value; count]).map_err(|e| e.to_string())?;
            }
        }
    }
    out.peak_bytes = bridge::dna_peak_bytes(dna);
    Ok(out)
}

fn mesh(dna: &bridge::Dna, m: u16, blend_shapes: bool) -> MeshData {
    let mut out = MeshData {
        positions: bridge::mesh_positions(dna, m),
        normals: bridge::mesh_normals(dna, m),
        uvs: bridge::mesh_uvs(dna, m),
        layouts: bridge::mesh_layouts(dna, m),
        faces: bridge::mesh_faces(dna, m),
        ..Default::default()
    };
    bridge::mesh_skin(dna, m, &mut out.skin_counts, &mut out.skin_joints, &mut out.skin_weights);
    if blend_shapes {
        for t in 0..bridge::target_count(dna, m) {
            let mut target = TargetData { channel: bridge::target_channel(dna, m, t), ..Default::default() };
            bridge::mesh_target(dna, m, t, &mut target.vertices, &mut target.deltas);
            if !target.vertices.is_empty() {
                out.targets.push(target);
            }
        }
    }
    out
}

/// The path as openriglogic opens it: raw bytes on Unix, UTF-8 elsewhere.
pub(crate) fn path_bytes(path: &Path) -> Result<Vec<u8>, String> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        Ok(path.as_os_str().as_bytes().to_vec())
    }
    #[cfg(not(unix))]
    {
        path.to_str().map(|s| s.as_bytes().to_vec()).ok_or_else(|| "the path isn't valid Unicode".to_string())
    }
}
