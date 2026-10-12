//! Meshes: vertex attributes, indices, topology and morph targets.

use crate::Ctx;
use crate::accessor::Accessors;
use crate::json::{MeshJ, PrimitiveJ, Root};
use rayon::prelude::*;
use slerp_scene::glam::{Vec2, Vec3, Vec4};
use slerp_scene::{LoadError, Mesh, MorphTarget, Primitive, Topology};
use std::collections::HashMap;

/// How many triangles a mesh will have, from the JSON alone, so oversized
/// scenes are refused before anything is decoded.
pub fn triangle_estimate(root: &Root, mesh: &MeshJ) -> u64 {
    let count = |i: Option<usize>| i.and_then(|i| root.accessors.get(i)).map_or(0, |a| a.count);
    mesh.primitives
        .iter()
        .map(|p| {
            let n = if p.indices.is_some() { count(p.indices) } else { count(p.attributes.get("POSITION").copied()) };
            match p.mode.unwrap_or(4) {
                4 => n / 3,
                5 | 6 => n.saturating_sub(2),
                _ => 0,
            }
        })
        .sum()
}

/// The number of morph targets of each of the file's meshes.
pub fn morph_counts(root: &Root) -> Vec<usize> {
    root.meshes.iter().map(|m| m.primitives.iter().map(|p| p.targets.len()).max().unwrap_or(0)).collect()
}

/// The accessors a primitive's loading reads, for working out which buffer
/// views to decode.
pub fn accessors_of(p: &PrimitiveJ) -> impl Iterator<Item = usize> + '_ {
    const USED: [&str; 10] = [
        "POSITION",
        "NORMAL",
        "TANGENT",
        "TEXCOORD_0",
        "TEXCOORD_1",
        "COLOR_0",
        "JOINTS_0",
        "WEIGHTS_0",
        "JOINTS_1",
        "WEIGHTS_1",
    ];
    // Attributes and indices a Draco extension holds come from its own
    // view; an uncompressed copy, if the file has one, isn't read.
    let draco = p.extensions.draco.as_ref();
    let held = move |name: &str| draco.is_some_and(|d| d.attributes.contains_key(name));
    let attributes = USED.iter().filter(move |n| !held(n)).filter_map(|name| p.attributes.get(*name).copied());
    let targets = p.targets.iter().flat_map(|t| ["POSITION", "NORMAL"].into_iter().filter_map(|k| t.get(k).copied()));
    attributes.chain(p.indices.filter(|_| draco.is_none())).chain(targets)
}

/// Loads a mesh.  A primitive that can't be read is left out with a
/// warning; only running out of the decoding budget fails the mesh.
/// `material` maps the file's material index (or `None`) to the scene's.
pub fn load_mesh(
    index: usize,
    json: &MeshJ,
    acc: &Accessors,
    ctx: &Ctx,
    material: &(dyn Fn(Option<usize>) -> Option<usize> + Sync),
) -> Result<Mesh, LoadError> {
    let name = json.name.clone().unwrap_or_else(|| format!("mesh {index}"));
    let mut mesh = Mesh {
        name: name.clone(),
        primitives: Vec::with_capacity(json.primitives.len()),
        target_names: json.extras.as_ref().map(|e| e.target_names.clone()).unwrap_or_default(),
    };
    for (pi, p) in json.primitives.iter().enumerate() {
        match load_primitive(p, acc, ctx) {
            Ok(Some(mut prim)) => {
                prim.material = material(p.material);
                mesh.primitives.push(prim);
            }
            Ok(None) => {}
            Err(e @ LoadError::TooLarge(_)) => return Err(e),
            Err(e) => ctx.warn(format!("{name}, primitive {pi} skipped: {e}")),
        }
    }
    // Every primitive must have the same number of morph targets; extra
    // names would mislabel sliders.
    let targets = mesh.primitives.iter().map(|p| p.targets.len()).max().unwrap_or(0);
    if !mesh.target_names.is_empty() && mesh.target_names.len() != targets {
        mesh.target_names.resize(targets, String::new());
    }
    Ok(mesh)
}

fn load_primitive(p: &PrimitiveJ, acc: &Accessors, ctx: &Ctx) -> Result<Option<Primitive>, LoadError> {
    let attr = |name: &str| p.attributes.get(name).copied();
    let Some(position) = attr("POSITION") else {
        return Ok(None);
    };
    // 1. Draco-compressed geometry, decoded first: its attributes and
    //    faces replace the accessors' (whose views, if any, hold an
    //    uncompressed copy).  Attributes it doesn't hold read as usual.
    let draco = p.extensions.draco.as_ref().map(|d| crate::draco::Decoded::new(d, acc, ctx)).transpose()?;
    let read = |name: &str, i: usize, pad| -> Result<Vec<[f32; 3]>, LoadError> {
        read_attr(name, i, pad, draco.as_ref(), acc)
    };

    // 2. Positions decide the vertex count every other attribute must match.
    let mut prim = Primitive { positions: vec3s(read("POSITION", position, [0.0; 3])?), ..Default::default() };
    let n = prim.positions.len();

    // 3. Optional attributes.  Quantized normals and tangents are only
    // approximately unit length, so they are renormalized.
    let quantized = |i: usize| draco.is_some() || acc.is_quantized(i);
    prim.normals = optional("NORMAL", attr("NORMAL"), n, ctx, |i| {
        let mut normals = vec3s(read("NORMAL", i, [0.0; 3])?);
        if quantized(i) {
            normals.iter_mut().for_each(|v| *v = v.normalize_or(Vec3::Y));
        }
        Ok(normals)
    })?;
    prim.tangents = optional("TANGENT", attr("TANGENT"), n, ctx, |i| {
        let mut tangents: Vec<Vec4> = read_attr("TANGENT", i, [0.0, 0.0, 0.0, 1.0], draco.as_ref(), acc)?
            .into_iter()
            .map(Vec4::from_array)
            .collect();
        if quantized(i) {
            for t in &mut tangents {
                *t = t.truncate().normalize_or(Vec3::X).extend(if t.w < 0.0 { -1.0 } else { 1.0 });
            }
        }
        Ok(tangents)
    })?;
    let uv = |name: &'static str| {
        optional(name, attr(name), n, ctx, |i| Ok(vec2s(read_attr(name, i, [0.0; 2], draco.as_ref(), acc)?)))
    };
    prim.uv0 = uv("TEXCOORD_0")?;
    prim.uv1 = uv("TEXCOORD_1")?;
    let rgba = |i| {
        Ok(read_attr("COLOR_0", i, [0.0, 0.0, 0.0, 1.0], draco.as_ref(), acc)?
            .into_iter()
            .map(Vec4::from_array)
            .collect())
    };
    prim.colors = optional("COLOR_0", attr("COLOR_0"), n, ctx, rgba)?;
    let raw4 = |name: &str| optional(name, attr(name), n, ctx, |i| read_attr(name, i, [0.0; 4], draco.as_ref(), acc));
    let (joints, weights) = (raw4("JOINTS_0")?, raw4("WEIGHTS_0")?);
    if !joints.is_empty() && !weights.is_empty() {
        let (joints1, weights1) = (raw4("JOINTS_1")?, raw4("WEIGHTS_1")?);
        (prim.joints, prim.weights) = skin_influences(&joints, &weights, &joints1, &weights1);
    }

    // 4. Indices, checked against the vertex count, then the topology turned
    // into one the renderer draws.  Draco geometry is always triangles.
    let (mode, indices) = match (&draco, p.indices) {
        (Some(d), _) => {
            ctx.reserve(d.points() as u64 * 4)?;
            (4, d.indices())
        }
        (None, Some(i)) => (p.mode.unwrap_or(4), acc.read_u32(i)?),
        (None, None) => {
            ctx.reserve(n as u64 * 4)?;
            (p.mode.unwrap_or(4), (0..n as u32).collect())
        }
    };
    if indices.iter().any(|&i| i as usize >= n) {
        return Err(LoadError::invalid("an index is past the last vertex"));
    }
    (prim.topology, prim.indices) = topology(mode, indices)?;

    // 5. Morph targets.  If any can't be read, all are dropped, since
    // targets are numbered and weights and names refer to them by number.
    match morph_targets(p, n, acc, ctx) {
        Ok(targets) => prim.targets = targets,
        Err(e @ LoadError::TooLarge(_)) => return Err(e),
        Err(e) => ctx.warn(format!("morph targets dropped: {e}")),
    }
    compact(&mut prim);
    Ok(Some(prim))
}

/// Keeps only the vertices a primitive's indices use, when that is under
/// three quarters of them.  VRoid and some other exporters give every
/// primitive of a mesh the mesh's whole vertex buffer, which would
/// otherwise be stored, uploaded and skinned once per primitive.
fn compact(prim: &mut Primitive) {
    let n = prim.positions.len();
    let mut new_of_old = vec![u32::MAX; n];
    for &i in &prim.indices {
        new_of_old[i as usize] = 0;
    }
    let keep: Vec<u32> = (0..n as u32).filter(|&i| new_of_old[i as usize] == 0).collect();
    if keep.len() * 4 >= n * 3 {
        return;
    }
    for (new, &old) in keep.iter().enumerate() {
        new_of_old[old as usize] = new as u32;
    }
    for i in &mut prim.indices {
        *i = new_of_old[*i as usize];
    }
    fn gather<T: Copy>(v: &mut Vec<T>, keep: &[u32]) {
        if !v.is_empty() {
            *v = keep.iter().map(|&i| v[i as usize]).collect();
        }
    }
    gather(&mut prim.positions, &keep);
    gather(&mut prim.normals, &keep);
    gather(&mut prim.tangents, &keep);
    gather(&mut prim.uv0, &keep);
    gather(&mut prim.uv1, &keep);
    gather(&mut prim.colors, &keep);
    gather(&mut prim.joints, &keep);
    gather(&mut prim.weights, &keep);
    for t in &mut prim.targets {
        gather(&mut t.positions, &keep);
        gather(&mut t.normals, &keep);
    }
}

/// A primitive's morph targets.  A target without positions moves nothing.
fn morph_targets(p: &PrimitiveJ, n: usize, acc: &Accessors, ctx: &Ctx) -> Result<Vec<MorphTarget>, LoadError> {
    // A face can carry dozens of targets over tens of thousands of
    // vertices, the largest part of a character's geometry, so they are
    // read in parallel.
    let target = |(ti, t): (usize, &HashMap<String, usize>)| -> Result<MorphTarget, LoadError> {
        let mut target = MorphTarget::default();
        match t.get("POSITION") {
            Some(&i) => target.positions = vec3s(acc.read(i, [0.0; 3])?),
            None => {
                ctx.reserve(n as u64 * 12)?;
                target.positions = vec![Vec3::ZERO; n];
            }
        }
        if let Some(&i) = t.get("NORMAL") {
            target.normals = vec3s(acc.read(i, [0.0; 3])?);
        }
        if target.positions.len() != n || !(target.normals.is_empty() || target.normals.len() == n) {
            return Err(LoadError::invalid(format!("target {ti} has the wrong number of elements")));
        }
        Ok(target)
    };
    p.targets.par_iter().enumerate().map(target).collect()
}

/// Reads attribute `name` from a primitive's Draco geometry when that holds
/// it, else from accessor `index`.
fn read_attr<const N: usize>(
    name: &str,
    index: usize,
    pad: [f32; N],
    draco: Option<&crate::draco::Decoded>,
    acc: &Accessors,
) -> Result<Vec<[f32; N]>, LoadError> {
    let normalized = acc.json.get(index).is_some_and(|a| a.normalized);
    match draco.and_then(|d| d.read(name, pad, normalized)) {
        Some(values) => values,
        None => acc.read(index, pad),
    }
}

/// Reads an optional attribute, which must have one element per vertex.
/// A missing or unreadable one is left empty, with a warning for the latter.
fn optional<T>(
    name: &str,
    attr: Option<usize>,
    n: usize,
    ctx: &Ctx,
    read: impl FnOnce(usize) -> Result<Vec<T>, LoadError>,
) -> Result<Vec<T>, LoadError> {
    let Some(i) = attr else { return Ok(Vec::new()) };
    match read(i) {
        Ok(v) if v.len() == n => Ok(v),
        Ok(_) => {
            ctx.warn(format!("{name} has the wrong number of elements and was ignored"));
            Ok(Vec::new())
        }
        Err(e @ LoadError::TooLarge(_)) => Err(e),
        Err(e) => {
            ctx.warn(format!("{name} couldn't be read: {e}"));
            Ok(Vec::new())
        }
    }
}

fn vec3s(v: Vec<[f32; 3]>) -> Vec<Vec3> {
    v.into_iter().map(Vec3::from_array).collect()
}

fn vec2s(v: Vec<[f32; 2]>) -> Vec<Vec2> {
    v.into_iter().map(Vec2::from_array).collect()
}

/// Joint indices and weights, keeping the four strongest influences when
/// the file has eight (`JOINTS_1`), and renormalizing what is kept.
fn skin_influences(j0: &[[f32; 4]], w0: &[[f32; 4]], j1: &[[f32; 4]], w1: &[[f32; 4]]) -> (Vec<[u16; 4]>, Vec<Vec4>) {
    let joint = |v: f32| v.clamp(0.0, u16::MAX as f32) as u16;
    if j1.len() != j0.len() || w1.len() != w0.len() {
        let joints = j0.iter().map(|j| j.map(joint)).collect();
        let weights = w0.iter().map(|&w| Vec4::from_array(w)).collect();
        return (joints, weights);
    }
    let mut joints = Vec::with_capacity(j0.len());
    let mut weights = Vec::with_capacity(j0.len());
    for i in 0..j0.len() {
        let mut all: [(f32, f32); 8] = [(0.0, 0.0); 8];
        for k in 0..4 {
            all[k] = (w0[i][k], j0[i][k]);
            all[k + 4] = (w1[i][k], j1[i][k]);
        }
        all.sort_by(|a, b| b.0.total_cmp(&a.0));
        let sum: f32 = all[..4].iter().map(|a| a.0).sum();
        let scale = if sum > 0.0 { 1.0 / sum } else { 0.0 };
        joints.push([joint(all[0].1), joint(all[1].1), joint(all[2].1), joint(all[3].1)]);
        weights.push(Vec4::new(all[0].0, all[1].0, all[2].0, all[3].0) * scale);
    }
    (joints, weights)
}

/// Turns glTF's seven primitive modes into triangle, line and point lists.
fn topology(mode: u32, indices: Vec<u32>) -> Result<(Topology, Vec<u32>), LoadError> {
    let n = indices.len();
    Ok(match mode {
        0 => (Topology::Points, indices),
        1 => (Topology::Lines, indices[..n - n % 2].to_vec()),
        2 => {
            let lines = (0..n).flat_map(|i| [indices[i], indices[(i + 1) % n]]);
            (Topology::Lines, if n < 2 { Vec::new() } else { lines.collect() })
        }
        3 => (Topology::Lines, indices.windows(2).flat_map(|w| [w[0], w[1]]).collect()),
        4 => {
            let mut indices = indices;
            indices.truncate(n - n % 3);
            (Topology::Triangles, indices)
        }
        5 => {
            let tri = |i: usize| {
                if i.is_multiple_of(2) {
                    [indices[i], indices[i + 1], indices[i + 2]]
                } else {
                    [indices[i + 1], indices[i], indices[i + 2]]
                }
            };
            (Topology::Triangles, (0..n.saturating_sub(2)).flat_map(tri).collect())
        }
        6 => {
            let tri = |i: usize| [indices[i + 1], indices[i + 2], indices[0]];
            (Topology::Triangles, (0..n.saturating_sub(2)).flat_map(tri).collect())
        }
        other => return Err(LoadError::invalid(format!("unknown primitive mode {other}"))),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_fans_and_loops() {
        assert_eq!(topology(5, vec![0, 1, 2, 3]).unwrap().1, vec![0, 1, 2, 2, 1, 3]);
        assert_eq!(topology(6, vec![0, 1, 2, 3]).unwrap().1, vec![1, 2, 0, 2, 3, 0]);
        assert_eq!(topology(2, vec![0, 1, 2]).unwrap().1, vec![0, 1, 1, 2, 2, 0]);
        assert_eq!(topology(3, vec![0, 1, 2]).unwrap().1, vec![0, 1, 1, 2]);
        assert_eq!(topology(4, vec![0, 1, 2, 3]).unwrap().1, vec![0, 1, 2]);
        assert_eq!(topology(5, vec![0]).unwrap().1, Vec::<u32>::new());
        assert_eq!(topology(2, vec![0]).unwrap().1, Vec::<u32>::new());
        assert!(topology(9, vec![0]).is_err());
    }

    #[test]
    fn shared_vertex_buffers_are_compacted() {
        let mut p = Primitive {
            positions: (0..10).map(|i| Vec3::splat(i as f32)).collect(),
            uv0: (0..10).map(|i| Vec2::splat(i as f32)).collect(),
            indices: vec![7, 2, 9],
            targets: vec![MorphTarget { positions: (0..10).map(|i| Vec3::splat(i as f32)).collect(), normals: vec![] }],
            ..Default::default()
        };
        compact(&mut p);
        assert_eq!(p.indices, vec![1, 0, 2]);
        assert_eq!(p.positions, vec![Vec3::splat(2.0), Vec3::splat(7.0), Vec3::splat(9.0)]);
        assert_eq!(p.uv0[2], Vec2::splat(9.0));
        assert_eq!(p.targets[0].positions.len(), 3);
        // Mostly used buffers are left alone.
        let mut q = Primitive { positions: vec![Vec3::ZERO; 4], indices: vec![0, 1, 2], ..Default::default() };
        compact(&mut q);
        assert_eq!(q.positions.len(), 4);
    }

    #[test]
    fn eight_influences_keep_the_strongest_four() {
        let (j, w) = skin_influences(
            &[[1.0, 2.0, 3.0, 4.0]],
            &[[0.1, 0.3, 0.0, 0.1]],
            &[[5.0, 6.0, 7.0, 8.0]],
            &[[0.2, 0.0, 0.2, 0.1]],
        );
        assert_eq!(j[0][0], 2);
        assert!((w[0].element_sum() - 1.0).abs() < 1e-6);
        assert!(!j[0].contains(&3));
    }
}
