//! antimatter15's `.splat`: no header, one 32-byte record per splat with
//! the centre and linear scales as little-endian floats, the base colour
//! and opacity as bytes, and the rotation as four bytes (w, x, y, z,
//! each `128 + 128 × q`).  The records come straight from a reference PLY,
//! so the axes are the PLY's.

use crate::{Builder, SplatFormat};
use glam::{Quat, Vec3, Vec4};
use slerp_scene::{Info, Limits, LoadError, Scene};
use std::io::{BufReader, Read};
use std::path::Path;

const RECORD: usize = 32;

fn floats(r: &[u8], at: usize) -> Vec3 {
    let f = |i: usize| f32::from_le_bytes([r[i], r[i + 1], r[i + 2], r[i + 3]]);
    Vec3::new(f(at), f(at + 4), f(at + 8))
}

/// Whether the first record looks like a splat: a finite centre and
/// positive, finite, not absurd scales.  The format has no signature, so
/// this keeps other files named `.splat` from being misread.
pub(crate) fn plausible(head: &[u8]) -> bool {
    if head.len() < RECORD {
        return false;
    }
    let (p, s) = (floats(head, 0), floats(head, 12));
    p.is_finite() && p.abs().max_element() < 1e7 && s.is_finite() && s.min_element() >= 0.0 && s.max_element() < 1e5
}

fn decode(r: &[u8], out: &mut Builder) {
    let byte = |i: usize| r[i] as f32 / 255.0;
    let q = |i: usize| (r[i] as f32 - 128.0) / 128.0;
    let color = Vec4::new(byte(24), byte(25), byte(26), byte(27));
    let rotation = Quat::from_xyzw(q(29), q(30), q(31), q(28));
    out.push(floats(r, 0), floats(r, 12), rotation, color, |_, _| 0.0);
}

pub(crate) fn load(path: &Path, size: u64, limits: &Limits) -> Result<Scene, LoadError> {
    load_reader(std::fs::File::open(path)?, size, limits)
}

pub(crate) fn load_reader(reader: impl Read, size: u64, limits: &Limits) -> Result<Scene, LoadError> {
    let count = size / RECORD as u64;
    if count == 0 {
        return Err(LoadError::invalid("too short for a .splat record"));
    }
    let mut info = Info { format: SplatFormat::Splat.name().into(), ..Default::default() };
    if !size.is_multiple_of(RECORD as u64) {
        info.warnings.push(format!("{} bytes after the last whole record ignored", size % RECORD as u64));
    }
    let mut reader = BufReader::with_capacity(1 << 20, reader);
    let mut out = Builder::new(count, limits, 0, true);
    let per_block = (1 << 20) / RECORD;
    let mut block = vec![0u8; per_block * RECORD];
    let mut i = 0u64;
    while i < count {
        let n = (per_block as u64).min(count - i) as usize;
        let buf = &mut block[..n * RECORD];
        if let Err(e) = reader.read_exact(buf) {
            if e.kind() == std::io::ErrorKind::UnexpectedEof {
                break;
            }
            return Err(e.into());
        }
        for (j, r) in buf.chunks_exact(RECORD).enumerate() {
            if out.keeps(i + j as u64) {
                decode(r, &mut out);
            }
        }
        i += n as u64;
    }
    out.finish(info)
}
