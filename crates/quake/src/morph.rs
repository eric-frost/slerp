//! Vertex animation shared by Quake MDL, MD2 and MD3: every frame of these
//! formats stores every vertex again, so each frame becomes a morph target
//! relative to frame 0, and runs of frames named alike (`stand1` ...
//! `stand9`) become clips that step a one-hot weight through them.  Linear
//! interpolation between neighbouring keys gives the same in-between frames
//! Quake's own model lerping does.

use slerp_scene::glam::Vec3;
use slerp_scene::{Animation, Channel, Interpolation, MorphTarget, Property};

/// Frames per second of Quake's model animations: the server steps them at
/// 10 Hz.
pub const FPS: f32 = 10.0;

/// The most frames x vertices kept as morph targets (about 240 MB of
/// positions and normals).  Beyond that only the rest pose loads.
pub const MAX_FRAME_VERTICES: usize = 10_000_000;

/// The most frames whose clips are built: weights take frames squared
/// floats, since each key holds a weight for every frame.
pub const MAX_ANIMATED_FRAMES: usize = 4096;

/// Every frame's positions and normals of a model's distinct vertices, in
/// Slerp's Y-up metres.
#[derive(Default)]
pub struct Frames {
    pub names: Vec<String>,
    pub positions: Vec<Vec<Vec3>>,
    pub normals: Vec<Vec<Vec3>>,
}

impl Frames {
    pub fn len(&self) -> usize {
        self.positions.len()
    }

    /// Morph targets for a primitive whose vertex `i` is model vertex
    /// `remap[i]`: one per frame, as offsets from frame 0.
    pub fn targets(&self, remap: &[u32]) -> Vec<MorphTarget> {
        let (Some(p0), Some(n0)) = (self.positions.first(), self.normals.first()) else { return Vec::new() };
        let at = |v: &[Vec3], i: u32| v.get(i as usize).copied().unwrap_or(Vec3::ZERO);
        self.positions
            .iter()
            .zip(&self.normals)
            .map(|(p, n)| MorphTarget {
                positions: remap.iter().map(|&i| at(p, i) - at(p0, i)).collect(),
                normals: remap.iter().map(|&i| at(n, i) - at(n0, i)).collect(),
            })
            .collect()
    }
}

/// The clip a frame belongs to: its name without trailing digits, so
/// `stand1` and `stand12` both give `stand`.
pub fn clip_name(frame: &str) -> &str {
    let trimmed = frame.trim_end_matches(|c: char| c.is_ascii_digit()).trim_end_matches(['_', ' ', '-']);
    if trimmed.is_empty() { "frames" } else { trimmed }
}

/// One clip per run of frames with the same clip name, played at 10 frames
/// per second on `node`'s morph weights.  `durations` gives each frame's
/// own time when the file stores one (MDL frame groups).
pub fn clips(names: &[String], durations: Option<&[f32]>, node: usize) -> Vec<Animation> {
    let count = names.len();
    if !(2..=MAX_ANIMATED_FRAMES).contains(&count) {
        return Vec::new();
    }
    let mut out: Vec<Animation> = Vec::new();
    let mut start = 0;
    while start < count {
        let name = clip_name(&names[start]);
        let mut end = start + 1;
        while end < count && clip_name(&names[end]) == name {
            end += 1;
        }
        let mut times = Vec::with_capacity(end - start);
        let mut t = 0.0;
        for k in start..end {
            times.push(t);
            let step = durations.and_then(|d| d.get(k)).copied().filter(|d| *d > 0.0 && d.is_finite());
            t += step.unwrap_or(1.0 / FPS);
        }
        let mut values = vec![0.0; (end - start) * count];
        for (key, frame) in (start..end).enumerate() {
            values[key * count + frame] = 1.0;
        }
        let mut clip_name = name.to_string();
        let mut n = 2;
        while out.iter().any(|a| a.name == clip_name) {
            clip_name = format!("{name} {n}");
            n += 1;
        }
        out.push(Animation {
            name: clip_name,
            duration: *times.last().unwrap_or(&0.0),
            channels: vec![Channel {
                node,
                property: Property::Weights,
                interpolation: Interpolation::Linear,
                times,
                values,
            }],
        });
        start = end;
    }
    out
}

/// Smooth vertex normals for one frame, weighting each triangle by its
/// corner angle, for formats whose stored normals index id's table of 162
/// directions (which isn't freely licensed) or are absent.
pub fn smooth_normals(positions: &[Vec3], triangles: &[[u32; 3]]) -> Vec<Vec3> {
    let mut normals = vec![Vec3::ZERO; positions.len()];
    for t in triangles {
        let (Some(&a), Some(&b), Some(&c)) =
            (positions.get(t[0] as usize), positions.get(t[1] as usize), positions.get(t[2] as usize))
        else {
            continue;
        };
        let n = (b - a).cross(c - a).normalize_or_zero();
        let angle = |u: Vec3, v: Vec3| u.normalize_or_zero().dot(v.normalize_or_zero()).clamp(-1.0, 1.0).acos();
        normals[t[0] as usize] += n * angle(b - a, c - a);
        normals[t[1] as usize] += n * angle(c - b, a - b);
        normals[t[2] as usize] += n * angle(a - c, b - c);
    }
    for n in &mut normals {
        *n = n.try_normalize().unwrap_or(Vec3::Y);
    }
    normals
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn groups_frames_by_name() {
        let names: Vec<String> =
            ["stand1", "stand2", "stand3", "run1", "run2", "pain", "stand1"].iter().map(|s| s.to_string()).collect();
        let clips = clips(&names, None, 0);
        let got: Vec<(&str, usize)> = clips.iter().map(|c| (c.name.as_str(), c.channels[0].times.len())).collect();
        assert_eq!(got, [("stand", 3), ("run", 2), ("pain", 1), ("stand 2", 1)]);
        let stand = &clips[0];
        assert!((stand.duration - 0.2).abs() < 1e-6);
        assert_eq!(stand.channels[0].values.len(), 3 * names.len());
        assert_eq!(stand.channels[0].values[names.len() + 1], 1.0);
        assert_eq!(clip_name("123"), "frames");
        assert_eq!(clip_name("walk_01"), "walk");
    }

    #[test]
    fn targets_are_offsets_from_frame_zero() {
        let frames = Frames {
            names: vec!["a".into(), "b".into()],
            positions: vec![vec![Vec3::ZERO, Vec3::X], vec![Vec3::Y, Vec3::X]],
            normals: vec![vec![Vec3::Z; 2], vec![Vec3::Z; 2]],
        };
        let t = frames.targets(&[1, 0, 0]);
        assert_eq!(t.len(), 2);
        assert_eq!(t[0].positions, vec![Vec3::ZERO; 3]);
        assert_eq!(t[1].positions, vec![Vec3::ZERO, Vec3::Y, Vec3::Y]);
    }
}
