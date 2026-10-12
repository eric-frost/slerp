//! Skins and animation clips.
//!
//! Keyframes are written as the scene holds them, with their
//! interpolation.  What glTF forbids is repaired: times must rise strictly
//! from zero, rotations must be unit quaternions, a cubic spline needs two
//! keys, and a weights channel needs a mesh with that many morph targets.
//! Channels of one clip usually share their times, so equal time lists
//! share one accessor.

use super::WriteError;
use super::Writer;
use super::bin::Kind;
use super::plan::{MeshSource, Plan};
use serde_json::{Map, Value, json};
use slerp_scene::glam::Quat;
use slerp_scene::{Interpolation, Property};
use std::collections::HashMap;

/// Writes the planned skins, in the plan's order, so a plan's skin index is
/// the file's.
pub fn write_skins(w: &mut Writer, plan: &Plan) -> Result<(), WriteError> {
    for skin in &plan.skins {
        let mut o = Map::new();
        if !skin.name.is_empty() {
            o.insert("name".into(), skin.name.clone().into());
        }
        o.insert("joints".into(), json!(skin.joints));
        let matrices: Vec<f32> = skin.inverse_bind.iter().flat_map(|m| m.to_cols_array()).collect();
        if !matrices.is_empty() {
            o.insert("inverseBindMatrices".into(), w.bin.floats(&matrices, Kind::Mat4, None, false)?.into());
        }
        if let Some(s) = skin.skeleton {
            o.insert("skeleton".into(), s.into());
        }
        w.skins.push(Value::Object(o));
    }
    Ok(())
}

/// Writes the planned clips.  `meshes` gives each planned mesh's index in
/// the file, which a weights channel's node must show.
pub fn write_clips(w: &mut Writer, plan: &Plan, meshes: &[Option<usize>]) -> Result<(), WriteError> {
    let scene = w.scene;
    let morphs = |node: usize| -> usize {
        let Some(m) = plan.nodes.get(node).and_then(|n| n.mesh) else { return 0 };
        if meshes.get(m.index).copied().flatten().is_none() {
            return 0;
        }
        match plan.meshes[m.index].source {
            MeshSource::Scene(i) => scene.meshes[i].primitives.iter().map(|p| p.targets.len()).max().unwrap_or(0),
            MeshSource::Splats => 0,
        }
    };
    let mut times_cache: HashMap<Vec<u32>, usize> = HashMap::new();
    let mut dropped_keys = 0usize;
    for clip in &plan.clips {
        let mut channels = Vec::new();
        let mut samplers = Vec::new();
        for &(ch, target) in &clip.channels {
            let (comps, path, kind) = match ch.property {
                Property::Translation => (3, "translation", Kind::Vec3),
                Property::Rotation => (4, "rotation", Kind::Vec4),
                Property::Scale => (3, "scale", Kind::Vec3),
                Property::Weights => (morphs(target), "weights", Kind::Scalar),
            };
            if comps == 0 {
                continue;
            }
            let cubic = ch.interpolation == Interpolation::CubicSpline;
            let stride = comps * if cubic { 3 } else { 1 };

            // 1. Keys with rising times and a full set of values.
            let count = ch.times.len().min(ch.values.len() / stride);
            let mut keys = Vec::with_capacity(count);
            let mut last = f32::NEG_INFINITY;
            for (i, &t) in ch.times[..count].iter().enumerate() {
                if t.is_finite() && t >= 0.0 && t > last {
                    keys.push(i);
                    last = t;
                }
            }
            dropped_keys += ch.times.len() - keys.len();
            if keys.is_empty() {
                continue;
            }

            // 2. A cubic spline needs two keys; one key holds still.
            let (interpolation, element) = match ch.interpolation {
                Interpolation::CubicSpline if keys.len() < 2 => (Interpolation::Linear, Some(1)),
                i => (i, None),
            };
            let times: Vec<f32> = keys.iter().map(|&i| ch.times[i]).collect();
            let mut values = Vec::with_capacity(keys.len() * stride);
            for &i in &keys {
                let key = &ch.values[i * stride..(i + 1) * stride];
                match element {
                    Some(e) => values.extend_from_slice(&key[e * comps..(e + 1) * comps]),
                    None => values.extend_from_slice(key),
                }
            }
            values.iter_mut().filter(|v| !v.is_finite()).for_each(|v| *v = 0.0);
            if ch.property == Property::Rotation {
                // Values (the middle of each cubic triple) must be unit
                // quaternions; tangents may be anything.
                let per_key = if interpolation == Interpolation::CubicSpline { 3 } else { 1 };
                for (k, q) in values.chunks_exact_mut(4).enumerate() {
                    if per_key == 1 || k % 3 == 1 {
                        let n = Quat::from_slice(q).normalize();
                        let n = if n.is_finite() { n } else { Quat::IDENTITY };
                        q.copy_from_slice(&n.to_array());
                    }
                }
            }

            // 3. The sampler and channel.
            let bits: Vec<u32> = times.iter().map(|t| t.to_bits()).collect();
            let input = match times_cache.get(&bits) {
                Some(&a) => a,
                None => {
                    let a = w.bin.floats(&times, Kind::Scalar, None, true)?;
                    times_cache.insert(bits, a);
                    a
                }
            };
            let output = w.bin.floats(&values, kind, None, false)?;
            let mut sampler = Map::new();
            sampler.insert("input".into(), input.into());
            sampler.insert("output".into(), output.into());
            match interpolation {
                Interpolation::Linear => {}
                Interpolation::Step => {
                    sampler.insert("interpolation".into(), "STEP".into());
                }
                Interpolation::CubicSpline => {
                    sampler.insert("interpolation".into(), "CUBICSPLINE".into());
                }
            }
            samplers.push(Value::Object(sampler));
            channels.push(json!({ "sampler": samplers.len() - 1, "target": { "node": target, "path": path } }));
        }
        if channels.is_empty() {
            continue;
        }
        let mut o = Map::new();
        if !clip.name.is_empty() {
            o.insert("name".into(), clip.name.clone().into());
        }
        o.insert("channels".into(), Value::Array(channels));
        o.insert("samplers".into(), Value::Array(samplers));
        w.animations.push(Value::Object(o));
    }
    if dropped_keys > 0 {
        w.warn(format!("{dropped_keys} keyframes out of order, before zero or not numbers were left out"));
    }
    Ok(())
}
