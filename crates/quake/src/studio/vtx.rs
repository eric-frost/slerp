//! The `.vtx` file beside a Source model (`.dx90.vtx`, `.dx80.vtx`,
//! `.sw.vtx` or plain `.vtx`): the triangles of each mesh, for each level
//! of detail, as strip groups of vertices that point back into the mesh's
//! own vertices and strips of 16-bit indices into the group.
//!
//! The file mirrors the model's tree: body parts, models, levels of
//! detail, meshes.  Only the full-detail level is read.  Games from Left 4
//! Dead 2 on added two fields to strip groups and strips without changing
//! the version number, so the file is read with the old record sizes and,
//! when those don't fit together, the new ones.

use crate::read::{Bytes, Result, cap};
use slerp_scene::LoadError;

const HEADER_BYTES: usize = 36;
const VERTEX_BYTES: usize = 9;
/// Body parts, models and meshes, as the `.mdl` file caps them.
const MAX_PARTS: usize = 256;
const MAX_MESHES: usize = 1024;
/// Strip groups of a mesh and strips of a group: studiomdl writes a few.
const MAX_GROUPS: usize = 256;
const MAX_STRIPS: usize = 1 << 14;
/// Vertices of one strip group, which its 16-bit indices address.
const MAX_GROUP_VERTICES: usize = 1 << 16;
/// Triangle corners of the whole file.  Records may share their data, so
/// without a total a small file could list the same indices endlessly.
const MAX_CORNERS: usize = 1 << 25;

const STRIP_TRILIST: u8 = 1;
const STRIP_TRISTRIP: u8 = 2;

/// Triangles of every mesh of every model of every body part, as indices
/// into the mesh's own vertices (`[part][model][mesh]`).
pub struct Vtx {
    pub parts: Vec<Vec<Vec<Vec<u32>>>>,
    pub checksum: i32,
}

/// Reads the full-detail triangles.  `extended_first` tries the later
/// games' record sizes first, as models of version 49 mostly use them.
pub fn read(data: &[u8], extended_first: bool) -> Result<Vtx> {
    let b = Bytes::new(data);
    b.slice(0, HEADER_BYTES)?;
    let version = b.i32(0)?;
    if version != 7 {
        return Err(LoadError::Unsupported(format!(".vtx version {version}")));
    }
    match read_with(b, extended_first) {
        Ok(v) => Ok(v),
        Err(first) => read_with(b, !extended_first).map_err(|_| first),
    }
}

/// Offset `rel` from `base`, which must stay inside the file.
fn at(b: Bytes, base: usize, rel: i32) -> Result<usize> {
    base.checked_add_signed(rel as isize)
        .filter(|&a| a <= b.len())
        .ok_or_else(|| LoadError::invalid("a .vtx offset points outside the file"))
}

/// Reads the tree with the record sizes of the original layout, or with
/// the two extra topology fields of later games when `extended`.
fn read_with(b: Bytes, extended: bool) -> Result<Vtx> {
    let (group_bytes, strip_bytes) = if extended { (33, 35) } else { (25, 27) };
    let checksum = b.i32(16)?;
    let part_count = cap(b.index(28)?, MAX_PARTS, ".vtx body parts")?;
    let part_base = b.index(32)?;
    let mut budget = MAX_CORNERS;
    let mut parts = Vec::with_capacity(part_count);
    for p in 0..part_count {
        let pa = part_base + p * 8;
        let model_count = cap(b.index(pa)?, MAX_PARTS, ".vtx models")?;
        let model_base = at(b, pa, b.i32(pa + 4)?)?;
        let mut models = Vec::with_capacity(model_count);
        for m in 0..model_count {
            let ma = model_base + m * 8;
            let lods = b.index(ma)?;
            let mut meshes = Vec::new();
            if lods > 0 {
                let lod = at(b, ma, b.i32(ma + 4)?)?;
                let mesh_count = cap(b.index(lod)?, MAX_MESHES, ".vtx meshes")?;
                let mesh_base = at(b, lod, b.i32(lod + 4)?)?;
                for k in 0..mesh_count {
                    let mesh = mesh_base + k * 9;
                    meshes.push(read_mesh(b, mesh, group_bytes, strip_bytes, &mut budget)?);
                }
            }
            models.push(meshes);
        }
        parts.push(models);
    }
    Ok(Vtx { parts, checksum })
}

fn read_mesh(b: Bytes, mesh: usize, group_bytes: usize, strip_bytes: usize, budget: &mut usize) -> Result<Vec<u32>> {
    let groups = cap(b.index(mesh)?, MAX_GROUPS, "strip groups")?;
    let group_base = at(b, mesh, b.i32(mesh + 4)?)?;
    let mut out = Vec::new();
    for g in 0..groups {
        let sg = group_base + g * group_bytes;
        b.slice(sg, group_bytes)?;
        let verts = cap(b.index(sg)?, MAX_GROUP_VERTICES, "strip group vertices")?;
        let vert_base = at(b, sg, b.i32(sg + 4)?)?;
        let index_count = b.index(sg + 8)?;
        let index_base = at(b, sg, b.i32(sg + 12)?)?;
        let strips = cap(b.index(sg + 16)?, MAX_STRIPS, "strips")?;
        let strip_base = at(b, sg, b.i32(sg + 20)?)?;
        let vdata = Bytes::new(b.array(vert_base, verts, VERTEX_BYTES)?);
        let idata = Bytes::new(b.array(index_base, index_count, 2)?);
        // The mesh vertex each strip group vertex stands for.
        let mut mesh_vertex = Vec::with_capacity(verts);
        for v in 0..verts {
            mesh_vertex.push(vdata.u16(v * VERTEX_BYTES + 4)? as u32);
        }
        let index = |i: usize| -> Result<u32> {
            let v = idata.u16(i * 2)? as usize;
            mesh_vertex.get(v).copied().ok_or_else(|| LoadError::invalid("a .vtx index points past its vertices"))
        };
        let push = |a: usize, c: usize, d: usize, out: &mut Vec<u32>| -> Result<()> {
            let (a, c, d) = (index(a)?, index(c)?, index(d)?);
            if a != c && c != d && a != d {
                out.extend_from_slice(&[a, c, d]);
            }
            Ok(())
        };
        let mut spend = |corners: usize| -> Result<()> {
            *budget = budget
                .checked_sub(corners)
                .ok_or_else(|| LoadError::TooLarge("the .vtx file lists too many triangles".into()))?;
            Ok(())
        };
        if strips == 0 {
            spend(index_count)?;
            for t in 0..index_count / 3 {
                push(3 * t, 3 * t + 1, 3 * t + 2, &mut out)?;
            }
            continue;
        }
        b.array(strip_base, strips, strip_bytes)?;
        for s in 0..strips {
            let st = strip_base + s * strip_bytes;
            let count = b.index(st)?;
            let first = b.index(st + 4)?;
            let flags = b.u8(st + 18)?;
            if first.checked_add(count).is_none_or(|end| end > index_count) {
                return Err(LoadError::invalid("a .vtx strip runs past its indices"));
            }
            spend(count.saturating_mul(3))?;
            if flags & STRIP_TRISTRIP != 0 && flags & STRIP_TRILIST == 0 {
                for i in 2..count {
                    let (x, y, z) = (first + i - 2, first + i - 1, first + i);
                    if i % 2 == 0 { push(x, y, z, &mut out)? } else { push(y, x, z, &mut out)? }
                }
            } else {
                for t in 0..count / 3 {
                    push(first + 3 * t, first + 3 * t + 1, first + 3 * t + 2, &mut out)?;
                }
            }
        }
    }
    Ok(out)
}
