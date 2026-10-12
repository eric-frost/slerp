//! Skins and animation clips.

use crate::Ctx;
use crate::accessor::Accessors;
use crate::json::{AnimationJ, Root, SkinJ};
use slerp_scene::glam::Mat4;
use slerp_scene::{Animation, Channel, Interpolation, LoadError, Property, Skin};
use std::collections::HashMap;

/// Loads a skin with any number of joints.  Joints index the scene's nodes
/// through `node_map`; a skin naming a node that doesn't exist is dropped.
pub fn load_skin(
    index: usize,
    json: &SkinJ,
    acc: &Accessors,
    ctx: &Ctx,
    node_map: &[Option<usize>],
) -> Result<Option<Skin>, LoadError> {
    let name = json.name.clone().unwrap_or_else(|| format!("skin {index}"));
    let joints: Option<Vec<usize>> = json.joints.iter().map(|&j| node_map.get(j).copied().flatten()).collect();
    let Some(joints) = joints else {
        ctx.warn(format!("{name} names a joint that doesn't exist and was ignored"));
        return Ok(None);
    };
    let mut inverse_bind = match json.inverse_bind_matrices {
        Some(a) => match acc.read::<16>(a, Mat4::IDENTITY.to_cols_array()) {
            Ok(m) => m.iter().map(Mat4::from_cols_array).collect(),
            Err(e @ LoadError::TooLarge(_)) => return Err(e),
            Err(e) => {
                ctx.warn(format!("{name}'s inverse bind matrices couldn't be read: {e}"));
                Vec::new()
            }
        },
        None => Vec::new(),
    };
    if json.inverse_bind_matrices.is_some() && inverse_bind.len() < joints.len() {
        ctx.warn(format!("{name} has fewer inverse bind matrices than joints"));
    }
    inverse_bind.resize(joints.len(), Mat4::IDENTITY);
    for m in &mut inverse_bind {
        if !m.is_finite() {
            *m = Mat4::IDENTITY;
        }
    }
    Ok(Some(Skin {
        name,
        joints,
        inverse_bind,
        skeleton: json.skeleton.and_then(|s| node_map.get(s).copied().flatten()),
    }))
}

/// Number of floats per keyframe for a channel's property, or `None` for
/// paths Slerp doesn't animate.  `morphs` is the number of morph
/// targets of the targeted node's mesh.
fn floats_per_key(path: &str, morphs: usize) -> Option<(Property, usize)> {
    Some(match path {
        "translation" => (Property::Translation, 3),
        "rotation" => (Property::Rotation, 4),
        "scale" => (Property::Scale, 3),
        "weights" if morphs > 0 => (Property::Weights, morphs),
        _ => return None,
    })
}

/// The accessors a clip reads, for working out which buffer views to
/// decode.  Channels aimed at nodes outside the loaded scene are skipped.
pub fn accessors_of<'a>(json: &'a AnimationJ, node_map: &'a [Option<usize>]) -> impl Iterator<Item = usize> + 'a {
    json.channels
        .iter()
        .filter(|c| c.target.node.and_then(|n| node_map.get(n).copied().flatten()).is_some())
        .filter_map(|c| json.samplers.get(c.sampler))
        .flat_map(|s| [s.input, s.output])
}

/// Loads a clip.  Channels that can't be read are dropped with a warning;
/// a clip left with none is dropped.  `morphs` gives the number of morph
/// targets of each of the file's meshes, which weight channels must match.
pub fn load_animation(
    index: usize,
    json: &AnimationJ,
    acc: &Accessors,
    ctx: &Ctx,
    root: &Root,
    node_map: &[Option<usize>],
    morphs: &[usize],
) -> Result<Option<Animation>, LoadError> {
    let name = json.name.clone().unwrap_or_else(|| format!("animation {index}"));
    let mut times_cache: HashMap<usize, Vec<f32>> = HashMap::new();
    let mut anim = Animation { name: name.clone(), duration: 0.0, channels: Vec::with_capacity(json.channels.len()) };
    for (ci, ch) in json.channels.iter().enumerate() {
        let Some(old) = ch.target.node else { continue };
        let Some(node) = node_map.get(old).copied().flatten() else { continue };
        let node_morphs = root.nodes.get(old).and_then(|n| n.mesh).and_then(|m| morphs.get(m)).copied();
        let Some((property, n)) = floats_per_key(&ch.target.path, node_morphs.unwrap_or(0)) else {
            continue;
        };
        let Some(sampler) = json.samplers.get(ch.sampler) else {
            ctx.warn(format!("{name}, channel {ci} has no sampler"));
            continue;
        };
        let interpolation = match sampler.interpolation.as_deref() {
            Some("STEP") => Interpolation::Step,
            Some("CUBICSPLINE") => Interpolation::CubicSpline,
            _ => Interpolation::Linear,
        };
        let result = (|| -> Result<Channel, LoadError> {
            let times = match times_cache.get(&sampler.input) {
                Some(t) => t.clone(),
                None => {
                    let t: Vec<f32> = acc.read::<1>(sampler.input, [0.0])?.into_iter().map(|[t]| t).collect();
                    times_cache.insert(sampler.input, t.clone());
                    t
                }
            };
            if times.iter().any(|t| !t.is_finite()) {
                return Err(LoadError::invalid("keyframe times aren't numbers"));
            }
            let mut values: Vec<f32> = match property {
                Property::Rotation => acc.read::<4>(sampler.output, [0.0; 4])?.into_flattened(),
                Property::Weights => acc.read::<1>(sampler.output, [0.0])?.into_flattened(),
                _ => acc.read::<3>(sampler.output, [0.0; 3])?.into_flattened(),
            };
            let per_key = if interpolation == Interpolation::CubicSpline { n * 3 } else { n };
            let needed = times.len().checked_mul(per_key).ok_or_else(|| LoadError::invalid("too many keys"))?;
            if values.len() < needed {
                return Err(LoadError::invalid("fewer values than keyframes"));
            }
            values.truncate(needed);
            Ok(Channel { node, property, interpolation, times, values })
        })();
        match result {
            Ok(channel) => {
                if let Some(&last) = channel.times.last() {
                    anim.duration = anim.duration.max(last);
                }
                anim.channels.push(channel);
            }
            Err(e @ LoadError::TooLarge(_)) => return Err(e),
            Err(e) => ctx.warn(format!("{name}, channel {ci} skipped: {e}")),
        }
    }
    Ok((!anim.channels.is_empty()).then_some(anim))
}
