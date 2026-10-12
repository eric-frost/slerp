//! Bounding boxes of posed scenes, for framing cameras.

use crate::{Pose, Scene};
use glam::{Mat4, Vec3};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Aabb {
    pub min: Vec3,
    pub max: Vec3,
}

impl Aabb {
    pub const EMPTY: Aabb = Aabb { min: Vec3::splat(f32::INFINITY), max: Vec3::splat(f32::NEG_INFINITY) };

    pub fn is_empty(&self) -> bool {
        self.min.x > self.max.x
    }

    pub fn add(&mut self, p: Vec3) {
        if p.is_finite() {
            self.min = self.min.min(p);
            self.max = self.max.max(p);
        }
    }

    pub fn union(&mut self, other: &Aabb) {
        if !other.is_empty() {
            self.min = self.min.min(other.min);
            self.max = self.max.max(other.max);
        }
    }

    pub fn center(&self) -> Vec3 {
        (self.min + self.max) * 0.5
    }

    pub fn size(&self) -> Vec3 {
        self.max - self.min
    }

    /// Radius of the bounding sphere around the box.
    pub fn radius(&self) -> f32 {
        self.size().length() * 0.5
    }
}

impl Scene {
    /// World-space bounds of every visible mesh in `pose`, with skinned
    /// meshes skinned on the CPU, and of the dense part of any Gaussian
    /// splats (`Splats::dense_bounds`).  Morph targets are ignored.
    pub fn bounds(&self, pose: &Pose) -> Aabb {
        let mut out = Aabb::EMPTY;
        for (ni, node) in self.nodes.iter().enumerate() {
            let (Some(mi), true) = (node.mesh, node.visible) else { continue };
            let Some(mesh) = self.meshes.get(mi) else { continue };
            let joints = node.skin.filter(|&s| s < self.skins.len()).map(|s| pose.joint_matrices(self, s));
            for prim in &mesh.primitives {
                match (&joints, prim.is_skinned()) {
                    (Some(joints), true) => {
                        for (i, &p) in prim.positions.iter().enumerate() {
                            let (j, w) = (prim.joints[i], prim.weights[i]);
                            let mut m = Mat4::ZERO;
                            for k in 0..4 {
                                if w[k] != 0.0 {
                                    m += joints.get(j[k] as usize).copied().unwrap_or(Mat4::IDENTITY) * w[k];
                                }
                            }
                            out.add(m.transform_point3(p));
                        }
                    }
                    _ => {
                        let m = pose.world[ni];
                        // Transform the primitive's own box rather than
                        // every vertex: cheap and only slightly loose.
                        let mut local = Aabb::EMPTY;
                        prim.positions.iter().for_each(|&p| local.add(p));
                        if local.is_empty() {
                            continue;
                        }
                        for c in 0..8 {
                            let corner = Vec3::new(
                                if c & 1 == 0 { local.min.x } else { local.max.x },
                                if c & 2 == 0 { local.min.y } else { local.max.y },
                                if c & 4 == 0 { local.min.z } else { local.max.z },
                            );
                            out.add(m.transform_point3(corner));
                        }
                    }
                }
            }
        }
        if let Some(splats) = &self.splats {
            out.union(&splats.dense_bounds());
        }
        out
    }
}
