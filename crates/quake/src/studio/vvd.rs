//! The `.vvd` file beside a Source model: every vertex of the model in its
//! reference pose, with up to three bone weights each.
//!
//! A model with levels of detail lists its vertices once and a fixup table
//! says which runs of them each level uses; the full-detail level is all
//! the runs in order.  Without fixups the vertices are used as stored.

use crate::read::{Bytes, Result, cap};
use slerp_scene::LoadError;
use slerp_scene::glam::{Vec2, Vec3};

const HEADER_BYTES: usize = 64;
const VERTEX_BYTES: usize = 48;
/// More vertices than any Source model has (the engine stops at 65,536 per
/// mesh, and models have few meshes).
const MAX_VERTICES: usize = 1 << 22;
const MAX_FIXUPS: usize = 1 << 16;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Vertex {
    pub position: Vec3,
    pub normal: Vec3,
    pub uv: Vec2,
    /// Bones and their weights; only the first `count` are used.
    pub bones: [u8; 3],
    pub weights: [f32; 3],
    pub count: u8,
}

/// The vertices of the full-detail level, fixups applied, and the checksum
/// that ties the file to its `.mdl`.
pub struct Vvd {
    pub vertices: Vec<Vertex>,
    pub checksum: i32,
}

pub fn read(data: &[u8]) -> Result<Vvd> {
    let b = Bytes::new(data);
    if !b.starts_with(b"IDSV") {
        return Err(LoadError::invalid("the .vvd file doesn't start with IDSV"));
    }
    b.slice(0, HEADER_BYTES)?;
    let version = b.i32(4)?;
    if version != 4 {
        return Err(LoadError::Unsupported(format!(".vvd version {version}")));
    }
    let checksum = b.i32(8)?;
    let lod0 = cap(b.index(16)?, MAX_VERTICES, "vertices")?;
    let fixups = cap(b.index(48)?, MAX_FIXUPS, "vertex fixups")?;
    let fixup_at = b.index(52)?;
    let start = b.index(56)?;
    // The whole table must be in the file before any of it is copied.
    let stored = (b.len().saturating_sub(start)) / VERTEX_BYTES;
    let read_vertex = |i: usize| -> Result<Vertex> {
        if i >= stored {
            return Err(LoadError::invalid("a vertex lies past the end of the .vvd file"));
        }
        let at = start + i * VERTEX_BYTES;
        let count = b.u8(at + 15)?.min(3);
        Ok(Vertex {
            weights: [b.f32(at)?, b.f32(at + 4)?, b.f32(at + 8)?],
            bones: [b.u8(at + 12)?, b.u8(at + 13)?, b.u8(at + 14)?],
            count,
            position: b.vec3(at + 16)?,
            normal: b.vec3(at + 28)?,
            uv: Vec2::new(b.f32(at + 40)?, b.f32(at + 44)?),
        })
    };
    let mut vertices = Vec::with_capacity(lod0.min(stored));
    if fixups == 0 {
        for i in 0..lod0 {
            vertices.push(read_vertex(i)?);
        }
    } else {
        b.array(fixup_at, fixups, 12)?;
        for f in 0..fixups {
            let at = fixup_at + f * 12;
            // Every run whose level is 0 or above belongs to level 0.
            if b.i32(at)? < 0 {
                continue;
            }
            let source = b.index(at + 4)?;
            let count = b.index(at + 8)?;
            if vertices.len().saturating_add(count) > lod0 {
                return Err(LoadError::invalid("the .vvd fixups list more vertices than the file has"));
            }
            for i in source..source.saturating_add(count) {
                vertices.push(read_vertex(i)?);
            }
        }
    }
    Ok(Vvd { vertices, checksum })
}
