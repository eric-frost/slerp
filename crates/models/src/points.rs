//! Point clouds, shared by the LAS/LAZ, E57, PCD, PTS/PTX and PLY readers.
//!
//! Scans are large: aerial lidar tiles hold tens of millions of points and
//! terrestrial scans hundreds of millions, often in georeferenced
//! coordinates millions of metres from the origin.  `Cloud` takes points as
//! a reader decodes them and:
//!
//! - keeps an even sample when there are more than the budget allows
//!   (twice `Limits::max_splats`, so two million for thumbnails, at most
//!   `VIEWER_POINTS` for the viewer), asking before each point so skipped
//!   points needn't even be decoded;
//! - takes positions in `f64` and stores them relative to the first point,
//!   then recentres the whole cloud on its middle, so `f32` keeps millimetre
//!   precision; the offset is reported in `Info::details`;
//! - colours points by their RGB, or by intensity mapped to grey when they
//!   have none, stretched between the 1st and 99th percentiles because
//!   scanners use wildly different intensity ranges.
//!
//! The result is one `Topology::Points` primitive with an unlit material,
//! since points have no normals to light.

use crate::common::srgb_to_linear;
use glam::{DVec3, Vec3, Vec4};
use slerp_scene::{Aabb, Limits, LoadError, Material, Mesh, Node, Primitive, Scene, SceneKind, Topology};

/// The most points the viewer keeps: 32 bytes each in memory, so about a
/// gigabyte.
pub const VIEWER_POINTS: u64 = 30_000_000;

/// Clouds wider than this (metres, horizontally) are places, opened in the
/// first-person camera and framed from above.
const LEVEL_EXTENT: f32 = 50.0;

/// The points `limits` allows.
pub fn point_budget(limits: &Limits) -> u64 {
    limits.max_splats.saturating_mul(2).clamp(1, VIEWER_POINTS)
}

pub struct Cloud {
    origin: Option<DVec3>,
    positions: Vec<Vec3>,
    /// Raw 16-bit RGB per kept point, when any point has colour.
    colors: Vec<[u16; 3]>,
    /// Raw intensity per kept point, when any point has one.
    intensities: Vec<u16>,
    cap: usize,
    /// Every `stride`-th point offered is kept.
    stride: u64,
    offered: u64,
    /// Points rejected for having no position (no return, NaN).
    invalid: u64,
}

impl Cloud {
    /// A cloud expecting about `expected` points, when the file says.
    pub fn new(expected: Option<u64>, limits: &Limits) -> Self {
        let cap = point_budget(limits);
        let stride = expected.map_or(1, |n| n.div_ceil(cap).max(1));
        let reserve = expected.map_or(0, |n| n.div_ceil(stride)).min(cap).min(1 << 22) as usize;
        Cloud {
            origin: None,
            positions: Vec::with_capacity(reserve),
            colors: Vec::new(),
            intensities: Vec::new(),
            cap: cap as usize,
            stride,
            offered: 0,
            invalid: 0,
        }
    }

    /// Counts the next point and says whether it will be kept, so readers
    /// can skip decoding the rest.  Call `push` after a true answer.
    #[inline]
    pub fn wants(&mut self) -> bool {
        let keep = self.offered.is_multiple_of(self.stride);
        self.offered += 1;
        keep
    }

    /// Adds a point `wants` said to keep.  `color` is 16-bit RGB (8-bit
    /// values may be passed as they are: a cloud whose colours never pass
    /// 255 is read as 8-bit).
    pub fn push(&mut self, p: DVec3, color: Option<[u16; 3]>, intensity: Option<u16>) {
        if !p.is_finite() {
            self.invalid += 1;
            return;
        }
        let origin = *self.origin.get_or_insert(p);
        let n = self.positions.len();
        self.positions.push((p - origin).as_vec3());
        if let Some(c) = color {
            if self.colors.len() < n {
                self.colors.resize(n, [u16::MAX; 3]);
            }
            self.colors.push(c);
        } else if !self.colors.is_empty() {
            self.colors.push([u16::MAX; 3]);
        }
        if let Some(i) = intensity {
            if self.intensities.len() < n {
                self.intensities.resize(n, 0);
            }
            self.intensities.push(i);
        } else if !self.intensities.is_empty() {
            self.intensities.push(0);
        }
        if self.positions.len() > self.cap {
            self.halve();
        }
    }

    /// Drops every other kept point and doubles the stride, for clouds that
    /// turn out larger than expected.
    fn halve(&mut self) {
        let n = self.positions.len();
        thin(&mut self.positions, n, 2);
        thin(&mut self.colors, n, 2);
        thin(&mut self.intensities, n, 2);
        self.stride = self.stride.saturating_mul(2);
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.positions.len()
    }

    /// Builds the scene.  `z_up` turns the cloud to Y-up.
    pub fn finish(self, format: &str, z_up: bool) -> Result<Scene, LoadError> {
        let Cloud { origin, mut positions, colors, intensities, stride, offered, invalid, .. } = self;
        let n = positions.len();
        if n == 0 {
            return Err(LoadError::invalid(format!("{format} holds no points")));
        }
        // 1. Recentre on the middle of the cloud.
        let mut bounds = Aabb::EMPTY;
        positions.iter().for_each(|&p| bounds.add(p));
        let centre = bounds.center();
        positions.iter_mut().for_each(|p| *p -= centre);
        let origin = origin.unwrap_or(DVec3::ZERO) + centre.as_dvec3();

        // 2. Colours: RGB, or grey from intensity.
        let colors: Vec<Vec4> = if colors.len() == n {
            let max = colors.iter().flat_map(|c| c.iter()).copied().max().unwrap_or(0);
            let scale = if max <= 255 { 1.0 / 255.0 } else { 1.0 / 65535.0 };
            let table: Vec<f32> =
                if max <= 255 { (0..=255).map(|v| srgb_to_linear(v as f32 / 255.0)).collect() } else { Vec::new() };
            colors
                .iter()
                .map(|c| {
                    let ch =
                        |v: u16| if table.is_empty() { srgb_to_linear(v as f32 * scale) } else { table[v as usize] };
                    Vec4::new(ch(c[0]), ch(c[1]), ch(c[2]), 1.0)
                })
                .collect()
        } else if intensities.len() == n {
            intensity_greys(&intensities)
        } else {
            Vec::new()
        };

        // 3. One primitive, one unlit material.
        let mut scene = Scene::default();
        let size = bounds.size();
        let horizontal = if z_up { size.x.max(size.y) } else { size.x.max(size.z) };
        scene.kind = if horizontal > LEVEL_EXTENT { SceneKind::Level } else { SceneKind::Object };
        scene.materials.push(Material {
            name: "points".into(),
            base_color: if colors.is_empty() { Vec4::new(0.8, 0.8, 0.8, 1.0) } else { Vec4::ONE },
            unlit: true,
            double_sided: true,
            ..Material::default()
        });
        let prim = Primitive {
            positions,
            colors,
            indices: (0..n as u32).collect(),
            topology: Topology::Points,
            material: Some(0),
            ..Default::default()
        };
        scene.meshes.push(Mesh { name: "points".into(), primitives: vec![prim], target_names: Vec::new() });
        let mut node = Node::new("points");
        node.mesh = Some(0);
        scene.nodes.push(node);
        scene.roots.push(0);
        if z_up {
            scene.convert_z_up();
        }
        scene.info.format = format.to_string();
        describe(&mut scene, n as u64, offered, stride, origin, invalid);
        Ok(scene)
    }
}

/// Facts every point cloud reports.
fn describe(scene: &mut Scene, kept: u64, offered: u64, stride: u64, origin: DVec3, invalid: u64) {
    let d = &mut scene.info.details;
    d.push(("Points".into(), offered.saturating_sub(invalid).to_string()));
    if stride > 1 {
        d.push(("Points shown".into(), format!("{kept} (every {stride}th)")));
    }
    if invalid > 0 {
        d.push(("Points without a position".into(), invalid.to_string()));
    }
    if origin.abs().max_element() > 1.0 {
        d.push(("Origin offset".into(), format!("{:.3}, {:.3}, {:.3}", origin.x, origin.y, origin.z)));
    }
}

/// Intensities as linear greys, stretched between the 1st and 99th
/// percentiles of a sample.
fn intensity_greys(values: &[u16]) -> Vec<Vec4> {
    let step = values.len().div_ceil(65_536).max(1);
    let mut sample: Vec<u16> = values.iter().step_by(step).copied().collect();
    sample.sort_unstable();
    let lo = sample[sample.len() / 100] as f32;
    let hi = sample[(sample.len() * 99 / 100).min(sample.len() - 1)] as f32;
    if hi <= lo {
        return Vec::new();
    }
    let range = hi - lo;
    values
        .iter()
        .map(|&v| {
            let g = srgb_to_linear(((v as f32 - lo) / range).clamp(0.0, 1.0) * 0.9 + 0.1);
            Vec4::new(g, g, g, 1.0)
        })
        .collect()
}

/// Keeps every `stride`-th entry of an attribute that has one per point.
fn thin<T>(v: &mut Vec<T>, n: usize, stride: usize) {
    if v.len() == n {
        let mut i = 0usize;
        v.retain(|_| {
            i += 1;
            (i - 1).is_multiple_of(stride)
        });
    }
}

/// Thins, recentres and classifies a point primitive another loader built
/// (PLY point clouds).
pub fn tidy_primitive(scene: &mut Scene, mesh: usize, limits: &Limits) {
    let Some(prim) = scene.meshes.get_mut(mesh).and_then(|m| m.primitives.first_mut()) else { return };
    let n = prim.positions.len();
    let cap = point_budget(limits) as usize;
    if n > cap {
        let stride = n.div_ceil(cap);
        thin(&mut prim.positions, n, stride);
        thin(&mut prim.colors, n, stride);
        thin(&mut prim.normals, n, stride);
        thin(&mut prim.uv0, n, stride);
        prim.indices = (0..prim.positions.len() as u32).collect();
        scene.info.details.push(("Points shown".into(), format!("{} (every {stride}th)", prim.positions.len())));
    }
    let mut bounds = Aabb::EMPTY;
    prim.positions.iter().filter(|p| p.is_finite()).for_each(|&p| bounds.add(p));
    if bounds.is_empty() {
        return;
    }
    let centre = bounds.center();
    if centre.abs().max_element() > 1000.0 {
        prim.positions.iter_mut().for_each(|p| *p -= centre);
        scene.info.details.push(("Origin offset".into(), format!("{:.3}, {:.3}, {:.3}", centre.x, centre.y, centre.z)));
    }
    let size = bounds.size();
    if size.x.max(size.z) > LEVEL_EXTENT {
        scene.kind = SceneKind::Level;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thins_recentres_and_colours() {
        let limits = Limits { max_splats: 50, ..Limits::viewer() };
        // Unknown count: the cloud halves itself as it grows.
        let mut c = Cloud::new(None, &limits);
        for i in 0..1000 {
            if c.wants() {
                c.push(DVec3::new(5e6 + i as f64, 2e6, 100.0), None, Some(i as u16));
            }
        }
        assert!(c.len() <= 100 && c.len() >= 50, "{}", c.len());
        let s = c.finish("test", true).unwrap();
        let p = &s.meshes[0].primitives[0];
        assert_eq!(p.colors.len(), p.positions.len());
        assert!(p.positions.iter().all(|q| q.length() < 600.0));
        assert!(s.info.details.iter().any(|d| d.0 == "Origin offset"));
        assert_eq!(s.kind, SceneKind::Level);
        // Known count: every 20th of 1000 points under a 100-point budget.
        let mut c = Cloud::new(Some(1000), &limits);
        for i in 0..1000 {
            if c.wants() {
                c.push(DVec3::new(i as f64 * 0.001, 0.0, 0.0), Some([255, 0, 0]), None);
            }
        }
        assert_eq!(c.len(), 100);
        let s = c.finish("test", false).unwrap();
        assert_eq!(s.meshes[0].primitives[0].colors[0], Vec4::new(1.0, 0.0, 0.0, 1.0));
        assert_eq!(s.kind, SceneKind::Object);
        assert!(Cloud::new(Some(0), &limits).finish("empty", false).is_err());
    }
}
