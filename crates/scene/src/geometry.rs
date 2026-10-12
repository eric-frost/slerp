//! Generated vertex data for formats that don't store it.

use crate::{Primitive, Topology};
use glam::Vec3;

impl Primitive {
    /// Computes smooth, angle-weighted normals when the primitive has none.
    pub fn ensure_normals(&mut self) {
        if !self.normals.is_empty() || self.topology != Topology::Triangles {
            return;
        }
        let mut normals = vec![Vec3::ZERO; self.positions.len()];
        for tri in self.indices.chunks_exact(3) {
            let [a, b, c] = [tri[0] as usize, tri[1] as usize, tri[2] as usize];
            let (Some(&pa), Some(&pb), Some(&pc)) =
                (self.positions.get(a), self.positions.get(b), self.positions.get(c))
            else {
                continue;
            };
            let n = (pb - pa).cross(pc - pa).normalize_or_zero();
            normals[a] += n * angle(pb - pa, pc - pa);
            normals[b] += n * angle(pc - pb, pa - pb);
            normals[c] += n * angle(pa - pc, pb - pc);
        }
        for n in &mut normals {
            *n = n.try_normalize().unwrap_or(Vec3::Y);
        }
        self.normals = normals;
    }

    /// Gives every triangle its own vertices and a face normal, for formats
    /// meant to look faceted (STL, voxels without normals).
    pub fn make_flat(&mut self) {
        if self.topology != Topology::Triangles {
            return;
        }
        let old = std::mem::take(self);
        let mut out = Primitive { topology: Topology::Triangles, material: old.material, ..Default::default() };
        for tri in old.indices.chunks_exact(3) {
            let get = |i: u32| old.positions.get(i as usize).copied();
            let (Some(a), Some(b), Some(c)) = (get(tri[0]), get(tri[1]), get(tri[2])) else { continue };
            let p = [a, b, c];
            let n = (p[1] - p[0]).cross(p[2] - p[0]).normalize_or_zero();
            for &i in tri {
                let i = i as usize;
                out.indices.push(out.positions.len() as u32);
                out.positions.push(old.positions[i]);
                out.normals.push(n);
                copy(&old.uv0, i, &mut out.uv0);
                copy(&old.uv1, i, &mut out.uv1);
                copy(&old.colors, i, &mut out.colors);
            }
        }
        *self = out;
    }
}

/// Copies attribute `i` when the attribute exists, padding a short one.
fn copy<T: Copy + Default>(src: &[T], i: usize, dst: &mut Vec<T>) {
    if !src.is_empty() {
        dst.push(src.get(i).copied().unwrap_or_default());
    }
}

fn angle(a: Vec3, b: Vec3) -> f32 {
    let d = a.normalize_or_zero().dot(b.normalize_or_zero()).clamp(-1.0, 1.0);
    d.acos()
}
