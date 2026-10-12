//! Turning a polygon mesh into the scene's triangle primitives: faces
//! triangulated, split by material, with one vertex per distinct
//! combination of position, normal, UVs and colour.

use crate::PolyMesh;
use glam::{Vec2, Vec3, Vec4};
use slerp_scene::{Primitive, Topology};
use std::collections::HashMap;

/// Most corners ear clipping is asked to handle in one face: it takes time
/// by the square of the corners or worse (a hostile face of repeated points
/// takes seconds at a few thousand), and larger faces fall back to a fan.
const MAX_EARCUT: usize = 4096;

/// How much ear clipping one mesh may ask for, as the sum of its faces'
/// corners squared: about a tenth of a second for convex faces, under a
/// second for the worst outlines.  Faces past it fall back to fans, so a
/// mesh of many large n-gons can't hold a thumbnail up.
pub struct EarBudget(u64);

impl Default for EarBudget {
    fn default() -> Self {
        Self::new()
    }
}

impl EarBudget {
    pub fn new() -> Self {
        EarBudget(1 << 27)
    }

    /// Takes the cost of ear clipping `n` corners if it fits.
    fn take(&mut self, n: usize) -> bool {
        let cost = (n as u64).saturating_mul(n as u64);
        let fits = n <= MAX_EARCUT && cost <= self.0;
        if fits {
            self.0 -= cost;
        }
        fits
    }
}

/// Which corner attributes a material's primitive carries.
#[derive(Clone, Debug, Default)]
pub struct Wants {
    /// UV layers for the primitive's first and second UV sets, as indices
    /// into `PolyMesh::uvs`.
    pub uv: [Option<usize>; 2],
    /// The colour layer, an index into `PolyMesh::colors`.
    pub color: Option<usize>,
}

/// The corners of each triangle of face `f`, as offsets into the face.
/// N-gons are ear clipped while `budget` lasts, then fanned.
pub fn face_triangles(poly: &PolyMesh, f: usize, normal: Vec3, out: &mut Vec<[usize; 3]>, budget: &mut EarBudget) {
    let r = poly.face(f);
    let n = r.len();
    let p = |k: usize| poly.positions[poly.corner_verts[r.start + k] as usize];
    match n {
        3 => out.push([0, 1, 2]),
        4 => {
            // Quads split along the first diagonal unless that would put
            // a triangle outside a concave quad.
            let (a, b, c, d) = (p(0), p(1), p(2), p(3));
            let flip = (b - a).cross(c - a).dot((d - a).cross(c - a)) > 0.0;
            if flip {
                out.extend([[1, 2, 3], [1, 3, 0]]);
            } else {
                out.extend([[0, 1, 2], [0, 2, 3]]);
            }
        }
        n if n > 4 => {
            // N-gons: ear clipping in the plane of the face.
            let axis = normal.abs();
            let (i, j) = if axis.x >= axis.y && axis.x >= axis.z {
                (1, 2)
            } else if axis.y >= axis.z {
                (2, 0)
            } else {
                (0, 1)
            };
            // Keep the projected polygon's winding the face's own.
            let flip = normal[3 - i - j] < 0.0;
            let mut coords = Vec::with_capacity(n * 2);
            for k in 0..n {
                let q = p(k);
                coords.push(q[i] as f64);
                coords.push(if flip { -q[j] as f64 } else { q[j] as f64 });
            }
            // earcut asserts on outlines past its index range; a broken
            // face falls back to a fan rather than taking the load down.
            let affordable = budget.take(n);
            let tris: Vec<usize> = std::panic::catch_unwind(|| {
                let mut tris = Vec::new();
                if affordable && coords.iter().all(|c| c.is_finite()) {
                    earcut::Earcut::new().earcut(coords.chunks_exact(2).map(|c| [c[0], c[1]]), &[], &mut tris);
                }
                tris
            })
            .unwrap_or_default();
            if tris.len() / 3 == n - 2 && tris.iter().all(|&t| t < n) {
                for t in tris.chunks_exact(3) {
                    // earcut gives triangles in its own winding; match the
                    // face's.
                    let (a, b, c) = (p(t[0]), p(t[1]), p(t[2]));
                    if (b - a).cross(c - a).dot(normal) < 0.0 {
                        out.push([t[0], t[2], t[1]]);
                    } else {
                        out.push([t[0], t[1], t[2]]);
                    }
                }
            } else {
                // Degenerate outlines: a fan still covers the face.
                out.extend((1..n - 1).map(|k| [0, k, k + 1]));
            }
        }
        _ => {}
    }
}

/// One primitive per material slot used, as (slot, primitive), with
/// `normals` per corner.  `wants` says per slot which layers to carry.
/// A primitive being built, the vertices it has so far by their
/// attributes, and what it carries.
type Building = (Primitive, HashMap<[u32; 12], u32>, Wants);

pub fn primitives(poly: &PolyMesh, normals: &[Vec3], wants: &dyn Fn(usize) -> Wants) -> Vec<(usize, Primitive)> {
    let face_normals = poly.face_normals();
    let mut by_slot: HashMap<usize, Building> = HashMap::new();
    let mut order: Vec<usize> = Vec::new();
    let mut tris = Vec::new();
    let mut budget = EarBudget::new();
    for f in 0..poly.face_count() {
        let slot = poly.face_material.get(f).copied().unwrap_or(0) as usize;
        let (prim, seen, want) = by_slot.entry(slot).or_insert_with(|| {
            order.push(slot);
            (Primitive { topology: Topology::Triangles, ..Default::default() }, HashMap::new(), wants(slot))
        });
        tris.clear();
        face_triangles(poly, f, face_normals[f], &mut tris, &mut budget);
        let start = poly.face_starts[f] as usize;
        for t in &tris {
            for &k in t {
                let c = start + k;
                let v = poly.corner_verts[c];
                let n = normals.get(c).copied().unwrap_or(face_normals[f]);
                let uv = |l: Option<usize>| {
                    l.and_then(|l| poly.uvs.get(l)).and_then(|l| l.values.get(c)).copied().unwrap_or(Vec2::ZERO)
                };
                let (uv0, uv1) = (uv(want.uv[0]), uv(want.uv[1]));
                let col = want
                    .color
                    .and_then(|l| poly.colors.get(l))
                    .and_then(|l| l.values.get(c))
                    .copied()
                    .unwrap_or(Vec4::ONE);
                let key = [
                    v,
                    n.x.to_bits(),
                    n.y.to_bits(),
                    n.z.to_bits(),
                    uv0.x.to_bits(),
                    uv0.y.to_bits(),
                    uv1.x.to_bits(),
                    uv1.y.to_bits(),
                    col.x.to_bits(),
                    col.y.to_bits(),
                    col.z.to_bits(),
                    col.w.to_bits(),
                ];
                let index = *seen.entry(key).or_insert_with(|| {
                    prim.positions.push(poly.positions[v as usize]);
                    prim.normals.push(n);
                    // The tools' UVs start at the bottom left; glTF's, and
                    // the renderer's, at the top left.
                    if want.uv[0].is_some() {
                        prim.uv0.push(Vec2::new(uv0.x, 1.0 - uv0.y));
                    }
                    if want.uv[1].is_some() {
                        prim.uv1.push(Vec2::new(uv1.x, 1.0 - uv1.y));
                    }
                    if want.color.is_some() {
                        prim.colors.push(col);
                    }
                    (prim.positions.len() - 1) as u32
                });
                prim.indices.push(index);
            }
        }
    }
    order
        .into_iter()
        .filter_map(|slot| {
            let (prim, _, _) = by_slot.remove(&slot)?;
            (!prim.indices.is_empty()).then_some((slot, prim))
        })
        .collect()
}

/// Triangulates a flat outline with holes in it (Maya's faces with
/// holes), all given as points around their loops.  Returns triangles as
/// indices into the outline followed by each hole in turn, wound like the
/// outline, or nothing when the outline is degenerate.
pub fn triangulate_with_holes(outline: &[Vec3], holes: &[&[Vec3]]) -> Vec<[usize; 3]> {
    let all: Vec<Vec3> = outline.iter().chain(holes.iter().flat_map(|h| h.iter())).copied().collect();
    let n = all.len();
    if outline.len() < 3 || n > MAX_EARCUT || !all.iter().all(|p| p.is_finite()) {
        return Vec::new();
    }
    // Newell's normal of the outline picks the plane to project onto.
    let mut normal = Vec3::ZERO;
    for (k, &a) in outline.iter().enumerate() {
        let b = outline[(k + 1) % outline.len()];
        normal += Vec3::new((a.y - b.y) * (a.z + b.z), (a.z - b.z) * (a.x + b.x), (a.x - b.x) * (a.y + b.y));
    }
    let axis = normal.abs();
    let (i, j) = if axis.x >= axis.y && axis.x >= axis.z {
        (1, 2)
    } else if axis.y >= axis.z {
        (2, 0)
    } else {
        (0, 1)
    };
    let mut starts = Vec::with_capacity(holes.len());
    let mut at = outline.len();
    for h in holes {
        starts.push(at);
        at += h.len();
    }
    let coords: Vec<[f64; 2]> = all.iter().map(|p| [p[i] as f64, p[j] as f64]).collect();
    let tris: Vec<usize> = std::panic::catch_unwind(|| {
        let mut tris = Vec::new();
        earcut::Earcut::new().earcut(coords.iter().copied(), &starts, &mut tris);
        tris
    })
    .unwrap_or_default();
    tris.chunks_exact(3)
        .filter(|t| t.iter().all(|&k| k < n))
        .map(|t| {
            let (a, b, c) = (all[t[0]], all[t[1]], all[t[2]]);
            if (b - a).cross(c - a).dot(normal) < 0.0 { [t[0], t[2], t[1]] } else { [t[0], t[1], t[2]] }
        })
        .collect()
}
