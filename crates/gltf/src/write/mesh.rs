//! Meshes: each primitive is checked and repaired while it is written, one
//! at a time, so a large scene isn't copied twice.
//!
//! glTF and its validator are stricter than Slerp's renderer: normals and
//! tangents must be unit length, skin weights must sum to one with no
//! joint named twice, indices must stay inside the vertices, every
//! primitive of a mesh must have the same number of morph targets, and a
//! material's texture needs the UV set it reads.  Whatever falls short is
//! fixed here (or the primitive left out), and counted in one warning per
//! kind of problem.

use super::Writer;
use super::bin::{ARRAY_BUFFER, BYTE, ELEMENT_ARRAY_BUFFER, INT, Kind, SHORT};
use super::plan::{MeshSource, Plan};
use super::{WriteError, material};
use serde_json::{Map, Value, json};
use slerp_scene::glam::{Vec2, Vec3, Vec4};
use slerp_scene::{Effect, Primitive, Topology};
use std::borrow::Cow;
use std::collections::HashMap;

/// What a primitive is drawn with: a scene material, Slerp's grey default
/// for primitives that name none, or plain white for splats' points.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MaterialKey {
    Scene(usize),
    Default,
    Points,
}

/// The materials the planned meshes use, in first-use order.
pub struct Prepared {
    pub materials: Vec<MaterialKey>,
}

/// Lists the materials the planned meshes use, so they and their pictures
/// can be written before the geometry.
pub fn prepare(w: &Writer, plan: &Plan) -> Prepared {
    let scene = w.scene;
    let mut materials = Vec::new();
    let mut add = |k: MaterialKey| {
        if !materials.contains(&k) {
            materials.push(k);
        }
    };
    for m in &plan.meshes {
        match m.source {
            MeshSource::Scene(i) => {
                for p in scene.meshes[i].primitives.iter().filter(|p| !panorama_sky(w, p)) {
                    add(key_of(w, p.material));
                }
            }
            MeshSource::Splats => add(MaterialKey::Points),
        }
    }
    Prepared { materials }
}

/// Doom's sky: a curtain reaching a kilometre up whose picture the
/// renderer wraps around the viewer at infinity.  glTF can't say that, and
/// as a surface it would dwarf the level, so it is left out.
fn panorama_sky(w: &Writer, p: &Primitive) -> bool {
    p.material.and_then(|m| w.scene.materials.get(m)).is_some_and(|m| m.effect == Effect::PanoramaSky)
}

fn key_of(w: &Writer, material: Option<usize>) -> MaterialKey {
    match material {
        Some(i) if i < w.scene.materials.len() => MaterialKey::Scene(i),
        _ => MaterialKey::Default,
    }
}

/// Counts of repairs, reported once each at the end.
#[derive(Default)]
struct Repairs {
    empty: usize,
    bad_indices: usize,
    non_finite: usize,
    mismatched: usize,
    unweighted: usize,
    billboards: usize,
    skies: usize,
}

/// Writes the planned meshes.  Returns each one's index in the file, or
/// `None` when none of its primitives could be written.
pub fn write(
    w: &mut Writer,
    plan: &Plan,
    prepared: &Prepared,
    materials: &[usize],
) -> Result<Vec<Option<usize>>, WriteError> {
    let scene = w.scene;
    let mut out = Vec::new();
    let mut zeros: HashMap<(usize, bool), usize> = HashMap::new();
    let mut repairs = Repairs::default();
    for planned in &plan.meshes {
        let (name, prims, target_names): (String, Vec<Cow<Primitive>>, Vec<String>) = match planned.source {
            MeshSource::Scene(i) => {
                let m = &scene.meshes[i];
                (m.name.clone(), m.primitives.iter().map(Cow::Borrowed).collect(), m.target_names.clone())
            }
            MeshSource::Splats => {
                let Some(s) = &scene.splats else {
                    out.push(None);
                    continue;
                };
                (String::from("Gaussian splats"), vec![Cow::Owned(splat_points(s))], Vec::new())
            }
        };
        let targets = prims.iter().map(|p| p.targets.len()).max().unwrap_or(0);
        let mut written = Vec::new();
        for p in &prims {
            if panorama_sky(w, p) {
                repairs.skies += 1;
                continue;
            }
            let key = match planned.source {
                MeshSource::Splats => MaterialKey::Points,
                MeshSource::Scene(_) => key_of(w, p.material),
            };
            let slot = prepared.materials.iter().position(|&k| k == key).unwrap_or(0);
            let material = materials.get(slot).copied();
            let ctx = PrimCtx {
                skinned: planned.skinned,
                joint_limit: planned.joint_limit,
                targets,
                material,
                uv_needed: material::uv_sets(w.scene, key),
                billboard: matches!(key, MaterialKey::Scene(i) if scene.materials[i].effect == Effect::Billboard),
                indexed: planned.source != MeshSource::Splats,
            };
            if let Some(prim) = write_primitive(w, p, &ctx, &mut zeros, &mut repairs)? {
                written.push(prim);
            }
        }
        if written.is_empty() {
            out.push(None);
            continue;
        }
        let mut mesh = Map::new();
        if !name.is_empty() {
            mesh.insert("name".into(), name.into());
        }
        mesh.insert("primitives".into(), Value::Array(written));
        if targets > 0 && target_names.iter().any(|n| !n.is_empty()) {
            let mut names = target_names;
            names.resize(targets, String::new());
            mesh.insert("extras".into(), json!({ "targetNames": names }));
        }
        w.meshes.push(Value::Object(mesh));
        out.push(Some(w.meshes.len() - 1));
    }
    report(w, &repairs);
    Ok(out)
}

fn report(w: &mut Writer, r: &Repairs) {
    if r.empty > 0 {
        w.warn(format!("{} empty primitives were left out", r.empty));
    }
    if r.bad_indices > 0 {
        w.warn(format!(
            "{} triangles, lines or points named vertices that don't exist and were left out",
            r.bad_indices
        ));
    }
    if r.non_finite > 0 {
        w.warn(format!("{} vertex values weren't numbers and were replaced", r.non_finite));
    }
    if r.mismatched > 0 {
        w.warn(format!("{} vertex attributes had the wrong length and were left out", r.mismatched));
    }
    if r.unweighted > 0 {
        w.warn(format!(
            "{} primitives of skinned meshes had no skin weights and were bound to the first joint",
            r.unweighted
        ));
    }
    if r.skies > 0 {
        w.warn("Doom's sky was left out, since glTF can't draw a sky at infinity");
    }
    if r.billboards > 0 {
        w.warn("sprites that turn to face the camera were saved as crossed pairs of quads");
    }
}

/*┌─────────────────╮
| Primitives      |
└─────────────────╯*/
//region Primitives

struct PrimCtx {
    skinned: bool,
    joint_limit: usize,
    targets: usize,
    material: Option<usize>,
    /// The UV sets the material's textures read.
    uv_needed: [bool; 2],
    billboard: bool,
    indexed: bool,
}

fn write_primitive(
    w: &mut Writer,
    p: &Primitive,
    ctx: &PrimCtx,
    zeros: &mut HashMap<(usize, bool), usize>,
    r: &mut Repairs,
) -> Result<Option<Value>, WriteError> {
    let p: Cow<Primitive> = if ctx.billboard && p.uv1.len() == p.positions.len() && p.topology == Topology::Triangles {
        r.billboards += 1;
        Cow::Owned(crossed_quads(p))
    } else {
        Cow::Borrowed(p)
    };
    let n = p.positions.len();
    if n == 0 || n >= u32::MAX as usize {
        r.empty += 1;
        return Ok(None);
    }

    // 1. Indices first: a primitive left with none writes nothing.
    let indices = if ctx.indexed { Some(clean_indices(&p, r)) } else { None };
    if indices.as_ref().is_some_and(|i| i.is_empty()) {
        r.empty += 1;
        return Ok(None);
    }
    let mut attributes = Map::new();
    let mut add = |name: &str, accessor: usize| {
        attributes.insert(name.into(), accessor.into());
    };

    // 2. Positions and the optional attributes.
    let positions: Vec<f32> = p.positions.iter().flat_map(|v| finite3(*v, Vec3::ZERO, r).to_array()).collect();
    add("POSITION", w.bin.floats(&positions, Kind::Vec3, Some(ARRAY_BUFFER), true)?);
    drop(positions);
    let has_normals = matching(&p.normals, n, r);
    if has_normals {
        let normals: Vec<f32> = p.normals.iter().flat_map(|v| unit(*v, Vec3::Y).to_array()).collect();
        add("NORMAL", w.bin.floats(&normals, Kind::Vec3, Some(ARRAY_BUFFER), false)?);
    }
    if has_normals && matching(&p.tangents, n, r) {
        let tangents: Vec<f32> =
            p.tangents.iter().zip(&p.normals).flat_map(|(t, nrm)| tangent(*t, *nrm).to_array()).collect();
        add("TANGENT", w.bin.floats(&tangents, Kind::Vec4, Some(ARRAY_BUFFER), false)?);
    }
    let mut uv0 = None;
    for (set, uv) in [&p.uv0, &p.uv1].into_iter().enumerate() {
        let name = if set == 0 { "TEXCOORD_0" } else { "TEXCOORD_1" };
        let accessor = if matching(uv, n, r) {
            let values: Vec<f32> = uv.iter().flat_map(|v| finite2(*v, r).to_array()).collect();
            Some(w.bin.floats(&values, Kind::Vec2, Some(ARRAY_BUFFER), false)?)
        } else if ctx.uv_needed[set] {
            // A texture reads this set: the first set, or zeros.
            Some(uv0.unwrap_or_else(|| *zeros.entry((n, false)).or_insert_with(|| w.bin.zeros(Kind::Vec2, n, false))))
        } else {
            None
        };
        if let Some(a) = accessor {
            add(name, a);
            if set == 0 {
                uv0 = Some(a);
            }
        }
    }
    if matching(&p.colors, n, r) {
        let colors: Vec<f32> = p
            .colors
            .iter()
            .flat_map(|c| {
                let c = if c.is_finite() { *c } else { Vec4::ONE };
                c.clamp(Vec4::ZERO, Vec4::ONE).to_array()
            })
            .collect();
        add("COLOR_0", w.bin.floats(&colors, Kind::Vec4, Some(ARRAY_BUFFER), false)?);
    }

    // 3. Skinning.
    if ctx.skinned {
        let (joints, weights) = if p.joints.len() == n && p.weights.len() == n {
            p.joints.iter().zip(&p.weights).map(|(j, wt)| clean_influence(*j, *wt, ctx.joint_limit)).unzip()
        } else {
            r.unweighted += 1;
            (vec![[0u16; 4]; n], vec![Vec4::X; n])
        };
        let largest = joints.iter().flatten().copied().max().unwrap_or(0);
        let accessor = if largest < 256 {
            let bytes: Vec<u8> = joints.iter().flatten().map(|&j| j as u8).collect();
            w.bin.ints(&bytes, BYTE, Kind::Vec4, Some(ARRAY_BUFFER), false)?
        } else {
            w.bin.ints(bytemuck::cast_slice(&joints), SHORT, Kind::Vec4, Some(ARRAY_BUFFER), false)?
        };
        add("JOINTS_0", accessor);
        let weights: Vec<f32> = weights.iter().flat_map(|v| v.to_array()).collect();
        add("WEIGHTS_0", w.bin.floats(&weights, Kind::Vec4, Some(ARRAY_BUFFER), false)?);
    }

    // 4. Morph targets, all present: padding is an accessor of zeros
    //    with no data.
    let mut targets = Vec::with_capacity(ctx.targets);
    for t in 0..ctx.targets {
        let mut target = Map::new();
        match p.targets.get(t) {
            Some(mt) if mt.positions.len() == n => {
                let values: Vec<f32> =
                    mt.positions.iter().flat_map(|v| finite3(*v, Vec3::ZERO, r).to_array()).collect();
                target.insert("POSITION".into(), w.bin.floats(&values, Kind::Vec3, Some(ARRAY_BUFFER), true)?.into());
                if has_normals && mt.normals.len() == n {
                    let values: Vec<f32> =
                        mt.normals.iter().flat_map(|v| finite3(*v, Vec3::ZERO, r).to_array()).collect();
                    target
                        .insert("NORMAL".into(), w.bin.floats(&values, Kind::Vec3, Some(ARRAY_BUFFER), false)?.into());
                }
            }
            _ => {
                let zero = *zeros.entry((n, true)).or_insert_with(|| w.bin.zeros(Kind::Vec3, n, true));
                target.insert("POSITION".into(), zero.into());
            }
        }
        targets.push(Value::Object(target));
    }

    // 5. The primitive.
    let mut prim = Map::new();
    prim.insert("attributes".into(), Value::Object(attributes));
    if let Some(indices) = indices {
        let accessor = if n <= u16::MAX as usize {
            let short: Vec<u16> = indices.iter().map(|&i| i as u16).collect();
            w.bin.ints(bytemuck::cast_slice(&short), SHORT, Kind::Scalar, Some(ELEMENT_ARRAY_BUFFER), false)?
        } else {
            w.bin.ints(bytemuck::cast_slice(&indices), INT, Kind::Scalar, Some(ELEMENT_ARRAY_BUFFER), false)?
        };
        prim.insert("indices".into(), accessor.into());
    }
    if let Some(m) = ctx.material {
        prim.insert("material".into(), m.into());
    }
    match p.topology {
        Topology::Triangles => {}
        Topology::Lines => {
            prim.insert("mode".into(), 1.into());
        }
        Topology::Points => {
            prim.insert("mode".into(), 0.into());
        }
    }
    if !targets.is_empty() {
        prim.insert("targets".into(), Value::Array(targets));
    }
    Ok(Some(Value::Object(prim)))
}

/// Indices inside the vertices, in whole triangles or lines.
fn clean_indices(p: &Primitive, r: &mut Repairs) -> Vec<u32> {
    let n = p.positions.len() as u64;
    let group = match p.topology {
        Topology::Triangles => 3,
        Topology::Lines => 2,
        Topology::Points => 1,
    };
    let mut out = Vec::with_capacity(p.indices.len() - p.indices.len() % group);
    for g in p.indices.chunks_exact(group) {
        if g.iter().all(|&i| (i as u64) < n) {
            out.extend_from_slice(g);
        } else {
            r.bad_indices += 1;
        }
    }
    out
}

/// True when an optional attribute is present with one entry per vertex.
fn matching<T>(values: &[T], n: usize, r: &mut Repairs) -> bool {
    if !values.is_empty() && values.len() != n {
        r.mismatched += 1;
    }
    values.len() == n
}

fn finite3(v: Vec3, fallback: Vec3, r: &mut Repairs) -> Vec3 {
    if v.is_finite() {
        v
    } else {
        r.non_finite += 1;
        fallback
    }
}

fn finite2(v: Vec2, r: &mut Repairs) -> Vec2 {
    if v.is_finite() {
        v
    } else {
        r.non_finite += 1;
        Vec2::ZERO
    }
}

/// A unit vector, or `fallback` for zero and broken ones.
fn unit(v: Vec3, fallback: Vec3) -> Vec3 {
    let v = v.normalize_or(fallback);
    // normalize_or can land a hair off unit length; once more settles it.
    v.normalize_or(fallback)
}

/// A tangent glTF accepts: unit xyz (any direction across the normal when
/// it has none) and a handedness of exactly 1 or -1.
fn tangent(t: Vec4, normal: Vec3) -> Vec4 {
    let normal = unit(normal, Vec3::Y);
    let xyz = t.truncate();
    let xyz = if xyz.is_finite() && xyz.length_squared() > 1e-12 { xyz } else { normal.any_orthonormal_vector() };
    let w = if t.w < 0.0 { -1.0 } else { 1.0 };
    unit(xyz, normal.any_orthonormal_vector()).extend(w)
}

/// One vertex's joints and weights as glTF wants them: weights that are
/// numbers, at least zero and sum to one; no joint twice; no joint past
/// the skin; and joint 0 wherever the weight is zero.
fn clean_influence(joints: [u16; 4], weights: Vec4, limit: usize) -> ([u16; 4], Vec4) {
    let mut j = [0u16; 4];
    let mut wt = [0f32; 4];
    let mut used = 0;
    for k in 0..4 {
        let weight = weights[k];
        if !(weight.is_finite() && weight > 0.0) || joints[k] as usize >= limit {
            continue;
        }
        match j[..used].iter().position(|&x| x == joints[k]) {
            Some(i) => wt[i] += weight,
            None => {
                j[used] = joints[k];
                wt[used] = weight;
                used += 1;
            }
        }
    }
    let sum: f32 = wt.iter().sum();
    if used == 0 || !(sum.is_finite() && sum > 0.0) {
        return ([0; 4], Vec4::X);
    }
    let mut w = Vec4::from_array(wt) / sum;
    // Put rounding's remainder on the largest weight, so the sum is as
    // close to one as 32-bit floats get.
    let largest = (0..4).max_by(|&a, &b| w[a].total_cmp(&w[b])).unwrap_or(0);
    let rest: f32 = (0..4).filter(|&k| k != largest).map(|k| w[k]).sum();
    w[largest] = 1.0 - rest;
    (j, w)
}

//endregion

/*┌─────────────────╮
| Special shapes  |
└─────────────────╯*/
//region Special shapes

/// Doom's things turn to face the camera, which a glTF file can't do: each
/// becomes two quads crossed at right angles, so it reads from every side.
/// The second UV set holds each corner's offset from the anchor, to the
/// right and up as the camera sees it.
fn crossed_quads(p: &Primitive) -> Primitive {
    let n = p.positions.len();
    let mut out = Primitive { topology: Topology::Triangles, material: p.material, ..Default::default() };
    for (facing, normal) in [(0, Vec3::Z), (1, Vec3::X)] {
        for (pos, off) in p.positions.iter().zip(&p.uv1) {
            let corner = match facing {
                0 => Vec3::new(off.x, off.y, 0.0),
                _ => Vec3::new(0.0, off.y, -off.x),
            };
            out.positions.push(*pos + corner);
            out.normals.push(normal);
        }
        if p.uv0.len() == n {
            out.uv0.extend_from_slice(&p.uv0);
        }
        if p.colors.len() == n {
            out.colors.extend_from_slice(&p.colors);
        }
    }
    let inside: Vec<u32> =
        p.indices.chunks_exact(3).filter(|t| t.iter().all(|&i| (i as usize) < n)).flatten().copied().collect();
    let offset = n as u32;
    out.indices = inside.iter().copied().chain(inside.iter().map(|i| i + offset)).collect();
    out
}

/// Gaussian splats as points coloured by each splat's base colour (from
/// the photos' sRGB to glTF's linear) and opacity.
fn splat_points(s: &slerp_scene::Splats) -> Primitive {
    let linear = |c: f32| {
        let c = c.clamp(0.0, 1.0);
        if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
    };
    Primitive {
        positions: s.positions.clone(),
        colors: s.colors.iter().map(|c| Vec4::new(linear(c.x), linear(c.y), linear(c.z), c.w)).collect(),
        topology: Topology::Points,
        ..Default::default()
    }
}

//endregion
