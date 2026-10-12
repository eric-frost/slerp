//! The polygon mesh that reading, modifiers and normals work on: positions,
//! faces as runs of corners, and per-corner attributes, as Blender keeps a
//! mesh.  It turns into triangle primitives only at the end, so modifiers
//! see the quads and n-gons they were written for.

use glam::{Vec2, Vec3, Vec4};
use std::collections::{HashMap, HashSet};

/// A corner attribute layer with its name, such as a UV map "UVMap".
#[derive(Clone, Debug, Default)]
pub struct Layer<T> {
    pub name: String,
    pub values: Vec<T>,
}

#[derive(Clone, Debug, Default)]
pub struct PolyMesh {
    pub positions: Vec<Vec3>,
    /// Where each face's corners start in `corner_verts`, plus one entry
    /// for the end: faces + 1 entries.
    pub face_starts: Vec<u32>,
    /// The vertex of each face corner.
    pub corner_verts: Vec<u32>,
    /// The material slot of each face.
    pub face_material: Vec<u16>,
    /// Smooth (true) or flat shading of each face.
    pub face_smooth: Vec<bool>,
    /// Edges marked sharp, as vertex pairs with the smaller index first.
    pub sharp_edges: HashSet<(u32, u32)>,
    /// Subdivision creases from 0 to 1, by vertex pair.
    pub creases: HashMap<(u32, u32), f32>,
    /// UV maps, one value per corner.
    pub uvs: Vec<Layer<Vec2>>,
    /// The UV map renders use, an index into `uvs`.
    pub default_uv: usize,
    /// Colour attributes, linear RGBA, one value per corner.
    pub colors: Vec<Layer<Vec4>>,
    /// The colour attribute renders use, an index into `colors`.
    pub default_color: usize,
    /// Custom normals, one per corner, already decoded to vectors.
    pub custom_normals: Option<Vec<Vec3>>,
}

impl PolyMesh {
    pub fn face_count(&self) -> usize {
        self.face_starts.len().saturating_sub(1)
    }

    /// The corner range of face `f`.
    #[inline]
    pub fn face(&self, f: usize) -> std::ops::Range<usize> {
        self.face_starts[f] as usize..self.face_starts[f + 1] as usize
    }

    /// Triangles once triangulated: two fewer than corners, per face.
    pub fn triangle_count(&self) -> usize {
        (0..self.face_count()).map(|f| self.face(f).len().saturating_sub(2)).sum()
    }

    /// The normal of each face by Newell's method, which copes with
    /// n-gons that aren't quite flat.
    pub fn face_normals(&self) -> Vec<Vec3> {
        (0..self.face_count())
            .map(|f| {
                let r = self.face(f);
                let mut n = Vec3::ZERO;
                for c in r.clone() {
                    let next = if c + 1 == r.end { r.start } else { c + 1 };
                    let a = self.positions[self.corner_verts[c] as usize];
                    let b = self.positions[self.corner_verts[next] as usize];
                    n += Vec3::new((a.y - b.y) * (a.z + b.z), (a.z - b.z) * (a.x + b.x), (a.x - b.x) * (a.y + b.y));
                }
                n.normalize_or_zero()
            })
            .collect()
    }

    /// Checks the invariants every later stage relies on, dropping faces
    /// that break them (fewer than three corners, vertices out of range)
    /// and padding or trimming attribute layers to the corner count.
    pub fn validate(&mut self) {
        let nv = self.positions.len() as u32;
        let faces = self.face_count();
        let mut keep_faces = Vec::with_capacity(faces);
        let mut ok = true;
        for f in 0..faces {
            let (s, e) = (self.face_starts[f], self.face_starts[f + 1]);
            let good = s <= e
                && (e as usize) <= self.corner_verts.len()
                && e - s >= 3
                && self.corner_verts[s as usize..e as usize].iter().all(|&v| v < nv);
            ok &= good;
            keep_faces.push(good);
        }
        let corners = self.corner_verts.len();
        for l in &mut self.uvs {
            l.values.resize(corners, Vec2::ZERO);
        }
        for l in &mut self.colors {
            l.values.resize(corners, Vec4::ONE);
        }
        if let Some(n) = &mut self.custom_normals {
            n.resize(corners, Vec3::ZERO);
        }
        self.face_material.resize(faces, 0);
        self.face_smooth.resize(faces, false);
        if !ok {
            self.keep_faces(&keep_faces);
        }
        self.default_uv = self.default_uv.min(self.uvs.len().saturating_sub(1));
        self.default_color = self.default_color.min(self.colors.len().saturating_sub(1));
    }

    /// Keeps only the faces marked true, with their corners.
    pub fn keep_faces(&mut self, keep: &[bool]) {
        let mut starts = vec![0u32];
        let mut corners = Vec::new();
        let mut materials = Vec::new();
        let mut smooth = Vec::new();
        let mut uvs: Vec<Vec<Vec2>> = vec![Vec::new(); self.uvs.len()];
        let mut colors: Vec<Vec<Vec4>> = vec![Vec::new(); self.colors.len()];
        let mut normals = self.custom_normals.as_ref().map(|_| Vec::new());
        for (f, &k) in keep.iter().enumerate() {
            if !k {
                continue;
            }
            let (s, e) = (self.face_starts[f] as usize, self.face_starts[f + 1] as usize);
            let (s, e) = (s.min(self.corner_verts.len()), e.min(self.corner_verts.len()));
            if s >= e {
                continue;
            }
            corners.extend_from_slice(&self.corner_verts[s..e]);
            for (out, l) in uvs.iter_mut().zip(&self.uvs) {
                out.extend_from_slice(&l.values[s..e]);
            }
            for (out, l) in colors.iter_mut().zip(&self.colors) {
                out.extend_from_slice(&l.values[s..e]);
            }
            if let (Some(out), Some(n)) = (&mut normals, &self.custom_normals) {
                out.extend_from_slice(&n[s..e]);
            }
            materials.push(self.face_material.get(f).copied().unwrap_or(0));
            smooth.push(self.face_smooth.get(f).copied().unwrap_or(false));
            starts.push(corners.len() as u32);
        }
        self.face_starts = starts;
        self.corner_verts = corners;
        self.face_material = materials;
        self.face_smooth = smooth;
        for (l, v) in self.uvs.iter_mut().zip(uvs) {
            l.values = v;
        }
        for (l, v) in self.colors.iter_mut().zip(colors) {
            l.values = v;
        }
        self.custom_normals = normals;
    }

    /// Appends `other`, offsetting its vertices.  Both must have the same
    /// attribute layers (as copies of one mesh do).
    pub fn append(&mut self, other: &PolyMesh) {
        let v0 = self.positions.len() as u32;
        let c0 = self.corner_verts.len() as u32;
        self.positions.extend_from_slice(&other.positions);
        self.corner_verts.extend(other.corner_verts.iter().map(|&v| v + v0));
        self.face_starts.extend(other.face_starts.iter().skip(1).map(|&s| s + c0));
        self.face_material.extend_from_slice(&other.face_material);
        self.face_smooth.extend_from_slice(&other.face_smooth);
        self.sharp_edges.extend(other.sharp_edges.iter().map(|&(a, b)| (a + v0, b + v0)));
        self.creases.extend(other.creases.iter().map(|(&(a, b), &w)| ((a + v0, b + v0), w)));
        for (l, o) in self.uvs.iter_mut().zip(&other.uvs) {
            l.values.extend_from_slice(&o.values);
        }
        for (l, o) in self.colors.iter_mut().zip(&other.colors) {
            l.values.extend_from_slice(&o.values);
        }
        match (&mut self.custom_normals, &other.custom_normals) {
            (Some(a), Some(b)) => a.extend_from_slice(b),
            (Some(a), None) => a.resize(self.corner_verts.len(), Vec3::ZERO),
            _ => {}
        }
    }

    /// An empty mesh with the same attribute layers, for building copies.
    pub fn empty_like(&self) -> PolyMesh {
        PolyMesh {
            face_starts: vec![0],
            uvs: self.uvs.iter().map(|l| Layer { name: l.name.clone(), values: Vec::new() }).collect(),
            default_uv: self.default_uv,
            colors: self.colors.iter().map(|l| Layer { name: l.name.clone(), values: Vec::new() }).collect(),
            default_color: self.default_color,
            custom_normals: self.custom_normals.as_ref().map(|_| Vec::new()),
            ..Default::default()
        }
    }

    /// Reverses the winding of every face, keeping each face's first
    /// corner first, as Blender's flip does.  Custom normals turn round too
    /// when `negate` is set (a flipped shell), but not for a mirrored copy,
    /// whose normals the mirror already reflected.
    pub fn flip_faces(&mut self, negate: bool) {
        for f in 0..self.face_count() {
            let r = self.face(f);
            let s = r.start + 1;
            self.corner_verts[s..r.end].reverse();
            for l in &mut self.uvs {
                l.values[s..r.end].reverse();
            }
            for l in &mut self.colors {
                l.values[s..r.end].reverse();
            }
            if let Some(n) = &mut self.custom_normals {
                n[s..r.end].reverse();
                if negate {
                    for v in &mut n[r.clone()] {
                        *v = -*v;
                    }
                }
            }
        }
    }
}

/// An edge's key: its vertices, smaller first.
#[inline]
pub fn edge_key(a: u32, b: u32) -> (u32, u32) {
    if a < b { (a, b) } else { (b, a) }
}
