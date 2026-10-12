//! Polygon meshes as modelling tools keep them, for the loaders of their
//! files (Maya, 3ds Max): positions, faces as runs of corners, and
//! per-corner UVs, colours and normals.  A mesh stays in polygons while
//! loaders work on it (subdivision needs the quads), and turns into the
//! scene's triangle primitives only at the end, through [`primitives`].
//!
//! Shading follows one of the tools' two ways of saying which faces blend
//! smoothly into each other: edges marked hard (Maya), or smoothing groups,
//! bit masks per face where faces sharing a bit blend at shared vertices
//! (3ds Max).  [`corner_normals`] turns either into a normal per corner.
//!
//! [`textures`] finds and decodes the pictures such scenes name by paths
//! from their author's machine.
//!
//! The structure, the triangulation and the subdivision follow the Blender
//! loader's (`crates/blend`), which works on meshes of the same shape.

// Faces, corners and vertices are numbered across several parallel lists,
// so loops over indices read most plainly.
#![allow(clippy::needless_range_loop)]

mod normals;
mod subdivide;
pub mod textures;
mod triangulate;

pub use normals::corner_normals;
pub use subdivide::{Options as SubdivideOptions, subdivide};
pub use triangulate::{EarBudget, Wants, face_triangles, primitives, triangulate_with_holes};

use glam::{Vec2, Vec3, Vec4};
use std::collections::HashSet;

/// A corner attribute layer with its name, such as a UV set "map1".
#[derive(Clone, Debug, Default)]
pub struct Layer<T> {
    pub name: String,
    pub values: Vec<T>,
}

/// How faces blend into their neighbours.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum Smoothing {
    /// Every face is flat.
    #[default]
    Flat,
    /// Faces blend across every edge except those in `PolyMesh::hard_edges`
    /// and open or non-manifold ones (Maya).
    HardEdges,
    /// Faces sharing a bit of their `PolyMesh::face_groups` mask blend at
    /// the vertices they share; a face with no bits is flat (3ds Max).
    Groups,
}

#[derive(Clone, Debug, Default)]
pub struct PolyMesh {
    pub positions: Vec<Vec3>,
    /// Where each face's corners start in `corner_verts`, plus one entry
    /// for the end: faces + 1 entries.
    pub face_starts: Vec<u32>,
    /// The vertex of each face corner.
    pub corner_verts: Vec<u32>,
    /// The material of each face, as the format numbers it.
    pub face_material: Vec<u32>,
    pub smoothing: Smoothing,
    /// Hard edges, as vertex pairs with the smaller index first.
    pub hard_edges: HashSet<(u32, u32)>,
    /// Smoothing groups of each face (for `Smoothing::Groups`).
    pub face_groups: Vec<u32>,
    /// UV sets, one value per corner, with v pointing up as the tools
    /// store it.
    pub uvs: Vec<Layer<Vec2>>,
    /// Colour sets, linear RGBA, one value per corner.
    pub colors: Vec<Layer<Vec4>>,
    /// Normals the file stores, one per corner; zero vectors where it
    /// stores none, which get the computed normal.
    pub custom_normals: Option<Vec<Vec3>>,
}

impl PolyMesh {
    pub fn new() -> Self {
        PolyMesh { face_starts: vec![0], ..Default::default() }
    }

    pub fn face_count(&self) -> usize {
        self.face_starts.len().saturating_sub(1)
    }

    /// The corner range of face `f`.
    #[inline]
    pub fn face(&self, f: usize) -> std::ops::Range<usize> {
        self.face_starts[f] as usize..self.face_starts[f + 1] as usize
    }

    /// Adds a face over `verts`, with its material and smoothing groups.
    pub fn push_face(&mut self, verts: &[u32], material: u32, groups: u32) {
        self.corner_verts.extend_from_slice(verts);
        self.face_starts.push(self.corner_verts.len() as u32);
        self.face_material.push(material);
        self.face_groups.push(groups);
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
    /// and padding or trimming per-face and per-corner layers.  Returns
    /// how many faces were dropped.
    pub fn validate(&mut self) -> usize {
        let nv = self.positions.len() as u32;
        let faces = self.face_count();
        let corners = self.corner_verts.len();
        let mut keep = Vec::with_capacity(faces);
        let mut dropped = 0;
        for f in 0..faces {
            let (s, e) = (self.face_starts[f], self.face_starts[f + 1]);
            let good = s <= e
                && (e as usize) <= corners
                && e - s >= 3
                && self.corner_verts[s as usize..e as usize].iter().all(|&v| v < nv);
            dropped += usize::from(!good);
            keep.push(good);
        }
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
        self.face_groups.resize(faces, 0);
        if dropped > 0 {
            self.keep_faces(&keep);
        }
        dropped
    }

    /// Keeps only the faces marked true, with their corners.
    pub fn keep_faces(&mut self, keep: &[bool]) {
        let mut out = self.empty_like();
        for (f, &k) in keep.iter().enumerate() {
            if !k || f >= self.face_count() {
                continue;
            }
            let (s, e) = (self.face_starts[f] as usize, self.face_starts[f + 1] as usize);
            let (s, e) = (s.min(self.corner_verts.len()), e.min(self.corner_verts.len()));
            if s >= e {
                continue;
            }
            out.corner_verts.extend_from_slice(&self.corner_verts[s..e]);
            for (o, l) in out.uvs.iter_mut().zip(&self.uvs) {
                o.values.extend_from_slice(l.values.get(s..e).unwrap_or(&[]));
            }
            for (o, l) in out.colors.iter_mut().zip(&self.colors) {
                o.values.extend_from_slice(l.values.get(s..e).unwrap_or(&[]));
            }
            if let (Some(o), Some(n)) = (&mut out.custom_normals, &self.custom_normals) {
                o.extend_from_slice(n.get(s..e).unwrap_or(&[]));
            }
            out.face_material.push(self.face_material.get(f).copied().unwrap_or(0));
            out.face_groups.push(self.face_groups.get(f).copied().unwrap_or(0));
            out.face_starts.push(out.corner_verts.len() as u32);
        }
        out.positions = std::mem::take(&mut self.positions);
        out.hard_edges = std::mem::take(&mut self.hard_edges);
        *self = out;
    }

    /// An empty mesh with the same layers and smoothing, for building
    /// copies.
    pub fn empty_like(&self) -> PolyMesh {
        PolyMesh {
            face_starts: vec![0],
            smoothing: self.smoothing.clone(),
            uvs: self.uvs.iter().map(|l| Layer { name: l.name.clone(), values: Vec::new() }).collect(),
            colors: self.colors.iter().map(|l| Layer { name: l.name.clone(), values: Vec::new() }).collect(),
            custom_normals: self.custom_normals.as_ref().map(|_| Vec::new()),
            ..Default::default()
        }
    }

    /// Moves, turns and scales every position (and stored normal) by `m`,
    /// reversing the faces when `m` mirrors, so they keep facing out.
    pub fn transform(&mut self, m: glam::Mat4) {
        for p in &mut self.positions {
            *p = m.transform_point3(*p);
        }
        let normal_matrix = m.inverse().transpose();
        if let Some(n) = &mut self.custom_normals {
            for v in n.iter_mut() {
                if *v != Vec3::ZERO {
                    *v = normal_matrix.transform_vector3(*v).normalize_or_zero();
                }
            }
        }
        if m.determinant() < 0.0 {
            self.flip_faces();
        }
    }

    /// Reverses the winding of every face, keeping each face's first
    /// corner first.
    pub fn flip_faces(&mut self) {
        for f in 0..self.face_count() {
            let r = self.face(f);
            let s = r.start + 1;
            self.corner_verts[s..r.end].reverse();
            for l in &mut self.uvs {
                if let Some(v) = l.values.get_mut(s..r.end) {
                    v.reverse();
                }
            }
            for l in &mut self.colors {
                if let Some(v) = l.values.get_mut(s..r.end) {
                    v.reverse();
                }
            }
            if let Some(n) = &mut self.custom_normals
                && let Some(v) = n.get_mut(s..r.end)
            {
                v.reverse();
            }
        }
    }
}

/// An edge's key: its vertices, smaller first.
#[inline]
pub fn edge_key(a: u32, b: u32) -> (u32, u32) {
    if a < b { (a, b) } else { (b, a) }
}

#[cfg(test)]
mod tests;
