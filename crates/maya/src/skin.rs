//! Skin clusters evaluated, so a skinned mesh shows the pose its joints
//! were saved in rather than the pose it was bound in.
//!
//! A `skinCluster` deforms its input points by linear blend skinning: each
//! point is moved by each influence's current world matrix times the
//! inverse of its world matrix at binding (`.pm[j]`, "bind pre-matrix"),
//! after the mesh's own matrix at binding (`.gm`, "geometry matrix"), and
//! the results are blended by the point's weights (`.wl[i].w[j]`).  The
//! influences are the joints connected to `.ma[j]` (their world matrices).
//! The result is in world space; the mesh's own world matrix is undone so
//! it can sit under its transform as usual.

use crate::graph::{Graph, Value};
use crate::mesh::MeshData;
use glam::{Mat4, Vec3};
use std::collections::HashMap;

/// Most influences per skin cluster read.
const MAX_INFLUENCES: u32 = 65_536;

/// Poses `data` through skin cluster `skin`.  `world` gives a DAG node's
/// world matrix; `mesh_world` is the deformed mesh's.  Returns false (and
/// leaves `data` alone) when the cluster lacks what it needs.
pub fn apply(
    g: &Graph,
    skin: usize,
    world: &mut dyn FnMut(usize) -> Option<Mat4>,
    mesh_world: Mat4,
    data: &mut MeshData,
) -> bool {
    let node = &g.nodes[skin];
    // 1. Influences and their matrices.
    let mut matrices: HashMap<u32, Mat4> = HashMap::new();
    for c in g.connections.iter().filter(|c| c.dst == skin) {
        let Some(j) = index_of(&c.dst_attr, "ma") else { continue };
        if j >= MAX_INFLUENCES {
            continue;
        }
        let Some(w) = world(c.src) else { continue };
        let pm = node
            .attrs
            .iter()
            .rev()
            .find(|a| a.key == "pm" && a.range == Some((j, j)))
            .and_then(|a| match &a.value {
                Value::Nums(v) if v.len() >= 16 => Some(matrix(v)),
                _ => None,
            })
            .unwrap_or_else(|| w.inverse());
        matrices.insert(j, w * pm);
    }
    if matrices.is_empty() {
        return false;
    }
    let gm = node.nums("gm").filter(|v| v.len() >= 16).map(matrix).unwrap_or(Mat4::IDENTITY);
    // 2. Weights per vertex.
    let weights = weights(g, skin, data.positions.len());
    if weights.iter().all(|w| w.is_empty()) {
        return false;
    }
    let back = mesh_world.inverse();
    if !back.is_finite() {
        return false;
    }
    // 3. Blend.
    for (p, w) in data.positions.iter_mut().zip(&weights) {
        let bound = gm.transform_point3(*p);
        let mut sum = Vec3::ZERO;
        let mut total = 0.0;
        for &(j, weight) in w {
            if let Some(m) = matrices.get(&j) {
                sum += m.transform_point3(bound) * weight;
                total += weight;
            }
        }
        if total > 1e-4 {
            let posed = back.transform_point3(sum / total);
            if posed.is_finite() {
                *p = posed;
            }
        }
    }
    // Normals the file stored belong to the bind pose.
    data.normals.clear();
    true
}

/// Whether a posed mesh still looks like its bind pose stretched by
/// joints, not torn apart: at most one edge in twenty more than two and a
/// half times longer or shorter.  A pose read wrongly (joints driven by
/// something Slerp doesn't evaluate) tears meshes; the bind pose is the
/// better picture then.
pub fn plausible(bind: &MeshData, posed: &MeshData) -> bool {
    let mut bad = 0usize;
    let mut all = 0usize;
    for &[a, b] in &bind.edges {
        let (Some(p0), Some(p1), Some(q0), Some(q1)) = (
            bind.positions.get(a as usize),
            bind.positions.get(b as usize),
            posed.positions.get(a as usize),
            posed.positions.get(b as usize),
        ) else {
            continue;
        };
        let (l0, l1) = ((*p1 - *p0).length(), (*q1 - *q0).length());
        if l0 < 1e-6 {
            continue;
        }
        all += 1;
        let r = l1 / l0;
        if !(0.4..=2.5).contains(&r) {
            bad += 1;
        }
    }
    all == 0 || bad * 20 <= all
}

/// A Maya matrix (16 numbers, row by row, for row vectors) as a glam
/// matrix for column vectors: the same numbers read column by column.
fn matrix(v: &[f64]) -> Mat4 {
    let a: [f32; 16] = std::array::from_fn(|i| v[i] as f32);
    let m = Mat4::from_cols_array(&a);
    if m.is_finite() { m } else { Mat4::IDENTITY }
}

/// The index in "ma[3]" for prefix "ma".
fn index_of(attr: &str, prefix: &str) -> Option<u32> {
    attr.strip_prefix(prefix)?.strip_prefix('[')?.strip_suffix(']')?.parse().ok()
}

/// Each vertex's (influence, weight) pairs, from ASCII's `.wl[i].w[j]`
/// pieces or binary's packed lists.
fn weights(g: &Graph, skin: usize, vertices: usize) -> Vec<Vec<(u32, f32)>> {
    let mut out: Vec<Vec<(u32, f32)>> = vec![Vec::new(); vertices];
    for a in &g.nodes[skin].attrs {
        let Some(rest) = a.key.strip_prefix("wl[") else { continue };
        let Some((index, tail)) = rest.split_once(']') else { continue };
        if tail != ".w" {
            continue;
        }
        let (first, last) = match index.split_once(':') {
            Some((x, y)) => (x.parse::<usize>().ok(), y.parse::<usize>().ok()),
            None => (index.parse::<usize>().ok(), index.parse::<usize>().ok()),
        };
        let (Some(first), Some(_last)) = (first, last) else { continue };
        match &a.value {
            Value::Weights(lists) => {
                for (k, list) in lists.iter().enumerate() {
                    if let Some(slot) = out.get_mut(first + k) {
                        slot.clone_from(list);
                    }
                }
            }
            Value::Nums(v) => {
                let Some(slot) = out.get_mut(first) else { continue };
                let (j0, j1) = a.range.unwrap_or((0, v.len().saturating_sub(1) as u32));
                for (k, &w) in v.iter().enumerate().take((j1.saturating_sub(j0) as usize).saturating_add(1)) {
                    let j = j0 + k as u32;
                    if w.is_finite() && w != 0.0 {
                        slot.retain(|(i, _)| *i != j);
                        slot.push((j, w as f32));
                    }
                }
            }
            _ => {}
        }
    }
    out
}
