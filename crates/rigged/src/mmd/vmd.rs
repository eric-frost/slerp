//! VMD, MikuMikuDance's motion format: keyframes for bones (a position
//! offset from the bone's rest place, a rotation, and Bézier curves that
//! ease each channel from the key before) and for morphs (a weight), named
//! by their Shift-JIS names cut to 15 bytes, at 30 frames a second.
//! Camera, light, shadow and IK-switch keys follow and aren't read.
//!
//! On a model, keys whose curves aren't straight are baked to one key per
//! frame, so playback eases as MMD's does.  A motion opened on its own (or
//! added in the viewer) is a skeleton of the bones it moves, each at the
//! origin and unrotated, which is exactly how MMD bones rest relative to
//! their parents' places; the viewer matches them to a model's bones by
//! name, and its morph keys to the model's morph targets by name when the
//! motion was made for a model of the same name.

use super::Layout;
use crate::basis::Basis;
use crate::read::{Cursor, Result, fixed_name, shift_jis};
use crate::skeleton::{Budget, Rig, unique_names};
use crate::textures::{read_limited, siblings};
use slerp_scene::glam::{Quat, Vec3};
use slerp_scene::{Animation, Channel, Interpolation, Limits, LoadError, Mesh, Node, Property, Scene};
use std::collections::HashMap;
use std::path::Path;

pub const FPS: f32 = 30.0;

/// Names in a motion are cut to this many bytes.
const NAME_BYTES: usize = 15;

/// VMD files read beside a model, and their total size.
const MAX_FILES: usize = 16;
const MAX_BYTES: u64 = 256 << 20;

struct BoneKey {
    frame: u32,
    position: Vec3,
    rotation: Quat,
    /// x1, y1, x2, y2 of the curves for X, Y, Z and rotation, 0 to 1.
    curves: [[f32; 4]; 4],
}

/// A morph's keys: frame and weight.
type MorphKeys = Vec<(u32, f32)>;

struct Vmd {
    model: String,
    /// Keys by bone name (Shift-JIS bytes), in file order.
    bones: Vec<(Vec<u8>, Vec<BoneKey>)>,
    morphs: Vec<(Vec<u8>, MorphKeys)>,
}

/// A name field: its bytes up to the first zero.
fn raw_name(b: &[u8]) -> Vec<u8> {
    b[..b.iter().position(|&x| x == 0).unwrap_or(b.len())].to_vec()
}

/// A model's name as a motion would store it: Shift-JIS, cut to 15 bytes.
fn cut(sjis: &[u8]) -> &[u8] {
    &sjis[..sjis.len().min(NAME_BYTES)]
}

fn parse(bytes: &[u8], budget: &mut Budget) -> Result<Vmd> {
    let mut c = Cursor::new(bytes);
    let magic = c.bytes(30, "the header")?;
    // "file" marks the first version, with a shorter model name; newer
    // ones say "0002", and some tools write nothing after "Data".
    let name_len = if magic.starts_with(b"Vocaloid Motion Data file") {
        10
    } else if magic.starts_with(b"Vocaloid Motion Data") {
        20
    } else {
        return Err(LoadError::NotMine);
    };
    let model = fixed_name(c.bytes(name_len, "the header")?, shift_jis);
    let mut index: HashMap<Vec<u8>, usize> = HashMap::new();
    let mut vmd = Vmd { model, bones: Vec::new(), morphs: Vec::new() };

    // 1. Bone keys, 111 bytes each.
    let count = c.u32("the bone key count")?;
    let count = c.count(count.into(), 111, "bone keys")?;
    budget.take(count, 96, "bone keys")?;
    for _ in 0..count {
        let name = raw_name(c.bytes(NAME_BYTES, "a bone key")?);
        let frame = c.u32("a bone key")?;
        let position = c.vec3("a bone key")?;
        let rotation = c.quat("a bone key")?;
        let ip = c.bytes(64, "a bone key")?;
        let mut curves = [[0f32; 4]; 4];
        for (k, curve) in curves.iter_mut().enumerate() {
            *curve = [ip[k], ip[4 + k], ip[8 + k], ip[12 + k]].map(|b| (b.min(127)) as f32 / 127.0);
        }
        let rotation = if rotation.length_squared() > 1e-12 { rotation.normalize() } else { Quat::IDENTITY };
        let slot = *index.entry(name.clone()).or_insert_with(|| {
            vmd.bones.push((name, Vec::new()));
            vmd.bones.len() - 1
        });
        vmd.bones[slot].1.push(BoneKey { frame, position, rotation, curves });
    }

    // 2. Morph keys, 23 bytes each.  Old files may end before them.
    if c.remaining() >= 4 {
        let count = c.u32("the morph key count")?;
        let count = c.count(count.into(), 23, "morph keys")?;
        budget.take(count, 32, "morph keys")?;
        index.clear();
        for _ in 0..count {
            let name = raw_name(c.bytes(NAME_BYTES, "a morph key")?);
            let frame = c.u32("a morph key")?;
            let weight = c.f32("a morph key")?;
            let slot = *index.entry(name.clone()).or_insert_with(|| {
                vmd.morphs.push((name, Vec::new()));
                vmd.morphs.len() - 1
            });
            vmd.morphs[slot].1.push((frame, weight));
        }
    }
    // Keys in frame order, the last of any repeated frame kept.
    for (_, keys) in &mut vmd.bones {
        keys.sort_by_key(|k| k.frame);
        dedup_frames(keys, |k| k.frame);
    }
    for (_, keys) in &mut vmd.morphs {
        keys.sort_by_key(|k| k.0);
        dedup_frames(keys, |k| k.0);
    }
    Ok(vmd)
}

/// Keeps the last key of each frame in a list sorted by frame (a stable
/// sort keeps file order within a frame).
fn dedup_frames<T>(keys: &mut Vec<T>, frame: impl Fn(&T) -> u32) {
    let mut out: Vec<T> = Vec::with_capacity(keys.len());
    for k in keys.drain(..) {
        if out.last().is_some_and(|l| frame(l) == frame(&k)) {
            out.pop();
        }
        out.push(k);
    }
    *keys = out;
}

/*┌─────────────────╮
| Curves          |
└─────────────────╯*/
//region Curves

/// MMD's easing: the Bézier curve from (0, 0) through (x1, y1) and (x2,
/// y2) to (1, 1), read as y for x = `t`.
fn ease(curve: [f32; 4], t: f32) -> f32 {
    let [x1, y1, x2, y2] = curve.map(f64::from);
    if x1 == y1 && x2 == y2 {
        return t;
    }
    let bez = |a: f64, b: f64, s: f64| {
        let r = 1.0 - s;
        3.0 * r * r * s * a + 3.0 * r * s * s * b + s * s * s
    };
    // x(s) rises monotonically for control points inside the unit square,
    // so bisection finds s.  In doubles, since x(s) can be nearly flat
    // where y(s) is steep.
    let (mut lo, mut hi) = (0.0f64, 1.0f64);
    for _ in 0..40 {
        let mid = 0.5 * (lo + hi);
        if bez(x1, x2, mid) < t as f64 {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    bez(y1, y2, 0.5 * (lo + hi)) as f32
}

fn is_linear(c: &[f32; 4]) -> bool {
    c[0] == c[1] && c[2] == c[3]
}

/// A bone's keys as per-frame samples: the keys themselves, plus every
/// frame between two keys whose curve into the later one isn't straight.
/// Each sample is (frame, position offset, rotation) in MMD space.
fn samples(keys: &[BoneKey], budget: &mut Budget) -> Result<Vec<(f32, Vec3, Quat)>> {
    let mut out = Vec::with_capacity(keys.len());
    for (i, k) in keys.iter().enumerate() {
        if i > 0 {
            let p = &keys[i - 1];
            let span = k.frame - p.frame;
            if span > 1 && !k.curves.iter().all(is_linear) {
                budget.take(span as usize, 32, "eased keys")?;
                for f in 1..span {
                    let t = f as f32 / span as f32;
                    let pos = Vec3::new(
                        p.position.x + (k.position.x - p.position.x) * ease(k.curves[0], t),
                        p.position.y + (k.position.y - p.position.y) * ease(k.curves[1], t),
                        p.position.z + (k.position.z - p.position.z) * ease(k.curves[2], t),
                    );
                    let rot = p.rotation.slerp(k.rotation, ease(k.curves[3], t));
                    out.push(((p.frame + f) as f32, pos, rot));
                }
            }
        }
        out.push((k.frame as f32, k.position, k.rotation));
    }
    Ok(out)
}

//endregion

/*┌─────────────────╮
| Clips           |
└─────────────────╯*/
//region Clips

/// A motion's bone keys as channels.  `bone(name)` finds the node and its
/// rest translation for a bone name.
fn bone_channels(
    vmd: &Vmd,
    basis: &Basis,
    budget: &mut Budget,
    bone: impl Fn(&[u8]) -> Option<(usize, Vec3)>,
) -> Result<(Vec<Channel>, f32)> {
    let mut channels = Vec::new();
    let mut end = 0f32;
    for (name, keys) in &vmd.bones {
        let Some((node, rest)) = bone(name) else { continue };
        let s = samples(keys, budget)?;
        budget.take(s.len(), 28, "keys")?;
        let times: Vec<f32> = s.iter().map(|k| k.0 / FPS).collect();
        end = end.max(times.last().copied().unwrap_or(0.0));
        if s.iter().any(|k| k.1 != Vec3::ZERO) {
            let values = s.iter().flat_map(|k| (rest + basis.point(k.1)).to_array()).collect();
            channels.push(Channel {
                node,
                property: Property::Translation,
                interpolation: Interpolation::Linear,
                times: times.clone(),
                values,
            });
        }
        let mut rotations: Vec<Quat> = s.iter().map(|k| basis.rotation(k.2)).collect();
        for i in 1..rotations.len() {
            if rotations[i].dot(rotations[i - 1]) < 0.0 {
                rotations[i] = -rotations[i];
            }
        }
        channels.push(Channel {
            node,
            property: Property::Rotation,
            interpolation: Interpolation::Linear,
            times,
            values: rotations.iter().flat_map(|q| q.to_array()).collect(),
        });
    }
    Ok((channels, end))
}

/// A motion's morph keys as one weights channel for `node`, whose targets
/// are named `targets` (Shift-JIS).  Every target gets a value at every
/// frame any matched morph has a key, interpolated between its own keys.
fn morph_channel(vmd: &Vmd, node: usize, targets: &[&[u8]], budget: &mut Budget) -> Result<Option<(Channel, f32)>> {
    let keyed: Vec<(usize, &MorphKeys)> = vmd
        .morphs
        .iter()
        .filter_map(|(name, keys)| Some((targets.iter().position(|t| cut(t) == name.as_slice())?, keys)))
        .filter(|(_, keys)| !keys.is_empty())
        .collect();
    if keyed.is_empty() {
        return Ok(None);
    }
    let mut frames: Vec<u32> = keyed.iter().flat_map(|(_, k)| k.iter().map(|k| k.0)).collect();
    frames.sort_unstable();
    frames.dedup();
    budget.take(frames.len().saturating_mul(targets.len()), 4, "morph keys")?;
    let mut values = vec![0f32; frames.len() * targets.len()];
    for &(t, keys) in &keyed {
        let mut at = 0;
        for (fi, &f) in frames.iter().enumerate() {
            while at + 1 < keys.len() && keys[at + 1].0 <= f {
                at += 1;
            }
            let (f0, w0) = keys[at];
            let w = match keys.get(at + 1) {
                Some(&(f1, w1)) if f > f0 => w0 + (w1 - w0) * (f - f0) as f32 / (f1 - f0) as f32,
                _ => w0,
            };
            values[fi * targets.len() + t] = w;
        }
    }
    let times: Vec<f32> = frames.iter().map(|&f| f as f32 / FPS).collect();
    let end = times.last().copied().unwrap_or(0.0);
    let channel = Channel { node, property: Property::Weights, interpolation: Interpolation::Linear, times, values };
    Ok(Some((channel, end)))
}

/// A motion as a clip for a model.  `None` when it moves nothing the model
/// has (a camera motion, or one for another model).
fn clip_for_model(
    name: &str,
    vmd: &Vmd,
    rig: &Rig,
    bone_names: &[&[u8]],
    layout: &Layout,
    budget: &mut Budget,
) -> Result<Option<Animation>> {
    let basis = Basis::mmd();
    let (mut channels, mut end) = bone_channels(vmd, &basis, budget, |name| {
        let b = bone_names.iter().position(|n| cut(n) == name)?;
        Some((layout.base + b, rig.bones[b].local.translation))
    })?;
    let targets: Vec<&[u8]> = layout.target_names.iter().map(Vec::as_slice).collect();
    if let Some((ch, morph_end)) = morph_channel(vmd, layout.mesh_node, &targets, budget)? {
        channels.push(ch);
        end = end.max(morph_end);
    }
    Ok((!channels.is_empty()).then(|| Animation { name: name.to_string(), duration: end, channels }))
}

/// Adds the motions beside a model that move its bones or morphs.
pub(super) fn add_neighbours(
    path: &Path,
    rig: &Rig,
    bone_names: &[&[u8]],
    layout: &Layout,
    scene: &mut Scene,
    budget: &mut Budget,
) {
    let Some(dir) = path.parent() else { return };
    let mut read = 0u64;
    let mut clips = Vec::new();
    for file in siblings(dir, &["vmd"], MAX_FILES) {
        let Some(bytes) = read_limited(&file, MAX_BYTES.saturating_sub(read)) else { continue };
        read += bytes.len() as u64;
        let name = file.file_stem().map_or("motion".into(), |s| s.to_string_lossy().into_owned());
        let clip = parse(&bytes, budget).and_then(|vmd| clip_for_model(&name, &vmd, rig, bone_names, layout, budget));
        match clip {
            Ok(Some(clip)) => clips.push(clip),
            Ok(None) => {}
            Err(LoadError::TooLarge(_)) => {
                scene.info.warnings.push("more motions than memory allows; some were left out".into());
                break;
            }
            Err(e) => scene.info.warnings.push(format!("{}: {e}", file.display())),
        }
    }
    unique_names(&mut clips);
    scene.animations.extend(clips);
}

/// A motion opened on its own: a node per bone it moves, a node with the
/// morphs it keys, and its one clip.
pub fn load_vmd(path: &Path, bytes: &[u8], limits: &Limits) -> Result<Scene> {
    let mut budget = Budget::new(bytes.len() as u64);
    let vmd = parse(bytes, &mut budget)?;
    let basis = Basis::mmd();
    let mut scene = Scene::default();
    for (name, _) in &vmd.bones {
        scene.nodes.push(Node::new(shift_jis(name)));
        scene.roots.push(scene.nodes.len() - 1);
    }
    let name = path.file_stem().map_or("motion".into(), |s| s.to_string_lossy().into_owned());
    let bone_count = vmd.bones.len();
    let mut clip = Animation { name, ..Default::default() };
    if limits.animations {
        let (channels, end) = bone_channels(&vmd, &basis, &mut budget, |n| {
            Some((vmd.bones.iter().position(|(b, _)| b == n)?, Vec3::ZERO))
        })?;
        clip.channels = channels;
        clip.duration = end;
    }
    // Morph weights need a mesh with named targets to land on; this one
    // has no geometry.  It carries the model's name, which the viewer
    // matches against the model the motion is added to.
    if !vmd.morphs.is_empty() {
        let names: Vec<Vec<u8>> = vmd.morphs.iter().map(|(n, _)| n.clone()).collect();
        scene.meshes.push(Mesh {
            name: "morphs".into(),
            primitives: Vec::new(),
            target_names: names.iter().map(|n| shift_jis(n)).collect(),
        });
        let mut node = Node::new(if vmd.model.is_empty() { "morphs".to_string() } else { vmd.model.clone() });
        node.mesh = Some(0);
        scene.nodes.push(node);
        let morph_node = scene.nodes.len() - 1;
        scene.roots.push(morph_node);
        if limits.animations {
            let targets: Vec<&[u8]> = names.iter().map(Vec::as_slice).collect();
            if let Some((ch, end)) = morph_channel(&vmd, morph_node, &targets, &mut budget)? {
                clip.channels.push(ch);
                clip.duration = clip.duration.max(end);
            }
        }
    }
    if limits.animations && !clip.channels.is_empty() {
        scene.animations.push(clip);
    }
    scene.info.format = "MikuMikuDance motion (VMD)".into();
    if !vmd.model.is_empty() {
        scene.info.details.push(("Made for".into(), vmd.model.clone()));
    }
    scene.info.details.push(("Bones keyed".into(), bone_count.to_string()));
    scene.info.details.push(("Morphs keyed".into(), vmd.morphs.len().to_string()));
    Ok(scene)
}

//endregion

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Writes a VMD for tests: bone keys (name, frame, position, rotation,
    /// curve bytes) and morph keys.
    pub fn write(model: &str, bones: &[(&str, u32, Vec3, Quat, [u8; 4])], morphs: &[(&str, u32, f32)]) -> Vec<u8> {
        let mut out = b"Vocaloid Motion Data 0002\0\0\0\0\0".to_vec();
        let name = |s: &str, n: usize| {
            let (b, _, _) = encoding_rs::SHIFT_JIS.encode(s);
            let mut v = b.into_owned();
            v.resize(n, 0);
            v
        };
        out.extend(name(model, 20));
        out.extend((bones.len() as u32).to_le_bytes());
        for &(n, f, p, q, curve) in bones {
            out.extend(name(n, 15));
            out.extend(f.to_le_bytes());
            for v in p.to_array().iter().chain(q.to_array().iter()) {
                out.extend(v.to_le_bytes());
            }
            let mut ip = [0u8; 64];
            for k in 0..4 {
                ip[k] = curve[0];
                ip[4 + k] = curve[1];
                ip[8 + k] = curve[2];
                ip[12 + k] = curve[3];
            }
            out.extend(ip);
        }
        out.extend((morphs.len() as u32).to_le_bytes());
        for &(n, f, w) in morphs {
            out.extend(name(n, 15));
            out.extend(f.to_le_bytes());
            out.extend(w.to_le_bytes());
        }
        out.extend([0u8; 12]);
        out
    }

    pub const LINEAR: [u8; 4] = [20, 20, 107, 107];

    #[test]
    fn easing_curves() {
        let linear = LINEAR.map(|b| b as f32 / 127.0);
        assert_eq!(ease(linear, 0.3), 0.3);
        // Ease-in-out: slow at both ends, half way at the middle.
        let s = [1.0, 0.0, 0.0, 1.0];
        assert!(ease(s, 0.1) < 0.05);
        assert!((ease(s, 0.5) - 0.5).abs() < 1e-3);
        assert!(ease(s, 0.9) > 0.95);
        assert!((ease(s, 1.0) - 1.0).abs() < 1e-3 && ease(s, 0.0).abs() < 1e-3);
    }

    #[test]
    fn standalone_motion() {
        let bytes = write(
            "初音ミク",
            &[
                ("センター", 30, Vec3::new(0.0, 0.0, 10.0), Quat::IDENTITY, LINEAR),
                ("センター", 0, Vec3::ZERO, Quat::IDENTITY, LINEAR),
                ("頭", 0, Vec3::ZERO, Quat::from_rotation_y(0.5), LINEAR),
            ],
            &[("あ", 0, 0.0), ("あ", 15, 1.0)],
        );
        let scene = load_vmd(Path::new("dance.vmd"), &bytes, &Limits::viewer()).unwrap();
        assert_eq!(scene.nodes[0].name, "センター");
        assert_eq!(scene.nodes[2].name, "初音ミク");
        assert_eq!(scene.meshes[0].target_names, vec!["あ"]);
        let clip = &scene.animations[0];
        assert_eq!(clip.name, "dance");
        assert!((clip.duration - 1.0).abs() < 1e-6);
        // Keys sorted by frame; MMD's +Z (away from a model's front) becomes
        // -Z, and 10 units are 80 cm.
        let t = &clip.channels[0];
        assert_eq!(t.property, Property::Translation);
        assert_eq!(t.times, vec![0.0, 1.0]);
        assert!((t.values[5] + 0.8).abs() < 1e-6);
        // The head's turn about Y, mirrored, turns the other way.
        let head = clip.channels.iter().find(|c| c.node == 1).unwrap();
        let q = Quat::from_slice(&head.values[0..4]);
        assert!(q.angle_between(Quat::from_rotation_y(-0.5)) < 1e-5);
        let w = clip.channels.iter().find(|c| c.property == Property::Weights).unwrap();
        assert_eq!(w.times, vec![0.0, 0.5]);
        assert_eq!(w.values, vec![0.0, 1.0]);
    }

    #[test]
    fn eased_keys_are_baked() {
        let ease_in_out = [127, 0, 0, 127];
        let bytes = write(
            "",
            &[
                ("腕", 0, Vec3::ZERO, Quat::IDENTITY, LINEAR),
                ("腕", 10, Vec3::new(10.0, 0.0, 0.0), Quat::IDENTITY, ease_in_out),
            ],
            &[],
        );
        let scene = load_vmd(Path::new("m.vmd"), &bytes, &Limits::viewer()).unwrap();
        let t = &scene.animations[0].channels[0];
        assert_eq!(t.times.len(), 11);
        // Slow start: after one frame of ten it has moved under a tenth.
        assert!(t.values[3] > 0.0 && t.values[3] < 0.08 * 10.0 * 0.05);
    }
}
