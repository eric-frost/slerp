//! Polygon meshes as most formats store them, turned into the scene's
//! triangle primitives.  A format fills in shared positions, polygons whose
//! corners index them, and whatever attributes it has, bound per position,
//! per corner or per face, directly or through an index list.  `build`
//! triangulates the polygons, generates missing normals (by crease angle or
//! by 3DS-style smoothing groups), merges corners whose attributes agree into
//! shared vertices, and returns one primitive per material slot.
//!
//! The 3MF, OFF, 3DS, DirectX and VRML/X3D readers all share it, so each of
//! them only has to say what its file holds.

use crate::common::{FastMap, newell_normal, triangulate};
use glam::{Vec2, Vec3, Vec4};
use slerp_scene::{Primitive, Topology};

/// How an attribute's values attach to the mesh.
#[derive(Clone, Debug, Default)]
pub enum Binding {
    /// One value per position, indexed like the positions.
    #[default]
    Vertex,
    /// One value per corner, in corner order.
    Corner,
    /// One index per corner into the values.
    CornerIndexed(Vec<u32>),
    /// One value per face, in face order.
    Face,
    /// One index per face into the values.
    FaceIndexed(Vec<u32>),
}

/// Values of one vertex attribute and how they attach.  A lookup that falls
/// outside the values (a short array, an index of -1) gives the attribute's
/// default for that corner rather than failing the mesh.
#[derive(Clone, Debug, Default)]
pub struct Attr<T> {
    pub values: Vec<T>,
    pub binding: Binding,
}

impl<T: Copy> Attr<T> {
    pub fn new(values: Vec<T>, binding: Binding) -> Self {
        Attr { values, binding }
    }

    fn get(&self, corner: usize, position: u32, face: usize) -> Option<T> {
        let i = match &self.binding {
            Binding::Vertex => position as usize,
            Binding::Corner => corner,
            Binding::CornerIndexed(ix) => *ix.get(corner)? as usize,
            Binding::Face => face,
            Binding::FaceIndexed(ix) => *ix.get(face)? as usize,
        };
        self.values.get(i).copied()
    }
}

/// How normals are generated when the file has none.
#[derive(Clone, Debug)]
pub enum Smoothing {
    /// Faces meeting at less than this angle (radians) share smooth normals
    /// across their edge, as VRML's `creaseAngle` says.  0 is flat, π is
    /// smooth everywhere.
    Crease(f32),
    /// 3DS smoothing groups: a bit mask per face.  Faces that share a bit
    /// share normals; faces with no bits are flat.
    Groups(Vec<u32>),
}

impl Default for Smoothing {
    /// Smooth across shallow edges and sharp across real corners, which
    /// suits both scanned surfaces and CAD parts in formats that don't say.
    fn default() -> Self {
        Smoothing::Crease(40f32.to_radians())
    }
}

/// A polygon mesh waiting to become primitives.
#[derive(Clone, Debug, Default)]
pub struct PolyMesh {
    pub positions: Vec<Vec3>,
    /// Corners per face.
    pub face_sizes: Vec<u32>,
    /// The position index of each corner, faces one after another.
    pub corners: Vec<u32>,
    /// Material slot per face; empty puts every face in slot 0.
    pub face_slots: Vec<u32>,
    pub normals: Option<Attr<Vec3>>,
    pub uvs: Option<Attr<Vec2>>,
    /// Linear RGBA.
    pub colors: Option<Attr<Vec4>>,
    /// Joints and weights per position, for skinned meshes.
    pub skin: Option<(Vec<[u16; 4]>, Vec<Vec4>)>,
    pub smoothing: Smoothing,
}

/// Groups of corners merged per position while generating normals.  More
/// distinct directions at one point than this share the closest group, which
/// bounds the work on adversarial fans.
const MAX_GROUPS: usize = 32;

/// Polygons with more corners than this are dropped: no real file has them,
/// and ear clipping costs the square of the corner count.
const MAX_FACE_CORNERS: usize = 1 << 16;

impl PolyMesh {
    /// Appends a polygon.
    pub fn add_face(&mut self, corners: &[u32]) {
        self.face_sizes.push(corners.len() as u32);
        self.corners.extend_from_slice(corners);
    }

    /// Triangles the faces will make, counting every polygon of n corners as
    /// n - 2, for checking `Limits::max_triangles` before building.
    pub fn triangle_count(&self) -> u64 {
        self.face_sizes.iter().map(|&n| n.saturating_sub(2) as u64).sum()
    }

    /// Reverses every face's winding, for files whose front faces turn
    /// clockwise (VRML's `ccw FALSE`, DirectX's left-handed coordinates).
    /// Per-corner attributes are reversed with them.
    pub fn flip_winding(&mut self) {
        fn flip<T>(v: &mut [T], start: usize, end: usize) {
            let end = end.min(v.len());
            if start < end {
                v[start..end].reverse();
            }
        }
        fn flip_attr<T>(attr: &mut Option<Attr<T>>, start: usize, end: usize) {
            if let Some(a) = attr {
                match &mut a.binding {
                    Binding::Corner => flip(&mut a.values, start, end),
                    Binding::CornerIndexed(ix) => flip(ix, start, end),
                    _ => {}
                }
            }
        }
        let mut start = 0usize;
        for i in 0..self.face_sizes.len() {
            let end = start.saturating_add(self.face_sizes[i] as usize).min(self.corners.len());
            flip(&mut self.corners, start, end);
            flip_attr(&mut self.normals, start, end);
            flip_attr(&mut self.uvs, start, end);
            flip_attr(&mut self.colors, start, end);
            start = end;
        }
    }

    /// Builds the primitives, one per material slot in ascending slot order,
    /// with normals always present.  Faces with fewer than three corners,
    /// corners naming missing positions or positions that aren't finite are
    /// dropped and counted in a warning.
    pub fn build(&self, warnings: &mut Vec<String>) -> Vec<(u32, Primitive)> {
        // 1. Find each valid face's first corner.
        let mut starts: Vec<u32> = Vec::with_capacity(self.face_sizes.len());
        let mut valid: Vec<bool> = Vec::with_capacity(self.face_sizes.len());
        let mut at = 0usize;
        let mut dropped = 0usize;
        for &n in &self.face_sizes {
            let n = n as usize;
            let end = at.saturating_add(n);
            let ok = (3..=MAX_FACE_CORNERS).contains(&n)
                && end <= self.corners.len()
                && self.corners[at..end].iter().all(|&c| self.positions.get(c as usize).is_some_and(|p| p.is_finite()));
            if !ok && n > 0 {
                dropped += 1;
            }
            starts.push(at.min(u32::MAX as usize) as u32);
            valid.push(ok && at <= u32::MAX as usize);
            at = end.min(self.corners.len());
        }
        if dropped > 0 {
            warnings.push(format!("{dropped} broken faces skipped"));
        }

        // 2. Face normals, and per-corner normals from the file or generated.
        let face_normals: Vec<Vec3> = (0..self.face_sizes.len())
            .map(|f| {
                if !valid[f] {
                    return Vec3::ZERO;
                }
                let (s, n) = (starts[f] as usize, self.face_sizes[f] as usize);
                let pts: Vec<Vec3> = self.corners[s..s + n].iter().map(|&c| self.positions[c as usize]).collect();
                newell_normal(&pts).normalize_or_zero()
            })
            .collect();
        let corner_normals = self.corner_normals(&starts, &valid, &face_normals);

        // 3. Faces per slot, so each primitive is built in one pass.
        let mut by_slot: Vec<(u32, u32)> = (0..self.face_sizes.len() as u32)
            .filter(|&f| valid[f as usize])
            .map(|f| (self.face_slots.get(f as usize).copied().unwrap_or(0), f))
            .collect();
        by_slot.sort_unstable();

        // 4. Triangulate and merge corners into vertices.
        let mut out: Vec<(u32, Primitive)> = Vec::new();
        let mut map: FastMap<VertexKey, u32> = FastMap::default();
        let mut tris: Vec<[u32; 3]> = Vec::new();
        let mut pts: Vec<Vec3> = Vec::new();
        let mut face_vertices: Vec<u32> = Vec::new();
        let mut i = 0;
        while i < by_slot.len() {
            let slot = by_slot[i].0;
            let mut prim = Primitive { topology: Topology::Triangles, ..Default::default() };
            map.clear();
            while i < by_slot.len() && by_slot[i].0 == slot {
                let f = by_slot[i].1 as usize;
                i += 1;
                let (s, n) = (starts[f] as usize, self.face_sizes[f] as usize);
                face_vertices.clear();
                for k in 0..n {
                    let corner = s + k;
                    let position = self.corners[corner];
                    let normal = corner_normals[corner];
                    let uv = self.uvs.as_ref().map(|a| a.get(corner, position, f).unwrap_or(Vec2::ZERO));
                    let color = self.colors.as_ref().map(|a| a.get(corner, position, f).unwrap_or(Vec4::ONE));
                    let key = VertexKey {
                        position,
                        normal: normal.to_array().map(f32::to_bits),
                        uv: uv.unwrap_or_default().to_array().map(f32::to_bits),
                        color: color.unwrap_or_default().to_array().map(f32::to_bits),
                    };
                    let next = prim.positions.len() as u32;
                    let v = *map.entry(key).or_insert(next);
                    if v == next {
                        prim.positions.push(self.positions[position as usize]);
                        prim.normals.push(normal);
                        if let Some(uv) = uv {
                            prim.uv0.push(uv);
                        }
                        if let Some(c) = color {
                            prim.colors.push(c);
                        }
                        if let Some((joints, weights)) = &self.skin {
                            prim.joints.push(joints.get(position as usize).copied().unwrap_or_default());
                            prim.weights.push(weights.get(position as usize).copied().unwrap_or(Vec4::X));
                        }
                    }
                    face_vertices.push(v);
                }
                if n == 3 {
                    prim.indices.extend_from_slice(&face_vertices);
                } else {
                    pts.clear();
                    pts.extend(self.corners[s..s + n].iter().map(|&c| self.positions[c as usize]));
                    tris.clear();
                    triangulate(&pts, &mut tris);
                    for t in &tris {
                        prim.indices.extend(t.map(|k| face_vertices[k as usize]));
                    }
                }
            }
            if !prim.indices.is_empty() {
                out.push((slot, prim));
            }
        }
        out
    }

    /// The normal of every corner: the file's own where it has them,
    /// otherwise generated from the faces around each position.
    fn corner_normals(&self, starts: &[u32], valid: &[bool], face_normals: &[Vec3]) -> Vec<Vec3> {
        let mut normals = vec![Vec3::ZERO; self.corners.len()];
        let mut missing = false;
        for f in 0..self.face_sizes.len() {
            if !valid[f] {
                continue;
            }
            let (s, n) = (starts[f] as usize, self.face_sizes[f] as usize);
            for (k, slot) in normals[s..s + n].iter_mut().enumerate() {
                let corner = s + k;
                let given = self.normals.as_ref().and_then(|a| a.get(corner, self.corners[corner], f));
                match given.and_then(|v| v.try_normalize()) {
                    Some(v) => *slot = v,
                    None => missing = true,
                }
            }
        }
        if !missing {
            return normals;
        }

        // 1. Corner weights: the polygon's angle at the corner, so long thin
        //    triangles don't pull a normal off true.
        let mut weights = vec![0f32; self.corners.len()];
        for f in 0..self.face_sizes.len() {
            if !valid[f] {
                continue;
            }
            let (s, n) = (starts[f] as usize, self.face_sizes[f] as usize);
            for k in 0..n {
                let p = self.positions[self.corners[s + k] as usize];
                let a = self.positions[self.corners[s + (k + n - 1) % n] as usize] - p;
                let b = self.positions[self.corners[s + (k + 1) % n] as usize] - p;
                let d = a.normalize_or_zero().dot(b.normalize_or_zero()).clamp(-1.0, 1.0);
                weights[s + k] = d.acos().max(1e-6);
            }
        }

        // 2. Corners per position, as compressed rows.
        let np = self.positions.len();
        let mut row = vec![0u32; np + 1];
        for f in 0..self.face_sizes.len() {
            if valid[f] {
                let (s, n) = (starts[f] as usize, self.face_sizes[f] as usize);
                for &c in &self.corners[s..s + n] {
                    row[c as usize + 1] += 1;
                }
            }
        }
        for i in 0..np {
            row[i + 1] += row[i];
        }
        let mut fill = row.clone();
        let mut members = vec![0u32; row[np] as usize];
        let mut face_of = vec![0u32; self.corners.len()];
        for f in 0..self.face_sizes.len() {
            if valid[f] {
                let (s, n) = (starts[f] as usize, self.face_sizes[f] as usize);
                for (k, &c) in self.corners[s..s + n].iter().enumerate() {
                    let c = c as usize;
                    members[fill[c] as usize] = (s + k) as u32;
                    fill[c] += 1;
                    face_of[s + k] = f as u32;
                }
            }
        }

        // 3. Group the corners at each position and average within groups.
        let fn_of = |corner: u32| face_normals[face_of[corner as usize] as usize];
        let mut groups: Vec<(Vec3, Vec3)> = Vec::with_capacity(MAX_GROUPS);
        let mut assigned: Vec<usize> = Vec::new();
        for p in 0..np {
            let list = &members[row[p] as usize..row[p + 1] as usize];
            if list.is_empty() {
                continue;
            }
            groups.clear();
            assigned.clear();
            match &self.smoothing {
                Smoothing::Crease(angle) => {
                    let cos = angle.clamp(0.0, std::f32::consts::PI).cos() - 1e-5;
                    for &corner in list {
                        let n = fn_of(corner);
                        let w = n * weights[corner as usize];
                        let mut best = None;
                        for (gi, g) in groups.iter().enumerate() {
                            if g.1.dot(n) >= cos {
                                best = Some(gi);
                                break;
                            }
                        }
                        let gi = match best {
                            Some(gi) => gi,
                            None if groups.len() < MAX_GROUPS => {
                                groups.push((Vec3::ZERO, n));
                                groups.len() - 1
                            }
                            None => {
                                let dots = groups.iter().map(|g| g.1.dot(n));
                                dots.enumerate().max_by(|a, b| a.1.total_cmp(&b.1)).map_or(0, |(i, _)| i)
                            }
                        };
                        groups[gi].0 += w;
                        groups[gi].1 = groups[gi].0.try_normalize().unwrap_or(groups[gi].1);
                        assigned.push(gi);
                    }
                    for (&corner, &gi) in list.iter().zip(&assigned) {
                        if normals[corner as usize] == Vec3::ZERO {
                            normals[corner as usize] = groups[gi].0.try_normalize().unwrap_or(fn_of(corner));
                        }
                    }
                }
                Smoothing::Groups(masks) => {
                    let mask_of = |corner: u32| masks.get(face_of[corner as usize] as usize).copied().unwrap_or(0);
                    // Distinct masks at this position, each with the sum of
                    // the faces it smooths with.
                    let mut distinct: Vec<(u32, Vec3)> = Vec::new();
                    for &corner in list {
                        let m = mask_of(corner);
                        if m != 0 && distinct.len() < MAX_GROUPS && !distinct.iter().any(|d| d.0 == m) {
                            distinct.push((m, Vec3::ZERO));
                        }
                    }
                    for &corner in list {
                        let (m, w) = (mask_of(corner), fn_of(corner) * weights[corner as usize]);
                        for d in distinct.iter_mut().filter(|d| d.0 & m != 0) {
                            d.1 += w;
                        }
                    }
                    for &corner in list {
                        if normals[corner as usize] != Vec3::ZERO {
                            continue;
                        }
                        let m = mask_of(corner);
                        let n = distinct.iter().find(|d| d.0 == m).and_then(|d| d.1.try_normalize());
                        normals[corner as usize] = n.unwrap_or(fn_of(corner));
                    }
                }
            }
        }
        for n in &mut normals {
            if *n == Vec3::ZERO {
                *n = Vec3::Y;
            }
        }
        normals
    }
}

/// What makes a corner a vertex of its own: equal keys share one.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct VertexKey {
    position: u32,
    normal: [u32; 3],
    uv: [u32; 2],
    color: [u32; 4],
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cube() -> PolyMesh {
        let mut m = PolyMesh::default();
        for i in 0..8 {
            m.positions.push(Vec3::new((i & 1) as f32, ((i >> 1) & 1) as f32, ((i >> 2) & 1) as f32));
        }
        for f in [[0, 2, 3, 1], [4, 5, 7, 6], [0, 1, 5, 4], [2, 6, 7, 3], [0, 4, 6, 2], [1, 3, 7, 5]] {
            m.add_face(&f);
        }
        m
    }

    #[test]
    fn crease_angle_splits_cube_corners() {
        let mut m = cube();
        m.smoothing = Smoothing::Crease(0.5);
        let prims = m.build(&mut Vec::new());
        assert_eq!(prims.len(), 1);
        let p = &prims[0].1;
        assert_eq!(p.triangle_count(), 12);
        // Flat faces: every corner of each face has its own vertex.
        assert_eq!(p.positions.len(), 24);
        m.smoothing = Smoothing::Crease(std::f32::consts::PI);
        let p = &m.build(&mut Vec::new())[0].1;
        assert_eq!(p.positions.len(), 8);
        let n = p.normals[p.positions.iter().position(|&q| q == Vec3::ONE).unwrap()];
        assert!((n - Vec3::ONE.normalize()).length() < 1e-4, "{n}");
        // Outward normals: the winding above is counter-clockwise from
        // outside.
        let first = p.positions[0];
        assert!(p.normals[0].dot(first - Vec3::splat(0.5)) > 0.0);
    }

    #[test]
    fn smoothing_groups_and_slots() {
        let mut m = cube();
        // Top and one side share group 1; the rest are flat.
        m.smoothing = Smoothing::Groups(vec![0, 0, 0, 1, 0, 1]);
        m.face_slots = vec![0, 0, 1, 1, 1, 1];
        let prims = m.build(&mut Vec::new());
        assert_eq!(prims.iter().map(|p| p.0).collect::<Vec<_>>(), vec![0, 1]);
        assert_eq!(prims[0].1.triangle_count(), 4);
        assert_eq!(prims[1].1.triangle_count(), 8);
    }

    #[test]
    fn bindings_and_broken_faces() {
        let mut m = cube();
        m.colors = Some(Attr::new(vec![Vec4::X, Vec4::Y], Binding::FaceIndexed(vec![0, 1, 0, 1, 0, 9])));
        m.uvs = Some(Attr::new(vec![Vec2::ONE; 3], Binding::Vertex));
        m.add_face(&[0, 1, 99]);
        m.add_face(&[0, 1]);
        let mut warnings = Vec::new();
        let p = &m.build(&mut warnings)[0].1;
        assert_eq!(p.triangle_count(), 12);
        assert_eq!(warnings.len(), 1);
        assert_eq!(p.colors.len(), p.positions.len());
        assert_eq!(p.uv0.len(), p.positions.len());
        // The out-of-range colour index falls back to white.
        assert!(p.colors.contains(&Vec4::ONE));
        m.flip_winding();
        let q = &m.build(&mut Vec::new())[0].1;
        assert!(q.normals[0].dot(q.positions[0] - Vec3::splat(0.5)) < 0.0);
    }
}
